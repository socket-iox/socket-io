use std::{
    fmt::Debug,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::SystemTime,
};

use async_stream::try_stream;
use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use futures_util::{ready, FutureExt, Stream, StreamExt};
use http::HeaderMap;
use reqwest::{Client, ClientBuilder, Response, Url};
use tokio::sync::{
    mpsc::{Receiver, Sender},
    Mutex,
};

use crate::{
    error::Result,
    transports::{Data, Transport},
    Error,
};

type ClientPollStream = Box<dyn Stream<Item = Result<Bytes>> + 'static + Send>;

#[derive(Clone)]
pub struct ClientPollingTransport {
    client: Client,
    url: Url,
    stream: Arc<Mutex<Pin<ClientPollStream>>>,
}

#[derive(Debug, Clone)]
pub struct ServerPollingTransport {
    sender: Arc<Sender<Bytes>>,
    receiver: Arc<Mutex<Receiver<Bytes>>>,
}

impl Debug for ClientPollingTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientPollingTransport")
            .field("url", &self.url)
            .field("client", &self.client)
            .finish()
    }
}

impl ClientPollingTransport {
    pub(crate) fn new(mut url: Url, headers: Option<HeaderMap>) -> Result<Self> {
        let mut builder = ClientBuilder::new();
        if let Some(headers) = headers {
            builder = builder.default_headers(headers);
        }
        let client = builder.no_proxy().build()?;
        url.query_pairs_mut().append_pair("transport", "polling");

        Ok(Self {
            stream: Arc::new(Mutex::new(Self::stream(url.clone(), client.clone()))),
            client,
            url,
        })
    }

    #[cfg(test)]
    fn url(&self) -> Url {
        self.url.clone()
    }

    fn send_request(url: Url, client: Client) -> impl Stream<Item = Result<Response>> {
        try_stream! {
            let url = append_hash(&url);

            yield client
                .get(url)
                .send().await?
        }
    }

    fn stream(
        url: Url,
        client: Client,
    ) -> Pin<Box<dyn Stream<Item = Result<Bytes>> + 'static + Send>> {
        Box::pin(try_stream! {
            loop {
                for await elem in Self::send_request(url.clone(), client.clone()) {
                    for await bytes in elem?.bytes_stream() {
                        yield bytes?;
                    }
                }
            }
        })
    }
}

#[async_trait]
impl Transport for ClientPollingTransport {
    async fn emit(&self, payload: Data) -> Result<()> {
        let body = match payload {
            Data::Text(data) => data,
            Data::Binary(data) => {
                let mut buf = BytesMut::with_capacity(data.len() + 1);
                buf.put_u8(b'b');
                buf.put(base64::encode(data).as_bytes());
                buf.freeze()
            }
        };

        let status = self
            .client
            .post(append_hash(&self.url))
            .body(body)
            .send()
            .await?
            .status()
            .as_u16();

        match status {
            200 => Ok(()),
            _ => Err(crate::Error::InvalidHttpResponseStatus(status)),
        }
    }
}

impl Stream for ClientPollingTransport {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut fut = ready!(Box::pin(self.stream.lock()).poll_unpin(cx));
        fut.poll_next_unpin(cx)
    }
}

impl ServerPollingTransport {
    pub(crate) fn new(sender: Sender<Bytes>, receiver: Receiver<Bytes>) -> Self {
        Self {
            sender: Arc::new(sender),
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }
}

#[async_trait]
impl Transport for ServerPollingTransport {
    async fn emit(&self, payload: Data) -> Result<()> {
        let data = match payload {
            Data::Text(data) => data,
            Data::Binary(data) => {
                let mut buf = BytesMut::with_capacity(data.len() + 1);
                buf.put_u8(b'b');
                buf.put(base64::encode(data).as_bytes());
                buf.freeze()
            }
        };

        self.sender.send(data).await.map_err(Error::SendError)?;
        Ok(())
    }
}

impl Stream for ServerPollingTransport {
    type Item = Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut lock = ready!(Box::pin(self.receiver.lock()).poll_unpin(cx));
        let recv = ready!(Box::pin(lock.recv()).poll_unpin(cx));

        match recv {
            Some(bytes) => Poll::Ready(Some(Ok(bytes))),
            None => Poll::Ready(None),
        }
    }
}

pub(crate) fn append_hash(url: &Url) -> Url {
    let mut url = url.clone();
    let now_str = format!("{:#?}", SystemTime::now());
    // SAFETY: time string is valid for adler32
    let hash = adler32::adler32(now_str.as_bytes()).unwrap();
    url.query_pairs_mut().append_pair("t", &hash.to_string());
    url
}

#[cfg(test)]
mod test {
    use tokio::sync::mpsc::channel;

    use super::*;
    use futures_util::StreamExt;
    use std::str::FromStr;

    #[test]
    fn polling_transport_url() -> Result<()> {
        let url = Url::from_str("http://127.0.0.1").unwrap();
        let transport = ClientPollingTransport::new(url.clone(), None).unwrap();
        assert_eq!(
            transport.url().to_string(),
            url.to_string() + "?transport=polling"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_server_polling_transport() -> Result<()> {
        let (send_tx, mut send_rx) = channel(100);
        let (recv_tx, recv_rx) = channel(100);
        let mut transport = ServerPollingTransport::new(send_tx, recv_rx);

        let data = Bytes::from_static(b"1Hello\x1e1HelloWorld");

        recv_tx.send(data.clone()).await.unwrap();

        let msg = transport.next().await;
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert!(msg.is_ok());
        let msg = msg.unwrap();

        assert_eq!(msg, data);

        let payload = Data::Text(data.clone());
        transport.emit(payload).await?;
        let msg = send_rx.recv().await;
        assert!(msg.is_some());
        let msg = msg.unwrap();
        assert_eq!(msg, data);

        Ok(())
    }
}

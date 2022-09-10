use std::{
    borrow::Cow,
    str::from_utf8,
    sync::Arc,
    task::{ready, Poll},
};

use async_trait::async_trait;
use bytes::{BufMut, Bytes, BytesMut};
use futures_util::{
    stream::{SplitSink, SplitStream},
    FutureExt, SinkExt, Stream, StreamExt,
};
use http::HeaderMap;
use reqwest::Url;
use tokio::{net::TcpStream, sync::Mutex};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tungstenite::{client::IntoClientRequest, Message};

use crate::{
    error::Result,
    transports::{Payload, Transport},
    Error, Packet, PacketType,
};

type WebsocketSender = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type WebsocketReceiver = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

#[derive(Clone)]
pub struct Websocket {
    sender: Arc<Mutex<WebsocketSender>>,
    receiver: Arc<Mutex<WebsocketReceiver>>,
}

impl Websocket {
    pub async fn connect(
        mut url: Url,
        headers: Option<HeaderMap>,
    ) -> Result<(WebsocketSender, WebsocketReceiver)> {
        // SAFETY: ws is valid to parse scheme in `set_scheme`
        url.set_scheme("ws").unwrap();
        url.query_pairs_mut().append_pair("transport", "websocket");

        let mut req = url.clone().into_client_request()?;
        if let Some(map) = headers {
            req.headers_mut().extend(map)
        }

        let (stream, _) = connect_async(req).await?;
        let (sender, receiver) = stream.split();

        Ok((sender, receiver))
    }

    pub fn new(sender: WebsocketSender, receiver: WebsocketReceiver) -> Self {
        Websocket {
            sender: Arc::new(Mutex::new(sender)),
            receiver: Arc::new(Mutex::new(receiver)),
        }
    }
    /// Sends probe packet to ensure connection is valid, then sends upgrade
    /// request
    pub(crate) async fn upgrade(&self) -> Result<()> {
        let mut receiver = self.receiver.lock().await;
        let mut sender = self.sender.lock().await;

        sender
            .send(Message::text(from_utf8(&Bytes::from(Packet::new(
                PacketType::Ping,
                Bytes::from("probe"),
            )))?))
            .await?;

        let msg = receiver
            .next()
            .await
            .ok_or(Error::IllegalWebsocketUpgrade())??;

        if msg.into_data() != Bytes::from(Packet::new(PacketType::Pong, Bytes::from("probe"))) {
            return Err(Error::InvalidPacket());
        }

        sender
            .send(Message::text(Cow::Borrowed(from_utf8(&Bytes::from(
                Packet::new(PacketType::Upgrade, Bytes::from("")),
            ))?)))
            .await?;

        Ok(())
    }

    pub(crate) async fn poll_next(&self) -> Result<Option<Bytes>> {
        loop {
            let mut receiver = self.receiver.lock().await;
            let next = receiver.next().await;
            match next {
                Some(Ok(Message::Text(str))) => return Ok(Some(Bytes::from(str))),
                Some(Ok(Message::Binary(data))) => {
                    let mut msg = BytesMut::with_capacity(data.len() + 1);
                    msg.put_u8(PacketType::Message as u8);
                    msg.put(data.as_ref());

                    return Ok(Some(msg.freeze()));
                }
                // ignore packets other than text and binary
                Some(Ok(_)) => (),
                Some(Err(err)) => return Err(err.into()),
                None => return Ok(None),
            }
        }
    }
}

#[async_trait]
impl Transport for Websocket {
    async fn emit(&self, payload: Payload) -> Result<()> {
        let mut sender = self.sender.lock().await;
        let message: Message = payload.try_into()?;

        sender.send(message).await?;

        Ok(())
    }
}

impl Stream for Websocket {
    type Item = Result<Bytes>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            let mut lock = ready!(Box::pin(self.receiver.lock()).poll_unpin(cx));
            let next = ready!(lock.poll_next_unpin(cx));

            match next {
                Some(Ok(Message::Text(str))) => return Poll::Ready(Some(Ok(Bytes::from(str)))),
                Some(Ok(Message::Binary(data))) => {
                    let mut msg = BytesMut::with_capacity(data.len() + 1);
                    msg.put_u8(PacketType::Message as u8);
                    msg.put(data.as_ref());

                    return Poll::Ready(Some(Ok(msg.freeze())));
                }
                // ignore packets other than text and binary
                Some(Ok(_)) => (),
                Some(Err(err)) => return Poll::Ready(Some(Err(err.into()))),
                None => return Poll::Ready(None),
            }
        }
    }
}

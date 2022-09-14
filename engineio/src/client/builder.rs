use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::Url;
use tokio::sync::mpsc::channel;

use crate::{
    error::Result,
    header::HeaderMap,
    packet::HandshakePacket,
    socket::Socket,
    transports::{polling::ClientPollingTransport, websocket::WebsocketTransport, Transport},
    Error, Event, Packet, ENGINE_IO_VERSION,
};

use super::Client;

#[derive(Clone, Debug)]
pub struct ClientBuilder {
    url: Url,
    should_pong: bool,
    headers: Option<HeaderMap>,
    handshake: Option<HandshakePacket>,
    channel_size: usize,
}

impl ClientBuilder {
    pub fn new(url: Url) -> Self {
        let mut url = url;
        url.query_pairs_mut()
            .append_pair("EIO", &ENGINE_IO_VERSION.to_string());

        // No path add engine.io
        if url.path() == "/" {
            url.set_path("/engine.io/");
        }
        ClientBuilder {
            url,
            headers: None,
            should_pong: true,
            handshake: None,
            channel_size: 100,
        }
    }

    pub fn headers(mut self, headers: HeaderMap) -> Self {
        self.headers = Some(headers);
        self
    }

    pub fn channel_buf(mut self, size: usize) -> Self {
        self.channel_size = size;
        self
    }

    async fn handshake_with_transport<T: Transport>(&mut self, transport: &mut T) -> Result<()> {
        // No need to handshake twice
        if self.handshake.is_some() {
            return Ok(());
        }

        let mut url = self.url.clone();

        let handshake: HandshakePacket =
            Packet::try_from(transport.next().await.ok_or(Error::IncompletePacket())??)?
                .try_into()?;

        // update the base_url with the new sid
        url.query_pairs_mut().append_pair("sid", &handshake.sid[..]);

        self.handshake = Some(handshake);

        self.url = url;

        Ok(())
    }

    async fn handshake(&mut self) -> Result<()> {
        if self.handshake.is_some() {
            return Ok(());
        }

        let headers = if let Some(map) = self.headers.clone() {
            Some(map.try_into()?)
        } else {
            None
        };

        // Start with polling transport
        let mut transport = ClientPollingTransport::new(self.url.clone(), headers)?;

        self.handshake_with_transport(&mut transport).await
    }

    /// Build websocket if allowed, if not fall back to polling
    pub async fn build(mut self) -> Result<Client> {
        self.handshake().await?;

        if self.websocket_upgrade()? {
            self.build_websocket_with_upgrade().await
        } else {
            self.build_polling().await
        }
    }

    /// Checks the handshake to see if websocket upgrades are allowed
    fn websocket_upgrade(&mut self) -> Result<bool> {
        if self.handshake.is_none() {
            return Ok(false);
        }

        Ok(self
            .handshake
            .as_ref()
            .unwrap()
            .upgrades
            .iter()
            .any(|upgrade| upgrade.to_lowercase() == *"websocket"))
    }

    /// Build socket with a polling transport then upgrade to websocket transport
    pub async fn build_websocket_with_upgrade(mut self) -> Result<Client> {
        self.handshake().await?;

        if self.websocket_upgrade()? {
            self.build_websocket().await
        } else {
            Err(Error::IllegalWebsocketUpgrade())
        }
    }

    /// Build socket with only a websocket transport
    pub async fn build_websocket(mut self) -> Result<Client> {
        let headers = if let Some(map) = self.headers.clone() {
            Some(map.try_into()?)
        } else {
            None
        };

        match self.url.scheme() {
            "http" | "ws" => {
                let (sender, receiver) =
                    WebsocketTransport::connect(self.url.clone(), headers).await?;
                let mut transport = WebsocketTransport::new(sender, receiver);

                if self.handshake.is_some() {
                    transport.upgrade().await?;
                } else {
                    self.handshake_with_transport(&mut transport).await?;
                }

                let (tx, rx) = channel::<Event>(self.channel_size);
                // NOTE: Although self.url contains the sid, it does not propagate to the transport
                // SAFETY: handshake function called previously.
                Ok(Client::new(
                    Socket::new(
                        Box::new(transport),
                        self.handshake.unwrap(),
                        Arc::new(tx),
                        self.should_pong,
                        false,
                    ),
                    rx,
                ))
            }
            // TODO: tls
            _ => Err(Error::InvalidUrlScheme(self.url.scheme().to_string())),
        }
    }

    pub async fn build_polling(mut self) -> Result<Client> {
        self.handshake().await?;

        // Make a polling transport with new sid
        // TODO: tls
        let transport =
            ClientPollingTransport::new(self.url, self.headers.map(|v| v.try_into().unwrap()))?;

        let (tx, rx) = channel::<Event>(self.channel_size);
        // SAFETY: handshake function called previously.
        Ok(Client::new(
            Socket::new(
                Box::new(transport),
                self.handshake.unwrap(),
                Arc::new(tx),
                self.should_pong,
                false,
            ),
            rx,
        ))
    }

    #[cfg(test)]
    pub(crate) fn should_pong_for_test(mut self, should_pong: bool) -> Self {
        self.should_pong = should_pong;
        self
    }
}

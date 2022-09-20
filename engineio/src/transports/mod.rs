use std::{borrow::Cow, fmt::Debug};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use tungstenite::Message;

use crate::{
    error::Result,
    transports::{
        polling::{ClientPollingTransport, ServerPollingTransport},
        websocket::WebsocketTransport,
    },
};

pub(crate) mod polling;
pub(crate) mod websocket;

#[async_trait]
pub trait Transport: Send + Debug + Unpin + Stream<Item = Result<Bytes>> {
    async fn emit(&self, payload: Data) -> Result<()>;
}

pub enum Data {
    Text(Bytes),
    Binary(Bytes),
}

impl TryFrom<Data> for Message {
    type Error = crate::Error;

    fn try_from(payload: Data) -> std::result::Result<Self, Self::Error> {
        let message = match payload {
            Data::Text(data) => Message::text(Cow::Borrowed(std::str::from_utf8(data.as_ref())?)),
            Data::Binary(data) => Message::binary(Cow::Borrowed(data.as_ref())),
        };
        Ok(message)
    }
}

#[derive(Debug, Clone)]
pub enum TransportType {
    ClientPolling(ClientPollingTransport),
    ServerPolling(ServerPollingTransport),
    Websocket(WebsocketTransport),
}

impl From<ClientPollingTransport> for TransportType {
    fn from(transport: ClientPollingTransport) -> Self {
        TransportType::ClientPolling(transport)
    }
}

impl From<ServerPollingTransport> for TransportType {
    fn from(transport: ServerPollingTransport) -> Self {
        TransportType::ServerPolling(transport)
    }
}

impl From<WebsocketTransport> for TransportType {
    fn from(transport: WebsocketTransport) -> Self {
        TransportType::Websocket(transport)
    }
}

impl TransportType {
    pub fn as_transport(&self) -> &(dyn Transport + Send) {
        match self {
            TransportType::ClientPolling(transport) => transport,
            TransportType::ServerPolling(transport) => transport,
            TransportType::Websocket(transport) => transport,
        }
    }

    pub fn as_pin_box(&mut self) -> std::pin::Pin<Box<&mut (dyn Transport + Send)>> {
        match self {
            TransportType::ClientPolling(transport) => Box::pin(transport),
            TransportType::ServerPolling(transport) => Box::pin(transport),
            TransportType::Websocket(transport) => Box::pin(transport),
        }
    }
}

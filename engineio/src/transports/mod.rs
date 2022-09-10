use std::borrow::Cow;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use tungstenite::Message;

use crate::error::Result;

pub(crate) mod polling;
pub(crate) mod websocket;

#[async_trait]
pub trait Transport: Stream<Item = Result<Bytes>> {
    async fn emit(&self, payload: Payload) -> Result<()>;
}

pub enum Payload {
    Text(Bytes),
    Binary(Bytes),
}

impl TryFrom<Payload> for Message {
    type Error = crate::Error;

    fn try_from(payload: Payload) -> std::result::Result<Self, Self::Error> {
        let message = match payload {
            Payload::Text(data) => {
                Message::text(Cow::Borrowed(std::str::from_utf8(data.as_ref())?))
            }
            Payload::Binary(data) => Message::binary(Cow::Borrowed(data.as_ref())),
        };
        Ok(message)
    }
}

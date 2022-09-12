use std::{borrow::Cow, fmt::Debug};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use tungstenite::Message;

use crate::error::Result;

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

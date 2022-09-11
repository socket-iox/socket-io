use base64::DecodeError;
use bytes::Bytes;
use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use reqwest::Error as HttpError;
use serde_json::Error as JsonError;
use std::io::Error as IoError;
use std::str::Utf8Error;
use thiserror::Error;
use tokio::sync::mpsc::error::SendError;
use tungstenite::Error as WsError;

use crate::Event;

#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    #[error("Invalid packet type: {0}")]
    InvalidPacketType(u8),
    #[error("Invalid packet")]
    InvalidPacket(),
    #[error("Incomplete packet")]
    IncompletePacket(),
    #[error("Invalid base 64: {0}")]
    InvalidBase64(#[from] DecodeError),
    #[error("Invalid json: {0}")]
    InvalidJson(#[from] JsonError),
    #[error("Invalid header name")]
    InvalidHeaderNameFromReqwest(#[from] InvalidHeaderName),
    #[error("Invalid header value")]
    InvalidHeaderValueFromReqwest(#[from] InvalidHeaderValue),
    #[error("Invalid hand shake: {0}")]
    InvalidHandShake(String),
    #[error("Io Error: {0}")]
    IoError(#[from] IoError),
    #[error("Websocket Error: {0}")]
    WsError(#[from] WsError),
    #[error("Utf8 error: {0}")]
    Utf8Error(#[from] Utf8Error),
    #[error("Http error: {0}")]
    HttpError(#[from] HttpError),
    #[error("Invalid http resposne status: {0}")]
    InvalidHttpResponseStatus(u16),
    #[error("Send error: {0}")]
    SendError(#[from] SendError<Bytes>),
    #[error("Event Send error: {0}")]
    EventSendError(#[from] SendError<Event>),
    #[error("Server not allow upgrading to websocket")]
    IllegalWebsocketUpgrade(),
    #[error("Illegal action before open")]
    IllegalActionBeforeOpen(),
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

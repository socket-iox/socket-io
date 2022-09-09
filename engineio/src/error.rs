use base64::DecodeError;
use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use serde_json::Error as JsonError;
use std::io::Error as IoError;
use std::str::Utf8Error;
use thiserror::Error;
use tungstenite::Error as WsError;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Invalid packet type: {0}")]
    InvalidPacketType(u8),
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
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

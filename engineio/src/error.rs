use base64::DecodeError;
use reqwest::header::{InvalidHeaderName, InvalidHeaderValue};
use serde_json::Error as JsonError;
use thiserror::Error;

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
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

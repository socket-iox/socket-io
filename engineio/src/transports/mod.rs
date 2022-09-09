use std::time::SystemTime;

use async_trait::async_trait;
use bytes::Bytes;
use reqwest::Url;

use crate::error::Result;

pub(crate) mod polling;

pub enum Payload {
    String(Bytes),
    Binary(Bytes),
}

#[async_trait]
pub trait Transport {
    async fn emit(&self, payload: Payload) -> Result<()>;
}

pub(crate) fn append_hash(url: &Url) -> Url {
    let mut url = url.clone();
    let now_str = format!("{:#?}", SystemTime::now());
    // SAFETY: time string is valid for adler32
    let hash = adler32::adler32(now_str.as_bytes()).unwrap();
    url.query_pairs_mut().append_pair("t", &hash.to_string());
    url
}

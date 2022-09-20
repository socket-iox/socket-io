pub(crate) mod client;
pub(crate) mod error;
pub(crate) mod generator;
pub(crate) mod header;
pub(crate) mod packet;
pub(crate) mod server;
pub(crate) mod socket;
pub(crate) mod transports;

pub use client::Client;
pub use error::Error;
pub(crate) use error::Result;
pub use generator::{Generator, StreamGenerator};
pub use packet::{Packet, PacketType};
pub use socket::Event;

pub(crate) type Sid = std::sync::Arc<String>;

pub const ENGINE_IO_VERSION: i32 = 4;

#[cfg(test)]
pub(crate) mod test {
    use reqwest::Url;

    const RUST_SERVER_URL: &str = "http://localhost:4205";
    const RUST_TIMEOUT_SERVER_URL: &str = "http://localhost:4206";

    pub(crate) fn rust_engine_io_server() -> Url {
        let url =
            std::env::var("RUST_ENGINE_IO_SERVER").unwrap_or_else(|_| RUST_SERVER_URL.to_owned());
        Url::parse(&url).unwrap()
    }

    pub(crate) fn rust_engine_io_timeout_server() -> Url {
        let url = std::env::var("RUST_ENGINE_IO_TIMEOUT_SERVER")
            .unwrap_or_else(|_| RUST_TIMEOUT_SERVER_URL.to_owned());
        Url::parse(&url).unwrap()
    }
}

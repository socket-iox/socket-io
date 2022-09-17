pub mod client;
pub mod error;
pub mod header;
pub mod packet;
pub mod server;
pub(crate) mod socket;
pub(crate) mod transports;

pub use error::Error;
pub(crate) use error::Result;
pub use packet::{Packet, PacketType};
pub use socket::Event;

pub(crate) type Sid = std::sync::Arc<String>;

pub const ENGINE_IO_VERSION: i32 = 4;

#[cfg(test)]
pub(crate) mod test {
    use reqwest::Url;

    const RUST_SERVER_URL: &str = "http://127.0.0.1:4205";

    pub(crate) fn rust_engine_io_server() -> Url {
        let url =
            std::env::var("RUST_ENGINE_IO_SERVER").unwrap_or_else(|_| RUST_SERVER_URL.to_owned());
        Url::parse(&url).unwrap()
    }
}

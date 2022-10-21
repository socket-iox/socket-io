pub(crate) mod ack;
pub(crate) mod callback;
pub(crate) mod client;
pub(crate) mod error;
pub(crate) mod event;
pub(crate) mod packet;
pub(crate) mod payload;
pub(crate) mod server;

mod socket;

pub use ack::AckId;
pub use client::{Client, ClientBuilder, TransportType};
pub use error::{Error, Result};
pub use event::Event;
pub use packet::{Packet, PacketType};
pub use payload::Payload;
pub use server::{Client as ServerClient, Server, ServerBuilder};

pub(crate) type NameSpace = String;

#[cfg(test)]
pub(crate) mod test {
    use url::Url;

    /// The socket.io server for testing runs on port 4200
    const SERVER_URL: &str = "http://localhost:4200";

    pub(crate) fn socket_io_server() -> Url {
        let url = std::env::var("SOCKET_IO_SERVER").unwrap_or_else(|_| SERVER_URL.to_owned());
        let mut url = Url::parse(&url).unwrap();

        if url.path() == "/" {
            url.set_path("/socket.io/");
        }

        url
    }

    /// The rust socket.io server for testing runs on port 4209
    const RUST_SERVER_URL: &str = "http://localhost:4209";

    pub(crate) fn rust_socket_io_server() -> Url {
        let url =
            std::env::var("SOCKET_IO_RUST_SERVER").unwrap_or_else(|_| RUST_SERVER_URL.to_owned());
        let mut url = Url::parse(&url).unwrap();

        if url.path() == "/" {
            url.set_path("/socket.io/");
        }

        url
    }
}

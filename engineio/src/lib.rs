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

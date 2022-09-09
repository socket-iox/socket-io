pub mod error;
pub mod header;
pub mod packet;
pub mod server;

pub use error::Error;
pub(crate) use error::Result;
pub use packet::{Packet, PacketType};

pub(crate) type Sid = std::sync::Arc<String>;

mod builder;
#[allow(clippy::module_inception)]
mod socket;

pub use builder::SocketBuilder;
pub use socket::{Event, Socket};

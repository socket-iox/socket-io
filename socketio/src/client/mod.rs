pub(crate) mod builder;
#[allow(clippy::module_inception)]
pub(crate) mod client;

pub use builder::{ClientBuilder, TransportType};
pub use client::{Client, Socket};

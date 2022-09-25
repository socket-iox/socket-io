use std::time::Duration;

use tokio::time::Instant;

use crate::callback::Callback;

/// Represents an `Ack` as given back to the caller. Holds the internal `id` as
/// well as the current ack'ed state. Holds data which will be accessible as
/// soon as the ack'ed state is set to true. An `Ack` that didn't get ack'ed
/// won't contain data.
#[derive(Debug)]
pub(crate) struct Ack<C> {
    pub id: AckId,
    pub timeout: Duration,
    pub time_started: Instant,
    pub callback: Callback<C>,
}

pub type AckId = usize;

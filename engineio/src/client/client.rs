use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
    task::Poll,
};

use futures_util::{Stream, StreamExt};
use tokio::sync::{mpsc::Receiver, Mutex};

use crate::{error::Result, socket::Socket, Event, Packet};

#[derive(Debug, Clone)]
pub struct Client {
    socket: Socket,
    event_rx: Arc<Mutex<Receiver<Event>>>,
}

impl Client {
    pub(crate) fn new(socket: Socket, event_rx: Receiver<Event>) -> Self {
        Self {
            socket,
            event_rx: Arc::new(Mutex::new(event_rx)),
        }
    }

    pub async fn next_event(&self) -> Option<Event> {
        let mut event_rx = self.event_rx.lock().await;
        event_rx.recv().await
    }
}

impl Deref for Client {
    type Target = Socket;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

impl DerefMut for Client {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.socket
    }
}

impl Stream for Client {
    type Item = Result<Packet>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.socket.poll_next_unpin(cx)
    }
}

use std::{ops::Deref, sync::Arc};

use tokio::sync::{mpsc::Receiver, Mutex};

use crate::{socket::Socket, Event};

#[derive(Debug, Clone)]
pub struct Client {
    socket: Socket,
    _event_rx: Arc<Mutex<Receiver<Event>>>,
}

impl Client {
    pub(crate) fn new(socket: Socket, event_rx: Receiver<Event>) -> Self {
        Self {
            socket,
            _event_rx: Arc::new(Mutex::new(event_rx)),
        }
    }
}

impl Deref for Client {
    type Target = Socket;
    fn deref(&self) -> &Self::Target {
        &self.socket
    }
}

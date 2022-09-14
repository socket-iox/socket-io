use std::sync::Arc;

use tokio::sync::{mpsc::channel, Mutex};

use crate::server::{self, server::ServerInner, Server, ServerOption};

pub struct ServerBuilder {
    port: u16,
    server_option: ServerOption,
    polling_buffer: usize,
    event_size: usize,
}

impl ServerBuilder {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            server_option: Default::default(),
            polling_buffer: 100,
            event_size: 1000,
        }
    }

    pub fn server_option(mut self, server_option: ServerOption) -> Self {
        self.server_option = server_option;
        self
    }

    pub fn polling_buffer(mut self, polling_buffer: usize) -> Self {
        self.polling_buffer = polling_buffer;
        self
    }

    pub fn event_size(mut self, event_size: usize) -> Self {
        self.event_size = event_size;
        self
    }

    pub fn build(self) -> Server {
        let (event_tx, event_rx) = channel(self.event_size);
        Server {
            inner: Arc::new(ServerInner {
                port: self.port,
                server_option: self.server_option,
                id_generator: Default::default(),
                sockets: Default::default(),
                polling_handles: Default::default(),
                polling_buffer: self.polling_buffer,
                event_tx: Arc::new(event_tx),
                event_rx: Arc::new(Mutex::new(event_rx)),
            }),
        }
    }
}

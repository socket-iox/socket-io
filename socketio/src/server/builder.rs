use crate::server::server::Server;
use crate::{callback::Callback, server::client::Client};
use crate::{AckId, NameSpace};
use crate::{Event, Payload};
use engineio::{ServerBuilder as EngineServerBuilder, ServerOption};
use futures_util::future::BoxFuture;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;

#[allow(dead_code)]
pub struct ServerBuilder {
    server_option: ServerOption,
    on: HashMap<NameSpace, HashMap<Event, Callback<Client>>>,
    builder: EngineServerBuilder,
}

#[allow(dead_code)]
impl ServerBuilder {
    pub fn new(port: u16) -> Self {
        Self {
            builder: EngineServerBuilder::new(port),
            server_option: Default::default(),
            on: Default::default(),
        }
    }

    pub fn server_option(mut self, server_option: ServerOption) -> Self {
        self.builder = self.builder.server_option(server_option);
        self
    }

    pub fn on<S: Into<String>, T: Into<Event>, F>(
        mut self,
        namespace: S,
        event: T,
        callback: F,
    ) -> Self
    where
        F: for<'a> std::ops::FnMut(Payload, Client, Option<AckId>) -> BoxFuture<'static, ()>
            + 'static
            + Send
            + Sync,
    {
        let namespace = namespace.into();
        if let Some(on) = self.on.get_mut(&namespace) {
            on.insert(event.into(), Callback::new(callback));
        } else {
            let mut on = HashMap::new();
            on.insert(event.into(), Callback::new(callback));
            self.on.insert(namespace, on);
        }
        self
    }

    pub fn build(self) -> Arc<Server> {
        let engine_server = self.builder.build();
        let mut on = HashMap::new();

        for (k, v) in self.on.into_iter() {
            on.insert(k, Arc::new(RwLock::new(v)));
        }

        Arc::new(Server {
            on,
            engine_server,
            rooms: Default::default(),
            clients: Default::default(),
            sid_generator: Default::default(),
        })
    }
}

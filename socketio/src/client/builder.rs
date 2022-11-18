use std::sync::Arc;

use super::client::{Client, Socket as ClientSocket};
use crate::socket::RawSocket;
use crate::{ack::AckId, socket::Socket};
use crate::{callback::Callback, error::Result, Event, Payload};

use dashmap::DashMap;
use engineio_rs::{HeaderMap, HeaderValue, SocketBuilder as EngineSocketBuilder};
use futures_util::future::BoxFuture;
use tracing::trace;
use url::Url;

/// Flavor of Engine.IO transport.
#[derive(Clone, Eq, PartialEq)]
pub enum TransportType {
    /// Handshakes with polling, upgrades if possible
    Any,
    /// Handshakes with websocket. Does not use polling.
    Websocket,
    /// Handshakes with polling, errors if upgrade fails
    WebsocketUpgrade,
    /// Handshakes with polling
    Polling,
}

/// A builder class for a `socket.io` socket. This handles setting up the client and
/// configuring the callback, the namespace and metadata of the socket. If no
/// namespace is specified, the default namespace `/` is taken. The `connect` method
/// acts the `build` method and returns a connected [`Client`].
#[derive(Clone)]
pub struct ClientBuilder {
    address: String,
    on: Arc<DashMap<Event, Callback<ClientSocket>>>,
    namespace: String,
    opening_headers: Option<HeaderMap>,
    transport_type: TransportType,
    pub(crate) reconnect: bool,
    // None reconnect attempts represent infinity.
    pub(crate) max_reconnect_attempts: Option<usize>,
    pub(crate) reconnect_delay_min: u64,
    pub(crate) reconnect_delay_max: u64,
}

impl ClientBuilder {
    /// Create as client builder from a URL. URLs must be in the form
    /// `[ws or wss or http or https]://[domain]:[port]/[path]`. The
    /// path of the URL is optional and if no port is given, port 80
    /// will be used.
    /// # Example
    /// ```no_run
    /// use socketio_rs::{Payload, ClientBuilder, Socket, AckId};
    /// use serde_json::json;
    /// use futures_util::future::FutureExt;
    ///
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let callback = |payload: Option<Payload>, socket: Socket, need_ack: Option<AckId>| {
    ///         async move {
    ///             match payload {
    ///                 Some(Payload::Json(data)) => println!("Received: {:?}", data),
    ///                 Some(Payload::Binary(bin)) => println!("Received bytes: {:#?}", bin),
    ///                 Some(Payload::Multi(multi)) => println!("Received multi: {:?}", multi),
    ///                 _ => {},
    ///             }
    ///         }.boxed()
    ///     };
    ///
    ///     let mut socket = ClientBuilder::new("http://localhost:4200")
    ///         .namespace("/admin")
    ///         .on("test", callback)
    ///         .connect()
    ///         .await
    ///         .expect("error while connecting");
    ///
    ///     // use the socket
    ///     let json_payload = json!({"token": 123});
    ///
    ///     let result = socket.emit("foo", json_payload).await;
    ///
    ///     assert!(result.is_ok());
    /// }
    /// ```
    pub fn new<T: Into<String>>(address: T) -> Self {
        Self {
            address: address.into(),
            on: Default::default(),
            namespace: "/".to_owned(),
            opening_headers: None,
            transport_type: TransportType::Any,
            reconnect: true,
            // None means infinity
            max_reconnect_attempts: None,
            reconnect_delay_min: 1000,
            reconnect_delay_max: 5000,
        }
    }

    /// Sets the target namespace of the client. The namespace should start
    /// with a leading `/`. Valid examples are e.g. `/admin`, `/foo`.
    /// If the String provided doesn't start with a leading `/`, it is
    /// added manually.
    pub fn namespace<T: Into<String>>(mut self, namespace: T) -> Self {
        let mut nsp = namespace.into();
        if !nsp.starts_with('/') {
            nsp = "/".to_owned() + &nsp;
            trace!("Added `/` to the given namespace: {}", nsp);
        }
        self.namespace = nsp;
        self
    }

    /// Registers a new callback for a certain [`crate::event::Event`]. The event could either be
    /// one of the common events like `message`, `error`, `connect`, `close` or a custom
    /// event defined by a string, e.g. `onPayment` or `foo`.
    ///
    /// # Example
    /// ```rust
    /// use socketio_rs::{ClientBuilder, Payload};
    /// use futures_util::FutureExt;
    ///
    ///  #[tokio::main]
    /// async fn main() {
    ///     let socket = ClientBuilder::new("http://localhost:4200/")
    ///         .namespace("/admin")
    ///         .on("test", |payload: Option<Payload>, _, _| {
    ///             async move {
    ///                 match payload {
    ///                     Some(Payload::Json(data)) => println!("Received: {:?}", data),
    ///                     Some(Payload::Binary(bin)) => println!("Received bytes: {:#?}", bin),
    ///                     Some(Payload::Multi(multi)) => println!("Received multi: {:?}", multi),
    ///                     _ => {},
    ///                 }
    ///             }
    ///             .boxed()
    ///         })
    ///         .on("error", |err, _, _| async move { eprintln!("Error: {:#?}", err) }.boxed())
    ///         .connect()
    ///         .await;
    /// }
    /// ```
    ///
    /// # Issues with type inference for the callback method
    ///
    /// Currently stable Rust does not contain types like `AsyncFnMut`.
    /// That is why this library uses the type `FnMut(..) -> BoxFuture<_>`,
    /// which basically represents a closure or function that returns a
    /// boxed future that can be executed in an async executor.
    /// The complicated constraints for the callback function
    /// bring the Rust compiler to it's limits, resulting in confusing error
    /// messages when passing in a variable that holds a closure (to the `on` method).
    /// In order to make sure type inference goes well, the [`futures_util::FutureExt::boxed`]
    /// method can be used on an async block (the future) to make sure the return type
    /// is conform with the generic requirements. An example can be found here:
    ///
    /// ```rust
    /// use socketio_rs::{ClientBuilder, Payload};
    /// use futures_util::FutureExt;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let callback = |payload: Option<Payload>, _, _| {
    ///             async move {
    ///                 match payload {
    ///                     Some(Payload::Json(data)) => println!("Received: {:?}", data),
    ///                     Some(Payload::Binary(bin)) => println!("Received bytes: {:#?}", bin),
    ///                     Some(Payload::Multi(multi)) => println!("Received multi: {:?}", multi),
    ///                     _ => {},
    ///                 }
    ///             }
    ///             .boxed() // <-- this makes sure we end up with a `BoxFuture<_>`
    ///         };
    ///
    ///     let socket = ClientBuilder::new("http://localhost:4200/")
    ///         .namespace("/admin")
    ///         .on("test", callback)
    ///         .connect()
    ///         .await;
    /// }
    /// ```
    ///
    pub fn on<T: Into<Event>, F>(self, event: T, callback: F) -> Self
    where
        F: for<'a> std::ops::FnMut(
                Option<Payload>,
                ClientSocket,
                Option<AckId>,
            ) -> BoxFuture<'static, ()>
            + 'static
            + Send
            + Sync,
    {
        let callback = Callback::new(callback);
        let event = event.into();
        // SAFETY: Lock is held for such amount of time no code paths lead to a panic while lock is held
        let on = self.on.clone();
        tokio::spawn(async move {
            on.insert(event, callback);
        });
        self
    }

    /// Sets custom http headers for the opening request. The headers will be passed to the underlying
    /// transport type (either websockets or polling) and then get passed with every request thats made.
    /// via the transport layer.
    /// # Example
    /// ```rust
    /// use socketio_rs::{ClientBuilder, Payload};
    /// use futures_util::future::FutureExt;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let socket = ClientBuilder::new("http://localhost:4200/")
    ///         .namespace("/admin")
    ///         .on("error", |err, _, _| async move { eprintln!("Error: {:#?}", err) }.boxed())
    ///         .opening_header("accept-encoding", "application/json")
    ///         .connect()
    ///         .await;
    /// }
    /// ```
    pub fn opening_header<T: Into<HeaderValue>, K: Into<String>>(mut self, key: K, val: T) -> Self {
        match self.opening_headers {
            Some(ref mut map) => {
                map.insert(key.into(), val.into());
            }
            None => {
                let mut map = HeaderMap::default();
                map.insert(key.into(), val.into());
                self.opening_headers = Some(map);
            }
        }
        self
    }

    /// Specifies which EngineIO [`TransportType`] to use.
    ///
    /// # Example
    /// ```no_run
    /// use socketio_rs::{ClientBuilder, TransportType};
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let socket = ClientBuilder::new("http://localhost:4200/")
    ///         // Use websockets to handshake and connect.
    ///         .transport_type(TransportType::Websocket)
    ///         .connect()
    ///         .await
    ///         .expect("connection failed");
    /// }
    /// ```
    pub fn transport_type(mut self, transport_type: TransportType) -> Self {
        self.transport_type = transport_type;

        self
    }

    /// Connects the socket to a certain endpoint. This returns a connected
    /// [`Client`] instance. This method returns an [`std::result::Result::Err`]
    /// value if something goes wrong during connection. Also starts a separate
    /// thread to start polling for packets. Used with callbacks.
    /// # Example
    /// ```no_run
    /// use socketio_rs::{ClientBuilder, Payload};
    /// use serde_json::json;
    /// use futures_util::future::FutureExt;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let mut socket = ClientBuilder::new("http://localhost:4200/")
    ///         .namespace("/admin")
    ///         .on("error", |err, _, _| async move { eprintln!("Error: {:#?}", err) }.boxed())
    ///         .connect()
    ///         .await
    ///         .expect("connection failed");
    ///
    ///     // use the socket
    ///     let json_payload = json!({"token": 123});
    ///
    ///     let result = socket.emit("foo", json_payload).await;
    ///
    ///     assert!(result.is_ok());
    /// }
    /// ```
    pub async fn connect(self) -> Result<Client> {
        let client = Client::new(self).await;
        if let Ok(c) = &client {
            c.poll_callback();
        }
        client
    }

    pub fn reconnect(mut self, reconnect: bool) -> Self {
        self.reconnect = reconnect;
        self
    }

    pub fn reconnect_delay(mut self, min: u64, max: u64) -> Self {
        self.reconnect_delay_min = min;
        self.reconnect_delay_max = max;

        self
    }

    pub fn max_reconnect_attempts(mut self, reconnect_attempts: usize) -> Self {
        self.max_reconnect_attempts = Some(reconnect_attempts);
        self
    }

    #[cfg(test)]
    pub(crate) async fn connect_client(self) -> Result<Client> {
        Client::new(self.clone()).await
    }

    pub(crate) async fn connect_socket(&self) -> Result<Socket<ClientSocket>> {
        // Parse url here rather than in new to keep new returning Self.
        let mut url = Url::parse(&self.address)?;

        if url.path() == "/" {
            url.set_path("/socket.io/");
        }

        let mut builder = EngineSocketBuilder::new(url);

        if let Some(headers) = &self.opening_headers {
            builder = builder.headers(headers.clone());
        }

        let engine_client = match self.transport_type {
            TransportType::Any => builder.build_with_fallback().await?,
            TransportType::Polling => builder.build_polling().await?,
            TransportType::Websocket => builder.build_websocket().await?,
            TransportType::WebsocketUpgrade => builder.build_websocket_with_upgrade().await?,
        };

        let inner_socket = RawSocket::client_end(engine_client);
        let socket = Socket::<ClientSocket>::new(
            inner_socket,
            self.namespace.clone(),
            self.on.clone(),
            Arc::new(|s| s.into()),
        );

        socket.connect().await?;
        Ok(socket)
    }
}

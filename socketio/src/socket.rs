use std::{
    collections::HashMap,
    fmt::Debug,
    ops::DerefMut,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{ready, Poll},
    time::Duration,
};

use crate::{
    ack::Ack,
    callback::Callback,
    error::Result,
    packet::{AckIdGenerator, Packet, PacketType},
    AckId, Error, Event, Payload,
};

use async_stream::try_stream;
use bytes::Bytes;
use engineio_rs::{
    Packet as EnginePacket, PacketType as EnginePacketType, Socket as EngineSocket, StreamGenerator,
};
use futures_util::{future::BoxFuture, FutureExt, Stream, StreamExt};
use serde_json::{from_str, Value};
use tokio::{
    sync::{Mutex, RwLock},
    time::Instant,
};
use tracing::trace;

/// A socket which handles communication with the server. It's initialized with
/// a specific address as well as an optional namespace to connect to. If `None`
/// is given the client will connect to the default namespace `"/"`. Both client side
/// and server side, use this Client.
#[derive(Clone)]
pub struct Socket<C> {
    // namespace, for multiplexing messages
    pub(crate) nsp: String,
    /// The inner socket client to delegate the methods to.
    socket: RawSocket,
    on: Arc<RwLock<HashMap<Event, Callback<C>>>>,
    outstanding_acks: Arc<RwLock<Vec<Ack<C>>>>,
    is_connected: Arc<AtomicBool>,
    callback_client_fn: Arc<dyn Fn(Self) -> C + Send + Sync>,
    ack_id_gen: Arc<AckIdGenerator>,
}

#[derive(Clone)]
pub(crate) struct RawSocket {
    engine_client: Arc<EngineSocket>,
    generator: Arc<Mutex<StreamGenerator<Packet, Error>>>,
    is_server: bool,
}

impl<C: Clone> Socket<C> {
    /// Creates a socket with a certain address to connect to as well as a
    /// namespace. If `None` is passed in as namespace, the default namespace
    /// `"/"` is taken.
    /// ```
    pub(crate) fn new<T: Into<String>>(
        socket: RawSocket,
        namespace: T,
        on: Arc<RwLock<HashMap<Event, Callback<C>>>>,
        callback_client_fn: Arc<dyn Fn(Self) -> C + Send + Sync>,
    ) -> Self {
        Socket {
            socket,
            nsp: namespace.into(),
            on,
            outstanding_acks: Arc::new(RwLock::new(Vec::new())),
            is_connected: Arc::new(AtomicBool::new(true)),
            callback_client_fn,
            ack_id_gen: Default::default(),
        }
    }

    /// Connects the client to a server. Afterwards the `emit_*` methods can be
    /// called to interact with the server.
    pub(crate) async fn connect(&self) -> Result<()> {
        // Connect the underlying socket
        self.socket.connect().await?;

        // construct the opening packet
        let open_packet = Packet::new(PacketType::Connect, self.nsp.clone(), None, None, 0, None);

        self.socket.send(open_packet).await?;

        Ok(())
    }

    /// Sends a message to the server using the underlying `engine.io` protocol.
    /// This message takes an event, which could either be one of the common
    /// events like "message" or "error" or a custom event like "foo". But be
    /// careful, the data string needs to be valid JSON. It's recommended to use
    /// a library like `serde_json` to serialize the data properly.
    ///
    /// # Example
    /// ```no_run
    /// use socketio_rs::{ClientBuilder, Client, AckId, Payload};
    /// use serde_json::json;
    /// use futures_util::FutureExt;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let mut socket = ClientBuilder::new("http://localhost:4200/")
    ///         .on("test", |payload: Payload, socket: Client, need_ack: Option<AckId>| {
    ///             async move {
    ///                 println!("Received: {:#?}", payload);
    ///                 socket.emit("test", json!({"hello": true})).await.expect("Server unreachable");
    ///             }.boxed()
    ///         })
    ///         .connect()
    ///         .await
    ///         .expect("connection failed");
    ///
    ///     let json_payload = json!({"token": 123});
    ///
    ///     let result = socket.emit("foo", json_payload).await;
    ///
    ///     assert!(result.is_ok());
    /// }
    /// ```
    #[inline]
    pub async fn emit<E, D>(&self, event: E, data: D) -> Result<()>
    where
        E: Into<Event>,
        D: Into<Payload>,
    {
        if !self.is_connected.load(Ordering::Acquire) {
            return Err(Error::IllegalActionBeforeOpen());
        }
        self.socket.emit(&self.nsp, event.into(), data.into()).await
    }

    #[inline]
    pub async fn ack<D>(&self, id: usize, data: D) -> Result<()>
    where
        D: Into<Payload>,
    {
        if !self.is_connected.load(Ordering::Acquire) {
            return Err(Error::IllegalActionBeforeOpen());
        }
        self.socket.ack(&self.nsp, id, data.into()).await
    }

    #[inline]
    pub(crate) async fn handshake(&self, data: String) -> Result<()> {
        if !self.is_connected.load(Ordering::Acquire) {
            return Err(Error::IllegalActionBeforeOpen());
        }
        self.socket.handshake(&self.nsp, data).await
    }

    /// Disconnects this client from the server by sending a `socket.io` closing
    /// packet.
    /// # Example
    /// ```no_run
    /// use socketio_rs::{ClientBuilder, Client, AckId, Payload};
    /// use serde_json::json;
    /// use futures_util::{FutureExt, future::BoxFuture};
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     // apparently the syntax for functions is a bit verbose as rust currently doesn't
    ///     // support an `AsyncFnMut` type that conform with async functions
    ///     fn handle_test(payload: Payload, socket: Client, need_ack: Option<AckId>) -> BoxFuture<'static, ()> {
    ///         async move {
    ///             println!("Received: {:#?}", payload);
    ///             socket.emit("test", json!({"hello": true})).await.expect("Server unreachable");
    ///         }.boxed()
    ///     }
    ///
    ///     let mut socket = ClientBuilder::new("http://localhost:4200/")
    ///         .on("test", handle_test)
    ///         .connect()
    ///         .await
    ///         .expect("connection failed");
    ///
    ///     let json_payload = json!({"token": 123});
    ///
    ///     socket.emit("foo", json_payload).await;
    ///
    ///     // disconnect from the server
    ///     socket.disconnect().await;
    /// }
    /// ```
    pub async fn disconnect(&self) -> Result<()> {
        if self.is_connected.load(Ordering::Acquire) {
            self.is_connected.store(false, Ordering::Release);
        }
        let disconnect_packet = Packet::new(
            PacketType::Disconnect,
            self.nsp.clone(),
            None,
            None,
            0,
            None,
        );

        self.socket.send(disconnect_packet).await?;
        self.socket.disconnect().await?;

        Ok(())
    }

    /// Sends a message to the server but `alloc`s an `ack` to check whether the
    /// server responded in a given time span. This message takes an event, which
    /// could either be one of the common events like "message" or "error" or a
    /// custom event like "foo", as well as a data parameter. But be careful,
    /// in case you send a [`Payload::String`], the string needs to be valid JSON.
    /// It's even recommended to use a library like serde_json to serialize the data properly.
    /// It also requires a timeout `Duration` in which the client needs to answer.
    /// If the ack is acked in the correct time span, the specified callback is
    /// called. The callback consumes a [`Payload`] which represents the data send
    /// by the server.
    ///
    /// Please note that the requirements on the provided callbacks are similar to the ones
    /// for [`crate::asynchronous::ClientBuilder::on`].
    /// # Example
    /// ```no_run
    /// use socketio_rs::{ClientBuilder, Client, Payload};
    /// use serde_json::json;
    /// use std::time::Duration;
    /// use std::thread::sleep;
    /// use futures_util::FutureExt;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let mut socket = ClientBuilder::new("http://localhost:4200/")
    ///         .on("foo", |payload: Payload, _, _| async move { println!("Received: {:#?}", payload) }.boxed())
    ///         .connect()
    ///         .await
    ///         .expect("connection failed");
    ///
    ///     let ack_callback = |message: Payload, socket: Client, _| {
    ///         async move {
    ///             match message {
    ///                 Payload::String(str) => println!("{}", str),
    ///                 Payload::Binary(bytes) => println!("Received bytes: {:#?}", bytes),
    ///             }
    ///         }.boxed()
    ///     };    
    ///
    ///
    ///     let payload = json!({"token": 123});
    ///     socket.emit_with_ack("foo", payload, Duration::from_secs(2), ack_callback).await.unwrap();
    ///
    ///     sleep(Duration::from_secs(2));
    /// }
    /// ```
    #[inline]
    pub async fn emit_with_ack<F, E, D>(
        &self,
        event: E,
        data: D,
        timeout: Duration,
        callback: F,
    ) -> Result<()>
    where
        F: for<'a> std::ops::FnMut(Payload, C, Option<AckId>) -> BoxFuture<'static, ()>
            + 'static
            + Send
            + Sync,
        E: Into<Event>,
        D: Into<Payload>,
    {
        if !self.is_connected.load(Ordering::Acquire) {
            return Err(Error::IllegalActionBeforeOpen());
        }
        let id = self.ack_id_gen.generate();
        let socket_packet = self.socket.build_packet_for_payload(
            data.into(),
            Some(event.into()),
            &self.nsp,
            Some(id),
            false,
        )?;

        let ack = Ack {
            id,
            time_started: Instant::now(),
            timeout,
            callback: Callback::new(callback),
        };

        // add the ack to the tuple of outstanding acks
        self.outstanding_acks.write().await.push(ack);

        self.socket.send(socket_packet).await
    }

    async fn callback<P: Into<Payload>>(
        &self,
        event: &Event,
        payload: P,
        need_ack: Option<AckId>,
    ) -> Result<()> {
        let mut on = self.on.write().await;
        let lock = on.deref_mut();
        trace!("callback on keys {:?}", lock.keys());
        if let Some(callback) = lock.get_mut(event) {
            let c = (self.callback_client_fn)((self).clone());
            trace!("do callback {:?}", event);
            callback(payload.into(), c, need_ack).await;
        }
        drop(on);
        Ok(())
    }

    /// Handles the incoming acks and classifies what callbacks to call and how.
    #[inline]
    async fn handle_ack(&self, socket_packet: &Packet) -> Result<()> {
        let mut to_be_removed = Vec::new();
        if let Some(id) = socket_packet.id {
            for (index, ack) in self.outstanding_acks.write().await.iter_mut().enumerate() {
                if ack.id == id {
                    to_be_removed.push(index);

                    if ack.time_started.elapsed() < ack.timeout {
                        if let Some(ref payload) = socket_packet.data {
                            ack.callback.deref_mut()(
                                Payload::String(payload.to_owned()),
                                (self.callback_client_fn)(self.clone()),
                                None,
                            )
                            .await;
                        }
                        if let Some(ref attachments) = socket_packet.attachments {
                            if let Some(payload) = attachments.get(0) {
                                ack.callback.deref_mut()(
                                    Payload::Binary(payload.to_owned()),
                                    (self.callback_client_fn)(self.clone()),
                                    None,
                                )
                                .await;
                            }
                        }
                    } else {
                        trace!("Received an Ack that is now timed out (elapsed time was longer than specified duration)");
                    }
                }
            }
            for index in to_be_removed {
                self.outstanding_acks.write().await.remove(index);
            }
        }
        Ok(())
    }

    /// Handles a binary event.
    #[inline]
    async fn handle_binary_event(&self, packet: &Packet) -> Result<()> {
        let event = if let Some(string_data) = &packet.data {
            string_data.replace('\"', "").into()
        } else {
            Event::Message
        };

        if let Some(attachments) = &packet.attachments {
            if let Some(binary_payload) = attachments.get(0) {
                self.callback(
                    &event,
                    Payload::Binary(binary_payload.to_owned()),
                    packet.id,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// A method that parses a packet and eventually calls the corresponding
    // callback with the supplied data.
    async fn handle_event(&self, packet: &Packet) -> Result<()> {
        if packet.data.is_none() {
            return Ok(());
        }
        let data = packet.data.as_ref().unwrap();

        // a socketio message always comes in one of the following two flavors (both JSON):
        // 1: `["event", "msg"]`
        // 2: `["msg"]`
        // in case 2, the message is ment for the default message event, in case 1 the event
        // is specified
        if let Ok(Value::Array(contents)) = from_str::<Value>(data) {
            let (event, data) = if contents.len() > 1 {
                // case 1
                (
                    contents
                        .get(0)
                        .map(|value| match value {
                            Value::String(ev) => ev,
                            _ => "message",
                        })
                        .unwrap_or("message")
                        .into(),
                    contents.get(1).ok_or(Error::IncompletePacket())?,
                )
            } else {
                // case 2
                (
                    Event::Message,
                    contents.get(0).ok_or(Error::IncompletePacket())?,
                )
            };

            // call the correct callback
            self.callback(&event, data.to_string(), packet.id).await?;
        }

        Ok(())
    }

    pub(crate) async fn handle_connect(&self) -> Result<()> {
        self.is_connected.store(true, Ordering::Release);
        trace!("callback connect");
        self.callback(&Event::Connect, "", None).await?;
        Ok(())
    }

    /// Handles the incoming messages and classifies what callbacks to call and how.
    /// This method is later registered as the callback for the `on_data` event of the
    /// engineio client.
    #[inline]
    async fn handle_socketio_packet(&self, packet: &Packet) -> Result<()> {
        if packet.nsp == self.nsp {
            match packet.ptype {
                PacketType::Ack | PacketType::BinaryAck => {
                    if let Err(err) = self.handle_ack(packet).await {
                        self.callback(&Event::Error, err.to_string(), None).await?;
                        return Err(err);
                    }
                }
                PacketType::BinaryEvent => {
                    if let Err(err) = self.handle_binary_event(packet).await {
                        self.callback(&Event::Error, err.to_string(), None).await?;
                    }
                }
                PacketType::Connect => self.handle_connect().await?,
                PacketType::Disconnect => {
                    self.is_connected.store(false, Ordering::Release);
                    self.callback(&Event::Close, "", None).await?;
                }
                PacketType::ConnectError => {
                    self.is_connected.store(false, Ordering::Release);
                    self.callback(
                        &Event::Error,
                        String::from("Received an ConnectError frame: ")
                            + packet
                                .data
                                .as_ref()
                                .unwrap_or(&String::from("\"No error message provided\"")),
                        None,
                    )
                    .await?;
                }
                PacketType::Event => {
                    if let Err(err) = self.handle_event(packet).await {
                        self.callback(&Event::Error, err.to_string(), None).await?;
                    }
                }
            }
        }
        Ok(())
    }
}

impl<C: Clone> Stream for Socket<C> {
    type Item = Result<Packet>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        loop {
            // poll for the next payload
            let next = ready!(self.socket.poll_next_unpin(cx));
            match next {
                None => {
                    // end the stream if the underlying one is closed
                    return Poll::Ready(None);
                }
                Some(Err(err)) => {
                    // call the error callback
                    ready!(
                        Box::pin(self.callback(&Event::Error, err.to_string(), None))
                            .poll_unpin(cx)
                    )?;
                    return Poll::Ready(Some(Err(err)));
                }
                Some(Ok(packet)) => {
                    // if this packet is not meant for the current namespace, skip it an poll for the next one
                    if packet.nsp == self.nsp {
                        ready!(Box::pin(self.handle_socketio_packet(&packet)).poll_unpin(cx))?;
                        return Poll::Ready(Some(Ok(packet)));
                    }
                }
            }
        }
    }
}

impl RawSocket {
    /// Creates an instance of `Socket`.
    pub(super) fn client_end(engine_client: EngineSocket) -> Self {
        RawSocket {
            engine_client: Arc::new(engine_client.clone()),
            generator: Arc::new(Mutex::new(StreamGenerator::new(Self::stream(
                engine_client,
            )))),
            is_server: false,
        }
    }

    pub(super) fn server_end(engine_client: EngineSocket) -> Self {
        RawSocket {
            engine_client: Arc::new(engine_client.clone()),
            generator: Arc::new(Mutex::new(StreamGenerator::new(Self::stream(
                engine_client,
            )))),
            is_server: true,
        }
    }

    /// Connects to the server. This includes a connection of the underlying
    /// engine.io client and afterwards an opening socket.io request.
    pub async fn connect(&self) -> Result<()> {
        if !self.is_server {
            self.engine_client.connect().await?;
        }

        Ok(())
    }

    /// Disconnects from the server by sending a socket.io `Disconnect` packet. This results
    /// in the underlying engine.io transport to get closed as well.
    pub async fn disconnect(&self) -> Result<()> {
        if self.is_engineio_connected() {
            self.engine_client.disconnect().await?;
        }

        Ok(())
    }

    /// Sends a `socket.io` packet to the server using the `engine.io` client.
    pub async fn send(&self, packet: Packet) -> Result<()> {
        if !self.is_engineio_connected() {
            trace!("socket emit before open {:?}", packet);
            return Err(Error::IllegalActionBeforeOpen());
        }

        // the packet, encoded as an engine.io message packet
        let engine_packet = EnginePacket::new(EnginePacketType::Message, Bytes::from(&packet));
        self.engine_client.emit(engine_packet).await?;

        if let Some(attachments) = packet.attachments {
            for attachment in attachments {
                let engine_packet = EnginePacket::new(EnginePacketType::MessageBinary, attachment);
                self.engine_client.emit(engine_packet).await?;
            }
        }

        Ok(())
    }

    pub async fn ack(&self, nsp: &str, id: usize, data: Payload) -> Result<()> {
        let socket_packet = self.build_packet_for_payload(data, None, nsp, Some(id), true)?;

        self.send(socket_packet).await
    }

    /// Emits to certain event with given data. The data needs to be JSON,
    /// otherwise this returns an `InvalidJson` error.
    pub async fn emit(&self, nsp: &str, event: Event, data: Payload) -> Result<()> {
        let socket_packet = self.build_packet_for_payload(data, Some(event), nsp, None, false)?;

        self.send(socket_packet).await
    }

    pub(crate) async fn handshake(&self, nsp: &str, data: String) -> Result<()> {
        let socket_packet = Packet::new(
            PacketType::Connect,
            nsp.to_owned(),
            Some(data),
            None,
            0,
            None,
        );
        self.send(socket_packet).await
    }

    /// Returns a packet for a payload, could be used for bot binary and non binary
    /// events and acks. Convenance method.
    #[inline]
    pub(crate) fn build_packet_for_payload<'a>(
        &'a self,
        payload: Payload,
        event: Option<Event>,
        nsp: &'a str,
        id: Option<usize>,
        is_ack: bool,
    ) -> Result<Packet> {
        match payload {
            Payload::Binary(bin_data) => Ok(Packet::new(
                if id.is_some() && is_ack {
                    PacketType::BinaryAck
                } else {
                    PacketType::BinaryEvent
                },
                nsp.to_owned(),
                event.map(|event| serde_json::Value::String(event.into()).to_string()),
                id,
                1,
                Some(vec![bin_data]),
            )),
            Payload::String(str_data) => {
                let payload = match event {
                    None => format!("[{}]", str_data),
                    Some(event) => match serde_json::from_str::<serde_json::Value>(&str_data) {
                        Ok(_) => format!("[\"{}\",{}]", String::from(event), str_data),
                        Err(_) => format!("[\"{}\",\"{}\"]", String::from(event), str_data),
                    },
                };

                Ok(Packet::new(
                    if id.is_some() && is_ack {
                        PacketType::Ack
                    } else {
                        PacketType::Event
                    },
                    nsp.to_owned(),
                    Some(payload),
                    id,
                    0,
                    None,
                ))
            }
        }
    }

    fn stream(client: EngineSocket) -> Pin<Box<impl Stream<Item = Result<Packet>> + Send>> {
        Box::pin(try_stream! {
            for await received_data in client.clone() {
                let packet = received_data?;
                if packet.ptype == EnginePacketType::Message || packet.ptype == EnginePacketType::MessageBinary {
                    let packet = Self::handle_engineio_packet(packet, client.clone()).await?;
                    yield packet;
                }
            }
        })
    }

    /// Handles new incoming engineio packets
    async fn handle_engineio_packet(
        packet: EnginePacket,
        mut client: EngineSocket,
    ) -> Result<Packet> {
        let mut socket_packet = Packet::try_from(&packet.data)?;

        // Only handle attachments if there are any
        if socket_packet.attachment_count > 0 {
            let mut attachments_left = socket_packet.attachment_count;
            let mut attachments = Vec::new();
            while attachments_left > 0 {
                // TODO: This is not nice! Find a different way to peek the next element while mapping the stream
                let next = client.next().await.unwrap();
                match next {
                    Err(err) => return Err(err.into()),
                    Ok(packet) => match packet.ptype {
                        EnginePacketType::MessageBinary | EnginePacketType::Message => {
                            attachments.push(packet.data);
                            attachments_left -= 1;
                        }
                        _ => {
                            return Err(Error::InvalidAttachmentPacketType(packet.ptype.into()));
                        }
                    },
                }
            }
            socket_packet.attachments = Some(attachments);
        }

        Ok(socket_packet)
    }

    fn is_engineio_connected(&self) -> bool {
        self.engine_client.is_connected()
    }
}

impl Stream for RawSocket {
    type Item = Result<Packet>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut lock = ready!(Box::pin(self.generator.lock()).poll_unpin(cx));
        lock.poll_next_unpin(cx)
    }
}

impl Debug for RawSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socket")
            .field("engine_client", &self.engine_client)
            .field("is_server", &self.is_server)
            .field("connected", &self.is_engineio_connected())
            .finish()
    }
}

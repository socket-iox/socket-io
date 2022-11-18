use std::{
    fmt::Debug,
    ops::DerefMut,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    ack::Ack,
    callback::Callback,
    error::Result,
    packet::{AckIdGenerator, Packet, PacketType},
    payload::RawPayload,
    AckId, Error, Event, Payload,
};

use async_stream::try_stream;
use bytes::Bytes;
use dashmap::DashMap;
use engineio_rs::{
    Packet as EnginePacket, PacketType as EnginePacketType, Socket as EngineSocket, StreamGenerator,
};
use futures_util::{future::BoxFuture, Stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::{
    sync::{Mutex, RwLock},
    time::Instant,
};
use tracing::error;
use tracing::{trace, warn};

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
    on: Arc<DashMap<Event, Callback<C>>>,
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

#[derive(Serialize)]
struct BinaryPlaceHolder {
    _placeholder: bool,
    num: usize,
}

impl BinaryPlaceHolder {
    fn new(num: usize) -> Self {
        Self {
            _placeholder: true,
            num,
        }
    }
}

impl<C: Clone + Send + 'static> Socket<C> {
    /// Creates a socket with a certain address to connect to as well as a
    /// namespace. If `None` is passed in as namespace, the default namespace
    /// `"/"` is taken.
    /// ```
    pub(crate) fn new<T: Into<String>>(
        socket: RawSocket,
        namespace: T,
        on: Arc<DashMap<Event, Callback<C>>>,
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
    #[cfg(feature = "client")]
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
    /// use socketio_rs::{ClientBuilder, Socket, AckId, Payload};
    /// use serde_json::json;
    /// use futures_util::FutureExt;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let mut socket = ClientBuilder::new("http://localhost:4200/")
    ///         .on("test", |payload: Option<Payload>, socket: Socket, need_ack: Option<AckId>| {
    ///             async move {
    ///                 println!("Received: {:?}", payload);
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

    #[cfg(feature = "server")]
    #[inline]
    pub(crate) async fn handshake(&self, data: Value) -> Result<()> {
        if !self.is_connected.load(Ordering::Acquire) {
            return Err(Error::IllegalActionBeforeOpen());
        }
        self.socket.handshake(&self.nsp, data).await
    }

    /// Disconnects this client from the server by sending a `socket.io` closing
    /// packet.
    /// # Example
    /// ```no_run
    /// use socketio_rs::{ClientBuilder, Socket, AckId, Payload};
    /// use serde_json::json;
    /// use futures_util::{FutureExt, future::BoxFuture};
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     // apparently the syntax for functions is a bit verbose as rust currently doesn't
    ///     // support an `AsyncFnMut` type that conform with async functions
    ///     fn handle_test(payload: Option<Payload>, socket: Socket, need_ack: Option<AckId>) -> BoxFuture<'static, ()> {
    ///         async move {
    ///             println!("Received: {:?}", payload);
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
    /// custom event like "foo", as well as a data parameter.
    /// It also requires a timeout `Duration` in which the client needs to answer.
    /// If the ack is acked in the correct time span, the specified callback is
    /// called. The callback consumes a [`Payload`] which represents the data send
    /// by the server.
    ///
    /// Please note that the requirements on the provided callbacks are similar to the ones
    /// for [`crate::asynchronous::ClientBuilder::on`].
    /// # Example
    /// ```no_run
    /// use socketio_rs::{ClientBuilder, Socket, Payload};
    /// use serde_json::json;
    /// use std::time::Duration;
    /// use std::thread::sleep;
    /// use futures_util::FutureExt;
    ///
    /// #[tokio::main]
    /// async fn main() {
    ///     let mut socket = ClientBuilder::new("http://localhost:4200/")
    ///         .on("foo", |payload: Option<Payload>, _, _| async move { println!("Received: {:#?}", payload) }.boxed())
    ///         .connect()
    ///         .await
    ///         .expect("connection failed");
    ///
    ///     let ack_callback = |message: Option<Payload>, socket: Socket, _| {
    ///         async move {
    ///                 println!("{:?}", message);
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
        F: for<'a> std::ops::FnMut(Option<Payload>, C, Option<AckId>) -> BoxFuture<'static, ()>
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
        let packet = RawSocket::build_packet_for_payload(
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

        trace!("socket emit_with_ack {:?}", packet);
        self.socket.send(packet).await
    }

    async fn callback(&self, event: &Event, payload: Option<Payload>, need_ack: Option<AckId>) {
        let self_clone = self.clone();
        let event = event.to_owned();
        tokio::spawn(async move {
            if let Some(mut callback) = self_clone.on.get_mut(&event) {
                let c = (self_clone.callback_client_fn)((self_clone).clone());
                trace!("do callback {:?}", event);
                callback(payload, c, need_ack).await;
                trace!("done callback {:?}", event);
            }
        });
    }

    /// Handles the incoming acks and classifies what callbacks to call and how.
    #[inline]
    async fn handle_ack(&self, packet: &Packet, is_binary: bool) -> Result<()> {
        //TODO: fix multi
        let mut to_be_removed = Vec::new();
        if let Some(id) = packet.id {
            for (index, ack) in self.outstanding_acks.write().await.iter_mut().enumerate() {
                if ack.id == id {
                    to_be_removed.push(index);

                    if ack.time_started.elapsed() < ack.timeout {
                        trace!("ack packet {:?}", packet);
                        let payload = if is_binary {
                            Self::decode_binary_payload(&packet.data, &packet.attachments, false)
                        } else {
                            Self::decode_event_payload(packet, false)
                        };

                        trace!("decode ack payload {:?}", payload);

                        ack.callback.deref_mut()(
                            payload,
                            (self.callback_client_fn)(self.clone()),
                            None,
                        )
                        .await;
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
        trace!("handle_binary_event {:?}", packet);
        let event = match &packet.data {
            Some(Value::String(e)) => Event::from(e.to_owned()),
            Some(Value::Array(array)) => match array.first() {
                Some(Value::String(e)) => Event::from(e.to_owned()),
                _ => Event::Message,
            },
            _ => Event::Message,
        };

        let payload = Self::decode_binary_payload(&packet.data, &packet.attachments, true);
        self.callback(&event, payload, packet.id).await;

        Ok(())
    }

    fn decode_binary_payload(
        data: &Option<Value>,
        attachments: &Option<Vec<Bytes>>,
        skip_event: bool,
    ) -> Option<Payload> {
        match &data {
            Some(Value::Array(vec)) => match Self::decode_binary(vec, attachments, skip_event) {
                Ok(payload) => Some(payload),
                Err(e) => {
                    error!("decode binary event: {}", e.to_string());
                    None
                }
            },
            _ => None,
        }
    }

    fn decode_binary(
        vec: &Vec<Value>,
        attachments: &Option<Vec<Bytes>>,
        skip_event: bool,
    ) -> Result<Payload> {
        trace!(
            "do_decode_binary_payload vec {:?}, attachments {:?}",
            vec,
            attachments
        );
        let mut vec_payload = vec![];
        for (index, value) in vec.iter().enumerate() {
            if skip_event && index == 0 {
                continue;
            }
            trace!("do_decode_binary_payload {:?}", value);
            if value.get("_placeholder").is_some() {
                let index = value
                    .get("num")
                    .ok_or(Error::InvalidPacket())?
                    .as_u64()
                    .ok_or(Error::InvalidPacket())? as usize;
                if let Some(atts) = attachments {
                    let b = atts.get(index).ok_or(Error::InvalidPacket())?.to_owned();
                    vec_payload.push(RawPayload::Binary(b));
                } else {
                    return Err(Error::InvalidPacket());
                }
            } else {
                vec_payload.push(RawPayload::Json(value.to_owned()))
            }
        }

        if vec_payload.len() == 1 {
            // SAFETY: len checked before
            let payload = vec_payload.pop().unwrap();
            return Ok(payload.into());
        }
        Ok(Payload::Multi(vec_payload))
    }

    /// A method for handling the Event Client Packets.
    // this could only be called with an event
    async fn handle_event(&self, packet: &Packet) -> Result<()> {
        // the string must be a valid json array with the event at index 0 and the
        // payload at index 1. if no event is specified, the message callback is used
        if let Some(serde_json::Value::Array(contents)) = &packet.data {
            let event: Event = if contents.len() > 1 {
                contents
                    .first()
                    .map(|value| match value {
                        serde_json::Value::String(ev) => ev,
                        _ => "message",
                    })
                    .unwrap_or("message")
                    .into()
            } else {
                Event::Message
            };

            let payload = Self::decode_event_payload(packet, true);
            self.callback(&event, payload, packet.id).await;
        } else {
            warn!("handle_event invalid packet data {:?}", packet.data);
        }

        Ok(())
    }

    fn decode_event_payload(packet: &Packet, skip_event: bool) -> Option<Payload> {
        match packet.data {
            Some(serde_json::Value::Array(ref contents)) if contents.is_empty() => None,
            Some(serde_json::Value::Array(ref contents)) if contents.len() == 1 => {
                Some(Payload::Json(contents.get(0).unwrap().clone()))
            }
            Some(serde_json::Value::Array(ref contents)) if contents.len() == 2 => {
                if skip_event {
                    // SAFETY: len checked before
                    Some(Payload::Json(contents.get(1).unwrap().clone()))
                } else {
                    Some(Payload::Multi(
                        contents.iter().cloned().map(RawPayload::from).collect(),
                    ))
                }
            }
            Some(serde_json::Value::Array(ref contents)) => {
                let payload = if skip_event {
                    Payload::Multi(
                        contents
                            .iter()
                            .skip(1)
                            .cloned()
                            .map(RawPayload::from)
                            .collect(),
                    )
                } else {
                    Payload::Multi(contents.iter().cloned().map(RawPayload::from).collect())
                };
                Some(payload)
            }
            _ => None,
        }
    }

    pub(crate) async fn handle_connect(&self, packet: Option<&Packet>) -> Result<()> {
        self.is_connected.store(true, Ordering::Release);
        trace!("callback connect {:?}", packet);
        let payload = packet.map(|p| p.data.clone().into());

        self.callback(&Event::Connect, payload, None).await;
        Ok(())
    }

    /// Handles the incoming messages and classifies what callbacks to call and how.
    /// This method is later registered as the callback for the `on_data` event of the
    /// engineio client.
    #[inline]
    async fn handle_socketio_packet(&self, packet: &Packet) -> Result<()> {
        trace!("handle_socketio_packet {:?}", packet);
        if packet.nsp == self.nsp {
            match packet.ptype {
                PacketType::Ack => {
                    if let Err(err) = self.handle_ack(packet, false).await {
                        self.callback(&Event::Error, None, None).await;
                        return Err(err);
                    }
                }
                PacketType::BinaryAck => {
                    if let Err(err) = self.handle_ack(packet, true).await {
                        self.callback(&Event::Error, None, None).await;
                        return Err(err);
                    }
                }
                PacketType::Event => {
                    if let Err(err) = self.handle_event(packet).await {
                        self.callback(&Event::Error, Some(json!(err.to_string()).into()), None)
                            .await;
                    }
                }
                PacketType::BinaryEvent => {
                    if let Err(err) = self.handle_binary_event(packet).await {
                        self.callback(&Event::Error, Some(json!(err.to_string()).into()), None)
                            .await;
                    }
                }
                PacketType::Connect => self.handle_connect(Some(packet)).await?,
                PacketType::Disconnect => {
                    self.is_connected.store(false, Ordering::Release);
                    self.callback(&Event::Close, None, None).await;
                }
                PacketType::ConnectError => {
                    self.is_connected.store(false, Ordering::Release);
                    self.callback(
                        &Event::Error,
                        Some(
                            json!(format!("Received an ConnectError frame: {:?}", packet.data))
                                .into(),
                        ),
                        None,
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn poll_packet(&self) -> Option<Result<Packet>> {
        loop {
            // poll for the next payload
            let next = self.socket.poll_packet().await;
            match next {
                None => {
                    // end the stream if the underlying one is closed
                    return None;
                }
                Some(Err(err)) => {
                    // call the error callback
                    self.callback(&Event::Error, Some(json!(err.to_string()).into()), None)
                        .await;
                    return Some(Err(err));
                }
                Some(Ok(packet)) => {
                    // if this packet is not meant for the current namespace, skip it an poll for the next one
                    if packet.nsp == self.nsp {
                        let _ = self.handle_socketio_packet(&packet).await;
                        return Some(Ok(packet));
                    }
                }
            }
        }
    }
}

impl RawSocket {
    /// Creates an instance of `Socket`.
    #[cfg(feature = "client")]
    pub(super) fn client_end(engine_client: EngineSocket) -> Self {
        RawSocket {
            engine_client: Arc::new(engine_client.clone()),
            generator: Arc::new(Mutex::new(StreamGenerator::new(Self::stream(
                engine_client,
            )))),
            is_server: false,
        }
    }

    #[cfg(feature = "server")]
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
    #[cfg(feature = "client")]
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

        match packet.attachments {
            None => {
                self.engine_client.emit(engine_packet).await?;
            }
            Some(attachments) if attachments.is_empty() => {
                self.engine_client.emit(engine_packet).await?;
            }
            Some(attachments) => {
                let mut packets = vec![engine_packet];

                for attachment in attachments {
                    let engine_packet =
                        EnginePacket::new(EnginePacketType::MessageBinary, attachment);
                    packets.push(engine_packet);
                }

                // atomic send attachments
                self.engine_client.emit_multi(packets).await?;
            }
        }

        Ok(())
    }

    pub async fn ack(&self, nsp: &str, id: usize, data: Payload) -> Result<()> {
        let packet = RawSocket::build_packet_for_payload(data, None, nsp, Some(id), true)?;

        trace!("socket ack {:?}", packet);
        self.send(packet).await
    }

    /// Emits to certain event with given data. The data needs to be JSON,
    /// otherwise this returns an `InvalidJson` error.
    pub async fn emit(&self, nsp: &str, event: Event, data: Payload) -> Result<()> {
        let packet = RawSocket::build_packet_for_payload(data, Some(event), nsp, None, false)?;

        self.send(packet).await
    }

    #[cfg(feature = "server")]
    pub(crate) async fn handshake(&self, nsp: &str, data: Value) -> Result<()> {
        let packet = Packet::new(
            PacketType::Connect,
            nsp.to_owned(),
            Some(data),
            None,
            0,
            None,
        );
        self.send(packet).await
    }

    /// Returns a packet for a payload, could be used for bot binary and non binary
    /// events and acks. Convenance method.
    #[inline]
    pub(crate) fn build_packet_for_payload(
        payload: Payload,
        event: Option<Event>,
        nsp: &str,
        id: Option<usize>,
        is_ack: bool,
    ) -> Result<Packet> {
        let (data, attachments) = Self::encode_data(event, payload);

        let packet_type = match attachments.is_empty() {
            true if is_ack => PacketType::Ack,
            true => PacketType::Event,
            false if is_ack => PacketType::BinaryAck,
            false => PacketType::BinaryEvent,
        };

        Ok(Packet::new(
            packet_type,
            nsp.to_owned(),
            Some(data),
            id,
            attachments.len() as u8,
            Some(attachments),
        ))
    }

    fn encode_data(event: Option<Event>, payload: Payload) -> (Value, Vec<Bytes>) {
        let mut attachments = vec![];
        let mut data: Vec<Value> = vec![];

        if let Some(event) = event {
            data.push(json!(String::from(event)));
        }

        Self::process_payload(&mut data, payload, &mut attachments);

        let data = Value::Array(data);

        (data, attachments)
    }

    fn process_payload(data: &mut Vec<Value>, payload: Payload, attachments: &mut Vec<Bytes>) {
        match payload {
            Payload::Binary(bin_data) => Self::process_binary(data, bin_data, attachments),
            Payload::Json(value) => data.push(value),
            Payload::Multi(payloads) => {
                for payload in payloads {
                    match payload {
                        RawPayload::Binary(bin) => Self::process_binary(data, bin, attachments),
                        RawPayload::Json(value) => data.push(value),
                    }
                }
            }
        };
    }

    fn process_binary(data: &mut Vec<Value>, bin_data: Bytes, attachments: &mut Vec<Bytes>) {
        let place_holder = BinaryPlaceHolder::new(attachments.len());
        data.push(json!(&place_holder));
        attachments.push(bin_data);
    }

    pub(crate) async fn poll_packet(&self) -> Option<Result<Packet>> {
        let mut generator = self.generator.lock().await;
        generator.next().await
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
        let mut packet = Packet::try_from(&packet.data)?;

        // Only handle attachments if there are any
        if packet.attachment_count > 0 {
            let mut attachments_left = packet.attachment_count;
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
            packet.attachments = Some(attachments);
        }

        Ok(packet)
    }

    fn is_engineio_connected(&self) -> bool {
        self.engine_client.is_connected()
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

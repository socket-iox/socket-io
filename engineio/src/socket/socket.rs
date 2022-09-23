use std::{
    fmt::Debug,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{ready, Poll},
};

use async_stream::try_stream;
use bytes::Bytes;
use futures_util::{FutureExt, Stream, StreamExt};
use tokio::{
    sync::{mpsc::Sender, Mutex},
    time::Instant,
};
use tracing::trace;

use crate::{
    error::Result,
    packet::{HandshakePacket, Payload},
    transports::{Data, TransportType},
    Error, Packet, PacketType, Sid, StreamGenerator,
};

#[derive(Clone)]
pub struct Socket {
    transport: Arc<Mutex<TransportType>>,
    event_tx: Option<Arc<Sender<Event>>>,
    connected: Arc<AtomicBool>,
    last_ping: Arc<Mutex<Instant>>,
    last_pong: Arc<Mutex<Instant>>,
    connection_data: Arc<HandshakePacket>,
    generator: Arc<Mutex<StreamGenerator<Packet, Error>>>,
    server_end: bool,
    should_pong: bool,
}

#[derive(Debug)]
pub enum Event {
    OnOpen(Sid),
    OnClose(Sid),
    OnData(Sid, Bytes),
    OnPacket(Sid, Packet),
    OnError(Sid, String),
}

impl Socket {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        transport: TransportType,
        handshake: HandshakePacket,
        event_tx: Option<Arc<Sender<Event>>>,
        should_pong: bool,
        server_end: bool,
    ) -> Self {
        Socket {
            transport: Arc::new(Mutex::new(transport.clone())),
            connected: Arc::new(AtomicBool::default()),
            last_ping: Arc::new(Mutex::new(Instant::now())),
            last_pong: Arc::new(Mutex::new(Instant::now())),
            connection_data: Arc::new(handshake),
            generator: Arc::new(Mutex::new(StreamGenerator::new(Self::stream(transport)))),
            event_tx,
            server_end,
            should_pong,
        }
    }

    /// Opens the connection to a specified server. The first Pong packet is sent
    /// to the server to trigger the Ping-cycle.
    pub async fn connect(&self) -> Result<()> {
        // SAFETY: Has valid handshake due to type
        self.connected.store(true, Ordering::Release);
        if let Some(ref event_tx) = self.event_tx {
            event_tx.send(Event::OnOpen(self.sid())).await?;
        }

        // set the last ping to now and set the connected state
        *self.last_ping.lock().await = Instant::now();

        if !self.server_end {
            // emit a pong packet to keep trigger the ping cycle on the server
            self.emit(Packet::new(PacketType::Pong, Bytes::new()))
                .await?;
        }

        Ok(())
    }

    pub(crate) async fn last_pong(&self) -> Instant {
        *(self.last_pong.lock().await)
    }

    async fn handle_incoming_packet(&self, packet: Packet) {
        trace!("handle_incoming_packet {:?}", packet);
        // update last_pong on any packet, incoming data is a good sign of other side's liveness
        self.ponged().await;
        // check for the appropriate action or callback
        self.handle_packet(packet.clone()).await;
        match packet.ptype {
            PacketType::MessageBinary => {
                self.handle_data(packet.data).await;
            }
            PacketType::Message => {
                self.handle_data(packet.data).await;
            }
            PacketType::Close => {
                self.handle_close().await;
            }
            PacketType::Upgrade => {
                // this is already checked during the handshake, so just do nothing here
            }
            PacketType::Ping => {
                self.pinged().await;
                // server and pong timeout test case should not pong
                if self.should_pong {
                    let _ = self.emit(Packet::new(PacketType::Pong, packet.data)).await;
                }
            }
            PacketType::Pong | PacketType::Open | PacketType::Noop => (),
        }
    }

    fn sid(&self) -> Sid {
        Arc::clone(&self.connection_data.sid)
    }

    pub async fn disconnect(&self) -> Result<()> {
        if !self.is_connected() {
            return Ok(());
        }

        if let Some(ref event_tx) = self.event_tx {
            event_tx.send(Event::OnClose(self.sid())).await?;
        }

        self.emit(Packet::new(PacketType::Close, Bytes::new()))
            .await?;

        self.connected.store(false, Ordering::Release);

        Ok(())
    }

    /// Sends a packet to the server.
    pub async fn emit(&self, packet: Packet) -> Result<()> {
        if !self.connected.load(Ordering::Acquire) {
            let error = Error::IllegalActionBeforeOpen();
            self.on_error(format!("{}", error)).await;
            return Err(error);
        }

        trace!("socket emit {:?}", packet);
        // send a post request with the encoded payload as body
        // if this is a binary attachment, then send the raw bytes
        let data = match packet.ptype {
            PacketType::MessageBinary => Data::Binary(packet.data),
            _ => Data::Text(packet.into()),
        };

        let lock = self.transport.lock().await;
        let fut = lock.as_transport().emit(data);

        if let Err(error) = fut.await {
            self.on_error(error.to_string()).await;
            return Err(error);
        }

        Ok(())
    }

    /// Calls the error callback with a given message.
    #[inline]
    async fn on_error(&self, text: String) {
        trace!("socket on_error {}", text);
        if let Some(ref event_tx) = self.event_tx {
            let _ = event_tx.send(Event::OnError(self.sid(), text)).await;
        }
    }

    // Check if the underlying transport client is connected.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub(crate) async fn pinged(&self) {
        *self.last_ping.lock().await = Instant::now();
    }

    pub(crate) async fn ponged(&self) {
        *self.last_pong.lock().await = Instant::now();
    }

    pub(crate) async fn handle_packet(&self, packet: Packet) {
        if let Some(ref event_tx) = self.event_tx {
            let _ = event_tx.send(Event::OnPacket(self.sid(), packet)).await;
        }
    }

    pub(crate) async fn handle_data(&self, data: Bytes) {
        if let Some(ref event_tx) = self.event_tx {
            let _ = event_tx.send(Event::OnData(self.sid(), data)).await;
        }
    }

    pub(crate) async fn handle_close(&self) {
        if !self.is_connected() {
            return;
        }
        if let Some(ref event_tx) = self.event_tx {
            let _ = event_tx.send(Event::OnClose(self.sid())).await;
        }

        self.connected.store(false, Ordering::Release);
    }

    /// Helper method that parses bytes and returns an iterator over the elements.
    fn parse_payload(bytes: Bytes) -> impl Stream<Item = Result<Packet>> {
        try_stream! {
            let payload = Payload::try_from(bytes);

            for elem in payload?.into_iter() {
                trace!("parse_payload yield {:?}", elem);
                yield elem;
            }
        }
    }

    /// Creates a stream over the incoming packets, uses the streams provided by the
    /// underlying transport types.
    fn stream(
        mut transport: TransportType,
    ) -> Pin<Box<impl Stream<Item = Result<Packet>> + 'static + Send>> {
        // map the byte stream of the underlying transport
        // to a packet stream
        Box::pin(try_stream! {
            for await payload in transport.as_pin_box() {
                for await packet in Self::parse_payload(payload?) {
                    yield packet?;
                }
            }
        })
    }
}

impl Stream for Socket {
    type Item = Result<Packet>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut lock = ready!(Box::pin(self.generator.lock()).poll_unpin(cx));
        let item = lock.poll_next_unpin(cx);
        if let Poll::Ready(Some(Ok(packet))) = &item {
            ready!(Box::pin(self.handle_incoming_packet(packet.clone())).poll_unpin(cx));
        }
        item
    }
}

// impl Stre
#[cfg_attr(tarpaulin, ignore)]
impl Debug for Socket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Socket")
            .field("transport", &self.transport)
            .field("connected", &self.connected)
            .field("last_ping", &self.last_ping)
            .field("last_pong", &self.last_pong)
            .field("connection_data", &self.connection_data)
            .field("server_end", &self.server_end)
            .finish()
    }
}

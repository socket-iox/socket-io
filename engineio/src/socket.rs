use std::{
    fmt::Debug,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{ready, Poll},
};

use bytes::Bytes;
use futures_util::{FutureExt, Stream, StreamExt};
use tokio::{
    sync::{mpsc::Sender, Mutex},
    time::Instant,
};

use crate::{
    error::Result,
    packet::{HandshakePacket, Payload},
    transports::{Data, Transport},
    Error, Packet, PacketType, Sid,
};

#[derive(Clone)]
pub struct Socket {
    transport: Arc<Mutex<Box<dyn Transport + Unpin>>>,
    event_tx: Arc<Sender<Event>>,
    connected: Arc<AtomicBool>,
    last_ping: Arc<Mutex<Instant>>,
    last_pong: Arc<Mutex<Instant>>,
    connection_data: Arc<HandshakePacket>,
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
    // TODO: fix too_many_arguments
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        transport: Box<dyn Transport + Unpin>,
        handshake: HandshakePacket,
        event_tx: Sender<Event>,
        should_pong: bool,
        server_end: bool,
    ) -> Self {
        Socket {
            transport: Arc::new(Mutex::new(transport)),
            event_tx: Arc::new(event_tx),
            connected: Arc::new(AtomicBool::default()),
            last_ping: Arc::new(Mutex::new(Instant::now())),
            last_pong: Arc::new(Mutex::new(Instant::now())),
            connection_data: Arc::new(handshake),
            server_end,
            should_pong,
        }
    }

    pub(crate) async fn last_pong(&self) -> Instant {
        *(self.last_pong.lock().await)
    }

    /// Opens the connection to a specified server. The first Pong packet is sent
    /// to the server to trigger the Ping-cycle.
    pub async fn connect(&self) -> Result<()> {
        // SAFETY: Has valid handshake due to type
        self.connected.store(true, Ordering::Release);
        self.event_tx.send(Event::OnOpen(self.sid())).await?;

        // set the last ping to now and set the connected state
        *self.last_ping.lock().await = Instant::now();

        if !self.server_end {
            // emit a pong packet to keep trigger the ping cycle on the server
            self.emit(Packet::new(PacketType::Pong, Bytes::new()))
                .await?;
        }

        Ok(())
    }
    pub(super) async fn handle_incoming_packets(&self, packets: Vec<Packet>) {
        for packet in packets {
            self.handle_incoming_packet(packet).await;
        }
    }

    async fn handle_incoming_packet(&self, packet: Packet) {
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

    fn parse_payload(bytes: Bytes) -> Result<Vec<Packet>> {
        let payload = Payload::try_from(bytes)?;
        let packets: Vec<Packet> = payload.into_iter().collect();
        Ok(packets)
    }

    pub async fn disconnect(&self) -> Result<()> {
        if !self.is_connected() {
            return Ok(());
        }

        self.event_tx.send(Event::OnClose(self.sid())).await?;

        self.emit(Packet::new(PacketType::Close, Bytes::new()))
            .await?;

        self.connected.store(false, Ordering::Release);

        Ok(())
    }

    /// Sends a packet to the server.
    pub async fn emit(&self, packet: Packet) -> Result<()> {
        if !self.connected.load(Ordering::Acquire) {
            let error = Error::IllegalActionBeforeOpen();
            self.call_error_callback(format!("{}", error)).await;
            return Err(error);
        }

        // send a post request with the encoded payload as body
        // if this is a binary attachment, then send the raw bytes
        let data = match packet.ptype {
            PacketType::MessageBinary => Data::Binary(packet.data),
            _ => Data::Text(packet.into()),
        };

        let lock = self.transport.lock().await;
        let fut = lock.emit(data);

        if let Err(error) = fut.await {
            self.call_error_callback(error.to_string()).await;
            return Err(error);
        }

        Ok(())
    }

    /// Calls the error callback with a given message.
    #[inline]
    async fn call_error_callback(&self, text: String) {
        let _ = self.event_tx.send(Event::OnError(self.sid(), text)).await;
    }

    // Check if the underlying transport client is connected.
    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub(crate) async fn pinged(&self) {
        *self.last_ping.lock().await = Instant::now();
    }

    pub(crate) async fn ponged(&self) {
        *self.last_pong.lock().await = Instant::now();
    }

    pub(crate) async fn handle_packet(&self, packet: Packet) {
        let _ = self
            .event_tx
            .send(Event::OnPacket(self.sid(), packet))
            .await;
    }

    pub(crate) async fn handle_data(&self, data: Bytes) {
        let _ = self.event_tx.send(Event::OnData(self.sid(), data)).await;
    }

    pub(crate) async fn handle_close(&self) {
        if !self.is_connected() {
            return;
        }

        let _ = self.event_tx.send(Event::OnClose(self.sid())).await;

        self.connected.store(false, Ordering::Release);
    }
}

impl Stream for Socket {
    type Item = Result<Vec<Packet>>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut lock = ready!(Box::pin(self.transport.lock()).poll_unpin(cx));
        let next = ready!(lock.poll_next_unpin(cx));

        match next {
            Some(Ok(bytes)) => {
                let packets = Socket::parse_payload(bytes);
                if let Ok(packets) = &packets {
                    ready!(Box::pin(self.handle_incoming_packets(packets.clone())).poll_unpin(cx));
                }
                Poll::Ready(Some(packets))
            }
            Some(Err(e)) => Poll::Ready(Some(Err(e))),
            None => Poll::Ready(None),
        }
    }
}

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

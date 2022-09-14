use std::{
    collections::HashMap,
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use bytes::Bytes;
use tokio::{
    net::TcpListener,
    sync::{mpsc::channel, Mutex, RwLock},
    time::{interval, Instant},
};

use crate::{
    error::Result,
    packet::HandshakePacket,
    server::http::{handle_http, PollingHandle},
    socket::Socket,
    transports::Transport,
    Event, Packet, PacketType, Sid,
};

#[derive(Clone)]
pub struct Server {
    inner: Arc<ServerInner>,
}

struct ServerInner {
    port: u16,
    server_option: ServerOption,
    id_generator: SidGenerator,
    polling_handles: Arc<Mutex<HashMap<Sid, PollingHandle>>>,
    buffer_size: usize,
    sockets: RwLock<HashMap<Sid, Socket>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ServerOption {
    pub ping_timeout: u64,
    pub ping_interval: u64,
    pub max_payload: usize,
}

#[derive(Default)]
struct SidGenerator {
    seq: AtomicUsize,
}

impl Server {
    pub async fn serve(&self) {
        let addr = format!("0.0.0.0:{}", self.inner.port);
        let listener = TcpListener::bind(&addr)
            .await
            .expect("engine-io server can not listen port");

        while let Ok((stream, peer_addr)) = listener.accept().await {
            let server = self.clone();
            tokio::spawn(async move { handle_http(server, stream, peer_addr).await });
        }
    }

    pub async fn emit(&self, sid: &Sid, packet: Packet) -> Result<()> {
        let sockets = self.inner.sockets.read().await;
        let socket = sockets.get(sid);
        if let Some(s) = socket {
            s.emit(packet).await?;
        }
        Ok(())
    }

    pub fn polling_handles(&self) -> Arc<Mutex<HashMap<Sid, PollingHandle>>> {
        self.inner.polling_handles.clone()
    }

    pub fn polling_buffer(&self) -> usize {
        self.inner.buffer_size
    }

    pub fn generate_sid(&self) -> Sid {
        self.inner.id_generator.generate()
    }

    pub fn handshake_packet(&self, upgrades: Vec<String>, sid: Option<Sid>) -> HandshakePacket {
        let sid = match sid {
            Some(sid) => sid,
            None => self.inner.id_generator.generate(),
        };

        HandshakePacket {
            sid,
            upgrades,
            ping_interval: 1000,
            ping_timeout: 1000,
        }
    }

    pub async fn store_transport(&self, sid: Sid, transport: Box<dyn Transport>) -> Result<()> {
        let (tx, _rx) = channel::<Event>(self.inner.buffer_size);
        let handshake = self.handshake_packet(vec!["webscocket".to_owned()], Some(sid.clone()));
        let socket = Socket::new(
            transport, handshake, tx, false, // server no need to pong
            true,
        );

        let mut sockets = self.inner.sockets.write().await;
        let _ = sockets.insert(sid.clone(), socket);

        self.start_ping_pong(&sid);
        Ok(())
    }

    pub fn start_ping_pong(&self, sid: &Sid) {
        let sid = sid.to_owned();
        let server = self.clone();
        let option = server.inner.server_option;
        let timeout = Duration::from_millis(option.ping_timeout + option.ping_interval);
        let mut interval = interval(Duration::from_millis(option.ping_interval));

        tokio::spawn(async move {
            loop {
                interval.tick().await;
                let ping_packet = Packet {
                    ptype: PacketType::Ping,
                    data: Bytes::new(),
                };
                if server.emit(&sid, ping_packet).await.is_err() {
                    break;
                };
                let last_pong = server.last_pong(&sid).await;
                match last_pong {
                    Some(instant) if instant.elapsed() < timeout => {}
                    _ => break,
                }
            }
            server.close_socket(&sid).await;
        });
    }

    pub(crate) fn max_payload(&self) -> usize {
        1000
    }

    async fn last_pong(&self, sid: &Sid) -> Option<Instant> {
        let sockets = self.inner.sockets.read().await;
        Some(sockets.get(sid)?.last_pong().await)
    }

    async fn close_socket(&self, sid: &Sid) {
        let mut sockets = self.inner.sockets.write().await;
        if let Some(socket) = sockets.remove(sid) {
            drop(sockets);
            let _ = socket.disconnect().await;
        }
    }
}

impl Default for ServerOption {
    fn default() -> Self {
        Self {
            ping_timeout: 25000,
            ping_interval: 20000,
            max_payload: 1024,
        }
    }
}

impl SidGenerator {
    fn generate(&self) -> Sid {
        let seq = self.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Arc::new(base64::encode(seq.to_string()))
    }
}

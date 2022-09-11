use std::{
    net::SocketAddr,
    sync::{atomic::AtomicUsize, Arc},
};

use bytes::Bytes;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::{error::Result, packet::HandshakePacket, transports::Transport, Sid};

#[derive(Clone)]
pub struct Server {
    pub(crate) inner: Arc<ServerInner>,
}

pub(crate) struct ServerInner {
    pub(self) port: u16,
    pub(self) server_option: ServerOption,
    pub(self) id_generator: SidGenerator,
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

    pub async fn store_stream(
        &self,
        sid: Sid,
        peer_addr: &SocketAddr,
        ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    ) -> Result<()> {
        Ok(())
    }

    pub async fn store_transport(&self, sid: Sid, transport: Box<dyn Transport>) -> Result<()> {
        Ok(())
    }

    pub async fn close_polling(&self, sid: &Sid) {}

    pub(crate) fn max_payload(&self) -> usize {
        1000
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

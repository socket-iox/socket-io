use std::{net::SocketAddr, sync::Arc};

use bytes::Bytes;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::{error::Result, packet::HandshakePacket, Sid};

#[derive(Clone)]
pub struct Server {}

impl Server {
    pub fn generate_sid(&self) -> Sid {
        Arc::new(String::from("sid"))
    }

    pub fn handshake_packet(&self, upgrades: Vec<String>, sid: Option<Sid>) -> HandshakePacket {
        HandshakePacket {
            sid: sid.unwrap(),
            upgrades,
            ping_interval: 1000,
            ping_timeout: 1000,
        }
    }

    pub async fn close_polling(&self, sid: &Sid) {}

    pub async fn store_stream(
        &self,
        sid: Sid,
        peer_addr: &SocketAddr,
        ws_stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    ) -> Result<()> {
        Ok(())
    }

    pub async fn store_polling(&self, sid: Sid, peer_addr: &SocketAddr) -> Result<()> {
        Ok(())
    }

    pub(crate) async fn polling_get(&self, sid: &Sid) -> Option<String> {
        None
    }

    pub(crate) async fn polling_post(&self, sid: &Sid, data: Bytes) {}

    pub(crate) fn max_payload(&self) -> usize {
        1000
    }
}

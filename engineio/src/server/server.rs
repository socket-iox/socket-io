use std::{
    collections::HashMap,
    sync::{atomic::AtomicUsize, Arc},
    time::Duration,
};

use bytes::Bytes;
use tokio::{
    net::TcpListener,
    sync::{
        mpsc::{Receiver, Sender},
        Mutex, RwLock,
    },
    time::{interval, Instant},
};
use tracing::trace;

use crate::{
    error::Result,
    packet::HandshakePacket,
    server::http::{handle_http, PollingHandle},
    socket::Socket,
    transports::TransportType,
    Event, Packet, PacketType, Sid,
};

#[derive(Clone)]
pub struct Server {
    pub(super) inner: Arc<ServerInner>,
}

pub(super) struct ServerInner {
    pub(super) port: u16,
    pub(super) server_option: ServerOption,
    pub(super) id_generator: SidGenerator,
    pub(super) polling_handles: Arc<Mutex<HashMap<Sid, PollingHandle>>>,
    pub(super) polling_buffer: usize,
    pub(super) event_tx: Arc<Sender<Event>>,
    pub(super) event_rx: Arc<Mutex<Receiver<Event>>>,
    pub(super) sockets: RwLock<HashMap<Sid, Socket>>,
}

#[derive(Debug, Clone, Copy)]
pub struct ServerOption {
    pub ping_timeout: u64,
    pub ping_interval: u64,
    pub max_payload: usize,
}

#[derive(Default)]
pub(super) struct SidGenerator {
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
        trace!("emit {} {:?}", sid, packet);
        let sockets = self.inner.sockets.read().await;
        let socket = sockets.get(sid);
        if let Some(s) = socket {
            s.emit(packet).await?;
        }
        Ok(())
    }

    pub fn event_rx(&self) -> Arc<Mutex<Receiver<Event>>> {
        self.inner.event_rx.clone()
    }

    pub async fn socket(&self, sid: &Sid) -> Option<Socket> {
        let sockets = self.inner.sockets.read().await;
        sockets.get(sid).map(|x| x.to_owned())
    }

    pub async fn close_socket(&self, sid: &Sid) {
        let mut sockets = self.inner.sockets.write().await;
        if let Some(socket) = sockets.remove(sid) {
            drop(sockets);
            let _ = socket.disconnect().await;
        }
    }

    pub(crate) fn polling_handles(&self) -> Arc<Mutex<HashMap<Sid, PollingHandle>>> {
        self.inner.polling_handles.clone()
    }

    pub(crate) async fn polling_handle(&self, sid: &Sid) -> Option<PollingHandle> {
        let lock = self.inner.polling_handles.lock().await;
        let handle = lock.get(sid);
        handle.cloned()
    }

    pub(crate) async fn drain_polling(&self, sid: &Sid) {
        if let Some(socket) = self.socket(sid).await {
            let _ = socket.emit(Packet::noop()).await;
        }
    }

    pub(crate) fn polling_buffer(&self) -> usize {
        self.inner.polling_buffer
    }

    pub(crate) fn generate_sid(&self) -> Sid {
        self.inner.id_generator.generate()
    }

    pub(crate) fn handshake_packet(
        &self,
        upgrades: Vec<String>,
        sid: Option<Sid>,
    ) -> HandshakePacket {
        let sid = match sid {
            Some(sid) => sid,
            None => self.inner.id_generator.generate(),
        };

        HandshakePacket {
            sid,
            upgrades,
            ping_interval: self.inner.server_option.ping_interval,
            ping_timeout: self.inner.server_option.ping_timeout,
            max_payload: self.inner.server_option.max_payload,
        }
    }

    pub(crate) async fn store_transport(&self, sid: Sid, transport: TransportType) -> Result<()> {
        trace!("store_transport {} {:?}", sid, transport);
        let handshake = self.handshake_packet(vec!["webscocket".to_owned()], Some(sid.clone()));
        let socket = Socket::new(
            transport,
            handshake,
            Some(self.inner.event_tx.clone()),
            false, // server no need to pong
            true,
        );

        socket.connect().await?;

        let mut sockets = self.inner.sockets.write().await;
        let _ = sockets.insert(sid.clone(), socket);
        self.start_ping_pong(&sid);

        Ok(())
    }

    pub(crate) fn start_ping_pong(&self, sid: &Sid) {
        let sid = sid.to_owned();
        let server = self.clone();
        let option = server.inner.server_option;
        let timeout = Duration::from_millis(option.ping_timeout + option.ping_interval);
        let duration = Duration::from_millis(option.ping_interval);
        trace!("start_ping_pong {} interval {:?}", sid, duration);
        let mut interval = interval(duration);

        tokio::spawn(async move {
            loop {
                interval.tick().await;
                let ping_packet = Packet {
                    ptype: PacketType::Ping,
                    data: Bytes::new(),
                };
                if let Err(e) = server.emit(&sid, ping_packet).await {
                    trace!("emit ping error {} {}", sid, e);
                    break;
                };
                let last_pong = server.last_pong(&sid).await;
                match last_pong {
                    Some(instant) if instant.elapsed() < timeout => {}
                    _ => break,
                }
            }
            trace!("pong_timeout close {}", sid);
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

#[cfg(test)]
mod test {
    use super::*;

    use std::{sync::Arc, time::Duration};

    use futures_util::{Stream, StreamExt};
    use reqwest::Url;

    use crate::{server::builder::ServerBuilder, socket::SocketBuilder, Packet};

    #[tokio::test]
    async fn test_connection() -> Result<()> {
        // tracing_subscriber::fmt() .with_env_filter("engineio=trace") .init();
        let url = crate::test::rust_engine_io_server();
        let (mut rx, _server) = start_server(url.clone()).await;

        let socket = SocketBuilder::new(url.clone()).build_polling().await?;
        test_data_transport(socket, &mut rx).await?;

        let socket = SocketBuilder::new(url.clone()).build().await?;
        test_data_transport(socket, &mut rx).await?;

        let socket = SocketBuilder::new(url.clone()).build_websocket().await?;
        test_data_transport(socket, &mut rx).await?;

        let socket = SocketBuilder::new(url)
            .build_websocket_with_upgrade()
            .await?;
        test_data_transport(socket, &mut rx).await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_pong_timeout() -> Result<()> {
        // tracing_subscriber::fmt() .with_env_filter("engineio=trace") .init();
        let url = crate::test::rust_engine_io_timeout_server();
        let _ = start_server(url.clone()).await;

        let socket = SocketBuilder::new(url.clone())
            .should_pong_for_test(false)
            .build_polling()
            .await?;
        test_transport_timeout(socket).await?;

        let socket = SocketBuilder::new(url.clone())
            .should_pong_for_test(false)
            .build()
            .await?;
        test_transport_timeout(socket).await?;

        let socket = SocketBuilder::new(url.clone())
            .should_pong_for_test(false)
            .build_websocket()
            .await?;
        test_transport_timeout(socket).await?;

        let socket = SocketBuilder::new(url)
            .should_pong_for_test(false)
            .build_websocket_with_upgrade()
            .await?;
        test_transport_timeout(socket).await?;

        Ok(())
    }

    async fn test_transport_timeout(mut client: Socket) -> Result<()> {
        client.connect().await?;

        let client_clone = client.clone();
        tokio::spawn(async move {
            loop {
                let next = client.next().await;
                if next.is_none() {
                    break;
                }
            }
        });

        tokio::time::sleep(Duration::from_millis(200)).await;

        // closed by server
        assert!(!client_clone.is_connected());

        Ok(())
    }

    async fn start_server(url: Url) -> (Receiver<String>, Server) {
        let port = url.port().unwrap();
        let server_option = ServerOption {
            ping_timeout: 20,
            ping_interval: 20,
            max_payload: 1024,
        };
        let (server, rx) = setup(port, server_option);
        let server_clone = server.clone();

        tokio::spawn(async move {
            server_clone.serve().await;
        });

        // wait server start
        tokio::time::sleep(Duration::from_millis(100)).await;

        (rx, server)
    }

    fn setup(port: u16, server_option: ServerOption) -> (Server, Receiver<String>) {
        let (tx, rx) = tokio::sync::mpsc::channel(100);
        let server = ServerBuilder::new(port)
            .polling_buffer(100)
            .event_size(100)
            .server_option(server_option)
            .build();

        let event_rx = server.event_rx();
        let server_clone = server.clone();

        tokio::spawn(async move {
            let mut event_rx = event_rx.lock().await;

            while let Some(event) = event_rx.recv().await {
                match event {
                    Event::OnOpen(sid) => {
                        let socket = server_clone.socket(&sid).await;
                        poll_stream(socket.unwrap());
                        let _ = tx.send(format!("open {}", sid)).await;
                    }
                    Event::OnPacket(_sid, packet) => {
                        let _ = tx.send(String::from(packet.ptype)).await;
                    }
                    Event::OnData(_sid, data) => {
                        let data = std::str::from_utf8(&data).unwrap();
                        let _ = tx.send(data.to_owned()).await;
                    }
                    Event::OnClose(_sid) => {
                        let _ = tx.send("close".to_owned()).await;
                    }
                    _ => {}
                };
            }
        });

        (server, rx)
    }

    async fn test_data_transport(client: Socket, server_rx: &mut Receiver<String>) -> Result<()> {
        client.connect().await?;

        let client_clone = client.clone();
        poll_stream(client_clone);

        client
            .emit(Packet::new(crate::PacketType::Message, Bytes::from("msg")))
            .await?;

        let mut sid = Arc::new("".to_owned());

        // ignore item send by last client
        while let Some(item) = server_rx.recv().await {
            if item.starts_with("open") {
                let items: Vec<&str> = item.split(' ').collect();
                sid = Arc::new(items[1].to_owned());
                break;
            }
        }
        trace!("test_data_transport 4, sid {}", sid);

        // wait ping pong
        tokio::time::sleep(Duration::from_millis(100)).await;

        client.disconnect().await?;

        let mut receive_pong = false;
        let mut receive_msg = false;

        while let Some(item) = server_rx.recv().await {
            match item.as_str() {
                "3" => receive_pong = true,
                "msg" => receive_msg = true,
                "close" => break,
                _ => {}
            }
        }

        assert!(receive_pong);
        assert!(receive_msg);
        assert!(!client.is_connected());

        Ok(())
    }

    fn poll_stream(mut stream: impl Stream + Unpin + Send + 'static) {
        tokio::spawn(async move { while stream.next().await.is_some() {} });
    }
}

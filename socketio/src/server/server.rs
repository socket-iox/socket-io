use crate::{
    ack::AckId, callback::Callback, packet::PacketType, server::Client as ServerSocket,
    socket::RawSocket, Error, Event, NameSpace, Payload,
};
use dashmap::DashMap;
use engineio_rs::{Event as EngineEvent, Server as EngineServer, Sid as EngineSid};
use futures_util::future::BoxFuture;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tracing::{error, trace, warn};

// TODO: read from config
const CONNECT_TIMEOUT: u64 = 5;

type Sid = Arc<String>;
type Room = String;
type Rooms = DashMap<NameSpace, HashMap<Room, HashSet<Sid>>>;
type On = DashMap<Event, Callback<ServerSocket>>;

pub struct Server {
    pub(crate) on: DashMap<NameSpace, Arc<On>>,
    pub(crate) rooms: Rooms,
    pub(crate) clients: DashMap<EngineSid, DashMap<Sid, HashMap<NameSpace, ServerSocket>>>,
    pub(crate) engine_server: EngineServer,
    pub(crate) sid_generator: SidGenerator,
}

impl Server {
    #[allow(dead_code)]
    pub async fn serve(self: Arc<Self>) {
        self.recv_event();
        self.engine_server.serve().await
    }

    pub async fn emit_to<E, D>(self: &Arc<Self>, nsp: &str, rooms: Vec<&str>, event: E, data: D)
    where
        E: Into<Event>,
        D: Into<Payload>,
    {
        let event = event.into();
        let payload = data.into();

        let sids_to_emit = self.sids_to_emit(nsp, rooms).await;

        for sid in sids_to_emit {
            if let Some(client) = self.client(&sid, nsp).await {
                let event = event.clone();
                let payload = payload.clone();

                tokio::spawn(async move {
                    let r = client.emit(event, payload).await;
                    trace!("server emit_to: {}, status: {:?}", sid, r);
                    if r.is_err() {
                        error!("emit_to {} failed {:?}", sid, r);
                    }
                });
            }
        }
    }

    pub async fn emit_to_with_ack<F, E, D>(
        &self,
        nsp: &str,
        rooms: Vec<&str>,
        event: E,
        data: D,
        timeout: Duration,
        callback: F,
    ) where
        F: for<'a> std::ops::FnMut(
                Option<Payload>,
                ServerSocket,
                Option<AckId>,
            ) -> BoxFuture<'static, ()>
            + 'static
            + Send
            + Sync
            + Clone,
        E: Into<Event>,
        D: Into<Payload>,
    {
        let event = event.into();
        let payload = data.into();

        for sid in self.sids_to_emit(nsp, rooms).await {
            if let Some(client) = self.client(&sid, nsp).await {
                let event = event.clone();
                let payload = payload.clone();

                let callback_clone = callback.clone();

                tokio::spawn(async move {
                    let r = client
                        .emit_with_ack(
                            event.clone(),
                            payload.clone(),
                            timeout,
                            callback_clone.clone(),
                        )
                        .await;
                    if r.is_err() {
                        error!("emit_with_ack to {} {:?}", sid, r);
                    }
                });
            }
        }
    }

    async fn sids_to_emit(&self, nsp: &str, rooms: Vec<&str>) -> HashSet<Sid> {
        let clients = &self.rooms;
        let mut sids_to_emit = HashSet::new();
        if let Some(room_clients) = clients.get(nsp) {
            for room_name in rooms {
                match room_clients.get(room_name) {
                    Some(room) => {
                        for sid in room {
                            sids_to_emit.insert(sid.clone());
                        }
                    }
                    // room may be sid
                    None => {
                        let _ = sids_to_emit.insert(Arc::new(room_name.to_owned()));
                    }
                };
            }
        }
        sids_to_emit
    }

    pub(crate) fn recv_event(self: &Arc<Self>) {
        let event_rx = self.engine_server.event_rx();
        let server = self.to_owned();
        tokio::spawn(async move {
            let mut event_rx = event_rx.lock().await;

            while let Some(event) = event_rx.recv().await {
                trace!("server recv_event: {:?}", event);
                match event {
                    EngineEvent::OnOpen(esid) => server.create_client(esid).await,
                    EngineEvent::OnClose(esid) => server.drop_client(&esid).await,
                    EngineEvent::OnPacket(_esid, _packet) => {
                        // TODO: watch new namespace packet
                    }
                    _ => {}
                };
            }
        });
    }

    pub(crate) async fn client(&self, sid: &Sid, nsp: &str) -> Option<ServerSocket> {
        let esid = &SidGenerator::decode(sid)?;
        self.clients.get(esid)?.get(sid)?.get(nsp).cloned()
    }

    pub(crate) async fn join<T: Into<String>>(
        self: &Arc<Self>,
        nsp: &str,
        rooms: Vec<T>,
        sid: Sid,
    ) {
        for room_name in rooms {
            let room_name = room_name.into();
            match self.rooms.get_mut(nsp) {
                None => {
                    let mut room_sids = HashSet::new();
                    room_sids.insert(sid.clone());
                    let mut rooms = HashMap::new();
                    rooms.insert(room_name, room_sids);
                    self.rooms.insert(nsp.to_owned(), rooms);
                }
                Some(mut rooms) => {
                    if let Some(room_sids) = rooms.get_mut(&room_name) {
                        let _ = room_sids.insert(sid.clone());
                    } else {
                        let mut room_sids = HashSet::new();
                        room_sids.insert(sid.clone());
                        rooms.insert(room_name, room_sids);
                    }
                }
            };
        }
    }

    pub(crate) async fn leave(self: &Arc<Self>, nsp: &str, rooms: Vec<&str>, sid: &Sid) {
        for room_name in rooms {
            if let Some(mut nsp_rooms) = self.rooms.get_mut(nsp) {
                if let Some(room_sids) = nsp_rooms.get_mut(room_name) {
                    room_sids.remove(sid);
                }
            };
        }
    }

    async fn create_client(self: &Arc<Self>, esid: EngineSid) {
        if let Some(engine_socket) = self.engine_server.socket(&esid).await {
            let socket = RawSocket::server_end(engine_socket);

            // TODO: support multiple namespace
            match self.client_info(&esid).await {
                Some((sid, nsp)) => self.insert_clients(socket, nsp, esid, sid, false).await,
                None => self.handle_connect(socket, esid).await,
            };
        }
    }

    // TODO: support multiple nsp
    // currently one esid mapping to one sid,
    // one sid mapping one nsp
    async fn client_info(&self, esid: &EngineSid) -> Option<(Sid, String)> {
        let sid_map = self.clients.get(esid)?;
        let entry = sid_map.iter().next()?;
        let (sid, nsp_map) = entry.pair();
        let (nsp, _) = nsp_map.iter().next()?;

        Some((sid.to_owned(), nsp.to_owned()))
    }

    async fn handle_connect(self: &Arc<Self>, socket: RawSocket, esid: EngineSid) {
        trace!("handle_connect: {:?}", esid);
        let slf = self.clone();
        tokio::spawn(async move {
            if tokio::time::timeout(
                Duration::from_secs(CONNECT_TIMEOUT),
                slf.do_handle_connect(socket, esid.clone()),
            )
            .await
            .is_err()
            {
                warn!("handle_connect timeout, {:?} dropped", esid);
                slf.drop_client(&esid).await;
            }
        });
    }

    async fn do_handle_connect(self: &Arc<Self>, socket: RawSocket, esid: EngineSid) {
        let sid = self.sid_generator.generate(&esid);
        while let Some(Ok(packet)) = socket.poll_packet().await {
            if packet.ptype == PacketType::Connect {
                let nsp = packet.nsp.clone();
                self.insert_clients(socket, nsp, esid, sid, true).await;
                break;
            } else {
                continue;
            }
        }
    }

    async fn insert_clients(
        self: &Arc<Self>,
        socket: RawSocket,
        nsp: String,
        esid: EngineSid,
        sid: Sid,
        handshake: bool,
    ) {
        if let Some(on) = self.on.get(&nsp) {
            let client = ServerSocket::new(
                socket,
                nsp.clone(),
                sid.clone(),
                on.to_owned(),
                self.clone(),
            );

            client.connect_callback().await;

            poll(client.clone());

            if handshake {
                let _ = client.handshake(json!({ "sid": sid.clone() })).await;
            }

            let sid_map = self.clients.entry(esid).or_default();

            let mut nsp_map = sid_map.entry(sid).or_default();
            nsp_map.insert(nsp, client);
        } else {
            warn!("unkown nsp {} from client", nsp);
        }
    }

    async fn drop_client(self: &Arc<Self>, esid: &EngineSid) {
        self.engine_server.close_socket(esid).await;

        if self.clients.remove(esid).is_some() {
            //TODO: disconnect
        }

        // FIXME: performance will be low if too many nsp and rooms
        self.rooms.iter_mut().for_each(|mut nsp_clients| {
            for room_clients in nsp_clients.values_mut() {
                room_clients.retain(|sid| SidGenerator::decode(sid).as_ref() != Some(esid))
            }
        });
    }
}

#[derive(Default)]
pub(crate) struct SidGenerator {
    seq: AtomicUsize,
}

impl SidGenerator {
    pub fn generate(&self, engine_sid: &EngineSid) -> Sid {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        Arc::new(base64::encode(format!("{}-{}", engine_sid, seq)))
    }

    pub fn decode(sid: &Sid) -> Option<EngineSid> {
        let sid_vec = base64::decode(sid.as_bytes()).ok()?;
        let esid_sid = std::str::from_utf8(&sid_vec).ok()?;
        let tokens: Vec<&str> = esid_sid.split('-').collect();
        Some(Arc::new(tokens[0].to_owned()))
    }
}

fn poll(socket: ServerSocket) {
    tokio::runtime::Handle::current().spawn(async move {
        loop {
            // tries to restart a poll cycle whenever a 'normal' error occurs,
            // it just logs on network errors, in case the poll cycle returned
            // `Result::Ok`, the server receives a close frame so it's safe to
            // terminate
            let next = socket.poll_packet().await;
            match next {
                Some(e @ Err(Error::IncompleteResponseFromEngineIo(_))) => {
                    trace!("Network error occured: {:?}", e.err());
                }
                None => break,
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod test {
    use std::{
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
        time::Duration,
    };

    use crate::{
        client::ClientBuilder, client::Socket, server::client::Client as ServerClient,
        test::rust_socket_io_server, AckId, Event, Payload, ServerBuilder,
    };

    use super::SidGenerator;
    use futures_util::FutureExt;
    use serde_json::json;
    use tracing::info;

    #[test]
    fn test_sid_generator() {
        let generator = SidGenerator::default();
        let engine_sid = Arc::new("engine_sid".to_owned());
        let sid = generator.generate(&engine_sid);

        assert_eq!(SidGenerator::decode(&sid), Some(engine_sid));
    }

    #[tokio::test]
    async fn test_server() {
        // tracing_subscriber::fmt()
        //     .with_env_filter("engineio=trace,socketio=trace")
        //     .init();
        setup();
        test_emit().await;
        test_client_ask_ack().await;
        test_server_ask_ack().await;
    }

    async fn test_emit() {
        let is_recv = Arc::new(AtomicBool::default());
        let is_recv_clone = Arc::clone(&is_recv);

        let callback = move |_: Option<Payload>, _: Socket, _: Option<AckId>| {
            let is_recv = is_recv_clone.clone();
            async move {
                tracing::info!("1");
                is_recv.store(true, Ordering::SeqCst);
                tracing::info!("2");
            }
            .boxed()
        };

        let url = rust_socket_io_server();
        let socket = ClientBuilder::new(url)
            .namespace("/admin")
            .on("echo", callback)
            .on(Event::Connect, move |_payload, socket, _| {
                async move {
                    socket.emit("echo", json!("data")).await.expect("success");
                }
                .boxed()
            })
            .connect()
            .await;

        assert!(socket.is_ok());

        // wait recv data
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(is_recv.load(Ordering::SeqCst));
    }

    async fn test_client_ask_ack() {
        let is_client_ack = Arc::new(AtomicBool::default());
        let is_client_ack_clone = Arc::clone(&is_client_ack);

        let client_ack_callback =
            move |_payload: Option<Payload>, _socket: Socket, _need_ack: Option<AckId>| {
                let is_client_ack = is_client_ack_clone.clone();
                async move {
                    is_client_ack.store(true, Ordering::SeqCst);
                }
                .boxed()
            };

        let url = rust_socket_io_server();
        let socket = ClientBuilder::new(url)
            .namespace("/admin")
            .on(Event::Connect, move |_payload, socket, _| {
                let client_ack_callback = client_ack_callback.clone();
                async move {
                    socket
                        .emit_with_ack(
                            "client_ack",
                            json!("data"),
                            Duration::from_millis(200),
                            client_ack_callback,
                        )
                        .await
                        .expect("success");
                }
                .boxed()
            })
            .connect()
            .await;

        assert!(socket.is_ok());

        // wait recv data
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(is_client_ack.load(Ordering::SeqCst));
    }

    async fn test_server_ask_ack() {
        let is_server_ask_ack = Arc::new(AtomicBool::default());
        let is_server_recv_ack = Arc::new(AtomicBool::default());
        let is_server_ask_ack_clone = Arc::clone(&is_server_ask_ack);
        let is_server_recv_ack_clone = Arc::clone(&is_server_recv_ack);

        let server_ask_ack =
            move |_payload: Option<Payload>, socket: Socket, need_ack: Option<AckId>| {
                let is_server_ask_ack = is_server_ask_ack_clone.clone();
                async move {
                    assert!(need_ack.is_some());
                    if let Some(ack_id) = need_ack {
                        socket.ack(ack_id, json!("")).await.expect("success");
                        is_server_ask_ack.store(true, Ordering::SeqCst);
                    }
                }
                .boxed()
            };

        let server_recv_ack =
            move |_payload: Option<Payload>, _socket: Socket, _need_ack: Option<AckId>| {
                let is_server_recv_ack = is_server_recv_ack_clone.clone();
                async move {
                    is_server_recv_ack.store(true, Ordering::SeqCst);
                }
                .boxed()
            };

        let url = rust_socket_io_server();
        let socket = ClientBuilder::new(url)
            .namespace("/admin")
            .on("server_ask_ack", server_ask_ack)
            .on("server_recv_ack", server_recv_ack)
            .on(Event::Connect, move |_payload, socket, _| {
                async move {
                    socket
                        .emit("trigger_server_ack", json!("data"))
                        .await
                        .expect("success");
                }
                .boxed()
            })
            .connect()
            .await;

        assert!(socket.is_ok());

        // wait recv data
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(is_server_ask_ack.load(Ordering::SeqCst));
        assert!(is_server_recv_ack.load(Ordering::SeqCst));
    }

    fn setup() {
        let echo_callback =
            move |_payload: Option<Payload>, socket: ServerClient, _need_ack: Option<AckId>| {
                async move {
                    info!("server echo callback");
                    socket.join(vec!["room 1"]).await;
                    socket.emit_to(vec!["room 1"], "echo", json!("")).await;
                    socket.leave(vec!["room 1"]).await;
                    info!("server echo callback done");
                }
                .boxed()
            };

        let client_ack =
            move |_payload: Option<Payload>, socket: ServerClient, need_ack: Option<AckId>| {
                async move {
                    if let Some(ack_id) = need_ack {
                        socket
                            .ack(ack_id, json!("ack to client"))
                            .await
                            .expect("success");
                    }
                }
                .boxed()
            };

        let server_recv_ack =
            move |_payload: Option<Payload>, socket: ServerClient, _need_ack: Option<AckId>| {
                async move {
                    socket
                        .emit("server_recv_ack", json!(""))
                        .await
                        .expect("success");
                }
                .boxed()
            };

        let trigger_ack = move |_message: Option<Payload>, socket: ServerClient, _| {
            async move {
                socket.join(vec!["room 2"]).await;
                socket
                    .emit_to_with_ack(
                        vec!["room 2"],
                        "server_ask_ack",
                        json!(true),
                        Duration::from_millis(400),
                        server_recv_ack,
                    )
                    .await;
                socket.leave(vec!["room 2"]).await;
            }
            .boxed()
        };

        let url = rust_socket_io_server();
        let server = ServerBuilder::new(url.port().unwrap())
            .on("/admin", "echo", echo_callback)
            .on("/admin", "client_ack", client_ack)
            .on("/admin", "trigger_server_ack", trigger_ack)
            .build();

        tokio::spawn(async move { server.serve().await });
    }
}

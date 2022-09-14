use bytes::Bytes;
use futures_util::SinkExt;
use futures_util::{future::poll_fn, StreamExt};
use http::Response;
use httparse::{Request, Status, EMPTY_HEADER};
use reqwest::Url;
use std::{borrow::Cow, collections::VecDeque, net::SocketAddr};
use std::{str::from_utf8, sync::Arc};
use tokio::net::TcpStream;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadBuf},
    sync::mpsc::{channel, Receiver, Sender},
};
use tokio_tungstenite::{accept_async, MaybeTlsStream, WebSocketStream};
use tungstenite::Message;

use crate::{
    error::Result,
    packet::build_polling_payload,
    transports::{polling::ServerPollingTransport, websocket::WebsocketTransport},
    Error,
};
use crate::{Packet, PacketType, Sid};

use super::Server;

/// Limit for the number of header lines.
const MAX_HEADERS: usize = 124;

pub type PollingHandle = (Sender<Bytes>, Receiver<Bytes>);

pub(crate) struct Polling {}

impl Polling {
    pub(crate) async fn handle(
        server: Server,
        mut stream: TcpStream,
        peer_addr: &SocketAddr,
    ) -> Result<()> {
        match read_request_type(&mut stream, peer_addr, server.max_payload()).await {
            Some(RequestType::PollingOpen) => {
                let sid = server.generate_sid();
                let transport = Self::polling_transport(&server, sid.clone()).await;

                if server
                    .store_transport(sid.clone(), Box::new(transport))
                    .await
                    .is_ok()
                {
                    write_stream(&mut stream, 200, Some(Self::handshake_body(&server, sid))).await
                } else {
                    write_stream(&mut stream, 500, None).await
                }
            }
            Some(RequestType::PollingPost(sid, data)) => {
                Self::polling_post(&server, &sid, data).await;
                write_stream(&mut stream, 200, Some("ok".to_string())).await
            }
            Some(RequestType::PollingGet(sid)) => {
                let data = Self::polling_get(&server, &sid).await;
                write_stream(&mut stream, 200, data).await
            }
            _ => write_stream(&mut stream, 400, None).await,
        }
    }

    fn handshake_body(server: &Server, sid: Sid) -> String {
        let packet = server.handshake_packet(vec!["websocket".to_owned()], Some(sid));
        // SAFETY: all fields are safe to serialize
        let data = serde_json::to_string(&packet).unwrap();
        format!("{}{}", PacketType::Open as u8, data)
    }

    async fn polling_transport(server: &Server, sid: Sid) -> ServerPollingTransport {
        let (send_tx, send_rx) = channel(server.polling_buffer());
        let (recv_tx, recv_rx) = channel(server.polling_buffer());

        let handles = server.polling_handles();
        let mut handles = handles.lock().await;
        handles.insert(sid.clone(), (recv_tx, send_rx));

        ServerPollingTransport::new(send_tx, recv_rx)
    }

    async fn polling_get(server: &Server, sid: &Sid) -> Option<String> {
        let handles = server.polling_handles();
        let mut handles = handles.lock().await;
        if let Some((_, rx)) = handles.get_mut(sid) {
            let mut byte_vec = VecDeque::new();
            while let Ok(bytes) = rx.try_recv() {
                byte_vec.push_back(bytes);
            }
            match build_polling_payload(byte_vec) {
                Some(payload) => return Some(payload),
                None => return Some(PacketType::Noop.into()),
            };
        }
        None
    }

    async fn polling_post(server: &Server, sid: &Sid, data: Bytes) {
        let handles = server.polling_handles();
        let mut handles = handles.lock().await;

        if let Some((ref mut tx, _)) = handles.get_mut(sid) {
            let _ = tx.send(data).await;
        }
    }
}

pub(crate) struct Websocket {}

impl Websocket {
    pub(crate) async fn handle(
        server: Server,
        sid: Option<Sid>,
        stream: MaybeTlsStream<TcpStream>,
        _addr: &SocketAddr,
    ) -> Result<()> {
        let mut ws_stream = accept_async(stream).await?;
        let sid = match sid {
            // websocket connecting directly, instead of upgrading from polling
            None => handshake(server.clone(), &mut ws_stream).await?,
            Some(sid) => handle_probe(server.clone(), sid, &mut ws_stream).await?,
        };

        let (sender, receiver) = ws_stream.split();
        let transport = WebsocketTransport::new(sender, receiver);

        server.store_transport(sid, Box::new(transport)).await?;

        Ok(())
    }
}

pub(crate) async fn handle_http(
    server: Server,
    stream: TcpStream,
    peer_addr: SocketAddr,
) -> Result<()> {
    // TODO: tls
    match peek_request_type(&stream, &peer_addr, server.max_payload()).await {
        Some(RequestType::WsUpgrade(sid)) => {
            Websocket::handle(server, sid, MaybeTlsStream::Plain(stream), &peer_addr).await
        }
        _ => Polling::handle(server.clone(), stream, &peer_addr).await,
    }
}

async fn handle_probe(
    server: Server,
    sid: Sid,
    ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> Result<Sid> {
    if let Some(Ok(Message::Text(packet))) = ws_stream.next().await {
        if packet == "2probe" {
            let message = Message::text(Cow::Borrowed(from_utf8(&Bytes::from(Packet::new(
                PacketType::Pong,
                Bytes::from("probe"),
            )))?));
            ws_stream.send(message).await?;
        }
    }

    if let Some(Ok(Message::Text(packet))) = ws_stream.next().await {
        // PacketType::Upgrade
        if packet == "5" {
            close_polling(&server, &sid).await;
            return Ok(sid);
        }
    }

    Err(Error::InvalidHandShake(
        "upgrade missing packet".to_string(),
    ))
}

async fn close_polling(server: &Server, sid: &Sid) {
    let handles = server.polling_handles();
    let mut handles = handles.lock().await;
    handles.remove(sid);
}

async fn handshake(
    server: Server,
    ws_stream: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
) -> Result<Sid> {
    let sid = server.generate_sid();
    let packet = server.handshake_packet(vec![], Some(sid.clone()));
    // SAFETY: all fields are safe to serialize
    let data = serde_json::to_string(&packet).unwrap();
    let message = Message::text(Cow::Borrowed(from_utf8(&Bytes::from(Packet::new(
        PacketType::Open,
        Bytes::from(data),
    )))?));
    ws_stream.send(message).await?;
    Ok(sid)
}

pub(crate) enum RequestType {
    WsUpgrade(Option<Sid>),
    PollingOpen,
    PollingGet(Sid),
    PollingPost(Sid, Bytes),
}

pub(crate) async fn peek_request_type(
    stream: &TcpStream,
    addr: &SocketAddr,
    max_payload: usize,
) -> Option<RequestType> {
    let mut buf = vec![0; max_payload];
    let mut buf = ReadBuf::new(&mut buf);

    poll_fn(|cx| stream.poll_peek(cx, &mut buf)).await.ok()?;
    parse_request_type(buf.filled(), addr)
}

async fn read_request_type(
    stream: &mut TcpStream,
    addr: &SocketAddr,
    max_payload: usize,
) -> Option<RequestType> {
    let mut buf = vec![0; max_payload];
    let n = stream.read(&mut buf).await.ok()?;

    parse_request_type(&buf[0..n], addr)
}

pub(crate) fn parse_request_type(buf: &[u8], addr: &SocketAddr) -> Option<RequestType> {
    let mut header_buf = [EMPTY_HEADER; MAX_HEADERS];
    let mut req = Request::new(&mut header_buf);
    let (req, idx) = match req.parse(buf) {
        Ok(Status::Complete(idx)) => (req, idx),
        _ => return None,
    };

    if req.method? != "GET" && req.method? != "POST" {
        return None;
    }

    let mut content_length = 0;
    let url = format!("http://{}{}", addr, req.path?);
    let url = Url::parse(&url).ok()?;
    let mut sid = None;

    for (query_key, query_value) in url.query_pairs() {
        if query_key == "EIO" && query_value != "4" {
            return None;
        }
        if query_key == "sid" {
            sid = Some(Arc::new(query_value.to_string()));
        }
    }

    for header in req.headers {
        if header.name == "Upgrade" && req.method? == "GET" {
            return Some(RequestType::WsUpgrade(sid));
        }

        if header.name == "Content-Length" {
            let len_str = from_utf8(header.value).ok()?;
            content_length = len_str.parse().ok()?;
        }
    }

    if req.method? == "POST" {
        let body_bytes = Bytes::from(buf[idx..idx + content_length].to_vec());

        if let Some(sid) = sid {
            return Some(RequestType::PollingPost(sid, body_bytes));
        }
    }

    match sid {
        Some(sid) => Some(RequestType::PollingGet(sid)),
        _ => Some(RequestType::PollingOpen),
    }
}

async fn write_stream(stream: &mut TcpStream, status: u16, body: Option<String>) -> Result<()> {
    let response = http_response(status, body); // not ok, will lost message
    stream.write_all(&Bytes::from(response)).await?;
    Ok(())
}

fn http_response(status: u16, body: Option<String>) -> String {
    let body_len = match body {
        None => 0,
        Some(ref b) => b.len(),
    };
    let response = Response::builder()
        .status(status)
        .header("Content-Type", "text/plain; charset=UTF-8")
        .header("Connection", "Close")
        .header("Content-Length", body_len)
        .body(body);
    // SAFETY: all response fields are valid to build
    let response = response.unwrap();

    let mut response_str = format!(
        "{version:?} {status}\r\n",
        version = response.version(),
        status = response.status()
    );

    for (k, v) in response.headers() {
        // SAFETY: all header value is valid
        let header = format!("{}: {}\r\n", k, v.to_str().unwrap());
        response_str.push_str(&header);
    }

    if let Some(body) = response.body() {
        response_str.push_str("\r\n");
        response_str.push_str(body);
    }

    response_str
}

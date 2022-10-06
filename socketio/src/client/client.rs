use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    sync::Arc,
};

use crate::{
    callback::Callback,
    socket::{RawSocket, Socket},
    Event,
};

use tokio::sync::RwLock;

#[derive(Clone)]
pub struct Client {
    inner: Socket<Self>,
}

impl Deref for Client {
    type Target = Socket<Self>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl DerefMut for Client {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

impl Client {
    pub(crate) fn new<T: Into<String>>(
        socket: RawSocket,
        namespace: T,
        on: Arc<RwLock<HashMap<Event, Callback<Self>>>>,
    ) -> Self {
        Self {
            inner: Socket::new(socket, namespace, on, Arc::new(|inner| Client { inner })),
        }
    }
}

// TODO: move CommonClient to Socket

#[cfg(test)]
mod test {

    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::{FutureExt, StreamExt};
    use serde_json::json;
    use tokio::time::sleep;

    use crate::{
        client::{builder::ClientBuilder, client::Client},
        error::Result,
        packet::{Packet, PacketType},
        Payload, TransportType,
    };

    #[tokio::test]
    async fn socket_io_integration() -> Result<()> {
        let url = crate::test::socket_io_server();

        let socket = ClientBuilder::new(url)
            .on("test", |msg, _, _| {
                async {
                    match msg {
                        Payload::String(str) => println!("Received string: {}", str),
                        Payload::Binary(bin) => println!("Received binary data: {:#?}", bin),
                    }
                }
                .boxed()
            })
            .connect()
            .await?;

        let payload = json!({"token": 123_i32});
        let result = socket
            .emit("test", Payload::String(payload.to_string()))
            .await;

        assert!(result.is_ok());

        let ack = socket
            .emit_with_ack(
                "test",
                Payload::String(payload.to_string()),
                Duration::from_secs(1),
                |message: Payload, socket: Client, _| {
                    async move {
                        let result = socket
                            .emit(
                                "test",
                                Payload::String(json!({"got ack": true}).to_string()),
                            )
                            .await;
                        assert!(result.is_ok());

                        println!("Yehaa! My ack got acked?");
                        if let Payload::String(str) = message {
                            println!("Received string Ack");
                            println!("Ack data: {}", str);
                        }
                    }
                    .boxed()
                },
            )
            .await;
        assert!(ack.is_ok());

        sleep(Duration::from_secs(2)).await;

        assert!(socket.disconnect().await.is_ok());

        Ok(())
    }

    #[tokio::test]
    async fn socket_io_builder_integration() -> Result<()> {
        let url = crate::test::socket_io_server();

        // test socket build logic
        let socket_builder = ClientBuilder::new(url);

        let socket = socket_builder
            .namespace("/admin")
            .opening_header("accept-encoding", "application/json")
            .on("test", |str, _, _| {
                async move { println!("Received: {:#?}", str) }.boxed()
            })
            .on("message", |payload, _, _| {
                async move { println!("{:#?}", payload) }.boxed()
            })
            .connect()
            .await?;

        assert!(socket.emit("message", json!("Hello World")).await.is_ok());

        assert!(socket
            .emit("binary", Bytes::from_static(&[46, 88]))
            .await
            .is_ok());

        assert!(socket
            .emit_with_ack(
                "binary",
                json!("pls ack"),
                Duration::from_secs(1),
                |payload, _, _| async move {
                    println!("Yehaa the ack got acked");
                    println!("With data: {:#?}", payload);
                }
                .boxed()
            )
            .await
            .is_ok());

        sleep(Duration::from_secs(2)).await;

        Ok(())
    }

    #[tokio::test]
    async fn socket_io_builder_integration_iterator() -> Result<()> {
        let url = crate::test::socket_io_server();

        // test socket build logic
        let socket_builder = ClientBuilder::new(url);

        let socket = socket_builder
            .namespace("/admin")
            .opening_header("accept-encoding", "application/json")
            .on("test", |str, _, _| {
                async move { println!("Received: {:#?}", str) }.boxed()
            })
            .on("message", |payload, _, _| {
                async move { println!("{:#?}", payload) }.boxed()
            })
            .connect_manual()
            .await?;

        assert!(socket.emit("message", json!("Hello World")).await.is_ok());

        assert!(socket
            .emit("binary", Bytes::from_static(&[46, 88]))
            .await
            .is_ok());

        assert!(socket
            .emit_with_ack(
                "binary",
                json!("pls ack"),
                Duration::from_secs(1),
                |payload, _, _| async move {
                    println!("Yehaa the ack got acked");
                    println!("With data: {:#?}", payload);
                }
                .boxed()
            )
            .await
            .is_ok());

        test_socketio_socket(socket, "/admin".to_owned()).await
    }

    #[tokio::test]
    async fn socketio_polling_integration() -> Result<()> {
        let url = crate::test::socket_io_server();
        let socket = ClientBuilder::new(url.clone())
            .transport_type(TransportType::Polling)
            .connect_manual()
            .await?;
        test_socketio_socket(socket, "/".to_owned()).await
    }

    #[tokio::test]
    async fn socket_io_websocket_integration() -> Result<()> {
        let url = crate::test::socket_io_server();
        let socket = ClientBuilder::new(url.clone())
            .transport_type(TransportType::Websocket)
            .connect_manual()
            .await?;
        test_socketio_socket(socket, "/".to_owned()).await
    }

    #[tokio::test]
    async fn socket_io_websocket_upgrade_integration() -> Result<()> {
        let url = crate::test::socket_io_server();
        let socket = ClientBuilder::new(url)
            .transport_type(TransportType::WebsocketUpgrade)
            .connect_manual()
            .await?;
        test_socketio_socket(socket, "/".to_owned()).await
    }

    #[tokio::test]
    async fn socket_io_any_integration() -> Result<()> {
        let url = crate::test::socket_io_server();
        let socket = ClientBuilder::new(url)
            .transport_type(TransportType::Any)
            .connect_manual()
            .await?;
        test_socketio_socket(socket, "/".to_owned()).await
    }

    async fn test_socketio_socket(mut socket: Client, nsp: String) -> Result<()> {
        // open packet
        let _: Option<Packet> = Some(socket.next().await.unwrap()?);

        let packet: Option<Packet> = Some(socket.next().await.unwrap()?);

        assert!(packet.is_some());

        let packet = packet.unwrap();

        assert_eq!(
            packet,
            Packet::new(
                PacketType::Event,
                nsp.clone(),
                Some("[\"Hello from the message event!\"]".to_owned()),
                None,
                0,
                None,
            )
        );

        let packet: Option<Packet> = Some(socket.next().await.unwrap()?);

        assert!(packet.is_some());

        let packet = packet.unwrap();

        assert_eq!(
            packet,
            Packet::new(
                PacketType::Event,
                nsp.clone(),
                Some("[\"test\",\"Hello from the test event!\"]".to_owned()),
                None,
                0,
                None
            )
        );
        let packet: Option<Packet> = Some(socket.next().await.unwrap()?);

        assert!(packet.is_some());

        let packet = packet.unwrap();
        assert_eq!(
            packet,
            Packet::new(
                PacketType::BinaryEvent,
                nsp.clone(),
                None,
                None,
                1,
                Some(vec![Bytes::from_static(&[4, 5, 6])]),
            )
        );

        let packet: Option<Packet> = Some(socket.next().await.unwrap()?);

        assert!(packet.is_some());

        let packet = packet.unwrap();
        assert_eq!(
            packet,
            Packet::new(
                PacketType::BinaryEvent,
                nsp.clone(),
                Some("\"test\"".to_owned()),
                None,
                1,
                Some(vec![Bytes::from_static(&[1, 2, 3])]),
            )
        );

        let cb = |message: Payload, _, _| {
            async {
                println!("Yehaa! My ack got acked?");
                if let Payload::String(str) = message {
                    println!("Received string ack");
                    println!("Ack data: {}", str);
                }
            }
            .boxed()
        };

        assert!(socket
            .emit_with_ack(
                "test",
                Payload::String("123".to_owned()),
                Duration::from_secs(10),
                cb
            )
            .await
            .is_ok());

        Ok(())
    }
}

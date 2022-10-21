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

#[cfg(test)]
mod test {
    use std::time::Duration;

    use crate::{
        test::socket_io_server, AckId, Client, ClientBuilder, Event, Packet, PacketType, Payload,
        Result, ServerBuilder, ServerClient,
    };

    use bytes::Bytes;
    use futures_util::{FutureExt, StreamExt};
    use serde_json::json;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_client() -> Result<()> {
        // tracing_subscriber::fmt()
        //     .with_env_filter("engineio=trace,socketio=trace,integration=trace")
        //     .init();
        setup_server();

        socket_io_integration().await?;
        socket_io_builder_integration().await?;
        socket_io_builder_integration_iterator().await?;
        Ok(())
    }

    async fn socket_io_integration() -> Result<()> {
        let url = socket_io_server();

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

    async fn socket_io_builder_integration() -> Result<()> {
        let url = socket_io_server();

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

    async fn socket_io_builder_integration_iterator() -> Result<()> {
        let url = socket_io_server();

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

    async fn test_socketio_socket(mut socket: Client, nsp: String) -> Result<()> {
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

    fn setup_server() {
        let echo_callback =
            move |_payload: Payload, socket: ServerClient, _need_ack: Option<AckId>| {
                async move {
                    socket.join(vec!["room 1"]).await;
                    socket.emit_to(vec!["room 1"], "echo", json!("")).await;
                    socket.leave(vec!["room 1"]).await;
                }
                .boxed()
            };

        let client_ack = move |_payload: Payload, socket: ServerClient, need_ack: Option<AckId>| {
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
            move |_payload: Payload, socket: ServerClient, _need_ack: Option<AckId>| {
                async move {
                    socket
                        .emit("server_recv_ack", json!(""))
                        .await
                        .expect("success");
                }
                .boxed()
            };

        let trigger_ack = move |_message: Payload, socket: ServerClient, _| {
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

        let connect_cb = move |_payload: Payload, socket: ServerClient, _| {
            async move {
                socket
                    .emit("test", "Hello from the test event!")
                    .await
                    .expect("success");

                socket
                    .emit("test", Payload::Binary(Bytes::from_static(&[1, 2, 3])))
                    .await
                    .expect("success");
            }
            .boxed()
        };

        let url = socket_io_server();
        let server = ServerBuilder::new(url.port().unwrap())
            .on("/admin", "echo", echo_callback)
            .on("/admin", "client_ack", client_ack)
            .on("/admin", "trigger_server_ack", trigger_ack)
            .on("/admin", Event::Connect, connect_cb)
            .build();

        tokio::spawn(async move { server.serve().await });
    }
}

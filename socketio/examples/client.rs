use bytes::Bytes;
use futures_util::FutureExt;
use serde_json::json;
use socketio_rs::{ClientBuilder, Payload, Socket};
use std::time::Duration;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("engineio=info,socketio=info")
        .init();

    // define a callback which is called when a payload is received
    // this callback gets the payload as well as an instance of the
    // socket to communicate with the server
    let callback = |payload: Option<Payload>, socket: Socket, _| {
        async move {
            match payload {
                Some(Payload::Json(data)) => println!("Received: {:?}", data),
                Some(Payload::Binary(bin)) => println!("Received bytes: {:#?}", bin),
                Some(Payload::Multi(multi)) => println!("Received multi: {:?}", multi),
                _ => {}
            }
            socket
                .emit("test", json!({"got ack": true}))
                .await
                .expect("Server unreachable");
        }
        .boxed()
    };

    // get a socket that is connected to the admin namespace
    let client = ClientBuilder::new("http://localhost:4209/")
        .namespace("/admin")
        .on("test", callback)
        .on("error", |err, _, _| {
            async move { eprintln!("Error: {:#?}", err) }.boxed()
        })
        .connect()
        .await
        .expect("Connection failed");

    // emit to the "foo" event
    let json_payload = json!({"token": 123});
    client
        .emit("foo", json_payload)
        .await
        .expect("Server unreachable");

    // define a callback, that's executed when the ack got acked
    let ack_callback = |message: Option<Payload>, _: Socket, _| {
        async move {
            println!("Yehaa! My ack got acked?");
            println!("Ack data: {:#?}", message);
        }
        .boxed()
    };

    let payload: Payload = Payload::Multi(vec![
        json!({"myAckData": 123}).into(),
        json!("4").into(),
        Bytes::from_static(b"binary").into(),
    ]);
    // emit with an ack
    client
        .emit_with_ack("ack", payload, Duration::from_secs(2), ack_callback)
        .await
        .expect("Server unreachable");

    tokio::time::sleep(Duration::from_secs(5)).await;
    client.disconnect().await.expect("Disconnect failed");
}

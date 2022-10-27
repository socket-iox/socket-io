use futures_util::FutureExt;
use serde_json::json;
use socketio_rs::{AckId, Payload, ServerBuilder, ServerSocket};

#[tokio::main]
async fn main() {
    // tracing_subscriber::fmt()
    //     .with_env_filter("engineio=trace,socketio=trace")
    //     .init();
    let callback = |_payload: Payload, socket: ServerSocket, _| {
        async move {
            socket.join(vec!["room 1"]).await;
            socket.emit_to(vec!["room 1"], "test", "foo").await;
        }
        .boxed()
    };

    let ack_callback = |_payload: Payload, socket: ServerSocket, ack: Option<AckId>| {
        async move {
            if let Some(id) = ack {
                let _ = socket.ack(id, json!("ack back")).await;
            }
        }
        .boxed()
    };
    let server = ServerBuilder::new(4209)
        .on("/admin", "foo", callback)
        .on("/admin", "ack", ack_callback)
        .build();
    server.serve().await;
}

use futures_util::FutureExt;
use socketio_rs::{Payload, ServerBuilder, ServerClient};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("engineio=trace,socketio=trace")
        .init();
    let callback = |_payload: Payload, socket: ServerClient, _| {
        async move {
            socket.join(vec!["room 1"]).await;
            socket.emit_to(vec!["room 1"], "test", "foo").await;
        }
        .boxed()
    };
    let server = ServerBuilder::new(4209)
        .on("/admin", "foo", callback)
        .build();
    server.serve().await;
}

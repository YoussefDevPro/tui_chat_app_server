use futures_util::{SinkExt, StreamExt};
use redis::AsyncCommands;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::Message as WsMessage;

pub async fn websocket_server(redis_url: String, addr: &str) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    println!("WebSocket listening on ws://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let redis_url = redis_url.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &redis_url).await {
                eprintln!("WebSocket error: {:?}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    redis_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let ws_stream = accept_async(stream).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Connect to Redis and subscribe to "chat"
    let client = redis::Client::open(redis_url)?;
    let conn = client.get_async_connection().await?;
    let mut pubsub = conn.into_pubsub();
    pubsub.subscribe("chat").await?;

    // Move pubsub into the async task and create the stream inside the task
    let ws_send_task = tokio::spawn(async move {
        let mut pubsub_stream = pubsub.on_message();
        while let Some(msg) = pubsub_stream.next().await {
            let payload: String = msg.get_payload().unwrap_or_default();
            if ws_sender
                .send(WsMessage::Text(payload.into()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Listen for incoming messages from WebSocket and publish to Redis
    let redis_client = redis::Client::open(redis_url)?;
    let mut publish_conn = redis_client.get_async_connection().await?;
    while let Some(Ok(msg)) = ws_receiver.next().await {
        if let WsMessage::Text(text) = msg {
            let _: () = publish_conn.publish("chat", text.to_string()).await?;
        }
    }

    ws_send_task.await?;

    Ok(())
}

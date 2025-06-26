use crate::models::Message;
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{decode, Algorithm, DecodingKey, TokenData, Validation};
use redis::AsyncCommands;
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::{frame::coding::CloseCode, CloseFrame, Message as WsMessage};

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    // Add other fields as needed
}

pub async fn websocket_server(
    redis_url: String,
    addr: &str,
    jwt_secret: String,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    println!("WebSocket listening on ws://{}", addr);

    loop {
        let (stream, _) = listener.accept().await?;
        let redis_url = redis_url.clone();
        let jwt_secret = jwt_secret.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &redis_url, &jwt_secret).await {
                eprintln!("WebSocket error: {:?}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    redis_url: &str,
    jwt_secret: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let ws_stream = accept_async(stream).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // --- JWT handshake ---
    let jwt_msg = match ws_receiver.next().await {
        Some(Ok(WsMessage::Text(token))) => token,
        _ => {
            let _ = ws_sender
                .send(WsMessage::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "No token provided".into(),
                })))
                .await;
            return Ok(());
        }
    };

    let token_data: Result<TokenData<Claims>, _> = decode::<Claims>(
        &jwt_msg,
        &DecodingKey::from_secret(jwt_secret.as_ref()),
        &Validation::new(Algorithm::HS256),
    );

    let user = match token_data {
        Ok(data) => data.claims.sub.clone(),
        Err(_) => {
            let _ = ws_sender
                .send(WsMessage::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "Invalid or expired token".into(),
                })))
                .await;
            return Ok(());
        }
    };

    println!("WebSocket auth OK for user: {}", user);

    // Connect to Redis and subscribe to "chat"
    let client = redis::Client::open(redis_url)?;
    let conn = client.get_async_connection().await?;
    let mut pubsub = conn.into_pubsub();
    pubsub.subscribe("chat").await?;

    // Forward Redis messages to this client (JSON format)
    let ws_send_task = tokio::spawn(async move {
        let mut pubsub_stream = pubsub.on_message();
        let mut ws_sender = ws_sender;
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

    // Listen for incoming messages from this client and publish to Redis (as JSON)
    let redis_client = redis::Client::open(redis_url)?;
    let mut publish_conn = redis_client.get_async_connection().await?;
    while let Some(Ok(msg)) = ws_receiver.next().await {
        if let WsMessage::Text(text) = msg {
            // Construct message struct
            let message = Message {
                user: user.clone(),
                content: text.to_string(),
                timestamp: Utc::now().timestamp(),
            };
            let msg_json = serde_json::to_string(&message)?;
            let _: () = publish_conn.publish("chat", msg_json).await?;
        }
    }

    ws_send_task.await?;

    Ok(())
}

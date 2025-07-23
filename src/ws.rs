use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{decode, Algorithm, DecodingKey, TokenData, Validation};
use log::{debug, error, info, warn};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Sqlite};

use crate::commands::{admin, user};
use crate::data_access;
use crate::models::Channel;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::{frame::coding::CloseCode, CloseFrame, Message as WsMessage};

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BroadcastMessage {
    pub user: String,
    pub icon: String,
    pub content: String,
    pub timestamp: i64,
    pub channel_id: String,
    pub channel_name: String,
    pub channel_icon: String,
    #[serde(default = "default_message_type")]
    pub message_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_extension: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_size_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_image: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_progress: Option<u8>,
}

fn default_message_type() -> String {
    "text".to_string()
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum ClientWsMessage {
    Text(String),
    ChannelCommand { content: String, channel_id: String },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChannelBroadcast {
    pub id: String, // Keep as String for external communication
    pub name: String,
    pub icon: String,
}

// New struct for channel proposals

pub async fn websocket_server(
    db_pool: Pool<Sqlite>,
    redis_url: String,
    addr: &str,
    jwt_secret: String,
) -> std::io::Result<()> {
    info!("Attempting to bind WebSocket server to: {}", addr);
    let listener = TcpListener::bind(addr).await?;
    info!("WebSocket listening on ws://{}", addr);

    loop {
        debug!("Waiting for new WebSocket connection...");
        let (stream, peer_addr) = listener.accept().await?;
        info!("New WebSocket connection from: {}", peer_addr);
        let redis_url = redis_url.clone();
        let jwt_secret = jwt_secret.clone();
        let db_pool = db_pool.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &db_pool, &redis_url, &jwt_secret).await {
                error!("WebSocket connection handler error: {:?}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    db_pool: &Pool<Sqlite>,
    redis_url: &str,
    jwt_secret: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync + 'static>> {
    debug!("Handling new WebSocket connection.");
    let ws_stream = accept_async(stream).await?;
    info!("WebSocket handshake successful.");
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<WsMessage>();

    // Define a local send_reply closure for server-generated messages

    async fn send_reply(
        msg_tx: &tokio::sync::mpsc::UnboundedSender<WsMessage>,
        msg: String,
        icon: String,
        channel_info: Channel,
    ) {
        let reply = BroadcastMessage {
            user: "server".to_string(),
            icon,
            content: msg,
            timestamp: Utc::now().timestamp(),
            channel_id: channel_info.id.clone(),
            channel_name: channel_info.name.clone(),
            channel_icon: channel_info.icon.clone(),
            message_type: "text".to_string(),
            file_name: None,
            file_extension: None,
            file_icon: None,
            file_size_mb: None,
            is_image: None,
            image_preview: None,
            file_id: None,
            download_url: None,
        };
        let reply_json = serde_json::to_string(&reply).unwrap_or_else(|e| {
            eprintln!("Error serializing reply: {:?}", e);
            "{}".to_string()
        });
        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
    }

    // Authenticate user with JWT
    debug!("Waiting for JWT token from client.");
    let jwt_msg = match ws_receiver.next().await {
        Some(Ok(WsMessage::Text(token))) => {
            info!("Received JWT token from client.");
            token
        }
        _ => {
            warn!("No JWT token provided or invalid message type. Closing connection.");
            let _ = ws_sender
                .send(WsMessage::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "No token provided".into(),
                })))
                .await;
            return Ok(());
        }
    };

    debug!("Decoding JWT token.");
    let token_data: Result<TokenData<Claims>, _> = decode::<Claims>(
        &jwt_msg,
        &DecodingKey::from_secret(jwt_secret.as_ref()),
        &Validation::new(Algorithm::HS256),
    );

    let user_username = match token_data {
        Ok(data) => {
            info!(
                "JWT token decoded successfully for user: {}",
                data.claims.sub
            );
            data.claims.sub.clone()
        }
        Err(e) => {
            warn!("Invalid or expired JWT token. Closing connection: {:?}", e);
            let _ = ws_sender
                .send(WsMessage::Close(Some(CloseFrame {
                    code: CloseCode::Policy,
                    reason: "Invalid or expired token".into(),
                })))
                .await;
            return Ok(());
        }
    };

    info!(
        "WebSocket authentication successful for user: {}",
        user_username
    );

    // Redis connections for pubsub and commands
    debug!("Opening Redis client for pubsub and commands.");
    let client = redis::Client::open(redis_url)?;
    // This connection will be moved into the pubsub_task
    let mut pubsub_listener_conn = client.get_async_connection().await?.into_pubsub();
    // This connection will be used for publishing and other commands
    let mut publish_conn = client.get_async_connection().await?;
    // This connection will be used for subscribing to new channels outside the listener task
    let _pubsub_publisher_conn = client.get_async_connection().await?.into_pubsub();
    info!("Redis connections established.");
    // Add user to active users set in Redis
    let _: () = publish_conn.sadd("active_users", &user_username).await?;

    // Send initial list of all APPROVED channels to the client upon connection
    let all_channels = data_access::get_all_approved_channels(db_pool).await?;
    let channel_list_message = serde_json::json!({ "ChannelList": all_channels });
    let channel_list_json = serde_json::to_string(&channel_list_message)?;
    if ws_sender.send(WsMessage::Text(channel_list_json.into())).await.is_err() {
        warn!("Failed to send channel list to client. Closing connection.");
        return Ok(());
    }

    // Subscribe to all existing channels' Redis topics using pubsub_listener_conn
    for channel_info in &all_channels {
        let _ = pubsub_listener_conn
            .subscribe(format!("chat:{}", channel_info.id))
            .await;
    }

    // Subscribe to a general channel_updates topic for new channel notifications
    let _ = pubsub_listener_conn.subscribe("channel_updates").await;

    // Task to forward messages from Redis pubsub to the client
    let ws_sender_clone = msg_tx.clone(); // Clone for the pubsub task
    let pubsub_task = tokio::spawn(async move {
        let mut message_stream = pubsub_listener_conn.on_message(); // Get the message stream inside the task
        while let Some(msg) = message_stream.next().await {
            // Correctly consume from the stream
            if let Ok(payload) = msg.get_payload::<String>() {
                let _ = ws_sender_clone.send(WsMessage::Text(payload.into()));
            }
        }
    });

    // Task to send messages from the mpsc channel to the WebSocket client
    let ws_send_task = tokio::spawn(async move {
        while let Some(msg) = msg_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Main loop for handling incoming WebSocket messages from the client
    while let Some(Ok(ws_msg)) = ws_receiver.next().await {
        let now_ts = Utc::now().timestamp();
        let current_user_info = data_access::get_user_info(db_pool, &user_username).await?;

        let client_msg: ClientWsMessage = match ws_msg {
            WsMessage::Text(text) => {
                if let Ok(cmd) = serde_json::from_str(&text) {
                    cmd
                } else {
                    ClientWsMessage::Text(text.to_string())
                }
            }
            WsMessage::Ping(payload) => {
                let _ = msg_tx.send(WsMessage::Pong(payload));
                continue;
            }
            WsMessage::Close(_) => break, // Client wants to close connection
            _ => continue,                // Ignore other message types (Binary, Pong, Frame)
        };

        let (mut content, target_channel_id) = match client_msg.clone() {
            ClientWsMessage::Text(t) => {
                // For simple text messages, assume it's for the 'home' channel
                // In a real client, the client would specify the channel ID
                (t, "home".to_string())
            }
            ClientWsMessage::ChannelCommand {
                content,
                channel_id,
            } => (content, channel_id),
        };

        let channel_info_result = data_access::get_channel_info(db_pool, &target_channel_id).await;
        let channel_info = match channel_info_result {
            Ok(info) => info,
            Err(_) => {
                // Create a dummy channel_info for the reply if the channel is not found
                let dummy_channel_info = Channel {
                    id: target_channel_id.clone(),
                    name: "Unknown Channel".to_string(),
                    icon: "🚫".to_string(),
                };
                send_reply(
                    &msg_tx,
                    format!("Channel '{}' not found.", target_channel_id),
                    "🚫".to_string(),
                    dummy_channel_info,
                )
                .await;
                continue;
            }
        };

        // --- Global Ban Check (Super Admins are immune) ---
        if current_user_info.is_banned && !current_user_info.is_super_admin {
            if current_user_info.ban_mute_until.unwrap_or(0) > now_ts {
                send_reply(
                    &msg_tx,
                    format!("You are currently globally banned and cannot send messages."),
                    "⛔".to_string(),
                    channel_info.clone(),
                )
                .await;
                continue;
            }
            // If global ban expired, but user is still IsBanned = 1, re-mute for 30 mins
            let _ = sqlx::query("UPDATE users SET BanMuteUntil = ? WHERE username = ?")
                .bind(now_ts + 30 * 60)
                .bind(&user_username)
                .execute(db_pool)
                .await?;
            send_reply(
                &msg_tx,
                format!(
                    "User '{}' (globally banned) tried to send a message. Global mute extended.",
                    user_username
                ),
                "⛔".to_string(),
                channel_info.clone(),
            )
            .await;
            continue;
        }

        let channel_ban_status =
            data_access::get_channel_ban_info(db_pool, &channel_info.id, &user_username).await?;
        if let Some(ban_info) = channel_ban_status {
            if let Some(mute_until) = ban_info.ban_mute_until {
                if mute_until == 0 || mute_until > now_ts {
                    // 0 for permanent ban
                    send_reply(
                        &msg_tx,
                        format!("You are banned from this channel."),
                        "⛔".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                    continue;
                } else {
                    // Ban expired, remove the ban entry
                    let _ = sqlx::query(
                        "DELETE FROM channel_bans WHERE channel_id = ? AND banned_username = ?",
                    )
                    .bind(&channel_info.id)
                    .bind(&user_username)
                    .execute(db_pool)
                    .await?;
                    send_reply(
                        &msg_tx,
                        format!("Your ban in channel '{}' has expired.", channel_info.name),
                        "✅".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                }
            }
        }

        // Handle commands
        if content.starts_with("/") {
            let parts: Vec<&str> = content.splitn(2, ' ').collect();
            let command = parts[0];
            let args = parts.get(1).unwrap_or(&"").trim();

            let mut handled = false;

            // User commands
            if user::handle_user_command(
                command,
                args,
                db_pool,
                &mut publish_conn,
                &msg_tx,
                &channel_info,
                &user_username,
            )
            .await?
            {
                handled = true;
            }

            // Admin commands
            if !handled && (current_user_info.is_admin || current_user_info.is_super_admin) {
                if admin::handle_admin_command(
                    command,
                    args,
                    db_pool,
                    &mut publish_conn,
                    &msg_tx,
                    &channel_info,
                    &user_username,
                    &current_user_info,
                )
                .await?
                {
                    handled = true;
                }
            }

            if !handled {
                send_reply(
                    &msg_tx,
                    format!("Unknown command or insufficient permissions: '{}'. Type /help for a list of commands.", content),
                    "💻".to_string(),
                    channel_info.clone(),
                )
                .await;
            }
            continue;
        }

        let meow_mode: i64 = publish_conn.get("meow_mode").await.unwrap_or(0);
        if meow_mode == 1 {
            content = "meow".to_string();
        }

        let message = BroadcastMessage {
            user: user_username.clone(),
            icon: current_user_info.icon,
            content,
            timestamp: now_ts,
            channel_id: channel_info.id.clone(),
            channel_name: channel_info.name.clone(),
            channel_icon: channel_info.icon.clone(),
            message_type: "text".to_string(),
            file_name: None,
            file_extension: None,
            file_icon: None,
            file_size_mb: None,
            is_image: None,
            image_preview: None,
            file_id: None,
            download_url: None,
        };
        let msg_json = serde_json::to_string(&message)?;
        let _: () = publish_conn
            .rpush(format!("chat_history:{}", channel_info.id), &msg_json)
            .await?;
        // Keep only the last 500 messages
        let _: () = publish_conn
            .ltrim(format!("chat_history:{}", channel_info.id), -500, -1)
            .await?;

        let _: () = publish_conn
            .publish(format!("chat:{}", channel_info.id), &msg_json)
            .await?;
    }

    // Await tasks before closing connection
    let _ = tokio::join!(ws_send_task, pubsub_task);

    // Remove user from active users set on disconnect
    let _: () = publish_conn.srem("active_users", &user_username).await?;

    Ok(())
}

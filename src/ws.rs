use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{decode, Algorithm, DecodingKey, TokenData, Validation};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Sqlite};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_tungstenite::accept_async;
use tungstenite::protocol::{frame::coding::CloseCode, CloseFrame, Message as WsMessage};

use crate::models::Channel;

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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum ClientWsMessage {
    Text(String),
    ChannelCommand { content: String, channel_id: String },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChannelBroadcast {
    pub id: String,
    pub name: String,
    pub icon: String,
}

#[derive(sqlx::FromRow, Debug, Clone)]
struct UserInfo {
    id: String,
    icon: String,
    is_admin: bool,
    is_super_admin: bool,
    is_banned: bool,
    ban_mute_until: Option<i64>,
}

#[derive(sqlx::FromRow, Debug)]
struct ChannelInfo {
    id: String,
    name: String,
    icon: String,
}

#[derive(sqlx::FromRow, Debug)]
struct ChannelBanInfo {
    ban_mute_until: Option<i64>,
}

async fn get_user_info(db: &Pool<Sqlite>, username: &str) -> Result<UserInfo, sqlx::Error> {
    sqlx::query_as::<_, UserInfo>(
        "SELECT id, icon, IsAdmin as is_admin, IsSuperAdmin as is_super_admin, IsBanned as is_banned, BanMuteUntil as ban_mute_until FROM users WHERE username = ?"
    )
    .bind(username)
    .fetch_one(db)
    .await
}

// Function to get a single channel's info using the simplified Channel struct
async fn get_channel_info(db: &Pool<Sqlite>, channel_id: &str) -> Result<Channel, sqlx::Error> {
    sqlx::query_as::<_, Channel>("SELECT id, name, icon FROM channels WHERE id = ?")
        .bind(channel_id)
        .fetch_one(db)
        .await
}

async fn get_channel_ban_info(
    db: &Pool<Sqlite>,
    channel_id: &str,
    username: &str,
) -> Result<Option<ChannelBanInfo>, sqlx::Error> {
    sqlx::query_as::<_, ChannelBanInfo>(
        "SELECT ban_mute_until FROM channel_bans WHERE channel_id = ? AND banned_username = ?",
    )
    .bind(channel_id)
    .bind(username)
    .fetch_optional(db)
    .await
}

// Function to get all channels using the simplified Channel struct
async fn get_all_channels(db: &Pool<Sqlite>) -> Result<Vec<Channel>, sqlx::Error> {
    sqlx::query_as::<_, Channel>("SELECT id, name, icon FROM channels")
        .fetch_all(db)
        .await
}

pub async fn websocket_server(
    db_pool: Pool<Sqlite>,
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
        let db_pool = db_pool.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &db_pool, &redis_url, &jwt_secret).await {
                eprintln!("WebSocket error: {:?}", e);
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
    let ws_stream = accept_async(stream).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<WsMessage>();

    // Authenticate user with JWT
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

    let user_username = match token_data {
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

    println!("WebSocket auth OK for user: {}", user_username);

    // Redis connections for pubsub and commands
    let client = redis::Client::open(redis_url)?;
    // This connection will be moved into the pubsub_task
    let mut pubsub_listener_conn = client.get_async_connection().await?.into_pubsub();
    // This connection will be used for publishing and other commands
    let mut publish_conn = client.get_async_connection().await?;
    // This connection will be used for subscribing to new channels outside the listener task
    let mut pubsub_publisher_conn = client.get_async_connection().await?.into_pubsub();

    // Send initial list of all channels to the client upon connection
    let all_channels = get_all_channels(db_pool).await?;
    for channel_info in &all_channels {
        // Borrow all_channels to avoid moving
        let channel_broadcast = ChannelBroadcast {
            id: channel_info.id.clone(),
            name: channel_info.name.clone(), // Clone name
            icon: channel_info.icon.clone(), // Clone icon
        };
        let channel_json = serde_json::to_string(&channel_broadcast)?;
        let _ = ws_sender
            .send(WsMessage::Text(
                format!("/channel_update {}", channel_json).into(),
            ))
            .await;
    }

    // Subscribe to all existing channels' Redis topics using pubsub_listener_conn
    for channel_info in all_channels {
        // Now consume all_channels
        let _ = pubsub_listener_conn
            .subscribe(format!("chat:{}", channel_info.id))
            .await;
    }

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
        let current_user_info = get_user_info(db_pool, &user_username).await?;

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

        let channel_info = match get_channel_info(db_pool, &target_channel_id).await {
            Ok(info) => info,
            Err(_) => {
                let reply = BroadcastMessage {
                    user: "server".to_string(),
                    icon: "🚫".to_string(),
                    content: format!("Channel '{}' not found.", target_channel_id),
                    timestamp: now_ts,
                    channel_id: "home".to_string(), // Default to home if channel not found
                    channel_name: "Home".to_string(),
                    channel_icon: "❓".to_string(),
                };
                let reply_json = serde_json::to_string(&reply)?;
                let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                continue;
            }
        };

        // --- Global Ban Check (Super Admins are immune) ---
        if current_user_info.is_banned && !current_user_info.is_super_admin {
            if current_user_info.ban_mute_until.unwrap_or(0) > now_ts {
                let server_msg = BroadcastMessage {
                    user: "server".to_string(),
                    icon: "⛔".to_string(),
                    content: format!("You are currently globally banned and cannot send messages."),
                    timestamp: now_ts,
                    channel_id: channel_info.id.clone(),
                    channel_name: channel_info.name.clone(),
                    channel_icon: channel_info.icon.clone(),
                };
                let server_json = serde_json::to_string(&server_msg)?;
                let _ = msg_tx.send(WsMessage::Text(server_json.into()));
                continue;
            }
            // If global ban expired, but user is still IsBanned = 1, re-mute for 30 mins
            let _ = sqlx::query("UPDATE users SET BanMuteUntil = ? WHERE username = ?")
                .bind(now_ts + 30 * 60)
                .bind(&user_username)
                .execute(db_pool)
                .await?;
            let server_msg = BroadcastMessage {
                user: "server".to_string(),
                icon: "⛔".to_string(),
                content: format!(
                    "User '{}' (globally banned) tried to send a message. Global mute extended.",
                    user_username
                ),
                timestamp: now_ts,
                channel_id: channel_info.id.clone(),
                channel_name: channel_info.name.clone(),
                channel_icon: channel_info.icon.clone(),
            };
            let server_json = serde_json::to_string(&server_msg)?;
            let _ = msg_tx.send(WsMessage::Text(server_json.into()));
            continue;
        }

        let channel_ban_status =
            get_channel_ban_info(db_pool, &channel_info.id, &user_username).await?;
        if let Some(ban_info) = channel_ban_status {
            if let Some(mute_until) = ban_info.ban_mute_until {
                if mute_until == 0 || mute_until > now_ts {
                    // 0 for permanent ban
                    let server_msg = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "⛔".to_string(),
                        content: format!("You are banned from this channel."),
                        timestamp: now_ts,
                        channel_id: channel_info.id.clone(),
                        channel_name: channel_info.name.clone(),
                        channel_icon: channel_info.icon.clone(),
                    };
                    let server_json = serde_json::to_string(&server_msg)?;
                    let _ = msg_tx.send(WsMessage::Text(server_json.into()));
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
                    let server_msg = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "✅".to_string(),
                        content: format!(
                            "Your ban in channel '{}' has expired.",
                            channel_info.name
                        ),
                        timestamp: now_ts,
                        channel_id: channel_info.id.clone(),
                        channel_name: channel_info.name.clone(),
                        channel_icon: channel_info.icon.clone(),
                    };
                    let server_json = serde_json::to_string(&server_msg)?;
                    let _ = msg_tx.send(WsMessage::Text(server_json.into()));
                }
            }
        }

        // Handle commands
        if content.starts_with("/") {
            let mut handled = false;

            if current_user_info.is_admin || current_user_info.is_super_admin {
                // Global admin commands
                if content.starts_with("/ban ") {
                    let target_username = content.trim_start_matches("/ban ").trim();
                    let target_user_info = get_user_info(db_pool, target_username).await;

                    if let Ok(info) = target_user_info {
                        if info.is_super_admin {
                            let reply = BroadcastMessage {
                                user: "server".to_string(),
                                icon: "🚫".to_string(),
                                content: format!("Cannot globally ban a super admin."),
                                timestamp: now_ts,
                                channel_id: channel_info.id.clone(),
                                channel_name: channel_info.name.clone(),
                                channel_icon: channel_info.icon.clone(),
                            };
                            let reply_json = serde_json::to_string(&reply)?;
                            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                            handled = true;
                        } else {
                            let _ = sqlx::query("UPDATE users SET IsBanned = 1 WHERE username = ?")
                                .bind(target_username)
                                .execute(db_pool)
                                .await?;
                            let reply = BroadcastMessage {
                                user: "server".to_string(),
                                icon: "💻".to_string(),
                                content: format!(
                                    "User '{}' has been GLOBALLY banned by admin '{}'.",
                                    target_username, user_username
                                ),
                                timestamp: now_ts,
                                channel_id: channel_info.id.clone(),
                                channel_name: channel_info.name.clone(),
                                channel_icon: channel_info.icon.clone(),
                            };
                            let reply_json = serde_json::to_string(&reply)?;
                            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                            handled = true;
                        }
                    } else {
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!("User '{}' not found.", target_username),
                            timestamp: now_ts,
                            channel_id: channel_info.id.clone(),
                            channel_name: channel_info.name.clone(),
                            channel_icon: channel_info.icon.clone(),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                } else if content.starts_with("/rm_admin ") {
                    let target_username = content.trim_start_matches("/rm_admin ").trim();
                    let target_user_info = get_user_info(db_pool, target_username).await;

                    if let Ok(info) = target_user_info {
                        if info.is_super_admin {
                            let reply = BroadcastMessage {
                                user: "server".to_string(),
                                icon: "🚫".to_string(),
                                content: format!("Cannot remove admin status from a super admin."),
                                timestamp: now_ts,
                                channel_id: channel_info.id.clone(),
                                channel_name: channel_info.name.clone(),
                                channel_icon: channel_info.icon.clone(),
                            };
                            let reply_json = serde_json::to_string(&reply)?;
                            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                            handled = true;
                        } else {
                            let _ = sqlx::query("UPDATE users SET IsAdmin = 0 WHERE username = ?")
                                .bind(target_username)
                                .execute(db_pool)
                                .await?;
                            let reply = BroadcastMessage {
                                user: "server".to_string(),
                                icon: "💻".to_string(),
                                content: format!(
                                    "User '{}' is no longer a global admin.",
                                    target_username
                                ),
                                timestamp: now_ts,
                                channel_id: channel_info.id.clone(),
                                channel_name: channel_info.name.clone(),
                                channel_icon: channel_info.icon.clone(),
                            };
                            let reply_json = serde_json::to_string(&reply)?;
                            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                            handled = true;
                        }
                    } else {
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!("User '{}' not found.", target_username),
                            timestamp: now_ts,
                            channel_id: channel_info.id.clone(),
                            channel_name: channel_info.name.clone(),
                            channel_icon: channel_info.icon.clone(),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                } else if content.starts_with("/meow_event") {
                    let _: () = publish_conn.set("meow_mode", 1).await?;
                    let _: () = publish_conn.expire("meow_mode", 300).await?; // 5 min
                    let reply = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "🐈".to_string(),
                        content: "Meow mode activated! Every message is now 'meow'.".to_string(),
                        timestamp: now_ts,
                        channel_id: channel_info.id.clone(),
                        channel_name: channel_info.name.clone(),
                        channel_icon: channel_info.icon.clone(),
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    handled = true;
                } else if content.starts_with("/stop_meow") {
                    let _: () = publish_conn.del("meow_mode").await?;
                    let reply = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "🐈".to_string(),
                        content: "Meow mode deactivated.".to_string(),
                        timestamp: now_ts,
                        channel_id: channel_info.id.clone(),
                        channel_name: channel_info.name.clone(),
                        channel_icon: channel_info.icon.clone(),
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    handled = true;
                }
                // New: Create channel command (simplified)
                else if content.starts_with("/create_channel ") {
                    let parts: Vec<&str> = content.splitn(4, ' ').collect(); // Removed password part
                    if parts.len() >= 4 {
                        // /create_channel <id> <name> <icon>
                        let new_channel_id = parts[1].to_string();
                        let new_channel_name = parts[2].to_string();
                        let new_channel_icon = parts[3].to_string();

                        let _ =
                            sqlx::query("INSERT INTO channels (id, name, icon) VALUES (?, ?, ?)")
                                .bind(&new_channel_id)
                                .bind(&new_channel_name)
                                .bind(&new_channel_icon)
                                .execute(db_pool)
                                .await?;

                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!(
                                "Channel '{}' (ID: {}) created.",
                                new_channel_name, new_channel_id
                            ),
                            timestamp: now_ts,
                            channel_id: channel_info.id.clone(),
                            channel_name: channel_info.name.clone(),
                            channel_icon: channel_info.icon.clone(),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));

                        // Broadcast new channel to all connected clients
                        let new_channel_broadcast = ChannelBroadcast {
                            id: new_channel_id.clone(),
                            name: new_channel_name.clone(),
                            icon: new_channel_icon.clone(),
                        };
                        let new_channel_json = serde_json::to_string(&new_channel_broadcast)?;
                        // Send to all connected clients (via Redis pubsub for channel updates)
                        let _: () = publish_conn
                            .publish("channel_updates", new_channel_json)
                            .await?;

                        // Also subscribe the current client to the new channel using pubsub_publisher_conn
                        let _ = pubsub_publisher_conn
                            .subscribe(format!("chat:{}", new_channel_id))
                            .await;
                    } else {
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: "Usage: /create_channel <id> <name> <icon>".to_string(),
                            timestamp: now_ts,
                            channel_id: channel_info.id.clone(),
                            channel_name: channel_info.name.clone(),
                            channel_icon: channel_info.icon.clone(),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    }
                    handled = true;
                }
                // Delete channel command
                else if content.starts_with("/delete_channel ") {
                    let channel_to_delete_id = content
                        .trim_start_matches("/delete_channel ")
                        .trim()
                        .to_string();

                    // Delete from channel_bans first to satisfy foreign key constraints
                    let _ = sqlx::query("DELETE FROM channel_bans WHERE channel_id = ?")
                        .bind(&channel_to_delete_id)
                        .execute(db_pool)
                        .await?;
                    // Delete the channel itself
                    let _ = sqlx::query("DELETE FROM channels WHERE id = ?")
                        .bind(&channel_to_delete_id)
                        .execute(db_pool)
                        .await?;
                    let reply = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "💻".to_string(),
                        content: format!(
                            "Channel '{}' and its related data deleted.",
                            channel_to_delete_id
                        ),
                        timestamp: now_ts,
                        channel_id: channel_info.id.clone(),
                        channel_name: channel_info.name.clone(),
                        channel_icon: channel_info.icon.clone(),
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    let _ = msg_tx.send(WsMessage::Text(
                        format!("/channel_delete {}", channel_to_delete_id).into(),
                    ));
                    handled = true;
                }
            }

            // Channel admin commands (only if user is admin of this specific channel or super admin)
            // Note: With no creator_username, this logic needs adjustment if you want channel-specific admins.
            // For now, only global admins can use channel_ban/unban.
            if current_user_info.is_super_admin {
                // Only super admins can ban/unban from channels
                // /channel_ban <username> [duration_minutes]
                if content.starts_with("/channel_ban ") {
                    let parts: Vec<&str> = content.splitn(3, ' ').collect();
                    if parts.len() >= 2 {
                        let target_username = parts[1].trim();
                        let duration_minutes: Option<i64> =
                            parts.get(2).and_then(|s| s.parse().ok());
                        let ban_mute_until = duration_minutes.map(|d| now_ts + d * 60).unwrap_or(0); // 0 for permanent

                        let target_user_info = get_user_info(db_pool, target_username).await;
                        if let Ok(info) = target_user_info {
                            if info.is_super_admin {
                                let reply = BroadcastMessage {
                                    user: "server".to_string(),
                                    icon: "🚫".to_string(),
                                    content: format!("Cannot ban a super admin from any channel."),
                                    timestamp: now_ts,
                                    channel_id: channel_info.id.clone(),
                                    channel_name: channel_info.name.clone(),
                                    channel_icon: channel_info.icon.clone(),
                                };
                                let reply_json = serde_json::to_string(&reply)?;
                                let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                                handled = true;
                            } else {
                                let _ = sqlx::query("INSERT OR REPLACE INTO channel_bans (channel_id, banned_username, ban_mute_until) VALUES (?, ?, ?)")
                                    .bind(&channel_info.id)
                                    .bind(target_username)
                                    .bind(ban_mute_until)
                                    .execute(db_pool)
                                    .await?;
                                let reply = BroadcastMessage {
                                    user: "server".to_string(),
                                    icon: "💻".to_string(),
                                    content: format!(
                                        "User '{}' has been banned from channel '{}' {}.",
                                        target_username,
                                        channel_info.name,
                                        if duration_minutes.is_some() {
                                            format!("for {} minutes", duration_minutes.unwrap())
                                        } else {
                                            "permanently".to_string()
                                        }
                                    ),
                                    timestamp: now_ts,
                                    channel_id: channel_info.id.clone(),
                                    channel_name: channel_info.name.clone(),
                                    channel_icon: channel_info.icon.clone(),
                                };
                                let reply_json = serde_json::to_string(&reply)?;
                                let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                                handled = true;
                            }
                        } else {
                            let reply = BroadcastMessage {
                                user: "server".to_string(),
                                icon: "💻".to_string(),
                                content: format!("User '{}' not found.", target_username),
                                timestamp: now_ts,
                                channel_id: channel_info.id.clone(),
                                channel_name: channel_info.name.clone(),
                                channel_icon: channel_info.icon.clone(),
                            };
                            let reply_json = serde_json::to_string(&reply)?;
                            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                            handled = true;
                        }
                    } else {
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: "Usage: /channel_ban <username> [duration_minutes]"
                                .to_string(),
                            timestamp: now_ts,
                            channel_id: channel_info.id.clone(),
                            channel_name: channel_info.name.clone(),
                            channel_icon: channel_info.icon.clone(),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                }
                // /channel_unban <username>
                else if content.starts_with("/channel_unban ") {
                    let target_username = content.trim_start_matches("/channel_unban ").trim();
                    let _ = sqlx::query(
                        "DELETE FROM channel_bans WHERE channel_id = ? AND banned_username = ?",
                    )
                    .bind(&channel_info.id)
                    .bind(target_username)
                    .execute(db_pool)
                    .await?;
                    let reply = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "💻".to_string(),
                        content: format!(
                            "User '{}' has been unbanned from channel '{}'.",
                            target_username, channel_info.name
                        ),
                        timestamp: now_ts,
                        channel_id: channel_info.id.clone(),
                        channel_name: channel_info.name.clone(),
                        channel_icon: channel_info.icon.clone(),
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    handled = true;
                }
            }

            if !handled {
                let reply = BroadcastMessage {
                    user: "server".to_string(),
                    icon: "💻".to_string(),
                    content: format!("Hello, we received your command '{}', unfortunately, we don't have enough information to accept this request, could you give us a picture of yourself holding the command ?", content),
                    timestamp: now_ts,
                    channel_id: channel_info.id.clone(),
                    channel_name: channel_info.name.clone(),
                    channel_icon: channel_info.icon.clone(),
                };
                let reply_json = serde_json::to_string(&reply)?;
                let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
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
        };
        let msg_json = serde_json::to_string(&message)?;
        let _: () = publish_conn
            .rpush(format!("chat_history:{}", channel_info.id), &msg_json)
            .await?;
        let _: () = publish_conn
            .ltrim(format!("chat_history:{}", channel_info.id), -100, -1)
            .await?;
        let _: () = publish_conn
            .publish(format!("chat:{}", channel_info.id), &msg_json)
            .await?;
    }

    // Await tasks before closing connection
    let _ = tokio::join!(ws_send_task, pubsub_task);

    Ok(())
}

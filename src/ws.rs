use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, PasswordHash,
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{decode, Algorithm, DecodingKey, TokenData, Validation};
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::{Pool, Sqlite};
use std::error::Error as StdError;
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
    pub channel_id: Option<String>,
    pub channel_name: Option<String>,
    pub channel_icon: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum ClientWsMessage {
    Text(String),
    ChannelCommand {
        content: String,
        channel_id: String,
        password: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChannelBroadcast {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub is_private: bool,
    pub is_not_allowed: Option<bool>,
    pub admin_username: String,
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
    password_hash: Option<String>, // Hash for private channels
    creator_username: String,      // Admin of this channel
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

async fn get_channel_info(db: &Pool<Sqlite>, channel_id: &str) -> Result<ChannelInfo, sqlx::Error> {
    sqlx::query_as::<_, ChannelInfo>(
        "SELECT id, name, icon, password_hash, creator_username FROM channels WHERE id = ?",
    )
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

// Function to get all channels
async fn get_all_channels(db: &Pool<Sqlite>) -> Result<Vec<ChannelInfo>, sqlx::Error> {
    sqlx::query_as::<_, ChannelInfo>(
        "SELECT id, name, icon, password_hash, creator_username FROM channels",
    )
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

    let user_info = get_user_info(db_pool, &user_username).await?;

    // --- Redis connections ---
    let client = redis::Client::open(redis_url)?;

    // Connection for pubsub
    let pubsub_conn = client.get_async_connection().await?;
    let mut pubsub = pubsub_conn.into_pubsub();

    // Map to keep track of active channel subscriptions for this client

    // Send initial list of all channels to the client
    let all_channels = get_all_channels(db_pool).await?;
    for channel_info in all_channels {
        let is_private = channel_info.password_hash.is_some();
        let user_is_allowed = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM user_channels WHERE user_id = ? AND channel_id = ?)",
        )
        .bind(&user_info.id)
        .bind(&channel_info.id)
        .fetch_one(db_pool)
        .await
        .unwrap_or(false); // Default to false if query fails

        let channel_broadcast = ChannelBroadcast {
            id: channel_info.id.clone(),
            name: channel_info.name,
            icon: channel_info.icon,
            is_private,
            is_not_allowed: if is_private && !user_is_allowed {
                Some(true)
            } else {
                None
            },
            admin_username: channel_info.creator_username,
        };
        let channel_json = serde_json::to_string(&channel_broadcast)?;
        let _ = ws_sender
            .send(WsMessage::Text(
                format!("/channel_update {}", channel_json).into(),
            ))
            .await;
        // Send as a special command to client
    }

    // This async block manages a single channel's pubsub subscription
    // A more advanced approach would have multiple such tasks, or a single task
    // managing multiple subscriptions dynamically. For now, we'll only subscribe
    // when a user successfully "joins" a channel.
    let mut current_channel_id: Option<String> = None; // Track the channel the user is currently "in"

    // Separate connection for history and commands
    let mut cmd_conn = client.get_async_connection().await?;
    let mut publish_conn = client.get_async_connection().await?;

    // --- Forward live Redis and command messages ---
    let ws_send_task = tokio::spawn(async move {
        // ws_sender is moved here, so it's owned by this task.
        // It can now send messages directly to the client.
        while let Some(msg) = msg_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                // Use ws_sender directly
                break;
            }
        }
    });

    while let Some(Ok(ws_msg)) = ws_receiver.next().await {
        let now_ts = Utc::now().timestamp();
        let current_user_info = get_user_info(db_pool, &user_username).await?; // Get fresh user info

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

        // Determine the target channel for the message/command
        let target_channel_id = match &client_msg {
            ClientWsMessage::ChannelCommand { channel_id, .. } => channel_id.clone(),
            _ => {
                // If not a channel command, and no current channel, default or error
                current_channel_id
                    .as_ref()
                    .unwrap_or(&"general".to_string())
                    .clone() // Default to "general" if not in any channel
            }
        };

        let current_channel_info = get_channel_info(db_pool, &target_channel_id).await;
        let _channel_exists = current_channel_info.is_ok();
        let channel_info = current_channel_info.unwrap_or_else(|_| ChannelInfo {
            // Default ChannelInfo if not found (e.g. for creating new)
            id: target_channel_id.clone(),
            name: "Unknown Channel".to_string(),
            icon: "❓".to_string(),
            password_hash: None,
            creator_username: "server".to_string(),
        });

        let is_channel_private = channel_info.password_hash.is_some();
        let user_is_subscribed = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM user_channels WHERE user_id = ? AND channel_id = ?)",
        )
        .bind(&current_user_info.id)
        .bind(&target_channel_id)
        .fetch_one(db_pool)
        .await
        .unwrap_or(false);

        let mut content = match client_msg.clone() {
            // Clone `client_msg` to avoid moving
            ClientWsMessage::Text(t) => t,
            ClientWsMessage::ChannelCommand { content, .. } => content,
        };

        // Handle the /join_channel command
        if content.starts_with("/join_channel ") {
            let parts: Vec<&str> = content.splitn(3, ' ').collect();
            if parts.len() >= 2 {
                let join_channel_id = parts[1].to_string();
                let join_password = parts.get(2).map(|s| s.to_string()); // Optional password

                let target_channel_info = get_channel_info(db_pool, &join_channel_id).await;

                if let Ok(ch_info) = target_channel_info {
                    let mut allow_join = false;
                    if ch_info.password_hash.is_none() {
                        // Public channel
                        allow_join = true;
                    } else if let Some(ref stored_hash) = ch_info.password_hash {
                        // Private channel
                        if let Some(provided_password) = join_password {
                            let parsed_hash = PasswordHash::new(&stored_hash).map_err(
                                |e| -> Box<dyn StdError + Send + Sync + 'static> {
                                    e.to_string().into()
                                },
                            )?;

                            let hasher = Argon2::default();
                            if hasher
                                .verify_password(provided_password.as_bytes(), &parsed_hash)
                                .is_ok()
                            {
                                allow_join = true;
                            }
                        }
                    }

                    if allow_join {
                        // Record user joining the channel
                        sqlx::query("INSERT OR IGNORE INTO user_channels (user_id, channel_id) VALUES (?, ?)")
                            .bind(&current_user_info.id)
                            .bind(&join_channel_id)
                            .execute(db_pool)
                            .await?;

                        // Unsubscribe from previous channel if any
                        if let Some(prev_channel_id) = current_channel_id.take() {
                            // This is a simplified way to handle unsubscription.
                            // In a real app, `pubsub` stream might need to be reset or a dedicated
                            // pubsub task per channel would be better.
                            let _ = pubsub
                                .unsubscribe(format!("chat:{}", prev_channel_id))
                                .await;
                        }

                        // Subscribe to the new channel's Redis topic
                        let _ = pubsub.subscribe(format!("chat:{}", join_channel_id)).await;
                        current_channel_id = Some(join_channel_id.clone());

                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "🚪".to_string(),
                            content: format!(
                                "You have successfully joined channel '{}' (ID: {}).",
                                ch_info.name, join_channel_id
                            ),
                            timestamp: now_ts,
                            channel_id: Some(join_channel_id.clone()),
                            channel_name: Some(ch_info.name.clone()),
                            channel_icon: Some(ch_info.icon.clone()),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));

                        // Send history for the newly joined channel
                        let new_old_messages: Vec<String> = redis::cmd("LRANGE")
                            .arg(format!("chat_history:{}", join_channel_id))
                            .arg(-50)
                            .arg(-1)
                            .query_async(&mut cmd_conn)
                            .await
                            .unwrap_or_default();
                        for msg in new_old_messages {
                            let _ = msg_tx.send(WsMessage::Text(msg.into())); // Send to client's message queue
                        }

                        // Also send an updated channel list to reflect the new joined status
                        let channel_broadcast_update = ChannelBroadcast {
                            id: ch_info.id.clone(),
                            name: ch_info.name,
                            icon: ch_info.icon,
                            is_private: ch_info.password_hash.is_some(),
                            is_not_allowed: None, // Now allowed
                            admin_username: ch_info.creator_username,
                        };
                        let channel_json = serde_json::to_string(&channel_broadcast_update)?;
                        let _ = msg_tx.send(WsMessage::Text(
                            format!("/channel_update {}", channel_json).into(),
                        ));
                    } else {
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "🚫".to_string(),
                            content: format!(
                                "Failed to join channel '{}'. Invalid password or not allowed.",
                                join_channel_id
                            ),
                            timestamp: now_ts,
                            channel_id: Some(join_channel_id.clone()),
                            channel_name: None,
                            channel_icon: None,
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    }
                } else {
                    let reply = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "🚫".to_string(),
                        content: format!("Channel '{}' not found.", join_channel_id),
                        timestamp: now_ts,
                        channel_id: Some(join_channel_id.clone()),
                        channel_name: None,
                        channel_icon: None,
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                }
            } else {
                let reply = BroadcastMessage {
                    user: "server".to_string(),
                    icon: "💻".to_string(),
                    content: "Usage: /join_channel <id> [password]".to_string(),
                    timestamp: now_ts,
                    channel_id: Some(target_channel_id.clone()),
                    channel_name: None,
                    channel_icon: None,
                };
                let reply_json = serde_json::to_string(&reply)?;
                let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
            }
            continue; // Command handled, don't process as regular message
        }

        // If not a join command, enforce channel subscription for private channels
        if is_channel_private && !user_is_subscribed {
            let reply = BroadcastMessage {
                user: "server".to_string(),
                icon: "🚫".to_string(),
                content: format!("You are not allowed to send messages in private channel '{}'. Please join it first.", channel_info.name),
                timestamp: now_ts,
                channel_id: Some(target_channel_id.clone()),
                channel_name: Some(channel_info.name),
                channel_icon: Some(channel_info.icon),
            };
            let reply_json = serde_json::to_string(&reply)?;
            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
            continue;
        }

        // --- Global Ban Check (Super Admins are immune) ---
        if current_user_info.is_banned && !current_user_info.is_super_admin {
            if current_user_info.ban_mute_until.unwrap_or(0) > now_ts {
                let server_msg = BroadcastMessage {
                    user: "server".to_string(),
                    icon: "⛔".to_string(),
                    content: format!("You are currently globally banned and cannot send messages."),
                    timestamp: now_ts,
                    channel_id: Some(target_channel_id.clone()),
                    channel_name: Some(channel_info.name),
                    channel_icon: Some(channel_info.icon),
                };
                let server_json = serde_json::to_string(&server_msg)?;
                let _ = msg_tx.send(WsMessage::Text(server_json.into()));
                continue;
            }
            // If global ban expired, but user is still IsBanned = 1, re-mute for 30 mins
            sqlx::query("UPDATE users SET BanMuteUntil = ? WHERE username = ?")
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
                channel_id: Some(target_channel_id.clone()),
                channel_name: Some(channel_info.name),
                channel_icon: Some(channel_info.icon),
            };
            let server_json = serde_json::to_string(&server_msg)?;
            let _ = msg_tx.send(WsMessage::Text(server_json.into()));
            continue;
        }

        let channel_ban_status =
            get_channel_ban_info(db_pool, &target_channel_id, &user_username).await?;
        if let Some(ban_info) = channel_ban_status {
            if let Some(mute_until) = ban_info.ban_mute_until {
                if mute_until == 0 || mute_until > now_ts {
                    // 0 for permanent ban
                    let server_msg = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "⛔".to_string(),
                        content: format!("You are banned from this channel."),
                        timestamp: now_ts,
                        channel_id: Some(target_channel_id.clone()),
                        channel_name: Some(channel_info.name),
                        channel_icon: Some(channel_info.icon),
                    };
                    let server_json = serde_json::to_string(&server_msg)?;
                    let _ = msg_tx.send(WsMessage::Text(server_json.into()));
                    continue;
                } else {
                    // Ban expired, remove the ban entry
                    sqlx::query(
                        "DELETE FROM channel_bans WHERE channel_id = ? AND banned_username = ?",
                    )
                    .bind(&target_channel_id)
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
                        channel_id: Some(target_channel_id.clone()),
                        channel_name: Some(channel_info.name.clone()),
                        channel_icon: Some(channel_info.icon.clone()),
                    };
                    let server_json = serde_json::to_string(&server_msg)?;
                    let _ = msg_tx.send(WsMessage::Text(server_json.into()));
                }
            }
        }

        if content.starts_with("/") {
            let mut handled = false;

            if current_user_info.is_admin || current_user_info.is_super_admin {
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
                                channel_id: Some(target_channel_id.clone()),
                                channel_name: Some(channel_info.name.clone()),
                                channel_icon: Some(channel_info.icon.clone()),
                            };
                            let reply_json = serde_json::to_string(&reply)?;
                            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                            handled = true;
                            // Do not return, allow other admin commands to be checked if applicable
                        } else {
                            sqlx::query("UPDATE users SET IsBanned = 1 WHERE username = ?")
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
                                channel_id: Some(target_channel_id.clone()),
                                channel_name: Some(channel_info.name.clone()),
                                channel_icon: Some(channel_info.icon.clone()),
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
                            channel_id: Some(target_channel_id.clone()),
                            channel_name: Some(channel_info.name.clone()),
                            channel_icon: Some(channel_info.icon.clone()),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                }
                // Refactored /rm_admin command to check super admin immunity
                else if content.starts_with("/rm_admin ") {
                    let target_username = content.trim_start_matches("/rm_admin ").trim();
                    let target_user_info = get_user_info(db_pool, target_username).await;

                    if let Ok(info) = target_user_info {
                        if info.is_super_admin {
                            let reply = BroadcastMessage {
                                user: "server".to_string(),
                                icon: "🚫".to_string(),
                                content: format!("Cannot remove admin status from a super admin."),
                                timestamp: now_ts,
                                channel_id: Some(target_channel_id.clone()),
                                channel_name: Some(channel_info.name.clone()),
                                channel_icon: Some(channel_info.icon.clone()),
                            };
                            let reply_json = serde_json::to_string(&reply)?;
                            let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                            handled = true;
                        } else {
                            sqlx::query("UPDATE users SET IsAdmin = 0 WHERE username = ?")
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
                                channel_id: Some(target_channel_id.clone()),
                                channel_name: Some(channel_info.name.clone()),
                                channel_icon: Some(channel_info.icon.clone()),
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
                            channel_id: Some(target_channel_id.clone()),
                            channel_name: Some(channel_info.name.clone()),
                            channel_icon: Some(channel_info.icon.clone()),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                }
                // Other global admin commands (meow, stop_meow, etc.)
                else if content.starts_with("/meow_event") {
                    let _: () = publish_conn.set("meow_mode", 1).await?;
                    let _: () = publish_conn.expire("meow_mode", 300).await?; // 5 min
                    let reply = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "🐈".to_string(),
                        content: "Meow mode activated! Every message is now 'meow'.".to_string(),
                        timestamp: now_ts,
                        channel_id: Some(target_channel_id.clone()),
                        channel_name: Some(channel_info.name.clone()),
                        channel_icon: Some(channel_info.icon.clone()),
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
                        channel_id: Some(target_channel_id.clone()),
                        channel_name: Some(channel_info.name.clone()),
                        channel_icon: Some(channel_info.icon.clone()),
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    handled = true;
                }
                // New: Create channel command (global admin or super admin can create any channel)
                else if content.starts_with("/create_channel ") {
                    let parts: Vec<&str> = content.splitn(5, ' ').collect();
                    if parts.len() >= 4 {
                        // /create_channel <id> <name> <icon> [password]
                        let new_channel_id = parts[1].to_string();
                        let new_channel_name = parts[2].to_string();
                        let new_channel_icon = parts[3].to_string();
                        let new_channel_password_hash = if parts.len() == 5 {
                            let password = parts[4];
                            let salt = SaltString::generate(&mut OsRng);
                            let argon2 = Argon2::default();
                            Some(
                                argon2
                                    .hash_password(password.as_bytes(), &salt)
                                    .map_err(|e| -> Box<dyn StdError + Send + Sync + 'static> {
                                        e.to_string().into() // Corrected line
                                    })?
                                    .to_string(),
                            )
                        } else {
                            None
                        };

                        let creator = if current_user_info.is_super_admin {
                            "super_admin".to_string() // Or the actual super admin username
                        } else {
                            user_username.clone()
                        };

                        sqlx::query("INSERT INTO channels (id, name, icon, password_hash, creator_username) VALUES (?, ?, ?, ?, ?)")
                            .bind(&new_channel_id)
                            .bind(&new_channel_name)
                            .bind(&new_channel_icon)
                            .bind(&new_channel_password_hash)
                            .bind(&creator)
                            .execute(db_pool)
                            .await?;

                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!(
                                "Channel '{}' (ID: {}) created {}.",
                                new_channel_name,
                                new_channel_id,
                                if new_channel_password_hash.is_some() {
                                    "as private"
                                } else {
                                    "as public"
                                }
                            ),
                            timestamp: now_ts,
                            channel_id: Some(target_channel_id.clone()),
                            channel_name: Some(channel_info.name.clone()),
                            channel_icon: Some(channel_info.icon.clone()),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));

                        // Broadcast new channel to all connected clients (simplified, in a real app, use Redis pubsub for channel updates)
                        let new_channel_broadcast = ChannelBroadcast {
                            id: new_channel_id.clone(),
                            name: new_channel_name,
                            icon: new_channel_icon,
                            is_private: new_channel_password_hash.is_some(),
                            is_not_allowed: Some(new_channel_password_hash.is_some()), // Initially, user is not allowed if private
                            admin_username: creator,
                        };
                        let new_channel_json = serde_json::to_string(&new_channel_broadcast)?;
                        // This broadcast needs to go to all *currently connected* clients.
                        // You would need a global list of `msg_tx` channels to send this.
                        // For now, let's assume it's sent back to the current client.
                        // A proper solution would involve a global state manager for connections.
                        let _ = msg_tx.send(WsMessage::Text(
                            format!("/channel_update {}", new_channel_json).into(),
                        ));
                    } else {
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: "Usage: /create_channel <id> <name> <icon> [password]"
                                .to_string(),
                            timestamp: now_ts,
                            channel_id: Some(target_channel_id.clone()),
                            channel_name: Some(channel_info.name.clone()),
                            channel_icon: Some(channel_info.icon.clone()),
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
                    sqlx::query("DELETE FROM channel_bans WHERE channel_id = ?")
                        .bind(&channel_to_delete_id)
                        .execute(db_pool)
                        .await?;
                    // Delete from user_channels
                    sqlx::query("DELETE FROM user_channels WHERE channel_id = ?")
                        .bind(&channel_to_delete_id)
                        .execute(db_pool)
                        .await?;
                    // Then delete the channel itself
                    sqlx::query("DELETE FROM channels WHERE id = ?")
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
                        channel_id: Some(target_channel_id.clone()),
                        channel_name: Some(channel_info.name.clone()),
                        channel_icon: Some(channel_info.icon.clone()),
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                    let _ = msg_tx.send(WsMessage::Text(
                        format!("/channel_delete {}", channel_to_delete_id).into(),
                    ));
                    handled = true;
                }
            }

            if channel_info.creator_username == user_username || current_user_info.is_super_admin {
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
                                    channel_id: Some(target_channel_id.clone()),
                                    channel_name: Some(channel_info.name.clone()),
                                    channel_icon: Some(channel_info.icon.clone()),
                                };
                                let reply_json = serde_json::to_string(&reply)?;
                                let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                                handled = true;
                            } else {
                                sqlx::query("INSERT OR REPLACE INTO channel_bans (channel_id, banned_username, ban_mute_until) VALUES (?, ?, ?)")
                                    .bind(&target_channel_id)
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
                                    channel_id: Some(target_channel_id.clone()),
                                    channel_name: Some(channel_info.name.clone()),
                                    channel_icon: Some(channel_info.icon.clone()),
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
                                channel_id: Some(target_channel_id.clone()),
                                channel_name: Some(channel_info.name.clone()),
                                channel_icon: Some(channel_info.icon.clone()),
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
                            channel_id: Some(target_channel_id.clone()),
                            channel_name: Some(channel_info.name.clone()),
                            channel_icon: Some(channel_info.icon.clone()),
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                }
                // /channel_unban <username>
                else if content.starts_with("/channel_unban ") {
                    let target_username = content.trim_start_matches("/channel_unban ").trim();
                    sqlx::query(
                        "DELETE FROM channel_bans WHERE channel_id = ? AND banned_username = ?",
                    )
                    .bind(&target_channel_id)
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
                        channel_id: Some(target_channel_id.clone()),
                        channel_name: Some(channel_info.name.clone()),
                        channel_icon: Some(channel_info.icon.clone()),
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
                    channel_id: Some(target_channel_id.clone()),
                    channel_name: Some(channel_info.name.clone()),
                    channel_icon: Some(channel_info.icon.clone()),
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
            channel_id: Some(target_channel_id.clone()),
            channel_name: Some(channel_info.name.clone()),
            channel_icon: Some(channel_info.icon.clone()),
        };
        let msg_json = serde_json::to_string(&message)?;
        let _: () = publish_conn
            .rpush(format!("chat_history:{}", target_channel_id), &msg_json)
            .await?;
        let _: () = publish_conn
            .ltrim(format!("chat_history:{}", target_channel_id), -100, -1)
            .await?;
        let _: () = publish_conn
            .publish(format!("chat:{}", target_channel_id), &msg_json)
            .await?;
    }

    ws_send_task.await?; // Await the send task before closing

    Ok(())
}

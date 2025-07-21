use log::{info, warn, error, debug};
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
use uuid::Uuid;

// Add rand for dice rolling
use rand::Rng;

use crate::models::Channel; // Assuming Channel struct might also need id as Uuid

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
    pub file_size_mb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_image: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_url: Option<String>,
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
#[derive(sqlx::FromRow, Debug, Clone, Serialize)]
pub struct ChannelProposal {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub proposer_username: String,
    pub status: String, // e.g., "PENDING", "APPROVED", "REJECTED"
    pub timestamp: i64,
}

#[derive(sqlx::FromRow, Debug, Clone)]
#[allow(dead_code)]
struct UserInfo {
    // Change id from String to Vec<u8> to match BLOB in DB
    id: Vec<u8>,
    icon: String,
    is_admin: bool,
    is_super_admin: bool,
    is_banned: bool,
    ban_mute_until: Option<i64>,
    // Add password_hash to UserInfo to retrieve it for verification
    password_hash: String,
}

#[derive(sqlx::FromRow, Debug)]
struct ChannelBanInfo {
    ban_mute_until: Option<i64>,
}

async fn get_user_info(db: &Pool<Sqlite>, username: &str) -> Result<UserInfo, sqlx::Error> {
    // Include password_hash in the SELECT statement
    sqlx::query_as::<_, UserInfo>(
        "SELECT id, icon, IsAdmin as is_admin, IsSuperAdmin as is_super_admin, IsBanned as is_banned, BanMuteUntil as ban_mute_until, password_hash FROM users WHERE username = ?"
    )
    .bind(username)
    .fetch_one(db)
    .await
}

// Function to get a single channel's info using the simplified Channel struct
pub async fn get_channel_info(db: &Pool<Sqlite>, channel_id: &str) -> Result<Channel, sqlx::Error> {
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

// Function to get all APPROVED channels
async fn get_all_approved_channels(db: &Pool<Sqlite>) -> Result<Vec<Channel>, sqlx::Error> {
    sqlx::query_as::<_, Channel>("SELECT id, name, icon FROM channels")
        .fetch_all(db)
        .await
}

// Function to get a single channel proposal info
async fn get_channel_proposal_info(
    db: &Pool<Sqlite>,
    proposal_id: &str,
) -> Result<ChannelProposal, sqlx::Error> {
    sqlx::query_as::<_, ChannelProposal>(
        "SELECT id, name, icon, proposer_username, status, timestamp FROM channel_proposals WHERE id = ?"
    )
    .bind(proposal_id)
    .fetch_one(db)
    .await
}

// Function to get all pending channel proposals
async fn get_all_pending_channel_proposals(
    db: &Pool<Sqlite>,
) -> Result<Vec<ChannelProposal>, sqlx::Error> {
    sqlx::query_as::<_, ChannelProposal>(
        "SELECT id, name, icon, proposer_username, status, timestamp FROM channel_proposals WHERE status = 'PENDING'"
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

    // Authenticate user with JWT
    debug!("Waiting for JWT token from client.");
    let jwt_msg = match ws_receiver.next().await {
        Some(Ok(WsMessage::Text(token))) => {
            info!("Received JWT token from client.");
            token
        },
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
            info!("JWT token decoded successfully for user: {}", data.claims.sub);
            data.claims.sub.clone()
        },
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

    info!("WebSocket authentication successful for user: {}", user_username);

    // Redis connections for pubsub and commands
    debug!("Opening Redis client for pubsub and commands.");
    debug!("Opening Redis client for pubsub and commands.");
    let client = redis::Client::open(redis_url)?;
    // This connection will be moved into the pubsub_task
    let mut pubsub_listener_conn = client.get_async_connection().await?.into_pubsub();
    // This connection will be used for publishing and other commands
    let mut publish_conn = client.get_async_connection().await?;
    // This connection will be used for subscribing to new channels outside the listener task
    let mut pubsub_publisher_conn = client.get_async_connection().await?.into_pubsub();
    info!("Redis connections established.");
    info!("Redis connections established.");

    // Add user to active users set in Redis
    let _: () = publish_conn.sadd("active_users", &user_username).await?;

    // Send initial list of all APPROVED channels to the client upon connection
    let all_channels = get_all_approved_channels(db_pool).await?;
    for channel_info in &all_channels {
        let channel_broadcast = ChannelBroadcast {
            id: channel_info.id.clone(),
            name: channel_info.name.clone(),
            icon: channel_info.icon.clone(),
        };
        let channel_json = serde_json::to_string(&channel_broadcast)?;
        let _ = ws_sender
            .send(WsMessage::Text(
                format!("/channel_update {}", channel_json).into(),
            ))
            .await;

        // Subscribe to all existing channels' Redis topics using pubsub_listener_conn
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
                    message_type: "text".to_string(),
                    file_name: None,
                    file_extension: None,
                    file_size_mb: None,
                    is_image: None,
                    image_preview: None,
                    file_id: None,
                    download_url: None,
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
                    message_type: "text".to_string(),
                    file_name: None,
                    file_extension: None,
                    file_size_mb: None,
                    is_image: None,
                    image_preview: None,
                    file_id: None,
                    download_url: None,
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
                message_type: "text".to_string(),
                file_name: None,
                file_extension: None,
                file_size_mb: None,
                is_image: None,
                image_preview: None,
                file_id: None,
                download_url: None,
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
                        message_type: "text".to_string(),
                        file_name: None,
                        file_extension: None,
                        file_size_mb: None,
                        is_image: None,
                        image_preview: None,
                        file_id: None,
                        download_url: None,
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
                        message_type: "text".to_string(),
                        file_name: None,
                        file_extension: None,
                        file_size_mb: None,
                        is_image: None,
                        image_preview: None,
                        file_id: None,
                        download_url: None,
                    };
                    let server_json = serde_json::to_string(&server_msg)?;
                    let _ = msg_tx.send(WsMessage::Text(server_json.into()));
                }
            }
        }

        // Handle commands
        if content.starts_with("/") {
            let mut handled = false;
            let parts: Vec<&str> = content.splitn(2, ' ').collect();
            let command = parts[0];
            let args = parts.get(1).unwrap_or(&"").trim();

            let msg_tx_clone = msg_tx.clone();
            let send_reply = move |msg: String, icon: String, channel_info: Channel| async move {
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
                    file_size_mb: None,
                    is_image: None,
                    image_preview: None,
                    file_id: None,
                    download_url: None,
                };
                let reply_json = serde_json::to_string(&reply).unwrap_or_else(|e| {
                    eprintln!("Error serializing reply: {:?}", e);
                    "{}".to_string() // Fallback to empty JSON or handle error appropriately
                });
                let _ = msg_tx_clone.send(WsMessage::Text(reply_json.into()));
            };

            match command {
                "/help" => {
                    let help_message = r"Available commands:
- /help: Show this help message.
- /echo <message>: Repeats your message.
- /calc <expression>: Basic calculator (e.g., 5+3, 10/2).
- /roll <NdM>: Roll dice (e.g., 2d6).
- /get_history <channel_id>: Get message history for a channel.
- /propose_channel <name> <icon>: Propose a new channel.
- /get_active_users: Get a list of currently active users.

Admin Commands:
- /ban <username>: Globally ban a user.
- /rm_admin <username>: Remove global admin status.
- /meow_event: Activate meow mode.
- /stop_meow: Deactivate meow mode.
- /delete_channel <id>: Delete a channel.
- /list_proposals: List pending channel proposals.
- /approve_channel <proposal_id>: Approve a channel proposal.
- /reject_channel <proposal_id>: Reject a channel proposal.
- /channel_ban <username> [duration_minutes]: Ban user from current channel.
- /channel_unban <username>: Unban user from current channel.";
                    send_reply(
                        help_message.to_string(),
                        "💡".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                    handled = true;
                }
                "/echo" => {
                    send_reply(args.to_string(), "🗣️".to_string(), channel_info.clone()).await;
                    handled = true;
                }
                "/calc" => {
                    let expression = args;
                    let result = parse_and_calculate(expression);
                    send_reply(
                        format!("Result: {}", result),
                        "🧮".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                    handled = true;
                }
                "/roll" => {
                    let roll_result = parse_and_roll_dice(args);
                    send_reply(
                        format!("Roll: {}", roll_result),
                        "🎲".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                    handled = true;
                }
                "/get_history" => {
                    let parts: Vec<&str> = args.split_whitespace().collect();
                    if let Some(channel_id) = parts.get(0) {
                        let offset: i64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                        let limit: i64 = 50;
                        let start = -(offset + limit);
                        let end = -(offset + 1);

                        let history_key = format!("chat_history:{}", channel_id);
                        let history_json_strings: Vec<String> = publish_conn.lrange(&history_key, start as isize, end as isize).await?;
                        
                        let history_values: Vec<serde_json::Value> = history_json_strings
                            .into_iter()
                            .filter_map(|s| serde_json::from_str(&s).ok())
                            .collect();

                        let response = serde_json::json!({
                            "history": history_values
                        });
                        
                        let _ = msg_tx.send(WsMessage::Text(response.to_string().into()));
                    } else {
                        send_reply(
                            "Usage: /get_history <channel_id> [offset]".to_string(),
                            "🚫".to_string(),
                            channel_info.clone(),
                        )
                        .await;
                    }
                    handled = true;
                }
                "/propose_channel" => {
                    let proposal_parts: Vec<&str> = args.splitn(2, ' ').collect();
                    if proposal_parts.len() == 2 {
                        let new_channel_name = proposal_parts[0].to_string();
                        let new_channel_icon = proposal_parts[1].to_string();
                        let proposal_id = Uuid::new_v4().to_string();

                        let _ = sqlx::query(
                            "INSERT INTO channel_proposals (id, name, icon, proposer_username, status, timestamp) VALUES (?, ?, ?, ?, ?, ?)"
                        )
                        .bind(&proposal_id)
                        .bind(&new_channel_name)
                        .bind(&new_channel_icon)
                        .bind(&user_username)
                        .bind("PENDING")
                        .bind(now_ts)
                        .execute(db_pool)
                        .await?;

                        send_reply(
                            format!("Channel proposal '{}' (ID: {}) submitted for review. Please wait for a super-admin to approve it.", new_channel_name, proposal_id),
                            "📝".to_string(), channel_info.clone()
                        ).await;
                        handled = true;
                    } else {
                        send_reply(
                            "Usage: /propose_channel <name> <icon>".to_string(),
                            "🚫".to_string(),
                            channel_info.clone(),
                        )
                        .await;
                        handled = true;
                    }
                }
                "/get_active_users" => {
                    let active_users: Vec<String> = publish_conn.smembers("active_users").await?;
                    let response = serde_json::json!({
                        "active_users": active_users
                    });
                    let _ = msg_tx.send(WsMessage::Text(response.to_string().into()));
                    handled = true;
                }
                _ => {
                    // Handle admin/super-admin commands
                    if current_user_info.is_admin || current_user_info.is_super_admin {
                        match command {
                            "/ban" => {
                                let target_username = args;
                                let target_user_info =
                                    get_user_info(db_pool, target_username).await;

                                if let Ok(info) = target_user_info {
                                    if info.is_super_admin {
                                        send_reply(
                                            "Cannot globally ban a super admin.".to_string(),
                                            "🚫".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    } else {
                                        let _ = sqlx::query(
                                            "UPDATE users SET IsBanned = 1 WHERE username = ?",
                                        )
                                        .bind(target_username)
                                        .execute(db_pool)
                                        .await?;
                                        send_reply(
                                            format!(
                                                "User '{}' has been GLOBALLY banned by admin '{}'.",
                                                target_username, user_username
                                            ),
                                            "💻".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    }
                                } else {
                                    send_reply(
                                        format!("User '{}' not found.", target_username),
                                        "💻".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                }
                                handled = true;
                            }
                            "/rm_admin" => {
                                let target_username = args;
                                let target_user_info =
                                    get_user_info(db_pool, target_username).await;

                                if let Ok(info) = target_user_info {
                                    if info.is_super_admin {
                                        send_reply(
                                            "Cannot remove admin status from a super admin."
                                                .to_string(),
                                            "🚫".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    } else {
                                        let _ = sqlx::query(
                                            "UPDATE users SET IsAdmin = 0 WHERE username = ?",
                                        )
                                        .bind(target_username)
                                        .execute(db_pool)
                                        .await?;
                                        send_reply(
                                            format!(
                                                "User '{}' is no longer a global admin.",
                                                target_username
                                            ),
                                            "💻".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    }
                                } else {
                                    send_reply(
                                        format!("User '{}' not found.", target_username),
                                        "💻".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                }
                                handled = true;
                            }
                            "/meow_event" => {
                                let _: () = publish_conn.set("meow_mode", 1).await?;
                                let _: () = publish_conn.expire("meow_mode", 300).await?; // 5 min
                                send_reply(
                                    "Meow mode activated! Every message is now 'meow'.".to_string(),
                                    "🐈".to_string(),
                                    channel_info.clone(),
                                )
                                .await;
                                handled = true;
                            }
                            "/stop_meow" => {
                                let _: () = publish_conn.del("meow_mode").await?;
                                send_reply(
                                    "Meow mode deactivated.".to_string(),
                                    "🐈".to_string(),
                                    channel_info.clone(),
                                )
                                .await;
                                handled = true;
                            }
                            "/delete_channel" => {
                                let channel_to_delete_id = args.to_string();
                                // Delete from channel_bans first to satisfy foreign key constraints
                                let _ =
                                    sqlx::query("DELETE FROM channel_bans WHERE channel_id = ?")
                                        .bind(&channel_to_delete_id)
                                        .execute(db_pool)
                                        .await?;
                                // Delete the channel itself
                                let _ = sqlx::query("DELETE FROM channels WHERE id = ?")
                                    .bind(&channel_to_delete_id)
                                    .execute(db_pool)
                                    .await?;
                                send_reply(
                                    format!(
                                        "Channel '{}' and its related data deleted.",
                                        channel_to_delete_id
                                    ),
                                    "💻".to_string(),
                                    channel_info.clone(),
                                )
                                .await;
                                let _ = msg_tx.send(WsMessage::Text(
                                    format!("/channel_delete {}", channel_to_delete_id).into(),
                                ));
                                handled = true;
                            }
                            "/list_proposals" => {
                                if current_user_info.is_super_admin {
                                    let pending_proposals =
                                        get_all_pending_channel_proposals(db_pool).await?;
                                    if pending_proposals.is_empty() {
                                        send_reply(
                                            "No pending channel proposals.".to_string(),
                                            "📝".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    } else {
                                        let mut proposal_list =
                                            "Pending Channel Proposals:\n".to_string();
                                        for proposal in pending_proposals {
                                            proposal_list.push_str(&format!("- ID: {}, Name: '{}', Icon: '{}', Proposer: '{}'\n",
                                                proposal.id, proposal.name, proposal.icon, proposal.proposer_username));
                                        }
                                        send_reply(
                                            proposal_list,
                                            "📝".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    }
                                } else {
                                    send_reply(
                                        "You do not have permission to list proposals.".to_string(),
                                        "🚫".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                }
                                handled = true;
                            }
                            "/approve_channel" => {
                                if current_user_info.is_super_admin {
                                    let proposal_id = args;
                                    let proposal_info =
                                        get_channel_proposal_info(db_pool, proposal_id).await;

                                    if let Ok(proposal) = proposal_info {
                                        if proposal.status == "PENDING" {
                                            // Insert into active channels
                                            let _ = sqlx::query("INSERT INTO channels (id, name, icon) VALUES (?, ?, ?)")
                                                .bind(&proposal.id)
                                                .bind(&proposal.name)
                                                .bind(&proposal.icon)
                                                .execute(db_pool)
                                                .await?;

                                            // Update proposal status
                                            let _ = sqlx::query("UPDATE channel_proposals SET status = 'APPROVED' WHERE id = ?")
                                                .bind(&proposal.id)
                                                .execute(db_pool)
                                                .await?;

                                            send_reply(
                                                format!("Channel '{}' (ID: {}) has been APPROVED and is now active.", proposal.name, proposal.id),
                                                "✅".to_string(), channel_info.clone()
                                            ).await;

                                            // Broadcast new channel to all connected clients
                                            let new_channel_broadcast = ChannelBroadcast {
                                                id: proposal.id.clone(),
                                                name: proposal.name.clone(),
                                                icon: proposal.icon.clone(),
                                            };
                                            let new_channel_json =
                                                serde_json::to_string(&new_channel_broadcast)?;
                                            let _: () = publish_conn
                                                .publish("channel_updates", new_channel_json)
                                                .await?;

                                            // Also subscribe the current client to the new channel
                                            let _ = pubsub_publisher_conn
                                                .subscribe(format!("chat:{}", proposal.id))
                                                .await;
                                        } else {
                                            send_reply(
                                                "This proposal is not in PENDING status."
                                                    .to_string(),
                                                "🚫".to_string(),
                                                channel_info.clone(),
                                            )
                                            .await;
                                        }
                                    } else {
                                        send_reply(
                                            format!(
                                                "Channel proposal '{}' not found.",
                                                proposal_id
                                            ),
                                            "🚫".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    }
                                } else {
                                    send_reply(
                                        "You do not have permission to approve channels."
                                            .to_string(),
                                        "🚫".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                }
                                handled = true;
                            }
                            "/reject_channel" => {
                                if current_user_info.is_super_admin {
                                    let proposal_id = args;
                                    let proposal_info =
                                        get_channel_proposal_info(db_pool, proposal_id).await;

                                    if let Ok(proposal) = proposal_info {
                                        if proposal.status == "PENDING" {
                                            let _ = sqlx::query("UPDATE channel_proposals SET status = 'REJECTED' WHERE id = ?")
                                                .bind(&proposal.id)
                                                .execute(db_pool)
                                                .await?;
                                            send_reply(
                                                format!("Channel proposal '{}' (ID: {}) has been REJECTED.", proposal.name, proposal.id),
                                                "❌".to_string(), channel_info.clone()

                                            ).await;
                                        } else {
                                            send_reply(
                                                "This proposal is not in PENDING status."
                                                    .to_string(),
                                                "🚫".to_string(),
                                                channel_info.clone(),
                                            )
                                            .await;
                                        }
                                    } else {
                                        send_reply(
                                            format!(
                                                "Channel proposal '{}' not found.",
                                                proposal_id
                                            ),
                                            "🚫".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    }
                                } else {
                                    send_reply(
                                        "You do not have permission to reject channels."
                                            .to_string(),
                                        "🚫".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                }
                                handled = true;
                            }
                            // Channel admin commands (only if user is super admin)
                            "/channel_ban" => {
                                if current_user_info.is_super_admin {
                                    let parts: Vec<&str> = args.splitn(2, ' ').collect();
                                    if parts.len() >= 1 {
                                        let target_username = parts[0].trim();
                                        let duration_minutes: Option<i64> =
                                            parts.get(1).and_then(|s| s.parse().ok());
                                        let ban_mute_until =
                                            duration_minutes.map(|d| now_ts + d * 60).unwrap_or(0); // 0 for permanent

                                        let target_user_info =
                                            get_user_info(db_pool, target_username).await;
                                        if let Ok(info) = target_user_info {
                                            if info.is_super_admin {
                                                send_reply(
                                                    "Cannot ban a super admin from any channel."
                                                        .to_string(),
                                                    "🚫".to_string(),
                                                    channel_info.clone(),
                                                )
                                                .await;
                                            } else {
                                                let _ = sqlx::query("INSERT OR REPLACE INTO channel_bans (channel_id, banned_username, ban_mute_until) VALUES (?, ?, ?)")
                                                    .bind(&channel_info.id)
                                                    .bind(target_username)
                                                    .bind(ban_mute_until)
                                                    .execute(db_pool)
                                                    .await?;
                                                send_reply(
                                                    format!(
                                                        "User '{}' has been banned from channel '{}' {}.",
                                                        target_username,
                                                        channel_info.name,
                                                        if duration_minutes.is_some() {
                                                            format!("for {} minutes", duration_minutes.unwrap())
                                                        } else {
                                                            "permanently".to_string()
                                                        }
                                                    ),
                                                    "💻".to_string(), channel_info.clone()
                                                ).await;
                                            }
                                        } else {
                                            send_reply(
                                                format!("User '{}' not found.", target_username),
                                                "💻".to_string(),
                                                channel_info.clone(),
                                            )
                                            .await;
                                        }
                                    } else {
                                        send_reply(
                                            "Usage: /channel_ban <username> [duration_minutes]"
                                                .to_string(),
                                            "🚫".to_string(),
                                            channel_info.clone(),
                                        )
                                        .await;
                                    }
                                } else {
                                    send_reply(
                                        "You do not have permission to ban users from channels."
                                            .to_string(),
                                        "🚫".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                }
                                handled = true;
                            }
                            "/channel_unban" => {
                                if current_user_info.is_super_admin {
                                    let target_username = args;
                                    let _ = sqlx::query(
                                        "DELETE FROM channel_bans WHERE channel_id = ? AND banned_username = ?",
                                    )
                                    .bind(&channel_info.id)
                                    .bind(target_username)
                                    .execute(db_pool)
                                    .await?;
                                    send_reply(
                                        format!(
                                            "User '{}' has been unbanned from channel '{}'.",
                                            target_username, channel_info.name
                                        ),
                                        "💻".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                } else {
                                    send_reply(
                                        "You do not have permission to unban users from channels."
                                            .to_string(),
                                        "🚫".to_string(),
                                        channel_info.clone(),
                                    )
                                    .await;
                                }
                                handled = true;
                            }
                            _ => {} // Fall through if not a recognized admin command
                        }
                    }
                }
            }

            if !handled {
                let reply = BroadcastMessage {
                    user: "server".to_string(),
                    icon: "💻".to_string(),
                    content: format!("Unknown command or insufficient permissions: '{}'. Type /help for a list of commands.", content),
                    timestamp: now_ts,
                    channel_id: channel_info.id.clone(),
                    channel_name: channel_info.name.clone(),
                    channel_icon: channel_info.icon.clone(),
                    message_type: "text".to_string(),
                    file_name: None,
                    file_extension: None,
                    file_size_mb: None,
                    is_image: None,
                    image_preview: None,
                    file_id: None,
                    download_url: None,
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
            message_type: "text".to_string(),
            file_name: None,
            file_extension: None,
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
            .publish(format!("chat:{}", channel_info.id), &msg_json)
            .await?;
    }

    // Await tasks before closing connection
    let _ = tokio::join!(ws_send_task, pubsub_task);

    // Remove user from active users set on disconnect
    let _: () = publish_conn.srem("active_users", &user_username).await?;

    Ok(())
}

// --- Helper Functions for Commands ---

// Very basic calculator for simple expressions like "5+3", "10/2"
fn parse_and_calculate(expression: &str) -> String {
    let ops = ['+', '-', '*', '/'];
    for op in ops.iter() {
        if expression.contains(*op) {
            let parts: Vec<&str> = expression.split(*op).collect();
            if parts.len() == 2 {
                if let (Ok(num1), Ok(num2)) = (
                    parts[0].trim().parse::<f64>(),
                    parts[1].trim().parse::<f64>(),
                ) {
                    return match op {
                        '+' => (num1 + num2).to_string(),
                        '-' => (num1 - num2).to_string(),
                        '*' => (num1 * num2).to_string(),
                        '/' => {
                            if num2 != 0.0 {
                                (num1 / num2).to_string()
                            } else {
                                "Error: Division by zero".to_string()
                            }
                        }
                        _ => "Error: Invalid operator".to_string(),
                    };
                }
            }
        }
    }
    "Error: Invalid expression format. Use 'num1 operator num2' (e.g., 5+3)".to_string()
}

// Dice rolling function (e.g., "2d6")
fn parse_and_roll_dice(input: &str) -> String {
    let lowercased_input = input.to_lowercase();
    let parts: Vec<&str> = lowercased_input.split('d').collect();
    if parts.len() == 2 {
        if let (Ok(num_dice), Ok(num_sides)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            if num_dice == 0 || num_sides == 0 {
                return "Error: Number of dice or sides cannot be zero.".to_string();
            }
            if num_dice > 100 || num_sides > 1000 {
                // Prevent abuse
                return "Error: Too many dice or sides. Max 100d1000.".to_string();
            }

            let mut total_roll = 0;
            let mut rng = rand::rngs::ThreadRng::default();
            let mut individual_rolls = Vec::new();
            for _ in 0..num_dice {
                let roll: u32 = rng.gen_range(1..=num_sides);
                total_roll += roll;
                individual_rolls.push(roll.to_string());
            }
            return format!("{} (rolls: {})", total_roll, individual_rolls.join(", "));
        }
    }
    "Error: Invalid dice format. Use 'NdM' (e.g., 2d6)".to_string()
}

// --- End Helper Functions ---

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

#[derive(Debug, Deserialize)]
struct Claims {
    sub: String, // username
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BroadcastMessage {
    pub user: String,
    pub icon: String,
    pub content: String,
    pub timestamp: i64,
}

#[derive(sqlx::FromRow, Debug)]
struct UserInfo {
    icon: String,
    is_admin: bool,
    is_banned: bool,
    ban_mute_until: Option<i64>,
}

async fn get_user_info(db: &Pool<Sqlite>, username: &str) -> Result<UserInfo, sqlx::Error> {
    sqlx::query_as::<_, UserInfo>(
        "SELECT icon, IsAdmin as is_admin, IsBanned as is_banned, BanMuteUntil as ban_mute_until FROM users WHERE username = ?"
    )
    .bind(username)
    .fetch_one(db)
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
) -> Result<(), Box<dyn std::error::Error>> {
    let ws_stream = accept_async(stream).await?;
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel::<WsMessage>();

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

    // --- Redis connections ---
    let client = redis::Client::open(redis_url)?;

    // Connection for pubsub
    let pubsub_conn = client.get_async_connection().await?;
    let mut pubsub = pubsub_conn.into_pubsub();
    pubsub.subscribe("chat").await?;

    // Separate connection for history and commands
    let mut cmd_conn = client.get_async_connection().await?;

    // Send last 50 messages as history
    let old_messages: Vec<String> = redis::cmd("LRANGE")
        .arg("chat_history")
        .arg(-50)
        .arg(-1)
        .query_async(&mut cmd_conn)
        .await
        .unwrap_or_default();

    for msg in old_messages {
        ws_sender.send(WsMessage::Text(msg.into())).await?;
    }

    // --- Forward live Redis and command messages ---
    let ws_send_task = tokio::spawn(async move {
        let mut pubsub_stream = pubsub.on_message();
        loop {
            tokio::select! {
                Some(msg) = pubsub_stream.next() => {
                    let payload: String = msg.get_payload().unwrap_or_default();
                    if ws_sender.send(WsMessage::Text(payload.into())).await.is_err() {
                        break;
                    }
                }
                Some(msg) = msg_rx.recv() => {
                    if ws_sender.send(msg).await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    // --- Listen for incoming client messages, process admin/cms/ban/meow logic ---
    let redis_client = redis::Client::open(redis_url)?;
    let mut publish_conn = redis_client.get_async_connection().await?;
    while let Some(Ok(msg)) = ws_receiver.next().await {
        if let WsMessage::Text(text) = msg {
            let user_info = get_user_info(db_pool, &user).await?;
            let now_ts = Utc::now().timestamp();

            // --- Banned user logic ---
            if user_info.is_banned {
                if user_info.ban_mute_until.unwrap_or(0) > now_ts {
                    continue;
                }
                let new_mute_until = now_ts + 30 * 60;
                sqlx::query("UPDATE users SET BanMuteUntil = ? WHERE username = ?")
                    .bind(new_mute_until)
                    .bind(&user)
                    .execute(db_pool)
                    .await?;
                let server_msg = BroadcastMessage {
                    user: "server".to_string(),
                    icon: "⛔".to_string(),
                    content: format!("User '{}' (banned) tried to send a message.", user),
                    timestamp: now_ts,
                };
                let server_json = serde_json::to_string(&server_msg)?;
                let _ = msg_tx.send(WsMessage::Text(server_json.into()));
                continue;
            }

            // --- Command handling (admin/normal/cms) ---
            if text.starts_with("/") {
                let mut handled = false;
                if user_info.is_admin {
                    // /ban username
                    if text.starts_with("/ban ") {
                        let target = text.trim_start_matches("/ban ").trim();
                        sqlx::query("UPDATE users SET IsBanned = 1 WHERE username = ?")
                            .bind(target)
                            .execute(db_pool)
                            .await?;
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!(
                                "User '{}' has been banned by admin '{}'.",
                                target, user
                            ),
                            timestamp: now_ts,
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                    // /unban username
                    if text.starts_with("/unban ") {
                        let target = text.trim_start_matches("/unban ").trim();
                        sqlx::query(
                            "UPDATE users SET IsBanned = 0, BanMuteUntil = NULL WHERE username = ?",
                        )
                        .bind(target)
                        .execute(db_pool)
                        .await?;
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!(
                                "User '{}' has been unbanned by admin '{}'.",
                                target, user
                            ),
                            timestamp: now_ts,
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                    // /make_admin username
                    if text.starts_with("/make_admin ") {
                        let target = text.trim_start_matches("/make_admin ").trim();
                        sqlx::query("UPDATE users SET IsAdmin = 1 WHERE username = ?")
                            .bind(target)
                            .execute(db_pool)
                            .await?;
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!("User '{}' is now an admin.", target),
                            timestamp: now_ts,
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                    // /rm_admin username
                    if text.starts_with("/rm_admin ") {
                        let target = text.trim_start_matches("/rm_admin ").trim();
                        sqlx::query("UPDATE users SET IsAdmin = 0 WHERE username = ?")
                            .bind(target)
                            .execute(db_pool)
                            .await?;
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "💻".to_string(),
                            content: format!("User '{}' is no longer an admin.", target),
                            timestamp: now_ts,
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                    // /meow_event
                    if text.starts_with("/meow_event") {
                        let _: () = publish_conn.set("meow_mode", 1).await?;
                        let _: () = publish_conn.expire("meow_mode", 300).await?; // 5 min
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "🐈".to_string(),
                            content: "Meow mode activated! Every message is now 'meow'."
                                .to_string(),
                            timestamp: now_ts,
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                    // /stop_meow
                    if text.starts_with("/stop_meow") {
                        let _: () = publish_conn.del("meow_mode").await?;
                        let reply = BroadcastMessage {
                            user: "server".to_string(),
                            icon: "🐈".to_string(),
                            content: "Meow mode deactivated.".to_string(),
                            timestamp: now_ts,
                        };
                        let reply_json = serde_json::to_string(&reply)?;
                        let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                        handled = true;
                    }
                }
                if !handled {
                    // Normal user command fallback
                    let reply = BroadcastMessage {
                        user: "server".to_string(),
                        icon: "💻".to_string(),
                        content: format!("Hello, we received ur command '{}', unfortunately, we don't have enough information to accept this request, could u give us a picture of urself holding the command ?", text),
                        timestamp: now_ts,
                    };
                    let reply_json = serde_json::to_string(&reply)?;
                    let _ = msg_tx.send(WsMessage::Text(reply_json.into()));
                }
                continue;
            }

            // --- Meow mode logic ---
            let meow_mode: i64 = publish_conn.get("meow_mode").await.unwrap_or(0);
            let mut content = text.to_string();
            if meow_mode == 1 {
                content = "meow".to_string();
            }

            // --- Normal message processing ---
            let message = BroadcastMessage {
                user: user.clone(),
                icon: user_info.icon,
                content,
                timestamp: now_ts,
            };
            let msg_json = serde_json::to_string(&message)?;
            let _: () = publish_conn.rpush("chat_history", &msg_json).await?;
            let _: () = publish_conn.ltrim("chat_history", -100, -1).await?;
            let _: () = publish_conn.publish("chat", &msg_json).await?;
        }
    }

    ws_send_task.await?;

    Ok(())
}

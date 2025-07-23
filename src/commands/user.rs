use crate::models::Channel;
use crate::ws::BroadcastMessage;

use crate::utils;
use chrono::Utc;
use redis::AsyncCommands;
use sqlx::{Pool, Sqlite};
use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;
use uuid::Uuid;

pub async fn handle_user_command(
    command: &str,
    args: &str,
    db_pool: &Pool<Sqlite>,
    publish_conn: &mut redis::aio::Connection,
    msg_tx: &UnboundedSender<WsMessage>,
    channel_info: &Channel,
    user_username: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync + 'static>> {
    let now_ts = Utc::now().timestamp();
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
            Ok(true)
        }
        "/echo" => {
            send_reply(args.to_string(), "🗣️".to_string(), channel_info.clone()).await;
            Ok(true)
        }
        "/calc" => {
            let expression = args;
            let result = utils::parse_and_calculate(expression);
            send_reply(
                format!("Result: {}", result),
                "🧮".to_string(),
                channel_info.clone(),
            )
            .await;
            Ok(true)
        }
        "/roll" => {
            let roll_result = utils::parse_and_roll_dice(args);
            send_reply(
                format!("Roll: {}", roll_result),
                "🎲".to_string(),
                channel_info.clone(),
            )
            .await;
            Ok(true)
        }
        "/get_history" => {
            let parts: Vec<&str> = args.split_whitespace().collect();
            if let Some(channel_id) = parts.get(0) {
                let offset: usize = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let limit: usize = 50;
                let start = -(offset as isize + limit as isize);
                let end = -(offset as isize + 1);

                let history_key = format!("chat_history:{}", channel_id);
                let history_json_strings: Vec<String> =
                    publish_conn.lrange(&history_key, start, end).await?;

                let history_messages: Vec<BroadcastMessage> = history_json_strings
                    .into_iter()
                    .filter_map(|s| serde_json::from_str(&s).ok())
                    .collect();

                let total_history: isize = publish_conn.llen(&history_key).await?;
                let has_more = total_history > (offset + limit) as isize;
                let new_offset = offset + limit;

                let response = serde_json::json!({
                    "History": {
                        "channel_id": channel_id.to_string(),
                        "messages": history_messages,
                        "offset": new_offset,
                        "has_more": has_more
                    }
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
            Ok(true)
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
            } else {
                send_reply(
                    "Usage: /propose_channel <name> <icon>".to_string(),
                    "🚫".to_string(),
                    channel_info.clone(),
                )
                .await;
            }
            Ok(true)
        }
        "/get_active_users" => {
            let active_users: Vec<String> = publish_conn.smembers("active_users").await?;
            let response = serde_json::json!({
                "active_users": active_users
            });
            let _ = msg_tx.send(WsMessage::Text(response.to_string().into()));
            Ok(true)
        }
        _ => Ok(false),
    }
}

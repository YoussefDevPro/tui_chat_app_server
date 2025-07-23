use crate::data_access;
use crate::models::{Channel, User};
use crate::ws::BroadcastMessage;
use chrono::Utc;
use redis::AsyncCommands;
use sqlx::{Pool, Sqlite};
use tokio::sync::mpsc::UnboundedSender;
use tungstenite::protocol::Message as WsMessage;

pub async fn handle_admin_command(
    command: &str,
    args: &str,
    db_pool: &Pool<Sqlite>,
    publish_conn: &mut redis::aio::Connection,
    msg_tx: &UnboundedSender<WsMessage>,
    channel_info: &Channel,
    user_username: &str,
    current_user_info: &User,
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
        "/ban" => {
            let target_username = args;
            let target_user_info = data_access::get_user_info(db_pool, target_username).await;
            if let Ok(info) = target_user_info {
                if info.is_super_admin {
                    send_reply(
                        "Cannot globally ban a super admin.".to_string(),
                        "🚫".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                } else {
                    let _ = sqlx::query("UPDATE users SET IsBanned = 1 WHERE username = ?")
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
            Ok(true)
        }
        "/rm_admin" => {
            let target_username = args;
            let target_user_info = data_access::get_user_info(db_pool, target_username).await;
            if let Ok(info) = target_user_info {
                if info.is_super_admin {
                    send_reply(
                        "Cannot remove admin status from a super admin.".to_string(),
                        "🚫".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                } else {
                    let _ = sqlx::query("UPDATE users SET IsAdmin = 0 WHERE username = ?")
                        .bind(target_username)
                        .execute(db_pool)
                        .await?;
                    send_reply(
                        format!("User '{}' is no longer a global admin.", target_username),
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
            Ok(true)
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
            Ok(true)
        }
        "/stop_meow" => {
            let _: () = publish_conn.del("meow_mode").await?;
            send_reply(
                "Meow mode deactivated.".to_string(),
                "🐈".to_string(),
                channel_info.clone(),
            )
            .await;
            Ok(true)
        }
        "/delete_channel" => {
            let channel_to_delete_id = args.to_string();
            let _ = sqlx::query("DELETE FROM channel_bans WHERE channel_id = ?")
                .bind(&channel_to_delete_id)
                .execute(db_pool)
                .await?;
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
            Ok(true)
        }
        "/list_proposals" => {
            if current_user_info.is_super_admin {
                let pending_proposals =
                    data_access::get_all_pending_channel_proposals(db_pool).await?;
                if pending_proposals.is_empty() {
                    send_reply(
                        "No pending channel proposals.".to_string(),
                        "📝".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                } else {
                    let mut proposal_list = "Pending Channel Proposals:\n".to_string();
                    for proposal in pending_proposals {
                        proposal_list.push_str(&format!(
                            "- ID: {}, Name: '{}', Icon: '{}', Proposer: '{}'\n",
                            proposal.id, proposal.name, proposal.icon, proposal.proposer_username
                        ));
                    }
                    send_reply(proposal_list, "📝".to_string(), channel_info.clone()).await;
                }
            } else {
                send_reply(
                    "You do not have permission to list proposals.".to_string(),
                    "🚫".to_string(),
                    channel_info.clone(),
                )
                .await;
            }
            Ok(true)
        }
        "/approve_channel" => {
            if current_user_info.is_super_admin {
                let proposal_id = args;
                let proposal_info =
                    data_access::get_channel_proposal_info(db_pool, proposal_id).await;
                if let Ok(proposal) = proposal_info {
                    if proposal.status == "PENDING" {
                        let _ =
                            sqlx::query("INSERT INTO channels (id, name, icon) VALUES (?, ?, ?)")
                                .bind(&proposal.id)
                                .bind(&proposal.name)
                                .bind(&proposal.icon)
                                .execute(db_pool)
                                .await?;
                        let _ = sqlx::query(
                            "UPDATE channel_proposals SET status = 'APPROVED' WHERE id = ?",
                        )
                        .bind(&proposal.id)
                        .execute(db_pool)
                        .await?;
                        send_reply(
                            format!(
                                "Channel '{}' (ID: {}) has been APPROVED and is now active.",
                                proposal.name, proposal.id
                            ),
                            "✅".to_string(),
                            channel_info.clone(),
                        )
                        .await;
                        let new_channel_broadcast = crate::ws::ChannelBroadcast {
                            id: proposal.id.clone(),
                            name: proposal.name.clone(),
                            icon: proposal.icon.clone(),
                        };
                        let new_channel_json = serde_json::to_string(&new_channel_broadcast)?;
                        let _: () = publish_conn
                            .publish("channel_updates", new_channel_json)
                            .await?;
                    } else {
                        send_reply(
                            "This proposal is not in PENDING status.".to_string(),
                            "🚫".to_string(),
                            channel_info.clone(),
                        )
                        .await;
                    }
                } else {
                    send_reply(
                        format!("Channel proposal '{}' not found.", proposal_id),
                        "🚫".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                }
            } else {
                send_reply(
                    "You do not have permission to approve channels.".to_string(),
                    "🚫".to_string(),
                    channel_info.clone(),
                )
                .await;
            }
            Ok(true)
        }
        "/reject_channel" => {
            if current_user_info.is_super_admin {
                let proposal_id = args;
                let proposal_info =
                    data_access::get_channel_proposal_info(db_pool, proposal_id).await;
                if let Ok(proposal) = proposal_info {
                    if proposal.status == "PENDING" {
                        let _ = sqlx::query(
                            "UPDATE channel_proposals SET status = 'REJECTED' WHERE id = ?",
                        )
                        .bind(&proposal.id)
                        .execute(db_pool)
                        .await?;
                        send_reply(
                            format!(
                                "Channel proposal '{}' (ID: {}) has been REJECTED.",
                                proposal.name, proposal.id
                            ),
                            "❌".to_string(),
                            channel_info.clone(),
                        )
                        .await;
                    } else {
                        send_reply(
                            "This proposal is not in PENDING status.".to_string(),
                            "🚫".to_string(),
                            channel_info.clone(),
                        )
                        .await;
                    }
                } else {
                    send_reply(
                        format!("Channel proposal '{}' not found.", proposal_id),
                        "🚫".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                }
            } else {
                send_reply(
                    "You do not have permission to reject channels.".to_string(),
                    "🚫".to_string(),
                    channel_info.clone(),
                )
                .await;
            }
            Ok(true)
        }
        "/channel_ban" => {
            if current_user_info.is_super_admin {
                let parts: Vec<&str> = args.splitn(2, ' ').collect();
                if parts.len() >= 1 {
                    let target_username = parts[0].trim();
                    let duration_minutes: Option<i64> = parts.get(1).and_then(|s| s.parse().ok());
                    let ban_mute_until = duration_minutes.map(|d| now_ts + d * 60).unwrap_or(0); // 0 for permanent
                    let target_user_info =
                        data_access::get_user_info(db_pool, target_username).await;
                    if let Ok(info) = target_user_info {
                        if info.is_super_admin {
                            send_reply(
                                "Cannot ban a super admin from any channel.".to_string(),
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
                } else {
                    send_reply(
                        "Usage: /channel_ban <username> [duration_minutes]".to_string(),
                        "🚫".to_string(),
                        channel_info.clone(),
                    )
                    .await;
                }
            } else {
                send_reply(
                    "You do not have permission to ban users from channels.".to_string(),
                    "🚫".to_string(),
                    channel_info.clone(),
                )
                .await;
            }
            Ok(true)
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
                    "You do not have permission to unban users from channels.".to_string(),
                    "🚫".to_string(),
                    channel_info.clone(),
                )
                .await;
            }
            Ok(true)
        } // <<< FIX: Added missing comma here
        _ => Ok(false),
    }
}

use crate::{
    db::DbPool,
    models::{ActionResponse, Message, User},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize)]
struct SendMessagePayload {
    sender_id: String,
    content: String,
}

#[derive(Deserialize)]
struct GetMessagesPayload {
    limit: usize,
}

pub async fn send_message(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let SendMessagePayload { sender_id, content } = serde_json::from_value(payload)?;
    if content.len() == 0 || content.len() > 2048 {
        return Ok(ActionResponse::error("Invalid message length"));
    }
    let message_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();

    sqlx::query("INSERT INTO messages (id, sender_id, content, created_at) VALUES (?, ?, ?, ?)")
        .bind(&message_id)
        .bind(&sender_id)
        .bind(&content)
        .bind(&created_at)
        .execute(&db)
        .await?;

    Ok(ActionResponse::ok(serde_json::json!({
        "message_id": message_id,
        "created_at": created_at
    })))
}

pub async fn get_messages(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let GetMessagesPayload { limit } = serde_json::from_value(payload)?;
    let rows =
        sqlx::query_as::<_, Message>("SELECT * FROM messages ORDER BY created_at DESC LIMIT ?")
            .bind(limit as i64)
            .fetch_all(&db)
            .await?;

    // Fetch usernames/icons for each message
    let mut messages = Vec::new();
    for msg in rows.into_iter().rev() {
        let sender: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = ?")
            .bind(&msg.sender_id)
            .fetch_optional(&db)
            .await?;
        messages.push(serde_json::json!({
            "content": msg.content,
            "sender": sender.as_ref().map(|u| u.username.clone()).unwrap_or("??".to_string()),
            "icon": sender.as_ref().map(|u| u.icon.clone()).unwrap_or("".to_string()),
            "created_at": msg.created_at,
        }));
    }
    Ok(ActionResponse::ok(messages))
}

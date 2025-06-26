use crate::auth::AuthUser;
use crate::models::Message;
use chrono::Utc;
use redis::AsyncCommands;
use rocket::info;
use rocket::{http::Status, post, serde::json::Json, State};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};

#[derive(serde::Deserialize)]
pub struct MessageInput {
    pub content: String,
}

// Message broadcaster for WebSocket clients
pub type Broadcaster = broadcast::Sender<String>;
pub type BroadcasterHandle = Arc<Mutex<Broadcaster>>;

/// Attach this in your Rocket state initialization.
/// Example:
/// let (broadcaster, _) = broadcast::channel(100);
/// rocket.manage(Arc::new(Mutex::new(broadcaster)));

#[post("/send", data = "<input>")]
pub async fn send_message(
    user: AuthUser,
    db: &State<SqlitePool>,
    redis_url: &State<String>,
    broadcaster: &State<BroadcasterHandle>,
    input: Json<MessageInput>,
) -> Result<Status, Status> {
    let content = input.content.trim();

    // 1. Validation
    if content.is_empty() {
        info!("Rejected empty message from user: {}", user.0);
        return Err(Status::BadRequest);
    }
    if content.len() > 1000 {
        info!(
            "Rejected long message from user: {} ({} chars)",
            user.0,
            content.len()
        );
        return Err(Status::BadRequest);
    }

    // 2. Check user in database for extra security
    let db_user: Option<(String,)> =
        match sqlx::query_as("SELECT username FROM users WHERE username = ?")
            .bind(&user.0)
            .fetch_optional(db.inner())
            .await
        {
            Ok(result) => result,
            Err(e) => {
                info!("DB error while checking user existence: {:?}", e);
                return Err(Status::InternalServerError);
            }
        };
    if db_user.is_none() {
        info!("User from token not found in DB: {}", user.0);
        return Err(Status::Unauthorized);
    }

    // 3. Optional: Basic profanity filtering (simple example)
    let profanities = ["badword", "swear"]; // put your own list here
    for word in profanities.iter() {
        if content.to_lowercase().contains(word) {
            info!("Profanity detected from user: {}", user.0);
            return Err(Status::BadRequest);
        }
    }

    // 4. Optional: Basic rate limiting (per user in-memory, example)
    // If you want to implement rate limiting, use a shared HashMap<username, last_message_time>
    // and refuse if the user has sent too many messages in a short time.

    // 5. Construct and serialize message
    let msg = Message {
        user: user.0.clone(),
        content: content.to_owned(),
        timestamp: Utc::now().timestamp(),
    };
    let msg_json = match serde_json::to_string(&msg) {
        Ok(j) => j,
        Err(e) => {
            info!("Serialization error: {:?}", e);
            return Err(Status::InternalServerError);
        }
    };

    // 6. Publish to Redis
    let client = match redis::Client::open(redis_url.inner().as_str()) {
        Ok(c) => c,
        Err(e) => {
            info!("Redis client error: {:?}", e);
            return Err(Status::InternalServerError);
        }
    };
    let mut conn = match client.get_async_connection().await {
        Ok(c) => c,
        Err(e) => {
            info!("Redis connection error: {:?}", e);
            return Err(Status::InternalServerError);
        }
    };
    if let Err(e) = conn.publish::<_, _, ()>("chat", msg_json.clone()).await {
        info!("Redis publish error: {:?}", e);
        return Err(Status::InternalServerError);
    }

    // 7. Broadcast to all connected WebSocket clients
    {
        let broadcaster = broadcaster.lock().await;
        let _ = broadcaster.send(msg_json); // ignore errors (e.g. no clients)
    }

    Ok(Status::Ok)
}

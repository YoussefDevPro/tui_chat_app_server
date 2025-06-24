use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Deserialize)]
pub struct ActionRequest {
    pub action: String,
    pub payload: Value,
}

#[derive(Serialize)]
pub struct ActionResponse {
    pub success: bool,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
}

impl ActionResponse {
    pub fn ok<T: Serialize>(data: T) -> Self {
        ActionResponse {
            success: true,
            data: Some(serde_json::to_value(data).unwrap()),
            error: None,
        }
    }
    pub fn error(msg: &str) -> Self {
        ActionResponse {
            success: false,
            data: None,
            error: Some(msg.to_string()),
        }
    }
}

#[derive(sqlx::FromRow, Serialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub password_hash: String,
    pub icon: String,
    pub role: String,
    pub created_at: String,
}

#[derive(sqlx::FromRow, Serialize)]
pub struct Message {
    pub id: String,
    pub sender_id: String,
    pub content: String,
    pub created_at: String,
}

#[derive(sqlx::FromRow, Serialize)]
pub struct BannedUser {
    pub id: String,
    pub admin_id: String,
    pub target_id: String,
    pub reason: String,
    pub banned_at: String,
}

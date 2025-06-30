use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub icon: String,
    pub password_hash: String,
}

#[derive(Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub exp: usize,
}

#[derive(Serialize, Deserialize)]
pub struct Message {
    pub user: String,
    pub icon: String,
    pub content: String,
    pub timestamp: i64,
    pub channel_id: String,
}

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub icon: String,
}

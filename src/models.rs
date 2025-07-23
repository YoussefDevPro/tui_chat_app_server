use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(sqlx::FromRow, Serialize, Deserialize, Debug, Clone)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub icon: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub is_super_admin: bool,
    pub is_banned: bool,
    pub ban_mute_until: Option<i64>,
}

#[derive(sqlx::FromRow, Debug, Clone)]
pub struct ChannelBanInfo {
    pub ban_mute_until: Option<i64>,
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

#[derive(sqlx::FromRow, Debug, Clone, Serialize)]
pub struct ChannelProposal {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub proposer_username: String,
    pub status: String,
    pub timestamp: i64,
}

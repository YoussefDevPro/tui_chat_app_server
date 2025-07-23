use sqlx::{Pool, Sqlite};
use crate::models::{Channel, ChannelProposal, User, ChannelBanInfo};

pub async fn get_user_info(db: &Pool<Sqlite>, username: &str) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "SELECT id, username, icon, password_hash, IsAdmin as is_admin, IsSuperAdmin as is_super_admin, IsBanned as is_banned, BanMuteUntil as ban_mute_until FROM users WHERE username = ?"
    )
    .bind(username)
    .fetch_one(db)
    .await
}

pub async fn get_channel_info(db: &Pool<Sqlite>, channel_id: &str) -> Result<Channel, sqlx::Error> {
    sqlx::query_as::<_, Channel>("SELECT id, name, icon FROM channels WHERE id = ?")
        .bind(channel_id)
        .fetch_one(db)
        .await
}

pub async fn get_channel_ban_info(
    db: &Pool<Sqlite>,
    channel_id: &str,
    username: &str,
) -> Result<Option<ChannelBanInfo>, sqlx::Error> {
    sqlx::query_as::<_, ChannelBanInfo>(
        "SELECT ban_mute_until FROM channel_bans WHERE channel_id = ? AND banned_username = ?"
    )
    .bind(channel_id)
    .bind(username)
    .fetch_optional(db)
    .await
}

pub async fn get_all_approved_channels(db: &Pool<Sqlite>) -> Result<Vec<Channel>, sqlx::Error> {
    sqlx::query_as::<_, Channel>("SELECT id, name, icon FROM channels")
        .fetch_all(db)
        .await
}

pub async fn get_channel_proposal_info(
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

pub async fn get_all_pending_channel_proposals(
    db: &Pool<Sqlite>,
) -> Result<Vec<ChannelProposal>, sqlx::Error> {
    sqlx::query_as::<_, ChannelProposal>(
        "SELECT id, name, icon, proposer_username, status, timestamp FROM channel_proposals WHERE status = 'PENDING'"
    )
    .fetch_all(db)
    .await
}


use crate::{
    db::DbPool,
    models::{ActionResponse, User},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize)]
struct BanUserPayload {
    admin_id: String,
    target_id: String,
    reason: String,
}
#[derive(Deserialize)]
struct UnbanUserPayload {
    admin_id: String,
    target_id: String,
}
#[derive(Deserialize)]
struct PromoteUserPayload {
    admin_id: String,
    target_id: String,
}
#[derive(Deserialize)]
struct ChangeUsernamePayload {
    user_id: String,
    new_username: String,
}
#[derive(Deserialize)]
struct ChangeIconPayload {
    user_id: String,
    new_icon: String,
}

async fn is_admin(db: &DbPool, user_id: &str) -> anyhow::Result<bool> {
    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_optional(db)
        .await?;
    Ok(user.map(|u| u.role == "admin").unwrap_or(false))
}

pub async fn ban_user(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let BanUserPayload {
        admin_id,
        target_id,
        reason,
    } = serde_json::from_value(payload)?;
    if !is_admin(&db, &admin_id).await? {
        return Ok(ActionResponse::error("Admin only"));
    }
    let ban_id = Uuid::new_v4().to_string();
    let banned_at = Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO banned_users (id, admin_id, target_id, reason, banned_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&ban_id)
        .bind(&admin_id)
        .bind(&target_id)
        .bind(&reason)
        .bind(&banned_at)
        .execute(&db)
        .await?;
    // Optionally: update user's role or status
    Ok(ActionResponse::ok(
        serde_json::json!({ "banned": true, "banned_at": banned_at }),
    ))
}

pub async fn unban_user(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let UnbanUserPayload {
        admin_id,
        target_id,
    } = serde_json::from_value(payload)?;
    if !is_admin(&db, &admin_id).await? {
        return Ok(ActionResponse::error("Admin only"));
    }
    sqlx::query("DELETE FROM banned_users WHERE target_id = ?")
        .bind(&target_id)
        .execute(&db)
        .await?;
    Ok(ActionResponse::ok(serde_json::json!({ "unbanned": true })))
}

pub async fn promote_user(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let PromoteUserPayload {
        admin_id,
        target_id,
    } = serde_json::from_value(payload)?;
    if !is_admin(&db, &admin_id).await? {
        return Ok(ActionResponse::error("Admin only"));
    }
    sqlx::query("UPDATE users SET role = 'admin' WHERE id = ?")
        .bind(&target_id)
        .execute(&db)
        .await?;
    Ok(ActionResponse::ok(serde_json::json!({ "promoted": true })))
}

pub async fn change_username(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let ChangeUsernamePayload {
        user_id,
        new_username,
    } = serde_json::from_value(payload)?;
    if new_username.len() < 3 || new_username.len() > 32 {
        return Ok(ActionResponse::error("Username length must be 3-32 chars"));
    }
    let res = sqlx::query("UPDATE users SET username = ? WHERE id = ?")
        .bind(&new_username)
        .bind(&user_id)
        .execute(&db)
        .await;
    match res {
        Ok(_) => Ok(ActionResponse::ok(
            serde_json::json!({ "username_changed": true }),
        )),
        Err(e) => {
            if e.to_string().contains("UNIQUE constraint failed") {
                Ok(ActionResponse::error("Username already exists"))
            } else {
                Ok(ActionResponse::error(&format!("DB Error: {e}")))
            }
        }
    }
}

pub async fn change_icon(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let ChangeIconPayload { user_id, new_icon } = serde_json::from_value(payload)?;
    if new_icon.len() > 6 {
        return Ok(ActionResponse::error("Icon too long"));
    }
    sqlx::query("UPDATE users SET icon = ? WHERE id = ?")
        .bind(&new_icon)
        .bind(&user_id)
        .execute(&db)
        .await?;
    Ok(ActionResponse::ok(
        serde_json::json!({ "icon_changed": true }),
    ))
}

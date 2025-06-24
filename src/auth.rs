use crate::{
    db::DbPool,
    models::{ActionResponse, User},
};
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, SaltString};
use argon2::{Argon2, PasswordHasher, PasswordVerifier};
use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

#[derive(Deserialize)]
struct RegisterPayload {
    username: String,
    password: String,
    icon: String,
}

#[derive(Deserialize)]
struct LoginPayload {
    username: String,
    password: String,
}

pub async fn register(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let RegisterPayload {
        username,
        password,
        icon,
    } = serde_json::from_value(payload)?;
    if username.len() < 3 || username.len() > 32 {
        return Ok(ActionResponse::error("Username length must be 3-32 chars"));
    }
    if icon.len() > 6 {
        return Ok(ActionResponse::error("Icon too long"));
    }
    let user_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let role = "member";

    // Hash password
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .to_string();

    // Insert user
    let res = sqlx::query("INSERT INTO users (id, username, password_hash, icon, role, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&user_id)
        .bind(&username)
        .bind(&password_hash)
        .bind(&icon)
        .bind(&role)
        .bind(&created_at)
        .execute(&db)
        .await;
    match res {
        Ok(_) => Ok(ActionResponse::ok(serde_json::json!({
            "user_id": user_id,
            "username": username,
            "icon": icon,
            "role": role,
            "created_at": created_at
        }))),
        Err(e) => {
            if e.to_string().contains("UNIQUE constraint failed") {
                Ok(ActionResponse::error("Username already exists"))
            } else {
                Ok(ActionResponse::error(&format!("DB Error: {e}")))
            }
        }
    }
}

pub async fn login(db: DbPool, payload: Value) -> anyhow::Result<ActionResponse> {
    let LoginPayload { username, password } = serde_json::from_value(payload)?;

    let user: Option<User> = sqlx::query_as("SELECT * FROM users WHERE username = ?")
        .bind(&username)
        .fetch_optional(&db)
        .await?;

    if let Some(user) = user {
        let parsed_hash =
            PasswordHash::new(&user.password_hash).map_err(|e| anyhow::anyhow!("{e}"))?;
        let argon2 = Argon2::default();
        if argon2
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
        {
            Ok(ActionResponse::ok(serde_json::json!({
                "user_id": user.id,
                "username": user.username,
                "role": user.role,
                "icon": user.icon,
                "created_at": user.created_at
            })))
        } else {
            Ok(ActionResponse::error("Invalid password"))
        }
    } else {
        Ok(ActionResponse::error("User not found"))
    }
}

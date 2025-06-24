use sqlx::{
    sqlite::{SqlitePoolOptions, SqliteRow},
    Pool, Row, Sqlite,
};
use std::time::Duration;

pub type DbPool = Pool<Sqlite>;

pub async fn init_db() -> anyhow::Result<DbPool> {
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_timeout(Duration::from_secs(30))
        .connect("sqlite://chat_server.db")
        .await?;

    // Create tables if not exist
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT UNIQUE NOT NULL,
            password_hash TEXT NOT NULL,
            icon TEXT NOT NULL,
            role TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS messages (
            id TEXT PRIMARY KEY,
            sender_id TEXT NOT NULL,
            content TEXT NOT NULL,
            created_at TEXT NOT NULL,
            FOREIGN KEY(sender_id) REFERENCES users(id)
        );
        CREATE TABLE IF NOT EXISTS banned_users (
            id TEXT PRIMARY KEY,
            admin_id TEXT NOT NULL,
            target_id TEXT NOT NULL,
            reason TEXT NOT NULL,
            banned_at TEXT NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await?;

    Ok(pool)
}

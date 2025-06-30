#[macro_use]
extern crate rocket;

mod auth;
mod db;
mod models;
mod ws;

use auth::{login, register};
use db::establish_db;
use sqlx::{Pool, Sqlite};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    dotenv::dotenv().ok();
    let db: Pool<Sqlite> = establish_db().await.expect("DB connection failed");
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let (broadcaster, _) = broadcast::channel::<String>(100);
    let redis_url_ws = redis_url.clone();
    let jwt_secret = "heyoheyoimahardcodedsecretandimgonnabepushedtotherepoingithubyay".to_string(); // Or load from config/env
    let db_ws = db.clone(); // Clone db_pool for the WebSocket server task

    // --- Channel creation logic moved here ---
    // Check if any channels exist. If not, create a default "general" channel.
    let channels_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&db) // Use the main `db` pool
        .await
        .expect("Failed to query channel count");

    if channels_count == 0 {
        println!("No channels found. Creating default 'home' channel.");
        sqlx::query(
            // Removed is_private and creator_username
            "INSERT INTO channels (id, name, icon) VALUES (?, ?, ?)",
        )
        .bind("home")
        .bind("Home")
        .bind("")
        .execute(&db) // Use the main `db` pool
        .await
        .expect("Failed to create default 'home' channel");
    }
    // --- End of channel creation logic ---

    tokio::spawn(async move {
        ws::websocket_server(db_ws, redis_url_ws, "127.0.0.1:9001", jwt_secret)
            .await
            .unwrap();
    });

    rocket::build()
        .manage(db)
        .manage(redis_url)
        .manage(Arc::new(Mutex::new(broadcaster)))
        .mount("/auth", routes![register, login])
        .launch()
        .await?;

    Ok(())
} // 󰱫

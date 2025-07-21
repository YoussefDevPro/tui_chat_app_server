#[macro_use]
extern crate rocket;

use log::{info, warn};

mod auth;
mod db;
mod files;
mod models;
mod ws;

use auth::{login, register};
use db::establish_db;
use files::{download_file, upload_file};
use redis::Client as RedisClient;
use sqlx::{Pool, Sqlite}; // Imported download_file

#[derive(Clone)]
pub struct AppState {
    pub db: Pool<Sqlite>,
    pub jwt_secret: String,
    pub redis_client: RedisClient,
}

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    env_logger::init();
    dotenv::dotenv().ok();
    let db_pool: Pool<Sqlite> = establish_db().await.expect("DB connection failed");
    info!("Database connection established.");
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6333".to_string());
    let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| {
        "heyoheyoimahardcodedsecretandimgonnabepushedtotherepoingithubyay".to_string()
    });

    let redis_client =
        redis::Client::open(redis_url.clone()).expect("Failed to create Redis client");
    info!("Redis client created.");

    let app_state = AppState {
        db: db_pool.clone(),
        jwt_secret: jwt_secret.clone(),
        redis_client,
    };

    // --- Channel creation logic moved here ---
    // Check if any channels exist. If not, create a default "general" channel.
    let channels_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM channels")
        .fetch_one(&db_pool) // Use the main `db` pool
        .await
        .expect("Failed to query channel count");

    if channels_count == 0 {
        warn!("No channels found. Creating default 'home' channel.");
        sqlx::query(
            // Removed is_private and creator_username
            "INSERT INTO channels (id, name, icon) VALUES (?, ?, ?)",
        )
        .bind("home")
        .bind("Home")
        .bind("")
        .execute(&db_pool) // Use the main `db` pool
        .await
        .expect("Failed to create default 'home' channel");
    }
    // --- End of channel creation logic ---

    let db_ws = db_pool.clone();
    let redis_url_ws = redis_url.clone();
    tokio::spawn(async move {
        ws::websocket_server(db_ws, redis_url_ws, "0.0.0.0:34093", jwt_secret)
            .await
            .unwrap();
    });

    info!("Starting Rocket server...");
    rocket::build()
        .manage(app_state)
        .mount("/auth", routes![register, login])
        .mount("/files", routes![upload_file, download_file]) // Mounted download_file
        .launch()
        .await?;

    Ok(())
} // 󰱫

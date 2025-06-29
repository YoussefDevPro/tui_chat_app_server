#[macro_use]
extern crate rocket;

mod auth;
mod db;
mod models;
mod ws;

use auth::{login, register};
use db::establish_db;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::sync::Mutex;

#[rocket::main]
async fn main() -> Result<(), rocket::Error> {
    dotenv::dotenv().ok();
    let db = establish_db().await.expect("DB connection failed");
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let (broadcaster, _) = broadcast::channel::<String>(100);
    let redis_url_ws = redis_url.clone();
    let jwt_secret = "heyoheyoimahardcodedsecretandimgonnabepushedtotherepoingithubyay".to_string(); // Or load from config/env
    let db_ws = db.clone();

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

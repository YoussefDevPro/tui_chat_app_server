use serde;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

mod auth;
mod chat;
mod db;
mod models;
mod user;

use models::{ActionRequest, ActionResponse};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = db::init_db().await?;
    let db = Arc::new(db);

    let listener = TcpListener::bind("127.0.0.1:5000").await?;
    println!("Server listening on 127.0.0.1:5000");

    loop {
        let (socket, _) = listener.accept().await?;
        let db = db.clone();

        tokio::spawn(async move {
            let (reader, mut writer) = socket.into_split();
            let mut lines = BufReader::new(reader).lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let req: Result<ActionRequest, _> = serde_json::from_str(&line);
                let db = db.clone();

                let response = match req {
                    Ok(action) => match action.action.as_str() {
                        "register" => auth::register((*db).clone(), action.payload).await,
                        "login" => auth::login((*db).clone(), action.payload).await,
                        "send_message" => chat::send_message((*db).clone(), action.payload).await,
                        "get_messages" => chat::get_messages((*db).clone(), action.payload).await,
                        "ban_user" => user::ban_user((*db).clone(), action.payload).await,
                        "unban_user" => user::unban_user((*db).clone(), action.payload).await,
                        "promote_user" => user::promote_user((*db).clone(), action.payload).await,
                        "change_username" => {
                            user::change_username((*db).clone(), action.payload).await
                        }
                        "change_icon" => user::change_icon((*db).clone(), action.payload).await,
                        _ => Ok(ActionResponse::error("Unknown action")),
                    },
                    Err(e) => Ok(ActionResponse::error(&format!("Invalid JSON: {}", e))),
                };

                let resp_json = serde_json::to_string(
                    &response.unwrap_or_else(|e| ActionResponse::error(&e.to_string())),
                )
                .unwrap();
                if let Err(e) = writer.write_all(resp_json.as_bytes()).await {
                    eprintln!("Write error: {e}");
                    break;
                }
                if let Err(e) = writer.write_all(b"\n").await {
                    eprintln!("Write error: {e}");
                    break;
                }
            }
        });
    }
}
// 󰱫

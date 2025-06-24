use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

mod auth;
mod chat;
mod db;
mod models;
mod user;

use db::DbPool;
use models::{ActionRequest, ActionResponse};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db = db::init_db().await?;
    let db = Arc::new(db);

    let listener = TcpListener::bind("127.0.0.1:6969").await?;
    println!("Server listening on 127.0.0.1:6969");

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
                        "register" => auth::register(db, action.payload).await,
                        "login" => auth::login(db, action.payload).await,
                        "send_message" => chat::send_message(db, action.payload).await,
                        "get_messages" => chat::get_messages(db, action.payload).await,
                        "ban_user" => user::ban_user(db, action.payload).await,
                        "unban_user" => user::unban_user(db, action.payload).await,
                        "promote_user" => user::promote_user(db, action.payload).await,
                        "change_username" => user::change_username(db, action.payload).await,
                        "change_icon" => user::change_icon(db, action.payload).await,
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

// i think i just made the project more complex
// what i want, is one server
// where i am the admin (ofc)
// and everyone can access this server (bc its the only one)
// and we have nerd font icon as the pfp os a user ' 󰄛 '
// well, now the only thing we have to save is messages, for the user, we're just going to add a
// role , admin, and member
// now its a managable project
// time to get back to work
// i think ill use reddis, bc the data is small and a 16 gb of ram can do more than what we need
// and for the tui, the standar ratatui 󰱫

// ADDED: ContentDisposition for parsing multipart headers.
use rocket::{http::{Status, Header}, post, get, serde::json::Json, State, response::{self, Responder, Response}, request::Request};
use rocket::tokio::fs::File;
use tokio::io::AsyncReadExt;
use uuid::Uuid;
use rocket_multipart::MultipartReader;
use content_disposition::parse_content_disposition; // Corrected import: Removed unused DispositionType

use std::path::{Path, PathBuf};
use crate::AppState;
use crate::ws::BroadcastMessage;
use chrono::Utc;
use redis::AsyncCommands;
use mime_guess;
use mime; // Ensure 'mime' crate is in scope
use base64::engine::Engine;
use futures_util::StreamExt; // This is likely needed for .next() and the warning should disappear.

const UPLOADS_DIR: &str = "uploads";
const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

use crate::auth::AuthUser;

#[post("/upload/<channel_id>", data = "<reader>")]
pub async fn upload_file(
    state: &State<AppState>,
    user: AuthUser,
    channel_id: String,
    mut reader: MultipartReader<'_>,
) -> Result<Json<String>, (Status, Json<String>)> {
    let file_id = Uuid::new_v4().to_string();
    let mut original_file_name = "unknown".to_string();
    let mut file_size: u64 = 0;
    let mut save_path = PathBuf::new();

    if !Path::new(UPLOADS_DIR).exists() {
        tokio::fs::create_dir_all(UPLOADS_DIR).await.map_err(|e| (Status::InternalServerError, Json(format!("Failed to create upload directory: {}", e))))?;
    }

    while let Ok(Some(mut field)) = reader.next().await {
        // Parse Content-Disposition from headers using the 'content_disposition' crate
        if let Some(disposition_str) = field.headers().get_one("Content-Disposition") {
            let parsed_disposition = parse_content_disposition(disposition_str);
            
            // Check if the field name is "file"
            // The `name()` method of ParsedContentDisposition returns an Option<String>
            if let Some(name_value) = parsed_disposition.name() { // Get the String value from Option
                if name_value == "file" { // Compare String with &str
                    // Get the filename from the disposition header
                    // The `filename()` method of ParsedContentDisposition returns an Option<(String, Option<String>)>
                    if let Some((filename_value, _)) = parsed_disposition.filename() { // Destructure the tuple
                        original_file_name = filename_value; // Assign String to String directly
                    }

                    save_path = PathBuf::from(UPLOADS_DIR).join(&file_id);
                    let mut dest_file = tokio::fs::File::create(&save_path).await.map_err(|e| (Status::InternalServerError, Json(format!("Failed to create destination file: {}", e))))?;
                    let bytes_written = tokio::io::copy(&mut field, &mut dest_file).await.map_err(|e| (Status::InternalServerError, Json(format!("Failed to write file: {}", e))))?;
                    file_size = bytes_written;
                    break; // Exit after processing the file field
                }
            }
        }
    }

    if file_size == 0 {
        return Err((Status::BadRequest, Json("No file uploaded or file is empty.".to_string())));
    }

    if file_size > MAX_FILE_SIZE {
        tokio::fs::remove_file(&save_path).await.map_err(|e| {
            (Status::InternalServerError, Json(format!("Failed to remove oversized file: {}", e)))
        })?;
        return Err((Status::PayloadTooLarge, Json(format!("File size exceeds limit of {} MB", MAX_FILE_SIZE / (1024 * 1024)))));
    }

    let file_size_mb = file_size as f64 / (1024.0 * 1024.0);
    let is_image = mime_guess::from_path(&original_file_name).first_or(mime::APPLICATION_OCTET_STREAM).type_() == "image";
    let image_preview: Option<String> = None;

    sqlx::query(
        "INSERT INTO files (id, original_name, uploader_username, size_bytes, channel_id, created_at) VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind(&file_id)
    .bind(&original_file_name)
    .bind(&user.0)
    .bind(file_size as i64)
    .bind(&channel_id)
    .bind(Utc::now().timestamp())
    .execute(&state.db)
    .await
    .map_err(|e| (Status::InternalServerError, Json(format!("Failed to insert file metadata: {}", e))))?;

    let mut redis_conn = state.redis_client.get_async_connection().await.map_err(|e| (Status::InternalServerError, Json(format!("Failed to connect to Redis: {}", e))))?;
    let channel_info = crate::ws::get_channel_info(&state.db, &channel_id).await.map_err(|e| (Status::InternalServerError, Json(format!("Failed to get channel info: {}", e))))?;

    let file_extension = Path::new(&original_file_name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());

    let message = BroadcastMessage {
        user: user.0.clone(),
        icon: "📁".to_string(),
        content: format!("{} uploaded a file: {}", user.0, original_file_name),
        timestamp: Utc::now().timestamp(),
        channel_id: channel_id.clone(),
        channel_name: channel_info.name,
        channel_icon: channel_info.icon,
        message_type: "file".to_string(),
        file_name: Some(original_file_name.clone()),
        file_extension,
        file_size_mb: Some(file_size_mb),
        is_image: Some(is_image),
        image_preview,
        file_id: Some(file_id.clone()),
        download_url: Some(format!("https://back.reetui.hackclub.app/files/download/{}", file_id)),
    };

    let msg_json = serde_json::to_string(&message).map_err(|e| (Status::InternalServerError, Json(format!("Failed to serialize message: {}", e))))?;
    let _: () = redis_conn.publish(format!("chat:{}", channel_id), msg_json).await.map_err(|e| (Status::InternalServerError, Json(format!("Failed to publish message to Redis: {}", e))))?;

    Ok(Json(file_id))
}


// A custom responder for streaming a file with a specific download name.
pub struct FileDownload {
    file: File,
    filename: String,
}

#[rocket::async_trait]
impl<'r> Responder<'r, 'static> for FileDownload {
    fn respond_to(self, _req: &'r Request<'_>) -> response::Result<'static> {
        let content_type = mime_guess::from_path(&self.filename)
            .first_or(mime::APPLICATION_OCTET_STREAM)
            .to_string();

        Response::build()
            .header(Header::new("Content-Type", content_type))
            .header(Header::new("Content-Disposition", format!("attachment; filename=\"{}\"", self.filename)))
            .streamed_body(self.file)
            .ok()
    }
}

#[get("/download/<file_id>")]
pub async fn download_file(
    state: &State<AppState>,
    file_id: String,
) -> Result<FileDownload, (Status, String)> {
    let file_metadata = sqlx::query!(
        "SELECT original_name FROM files WHERE id = ?",
        file_id
    )
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (Status::InternalServerError, format!("Failed to query file metadata: {}", e)))?
    .ok_or((Status::NotFound, "File not found.".to_string()))?;

    let file_path = PathBuf::from(UPLOADS_DIR).join(&file_id);

    let file = File::open(&file_path).await.map_err(|e| {
        (Status::InternalServerError, format!("Failed to open file for download: {}", e))
    })?;

    // Return our custom responder struct.
    Ok(FileDownload {
        file,
        filename: file_metadata.original_name,
    })
}


// ADDED: ContentDisposition for parsing multipart headers.
use content_disposition::parse_content_disposition;
use log::{debug, error, info, warn};
use rocket::tokio::fs::File;
use rocket::{
    get,
    http::{Header, Status},
    post,
    request::Request,
    response::{self, Responder, Response},
    serde::json::Json,
    State,
};
use rocket_multipart::MultipartReader;
use tokio::io::AsyncReadExt;
use uuid::Uuid; // Corrected import: Removed unused DispositionType

use crate::ws::BroadcastMessage;
use crate::AppState;
use base64::engine::Engine;
use chrono::Utc;
use futures_util::StreamExt;
use mime; // Ensure 'mime' crate is in scope
use mime_guess;
use redis::AsyncCommands;
use std::path::{Path, PathBuf}; // This is likely needed for .next() and the warning should disappear.

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
    info!("Received file upload request for channel: {}", channel_id);
    let file_id = Uuid::new_v4().to_string();
    let mut original_file_name = "unknown".to_string();
    let mut file_size: u64 = 0;
    let mut save_path = PathBuf::new();

    debug!("Checking if upload directory exists: {}", UPLOADS_DIR);
    if !Path::new(UPLOADS_DIR).exists() {
        info!("Upload directory does not exist, creating: {}", UPLOADS_DIR);
        tokio::fs::create_dir_all(UPLOADS_DIR).await.map_err(|e| {
            error!("Failed to create upload directory: {}", e);
            (
                Status::InternalServerError,
                Json(format!("Failed to create upload directory: {}", e)),
            )
        })?;
        info!("Upload directory created successfully.");
    }

    debug!("Processing multipart fields.");
    while let Ok(Some(mut field)) = reader.next().await {
        if let Some(disposition_str) = field.headers().get_one("Content-Disposition") {
            debug!("Parsing Content-Disposition: {}", disposition_str);
            let parsed_disposition = parse_content_disposition(disposition_str);

            if let Some(name_value) = parsed_disposition.name() {
                debug!("Field name: {}", name_value);
                if name_value == "file" {
                    if let Some((filename_value, _)) = parsed_disposition.filename() {
                        original_file_name = filename_value;
                        info!("Detected file upload: {}", original_file_name);
                    }

                    save_path = PathBuf::from(UPLOADS_DIR).join(&file_id);
                    info!("Saving file to: {:?}", save_path);
                    let mut dest_file = tokio::fs::File::create(&save_path).await.map_err(|e| {
                        error!("Failed to create destination file: {:?}: {}", save_path, e);
                        (
                            Status::InternalServerError,
                            Json(format!("Failed to create destination file: {}", e)),
                        )
                    })?;
                    let bytes_written =
                        tokio::io::copy(&mut field, &mut dest_file)
                            .await
                            .map_err(|e| {
                                error!("Failed to write file to {:?}: {}", save_path, e);
                                (
                                    Status::InternalServerError,
                                    Json(format!("Failed to write file: {}", e)),
                                )
                            })?;
                    file_size = bytes_written;
                    info!("File saved successfully. Size: {} bytes.", file_size);
                    break;
                }
            }
        }
    }

    if file_size == 0 {
        warn!("No file uploaded or file is empty.");
        return Err((
            Status::BadRequest,
            Json("No file uploaded or file is empty.".to_string()),
        ));
    }

    if file_size > MAX_FILE_SIZE {
        warn!(
            "Uploaded file size ({} bytes) exceeds limit ({} bytes).",
            file_size, MAX_FILE_SIZE
        );
        tokio::fs::remove_file(&save_path).await.map_err(|e| {
            error!("Failed to remove oversized file {:?}: {}", save_path, e);
            (
                Status::InternalServerError,
                Json(format!("Failed to remove oversized file: {}", e)),
            )
        })?;
        return Err((
            Status::PayloadTooLarge,
            Json(format!(
                "File size exceeds limit of {} MB",
                MAX_FILE_SIZE / (1024 * 1024)
            )),
        ));
    }

    let file_size_mb = file_size as f64 / (1024.0 * 1024.0);
    let is_image = mime_guess::from_path(&original_file_name)
        .first_or(mime::APPLICATION_OCTET_STREAM)
        .type_()
        == "image";
    let image_preview: Option<String> = None;

    info!(
        "Inserting file metadata into database for file_id: {}",
        file_id
    );
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
    .map_err(|e| {
        error!("Failed to insert file metadata for file_id {}: {}", file_id, e);
        (Status::InternalServerError, Json(format!("Failed to insert file metadata: {}", e)))
    })?;
    info!(
        "File metadata inserted successfully for file_id: {}",
        file_id
    );

    debug!("Connecting to Redis.");
    let mut redis_conn = state
        .redis_client
        .get_async_connection()
        .await
        .map_err(|e| {
            error!("Failed to connect to Redis: {}", e);
            (
                Status::InternalServerError,
                Json(format!("Failed to connect to Redis: {}", e)),
            )
        })?;
    info!("Connected to Redis.");

    debug!("Getting channel info for channel_id: {}", channel_id);
    let channel_info = crate::ws::get_channel_info(&state.db, &channel_id)
        .await
        .map_err(|e| {
            error!(
                "Failed to get channel info for channel_id {}: {}",
                channel_id, e
            );
            (
                Status::InternalServerError,
                Json(format!("Failed to get channel info: {}", e)),
            )
        })?;
    info!("Channel info retrieved for channel_id: {}", channel_id);

    let file_extension = Path::new(&original_file_name)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string());

    let message = BroadcastMessage {
        user: user.0.clone(),
        icon: get_nerd_font_icon_for_extension(file_extension.as_deref()).to_string(),
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
        download_url: Some(format!(
            "https://back.reetui.hackclub.app/files/download/{}",
            file_id
        )),
    };

    debug!("Serializing broadcast message.");
    let msg_json = serde_json::to_string(&message).map_err(|e| {
        error!("Failed to serialize message: {}", e);
        (
            Status::InternalServerError,
            Json(format!("Failed to serialize message: {}", e)),
        )
    })?;
    info!("Broadcast message serialized.");

    debug!(
        "Saving message to Redis history for channel: {}",
        channel_id
    );
    let _: () = redis_conn
        .rpush(format!("chat_history:{}", channel_id), &msg_json)
        .await
        .map_err(|e| {
            error!(
                "Failed to save message to Redis history for channel {}: {}",
                channel_id, e
            );
            (
                Status::InternalServerError,
                Json(format!("Failed to save message to Redis history: {}", e)),
            )
        })?;
    info!("Message saved to Redis history.");

    debug!("Publishing message to Redis channel: {}", channel_id);
    let _: () = redis_conn
        .publish(format!("chat:{}", channel_id), msg_json)
        .await
        .map_err(|e| {
            error!(
                "Failed to publish message to Redis channel {}: {}",
                channel_id, e
            );
            (
                Status::InternalServerError,
                Json(format!("Failed to publish message to Redis: {}", e)),
            )
        })?;
    info!("Message published to Redis.");

    info!("File upload successful for file_id: {}", file_id);
    Ok(Json(file_id))
}

fn get_nerd_font_icon_for_extension(extension: Option<&str>) -> &str {
    match extension {
        Some("pdf") => "",                                          // nf-fa-file_pdf_o
        Some("doc") | Some("docx") => "",                           // nf-fa-file_word_o
        Some("xls") | Some("xlsx") => "",                           // nf-fa-file_excel_o
        Some("ppt") | Some("pptx") => "",                           // nf-fa-file_powerpoint_o
        Some("zip") | Some("tar") | Some("gz") | Some("rar") => "", // nf-fa-file_archive_o
        Some("jpg") | Some("jpeg") | Some("png") | Some("gif") | Some("bmp") | Some("svg") => "", // nf-fa-file_image_o
        Some("mp3") | Some("wav") | Some("ogg") => "", // nf-fa-file_audio_o
        Some("mp4") | Some("mkv") | Some("avi") => "", // nf-fa-file_video_o
        Some("txt") => "",                             // nf-fa-file_text_o
        Some("json") => "",                            // nf-md-json
        Some("xml") => "󰗀",                             // nf-md-xml
        Some("html") | Some("htm") => "htm",            // nf-dev-html5
        Some("css") => "",                             // nf-dev-css3
        Some("js") | Some("jsx") => "",                // nf-dev-javascript
        Some("ts") | Some("tsx") => "",                // nf-dev-typescript
        Some("py") => "",                              // nf-dev-python
        Some("rs") => "󱘗",                              // nf-dev-rust
        Some("go") => "󰟓",                              // nf-dev-go
        Some("java") => "",                            // nf-dev-java
        Some("c") | Some("cpp") | Some("h") => "",     // nf-dev-c
        Some("sh") | Some("bash") => "",               // nf-fa-terminal
        Some("md") => " ",                             // nf-fa-file_markdown
        _ => "",                                       // nf-fa-file_o (generic file icon)
    }
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
            .header(Header::new(
                "Content-Disposition",
                format!("attachment; filename=\"{}\"", self.filename),
            ))
            .streamed_body(self.file)
            .ok()
    }
}

#[get("/download/<file_id>")]
pub async fn download_file(
    state: &State<AppState>,
    file_id: String,
) -> Result<FileDownload, (Status, String)> {
    info!("Received file download request for file_id: {}", file_id);
    let file_metadata = sqlx::query!("SELECT original_name FROM files WHERE id = ?", file_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            error!(
                "Failed to query file metadata for file_id {}: {}",
                file_id, e
            );
            (
                Status::InternalServerError,
                format!("Failed to query file metadata: {}", e),
            )
        })?
        .ok_or_else(|| {
            warn!("File not found in database for file_id: {}", file_id);
            (Status::NotFound, "File not found.".to_string())
        })?;

    let file_path = PathBuf::from(UPLOADS_DIR).join(&file_id);
    info!("Attempting to open file for download: {:?}", file_path);

    let file = File::open(&file_path).await.map_err(|e| {
        error!("Failed to open file {:?} for download: {}", file_path, e);
        (
            Status::InternalServerError,
            format!("Failed to open file for download: {}", e),
        )
    })?;
    info!("File opened successfully for download: {:?}", file_path);

    info!(
        "Serving file {} (id: {}) for download.",
        file_metadata.original_name, file_id
    );
    // Return our custom responder struct.
    Ok(FileDownload {
        file,
        filename: file_metadata.original_name,
    })
}

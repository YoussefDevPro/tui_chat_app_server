CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,
    original_name TEXT NOT NULL,
    uploader_username TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    channel_id TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

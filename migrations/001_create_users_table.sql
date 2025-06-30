CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    icon TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    IsAdmin BOOLEAN NOT NULL DEFAULT 0,
    IsSuperAdmin BOOLEAN NOT NULL DEFAULT 0,
    IsBanned BOOLEAN NOT NULL DEFAULT 0, 
    BanMuteUntil INTEGER
);

CREATE TABLE IF NOT EXISTS channels (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    icon TEXT NOT NULL,
    password_hash TEXT,     creator_username TEXT NOT NULL, 
    FOREIGN KEY (creator_username) REFERENCES users(username)
);

CREATE TABLE IF NOT EXISTS channel_bans (
    channel_id TEXT NOT NULL,
    banned_username TEXT NOT NULL,
    ban_mute_until INTEGER, 
    PRIMARY KEY (channel_id, banned_username),
    FOREIGN KEY (channel_id) REFERENCES channels(id),
    FOREIGN KEY (banned_username) REFERENCES users(username)
);

CREATE TABLE IF NOT EXISTS user_channels (
    user_id TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    PRIMARY KEY (user_id, channel_id),
    FOREIGN KEY (user_id) REFERENCES users(id),
    FOREIGN KEY (channel_id) REFERENCES channels(id)
);

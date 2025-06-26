use sqlx::{sqlite::SqlitePoolOptions, SqlitePool};

pub type DbConn = SqlitePool;

pub async fn establish_db() -> Result<DbConn, sqlx::Error> {
    let db_url = std::env::var("DATABASE_URL").unwrap_or("sqlite://chat.db".to_string());
    SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&db_url)
        .await
}

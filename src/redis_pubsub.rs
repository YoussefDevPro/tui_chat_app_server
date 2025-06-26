use redis::aio::Connection;
use redis::Client;

pub type RedisConn = Connection;

pub async fn establish_redis() -> redis::RedisResult<RedisConn> {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1/".to_string());
    let client = Client::open(url)?;
    client.get_async_connection().await
}

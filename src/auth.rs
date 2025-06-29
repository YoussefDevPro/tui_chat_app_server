use crate::{db::DbConn, models::Claims};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, TokenData, Validation};
use rocket::{
    http::Status,
    post,
    request::{FromRequest, Outcome, Request},
    serde::json::Json,
    State,
};
use sqlx::FromRow;
use std::env;
use uuid::Uuid;

#[derive(serde::Deserialize)]
pub struct RegisterInput {
    pub username: String,
    pub password_hash: String,
    pub icon: String, // New field
}

#[derive(serde::Deserialize)]
pub struct LoginInput {
    pub username: String,
    pub password_hash: String,
}

#[derive(serde::Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub icon: String, // New field
}

#[derive(FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub icon: String, // Ensure this is here
}
#[post("/register", data = "<input>")]
pub async fn register(
    db: &State<DbConn>,
    input: Json<RegisterInput>,
) -> Result<Json<TokenResponse>, Status> {
    let id = Uuid::new_v4();
    let username = &input.username;
    let password_hash = &input.password_hash;
    let icon = &input.icon;

    let res =
        sqlx::query("INSERT INTO users (id, username, password_hash, icon) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(username)
            .bind(password_hash)
            .bind(icon)
            .execute(db.inner())
            .await;

    match res {
        Ok(_) => {
            let token = generate_token(username)?;
            Ok(Json(TokenResponse {
                token,
                icon: icon.clone(),
            }))
        }
        Err(_) => Err(Status::Conflict),
    }
}

#[post("/login", data = "<input>")]
pub async fn login(
    db: &State<DbConn>,
    input: Json<LoginInput>,
) -> Result<Json<TokenResponse>, Status> {
    let username = &input.username;
    let password_hash = &input.password_hash;

    let user =
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ? AND password_hash = ?")
            .bind(username)
            .bind(password_hash)
            .fetch_optional(db.inner())
            .await
            .map_err(|_| Status::InternalServerError)?;

    match user {
        Some(user) => {
            let token = generate_token(username)?;
            Ok(Json(TokenResponse {
                token,
                icon: user.icon,
            }))
        }
        None => Err(Status::Unauthorized),
    }
}

pub fn generate_token(username: &str) -> Result<String, Status> {
    let secret = env::var("JWT_SECRET").map_err(|_| Status::InternalServerError)?;
    let claims = Claims {
        sub: username.to_owned(),
        exp: (chrono::Utc::now().timestamp() + 60 * 60 * 24) as usize, // 24h
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| Status::InternalServerError)
}

pub struct AuthUser(pub String);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AuthUser {
    type Error = ();

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let secret = env::var("JWT_SECRET");
        if let Some(auth) = req.headers().get_one("Authorization") {
            if let Some(token) = auth.strip_prefix("Bearer ") {
                let res = decode::<Claims>(
                    token,
                    &DecodingKey::from_secret(secret.unwrap().as_bytes()),
                    &Validation::default(),
                );
                if let Ok(TokenData { claims, .. }) = res {
                    return Outcome::Success(AuthUser(claims.sub));
                }
            }
        }
        Outcome::Error((Status::Unauthorized, ()))
    }
}

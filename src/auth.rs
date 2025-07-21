use crate::{models::Claims, AppState};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, TokenData, Validation};
use rocket::{
    http::Status,
    post,
    request::{FromRequest, Outcome, Request},
    serde::json::Json,
    State,
};
use sqlx::FromRow;
use uuid::Uuid;
use log::{info, warn, error};

// Add these imports for argon2
use argon2::{
    password_hash::{
        rand_core::OsRng, // For generating secure random salts
        PasswordHasher,
        PasswordVerifier,
        SaltString,
    },
    Argon2, // The Argon2 algorithm struct
};

#[derive(serde::Deserialize)]
pub struct RegisterInput {
    pub username: String,
    pub password: String, // Plain password
    pub icon: String,
}

#[derive(serde::Deserialize)]
pub struct LoginInput {
    pub username: String,
    pub password: String, // Plain password
}

#[derive(serde::Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub icon: String,
}

#[derive(FromRow, Debug)]
#[allow(dead_code)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String, // Stores the Argon2 hash string
    pub icon: String,
}

#[post("/register", data = "<input>")]
pub async fn register(
    state: &State<AppState>,
    input: Json<RegisterInput>,
) -> Result<Json<TokenResponse>, Status> {
    let id = Uuid::new_v4();
    let username = &input.username;
    let password = &input.password;
    let icon = &input.icon;

    info!("Attempting to register user: {}", username);

    // Generate a random salt
    let salt = SaltString::generate(&mut OsRng);
    // Create an Argon2 instance with default parameters (or configure as needed)
    let argon2 = Argon2::default();

    // Hash the password
    let hashed_password = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| {
            error!("Password hashing failed for user: {}: {:?}", username, e);
            Status::InternalServerError
        })? // Handle hashing errors
        .to_string(); // Convert the PasswordHash struct to a string for storage
    info!("Password hashed successfully for user: {}", username);
    eprintln!("Registering user: {}, Hashed Password: {}", username, hashed_password);

    let res =
        sqlx::query("INSERT INTO users (id, username, password_hash, icon) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(username)
            .bind(hashed_password) // Use the Argon2 hashed password
            .bind(icon)
            .execute(&state.db)
            .await;

    match res {
        Ok(_) => {
            info!("User {} successfully registered.", username);
            let token = generate_token(username, &state.jwt_secret)?;
            Ok(Json(TokenResponse {
                token,
                icon: icon.clone(),
            }))
        }
        Err(e) => {
            warn!("Failed to register user {}: {:?}", username, e);
            Err(Status::Conflict)
        } // Username might already exist, or other DB error
    }
}

#[post("/login", data = "<input>")]
pub async fn login(
    state: &State<AppState>,
    input: Json<LoginInput>,
) -> Result<Json<TokenResponse>, Status> {
    let username = &input.username;
    let password = &input.password; // Plain password from input

    info!("Attempting login for user: {}", username);

    eprintln!("Attempting login for user: {}", username);

    let user = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| {
            error!("Database error fetching user {}: {:?}", username, e);
            Status::InternalServerError
        })?;

    match user {
        Some(user) => {
            info!("User {} found. Verifying password.", username);
            // Reconstruct the stored hash from the string
            let parsed_hash = argon2::password_hash::PasswordHash::new(&user.password_hash)
                .map_err(|e| {
                    error!("Error parsing stored hash for {}: {:?}", username, e);
                    Status::InternalServerError
                })?; // Error if stored hash is invalid

            let argon2 = Argon2::default();

            // Verify the plain password against the stored hash
            let passwords_match = argon2
                .verify_password(password.as_bytes(), &parsed_hash)
                .map(|_| {
                    info!("Password verification successful for {}", username);
                    true
                }) // If verification succeeds, map to true
                .map_err(|e| {
                    warn!("Password verification failed for {}: {:?}", username, e);
                    Status::Unauthorized
                })?; // If verification fails (or internal error), map to Unauthorized

            if passwords_match {
                let token = generate_token(username, &state.jwt_secret)?;
                info!("Login successful for user: {}", username);
                Ok(Json(TokenResponse {
                    token,
                    icon: user.icon,
                }))
            } else {
                warn!("Incorrect password for user: {}", username);
                Err(Status::Unauthorized) // Passwords don't match
            }
        }
        None => {
            warn!("User {} not found in database.", username);
            Err(Status::Unauthorized) // User not found
        }
    }
}

pub fn generate_token(username: &str, secret: &str) -> Result<String, Status> {
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

#[derive(Debug)]
#[allow(dead_code)]
pub struct AuthUser(pub String, pub String);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for AuthUser {
    type Error = ();

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let state = req.rocket().state::<AppState>().unwrap();
        if let Some(auth) = req.headers().get_one("Authorization") {
            if let Some(token) = auth.strip_prefix("Bearer ") {
                let res = decode::<Claims>(
                    token,
                    &DecodingKey::from_secret(state.jwt_secret.as_bytes()),
                    &Validation::default(),
                );
                if let Ok(TokenData { claims, .. }) = res {
                    let username = claims.sub;
                    // Retrieve user icon from the database
                    let user_info = sqlx::query_as::<_, User>("SELECT * FROM users WHERE username = ?")
                        .bind(&username)
                        .fetch_one(&state.db)
                        .await;

                    if let Ok(user) = user_info {
                        return Outcome::Success(AuthUser(username, user.icon));
                    } else {
                        error!("Failed to retrieve user info for {}. Token valid but user not found in DB.", username);
                        return Outcome::Error((Status::InternalServerError, ()));
                    }
                }
            }
        }
        Outcome::Error((Status::Unauthorized, ()))
    }
}

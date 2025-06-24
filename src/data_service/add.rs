use super::DataService;
use argon2::password_hash::{rand_core::OsRng, SaltString};
use argon2::{Argon2, PasswordHasher};
use chrono::Utc;

impl DataService {
    pub async fn add_user(
        &self,
        name: String,
        password: String,
        icon: String,
    ) -> anyhow::Result<i64> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
        let hashed_password = argon2
            .hash_password(password.as_bytes(), &salt)
            .unwrap()
            .to_string();
        let now = Utc::now();

        let rec = sqlx::query!(
            r#"
            INSERT INTO users (name, password, date_of_creation, icon)
            VALUES (?, ?, ?, ?)
            "#,
            name,
            hashed_password,
            now.to_rfc3339(),
            icon
        )
        .execute(&*self.pool)
        .await?;

        Ok(rec.last_insert_rowid())
    }

    // ill add more later, i have to test if everything work before
}

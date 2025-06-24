use super::DataService;
use crate::models::User;
use chrono::{DateTime, Utc};

impl DataService {
    pub async fn get_user(&self, user_id: i64) -> anyhow::Result<Option<User>> {
        let row = sqlx::query!(
            "SELECT id, name, password, date_of_creation, icon FROM users WHERE id = ?",
            user_id
        )
        .fetch_optional(&*self.pool)
        .await?;

        Ok(row.map(|r| User {
            id: r.id,
            name: r.name,
            password: r.password,
            date_of_creation: r.date_of_creation.parse::<DateTime<Utc>>().unwrap(),
            icon: r.icon,
        }))
    }
}

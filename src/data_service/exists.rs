use super::DataService;

impl DataService {
    pub async fn user_exists(&self, user_id: i64) -> anyhow::Result<bool> {
        let row = sqlx::query!("SELECT id FROM users WHERE id = ?", user_id)
            .fetch_optional(&*self.pool)
            .await?;
        Ok(row.is_some())
    }

    pub async fn server_exists(&self, server_id: i64) -> anyhow::Result<bool> {
        let row = sqlx::query!("SELECT id FROM servers WHERE id = ?", server_id)
            .fetch_optional(&*self.pool)
            .await?;
        Ok(row.is_some())
    }
}

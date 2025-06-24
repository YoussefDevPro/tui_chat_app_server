use super::DataService;

impl DataService {
    pub async fn remove_user(&self, user_id: i64) -> anyhow::Result<()> {
        sqlx::query!("DELETE FROM users WHERE id = ?", user_id)
            .execute(&*self.pool)
            .await?;
        Ok(())
    }
}

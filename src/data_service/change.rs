use super::DataService;

impl DataService {
    pub async fn change_user_name(&self, user_id: i64, new_name: String) -> anyhow::Result<()> {
        sqlx::query!("UPDATE users SET name = ? WHERE id = ?", new_name, user_id)
            .execute(&*self.pool)
            .await?;
        Ok(())
    }
}

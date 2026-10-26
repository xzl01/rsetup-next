use crate::{ControllerConfig, ControllerError};

#[derive(Clone)]
pub struct DbPool(pub sqlx::MySqlPool);

impl DbPool {
    pub async fn connect(config: &ControllerConfig) -> Result<Self, ControllerError> {
        Ok(Self(sqlx::MySqlPool::connect(&config.database_url).await?))
    }
}

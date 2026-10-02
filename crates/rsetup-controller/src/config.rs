use crate::ControllerError;

#[derive(Clone, Debug)]
pub struct ControllerConfig {
    pub database_url: String,
    pub listen_address: String,
}

impl ControllerConfig {
    pub fn from_env() -> Result<Self, ControllerError> {
        let database_url = std::env::var("CONTROLLER_DATABASE_URL")
            .map_err(|_| ControllerError::Config("CONTROLLER_DATABASE_URL is required".into()))?;
        if database_url.is_empty() {
            return Err(ControllerError::Config(
                "CONTROLLER_DATABASE_URL is empty".into(),
            ));
        }
        Ok(Self {
            database_url,
            listen_address: std::env::var("CONTROLLER_LISTEN_ADDRESS")
                .unwrap_or_else(|_| "127.0.0.1:8080".into()),
        })
    }
}

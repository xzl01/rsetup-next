#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error("invalid argument")]
    InvalidArgument,
    #[error("revision conflict")]
    RevisionConflict,
    #[error("not found")]
    NotFound,
    #[error("configuration: {0}")]
    Config(String),
    #[error("identity schema not ready: found {found:?}, required {required}")]
    SchemaNotReady { found: Option<i32>, required: i32 },
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("cryptography error")]
    Crypto,
}

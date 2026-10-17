#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error("invalid argument")]
    InvalidArgument,
    #[error("revision conflict")]
    RevisionConflict,
    #[error("permission denied")]
    PermissionDenied,
    #[error("not found")]
    NotFound,
    #[error("resource exhausted")]
    ResourceExhausted,
    #[error("configuration: {0}")]
    Config(String),
    #[error("identity schema not ready: found {found:?}, required {required}")]
    SchemaNotReady { found: Option<i32>, required: i32 },
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("cryptography error")]
    Crypto,
    #[error("last administrator cannot be deactivated or demoted")]
    LastAdmin,
    #[error("invalid state for this operation")]
    InvalidState,
    /// 同一 actor 对相同幂等键提交了不同 `request_hash` 的载荷（HTTP 409
    /// `IDEMPOTENCY_CONFLICT`）；与 `InvalidState` 分列，避免丢失契约分支。
    #[error("task idempotency conflict")]
    TaskIdempotencyConflict,
    /// 预览快照处于无效状态：过期、已消费或跨进程代际（HTTP 409 `INVALID_STATE`）。
    #[error("task preview invalid state")]
    TaskPreviewInvalidState,
}

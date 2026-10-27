use crate::error::ControllerError;
use crate::tasks::service::contracts::SubmitError;

#[test]
fn submit_errors_map_to_contract_codes() {
    assert!(matches!(
        ControllerError::from(SubmitError::InvalidArgument),
        ControllerError::InvalidArgument
    ));
    assert!(matches!(
        ControllerError::from(SubmitError::ActorInactive),
        ControllerError::PermissionDenied
    ));
    // 忙目标不整体报错：防御兜底必须是 400，绝不 409
    assert!(matches!(
        ControllerError::from(SubmitError::DeviceBusyCount { busy: 1, total: 2 }),
        ControllerError::InvalidArgument
    ));
    assert!(matches!(
        ControllerError::from(SubmitError::PreviewExpired),
        ControllerError::TaskPreviewInvalidState
    ));
    assert!(matches!(
        ControllerError::from(SubmitError::PreviewConsumed),
        ControllerError::TaskPreviewInvalidState
    ));
    assert!(matches!(
        ControllerError::from(SubmitError::IdempotencyConflict),
        ControllerError::TaskIdempotencyConflict
    ));
    assert!(matches!(
        ControllerError::from(SubmitError::NotVisible),
        ControllerError::NotFound
    ));
}

#[test]
fn sqlx_errors_are_wrapped_as_backend_database() {
    let err = SubmitError::from(sqlx::Error::RowNotFound);
    assert!(matches!(
        err,
        SubmitError::Backend(ControllerError::Database(_))
    ));
    assert!(matches!(
        ControllerError::from(err),
        ControllerError::Database(_)
    ));
}

#[test]
fn submit_error_backend_maps_through_to_same_controller_error() {
    // 错误保真回归：From<SubmitError> for ControllerError 的 Backend(err) => err 原样透传。
    // actor 行缺失在 Identity 真源是 PermissionDenied（403），不得重映 503 或降为 400；
    // Config（503 STORAGE_UNAVAILABLE）保持原变体。本测试不覆盖真实异步 DB 路径。
    let denied = ControllerError::from(SubmitError::Backend(ControllerError::PermissionDenied));
    assert!(matches!(denied, ControllerError::PermissionDenied));
    let config = ControllerError::from(SubmitError::Backend(ControllerError::Config(
        "policy-fixture".into(),
    )));
    assert!(matches!(config, ControllerError::Config(_)));
}

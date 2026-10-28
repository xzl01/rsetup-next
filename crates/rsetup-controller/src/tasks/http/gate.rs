//! 任务业务会话门禁与统一错误映射。
//!
//! 提供业务会话状态校验（临时密码 403 `PASSWORD_CHANGE_REQUIRED` 拦截）、
//! 受控恢复模式拦截响应以及 8 条任务路由共用的统一错误映射。

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::json;

use crate::auth::service::Session;
use crate::error::ControllerError;
use crate::http_api::err_response;
use crate::http_gate::{exhausted, not_ready};
use crate::operations::RecoveryMode;

/// 业务会话前置：`must_change_password`（规格 01 A-02 临时密码）一律 403
/// `PASSWORD_CHANGE_REQUIRED`——**不要求** `is_admin`（admin 临时密码同样 403）、
/// 不复制 Host/Cookie/CSRF；缺失会话仍由共享 gate 严格 401 `AUTH_REQUIRED`。
#[allow(clippy::result_large_err)]
pub(crate) fn require_business_session(session: &Session) -> Result<(), Response> {
    if session.user.must_change_password {
        return Err(err_response(
            StatusCode::FORBIDDEN,
            "PASSWORD_CHANGE_REQUIRED",
            "auth.password_change_required",
            json!({}),
        ));
    }
    Ok(())
}

/// 受控恢复模式下的变更任务提交响应（05 §9「禁止变更任务调度/提交」）。
///
/// 503 而非 403：模式是**临时**运行状态，不是对调用者身份的判定，解除受控模式后
/// 同一请求即可重放；`params.recovery_mode` 逐字给出线上契约字符串，便于前端提示，
/// 不含设备、任务或凭据信息（去敏）。
pub(crate) fn controlled_recovery_response() -> Response {
    err_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "CONTROLLED_RECOVERY_ACTIVE",
        "task.controlled_recovery_active",
        json!({ "recovery_mode": RecoveryMode::ControlledRecovery.as_str() }),
    )
}

/// 8 条任务路由共用的唯一错误映射：既有契约码 + Task 4 两个新变体，
/// **无通用 500 兜底**（其余变体统一 503 `NOT_READY`，与既有 `backend_error` 一致）。
pub(crate) fn task_error_response(err: &ControllerError) -> Response {
    match err {
        ControllerError::PermissionDenied => err_response(
            StatusCode::FORBIDDEN,
            "PERMISSION_DENIED",
            "permission.denied",
            json!({}),
        ),
        ControllerError::InvalidArgument => err_response(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "task.invalid_request",
            json!({}),
        ),
        ControllerError::NotFound => err_response(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "system.not_found",
            json!({}),
        ),
        ControllerError::RevisionConflict => err_response(
            StatusCode::CONFLICT,
            "REVISION_CONFLICT",
            "task.revision_conflict",
            json!({}),
        ),
        ControllerError::TaskIdempotencyConflict => err_response(
            StatusCode::CONFLICT,
            "IDEMPOTENCY_CONFLICT",
            "task.idempotency_conflict",
            json!({}),
        ),
        ControllerError::TaskPreviewInvalidState => err_response(
            StatusCode::CONFLICT,
            "INVALID_STATE",
            "task.preview_invalid_state",
            json!({}),
        ),
        ControllerError::Database(_) => err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "STORAGE_UNAVAILABLE",
            "system.storage_unavailable",
            json!({}),
        ),
        ControllerError::ResourceExhausted => exhausted(),
        _ => not_ready(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::Value;

    use crate::tasks::http::fixtures::fixture_session;

    #[tokio::test]
    async fn temporary_password_forbids_admin_and_plain_with_exact_envelope() {
        for is_admin in [true, false] {
            let resp = require_business_session(&fixture_session(is_admin, true))
                .expect_err("临时密码（含 admin）必须 403");
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "is_admin={is_admin}");
            let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["error"]["code"],
                json!("PASSWORD_CHANGE_REQUIRED"),
                "is_admin={is_admin}"
            );
            assert_eq!(
                body["error"]["message_key"],
                json!("auth.password_change_required"),
                "is_admin={is_admin}"
            );
            assert_eq!(body["error"]["params"], json!({}), "is_admin={is_admin}");
            let error_keys: Vec<&str> = body["error"]
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                error_keys.len(),
                3,
                "error 必须恰为 code/message_key/params: {body}"
            );
            let top_keys: Vec<&str> = body
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                top_keys.len(),
                2,
                "envelope 必须恰为 error+request_id: {body}"
            );
        }
    }

    #[test]
    fn normal_session_passes_for_plain_and_admin() {
        for is_admin in [false, true] {
            assert!(
                require_business_session(&fixture_session(is_admin, false)).is_ok(),
                "正常（非临时密码）会话必须通过, is_admin={is_admin}"
            );
        }
    }

    async fn status_and_code(err: &ControllerError) -> (u16, String, String) {
        let resp = task_error_response(err);
        let status = resp.status().as_u16();
        let body = to_bytes(resp.into_body(), 1_048_576).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        (
            status,
            value["error"]["code"].as_str().unwrap().to_owned(),
            value["error"]["message_key"].as_str().unwrap().to_owned(),
        )
    }

    #[tokio::test]
    async fn error_mapping_is_exact_for_every_contract_variant() {
        assert_eq!(
            status_and_code(&ControllerError::TaskIdempotencyConflict).await,
            (
                409,
                "IDEMPOTENCY_CONFLICT".into(),
                "task.idempotency_conflict".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::TaskPreviewInvalidState).await,
            (
                409,
                "INVALID_STATE".into(),
                "task.preview_invalid_state".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::RevisionConflict).await,
            (
                409,
                "REVISION_CONFLICT".into(),
                "task.revision_conflict".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::NotFound).await,
            (404, "NOT_FOUND".into(), "system.not_found".into())
        );
        assert_eq!(
            status_and_code(&ControllerError::PermissionDenied).await,
            (403, "PERMISSION_DENIED".into(), "permission.denied".into())
        );
        assert_eq!(
            status_and_code(&ControllerError::InvalidArgument).await,
            (
                400,
                "INVALID_ARGUMENT".into(),
                "task.invalid_request".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::Database(sqlx::Error::RowNotFound)).await,
            (
                503,
                "STORAGE_UNAVAILABLE".into(),
                "system.storage_unavailable".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::ResourceExhausted).await,
            (
                503,
                "RESOURCE_EXHAUSTED".into(),
                "system.resource_exhausted".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::Crypto).await,
            (503, "NOT_READY".into(), "system.not_ready".into())
        );
    }

    #[tokio::test]
    async fn resource_exhausted_carries_retry_after_60() {
        let resp = task_error_response(&ControllerError::ResourceExhausted);
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("60")
        );
    }
}

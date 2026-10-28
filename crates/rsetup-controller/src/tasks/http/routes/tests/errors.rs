//! 错误透传与状态码映射集成测试（4 例）。

use std::sync::Arc;

use axum::http::StatusCode;
use tower::ServiceExt;

use super::fakes::{
    CREATE_TASK_BODY, FakeFailure, FakeTaskApi, app, envelope, get_request, post_request,
};
use crate::tasks::http::fixtures::IDEMPOTENCY_KEY;

#[tokio::test]
async fn database_error_maps_to_503_never_empty_list() {
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::Database);
    let api = Arc::new(fake);
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request("/api/v1/tasks", true))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "STORAGE_UNAVAILABLE");
    assert_eq!(body["error"]["message_key"], "system.storage_unavailable");
    assert!(body.get("data").is_none(), "DB 错误绝不降级为空 items");
}

#[tokio::test]
async fn permission_denied_and_exhausted_map_through_router() {
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::PermissionDenied);
    let api = Arc::new(fake);
    let task_id = api.detail.id;
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request(&format!("/api/v1/tasks/{task_id}"), true))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
    assert_eq!(body["error"]["message_key"], "permission.denied");

    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::ResourceExhausted);
    let api = Arc::new(fake);
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request("/api/v1/tasks", true))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "RESOURCE_EXHAUSTED");
    assert_eq!(body["error"]["message_key"], "system.resource_exhausted");
}

#[tokio::test]
async fn idempotency_conflict_maps_to_409_on_create() {
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::IdempotencyConflict);
    let api = Arc::new(fake);
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "IDEMPOTENCY_CONFLICT");
    assert_eq!(body["error"]["message_key"], "task.idempotency_conflict");
}

#[tokio::test]
async fn preview_invalid_state_maps_to_409_on_create() {
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::PreviewInvalidState);
    let api = Arc::new(fake);
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "INVALID_STATE");
    assert_eq!(body["error"]["message_key"], "task.preview_invalid_state");
}

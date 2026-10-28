//! 鉴权/CSRF/限速与请求格式前置门禁集成测试（7 例）。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use uuid::Uuid;

use super::fakes::{
    CREATE_TASK_BODY, CSRF_TOKEN, FakeTaskApi, app, envelope, get_request, post_request,
};
use crate::tasks::http::fixtures::{IDEMPOTENCY_KEY, RAW_TOKEN};

#[tokio::test]
async fn missing_session_never_reaches_business_repository() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            false,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "AUTH_REQUIRED");
    assert_eq!(body["error"]["message_key"], "auth.required");
    assert_eq!(body["error"]["params"], serde_json::json!({}));
    assert!(
        api.calls().is_empty(),
        "缺会话必须在触达业务仓储之前拒绝: {:?}",
        api.calls()
    );
}

#[tokio::test]
async fn missing_session_on_read_is_401_before_business() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), true, false, true)
        .oneshot(get_request("/api/v1/tasks", false))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "AUTH_REQUIRED");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn temporary_password_session_is_forbidden_before_business() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), false, true, true)
        .oneshot(get_request("/api/v1/tasks", true))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "PASSWORD_CHANGE_REQUIRED");
    assert_eq!(
        body["error"]["message_key"],
        "auth.password_change_required"
    );
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn write_without_csrf_binding_is_403_before_business() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), true, false, false)
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "CSRF_INVALID");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn unknown_field_in_create_task_body_is_400_before_business() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            "/api/v1/tasks",
            r#"{"preview_token":"00","confirm":true,"evil":1}"#,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(body["error"]["message_key"], "request.invalid_body");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn non_json_content_type_is_400_before_business() {
    let api = Arc::new(FakeTaskApi::new());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/tasks")
        .header("host", "localhost")
        .header("origin", "http://localhost")
        .header("content-type", "text/plain")
        .header("x-csrf-token", CSRF_TOKEN)
        .header("cookie", format!("rsc_session={RAW_TOKEN}"))
        .header("idempotency-key", IDEMPOTENCY_KEY)
        .body(Body::from(CREATE_TASK_BODY))
        .unwrap();
    let response = app(api.clone(), true, false, true)
        .oneshot(request)
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message_key"], "request.invalid_body");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn non_admin_malformed_write_requests_prefer_400_over_403() {
    // 门禁顺序钉死：path/body 参数解析与校验先于 require_admin_session，
    // 非管理员的非法输入优先 400，不得先 403。
    let task_id = Uuid::new_v4();
    let time_review_body =
        r#"{"expected_revision":"3","decision":"expire","evidence":"inconclusive"}"#;
    let release_lock_body = r#"{"expected_revision":"4","acknowledge_unknown":true,"reason":"ok"}"#;
    for (uri, body, key) in [
        (
            format!("/api/v1/tasks/{task_id}/time-review"),
            "{not-json",
            "request.invalid_body",
        ),
        (
            "/api/v1/tasks/not-a-uuid/time-review".to_owned(),
            time_review_body,
            "task.invalid_request",
        ),
        (
            format!("/api/v1/subtasks/{task_id}/release-lock"),
            "{not-json",
            "request.invalid_body",
        ),
        (
            "/api/v1/subtasks/not-a-uuid/release-lock".to_owned(),
            release_lock_body,
            "task.invalid_request",
        ),
    ] {
        let api = Arc::new(FakeTaskApi::new());
        let response = app(api.clone(), false, false, true)
            .oneshot(post_request(&uri, body, true, None))
            .await
            .unwrap();
        let (status, value) = envelope(response).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{uri} 非管理员非法输入必须优先 400 而非 403"
        );
        assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(value["error"]["message_key"], key);
        assert!(api.calls().is_empty(), "非法输入不得触达业务仓储");
    }
}

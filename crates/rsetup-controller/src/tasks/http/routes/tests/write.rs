//! 写端点（cancel / time_review / release_lock）集成测试（10 例）。

use std::sync::Arc;

use axum::http::StatusCode;
use tower::ServiceExt;
use uuid::Uuid;

use super::fakes::{FakeFailure, FakeTaskApi, app, envelope, post_request};
use crate::tasks::model::SubTaskState;

#[tokio::test]
async fn cancel_projects_accepted_and_maps_revision_conflict() {
    let api = Arc::new(FakeTaskApi::new());
    let task_id = api.detail.id;
    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/cancel"),
            r#"{"expected_revision":"3","confirm":true}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["accepted"], true);
    assert_eq!(
        body["data"]["cancelled"],
        serde_json::json!([task_id.to_string()])
    );

    let mut failing = FakeTaskApi::new();
    failing.failure = Some(FakeFailure::RevisionConflict);
    let api = Arc::new(failing);
    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/cancel"),
            r#"{"expected_revision":"3","confirm":true}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "REVISION_CONFLICT");
    assert_eq!(body["error"]["message_key"], "task.revision_conflict");
}

#[tokio::test]
async fn cancel_rejects_non_decimal_revision_before_business() {
    let api = Arc::new(FakeTaskApi::new());
    let task_id = api.detail.id;
    for revision in ["\"-1\"", "\"03\"", "\"abc\"", "3"] {
        let api = Arc::new(FakeTaskApi::new());
        let response = app(api.clone(), false, false, true)
            .oneshot(post_request(
                &format!("/api/v1/tasks/{task_id}/cancel"),
                &format!("{{\"expected_revision\":{revision},\"confirm\":true}}"),
                true,
                None,
            ))
            .await
            .unwrap();
        let (status, body) = envelope(response).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "revision {revision} 必须 400"
        );
        assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
        assert!(api.calls().is_empty(), "非法 revision 不得触达业务仓储");
    }
}

#[tokio::test]
async fn time_review_requires_admin_and_projects_current_view() {
    let api = Arc::new(FakeTaskApi::new());
    let task_id = api.detail.id;
    let body = r#"{"expected_revision":"3","decision":"resume_original_deadline","evidence":"reference_deadline_proven"}"#;

    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/time-review"),
            body,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, value) = envelope(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value["error"]["code"], "PERMISSION_DENIED");
    assert!(api.calls().is_empty(), "非管理员不得触达业务仓储");

    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/time-review"),
            body,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, value) = envelope(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["data"]["counts"]["succeeded"], "1");
    assert_eq!(value["data"]["view_scope"], "authorized_subset");
    assert_eq!(api.calls(), vec!["time_review"]);
}

#[tokio::test]
async fn time_review_rejects_unknown_decision_and_evidence_with_400() {
    let task_id = Uuid::new_v4();
    for body in [
        r#"{"expected_revision":"3","decision":"extend","evidence":"reference_deadline_proven"}"#,
        r#"{"expected_revision":"3","decision":"expire","evidence":"extend_by_1h"}"#,
        r#"{"expected_revision":"3","decision":"expire"}"#,
    ] {
        let api = Arc::new(FakeTaskApi::new());
        let response = app(api.clone(), true, false, true)
            .oneshot(post_request(
                &format!("/api/v1/tasks/{task_id}/time-review"),
                body,
                true,
                None,
            ))
            .await
            .unwrap();
        let (status, value) = envelope(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} 必须 400");
        assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
        assert!(api.calls().is_empty());
    }
}

#[tokio::test]
async fn time_review_no_held_maps_to_400_task_no_held() {
    // 复核成功且可见全集（Succeeded/Unknown）不含 held → 确实无 held 子任务。
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::InvalidArgument);
    fake.detail_ok = true;
    let api = Arc::new(fake);
    let task_id = api.detail.id;
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/time-review"),
            r#"{"expected_revision":"3","decision":"expire","evidence":"inconclusive"}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(body["error"]["message_key"], "task.no_held");
    assert_eq!(api.calls(), vec!["time_review", "task_detail"]);
}

#[tokio::test]
async fn time_review_contradiction_with_held_maps_to_400_invalid_request() {
    // 复核成功但可见全集仍含 held：服务侧 InvalidArgument 的成因是决策矛盾
    // （Contradiction）等非法请求，任务确有 held 子任务，不得谎报 task.no_held。
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::InvalidArgument);
    fake.detail_ok = true;
    fake.detail.visible_states = vec![SubTaskState::Held, SubTaskState::Succeeded];
    let api = Arc::new(fake);
    let task_id = api.detail.id;
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/time-review"),
            r#"{"expected_revision":"3","decision":"expire","evidence":"inconclusive"}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(body["error"]["message_key"], "task.invalid_request");
    assert_eq!(api.calls(), vec!["time_review", "task_detail"]);
}

#[tokio::test]
async fn time_review_detail_database_error_maps_to_503() {
    // 复核自身失败（Database）必须原样透传 503，不得被吞没为 400 task.no_held。
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::InvalidArgument);
    fake.detail_failure = Some(FakeFailure::Database);
    let api = Arc::new(fake);
    let task_id = api.detail.id;
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/time-review"),
            r#"{"expected_revision":"3","decision":"expire","evidence":"inconclusive"}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    // 复核自身失败不得被吞没为 400 `task.no_held`。
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "STORAGE_UNAVAILABLE");
    assert_eq!(body["error"]["message_key"], "system.storage_unavailable");
    assert_eq!(api.calls(), vec!["time_review", "task_detail"]);
}

#[tokio::test]
async fn time_review_missing_task_maps_to_404_not_no_held() {
    // 服务层对「任务不存在」同样报 InvalidArgument（与 no_held 共用变体），
    // handler 必须以只读 task_detail 复核区分：NotFound → 标准 404。
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::InvalidArgument);
    fake.detail_failure = Some(FakeFailure::NotFound);
    let api = Arc::new(fake);
    let task_id = api.detail.id;
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/time-review"),
            r#"{"expected_revision":"3","decision":"expire","evidence":"inconclusive"}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "任务不存在必须 404 而非 400");
    assert_eq!(body["error"]["code"], "NOT_FOUND");
    assert_eq!(body["error"]["message_key"], "system.not_found");
    assert_eq!(api.calls(), vec!["time_review", "task_detail"]);
}

#[tokio::test]
async fn release_lock_projects_only_two_fields_and_requires_admin() {
    let body = r#"{"expected_revision":"4","acknowledge_unknown":true,"reason":"现场已人工核实"}"#;
    let subtask_id = Uuid::new_v4();

    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            &format!("/api/v1/subtasks/{subtask_id}/release-lock"),
            body,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, value) = envelope(response).await;
    assert_eq!(status, StatusCode::OK);
    let data = &value["data"];
    let keys: Vec<&str> = data
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap();
    assert_eq!(keys.len(), 2, "release-lock 响应必须恰两字段: {data}");
    assert_eq!(data["released"], true);
    assert_eq!(data["state"], "unknown");

    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            &format!("/api/v1/subtasks/{subtask_id}/release-lock"),
            body,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, value) = envelope(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(value["error"]["code"], "PERMISSION_DENIED");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn release_lock_requires_ack_and_nonempty_reason() {
    let subtask_id = Uuid::new_v4();
    for (body, key) in [
        (
            r#"{"expected_revision":"4","acknowledge_unknown":false,"reason":"ok"}"#,
            "task.ack_required",
        ),
        (
            r#"{"expected_revision":"4","acknowledge_unknown":true,"reason":"   "}"#,
            "task.reason_required",
        ),
    ] {
        let api = Arc::new(FakeTaskApi::new());
        let response = app(api.clone(), true, false, true)
            .oneshot(post_request(
                &format!("/api/v1/subtasks/{subtask_id}/release-lock"),
                body,
                true,
                None,
            ))
            .await
            .unwrap();
        let (status, value) = envelope(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} 必须 400");
        assert_eq!(value["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(value["error"]["message_key"], key);
        assert!(api.calls().is_empty(), "{key} 不得触达业务仓储");
    }
}

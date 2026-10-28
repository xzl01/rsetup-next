//! 读端点（detail / children / list）集成测试（5 例）。

use std::sync::Arc;

use axum::http::StatusCode;
use tower::ServiceExt;

use super::fakes::{FakeFailure, FakeTaskApi, app, envelope, get_request};

#[tokio::test]
async fn task_detail_projects_authorized_subset_with_decimal_counts() {
    let api = Arc::new(FakeTaskApi::new());
    let task_id = api.detail.id;
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request(&format!("/api/v1/tasks/{task_id}"), true))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["id"], task_id.to_string());
    assert_eq!(data["operation"], "reboot");
    assert_eq!(data["view_scope"], "authorized_subset");
    assert_eq!(data["revision"], "4");
    assert_eq!(data["counts"]["succeeded"], "1");
    assert_eq!(data["counts"]["unknown"], "1");
    assert_eq!(data["counts"]["expired"], "0");
    assert!(data.get("total_targets").is_none());
    assert!(data.get("hidden_count").is_none());
    assert!(data["created_time"]["quality"].is_string());
    assert_eq!(api.calls(), vec!["task_detail"]);
}

#[tokio::test]
async fn task_detail_not_found_maps_to_404() {
    let mut fake = FakeTaskApi::new();
    fake.failure = Some(FakeFailure::NotFound);
    let api = Arc::new(fake);
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request(
            "/api/v1/tasks/7c9e6679-7425-40de-944b-e07fc1f90ae7",
            true,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NOT_FOUND");
    assert_eq!(body["error"]["message_key"], "system.not_found");
    assert_eq!(api.calls(), vec!["task_detail"]);
}

#[tokio::test]
async fn invalid_path_id_is_400_before_business() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request("/api/v1/tasks/not-a-uuid", true))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn task_children_page_has_no_view_scope_or_counts() {
    let api = Arc::new(FakeTaskApi::new());
    let task_id = api.detail.id;
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request(
            &format!("/api/v1/tasks/{task_id}/children"),
            true,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert!(data.get("view_scope").is_none());
    assert!(data.get("counts").is_none());
    assert!(data["next_cursor"].is_null());
    let items = data["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "不可见目标既不返回也不计数");
    assert_eq!(items[0]["revision"], "3");
    assert_eq!(items[0]["result_evidence"], "boot_transition_observed");
    assert_eq!(api.calls(), vec!["task_children"]);
}

#[tokio::test]
async fn list_tasks_projects_items_and_rejects_invalid_limit() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), false, false, true)
        .oneshot(get_request("/api/v1/tasks?limit=50&state=completed", true))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["data"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["counts"]["succeeded"], "1");
    assert_eq!(items[0]["counts"]["queued"], "0");

    for uri in [
        "/api/v1/tasks?limit=0",
        "/api/v1/tasks?limit=201",
        "/api/v1/tasks?evil=1",
    ] {
        let api = Arc::new(FakeTaskApi::new());
        let response = app(api.clone(), false, false, true)
            .oneshot(get_request(uri, true))
            .await
            .unwrap();
        let (status, body) = envelope(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 必须 400");
        assert_eq!(body["error"]["code"], "INVALID_ARGUMENT", "{uri}");
        assert!(api.calls().is_empty(), "{uri} 不得触达业务仓储");
    }
}

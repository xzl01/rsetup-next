//! 任务预览与任务创建端点集成测试（8 例）。

use std::sync::Arc;

use axum::http::StatusCode;
use tower::ServiceExt;

use super::fakes::{
    CREATE_TASK_BODY, FakeTaskApi, app, app_with_recovery, envelope, get_request, post_request,
};
use crate::operations::{RecoveryMode, RuntimeRecoveryController};
use crate::tasks::http::fixtures::IDEMPOTENCY_KEY;

#[tokio::test]
async fn create_preview_projects_token_expiry_and_targets() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            "/api/v1/task-previews",
            &format!(
                r#"{{"operation":"reboot","device_ids":["{}"],"group_ids":[]}}"#,
                "aa".repeat(32)
            ),
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["preview_token"], "cd".repeat(32));
    assert_eq!(data["expires_in_ms"], 60_000);
    assert_eq!(data["targets"][0]["device_id"], "aa".repeat(32));
    assert_eq!(data["targets"][0]["eligible"], true);
    assert!(data["targets"][0]["reason"].is_null());
    assert_eq!(data["targets"][1]["reason"], "DEVICE_BUSY");
    assert_eq!(api.calls(), vec!["preview"]);
}

#[tokio::test]
async fn create_preview_rejects_unknown_operation_with_400() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            "/api/v1/task-previews",
            r#"{"operation":"shutdown","device_ids":["aa"]}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn create_preview_rejects_non_hex_device_id_with_400() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            "/api/v1/task-previews",
            r#"{"operation":"reboot","device_ids":["serial-001"]}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn create_task_requires_canonical_idempotency_key() {
    for (label, key) in [
        ("缺失", None),
        ("非 canonical", Some("3F2504E0-4F89-41D3-9A0C-0305E82C3301")),
    ] {
        let api = Arc::new(FakeTaskApi::new());
        let response = app(api.clone(), true, false, true)
            .oneshot(post_request("/api/v1/tasks", CREATE_TASK_BODY, true, key))
            .await
            .unwrap();
        let (status, body) = envelope(response).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{label} 幂等键必须 400");
        assert_eq!(body["error"]["code"], "INVALID_ARGUMENT", "{label}");
        assert!(api.calls().is_empty(), "{label} 不得触达业务仓储");
    }
}

#[tokio::test]
async fn create_task_requires_confirm_true() {
    let api = Arc::new(FakeTaskApi::new());
    let response = app(api.clone(), true, false, true)
        .oneshot(post_request(
            "/api/v1/tasks",
            r#"{"preview_token":"00","confirm":false}"#,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "INVALID_ARGUMENT");
    assert!(api.calls().is_empty());
}

#[tokio::test]
async fn create_task_returns_202_with_only_task_id_and_accepted() {
    let api = Arc::new(FakeTaskApi::new());
    let task_id = api.detail.id;
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
    assert_eq!(status, StatusCode::ACCEPTED);
    let data = &body["data"];
    let keys: Vec<&str> = data
        .as_object()
        .map(|o| o.keys().map(String::as_str).collect())
        .unwrap();
    assert_eq!(keys.len(), 2, "创建响应必须恰为 task_id+accepted: {data}");
    assert_eq!(data["task_id"], task_id.to_string());
    assert_eq!(data["accepted"], true);
    assert_eq!(api.calls(), vec!["submit"]);
}

/// 受控恢复模式下的提交闸门（05 §9「禁止变更任务调度/提交」）：503
/// `CONTROLLED_RECOVERY_ACTIVE`，且**绝不触达** `TaskApi::submit`
/// （不落库、不下发、不发事件）。
#[tokio::test]
async fn create_task_returns_503_when_controlled_recovery_active() {
    let api = Arc::new(FakeTaskApi::new());
    let recovery = Arc::new(RuntimeRecoveryController::new());
    recovery.enter_controlled_recovery();
    let response = app_with_recovery(api.clone(), recovery)
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, body) = envelope(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "CONTROLLED_RECOVERY_ACTIVE");
    assert_eq!(
        body["error"]["message_key"],
        "task.controlled_recovery_active"
    );
    // 去敏：只暴露线上契约的模式字符串，不含设备、任务或凭据信息。
    assert_eq!(
        body["error"]["params"]["recovery_mode"],
        "controlled_recovery"
    );
    assert!(
        api.calls().is_empty(),
        "受控模式下 submit 必须零调用: {:?}",
        api.calls()
    );
}

/// 闸门的边界与时效：受控模式只挡变更任务提交，只读列表与预览照旧放行；
/// 管理员解除受控模式后，**同一路由实例**的下一条提交立即放行（模式不缓存）。
#[tokio::test]
async fn controlled_recovery_gate_spares_reads_and_reopens_after_exit() {
    use crate::operations::RecoveryExitChecklist;

    let api = Arc::new(FakeTaskApi::new());
    let recovery = Arc::new(RuntimeRecoveryController::new());
    recovery.enter_controlled_recovery();
    let router = app_with_recovery(api.clone(), Arc::clone(&recovery));

    let response = router
        .clone()
        .oneshot(get_request("/api/v1/tasks", true))
        .await
        .unwrap();
    let (status, _) = envelope(response).await;
    assert_eq!(status, StatusCode::OK, "受控模式下任务列表必须仍可读");

    let response = router
        .clone()
        .oneshot(post_request(
            "/api/v1/task-previews",
            &format!(
                r#"{{"operation":"reboot","device_ids":["{}"],"group_ids":[]}}"#,
                "aa".repeat(32)
            ),
            true,
            None,
        ))
        .await
        .unwrap();
    let (status, _) = envelope(response).await;
    assert_eq!(status, StatusCode::OK, "预览是只读资格投影，不创建任务");

    let before = api.calls();
    assert!(
        !before.contains(&"submit"),
        "受控模式下 submit 不得被调用: {before:?}"
    );

    recovery
        .exit_controlled_recovery(&RecoveryExitChecklist {
            revocations_checked: true,
            grants_checked: true,
            passwords_checked: true,
            tasks_checked: true,
        })
        .expect("默认观察窗为 0，四项核对齐备即可解除");
    assert_eq!(recovery.current_mode(), RecoveryMode::Normal);

    let response = router
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    let (status, _) = envelope(response).await;
    assert_eq!(status, StatusCode::ACCEPTED, "解除受控模式后提交即放行");
    assert!(api.calls().contains(&"submit"));
}

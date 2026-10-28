//! 任务域路由集成测试：`task.updated` 业务事件发布。

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::events::{EventAudience, EventBus, EventEnvelope, EventName, EventObjectAudience};
use crate::tasks::http::contract::CancelTaskResponse;
use crate::tasks::http::fixtures::{FIXTURE_USER_ID, IDEMPOTENCY_KEY};

use super::fakes::{
    CREATE_TASK_BODY, FakeFailure, FakeTaskApi, app, app_with_events, envelope, post_request,
};

/// handler 在返回响应之前完成发布，故此处同步排空即可（无需 await）。
fn collect_events(rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>) -> Vec<EventEnvelope> {
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    events
}

#[tokio::test]
async fn task_operations_publish_task_updated_events_when_event_bus_is_configured() {
    let bus = EventBus::new();
    let mut rx = bus.subscribe();
    let api = Arc::new(FakeTaskApi::new());
    let task_id = api.detail.id;

    // 1) 普通用户创建：本人受众 + 管理员受众各一条，载荷形状一致。
    let response = app_with_events(api.clone(), false, false, true, Some(bus.clone()))
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    assert_eq!(envelope(response).await.0, StatusCode::ACCEPTED);
    let events = collect_events(&mut rx);
    assert_eq!(
        events.len(),
        2,
        "普通用户写操作 = 创建者本人 + 管理员各一条"
    );
    for event in &events {
        assert_eq!(event.name(), EventName::TaskUpdated);
        assert_eq!(event.data()["kind"], json!("task"));
        assert_eq!(event.data()["id"], json!(task_id.to_string()));
        // 新建主任务的持久 revision 恒 1，且必须是十进制字符串。
        assert_eq!(event.data()["revision"], json!("1"));
        assert!(event.data()["revision"].is_string());
    }
    assert_eq!(events[0].audience(), EventAudience::User(FIXTURE_USER_ID));
    assert_eq!(events[1].audience(), EventAudience::Admins);

    // 2) 管理员创建：本人受众已覆盖其会话，不重复投管理员受众。
    let response = app_with_events(api.clone(), true, false, true, Some(bus.clone()))
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    assert_eq!(envelope(response).await.0, StatusCode::ACCEPTED);
    let events = collect_events(&mut rx);
    assert_eq!(events.len(), 1, "管理员写操作只发一条，不得重复投递");
    assert_eq!(events[0].audience(), EventAudience::User(FIXTURE_USER_ID));

    // 3) 取消：`expected_revision` 即取消前主任务 revision，事件按 +1 下界发布。
    let response = app_with_events(api.clone(), false, false, true, Some(bus.clone()))
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/cancel"),
            r#"{"expected_revision":"3","confirm":true}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(envelope(response).await.0, StatusCode::OK);
    let events = collect_events(&mut rx);
    assert_eq!(events.len(), 2);
    for event in &events {
        assert_eq!(event.name(), EventName::TaskUpdated);
        assert_eq!(event.data()["id"], json!(task_id.to_string()));
        assert_eq!(event.data()["revision"], json!("4"), "3 + 1 的单调下界");
    }
    assert_eq!(events[0].audience(), EventAudience::User(FIXTURE_USER_ID));
    assert_eq!(events[1].audience(), EventAudience::Admins);

    // 4) 时限复核（管理员专属）：只投管理员受众，revision 用服务层回读值。
    let response = app_with_events(api.clone(), true, false, true, Some(bus.clone()))
        .oneshot(post_request(
            &format!("/api/v1/tasks/{task_id}/time-review"),
            r#"{"expected_revision":"3","decision":"resume_original_deadline","evidence":"reference_deadline_proven"}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(envelope(response).await.0, StatusCode::OK);
    let events = collect_events(&mut rx);
    assert_eq!(events.len(), 1, "时限复核只投管理员受众");
    assert_eq!(events[0].name(), EventName::TaskUpdated);
    assert_eq!(events[0].audience(), EventAudience::Admins);
    assert_eq!(events[0].data()["id"], json!(task_id.to_string()));
    assert_eq!(
        events[0].data()["revision"],
        json!("4"),
        "fake 服务层回读的主任务 revision"
    );

    // 5) release-lock（管理员专属）：只投管理员受众，对象身份为子任务 id。
    let subtask_id = Uuid::new_v4();
    let response = app_with_events(api.clone(), true, false, true, Some(bus.clone()))
        .oneshot(post_request(
            &format!("/api/v1/subtasks/{subtask_id}/release-lock"),
            r#"{"expected_revision":"3","acknowledge_unknown":true,"reason":"现场已人工核实"}"#,
            true,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(envelope(response).await.0, StatusCode::OK);
    let events = collect_events(&mut rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name(), EventName::TaskUpdated);
    assert_eq!(events[0].audience(), EventAudience::Admins);
    assert_eq!(events[0].data()["id"], json!(subtask_id.to_string()));
    assert_eq!(events[0].data()["revision"], json!("4"));

    // 6) 失败响应零发布：幂等冲突（409）不改变任何任务状态。
    let mut failing = FakeTaskApi::new();
    failing.failure = Some(FakeFailure::IdempotencyConflict);
    let failing: Arc<FakeTaskApi> = Arc::new(failing);
    let response = app_with_events(failing, true, false, true, Some(bus.clone()))
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    assert_eq!(envelope(response).await.0, StatusCode::CONFLICT);
    assert!(
        collect_events(&mut rx).is_empty(),
        "写操作失败不得发任务变化提示"
    );

    // 7) 未配置事件总线（events = None）：响应不变、零发布。
    let response = app(api.clone(), false, false, true)
        .oneshot(post_request(
            "/api/v1/tasks",
            CREATE_TASK_BODY,
            true,
            Some(IDEMPOTENCY_KEY),
        ))
        .await
        .unwrap();
    assert_eq!(envelope(response).await.0, StatusCode::ACCEPTED);
    assert!(
        collect_events(&mut rx).is_empty(),
        "events = None 不得发布任何事件"
    );
    assert!(api.calls().contains(&"submit"));
}

/// 零目标取消不发布（审查反馈 Medium-2）：`cancelled` 为 `None` 或空数组表示本次没有
/// 任何目标进入取消结果（HTTP 契约把零目标投影成 `None`），主任务状态未变，不得用
/// `task.updated` 打扰客户端。同一条用例顺带钉住饱和下界：`expected_revision` 取
/// `u64::MAX` 也不得 panic 或回绕。
#[tokio::test]
async fn task_cancel_without_cancelled_targets_publishes_no_task_updated() {
    for cancelled in [None, Some(vec![])] {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let mut fake = FakeTaskApi::new();
        fake.cancel = CancelTaskResponse {
            accepted: true,
            cancelled: cancelled.clone(),
        };
        let api: Arc<FakeTaskApi> = Arc::new(fake);
        let task_id = api.detail.id;
        let response = app_with_events(api, false, false, true, Some(bus.clone()))
            .oneshot(post_request(
                &format!("/api/v1/tasks/{task_id}/cancel"),
                r#"{"expected_revision":"18446744073709551615","confirm":true}"#,
                true,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(envelope(response).await.0, StatusCode::OK);
        assert!(
            collect_events(&mut rx).is_empty(),
            "未取消任何目标不得发 task.updated（cancelled = {cancelled:?}）"
        );
        // 反向证据：同一订阅者确实能收到事件，说明上面的 0 条是「未发布」而非「订阅失效」。
        assert_eq!(
            bus.publish(EventEnvelope::task_updated(
                &task_id.to_string(),
                1,
                EventObjectAudience::admins(),
            )),
            1
        );
    }
}

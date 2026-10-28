//! 任务域 HTTP 端点（Runtime Task 6）：8 条任务路由、统一门禁顺序、
//! 授权子集投影与统一错误映射。
//!
//! 本模块只提供**组件子 router** [`build_tasks_router`]，不改 `lib.rs`/`main.rs`：
//! 顶层生产装配（合并三子 router、路径集互斥、不重复注册 `/healthz`/`/readyz`）
//! 归唯一独立集成任务所有者。
//!
//! 固定 handler 顺序（8 条路由一致，复用 Identity 共享 gate，本模块**不自写**
//! Host/Cookie/CSRF）：①`authenticate_read`/`authenticate_write`（Host → Origin →
//! 严格单值会话 Cookie → 活会话预检 → 权威 authenticate → CSRF → 限速）→
//! ②[`require_business_session`]（临时密码 403）→ ③body/参数（`read_json_body`
//! ≤1 MiB + `deny_unknown_fields`、`parse_decimal_u64`、`parse_idempotency_key`、
//! `clamp_page_limit`）→ ④管理员前置（**仅** time-review / release-lock）→
//! ⑤[`TaskApi`] → ⑥[`task_error_response`] 统一映射。
//!
//! 业务事件（06 §4 `task.updated`）：`state.events` 配置了 [`EventBus`] 时，改变
//! 任务状态的 4 个写端点（create / cancel / time-review / release-lock）在各自
//! 成功分支返回响应之前发布一次变化提示；受众由 [`HttpTasksState`] 的发布入口
//! 如实标注（创建者本人受众，操作者非管理员时再补投一份管理员受众），失败响应
//! 零发布，`events = None` 时与接入事件总线之前行为完全一致。事件只是变化提示，
//! 权威事实始终来自 GET 快照。

use axum::Router;
use axum::routing::{get, post};

#[cfg(doc)]
use crate::events::EventBus;

mod cursor;
pub use cursor::{TaskCursorScope, decode_task_cursor, encode_task_cursor};

mod contract;
pub use contract::{
    CancelTaskRequest, CancelTaskResponse, CancelledTarget, ChildEntry, ChildItem, ChildrenPage,
    CreatePreviewRequest, CreateTaskRequest, ReleaseLockRequest, ReleaseLockResponse, TaskActions,
    TaskApi, TaskCounts, TaskDetail, TaskFuture, TaskOperation, TaskPage, TimeReviewDecision,
    TimeReviewTaskRequest, ViewScope,
};

mod state;
pub use state::HttpTasksState;

mod gate;
pub(crate) use gate::{
    controlled_recovery_response, require_business_session, task_error_response,
};

mod query;
pub use query::{
    DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, clamp_page_limit, parse_decimal_u64, parse_idempotency_key,
};

mod projection;
pub use projection::{
    cancel_permitted, project_cancel_result, project_main_task_summary, project_task_children,
    release_lock_permitted, time_review_permitted,
};

mod routes;

#[cfg(test)]
mod fixtures;

/// 任务子路由：8 条端点对齐 Web 专题白名单。
pub fn build_tasks_router(state: HttpTasksState) -> Router<()> {
    Router::new()
        .route("/api/v1/task-previews", post(routes::create_preview))
        .route(
            "/api/v1/tasks",
            post(routes::create_task).get(routes::list_tasks),
        )
        .route("/api/v1/tasks/{id}", get(routes::task_detail))
        .route("/api/v1/tasks/{id}/children", get(routes::task_children))
        .route("/api/v1/tasks/{id}/cancel", post(routes::cancel_task))
        .route("/api/v1/tasks/{id}/time-review", post(routes::time_review))
        .route(
            "/api/v1/subtasks/{id}/release-lock",
            post(routes::release_lock),
        )
        .with_state(state)
}

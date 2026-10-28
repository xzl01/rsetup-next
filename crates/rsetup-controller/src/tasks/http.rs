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
use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::ControllerError;
#[cfg(doc)]
use crate::events::EventBus;
use crate::events::{EventEnvelope, EventObjectAudience};
use crate::http_api::{err_response, ok_response, request_id};
use crate::http_auth::EndpointId;
use crate::http_gate::{
    authenticate_read, authenticate_write, read_json_body, require_admin_session, single_header,
};
use crate::parse_device_id;
use crate::tasks::model::{MainTaskState, SubTaskState};
use crate::tasks::service::SubmitOutcome;

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

mod routes {
    use super::projection::strict_json;
    use super::query::{IDEMPOTENCY_KEY_HEADER, parse_path_id, preview_expires_in_ms};
    use super::*;

    /// 分页查询参数（`/tasks` 另带 `state`）：缺省即"未提供该查询键"。
    #[derive(Default)]
    struct PageQuery {
        limit: Option<usize>,
        cursor: Option<String>,
        state: Option<MainTaskState>,
    }

    /// 严格查询串解析：总长 ≤512、禁 `%`、只允许已知键、禁重复键、禁空值；
    /// `limit` 纯数字且无前导零，`cursor` 恰 64 小写 hex，`state` 四值 snake_case。
    fn parse_page_query(
        raw: Option<&str>,
        allow_state: bool,
    ) -> Result<PageQuery, ControllerError> {
        let mut query = PageQuery::default();
        let Some(raw) = raw.filter(|value| !value.is_empty()) else {
            return Ok(query);
        };
        if raw.len() > 512 || raw.contains('%') {
            return Err(ControllerError::InvalidArgument);
        }
        for part in raw.split('&') {
            let (key, value) = part
                .split_once('=')
                .ok_or(ControllerError::InvalidArgument)?;
            if value.is_empty() {
                return Err(ControllerError::InvalidArgument);
            }
            match key {
                "limit" => {
                    if query.limit.is_some()
                        || !value.bytes().all(|b| b.is_ascii_digit())
                        || (value.len() > 1 && value.starts_with('0'))
                    {
                        return Err(ControllerError::InvalidArgument);
                    }
                    query.limit = Some(
                        value
                            .parse::<usize>()
                            .map_err(|_| ControllerError::InvalidArgument)?,
                    );
                }
                "cursor" => {
                    if query.cursor.is_some()
                        || value.len() != 64
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    {
                        return Err(ControllerError::InvalidArgument);
                    }
                    query.cursor = Some(value.to_owned());
                }
                "state" => {
                    if !allow_state || query.state.is_some() {
                        return Err(ControllerError::InvalidArgument);
                    }
                    query.state = Some(
                        serde_json::from_value::<MainTaskState>(Value::String(value.to_owned()))
                            .map_err(|_| ControllerError::InvalidArgument)?,
                    );
                }
                _ => return Err(ControllerError::InvalidArgument),
            }
        }
        Ok(query)
    }

    /// 详情与列表项共用同一投影：`is_full_access` 取服务层 `view_scope`。
    fn project_detail(detail: &TaskDetail) -> Result<Value, ControllerError> {
        project_main_task_summary(
            detail.id,
            detail.operation,
            detail.state,
            detail.outcome,
            detail.revision,
            &detail.created_time,
            &detail.visible_states,
            detail.actions.can_cancel,
            matches!(detail.view_scope, ViewScope::Full),
        )
    }

    /// `POST /api/v1/task-previews`：签发预览令牌（02 §6）。
    pub async fn create_preview(
        State(state): State<HttpTasksState>,
        headers: HeaderMap,
        body: Body,
    ) -> Response {
        let session = match authenticate_write(&state.auth, &headers).await {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        let request: CreatePreviewRequest = match read_json_body(&headers, body).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let mut device_ids = Vec::with_capacity(request.device_ids.len());
        for raw in &request.device_ids {
            match parse_device_id(raw) {
                Ok(device_id) => device_ids.push(device_id),
                Err(err) => return task_error_response(&err),
            }
        }
        match state
            .api
            .preview(&session, &device_ids, &request.group_ids)
            .await
        {
            Ok(outcome) => ok_response(json!({
                "preview_token": outcome.preview_token,
                "expires_in_ms": preview_expires_in_ms(),
                "targets": outcome
                    .targets
                    .iter()
                    .map(|target| json!({
                        "device_id": hex::encode(target.device_id),
                        "eligible": target.eligible,
                        "reason": strict_json(&target.reason_code),
                    }))
                    .collect::<Vec<Value>>(),
            })),
            Err(err) => task_error_response(&err),
        }
    }

    /// `POST /api/v1/tasks`：消费预览创建任务（202 仅表示持久接收）。
    ///
    /// 唯一校验 `Idempotency-Key` 的端点（其余写端点靠 `expected_revision` CAS）。
    ///
    /// 受控恢复模式闸门（05 §9「禁止变更任务调度/提交」）：认证与临时密码前置之后、
    /// 解析任何请求体之前判定，处于受控恢复模式即 503 `CONTROLLED_RECOVERY_ACTIVE`
    /// 且**绝不触达** [`TaskApi::submit`]（不落库、不下发、不发事件）。放在认证之后
    /// 保证未认证请求仍是 401，管理/只读类操作（登录、状态查看、单设备只读核验、
    /// 任务列表与详情）不受本闸门影响。
    pub async fn create_task(
        State(state): State<HttpTasksState>,
        headers: HeaderMap,
        body: Body,
    ) -> Response {
        let session = match authenticate_write(&state.auth, &headers).await {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        if state.blocks_mutation_task() {
            return controlled_recovery_response();
        }
        let idempotency_key =
            match parse_idempotency_key(single_header(&headers, IDEMPOTENCY_KEY_HEADER)) {
                Ok(key) => key,
                Err(err) => return task_error_response(&err),
            };
        let request: CreateTaskRequest = match read_json_body(&headers, body).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        if !request.confirm {
            return err_response(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "task.confirm_required",
                json!({}),
            );
        }
        match state
            .api
            .submit(&session, &request.preview_token, &idempotency_key)
            .await
        {
            Ok(outcome) => {
                let task_id = match outcome {
                    SubmitOutcome::Created { main_task_id }
                    | SubmitOutcome::Replayed { main_task_id } => main_task_id,
                };
                // 新建主任务的持久 revision 恒为 1（Task 4 submit 的首个版本）；
                // 幂等重放只是同一主任务的重投，仍按同一 revision 发变化提示。
                state.publish_task_update(
                    &task_id.to_string(),
                    1,
                    session.user.id,
                    session.user.is_admin,
                );
                accepted_response(json!({ "task_id": task_id.to_string(), "accepted": true }))
            }
            Err(err) => task_error_response(&err),
        }
    }

    /// `GET /api/v1/tasks`：主任务分页（授权子集投影与详情同形）。
    pub async fn list_tasks(
        State(state): State<HttpTasksState>,
        raw_query: RawQuery,
        headers: HeaderMap,
    ) -> Response {
        let session = match authenticate_read(&state.auth, &headers, EndpointId::TaskList).await {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        let query = match parse_page_query(raw_query.0.as_deref(), true) {
            Ok(query) => query,
            Err(err) => return task_error_response(&err),
        };
        let limit = match clamp_page_limit(query.limit) {
            Ok(limit) => limit,
            Err(err) => return task_error_response(&err),
        };
        match state
            .api
            .list_tasks(&session, query.state, query.cursor.as_deref(), limit)
            .await
        {
            Ok(page) => {
                let mut items = Vec::with_capacity(page.items.len());
                for detail in &page.items {
                    match project_detail(detail) {
                        Ok(view) => items.push(view),
                        Err(err) => return task_error_response(&err),
                    }
                }
                ok_response(json!({ "items": items, "next_cursor": page.next_cursor }))
            }
            Err(err) => task_error_response(&err),
        }
    }

    /// `GET /api/v1/tasks/{id}`：主任务授权视图（state/outcome/counts 由可见全集重算）。
    pub async fn task_detail(
        State(state): State<HttpTasksState>,
        Path(raw_id): Path<String>,
        headers: HeaderMap,
    ) -> Response {
        let session = match authenticate_read(&state.auth, &headers, EndpointId::TaskDetail).await {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        let task_id = match parse_path_id(&raw_id) {
            Ok(task_id) => task_id,
            Err(err) => return task_error_response(&err),
        };
        match state.api.task_detail(&session, task_id).await {
            Ok(detail) => match project_detail(&detail) {
                Ok(view) => ok_response(view),
                Err(err) => task_error_response(&err),
            },
            Err(err) => task_error_response(&err),
        }
    }

    /// `GET /api/v1/tasks/{id}/children`：可见子任务分页（与 main counts 完全独立）。
    pub async fn task_children(
        State(state): State<HttpTasksState>,
        Path(raw_id): Path<String>,
        raw_query: RawQuery,
        headers: HeaderMap,
    ) -> Response {
        let session = match authenticate_read(&state.auth, &headers, EndpointId::TaskChildren).await
        {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        let task_id = match parse_path_id(&raw_id) {
            Ok(task_id) => task_id,
            Err(err) => return task_error_response(&err),
        };
        let query = match parse_page_query(raw_query.0.as_deref(), false) {
            Ok(query) => query,
            Err(err) => return task_error_response(&err),
        };
        let limit = match clamp_page_limit(query.limit) {
            Ok(limit) => limit,
            Err(err) => return task_error_response(&err),
        };
        match state
            .api
            .task_children(&session, task_id, query.cursor.as_deref(), limit)
            .await
        {
            Ok(page) => ok_response(project_task_children(
                &page.items,
                page.next_cursor.as_deref(),
            )),
            Err(err) => task_error_response(&err),
        }
    }

    /// `POST /api/v1/tasks/{id}/cancel`：CAS 取消 + 逐目标可读过滤。
    pub async fn cancel_task(
        State(state): State<HttpTasksState>,
        Path(raw_id): Path<String>,
        headers: HeaderMap,
        body: Body,
    ) -> Response {
        let session = match authenticate_write(&state.auth, &headers).await {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        let task_id = match parse_path_id(&raw_id) {
            Ok(task_id) => task_id,
            Err(err) => return task_error_response(&err),
        };
        let request: CancelTaskRequest = match read_json_body(&headers, body).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let expected_revision = match parse_decimal_u64(&request.expected_revision) {
            Ok(revision) => revision,
            Err(err) => return task_error_response(&err),
        };
        if !request.confirm {
            return err_response(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "task.confirm_required",
                json!({}),
            );
        }
        match state.api.cancel(&session, task_id, request).await {
            Ok(response) => {
                // 只有**确实取消了目标**才发变化提示：HTTP 契约把「本事务没有取消任何
                // 目标」（含无可读目标）投影成 `cancelled: None`（见 `project_cancel_result`，
                // 绝不为 `Some([])`），此时没有任何值得通知的状态变化，发 `task.updated`
                // 只是让客户端白跑一次重取。
                //
                // HTTP 契约的 `expected_revision` 即取消前的主任务 revision，服务层
                // 每取消成功一个目标都把主行 revision +1（`sync_main_task_summary`）：
                // 本层拿不到逐目标结果与最终 revision，故按 **+1 的饱和单调下界**发提示
                // （revision 只用于同一 id 内的新旧比较，客户端仍以 GET 为权威）。
                if response
                    .cancelled
                    .as_deref()
                    .is_some_and(|cancelled| !cancelled.is_empty())
                {
                    state.publish_task_update(
                        &task_id.to_string(),
                        expected_revision.saturating_add(1),
                        session.user.id,
                        session.user.is_admin,
                    );
                }
                ok_response(strict_json(&response))
            }
            Err(err) => task_error_response(&err),
        }
    }

    /// `POST /api/v1/tasks/{id}/time-review`：管理员时限复核，返回当前授权视图。
    pub async fn time_review(
        State(state): State<HttpTasksState>,
        Path(raw_id): Path<String>,
        headers: HeaderMap,
        body: Body,
    ) -> Response {
        let session = match authenticate_write(&state.auth, &headers).await {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        let task_id = match parse_path_id(&raw_id) {
            Ok(task_id) => task_id,
            Err(err) => return task_error_response(&err),
        };
        let request: TimeReviewTaskRequest = match read_json_body(&headers, body).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        if let Err(err) = parse_decimal_u64(&request.expected_revision) {
            return task_error_response(&err);
        }
        if let Err(response) = require_admin_session(&session) {
            return response;
        }
        match state.api.time_review(&session, task_id, request).await {
            Ok(detail) => match project_detail(&detail) {
                Ok(view) => {
                    // 时限复核是管理员专属写操作：投影成功（即 200）后向管理员范围投
                    // 递 `task.updated`，revision 用服务层回读的当前主任务 revision
                    // （精确值，不是下界）；创建者不是本次操作者，其可见性由其自身的
                    // 后续 GET/自身写操作的提示覆盖，不在此跨受众广播。
                    if let Some(bus) = state.events.as_ref() {
                        bus.publish(EventEnvelope::task_updated(
                            &task_id.to_string(),
                            detail.revision,
                            EventObjectAudience::admins(),
                        ));
                    }
                    ok_response(view)
                }
                Err(err) => task_error_response(&err),
            },
            // 本端点 DTO 已拒绝未知字段与未知枚举；服务侧 `InvalidArgument` 仍有三
            // 个业务成因——「任务不存在」「主任务无 held 子任务」与「决策与现状矛
            // 盾」（Task 4 `time_review_in_tx` 多处分支共用同一变体，service.rs 越
            // 出本任务写作用域，HTTP 层以只读 `task_detail` 复核区分）：
            // - 复核 Ok 且可见全集仍含 held → 任务确有 held 子任务，属矛盾等非法
            //   请求 → 标准 400 `task.invalid_request`，不得谎报 `task.no_held`；
            // - 复核 Ok 且无 held → 400 `task.no_held`；
            // - 复核 Err（NotFound → 404；Database 等 → 503）→ 原样透传，
            //   不得把复核自身的失败吞没成 400。
            Err(ControllerError::InvalidArgument) => {
                match state.api.task_detail(&session, task_id).await {
                    Ok(detail) => {
                        if detail.visible_states.contains(&SubTaskState::Held) {
                            task_error_response(&ControllerError::InvalidArgument)
                        } else {
                            err_response(
                                StatusCode::BAD_REQUEST,
                                "INVALID_ARGUMENT",
                                "task.no_held",
                                json!({}),
                            )
                        }
                    }
                    Err(err) => task_error_response(&err),
                }
            }
            Err(err) => task_error_response(&err),
        }
    }

    /// `POST /api/v1/subtasks/{id}/release-lock`：管理员风险确认释放占用锁。
    pub async fn release_lock(
        State(state): State<HttpTasksState>,
        Path(raw_id): Path<String>,
        headers: HeaderMap,
        body: Body,
    ) -> Response {
        let session = match authenticate_write(&state.auth, &headers).await {
            Ok(session) => session,
            Err(response) => return response,
        };
        if let Err(response) = require_business_session(&session) {
            return response;
        }
        let subtask_id = match parse_path_id(&raw_id) {
            Ok(subtask_id) => subtask_id,
            Err(err) => return task_error_response(&err),
        };
        let request: ReleaseLockRequest = match read_json_body(&headers, body).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        if !request.acknowledge_unknown {
            return err_response(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "task.ack_required",
                json!({}),
            );
        }
        if request.reason.trim().is_empty() {
            return err_response(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "task.reason_required",
                json!({}),
            );
        }
        let expected_revision = match parse_decimal_u64(&request.expected_revision) {
            Ok(revision) => revision,
            Err(err) => return task_error_response(&err),
        };
        if let Err(response) = require_admin_session(&session) {
            return response;
        }
        match state.api.release_lock(&session, subtask_id, request).await {
            Ok(response) => {
                // 释放成功即该子任务的代际锁已删除、子任务 revision 已 CAS +1
                // （服务层 `ReleaseLockOutcome::Released { revision = expected + 1 }`）。
                // 本层 HTTP 契约（`ReleaseLockResponse` 只有 `released`/`state`）不携带
                // 主任务 id，也不携带 revision，故此处能如实标注的对象身份只有路径上的
                // 子任务 id、版本只有 CAS 后的新值；主任务 id 需扩 `ReleaseLockResponse`
                // 才能带上，那属服务层契约变更，不在本文件作用域内。
                if let Some(bus) = state.events.as_ref().filter(|_| response.released) {
                    bus.publish(EventEnvelope::task_updated(
                        &subtask_id.to_string(),
                        expected_revision.saturating_add(1),
                        EventObjectAudience::admins(),
                    ));
                }
                ok_response(strict_json(&response))
            }
            Err(err) => task_error_response(&err),
        }
    }

    /// 202 成功信封：与 `ok_response` 同形状（`{data, request_id}`），仅状态码不同
    /// （02 §3「任务创建 202 仅表示持久接收」）。既有 `ok_response` 固定 200 且
    /// `src/http_api.rs` 不在本任务写作用域内，故在路由层按同一契约构造。
    fn accepted_response(data: Value) -> Response {
        (
            StatusCode::ACCEPTED,
            axum::Json(json!({ "data": data, "request_id": request_id() })),
        )
            .into_response()
    }
}
#[cfg(test)]
mod tests {
    use super::fixtures::{IDEMPOTENCY_KEY, RAW_TOKEN, fixture_detail, fixture_session};
    use super::*;
    use crate::auth::service::{Login, Session, SessionPage};
    use crate::events::{EventAudience, EventBus, EventEnvelope, EventName};
    use crate::http_auth::{
        AuthFuture, AuthServiceApi, CsrfBinding, HttpAuthState, SessionAuthority,
    };
    use crate::http_security::HttpSecurityConfig;
    use crate::operations::{RecoveryMode, RuntimeRecoveryController};
    use crate::tasks::model::{EvidenceKind, ReasonCode};
    use crate::tasks::preview::{PreviewOutcome, PreviewTarget};
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Instant;
    use tower::ServiceExt;
    use uuid::Uuid;

    const CSRF_TOKEN: &str = "csrf-token-fixture";

    // ── 路由集成（fake TaskApi + fake AuthServiceApi + oneshot）────────

    #[derive(Clone, Copy)]
    enum FakeFailure {
        NotFound,
        RevisionConflict,
        PermissionDenied,
        InvalidArgument,
        Database,
        ResourceExhausted,
        IdempotencyConflict,
        PreviewInvalidState,
    }

    impl FakeFailure {
        fn error(self) -> ControllerError {
            match self {
                FakeFailure::NotFound => ControllerError::NotFound,
                FakeFailure::RevisionConflict => ControllerError::RevisionConflict,
                FakeFailure::PermissionDenied => ControllerError::PermissionDenied,
                FakeFailure::InvalidArgument => ControllerError::InvalidArgument,
                FakeFailure::Database => ControllerError::Database(sqlx::Error::RowNotFound),
                FakeFailure::ResourceExhausted => ControllerError::ResourceExhausted,
                FakeFailure::IdempotencyConflict => ControllerError::TaskIdempotencyConflict,
                FakeFailure::PreviewInvalidState => ControllerError::TaskPreviewInvalidState,
            }
        }
    }

    struct FakeTaskApi {
        calls: Mutex<Vec<&'static str>>,
        failure: Option<FakeFailure>,
        /// 仅作用于 `task_detail` 的失败注入（优先于 `failure`）：用于模拟
        /// 「time_review 报 InvalidArgument 而任务不存在（detail 报 NotFound）」
        /// 的服务层组合。
        detail_failure: Option<FakeFailure>,
        /// 为真时 `task_detail` 忽略 `failure` 注入直接返回 `detail`：用于模拟
        /// 「time_review 报 InvalidArgument 但只读复核成功」的组合（no_held 与
        /// 决策矛盾两条分支）。
        detail_ok: bool,
        detail: TaskDetail,
        page: TaskPage,
        children: ChildrenPage,
        preview: PreviewOutcome,
        submit: SubmitOutcome,
        cancel: CancelTaskResponse,
        release: ReleaseLockResponse,
    }

    impl FakeTaskApi {
        fn new() -> Self {
            let detail = fixture_detail();
            Self {
                calls: Mutex::new(Vec::new()),
                failure: None,
                detail_failure: None,
                detail_ok: false,
                preview: PreviewOutcome {
                    preview_token: "cd".repeat(32),
                    targets: vec![
                        PreviewTarget {
                            device_id: [0xAA; 32],
                            eligible: true,
                            reason_code: None,
                        },
                        PreviewTarget {
                            device_id: [0xBB; 32],
                            eligible: false,
                            reason_code: Some(ReasonCode::DeviceBusy),
                        },
                    ],
                },
                submit: SubmitOutcome::Created {
                    main_task_id: detail.id,
                },
                cancel: CancelTaskResponse {
                    accepted: true,
                    cancelled: Some(vec![detail.id]),
                },
                release: ReleaseLockResponse {
                    released: true,
                    state: SubTaskState::Unknown,
                },
                children: ChildrenPage {
                    items: vec![
                        ChildEntry {
                            id: Uuid::new_v4(),
                            device_id: "aa".repeat(32),
                            state: SubTaskState::Accepted,
                            reason_code: None,
                            revision: 3,
                            result_evidence: Some(EvidenceKind::BootTransitionObserved),
                            eligible: true,
                        },
                        ChildEntry {
                            id: Uuid::new_v4(),
                            device_id: "bb".repeat(32),
                            state: SubTaskState::Succeeded,
                            reason_code: None,
                            revision: 2,
                            result_evidence: None,
                            eligible: false,
                        },
                    ],
                    next_cursor: None,
                },
                detail: detail.clone(),
                page: TaskPage {
                    items: vec![detail],
                    next_cursor: None,
                },
            }
        }

        fn record(&self, name: &'static str) {
            self.calls.lock().unwrap().push(name);
        }

        fn calls(&self) -> Vec<&'static str> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl TaskApi for FakeTaskApi {
        fn preview<'a>(
            &'a self,
            _actor: &'a Session,
            _device_ids: &'a [[u8; 32]],
            _group_ids: &'a [Uuid],
        ) -> TaskFuture<'a, PreviewOutcome> {
            self.record("preview");
            let outcome = self.preview.clone();
            let failure = self.failure.map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(outcome), Err) })
        }

        fn submit<'a>(
            &'a self,
            _actor: &'a Session,
            _preview_token: &'a str,
            _idempotency_key: &'a str,
        ) -> TaskFuture<'a, SubmitOutcome> {
            self.record("submit");
            let outcome = self.submit;
            let failure = self.failure.map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(outcome), Err) })
        }

        fn list_tasks<'a>(
            &'a self,
            _actor: &'a Session,
            _state: Option<MainTaskState>,
            _cursor: Option<&'a str>,
            _limit: usize,
        ) -> TaskFuture<'a, TaskPage> {
            self.record("list_tasks");
            let page = self.page.clone();
            let failure = self.failure.map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(page), Err) })
        }

        fn task_detail<'a>(
            &'a self,
            _actor: &'a Session,
            _task_id: Uuid,
        ) -> TaskFuture<'a, TaskDetail> {
            self.record("task_detail");
            let detail = self.detail.clone();
            let failure = if self.detail_ok {
                None
            } else {
                self.detail_failure.or(self.failure)
            }
            .map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(detail), Err) })
        }

        fn task_children<'a>(
            &'a self,
            _actor: &'a Session,
            _task_id: Uuid,
            _cursor: Option<&'a str>,
            _limit: usize,
        ) -> TaskFuture<'a, ChildrenPage> {
            self.record("task_children");
            let page = self.children.clone();
            let failure = self.failure.map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(page), Err) })
        }

        fn cancel<'a>(
            &'a self,
            _actor: &'a Session,
            _task_id: Uuid,
            _request: CancelTaskRequest,
        ) -> TaskFuture<'a, CancelTaskResponse> {
            self.record("cancel");
            let response = self.cancel.clone();
            let failure = self.failure.map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(response), Err) })
        }

        fn time_review<'a>(
            &'a self,
            _actor: &'a Session,
            _task_id: Uuid,
            _request: TimeReviewTaskRequest,
        ) -> TaskFuture<'a, TaskDetail> {
            self.record("time_review");
            let detail = self.detail.clone();
            let failure = self.failure.map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(detail), Err) })
        }

        fn release_lock<'a>(
            &'a self,
            _actor: &'a Session,
            _subtask_id: Uuid,
            _request: ReleaseLockRequest,
        ) -> TaskFuture<'a, ReleaseLockResponse> {
            self.record("release_lock");
            let response = self.release;
            let failure = self.failure.map(FakeFailure::error);
            Box::pin(async move { failure.map_or(Ok(response), Err) })
        }
    }

    struct FakeAuth {
        session: Session,
    }

    impl AuthServiceApi for FakeAuth {
        fn login<'a>(&'a self, _username: &'a str, _password: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { unreachable!("任务路由不得触达 login") })
        }
        fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
            let session = self.session.clone();
            let live = raw == RAW_TOKEN;
            Box::pin(async move {
                if live {
                    Ok(session)
                } else {
                    Err(ControllerError::InvalidArgument)
                }
            })
        }
        fn change_password<'a>(
            &'a self,
            _session: &'a Session,
            _current: &'a str,
            _new_password: &'a str,
        ) -> AuthFuture<'a, ()> {
            Box::pin(async { unreachable!("任务路由不得触达 change_password") })
        }
        fn logout<'a>(&'a self, _session: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async { unreachable!("任务路由不得触达 logout") })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            Box::pin(async { unreachable!("任务路由不得触达 authz_epoch") })
        }
        fn is_live_session(&self, digest: &[u8; 32]) -> bool {
            digest == &self.session.digest
        }
        fn session_authority<'a>(
            &'a self,
            digest: &'a [u8; 32],
        ) -> AuthFuture<'a, SessionAuthority> {
            // 任务线 fake：权威侧结论与内存时钟同构，管理员标志取 fake 会话用户。
            // 任务路由不建 SSE 流，本实现只为边界完整性存在，刻意不做宽松默认。
            let session_live = self.session.digest == *digest;
            let is_admin = self.session.user.is_admin;
            Box::pin(async move {
                Ok(SessionAuthority {
                    session_live,
                    is_admin,
                    authz_epoch: 0,
                })
            })
        }
        fn list_sessions<'a>(
            &'a self,
            _session: &'a Session,
            _cursor: Option<&'a str>,
            _limit: usize,
        ) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { unreachable!("任务路由不得触达 list_sessions") })
        }
        fn revoke_by_alias<'a>(
            &'a self,
            _session: &'a Session,
            _id: &'a str,
        ) -> AuthFuture<'a, bool> {
            Box::pin(async { unreachable!("任务路由不得触达 revoke_by_alias") })
        }
        fn revoke_others<'a>(&'a self, _session: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async { unreachable!("任务路由不得触达 revoke_others") })
        }
    }

    fn app(
        api: Arc<FakeTaskApi>,
        is_admin: bool,
        must_change_password: bool,
        with_csrf: bool,
    ) -> Router<()> {
        app_with_events(api, is_admin, must_change_password, with_csrf, None)
    }

    /// 同 [`app`]，但可注入事件总线：`None` 即「未开启 SSE 事件线」的装配形态。
    fn app_with_events(
        api: Arc<FakeTaskApi>,
        is_admin: bool,
        must_change_password: bool,
        with_csrf: bool,
        events: Option<EventBus>,
    ) -> Router<()> {
        build_app(api, is_admin, must_change_password, with_csrf, events, None)
    }

    /// 同 [`app`]，但注入受控恢复模式唯一真源（05 §9 提交闸门测试入口）：
    /// 会话为非临时密码的管理员、CSRF 已绑定，排除其它门禁的干扰。
    fn app_with_recovery(
        api: Arc<FakeTaskApi>,
        recovery: Arc<RuntimeRecoveryController>,
    ) -> Router<()> {
        build_app(api, true, false, true, None, Some(recovery))
    }

    #[allow(clippy::too_many_arguments)]
    fn build_app(
        api: Arc<FakeTaskApi>,
        is_admin: bool,
        must_change_password: bool,
        with_csrf: bool,
        events: Option<EventBus>,
        recovery: Option<Arc<RuntimeRecoveryController>>,
    ) -> Router<()> {
        let session = fixture_session(is_admin, must_change_password);
        let auth = Arc::new(HttpAuthState::new(
            Arc::new(FakeAuth {
                session: session.clone(),
            }),
            HttpSecurityConfig {
                allowed_hosts: vec!["localhost".into()],
                allowed_origin: Some("http://localhost".into()),
                trust_tls: false,
            },
        ));
        if with_csrf {
            auth.csrf.lock().unwrap().insert(
                session.digest,
                CsrfBinding {
                    token: CSRF_TOKEN.to_string(),
                    issued_at: Instant::now(),
                },
            );
        }
        build_tasks_router(HttpTasksState {
            api,
            auth,
            events,
            recovery,
        })
    }

    fn get_request(uri: &str, with_cookie: bool) -> Request<Body> {
        let mut builder = Request::builder()
            .method("GET")
            .uri(uri)
            .header("host", "localhost");
        if with_cookie {
            builder = builder.header("cookie", format!("rsc_session={RAW_TOKEN}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    fn post_request(
        uri: &str,
        body: &str,
        with_cookie: bool,
        idempotency: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "localhost")
            .header("origin", "http://localhost")
            .header("content-type", "application/json")
            .header("x-csrf-token", CSRF_TOKEN);
        if with_cookie {
            builder = builder.header("cookie", format!("rsc_session={RAW_TOKEN}"));
        }
        if let Some(key) = idempotency {
            builder = builder.header("idempotency-key", key);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    async fn envelope(resp: Response) -> (StatusCode, Value) {
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 1_048_576).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    const CREATE_TASK_BODY: &str = r#"{"preview_token":"00","confirm":true}"#;

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
    async fn non_admin_malformed_write_requests_prefer_400_over_403() {
        // 门禁顺序钉死：path/body 参数解析与校验先于 require_admin_session，
        // 非管理员的非法输入优先 400，不得先 403。
        let task_id = Uuid::new_v4();
        let time_review_body =
            r#"{"expected_revision":"3","decision":"expire","evidence":"inconclusive"}"#;
        let release_lock_body =
            r#"{"expected_revision":"4","acknowledge_unknown":true,"reason":"ok"}"#;
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

    #[tokio::test]
    async fn release_lock_projects_only_two_fields_and_requires_admin() {
        let body =
            r#"{"expected_revision":"4","acknowledge_unknown":true,"reason":"现场已人工核实"}"#;
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

    // ── task.updated 业务事件发布 ─────────────────────────────────────

    /// handler 在返回响应之前完成发布，故此处同步排空即可（无需 await）。
    fn collect_events(
        rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>,
    ) -> Vec<EventEnvelope> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// fixture 会话的用户 id（`fixture_session` 里 admin 与 alice 共用）。
    const FIXTURE_USER_ID: [u8; 16] = [7u8; 16];

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
}

//! 任务 HTTP 路由处理函数与请求参数解析。
//!
//! 提供任务预览创建、任务提交/取消/详情/分页/时间复核与锁释放等 8 个端点 handler，
//! 以及严格查询串解析和统一 202 成功信封。

use axum::body::Body;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::ControllerError;
use crate::events::{EventEnvelope, EventObjectAudience};
use crate::http_api::{err_response, ok_response, request_id};
use crate::http_auth::EndpointId;
use crate::http_gate::{
    authenticate_read, authenticate_write, read_json_body, require_admin_session, single_header,
};
use crate::parse_device_id;
use crate::tasks::model::{MainTaskState, SubTaskState};
use crate::tasks::service::SubmitOutcome;

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
fn parse_page_query(raw: Option<&str>, allow_state: bool) -> Result<PageQuery, ControllerError> {
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
    let session = match authenticate_read(&state.auth, &headers, EndpointId::TaskChildren).await {
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

#[cfg(test)]
pub(crate) mod tests;

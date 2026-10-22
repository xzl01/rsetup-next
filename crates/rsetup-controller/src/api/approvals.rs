//! approvals 资源管理路由：网关/通知 sink 契约与 6 条管理端点（Identity Task 11）。
//!
//! 契约（spec 02 §3/§4）：全部端点先过 `http_gate` 会话门禁，再过 `require_admin_session`
//! 管理前置（admin 端点读写皆尔），权威判定仍在仓储事务 guard 内重验；5 条 `POST` 写路由
//! 只按 `session.digest` 走写限速桶，`GET /api/v1/approvals` 按
//! `(EndpointId::ApprovalsRead, session.digest)` 走读桶。批量 approve/reject 限制
//! `1 <= targets.len() <= 1024` 且必须 `confirm == true`，逐项 CAS 结果收集进
//! `results` 数组、HTTP 状态码固定 200；`reopen`/`revoke`/`reauthorize` 返回含
//! `requires_device_reset: true` 的固定契约。`GET` 列表的 `cursor`/`next_cursor` 一律经
//! `api/mod.rs` 统一签名 codec（DeviceKey 96 hex profile）签发与校验，绑定 `decision`
//! 规范筛选；`connection_state` 逐可见项经同一次真实 `DeviceLiveProbe` 投影，失败 503。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use axum::routing::{get, post};
use serde::Deserialize;

use crate::auth::service::Session;
use crate::devices::service::{AdmissionChangeEvent, MAX_ADMISSION_REASON_BYTES};
use crate::error::ControllerError;
use crate::events::{EventBus, EventEnvelope, EventObjectAudience};
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{
    authenticate_read, authenticate_write, read_json_body, require_admin_session,
};
use crate::model::ReviewDecision;

use super::devices::DeviceLiveProbe;
use super::{
    CursorFilterCtx, CursorKey, ManagementFuture, decode_management_list_cursor,
    management_cursor_secret, map_error, parse_device_id_path, parse_revision,
    sign_management_list_page,
};

#[derive(Clone, Copy, Debug)]
pub struct ApprovalTarget {
    pub device_id: [u8; 32],
    pub expected_revision: u64,
}

/// 只消费已 commit 的 typed `AdmissionChangeEvent`，不接受客户端伪造事件；
/// 与既有同步 `AdmissionChangeSink` 接线兼容的可失败边界。
pub trait ManagementNotificationSink: Send + Sync {
    fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ControllerError>;
}

#[derive(Clone)]
pub struct ApprovalsApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn ApprovalsGateway>,
    pub notification_sink: Arc<dyn ManagementNotificationSink>,
    /// 审批 `connection_state` 的唯一消费通道：与 devices 侧同源的真实探针实例，
    /// 不新造第二套探针类型、不用常量 offline 源或 `Option` 缺省。
    pub probe: Arc<dyn DeviceLiveProbe>,
    /// 进程内事件总线句柄（06 §4 `device.updated`）：与 SSE 读线 `EventsState.bus`
    /// 及 devices 侧发布方必须是同一通道句柄（[`EventBus::clone`] 不新建通道）。
    /// `None` 表示本装配未开启事件线，写端点行为与接入总线之前完全一致（不发布、
    /// 不报错、不阻塞）；发布返回值一律忽略，不得据此改变响应。
    pub events: Option<EventBus>,
}

pub trait ApprovalsGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        decision: Option<ReviewDecision>,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn approve<'a>(
        &'a self,
        actor: &'a Session,
        targets: &'a [ApprovalTarget],
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reject<'a>(
        &'a self,
        actor: &'a Session,
        targets: &'a [ApprovalTarget],
        reason: &'a str,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reopen<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn revoke<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
        reason: &'a str,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reauthorize<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

// ---------------------------------------------------------------------------
// 严格 query / body DTO
// ---------------------------------------------------------------------------

/// `GET /api/v1/approvals` 的严格 query DTO：`deny_unknown_fields` 拒绝任何未知参数；
/// `Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalsListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) decision: Option<String>,
}

/// 批量 approve/reject 的单个目标：`device_id` 恰 64 位小写 hex、`expected_revision`
/// 严格十进制字符串，逐项解析失败一律 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalTargetInput {
    pub(crate) device_id: String,
    pub(crate) expected_revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchApproveInput {
    pub(crate) targets: Vec<ApprovalTargetInput>,
    pub(crate) confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchRejectInput {
    pub(crate) targets: Vec<ApprovalTargetInput>,
    pub(crate) reason: String,
    pub(crate) confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReopenInput {
    pub(crate) expected_revision: String,
    pub(crate) confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevokeInput {
    pub(crate) expected_revision: String,
    pub(crate) reason: String,
    pub(crate) confirm: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReauthorizeInput {
    pub(crate) expected_revision: String,
    pub(crate) confirm: bool,
}

// ---------------------------------------------------------------------------
// 共享输入门禁
// ---------------------------------------------------------------------------

/// 批量目标上限（spec 02 §3：单请求 1..=1024）。
const MAX_BATCH_TARGETS: usize = 1024;

/// `confirm` 危险操作显式确认门禁：缺失由 serde 必填字段拒绝（400），`false` 一律
/// 400 `INVALID_ARGUMENT`，绝不静默执行。
#[allow(clippy::result_large_err)]
fn require_confirm(confirm: bool) -> Result<(), Response> {
    if confirm {
        Ok(())
    } else {
        Err(map_error(ControllerError::InvalidArgument))
    }
}

/// 批量目标解析：数量 `1..=1024`，逐项 `device_id` 严格小写 hex64、`expected_revision`
/// 严格十进制；任何违规 400，不截断、不部分接受。
#[allow(clippy::result_large_err)]
fn parse_targets(inputs: &[ApprovalTargetInput]) -> Result<Vec<ApprovalTarget>, Response> {
    if inputs.is_empty() || inputs.len() > MAX_BATCH_TARGETS {
        return Err(map_error(ControllerError::InvalidArgument));
    }
    let mut targets = Vec::with_capacity(inputs.len());
    for input in inputs {
        let device_id = parse_device_id_path(&input.device_id)?;
        let expected_revision = parse_revision(&input.expected_revision)?;
        targets.push(ApprovalTarget {
            device_id,
            expected_revision,
        });
    }
    Ok(targets)
}

/// `reason` 门禁：与 `devices::service::validate_reason` 同口径（trim 后非空、
/// ≤ `MAX_ADMISSION_REASON_BYTES`、无控制字符），违规 400。
#[allow(clippy::result_large_err)]
fn validate_reason(reason: &str) -> Result<(), Response> {
    if reason.trim().is_empty()
        || reason.len() > MAX_ADMISSION_REASON_BYTES
        || reason.chars().any(char::is_control)
    {
        return Err(map_error(ControllerError::InvalidArgument));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// GET /api/v1/approvals：decision 规范筛选 + 签名 cursor + 实时连接投影
// ---------------------------------------------------------------------------

/// approvals 列表 filter schema 的固定 tag 表：tag 1 = `decision` 规范字符串字节；
/// schema 恒含该字段（存在 → `[(1, Some(bytes))]`、缺省 → `[(1, None)]`），
/// `limit` 不进 schema、不参与 MAC 绑定。
const APPROVALS_FILTER_TAG_DECISION: u8 = 1;

/// `GET /api/v1/approvals` 的 inner：`secret_provider` 为唯一密钥入口（生产 wrapper 恒只经
/// `management_cursor_secret()`，测试 wrapper 从局部 `OnceLock` 槽读取）。调用顺序固定：
/// `authenticate_read(ApprovalsRead)` → `require_admin_session`（401/403 先于 query 解析）→
/// query/limit/decision 校验 → filter ctx 构造 → 取密钥（`cursor = None` 也不得短路跳过）→
/// cursor 解码（Device profile）→ 网关单页 → 逐可见项 probe 投影 → 出口签名。
/// 配置/后端错误经 `map_error` 映射（client 400、config 503、backend 503），绝不吞 `Err`
/// 或返回未签名页面。
async fn list_approvals_with_secret_provider<F>(
    state: &ApprovalsApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::ApprovalsRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<ApprovalsListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    // `decision` 严格小写规范字符串，与 Task 3 既有 wire 映射同源；大小写变体、空串与
    // `pending`/`rejected` 等非规范词一律 400，不做大小写不敏感或前缀匹配。
    let decision = match query.decision.as_deref() {
        None => None,
        Some("none") => Some(("none", ReviewDecision::None)),
        Some("approved") => Some(("approved", ReviewDecision::Approved)),
        Some("denied") => Some(("denied", ReviewDecision::Denied)),
        Some("revoked") => Some(("revoked", ReviewDecision::Revoked)),
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let filter = match CursorFilterCtx::from_ordered_fields(&[(
        APPROVALS_FILTER_TAG_DECISION,
        decision.map(|(name, _)| name.as_bytes()),
    )]) {
        Ok(filter) => filter,
        Err(error) => return map_error(error),
    };
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(
        &secret,
        EndpointId::ApprovalsRead,
        actor.user.id,
        &filter,
        query.cursor.as_deref(),
    ) {
        Ok(None) => None,
        Ok(Some(CursorKey::DeviceKey(raw))) => Some(raw),
        // `Uuid` 分支在 `ApprovalsRead` 的 `Device` profile 下不可达，仍 fail-closed 400。
        Ok(Some(CursorKey::Uuid(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let mut page = match state
        .gateway
        .list(&actor, decision.map(|(_, value)| value), cursor, limit)
        .await
    {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    // 审批连接投影：仓储页只含持久真相，逐可见项各调用一次同次真实 `probe.snapshot`；
    // `device_id` 反解失败或 probe `Err` → 整页失败经 `map_error` 精确 503，不得静默
    // 降级 `offline`、跳过该项或返回 200；items 为空时不调用 probe。
    let items = match page.get_mut("items").and_then(|items| items.as_array_mut()) {
        Some(items) => items,
        None => return map_error(ControllerError::Config("approvals page shape".into())),
    };
    for item in items.iter_mut() {
        let encoded = match item.get("device_id").and_then(|value| value.as_str()) {
            Some(encoded) => encoded,
            None => return map_error(ControllerError::Config("approvals page item shape".into())),
        };
        let id = match crate::parse_device_id(encoded).ok() {
            Some(id) => id,
            None => return map_error(ControllerError::Config("approvals page device id".into())),
        };
        let view = match state.probe.snapshot(&id).await {
            Ok(view) => view,
            Err(error) => return map_error(error),
        };
        item["connection_state"] = serde_json::json!(view.connection_state);
    }
    match sign_management_list_page(
        &secret,
        EndpointId::ApprovalsRead,
        actor.user.id,
        &filter,
        page,
    ) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_approvals(
    State(state): State<ApprovalsApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_approvals_with_secret_provider(&state, &headers, &uri, || {
        management_cursor_secret().copied()
    })
    .await
}

// ---------------------------------------------------------------------------
// POST /api/v1/approvals/approve 与 /reject：批量逐项结果，HTTP 200
// ---------------------------------------------------------------------------

/// `POST /api/v1/approvals/approve`：`authenticate_write` → `require_admin_session` →
/// body 严格解析 → `confirm == true` → 目标 `1..=1024` 逐项严格解析 → 网关逐项 CAS
/// （单项冲突/失败收集进 `results`，不改 HTTP 状态码）→ 固定 200。
/// 事件线：只对逐项结果中 `ok == true` 的项发布 `device.updated`（失败项未发生任何
/// 变更，不得发布），受众恒管理员范围。
async fn approve_devices(
    State(state): State<ApprovalsApiState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let input = match read_json_body::<BatchApproveInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    if let Err(response) = require_confirm(input.confirm) {
        return response;
    }
    let targets = match parse_targets(&input.targets) {
        Ok(targets) => targets,
        Err(response) => return response,
    };
    let results = match state.gateway.approve(&actor, &targets).await {
        Ok(results) => results,
        Err(error) => return map_error(error),
    };
    if let Some(ref bus) = state.events {
        publish_approved_devices(bus, &results, &targets);
    }
    ok_response(results)
}

/// 单目标准入状态机（revoke/reauthorize）成功后的 `device.updated` 发布：网关只回显
/// `{state|decision, requires_device_reset}`，不含新 revision，因此载荷 revision 取
/// CAS 的**递增下界** `expected_revision.saturating_add(1)`（06 §4：revision 只作同
/// kind+id 内的单调提示，不是全局版本）。饱和加而非 `+ 1`：`expected_revision` 由客户端
/// 任意给出，极限值下 `+ 1` 会 panic 或回绕成倒退版本。载荷 id 是设备公钥的小写 hex64，
/// 受众恒管理员范围。
fn publish_device_updated(bus: &EventBus, id: &[u8; 32], revision: u64) {
    bus.publish(EventEnvelope::device_updated(
        &hex::encode(id),
        revision,
        EventObjectAudience::admins(),
    ));
}

/// 批量 approve 成功项的 `device.updated` 发布：只遍历逐项结果中 `ok == true` 的项
/// （集合键兼容 `approved`/`results`，id 键兼容 `device_id`/`id`），载荷 revision 优先
/// 取该项回显的新 revision，回显缺失时退回同下标 target 的
/// `expected_revision.saturating_add(1)` 这一单调下界（饱和加：`expected_revision` 由
/// 客户端任意给出，极限值下 `+ 1` 会 panic 或回绕成倒退版本）。形状不符（非数组/缺 id）
/// 一律跳过该项，不影响 HTTP 200 响应。
fn publish_approved_devices(bus: &EventBus, value: &serde_json::Value, targets: &[ApprovalTarget]) {
    let Some(items) = value
        .get("approved")
        .or_else(|| value.get("results"))
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for (index, item) in items.iter().enumerate() {
        if item.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            continue;
        }
        let Some(raw_id) = item
            .get("device_id")
            .or_else(|| item.get("id"))
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let revision = item
            .get("revision")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| raw.parse::<u64>().ok())
            .or_else(|| {
                targets
                    .get(index)
                    .map(|target| target.expected_revision.saturating_add(1))
            })
            .unwrap_or(1);
        bus.publish(EventEnvelope::device_updated(
            raw_id,
            revision,
            EventObjectAudience::admins(),
        ));
    }
}

/// `POST /api/v1/approvals/reject`：同 approve，另加 `reason` 门禁（trim 后非空、
/// ≤1024B、无控制字符），违规 400 先于网关。
async fn reject_devices(
    State(state): State<ApprovalsApiState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let input = match read_json_body::<BatchRejectInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    if let Err(response) = require_confirm(input.confirm) {
        return response;
    }
    if let Err(response) = validate_reason(&input.reason) {
        return response;
    }
    let targets = match parse_targets(&input.targets) {
        Ok(targets) => targets,
        Err(response) => return response,
    };
    let results = match state.gateway.reject(&actor, &targets, &input.reason).await {
        Ok(results) => results,
        Err(error) => return map_error(error),
    };
    ok_response(results)
}

// ---------------------------------------------------------------------------
// 单目标状态机：reopen / revoke / reauthorize（CAS + requires_device_reset 契约）
// ---------------------------------------------------------------------------

/// `POST /api/v1/approvals/{device_id}/reopen`：审核决定回到 `none`，设备必须重新
/// 准入（`requires_device_reset: true` 固定契约）。版本不符 409 `REVISION_CONFLICT`；
/// 不存在 404。
async fn reopen_approval(
    State(state): State<ApprovalsApiState>,
    Path(device_id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let id = match parse_device_id_path(&device_id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let input = match read_json_body::<ReopenInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    if let Err(response) = require_confirm(input.confirm) {
        return response;
    }
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    let value = match state.gateway.reopen(&actor, id, revision).await {
        Ok(value) => value,
        Err(error) => return map_error(error),
    };
    ok_response(value)
}

/// `POST /api/v1/devices/{id}/revoke`：准入吊销（仅限已批准），返回
/// `{ state: "REVOKED", requires_device_reset: true }`；`reason` 门禁同 reject。
/// 吊销成功后发布 `device.updated`（revision = `expected_revision.saturating_add(1)`，
/// 仅管理员受众）。
async fn revoke_device(
    State(state): State<ApprovalsApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let id = match parse_device_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let input = match read_json_body::<RevokeInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    if let Err(response) = require_confirm(input.confirm) {
        return response;
    }
    if let Err(response) = validate_reason(&input.reason) {
        return response;
    }
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    let value = match state
        .gateway
        .revoke(&actor, id, revision, &input.reason)
        .await
    {
        Ok(value) => value,
        Err(error) => return map_error(error),
    };
    if let Some(ref bus) = state.events {
        publish_device_updated(bus, &id, revision.saturating_add(1));
    }
    ok_response(value)
}

/// `POST /api/v1/devices/{id}/reauthorize`：仅限原为 `REVOKED` 重新授权，返回
/// `{ state: "APPROVED", requires_device_reset: true }`；非法前态由网关
/// `InvalidState` → 409。重新授权成功后发布 `device.updated`（受众恒管理员范围）。
async fn reauthorize_device(
    State(state): State<ApprovalsApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let id = match parse_device_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let input = match read_json_body::<ReauthorizeInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    if let Err(response) = require_confirm(input.confirm) {
        return response;
    }
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    let value = match state.gateway.reauthorize(&actor, id, revision).await {
        Ok(value) => value,
        Err(error) => return map_error(error),
    };
    if let Some(ref bus) = state.events {
        publish_device_updated(bus, &id, revision.saturating_add(1));
    }
    ok_response(value)
}

/// 挂载 approvals 的 6 条管理路由：集合级 `GET` 唯一注册锚点为 `get(list_approvals)`，
/// 无第二处注册；5 条 `POST` 写路由各只挂载一次。`/api/v1/devices/{id}/revoke` 与
/// `/api/v1/devices/{id}/reauthorize` 与 devices 子 router 的 `/api/v1/devices/{id}`
/// 路径段数不同，merge 后无冲突。
pub fn approvals_router(state: ApprovalsApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/approvals", get(list_approvals))
        .route("/api/v1/approvals/approve", post(approve_devices))
        .route("/api/v1/approvals/reject", post(reject_devices))
        .route(
            "/api/v1/approvals/{device_id}/reopen",
            post(reopen_approval),
        )
        .route("/api/v1/devices/{id}/revoke", post(revoke_device))
        .route("/api/v1/devices/{id}/reauthorize", post(reauthorize_device))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::devices::{DeviceConnectionState, DeviceLiveView, DeviceStreamHealth};
    use crate::api::users::test_fixture::*;
    use crate::events::{EventAudience, EventName, EventViewer};
    use axum::http::StatusCode;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    /// 批量混合结果用例的冲突样本（`b`×64 → 桩内映射 `REVISION_CONFLICT`）。
    const DEVICE_A: char = 'a';
    const DEVICE_B: char = 'b';

    fn device_id(fill: char) -> String {
        fill.to_string().repeat(64)
    }

    async fn error_code(res: axum::response::Response) -> (StatusCode, String) {
        let status = res.status();
        let value = body_json(res).await;
        (
            status,
            value["error"]["code"].as_str().unwrap_or("").to_string(),
        )
    }

    /// 纯内存 `ApprovalsGateway`：`Standard` 按契约固定形状回显（批量逐项 ok、单目标
    /// 返回 `requires_device_reset: true` 契约）；`Mixed` 批量逐项——`b`×64 目标返回
    /// `REVISION_CONFLICT`、其余 ok；`Conflict` 单目标恒 `Err(RevisionConflict)`。
    #[derive(Clone, Copy)]
    enum FixtureApprovalsGateway {
        Standard,
        Mixed,
        Conflict,
    }

    fn batch_results(targets: &[ApprovalTarget], mixed: bool) -> serde_json::Value {
        let results = targets
            .iter()
            .map(|target| {
                let conflicted = mixed && target.device_id == [0xbbu8; 32];
                if conflicted {
                    serde_json::json!({
                        "device_id": hex::encode(target.device_id),
                        "ok": false,
                        "error": { "code": "REVISION_CONFLICT" },
                    })
                } else {
                    serde_json::json!({
                        "device_id": hex::encode(target.device_id),
                        "ok": true,
                        "revision": target.expected_revision.saturating_add(1).to_string(),
                    })
                }
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "results": results })
    }

    impl ApprovalsGateway for FixtureApprovalsGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            _decision: Option<ReviewDecision>,
            _cursor: Option<[u8; 32]>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async {
                Ok(serde_json::json!({
                    "items": [{
                        "device_id": hex::encode([0x11u8; 32]),
                        "fingerprint": hex::encode([0x11u8; 32]),
                        "descriptor": null,
                        "decision": "none",
                        "revision": "1",
                    }],
                    "next_cursor": null,
                }))
            })
        }
        fn approve<'a>(
            &'a self,
            _actor: &'a Session,
            targets: &'a [ApprovalTarget],
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            let targets = targets.to_vec();
            Box::pin(async move {
                match gateway {
                    Self::Conflict => Err(ControllerError::RevisionConflict),
                    other => Ok(batch_results(&targets, matches!(other, Self::Mixed))),
                }
            })
        }
        fn reject<'a>(
            &'a self,
            _actor: &'a Session,
            targets: &'a [ApprovalTarget],
            _reason: &'a str,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            let targets = targets.to_vec();
            Box::pin(async move {
                match gateway {
                    Self::Conflict => Err(ControllerError::RevisionConflict),
                    other => Ok(batch_results(&targets, matches!(other, Self::Mixed))),
                }
            })
        }
        fn reopen<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
            _revision: u64,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                match gateway {
                    Self::Conflict => Err(ControllerError::RevisionConflict),
                    _ => Ok(serde_json::json!({
                        "decision": "none",
                        "requires_device_reset": true,
                    })),
                }
            })
        }
        fn revoke<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
            _revision: u64,
            _reason: &'a str,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                match gateway {
                    Self::Conflict => Err(ControllerError::RevisionConflict),
                    _ => Ok(serde_json::json!({
                        "state": "REVOKED",
                        "requires_device_reset": true,
                    })),
                }
            })
        }
        fn reauthorize<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
            _revision: u64,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                match gateway {
                    Self::Conflict => Err(ControllerError::RevisionConflict),
                    _ => Ok(serde_json::json!({
                        "state": "APPROVED",
                        "requires_device_reset": true,
                    })),
                }
            })
        }
    }

    /// 通知 sink 私有桩：本任务 handler 不调用 sink（typed 事件只由生产 bridge 在
    /// commit 后投递），此处仅满足装配。
    struct FixtureNotificationSink;

    impl ManagementNotificationSink for FixtureNotificationSink {
        fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
            Ok(())
        }
    }

    /// `Ok` 返回固定在线实时视图；`Fail` 模拟 probe 读取失败（必须 503，不得降级）。
    #[derive(Clone, Copy)]
    enum FixtureProbe {
        Ok,
        Fail,
    }

    impl DeviceLiveProbe for FixtureProbe {
        fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
            let fail = matches!(self, Self::Fail);
            Box::pin(async move {
                if fail {
                    Err(ControllerError::Config("fixture probe down".into()))
                } else {
                    Ok(DeviceLiveView {
                        connection_state: DeviceConnectionState::Online,
                        control_health: DeviceStreamHealth::Healthy,
                        data_health: DeviceStreamHealth::Healthy,
                        capabilities: None,
                    })
                }
            })
        }
    }

    /// 事件线关闭（`events: None`）的装配：既有全部用例的行为基线。
    fn approvals_state_with(
        gateway: FixtureApprovalsGateway,
        session: Session,
    ) -> ApprovalsApiState {
        approvals_state_with_events(gateway, session, None)
    }

    /// 事件总线可选的装配：`Some(bus)` 与 SSE 读线 `EventsState.bus` 是同一通道句柄。
    fn approvals_state_with_events(
        gateway: FixtureApprovalsGateway,
        session: Session,
        events: Option<EventBus>,
    ) -> ApprovalsApiState {
        ApprovalsApiState {
            auth: fixture_auth(session),
            gateway: Arc::new(gateway),
            notification_sink: Arc::new(FixtureNotificationSink),
            probe: Arc::new(FixtureProbe::Ok),
            events,
        }
    }

    /// 恒成功桩：400 一律由 handler 的 body/门禁校验先于网关产生。
    fn fixture_approvals_state() -> ApprovalsApiState {
        approvals_state_with(FixtureApprovalsGateway::Standard, fixture_session(true))
    }

    /// 批量逐项混合结果桩：`b`×64 目标返回 `REVISION_CONFLICT`。
    fn fixture_approvals_state_with_mixed_results() -> ApprovalsApiState {
        approvals_state_with(FixtureApprovalsGateway::Mixed, fixture_session(true))
    }

    // RED 前置：骨架 `approvals_router` 挂载 0 条路由，以下断言在实现前必须失败于
    // 未挂载路由的 404 行为（行为 RED），不得是编译错误。

    #[tokio::test]
    async fn approve_without_confirm_is_rejected_with_400() {
        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            "/api/v1/approvals/approve",
            serde_json::json!({
                "targets": [{ "device_id": device_id(DEVICE_A), "expected_revision": "1" }],
                "confirm": false,
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn batch_approve_returns_200_with_per_item_results_even_with_partial_conflict() {
        let router = approvals_router(fixture_approvals_state_with_mixed_results());
        let req = authenticated_json_post(
            "/api/v1/approvals/approve",
            serde_json::json!({
                "targets": [
                    { "device_id": device_id(DEVICE_A), "expected_revision": "1" },
                    { "device_id": device_id(DEVICE_B), "expected_revision": "99" },
                ],
                "confirm": true,
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        let results = val["data"]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["ok"], true);
        assert_eq!(results[1]["ok"], false);
        assert_eq!(results[1]["error"]["code"], "REVISION_CONFLICT");
    }

    #[tokio::test]
    async fn revoke_indicates_requires_device_reset() {
        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            &format!("/api/v1/devices/{}/revoke", device_id(DEVICE_A)),
            serde_json::json!({
                "expected_revision": "1", "reason": "compromised", "confirm": true,
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["data"]["requires_device_reset"], true);
        assert_eq!(val["data"]["state"], "REVOKED");
    }

    #[tokio::test]
    async fn reject_requires_non_blank_reason_within_1024_without_control_chars() {
        let cases = [
            serde_json::json!(""),
            serde_json::json!("   "),
            serde_json::json!("line\nbreak"),
            serde_json::json!("x".repeat(1025)),
        ];
        for reason in cases {
            let router = approvals_router(fixture_approvals_state());
            let req = authenticated_json_post(
                "/api/v1/approvals/reject",
                serde_json::json!({
                    "targets": [{ "device_id": device_id(DEVICE_A), "expected_revision": "1" }],
                    "reason": reason,
                    "confirm": true,
                }),
            );
            let res = router.oneshot(req).await.unwrap();
            let (status, code) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {reason}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {reason}");
        }
    }

    #[tokio::test]
    async fn reject_with_valid_reason_returns_per_item_results() {
        let router = approvals_router(fixture_approvals_state_with_mixed_results());
        let req = authenticated_json_post(
            "/api/v1/approvals/reject",
            serde_json::json!({
                "targets": [
                    { "device_id": device_id(DEVICE_A), "expected_revision": "1" },
                    { "device_id": device_id(DEVICE_B), "expected_revision": "99" },
                ],
                "reason": "stolen credential",
                "confirm": true,
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        let results = val["data"]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["ok"], true);
        assert_eq!(results[1]["error"]["code"], "REVISION_CONFLICT");
    }

    #[tokio::test]
    async fn batch_targets_must_be_between_1_and_1024() {
        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            "/api/v1/approvals/approve",
            serde_json::json!({ "targets": [], "confirm": true }),
        );
        let (status, code) = error_code(router.oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");

        let targets = (0..1025)
            .map(|i| {
                serde_json::json!({
                    "device_id": format!("{i:064x}"),
                    "expected_revision": "1",
                })
            })
            .collect::<Vec<_>>();
        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            "/api/v1/approvals/approve",
            serde_json::json!({ "targets": targets, "confirm": true }),
        );
        let (status, code) = error_code(router.oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
    }

    #[tokio::test]
    async fn reopen_returns_decision_none_and_requires_device_reset() {
        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            &format!("/api/v1/approvals/{}/reopen", device_id(DEVICE_A)),
            serde_json::json!({ "expected_revision": "1", "confirm": true }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["data"]["decision"], "none");
        assert_eq!(val["data"]["requires_device_reset"], true);
    }

    #[tokio::test]
    async fn reauthorize_returns_state_approved_and_requires_device_reset() {
        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            &format!("/api/v1/devices/{}/reauthorize", device_id(DEVICE_A)),
            serde_json::json!({ "expected_revision": "1", "confirm": true }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["data"]["state"], "APPROVED");
        assert_eq!(val["data"]["requires_device_reset"], true);
    }

    #[tokio::test]
    async fn single_target_revision_conflict_maps_to_409() {
        let state = approvals_state_with(FixtureApprovalsGateway::Conflict, fixture_session(true));
        let router = approvals_router(state);
        let cases = [
            (
                format!("/api/v1/approvals/{}/reopen", device_id(DEVICE_A)),
                serde_json::json!({ "expected_revision": "1", "confirm": true }),
            ),
            (
                format!("/api/v1/devices/{}/revoke", device_id(DEVICE_A)),
                serde_json::json!({
                    "expected_revision": "1", "reason": "compromised", "confirm": true,
                }),
            ),
            (
                format!("/api/v1/devices/{}/reauthorize", device_id(DEVICE_A)),
                serde_json::json!({ "expected_revision": "1", "confirm": true }),
            ),
        ];
        for (uri, body) in cases {
            let req = authenticated_json_post(&uri, body);
            let (status, code) = error_code(router.clone().oneshot(req).await.unwrap()).await;
            assert_eq!(status, StatusCode::CONFLICT, "case {uri}");
            assert_eq!(code, "REVISION_CONFLICT", "case {uri}");
        }
    }

    #[tokio::test]
    async fn approvals_routes_require_admin_session() {
        let state = approvals_state_with(FixtureApprovalsGateway::Standard, fixture_session(false));
        let router = approvals_router(state);
        let req = authenticated_json_post(
            "/api/v1/approvals/approve",
            serde_json::json!({
                "targets": [{ "device_id": device_id(DEVICE_A), "expected_revision": "1" }],
                "confirm": true,
            }),
        );
        let (status, code) = error_code(router.clone().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");

        let (status, code) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/approvals"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn must_change_password_returns_distinct_403_code() {
        let state = approvals_state_with(
            FixtureApprovalsGateway::Standard,
            fixture_session_must_change_password(),
        );
        let router = approvals_router(state);
        let req = authenticated_json_post(
            "/api/v1/approvals/reject",
            serde_json::json!({
                "targets": [{ "device_id": device_id(DEVICE_A), "expected_revision": "1" }],
                "reason": "fixture",
                "confirm": true,
            }),
        );
        let (status, code) = error_code(router.clone().oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PASSWORD_CHANGE_REQUIRED");

        let (status, code) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/approvals"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PASSWORD_CHANGE_REQUIRED");
    }

    #[tokio::test]
    async fn approvals_list_without_installed_cursor_secret_returns_503() {
        // 生产 router 恒只经全局 `management_cursor_secret()`；单元测试进程内全局
        // OnceLock 未安装 → 精确 503 `STORAGE_UNAVAILABLE`（fail-closed，不降级未签名页）。
        let router = approvals_router(fixture_approvals_state());
        let (status, code) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/approvals"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "STORAGE_UNAVAILABLE");
    }

    #[tokio::test]
    async fn reopen_rejects_malformed_device_id_or_revision_with_400() {
        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            "/api/v1/approvals/not-a-hex64/reopen",
            serde_json::json!({ "expected_revision": "1", "confirm": true }),
        );
        let (status, code) = error_code(router.oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");

        let router = approvals_router(fixture_approvals_state());
        let req = authenticated_json_post(
            &format!("/api/v1/approvals/{}/reopen", device_id(DEVICE_A)),
            serde_json::json!({ "expected_revision": "+1", "confirm": true }),
        );
        let (status, code) = error_code(router.oneshot(req).await.unwrap()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
    }

    // -----------------------------------------------------------------------
    // GET list 签名 cursor 真实接线（test-only wrapper + 局部 OnceLock 密钥槽）
    // -----------------------------------------------------------------------
    //
    // 生产 `list_approvals` 恒只经 `management_cursor_secret()`；测试进程内全局
    // `MANAGEMENT_CURSOR_SECRET` 未安装，挂生产 router 的 GET list 恒 503（已由
    // `approvals_list_without_installed_cursor_secret_returns_503` 实证）。因此 list
    // 正例不挂生产 router，改挂只包 `list_approvals_with_secret_provider` 的 test-only
    // wrapper：真实 gate/query/codec/gateway 顺序不绕过，密钥经 `cursor_secret_of` 从本
    // 测试局部槽读取。红线：不 set 全局 OnceLock、不给 `ApprovalsApiState` 扩 secret
    // 字段、不用假 handler 或自签 token 绕过真实链路。

    /// 与 users/roles/grants/groups/devices 区同值的测试合成 key，不引用任何 secret 文件。
    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];
    /// 网关内部 raw 64 hex（Device profile 32B），出口必须被替换为 96 字符签名 token。
    const NEXT_RAW_HEX: &str = "abababababababababababababababababababababababababababababababab";

    fn decision_str(decision: ReviewDecision) -> &'static str {
        match decision {
            ReviewDecision::None => "none",
            ReviewDecision::Approved => "approved",
            ReviewDecision::Denied => "denied",
            ReviewDecision::Revoked => "revoked",
        }
    }

    /// `list` 单次调用的记录形状（规避 clippy::type_complexity）。
    type PageCall = (Option<String>, Option<[u8; 32]>, usize);

    /// 记录每次 `list` 收到的 typed decision/cursor/limit 的页面桩：`cursor = None`
    /// 返回带 raw `next_cursor` 的第一页，否则返回末页。
    #[derive(Default)]
    struct PageApprovalsGateway {
        calls: std::sync::Mutex<Vec<PageCall>>,
    }

    impl PageApprovalsGateway {
        fn recorded(&self) -> Vec<PageCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl ApprovalsGateway for PageApprovalsGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            decision: Option<ReviewDecision>,
            cursor: Option<[u8; 32]>,
            limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move {
                self.calls.lock().unwrap().push((
                    decision.map(decision_str).map(str::to_string),
                    cursor,
                    limit,
                ));
                let next_cursor = if cursor.is_none() {
                    serde_json::json!(NEXT_RAW_HEX)
                } else {
                    serde_json::Value::Null
                };
                Ok(serde_json::json!({
                    "items": [{
                        "device_id": hex::encode([0x11u8; 32]),
                        "fingerprint": hex::encode([0x11u8; 32]),
                        "descriptor": null,
                        "decision": "approved",
                        "revision": "1",
                    }],
                    "next_cursor": next_cursor,
                }))
            })
        }
        fn approve<'a>(
            &'a self,
            _actor: &'a Session,
            _targets: &'a [ApprovalTarget],
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn reject<'a>(
            &'a self,
            _actor: &'a Session,
            _targets: &'a [ApprovalTarget],
            _reason: &'a str,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn reopen<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
            _revision: u64,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
            _revision: u64,
            _reason: &'a str,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn reauthorize<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
            _revision: u64,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    /// 记录 `snapshot` 调用次数的在线探针桩：投影必须逐可见项各调用一次。
    struct CountingProbe {
        calls: Arc<AtomicUsize>,
    }

    impl DeviceLiveProbe for CountingProbe {
        fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(DeviceLiveView {
                    connection_state: DeviceConnectionState::Online,
                    control_health: DeviceStreamHealth::Healthy,
                    data_health: DeviceStreamHealth::Healthy,
                    capabilities: None,
                })
            })
        }
    }

    #[derive(Clone)]
    struct SignedApprovalsCursorTestState {
        approvals: ApprovalsApiState,
        secret_slot: Arc<std::sync::OnceLock<[u8; 32]>>,
    }

    /// test-only wrapper handler：内部调用同一个 `list_approvals_with_secret_provider`，
    /// 真实 gate/query/codec/gateway 顺序不绕过，provider 经 `cursor_secret_of` 从本
    /// 测试局部槽读取。
    async fn list_approvals_with_test_secret(
        State(state): State<SignedApprovalsCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = state.secret_slot.clone();
        list_approvals_with_secret_provider(&state.approvals, &headers, &uri, move || {
            crate::api::cursor_secret_of(&slot).copied()
        })
        .await
    }

    /// fixture helper：只挂 wrapper 的 GET list 路由并 `.with_state(state)` 归约；
    /// `Some` 经 `install_cursor_secret_into` 装入测试合成 key、`None` 留空作网关前
    /// 503 负例。
    fn signed_approvals_cursor_router(
        secret: Option<[u8; 32]>,
        gateway: Arc<PageApprovalsGateway>,
        probe: Arc<dyn DeviceLiveProbe>,
    ) -> axum::Router<()> {
        let secret_slot = Arc::new(std::sync::OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret)
                .expect("synthetic key 安装必须成功");
        }
        let state = SignedApprovalsCursorTestState {
            approvals: ApprovalsApiState {
                auth: fixture_auth(fixture_session(true)),
                gateway,
                notification_sink: Arc::new(FixtureNotificationSink),
                probe,
                events: None,
            },
            secret_slot,
        };
        axum::Router::new()
            .route("/api/v1/approvals", get(list_approvals_with_test_secret))
            .with_state(state)
    }

    fn online_probe() -> Arc<dyn DeviceLiveProbe> {
        Arc::new(FixtureProbe::Ok)
    }

    #[tokio::test]
    async fn wrapper_list_without_secret_returns_503_before_gateway() {
        let gateway = Arc::new(PageApprovalsGateway::default());
        let router = signed_approvals_cursor_router(None, gateway.clone(), online_probe());
        let (status, code) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/approvals"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "STORAGE_UNAVAILABLE");
        assert!(
            gateway.recorded().is_empty(),
            "取密钥必须在 gateway.list 之前（含 cursor=None 路径）"
        );
    }

    #[tokio::test]
    async fn list_signs_96_hex_token_and_decodes_second_page_device_key_cursor() {
        let gateway = Arc::new(PageApprovalsGateway::default());
        let router =
            signed_approvals_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone(), online_probe());
        let res = router
            .oneshot(authenticated_get("/api/v1/approvals?decision=approved"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        assert_eq!(token.len(), 96, "DeviceKey profile 签名 token 恒 96 hex");
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_ne!(token, NEXT_RAW_HEX, "裸 raw key 不得直传出口");
        // 第二页：同一 decision 文本 + 换 limit=7——limit 不进 schema，仍须 200。
        let router =
            signed_approvals_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone(), online_probe());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/approvals?cursor={token}&decision=approved&limit=7"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert!(value["data"]["next_cursor"].is_null());
        let calls = gateway.recorded();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (Some("approved".into()), None, 50));
        assert_eq!(
            calls[1],
            (Some("approved".into()), Some([0xabu8; 32]), 7),
            "网关必须收到 token 解回的 DeviceKey raw32"
        );
    }

    #[tokio::test]
    async fn list_rejects_tampered_tag_before_gateway() {
        let gateway = Arc::new(PageApprovalsGateway::default());
        let router =
            signed_approvals_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone(), online_probe());
        let res = router
            .oneshot(authenticated_get("/api/v1/approvals"))
            .await
            .unwrap();
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        let mut raw = hex::decode(&token).expect("lowercase hex");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = hex::encode(&raw);
        let before = gateway.recorded().len();
        let router =
            signed_approvals_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone(), online_probe());
        let (status, code) = error_code(
            router
                .oneshot(authenticated_get(&format!(
                    "/api/v1/approvals?cursor={tampered}"
                )))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(gateway.recorded().len(), before, "客户端 400 不得触达网关");
    }

    #[tokio::test]
    async fn approvals_list_rejects_unknown_decision() {
        for decision in ["pending", "Approved", "APPROVED", "rejected", "", "none%20"] {
            let gateway = Arc::new(PageApprovalsGateway::default());
            let router = signed_approvals_cursor_router(
                Some(SYNTHETIC_SECRET),
                gateway.clone(),
                online_probe(),
            );
            let (status, code) = error_code(
                router
                    .oneshot(authenticated_get(&format!(
                        "/api/v1/approvals?decision={decision}"
                    )))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {decision:?}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {decision:?}");
            assert!(
                gateway.recorded().is_empty(),
                "400 不得触达网关：case {decision:?}"
            );
        }
    }

    #[tokio::test]
    async fn approvals_list_rejects_filter_swap() {
        let gateway = Arc::new(PageApprovalsGateway::default());
        let router =
            signed_approvals_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone(), online_probe());
        let res = router
            .oneshot(authenticated_get("/api/v1/approvals?decision=approved"))
            .await
            .unwrap();
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        // 下一页改写或缺省 decision → filter 不一致 → MAC 校验失败 400，不触网关。
        for query in [
            format!("cursor={token}"),
            format!("cursor={token}&decision=denied"),
            format!("cursor={token}&decision=none"),
        ] {
            let before = gateway.recorded().len();
            let router = signed_approvals_cursor_router(
                Some(SYNTHETIC_SECRET),
                gateway.clone(),
                online_probe(),
            );
            let (status, code) = error_code(
                router
                    .oneshot(authenticated_get(&format!("/api/v1/approvals?{query}")))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
            assert_eq!(
                gateway.recorded().len(),
                before,
                "客户端 400 不得触达网关：case {query}"
            );
        }
    }

    #[tokio::test]
    async fn approvals_list_rejects_out_of_range_limit_with_400() {
        for query in [
            "limit=0",
            "limit=201",
            "limit=abc",
            "foo=1",
            "decision=approved&x=1",
        ] {
            let gateway = Arc::new(PageApprovalsGateway::default());
            let router = signed_approvals_cursor_router(
                Some(SYNTHETIC_SECRET),
                gateway.clone(),
                online_probe(),
            );
            let (status, code) = error_code(
                router
                    .oneshot(authenticated_get(&format!("/api/v1/approvals?{query}")))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
            assert!(
                gateway.recorded().is_empty(),
                "400 不得触达网关：case {query}"
            );
        }
    }

    #[tokio::test]
    async fn approvals_list_projects_live_connection_state() {
        let gateway = Arc::new(PageApprovalsGateway::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let router = signed_approvals_cursor_router(
            Some(SYNTHETIC_SECRET),
            gateway,
            Arc::new(CountingProbe {
                calls: calls.clone(),
            }),
        );
        let res = router
            .oneshot(authenticated_get("/api/v1/approvals"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["items"][0]["connection_state"], "online");
        let visible = value["data"]["items"].as_array().unwrap().len();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            visible,
            "probe 必须逐可见项各调用一次"
        );
    }

    #[tokio::test]
    async fn approvals_list_returns_503_when_probe_fails() {
        let gateway = Arc::new(PageApprovalsGateway::default());
        let router = signed_approvals_cursor_router(
            Some(SYNTHETIC_SECRET),
            gateway,
            Arc::new(FixtureProbe::Fail),
        );
        let res = router
            .oneshot(authenticated_get("/api/v1/approvals"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let value = body_json(res).await;
        assert_eq!(value["error"]["code"], "STORAGE_UNAVAILABLE");
        // 不得静默降级：503 响应体既不含 `connection_state` 也不含 `"offline"`。
        let text = value.to_string();
        assert!(!text.contains("connection_state"), "{text}");
        assert!(!text.contains("offline"), "{text}");
    }

    // -----------------------------------------------------------------------
    // 准入写端点的 `device.updated` 事件发布（06 §4）
    // -----------------------------------------------------------------------

    /// 收集订阅通道当前待读的事件（订阅只收「之后」的事件，不回放历史）。
    fn collect_events(
        rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>,
    ) -> Vec<EventEnvelope> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// `revoke` 成功：恰好一条 `device.updated`，载荷 id 为路径设备公钥 hex64、
    /// revision 为 CAS 递增下界 `expected_revision.saturating_add(1)`，受众仅管理员。
    #[tokio::test]
    async fn revoke_publishes_device_updated_to_admins_when_bus_configured() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = approvals_router(approvals_state_with_events(
            FixtureApprovalsGateway::Standard,
            fixture_session(true),
            Some(bus),
        ));
        let res = router
            .oneshot(authenticated_json_post(
                &format!("/api/v1/devices/{}/revoke", device_id(DEVICE_A)),
                serde_json::json!({
                    "expected_revision": "1", "reason": "compromised", "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1, "吊销成功必须恰好发布一条 device.updated");
        let event = &events[0];
        assert_eq!(event.name(), EventName::DeviceUpdated);
        assert_eq!(event.data()["kind"], serde_json::json!("device"));
        assert_eq!(event.data()["id"], serde_json::json!(device_id(DEVICE_A)));
        assert_eq!(event.data()["revision"], serde_json::json!("2"));
        assert!(event.data()["revision"].is_string());
        assert_eq!(event.audience(), EventAudience::Admins);
        assert!(
            event.visible_to(EventViewer {
                user_id: [7u8; 16],
                is_admin: true,
            }),
            "管理员受众事件对管理员会话可投递"
        );
        assert!(
            !event.visible_to(EventViewer {
                user_id: [7u8; 16],
                is_admin: false,
            }),
            "对象级管理员范围事件对普通用户不可见"
        );
    }

    /// 溢出防御：`expected_revision` 是客户端任意输入（`parse_revision` 允许上到
    /// `u64::MAX`），CAS 下界取**饱和加**：既不在 debug 构建 panic，也不回绕成 `0`
    /// 这一倒退 revision。
    #[tokio::test]
    async fn revoke_at_u64_max_revision_publishes_saturating_lower_bound() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = approvals_router(approvals_state_with_events(
            FixtureApprovalsGateway::Standard,
            fixture_session(true),
            Some(bus),
        ));
        let res = router
            .oneshot(authenticated_json_post(
                &format!("/api/v1/devices/{}/revoke", device_id(DEVICE_A)),
                serde_json::json!({
                    "expected_revision": u64::MAX.to_string(),
                    "reason": "compromised",
                    "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].data()["revision"],
            serde_json::json!(u64::MAX.to_string()),
            "极限 revision 必须饱和在 u64::MAX，不得回绕"
        );
    }

    /// `reauthorize` 成功：同样恰好一条 `device.updated`（受众仅管理员）。
    #[tokio::test]
    async fn reauthorize_publishes_device_updated_when_bus_configured() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = approvals_router(approvals_state_with_events(
            FixtureApprovalsGateway::Standard,
            fixture_session(true),
            Some(bus),
        ));
        let res = router
            .oneshot(authenticated_json_post(
                &format!("/api/v1/devices/{}/reauthorize", device_id(DEVICE_A)),
                serde_json::json!({ "expected_revision": "5", "confirm": true }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name(), EventName::DeviceUpdated);
        assert_eq!(
            events[0].data()["id"],
            serde_json::json!(device_id(DEVICE_A))
        );
        assert_eq!(events[0].data()["revision"], serde_json::json!("6"));
        assert_eq!(events[0].audience(), EventAudience::Admins);
    }

    /// 批量 `approve`：只对逐项结果里 `ok == true` 的项发布（Mixed 桩下 `b`×64 的
    /// `REVISION_CONFLICT` 项未发生任何变更，不得发布）；载荷 revision 取回显值。
    #[tokio::test]
    async fn approve_publishes_only_successful_items_when_bus_configured() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = approvals_router(approvals_state_with_events(
            FixtureApprovalsGateway::Mixed,
            fixture_session(true),
            Some(bus),
        ));
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/approvals/approve",
                serde_json::json!({
                    "targets": [
                        { "device_id": device_id(DEVICE_A), "expected_revision": "1" },
                        { "device_id": device_id(DEVICE_B), "expected_revision": "99" },
                    ],
                    "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let events = collect_events(&mut rx);
        assert_eq!(
            events.len(),
            1,
            "冲突项未发生任何变更，不得发布 device.updated"
        );
        assert_eq!(events[0].name(), EventName::DeviceUpdated);
        assert_eq!(
            events[0].data()["id"],
            serde_json::json!(device_id(DEVICE_A))
        );
        assert_eq!(events[0].data()["revision"], serde_json::json!("2"));
        assert_eq!(events[0].audience(), EventAudience::Admins);
    }

    /// 全部目标成功的批量 approve：逐项各一条，不多发、不漏发。
    #[tokio::test]
    async fn approve_publishes_one_event_per_successful_device() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = approvals_router(approvals_state_with_events(
            FixtureApprovalsGateway::Standard,
            fixture_session(true),
            Some(bus),
        ));
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/approvals/approve",
                serde_json::json!({
                    "targets": [
                        { "device_id": device_id(DEVICE_A), "expected_revision": "1" },
                        { "device_id": device_id(DEVICE_B), "expected_revision": "7" },
                    ],
                    "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events
                .iter()
                .map(|event| event.data()["id"].as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>(),
            vec![device_id(DEVICE_A), device_id(DEVICE_B)]
        );
        assert_eq!(events[0].data()["revision"], serde_json::json!("2"));
        assert_eq!(events[1].data()["revision"], serde_json::json!("8"));
    }

    /// 负向：`events: None` 的既有装配不发布任何事件（行为与接入总线前一致）；
    /// 有总线但写失败（409）同样不发布。
    #[tokio::test]
    async fn approval_writes_publish_nothing_without_event_bus_or_on_failure() {
        // 1) `events: None`：装配不持有任何通道句柄，故不存在发布通路；响应不变。
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = approvals_router(fixture_approvals_state());
        let res = router
            .oneshot(authenticated_json_post(
                &format!("/api/v1/devices/{}/revoke", device_id(DEVICE_A)),
                serde_json::json!({
                    "expected_revision": "1", "reason": "compromised", "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let router = approvals_router(fixture_approvals_state());
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/approvals/approve",
                serde_json::json!({
                    "targets": [{ "device_id": device_id(DEVICE_A), "expected_revision": "1" }],
                    "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            collect_events(&mut rx).is_empty(),
            "events: None 不得有任何事件进入通道"
        );

        // 2) 有总线但 CAS 409：未发生状态变化即不发布。
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = approvals_router(approvals_state_with_events(
            FixtureApprovalsGateway::Conflict,
            fixture_session(true),
            Some(bus.clone()),
        ));
        let res = router
            .oneshot(authenticated_json_post(
                &format!("/api/v1/devices/{}/revoke", device_id(DEVICE_A)),
                serde_json::json!({
                    "expected_revision": "1", "reason": "compromised", "confirm": true,
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(collect_events(&mut rx).len(), 0);
        // 反向证据：同一订阅者确实能收到事件，说明上面的 0 条是「未发布」而非「订阅失效」。
        assert_eq!(
            bus.publish(EventEnvelope::device_updated(
                &device_id(DEVICE_A),
                1,
                EventObjectAudience::admins(),
            )),
            1
        );
    }
}

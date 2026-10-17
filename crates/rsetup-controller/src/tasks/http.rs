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

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{FromRef, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::ControllerError;
use crate::auth::service::Session;
use crate::http_api::{err_response, ok_response, request_id};
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{
    authenticate_read, authenticate_write, exhausted, not_ready, read_json_body,
    require_admin_session, single_header,
};
use crate::model::decimal_u64_json;
use crate::parse_device_id;
use crate::tasks::model::{EvidenceKind, MainTaskOutcome, MainTaskState, ReasonCode, SubTaskState};
use crate::tasks::preview::{PREVIEW_TTL, PreviewOutcome, is_canonical_uuid_v4_key};
use crate::tasks::recovery::calculate_main_task_outcome;
use crate::tasks::service::{SubmitOutcome, TimeReviewEvidenceKind};
use crate::time::evidence::TimeEvidence;

/// 默认分页大小（02 §1 `Page`）。
pub const DEFAULT_PAGE_LIMIT: usize = 50;
/// 单页上限（02 §3 批量上限 200）。
pub const MAX_PAGE_LIMIT: usize = 200;
/// 幂等键请求头（仅 `POST /tasks` 校验）。
const IDEMPOTENCY_KEY_HEADER: HeaderName = HeaderName::from_static("idempotency-key");
/// 游标 HMAC 域分隔常量：与 `SessionAliasKey` 同一风格，变更即令全部游标失效。
const TASK_CURSOR_DOMAIN: &[u8] = b"rsetup-task-cursor-v1/";
/// 游标字节布局：24B payload（`created_at_ms` 大端 8B ‖ `tie_break_id` 16B）+ 8B tag。
const TASK_CURSOR_BYTES: usize = 32;
/// HMAC tag 截断长度（与账本决议④补充 UUID 键 profile 一致）。
const TASK_CURSOR_TAG_BYTES: usize = 8;

/// 本任务唯一 operation（02 §6）：JSON 为 `"reboot"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOperation {
    Reboot,
}

/// 视图范围：全权 `full` 与授权子集 `authorized_subset`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewScope {
    Full,
    AuthorizedSubset,
}

/// 内部全集计数：10×u64（**不用 usize**）。跨 serde 时逐键 `to_string()` 输出
/// 十进制字符串，故不派生 `Serialize`（直接派生会退化成 JSON number）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskCounts {
    pub queued: u64,
    pub held: u64,
    pub dispatching: u64,
    pub accepted: u64,
    pub verifying: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub unknown: u64,
    pub cancelled: u64,
    pub expired: u64,
}

/// 唯一响应动作字段（仅表示"可请求取消"，不保证一定成功）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskActions {
    pub can_cancel: bool,
}

/// MainTask 授权投影：**只是内部 TaskApi 返回值/投影入参**，不是直接 serde HTTP 模型。
#[derive(Clone, Debug, PartialEq)]
pub struct TaskDetail {
    pub id: Uuid,
    pub operation: TaskOperation,
    pub state: MainTaskState,
    pub outcome: Option<MainTaskOutcome>,
    pub revision: u64,
    pub created_time: TimeEvidence,
    pub view_scope: ViewScope,
    /// 服务层自带全集计数：**投影层不读它**（F06：state/outcome/counts 的唯一
    /// 真相源是 `visible_states`），保留该字段只为与 TaskApi 契约同形。
    pub counts: TaskCounts,
    pub actions: TaskActions,
    /// 内部字段（非 HTTP 外部字段）：当前可见全集 states，与 state/outcome 同源。
    pub visible_states: Vec<SubTaskState>,
}

/// SubTask 授权投影条目（服务层入参）：`eligible` 由调用方按当前身份计算
/// （普通 = 当前持 `device.task.read`；admin = 事务内重验后恒 true）。
#[derive(Clone, Debug, PartialEq)]
pub struct ChildEntry {
    pub id: Uuid,
    pub device_id: String,
    pub state: SubTaskState,
    pub reason_code: Option<ReasonCode>,
    pub revision: u64,
    pub result_evidence: Option<EvidenceKind>,
    pub eligible: bool,
}

/// SubTask 授权投影（02 §1）：不暴露 `execution_token` 或其 token 摘要。
#[derive(Clone, Debug, PartialEq)]
pub struct ChildItem {
    pub id: Uuid,
    pub device_id: String,
    pub state: SubTaskState,
    pub reason_code: Option<ReasonCode>,
    /// 内部 u64；HTTP 输出十进制字符串（见 [`project_task_children`]）。
    pub revision: u64,
    pub result_evidence: Option<EvidenceKind>,
}

/// 主任务分页（02 §1 `Page{items,next_cursor}`）：内部返回值，不派生 serde。
#[derive(Clone, Debug, PartialEq)]
pub struct TaskPage {
    pub items: Vec<TaskDetail>,
    pub next_cursor: Option<String>,
}

/// 子任务分页（02 §1 `Page{items,next_cursor}`）：内部返回值，不派生 serde。
///
/// 与计划文本的 `items: Vec<ChildItem>` 相比改为 [`ChildEntry`]：`eligible` 是
/// 授权可见性的唯一判据，必须由服务层随条目一并给出、由投影层统一剔除，
/// 否则 HTTP 层无从区分"可见"与"已按身份过滤"（两处真相源）。
#[derive(Clone, Debug, PartialEq)]
pub struct ChildrenPage {
    pub items: Vec<ChildEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePreviewRequest {
    pub operation: TaskOperation,
    #[serde(default)]
    pub device_ids: Vec<String>,
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskRequest {
    pub preview_token: String,
    pub confirm: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTaskRequest {
    pub expected_revision: String,
    pub confirm: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeReviewDecision {
    ResumeOriginalDeadline,
    Expire,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeReviewTaskRequest {
    pub expected_revision: String,
    pub decision: TimeReviewDecision,
    pub evidence: TimeReviewEvidenceKind,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLockRequest {
    pub expected_revision: String,
    pub acknowledge_unknown: bool,
    pub reason: String,
}

/// `POST /subtasks/{id}/release-lock` 响应：仅两字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLockResponse {
    pub released: bool,
    pub state: SubTaskState,
}

/// `POST /tasks/{id}/cancel` 响应：接收确认 + 逐目标可读取结果。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTaskResponse {
    pub accepted: bool,
    pub cancelled: Option<Vec<Uuid>>,
}

/// 本事务实际 CAS 取消成功的目标（内部小类型，不暴露到 HTTP wire）。
///
/// `readable_in_tx` 生产组装来自当前 actor（或有效 admin）及该 device 的
/// `device.task.read` **事务授权快照**——不来自客户端声明、旧 `Session.is_admin`
/// 或 `has_any_taskread` 类整批布尔；该组装是 TaskApi 生产实现必须履约的依赖，
/// 本切片只钉投影契约，不假称已实现。
pub struct CancelledTarget {
    pub id: Uuid,
    pub device_id: [u8; 32],
    pub readable_in_tx: bool,
}

/// 对象安全 future 别名，保证 [`TaskApi`] object-safe。
pub type TaskFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// HTTP 边界消费的任务业务网关：只返回契约形状，内部服务类型不外泄。
///
/// 生产实现（`TaskService<SqlxTaskRepository>` 适配器）由唯一独立集成任务 owner
/// 装配；游标密钥（32B）由该生产实现持有，HTTP handler 层不经手密钥。
pub trait TaskApi: Send + Sync {
    fn preview<'a>(
        &'a self,
        actor: &'a Session,
        device_ids: &'a [[u8; 32]],
        group_ids: &'a [Uuid],
    ) -> TaskFuture<'a, PreviewOutcome>;
    fn submit<'a>(
        &'a self,
        actor: &'a Session,
        preview_token: &'a str,
        idempotency_key: &'a str,
    ) -> TaskFuture<'a, SubmitOutcome>;
    fn list_tasks<'a>(
        &'a self,
        actor: &'a Session,
        state: Option<MainTaskState>,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> TaskFuture<'a, TaskPage>;
    fn task_detail<'a>(&'a self, actor: &'a Session, task_id: Uuid) -> TaskFuture<'a, TaskDetail>;
    fn task_children<'a>(
        &'a self,
        actor: &'a Session,
        task_id: Uuid,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> TaskFuture<'a, ChildrenPage>;
    fn cancel<'a>(
        &'a self,
        actor: &'a Session,
        task_id: Uuid,
        request: CancelTaskRequest,
    ) -> TaskFuture<'a, CancelTaskResponse>;
    fn time_review<'a>(
        &'a self,
        actor: &'a Session,
        task_id: Uuid,
        request: TimeReviewTaskRequest,
    ) -> TaskFuture<'a, TaskDetail>;
    fn release_lock<'a>(
        &'a self,
        actor: &'a Session,
        subtask_id: Uuid,
        request: ReleaseLockRequest,
    ) -> TaskFuture<'a, ReleaseLockResponse>;
}

/// 任务路由 state：两字段**不扩**（游标密钥由 `api` 的生产实现持有）。
#[derive(Clone)]
pub struct HttpTasksState {
    pub api: Arc<dyn TaskApi>,
    pub auth: Arc<HttpAuthState>,
}

impl FromRef<HttpTasksState> for Arc<HttpAuthState> {
    fn from_ref(state: &HttpTasksState) -> Self {
        Arc::clone(&state.auth)
    }
}

impl FromRef<HttpTasksState> for Arc<dyn TaskApi> {
    fn from_ref(state: &HttpTasksState) -> Self {
        Arc::clone(&state.api)
    }
}

/// 业务会话前置：`must_change_password`（规格 01 A-02 临时密码）一律 403
/// `PASSWORD_CHANGE_REQUIRED`——**不要求** `is_admin`（admin 临时密码同样 403）、
/// 不复制 Host/Cookie/CSRF；缺失会话仍由共享 gate 严格 401 `AUTH_REQUIRED`。
#[allow(clippy::result_large_err)]
pub(crate) fn require_business_session(session: &Session) -> Result<(), Response> {
    if session.user.must_change_password {
        return Err(err_response(
            StatusCode::FORBIDDEN,
            "PASSWORD_CHANGE_REQUIRED",
            "auth.password_change_required",
            json!({}),
        ));
    }
    Ok(())
}

/// 8 条任务路由共用的唯一错误映射：既有契约码 + Task 4 两个新变体，
/// **无通用 500 兜底**（其余变体统一 503 `NOT_READY`，与既有 `backend_error` 一致）。
pub(crate) fn task_error_response(err: &ControllerError) -> Response {
    match err {
        ControllerError::PermissionDenied => err_response(
            StatusCode::FORBIDDEN,
            "PERMISSION_DENIED",
            "permission.denied",
            json!({}),
        ),
        ControllerError::InvalidArgument => err_response(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "task.invalid_request",
            json!({}),
        ),
        ControllerError::NotFound => err_response(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "system.not_found",
            json!({}),
        ),
        ControllerError::RevisionConflict => err_response(
            StatusCode::CONFLICT,
            "REVISION_CONFLICT",
            "task.revision_conflict",
            json!({}),
        ),
        ControllerError::TaskIdempotencyConflict => err_response(
            StatusCode::CONFLICT,
            "IDEMPOTENCY_CONFLICT",
            "task.idempotency_conflict",
            json!({}),
        ),
        ControllerError::TaskPreviewInvalidState => err_response(
            StatusCode::CONFLICT,
            "INVALID_STATE",
            "task.preview_invalid_state",
            json!({}),
        ),
        ControllerError::Database(_) => err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "STORAGE_UNAVAILABLE",
            "system.storage_unavailable",
            json!({}),
        ),
        ControllerError::ResourceExhausted => exhausted(),
        _ => not_ready(),
    }
}

/// 单元枚举 / `TimeEvidence` 的 serde 编码恒不失败（无自定义 `Serialize`），
/// 失败即为实现级缺陷，故 `expect` 而非静默降级为 `null`。
fn strict_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("内部枚举与时间证据的 serde 编码不会失败")
}

/// 预览有效期毫秒数：与 Task 4 `PREVIEW_TTL` 同源，由路由层补齐
/// （不改 `PreviewOutcome` 形状）。
fn preview_expires_in_ms() -> u64 {
    u64::try_from(PREVIEW_TTL.as_millis()).unwrap_or(60_000)
}

/// 严格无符号十进制 u64 解析：拒绝空串、非数字、正负号、下划线、小数点、
/// 前导零（`"0"` 除外）与溢出。
pub fn parse_decimal_u64(value: &str) -> Result<u64, ControllerError> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ControllerError::InvalidArgument);
    }
    if value.len() > 1 && value.starts_with('0') {
        return Err(ControllerError::InvalidArgument);
    }
    value
        .parse::<u64>()
        .map_err(|_| ControllerError::InvalidArgument)
}

/// 分页 limit：缺省 50，`1..=200` 合法，其余 `InvalidArgument`。
pub fn clamp_page_limit(requested: Option<usize>) -> Result<usize, ControllerError> {
    match requested {
        None => Ok(DEFAULT_PAGE_LIMIT),
        Some(value) if (1..=MAX_PAGE_LIMIT).contains(&value) => Ok(value),
        Some(_) => Err(ControllerError::InvalidArgument),
    }
}

/// 严格 canonical 小写 UUIDv4 幂等键（复用 Task 4 `is_canonical_uuid_v4_key`）；
/// 缺失或非法一律 `InvalidArgument`。
pub fn parse_idempotency_key(value: Option<&str>) -> Result<String, ControllerError> {
    match value.filter(|key| is_canonical_uuid_v4_key(key)) {
        Some(key) => Ok((*key).to_owned()),
        None => Err(ControllerError::InvalidArgument),
    }
}

/// 路径 ID：恰 36 字符 canonical 小写带连字符 UUIDv4（与 `is_canonical_uuid_v4_key`
/// 同一口径），其余一律 `InvalidArgument`（不交给 axum 默认 400 纯文本）。
fn parse_path_id(raw: &str) -> Result<Uuid, ControllerError> {
    if !is_canonical_uuid_v4_key(raw) {
        return Err(ControllerError::InvalidArgument);
    }
    Uuid::parse_str(raw).map_err(|_| ControllerError::InvalidArgument)
}

/// 取消前置判定（纯函数）：仅 `queued`/`held` 可取消；admin 无需逐目标资格，
/// 普通用户须同时是 owner 且持有该 device 的 `device.task.reboot`。
pub fn cancel_permitted(
    is_admin: bool,
    is_owner: bool,
    has_target_reboot: bool,
    state: SubTaskState,
) -> bool {
    let cancellable = matches!(state, SubTaskState::Queued | SubTaskState::Held);
    if is_admin {
        return cancellable;
    }
    is_owner && has_target_reboot && cancellable
}

/// release-lock 前置判定（纯函数）：管理员 + 风险确认 + 非空 reason + 目标 `unknown`。
pub fn release_lock_permitted(
    is_admin: bool,
    acknowledge_unknown: bool,
    reason: &str,
    state: SubTaskState,
) -> bool {
    is_admin && acknowledge_unknown && !reason.trim().is_empty() && state == SubTaskState::Unknown
}

/// time-review 前置判定（纯函数）：管理员且**至少一个** `held` 子任务（无 held → 400）。
pub fn time_review_permitted(is_admin: bool, held_count: usize) -> bool {
    is_admin && held_count > 0
}

/// 逐目标取消结果投影：仅 map `readable_in_tx == true` 的目标 id；空可读结果
/// （含无 CAS 成功目标）→ `cancelled = None`（非 `Some([])`，不误称取消全部）；
/// `accepted` 恒 true（仅为取消请求接收确认，非全部成功保证）。
pub fn project_cancel_result(targets: &[CancelledTarget]) -> CancelTaskResponse {
    let readable_ids: Vec<Uuid> = targets
        .iter()
        .filter(|target| target.readable_in_tx)
        .map(|target| target.id)
        .collect();
    CancelTaskResponse {
        accepted: true,
        cancelled: if readable_ids.is_empty() {
            None
        } else {
            Some(readable_ids)
        },
    }
}

/// MainTask 授权投影（详情与列表项**共用**）：state/outcome/counts 一律由
/// `visible_states` 全集重算（F06：绝不透传 caller 传入的全量 state/outcome）；
/// 可见全集为空 → `NotFound`（与"不存在"同投影，不输出零值汇总）。
#[allow(clippy::too_many_arguments)]
pub fn project_main_task_summary(
    id: Uuid,
    operation: TaskOperation,
    _state: MainTaskState,
    _outcome: Option<MainTaskOutcome>,
    revision: u64,
    created_time: &TimeEvidence,
    visible_states: &[SubTaskState],
    can_cancel: bool,
    is_full_access: bool,
) -> Result<Value, ControllerError> {
    if visible_states.is_empty() {
        return Err(ControllerError::NotFound);
    }
    let (calc_state, calc_outcome) = calculate_main_task_outcome(visible_states);
    let count = |want: SubTaskState| -> u64 {
        u64::try_from(
            visible_states
                .iter()
                .filter(|state| **state == want)
                .count(),
        )
        .unwrap_or(u64::MAX)
    };
    Ok(json!({
        "id": id.to_string(),
        "operation": strict_json(&operation),
        "state": strict_json(&calc_state),
        "outcome": strict_json(&calc_outcome),
        "revision": decimal_u64_json(revision),
        "created_time": strict_json(created_time),
        "view_scope": if is_full_access { "full" } else { "authorized_subset" },
        "counts": {
            "queued": decimal_u64_json(count(SubTaskState::Queued)),
            "held": decimal_u64_json(count(SubTaskState::Held)),
            "dispatching": decimal_u64_json(count(SubTaskState::Dispatching)),
            "accepted": decimal_u64_json(count(SubTaskState::Accepted)),
            "verifying": decimal_u64_json(count(SubTaskState::Verifying)),
            "succeeded": decimal_u64_json(count(SubTaskState::Succeeded)),
            "failed": decimal_u64_json(count(SubTaskState::Failed)),
            "unknown": decimal_u64_json(count(SubTaskState::Unknown)),
            "cancelled": decimal_u64_json(count(SubTaskState::Cancelled)),
            "expired": decimal_u64_json(count(SubTaskState::Expired)),
        },
        "actions": { "can_cancel": can_cancel }
    }))
}

/// 单个可见子任务条目的 HTTP 形状：`revision` 十进制字符串，不含
/// `execution_token`/token 摘要/`view_scope`/`counts`。
fn child_item_json(item: &ChildItem) -> Value {
    json!({
        "id": item.id.to_string(),
        "device_id": item.device_id,
        "state": strict_json(&item.state),
        "reason_code": strict_json(&item.reason_code),
        "revision": decimal_u64_json(item.revision),
        "result_evidence": strict_json(&item.result_evidence),
    })
}

/// `/tasks/{id}/children` 分页投影：`eligible == false` 的项**完全剔除**（不返回
/// `device_id`、不计入任何计数）；顶层恰 `items` + `next_cursor`，**绝不**输出
/// `view_scope`/`counts`/`total_targets`/`hidden_count`（main counts 独立重算）。
pub fn project_task_children(entries: &[ChildEntry], next_cursor: Option<&str>) -> Value {
    let items: Vec<Value> = entries
        .iter()
        .filter(|entry| entry.eligible)
        .map(|entry| {
            child_item_json(&ChildItem {
                id: entry.id,
                device_id: entry.device_id.clone(),
                state: entry.state,
                reason_code: entry.reason_code,
                revision: entry.revision,
                result_evidence: entry.result_evidence,
            })
        })
        .collect();
    json!({ "items": items, "next_cursor": next_cursor })
}

/// 游标端点上下文：HMAC 输入含 scope 字节，跨端点复用必在解码时失败。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskCursorScope {
    Tasks,
    TaskChildren,
}

impl TaskCursorScope {
    fn scope_byte(self) -> u8 {
        match self {
            TaskCursorScope::Tasks => 0x01,
            TaskCursorScope::TaskChildren => 0x02,
        }
    }
}

/// HMAC-SHA256（`sha2` 手写 ipad/opad，与 `auth::session::hmac_sha256` 同一构造；
/// 零新增依赖）。
fn hmac_sha256(key: &[u8; 32], domain: &[u8], scope_byte: u8, payload: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }
    let mut inner = sha2::Sha256::new();
    inner.update(ipad);
    inner.update(domain);
    inner.update([scope_byte]);
    inner.update(payload);
    let inner_hash = inner.finalize();

    let mut outer = sha2::Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// 恒定时间比较（长度已由调用方固定，不提前短路内容差异）。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 签发游标：`hex(payload ‖ HMAC-SHA256(secret, domain ‖ scope ‖ payload)[..8])`
/// = 64 小写 hex。密钥由调用方（TaskApi 生产实现）显式传入，HTTP 层不经手。
pub fn encode_task_cursor(
    endpoint: TaskCursorScope,
    created_at_ms: i64,
    tie_break_id: Uuid,
    secret: &[u8; 32],
) -> String {
    let mut payload = [0u8; 24];
    payload[..8].copy_from_slice(&created_at_ms.to_be_bytes());
    payload[8..].copy_from_slice(tie_break_id.as_bytes());
    let mac = hmac_sha256(secret, TASK_CURSOR_DOMAIN, endpoint.scope_byte(), &payload);

    let mut raw = [0u8; TASK_CURSOR_BYTES];
    raw[..24].copy_from_slice(&payload);
    raw[24..].copy_from_slice(&mac[..TASK_CURSOR_TAG_BYTES]);
    hex::encode(raw)
}

/// 解码游标，校验序**先 MAC 后字段**：① 恰 64 小写 hex → ② 以当前 scope 重算
/// HMAC 并恒定时间比较 8B tag → ③ tag 通过后才解析字段。任一步失败一律
/// `InvalidArgument`（400），不区分篡改/过期/外来细节，也不得回退到未分页全量。
pub fn decode_task_cursor(
    endpoint: TaskCursorScope,
    cursor: &str,
    secret: &[u8; 32],
) -> Result<(i64, Uuid), ControllerError> {
    if cursor.len() != 64
        || !cursor
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(ControllerError::InvalidArgument);
    }
    let mut raw = [0u8; TASK_CURSOR_BYTES];
    hex::decode_to_slice(cursor, &mut raw).map_err(|_| ControllerError::InvalidArgument)?;

    let mac = hmac_sha256(
        secret,
        TASK_CURSOR_DOMAIN,
        endpoint.scope_byte(),
        &raw[..24],
    );
    if !constant_time_eq(&raw[24..], &mac[..TASK_CURSOR_TAG_BYTES]) {
        return Err(ControllerError::InvalidArgument);
    }

    let created_at_ms = i64::from_be_bytes(
        raw[..8]
            .try_into()
            .map_err(|_| ControllerError::InvalidArgument)?,
    );
    let tie_break_id = Uuid::from_bytes(
        raw[8..24]
            .try_into()
            .map_err(|_| ControllerError::InvalidArgument)?,
    );
    Ok((created_at_ms, tie_break_id))
}

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
        if let Err(err) = parse_decimal_u64(&request.expected_revision) {
            return task_error_response(&err);
        }
        if !request.confirm {
            return err_response(
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "task.confirm_required",
                json!({}),
            );
        }
        match state.api.cancel(&session, task_id, request).await {
            Ok(response) => ok_response(strict_json(&response)),
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
                Ok(view) => ok_response(view),
                Err(err) => task_error_response(&err),
            },
            // 本端点 DTO 已拒绝未知字段与未知枚举；服务侧 `InvalidArgument` 仍有两
            // 个业务成因——「任务不存在」与「主任务无 held 子任务」（Task 4
            // `time_review_in_tx` 两处分支共用同一变体，service.rs 越出本任务写
            // 作用域，HTTP 层以只读 `task_detail` 复核区分）：复核确认 NotFound →
            // 标准 404；其余（任务存在、复核不可用等）→ 400 `task.no_held`。
            Err(ControllerError::InvalidArgument) => {
                match state.api.task_detail(&session, task_id).await {
                    Err(err @ ControllerError::NotFound) => task_error_response(&err),
                    _ => err_response(
                        StatusCode::BAD_REQUEST,
                        "INVALID_ARGUMENT",
                        "task.no_held",
                        json!({}),
                    ),
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
        if let Err(err) = parse_decimal_u64(&request.expected_revision) {
            return task_error_response(&err);
        }
        if let Err(response) = require_admin_session(&session) {
            return response;
        }
        match state.api.release_lock(&session, subtask_id, request).await {
            Ok(response) => ok_response(strict_json(&response)),
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
    use super::*;
    use crate::auth::service::{IdentityUser, Login, SessionPage};
    use crate::auth::session::token_digest;
    use crate::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding};
    use crate::http_security::HttpSecurityConfig;
    use crate::tasks::model::ReasonCode;
    use crate::tasks::preview::PreviewTarget;
    use crate::time::evidence::{TimeEvidence, TimeQuality};
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use chrono::{DateTime, Utc};
    use std::sync::Mutex;
    use std::time::Instant;
    use tower::ServiceExt;

    const RAW_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const CSRF_TOKEN: &str = "csrf-token-fixture";
    const IDEMPOTENCY_KEY: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    const CREATED_AT_MS: i64 = 1_725_000_000_123;
    const SECRET: &[u8; 32] = &[0x5A; 32];

    fn fixture_time_evidence() -> TimeEvidence {
        let ts = DateTime::parse_from_rfc3339("2026-10-16T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        TimeEvidence {
            system_wall_utc: ts,
            reference_utc: ts,
            quality: TimeQuality::SystemFallback,
            clock_epoch: Uuid::new_v4(),
            source: None,
            sample_age_ms: None,
            offset_ms: None,
            uncertainty_ms: None,
        }
    }

    fn fixture_session(is_admin: bool, must_change_password: bool) -> Session {
        Session {
            user: IdentityUser {
                id: [7u8; 16],
                username: if is_admin {
                    "admin".into()
                } else {
                    "alice".into()
                },
                password_hash: "$argon2id$=synthetic-test-only=".to_string(),
                active: true,
                is_admin,
                must_change_password,
                revision: 1,
            },
            digest: token_digest(RAW_TOKEN.as_bytes()),
        }
    }

    fn fixture_detail() -> TaskDetail {
        TaskDetail {
            id: Uuid::from_u128(0x1234567812344abc80de123456789a01),
            operation: TaskOperation::Reboot,
            state: MainTaskState::Running,
            outcome: None,
            revision: 4,
            created_time: fixture_time_evidence(),
            view_scope: ViewScope::AuthorizedSubset,
            counts: TaskCounts {
                queued: 0,
                held: 0,
                dispatching: 0,
                accepted: 0,
                verifying: 0,
                succeeded: 1,
                failed: 0,
                unknown: 1,
                cancelled: 0,
                expired: 0,
            },
            actions: TaskActions { can_cancel: false },
            visible_states: vec![SubTaskState::Succeeded, SubTaskState::Unknown],
        }
    }

    // ── 授权投影纯函数 ────────────────────────────────────────────────

    #[test]
    fn main_detail_summary_counts_are_decimal_strings_over_authorized_full_set() {
        let main_id = Uuid::new_v4();
        let created = fixture_time_evidence();
        let visible = [SubTaskState::Succeeded, SubTaskState::Unknown];
        let view = project_main_task_summary(
            main_id,
            TaskOperation::Reboot,
            MainTaskState::Running,
            None,
            4,
            &created,
            &visible,
            false,
            false,
        )
        .unwrap();
        assert_eq!(view["view_scope"], "authorized_subset");
        assert_eq!(view["counts"]["succeeded"], "1");
        assert_eq!(view["counts"]["unknown"], "1");
        assert_eq!(view["counts"]["failed"], "0");
        assert_eq!(view["counts"]["dispatching"], "0");
        assert_eq!(view["counts"]["expired"], "0");
        assert!(view.get("total_targets").is_none());
        assert!(view.get("hidden_count").is_none());
        assert_eq!(view["revision"], "4");
        assert_eq!(view["operation"], "reboot");
        assert_eq!(view["actions"]["can_cancel"], false);
        assert!(view["created_time"]["quality"].is_string());
        assert!(view["created_time"]["clock_epoch"].is_string());

        let full = project_main_task_summary(
            main_id,
            TaskOperation::Reboot,
            MainTaskState::Running,
            None,
            4,
            &created,
            &visible,
            true,
            true,
        )
        .unwrap();
        assert_eq!(full["view_scope"], "full");
        assert!(full.get("total_targets").is_none());
    }

    #[test]
    fn main_summary_recomputes_from_visible_full_set_ignoring_full_state() {
        let main_id = Uuid::new_v4();
        let created = fixture_time_evidence();
        let visible = [
            SubTaskState::Succeeded,
            SubTaskState::Succeeded,
            SubTaskState::Succeeded,
        ];
        let view = project_main_task_summary(
            main_id,
            TaskOperation::Reboot,
            MainTaskState::Running,
            None,
            7,
            &created,
            &visible,
            false,
            false,
        )
        .unwrap();
        assert_eq!(view["state"], "completed");
        assert_eq!(view["outcome"], "success");
        assert_eq!(view["counts"]["succeeded"], "3");
        assert_eq!(view["counts"]["failed"], "0");
        assert_eq!(view["counts"]["unknown"], "0");
        assert_eq!(view["counts"]["queued"], "0");
        assert_eq!(view["view_scope"], "authorized_subset");
        assert!(view.get("total_targets").is_none());
        assert!(view.get("hidden_count").is_none());
        assert_eq!(view["revision"], "7");

        let view2 = project_main_task_summary(
            main_id,
            TaskOperation::Reboot,
            MainTaskState::Completed,
            Some(MainTaskOutcome::Partial),
            7,
            &created,
            &visible,
            false,
            false,
        )
        .unwrap();
        assert_eq!(view2["state"], "completed");
        assert_eq!(view2["outcome"], "success");
        assert_eq!(view2["counts"]["succeeded"], "3");

        let empty = project_main_task_summary(
            main_id,
            TaskOperation::Reboot,
            MainTaskState::Completed,
            Some(MainTaskOutcome::Success),
            7,
            &created,
            &[],
            false,
            false,
        );
        assert!(
            matches!(empty, Err(ControllerError::NotFound)),
            "空 visible 全集必须 NotFound（404），不得输出零值汇总"
        );
    }

    #[test]
    fn children_page_projection_is_independent_from_main_counts() {
        let dev_ok = "aa".repeat(32);
        let dev_hidden = "bb".repeat(32);
        let entries = vec![
            ChildEntry {
                id: Uuid::new_v4(),
                device_id: dev_ok.clone(),
                state: SubTaskState::Succeeded,
                reason_code: None,
                revision: 3,
                result_evidence: Some(EvidenceKind::BootTransitionObserved),
                eligible: true,
            },
            ChildEntry {
                id: Uuid::new_v4(),
                device_id: dev_hidden.clone(),
                state: SubTaskState::Succeeded,
                reason_code: None,
                revision: 3,
                result_evidence: None,
                eligible: false,
            },
        ];
        let view = project_task_children(&entries, None);
        assert!(view.get("view_scope").is_none());
        assert!(view.get("counts").is_none());
        assert!(
            view.get("next_cursor")
                .expect("Page 必须含 next_cursor")
                .is_null(),
            "无下一页时 next_cursor 必须是 JSON null"
        );
        let items = view["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["state"], "succeeded");
        assert_eq!(items[0]["revision"], "3");
        assert_eq!(items[0]["result_evidence"], "boot_transition_observed");
        assert!(items[0].get("total_targets").is_none());
        assert!(items[0].get("execution_token").is_none());

        let full_entries = vec![
            ChildEntry {
                id: Uuid::new_v4(),
                device_id: dev_ok,
                state: SubTaskState::Succeeded,
                reason_code: None,
                revision: 3,
                result_evidence: Some(EvidenceKind::BootTransitionObserved),
                eligible: true,
            },
            ChildEntry {
                id: Uuid::new_v4(),
                device_id: dev_hidden,
                state: SubTaskState::Succeeded,
                reason_code: None,
                revision: 3,
                result_evidence: None,
                eligible: true,
            },
        ];
        let cursor = "01".repeat(32);
        let full = project_task_children(&full_entries, Some(cursor.as_str()));
        let full_items = full["items"].as_array().unwrap();
        assert_eq!(full_items.len(), 2);
        assert_eq!(full_items[1]["device_id"], "bb".repeat(32));
        assert_eq!(full["next_cursor"], "01".repeat(32));
        assert!(full.get("counts").is_none());
    }

    #[test]
    fn cancel_permission_matrix_is_exact() {
        assert!(cancel_permitted(true, false, false, SubTaskState::Queued));
        assert!(cancel_permitted(true, false, false, SubTaskState::Held));
        assert!(!cancel_permitted(
            true,
            true,
            true,
            SubTaskState::Dispatching
        ));
        assert!(!cancel_permitted(true, true, true, SubTaskState::Accepted));
        assert!(!cancel_permitted(true, true, true, SubTaskState::Verifying));
        assert!(!cancel_permitted(true, true, true, SubTaskState::Unknown));
        assert!(cancel_permitted(false, true, true, SubTaskState::Queued));
        assert!(!cancel_permitted(false, true, false, SubTaskState::Queued));
        assert!(!cancel_permitted(false, false, true, SubTaskState::Queued));
    }

    #[test]
    fn cancel_result_projection_filters_unreadable_targets() {
        let a_id = Uuid::from_u128(0x1234567812344abc80de123456789a01);
        let b_id = Uuid::from_u128(0x1234567812344abc80de123456789a02);

        let targets = vec![
            CancelledTarget {
                id: a_id,
                device_id: [0xAA; 32],
                readable_in_tx: true,
            },
            CancelledTarget {
                id: b_id,
                device_id: [0xBB; 32],
                readable_in_tx: false,
            },
        ];
        let resp = project_cancel_result(&targets);
        assert!(resp.accepted, "accepted 仅为取消请求接收确认，恒 true");
        assert_eq!(resp.cancelled, Some(vec![a_id]), "不可读 B 不得泄露");

        let all_hidden = vec![
            CancelledTarget {
                id: a_id,
                device_id: [0xAA; 32],
                readable_in_tx: false,
            },
            CancelledTarget {
                id: b_id,
                device_id: [0xBB; 32],
                readable_in_tx: false,
            },
        ];
        let hidden = project_cancel_result(&all_hidden);
        assert!(hidden.accepted);
        assert_eq!(hidden.cancelled, None);

        let empty = project_cancel_result(&[]);
        assert!(empty.accepted);
        assert_eq!(empty.cancelled, None);
    }

    #[test]
    fn release_lock_requires_admin_ack_and_nonempty_reason() {
        assert!(release_lock_permitted(
            true,
            true,
            "现场已人工核实",
            SubTaskState::Unknown
        ));
        assert!(!release_lock_permitted(
            false,
            true,
            "现场已人工核实",
            SubTaskState::Unknown
        ));
        assert!(!release_lock_permitted(
            true,
            false,
            "现场已人工核实",
            SubTaskState::Unknown
        ));
        assert!(!release_lock_permitted(
            true,
            true,
            "   ",
            SubTaskState::Unknown
        ));
        assert!(!release_lock_permitted(
            true,
            true,
            "ok",
            SubTaskState::Succeeded
        ));
    }

    #[test]
    fn time_review_requires_at_least_one_held_subtask() {
        assert!(time_review_permitted(true, 1));
        assert!(time_review_permitted(true, 5));
        assert!(!time_review_permitted(true, 0));
        assert!(!time_review_permitted(false, 3));
    }

    #[test]
    fn decimal_u64_rejects_negative_leading_zero_and_overflow() {
        assert_eq!(parse_decimal_u64("0").unwrap(), 0);
        assert_eq!(parse_decimal_u64("18446744073709551615").unwrap(), u64::MAX);
        for bad in [
            "",
            " ",
            "-1",
            "+1",
            "01",
            "007",
            "1_0",
            "1.0",
            "abc",
            "99999999999999999999999",
        ] {
            assert!(
                matches!(
                    parse_decimal_u64(bad),
                    Err(ControllerError::InvalidArgument)
                ),
                "非法十进制字符串必须 InvalidArgument: {bad:?}"
            );
        }
    }

    #[test]
    fn page_limit_defaults_to_50_and_caps_at_200() {
        assert_eq!(clamp_page_limit(None).unwrap(), 50);
        assert_eq!(clamp_page_limit(Some(1)).unwrap(), 1);
        assert_eq!(clamp_page_limit(Some(200)).unwrap(), 200);
        for bad in [0usize, 201, 1_000] {
            assert!(
                matches!(
                    clamp_page_limit(Some(bad)),
                    Err(ControllerError::InvalidArgument)
                ),
                "越界 limit 必须 InvalidArgument: {bad}"
            );
        }
    }

    #[test]
    fn idempotency_key_must_be_canonical_lowercase_uuidv4() {
        assert_eq!(
            parse_idempotency_key(Some(IDEMPOTENCY_KEY)).unwrap(),
            IDEMPOTENCY_KEY
        );
        assert!(matches!(
            parse_idempotency_key(None),
            Err(ControllerError::InvalidArgument)
        ));
        for bad in [
            "",
            "3F2504E0-4F89-41D3-9A0C-0305E82C3301",
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "3f2504e04f8941d39a0c0305e82c3301",
            "not-a-uuid",
        ] {
            assert!(
                matches!(
                    parse_idempotency_key(Some(bad)),
                    Err(ControllerError::InvalidArgument)
                ),
                "非法幂等键必须 InvalidArgument: {bad:?}"
            );
        }
    }

    // ── 游标编解码 ────────────────────────────────────────────────────

    fn tie_break_id() -> Uuid {
        Uuid::parse_str("7c9e6679-7425-40de-944b-e07fc1f90ae7").unwrap()
    }

    #[test]
    fn task_cursor_round_trip_success() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        assert_eq!(token.len(), 64);
        assert!(
            token
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        let decoded = decode_task_cursor(TaskCursorScope::Tasks, &token, SECRET)
            .expect("同 secret 同 scope 必须精确解回");
        assert_eq!(decoded, (CREATED_AT_MS, tie_break_id()));
    }

    #[test]
    fn task_cursor_rejects_tampered_tag_with_invalid_argument() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        let mut tampered: Vec<char> = token.chars().collect();
        let last = tampered.len() - 1;
        tampered[last] = if tampered[last] == '0' { '1' } else { '0' };
        let tampered: String = tampered.into_iter().collect();
        let err = decode_task_cursor(TaskCursorScope::Tasks, &tampered, SECRET)
            .expect_err("篡改 tag 必须失败");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }

    #[test]
    fn task_cursor_rejects_cross_endpoint_with_invalid_argument() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        let err = decode_task_cursor(TaskCursorScope::TaskChildren, &token, SECRET)
            .expect_err("游标不得跨端点复用");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }

    #[test]
    fn task_cursor_rejects_malformed_length_or_hex() {
        let token = encode_task_cursor(
            TaskCursorScope::Tasks,
            CREATED_AT_MS,
            tie_break_id(),
            SECRET,
        );
        let mut too_short = token.clone();
        too_short.pop();
        let mut too_long = token.clone();
        too_long.push('0');
        let mut non_hex = token.clone();
        non_hex.replace_range(0..1, "z");
        let cases = [too_short, too_long, non_hex, token.to_uppercase()];
        for case in cases {
            let err = decode_task_cursor(TaskCursorScope::Tasks, &case, SECRET)
                .expect_err("格式非法必须失败");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
    }

    // ── 业务会话 gate 与统一错误映射 ─────────────────────────────────

    #[tokio::test]
    async fn temporary_password_forbids_admin_and_plain_with_exact_envelope() {
        for is_admin in [true, false] {
            let resp = require_business_session(&fixture_session(is_admin, true))
                .expect_err("临时密码（含 admin）必须 403");
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "is_admin={is_admin}");
            let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["error"]["code"],
                json!("PASSWORD_CHANGE_REQUIRED"),
                "is_admin={is_admin}"
            );
            assert_eq!(
                body["error"]["message_key"],
                json!("auth.password_change_required"),
                "is_admin={is_admin}"
            );
            assert_eq!(body["error"]["params"], json!({}), "is_admin={is_admin}");
            let error_keys: Vec<&str> = body["error"]
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                error_keys.len(),
                3,
                "error 必须恰为 code/message_key/params: {body}"
            );
            let top_keys: Vec<&str> = body
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                top_keys.len(),
                2,
                "envelope 必须恰为 error+request_id: {body}"
            );
        }
    }

    #[test]
    fn normal_session_passes_for_plain_and_admin() {
        for is_admin in [false, true] {
            assert!(
                require_business_session(&fixture_session(is_admin, false)).is_ok(),
                "正常（非临时密码）会话必须通过, is_admin={is_admin}"
            );
        }
    }

    async fn status_and_code(err: &ControllerError) -> (u16, String, String) {
        let resp = task_error_response(err);
        let status = resp.status().as_u16();
        let body = to_bytes(resp.into_body(), 1_048_576).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        (
            status,
            value["error"]["code"].as_str().unwrap().to_owned(),
            value["error"]["message_key"].as_str().unwrap().to_owned(),
        )
    }

    #[tokio::test]
    async fn error_mapping_is_exact_for_every_contract_variant() {
        assert_eq!(
            status_and_code(&ControllerError::TaskIdempotencyConflict).await,
            (
                409,
                "IDEMPOTENCY_CONFLICT".into(),
                "task.idempotency_conflict".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::TaskPreviewInvalidState).await,
            (
                409,
                "INVALID_STATE".into(),
                "task.preview_invalid_state".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::RevisionConflict).await,
            (
                409,
                "REVISION_CONFLICT".into(),
                "task.revision_conflict".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::NotFound).await,
            (404, "NOT_FOUND".into(), "system.not_found".into())
        );
        assert_eq!(
            status_and_code(&ControllerError::PermissionDenied).await,
            (403, "PERMISSION_DENIED".into(), "permission.denied".into())
        );
        assert_eq!(
            status_and_code(&ControllerError::InvalidArgument).await,
            (
                400,
                "INVALID_ARGUMENT".into(),
                "task.invalid_request".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::Database(sqlx::Error::RowNotFound)).await,
            (
                503,
                "STORAGE_UNAVAILABLE".into(),
                "system.storage_unavailable".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::ResourceExhausted).await,
            (
                503,
                "RESOURCE_EXHAUSTED".into(),
                "system.resource_exhausted".into()
            )
        );
        assert_eq!(
            status_and_code(&ControllerError::Crypto).await,
            (503, "NOT_READY".into(), "system.not_ready".into())
        );
    }

    #[tokio::test]
    async fn resource_exhausted_carries_retry_after_60() {
        let resp = task_error_response(&ControllerError::ResourceExhausted);
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("60")
        );
    }

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
            let failure = self.detail_failure.or(self.failure).map(FakeFailure::error);
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
        build_tasks_router(HttpTasksState { api, auth })
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
        let mut fake = FakeTaskApi::new();
        fake.failure = Some(FakeFailure::InvalidArgument);
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
}

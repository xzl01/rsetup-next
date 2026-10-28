use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg(doc)]
use super::project_task_children;
use crate::ControllerError;
use crate::auth::service::Session;
use crate::tasks::model::{EvidenceKind, MainTaskOutcome, MainTaskState, ReasonCode, SubTaskState};
use crate::tasks::preview::PreviewOutcome;
use crate::tasks::service::{SubmitOutcome, TimeReviewEvidenceKind};
use crate::time::evidence::TimeEvidence;

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

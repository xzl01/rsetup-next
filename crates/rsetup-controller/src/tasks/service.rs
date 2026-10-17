//! 任务编排服务：预览签发、原子提交（幂等 + 设备唯一操作锁）、取消、时限复核与解除占用。
//!
//! 事务纪律：一律「短事务 + 事务内零网络」，事务内绝不调用任何 Board 客户端；
//! 锁序固定为 `schema_meta → users → roles → device_groups → devices →
//! device_operation_locks`，不得逆序、不得重复锁同一行。
//!
//! 时钟纪律（RT-N5）：时限复核只接受服务端可信时钟快照（`TimeEvidence`），
//! 决不接受请求侧自报时钟或自报证据种类参与分派；任何路径都不会把
//! `created_at` / `observe_deadline` 改成更晚的值。
//!
//! 两条期限列各记各的含义，绝不互相顶替（`sub_tasks` DDL 为真相源）：
//! `queue_deadline` 是**排队截止**（04 §5.1 排队有效期），由创建时可信时基
//! （[`queue_deadline_of`]）算出，是时限复核判定 `held` 是否过期的唯一比对依据；
//! `observe_deadline` 是**观察窗截止**（04 §5.2/§5.3，自 `dispatching` 起算），
//! 由派发路径持有，复核路径只读不写、更不拿它替代排队截止。
//!
//! 行读取口径：本 crate 的 `sqlx` 未启用 `derive` 特性，原始行统一经
//! [`tx_rows`] / [`tx_row`] + [`column`] 显式解码，checked 转换失败一律 503。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use chrono::{DateTime, Utc};
use sqlx::Transaction;
use sqlx::mysql::{MySql, MySqlArguments};
use uuid::Uuid;

use crate::authz::policy::{Permission, eval_memory_permissions};
use crate::authz::sqlx_repo::{AuthorizationSnapshot, load_snapshot_in_tx};
use crate::error::ControllerError;
use crate::tasks::model::{MainTaskOutcome, MainTaskState, ReasonCode, SubTaskState};
use crate::tasks::preview::{
    PREVIEW_TTL, PreviewOutcome, PreviewTarget, decode_preview_targets, device_target_hash,
    is_canonical_uuid_v4_key, new_preview_token, preview_generation_bytes, preview_mono_valid,
    preview_token_bytes, preview_token_digest, preview_token_nonce, validate_target_count,
};
use crate::tasks::repository::{
    TaskRepository, column, enum_wire, fixed_mysql_bytes, mysql_uuid, task_row_decode_error,
    tx_execute, tx_row, tx_rows,
};
use crate::time::evidence::{TimeEvidence, TimeQuality};

/// 进程内审计事件序号；与 `audit_events` 的 `(process_epoch, event_seq)` 唯一键同源。
static TASK_PROCESS_EPOCH: OnceLock<Uuid> = OnceLock::new();
static TASK_EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// 时限复核的证据种类（仅记审计，绝不作为分派依据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeReviewEvidenceKind {
    ReferenceDeadlineProven,
    ReferenceDeadlineExpired,
    Inconclusive,
}

/// 复核结论：只能恢复**原**期限或判过期，绝不写入更晚的期限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeReviewDecision {
    ResumeOriginalDeadline,
    Expire,
}

/// 是否允许「延长」期限：仅当证据为 `ReferenceDeadlineProven` 且延长量为 `0`
/// （即恢复原期限）时放行；任何正数延长一律拒绝，绝不延长。
pub fn time_review_allows_extend(
    requested_extension_ms: i64,
    kind: TimeReviewEvidenceKind,
) -> bool {
    matches!(kind, TimeReviewEvidenceKind::ReferenceDeadlineProven) && requested_extension_ms == 0
}

/// 排队有效期（04 §5.1 建议 60min）：`sub_tasks.queue_deadline` 的唯一 TTL 来源。
pub const QUEUE_TTL_MS: i64 = 3_600_000;

/// 排队截止：与时限复核的比对处于**同一可信时基**，绝不由 DB 宿主墙钟
/// （`DATE_ADD(NOW(6), …)`）生成——数据库漂移会变成排队寿命错配。
///
/// - `NtpValid`：`reference_utc` 是静态采样时刻，须累加样本年龄 `sample_age_ms`，
///   否则样本越旧、排队寿命被隐性拉长（缺失按 0 处理）；
/// - `SystemFallback` / `Stale`：`reference_utc` 已由时钟源刷新为当前墙钟，
///   直接加 TTL，不重复累加历史 age，否则排队截止被双重放大。
///
/// 溢出（TTL 或 age 大到不可表示）返回 `None`，由调用方 fail-closed 拒绝创建。
pub fn queue_deadline_of(created: &TimeEvidence) -> Option<DateTime<Utc>> {
    let age_ms = match created.quality {
        // 样本年龄缺失按 0 处理：缺列不等于「无限陈旧」，更不得让创建失败。
        TimeQuality::NtpValid => created.sample_age_ms.unwrap_or(0),
        TimeQuality::SystemFallback | TimeQuality::Stale => 0,
    };
    let age = chrono::Duration::try_milliseconds(i64::try_from(age_ms).ok()?)?;
    let ttl = chrono::Duration::try_milliseconds(QUEUE_TTL_MS)?;
    created
        .reference_utc
        .checked_add_signed(age)?
        .checked_add_signed(ttl)
}

/// 推进后的当前参考时刻：`reference_utc` 是静态 NTP 采样时刻（driver 周期性复验
/// 才刷新），直接当当前时刻比对，会把「采样时刻 < 期限 < 真实当前时刻」的已过期
/// 任务误判为未过期，因此必须先累加样本年龄再比对。
///
/// 溢出（age 大到不可表示）取 `DateTime::MAX_UTC`：视为早已越过任何合理期限，
/// 绝不回退到采样时刻。
pub fn advanced_reference_utc(clock: &TimeEvidence) -> DateTime<Utc> {
    let age = match clock.sample_age_ms {
        None => chrono::Duration::zero(),
        Some(ms) => chrono::Duration::try_milliseconds(i64::try_from(ms).unwrap_or(i64::MAX))
            .unwrap_or_else(|| chrono::Duration::seconds(i64::MAX / 4)),
    };
    clock
        .reference_utc
        .checked_add_signed(age)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// 时间安全核验的纯策略结论：时限复核分派的唯一依据（`evidence_kind` 只记审计）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeReviewVerdict {
    /// 原排队期限的有效性**无法证明**：当前或创建时时钟不可信、时钟纪元不连续、
    /// 创建证据缺失/不可解析，或恢复决策下已存在过期子任务。
    /// 一律保持 `held`、零状态变更（回滚），绝不盲目恢复。
    Unverifiable,
    /// 全部未过期且请求恢复原期限：整批 `held → queued`，不写任何更晚期限。
    Resume,
    /// 全部已过期且请求判过期：整批 `expired` + `QUEUE_EXPIRED` 并释放本代际设备锁。
    Expire,
    /// 请求与服务端真实比对矛盾（仍有未过期子任务却请求整批判过期）：拒绝。
    Contradiction,
}

/// 纯策略 seam：由创建时证据、当前时钟快照与全部 `held` 子任务的**排队截止**
/// 推出唯一允许的处置，零 IO、零副作用。
///
/// `queue_deadlines` 是被锁定的全部 `held` 子任务的 `queue_deadline`；`None`
/// 表示列值缺失，无法证明未过期，按已过期处置。空集合由调用方先行拒绝。
pub fn time_review_verdict(
    created: Option<&TimeEvidence>,
    clock: &TimeEvidence,
    decision: TimeReviewDecision,
    queue_deadlines: &[Option<DateTime<Utc>>],
) -> TimeReviewVerdict {
    // 当前时钟不可信（Stale/SystemFallback）：无法证明任何期限的有效性。
    if clock.quality != TimeQuality::NtpValid {
        return TimeReviewVerdict::Unverifiable;
    }
    // 创建证据缺失或不可解析：`queue_deadline` 的来源时基无法证明可信。
    let Some(created) = created else {
        return TimeReviewVerdict::Unverifiable;
    };
    // 创建当时时钟不可信（纪元虽连续也一样），或跳钟/进程换代导致纪元不连续：
    // 原排队期限的参考系连续性无法证明，绝不盲目恢复。
    if created.quality != TimeQuality::NtpValid || created.clock_epoch != clock.clock_epoch {
        return TimeReviewVerdict::Unverifiable;
    }
    let now = advanced_reference_utc(clock);
    let expired = |deadline: &Option<DateTime<Utc>>| deadline.is_none_or(|deadline| now > deadline);
    let expired_count = queue_deadlines
        .iter()
        .filter(|deadline| expired(deadline))
        .count();
    match decision {
        // 要求**全部**未过期才允许整批恢复；存在任一过期即绝不整批恢复。
        TimeReviewDecision::ResumeOriginalDeadline if expired_count == 0 => {
            TimeReviewVerdict::Resume
        }
        TimeReviewDecision::ResumeOriginalDeadline => TimeReviewVerdict::Unverifiable,
        // 要求**全部**已过期才允许整批判过期；仍有未过期者即误伤，拒绝矛盾请求。
        TimeReviewDecision::Expire if expired_count == queue_deadlines.len() => {
            TimeReviewVerdict::Expire
        }
        TimeReviewDecision::Expire => TimeReviewVerdict::Contradiction,
    }
}

/// 解析主任务 `created_time_json` 为创建时时钟证据；缺失或不可解析一律 `None`
/// （无法证明原期限有效，fail-closed 保持 `held`），绝不猜测近似值。
fn parse_created_time_evidence(raw: &str) -> Option<TimeEvidence> {
    serde_json::from_str(raw).ok()
}

/// 取消前置条件：只有 `queued` / `held` 可取消，且要求管理员或持有该设备的
/// reboot 权限（04 §8：`dispatching` 及之后的状态一律不可取消）。
pub fn cancel_precondition(state: SubTaskState, is_admin: bool, has_reboot: bool) -> bool {
    matches!(state, SubTaskState::Queued | SubTaskState::Held) && (is_admin || has_reboot)
}

/// 终态判定（本地口径；Task 5 聚合器落地后由其统一提供，两者必须同口径）。
fn is_terminal_state(state: SubTaskState) -> bool {
    matches!(
        state,
        SubTaskState::Succeeded
            | SubTaskState::Failed
            | SubTaskState::Unknown
            | SubTaskState::Cancelled
            | SubTaskState::Expired
    )
}

/// 十态全集的固定写出顺序与 wire 键（`counts_json` 十键口径的唯一生产者）。
/// 键与 Task 2 serde 定义的一致性由 `state_keys_match_serde_wire` 钉死。
const SUB_TASK_STATE_KEYS: [(SubTaskState, &str); 10] = [
    (SubTaskState::Queued, "queued"),
    (SubTaskState::Held, "held"),
    (SubTaskState::Dispatching, "dispatching"),
    (SubTaskState::Accepted, "accepted"),
    (SubTaskState::Verifying, "verifying"),
    (SubTaskState::Succeeded, "succeeded"),
    (SubTaskState::Failed, "failed"),
    (SubTaskState::Unknown, "unknown"),
    (SubTaskState::Cancelled, "cancelled"),
    (SubTaskState::Expired, "expired"),
];

/// `counts_json` 唯一生产者：十键频数对象，值为十进制字符串（与 Task 6
/// `project_main_task_summary` 十键口径同源）。未列出的状态计 0。
pub fn counts_json_of(counts: &[(SubTaskState, u64)]) -> String {
    let mut totals: HashMap<SubTaskState, u64> = HashMap::new();
    for (state, count) in counts {
        *totals.entry(*state).or_insert(0) += count;
    }
    let mut object = serde_json::Map::new();
    for (state, key) in SUB_TASK_STATE_KEYS {
        object.insert(
            key.to_owned(),
            serde_json::Value::String(totals.get(&state).copied().unwrap_or(0).to_string()),
        );
    }
    serde_json::Value::Object(object).to_string()
}

/// 主任务创建时的十键全零占位；真实频数在子任务落库后同事务回写。
pub fn empty_counts_json() -> String {
    counts_json_of(&[])
}

/// 提交路径的 counts：`queued = 抢到锁的目标数`、`failed = 设备忙 + 无权目标数`。
pub fn submit_counts_json(locked: u64, device_busy: u64) -> String {
    counts_json_of(&[
        (SubTaskState::Queued, locked),
        (SubTaskState::Failed, device_busy),
    ])
}

/// 主任务收敛（本地实现；Task 5 唯一聚合器 `calculate_main_task_outcome` 落地后由其
/// 替换，两者口径必须一致）：严格按 04 §4 的优先级汇总，**顺序不可调换**。
///
/// 1. `running`：存在 `dispatching`/`accepted`/`verifying`，或批次已开始（至少一个
///    子任务已离开 `queued`）且仍有可运行 `queued`；
/// 2. `queued`：尚未开始（全部仍为 `queued`）；
/// 3. `held`：无执行中、无可运行 `queued`，且至少一个 `held`（**不是** `running`）；
/// 4. `completed`：全部属于批次终态，outcome 优先级为
///    含 `unknown` → `unknown` ＞ 全 `succeeded` → `success` ＞ 全 `cancelled` →
///    `cancelled` ＞ 有 `succeeded` 且有非成功 → `partial` ＞ 其余（含仅 `expired`、
///    `failed`+`cancelled` 组合）→ `failed`。
pub fn main_task_convergence(states: &[SubTaskState]) -> (MainTaskState, Option<MainTaskOutcome>) {
    if states.is_empty() {
        return (MainTaskState::Queued, None);
    }
    let any = |target: SubTaskState| states.contains(&target);
    let all = |target: SubTaskState| states.iter().all(|state| *state == target);
    let executing = states.iter().any(|state| {
        matches!(
            state,
            SubTaskState::Dispatching | SubTaskState::Accepted | SubTaskState::Verifying
        )
    });
    // 「批次已开始」= 至少一个子任务已离开 queued；与「仍有可运行 queued」同时成立即 running
    if executing || (any(SubTaskState::Queued) && !all(SubTaskState::Queued)) {
        return (MainTaskState::Running, None);
    }
    if all(SubTaskState::Queued) {
        return (MainTaskState::Queued, None);
    }
    if any(SubTaskState::Held) {
        return (MainTaskState::Held, None);
    }
    // 走到这里全部子任务都是批次终态（succeeded/failed/unknown/cancelled/expired）
    let outcome = if any(SubTaskState::Unknown) {
        MainTaskOutcome::Unknown
    } else if all(SubTaskState::Succeeded) {
        MainTaskOutcome::Success
    } else if all(SubTaskState::Cancelled) {
        MainTaskOutcome::Cancelled
    } else if any(SubTaskState::Succeeded) {
        MainTaskOutcome::Partial
    } else {
        MainTaskOutcome::Failed
    };
    (MainTaskState::Completed, Some(outcome))
}

/// 恒定时间字节比较：不短路，避免把摘要差异做成时序侧信道。
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

/// 目标授权三态（RT-N6：可见性与 reboot 权限解耦）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetAuthz {
    /// 可见且持有 `Permission::DeviceReboot`：抢设备锁、创建 `queued` 子任务。
    Permitted,
    /// 可见但缺 `DeviceReboot`：**不**伪装成不可见、不抢锁，直接落 `failed` 子任务。
    VisibleForbidden,
    /// 完全不可见（无任何有效权限）：整体回滚 404，不泄露部分结果。
    NotVisible,
}

/// 对 Identity 真源快照求该设备的当前有效权限集：先排除 missing/archived 设备，
/// 再交 `eval_memory_permissions`（其内部已排除 inactive/must-change actor，
/// admin shortcut 亦按事务内新读 actor 行判定）。
pub fn snapshot_effective_permissions(
    snapshot: &AuthorizationSnapshot,
    device_id: [u8; 32],
) -> HashSet<Permission> {
    let Some(device) = snapshot.devices.get(&device_id) else {
        return HashSet::new();
    };
    if device.archived {
        return HashSet::new();
    }
    let empty_groups: HashSet<Uuid> = HashSet::new();
    let groups = snapshot
        .device_groups
        .get(&device_id)
        .unwrap_or(&empty_groups);
    eval_memory_permissions(
        &snapshot.actor,
        device_id,
        &snapshot.grants,
        &snapshot.roles,
        groups,
    )
}

/// 纯判定 seam：含 reboot → `Permitted`；有其他有效权限 → `VisibleForbidden`；
/// 空集 → `NotVisible`。提交路径的唯一授权判定实现。
fn check_target_authz(snapshot: &AuthorizationSnapshot, device_id: [u8; 32]) -> TargetAuthz {
    let effective = snapshot_effective_permissions(snapshot, device_id);
    if effective.contains(&Permission::DeviceReboot) {
        return TargetAuthz::Permitted;
    }
    if !effective.is_empty() {
        return TargetAuthz::VisibleForbidden;
    }
    TargetAuthz::NotVisible
}

/// 纯策略单测的布尔薄包装（`Permitted` 投影）；提交路径不消费本包装。
#[cfg(test)]
fn snapshot_allows_task_creation(snapshot: &AuthorizationSnapshot, device_id: [u8; 32]) -> bool {
    matches!(
        check_target_authz(snapshot, device_id),
        TargetAuthz::Permitted
    )
}

/// HTTP 层由 `auth::service::Session` 产出的廉价前置快照；服务层在事务内重验
/// actor 行，**不信**本结构的 `is_admin` 布尔。
#[derive(Clone, Debug)]
pub struct AuthCtx {
    pub user_id: Uuid,
    pub is_admin: bool,
}

/// 提交请求：目标清单**只**来自服务端预览快照，客户端自报目标/摘要/counts 一律不接受，
/// 因此本结构不提供 `target` / `target_hash` / `counts_json` 字段。
#[derive(Clone, Debug)]
pub struct SubmitRequest {
    pub idempotency_key: String,
    pub request_hash: [u8; 32],
    /// 以下两字段均为服务端对 `preview_token` 的派生值，客户端永不携带。
    pub preview_token_hash: [u8; 32],
    pub preview_token_nonce: [u8; 32],
    pub observe_deadline: DateTime<Utc>,
    /// 创建时经服务端时钟取出的证据快照，与主任务同一事务持久化（RT-R8-2）。
    pub time_evidence: TimeEvidence,
}

/// 提交结果：`Created` 为首次创建，`Replayed` 为幂等重放（零副作用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitOutcome {
    Created { main_task_id: Uuid },
    Replayed { main_task_id: Uuid },
}

/// 提交错误：请求期短路、事务内重验与存储透传三类。
#[derive(Debug)]
pub enum SubmitError {
    InvalidArgument,
    ActorInactive,
    DeviceBusyCount { busy: usize, total: usize },
    PreviewExpired,
    PreviewConsumed,
    IdempotencyConflict,
    NotVisible,
    Backend(ControllerError),
}

impl From<ControllerError> for SubmitError {
    fn from(err: ControllerError) -> Self {
        SubmitError::Backend(err)
    }
}

impl From<sqlx::Error> for SubmitError {
    fn from(err: sqlx::Error) -> Self {
        SubmitError::Backend(ControllerError::Database(err))
    }
}

impl SubmitError {
    /// HTTP 边界的显式转换（`TaskApi` adapter 使用，不以 `?` 直接传播本类型）。
    pub fn into_controller_error(self) -> ControllerError {
        self.into()
    }
}

impl From<SubmitError> for ControllerError {
    fn from(err: SubmitError) -> Self {
        match err {
            SubmitError::InvalidArgument => ControllerError::InvalidArgument,
            SubmitError::ActorInactive => ControllerError::PermissionDenied,
            // 防御兜底：忙目标在提交路径内落为 failed 子任务，不整体报错，更不得映射 409。
            SubmitError::DeviceBusyCount { .. } => ControllerError::InvalidArgument,
            SubmitError::PreviewExpired | SubmitError::PreviewConsumed => {
                ControllerError::TaskPreviewInvalidState
            }
            SubmitError::IdempotencyConflict => ControllerError::TaskIdempotencyConflict,
            SubmitError::NotVisible => ControllerError::NotFound,
            SubmitError::Backend(err) => err,
        }
    }
}

/// 取消请求。
#[derive(Clone, Debug)]
pub struct CancelRequest {
    pub subtask_id: Uuid,
    pub expected_revision: u64,
}

/// 取消结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelOutcome {
    Cancelled,
    AlreadyTerminal { state: SubTaskState },
    NotCancellable { state: SubTaskState },
    RevisionConflict,
}

/// 取消错误（授权拒绝与存储透传）。
#[derive(Debug)]
pub enum CancelError {
    PermissionDenied,
    Backend(ControllerError),
}

impl From<ControllerError> for CancelError {
    fn from(err: ControllerError) -> Self {
        CancelError::Backend(err)
    }
}

impl From<sqlx::Error> for CancelError {
    fn from(err: sqlx::Error) -> Self {
        CancelError::Backend(ControllerError::Database(err))
    }
}

impl From<CancelError> for ControllerError {
    fn from(err: CancelError) -> Self {
        match err {
            CancelError::PermissionDenied => ControllerError::PermissionDenied,
            CancelError::Backend(err) => err,
        }
    }
}

/// 时限复核请求：`evidence_kind` 仅记审计；分派只依据服务端时钟快照的真实比对。
#[derive(Clone, Debug)]
pub struct TimeReviewRequest {
    pub main_task_id: Uuid,
    pub expected_revision: u64,
    pub decision: TimeReviewDecision,
    pub evidence_kind: TimeReviewEvidenceKind,
}

/// 时限复核结果。
#[derive(Debug)]
pub enum TimeReviewOutcome {
    Completed {
        affected: usize,
    },
    /// 原排队期限的有效性无法证明（当前或创建时时钟非 `NtpValid`、时钟纪元不连续、
    /// 创建证据缺失/不可解析，或恢复决策下已存在过期子任务）：
    /// 全部保持 `held`、零状态变更（事务回滚）。
    Inconclusive,
}

/// 时限复核错误。
#[derive(Debug)]
pub enum TimeReviewError {
    InvalidArgument,
    PermissionDenied,
    RevisionConflict,
    Backend(ControllerError),
}

impl From<ControllerError> for TimeReviewError {
    fn from(err: ControllerError) -> Self {
        TimeReviewError::Backend(err)
    }
}

impl From<sqlx::Error> for TimeReviewError {
    fn from(err: sqlx::Error) -> Self {
        TimeReviewError::Backend(ControllerError::Database(err))
    }
}

impl From<TimeReviewError> for ControllerError {
    fn from(err: TimeReviewError) -> Self {
        match err {
            TimeReviewError::InvalidArgument => ControllerError::InvalidArgument,
            TimeReviewError::PermissionDenied => ControllerError::PermissionDenied,
            TimeReviewError::RevisionConflict => ControllerError::RevisionConflict,
            TimeReviewError::Backend(err) => err,
        }
    }
}

/// 解除设备占用请求：`unknown` 子任务保持原状态，只删除本代际的设备锁。
#[derive(Clone, Debug)]
pub struct ReleaseRequest {
    pub subtask_id: Uuid,
    pub expected_revision: u64,
    pub acknowledge_unknown: bool,
    pub reason: String,
}

/// 解除占用结果。
#[derive(Debug)]
pub enum ReleaseLockOutcome {
    Released { revision: u64 },
}

/// 解除占用错误。
#[derive(Debug)]
pub enum ReleaseLockError {
    InvalidArgument,
    PermissionDenied,
    UnknownNotAcknowledged,
    SubTaskNotFound,
    RevisionConflict,
    Backend(ControllerError),
}

impl From<ControllerError> for ReleaseLockError {
    fn from(err: ControllerError) -> Self {
        ReleaseLockError::Backend(err)
    }
}

impl From<sqlx::Error> for ReleaseLockError {
    fn from(err: sqlx::Error) -> Self {
        ReleaseLockError::Backend(ControllerError::Database(err))
    }
}

impl From<ReleaseLockError> for ControllerError {
    fn from(err: ReleaseLockError) -> Self {
        match err {
            ReleaseLockError::InvalidArgument => ControllerError::InvalidArgument,
            ReleaseLockError::PermissionDenied => ControllerError::PermissionDenied,
            ReleaseLockError::UnknownNotAcknowledged => ControllerError::InvalidArgument,
            ReleaseLockError::SubTaskNotFound => ControllerError::NotFound,
            ReleaseLockError::RevisionConflict => ControllerError::RevisionConflict,
            ReleaseLockError::Backend(err) => err,
        }
    }
}

/// 事务内重验后的 actor 行（`users` 环节的唯一取锁点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActorRow {
    active: bool,
    is_admin: bool,
    must_change_password: bool,
}

impl ActorRow {
    /// `integrity::require_active_admin` 同语义：`active && is_admin &&
    /// !must_change_password` 三条件缺一即拒绝管理写操作（并发降权/停用/未改
    /// 初始口令都必须在事务内失败关闭，绝不信 HTTP 层快照的 `is_admin` 布尔）。
    fn active_admin(&self) -> bool {
        self.active && self.is_admin && !self.must_change_password
    }

    /// 提交/取消等业务写操作的最低要求：账号有效且未待改初始口令（不要求 admin）。
    fn writable(&self) -> bool {
        self.active && !self.must_change_password
    }
}

/// 预览快照在锁内读出并校验后的固定值。
#[derive(Clone, Debug)]
struct PreviewSnapshotFixed {
    target_hash: [u8; 32],
    targets_json: String,
    preview_generation: [u8; 16],
}

/// 待写入的子任务行（避免 7 参数以上的 helper 签名）。
///
/// 两条期限列语义独立、绝不互替：`queue_deadline` 是排队截止（服务端可信时基 +
/// 排队 TTL，时限复核的唯一比对依据）；`observe_deadline` 是观察窗截止（04 §5.2
/// 派发期语义，复核路径只读不写）。
#[derive(Clone, Copy, Debug)]
struct NewSubTask {
    subtask_id: Uuid,
    main_task_id: Uuid,
    device_id: [u8; 32],
    state: SubTaskState,
    reason_code: Option<ReasonCode>,
    queue_deadline: DateTime<Utc>,
    observe_deadline: DateTime<Utc>,
}

/// 11 字段脱敏审计的入参集合。
#[derive(Clone, Copy, Debug)]
struct TaskAudit<'a> {
    actor_user_id: Option<Uuid>,
    event_type: &'a str,
    target_kind: &'a str,
    target_id: &'a str,
    params_redacted: &'a str,
    outcome: &'a str,
    time_evidence: &'a str,
}

/// 任务编排服务：仓储是唯一事务入口，本类型不直接持有连接池。
#[derive(Debug)]
pub struct TaskService<R: TaskRepository> {
    pub repo: R,
}

impl<R: TaskRepository> TaskService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    /// 签发预览：规范化目标 → 生成 CSPRNG 令牌 → 服务端派生 hash/nonce → 落盘。
    ///
    /// 资格判定（设备在线、能力可用等）由调用方在传入的 `targets` 中给出；
    /// 本方法不伪造资格，也不改写 `eligible` / `reason_code`。
    pub async fn preview(
        &self,
        actor_user_id: Uuid,
        targets: Vec<PreviewTarget>,
    ) -> Result<PreviewOutcome, ControllerError> {
        let mut ordered = targets;
        ordered.sort_by_key(|target| target.device_id);
        ordered.dedup_by_key(|target| target.device_id);
        validate_target_count(ordered.len())?;

        let preview_token = new_preview_token();
        let raw_token = preview_token_bytes(&preview_token).ok_or(ControllerError::Crypto)?;
        let token_hash = preview_token_digest(&raw_token);
        let token_nonce = preview_token_nonce(&raw_token);
        let seconds = i64::try_from(PREVIEW_TTL.as_secs()).map_err(|_| preview_ttl_error())?;
        let ttl = chrono::Duration::try_seconds(seconds).ok_or_else(preview_ttl_error)?;
        let expires_at = Utc::now() + ttl;
        self.repo
            .create_preview(
                token_nonce,
                actor_user_id,
                &ordered,
                &token_hash,
                expires_at,
            )
            .await?;
        Ok(PreviewOutcome {
            preview_token,
            targets: ordered,
        })
    }

    /// 原子提交：幂等重放优先，其次消费预览并按锁序创建任务。
    pub async fn submit(
        &self,
        req: &SubmitRequest,
        ctx: &AuthCtx,
    ) -> Result<SubmitOutcome, SubmitError> {
        if !is_canonical_uuid_v4_key(&req.idempotency_key) {
            return Err(SubmitError::InvalidArgument);
        }
        let mut tx = self.repo.begin().await?;
        match self.submit_in_tx(&mut tx, req, ctx).await {
            Ok(outcome) => {
                // ⑪ 仅提交成功后才返回 Created/Replayed
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                // 任何失败路径都显式回滚、零残留；回滚失败不覆盖原始错误
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }

    async fn submit_in_tx(
        &self,
        tx: &mut Transaction<'_, MySql>,
        req: &SubmitRequest,
        ctx: &AuthCtx,
    ) -> Result<SubmitOutcome, SubmitError> {
        // ② 写事务 guard：期望版本来自编译期配置，绝不从数据库行推断
        crate::integrity::lock_integrity_guard(tx).await?;

        // ③ actor 事务内重验（`users` 环节的唯一取锁点）：账号失效或待改初始口令即拒绝
        let actor = read_actor_in_tx(tx, ctx.user_id).await?;
        if !actor.writable() {
            return Err(SubmitError::ActorInactive);
        }

        // ④ 先查持久幂等记录：存在即重放或冲突，绝不先消费预览再判幂等
        let idempotency_row = tx_row(
            tx,
            "SELECT id, request_hash FROM main_tasks WHERE actor_user_id = ? AND idempotency_key = ?",
            params![ctx.user_id.as_bytes().as_slice(), &req.idempotency_key],
        )
        .await?;
        if let Some(row) = idempotency_row {
            let main_task_id = mysql_uuid(&column::<Vec<u8>>(&row, "id")?)?;
            let stored = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "request_hash")?)?;
            if !constant_time_eq(&stored, &req.request_hash) {
                return Err(SubmitError::IdempotencyConflict);
            }
            // 同一请求此前已成功：即使预览此刻已过期或被消费，仍直接重放，零副作用
            return Ok(SubmitOutcome::Replayed { main_task_id });
        }

        // ⑤ 首次提交才判定有效性、原子消费预览并读出固定目标清单（唯一真相源）
        let snapshot = lock_preview_in_tx(tx, ctx.user_id, req).await?;
        let targets = decode_preview_targets(&snapshot.targets_json)
            .ok_or_else(|| SubmitError::Backend(task_row_decode_error()))?;
        validate_target_count(targets.len())
            .map_err(|_| SubmitError::Backend(task_row_decode_error()))?;
        if !constant_time_eq(&device_target_hash(&targets, &[]), &snapshot.target_hash) {
            // 快照摘要与清单不一致：DB 内容被污染，fail-closed
            return Err(SubmitError::Backend(task_row_decode_error()));
        }

        // ⑥ 锁序：roles → device_groups → devices（devices 段按 public_key 字节序）
        lock_authz_rows_in_order(tx, ctx.user_id, &targets).await?;

        // ⑦ 主任务先落库：counts 先写十键全零占位，第 ⑩ 步按真实结果回写
        let main_task_id = Uuid::new_v4();
        let created_time_json =
            serde_json::to_string(&req.time_evidence).map_err(|_| time_evidence_error())?;
        tx_execute(
            tx,
            "INSERT INTO main_tasks (id, actor_user_id, idempotency_key, request_hash, \
             target_hash, preview_token_hash, state, outcome, revision, counts_json, \
             created_time_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL, 1, ?, ?, NOW(6), NOW(6))",
            params![
                main_task_id.as_bytes().as_slice(),
                ctx.user_id.as_bytes().as_slice(),
                &req.idempotency_key,
                &req.request_hash[..],
                &snapshot.target_hash[..],
                &req.preview_token_hash[..],
                &enum_wire(&MainTaskState::Queued)?,
                &empty_counts_json(),
                &created_time_json,
            ],
        )
        .await?;

        // ⑧ 逐目标重验当前授权并按锁序申请设备锁（同一快照、同一事务、零网络）
        let authz = load_snapshot_in_tx(tx, ctx.user_id.into_bytes(), &targets).await?;
        // 排队截止由服务端创建时证据在同一可信时基上算出（与 time_review 的比对同源）；
        // 观察窗截止属派发期语义，随请求携带的服务端值原样落 observe_deadline，两列不互替。
        let queue_deadline =
            queue_deadline_of(&req.time_evidence).ok_or_else(time_evidence_error)?;
        let mut created = Vec::with_capacity(targets.len());
        for device_id in &targets {
            let (state, reason_code) = match check_target_authz(&authz, *device_id) {
                TargetAuthz::NotVisible => return Err(SubmitError::NotVisible),
                // 可见但无权：保留固定目标清单、不申请设备锁，直接落 failed
                TargetAuthz::VisibleForbidden => {
                    (SubTaskState::Failed, Some(ReasonCode::PermissionDenied))
                }
                TargetAuthz::Permitted => (SubTaskState::Queued, None),
            };
            let subtask_id = Uuid::new_v4();
            insert_sub_task(
                tx,
                &NewSubTask {
                    subtask_id,
                    main_task_id,
                    device_id: *device_id,
                    state,
                    reason_code,
                    queue_deadline,
                    observe_deadline: req.observe_deadline,
                },
            )
            .await?;
            // 三种落库结局（queued / failed:PERMISSION_DENIED / failed:DEVICE_BUSY）
            // **一律**计入 created：counts_json 与主任务收敛必须与子任务行一一对应，
            // 漏计任一失败目标都会让主任务永远停在非终态。
            let final_state = match state {
                SubTaskState::Queued => {
                    if acquire_device_lock(tx, *device_id, main_task_id, subtask_id).await? {
                        SubTaskState::Queued
                    } else {
                        // 设备唯一操作锁 CAS 未插入即设备忙：不排队、不等待，就地落 failed
                        mark_sub_task_failed(tx, subtask_id, ReasonCode::DeviceBusy).await?;
                        SubTaskState::Failed
                    }
                }
                other => other,
            };
            created.push(final_state);
        }

        // ⑩ counts 真实回写 + 主任务收敛（与实际子状态分布同源，不写假持久化）
        let (state, outcome) = main_task_convergence(&created);
        let counts_json = counts_json_of(&tally(&created));
        tx_execute(
            tx,
            "UPDATE main_tasks SET state = ?, outcome = ?, revision = revision + 1, \
             counts_json = ?, updated_at = NOW(6) WHERE id = ?",
            params![
                &enum_wire(&state)?,
                &match outcome {
                    None => None,
                    Some(outcome) => Some(enum_wire(&outcome)?),
                },
                &counts_json,
                main_task_id.as_bytes().as_slice(),
            ],
        )
        .await?;

        // ⑨ 同事务写 11 字段脱敏审计（不含设备 id、令牌与摘要）
        let audit_params = serde_json::json!({
            "targets": targets.len(),
            "queued": created.iter().filter(|s| **s == SubTaskState::Queued).count(),
            "failed": created.iter().filter(|s| **s == SubTaskState::Failed).count(),
        })
        .to_string();
        insert_task_audit(
            tx,
            &TaskAudit {
                actor_user_id: Some(ctx.user_id),
                event_type: "task.submit",
                target_kind: "main_task",
                target_id: &main_task_id.simple().to_string(),
                params_redacted: &audit_params,
                outcome: "success",
                time_evidence: &created_time_json,
            },
        )
        .await?;

        Ok(SubmitOutcome::Created { main_task_id })
    }

    /// 取消子任务：仅 `queued`/`held`，且要求管理员或该设备的 reboot 权限。
    pub async fn cancel(
        &self,
        req: &CancelRequest,
        ctx: &AuthCtx,
    ) -> Result<CancelOutcome, CancelError> {
        let mut tx = self.repo.begin().await?;
        match cancel_in_tx(&mut tx, req, ctx).await {
            Ok(outcome) => {
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }

    /// 时限复核：只以服务端时钟快照真实比对，绝不延长任何期限。
    ///
    /// `clock` 必须由服务端时钟源（TimeGate）产出，绝不来自请求体。
    /// `Inconclusive`（原期限有效性无法证明）走**显式回滚**：释放 `FOR UPDATE`
    /// 行锁、零状态变更，`held` 子任务与主任务 revision 一律保持原样。
    pub async fn time_review(
        &self,
        req: &TimeReviewRequest,
        clock: &TimeEvidence,
        ctx: &AuthCtx,
    ) -> Result<TimeReviewOutcome, TimeReviewError> {
        let mut tx = self.repo.begin().await?;
        match time_review_in_tx(&mut tx, req, clock, ctx).await {
            Ok(TimeReviewOutcome::Inconclusive) => {
                let _ = tx.rollback().await;
                Ok(TimeReviewOutcome::Inconclusive)
            }
            Ok(outcome) => {
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }

    /// 解除设备占用：只删除本代际锁，子任务保持 `unknown`，绝不触碰新 generation。
    pub async fn release_lock(
        &self,
        req: &ReleaseRequest,
        ctx: &AuthCtx,
    ) -> Result<ReleaseLockOutcome, ReleaseLockError> {
        if !req.acknowledge_unknown {
            return Err(ReleaseLockError::UnknownNotAcknowledged);
        }
        if req.reason.trim().is_empty() {
            return Err(ReleaseLockError::InvalidArgument);
        }
        let mut tx = self.repo.begin().await?;
        match release_lock_in_tx(&mut tx, req, ctx).await {
            Ok(outcome) => {
                tx.commit().await?;
                Ok(outcome)
            }
            Err(err) => {
                let _ = tx.rollback().await;
                Err(err)
            }
        }
    }
}

fn preview_ttl_error() -> ControllerError {
    ControllerError::Config("preview ttl out of range".into())
}

fn time_evidence_error() -> ControllerError {
    ControllerError::Config("time evidence is not serializable".into())
}

/// 把状态序列折叠成 `(状态, 频数)` 列表，供 `counts_json_of` 消费。
fn tally(states: &[SubTaskState]) -> Vec<(SubTaskState, u64)> {
    let mut totals: HashMap<SubTaskState, u64> = HashMap::new();
    for state in states {
        *totals.entry(*state).or_insert(0) += 1;
    }
    totals.into_iter().collect()
}

/// actor 行事务内重验：`CAST(... AS SIGNED)` 严格解码，污染值即 DB 损坏（503）。
/// 三列一次读全（`active`/`is_admin`/`must_change_password`），写操作路径不得只读其中
/// 一部分而漏掉「未改初始口令」这一 fail-closed 条件。
async fn read_actor_in_tx(
    tx: &mut Transaction<'_, MySql>,
    user_id: Uuid,
) -> Result<ActorRow, ControllerError> {
    let row = tx_row(
        tx,
        "SELECT CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, \
         CAST(must_change_password AS SIGNED) AS must_change_password \
         FROM users WHERE id = ? FOR UPDATE",
        params![user_id.as_bytes().as_slice()],
    )
    .await?
    .ok_or(ControllerError::PermissionDenied)?;
    Ok(ActorRow {
        active: strict_bool(column::<i64>(&row, "active")?)?,
        is_admin: strict_bool(column::<i64>(&row, "is_admin")?)?,
        must_change_password: strict_bool(column::<i64>(&row, "must_change_password")?)?,
    })
}

/// 锁定并校验预览快照，随后原子单次消费（`DELETE`；禁止 `UPDATE consumed_at` 两段式）。
async fn lock_preview_in_tx(
    tx: &mut Transaction<'_, MySql>,
    user_id: Uuid,
    req: &SubmitRequest,
) -> Result<PreviewSnapshotFixed, SubmitError> {
    let row = tx_row(
        tx,
        "SELECT target_hash, CAST(targets_json AS CHAR) AS targets_json, preview_generation, \
         preview_token_hash, consumed_at, expires_at, NOW(6) AS db_now \
         FROM task_previews WHERE token_nonce = ? AND actor_user_id = ? FOR UPDATE",
        params![&req.preview_token_nonce[..], user_id.as_bytes().as_slice()],
    )
    .await?;
    let Some(row) = row else {
        return Err(classify_missing_preview(tx, &req.preview_token_nonce).await?);
    };
    // 恒定时间核验：持久摘要必须等于请求 token 的同一派生摘要
    let stored_hash = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "preview_token_hash")?)?;
    if !constant_time_eq(&stored_hash, &req.preview_token_hash) {
        // 与「行不存在/已消费」同投影，不泄露差异
        return Err(SubmitError::PreviewConsumed);
    }
    if column::<Option<DateTime<Utc>>>(&row, "consumed_at")?.is_some() {
        return Err(SubmitError::PreviewConsumed);
    }
    let snapshot = PreviewSnapshotFixed {
        target_hash: fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "target_hash")?)?,
        targets_json: column::<String>(&row, "targets_json")?,
        preview_generation: fixed_mysql_bytes::<16>(&column::<Vec<u8>>(
            &row,
            "preview_generation",
        )?)?,
    };
    // 有效性三重判定，缺一即 PreviewExpired：
    // ① 进程启动代际（跨代际 token 无法重新派生匹配）；
    // ② 进程内单调 TTL（墙钟回拨不得延长）；
    // ③ DB 时钟上限仅作跨进程兜底，不单独作为有效性依据。
    if snapshot.preview_generation != preview_generation_bytes() {
        return Err(SubmitError::PreviewExpired);
    }
    if !preview_mono_valid(&req.preview_token_nonce) {
        return Err(SubmitError::PreviewExpired);
    }
    if column::<DateTime<Utc>>(&row, "expires_at")? <= column::<DateTime<Utc>>(&row, "db_now")? {
        return Err(SubmitError::PreviewExpired);
    }

    let deleted = tx_execute(
        tx,
        "DELETE FROM task_previews WHERE token_nonce = ? AND actor_user_id = ? \
         AND consumed_at IS NULL AND expires_at > NOW(6) AND preview_generation = ?",
        params![
            &req.preview_token_nonce[..],
            user_id.as_bytes().as_slice(),
            &snapshot.preview_generation[..],
        ],
    )
    .await?;
    if deleted != 1 {
        return Err(classify_missing_preview(tx, &req.preview_token_nonce).await?);
    }
    Ok(snapshot)
}

/// 预览缺失/消费失败的原因判别（必须在回滚**之前**执行）。
async fn classify_missing_preview(
    tx: &mut Transaction<'_, MySql>,
    token_nonce: &[u8; 32],
) -> Result<SubmitError, ControllerError> {
    let row = tx_row(
        tx,
        "SELECT consumed_at FROM task_previews WHERE token_nonce = ?",
        params![&token_nonce[..]],
    )
    .await?;
    Ok(match row {
        Some(row) if column::<Option<DateTime<Utc>>>(&row, "consumed_at")?.is_some() => {
            SubmitError::PreviewConsumed
        }
        _ => SubmitError::PreviewExpired,
    })
}

/// 第 ⑥ 步锁序的唯一实现：三段严格 `roles → device_groups → devices`，每段按主键
/// 字节序升序 `FOR UPDATE`（`BTreeSet<Vec<u8>>` 自然序 = 升序 + 去重），空集合安全跳过。
/// `grants` 引用集以普通 SELECT 读出，本身不是锁目标；`device_operation_locks` 的 CAS
/// 只在本函数返回后进入。
async fn lock_authz_rows_in_order(
    tx: &mut Transaction<'_, MySql>,
    user_id: Uuid,
    targets: &[[u8; 32]],
) -> Result<(), SubmitError> {
    let rows = tx_rows(
        tx,
        "SELECT role_id, scope_group_id FROM grants WHERE user_id = ?",
        params![user_id.as_bytes().as_slice()],
    )
    .await?;
    let mut role_ids: BTreeSet<Vec<u8>> = BTreeSet::new();
    let mut group_ids: BTreeSet<Vec<u8>> = BTreeSet::new();
    for row in &rows {
        if let Some(role_id) = column::<Option<Vec<u8>>>(row, "role_id")? {
            role_ids.insert(role_id);
        }
        if let Some(group_id) = column::<Option<Vec<u8>>>(row, "scope_group_id")? {
            group_ids.insert(group_id);
        }
    }
    lock_table_in_byte_order(tx, "SELECT id FROM roles WHERE id IN", &role_ids).await?;
    lock_table_in_byte_order(tx, "SELECT id FROM device_groups WHERE id IN", &group_ids).await?;
    let device_ids: BTreeSet<Vec<u8>> = targets.iter().map(|id| id.to_vec()).collect();
    lock_table_in_byte_order(
        tx,
        "SELECT public_key FROM devices WHERE public_key IN",
        &device_ids,
    )
    .await
}

/// 一段共用的按字节序 `FOR UPDATE` 锁表；空集合安全跳过。
async fn lock_table_in_byte_order(
    tx: &mut Transaction<'_, MySql>,
    prefix: &str,
    ids: &BTreeSet<Vec<u8>>,
) -> Result<(), SubmitError> {
    if ids.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; ids.len()].join(", ");
    let sql = format!("{prefix} ({placeholders}) FOR UPDATE");
    let mut args = MySqlArguments::default();
    for id in ids {
        sqlx::Arguments::add(&mut args, &id[..]).map_err(|err| {
            SubmitError::Backend(ControllerError::Database(sqlx::Error::Encode(err)))
        })?;
    }
    tx_execute(tx, &sql, args).await?;
    Ok(())
}

/// 落一条子任务。`sub_tasks` 无 `outcome` 列（0004 DDL 真相源），
/// 子任务结局由 `state` + `reason_code` 表达。
///
/// `queue_deadline` 与 `observe_deadline` **分别**绑定各自的值：把排队截止写进观察
/// 窗列（或反之）会让复核拿观察窗判排队寿命，属跨生命周期混用。
async fn insert_sub_task(
    tx: &mut Transaction<'_, MySql>,
    row: &NewSubTask,
) -> Result<(), SubmitError> {
    tx_execute(
        tx,
        "INSERT INTO sub_tasks (id, main_task_id, device_id, state, reason_code, revision, \
         queue_deadline, observe_deadline, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, 1, ?, ?, NOW(6), NOW(6))",
        params![
            row.subtask_id.as_bytes().as_slice(),
            row.main_task_id.as_bytes().as_slice(),
            &row.device_id[..],
            &enum_wire(&row.state)?,
            &match row.reason_code {
                None => None,
                Some(code) => Some(enum_wire(&code)?),
            },
            row.queue_deadline,
            row.observe_deadline,
        ],
    )
    .await?;
    Ok(())
}

/// 设备唯一操作锁 CAS：条件插入成功（恰 1 行）即抢到，并把本代际 `generation = 1`
/// 回写子任务；未插入即设备忙，由调用方就地落 `failed`，不排队、不等待。
async fn acquire_device_lock(
    tx: &mut Transaction<'_, MySql>,
    device_id: [u8; 32],
    main_task_id: Uuid,
    subtask_id: Uuid,
) -> Result<bool, SubmitError> {
    let acquired = tx_execute(
        tx,
        "INSERT INTO device_operation_locks \
         (device_id, main_task_id, owner_sub_task_id, generation, acquired_at) \
         SELECT ?, ?, ?, 1, NOW(6) FROM DUAL \
         WHERE NOT EXISTS (SELECT 1 FROM device_operation_locks WHERE device_id = ?)",
        params![
            &device_id[..],
            main_task_id.as_bytes().as_slice(),
            subtask_id.as_bytes().as_slice(),
            &device_id[..],
        ],
    )
    .await?;
    if acquired != 1 {
        return Ok(false);
    }
    tx_execute(
        tx,
        "UPDATE sub_tasks SET lock_generation = 1 WHERE id = ?",
        params![subtask_id.as_bytes().as_slice()],
    )
    .await?;
    Ok(true)
}

/// 把子任务就地改写为 `failed` + 指定原因码（提交期设备忙的唯一实现）。
async fn mark_sub_task_failed(
    tx: &mut Transaction<'_, MySql>,
    subtask_id: Uuid,
    reason_code: ReasonCode,
) -> Result<(), SubmitError> {
    tx_execute(
        tx,
        "UPDATE sub_tasks SET state = ?, reason_code = ? WHERE id = ?",
        params![
            &enum_wire(&SubTaskState::Failed)?,
            &enum_wire(&reason_code)?,
            subtask_id.as_bytes().as_slice(),
        ],
    )
    .await?;
    Ok(())
}

async fn cancel_in_tx(
    tx: &mut Transaction<'_, MySql>,
    req: &CancelRequest,
    ctx: &AuthCtx,
) -> Result<CancelOutcome, CancelError> {
    crate::integrity::lock_integrity_guard(tx).await?;
    // 事务内完整重验 actor（active && !must_change_password）：HTTP 层快照不可信，
    // 并发停用/待改初始口令必须失败关闭；is_admin 只作 admin 直通判定输入。
    let caller = read_actor_in_tx(tx, ctx.user_id).await?;
    if !caller.writable() {
        return Err(CancelError::PermissionDenied);
    }
    let caller_admin = caller.is_admin;

    let row = tx_row(
        tx,
        "SELECT st.revision, st.state, st.device_id, st.main_task_id, mt.actor_user_id, \
         mt.revision AS main_revision FROM sub_tasks st \
         JOIN main_tasks mt ON mt.id = st.main_task_id WHERE st.id = ? FOR UPDATE",
        params![req.subtask_id.as_bytes().as_slice()],
    )
    .await?;
    let Some(row) = row else {
        // 与「不存在」同投影，不泄露他人任务是否存在
        return Err(CancelError::Backend(ControllerError::NotFound));
    };
    let owner = mysql_uuid(&column::<Vec<u8>>(&row, "actor_user_id")?)?;
    if !caller_admin && owner != ctx.user_id {
        return Err(CancelError::Backend(ControllerError::NotFound));
    }
    if column::<u64>(&row, "revision")? != req.expected_revision {
        return Ok(CancelOutcome::RevisionConflict);
    }
    let state = sub_task_state_from_str(&column::<String>(&row, "state")?)?;
    if is_terminal_state(state) {
        return Ok(CancelOutcome::AlreadyTerminal { state });
    }
    let device_id = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "device_id")?)?;
    let authz = load_snapshot_in_tx(
        tx,
        ctx.user_id.into_bytes(),
        std::slice::from_ref(&device_id),
    )
    .await?;
    let has_reboot = matches!(
        check_target_authz(&authz, device_id),
        TargetAuthz::Permitted
    );
    if !cancel_precondition(state, caller_admin, has_reboot) {
        return Ok(CancelOutcome::NotCancellable { state });
    }
    let updated = tx_execute(
        tx,
        "UPDATE sub_tasks SET state = ?, reason_code = ?, revision = revision + 1, \
         updated_at = NOW(6) WHERE id = ? AND revision = ?",
        params![
            &enum_wire(&SubTaskState::Cancelled)?,
            &enum_wire(&ReasonCode::UserCancelled)?,
            req.subtask_id.as_bytes().as_slice(),
            req.expected_revision,
        ],
    )
    .await?;
    if updated != 1 {
        return Ok(CancelOutcome::RevisionConflict);
    }
    tx_execute(
        tx,
        "DELETE FROM device_operation_locks WHERE device_id = ? AND owner_sub_task_id = ? \
         AND generation = 1",
        params![&device_id[..], req.subtask_id.as_bytes().as_slice()],
    )
    .await?;
    // 取消改写子任务状态后，主任务持久汇总必须**同事务**重算回写（state/outcome/
    // counts_json 与 revision + 1 同条 UPDATE）；只递增子任务 revision 会让主行永远
    // 停留在 running，且 counts 漏掉 cancelled。主任务行已被上面的 FOR UPDATE 锁住。
    let main_task_id = mysql_uuid(&column::<Vec<u8>>(&row, "main_task_id")?)?;
    let main_revision = column::<u64>(&row, "main_revision")?;
    sync_main_task_summary(tx, main_task_id, main_revision).await?;
    insert_task_audit(
        tx,
        &TaskAudit {
            actor_user_id: Some(ctx.user_id),
            event_type: "task.cancel",
            target_kind: "sub_task",
            target_id: &req.subtask_id.simple().to_string(),
            params_redacted: "{}",
            outcome: "success",
            time_evidence: &crate::db::system_fallback_time_evidence(),
        },
    )
    .await?;
    Ok(CancelOutcome::Cancelled)
}

async fn time_review_in_tx(
    tx: &mut Transaction<'_, MySql>,
    req: &TimeReviewRequest,
    clock: &TimeEvidence,
    ctx: &AuthCtx,
) -> Result<TimeReviewOutcome, TimeReviewError> {
    crate::integrity::lock_integrity_guard(tx).await?;
    // 管理写操作在事务内完整重验 actor：active && is_admin && !must_change_password
    let caller = read_actor_in_tx(tx, ctx.user_id).await?;
    if !caller.active_admin() {
        return Err(TimeReviewError::PermissionDenied);
    }
    let task = tx_row(
        tx,
        "SELECT revision, CAST(created_time_json AS CHAR) AS created_time_json \
         FROM main_tasks WHERE id = ? FOR UPDATE",
        params![req.main_task_id.as_bytes().as_slice()],
    )
    .await?
    .ok_or(TimeReviewError::InvalidArgument)?;
    if column::<u64>(&task, "revision")? != req.expected_revision {
        return Err(TimeReviewError::RevisionConflict);
    }
    // 锁定该主任务的全部 held 子任务（按 id 字节序，单一取锁段）。
    // 比对依据是 **queue_deadline**（排队截止）：held 子任务尚未派发，其寿命由排队
    // TTL 决定，绝不得混用派发后才生效的 observe_deadline 观察窗。
    let rows = tx_rows(
        tx,
        "SELECT id, device_id, lock_generation, queue_deadline FROM sub_tasks \
         WHERE main_task_id = ? AND state = ? ORDER BY id FOR UPDATE",
        params![
            req.main_task_id.as_bytes().as_slice(),
            &enum_wire(&SubTaskState::Held)?,
        ],
    )
    .await?;
    if rows.is_empty() {
        return Err(TimeReviewError::InvalidArgument);
    }
    let mut held = Vec::with_capacity(rows.len());
    let mut queue_deadlines = Vec::with_capacity(rows.len());
    for row in &rows {
        queue_deadlines.push(column::<Option<DateTime<Utc>>>(row, "queue_deadline")?);
        held.push((
            mysql_uuid(&column::<Vec<u8>>(row, "id")?)?,
            fixed_mysql_bytes::<32>(&column::<Vec<u8>>(row, "device_id")?)?,
            column::<u64>(row, "lock_generation")?,
        ));
    }
    // 时间安全核验（纯策略，零 IO）：创建时证据缺失/不可解析、创建时或当前时钟
    // 不可信、时钟纪元不连续、恢复决策下存在过期子任务，一律无法证明原期限有效。
    let created = parse_created_time_evidence(&column::<String>(&task, "created_time_json")?);
    let verdict = time_review_verdict(created.as_ref(), clock, req.decision, &queue_deadlines);
    match verdict {
        TimeReviewVerdict::Unverifiable => return Ok(TimeReviewOutcome::Inconclusive),
        TimeReviewVerdict::Contradiction => return Err(TimeReviewError::InvalidArgument),
        TimeReviewVerdict::Resume | TimeReviewVerdict::Expire => {}
    }
    let resume = verdict == TimeReviewVerdict::Resume;
    let target_state = if resume {
        SubTaskState::Queued
    } else {
        SubTaskState::Expired
    };
    let target_reason = if resume {
        None
    } else {
        Some(enum_wire(&ReasonCode::QueueExpired)?)
    };
    for (subtask_id, device_id, lock_generation) in &held {
        let updated = tx_execute(
            tx,
            "UPDATE sub_tasks SET state = ?, reason_code = ?, revision = revision + 1, \
             updated_at = NOW(6) WHERE id = ? AND state = ?",
            params![
                &enum_wire(&target_state)?,
                &target_reason,
                subtask_id.as_bytes().as_slice(),
                &enum_wire(&SubTaskState::Held)?,
            ],
        )
        .await?;
        if updated != 1 {
            return Err(TimeReviewError::RevisionConflict);
        }
        if !resume {
            // 过期即释放本代际设备锁；绝不触碰该设备后续任务的新 generation
            tx_execute(
                tx,
                "DELETE FROM device_operation_locks WHERE device_id = ? \
                 AND owner_sub_task_id = ? AND generation = ?",
                params![
                    &device_id[..],
                    subtask_id.as_bytes().as_slice(),
                    *lock_generation,
                ],
            )
            .await?;
        }
    }
    // 主任务持久汇总与子任务变更同事务原子回写（RT-R8-3）：只递增 revision 会让主行
    // state/outcome/counts_json 严重陈旧。
    sync_main_task_summary(tx, req.main_task_id, req.expected_revision).await?;
    let audit_params = serde_json::json!({
        "decision": if resume { "resume_original_deadline" } else { "expire" },
        "evidence_kind": format!("{:?}", req.evidence_kind),
        "affected": held.len(),
    })
    .to_string();
    insert_task_audit(
        tx,
        &TaskAudit {
            actor_user_id: Some(ctx.user_id),
            event_type: "task.time_review",
            target_kind: "main_task",
            target_id: &req.main_task_id.simple().to_string(),
            params_redacted: &audit_params,
            outcome: "success",
            time_evidence: &serde_json::to_string(clock).map_err(|_| time_evidence_error())?,
        },
    )
    .await?;
    Ok(TimeReviewOutcome::Completed {
        affected: held.len(),
    })
}

async fn release_lock_in_tx(
    tx: &mut Transaction<'_, MySql>,
    req: &ReleaseRequest,
    ctx: &AuthCtx,
) -> Result<ReleaseLockOutcome, ReleaseLockError> {
    crate::integrity::lock_integrity_guard(tx).await?;
    // 管理写操作在事务内完整重验 actor：active && is_admin && !must_change_password
    let caller = read_actor_in_tx(tx, ctx.user_id).await?;
    if !caller.active_admin() {
        return Err(ReleaseLockError::PermissionDenied);
    }
    let row = tx_row(
        tx,
        "SELECT revision, state, lock_generation, device_id FROM sub_tasks WHERE id = ? FOR UPDATE",
        params![req.subtask_id.as_bytes().as_slice()],
    )
    .await?
    .ok_or(ReleaseLockError::SubTaskNotFound)?;
    if column::<u64>(&row, "revision")? != req.expected_revision {
        return Err(ReleaseLockError::RevisionConflict);
    }
    // CAS 之前先真实解析状态：非法值即 DB 损坏（503），绝不当作 unknown 放行
    if sub_task_state_from_str(&column::<String>(&row, "state")?)? != SubTaskState::Unknown {
        return Err(ReleaseLockError::InvalidArgument);
    }
    let device_id = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(&row, "device_id")?)?;
    let lock_generation = column::<u64>(&row, "lock_generation")?;
    let deleted = tx_execute(
        tx,
        "DELETE FROM device_operation_locks WHERE device_id = ? AND owner_sub_task_id = ? \
         AND generation = ?",
        params![
            &device_id[..],
            req.subtask_id.as_bytes().as_slice(),
            lock_generation,
        ],
    )
    .await?;
    if deleted != 1 {
        return Err(ReleaseLockError::RevisionConflict);
    }
    let updated = tx_execute(
        tx,
        "UPDATE sub_tasks SET revision = revision + 1, updated_at = NOW(6) \
         WHERE id = ? AND revision = ?",
        params![req.subtask_id.as_bytes().as_slice(), req.expected_revision],
    )
    .await?;
    if updated != 1 {
        return Err(ReleaseLockError::RevisionConflict);
    }
    let audit_params = serde_json::json!({
        "reason": redact_reason(&req.reason),
        "acknowledge_unknown": true,
    })
    .to_string();
    insert_task_audit(
        tx,
        &TaskAudit {
            actor_user_id: Some(ctx.user_id),
            event_type: "task.release_lock",
            target_kind: "sub_task",
            target_id: &req.subtask_id.simple().to_string(),
            params_redacted: &audit_params,
            outcome: "success",
            time_evidence: &crate::db::system_fallback_time_evidence(),
        },
    )
    .await?;
    Ok(ReleaseLockOutcome::Released {
        revision: req.expected_revision + 1,
    })
}

/// 读回主任务全部子状态（主任务收敛与 counts 的唯一真相源）。
async fn read_sub_task_states(
    tx: &mut Transaction<'_, MySql>,
    main_task_id: Uuid,
) -> Result<Vec<SubTaskState>, ControllerError> {
    let rows = tx_rows(
        tx,
        "SELECT state FROM sub_tasks WHERE main_task_id = ? ORDER BY id",
        params![main_task_id.as_bytes().as_slice()],
    )
    .await?;
    let mut states = Vec::with_capacity(rows.len());
    for row in &rows {
        states.push(sub_task_state_from_str(&column::<String>(row, "state")?)?);
    }
    Ok(states)
}

/// 同事务回写主任务持久汇总：重查全部子任务最新状态 → 重算 `state`/`outcome` 与
/// 十态 `counts_json` → 连同 `revision + 1` 一条 UPDATE 落库（子任务变更与主行
/// 汇总同提交、同回滚，不存在「只动子任务、主行陈旧」的中间态）。
///
/// 调用方必须已在本事务内锁住主任务行（`FOR UPDATE`）；`expected_revision` 作 CAS
/// 兜底，0 行即并发冲突（503/409 语义由调用方错误类型承载）。
async fn sync_main_task_summary(
    tx: &mut Transaction<'_, MySql>,
    main_task_id: Uuid,
    expected_revision: u64,
) -> Result<(), ControllerError> {
    let states = read_sub_task_states(tx, main_task_id).await?;
    let (state, outcome) = main_task_convergence(&states);
    let updated = tx_execute(
        tx,
        "UPDATE main_tasks SET state = ?, outcome = ?, counts_json = ?, revision = revision + 1, \
         updated_at = NOW(6) WHERE id = ? AND revision = ?",
        params![
            &enum_wire(&state)?,
            &match outcome {
                None => None,
                Some(outcome) => Some(enum_wire(&outcome)?),
            },
            &counts_json_of(&tally(&states)),
            main_task_id.as_bytes().as_slice(),
            expected_revision,
        ],
    )
    .await?;
    if updated != 1 {
        return Err(ControllerError::RevisionConflict);
    }
    Ok(())
}

/// 以既有 serde 定义（snake_case，Task 2 唯一真相源）解析 `sub_tasks.state`。
/// 非法值视为 DB 损坏返回 `ControllerError::Database`（503），绝不返回 400，
/// 也绝不把非法值当合法 `Unknown` 放行；与 `SubTaskState::from_str_exact` 同源。
fn sub_task_state_from_str(raw: &str) -> Result<SubTaskState, ControllerError> {
    SubTaskState::from_str_exact(raw).ok_or_else(task_row_decode_error)
}

/// 严格 `0`/`1` 解码 `CAST(... AS SIGNED)` 布尔列；污染值即 DB 损坏（503）。
fn strict_bool(raw: i64) -> Result<bool, ControllerError> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(task_row_decode_error()),
    }
}

/// 审计原因脱敏：只保留词数指纹，绝不把自由文本原样写进审计。
fn redact_reason(reason: &str) -> String {
    format!("redacted:{}", reason.split_whitespace().count())
}

/// 11 字段脱敏审计写入：`id` 与 `time_evidence` 由服务端生成，绝不取请求自报值。
async fn insert_task_audit(
    tx: &mut Transaction<'_, MySql>,
    audit: &TaskAudit<'_>,
) -> Result<(), ControllerError> {
    let seq = crate::db::next_event_seq(&TASK_EVENT_SEQ)?;
    let epoch = TASK_PROCESS_EPOCH.get_or_init(Uuid::new_v4);
    let actor_user_id = audit.actor_user_id;
    let actor_user_bytes: Option<&[u8]> = actor_user_id.as_ref().map(|id| id.as_bytes().as_slice());
    tx_execute(
        tx,
        "INSERT INTO audit_events (id, actor_kind, actor_user_id, event_type, target_kind, \
         target_id, params_redacted, outcome, time_evidence, process_epoch, event_seq) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            Uuid::new_v4().as_bytes().as_slice(),
            "user",
            actor_user_bytes,
            audit.event_type,
            audit.target_kind,
            audit.target_id,
            audit.params_redacted,
            audit.outcome,
            audit.time_evidence,
            epoch.as_bytes().as_slice(),
            seq,
        ],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::preview::{preview_token_bytes, preview_token_digest, preview_token_nonce};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 落盘记录：`(token_nonce, actor, targets, token_hash, expires_at)`。
    type RecordedPreview = ([u8; 32], Uuid, Vec<PreviewTarget>, [u8; 32], DateTime<Utc>);

    /// 零 DB 假仓储：只记录预览落盘、其余路径一律以存储错误短路。
    #[derive(Default)]
    struct FakeRepo {
        previews: Mutex<Vec<RecordedPreview>>,
        begins: AtomicUsize,
    }

    // 与生产实现同风格：future 的 Send 边界写进签名，故豁免 manual_async_fn。
    #[allow(clippy::manual_async_fn)]
    impl TaskRepository for FakeRepo {
        fn begin(
            &self,
        ) -> impl std::future::Future<Output = Result<Transaction<'static, MySql>, ControllerError>> + Send
        {
            async move {
                self.begins.fetch_add(1, Ordering::Relaxed);
                Err(ControllerError::Database(sqlx::Error::RowNotFound))
            }
        }

        fn create_preview(
            &self,
            token_nonce: [u8; 32],
            actor_user_id: Uuid,
            targets: &[PreviewTarget],
            token_hash: &[u8; 32],
            expires_at: DateTime<Utc>,
        ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send {
            async move {
                self.previews.lock().expect("unpoisoned").push((
                    token_nonce,
                    actor_user_id,
                    targets.to_vec(),
                    *token_hash,
                    expires_at,
                ));
                Ok(())
            }
        }

        fn consume_preview_and_create_tasks(
            &self,
            _req: &SubmitRequest,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<SubmitOutcome, ControllerError>> + Send
        {
            async { Err(ControllerError::Database(sqlx::Error::RowNotFound)) }
        }

        fn find_main_task(
            &self,
            _main_task_id: Uuid,
        ) -> impl std::future::Future<
            Output = Result<Option<crate::tasks::repository::MainTaskRow>, ControllerError>,
        > + Send {
            async { Ok(None) }
        }

        fn list_subtasks_page(
            &self,
            _main_task_id: Uuid,
            _cursor: Option<String>,
            _limit: usize,
        ) -> impl std::future::Future<
            Output = Result<
                (Vec<crate::tasks::repository::SubTaskRow>, Option<String>),
                ControllerError,
            >,
        > + Send {
            async { Ok((Vec::new(), None)) }
        }

        fn cancel_subtask(
            &self,
            _req: &CancelRequest,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<CancelOutcome, ControllerError>> + Send
        {
            async { Err(ControllerError::Database(sqlx::Error::RowNotFound)) }
        }

        fn time_review(
            &self,
            _req: &TimeReviewRequest,
            _clock: &TimeEvidence,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<TimeReviewOutcome, TimeReviewError>> + Send
        {
            async {
                Err(TimeReviewError::Backend(ControllerError::Database(
                    sqlx::Error::RowNotFound,
                )))
            }
        }

        fn release_lock(
            &self,
            _req: &ReleaseRequest,
            _ctx: &AuthCtx,
        ) -> impl std::future::Future<Output = Result<ReleaseLockOutcome, ReleaseLockError>> + Send
        {
            async {
                Err(ReleaseLockError::Backend(ControllerError::Database(
                    sqlx::Error::RowNotFound,
                )))
            }
        }
    }

    fn target(seed: u8, eligible: bool) -> PreviewTarget {
        PreviewTarget {
            device_id: [seed; 32],
            eligible,
            reason_code: if eligible {
                None
            } else {
                Some(ReasonCode::DeviceOffline)
            },
        }
    }

    fn submit_request(key: &str) -> SubmitRequest {
        let now = Utc::now();
        SubmitRequest {
            idempotency_key: key.to_owned(),
            request_hash: [1u8; 32],
            preview_token_hash: [2u8; 32],
            preview_token_nonce: [3u8; 32],
            observe_deadline: now + chrono::Duration::seconds(60),
            time_evidence: TimeEvidence {
                system_wall_utc: now,
                reference_utc: now,
                quality: TimeQuality::NtpValid,
                clock_epoch: Uuid::new_v4(),
                source: None,
                sample_age_ms: None,
                offset_ms: None,
                uncertainty_ms: None,
            },
        }
    }

    #[test]
    fn time_review_never_extends_deadline() {
        // 核验只能恢复原期限或判过期，绝不允许延长
        assert!(!time_review_allows_extend(
            60_000,
            TimeReviewEvidenceKind::ReferenceDeadlineProven
        ));
        assert!(!time_review_allows_extend(
            1,
            TimeReviewEvidenceKind::Inconclusive
        ));
        assert!(!time_review_allows_extend(
            -1,
            TimeReviewEvidenceKind::ReferenceDeadlineExpired
        ));
        // 任何证据种类下的正数延长都必须拒绝
        for kind in [
            TimeReviewEvidenceKind::ReferenceDeadlineProven,
            TimeReviewEvidenceKind::ReferenceDeadlineExpired,
            TimeReviewEvidenceKind::Inconclusive,
        ] {
            assert!(!time_review_allows_extend(1, kind));
            // 只有「证据已证明 + 延长量为 0」才允许恢复原期限
            assert_eq!(
                time_review_allows_extend(0, kind),
                matches!(kind, TimeReviewEvidenceKind::ReferenceDeadlineProven)
            );
        }
    }

    #[test]
    fn cancel_is_only_for_queued_or_held() {
        use SubTaskState::*;
        assert!(cancel_precondition(Queued, false, true));
        assert!(cancel_precondition(Held, false, true));
        assert!(!cancel_precondition(Dispatching, true, true));
        assert!(!cancel_precondition(Accepted, true, true));
        assert!(!cancel_precondition(Verifying, true, true));
        assert!(!cancel_precondition(Unknown, true, true));
        assert!(!cancel_precondition(Succeeded, true, true));
        assert!(!cancel_precondition(Cancelled, true, true));
        assert!(!cancel_precondition(Expired, true, true));
        assert!(!cancel_precondition(Failed, true, true));
        // 普通用户失去 device.reboot 即不可取消
        assert!(!cancel_precondition(Queued, false, false));
    }

    #[test]
    fn submit_errors_map_to_contract_codes() {
        assert!(matches!(
            ControllerError::from(SubmitError::InvalidArgument),
            ControllerError::InvalidArgument
        ));
        assert!(matches!(
            ControllerError::from(SubmitError::ActorInactive),
            ControllerError::PermissionDenied
        ));
        // 忙目标不整体报错：防御兜底必须是 400，绝不 409
        assert!(matches!(
            ControllerError::from(SubmitError::DeviceBusyCount { busy: 1, total: 2 }),
            ControllerError::InvalidArgument
        ));
        assert!(matches!(
            ControllerError::from(SubmitError::PreviewExpired),
            ControllerError::TaskPreviewInvalidState
        ));
        assert!(matches!(
            ControllerError::from(SubmitError::PreviewConsumed),
            ControllerError::TaskPreviewInvalidState
        ));
        assert!(matches!(
            ControllerError::from(SubmitError::IdempotencyConflict),
            ControllerError::TaskIdempotencyConflict
        ));
        assert!(matches!(
            ControllerError::from(SubmitError::NotVisible),
            ControllerError::NotFound
        ));
    }

    #[test]
    fn sqlx_errors_are_wrapped_as_backend_database() {
        let err = SubmitError::from(sqlx::Error::RowNotFound);
        assert!(matches!(
            err,
            SubmitError::Backend(ControllerError::Database(_))
        ));
        assert!(matches!(
            ControllerError::from(err),
            ControllerError::Database(_)
        ));
    }

    #[test]
    fn counts_json_is_ten_string_keys_and_sums_batches() {
        let empty: serde_json::Value =
            serde_json::from_str(&empty_counts_json()).expect("json object");
        let object = empty.as_object().expect("object");
        assert_eq!(object.len(), 10);
        for (key, value) in object {
            assert_eq!(value.as_str(), Some("0"), "{key} 必须为零频数字符串");
        }
        let batch: serde_json::Value =
            serde_json::from_str(&submit_counts_json(3, 2)).expect("json object");
        assert_eq!(batch["queued"].as_str(), Some("3"));
        assert_eq!(batch["failed"].as_str(), Some("2"));
        assert_eq!(batch["held"].as_str(), Some("0"));
        // 同一状态多次出现必须累加，不得互相覆盖
        let merged = counts_json_of(&[
            (SubTaskState::Queued, 1),
            (SubTaskState::Queued, 2),
            (SubTaskState::Unknown, 4),
        ]);
        let merged: serde_json::Value = serde_json::from_str(&merged).expect("json");
        assert_eq!(merged["queued"].as_str(), Some("3"));
        assert_eq!(merged["unknown"].as_str(), Some("4"));
    }

    #[test]
    fn state_keys_match_serde_wire() {
        for (state, key) in SUB_TASK_STATE_KEYS {
            assert_eq!(enum_wire(&state).expect("wire string"), key);
        }
    }

    #[test]
    fn main_task_convergence_follows_sub_task_distribution() {
        use SubTaskState::*;
        assert_eq!(main_task_convergence(&[]), (MainTaskState::Queued, None));
        assert_eq!(
            main_task_convergence(&[Queued, Queued]),
            (MainTaskState::Queued, None)
        );
        // 混合 [queued, failed] → running 且 outcome 仍为空（04 规格 L79–83）
        assert_eq!(
            main_task_convergence(&[Queued, Failed]),
            (MainTaskState::Running, None)
        );
        assert_eq!(
            main_task_convergence(&[Accepted, Queued]),
            (MainTaskState::Running, None)
        );
        assert_eq!(
            main_task_convergence(&[Failed, Failed]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Failed))
        );
        assert_eq!(
            main_task_convergence(&[Succeeded, Succeeded]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Success))
        );
        assert_eq!(
            main_task_convergence(&[Cancelled]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Cancelled))
        );
        assert_eq!(
            main_task_convergence(&[Unknown]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Unknown))
        );
        assert_eq!(
            main_task_convergence(&[Succeeded, Failed]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Partial))
        );
    }

    /// 固定可复现时刻（不依赖测试运行时刻）。
    fn fixed_instant() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("fixed instant in range")
    }

    fn evidence_at(
        quality: TimeQuality,
        reference: DateTime<Utc>,
        age_ms: Option<u64>,
    ) -> TimeEvidence {
        TimeEvidence {
            system_wall_utc: reference,
            reference_utc: reference,
            quality,
            clock_epoch: Uuid::new_v4(),
            source: None,
            sample_age_ms: age_ms,
            offset_ms: None,
            uncertainty_ms: None,
        }
    }

    /// 同一纪元下的「创建时证据 + 当前时钟」对（时间安全核验用例的共用夹具）。
    fn same_epoch_pair(age_ms: Option<u64>) -> (TimeEvidence, TimeEvidence) {
        let epoch = Uuid::new_v4();
        let mut created = evidence_at(TimeQuality::NtpValid, fixed_instant(), None);
        let mut clock = evidence_at(TimeQuality::NtpValid, fixed_instant(), age_ms);
        created.clock_epoch = epoch;
        clock.clock_epoch = epoch;
        (created, clock)
    }

    #[test]
    fn main_task_convergence_priority_follows_spec_04() {
        use SubTaskState::*;
        let completed = |outcome: MainTaskOutcome| (MainTaskState::Completed, Some(outcome));
        // 全体 held → held（不是 running）
        assert_eq!(
            main_task_convergence(&[Held, Held]),
            (MainTaskState::Held, None)
        );
        // 无执行中、无可运行 queued，至少一个 held → held
        assert_eq!(
            main_task_convergence(&[Held, Succeeded]),
            (MainTaskState::Held, None)
        );
        assert_eq!(
            main_task_convergence(&[Held, Expired, Cancelled]),
            (MainTaskState::Held, None)
        );
        // 批次已开始且仍有可运行 queued → running
        assert_eq!(
            main_task_convergence(&[Queued, Held]),
            (MainTaskState::Running, None)
        );
        assert_eq!(
            main_task_convergence(&[Queued, Expired]),
            (MainTaskState::Running, None)
        );
        // 执行中优先一切
        assert_eq!(
            main_task_convergence(&[Verifying, Held, Succeeded]),
            (MainTaskState::Running, None)
        );
        // 含 unknown 优先 unknown（不是 partial/failed）
        assert_eq!(
            main_task_convergence(&[Unknown, Failed]),
            completed(MainTaskOutcome::Unknown)
        );
        assert_eq!(
            main_task_convergence(&[Succeeded, Unknown]),
            completed(MainTaskOutcome::Unknown)
        );
        assert_eq!(
            main_task_convergence(&[Cancelled, Unknown]),
            completed(MainTaskOutcome::Unknown)
        );
        // 仅 expired、failed + cancelled → failed（不是 partial）
        assert_eq!(
            main_task_convergence(&[Expired]),
            completed(MainTaskOutcome::Failed)
        );
        assert_eq!(
            main_task_convergence(&[Expired, Expired]),
            completed(MainTaskOutcome::Failed)
        );
        assert_eq!(
            main_task_convergence(&[Failed, Cancelled]),
            completed(MainTaskOutcome::Failed)
        );
        // 有成功且有非成功 → partial
        assert_eq!(
            main_task_convergence(&[Succeeded, Expired]),
            completed(MainTaskOutcome::Partial)
        );
        assert_eq!(
            main_task_convergence(&[Succeeded, Cancelled]),
            completed(MainTaskOutcome::Partial)
        );
    }

    #[test]
    fn every_created_sub_task_is_counted_including_visible_forbidden() {
        // 提交期三种落库结局（queued / failed:PERMISSION_DENIED / failed:DEVICE_BUSY）
        // 一律计入 created：漏计失败目标会让十键频数少算、主任务永不收敛。
        let created = [
            SubTaskState::Queued,
            SubTaskState::Failed,
            SubTaskState::Failed,
        ];
        let counts: serde_json::Value =
            serde_json::from_str(&counts_json_of(&tally(&created))).expect("json");
        let total: u64 = counts
            .as_object()
            .expect("object")
            .values()
            .map(|value| {
                value
                    .as_str()
                    .expect("decimal string")
                    .parse::<u64>()
                    .expect("decimal")
            })
            .sum();
        assert_eq!(
            total,
            created.len() as u64,
            "十键频数之和必须等于子任务行数"
        );
        assert_eq!(counts["queued"].as_str(), Some("1"));
        assert_eq!(counts["failed"].as_str(), Some("2"));
        // 全部目标都因 VisibleForbidden 落 failed：主任务必须直接收敛 completed/failed；
        // 漏计时 created 为空，只会得到 (queued, None) 并永远停在非终态。
        assert_eq!(
            main_task_convergence(&[SubTaskState::Failed, SubTaskState::Failed]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Failed))
        );
    }

    #[test]
    fn queue_deadline_uses_trusted_time_base_and_sample_age() {
        let reference = fixed_instant();
        let ttl = chrono::Duration::milliseconds(QUEUE_TTL_MS);
        // NtpValid：reference + 样本年龄 + TTL（样本越旧，排队寿命不得被隐性拉长）
        let ntp = queue_deadline_of(&evidence_at(TimeQuality::NtpValid, reference, Some(150)))
            .expect("deadline");
        assert_eq!(ntp, reference + ttl + chrono::Duration::milliseconds(150));
        // NtpValid 且样本年龄缺失：按 0 处理，绝不让创建失败
        let no_age = queue_deadline_of(&evidence_at(TimeQuality::NtpValid, reference, None))
            .expect("deadline");
        assert_eq!(no_age, reference + ttl);
        // SystemFallback/Stale：reference_utc 已是当前墙钟，不重复累加历史 age
        for quality in [TimeQuality::SystemFallback, TimeQuality::Stale] {
            let fallback =
                queue_deadline_of(&evidence_at(quality, reference, Some(9_999))).expect("deadline");
            assert_eq!(
                fallback,
                reference + ttl,
                "{quality:?} 不得重复累加样本年龄"
            );
        }
        assert!(ntp > reference, "排队截止必须晚于创建参考时刻");
    }

    #[test]
    fn time_review_resumes_only_when_every_queue_deadline_is_live() {
        use TimeReviewDecision::ResumeOriginalDeadline;
        use TimeReviewVerdict::*;
        let (created, clock) = same_epoch_pair(None);
        let live = fixed_instant() + chrono::Duration::seconds(60);
        let past = fixed_instant() - chrono::Duration::seconds(1);
        // 全部未过期 + Resume → 整批恢复
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                ResumeOriginalDeadline,
                &[Some(live), Some(live)]
            ),
            Resume
        );
        // 部分过期 + Resume → 绝不允许整批恢复（保持 held，不是错误恢复）
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                ResumeOriginalDeadline,
                &[Some(live), Some(past)]
            ),
            Unverifiable
        );
        // 全部过期 + Resume → 同样不得恢复
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                ResumeOriginalDeadline,
                &[Some(past)]
            ),
            Unverifiable
        );
        // 缺失 queue_deadline 无法证明未过期，按已过期处置
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                ResumeOriginalDeadline,
                &[Some(live), None]
            ),
            Unverifiable
        );
        // 全部已过期 + Expire → 整批 expired
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                TimeReviewDecision::Expire,
                &[Some(past), None]
            ),
            Expire
        );
        // 部分过期 + Expire → 矛盾请求（整批判过期会误伤未过期者）
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                TimeReviewDecision::Expire,
                &[Some(past), Some(live)]
            ),
            Contradiction
        );
    }

    #[test]
    fn time_review_fails_closed_on_untrusted_or_discontinuous_clock() {
        use TimeReviewDecision::ResumeOriginalDeadline;
        use TimeReviewVerdict::{Resume, Unverifiable};
        let live = fixed_instant() + chrono::Duration::seconds(60);
        let deadlines = [Some(live)];
        let (created, clock) = same_epoch_pair(None);
        // 基线：同纪元、双方 NtpValid、未过期 → 允许恢复
        assert_eq!(
            time_review_verdict(Some(&created), &clock, ResumeOriginalDeadline, &deadlines),
            Resume
        );
        // ① 当前时钟不可信
        for quality in [TimeQuality::SystemFallback, TimeQuality::Stale] {
            let mut untrusted = clock.clone();
            untrusted.quality = quality;
            assert_eq!(
                time_review_verdict(
                    Some(&created),
                    &untrusted,
                    ResumeOriginalDeadline,
                    &deadlines
                ),
                Unverifiable,
                "{quality:?} 的当前时钟不得参与恢复判定"
            );
        }
        // ② 创建当时时钟不可信（纪元虽连续也一样）
        for quality in [TimeQuality::SystemFallback, TimeQuality::Stale] {
            let mut created_at = created.clone();
            created_at.quality = quality;
            assert_eq!(
                time_review_verdict(
                    Some(&created_at),
                    &clock,
                    ResumeOriginalDeadline,
                    &deadlines
                ),
                Unverifiable,
                "创建时 {quality:?} 的原排队期限无法证明有效"
            );
        }
        // ③ 时钟纪元不连续（跳钟/进程换代）：参考系连续性无法证明
        let mut shifted = clock.clone();
        shifted.clock_epoch = Uuid::new_v4();
        assert_eq!(
            time_review_verdict(Some(&created), &shifted, ResumeOriginalDeadline, &deadlines),
            Unverifiable
        );
        // ④ 创建证据缺失（created_time_json 缺失或不可解析）
        assert_eq!(
            time_review_verdict(None, &clock, ResumeOriginalDeadline, &deadlines),
            Unverifiable
        );
    }

    #[test]
    fn stale_sample_clock_cannot_keep_an_expired_task_alive() {
        use TimeReviewDecision::ResumeOriginalDeadline;
        use TimeReviewVerdict::*;
        // 采样时刻 < queue_deadline < 采样时刻 + 样本年龄：必须判过期，
        // 否则陈旧时钟会把已越期的任务错误恢复为 queued。
        let (created, mut clock) = same_epoch_pair(Some(50));
        let deadline = fixed_instant() + chrono::Duration::milliseconds(10);
        assert!(advanced_reference_utc(&clock) > deadline);
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                ResumeOriginalDeadline,
                &[Some(deadline)]
            ),
            Unverifiable
        );
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                TimeReviewDecision::Expire,
                &[Some(deadline)]
            ),
            Expire
        );
        // 样本年龄缺失按 0 推进：同一期限此刻仍未过期，允许恢复
        clock.sample_age_ms = None;
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &clock,
                ResumeOriginalDeadline,
                &[Some(deadline)]
            ),
            Resume
        );
    }

    #[test]
    fn created_time_evidence_parser_is_fail_closed() {
        let evidence = evidence_at(TimeQuality::NtpValid, fixed_instant(), Some(7));
        let raw = serde_json::to_string(&evidence).expect("serialize");
        assert_eq!(parse_created_time_evidence(&raw), Some(evidence));
        // 空串、非对象、缺字段一律 None（无法证明原期限有效，fail-closed）
        for raw in ["", "null", "\"x\"", "{}", "{\"clock_epoch\":\"nope\"}"] {
            assert_eq!(
                parse_created_time_evidence(raw),
                None,
                "{raw:?} 必须解析失败"
            );
        }
    }

    #[test]
    fn constant_time_eq_is_length_and_content_sensitive() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(&[], &[]));
    }

    #[test]
    fn audit_reason_is_never_echoed_verbatim() {
        let redacted = redact_reason("operator forced release after 3 retries");
        assert!(!redacted.contains("operator"));
        assert!(!redacted.contains("retries"));
        assert_eq!(redacted, "redacted:6");
    }

    #[test]
    fn sub_task_state_parser_rejects_polluted_values() {
        assert_eq!(
            sub_task_state_from_str("dispatching").expect("valid"),
            SubTaskState::Dispatching
        );
        // 非法值必须是 503 语义的 DB 损坏，绝不返回 400 或当作 Unknown 放行
        for raw in ["", "DISPATCHING", "unknown_state", "Unknown"] {
            assert!(
                matches!(
                    sub_task_state_from_str(raw),
                    Err(ControllerError::Database(_))
                ),
                "非法状态串 {raw:?} 必须判为 DB 损坏"
            );
        }
    }

    #[tokio::test]
    async fn preview_derives_hash_and_nonce_from_the_returned_token() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let actor = Uuid::new_v4();
        let outcome = service
            .preview(
                actor,
                vec![target(0x02, true), target(0x01, false), target(0x02, true)],
            )
            .await
            .expect("preview");

        // 对外只回一枚 64 位小写 hex token
        let raw = preview_token_bytes(&outcome.preview_token).expect("canonical token");
        // 目标按字节序升序去重，资格判定原样保留（不伪造资格）
        assert_eq!(
            outcome
                .targets
                .iter()
                .map(|target| target.device_id)
                .collect::<Vec<_>>(),
            vec![[0x01u8; 32], [0x02u8; 32]]
        );
        assert_eq!(
            outcome.targets[0].reason_code,
            Some(ReasonCode::DeviceOffline)
        );

        let stored = repo.previews.lock().expect("unpoisoned");
        assert_eq!(stored.len(), 1);
        let (nonce, stored_actor, stored_targets, hash, expires_at) = &stored[0];
        // RT-N2 闭环：token → 同一对派生函数 → 落库值
        assert_eq!(nonce, &preview_token_nonce(&raw));
        assert_eq!(hash, &preview_token_digest(&raw));
        assert_eq!(stored_actor, &actor);
        assert_eq!(stored_targets.len(), 2);
        assert!(*expires_at > Utc::now());
    }

    #[tokio::test]
    async fn preview_rejects_empty_and_oversized_batches_without_writing() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        assert!(matches!(
            service.preview(Uuid::new_v4(), Vec::new()).await,
            Err(ControllerError::InvalidArgument)
        ));
        // 1025 个互不相同的目标：去重后仍超限，必须在落盘前拒绝
        let too_many = (0..1025u32)
            .map(|i| {
                let mut device_id = [0u8; 32];
                device_id[..4].copy_from_slice(&i.to_be_bytes());
                PreviewTarget {
                    device_id,
                    eligible: true,
                    reason_code: None,
                }
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            service.preview(Uuid::new_v4(), too_many).await,
            Err(ControllerError::InvalidArgument)
        ));
        assert!(repo.previews.lock().expect("unpoisoned").is_empty());
    }

    #[tokio::test]
    async fn submit_rejects_non_canonical_idempotency_key_before_any_transaction() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let ctx = AuthCtx {
            user_id: Uuid::new_v4(),
            is_admin: false,
        };
        for key in [
            "",
            "3F2504E0-4F89-41D3-9A0C-0305E82C3301",
            "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
            "not-an-uuid",
        ] {
            let err = service.submit(&submit_request(key), &ctx).await;
            assert!(
                matches!(err, Err(SubmitError::InvalidArgument)),
                "键 {key:?} 必须 400"
            );
        }
        assert_eq!(repo.begins.load(Ordering::Relaxed), 0, "非法键不得开启事务");
    }

    #[tokio::test]
    async fn submit_propagates_storage_faults_as_backend_not_400() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let ctx = AuthCtx {
            user_id: Uuid::new_v4(),
            is_admin: false,
        };
        let err = service
            .submit(
                &submit_request("3f2504e0-4f89-41d3-9a0c-0305e82c3301"),
                &ctx,
            )
            .await
            .expect_err("fake repo always fails to begin");
        assert!(matches!(
            err,
            SubmitError::Backend(ControllerError::Database(_))
        ));
        assert_eq!(repo.begins.load(Ordering::Relaxed), 1);
        // 503 语义保真：Backend(Database) 原样透传，不降级为 400/409
        assert!(matches!(
            err.into_controller_error(),
            ControllerError::Database(_)
        ));
    }

    #[tokio::test]
    async fn release_lock_validates_acknowledgement_before_any_transaction() {
        let repo = FakeRepo::default();
        let service = TaskService::new(&repo);
        let ctx = AuthCtx {
            user_id: Uuid::new_v4(),
            is_admin: true,
        };
        let base = ReleaseRequest {
            subtask_id: Uuid::new_v4(),
            expected_revision: 4,
            acknowledge_unknown: false,
            reason: "operator confirmed".to_owned(),
        };
        assert!(matches!(
            service.release_lock(&base, &ctx).await,
            Err(ReleaseLockError::UnknownNotAcknowledged)
        ));
        let blank = ReleaseRequest {
            acknowledge_unknown: true,
            reason: "   ".to_owned(),
            ..base.clone()
        };
        assert!(matches!(
            service.release_lock(&blank, &ctx).await,
            Err(ReleaseLockError::InvalidArgument)
        ));
        assert_eq!(repo.begins.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod policy_tests {
    use super::{SubmitError, snapshot_allows_task_creation};
    use crate::authz::policy::{GrantRecord, GrantScope, GrantSource, Permission, SnapshotActor};
    use crate::authz::sqlx_repo::AuthorizationSnapshot;
    use crate::devices::repository::DeviceAdminView;
    use crate::error::ControllerError;
    use crate::model::{AdmissionState, ReviewDecision};
    use std::collections::HashMap;
    use uuid::Uuid;

    const POLICY_DEVICE: [u8; 32] = [3u8; 32];
    const ACTIVE_USER: SnapshotActor = SnapshotActor {
        id: [2u8; 16],
        active: true,
        is_admin: false,
        must_change_password: false,
    };

    fn policy_device_fixture(id: [u8; 32], archived: bool) -> DeviceAdminView {
        DeviceAdminView {
            public_key: id,
            display_name: "policy-fixture".into(),
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::Approved,
            revision: 1,
            descriptor: None,
            archived,
        }
    }

    fn policy_snapshot(
        actor: SnapshotActor,
        grants: Vec<GrantRecord>,
        devices: Vec<([u8; 32], bool)>,
    ) -> AuthorizationSnapshot {
        AuthorizationSnapshot {
            actor,
            authz_epoch: 1,
            grants,
            roles: HashMap::new(),
            device_groups: HashMap::new(),
            devices: devices
                .into_iter()
                .map(|(id, archived)| (id, policy_device_fixture(id, archived)))
                .collect(),
        }
    }

    fn policy_direct_grant(user_id: [u8; 16], permissions: Vec<Permission>) -> GrantRecord {
        GrantRecord {
            id: Uuid::new_v4(),
            user_id,
            source: GrantSource::Direct(permissions),
            scope: GrantScope::All,
            revision: 1,
        }
    }

    #[test]
    fn snapshot_allows_task_creation_accepts_reboot_only_grant() {
        let snapshot = policy_snapshot(
            ACTIVE_USER,
            vec![policy_direct_grant(
                ACTIVE_USER.id,
                vec![Permission::DeviceReboot],
            )],
            vec![(POLICY_DEVICE, false)],
        );
        assert!(snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
    }

    #[test]
    fn snapshot_allows_task_creation_rejects_task_read_only_grant() {
        let snapshot = policy_snapshot(
            ACTIVE_USER,
            vec![policy_direct_grant(
                ACTIVE_USER.id,
                vec![Permission::DeviceTaskRead],
            )],
            vec![(POLICY_DEVICE, false)],
        );
        // task_read-only 是 VisibleForbidden，不是 Permitted
        assert!(!snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
    }

    #[test]
    fn snapshot_allows_task_creation_rejects_inactive_or_must_change_actor() {
        let grants = vec![policy_direct_grant(
            ACTIVE_USER.id,
            vec![Permission::DeviceReboot],
        )];
        for actor in [
            SnapshotActor {
                active: false,
                ..ACTIVE_USER
            },
            SnapshotActor {
                must_change_password: true,
                ..ACTIVE_USER
            },
        ] {
            let snapshot = policy_snapshot(actor, grants.clone(), vec![(POLICY_DEVICE, false)]);
            assert!(!snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
        }
    }

    #[test]
    fn snapshot_allows_task_creation_rejects_missing_or_archived_device() {
        let grants = vec![policy_direct_grant(
            ACTIVE_USER.id,
            vec![Permission::DeviceReboot],
        )];
        let missing = policy_snapshot(ACTIVE_USER, grants.clone(), vec![]);
        let archived = policy_snapshot(ACTIVE_USER, grants, vec![(POLICY_DEVICE, true)]);
        assert!(!snapshot_allows_task_creation(&missing, POLICY_DEVICE));
        assert!(!snapshot_allows_task_creation(&archived, POLICY_DEVICE));
    }

    #[test]
    fn submit_error_backend_maps_through_to_same_controller_error() {
        // 错误保真回归：From<SubmitError> for ControllerError 的 Backend(err) => err 原样透传。
        // actor 行缺失在 Identity 真源是 PermissionDenied（403），不得重映 503 或降为 400；
        // Config（503 STORAGE_UNAVAILABLE）保持原变体。本测试不覆盖真实异步 DB 路径。
        let denied = ControllerError::from(SubmitError::Backend(ControllerError::PermissionDenied));
        assert!(matches!(denied, ControllerError::PermissionDenied));
        let config = ControllerError::from(SubmitError::Backend(ControllerError::Config(
            "policy-fixture".into(),
        )));
        assert!(matches!(config, ControllerError::Config(_)));
    }
}

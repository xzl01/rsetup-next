//! 生产 `TaskApi` 适配器（Task 4 缺口闭环）：把 HTTP 边界契约接到
//! [`TaskService<SqlxTaskRepository>`] 与 Identity 权威授权快照上。
//!
//! 可见性契约（规格 01 §6 A-06）：
//! - 不可见与不存在统一 `NotFound`（404 同投影，不输出零值汇总、不泄漏过滤前总数）；
//! - 普通用户只看**本人发起**且**至少含一个当前 `device.task.read` 目标**的主任务；
//! - admin（以授权快照事务内重读的 actor 行为准，即 `is_current_admin`）全域可见，
//!   `view_scope = full`；
//! - 隐藏目标不计入任何 counts、不出现在 children 页；`counts`/`state`/`outcome`
//!   一律按可见全集重算（与 `project_main_task_summary` 的 F06 口径同源）。
//!
//! 授权唯一真源：读路径一律经 `SqlxAuthorizationSnapshotRepository::load_snapshot`
//! 与 `snapshot_effective_permissions` 求值，本模块**不写第二套** grant/role/group
//! 求值，也不得把「整批 has_any」类布尔当可见性判据。owner/admin 门控同样以快照
//! actor（`is_current_admin`）为唯一权威：会话 `Session.is_admin` 只是廉价前置
//! 过滤，快照取得后必须二次门控（并发降级重验，M2）。
//!
//! 时钟口径（RT-N5 缺口闭环）：本适配器通过 [`TaskTimeSource`] **显式注入**时间证据源，
//! `submit` 的创建时证据与 `time_review` 的比对时钟都取自**同一个注入源**的**当前**
//! 快照（每次入口调用各读一次，绝不缓存、绝不复用上一次读到的证据）。生产顶层装配
//! （`main.rs` 的 `production_inputs`）注入与启动门禁共享的同一个 `TimeGateHandle`，
//! 其 `current()` 按样本年龄/跳钟动态派生质量，因此「一次 `NtpValid`」不会被当成永久
//! 可信；证据字段（`quality`/`clock_epoch`/`source`/`sample_age_ms`/`offset_ms`/
//! `uncertainty_ms`/`reference_utc`）一律原样透传，本模块不抬高质量、不抹除字段，也
//! 绝不伪造 `NtpValid`。源处于 `SystemFallback`/`Stale`、样本超龄、墙钟跳钟换代或纪元
//! 不连续时，下游 `TaskService::time_review` 依既定纯策略落 `Inconclusive`（零状态
//! 变更），绝不延长任何原期限。四入参 [`SqlxTaskApi::new`] 保留为兼容构造，等价于注入
//! [`SystemFallbackTimeSource`]（进程内墙钟，恒不可信），既有 fail-closed 行为不变；
//! 注入门禁时钟的新构造是 [`SqlxTaskApi::with_time_source`]。
//!
//! 游标纪律：`/tasks` 与 `/tasks/{id}/children` 的下一页游标一律经
//! `encode_task_cursor`（HMAC 签名，密钥由本结构构造入参显式注入，HTTP 层不经手）
//! 签发；解码失败（篡改/跨端点/换密钥）一律 `InvalidArgument`，绝不回退首页。
//! `/tasks` 的 keyset 比较与排序在仓储层统一到**毫秒时间轴**（与游标 payload 的
//! 毫秒截断同轴，同毫秒行由 `id` 决胜），保证跨页不重复、不丢行、不死循环。

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::ControllerError;
use crate::auth::service::Session;
use crate::authz::policy::{Permission, is_current_admin};
use crate::authz::sqlx_repo::{
    AuthorizationSnapshot, AuthorizationSnapshotRepository, SqlxAuthorizationSnapshotRepository,
};
use crate::tasks::http::{
    CancelTaskRequest, CancelTaskResponse, CancelledTarget, ChildEntry, ChildrenPage,
    ReleaseLockRequest, ReleaseLockResponse, TaskActions, TaskApi, TaskCounts, TaskCursorScope,
    TaskDetail, TaskFuture, TaskOperation, TaskPage, TimeReviewTaskRequest, ViewScope,
    cancel_permitted, decode_task_cursor, encode_task_cursor, parse_decimal_u64,
    project_cancel_result,
};
use crate::tasks::model::{EvidenceKind, MainTaskState, ReasonCode, SubTaskState};
use crate::tasks::preview::{
    PreviewOutcome, PreviewTarget, preview_token_bytes, preview_token_digest, preview_token_nonce,
    request_hash as preview_request_hash, validate_target_count,
};
use crate::tasks::repository::{
    MainTaskRow, SqlxTaskRepository, SubTaskRow, TaskRepository, task_row_decode_error,
};
use crate::tasks::scheduler::OBSERVE_WINDOW_MS;
use crate::tasks::service::{
    AuthCtx, CancelError, CancelOutcome, CancelRequest, ReleaseLockOutcome, ReleaseRequest,
    SubmitError, SubmitOutcome, SubmitRequest, TaskService, TimeReviewDecision, TimeReviewRequest,
    parse_created_time_evidence, snapshot_effective_permissions,
};
use crate::time::TimeGateHandle;
use crate::time::evidence::{TimeEvidence, TimeQuality};

/// 生产 `TaskApi` 实现：服务编排 + 仓储读 + 权威授权快照 + 游标签名密钥。
///
/// 构造入参显式注入（fail-closed：密钥缺失/长度不符即装配失败，无默认密钥）；
/// 顶层集成装配（唯一独立集成任务 owner）负责把本类型 `Arc` 化后塞入
/// `HttpTasksState.api`。
pub struct SqlxTaskApi {
    service: Arc<TaskService<SqlxTaskRepository>>,
    repo: Arc<SqlxTaskRepository>,
    snapshots: Arc<SqlxAuthorizationSnapshotRepository>,
    cursor_secret: [u8; 32],
    /// 时间证据源（`submit` 创建证据与 `time_review` 比对时钟的唯一来源）。
    time: Arc<dyn TaskTimeSource>,
}

/// 本任务唯一 operation 的持久化派生（裁定已定稿）：当前 `TaskOperation` 只有
/// `Reboot` 单变体，且 submit 请求不携带 operation。以**穷尽 match** 派生——未来
/// 枚举新增变体时此处编译失败，强制补持久化映射，绝不静默写错 operation。
fn persisted_task_operation() -> TaskOperation {
    match TaskOperation::Reboot {
        TaskOperation::Reboot => TaskOperation::Reboot,
    }
}

/// `sub_tasks.evidence_json` → `ChildEntry.result_evidence` 的防御性解码。
///
/// 该列当前没有任何生产者（写入属 Task 5 typed 切片 + codec 门禁后的工作），恒
/// NULL；读取侧不得因脏值拖垮整页：非 JSON、JSON 非字符串、未知/大写变体一律
/// `None`（与「证据未产生」同投影），仅精确 serde wire 串才 `Some`。生产者接入
/// 后本函数无需改动即生效。
pub(crate) fn parse_result_evidence(raw: Option<&str>) -> Option<EvidenceKind> {
    let text = raw?;
    let parsed: serde_json::Value = serde_json::from_str(text).ok()?;
    let serde_json::Value::String(wire) = parsed else {
        return None;
    };
    serde_json::from_value::<EvidenceKind>(serde_json::Value::String(wire)).ok()
}

/// 可见全集的十态计数（与 `project_main_task_summary` 的 counts 重算同口径）。
pub(crate) fn task_counts_of(states: &[SubTaskState]) -> TaskCounts {
    let count = |want: SubTaskState| -> u64 {
        u64::try_from(states.iter().filter(|state| **state == want).count()).unwrap_or(u64::MAX)
    };
    TaskCounts {
        queued: count(SubTaskState::Queued),
        held: count(SubTaskState::Held),
        dispatching: count(SubTaskState::Dispatching),
        accepted: count(SubTaskState::Accepted),
        verifying: count(SubTaskState::Verifying),
        succeeded: count(SubTaskState::Succeeded),
        failed: count(SubTaskState::Failed),
        unknown: count(SubTaskState::Unknown),
        cancelled: count(SubTaskState::Cancelled),
        expired: count(SubTaskState::Expired),
    }
}

/// 当前 actor 对设备持有 `device.task.read` 的子任务行（可见全集）。
///
/// 授权判定唯一走 `snapshot_effective_permissions`：admin shortcut、archived/
/// missing 设备排除、actor 行 fail-closed 全部内含于求值器，本模块不复写。
pub(crate) fn readable_subtasks<'a>(
    snapshot: &AuthorizationSnapshot,
    subtasks: &'a [SubTaskRow],
) -> Vec<&'a SubTaskRow> {
    subtasks
        .iter()
        .filter(|subtask| {
            snapshot_effective_permissions(snapshot, subtask.device_id)
                .contains(&Permission::DeviceTaskRead)
        })
        .collect()
}

/// children 页条目：与行一一对应，`eligible` 按当前快照逐设备标记；投影层
/// （`project_task_children`）统一剔除 ineligible 项，本层不提前删行。
pub(crate) fn child_entries_of(
    snapshot: &AuthorizationSnapshot,
    subtasks: &[SubTaskRow],
) -> Vec<ChildEntry> {
    subtasks
        .iter()
        .map(|subtask| ChildEntry {
            id: subtask.id,
            device_id: hex::encode(subtask.device_id),
            state: subtask.state,
            reason_code: subtask.reason_code,
            revision: subtask.revision,
            result_evidence: parse_result_evidence(subtask.evidence_json.as_deref()),
            eligible: snapshot_effective_permissions(snapshot, subtask.device_id)
                .contains(&Permission::DeviceTaskRead),
        })
        .collect()
}

/// `can_cancel`：传入清单中任一目标可取消即为 true（与 cancel 编排同一判据
/// `cancel_permitted`，逐目标查 `DeviceReboot` 权限独立判定）。
///
/// 调用方必须传**全量子任务**（M1）：`actions.can_cancel` 按操作权限/状态独立
/// 判定，不偷与 `device.task.read` 绑定——用户对某隐藏目标持有 `device.reboot`
/// 时 `cancel` 确实会取消它，入口不得因该目标不可见而误关闭。
pub(crate) fn can_cancel_any(
    snapshot: &AuthorizationSnapshot,
    admin_mode: bool,
    is_owner: bool,
    subtasks: &[&SubTaskRow],
) -> bool {
    subtasks.iter().any(|subtask| {
        let has_reboot = snapshot_effective_permissions(snapshot, subtask.device_id)
            .contains(&Permission::DeviceReboot);
        cancel_permitted(admin_mode, is_owner, has_reboot, subtask.state)
    })
}

/// 权威 owner/admin 门控（M2）：owner 或快照有效 admin 才放行，否则与不存在
/// 同投影 `NotFound`（404）。detail/children/cancel 在取得授权快照后必须过这道
/// 二次门控——会话伪称 admin 而快照 actor 已降级时，绝不凭会话声明放行。
pub(crate) fn snapshot_owner_gate(is_owner: bool, admin_mode: bool) -> Result<(), ControllerError> {
    if is_owner || admin_mode {
        Ok(())
    } else {
        Err(ControllerError::NotFound)
    }
}

/// 列表逐行扫描域（M2）：权威快照非有效 admin 时只保留本人发起的行——即使扫描
/// 域按会话伪称 admin 走了全域，他人任务也逐行丢弃（fail-closed，不泄漏存在性）。
pub(crate) fn list_row_in_scope(admin_mode: bool, row_actor: Uuid, caller: Uuid) -> bool {
    admin_mode || row_actor == caller
}

/// children 的 A-06 全集门控（M3）：非有效 admin 且该主任务在当前快照下**全集**
/// 没有任何 `device.task.read` 可读目标 → 与不存在同投影 `NotFound`（404），与
/// `task_detail` 的 `visible_states` 为空口径完全一致；绝不返回 200 空页让 owner
/// 探测「任务存在但当前无可读目标」。有效 admin 不受该门控（全域可见）。
pub(crate) fn children_fully_invisible(admin_mode: bool, readable_count: usize) -> bool {
    !admin_mode && readable_count == 0
}

/// 分批授权快照的合并（M4）：各批共享同一 actor（actor 行由同一用户读出），
/// grants 取并集、roles/device_groups/devices 按键合并，`authz_epoch` 取最小值
/// （保守）。设备 id 入参已去重、跨批不重复，映射键无冲突。
fn merge_snapshot(base: &mut AuthorizationSnapshot, other: AuthorizationSnapshot) {
    base.authz_epoch = base.authz_epoch.min(other.authz_epoch);
    base.grants.extend(other.grants);
    for (role_id, role) in other.roles {
        base.roles.entry(role_id).or_insert(role);
    }
    for (device_id, groups) in other.device_groups {
        base.device_groups
            .entry(device_id)
            .or_default()
            .extend(groups);
    }
    for (public_key, device) in other.devices {
        base.devices.entry(public_key).or_insert(device);
    }
}

/// 游标 payload 的 `created_at_ms` → keyset 时间键；越界（不可表示为 UTC 时刻）
/// 返回 `None`，由调用方映射 `InvalidArgument`，绝不钳位到极值。
pub(crate) fn keyset_from_millis(created_at_ms: i64) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp_millis(created_at_ms)
}

/// 进程内墙钟证据纪元（与 `db::system_fallback_time_evidence` 同一稳定纪元口径：
/// 进程生命周期内恒定，供 `time_review` 的纪元连续性判定使用）。
fn process_clock_epoch() -> Uuid {
    static CLOCK_EPOCH: OnceLock<Uuid> = OnceLock::new();
    *CLOCK_EPOCH.get_or_init(Uuid::new_v4)
}

/// 进程内墙钟快照（`SystemFallback`）：本适配器无 TimeGate 注入时的唯一诚实
/// 时钟——质量如实标注不可信，下游一律 fail-closed（见模块头「时钟口径」）。
pub(crate) fn fallback_clock() -> TimeEvidence {
    let now = Utc::now();
    TimeEvidence {
        system_wall_utc: now,
        reference_utc: now,
        quality: TimeQuality::SystemFallback,
        clock_epoch: process_clock_epoch(),
        source: None,
        sample_age_ms: None,
        offset_ms: None,
        uncertainty_ms: None,
    }
}

/// 任务线时间证据 future（对象安全签名，与 `TaskFuture` 同风格但不携带错误通道：
/// 读时钟本身不产生业务错误，不可信由证据质量表达）。
pub type TimeEvidenceFuture<'a> = Pin<Box<dyn Future<Output = TimeEvidence> + Send + 'a>>;

/// 任务线时间证据源：`submit` 的创建时证据与 `time_review` 的比对时钟的**唯一**读取契约。
///
/// 实现方契约（消费侧据此起见 fail-closed，本模块不复写任何时钟算法）：
/// - 每次调用返回**当前**快照，并按断裂性质分别「降级」与「换代」，两者不等价：
///   **样本超龄**只即时把质量从 `NtpValid` 降级为不可信（`SystemFallback`），**不**更换
///   `clock_epoch`（同一参考系只是变旧，超龄证据与同纪元创建的期限仍可对照）；只有
///   **跳钟失效或连续性断裂**（墙钟跳变、来源切换、同源阶跃超阈值、失效后重新取得样本）
///   才更换 `clock_epoch`——旧纪元上的一切期限/寿命比较不得跨纪元继续信任；
///   绝不把一次 `NtpValid` 缓存成永久可信；
/// - 字段（`system_wall_utc`/`reference_utc`/`quality`/`clock_epoch`/`source`/
///   `sample_age_ms`/`offset_ms`/`uncertainty_ms`）由源自身给出，消费侧原样透传，
///   既不抬高质量也不抹除字段（未知不是 0，不可信不是伪造的理由）。
pub trait TaskTimeSource: Send + Sync {
    fn current_evidence(&self) -> TimeEvidenceFuture<'_>;
}

/// 生产装配：时间门禁句柄**本身**就是任务线时间源。
///
/// `TimeGateHandle::current()` 已在读取侧按 `config.max_evidence_age_ms` 动态降级、
/// 并在 `SystemFallback`/`Stale` 时刷新为当前墙钟，因此把它直接当作源不会引入第二套
/// 时钟口径，也不会冻结时间。
impl TaskTimeSource for TimeGateHandle {
    fn current_evidence(&self) -> TimeEvidenceFuture<'_> {
        Box::pin(self.current())
    }
}

/// 兼容装配的默认源（无门禁注入时唯一诚实时钟）：进程内墙钟 `SystemFallback`。
///
/// 与 [`fallback_clock`] 逐个快照等价：质量如实标注不可信、纪元进程内稳定、无来源/
/// 无偏差/无样本年龄，下游 `time_review` 据此恒 fail-closed（`Inconclusive`）。
pub struct SystemFallbackTimeSource;

impl TaskTimeSource for SystemFallbackTimeSource {
    fn current_evidence(&self) -> TimeEvidenceFuture<'_> {
        Box::pin(async { fallback_clock() })
    }
}

/// `submit` 消费入口的时钟前置段：从注入源读**当前**证据并组装 [`SubmitRequest`]。
///
/// 唯一读取点（一次调用读一次）：观察窗口与创建证据必须来自**同一**快照，否则同一
/// 次提交内部就会跨参考系。本函数不抬高质量、不重算字段，也不延长任何期限。
///
/// `raw_preview_token` 是 `preview_token_bytes` 的产物；该解码器只接受 64 位**小写**
/// hex，故 `hex::encode(raw_preview_token)` 与调用方手里的原始令牌字符串逐字节相等，
/// 幂等摘要与重构前完全同源（由单测
/// `submit_request_binds_evidence_and_keeps_token_derived_digests` 逐字段钉死）。
pub async fn prepare_submit_request(
    time: &dyn TaskTimeSource,
    raw_preview_token: &[u8; 32],
    idempotency_key: &str,
) -> Result<SubmitRequest, ControllerError> {
    let time_evidence = time.current_evidence().await;
    let window = chrono::Duration::try_milliseconds(OBSERVE_WINDOW_MS)
        .ok_or_else(|| ControllerError::Config("observe window out of range".into()))?;
    let observe_deadline = time_evidence
        .reference_utc
        .checked_add_signed(window)
        .ok_or_else(|| ControllerError::Config("observe deadline out of range".into()))?;
    Ok(SubmitRequest {
        idempotency_key: idempotency_key.to_owned(),
        // 请求体只携带预览令牌：幂等摘要绑定令牌本身，目标清单以服务端
        // 预览快照为唯一真相源（客户端永不携带目标/摘要/counts）。
        request_hash: preview_request_hash(&hex::encode(raw_preview_token), &[], &[]),
        preview_token_hash: preview_token_digest(raw_preview_token),
        preview_token_nonce: preview_token_nonce(raw_preview_token),
        observe_deadline,
        time_evidence,
    })
}

/// `time_review` 消费入口的时钟前置段：从注入源读**当前**证据作为比对时钟。
pub async fn prepare_time_review_clock(time: &dyn TaskTimeSource) -> TimeEvidence {
    time.current_evidence().await
}

/// 去重并升序（`[u8; 32]` 字节序）：`load_snapshot` 入参的稳定形状。
fn unique_device_ids(device_ids: impl Iterator<Item = [u8; 32]>) -> Vec<[u8; 32]> {
    device_ids.collect::<BTreeSet<_>>().into_iter().collect()
}

/// 组装 `TaskDetail`（详情与列表项共用；投影层再按 `visible_states` 重算
/// state/outcome/counts）。
///
/// - 可见全集为空 → `NotFound`（完全不可见与不存在统一 404，不输出零值汇总）；
/// - `view_scope` 仅当快照 actor 为有效 admin 时 `full`；
/// - `counts`/`visible_states` **严格仅按 `visible`（task.read 可见子集）计算**，
///   隐藏目标不计入任何 counts、不泄漏隐藏数量；
/// - `actions.can_cancel` 按 `all_subtasks`（**全量**子任务，M1）逐目标查
///   `DeviceReboot` 权限独立判定，不偷与 `device.task.read` 绑定。
pub(crate) fn assemble_detail(
    row: &MainTaskRow,
    snapshot: &AuthorizationSnapshot,
    admin_mode: bool,
    is_owner: bool,
    visible: &[&SubTaskRow],
    all_subtasks: &[SubTaskRow],
) -> Result<TaskDetail, ControllerError> {
    let visible_states: Vec<SubTaskState> = visible.iter().map(|subtask| subtask.state).collect();
    if visible_states.is_empty() {
        return Err(ControllerError::NotFound);
    }
    let created_time =
        parse_created_time_evidence(&row.created_time_json).ok_or_else(task_row_decode_error)?;
    let all_refs: Vec<&SubTaskRow> = all_subtasks.iter().collect();
    Ok(TaskDetail {
        id: row.id,
        operation: persisted_task_operation(),
        state: row.state,
        outcome: row.outcome,
        revision: row.revision,
        created_time,
        view_scope: if admin_mode {
            ViewScope::Full
        } else {
            ViewScope::AuthorizedSubset
        },
        counts: task_counts_of(&visible_states),
        actions: TaskActions {
            can_cancel: can_cancel_any(snapshot, admin_mode, is_owner, &all_refs),
        },
        visible_states,
    })
}

impl SqlxTaskApi {
    /// 兼容构造（四入参）：时间证据源固定为 [`SystemFallbackTimeSource`]（进程内墙钟，
    /// 恒不可信），既有调用点与 fail-closed 行为逐字节不变。生产进程应改用
    /// [`Self::with_time_source`] 注入门禁时钟。
    pub fn new(
        service: Arc<TaskService<SqlxTaskRepository>>,
        repo: Arc<SqlxTaskRepository>,
        snapshots: Arc<SqlxAuthorizationSnapshotRepository>,
        cursor_secret: [u8; 32],
    ) -> Self {
        Self::with_time_source(
            service,
            repo,
            snapshots,
            cursor_secret,
            Arc::new(SystemFallbackTimeSource),
        )
    }

    /// 构造入参全部显式注入；`cursor_secret` 由装配方从环境链载入并校验
    /// （缺失/长度不符即装配失败），本类型不设默认密钥。
    ///
    /// `time` 是 `submit` 创建证据与 `time_review` 比对时钟的**同一**注入源：生产顶层
    /// 装配传入与启动门禁共享的 `TimeGateHandle`（`Arc` 克隆共享同一权威快照，不是
    /// 第二个采样器）；本类型不自行取墙钟、不缓存读到的证据。
    pub fn with_time_source(
        service: Arc<TaskService<SqlxTaskRepository>>,
        repo: Arc<SqlxTaskRepository>,
        snapshots: Arc<SqlxAuthorizationSnapshotRepository>,
        cursor_secret: [u8; 32],
        time: Arc<dyn TaskTimeSource>,
    ) -> Self {
        Self {
            service,
            repo,
            snapshots,
            cursor_secret,
            time,
        }
    }

    /// 服务层廉价前置快照；权威 actor 重验始终在事务内（`users` 环节唯一取锁点）。
    fn auth_ctx(&self, actor: &Session) -> AuthCtx {
        AuthCtx {
            user_id: Uuid::from_bytes(actor.user.id),
            is_admin: actor.user.is_admin,
        }
    }

    async fn load_snapshot(
        &self,
        actor: &Session,
        device_ids: &[[u8; 32]],
    ) -> Result<AuthorizationSnapshot, ControllerError> {
        self.snapshots
            .load_snapshot(actor.user.id, device_ids)
            .await
    }

    /// 分批授权快照（M4）：单页列表的 unique 设备数可达 `200 × 1024`，一次
    /// `IN (...)` 会撞上 MySQL 预处理语句 65,535 占位符上限（整页 503）。按
    /// [`SNAPSHOT_DEVICE_CHUNK`] 分批读取后合并；空设备清单保持一次空查询
    /// （快照仍需 actor 行）。
    async fn load_snapshot_chunked(
        &self,
        actor: &Session,
        device_ids: &[[u8; 32]],
    ) -> Result<AuthorizationSnapshot, ControllerError> {
        const SNAPSHOT_DEVICE_CHUNK: usize = 1024;
        let mut chunks = device_ids.chunks(SNAPSHOT_DEVICE_CHUNK);
        let first = chunks.next().unwrap_or(&[]);
        let mut merged = self.load_snapshot(actor, first).await?;
        for chunk in chunks {
            let next = self.load_snapshot(actor, chunk).await?;
            merge_snapshot(&mut merged, next);
        }
        Ok(merged)
    }

    async fn preview_inner(
        &self,
        actor: &Session,
        device_ids: &[[u8; 32]],
        group_ids: &[Uuid],
    ) -> Result<PreviewOutcome, ControllerError> {
        // `group_ids`：`PreviewTarget` 不携带 group 维度（预览快照只存设备清单，
        // 提交侧 `device_target_hash` 恒以空 groups 比对）；本实现不展开分组，
        // 目标恒为显式传入的 `device_ids`（fail-closed，不静默扩大授权面）。
        let _ = group_ids;
        // M4：目标数量校验（1..=1024）前移到读快照**之前**——超限/空清单绝不
        // 先用请求里的设备清单放大一次授权读（service 层同口径校验保留兜底）。
        validate_target_count(device_ids.len())?;
        let snapshot = self.load_snapshot(actor, device_ids).await?;
        let mut targets = Vec::with_capacity(device_ids.len());
        let mut any_visible = false;
        for device_id in device_ids {
            let effective = snapshot_effective_permissions(&snapshot, *device_id);
            if effective.is_empty() {
                // 完全不可见与可见无权同投影（PERMISSION_DENIED），不泄露差异。
                targets.push(PreviewTarget {
                    device_id: *device_id,
                    eligible: false,
                    reason_code: Some(ReasonCode::PermissionDenied),
                });
                continue;
            }
            any_visible = true;
            let eligible = effective.contains(&Permission::DeviceReboot);
            targets.push(PreviewTarget {
                device_id: *device_id,
                eligible,
                reason_code: if eligible {
                    None
                } else {
                    Some(ReasonCode::PermissionDenied)
                },
            });
        }
        // 全部目标不可见 → 404（与不存在同投影；预览要求至少一个可见目标）。
        if !any_visible {
            return Err(ControllerError::NotFound);
        }
        self.service
            .preview(Uuid::from_bytes(actor.user.id), targets)
            .await
    }

    async fn submit_inner(
        &self,
        actor: &Session,
        preview_token: &str,
        idempotency_key: &str,
    ) -> Result<SubmitOutcome, ControllerError> {
        let raw_token =
            preview_token_bytes(preview_token).ok_or(ControllerError::InvalidArgument)?;
        // 时钟前置段：从注入源读一次当前证据（与 time_review 同源），随后才触达存储。
        let request =
            prepare_submit_request(self.time.as_ref(), &raw_token, idempotency_key).await?;
        let ctx = self.auth_ctx(actor);
        self.service
            .submit(&request, &ctx)
            .await
            .map_err(SubmitError::into_controller_error)
    }

    async fn list_inner(
        &self,
        actor: &Session,
        state: Option<MainTaskState>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<TaskPage, ControllerError> {
        let secret = self.cursor_secret;
        let caller = Uuid::from_bytes(actor.user.id);
        let after = match cursor {
            None => None,
            Some(raw) => {
                let (created_at_ms, tie_break_id) =
                    decode_task_cursor(TaskCursorScope::Tasks, raw, &secret)?;
                let created_at =
                    keyset_from_millis(created_at_ms).ok_or(ControllerError::InvalidArgument)?;
                Some((created_at, tie_break_id))
            }
        };
        // 会话侧 admin → 全域扫描；普通用户只扫本人发起（spec 01 §6 A-06）。
        // 这只是廉价前置过滤：权威 admin 判定以授权快照事务内重读的 actor 行为准
        // （下方 `admin_mode`），会话伪称 admin 而快照已降级时逐行丢弃（M2）。
        let actor_filter = if actor.user.is_admin {
            None
        } else {
            Some(caller)
        };
        let scan = self
            .repo
            .list_main_tasks_page(actor_filter, state, after, limit)
            .await?;
        if scan.rows.is_empty() {
            return Ok(TaskPage {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        let main_ids: Vec<Uuid> = scan.rows.iter().map(|row| row.id).collect();
        let children = self.repo.list_subtasks_for_main_tasks(&main_ids).await?;
        let device_ids = unique_device_ids(children.iter().map(|(_, subtask)| subtask.device_id));
        // M4：本页 unique 设备数可达 200 × 1024，分批读授权快照后合并。
        let snapshot = self.load_snapshot_chunked(actor, &device_ids).await?;
        let admin_mode = is_current_admin(&snapshot.actor);
        let mut by_main: HashMap<Uuid, Vec<SubTaskRow>> = HashMap::new();
        for (main_id, subtask) in children {
            by_main.entry(main_id).or_default().push(subtask);
        }
        let mut items = Vec::new();
        for row in &scan.rows {
            // M2：权威快照已非 admin 时，他人发起的行逐行丢弃（即使扫描域是全域，
            // fail-closed，不泄漏任何存在性信号）。
            if !list_row_in_scope(admin_mode, row.actor_user_id, caller) {
                continue;
            }
            let empty: Vec<SubTaskRow> = Vec::new();
            let all = by_main.get(&row.id).unwrap_or(&empty);
            let visible = readable_subtasks(&snapshot, all);
            if visible.is_empty() {
                // 完全不可见主任务：跳过，不计入页、不泄漏过滤前总数。
                continue;
            }
            items.push(assemble_detail(
                row,
                &snapshot,
                admin_mode,
                row.actor_user_id == caller,
                &visible,
                all,
            )?);
        }
        // 下一页游标 = 最后一条**已扫描候选**的 keyset 键（扫描满页才有）：仓储层
        // 已把比较与排序统一到毫秒时间轴（与游标 payload 同轴，同毫秒由 id 决胜），
        // 故不重扫、不死循环；游标经 HMAC 签名。
        let next_cursor = scan.next.map(|(created_at, id)| {
            encode_task_cursor(
                TaskCursorScope::Tasks,
                created_at.timestamp_millis(),
                id,
                &secret,
            )
        });
        Ok(TaskPage { items, next_cursor })
    }

    async fn detail_inner(
        &self,
        actor: &Session,
        task_id: Uuid,
    ) -> Result<TaskDetail, ControllerError> {
        let caller = Uuid::from_bytes(actor.user.id);
        let (row, subtasks) = self
            .repo
            .load_main_task_with_subtasks(task_id)
            .await?
            .ok_or(ControllerError::NotFound)?;
        let is_owner = row.actor_user_id == caller;
        // owner 身份先于设备可见性判定：会话侧廉价前置过滤（非 owner 且会话非
        // admin → 404），权威门控在快照后二次重验（M2）。
        if !is_owner && !actor.user.is_admin {
            return Err(ControllerError::NotFound);
        }
        let device_ids = unique_device_ids(subtasks.iter().map(|subtask| subtask.device_id));
        let snapshot = self.load_snapshot(actor, &device_ids).await?;
        let admin_mode = is_current_admin(&snapshot.actor);
        // M2：会话伪称 admin 而快照 actor 已降级 → 404（与不存在同投影）。
        snapshot_owner_gate(is_owner, admin_mode)?;
        let visible = readable_subtasks(&snapshot, &subtasks);
        assemble_detail(&row, &snapshot, admin_mode, is_owner, &visible, &subtasks)
    }

    async fn children_inner(
        &self,
        actor: &Session,
        task_id: Uuid,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ChildrenPage, ControllerError> {
        let secret = self.cursor_secret;
        let caller = Uuid::from_bytes(actor.user.id);
        // 主任务行与**全集**子任务同一读事务取出（M3）：A-06 门控必须按全部目标
        // 判定，只看当前页会把「本页恰无可读目标」误判成「整单不可见」（反之
        // 漏判）。
        let (row, all_subtasks) = self
            .repo
            .load_main_task_with_subtasks(task_id)
            .await?
            .ok_or(ControllerError::NotFound)?;
        let is_owner = row.actor_user_id == caller;
        // 会话侧廉价前置过滤；权威门控在快照后二次重验（M2）。
        if !is_owner && !actor.user.is_admin {
            return Err(ControllerError::NotFound);
        }
        // children 页 keyset 键是子任务 id（`uq_sub_tasks_main_device` 下唯一稳定
        // 全序）；payload 的 created_at_ms 仅经合法性校验保持游标形状一致。
        let raw_cursor = match cursor {
            None => None,
            Some(raw) => {
                let (created_at_ms, tie_break_id) =
                    decode_task_cursor(TaskCursorScope::TaskChildren, raw, &secret)?;
                keyset_from_millis(created_at_ms).ok_or(ControllerError::InvalidArgument)?;
                Some(hex::encode(tie_break_id.as_bytes()))
            }
        };
        let (subtasks, raw_next) = self
            .repo
            .list_subtasks_page(task_id, raw_cursor, limit)
            .await?;
        let device_ids = unique_device_ids(all_subtasks.iter().map(|subtask| subtask.device_id));
        let snapshot = self.load_snapshot(actor, &device_ids).await?;
        let admin_mode = is_current_admin(&snapshot.actor);
        // M2：会话伪称 admin 而快照 actor 已降级 → 404（与不存在同投影）。
        snapshot_owner_gate(is_owner, admin_mode)?;
        // M3：全集没有任何 `device.task.read` 可读目标 → 404（与 detail 的
        // `visible_states` 为空同口径），绝不返回 200 空页。
        let readable_count = readable_subtasks(&snapshot, &all_subtasks).len();
        if children_fully_invisible(admin_mode, readable_count) {
            return Err(ControllerError::NotFound);
        }
        let items = child_entries_of(&snapshot, &subtasks);
        let next_cursor = match (raw_next, subtasks.last()) {
            (Some(_), Some(last)) => Some(encode_task_cursor(
                TaskCursorScope::TaskChildren,
                last.created_at.timestamp_millis(),
                last.id,
                &secret,
            )),
            _ => None,
        };
        Ok(ChildrenPage { items, next_cursor })
    }

    async fn cancel_inner(
        &self,
        actor: &Session,
        task_id: Uuid,
        request: CancelTaskRequest,
    ) -> Result<CancelTaskResponse, ControllerError> {
        let expected_revision = parse_decimal_u64(&request.expected_revision)?;
        let caller = Uuid::from_bytes(actor.user.id);
        let (row, subtasks) = self
            .repo
            .load_main_task_with_subtasks(task_id)
            .await?
            .ok_or(ControllerError::NotFound)?;
        let is_owner = row.actor_user_id == caller;
        // 会话侧廉价前置过滤；权威门控在快照后二次重验（M2）。
        if !is_owner && !actor.user.is_admin {
            return Err(ControllerError::NotFound);
        }
        // 主任务级乐观并发：`expected_revision` 匹配 `main_tasks.revision`（409）。
        if row.revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }
        let device_ids = unique_device_ids(subtasks.iter().map(|subtask| subtask.device_id));
        let snapshot = self.load_snapshot(actor, &device_ids).await?;
        let admin_mode = is_current_admin(&snapshot.actor);
        // M2：会话伪称 admin 而快照 actor 已降级 → 404，绝不进入取消编排。
        snapshot_owner_gate(is_owner, admin_mode)?;
        let ctx = self.auth_ctx(actor);
        let mut cancelled = Vec::new();
        for subtask in &subtasks {
            let effective = snapshot_effective_permissions(&snapshot, subtask.device_id);
            if !cancel_permitted(
                admin_mode,
                is_owner,
                effective.contains(&Permission::DeviceReboot),
                subtask.state,
            ) {
                continue;
            }
            // `readable_in_tx` 来自当前授权快照（有效 admin 或该设备
            // `device.task.read`），不来自客户端声明或整批布尔。
            let readable_in_tx = admin_mode || effective.contains(&Permission::DeviceTaskRead);
            let request = CancelRequest {
                subtask_id: subtask.id,
                expected_revision: subtask.revision,
            };
            match self.service.cancel(&request, &ctx).await {
                Ok(CancelOutcome::Cancelled) => cancelled.push(CancelledTarget {
                    id: subtask.id,
                    device_id: subtask.device_id,
                    readable_in_tx,
                }),
                // 单目标业务性结果（终态/不可取消/逐行并发冲突）不整体失败；
                // 存储级故障必须上抛（503），不伪装成部分成功。
                Ok(_) => {}
                // L3：`PermissionDenied` 是请求级授权失败（actor 失效/待改密），
                // 不是单目标业务结果——必须上抛 403，绝不吞成 `{accepted:true}` 200。
                Err(CancelError::PermissionDenied) => {
                    return Err(ControllerError::PermissionDenied);
                }
                Err(CancelError::Backend(err)) => return Err(err),
            }
        }
        Ok(project_cancel_result(&cancelled))
    }

    async fn time_review_inner(
        &self,
        actor: &Session,
        task_id: Uuid,
        request: TimeReviewTaskRequest,
    ) -> Result<TaskDetail, ControllerError> {
        let expected_revision = parse_decimal_u64(&request.expected_revision)?;
        let decision = match request.decision {
            crate::tasks::http::TimeReviewDecision::ResumeOriginalDeadline => {
                TimeReviewDecision::ResumeOriginalDeadline
            }
            crate::tasks::http::TimeReviewDecision::Expire => TimeReviewDecision::Expire,
        };
        // 时钟前置段：与 submit 共用同一个注入源，比对时钟即该源的**当前**快照。
        let clock = prepare_time_review_clock(self.time.as_ref()).await;
        let ctx = self.auth_ctx(actor);
        let request = TimeReviewRequest {
            main_task_id: task_id,
            expected_revision,
            decision,
            evidence_kind: request.evidence,
        };
        // `Completed` 与 `Inconclusive` 都返回当前授权视图（Task 6 契约：
        // Inconclusive 仍 200 返回当前 MainTask 视图、零状态变更）；
        // 其余错误（400/403/409/503）原样上抛。
        self.service.time_review(&request, &clock, &ctx).await?;
        self.detail_inner(actor, task_id).await
    }

    async fn release_inner(
        &self,
        actor: &Session,
        subtask_id: Uuid,
        request: ReleaseLockRequest,
    ) -> Result<ReleaseLockResponse, ControllerError> {
        let expected_revision = parse_decimal_u64(&request.expected_revision)?;
        let ctx = self.auth_ctx(actor);
        let request = ReleaseRequest {
            subtask_id,
            expected_revision,
            acknowledge_unknown: request.acknowledge_unknown,
            reason: request.reason,
        };
        match self.service.release_lock(&request, &ctx).await? {
            // 服务层唯一成功形态：`unknown` 子任务保持原状态，只删本代际锁。
            ReleaseLockOutcome::Released { .. } => Ok(ReleaseLockResponse {
                released: true,
                state: SubTaskState::Unknown,
            }),
        }
    }
}

impl TaskApi for SqlxTaskApi {
    fn preview<'a>(
        &'a self,
        actor: &'a Session,
        device_ids: &'a [[u8; 32]],
        group_ids: &'a [Uuid],
    ) -> TaskFuture<'a, PreviewOutcome> {
        Box::pin(async move { self.preview_inner(actor, device_ids, group_ids).await })
    }

    fn submit<'a>(
        &'a self,
        actor: &'a Session,
        preview_token: &'a str,
        idempotency_key: &'a str,
    ) -> TaskFuture<'a, SubmitOutcome> {
        Box::pin(async move {
            self.submit_inner(actor, preview_token, idempotency_key)
                .await
        })
    }

    fn list_tasks<'a>(
        &'a self,
        actor: &'a Session,
        state: Option<MainTaskState>,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> TaskFuture<'a, TaskPage> {
        Box::pin(async move { self.list_inner(actor, state, cursor, limit).await })
    }

    fn task_detail<'a>(&'a self, actor: &'a Session, task_id: Uuid) -> TaskFuture<'a, TaskDetail> {
        Box::pin(async move { self.detail_inner(actor, task_id).await })
    }

    fn task_children<'a>(
        &'a self,
        actor: &'a Session,
        task_id: Uuid,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> TaskFuture<'a, ChildrenPage> {
        Box::pin(async move { self.children_inner(actor, task_id, cursor, limit).await })
    }

    fn cancel<'a>(
        &'a self,
        actor: &'a Session,
        task_id: Uuid,
        request: CancelTaskRequest,
    ) -> TaskFuture<'a, CancelTaskResponse> {
        Box::pin(async move { self.cancel_inner(actor, task_id, request).await })
    }

    fn time_review<'a>(
        &'a self,
        actor: &'a Session,
        task_id: Uuid,
        request: TimeReviewTaskRequest,
    ) -> TaskFuture<'a, TaskDetail> {
        Box::pin(async move { self.time_review_inner(actor, task_id, request).await })
    }

    fn release_lock<'a>(
        &'a self,
        actor: &'a Session,
        subtask_id: Uuid,
        request: ReleaseLockRequest,
    ) -> TaskFuture<'a, ReleaseLockResponse> {
        Box::pin(async move { self.release_inner(actor, subtask_id, request).await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::policy::{GrantRecord, GrantScope, GrantSource, SnapshotActor};
    use crate::devices::repository::DeviceAdminView;
    use crate::model::{AdmissionState, ReviewDecision};
    use crate::tasks::model::MainTaskOutcome;
    use crate::tasks::preview::new_preview_token;

    const ACTOR_ID: [u8; 16] = [0x11; 16];
    const DEVICE_A: [u8; 32] = [0xA1; 32];
    const DEVICE_B: [u8; 32] = [0xB2; 32];
    const DEVICE_C: [u8; 32] = [0xC3; 32];

    fn fixed_time() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("fixed instant in range")
    }

    fn evidence_fixture() -> TimeEvidence {
        TimeEvidence {
            system_wall_utc: fixed_time(),
            reference_utc: fixed_time(),
            quality: TimeQuality::NtpValid,
            clock_epoch: Uuid::nil(),
            source: None,
            sample_age_ms: Some(10),
            offset_ms: None,
            uncertainty_ms: None,
        }
    }

    fn device_view(public_key: [u8; 32], archived: bool) -> DeviceAdminView {
        DeviceAdminView {
            public_key,
            display_name: "dev".to_owned(),
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::Approved,
            revision: 1,
            descriptor: None,
            archived,
        }
    }

    fn direct_grant(device_id: [u8; 32], permissions: Vec<Permission>) -> GrantRecord {
        GrantRecord {
            id: Uuid::new_v4(),
            user_id: ACTOR_ID,
            source: GrantSource::Direct(permissions),
            scope: GrantScope::Device(device_id),
            revision: 1,
        }
    }

    fn snapshot_with(
        is_admin: bool,
        grants: Vec<GrantRecord>,
        devices: Vec<DeviceAdminView>,
    ) -> AuthorizationSnapshot {
        AuthorizationSnapshot {
            actor: SnapshotActor {
                id: ACTOR_ID,
                active: true,
                is_admin,
                must_change_password: false,
            },
            authz_epoch: 7,
            grants,
            roles: HashMap::new(),
            device_groups: HashMap::new(),
            devices: devices
                .into_iter()
                .map(|device| (device.public_key, device))
                .collect(),
        }
    }

    /// 普通用户：A 有 `device.task.read`，B 无任何权限（不可见）。
    fn reader_snapshot() -> AuthorizationSnapshot {
        snapshot_with(
            false,
            vec![direct_grant(DEVICE_A, vec![Permission::DeviceTaskRead])],
            vec![device_view(DEVICE_A, false), device_view(DEVICE_B, false)],
        )
    }

    fn subtask(device_id: [u8; 32], state: SubTaskState) -> SubTaskRow {
        SubTaskRow {
            id: Uuid::new_v4(),
            device_id,
            state,
            reason_code: None,
            revision: 1,
            evidence_json: None,
            created_at: fixed_time(),
        }
    }

    fn main_row() -> MainTaskRow {
        MainTaskRow {
            id: Uuid::new_v4(),
            actor_user_id: Uuid::new_v4(),
            state: MainTaskState::Running,
            outcome: None,
            revision: 9,
            counts_json: "{}".to_owned(),
            created_time_json: serde_json::to_string(&evidence_fixture()).unwrap(),
            created_at: fixed_time(),
        }
    }

    #[test]
    fn persisted_task_operation_is_the_exhaustive_single_variant() {
        assert_eq!(persisted_task_operation(), TaskOperation::Reboot);
    }

    #[test]
    fn result_evidence_decodes_defensively() {
        assert_eq!(parse_result_evidence(None), None);
        assert_eq!(
            parse_result_evidence(Some("\"attempted\"")),
            Some(EvidenceKind::Attempted)
        );
        assert_eq!(
            parse_result_evidence(Some("\"boot_transition_observed\"")),
            Some(EvidenceKind::BootTransitionObserved)
        );
        // `"none"` 是合法变体（证据明确未产生），与列 NULL（None）区分
        assert_eq!(
            parse_result_evidence(Some("\"none\"")),
            Some(EvidenceKind::None)
        );
        // 非 JSON / JSON null / 非字符串 / 未知或大写变体一律 None，绝不 panic
        assert_eq!(parse_result_evidence(Some("not json")), None);
        assert_eq!(parse_result_evidence(Some("null")), None);
        assert_eq!(parse_result_evidence(Some("42")), None);
        assert_eq!(parse_result_evidence(Some("{}")), None);
        assert_eq!(parse_result_evidence(Some("\"bogus\"")), None);
        assert_eq!(parse_result_evidence(Some("\"ATTEMPTED\"")), None);
    }

    #[test]
    fn task_counts_of_aggregates_visible_states() {
        let states = vec![
            SubTaskState::Queued,
            SubTaskState::Queued,
            SubTaskState::Held,
            SubTaskState::Succeeded,
            SubTaskState::Cancelled,
        ];
        let counts = task_counts_of(&states);
        assert_eq!(counts.queued, 2);
        assert_eq!(counts.held, 1);
        assert_eq!(counts.succeeded, 1);
        assert_eq!(counts.cancelled, 1);
        assert_eq!(counts.dispatching, 0);
        assert_eq!(counts.accepted, 0);
        assert_eq!(counts.verifying, 0);
        assert_eq!(counts.failed, 0);
        assert_eq!(counts.unknown, 0);
        assert_eq!(counts.expired, 0);
    }

    #[test]
    fn readable_subtasks_only_keeps_task_read_devices() {
        let snapshot = reader_snapshot();
        let subtasks = vec![
            subtask(DEVICE_A, SubTaskState::Queued),
            subtask(DEVICE_B, SubTaskState::Dispatching),
        ];
        let readable = readable_subtasks(&snapshot, &subtasks);
        assert_eq!(readable.len(), 1);
        assert_eq!(readable[0].device_id, DEVICE_A);

        let entries = child_entries_of(&snapshot, &subtasks);
        // 行一一对应；eligible 是唯一可见性判据，剔除属投影层职责
        assert_eq!(entries.len(), 2);
        assert!(entries[0].eligible);
        assert!(!entries[1].eligible);
        assert_eq!(entries[0].device_id, hex::encode(DEVICE_A));
        assert_eq!(entries[1].device_id, hex::encode(DEVICE_B));
    }

    #[test]
    fn admin_snapshot_reads_live_devices_but_not_archived_or_missing() {
        let snapshot = snapshot_with(
            true,
            vec![],
            vec![device_view(DEVICE_A, false), device_view(DEVICE_B, true)],
        );
        let subtasks = vec![
            subtask(DEVICE_A, SubTaskState::Queued),
            // B 已归档：即使 admin 也不可见（求值器唯一真源）
            subtask(DEVICE_B, SubTaskState::Held),
            // C 不在快照（设备已删除）：不可见
            subtask(DEVICE_C, SubTaskState::Succeeded),
        ];
        let readable = readable_subtasks(&snapshot, &subtasks);
        assert_eq!(readable.len(), 1);
        assert_eq!(readable[0].device_id, DEVICE_A);
    }

    #[test]
    fn assemble_detail_rejects_fully_invisible_main_task() {
        let snapshot = reader_snapshot();
        let row = main_row();
        let err = assemble_detail(&row, &snapshot, false, true, &[], &[]).unwrap_err();
        assert!(matches!(err, ControllerError::NotFound));
    }

    #[test]
    fn assemble_detail_uses_subset_counts_and_scope() {
        let snapshot = reader_snapshot();
        let row = main_row();
        let subtasks = vec![
            subtask(DEVICE_A, SubTaskState::Queued),
            subtask(DEVICE_A, SubTaskState::Succeeded),
            // 隐藏目标（B 不可见）：不得进 counts / visible_states
            subtask(DEVICE_B, SubTaskState::Dispatching),
        ];
        let visible = readable_subtasks(&snapshot, &subtasks);
        let detail =
            assemble_detail(&row, &snapshot, false, true, &visible, &subtasks).expect("detail");
        assert_eq!(detail.view_scope, ViewScope::AuthorizedSubset);
        assert_eq!(
            detail.visible_states,
            vec![SubTaskState::Queued, SubTaskState::Succeeded]
        );
        assert_eq!(detail.counts.queued, 1);
        assert_eq!(detail.counts.succeeded, 1);
        assert_eq!(detail.counts.dispatching, 0);
        assert_eq!(detail.revision, 9);
        assert_eq!(detail.outcome, None);
        assert_eq!(detail.created_time, evidence_fixture());
        // owner 只有 task.read、无 reboot（隐藏目标 B 也无 reboot）：不可取消
        assert!(!detail.actions.can_cancel);
    }

    #[test]
    fn assemble_detail_marks_full_scope_and_cancel_for_admin() {
        let snapshot = snapshot_with(true, vec![], vec![device_view(DEVICE_A, false)]);
        let row = main_row();
        let subtasks = vec![subtask(DEVICE_A, SubTaskState::Queued)];
        let visible = readable_subtasks(&snapshot, &subtasks);
        let detail =
            assemble_detail(&row, &snapshot, true, false, &visible, &subtasks).expect("detail");
        assert_eq!(detail.view_scope, ViewScope::Full);
        assert!(detail.actions.can_cancel);
    }

    #[test]
    fn assemble_detail_rejects_unparsable_created_time() {
        let snapshot = snapshot_with(true, vec![], vec![device_view(DEVICE_A, false)]);
        let mut row = main_row();
        row.created_time_json = "not-json".to_owned();
        let subtasks = vec![subtask(DEVICE_A, SubTaskState::Queued)];
        let visible = readable_subtasks(&snapshot, &subtasks);
        let err = assemble_detail(&row, &snapshot, true, false, &visible, &subtasks).unwrap_err();
        assert!(matches!(err, ControllerError::Database(_)));
    }

    #[test]
    fn can_cancel_requires_queued_or_held_and_permission() {
        let snapshot = reader_snapshot();
        let queued_a = subtask(DEVICE_A, SubTaskState::Queued);
        let held_a = subtask(DEVICE_A, SubTaskState::Held);
        let dispatching_a = subtask(DEVICE_A, SubTaskState::Dispatching);
        // 普通 owner 无 reboot → false
        assert!(!can_cancel_any(&snapshot, false, true, &[&queued_a]));
        // 有效 admin：queued/held → true；dispatching → false
        assert!(can_cancel_any(&snapshot, true, false, &[&queued_a]));
        assert!(can_cancel_any(&snapshot, true, false, &[&held_a]));
        assert!(!can_cancel_any(&snapshot, true, false, &[&dispatching_a]));
        // owner 且持有该设备 reboot → queued/held true
        let reboot_snapshot = snapshot_with(
            false,
            vec![direct_grant(
                DEVICE_A,
                vec![Permission::DeviceTaskRead, Permission::DeviceReboot],
            )],
            vec![device_view(DEVICE_A, false)],
        );
        assert!(can_cancel_any(&reboot_snapshot, false, true, &[&held_a]));
        // 非 owner 即使有 reboot 也不可取消
        assert!(!can_cancel_any(
            &reboot_snapshot,
            false,
            false,
            &[&queued_a]
        ));
    }

    #[test]
    fn task_cursor_round_trips_and_is_scope_bound() {
        let secret = [0x5Au8; 32];
        let id = Uuid::new_v4();
        let ms = 1_800_000_000_123i64;
        let cursor = encode_task_cursor(TaskCursorScope::Tasks, ms, id, &secret);
        let (decoded_ms, decoded_id) =
            decode_task_cursor(TaskCursorScope::Tasks, &cursor, &secret).expect("round trip");
        assert_eq!((decoded_ms, decoded_id), (ms, id));
        let created_at = keyset_from_millis(decoded_ms).expect("in-range millis");
        assert_eq!(
            created_at,
            fixed_time() + chrono::Duration::milliseconds(123)
        );
        // 跨端点复用与换密钥必失败（不回退首页）
        assert!(decode_task_cursor(TaskCursorScope::TaskChildren, &cursor, &secret).is_err());
        assert!(decode_task_cursor(TaskCursorScope::Tasks, &cursor, &[0x00u8; 32]).is_err());
        // 越界 millis 不可还原为 keyset 时间键
        assert!(keyset_from_millis(i64::MAX).is_none());
    }

    #[test]
    fn fallback_clock_is_system_fallback_with_stable_epoch() {
        let first = fallback_clock();
        let second = fallback_clock();
        // 如实标注不可信：下游 time_review 据此恒 fail-closed（Inconclusive）
        assert_eq!(first.quality, TimeQuality::SystemFallback);
        assert_eq!(first.clock_epoch, second.clock_epoch);
        assert!(first.sample_age_ms.is_none());
        assert!(first.offset_ms.is_none());
        assert!(first.uncertainty_ms.is_none());
        assert!(first.reference_utc <= second.reference_utc);
    }

    /// 固定证据的注入源替身：用于断言两个消费入口的前置段**原样透传**证据字段。
    struct FixedTimeSource {
        evidence: TimeEvidence,
    }

    impl TaskTimeSource for FixedTimeSource {
        fn current_evidence(&self) -> TimeEvidenceFuture<'_> {
            let evidence = self.evidence.clone();
            Box::pin(async move { evidence })
        }
    }

    fn injected_evidence() -> TimeEvidence {
        TimeEvidence {
            system_wall_utc: fixed_time(),
            reference_utc: fixed_time(),
            quality: TimeQuality::NtpValid,
            clock_epoch: Uuid::from_u128(0xE7),
            source: Some("fake.adapter-unit.local".to_owned()),
            sample_age_ms: Some(123),
            offset_ms: Some(-42),
            uncertainty_ms: Some(7),
        }
    }

    #[tokio::test]
    async fn prepared_clock_passes_every_evidence_field_through() {
        // 消费入口前置段绝不改写证据：逐字段等值（不抬质量、不抹 source/偏差/年龄）。
        let source = FixedTimeSource {
            evidence: injected_evidence(),
        };
        assert_eq!(
            prepare_time_review_clock(&source).await,
            injected_evidence()
        );
    }

    #[tokio::test]
    async fn system_fallback_source_is_the_untrusted_process_wall_clock() {
        // 兼容构造 `SqlxTaskApi::new` 的默认源：与 fallback_clock 同口径（不可信、
        // 纪元稳定、无来源/偏差/年龄），保持既有 Inconclusive 语义。
        let first = SystemFallbackTimeSource.current_evidence().await;
        let second = SystemFallbackTimeSource.current_evidence().await;
        assert_eq!(first.quality, TimeQuality::SystemFallback);
        assert_eq!(first.clock_epoch, fallback_clock().clock_epoch);
        assert_eq!(first.clock_epoch, second.clock_epoch);
        assert!(first.source.is_none());
        assert!(first.sample_age_ms.is_none());
        assert!(first.offset_ms.is_none());
        assert!(first.uncertainty_ms.is_none());
        assert!(first.reference_utc <= second.reference_utc);
    }

    #[tokio::test]
    async fn submit_request_binds_evidence_and_keeps_token_derived_digests() {
        // 幂等摘要/令牌派生值与重构前的内联写法完全同源（小写 hex 的往返等值）。
        let token = new_preview_token();
        let raw = preview_token_bytes(&token).expect("canonical token");
        let source = FixedTimeSource {
            evidence: injected_evidence(),
        };
        let request = prepare_submit_request(&source, &raw, "time-consumption-key")
            .await
            .expect("组装 SubmitRequest");
        assert_eq!(request.time_evidence, injected_evidence());
        assert_eq!(request.idempotency_key, "time-consumption-key");
        assert_eq!(
            request.request_hash,
            preview_request_hash(&token, &[], &[]),
            "hex::encode(raw) 必须与调用方原始令牌字符串逐字节一致"
        );
        assert_eq!(request.preview_token_hash, preview_token_digest(&raw));
        assert_eq!(request.preview_token_nonce, preview_token_nonce(&raw));
        assert_eq!(
            request.observe_deadline,
            fixed_time()
                + chrono::Duration::try_milliseconds(OBSERVE_WINDOW_MS).expect("window in range"),
            "观察窗口必须由同一份注入证据派生"
        );
    }

    #[test]
    fn submit_request_hash_binds_only_the_preview_token() {
        let base = preview_request_hash("token-a", &[], &[]);
        assert_eq!(base, preview_request_hash("token-a", &[], &[]));
        assert_ne!(base, preview_request_hash("token-b", &[], &[]));
    }

    #[test]
    fn main_task_outcome_type_is_carried_through_detail() {
        // 编译级钉：TaskDetail.outcome 与 MainTaskRow.outcome 同型（Option<MainTaskOutcome>）
        let snapshot = snapshot_with(true, vec![], vec![device_view(DEVICE_A, false)]);
        let mut row = main_row();
        row.outcome = Some(MainTaskOutcome::Partial);
        let subtasks = vec![subtask(DEVICE_A, SubTaskState::Succeeded)];
        let visible = readable_subtasks(&snapshot, &subtasks);
        let detail =
            assemble_detail(&row, &snapshot, true, false, &visible, &subtasks).expect("detail");
        assert_eq!(detail.outcome, Some(MainTaskOutcome::Partial));
    }

    #[test]
    fn can_cancel_covers_hidden_reboot_targets_without_leaking_counts() {
        // M1：A 仅 task.read（可见）、B 仅 reboot（隐藏）。can_cancel 按全量
        // 子任务逐目标独立判定 → true（cancel 实际会取消 B）；counts 与
        // visible_states 仍只含 A，绝不泄漏隐藏目标数量。
        let snapshot = snapshot_with(
            false,
            vec![
                direct_grant(DEVICE_A, vec![Permission::DeviceTaskRead]),
                direct_grant(DEVICE_B, vec![Permission::DeviceReboot]),
            ],
            vec![device_view(DEVICE_A, false), device_view(DEVICE_B, false)],
        );
        let row = main_row();
        let subtasks = vec![
            subtask(DEVICE_A, SubTaskState::Succeeded),
            subtask(DEVICE_B, SubTaskState::Queued),
        ];
        let visible = readable_subtasks(&snapshot, &subtasks);
        let detail =
            assemble_detail(&row, &snapshot, false, true, &visible, &subtasks).expect("detail");
        assert!(detail.actions.can_cancel);
        assert_eq!(detail.visible_states, vec![SubTaskState::Succeeded]);
        assert_eq!(detail.counts.succeeded, 1);
        assert_eq!(detail.counts.queued, 0);
        // 非 owner 时隐藏 reboot 目标同样不可取消（owner 门控不放宽）
        let not_owner =
            assemble_detail(&row, &snapshot, false, false, &visible, &subtasks).expect("detail");
        assert!(!not_owner.actions.can_cancel);
    }

    #[test]
    fn snapshot_owner_gate_rejects_stale_session_admin() {
        // M2：会话伪称 admin、快照 actor 已降级且非 owner → 404（与不存在同投影）。
        assert!(matches!(
            snapshot_owner_gate(false, false),
            Err(ControllerError::NotFound)
        ));
        assert!(snapshot_owner_gate(true, false).is_ok());
        assert!(snapshot_owner_gate(false, true).is_ok());
    }

    #[test]
    fn list_row_scope_requires_ownership_when_snapshot_not_admin() {
        // M2：权威快照非 admin 时，列表逐行丢弃他人发起的行（即使扫描域是全域）。
        let caller = Uuid::from_u128(0xC0);
        let other = Uuid::from_u128(0xFF);
        assert!(!list_row_in_scope(false, other, caller));
        assert!(list_row_in_scope(false, caller, caller));
        assert!(list_row_in_scope(true, other, caller));
    }

    #[test]
    fn children_gate_404s_when_no_target_readable() {
        // M3：owner 对全部目标都没有 device.task.read（全集不可见）→ 404；
        // 任一目标可读 → 不触发；有效 admin 不受该门控（与 detail 全域可见同口径）。
        let snapshot = snapshot_with(
            false,
            vec![direct_grant(DEVICE_B, vec![Permission::DeviceReboot])],
            vec![device_view(DEVICE_A, false), device_view(DEVICE_B, false)],
        );
        let subtasks = vec![
            subtask(DEVICE_A, SubTaskState::Queued),
            subtask(DEVICE_B, SubTaskState::Queued),
        ];
        assert_eq!(readable_subtasks(&snapshot, &subtasks).len(), 0);
        assert!(children_fully_invisible(false, 0));
        assert!(!children_fully_invisible(false, 1));
        assert!(!children_fully_invisible(true, 0));
    }

    #[test]
    fn merge_snapshot_unions_devices_grants_and_keeps_min_epoch() {
        // M4：分批设备清单的快照合并口径——grants/devices 取并集、epoch 取最小
        // （保守），合并后两批设备都能独立求值。
        let mut base = snapshot_with(
            false,
            vec![direct_grant(DEVICE_A, vec![Permission::DeviceTaskRead])],
            vec![device_view(DEVICE_A, false)],
        );
        base.authz_epoch = 7;
        let mut other = snapshot_with(
            false,
            vec![direct_grant(DEVICE_B, vec![Permission::DeviceReboot])],
            vec![device_view(DEVICE_B, false)],
        );
        other.authz_epoch = 5;
        merge_snapshot(&mut base, other);
        assert_eq!(base.authz_epoch, 5);
        assert_eq!(base.grants.len(), 2);
        assert!(
            snapshot_effective_permissions(&base, DEVICE_A).contains(&Permission::DeviceTaskRead)
        );
        assert!(
            snapshot_effective_permissions(&base, DEVICE_B).contains(&Permission::DeviceReboot)
        );
    }
}

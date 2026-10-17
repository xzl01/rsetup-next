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
//! 求值，也不得把「整批 has_any」类布尔当可见性判据。
//!
//! 时钟口径（RT-N5，fail-closed）：本适配器按简报固定结构不持有 `TimeGateHandle`；
//! `submit` 的创建时证据与 `time_review` 的比对时钟取进程内墙钟快照
//! （`SystemFallback`，纪元进程内稳定）。后果是确定且安全的：`time_review` 因
//! 「非 `NtpValid` 无法证明原期限有效」恒 `Inconclusive`（零状态变更、返回当前
//! 视图），绝不伪造 `NtpValid` 证据、绝不延长任何期限。真实可信时钟注入归顶层
//! 集成装配 owner（届时扩展构造入参即可，本模块纯函数不受影响）。
//!
//! 游标纪律：`/tasks` 与 `/tasks/{id}/children` 的下一页游标一律经
//! `encode_task_cursor`（HMAC 签名，密钥由本结构构造入参显式注入，HTTP 层不经手）
//! 签发；解码失败（篡改/跨端点/换密钥）一律 `InvalidArgument`，绝不回退首页。

use std::collections::{BTreeSet, HashMap};
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
    request_hash as preview_request_hash,
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

/// `can_cancel`：任一**可见**目标可取消即为 true（与 cancel 编排同一判据
/// `cancel_permitted`；隐藏目标不参与）。
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

/// 去重并升序（`[u8; 32]` 字节序）：`load_snapshot` 入参的稳定形状。
fn unique_device_ids(device_ids: impl Iterator<Item = [u8; 32]>) -> Vec<[u8; 32]> {
    device_ids.collect::<BTreeSet<_>>().into_iter().collect()
}

/// 组装 `TaskDetail`（详情与列表项共用；投影层再按 `visible_states` 重算
/// state/outcome/counts）。
///
/// - 可见全集为空 → `NotFound`（完全不可见与不存在统一 404，不输出零值汇总）；
/// - `view_scope` 仅当快照 actor 为有效 admin 时 `full`；
/// - `counts`/`can_cancel` 只按可见全集计算，隐藏目标不计入任何 counts。
pub(crate) fn assemble_detail(
    row: &MainTaskRow,
    snapshot: &AuthorizationSnapshot,
    admin_mode: bool,
    is_owner: bool,
    visible: &[&SubTaskRow],
) -> Result<TaskDetail, ControllerError> {
    let visible_states: Vec<SubTaskState> = visible.iter().map(|subtask| subtask.state).collect();
    if visible_states.is_empty() {
        return Err(ControllerError::NotFound);
    }
    let created_time =
        parse_created_time_evidence(&row.created_time_json).ok_or_else(task_row_decode_error)?;
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
            can_cancel: can_cancel_any(snapshot, admin_mode, is_owner, visible),
        },
        visible_states,
    })
}

impl SqlxTaskApi {
    /// 构造入参全部显式注入；`cursor_secret` 由装配方从环境链载入并校验
    /// （缺失/长度不符即装配失败），本类型不设默认密钥。
    pub fn new(
        service: Arc<TaskService<SqlxTaskRepository>>,
        repo: Arc<SqlxTaskRepository>,
        snapshots: Arc<SqlxAuthorizationSnapshotRepository>,
        cursor_secret: [u8; 32],
    ) -> Self {
        Self {
            service,
            repo,
            snapshots,
            cursor_secret,
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
        if device_ids.is_empty() {
            return Err(ControllerError::InvalidArgument);
        }
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
        let time_evidence = fallback_clock();
        let window = chrono::Duration::try_milliseconds(OBSERVE_WINDOW_MS)
            .ok_or_else(|| ControllerError::Config("observe window out of range".into()))?;
        let observe_deadline = time_evidence
            .reference_utc
            .checked_add_signed(window)
            .ok_or_else(|| ControllerError::Config("observe deadline out of range".into()))?;
        let request = SubmitRequest {
            idempotency_key: idempotency_key.to_owned(),
            // 请求体只携带预览令牌：幂等摘要绑定令牌本身，目标清单以服务端
            // 预览快照为唯一真相源（客户端永不携带目标/摘要/counts）。
            request_hash: preview_request_hash(preview_token, &[], &[]),
            preview_token_hash: preview_token_digest(&raw_token),
            preview_token_nonce: preview_token_nonce(&raw_token),
            observe_deadline,
            time_evidence,
        };
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
        // 权威 admin 判定以授权快照事务内重读的 actor 行为准（下方 `admin_mode`）：
        // 会话伪称 admin 而 DB 否定时，可见性求值自然为空集 → 整页被过滤，
        // fail-closed 且不泄漏任何存在性信号。
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
        let snapshot = self.load_snapshot(actor, &device_ids).await?;
        let admin_mode = is_current_admin(&snapshot.actor);
        let mut by_main: HashMap<Uuid, Vec<SubTaskRow>> = HashMap::new();
        for (main_id, subtask) in children {
            by_main.entry(main_id).or_default().push(subtask);
        }
        let mut items = Vec::new();
        for row in &scan.rows {
            let empty: Vec<SubTaskRow> = Vec::new();
            let visible = readable_subtasks(&snapshot, by_main.get(&row.id).unwrap_or(&empty));
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
            )?);
        }
        // 下一页游标 = 最后一条**已扫描候选**的 keyset 键（扫描满页才有）：
        // 不重扫、不死循环；游标经 HMAC 签名。
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
        // owner 身份先于设备可见性判定：非 owner 且非 admin → 404（与不存在同投影）。
        if !is_owner && !actor.user.is_admin {
            return Err(ControllerError::NotFound);
        }
        let device_ids = unique_device_ids(subtasks.iter().map(|subtask| subtask.device_id));
        let snapshot = self.load_snapshot(actor, &device_ids).await?;
        let admin_mode = is_current_admin(&snapshot.actor);
        let visible = readable_subtasks(&snapshot, &subtasks);
        assemble_detail(&row, &snapshot, admin_mode, is_owner, &visible)
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
        let row = self
            .repo
            .find_main_task(task_id)
            .await?
            .ok_or(ControllerError::NotFound)?;
        if row.actor_user_id != caller && !actor.user.is_admin {
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
        let device_ids = unique_device_ids(subtasks.iter().map(|subtask| subtask.device_id));
        let snapshot = self.load_snapshot(actor, &device_ids).await?;
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
                // 单目标业务性结果（终态/不可取消/逐行并发冲突/授权兜底）
                // 不整体失败；存储级故障必须上抛（503），不伪装成部分成功。
                Ok(_) | Err(CancelError::PermissionDenied) => {}
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
        let clock = fallback_clock();
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
        let err = assemble_detail(&row, &snapshot, false, true, &[]).unwrap_err();
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
        let detail = assemble_detail(&row, &snapshot, false, true, &visible).expect("detail");
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
        // owner 只有 task.read、无 reboot：不可取消
        assert!(!detail.actions.can_cancel);
    }

    #[test]
    fn assemble_detail_marks_full_scope_and_cancel_for_admin() {
        let snapshot = snapshot_with(true, vec![], vec![device_view(DEVICE_A, false)]);
        let row = main_row();
        let subtasks = vec![subtask(DEVICE_A, SubTaskState::Queued)];
        let visible = readable_subtasks(&snapshot, &subtasks);
        let detail = assemble_detail(&row, &snapshot, true, false, &visible).expect("detail");
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
        let err = assemble_detail(&row, &snapshot, true, false, &visible).unwrap_err();
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
        let detail = assemble_detail(&row, &snapshot, true, false, &visible).expect("detail");
        assert_eq!(detail.outcome, Some(MainTaskOutcome::Partial));
    }
}

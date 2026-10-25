//! 任务结果证据合并与状态机推进服务（04 §3、§4、§6）。
//!
//! 统一处理来自以下三条路径的任务结果：
//! 1. execute 同步返回的 TaskRecord；
//! 2. `device.task.result` 板端主动上报事件；
//! 3. 恢复/持续核查调用 `task_record` 查询获得的记录。
//!
//! 保证：
//! - 契约与绑定校验（sub_task_id、pre_boot_id、journal_epoch、record_version）；
//! - 状态机严格合法跃迁（不反向跃迁，queued/held -> accepted -> verifying ->
//!   succeeded/failed/unknown；`dispatching` 不得直跳 `succeeded`，`queued`/`held`
//!   不得直跳 `verifying`/`succeeded`/`unknown`）；
//! - `succeeded` 判定严格核验 04 §6 成功证据：必须携带 `boot_transition_observed`
//!   证据，且 `observed_boot_id` 存在并与 `pre_boot_id` 不同，缺一即拒；
//! - 板端 `attempted` 记录（03 §5 词表）按派发确认态 `accepted` 收敛；
//! - 终态以**物理删除**释放设备锁（与取消/过期/手工释放同一口径），但 `unknown`
//!   保留设备锁；
//! - 同一数据库短事务内重算主任务汇总（state, outcome, 十态 `counts_json`）并递增
//!   revision；
//! - 事务提交后向 EventBus 发布 `task_updated` 提示。

use chrono::Utc;
use rsetup_protocol::wire::{TaskRecord, validate_dto};
use serde_json::json;
use sqlx::Row as _;
use sqlx::mysql::MySqlArguments;
use sqlx::{MySql, Pool, Transaction};
use std::sync::Arc;
use uuid::Uuid;

use crate::ControllerError;
use crate::events::{EventBus, EventEnvelope, EventObjectAudience};
use crate::tasks::model::{ReasonCode, SubTaskState};
use crate::tasks::recovery::calculate_main_task_outcome;
use crate::tasks::repository::{
    column, enum_wire, fixed_mysql_bytes, mysql_uuid, task_row_decode_error,
};
use crate::tasks::service::counts_json_of;

/// 位置绑定：与 `tasks::params!` 宏完全同口径（编码失败 → `Database(Encode)` 503）。
/// 本模块的 SQL 全部定长，逐个位置绑定即可，不需要条件动态拼装。
fn push_arg<'q, T>(args: &mut MySqlArguments, value: &'q T) -> Result<(), ControllerError>
where
    T: sqlx::Type<MySql> + sqlx::Encode<'q, MySql>,
{
    sqlx::Arguments::add(args, value)
        .map_err(|err| ControllerError::Database(sqlx::Error::Encode(err)))
}

/// 板端记录词表（03 §5）中的**派发确认态**之一：意图已可靠落盘、OS 调用可能已开始。
/// 中控十态无对应项，证据侧与 `accepted` 同义收敛；派发侧同一口径见
/// [`crate::tasks::scheduler::DISPATCH_ACCEPT_STATES`]。
const BOARD_STATE_ATTEMPTED: &str = "attempted";

/// 04 §6 成功证据词表（03 §5，与 `rsetup_protocol::wire::EVIDENCE_KINDS` 同一词表）
/// 中唯一的**可靠启动切换证据**标识：`succeeded` 必须携带该证据，且
/// `observed_boot_id` 与 `pre_boot_id` 不同，否则不得判成功、不得释放设备锁。
const EVIDENCE_KIND_BOOT_TRANSITION: &str = "boot_transition_observed";

/// 状态跃迁判定裁决
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionVerdict {
    /// 接受并应用该转移
    Apply {
        target_state: SubTaskState,
        reason_code: Option<ReasonCode>,
        release_lock: bool,
    },
    /// 收到旧版本记录（已落库版本 >= 到来版本），安全忽略
    RejectStaleVersion { current: u64, incoming: u64 },
    /// 证据内容或绑定不符（错任务、错 boot 等）
    RejectMismatch { reason: &'static str },
    /// 非法状态跃迁（例如终态反向变为进行中）
    InvalidStateTransition {
        from: SubTaskState,
        to: SubTaskState,
    },
}

/// 纯函数：根据当前任务状态与到来的 TaskRecord 计算跃迁裁决
pub fn evaluate_record_transition(
    expected_subtask_id: Uuid,
    expected_pre_boot_id: &str,
    current_state: SubTaskState,
    current_record_version: u64,
    record: &TaskRecord,
) -> TransitionVerdict {
    if validate_dto(record).is_err() {
        return TransitionVerdict::RejectMismatch {
            reason: "dto_invalid",
        };
    }
    let Ok(record_sub) = Uuid::parse_str(&record.sub_task_id) else {
        return TransitionVerdict::RejectMismatch {
            reason: "invalid_subtask_id",
        };
    };
    if record_sub != expected_subtask_id {
        return TransitionVerdict::RejectMismatch {
            reason: "subtask_id_mismatch",
        };
    }
    if record.pre_boot_id != expected_pre_boot_id {
        return TransitionVerdict::RejectMismatch {
            reason: "pre_boot_id_mismatch",
        };
    }
    if Uuid::parse_str(&record.journal_epoch)
        .map(|e| e.is_nil())
        .unwrap_or(true)
    {
        return TransitionVerdict::RejectMismatch {
            reason: "journal_epoch_nil",
        };
    }

    // 版本检查：必须严格递增
    if record.record_version <= current_record_version {
        return TransitionVerdict::RejectStaleVersion {
            current: current_record_version,
            incoming: record.record_version,
        };
    }

    // 解析目标状态。板端 TaskRecord 的 state 词表（03 §5）是
    // {accepted, attempted, succeeded, failed, unknown}：`attempted` 表示意图已可靠
    // 落盘、OS 调用可能已开始，与 `accepted` 同属**派发确认态**（口径同
    // `scheduler::DISPATCH_ACCEPT_STATES`）。中控十态没有 `attempted` 对应项，因此
    // 它不是非法状态，一律映射为 `accepted` 收敛，不得报 `unknown_state_enum`。
    let target_state = if record.state == BOARD_STATE_ATTEMPTED {
        SubTaskState::Accepted
    } else {
        let Some(state) = SubTaskState::from_str_exact(&record.state) else {
            return TransitionVerdict::RejectMismatch {
                reason: "unknown_state_enum",
            };
        };
        state
    };

    // 状态机合法跃迁校验（04 §3「合法转移」，只允许前进、不反向）。记录驱动的跃迁
    // 口径逐条对齐 04 §3：
    // - `dispatching → accepted/verifying/failed/unknown`，**不允许直跳 succeeded**：
    //   首次拿到的记录即使已是终态，也必须先经 accepted 再到 verifying 才转终态；
    // - `queued`/`held` 是派发前状态，记录证据最多确认到派发确认态或失败，
    //   不得直跳 `verifying`/`succeeded`/`unknown`；
    // - `accepted → verifying/succeeded/failed/unknown`、`verifying → 终态`；同态
    //   记录（record_version 已严格递增）按证据刷新放行；
    // - `unknown` 只由后续有效证据收敛为 succeeded/failed，不改回执行中；
    // - succeeded/failed/cancelled/expired 是批次终态，不可反向变为任何状态，矛盾的
    //   迟到结果由一致性告警承载，绝不静默覆盖。
    let valid_transition = match (current_state, target_state) {
        // 派发前：只接受派发确认与失败
        (
            SubTaskState::Queued | SubTaskState::Held,
            SubTaskState::Accepted | SubTaskState::Failed,
        ) => true,

        // 派发中：确认/核验中/失败/未知，不得直跳 succeeded
        (
            SubTaskState::Dispatching,
            SubTaskState::Accepted
            | SubTaskState::Verifying
            | SubTaskState::Failed
            | SubTaskState::Unknown,
        ) => true,

        (
            SubTaskState::Accepted,
            SubTaskState::Accepted
            | SubTaskState::Verifying
            | SubTaskState::Succeeded
            | SubTaskState::Failed
            | SubTaskState::Unknown,
        ) => true,

        (
            SubTaskState::Verifying,
            SubTaskState::Verifying
            | SubTaskState::Succeeded
            | SubTaskState::Failed
            | SubTaskState::Unknown,
        ) => true,

        (SubTaskState::Unknown, SubTaskState::Succeeded | SubTaskState::Failed) => true,

        _ => false,
    };

    if !valid_transition {
        return TransitionVerdict::InvalidStateTransition {
            from: current_state,
            to: target_state,
        };
    }

    // 04 §6：`succeeded` 不是"记录里出现了 succeeded 字样"就成立，必须同时满足——
    // 1. `observed_boot_id` 存在且非空（缺失即拒绝）；
    // 2. `evidence_kind` 为可靠启动切换证据 `boot_transition_observed`（缺失即拒绝）；
    // 3. `observed_boot_id` 与 `pre_boot_id` 不同（boot 未切换即拒绝）。
    // 该校验必须放在合法跃迁判定**之后**：`dispatching -> succeeded` 这类非法跃迁
    // 仍优先返回 `InvalidStateTransition`，不被证据缺失误报为内容不符。记录已在前
    // 方核验 `record.pre_boot_id == expected_pre_boot_id`，此处对两者都比对，防御
    // 纯函数被其他调用方直接复用。
    if target_state == SubTaskState::Succeeded {
        let Some(observed_boot_id) = record
            .observed_boot_id
            .as_deref()
            .filter(|observed| !observed.is_empty())
        else {
            return TransitionVerdict::RejectMismatch {
                reason: "missing_observed_boot_id",
            };
        };
        if record.evidence_kind.as_deref() != Some(EVIDENCE_KIND_BOOT_TRANSITION) {
            return TransitionVerdict::RejectMismatch {
                reason: "missing_boot_transition_evidence",
            };
        }
        if observed_boot_id == expected_pre_boot_id || observed_boot_id == record.pre_boot_id {
            return TransitionVerdict::RejectMismatch {
                reason: "boot_id_unchanged",
            };
        }
    }

    let parsed_reason = record
        .reason_code
        .as_deref()
        .and_then(ReasonCode::from_str_exact);

    // 04 §3：succeeded, failed, cancelled, expired 释放设备锁；unknown 保留设备锁；进行中态持有锁
    let release_lock = matches!(
        target_state,
        SubTaskState::Succeeded
            | SubTaskState::Failed
            | SubTaskState::Cancelled
            | SubTaskState::Expired
    );

    TransitionVerdict::Apply {
        target_state,
        reason_code: parsed_reason,
        release_lock,
    }
}

/// 纯函数：由主任务下子任务状态全集重算主任务的持久汇总三元组
/// （`state` / `outcome` / 十态 `counts_json`），与主任务行的 `revision + 1` 同条
/// UPDATE 落库。
///
/// `counts_json` 走 `service::counts_json_of` 这一**唯一生产者**，与提交、取消、
/// 时间核验路径同口径（十键固定顺序、值为十进制字符串，未出现的状态计 0）；
/// `outcome` 在主任务未收敛时为 `None`（落库 NULL）。
pub fn recalculate_main_summary(
    states: &[SubTaskState],
) -> Result<(String, Option<String>, String), ControllerError> {
    let (main_state, main_outcome) = calculate_main_task_outcome(states);
    let counts_pairs: Vec<(SubTaskState, u64)> =
        states.iter().copied().map(|state| (state, 1_u64)).collect();
    Ok((
        enum_wire(&main_state)?,
        match main_outcome {
            Some(outcome) => Some(enum_wire(&outcome)?),
            None => None,
        },
        counts_json_of(&counts_pairs),
    ))
}

/// 证据应用结果
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyOutcome {
    Applied {
        subtask_id: Uuid,
        new_state: SubTaskState,
    },
    IgnoredStale,
    RejectedMismatch,
    NotFound,
}

/// 证据持久化应用器接口
pub trait TaskEvidenceApplier: Send + Sync {
    fn apply_task_record(
        &self,
        device_id: &[u8; 32],
        record: &TaskRecord,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ApplyOutcome, ControllerError>> + Send + '_>,
    >;
}

/// 基于 SQLx 的生产任务证据应用服务
pub struct SqlxTaskEvidenceApplier {
    pool: Pool<MySql>,
    events: Option<Arc<EventBus>>,
}

impl SqlxTaskEvidenceApplier {
    pub fn new(pool: Pool<MySql>, events: Option<Arc<EventBus>>) -> Self {
        Self { pool, events }
    }
}

impl TaskEvidenceApplier for SqlxTaskEvidenceApplier {
    fn apply_task_record(
        &self,
        device_id: &[u8; 32],
        record: &TaskRecord,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ApplyOutcome, ControllerError>> + Send + '_>,
    > {
        let pool = self.pool.clone();
        let events = self.events.clone();
        let dev_id = *device_id;
        let rec = record.clone();

        Box::pin(async move {
            let record_sub_id = match Uuid::parse_str(&rec.sub_task_id) {
                Ok(id) => id,
                Err(_) => return Ok(ApplyOutcome::RejectedMismatch),
            };

            let mut tx: Transaction<'_, MySql> = pool.begin().await?;

            // 1. 查询并锁住子任务行（`lock_generation` 是本子任务持有设备锁的代际，
            //    释放锁时必须按代际 CAS，绝不触碰该设备后续任务的新锁）
            let select_sql = "SELECT id, main_task_id, device_id, state, reason_code, pre_boot_id, \
                              revision, lock_generation, \
                              JSON_EXTRACT(evidence_json, '$.record_version') AS curr_rec_ver \
                              FROM sub_tasks WHERE id = ? FOR UPDATE";
            let mut args = MySqlArguments::default();
            push_arg(&mut args, &record_sub_id.as_bytes().as_slice())?;

            let rows = sqlx::query_with(select_sql, args)
                .fetch_all(&mut *tx)
                .await?;
            if rows.is_empty() {
                tx.rollback().await.ok();
                return Ok(ApplyOutcome::NotFound);
            }
            let row = &rows[0];

            // 1.1 结果写入必须绑定所属设备：认证公钥与任务归属设备不一致时，
            //     说明该设备上报了不属于自己的子任务 ID，回滚并以错配拒绝。
            let task_dev_id = fixed_mysql_bytes::<32>(&column::<Vec<u8>>(row, "device_id")?)?;
            if task_dev_id != dev_id {
                tx.rollback().await.ok();
                return Ok(ApplyOutcome::RejectedMismatch);
            }

            let main_task_id = mysql_uuid(
                &row.try_get::<Vec<u8>, _>("main_task_id")
                    .map_err(|_| task_row_decode_error())?,
            )?;
            let current_state_str: String =
                row.try_get("state").map_err(|_| task_row_decode_error())?;
            let current_state = SubTaskState::from_str_exact(&current_state_str)
                .ok_or_else(task_row_decode_error)?;
            let current_pre_boot: Option<String> = row
                .try_get("pre_boot_id")
                .map_err(|_| task_row_decode_error())?;
            let pre_boot_id_str = current_pre_boot.unwrap_or_default();
            // revision 为 BIGINT UNSIGNED，按 u64 严格解码（与 lock_generation 同一
            // 口径）；i64 解码会被 SQLx MySQL 驱动以类型不兼容拒绝（503）。
            let revision: u64 = column(row, "revision")?;
            // lock_generation 为 BIGINT UNSIGNED，按 u64 严格解码（与 service 释放
            // 路径同一口径）；未持锁时为 0，DELETE 按代际自然不命中任何行。
            let lock_generation: u64 = column(row, "lock_generation")?;

            let curr_rec_ver: u64 = row
                .try_get::<Option<i64>, _>("curr_rec_ver")
                .map_err(|_| task_row_decode_error())?
                .and_then(|v| u64::try_from(v).ok())
                .unwrap_or(0);

            // 2. 纯逻辑判定
            let verdict = evaluate_record_transition(
                record_sub_id,
                &pre_boot_id_str,
                current_state,
                curr_rec_ver,
                &rec,
            );

            let (target_state, reason_code, release_lock) = match verdict {
                TransitionVerdict::Apply {
                    target_state,
                    reason_code,
                    release_lock,
                } => (target_state, reason_code, release_lock),
                TransitionVerdict::RejectStaleVersion { .. } => {
                    tx.rollback().await.ok();
                    return Ok(ApplyOutcome::IgnoredStale);
                }
                TransitionVerdict::RejectMismatch { .. }
                | TransitionVerdict::InvalidStateTransition { .. } => {
                    tx.rollback().await.ok();
                    return Ok(ApplyOutcome::RejectedMismatch);
                }
            };

            // 3. 更新子任务行：evidence_json 存储 TaskRecord 结构化 JSON
            let evidence_json_val = json!({
                "schema_version": rec.schema_version,
                "sub_task_id": rec.sub_task_id,
                "journal_epoch": rec.journal_epoch,
                "record_version": rec.record_version,
                "state": rec.state,
                "pre_boot_id": rec.pre_boot_id,
                "observed_boot_id": rec.observed_boot_id,
                "reason_code": rec.reason_code,
                "updated_unix_ms": rec.updated_unix_ms,
                "evidence_kind": rec.evidence_kind,
            });
            let evidence_json_str = evidence_json_val.to_string();
            let new_state_str = enum_wire(&target_state)?;
            let reason_str = match reason_code {
                Some(r) => Some(enum_wire(&r)?),
                None => None,
            };
            let now = Utc::now().naive_utc();

            let update_sub_sql = "UPDATE sub_tasks SET state = ?, reason_code = ?, evidence_json = ?, \
                                  revision = revision + 1, updated_at = ? WHERE id = ?";
            let mut up_args = MySqlArguments::default();
            push_arg(&mut up_args, &new_state_str)?;
            push_arg(&mut up_args, &reason_str)?;
            push_arg(&mut up_args, &evidence_json_str)?;
            push_arg(&mut up_args, &now)?;
            push_arg(&mut up_args, &record_sub_id.as_bytes().as_slice())?;

            sqlx::query_with(update_sub_sql, up_args)
                .execute(&mut *tx)
                .await?;

            // 4. 处理设备锁：终态释放，unknown 保留。
            //
            // 释放口径与取消/过期/手工释放**完全一致**：物理 `DELETE`，不是把
            // `released_at` 置值。`acquire_device_lock` 的 CAS 以
            // `WHERE NOT EXISTS (SELECT 1 FROM device_operation_locks WHERE device_id = ?)`
            // 判空，残留行会让该设备**永久判忙**（锁死）。按
            // `device_id + owner_sub_task_id + generation` 三条件删除，只回收本
            // 子任务本代际的锁，绝不触碰该设备后续任务的新 generation。
            if release_lock {
                let release_sql = "DELETE FROM device_operation_locks \
                                   WHERE device_id = ? AND owner_sub_task_id = ? AND generation = ?";
                let mut lock_args = MySqlArguments::default();
                push_arg(&mut lock_args, &dev_id.as_slice())?;
                push_arg(&mut lock_args, &record_sub_id.as_bytes().as_slice())?;
                push_arg(&mut lock_args, &lock_generation)?;
                sqlx::query_with(release_sql, lock_args)
                    .execute(&mut *tx)
                    .await?;
            }

            // 5. 联查主任务下所有子任务，重算并更新主任务汇总与 revision。
            //    state/outcome/counts_json 必须与 revision + 1 同条 UPDATE 落库
            //    （RT-R8-3）：只递增 revision 会让主行 counts 永远停留在提交时的口径。
            let all_subs_sql = "SELECT state FROM sub_tasks WHERE main_task_id = ? FOR UPDATE";
            let mut sub_args = MySqlArguments::default();
            push_arg(&mut sub_args, &main_task_id.as_bytes().as_slice())?;
            let sub_rows = sqlx::query_with(all_subs_sql, sub_args)
                .fetch_all(&mut *tx)
                .await?;

            let mut all_states = Vec::with_capacity(sub_rows.len());
            for r in &sub_rows {
                let s_str: String = r.try_get("state").map_err(|_| task_row_decode_error())?;
                let s = SubTaskState::from_str_exact(&s_str).ok_or_else(task_row_decode_error)?;
                all_states.push(s);
            }

            let (main_state_str, main_outcome_str, counts_json) =
                recalculate_main_summary(&all_states)?;

            let update_main_sql = "UPDATE main_tasks SET state = ?, outcome = ?, counts_json = ?, \
                                   revision = revision + 1, updated_at = ? WHERE id = ?";
            let mut main_up_args = MySqlArguments::default();
            push_arg(&mut main_up_args, &main_state_str)?;
            push_arg(&mut main_up_args, &main_outcome_str)?;
            push_arg(&mut main_up_args, &counts_json)?;
            push_arg(&mut main_up_args, &now)?;
            push_arg(&mut main_up_args, &main_task_id.as_bytes().as_slice())?;
            sqlx::query_with(update_main_sql, main_up_args)
                .execute(&mut *tx)
                .await?;

            // 6. 提交事务
            tx.commit().await?;

            // 7. 提交后向 EventBus 发布 task_updated (发布给全局管理员受众)
            if let Some(bus) = events {
                bus.publish(EventEnvelope::task_updated(
                    &main_task_id.to_string(),
                    revision.saturating_add(1),
                    EventObjectAudience::admins(),
                ));
            }

            Ok(ApplyOutcome::Applied {
                subtask_id: record_sub_id,
                new_state: target_state,
            })
        })
    }
}

//! 只读恢复分类、主任务结局聚合纯逻辑与只读恢复编排（04 §4、§5.2、§6）。
//!
//! 纯函数切片（`classify_recovery`/`success_evidence_satisfied`/
//! `calculate_main_task_outcome`）不依赖 `rsetup_protocol`、不连数据库、不调用
//! 板端。[`recover_process`] 为 wire DTO 门禁通过后的 typed 编排切片，严格三阶段
//! 执行，任何板端 `.await` 前后都不存在未提交的数据库事务：
//! 1. **只读短事务**：经 [`TaskRepository::begin`] 全量重建非终态未决集后立即提交，
//!    释放连接与锁；
//! 2. **事务外零锁网络核验**：对可能已发送的状态**只**调用
//!    [`BoardClient::task_record`]，在纯内存中收集 `recovery_checks` 待插入项与
//!    `queued → held` 待更新项（对 `queued` 按不可信期限保守分类为 `MarkHeld`）；
//! 3. **写入短事务**：批量插入 `recovery_checks` 并条件更新 `held` 后立即提交，
//!    事务内零网络调用。
//!
//! 恢复路径**永不**产生发送动作：实现中不存在任何 `prepare`/`execute` 调用点，
//! `RecoverySummary::execute_calls` 恒为 0。

use crate::ControllerError;
use crate::tasks::board_client::{BoardClient, BoardClientError};
use crate::tasks::model::{
    EvidenceKind, MainTaskOutcome, MainTaskState, SubTaskState, TaskEvidence,
};
use crate::tasks::repository::{
    TaskRepository, column, fixed_mysql_bytes, mysql_uuid, task_row_decode_error, tx_execute,
    tx_rows,
};
use sqlx::mysql::MySqlArguments;
use uuid::Uuid;

/// 恢复动作：只读核验 / 置 held / 置 expired。三者都不含任何发送语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    ReadOnlyVerify { subtask_id: Uuid },
    MarkHeld { subtask_id: Uuid },
    MarkExpired { subtask_id: Uuid },
}

/// 成功核验的基线/历史比较输入（字段语义与缺省口径见计划 04 Task 5 Interfaces，
/// 桩与实现共用该定义）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvidenceBaseline {
    pub journal_epoch: Uuid,
    pub record_version: u64,
    pub confirmed_failed_versions: Vec<u64>,
}

/// 恢复分类（04 §5.2 只读恢复）。调用方先判定 `sub_tasks.queue_deadline` 的可信性：
/// 不可信时固定传 `queue_deadline_trustworthy = false` 且 `queue_deadline_expired = false`
/// （本函数忽略 `expired`，使“可信且已过期 → `MarkExpired`”分支可真实产生）；
/// 可信时以队列期限对照可信当前时刻求得 `queue_deadline_expired`。
///
/// 分支表：
/// - `Queued`：期限不可信 → `MarkHeld`；可信且已过期 → `MarkExpired`；可信且未过期
///   → `ReadOnlyVerify`（重新核验后由调度器决定，**不直接发送**）；
/// - `Held | Dispatching | Accepted | Verifying | Unknown`（存活进行中组，`Held` 是存活态）
///   → `ReadOnlyVerify`，**永不**产生发送动作；
/// - 终态 → `ReadOnlyVerify`（幂等，不做任何写）。
///
/// 纯函数不持有子任务 id，返回的 `subtask_id` 恒为 `Uuid::nil()` 占位，由调用方以
/// 实际 `sub_tasks.id` 填充后再落库。
pub fn classify_recovery(
    state: SubTaskState,
    queue_deadline_trustworthy: bool,
    queue_deadline_expired: bool,
) -> RecoveryAction {
    match state {
        SubTaskState::Queued => {
            if !queue_deadline_trustworthy {
                RecoveryAction::MarkHeld {
                    subtask_id: Uuid::nil(),
                }
            } else if queue_deadline_expired {
                RecoveryAction::MarkExpired {
                    subtask_id: Uuid::nil(),
                }
            } else {
                RecoveryAction::ReadOnlyVerify {
                    subtask_id: Uuid::nil(),
                }
            }
        }
        // 存活进行中组：Held 是存活态，并入本组；只读核验，绝不重发
        SubTaskState::Held
        | SubTaskState::Dispatching
        | SubTaskState::Accepted
        | SubTaskState::Verifying
        | SubTaskState::Unknown => RecoveryAction::ReadOnlyVerify {
            subtask_id: Uuid::nil(),
        },
        // 终态组：幂等只读，不做任何写
        SubTaskState::Succeeded
        | SubTaskState::Failed
        | SubTaskState::Cancelled
        | SubTaskState::Expired => RecoveryAction::ReadOnlyVerify {
            subtask_id: Uuid::nil(),
        },
    }
}

/// 成功证据核验（04 §6 全部条件，逐条 AND，任一不满足即 `false`）。
///
/// `expected_pre_boot` 由调用方从数据库 `sub_tasks.pre_boot_id` 读取传入（不作
/// `TaskEvidence` 字段）；`baseline` 由调用方从该子任务已持久化记录构造传入
/// （无已持久化记录时：`journal_epoch` 取 DB 读到的当前 epoch、`record_version` 取 0、
/// `confirmed_failed_versions` 取空集合）。
///
/// 仅 boot 变化、断线、Pong、`TASK_NOT_FOUND`、跨 epoch、版本倒退、矛盾失败记录
/// 一律不判成功。
pub fn success_evidence_satisfied(
    expected_pre_boot: &str,
    ev: &TaskEvidence,
    baseline: &EvidenceBaseline,
) -> bool {
    ev.reliable_attempted
        && ev.pre_boot_id == expected_pre_boot
        && ev.current_boot_id != ev.pre_boot_id
        && ev.current_boot_id == ev.observed_boot_id
        && ev.evidence_kind == EvidenceKind::BootTransitionObserved
        && ev.journal_epoch == baseline.journal_epoch
        && ev.record_version > baseline.record_version
        && baseline.confirmed_failed_versions.is_empty()
}

/// 主任务状态与结局聚合（04 §4 优先级矩阵，自上而下短路）：
/// 1. 任一 `Dispatching | Accepted | Verifying`，或“批次已开始且仍有 `Queued`”
///    → `(Running, None)`；
/// 2. 无执行中且全 `Queued` → `(Queued, None)`；
/// 3. 无执行中、无 `Queued`、至少一个 `Held` → `(Held, None)`；
/// 4. 全部终态 → `(Completed, Some(outcome))`，其中含 `Unknown` → `Unknown`；
///    全 `Succeeded` → `Success`；全 `Cancelled` → `Cancelled`；
///    有成功也有非成功 → `Partial`；其余 → `Failed`。
///
/// 空切片按“全 `Queued`”的 `all` 语义退化为 `(Queued, None)`（批次无子任务不是
/// 合法输入，由上层校验）。
pub fn calculate_main_task_outcome(
    states: &[SubTaskState],
) -> (MainTaskState, Option<MainTaskOutcome>) {
    let executing = states.iter().any(|s| {
        matches!(
            s,
            SubTaskState::Dispatching | SubTaskState::Accepted | SubTaskState::Verifying
        )
    });
    let queued = states.contains(&SubTaskState::Queued);
    let started = states.iter().any(|s| *s != SubTaskState::Queued);
    if executing || (started && queued) {
        return (MainTaskState::Running, None);
    }
    if states.iter().all(|s| *s == SubTaskState::Queued) {
        return (MainTaskState::Queued, None);
    }
    if states.contains(&SubTaskState::Held) {
        return (MainTaskState::Held, None);
    }
    let outcome = if states.contains(&SubTaskState::Unknown) {
        MainTaskOutcome::Unknown
    } else if states.iter().all(|s| *s == SubTaskState::Succeeded) {
        MainTaskOutcome::Success
    } else if states.iter().all(|s| *s == SubTaskState::Cancelled) {
        MainTaskOutcome::Cancelled
    } else if states.contains(&SubTaskState::Succeeded) {
        MainTaskOutcome::Partial
    } else {
        MainTaskOutcome::Failed
    };
    (MainTaskState::Completed, Some(outcome))
}

/// 只读恢复一页的结果统计。
///
/// `execute_calls` **恒为 0**：本模块不存在任何 `prepare`/`execute` 调用点，
/// 该字段是供调用方与测试做计数断言的哨兵，而非真实计数来源。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoverySummary {
    /// 已执行只读核验（`task_record` 查询成功返回，含 `Ok(None)`）的子任务数。
    pub read_only_verified: usize,
    /// 分类为 `MarkHeld` 的子任务数（本切片只统计分类结果，落库属集成接线）。
    pub marked_held: usize,
    /// 分类为 `MarkExpired` 的子任务数（本切片无可信时钟输入，恒为 0）。
    pub marked_expired: usize,
    /// 发送调用计数：恒为 0（恢复路径永不发送）。
    pub execute_calls: usize,
}

/// 恢复期读取的子任务行（只读投影；`device_id` 即 [`BoardClient`] 的设备键）。
#[derive(Clone, Debug)]
struct RecoveryPendingRow {
    subtask_id: Uuid,
    device_id: [u8; 32],
    state: SubTaskState,
}

/// 未决集读取 SQL（非终态全量查询）：筛选所有仍在进行中或排队中的未决行。
/// 包含旧代际崩溃前残留的任务与未分配 epoch（NULL）的 queued 任务，确保全面恢复；
/// 终态行不产生任何恢复动作，不读取。
const RECOVERY_PENDING_SQL: &str = "SELECT id, device_id, state FROM sub_tasks \
                                    WHERE state IN ('dispatching', 'accepted', 'verifying', \
                                                  'unknown', 'queued') \
                                    ORDER BY id ASC";

/// 恢复检查结果插入 SQL（去敏详情持久化至 recovery_checks）。
const INSERT_RECOVERY_CHECK_SQL: &str = "INSERT INTO recovery_checks (id, process_epoch, subtask_id, outcome, detail_redacted, checked_at) \
     VALUES (?, ?, ?, ?, ?, NOW(6))";

/// Queued 恢复分类更新 SQL（MarkHeld 分类持久化更新为 held 状态）。
const UPDATE_QUEUED_HELD_SQL: &str =
    "UPDATE sub_tasks SET state = 'held', updated_at = NOW(6) WHERE id = ? AND state = 'queued'";

/// 板端核验 outcome 与去敏详情。
struct TaskCheckResult {
    outcome: &'static str,
    detail_redacted: String,
    verified: bool,
}

/// 对单个待核验子任务调用板端 task_record，返回去敏持久化数据。
/// 仅在查询成功（Ok(Some)/Ok(None)）时算已核验（None 即 TASK_NOT_FOUND(1006)）；
/// Timeout/Transport 等错误记录对应错误 outcome，但不计入 read_only_verified。
async fn query_board_task_record<C: BoardClient>(
    board_client: &C,
    device_id: &[u8; 32],
    subtask_id: Uuid,
) -> TaskCheckResult {
    match board_client.task_record(device_id, subtask_id).await {
        Ok(Some(_)) => TaskCheckResult {
            outcome: "found",
            detail_redacted: "{\"verified\":true}".to_owned(),
            verified: true,
        },
        Ok(None) => TaskCheckResult {
            outcome: "not_found",
            detail_redacted: "{\"verified\":true}".to_owned(),
            verified: true,
        },
        Err(BoardClientError::Timeout) => TaskCheckResult {
            outcome: "timeout",
            detail_redacted: "{\"verified\":false,\"error\":\"timeout\"}".to_owned(),
            verified: false,
        },
        Err(BoardClientError::Transport) => TaskCheckResult {
            outcome: "transport",
            detail_redacted: "{\"verified\":false,\"error\":\"transport\"}".to_owned(),
            verified: false,
        },
        Err(_) => TaskCheckResult {
            outcome: "error",
            detail_redacted: "{\"verified\":false,\"error\":\"other\"}".to_owned(),
            verified: false,
        },
    }
}

/// 恢复模块内部的位置参数绑定宏（与 tasks/mod.rs 的 params! 同口径）。
macro_rules! recovery_params {
    ($($e:expr),* $(,)?) => {{
        let mut args = sqlx::mysql::MySqlArguments::default();
        $(
            sqlx::Arguments::add(&mut args, &$e)
                .map_err(|err| crate::ControllerError::Database(sqlx::Error::Encode(err)))?;
        )*
        args
    }};
}

/// 进程重启后的只读恢复编排（04 §5.2、04 Task 5 Step 3.6，F1-F3 修复 +
/// 修复轮 2 读写短事务拆分）：
///
/// 1. **阶段 1 只读短事务**：经 [`TaskRepository::begin`] 开启事务，使用全量非终态
///    SQL (`RECOVERY_PENDING_SQL`) 重建未决集（涵盖旧代际意图与 NULL epoch 的 queued
///    任务）后立即提交，释放连接与锁；
/// 2. **阶段 2 事务外零锁网络核验**：对可能已发送状态
///    `Dispatching | Accepted | Verifying | Unknown` **只**调用
///    [`BoardClient::task_record`] 取得去敏核验结果，在纯内存中收集
///    `recovery_checks` 待插入项与 `queued → held` 待更新项；此期间不存在任何
///    打开的数据库事务，网络等待不持有任何行锁；
/// 3. **阶段 3 写入短事务**：仅当存在待写入项时重新开启事务，纯内存批量把去敏核验
///    结果 `INSERT INTO recovery_checks`（`id, process_epoch, subtask_id, outcome,
///    detail_redacted, checked_at`），并对 [`classify_recovery`] 分类为 `MarkHeld`
///    的 `Queued` 执行
///    `UPDATE sub_tasks SET state = 'held', updated_at = NOW(6) WHERE id = ? AND state = 'queued'`，
///    随后立即提交，事务内零网络调用；
/// 4. 硬约束：恢复流程**永不**产生发送动作，`RecoverySummary::execute_calls` 恒为 0。
pub async fn recover_process<R: TaskRepository, C: BoardClient>(
    repo: &R,
    board_client: &C,
    process_epoch: Uuid,
) -> Result<RecoverySummary, ControllerError> {
    // 阶段 1：只读短事务——读取未决集后立即提交，释放连接与锁。
    let mut read_tx = repo.begin().await?;
    let rows = tx_rows(
        &mut read_tx,
        RECOVERY_PENDING_SQL,
        MySqlArguments::default(),
    )
    .await?;
    let mut pending = Vec::with_capacity(rows.len());
    for row in &rows {
        pending.push(RecoveryPendingRow {
            subtask_id: mysql_uuid(&column::<Vec<u8>>(row, "id")?)?,
            device_id: fixed_mysql_bytes::<32>(&column::<Vec<u8>>(row, "device_id")?)?,
            state: SubTaskState::from_str_exact(&column::<String>(row, "state")?)
                .ok_or_else(task_row_decode_error)?,
        });
    }
    read_tx.commit().await?;

    // 阶段 2：事务外零锁网络核验——此刻不存在任何打开的数据库事务，
    // 板端 `.await` 只等待网络，不持有数据库连接或行锁。
    let mut summary = RecoverySummary::default();
    let mut checks_to_insert: Vec<(Uuid, [u8; 16], &'static str, String)> = Vec::new();
    let mut held_to_update: Vec<Uuid> = Vec::new();

    for row in &pending {
        match row.state {
            SubTaskState::Dispatching
            | SubTaskState::Accepted
            | SubTaskState::Verifying
            | SubTaskState::Unknown => {
                let check =
                    query_board_task_record(board_client, &row.device_id, row.subtask_id).await;
                if check.verified {
                    summary.read_only_verified += 1;
                }
                checks_to_insert.push((
                    Uuid::new_v4(),
                    *row.subtask_id.as_bytes(),
                    check.outcome,
                    check.detail_redacted,
                ));
            }
            SubTaskState::Queued => match classify_recovery(row.state, false, false) {
                RecoveryAction::MarkHeld { .. } => {
                    summary.marked_held += 1;
                    held_to_update.push(row.subtask_id);
                }
                RecoveryAction::MarkExpired { .. } => summary.marked_expired += 1,
                RecoveryAction::ReadOnlyVerify { .. } => summary.read_only_verified += 1,
            },
            // Held 与终态：幂等只读，零查询零写入
            _ => {}
        }
    }

    // 阶段 3：写入短事务——纯内存批量写，事务内零网络调用；无待写入项则不开事务。
    if !checks_to_insert.is_empty() || !held_to_update.is_empty() {
        let mut write_tx = repo.begin().await?;
        for (check_id, subtask_id, outcome, detail_redacted) in checks_to_insert {
            tx_execute(
                &mut write_tx,
                INSERT_RECOVERY_CHECK_SQL,
                recovery_params![
                    check_id.as_bytes().as_slice(),
                    process_epoch.as_bytes().as_slice(),
                    subtask_id.as_slice(),
                    outcome,
                    detail_redacted,
                ],
            )
            .await?;
        }
        for subtask_id in held_to_update {
            tx_execute(
                &mut write_tx,
                UPDATE_QUEUED_HELD_SQL,
                recovery_params![subtask_id.as_bytes().as_slice()],
            )
            .await?;
        }
        write_tx.commit().await?;
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_never_plans_a_resend_for_possibly_sent_states() {
        use crate::tasks::model::SubTaskState::*;
        for s in [Dispatching, Accepted, Verifying] {
            let action = classify_recovery(s, true, false);
            assert!(
                matches!(action, RecoveryAction::ReadOnlyVerify { .. }),
                "{s:?} must be read-only verified, got {action:?}"
            );
        }
        // 期限不可信 -> held（expired 参数被忽略，调用方固定传 false）
        assert!(matches!(
            classify_recovery(Queued, false, false),
            RecoveryAction::MarkHeld { .. }
        ));
        // 可信且未过期 -> 只读核验（精确分支断言，无 OR 掩盖）
        assert!(matches!(
            classify_recovery(Queued, true, false),
            RecoveryAction::ReadOnlyVerify { .. }
        ));
        // 可信且已过期 -> MarkExpired（精确分支断言，无 OR 掩盖）
        assert!(matches!(
            classify_recovery(Queued, true, true),
            RecoveryAction::MarkExpired { .. }
        ));
        // 终态不重新执行
        for s in [Succeeded, Failed, Cancelled, Expired] {
            assert!(matches!(
                classify_recovery(s, true, false),
                RecoveryAction::ReadOnlyVerify { .. }
            ));
        }
        // Unknown：只读核实，绝不重发
        assert!(matches!(
            classify_recovery(Unknown, true, false),
            RecoveryAction::ReadOnlyVerify { .. }
        ));
    }

    #[test]
    fn success_evidence_rejects_cross_epoch_version_regression_and_contradiction() {
        use crate::tasks::model::{EvidenceKind, TaskEvidence};
        let ev = TaskEvidence {
            subtask_id: Uuid::nil(),
            journal_epoch: Uuid::nil(),
            record_version: 2,
            observed_boot_id: "boot-b2".into(),
            pre_boot_id: "boot-b1".into(),
            current_boot_id: "boot-b2".into(),
            evidence_kind: EvidenceKind::BootTransitionObserved,
            reliable_attempted: true,
        };
        let baseline = EvidenceBaseline {
            journal_epoch: Uuid::nil(),
            record_version: 1,
            confirmed_failed_versions: Vec::new(),
        };
        // 正例：同 epoch、record_version 合法递增、无矛盾失败记录 -> 成功
        assert!(success_evidence_satisfied("boot-b1", &ev, &baseline));
        // 负例 1：跨 journal_epoch 拒绝
        let mut cross_epoch = ev.clone();
        cross_epoch.journal_epoch = Uuid::from_u128(7);
        assert!(!success_evidence_satisfied(
            "boot-b1",
            &cross_epoch,
            &baseline
        ));
        // 负例 2：record_version 倒退（不大于基线）拒绝
        let mut regressed = ev.clone();
        regressed.record_version = 1;
        assert!(!success_evidence_satisfied(
            "boot-b1", &regressed, &baseline
        ));
        // 负例 3：存在已确认 Failed 矛盾记录 -> 拒绝
        let contradicted = EvidenceBaseline {
            journal_epoch: Uuid::nil(),
            record_version: 1,
            confirmed_failed_versions: vec![2],
        };
        assert!(!success_evidence_satisfied("boot-b1", &ev, &contradicted));
    }

    #[test]
    fn main_task_outcome_corrected_by_late_evidence() {
        use crate::tasks::model::SubTaskState::*;
        assert_eq!(
            calculate_main_task_outcome(&[Succeeded, Succeeded]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Success))
        );
        assert_eq!(
            calculate_main_task_outcome(&[Succeeded, Unknown]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Unknown))
        );
        assert_eq!(
            calculate_main_task_outcome(&[Succeeded, Failed]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Partial))
        );
        assert_eq!(
            calculate_main_task_outcome(&[Cancelled, Cancelled]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Cancelled))
        );
        assert_eq!(
            calculate_main_task_outcome(&[Failed, Failed]),
            (MainTaskState::Completed, Some(MainTaskOutcome::Failed))
        );
        assert_eq!(
            calculate_main_task_outcome(&[Queued, Queued]),
            (MainTaskState::Queued, None)
        );
        assert_eq!(
            calculate_main_task_outcome(&[Dispatching, Queued]),
            (MainTaskState::Running, None)
        );
        assert_eq!(
            calculate_main_task_outcome(&[Held, Succeeded]),
            (MainTaskState::Held, None)
        );
    }

    // ---- recover_process：模拟 BoardClient 下的只读核验 ----

    use crate::tasks::board_client::{BoardClientError, BoardFuture};
    use crate::tasks::preview::PreviewTarget;
    use crate::tasks::repository::{MainTaskRow, SqlxTaskRepository, SubTaskRow};
    use crate::tasks::service::{
        AuthCtx, CancelOutcome, CancelRequest, ReleaseLockError, ReleaseLockOutcome,
        ReleaseRequest, SubmitOutcome, SubmitRequest, TimeReviewError, TimeReviewOutcome,
        TimeReviewRequest,
    };
    use crate::time::evidence::TimeEvidence;
    use chrono::{DateTime, Utc};
    use rsetup_protocol::wire::{
        Capabilities, ClockSample, DeviceStatus, RebootExecute, RebootTicket, TaskRecord,
    };
    use sqlx::Transaction;
    use sqlx::mysql::{MySql, MySqlPool, MySqlPoolOptions};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// 零 DB 假仓储：`begin` 一律以存储错误短路（与 service.rs 的 FakeRepo 同口径），
    /// 用于证明 `recover_process` 在 DB 故障下 fail-closed、绝不触板端。
    #[derive(Default)]
    struct FakeRepo;

    // 与生产实现同风格：future 的 Send 边界写进签名，故豁免 manual_async_fn。
    #[allow(clippy::manual_async_fn)]
    impl TaskRepository for FakeRepo {
        fn begin(
            &self,
        ) -> impl std::future::Future<Output = Result<Transaction<'static, MySql>, ControllerError>> + Send
        {
            async move { Err(ControllerError::Database(sqlx::Error::RowNotFound)) }
        }

        fn create_preview(
            &self,
            _token_nonce: [u8; 32],
            _actor_user_id: Uuid,
            _targets: &[PreviewTarget],
            _token_hash: &[u8; 32],
            _expires_at: DateTime<Utc>,
        ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send {
            async { Err(ControllerError::Database(sqlx::Error::RowNotFound)) }
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
        ) -> impl std::future::Future<Output = Result<Option<MainTaskRow>, ControllerError>> + Send
        {
            async { Ok(None) }
        }

        fn list_subtasks_page(
            &self,
            _main_task_id: Uuid,
            _cursor: Option<String>,
            _limit: usize,
        ) -> impl std::future::Future<
            Output = Result<(Vec<SubTaskRow>, Option<String>), ControllerError>,
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

    /// 板端记录 fixture（`BoardClientError` 刻意不派生 Clone，故用本地枚举构造）。
    #[derive(Clone)]
    enum FakeRecord {
        Found(TaskRecord),
        NotFound,
        Timeout,
        Transport,
    }

    /// 模拟板端 Client：逐调用记账。恢复路径只允许出现 `task_record`；
    /// `prepare`/`execute` 计数即「发送调用」计数，必须恒为 0。
    #[derive(Default)]
    struct FakeBoardClient {
        task_record_calls: Mutex<Vec<([u8; 32], Uuid)>>,
        read_calls: AtomicUsize,
        prepare_calls: AtomicUsize,
        execute_calls: AtomicUsize,
        records: Mutex<HashMap<Uuid, FakeRecord>>,
        /// 每次 `task_record` 调用时刻数据库已发生的 COMMIT 次数：
        /// 修复轮 2 用于断言网络核验发生在读、写两次短事务之间。
        commits_at_board_calls: Mutex<Vec<usize>>,
        /// 测试注入的数据库事件记录器；未注入时不记账。
        db_recorded: Mutex<Option<Arc<Mutex<MockRecordedDb>>>>,
    }

    impl FakeBoardClient {
        fn with_record(&mut self, subtask_id: Uuid, record: FakeRecord) {
            self.records
                .get_mut()
                .expect("未中毒")
                .insert(subtask_id, record);
        }
        /// 注入 MockDbServer 的事件记录器，使 `task_record` 能记录调用时刻已提交的
        /// 短事务数（验证网络在两次事务之间）。
        fn attach_db_recorded(&mut self, recorded: Arc<Mutex<MockRecordedDb>>) {
            *self.db_recorded.lock().expect("未中毒") = Some(recorded);
        }
        fn commits_at_board_calls(&self) -> Vec<usize> {
            self.commits_at_board_calls.lock().expect("未中毒").clone()
        }
        fn task_record_calls(&self) -> Vec<([u8; 32], Uuid)> {
            self.task_record_calls.lock().expect("未中毒").clone()
        }
        /// 发送调用总数（prepare + execute）：恢复路径必须恒为 0。
        fn send_calls(&self) -> usize {
            self.prepare_calls.load(Ordering::Relaxed) + self.execute_calls.load(Ordering::Relaxed)
        }
    }

    impl BoardClient for FakeBoardClient {
        fn capabilities<'a>(&'a self, _device: &'a [u8; 32]) -> BoardFuture<'a, Capabilities> {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(BoardClientError::CapabilityUnavailable) })
        }
        fn status<'a>(&'a self, _device: &'a [u8; 32]) -> BoardFuture<'a, DeviceStatus> {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(BoardClientError::DeviceOffline) })
        }
        fn clock<'a>(&'a self, _device: &'a [u8; 32]) -> BoardFuture<'a, ClockSample> {
            self.read_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(BoardClientError::DeviceOffline) })
        }
        fn prepare<'a>(
            &'a self,
            _device: &'a [u8; 32],
            _sub_task_id: Uuid,
            _expected_boot_id: &'a str,
        ) -> BoardFuture<'a, RebootTicket> {
            self.prepare_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(BoardClientError::DeviceOffline) })
        }
        fn execute<'a>(
            &'a self,
            _device: &'a [u8; 32],
            _ticket: &'a RebootTicket,
            _request: &'a RebootExecute,
        ) -> BoardFuture<'a, TaskRecord> {
            self.execute_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(BoardClientError::DeviceOffline) })
        }
        fn task_record<'a>(
            &'a self,
            device: &'a [u8; 32],
            sub_task_id: Uuid,
        ) -> BoardFuture<'a, Option<TaskRecord>> {
            self.task_record_calls
                .lock()
                .expect("未中毒")
                .push((*device, sub_task_id));
            if let Some(recorded) = self.db_recorded.lock().expect("未中毒").as_ref() {
                let commits = recorded.lock().expect("未中毒").commit_count;
                self.commits_at_board_calls
                    .lock()
                    .expect("未中毒")
                    .push(commits);
            }
            let record = self
                .records
                .lock()
                .expect("未中毒")
                .get(&sub_task_id)
                .cloned();
            Box::pin(async move {
                match record {
                    Some(FakeRecord::Found(task_record)) => Ok(Some(task_record)),
                    // 未预置与显式 NotFound 同为 TASK_NOT_FOUND(1006)
                    Some(FakeRecord::NotFound) | None => Ok(None),
                    Some(FakeRecord::Timeout) => Err(BoardClientError::Timeout),
                    Some(FakeRecord::Transport) => Err(BoardClientError::Transport),
                }
            })
        }
    }

    fn board_task_record(subtask_id: Uuid) -> TaskRecord {
        TaskRecord {
            schema_version: 1,
            sub_task_id: subtask_id.to_string(),
            journal_epoch: Uuid::nil().to_string(),
            record_version: 1,
            state: "attempted".to_owned(),
            pre_boot_id: "boot-b1".to_owned(),
            observed_boot_id: Some("boot-b2".to_owned()),
            reason_code: None,
            updated_unix_ms: None,
            evidence_kind: Some("boot_transition_observed".to_owned()),
        }
    }

    fn lenenc_int(val: usize) -> Vec<u8> {
        if val < 251 {
            vec![val as u8]
        } else if val < 65536 {
            let mut b = vec![0xfc];
            b.extend(&(val as u16).to_le_bytes());
            b
        } else {
            let mut b = vec![0xfd];
            b.extend(&(val as u32).to_le_bytes()[..3]);
            b
        }
    }

    fn lenenc_str(s: &[u8]) -> Vec<u8> {
        let mut b = lenenc_int(s.len());
        b.extend(s);
        b
    }

    fn make_col_def(name: &str, col_type: u8) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(lenenc_str(b"def"));
        b.extend(lenenc_str(b"test"));
        b.extend(lenenc_str(b"sub_tasks"));
        b.extend(lenenc_str(b"sub_tasks"));
        b.extend(lenenc_str(name.as_bytes()));
        b.extend(lenenc_str(name.as_bytes()));
        b.push(0x0c);
        b.extend(&33u16.to_le_bytes());
        b.extend(&1024u32.to_le_bytes());
        b.push(col_type);
        b.extend(&0u16.to_le_bytes());
        b.push(0);
        b.extend(&[0, 0]);
        b
    }

    fn send_packet(stream: &mut TcpStream, seq: u8, payload: &[u8]) {
        let len = payload.len();
        let hdr = [
            (len & 0xff) as u8,
            ((len >> 8) & 0xff) as u8,
            ((len >> 16) & 0xff) as u8,
            seq,
        ];
        let _ = stream.write_all(&hdr);
        let _ = stream.write_all(payload);
        let _ = stream.flush();
    }

    fn recv_packet(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
        let mut hdr = [0u8; 4];
        if stream.read_exact(&mut hdr).is_err() {
            return None;
        }
        let plen = (hdr[0] as usize) | ((hdr[1] as usize) << 8) | ((hdr[2] as usize) << 16);
        let seq = hdr[3];
        let mut body = vec![0u8; plen];
        if stream.read_exact(&mut body).is_err() {
            return None;
        }
        Some((seq, body))
    }

    #[derive(Default)]
    struct MockRecordedDb {
        select_count: usize,
        insert_checks_count: usize,
        update_held_count: usize,
        commit_count: usize,
        /// 有序事件轨迹（select/insert/update/commit）：用于断言读、写两个短事务
        /// 的边界，验证板端网络核验确实发生在两次事务之间。
        events: Vec<String>,
    }

    struct MockDbServer {
        port: u16,
        recorded: Arc<Mutex<MockRecordedDb>>,
    }

    impl MockDbServer {
        fn start(rows: Vec<([u8; 16], [u8; 32], &'static str)>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("port").port();
            let recorded = Arc::new(Mutex::new(MockRecordedDb::default()));
            let rec = recorded.clone();
            let rows = Arc::new(rows);

            thread::spawn(move || {
                while let Ok((mut stream, _)) = listener.accept() {
                    let rec = rec.clone();
                    let rows = rows.clone();
                    thread::spawn(move || {
                        let server_ver = b"8.0.46\0";
                        let auth_data_1 = b"12345678";
                        let auth_data_2 = b"123456789012\0";
                        let cap: u32 = 0x0000
                            | 0x0200
                            | 0x8000
                            | 0x0004
                            | 0x020000
                            | 0x0008
                            | 0x00800000
                            | 0x00000001;
                        let mut payload = Vec::new();
                        payload.push(10);
                        payload.extend(server_ver);
                        payload.extend(&1u32.to_le_bytes());
                        payload.extend(auth_data_1);
                        payload.push(0);
                        payload.extend(&(cap as u16).to_le_bytes());
                        payload.push(33);
                        payload.extend(&2u16.to_le_bytes());
                        payload.extend(&((cap >> 16) as u16).to_le_bytes());
                        payload.push(21);
                        payload.extend(&[0u8; 10]);
                        payload.extend(auth_data_2);
                        payload.extend(b"mysql_native_password\0");
                        send_packet(&mut stream, 0, &payload);

                        let (seq, _) = match recv_packet(&mut stream) {
                            Some(p) => p,
                            None => return,
                        };
                        send_packet(
                            &mut stream,
                            seq.wrapping_add(1),
                            &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00],
                        );

                        let mut next_stmt_id = 1u32;
                        let mut stmt_types = HashMap::new();

                        while let Some((seq, body)) = recv_packet(&mut stream) {
                            if body.is_empty() {
                                break;
                            }
                            let cmd = body[0];
                            if cmd == 0x03 {
                                let q = String::from_utf8_lossy(&body[1..]);
                                let status = if q.contains("BEGIN") {
                                    0x0003u16
                                } else {
                                    0x0002u16
                                };
                                if q.contains("COMMIT") {
                                    let mut rec = rec.lock().unwrap();
                                    rec.commit_count += 1;
                                    rec.events.push("commit".to_owned());
                                }
                                let ok = [
                                    0x00,
                                    0x00,
                                    0x00,
                                    (status & 0xff) as u8,
                                    (status >> 8) as u8,
                                    0x00,
                                    0x00,
                                ];
                                send_packet(&mut stream, seq.wrapping_add(1), &ok);
                            } else if cmd == 0x0e {
                                send_packet(
                                    &mut stream,
                                    seq.wrapping_add(1),
                                    &[0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00],
                                );
                            } else if cmd == 0x16 {
                                let q = String::from_utf8_lossy(&body[1..]);
                                let stmt_id = next_stmt_id;
                                next_stmt_id += 1;
                                let is_select = q.contains("SELECT");
                                let is_insert = q.contains("INSERT INTO recovery_checks");
                                let is_update = q.contains("UPDATE sub_tasks SET state = 'held'");
                                let cols = if is_select { 3u16 } else { 0u16 };
                                let params = if is_insert {
                                    5u16
                                } else if is_update {
                                    1u16
                                } else {
                                    0u16
                                };
                                stmt_types.insert(stmt_id, (is_select, is_insert, is_update));

                                let mut prep_ok = vec![0x00];
                                prep_ok.extend(&stmt_id.to_le_bytes());
                                prep_ok.extend(&cols.to_le_bytes());
                                prep_ok.extend(&params.to_le_bytes());
                                prep_ok.push(0);
                                prep_ok.extend(&0u16.to_le_bytes());
                                let mut cur_seq = seq.wrapping_add(1);
                                send_packet(&mut stream, cur_seq, &prep_ok);

                                if params > 0 {
                                    for i in 0..params {
                                        cur_seq = cur_seq.wrapping_add(1);
                                        send_packet(
                                            &mut stream,
                                            cur_seq,
                                            &make_col_def(&format!("?_{i}"), 0xfd),
                                        );
                                    }
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(
                                        &mut stream,
                                        cur_seq,
                                        &[0xfe, 0x00, 0x00, 0x03, 0x00],
                                    );
                                }
                                if cols > 0 {
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(&mut stream, cur_seq, &make_col_def("id", 0xfc));
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(
                                        &mut stream,
                                        cur_seq,
                                        &make_col_def("device_id", 0xfc),
                                    );
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(&mut stream, cur_seq, &make_col_def("state", 0xfd));
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(
                                        &mut stream,
                                        cur_seq,
                                        &[0xfe, 0x00, 0x00, 0x03, 0x00],
                                    );
                                }
                            } else if cmd == 0x17 {
                                let stmt_id = u32::from_le_bytes(body[1..5].try_into().unwrap());
                                let (is_select, is_insert, is_update) = stmt_types
                                    .get(&stmt_id)
                                    .copied()
                                    .unwrap_or((false, false, false));
                                let mut cur_seq = seq.wrapping_add(1);
                                if is_select {
                                    {
                                        let mut rec = rec.lock().unwrap();
                                        rec.select_count += 1;
                                        rec.events.push("select".to_owned());
                                    }
                                    send_packet(&mut stream, cur_seq, &lenenc_int(3));
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(&mut stream, cur_seq, &make_col_def("id", 0xfc));
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(
                                        &mut stream,
                                        cur_seq,
                                        &make_col_def("device_id", 0xfc),
                                    );
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(&mut stream, cur_seq, &make_col_def("state", 0xfd));
                                    cur_seq = cur_seq.wrapping_add(1);
                                    let eof = [0xfe, 0x00, 0x00, 0x03, 0x00];
                                    send_packet(&mut stream, cur_seq, &eof);

                                    for (id, dev, st) in rows.iter() {
                                        let mut r = vec![0x00, 0x00];
                                        r.extend(lenenc_str(id));
                                        r.extend(lenenc_str(dev));
                                        r.extend(lenenc_str(st.as_bytes()));
                                        cur_seq = cur_seq.wrapping_add(1);
                                        send_packet(&mut stream, cur_seq, &r);
                                    }
                                    cur_seq = cur_seq.wrapping_add(1);
                                    send_packet(&mut stream, cur_seq, &eof);
                                } else {
                                    {
                                        let mut rec = rec.lock().unwrap();
                                        if is_insert {
                                            rec.insert_checks_count += 1;
                                            rec.events.push("insert".to_owned());
                                        } else if is_update {
                                            rec.update_held_count += 1;
                                            rec.events.push("update".to_owned());
                                        }
                                    }
                                    send_packet(
                                        &mut stream,
                                        cur_seq,
                                        &[0x00, 0x01, 0x00, 0x03, 0x00, 0x00, 0x00],
                                    );
                                }
                            } else if cmd == 0x19 {
                                send_packet(
                                    &mut stream,
                                    seq.wrapping_add(1),
                                    &[0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00],
                                );
                            } else if cmd == 0x1a {
                                // close
                            } else if cmd == 0x01 {
                                break;
                            }
                        }
                    });
                }
            });

            Self { port, recorded }
        }

        async fn pool(&self) -> MySqlPool {
            let url = format!("mysql://127.0.0.1:{}/test", self.port);
            MySqlPoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await
                .expect("connect mock mysql")
        }
    }

    #[tokio::test]
    async fn recover_process_end_to_end_verifies_states_and_persists_checks_and_held() {
        let dispatching_id = Uuid::from_u128(101);
        let accepted_id = Uuid::from_u128(102);
        let verifying_id = Uuid::from_u128(103);
        let unknown_id = Uuid::from_u128(104);
        let queued_id = Uuid::from_u128(105);

        let dev_1 = [0x11u8; 32];
        let dev_2 = [0x22u8; 32];
        let dev_3 = [0x33u8; 32];
        let dev_4 = [0x44u8; 32];
        let dev_5 = [0x55u8; 32];

        let server = MockDbServer::start(vec![
            (*dispatching_id.as_bytes(), dev_1, "dispatching"),
            (*accepted_id.as_bytes(), dev_2, "accepted"),
            (*verifying_id.as_bytes(), dev_3, "verifying"),
            (*unknown_id.as_bytes(), dev_4, "unknown"),
            (*queued_id.as_bytes(), dev_5, "queued"),
        ]);
        let pool = server.pool().await;
        let repo = SqlxTaskRepository::new(pool);

        let mut client = FakeBoardClient::default();
        client.attach_db_recorded(server.recorded.clone());
        client.with_record(
            dispatching_id,
            FakeRecord::Found(board_task_record(dispatching_id)),
        );
        client.with_record(accepted_id, FakeRecord::NotFound);
        client.with_record(verifying_id, FakeRecord::Timeout);
        client.with_record(unknown_id, FakeRecord::Transport);

        let process_epoch = Uuid::from_u128(999);
        let summary = recover_process(&repo, &client, process_epoch)
            .await
            .expect("recover_process 必须成功");

        // F3 (P2) 核心断言：断言 board_client.task_record 收到的 device_id 与 sub_task_id 准确匹配行数据
        assert_eq!(
            client.task_record_calls(),
            vec![
                (dev_1, dispatching_id),
                (dev_2, accepted_id),
                (dev_3, verifying_id),
                (dev_4, unknown_id),
            ]
        );

        // 摘要统计断言
        assert_eq!(summary.read_only_verified, 2);
        assert_eq!(summary.marked_held, 1);
        assert_eq!(summary.marked_expired, 0);
        assert_eq!(summary.execute_calls, 0);

        // 发送调用恒为 0
        assert_eq!(client.send_calls(), 0);
        assert_eq!(client.read_calls.load(Ordering::Relaxed), 0);

        // 持久化与事务断言 (F2 + 修复轮 2)：
        // 4 个可能已发送态插入 recovery_checks，1 个 queued 状态更新为 held；
        // 读、写两个独立短事务各自提交（commit_count == 2），网络核验发生在两者之间。
        let rec = server.recorded.lock().unwrap();
        assert_eq!(rec.select_count, 1);
        assert_eq!(rec.insert_checks_count, 4);
        assert_eq!(rec.update_held_count, 1);
        assert_eq!(rec.commit_count, 2);
        // 4 次 task_record 调用时刻数据库均只完成了第 1 次 COMMIT（读事务已提交、
        // 写事务尚未开始）：板端网络等待完全发生在两次短事务之间，零持锁。
        assert_eq!(client.commits_at_board_calls(), vec![1, 1, 1, 1]);
        // 有序轨迹：读事务（SELECT→COMMIT）→（事务外板端核验）→
        // 写事务（INSERT×4→UPDATE→COMMIT）。
        assert_eq!(
            rec.events,
            vec![
                "select".to_owned(),
                "commit".to_owned(),
                "insert".to_owned(),
                "insert".to_owned(),
                "insert".to_owned(),
                "insert".to_owned(),
                "update".to_owned(),
                "commit".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn recover_process_empty_pending_succeeds_without_board_calls() {
        let server = MockDbServer::start(vec![]);
        let pool = server.pool().await;
        let repo = SqlxTaskRepository::new(pool);
        let client = FakeBoardClient::default();

        let process_epoch = Uuid::from_u128(888);
        let summary = recover_process(&repo, &client, process_epoch)
            .await
            .expect("空未决集必须成功");

        assert_eq!(
            summary,
            RecoverySummary {
                read_only_verified: 0,
                marked_held: 0,
                marked_expired: 0,
                execute_calls: 0,
            }
        );
        assert!(client.task_record_calls().is_empty());
        assert_eq!(client.send_calls(), 0);

        let rec = server.recorded.lock().unwrap();
        assert_eq!(rec.select_count, 1);
        assert_eq!(rec.insert_checks_count, 0);
        assert_eq!(rec.update_held_count, 0);
        // 空未决集：仅经历一次只读短事务，不开启写事务。
        assert_eq!(rec.commit_count, 1);
        assert_eq!(rec.events, vec!["select".to_owned(), "commit".to_owned()]);
    }

    #[tokio::test]
    async fn recover_process_fails_closed_without_touching_board_client() {
        let client = FakeBoardClient::default();
        let err = recover_process(&FakeRepo, &client, Uuid::from_u128(9))
            .await
            .expect_err("begin 失败必须原样上抛");
        assert!(matches!(err, ControllerError::Database(_)));
        // DB 故障在任何板端调用之前短路：零查询、零发送。
        assert!(client.task_record_calls().is_empty());
        assert_eq!(client.send_calls(), 0);
        assert_eq!(client.read_calls.load(Ordering::Relaxed), 0);
    }
}

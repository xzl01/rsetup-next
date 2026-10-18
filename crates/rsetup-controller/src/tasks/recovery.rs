//! 只读恢复分类、主任务结局聚合纯逻辑与只读恢复编排（04 §4、§5.2、§6）。
//!
//! 纯函数切片（`classify_recovery`/`success_evidence_satisfied`/
//! `calculate_main_task_outcome`）不依赖 `rsetup_protocol`、不连数据库、不调用
//! 板端。[`recover_process`] 为 wire DTO 门禁通过后的 typed 编排切片：经
//! [`TaskRepository::begin`] 的只读短事务重建未决集，对可能已发送的状态**只**
//! 调用 [`BoardClient::task_record`]；`recovery_checks` 落库、状态迁移与可信
//! 时钟判定属集成接线，归集成 owner，本切片不写 DB、不构造伪桩。
//!
//! 恢复路径**永不**产生发送动作：实现中不存在任何 `prepare`/`execute` 调用点，
//! `RecoverySummary::execute_calls` 恒为 0。

use crate::ControllerError;
use crate::tasks::board_client::BoardClient;
use crate::tasks::model::{
    EvidenceKind, MainTaskOutcome, MainTaskState, SubTaskState, TaskEvidence,
};
use crate::tasks::repository::{
    TaskRepository, column, fixed_mysql_bytes, mysql_uuid, task_row_decode_error, tx_rows,
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

/// 未决集读取 SQL（只读）：本进程代际（`controller_epoch`）派发、仍可能产生板端
/// 效果的行。`dispatching/accepted/verifying/unknown` 走只读核验，`queued` 走
/// [`classify_recovery`] 分类；终态行不产生任何恢复动作，不读取。
const RECOVERY_PENDING_SQL: &str = "SELECT id, device_id, state FROM sub_tasks \
                                    WHERE controller_epoch = ? \
                                    AND state IN ('dispatching', 'accepted', 'verifying', \
                                                  'unknown', 'queued') \
                                    ORDER BY id ASC";

/// 经 [`TaskRepository::begin`] 的只读短事务重建未决集；fail-closed：任一行损坏
/// 即整体失败（去敏 DB 损坏 503），不跳行、不截断。事务内零网络。
async fn load_recovery_pending<R: TaskRepository>(
    repo: &R,
    process_epoch: Uuid,
) -> Result<Vec<RecoveryPendingRow>, ControllerError> {
    let mut tx = repo.begin().await?;
    let mut args = MySqlArguments::default();
    let epoch_bytes = process_epoch.as_bytes().as_slice();
    // 与 params! 宏同口径：编码失败 → Database(Encode) 503
    sqlx::Arguments::add(&mut args, &epoch_bytes)
        .map_err(|err| ControllerError::Database(sqlx::Error::Encode(err)))?;
    let rows = tx_rows(&mut tx, RECOVERY_PENDING_SQL, args).await?;
    let mut pending = Vec::with_capacity(rows.len());
    for row in &rows {
        pending.push(RecoveryPendingRow {
            subtask_id: mysql_uuid(&column::<Vec<u8>>(row, "id")?)?,
            device_id: fixed_mysql_bytes::<32>(&column::<Vec<u8>>(row, "device_id")?)?,
            state: SubTaskState::from_str_exact(&column::<String>(row, "state")?)
                .ok_or_else(task_row_decode_error)?,
        });
    }
    tx.commit().await?;
    Ok(pending)
}

/// 只读核验决策环（[`recover_process`] 的板端交互核心，独立可测）：
///
/// - `Dispatching | Accepted | Verifying | Unknown`：**只**调用
///   [`BoardClient::task_record`]。`Ok(Some(_))`/`Ok(None)` 都算已核验（`None` 即
///   TASK_NOT_FOUND(1006)，不证明 `execute` 未执行，不单独推进状态）；
///   `Err(Timeout)`/`Err(Transport)` 等一律不计数、不推进（保持原状态，由观察窗
///   到期后再处置）。本函数不写 DB：核验结果落 `recovery_checks` 属集成接线；
/// - `Queued`：按 [`classify_recovery`] 分类。本函数无可信时钟源输入（时钟可信性
///   判定属集成接线），按 fail-closed 口径以「期限不可信」调用 → `MarkHeld`；
/// - `Held` 与终态：幂等只读，不查询、不计数、不写。
///
/// 本函数**不存在任何 `prepare`/`execute` 调用点**，返回的 `execute_calls` 恒为 0。
async fn verify_pending_read_only<C: BoardClient>(
    board_client: &C,
    pending: &[RecoveryPendingRow],
) -> RecoverySummary {
    let mut summary = RecoverySummary::default();
    for row in pending {
        match row.state {
            SubTaskState::Dispatching
            | SubTaskState::Accepted
            | SubTaskState::Verifying
            | SubTaskState::Unknown => {
                // 仅查询成功（Ok(Some)/Ok(None)）计数；Timeout/Transport/其余错误
                // 一律不推进、不重发、不计数。
                if board_client
                    .task_record(&row.device_id, row.subtask_id)
                    .await
                    .is_ok()
                {
                    summary.read_only_verified += 1;
                }
            }
            SubTaskState::Queued => match classify_recovery(row.state, false, false) {
                RecoveryAction::MarkHeld { .. } => summary.marked_held += 1,
                RecoveryAction::MarkExpired { .. } => summary.marked_expired += 1,
                RecoveryAction::ReadOnlyVerify { .. } => summary.read_only_verified += 1,
            },
            // Held 与终态：幂等只读，零查询零写入
            _ => {}
        }
    }
    summary
}

/// 进程重启后的只读恢复（04 §5.2、04 Task 5 Step 3.6）。
///
/// 流程：①经 [`TaskRepository::begin`] 只读短事务按 `controller_epoch` 重建未决
/// 集；②对 `Dispatching | Accepted | Verifying | Unknown` **只**调用
/// [`BoardClient::task_record`] 做只读核验（[`verify_pending_read_only`]），
/// 对 `Queued` 按 [`classify_recovery`] 分类统计。
///
/// 硬约束：本函数与其调用的核心**永不**产生发送动作——不存在任何
/// `prepare`/`execute` 调用点，`RecoverySummary::execute_calls` 恒为 0。
/// `board_client` 由调用方显式注入（集成装配以 `Arc<dyn BoardClient>` 构造，与
/// `TaskApi` adapter 注入路径同源）。核验结果写 `recovery_checks`
/// （`detail_redacted` 去敏）、状态迁移（`held`/`expired` 落库）与可信时钟判定
/// 属集成接线，归集成 owner，本切片不实现。
pub async fn recover_process<R: TaskRepository, C: BoardClient>(
    repo: &R,
    board_client: &C,
    process_epoch: Uuid,
) -> Result<RecoverySummary, ControllerError> {
    let pending = load_recovery_pending(repo, process_epoch).await?;
    Ok(verify_pending_read_only(board_client, &pending).await)
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

    use super::{RecoveryPendingRow, verify_pending_read_only};
    use crate::tasks::board_client::{BoardClientError, BoardFuture};
    use crate::tasks::preview::PreviewTarget;
    use crate::tasks::repository::{MainTaskRow, SubTaskRow};
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
    use sqlx::mysql::MySql;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        task_record_calls: Mutex<Vec<Uuid>>,
        read_calls: AtomicUsize,
        prepare_calls: AtomicUsize,
        execute_calls: AtomicUsize,
        records: Mutex<HashMap<Uuid, FakeRecord>>,
    }

    impl FakeBoardClient {
        fn with_record(&mut self, subtask_id: Uuid, record: FakeRecord) {
            self.records
                .get_mut()
                .expect("未中毒")
                .insert(subtask_id, record);
        }
        fn task_record_calls(&self) -> Vec<Uuid> {
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
            _device: &'a [u8; 32],
            sub_task_id: Uuid,
        ) -> BoardFuture<'a, Option<TaskRecord>> {
            self.task_record_calls
                .lock()
                .expect("未中毒")
                .push(sub_task_id);
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

    fn pending_row(subtask_id: Uuid, state: SubTaskState) -> RecoveryPendingRow {
        RecoveryPendingRow {
            subtask_id,
            device_id: [0xa1u8; 32],
            state,
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

    #[tokio::test]
    async fn recover_process_read_only_verifies_possibly_sent_states() {
        let dispatching = Uuid::from_u128(1);
        let accepted = Uuid::from_u128(2);
        let verifying = Uuid::from_u128(3);
        let unknown = Uuid::from_u128(4);
        let queued = Uuid::from_u128(5);
        let held = Uuid::from_u128(6);
        let succeeded = Uuid::from_u128(7);

        let mut client = FakeBoardClient::default();
        client.with_record(
            dispatching,
            FakeRecord::Found(board_task_record(dispatching)),
        );
        client.with_record(accepted, FakeRecord::NotFound);
        client.with_record(verifying, FakeRecord::Timeout);
        client.with_record(unknown, FakeRecord::Transport);

        let pending = vec![
            pending_row(dispatching, SubTaskState::Dispatching),
            pending_row(accepted, SubTaskState::Accepted),
            pending_row(verifying, SubTaskState::Verifying),
            pending_row(unknown, SubTaskState::Unknown),
            pending_row(queued, SubTaskState::Queued),
            pending_row(held, SubTaskState::Held),
            pending_row(succeeded, SubTaskState::Succeeded),
        ];
        let summary = verify_pending_read_only(&client, &pending).await;

        // 只读核验计数：Ok(Some)/Ok(None) 均算已核验（None 不证明未执行、不推进）；
        // Timeout/Transport 不计数、不推进、不重发。
        assert_eq!(summary.read_only_verified, 2);
        // Queued：无可信时钟输入 → 期限按不可信 fail-closed → MarkHeld；
        // MarkExpired 分支在本切片不可达，恒为 0。
        assert_eq!(summary.marked_held, 1);
        assert_eq!(summary.marked_expired, 0);
        // 计数哨兵：execute_calls 恒为 0
        assert_eq!(summary.execute_calls, 0);

        // 只有 4 个可能已发送状态被查询，且每个恰一次；Queued/Held/终态零查询。
        assert_eq!(
            client.task_record_calls(),
            vec![dispatching, accepted, verifying, unknown]
        );
        // 发送调用（prepare/execute）与其余板端读调用一律为零。
        assert_eq!(client.send_calls(), 0);
        assert_eq!(client.read_calls.load(Ordering::Relaxed), 0);
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

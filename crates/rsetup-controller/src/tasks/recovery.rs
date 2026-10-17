//! 只读恢复分类与主任务结局聚合纯逻辑（04 §4、§5.2、§6）。
//!
//! 本模块只交付**离线纯函数切片**：不依赖 `rsetup_protocol` typed wire DTO，
//! 不连数据库、不调用板端。`recover_process`/`RecoverySummary`/`BoardClient`
//! 依赖尚未引入本 crate 的 wire DTO，属 `BLOCKED (awaiting BoardClient)` 的接线
//! 切片，归集成 owner，不在本文件定义，也不构造伪桩。
//!
//! 恢复路径**永不**产生发送动作：可能已发送的状态只做只读核验。

use crate::tasks::model::{
    EvidenceKind, MainTaskOutcome, MainTaskState, SubTaskState, TaskEvidence,
};
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
}

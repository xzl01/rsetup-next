//! 任务状态规则、终态判定与十态频数收敛纯策略。
//!
//! 包含取消前置条件校验、终态判定、十态全集频数序列化（`counts_json` 口径真相源）、
//! 状态折叠（`tally`）、子任务状态解析以及主任务按 04 §4 优先级收敛逻辑。

use std::collections::HashMap;

use crate::error::ControllerError;
use crate::tasks::model::{MainTaskOutcome, MainTaskState, SubTaskState};
use crate::tasks::repository::task_row_decode_error;

#[cfg(test)]
mod tests;

/// 取消前置条件：只有 `queued` / `held` 可取消，且要求管理员或持有该设备的
/// reboot 权限（04 §8：`dispatching` 及之后的状态一律不可取消）。
pub fn cancel_precondition(state: SubTaskState, is_admin: bool, has_reboot: bool) -> bool {
    matches!(state, SubTaskState::Queued | SubTaskState::Held) && (is_admin || has_reboot)
}

/// 终态判定（本地口径；Task 5 聚合器落地后由其统一提供，两者必须同口径）。
pub(super) fn is_terminal_state(state: SubTaskState) -> bool {
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

/// 把状态序列折叠成 `(状态, 频数)` 列表，供 `counts_json_of` 消费。
pub(super) fn tally(states: &[SubTaskState]) -> Vec<(SubTaskState, u64)> {
    let mut totals: HashMap<SubTaskState, u64> = HashMap::new();
    for state in states {
        *totals.entry(*state).or_insert(0) += 1;
    }
    totals.into_iter().collect()
}

/// 以既有 serde 定义（snake_case，Task 2 唯一真相源）解析 `sub_tasks.state`。
/// 非法值视为 DB 损坏返回 `ControllerError::Database`（503），绝不返回 400，
/// 也绝不把非法值当合法 `Unknown` 放行；与 `SubTaskState::from_str_exact` 同源。
pub(super) fn sub_task_state_from_str(raw: &str) -> Result<SubTaskState, ControllerError> {
    SubTaskState::from_str_exact(raw).ok_or_else(task_row_decode_error)
}

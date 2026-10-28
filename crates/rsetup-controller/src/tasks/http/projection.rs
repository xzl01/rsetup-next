//! 任务 HTTP 授权投影与判定。
//!
//! 包含取消/释放锁/时间复核的前置条件判断、主任务汇总重算投影、
//! 子任务列表分页投影及取消结果过滤投影。

use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::ControllerError;
use crate::model::decimal_u64_json;
use crate::tasks::http::contract::{CancelTaskResponse, CancelledTarget, ChildEntry, ChildItem};
use crate::tasks::model::{MainTaskOutcome, MainTaskState, SubTaskState};
use crate::tasks::recovery::calculate_main_task_outcome;
use crate::time::evidence::TimeEvidence;

#[cfg(doc)]
use super::contract::TaskOperation;

/// 单元枚举 / `TimeEvidence` 的 serde 编码恒不失败（无自定义 `Serialize`），
/// 失败即为实现级缺陷，故 `expect` 而非静默降级为 `null`。
pub(super) fn strict_json<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("内部枚举与时间证据的 serde 编码不会失败")
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
    operation: super::contract::TaskOperation,
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

#[cfg(test)]
mod tests {
    use super::super::contract::TaskOperation;
    use super::super::fixtures::fixture_time_evidence;
    use super::*;
    use crate::tasks::model::EvidenceKind;

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
}

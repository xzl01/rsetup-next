use super::*;
use crate::tasks::repository::enum_wire;

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
fn counts_json_is_ten_string_keys_and_sums_batches() {
    let empty: serde_json::Value = serde_json::from_str(&empty_counts_json()).expect("json object");
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

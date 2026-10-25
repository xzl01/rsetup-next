//! 任务结果证据合并的纯逻辑测试：04 §3 合法跃迁、03 §5 板端记录词表、
//! 04 §4 主任务持久汇总（`counts_json` 十键）。

use rsetup_controller::tasks::evidence::{
    TransitionVerdict, evaluate_record_transition, recalculate_main_summary,
};
use rsetup_controller::tasks::model::{ReasonCode, SubTaskState};
use rsetup_protocol::wire::TaskRecord;
use serde_json::Value;
use uuid::Uuid;

/// 构造一条可通过 `validate_dto` 的板端 TaskRecord（03 §5 词表内的 `state`、
/// 递增的 `record_version` 由用例指定，其余字段固定合法）。
fn board_record(
    subtask_id: Uuid,
    pre_boot_id: &str,
    state: &str,
    record_version: u64,
) -> TaskRecord {
    TaskRecord {
        schema_version: 1,
        sub_task_id: subtask_id.to_string(),
        journal_epoch: Uuid::new_v4().to_string(),
        record_version,
        state: state.to_owned(),
        pre_boot_id: pre_boot_id.to_owned(),
        observed_boot_id: None,
        reason_code: None,
        updated_unix_ms: Some(1_000),
        evidence_kind: None,
    }
}

/// 由 `pre_boot_id` 确定性地派生一个与之**不同**的规范 UUIDv4 字符串（只翻转
/// time_low 区间的低位，不触碰 version/variant 位），用作 `observed_boot_id`。
fn observed_boot_of(pre_boot_id: &str) -> String {
    let base = Uuid::parse_str(pre_boot_id).expect("pre_boot_id 必须是合法 UUID");
    let mut bytes = *base.as_bytes();
    bytes[0] ^= 0x0f;
    Uuid::from_bytes(bytes).to_string()
}

/// 为记录补齐 04 §6 要求的完整成功证据：`evidence_kind=boot_transition_observed`
/// 且 `observed_boot_id` 存在并与 `pre_boot_id` 不同。
fn with_success_evidence(mut record: TaskRecord, pre_boot_id: &str) -> TaskRecord {
    record.observed_boot_id = Some(observed_boot_of(pre_boot_id));
    record.evidence_kind = Some("boot_transition_observed".into());
    record
}

#[test]
fn evaluate_record_transition_accepts_valid_accepted_progression() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    let record = board_record(subtask_id, &pre_boot_id, "accepted", 1);

    let verdict = evaluate_record_transition(
        subtask_id,
        &pre_boot_id,
        SubTaskState::Dispatching,
        0, // current record_version
        &record,
    );

    assert_eq!(
        verdict,
        TransitionVerdict::Apply {
            target_state: SubTaskState::Accepted,
            reason_code: None,
            release_lock: false,
        }
    );
}

#[test]
fn evaluate_record_transition_rejects_stale_record_version() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    let record = board_record(subtask_id, &pre_boot_id, "accepted", 1);

    // 当前库中已经是 version 2，到来的 version 1 必须被拒绝（版本回退/过期结果）
    let verdict =
        evaluate_record_transition(subtask_id, &pre_boot_id, SubTaskState::Accepted, 2, &record);

    assert!(matches!(
        verdict,
        TransitionVerdict::RejectStaleVersion {
            current: 2,
            incoming: 1
        }
    ));
}

#[test]
fn evaluate_record_transition_releases_lock_on_terminal_success() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();
    let observed_boot = Uuid::new_v4().to_string();

    let mut record = board_record(subtask_id, &pre_boot_id, "succeeded", 2);
    record.observed_boot_id = Some(observed_boot);
    record.evidence_kind = Some("boot_transition_observed".into());

    let verdict =
        evaluate_record_transition(subtask_id, &pre_boot_id, SubTaskState::Accepted, 1, &record);

    assert_eq!(
        verdict,
        TransitionVerdict::Apply {
            target_state: SubTaskState::Succeeded,
            reason_code: None,
            release_lock: true,
        }
    );
}

#[test]
fn evaluate_record_transition_rejects_succeeded_without_observed_boot_id() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    // 04 §6：跃迁合法（accepted/verifying/unknown 均可前进到 succeeded），但记录
    // 缺少 observed_boot_id 时不得判成功，必须按证据缺失拒绝。
    for from in [
        SubTaskState::Accepted,
        SubTaskState::Verifying,
        SubTaskState::Unknown,
    ] {
        let record = board_record(subtask_id, &pre_boot_id, "succeeded", 2);
        assert!(
            record.observed_boot_id.is_none(),
            "夹具必须缺 observed_boot_id"
        );
        let verdict = evaluate_record_transition(subtask_id, &pre_boot_id, from, 1, &record);
        assert_eq!(
            verdict,
            TransitionVerdict::RejectMismatch {
                reason: "missing_observed_boot_id"
            },
            "{from:?} -> succeeded 缺 observed_boot_id 必须被拒"
        );
    }
}

#[test]
fn evaluate_record_transition_rejects_succeeded_without_boot_transition_evidence() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();
    let observed_boot = observed_boot_of(&pre_boot_id);

    // observed_boot_id 已存在，但没有任何启动切换证据：boot 变化可能与本指令无关
    // （如人工重启），缺 boot_transition_observed 证据不得判成功。
    let mut record = board_record(subtask_id, &pre_boot_id, "succeeded", 2);
    record.observed_boot_id = Some(observed_boot.clone());
    let verdict = evaluate_record_transition(
        subtask_id,
        &pre_boot_id,
        SubTaskState::Verifying,
        1,
        &record,
    );
    assert_eq!(
        verdict,
        TransitionVerdict::RejectMismatch {
            reason: "missing_boot_transition_evidence"
        },
        "缺 evidence_kind 的 succeeded 必须被拒"
    );

    // evidence_kind 为词表内其他证据（如板端确认态 attempted）同样不是启动切换
    // 证据：DTO 词表校验或证据核验必须拒绝，绝不放行。
    let mut record = board_record(subtask_id, &pre_boot_id, "succeeded", 2);
    record.observed_boot_id = Some(observed_boot);
    record.evidence_kind = Some("attempted".into());
    let verdict = evaluate_record_transition(
        subtask_id,
        &pre_boot_id,
        SubTaskState::Verifying,
        1,
        &record,
    );
    assert!(
        matches!(verdict, TransitionVerdict::RejectMismatch { .. }),
        "非 boot_transition_observed 证据的 succeeded 必须被拒，实际 {verdict:?}"
    );
}

#[test]
fn evaluate_record_transition_rejects_succeeded_with_unchanged_boot_id() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    // 04 §6：observed_boot_id 与 pre_boot_id 相同 = 没有发生启动切换，证据不足，
    // 即使证据字段齐全也必须拒绝，绝不释放设备锁。
    let mut record = board_record(subtask_id, &pre_boot_id, "succeeded", 2);
    record.observed_boot_id = Some(pre_boot_id.clone());
    record.evidence_kind = Some("boot_transition_observed".into());
    let verdict =
        evaluate_record_transition(subtask_id, &pre_boot_id, SubTaskState::Accepted, 1, &record);
    assert_eq!(
        verdict,
        TransitionVerdict::RejectMismatch {
            reason: "boot_id_unchanged"
        },
        "observed_boot_id == pre_boot_id 的 succeeded 必须被拒"
    );
}

#[test]
fn evaluate_record_transition_retains_lock_on_unknown() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    let mut record = board_record(subtask_id, &pre_boot_id, "unknown", 3);
    record.reason_code = Some("RESULT_TIMEOUT".into());

    let verdict = evaluate_record_transition(
        subtask_id,
        &pre_boot_id,
        SubTaskState::Verifying,
        2,
        &record,
    );

    // 04 §3：unknown 属于终态，但保留设备锁（release_lock = false）
    assert_eq!(
        verdict,
        TransitionVerdict::Apply {
            target_state: SubTaskState::Unknown,
            reason_code: Some(ReasonCode::ResultTimeout),
            release_lock: false,
        }
    );
}

#[test]
fn evaluate_record_transition_maps_board_attempted_to_accepted() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    // 03 §5：`attempted` 是板端可上报的合法状态，与 `accepted` 同属派发确认态。
    // 它不在中控十态内，必须按 accepted 收敛，绝不报 unknown_state_enum。
    for from in [SubTaskState::Queued, SubTaskState::Dispatching] {
        let record = board_record(subtask_id, &pre_boot_id, "attempted", 5);
        let verdict = evaluate_record_transition(subtask_id, &pre_boot_id, from, 4, &record);
        assert_eq!(
            verdict,
            TransitionVerdict::Apply {
                target_state: SubTaskState::Accepted,
                reason_code: None,
                release_lock: false,
            },
            "attempted 必须从 {from:?} 收敛为 accepted"
        );
    }
}

#[test]
fn evaluate_record_transition_still_rejects_states_outside_both_vocabularies() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    // `running` 既非十态、也非板端词表；`verifying` 只属中控十态（由中控自己推进），
    // 不在板端 03 §5 词表（accepted/attempted/succeeded/failed/unknown）内：
    // 两者都在 DTO 契约校验先失败，仍归 RejectMismatch，绝不放行。
    for state in ["running", "verifying"] {
        let record = board_record(subtask_id, &pre_boot_id, state, 2);
        let verdict = evaluate_record_transition(
            subtask_id,
            &pre_boot_id,
            SubTaskState::Dispatching,
            1,
            &record,
        );
        assert!(
            matches!(verdict, TransitionVerdict::RejectMismatch { .. }),
            "{state} 记录必须被拒，实际 {verdict:?}"
        );
    }
}

#[test]
fn evaluate_record_transition_rejects_dispatching_direct_to_succeeded() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    // 04 §3：首次获得的记录已是终态，仍必须先经 accepted 再到 verifying，
    // 不允许 dispatching 直跳 succeeded。
    let record = board_record(subtask_id, &pre_boot_id, "succeeded", 1);
    let verdict = evaluate_record_transition(
        subtask_id,
        &pre_boot_id,
        SubTaskState::Dispatching,
        0,
        &record,
    );
    assert_eq!(
        verdict,
        TransitionVerdict::InvalidStateTransition {
            from: SubTaskState::Dispatching,
            to: SubTaskState::Succeeded,
        }
    );
}

#[test]
fn evaluate_record_transition_rejects_pre_dispatch_shortcuts() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    // queued / held 是派发前状态：记录证据最多确认到 accepted 或 failed，
    // 不得直跳 verifying / succeeded / unknown。
    let illegal = [
        ("queued", SubTaskState::Queued, "verifying"),
        ("queued", SubTaskState::Queued, "succeeded"),
        ("queued", SubTaskState::Queued, "unknown"),
        ("held", SubTaskState::Held, "verifying"),
        ("held", SubTaskState::Held, "succeeded"),
        ("held", SubTaskState::Held, "unknown"),
    ];
    for (label, from, state) in illegal {
        let record = board_record(subtask_id, &pre_boot_id, state, 2);
        let verdict = evaluate_record_transition(subtask_id, &pre_boot_id, from, 1, &record);
        match verdict {
            // succeeded/unknown 是两侧词表共有的状态名，直跳必须是状态机裁决；
            // verifying 只属中控十态、不在板端 03 §5 词表内，契约校验即失败。
            TransitionVerdict::InvalidStateTransition { from: f, to } => {
                assert_eq!(f, from, "{label} 的 from 不符");
                assert!(
                    matches!(
                        to,
                        SubTaskState::Verifying | SubTaskState::Succeeded | SubTaskState::Unknown
                    ),
                    "{label} 直跳目标异常：{to:?}"
                );
            }
            TransitionVerdict::RejectMismatch { reason } => {
                assert_eq!(state, "verifying", "{label} 直跳被误拒：{reason}");
            }
            other => panic!("{label} -> {state} 必须被拒绝，实际 {other:?}"),
        }
    }
}

#[test]
fn evaluate_record_transition_accepts_legal_forward_matrix() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    let legal: &[(SubTaskState, &str)] = &[
        (SubTaskState::Queued, "accepted"),
        (SubTaskState::Queued, "failed"),
        (SubTaskState::Held, "accepted"),
        (SubTaskState::Held, "failed"),
        (SubTaskState::Dispatching, "accepted"),
        (SubTaskState::Dispatching, "failed"),
        (SubTaskState::Dispatching, "unknown"),
        (SubTaskState::Accepted, "succeeded"),
        (SubTaskState::Accepted, "failed"),
        (SubTaskState::Accepted, "unknown"),
        (SubTaskState::Verifying, "succeeded"),
        (SubTaskState::Verifying, "failed"),
        (SubTaskState::Verifying, "unknown"),
        (SubTaskState::Unknown, "succeeded"),
        (SubTaskState::Unknown, "failed"),
    ];
    for (from, state) in legal {
        let record = board_record(subtask_id, &pre_boot_id, state, 2);
        // 04 §6：succeeded 跃迁必须携带完整启动切换证据，矩阵夹具统一补齐。
        let record = if *state == "succeeded" {
            with_success_evidence(record, &pre_boot_id)
        } else {
            record
        };
        let verdict = evaluate_record_transition(subtask_id, &pre_boot_id, *from, 1, &record);
        assert!(
            matches!(verdict, TransitionVerdict::Apply { .. }),
            "{from:?} -> {state} 必须合法，实际 {verdict:?}"
        );
    }
}

#[test]
fn evaluate_record_transition_rejects_reverse_and_terminal_rewrites() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();

    // 批次终态不可反向；unknown 不得回到执行中或再次转 unknown。
    let illegal: &[(SubTaskState, &str)] = &[
        (SubTaskState::Succeeded, "failed"),
        (SubTaskState::Succeeded, "accepted"),
        (SubTaskState::Failed, "succeeded"),
        (SubTaskState::Cancelled, "accepted"),
        (SubTaskState::Expired, "succeeded"),
        (SubTaskState::Unknown, "accepted"),
        (SubTaskState::Unknown, "unknown"),
        (SubTaskState::Accepted, "queued"),
        (SubTaskState::Verifying, "accepted"),
    ];
    for (from, state) in illegal {
        let record = board_record(subtask_id, &pre_boot_id, state, 9);
        let verdict = evaluate_record_transition(subtask_id, &pre_boot_id, *from, 8, &record);
        assert!(
            matches!(
                verdict,
                TransitionVerdict::InvalidStateTransition { .. }
                    | TransitionVerdict::RejectMismatch { .. }
            ),
            "{from:?} -> {state} 必须被拒绝，实际 {verdict:?}"
        );
    }
}

#[test]
fn evaluate_record_transition_rejects_binding_mismatches() {
    let subtask_id = Uuid::new_v4();
    let pre_boot_id = Uuid::new_v4().to_string();
    let other_pre_boot = Uuid::new_v4().to_string();

    // 错 pre_boot（跨 boot 的迟到记录）
    let record = board_record(subtask_id, &other_pre_boot, "accepted", 2);
    assert!(matches!(
        evaluate_record_transition(
            subtask_id,
            &pre_boot_id,
            SubTaskState::Dispatching,
            1,
            &record
        ),
        TransitionVerdict::RejectMismatch { .. }
    ));

    // 错 sub_task_id（串任务）
    let record = board_record(Uuid::new_v4(), &pre_boot_id, "accepted", 2);
    assert!(matches!(
        evaluate_record_transition(
            subtask_id,
            &pre_boot_id,
            SubTaskState::Dispatching,
            1,
            &record
        ),
        TransitionVerdict::RejectMismatch { .. }
    ));

    // nil journal_epoch（日志纪元不可信）同样按不符拒绝
    let mut record = board_record(subtask_id, &pre_boot_id, "accepted", 2);
    record.journal_epoch = Uuid::nil().to_string();
    assert!(matches!(
        evaluate_record_transition(
            subtask_id,
            &pre_boot_id,
            SubTaskState::Dispatching,
            1,
            &record
        ),
        TransitionVerdict::RejectMismatch { .. }
    ));
}

#[test]
fn recalculate_main_summary_writes_ten_key_counts() {
    let states = [
        SubTaskState::Succeeded,
        SubTaskState::Succeeded,
        SubTaskState::Failed,
        SubTaskState::Unknown,
    ];

    let (state, outcome, counts_json) = recalculate_main_summary(&states).expect("summary");
    assert_eq!(state, "completed");
    assert_eq!(outcome.as_deref(), Some("unknown"));

    let Value::Object(counts) = serde_json::from_str::<Value>(&counts_json).expect("json") else {
        panic!("counts_json 必须是 JSON 对象：{counts_json}");
    };
    let keys = [
        "queued",
        "held",
        "dispatching",
        "accepted",
        "verifying",
        "succeeded",
        "failed",
        "unknown",
        "cancelled",
        "expired",
    ];
    assert_eq!(counts.len(), keys.len(), "counts_json 必须恰好十键");
    let mut total = 0_u64;
    for key in keys {
        let raw = counts
            .get(key)
            .unwrap_or_else(|| panic!("counts_json 缺键 {key}：{counts_json}"));
        let n: u64 = raw
            .as_str()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("{key} 必须是十进制字符串：{raw}"));
        total += n;
    }
    assert_eq!(total, states.len() as u64, "十键频数之和必须等于子任务总数");
    assert_eq!(counts["succeeded"], Value::from("2"));
    assert_eq!(counts["failed"], Value::from("1"));
    assert_eq!(counts["unknown"], Value::from("1"));
    assert_eq!(counts["queued"], Value::from("0"));
}

#[test]
fn recalculate_main_summary_keeps_outcome_null_while_running() {
    let states = [SubTaskState::Accepted, SubTaskState::Queued];
    let (state, outcome, counts_json) = recalculate_main_summary(&states).expect("summary");
    assert_eq!(state, "running");
    assert_eq!(outcome, None, "非 completed 时 outcome 必须为 NULL");

    let Value::Object(counts) = serde_json::from_str::<Value>(&counts_json).expect("json") else {
        panic!("counts_json 必须是 JSON 对象");
    };
    // 证据推进后主任务计数必须跟着走：accepted 1、queued 1，提交时的旧口径已覆盖
    assert_eq!(counts["accepted"], Value::from("1"));
    assert_eq!(counts["queued"], Value::from("1"));
    assert_eq!(counts["succeeded"], Value::from("0"));
}

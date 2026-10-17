use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ControllerError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubTaskState {
    Queued,
    Held,
    Dispatching,
    Accepted,
    Verifying,
    Succeeded,
    Failed,
    Unknown,
    Cancelled,
    Expired,
}

pub const SUBTASK_STATE_COUNT: usize = 10;

/// 结果证据种类（04 §6）：六值 wire 严格 snake_case 串（`attempted`/
/// `boot_transition_observed`/`execution_rejected`/`os_error`/`journal_lost`/
/// `none`）。模型定义，serde 自 Step 1 即正确；`Option::<EvidenceKind>::None`
/// 为 JSON `null`（证据未产生），`Some(EvidenceKind::None)` 为串 `"none"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Attempted,
    BootTransitionObserved,
    ExecutionRejected,
    OsError,
    JournalLost,
    None,
}

/// 失败/终止原因码（04 §3）：十二值 wire 严格 SCREAMING_SNAKE_CASE
/// （`PERMISSION_DENIED`/`DEVICE_OFFLINE`/`CAPABILITY_UNAVAILABLE`/`DEVICE_BUSY`/
/// `QUEUE_EXPIRED`/`TIME_UNCERTAIN`/`EXECUTION_REJECTED`/`OS_ERROR`/
/// `RESULT_TIMEOUT`/`JOURNAL_LOST`/`PROTOCOL_MISMATCH`/`USER_CANCELLED`），
/// 与 04 §3 十二码一一对应，不新增原因/权限/API 字段。
/// 模型定义，serde 自 Step 1 即正确；`Option<ReasonCode>::None` 为 JSON `null`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReasonCode {
    PermissionDenied,
    DeviceOffline,
    CapabilityUnavailable,
    DeviceBusy,
    QueueExpired,
    TimeUncertain,
    ExecutionRejected,
    OsError,
    ResultTimeout,
    JournalLost,
    ProtocolMismatch,
    UserCancelled,
}

/// 主任务状态（04 §4）：四值 wire 严格 snake_case（`queued`/`running`/
/// `held`/`completed`），由子任务批次汇总计算；模型定义，serde 自 Step 1 即正确。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTaskState {
    Queued,
    Running,
    Held,
    Completed,
}

/// 主任务 outcome（04 §4）：五值 wire 严格 snake_case（`success`/`cancelled`/
/// `unknown`/`partial`/`failed`）；“非 completed 为 null”是上层配对规则，
/// 本枚举 serde 不伪证明配对。模型定义，serde 自 Step 1 即正确。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTaskOutcome {
    Success,
    Cancelled,
    Unknown,
    Partial,
    Failed,
}

/// 分派意图种类：单值 wire 严格 snake_case（`dispatch`）。
/// 模型定义，serde 自 Step 1 即正确；`TaskIntent` 不另设 `kind` 字段。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentKind {
    Dispatch,
}

/// 分派意图（04 §5.3 观测语义 + Task 5 意图后发送/commit-before-execute）：
/// 全部 8 字段与 Interfaces 既定一致。`ticket_digest`/`request_digest` 是
/// 32B 内部私有持久化证据（摘要而非明文 token），不得进入 HTTP 投影、
/// 普通日志或 debug dump；故仅派生 `Clone, PartialEq, Eq, Serialize,
/// Deserialize`——**不派生 Debug**，避免摘要随 `{intent:?}` 外泄；
/// 若消费者确需 Debug，回报即可，本切片不擅写全值 Debug。
/// 各字段类型皆 Eq（`Uuid`/`String`/`[u8; 32]`/`i64`，无浮点），`Eq`
/// 为字段类型依据的真 Eq，非伪 Eq。模型定义，serde 自 Step 1 即正确；
/// 本定义不证明 commit-before-execute、at-most-once/CAS 或 DB 恢复——
/// 真行为留 Task 4/5 受门禁验证。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskIntent {
    pub subtask_id: Uuid,
    pub controller_epoch: Uuid,
    pub session_epoch: Uuid,
    pub stream_epoch: Uuid,
    pub pre_boot_id: String,
    pub ticket_digest: [u8; 32],
    pub request_digest: [u8; 32],
    pub observe_deadline_ms: i64,
}

/// 结果证据（04 §6 核验输入）：内部持久化/核验结构，8 字段与 Interfaces
/// 既定一致；`expected_pre_boot` 不作字段（Task 5 核验入参，从数据库
/// `sub_tasks.pre_boot_id` 读取传入），不加 `TimeEvidence`/HTTP 字段；不通过
/// HTTP 投影完整对象（Task 6 `ChildItem` 仅 `result_evidence` 种类）；不证明
/// boot 转换/授权可信/at-most-once。字段类型皆 Eq（无浮点），`Eq` 为真 Eq。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEvidence {
    pub subtask_id: Uuid,
    pub journal_epoch: Uuid,
    pub record_version: u64,
    pub observed_boot_id: String,
    pub pre_boot_id: String,
    pub current_boot_id: String,
    pub evidence_kind: EvidenceKind,
    pub reliable_attempted: bool,
}

/// prepare 票据摘要引用（`boot_id`/`valid_for_ms`）：字段名/语义与 transport 线 `RebootTicket` 对应，
/// 但数值宽度不同——`RebootTicket.valid_for_ms` 为 `u32`，此处为 `u64`：wire→内部 `u64::from(u32)` 无损，
/// 内部→wire 必须 `u32::try_from` fail-closed（该映射实现仍前置未完成、不得 `as` 窄化）；
/// `u64::MAX` 纯 serde 例只证明内部 storage、不证明 wire 可携带；`Token` 字段不由本结构导出，codec 仍未批。
/// **只是**内部 prepare 引用，**不代表**获批 Protobuf/wire codec。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecordRef {
    pub boot_id: String,
    pub valid_for_ms: u64,
}

/// 终态集合（04 §3）：succeeded/failed/unknown/cancelled/expired。
pub fn is_terminal(state: SubTaskState) -> bool {
    matches!(
        state,
        SubTaskState::Succeeded
            | SubTaskState::Failed
            | SubTaskState::Unknown
            | SubTaskState::Cancelled
            | SubTaskState::Expired
    )
}

/// 设备锁持有（04 §3）：queued/held/dispatching/accepted/verifying 执行中持锁；
/// unknown 是终态但【保留】设备锁直到核实或管理员释放；
/// succeeded/failed/cancelled/expired 释放/未取得。
pub fn holds_device_lock(state: SubTaskState) -> bool {
    matches!(
        state,
        SubTaskState::Queued
            | SubTaskState::Held
            | SubTaskState::Dispatching
            | SubTaskState::Accepted
            | SubTaskState::Verifying
            | SubTaskState::Unknown
    )
}

/// 04 §3 合法转移表（纯函数）。`Dispatching → Cancelled` 不属合法边（C2）：
/// 04 §8 取消只有 queued/held 可原子改 cancelled；intent 后（dispatching）仅
/// accepted/verifying/failed/unknown 合法，可证 execute 未被接受/执行或明确
/// 拒绝时收敛 failed（而非 cancelled），不自动补发；`Dispatching → Succeeded`
/// 永久禁止；unknown 只能由后续有效证据收敛为 succeeded/failed，绝不回到执行中。
pub fn transition_allowed(from: SubTaskState, to: SubTaskState) -> bool {
    use SubTaskState::*;
    match (from, to) {
        (Queued, Dispatching)
        | (Queued, Failed)
        | (Queued, Cancelled)
        | (Queued, Expired)
        | (Queued, Held) => true,
        (Held, Queued) | (Held, Expired) | (Held, Cancelled) | (Held, Failed) => true,
        (Dispatching, Accepted)
        | (Dispatching, Verifying)
        | (Dispatching, Failed)
        | (Dispatching, Unknown) => true,
        (Accepted, Verifying)
        | (Accepted, Succeeded)
        | (Accepted, Failed)
        | (Accepted, Unknown) => true,
        (Verifying, Succeeded) | (Verifying, Failed) | (Verifying, Unknown) => true,
        // unknown 只能由后续有效证据收敛，绝不回到执行中
        (Unknown, Succeeded) | (Unknown, Failed) => true,
        _ => false,
    }
}

/// 状态推进纯函数：同 state 幂等返回；合法边 `Ok(desired)`；
/// 终态出发的非法边 `RevisionConflict`（并发覆盖信号）；其余 `InvalidArgument`。
pub fn advance_subtask(
    current: SubTaskState,
    desired: SubTaskState,
) -> Result<SubTaskState, ControllerError> {
    if current == desired {
        return Ok(current);
    }
    if transition_allowed(current, desired) {
        Ok(desired)
    } else if is_terminal(current) {
        Err(ControllerError::RevisionConflict)
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtask_state_serializes_snake_case_and_covers_ten_states() {
        let all = [
            SubTaskState::Queued,
            SubTaskState::Held,
            SubTaskState::Dispatching,
            SubTaskState::Accepted,
            SubTaskState::Verifying,
            SubTaskState::Succeeded,
            SubTaskState::Failed,
            SubTaskState::Unknown,
            SubTaskState::Cancelled,
            SubTaskState::Expired,
        ];
        assert_eq!(all.len(), SUBTASK_STATE_COUNT);
        assert_eq!(
            serde_json::to_string(&SubTaskState::Dispatching).unwrap(),
            "\"dispatching\""
        );
        assert_eq!(
            serde_json::to_string(&SubTaskState::Unknown).unwrap(),
            "\"unknown\""
        );
    }

    #[test]
    fn reason_code_serializes_screaming_snake_case() {
        // 04 §3 + 02 §1：十二值严格 SCREAMING_SNAKE_CASE roundtrip；枚举与 serde 派生
        // 属于模型定义，Step 1 即应 PASS，不是行为 RED。
        let all: [(ReasonCode, &str); 12] = [
            (ReasonCode::PermissionDenied, "PERMISSION_DENIED"),
            (ReasonCode::DeviceOffline, "DEVICE_OFFLINE"),
            (ReasonCode::CapabilityUnavailable, "CAPABILITY_UNAVAILABLE"),
            (ReasonCode::DeviceBusy, "DEVICE_BUSY"),
            (ReasonCode::QueueExpired, "QUEUE_EXPIRED"),
            (ReasonCode::TimeUncertain, "TIME_UNCERTAIN"),
            (ReasonCode::ExecutionRejected, "EXECUTION_REJECTED"),
            (ReasonCode::OsError, "OS_ERROR"),
            (ReasonCode::ResultTimeout, "RESULT_TIMEOUT"),
            (ReasonCode::JournalLost, "JOURNAL_LOST"),
            (ReasonCode::ProtocolMismatch, "PROTOCOL_MISMATCH"),
            (ReasonCode::UserCancelled, "USER_CANCELLED"),
        ];
        for (code, wire) in all {
            assert_eq!(serde_json::to_value(code).unwrap(), serde_json::json!(wire));
            assert_eq!(
                serde_json::from_value::<ReasonCode>(serde_json::json!(wire)).unwrap(),
                code
            );
        }
        // `Option<ReasonCode>`：`Some` 为 wire 串，`None`（无原因）为 JSON `null`。
        assert_eq!(
            serde_json::to_value(Some(ReasonCode::DeviceBusy)).unwrap(),
            serde_json::json!("DEVICE_BUSY")
        );
        let none: Option<ReasonCode> = None;
        assert_eq!(serde_json::to_value(none).unwrap(), serde_json::Value::Null);
        // 未知枚举值、大小写不符（小写 snake/PascalCase）与旧板端/中控来源词一律拒绝。
        for bad in [
            "permission_denied",
            "PermissionDenied",
            "REBOOTED",
            "board",
            "controller",
            "success",
        ] {
            assert!(
                serde_json::from_value::<ReasonCode>(serde_json::json!(bad)).is_err(),
                "{bad:?} 不在十二值词表，必须拒绝"
            );
        }
        // number/boolean/null（非 Option 枚举）与任意 object 形状一律拒绝：
        // `reason_code` 为纯字符串枚举，不是 token 或其摘要（02 §1），新字段同样拒绝。
        assert!(serde_json::from_value::<ReasonCode>(serde_json::json!(1)).is_err());
        assert!(serde_json::from_value::<ReasonCode>(serde_json::json!(true)).is_err());
        assert!(serde_json::from_value::<ReasonCode>(serde_json::Value::Null).is_err());
        assert!(
            serde_json::from_value::<ReasonCode>(serde_json::json!({
                "reason_code": "PERMISSION_DENIED"
            }))
            .is_err()
        );
    }

    #[test]
    fn result_evidence_kind_serializes_six_snake_case_values_and_distinguishes_null() {
        // 04 §6 + 02 §1：六值严格 snake_case；枚举与 serde 派生属于模型定义，
        // 本用例在 Step 1 即应 GREEN，不是行为 RED。
        let all: [(EvidenceKind, &str); 6] = [
            (EvidenceKind::Attempted, "attempted"),
            (
                EvidenceKind::BootTransitionObserved,
                "boot_transition_observed",
            ),
            (EvidenceKind::ExecutionRejected, "execution_rejected"),
            (EvidenceKind::OsError, "os_error"),
            (EvidenceKind::JournalLost, "journal_lost"),
            (EvidenceKind::None, "none"),
        ];
        for (kind, wire) in all {
            assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(wire));
            assert_eq!(
                serde_json::from_value::<EvidenceKind>(serde_json::json!(wire)).unwrap(),
                kind
            );
        }
        // `Option<EvidenceKind>`：`Some(EvidenceKind::None)` 是串 `"none"`，
        // `Option::<EvidenceKind>::None`（证据未产生）是 JSON `null`，两者不得混淆。
        assert_eq!(
            serde_json::to_value(Some(EvidenceKind::None)).unwrap(),
            serde_json::json!("none")
        );
        let not_produced: Option<EvidenceKind> = None;
        assert_eq!(
            serde_json::to_value(not_produced).unwrap(),
            serde_json::Value::Null
        );
        // 未知枚举值、大小写不符、PascalCase 与旧板端/中控来源词一律拒绝。
        for bad in [
            "ATTEMPTED",
            "BootTransitionObserved",
            "attempt",
            "rebooted",
            "board",
            "controller",
            "success",
        ] {
            assert!(
                serde_json::from_value::<EvidenceKind>(serde_json::json!(bad)).is_err(),
                "{bad:?} 不在六值词表，必须拒绝"
            );
        }
        // number/boolean/null（非 Option 枚举）与任意 object 形状一律拒绝：
        // `result_evidence` 不暴露 `execution_token` 或其摘要（02 §1），新字段同样拒绝。
        assert!(serde_json::from_value::<EvidenceKind>(serde_json::json!(1)).is_err());
        assert!(serde_json::from_value::<EvidenceKind>(serde_json::json!(true)).is_err());
        assert!(serde_json::from_value::<EvidenceKind>(serde_json::Value::Null).is_err());
        assert!(
            serde_json::from_value::<EvidenceKind>(serde_json::json!({
                "kind": "boot_transition_observed",
                "execution_token_digest": "ab".repeat(32)
            }))
            .is_err()
        );
    }

    #[test]
    fn main_task_model_serializes_four_states_and_five_outcomes() {
        // 04 §4：MainTaskState 四值 + MainTaskOutcome 五值严格 snake_case；
        // 枚举与 serde 派生属于模型定义，Step 1 即应 GREEN，不是行为 RED。
        let states: [(MainTaskState, &str); 4] = [
            (MainTaskState::Queued, "queued"),
            (MainTaskState::Running, "running"),
            (MainTaskState::Held, "held"),
            (MainTaskState::Completed, "completed"),
        ];
        for (state, wire) in states {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(wire)
            );
            assert_eq!(
                serde_json::from_value::<MainTaskState>(serde_json::json!(wire)).unwrap(),
                state
            );
        }
        let outcomes: [(MainTaskOutcome, &str); 5] = [
            (MainTaskOutcome::Success, "success"),
            (MainTaskOutcome::Cancelled, "cancelled"),
            (MainTaskOutcome::Unknown, "unknown"),
            (MainTaskOutcome::Partial, "partial"),
            (MainTaskOutcome::Failed, "failed"),
        ];
        for (outcome, wire) in outcomes {
            assert_eq!(
                serde_json::to_value(outcome).unwrap(),
                serde_json::json!(wire)
            );
            assert_eq!(
                serde_json::from_value::<MainTaskOutcome>(serde_json::json!(wire)).unwrap(),
                outcome
            );
        }
        // `Option<MainTaskOutcome>`：`Some` 为 wire 串，`None`（非 completed、无
        // outcome）为 JSON `null`；“非 completed 为 null”是上层配对规则，
        // 本枚举 serde 单独不伪证明配对。
        assert_eq!(
            serde_json::to_value(Some(MainTaskOutcome::Partial)).unwrap(),
            serde_json::json!("partial")
        );
        let not_completed: Option<MainTaskOutcome> = None;
        assert_eq!(
            serde_json::to_value(not_completed).unwrap(),
            serde_json::Value::Null
        );
        // MainTaskState 拒绝子任务状态词（04 §4 主任务四态与 04 §3 子任务十态
        // 是两套词表；`unknown` 只是子任务状态，不是主任务状态）。
        for bad in ["dispatching", "accepted", "succeeded", "unknown"] {
            assert!(
                serde_json::from_value::<MainTaskState>(serde_json::json!(bad)).is_err(),
                "{bad:?} 不在主任务四值词表，必须拒绝"
            );
        }
        // MainTaskOutcome 拒绝未知词（子任务状态词 `succeeded` 与主任务状态词
        // `queued`/`running`/`completed` 均非 outcome 词）。
        for bad in ["succeeded", "queued", "running", "completed"] {
            assert!(
                serde_json::from_value::<MainTaskOutcome>(serde_json::json!(bad)).is_err(),
                "{bad:?} 不在主任务 outcome 五值词表，必须拒绝"
            );
        }
    }

    #[test]
    fn dispatch_intent_model_roundtrips_without_expanding_http_contract() {
        // `IntentKind` 单值严格 snake_case；枚举与 serde 派生属于模型定义，
        // 本用例在 Step 1 即应 PASS，不是行为 RED。
        assert_eq!(
            serde_json::to_value(IntentKind::Dispatch).unwrap(),
            serde_json::json!("dispatch")
        );
        assert_eq!(
            serde_json::from_value::<IntentKind>(serde_json::json!("dispatch")).unwrap(),
            IntentKind::Dispatch
        );
        // 未知值、大小写不符与 object 形状一律拒绝（与 `ReasonCode`/`EvidenceKind` 同纪律）。
        for bad in [
            "DISPATCH",
            "dispatching",
            "retry",
            "board",
            "controller",
            "success",
        ] {
            assert!(
                serde_json::from_value::<IntentKind>(serde_json::json!(bad)).is_err(),
                "{bad:?} 不在单值词表，必须拒绝"
            );
        }
        assert!(
            serde_json::from_value::<IntentKind>(serde_json::json!({ "kind": "dispatch" }))
                .is_err()
        );
        // `TaskIntent`：合成 fixture（8 字段全给；Uuid 为合成值，无真实
        // token/credentials），进程内 `to_value`/`from_value` 往返
        // （内存持久化语义，不冒称可以公网输出）。
        let expected = TaskIntent {
            subtask_id: Uuid::parse_str("0f0f0f0f-0000-4000-8000-000000000001").unwrap(),
            controller_epoch: Uuid::parse_str("0f0f0f0f-0000-4000-8000-000000000002").unwrap(),
            session_epoch: Uuid::parse_str("0f0f0f0f-0000-4000-8000-000000000003").unwrap(),
            stream_epoch: Uuid::parse_str("0f0f0f0f-0000-4000-8000-000000000004").unwrap(),
            pre_boot_id: "synthetic-pre-boot-0001".to_owned(),
            ticket_digest: [7u8; 32],
            request_digest: [9u8; 32],
            observe_deadline_ms: 300_000,
        };
        let value = serde_json::to_value(&expected).unwrap();
        let object = value.as_object().expect("TaskIntent 序列化为 8 key object");
        assert_eq!(object.len(), 8);
        for key in [
            "subtask_id",
            "controller_epoch",
            "session_epoch",
            "stream_epoch",
            "pre_boot_id",
            "ticket_digest",
            "request_digest",
            "observe_deadline_ms",
        ] {
            assert!(object.contains_key(key), "缺少既定 key：{key}");
        }
        // 两个 32B 摘要在 JSON 内为 32 元素数组（内部持久化证据，非 HTTP 投影）。
        assert_eq!(
            object["ticket_digest"].as_array().map(|a| a.len()),
            Some(32)
        );
        assert_eq!(
            object["request_digest"].as_array().map(|a| a.len()),
            Some(32)
        );
        let decoded: TaskIntent = serde_json::from_value(value).unwrap();
        // 用 `assert!(decoded == expected)`（PartialEq）而非 `assert_eq!`：
        // `TaskIntent` 有意不派生 Debug，摘要不随失败消息外泄。
        assert!(decoded == expected);
        // 本用例只证明模型定义与 serde 形状；不证明 commit-before-execute、
        // at-most-once/CAS 或 DB 恢复——真行为留 Task 4/5 受门禁未验证。
    }

    #[test]
    fn task_evidence_and_record_ref_internal_models_roundtrip() {
        // `TaskEvidence`/`TaskRecordRef` 的 serde 属于模型定义，Step 1 即应
        // PASS，不是行为 RED，也不是缺函数编译错误。
        let expected = TaskEvidence {
            subtask_id: Uuid::parse_str("0f0f0f0f-0000-4000-8000-000000000101").unwrap(),
            journal_epoch: Uuid::parse_str("0f0f0f0f-0000-4000-8000-000000000102").unwrap(),
            record_version: 7,
            observed_boot_id: "synthetic-boot-observed-0001".to_owned(),
            pre_boot_id: "synthetic-pre-boot-0002".to_owned(),
            current_boot_id: "synthetic-boot-current-0003".to_owned(),
            evidence_kind: EvidenceKind::BootTransitionObserved,
            reliable_attempted: true,
        };
        let value = serde_json::to_value(&expected).unwrap();
        let object = value
            .as_object()
            .expect("TaskEvidence 序列化为 8 key object");
        assert_eq!(object.len(), 8);
        for key in [
            "subtask_id",
            "journal_epoch",
            "record_version",
            "observed_boot_id",
            "pre_boot_id",
            "current_boot_id",
            "evidence_kind",
            "reliable_attempted",
        ] {
            assert!(object.contains_key(key), "缺少既定 key：{key}");
        }
        // key 集合与预期类型逐一相等：Uuid/String 为串、u64 为数字、
        // EvidenceKind 为严格 snake_case 串、bool 为布尔。
        for key in [
            "subtask_id",
            "journal_epoch",
            "observed_boot_id",
            "pre_boot_id",
            "current_boot_id",
        ] {
            assert!(object[key].is_string(), "{key} 必须为字符串");
        }
        assert!(
            object["record_version"].is_u64(),
            "record_version 必须为数字（u64）"
        );
        assert_eq!(
            object["evidence_kind"],
            serde_json::json!("boot_transition_observed")
        );
        assert!(object["reliable_attempted"].is_boolean());
        let decoded: TaskEvidence = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded, expected);
        // 8 字段全必含：缺任一 key 即拒绝。
        for key in [
            "subtask_id",
            "journal_epoch",
            "record_version",
            "observed_boot_id",
            "pre_boot_id",
            "current_boot_id",
            "evidence_kind",
            "reliable_attempted",
        ] {
            let mut missing = object.clone();
            missing.remove(key);
            assert!(
                serde_json::from_value::<TaskEvidence>(serde_json::Value::Object(missing)).is_err(),
                "缺 {key} 必须拒绝"
            );
        }
        // 非法 `EvidenceKind`（未知值/大小写不符/非串）一律拒绝（与
        // `ReasonCode`/`IntentKind` 同纪律）。
        for bad in [
            serde_json::json!("RELIABLE"),
            serde_json::json!("boot_transition"),
            serde_json::json!(1),
            serde_json::json!(true),
        ] {
            let mut v = serde_json::to_value(&expected).unwrap();
            v["evidence_kind"] = bad;
            assert!(
                serde_json::from_value::<TaskEvidence>(v).is_err(),
                "非法 evidence_kind 必须拒绝"
            );
        }
        // `record_version` 为 u64：负数与小数按类型语义拒绝（不硬加新 schema 字段）。
        for bad in [serde_json::json!(-1), serde_json::json!(1.5)] {
            let mut v = serde_json::to_value(&expected).unwrap();
            v["record_version"] = bad;
            assert!(
                serde_json::from_value::<TaskEvidence>(v).is_err(),
                "负/小数 record_version 必须拒绝"
            );
        }
        // `TaskRecordRef`：prepare 票据摘要引用（2 字段全给、合成值、无
        // token/真实凭据），`valid_for_ms` 为 u64，`u64::MAX` 边界同形支持。
        let ref_expected = TaskRecordRef {
            boot_id: "synthetic-boot-prepare-0004".to_owned(),
            valid_for_ms: 300_000,
        };
        let ref_value = serde_json::to_value(&ref_expected).unwrap();
        let ref_object = ref_value
            .as_object()
            .expect("TaskRecordRef 序列化为 2 key object");
        assert_eq!(ref_object.len(), 2);
        for key in ["boot_id", "valid_for_ms"] {
            assert!(ref_object.contains_key(key), "缺少既定 key：{key}");
        }
        assert!(ref_object["boot_id"].is_string());
        assert!(ref_object["valid_for_ms"].is_u64());
        let ref_decoded: TaskRecordRef = serde_json::from_value(ref_value).unwrap();
        assert_eq!(ref_decoded, ref_expected);
        let ref_max = TaskRecordRef {
            boot_id: "synthetic-boot-prepare-0005".to_owned(),
            valid_for_ms: u64::MAX,
        };
        assert_eq!(
            serde_json::from_value::<TaskRecordRef>(serde_json::to_value(&ref_max).unwrap())
                .unwrap(),
            ref_max
        );
        // 缺字段与负数按同纪律拒绝（u64 类型语义）。
        for v in [
            serde_json::json!({ "boot_id": "synthetic-boot-prepare-0006" }),
            serde_json::json!({ "boot_id": "synthetic-boot-prepare-0007", "valid_for_ms": -1 }),
        ] {
            assert!(
                serde_json::from_value::<TaskRecordRef>(v.clone()).is_err(),
                "{v} 必须拒绝"
            );
        }
        // 本用例只证明模型定义与 serde 形状（内部持久化/核验输入）；不证明
        // boot 转换/授权可信/at-most-once，`TaskRecordRef` 亦非获批 wire codec——真行为留 Task 5 受门禁验证。
    }

    #[test]
    fn terminal_states_and_device_lock_ownership_follow_lifecycle_table() {
        for terminal in [
            SubTaskState::Succeeded,
            SubTaskState::Failed,
            SubTaskState::Unknown,
            SubTaskState::Cancelled,
            SubTaskState::Expired,
        ] {
            assert!(is_terminal(terminal), "{terminal:?} must be terminal");
        }
        for live in [
            SubTaskState::Queued,
            SubTaskState::Held,
            SubTaskState::Dispatching,
            SubTaskState::Accepted,
            SubTaskState::Verifying,
        ] {
            assert!(!is_terminal(live), "{live:?} must not be terminal");
        }
        // 04 §3：unknown 是终态但【保留】设备锁，直到核实或管理员释放。
        assert!(holds_device_lock(SubTaskState::Unknown));
        assert!(holds_device_lock(SubTaskState::Dispatching));
        assert!(holds_device_lock(SubTaskState::Verifying));
        assert!(!holds_device_lock(SubTaskState::Succeeded));
        assert!(!holds_device_lock(SubTaskState::Cancelled));
        assert!(!holds_device_lock(SubTaskState::Expired));
        // 04 §3 表格明确指出 Failed 是“释放/未取得”，holds_device_lock 必须为 false
        assert!(!holds_device_lock(SubTaskState::Failed));
    }

    #[test]
    fn dispatch_requires_intent_and_cannot_skip_to_success() {
        // dispatching 不得直达 succeeded（04 §3 合法转移）
        assert!(!transition_allowed(
            SubTaskState::Dispatching,
            SubTaskState::Succeeded
        ));
        assert!(matches!(
            advance_subtask(SubTaskState::Dispatching, SubTaskState::Succeeded),
            Err(ControllerError::InvalidArgument)
        ));
        // 允许路径
        assert!(transition_allowed(
            SubTaskState::Queued,
            SubTaskState::Dispatching
        ));
        assert!(transition_allowed(
            SubTaskState::Dispatching,
            SubTaskState::Accepted
        ));
        assert!(transition_allowed(
            SubTaskState::Accepted,
            SubTaskState::Verifying
        ));
        assert!(transition_allowed(
            SubTaskState::Verifying,
            SubTaskState::Unknown
        ));
        // 终态不可反向
        assert!(!transition_allowed(
            SubTaskState::Succeeded,
            SubTaskState::Queued
        ));
        assert!(!transition_allowed(
            SubTaskState::Unknown,
            SubTaskState::Dispatching
        ));
        assert!(matches!(
            advance_subtask(SubTaskState::Succeeded, SubTaskState::Unknown),
            Err(ControllerError::RevisionConflict)
        ));
    }

    #[test]
    fn dispatching_cancel_only_permitted_from_queued_or_held() {
        // C2（04 §3）：dispatching → cancelled 不在合法转移；intent 后（dispatching）
        // 仅 accepted/verifying/failed/unknown 合法。
        assert!(!transition_allowed(
            SubTaskState::Dispatching,
            SubTaskState::Cancelled
        ));
        assert!(advance_subtask(SubTaskState::Dispatching, SubTaskState::Cancelled).is_err());
        // 04 §8：取消只有 queued/held 可原子改 cancelled
        assert!(transition_allowed(
            SubTaskState::Queued,
            SubTaskState::Cancelled
        ));
        assert!(transition_allowed(
            SubTaskState::Held,
            SubTaskState::Cancelled
        ));
        // 可证 execute 未被接受/执行或明确拒绝时收敛 failed（而非 cancelled），不自动补发
        assert!(transition_allowed(
            SubTaskState::Dispatching,
            SubTaskState::Failed
        ));
        assert!(advance_subtask(SubTaskState::Dispatching, SubTaskState::Failed).is_ok());
    }

    #[test]
    fn full_ten_by_ten_transition_table_matches_spec() {
        let states = [
            SubTaskState::Queued,
            SubTaskState::Held,
            SubTaskState::Dispatching,
            SubTaskState::Accepted,
            SubTaskState::Verifying,
            SubTaskState::Succeeded,
            SubTaskState::Failed,
            SubTaskState::Unknown,
            SubTaskState::Cancelled,
            SubTaskState::Expired,
        ];
        // 04 §3 合法转移原样：行 = from、列 = to（按 states 顺序 Q H D A V S F U C E）；
        // same-state transition_allowed 通常为 false（04 未列 same-state）；
        // advance_subtask 同 state 幂等由 `current == desired` 单独处理，既有逻辑不改。
        let expected: [[bool; 10]; 10] = [
            /* Q */
            [
                false, true, true, false, false, false, true, false, true, true,
            ],
            /* H */
            [
                true, false, false, false, false, false, true, false, true, true,
            ],
            /* D */
            [
                false, false, false, true, true, false, true, true, false, false,
            ],
            /* A */
            [
                false, false, false, false, true, true, true, true, false, false,
            ],
            /* V */
            [
                false, false, false, false, false, true, true, true, false, false,
            ],
            /* S */ [false; 10],
            /* F */ [false; 10],
            /* U */
            [
                false, false, false, false, false, true, true, false, false, false,
            ],
            /* C */ [false; 10],
            /* E */ [false; 10],
        ];
        for (row, &from) in states.iter().enumerate() {
            for (col, &to) in states.iter().enumerate() {
                assert_eq!(
                    transition_allowed(from, to),
                    expected[row][col],
                    "transition table mismatch: {:?} -> {:?}",
                    from,
                    to
                );
            }
        }
        // advance_subtask：同 state 幂等（既有语义）；非法边 Err、合法边 Ok
        assert_eq!(
            advance_subtask(SubTaskState::Dispatching, SubTaskState::Dispatching)
                .expect("same-state transition is idempotent"),
            SubTaskState::Dispatching
        );
        assert!(advance_subtask(SubTaskState::Dispatching, SubTaskState::Cancelled).is_err());
        assert!(advance_subtask(SubTaskState::Queued, SubTaskState::Cancelled).is_ok());
        assert!(advance_subtask(SubTaskState::Held, SubTaskState::Cancelled).is_ok());
        assert!(advance_subtask(SubTaskState::Dispatching, SubTaskState::Failed).is_ok());
    }
}

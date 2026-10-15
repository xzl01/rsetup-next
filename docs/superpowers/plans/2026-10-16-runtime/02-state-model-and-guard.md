### Task 2: 任务状态模型、原因码与合法转移纯函数 (Task State Model & Transition Invariants)

**Files:**
- Create: `crates/rsetup-controller/src/tasks/model.rs`
- Create: `crates/rsetup-controller/src/tasks/mod.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- Consumes: `serde::{Serialize, Deserialize}`、`uuid::Uuid`、`crate::ControllerError`。
- Produces:
  - `pub const SUBTASK_STATE_COUNT: usize = 10`
  - `pub enum SubTaskState { Queued, Held, Dispatching, Accepted, Verifying, Succeeded, Failed, Unknown, Cancelled, Expired }`（`serde(rename_all = "snake_case")`，与前端 `SUBTASK_STATES` 完全一致）
  - `pub fn is_terminal(state: SubTaskState) -> bool`、`pub fn holds_device_lock(state: SubTaskState) -> bool`
  - `pub enum ReasonCode { PermissionDenied, DeviceOffline, CapabilityUnavailable, DeviceBusy, QueueExpired, TimeUncertain, ExecutionRejected, OsError, ResultTimeout, JournalLost, ProtocolMismatch, UserCancelled }`（`serde(rename_all = "SCREAMING_SNAKE_CASE")`）
  - `pub fn transition_allowed(from: SubTaskState, to: SubTaskState) -> bool`
  - `pub fn advance_subtask(current: SubTaskState, desired: SubTaskState) -> Result<SubTaskState, ControllerError>`
  - `pub enum MainTaskState { Queued, Running, Held, Completed }`、`pub enum MainTaskOutcome { Success, Cancelled, Unknown, Partial, Failed }`
  - `pub enum IntentKind { Dispatch }`
  - `pub struct TaskIntent { pub subtask_id: Uuid, pub controller_epoch: Uuid, pub session_epoch: Uuid, pub stream_epoch: Uuid, pub pre_boot_id: String, pub ticket_digest: [u8; 32], pub request_digest: [u8; 32], pub observe_deadline_ms: i64 }`
  - `pub enum EvidenceKind { Attempted, BootTransitionObserved, ExecutionRejected, OsError, JournalLost, None }`（`#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]` + `#[serde(rename_all = "snake_case")]`，六值 wire 严格 snake_case：`attempted`/`boot_transition_observed`/`execution_rejected`/`os_error`/`journal_lost`/`none`；`Option::<EvidenceKind>::None` 为 JSON `null`，与 `"none"` 不同）
  - `pub struct TaskEvidence { pub subtask_id: Uuid, pub journal_epoch: Uuid, pub record_version: u64, pub observed_boot_id: String, pub pre_boot_id: String, pub current_boot_id: String, pub evidence_kind: EvidenceKind, pub reliable_attempted: bool }`
  - `pub struct TaskRecordRef { pub boot_id: String, pub valid_for_ms: u64 }`（prepare 票据摘要引用，**不代表**获批 Protobuf codec）

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

`src/tasks/model.rs` 先放可编译桩（枚举齐全、序列化正确，但转移函数故意放宽）：

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::ControllerError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubTaskState {
    Queued, Held, Dispatching, Accepted, Verifying,
    Succeeded, Failed, Unknown, Cancelled, Expired,
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

/// 可编译 RED 桩：把终态判断反了，使终态/锁断言 RED。
pub fn is_terminal(_state: SubTaskState) -> bool { false }

/// 可编译 RED 桩：unknown 也报“不持锁”，与设计（unknown 始终持设备锁）冲突。
pub fn holds_device_lock(state: SubTaskState) -> bool {
    matches!(state, SubTaskState::Queued)
}

/// 可编译 RED 桩：无条件允许任何转移。
pub fn transition_allowed(_from: SubTaskState, _to: SubTaskState) -> bool { true }

pub fn advance_subtask(_current: SubTaskState, desired: SubTaskState) -> Result<SubTaskState, ControllerError> {
    Ok(desired)
}
```

同文件 `#[cfg(test)] mod tests` 中编写断言：`subtask_state_serializes_snake_case_and_covers_ten_states`/`reason_code_serializes_screaming_snake_case`/`result_evidence_kind_serializes_six_snake_case_values_and_distinguishes_null`/`main_task_model_serializes_four_states_and_five_outcomes`/`dispatch_intent_model_roundtrips_without_expanding_http_contract`/`task_evidence_and_record_ref_internal_models_roundtrip` 六个序列化用例属于模型定义（`SubTaskState`/`ReasonCode`/`EvidenceKind`/`MainTaskState`/`MainTaskOutcome`/`IntentKind`/`TaskIntent`/`TaskEvidence`/`TaskRecordRef` 的 serde 派生自 Step 1 即正确、应直接 PASS；`TaskIntent` 用例用 `assert!(decoded == expected)`、不要求 `Debug`），而 `is_terminal`/`holds_device_lock`/`transition_allowed`/`advance_subtask` 四条行为断言在桩阶段必须因桩而 RED（模型阶段合计 6 serde + 4 behavior = 10 方法）；不把模型定义硬造为行为 RED 或编译失败：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtask_state_serializes_snake_case_and_covers_ten_states() {
        let all = [
            SubTaskState::Queued, SubTaskState::Held, SubTaskState::Dispatching,
            SubTaskState::Accepted, SubTaskState::Verifying, SubTaskState::Succeeded,
            SubTaskState::Failed, SubTaskState::Unknown, SubTaskState::Cancelled,
            SubTaskState::Expired,
        ];
        assert_eq!(all.len(), SUBTASK_STATE_COUNT);
        assert_eq!(serde_json::to_string(&SubTaskState::Dispatching).unwrap(), "\"dispatching\"");
        assert_eq!(serde_json::to_string(&SubTaskState::Unknown).unwrap(), "\"unknown\"");
    }

    #[test]
    fn reason_code_serializes_screaming_snake_case() {
        // 04 §3 + 02 §1：十二值严格 SCREAMING_SNAKE_CASE roundtrip；枚举与 serde 派生
        // 属于模型定义，Step 1 即应 PASS，不是行为 RED（本文档切片未运行，
        // 未实际执行前不得记 PASS）。
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
            assert_eq!(serde_json::from_value::<ReasonCode>(serde_json::json!(wire)).unwrap(), code);
        }
        // `Option<ReasonCode>`：`Some` 为 wire 串，`None`（无原因）为 JSON `null`。
        assert_eq!(serde_json::to_value(Some(ReasonCode::DeviceBusy)).unwrap(), serde_json::json!("DEVICE_BUSY"));
        let none: Option<ReasonCode> = None;
        assert_eq!(serde_json::to_value(none).unwrap(), serde_json::Value::Null);
        // 未知枚举值、大小写不符（小写 snake/PascalCase）与旧板端/中控来源词一律拒绝。
        for bad in ["permission_denied", "PermissionDenied", "REBOOTED", "board", "controller", "success"] {
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
        assert!(serde_json::from_value::<ReasonCode>(serde_json::json!({
            "reason_code": "PERMISSION_DENIED"
        })).is_err());
    }

    #[test]
    fn result_evidence_kind_serializes_six_snake_case_values_and_distinguishes_null() {
        // 04 §6 + 02 §1：六值严格 snake_case；枚举与 serde 派生属于模型定义，
        // 本用例在 Step 1 即应 GREEN，不是行为 RED。
        let all: [(EvidenceKind, &str); 6] = [
            (EvidenceKind::Attempted, "attempted"),
            (EvidenceKind::BootTransitionObserved, "boot_transition_observed"),
            (EvidenceKind::ExecutionRejected, "execution_rejected"),
            (EvidenceKind::OsError, "os_error"),
            (EvidenceKind::JournalLost, "journal_lost"),
            (EvidenceKind::None, "none"),
        ];
        for (kind, wire) in all {
            assert_eq!(serde_json::to_value(kind).unwrap(), serde_json::json!(wire));
            assert_eq!(serde_json::from_value::<EvidenceKind>(serde_json::json!(wire)).unwrap(), kind);
        }
        // `Option<EvidenceKind>`：`Some(EvidenceKind::None)` 是串 `"none"`，
        // `Option::<EvidenceKind>::None`（证据未产生）是 JSON `null`，两者不得混淆。
        assert_eq!(serde_json::to_value(Some(EvidenceKind::None)).unwrap(), serde_json::json!("none"));
        let not_produced: Option<EvidenceKind> = None;
        assert_eq!(serde_json::to_value(not_produced).unwrap(), serde_json::Value::Null);
        // 未知枚举值、大小写不符、PascalCase 与旧板端/中控来源词一律拒绝。
        for bad in ["ATTEMPTED", "BootTransitionObserved", "attempt", "rebooted", "board", "controller", "success"] {
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
        assert!(serde_json::from_value::<EvidenceKind>(serde_json::json!({
            "kind": "boot_transition_observed",
            "execution_token_digest": "ab".repeat(32)
        })).is_err());
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
            assert_eq!(serde_json::to_value(state).unwrap(), serde_json::json!(wire));
            assert_eq!(serde_json::from_value::<MainTaskState>(serde_json::json!(wire)).unwrap(), state);
        }
        let outcomes: [(MainTaskOutcome, &str); 5] = [
            (MainTaskOutcome::Success, "success"),
            (MainTaskOutcome::Cancelled, "cancelled"),
            (MainTaskOutcome::Unknown, "unknown"),
            (MainTaskOutcome::Partial, "partial"),
            (MainTaskOutcome::Failed, "failed"),
        ];
        for (outcome, wire) in outcomes {
            assert_eq!(serde_json::to_value(outcome).unwrap(), serde_json::json!(wire));
            assert_eq!(serde_json::from_value::<MainTaskOutcome>(serde_json::json!(wire)).unwrap(), outcome);
        }
        // `Option<MainTaskOutcome>`：`Some` 为 wire 串，`None`（非 completed、无
        // outcome）为 JSON `null`；“非 completed 为 null”是上层配对规则，
        // 本枚举 serde 单独不伪证明配对。
        assert_eq!(serde_json::to_value(Some(MainTaskOutcome::Partial)).unwrap(), serde_json::json!("partial"));
        let not_completed: Option<MainTaskOutcome> = None;
        assert_eq!(serde_json::to_value(not_completed).unwrap(), serde_json::Value::Null);
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
        // 本用例在 Step 1 即应 PASS，不是行为 RED（本文档切片未运行，
        // 未实际执行前不得记 PASS）。
        assert_eq!(serde_json::to_value(IntentKind::Dispatch).unwrap(), serde_json::json!("dispatch"));
        assert_eq!(
            serde_json::from_value::<IntentKind>(serde_json::json!("dispatch")).unwrap(),
            IntentKind::Dispatch
        );
        // 未知值、大小写不符与 object 形状一律拒绝（与 `ReasonCode`/`EvidenceKind` 同纪律）。
        for bad in ["DISPATCH", "dispatching", "retry", "board", "controller", "success"] {
            assert!(
                serde_json::from_value::<IntentKind>(serde_json::json!(bad)).is_err(),
                "{bad:?} 不在单值词表，必须拒绝"
            );
        }
        assert!(
            serde_json::from_value::<IntentKind>(serde_json::json!({ "kind": "dispatch" })).is_err()
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
            "subtask_id", "controller_epoch", "session_epoch", "stream_epoch",
            "pre_boot_id", "ticket_digest", "request_digest", "observe_deadline_ms",
        ] {
            assert!(object.contains_key(key), "缺少既定 key：{key}");
        }
        // 两个 32B 摘要在 JSON 内为 32 元素数组（内部持久化证据，非 HTTP 投影）。
        assert_eq!(object["ticket_digest"].as_array().map(|a| a.len()), Some(32));
        assert_eq!(object["request_digest"].as_array().map(|a| a.len()), Some(32));
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
        // PASS，不是行为 RED，也不是缺函数编译错误（本文档切片未运行、未实际执行前不得记 PASS）。
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
        let object = value.as_object().expect("TaskEvidence 序列化为 8 key object");
        assert_eq!(object.len(), 8);
        for key in [
            "subtask_id", "journal_epoch", "record_version", "observed_boot_id",
            "pre_boot_id", "current_boot_id", "evidence_kind", "reliable_attempted",
        ] {
            assert!(object.contains_key(key), "缺少既定 key：{key}");
        }
        // key 集合与预期类型逐一相等：Uuid/String 为串、u64 为数字、
        // EvidenceKind 为严格 snake_case 串、bool 为布尔。
        for key in ["subtask_id", "journal_epoch", "observed_boot_id", "pre_boot_id", "current_boot_id"] {
            assert!(object[key].is_string(), "{key} 必须为字符串");
        }
        assert!(object["record_version"].is_u64(), "record_version 必须为数字（u64）");
        assert_eq!(object["evidence_kind"], serde_json::json!("boot_transition_observed"));
        assert!(object["reliable_attempted"].is_boolean());
        let decoded: TaskEvidence = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(decoded, expected);
        // 8 字段全必含：缺任一 key 即拒绝。
        for key in [
            "subtask_id", "journal_epoch", "record_version", "observed_boot_id",
            "pre_boot_id", "current_boot_id", "evidence_kind", "reliable_attempted",
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
            assert!(serde_json::from_value::<TaskEvidence>(v).is_err(), "非法 evidence_kind 必须拒绝");
        }
        // `record_version` 为 u64：负数与小数按类型语义拒绝（不硬加新 schema 字段）。
        for bad in [serde_json::json!(-1), serde_json::json!(1.5)] {
            let mut v = serde_json::to_value(&expected).unwrap();
            v["record_version"] = bad;
            assert!(serde_json::from_value::<TaskEvidence>(v).is_err(), "负/小数 record_version 必须拒绝");
        }
        // `TaskRecordRef`：prepare 票据摘要引用（2 字段全给、合成值、无
        // token/真实凭据），`valid_for_ms` 为 u64，`u64::MAX` 边界同形支持。
        let ref_expected = TaskRecordRef {
            boot_id: "synthetic-boot-prepare-0004".to_owned(),
            valid_for_ms: 300_000,
        };
        let ref_value = serde_json::to_value(&ref_expected).unwrap();
        let ref_object = ref_value.as_object().expect("TaskRecordRef 序列化为 2 key object");
        assert_eq!(ref_object.len(), 2);
        for key in ["boot_id", "valid_for_ms"] {
            assert!(ref_object.contains_key(key), "缺少既定 key：{key}");
        }
        assert!(ref_object["boot_id"].is_string());
        assert!(ref_object["valid_for_ms"].is_u64());
        let ref_decoded: TaskRecordRef = serde_json::from_value(ref_value).unwrap();
        assert_eq!(ref_decoded, ref_expected);
        let ref_max = TaskRecordRef { boot_id: "synthetic-boot-prepare-0005".to_owned(), valid_for_ms: u64::MAX };
        assert_eq!(serde_json::from_value::<TaskRecordRef>(serde_json::to_value(&ref_max).unwrap()).unwrap(), ref_max);
        // 缺字段与负数按同纪律拒绝（u64 类型语义）。
        for v in [
            serde_json::json!({ "boot_id": "synthetic-boot-prepare-0006" }),
            serde_json::json!({ "boot_id": "synthetic-boot-prepare-0007", "valid_for_ms": -1 }),
        ] {
            assert!(serde_json::from_value::<TaskRecordRef>(v).is_err(), "{v} 必须拒绝");
        }
        // 本用例只证明模型定义与 serde 形状（内部持久化/核验输入）；不证明
        // boot 转换/授权可信/at-most-once，`TaskRecordRef` 亦非获批 wire codec——真行为留 Task 5 受门禁验证。
    }

    #[test]
    fn terminal_states_and_device_lock_ownership_follow_lifecycle_table() {
        for terminal in [SubTaskState::Succeeded, SubTaskState::Failed, SubTaskState::Unknown,
                         SubTaskState::Cancelled, SubTaskState::Expired] {
            assert!(is_terminal(terminal), "{terminal:?} must be terminal");
        }
        for live in [SubTaskState::Queued, SubTaskState::Held, SubTaskState::Dispatching,
                     SubTaskState::Accepted, SubTaskState::Verifying] {
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
        assert!(!transition_allowed(SubTaskState::Dispatching, SubTaskState::Succeeded));
        assert!(matches!(
            advance_subtask(SubTaskState::Dispatching, SubTaskState::Succeeded),
            Err(ControllerError::InvalidArgument)
        ));
        // 允许路径
        assert!(transition_allowed(SubTaskState::Queued, SubTaskState::Dispatching));
        assert!(transition_allowed(SubTaskState::Dispatching, SubTaskState::Accepted));
        assert!(transition_allowed(SubTaskState::Accepted, SubTaskState::Verifying));
        assert!(transition_allowed(SubTaskState::Verifying, SubTaskState::Unknown));
        // 终态不可反向
        assert!(!transition_allowed(SubTaskState::Succeeded, SubTaskState::Queued));
        assert!(!transition_allowed(SubTaskState::Unknown, SubTaskState::Dispatching));
        assert!(matches!(
            advance_subtask(SubTaskState::Succeeded, SubTaskState::Unknown),
            Err(ControllerError::RevisionConflict)
        ));
    }

    #[test]
    fn dispatching_cancel_only_permitted_from_queued_or_held() {
        // C2（04 §3）：dispatching → cancelled 不在合法转移；intent 后（dispatching）
        // 仅 accepted/verifying/failed/unknown 合法。
        assert!(!transition_allowed(SubTaskState::Dispatching, SubTaskState::Cancelled));
        assert!(advance_subtask(SubTaskState::Dispatching, SubTaskState::Cancelled).is_err());
        // 04 §8：取消只有 queued/held 可原子改 cancelled
        assert!(transition_allowed(SubTaskState::Queued, SubTaskState::Cancelled));
        assert!(transition_allowed(SubTaskState::Held, SubTaskState::Cancelled));
        // 可证 execute 未被接受/执行或明确拒绝时收敛 failed（而非 cancelled），不自动补发
        assert!(transition_allowed(SubTaskState::Dispatching, SubTaskState::Failed));
        assert!(advance_subtask(SubTaskState::Dispatching, SubTaskState::Failed).is_ok());
    }

    #[test]
    fn full_ten_by_ten_transition_table_matches_spec() {
        let states = [
            SubTaskState::Queued, SubTaskState::Held, SubTaskState::Dispatching,
            SubTaskState::Accepted, SubTaskState::Verifying, SubTaskState::Succeeded,
            SubTaskState::Failed, SubTaskState::Unknown, SubTaskState::Cancelled,
            SubTaskState::Expired,
        ];
        // 04 §3 合法转移原样：行 = from、列 = to（按 states 顺序 Q H D A V S F U C E）；
        // same-state transition_allowed 通常为 false（04 未列 same-state）；
        // advance_subtask 同 state 幂等由 `current == desired` 单独处理，既有逻辑不改。
        let expected: [[bool; 10]; 10] = [
            /* Q */ [false, true, true, false, false, false, true, false, true, true],
            /* H */ [true, false, false, false, false, false, true, false, true, true],
            /* D */ [false, false, false, true, true, false, true, true, false, false],
            /* A */ [false, false, false, false, true, true, true, true, false, false],
            /* V */ [false, false, false, false, false, true, true, true, false, false],
            /* S */ [false; 10],
            /* F */ [false; 10],
            /* U */ [false, false, false, false, false, true, true, false, false, false],
            /* C */ [false; 10],
            /* E */ [false; 10],
        ];
        for (row, &from) in states.iter().enumerate() {
            for (col, &to) in states.iter().enumerate() {
                assert_eq!(
                    transition_allowed(from, to),
                    expected[row][col],
                    "transition table mismatch: {:?} -> {:?}",
                    from, to
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
```

在 `src/tasks/mod.rs` 增加 `pub mod model;` 并在 `src/lib.rs` 增加 `pub mod tasks;`。

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

```bash
cargo test --offline --locked -p rsetup-controller terminal_states_and_device_lock_ownership_follow_lifecycle_table --
cargo test --offline --locked -p rsetup-controller dispatch_requires_intent_and_cannot_skip_to_success --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::dispatching_cancel_only_permitted_from_queued_or_held --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::full_ten_by_ten_transition_table_matches_spec --
```
预期：各恰好执行 1 个用例并因业务断言失败 —— `is_terminal` 桩恒 `false`、`holds_device_lock` 桩只承认 `Queued`、`advance_subtask` 桩无条件放行（终态反向返回 `Ok` 而非 `Err(RevisionConflict)`）；`dispatching_cancel_only_permitted_from_queued_or_held` 因 `transition_allowed` 桩恒 `true` 使 `assert!(!transition_allowed(Dispatching, Cancelled))` 失败、`advance_subtask` 桩恒 `Ok` 使 `.is_err()` 失败而 RED；`full_ten_by_ten_transition_table_matches_spec` 因表内 0 项（same-state，如 `Queued→Queued`）与非法边 `Dispatching→Cancelled` 期望 `false` 而桩返回 `true`、且 `advance_subtask(Dispatching, Cancelled).is_err()` 断言失败而 RED。两个新用例均为**业务断言失败**（现有错误 `Dispatching→Cancelled` 边即 RED 源），不是缺函数编译错误、也不是 0 用例；新增测试仅使用既有 `use super::*;`，无新 import。后两条命令在设计树根目录运行：`CARGO_HOME` 指向主工作区 `/.cargo-home`、`--target-dir target` 使用设计树私有 target、`--offline --locked`、`--lib` 过滤为实际 fullpath；本切片（文档编辑）四条命令均未运行，未实际执行前不得记 PASS。`subtask_state_serializes_snake_case_and_covers_ten_states`、`reason_code_serializes_screaming_snake_case` 与 `result_evidence_kind_serializes_six_snake_case_values_and_distinguishes_null` 在 Step 1 即应 PASS（`SubTaskState`/`ReasonCode`/`EvidenceKind` 的 serde 属于模型定义、Step 1 即正确，不是被桩破坏的行为），**不得**把它们列为 RED；`main_task_model_serializes_four_states_and_five_outcomes`、`dispatch_intent_model_roundtrips_without_expanding_http_contract` 与 `task_evidence_and_record_ref_internal_models_roundtrip` 同样在 Step 1 即应 PASS（`MainTaskState`/`MainTaskOutcome`/`IntentKind`/`TaskIntent`/`TaskEvidence`/`TaskRecordRef` 的 serde 属于模型定义、Step 1 即正确；模型阶段合计 6 serde + 4 behavior = 10 方法），**不得**把它们列为 RED。

- [ ] **Step 3: 最小实现**

```rust
pub fn is_terminal(state: SubTaskState) -> bool {
    matches!(
        state,
        SubTaskState::Succeeded | SubTaskState::Failed | SubTaskState::Unknown
            | SubTaskState::Cancelled | SubTaskState::Expired
    )
}

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

pub fn transition_allowed(from: SubTaskState, to: SubTaskState) -> bool {
    use SubTaskState::*;
    match (from, to) {
        (Queued, Dispatching) | (Queued, Failed) | (Queued, Cancelled) | (Queued, Expired)
        | (Queued, Held) => true,
        (Held, Queued) | (Held, Expired) | (Held, Cancelled) | (Held, Failed) => true,
        (Dispatching, Accepted) | (Dispatching, Verifying) | (Dispatching, Failed)
        | (Dispatching, Unknown) => true,
        (Accepted, Verifying) | (Accepted, Succeeded) | (Accepted, Failed) | (Accepted, Unknown) => true,
        (Verifying, Succeeded) | (Verifying, Failed) | (Verifying, Unknown) => true,
        // unknown 只能由后续有效证据收敛，绝不回到执行中
        (Unknown, Succeeded) | (Unknown, Failed) => true,
        _ => false,
    }
}

pub fn advance_subtask(current: SubTaskState, desired: SubTaskState) -> Result<SubTaskState, ControllerError> {
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
```

`Dispatching → Cancelled` 不再属于纯转移表（04 §3 合法转移不含此边；04 §8：取消只有 queued/held 可原子改 cancelled，与 dispatching 竞争用 CAS 只有一个胜出，dispatching 以后仅拒绝取消或报告不可撤回、不伪装已取消）：intent 后（进入 `dispatching`）仅 `accepted`/`verifying`/`failed`/`unknown` 合法；可证 `execute` 未被接受/执行或明确拒绝时收敛 `failed`（而非 `cancelled`），不自动补发；`Dispatching → Succeeded` 仍永久禁止。取消完整 HTTP 路由、事务 CAS 与 `USER_CANCELLED` 证据写入属 Task 4/6 完整测试，本纯函数切片不宣称已验证。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
cargo test --offline --locked -p rsetup-controller subtask_state_serializes_snake_case_and_covers_ten_states --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::reason_code_serializes_screaming_snake_case -- --exact
cargo test --offline --locked -p rsetup-controller terminal_states_and_device_lock_ownership_follow_lifecycle_table --
cargo test --offline --locked -p rsetup-controller dispatch_requires_intent_and_cannot_skip_to_success --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::dispatching_cancel_only_permitted_from_queued_or_held --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::full_ten_by_ten_transition_table_matches_spec --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::result_evidence_kind_serializes_six_snake_case_values_and_distinguishes_null -- --exact
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::main_task_model_serializes_four_states_and_five_outcomes -- --exact
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::dispatch_intent_model_roundtrips_without_expanding_http_contract -- --exact
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib tasks::model::tests::task_evidence_and_record_ref_internal_models_roundtrip -- --exact
```
预期：各执行 1 个用例（五条命令用 `-- --exact` 精确钉住恰 1 方法），全部 PASS（10 用例 PASS，0 失败）。七条 `--lib` fullpath 命令（含 `reason_code_serializes_screaming_snake_case`、`dispatch_intent_model_roundtrips_without_expanding_http_contract` 与新增 `task_evidence_and_record_ref_internal_models_roundtrip`）与 Step 2 末两条命令同形（设计树根目录、`CARGO_HOME` 指向主工作区 `/.cargo-home`、`--target-dir target` 设计树私有 target、`--offline --locked`、`--lib` 实际 fullpath 过滤）；`reason_code_serializes_screaming_snake_case` 已同样改为真实 fullpath `tasks::model::tests::reason_code_serializes_screaming_snake_case` + `-- --exact` 精确形式（裸名 + `-- --exact` 不会命中，因 `--exact` 匹配测试全路径，必须给 `--lib` fullpath）；本切片纯函数测试（含新增 `EvidenceKind` 序列化用例、`MainTaskState`/`MainTaskOutcome` 序列化用例、`IntentKind`/`TaskIntent` 往返用例与 `TaskEvidence`/`TaskRecordRef` 往返用例）**尚未运行**，未实际执行前不得记 PASS。

- [ ] **Step 5: 静态检查与提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/tasks/model.rs crates/rsetup-controller/src/tasks/mod.rs \
  crates/rsetup-controller/src/lib.rs
git commit -m "feat(controller): 任务状态模型与合法转移纯函数，移除 Dispatching→Cancelled 非法边（C2）"
```

---

### Task 3: Schema v4 版本化 Guard 与五表 SQL (Versioned Integrity Guard & Runtime Schema v4)

**Files:**
- Create: `crates/rsetup-controller/migrations/0004_tasks.sql`
- Modify: `crates/rsetup-controller/src/integrity.rs`
- Modify: `crates/rsetup-controller/src/db.rs`
- Modify: `crates/rsetup-controller/src/error.rs`（`ControllerError` 新增 `SchemaNotReady { found, required }` 变体）
- Test: 内联于 `src/integrity.rs` 的 `#[cfg(test)] mod tests`（纯函数，不需要数据库）

**Interfaces:**
- Consumes: `crate::db::DbPool`、现有 `validate_guard_rows`/`validate_identity_rows`/`check_identity_data`/`lock_integrity_guard`。
- Produces:
  - `pub const SUPPORTED_SCHEMA_VERSION: i32 = 4;`
  - `pub const MIN_SUPPORTED_SCHEMA_VERSION: i32 = 1;`
  - `pub fn is_recognized_schema_version(version: i32) -> bool`
  - `pub fn startup_guard_decision(db_version: Option<i32>) -> Result<(), ControllerError>`（普通启动仅接受编译期配置 4，拒绝旧版、未知版本与缺失行）
  - `pub fn validate_schema_version(found: Option<i32>) -> Result<(), ControllerError>`（版本化 Guard 纯函数：仅接受编译期配置 `SUPPORTED_SCHEMA_VERSION = 4`；`None`、旧版 v1..=3、未知 v5+、0/负数一律 `Err(ControllerError::SchemaNotReady { found, required: 4 })`，fail-closed）
  - `pub fn validate_guard_rows_for_version(rows: Result<Vec<Result<(i8, i32), sqlx::Error>>, sqlx::Error>, expected_version: i32) -> Result<(), ControllerError>`
  - `pub fn validate_runtime_tables_for_version(version: i32, present: &super::db::RuntimeTablePresence) -> Result<(), ControllerError>`
  - `pub fn lock_schema_guard_for_version(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, expected_version: i32) -> Result<(), ControllerError>`
  - `pub async fn check_runtime_data(db: &DbPool, version: i32) -> Result<(), ControllerError>`
  - `pub const TASKS_MIGRATION: &str = include_str!("../migrations/0004_tasks.sql");`
  - `pub struct RuntimeTablePresence { pub main_tasks: bool, pub sub_tasks: bool, pub task_previews: bool, pub device_operation_locks: bool, pub recovery_checks: bool }`

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `src/integrity.rs` 增加可编译桩（保留旧 `(1, 3)` 语义，故意不认识 4，且对未知版本不 fail-closed）：

```rust
pub const SUPPORTED_SCHEMA_VERSION: i32 = 4;
pub const MIN_SUPPORTED_SCHEMA_VERSION: i32 = 1;

/// 可编译 RED 桩：硬编码只承认 3，使 v4 与未知版本断言全部 RED。
pub fn is_recognized_schema_version(version: i32) -> bool { version == 3 }

/// 可编译 RED 桩：直接 Ok，使旧版/未知版本/缺失行断言 RED。
pub fn startup_guard_decision(_db_version: Option<i32>) -> Result<(), ControllerError> {
    Ok(())
}

/// 可编译 RED 桩：恒 `Ok`，使 `None`/旧版 v1..=3/未知 v5+ 负例断言 RED。
pub fn validate_schema_version(_found: Option<i32>) -> Result<(), ControllerError> {
    Ok(())
}

/// 可编译 RED 桩：忽略 version 参数，仍按 (1, 3) 判定。
pub fn validate_guard_rows_for_version(
    rows: Result<Vec<Result<(i8, i32), sqlx::Error>>, sqlx::Error>,
    _version: i32,
) -> Result<(), ControllerError> {
    validate_guard_rows(rows)
}
```

同文件 `#[cfg(test)] mod tests` 增加（这些断言必须 RED）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Error as SqlxError;

    fn rows(pairs: &[(i8, i32)]) -> Result<Vec<Result<(i8, i32), SqlxError>>, SqlxError> {
        Ok(pairs.iter().map(|p| Ok(*p)).collect())
    }

    #[test]
    fn guard_accepts_supported_versions_and_rejects_unknown() {
        assert!(is_recognized_schema_version(1));
        assert!(is_recognized_schema_version(2));
        assert!(is_recognized_schema_version(3));
        assert!(is_recognized_schema_version(SUPPORTED_SCHEMA_VERSION)); // 4
        assert!(!is_recognized_schema_version(0));
        assert!(!is_recognized_schema_version(5));
        assert!(!is_recognized_schema_version(999));
        assert!(!is_recognized_schema_version(-1));
    }

    #[test]
    fn startup_guard_rejects_old_unknown_and_dirty_versions() {
        // 普通启动期望版本来自编译期配置 = 4，而不是从数据库行本身推断期望
        assert!(startup_guard_decision(Some(SUPPORTED_SCHEMA_VERSION)).is_ok());
        // 旧版本（v1..=3）仅能用于显式迁移 preflight，普通启动坚决拒绝
        assert!(startup_guard_decision(Some(3)).is_err());
        assert!(startup_guard_decision(Some(2)).is_err());
        assert!(startup_guard_decision(Some(1)).is_err());
        // 未知或非法版本/缺失行
        assert!(startup_guard_decision(Some(5)).is_err());
        assert!(startup_guard_decision(Some(0)).is_err());
        assert!(startup_guard_decision(None).is_err());
    }

    #[test]
    fn guard_row_validation_is_version_scoped_and_fail_closed() {
        // 恰好一行 singleton=1 且版本与期望一致
        assert!(validate_guard_rows_for_version(rows(&[(1, 3)]), 3).is_ok());
        assert!(validate_guard_rows_for_version(rows(&[(1, 4)]), 4).is_ok());
        // singleton 不是 1
        assert!(validate_guard_rows_for_version(rows(&[(0, 3)]), 3).is_err());
        // 多行 / 空行
        assert!(validate_guard_rows_for_version(rows(&[(1, 3), (1, 3)]), 3).is_err());
        assert!(validate_guard_rows_for_version(rows(&[]), 3).is_err());
        // 版本与期望不一致（例如 v3 结构残留但库声称 4）
        assert!(validate_guard_rows_for_version(rows(&[(1, 3)]), 4).is_err());
        // 行本身是失败行
        assert!(validate_guard_rows_for_version(Err(SqlxError::RowNotFound), 4).is_err());
    }

    #[test]
    fn runtime_tables_are_required_exactly_from_v4() {
        let full = crate::db::RuntimeTablePresence {
            main_tasks: true, sub_tasks: true, task_previews: true,
            device_operation_locks: true, recovery_checks: true,
        };
        let empty = crate::db::RuntimeTablePresence {
            main_tasks: false, sub_tasks: false, task_previews: false,
            device_operation_locks: false, recovery_checks: false,
        };
        assert!(validate_runtime_tables_for_version(4, &full).is_ok());
        assert!(validate_runtime_tables_for_version(4, &empty).is_err());
        // v1..=v3 不得要求 runtime 表存在（历史库仍可只读通过）
        assert!(validate_runtime_tables_for_version(3, &empty).is_ok());
        assert!(validate_runtime_tables_for_version(3, &full).is_err());
        // 未识别版本一律拒绝
        assert!(validate_runtime_tables_for_version(5, &full).is_err());
    }

    #[test]
    fn schema_version_guard_accepts_v4_and_rejects_others() {
        // 正例：编译期配置版本 v4 通过
        assert!(validate_schema_version(Some(SUPPORTED_SCHEMA_VERSION)).is_ok());
        // 负例表：缺失行 None、旧版 v1..=3、未知 v5+、0 与负数一律 fail-closed
        // `ControllerError` 未派生 `PartialEq`，统一用 `matches!` 断言变体与字段值
        let rejected: [Option<i32>; 7] = [None, Some(3), Some(2), Some(1), Some(5), Some(0), Some(-1)];
        for found in rejected {
            assert!(
                matches!(
                    validate_schema_version(found),
                    Err(ControllerError::SchemaNotReady { found: f, required: 4 }) if f == found
                ),
                "expected SchemaNotReady {{ found: {found:?}, required: 4 }}"
            );
        }
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

```bash
cargo test --offline --locked -p rsetup-controller guard_accepts_supported_versions_and_rejects_unknown --
cargo test --offline --locked -p rsetup-controller startup_guard_rejects_old_unknown_and_dirty_versions --
cargo test --offline --locked -p rsetup-controller guard_row_validation_is_version_scoped_and_fail_closed --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib integrity::tests::schema_version_guard_accepts_v4_and_rejects_others -- --exact
cargo test --offline --locked -p rsetup-controller runtime_tables_are_required_exactly_from_v4 --
```
预期：各恰好执行 1 个用例并因业务断言失败 —— `is_recognized_schema_version` 桩拒绝 4 且接受 0/负数是矛盾的（`assert!(is_recognized_schema_version(4))` 失败）；`startup_guard_decision` 桩恒 Ok 导致 `Some(3)` 等断言失败；`validate_guard_rows_for_version` 桩忽略版本，`rows(&[(1,4)]), 4` 会失败。第四条命令的纯测试 `schema_version_guard_accepts_v4_and_rejects_others` 同样 RED（桩恒 `Ok(())`，负例表用例 `None`/v3/v5/0/负数返回 `Ok` 而非 `Err(ControllerError::SchemaNotReady { found, required: 4 })`）。第五条命令 `runtime_tables_are_required_exactly_from_v4` 同样在 RED 阶段即运行并失败（恒 `Ok(())` 桩下 `validate_runtime_tables_for_version(4, &empty)` 返回 `Ok` 而非 `Err`，断言失败；`RuntimeTablePresence` 类型与桩已在 Step 1 落地，属业务断言失败而非编译错误）。选择器说明：第四条命令为 `--lib` fullpath + `-- --exact` 精确形式——`integrity::tests::schema_version_guard_accepts_v4_and_rejects_others` 是测试全路径，`--exact` 匹配测试全路径，裸名 + `-- --exact` 会命中 0 用例，必须给 `integrity::tests::` 全路径；该测试纯逻辑自包含，仅依赖同文件常量与纯函数，无需 `CONTROLLER_TEST_DATABASE_URL` 等数据库环境变量。第四个测试依赖 `RuntimeTablePresence` 类型存在，若尚未定义则属于**编译错误**（禁止作为 RED），因此 Step 1 必须先落地 `db.rs` 的类型与 `validate_runtime_tables_for_version` 桩：

```rust
// src/db.rs —— 可编译 RED 桩：忽略 version 与 presence，恒返回 Ok。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeTablePresence {
    pub main_tasks: bool, pub sub_tasks: bool, pub task_previews: bool,
    pub device_operation_locks: bool, pub recovery_checks: bool,
}
```
```rust
// src/integrity.rs —— 可编译 RED 桩
pub fn validate_runtime_tables_for_version(
    _version: i32,
    _present: &crate::db::RuntimeTablePresence,
) -> Result<(), ControllerError> {
    Ok(())
}
```

- [ ] **Step 3: 最小实现**

1. `migrations/0004_tasks.sql`（**无 FK、无 CHECK**；所有时间列 `DATETIME(6)`；所有 revision/epoch 计数 `BIGINT UNSIGNED`）：
```sql
CREATE TABLE IF NOT EXISTS task_previews (
  token_nonce BINARY(32) NOT NULL PRIMARY KEY,
  actor_user_id BINARY(16) NOT NULL,
  target_hash BINARY(32) NOT NULL,        -- 目标清单规范化升序后的 32 字节 SHA256 摘要（device_target_hash 计算值）；提交时直读该列写入 main_tasks.target_hash
  targets_json JSON NOT NULL,             -- 固定目标快照：canonical 升序字节序 device_id 清单（提交时的唯一目标真相源）
  preview_token_hash BINARY(32) NOT NULL,
  preview_generation BINARY(16) NOT NULL, -- 创建该 preview 的进程启动 epoch；跨代际一律失效，并参与 token 摘要输入
  created_at DATETIME(6) NOT NULL,
  expires_at DATETIME(6) NOT NULL,
  consumed_at DATETIME(6) NULL,
  KEY ix_task_previews_actor (actor_user_id),
  UNIQUE KEY uq_task_previews_token_hash (preview_token_hash),
  KEY ix_task_previews_expires (expires_at)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS main_tasks (
  id BINARY(16) NOT NULL PRIMARY KEY,
  actor_user_id BINARY(16) NOT NULL,
  idempotency_key VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  request_hash BINARY(32) NOT NULL,
  target_hash BINARY(32) NOT NULL,
  preview_token_hash BINARY(32) NOT NULL,
  state VARCHAR(16) NOT NULL,
  outcome VARCHAR(16) NULL,
  revision BIGINT UNSIGNED NOT NULL,
  counts_json JSON NOT NULL,
  created_time_json JSON NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  UNIQUE KEY uq_main_tasks_actor_idem (actor_user_id, idempotency_key),
  KEY ix_main_tasks_actor_created (actor_user_id, created_at),
  KEY ix_main_tasks_state (state)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS sub_tasks (
  id BINARY(16) NOT NULL PRIMARY KEY,
  main_task_id BINARY(16) NOT NULL,
  device_id BINARY(32) NOT NULL,
  state VARCHAR(16) NOT NULL,
  reason_code VARCHAR(32) NULL,
  revision BIGINT UNSIGNED NOT NULL,
  lock_generation BIGINT UNSIGNED NOT NULL DEFAULT 0,
  pre_boot_id VARCHAR(128) NULL,
  controller_epoch BINARY(16) NULL,
  session_epoch BINARY(16) NULL,
  stream_epoch BINARY(16) NULL,
  ticket_digest BINARY(32) NULL,
  request_digest BINARY(32) NULL,
  queue_deadline DATETIME(6) NOT NULL,
  observe_deadline DATETIME(6) NULL,
  evidence_json JSON NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  UNIQUE KEY uq_sub_tasks_main_device (main_task_id, device_id),
  KEY ix_sub_tasks_main (main_task_id),
  KEY ix_sub_tasks_device_state (device_id, state)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS device_operation_locks (
  device_id BINARY(32) NOT NULL PRIMARY KEY,
  main_task_id BINARY(16) NOT NULL,
  owner_sub_task_id BINARY(16) NOT NULL,
  generation BIGINT UNSIGNED NOT NULL,
  acquired_at DATETIME(6) NOT NULL,
  released_at DATETIME(6) NULL,
  UNIQUE KEY uq_device_locks_owner (owner_sub_task_id),
  KEY ix_device_locks_main (main_task_id)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS recovery_checks (
  id BINARY(16) NOT NULL PRIMARY KEY,
  process_epoch BINARY(16) NOT NULL,
  subtask_id BINARY(16) NOT NULL,
  outcome VARCHAR(32) NOT NULL,
  detail_redacted JSON NOT NULL,
  checked_at DATETIME(6) NOT NULL,
  KEY ix_recovery_checks_subtask (subtask_id),
  UNIQUE KEY uq_recovery_checks_epoch_subtask (process_epoch, subtask_id)
) CHARACTER SET utf8mb4;
```
2. `validate_runtime_tables_for_version(version, present)`：`version == 4` 时要求五表全为 `true`，任一缺失返回 `Err(Config("runtime schema table missing: ..."))`；`(1..=3)` 时要求五表全为 `false`；`!is_recognized_schema_version(version)` 直接 `Err`。
3. `is_recognized_schema_version(v) = (MIN_SUPPORTED_SCHEMA_VERSION..=SUPPORTED_SCHEMA_VERSION).contains(&v)`。
4. `validate_guard_rows_for_version(rows, expected_version)`：`rows` 为 `Err` → `Err(scan_error("meta.guard"))`；行数必须为 1；该行必须是 `Ok((1, expected_version))`；**版本不匹配即拒绝**（覆盖“库声称 v4 但残留 v3 行”）。
5. `lock_schema_guard_for_version(tx, expected_version)`：`SELECT singleton, schema_version FROM schema_meta WHERE singleton = 1 FOR UPDATE`，取回后交给 `validate_guard_rows_for_version(rows, expected_version)`。**核心安全约束**：期望版本必须来自编译期配置（`SUPPORTED_SCHEMA_VERSION = 4`），**绝对禁止**从数据库行本身读取版本后再验证自己。`db.rs` 中的 `identity_schema_decision` 等旧 `3` 硬编码必须统一重构，`v3` 仅允许在显式迁移前置检查（preflight）中使用；普通启动对旧版（v1..=3）、未知版本（v5+）或脏数据一律拒绝（fail-closed），统一走版本化 guard。
6. `check_runtime_data(db, version)`：`version < 4` 直接 `Ok(())`；`version == 4` 时执行只读扫描（全部使用固定语句、绝不插值标识符）：`sub_tasks.main_task_id` 无孤儿主任务、`sub_tasks.state` 属于 10 个已知值、`device_operation_locks.owner_sub_task_id` 指向存在的子任务、`main_tasks.outcome` 在非 `completed` 时为 `NULL`、`counts_json` 是合法 JSON 对象；任一违规返回 `Err(Config("runtime data <code>"))`，登录/启动路径据此 fail-closed。

   **完整实现（与 `check_identity_data` 同模块；纯只读扫描，不加 `FOR UPDATE`、不写库，可在登录路径调用）**：

   ```rust
   /// v4 运行时数据只读扫描：任一违规返回 `Err(Config)`，登录/启动路径据此 fail-closed。
   /// 全部 SQL 为固定字面量（见函数内常量），绝不从外部输入插值表名或列名。
   pub async fn check_runtime_data(db: &DbPool, version: i32) -> Result<(), ControllerError> {
       // v1..=3 未建五张 runtime 表，直接放行。
       if version < 4 {
           return Ok(());
       }
       // 更高未知版本（>=5）已在上游 `validate_schema_version` 被拒绝，不会到达此处；
       // 此处保持 `Ok` 以维持本函数“仅 v4 做数据扫描”的契约。
       if version != 4 {
           return Ok(());
       }

       // (1) `sub_tasks.main_task_id` 无孤儿主任务
       const ORPHAN_SUB_TASKS: &str = "SELECT st.id FROM sub_tasks st \
           LEFT JOIN main_tasks mt ON st.main_task_id = mt.id \
           WHERE mt.id IS NULL LIMIT 1";
       let hit = sqlx::query(ORPHAN_SUB_TASKS)
           .fetch_optional(db.pool())
           .await
           .map_err(|e| scan_error("runtime.sub_tasks_orphan_main_task_id"))?;
       if hit.is_some() {
           return Err(ControllerError::Config("runtime data sub_tasks orphan main_task_id".into()));
       }

       // (2) `sub_tasks.state` 必须属于 10 个已知合法值（对齐 Task 2 `SubTaskState` 10 状态集合）
       const BAD_SUB_TASK_STATE: &str = "SELECT id FROM sub_tasks WHERE state NOT IN \
           ('queued', 'held', 'dispatching', 'accepted', 'verifying', \
            'succeeded', 'failed', 'unknown', 'cancelled', 'expired') LIMIT 1";
       let hit = sqlx::query(BAD_SUB_TASK_STATE)
           .fetch_optional(db.pool())
           .await
           .map_err(|e| scan_error("runtime.sub_tasks_invalid_state"))?;
       if hit.is_some() {
           return Err(ControllerError::Config("runtime data sub_tasks invalid state".into()));
       }

       // (3) `device_operation_locks.owner_sub_task_id` 必须指向存在的子任务
       const ORPHAN_DEVICE_LOCKS: &str = "SELECT l.device_id FROM device_operation_locks l \
           LEFT JOIN sub_tasks st ON l.owner_sub_task_id = st.id \
           WHERE st.id IS NULL LIMIT 1";
       let hit = sqlx::query(ORPHAN_DEVICE_LOCKS)
           .fetch_optional(db.pool())
           .await
           .map_err(|e| scan_error("runtime.device_operation_locks_orphan_owner"))?;
       if hit.is_some() {
           return Err(ControllerError::Config("runtime data device_operation_locks orphan owner_sub_task_id".into()));
       }

       // (4) `main_tasks.outcome` 在非 `completed` 状态时必须为 NULL
       const OUTCOME_NOT_NULL: &str = "SELECT id FROM main_tasks \
           WHERE state != 'completed' AND outcome IS NOT NULL LIMIT 1";
       let hit = sqlx::query(OUTCOME_NOT_NULL)
           .fetch_optional(db.pool())
           .await
           .map_err(|e| scan_error("runtime.main_tasks_outcome_not_null"))?;
       if hit.is_some() {
           return Err(ControllerError::Config("runtime data main_tasks outcome not null for non-completed state".into()));
       }

       // (5) `counts_json` 必须解析为合法 JSON 对象（固定语句 + 应用层扫描，精确区分“解析失败/非 object”）
       const COUNTS_JSON: &str = "SELECT id, counts_json FROM main_tasks";
       let rows = sqlx::query(COUNTS_JSON)
           .fetch_all(db.pool())
           .await
           .map_err(|e| scan_error("runtime.main_tasks_counts_json"))?;
       for row in &rows {
           let raw: String = row.try_get(1).map_err(|e| scan_error("runtime.main_tasks_counts_json"))?;
           let value: serde_json::Value = serde_json::from_str(&raw)
               .map_err(|_| ControllerError::Config("runtime data main_tasks invalid counts_json".into()))?;
           if !value.is_object() {
               return Err(ControllerError::Config("runtime data main_tasks invalid counts_json".into()));
           }
       }

       Ok(())
   }
   ```

   **说明**：检查 (1)–(4) 均为 `LIMIT 1` 短路探测，命中即返回对应 `Err(Config(...))`，消息文本与编号一一对应、作为操作员排查锚点，不得随意改写；检查 (5) 为全表应用层扫描，`serde_json` 解析失败或解析结果非 object（如数组、标量）同样返回 `Err(Config)`。任一查询在 DB 层失败时经 `scan_error("<runtime 标签>")` 统一映射（与第 4 条 `scan_error("meta.guard")` 同约定），同样落入 fail-closed。`db.pool()` 的取用方式跟随现有 `check_identity_data` 对 `DbPool` 的调用约定（若现有包装 API 形如 `&*db.pool()`，以实现侧为准，契约不变）；`scan_error` 的签名（标签 + 错误值的映射方式）以现有实现为准。本函数必须在版本 guard 通过后调用（此时 `version == 4` 已被确认），更高版本由 guard 层拒绝，不在本函数重复判定。
7. `src/db.rs` 增加 `TASKS_MIGRATION` 常量与 `run_schema_v4_migration` 的**显式**入口（只由测试/操作员显式调用，**不得**在普通启动路径隐式执行）。preflight 必须验证原版本为 3 且数据结构完整，迁移执行完成后原子更新 `schema_version = 4`。
8. `validate_schema_version(found)`：纯函数、无 DB——`found == Some(SUPPORTED_SCHEMA_VERSION)`（即 `Some(4)`）返回 `Ok(())`；其余（`None`、v1..=3、v5+、0、负数）返回 `Err(ControllerError::SchemaNotReady { found, required: SUPPORTED_SCHEMA_VERSION })`。`src/error.rs` 的 `ControllerError` 须新增该变体 `SchemaNotReady { found: Option<i32>, required: i32 }`（thiserror，`found` 记录实际发现的版本，`required` 固定为编译期配置值）；`startup_guard_decision` 缺失行/旧版/未知版本拒绝路径复用该变体，不新增第二套错误变体。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
cargo test --offline --locked -p rsetup-controller guard_accepts_supported_versions_and_rejects_unknown --
cargo test --offline --locked -p rsetup-controller startup_guard_rejects_old_unknown_and_dirty_versions --
cargo test --offline --locked -p rsetup-controller guard_row_validation_is_version_scoped_and_fail_closed --
cargo test --offline --locked -p rsetup-controller runtime_tables_are_required_exactly_from_v4 --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib integrity::tests::schema_version_guard_accepts_v4_and_rejects_others -- --exact
```
预期：各执行 1 个用例，全部 PASS（5 用例 PASS，0 失败；第五条命令为纯测试 `schema_version_guard_accepts_v4_and_rejects_others` 的 `--lib` fullpath + `-- --exact` 精确选择器，与 Step 2 第四条命令同形，恰命中 1 用例）。同时确认既有 identity 测试仍绿：
```bash
cargo test --offline --locked -p rsetup-controller --lib
```
预期：全绿，且**没有**任何测试仍断言“普通启动只接受版本 3”。

- [ ] **Step 5（严格条件门禁）：真实备份后的隔离双库迁移验证与独立 migration_v4 验收**

**执行前置（全部满足才可执行；缺一即保持未运行并记录 `BLOCKED`）**：目标属于已授权的两个专用隔离开发库；凭据仅在进程内从本地文件读取（**不得**回显、不得写入任何被跟踪文件或命令参数）；只读核验 `SELECT DATABASE()`、`schema_version = 3`、身份表形状与数据完整、引擎版本与实际权限；已完成**真实备份**并校验 SHA256 与可读性、保留去敏回执；独占与停写条件确认；现有显式迁移门禁存在。跳过需要数据库 root 权限的锁验证，但不跳过应用账号下真实事务验证。

**重要说明**：既有的 `mysql_identity` / `tidb_identity` 测试是 v3 专用的身份基线测试，**绝不可当作 v4 迁移验收**。必须新增独立的 `tests/migration_v4.rs` 集成测试，串行执行显式迁移并在两引擎分别验收：
1. `schema_meta` 存在且 `singleton = 1`, `schema_version = 4`；
2. 整个数据库恰好包含 16 张表（11 张 identity 表 + 5 张 runtime 表）；
3. 无任何外键定义：`information_schema.REFERENTIAL_CONSTRAINTS` 为 0 行；
4. 无任何 CHECK 约束：`information_schema.CHECK_CONSTRAINTS` 为 0 行。

满足后按引擎**串行**执行（两引擎分别记录被测版本；不得跨引擎共用同一目标）：
```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test migration_v4 -- --ignored --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test migration_v4 -- --ignored --test-threads=1
```
预期：各引擎执行用例数 **> 0** 且全部 PASS，并记录 `SELECT VERSION()` 与迁移后 `schema_version = 4`。缺 URL、连接失败、0 用例、备份不可证（无法确认可恢复）**一律**记 `BLOCKED`，迁移保持阻断，绝不 `return`/伪造 `backup_ref`。既有 `mysql_identity`/`tidb_identity` 仅在需要确认 v3 历史基线时作为辅助运行。

- [ ] **Step 6: 静态检查与提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/migrations/0004_tasks.sql crates/rsetup-controller/src/integrity.rs \
  crates/rsetup-controller/src/db.rs
# tests/migration_v4.rs 仅在 Step 5 门禁通过后纳入提交
git commit -m "feat(controller): versioned integrity guard and runtime schema v4"
```

---


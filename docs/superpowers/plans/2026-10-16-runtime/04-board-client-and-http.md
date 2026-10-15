### Task 5: 共享类型化 BoardClient、意图后发送与只读恢复 (BoardClient, Intent-Commit-First Dispatch, Read-Only Recovery)

**Files:**
- Create（条件）: `crates/rsetup-controller/src/tasks/board_client.rs`
- Create: `crates/rsetup-controller/src/tasks/scheduler.rs`
- Create: `crates/rsetup-controller/src/tasks/recovery.rs`
- Create: `crates/rsetup-controller/src/tasks/evidence.rs`
- Modify: `crates/rsetup-controller/src/tasks/model.rs`（前置说明：`TaskIntent`/`TaskEvidence`/`TaskRecordRef` 三类型的单一定义（含 serde 派生）已由 Task 2 Step 1 单 producer 定义，本任务只核对消费与 04 §6 真实行为，不重复 derive 或定义）
- Test: 内联单元测试 + 条件性 `crates/rsetup-controller/tests/controller_recovery.rs`

**Interfaces:**
- Consumes: Task 2 `SubTaskState`/`TaskIntent`/`TaskEvidence`/`EvidenceKind`；Task 3 guard；Task 4 `TaskRepository`。
- Produces:
  - `pub const GLOBAL_DISPATCH_SLOTS: usize = 16`（05 §5 待审默认）
  - `pub const QUEUE_TTL_MS: i64 = 60 * 60 * 1000`（05 §5 待审默认）
  - `pub const OBSERVE_WINDOW_MS: i64 = 5 * 60 * 1000`（04 §5.3 待审建议）
  - `pub struct DeviceIdentity { pub device_id: [u8; 32], pub agent_epoch: Uuid, pub current_boot_id: String }`（内部调度/恢复上下文；`device_id` 可作设备键来源，但**不**替代 `BoardClient` 的 `&[u8; 32]` 键参数）
  - `pub enum AdmissionVerdict { Admit, Deny { reason_code: ReasonCode } }`
  - `pub fn admit_for_dispatch(state: SubTaskState, actor_active: bool, has_reboot: bool, admission_approved: bool, control_flow_healthy: bool, capability_supported: bool, persistent_journal_ok: bool, queue_deadline_valid: bool, lock_owned: bool) -> AdmissionVerdict`
  - `pub struct DispatchPlan { pub subtask_id: Uuid, pub device_id: [u8; 32], pub pre_boot_id: String, pub ticket_boot_id: String, pub ticket_valid_for_ms: u64, pub observe_deadline_ms: i64 }`
  - `pub enum RecoveryAction { ReadOnlyVerify { subtask_id: Uuid }, MarkHeld { subtask_id: Uuid }, MarkExpired { subtask_id: Uuid } }`
  - `pub fn classify_recovery(state: SubTaskState, queue_deadline_trustworthy: bool, queue_deadline_expired: bool) -> RecoveryAction`（`queue_deadline_expired` 为队列期限过期判定结果，由调用方以 `sub_tasks.queue_deadline` 对照可信当前时刻求得后传入；仅当 `queue_deadline_trustworthy == true` 时被消费，不可信时调用方传 `false` 且本函数忽略该参数，使"可信且已过期 → `MarkExpired`"分支可真实产生）
  - `pub fn calculate_main_task_outcome(subtask_states: &[SubTaskState]) -> (MainTaskState, Option<MainTaskOutcome>)`
  - `pub struct EvidenceBaseline { pub journal_epoch: Uuid, pub record_version: u64, pub confirmed_failed_versions: Vec<u64> }`（成功核验的基线/历史比较输入：`journal_epoch`/`record_version` 为该子任务最近一条已持久化记录的 journal epoch 与记录版本，缺省口径：无已持久化记录时 `journal_epoch` 取调用方从 DB 读取的当前 epoch、`record_version` 取 0；`confirmed_failed_versions` 为已确认 `Failed` 记录的 record_version 集合，缺省口径：空集合即无已确认失败）
  - `pub fn success_evidence_satisfied(expected_pre_boot: &str, ev: &TaskEvidence, baseline: &EvidenceBaseline) -> bool`（期望 pre_boot_id 作为核验入参，由调用方从数据库 `sub_tasks.pre_boot_id` 读取传入，不作未定义字段；`baseline` 由调用方从该子任务已持久化记录读取构造后传入）
  - `pub struct SchedulerSummary { pub admitted: usize, pub prepared: usize, pub intent_committed: usize, pub executed_sent: usize, pub rejected: usize }`
  - `BoardFuture` 别名 + `pub trait BoardClient: Send + Sync` + `pub enum BoardClientError`（**仅当 Step 0 门禁通过**；签名逐字传播自四线接口所有权清单 §3 之“6 方法待审统一目标契约”，dyn-safe 形式，不用 `async fn`）：
```rust
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use uuid::Uuid;
use rsetup_protocol::wire::{Capabilities, ClockSample, DeviceStatus, RebootExecute, RebootTicket, TaskRecord};

pub type BoardFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BoardClientError>> + Send + 'a>>;

pub trait BoardClient: Send + Sync {
    fn capabilities<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, Capabilities>;
    fn status<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, DeviceStatus>;
    fn clock<'a>(&'a self, device: &'a [u8; 32]) -> BoardFuture<'a, ClockSample>;
    fn prepare<'a>(&'a self, device: &'a [u8; 32], sub_task_id: Uuid, expected_boot_id: &'a str) -> BoardFuture<'a, RebootTicket>;
    fn execute<'a>(&'a self, device: &'a [u8; 32], ticket: &'a RebootTicket, request: &'a RebootExecute) -> BoardFuture<'a, TaskRecord>;
    fn task_record<'a>(&'a self, device: &'a [u8; 32], sub_task_id: Uuid) -> BoardFuture<'a, Option<TaskRecord>>;
}

pub enum BoardClientError {
    DeviceOffline,
    Rejected { status: i32, reason_code: Option<String> },
    ProtocolMismatch,
    CapabilityUnavailable,
    DeviceBusy,
    ResourceExhausted,
    Timeout,
    Transport,
}
```
  - 契约要点（与所有权清单 §3 一致）：设备键为 `device: &'a [u8; 32]`（设备公钥字节），`DeviceIdentity` 仍可作内部调度/恢复上下文但**不可替代** trait 键参数；6 方法不可增 `task_query(&TaskQuery)`/`prepare_request()`（二者为 Transport 待审扩展，不属本契约）；`BoardClientError` 为独立 typed 枚举，**不**泛携 `ControllerError`/driver 字符串（实际 `ControllerError`（`src/error.rs`）仅派生 `Debug`/`thiserror`，不照抄 Transport 草案 `#[derive(Clone, PartialEq, Eq)]` 与 `Controller(#[from] ControllerError)` 变体），不加 `DiagCode` 类型或错误派生 Eq 依赖；`Rejected.reason_code` 严格按 03/04 稳定词表校验，不存原 driver 字符串；`execute` 消费 `ticket` 与 `request` 双引用，实现层发送前须核对 `ticket.sub_task_id == request.sub_task_id`；`task_record` 返回 `Option<TaskRecord>`：`Ok(None)` 即 TASK_NOT_FOUND(1006)，表示查询时未找到记录，**不证明** `execute` 未执行，也不单独推进状态。
  - 对象安全编译证明（计划代码实际符号完整，不跑测试；不声称独立编译或测试）：
```rust
fn assert_board_client_dyn_safe(client: Arc<dyn BoardClient>, device: &[u8; 32], subtask_id: Uuid, ticket: &RebootTicket, request: &RebootExecute) {
    let _: BoardFuture<'_, Capabilities> = client.capabilities(device);
    let _: BoardFuture<'_, DeviceStatus> = client.status(device);
    let _: BoardFuture<'_, ClockSample> = client.clock(device);
    let _: BoardFuture<'_, RebootTicket> = client.prepare(device, subtask_id, "");
    let _: BoardFuture<'_, TaskRecord> = client.execute(device, ticket, request);
    let _: BoardFuture<'_, Option<TaskRecord>> = client.task_record(device, subtask_id);
}
```
  该函数可编译即证 `Arc<dyn BoardClient>` 对象安全（各方法均返回 Box 化 `BoardFuture`，无泛型方法/`async fn`/RPIT）；集成装配时由调度器/集成 `TaskApi` adapter 以 `Arc<dyn BoardClient>` 持有与注入；`HttpTasksState` 保持 `{api, auth}` 两个字段（不新增 `BoardClient` 字段），经 `TaskApi` 间接消费。
  - **DTO 缺席门禁约束**：Step 0 门禁未通过前，`scheduler.rs`/`recovery.rs`/`model.rs` 仅纯函数/纯结构切片（**不得** `use rsetup_protocol`），typed `board_client.rs`（含上列别名/trait/enum）**不创建、不导出**；其创建与导出在 wire DTO 依赖（含 `RebootExecute`）满足后由集成 owner 按共享文件串行顺序（Identity gate → ADMIN audit → Runtime v4，lib/main 归独立集成 owner）前置推进；泛型结构体 `TaskScheduler<C: BoardClient, R: TaskRepository>` 与 `run_once` 暂不定义并明确标记为 `BLOCKED (awaiting BoardClient)`，绝不在缺少 wire DTO 时构造伪泛型桩。既有具体 `TaskScheduler` 实现缺口仍明确未解决：本契约的文本级传播**不**代表调度已接线。
  - `pub async fn recover_process<R: TaskRepository, C: BoardClient>(repo: &R, board_client: &C, process_epoch: Uuid) -> Result<RecoverySummary, ControllerError>`（`board_client` 由调用方显式注入：集成装配时由调度器/集成入口以 `Arc<dyn BoardClient>` 构造并传入——与本节"对象安全编译证明"段既定的 `TaskApi` adapter 持有与注入路径同源——`recover_process` 内部仅调用其只读 `task_record`；本函数因签名依赖 `BoardClient`，在 Step 0 门禁未通过时与 `board_client.rs` 一并标 `BLOCKED (awaiting BoardClient)`，不作为纯逻辑切片单独编译）
  - `pub struct RecoverySummary { pub read_only_verified: usize, pub marked_held: usize, pub marked_expired: usize, pub execute_calls: usize }`（`execute_calls` 必须恒为 0）

- [ ] **Step 0: 线协议 DTO 门禁判定（未通过则本任务整体降级为 BLOCKED）**

```bash
grep -n "RebootTicket\|RebootExecute\|TaskRecord\|ClockSample\|DeviceStatus\|Capabilities" crates/rsetup-protocol/src/wire.rs
```
- 若**无输出**（当前分支即为该状态）：`board_client.rs` 与 `evidence.rs` 中任何 `use rsetup_protocol::wire::...` 的代码**一律不创建**；本任务只提交 `scheduler.rs`、`recovery.rs` 与 `model.rs` 的**纯逻辑**切片（它们**不得**引用 `rsetup_protocol`），并把 `BoardClient`、`evidence` 标 `BLOCKED (waiting for approved transport wire DTOs)`。
- 若 DTO 已交付：按接口所有权账本「第 5 轮修复仲裁记录」决议①执行**两段式门禁**（账本 §3 既有 `TaskRecord` 定义为唯一权威 wire 形状；**任何线不得新增 `accepted`/`attempted`/OS 结果等 wire 字段**，出现即越界并报告缺口；旧"缺字段即影子 DTO/BLOCKED"口径由决议①取代，不再使用）：
  - **a) 形状核验（逐字段）**：`RebootTicket` 必须含 `boot_id` 与 `valid_for_ms`；`RebootExecute` 必须含 `sub_task_id`、`expected_boot_id`、`execution_token`（32B）；`TaskRecord` 必须与账本定义逐字段一致：`schema_version:u32`、`sub_task_id:String`、`journal_epoch:String`、`record_version:u64`、`state:String`、`pre_boot_id:String`、`observed_boot_id:Option<String>`、`reason_code:Option<String>`、`updated_unix_ms:Option<i64>`、`evidence_kind:String`（wire 为 proto3 非 optional string；Rust 侧 `Option<String>` 空串↔None 双向归一为唯一合法归一形态——账本决议②，**不得**因 `Option`/空串形态判 `BLOCKED`）——多字段/少字段/类型不符均为形状违规，标 `BLOCKED` 并报告缺口；逐字段核对在归一形态上进行；`observed_boot_id`/`reason_code`/`updated_unix_ms` 取 `Option=None` 为消息合法缺省值，**不**视为缺字段。
  - **b) 语义核验（attempted 可靠性与 OS 结果）**：仅以 Transport 实际 wire 载荷可表达的字段与值域判定——`state`（五值：`accepted`/`attempted`/`succeeded`/`failed`/`unknown`）、`evidence_kind`（归一后仅 `boot_transition_observed` 或 `None`=未提供）、`reason_code`（可选，12 值 SCREAMING_SNAKE_CASE）、`observed_boot_id`（可选）；wire 无 `accepted`/`attempted` 字段，`verifying` 非 wire state（Runtime 内存/DB 态，非 wire 判据）。判定对照表：`state=attempted` → attempted 已可靠（板端 journal 已持久记录尝试，03 §5 attempted≠成功），OS 结果未闭环；`state=succeeded` + `evidence_kind=boot_transition_observed` + `observed_boot_id` 与当前认证连接 boot_id 一致 → OS 结果成功闭环（还须同时满足 04 §6 全部条件，即 Task 2 `success_evidence_satisfied`）；`state=failed` + 相应 `reason_code`（如 `OS_ERROR`，其 `evidence_kind` 默认 `None`）→ OS 结果失败闭环；`state=accepted`（含配 `reason_code=EXECUTION_REJECTED` 的尝试前执行被拒组合）→ 未尝试、OS 结果不适用；`state=unknown` 及其余组合 → attempted 不可靠或 OS 结果未闭环，按 `unknown` 只读恢复、不自动补发。生产者契约以 Transport 计划锚点（L143–149 词表、L2546–2549/L3838–3843 `evidence_kind` 默认值与归一语义）及账本决议①②为准。
- **禁止**在任何情况下自定义编码器、固定业务 bytes 或在本 crate 内重新定义这些 DTO。
- 若门禁通过需创建 `board_client.rs`：controller 必须新增本地 `rsetup-protocol` 直接依赖（workspace 内 path 依赖，**不装** registry 新包）并做离线 `Cargo.lock` 更新；该 manifest/lock 修改为**明确待审步骤**（先登记待审再提交，本任务不自动修改 manifest；当前分支 `Cargo.toml` 无 `rsetup-protocol` 依赖），且**不创造**影子 DTO/codec。

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

`src/tasks/scheduler.rs` 桩：

```rust
use crate::tasks::model::{ReasonCode, SubTaskState};

pub const GLOBAL_DISPATCH_SLOTS: usize = 16;
pub const QUEUE_TTL_MS: i64 = 60 * 60 * 1000;
pub const OBSERVE_WINDOW_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionVerdict { Admit, Deny { reason_code: ReasonCode } }

/// 可编译 RED 桩：忽略所有前置，恒 Admit，使准入拒绝断言 RED。
pub fn admit_for_dispatch(
    _state: SubTaskState, _actor_active: bool, _has_reboot: bool, _admission_approved: bool,
    _control_flow_healthy: bool, _capability_supported: bool, _persistent_journal_ok: bool,
    _queue_deadline_valid: bool, _lock_owned: bool,
) -> AdmissionVerdict { AdmissionVerdict::Admit }
```

`src/tasks/recovery.rs` 桩：

```rust
use crate::tasks::model::{MainTaskOutcome, MainTaskState, SubTaskState, TaskEvidence};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    ReadOnlyVerify { subtask_id: Uuid },
    MarkHeld { subtask_id: Uuid },
    MarkExpired { subtask_id: Uuid },
}

/// 成功核验的基线/历史比较输入（字段语义与缺省口径见 Interfaces，桩与实现共用该定义）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvidenceBaseline {
    pub journal_epoch: Uuid,
    pub record_version: u64,
    pub confirmed_failed_versions: Vec<u64>,
}

/// 可编译 RED 桩：对所有状态都返回 ReadOnlyVerify，且不区分 queued 期限可信性与是否过期。
pub fn classify_recovery(
    _state: SubTaskState,
    _queue_deadline_trustworthy: bool,
    _queue_deadline_expired: bool,
) -> RecoveryAction {
    RecoveryAction::ReadOnlyVerify { subtask_id: Uuid::nil() }
}

/// 可编译 RED 桩：恒返回 true，使跨 epoch/版本倒退/矛盾失败记录三条负例断言 RED。
pub fn success_evidence_satisfied(
    _expected_pre_boot: &str,
    _ev: &TaskEvidence,
    _baseline: &EvidenceBaseline,
) -> bool { true }

/// 可编译 RED 桩：恒返回 (Running, None)。
pub fn calculate_main_task_outcome(_states: &[SubTaskState]) -> (MainTaskState, Option<MainTaskOutcome>) {
    (MainTaskState::Running, None)
}
```

内联/集成测试（必须 RED）：

```rust
#[test]
fn dispatch_admission_denies_on_every_failed_precondition() {
    use crate::tasks::model::ReasonCode;
    use AdmissionVerdict::Deny;
    // 全部满足 -> Admit
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, true, true, true, true, true, true, true),
        AdmissionVerdict::Admit
    );
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, false, true, true, true, true, true, true, true),
        Deny { reason_code: ReasonCode::PermissionDenied }
    );
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, false, true, true, true, true, true, true),
        Deny { reason_code: ReasonCode::PermissionDenied }
    );
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, true, false, true, true, true, true, true),
        Deny { reason_code: ReasonCode::PermissionDenied }
    );
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, true, true, false, true, true, true, true),
        Deny { reason_code: ReasonCode::DeviceOffline }
    );
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, true, true, true, false, true, true, true),
        Deny { reason_code: ReasonCode::CapabilityUnavailable }
    );
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, true, true, true, true, false, true, true),
        Deny { reason_code: ReasonCode::CapabilityUnavailable }
    );
    // 期限不可信 -> TIME_UNCERTAIN（进 held，而非 failed）
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, true, true, true, true, true, false, true),
        Deny { reason_code: ReasonCode::TimeUncertain }
    );
    // 锁不属于本子任务：绝不发送
    assert_eq!(
        admit_for_dispatch(SubTaskState::Queued, true, true, true, true, true, true, true, false),
        Deny { reason_code: ReasonCode::DeviceBusy }
    );
    // 非 queued 状态不得进入 dispatch
    assert!(matches!(
        admit_for_dispatch(SubTaskState::Held, true, true, true, true, true, true, true, true),
        AdmissionVerdict::Deny { .. }
    ));
}

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
    assert!(matches!(classify_recovery(Queued, false, false), RecoveryAction::MarkHeld { .. }));
    // 可信且未过期 -> 只读核验（精确分支断言，无 OR 掩盖）
    assert!(matches!(classify_recovery(Queued, true, false), RecoveryAction::ReadOnlyVerify { .. }));
    // 可信且已过期 -> MarkExpired（精确分支断言，无 OR 掩盖）
    assert!(matches!(classify_recovery(Queued, true, true), RecoveryAction::MarkExpired { .. }));
    // 终态不重新执行
    for s in [Succeeded, Failed, Cancelled, Expired] {
        assert!(matches!(classify_recovery(s, true, false), RecoveryAction::ReadOnlyVerify { .. }));
    }
    // Unknown：只读核实，绝不重发
    assert!(matches!(classify_recovery(Unknown, true, false), RecoveryAction::ReadOnlyVerify { .. }));
}

#[test]
fn success_evidence_rejects_cross_epoch_version_regression_and_contradiction() {
    use crate::tasks::model::{EvidenceKind, TaskEvidence};
    let ev = TaskEvidence { subtask_id: Uuid::nil(), journal_epoch: Uuid::nil(), record_version: 2,
        observed_boot_id: "boot-b2".into(), pre_boot_id: "boot-b1".into(), current_boot_id: "boot-b2".into(),
        evidence_kind: EvidenceKind::BootTransitionObserved, reliable_attempted: true };
    let baseline = EvidenceBaseline { journal_epoch: Uuid::nil(), record_version: 1, confirmed_failed_versions: Vec::new() };
    // 正例：同 epoch、record_version 合法递增、无矛盾失败记录 -> 成功
    assert!(success_evidence_satisfied("boot-b1", &ev, &baseline));
    // 负例 1：跨 journal_epoch 拒绝
    let mut cross_epoch = ev.clone();
    cross_epoch.journal_epoch = Uuid::from_u128(7);
    assert!(!success_evidence_satisfied("boot-b1", &cross_epoch, &baseline));
    // 负例 2：record_version 倒退（不大于基线）拒绝
    let mut regressed = ev.clone();
    regressed.record_version = 1;
    assert!(!success_evidence_satisfied("boot-b1", &regressed, &baseline));
    // 负例 3：存在已确认 Failed 矛盾记录 -> 拒绝
    let contradicted = EvidenceBaseline { journal_epoch: Uuid::nil(), record_version: 1, confirmed_failed_versions: vec![2] };
    assert!(!success_evidence_satisfied("boot-b1", &ev, &contradicted));
}

#[test]
fn main_task_outcome_corrected_by_late_evidence() {
    use crate::tasks::model::SubTaskState::*;
    assert_eq!(calculate_main_task_outcome(&[Succeeded, Succeeded]), (MainTaskState::Completed, Some(MainTaskOutcome::Success)));
    assert_eq!(calculate_main_task_outcome(&[Succeeded, Unknown]), (MainTaskState::Completed, Some(MainTaskOutcome::Unknown)));
    assert_eq!(calculate_main_task_outcome(&[Succeeded, Failed]), (MainTaskState::Completed, Some(MainTaskOutcome::Partial)));
    assert_eq!(calculate_main_task_outcome(&[Cancelled, Cancelled]), (MainTaskState::Completed, Some(MainTaskOutcome::Cancelled)));
    assert_eq!(calculate_main_task_outcome(&[Failed, Failed]), (MainTaskState::Completed, Some(MainTaskOutcome::Failed)));
    assert_eq!(calculate_main_task_outcome(&[Queued, Queued]), (MainTaskState::Queued, None));
    assert_eq!(calculate_main_task_outcome(&[Dispatching, Queued]), (MainTaskState::Running, None));
    assert_eq!(calculate_main_task_outcome(&[Held, Succeeded]), (MainTaskState::Held, None));
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

```bash
cargo test --offline --locked -p rsetup-controller dispatch_admission_denies_on_every_failed_precondition --
cargo test --offline --locked -p rsetup-controller recovery_never_plans_a_resend_for_possibly_sent_states --
cargo test --offline --locked -p rsetup-controller success_evidence_rejects_cross_epoch_version_regression_and_contradiction --
cargo test --offline --locked -p rsetup-controller main_task_outcome_corrected_by_late_evidence --
```
预期：若 Step 0 门禁未过，本条命令先行核对**只测试不含 `BoardClient` 的模块**；四条命令各恰好执行 1 个用例并因业务断言失败（准入桩恒 `Admit`；恢复桩对所有状态返回 `ReadOnlyVerify` 故 `Queued + 不可信期限` 的 `MarkHeld` 断言失败；成功证据桩恒 `true` 故跨 epoch/版本倒退/矛盾失败记录三条负例断言失败；汇总桩恒 `(Running, None)`）。

- [ ] **Step 3: 最小实现**

1. `admit_for_dispatch` 严格按 04 §5.1 顺序判定：`state != Queued` → `Deny(PermissionDenied)`；`!actor_active || !has_reboot || !admission_approved` → `PermissionDenied`；`!control_flow_healthy` → `DeviceOffline`；`!capability_supported || !persistent_journal_ok` → `CapabilityUnavailable`；`!queue_deadline_valid` → `TimeUncertain`（调用方据此置 `held`）；`!lock_owned` → `DeviceBusy`；否则 `Admit`。
2. `classify_recovery` 分支表（调用方先判定 `sub_tasks.queue_deadline` 可信性：不可信 → `queue_deadline_trustworthy = false` 且 `queue_deadline_expired` 传 `false`；可信时以期限对照可信当前时刻求得 `queue_deadline_expired`）：
   - `Queued`：`!queue_deadline_trustworthy` → `MarkHeld`（忽略 expired）；可信且 `queue_deadline_expired` → `MarkExpired`；可信且未过期 → `ReadOnlyVerify`（重新核验后由调度器决定，**不直接发送**）；
   - `Dispatching | Accepted | Verifying | Unknown`：`ReadOnlyVerify`（**永不**产生发送动作）；
   - 终态：`ReadOnlyVerify`（幂等，不做任何写）。
3. `calculate_main_task_outcome`：任一 `Dispatching/Accepted/Verifying` 或“批次已开始且仍有 `Queued`” → `(Running, None)`；无执行中且全 `Queued` → `(Queued, None)`；无执行中、无 `Queued`、至少一个 `Held` → `(Held, None)`；全部终态 → `(Completed, outcome)`，其中含 `Unknown` → `Unknown`；全 `Succeeded` → `Success`；全 `Cancelled` → `Cancelled`；有成功也有非成功 → `Partial`；其余 → `Failed`。
4. `success_evidence_satisfied(expected_pre_boot, ev, baseline)` 逐条实现 04 §6 的**全部**条件：`reliable_attempted == true`；`ev.pre_boot_id == expected_pre_boot`（`expected_pre_boot` 作为入参从 DB `sub_tasks.pre_boot_id` 读取传入，非未定义结构体字段）；`ev.current_boot_id != ev.pre_boot_id`；`ev.current_boot_id == ev.observed_boot_id`；`ev.evidence_kind == BootTransitionObserved`；同一 `journal_epoch`（`ev.journal_epoch == baseline.journal_epoch`，缺省口径：无已持久化记录时基线取 DB 读到的该子任务当前 epoch）；`ev.record_version` 合法递增（`ev.record_version > baseline.record_version`，缺省口径：无已持久化记录时基线取 0）；无已确认矛盾 `Failed` 记录（`baseline.confirmed_failed_versions` 为空，缺省口径：空集合即无已确认失败）。任何一项不满足即 `false`（仅 boot 变化、断线、Pong、`TASK_NOT_FOUND`、跨 epoch、版本倒退、矛盾失败记录一律不判成功）。
5. `TaskScheduler::run_once`（仅在 Step 0 wire DTO 交付并获批后由包含 `BoardClient` 的实现启用）：严格顺序：①按 `admit_for_dispatch` 取至多 `GLOBAL_DISPATCH_SLOTS` 个候选；②`prepare(device, sub_task_id, expected_boot_id)`（沿用 Task 5 门禁通过后的 `BoardClient`）取得 `RebootTicket` 并**保存 `boot_id` 与 `valid_for_ms`**；**`prepare` 失败（`dispatching` 意图未提交前）可明确 `failed`（对应具体 `reason_code`）并释放自己的锁**；③`prepare` 成功后**再次**核验 actor 权限与 `authz_epoch`、锁归属；④短事务提交 `dispatching` 意图（`TaskIntent`：controller/session/stream 代际、`pre_boot_id`、`ticket_digest`、请求摘要、`observe_deadline`）；⑤**仅** `COMMIT` 成功后，在事务外**至多一次**调用 `execute(device, &ticket, &request)`（`request` 由当前进程仍持有的 `ticket` 明文 `execution_token` 加已提交意图的 `sub_task_id`/`pre_boot` 字段构造 `RebootExecute`（保留 `schema_version=1` 字段要求）并与其一致核对：发送前核对 `ticket.sub_task_id == request.sub_task_id`，不匹配不发并记 `ProtocolMismatch`；`DB`/`audit` 只存摘要、不持久化明文 token，恢复从不构建 `execute`、不重发）；⑥`execute` `Err(Timeout)`/`Err(Transport)` → `verifying`（**绝不**判 `failed`、**绝不**重发）；⑦同时只允许当前进程为该子任务保留一个发送协程（以内存 `HashSet<Uuid>` 记账，重复调用直接跳过）。若 wire DTO 缺席，调度器停留在纯函数纯结构阶段，保持 `BLOCKED`。
6. `recover_process(repo, board_client, process_epoch)`：`board_client` 由调用方显式注入（集成入口以 `Arc<dyn BoardClient>` 构造，与"对象安全编译证明"段的 `TaskApi` adapter 注入路径同源）；先读 DB 重建锁与未决集 → 对 `Dispatching/Accepted/Verifying/Unknown` 只调用 `board_client.task_record`（只读）并把结果写入 `recovery_checks`（`detail_redacted` 去敏）（`task_record` 返回 `Option<TaskRecord>`：`Ok(None)` 即 TASK_NOT_FOUND(1006)，仅表示查询时未找到记录，**不证明** `execute` 未执行，不单独推进状态；查询 `Err(Timeout)`/`Err(Transport)` 不推进出 `verifying`，保持 `verifying` 直到观察窗口 expired 才转 `unknown`，`unknown` 阶段查询超时保持 `unknown`）→ 按 `classify_recovery` 处理 `Queued`；`RecoverySummary.execute_calls` 由计数断言恒为 0（实现中不得存在任何 `execute` 调用点）。

```rust
// crates/rsetup-controller/src/tasks/scheduler.rs 纯逻辑实现（Task 5 Step 3）：
// admit_for_dispatch 函数体归属 scheduler.rs、替换 Step 1 桩；AdmissionVerdict 为该文件内
// 已定义符号（同文件直接引用，无需新增 use），scheduler 既有内容不改动。
use crate::tasks::model::{ReasonCode, SubTaskState};

pub fn admit_for_dispatch(
    state: SubTaskState,
    actor_active: bool,
    has_reboot: bool,
    admission_approved: bool,
    control_flow_healthy: bool,
    capability_supported: bool,
    persistent_journal_ok: bool,
    queue_deadline_valid: bool,
    lock_owned: bool,
) -> AdmissionVerdict {
    if state != SubTaskState::Queued {
        return AdmissionVerdict::Deny { reason_code: ReasonCode::PermissionDenied };
    }
    if !actor_active || !has_reboot || !admission_approved {
        return AdmissionVerdict::Deny { reason_code: ReasonCode::PermissionDenied };
    }
    if !control_flow_healthy {
        return AdmissionVerdict::Deny { reason_code: ReasonCode::DeviceOffline };
    }
    if !capability_supported || !persistent_journal_ok {
        return AdmissionVerdict::Deny { reason_code: ReasonCode::CapabilityUnavailable };
    }
    if !queue_deadline_valid {
        return AdmissionVerdict::Deny { reason_code: ReasonCode::TimeUncertain };
    }
    if !lock_owned {
        return AdmissionVerdict::Deny { reason_code: ReasonCode::DeviceBusy };
    }
    AdmissionVerdict::Admit
}
```

```rust
// crates/rsetup-controller/src/tasks/recovery.rs 纯逻辑实现（Task 5 Step 3）：
// admit_for_dispatch/AdmissionVerdict 归属 scheduler.rs（见上块），本块不重复定义、不 re-export。
use crate::tasks::model::{EvidenceKind, MainTaskOutcome, MainTaskState, SubTaskState, TaskEvidence};
use uuid::Uuid;

pub fn classify_recovery(
    state: SubTaskState,
    queue_deadline_trustworthy: bool,
    queue_deadline_expired: bool,
) -> RecoveryAction {
    match state {
        SubTaskState::Queued => {
            if !queue_deadline_trustworthy {
                RecoveryAction::MarkHeld { subtask_id: Uuid::nil() }
            } else if queue_deadline_expired {
                RecoveryAction::MarkExpired { subtask_id: Uuid::nil() }
            } else {
                RecoveryAction::ReadOnlyVerify { subtask_id: Uuid::nil() }
            }
        }
        // 存活进行中组：Held 是存活态，并入本组（移除不存在旧状态 Preflight/Received/Cancelling）
        SubTaskState::Held | SubTaskState::Dispatching | SubTaskState::Accepted | SubTaskState::Verifying
            | SubTaskState::Unknown => {
            RecoveryAction::ReadOnlyVerify { subtask_id: Uuid::nil() }
        }
        // 终态组
        SubTaskState::Succeeded | SubTaskState::Failed | SubTaskState::Cancelled | SubTaskState::Expired => {
            RecoveryAction::ReadOnlyVerify { subtask_id: Uuid::nil() }
        }
    }
}

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

pub fn calculate_main_task_outcome(states: &[SubTaskState]) -> (MainTaskState, Option<MainTaskOutcome>) {
    if states.iter().any(|s| matches!(s, SubTaskState::Dispatching | SubTaskState::Accepted | SubTaskState::Verifying))
        || (states.iter().any(|s| *s != SubTaskState::Queued) && states.iter().any(|s| *s == SubTaskState::Queued))
    {
        return (MainTaskState::Running, None);
    }
    if states.iter().all(|s| *s == SubTaskState::Queued) {
        return (MainTaskState::Queued, None);
    }
    if states.iter().any(|s| *s == SubTaskState::Held) {
        return (MainTaskState::Held, None);
    }
    let outcome = if states.iter().any(|s| *s == SubTaskState::Unknown) {
        Some(MainTaskOutcome::Unknown)
    } else if states.iter().all(|s| *s == SubTaskState::Succeeded) {
        Some(MainTaskOutcome::Success)
    } else if states.iter().all(|s| *s == SubTaskState::Cancelled) {
        Some(MainTaskOutcome::Cancelled)
    } else if states.iter().any(|s| *s == SubTaskState::Succeeded) {
        Some(MainTaskOutcome::Partial)
    } else {
        Some(MainTaskOutcome::Failed)
    };
    (MainTaskState::Completed, outcome)
}
```

- [ ] **Step 4: 运行并确认 GREEN**

```bash
cargo test --offline --locked -p rsetup-controller dispatch_admission_denies_on_every_failed_precondition --
cargo test --offline --locked -p rsetup-controller recovery_never_plans_a_resend_for_possibly_sent_states --
cargo test --offline --locked -p rsetup-controller success_evidence_rejects_cross_epoch_version_regression_and_contradiction --
cargo test --offline --locked -p rsetup-controller main_task_outcome_corrected_by_late_evidence --
```
预期：各执行 1 个用例，全部 PASS（4 用例 PASS，0 失败）。

- [ ] **Step 5（严格条件门禁）：跨进程恢复测试**

**前置**：Task 3 Step 5 的全部门禁 + Task 5 Step 0 的 DTO 门禁已通过 + 受控 `BoardClient` 替身已按获批 DTO 编译可用。满足后才创建 `tests/controller_recovery.rs` 并运行：

```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test controller_recovery -- --ignored --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test controller_recovery -- --ignored --test-threads=1
```
必须覆盖（**必须是真的第二个进程**，通过重新 `exec` 测试二进制或独立的 `#[test]` 进程入口启动恢复，绝不在同进程内调用纯函数充当证据）：
- `crash_after_intent_never_auto_resends`：在“`dispatching` 意图已提交、`execute` 未发送”窗口终止进程 → 新进程运行 `recover_process` → 断言该子任务**未回到 `queued`**、`device_operation_locks` 仍属旧 owner 与旧 `generation`、`BoardClient::execute` 计数为 0，且发生了只读 `task_record` 调用；
- `late_evidence_corrects_outcome_without_reexecution`：注入迟到有效证据 → `main_tasks.outcome`/计数被修正且 `revision` 递增，子任务**不回到** `running`，后续新任务的锁**未被删除**。

预期：各引擎执行用例数 **> 0** 且全部 PASS，记录引擎版本。缺 URL/fixture/备份/许可证时**不运行**并记录 `BLOCKED`；**禁止**用 `return`、空断言或 mock 调用次数替代持久性断言。

- [ ] **Step 6: 静态检查与提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/tasks/scheduler.rs crates/rsetup-controller/src/tasks/recovery.rs \
  crates/rsetup-controller/src/tasks/model.rs
# board_client.rs / evidence.rs 仅在 Step 0 门禁通过后加入
# tests/controller_recovery.rs 仅在 Step 5 门禁通过后加入
git commit -m "feat(controller): intent-commit-first scheduler and read-only crash recovery"
```

---

### Task 6: 必要任务 HTTP 端点（time-review、release-lock、分页授权子集投影）(Task HTTP Surface)

**Files:**
- Create: `crates/rsetup-controller/src/http_tasks/mod.rs`
- Create: `crates/rsetup-controller/src/http_tasks/decision.rs`
- Create: `crates/rsetup-controller/src/http_tasks/routes.rs`
- Create: `crates/rsetup-controller/src/http_device_status.rs`（正式 Runtime 路由：`GET /api/v1/devices/{id}/status`）
- Modify: `crates/rsetup-controller/src/lib.rs`（仅根导出 `pub mod http_tasks;`、`pub mod http_device_status;`）
- Test: `crates/rsetup-controller/tests/task_http.rs`

`lib.rs`/`main.rs` 顶层生产装配归**唯一独立集成任务所有者**（合并三子 router、路径集互斥、不重复注册 `/healthz`、`/readyz`）：本任务只提供组件子 router `build_tasks_router(HttpTasksState) -> Router<()>` 与 `build_device_status_router(DeviceStatusState) -> Router<()>`，**不自改 `main.rs`**、不自设顶层合并入口、不宣称生产挂载完成（原文件表 `Modify: main.rs` 行删除，不挂“未分配”）。

**Interfaces:**
- Consumes: Task 2 `SubTaskState`/`ReasonCode`/`MainTaskState`/`MainTaskOutcome`/`EvidenceKind`；Task 4 `TaskService`、`TaskRepository`、`SubmitOutcome`（仅 ID）、`PreviewOutcome`/`PreviewTarget`、`CancelRequest`、`CancelOutcome`、`TimeReviewEvidenceKind`、`ReleaseRequest`；既有 `AuthServiceApi`、`Session`、`HttpSecurityConfig`、`check_pre_session`/`check_session_write`、`ok_response`/`err_response`/`request_id`（`src/http_api.rs`）、`crate::model::decimal_u64_json`（`src/model.rs:21`）、`crate::time::evidence::TimeEvidence`/`TimeQuality`（仅派生 `PartialEq`，**未派生 `Eq`**，`src/time/evidence.rs:53-63`；含它的新 DTO **不得** `derive(Eq)`，也不改旧 `TimeEvidence` 派生）、`EndpointId`（`src/http_auth.rs:106`，现仅 `Me`/`Sessions`）。
- Consumes Identity 共享 gate（`http_gate` 共享边界，`pub(crate)`，由 Identity 任务一次性加入；**`http_tasks` 内不自写 Host/Cookie/CSRF 检查**、不占用 `EndpointId::Me` 读桶）：
  - `pub(crate) async fn authenticate_read(state: &HttpAuthState, headers: &HeaderMap, endpoint: EndpointId) -> Result<Session, Response>`
  - `pub(crate) async fn authenticate_write(state: &HttpAuthState, headers: &HeaderMap) -> Result<Session, Response>`
  - `pub(crate) async fn read_json_body<T: DeserializeOwned>(headers: &HeaderMap, body: Body) -> Result<T, Response>`（强制 `Content-Type: application/json` + body ≤ `MAX_BODY_BYTES` = 1 MiB，超限固定 400）
  - `pub(crate) fn require_admin_session(session: &Session) -> Result<(), Response>`
  限速：读按 (EndpointId, session digest) 50/s 且按 GET 模板隔离，写按 session digest 20/s；共享 gate 未就位时本任务标 `BLOCKED`，不自写第二套。
- Produces:
  - Runtime 本地普通业务 gate helper（`http_tasks/mod.rs` 新增本地函数，`pub(crate)`，**不扩展** Identity 共享 gate、不依赖不存在共享导出）：
    `pub(crate) fn require_business_session(session: &Session) -> Result<(), Response>` — `session.user.must_change_password`（规格 01 A-02 临时密码）时返回 403 `PASSWORD_CHANGE_REQUIRED`（message_key `auth.password_change_required`、params `{}`，用既有 `err_response(status, code, message_key, params)`，`src/http_api.rs:35`）；否则 `Ok(())`。**不要求** `is_admin`（admin 临时密码同样 403）、不复制 Host/Cookie/CSRF；缺失会话仍严格 401 `AUTH_REQUIRED`（不以 401|403 放宽）。TDD：Step 1 恒 Ok 可编译桩 + 2 个局部行为测试、Step 2 第九条命令 RED、Step 3 GREEN（计划代码，本次均未运行）。
  - 核心 DTO 全部字段明确完整定义（避免不存在类型或字段漂移）；`TaskDetail`/`TaskPage` **只是内部 TaskApi 返回值/投影入参**，不是直接 serde HTTP 模型（已移除 `Serialize`/`Deserialize` 派生与 `deny_unknown_fields`）；**所有响应只能由独立 HTTP 投影生成 `serde_json::Value`**：10 counts 完整 u64 十进制字符串、`revision` 用 `revision.to_string()`、`TimeEvidence` 合法 JSON 对象均在投影层显式编码，再 `ok_response(value)`；直接 `Json<TaskDetail>`、`ok_response(&TaskDetail)`、`serde_json::to_value(TaskDetail)` 均被拒绝（那会把 `revision`/`counts` 退化成 JSON number）：
```rust
use crate::time::evidence::TimeEvidence;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewScope { Full, AuthorizedSubset }

/// 本任务唯一 operation（02 §6）：JSON 为 `"reboot"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOperation { Reboot }

/// 内部全集计数：10×u64（**不用 usize**）。跨 serde 时每个字段**必须**经
/// `crate::model::decimal_u64_json` 或显式 `to_string()` HTTP 投影输出为
/// 无符号 u64 十进制字符串；10 键全部存在（零值 `"0"`），拒绝负值/前导零/
/// 溢出/JSON number；故不直接 `derive(Serialize)` 整结构成 JSON number。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskCounts {
    pub queued: u64,
    pub held: u64,
    pub dispatching: u64,
    pub accepted: u64,
    pub verifying: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub unknown: u64,
    pub cancelled: u64,
    pub expired: u64,
}

/// 本次用户批准新增的唯一响应字段（批准来源：本会话用户对
/// `approve-task-cancel-action` 的明确批准，见跨线接口清单 §5）：
/// 只表示“可请求取消”（不保证一定成功），不提供隐藏数量、不赋予
/// `device.task.read`；缺字段时前端必须关闭 cancel 入口。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskActions {
    pub can_cancel: bool,
}

/// MainTask 授权投影：列表项与详情 `TaskDetail` **同形**（02 §1 响应投影：
/// id、operation、state、outcome、revision、created_time、view_scope、counts）。
/// 含 `TimeEvidence`（仅 `PartialEq`），故**不得** `derive(Eq)`；不改旧
/// `TimeEvidence` 的派生。`MainTaskState`（4 态 queued/running/held/completed）
/// 与 `MainTaskOutcome`（5 词 success/cancelled/unknown/partial/failed，
/// 非 completed 为 null）为 Task 2 类型，本任务不重定义；JSON 一律小写。
/// 本 struct **只是内部 TaskApi 返回值/投影入参**（`task_detail`/`time_review`
/// 的返回），不是直接 serde HTTP 模型：含 `TaskCounts`（内部 u64 计数，仅
/// `Clone/Copy/Debug/PartialEq/Eq` 派生、无 `Serialize`），直接派生
/// `Serialize` 即 E0277，故仅派生 `Clone/Debug/PartialEq` 并移除已无 serde
/// 意义的 `deny_unknown_fields`。响应必须先经 `project_main_task_summary`
/// 投影为 `serde_json::Value`（10 counts 完整 u64 十进制字符串、`revision`
/// 用 `revision.to_string()`、`TimeEvidence` 为合法 JSON 对象），再
/// `ok_response(value)`；禁止直接 `Json<TaskDetail>`/`ok_response(&TaskDetail)`/
/// `serde_json::to_value(TaskDetail)`。
/// `visible_states` 为内部字段：非 HTTP 外部字段，仅供 handler 在同一次快照中传递给
/// `project_main_task_summary`（无需二次查库）；该字段由 Task 4 读快照阶段产出，与
/// state/outcome/counts 同源；TaskDetail 保持不派生 serde `Serialize`/`Deserialize`（安全约束不变）。
#[derive(Clone, Debug, PartialEq)]
pub struct TaskDetail {
    pub id: Uuid,                        // 02：`id`（删除旧 `main_task_id` 影子名）
    pub operation: TaskOperation,        // `"reboot"`
    pub state: MainTaskState,
    pub outcome: Option<MainTaskOutcome>,
    pub revision: u64,                   // 内部 u64；JSON 十进制字符串（投影层 to_string）
    pub created_time: TimeEvidence,      // 既有时间证据对象，非裸 RFC3339 字符串
    pub view_scope: ViewScope,
    pub counts: TaskCounts,              // 10 键全集摘要、非分页、无 hidden counts
    pub actions: TaskActions,
    // 内部字段（非 HTTP 外部字段、不参与 serde）：当前可见全集 states，
    // 由 Task 4 TaskService 读快照阶段产出，与上方 state/outcome/counts 同源同规则；
    // handler 在同一次快照中直接传递给 `project_main_task_summary`，无需二次查库
    // （闭合 02 §1/F06“同快照投影”契约：可见全集为空 → NotFound → 404，不假零汇总）。
    pub visible_states: Vec<SubTaskState>,
}

/// SubTask 授权投影（ChildItem，02 §1：id、device_id、state、reason_code、
/// revision 及授权可见的 result_evidence）：保留证据但**不暴露
/// `execution_token` 或其 token 摘要**；`result_evidence` 取 Task 2
/// `EvidenceKind`（未产生为 null），其 serde 形状由 Task 2 Step 1 钉死（`#[serde(rename_all = "snake_case")]`
/// 六值串 `attempted`/`boot_transition_observed`/`execution_rejected`/`os_error`/`journal_lost`/`none`，
/// `Option::<EvidenceKind>::None` 为 JSON `null`），本任务不重定义。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildItem {
    pub id: Uuid,                        // 子任务 id（删除旧 `subtask_id` 影子名）
    pub device_id: String,               // 64-hex 小写
    pub state: SubTaskState,
    pub reason_code: Option<ReasonCode>,
    pub revision: u64,                   // 内部 u64；JSON 十进制字符串
    pub result_evidence: Option<EvidenceKind>,
}

/// 02 §1 公共类型 `Page{items,next_cursor}`：默认 50 / max 200。main counts
/// 独立重算，**不得用分页 children 判断全集**。
/// 本 struct **只是内部 TaskApi 返回值/投影入参**（`list_tasks` 的返回），
/// 不是直接 serde HTTP 模型：`items` 含 `TaskDetail`（内含无 `Serialize`
/// 的内部 `TaskCounts`），直接派生 `Serialize` 即 E0277，故仅派生
/// `Clone/Debug/PartialEq` 并移除已无 serde 意义的 `deny_unknown_fields`。
/// 响应必须先投影为 `serde_json::Value`（10 counts 完整 u64 十进制字符串、
/// `revision` 用 `revision.to_string()`）再 `ok_response(value)`；禁止直接
/// `Json<TaskPage>`/`ok_response(&page)`/`serde_json::to_value(TaskPage)`。
#[derive(Clone, Debug, PartialEq)]
pub struct TaskPage {
    pub items: Vec<TaskDetail>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildrenPage {
    pub items: Vec<ChildItem>,
    pub next_cursor: Option<String>,
}

/// 请求 DTO：未知请求字段一律拒绝（`#[serde(deny_unknown_fields)]` → 400）；
/// `expected_revision` 在 JSON 中为**十进制字符串**（无符号，拒绝负值/前导零/
/// 溢出/JSON number），经 `parse_decimal_u64` 严格解析为 u64 后传给 Task 4
/// 服务类型（服务层仍为 u64，请求体形状与 Task 4 一致）。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePreviewRequest {
    pub operation: TaskOperation,
    #[serde(default)]
    pub device_ids: Vec<String>,
    #[serde(default)]
    pub group_ids: Vec<Uuid>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTaskRequest {
    pub preview_token: String,
    pub confirm: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTaskRequest {
    pub expected_revision: String, // 十进制字符串
    pub confirm: bool,
}

/// `decision` 严格 `resume_original_deadline`|`expire`（未知枚举 → 400）；
/// `evidence` 复用 Task 4 `TimeReviewEvidenceKind`（02 §6 evidence）。
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeReviewTaskRequest {
    pub expected_revision: String, // 十进制字符串
    pub decision: TimeReviewDecision,
    pub evidence: TimeReviewEvidenceKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeReviewDecision { ResumeOriginalDeadline, Expire }

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLockRequest {
    pub expected_revision: String, // 十进制字符串
    pub acknowledge_unknown: bool,
    pub reason: String,
}

/// 响应 DTO：`POST /subtasks/{id}/release-lock` 仅 `{released:true,
/// state:"unknown"}` 两字段（删除旧 `ReleaseOutcome` 影子名，无
/// `lock_released`/`revision` 影子 payload）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseLockResponse {
    pub released: bool,        // 固定 true
    pub state: SubTaskState,   // 固定 SubTaskState::Unknown，JSON `"unknown"`
}

/// 响应 DTO：`POST /tasks/{id}/cancel` 的“已授权目标的取消请求接收确认及
/// 可读取结果”（02 §6）：`cancelled` 为本事务内实际 CAS 成功的合格
/// queued/held 子任务 id，**逐目标投影**——对每个实际 CAS 成功的目标，按
/// 同事务当前有效权限（当前 actor/有效 admin 及该 device 的
/// `device.task.read` 事务授权快照，即 `CancelledTarget.readable_in_tx`）过滤，
/// 只返回可读目标（running 主任务也可能含合格目标；无 CAS 成功目标或
/// 全部不可读时为 null，**不强假“全部取消”**；接收确认本身不泄露可见性，
/// 不返回隐藏数，不新增协议字段）；**不写 Main state 为 cancelled**（Main
/// state 仍按 4 态重算，cancelled 只存在于子任务/主 outcome 词表）。`accepted`
/// 仅为**取消请求接收确认**（revision 匹配 + 事务通过），非全部目标成功保证。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelTaskResponse {
    pub accepted: bool,
    pub cancelled: Option<Vec<Uuid>>,
}
```
  - `pub struct TaskApi` 对象安全 trait（`TaskFuture<'a,T>` BoxFuture 形式，待审；HTTP 边界只返回契约形状，内部服务类型不外泄）：
```rust
pub type TaskFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

pub trait TaskApi: Send + Sync {
    fn preview<'a>(&'a self, actor: &'a Session, device_ids: &'a [[u8; 32]], group_ids: &'a [Uuid]) -> TaskFuture<'a, PreviewOutcome>;
    fn submit<'a>(&'a self, actor: &'a Session, preview_token: &'a str, idempotency_key: &'a str) -> TaskFuture<'a, SubmitOutcome>;
    fn list_tasks<'a>(&'a self, actor: &'a Session, state: Option<MainTaskState>, cursor: Option<&'a str>, limit: usize) -> TaskFuture<'a, TaskPage>;
    fn task_detail<'a>(&'a self, actor: &'a Session, task_id: Uuid) -> TaskFuture<'a, TaskDetail>;
    fn task_children<'a>(&'a self, actor: &'a Session, task_id: Uuid, cursor: Option<&'a str>, limit: usize) -> TaskFuture<'a, ChildrenPage>;
    fn cancel<'a>(&'a self, actor: &'a Session, task_id: Uuid, request: CancelTaskRequest) -> TaskFuture<'a, CancelTaskResponse>;
    fn time_review<'a>(&'a self, actor: &'a Session, task_id: Uuid, request: TimeReviewTaskRequest) -> TaskFuture<'a, TaskDetail>;
    fn release_lock<'a>(&'a self, actor: &'a Session, subtask_id: Uuid, request: ReleaseLockRequest) -> TaskFuture<'a, ReleaseLockResponse>;
}
```
  - 语义钉死：`submit` 返回的 `SubmitOutcome`（Task 4，`Created`/`Replayed` 仅 ID）只映射为响应 `{task_id, accepted:true}`，**不扩大创建响应**（`state`/`subtask_count` 会泄读投影）；`time_review` 返回**当前 MainTask 授权视图**（与详情同形的 `TaskDetail`，02 §6“当前视图”，删除旧 `TimeReviewOutcome` 影子名）；`cancel` 对已持 reboot+queued/held 的合格目标在事务内 CAS，返回接收确认 + **逐目标**可读取结果（`CancelTaskResponse`，经 `project_cancel_result` 生成；读权限不阻止合法取消行为，只过滤结果；删除旧 `CancelOutcome` 直出）；`release_lock` 仅 `{released:true,state:"unknown"}`（`ReleaseLockResponse`）。
  - `pub struct HttpTasksState { pub api: Arc<dyn TaskApi>, pub auth: Arc<HttpAuthState> }` + `impl FromRef<HttpTasksState> for Arc<HttpAuthState>`
  - `pub fn build_tasks_router(state: HttpTasksState) -> Router<()>`（本任务唯一对外 HTTP 组件；顶层 lib/main 归唯一独立集成任务所有者）
  - 主任务与分页子任务具有各自独立的授权投影纯函数（严禁用单页 children 取代 main 汇总 counts；全集无隐藏总数泄露）：
    - `pub fn project_main_task_summary(id: Uuid, operation: TaskOperation, state: MainTaskState, outcome: Option<MainTaskOutcome>, revision: u64, created_time: &TimeEvidence, visible_states: &[SubTaskState], can_cancel: bool, is_full_access: bool) -> Result<serde_json::Value, ControllerError>`（`/tasks/{id}` 主任务详情与列表项同形的摘要投影；`visible_states` 为**唯一数据源**：counts 对**当前授权全集**子任务状态（普通用户：当前身份 + 当前 `device.task.read` 可见子集；admin：全域、事务内重验）做十态频数聚合，10 键全部存在、值逐键 u64 `to_string` 十进制字符串（**不直接 serde 内部 `TaskCounts`**）；`revision` 用 `to_string`；`created_time` 输出 `TimeEvidence` 对象（既有 `Serialize`，不改其派生、不加 `Eq`）；**F06**：`state`/`outcome` 一律由同一 `visible_states` 经 `calculate_main_task_outcome` 重算——full 与 authorized_subset **各自**按自己可见全集求值，caller 传入的全量 `state`/`outcome` 仅作同源一致性核对、**不得**成为第二真相源（否则可见 Succeeded 却见 Running/Partial/Unknown，泄隐藏 targets）；`visible_states` 为空 → `Err(ControllerError::NotFound)`（既有统一错误映射 → 404，与“不存在”同投影，**不输出零值汇总**）；非 full 时 `view_scope = "authorized_subset"`；`actions.can_cancel` 由入参传入（合法操作权限快照独立输入、不当任务 read；handler 对可见目标逐条 `cancel_permitted` 预计算）；绝不输出 `total_targets` 或 `hidden_count`）
    - `pub struct ChildEntry { pub id: Uuid, pub device_id: String, pub state: SubTaskState, pub reason_code: Option<ReasonCode>, pub revision: u64, pub result_evidence: Option<EvidenceKind>, pub eligible: bool }`
    - `pub fn project_task_children(entries: &[ChildEntry], next_cursor: Option<&str>) -> serde_json::Value`（`/tasks/{id}/children` 分页子任务列表的投影：输出 `Page{items,next_cursor}`（02 §1）；items 仅含 `eligible == true` 项——`eligible` 由**调用方**按当前身份计算（普通 = 当前持 `device.task.read`，admin = 事务内重验后恒 true）；字段为 `id`/`device_id`/`state`/`reason_code`/`revision`（十进制字符串）/`result_evidence`（不暴露 `execution_token` 或其 token 摘要）；**不含 `view_scope`/`counts`**（旧单页计数形状废止，main counts 独立重算）；绝不输出 `total_targets` 或 `hidden_count`）
  - `pub enum TaskCursorScope { Tasks, TaskChildren }`（端点上下文，固定域分隔字节 `0x01`/`0x02`）、`pub fn encode_task_cursor(endpoint: TaskCursorScope, created_at_ms: i64, tie_break_id: Uuid, secret: &[u8; 32]) -> String`、`pub fn decode_task_cursor(endpoint: TaskCursorScope, cursor: &str, secret: &[u8; 32]) -> Result<(i64, Uuid), ControllerError>`（`/tasks` 以 `Tasks` 传主任务 `id` 作 `tie_break_id`；`/tasks/{id}/children` 以 `TaskChildren` 传子任务 `id`；二者复用同一编解码但游标**不得跨端点复用**——端点标识参与 HMAC 输入，解码以当前端点上下文重算 MAC，跨端点必然校验失败）；字节布局钉死：`payload 24B = created_at_ms（大端 i64，8B）‖ tie_break_id（16B）` + `tag 8B`（`HMAC-SHA256(secret, b"rsetup-task-cursor-v1/" ‖ scope_byte ‖ payload)` 前 8B 截断，恒定时间比较）共 32B → **64 小写 hex**，与接口所有权账本《第 5 轮修复仲裁记录》决议④补充（2026-10-14）UUID 键 profile（24B payload + 8B tag = 32B → 64 小写 hex）尺寸完全一致
  - `pub fn clamp_page_limit(requested: Option<usize>) -> Result<usize, ControllerError>`（默认 50、max 200，即 1..=200，非法 `InvalidArgument`）
  - `pub fn parse_idempotency_key(value: Option<&str>) -> Result<String, ControllerError>`（严格 canonical 小写 UUIDv4，复用 Task 4 `is_canonical_uuid_v4_key`；缺失或非法 `InvalidArgument`）
  - `pub fn parse_decimal_u64(value: &str) -> Result<u64, ControllerError>`（请求体 `expected_revision`：无符号十进制字符串，拒绝负值/前导零/溢出/空）
  - `pub fn project_cancel_result(targets: &[CancelledTarget]) -> CancelTaskResponse`（`/tasks/{id}/cancel` 逐目标取消结果投影；内部小类型 `pub struct CancelledTarget { pub id: Uuid, pub device_id: [u8; 32], pub readable_in_tx: bool }`（不暴露到 HTTP wire）：`readable_in_tx` 来自当前 actor（或有效 admin）及该 device 的 `device.task.read` 事务授权快照，**不来自**客户端声明、旧 `Session.is_admin` 或 `has_any_taskread` 类整批布尔；投影仅 map `readable_in_tx == true` 的目标 `id`，空可读结果（含无 CAS 成功目标）→ `cancelled = None`，否则 `Some(filtered ids)`；`accepted` 恒 true（仅为取消请求接收确认，非全部成功保证）；不返回隐藏数、不新增协议字段）
  - `pub fn cancel_permitted(is_admin: bool, is_owner: bool, has_target_reboot: bool, state: SubTaskState) -> bool`（合格目标 = `queued`/`held`；admin：合格即可；普通：`is_owner && has_target_reboot && 合格`；用于 `actions.can_cancel` 预计算；user 已批准其按操作权限/状态独立判定、不偷与 `device.task.read` 绑定）、`pub fn release_lock_permitted(is_admin: bool, acknowledge_unknown: bool, reason: &str, state: SubTaskState) -> bool`、`pub fn time_review_permitted(is_admin: bool, held_count: usize) -> bool`
  - 端点契约（响应统一为既有 `ok_response`/`err_response` 信封（`src/http_api.rs`）：成功 `{data:...,request_id}`，失败 `{error:{code,message_key,params},request_id}`；所有 `revision`/`counts` 值为**十进制字符串**，`created_time` 为 `TimeEvidence` 对象，不裸 RFC3339 字符串）：
```jsonc
// POST /api/v1/task-previews   req: {"operation":"reboot","device_ids":["<64hex>"],"group_ids":["<uuid>"]}
//   resp: {"preview_token":"<64hex>","expires_in_ms":60000,
//          "targets":[{"device_id":"<64hex>","eligible":true,"reason":null}]}
//   —— `operation` 严格 `"reboot"`（未知枚举 400）；`reason` 为 04 ReasonCode 或 null（字段名按 02 §6）；
//      `expires_in_ms` 固定 60s（单调、随进程代际失效）由路由层补齐（不改 Task 4 `PreviewOutcome` 形状）；至少最低可见
// POST /api/v1/tasks   headers: Idempotency-Key: <canonical lowercase UUIDv4>
//   req: {"preview_token":"<64hex>","confirm":true}
//   resp 202（创建）与合法重放（同 key 同内容、返回原任务）均为: {"task_id":"<uuid>","accepted":true}
//   —— 仅此两字段，不扩大为 state/subtask_count（会泄读投影）；合法重放允许 preview 已过期/已消费/已删除
//   409 IDEMPOTENCY_CONFLICT（同 key 不同内容）；409 INVALID_STATE（首次提交但 preview 已过期/已消费）；
//   404 NOT_FOUND（提交时目标已完全不可见，整体不创建）；400 INVALID_ARGUMENT（未知字段/confirm != true/幂等键非法）
//   —— 目标 busy 不整体报错：任务创建，忙目标成为 failed/DEVICE_BUSY 子任务（不静默丢目标、绝不排队）
// GET /api/v1/tasks?limit=50&cursor=...&state=...
//   resp: {"items":[{"id":"<uuid>","operation":"reboot","state":"completed","outcome":"unknown",
//                    "revision":"4",
//                    "created_time":{"system_wall_utc":"2026-10-16T00:00:00Z","reference_utc":"2026-10-16T00:00:00Z",
//                             "quality":"system_fallback","clock_epoch":"<uuid>","source":null,
//                             "sample_age_ms":null,"offset_ms":null,"uncertainty_ms":null},
//                    "view_scope":"authorized_subset",
//                    "counts":{"queued":"0","held":"0","dispatching":"0","accepted":"0","verifying":"0",
//                              "succeeded":"1","failed":"0","unknown":"1","cancelled":"0","expired":"0"},
//                    "actions":{"can_cancel":false}}],
//          "next_cursor":"<64hex>"|null}
//   —— 普通用户仅见本人任务，counts 为当前授权全集摘要（10 键齐全、零值 "0"、十进制字符串、无 hidden counts）；
//      admin 全域、事务内重验
// GET /api/v1/tasks/{id}
//   全权:  {"id":"<uuid>","operation":"reboot","state":"held","outcome":null,"revision":"7",
//           "created_time":{TimeEvidence 对象},"view_scope":"full",
//           "counts":{...10 键...},"actions":{"can_cancel":true}}
//   授权子集: 同形，"view_scope":"authorized_subset"，counts 按授权全集重算，actions.can_cancel 按当前身份计算
//   —— 非 owner 非 admin 一律 404（与“不存在”同投影）；无 total_targets / 无隐藏目标计数
//   —— 详情端点**不带**分页子任务清单；子任务一律走下面的 /children（规格 8 条路由，不得合并）
// GET /api/v1/tasks/{id}/children?limit=50&cursor=...
//   resp: {"items":[{"id":"<uuid>","device_id":"<64hex>","state":"accepted",
//           "reason_code":null,"revision":"3","result_evidence":"boot_transition_observed"}],
//          "next_cursor":"<64hex>"|null}
//   —— Page{items,next_cursor}（02 §1）；items 仅当前授权目标（普通：本人 + 当前 device.task.read 子集；
//      admin：全域、事务内重验）；隐藏目标既不返回、也不计入任何计数；**无 view_scope、无 counts**（main
//      counts 独立重算，与 Page 分离）；result_evidence 保留证据但不暴露 execution_token 或其 token 摘要；
//      非 owner 非 admin 一律 404；DB 错误 503 STORAGE_UNAVAILABLE，绝不降级为空 items 或零计数
// POST /api/v1/tasks/{id}/cancel   req: {"expected_revision":"3","confirm":true}
//   resp: {"accepted":true,"cancelled":["<可读目标 uuid>"]|null}
//   —— 接收确认 + **逐目标**可读取结果：`cancelled` 取本事务实际 CAS 成功的合格 queued/held 子任务
//      （running 主任务也可能含这些目标），对每个实际 CAS 成功的目标按同事务当前有效权限
//      （当前 actor/有效 admin 及该 device 的 device.task.read 事务授权快照）过滤投影：
//      只返回可读目标，隐藏目标既不列出也不计数；无 CAS 成功目标或全部不可读则
//      "cancelled":null（**不强假“全部取消”**）；`accepted` 仅为取消请求接收确认、非全部成功
//      保证；读权限不阻止合法取消行为，只过滤结果；无隐藏数、无新增协议字段；**不写 Main state 为 cancelled**
//   409 REVISION_CONFLICT（expected_revision 不匹配，事务重验 + CAS）；404 NOT_FOUND（非 owner 非 admin 或不存在）
// POST /api/v1/tasks/{id}/time-review   req: {"expected_revision":"3","decision":"resume_original_deadline","evidence":"reference_deadline_proven"}
//   （decision 严格 resume_original_deadline|expire；未知字段/未知枚举 400 INVALID_ARGUMENT）
//   resp: 当前 MainTask 授权视图（TaskDetail 同形，counts 按当前授权全集重算）
//   —— 对该主任务全部 held 子任务原子生效、绝不延长原期限（`time_review_allows_extend` 兜底拒绝）；
//      无 held 400 INVALID_ARGUMENT（message_key task.no_held）；409 REVISION_CONFLICT
// POST /api/v1/subtasks/{id}/release-lock
//   req: {"expected_revision":"4","acknowledge_unknown":true,"reason":"<非空>"}
//   resp: {"released":true,"state":"unknown"}
//   —— 仅此两字段（无 lock_released / 无 revision 影子 payload）
//   403 PERMISSION_DENIED（非管理员，以 Session.user.is_admin 与事务内重验为准）；
//   400 INVALID_ARGUMENT（acknowledge_unknown != true → message_key task.ack_required；
//   reason 去空白后为空 → task.reason_required）；409 REVISION_CONFLICT
```
  - 统一错误映射（与既有 `src/http_auth.rs` 固定响应同一 code/message_key 体系，不新造码，不得误映射为 `ORIGIN_INVALID`/`BACKEND_UNAVAILABLE`/`Exhausted` 429/通用 500）：
    - 未认证 → 401 `AUTH_REQUIRED`（auth.required）；Host/Origin/CSRF 失败 → 403 `CSRF_INVALID`（security.origin_host / security.csrf）
    - `ControllerError::PermissionDenied` → 403 `PERMISSION_DENIED`
    - `ControllerError::InvalidArgument`（含未知请求字段、未知枚举、非法 cursor、非法十进制）→ 400 `INVALID_ARGUMENT`
    - `ControllerError::NotFound` → 404 `NOT_FOUND`（system.not_found）；`ControllerError::RevisionConflict` → 409 `REVISION_CONFLICT`
    - 限速（读桶 (EndpointId, session digest) 50/s、写桶 session digest 20/s）→ 429 `RATE_LIMITED`（auth.rate_limited）+ `Retry-After`
    - `ControllerError::Database` → 503 `STORAGE_UNAVAILABLE`（system.storage_unavailable；**绝不**降级为空列表、空权限或 404）
    - `ControllerError::ResourceExhausted` → 503 `RESOURCE_EXHAUSTED`（system.resource_exhausted）+ `Retry-After: 60`（**不**映射 429）
    - 其余变体 → 503 `NOT_READY`（system.not_ready，与既有 `backend_error` 一致）；**无通用 500 兜底**
    - `ControllerError::TaskIdempotencyConflict` → 409 `IDEMPOTENCY_CONFLICT`（message_key `task.idempotency_conflict`）；`ControllerError::TaskPreviewInvalidState` → 409 `INVALID_STATE`（message_key `task.preview_invalid_state`）——两码均为既有契约码，不新增 HTTP 码、不通用 500；两变体来自 Task 4「B2 submit 错误边界」的 error.rs 前置；两 message_key 为固定非敏感键（无既有 task.* 词条可用），Web 端 i18n 字典须接线该两词条（Web 计划协作点）。两新变体在兜底之前显式匹配；统一映射落地为步骤⑥唯一映射函数 `pub(crate) fn task_error_response(err: &ControllerError) -> axum::Response`（`src/http_tasks/mod.rs`，全部 8 条任务路由共用），两新分支用既有四参 `err_response(status, code, message_key, params)`（`src/http_api.rs:35`）：

```rust
ControllerError::TaskIdempotencyConflict => err_response(axum::http::StatusCode::CONFLICT, "IDEMPOTENCY_CONFLICT", "task.idempotency_conflict", serde_json::json!({})),
ControllerError::TaskPreviewInvalidState => err_response(axum::http::StatusCode::CONFLICT, "INVALID_STATE", "task.preview_invalid_state", serde_json::json!({})),
```

行为测试（lib 单测，模块 `http_tasks::task_error_mapping_tests`，命令沿用本计划 lib selector 形式；`to_bytes` 为 `task_http` 测试同款既有响应体读取路径）：

```rust
use axum::body::to_bytes;
use crate::ControllerError;
use crate::http_tasks::task_error_response;
async fn status_and_code(err: &ControllerError) -> (u16, String) {
    let resp = task_error_response(err);
    let status = resp.status().as_u16();
    let body = serde_json::from_slice::<serde_json::Value>(&to_bytes(resp.into_body(), 1_048_576).await.unwrap()).unwrap();
    (status, body["error"]["code"].as_str().unwrap().to_owned())
}
#[tokio::test]
async fn idempotency_conflict_maps_to_409_idempotency_conflict() {
    assert_eq!(status_and_code(&ControllerError::TaskIdempotencyConflict).await, (409, "IDEMPOTENCY_CONFLICT".into()));
}
#[tokio::test]
async fn preview_invalid_state_maps_to_409_invalid_state() {
    assert_eq!(status_and_code(&ControllerError::TaskPreviewInvalidState).await, (409, "INVALID_STATE".into()));
}
```

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

`src/http_tasks/decision.rs` 桩（DTO 按 Interfaces 完整落地；投影/判定函数为故意错误的可编译桩，使行为断言 RED 而非编译错误）：

```rust
use uuid::Uuid;
use crate::ControllerError;
use crate::tasks::model::{EvidenceKind, MainTaskOutcome, MainTaskState, ReasonCode, SubTaskState};
use crate::time::evidence::TimeEvidence;

pub struct ChildEntry {
    pub id: Uuid,
    pub device_id: String,
    pub state: SubTaskState,
    pub reason_code: Option<ReasonCode>,
    pub revision: u64,
    pub result_evidence: Option<EvidenceKind>,
    pub eligible: bool,
}

/// 可编译 RED 桩：恒 true，暴露未授权目标。
pub fn cancel_permitted(_is_admin: bool, _is_owner: bool, _has_target_reboot: bool, _state: SubTaskState) -> bool { true }

/// 内部小类型（不暴露到 HTTP wire）：本事务实际 CAS 取消成功的目标，
/// `project_cancel_result` 的入参。`readable_in_tx` 生产组装来自当前 actor
/// （或有效 admin）及该 device 的 `device.task.read` 事务授权快照——**不来自**
/// 客户端声明、旧 `Session.is_admin` 或 `has_any_taskread` 类整批布尔；该生产
/// 组装是后续 TaskService 定义必须履约的依赖（当前切片不假称已实现，也不引入
/// 需要未定义 `AuthzRepo` 的真 SQL fixture）。
pub struct CancelledTarget {
    pub id: Uuid,
    pub device_id: [u8; 32],
    pub readable_in_tx: bool,
}

/// 可编译 RED 桩：未过滤地返回**全部** CAS 成功目标 id（旧“整体可读则全量”
/// 语义：含空目标 `Some([])`、全不可读仍 `Some`），暴露逐目标可读过滤缺失，
/// 使 `cancel_result_projection_filters_unreadable_targets` 负例 RED。
pub fn project_cancel_result(_targets: &[CancelledTarget]) -> CancelTaskResponse {
    CancelTaskResponse {
        accepted: true,
        cancelled: Some(_targets.iter().map(|t| t.id).collect()),
    }
}

/// 可编译 RED 桩：忽略 ack 与 reason。
pub fn release_lock_permitted(_is_admin: bool, _acknowledge_unknown: bool, _reason: &str, _state: SubTaskState) -> bool { true }

/// 可编译 RED 桩：无 held 也允许核验（应为 400）。
pub fn time_review_permitted(_is_admin: bool, _held_count: usize) -> bool { true }

/// 可编译 RED 桩：**原样回显 caller 传入的全量 `state`/`outcome`**（旧“整体透传”语义，
/// 即 F06 未修复时的泄源）、`revision`/`created_time`/`view_scope` 已按契约编码但
/// **无 `counts` 键**、`visible_states` 为空仍 `Ok`（未报 `NotFound`）——使 F06 负例
/// （visible 全 Succeeded 却见 Running/Partial/Unknown）与十进制字符串/counts 契约
/// 断言 RED。返回类型升为 `Result<Value, ControllerError>`，为 NotFound 预留位置
/// （桩恒 `Ok`：空 visible 未报 404 亦为新负例 RED 源）。
pub fn project_main_task_summary(
    id: Uuid,
    operation: TaskOperation,
    state: MainTaskState,
    outcome: Option<MainTaskOutcome>,
    revision: u64,
    created_time: &TimeEvidence,
    _visible_states: &[SubTaskState],
    can_cancel: bool,
    is_full_access: bool,
) -> Result<serde_json::Value, ControllerError> {
    Ok(serde_json::json!({
        "id": id.to_string(),
        "operation": serde_json::to_value(operation).unwrap(),
        "state": serde_json::to_value(state).unwrap(),
        "outcome": serde_json::to_value(outcome).unwrap(),
        "revision": revision.to_string(),
        "created_time": serde_json::to_value(created_time).unwrap(),
        "view_scope": if is_full_access { "full" } else { "authorized_subset" },
        "actions": { "can_cancel": can_cancel }
    }))
}

/// 可编译 RED 桩：恒返回 full 视图且带旧单页 `counts` 形状（无 items 分页语义），
/// 使 children Page 投影与“无 view_scope/counts”独立性断言 RED。
pub fn project_task_children(_entries: &[ChildEntry], _next_cursor: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "view_scope": "full", "counts": { "succeeded": 1 } })
}
```

`tests/task_http.rs` 集成测试（`FakeTaskApi`/fake `AuthServiceApi` **定义在本集成测试文件自身**——lib 侧 `#[cfg(test)]` 模块对集成测试 crate 不可见；用 `tower::ServiceExt::oneshot` 打真实 router）：

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Utc};
use rsetup_controller::auth::service::Session;
use rsetup_controller::ControllerError;
use rsetup_controller::http_tasks::decision::{
    CancelledTarget, ChildEntry, cancel_permitted, project_cancel_result, project_main_task_summary,
    project_task_children, release_lock_permitted, time_review_permitted,
};
use rsetup_controller::http_tasks::{
    ChildrenPage, CancelTaskRequest, CancelTaskResponse, ReleaseLockRequest, ReleaseLockResponse,
    TaskApi, TaskDetail, TaskFuture, TaskPage, TaskOperation, TimeReviewTaskRequest,
};
use rsetup_controller::tasks::model::{EvidenceKind, MainTaskOutcome, MainTaskState, SubTaskState};
use rsetup_controller::tasks::preview::PreviewOutcome;
use rsetup_controller::tasks::service::SubmitOutcome;
use rsetup_controller::time::evidence::{TimeEvidence, TimeQuality};
use tower::ServiceExt;
use uuid::Uuid;

/// 真实字段构造的时间证据 fixture（`TimeEvidence` 仅 `PartialEq`，测试按值断言 JSON）。
fn fixture_time_evidence() -> TimeEvidence {
    let ts = DateTime::parse_from_rfc3339("2026-10-16T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    TimeEvidence {
        system_wall_utc: ts,
        reference_utc: ts,
        quality: TimeQuality::SystemFallback,
        clock_epoch: Uuid::new_v4(),
        source: None,
        sample_age_ms: None,
        offset_ms: None,
        uncertainty_ms: None,
    }
}

#[derive(Default)]
struct FakeTaskApi {
    submit: std::sync::atomic::AtomicUsize,
}

impl FakeTaskApi {
    fn submit_calls(&self) -> usize {
        self.submit.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl TaskApi for FakeTaskApi {
    fn submit<'a>(&'a self, _actor: &'a Session, _preview_token: &'a str, _idempotency_key: &'a str) -> TaskFuture<'a, SubmitOutcome> {
        self.submit.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { unreachable!("boundary test must not reach business") })
    }
    fn preview<'a>(&'a self, _actor: &'a Session, _device_ids: &'a [[u8; 32]], _group_ids: &'a [Uuid]) -> TaskFuture<'a, PreviewOutcome> { Box::pin(async { unreachable!() }) }
    fn list_tasks<'a>(&'a self, _actor: &'a Session, _state: Option<MainTaskState>, _cursor: Option<&'a str>, _limit: usize) -> TaskFuture<'a, TaskPage> { Box::pin(async { unreachable!() }) }
    fn task_detail<'a>(&'a self, _actor: &'a Session, _task_id: Uuid) -> TaskFuture<'a, TaskDetail> { Box::pin(async { unreachable!() }) }
    fn task_children<'a>(&'a self, _actor: &'a Session, _task_id: Uuid, _cursor: Option<&'a str>, _limit: usize) -> TaskFuture<'a, ChildrenPage> { Box::pin(async { unreachable!() }) }
    fn cancel<'a>(&'a self, _actor: &'a Session, _task_id: Uuid, _request: CancelTaskRequest) -> TaskFuture<'a, CancelTaskResponse> { Box::pin(async { unreachable!() }) }
    fn time_review<'a>(&'a self, _actor: &'a Session, _task_id: Uuid, _request: TimeReviewTaskRequest) -> TaskFuture<'a, TaskDetail> { Box::pin(async { unreachable!() }) }
    fn release_lock<'a>(&'a self, _actor: &'a Session, _subtask_id: Uuid, _request: ReleaseLockRequest) -> TaskFuture<'a, ReleaseLockResponse> { Box::pin(async { unreachable!() }) }
}

struct FakeAuthServiceApi;

impl rsetup_controller::http_auth::AuthServiceApi for FakeAuthServiceApi {
    fn is_live_session(&self, _digest: &[u8; 32]) -> bool { false }
    fn login<'a>(&'a self, _username: &'a str, _password: &'a str) -> rsetup_controller::http_auth::AuthFuture<'a, rsetup_controller::auth::service::Login> { Box::pin(async { unreachable!() }) }
    fn authenticate<'a>(&'a self, _raw: &'a str) -> rsetup_controller::http_auth::AuthFuture<'a, Session> { Box::pin(async { unreachable!() }) }
    fn change_password<'a>(&'a self, _session: &'a Session, _current: &'a str, _new_password: &'a str) -> rsetup_controller::http_auth::AuthFuture<'a, ()> { Box::pin(async { unreachable!() }) }
    fn logout<'a>(&'a self, _session: &'a Session) -> rsetup_controller::http_auth::AuthFuture<'a, ()> { Box::pin(async { unreachable!() }) }
    fn authz_epoch(&self) -> rsetup_controller::http_auth::AuthFuture<'_, u64> { Box::pin(async { unreachable!() }) }
    fn list_sessions<'a>(&'a self, _session: &'a Session, _cursor: Option<&'a str>, _limit: usize) -> rsetup_controller::http_auth::AuthFuture<'a, rsetup_controller::auth::service::SessionPage> { Box::pin(async { unreachable!() }) }
    fn revoke_by_alias<'a>(&'a self, _session: &'a Session, _id: &'a str) -> rsetup_controller::http_auth::AuthFuture<'a, bool> { Box::pin(async { unreachable!() }) }
    fn revoke_others<'a>(&'a self, _session: &'a Session) -> rsetup_controller::http_auth::AuthFuture<'a, u64> { Box::pin(async { unreachable!() }) }
}

#[test]
fn main_detail_summary_counts_are_decimal_strings_over_authorized_full_set() {
    let main_id = Uuid::new_v4();
    let created = fixture_time_evidence();
    // 全部 3 个子任务中，普通用户仅对前 2 个有权限；第 3 个（隐藏 target）为 dispatching
    let visible = [SubTaskState::Succeeded, SubTaskState::Unknown];
    let view = project_main_task_summary(
        main_id, TaskOperation::Reboot, MainTaskState::Running, None, 4, &created, &visible, false, false,
    )
    .unwrap();
    assert_eq!(view["view_scope"], "authorized_subset");
    // counts 按当前授权全集重算（不得借全量运行状态）：10 键齐全、值为十进制字符串
    assert_eq!(view["counts"]["succeeded"], "1");
    assert_eq!(view["counts"]["unknown"], "1");
    assert_eq!(view["counts"]["failed"], "0");
    assert_eq!(view["counts"]["dispatching"], "0"); // 隐藏 target 为 dispatching，不计入、不泄露
    assert_eq!(view["counts"]["expired"], "0");
    // 严禁泄露未授权目标存在与总数
    assert!(view.get("total_targets").is_none());
    assert!(view.get("hidden_count").is_none());
    // 契约字段：revision 十进制字符串（非 JSON number）、operation、actions、TimeEvidence 对象
    assert_eq!(view["revision"], "4");
    assert_eq!(view["operation"], "reboot");
    assert_eq!(view["actions"]["can_cancel"], false);
    assert!(view["created_time"]["quality"].is_string());
    assert!(view["created_time"]["clock_epoch"].is_string());

    let full = project_main_task_summary(
        main_id, TaskOperation::Reboot, MainTaskState::Running, None, 4, &created, &visible, true, true,
    )
    .unwrap();
    assert_eq!(full["view_scope"], "full");
    assert!(full.get("total_targets").is_none());
}

#[test]
fn main_summary_recomputes_from_visible_full_set_ignoring_full_state() {
    // F06 负例：可见全集（普通用户可读目标）全 Succeeded；隐藏目标为
    // Running/Failed/Unknown。无论 caller 输入的全量 state/outcome 是什么，
    // 输出只能由 visible 全集重算：completed/success + visible counts。
    let main_id = Uuid::new_v4();
    let created = fixture_time_evidence();
    let visible = [SubTaskState::Succeeded, SubTaskState::Succeeded, SubTaskState::Succeeded];
    let view = project_main_task_summary(
        main_id, TaskOperation::Reboot, MainTaskState::Running, None, 7, &created, &visible, false, false,
    )
    .unwrap();
    // 不得泄隐藏目标的 Running/Failed/Unknown：state/outcome 只由 visible 全集决定
    assert_eq!(view["state"], "completed");
    assert_eq!(view["outcome"], "success");
    // counts 为 visible 全集频数：10 键齐全、十进制字符串、零值 "0"
    assert_eq!(view["counts"]["succeeded"], "3");
    assert_eq!(view["counts"]["failed"], "0");
    assert_eq!(view["counts"]["unknown"], "0");
    assert_eq!(view["counts"]["queued"], "0");
    assert_eq!(view["view_scope"], "authorized_subset");
    // 不输出 hidden 数量（隐藏目标存在与否均不可见）
    assert!(view.get("total_targets").is_none());
    assert!(view.get("hidden_count").is_none());
    assert_eq!(view["revision"], "7");

    // caller 再换一组全量 state/outcome，输出仍只由 visible 全集决定
    let view2 = project_main_task_summary(
        main_id, TaskOperation::Reboot, MainTaskState::Completed, Some(MainTaskOutcome::Partial), 7, &created, &visible, false, false,
    )
    .unwrap();
    assert_eq!(view2["state"], "completed");
    assert_eq!(view2["outcome"], "success");
    assert_eq!(view2["counts"]["succeeded"], "3");

    // visible 全集为空（目标全部隐藏）=> NotFound（404），不假零汇总
    let empty = project_main_task_summary(
        main_id, TaskOperation::Reboot, MainTaskState::Completed, Some(MainTaskOutcome::Success), 7, &created, &[], false, false,
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
    // 普通用户：dev_hidden 无 device.task.read（eligible 由调用方按当前身份计算为 false）
    let entries = vec![
        ChildEntry { id: Uuid::new_v4(), device_id: dev_ok.clone(), state: SubTaskState::Succeeded, reason_code: None, revision: 3, result_evidence: Some(EvidenceKind::BootTransitionObserved), eligible: true },
        ChildEntry { id: Uuid::new_v4(), device_id: dev_hidden.clone(), state: SubTaskState::Succeeded, reason_code: None, revision: 3, result_evidence: None, eligible: false },
    ];
    // Page{items,next_cursor}（02 §1）：旧单页 view_scope/counts 不再定义
    let view = project_task_children(&entries, None);
    assert!(view.get("view_scope").is_none());
    assert!(view.get("counts").is_none());
    assert!(view.get("next_cursor").expect("Page must contain next_cursor").is_null());
    // 仅含当前授权目标，隐藏目标既不在 items 也不计入任何计数；期望 user count 不随页面串扰
    let items = view["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["state"], "succeeded");
    assert_eq!(items[0]["revision"], "3"); // 十进制字符串
    assert_eq!(items[0]["result_evidence"], "boot_transition_observed");
    assert!(items[0].get("total_targets").is_none());

    // admin 全域（事务内重验后 eligible 恒 true）：items 变 2，但本页投影仍不产生 counts
    let full_entries = vec![
        ChildEntry { id: Uuid::new_v4(), device_id: dev_ok, state: SubTaskState::Succeeded, reason_code: None, revision: 3, result_evidence: Some(EvidenceKind::BootTransitionObserved), eligible: true },
        ChildEntry { id: Uuid::new_v4(), device_id: dev_hidden, state: SubTaskState::Succeeded, reason_code: None, revision: 3, result_evidence: None, eligible: true },
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
    assert!(!cancel_permitted(true, true, true, SubTaskState::Dispatching));
    assert!(!cancel_permitted(true, true, true, SubTaskState::Accepted));
    assert!(!cancel_permitted(true, true, true, SubTaskState::Verifying));
    assert!(!cancel_permitted(true, true, true, SubTaskState::Unknown));
    assert!(cancel_permitted(false, true, true, SubTaskState::Queued));
    assert!(!cancel_permitted(false, true, false, SubTaskState::Queued)); // 失去 device.reboot
    assert!(!cancel_permitted(false, false, true, SubTaskState::Queued)); // 非 owner
}

#[test]
fn cancel_result_projection_filters_unreadable_targets() {
    // Uuid 用已锁的合法 v4 常量（`Uuid::from_u128`）；device key bytes 为测试
    // 假值（`[0xAA; 32]`/`[0xBB; 32]`），仅占位、不冒充授权认证。
    let a_id = Uuid::from_u128(0x1234567812344abc80de123456789a01);
    let b_id = Uuid::from_u128(0x1234567812344abc80de123456789a02);

    // A（readable + reboot）/ B（reboot-only、该 device 不可读）：只返回 A。
    let targets = vec![
        CancelledTarget { id: a_id, device_id: [0xAA; 32], readable_in_tx: true },
        CancelledTarget { id: b_id, device_id: [0xBB; 32], readable_in_tx: false },
    ];
    let resp = project_cancel_result(&targets);
    assert!(resp.accepted, "accepted 仅为取消请求接收确认，恒 true");
    assert_eq!(resp.cancelled, Some(vec![a_id]), "不可读 B 不得泄露");

    // 全部目标不可读 → cancelled 为 None（而非空列表，不宣称“全部取消”）。
    let all_hidden = vec![
        CancelledTarget { id: a_id, device_id: [0xAA; 32], readable_in_tx: false },
        CancelledTarget { id: b_id, device_id: [0xBB; 32], readable_in_tx: false },
    ];
    let hidden = project_cancel_result(&all_hidden);
    assert!(hidden.accepted);
    assert_eq!(hidden.cancelled, None);

    // 空目标（无 CAS 成功）→ cancelled 为 None，不误称取消全部（桩 `Some([])` 须失败）。
    let empty = project_cancel_result(&[]);
    assert!(empty.accepted);
    assert_eq!(empty.cancelled, None);
}

#[test]
fn release_lock_requires_admin_ack_and_nonempty_reason() {
    assert!(release_lock_permitted(true, true, "现场已人工核实", SubTaskState::Unknown));
    assert!(!release_lock_permitted(false, true, "现场已人工核实", SubTaskState::Unknown));
    assert!(!release_lock_permitted(true, false, "现场已人工核实", SubTaskState::Unknown));
    assert!(!release_lock_permitted(true, true, "   ", SubTaskState::Unknown));
    assert!(!release_lock_permitted(true, true, "ok", SubTaskState::Succeeded));
}

#[test]
fn time_review_requires_at_least_one_held_subtask() {
    assert!(time_review_permitted(true, 1));
    assert!(time_review_permitted(true, 5));
    assert!(!time_review_permitted(true, 0)); // 无 held -> 400
    assert!(!time_review_permitted(false, 3)); // 非管理员
}

#[tokio::test]
async fn missing_session_never_reaches_business_repository() {
    // 缺会话写请求：先提供合法 Host/Origin/JSON/幂等头，唯独不带 Cookie；
    // 必须精确 401 AUTH_REQUIRED，不能由 Host/Origin/CSRF 的 403 假通过。
    let api = std::sync::Arc::new(FakeTaskApi::default());
    let auth = std::sync::Arc::new(rsetup_controller::HttpAuthState::new(
        std::sync::Arc::new(FakeAuthServiceApi),
        rsetup_controller::http_security::HttpSecurityConfig {
            allowed_hosts: vec!["localhost".into()],
            allowed_origin: Some("http://localhost".into()), // 合成同源 fixture，非实际部署地址
            trust_tls: false,
        },
    ));
    let router = rsetup_controller::http_tasks::build_tasks_router(
        rsetup_controller::http_tasks::HttpTasksState { api: api.clone(), auth },
    );
    let response = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/tasks")
                .header("host", "localhost")
                .header("origin", "http://localhost")
                .header("content-type", "application/json")
                .header("idempotency-key", "3f2504e0-4f89-41d3-9a0c-0305e82c3301")
                .body(Body::from(r#"{"preview_token":"00","confirm":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = axum::body::to_bytes(response.into_body(), 1_048_576).await.unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(envelope["error"]["code"], "AUTH_REQUIRED");
    assert_eq!(envelope["error"]["message_key"], "auth.required");
    assert_eq!(envelope["error"]["params"], serde_json::json!({}));
    assert_eq!(api.submit_calls(), 0, "business repository must not be touched without a session");
}
```

`FakeTaskApi`（含调用计数）与 fake `AuthServiceApi` **定义在 `tests/task_http.rs` 自身**（本切片采用，上例即完整形状）：lib 侧 `#[cfg(test)]` 模块对集成测试 crate **不可见**（`cargo test --test task_http` 编译 lib 为普通依赖时 `cfg(test)` 不生效），直接放在集成测试内即可保证“可编译、非 0 用例”，且不向生产 lib 导出测试符号。若必须落在 lib，则须用 `#[cfg(any(test, feature = "test-support"))]` 模块并在 `Cargo.toml` `[features]` 声明 `test-support = []`（**不新增依赖**），且 Step 2/4 命令统一追加 `--features test-support` 并在提交信息记录。

`src/http_tasks/mod.rs` 业务 gate RED 桩与局部行为测试（**计划代码，本次未运行**；桩恒 Ok，RED 是行为断言而非编译错误；`#[cfg(test)]` 单测与 mod.rs 同文件、对 `--test task_http` 集成测试 crate 不可见（同上一节约定），故走 `--lib` 选择器）：

```rust
// 在 http_tasks/mod.rs 追加的 imports（与 GREEN 同一组；桩阶段仅 unused-import/dead_code 警告、无错误）
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::json;

use crate::auth::service::Session;
use crate::http_api::err_response;

/// Step 1 可编译桩：恒 Ok（故意暂不检查 must_change_password），
/// 使下方行为断言 RED 而非编译错误；Step 2 第九条命令确认 RED 后由 Step 3 替换为 GREEN。
pub(crate) fn require_business_session(_session: &Session) -> Result<(), Response> {
    Ok(())
}

/// 测试模块注册（lib 单测，与 mod.rs 同文件；不向生产 lib 导出测试符号）。
#[cfg(test)]
mod business_session_tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::Value;

    fn session_fixture(is_admin: bool, must_change_password: bool) -> Session {
        Session {
            user: crate::auth::service::IdentityUser {
                id: [7u8; 16],
                username: if is_admin { "admin".into() } else { "alice".into() },
                password_hash: "$argon2id$=synthetic-test-only=".to_string(),
                active: true,
                is_admin,
                must_change_password,
                revision: 1,
            },
            digest: [9u8; 32],
        }
    }

    #[tokio::test]
    async fn temporary_password_forbids_admin_and_plain_with_exact_envelope() {
        for is_admin in [true, false] {
            let resp = require_business_session(&session_fixture(is_admin, true))
                .expect_err("临时密码（含 admin）必须 403");
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "is_admin={is_admin}");
            let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["error"]["code"],
                json!("PASSWORD_CHANGE_REQUIRED"),
                "is_admin={is_admin}"
            );
            assert_eq!(
                body["error"]["message_key"],
                json!("auth.password_change_required"),
                "is_admin={is_admin}"
            );
            assert_eq!(body["error"]["params"], json!({}), "is_admin={is_admin}");
            let error_keys: Vec<&str> = body["error"]
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                error_keys.len(),
                3,
                "error 必须恰为 code/message_key/params: {body}"
            );
            assert!(
                error_keys.contains(&"code")
                    && error_keys.contains(&"message_key")
                    && error_keys.contains(&"params"),
                "error keys: {body}"
            );
            let top_keys: Vec<&str> = body
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(
                top_keys.len(),
                2,
                "envelope 必须恰为 error+request_id: {body}"
            );
            assert!(
                top_keys.contains(&"error") && top_keys.contains(&"request_id"),
                "envelope keys: {body}"
            );
        }
    }

    #[test]
    fn normal_session_passes_for_plain_and_admin() {
        for is_admin in [false, true] {
            assert!(
                require_business_session(&session_fixture(is_admin, false)).is_ok(),
                "正常（非临时密码）会话必须通过, is_admin={is_admin}"
            );
        }
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

```bash
cargo test --offline --locked -p rsetup-controller --test task_http main_detail_summary_counts_are_decimal_strings_over_authorized_full_set --
cargo test --offline --locked -p rsetup-controller --test task_http main_summary_recomputes_from_visible_full_set_ignoring_full_state --
cargo test --offline --locked -p rsetup-controller --test task_http children_page_projection_is_independent_from_main_counts --
cargo test --offline --locked -p rsetup-controller --test task_http cancel_permission_matrix_is_exact --
cargo test --offline --locked -p rsetup-controller --test task_http release_lock_requires_admin_ack_and_nonempty_reason --
cargo test --offline --locked -p rsetup-controller --test task_http time_review_requires_at_least_one_held_subtask --
cargo test --offline --locked -p rsetup-controller --test task_http missing_session_never_reaches_business_repository --
cargo test --offline --locked -p rsetup-controller --test task_http cancel_result_projection_filters_unreadable_targets --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib http_tasks::business_session_tests --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib http_tasks::task_cursor_tests --
```
第九条命令（lib 单测，过滤 `http_tasks::business_session_tests`）与第十条命令（lib 单测，过滤 `http_tasks::task_cursor_tests`）在设计树根目录运行：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home` 与主工作区隔离、`--target-dir target` 隔离设计树自身 target、`--offline --locked` 与前八条一致；本节十条命令均为**未来命令，本次切片（文档编辑）未运行**。
预期：第十条命令恰好执行 4 个游标 lib 单测用例（模块 `http_tasks::task_cursor_tests`，完整定义见 Step 3 第 4 条代码块）——桩 `decode_task_cursor` 恒 `Ok` 下 `task_cursor_rejects_tampered_tag_with_invalid_argument`（tag hex 翻转 1 bit，期望 `InvalidArgument` 400）、`task_cursor_rejects_cross_endpoint_with_invalid_argument`（`Tasks` 签发游标以 `TaskChildren` 解码，期望 `InvalidArgument` 400）与 `task_cursor_rejects_malformed_length_or_hex`（63/65 字符、非 hex、大写 hex，期望 `InvalidArgument` 400）因 `expect_err` 断言失败 RED；`task_cursor_round_trip_success` 在占位桩（恒 `Ok` 返回常量元组，非真实 HMAC 编解码）下与测试内非零 `created_at_ms`/非 nil `tie_break_id` 的精确回环断言不符，同样断言失败 RED；四用例均非编译错误且非 0 用例；前八条命令各恰好执行 1 个用例并因**业务断言失败**而 RED（第一条：`project_main_task_summary` 桩无 10 键 `counts`（`view["counts"][...]` 为 null），十进制字符串/counts 形状断言失败；第二条：F06 负例——桩**原样回显 caller 传入的全量 `state`/`outcome`**（visible 全 Succeeded 仍输出 `running`/null，而非 `completed`/`success`）、无 `counts`、空 visible 仍 `Ok`（未报 `NotFound`），三组断言均失败；第三条：`project_task_children` 桩带旧单页 `view_scope`/`counts` 且 items 缺位；第四至六条：`cancel_permitted` 恒 true、`release_lock_permitted` 忽略 ack/reason、`time_review_permitted` 对 0 个 held 放行；第七条：若非 401/403 则失败；第八条：`project_cancel_result` 桩未过滤返回全部 id（不可读目标泄露、全不可读仍 `Some`、空目标 `Some([])`），逐目标可读负例三断言均失败）。**若因 router/state 缺失而编译失败则属于编译错误，禁止作为 RED** —— 因此 Step 1 必须先把 `TaskApi`、`HttpTasksState`、`build_tasks_router`、全部 DTO 与 `CancelledTarget`/`project_cancel_result`/`project_main_task_summary` 的**可编译桩**落地（`--test task_http` 选择器必须真实命中非零用例）；第九条命令恰好执行 2 个 lib 单测用例：恒 Ok 桩下 `temporary_password_forbids_admin_and_plain_with_exact_envelope` RED（循环首个 admin 样例先于 `expect_err` 403 断言失败而 RED，修绿后再覆盖普通样例）、`normal_session_passes_for_plain_and_admin` 已绿，均非编译错误且非 0 用例。

- [ ] **Step 3: 最小实现**

1. `HttpTasksState`/`FromRef`/`build_tasks_router`：只注册规格列出的 **8 条**路由；**不定义** `GET /task-previews`，**不注册**任何 SSE/`/system/time` 路由。未实现的路由**不注册**（不得返回恒定 200）：
```rust
pub fn build_tasks_router(state: HttpTasksState) -> Router<()> {
    Router::new()
        .route("/api/v1/task-previews", post(routes::create_preview))
        .route("/api/v1/tasks", post(routes::create_task).get(routes::list_tasks))
        .route("/api/v1/tasks/{id}", get(routes::task_detail))
        .route("/api/v1/tasks/{id}/children", get(routes::task_children))
        .route("/api/v1/tasks/{id}/cancel", post(routes::cancel_task))
        .route("/api/v1/tasks/{id}/time-review", post(routes::time_review))
        .route("/api/v1/subtasks/{id}/release-lock", post(routes::release_lock))
        .with_state(state)
}
```
   `src/http_device_status.rs` 另提供独立组件（不计入 8 条任务路由、不混入 Identity 26 条管理路由计数）：
```rust
// ── BLOCKED (awaiting transport wire DTO)：设备状态读整链条件门禁 ─────────────────────────
// 下方 DeviceStatusState / DeviceStatusSnapshotSource / DeviceStatusCacheEntry /
// DeviceStatusFreshness / build_device_status_router 依赖外部尚未获批的 wire DTO
// （`rsetup_protocol::wire::DeviceStatus`）；门禁通过（transport 交付并获批）之前，
// 整块作为**条件门禁**（BLOCKED）处理，不进入普通 Task 6 的编译与验收范围，
// 且不得在门禁前引入 `rsetup-protocol` 新依赖；Task 6 其余 8 条任务路由不受本门禁影响。
pub struct DeviceStatusState {
    pub auth: Arc<HttpAuthState>,
    pub snapshot: Arc<dyn DeviceStatusSnapshotSource>,
}

pub type DeviceStatusFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 状态源（待审目标接口，非现有代码）：读**当前进程真实缓存**的设备状态快照；
/// GET 不得触发执行/采集；缓存无样本返回 Ok(None)（响应 snapshot=null、
/// freshness="unknown"、age_ms=null），**不得**常量伪 PASS。
pub trait DeviceStatusSnapshotSource: Send + Sync {
    fn snapshot<'a>(&'a self, device_id: &'a [u8; 32]) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>>;
}

pub struct DeviceStatusCacheEntry {
    pub snapshot: rsetup_protocol::wire::DeviceStatus, // BLOCKED (awaiting transport wire DTO)：真实缓存，本字段是对未获批 wire DTO 的唯一直依赖
    pub received_time: TimeEvidence,
    pub freshness: DeviceStatusFreshness,
    pub last_error: Option<String>, // 固定去敏稳定码，无原始 driver 日志
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatusFreshness { Unknown, Fresh, Stale, Error }

// BLOCKED (awaiting transport wire DTO)：本路由属上方条件门禁，门禁通过前不落地、不注册该 router
pub fn build_device_status_router(state: DeviceStatusState) -> Router<()> {
    Router::new()
        .route("/api/v1/devices/{id}/status", get(get_device_status))
        .with_state(state)
}
```
   该路由的响应保持 02 §1：`snapshot`（03 `DeviceStatus` 语义投影或 null）、`received_time`（`TimeEvidence` 对象）、`freshness`（`unknown`/`fresh`/`stale`/`error`）、`age_ms`（无符号 u64 十进制字符串或 null，无样本 null；error 保留最后好样本时同时返回 `last_error` 且不把其 age 清零）。`get_device_status` handler 同样按第 2 条固定顺序执行：`authenticate_read`（`EndpointId::DeviceStatusRead`）→ 同一 `require_business_session` helper（临时密码 403 `PASSWORD_CHANGE_REQUIRED`，普通/管理员同）→ 读真实缓存快照；不新增业务 permission、不复制 Host/Cookie/CSRF。
2. 每个 handler 的固定顺序（**全部 8 条任务路由 + 额外 `GET /devices/{id}/status` 均适用**；复用 Identity 共享 gate，与 `src/http_auth.rs` 既有 handler 的边界行为完全一致）：①共享认证 `authenticate_read`（GET：`EndpointId::TaskList`/`TaskDetail`/`TaskChildren`，`/devices/{id}/status` 用 `EndpointId::DeviceStatusRead`）或 `authenticate_write`（全部任务 POST：既有 session 写桶，**不设**每端点写变体）——二者内部按序完成 单值 Host 精确匹配 → 写方法单值 Origin 检查（无 Origin 只豁免此项，不豁免会话/CSRF）→ 严格单值会话 Cookie（缺失 → 严格 401 `AUTH_REQUIRED`，**在触达仓储之前**，**不**以 401|403 放宽）→ `AuthServiceApi::is_live_session` 内存预过滤 → `authenticate` 权威会话 → 写方法绑定已认证 session.digest 的 CSRF 检查 → 限速（读 (EndpointId, session digest) 50/s、写 session digest 20/s）→ ②`require_business_session`（Runtime 本地 helper：`must_change_password` 临时密码一律 403 `PASSWORD_CHANGE_REQUIRED`，admin 同样、**不要求** `is_admin`）→ ③写 body/参数（`read_json_body`：写方法要求 `Content-Type: application/json` 且 body ≤ 1 MiB，DTO `#[serde(deny_unknown_fields)]` 拒绝未知字段 → 400；`parse_decimal_u64`/`parse_idempotency_key`/`clamp_page_limit`）→ ④需要的业务授权（`require_admin_session`：**仅** `time-review`/`release-lock` 独立要求管理员，不满足 403 `PERMISSION_DENIED`；其余路由**不全局** admin、保持既有逐路由授权）→ ⑤调用 `TaskApi` → ⑥统一错误映射。**`http_tasks` 内不自写 Host/Cookie/CSRF**、**不占用 `EndpointId::Me` 读桶**；`TaskList`/`TaskDetail`/`TaskChildren`/`DeviceStatusRead` 由 Identity 一次性加入 `EndpointId`（见《协作点》6）。

`require_business_session` GREEN 实现（替换 Step 1 恒 Ok 桩；imports 与 Step 1 桩导入集一致，已列于其上）：
```rust
/// Runtime 本地普通业务 gate（规格 01 A-02 + 02 §3）：临时密码（`must_change_password`，
/// 含 admin）一律 403 `PASSWORD_CHANGE_REQUIRED`；用既有 `err_response(status, code,
/// message_key, params)`（src/http_api.rs:35）构造，不依赖不存在共享导出；
/// 不要求 `is_admin`、不复制 Host/Cookie/CSRF。
pub(crate) fn require_business_session(session: &Session) -> Result<(), Response> {
    if session.user.must_change_password {
        return Err(err_response(
            StatusCode::FORBIDDEN,
            "PASSWORD_CHANGE_REQUIRED",
            "auth.password_change_required",
            json!({}),
        ));
    }
    Ok(())
}
```
3. 授权投影实现（两端点各自独立投影，禁止用 children 取代 main counts，全集无隐藏总数泄露）：
   - `project_main_task_summary`（`/tasks/{id}` 与 `GET /tasks` 列表项同形）：`visible_states` 是**唯一数据源**——对**当前授权全集**子任务状态（普通用户为“当前身份 + 当前 `device.task.read`”可见子集，admin 为全域且事务内重验）做十态频数聚合写入 `counts`（10 键齐全、逐键 u64 `to_string` 十进制字符串，**不直接 serde 内部 `TaskCounts`**）；`revision` 用 `to_string`；`created_time` 输出 `TimeEvidence` 对象（既有 `Serialize`，不改派生、不加 `Eq`）；**F06 修复**：`state`/`outcome` 一律由同一 `visible_states` 经 `calculate_main_task_outcome` 重算——full 与 authorized_subset **各自**按自己可见全集求值，caller 传入的全量 `state`/`outcome` **不得**成为第二真相源（否则可见 Succeeded 却见 Running/Partial/Unknown，泄隐藏 targets）；`visible_states` 为空 → `Err(ControllerError::NotFound)`（既有统一错误映射 → 404，与“不存在”同投影，**不输出零值汇总**）；非全权置 `view_scope = "authorized_subset"`；`actions.can_cancel` 由入参传入（合法操作权限快照独立输入、不当任务 read；handler 对可见目标逐条 `cancel_permitted` 预计算）；**绝不**输出 `total_targets`、`hidden_count` 或任何泄漏未授权目标存在与数量的字段。`calculate_main_task_outcome` 唯一生产者为本计划 Task 5 纯 helper（`crate::tasks::recovery`，4 态计算规则见 04，不受 wire DTO 门禁影响）：本函数只消费、**不复制第二套状态机**、不在本切片改其合法转移；若 Task 5 未就位，须先把该纯 helper 作为前置交付（可编译桩 → 其 Step 3 实现），不假称已实现。生产 `visible_states` 来源见本条第 5 项（同一授权读取快照）。最小实现（替换 Step 1 透传桩；`decision.rs` 需补 `use crate::ControllerError;` 与下述 recovery import，其余类型沿用该模块既有 import）：
   ```rust
   use crate::tasks::recovery::calculate_main_task_outcome;

   /// F06 修复：state/outcome/counts 一律由**同一** `visible_states`（当前可见全集）重算；
   /// `_state`/`_outcome` 仅保留签名位（Interfaces 9 参不变、TaskDetail 构造阶段同源产出），
   /// 投影不读取，避免第二真相源。
   pub fn project_main_task_summary(
       id: Uuid,
       operation: TaskOperation,
       _state: MainTaskState,
       _outcome: Option<MainTaskOutcome>,
       revision: u64,
       created_time: &TimeEvidence,
       visible_states: &[SubTaskState],
       can_cancel: bool,
       is_full_access: bool,
   ) -> Result<serde_json::Value, ControllerError> {
       // 可见全集为空（目标全部隐藏 / 非 owner 非 admin）→ 404 NOT_FOUND（与“不存在”
       // 同投影，经既有统一错误映射）；不输出零值汇总
       if visible_states.is_empty() {
           return Err(ControllerError::NotFound);
       }
       // state/outcome 由同一 visible 全集重算（唯一生产者：Task 5 纯 helper，
       // 4 态规则见 04；本切片只消费、不改其合法转移）
       let (calc_state, calc_outcome) = calculate_main_task_outcome(visible_states);
       // counts：visible 全集十态频数（内部 u64、逐键 to_string；不直接 serde TaskCounts）
       let count = |want: SubTaskState| -> u64 {
           visible_states.iter().filter(|s| **s == want).count() as u64
       };
       Ok(serde_json::json!({
           "id": id.to_string(),
           "operation": serde_json::to_value(operation).unwrap(),
           "state": serde_json::to_value(calc_state).unwrap(),
           "outcome": serde_json::to_value(calc_outcome).unwrap(),
           "revision": revision.to_string(),
           "created_time": serde_json::to_value(created_time).unwrap(),
           "view_scope": if is_full_access { "full" } else { "authorized_subset" },
           "counts": {
               "queued": count(SubTaskState::Queued).to_string(),
               "held": count(SubTaskState::Held).to_string(),
               "dispatching": count(SubTaskState::Dispatching).to_string(),
               "accepted": count(SubTaskState::Accepted).to_string(),
               "verifying": count(SubTaskState::Verifying).to_string(),
               "succeeded": count(SubTaskState::Succeeded).to_string(),
               "failed": count(SubTaskState::Failed).to_string(),
               "unknown": count(SubTaskState::Unknown).to_string(),
               "cancelled": count(SubTaskState::Cancelled).to_string(),
               "expired": count(SubTaskState::Expired).to_string()
           },
           "actions": { "can_cancel": can_cancel }
       }))
   }
   ```
   - `project_task_children`（`/tasks/{id}/children`）：输出 `Page{items,next_cursor}`；`entries` 中 `eligible == false` 的项**完全剔除**（不返回 `device_id`、不计入任何计数）；items 字段为 `id`/`device_id`/`state`/`reason_code`/`revision`（十进制字符串）/`result_evidence`（不暴露 `execution_token` 或其 token 摘要）；**不含** `view_scope`/`counts`（main counts 独立重算，与 Page 分离）；**绝不**输出 `total_targets` 或 `hidden_count`；`items[*].intent` 若存在，必须在响应中**同时**给出顶层 `state` 并把 `state` 作为唯一状态来源（`intent` 仅用于审计/调试展示，不得被当作状态）。
   - `project_cancel_result`（`/tasks/{id}/cancel`）：逐目标投影——仅 map `readable_in_tx == true` 的目标 `id`；空可读结果（含无 CAS 成功目标）→ `cancelled = None`，否则 `Some(filtered ids)`；`accepted` 恒 true（仅为取消请求接收确认，非全部成功保证）；不返回隐藏数、不新增协议字段。生产 `readable_in_tx` 由当前 actor（或有效 admin）及该 device 的 `device.task.read` 事务授权快照组装，**不得**取自客户端声明、旧 `Session.is_admin` 或 `has_any_taskread` 类整批布尔；该组装是后续 TaskService 定义必须履约的依赖，当前切片只钉投影契约、不假称已实现（不引入需要未定义 `AuthzRepo` 的真 SQL fixture）。`TaskApi::cancel` 对已持 reboot+queued/held 的合格目标事务内 CAS，经本函数返回 `CancelTaskResponse`；读权限不阻止合法取消行为，只过滤结果（`actions.can_cancel` 按操作权限/状态独立判定、不偷与 `device.task.read` 绑定，user 已批准）。最小实现（**TDD 次序**：Step 1 的未过滤可编译 RED 桩保留至本步骤之前不动，本步骤以该实现**替换**桩，不两实现同阶段重复）：
   ```rust
   pub fn project_cancel_result(targets: &[CancelledTarget]) -> CancelTaskResponse {
       // 逐目标过滤：仅 map 本事务可读（readable_in_tx == true）的目标 id
       let readable_ids: Vec<Uuid> = targets
           .iter()
           .filter(|t| t.readable_in_tx)
           .map(|t| t.id)
           .collect();
       CancelTaskResponse {
           // accepted 仅为取消请求接收确认（revision 匹配 + 事务通过），非全部成功保证
           accepted: true,
           // 空可读结果（含无 CAS 成功目标）→ None，非 Some([])
           cancelled: if readable_ids.is_empty() { None } else { Some(readable_ids) },
       }
   }
   ```
4. 游标：`encode_task_cursor`/`decode_task_cursor` 使用既有 `SessionAliasKey` 风格的 HMAC，首参端点上下文 `TaskCursorScope`（`Tasks`/`TaskChildren`）；**密钥注入**：`HttpTasksState` 保持 `{api, auth}` 两字段**不扩**（账本 L133 state 契约，不得擅自扩展），`CURSOR_SECRET`（32B）由 `api` 字段的生产实现 `TaskService<SqlxTaskRepository>` 持有——装配路径：唯一独立集成任务 owner 装配层经既有 `ControllerConfig::from_env` 环境载入链读入并注入构造（缺失或长度不符即装配失败，失败关闭），`list_tasks`/`task_children` 编解码调用点以显式调用参数传入 codec，HTTP handler 层不经手密钥，**不得**硬编码常量密钥；字节布局 = 24B payload（`created_at_ms` 大端 i64 8B ‖ `tie_break_id` 16B）+ 8B HMAC-SHA256 截断 tag = 32B → 64 小写 hex，tag 截断比较恒定时间，与账本决议④补充 UUID 键 profile 完全一致（出处：接口所有权账本《第 5 轮修复仲裁记录》决议④补充，2026-10-14）；解码校验序（先 MAC 后字段）：① 长度恰 64 且全小写 hex、hex decode 成功 → ② 以当前 `TaskCursorScope` 重算 HMAC 并恒定时间比较 8B tag（篡改或跨端点复用因 scope_byte/MAC 输入不同必在本步失败，即解码时校验 endpoint 匹配）→ ③ tag 通过后才解析 `created_at_ms`/`tie_break_id` 字段；任一步失败（篡改/跨端点/格式非法）一律 `InvalidArgument`（400），不区分篡改/过期/外来细节，不得回退到未分页全量；`/tasks` 传主任务 `id`、`/tasks/{id}/children` 传子任务 `id`；预期 user count 不随页面串扰。负例 selector（lib 单测模块 `http_tasks::task_cursor_tests`，命令格式与 Step 2/Step 4 一致）：`task_cursor_rejects_tampered_tag_with_invalid_argument`（tag hex 翻转 1 bit → `InvalidArgument` 400）、`task_cursor_rejects_cross_endpoint_with_invalid_argument`（`Tasks` 签发游标以 `TaskChildren` 解码 → `InvalidArgument` 400）。模块共 4 个用例：另含正向 `task_cursor_round_trip_success`（64 小写 hex + 同 secret 同 scope 精确解回 `(created_at_ms, tie_break_id)`）与 `task_cursor_rejects_malformed_length_or_hex`（63/65 字符或非 hex/大写 hex → `InvalidArgument` 400）。**测试模块定义（B3 补齐，消除空 selector 与缺失实现）**：`http_tasks::task_cursor_tests` 完整代码如下，Step 1 与桩一并落地为可编译模块，选择器完全就位且拥有 4 个非零断言用例；调用序遵循本计划钉死的签名（`endpoint` 首参、`secret` 末参，见接口所有权小节 `encode_task_cursor`/`decode_task_cursor` 声明），语义契约不变（24B payload + 8B scope 绑定 HMAC tag = 32B → 64 小写 hex；先校验 64 小写 hex，再恒定时间比较 tag，tag 通过后才解析字段；任一失败一律 `InvalidArgument` 400）。测试全部使用**合成 secret**（测试内常量 `[0x5A; 32]`），不读取环境变量、不触碰 `secret/` 或任何真实密钥：
    ```rust
    #[cfg(test)]
    mod task_cursor_tests {
        use super::*;

        // 合成 secret：测试内常量，不读取环境或真实密钥
        const SECRET: [u8; 32] = [0x5A; 32];
        const CREATED_AT_MS: i64 = 1_725_000_000_123;

        fn tie_break_id() -> Uuid {
            Uuid::parse_str("7c9e6679-7425-40de-944b-e07fc1f90ae7").unwrap()
        }

        #[test]
        fn task_cursor_round_trip_success() {
            let token =
                encode_task_cursor(TaskCursorScope::Tasks, CREATED_AT_MS, tie_break_id(), &SECRET);
            assert_eq!(token.len(), 64);
            assert!(token.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            let decoded = decode_task_cursor(TaskCursorScope::Tasks, &token, &SECRET)
                .expect("同 secret 同 scope 必须精确解回");
            assert_eq!(decoded, (CREATED_AT_MS, tie_break_id()));
        }

        #[test]
        fn task_cursor_rejects_tampered_tag_with_invalid_argument() {
            let token =
                encode_task_cursor(TaskCursorScope::Tasks, CREATED_AT_MS, tie_break_id(), &SECRET);
            // tag = 末 8B → 末 16 个 hex 字符；翻转最后一个 hex 字符一个 bit
            let mut tampered: Vec<char> = token.chars().collect();
            let last = tampered.len() - 1;
            tampered[last] = if tampered[last] == '0' { '1' } else { '0' };
            let tampered: String = tampered.into_iter().collect();
            let err = decode_task_cursor(TaskCursorScope::Tasks, &tampered, &SECRET)
                .expect_err("篡改 tag 必须失败");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }

        #[test]
        fn task_cursor_rejects_cross_endpoint_with_invalid_argument() {
            let token =
                encode_task_cursor(TaskCursorScope::Tasks, CREATED_AT_MS, tie_break_id(), &SECRET);
            let err = decode_task_cursor(TaskCursorScope::TaskChildren, &token, &SECRET)
                .expect_err("游标不得跨端点复用");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }

        #[test]
        fn task_cursor_rejects_malformed_length_or_hex() {
            let token =
                encode_task_cursor(TaskCursorScope::Tasks, CREATED_AT_MS, tie_break_id(), &SECRET);
            let mut too_short = token.clone();
            too_short.pop(); // 63 字符：长度非法
            let mut too_long = token.clone();
            too_long.push('0'); // 65 字符：长度非法
            let mut non_hex = token.clone();
            non_hex.replace_range(0..1, "z"); // 非 hex 字符
            let cases = [too_short, too_long, non_hex, token.to_uppercase()]; // 大写 hex 亦非法
            for case in cases {
                let err = decode_task_cursor(TaskCursorScope::Tasks, &case, &SECRET)
                    .expect_err("格式非法必须失败");
                assert!(matches!(err, ControllerError::InvalidArgument));
            }
        }
    }
    ```
5. `task_detail`：只返回 MainTask 授权投影（由 `project_main_task_summary` 生成，与列表项同形，不含子任务列表）；`task_children` 独立分页返回 `Page{items,next_cursor}`（由 `project_task_children` 生成）。**生产数据源**：投影入参 `visible_states`（可见全集 states）必须取自与 TaskDetail 字段**同一次授权读取快照**——`TaskService`（Task 4）内部读快照并在构造 TaskDetail 阶段完成投影（state/outcome/counts 已按同一 visible 全集同规则产出），`visible_states` 随该次读取产出；handler **不得**凭 `/children` 分页 states 构造、不得再查仓储、不得用分页 children 判断全集（`GET /tasks` 列表项同形：每项的 `visible_states` 同次读取快照产出）。投影返回 `Result<Value, ControllerError>`：`Ok(value)` → `ok_response(value)`，`Err` 走既有统一错误映射（`NotFound` → 404，**不假零汇总**）。**禁止**把 `/children` 的分页塞回 `/tasks/{id}`（规格是 8 条独立路由）。
6. `time_review`：解析 `TimeReviewTaskRequest`（`expected_revision` 十进制字符串、`decision` 严格两值、`evidence` 为 `TimeReviewEvidenceKind`，未知字段/未知枚举 400）；调用 `TaskApi::time_review`；对该主任务**全部 held** 子任务原子生效、绝不延长原期限（`time_review_allows_extend` 兜底拒绝任何“延长”参数，例如伪造的 `requested_extension_ms > 0` → 400）；`held_count == 0` → 400 `INVALID_ARGUMENT`（message_key `task.no_held`）；`expected_revision` 事务重验，不匹配 → 409 `REVISION_CONFLICT`；响应为**当前 MainTask 授权视图**（`TaskDetail` 同形，counts 按当前授权全集重算）。
7. `release_lock`：非管理员 → 403 `PERMISSION_DENIED`（以 `Session.user.is_admin` 与事务内重验为准）；`acknowledge_unknown != true` → 400 `INVALID_ARGUMENT`（message_key `task.ack_required`）；`reason.trim().is_empty()` → 400 `INVALID_ARGUMENT`（message_key `task.reason_required`）；`expected_revision` 不匹配 → 409 `REVISION_CONFLICT`；成功后响应**仅** `{released:true,state:"unknown"}` 两字段（无 `lock_released`/`revision` 影子 payload）。
8. 顶层生产装配（**唯一独立集成任务所有者**）：`build_tasks_router` 的 `TaskApi` **只**由真实 SQLx 仓储构造（`TaskService<SqlxTaskRepository>`）；`build_device_status_router` 的 `DeviceStatusSnapshotSource` 只接 typed 真实缓存源。**本任务不改 `main.rs`**：若任一真实源尚未接线，集成 owner **不注册**对应 router（`main.rs` 保持现状），在审查记录中标注“任务 API/设备状态读尚未生产挂载”，**绝不用常量空结果或常量快照占位**；未接线禁止宣称完整，不挂“未分配”。Identity 承诺的 `GET /api/v1/system/status` 由 Identity 专题实现；本计划**不**改 `http_system.rs`、**不**实现 `/system/time` 或 SSE，并在验收账本中把这些列为 Runtime Task 5/6 的未实施缺口。
9. `GET /api/v1/devices/{id}/status` 接线依赖与契约记录（已定责、待实施）：handler 读 `DeviceStatusSnapshotSource` 真实缓存快照（权限保持既有 `device.status.read`，**不新增**业务 permission）；读桶 `EndpointId::DeviceStatusRead` 由 Identity 独立提供（50/s，(EndpointId, session digest) 隔离，不并入既有模板）；wire `DeviceStatus` DTO 缺席（Task 5 Step 0 门禁）时整条设备状态读链（trait / 缓存条目 / handler / 路由）按条件门禁 `BLOCKED (awaiting transport wire DTO)` 处理，待 transport 交付并获批 DTO 后再整体落地，**不常量伪 PASS**，也不得宣称整验收完成。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
cargo test --offline --locked -p rsetup-controller --test task_http main_detail_summary_counts_are_decimal_strings_over_authorized_full_set --
cargo test --offline --locked -p rsetup-controller --test task_http main_summary_recomputes_from_visible_full_set_ignoring_full_state --
cargo test --offline --locked -p rsetup-controller --test task_http children_page_projection_is_independent_from_main_counts --
cargo test --offline --locked -p rsetup-controller --test task_http cancel_permission_matrix_is_exact --
cargo test --offline --locked -p rsetup-controller --test task_http release_lock_requires_admin_ack_and_nonempty_reason --
cargo test --offline --locked -p rsetup-controller --test task_http time_review_requires_at_least_one_held_subtask --
cargo test --offline --locked -p rsetup-controller --test task_http missing_session_never_reaches_business_repository --
cargo test --offline --locked -p rsetup-controller --test task_http cancel_result_projection_filters_unreadable_targets --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib http_tasks::business_session_tests --
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home cargo test --offline --locked --target-dir target -p rsetup-controller --lib http_tasks::task_cursor_tests --
```
预期：前八条各执行 1 个用例，全部 PASS（8 用例 PASS，0 失败）；第九条命令执行 2 个 lib 单测用例，全部 PASS（2 用例 PASS，0 失败）；第十条命令执行 4 个游标 lib 单测用例（1 正向回环 + 3 负例），全部 PASS（4 用例 PASS，0 失败）。

- [ ] **Step 5: 静态检查与提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/http_tasks/mod.rs crates/rsetup-controller/src/http_tasks/decision.rs \
  crates/rsetup-controller/src/http_tasks/routes.rs crates/rsetup-controller/src/http_device_status.rs \
  crates/rsetup-controller/src/lib.rs crates/rsetup-controller/tests/task_http.rs
git commit -m "feat(controller): 最小任务 HTTP 端点、设备状态读路由与授权子集投影"
```
  （提交信息按仓库规范用中文描述，type/scope 保留英文；不提交 `main.rs`。）

---

## 自我审查

**覆盖检查（设计文档逐条 → 任务）**

| 设计/规格要求 | 落地任务 |
| :--- | :--- |
| 完整 NTP 客户端校验真实 SocketAddr 源、64位 originate/nonce 全量回显、服务端收发时戳合法、单调 RTT 上限与 RFC 四时戳 checked i128 偏移 | Task 1 Step 1/3（`correlate_sntp_response`） |
| ≤30s 启动门禁 + `SystemFallback` + 后台有界退避 | Task 1（`gate_should_close`、`next_backoff`、`run_startup_sample`、`start_time_gate`） |
| 既有纯 parser 从独立分支导入，在导入处扩充 `NtpError` 枚举变体确保可编译，绝不产生 `NtpValid` | Task 1 Step 0 + Global Constraint 4 |
| 10 状态模型、12 原因码、合法转移；holds_device_lock 对 Failed 明确判定 false | Task 2（`holds_device_lock`、`advance_subtask`） |
| schema v4 五表、无 FK/无 CHECK；独立 `migration_v4` 验收 16 表与完整性 | Task 3 Step 3.1 + Step 5 |
| 版本化只读 guard：期望版本来自编译期配置 = 4，统一重构旧 3 硬编码，登录/改密/准入/任务共用；v3 仅用于迁移 preflight | Task 3 Step 1/3.2–3.5、Global Constraint 9 |
| 真实备份/隔离/独占/停写门禁后才迁移；跳过 root 锁但不跳过应用账号真实事务验证 | Task 3 Step 5（`BLOCKED` 契约） |
| 预览 1..=1024、规范小写 UUIDv4 幂等键 | Task 4 Step 3 |
| 同事务先查持久幂等记录（直接重放，即使 preview 已过期），首次提交才原子单次消费 preview token；增加过期后重放真库测试 | Task 4 Step 4.4–4.5 + Step 6 |
| 设备锁 CAS、`DEVICE_BUSY` 不排队、事务内零网络 | Task 4 Step 4.7 + Step 6 |
| prepare 票据保留 `boot_id`/`valid_for_ms`、最后重验 | Task 5 Step 1/3.5 |
| 意图提交后才发送、至多一次、超时转 verifying 不重发 | Task 5 Step 3.5 |
| wire DTO 缺席时 `scheduler.rs` 仅实现纯函数/纯结构，泛型 `TaskScheduler`/`run_once` 标记 BLOCKED 等待 BoardClient 定义 | Task 5 Step 0 + Produces |
| 恢复只读不重发、`unknown` 保锁释放槽 | Task 5 Step 3.6 + Step 5 |
| 成功证据 04 §6 全条件；`expected_pre_boot` 作为参数从 DB 读取核验 | Task 5 Step 3.4 |
| 规格 8 条最小任务 HTTP 路由（`POST /task-previews`、`GET|POST /tasks`、`GET /tasks/{id}`、`GET /tasks/{id}/children`、`cancel`、`time-review`、`release-lock`；**不含** `GET /task-previews`） | Task 6 Step 3.1 + Global Constraint 6 |
| `GET /api/v1/devices/{id}/status` 正式 Runtime 路由（`device.status.read`、独立 `DeviceStatusRead` 读桶、读真实缓存快照、不常量伪 PASS、不计入 8 任务/26 管理路由） | Task 6 Step 3.1/3.9 |
| Task 6 依赖 Task 4，明确定义全部 HTTP DTO 字段（TaskDetail/TaskPage/ChildrenPage/TaskCounts/TaskActions/ChildItem 与请求/响应 DTO；旧 `TimeReviewOutcome`/`ReleaseOutcome`/`main_task_id`/`subtask_id` 影子名删除） | Task 6 Interfaces |
| 主任务详情摘要与 children Page 投影各自拥有独立授权投影纯函数，严禁用 children 单页取代 main 计数，绝不泄露隐藏总数，children 无 `view_scope`/`counts` | Task 6 Step 1/3.3/3.5 + Step 4 |
| 取消只对 queued/held、dispatching 后不伪称取消、结果按逐目标 `device.task.read` 过滤（不可读目标不返回、全不可读/无成功为 null） | Task 4 Step 3、Task 6 Step 1/3 |
| time-review 原子作用于全部 held、无 held 400、绝不延长 | Task 4 Step 3/4、Task 6 Step 1/3.6 |
| unknown 释放需管理员 + revision + ack + 非空 reason，保留 unknown | Task 4 Step 4、Task 6 Step 1/3.7 |
| 生产网关不得返回常量空结果、DB 错误 503 | Task 6 Step 3.8 + 错误映射 |
| 不把无关 SSE/轮询扩展/`/system/time` 冒充为本目标 | Global Constraint 6、Task 6 Step 3.8（`http_events.rs`/`http_system.rs` 不在文件表内） |
| 双引擎隔离真实库验证、0 用例不算通过、不伪造证据 | Task 3 Step 5、Task 4 Step 6、Task 5 Step 5 |
| 跨进程故障测试（新进程、真 DB 断言） | Task 5 Step 5 |
| 旧 SQLx v3 identity 协作点明确 | Global Constraint 9、Task 3 Step 3.5、下文《协作点与冲突》 |
| 业务会话 gate（Runtime 本地 `require_business_session`）：`must_change_password` 临时密码（含 admin）一律 403 `PASSWORD_CHANGE_REQUIRED`（`auth.password_change_required`），普通/管理员正常会话通过；缺失会话仍严格 401 `AUTH_REQUIRED`（未采 401|403 放宽） | Task 6 Step 1（恒 Ok 可编译桩 + 局部行为测试）/ Step 2（第九条命令）/ Step 3（GREEN + 固定 handler 顺序 ①→⑥） |
| Task 1 gate_udp 忙循环检测进程级外部 watchdog 机制 | 规范已补齐（Task 1 Step 2/Step 4 明确包装器要求：执行阶段使用 `timeout --kill-after=5s 35s cargo test ...` 强制截断孤儿进程且非 0 退出码记 FAIL；实际执行命令待执行阶段按此包装） |
| 9 路由（8 任务路由 + `GET /devices/{id}/status`）完整 fixture/spy 矩阵**仍未完成**：Step 2 的 8 条命令 + 1 组 lib 单测不覆盖 9 路由全 fixture/spy，本次不写假 PASS；已**具名**为独立验收任务 **Task 7: Runtime 九路由 fixture/spy 矩阵验收（RT-ROUTE-MATRIX-ACCEPT）**——二选一中选择"具名独立验收任务"而非"本计划内一节写明全矩阵"（矩阵格定义以集成计划为准，本计划不重复列全清单）；owner = Runtime 集成验收 owner（唯一负责，独立于本计划 Task 1–6 owner，不写业务代码，只补验收用例与记录）；依赖 = 本计划 Task 1–6 全部完成（含各条件门禁）后进入，任一未完成保持 `BLOCKED`；验收 = 按 `2026-10-16-controller-v1-integration-acceptance-tdd.md` 行 142（Identity 26 管理 + Runtime 9 业务真实 gateway spy 矩阵）与行 632（T4-02：9 条 Runtime 业务路由进程内权限矩阵 + 真实同源正反例、写端点缺有效会话不触达业务仓储）将 9 路由 × fixture/spy 矩阵逐格执行，任一格未执行记 `BLOCKED`，不得写假 PASS | Task 7（具名独立验收任务，本计划 Task 6 不覆盖） |

**占位扫描：** 全文无 `TBD`/`TODO`/“以后实现”/“类似任务 N”。每个 RED 断言都给出可运行命令与预期失败原因；每个 GREEN 都给出最小实现代码或明确的实现条目。

**类型一致性核对：** `SubTaskState`（10 态 snake_case）在 Task 2、4、5、6 中同名同形；`ReasonCode`（大写下划线）在 Task 2、3、4、5 中同名；`EvidenceKind`（六值 snake_case）唯一定义在 Task 2 Step 1（wire 严格 `attempted`/`boot_transition_observed`/`execution_rejected`/`os_error`/`journal_lost`/`none` 六串，`Option<EvidenceKind>::None` 为 JSON `null`、与串 `"none"` 不同），Web 计划 Task 12 行 12 的 `result_evidence` 跨线待补项以该定义为 producer（本计划 Task 2 Step 1），Web 侧不重定义；`TimeReviewEvidenceKind` 在 Task 4、6 中同名（Task 6 `TimeReviewTaskRequest.evidence` 直接复用）；请求体 JSON 的 `expected_revision` 在 Task 4 与 Task 6 均为十进制字符串（Rust 服务类型为 u64，HTTP 层用 `parse_decimal_u64` 严格解析）；`calculate_main_task_outcome` 在 Task 5 定义、Runtime 计划 Task 4 中同名（**不得**改名为 `main_task_summary`）；主任务投影字段名为 `id`/`operation`（旧 `main_task_id` 影子名删除）
、`created_time` 为既有 `TimeEvidence`（仅 `PartialEq`，含它的新 DTO 不派生 `Eq`，不改旧派生）、`revision`/`counts` 为内部 u64 且 JSON 十进制字符串（`crate::model::decimal_u64_json` 或显式 `to_string`，10 键齐全、零值 `"0"`）；`project_main_task_summary` 返回 `Result<serde_json::Value, ControllerError>`（空 visible 全集 → `NotFound` → 404，不假零汇总），其 `state`/`outcome`/counts 一律由同一 `visible_states` 重算（full/authorized_subset 各自按自己可见全集求值，caller 传入的全量 `state`/`outcome` 仅作同源一致性核对、非第二真相源——F06；`calculate_main_task_outcome` 为 Task 5 纯 helper 唯一生产者，本切片只消费）；主任务详情采用 `project_main_task_summary`、子任务分页采用 `project_task_children`，二者各自独立投影，禁止用单页 children 替代 main counts；`TaskApi::task_detail` 返回 `TaskDetail`（不含分页子列表），分页只属于 `TaskApi::task_children`（输出 `Page{items,next_cursor}`）；`TaskEvidence` 不设未定义字段，`expected_pre_boot` 作为参数从 DB 传入；
Task 6 代码段引用的类型（`TaskOperation`/`TimeReviewDecision`/`TaskActions`/`ChildEntry`/`ReleaseLockResponse`/`CancelTaskResponse`/`CancelledTarget`/`project_cancel_result`/`DeviceStatusFreshness`/`DeviceStatusSnapshotSource` 等）均在本 Task 6 切片定义或已存在于 Task 2/4 与既有源码，无 placeholder；`run_startup_sample`（Task 1）在 Interfaces/Step 1 桩/Step 3 代码三处同名同形（`(bind: SntpBindFuture, resolver: &dyn SntpResolver, servers: &[String], request_timeout: Duration, budget: Duration, max_rtt_ms: i64, now: tokio::time::Instant) -> Option<(NtpSample, String)>`），`SntpBindFuture` 仅在 socket.rs 定义一次，测试模块只引用本模块内完整定义的 fixture/helper 与文件内既有的 `make_server_response` 构造器（无 `ClockSource` 类未定义占位）；
本次单次请求 3s 上限切片中 `NTP_SINGLE_REQUEST_TIMEOUT_CAP = 3s` 与 `select_request_timeout(configured: Duration, remaining: Duration) -> Duration` 在 Interfaces/Step 1 桩/Step 3 实现三处同名同形，测试模块只引用本切片新定义的 `SplitResolver` 与既有 `fake_ntp_server`/`bind_spy`/`make_server_response`（无新增占位），新增 2 个测试方法（顶层纯表驱动 1 个 + `startup_budget` 嵌套模块事件驱动 1 个），RED/GREEN 选择器 13 条与 13 个测试方法一一对应。

**切片边界声明（本次文档编辑）：** 仅新增 Task 6 业务 gate helper `require_business_session`（恒 Ok RED 桩 → 2 个局部行为测试 → GREEN）及其固定 handler 顺序 / status 契约引用；完整计划仍 `CHANGES_REQUIRED`，本切片**不消除**其他安全/接线缺口（missing-session 全套、9 路由 fixture/spy 矩阵、真实源接线等保持原状）。

**切片边界声明（本 C2 切片）：** 仅修 Task 2 计划纯转移 `transition_allowed`——移除非法边 `Dispatching → Cancelled`（04 §3 不含）及“证明未发送可取消”例外，新增 `dispatching_cancel_only_permitted_from_queued_or_held` 与 `full_ten_by_ten_transition_table_matches_spec` 两个 lib 单测及其 RED/GREEN 命令；**计划纯转移已修**（GREEN 阶段移除该边），但取消与 dispatching 的实际 CAS 竞争、cancel HTTP 路由与事务内 `USER_CANCELLED` 证据写入未在本纯函数切片验证，完整 Runtime 仍 `CHANGES_REQUIRED`。`2026-10-16-controller-v1-four-task4-interface-ownership.md` §7 第 8 条“已知 Runtime 错误”仍列 `Dispatching→Cancelled`（该文件不在本切片唯一写范围，未改动，亦不宣称其清单已清）。

**切片边界声明（本启动总预算切片）：** 仅补 Task 1 启动采样预算函数 `run_startup_sample`——Interfaces 增 `SntpBindFuture` 与 `run_startup_sample` 精确签名、Step 1 增恒 `None` 可编译桩（socket.rs 增 `SntpBindFuture` 类型）、`tests/gate_udp.rs` 新增 `mod startup_budget` 的 4 条受控测试（成功提前返回且 query spy>0、pending DNS 被总预算取消、多源总预算不扩大、空源有界等待不忙循环）及其完整 fixture/helper、Step 3 第 3 条以完整 `run_startup_sample` 实现代码替换原单段描述、第 6 条 `start_time_gate` 明确为 `run_startup_sample` 启动侧唯一消费者（startup gate 结束只表示 NTP 门结束，不替代 DB/bootstrap/恢复 ready）；同步把 Step 2/4 的 RED/GREEN 命令与计数更新为 11 条（并补齐原 RED 清单遗漏的 `sntp_response_with_zero_receive_timestamp_is_rejected`）。本切片**只补启动采样预算**：后台 `TimeGateHandle` 持久状态/后台 driver（源列表重读、stale 复验、墙钟跳变失效、审计事件）仍是其他后续任务，**不宣称整个 Task 1 或验收账本条目 P-RT-05 完成**；不混入 v4 迁移/HTTP/BoardClient；未修改任何实际业务 `src/tests/manifest`，本切片测试未运行（未运行不得记 PASS）。

**切片边界声明（本次单次请求 3s 上限切片）：** 只聚焦 Task 1 `run_startup_sample` 的单次请求预算选择与成功 UDP fixture——①明确采用规格 05 §2 建议“单次请求上限3s”作为实现策略（规格原文是**建议**、非强制契约，`DEFAULT_NTP_REQUEST_TIMEOUT` 仍为配置默认；新增 `NTP_SINGLE_REQUEST_TIMEOUT_CAP = 3s` 与纯 helper `select_request_timeout(configured, remaining) = min(configured, 3s 上限, remaining)`，Interfaces/Step 1 桩/Step 3 实现三处同名同形），`run_startup_sample` 的 `per = request_timeout.min(remaining)` 改为 `per = select_request_timeout(request_timeout, remaining)`；socket bind/DNS/send/recv 与多源仍共用唯一 ≤30s 绝对 deadline（`timeout_at` 只创建一次），不每阶段/每源重建 deadline；②`tests/gate_udp.rs` 新增 2 个测试方法：顶层纯表驱动 `single_request_timeout_selection_caps_at_3s_and_clamps_to_remaining`（7 个表用例：configured>3s 不突破上限、configured==上限、configured<3s 不放大、剩余<3s 用剩余、剩余<configured、剩余为 0、configured 为 0；证明选择规则）
与 `startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger`（事件驱动本地 pending/多源真实 UDP 路径：源 0 静默不响应、源 1 应答，configured=10s、总预算 5s，断言源 1 在预算内成功、`elapsed ≥ 3s` 且 `< 3s+slack`；证明**实际**请求超时而非以纯 helper 冒充真实超时证据）——纯 helper 证明选择规则、事件驱动用例证明真实超时，二者并存；③成功 fixture `fake_ntp_server` 从不合理的 `t3 = t2 + 1s`（同时要求 <300ms 成功）改为 `t3 = t2` 本地短驻留标准响应（同步 doc 与内联注释），RTT 上限仍由本地 T1/T4 单调跨度度量（已确立 SNTP 模型不变），规格 05 板端时间校验（board_clock）`rtt=(T4-T1)-(T3-T2)` 负 RTT 定义不混入 SNTP 模型；④Step 2/4 的 RED/GREEN 命令与计数由 11 条更新为 **13 条**（新增 2 条：纯表驱动 1 条 + 事件驱动 1 条，后者为模块内唯一 3s 量级用例、真实时间运行约 3s，其余预算用例仍 300ms 短预算；嵌套模块选择器用全名 `startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger`，若追加 `--exact` 必须用该全名）。本切片新增 2 个测试方法（未运行，未实际执行前不得记 PASS）
，不扩整个 NTP driver、nonce uniqueness、watchdog、typed 授权、迁移/HTTP/BoardClient，**不宣称整个 Task 1 或验收账本条目 P-RT-05 完成**；未修改任何实际业务 `src/tests/manifest`，人类成果（中文 commit 示例、安全门、条件门禁步骤）不变。本切片及后续切片**已修复**的排版缺口：Task 1 两个重复的 “Step 5: 静态检查与提交” 块（既有排版缺陷已合并为单一规范步骤，提交信息统一为中文 Conventional Commits）；“05 §1 建议 3s 上限”条款号标注（当前规格文件该句位于 §2“NTP生命周期”标题下，本修复切片已将 ① 的标注统一为 05 §2；3s 上限切片原“沿用既有 05 §1 标注避免越界”为历史记载不重写）；忙循环检测依赖另有进程级外部 watchdog（强制终止 + 清理前置，已在 Step 2/Step 4 明确定义进程级包装器规范，执行命令待执行阶段按此包装，不冒称测试已运行）；`DEFAULT_NTP_MAX_RTT_MS` 的 “05 §2” 标注同样存在条款号存疑（未在本切片改动）。

**切片边界声明（本 IntentKind/TaskIntent 切片）：** 只补 Task 2 既定 2 types——`IntentKind { Dispatch }`（serde snake_case、`Copy` 枚举）与 `TaskIntent`（全部 8 字段与 Interfaces 既定一致；`ticket_digest`/`request_digest` 为 32B 内部私有持久化证据，仅派生 `Clone, PartialEq, Eq, Serialize, Deserialize`、**不派生 Debug**，`Eq` 为字段类型依据的真 Eq），插入 Step 1、复用既有 `serde`/`Uuid` imports、单一定义、不加 `kind` 字段、不改名其他 model；恰新增 1 个 pure lib 测试 `dispatch_intent_model_roundtrips_without_expanding_http_contract`（合成 fixture 8 字段全给、`to_value`/`from_value` 内部往返、`assert!(decoded == expected)` 不要求 `Debug`、核 8 key 与两个摘要数组 32 元素长度）；同步 Task 2 测试方法 8→9 与 Step 4 第 9 条 `--lib tasks::model::tests::<name> -- --exact` 精确命令及计数（9 用例/四条 `-- --exact`/六条 `--lib` fullpath），4 条行为 RED 命令未改、Task 1 未改。`TaskEvidence`/`TaskRecordRef` 留最后一次任务，未扩 driver/HTTP/迁移；Task 5 的 “追加 `TaskIntent`/`TaskEvidence` 的 serde” 条目中 `TaskIntent` 部分已被 Step 1 覆盖（`TaskEvidence`/04 §6 字段仍归 Task 5）。本切片未改任何实际源 `src/tests/manifest`；新增 types/test **未编译、未运行**（未实际执行前不得记 GREEN/PASS），四线计划仍未写齐/未 `APPROVED`。

**切片边界声明（本 TaskEvidence/TaskRecordRef 切片）：** 只补 Task 2 既定最后 2 models——`TaskEvidence`（全部 8 字段与 Interfaces 既定一致、内部持久化/核验输入，不通过 HTTP 投影完整对象（Task 6 `ChildItem` 仅 `result_evidence: Option<EvidenceKind>` 种类）、不证明 boot 转换/授权可信/at-most-once，不加 `expected_pre_boot`/`TimeEvidence`/HTTP 字段）与 `TaskRecordRef`（`boot_id`/`valid_for_ms` 2 字段，prepare 票据摘要引用、**不代表**获批 wire codec），均派生 `Clone, Debug, PartialEq, Eq, Serialize, Deserialize`（字段类型皆 Eq、无浮点，非伪 Eq），插入 Step 1、复用既有 `serde`/`Uuid` imports、单一定义；恰新增 1 个 pure lib 测试 `task_evidence_and_record_ref_internal_models_roundtrip`（合成 8/2 字段 fixture、`to_value`/`from_value` 往返、key 集合长度 8/2 与预期类型逐一相等、无 token/真实凭据；缺必含字段/非法 `EvidenceKind`/负或小数 `record_version` 按类型语义拒绝、`valid_for_ms` 为 u64 且 `u64::MAX` 边界同形支持）。同步 Task 2 测试方法 9→10 与 Step 4 新增 `--lib tasks::model::tests::task_evidence_and_record_ref_internal_models_roundtrip -- --exact` 精确命令及计数（10 用例/五条 `-- --exact`/七条 `--lib` fullpath）、Step 1/2 模型阶段文案同步为 6 serde + 4 behavior；4 条行为 RED 命令未改、Task 1/watchdog/HTTP/v4 未扩。Task 5 “追加 `TaskIntent`/`TaskEvidence` 的 serde” 条目更新为 Task 2 单 producer 已定义、Task 5 只核对消费/真实行为、不重复 derive 或定义。本切片未改任何实际源 `src/tests/manifest`；新增 2 models/1 test **未编译、未运行**（未实际执行前不得记 GREEN/PASS），四线计划仍未写齐/未 `APPROVED`。

---

## 协作点与跨专题接口冲突（必须由父任务裁决）

1. **`src/integrity.rs` 的版本化 guard 是与 Identity Task 2/3 的强耦合点。** Identity 计划的 SQLx 仓储按 `schema_version = 3` 编写；一旦 Runtime 升级到 v4，身份路径必须改用 `validate_guard_rows_for_version`。**安全约束**：期望版本必须由编译期配置（`SUPPORTED_SCHEMA_VERSION = 4`）决定，普通启动对旧版（v1..=3）、未知版本（v5+）与脏数据一律拒绝（fail-closed）；`v3` 仅用于显式迁移 preflight。**建议裁决**：本计划 Task 3 先落地版本化函数与重构，Identity 统一消费。
2. **`BoardClient` 依赖未交付的 wire DTO（当前阻塞项）。** `crates/rsetup-protocol/src/wire.rs`（179 行）**没有** `Capabilities`/`DeviceStatus`/`ClockSample`/`RebootTicket`/`RebootExecute`/`TaskRecord`，只有 `PacketType`/`TunnelPacket`/`WireError`/`validate_packet`。**裁决需求**：由获批 transport 工作交付与账本 §3 既有定义形状一致的 DTO（`TaskRecord` 以 `state`/`pre_boot_id`/`observed_boot_id?`/`reason_code?`/`updated_unix_ms?`/`evidence_kind` 等逐字段核验；按「第 5 轮修复仲裁记录」决议①**不新增** `accepted`/`attempted`/OS 结果等 wire 字段，attempted 可靠性与 OS 结果经 state/evidence_kind/reason_code 语义判定，见 Task 5 Step 0 两段式门禁）。在此之前 Task 5 的 `board_client.rs`/`evidence.rs` 与其真实库测试保持 `BLOCKED`；`scheduler.rs` 仅提供纯函数/纯结构切片，泛型 `TaskScheduler`/`run_once` 保持未定义并标 `BLOCKED`。本计划**不**创建影子 DTO，也**不**自定义编码器。
3. **投影函数与授权子集视图契约。** 最新规格把主任务详情和分页子任务固定为独立路由：`GET /api/v1/tasks/{id}` 与 `GET /api/v1/tasks/{id}/children`。主任务详情（与列表项同形）由 `project_main_task_summary` 重新计算**当前授权全集**摘要：普通用户为“当前身份 + 当前 `device.task.read`”可见子集，admin 为全域且事务内重验；counts 十键齐全、十进制字符串；分页子任务列表由 `project_task_children` 投影当前页内可见条目，输出 `Page{items,next_cursor}`，**无** `view_scope`/`counts`（与 main counts 分离）。**裁决需求**：两处各司其职，**不得**用分页 children 替代 main counts，**不得**把 children 合并回 detail，两处均**严禁**输出 `total_targets` 或 `hidden_count`。F06 补充契约：主摘要 `state`/`outcome`/`counts` 一律按**当前可见全集**（`visible_states`）重算——full 与 authorized_subset 各自按自己可见全集求值，caller 传入的全量 `state`/`outcome` 不得成为第二真相源；可见全集为空 → 404 `NOT_FOUND`（不假零汇总）；`visible_states` 生产来源为与 TaskDetail 同一次授权读取快照（`TaskService` 构造阶段完成投影，handler 不得凭分页构造）。
4. **`src/http_tasks.rs` vs `src/http_tasks/` 目录布局。** Runtime 计划写的是单文件 `src/http_tasks.rs`；本计划拆为 `src/http_tasks/{mod,decision,routes}.rs`。**风险**：若两个专题各自创建，会产生同名路径冲突（Rust 中 `http_tasks.rs` 与 `http_tasks/` 不能并存）。**裁决需求**：合并前必须确认另一专题未创建 `src/http_tasks.rs`；本计划保留目录形式。
5. **本计划明确不交付的范围（防止越界冒充）。** 规格第 7/30 行把 Runtime Task 6 收敛为上述 8 条任务 HTTP 路由，并指定 Identity Task 4 承诺的 `GET /api/v1/system/status` 由 Identity 实现。因此**不创建** `src/http_events.rs`（SSE）、**不实现** `/api/v1/system/time`、**不做**轮询/快照增强（`src/polling/snapshot.rs` 不在本计划文件表内）。**裁决需求**：Runtime Task 5/6 的其余部分（SSE、`/system/time`、错峰轮询与快照）如需交付，另立计划；验收账本中必须把它们列为**未实施缺口**，不得以本计划的 8 条路由宣称 Runtime Task 6 完整。
6. **`EndpointId` 枚举归属冲突。** 任务端点限速需要新增 `EndpointId` 变体，但该枚举定义在 `src/http_auth.rs`（Identity 所有，现仅 `Me`/`Sessions`）。**裁决需求**：由 Identity 一次性加入任务读 3 变体（`TaskList`/`TaskDetail`/`TaskChildren`，按 GET 模板隔离）与独立 `DeviceStatusRead` 读桶（`Me`/`Sessions` 原样保留）；**所有**任务 POST 走既有 session 写桶（按 session digest，20/s），不设每端点写变体（原草案 8 变体提案作废）；本计划**不**占用 `EndpointId::Me` 读桶、**不自写** Host/Cookie/CSRF（复用 Identity `pub(crate)` 共享 gate）；**禁止**两个专题各自定义同名枚举。
7. **`reason_code` 语义必须与 transport 板端报告一致。** 04 §3 表与 03 §5 共享同一枚举值；本计划 Task 2 是唯一 Rust 侧权威定义。**裁决需求**：transport 板端类型化白名单若引入新代码，必须同步扩到 `ReasonCode`，不得在板端 crate 另建枚举后靠字符串拼接。
8. **`device.task.read` 简写冲突。** Web/Runtime 部分文字用 `task.read` 简写。**裁决**：统一映射到 `Permission::DeviceTaskRead`（`"device.task.read"`），**严禁**新增独立简写权限；本计划 Task 6 只消费 `authz::Permission`。
9. **`Cargo.toml` 直接依赖声明。** Task 1/5/6 的测试需要 `serde`（直接依赖已存在）与 `tokio` 的 `test-util` 特性（当前 workspace `tokio` 特性列表**不含** `test-util`）。**裁决需求**：由父任务统一决定是在 workspace 层加特性还是仅 controller crate 的 `[dev-dependencies]` 加 `tokio = { workspace = true, features = ["test-util"] }`；本计划使用后者，且**不使用** `start_paused`。
10. **Web 侧 `authorized_subset` 的消费契约。** 本计划输出 MainTask 授权投影（列表项与详情同形：`id`/`operation`/`state`/`outcome`/`revision`/`created_time`/`view_scope`/`counts`/`actions.can_cancel`）与 `/children` 端点 `Page{items,next_cursor}` 的确切形状；Web 计划 Task 4 必须消费该形状并保留“隐藏目标不泄露计数”。**裁决需求**：若 Web 已实现另一形状（例如 `total_targets`/`main_task_id`），以本计划为准并在 Web 侧调整；`actions` 中仅 `can_cancel` 经本次用户批准，缺字段时 Web 必须关闭 cancel 入口，其余 actions 字段（如 `can_time_review`）仍属待审扩展建议。
11. **NTP 非机密源与真实网络。** 本计划全部 NTP 测试使用进程内受控 fake 应答；**未**验证任意外部 NTP 或真实网络路径。生产 `SntpResolver` 的 DNS 解析属于真实网络边界，需在集成验收阶段单独记录，**不得**把 fake 应答结果推广为对真实 NTP 服务的可信性声明。

---

**执行交接：**

计划已保存到 `docs/superpowers/plans/2026-10-16-controller-v1-four-task4-runtime-execution.md`。有两种执行方式：
1. 子代理驱动（推荐）：每项派新的子代理，任务间审查，快速迭代。
2. 当前会话执行：使用 executing-plans，分批落实并设置检查点。

本计划自身不执行任何步骤。Task 3 Step 5、Task 4 Step 6、Task 5 Step 5 为**严格条件门禁步骤**，未满足授权隔离目标、真实备份与独占停写前置时保持未运行并记录 `BLOCKED`；Task 5 另受 wire DTO 门禁约束。

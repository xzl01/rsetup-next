### Task 5: 内存双流绑定、先 Control 后 Data 与审批门（网络与 codec 双 fail-closed）

**Files:**
- Create: `crates/rsetup-board-agent/src/tunnel_binding.rs`
- Modify: `crates/rsetup-board-agent/src/lib.rs`
- Test: `crates/rsetup-board-agent/tests/interop_dual_stream.rs`

**Interfaces:**
```rust
// ---- tunnel_binding.rs ----
/// 唯一允许建立真实加密通道/真实网络连接的凭证。
/// 在外部专家书面决议与用户复审落地前，`codec_approved` / `network_approved` 必须保持 false。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalGate {
    pub codec_approved: bool,
    pub network_approved: bool,
    pub evidence_path: &'static str,
}

impl Default for ApprovalGate {
    fn default() -> Self {
        Self {
            codec_approved: false,
            network_approved: false,
            evidence_path: "docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md",
        }
    }
}

impl ApprovalGate {
    /// 测试中显式构造；生产路径只有默认未批准值。
    pub fn new(codec_approved: bool, network_approved: bool, evidence_path: &'static str) -> Self;
    pub fn require_codec(&self) -> Result<(), StreamError>;
    pub fn require_network_io(&self) -> Result<(), StreamError>;
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    #[error("codec approval gate is closed")]
    CodecApprovalRequired,
    #[error("network io approval gate is closed")]
    NetworkApprovalRequired,
    #[error("data stream cannot open before the control stream")]
    ControlStreamRequired,
    #[error("action is not allowed on this stream")]
    ActionNotAllowedOnStream,
    #[error("response trace id mismatch")]
    ResponseMismatch,
    #[error("serialization failed: {0}")]
    Serialization(String),
    #[error("agent rejected the request: {0}")]
    AgentRejected(BusinessStatus),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamType { Control, Data }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState { NotOpened, Open, Closed }

/// 内存双流绑定：用真实 `BoardAgent` + 真实 `TaskJournal` + mpsc 通道装配，
/// 不含任何真实网络 socket、不含任何加密实现。
pub struct DualStreamBinding<R, J, E, O, F>
where
    R: ReadOnlySource,
    J: TaskJournalReader,
    E: JournalEpochSource,
    O: RebootOs,
    F: Fs,
{
    agent: BoardAgent<R, J, E>,
    executor: RebootExecutor<O, F>,
    control: StreamState,
    data: StreamState,
    // 获批网络切片前保留该接收端以维持 channel 生命周期；当前不声称已有生产消费方。
    // 未来实现 new 初始化该字段时应使用此字段名。
    _outbound: tokio::sync::mpsc::Receiver<rsetup_protocol::wire::BusinessPayload>,
    gate: ApprovalGate,
}

impl<R, J, E, O, F> DualStreamBinding<R, J, E, O, F>
where R: ReadOnlySource, J: TaskJournalReader, E: JournalEpochSource, O: RebootOs, F: Fs
{
    pub fn new(
        agent: BoardAgent<R, J, E>,
        executor: RebootExecutor<O, F>,
        gate: ApprovalGate,
    ) -> (Self, tokio::sync::mpsc::Sender<rsetup_protocol::wire::BusinessPayload>);

    /// 必须先于任何 Data 操作与业务请求调用（03 §1 / 现行规范 §6.6 顺序契约）。
    pub fn open_control(&mut self) -> Result<(), StreamError>;
    pub fn close_control(&mut self);
    /// 未先开 Control 时返回 `ControlStreamRequired`，绝不静默放行。
    pub fn open_data(&mut self) -> Result<(), StreamError>;
    pub fn stream_state(&self, stream: StreamType) -> StreamState;
    pub fn gate(&self) -> &ApprovalGate;
    pub fn os(&self) -> &O;

    /// 统一守卫：Control 未开流（NotOpened/Closed）恒返回 `ControlStreamRequired`，不触及 source/journal。
    pub fn handle_typed(
        &self,
        stream: StreamType,
        peer: &PeerContext,
        action: &str,
        payload: &rsetup_protocol::wire::BusinessPayload,
    ) -> Result<BusinessResponse, StreamError>;

    /// 统一守卫：与 `handle_typed` 共享判定函数，fail-closed 拒绝未开流或非 Control 业务访问。
    pub fn handle_wire(
        &self,
        stream: StreamType,
        peer: &PeerContext,
        action: &str,
        bytes: &[u8],
    ) -> Result<BusinessResponse, StreamError>;

    /// `TYPE_SYSTEM`（`tunnel.kick` / `tunnel.revoke`）只允许在 Control 流。
    pub fn handle_system(&self, stream: StreamType, action: &str) -> Result<(), StreamError>;

    /// 真实网络入口：在审批门关闭时恒返回 `NetworkApprovalRequired`，绝不建立连接。
    pub async fn bind_real_network(&self, addr: &str) -> Result<(), StreamError>;

    /// 板端主动事件（`device.task.result`）：必须先在 Control 流打开后发送。
    pub fn emit_task_result(
        &self,
        record: &crate::journal::TaskRecord,
    ) -> Result<rsetup_protocol::wire::BusinessPayload, StreamError>;
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `src/lib.rs` 追加 `pub mod tunnel_binding;` 与导出。桩实现：`open_data` 无条件 `Ok(())`（错误地不检查 Control 是否已开）；`bind_real_network` 无条件 `Ok(())`（错误地越过审批门）。

`tests/interop_dual_stream.rs`：
```rust
mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::dispatch::{BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, TaskJournalReader};
use rsetup_board_agent::journal::{JournalStore, RealFs, TaskRecord};
use rsetup_board_agent::read_only::{PeerContext, ReadOnlyMutationGuard, ReadOnlySource};
use rsetup_board_agent::reboot::{RebootExecutor, RecordingOs};
use rsetup_board_agent::ticket::TicketManager;
use rsetup_board_agent::tunnel_binding::{
    ApprovalGate, DualStreamBinding, StreamError, StreamState, StreamType,
};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, ClockSample, DeviceStatus, NoCodec,
    Query, TaskQuery,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";

#[derive(Default)]
struct Source { mutations: AtomicUsize }

impl ReadOnlySource for Source {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_version: "0.5.0".to_string(),
            supported_actions: vec!["device.reboot.execute".to_string()],
            durable_task_journal: true,
            max_status_bytes: 65_536,
            journal_epoch: EPOCH.to_string(),
        }
    }
    fn status(&self) -> DeviceStatus {
        DeviceStatus {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_epoch: AGENT_EPOCH.to_string(),
            sample_seq: 1,
            sampled_unix_ms: 1,
            sampled_monotonic_ns: 1,
            clock_epoch: CLOCK_EPOCH.to_string(),
            metrics: vec![],
            clock_quality: "observed".to_string(),
        }
    }
    fn clock(&self) -> ClockSample {
        ClockSample {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            clock_epoch: CLOCK_EPOCH.to_string(),
            receive_unix_ms: 1,
            send_unix_ms: 2,
            receive_monotonic_ns: 1,
            send_monotonic_ns: 2,
            quality: "observed".to_string(),
        }
    }
}

impl ReadOnlyMutationGuard for Source {
    fn mutation_calls(&self) -> usize { self.mutations.load(Ordering::SeqCst) }
}

struct JournalReader(Arc<JournalStore<RealFs>>);

impl TaskJournalReader for JournalReader {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<rsetup_protocol::wire::TaskRecord>, BusinessStatus> {
        self.0
            .get(controller_key, &query.sub_task_id)
            .map(|found| found.map(|record| record.to_wire()))
            .map_err(|_| BusinessStatus::InternalError)
    }
}

struct FixedEpoch(Arc<JournalStore<RealFs>>);

impl JournalEpochSource for FixedEpoch {
    fn journal_epoch(&self) -> String { self.0.journal_epoch().to_string() }
}

struct Fixture {
    _dir: TempDir,
    journal: Arc<JournalStore<RealFs>>,
    binding: DualStreamBinding<Source, JournalReader, FixedEpoch, RecordingOs, RealFs>,
}

fn fixture(gate: ApprovalGate) -> Fixture {
    let dir = TempDir::new("dual");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("journal"));
    let agent = BoardAgent::new(
        Source::default(),
        JournalReader(Arc::clone(&journal)),
        FixedEpoch(Arc::clone(&journal)),
    );
    let os = RecordingOs::new();
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT.to_string()),
        "agent-1",
    );
    let (binding, _outbound) = DualStreamBinding::new(agent, executor, gate);
    Fixture { _dir: dir, journal, binding }
}

#[test]
fn data_stream_is_blocked_until_control_stream_is_open() {
    let mut f = fixture(ApprovalGate::default());
    assert_eq!(f.binding.stream_state(StreamType::Control), StreamState::NotOpened);
    assert_eq!(f.binding.stream_state(StreamType::Data), StreamState::NotOpened);
    assert_eq!(f.binding.open_data(), Err(StreamError::ControlStreamRequired));
    assert_eq!(f.binding.stream_state(StreamType::Data), StreamState::NotOpened);

    f.binding.open_control().expect("open control");
    assert_eq!(f.binding.stream_state(StreamType::Control), StreamState::Open);
    f.binding.open_data().expect("open data after control");
    assert_eq!(f.binding.stream_state(StreamType::Data), StreamState::Open);
}

#[test]
fn business_requests_are_rejected_when_control_is_not_open_or_closed() {
    let mut f = fixture(ApprovalGate::default());
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };
    let query = BusinessPayload::Query(Query { schema_version: BUSINESS_SCHEMA_VERSION });

    // 1) NotOpened: typed 与 wire 入口均被拒，不触碰 source/journal
    assert_eq!(
        f.binding.handle_typed(StreamType::Control, &peer, "device.capabilities.get", &query),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(
        f.binding.handle_wire(StreamType::Control, &peer, "device.capabilities.get", &[]),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(f.binding.os().call_count(), 0);

    // 2) Open 后正常访问
    f.binding.open_control().expect("open control");
    assert!(f.binding.handle_typed(StreamType::Control, &peer, "device.capabilities.get", &query).is_ok());

    // 3) Closed 后再访问同样被拒
    f.binding.close_control();
    assert_eq!(f.binding.stream_state(StreamType::Control), StreamState::Closed);
    assert_eq!(
        f.binding.handle_typed(StreamType::Control, &peer, "device.capabilities.get", &query),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(
        f.binding.handle_wire(StreamType::Control, &peer, "device.capabilities.get", &[]),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(f.binding.os().call_count(), 0);
}

#[test]
fn typed_readonly_round_trip_uses_real_journal_and_zero_os_calls() {
    let mut f = fixture(ApprovalGate::default());
    f.binding.open_control().expect("control");
    f.binding.open_data().expect("data");
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };

    let caps = f
        .binding
        .handle_typed(
            StreamType::Control,
            &peer,
            "device.capabilities.get",
            &BusinessPayload::Query(Query { schema_version: BUSINESS_SCHEMA_VERSION }),
        )
        .expect("capabilities");
    let StreamState::Open = f.binding.stream_state(StreamType::Control) else { panic!() };
    match caps {
        BusinessResponse::Capabilities(caps) => assert_eq!(caps.journal_epoch, EPOCH),
        other => panic!("expected capabilities, got {other:?}"),
    }

    // 真实 journal：先落一条 accepted，再经查询路径读回
    let record: TaskRecord = f
        .journal
        .insert_accepted(&[1u8; 32], SUB_TASK, &[0u8; 32], BOOT)
        .expect("accepted")
        .0;
    let queried = f
        .binding
        .handle_typed(
            StreamType::Control,
            &peer,
            "device.task.get",
            &BusinessPayload::TaskQuery(TaskQuery {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
            }),
        )
        .expect("task get");
    match queried {
        BusinessResponse::TaskRecord(returned) => {
            assert_eq!(returned, record.to_wire());
            assert_eq!(returned.journal_epoch, f.journal.journal_epoch());
        }
        other => panic!("expected task record, got {other:?}"),
    }

    // 另一个认证身份不得看到该记录
    let stranger = PeerContext { authenticated_controller_key: [2u8; 32] };
    assert_eq!(
        f.binding.handle_typed(
            StreamType::Control,
            &stranger,
            "device.task.get",
            &BusinessPayload::TaskQuery(TaskQuery {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
            }),
        ),
        Err(StreamError::AgentRejected(BusinessStatus::TaskNotFound))
    );
    assert_eq!(f.binding.gate().require_network_io(), Err(StreamError::NetworkApprovalRequired));
    assert_eq!(f.binding.gate().require_codec(), Err(StreamError::CodecApprovalRequired));
    assert_eq!(f.binding.os().call_count(), 0, "readonly round-trip must never touch the OS");
}

#[test]
fn not_allowlisted_action_is_rejected_before_any_stream_work() {
    let mut f = fixture(ApprovalGate::default());
    f.binding.open_control().expect("control");
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };
    assert_eq!(
        f.binding.handle_typed(
            StreamType::Control,
            &peer,
            "device.fan.apply",
            &BusinessPayload::Query(Query { schema_version: BUSINESS_SCHEMA_VERSION }),
        ),
        Err(StreamError::AgentRejected(BusinessStatus::UnsupportedAction))
    );
    // 白名单外动作不消耗 codec：即使没有获批 codec，也报告动作不被允许
    assert_eq!(
        f.binding.handle_wire(StreamType::Control, &peer, "device.fan.apply", &[]),
        Err(StreamError::AgentRejected(BusinessStatus::UnsupportedAction))
    );
}

#[test]
fn raw_wire_entries_fail_closed_without_an_approved_codec() {
    let mut f = fixture(ApprovalGate::default());
    f.binding.open_control().expect("control");
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };

    for action in ["device.status.get", "device.clock.get", "device.task.get"] {
        assert_eq!(
            f.binding.handle_wire(StreamType::Control, &peer, action, b"raw-bytes"),
            Err(StreamError::AgentRejected(BusinessStatus::UnsupportedVersion)),
            "action={action} must fail closed without an approved codec"
        );
    }
    assert_eq!(f.binding.gate().require_codec(), Err(StreamError::CodecApprovalRequired));
    // 未获批 codec：解密/编码路径根本不可达，绝不产生「假 codec 通过」
    assert_eq!(
        rsetup_protocol::wire::decode_business(
            &NoCodec,
            None,
            rsetup_protocol::wire::BusinessPayloadKind::TaskRecord,
            b"raw-bytes",
        ),
        Err(rsetup_protocol::wire::CodecError::CodecUnapproved("no approved business codec"))
    );
}

#[test]
fn system_frames_are_confined_to_the_control_stream() {
    let mut f = fixture(ApprovalGate::default());
    assert_eq!(
        f.binding.handle_system(StreamType::Data, "tunnel.kick"),
        Err(StreamError::ControlStreamRequired),
        "TYPE_SYSTEM frames must never travel on the data stream"
    );
    f.binding.open_control().expect("control");
    assert_eq!(f.binding.handle_system(StreamType::Control, "tunnel.kick"), Ok(()));
    assert_eq!(
        f.binding.handle_system(StreamType::Control, "tunnel.ping"),
        Err(StreamError::ActionNotAllowedOnStream),
        "tunnel.ping is a probe action, not a system action"
    );
}

#[test]
fn real_network_binding_is_refused_while_the_approval_gate_is_closed() {
    let f = fixture(ApprovalGate::default());
    assert_eq!(
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(f.binding.bind_real_network("127.0.0.1:0")),
        Err(StreamError::NetworkApprovalRequired)
    );
    assert_eq!(
        f.binding.gate().require_network_io(),
        Err(StreamError::NetworkApprovalRequired)
    );
}

#[test]
fn task_result_event_is_emitted_only_after_control_is_open() {
    let mut f = fixture(ApprovalGate::default());
    let record = f
        .journal
        .insert_accepted(&[1u8; 32], SUB_TASK, &[0u8; 32], BOOT)
        .expect("accepted")
        .0;
    assert_eq!(
        f.binding.emit_task_result(&record),
        Err(StreamError::ControlStreamRequired)
    );
    f.binding.open_control().expect("control");
    let payload = f.binding.emit_task_result(&record).expect("event");
    assert!(matches!(payload, BusinessPayload::TaskRecord(_)));
    // 板端事件不得回包
    assert!(matches!(
        f.binding.handle_system(StreamType::Control, "device.task.result"),
        Err(StreamError::ActionNotAllowedOnStream)
    ));
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
cargo test --offline --locked -p rsetup-board-agent --test interop_dual_stream data_stream_is_blocked_until_control_stream_is_open -- --exact
```
预期：编译通过、执行 1 个用例，因 `open_data` 桩无条件 `Ok(())` 而断言失败。
```bash
cargo test --offline --locked -p rsetup-board-agent --test interop_dual_stream business_requests_are_rejected_when_control_is_not_open_or_closed -- --exact
```
预期：编译通过、执行 1 个用例，因未开流守卫缺失而断言失败。
```bash
cargo test --offline --locked -p rsetup-board-agent --test interop_dual_stream real_network_binding_is_refused_while_the_approval_gate_is_closed -- --exact
```
预期：断言失败（桩 `bind_real_network` 返回 `Ok(())`）。**不得出现编译错误或 0 用例。**

- [ ] **Step 3: 最小实现**

1. `open_data`：`if self.control != StreamState::Open { return Err(StreamError::ControlStreamRequired); }`，随后置 `Data = Open`。
2. `handle_typed` / `handle_wire`：共用私有统一判定函数 `ensure_business_stream(stream)`——若 `self.control != StreamState::Open`（含 NotOpened 与 Closed），拒绝一切业务请求并返回 `Err(StreamError::ControlStreamRequired)`，覆盖 source/journal/业务 action 全部访问且不触碰下层；若 `stream == StreamType::Data` 且 action 属于业务目录，返回 `Err(StreamError::ActionNotAllowedOnStream)`（03 §1：首版业务请求/响应与事件均在 OpenControl；Data 流只参与传输层应用探测）；守卫通过后分别委托 `agent.handle_typed_action(...)` 与 `agent.handle_wire(&NoCodec, ...)` 并把 `BusinessStatus` 包装为 `StreamError::AgentRejected`。**绑定内不得出现任何 codec 实现**；`handle_wire` 必须恒用 `NoCodec` 语义（先查白名单，再 fail-closed）。

> **第 1、2 条边界校验说明代码**（展开实现，与上文契约严格一致、整体 fail-closed；两入口共享 `ensure_business_stream` 判定函数避免分叉，绑定只消费 agent 既有契约，绑定内不得出现任何外部 codec 实现）：

```rust
// (1) open_data —— fail-closed：Control 未打开时绝不放行 Data 流
fn open_data(&mut self) -> Result<(), StreamError> {
    if self.control != StreamState::Open {
        // 绝不静默放行：不允许 Data-first 的乱序开流
        return Err(StreamError::ControlStreamRequired);
    }
    self.data = StreamState::Open;
    Ok(())
}

// 私有统一流状态判定函数：typed 与 wire 双入口共用，避免守卫分叉
fn ensure_business_stream(&self, stream: StreamType, action: &str) -> Result<(), StreamError> {
    // 守卫 1: Control 必须处于 Open 状态（NotOpened 或 Closed 均拒绝），拒绝覆盖全部业务访问
    if self.control != StreamState::Open {
        return Err(StreamError::ControlStreamRequired);
    }
    // 守卫 2: 规格 03 §1 业务请求/响应均在 Control（OpenControl）流；
    // Data 流只参与传输层应用探测。业务目录内 action 若落在 Data 流直接拒绝。
    if stream == StreamType::Data && ACTION_ALLOWLIST.iter().any(|a| a.name == action) {
        return Err(StreamError::ActionNotAllowedOnStream);
    }
    Ok(())
}

// (2) handle_typed / handle_wire —— 先经统一流守卫判定，再委托 agent
fn handle_typed(
    &self,
    stream: StreamType,
    peer: &PeerContext,
    action: &str,
    payload: &rsetup_protocol::wire::BusinessPayload,
) -> Result<BusinessResponse, StreamError> {
    self.ensure_business_stream(stream, action)?;
    self.agent.handle_typed_action(peer, action, payload)
        .map_err(StreamError::AgentRejected)
}

fn handle_wire(
    &self,
    stream: StreamType,
    peer: &PeerContext,
    action: &str,
    bytes: &[u8],
) -> Result<BusinessResponse, StreamError> {
    self.ensure_business_stream(stream, action)?;
    self.agent.handle_wire(&NoCodec, peer, action, bytes)
        .map_err(StreamError::AgentRejected)
}
```

3. `handle_system`：`tunnel.kick` / `tunnel.revoke` 仅 `StreamType::Control` 且 Control 已开；其他 action 返回 `ActionNotAllowedOnStream`。
4. `emit_task_result`：Control 未开 → `ControlStreamRequired`；否则返回 `BusinessPayload::TaskRecord(record.to_wire())` 并保持 `kind = Event`（由调用方的 `TunnelPacket` 决定，本绑定不构造字节）。
5. `bind_real_network`：
```rust
pub async fn bind_real_network(&self, _addr: &str) -> Result<(), StreamError> {
    self.gate.require_network_io()?;
    // 即便门被打开，本计划也不实现真实监听/握手；交由获批的传输计划实现。
    Err(StreamError::NetworkApprovalRequired)
}
```
   > 这里故意在门打开后仍返回 `NetworkApprovalRequired` 并附注释说明「真实监听器属独立获批切片」，避免任何本计划以外的网络行为。
6. `ApprovalGate::require_codec`：`codec_approved` 为 false 时 `Err(CodecApprovalRequired)`；`require_network_io`：`network_approved` 为 false 时 `Err(NetworkApprovalRequired)`。

- [ ] **Step 4: 运行并确认通过（PASS）**

```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam
```
预期：PASS（`read_only`、`wire_04_journal`、`journal_fault_injection`、`wire_03_tickets`、`reboot_boundary`、`interop_dual_stream` 全绿）。

- [ ] **Step 5: 提交**

```bash
cargo fmt -p rsetup-board-agent
cargo clippy --offline --locked -p rsetup-board-agent --features test-seam -- -D warnings
git add crates/rsetup-board-agent/src/lib.rs crates/rsetup-board-agent/src/tunnel_binding.rs crates/rsetup-board-agent/tests/interop_dual_stream.rs
git commit -m "feat(board-agent): 内存双流绑定，含控制优先顺序与审批门"
```

---

## Runtime唯一BoardClient契约消费（仅传播、不在Transport实现）

> 本节**传播**Runtime 唯一 owner 的 Task 4 `BoardClient` 契约（Runtime 计划 `docs/superpowers/plans/2026-10-16-controller-v1-four-task4-runtime-execution.md`；六方法 dyn-safe 签名与 `BoardClientError` 逐字取自 ownership 清单 `docs/superpowers/plans/2026-10-16-controller-v1-four-task4-interface-ownership.md` §3 待审统一目标契约，该接口文本刚获指定模型限定静态 APPROVED——仅该六方法切片、非整个计划），本计划只消费/引用、不实现、不改写、不扩展；旧 Transport 自定的八方法 `async fn`/error 影子契约（`ControllerError` 包装、泛 `String` 载荷、`task_query`/`prepare_request`）在此清除。

```rust
// 只读参考（Runtime 写域、本计划不写）：crates/rsetup-controller/src/tasks/board_client.rs
use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use rsetup_protocol::wire::{
    Capabilities, ClockSample, DeviceStatus, RebootExecute, RebootTicket, TaskRecord,
};

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
```

```rust
// 独立 typed 错误枚举：不包装 `ControllerError`（取代旧草案 `Controller(#[from] ControllerError)` 变体）；
// unit 变体 + 唯一结构变体 `Rejected { status, reason_code }`；`reason_code` 严格按 03/04 稳定词表校验，
// 不存 driver 原串、不自造 `DiagCode` 类型；不照抄旧草案 `#[derive(Clone, PartialEq, Eq)]` 派生
//（实际 `ControllerError` 仅派生 Debug/thiserror，照抄 Eq 派生会编译失败），不新增诊断参数。
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

**契约消费边界（本计划仅传播、不在 Transport 实现）：**

- 六方法为待审统一目标契约（dyn-safe 形式，不用 `async fn` trait 形式）：`prepare(device, sub_task_id: Uuid, expected_boot_id)` → `RebootTicket`；`execute(device, ticket: &RebootTicket, request: &RebootExecute)` → 完整 `TaskRecord`，**至多一次**、须在 `dispatching` 意图提交后调用，实现层核对 `ticket.sub_task_id == request.sub_task_id`；`task_record(device, sub_task_id: Uuid)` → `Option<TaskRecord>`，`TASK_NOT_FOUND` (1006) 映射 `Ok(None)`。
- `task_query(&TaskQuery)` 与 `prepare_request(sub_task_id, expected_boot_id)` 为**明确未批准的扩展（待审）**：不属六方法，本计划不将其加入上列 trait，不以 Transport 提案单方压过 Runtime 接口。
- 证据保留：`prepare` 票据保留 `boot_id` 与 `valid_for_ms`（写入发送前 boot 与 TTL 摘要）；`execute` 返回**完整** `TaskRecord`（含 `journal_epoch`/`record_version`/`pre_boot_id`/`observed_boot_id`/`evidence_kind`），绝不降级为 `Result<()>`/`Result<String>`；wire `RebootTicket` 由本计划 Task 1 完整定义（`schema_version`/`sub_task_id`/`boot_id`/`execution_token: [u8; 32]`/`valid_for_ms: u32`）。
- 所有权边界：Transport 仅生产共享 wire DTO/typed 适配源；`tasks/board_client.rs` 只 Runtime 写（唯一 owner、唯一写者），Transport 不得编辑 controller owner 文件；`lib.rs`/`main.rs` 顶层生产组装归唯一独立集成任务所有者，各线仅提供组件子 router（Runtime `build_tasks_router`），不得自设顶层合并入口；共享文件改动按 **Identity gate → ADMIN audit → Runtime v4** 串行。
- `None` 不证明未执行：按当前真实接口，`task_record` 的 `None` 仅证明板端 journal 无该记录，不证明 execute 未发出，亦不授权 Controller 自动重发；板端重复健壮性（dedup）不是 Controller 自动重发许可。Runtime 侧以「活票据（一次性消费）+ `execute` 前提交 `dispatching` 意图 + 至多一次 + 崩溃恢复后仅 `task.get` 查询核实」保证，本节不实现 adapter/async 网络。
- DTO ≠ codec：本计划 Task 1 的 DTO 与 `NoCodec` fail-closed 不证明 codec 已批准、加密通道或精确 512 KiB 整包上限；独立专家书面决议 + 用户复审 + codec 批准前，raw 字节入口持续 fail-closed，不宣称 WIRE-01 512 KiB 通过。
- 错误映射与阶段语义：`DeviceOffline`→`DEVICE_OFFLINE`、`CapabilityUnavailable`→`CAPABILITY_UNAVAILABLE`、`DeviceBusy`→`DEVICE_BUSY`；`ResourceExhausted`（status=1008）仅为协议层业务 `status_code`（板端拒发执行），不是任务生命周期 reason_code，不在本计划 `TASK_REASON_CODES` 十二个获批 reason 词表之内：构造 `Rejected { status, reason_code }` 时其 `reason_code` 为 `None`（或对应词表内合法 reason），不新增词表外原因；`Rejected { status, reason_code }` 准确承载 03 §1 稳定 `status_code` 词表（含 04 §3 `EXECUTION_REJECTED`），不再丢失成 `ControllerError::InvalidArgument`；`Timeout`/`Transport` 按阶段处理：prepare 失败（尚未提交 dispatching 意图、尚未调用 execute）时，子任务可转 `failed`（对应具体 reason_code）并释放自己持有的锁；intent 提交后 execute `Timeout`/`Transport` 不能 `failed` 或重发，进入 `verifying`；`verifying` query `Timeout`/`Transport` 保持直到观察窗口到期转 `unknown`；`unknown` query 超时保持 `unknown`，不重占执行槽、不开 execute。
- **实现不得出现在本计划交付物中**：获批 codec 与加密通道就位前，中控只可用测试 fake `BoardClient`（Runtime 线责任），fake 必须可断言「`execute` 调用次数」以证明崩溃恢复不重发。

**板端侧与中控侧的边界（本计划已交付/未交付）：**

| 内容 | 本计划 | 未交付（需独立获批切片） |
|---|---|---|
| 业务 DTO 与结构校验 | Task 1 | 真实 Protobuf codec（外部专家决议 + 用户复审后） |
| 板端 crate 与封闭白名单 | Task 2 | 板端可执行入口 `main.rs`（transport Task 9） |
| 幂等 journal、稳定 epoch、两阶段 fsync | Task 3 | 真实电源故障恢复证据 |
| 票据与无参 OS 边界 | Task 4 | 真实主机重启调用（HostRebootOs 恒 Unsupported，不得宣称 full Task4 端到端完成） |
| 内存双流绑定与审批门 | Task 5 | 真实加密握手、真实网络监听/双进程 smoke；`rsetup-controller` 侧 `BoardClient` 真实实现（Runtime 写，Transport 不写） |

---

## 覆盖矩阵与验收账本

| 验收项 | 对应任务 | 规格来源 | 证据层级 | 状态 |
|---|---|---|---|---|
| 业务 DTO schema=1、UUIDv4、字段长度、稳定词表、证据字段完整、Metric 使用 Option<f64> 且校验 is_finite 与 bytes <= 2^53-1、不派生 Eq | Task 1 | 03 §2 字段约束 | ① 可编译行为 RED→GREEN | 可独立实施 |
| 无获批 codec 时原始字节入口 fail-closed，采用 sealed Codec 与不可伪造 ApprovedCodecWitness，绝不出现假 codec 或公共方法绕过 | Task 1 / Task 2 / Task 5 | 03 §1、spec §4 | ① 可编译行为 RED→GREEN | 可独立实施 |
| 板端入站白名单封闭（仅 6 项入站请求动作；device.task.result 仅为板端→中控事件不入白名单与 supported_actions）、未知/变更动作零副作用、`(controller_key, sub_task_id)` 命名空间隔离 | Task 2 | 03 §1/§3、04 §1 | ① | 可独立实施 |
| `journal_epoch` 持久稳定、`pre_boot_id` 在 accepted 落盘、同 ID 去重与载荷冲突、serde/serde_json 序列化、删除越层 TicketConsumedNotExecuted | Task 3.1/3.3 | 03 §5、04 §2 | ① | 可独立实施 |
| 写入顺序「临时文件→文件 sync_all→rename→父目录 sync_all」与 rename 前后真行为失败分类（rename 前确定失败为 DefinitelyNotCommitted，rename 本身及之后为 CommitUnknown），TempDir Drop 校验独立临时根防误删 | Task 3.2 | 03 §5、spec §4 | ①（文件持久化故障注入真行为 RED→GREEN） | 可独立实施 |
| 日志容量 4096 满时拒绝新任务且不淘汰去重记录 | Task 3.3 | 03 §5 | ① | 可独立实施（容量为待审默认） |
| 跨 boot 证据只产出 `boot_transition_observed`；同 boot 不判成功；不自动重发 | Task 3.3 | 03 §5、04 §6/§7 | ① | 可独立实施 |
| 票据 30s 单调 TTL、连接/agent/sub_task_id/boot 绑定（consume 先匹配后消耗、失败不消耗）、一次性消费 | Task 4 | 03 §4、04 §5.1 | ① | 可独立实施（TTL 为待审默认） |
| 无参 OS 边界、accepted/attempted 可靠落盘前不调用 OS（fake 计数 0）、accepted 已落盘而 mark_attempted 确未提交时答 status=0 的 accepted 记录（计数 0）、insert_accepted/mark_attempted 的 CommitUnknown 为 Indeterminate 断流无业务回包（计数 0）、attempted 落盘且 OS 明确报错时仅 finish failed/OS_ERROR 可靠落盘才 status=0 回复真实 failed（FakeOS 计数 1、宿主物理 reboot 0）、finish DefinitelyNotCommitted 或 CommitUnknown 均 Indeterminate 断流无业务回包（计数 1）不返回旧 attempted、不宽泛声称「所有 OS 明确错误均无回包」（可靠 failed 仍可 status 0）、OS 返回 Ok 且 attempted 可靠只回复 attempted 不叫成功（真实重启证据需认证持久日志/boot transition）；HostRebootOs 恒 Unsupported 仅为纯组件切片，不等于完整真实任务互通验收，不得宣称 full Task4 端到端完成 | Task 4 | 03 §1/§5（B-04）、04 §6 | ① ＋ 宿主 reboot 恒零见证（纯组件切片，非全流程验收） | 可独立实施 |
| 先 Control 后 Data、`TYPE_SYSTEM` 仅 Control、板端事件不回包 | Task 5 | 03 §1、现行规范 §6.6 | ① 纯内存集成 | 可独立实施 |
| 真实 Protobuf codec / 512KiB 整包 | — | 03 §2、spec §4 | ④ 经外部专家书面决议与用户复审 | **BLOCKED** |
| 真实加密握手 / 真实网络双进程 smoke | Task 5 的门之外 | spec §4/§6 | ④ | **BLOCKED** |
| 真实电源故障恢复（断电持久性） | Task 3 之外 | spec §6 | ④ | **BLOCKED**：**普通 reopen 不证明断电持久性** |
| 真实主机 reboot | Task 4 之外 | spec §6 | 非自动化执行 | **不在自动化范围**（HostRebootOs 恒 Unsupported 仅为纯组件切片，不得宣称 full Task4 端到端完成） |

**分层记账规则（执行者必须遵守）：**
- ① 纯逻辑/组件级证据：本计划全部任务。
- ② 进程内 HTTP 权限矩阵 / 文件持久化故障注入 / 模拟网络与时钟：本计划只交付「文件持久化故障注入」部分；HTTP 与模拟网络属其它计划。
- ③ 双引擎真实 SQLx、并发/CAS、跨进程崩溃恢复：**本计划不涉及**；缺备份或隔离时相关 `#[ignore]` 测试必须保持未运行，绝不用 `return` 冒充通过。
- ④ 获批 codec 后的准确 512 KiB Protobuf 信封、真实认证/双流网络互通与受控浏览器端到端：**本计划一律不得声称**。HostRebootOs 永远 Unsupported 只能纯组件切片，不等于完整真实任务互通验收；**不得宣称 full Task4 端到端完成**。保留不获批 crypto/codec 绝不实施、不真实重启。

**最终自检清单（执行者在每项任务提交后核对）：**
- [ ] `cargo fmt -p <crate> -- --check` 通过。
- [ ] `cargo clippy --offline --locked -p <crate> --features test-seam -- -D warnings` 通过。
- [ ] `Cargo.toml` 与 `Cargo.lock` 始终同提交；未引入 `tempfile`/`prost`/`tonic`/任何密码学或网络新依赖。
- [ ] 没有新增任何密码学实现、没有自造 wire 编解码器、没有声称线协议互通。
- [ ] 所有测试中真实主机 reboot 调用次数为 0。
- [ ] 提交内容不含 `secret/` 内容、凭据、备份引用或伪造的 ACK/checkpoint。

---

## 执行交接

计划已保存到 `docs/superpowers/plans/2026-10-16-controller-v1-four-task4-transport-execution.md`。有两种执行方式：

1. **子代理驱动（推荐）：** 每项任务派新的子代理，任务间审查，快速迭代（必须使用 `superpowers:subagent-driven-development`，每项新代理 + 两阶段审查）。
2. **当前会话执行：** 使用 `superpowers:executing-plans`，分批落实并保留审查检查点。

**执行前必读的三条前置条件：**
- `crates/rsetup-board-agent` 尚不存在，且根 `Cargo.toml` 的 `members` 尚未包含它；Task 2 会同时改 `Cargo.toml` 与 `Cargo.lock`。
- `Cargo.lock` 中**没有** `tempfile`；Task 3 的测试临时目录必须使用 `tests/support/tempdir.rs` 的纯 std 实现。
- `rsetup-protocol::wire` 目前**没有** `Capabilities`/`DeviceStatus`/`ClockSample`/`RebootTicket`/`TaskRecord`，Runtime Task 4 的 `BoardClient` 契约依赖 Task 1 先交付它们。

**本轮单切片修订状态（Transport 草案 B04）：** 本轮仅同步「执行/持久化失败回包与 OS fake 计数」单一切片（B-04 裁决；权威依据 03 §1 第 13 行、03 §5 B-04 第 175 行、已 APPROVED interface-ownership 第 152 行），回执追踪见下方台账。其余旧问题（Metric/DeviceStatus 不派生 Eq（floatEq）、`BoardClient` 全接口（六方法文本契约已传播，真实适配/接线与完整复审未通过）、journal SERDE、tunnel binding route、目录清理等）保持未解，**整 Transport 仍 NOT APPROVED**；raw codec 未获批准前保持失败关闭（不以自造 Protobuf 替代）。本文件全部 git commit 提交示例均为简体中文描述（type/scope 可英文、描述中文），本轮不真实 commit。本轮另按 geminiB04 CHANGES_REQUIRED 修订 Task 4 `execute` 控制流（journal dedup 前置 → fresh boot 校验 → consume；dedup get `Err` 改 `Indeterminate`、无业务回包、不 OS；`ReusedExisting` 直接回已有持久 record、不 mark、无第二 OS）并新增 `JournalStore::dir()` 供 reopen 测试；**门禁状态更新**：`payload_matches` 已接入只读访问器（规范载荷哈希 CanonicalPayloadHash、排除 execution token/nonce），FakeJournal dedup 读故障与重放用例已补齐（dedup get 失败 → `Indeterminate` 且 OS 计数 0；bootChanged 后合法旧请求重放不得 `EXECUTION_TOKEN_INVALID`）——本补丁**不声称全部 B04 通过、不声称已可编译**；板端重复请求健壮性不变更中控当前策略（每 process 至多一次 execute、恢复仅 `task.get` 绝不自动重发）。

## B04 切片修订台账（回执追踪）

| 位置 | 同步内容 |
|---|---|
| Goal | OS 边界完整回包语义与 fake/物理计数（OS 边界前计数 0、OS 边界后计数 1、可靠 failed 仍可 status 0、attempted ≠ 成功） |
| Architecture 4 | 同上（组件视角） |
| Global Constraints 第 2 条 | 宿主物理 reboot 恒 0 与 FakeOS 计数严格区分（互为证据、无真硬件验收） |
| Global Constraints 第 4 条 | execute 严格错误与响应边界第 1–5 项（OS 边界前/后拆分；finish `DefinitelyNotCommitted`/`CommitUnknown` 均 Indeterminate） |
| Task 4 Interfaces | `ExecuteOutcome` 各变体 doc 注释（`Attempted` ≠ 成功；`Indeterminate` 拆 OS 边界前/后） |
| Task 4 Step 1 | 本切片 owned `FakeJournal`/`FakeOs`（故障在 open 成功后激活、针对 stage；真实文件故障仍由 journal-seam 切片门禁，不宣称完整 Task4 可编译）；桩 RED 说明 |
| Task 4 Step 2 | 原 `commit_unknown_at_any_stage…` 拆为 OS 边界前（计数 0）与 OS 后 finish（计数 1）两测试；新增 `finishDefinitelyNotCommitted` Indeterminate + 无业务回包断言；RED 预期（旧返回 attempted 或假零计数时断言 FAIL） |
| Task 4 Step 3 | `execute` 真实分支（finish `DefinitelyNotCommitted` → `Indeterminate`，不再返回旧 `attempted`）；调用方映射（error table） |
| Task 4 Step 4/5 | PASS 预期（FakeOS 与物理计数严格区分）；提交示例改中文（本轮不真实 commit） |
| 覆盖矩阵与验收账本 | Task 4 OS 边界行证据更新 |
| 执行交接 | 单切片修订状态：其余旧问题未解、整 Transport 仍 NOT APPROVED |
| Task 4 Step 3 / Task 3 接口 / 执行交接 | geminiB04 CHANGES_REQUIRED 控制流修订：journal dedup 前置 → fresh boot 校验 → consume（bootChanged 后合法旧请求不得 `EXECUTION_TOKEN_INVALID`）；dedup get `Err` → `Indeterminate`（只读 `task.get` 业务诊断不受影响）；`insert_accepted` `Ok((record, outcome))` 按 `DedupOutcome` 分支（`ReusedExisting` 直接回持久 record、不 mark、无第二 OS）；`ExecuteOutcome` 文档同步；`JournalStore::dir()` 访问器（接口 + 桩）；payload hash / test-fault 门禁已按后续切片闭合（`payload_matches` 已接入只读访问器、FakeJournal dedup 读故障与重放用例已补齐——dedup get 失败用例已改为经测试专用 `poison_records_for_test` 对 `records` 注入真实毒锁，execute 实际调用的 `get` 毒锁 `Err(Io)` 路径真实可达（`get` 为纯内存只读查询、不经 `Fs::read`，不再声称由 execute 首次 `Fs::read` 消耗故障））、不声称 APPROVED |

## 一次性切片修订台账：consume 先匹配 sub_task_id/expected_boot_id 再消耗（回执追踪）

**唯一聚焦任务：** `TicketManager::consume` 在消耗票据**之前**，严格匹配入站 `sub_task_id` 与 `expected_boot_id` 和票据绑定（当前 boot 由 execute 第 2 步 fresh 校验与 dedup 前置覆盖；`consume` 只比对绑定值），任一失败 `Err(ExecutionTokenInvalid)` 且保留原票据（**失败绝不消耗**），成功才标记已消费并返回绑定。`consume` 签名固定为 6 个业务参数（连同 `&self` 共 7 个输入）（新增 `sub_task_id: &str`、`expected_boot_id: &str`），计划内全部消费调用点与测试参数机械传播；dedup 命中仍在当前 boot/票据校验之前直接返回旧可靠 record，不给重放追加新 nonce/boot 门。

**本切片不混入 / 仍未补（列明）：** canonical payload hash（`payload_matches` 已接入只读访问器，CanonicalPayloadHash producer 已由 Task 1B 计划定义）、journal poison（FaultyFs/FakeJournal dedup 读故障与重放故障用例已补齐）、reconcile（中控侧收敛逻辑仍不在本计划）、OS（仍为固定六项 `BoardClient`/无参 `reboot()`，无真实重启，`HostRebootOs` 恒 `Unsupported`）、dualstream（Task 5 双流绑定未动）；固定六 `BoardClient` 方法/DTO/新 HTTP 字段不改；不新造 codec、不执行重启。本切片交付 `consume`、最小类型（`ActiveTicket` 私有结构、`map` 私有字段）与完整 `new`/`invalidate`/`active_ticket_count`（poison：fail-closed、不吞毒锁后继续授票、infallible 方法仅能 panic 时明确进程故障边界）、`checked_duration_since` 早 `Instant` 拒绝（纸面负例）；`prepare` 244-bit effective entropy dual UUIDv4 门禁已闭合，fail-closed poison 契约已明确，Task 4 已完成；四线尚未写齐/未完整 APPROVED，整 Transport 仍 NOT APPROVED；本轮不真实 commit。

| 位置 | 同步内容 |
|---|---|
| Task 4 Interfaces | `consume` 固定 6 个业务参数（连同 `&self` 共 7 个输入）签名（新增 `sub_task_id: &str`、`expected_boot_id: &str`）与完整 consume 实现说明（同一 Mutex 临界区先 `get`（不 `remove`）→ 严格校验 → 成功才标记已消费） |
| Task 4 Step 1 | 桩说明更新（RED：新 6 个业务参数（连同 `&self` 共 7 个输入）签名但故意忽略 `sub_task_id`/`expected_boot_id` 两参并照常消耗）；新增 2 个完整目标行为测试 `wrong_sub_task_id_fails_without_consuming_the_ticket` / `wrong_expected_boot_id_fails_without_consuming_the_ticket`（错 task/错 boot 失败不消耗 → 原正确 caller 仍能成功消费一次 → 再次消费拒绝）；旧 4 个测试 10 处 `consume` 调用参数机械传播（追加 `SUB_TASK, BOOT`），binding/manager/测试 fixture 直接复用既有 `prepare`/`binding()` |
| Task 4 Step 2 | 新增 2 条精确 `--exact` 测试选择器命令与 RED 预期（目标断言失败，非编译错误/非 0 用例） |
| Task 4 Step 3 | `consume` 完整实现代码（match-before-mutation；`map` 私有字段与 `active_ticket_count`（计 `consumed == false`）最小类型前置，无新增未定义 helper）；`execute` 消费调用传播 2 个新参（`&request.sub_task_id`、`&request.expected_boot_id`）与注释同步 |
| 覆盖矩阵与验收账本 | Task 4 票据行绑定维度补 `sub_task_id` 与「先匹配后消耗、失败不消耗」 |

## 一次性切片修订台账：TicketManager 最小类型/poison/早 `Instant`（回执追踪）

**唯一聚焦任务：** 票据切片提供可复制的完整局部实现——`ActiveTicket` 私有结构（`token`/`binding`/`issued_at: Instant`/`consumed: bool`）、`TicketManager { map: Mutex<BTreeMap<[u8; 32], ActiveTicket>> }`、完整 `pub fn new`/`pub fn invalidate`/`pub fn active_ticket_count` 与惯用 `impl Default`，并把 `pub fn consume`（6 个非 self 业务参数（连同 `&self` 共 7 个输入）、同 Mutex 只读 `get` 校验全部字段/期限后标记、不消费错 task/boot 票据）放入完整 `impl TicketManager` 上下文（含全部必要 `use`）；poison 行为 fail-closed（不吞毒锁后继续授票、保持现有无需 `Result` 的接口，infallible 方法毒锁时若只能 panic 只是可能的 task 线程 panic（不冒称整个进程必然退出；进程级故障隔离策略仍前置未解决、不把 panic 当业务成功）、不当安全业务状态；`consume` 用已有状态 fail-closed，毒锁调用直接返回而非持锁死等（std `Mutex` 中毒持久，本任务未实现 `clear_poison`/重建恢复路径，当前进程该 manager 保持 fail-closed、不得继续授票/消耗，需受控重建或进程重启且重建前不得 `execute`），不扩 journal poison）；`checked_duration_since` 拒绝早于 `issued_at` 的客户端任意早 `Instant`（纸面负例，不扩并发占用/OS/wire）；`prepare` 244-bit effective entropy dual UUIDv4 门禁已闭合（tuple 签名不变、不利用两个 Uuid 拼接冒称 256bit 全熵、不混引 `ring::rand`），fail-closed poison 契约已明确，Task 4 已完成。

| 位置 | 同步内容 |
|---|---|
| Task 4 Interfaces | `ActiveTicket` 私有结构完整定义 + `TicketManager` 真实 `map` 字段（替换注释字段）；`prepare` doc 更新为 244-bit effective entropy 门禁与 poison fail-closed 契约（现行正文已提供完整函数体，见 Task 4 Step 3 现行实现）；`consume` doc 补 `checked_duration_since` 早 `Instant` 拒绝与 poison fail-closed 契约（毒锁持久、本任务未实现恢复路径）；新增 `impl Default`（惯用 `Self::new()`） |
| Task 4 Step 1 | 桩 `ticket.rs` 声明完整最小类型（`ActiveTicket`/`map` 字段/`new`/`invalidate`/`active_ticket_count`/`impl Default` 完整代码在 Step 3），`prepare` 完整函数体已交付（dual UUIDv4 244-bit 熵 producer + poison fail-closed，见 Task 4 Step 3 现行实现） |
| Task 4 Step 3 | 完整 `TicketManager` 上下文代码（全部必要 `use`、`ActiveTicket`、`map` 字段、完整 `new`/`consume`/`invalidate`/`active_ticket_count`/`Default`、`prepare` 签名+门禁闭合注释（244-bit effective entropy dual UUIDv4 门禁已闭合、fail-closed poison 契约已明确））；`consume` 改 `pub` 与 `let mut map` 可变 guard（先只读 `get` 校验全部字段后才变更）；poison fail-closed 表述修正（毒锁持久、本任务未实现恢复路径，invalidate/count 毒锁 panic 只称可能的 task 线程 panic）；`checked_duration_since` 拒绝+纸面负例；新增惯用 `impl Default`（避免 Step 5 `-D warnings` 下 clippy `new_without_default`）；`execute` dedup→fresh boot→consume 顺序不变 |
| 文末台账 | 「四计划完整但未 APPROVED」改「四线尚未写齐/未完整 APPROVED」；`new` 由「不大修」改为本切片完整交付 |
| 计数不变 | 既有 6 个票据测试与 2 条新增 `--exact` 选择器命令不变（本切片不新增可执行测试；纸面负例不计数；`Default` 不新增测试）；不改其他领域；全文件 git commit 提交示例均为简体中文描述（type/scope 可英文、描述中文）；不新增依赖/codec |

## 一次性切片修订台账：RebootExecute 规范载荷哈希 producer（回执追踪）

**唯一聚焦任务：** 新增 Task 1B（位于 Task 1 之后、前置 Task 1）：Transport 唯一 DTO producer `crates/rsetup-protocol/src/wire.rs` 交付 `canonical_reboot_payload_hash(request: &RebootExecute) -> [u8; 32]` 完整生产定义 + 1 个完整表驱动纯测试（方法计数 1、多断言、全部 synthetic 输入、无真实凭据）。规范编码 = 固定版本化 domain/action 标签 `rsetup.device.reboot.execute.v1`（1B 长度前缀 + UTF-8）+ `schema_version`（固定 u32 大端）+ `sub_task_id`/`expected_boot_id`（固定 u64 大端长度前缀 + 规范小写 UUID UTF-8），整体 SHA-256（复用已直接依赖的 `ring::digest::Context`/`SHA256`，不加新依赖/manifest、不手写 SHA256；真实 `crates/rsetup-protocol/Cargo.toml` 已有 `ring.workspace = true`，board-agent 允许直接依赖无 `ring`/`rand`）；`execution_token` 不参与（票据一次性不属于载荷语义）、`nonce` 非 DTO 字段（wire 信封层、不造字段）、`controller_key` 归 journal key（去重键 `(controller_key, sub_task_id)`、不暗添 HTTP 字段）；形状校验为 Task 4 execute 前置、本函数纯函数（无 I/O/权限/boot 判断）且非 wire codec；长度前缀固定 u64 大端（绝不平台 usize），`u64::try_from` 可证明无损 + `expect` 守卫，函数返回 `[u8; 32]` 不带 `Result`、不引假错误枚举。RED 为可编译桩恒 `[0u8; 32]`（第一个语义字段变更 `assert_ne` 失败、非编译错误/非 0 用例），GREEN 为同名替换 ring SHA-256 实现；`--exact` 选择器恰 1（`--test wire_business_dto`，按文件位置不用 `--lib`）；按 worktree 私有 `CARGO_TARGET_DIR` 约定、不新引 `CARGO_HOME`、本任务不执行。

**本切片不混入 / 仍未补（列明，后续已更新）：** journal 只读访问器 `JournalStore::payload_hash` **已纸面实现**（完整实现 + 1 个纯内存测试，见 Task 4 Step 3 本节；**未编译、未实测**）；**不声称**完整 dedup 链闭合、**不声称**全链可编译、**不声称** producer 已实测；`execute` 签名与其他任务代码、其他 Agent 文件均不改；固定六 `BoardClient` 方法/DTO/新 HTTP 字段不改、不新造 codec、不加依赖；后续切片已接线 `payload_matches` 比较函数体与 `execute` 调用方（哈希传参为后续接线项：正文 `execute` 第 0 步仅注释占位说明调用方应传入 `payload_hash`，dispatch 尚无真实哈希计算代码）并补齐 Task 4 FakeJournal dedup 读故障与重放测试用例；四线尚未写齐/未完整 APPROVED、整 Transport 仍 NOT APPROVED；本轮不真实 commit。

| 位置 | 同步内容 |
|---|---|
| 文件结构与职责划分 | 新增 3 行 Task 1B（`wire.rs`/`lib.rs`/`wire_business_dto.rs`），复用 Task 1 文件与职责边界 |
| Task 1B（新增，位于 Task 1 之后） | Interfaces（Consumes：四字段 `RebootExecute` + `ring` 直接依赖证据；Produces：`[u8; 32]` 函数）、规范编码段与 6 条边界裁决、5 步 TDD（可编译桩 → 表驱动测试 → RED → GREEN 同名替换 → 提交）、`--exact` 选择器恰 1、私有 target 统一说明 |
| Task 4 Step 3 | `payload_matches` 剩余注释更新为「规范哈希 producer 已由 Task 1B 计划定义，且 payload_matches 辅助函数与 execute 调用方接线已纸面闭合」（不声称全链可编译/producer 已实测） |

## 一次性切片修订台账：Task 4 payload_matches 与 execute 接线纸面闭合（回执追踪）

**唯一聚焦任务：** 在 Task 4 `RebootExecutor` 补齐私有 helper `payload_matches(&self, controller_key: &[u8; 32], sub_task_id: &str, expected_hash: &[u8; 32]) -> bool`（经公共只读访问器 `JournalStore::payload_hash` 读取持久哈希比对，不触碰 `StoredRecord` 私有字段，读取失败 fail-closed 返回 false），并核对更新 `execute` 第 0 步校验后调用方接线（`canonical_reboot_payload_hash(request)` 产物传入 `payload_hash` 参数，向 `insert_accepted` 及 dedup 正常传参），更新剩余缺口注记为已纸面闭合。

**本切片不混入 / 仍未补（列明）：** OS 真实重启（`HostRebootOs` 恒 `Unsupported`）、FakeJournal/FaultyFs 故障注入场景、dualstream（Task 5 双流绑定未动）、raw codec 与未获批依赖；不声称整稿批准，不声称全链可编译或实测。

| 位置 | 同步内容 |
|---|---|
| Task 4 Step 3 | 补齐 `payload_matches` 私有 helper（经公共只读访问器 `JournalStore::payload_hash` 读取持久哈希比对、不触碰私有字段、读取 `Err` 保留 Result 语义原样上抛、由 `execute` dedup 调用处转 `Indeterminate`（TR-R7-3），仅确定哈希不一致才 `Ok(false)` 触发 `TaskConflict`，见 Task 4 Step 3 现行实现）；核对 `execute` 第 0 步校验后计算与接线说明；更新剩余项注释为已纸面闭合 |
| 文末台账 | 新增本切片修订回执，记录 `payload_matches` 只读访问器接线与 FakeJournal dedup 读故障及重放测试用例补齐，保留真实前置未闭合项 |

## 一次性切片修订台账：consume 签名 TicketOrigin 打包裁决（回执追踪）

**当前裁决：** 原规划 `consume` 为 `&self` 加 7 个业务参数（合计 8 个输入），曾与 clippy `too_many_arguments` 冲突；现由两个既有 binding 字符串字段（`connection_id`、`agent_instance_id`）打包为 `pub TicketOrigin { connection_id, agent_instance_id }`，`consume` 固定为 6 个业务参数（连同 `&self` 共 7 个输入，含 `origin`），接口与实现两签名及全部 17 个调用点（16 测试 + 1 execute）一致，常量为 `&str`；不 `allow`/`expect`、不降阈值、不改 wire/HTTP/`BoardClient`。参数承载变更仅落在本计划文档代码片段，未落地任何实际 Rust source，零新 payload/wire/HTTP/ownership 接口；本轮仅文字/台账修订，未运行 lint/测试。

**历史计数（保持不改）：** 旧台账「旧 4 个测试 10 处 `consume` 调用参数机械传播」为历史事实，明确不是当前总数；当前为 16 测试 + 1 execute 共 17 调用点。影子两新参 `sub_task_id`/`expected_boot_id` 的历史描述与真实参数仍匹配，不抹除。

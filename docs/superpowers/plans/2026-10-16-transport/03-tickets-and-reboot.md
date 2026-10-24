### Task 4: 30s 单调票据与固定无参 OS 重启边界（WIRE-03）

**Files:**
- Create: `crates/rsetup-board-agent/src/ticket.rs`
- Create: `crates/rsetup-board-agent/src/reboot.rs`
- Modify: `crates/rsetup-board-agent/src/lib.rs`
- Test: `crates/rsetup-board-agent/tests/wire_03_tickets.rs`
- Test: `crates/rsetup-board-agent/tests/reboot_boundary.rs`

**Interfaces:**
```rust
// ---- ticket.rs ----
pub const TICKET_TTL_SECS: u64 = 30; // 03 §5 待议默认，G0 审阅后才可定稿
pub const TICKET_TTL_MS: u32 = 30_000;
pub const TICKET_TOKEN_BYTES: usize = 32;

/// prepare 绑定：controller 公钥 + sub_task_id + expected_boot_id + 连接代际 + agent 实例。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketBinding {
    pub controller_key: [u8; 32],
    pub sub_task_id: String,
    pub expected_boot_id: String,
    pub connection_id: String,
    pub agent_instance_id: String,
}

/// consume 入站来源承载：仅把原 `connection_id`/`agent_instance_id` 两个 `&str` 参数合并为一个参数位，
/// 零新增数据、不改 wire/HTTP/`BoardClient` 接口与票据绑定匹配语义。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketOrigin<'a> {
    pub connection_id: &'a str,
    pub agent_instance_id: &'a str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketToken(pub [u8; TICKET_TOKEN_BYTES]);

/// 每 controller 一个活跃票据槽位（私有结构，不从 crate 导出）。
struct ActiveTicket {
    token: TicketToken,
    binding: TicketBinding,
    issued_at: std::time::Instant,
    consumed: bool,
}

/// 每 (controller_key) 至多一个活跃票据；已消费条目保留（拒绝重复）不计活跃；单调截止基于 `issued_at`。
pub struct TicketManager {
    map: std::sync::Mutex<std::collections::BTreeMap<[u8; 32], ActiveTicket>>,
}

impl TicketManager {
    pub fn new() -> Self;
    /// 每次调用生成新的 32B token，并覆盖该 controller 的活跃票据（03 §4）。
    /// **Task 4 已闭合**：producer = 两个 `Uuid::new_v4()` 的 16B 原始字节拼接，诚实声明为
    /// **244-bit effective entropy from dual UUIDv4**（每个 UUIDv4 固定 4 位 version + 2 位 variant，实际各 122 随机位），
    /// 不得虚标为 256-bit 全熵；`rsetup-core`/`rsetup-protocol` 当前均未导出经批准真熵源 helper，
    /// 若日后导出，接入点即 Task 4 Step 3 中 `prepare` 函数体唯一 producer 位置。
    /// **poison 契约（fail-closed）**：`self.map.lock()` 中毒 → `panic!` 并记录进程故障边界
    /// （"ticket map mutex poisoned: 进程故障边界（非安全业务状态）"），不得吞毒继续授票；完整实现见 Task 4 Step 3。
    pub fn prepare(
        &self,
        binding: TicketBinding,
        now: std::time::Instant,
    ) -> (TicketToken, rsetup_protocol::wire::RebootTicket);
    /// 一次性消费（match-before-mutation）：同一 Mutex 临界区先只读 `get`（不 `remove`），
    /// 按 controller（键定位）→ `token` → 连接代际 → agent 实例 → `sub_task_id` → `expected_boot_id` → 单调期限
    /// 严格匹配；任一不匹配返回 `Err(ExecutionTokenInvalid)` 且保留原票据（`consumed` 与条目均不变，失败绝不消耗）；
    /// 成功才标记已消费并返回绑定。重复调用必须返回 `ExecutionTokenInvalid`。
    /// `now` 早于 `issued_at`（客户端任意早的 `Instant`）由 `checked_duration_since` 拒绝（不靠 `duration_since` 饱和为 0 当合法）；
    /// 毒锁 → fail-closed `Err(ExecutionTokenInvalid)`（不 panic、不吞毒继续消耗）：调用直接返回而非持锁死等；std `Mutex` 中毒持久，本任务未实现 `clear_poison`/重建恢复路径，后续 `lock()` 仍 `Err`——当前进程该 manager 保持 fail-closed、不得继续授票/消耗，需受控重建或进程重启且重建前不得 `execute`；不主动吞毒（unsafe）授票；不扩 journal poison。
    /// 当前 boot 由 execute 第 2 步 fresh 校验（consume 之前）负责；此处只匹配票据绑定的 `expected_boot_id`，
    /// 不得靠消费后在 execute 检查 boot 再拒绝（会丢票据）。
    pub fn consume(
        &self,
        controller_key: &[u8; 32],
        origin: TicketOrigin<'_>,
        sub_task_id: &str,
        expected_boot_id: &str,
        token: TicketToken,
        now: std::time::Instant,
    ) -> Result<TicketBinding, BusinessStatus>;
    /// 连接断开或 agent 实例更换时作废。
    pub fn invalidate(&self, controller_key: &[u8; 32]);
    pub fn active_ticket_count(&self) -> usize;
}

/// 惯用构造兼容 `new()`：public 无参 `new` 触发 clippy `new_without_default`，
/// 与 Step 5 `cargo clippy -- -D warnings` 门禁冲突；同 Task 5 `ApprovalGate` 计划内先例；
/// 不改 wire/权限/依赖、不扩 board 公开 HTTP 接口。
impl Default for TicketManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---- reboot.rs ----
/// 固定无参：绝不接受 command / path / argv。
pub trait RebootOs: Send + Sync {
    fn reboot(&self) -> Result<(), std::io::Error>;
}

/// 生产实现：统计调用次数；测试中断言为 0。
pub struct HostRebootOs { /* 私有：calls: AtomicUsize */ }
impl HostRebootOs {
    pub fn new() -> Self;
    pub fn call_count(&self) -> usize;
}
/// 落地于 `crates/rsetup-board-agent/src/reboot.rs`，紧邻 `impl HostRebootOs`；后文 GREEN 片段不得重复实现本 `Default`。
impl Default for HostRebootOs {
    fn default() -> Self {
        Self::new()
    }
}
impl RebootOs for HostRebootOs { /* 真实主机重启入口；本计划中绝不实现调用 */ }

/// 测试替身：只记录调用，绝不触发真实重启。
pub struct RecordingOs { /* 私有：calls: AtomicUsize */ }
impl RecordingOs {
    pub fn new() -> Self;
    pub fn call_count(&self) -> usize;
}
/// 落地于 `crates/rsetup-board-agent/src/reboot.rs`，紧邻 `impl RecordingOs`；后文 GREEN 片段不得重复实现本 `Default`。
impl Default for RecordingOs {
    fn default() -> Self {
        Self::new()
    }
}
impl RebootOs for RecordingOs {
    fn reboot(&self) -> Result<(), std::io::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExecuteOutcome {
    /// `attempted` 已可靠落盘且 OS 调用返回 Ok（或重复 execute / 并发 `ReusedExisting` 命中已持久记录）：
    /// 只回复持久 record（status=0 持久 reply），**不等于成功**，也不承诺 `record.state`
    /// 为 `attempted`/succeeded（03 §5：attempted ≠ 成功；
    /// 真实重启证据需认证持久日志/boot transition）
    Attempted(TaskRecord),
    /// `accepted` 已可靠落盘但 `mark_attempted` 确未提交：不调用 OS（fake 计数 0），
    /// 必须可回复最后持久 accepted 的 status=0，不能非零
    Accepted(TaskRecord),
    /// OS 明确错误，且 `failed/OS_ERROR` 已可靠落盘：以 status=0 回复真实 failed 记录
    ///（FakeOS 计数 1、宿主物理 reboot 0）
    OsErrorPersisted(TaskRecord),
    /// 安全拒绝（非零 status_code，payload 为空）：仅允许在确证 `accepted` 未提交、
    /// 请求未被接受且尚未进入 OS 调用边界时
    Rejected(BusinessStatus),
    /// 断流、无业务回包，交由 `device.task.get` 核实：
    /// - dedup `journal.get` `Err`（OS 边界前、execute 第一次 I/O）：OS 绝对不调用（计数 0）；
    /// - `insert_accepted`/`mark_attempted` `CommitUnknown`（OS 边界前）：OS 绝对不调用（计数 0）；
    /// - `finish` `DefinitelyNotCommitted` 或 `CommitUnknown`（OS 边界后）：OS 已调用（计数 1），
    ///   不返回旧 `attempted`、不伪造新 failed/unknown、不非零暗示未执行
    Indeterminate,
}

pub struct RebootExecutor<O: RebootOs, F: Fs> {
    journal: std::sync::Arc<JournalStore<F>>,
    os: O,
    tickets: TicketManager,
    boot_id: Box<dyn Fn() -> String + Send + Sync>,
    agent_instance_id: String,
    /// 设备级原子占用标记（TR-N5 / 正审 R-05 / TR-R6-3）：同 agent 实例单设备串行互斥（1005 DeviceBusy）。
    /// 占用覆盖未决重启全生命周期：fresh execute 开始时申请；当且仅当确定未执行拒绝
    /// （DefinitelyNotCommitted / 票据无效 / boot 不匹配）或写入终态（Failed / Succeeded）才释放；OS Ok 返回 Attempted 中间态时保持占用。
    /// 恢复规则（TR-R6-3）：实例重启后经 new_recovering 重建时扫描持久 records，若存在任何
    /// state == Attempted 且 pre_boot_id == 当前 boot（重启已发起但 boot transition 未观察到）的记录，
    /// 恢复占用为 true（扫描失败按占用 fail-closed）；全部为终态或已观察到 boot transition 则为 false。
    device_occupied: std::sync::atomic::AtomicBool,
}

impl<O: RebootOs, F: Fs> RebootExecutor<O, F> {
    pub fn new(
        journal: std::sync::Arc<JournalStore<F>>,
        os: O,
        tickets: TicketManager,
        boot_id: Box<dyn Fn() -> String + Send + Sync>,
        agent_instance_id: impl Into<String>,
    ) -> Self; // device_occupied 初始化为 AtomicBool::new(false)（fresh journal 场景）
    /// TR-R6-3 恢复构造器：按上述恢复规则初始化 device_occupied（扫描失败按占用 fail-closed）
    pub fn new_recovering(
        journal: std::sync::Arc<JournalStore<F>>,
        os: O,
        tickets: TicketManager,
        boot_id: Box<dyn Fn() -> String + Send + Sync>,
        agent_instance_id: impl Into<String>,
    ) -> Self;
    pub fn os(&self) -> &O;
    pub fn tickets(&self) -> &TicketManager;

    pub fn prepare(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &rsetup_protocol::wire::RebootPrepare,
        now: std::time::Instant,
    ) -> Result<rsetup_protocol::wire::RebootTicket, BusinessStatus>;

    pub fn execute(
        &self,
        controller_key: &[u8; 32],
        connection_id: &str,
        request: &rsetup_protocol::wire::RebootExecute,
        payload_hash: &[u8; 32],
        now: std::time::Instant,
    ) -> ExecuteOutcome;
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `src/lib.rs` 追加 `pub mod reboot; pub mod ticket;` 与对应导出。桩 `ticket.rs` 按 Interfaces 块声明完整最小可编译类型前置：`ActiveTicket` 私有结构（`token`/`binding`/`issued_at: Instant`/`consumed: bool`）、`TicketManager { map: Mutex<BTreeMap<[u8; 32], ActiveTicket>> }`、完整 `pub fn new`/`pub fn invalidate`/`pub fn active_ticket_count`（完整代码在 Step 3，桩即按该代码）；`prepare` 仍缺失（桩仅给可编译占位体：硬编码 token 写入 `map` 槽位、返回按 binding 构造的 `RebootTicket`（`valid_for_ms = TICKET_TTL_MS`），不冒称熵门禁已闭合）。`ticket.rs` 的 `consume` 桩（6 个业务参数（连同 `&self` 共 7 个输入） `pub` 签名）恒返回 `Ok`（对应 binding）并标记 `consumed`，且不校验期限/代际/`sub_task_id`/`expected_boot_id`（本切片 RED：故意忽略新增的 `sub_task_id`/`expected_boot_id` 两参并照常消耗票据）；`reboot.rs` 的 `execute` 桩恒返回 `ExecuteOutcome::Rejected(BusinessStatus::Success)`（错误地把一切请求当成成功空结果）。本切片新增测试（`Indeterminate` 断言与 OS 计数断言）对该桩均可编译运行并断言失败；若保留旧分支（finish `DefinitelyNotCommitted` 返回 `Attempted`，或 OS 后 finish Unknown 仍用假零 OS 计数），相应断言同样 FAIL——正是本切片钉住的行为差异，**绝不能在已实现之后假称 RED**。

`tests/wire_03_tickets.rs`：
```rust
use rsetup_board_agent::dispatch::BusinessStatus;
use rsetup_board_agent::ticket::{TICKET_TTL_SECS, TicketBinding, TicketManager, TicketOrigin};
use std::time::{Duration, Instant};

const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";

fn binding() -> TicketBinding {
    TicketBinding {
        controller_key: [1u8; 32],
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT.to_string(),
        connection_id: "conn-1".to_string(),
        agent_instance_id: "agent-1".to_string(),
    }
}

fn ticket_origin<'a>(connection_id: &'a str, agent_instance_id: &'a str) -> TicketOrigin<'a> {
    TicketOrigin { connection_id, agent_instance_id }
}

#[test]
fn expired_ticket_never_executes() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, issued) = tickets.prepare(binding(), now);
    assert_eq!(issued.valid_for_ms, 30_000);
    assert_eq!(issued.boot_id, BOOT);
    assert_eq!(issued.sub_task_id, SUB_TASK);

    let after_ttl = now + Duration::from_secs(TICKET_TTL_SECS + 1);
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, after_ttl),
        Err(BusinessStatus::ExecutionTokenInvalid)
    );
    // 恰好到期也视为过期（单调期限严格小于）
    let exactly_ttl = now + Duration::from_secs(TICKET_TTL_SECS);
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, exactly_ttl),
        Err(BusinessStatus::ExecutionTokenInvalid)
    );
}

#[test]
fn ticket_is_bound_to_connection_agent_and_controller() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);

    assert_eq!(
        tickets.consume(&[2u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "another controller key must not consume the ticket"
    );
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-2", "agent-1"), SUB_TASK, BOOT, token, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a new connection must invalidate the ticket"
    );
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-2"), SUB_TASK, BOOT, token, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a new agent instance must invalidate the ticket"
    );
    // 上述失败尝试都不消耗票据
    let consumed = tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, now).expect("consume");
    assert_eq!(consumed, binding());
}

#[test]
fn ticket_is_single_use_and_new_prepare_replaces_the_active_one() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (first, _) = tickets.prepare(binding(), now);
    let (second, _) = tickets.prepare(binding(), now + Duration::from_millis(1));
    assert_ne!(first, second);
    assert_eq!(tickets.active_ticket_count(), 1, "at most one active ticket per controller");

    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, first, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a superseded ticket must not be usable"
    );
    // 第二张票据签发于 now + 1ms：消费时间戳不得早于签发时间，否则会被
    // 新实现的 checked_duration_since 正确拒绝；两次消费均使用该时间戳
    let consume_at = now + Duration::from_millis(1);
    tickets
        .consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, second, consume_at)
        .expect("newest ticket consumable");
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, second, consume_at),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a consumed ticket must not grant a second execution"
    );
    assert_eq!(tickets.active_ticket_count(), 0);
}

#[test]
fn invalidate_drops_the_active_ticket() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);
    tickets.invalidate(&[1u8; 32]);
    assert_eq!(tickets.active_ticket_count(), 0);
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, now),
        Err(BusinessStatus::ExecutionTokenInvalid)
    );
}

#[test]
fn wrong_sub_task_id_fails_without_consuming_the_ticket() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);

    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), "11111111-2222-4333-8444-555555555555", BOOT, token, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a different sub_task_id must not consume the ticket"
    );
    // 失败不消耗：原绑定 (SUB_TASK, BOOT) 的正确 caller 仍能成功消费一次
    let consumed = tickets
        .consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, now)
        .expect("the original binding must stay consumable after a wrong sub_task_id");
    assert_eq!(consumed, binding());
    // 一次性语义不变：再次消费必须被拒
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a consumed ticket must not grant a second execution"
    );
}

#[test]
fn wrong_expected_boot_id_fails_without_consuming_the_ticket() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);

    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, "6f8b1c2d-0000-4000-8000-000000000002", token, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a different boot id must not consume the ticket"
    );
    // 失败不消耗：原绑定 (SUB_TASK, BOOT) 的正确 caller 仍能成功消费一次
    // （consume 若先消费后校验，会在此处丢票据，正确 caller 将拿不到票据）
    let consumed = tickets
        .consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, now)
        .expect("the original binding must stay consumable after a wrong boot id");
    assert_eq!(consumed, binding());
    assert_eq!(
        tickets.consume(&[1u8; 32], ticket_origin("conn-1", "agent-1"), SUB_TASK, BOOT, token, now),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "a consumed ticket must not grant a second execution"
    );
}
```

`tests/reboot_boundary.rs`：

> 顺序要求：先完成下方测试文件创建及紧随其前的 Cargo.toml target 声明，再执行本任务任何测试命令（含 Step 2 定向 RED 与 Step 4 PASS）。`required-features` 不豁免显式 target 解析，target 文件缺失时所有 cargo 命令都会失败。

在 `crates/rsetup-board-agent/Cargo.toml` 既有 `[features]` 表之后追加本测试目标的显式声明（与下方文件创建构成同一准备步骤，二者齐备前不得运行任何测试命令；不得重复 `[features]` 表头）：
```toml
[[test]]
name = "reboot_boundary"
required-features = ["test-seam"]
```

> 说明：本文件顶部无条件 `use rsetup_board_agent::test_seam::RecordingJournal;`，而 `test_seam` 模块由 `#[cfg(any(test, feature = "test-seam"))]` 门控；集成测试 crate 编译其依赖 lib 时不设 `cfg(test)`，故该导入只能经 feature 门满足。运行本文件的任何命令（Step 2 定向 RED 与 Step 4 PASS）都必须带 `--features test-seam`，否则编译失败、选择器执行 0 用例。既有 cfg 写法本身不改。

```rust
mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::dispatch::BusinessStatus;
use rsetup_board_agent::journal::{Fs, JournalError, JournalStore, RealFs, TaskState};
use rsetup_board_agent::reboot::{ExecuteOutcome, HostRebootOs, RebootExecutor, RebootOs, RecordingOs};
use rsetup_board_agent::test_seam::RecordingJournal;
use rsetup_board_agent::ticket::TicketManager;
use rsetup_protocol::wire::{BUSINESS_SCHEMA_VERSION, RebootExecute, RebootPrepare};
use std::sync::Arc;
use std::time::Instant;

const BOOT_OLD: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const KEY: [u8; 32] = [1u8; 32];

struct Fixture<O: RebootOs> {
    _dir: TempDir,
    journal: Arc<JournalStore<RealFs>>,
    executor: RebootExecutor<O, RealFs>,
}

fn fixture_with<O: RebootOs>(os: O, boot: &'static str) -> Fixture<O> {
    let dir = TempDir::new("reboot");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("journal"));
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(move || boot.to_string()),
        "agent-1",
    );
    Fixture { _dir: dir, journal, executor }
}

fn prepare_request() -> RebootPrepare {
    RebootPrepare {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT_OLD.to_string(),
    }
}

fn execute_request(token: [u8; 32]) -> RebootExecute {
    RebootExecute {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT_OLD.to_string(),
        execution_token: token,
    }
}

// ---- 本切片的小 owned 测试替身（B04 裁决）----
// 故障注入必须在 `JournalStore::open` 成功后激活并针对本 stage，不被 open 的 meta 落盘消耗
//（meta 操作不计入注入计数）。两个替身均为 owned（无 self 借用、无 `Box<dyn Write + Send>`
// 生命周期缺口、`Fs`/`RebootOs` trait 匹配正确），供本切片直接测试 execute StateMachine；
// 真实文件故障注入（Task 3 `test_seam.rs` 的 `RecordingJournal` 偏移缝）仍门禁，
// 其依赖（`fail_at_offset` 偏移基准语义）由 journal-seam 切片提供；`RecordingWriter` 已是持有 `Arc<RecordingState>` 的 owned writer，无生命周期缺口。

/// execute StateMachine 的三个写 stage。
/// `activate_fault` 之后写 stage 按提交次序映射：
/// 第 1 次提交 = `insert_accepted`，第 2 次 = `mark_attempted`，第 3 次 = `finish`；
/// dedup get 为纯内存只读查找、不经 `Fs::read`，其失败用例由 `poison_records_for_test` 毒锁注入覆盖，不设读故障 stage。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage { InsertAccepted, MarkAttempted, Finish }

/// 单次提交的文件操作注入索引（write 发生在返回的 writer 上，不计入索引）：
/// 0 = `create_write`（rename 前 → DefinitelyNotCommitted）、1 = `sync_file`（rename 前 → DefinitelyNotCommitted）、
/// 2 = `rename`（rename 调用本身 → CommitUnknown）、3 = `sync_dir`（rename 后父目录 sync → CommitUnknown）。
const FAULT_CREATE_WRITE: usize = 0;
const FAULT_SYNC_FILE: usize = 1;
const FAULT_RENAME: usize = 2;
const FAULT_SYNC_DIR: usize = 3;

struct FakeJournalState {
    fs: RealFs,
    /// open 成功后激活：(目标 stage, 该 stage 单次落盘内的文件操作索引)，一次性。
    fault: Option<(Stage, usize)>,
    /// 激活以来的第 N 次提交。
    commit_no: usize,
}

#[derive(Clone)]
struct FakeJournal {
    state: std::sync::Arc<std::sync::Mutex<FakeJournalState>>,
}

fn stage_for_commit(commit_no: usize) -> Stage {
    match commit_no {
        1 => Stage::InsertAccepted,
        2 => Stage::MarkAttempted,
        _ => Stage::Finish,
    }
}

impl FakeJournal {
    fn new() -> Self {
        Self {
            state: std::sync::Arc::new(std::sync::Mutex::new(FakeJournalState {
                fs: RealFs,
                fault: None,
                commit_no: 0,
            })),
        }
    }

    /// 仅在 `JournalStore::open` 成功后调用：激活针对 stage 的故障并重置提交次序；
    /// open 的 meta 落盘操作已结束，不计入注入计数。
    fn activate_fault(&self, stage: Stage, op_in_commit: usize) {
        let mut st = self.state.lock().unwrap();
        st.fault = Some((stage, op_in_commit));
        st.commit_no = 0;
    }

    /// `create_write` 标志一次新提交开始（文件操作索引 0）。
    fn on_create_write(&self) -> std::io::Result<()> {
        let mut st = self.state.lock().unwrap();
        st.commit_no += 1;
        let stage = stage_for_commit(st.commit_no);
        if matches!(st.fault, Some((s, 0)) if s == stage) {
            st.fault = None;
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "injected failure at create_write"));
        }
        Ok(())
    }

    /// 提交内文件操作索引 1/2/3（sync_file/rename/sync_dir）。
    fn on_commit_op(&self, op: usize) -> std::io::Result<()> {
        let mut st = self.state.lock().unwrap();
        let stage = stage_for_commit(st.commit_no);
        if matches!(st.fault, Some((s, i)) if s == stage && i == op) {
            st.fault = None;
            return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("injected failure at commit op {op}")));
        }
        Ok(())
    }
}

impl Fs for FakeJournal {
    fn create_dir_all(&self, dir: &std::path::Path) -> std::io::Result<()> {
        self.state.lock().unwrap().fs.create_dir_all(dir)
    }
    fn create_write(&self, path: &std::path::Path) -> std::io::Result<Box<dyn std::io::Write + Send>> {
        self.on_create_write()?;
        self.state.lock().unwrap().fs.create_write(path)
    }
    /// 纯委托读：dedup get 为纯内存只读查找、不经 `Fs::read`，此处不设文件读故障 seam；
    /// dedup get 失败用例由 `poison_records_for_test` 毒锁注入覆盖。
    fn read(&self, path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
        self.state.lock().unwrap().fs.read(path)
    }
    fn sync_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        self.on_commit_op(FAULT_SYNC_FILE)?;
        self.state.lock().unwrap().fs.sync_file(path)
    }
    fn rename(&self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        self.on_commit_op(FAULT_RENAME)?;
        self.state.lock().unwrap().fs.rename(from, to)
    }
    fn remove_file(&self, path: &std::path::Path) -> std::io::Result<()> {
        self.state.lock().unwrap().fs.remove_file(path)
    }
    fn sync_dir(&self, dir: &std::path::Path) -> std::io::Result<()> {
        // 提交点父目录 sync 为索引 3；提交后临时文件清理的 sync_dir 不参与注入（故障一次性）
        self.on_commit_op(FAULT_SYNC_DIR)?;
        self.state.lock().unwrap().fs.sync_dir(dir)
    }
}

/// 小 owned fake OS：可配置 Ok/错误并计数。这是「FakeOS 计数」，与宿主物理 reboot 计数
/// `HostRebootOs::call_count()`（恒 0）严格区分，互为证据、不得互相冒充。
struct FakeOs {
    calls: std::sync::atomic::AtomicUsize,
    fail: bool,
}

impl FakeOs {
    fn ok() -> Self {
        Self { calls: std::sync::atomic::AtomicUsize::new(0), fail: false }
    }
    fn failing() -> Self {
        Self { calls: std::sync::atomic::AtomicUsize::new(0), fail: true }
    }
    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl RebootOs for FakeOs {
    fn reboot(&self) -> Result<(), std::io::Error> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            Err(std::io::Error::new(std::io::ErrorKind::Other, "fake OS error"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn prepare_rejects_boot_mismatch_without_issuing_a_ticket() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    let wrong = RebootPrepare {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: "6f8b1c2d-0000-4000-8000-0000000000ff".to_string(),
    };
    assert_eq!(
        fixture.executor.prepare(&KEY, "conn-1", &wrong, Instant::now()),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fixture.executor.tickets().active_ticket_count(), 0);
    assert_eq!(fixture.executor.os().call_count(), 0);
}

#[test]
fn execute_with_invalid_or_expired_ticket_never_calls_os() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    // 无任何 prepare：token 无效
    assert_eq!(
        fixture
            .executor
            .execute(&KEY, "conn-1", &execute_request([0u8; 32]), &[0u8; 32], Instant::now()),
        ExecuteOutcome::Rejected(BusinessStatus::ExecutionTokenInvalid)
    );

    let now = Instant::now();
    let issued = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    assert_eq!(issued.valid_for_ms, 30_000);
    assert_eq!(
        fixture.executor.execute(
            &KEY,
            "conn-1",
            &execute_request(issued.execution_token),
            &[0u8; 32],
            now + std::time::Duration::from_secs(31),
        ),
        ExecuteOutcome::Rejected(BusinessStatus::ExecutionTokenInvalid)
    );
    assert_eq!(fixture.executor.os().call_count(), 0, "expired ticket must never reach the OS");
    assert_eq!(fixture.journal.record_count(), 0, "rejected execute must not create a record");
}

#[test]
fn accepted_and_attempted_are_durable_before_the_os_call() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    let now = Instant::now();
    let issued = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");

    let outcome = fixture.executor.execute(
        &KEY,
        "conn-1",
        &execute_request(issued.execution_token),
        &[0u8; 32],
        now,
    );
    match outcome {
        ExecuteOutcome::Attempted(record) => {
            assert_eq!(record.state, TaskState::Attempted);
            assert_eq!(record.pre_boot_id, BOOT_OLD);
        }
        other => panic!("expected Attempted, got {other:?}"),
    }
    assert_eq!(fixture.executor.os().call_count(), 1, "exactly one OS call after durable attempted");

    // 落盘事实独立于内存：重开日志必须看到 attempted
    let reopened = JournalStore::open(fixture.journal.dir(), RealFs).expect("reopen");
    let record = reopened.get(&KEY, SUB_TASK).expect("get").expect("record");
    assert_eq!(record.state, TaskState::Attempted);
}

#[test]
fn pending_attempted_keeps_device_occupied_until_terminal_or_boot_transition() {
    // TR-R6-3 用例 1：OS Ok 返回未决 Attempted 中间态期间设备持续占用，持有效票据的另一 sub_task
    // 必须收到 1005 DeviceBusy，FakeOS 计数保持 1（未决期间绝不发生第二次 OS 调用）
    const SUB_TASK_B: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    let fixture = fixture_with(FakeOs::ok(), BOOT_OLD);
    let now = Instant::now();
    let issued_a = fixture.executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare a");
    let outcome_a = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_request(issued_a.execution_token), &[0u8; 32], now);
    assert!(matches!(outcome_a, ExecuteOutcome::Attempted(_)));
    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    let issued_b = fixture.executor.prepare(&KEY, "conn-1", &prepare_b, now).expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    let outcome_b = fixture.executor.execute(&KEY, "conn-1", &execute_b, &[0u8; 32], now);
    assert!(matches!(outcome_b, ExecuteOutcome::Rejected(BusinessStatus::DeviceBusy)));
    assert_eq!(fixture.executor.os().call_count(), 1, "no second OS call while Attempted is pending");
}

#[test]
fn terminal_failed_releases_device_occupation_for_next_sub_task() {
    // TR-R6-3 用例 2：终态 Failed/OS_ERROR 可靠落盘后占用释放，新 sub_task 允许重新占用并进入 OS 边界
    const SUB_TASK_B: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
    let fixture = fixture_with(FakeOs::failing(), BOOT_OLD);
    let now = Instant::now();
    let issued_a = fixture.executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare a");
    let outcome_a = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_request(issued_a.execution_token), &[0u8; 32], now);
    assert!(matches!(outcome_a, ExecuteOutcome::OsErrorPersisted(_)));
    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    let issued_b = fixture.executor.prepare(&KEY, "conn-1", &prepare_b, now).expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    let outcome_b = fixture.executor.execute(&KEY, "conn-1", &execute_b, &[0u8; 32], now);
    assert!(matches!(outcome_b, ExecuteOutcome::OsErrorPersisted(_)), "terminal state written then new sub_task may occupy again");
    assert_eq!(fixture.executor.os().call_count(), 2);
}

#[test]
fn duplicate_execute_returns_persisted_record_without_a_second_os_call() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    let now = Instant::now();
    let issued = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    let first = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);
    assert!(matches!(first, ExecuteOutcome::Attempted(_)));

    // 同 sub_task_id、同 payload：返回现有记录，不要求旧票据仍有效，也不再次执行
    let replay = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_request([9u8; 32]), &[0u8; 32], now);
    assert!(
        matches!(replay, ExecuteOutcome::Attempted(ref r) if r.state == TaskState::Attempted),
        "replay must reuse the persisted record, got {replay:?}"
    );
    assert_eq!(fixture.executor.os().call_count(), 1, "replay must never call the OS twice");

    // 同 ID 不同 payload：TASK_CONFLICT，且仍然不调用 OS
    let conflict = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_request([9u8; 32]), &[7u8; 32], now);
    assert_eq!(conflict, ExecuteOutcome::Rejected(BusinessStatus::TaskConflict));
    assert_eq!(fixture.executor.os().call_count(), 1);

    // 同 ID 不同 expected_boot_id：同样冲突
    let mut different_boot = execute_request(issued.execution_token);
    different_boot.expected_boot_id = "6f8b1c2d-0000-4000-8000-000000000002".to_string();
    assert_eq!(
        fixture.executor.execute(&KEY, "conn-1", &different_boot, &[0u8; 32], now),
        ExecuteOutcome::Rejected(BusinessStatus::TaskConflict)
    );
    assert_eq!(fixture.executor.os().call_count(), 1);
}

#[test]
fn os_error_is_persisted_as_failed_and_reported_with_success_envelope() {
    // OS 明确报错（fake OS 计数 1、宿主物理 reboot 0）且 finish failed/OS_ERROR 可靠持久
    // → 以 status=0 回复真实 failed（B-04）
    let fixture = fixture_with(FakeOs::failing(), BOOT_OLD);
    let now = Instant::now();
    let issued = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    match fixture
        .executor
        .execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now)
    {
        ExecuteOutcome::OsErrorPersisted(record) => {
            assert_eq!(record.state, TaskState::Failed);
            assert_eq!(record.reason_code.as_deref(), Some("OS_ERROR"));
        }
        other => panic!("expected OsErrorPersisted, got {other:?}"),
    }
    assert_eq!(
        fixture.executor.os().call_count(),
        1,
        "OS error path must have crossed the OS boundary exactly once (fake count 1)"
    );
    let reopened = JournalStore::open(fixture.journal.dir(), RealFs).expect("reopen");
    assert_eq!(
        reopened.get(&KEY, SUB_TASK).unwrap().unwrap().state,
        TaskState::Failed,
        "OS error evidence must be durable before answering"
    );
}

#[test]
fn accepted_durable_but_attempted_definitely_not_committed_answers_accepted_status_zero() {
    let dir = TempDir::new("accepted-only");
    let fs = FakeJournal::new();
    let journal = Arc::new(JournalStore::open(dir.path(), fs.clone()).expect("open"));
    // 故障在 open 成功后激活、针对 mark_attempted stage（索引 0 = create_write，rename 前
    // → 确未提交）；open 的 meta 落盘操作不被注入计数消耗
    fs.activate_fault(Stage::MarkAttempted, FAULT_CREATE_WRITE);
    let os = FakeOs::ok();
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    );
    let now = Instant::now();
    let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

    let outcome = executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);
    match outcome {
        ExecuteOutcome::Accepted(record) => {
            // 03 §1/§5（B-04）权威规范：accepted 已可靠落盘而 attempted 确未提交时，
            // 绝不调用 OS（fake 计数 0），必须可回复 status=0 且携带最后可靠持久的 accepted（不能返回非零错误暗示未接收）
            assert_eq!(record.state, TaskState::Accepted);
            assert_eq!(record.pre_boot_id, BOOT_OLD);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(executor.os().call_count(), 0, "fake OS count must be 0 when attempted fails to commit");
}

#[test]
fn commit_unknown_at_sync_dir_puts_store_in_uncertain_state_and_later_ops_fail_closed() {
    // TR-N2 / 正审 R-02：`Stage::InsertAccepted` + `FAULT_SYNC_DIR`（rename 后父目录 sync 失败 →
    // `CommitUnknown`）时，新记录可能已在磁盘 rename 落地而内存未换；同一 store 实例必须进入
    // uncertain 隔离态：`get` 绝不回旧内存记录，`insert_accepted` 绝不再发起整文件提交。
    let dir = TempDir::new("cu-sync-dir-uncertain");
    let fs = FakeJournal::new();
    let journal = JournalStore::open(dir.path(), fs.clone()).expect("open");
    fs.activate_fault(Stage::InsertAccepted, FAULT_SYNC_DIR);
    assert!(
        matches!(
            journal.insert_accepted(&KEY, "cu-uncertain-probe", &[0u8; 32], BOOT_OLD),
            Err(JournalError::CommitUnknown)
        ),
        "sync_dir fault at insert_accepted must classify as CommitUnknown"
    );
    assert!(
        matches!(
            journal.get(&KEY, "cu-uncertain-probe"),
            Err(JournalError::Io(msg)) if msg.contains("uncertain state")
        ),
        "get on uncertain store must fail closed with the uncertain Io error, never stale in-memory record"
    );
    assert!(
        matches!(
            journal.insert_accepted(&KEY, "cu-uncertain-probe", &[0u8; 32], BOOT_OLD),
            Err(JournalError::Io(msg)) if msg.contains("uncertain state")
        ),
        "insert_accepted on uncertain store must fail closed, never rewrite the file from the stale snapshot"
    );
}

#[test]
fn commit_unknown_at_pre_os_boundary_results_in_indeterminate_with_zero_os_calls() {
    // (a) insert_accepted 提交结果不明（OS 边界前）
    {
        let dir = TempDir::new("cu-insert-accepted");
        let fs = FakeJournal::new();
        let journal = Arc::new(JournalStore::open(dir.path(), fs.clone()).expect("open"));
        fs.activate_fault(Stage::InsertAccepted, FAULT_RENAME); // rename 调用本身 → CommitUnknown
        let os = FakeOs::ok();
        let executor = RebootExecutor::new(
            Arc::clone(&journal),
            os,
            TicketManager::new(),
            Box::new(|| BOOT_OLD.to_string()),
            "agent-1",
        );
        let now = Instant::now();
        let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

        let outcome = executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);
        assert_eq!(
            outcome,
            ExecuteOutcome::Indeterminate,
            "CommitUnknown at insert_accepted must result in Indeterminate (stream cut, no business reply)"
        );
        assert_eq!(executor.os().call_count(), 0, "pre-OS boundary: fake OS count must be 0");
    }
    // (b) mark_attempted 提交结果不明（仍为 OS 边界前）
    {
        let dir = TempDir::new("cu-mark-attempted");
        let fs = FakeJournal::new();
        let journal = Arc::new(JournalStore::open(dir.path(), fs.clone()).expect("open"));
        fs.activate_fault(Stage::MarkAttempted, FAULT_RENAME);
        let os = FakeOs::ok();
        let executor = RebootExecutor::new(
            Arc::clone(&journal),
            os,
            TicketManager::new(),
            Box::new(|| BOOT_OLD.to_string()),
            "agent-1",
        );
        let now = Instant::now();
        let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

        let outcome = executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);
        assert_eq!(
            outcome,
            ExecuteOutcome::Indeterminate,
            "CommitUnknown at mark_attempted must result in Indeterminate (stream cut, no business reply)"
        );
        assert_eq!(
            executor.os().call_count(),
            0,
            "pre-OS boundary: attempted not reliably persisted, fake OS count must be 0"
        );
    }
}

#[test]
fn commit_unknown_at_finish_after_os_results_in_indeterminate_with_one_os_call() {
    let dir = TempDir::new("cu-finish");
    let fs = FakeJournal::new();
    let journal = Arc::new(JournalStore::open(dir.path(), fs.clone()).expect("open"));
    // OS 明确报错后 finish 的 rename 失败 → finish 提交结果不明（OS 边界后）
    fs.activate_fault(Stage::Finish, FAULT_RENAME);
    let os = FakeOs::failing();
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    );
    let now = Instant::now();
    let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

    let outcome = executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);
    assert_eq!(
        outcome,
        ExecuteOutcome::Indeterminate,
        "post-OS finish CommitUnknown must result in Indeterminate (stream cut, no business reply); must not answer a real failed/unknown"
    );
    // finish 故障前 OS 已调用一次：fake 计数为 1（与宿主物理 reboot 恒 0 严格区分）
    assert_eq!(
        executor.os().call_count(),
        1,
        "post-OS boundary: fake OS count must be 1 (the old fake zero count was wrong)"
    );
}

#[test]
fn finish_definitely_not_committed_is_indeterminate_without_business_reply() {
    let dir = TempDir::new("dnc-finish");
    let fs = FakeJournal::new();
    let journal = Arc::new(JournalStore::open(dir.path(), fs.clone()).expect("open"));
    // OS 明确报错后 finish 的 create_write 失败（rename 前 → 确未提交，OS 边界后）
    fs.activate_fault(Stage::Finish, FAULT_CREATE_WRITE);
    let os = FakeOs::failing();
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    );
    let now = Instant::now();
    let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

    let outcome = executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);
    // B-04：finish 确未提交 → Indeterminate 断流、**无业务回包**
    //（`Indeterminate` 不携带 `TaskRecord` payload，本精确匹配即「无 reply」断言）：
    // 不返回旧 `attempted`（旧分支正是返回它，对此断言 FAIL），
    // 不伪新 failed/unknown，不非零 `Rejected` 暗示未执行。
    assert_eq!(
        outcome,
        ExecuteOutcome::Indeterminate,
        "finish DefinitelyNotCommitted must be Indeterminate without any business reply (not the old Attempted, not a faked Failed/Unknown)"
    );
    assert_eq!(
        executor.os().call_count(),
        1,
        "post-OS boundary: fake OS count is 1 while the host physical reboot stays 0"
    );
}

#[cfg(feature = "test-seam")]
#[test]
fn durable_write_failure_never_reaches_the_os_and_never_claims_success() {
    use rsetup_board_agent::test_seam::RecordingJournal;
    use std::io::ErrorKind;

    let dir = TempDir::new("durable-fail");
    // 真实文件故障注入（仍门禁）：故障落在 accepted 提交的 create_write（rename 之前 => 确未提交）。
    // 偏移基准 = 激活时刻的调用计数（含 open 的 META_FILE 落盘全部操作；T5 后 open 不再含 CreateDir），
    // 故在 open 成功后经 `fs_handle().arm_at_offset` 激活，offset 0 才首个命中 accepted 提交而非 open 落盘。
    // 本任务不大改 writer（`RecordingWriter` 已是持有 `Arc<RecordingState>` 的 owned writer）。
    let journal = Arc::new(JournalStore::open(dir.path(), RecordingJournal::new()).expect("open"));
    journal.fs_handle().arm_at_offset(0, ErrorKind::Other);
    let os = RecordingOs::new();
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    );
    let now = Instant::now();
    let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

    assert_eq!(
        executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now),
        ExecuteOutcome::Rejected(BusinessStatus::InternalError),
        "a definite accepted-write failure must be safely rejected with non-zero error"
    );
    assert_eq!(executor.os().call_count(), 0, "attempted was never durable: OS calls must be zero");
}

#[test]
fn real_host_reboot_counter_stays_zero_in_automated_tests() {
    // 生产替身只暴露计数，绝不提供任何触发路径。
    // 与 FakeOS（FakeOs/RecordingOs）调用计数严格区分：fake 计数证明「OS 边界被进入」（可为 1），
    // 本宿主物理 reboot 计数恒 0；两者互为证据、不得互相冒充（无真硬件验收）。
    let os = HostRebootOs::new();
    assert_eq!(os.call_count(), 0);
    // 类型层面：reboot() 必须无参数
    let _arity: fn(&HostRebootOs) -> Result<(), std::io::Error> = RebootOs::reboot;
    // 本测试运行前后都不得出现真实 reboot
    assert_eq!(os.call_count(), 0);
}

// ---- B04 门禁补充：FakeJournal dedup 故障与重放用例（对齐 4465 行门禁）----
// 注：与上文 durable-write 用例同纪律——用例支撑单切片语义，不声称「完整 Task 4 可编译」。

#[test]
fn journal_dedup_get_failure_results_in_indeterminate_with_zero_os_calls() {
    // 使用文件本地 test seam（本文件定义的 FakeJournal）仅作为 open 的 Fs 后端，不再注入 Fs 读故障：
    // `get` 是纯内存只读查找（数据源为 `open` 后的内存 `records`，不经 `Fs::read`），
    // 旧文件读故障 seam（已删除）无法到达 execute 实际调用的 dedup get 错误路径（TR-01 修正：
    // 此前声称「故障由 execute 首次 read 真实消耗」不成立）。本用例改用测试专用 seam
    // `poison_records_for_test` 对 `records` mutex 注入真实毒锁，使 `get` 毒锁 `Err(Io)` 路径确定性可达：
    // 读失败 ≠ 确定未命中（记录可能已在盘上），execute 控制流必须断流为 Indeterminate，
    // 不得伪造 Ok(None) 继续消费票据或进入 OS 边界。
    let dir = TempDir::new("dedup-get-fail");
    let journal = Arc::new(JournalStore::open(dir.path(), FakeJournal::new())
        .expect("open must succeed: this case injects no Fs fault, open must land meta normally"));
    let os = RecordingOs::new();
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    );
    let now = Instant::now();
    let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

    // 先对 `records` mutex 注入真实毒锁（test-seam 专用：不读写 Fs、不清毒、不修改记录），
    // 再直接断言 `get` 毒锁路径返回去敏 `Err`（全限定路径，不依赖本文件导入；
    // 毒锁在过滤前即失败，probe `sub_task_id` 取值不影响结论）：
    journal.poison_records_for_test();
    assert!(
        matches!(
            journal.get(&KEY, "dedup-poison-probe"),
            Err(rsetup_board_agent::journal::JournalError::Io(_))
        ),
        "poisoned records mutex must make get return the sanitized Io error, never a faked Ok(None)"
    );

    // 同一 executor：execute 的 dedup get 命中毒锁返回 Err ⇒ 断流：Indeterminate、无业务回包、不进 OS 边界
    assert_eq!(
        executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now),
        ExecuteOutcome::Indeterminate,
        "dedup get poisoned-lock error must be cut as Indeterminate (no business reply, no OS)"
    );
    assert_eq!(executor.os().call_count(), 0, "dedup get fails before the OS boundary: OS calls must be strictly zero");
}

#[test]
fn replay_after_boot_changed_does_not_consume_or_return_execution_token_invalid() {
    let dir = TempDir::new("replay-boot-changed");
    let journal = Arc::new(JournalStore::open(dir.path(), RecordingJournal::new()).expect("open"));
    const BOOT_NEW: &str = "6f8b1c2d-0000-4000-8000-000000000002";
    let os = RecordingOs::new();
    // boot 源可翻转：首次 execute 前为 BOOT_OLD，重放前改为新 boot 值
    let boot = Arc::new(std::sync::Mutex::new(BOOT_OLD.to_string()));
    let boot_src = Arc::clone(&boot);
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(move || boot_src.lock().unwrap().clone()),
        "agent-1",
    );
    let now = Instant::now();
    let issued = executor.prepare(&KEY, "conn-1", &prepare_request(), now).expect("prepare");

    // 首次合法 execute：dedup 未命中，正常消费 token，记录持久化，OS 恰好一次
    let first = executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);
    assert_ne!(first, ExecuteOutcome::Indeterminate, "initial legitimate execute must not be cut");
    // os 的所有权已移入 executor，经其访问器读取计数，避免多余借用冲突
    assert_eq!(executor.os().call_count(), 1, "first execute crosses the OS boundary exactly once");

    // 设备 boot 变更后，重放同一合法旧请求：dedup 命中已有持久记录
    *boot.lock().unwrap() = BOOT_NEW.to_string();
    let second = executor.execute(&KEY, "conn-1", &execute_request(issued.execution_token), &[0u8; 32], now);

    // 命中时直接返回已有持久 record：绝不 ExecutionTokenInvalid、不二次 mark/消费、不二次触发 OS
    assert!(
        matches!(second, ExecuteOutcome::Attempted(_)),
        "replay after boot changed must return the persisted record, got {:?}",
        second
    );
    assert_eq!(executor.os().call_count(), 1, "replay must not trigger a second OS call");
}
```

- [ ] **Step 2: 运行并确认行为 RED**

> 说明：集成测试编译其依赖的 lib 时不会设置 `cfg(test)`，`test_seam` 模块只能经 `test-seam` feature 门暴露，因此本组命令必须统一带 `--features test-seam`；否则 `tests/reboot_boundary.rs` 顶部的 `use rsetup_board_agent::test_seam::RecordingJournal;` 直接编译失败，选择器执行 0 用例而非目标断言失败。

```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test wire_03_tickets expired_ticket_never_executes -- --exact
```
预期：编译通过、执行 1 个用例，因 `consume` 桩不校验期限而返回 `Ok`，断言失败。
```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test reboot_boundary execute_with_invalid_or_expired_ticket_never_calls_os -- --exact
```
预期：编译通过、执行 1 个用例，因 `execute` 桩返回 `Rejected(Success)` 而断言失败。**不得出现编译错误或 0 用例。**
```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test reboot_boundary commit_unknown_at_pre_os_boundary_results_in_indeterminate_with_zero_os_calls -- --exact
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test reboot_boundary commit_unknown_at_finish_after_os_results_in_indeterminate_with_one_os_call -- --exact
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test reboot_boundary finish_definitely_not_committed_is_indeterminate_without_business_reply -- --exact
```
预期：各自编译通过、执行 1 个用例并断言失败（`execute` 桩返回 `Rejected(Success)`，而断言期望 `Indeterminate`）。**具体可编译 RED 桩必须行为错误**：若保留旧分支（finish `DefinitelyNotCommitted` 返回 `Attempted`，或 OS 后 finish Unknown 仍按假零 OS 计数断言），相应断言同样 FAIL——GREEN 修真实分支后同批断言转 PASS。**不得出现编译错误或 0 用例，不得在已实现之后假称 RED。**
```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test wire_03_tickets wrong_sub_task_id_fails_without_consuming_the_ticket -- --exact
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test wire_03_tickets wrong_expected_boot_id_fails_without_consuming_the_ticket -- --exact
```
预期：各自编译通过、执行 1 个用例；RED 桩（`consume` 新 6 个业务参数（连同 `&self` 共 7 个输入）签名但忽略 `sub_task_id`/`expected_boot_id` 并照常消耗）下，首个断言（期望 `Err(ExecutionTokenInvalid)`）因返回 `Ok` 失败，且票据已被错误消耗——紧随的「原正确 caller 仍能成功消费一次」断言同样失败（目标断言失败，非编译错误、非 0 用例）；GREEN 以 match-before-mutation 修复后同批断言转 PASS。

- [ ] **Step 3: 最小实现**

1. `ticket.rs` 最小类型与 `consume`（**本切片交付**）；`prepare` 完整函数体已随本切片交付（244-bit effective entropy 门禁 + poison fail-closed 契约）。以下为完整 `TicketManager` 上下文（含全部必要 `use`），可逐字复制；常量 `TICKET_TTL_SECS`/`TICKET_TTL_MS`/`TICKET_TOKEN_BYTES` 与 `TicketBinding`/`TicketToken` 按上方 Interfaces 块（已完整定义）；含 `prepare` 完整函数体在内均为本切片完整实现：
   ```rust
   // ---- ticket.rs（本切片交付：最小类型 + consume + 完整 prepare）----
   use std::collections::BTreeMap;
   use std::sync::Mutex;
   use std::time::{Duration, Instant};
   use uuid::Uuid;

   use rsetup_protocol::wire::RebootTicket;

   use crate::dispatch::BusinessStatus;

   /// 每 controller 一个活跃票据槽位（私有结构，不从 crate 导出）。
   struct ActiveTicket {
       token: TicketToken,
       binding: TicketBinding,
       issued_at: Instant,
       consumed: bool,
   }

   /// 每 (controller_key) 至多一个活跃票据；已消费条目保留（拒绝重复）不计活跃；
   /// 单调截止基于 `issued_at`（单调时钟，绝不 `SystemTime`）。
   pub struct TicketManager {
       map: Mutex<BTreeMap<[u8; 32], ActiveTicket>>,
   }

   impl TicketManager {
       /// 空 manager。infallible：不持锁，无锁中毒。
       pub fn new() -> Self {
           Self { map: Mutex::new(BTreeMap::new()) }
       }

       /// 覆盖该 controller 的活跃票据并返回票据（03 §4）。**Task 4 实现规范与 32B token 熵门禁闭合**：
       /// **熵源（32B token producer）**：board-agent 允许直接依赖（`rsetup-protocol`/`rsetup-core`/`tokio`/`uuid`/`thiserror`/`serde`/`serde_json` workspace 依赖）内无真熵 helper；
       /// `rsetup-core`/`rsetup-protocol` 当前均未导出经批准真熵源 helper，故采用两个 `Uuid::new_v4()` 的 16B 原始字节拼接构造 32B；
       /// **诚实声明：244-bit effective entropy from dual UUIDv4** —— 每个 UUIDv4 固定 4 位 version + 2 位 variant，实际各 122 随机位，拼接后 244 位，**不得虚标为 256-bit 全熵**；`ring::rand` 虽锁定但不在允许直接依赖内，**不得混引**；
       /// 若日后 `rsetup-core` 或 `rsetup-protocol` 导出经批准真熵源 helper，接入点为本函数体唯一 producer 位置（`raw` 填充处），替换后须同步更新本注释与 `TicketToken` 类型文档。
       /// **poison 契约（fail-closed）**：`self.map.lock()` 返回 `Err`（中毒）→ `panic!` 并记录 "ticket map mutex poisoned: 进程故障边界（非安全业务状态）"；不得吞毒后继续授票，不得借 `Result` 回传（tuple 签名）；panic 即进程故障边界，由 board-agent 进程级故障处理（日志/受控重启）承接。
       pub fn prepare(
           &self,
           binding: TicketBinding,
           now: Instant,
       ) -> (TicketToken, RebootTicket) {
            // 32B token producer：dual UUIDv4 字节拼接（244-bit effective entropy from dual UUIDv4，非 256-bit 全熵）。
            // 若接入经批准真熵 helper，替换本段即可（唯一 producer 位置）。
            let a = Uuid::new_v4().into_bytes();
            let b = Uuid::new_v4().into_bytes();
            let mut raw = [0u8; TICKET_TOKEN_BYTES];
            raw[..16].copy_from_slice(&a);
            raw[16..].copy_from_slice(&b);
            let token = TicketToken(raw);
            // poison 契约（fail-closed）：中毒即 panic（进程故障边界，非安全业务状态），不吞毒授票。
            let mut map = match self.map.lock() {
                Ok(guard) => guard,
                Err(_) => panic!("ticket map mutex poisoned: 进程故障边界（非安全业务状态）"),
            };
            // 覆盖写入：每 controller 至多一个活跃票据槽位（03 §4）。
            map.insert(
                binding.controller_key,
                ActiveTicket {
                    token,
                    binding: binding.clone(),
                    issued_at: now,
                    consumed: false,
                },
            );
            let ticket = RebootTicket {
                schema_version: 1,
                sub_task_id: binding.sub_task_id,
                boot_id: binding.expected_boot_id,
                execution_token: token.0,
                valid_for_ms: TICKET_TTL_MS,
            };
            (token, ticket)
        }

       /// 一次性消费（match-before-mutation，6 个非 self 参数）：同一 Mutex 临界区先只读 `get`（不 `remove`），
       /// 按 controller 键 → `token` → `connection_id` → `agent_instance_id` → `sub_task_id` → `expected_boot_id` → 单调期限严格校验；
       /// 任一不匹配 → `Err(ExecutionTokenInvalid)` 且保留原票据（**失败绝不消耗**，不消费错 task/boot 票据）；`now` 早于 `issued_at` 由 `checked_duration_since` 拒绝
       /// （不靠 `duration_since` 饱和为 0 当合法）；全部通过才标记已消费（**不删除条目**，重复调用返回 `ExecutionTokenInvalid`）并返回 binding clone。
       /// poison：fail-closed——毒锁返回 `Err(ExecutionTokenInvalid)`（不 panic、不吞毒继续消耗）：调用直接返回而非持锁死等；std `Mutex` 中毒持久，本任务未实现 `clear_poison`/重建恢复路径，后续 `lock()` 仍 `Err`——当前进程该 manager 保持 fail-closed、不得继续授票/消耗，需受控重建或进程重启且重建前不得 `execute`；不主动吞毒（unsafe）授票；不扩 journal poison。
       pub fn consume(
           &self,
           controller_key: &[u8; 32],
           origin: TicketOrigin<'_>,
           sub_task_id: &str,
           expected_boot_id: &str,
           token: TicketToken,
           now: Instant,
       ) -> Result<TicketBinding, BusinessStatus> {
           let TicketOrigin { connection_id, agent_instance_id } = origin;
           let mut map = match self.map.lock() {
               Ok(guard) => guard,
               // fail-closed：毒锁 → 按无有效票据处理（不 panic、不吞毒继续消耗）
               Err(_) => return Err(BusinessStatus::ExecutionTokenInvalid),
           };
           // (a) 只读 get（不 remove）：不存在或已消费 → 失败，保留现状
           let slot = match map.get(controller_key) {
               Some(slot) if !slot.consumed => slot,
               _ => return Err(BusinessStatus::ExecutionTokenInvalid),
           };
           // (b) 严格校验：任一不匹配即失败，此刻尚无任何状态变更
           let elapsed = match now.checked_duration_since(slot.issued_at) {
               Some(elapsed) => elapsed,
               // `now` 早于 `issued_at`（客户端任意早的 Instant）：拒绝
               None => return Err(BusinessStatus::ExecutionTokenInvalid),
           };
           if slot.token != token
               || slot.binding.connection_id != connection_id
               || slot.binding.agent_instance_id != agent_instance_id
               || slot.binding.sub_task_id != sub_task_id
               || slot.binding.expected_boot_id != expected_boot_id
               || elapsed >= Duration::from_secs(TICKET_TTL_SECS)
           {
               return Err(BusinessStatus::ExecutionTokenInvalid);
           }
           // (c) 全部通过才变更：标记已消费并返回绑定（不删除条目）
           let slot = map.get_mut(controller_key).expect("present and active in (a)");
           slot.consumed = true;
           Ok(slot.binding.clone())
       }

       /// 连接断开或 agent 实例更换时作废。
       /// infallible 接口：毒锁时若只能 panic，须明确声明为**只是可能的 task 线程 panic**（下方 `expect`），非安全业务状态；
       /// 不冒称整个进程必然退出（进程级故障隔离策略仍前置未解决、本任务未实现）；不新增未批准错误变体，
       /// 不吞毒后称作废成功、不把 panic 当业务成功。
       pub fn invalidate(&self, controller_key: &[u8; 32]) {
           let mut map = self
               .map
               .lock()
               .expect("ticket map mutex poisoned: 进程故障边界（非安全业务状态）");
           map.remove(controller_key);
       }

       /// 活跃票据数：仅计 `consumed == false` 的条目（已消费但保留的条目不计活跃）。
       /// infallible 接口：毒锁时若只能 panic，须明确声明为**只是可能的 task 线程 panic**（下方 `expect`），非安全业务状态；
       /// 不冒称整个进程必然退出（进程级故障隔离策略仍前置未解决）；不吞毒后称作数成功、不把 panic 当业务成功。
       pub fn active_ticket_count(&self) -> usize {
           let map = self
               .map
               .lock()
               .expect("ticket map mutex poisoned: 进程故障边界（非安全业务状态）");
           map.values().filter(|ticket| !ticket.consumed).count()
       }
   }

   impl Default for TicketManager {
       fn default() -> Self {
           Self::new()
       }
   }
   ```
   - **纸面负例（非可执行测试；既有 6 个票据测试与 2 条 `--exact` 选择器计数不变）**：输入 `now = issued_at - 1ms`（客户端任意早的 `Instant`），其余字段匹配绑定且未过期 → 期望 `Err(ExecutionTokenInvalid)`；旧 `duration_since`（饱和为 0）下该用例会误判合法，`checked_duration_since` 返回 `None` 即拒绝。该负例仅覆盖 `consume` 的早 `Instant` 分支，**不扩**并发占用/OS/wire。
   - 新增惯用 `impl Default for TicketManager { fn default() -> Self { Self::new() } }`（同 Task 5 `ApprovalGate` 计划内先例与 `rsetup-core` 既有 `impl Default` 模式；全部调用点/测试仍经 `TicketManager::new()` 构造，`Default` 不改 wire/权限/依赖、不扩 board 公开 HTTP 接口）：Step 5 `cargo clippy -- -D warnings` 门禁下 clippy `new_without_default`（public 无参 `new`）与未来 `-D warnings` 冲突，补惯用实现而非新加 `#[allow]` lint。
   - **切片边界**：本切片交付 `consume`、最小类型与完整 `new`/`invalidate`/`active_ticket_count`/`impl Default`（poison：fail-closed、调用直接返回而非持锁死等、本任务未实现恢复路径）；`prepare`（244-bit dual UUIDv4 token producer/熵门禁）已随本切片交付——本切片不声称端到端已验收：OS/断电/真实 reboot 门禁仍前置未解，既有 6 个票据测试亦未在本任务运行。
2. `RebootExecutor::prepare`：
   - `request.schema_version != 1` → `UnsupportedVersion`；`request.sub_task_id` 非空且是规范 UUIDv4、`expected_boot_id` 是规范 UUIDv4 → 否则 `InvalidArgument`。
   - `self.current_boot() != request.expected_boot_id` → `InvalidArgument`（03 §4：不一致不发票据）。
   - 调用 `tickets.prepare(...)` 返回票据。
3. `RebootExecutor::execute`（**顺序不可调换**：形状校验 → journal dedup → fresh boot 校验 → consume → insert_accepted → mark_attempted → os.reboot）：
```rust
/// 私有 dedup 比较 helper：通过 JournalStore::payload_hash 只读访问器读取持久哈希，
/// 并比对是否与 expected_hash 相等；保留 Result 语义（TR-R7-3）：读取 Err（uncertain 隔离态、
/// 毒锁、解码失败等）原样上抛，由 execute 调用处转 Indeterminate，绝不吞为 false 伪造 TaskConflict；
/// Ok(None)（记录存在但哈希缺失/损坏）按不匹配，仅确定哈希不一致才由调用方回复 TaskConflict。
fn payload_matches(&self, controller_key: &[u8; 32], sub_task_id: &str, expected_hash: &[u8; 32]) -> Result<bool, JournalError> {
    match self.journal.payload_hash(controller_key, sub_task_id) {
        Ok(Some(actual_hash)) => Ok(&actual_hash == expected_hash),
        Ok(None) => Ok(false), // 记录存在但无对应哈希（损坏/缺失），按不匹配或由调用方处理
        Err(e) => Err(e),
    }
}

pub fn execute(
    &self,
    controller_key: &[u8; 32],
    connection_id: &str,
    request: &RebootExecute,
    payload_hash: &[u8; 32],
    now: Instant,
) -> ExecuteOutcome {
    // 0) 形状校验（不触达任何 I/O）：先拒绝不合法的不同请求；合法请求继续走去重。
    //    注：调用方在第 0 步校验完成后调用 canonical_reboot_payload_hash(request) 计算 payload_hash，
    //    并向本函数的 payload_hash 参数传参，供后续 dedup 比对与 insert_accepted 正常消费。
    if request.schema_version != BUSINESS_SCHEMA_VERSION {
        return ExecuteOutcome::Rejected(BusinessStatus::UnsupportedVersion);
    }
    if request.sub_task_id.trim().is_empty()
        || !is_canonical_uuid_v4(&request.sub_task_id)
        || !is_canonical_uuid_v4(&request.expected_boot_id)
    {
        return ExecuteOutcome::Rejected(BusinessStatus::InvalidArgument);
    }

    // 1) journal dedup 前置（键 = 已认证 controller_key + sub_task_id）：
    //    已存在同 ID 的持久记录时，核对历史 pre_boot/语义哈希并直接回已有记录，
    //    不要求当前 boot 匹配，也不要求旧票据仍有效
    //    （板端重复请求健壮性 ≠ 客户端获准重发；中控策略不变：每 process 至多一次 execute、恢复仅 get 绝不自动重发）
    match self.journal.get(controller_key, &request.sub_task_id) {
        // dedup get 是 execute 的第一次 I/O：Err → Indeterminate，无业务回包、绝不调用 OS（计数 0）；
        // 只读 `device.task.get` 出错返回业务诊断是另一条路径，不受此项影响
        Err(_) => return ExecuteOutcome::Indeterminate,
        Ok(Some(existing)) => {
            if existing.pre_boot_id != request.expected_boot_id {
                return ExecuteOutcome::Rejected(BusinessStatus::TaskConflict);
            }
            // TR-R7-3：读 Err（uncertain 隔离态/毒锁/解码失败）保持不确定性断流 → Indeterminate，
            // 无任何事实证明客户端发送的载荷不同，绝不伪造确定性 TaskConflict；仅可靠哈希确实不同才 TaskConflict
            match self.payload_matches(controller_key, &request.sub_task_id, payload_hash) {
                Err(_) => return ExecuteOutcome::Indeterminate,
                Ok(false) => return ExecuteOutcome::Rejected(BusinessStatus::TaskConflict),
                Ok(true) => {}
            }
            return ExecuteOutcome::Attempted(existing); // 不再次执行、不 mark、不消费第二张票据、无第二 OS
        }
        Ok(None) => {}
    }

    // 2) 仅首次（无持久记录）：fresh 校验当前 boot，再消费票据
    //    （一次性，失败即拒绝；此时尚未产生任何持久写；
    //    bootChanged 之后合法旧请求已在第 1 步 dedup 返回，不会到达本校验，不得误判 EXECUTION_TOKEN_INVALID；
    //    consume 内部还会把 sub_task_id/expected_boot_id 与票据绑定匹配（match-before-mutation）：
    //    错 task/错 boot 的入站请求在票据处被拒且不消耗，不得靠消费后再检查 boot 来拒绝——那会丢票据）
    // 2) 设备级独占占用申请（TR-N5 / 正审 R-05）：去重未命中且准备进入 fresh execute 时申请
    if self.device_occupied.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return ExecuteOutcome::Rejected(BusinessStatus::DeviceBusy);
    }
    let current_boot = (self.boot_id)();
    if current_boot != request.expected_boot_id {
        self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst);
        return ExecuteOutcome::Rejected(BusinessStatus::ExecutionTokenInvalid);
    }
    match self.tickets.consume(
        controller_key,
        crate::ticket::TicketOrigin { connection_id, agent_instance_id: &self.agent_instance_id },
        &request.sub_task_id,
        &request.expected_boot_id,
        TicketToken(request.execution_token),
        now,
    ) {
        Err(status) => { self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst); return ExecuteOutcome::Rejected(status); },
        Ok(_) => {}
    }

    // 3) accepted 必须可靠落盘（pre_boot_id 在此确定）
    let accepted_record = match self.journal.insert_accepted(controller_key, &request.sub_task_id, payload_hash, &current_boot) {
        // 只有 Inserted 继续进 mark_attempted；ReusedExisting（竞态窗口：dedup 查询时未见记录、并发请求已先持久化）
        // 直接回已有持久 record：此处 `Attempted(record)` 命名表示 status=0 持久 reply，
        // 不承诺 record.state 为 attempted/success；不 mark、无第二 OS
        Ok((record, DedupOutcome::Inserted)) => record,
        Ok((record, DedupOutcome::ReusedExisting)) => { self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst); return ExecuteOutcome::Attempted(record); },
        Err(JournalError::DefinitelyNotCommitted) => {
            // 请求确未被接受且 OS 未进边界：消费票据但不执行，安全返回非零错误
            self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst);
            return ExecuteOutcome::Rejected(BusinessStatus::InternalError);
        }
        Err(JournalError::StorageFull) => { self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst); return ExecuteOutcome::Rejected(BusinessStatus::ResourceExhausted); },
        // insert_accepted 提交结果不明（CommitUnknown，OS 边界前）：断流待查、无业务回包、绝不调用 OS（计数 0）
        // CommitUnknown 时 store 已进入 uncertain 隔离态（TR-N2），保持 occupied 为 true 直至实例丢弃
        Err(JournalError::CommitUnknown) => return ExecuteOutcome::Indeterminate,
        Err(_) => { self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst); return ExecuteOutcome::Indeterminate; },
    };

    // 4) attempted 必须可靠落盘，之后才允许进入 OS 边界
    let attempted = match self.journal.mark_attempted(controller_key, &request.sub_task_id) {
        Ok(record) => record,
        Err(JournalError::DefinitelyNotCommitted) => {
            // accepted 已可靠写，但 attempted 确未提交：绝对不调用 OS！
            // 03 §1/§5 约束：必须使用 status=0 回复最后可靠持久的 accepted（绝不能返回非零错误暗示未接收）
            self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst);
            return ExecuteOutcome::Accepted(accepted_record);
        }
        // mark_attempted 提交结果不明（CommitUnknown，仍为 OS 边界前）：网络断流无业务回包、绝对不调用 OS（计数 0），交由 task.get 核查
        // CommitUnknown 时 store 已进入 uncertain 隔离态（TR-N2），保持 occupied 为 true 直至实例丢弃
        Err(JournalError::CommitUnknown) => return ExecuteOutcome::Indeterminate,
        Err(_) => { self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst); return ExecuteOutcome::Indeterminate; },
    };

    // 5) 至多一次 OS 调用边界（仅 `attempted` 可靠落盘之后）
    match self.os.reboot() {
        // OS 返回 Ok 且 `attempted` 可靠落盘：只回复 `attempted`，不叫成功
        //（03 §5：attempted ≠ 成功；真实重启证据需认证持久日志/boot transition）
        // TR-R6-3：OS Ok 仅表示「重启已发起」，任务仍处未决 Attempted 中间态（boot transition 尚未观察到），
        // 占用必须覆盖未决生命周期，此处绝不释放 device_occupied——否则另一 controller/sub_task 可拿到票据
        // 再次穿过第 2 步 swap 并二次调用 os.reboot()，破坏单设备物理重启排他保护。
        // 释放仅发生在：确定未执行拒绝（DefinitelyNotCommitted / 票据无效 / boot 不匹配）或写入终态（Failed / Succeeded）。
        Ok(()) => ExecuteOutcome::Attempted(attempted),
        Err(_) => match self
            .journal
            .finish(controller_key, &request.sub_task_id, TaskState::Failed, Some("OS_ERROR"))
        {
            // OS 明确报错且 `failed/OS_ERROR` 可靠落盘：以 status=0 回复真实 failed（FakeOS 计数 1、宿主物理 reboot 0）
            Ok(record) => { self.device_occupied.store(false, std::sync::atomic::Ordering::SeqCst); ExecuteOutcome::OsErrorPersisted(record) },
            // `finish` 确未提交（OS 边界后）：OS 已调用（fake 计数 1），不返回旧 `attempted`
            //（旧分支的 `ExecuteOutcome::Attempted(attempted)` 会暗示 OS 结果仍可知）、
            // 不伪造新 failed/unknown、不非零暗示未执行 → Indeterminate 断流、无业务回包
            // TR-R7-2：finish 确未提交 ⇒ 磁盘持久记录仍为未决 `Attempted`（Failed 终态未写入），
            // 未决期间必须持续排他：绝不释放 device_occupied（保持 true），否则另一 controller/sub_task
            // 可拿到票据二次穿过第 2 步 swap 并二次调用 os.reboot()；仅返回 Indeterminate。
            Err(JournalError::DefinitelyNotCommitted) => ExecuteOutcome::Indeterminate,
            // `finish` 提交结果不明（OS 边界后）：同样 Indeterminate 断流、无业务回包；CommitUnknown 保持 occupied 为 true
            Err(JournalError::CommitUnknown) | Err(_) => ExecuteOutcome::Indeterminate,
        },
    }
}
```
   - `payload_matches(existing, payload_hash)`：`StoredRecord` 内的 payload 哈希比较；journal 需新增只读访问器（非行为接口）：
     **实现与接线说明（Task 4 更新）**：本切片中 `self.payload_matches` 已在 `RebootExecutor` 内提供私有 helper 完整定义（TR-R7-3：返回 `Result<bool, JournalError>` 保留 Result 语义——`JournalStore::payload_hash` 读取 `Err`（uncertain 隔离态、毒锁、解码失败）原样上抛、由 `execute` dedup 调用处转 `Indeterminate`，`Ok(None)`（记录存在但哈希缺失/损坏）按不匹配，仅确定哈希不一致才 `Ok(false)` 触发 `TaskConflict`）；其规范载荷哈希（CanonicalPayloadHash）producer **已由 Task 1B 计划定义**（`canonical_reboot_payload_hash(request: &RebootExecute) -> [u8; 32]`，覆盖全部固定语义字段、**排除 execution token/nonce**——票据一次性不属于载荷语义），**journal 只读访问器已纸面实现**（`JournalStore::payload_hash` 完整实现已在 Task 3 接口 `impl`（与 `get` 同一 impl），另加 1 个纯内存测试，见本节下方；**未编译、未实测**），**`payload_matches` 比较函数与 `execute` 调用方接线已纸面闭合**（不声称全链可编译、不声称 producer 已实测）；下方只读访问器即为该比较服务：
   - **测试断言补充（TR-R7-3）**：当 journal 处于 uncertain 隔离态（`CommitUnknown`）、毒锁或解码失败时，重复请求到达 dedup 步骤经 `payload_matches` 得到 `Err`，`execute` 必须返回 `Indeterminate`（无业务回包、OS 计数 0），绝不错误返回确定性 `TaskConflict`；仅可靠读到的持久哈希确实与请求载荷哈希不同，才断言 `Rejected(TaskConflict)`。
```rust
impl<F: Fs> JournalStore<F> {
    /// 去重查询用：返回该记录保存的请求载荷哈希。
    pub fn payload_hash(&self, controller_key: &[u8; 32], sub_task_id: &str) -> Result<Option<[u8; 32]>, JournalError>;
}
```
    - `payload_hash` 只读访问器：完整实现 + 1 个纯内存测试（本子步为**纸面实现**；完整实现已写在 Task 3 接口 `impl<F: Fs> JournalStore<F>`（与 `get` 同一 impl，`record_count` 之后的完整函数体），此处给 TDD 顺序与测试模块；不改 `execute` 接线、不填 `payload_matches` 函数体）：
      (1) 先在 `journal.rs` 末尾写完整测试模块（独立 `#[cfg(test)] mod payload_hash_tests`，不重名既有 `hash_hex_tests`；完整 module/imports；全部 synthetic 数据，无 Fs/open/磁盘/假凭据；测试在同模块内直接构造 `JournalStore{...}` 字面量——子模块可见私有字段、`RealFs` 为单位结构体构造即不触盘、`dir`/`epoch` 取合成值且 getter 不读它们——不新增未批准生产 API）：
```rust
#[cfg(test)]
mod payload_hash_tests {
    use super::{encode_journal_hash32, JournalError, JournalStore, RealFs, StoredRecord, StoredTaskRecord};
    use std::sync::Mutex;

    /// 纯内存构造：直接组装合成 `StoredRecord`（`StoredTaskRecord` 全部字段按结构事实给全，值均 synthetic）。
    fn stored_record(controller: [u8; 32], sub_task_id: &str, payload_hash: [u8; 32]) -> StoredRecord {
        StoredRecord {
            controller_hex: encode_journal_hash32(&controller),
            sub_task_id: sub_task_id.to_owned(),
            payload_hash_hex: encode_journal_hash32(&payload_hash),
            record: StoredTaskRecord {
                schema_version: 1,
                sub_task_id: sub_task_id.to_owned(),
                journal_epoch: "synthetic-epoch".to_owned(),
                record_version: 1,
                state: "accepted".to_owned(),
                pre_boot_id: "synthetic-pre-boot".to_owned(),
                observed_boot_id: None,
                reason_code: None,
                updated_unix_ms: None,
                evidence_kind: None,
            },
        }
    }

    /// 同模块字面量构造（子模块可见私有字段；`RealFs` 单位结构体，构造不触盘；`dir`/`epoch` 合成值，getter 不读）。
    fn store_with(records: Vec<StoredRecord>) -> JournalStore<RealFs> {
        JournalStore {
            dir: std::path::PathBuf::from("/synthetic/journal"),
            epoch: "synthetic-epoch".to_owned(),
            fs: RealFs,
            records: Mutex::new(records),
            uncertain: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 唯一纯 lib 测试（纯内存、无 I/O、不打印原始记录内容）：0 命中 / 唯一命中 / 两类键不串 / 同键重复损坏 / 坏 hex 字段。
    #[test]
    fn journal_payload_hash_accessor_key_lookup_is_exact_and_corruption_fails_closed() {
        let controller_a = [0xaau8; 32];
        let controller_b = [0xbbu8; 32];
        let hash_a1 = [0x11u8; 32];
        let hash_a2 = [0x22u8; 32];
        // (1) 0 命中 → Ok(None)
        let empty = store_with(Vec::new());
        assert_eq!(empty.payload_hash(&controller_a, "synthetic-task-a"), Ok(None));

        // (2) 唯一命中 → Ok(Some(正确哈希))
        let unique = store_with(vec![stored_record(controller_a, "synthetic-task-a", hash_a1)]);
        assert_eq!(
            unique.payload_hash(&controller_a, "synthetic-task-a"),
            Ok(Some(hash_a1)),
        );

        // (3) 同 controller、不同 sub_task_id 不串
        let same_controller = store_with(vec![
            stored_record(controller_a, "synthetic-task-a", hash_a1),
            stored_record(controller_a, "synthetic-task-b", hash_a2),
        ]);
        assert_eq!(
            same_controller.payload_hash(&controller_a, "synthetic-task-b"),
            Ok(Some(hash_a2)),
        );
        assert_eq!(
            same_controller.payload_hash(&controller_a, "synthetic-task-missing"),
            Ok(None),
        );

        // (4) 不同 controller、同 sub_task_id 不串
        let same_task = store_with(vec![
            stored_record(controller_a, "synthetic-task-a", hash_a1),
            stored_record(controller_b, "synthetic-task-a", hash_a2),
        ]);
        assert_eq!(
            same_task.payload_hash(&controller_b, "synthetic-task-a"),
            Ok(Some(hash_a2)),
        );
        assert_eq!(
            same_task.payload_hash(&controller_a, "synthetic-task-a"),
            Ok(Some(hash_a1)),
        );

        // (5) 同键重复记录（损坏）→ 固定去敏 Decode（绝不返回第一条、不伪 None）
        let duplicated = store_with(vec![
            stored_record(controller_a, "synthetic-task-a", hash_a1),
            stored_record(controller_a, "synthetic-task-a", hash_a2),
        ]);
        assert_eq!(
            duplicated.payload_hash(&controller_a, "synthetic-task-a"),
            Err(JournalError::Decode("journal.duplicate_record".to_owned())),
        );

        // (6) 坏 hex 字段（64 字符但 uppercase）→ 固定 Decode("journal.hash_format")
        let proto = stored_record(controller_a, "synthetic-task-a", hash_a1);
        let bad_hex = store_with(vec![StoredRecord {
            controller_hex: proto.controller_hex,
            sub_task_id: proto.sub_task_id,
            payload_hash_hex: "FF".repeat(32),
            record: proto.record,
        }]);
        assert_eq!(
            bad_hex.payload_hash(&controller_a, "synthetic-task-a"),
            Err(JournalError::Decode("journal.hash_format".to_owned())),
        );
    }
}
```
      (2) RED：先在 `journal.rs` 同一 `impl<F: Fs> JournalStore<F>`（与 `get` 同一 impl）放同签名可编译桩（恒 `Ok(None)`，不用 (3) 的完整实现）：
```rust
    pub fn payload_hash(&self, _controller_key: &[u8; 32], _sub_task_id: &str) -> Result<Option<[u8; 32]>, JournalError> {
        Ok(None)
    }
```
      运行（未来命令，本轮不执行；`--offline --locked` 与 `-p` 私有 target 统一本任务其他命令）：
```bash
cargo test --offline --locked -p rsetup-board-agent --lib journal::payload_hash_tests::journal_payload_hash_accessor_key_lookup_is_exact_and_corruption_fails_closed -- --exact
```
      预期：编译通过、执行 1 个用例；恒 `Ok(None)` 桩下用例 (1) 0 命中恰通过，用例 (2) 唯一命中为**首个失败断言**（`Ok(None) ≠ Ok(Some(hash))`），退出码非零——RED。
      (3) GREEN：以完整实现替换桩（`payload_hash` 完整函数体已在 Task 3 接口 `impl` 中，与 `get` 同一 impl、紧随 `record_count`；毒锁 fail-closed 照抄本文件既定策略），重跑同命令，预期 PASS（GREEN）；测试模块与访问器实现在本子步写入 `journal.rs`，随下方提交步骤与 Task 3 的 `journal.rs` 一并提交（不新增提交步）。
   - 调用方映射（供 `BoardAgent` 与执行者参考，必须写进代码注释）：
     - `ExecuteOutcome::Attempted(record)` → `status_code = 0` + 持久 `TaskRecord`（**不代表 OS 调用成功，更不代表任务成功**：03 §5 `attempted ≠ 成功`，真实重启证据需认证持久日志/boot transition）
     - `ExecuteOutcome::Accepted(record)` → `status_code = 0` + 持久 `Accepted` 记录（03 §1/§5：accepted 已可靠落盘而 attempted 确未提交时，不调用 OS（fake 计数 0），必须以 status_code=0 回复最后持久 accepted，绝不能返回非零错误）
     - `ExecuteOutcome::OsErrorPersisted(record)` → `status_code = 0` + 持久 `Failed(OS_ERROR)` 记录（03 §1：已可靠接受后不得用非零错误暗示未接收；FakeOS 计数 1、宿主物理 reboot 0）
     - `ExecuteOutcome::Rejected(status)` → 非零 `status_code`，payload 为空（仅允许在确证 accepted 未提交、请求未被接受且尚未进入 OS 调用边界时返回）
     - `ExecuteOutcome::Indeterminate` → **无业务回包**（网络断流），留待后续 `device.task.get` 核查：
       - dedup `journal.get` 的 `Err`（OS 边界前、execute 第一次 I/O）：OS 绝对不调用（计数 0）；只读 `task.get` 出错返回业务诊断是另一条路径，不受此项影响；
       - `insert_accepted`/`mark_attempted` 的 `CommitUnknown`（OS 边界前）：OS 绝对不调用（计数 0）；
       - `finish` 的 `DefinitelyNotCommitted` 或 `CommitUnknown`（OS 边界后）：OS 已调用（计数 1），不返回旧 `attempted`、不伪造新 failed/unknown、不非零暗示未执行；
       - **不得宽泛声称「所有 OS 明确错误均无回包」**——可靠的 `OsErrorPersisted` 仍可 status 0
4. `HostRebootOs`：本计划**不实现**真实调用。**严正声明：HostRebootOs 永远 Unsupported 仅为纯组件安全切片，绝不等于完整真实任务互通验收；不得宣称 full Task4 端到端完成。保留不获批 crypto/codec 绝不实施、不真实重启。**
```rust
impl RebootOs for HostRebootOs {
    fn reboot(&self) -> Result<(), std::io::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // 纯组件安全切片：本计划中永远不在此调用系统重启。
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "real host reboot is not implemented in this component slice (unsupported)",
        ))
    }
}
```
   > 这样即使被误调用，也只会返回明确错误并留下计数证据，绝不会重启开发机；测试恒断言 `call_count() == 0`。

- [ ] **Step 4: 运行并确认通过（PASS）**

```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test wire_03_tickets
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test reboot_boundary
```
预期：PASS；`real_host_reboot_counter_stays_zero_in_automated_tests` 通过即证明自动化测试中宿主物理 reboot 为 0；本切片的 fake OS 计数断言（OS 边界前 `Indeterminate`/`Accepted` 用例计数为 `0`、OS 边界后 `Indeterminate`/`OsErrorPersisted` 用例计数为 `1`）严格证明「OS 边界至多被进入一次」，与宿主物理 reboot 0 严格区分、互为证据，无真硬件验收。

- [ ] **Step 5: 提交**

```bash
cargo fmt -p rsetup-board-agent
cargo clippy --offline --locked -p rsetup-board-agent --features test-seam -- -D warnings
git add crates/rsetup-board-agent/src/lib.rs crates/rsetup-board-agent/src/ticket.rs crates/rsetup-board-agent/src/reboot.rs crates/rsetup-board-agent/tests/wire_03_tickets.rs crates/rsetup-board-agent/tests/reboot_boundary.rs crates/rsetup-board-agent/src/journal.rs
git commit -m "feat(board-agent): 用单调票据与固定无参 OS 边界门禁重启执行"
```

---

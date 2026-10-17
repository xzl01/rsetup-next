//! WIRE-04 执行切片：30s 单调票据、Reboot 执行调度的「持久 → OS 边界」次序，
//! 以及 `BoardAgent` 对 `device.reboot.prepare` / `device.reboot.execute` / `device.task.get`
//! 的类型化调度。
//!
//! 隔离纪律：本文件的 OS 替身（`RecordingOs` / `FakeOs`）都是进程内计数器，绝不触发真实重启；
//! 宿主物理 reboot 入口（`HostRebootOs`）恒 `Unsupported` 且其计数恒 0。
//! fake 计数（证明 OS 边界被进入，可为 1）与宿主物理计数（恒 0）互为证据、不得互相冒充。

mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, ExecuteOutcome, HostRebootOs, RebootDispatcher,
    RebootExecutor, RebootOs, RecordingOs,
};
use rsetup_board_agent::journal::{Fs, JournalStore, RealFs, TaskState};
use rsetup_board_agent::tickets::{TICKET_TTL_SECS, TicketBinding, TicketManager, TicketOrigin};
use rsetup_board_agent::{JournalEpochSource, PeerContext, ReadOnlySource, TaskJournalReader};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, ClockSample, DeviceStatus, NoCodec,
    Query, RebootExecute, RebootPrepare, RebootTicket, TaskQuery, canonical_reboot_payload_hash,
};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BOOT_OLD: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const BOOT_NEW: &str = "6f8b1c2d-0000-4000-8000-000000000002";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const SUB_TASK_B: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const JOURNAL_EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";
const KEY: [u8; 32] = [1u8; 32];
const OTHER_KEY: [u8; 32] = [2u8; 32];

// ---------------------------------------------------------------------------
// 公共构造
// ---------------------------------------------------------------------------

fn binding() -> TicketBinding {
    TicketBinding {
        controller_key: KEY,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT_OLD.to_string(),
        connection_id: "conn-1".to_string(),
        agent_instance_id: "agent-1".to_string(),
    }
}

fn origin<'a>(connection_id: &'a str, agent_instance_id: &'a str) -> TicketOrigin<'a> {
    TicketOrigin {
        connection_id,
        agent_instance_id,
    }
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

/// 可配置 Ok/Err 的进程内 fake OS 替身（fake 计数，与宿主物理计数严格区分）。
struct FakeOs {
    calls: AtomicUsize,
    fail: bool,
}

impl FakeOs {
    fn ok() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail: false,
        }
    }
    fn failing() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            fail: true,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl RebootOs for FakeOs {
    fn reboot(&self) -> Result<(), std::io::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            Err(std::io::Error::other("fake OS error"))
        } else {
            Ok(())
        }
    }
}

struct Fixture<O: RebootOs> {
    _dir: TempDir,
    journal: Arc<JournalStore<RealFs>>,
    executor: RebootExecutor<O, RealFs>,
}

fn fixture_with<O: RebootOs>(os: O, boot: &'static str) -> Fixture<O> {
    let dir = TempDir::new("reboot");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("open journal"));
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(move || boot.to_string()),
        "agent-1",
    );
    Fixture {
        _dir: dir,
        journal,
        executor,
    }
}

// ---------------------------------------------------------------------------
// 1. 30s 单调票据
// ---------------------------------------------------------------------------

#[test]
fn ticket_expires_on_the_monotonic_deadline() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, issued) = tickets.prepare(binding(), now);
    assert_eq!(issued.valid_for_ms, 30_000);
    assert_eq!(issued.boot_id, BOOT_OLD);
    assert_eq!(issued.sub_task_id, SUB_TASK);
    assert_eq!(issued.execution_token, token.0);
    assert_ne!(
        issued.execution_token, [0u8; 32],
        "全零 token 视为缺省伪造，票据必须携带非零令牌"
    );

    let after_ttl = now + Duration::from_secs(TICKET_TTL_SECS + 1);
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            after_ttl
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "超过 30s 单调期限必须拒绝"
    );
    // 恰好到期同样视为过期（严格小于）
    let exactly_ttl = now + Duration::from_secs(TICKET_TTL_SECS);
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            exactly_ttl
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "恰好到期的边界同样按过期处理"
    );
    assert_eq!(
        tickets.active_ticket_count(),
        1,
        "过期尝试绝不消耗票据：条目仍在，但已不可消费"
    );
}

#[test]
fn ticket_is_bound_to_controller_connection_and_agent_instance() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);

    assert_eq!(
        tickets.consume(
            &OTHER_KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "另一个 controller 公钥不得消费他人票据"
    );
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-2", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "新连接代际必须使旧票据失效"
    );
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-2"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "新 agent 实例必须使旧票据失效"
    );
    // 上述失败尝试都不消耗票据
    let consumed = tickets
        .consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now,
        )
        .expect("失败尝试不得消耗票据");
    assert_eq!(consumed, binding());
}

#[test]
fn ticket_is_single_use_and_new_prepare_supersedes_the_previous_one() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (first, _) = tickets.prepare(binding(), now);
    let (second, _) = tickets.prepare(binding(), now + Duration::from_millis(1));
    assert_ne!(first, second, "每次 prepare 必须签发新令牌");
    assert_eq!(
        tickets.active_ticket_count(),
        1,
        "每 controller 至多一个活跃票据"
    );

    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            first,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "被覆盖的旧票据不得再使用"
    );
    // 第二张票据签发于 now + 1ms：消费时刻不得早于签发时刻
    let consume_at = now + Duration::from_millis(1);
    tickets
        .consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            second,
            consume_at,
        )
        .expect("最新票据可消费一次");
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            second,
            consume_at
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "已消费票据不得授予第二次执行"
    );
    assert_eq!(tickets.active_ticket_count(), 0);
}

#[test]
fn consume_rejects_an_instant_earlier_than_the_issue_time() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);
    // 客户端任意早的 Instant：`checked_duration_since` 必须拒绝，
    // 绝不靠 `duration_since` 饱和为 0 当合法
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now - Duration::from_millis(1),
        ),
        Err(BusinessStatus::ExecutionTokenInvalid)
    );
}

#[test]
fn invalidate_drops_the_active_ticket() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);
    tickets.invalidate(&KEY);
    assert_eq!(tickets.active_ticket_count(), 0);
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid)
    );
}

#[test]
fn wrong_sub_task_id_fails_without_consuming_the_ticket() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);

    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            "11111111-2222-4333-8444-555555555555",
            BOOT_OLD,
            token,
            now,
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "不同 sub_task_id 不得消费票据"
    );
    // 失败不消耗：原绑定的正确 caller 仍能成功消费一次
    let consumed = tickets
        .consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now,
        )
        .expect("错 task 的失败尝试不得丢票据");
    assert_eq!(consumed, binding());
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "已消费票据不得授予第二次执行"
    );
}

#[test]
fn wrong_expected_boot_id_fails_without_consuming_the_ticket() {
    let tickets = TicketManager::new();
    let now = Instant::now();
    let (token, _) = tickets.prepare(binding(), now);

    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_NEW,
            token,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "不同 boot 代际不得消费票据"
    );
    // 失败不消耗：先消费后校验会在此处丢票据
    let consumed = tickets
        .consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now,
        )
        .expect("错 boot 的失败尝试不得丢票据");
    assert_eq!(consumed, binding());
    assert_eq!(
        tickets.consume(
            &KEY,
            origin("conn-1", "agent-1"),
            SUB_TASK,
            BOOT_OLD,
            token,
            now
        ),
        Err(BusinessStatus::ExecutionTokenInvalid),
        "已消费票据不得授予第二次执行"
    );
}

// ---------------------------------------------------------------------------
// 2. 执行调度：持久次序、OS 边界与占用
// ---------------------------------------------------------------------------

#[test]
fn prepare_rejects_boot_mismatch_without_issuing_a_ticket() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    let wrong = RebootPrepare {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT_NEW.to_string(),
    };
    assert_eq!(
        fixture
            .executor
            .prepare(&KEY, "conn-1", &wrong, Instant::now()),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fixture.executor.tickets().active_ticket_count(), 0);
    assert_eq!(fixture.executor.os().call_count(), 0);
}

#[test]
fn execute_without_valid_ticket_never_calls_os_or_writes_journal() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    // 无任何 prepare：token 无效
    assert_eq!(
        fixture.executor.execute(
            &KEY,
            "conn-1",
            &execute_request([9u8; 32]),
            &[0u8; 32],
            Instant::now()
        ),
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
            now + Duration::from_secs(31),
        ),
        ExecuteOutcome::Rejected(BusinessStatus::ExecutionTokenInvalid)
    );
    assert_eq!(
        fixture.executor.os().call_count(),
        0,
        "过期票据绝不允许触达 OS 边界"
    );
    assert_eq!(
        fixture.journal.record_count(),
        0,
        "被拒绝的 execute 不得留下任何持久记录"
    );
}

#[test]
fn accepted_and_attempted_are_durable_before_the_os_call() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    let now = Instant::now();
    let issued = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    let request = execute_request(issued.execution_token);
    let payload_hash = canonical_reboot_payload_hash(&request);

    let outcome = fixture
        .executor
        .execute(&KEY, "conn-1", &request, &payload_hash, now);
    match outcome {
        // attempted ≠ 成功：只回复已可靠落盘的 attempted 记录
        ExecuteOutcome::Attempted(record) => {
            assert_eq!(record.state, TaskState::Attempted);
            assert_eq!(record.pre_boot_id, BOOT_OLD);
        }
        other => panic!("expected Attempted, got {other:?}"),
    }
    assert_eq!(
        fixture.executor.os().call_count(),
        1,
        "attempted 可靠落盘后恰好一次 OS 调用"
    );

    // 落盘事实独立于内存：重开日志必须看到 attempted
    let reopened = JournalStore::open(fixture.journal.dir(), RealFs).expect("reopen");
    let record = reopened.get(&KEY, SUB_TASK).expect("get").expect("record");
    assert_eq!(record.state, TaskState::Attempted);
    assert_eq!(record.pre_boot_id, BOOT_OLD);
}

#[test]
fn duplicate_execute_reuses_persisted_record_without_a_second_os_call() {
    let fixture = fixture_with(RecordingOs::new(), BOOT_OLD);
    let now = Instant::now();
    let issued = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    let first = execute_request(issued.execution_token);
    let payload_hash = canonical_reboot_payload_hash(&first);
    let outcome = fixture
        .executor
        .execute(&KEY, "conn-1", &first, &payload_hash, now);
    assert!(matches!(outcome, ExecuteOutcome::Attempted(_)));

    // 同 sub_task_id、同规范载荷（规范哈希排除 execution_token）：
    // 回已有持久记录，不要求旧票据仍有效，也不再次执行
    let replay = execute_request([9u8; 32]);
    let replay_hash = canonical_reboot_payload_hash(&replay);
    assert_eq!(
        replay_hash, payload_hash,
        "规范载荷哈希必须排除 execution_token"
    );
    let replayed = fixture
        .executor
        .execute(&KEY, "conn-1", &replay, &replay_hash, now);
    assert!(
        matches!(replayed, ExecuteOutcome::Attempted(ref r) if r.state == TaskState::Attempted),
        "重放必须复用持久记录, got {replayed:?}"
    );
    assert_eq!(
        fixture.executor.os().call_count(),
        1,
        "重放绝不第二次调用 OS"
    );

    // 同 ID 不同载荷哈希：TASK_CONFLICT，且仍然不调用 OS
    assert_eq!(
        fixture
            .executor
            .execute(&KEY, "conn-1", &replay, &[7u8; 32], now,),
        ExecuteOutcome::Rejected(BusinessStatus::TaskConflict)
    );
    assert_eq!(fixture.executor.os().call_count(), 1);

    // 同 ID 不同 expected_boot_id：同样冲突
    let mut different_boot = execute_request(issued.execution_token);
    different_boot.expected_boot_id = BOOT_NEW.to_string();
    assert_eq!(
        fixture
            .executor
            .execute(&KEY, "conn-1", &different_boot, &payload_hash, now,),
        ExecuteOutcome::Rejected(BusinessStatus::TaskConflict)
    );
    assert_eq!(fixture.executor.os().call_count(), 1);
}

#[test]
fn os_error_is_persisted_as_failed_and_answered_with_persistent_record() {
    // OS 明确报错（fake OS 计数 1、宿主物理 reboot 0）且 failed/OS_ERROR 可靠落盘
    // → 以 status=0 回复真实 failed 记录
    let fixture = fixture_with(FakeOs::failing(), BOOT_OLD);
    let now = Instant::now();
    let issued = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    let request = execute_request(issued.execution_token);
    let payload_hash = canonical_reboot_payload_hash(&request);
    match fixture
        .executor
        .execute(&KEY, "conn-1", &request, &payload_hash, now)
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
        "OS 错误路径必须恰好进入 OS 边界一次（fake 计数 1）"
    );
    let reopened = JournalStore::open(fixture.journal.dir(), RealFs).expect("reopen");
    assert_eq!(
        reopened
            .get(&KEY, SUB_TASK)
            .expect("get")
            .expect("record")
            .state,
        TaskState::Failed,
        "OS 错误证据必须先可靠落盘再回答"
    );
}

#[test]
fn pending_attempted_keeps_device_occupied_until_terminal_state() {
    // 未决 Attempted 期间设备持续占用：持有效票据的另一 sub_task 必须收到 1005 DeviceBusy，
    // fake OS 计数保持 1（未决期间绝不发生第二次 OS 调用）
    let fixture = fixture_with(FakeOs::ok(), BOOT_OLD);
    let now = Instant::now();
    let issued_a = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare a");
    let request_a = execute_request(issued_a.execution_token);
    let hash_a = canonical_reboot_payload_hash(&request_a);
    let outcome_a = fixture
        .executor
        .execute(&KEY, "conn-1", &request_a, &hash_a, now);
    assert!(matches!(outcome_a, ExecuteOutcome::Attempted(_)));
    assert!(
        fixture.executor.device_occupied(),
        "OS Ok 后未决 Attempted 必须保持占用"
    );

    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    let issued_b = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_b, now)
        .expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    let hash_b = canonical_reboot_payload_hash(&execute_b);
    let outcome_b = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_b, &hash_b, now);
    assert!(matches!(
        outcome_b,
        ExecuteOutcome::Rejected(BusinessStatus::DeviceBusy)
    ));
    assert_eq!(
        fixture.executor.os().call_count(),
        1,
        "未决 Attempted 期间不得发生第二次 OS 调用"
    );
    assert_eq!(
        fixture.journal.record_count(),
        1,
        "DeviceBusy 拒绝不得留下第二条持久记录"
    );
}

#[test]
fn terminal_failed_releases_device_occupation_for_next_sub_task() {
    // 终态 Failed/OS_ERROR 可靠落盘后占用释放，新 sub_task 允许重新占用并进入 OS 边界
    let fixture = fixture_with(FakeOs::failing(), BOOT_OLD);
    let now = Instant::now();
    let issued_a = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare a");
    let request_a = execute_request(issued_a.execution_token);
    let hash_a = canonical_reboot_payload_hash(&request_a);
    let outcome_a = fixture
        .executor
        .execute(&KEY, "conn-1", &request_a, &hash_a, now);
    assert!(matches!(outcome_a, ExecuteOutcome::OsErrorPersisted(_)));
    assert!(
        !fixture.executor.device_occupied(),
        "终态落盘后必须释放设备占用"
    );

    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    let issued_b = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_b, now)
        .expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    let hash_b = canonical_reboot_payload_hash(&execute_b);
    let outcome_b = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_b, &hash_b, now);
    assert!(
        matches!(outcome_b, ExecuteOutcome::OsErrorPersisted(_)),
        "终态写入后新 sub_task 可重新占用, got {outcome_b:?}"
    );
    assert_eq!(fixture.executor.os().call_count(), 2);
}

#[test]
fn host_reboot_boundary_is_unsupported_and_never_physically_reboots() {
    // 类型层面：reboot() 必须固定无参
    let _arity: fn(&HostRebootOs) -> Result<(), std::io::Error> = RebootOs::reboot;
    // 恒 Unsupported：即便被误调用也只得到明确错误，绝不重启开发机
    let boundary = HostRebootOs::new();
    let err = RebootOs::reboot(&boundary).expect_err("本切片绝不实现真实主机重启");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    // 真实主机重启入口从未被触达：自动化测试中宿主物理 reboot 恒 0
    assert_eq!(
        boundary.call_count(),
        0,
        "Unsupported 拒绝不是真实重启：宿主物理重启计数必须恒 0"
    );
}

#[test]
fn new_recovering_restores_occupation_for_pending_attempted_same_boot() {
    // TR-R6-3 恢复占用：已持久 Attempted（pre_boot == 当前 boot，重启已发起但 boot transition
    // 未观察到）在 agent 重建后必须恢复占用，另一 sub_task 持有效票据仍须 1005 DeviceBusy，
    // 新实例 fake OS 计数恒 0——重建绝不绕过未决任务互斥再次触发重启。
    let fixture = fixture_with(FakeOs::ok(), BOOT_OLD);
    let now = Instant::now();
    let issued_a = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare a");
    let request_a = execute_request(issued_a.execution_token);
    let hash_a = canonical_reboot_payload_hash(&request_a);
    let outcome_a = fixture
        .executor
        .execute(&KEY, "conn-1", &request_a, &hash_a, now);
    assert!(matches!(outcome_a, ExecuteOutcome::Attempted(_)));

    // agent 重建：同一持久 journal、同 boot 代际，经 new_recovering 恢复
    let rebuilt = RebootExecutor::new_recovering(
        Arc::clone(&fixture.journal),
        FakeOs::ok(),
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-2",
    );
    assert!(
        rebuilt.device_occupied(),
        "同 boot 未决 Attempted 在重建后必须恢复占用"
    );
    assert_eq!(
        rebuilt.occupying_sub_task().as_deref(),
        Some(SUB_TASK),
        "恢复占用必须记录占用任务"
    );

    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    let issued_b = rebuilt
        .prepare(&KEY, "conn-1", &prepare_b, now)
        .expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    let hash_b = canonical_reboot_payload_hash(&execute_b);
    assert!(
        matches!(
            rebuilt.execute(&KEY, "conn-1", &execute_b, &hash_b, now),
            ExecuteOutcome::Rejected(BusinessStatus::DeviceBusy)
        ),
        "恢复占用后新任务必须被互斥阻断"
    );
    assert_eq!(
        rebuilt.os().call_count(),
        0,
        "恢复占用期间新实例绝不允许触达 OS 边界"
    );
}

#[test]
fn new_recovering_after_boot_transition_starts_unoccupied() {
    // 未决 Attempted 的 pre_boot = BOOT_OLD；重建时 boot 已变为 BOOT_NEW
    // （boot transition 已发生，重启已被代际更替终结）→ 不恢复占用。
    let fixture = fixture_with(FakeOs::ok(), BOOT_OLD);
    let now = Instant::now();
    let issued_a = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare a");
    let request_a = execute_request(issued_a.execution_token);
    let hash_a = canonical_reboot_payload_hash(&request_a);
    let outcome_a = fixture
        .executor
        .execute(&KEY, "conn-1", &request_a, &hash_a, now);
    assert!(matches!(outcome_a, ExecuteOutcome::Attempted(_)));

    let rebuilt = RebootExecutor::new_recovering(
        Arc::clone(&fixture.journal),
        FakeOs::ok(),
        TicketManager::new(),
        Box::new(|| BOOT_NEW.to_string()),
        "agent-2",
    );
    assert!(
        !rebuilt.device_occupied(),
        "boot transition 已观察到的未决记录不得恢复占用"
    );
    assert_eq!(rebuilt.occupying_sub_task(), None);

    // 新任务在 BOOT_NEW 上正常占用并进入 OS 边界
    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    prepare_b.expected_boot_id = BOOT_NEW.to_string();
    let issued_b = rebuilt
        .prepare(&KEY, "conn-1", &prepare_b, now)
        .expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    execute_b.expected_boot_id = BOOT_NEW.to_string();
    let hash_b = canonical_reboot_payload_hash(&execute_b);
    let outcome_b = rebuilt.execute(&KEY, "conn-1", &execute_b, &hash_b, now);
    assert!(
        matches!(outcome_b, ExecuteOutcome::Attempted(_)),
        "boot transition 后新任务应可正常占用, got {outcome_b:?}"
    );
    assert_eq!(rebuilt.os().call_count(), 1);
}

#[test]
fn external_terminal_state_releases_occupation_for_next_sub_task() {
    // 占用任务经外部可靠写入终态 Failed（如恢复流程 / 中控核实路径）后，
    // 同一 executor 必须能观察并解除占用，让新任务重新占用并进入 OS 边界。
    let fixture = fixture_with(FakeOs::ok(), BOOT_OLD);
    let now = Instant::now();
    let issued_a = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare a");
    let request_a = execute_request(issued_a.execution_token);
    let hash_a = canonical_reboot_payload_hash(&request_a);
    let outcome_a = fixture
        .executor
        .execute(&KEY, "conn-1", &request_a, &hash_a, now);
    assert!(matches!(outcome_a, ExecuteOutcome::Attempted(_)));
    assert!(fixture.executor.device_occupied());

    // 外部经同一 JournalStore 可靠写入终态 Failed
    fixture
        .journal
        .finish(&KEY, SUB_TASK, TaskState::Failed, Some("OS_ERROR"))
        .expect("external finish");

    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    let issued_b = fixture
        .executor
        .prepare(&KEY, "conn-1", &prepare_b, now)
        .expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    let hash_b = canonical_reboot_payload_hash(&execute_b);
    let outcome_b = fixture
        .executor
        .execute(&KEY, "conn-1", &execute_b, &hash_b, now);
    assert!(
        matches!(outcome_b, ExecuteOutcome::Attempted(_)),
        "占用任务终态收敛后新任务必须可重新占用, got {outcome_b:?}"
    );
    assert_eq!(fixture.executor.os().call_count(), 2);
    assert!(fixture.executor.device_occupied());
    assert_eq!(
        fixture.executor.occupying_sub_task().as_deref(),
        Some(SUB_TASK_B),
        "释放旧占用后占用者必须更新为新任务"
    );
}

#[test]
fn boot_transition_releases_stale_occupation_on_same_executor() {
    // 占用后设备 boot 代际变化（boot transition）：新任务被阻断前复核发现
    // 占用记录的 boot 与当前 boot 不同 → 自动安全释放占用。
    let dir = TempDir::new("boot-change-release");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("open journal"));
    let boot = Arc::new(Mutex::new(BOOT_OLD.to_string()));
    let boot_for_closure = Arc::clone(&boot);
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        FakeOs::ok(),
        TicketManager::new(),
        Box::new(move || boot_for_closure.lock().unwrap().clone()),
        "agent-1",
    );
    let now = Instant::now();
    let issued_a = executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare a");
    let request_a = execute_request(issued_a.execution_token);
    let hash_a = canonical_reboot_payload_hash(&request_a);
    let outcome_a = executor.execute(&KEY, "conn-1", &request_a, &hash_a, now);
    assert!(matches!(outcome_a, ExecuteOutcome::Attempted(_)));
    assert!(executor.device_occupied());

    // boot 代际变化：未决重启已被 boot transition 终结
    *boot.lock().unwrap() = BOOT_NEW.to_string();

    let mut prepare_b = prepare_request();
    prepare_b.sub_task_id = SUB_TASK_B.to_string();
    prepare_b.expected_boot_id = BOOT_NEW.to_string();
    let issued_b = executor
        .prepare(&KEY, "conn-1", &prepare_b, now)
        .expect("prepare b");
    let mut execute_b = execute_request(issued_b.execution_token);
    execute_b.sub_task_id = SUB_TASK_B.to_string();
    execute_b.expected_boot_id = BOOT_NEW.to_string();
    let hash_b = canonical_reboot_payload_hash(&execute_b);
    let outcome_b = executor.execute(&KEY, "conn-1", &execute_b, &hash_b, now);
    assert!(
        matches!(outcome_b, ExecuteOutcome::Attempted(_)),
        "boot transition 后占用必须自动解除, got {outcome_b:?}"
    );
    assert_eq!(executor.os().call_count(), 2);
}

// ---- 落盘故障注入替身：把失败精确投到 execute 的某个写 stage ----
// `activate_fault` 之后写 stage 按提交次序映射：
// 第 1 次提交 = insert_accepted，第 2 次 = mark_attempted，第 3 次 = finish。
// 单次提交内的文件操作索引：0 = create_write（rename 前 → DefinitelyNotCommitted）、
// 2 = rename（rename 调用本身 → CommitUnknown）、3 = sync_dir（rename 后父目录 sync → CommitUnknown）。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    InsertAccepted,
    MarkAttempted,
    Finish,
}

const FAULT_CREATE_WRITE: usize = 0;
const FAULT_RENAME: usize = 2;
const FAULT_SYNC_DIR: usize = 3;

struct FaultState {
    fault: Option<(Stage, usize)>,
    commit_no: usize,
}

#[derive(Clone)]
struct FaultFs {
    state: Arc<Mutex<FaultState>>,
}

fn stage_for_commit(commit_no: usize) -> Stage {
    match commit_no {
        1 => Stage::InsertAccepted,
        2 => Stage::MarkAttempted,
        _ => Stage::Finish,
    }
}

impl FaultFs {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(FaultState {
                fault: None,
                commit_no: 0,
            })),
        }
    }

    /// 仅在 `JournalStore::open` 成功后调用：激活针对 stage 的一次性故障并重置提交次序
    /// （open 自身的 meta 落盘不计入注入计数）。
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
            return Err(std::io::Error::other("injected failure at create_write"));
        }
        Ok(())
    }

    /// 提交内文件操作索引（sync_file / rename / sync_dir）。
    fn on_commit_op(&self, op: usize) -> std::io::Result<()> {
        let mut st = self.state.lock().unwrap();
        let stage = stage_for_commit(st.commit_no);
        if matches!(st.fault, Some((s, i)) if s == stage && i == op) {
            st.fault = None;
            return Err(std::io::Error::other(format!(
                "injected failure at commit op {op}"
            )));
        }
        Ok(())
    }
}

impl Fs for FaultFs {
    fn create_dir_all(&self, dir: &Path) -> std::io::Result<()> {
        RealFs.create_dir_all(dir)
    }
    fn create_write(&self, path: &Path) -> std::io::Result<Box<dyn std::io::Write + Send>> {
        self.on_create_write()?;
        RealFs.create_write(path)
    }
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        RealFs.read(path)
    }
    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        self.on_commit_op(1)?;
        RealFs.sync_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.on_commit_op(FAULT_RENAME)?;
        RealFs.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        RealFs.remove_file(path)
    }
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        // 提交点父目录 sync 为索引 3；提交后清理用的 sync_dir 不参与注入（故障一次性）
        self.on_commit_op(FAULT_SYNC_DIR)?;
        RealFs.sync_dir(dir)
    }
}

fn faulty_fixture<O: RebootOs>(
    tag: &str,
    os: O,
    stage: Stage,
    op: usize,
) -> (
    TempDir,
    Arc<JournalStore<FaultFs>>,
    RebootExecutor<O, FaultFs>,
) {
    let dir = TempDir::new(tag);
    let fs = FaultFs::new();
    let journal = Arc::new(JournalStore::open(dir.path(), fs.clone()).expect("open journal"));
    fs.activate_fault(stage, op);
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    );
    (dir, journal, executor)
}

#[test]
fn attempted_definitely_not_committed_answers_accepted_without_touching_os() {
    let (_dir, _journal, executor) = faulty_fixture(
        "accepted-only",
        FakeOs::ok(),
        Stage::MarkAttempted,
        FAULT_CREATE_WRITE,
    );
    let now = Instant::now();
    let issued = executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    let request = execute_request(issued.execution_token);
    let payload_hash = canonical_reboot_payload_hash(&request);

    let outcome = executor.execute(&KEY, "conn-1", &request, &payload_hash, now);
    match outcome {
        // accepted 已可靠落盘而 attempted 确未提交：绝不调用 OS，
        // 必须以 status=0 回复最后可靠持久的 accepted（不能返回非零错误暗示未接收）
        ExecuteOutcome::Accepted(record) => {
            assert_eq!(record.state, TaskState::Accepted);
            assert_eq!(record.pre_boot_id, BOOT_OLD);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert_eq!(
        executor.os().call_count(),
        0,
        "attempted 确未提交时 fake OS 计数必须为 0"
    );
}

#[test]
fn commit_unknown_before_os_boundary_is_indeterminate_with_zero_os_calls() {
    // (a) insert_accepted 提交结果不明（OS 边界前）
    {
        let (_dir, _journal, executor) = faulty_fixture(
            "cu-insert",
            FakeOs::ok(),
            Stage::InsertAccepted,
            FAULT_RENAME,
        );
        let now = Instant::now();
        let issued = executor
            .prepare(&KEY, "conn-1", &prepare_request(), now)
            .expect("prepare");
        let request = execute_request(issued.execution_token);
        let payload_hash = canonical_reboot_payload_hash(&request);
        assert_eq!(
            executor.execute(&KEY, "conn-1", &request, &payload_hash, now),
            ExecuteOutcome::Indeterminate,
            "insert_accepted CommitUnknown 必须断流为 Indeterminate（无业务回包）"
        );
        assert_eq!(
            executor.os().call_count(),
            0,
            "OS 边界前：fake OS 计数必须为 0"
        );
    }
    // (b) mark_attempted 提交结果不明（仍为 OS 边界前）
    {
        let (_dir, _journal, executor) = faulty_fixture(
            "cu-attempted",
            FakeOs::ok(),
            Stage::MarkAttempted,
            FAULT_RENAME,
        );
        let now = Instant::now();
        let issued = executor
            .prepare(&KEY, "conn-1", &prepare_request(), now)
            .expect("prepare");
        let request = execute_request(issued.execution_token);
        let payload_hash = canonical_reboot_payload_hash(&request);
        assert_eq!(
            executor.execute(&KEY, "conn-1", &request, &payload_hash, now),
            ExecuteOutcome::Indeterminate,
            "mark_attempted CommitUnknown 必须断流为 Indeterminate（无业务回包）"
        );
        assert_eq!(
            executor.os().call_count(),
            0,
            "attempted 未可靠持久化：fake OS 计数必须为 0"
        );
    }
}

#[test]
fn commit_unknown_at_finish_after_os_is_indeterminate_with_one_os_call() {
    let (_dir, _journal, executor) =
        faulty_fixture("cu-finish", FakeOs::failing(), Stage::Finish, FAULT_RENAME);
    let now = Instant::now();
    let issued = executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    let request = execute_request(issued.execution_token);
    let payload_hash = canonical_reboot_payload_hash(&request);

    let outcome = executor.execute(&KEY, "conn-1", &request, &payload_hash, now);
    assert_eq!(
        outcome,
        ExecuteOutcome::Indeterminate,
        "OS 边界后 finish CommitUnknown 必须断流：不得回答真实 failed/unknown，也不得回旧 attempted"
    );
    assert_eq!(
        executor.os().call_count(),
        1,
        "OS 边界后：fake 计数为 1（与宿主物理 reboot 恒 0 严格区分）"
    );
}

#[test]
fn finish_definitely_not_committed_after_os_is_indeterminate_with_one_os_call() {
    let (_dir, _journal, executor) = faulty_fixture(
        "dnc-finish",
        FakeOs::failing(),
        Stage::Finish,
        FAULT_CREATE_WRITE,
    );
    let now = Instant::now();
    let issued = executor
        .prepare(&KEY, "conn-1", &prepare_request(), now)
        .expect("prepare");
    let request = execute_request(issued.execution_token);
    let payload_hash = canonical_reboot_payload_hash(&request);

    let outcome = executor.execute(&KEY, "conn-1", &request, &payload_hash, now);
    // finish 确未提交 → Indeterminate 断流、无业务回包：
    // 不返回旧 attempted、不伪新 failed/unknown、不非零暗示未执行
    assert_eq!(outcome, ExecuteOutcome::Indeterminate);
    assert_eq!(
        executor.os().call_count(),
        1,
        "OS 边界后 fake 计数为 1，而宿主物理 reboot 恒 0"
    );
    assert!(
        executor.device_occupied(),
        "Failed 终态未落盘 ⇒ 未决记录仍排他，绝不释放占用"
    );
}

// ---------------------------------------------------------------------------
// 3. BoardAgent 类型化调度
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeSource;

impl ReadOnlySource for FakeSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT_OLD.to_string(),
            agent_version: "0.5.0".to_string(),
            supported_actions: vec!["device.reboot.prepare".to_string()],
            durable_task_journal: true,
            max_status_bytes: 65_536,
            journal_epoch: JOURNAL_EPOCH.to_string(),
        }
    }
    fn status(&self) -> DeviceStatus {
        DeviceStatus {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT_OLD.to_string(),
            agent_epoch: AGENT_EPOCH.to_string(),
            sample_seq: 1,
            sampled_unix_ms: 1_700_000_000_000,
            sampled_monotonic_ns: 42,
            clock_epoch: CLOCK_EPOCH.to_string(),
            metrics: vec![],
            clock_quality: "observed".to_string(),
        }
    }
    fn clock(&self) -> ClockSample {
        ClockSample {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT_OLD.to_string(),
            clock_epoch: CLOCK_EPOCH.to_string(),
            receive_unix_ms: 1,
            send_unix_ms: 2,
            receive_monotonic_ns: 10,
            send_monotonic_ns: 20,
            quality: "observed".to_string(),
        }
    }
}

struct JournalReader<F: Fs>(Arc<JournalStore<F>>);

impl<F: Fs> TaskJournalReader for JournalReader<F> {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<rsetup_protocol::wire::TaskRecord>, BusinessStatus> {
        self.0
            .get(controller_key, &query.sub_task_id)
            .map_err(|_| BusinessStatus::InternalError)
            .map(|record| record.map(|record| record.to_wire()))
    }
}

struct FixedEpoch;

impl JournalEpochSource for FixedEpoch {
    fn journal_epoch(&self) -> String {
        JOURNAL_EPOCH.to_string()
    }
}

struct DispatchFixture {
    _dir: TempDir,
    journal: Arc<JournalStore<RealFs>>,
    executor: Arc<RebootExecutor<RecordingOs, RealFs>>,
    agent: BoardAgent<FakeSource, JournalReader<RealFs>, FixedEpoch>,
}

fn dispatch_fixture() -> DispatchFixture {
    let dir = TempDir::new("dispatch");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("open journal"));
    let executor = Arc::new(RebootExecutor::new(
        Arc::clone(&journal),
        RecordingOs::new(),
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    ));
    let dispatcher: Arc<RebootExecutor<RecordingOs, RealFs>> = Arc::clone(&executor);
    let dispatcher: Arc<dyn RebootDispatcher> = dispatcher;
    let agent = BoardAgent::new(FakeSource, JournalReader(Arc::clone(&journal)), FixedEpoch)
        .with_reboot(dispatcher, "conn-1");
    DispatchFixture {
        _dir: dir,
        journal,
        executor,
        agent,
    }
}

fn peer(key: &[u8; 32]) -> PeerContext {
    PeerContext {
        authenticated_controller_key: *key,
    }
}

fn issue_ticket(agent: &BoardAgent<FakeSource, JournalReader<RealFs>, FixedEpoch>) -> RebootTicket {
    match agent.handle_typed_action(
        &peer(&KEY),
        "device.reboot.prepare",
        &BusinessPayload::RebootPrepare(prepare_request()),
    ) {
        Ok(BusinessResponse::RebootTicket(ticket)) => ticket,
        other => panic!("expected RebootTicket, got {other:?}"),
    }
}

#[test]
fn dispatch_reboot_prepare_then_execute_returns_persisted_record() {
    let fx = dispatch_fixture();
    let ticket = issue_ticket(&fx.agent);
    assert_eq!(ticket.valid_for_ms, 30_000);
    assert_eq!(ticket.boot_id, BOOT_OLD);
    assert_eq!(ticket.sub_task_id, SUB_TASK);
    assert_eq!(fx.executor.tickets().active_ticket_count(), 1);

    let execute = BusinessPayload::RebootExecute(execute_request(ticket.execution_token));
    match fx
        .agent
        .handle_typed_action(&peer(&KEY), "device.reboot.execute", &execute)
    {
        Ok(BusinessResponse::TaskRecord(record)) => {
            assert_eq!(record.sub_task_id, SUB_TASK);
            assert_eq!(record.state, "attempted");
            assert_eq!(record.pre_boot_id, BOOT_OLD);
        }
        other => panic!("expected persisted TaskRecord, got {other:?}"),
    }
    assert_eq!(fx.executor.os().call_count(), 1);

    // 同一条持久记录必须能经只读 device.task.get 复核
    let query = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
    });
    match fx
        .agent
        .handle_typed_action(&peer(&KEY), "device.task.get", &query)
    {
        Ok(BusinessResponse::TaskRecord(record)) => assert_eq!(record.state, "attempted"),
        other => panic!("expected persisted TaskRecord from task.get, got {other:?}"),
    }
}

#[test]
fn dispatch_reboot_execute_without_ticket_is_rejected_without_os_or_journal_write() {
    let fx = dispatch_fixture();
    let execute = BusinessPayload::RebootExecute(execute_request([9u8; 32]));
    assert_eq!(
        fx.agent
            .handle_typed_action(&peer(&KEY), "device.reboot.execute", &execute),
        Err(BusinessStatus::ExecutionTokenInvalid)
    );
    assert_eq!(fx.executor.os().call_count(), 0);
    assert_eq!(fx.journal.record_count(), 0);
}

#[test]
fn dispatch_reboot_ticket_is_bound_to_the_authenticated_controller() {
    let fx = dispatch_fixture();
    let ticket = issue_ticket(&fx.agent);
    // 另一个已认证 controller 拿着同一枚票据：必须拒绝且不消耗
    let execute = BusinessPayload::RebootExecute(execute_request(ticket.execution_token));
    assert_eq!(
        fx.agent
            .handle_typed_action(&peer(&OTHER_KEY), "device.reboot.execute", &execute),
        Err(BusinessStatus::ExecutionTokenInvalid)
    );
    assert_eq!(fx.executor.os().call_count(), 0);
    // 票据未被消耗：原 controller 仍可执行一次
    match fx
        .agent
        .handle_typed_action(&peer(&KEY), "device.reboot.execute", &execute)
    {
        Ok(BusinessResponse::TaskRecord(record)) => assert_eq!(record.state, "attempted"),
        other => panic!("expected persisted TaskRecord, got {other:?}"),
    }
    assert_eq!(fx.executor.os().call_count(), 1);
}

#[test]
fn board_agent_without_reboot_dispatcher_stays_capability_unavailable() {
    let dir = TempDir::new("no-dispatcher");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("open journal"));
    let agent = BoardAgent::new(FakeSource, JournalReader(journal), FixedEpoch);
    assert_eq!(
        agent.handle_typed_action(
            &peer(&KEY),
            "device.reboot.prepare",
            &BusinessPayload::RebootPrepare(prepare_request()),
        ),
        Err(BusinessStatus::CapabilityUnavailable)
    );
    assert_eq!(
        agent.handle_typed_action(
            &peer(&KEY),
            "device.reboot.execute",
            &BusinessPayload::RebootExecute(execute_request([9u8; 32])),
        ),
        Err(BusinessStatus::CapabilityUnavailable)
    );
}

#[test]
fn dispatch_wire_reboot_entry_fails_closed_without_approved_codec() {
    let fx = dispatch_fixture();
    // 无获批 codec 见证：fail-closed，绝不解析、绝不签发票据、绝不触达 OS
    assert_eq!(
        fx.agent
            .handle_wire(&NoCodec, &peer(&KEY), "device.reboot.prepare", b"bytes"),
        Err(BusinessStatus::UnsupportedVersion)
    );
    assert_eq!(
        fx.agent
            .handle_wire(&NoCodec, &peer(&KEY), "device.reboot.execute", b"bytes"),
        Err(BusinessStatus::UnsupportedVersion)
    );
    assert_eq!(fx.executor.tickets().active_ticket_count(), 0);
    assert_eq!(fx.executor.os().call_count(), 0);
    assert_eq!(fx.journal.record_count(), 0);
}

#[test]
fn dispatch_rejects_payload_kind_mismatch_before_touching_the_dispatcher() {
    let fx = dispatch_fixture();
    // 动作与载荷类型不匹配：InvalidArgument，且绝不签发票据
    assert_eq!(
        fx.agent.handle_typed_action(
            &peer(&KEY),
            "device.reboot.prepare",
            &BusinessPayload::RebootExecute(execute_request([9u8; 32])),
        ),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.executor.tickets().active_ticket_count(), 0);
    // 只读动作仍按 action 路由，不受 reboot 载荷影响
    assert!(matches!(
        fx.agent.handle_typed_action(
            &peer(&KEY),
            "device.capabilities.get",
            &BusinessPayload::Query(Query {
                schema_version: BUSINESS_SCHEMA_VERSION,
            }),
        ),
        Ok(BusinessResponse::Capabilities(_))
    ));
}

#[test]
fn dispatch_execute_with_uncertain_journal_has_no_business_reply_and_never_calls_os() {
    // 断流路径穿过 BoardAgent：insert_accepted CommitUnknown（OS 边界前）
    // → 无业务回包（Indeterminate），既不是成功也不是失败，fake OS 计数恒 0
    let dir = TempDir::new("dispatch-uncertain");
    let fs = FaultFs::new();
    let journal = Arc::new(JournalStore::open(dir.path(), fs.clone()).expect("open journal"));
    fs.activate_fault(Stage::InsertAccepted, FAULT_RENAME);
    let executor = Arc::new(RebootExecutor::new(
        Arc::clone(&journal),
        RecordingOs::new(),
        TicketManager::new(),
        Box::new(|| BOOT_OLD.to_string()),
        "agent-1",
    ));
    let dispatcher: Arc<RebootExecutor<RecordingOs, FaultFs>> = Arc::clone(&executor);
    let dispatcher: Arc<dyn RebootDispatcher> = dispatcher;
    let agent = BoardAgent::new(FakeSource, JournalReader(Arc::clone(&journal)), FixedEpoch)
        .with_reboot(dispatcher, "conn-1");

    let ticket = match agent.handle_typed_action(
        &peer(&KEY),
        "device.reboot.prepare",
        &BusinessPayload::RebootPrepare(prepare_request()),
    ) {
        Ok(BusinessResponse::RebootTicket(ticket)) => ticket,
        other => panic!("expected RebootTicket, got {other:?}"),
    };
    let execute = BusinessPayload::RebootExecute(execute_request(ticket.execution_token));
    assert_eq!(
        agent.handle_typed_action(&peer(&KEY), "device.reboot.execute", &execute),
        Ok(BusinessResponse::Indeterminate),
        "CommitUnknown 必须断流为无业务回包，绝不伪造 TaskRecord 或非零 status"
    );
    assert_eq!(
        executor.os().call_count(),
        0,
        "OS 边界前断流：fake OS 计数必须为 0"
    );
}

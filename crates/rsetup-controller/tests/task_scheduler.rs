//! `TaskScheduler::run_once` 编排行为测试（04 §5.1/§5.2）。
//!
//! 全部使用**合成 fixture**：真实现有 wire DTO（`RebootTicket`/`RebootExecute`/
//! `TaskRecord`）构造合法合成对象，**不**触真实网络/数据库/板端。
//!
//! 证明的核心行为：
//! - commit-before-execute：意图短事务 `Confirmed` **先于** `execute`；
//! - 两个并发 `run_once` 同候选不双发（fake store 在**同一进程内**以互斥锁 +
//!   内存比对模拟的提交竞争；不是持久 CAS 的证明，见下方诚实边界）；
//! - 存储侧 CAS 的**原子性本身**：10 个 OS 线程同时竞争同一候选，必须**恰好 1 个
//!   `Confirmed` + 9 个 `DefinitelyNotCommitted`**（只有检查与写入同处一个
//!   `MutexGuard` 才成立；旧的「锁外比对再无条件写入」在这条断言下必挂）；
//! - 全局 16 槽上限；同设备互斥；
//! - 锁/权限重验失败 → execute=0；
//! - 提交未知（CommitUnknown）→ execute=0 且本轮不重试；
//! - execute 超时/传输未知 → 不重发、只转核验；
//! - 取消/非 queued、时钟不可信 → 正确拒绝，execute=0。
//!
//! 时间流逝（单调时钟逐候选重取 + 票据剩余寿命动态重验）追加证明：
//! - 前序候选的 `prepare` 往返期间队列期限到期 → 后序候选不准入、不 prepare、不发送；
//! - `prepare` 往返消耗的正是票据寿命：票面充裕但折算后剩余不足最短值 → 不提交、
//!   不 execute、**归还预留槽**（以 2 槽 3 候选的配比区分「归还」与「泄漏」）；
//! - 提交短事务**之前**的门禁独立生效（「每次读时前进固定量」的时钟把 prepare 后
//!   与提交前两道门禁逐个逼出，不靠运气）；
//! - `Confirmed` **之后**才发现票据过期或时钟不可信 → **绝不**回退已持久化的
//!   `dispatching`（回退等于给并发周期留下可重发的窗口）、**绝不**发已过期的
//!   execute，只并入 `execute_unverified` 保留只读核验；
//! - `observe_deadline_ms` 自**提交阶段**重取的最新单调时刻起算，而非整轮起点。
//!
//! 安全切片 A 追加证明（票据/记录契约与绑定）：
//! - `prepare` 返回的票据必须先过现有 wire 校验（`validate_dto`），且 `sub_task_id`
//!   与 `boot_id` 必须绑定到**本**候选子任务与其 `expected_boot_id`，错绑/非法一律
//!   在持久意图之前丢弃（不提交、不 execute）；
//! - 实际发出的 `RebootExecute` 与持久 `TaskIntent` 必须指向同一任务与同一 boot；
//! - `execute` 返回记录三分类且互斥计数：非法/错绑记录（wire 校验、任务、boot、
//!   journal）计 `record_rejects`；合法但非派发确认态的终态（`succeeded`/`failed`/
//!   `unknown`）**不属非法记录**，不计 `record_rejects` 也不计 `execute_accepted`；
//!   两类都只并入 `execute_unverified` 转只读核验、不重发；
//! - 全部正向 fixture 为合法规范 UUIDv4 且构造即 `validate_dto` 通过。
//!
//! 失败与超时的**持久**收敛（04 §5.1/§5.2）追加证明：
//! - 排队期限已到期的候选被持久置 `expired` + `QUEUE_EXPIRED`，同点释放归属本代际的
//!   设备锁并回写主任务汇总，下一轮不再作为 `queued` 候选被读出（不再反复判定）；
//! - `prepare` **明确**失败被持久置 `failed` + 对应稳定原因码（板端可解析的稳定码原样
//!   采用、词表外按 `EXECUTION_REJECTED` 兜底），同样释放锁并回写汇总；
//! - 结果**未知**的失败（超时/传输）与**可重试**拒绝（设备忙/资源不足）**绝不**写终态，
//!   候选保持 `queued`、锁与主行汇总都不动；
//! - 终结写入 CAS 未命中或结果未知时，`expired_persisted`/`prepare_failed_persisted`
//!   均不增长，且**不**重复释放锁、**不**重复递增主任务汇总（内存计数绝不冒充已收敛）。
//!
//! 诚实边界：fake `SchedulerStore` 只在**同一进程内**用 `std::sync::Mutex` + 内存
//! 字段比对模拟提交互斥（内存 CAS），它证明的是**编排控制流**不双发**且该内存 CAS
//! 本身跨线程原子**，**不冒充**也**不能证明**跨进程持久 CAS 这一 exactly-once 强约束
//! ——后者要求真实数据库的条件更新/事务隔离，依赖后续真实 SQL 适配与生产
//! `BoardClient` driver（尚未接，见模块 doc 与报告）；全局并发槽的跨进程语义、
//! `Unknown`/代际/时钟可信性契约同样仍属未完成项。

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;

use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, Capabilities, ClockSample, DeviceStatus, RebootExecute, RebootTicket,
    TaskRecord, is_canonical_uuid_v4, validate_dto,
};
use uuid::Uuid;

use rsetup_controller::ControllerError;
use rsetup_controller::tasks::board_client::{BoardClient, BoardClientError, BoardFuture};
use rsetup_controller::tasks::model::{
    MainTaskOutcome, MainTaskState, ReasonCode, SubTaskState, TaskIntent,
};
use rsetup_controller::tasks::scheduler::{
    CommitVerdict, DispatchCandidate, DispatchIntentWrite, FinalizeVerdict, FinalizeWrite,
    MonoClock, OBSERVE_WINDOW_MS, RevalidateFailure, ScheduleSummary, SchedulerError,
    SchedulerPolicy, SchedulerStore, SystemMonoClock, TaskScheduler,
};
use rsetup_controller::tasks::service::{counts_json_of, main_task_convergence};

// ---- 合法规范 UUIDv4 fixture 基元（可复现，不依赖随机数）--------------------

/// 由稳定种子派生合法规范 UUIDv4：强制版本位 4 与 variant 位 10xx。
/// 所有 fixture 的身份字段都经它生成，再由 `validate_dto` 现场核验（见
/// [`fixtures_are_canonical_uuidv4_and_pass_wire_validation`]）。
fn v4(seed: u128) -> Uuid {
    let mut bytes = seed.to_be_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let id = Uuid::from_bytes(bytes);
    debug_assert!(is_canonical_uuid_v4(&id.to_string()));
    id
}

/// 由子任务 id 稳定派生该候选当前 boot（与子任务 id 不同、可复现的合法 UUIDv4）。
fn boot_of(sub_task_id: Uuid) -> Uuid {
    let mut bytes = *sub_task_id.as_bytes();
    bytes[0] ^= 0xa5;
    let id = Uuid::from_bytes(bytes);
    debug_assert!(is_canonical_uuid_v4(&id.to_string()));
    id
}

/// 板端 journal 纪元（合法 UUIDv4，非 nil）。
fn journal_epoch() -> Uuid {
    v4(0x1e7f_0000_0000_0001)
}

// ---- 单调时钟 fake ----------------------------------------------------------

/// 可手动推进的单调时钟 fake。
///
/// 三个注入能力专为"await 期间时间流逝"而设：
/// - [`FakeClock::advance`]：由被测方之外的 fake board/store 在 await 内部推进；
/// - [`FakeClock::set_trusted`]：模拟时钟可信性门禁中途失效；
/// - [`FakeClock::advance_on_each_read`]：让任意两次读时之间恒定前进，用于把
///   调度器的多道时钟门禁（prepare 后 / 提交前 / Confirmed 后）逐个逼出来。
#[derive(Default)]
struct FakeClock {
    now_ms: Mutex<i64>,
    trusted: Mutex<bool>,
    advance_on_each_read_ms: Mutex<i64>,
}

impl FakeClock {
    fn new(now_ms: i64, trusted: bool) -> Arc<Self> {
        Arc::new(Self {
            now_ms: Mutex::new(now_ms),
            trusted: Mutex::new(trusted),
            ..Default::default()
        })
    }
    /// 手动推进单调时间（模拟 `.await` 期间真实流逝的时间）。
    fn advance(&self, ms: i64) {
        *self.now_ms.lock().expect("未中毒") += ms;
    }
    /// 当前读数，**不**触发 `advance_on_each_read`（供测试断言观察窗基线）。
    fn peek(&self) -> i64 {
        *self.now_ms.lock().expect("未中毒")
    }
    fn set_trusted(&self, trusted: bool) {
        *self.trusted.lock().expect("未中毒") = trusted;
    }
    /// 之后每次读时都先前进 `ms`（读数之间的增量恒为 `ms`，与绝对起点无关）。
    fn advance_on_each_read(&self, ms: i64) {
        *self.advance_on_each_read_ms.lock().expect("未中毒") = ms;
    }
}

impl MonoClock for FakeClock {
    fn now_ms(&self) -> Option<i64> {
        if !*self.trusted.lock().expect("未中毒") {
            return None;
        }
        let mut now = self.now_ms.lock().expect("未中毒");
        let each = *self.advance_on_each_read_ms.lock().expect("未中毒");
        if each > 0 {
            *now += each;
        }
        Some(*now)
    }
}

// ---- 共享事件轨迹：证明 commit 严格先于 execute ----------------------------

#[derive(Clone, Default)]
struct Trace(Arc<Mutex<Vec<String>>>);

impl Trace {
    fn push(&self, event: impl Into<String>) {
        self.0.lock().expect("未中毒").push(event.into());
    }
    fn events(&self) -> Vec<String> {
        self.0.lock().expect("未中毒").clone()
    }
}

// ---- 调度存储 fake ---------------------------------------------------------
//
// 诚实边界：本 fake 的「CAS」是**同一进程内** `std::sync::Mutex` 保护的内存字段
// 比对，只用于逼出编排控制流的提交互斥；它不写数据库、不参与事务，**不能**证明
// 跨进程持久 CAS（真实条件更新 + 隔离级别）这一 exactly-once 强约束。
//
// 原子性要求：CAS 的「读当前行 + 重验 + 写入」必须在**同一个 `MutexGuard`** 内完成。
// 早期实现把重验放在锁外（clone 后释放锁再比对，然后重新加锁无条件写入），两个
// 并发调用者会同时读到 `Queued`、各自返回 `Confirmed`，即**双发**——那正是本切片
// 声称要防的失效模式，因此 [`FakeStore::cas_commit`] 必须是真原子 CAS，并由
// [`concurrent_commit_from_ten_threads_yields_exactly_one_confirmed`] 用 OS 线程并发证明。

/// 单行的**当前**可变状态（模拟并发：候选快照之后的撤权/取消/丢锁）。
#[derive(Clone)]
struct StoredRow {
    candidate: DispatchCandidate,
    /// 该子任务所属主任务：主任务汇总（`main_tasks` 的 state/outcome/counts_json）
    /// 回写的分组键，与生产同事务口径一致。
    main_task_id: Uuid,
    cur_state: SubTaskState,
    /// 终结写入落库的稳定原因码（`sub_tasks.reason_code`）。
    cur_reason: Option<ReasonCode>,
    cur_actor_active: bool,
    cur_has_reboot: bool,
    cur_admission_approved: bool,
    cur_lock_owned: bool,
    cur_lock_generation: u64,
    cur_authz_epoch: u64,
    /// 若 Some，则 `commit_dispatch_intent` 该子任务直接返回 CommitUnknown（不改状态）。
    inject_commit_unknown: bool,
    /// 令本行的终结写入返回 Unknown（模拟提交结果未知，不改状态）。
    inject_finalize_unknown: bool,
    committed_intent: Option<TaskIntent>,
}

/// `main_tasks` 的持久汇总投影：状态收敛、outcome、十态计数与 revision。
#[derive(Clone, Debug, PartialEq, Eq)]
struct MainSummary {
    revision: u64,
    state: MainTaskState,
    outcome: Option<MainTaskOutcome>,
    counts_json: String,
    /// 汇总被回写的次数：子任务每变更一次应恰好 +1。
    syncs: usize,
}

/// 子任务 → 主任务的稳定派生（每个子任务独占一个主任务，便于逐例断言汇总回写）。
fn main_of(sub_task_id: Uuid) -> Uuid {
    let mut bytes = *sub_task_id.as_bytes();
    bytes[0] ^= 0x3c;
    let id = Uuid::from_bytes(bytes);
    debug_assert!(is_canonical_uuid_v4(&id.to_string()));
    id
}

/// 十态频数（与生产 `tally`/`counts_json_of` 同口径）。
fn tally(states: &[SubTaskState]) -> Vec<(SubTaskState, u64)> {
    const ALL: [SubTaskState; 10] = [
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
    ALL.iter()
        .map(|state| {
            (
                *state,
                states.iter().filter(|s| **s == *state).count() as u64,
            )
        })
        .collect()
}

/// 与生产 `finalize_subtask` 第 6 步同口径：子任务变更**之后**、同一把锁之内重算主任务
/// 收敛态与十态计数，并递增 revision（模拟「同事务回写、同提交、同回滚」）。
///
/// 绝不留下「只动子任务、主行陈旧」的中间态：本函数是 fake 里唯一的主任务汇总写入口。
fn sync_main_locked(
    rows: &[StoredRow],
    mains: &mut HashMap<Uuid, MainSummary>,
    main_task_id: Uuid,
) {
    let states: Vec<SubTaskState> = rows
        .iter()
        .filter(|row| row.main_task_id == main_task_id)
        .map(|row| row.cur_state)
        .collect();
    let (state, outcome) = main_task_convergence(&states);
    let counts_json = counts_json_of(&tally(&states));
    let entry = mains.entry(main_task_id).or_insert_with(|| MainSummary {
        revision: 0,
        state,
        outcome,
        counts_json: counts_json.clone(),
        syncs: 0,
    });
    entry.revision += 1;
    entry.syncs += 1;
    entry.state = state;
    entry.outcome = outcome;
    entry.counts_json = counts_json;
}

#[derive(Default)]
struct FakeStore {
    rows: Mutex<Vec<StoredRow>>,
    list_calls: Mutex<usize>,
    commit_calls: Mutex<Vec<Uuid>>,
    /// 终结写入的调用顺序与裁决（含 CAS 未命中/未知），与状态写入同一 guard 内落定。
    finalize_calls: Mutex<Vec<Uuid>>,
    finalize_verdicts: Mutex<Vec<(Uuid, FinalizeVerdict)>>,
    /// `main_tasks` 的持久汇总（终结路径同事务回写的结果）。
    mains: Mutex<HashMap<Uuid, MainSummary>>,
    /// 设备锁释放事件：`(子任务, 被释放的锁代际)`。代际必须来自被终结的那一行。
    lock_releases: Mutex<Vec<(Uuid, u64)>>,
    /// 提交裁决计数：与状态写入同处一个 guard 内自增，跨线程不丢、不重复。
    verdicts: Mutex<Vec<CommitVerdict>>,
    /// 短事务期间的时钟注入（模拟提交 await 消耗票据剩余寿命）。
    clock: Mutex<Option<Arc<FakeClock>>>,
    commit_advance_ms: Mutex<i64>,
    commit_untrust_clock: Mutex<bool>,
    trace: Trace,
}

impl FakeStore {
    fn new(trace: Trace) -> Arc<Self> {
        Arc::new(Self {
            trace,
            ..Default::default()
        })
    }

    fn push_row(&self, row: StoredRow) {
        self.rows.lock().expect("未中毒").push(row);
    }

    fn commit_calls(&self) -> Vec<Uuid> {
        self.commit_calls.lock().expect("未中毒").clone()
    }

    fn committed_intent(&self, subtask_id: Uuid) -> Option<TaskIntent> {
        self.rows
            .lock()
            .expect("未中毒")
            .iter()
            .find(|r| r.candidate.subtask_id == subtask_id)
            .and_then(|r| r.committed_intent.clone())
    }

    /// 某个子任务在库中的**当前**状态（CAS 之后的真实落库结果）。
    fn state_of(&self, subtask_id: Uuid) -> Option<SubTaskState> {
        self.rows
            .lock()
            .expect("未中毒")
            .iter()
            .find(|r| r.candidate.subtask_id == subtask_id)
            .map(|r| r.cur_state)
    }

    /// 某个子任务的候选快照（供脱离 `run_once` 直接构造 CAS 入参）。
    fn candidate(&self, subtask_id: Uuid) -> Option<DispatchCandidate> {
        self.rows
            .lock()
            .expect("未中毒")
            .iter()
            .find(|r| r.candidate.subtask_id == subtask_id)
            .map(|r| r.candidate.clone())
    }

    /// 全部提交裁决（按发生顺序，供并发断言计数）。
    fn verdicts(&self) -> Vec<CommitVerdict> {
        self.verdicts.lock().expect("未中毒").clone()
    }

    fn confirmed_count(&self) -> usize {
        self.verdicts()
            .iter()
            .filter(|v| matches!(v, CommitVerdict::Confirmed))
            .count()
    }

    /// 指定原因的 `DefinitelyNotCommitted` 数量。
    fn denied_with_reason(&self, reason: RevalidateFailure) -> usize {
        self.verdicts
            .lock()
            .expect("未中毒")
            .iter()
            .fold(0usize, |acc, v| match v {
                CommitVerdict::DefinitelyNotCommitted { reason: r } if *r == reason => acc + 1,
                _ => acc,
            })
    }

    /// 某个子任务的**当前原因码**（`sub_tasks.reason_code`）。
    fn reason_of(&self, subtask_id: Uuid) -> Option<ReasonCode> {
        self.rows
            .lock()
            .expect("未中毒")
            .iter()
            .find(|r| r.candidate.subtask_id == subtask_id)
            .and_then(|r| r.cur_reason)
    }

    /// 某个子任务所属的主任务 id。
    fn main_task_of(&self, subtask_id: Uuid) -> Option<Uuid> {
        self.rows
            .lock()
            .expect("未中毒")
            .iter()
            .find(|r| r.candidate.subtask_id == subtask_id)
            .map(|r| r.main_task_id)
    }

    /// 主任务汇总的当前投影（从未回写过则 `None`）。
    fn main_summary(&self, main_task_id: Uuid) -> Option<MainSummary> {
        self.mains
            .lock()
            .expect("未中毒")
            .get(&main_task_id)
            .cloned()
    }

    /// 该子任务被释放的设备锁代际（未释放则 `None`）。
    fn lock_released_generation(&self, subtask_id: Uuid) -> Option<u64> {
        self.lock_releases
            .lock()
            .expect("未中毒")
            .iter()
            .rev()
            .find(|(id, _)| *id == subtask_id)
            .map(|(_, generation)| *generation)
    }

    fn finalize_calls(&self) -> Vec<Uuid> {
        self.finalize_calls.lock().expect("未中毒").clone()
    }

    /// 该子任务最近一次终结写入的裁决。
    fn finalize_verdict_of(&self, subtask_id: Uuid) -> Option<FinalizeVerdict> {
        self.finalize_verdicts
            .lock()
            .expect("未中毒")
            .iter()
            .rev()
            .find(|(id, _)| *id == subtask_id)
            .map(|(_, verdict)| *verdict)
    }

    /// 注入：该子任务的终结写入返回「结果未知」，且不改任何状态。
    fn inject_finalize_unknown(&self, subtask_id: Uuid) {
        if let Some(row) = self
            .rows
            .lock()
            .expect("未中毒")
            .iter_mut()
            .find(|r| r.candidate.subtask_id == subtask_id)
        {
            row.inject_finalize_unknown = true;
        }
    }

    /// 让短事务在返回前推进时钟 `ms`（模拟提交 await 期间票据寿命流逝）。
    fn advance_clock_in_commit(&self, ms: i64) {
        *self.commit_advance_ms.lock().expect("未中毒") = ms;
    }

    /// 让短事务在返回前把时钟置为不可信。
    fn untrust_clock_in_commit(&self) {
        *self.commit_untrust_clock.lock().expect("未中毒") = true;
    }

    fn attach_clock(&self, clock: &Arc<FakeClock>) {
        *self.clock.lock().expect("未中毒") = Some(clock.clone());
    }

    fn apply_commit_clock_hook(&self) {
        let clock = self.clock.lock().expect("未中毒").clone();
        let Some(clock) = clock else { return };
        let advance = *self.commit_advance_ms.lock().expect("未中毒");
        if advance > 0 {
            clock.advance(advance);
        }
        if *self.commit_untrust_clock.lock().expect("未中毒") {
            clock.set_trusted(false);
        }
    }

    /// **原子终结写入**：读当前行 → 重验（state CAS + 锁代际 CAS）→ 通过则原地写终态
    /// 与原因码、按归属与代际释放设备锁，并**在同一把锁内**回写主任务汇总。
    ///
    /// 与 [`FakeStore::cas_commit`] 同一纪律：检查与写入之间绝不释放锁，否则两个并发
    /// 周期都会看到 `queued`、各自「终结一次 + 释放一次锁 + 汇总回写一次」，主任务计数
    /// 被重复递增。CAS 未命中（并发已派发/取消/换锁代际）一律 `Skipped` 且**零副作用**
    /// ——包括不回写汇总；注入的 `Unknown` 同样零副作用。
    fn apply_finalize(&self, write: &FinalizeWrite) -> FinalizeVerdict {
        let subtask_id = write.subtask_id;
        let mut rows = self.rows.lock().expect("未中毒");
        self.finalize_calls.lock().expect("未中毒").push(subtask_id);
        let mut verdict = FinalizeVerdict::Skipped;
        let mut applied_main: Option<Uuid> = None;
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.candidate.subtask_id == subtask_id)
        {
            if row.inject_finalize_unknown {
                verdict = FinalizeVerdict::Unknown;
            } else if row.cur_state != write.expected_from_state
                || row.cur_lock_generation != write.expected_lock_generation
            {
                verdict = FinalizeVerdict::Skipped;
            } else {
                row.cur_state = write.target_state;
                row.cur_reason = Some(write.reason_code);
                // 与生产 DELETE 的 WHERE 同条件：只释放**归属本子任务本代际**的锁。
                if row.cur_lock_owned {
                    row.cur_lock_owned = false;
                    self.lock_releases
                        .lock()
                        .expect("未中毒")
                        .push((subtask_id, row.cur_lock_generation));
                }
                applied_main = Some(row.main_task_id);
                verdict = FinalizeVerdict::Applied;
            }
        }
        if let Some(main_task_id) = applied_main {
            let mut mains = self.mains.lock().expect("未中毒");
            sync_main_locked(&rows, &mut mains, main_task_id);
        }
        self.finalize_verdicts
            .lock()
            .expect("未中毒")
            .push((subtask_id, verdict));
        verdict
    }

    /// **真正的原子 CAS**：读当前行 → 重验（state/锁归属/authz_epoch/lock_generation）
    /// → 通过则**原地**置 `Dispatching` 并落意图，全部在同一个 `MutexGuard` 内完成。
    ///
    /// 检查与写入之间**绝不**释放锁：否则两个并发调用者都会看到 `Queued` 并各自
    /// 返回 `Confirmed`（双 Confirmed = 双发的根因）。重验失败一律 `DefinitelyNotCommitted`
    /// 且**不改**任何字段；`CommitUnknown` 是注入的"结果未知"，同样不改状态。
    fn cas_commit(&self, write: &DispatchIntentWrite) -> CommitVerdict {
        let subtask_id = write.intent.subtask_id;
        let mut rows = self.rows.lock().expect("未中毒");
        self.commit_calls.lock().expect("未中毒").push(subtask_id);
        let verdict = match rows
            .iter_mut()
            .find(|r| r.candidate.subtask_id == subtask_id)
        {
            None => CommitVerdict::DefinitelyNotCommitted {
                reason: RevalidateFailure::NotQueued,
            },
            Some(row) if row.inject_commit_unknown => CommitVerdict::CommitUnknown,
            Some(row)
                if row.cur_authz_epoch != write.expected_authz_epoch
                    || row.cur_lock_generation != write.expected_lock_generation =>
            {
                CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::StaleEpoch,
                }
            }
            Some(row)
                if !(row.cur_actor_active && row.cur_has_reboot && row.cur_admission_approved) =>
            {
                CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::AuthorizationRevoked,
                }
            }
            Some(row) if !row.cur_lock_owned => CommitVerdict::DefinitelyNotCommitted {
                reason: RevalidateFailure::DeviceLockLost,
            },
            Some(row) if row.cur_state != write.expected_from_state => {
                CommitVerdict::DefinitelyNotCommitted {
                    reason: RevalidateFailure::NotQueued,
                }
            }
            // 重验通过：同一个 guard 内原地写入，检查与写入不可分割。
            Some(row) => {
                row.cur_state = SubTaskState::Dispatching;
                row.committed_intent = Some(write.intent.clone());
                CommitVerdict::Confirmed
            }
        };
        // 计数也在同一 guard 内落定，杜绝「返回 Confirmed 但计数丢失」的假象。
        self.verdicts.lock().expect("未中毒").push(verdict);
        verdict
    }
}

impl SchedulerStore for FakeStore {
    fn list_candidates(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DispatchCandidate>, ControllerError>> + Send + '_>>
    {
        *self.list_calls.lock().expect("未中毒") += 1;
        // 只返回当前仍为 queued 的候选（非 queued 不进入调度候选集）。
        let cands: Vec<DispatchCandidate> = self
            .rows
            .lock()
            .expect("未中毒")
            .iter()
            .filter(|r| r.cur_state == SubTaskState::Queued)
            .map(|r| r.candidate.clone())
            .collect();
        Box::pin(async move { Ok(cands) })
    }

    fn commit_dispatch_intent(
        &self,
        write: &DispatchIntentWrite,
    ) -> Pin<Box<dyn Future<Output = Result<CommitVerdict, ControllerError>> + Send + '_>> {
        let subtask_id = write.intent.subtask_id;
        // CAS 全部发生在**同步段**（返回 future 之前），future 只负责把结果带出来，
        // 因此任何 poll 时机/是否 poll 都不影响原子性，也才能被 OS 线程并发直接观测。
        let verdict = self.cas_commit(write);
        let label = if matches!(verdict, CommitVerdict::Confirmed) {
            "commit"
        } else {
            "commit-not"
        };
        self.trace.push(format!("{label}:{subtask_id}"));
        Box::pin(async move {
            // 短事务的 await 同样消耗票据剩余寿命：在 future 内推进时钟，逼出
            // 调度器 Confirmed 之后、execute 之前的那道门禁。
            self.apply_commit_clock_hook();
            Ok(verdict)
        })
    }

    /// 终结写入同样在**同步段**完成（返回 future 之前），future 只带出结果：
    /// 「判—写—释放锁—回写汇总」的原子性不依赖 poll 时机。
    fn finalize_subtask(
        &self,
        write: &FinalizeWrite,
    ) -> Pin<Box<dyn Future<Output = Result<FinalizeVerdict, ControllerError>> + Send + '_>> {
        let subtask_id = write.subtask_id;
        let verdict = self.apply_finalize(write);
        self.trace
            .push(format!("finalize:{subtask_id}:{verdict:?}"));
        Box::pin(async move { Ok(verdict) })
    }
}

// ---- 板端 fake（6 方法，记账 prepare/execute 次序）-------------------------

#[derive(Clone)]
enum ExecOutcome {
    Ok,
    Timeout,
    Transport,
    Rejected,
}

/// `prepare` 票据的构造参数：默认对任意子任务都是**合法**票（规范 UUIDv4、非零
/// token、正 TTL），测试按需覆盖单个字段造非法/错绑票。
#[derive(Clone)]
struct TicketSpec {
    schema_version: u32,
    sub_override: Option<String>,
    boot_override: Option<String>,
    token: [u8; 32],
    valid_for_ms: u32,
}

impl TicketSpec {
    fn legal_for(_sub_task_id: Uuid) -> Self {
        Self {
            schema_version: BUSINESS_SCHEMA_VERSION,
            sub_override: None,
            boot_override: None,
            token: [0x5a; 32],
            valid_for_ms: 60_000,
        }
    }
    /// 造一张字段合法但绑到**别的**子任务的票（错绑，非非法）。
    fn misbound_to(mut self, other_sub_task_id: Uuid) -> Self {
        self.sub_override = Some(other_sub_task_id.to_string());
        self
    }
    /// 造一张 sub_task 对但 boot 不属于本候选的票（错绑，非非法）。
    fn with_boot(mut self, boot: Uuid) -> Self {
        self.boot_override = Some(boot.to_string());
        self
    }
    fn with_valid_for(mut self, ms: u32) -> Self {
        self.valid_for_ms = ms;
        self
    }
    fn with_token(mut self, token: [u8; 32]) -> Self {
        self.token = token;
        self
    }
    fn with_schema(mut self, v: u32) -> Self {
        self.schema_version = v;
        self
    }
    fn with_sub_text(mut self, sub: &str) -> Self {
        self.sub_override = Some(sub.to_string());
        self
    }
    fn build(&self, sub_task_id: Uuid) -> RebootTicket {
        RebootTicket {
            schema_version: self.schema_version,
            sub_task_id: self
                .sub_override
                .clone()
                .unwrap_or_else(|| sub_task_id.to_string()),
            boot_id: self
                .boot_override
                .clone()
                .unwrap_or_else(|| boot_of(sub_task_id).to_string()),
            execution_token: self.token,
            valid_for_ms: self.valid_for_ms,
        }
    }
}

#[derive(Clone)]
enum PrepareOutcome {
    Ok(TicketSpec),
    /// 板端**明确**失败（能力/持久 journal 不可用）：可持久收敛为 failed。
    Err,
    /// 结果**未知**的失败（超时）：绝不据此判 failed，候选必须保持 queued。
    ErrTimeout,
    /// 可重试的瞬时拒绝（设备忙）：同样不得写终态。
    ErrDeviceBusy,
    /// 板端明确拒绝，携带稳定原因码 `TIME_UNCERTAIN`（可解析成稳定词表）。
    ErrRejectedStable,
    /// 板端明确拒绝，携带**词表外**原因码：按执行前拒收兜底归因。
    ErrRejectedUnknownCode,
}

#[derive(Default)]
struct FakeBoard {
    prepare_calls: Mutex<Vec<Uuid>>,
    execute_calls: Mutex<Vec<Uuid>>,
    /// 实际发出的 execute 请求（按子任务记录，供绑定核验断言）。
    executed_requests: Mutex<Vec<(Uuid, RebootExecute)>>,
    prepare_outcomes: Mutex<HashMap<Uuid, PrepareOutcome>>,
    execute_outcomes: Mutex<HashMap<Uuid, ExecOutcome>>,
    /// 注入的 execute 返回记录（缺省 = 该子任务的合法记录）。
    record_overrides: Mutex<HashMap<Uuid, TaskRecord>>,
    default_ticket_valid_ms: Mutex<u32>,
    yield_in_prepare: Mutex<bool>,
    /// prepare 期间的时钟注入（模拟票据寿命在 await 中被消耗 / 时钟中途不可信）。
    clock: Mutex<Option<Arc<FakeClock>>>,
    prepare_advance_ms: Mutex<i64>,
    prepare_untrust_clock: Mutex<bool>,
    trace: Trace,
}

impl FakeBoard {
    fn new(trace: Trace) -> Arc<Self> {
        Arc::new(Self {
            default_ticket_valid_ms: Mutex::new(60_000),
            trace,
            ..Default::default()
        })
    }
    fn set_execute(&self, id: Uuid, outcome: ExecOutcome) {
        self.execute_outcomes
            .lock()
            .expect("未中毒")
            .insert(id, outcome);
    }
    /// 注入一张 `prepare` 返回的票（默认可用 [`TicketSpec::legal_for`] 改造）。
    fn set_ticket(&self, id: Uuid, spec: TicketSpec) {
        self.prepare_outcomes
            .lock()
            .expect("未中毒")
            .insert(id, PrepareOutcome::Ok(spec));
    }
    /// 注入 `execute` 返回的板端记录。
    fn set_record(&self, id: Uuid, record: TaskRecord) {
        self.record_overrides
            .lock()
            .expect("未中毒")
            .insert(id, record);
    }
    fn executed_request(&self, id: Uuid) -> Option<RebootExecute> {
        self.executed_requests
            .lock()
            .expect("未中毒")
            .iter()
            .rev()
            .find(|(sub, _)| *sub == id)
            .map(|(_, req)| req.clone())
    }
    fn set_prepare_err(&self, id: Uuid) {
        self.set_prepare_outcome(id, PrepareOutcome::Err);
    }
    /// 注入任意 `prepare` 失败形态（明确失败 / 结果未知 / 可重试拒绝）。
    fn set_prepare_outcome(&self, id: Uuid, outcome: PrepareOutcome) {
        self.prepare_outcomes
            .lock()
            .expect("未中毒")
            .insert(id, outcome);
    }
    fn set_ticket_valid(&self, ms: u32) {
        *self.default_ticket_valid_ms.lock().expect("未中毒") = ms;
    }
    fn enable_prepare_yield(&self) {
        *self.yield_in_prepare.lock().expect("未中毒") = true;
    }
    /// 让 `prepare` 在返回前推进时钟 `ms`（票据寿命在 await 中被消耗）。
    fn advance_clock_in_prepare(&self, ms: i64) {
        *self.prepare_advance_ms.lock().expect("未中毒") = ms;
    }
    /// 让 `prepare` 在返回前把时钟置为不可信。
    fn untrust_clock_in_prepare(&self) {
        *self.prepare_untrust_clock.lock().expect("未中毒") = true;
    }
    fn attach_clock(&self, clock: &Arc<FakeClock>) {
        *self.clock.lock().expect("未中毒") = Some(clock.clone());
    }
    fn apply_prepare_clock_hook(&self) {
        let clock = self.clock.lock().expect("未中毒").clone();
        let Some(clock) = clock else { return };
        let advance = *self.prepare_advance_ms.lock().expect("未中毒");
        if advance > 0 {
            clock.advance(advance);
        }
        if *self.prepare_untrust_clock.lock().expect("未中毒") {
            clock.set_trusted(false);
        }
    }
    fn prepare_calls(&self) -> Vec<Uuid> {
        self.prepare_calls.lock().expect("未中毒").clone()
    }
    fn execute_calls(&self) -> Vec<Uuid> {
        self.execute_calls.lock().expect("未中毒").clone()
    }
}

impl BoardClient for FakeBoard {
    fn capabilities<'a>(&'a self, _d: &'a [u8; 32]) -> BoardFuture<'a, Capabilities> {
        Box::pin(async { Err(BoardClientError::CapabilityUnavailable) })
    }
    fn status<'a>(&'a self, _d: &'a [u8; 32]) -> BoardFuture<'a, DeviceStatus> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
    fn clock<'a>(&'a self, _d: &'a [u8; 32]) -> BoardFuture<'a, ClockSample> {
        Box::pin(async { Err(BoardClientError::DeviceOffline) })
    }
    fn prepare<'a>(
        &'a self,
        _d: &'a [u8; 32],
        sub_task_id: Uuid,
        _expected_boot_id: &'a str,
    ) -> BoardFuture<'a, RebootTicket> {
        self.prepare_calls.lock().expect("未中毒").push(sub_task_id);
        let default_valid_ms = *self.default_ticket_valid_ms.lock().expect("未中毒");
        let outcome = self
            .prepare_outcomes
            .lock()
            .expect("未中毒")
            .get(&sub_task_id)
            .cloned()
            .unwrap_or_else(|| {
                PrepareOutcome::Ok(
                    TicketSpec::legal_for(sub_task_id).with_valid_for(default_valid_ms),
                )
            });
        let yield_now = *self.yield_in_prepare.lock().expect("未中毒");
        Box::pin(async move {
            if yield_now {
                tokio::task::yield_now().await;
            }
            // 票据在板端签发即开始倒计时：在 await 内部推进/失信时钟，才真实复现
            // 「prepare 返回时票据已被往返消耗」与「返回后时钟不再可信」两种情形。
            self.apply_prepare_clock_hook();
            match outcome {
                PrepareOutcome::Err => Err(BoardClientError::CapabilityUnavailable),
                PrepareOutcome::ErrTimeout => Err(BoardClientError::Timeout),
                PrepareOutcome::ErrDeviceBusy => Err(BoardClientError::DeviceBusy),
                PrepareOutcome::ErrRejectedStable => Err(BoardClientError::Rejected {
                    status: 409,
                    reason_code: Some("TIME_UNCERTAIN".into()),
                }),
                PrepareOutcome::ErrRejectedUnknownCode => Err(BoardClientError::Rejected {
                    status: 400,
                    reason_code: Some("BOARD_INTERNAL_FIZZLE".into()),
                }),
                PrepareOutcome::Ok(spec) => Ok(spec.build(sub_task_id)),
            }
        })
    }
    fn execute<'a>(
        &'a self,
        _d: &'a [u8; 32],
        ticket: &'a RebootTicket,
        request: &'a RebootExecute,
    ) -> BoardFuture<'a, TaskRecord> {
        let sub_task_id = match Uuid::parse_str(&request.sub_task_id) {
            Ok(id) => id,
            Err(_) => return Box::pin(async { Err(BoardClientError::ProtocolMismatch) }),
        };
        // 忠实契约：发送前核对 ticket.sub_task_id == request.sub_task_id。
        if ticket.sub_task_id != request.sub_task_id {
            return Box::pin(async { Err(BoardClientError::ProtocolMismatch) });
        }
        self.execute_calls.lock().expect("未中毒").push(sub_task_id);
        self.trace.push(format!("execute:{sub_task_id}"));
        self.executed_requests
            .lock()
            .expect("未中毒")
            .push((sub_task_id, request.clone()));
        let outcome = self
            .execute_outcomes
            .lock()
            .expect("未中毒")
            .get(&sub_task_id)
            .cloned()
            .unwrap_or(ExecOutcome::Ok);
        let injected = self
            .record_overrides
            .lock()
            .expect("未中毒")
            .get(&sub_task_id)
            .cloned();
        Box::pin(async move {
            match outcome {
                // 注入了记录就原样返回（含非法记录），以便断言调度器的核验行为。
                ExecOutcome::Ok => Ok(injected.unwrap_or_else(|| board_record(sub_task_id))),
                ExecOutcome::Timeout => Err(BoardClientError::Timeout),
                ExecOutcome::Transport => Err(BoardClientError::Transport),
                ExecOutcome::Rejected => Err(BoardClientError::Rejected {
                    status: 400,
                    reason_code: Some("EXECUTION_REJECTED".into()),
                }),
            }
        })
    }
    fn task_record<'a>(
        &'a self,
        _d: &'a [u8; 32],
        _s: Uuid,
    ) -> BoardFuture<'a, Option<TaskRecord>> {
        Box::pin(async { Ok(None) })
    }
}

// ---- 合成 fixture helper ---------------------------------------------------

/// 记录 fixture 的统一出口：构造即过现有 wire 校验，非法 fixture 直接失败。
fn legal_record(record: TaskRecord) -> TaskRecord {
    validate_dto(&record).expect("fixture 记录必须通过 wire 校验");
    record
}

/// 该子任务的**合法**板端记录：规范 UUIDv4 身份字段、非零 `record_version`、
/// 派发确认态 `state`，`pre_boot_id` 即候选的 `expected_boot_id`（板端 prepare 时的 boot）。
fn board_record(sub_task_id: Uuid) -> TaskRecord {
    legal_record(TaskRecord {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: sub_task_id.to_string(),
        journal_epoch: journal_epoch().to_string(),
        record_version: 1,
        state: "accepted".into(),
        pre_boot_id: boot_of(sub_task_id).to_string(),
        observed_boot_id: None,
        reason_code: None,
        updated_unix_ms: None,
        evidence_kind: None,
    })
}

/// 该子任务的**合法终态**板端记录：绑定字段（`sub_task_id`/`pre_boot_id`/
/// `journal_epoch`/`record_version`）与 [`board_record`] 完全一致（即本次派发的
/// 权威绑定），只有 `state` 换成 wire 词表内但**非派发确认态**的
/// `succeeded`/`failed`/`unknown`。`with_evidence` 为真时附带 `observed_boot_id`
/// + `evidence_kind`，即板端首次 `execute` 往返直接回带完整成功证据的合法信封。
fn terminal_record(
    sub_task_id: Uuid,
    state: &str,
    reason_code: Option<&str>,
    with_evidence: bool,
) -> TaskRecord {
    // 观察到的新 boot：与 pre_boot 不同，同样是合法规范 UUIDv4（只翻 version 之外的字节）。
    let mut observed_bytes = *boot_of(sub_task_id).as_bytes();
    observed_bytes[0] ^= 0x0f;
    let observed_boot = Uuid::from_bytes(observed_bytes);
    debug_assert!(is_canonical_uuid_v4(&observed_boot.to_string()));
    legal_record(TaskRecord {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: sub_task_id.to_string(),
        journal_epoch: journal_epoch().to_string(),
        record_version: 2,
        state: state.to_string(),
        pre_boot_id: boot_of(sub_task_id).to_string(),
        observed_boot_id: with_evidence.then(|| observed_boot.to_string()),
        reason_code: reason_code.map(str::to_string),
        updated_unix_ms: Some(1_700_000_000_000),
        evidence_kind: with_evidence.then(|| "boot_transition_observed".to_string()),
    })
}

/// 该子任务的**合法**票据（测试按需链式改造成错绑/非法票）。
fn legal_ticket(sub_task_id: Uuid) -> TicketSpec {
    let spec = TicketSpec::legal_for(sub_task_id);
    validate_dto(&spec.build(sub_task_id)).expect("fixture 票据必须通过 wire 校验");
    spec
}

/// 全准入的默认候选（当前状态=快照，一切匹配 → 会 Confirmed 并 execute）。
fn admissible_candidate(subtask_id: Uuid, device: [u8; 32], deadline: i64) -> StoredRow {
    let candidate = DispatchCandidate {
        subtask_id,
        device_id: device,
        state: SubTaskState::Queued,
        actor_active: true,
        has_reboot: true,
        admission_approved: true,
        control_flow_healthy: true,
        capability_supported: true,
        persistent_journal_ok: true,
        lock_owned: true,
        lock_generation: 1,
        expected_authz_epoch: 42,
        queue_deadline_mono_ms: deadline,
        expected_boot_id: boot_of(subtask_id).to_string(),
        controller_epoch: v4(0xc0),
        session_epoch: v4(0x5e0),
        stream_epoch: v4(0x570),
    };
    StoredRow {
        main_task_id: main_of(subtask_id),
        cur_state: SubTaskState::Queued,
        cur_reason: None,
        cur_actor_active: true,
        cur_has_reboot: true,
        cur_admission_approved: true,
        cur_lock_owned: true,
        cur_lock_generation: 1,
        cur_authz_epoch: 42,
        inject_commit_unknown: false,
        inject_finalize_unknown: false,
        committed_intent: None,
        candidate,
    }
}

fn build_scheduler(
    store: Arc<FakeStore>,
    board: Arc<FakeBoard>,
    clock: Arc<FakeClock>,
) -> TaskScheduler<FakeStore, FakeBoard, FakeClock> {
    TaskScheduler::new(store, board, clock, SchedulerPolicy::default(), v4(0xfeed))
}

// ============================================================================
// 行为测试
// ============================================================================

#[tokio::test]
async fn run_once_commits_intent_before_execute() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(1);
    store.push_row(admissible_candidate(id, [0x11; 32], 10_000));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary: ScheduleSummary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.committed, 1);
    assert_eq!(summary.executed, 1);
    assert_eq!(summary.execute_accepted, 1);
    // 意图确实落库（携带正确 subtask_id）
    let intent = store.committed_intent(id).expect("意图已持久化");
    assert_eq!(intent.subtask_id, id);
    // 关键顺序断言：事件轨迹中 commit 严格先于 execute
    let events = trace.events();
    let commit_pos = events
        .iter()
        .position(|e| e.starts_with(&format!("commit:{id}")))
        .expect("有 commit 事件");
    let exec_pos = events
        .iter()
        .position(|e| e.starts_with(&format!("execute:{id}")))
        .expect("有 execute 事件");
    assert!(commit_pos < exec_pos, "commit 必须先于 execute: {events:?}");
}

#[tokio::test]
async fn two_concurrent_run_once_do_not_double_send_same_candidate() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    // prepare 内 yield 让两个 run_once 交叠到 commit 前，逼出并发提交竞争。
    // 本用例证明的是编排控制流在同一进程内不双发（fake 的内存互斥提交），
    // 不构成跨进程持久 CAS 已成立的证据。
    board.enable_prepare_yield();
    let clock = FakeClock::new(1_000, true);
    let id = v4(7);
    store.push_row(admissible_candidate(id, [0x22; 32], 10_000));

    let a = build_scheduler(store.clone(), board.clone(), clock.clone());
    let b = build_scheduler(store.clone(), board.clone(), clock);
    let (sa, sb) = tokio::join!(a.run_once(0), b.run_once(0));
    let sa = sa.expect("run_once a 成功");
    let sb = sb.expect("run_once b 成功");

    // 跨两个周期：同一候选至多一次提交、至多一次执行。
    assert_eq!(sa.committed + sb.committed, 1, "恰一个 run_once 提交成功");
    assert_eq!(sa.executed + sb.executed, 1, "同候选绝不双发");
    assert_eq!(
        board.execute_calls().iter().filter(|x| **x == id).count(),
        1
    );
    // execute 事件在轨迹中只出现一次。
    assert_eq!(
        trace
            .events()
            .iter()
            .filter(|e| e.starts_with(&format!("execute:{id}")))
            .count(),
        1
    );
}

#[tokio::test]
async fn global_slot_cap_limits_dispatches_per_cycle() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    // 20 个不同设备、全部可准入；空闲槽 0 → 全局上限 16。
    for i in 0..20u128 {
        let id = v4(100 + i);
        let dev = [i as u8; 32];
        store.push_row(admissible_candidate(id, dev, 10_000));
    }

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.committed, 16, "全局并发槽硬上限 16");
    assert_eq!(summary.executed, 16);
    assert_eq!(summary.slot_limit_skips, 4, "20 - 16 个候选因无槽被跳过");
    assert_eq!(board.execute_calls().len(), 16);
}

#[tokio::test]
async fn active_slots_reduce_available_capacity() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    for i in 0..10u128 {
        let id = v4(200 + i);
        store.push_row(admissible_candidate(id, [i as u8; 32], 10_000));
    }

    // 已占 15 槽 → 本轮只剩 1 个空槽。
    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(15).await.expect("run_once 成功");
    assert_eq!(summary.committed, 1);
    assert_eq!(summary.executed, 1);
    assert_eq!(summary.slot_limit_skips, 9);
}

#[tokio::test]
async fn same_device_mutex_allows_single_dispatch_per_cycle() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let a = v4(301);
    let b = v4(302);
    let dev = [0xab; 32];
    // 同设备两个候选（防御性：即便快照都声称持锁，一轮也至多发一个）。
    store.push_row(admissible_candidate(a, dev, 10_000));
    store.push_row(admissible_candidate(b, dev, 10_000));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.committed, 1, "同设备一轮仅一次派发");
    assert_eq!(summary.device_mutex_skips, 1);
    assert_eq!(summary.executed, 1);
}

#[tokio::test]
async fn revoked_permission_revalidated_in_tx_yields_zero_execute() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(401);
    let mut row = admissible_candidate(id, [0x33; 32], 10_000);
    // 快照 has_reboot=true（准入通过），但提交时当前已撤权。
    row.cur_has_reboot = false;
    store.push_row(row);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.admitted, 1, "按快照准入通过");
    assert_eq!(summary.prepared, 1, "准入后才 prepare");
    assert_eq!(summary.committed, 0, "重验失败不提交");
    assert_eq!(summary.not_committed, 1);
    assert_eq!(summary.executed, 0, "重验失败绝不 execute");
    assert_eq!(board.execute_calls().len(), 0);
}

#[tokio::test]
async fn lost_device_lock_revalidated_in_tx_yields_zero_execute() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(402);
    let mut row = admissible_candidate(id, [0x34; 32], 10_000);
    row.cur_lock_owned = false; // 提交时锁已不属于本子任务
    store.push_row(row);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.committed, 0);
    assert_eq!(summary.executed, 0);
    assert_eq!(board.execute_calls().len(), 0);
}

#[tokio::test]
async fn commit_unknown_does_not_execute_or_retry() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(501);
    let mut row = admissible_candidate(id, [0x44; 32], 10_000);
    row.inject_commit_unknown = true; // 提交结果未知
    store.push_row(row);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.commit_unknown, 1);
    assert_eq!(summary.committed, 0);
    assert_eq!(
        summary.executed, 0,
        "提交未知绝不 execute（不能当作安全重试）"
    );
    // 本轮对同一候选只尝试一次提交，绝不重试。
    assert_eq!(store.commit_calls().iter().filter(|x| **x == id).count(), 1);
    assert_eq!(board.execute_calls().len(), 0);
}

#[tokio::test]
async fn execute_timeout_never_resends() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(601);
    store.push_row(admissible_candidate(id, [0x55; 32], 10_000));
    board.set_execute(id, ExecOutcome::Timeout);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.committed, 1);
    assert_eq!(summary.executed, 1, "至多一次 execute");
    assert_eq!(summary.execute_unverified, 1, "超时转只读核验");
    // 绝不重发：execute 调用恰一次。
    assert_eq!(
        board.execute_calls().iter().filter(|x| **x == id).count(),
        1
    );
}

#[tokio::test]
async fn execute_transport_error_never_resends_or_judges_success() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(602);
    store.push_row(admissible_candidate(id, [0x56; 32], 10_000));
    board.set_execute(id, ExecOutcome::Transport);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.executed, 1);
    assert_eq!(summary.execute_unverified, 1);
    assert_eq!(summary.execute_accepted, 0, "未知不判成功");
    assert_eq!(
        board.execute_calls().iter().filter(|x| **x == id).count(),
        1
    );
}

#[tokio::test]
async fn execute_explicit_rejection_never_resends() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(605);
    store.push_row(admissible_candidate(id, [0x59; 32], 10_000));
    board.set_execute(id, ExecOutcome::Rejected);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.committed, 1, "意图已提交");
    assert_eq!(summary.executed, 1, "至多一次 execute");
    assert_eq!(summary.execute_rejected, 1, "板端执行前明确拒绝");
    assert_eq!(summary.execute_accepted, 0);
    // 明确拒绝同样绝不重发。
    assert_eq!(
        board.execute_calls().iter().filter(|x| **x == id).count(),
        1
    );
}

#[tokio::test]
async fn prepare_failure_ends_before_execute() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(603);
    store.push_row(admissible_candidate(id, [0x57; 32], 10_000));
    board.set_prepare_err(id);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.prepared, 1);
    assert_eq!(summary.prepare_failed, 1);
    assert_eq!(summary.committed, 0, "prepare 失败不提交意图");
    assert_eq!(summary.executed, 0);
    assert_eq!(board.execute_calls().len(), 0);
}

#[tokio::test]
async fn expired_ticket_validity_blocks_execute() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(604);
    store.push_row(admissible_candidate(id, [0x58; 32], 10_000));
    // 票据有效窗 0ms → 不足最短有效期 → 不 execute（票据有效期守卫）。
    board.set_ticket_valid(0);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.prepared, 1);
    assert_eq!(summary.committed, 0);
    assert_eq!(summary.executed, 0);
}

#[tokio::test]
async fn non_queued_candidate_is_denied_without_send() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    // 快照状态为 Held 的候选（取消/held/非 queued）：准入直接 Deny。
    let mut row = admissible_candidate(v4(701), [0x66; 32], 10_000);
    row.candidate.state = SubTaskState::Held;
    row.cur_state = SubTaskState::Queued; // 让 list 能返回它，聚焦准入判定
    let id = row.candidate.subtask_id;
    store.push_row(row);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.candidates, 1);
    assert_eq!(summary.admitted, 0, "非 queued 状态不准入");
    assert_eq!(summary.prepared, 0);
    assert_eq!(summary.executed, 0);
    assert_eq!(store.commit_calls().iter().filter(|x| **x == id).count(), 0);
}

#[tokio::test]
async fn expired_queue_deadline_is_denied() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    // 当前单调时刻已超过 queue_deadline → TTL 复核不过 → TimeUncertain 拒绝。
    let clock = FakeClock::new(20_000, true);
    let id = v4(702);
    store.push_row(admissible_candidate(id, [0x67; 32], 10_000)); // deadline < now

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.admitted, 0);
    assert_eq!(summary.prepared, 0);
    assert_eq!(summary.executed, 0);
}

#[tokio::test]
async fn untrusted_clock_rejects_dispatch_entirely() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, false); // 不可信
    store.push_row(admissible_candidate(v4(801), [0x77; 32], 10_000));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert!(summary.clock_untrusted, "时钟不可信标志置位");
    assert_eq!(summary.prepared, 0);
    assert_eq!(summary.committed, 0);
    assert_eq!(summary.executed, 0, "时钟不可信绝不发送");
    // 完全短路：连候选都不读。
    assert_eq!(*store.list_calls.lock().expect("未中毒"), 0);
}

#[tokio::test]
async fn candidate_read_failure_fails_closed() {
    // 用一个 begin 恒失败的存储证明 fail-closed：读候选失败整体上抛、零发送。
    #[derive(Default)]
    struct BrokenStore;
    impl SchedulerStore for BrokenStore {
        fn list_candidates(
            &self,
        ) -> Pin<
            Box<dyn Future<Output = Result<Vec<DispatchCandidate>, ControllerError>> + Send + '_>,
        > {
            Box::pin(async { Err(ControllerError::Database(sqlx::Error::RowNotFound)) })
        }
        fn commit_dispatch_intent(
            &self,
            _w: &DispatchIntentWrite,
        ) -> Pin<Box<dyn Future<Output = Result<CommitVerdict, ControllerError>> + Send + '_>>
        {
            Box::pin(async { Ok(CommitVerdict::CommitUnknown) })
        }
        fn finalize_subtask(
            &self,
            _w: &FinalizeWrite,
        ) -> Pin<Box<dyn Future<Output = Result<FinalizeVerdict, ControllerError>> + Send + '_>>
        {
            Box::pin(async { Ok(FinalizeVerdict::Applied) })
        }
    }
    let trace = Trace::default();
    let store: Arc<BrokenStore> = Arc::new(BrokenStore);
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);

    let sched = TaskScheduler::new(
        store,
        board.clone(),
        clock,
        SchedulerPolicy::default(),
        v4(999),
    );
    let err = sched.run_once(0).await.expect_err("读候选失败必须上抛");
    assert!(matches!(err, SchedulerError::CandidateReadFailed(_)));
    assert_eq!(board.execute_calls().len(), 0);
    assert_eq!(board.prepare_calls().len(), 0);
}

#[test]
fn system_mono_clock_is_monotonic_and_trusted() {
    let clock = SystemMonoClock::new();
    let a = clock.now_ms().expect("可信");
    std::thread::sleep(std::time::Duration::from_millis(2));
    let b = clock.now_ms().expect("可信");
    assert!(b >= a, "单调不减");
}

// ============================================================================
// 安全切片 A：票据契约/绑定 + execute 记录核验
// ============================================================================

#[test]
fn fixtures_are_canonical_uuidv4_and_pass_wire_validation() {
    let id = v4(1);
    assert!(
        is_canonical_uuid_v4(&id.to_string()),
        "fixture 子任务 id 必须是规范小写 UUIDv4"
    );
    assert!(!id.is_nil());
    let boot = boot_of(id);
    assert!(is_canonical_uuid_v4(&boot.to_string()));
    assert_ne!(boot, id);
    let row = admissible_candidate(id, [0x11; 32], 10_000);
    assert_eq!(row.candidate.expected_boot_id, boot.to_string());
    validate_dto(&legal_ticket(id).build(id)).expect("票据 fixture 必须通过 wire 校验");
    validate_dto(&board_record(id)).expect("记录 fixture 必须通过 wire 校验");
}

/// 用单个候选跑一轮（可注入票据），返回持久化的意图。
async fn intent_for_ticket(subtask_id: Uuid, ticket: Option<TicketSpec>) -> Option<TaskIntent> {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    store.push_row(admissible_candidate(subtask_id, [0x99; 32], 10_000));
    if let Some(spec) = ticket {
        board.set_ticket(subtask_id, spec);
    }
    let sched = build_scheduler(store.clone(), board, clock);
    sched.run_once(0).await.expect("run_once 成功");
    store.committed_intent(subtask_id)
}

#[tokio::test]
async fn execute_request_matches_persisted_intent_task_and_boot() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(1);
    let row = admissible_candidate(id, [0x11; 32], 10_000);
    let expected_boot = row.candidate.expected_boot_id.clone();
    store.push_row(row);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    sched.run_once(0).await.expect("run_once 成功");

    // 实际发出的 execute 请求：合法 DTO、就是本候选子任务与其 boot。
    let request = board.executed_request(id).expect("execute 请求已发出");
    validate_dto(&request).expect("实际请求必须是合法 DTO");
    assert_eq!(request.schema_version, BUSINESS_SCHEMA_VERSION);
    assert_eq!(request.sub_task_id, id.to_string(), "不得派发到其他子任务");
    assert_eq!(
        request.expected_boot_id, expected_boot,
        "execute 必须绑定候选自身的 expected_boot_id"
    );
    // 持久意图与实际请求指向同一任务与同一 boot。
    let intent = store.committed_intent(id).expect("意图已持久化");
    assert_eq!(intent.subtask_id, id);
    assert_eq!(intent.pre_boot_id, request.expected_boot_id);
    assert_ne!(intent.ticket_digest, [0u8; 32], "票据摘要必须已计算");
    assert_ne!(intent.request_digest, [0u8; 32], "请求摘要必须已计算");
    assert_ne!(
        intent.ticket_digest, intent.request_digest,
        "两类摘要域不得混淆"
    );
}

#[tokio::test]
async fn ticket_digest_covers_real_ticket_fields() {
    let id = v4(2);
    let base = intent_for_ticket(id, None).await.expect("默认票应提交");
    // 换执行令牌：票据摘要与请求摘要都必须随之改变（覆盖真实 token）。
    let other_token = intent_for_ticket(id, Some(legal_ticket(id).with_token([0x5b; 32])))
        .await
        .expect("换令牌票仍合法");
    assert_ne!(base.ticket_digest, other_token.ticket_digest);
    assert_ne!(base.request_digest, other_token.request_digest);
    // 只换 valid_for_ms：属票据字段而非 execute 请求字段。
    let other_ttl = intent_for_ticket(id, Some(legal_ticket(id).with_valid_for(30_000)))
        .await
        .expect("换有效期票仍合法");
    assert_ne!(
        base.ticket_digest, other_ttl.ticket_digest,
        "票据摘要必须覆盖 valid_for_ms"
    );
    assert_eq!(
        base.request_digest, other_ttl.request_digest,
        "请求摘要不受票据 TTL 影响"
    );
}

#[tokio::test]
async fn misbound_ticket_sub_task_is_dropped_before_commit() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(3);
    store.push_row(admissible_candidate(id, [0xa1; 32], 10_000));
    // 字段完全合法、但 sub_task_id 属于另一个任务（也是合法 UUIDv4）的错绑票。
    board.set_ticket(id, legal_ticket(id).misbound_to(v4(4)));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.admitted, 1);
    assert_eq!(summary.prepared, 1);
    assert_eq!(summary.ticket_rejects, 1, "错绑票必须在提交前被丢弃");
    assert_eq!(summary.committed, 0);
    assert!(store.commit_calls().is_empty(), "绝不写持久意图");
    assert_eq!(summary.executed, 0, "错绑票绝不 execute");
    assert!(board.execute_calls().is_empty());
}

#[tokio::test]
async fn misbound_ticket_boot_is_dropped_before_commit() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(5);
    store.push_row(admissible_candidate(id, [0xa2; 32], 10_000));
    // 子任务对、boot 是别的合法 UUIDv4（不是候选的 expected_boot_id）。
    board.set_ticket(id, legal_ticket(id).with_boot(boot_of(v4(6))));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.prepared, 1);
    assert_eq!(summary.ticket_rejects, 1);
    assert_eq!(summary.committed, 0);
    assert!(store.commit_calls().is_empty());
    assert_eq!(summary.executed, 0);
}

#[tokio::test]
async fn invalid_ticket_fields_are_dropped_before_commit() {
    let id = v4(7);
    let cases: Vec<(&str, TicketSpec)> = vec![
        ("schema_version 非 1", legal_ticket(id).with_schema(999)),
        (
            "sub_task_id 非规范 UUIDv4",
            legal_ticket(id).with_sub_text(&Uuid::nil().to_string()),
        ),
        (
            "execution_token 全零",
            legal_ticket(id).with_token([0u8; 32]),
        ),
    ];
    for (label, spec) in cases {
        let trace = Trace::default();
        let store = FakeStore::new(trace.clone());
        let board = FakeBoard::new(trace.clone());
        let clock = FakeClock::new(1_000, true);
        store.push_row(admissible_candidate(id, [0xa3; 32], 10_000));
        board.set_ticket(id, spec);

        let sched = build_scheduler(store.clone(), board.clone(), clock);
        let summary = sched.run_once(0).await.expect("run_once 成功");
        assert_eq!(summary.prepared, 1, "{label}：仍应 prepare");
        assert_eq!(summary.ticket_rejects, 1, "{label}：应判非法票据");
        assert_eq!(summary.committed, 0, "{label}：绝不提交意图");
        assert!(store.commit_calls().is_empty(), "{label}：无 commit 调用");
        assert_eq!(summary.executed, 0, "{label}：绝不 execute");
        assert!(board.execute_calls().is_empty(), "{label}：板端零 execute");
    }
}

#[tokio::test]
async fn candidate_with_non_canonical_boot_is_not_prepared() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(8);
    let mut row = admissible_candidate(id, [0xa4; 32], 10_000);
    // 候选自身 boot 不是规范 UUIDv4 → prepare 请求本身就不合法，绝不发出。
    row.candidate.expected_boot_id = format!("boot-{id}");
    store.push_row(row);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.admitted, 1);
    assert_eq!(summary.prepared, 0, "候选 boot 非法时绝不 prepare");
    assert!(board.prepare_calls().is_empty());
    assert_eq!(summary.ticket_rejects, 1);
    assert_eq!(summary.committed, 0);
    assert_eq!(summary.executed, 0);
}

#[tokio::test]
async fn execute_record_for_other_task_is_not_accepted() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(9);
    store.push_row(admissible_candidate(id, [0xa5; 32], 10_000));
    // 合法 DTO、但属于另一个子任务的记录。
    board.set_record(
        id,
        TaskRecord {
            sub_task_id: v4(0xdead).to_string(),
            ..board_record(id)
        },
    );

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.committed, 1);
    assert_eq!(summary.executed, 1);
    assert_eq!(summary.execute_accepted, 0, "错任务记录不得记为 accepted");
    assert_eq!(summary.record_rejects, 1);
    assert_eq!(
        summary.execute_unverified, 1,
        "不可信记录转只读核验，不判成功"
    );
    assert_eq!(
        board.execute_calls().iter().filter(|x| **x == id).count(),
        1,
        "绝不重发"
    );
}

#[tokio::test]
async fn invalid_execute_records_are_not_accepted() {
    // 本用例只覆盖**非法/错绑记录**：DTO 不合法或绑定不属于本次派发 → `record_rejects`。
    // 合法但非派发确认态的终态记录（succeeded/failed/unknown）不属非法记录，
    // 见 `legal_terminal_records_are_unverified_not_rejected`。
    let id = v4(10);
    let record = board_record(id);
    let cases: Vec<(&str, TaskRecord)> = vec![
        (
            "schema_version 非 1",
            TaskRecord {
                schema_version: 999,
                ..record.clone()
            },
        ),
        (
            "state 非 wire 词表",
            TaskRecord {
                state: "accepted-by-board".into(),
                ..record.clone()
            },
        ),
        (
            "journal_epoch 非 UUIDv4",
            TaskRecord {
                journal_epoch: Uuid::nil().to_string(),
                ..record.clone()
            },
        ),
        (
            "record_version 为 0",
            TaskRecord {
                record_version: 0,
                ..record.clone()
            },
        ),
        (
            "pre_boot_id 非本候选 boot",
            TaskRecord {
                pre_boot_id: v4(0xbeef).to_string(),
                ..record
            },
        ),
    ];
    for (label, bad) in cases {
        let trace = Trace::default();
        let store = FakeStore::new(trace.clone());
        let board = FakeBoard::new(trace.clone());
        let clock = FakeClock::new(1_000, true);
        store.push_row(admissible_candidate(id, [0xa6; 32], 10_000));
        board.set_record(id, bad);

        let sched = build_scheduler(store.clone(), board.clone(), clock);
        let summary = sched.run_once(0).await.expect("run_once 成功");
        assert_eq!(summary.executed, 1, "{label}：已发出一次 execute");
        assert_eq!(summary.execute_accepted, 0, "{label}：不得记 accepted");
        assert_eq!(summary.record_rejects, 1, "{label}：应记非法记录数");
        assert_eq!(summary.execute_unverified, 1, "{label}：转只读核验");
        assert_eq!(
            board.execute_calls().iter().filter(|x| **x == id).count(),
            1,
            "{label}：绝不重发"
        );
    }
}

#[tokio::test]
async fn legal_terminal_records_are_unverified_not_rejected() {
    // 合法终态记录（`succeeded`/`failed`/`unknown`）：DTO 与任务/boot/journal 绑定
    // 全部合法，只是 `state` 不在派发确认态 {accepted, attempted} 内。
    // 三件事同时成立：不记 accepted（不是派发确认）、**不记 record_rejects**（不是
    // 非法记录）、只计 execute_unverified（转只读核验，由核验路径裁定）。
    // `attempted` 作为同 fixture 的对照，证明上面的分类不是无条件置零。
    let id = v4(12);
    let cases: Vec<(&str, TaskRecord)> = vec![
        (
            "failed/OS_ERROR（板端执行失败即刻回报）",
            terminal_record(id, "failed", Some("OS_ERROR"), false),
        ),
        (
            "succeeded 带 boot 迁移证据（首次往返即回终态）",
            terminal_record(id, "succeeded", None, true),
        ),
        (
            "unknown（板端结果未知）",
            terminal_record(id, "unknown", Some("RESULT_TIMEOUT"), false),
        ),
    ];
    for (label, record) in cases {
        // fixture 自身必须先过 wire 校验：它考察的是分类，不是合法性。
        validate_dto(&record).unwrap_or_else(|e| panic!("{label}：fixture 必须合法: {e}"));
        let trace = Trace::default();
        let store = FakeStore::new(trace.clone());
        let board = FakeBoard::new(trace.clone());
        let clock = FakeClock::new(1_000, true);
        store.push_row(admissible_candidate(id, [0xa8; 32], 10_000));
        board.set_record(id, record);

        let sched = build_scheduler(store.clone(), board.clone(), clock);
        let summary = sched.run_once(0).await.expect("run_once 成功");
        assert_eq!(summary.executed, 1, "{label}：已发出一次 execute");
        assert_eq!(
            summary.execute_accepted, 0,
            "{label}：非派发确认态不记 accepted"
        );
        assert_eq!(summary.record_rejects, 0, "{label}：合法记录不记非法数");
        assert_eq!(summary.execute_unverified, 1, "{label}：转只读核验");
        assert_eq!(summary.execute_rejected, 0, "{label}：不是板端明确拒绝");
        assert_eq!(
            board.execute_calls().iter().filter(|x| **x == id).count(),
            1,
            "{label}：绝不重发"
        );
    }

    // 同绑定、仅 state 换成派发确认态 → 必须正常记 accepted（对照组）。
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    store.push_row(admissible_candidate(id, [0xa8; 32], 10_000));
    board.set_record(id, terminal_record(id, "attempted", None, false));
    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.execute_accepted, 1, "attempted 仍属派发确认态");
    assert_eq!(summary.record_rejects, 0);
    assert_eq!(summary.execute_unverified, 0);
}

#[tokio::test]
async fn legal_execute_record_is_still_accepted() {
    // 反向对照：合法记录仍正常记 accepted，证明上述拒绝不是无条件置零。
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let id = v4(11);
    store.push_row(admissible_candidate(id, [0xa7; 32], 10_000));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.ticket_rejects, 0);
    assert_eq!(summary.record_rejects, 0);
    assert_eq!(summary.execute_accepted, 1, "合法记录正常记 accepted");
    assert_eq!(summary.execute_unverified, 0);
}

// ============================================================================
// 整改 P2：FakeStore 的提交 CAS 必须**真原子**（同一 MutexGuard 内重验 + 写入）
// ============================================================================

/// 把「副作用已在同步段完成」的 fake future 拉到底。
///
/// 本 fake 的 CAS 全部发生在 `commit_dispatch_intent` **返回 future 之前**，首次
/// poll 必为 `Ready`；有了它就能拿到每次调用的真实 [`CommitVerdict`]，而不必只从
/// 计数反推「有没有双 Confirmed」。
fn block_once<F: Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut fut = std::pin::pin!(fut);
    let mut cx = Context::from_waker(Waker::noop());
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("fake 边界的 future 必须首次 poll 即 Ready"),
    }
}

/// 不经 `run_once`，直接按候选快照组装 CAS 入参（压测存储原子性用）。
fn write_for_candidate(store: &FakeStore, subtask_id: Uuid) -> DispatchIntentWrite {
    let cand = store.candidate(subtask_id).expect("候选必须存在");
    DispatchIntentWrite {
        intent: TaskIntent {
            subtask_id,
            controller_epoch: v4(0xfeed),
            session_epoch: cand.session_epoch,
            stream_epoch: cand.stream_epoch,
            pre_boot_id: cand.expected_boot_id.clone(),
            ticket_digest: [0x11; 32],
            request_digest: [0x22; 32],
            observe_deadline_ms: 300_000,
        },
        expected_lock_generation: cand.lock_generation,
        expected_authz_epoch: cand.expected_authz_epoch,
        expected_from_state: SubTaskState::Queued,
    }
}

#[test]
fn concurrent_commit_from_ten_threads_yields_exactly_one_confirmed() {
    // 真·跨线程并发：10 个 OS 线程同时（同一 barrier 放行）对**同一**候选发起提交 CAS。
    // 早期实现「锁内 clone → 锁外比对 → 重新加锁无条件写入」会让多个线程都读到
    // queued 并各自返回 Confirmed —— 双 Confirmed 就是双发的根因，本用例把它钉死。
    //
    // 诚实边界：这证明的是 fake 的**同一进程内内存 CAS 具备跨线程原子性**（因此编排
    // 层的互斥假设在 fake 上成立），**不**构成跨进程持久 CAS（真实条件更新 + 事务
    // 隔离）已成立的证据；后者须由真实 SQL 适配 + 双进程测试证明。
    //
    // 用例灵敏度（整改时实测结论，非本文件常驻逻辑）：把 `cas_commit` 临时改回
    // 「锁内快照 → 锁外比对 → 重新加锁无条件写入」并撑开比对与写入之间的窗口，
    // 本用例即以 `left: 10, right: 1` 失败（10 个线程全部 Confirmed）；改回同一
    // `MutexGuard` 内的原子写法后稳定通过（连跑 40 次无抖动）。
    const RACERS: usize = 10;
    let store = FakeStore::new(Trace::default());
    let id = v4(0x5eed);
    store.push_row(admissible_candidate(id, [0x9c; 32], 10_000));
    // CAS 入参按 Arc 共享：10 个线程用**同一份**期望值竞争同一行。
    let write = Arc::new(write_for_candidate(&store, id));

    let gate = Arc::new(std::sync::Barrier::new(RACERS));
    let mut handles = Vec::with_capacity(RACERS);
    for _ in 0..RACERS {
        let store = store.clone();
        let gate = gate.clone();
        let write = write.clone();
        handles.push(std::thread::spawn(move || {
            gate.wait();
            block_once(store.commit_dispatch_intent(&write)).expect("内存 CAS 不产生连接级错误")
        }));
    }
    let verdicts: Vec<CommitVerdict> = handles
        .into_iter()
        .map(|h| h.join().expect("竞争线程不得 panic"))
        .collect();

    let confirmed = verdicts
        .iter()
        .filter(|v| matches!(v, CommitVerdict::Confirmed))
        .count();
    let denied = verdicts
        .iter()
        .filter(|v| matches!(v, CommitVerdict::DefinitelyNotCommitted { .. }))
        .count();
    assert_eq!(store.commit_calls().len(), RACERS, "10 次提交都到达存储");
    assert_eq!(confirmed, 1, "恰好 1 个 Confirmed：双 Confirmed 即双发");
    assert_eq!(denied, RACERS - 1, "其余必须确定未提交（不得出现 unknown）");
    assert!(
        !verdicts
            .iter()
            .any(|v| matches!(v, CommitVerdict::CommitUnknown)),
        "内存 CAS 不存在「结果未知」这一支"
    );
    // 败者的失败原因必须是「已非 queued」：胜出者在同一 guard 内已置 dispatching。
    assert_eq!(
        store.denied_with_reason(RevalidateFailure::NotQueued),
        RACERS - 1,
        "败者只能因 NotQueued 落败"
    );
    assert_eq!(
        store.state_of(id),
        Some(SubTaskState::Dispatching),
        "行状态恰被推进一次"
    );
    assert!(store.committed_intent(id).is_some(), "唯一胜出者落了意图");
    assert_eq!(store.confirmed_count(), 1, "存储侧计数与返回值一致");
}

// ============================================================================
// 整改 P1：单调时钟与票据剩余寿命的动态重验（时间只在 await 中流逝）
// ============================================================================

/// 前序候选的 `prepare` 往返期间时间流逝：只在整轮开头读一次时钟的实现，会把
/// 已过排队期限的后序候选继续准入并发出去。
#[tokio::test]
async fn queue_deadline_expires_for_later_candidates_during_await() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    board.advance_clock_in_prepare(15_000); // 一次 prepare 往返吃掉 15s
    // 两个候选共享同一 deadline；第一个的 prepare 把单调时间推过该期限。
    let first = v4(1);
    let second = v4(2);
    store.push_row(admissible_candidate(first, [0xb1; 32], 10_000));
    store.push_row(admissible_candidate(second, [0xb2; 32], 10_000));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.candidates, 2);
    assert_eq!(
        summary.admitted, 1,
        "第二个候选的队列期限已在 await 期间到期，不得准入"
    );
    assert_eq!(summary.prepared, 1, "只对准入的候选 prepare");
    assert!(
        !board.prepare_calls().contains(&second),
        "过期候选绝不 prepare"
    );
    assert!(store.commit_calls().iter().all(|x| *x != second));
    assert!(
        !board.execute_calls().contains(&second),
        "过期候选绝不 execute"
    );
    // 第一个候选自身不受影响（其期限在自身准入时刻仍有效）。
    assert!(board.execute_calls().contains(&first));
    assert_eq!(summary.committed, 1);
    assert_eq!(summary.executed, 1);
}

/// `prepare` 往返消耗的正是票据寿命：票面 60s、往返吃掉 50s → 只剩 10s，低于最短
/// 剩余值即**不得**提交短事务、**不得** execute，且必须**释放预留槽**。
#[tokio::test]
async fn ticket_lifetime_consumed_by_prepare_blocks_commit_and_releases_slot() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    board.advance_clock_in_prepare(50_000);
    board.set_ticket_valid(60_000); // 票面完全合法、充裕
    let ids: Vec<Uuid> = (0..3u128).map(|i| v4(900 + i)).collect();
    for (i, id) in ids.iter().enumerate() {
        store.push_row(admissible_candidate(*id, [0xc0 + i as u8; 32], 10_000_000));
    }

    // 只有 2 个空槽、3 个候选：过期路径若漏掉 `free_slots += 1`，槽位会被逐个吃掉，
    // 结果必是 prepared=2 + slot_limit_skips=1；只有每个候选都归还预留槽，3 个才
    // 都能被 prepare（本用例即以此区分「释放」与「泄漏」）。
    let policy = SchedulerPolicy {
        global_slots: 2,
        min_ticket_validity_ms: 20_000,
    };
    let sched = TaskScheduler::new(store.clone(), board.clone(), clock, policy, v4(0xfeed));
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.admitted, 3, "队列期限都还有效");
    assert_eq!(
        summary.prepared, 3,
        "剩余寿命不足必须归还预留槽，否则第 3 个候选轮不到 prepare"
    );
    assert_eq!(summary.slot_limit_skips, 0, "槽位从未泄漏");
    assert_eq!(summary.ticket_expired, 3, "剩余 10s < 最短 20s");
    assert_eq!(summary.committed, 0, "绝不提交短事务");
    assert!(store.commit_calls().is_empty(), "无任何持久副作用");
    assert_eq!(summary.executed, 0);
    assert!(board.execute_calls().is_empty());
    for id in &ids {
        assert_eq!(
            store.state_of(*id),
            Some(SubTaskState::Queued),
            "未提交的候选保持 queued"
        );
    }
}

/// 对照 + 观察窗基线：往返只吃掉 1s 时票据仍可用，且 `observe_deadline_ms` 必须
/// 自**提交阶段**的最新单调时刻起算（用周期起点等于凭空延长观察窗）。
#[tokio::test]
async fn fresh_observe_window_is_anchored_on_commit_stage_clock() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    board.advance_clock_in_prepare(1_000);
    board.set_ticket_valid(60_000);
    let id = v4(901);
    store.push_row(admissible_candidate(id, [0xc8; 32], 10_000_000));

    let policy = SchedulerPolicy {
        global_slots: 16,
        min_ticket_validity_ms: 20_000,
    };
    let sched = TaskScheduler::new(
        store.clone(),
        board.clone(),
        clock.clone(),
        policy,
        v4(0xfeed),
    );
    let summary = sched.run_once(0).await.expect("run_once 成功");
    assert_eq!(summary.ticket_expired, 0, "剩余 59s ≥ 20s：仍可用");
    assert_eq!(summary.committed, 1);
    assert_eq!(summary.execute_accepted, 1);

    // 提交阶段的读数即 prepare 往返之后的时钟；短事务未推进时钟。
    let commit_stage_now = clock.peek();
    assert_eq!(commit_stage_now, 2_000, "prepare 往返已消耗 1s");
    let intent = store.committed_intent(id).expect("意图已持久化");
    assert_eq!(
        intent.observe_deadline_ms,
        commit_stage_now.saturating_add(OBSERVE_WINDOW_MS),
        "观察窗必须自提交阶段的最新单调时刻起算"
    );
    assert!(
        intent.observe_deadline_ms > 1_000 + OBSERVE_WINDOW_MS,
        "以周期起点起算会漏掉 await 已消耗的时间"
    );
}

/// 用「每次读时前进 25s」的时钟把提交前那道门禁单独逼出来：prepare 返回后仍剩
/// 35s（可用），到短事务前只剩 10s（不可用）——证明提交前门禁不是重复检查。
#[tokio::test]
async fn ticket_expired_before_commit_blocks_short_transaction() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(0, true);
    clock.advance_on_each_read(25_000);
    board.set_ticket_valid(60_000);
    for i in 0..2u128 {
        store.push_row(admissible_candidate(
            v4(910 + i),
            [0xd0 + i as u8; 32],
            10_000_000,
        ));
    }

    let policy = SchedulerPolicy {
        global_slots: 16,
        min_ticket_validity_ms: 20_000,
    };
    let sched = TaskScheduler::new(store.clone(), board.clone(), clock, policy, v4(0xfeed));
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.prepared, 2, "两个候选都走到 prepare 之后");
    assert_eq!(
        summary.ticket_expired, 2,
        "提交前剩余仅 10s < 20s：提交前门禁独立生效"
    );
    assert_eq!(summary.committed, 0, "绝不提交短事务");
    assert!(store.commit_calls().is_empty());
    assert_eq!(summary.executed, 0);
    assert_eq!(summary.dispatched_not_executed, 0, "尚未提交，不属该计数");
}

/// **已提交**才发现票据过期：意图已持久化为 `dispatching`，此时既不得回退为
/// `queued`（回退会让并发周期的 CAS 误判可重发 → 双发），也绝不发已过期的
/// `execute`；只能保留 dispatching 并转只读核验。
#[tokio::test]
async fn ticket_expired_after_commit_never_executes_and_never_reverts() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    store.attach_clock(&clock);
    store.advance_clock_in_commit(59_000); // 短事务 await 吃掉剩余寿命
    board.set_ticket_valid(60_000);
    let id = v4(930);
    store.push_row(admissible_candidate(id, [0xe1; 32], 10_000_000));

    let policy = SchedulerPolicy {
        global_slots: 16,
        min_ticket_validity_ms: 20_000,
    };
    let sched = TaskScheduler::new(store.clone(), board.clone(), clock, policy, v4(0xfeed));
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.committed, 1, "短事务确已 Confirmed");
    assert_eq!(summary.dispatched_not_executed, 1, "已派发但未发 execute");
    assert_eq!(summary.execute_unverified, 1, "只转只读核验");
    assert_eq!(summary.executed, 0, "已过期的 execute 绝不发出");
    assert_eq!(summary.execute_accepted, 0, "更不得判成功");
    assert!(board.execute_calls().is_empty(), "板端零 execute");
    // 关键：已持久化的 dispatching **不得**回退。
    assert_eq!(
        store.state_of(id),
        Some(SubTaskState::Dispatching),
        "已提交意图绝不回退 queued"
    );
    assert!(
        store.committed_intent(id).is_some(),
        "已落库意图不得被抹掉（核验路径要用）"
    );
    assert_eq!(
        store.commit_calls().iter().filter(|x| **x == id).count(),
        1,
        "绝不重发提交"
    );
}

/// 同上，但失败原因是**时钟此刻不可信**（短事务期间失去可信时间源）：同样不得
/// 回退已提交的 dispatching，也不得在无法证明票据有效时发送。
#[tokio::test]
async fn clock_untrusted_after_commit_keeps_dispatching_and_never_executes() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    store.attach_clock(&clock);
    store.untrust_clock_in_commit();
    let id = v4(931);
    store.push_row(admissible_candidate(id, [0xe2; 32], 10_000_000));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.committed, 1);
    assert!(summary.clock_untrusted, "本轮因时钟不可信而中止");
    assert_eq!(summary.dispatched_not_executed, 1);
    assert_eq!(summary.execute_unverified, 1);
    assert_eq!(summary.executed, 0);
    assert!(board.execute_calls().is_empty());
    assert_eq!(
        store.state_of(id),
        Some(SubTaskState::Dispatching),
        "时钟不可信同样绝不回退已提交的 dispatching"
    );
}

/// `prepare` 返回后时钟不可信：无从证明票据仍有效 → 不提交、不 execute、中止本轮。
#[tokio::test]
async fn clock_untrusted_after_prepare_aborts_cycle_without_commit() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    board.untrust_clock_in_prepare();
    let id = v4(932);
    store.push_row(admissible_candidate(id, [0xe3; 32], 10_000_000));

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.prepared, 1, "prepare 已发出（票据在往返中取得）");
    assert!(
        summary.clock_untrusted,
        "返回后时钟不可信必须置位并中止本轮"
    );
    assert_eq!(summary.committed, 0, "无法证明票据有效即绝不提交");
    assert!(store.commit_calls().is_empty());
    assert_eq!(summary.executed, 0);
    assert_eq!(
        store.state_of(id),
        Some(SubTaskState::Queued),
        "保持 queued 留待下轮"
    );
}

// ---------------------------------------------------------------------------
// P1 回归：`min_ticket_validity_ms = 0` 也**绝不**放行寿命耗尽（remaining == 0）的票据
// ---------------------------------------------------------------------------

/// 策略把最短剩余值配成 `0`（只表示"不再额外加严"）**不等于**"票面寿命耗尽仍可用"。
///
/// 失效模式（本用例钉死）：过期判定若写成纯 `remaining < min`，在 `min == 0` 时
/// `0 < 0 == false`，寿命已耗尽（`remaining == 0`）的票据会被判"未过期"，调度器于是
/// 提交短事务并向板端发出**一张已被其自身倒计时否定的** `execute`。正确口径是剩余
/// 寿命必须**严格为正**：`remaining == 0 || remaining < min` 任一成立即过期。
///
/// 本例走**提交之前**那道门禁：prepare 往返把 60s 票面完全烧掉 → 不提交、不 execute、
/// 计入 `ticket_expired`，并保持 `queued`。另配 2 槽 3 候选，钉死过期路径归还预留槽。
#[tokio::test]
async fn exhausted_ticket_lifetime_blocks_commit_even_when_min_validity_is_zero() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    board.advance_clock_in_prepare(70_001); // 票面 60s 全部烧掉，剩余夹到 0
    board.set_ticket_valid(60_000); // 票面本身合法、充裕
    let ids: Vec<Uuid> = (0..3u128).map(|i| v4(940 + i)).collect();
    for (i, id) in ids.iter().enumerate() {
        store.push_row(admissible_candidate(*id, [0x60 + i as u8; 32], 10_000_000));
    }

    let policy = SchedulerPolicy {
        global_slots: 2,
        min_ticket_validity_ms: 0, // 策略放宽到 0：有效性下界仍必须是严格正剩余
    };
    let sched = TaskScheduler::new(store.clone(), board.clone(), clock, policy, v4(0xfeed));
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.admitted, 3);
    assert_eq!(
        summary.prepared, 3,
        "剩余寿命耗尽必须归还预留槽，否则第 3 个候选轮不到 prepare"
    );
    assert_eq!(summary.slot_limit_skips, 0, "槽位从未泄漏");
    assert_eq!(
        summary.ticket_expired, 3,
        "remaining == 0 无论策略如何配置都是过期"
    );
    assert_eq!(summary.committed, 0, "绝不提交短事务");
    assert!(store.commit_calls().is_empty(), "无任何持久副作用");
    assert_eq!(summary.executed, 0, "绝不为耗尽的票据发 execute");
    assert!(board.execute_calls().is_empty(), "板端零 execute");
    assert_eq!(summary.dispatched_not_executed, 0, "尚未提交，不属该计数");
    for id in &ids {
        assert_eq!(
            store.state_of(*id),
            Some(SubTaskState::Queued),
            "未提交的候选保持 queued"
        );
    }
}

/// 同一策略放宽下的**提交之后**门禁：短事务 await 烧掉 60s 票面 → `Confirmed` 之后
/// 才发现票据已被其倒计时否定。此时意图已持久化为 `dispatching`：**绝不**发已过期的
/// `execute`、**绝不**回退已提交意图（回退等于给并发周期留下可重发的窗口），只保留
/// `dispatching` 并转只读核验。
#[tokio::test]
async fn exhausted_ticket_lifetime_after_commit_never_executes_when_min_validity_is_zero() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    board.attach_clock(&clock);
    store.attach_clock(&clock);
    store.advance_clock_in_commit(70_001); // 短事务 await 把票面寿命烧光
    board.set_ticket_valid(60_000);
    let id = v4(950);
    store.push_row(admissible_candidate(id, [0x6a; 32], 10_000_000));

    let policy = SchedulerPolicy {
        global_slots: 16,
        min_ticket_validity_ms: 0,
    };
    let sched = TaskScheduler::new(store.clone(), board.clone(), clock, policy, v4(0xfeed));
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(
        summary.committed, 1,
        "短事务确已 Confirmed（提交前剩余仍 > 0）"
    );
    assert_eq!(summary.dispatched_not_executed, 1, "已派发但绝不发 execute");
    assert_eq!(summary.execute_unverified, 1, "只转只读核验");
    assert_eq!(summary.executed, 0, "已过耗尽的 execute 绝不发出");
    assert_eq!(summary.execute_accepted, 0, "更不得判成功");
    assert!(board.execute_calls().is_empty(), "板端零 execute");
    assert_eq!(
        store.state_of(id),
        Some(SubTaskState::Dispatching),
        "已提交意图绝不回退 queued"
    );
    assert!(
        store.committed_intent(id).is_some(),
        "已落库意图不得被抹掉（核验路径要用）"
    );
    assert_eq!(
        store.commit_calls().iter().filter(|x| **x == id).count(),
        1,
        "绝不重发提交"
    );
}

// ===========================================================================
// 失败与超时的**持久**收敛（04 §5.1/§5.2：终态不止于内存计数）
//
// 上面各例只断言「本轮不发」；本节的断言下推到**库里**：排队期限到期的候选必须落
// `expired` + `QUEUE_EXPIRED`，`prepare` 明确失败必须落 `failed` + 对应稳定原因码，
// 两者都要释放归属本代际的设备锁并同点回写主任务汇总。反过来，结果未知的失败
// （超时/传输）与可重试拒绝**绝不**写终态，终结写入 CAS 未命中或结果未知时也**绝不**
// 把内存计数当成"已收敛"。
// ===========================================================================

#[tokio::test]
async fn expired_queue_deadline_persists_expired_and_releases_lock() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    // 当前单调时刻已超过 queue_deadline：期限只会被单调时钟跨过，不会倒流回来。
    let clock = FakeClock::new(20_000, true);
    let row = admissible_candidate(v4(702), [0x67; 32], 10_000);
    let id = row.candidate.subtask_id;
    store.push_row(row);
    let main_id = store.main_task_of(id).expect("候选属于某个主任务");

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.admitted, 0, "过期候选不准入");
    assert_eq!(summary.prepared, 0, "过期候选绝不 prepare");
    assert_eq!(summary.executed, 0);
    // 关键：不只是本轮跳过，而是**持久**收敛。
    assert_eq!(summary.expired_persisted, 1, "expired 已落库");
    assert_eq!(store.state_of(id), Some(SubTaskState::Expired));
    assert_eq!(
        store.reason_of(id),
        Some(ReasonCode::QueueExpired),
        "原因码必须是 QUEUE_EXPIRED"
    );
    assert_eq!(
        store.lock_released_generation(id),
        Some(1),
        "释放的必须是被终结那一代的设备锁"
    );
    assert!(
        store.candidate(id).expect("候选快照仍在").lock_owned,
        "只改库中当前状态，绝不改写候选快照"
    );
    assert!(
        store.commit_calls().is_empty(),
        "终结不经过 commit_dispatch_intent"
    );

    // 主任务汇总与子任务变更同点收敛（RT-R8-3）：仅 expired 的批次收敛为 completed/failed。
    let main = store.main_summary(main_id).expect("主任务汇总已回写");
    assert_eq!(main.state, MainTaskState::Completed);
    assert_eq!(main.outcome, Some(MainTaskOutcome::Failed));
    assert_eq!(main.syncs, 1, "一次子任务变更对应一次汇总回写");
}

/// 持久收敛的实际效果：下一轮不再把它当 queued 候选读出，也不重复终结/重复释放锁。
#[tokio::test]
async fn persisted_expired_candidate_is_not_offered_again() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(20_000, true);
    let row = admissible_candidate(v4(703), [0x68; 32], 10_000);
    let id = row.candidate.subtask_id;
    store.push_row(row);
    let main_id = store.main_task_of(id).expect("主任务");

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let first = sched.run_once(0).await.expect("第一轮成功");
    let second = sched.run_once(0).await.expect("第二轮成功");

    assert_eq!(first.candidates, 1);
    assert_eq!(first.expired_persisted, 1);
    assert_eq!(
        second.candidates, 0,
        "库里已 expired，不再作为 queued 候选读出"
    );
    assert_eq!(second.expired_persisted, 0);
    assert_eq!(store.finalize_calls().len(), 1, "不重复终结、不重复释放锁");
    assert_eq!(
        store.main_summary(main_id).expect("汇总").syncs,
        1,
        "主任务汇总不被重复递增"
    );
}

/// `prepare` 明确失败：终态 `failed` + `CAPABILITY_UNAVAILABLE` 落库、释放设备锁、
/// 回写主任务汇总。另配 2 槽 3 候选，钉死失败路径归还预留槽。
#[tokio::test]
async fn prepare_rejection_persists_failed_releases_lock_and_syncs_main() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(1_000, true);
    let ids: Vec<Uuid> = (0..3u128).map(|i| v4(960 + i)).collect();
    for (i, id) in ids.iter().enumerate() {
        store.push_row(admissible_candidate(*id, [0xd0 + i as u8; 32], 10_000_000));
        board.set_prepare_err(*id);
    }

    let policy = SchedulerPolicy {
        global_slots: 2,
        min_ticket_validity_ms: 1,
    };
    let sched = TaskScheduler::new(store.clone(), board.clone(), clock, policy, v4(0xfeed));
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.admitted, 3);
    assert_eq!(summary.prepared, 3, "prepare 失败必须归还预留槽");
    assert_eq!(summary.slot_limit_skips, 0, "槽位从未泄漏");
    assert_eq!(summary.prepare_failed, 3);
    assert_eq!(summary.prepare_failed_persisted, 3, "三笔失败全部落库");
    assert_eq!(summary.executed, 0);
    assert!(board.execute_calls().is_empty(), "板端零 execute");
    assert!(store.commit_calls().is_empty(), "无 dispatching 意图落库");

    for id in &ids {
        assert_eq!(store.state_of(*id), Some(SubTaskState::Failed));
        assert_eq!(
            store.reason_of(*id),
            Some(ReasonCode::CapabilityUnavailable),
            "prepare 能力拒绝归因 CAPABILITY_UNAVAILABLE"
        );
        assert_eq!(store.lock_released_generation(*id), Some(1), "设备锁已释放");
        let main_id = store.main_task_of(*id).expect("主任务");
        let main = store.main_summary(main_id).expect("汇总已回写");
        assert_eq!(main.state, MainTaskState::Completed);
        assert_eq!(main.outcome, Some(MainTaskOutcome::Failed));
        assert_eq!(main.syncs, 1);
    }
}

/// 结果**未知**（超时）与**可重试**（设备忙）的 prepare 失败绝不写终态：票据可能已
/// 签发、锁仍可能被下一轮用到，据它们判 failed 会把可恢复的任务误判成失败。
#[tokio::test]
async fn prepare_result_unknown_never_persists_failure() {
    for outcome in [PrepareOutcome::ErrTimeout, PrepareOutcome::ErrDeviceBusy] {
        let trace = Trace::default();
        let store = FakeStore::new(trace.clone());
        let board = FakeBoard::new(trace.clone());
        let clock = FakeClock::new(1_000, true);
        let row = admissible_candidate(v4(970), [0xe1; 32], 10_000_000);
        let id = row.candidate.subtask_id;
        store.push_row(row);
        let main_id = store.main_task_of(id).expect("主任务");
        board.set_prepare_outcome(id, outcome.clone());

        let sched = build_scheduler(store.clone(), board.clone(), clock);
        let summary = sched.run_once(0).await.expect("run_once 成功");

        assert_eq!(summary.prepare_failed, 1, "内存计数仍记录本次失败");
        assert_eq!(summary.prepare_failed_persisted, 0, "结果未知绝不写终态");
        assert!(store.finalize_calls().is_empty(), "零持久终结调用");
        assert_eq!(
            store.state_of(id),
            Some(SubTaskState::Queued),
            "保持 queued 留待下轮"
        );
        assert_eq!(store.reason_of(id), None);
        assert_eq!(
            store.lock_released_generation(id),
            None,
            "绝不释放仍可能被用到的锁"
        );
        assert!(store.main_summary(main_id).is_none(), "主行汇总保持原样");
    }
}

/// 板端明确拒绝时的原因码归因：能解析成 04 §3 稳定词表的用板端归因，词表外按执行前
/// 拒收（`EXECUTION_REJECTED`）兜底——绝不写一个库里不存在的稳定码。
#[tokio::test]
async fn prepare_rejection_maps_stable_and_unknown_reason_codes() {
    let cases = [
        (PrepareOutcome::ErrRejectedStable, ReasonCode::TimeUncertain),
        (
            PrepareOutcome::ErrRejectedUnknownCode,
            ReasonCode::ExecutionRejected,
        ),
    ];
    for (index, (outcome, expected)) in cases.into_iter().enumerate() {
        let trace = Trace::default();
        let store = FakeStore::new(trace.clone());
        let board = FakeBoard::new(trace.clone());
        let clock = FakeClock::new(1_000, true);
        let row = admissible_candidate(v4(980 + index as u128), [0xe5; 32], 10_000_000);
        let id = row.candidate.subtask_id;
        store.push_row(row);
        board.set_prepare_outcome(id, outcome.clone());

        let sched = build_scheduler(store.clone(), board.clone(), clock);
        let summary = sched.run_once(0).await.expect("run_once 成功");

        assert_eq!(summary.prepare_failed_persisted, 1, "第 {index} 例应落库");
        assert_eq!(store.state_of(id), Some(SubTaskState::Failed));
        assert_eq!(store.reason_of(id), Some(expected));
    }
}

/// 终结写入结果未知时**绝不**冒充"已收敛"：`expired_persisted` 不增、库里仍 queued、
/// 锁与主行汇总都不动，且下一轮仍会重新读出再试一次。
#[tokio::test]
async fn finalize_unknown_is_counted_and_leaves_row_untouched() {
    let trace = Trace::default();
    let store = FakeStore::new(trace.clone());
    let board = FakeBoard::new(trace.clone());
    let clock = FakeClock::new(20_000, true);
    let row = admissible_candidate(v4(985), [0xe7; 32], 10_000);
    let id = row.candidate.subtask_id;
    store.push_row(row);
    let main_id = store.main_task_of(id).expect("主任务");
    store.inject_finalize_unknown(id);

    let sched = build_scheduler(store.clone(), board.clone(), clock);
    let summary = sched.run_once(0).await.expect("run_once 成功");

    assert_eq!(summary.finalize_unknown, 1, "未知单独计数");
    assert_eq!(summary.expired_persisted, 0, "结果未知绝不当成已落库");
    assert_eq!(store.state_of(id), Some(SubTaskState::Queued));
    assert_eq!(store.reason_of(id), None);
    assert_eq!(store.lock_released_generation(id), None);
    assert!(store.main_summary(main_id).is_none());

    let again = sched.run_once(0).await.expect("第二轮成功");
    assert_eq!(again.candidates, 1, "未落库的候选不丢失，下轮继续收敛");
    assert_eq!(again.finalize_unknown, 1);
}

/// 终结写入的 CAS 契约（fake 与生产同一要求）：状态 CAS 与锁代际 CAS 任一不成立即
/// `Skipped` 且**零副作用**——不重复释放锁、不重复回写主任务汇总。
#[tokio::test]
async fn finalize_write_is_cas_guarded() {
    let trace = Trace::default();
    let store = FakeStore::new(trace);
    let row = admissible_candidate(v4(990), [0xe9; 32], 10_000_000);
    let id = row.candidate.subtask_id;
    store.push_row(row);
    let main_id = store.main_task_of(id).expect("主任务");

    let write = FinalizeWrite {
        subtask_id: id,
        target_state: SubTaskState::Expired,
        reason_code: ReasonCode::QueueExpired,
        expected_from_state: SubTaskState::Queued,
        expected_lock_generation: 1,
    };
    assert_eq!(
        store.finalize_subtask(&write).await.expect("存储未报错"),
        FinalizeVerdict::Applied
    );
    // 并发第二个周期：状态已非 queued → 确定跳过。
    assert_eq!(
        store.finalize_subtask(&write).await.expect("存储未报错"),
        FinalizeVerdict::Skipped
    );
    assert_eq!(store.lock_released_generation(id), Some(1), "锁只释放一次");
    let main = store.main_summary(main_id).expect("汇总已回写");
    assert_eq!(main.syncs, 1, "汇总只回写一次");
    assert_eq!(main.revision, 1, "主行 revision 只递增一次");
    assert_eq!(
        store.finalize_verdict_of(id),
        Some(FinalizeVerdict::Skipped),
        "第二次终结的最近裁决必须是跳过"
    );

    // 锁代际 CAS：代际不符即跳过，零副作用。
    let other = admissible_candidate(v4(991), [0xea; 32], 10_000_000);
    let other_id = other.candidate.subtask_id;
    store.push_row(other);
    let stale = FinalizeWrite {
        subtask_id: other_id,
        expected_lock_generation: 7,
        ..write.clone()
    };
    assert_eq!(
        store.finalize_subtask(&stale).await.expect("存储未报错"),
        FinalizeVerdict::Skipped
    );
    assert_eq!(store.state_of(other_id), Some(SubTaskState::Queued));
    assert!(
        store
            .main_summary(store.main_task_of(other_id).expect("主任务"))
            .is_none()
    );
}

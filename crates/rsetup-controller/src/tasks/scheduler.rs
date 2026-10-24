//! 调度准入纯逻辑 + 可注入、可测试的单次派发编排（04 §5.1/§5.2）。
//!
//! 本模块交付两层：
//! 1. [`admit_for_dispatch`]：离线纯函数准入裁决（04 §5.1 严格顺序），不依赖
//!    `rsetup_protocol`、不连库、不发板端请求；顺序与稳定 [`ReasonCode`] 归因是
//!    契约的一部分，不得调换。
//! 2. [`TaskScheduler`] / [`TaskScheduler::run_once`]：一次调度周期的 typed 编排
//!    核心，依赖三个**可注入**边界——调度存储边界 [`SchedulerStore`]（表达显式
//!    事务结果 [`CommitVerdict`]）、板端 6 方法契约 [`BoardClient`]（复用现有
//!    dyn-safe 契约，不另造第二套）、单调时钟 [`MonoClock`]。
//!
//! 边界纪律（04 §5.2 T-04）：候选准入 → 全局 16 槽 + 同设备互斥取槽 →
//! 事务外 `prepare` → **短事务内重验当前授权/设备锁并持久化 `dispatching` 意图**
//! （返回 [`CommitVerdict`]）→ 仅 `Confirmed` 后于事务外至多一次 `execute`。
//! 网络等待永不发生在数据库锁/事务持有时。[`CommitVerdict::CommitUnknown`] 与
//! `execute` 的 `Timeout`/`Transport` 结果未知**只转只读核验、绝不重发、绝不判成功**；
//! 提交失败/未知、未准入、非 queued、无锁/撤权路径下 `execute` 恒为 0。
//!
//! 时间纪律（同一切片的第二道闸）：一次周期会在候选之间反复 `.await`（`prepare`
//! 往返、短事务提交），周期起点的那一次读时**只**够回答"本轮是否值得开始"，**不得**
//! 当作任何候选 TTL/票据寿命的基线。因此队列期限与票据剩余寿命都在**逐候选**重取的
//! 单调时间上判定，并在 `prepare` 前后、短事务前后各设一道门禁；`prepare` 之后的判定
//! 一律按"票据已被消耗的流逝寿命"折算（见 [`TaskScheduler::run_once`] 文档）。
//!
//! 诚实边界：本切片是**编排核心**，跨进程 exactly-once 的强约束由
//! [`SchedulerStore::commit_dispatch_intent`] 的**持久 CAS 契约**表达（state 仍为
//! queued、锁归属与 authz_epoch 未变才提交）——该契约是**对被注入实现的要求**，
//! 本模块的测试 fake 只在**同一进程内**以互斥锁 + 内存比对模拟这种竞争（内存
//! CAS：重验与写入同处一个 `MutexGuard`，测试以多线程并发竞争证明其跨线程原子性），
//! 只能证明编排控制流本身不双发，**不构成**跨进程持久 CAS 已成立的证明；
//! 真实 SQL 适配（数据库条件更新 + 事务隔离）与生产 `BoardClient` driver（业务 codec
//! 已按所有者授权启用，但真实传输接线仍未实现）待接，本模块不构造伪生产入口。进程内不以全局永久 seen 集合
//! 做去重（避免泄漏内存），至多一次由单调用点控制流保证，跨进程则待真实持久
//! CAS 落地后才成立。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, RebootExecute, RebootTicket, TaskRecord, is_canonical_uuid_v4,
    validate_dto,
};
use sha2::Digest as _;
use sha2::Sha256;
use uuid::Uuid;

use crate::ControllerError;
use crate::tasks::board_client::{BoardClient, BoardClientError};
use crate::tasks::model::{ReasonCode, SubTaskState, TaskIntent};

/// 全局并发分派槽位上限（05 §5 待审默认）。
pub const GLOBAL_DISPATCH_SLOTS: usize = 16;
/// 队列期限 TTL（05 §5 待审默认）。
pub const QUEUE_TTL_MS: i64 = 60 * 60 * 1000;
/// 观测窗口（04 §5.3 待审建议）。
pub const OBSERVE_WINDOW_MS: i64 = 5 * 60 * 1000;
/// `execute` 记录可接受的状态：仅**派发确认态**（wire 词表的子集，刻意收窄）。
/// `succeeded`/`failed`/`unknown` 属完成/核验语义，不得在 execute 返回上记 accepted；
/// 但它们仍是 wire 词表内的**合法状态**，不属非法记录，因此既不计
/// [`ScheduleSummary::execute_accepted`]，也**不计** [`ScheduleSummary::record_rejects`]，
/// 只并入 [`ScheduleSummary::execute_unverified`] 转只读核验。
pub const DISPATCH_ACCEPT_STATES: [&str; 2] = ["accepted", "attempted"];

/// 准入裁决：`Admit` 才允许进入 prepare/execute 链路；`Deny` 携带 04 §3 稳定原因码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionVerdict {
    Admit,
    Deny { reason_code: ReasonCode },
}

/// 准入裁决（04 §5.1 严格顺序）：任一前置不满足即 `Deny`，且只返回最高优先级
/// 的那一个原因码；全部满足才 `Admit`。
///
/// 判定顺序（不可调换，决定 `reason_code` 归因）：
/// 1. `state != Queued` → `PermissionDenied`（非排队态绝不进入 dispatch）；
/// 2. actor 失效 / 无 reboot 权限 / 准入未批准 → `PermissionDenied`；
/// 3. 控制流不健康 → `DeviceOffline`；
/// 4. 能力不支持或板端持久 journal 不可用 → `CapabilityUnavailable`；
/// 5. 队列期限不可信 → `TimeUncertain`（调用方据此置 `held`，不判 `failed`）；
/// 6. 设备锁不属于本子任务 → `DeviceBusy`（绝不发送）。
///
/// 本函数是纯函数：不读 DB、不看时钟、不发送板端请求；`GLOBAL_DISPATCH_SLOTS`
/// 的取候选与 `prepare`/`execute` 接线属 `BLOCKED (awaiting BoardClient)` 切片。
#[allow(clippy::too_many_arguments)] // 9 参数签名由计划 04 Task 5 Interfaces 逐字固定
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
        return AdmissionVerdict::Deny {
            reason_code: ReasonCode::PermissionDenied,
        };
    }
    if !actor_active || !has_reboot || !admission_approved {
        return AdmissionVerdict::Deny {
            reason_code: ReasonCode::PermissionDenied,
        };
    }
    if !control_flow_healthy {
        return AdmissionVerdict::Deny {
            reason_code: ReasonCode::DeviceOffline,
        };
    }
    if !capability_supported || !persistent_journal_ok {
        return AdmissionVerdict::Deny {
            reason_code: ReasonCode::CapabilityUnavailable,
        };
    }
    if !queue_deadline_valid {
        return AdmissionVerdict::Deny {
            reason_code: ReasonCode::TimeUncertain,
        };
    }
    if !lock_owned {
        return AdmissionVerdict::Deny {
            reason_code: ReasonCode::DeviceBusy,
        };
    }
    AdmissionVerdict::Admit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_admission_denies_on_every_failed_precondition() {
        use crate::tasks::model::ReasonCode;
        use AdmissionVerdict::Deny;
        // 全部满足 -> Admit
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true
            ),
            AdmissionVerdict::Admit
        );
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                false,
                true,
                true,
                true,
                true,
                true,
                true,
                true
            ),
            Deny {
                reason_code: ReasonCode::PermissionDenied
            }
        );
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                false,
                true,
                true,
                true,
                true,
                true,
                true
            ),
            Deny {
                reason_code: ReasonCode::PermissionDenied
            }
        );
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                true,
                false,
                true,
                true,
                true,
                true,
                true
            ),
            Deny {
                reason_code: ReasonCode::PermissionDenied
            }
        );
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                true,
                true,
                false,
                true,
                true,
                true,
                true
            ),
            Deny {
                reason_code: ReasonCode::DeviceOffline
            }
        );
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                true,
                true,
                true,
                false,
                true,
                true,
                true
            ),
            Deny {
                reason_code: ReasonCode::CapabilityUnavailable
            }
        );
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                true,
                true,
                true,
                true,
                false,
                true,
                true
            ),
            Deny {
                reason_code: ReasonCode::CapabilityUnavailable
            }
        );
        // 期限不可信 -> TIME_UNCERTAIN（进 held，而非 failed）
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                true,
                true,
                true,
                true,
                true,
                false,
                true
            ),
            Deny {
                reason_code: ReasonCode::TimeUncertain
            }
        );
        // 锁不属于本子任务：绝不发送
        assert_eq!(
            admit_for_dispatch(
                SubTaskState::Queued,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                false
            ),
            Deny {
                reason_code: ReasonCode::DeviceBusy
            }
        );
        // 非 queued 状态不得进入 dispatch
        assert!(matches!(
            admit_for_dispatch(
                SubTaskState::Held,
                true,
                true,
                true,
                true,
                true,
                true,
                true,
                true
            ),
            AdmissionVerdict::Deny { .. }
        ));
    }

    /// 票据剩余寿命折算的口径：剩余 = `valid_for_ms` − 自 prepare **发起**起的单调流逝。
    fn ticket(valid_for_ms: u32) -> RebootTicket {
        RebootTicket {
            schema_version: BUSINESS_SCHEMA_VERSION,
            sub_task_id: "00000000-0000-4000-8000-000000000001".into(),
            boot_id: "00000000-0000-4000-8000-000000000002".into(),
            execution_token: [0x5a; 32],
            valid_for_ms,
        }
    }

    #[test]
    fn ticket_remaining_ms_subtracts_elapsed_monotonic_time() {
        // 无流逝：剩余就是票面 TTL（与旧的静态比较等价，保证不退化）。
        assert_eq!(ticket_remaining_ms(&ticket(60_000), 0), 60_000);
        // 有流逝：往返吃掉多少就少多少。
        assert_eq!(ticket_remaining_ms(&ticket(60_000), 50_000), 10_000);
        // 流逝超过票面 TTL：夹到 0，不得翻负后被误判成"还剩很多"。
        assert_eq!(ticket_remaining_ms(&ticket(60_000), 90_000), 0);
        // 时钟实现违反单调性（流逝为负）：按 0 流逝折算，绝不放大剩余寿命。
        assert_eq!(ticket_remaining_ms(&ticket(60_000), -5_000), 60_000);
        // 票面 TTL 取 u32 上界、流逝取 i64 上界：saturating 折算不得溢出为负。
        assert!(ticket_remaining_ms(&ticket(u32::MAX), i64::MAX) >= 0);
    }

    #[test]
    fn ticket_expiry_gate_uses_remaining_validity_not_face_ttl() {
        // 票面 60s、往返吃掉 25s → 剩 35s ≥ 20s：仍可用。
        assert!(!ticket_expired(&ticket(60_000), 20_000, 25_000));
        // 票面 60s、往返吃掉 50s → 剩 10s < 20s：**票面合法但已不可用**。
        // 这一条正是"静态比较 valid_for_ms"会漏掉的情形。
        assert!(ticket_expired(&ticket(60_000), 20_000, 50_000));
        // 剩余恰好等于最短值：边界可用（严格小于才过期）。
        assert!(!ticket_expired(&ticket(60_000), 20_000, 40_000));
        // 流逝再多 1ms 即过期。
        assert!(ticket_expired(&ticket(60_000), 20_000, 40_001));
    }

    /// 最短值配成 0 **不等于**"耗尽的票据可用"：剩余寿命必须**严格为正**才可发送。
    #[test]
    fn ticket_expired_when_remaining_lifetime_is_exhausted_regardless_of_policy() {
        // 策略最短值为 0：票面寿命恰好耗尽（remaining == 0）仍**必须**判过期。
        // 这里曾错断言"可用"——`0 < 0` 为 false 会让已过耗尽的票据放行到 execute。
        assert!(ticket_expired(&ticket(0), 0, 0));
        assert!(ticket_expired(&ticket(60_000), 0, 60_000));
        // 流逝超过票面 TTL（remaining 夹到 0）：同样过期。
        assert!(ticket_expired(&ticket(60_000), 0, 70_001));
        // 剩余仅 1ms：策略为 0 时可用（严格正即未被否定的票据）。
        assert!(!ticket_expired(&ticket(60_000), 0, 59_999));
        // 非零策略下同一结论：耗尽永远过期，且耗尽优先于策略取值。
        assert!(ticket_expired(&ticket(60_000), 20_000, 60_000));
        assert!(ticket_expired(&ticket(0), 20_000, 0));
        // 出厂默认策略（`min_ticket_validity_ms = 1`）同样绝不放行耗尽的票据。
        assert!(ticket_expired(&ticket(60_000), 1, 60_000));
    }
}

// ============================================================================
// 单次派发编排：可注入边界 + TaskScheduler::run_once（04 §5.1/§5.2 核心）
// ============================================================================

/// 编排内部短返回用的 dyn-safe future 别名（对象安全，便于 `Arc<dyn SchedulerStore>`）。
pub type SchedFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 一次调度周期读取到的候选子任务行（调度器面向的**只读快照**投影）。
///
/// 准入相关的布尔快照是候选读取时刻的值，`run_once` 先据此做纯准入预筛；
/// 授权与设备锁的**当前**状态由 [`SchedulerStore::commit_dispatch_intent`] 在短事务
/// 内重验，快照不可信时以重验结果为准（撤权/失去锁/已取消必须落 `DefinitelyNotCommitted`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchCandidate {
    pub subtask_id: Uuid,
    pub device_id: [u8; 32],
    /// 候选读取时刻的子任务状态；非 `Queued` 一律不准入（04 §5.1）。
    pub state: SubTaskState,
    pub actor_active: bool,
    pub has_reboot: bool,
    pub admission_approved: bool,
    pub control_flow_healthy: bool,
    pub capability_supported: bool,
    pub persistent_journal_ok: bool,
    /// 候选读取时刻设备锁是否归属**本**子任务；false 时不准入、绝不发送。
    pub lock_owned: bool,
    /// `sub_tasks.lock_generation`，短事务 CAS 的期望代际。
    pub lock_generation: u64,
    /// 候选读取时刻的 `schema_meta.authz_epoch` 快照，短事务 CAS 的期望值。
    pub expected_authz_epoch: u64,
    /// 排队截止的**单调毫秒**（相对 [`MonoClock`] 原点）；TTL 复核唯一依据。
    pub queue_deadline_mono_ms: i64,
    /// prepare 的 `expected_boot_id`（当前连接查询到的 boot_id）。
    pub expected_boot_id: String,
    pub controller_epoch: Uuid,
    pub session_epoch: Uuid,
    pub stream_epoch: Uuid,
}

/// 提交 `dispatching` 意图短事务内**当前**重验失败的原因（确定未提交）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevalidateFailure {
    /// 授权/锁代际快照与库中当前值不符（并发提交或旧快照）。
    StaleEpoch,
    /// actor 失效、撤权或准入不再批准 → PERMISSION_DENIED。
    AuthorizationRevoked,
    /// 设备锁不再归属本子任务或已释放 → DEVICE_BUSY，绝不发送。
    DeviceLockLost,
    /// 并发取消/held/终态改写：当前已非 queued。
    NotQueued,
}

/// 短事务提交 `dispatching` 意图的**显式**事务结果。
///
/// `DefinitelyNotCommitted` 与 `CommitUnknown` 绝不合并成一个 `bool`：提交未知
/// **不能**当作安全重试，只能转只读核验（04 §5.2、恢复语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitVerdict {
    /// 重验通过且事务 COMMIT 成功：意图已持久化，`dispatching` 已落库。
    Confirmed,
    /// 重验失败或明确回滚：意图**确定未提交**，子任务保持 queued，可安全留待下轮。
    DefinitelyNotCommitted { reason: RevalidateFailure },
    /// 提交结果未知（连接/超时）：**不得**执行、**不得**重发、**不得**判成功。
    CommitUnknown,
}

/// 写入 `dispatching` 意图短事务的入参：意图实体 + CAS 期望（重验锚点）。
///
/// **不派生 `Debug`**：`TaskIntent` 携带 `ticket_digest`/`request_digest` 私有摘要，
/// 刻意不 `Debug` 以免随 `{write:?}` 外泄（与 [`TaskIntent`] 定义一致）。
#[derive(Clone)]
pub struct DispatchIntentWrite {
    pub intent: TaskIntent,
    /// CAS 期望：`sub_tasks.lock_generation` 不变。
    pub expected_lock_generation: u64,
    /// CAS 期望：`schema_meta.authz_epoch` 不变。
    pub expected_authz_epoch: u64,
    /// CAS 期望：仅当子任务仍为 `queued` 才提交。
    pub expected_from_state: SubTaskState,
}

/// 终结一个子任务（`expired`/`failed`）所需的持久写入描述（04 §5.1 的失败收敛）。
///
/// 调度器只在**尚未发出 execute** 的路径上使用它：排队期限已到期 → `Expired` +
/// `QUEUE_EXPIRED`；`prepare` 明确失败 → `Failed` + 对应稳定原因码。两个分支都必须
/// 同时释放归属本代际的设备锁并回写主任务汇总，绝不留下"内存已放弃、库里仍 queued"
/// 的僵尸候选。
///
/// 归属（主任务/设备）由存储侧从**库里的**子任务行读取，不作入参：调用方自报的设备
/// 不能替代真相源。本结构不含私有摘要，因此可安全 `Debug`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizeWrite {
    pub subtask_id: Uuid,
    /// 目标终态：只允许 `Expired` 或 `Failed`（其余状态由核验/恢复路径负责）。
    pub target_state: SubTaskState,
    /// 04 §3 稳定原因码，与 `target_state` 一起构成可审计的收敛结论。
    pub reason_code: ReasonCode,
    /// CAS 期望：仅当子任务仍为该状态才终结（调度路径恒为 `Queued`）。
    pub expected_from_state: SubTaskState,
    /// CAS 期望：`sub_tasks.lock_generation` 未变才终结并释放锁。
    pub expected_lock_generation: u64,
}

/// [`SchedulerStore::finalize_subtask`] 的**显式**结果：与 [`CommitVerdict`] 同样
/// 刻意区分"确定未提交"与"结果未知"，不得折叠成 `bool`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalizeVerdict {
    /// 状态 + 原因码 + 设备锁释放 + 主任务汇总已在同一短事务内提交。
    Applied,
    /// CAS 未命中（并发已改写 state/锁代际）：**确定未**做任何持久副作用。
    Skipped,
    /// 结果未知（连接/超时）：不改判、不重发，子任务按库中原状态继续。
    Unknown,
}

/// 调度存储边界（**本地 trait**，不复用 `TaskRepository`——后者的 `begin` 返回真实
/// sqlx `Transaction`，无法在无数据库环境模拟提交未知分支）。
///
/// 实现必须保证 `commit_dispatch_intent` 是**一个短事务**：先按 [`DispatchIntentWrite`]
/// 的 CAS 期望重验当前 authz/锁/state，重验失败即确定回滚（`DefinitelyNotCommitted`），
/// 重验通过则更新 `dispatching` 并提交（`Confirmed`）；提交结果无法确定时才返回
/// `CommitUnknown`。事务内**永不**发生任何网络调用。
pub trait SchedulerStore: Send + Sync {
    /// 读取候选集（非终态 queued 候选）；实现应在只读短事务内完成后立即提交释放。
    fn list_candidates(&self) -> SchedFuture<'_, Vec<DispatchCandidate>>;

    /// 短事务：重验当前授权/设备锁/state，通过后持久化 `dispatching` 意图并提交，
    /// 返回 [`CommitVerdict`]。跨进程 exactly-once 的强约束**由此方法的持久 CAS 表达**：
    /// 实现必须以真实数据库的条件更新/事务隔离保证，测试 fake 的进程内内存 CAS
    /// 只是替身，不构成该强约束已成立的证明。
    fn commit_dispatch_intent(&self, write: &DispatchIntentWrite)
    -> SchedFuture<'_, CommitVerdict>;

    /// 短事务：把一个**尚未发出 execute** 的候选持久收敛到终态（04 §5.1 的失败收敛）：
    ///
    /// 1. 条件更新 `sub_tasks`：仅当仍为 [`FinalizeWrite::expected_from_state`] 且
    ///    `lock_generation` 未变，才写 `target_state` + `reason_code`；否则确定跳过；
    /// 2. 释放**归属本子任务本代际**的设备锁（`device_operation_locks`）；
    /// 3. 同事务回写 `main_tasks` 的 `state`/`outcome`/`counts_json`（RT-R8-3：子任务
    ///    变更与主任务汇总同提交、同回滚，不留「只动子任务、主行陈旧」的中间态）；
    /// 4. 提交结果无法确定时才返回 [`FinalizeVerdict::Unknown`]。
    ///
    /// 事务内**永不**发生任何网络调用；本次终结失败**绝不**影响本周期其余候选，也
    /// **绝不**让调度器据此重发 `prepare`/`execute`。
    fn finalize_subtask(&self, write: &FinalizeWrite) -> SchedFuture<'_, FinalizeVerdict>;
}

/// 单调时钟边界（可注入、可模拟）。返回相对原点的单调毫秒；不可信时返回 `None`。
pub trait MonoClock: Send + Sync {
    fn now_ms(&self) -> Option<i64>;
}

/// 生产 `MonoClock`：以进程启动 `Instant` 为原点，单调递增。
///
/// **诚实边界**：本类型不提供时间可信性判定（`now_ms` 恒 `Some`）；NTP 可信性判定
/// 属 `crate::time` 时间门禁，接生产 driver 时须以门禁证据包装本时钟再注入。
#[derive(Clone, Copy)]
pub struct SystemMonoClock {
    origin: std::time::Instant,
}

impl SystemMonoClock {
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

impl Default for SystemMonoClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonoClock for SystemMonoClock {
    fn now_ms(&self) -> Option<i64> {
        let ms = self.origin.elapsed().as_millis();
        Some(i64::try_from(ms).unwrap_or(i64::MAX))
    }
}

/// 一次调度周期的可观察结果计数（供测试断言与上层观测）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScheduleSummary {
    /// 读取到的候选数。
    pub candidates: usize,
    /// 通过 [`admit_for_dispatch`] 准入的候选数。
    pub admitted: usize,
    /// `board.prepare` 调用数（准入后才发）。
    pub prepared: usize,
    /// `prepare` 失败数（可安全结束，尚未 execute）。
    pub prepare_failed: usize,
    /// 票据未过 wire 契约校验、或与候选任务/boot 错绑、或候选 boot 本身非法的数
    /// （prepare 已发但不提交、不 execute）。
    pub ticket_rejects: usize,
    /// 票据**剩余**有效毫秒（`valid_for_ms` 减去自 `prepare` 发起以来消耗的单调时间）
    /// 不足可用下限而在**提交短事务之前**放弃的候选数（不提交、不 execute、释放预留槽）。
    /// 可用下限是 `max(policy.min_ticket_validity_ms, 1)`：寿命耗尽（剩余 `0`）在**任何**
    /// 策略取值（含 `0`）下都算这里。静态比较 `valid_for_ms` 本身**不算**这里。
    pub ticket_expired: usize,
    /// 意图短事务已返回 `Confirmed`（`dispatching` 已落库），但在发出 `execute`
    /// **之前**的守卫失败（票据剩余寿命不足，或时钟此刻不可信）而放弃发送的数：
    /// **绝不**回退已持久化的 `dispatching`、**绝不**发已过期的 `execute`，只并入
    /// [`ScheduleSummary::execute_unverified`] 转只读核验（本数不是额外的发送）。
    pub dispatched_not_executed: usize,
    /// 意图短事务返回 `Confirmed` 数。
    pub committed: usize,
    /// 意图短事务返回 `DefinitelyNotCommitted` 数（重验失败/回滚）。
    pub not_committed: usize,
    /// 意图短事务返回 `CommitUnknown` 数（不执行、不重发）。
    pub commit_unknown: usize,
    /// `board.execute` 调用数（至多等于 `committed - dispatched_not_executed`；
    /// 进程内至多一次）。
    pub executed: usize,
    /// execute 返回 `Ok`、记录通过契约/绑定核验**且** `state` 属派发确认态
    /// （[`DISPATCH_ACCEPT_STATES`]）的数（可记为已受理）。合法但非确认态的终态
    /// 记录（`succeeded`/`failed`/`unknown`）不计入本数。
    pub execute_accepted: usize,
    /// 结果未知、只转只读核验的数：四种情形并入计数——`Timeout`/`Transport`；
    /// 记录非法/错绑（[`ScheduleSummary::record_rejects`]）；记录合法但 `state`
    /// 非派发确认态（`succeeded`/`failed`/`unknown`）；已 `Confirmed` 但发 execute
    /// 前守卫失败（[`ScheduleSummary::dispatched_not_executed`]）。四者都不判成功、不重发。
    pub execute_unverified: usize,
    /// execute 返回明确拒绝/协议错误（不重发）数。
    pub execute_rejected: usize,
    /// `execute` 返回记录**本身非法/不可信**的数：未过 wire 契约校验，或任务/
    /// `pre_boot_id`/`journal_epoch` 绑定与本次派发不符（不记 accepted、不重发；
    /// 已含在 `execute_unverified` 内，不是额外的发送）。
    /// **不含**合法但非派发确认态的终态记录——那些记录本身合法，只计
    /// `execute_unverified`，不属本数的非法记录语义。
    pub record_rejects: usize,
    /// 因全局并发槽已满而放弃的候选数。
    pub slot_limit_skips: usize,
    /// 因本轮该设备已取槽而放弃的候选数（同设备互斥）。
    pub device_mutex_skips: usize,
    /// 排队期限**已到期**、经 [`SchedulerStore::finalize_subtask`] 持久置 `expired`
    /// （原因码 `QUEUE_EXPIRED`）、释放设备锁并回写主任务汇总的候选数（04 §5.1）。
    /// 只统计落库成功（`Applied`）的候选：内存计数与库里状态在此同源。
    pub expired_persisted: usize,
    /// `prepare` **明确**失败、经 [`SchedulerStore::finalize_subtask`] 持久置 `failed`
    /// （对应稳定原因码）、释放设备锁并回写主任务汇总的候选数（04 §5.1）。
    /// `Timeout`/`Transport` 这类结果未知的失败**不**计入（也不终结状态）。
    pub prepare_failed_persisted: usize,
    /// 终结写入 CAS 未命中（并发已改写 `state`/锁代际）的候选数：本次**零**持久副作用，
    /// 子任务按库中当前状态继续，留待下轮或核验路径。
    pub finalize_skipped: usize,
    /// 终结写入结果未知（连接/超时）的候选数：不改判、不重发，转核验路径收敛。
    pub finalize_unknown: usize,
    /// 本轮是否出现**单调时钟不可信**而中止派发：周期起点守卫置位时零候选读取、
    /// 零 prepare、零 execute；在候选之间某个门禁置位时立即中止本轮剩余候选
    /// （fail-closed），此前已完成的派发不回退。
    pub clock_untrusted: bool,
}

/// 一次调度周期的错误：仅候选读取失败会整体上抛（fail-closed，零发送）。
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("candidate read failed: {0}")]
    CandidateReadFailed(#[from] ControllerError),
}

/// 调度执行策略（待审默认来自 05 §5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerPolicy {
    /// 全局并发重启槽上限（05 §5 待审默认 16）。
    pub global_slots: usize,
    /// prepare 票据的**最短剩余**有效毫秒。判定口径是"折算流逝寿命后的剩余"，不是
    /// `valid_for_ms` 字面值：票据在 `prepare` 往返、请求组装/摘要、短事务提交与发出
    /// `execute` 之前每一道门禁上都要重算剩余（见 [`TaskScheduler::run_once`]），
    /// 任一道门禁剩余低于此值即不提交或不发。
    ///
    /// 本值是**加严**阈值，不是有效性下界：把它配成 `0` 只表示"不再额外加严"，
    /// **不等于**"寿命已耗尽（剩余 `0`）的票据可用"。票据可用性的硬下界恒为**严格正**
    /// 剩余寿命（见 [`ticket_expired`]），任何取值下都不会为剩余 `0` 的票据发送。
    pub min_ticket_validity_ms: u32,
}

impl Default for SchedulerPolicy {
    fn default() -> Self {
        Self {
            global_slots: GLOBAL_DISPATCH_SLOTS,
            min_ticket_validity_ms: 1,
        }
    }
}

/// 一次调度周期的编排器：持有三个可注入边界 + 策略。
pub struct TaskScheduler<S, C, K> {
    store: Arc<S>,
    board: Arc<C>,
    clock: Arc<K>,
    policy: SchedulerPolicy,
    /// 本进程 controller_epoch（落 intent.controller_epoch）；非去重 seen 集合。
    controller_epoch: Uuid,
}

impl<S, C, K> TaskScheduler<S, C, K>
where
    S: SchedulerStore,
    C: BoardClient,
    K: MonoClock,
{
    pub fn new(
        store: Arc<S>,
        board: Arc<C>,
        clock: Arc<K>,
        policy: SchedulerPolicy,
        controller_epoch: Uuid,
    ) -> Self {
        Self {
            store,
            board,
            clock,
            policy,
            controller_epoch,
        }
    }

    /// 单个候选的准入预筛（复用 [`admit_for_dispatch`] 严格顺序）。
    fn admit_candidate(candidate: &DispatchCandidate, now_mono_ms: i64) -> AdmissionVerdict {
        let queue_deadline_valid = now_mono_ms < candidate.queue_deadline_mono_ms;
        admit_for_dispatch(
            candidate.state,
            candidate.actor_active,
            candidate.has_reboot,
            candidate.admission_approved,
            candidate.control_flow_healthy,
            candidate.capability_supported,
            candidate.persistent_journal_ok,
            queue_deadline_valid,
            candidate.lock_owned,
        )
    }

    /// 把候选**持久收敛**到终态并释放设备锁、回写主任务汇总（04 §5.1 的失败收敛）。
    ///
    /// 返回 `true` 当且仅当存储侧确认**已落库**（[`FinalizeVerdict::Applied`]）。
    /// `Skipped`（CAS 未命中）与 `Unknown`（结果未知）都**不改判、不重发**：候选按库中
    /// 当前状态继续，由下一轮或核验路径收敛——把一次写失败当成「已终结」会让内存计数
    /// 与库里状态分叉，把 `Unknown` 当成「确定未提交」则会重复释放锁。
    /// 计数分流是本方法的**唯一**出口，两个调用点（队列期限到期 / prepare 明确失败）
    /// 因此共用同一套口径。
    async fn finalize_candidate(
        &self,
        candidate: &DispatchCandidate,
        target_state: SubTaskState,
        reason_code: ReasonCode,
        summary: &mut ScheduleSummary,
    ) -> bool {
        let write = FinalizeWrite {
            subtask_id: candidate.subtask_id,
            target_state,
            reason_code,
            expected_from_state: SubTaskState::Queued,
            expected_lock_generation: candidate.lock_generation,
        };
        // 存储错误一律折叠为「结果未知」：绝不因一次写失败改判，也不据此重发。
        let verdict = match self.store.finalize_subtask(&write).await {
            Ok(verdict) => verdict,
            Err(_) => FinalizeVerdict::Unknown,
        };
        match verdict {
            FinalizeVerdict::Applied => true,
            FinalizeVerdict::Skipped => {
                summary.finalize_skipped += 1;
                false
            }
            FinalizeVerdict::Unknown => {
                summary.finalize_unknown += 1;
                false
            }
        }
    }

    /// `prepare` 失败 → 04 §3 稳定原因码；**返回 `None` 表示结果未知或本轮可跳过，
    /// 禁止终结**。
    ///
    /// `prepare` 只换一张短期票据，尚未 execute，因此**明确**失败可以安全收敛为
    /// `failed`（04 §5.1「prepare 失败可结束为 failed，因为尚未 execute」）。映射口径：
    ///
    /// - `DeviceOffline` → `DEVICE_OFFLINE`；
    /// - `CapabilityUnavailable` 与 `ResourceExhausted` → 均为 `CAPABILITY_UNAVAILABLE`
    ///   （板端返回 1007 的资源耗尽是对新任务的**明确拒绝**，调度不排队等待资源释放，
    ///   中控无从重试，因此同样终结为 `failed`；04 §3 十二码内没有独立的资源码）；
    /// - `ProtocolMismatch` → `PROTOCOL_MISMATCH`；
    /// - 板端返回的明确拒绝码（`Rejected`）→ 能解析成稳定词表的用板端归因，否则按
    ///   执行前拒收处理（`EXECUTION_REJECTED`）。
    ///
    /// 只有 `Timeout`/`Transport`（不证明板端从未签发票据，结果未知）与 `DeviceBusy`
    /// （设备忙，属本轮跳过）返回 `None`，据它们写终态会把可恢复的任务误判成失败——
    /// 候选保持 `queued` 留待下轮。
    fn prepare_failure_reason(error: &BoardClientError) -> Option<ReasonCode> {
        match error {
            BoardClientError::DeviceOffline => Some(ReasonCode::DeviceOffline),
            // 能力不支持与资源不足都属「本应能受理却没有受理」，统一归
            // CAPABILITY_UNAVAILABLE（04 §3 十二码内没有独立的资源码）。
            BoardClientError::CapabilityUnavailable | BoardClientError::ResourceExhausted => {
                Some(ReasonCode::CapabilityUnavailable)
            }
            BoardClientError::ProtocolMismatch => Some(ReasonCode::ProtocolMismatch),
            // 板端明确拒绝：能解析成稳定词表的用板端归因，否则按执行前拒收处理。
            BoardClientError::Rejected { reason_code, .. } => Some(
                reason_code
                    .as_deref()
                    .and_then(ReasonCode::from_str_exact)
                    .unwrap_or(ReasonCode::ExecutionRejected),
            ),
            BoardClientError::Timeout
            | BoardClientError::Transport
            | BoardClientError::DeviceBusy => None,
        }
    }

    /// 候选自身的 `expected_boot_id` 解析为 UUID；非「规范 36 字符小写 UUIDv4」一律 `None`。
    /// 该字段是 prepare 请求字段，也是票据与 execute 请求绑定的唯一基准。
    fn canonical_expected_boot(candidate: &DispatchCandidate) -> Option<Uuid> {
        if !is_canonical_uuid_v4(&candidate.expected_boot_id) {
            return None;
        }
        Uuid::parse_str(&candidate.expected_boot_id).ok()
    }

    /// `prepare` 票据的契约与绑定核验（04 §5.2：意图之前必须完成）。返回 `Some(原因)`
    /// 即拒绝该票据（不提交、不发送）：
    /// 1. 票据 DTO 原样交**现有** wire 校验（版本、UUID 规范形态、非零 token、正 TTL），
    ///    本模块不自造第二套 codec/校验器；
    /// 2. 票据绑定的 `sub_task_id` 必须就是**本**候选子任务（错任务拒）；
    /// 3. 票据的 `boot_id` 必须就是候选的 `expected_boot_id`（错 boot 拒）。
    fn ticket_rejection_reason(
        candidate: &DispatchCandidate,
        expected_boot: Uuid,
        ticket: &RebootTicket,
    ) -> Option<&'static str> {
        if validate_dto(ticket).is_err() {
            return Some("ticket_dto_invalid");
        }
        let ticket_sub = Uuid::parse_str(&ticket.sub_task_id).ok()?;
        if ticket_sub != candidate.subtask_id {
            return Some("ticket_subtask_mismatch");
        }
        let ticket_boot = Uuid::parse_str(&ticket.boot_id).ok()?;
        if ticket_boot != expected_boot {
            return Some("ticket_boot_mismatch");
        }
        None
    }

    /// `execute` 返回记录的**非法性**核验（只判「这条记录能不能信」，不判「它是不是
    /// 派发确认态」——后者是 [`Self::is_dispatch_accepted`]，两件事绝不合并）。
    /// 返回 `Some(原因)` 即记录非法/不可信：不得记 accepted、不得重发。
    /// 核验层次（前一层不过即不再看后一层）：
    /// 1. 记录 DTO 交**现有** wire 校验（版本、UUID 规范形态、`record_version != 0`、
    ///    `state` 词表、可选 `observed_boot_id`/`evidence_kind`）；
    /// 2. 记录的任务必须就是本候选子任务；
    /// 3. `pre_boot_id` 必须就是本次 execute 依据的票据 boot（跨 boot 记录不可信）；
    /// 4. journal 版本：`journal_epoch` 必须是非 nil 的 UUID（wire 已保证规范形态）。
    ///
    /// 注意：`state` 落在 [`DISPATCH_ACCEPT_STATES`] 之外（`succeeded`/`failed`/
    /// `unknown`）**不是**非法——它们是 wire 词表内的合法完成/核验态（板端在首次
    /// `execute` 往返里直接回终态是规范允许的），因此这里**不得**返回拒绝原因，
    /// 由调用方按「合法但非派发确认态」单独分类。
    fn record_rejection_reason(
        candidate: &DispatchCandidate,
        ticket: &RebootTicket,
        record: &TaskRecord,
    ) -> Option<&'static str> {
        if validate_dto(record).is_err() {
            return Some("record_dto_invalid");
        }
        let record_sub = Uuid::parse_str(&record.sub_task_id).ok()?;
        if record_sub != candidate.subtask_id {
            return Some("record_subtask_mismatch");
        }
        if record.pre_boot_id != ticket.boot_id {
            return Some("record_pre_boot_mismatch");
        }
        if Uuid::parse_str(&record.journal_epoch)
            .map(|epoch| epoch.is_nil())
            .unwrap_or(true)
        {
            return Some("record_journal_epoch_invalid");
        }
        None
    }

    /// 记录是否处于**派发确认态**（[`DISPATCH_ACCEPT_STATES`]）。
    ///
    /// 调用前置：[`Self::record_rejection_reason`] 已返回 `None`，即 `state` 已过
    /// wire 词表校验，此处只做「确认态 vs 合法终态」的区分：`true` 才允许记
    /// `execute_accepted`；`false` 的 `succeeded`/`failed`/`unknown` 属合法但非确认态，
    /// 既不记 accepted、也不是非法记录，只能转只读核验。
    fn is_dispatch_accepted(record: &TaskRecord) -> bool {
        DISPATCH_ACCEPT_STATES.contains(&record.state.as_str())
    }

    /// 运行一次调度周期（04 §5.1/§5.2 核心边界）。
    ///
    /// `active_slots` 为本次之前已占用全局并发槽数（来自上层读库），本轮新派发
    /// 不超过 `policy.global_slots - active_slots`。任何板端 `.await` 都发生在
    /// [`SchedulerStore`] 短事务之外；网络等待永不持有数据库锁/事务。
    ///
    /// ## 时间门禁（逐候选重取单调时间）
    ///
    /// 一个周期内会在候选之间反复 `.await`：前序候选的 `prepare` 往返与短事务提交
    /// 都能消耗掉**后序**候选的排队期限与**当前**候选的票据寿命。因此周期起点那次
    /// 读时只作"本轮是否值得开始"的守卫，任何 TTL/票据判定都必须用**当场**重取的
    /// 单调时间。本实现按顺序设四道门禁，任一读到不可信时钟即中止本轮（fail-closed，
    /// 置 [`ScheduleSummary::clock_untrusted`]）：
    /// 1. **准入前**：用最新时刻复核 `queue_deadline_mono_ms`（队列 TTL）；
    /// 2. **prepare 前**记下 `prepare_started_ms`，**返回后**立即按流逝寿命复核票据
    ///    剩余有效窗（[`ticket_expired`]），不足最短值即不提交、不 execute；
    /// 3. **短事务提交前**：组装/摘要同样在票据寿命内，再复核一次；观察窗基线也取自
    ///    这一最新时刻（而非周期起点）；
    /// 4. **`Confirmed` 之后、发出 `execute` 之前**：短事务 await 同样消耗票据寿命。
    ///    此时意图**已**持久化为 `dispatching`，因此**绝不**回退为 `queued`（回退会让
    ///    并发周期的 CAS 误判为可重发，制造双发），也**绝不**发出已过期的 `execute`，
    ///    只置 [`ScheduleSummary::dispatched_not_executed`] 并并入
    ///    [`ScheduleSummary::execute_unverified`] 保留只读核验。
    ///
    /// 四道票据门禁共用 [`ticket_expired`] 的口径：**剩余寿命必须严格为正**，寿命
    /// 耗尽（`remaining == 0`）在任何 `policy.min_ticket_validity_ms` 取值（含 `0`）
    /// 下都视为过期，绝不为已被板端倒计时耗尽的票据发送 `execute`。
    ///
    /// ## 失败收敛（不止于内存计数）
    ///
    /// 两条**尚未发出 execute** 的路径不再只加 `summary` 计数，而是经
    /// [`SchedulerStore::finalize_subtask`] 持久收敛（同事务写终态 + 原因码 + 释放设备锁
    /// + 回写主任务汇总）：
    ///
    /// - **排队期限已到期** → `expired` + `QUEUE_EXPIRED`（[`ScheduleSummary::expired_persisted`]）：
    ///   走到准入时单调时钟必然可信（不可信已在门禁 2.5 中止本轮），故 `TimeUncertain`
    ///   在这里只可能是"期限确实已被跨过"，而期限不会倒流，继续留 `queued` 只会让它
    ///   每轮被重新读出、反复判定；
    /// - **`prepare` 明确失败** → `failed` + 对应稳定原因码（[`ScheduleSummary::prepare_failed_persisted`]）：
    ///   `prepare` 只换票据、尚未 execute。`DeviceOffline`→`DEVICE_OFFLINE`、
    ///   `CapabilityUnavailable`/`ResourceExhausted`→`CAPABILITY_UNAVAILABLE`、
    ///   `ProtocolMismatch`→`PROTOCOL_MISMATCH`、板端明确拒绝码→对应稳定原因码，均终结为
    ///   `failed`；`Timeout`/`Transport`（结果未知）与 `DeviceBusy`（设备忙本轮跳过）**不**
    ///   终结，候选保持 `queued`。
    ///
    /// 终结写入 CAS 未命中记 `finalize_skipped`、结果未知记 `finalize_unknown`，两者都
    /// **不改判、不重发**；`commit_dispatch_intent` 之后的路径（含票据过期）**绝不**回退
    /// 已持久化的 `dispatching`，其收敛属核验/恢复切片的职责，不在本方法内。
    pub async fn run_once(&self, active_slots: usize) -> Result<ScheduleSummary, SchedulerError> {
        let mut summary = ScheduleSummary::default();

        // 0. 单调时钟可信性守卫（周期起点）：不可信 → 整体拒绝（连候选都不读，零 prepare、零 execute）。
        //    队列 TTL / 票据有效期只能由可信单调时钟判定，绝不用墙钟兜底。
        //    注意：这里的读数**不**向下传递为任何判定的基线——逐候选门禁各自重取。
        if self.clock.now_ms().is_none() {
            summary.clock_untrusted = true;
            return Ok(summary);
        }

        // 1. 只读候选集：实现内为只读短事务，读取后即提交释放；失败整体 fail-closed。
        let mut candidates = self
            .store
            .list_candidates()
            .await
            .map_err(SchedulerError::CandidateReadFailed)?;
        summary.candidates = candidates.len();
        // 确定性顺序：按 subtask_id 升序，保证跨进程/重启可复现的取槽次序。
        candidates.sort_by_key(|c| c.subtask_id);

        let mut free_slots = self.policy.global_slots.saturating_sub(active_slots);
        // 本轮**局部**设备集合（同设备互斥）：随函数返回即释放，绝不作为跨轮 seen 集合。
        let mut devices_this_run: std::collections::HashSet<[u8; 32]> =
            std::collections::HashSet::new();

        for cand in candidates {
            // 2. 全局并发槽：无空槽则本轮剩余候选一律放弃。
            if free_slots == 0 {
                summary.slot_limit_skips += 1;
                continue;
            }

            // 2.5 逐候选**重取**单调时间：走到这里说明本轮已经 await 过前序候选的
            //     prepare/短事务（或至少读库），周期起点的时刻早已过期，复用它判 TTL
            //     会把"排队期限已过"的候选继续放进 dispatch。不可信即中止本轮。
            let now_mono_ms = match self.clock.now_ms() {
                Some(ms) => ms,
                None => {
                    summary.clock_untrusted = true;
                    break;
                }
            };

            // 3. 候选准入预筛（复用 admit_for_dispatch 严格顺序，TTL 用刚重取的时刻）。
            //    未准入不占设备、不占槽。
            match Self::admit_candidate(&cand, now_mono_ms) {
                AdmissionVerdict::Deny { reason_code } => {
                    // 队列期限**已到期**（此刻时钟可信，故归因是确定的而非"不可信"）：
                    // 排队期限只会被单调时钟跨过、不会回来，继续留 queued 只会让它每轮
                    // 被重新读出。据 04 §5.1 持久收敛为 expired + QUEUE_EXPIRED，同事务
                    // 释放设备锁并回写主任务汇总；不占槽、不 prepare、不 execute。
                    if reason_code == ReasonCode::TimeUncertain
                        && now_mono_ms >= cand.queue_deadline_mono_ms
                    {
                        let persisted = self
                            .finalize_candidate(
                                &cand,
                                SubTaskState::Expired,
                                ReasonCode::QueueExpired,
                                &mut summary,
                            )
                            .await;
                        summary.expired_persisted += usize::from(persisted);
                    }
                    continue;
                }
                AdmissionVerdict::Admit => summary.admitted += 1,
            }

            // 4. 同设备互斥：本轮该设备已取槽则跳过（仅对已准入候选计数）。
            if !devices_this_run.insert(cand.device_id) {
                summary.device_mutex_skips += 1;
                continue;
            }
            // 预留一个并发槽，约束 prepare 数不超过空槽。
            free_slots -= 1;

            // 5. 候选自身的 expected_boot 必须是规范 UUIDv4：它既是 prepare 的请求字段，
            //    也是票据与 execute 请求绑定的唯一基准。非法即不发送（fail-closed）。
            let expected_boot = match Self::canonical_expected_boot(&cand) {
                Some(boot) => boot,
                None => {
                    summary.ticket_rejects += 1;
                    free_slots += 1;
                    continue;
                }
            };

            // 6. 事务外 prepare：换取短期票据。失败可安全结束（尚未 execute，可 failed）。
            //    发起**之前**先记下单调起点：票据从板端签发即开始倒计时，本行的 await
            //    往返正是随后所有"剩余寿命"判定的流逝基线，缺它就只能静态看 valid_for_ms。
            let prepare_started_ms = match self.clock.now_ms() {
                Some(ms) => ms,
                None => {
                    summary.clock_untrusted = true;
                    break;
                }
            };
            let prepared = self
                .board
                .prepare(&cand.device_id, cand.subtask_id, &cand.expected_boot_id)
                .await;
            summary.prepared += 1;
            let ticket = match prepared {
                Ok(ticket) => ticket,
                Err(error) => {
                    summary.prepare_failed += 1;
                    free_slots += 1; // 未提交任何意图，释放预留槽
                    // **明确**失败（非超时/传输未知）才可在 execute 之前收敛为 failed：
                    // 此时库里仍 queued、未发出任何重启请求，04 §5.1 允许结束为 failed。
                    // 同事务释放设备锁并回写主任务汇总，不留"已放弃但仍占锁"的僵尸候选。
                    if let Some(reason_code) = Self::prepare_failure_reason(&error) {
                        let persisted = self
                            .finalize_candidate(
                                &cand,
                                SubTaskState::Failed,
                                reason_code,
                                &mut summary,
                            )
                            .await;
                        summary.prepare_failed_persisted += usize::from(persisted);
                    }
                    continue;
                }
            };

            // 7. 票据契约与绑定核验（持久意图**之前**）：票据 DTO 交现有 wire 校验，
            //    再核对绑定的子任务与 boot 就是本候选本身。任何不符都不提交、不发送，
            //    并释放预留槽（未产生任何持久副作用）。
            if Self::ticket_rejection_reason(&cand, expected_boot, &ticket).is_some() {
                summary.ticket_rejects += 1;
                free_slots += 1;
                continue;
            }

            // 8. 票据**剩余寿命**门禁（prepare 返回后立刻重取时刻）：往返吃掉的时间必须
            //    从 valid_for_ms 里扣掉；不可信 → 中止本轮。剩余不足最短值 → 不提交、
            //    不 execute，并释放预留槽（尚未产生任何持久副作用）。
            let after_prepare_ms = match self.clock.now_ms() {
                Some(ms) => ms,
                None => {
                    summary.clock_untrusted = true;
                    break;
                }
            };
            let prepare_elapsed_ms = after_prepare_ms.saturating_sub(prepare_started_ms);
            if ticket_expired(
                &ticket,
                self.policy.min_ticket_validity_ms,
                prepare_elapsed_ms,
            ) {
                summary.ticket_expired += 1;
                free_slots += 1;
                continue;
            }

            // 9. 组装 execute 请求与 dispatching 意图（票据摘要而非明文 token）。
            //    请求的 sub_task_id/expected_boot_id 取自**候选自身**（不是票据回显），
            //    保证实际发送的请求与随后持久化的意图指向同一任务与同一 boot。
            let request = RebootExecute {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: cand.subtask_id.to_string(),
                expected_boot_id: cand.expected_boot_id.clone(),
                execution_token: ticket.execution_token,
            };
            if validate_dto(&request).is_err() {
                // 防御：绑定核验通过后仍不合法（如候选字段异常）→ 不提交、不发送。
                summary.ticket_rejects += 1;
                free_slots += 1;
                continue;
            }
            // 9.5 票据**剩余寿命**门禁（短事务**之前**再取一次时刻）：请求组装与两把摘要
            //     计算同样落在票据寿命之内。此次读数同时是观察窗基线——观察窗属"从实际
            //     提交/派发阶段起算"的窗口，用周期起点会把已在 prepare 往返中烧掉的时间
            //     也算进观察窗，等于凭空延长核验期限。
            let before_commit_ms = match self.clock.now_ms() {
                Some(ms) => ms,
                None => {
                    summary.clock_untrusted = true;
                    break;
                }
            };
            let commit_elapsed_ms = before_commit_ms.saturating_sub(prepare_started_ms);
            if ticket_expired(
                &ticket,
                self.policy.min_ticket_validity_ms,
                commit_elapsed_ms,
            ) {
                summary.ticket_expired += 1;
                free_slots += 1;
                continue;
            }
            let intent = TaskIntent {
                subtask_id: cand.subtask_id,
                controller_epoch: self.controller_epoch,
                session_epoch: cand.session_epoch,
                stream_epoch: cand.stream_epoch,
                pre_boot_id: ticket.boot_id.clone(),
                ticket_digest: digest_ticket(&ticket),
                request_digest: digest_request(&cand, self.controller_epoch, &request),
                // 观察窗截止自**提交阶段**重取的最新单调时刻起算（本方法第 9.5 步）；
                // 真实 SQL 适配映射到服务端墙钟基线。
                observe_deadline_ms: before_commit_ms.saturating_add(OBSERVE_WINDOW_MS),
            };
            let write = DispatchIntentWrite {
                intent,
                expected_lock_generation: cand.lock_generation,
                expected_authz_epoch: cand.expected_authz_epoch,
                expected_from_state: SubTaskState::Queued,
            };

            // 10. 短事务：重验当前授权/设备锁/state，通过后持久化 dispatching 意图并提交。
            //    任何 ControllerError（如连接错误）保守折叠为 CommitUnknown：结果未知，
            //    绝不 execute、绝不重试。
            let verdict = self
                .store
                .commit_dispatch_intent(&write)
                .await
                .unwrap_or(CommitVerdict::CommitUnknown);
            match verdict {
                CommitVerdict::DefinitelyNotCommitted { .. } => {
                    // 重验失败（撤权/丢锁/已非 queued）：确定未提交，释放预留槽。
                    summary.not_committed += 1;
                    free_slots += 1;
                    continue;
                }
                CommitVerdict::CommitUnknown => {
                    // 提交结果未知：保守保留预留槽，不 execute、不重试（不能当安全重试）。
                    summary.commit_unknown += 1;
                    continue;
                }
                CommitVerdict::Confirmed => summary.committed += 1,
            }

            // 10.5 票据**剩余寿命**门禁（`Confirmed` 之后、发出 execute **之前**）：短事务
            //      本身也在消耗票据寿命。此处**已无法**当作"什么都没发生"处理：
            //      意图已持久化为 dispatching，回退成 queued 会让并发周期的 CAS 认为可以
            //      重新提交，直接制造双发；发一张已过期的 execute 同样是发送被否定的票据。
            //      唯一诚实的收敛是：**不发** execute、保留已落库的 dispatching，转只读核验。
            let before_execute_ms = match self.clock.now_ms() {
                Some(ms) => ms,
                None => {
                    summary.clock_untrusted = true;
                    summary.dispatched_not_executed += 1;
                    summary.execute_unverified += 1;
                    break;
                }
            };
            let execute_elapsed_ms = before_execute_ms.saturating_sub(prepare_started_ms);
            if ticket_expired(
                &ticket,
                self.policy.min_ticket_validity_ms,
                execute_elapsed_ms,
            ) {
                summary.dispatched_not_executed += 1;
                summary.execute_unverified += 1;
                continue;
            }

            // 11. 仅 Confirmed 后，于事务外对本子任务至多一次 execute（单调用点，不循环重发）。
            summary.executed += 1;
            match self.board.execute(&cand.device_id, &ticket, &request).await {
                // 返回记录三分类（互斥，不得合并计数）：
                //   a) 非法/错绑记录（DTO、任务、boot、journal）→ record_rejects + 只读核验；
                //   b) 合法但非派发确认态（succeeded/failed/unknown）→ 只转只读核验，
                //      它不是非法记录（不计 record_rejects），也不是派发确认（不计 accepted）；
                //   c) 合法且 state ∈ DISPATCH_ACCEPT_STATES → 记 accepted。
                // 三类一律不重发（至多一次 execute 由单调用点保证）。
                Ok(record) => {
                    if Self::record_rejection_reason(&cand, &ticket, &record).is_some() {
                        summary.record_rejects += 1;
                        summary.execute_unverified += 1;
                    } else if !Self::is_dispatch_accepted(&record) {
                        summary.execute_unverified += 1;
                    } else {
                        summary.execute_accepted += 1;
                    }
                }
                // 超时/传输：结果未知，只转只读核验（由恢复/核验路径处理），绝不重发。
                Err(BoardClientError::Timeout) | Err(BoardClientError::Transport) => {
                    summary.execute_unverified += 1;
                }
                // 明确拒绝/协议不匹配等：不重发、不判成功。
                Err(_) => summary.execute_rejected += 1,
            }
        }

        Ok(summary)
    }
}

/// 票据在 `elapsed_ms` 单调流逝之后**还剩**多少有效毫秒。
///
/// `elapsed_ms` 是自 `prepare` **发起**那一刻（不是返回时刻）起消耗的单调时间：板端
/// 签发票据时给出的 `valid_for_ms` 从签发即开始倒计时，`prepare` 的网络往返、本地
/// 校验/摘要、短事务提交全部落在票据寿命之内，因此必须按流逝折算——静态比较
/// `valid_for_ms` 会把"往返已经吃掉寿命"的票据误判为可用。
/// `elapsed_ms` 为负（时钟实现违反单调性）时按 0 流逝折算，绝不因此放大剩余寿命。
fn ticket_remaining_ms(ticket: &RebootTicket, elapsed_ms: i64) -> i64 {
    i64::from(ticket.valid_for_ms)
        .saturating_sub(elapsed_ms.max(0))
        .max(0)
}

/// 票据剩余寿命的**可用下限**：剩余必须**严格为正**才允许据此发送 `execute`。
///
/// `min_ticket_validity_ms` 是策略对"还剩多少才算够用"的**加严**，不是对"还剩多少
/// 才算有效"的**放宽**：把它配成 `0` 只表示"不再额外加严"，**绝不等于**"剩余 0 也
/// 可用"。故有效下限取 `max(min_ticket_validity_ms, 1)`。
const MIN_USABLE_REMAINING_MS: i64 = 1;

/// 票据是否已**过期可用窗**：折算流逝后的剩余毫秒不足可用下限，判定式等价于
/// `remaining == 0 || remaining < min_ticket_validity_ms`。
///
/// 1. **寿命耗尽即过期，与策略取值无关**：`remaining == 0` 表示票据已被板端倒计时
///    耗尽，无论 `min_ticket_validity_ms` 配成什么——**包括 `0`**——都必须判为过期。
///    写成纯 `remaining < min` 时，`min == 0` 会让 `0 < 0 == false` 把已耗尽的票据
///    判为"未过期"并放行 `execute`，即主动发送一张已被否定的票据；这为本函数必须
///    排除的失效模式，**绝不能发送过期票据**。
/// 2. **策略加严**：剩余低于 `min_ticket_validity_ms` 同样不可用（该值 `== 0` 时本条
///    恒不成立，由第 1 条兜底）。
///
/// 这是策略判定，不是记录合法性判定——过期票据仍然是合法 DTO，只是不得据其发送。
fn ticket_expired(ticket: &RebootTicket, min_ticket_validity_ms: u32, elapsed_ms: i64) -> bool {
    let remaining = ticket_remaining_ms(ticket, elapsed_ms);
    remaining < i64::from(min_ticket_validity_ms).max(MIN_USABLE_REMAINING_MS)
}

/// 票据摘要域标签（与请求摘要域隔离，避免同物料同摘要）。
const TICKET_DIGEST_LABEL: &[u8] = b"rsetup:dispatch:ticket:v2";
/// execute 请求摘要域标签。
const REQUEST_DIGEST_LABEL: &[u8] = b"rsetup:dispatch:request:v2";

/// 长度前缀写入：字符串字段之间不得因拼接而歧义（与 wire 规范哈希同一口径）。
fn digest_str(hash: &mut Sha256, value: &str) {
    let len = u64::try_from(value.len()).expect("usize fits u64 on all supported Rust targets");
    hash.update(len.to_be_bytes());
    hash.update(value.as_bytes());
}

/// 计算 prepare 票据摘要（32B 私有证据，非明文 token；不进入 HTTP/debug）。
/// 覆盖票据 DTO 的全部安全字段：`schema_version`、绑定 `sub_task_id`、`boot_id`、
/// `execution_token` 与 `valid_for_ms`（缺任何一项都可能让不同票据撞同一摘要）。
fn digest_ticket(ticket: &RebootTicket) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(TICKET_DIGEST_LABEL);
    h.update(ticket.schema_version.to_be_bytes());
    digest_str(&mut h, &ticket.sub_task_id);
    digest_str(&mut h, &ticket.boot_id);
    h.update(ticket.execution_token);
    h.update(ticket.valid_for_ms.to_be_bytes());
    h.finalize().into()
}

/// 计算 execute 请求摘要（32B 私有证据）：覆盖**实际发出**请求的全部安全字段
/// （`schema_version`/`sub_task_id`/`expected_boot_id`/`execution_token`）与意图的
/// 三个代际（controller/session/stream），使摘要确实指向这一次执行所属的任务。
fn digest_request(
    candidate: &DispatchCandidate,
    controller_epoch: Uuid,
    request: &RebootExecute,
) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(REQUEST_DIGEST_LABEL);
    h.update(request.schema_version.to_be_bytes());
    digest_str(&mut h, &request.sub_task_id);
    digest_str(&mut h, &request.expected_boot_id);
    h.update(request.execution_token);
    h.update(controller_epoch.as_bytes());
    h.update(candidate.session_epoch.as_bytes());
    h.update(candidate.stream_epoch.as_bytes());
    h.finalize().into()
}

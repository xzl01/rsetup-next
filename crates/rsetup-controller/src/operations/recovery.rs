//! 受控恢复模式（Controlled Recovery Mode，05 §9 / 04 §5.2）。
//!
//! 旧备份恢复不同于普通重启：进入受控模式后**撤销会话、暂停自动准入/变更调度**，
//! 只有核对最近吊销/授权/密码/任务（[`RecoveryExitChecklist`]）且度过保留观察窗
//! （[`crate::operations::retention::RecoveryRetentionPolicy`]）后才允许解除。
//! 本模块是进程内**唯一**的恢复模式真源：HTTP 探针（[`RecoveryModeProbe`]）、
//! 调度循环（`sched_task`）与准入/变更任务闸门都必须读取同一个
//! [`RuntimeRecoveryController`] 实例，不存在第二套模式快照。
//!
//! 探针语义（`api::system` 契约）：`ready`/`mode` 必须来自 Runtime 当前真实快照。
//! 控制器已装配即恒 `ready = true`（模式本身可读写），`mode` 逐字为
//! `"normal"` 或 `"controlled_recovery"`；探针就绪 ≠ 恢复放行（05 §2），
//! 受控模式下调度与变更任务依旧被拒绝。
//!
//! 管理入口（05 §9 的「撤销会话」+「核对后才解除」）：
//! [`RuntimeRecoveryController::enter_controlled_recovery_with_revocation`] 在切模式的
//! 同一次调用里吊销传输线活跃会话；[`RuntimeRecoveryController::exit_controlled_recovery`]
//! 只按核对清单与观察窗判定。两条路径都留一条**去敏**审计记录
//! （[`RecoveryAuditEvent`]：只有事件种类、结果、模式与计数，不含身份字节与公钥原文），
//! 库内 `audit_events` 账本仍由网关层的写入线负责，本模块不复制身份。

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Instant;

use crate::api::ManagementFuture;
use crate::api::system::{RecoveryModeProbe, RecoveryView};
use crate::error::ControllerError;
use crate::transport::manager::TransportManager;

use super::retention::RecoveryRetentionPolicy;

/// Runtime 恢复模式。对外字符串恒为 [`Self::as_str`] 的返回值：
/// `Normal` → `"normal"`，`ControlledRecovery` → `"controlled_recovery"`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecoveryMode {
    /// 正常模式：自动调度与变更任务照常。
    Normal,
    /// 受控恢复模式：阻止自动调度与变更任务，只允许管理写
    /// （登录、改密、注销、审计、单设备只读核验、管理员退出恢复模式）。
    ControlledRecovery,
}

impl RecoveryMode {
    /// 线上契约字符串（`GET /api/v1/system/status` 的 `recovery_mode` 字段）。
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::ControlledRecovery => "controlled_recovery",
        }
    }
}

/// 受控恢复模式下的一次操作意图。闸门只回答「当前模式是否放行这一类操作」，
/// 不替代各条业务线自身的认证/授权/CSRF 门禁。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecoveryOperation {
    /// 管理员登录（管理写，05 §9「撤销会话」后管理员必须能重新进入）。
    AdminLogin,
    /// 修改密码（管理写）。
    PasswordChange,
    /// 注销（管理写）。
    Logout,
    /// 审计写入（管理写；审计不因模式而丢失）。
    AuditWrite,
    /// 单设备只读核验（管理员可**显式放行**的读核实，05 §9）。
    DeviceReadOnlyVerify,
    /// 管理员退出恢复模式（管理写；是否真正解除还受核对清单与保留窗约束）。
    ExitRecovery,
    /// 自动调度派发（受控模式下**阻止**）。
    AutomaticScheduling,
    /// 变更任务下发（受控模式下**阻止**；备份中的 queued 任务不自动重放）。
    MutationTask,
}

impl RecoveryOperation {
    /// 本操作是否属于「管理写」：受控恢复模式下唯一放行的类别。
    pub const fn is_admin_write(self) -> bool {
        matches!(
            self,
            Self::AdminLogin
                | Self::PasswordChange
                | Self::Logout
                | Self::AuditWrite
                | Self::DeviceReadOnlyVerify
                | Self::ExitRecovery
        )
    }
}

/// 解除受控恢复模式前的核对清单（05 §9「核对最近吊销/授权/密码/任务后才解除」）。
/// 四项必须由调用方逐项真实核对后置位；本类型不伪造「已核对」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryExitChecklist {
    /// 最近吊销（会话/授权撤销）已核对。
    pub revocations_checked: bool,
    /// 最近授权变更已核对。
    pub grants_checked: bool,
    /// 最近密码变更已核对。
    pub passwords_checked: bool,
    /// 最近任务（未决/queued）已核对。
    pub tasks_checked: bool,
}

impl RecoveryExitChecklist {
    /// 四项全部核对完成才满足解除条件。
    pub const fn is_complete(self) -> bool {
        self.revocations_checked
            && self.grants_checked
            && self.passwords_checked
            && self.tasks_checked
    }
}

// ---------------------------------------------------------------------------
// 去敏审计账本（05 §9 管理写必须留痕）
// ---------------------------------------------------------------------------

/// 审计事件种类：进入受控恢复模式（含会话吊销）。
pub const RECOVERY_AUDIT_ENTER: &str = "recovery_enter";
/// 审计事件种类：解除受控恢复模式的尝试（含被门禁拒绝的尝试）。
pub const RECOVERY_AUDIT_EXIT: &str = "recovery_exit";
/// 审计结果：状态迁移已生效。
pub const RECOVERY_AUDIT_OUTCOME_OK: &str = "ok";
/// 审计结果：核对清单未四项齐全（HTTP 侧映射 400 `CHECKLIST_INCOMPLETE`）。
pub const RECOVERY_AUDIT_OUTCOME_CHECKLIST_INCOMPLETE: &str = "checklist_incomplete";
/// 审计结果：当前不处于受控恢复模式（HTTP 侧映射 409 `INVALID_STATE`）。
pub const RECOVERY_AUDIT_OUTCOME_INVALID_STATE: &str = "invalid_state";
/// 审计结果：保留观察窗未度满（HTTP 侧映射 409 `INVALID_STATE`）。
pub const RECOVERY_AUDIT_OUTCOME_WINDOW_NOT_OPEN: &str = "observation_window_open";

/// 进程内保留的恢复模式审计条数上限：超出后丢弃最旧记录（管理动作频度极低）。
/// 本账本是**进程内**的运维可观测面，不替代库内 `audit_events` 账本，也不是它的缓存。
pub const MAX_RECOVERY_AUDIT_EVENTS: usize = 64;

/// 恢复模式管理动作的**去敏**审计记录。
///
/// 字段集合即去敏边界：只有事件种类、结果、迁移后的模式、被吊销的活跃会话计数与
/// 核对清单四个布尔量。**不含**操作者用户名/会话令牌/Cookie/CSRF/来源地址，也不含
/// 设备公钥原文——公钥只以「吊销了几条会话」的计数出现。操作者与库内账本的对应关系
/// 由网关层的 `audit_events` 写入线负责，本记录不复制任何身份字节。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryAuditEvent {
    /// [`RECOVERY_AUDIT_ENTER`] 或 [`RECOVERY_AUDIT_EXIT`]。
    pub event: &'static str,
    /// 结果：[`RECOVERY_AUDIT_OUTCOME_OK`] 或对应拒绝原因。
    pub outcome: &'static str,
    /// 本条记录**之后**的模式快照（[`RecoveryMode::as_str`]）。拒绝路径即未变更的模式。
    pub mode: &'static str,
    /// 本次进入时经 `apply_revoke` 终止的活跃会话数；非进入事件或未装配传输线为 `0`。
    pub sessions_revoked: usize,
    /// 解除尝试携带的核对清单；进入事件为 `None`。
    pub checklist: Option<RecoveryExitChecklist>,
}

/// 解除尝试的内部判定结果：同一次判定既决定返回值也决定审计结果码。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitOutcome {
    Ok,
    ChecklistIncomplete,
    InvalidState,
    WindowNotOpen,
}

impl ExitOutcome {
    const fn audit_outcome(self) -> &'static str {
        match self {
            Self::Ok => RECOVERY_AUDIT_OUTCOME_OK,
            Self::ChecklistIncomplete => RECOVERY_AUDIT_OUTCOME_CHECKLIST_INCOMPLETE,
            Self::InvalidState => RECOVERY_AUDIT_OUTCOME_INVALID_STATE,
            Self::WindowNotOpen => RECOVERY_AUDIT_OUTCOME_WINDOW_NOT_OPEN,
        }
    }
}

/// 控制器内部状态；模式与进入时刻同锁读写，不会出现「模式已切换但进入时刻缺失」
/// 的中间态。
#[derive(Clone, Copy, Debug)]
struct RecoveryState {
    mode: RecoveryMode,
    /// 进入受控恢复模式的时刻（`Instant`，单调钟，不受墙钟回拨影响）。
    /// `Normal` 时为 `None`。
    entered_at: Option<Instant>,
}

impl Default for RecoveryState {
    fn default() -> Self {
        Self {
            mode: RecoveryMode::Normal,
            entered_at: None,
        }
    }
}

/// 进程内唯一的恢复模式真源。
///
/// 内部状态全在 `Arc`/`Mutex` 后：克隆句柄共享同一份模式，HTTP 探针与调度循环
/// 读到的永远是同一个当前快照。所有方法均为非阻塞纯内存操作（无 I/O），
/// 因此可以在每秒一跳的调度循环里同步查询。
#[derive(Debug, Default)]
pub struct RuntimeRecoveryController {
    state: Mutex<RecoveryState>,
    retention: RecoveryRetentionPolicy,
    /// 有界去敏审计账本（最旧先出）；与 `state` 是两把独立的锁，`audit` 锁内不做任何
    /// 其它锁的获取，不存在锁序问题。
    audit: Mutex<VecDeque<RecoveryAuditEvent>>,
}

impl RuntimeRecoveryController {
    /// 以 [`RecoveryMode::Normal`] 启动，使用默认保留观察窗策略。
    pub fn new() -> Self {
        Self::with_retention_policy(RecoveryRetentionPolicy::default())
    }

    /// 显式指定保留观察窗策略（0，即解除前必须度满的观察窗）。
    pub fn with_retention_policy(retention: RecoveryRetentionPolicy) -> Self {
        Self {
            state: Mutex::new(RecoveryState {
                mode: RecoveryMode::Normal,
                entered_at: None,
            }),
            retention,
            audit: Mutex::new(VecDeque::new()),
        }
    }

    /// 当前模式的真实快照（读取侧无副作用）。
    pub fn current_mode(&self) -> RecoveryMode {
        self.lock().mode
    }

    /// 当前模式是否放行自动调度派发。受控恢复模式下恒 `false`
    /// （`sched_task` 以此跳过派发，05 §9「暂停自动准入/变更调度」）。
    pub fn allows_scheduling(&self) -> bool {
        self.permits(RecoveryOperation::AutomaticScheduling)
    }

    /// 当前模式是否放行指定操作。正常模式全量放行；受控恢复模式只放行管理写。
    pub fn permits(&self, operation: RecoveryOperation) -> bool {
        match self.current_mode() {
            RecoveryMode::Normal => true,
            RecoveryMode::ControlledRecovery => operation.is_admin_write(),
        }
    }

    /// 受控恢复模式的进入时刻（单调钟）。正常模式返回 `None`。
    pub fn entered_at(&self) -> Option<Instant> {
        self.lock().entered_at
    }

    /// 进入（或保持在）受控恢复模式。幂等：已在受控模式时保持原进入时刻不变，
    /// 不会因重复进入而重置观察窗。
    ///
    /// 本入口**不触碰传输线**（调度线/启动路径的进入语义），因此审计记录的
    /// `sessions_revoked` 恒为 `0`；管理端点要走会话吊销，用
    /// [`Self::enter_controlled_recovery_with_revocation`]。
    pub fn enter_controlled_recovery(&self) -> RecoveryMode {
        let mode = self.transition_into_recovery();
        self.record_audit(RecoveryAuditEvent {
            event: RECOVERY_AUDIT_ENTER,
            outcome: RECOVERY_AUDIT_OUTCOME_OK,
            mode: mode.as_str(),
            sessions_revoked: 0,
            checklist: None,
        });
        mode
    }

    /// 管理端点用的进入入口（05 §9「进入受控模式后撤销会话、暂停自动准入/变更调度」）：
    /// 先切模式（模式即刻阻止后续派发与变更任务），再吊销传输线上的全部活跃会话。
    ///
    /// - `transport = None`：本装配没有传输线（无隧道/纯只读装配），语义退化为
    ///   [`Self::enter_controlled_recovery`]，审计记 `sessions_revoked = 0`；
    /// - `transport = Some(m)`：遍历 `m.active_devices()` 逐个 `apply_revoke`，它同时
    ///   终止活跃会话并取消该公钥的全部挂起握手；返回**被终止活跃会话**的公钥数，
    ///   计入审计的 `sessions_revoked`。
    ///
    /// 顺序是「先切模式、后吊销」：吊销期间新登记的会话即便漏网，也已被模式闸门挡在
    /// 调度与变更任务之外，不会因为吊销窗口而放开写。吊销是内存内操作，不产生失败分支，
    /// 因此本方法不返回 `Result`。
    pub fn enter_controlled_recovery_with_revocation(
        &self,
        transport: Option<&TransportManager>,
    ) -> RecoveryMode {
        let mode = self.transition_into_recovery();
        let sessions_revoked = transport
            .map(revoke_all_active_sessions)
            .unwrap_or_default();
        self.record_audit(RecoveryAuditEvent {
            event: RECOVERY_AUDIT_ENTER,
            outcome: RECOVERY_AUDIT_OUTCOME_OK,
            mode: mode.as_str(),
            sessions_revoked,
            checklist: None,
        });
        mode
    }

    /// 管理员退出恢复模式（05 §9 唯一解除路径）。解除必须同时满足：
    /// 1. 当前确实处于受控恢复模式，否则 `InvalidState`；
    /// 2. [`RecoveryExitChecklist`] 四项全部真实核对，否则 `PermissionDenied`
    ///    （调用方是已认证管理员，语义是「解除门禁未满足」）；
    /// 3. 已度满保留观察窗（[`RuntimeRecoveryController::retention`]），否则
    ///    `InvalidState`（观察窗未结束，过早解除）。
    ///
    /// 成功即切换回 [`RecoveryMode::Normal`] 并返回新模式；无论成功或被拒都留下
    /// 一条去敏审计记录（拒绝原因逐字见 `RECOVERY_AUDIT_OUTCOME_*`）。
    pub fn exit_controlled_recovery(
        &self,
        checklist: &RecoveryExitChecklist,
    ) -> Result<RecoveryMode, ControllerError> {
        let (outcome, mode) = self.transition_out_of_recovery(checklist);
        self.record_audit(RecoveryAuditEvent {
            event: RECOVERY_AUDIT_EXIT,
            outcome: outcome.audit_outcome(),
            mode: mode.as_str(),
            sessions_revoked: 0,
            checklist: Some(*checklist),
        });
        match outcome {
            ExitOutcome::Ok => Ok(mode),
            ExitOutcome::ChecklistIncomplete => Err(ControllerError::PermissionDenied),
            ExitOutcome::InvalidState | ExitOutcome::WindowNotOpen => {
                Err(ControllerError::InvalidState)
            }
        }
    }

    /// 本控制器使用的保留观察窗策略。
    pub fn retention(&self) -> &RecoveryRetentionPolicy {
        &self.retention
    }

    /// 去敏审计账本的当前快照（最旧在前，上限 [`MAX_RECOVERY_AUDIT_EVENTS`] 条）。
    pub fn audit_events(&self) -> Vec<RecoveryAuditEvent> {
        self.audit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// 模式迁移的唯一落点：置模式与进入时刻，返回迁移后的模式（不含审计）。
    fn transition_into_recovery(&self) -> RecoveryMode {
        let mut state = self.lock();
        if state.entered_at.is_none() {
            state.entered_at = Some(Instant::now());
        }
        state.mode = RecoveryMode::ControlledRecovery;
        state.mode
    }

    /// 解除判定的唯一落点：在同一把锁内完成「模式 → 核对清单 → 观察窗」三段门禁，
    /// 返回判定结果与判定后的模式，供调用方映射错误码与审计。
    fn transition_out_of_recovery(
        &self,
        checklist: &RecoveryExitChecklist,
    ) -> (ExitOutcome, RecoveryMode) {
        let mut state = self.lock();
        if state.mode != RecoveryMode::ControlledRecovery {
            return (ExitOutcome::InvalidState, state.mode);
        }
        if !checklist.is_complete() {
            return (ExitOutcome::ChecklistIncomplete, state.mode);
        }
        let within_window = state
            .entered_at
            .is_some_and(|entered_at| self.retention.observation_window_open(entered_at));
        if !within_window {
            return (ExitOutcome::WindowNotOpen, state.mode);
        }
        state.mode = RecoveryMode::Normal;
        state.entered_at = None;
        (ExitOutcome::Ok, state.mode)
    }

    /// 追加一条去敏审计记录；超出上界即挤掉最旧一条（账本有界，绝不无界增长）。
    fn record_audit(&self, event: RecoveryAuditEvent) {
        let mut audit = self
            .audit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        audit.push_back(event);
        while audit.len() > MAX_RECOVERY_AUDIT_EVENTS {
            audit.pop_front();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RecoveryState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            // 临界区只有字段读写，无用户代码可 panic；中毒即状态仍一致，直接续用。
            poisoned.into_inner()
        })
    }
}

/// 吊销传输线上的全部活跃会话，返回被终止活跃会话的公钥数。
///
/// 判据是 [`TransportManager::active_devices`]（当前 ready 且未排空的公钥集合，稳定
/// 排序），对每个公钥执行 [`TransportManager::apply_revoke`]：同一把写锁内终止活跃
/// 会话、取消该公钥的全部挂起握手并关闭事件闸门，因此吊销后到达的帧不会复活会话。
/// 计入口径是 `RevokeOutcome::terminated_session`，即**确实切断了活跃会话**的公钥数；
/// 只取消挂起握手的公钥不计（它不是一条被切断的会话）。
fn revoke_all_active_sessions(transport: &TransportManager) -> usize {
    transport
        .active_devices()
        .into_iter()
        .filter(|public_key| transport.apply_revoke(*public_key).terminated_session)
        .count()
}

impl RecoveryModeProbe for RuntimeRecoveryController {
    /// 探针读取的就是本控制器的当前真实快照：控制器已装配即 `ready = true`，
    /// `mode` 逐字来自 [`RecoveryMode::as_str`]。这是「Runtime 真实控制器」的
    /// 装配形态，不是常量兜底：模式切换后本读数立即随之改变。
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
        let mode = RuntimeRecoveryController::current_mode(self);
        Box::pin(async move {
            Ok(RecoveryView {
                ready: true,
                mode: mode.as_str(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_strings_are_the_wire_contract() {
        assert_eq!(RecoveryMode::Normal.as_str(), "normal");
        assert_eq!(
            RecoveryMode::ControlledRecovery.as_str(),
            "controlled_recovery"
        );
    }

    #[test]
    fn controlled_recovery_blocks_dispatch_allows_admin_write() {
        let controller = RuntimeRecoveryController::new();
        assert_eq!(controller.current_mode(), RecoveryMode::Normal);
        assert!(controller.allows_scheduling());

        controller.enter_controlled_recovery();
        assert_eq!(controller.current_mode(), RecoveryMode::ControlledRecovery);
        assert!(!controller.allows_scheduling());
        assert!(!controller.permits(RecoveryOperation::MutationTask));
        for admin_write in [
            RecoveryOperation::AdminLogin,
            RecoveryOperation::PasswordChange,
            RecoveryOperation::Logout,
            RecoveryOperation::AuditWrite,
            RecoveryOperation::DeviceReadOnlyVerify,
            RecoveryOperation::ExitRecovery,
        ] {
            assert!(
                controller.permits(admin_write),
                "管理写 {admin_write:?} 必须放行"
            );
        }
    }

    #[test]
    fn exit_requires_complete_checklist() {
        let controller = RuntimeRecoveryController::new();
        controller.enter_controlled_recovery();
        let incomplete = RecoveryExitChecklist {
            revocations_checked: true,
            grants_checked: true,
            passwords_checked: true,
            tasks_checked: false,
        };
        assert!(matches!(
            controller.exit_controlled_recovery(&incomplete),
            Err(ControllerError::PermissionDenied)
        ));
        assert_eq!(
            controller.current_mode(),
            RecoveryMode::ControlledRecovery,
            "核对清单不完整不得解除"
        );
        let complete = RecoveryExitChecklist {
            revocations_checked: true,
            grants_checked: true,
            passwords_checked: true,
            tasks_checked: true,
        };
        assert!(matches!(
            controller.exit_controlled_recovery(&complete),
            Ok(RecoveryMode::Normal)
        ));
        assert!(controller.allows_scheduling());
    }

    #[test]
    fn exit_outside_recovery_mode_is_invalid_state() {
        let controller = RuntimeRecoveryController::new();
        let complete = RecoveryExitChecklist {
            revocations_checked: true,
            grants_checked: true,
            passwords_checked: true,
            tasks_checked: true,
        };
        assert!(matches!(
            controller.exit_controlled_recovery(&complete),
            Err(ControllerError::InvalidState)
        ));
    }

    /// 未装配传输线（`transport = None`）：进入语义退化为纯模式切换，审计记
    /// `sessions_revoked = 0`（不伪造吊销）。
    #[test]
    fn enter_with_revocation_without_transport_switches_mode_only() {
        let controller = RuntimeRecoveryController::new();
        assert_eq!(
            controller.enter_controlled_recovery_with_revocation(None),
            RecoveryMode::ControlledRecovery
        );
        assert_eq!(controller.current_mode(), RecoveryMode::ControlledRecovery);
        assert!(!controller.allows_scheduling());
        let events = controller.audit_events();
        assert_eq!(events.len(), 1, "一次进入恰好一条审计");
        assert_eq!(events[0].event, RECOVERY_AUDIT_ENTER);
        assert_eq!(events[0].outcome, RECOVERY_AUDIT_OUTCOME_OK);
        assert_eq!(events[0].mode, "controlled_recovery");
        assert_eq!(events[0].sessions_revoked, 0);
        assert!(events[0].checklist.is_none());
    }

    /// 重复进入不重置观察窗，但每次都留审计：`entered_at` 保持首次值。
    #[test]
    fn repeated_enter_keeps_the_first_observation_window_start() {
        let controller = RuntimeRecoveryController::new();
        controller.enter_controlled_recovery_with_revocation(None);
        let first = controller.entered_at().expect("进入时刻必须记录");
        controller.enter_controlled_recovery_with_revocation(None);
        assert_eq!(
            controller.entered_at(),
            Some(first),
            "重复进入不得重置观察窗"
        );
        assert_eq!(controller.audit_events().len(), 2);
    }

    /// 审计逐字记录被拒的解除原因：清单不齐、观察窗未满、非受控模式三种拒绝与成功
    /// 路径的 `outcome`/`mode` 必须可区分，且拒绝不改变模式。
    #[test]
    fn audit_records_which_exit_gate_rejected_the_attempt() {
        let complete = RecoveryExitChecklist {
            revocations_checked: true,
            grants_checked: true,
            passwords_checked: true,
            tasks_checked: true,
        };
        let incomplete = RecoveryExitChecklist {
            tasks_checked: false,
            ..complete
        };

        // 1. 非受控模式：invalid_state。
        let idle = RuntimeRecoveryController::new();
        assert!(idle.exit_controlled_recovery(&complete).is_err());
        let events = idle.audit_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event, RECOVERY_AUDIT_EXIT);
        assert_eq!(events[0].outcome, RECOVERY_AUDIT_OUTCOME_INVALID_STATE);
        assert_eq!(events[0].mode, "normal");
        assert_eq!(events[0].checklist, Some(complete));

        // 2. 清单不齐：checklist_incomplete，模式保持受控。
        let controller = RuntimeRecoveryController::new();
        controller.enter_controlled_recovery_with_revocation(None);
        assert!(matches!(
            controller.exit_controlled_recovery(&incomplete),
            Err(ControllerError::PermissionDenied)
        ));
        let events = controller.audit_events();
        assert_eq!(
            events[1].outcome,
            RECOVERY_AUDIT_OUTCOME_CHECKLIST_INCOMPLETE
        );
        assert_eq!(events[1].mode, "controlled_recovery");
        assert_eq!(events[1].checklist, Some(incomplete));

        // 3. 观察窗未满：observation_window_open；度满后成功切回 normal。
        let windowed = RuntimeRecoveryController::with_retention_policy(
            RecoveryRetentionPolicy::after_entry(std::time::Duration::from_secs(3_600)),
        );
        windowed.enter_controlled_recovery_with_revocation(None);
        assert!(matches!(
            windowed.exit_controlled_recovery(&complete),
            Err(ControllerError::InvalidState)
        ));
        let events = windowed.audit_events();
        assert_eq!(events[1].outcome, RECOVERY_AUDIT_OUTCOME_WINDOW_NOT_OPEN);
        assert_eq!(events[1].mode, "controlled_recovery");
        assert_eq!(windowed.current_mode(), RecoveryMode::ControlledRecovery);

        let open = RuntimeRecoveryController::new();
        open.enter_controlled_recovery_with_revocation(None);
        assert!(matches!(
            open.exit_controlled_recovery(&complete),
            Ok(RecoveryMode::Normal)
        ));
        let events = open.audit_events();
        assert_eq!(events[1].outcome, RECOVERY_AUDIT_OUTCOME_OK);
        assert_eq!(events[1].mode, "normal");
    }

    /// 审计账本有界：超出上限只保留最近 [`MAX_RECOVERY_AUDIT_EVENTS`] 条（最旧被挤掉），
    /// 不会因反复进出无界增长。
    #[test]
    fn audit_ledger_is_bounded_to_the_most_recent_events() {
        let controller = RuntimeRecoveryController::new();
        for _ in 0..(MAX_RECOVERY_AUDIT_EVENTS + 8) {
            controller.enter_controlled_recovery();
        }
        let events = controller.audit_events();
        assert_eq!(events.len(), MAX_RECOVERY_AUDIT_EVENTS);
        assert!(
            events
                .iter()
                .all(|event| event.event == RECOVERY_AUDIT_ENTER),
            "保留的记录必须都是最近的进入事件"
        );
    }
}

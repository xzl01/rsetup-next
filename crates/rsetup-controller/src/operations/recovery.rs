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

use std::sync::Mutex;
use std::time::Instant;

use crate::api::ManagementFuture;
use crate::api::system::{RecoveryModeProbe, RecoveryView};
use crate::error::ControllerError;

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
    pub fn enter_controlled_recovery(&self) -> RecoveryMode {
        let mut state = self.lock();
        if state.entered_at.is_none() {
            state.entered_at = Some(Instant::now());
        }
        state.mode = RecoveryMode::ControlledRecovery;
        state.mode
    }

    /// 管理员退出恢复模式（05 §9 唯一解除路径）。解除必须同时满足：
    /// 1. 当前确实处于受控恢复模式，否则 `InvalidState`；
    /// 2. [`RecoveryExitChecklist`] 四项全部真实核对，否则 `PermissionDenied`
    ///    （调用方是已认证管理员，语义是「解除门禁未满足」）；
    /// 3. 已度满保留观察窗（[`RuntimeRecoveryController::retention`]），否则
    ///    `InvalidState`（观察窗未结束，过早解除）。
    ///
    /// 成功即切换回 [`RecoveryMode::Normal`] 并返回新模式。
    pub fn exit_controlled_recovery(
        &self,
        checklist: &RecoveryExitChecklist,
    ) -> Result<RecoveryMode, ControllerError> {
        let mut state = self.lock();
        if state.mode != RecoveryMode::ControlledRecovery {
            return Err(ControllerError::InvalidState);
        }
        if !checklist.is_complete() {
            return Err(ControllerError::PermissionDenied);
        }
        let within_window = state
            .entered_at
            .is_some_and(|entered_at| self.retention.observation_window_open(entered_at));
        if !within_window {
            return Err(ControllerError::InvalidState);
        }
        state.mode = RecoveryMode::Normal;
        state.entered_at = None;
        Ok(state.mode)
    }

    /// 本控制器使用的保留观察窗策略。
    pub fn retention(&self) -> &RecoveryRetentionPolicy {
        &self.retention
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, RecoveryState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            // 临界区只有字段读写，无用户代码可 panic；中毒即状态仍一致，直接续用。
            poisoned.into_inner()
        })
    }
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
}

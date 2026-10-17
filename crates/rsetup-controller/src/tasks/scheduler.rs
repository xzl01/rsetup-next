//! 调度准入纯逻辑（04 §5.1）。
//!
//! 本模块只交付**离线纯函数切片**：不依赖 `rsetup_protocol` typed wire DTO，
//! 不连数据库、不发送板端请求。`TaskScheduler<C: BoardClient, R: TaskRepository>`
//! 泛型与 `run_once` 属 `BLOCKED (awaiting BoardClient)` 的接线切片，归集成 owner，
//! 不在本文件定义，也不构造伪泛型桩。

use crate::tasks::model::{ReasonCode, SubTaskState};

/// 全局并发分派槽位上限（05 §5 待审默认）。
pub const GLOBAL_DISPATCH_SLOTS: usize = 16;
/// 队列期限 TTL（05 §5 待审默认）。
pub const QUEUE_TTL_MS: i64 = 60 * 60 * 1000;
/// 观测窗口（04 §5.3 待审建议）。
pub const OBSERVE_WINDOW_MS: i64 = 5 * 60 * 1000;

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
}

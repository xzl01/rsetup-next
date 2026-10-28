//! 冷启动受控恢复模式判定与控制器构建。
//!
//! 在环境配置要求进入受控恢复模式时，保证在返回恢复控制器实例前即进入受控模式，
//! 杜绝在正常模式与受控模式切换之间的竞态窗口。

use std::sync::Arc;

use rsetup_controller::operations::RuntimeRecoveryController;

/// 冷启动进入受控恢复模式的外部环境变量（05 §9）。
///
/// 旧备份恢复后需要由运维**显式**让进程以受控模式启动（撤销会话、暂停自动
/// 准入/变更调度、禁止变更任务提交），再经管理员逐项核对解除。取值 `"1"` 或
/// `"true"`（大小写不敏感、首尾空白忽略）即触发；未设置或其他取值都是正常启动，
/// **不做任何猜测式推断**（不因为「刚从备份恢复」就自动进入）。
const CONTROLLED_RECOVERY_ENV: &str = "CONTROLLER_CONTROLLED_RECOVERY";

/// 外部环境变量是否明确要求冷启动进入受控恢复模式。
///
/// 只读一个已知键、只认两个已知取值；非 UTF-8 的取值一律按「未要求」处理，
/// 不回显原始取值（可能含非预期内容），日志只打固定的去敏文案。
fn controlled_recovery_requested() -> bool {
    controlled_recovery_flag(std::env::var_os(CONTROLLED_RECOVERY_ENV).as_deref())
}

/// 对单个环境变量原始取值求值：仅 `"1"`/`"true"`（大小写不敏感、忽略首尾空白）
/// 为真。非 UTF-8 取值与未设置同为假，不回显原值。
fn controlled_recovery_flag(raw: Option<&std::ffi::OsStr>) -> bool {
    raw.is_some_and(|raw| {
        raw.to_str()
            .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"))
    })
}

/// Runtime 恢复控制器真源的生产装配形态（05 §9 / 04 §5.2）。
///
/// [`RecoveryModeProbe`] 的契约要求 `ready`/`mode` 必须来自 Runtime 当前真实快照：
/// [`RuntimeRecoveryController`] 就是这份快照本身（进程内唯一实例，正常模式启动，
/// 管理员可进入/解除受控恢复模式），绝不返回伪造取值，也不造第二套恢复模式真源。
/// [`TransportManager::current_mode`] 只委托到本装配位。
///
/// 冷启动触发：[`CONTROLLED_RECOVERY_ENV`] 置 `"1"`/`"true"` 时，本函数在返回句柄
/// **之前**就调用 [`RuntimeRecoveryController::enter_controlled_recovery`]，因此
/// transport 探针、`/system/status`、调度循环与任务提交闸门自启动第一刻起读到的
/// 都是 `controlled_recovery`——不存在「先以正常模式对外服务、再补切模式」的窗口。
pub(super) fn build_recovery_controller() -> Arc<RuntimeRecoveryController> {
    new_recovery_controller(controlled_recovery_requested())
}

/// 装配形态的可测内核：`controlled_recovery = true` 即在返回前进入受控恢复模式。
fn new_recovery_controller(controlled_recovery: bool) -> Arc<RuntimeRecoveryController> {
    let controller = Arc::new(RuntimeRecoveryController::new());
    if controlled_recovery {
        controller.enter_controlled_recovery();
        eprintln!("controller 启动处于受控恢复模式 (CONTROLLER_CONTROLLED_RECOVERY=1)");
    }
    controller
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 冷启动受控恢复触发器（05 §9）：环境变量真值只认 `"1"`/`"true"`（大小写不敏感、
    /// 忽略首尾空白），未设置/其他取值/非 UTF-8 一律按正常启动，不做猜测式推断。
    #[test]
    fn controlled_recovery_flag_only_accepts_one_and_true() {
        for raw in ["1", "true", "TRUE", "True", " 1 ", "1 ", "\ttrue\n"] {
            assert!(
                controlled_recovery_flag(Some(std::ffi::OsStr::new(raw))),
                "{raw:?} 应触发受控恢复模式"
            );
        }
        for raw in ["", "0", "false", "no", "10", "true1", " "] {
            assert!(
                !controlled_recovery_flag(Some(std::ffi::OsStr::new(raw))),
                "{raw:?} 不应触发受控恢复模式"
            );
        }
        assert!(!controlled_recovery_flag(None), "未设置即正常启动");
        // 非 UTF-8 取值（Unix）按「未要求」处理，绝不解码后回显。
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let raw = std::ffi::OsStr::from_bytes(&[0xFF, b'1']);
            assert!(!controlled_recovery_flag(Some(raw)));
        }
    }

    /// 装配内核必须在返回句柄**之前**进入受控模式：探针、调度循环与提交闸门读到的
    /// 是同一个实例，启动第一刻即为 `controlled_recovery`。
    #[test]
    fn new_recovery_controller_enters_controlled_recovery_before_returning() {
        use rsetup_controller::operations::RecoveryOperation;

        let triggered = new_recovery_controller(true);
        assert_eq!(
            triggered.current_mode(),
            rsetup_controller::operations::RecoveryMode::ControlledRecovery
        );
        assert!(!triggered.allows_scheduling());
        assert!(!triggered.permits(RecoveryOperation::MutationTask));
        assert!(
            triggered.permits(RecoveryOperation::AdminLogin),
            "受控模式必须放行管理写"
        );
        assert!(triggered.entered_at().is_some(), "进入时刻必须就位");

        let plain = new_recovery_controller(false);
        assert_eq!(
            plain.current_mode(),
            rsetup_controller::operations::RecoveryMode::Normal
        );
        assert!(plain.allows_scheduling());
        assert!(plain.permits(RecoveryOperation::MutationTask));
    }
}

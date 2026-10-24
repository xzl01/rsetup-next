//! 受控恢复模式与生产恢复模式探针（05 §9 / 04 §5.2）。
//!
//! 证明的核心行为：
//! - 生产 [`RuntimeRecoveryController`] 作为 [`RecoveryModeProbe`] 装配后，探针
//!   `ready = true`，`mode` 逐字为 `"normal"` / `"controlled_recovery"`；
//! - 模式切换立即反映到探针读数（同一实例的唯一真源，不是常量兜底）；
//! - 受控恢复模式下阻止自动调度与变更任务，放行管理写
//!   （登录、改密、注销、审计、单设备只读核验、管理员退出恢复模式）；
//! - 调度循环的闸门语义：`allows_scheduling()` 在受控模式下恒 `false`
//!   （`main.rs` 的 `sched_task` 每秒以该同步闸门跳过派发）；
//! - 解除受控模式必须四项核对清单齐备，且（配置了观察窗时）度满保留观察窗。
//!
//! 全部为纯内存测试：不触数据库、网络或板端。

use std::sync::Arc;
use std::time::Duration;

use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
use rsetup_controller::operations::{
    RecoveryExitChecklist, RecoveryMode, RecoveryOperation, RecoveryRetentionPolicy,
    RuntimeRecoveryController,
};

fn complete_checklist() -> RecoveryExitChecklist {
    RecoveryExitChecklist {
        revocations_checked: true,
        grants_checked: true,
        passwords_checked: true,
        tasks_checked: true,
    }
}

async fn probe_view(controller: &Arc<RuntimeRecoveryController>) -> RecoveryView {
    // 经 `Arc<dyn RecoveryModeProbe>` 动态分发调用：与生产装配
    // （`build_transport_manager` / `SystemApiState::recovery_probe`）完全同形。
    let probe: Arc<dyn RecoveryModeProbe> = Arc::clone(controller) as Arc<dyn RecoveryModeProbe>;
    probe
        .current_mode()
        .await
        .expect("已装配的控制器探针不得失败")
}

#[tokio::test]
async fn production_probe_is_ready_and_reports_normal_mode() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    let view = probe_view(&controller).await;
    assert!(view.ready, "已装配的 Runtime 恢复控制器即 ready");
    assert_eq!(view.mode, "normal");
}

#[tokio::test]
async fn probe_reports_controlled_recovery_mode_after_entry() {
    let controller = Arc::new(RuntimeRecoveryController::new());
    assert_eq!(
        controller.enter_controlled_recovery(),
        RecoveryMode::ControlledRecovery
    );
    let view = probe_view(&controller).await;
    assert!(view.ready);
    assert_eq!(view.mode, "controlled_recovery");

    // 管理员按核对清单解除后，同一探针实例的读数立即回到 normal。
    controller
        .exit_controlled_recovery(&complete_checklist())
        .expect("核对清单齐备且无观察窗时应可解除");
    let view = probe_view(&controller).await;
    assert!(view.ready);
    assert_eq!(view.mode, "normal");
}

#[tokio::test]
async fn controlled_recovery_blocks_scheduling_and_mutation_allows_admin_write() {
    let controller = RuntimeRecoveryController::new();
    assert!(controller.allows_scheduling(), "正常模式必须放行自动调度");
    assert!(controller.permits(RecoveryOperation::MutationTask));

    controller.enter_controlled_recovery();
    assert!(
        !controller.allows_scheduling(),
        "受控恢复模式必须阻止自动调度派发"
    );
    assert!(
        !controller.permits(RecoveryOperation::MutationTask),
        "受控恢复模式必须阻止变更任务"
    );
    // 管理写全部放行（05 §9 明确允许的恢复期操作）。
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
            "受控恢复模式必须放行管理写 {admin_write:?}"
        );
    }
}

#[tokio::test]
async fn scheduling_gate_follows_mode_on_shared_controller() {
    // 调度循环与探针共享同一个 Arc 实例：进入受控模式后，任何持有该句柄的
    // 调度轮询（sched_task）当轮即被闸住；解除后立即恢复。
    let controller = Arc::new(RuntimeRecoveryController::new());
    let scheduler_side = Arc::clone(&controller);

    controller.enter_controlled_recovery();
    assert!(
        !scheduler_side.allows_scheduling(),
        "同一实例上调度侧必须看到受控模式"
    );
    let view = probe_view(&controller).await;
    assert_eq!(view.mode, "controlled_recovery");

    controller
        .exit_controlled_recovery(&complete_checklist())
        .expect("解除恢复模式");
    assert!(scheduler_side.allows_scheduling(), "解除后调度必须恢复放行");
    assert_eq!(probe_view(&controller).await.mode, "normal");
}

#[tokio::test]
async fn exit_requires_full_checklist() {
    let controller = RuntimeRecoveryController::new();
    controller.enter_controlled_recovery();
    for incomplete in [
        RecoveryExitChecklist::default(),
        RecoveryExitChecklist {
            tasks_checked: false,
            ..complete_checklist()
        },
        RecoveryExitChecklist {
            revocations_checked: false,
            ..complete_checklist()
        },
        RecoveryExitChecklist {
            grants_checked: false,
            ..complete_checklist()
        },
        RecoveryExitChecklist {
            passwords_checked: false,
            ..complete_checklist()
        },
    ] {
        assert!(
            controller.exit_controlled_recovery(&incomplete).is_err(),
            "核对清单不完整（{incomplete:?}）不得解除受控恢复模式"
        );
        assert_eq!(controller.current_mode(), RecoveryMode::ControlledRecovery);
    }
    assert!(
        matches!(
            controller.exit_controlled_recovery(&complete_checklist()),
            Ok(RecoveryMode::Normal)
        ),
        "核对清单齐备且无观察窗时应可解除"
    );
}

#[tokio::test]
async fn retention_window_gates_exit_until_elapsed() {
    let controller = RuntimeRecoveryController::with_retention_policy(
        RecoveryRetentionPolicy::after_entry(Duration::from_millis(50)),
    );
    controller.enter_controlled_recovery();
    // 进入后立刻解除：观察窗未度满，即使清单齐备也不放行。
    assert!(
        controller
            .exit_controlled_recovery(&complete_checklist())
            .is_err(),
        "保留观察窗未结束不得解除受控恢复模式"
    );
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        matches!(
            controller.exit_controlled_recovery(&complete_checklist()),
            Ok(RecoveryMode::Normal)
        ),
        "度满观察窗且清单齐备后应可解除"
    );
    // 退出后再解除：不在受控模式，InvalidState（幂等拒绝）。
    assert!(
        controller
            .exit_controlled_recovery(&complete_checklist())
            .is_err()
    );
}

#[tokio::test]
async fn entering_recovery_is_idempotent_for_observation_window() {
    let controller = RuntimeRecoveryController::with_retention_policy(
        RecoveryRetentionPolicy::after_entry(Duration::from_secs(3_600)),
    );
    controller.enter_controlled_recovery();
    let first = controller.entered_at();
    assert!(first.is_some(), "进入受控模式必须记录进入时刻");
    // 重复进入不得重置观察窗（否则核对永远无法完成）。
    controller.enter_controlled_recovery();
    assert_eq!(controller.entered_at(), first);
    assert!(
        controller
            .exit_controlled_recovery(&complete_checklist())
            .is_err(),
        "一小时观察窗内不得解除"
    );
}

#[test]
fn mode_strings_are_the_wire_contract() {
    assert_eq!(RecoveryMode::Normal.as_str(), "normal");
    assert_eq!(
        RecoveryMode::ControlledRecovery.as_str(),
        "controlled_recovery"
    );
}

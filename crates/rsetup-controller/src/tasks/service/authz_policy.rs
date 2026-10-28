//! 目标授权三态（RT-N6：可见性与 reboot 权限解耦）纯策略。
//!
//! 包含 `TargetAuthz` 三态枚举、对 Identity 快照求设备有效权限集
//! `snapshot_effective_permissions`、以及提交与取消路径的唯一授权判定实现
//! `check_target_authz`。

use std::collections::HashSet;

use uuid::Uuid;

use crate::authz::policy::{Permission, eval_memory_permissions};
use crate::authz::sqlx_repo::AuthorizationSnapshot;

#[cfg(test)]
mod tests;

/// 目标授权三态（RT-N6：可见性与 reboot 权限解耦）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetAuthz {
    /// 可见且持有 `Permission::DeviceReboot`：抢设备锁、创建 `queued` 子任务。
    Permitted,
    /// 可见但缺 `DeviceReboot`：**不**伪装成不可见、不抢锁，直接落 `failed` 子任务。
    VisibleForbidden,
    /// 完全不可见（无任何有效权限）：整体回滚 404，不泄露部分结果。
    NotVisible,
}

/// 对 Identity 真源快照求该设备的当前有效权限集：先排除 missing/archived 设备，
/// 再交 `eval_memory_permissions`（其内部已排除 inactive/must-change actor，
/// admin shortcut 亦按事务内新读 actor 行判定）。
pub fn snapshot_effective_permissions(
    snapshot: &AuthorizationSnapshot,
    device_id: [u8; 32],
) -> HashSet<Permission> {
    let Some(device) = snapshot.devices.get(&device_id) else {
        return HashSet::new();
    };
    if device.archived {
        return HashSet::new();
    }
    let empty_groups: HashSet<Uuid> = HashSet::new();
    let groups = snapshot
        .device_groups
        .get(&device_id)
        .unwrap_or(&empty_groups);
    eval_memory_permissions(
        &snapshot.actor,
        device_id,
        &snapshot.grants,
        &snapshot.roles,
        groups,
    )
}

/// 纯判定 seam：含 reboot → `Permitted`；有其他有效权限 → `VisibleForbidden`；
/// 空集 → `NotVisible`。提交路径的唯一授权判定实现。
pub(super) fn check_target_authz(
    snapshot: &AuthorizationSnapshot,
    device_id: [u8; 32],
) -> TargetAuthz {
    let effective = snapshot_effective_permissions(snapshot, device_id);
    if effective.contains(&Permission::DeviceReboot) {
        return TargetAuthz::Permitted;
    }
    if !effective.is_empty() {
        return TargetAuthz::VisibleForbidden;
    }
    TargetAuthz::NotVisible
}

/// 纯策略单测的布尔薄包装（`Permitted` 投影）；提交路径不消费本包装。
#[cfg(test)]
fn snapshot_allows_task_creation(snapshot: &AuthorizationSnapshot, device_id: [u8; 32]) -> bool {
    matches!(
        check_target_authz(snapshot, device_id),
        TargetAuthz::Permitted
    )
}

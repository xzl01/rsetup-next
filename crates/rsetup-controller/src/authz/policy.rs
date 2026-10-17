use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::ControllerError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "device.read")]
    DeviceRead,
    #[serde(rename = "device.status.read")]
    DeviceStatusRead,
    #[serde(rename = "device.reboot")]
    DeviceReboot,
    #[serde(rename = "device.task.read")]
    DeviceTaskRead,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeviceRead => "device.read",
            Self::DeviceStatusRead => "device.status.read",
            Self::DeviceReboot => "device.reboot",
            Self::DeviceTaskRead => "device.task.read",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "device.read" => Some(Self::DeviceRead),
            "device.status.read" => Some(Self::DeviceStatusRead),
            "device.reboot" => Some(Self::DeviceReboot),
            "device.task.read" => Some(Self::DeviceTaskRead),
            _ => None,
        }
    }
}

/// 角色权限列表门禁：非空且无重复。
pub fn validate_permission_list(permissions: &[Permission]) -> Result<(), ControllerError> {
    if permissions.is_empty() {
        return Err(ControllerError::InvalidArgument);
    }
    let mut seen: HashSet<&'static str> = HashSet::with_capacity(permissions.len());
    for permission in permissions {
        if !seen.insert(permission.as_str()) {
            return Err(ControllerError::InvalidArgument);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantSource {
    Role(Uuid),
    Direct(Vec<Permission>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantScope {
    All,
    Group(Uuid),
    Device([u8; 32]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRecord {
    pub id: Uuid,
    pub user_id: [u8; 16],
    pub source: GrantSource,
    pub scope: GrantScope,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleRecord {
    pub id: Uuid,
    pub name: String,
    pub builtin: bool,
    pub archived: bool,
    pub revision: u64,
    pub permissions: Vec<Permission>,
}

pub fn evaluate_grants(
    user_id: [u8; 16],
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
) -> HashSet<Permission> {
    let mut effective = HashSet::new();

    for grant in grants {
        if grant.user_id != user_id {
            continue;
        }

        let scope_matches = match &grant.scope {
            GrantScope::All => true,
            GrantScope::Group(gid) => device_groups.contains(gid),
            GrantScope::Device(did) => *did == device_id,
        };

        if !scope_matches {
            continue;
        }

        match &grant.source {
            GrantSource::Role(rid) => {
                if let Some(role) = roles.get(rid) {
                    if !role.archived {
                        effective.extend(role.permissions.iter().copied());
                    }
                }
            }
            GrantSource::Direct(perms) => {
                effective.extend(perms.iter().copied());
            }
        }
    }

    effective
}

pub fn minimum_device_projection(
    device_id: [u8; 32],
    display_name: &str,
    perms: &HashSet<Permission>,
) -> serde_json::Value {
    let mut perm_strings: Vec<&'static str> = perms.iter().map(|p| p.as_str()).collect();
    perm_strings.sort_unstable();

    serde_json::json!({
        "device_id": hex::encode(device_id),
        "display_name": display_name,
        "effective_permissions": perm_strings,
    })
}

/// 严格最后管理员规则：当前活跃管理员数 ≤ 1 时，任何"停用或降级活跃
/// 管理员"的操作必须拒绝。调用方必须只在 target 当前 `active && is_admin`
/// 且本次变更确实移除该状态时才调用本函数（见 Task 4 的事务契约）。
pub fn check_admin_demotion_allowed(current_admin_count: i64) -> Result<(), ControllerError> {
    if current_admin_count <= 1 {
        Err(ControllerError::LastAdmin)
    } else {
        Ok(())
    }
}

/// DB 当前 actor 的严格授权状态（同一读事务内读取；HTTP 层 `Session` 只是廉价前置过滤）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotActor {
    pub id: [u8; 16],
    pub active: bool,
    pub is_admin: bool,
    pub must_change_password: bool,
}

/// 封闭的 4 权限全集（规格 01 §1：管理员具有全局及全部设备权限）。
pub const ALL_DEVICE_PERMISSIONS: [Permission; 4] = [
    Permission::DeviceRead,
    Permission::DeviceStatusRead,
    Permission::DeviceReboot,
    Permission::DeviceTaskRead,
];

/// DB 当前管理员判定（admin shortcut 条件）：三个严格布尔全部满足。
pub fn is_current_admin(actor: &SnapshotActor) -> bool {
    actor.active && actor.is_admin && !actor.must_change_password
}

/// 目标设备的有效权限集（纯求值，离线 RED→GREEN 目标）。
/// 语义固定：`!active || must_change_password` → 空集（fail-closed，admin 未改密同样 0 权限）；
/// `active && is_admin` → `ALL_DEVICE_PERMISSIONS` 全量（admin shortcut，不看 grants）；
/// 普通用户 → `evaluate_grants(actor.id, device_id, grants, roles, device_groups)`，
/// 其中 `device_groups` 必须是**目标设备自身**所属 group 集（不得用 actor grant 的 group 全集）。
pub fn eval_memory_permissions(
    actor: &SnapshotActor,
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
) -> HashSet<Permission> {
    if !actor.active || actor.must_change_password {
        return HashSet::new();
    }
    if actor.is_admin {
        return HashSet::from(ALL_DEVICE_PERMISSIONS);
    }
    evaluate_grants(actor.id, device_id, grants, roles, device_groups)
}

/// 权限点来源类别：admin shortcut、角色 grant 或直接 grant。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionSourceKind {
    Admin,
    Role(Uuid),
    Direct(Uuid),
}

/// 单个权限点及其确定性归因来源。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionSourceEntry {
    pub permission: Permission,
    pub source: PermissionSourceKind,
}

/// 权限点来源归因（确定性）：admin shortcut → `Admin`；角色 grant → `Role(role_id)`；
/// 直接 grant → `Direct(grant_id)`。scope 匹配语义与 `evaluate_grants` 完全一致，
/// 输出按 permission 字典序 → kind 变体序 → id 字节序排序并去重。
pub fn permission_sources(
    user_id: [u8; 16],
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
    is_admin: bool,
) -> Vec<PermissionSourceEntry> {
    if is_admin {
        return ALL_DEVICE_PERMISSIONS
            .iter()
            .map(|permission| PermissionSourceEntry {
                permission: *permission,
                source: PermissionSourceKind::Admin,
            })
            .collect();
    }
    let mut entries = Vec::new();
    for grant in grants {
        if grant.user_id != user_id {
            continue;
        }
        let scope_matches = match &grant.scope {
            GrantScope::All => true,
            GrantScope::Group(gid) => device_groups.contains(gid),
            GrantScope::Device(did) => *did == device_id,
        };
        if !scope_matches {
            continue;
        }
        match &grant.source {
            GrantSource::Role(rid) => {
                if let Some(role) = roles.get(rid) {
                    if !role.archived {
                        for permission in &role.permissions {
                            entries.push(PermissionSourceEntry {
                                permission: *permission,
                                source: PermissionSourceKind::Role(*rid),
                            });
                        }
                    }
                }
            }
            GrantSource::Direct(perms) => {
                for permission in perms {
                    entries.push(PermissionSourceEntry {
                        permission: *permission,
                        source: PermissionSourceKind::Direct(grant.id),
                    });
                }
            }
        }
    }
    entries.sort_unstable_by(|a, b| {
        a.permission
            .as_str()
            .cmp(b.permission.as_str())
            .then_with(|| a.source.cmp(&b.source))
    });
    entries.dedup();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn role_permissions_never_cross_grant_scope_and_take_union() {
        let user = [1u8; 16];
        let dev_a = [0x0au8; 32];
        let dev_b = [0x0bu8; 32];
        let role_viewer_id = uuid::Uuid::new_v4();
        let role_operator_id = uuid::Uuid::new_v4();

        let mut roles = HashMap::new();
        roles.insert(
            role_viewer_id,
            RoleRecord {
                id: role_viewer_id,
                name: "viewer".into(),
                builtin: true,
                archived: false,
                revision: 1,
                permissions: vec![Permission::DeviceRead, Permission::DeviceStatusRead],
            },
        );
        roles.insert(
            role_operator_id,
            RoleRecord {
                id: role_operator_id,
                name: "operator".into(),
                builtin: true,
                archived: false,
                revision: 1,
                permissions: vec![Permission::DeviceReboot],
            },
        );

        // Grant 1: user has viewer role on Device A only
        // Grant 2: user has operator role on Device B only
        let grants = vec![
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Role(role_viewer_id),
                scope: GrantScope::Device(dev_a),
                revision: 1,
            },
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Role(role_operator_id),
                scope: GrantScope::Device(dev_b),
                revision: 1,
            },
        ];

        let perms_a = evaluate_grants(user, dev_a, &grants, &roles, &HashSet::new());
        assert!(perms_a.contains(&Permission::DeviceRead));
        assert!(perms_a.contains(&Permission::DeviceStatusRead));
        assert!(
            !perms_a.contains(&Permission::DeviceReboot),
            "Device A must not inherit Device B reboot role"
        );

        let perms_b = evaluate_grants(user, dev_b, &grants, &roles, &HashSet::new());
        assert!(perms_b.contains(&Permission::DeviceReboot));
        assert!(
            !perms_b.contains(&Permission::DeviceRead),
            "Device B must not inherit Device A viewer role"
        );
    }

    #[test]
    fn group_removal_preserves_independent_all_scope() {
        let user = [2u8; 16];
        let dev = [0x0cu8; 32];
        let group_id = uuid::Uuid::new_v4();

        let grants = vec![
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Direct(vec![Permission::DeviceRead]),
                scope: GrantScope::All,
                revision: 1,
            },
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Direct(vec![Permission::DeviceReboot]),
                scope: GrantScope::Group(group_id),
                revision: 1,
            },
        ];

        // Device is NOT in the group
        let no_groups = HashSet::new();
        let perms = evaluate_grants(user, dev, &grants, &HashMap::new(), &no_groups);
        assert_eq!(perms, HashSet::from([Permission::DeviceRead]));

        // Device IS in the group
        let mut in_group = HashSet::new();
        in_group.insert(group_id);
        let perms_in = evaluate_grants(user, dev, &grants, &HashMap::new(), &in_group);
        assert_eq!(
            perms_in,
            HashSet::from([Permission::DeviceRead, Permission::DeviceReboot])
        );
    }

    #[test]
    fn archived_role_or_missing_role_is_revoked_and_unauthorized_user_cannot_escalate() {
        let user = [3u8; 16];
        let attacker = [4u8; 16];
        let dev = [0x0du8; 32];
        let active_role_id = uuid::Uuid::new_v4();
        let archived_role_id = uuid::Uuid::new_v4();
        let missing_role_id = uuid::Uuid::new_v4();

        let mut roles = HashMap::new();
        roles.insert(
            active_role_id,
            RoleRecord {
                id: active_role_id,
                name: "active".into(),
                builtin: false,
                archived: false,
                revision: 1,
                permissions: vec![Permission::DeviceRead],
            },
        );
        roles.insert(
            archived_role_id,
            RoleRecord {
                id: archived_role_id,
                name: "archived".into(),
                builtin: false,
                archived: true,
                revision: 2,
                permissions: vec![Permission::DeviceReboot],
            },
        );

        let grants = vec![
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Role(active_role_id),
                scope: GrantScope::Device(dev),
                revision: 1,
            },
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Role(archived_role_id),
                scope: GrantScope::Device(dev),
                revision: 1,
            },
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Role(missing_role_id),
                scope: GrantScope::Device(dev),
                revision: 1,
            },
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Direct(vec![Permission::DeviceStatusRead]),
                scope: GrantScope::Device(dev),
                revision: 1,
            },
        ];

        // Valid user gets active role + direct perms, but NOT archived or missing role
        let perms = evaluate_grants(user, dev, &grants, &roles, &HashSet::new());
        assert_eq!(
            perms,
            HashSet::from([Permission::DeviceRead, Permission::DeviceStatusRead])
        );
        assert!(!perms.contains(&Permission::DeviceReboot));

        // Different user / attacker receives empty set (cannot escalate or inherit other users' grants)
        let attacker_perms = evaluate_grants(attacker, dev, &grants, &roles, &HashSet::new());
        assert!(attacker_perms.is_empty());
    }

    #[test]
    fn minimum_device_projection_formats_correctly() {
        let dev = [0xab; 32];
        let mut perms = HashSet::new();
        perms.insert(Permission::DeviceStatusRead);
        perms.insert(Permission::DeviceRead);

        let projection = minimum_device_projection(dev, "Sensor-01", &perms);
        let expected = serde_json::json!({
            "device_id": hex::encode(dev),
            "display_name": "Sensor-01",
            "effective_permissions": ["device.read", "device.status.read"],
        });
        assert_eq!(projection, expected);

        // Also test empty permissions
        let empty_projection = minimum_device_projection(dev, "Sensor-01", &HashSet::new());
        assert_eq!(
            empty_projection,
            serde_json::json!({
                "device_id": hex::encode(dev),
                "display_name": "Sensor-01",
                "effective_permissions": [],
            })
        );
    }

    #[test]
    fn permission_parse_and_serde() {
        assert_eq!(
            Permission::parse("device.read"),
            Some(Permission::DeviceRead)
        );
        assert_eq!(
            Permission::parse("device.status.read"),
            Some(Permission::DeviceStatusRead)
        );
        assert_eq!(
            Permission::parse("device.reboot"),
            Some(Permission::DeviceReboot)
        );
        assert_eq!(
            Permission::parse("device.task.read"),
            Some(Permission::DeviceTaskRead)
        );
        assert_eq!(Permission::parse("invalid.perm"), None);

        assert_eq!(Permission::DeviceRead.as_str(), "device.read");
        assert_eq!(Permission::DeviceStatusRead.as_str(), "device.status.read");
        assert_eq!(Permission::DeviceReboot.as_str(), "device.reboot");
        assert_eq!(Permission::DeviceTaskRead.as_str(), "device.task.read");

        // Serde roundtrip
        let json = serde_json::to_string(&Permission::DeviceReboot).unwrap();
        assert_eq!(json, "\"device.reboot\"");
        let deserialized: Permission = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, Permission::DeviceReboot);
    }

    #[test]
    fn last_admin_rule_rejects_demoting_the_only_active_admin() {
        assert!(matches!(
            super::check_admin_demotion_allowed(1),
            Err(crate::ControllerError::LastAdmin)
        ));
        assert!(matches!(
            super::check_admin_demotion_allowed(0),
            Err(crate::ControllerError::LastAdmin)
        ));
        assert!(super::check_admin_demotion_allowed(2).is_ok());
    }

    #[test]
    fn snapshot_eval_regular_user_group_scope_never_crosses_devices() {
        // 普通 group 范围不串设备：group 授权只覆盖**该设备自身所属**的 group——
        // device A 属于 G（有权限），device B 属于另一组（0 权限）；
        // 不得把 G 套到两个设备，也不得用 actor grant 的 group 全集。
        let group = uuid::Uuid::from_bytes([0x67; 16]);
        let actor = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: false,
            must_change_password: false,
        };
        let grants = vec![GrantRecord {
            id: uuid::Uuid::from_bytes([0x0a; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Direct(vec![Permission::DeviceRead]),
            scope: GrantScope::Group(group),
            revision: 1,
        }];
        let roles = HashMap::new();
        let mut groups_a = HashSet::new();
        groups_a.insert(group);
        let mut groups_b = HashSet::new();
        groups_b.insert(uuid::Uuid::from_bytes([0x68; 16]));
        assert_eq!(
            eval_memory_permissions(&actor, [0xa1; 32], &grants, &roles, &groups_a),
            HashSet::from([Permission::DeviceRead])
        );
        assert!(
            eval_memory_permissions(&actor, [0xa2; 32], &grants, &roles, &groups_b).is_empty(),
            "device B is not in G: the group grant must not cross devices"
        );
    }

    #[test]
    fn snapshot_eval_only_actor_own_grants_apply() {
        // 本人隔离：他人（即使 All scope 的 direct/reboot）grant 不给本 actor 任何权限。
        let actor = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: false,
            must_change_password: false,
        };
        let grants = vec![GrantRecord {
            id: uuid::Uuid::from_bytes([0x0b; 16]),
            user_id: [2u8; 16], // ← 他人的 grant
            source: GrantSource::Direct(vec![Permission::DeviceReboot]),
            scope: GrantScope::All,
            revision: 1,
        }];
        let roles = HashMap::new();
        let groups = HashSet::new();
        assert!(
            eval_memory_permissions(&actor, [0xb1; 32], &grants, &roles, &groups).is_empty(),
            "another user's grant must not grant permissions to this actor"
        );
    }

    #[test]
    fn snapshot_eval_admin_must_change_password_has_zero_permissions() {
        // 管理员当前改密：DB 当前 must_change_password 的 admin 仍是 0 权限（fail-closed）；
        // admin shortcut 只在 DB 当前 active && is_admin && !must_change_password 时全量。
        let blocked = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: true,
            must_change_password: true,
        };
        let inactive = SnapshotActor {
            id: [1u8; 16],
            active: false,
            is_admin: true,
            must_change_password: false,
        };
        let roles = HashMap::new();
        let groups = HashSet::new();
        assert!(eval_memory_permissions(&blocked, [0u8; 32], &[], &roles, &groups).is_empty());
        assert!(eval_memory_permissions(&inactive, [0u8; 32], &[], &roles, &groups).is_empty());
        let admin = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: true,
            must_change_password: false,
        };
        assert_eq!(
            eval_memory_permissions(&admin, [0u8; 32], &[], &roles, &groups),
            HashSet::from(ALL_DEVICE_PERMISSIONS)
        );
    }

    #[test]
    fn snapshot_eval_no_grants_regular_user_has_legal_empty_permissions() {
        // 无 grant 是合法空权限（不是错误、不是 503、不降级为"未知"）——
        // 与"引用污染"（503 Config）严格区分：污染在 build/decode 层拦，空集在求值层放行。
        let actor = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: false,
            must_change_password: false,
        };
        let roles = HashMap::new();
        let groups = HashSet::new();
        assert!(eval_memory_permissions(&actor, [0xc1; 32], &[], &roles, &groups).is_empty());
    }

    #[test]
    fn permission_sources_attributes_admin_role_and_direct_deterministically() {
        let user = [1u8; 16];
        let dev = [2u8; 32];
        // 管理员：每个权限的来源都是 Admin。
        let entries = permission_sources(user, dev, &[], &HashMap::new(), &HashSet::new(), true);
        assert_eq!(entries.len(), 4);
        assert!(
            entries
                .iter()
                .all(|e| matches!(e.source, PermissionSourceKind::Admin))
        );

        // 普通用户：归因到具体的 grant.id（direct）或 role_id（role）。
        let grant_id = uuid::Uuid::from_bytes([5u8; 16]);
        let direct = GrantRecord {
            id: grant_id,
            user_id: user,
            source: GrantSource::Direct(vec![Permission::DeviceRead]),
            scope: GrantScope::All,
            revision: 1,
        };
        let entries = permission_sources(
            user,
            dev,
            &[direct],
            &HashMap::new(),
            &HashSet::new(),
            false,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].permission, Permission::DeviceRead);
        assert_eq!(entries[0].source, PermissionSourceKind::Direct(grant_id));
    }
}

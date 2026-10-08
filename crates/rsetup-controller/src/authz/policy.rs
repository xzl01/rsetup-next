use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
        roles.insert(role_viewer_id, RoleRecord {
            id: role_viewer_id,
            name: "viewer".into(),
            builtin: true,
            archived: false,
            revision: 1,
            permissions: vec![Permission::DeviceRead, Permission::DeviceStatusRead],
        });
        roles.insert(role_operator_id, RoleRecord {
            id: role_operator_id,
            name: "operator".into(),
            builtin: true,
            archived: false,
            revision: 1,
            permissions: vec![Permission::DeviceReboot],
        });

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
        assert!(!perms_a.contains(&Permission::DeviceReboot), "Device A must not inherit Device B reboot role");

        let perms_b = evaluate_grants(user, dev_b, &grants, &roles, &HashSet::new());
        assert!(perms_b.contains(&Permission::DeviceReboot));
        assert!(!perms_b.contains(&Permission::DeviceRead), "Device B must not inherit Device A viewer role");
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
        assert_eq!(perms_in, HashSet::from([Permission::DeviceRead, Permission::DeviceReboot]));
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
        roles.insert(active_role_id, RoleRecord {
            id: active_role_id,
            name: "active".into(),
            builtin: false,
            archived: false,
            revision: 1,
            permissions: vec![Permission::DeviceRead],
        });
        roles.insert(archived_role_id, RoleRecord {
            id: archived_role_id,
            name: "archived".into(),
            builtin: false,
            archived: true,
            revision: 2,
            permissions: vec![Permission::DeviceReboot],
        });

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
        assert_eq!(perms, HashSet::from([Permission::DeviceRead, Permission::DeviceStatusRead]));
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
        assert_eq!(Permission::parse("device.read"), Some(Permission::DeviceRead));
        assert_eq!(Permission::parse("device.status.read"), Some(Permission::DeviceStatusRead));
        assert_eq!(Permission::parse("device.reboot"), Some(Permission::DeviceReboot));
        assert_eq!(Permission::parse("device.task.read"), Some(Permission::DeviceTaskRead));
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
}

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::codec::{read_error, uuid16};
use crate::authz::policy::{
    GrantRecord, GrantScope, GrantSource, Permission, RoleRecord, SnapshotActor,
};
use crate::authz::sqlx_repo::model::AuthorizationSnapshot;
use crate::devices::repository::DeviceAdminView;
use crate::error::ControllerError;
use crate::model::{AdmissionState, ReviewDecision};

pub fn decode_snapshot_actor(
    user_id: [u8; 16],
    row: Option<(i64, i64, i64)>,
) -> Result<SnapshotActor, ControllerError> {
    let (active, is_admin, must_change_password) = row.ok_or(ControllerError::PermissionDenied)?;
    Ok(SnapshotActor {
        id: user_id,
        active: crate::integrity::raw_bool(active, "snapshot.actor_active")
            .map_err(|_| read_error("snapshot.actor_active"))?,
        is_admin: crate::integrity::raw_bool(is_admin, "snapshot.actor_is_admin")
            .map_err(|_| read_error("snapshot.actor_is_admin"))?,
        must_change_password: crate::integrity::raw_bool(
            must_change_password,
            "snapshot.actor_must_change",
        )
        .map_err(|_| read_error("snapshot.actor_must_change"))?,
    })
}

pub fn decode_snapshot_roles(
    rows: &[(Vec<u8>, String, i64, i64, u64)],
    permissions: &[(Vec<u8>, String)],
) -> Result<HashMap<Uuid, RoleRecord>, ControllerError> {
    let mut perms_by_role: HashMap<Uuid, Vec<Permission>> = HashMap::new();
    for (role_id, name) in permissions {
        let rid = uuid16(role_id.clone(), "snapshot.role_permission")?;
        let permission =
            Permission::parse(name).ok_or_else(|| read_error("snapshot.role_permission"))?;
        perms_by_role.entry(rid).or_default().push(permission);
    }
    let mut roles: HashMap<Uuid, RoleRecord> = HashMap::new();
    for (id, name, builtin, archived, revision) in rows {
        let rid = uuid16(id.clone(), "snapshot.roles")?;
        if roles.contains_key(&rid) {
            return Err(read_error("snapshot.roles")); // 重复 role 行 = 污染
        }
        let is_archived = crate::integrity::raw_bool(*archived, "snapshot.role_bool")
            .map_err(|_| read_error("snapshot.role_bool"))?;
        let permissions = perms_by_role.remove(&rid).unwrap_or_default();
        // 归档合法清理role_permissions后保留父行；仅当前有效角色要求非空。
        if (!is_archived && permissions.is_empty())
            || permissions.iter().copied().collect::<HashSet<_>>().len() != permissions.len()
        {
            return Err(read_error("snapshot.role_permission")); // active 空权限或重复权限 = 污染
        }
        roles.insert(
            rid,
            RoleRecord {
                id: rid,
                name: name.clone(),
                builtin: crate::integrity::raw_bool(*builtin, "snapshot.role_bool")
                    .map_err(|_| read_error("snapshot.role_bool"))?,
                archived: is_archived,
                revision: *revision,
                permissions,
            },
        );
    }
    if !perms_by_role.is_empty() {
        return Err(read_error("snapshot.role_permission")); // dangling permission 行
    }
    Ok(roles)
}

pub fn decode_snapshot_group_rows(
    rows: &[(Vec<u8>, Vec<u8>, Option<i64>)],
) -> Result<HashMap<[u8; 32], HashSet<Uuid>>, ControllerError> {
    let mut out: HashMap<[u8; 32], HashSet<Uuid>> = HashMap::new();
    for (device_id, group_id, archived) in rows {
        let key: [u8; 32] = device_id
            .clone()
            .try_into()
            .map_err(|_| read_error("snapshot.device_id"))?;
        let gid = uuid16(group_id.clone(), "snapshot.group_missing")?;
        let archived = match archived {
            None => return Err(read_error("snapshot.group_missing")),
            Some(raw) => crate::integrity::raw_bool(*raw, "snapshot.group_archived")
                .map_err(|_| read_error("snapshot.group_archived"))?,
        };
        if !archived {
            out.entry(key).or_default().insert(gid);
        }
    }
    Ok(out)
}

/// 管理侧 `device_groups_for_device` 行解码（列序与 `DEVICE_GROUPS_FOR_DEVICE_SQL` 一致：
/// `group_id, group_exists, group_archived, device_exists, device_archived`）。
/// member 指向缺失/已归档的 device 行，或 group 行缺失，都是持久污染 → 固定去敏 `Config`
/// （503 形状），不得静默返回空集当"该设备无 group"；只有 group 归档才正常过滤。
pub fn decode_group_memberships_for_device(
    rows: &[(Vec<u8>, i64, i64, i64, i64)],
) -> Result<HashSet<Uuid>, ControllerError> {
    let mut out: HashSet<Uuid> = HashSet::new();
    for (group_id, group_exists, group_archived, device_exists, device_archived) in rows {
        let group_exists = crate::integrity::raw_bool(*group_exists, "groups.for_device")
            .map_err(|_| read_error("groups.for_device"))?;
        let group_archived = crate::integrity::raw_bool(*group_archived, "groups.for_device")
            .map_err(|_| read_error("groups.for_device"))?;
        let device_exists = crate::integrity::raw_bool(*device_exists, "groups.for_device")
            .map_err(|_| read_error("groups.for_device"))?;
        let device_archived = crate::integrity::raw_bool(*device_archived, "groups.for_device")
            .map_err(|_| read_error("groups.for_device"))?;
        if !group_exists || !device_exists {
            return Err(read_error("groups.for_device.dangling"));
        }
        if device_archived {
            return Err(read_error("groups.for_device.archived_device"));
        }
        if group_archived {
            continue;
        }
        out.insert(uuid16(group_id.clone(), "groups.for_device")?);
    }
    Ok(out)
}

pub fn decode_device_admin_view(
    public_key: Vec<u8>,
    display_name: String,
    descriptor_json: Option<String>,
    admission_state: &str,
    review_decision: &str,
    revision: u64,
    archived: i64,
) -> Result<DeviceAdminView, ControllerError> {
    let key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| read_error("devices.public_key"))?;
    super::codec::validate_display_name(&display_name)
        .map_err(|_| read_error("devices.display_name"))?;
    crate::integrity::validate_device_fields(admission_state, review_decision)
        .map_err(|_| read_error("devices.state"))?;
    let admission_state = match admission_state {
        "PENDING" => AdmissionState::Pending,
        "APPROVED" => AdmissionState::Approved,
        "REVOKED" => AdmissionState::Revoked,
        _ => return Err(read_error("devices.state")),
    };
    let review_decision = match review_decision {
        "none" => ReviewDecision::None,
        "approved" => ReviewDecision::Approved,
        "denied" => ReviewDecision::Denied,
        "revoked" => ReviewDecision::Revoked,
        _ => return Err(read_error("devices.state")),
    };
    let descriptor = match descriptor_json {
        None => None,
        Some(text) => Some(
            serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|_| read_error("devices.descriptor"))?,
        ),
    };
    Ok(DeviceAdminView {
        public_key: key,
        display_name,
        admission_state,
        review_decision,
        revision,
        descriptor,
        archived: crate::integrity::raw_bool(archived, "devices.archived")
            .map_err(|_| read_error("devices.archived"))?,
    })
}

pub fn build_authorization_snapshot(
    actor: SnapshotActor,
    authz_epoch: u64,
    grants: Vec<GrantRecord>,
    roles: HashMap<Uuid, RoleRecord>,
    device_groups: HashMap<[u8; 32], HashSet<Uuid>>,
    devices: HashMap<[u8; 32], DeviceAdminView>,
) -> Result<AuthorizationSnapshot, ControllerError> {
    for grant in &grants {
        if let GrantSource::Role(rid) = &grant.source {
            let role = roles
                .get(rid)
                .ok_or_else(|| read_error("snapshot.role_ref"))?;
            // 归档 role 仅可豁免"非空"要求（合法清空 permissions 后保留父行 = 该 grant 无权，
            // 而非污染）；重复权限无论是否归档都是持久污染，必须失败关闭，
            // 不得按"该 grant 无权限"继续降级求值。
            if role
                .permissions
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                != role.permissions.len()
                || (!role.archived && role.permissions.is_empty())
            {
                return Err(read_error("snapshot.role_permission"));
            }
        }
    }
    Ok(AuthorizationSnapshot {
        actor,
        authz_epoch,
        grants,
        roles,
        device_groups,
        devices,
    })
}

pub fn validate_snapshot_grant_references(
    grant: &GrantRecord,
    owner_exists: i64,
    role_exists: i64,
    group_exists: i64,
    device_exists: i64,
) -> Result<(), ControllerError> {
    let owner = crate::integrity::raw_bool(owner_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    let role = crate::integrity::raw_bool(role_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    let group = crate::integrity::raw_bool(group_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    let device = crate::integrity::raw_bool(device_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    if !owner
        || (matches!(&grant.source, GrantSource::Role(_)) && !role)
        || (matches!(&grant.scope, GrantScope::Group(_)) && !group)
        || (matches!(&grant.scope, GrantScope::Device(_)) && !device)
    {
        Err(read_error("snapshot.reference"))
    } else {
        Ok(())
    }
}

pub fn require_snapshot_isolation(isolation: &str) -> Result<(), ControllerError> {
    if isolation == "REPEATABLE-READ" || isolation == "REPEATABLE READ" {
        Ok(())
    } else {
        Err(read_error("snapshot.isolation"))
    }
}

pub fn decode_snapshot_meta(
    rows: &[(i64, i64, u64)],
    expected_version: i32,
) -> Result<u64, ControllerError> {
    match rows {
        [(1, version, epoch)] if *version == i64::from(expected_version) => Ok(*epoch),
        _ => Err(read_error("snapshot.epoch")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::policy::{GrantScope, GrantSource, Permission};

    /// 解码器契约回归：`descriptor_json` 的入参类型是 `Option<String>`，即 SQL 侧必须
    /// 交付 CHAR 文本；NULL 仍 `None`，非法 JSON/非法 descriptor 仍固定去敏 `Config`。
    #[test]
    // 显式 fn 指针类型是断言内容本身：第三参必须保持 `Option<String>`，即 SQL 侧必须
    // 交付 CHAR 文本投影，签名漂移必须在此暴露。
    #[allow(clippy::type_complexity)]
    fn descriptor_json_decoder_takes_char_text_and_fails_closed_on_malformed() {
        let decode: fn(
            Vec<u8>,
            String,
            Option<String>,
            &str,
            &str,
            u64,
            i64,
        ) -> Result<DeviceAdminView, ControllerError> = decode_device_admin_view;
        let mut cases: Vec<(&str, Option<&str>, Option<serde_json::Value>)> = Vec::new();
        // 非 NULL 合法 JSON 文本 → Some(value)，内容逐字保留（CAST 只换 wire 类型）。
        cases.push((
            "object",
            Some(r#"{"model":"qemu-x86_64","arch":"x86_64"}"#),
            Some(serde_json::json!({"model":"qemu-x86_64","arch":"x86_64"})),
        ));
        // CAST 文本带空白/嵌套/Unicode 也必须原样解析（TiDB/MySQL CAST 输出含空白）。
        cases.push((
            "padded_unicode",
            Some("{ \"型号\": \"板卡 A\" , \"tags\": [ 1 , 2 ] }"),
            Some(serde_json::json!({"型号": "板卡 A", "tags": [1, 2]})),
        ));
        // 边界：SQL NULL（None）与 JSON `null` 文本（Some(Value::Null)）语义不同，都合法。
        cases.push(("sql_null", None, None));
        cases.push((
            "json_null_literal",
            Some("null"),
            Some(serde_json::Value::Null),
        ));
        // 边界：空对象与 JSON 数组同样是合法 descriptor 文本。
        cases.push(("empty_object", Some("{}"), Some(serde_json::json!({}))));
        cases.push(("array", Some("[1,2,3]"), Some(serde_json::json!([1, 2, 3]))));
        // 边界：本层 descriptor 门禁是"必须是合法 JSON 值"（预算/形状门禁在写入侧
        // `transport::admission::descriptor_json`），合法标量按原样保留，不静默丢弃。
        cases.push((
            "scalar_text",
            Some("\"just text\""),
            Some(serde_json::json!("just text")),
        ));
        for (label, input, expected) in cases {
            let view = decode(
                vec![5u8; 32],
                "dev-1".into(),
                input.map(str::to_string),
                "PENDING",
                "none",
                1,
                0,
            )
            .unwrap_or_else(|e| panic!("{label} 合法 descriptor 文本必须解码成功：{e:?}"));
            assert_eq!(view.descriptor, expected, "{label} 解码结果不符");
        }
        // 非法 descriptor 一律失败关闭为固定去敏 Config（503 形状），不得返回空投影。
        for (label, malformed) in [
            ("empty_text", ""),
            ("truncated", r#"{"model":"a""#),
            ("not_json", "{not json"),
        ] {
            let error = match decode(
                vec![5u8; 32],
                "dev-1".into(),
                Some(malformed.to_string()),
                "PENDING",
                "none",
                1,
                0,
            ) {
                Ok(view) => panic!(
                    "{label} 非法 descriptor 必须失败关闭，实际解码 {:?}",
                    view.descriptor
                ),
                Err(error) => error,
            };
            assert!(
                matches!(&error, ControllerError::Config(message)
                    if message == "identity admin read devices.descriptor"),
                "{label} 必须是固定去敏 Config，实际：{error:?}"
            );
        }
    }

    #[test]
    fn snapshot_actor_decode_fails_closed_on_polluted_boolean_and_missing_row() {
        // 行缺失 → PermissionDenied（与 admin helper 同形，不得当 NotFound 泄露账号存在性）。
        assert!(matches!(
            decode_snapshot_actor([1u8; 16], None),
            Err(ControllerError::PermissionDenied)
        ));
        // 脏布尔（2）→ 固定去敏 Config（503 形状），不得当 true。
        let polluted = decode_snapshot_actor([1u8; 16], Some((2, 1, 0))).unwrap_err();
        assert!(
            matches!(&polluted, ControllerError::Config(m) if m == "identity admin read snapshot.actor_active"),
            "dirty boolean must be the exact fixed redacted Config, got {polluted:?}",
        );
        // 合法行严格解码。
        let ok =
            decode_snapshot_actor([1u8; 16], Some((1, 1, 0))).expect("valid actor row decodes");
        assert!(ok.active && ok.is_admin && !ok.must_change_password);
    }

    #[test]
    fn snapshot_roles_decode_fails_closed_on_duplicate_and_empty_permissions() {
        // 重复 role 行 / 被引用 role 的 role_permissions 行数为 0 = 污染 → 固定去敏 Config。
        let role = uuid::Uuid::from_bytes([0x7a; 16]);
        let role_bytes = role.as_bytes().to_vec();
        let duplicate = decode_snapshot_roles(
            &[
                (role_bytes.clone(), "viewer".into(), 1, 0, 1),
                (role_bytes.clone(), "viewer".into(), 1, 0, 1),
            ],
            &[(role_bytes.clone(), "device.read".into())],
        )
        .unwrap_err();
        assert!(matches!(
            &duplicate,
            ControllerError::Config(m) if m == "identity admin read snapshot.roles"
        ));
        let no_perm = decode_snapshot_roles(&[(role_bytes.clone(), "viewer".into(), 1, 0, 1)], &[])
            .unwrap_err();
        assert!(matches!(
            &no_perm,
            ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
        ));
        // 合法行解码：archived role 保留（是否生效由求值层判定），未知权限字符串失败关闭。
        let ok = decode_snapshot_roles(
            &[(role_bytes.clone(), "viewer".into(), 1, 1, 3)],
            &[(role_bytes.clone(), "device.read".into())],
        )
        .expect("valid role rows decode");
        let record = ok.get(&role).expect("referenced role present");
        assert!(
            record.builtin && record.archived && record.permissions == vec![Permission::DeviceRead]
        );
        let unknown_perm = decode_snapshot_roles(
            &[(role_bytes.clone(), "viewer".into(), 0, 0, 1)],
            &[(role_bytes.clone(), "device.fly".into())],
        )
        .unwrap_err();
        assert!(matches!(
            &unknown_perm,
            ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
        ));
    }

    #[test]
    fn snapshot_group_rows_decode_dangling_member_and_filter_archived() {
        // dangling member 行（LEFT JOIN 后 group 行缺失 → archived 为 None）= 污染 → 固定去敏 Config。
        let device = vec![9u8; 32];
        let group = uuid::Uuid::from_bytes([0x67; 16]);
        let group_bytes = group.as_bytes().to_vec();
        let dangling =
            decode_snapshot_group_rows(&[(device.clone(), group_bytes.clone(), None)]).unwrap_err();
        assert!(matches!(
            &dangling,
            ControllerError::Config(m) if m == "identity admin read snapshot.group_missing"
        ));
        // 合法行：archived group 被过滤（不入集合），未归档 group 保留；同设备多组取并集。
        let other_group = uuid::Uuid::from_bytes([0x68; 16]);
        let filtered = decode_snapshot_group_rows(&[
            (device.clone(), group_bytes.clone(), Some(1)), // archived
            (device.clone(), group_bytes.clone(), Some(0)), // 未归档
            (device, other_group.as_bytes().to_vec(), Some(0)),
        ])
        .expect("legal rows decode");
        assert_eq!(
            filtered.get(&[9u8; 32]),
            Some(&std::collections::HashSet::from([group, other_group]))
        );
    }

    #[test]
    fn snapshot_device_decode_fails_closed_on_polluted_state_and_boolean() {
        // 合法设备行解码。
        let view = decode_device_admin_view(
            vec![5u8; 32],
            "dev-1".into(),
            None,
            "APPROVED",
            "approved",
            3,
            0,
        )
        .expect("legal device row decodes");
        assert_eq!(view.public_key, [5u8; 32]);
        assert_eq!(view.admission_state, crate::model::AdmissionState::Approved);
        assert_eq!(view.review_decision, crate::model::ReviewDecision::Approved);
        assert!(!view.archived);
        // 非法状态组合 / 脏布尔 / 脏 descriptor JSON 均失败关闭为固定去敏 Config。
        let bad_state = decode_device_admin_view(
            vec![5u8; 32],
            "dev-1".into(),
            None,
            "PENDING",
            "approved",
            1,
            0,
        );
        assert!(matches!(&bad_state, Err(ControllerError::Config(_))));
        let bad_bool =
            decode_device_admin_view(vec![5u8; 32], "dev-1".into(), None, "PENDING", "none", 1, 2);
        assert!(matches!(&bad_bool, Err(ControllerError::Config(_))));
        let bad_descriptor = decode_device_admin_view(
            vec![5u8; 32],
            "dev-1".into(),
            Some("{not json".into()),
            "PENDING",
            "none",
            1,
            0,
        );
        assert!(matches!(&bad_descriptor, Err(ControllerError::Config(_))));
        // 公钥长度错误同样失败关闭。
        let bad_key =
            decode_device_admin_view(vec![5u8; 31], "dev-1".into(), None, "PENDING", "none", 1, 0);
        assert!(matches!(&bad_key, Err(ControllerError::Config(_))));
    }

    #[test]
    fn snapshot_build_fails_closed_on_missing_role_reference() {
        // 引用污染：grant 指向不在 roles 集的 role → 固定去敏 Config（503 形状），不得按"该 grant 无权限"继续。
        let missing_role = uuid::Uuid::from_bytes([0x77; 16]);
        let grants = vec![GrantRecord {
            id: uuid::Uuid::from_bytes([0x0c; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Role(missing_role),
            scope: GrantScope::All,
            revision: 1,
        }];
        let err = build_authorization_snapshot(
            SnapshotActor {
                id: [1u8; 16],
                active: true,
                is_admin: false,
                must_change_password: false,
            },
            7,
            grants,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
        .unwrap_err();
        assert!(
            matches!(&err, ControllerError::Config(m) if m == "identity admin read snapshot.role_ref"),
            "missing role reference must be the exact fixed redacted Config, got {err:?}",
        );
    }

    #[test]
    fn snapshot_build_fails_closed_on_active_role_empty_or_duplicate_permissions() {
        // 纯快照构建：被引用的当前有效 role 必须非空且无重复 permissions；
        // 归档 role 允许合法清空 permissions（保留父行 = 无权而非污染）。
        let role_id = Uuid::from_bytes([0x5a; 16]);
        let actor = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: false,
            must_change_password: false,
        };
        let grants = vec![GrantRecord {
            id: Uuid::from_bytes([0x0e; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Role(role_id),
            scope: GrantScope::All,
            revision: 1,
        }];
        let build_with = |archived: bool, permissions: Vec<Permission>| {
            let mut roles = HashMap::new();
            roles.insert(
                role_id,
                RoleRecord {
                    id: role_id,
                    name: "viewer".into(),
                    builtin: false,
                    archived,
                    revision: 1,
                    permissions,
                },
            );
            build_authorization_snapshot(
                actor,
                1,
                grants.clone(),
                roles,
                HashMap::new(),
                HashMap::new(),
            )
        };
        let empty = build_with(false, vec![]).unwrap_err();
        assert!(
            matches!(
                &empty,
                ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
            ),
            "active role without permissions must be the exact fixed redacted Config, got {empty:?}",
        );
        let duplicated =
            build_with(false, vec![Permission::DeviceRead, Permission::DeviceRead]).unwrap_err();
        assert!(
            matches!(
                &duplicated,
                ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
            ),
            "active role with duplicate permissions must be the exact fixed redacted Config, got {duplicated:?}",
        );
        assert!(
            build_with(true, vec![]).is_ok(),
            "archived role may keep empty permissions"
        );
        assert!(
            build_with(
                false,
                vec![Permission::DeviceRead, Permission::DeviceReboot]
            )
            .is_ok()
        );
    }

    #[test]
    fn snapshot_build_fails_closed_on_archived_role_duplicate_permissions() {
        // 归档 role 仅豁免"permissions 非空"，绝不豁免"无重复权限"：
        // archived + 重复 permissions 仍是持久污染，必须与 decode_snapshot_roles 一致失败关闭。
        let role_id = Uuid::from_bytes([0x5b; 16]);
        let actor = SnapshotActor {
            id: [1u8; 16],
            active: true,
            is_admin: false,
            must_change_password: false,
        };
        let grants = vec![GrantRecord {
            id: Uuid::from_bytes([0x0f; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Role(role_id),
            scope: GrantScope::All,
            revision: 1,
        }];
        let build_with = |permissions: Vec<Permission>| {
            let mut roles = HashMap::new();
            roles.insert(
                role_id,
                RoleRecord {
                    id: role_id,
                    name: "viewer".into(),
                    builtin: false,
                    archived: true,
                    revision: 1,
                    permissions,
                },
            );
            build_authorization_snapshot(
                actor,
                1,
                grants.clone(),
                roles,
                HashMap::new(),
                HashMap::new(),
            )
        };
        let duplicated =
            build_with(vec![Permission::DeviceRead, Permission::DeviceRead]).unwrap_err();
        assert!(
            matches!(
                &duplicated,
                ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
            ),
            "archived role with duplicate permissions must be the exact fixed redacted Config, got {duplicated:?}",
        );
        let duplicated_three = build_with(vec![
            Permission::DeviceRead,
            Permission::DeviceReboot,
            Permission::DeviceRead,
        ])
        .unwrap_err();
        assert!(matches!(
            &duplicated_three,
            ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
        ));
        assert!(
            build_with(vec![]).is_ok(),
            "archived role may keep empty permissions"
        );
        assert!(
            build_with(vec![Permission::DeviceRead, Permission::DeviceReboot]).is_ok(),
            "archived role with distinct permissions is legal"
        );
    }

    #[test]
    fn group_memberships_for_device_fails_closed_on_archived_or_missing_device() {
        // 列序：(group_id, group_exists, group_archived, device_exists, device_archived)。
        let group = Uuid::from_bytes([0x67; 16]);
        let group_bytes = group.as_bytes().to_vec();
        let archived_device =
            decode_group_memberships_for_device(&[(group_bytes.clone(), 1, 0, 1, 1)]).unwrap_err();
        assert!(
            matches!(
                &archived_device,
                ControllerError::Config(m) if m == "identity admin read groups.for_device.archived_device"
            ),
            "member on archived device must be the exact fixed redacted Config, got {archived_device:?}",
        );
        let missing_device =
            decode_group_memberships_for_device(&[(group_bytes.clone(), 1, 0, 0, 0)]).unwrap_err();
        assert!(matches!(
            &missing_device,
            ControllerError::Config(m) if m == "identity admin read groups.for_device.dangling"
        ));
        let missing_group =
            decode_group_memberships_for_device(&[(group_bytes.clone(), 0, 0, 1, 0)]).unwrap_err();
        assert!(matches!(
            &missing_group,
            ControllerError::Config(m) if m == "identity admin read groups.for_device.dangling"
        ));
        // 脏布尔（2）同样失败关闭，不得当 true/false。
        assert!(matches!(
            decode_group_memberships_for_device(&[(group_bytes.clone(), 1, 2, 1, 0)]),
            Err(ControllerError::Config(_))
        ));
        // 只有 group 归档才正常过滤；未归档 group 正常入集合。
        let other = Uuid::from_bytes([0x68; 16]);
        let ok = decode_group_memberships_for_device(&[
            (group_bytes, 1, 1, 1, 0),
            (other.as_bytes().to_vec(), 1, 0, 1, 0),
        ])
        .expect("legal member rows decode");
        assert_eq!(ok, HashSet::from([other]));
    }

    #[test]
    fn snapshot_build_passes_and_preserves_actor_grants_verbatim() {
        // 校验通过 → 原样组装（不排序/去重/丢弃），epoch 透传。
        let role = uuid::Uuid::from_bytes([0x7a; 16]);
        let grants = vec![GrantRecord {
            id: uuid::Uuid::from_bytes([0x0d; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Role(role),
            scope: GrantScope::All,
            revision: 9,
        }];
        let mut roles = std::collections::HashMap::new();
        roles.insert(
            role,
            RoleRecord {
                id: role,
                name: "viewer".into(),
                builtin: true,
                archived: false,
                revision: 1,
                permissions: vec![Permission::DeviceRead],
            },
        );
        let mut device_groups = std::collections::HashMap::new();
        device_groups.insert([0xa1; 32], {
            let mut set = std::collections::HashSet::new();
            set.insert(uuid::Uuid::from_bytes([0x67; 16]));
            set
        });
        let snapshot = build_authorization_snapshot(
            SnapshotActor {
                id: [1u8; 16],
                active: true,
                is_admin: false,
                must_change_password: false,
            },
            42,
            grants.clone(),
            roles,
            device_groups,
            std::collections::HashMap::new(),
        )
        .expect("valid inputs assemble");
        assert_eq!(snapshot.authz_epoch, 42);
        assert_eq!(snapshot.grants, grants);
        assert!(snapshot.grants.iter().all(|g| g.user_id == [1u8; 16]));
    }

    #[test]
    fn archived_role_empty_permissions_are_legal_but_active_empty_is_corruption() {
        let id = Uuid::new_v4();
        let archived = vec![(id.as_bytes().to_vec(), "archived-role".to_string(), 0, 1, 1)];
        let roles = decode_snapshot_roles(&archived, &[]).expect("合法历史父行不能当污染");
        let role = roles.get(&id).expect("必须保留父行");
        assert!(role.archived);
        assert!(role.permissions.is_empty());
        let actor = crate::authz::policy::SnapshotActor {
            id: [1; 16],
            active: true,
            is_admin: false,
            must_change_password: false,
        };
        let grants = vec![crate::authz::policy::GrantRecord {
            id: Uuid::new_v4(),
            user_id: actor.id,
            source: crate::authz::policy::GrantSource::Role(id),
            scope: crate::authz::policy::GrantScope::All,
            revision: 1,
        }];
        assert!(
            crate::authz::policy::eval_memory_permissions(
                &actor,
                [2; 32],
                &grants,
                &roles,
                &HashSet::new()
            )
            .is_empty()
        );
        let active = vec![(id.as_bytes().to_vec(), "active-role".to_string(), 0, 0, 1)];
        assert!(matches!(
            decode_snapshot_roles(&active, &[]),
            Err(ControllerError::Config(_))
        ));
        let unknown = vec![(id.as_bytes().to_vec(), "invalid.permission".to_string())];
        assert!(matches!(
            decode_snapshot_roles(&archived, &unknown),
            Err(ControllerError::Config(_))
        ));
        let duplicate = vec![(id.as_bytes().to_vec(), "device.read".to_string()); 2];
        assert!(matches!(
            decode_snapshot_roles(&archived, &duplicate),
            Err(ControllerError::Config(_))
        ));
        let dangling = vec![(
            Uuid::new_v4().as_bytes().to_vec(),
            "device.read".to_string(),
        )];
        assert!(matches!(
            decode_snapshot_roles(&archived, &dangling),
            Err(ControllerError::Config(_))
        ));
    }

    #[test]
    fn snapshot_grant_reference_flags_fail_closed_and_direct_all_is_legal() {
        let mut grant = GrantRecord {
            id: Uuid::new_v4(),
            user_id: [1; 16],
            source: GrantSource::Direct(vec![Permission::DeviceRead]),
            scope: GrantScope::All,
            revision: 1,
        };
        assert!(validate_snapshot_grant_references(&grant, 1, 0, 0, 0).is_ok());
        assert!(validate_snapshot_grant_references(&grant, 0, 0, 0, 0).is_err());
        assert!(validate_snapshot_grant_references(&grant, 1, 2, 0, 0).is_err());
        grant.source = GrantSource::Role(Uuid::new_v4());
        assert!(validate_snapshot_grant_references(&grant, 1, 0, 0, 0).is_err());
        assert!(validate_snapshot_grant_references(&grant, 1, 1, 0, 0).is_ok());
        grant.scope = GrantScope::Group(Uuid::new_v4());
        assert!(validate_snapshot_grant_references(&grant, 1, 1, 0, 0).is_err());
        assert!(validate_snapshot_grant_references(&grant, 1, 1, 1, 0).is_ok());
        grant.scope = GrantScope::Device([2; 32]);
        assert!(validate_snapshot_grant_references(&grant, 1, 1, 1, 0).is_err());
        assert!(validate_snapshot_grant_references(&grant, 1, 1, 1, 1).is_ok());
    }

    #[test]
    fn snapshot_meta_rejects_extra_wrong_or_unknown_rows_and_preserves_u64_epoch() {
        assert_eq!(
            decode_snapshot_meta(&[(1, 4, u64::MAX)], 4).unwrap(),
            u64::MAX
        );
        assert_eq!(decode_snapshot_meta(&[(1, 4, 0)], 4).unwrap(), 0); // epoch零可为尚无变更，不猜成污染
        for rows in [
            vec![],
            vec![(0, 4, 1)],
            vec![(2, 4, 1)],
            vec![(1, 3, 1)],
            vec![(1, 5, 1)],
            vec![(1, 4, 1), (2, 4, 2)],
        ] {
            assert!(matches!(
                decode_snapshot_meta(&rows, 4),
                Err(ControllerError::Config(_))
            ));
        }
        assert!(!super::super::queries::SNAPSHOT_META_SQL.contains("WHERE"));
        assert!(super::super::queries::SNAPSHOT_META_SQL.ends_with("LIMIT 2"));
    }

    #[test]
    fn snapshot_requires_verified_repeatable_read_configuration() {
        assert!(require_snapshot_isolation("REPEATABLE-READ").is_ok());
        assert!(require_snapshot_isolation("REPEATABLE READ").is_ok());
        for bad in ["", "READ-COMMITTED", "SERIALIZABLE", "unknown"] {
            assert!(matches!(
                require_snapshot_isolation(bad),
                Err(ControllerError::Config(_))
            ));
        }
    }
}

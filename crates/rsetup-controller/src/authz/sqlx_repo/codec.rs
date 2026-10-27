use uuid::Uuid;

use crate::authz::policy::{GrantRecord, GrantScope, GrantSource, Permission};
use crate::authz::sqlx_repo::model::{GrantColumns, UserRecord};
use crate::error::ControllerError;

pub(crate) fn read_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("identity admin read {code}"))
}

pub fn decode_user(
    id: Vec<u8>,
    username: String,
    display_name: String,
    active: i64,
    is_admin: i64,
    must_change_password: i64,
    revision: u64,
) -> Result<UserRecord, ControllerError> {
    let id: [u8; 16] = id.try_into().map_err(|_| read_error("users.id"))?;
    if !crate::db::valid_username(&username) {
        return Err(read_error("users.username"));
    }
    validate_display_name(&display_name).map_err(|_| read_error("users.display_name"))?;
    Ok(UserRecord {
        id,
        username,
        display_name,
        active: crate::integrity::raw_bool(active, "users.active")?,
        is_admin: crate::integrity::raw_bool(is_admin, "users.is_admin")?,
        must_change_password: crate::integrity::raw_bool(
            must_change_password,
            "users.must_change_password",
        )?,
        revision,
    })
}

/// 分页：SQL 绑定 `limit + 1`，本函数只接受"是否多取到一行"的判定结果。
pub fn page_cursor<T: Copy + PartialEq>(rows: &[T], limit: usize) -> (usize, Option<T>) {
    if rows.len() > limit {
        (limit, Some(rows[limit - 1]))
    } else {
        (rows.len(), None)
    }
}

pub fn validate_display_name(value: &str) -> Result<(), ControllerError> {
    let trimmed = value.trim();
    if trimmed.is_empty() || value.len() > 255 || value.chars().any(|c| c.is_control()) {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(())
}

/// `VARCHAR(128)+utf8mb4` 的 name 门禁：按 Unicode 字符数（与
/// `validate_display_name` 的字节 255 规则不同）。
pub(crate) fn validate_group_name(value: &str) -> Result<(), ControllerError> {
    if value.trim().is_empty()
        || value.chars().count() > 128
        || value.chars().any(|c| c.is_control())
    {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(())
}

/// 用户行锁顺序：按 UUID 字节序确定性排序；同一用户只锁一行，禁止自锁两次。
pub(crate) fn ordered_user_ids(actor: [u8; 16], target: [u8; 16]) -> ([u8; 16], Option<[u8; 16]>) {
    if actor <= target {
        (actor, (actor != target).then_some(target))
    } else {
        (target, Some(actor))
    }
}

/// LastAdmin 触发判定：仅"当前活跃管理员且本次变更确实移除该状态"才触发
/// （停用或降级活跃管理员），与既有六条断言一致。
pub(crate) fn admin_change_removes_admin_state(
    target_active: bool,
    target_is_admin: bool,
    next_active: bool,
    next_is_admin: bool,
) -> bool {
    target_active && target_is_admin && (!next_active || !next_is_admin)
}

/// 归档恢复门禁（方案 A）：从归档恢复必须携带合法非空权限集，否则 UPDATE 前
/// `InvalidArgument`(400) 拒绝，绝不留下 active 空权限非法状态。
/// 非恢复分支（归档、active 原地编辑、保持归档）不受限；与同事务重建+回读
/// 共同保证永不产生 active 空权限角色。
pub(super) fn validate_role_restore(
    locked_archived: bool,
    next_archived: bool,
    permissions: Option<&[Permission]>,
) -> Result<(), ControllerError> {
    if locked_archived && !next_archived {
        match permissions {
            None => Err(ControllerError::InvalidArgument),
            Some(perms) => crate::authz::policy::validate_permission_list(perms),
        }
    } else {
        Ok(())
    }
}

/// source 与 scope 必须成对；`validate_grant_fields` 是唯一权威判定。
pub fn grant_columns(
    source: &GrantSource,
    scope: &GrantScope,
) -> Result<GrantColumns, ControllerError> {
    let (source_kind, role_id, permissions_json) = match source {
        GrantSource::Role(role_id) => ("role", Some(*role_id), None),
        GrantSource::Direct(permissions) => {
            let mut values: Vec<&'static str> = permissions.iter().map(|p| p.as_str()).collect();
            values.sort_unstable();
            values.dedup();
            if values.len() != permissions.len() || values.is_empty() {
                return Err(ControllerError::InvalidArgument);
            }
            (
                "direct",
                None,
                Some(serde_json::Value::Array(
                    values.iter().map(|p| serde_json::Value::from(*p)).collect(),
                )),
            )
        }
    };
    let permissions_value = permissions_json.as_ref();
    let (scope_kind, scope_group_id, scope_device_id) = match scope {
        GrantScope::All => ("all", None, None),
        GrantScope::Group(group_id) => ("group", Some(*group_id), None),
        GrantScope::Device(device_id) => ("device", None, Some(*device_id)),
    };
    crate::integrity::validate_grant_fields(
        source_kind,
        role_id.is_some(),
        permissions_value,
        scope_kind,
        scope_group_id.is_some(),
        scope_device_id.is_some(),
    )?;
    Ok(GrantColumns {
        source_kind,
        role_id,
        permissions_json: permissions_json.map(|value| value.to_string()),
        scope_kind,
        scope_group_id,
        scope_device_id,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn decode_grant(
    id: Vec<u8>,
    user_id: Vec<u8>,
    source_kind: String,
    role_id: Option<Vec<u8>>,
    permissions: Option<serde_json::Value>,
    scope_kind: String,
    scope_group_id: Option<Vec<u8>>,
    scope_device_id: Option<Vec<u8>>,
    revision: u64,
) -> Result<GrantRecord, ControllerError> {
    // 持久行字段污染：`validate_grant_fields` 的 `InvalidArgument` 只是内部形状，
    // 管理读/快照读共用本解码，必须统一换成固定去敏 `Config`（503 形状），
    // 绝不把已落库的脏行当客户端 400 传出。
    crate::integrity::validate_grant_fields(
        &source_kind,
        role_id.is_some(),
        permissions.as_ref(),
        &scope_kind,
        scope_group_id.is_some(),
        scope_device_id.is_some(),
    )
    .map_err(|_| read_error("grants.fields"))?;
    let source = match source_kind.as_str() {
        "role" => GrantSource::Role(uuid_from(role_id)?),
        "direct" => {
            let mut parsed = Vec::new();
            for value in permissions
                .as_ref()
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| read_error("grants.permissions"))?
            {
                let text = value
                    .as_str()
                    .ok_or_else(|| read_error("grants.permissions"))?;
                parsed
                    .push(Permission::parse(text).ok_or_else(|| read_error("grants.permissions"))?);
            }
            GrantSource::Direct(parsed)
        }
        _ => return Err(read_error("grants.source")),
    };
    let scope = match scope_kind.as_str() {
        "all" => GrantScope::All,
        "group" => GrantScope::Group(uuid_from(scope_group_id)?),
        "device" => GrantScope::Device(device_from(scope_device_id)?),
        _ => return Err(read_error("grants.scope")),
    };
    Ok(GrantRecord {
        id: uuid_from(Some(id))?,
        user_id: user_id
            .try_into()
            .map_err(|_| read_error("grants.user_id"))?,
        source,
        scope,
        revision,
    })
}

fn uuid_from(value: Option<Vec<u8>>) -> Result<uuid::Uuid, ControllerError> {
    let bytes: [u8; 16] = value
        .ok_or_else(|| read_error("grants.uuid"))?
        .try_into()
        .map_err(|_| read_error("grants.uuid"))?;
    Ok(uuid::Uuid::from_bytes(bytes))
}

fn device_from(value: Option<Vec<u8>>) -> Result<[u8; 32], ControllerError> {
    value
        .ok_or_else(|| read_error("grants.device_id"))?
        .try_into()
        .map_err(|_| read_error("grants.device_id"))
}

pub(super) fn uuid16(value: Vec<u8>, code: &'static str) -> Result<Uuid, ControllerError> {
    let bytes: [u8; 16] = value.try_into().map_err(|_| read_error(code))?;
    Ok(Uuid::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::policy::{GrantScope, GrantSource, Permission};

    #[test]
    fn user_decode_fails_closed_on_polluted_boolean_and_username() {
        // 脏布尔（2）必须失败关闭，不得当作 true。
        assert!(decode_user(vec![1u8; 16], "alice".into(), "Alice".into(), 2, 1, 0, 1).is_err());
        // 非规范用户名必须失败关闭。
        assert!(decode_user(vec![1u8; 16], "Alice".into(), "Alice".into(), 1, 1, 0, 1).is_err());
        // 合法行必须成功且 revision 可到 u64 高位。
        let ok = decode_user(
            vec![1u8; 16],
            "alice".into(),
            "Alice".into(),
            1,
            0,
            1,
            u64::MAX,
        )
        .expect("valid row must decode");
        assert!(ok.active);
        assert!(!ok.is_admin);
        assert!(ok.must_change_password);
        assert_eq!(ok.revision, u64::MAX);
    }

    #[test]
    fn page_cursor_reports_exactly_one_extra_row_and_never_leaks_total() {
        let rows = [1u8, 2, 3];
        // limit=2 时应返回 2 项并把第 2 项作为下一页游标。
        let (take, cursor) = page_cursor(&rows, 2);
        assert_eq!(take, 2);
        assert_eq!(cursor, Some(2u8));
        // 恰好取完时不得给出游标。
        let (take, cursor) = page_cursor(&rows, 3);
        assert_eq!(take, 3);
        assert_eq!(cursor, None);
    }

    #[test]
    fn grant_columns_pair_source_with_scope_strictly() {
        let role = uuid::Uuid::nil();
        let direct = grant_columns(
            &GrantSource::Direct(vec![Permission::DeviceRead]),
            &GrantScope::All,
        )
        .expect("direct/all is valid");
        assert_eq!(direct.source_kind, "direct");
        assert!(direct.role_id.is_none());
        assert_eq!(
            direct.permissions_json.as_deref(),
            Some(r#"["device.read"]"#)
        );
        assert_eq!(direct.scope_kind, "all");
        assert!(direct.scope_group_id.is_none() && direct.scope_device_id.is_none());

        let by_role = grant_columns(&GrantSource::Role(role), &GrantScope::Group(role))
            .expect("role/group is valid");
        assert_eq!(by_role.source_kind, "role");
        assert_eq!(by_role.role_id, Some(role));
        assert!(by_role.permissions_json.is_none());
        assert_eq!(by_role.scope_kind, "group");
        assert_eq!(by_role.scope_group_id, Some(role));
    }

    #[test]
    fn decode_grant_maps_persistent_field_pollution_to_fixed_config() {
        // 已落库的 source/scope 列不成对 = 持久污染：必须是固定去敏 Config（503 形状），
        // 绝不能以 InvalidArgument 透传成客户端 400。
        let id = uuid::Uuid::from_bytes([0x11; 16]);
        let id_bytes = id.as_bytes().to_vec();
        let user = [1u8; 16].to_vec();
        let polluted = decode_grant(
            id_bytes.clone(),
            user.clone(),
            "role".into(),
            None,
            None,
            "all".into(),
            None,
            None,
            1,
        )
        .unwrap_err();
        assert!(
            matches!(
                &polluted,
                ControllerError::Config(m) if m == "identity admin read grants.fields"
            ),
            "persisted grant pollution must be the exact fixed redacted Config, got {polluted:?}",
        );
        // direct 权限数组含重复项 / 未知权限 / scope 与列不成对，同样失败关闭为 Config。
        for (source_kind, role_id, permissions, scope_kind, scope_group_id, scope_device_id) in [
            (
                "direct",
                None,
                Some(serde_json::json!(["device.read", "device.read"])),
                "all",
                None,
                None,
            ),
            (
                "direct",
                None,
                Some(serde_json::json!(["device.fly"])),
                "all",
                None,
                None,
            ),
            (
                "direct",
                None,
                Some(serde_json::json!(["device.read"])),
                "group",
                None,
                None,
            ),
            (
                "direct",
                None,
                Some(serde_json::json!(["device.read"])),
                "device",
                None,
                None,
            ),
        ] {
            let err = decode_grant(
                id_bytes.clone(),
                user.clone(),
                source_kind.into(),
                role_id,
                permissions,
                scope_kind.into(),
                scope_group_id,
                scope_device_id,
                1,
            )
            .unwrap_err();
            assert!(
                matches!(err, ControllerError::Config(_)),
                "polluted row must be Config, got {err:?}",
            );
        }
        // 合法行仍按原语义解码。
        let ok = decode_grant(
            id_bytes,
            user,
            "direct".into(),
            None,
            Some(serde_json::json!(["device.read"])),
            "all".into(),
            None,
            None,
            4,
        )
        .expect("legal grant row decodes");
        assert_eq!(ok.id, id);
        assert_eq!(ok.revision, 4);
    }

    #[test]
    fn user_lock_order_is_deterministic_by_uuid_bytes() {
        let low = [1u8; 16];
        let high = [2u8; 16];
        assert_eq!(ordered_user_ids(high, low), (low, Some(high)));
        assert_eq!(ordered_user_ids(low, high), (low, Some(high)));
        // 同一用户只锁一行，禁止自锁两次。
        assert_eq!(ordered_user_ids(low, low), (low, None));
    }

    #[test]
    fn only_removing_active_admin_state_triggers_last_admin_rule() {
        // 停用活跃管理员 / 降级活跃管理员 → 需要检查
        assert!(admin_change_removes_admin_state(true, true, false, true));
        assert!(admin_change_removes_admin_state(true, true, true, false));
        // 已是停用或非管理员 → 不触发（不得误报 LAST_ADMIN）
        assert!(!admin_change_removes_admin_state(false, true, false, true));
        assert!(!admin_change_removes_admin_state(true, false, true, false));
        // 不变更或升级 → 不触发
        assert!(!admin_change_removes_admin_state(true, true, true, true));
        assert!(!admin_change_removes_admin_state(false, false, true, true));
    }

    #[test]
    fn permission_list_must_be_non_empty_and_unique() {
        use crate::authz::policy::Permission::*;
        assert!(crate::authz::policy::validate_permission_list(&[]).is_err());
        assert!(crate::authz::policy::validate_permission_list(&[DeviceRead, DeviceRead]).is_err());
        assert!(
            crate::authz::policy::validate_permission_list(&[DeviceRead, DeviceReboot]).is_ok()
        );
    }

    #[test]
    fn role_restore_from_archive_requires_valid_non_empty_permissions() {
        use crate::authz::policy::Permission::*;
        // 正例：合法非空权限恢复 → 门禁过；同事务重建+回读得 active 非空权限。
        assert!(validate_role_restore(true, false, Some(&[DeviceRead, DeviceReboot])).is_ok());
        // 反例：无权限恢复 → InvalidArgument(400)，绝不产生 active 空权限角色。
        assert!(matches!(
            validate_role_restore(true, false, None),
            Err(ControllerError::InvalidArgument)
        ));
        // 反例：空权限恢复 → validate_permission_list 非空门禁拒绝（同为 400 形状）。
        assert!(validate_role_restore(true, false, Some(&[])).is_err());
        // 非恢复分支不受限：归档、active 原地编辑、保持归档。
        assert!(validate_role_restore(false, true, None).is_ok());
        assert!(validate_role_restore(false, false, None).is_ok());
        assert!(validate_role_restore(true, true, None).is_ok());
    }

    #[test]
    fn validate_group_name_boundaries() {
        let legal_128: String = "群".repeat(128); // 128 Unicode 字符 / 384 字节
        assert_eq!(legal_128.chars().count(), 128);
        let over_129: String = "群".repeat(129); // 129 字符
        let cjk_300_bytes: String = "群".repeat(100); // 100 字符 / 300 字节：字符数规则合法、字节 255 规则会拒
        let cases: [(&str, bool); 8] = [
            ("ops-eu-1", true),
            (legal_128.as_str(), true),
            (cjk_300_bytes.as_str(), true),
            ("", false),
            ("   ", false),
            (over_129.as_str(), false),
            ("a\tb", false),
            ("\u{1}", false), // 第 8 例：另一明确控制字符负例（U+0001）
        ];
        for (input, expect_ok) in cases {
            assert_eq!(
                validate_group_name(input).is_ok(),
                expect_ok,
                "input = {input:?}"
            );
        }
    }
}

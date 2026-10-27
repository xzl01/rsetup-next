use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::authz::policy::{GrantRecord, GrantSource, RoleRecord, SnapshotActor};
use crate::db::DbPool;
use crate::devices::repository::DeviceAdminView;
use crate::error::ControllerError;

use super::codec::{decode_grant, read_error};
use super::model::AuthorizationSnapshot;
use super::queries::{
    ACTOR_READONLY_SQL, DEVICES_BY_IDS_SQL_TEMPLATE, GROUPS_FOR_DEVICES_SQL_TEMPLATE,
    LIST_ACTOR_GRANTS_SQL, ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE, ROLES_BY_IDS_SQL_TEMPLATE,
    SNAPSHOT_META_SQL,
};
use super::snapshot_codec::{
    build_authorization_snapshot, decode_device_admin_view, decode_snapshot_actor,
    decode_snapshot_group_rows, decode_snapshot_meta, decode_snapshot_roles,
    require_snapshot_isolation, validate_snapshot_grant_references,
};
use super::tx_guard::placeholders;

pub(crate) async fn acquire_authorization_read_connection(
    db: &DbPool,
) -> Result<sqlx::pool::PoolConnection<sqlx::MySql>, ControllerError> {
    let mut connection =
        db.0.acquire()
            .await
            .map_err(|_| read_error("snapshot.tx"))?;
    let isolation: String = sqlx::query_scalar("SELECT @@session.transaction_isolation")
        .fetch_one(&mut *connection)
        .await
        .map_err(|_| read_error("snapshot.isolation"))?;
    require_snapshot_isolation(&isolation)?;
    Ok(connection)
}

pub(crate) async fn ensure_actor_admin_readonly(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor: &crate::auth::service::Session,
) -> Result<(), ControllerError> {
    use sqlx::Row;
    let row = sqlx::query(ACTOR_READONLY_SQL)
        .bind(actor.user.id.as_slice())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| read_error("users.actor"))?;
    let row = row.ok_or(ControllerError::PermissionDenied)?;
    let active = crate::integrity::raw_bool(
        row.try_get::<i64, _>("active")
            .map_err(|_| read_error("users.active"))?,
        "users.active",
    )?;
    let is_admin = crate::integrity::raw_bool(
        row.try_get::<i64, _>("is_admin")
            .map_err(|_| read_error("users.is_admin"))?,
        "users.is_admin",
    )?;
    let must_change_password = crate::integrity::raw_bool(
        row.try_get::<i64, _>("must_change_password")
            .map_err(|_| read_error("users.must_change_password"))?,
        "users.must_change_password",
    )?;
    if active && is_admin && !must_change_password {
        Ok(())
    } else {
        Err(ControllerError::PermissionDenied)
    }
}

pub(crate) async fn verify_management_read(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor: &crate::auth::service::Session,
) -> Result<(), ControllerError> {
    use sqlx::Row;
    let rows = sqlx::query(SNAPSHOT_META_SQL)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.epoch"))?;
    let raw = rows
        .iter()
        .map(|row| -> Result<(i64, i64, u64), ControllerError> {
            Ok((
                row.try_get("singleton")
                    .map_err(|_| read_error("snapshot.epoch"))?,
                row.try_get("schema_version")
                    .map_err(|_| read_error("snapshot.epoch"))?,
                row.try_get("authz_epoch")
                    .map_err(|_| read_error("snapshot.epoch"))?,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    decode_snapshot_meta(&raw, crate::integrity::SUPPORTED_SCHEMA_VERSION)?;
    ensure_actor_admin_readonly(tx, actor).await
}

pub(crate) struct ActorAuthorization {
    pub actor: SnapshotActor,
    pub authz_epoch: u64,
    pub grants: Vec<GrantRecord>,
    pub roles: HashMap<Uuid, RoleRecord>,
}

pub(crate) async fn load_actor_authorization_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor_id: [u8; 16],
) -> Result<ActorAuthorization, ControllerError> {
    use sqlx::Row;
    let row = sqlx::query(ACTOR_READONLY_SQL)
        .bind(actor_id.as_slice())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.actor"))?;
    let actor = decode_snapshot_actor(
        actor_id,
        match row {
            None => None,
            Some(r) => Some((
                r.try_get::<i64, _>("active")
                    .map_err(|_| read_error("snapshot.actor"))?,
                r.try_get::<i64, _>("is_admin")
                    .map_err(|_| read_error("snapshot.actor"))?,
                r.try_get::<i64, _>("must_change_password")
                    .map_err(|_| read_error("snapshot.actor"))?,
            )),
        },
    )?;

    let meta_rows = sqlx::query(SNAPSHOT_META_SQL)
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.epoch"))?;
    let raw_meta = meta_rows
        .iter()
        .map(|row| -> Result<(i64, i64, u64), ControllerError> {
            Ok((
                row.try_get("singleton")
                    .map_err(|_| read_error("snapshot.epoch"))?,
                row.try_get("schema_version")
                    .map_err(|_| read_error("snapshot.epoch"))?,
                row.try_get("authz_epoch")
                    .map_err(|_| read_error("snapshot.epoch"))?,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let authz_epoch = decode_snapshot_meta(&raw_meta, crate::integrity::SUPPORTED_SCHEMA_VERSION)?;

    let grant_rows = sqlx::query(LIST_ACTOR_GRANTS_SQL)
        .bind(actor_id.as_slice())
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.grants"))?;
    let grants: Vec<GrantRecord> = grant_rows
        .iter()
        .map(|r| -> Result<GrantRecord, ControllerError> {
            let permissions_raw: Option<String> = r
                .try_get("permissions")
                .map_err(|_| read_error("snapshot.grants"))?;
            let permissions = match permissions_raw {
                Some(text) => {
                    Some(serde_json::from_str(&text).map_err(|_| read_error("snapshot.grants"))?)
                }
                None => None,
            };
            let grant = decode_grant(
                r.try_get("id").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("user_id")
                    .map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("source_kind")
                    .map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("role_id")
                    .map_err(|_| read_error("snapshot.grants"))?,
                permissions,
                r.try_get("scope_kind")
                    .map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("scope_group_id")
                    .map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("scope_device_id")
                    .map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("revision")
                    .map_err(|_| read_error("snapshot.grants"))?,
            )
            .map_err(|_| read_error("snapshot.grants"))?;
            validate_snapshot_grant_references(
                &grant,
                r.try_get("owner_exists")
                    .map_err(|_| read_error("snapshot.reference"))?,
                r.try_get("role_exists")
                    .map_err(|_| read_error("snapshot.reference"))?,
                r.try_get("group_exists")
                    .map_err(|_| read_error("snapshot.reference"))?,
                r.try_get("device_exists")
                    .map_err(|_| read_error("snapshot.reference"))?,
            )?;
            Ok(grant)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut role_ids: Vec<Uuid> = grants
        .iter()
        .filter_map(|g| match &g.source {
            GrantSource::Role(rid) => Some(*rid),
            _ => None,
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    role_ids.sort();
    let roles: HashMap<Uuid, RoleRecord> = if role_ids.is_empty() {
        HashMap::new()
    } else {
        let ph = placeholders(role_ids.len());
        let roles_sql = ROLES_BY_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph);
        let mut q = sqlx::query(&roles_sql);
        for rid in &role_ids {
            q = q.bind(rid.as_bytes().as_slice());
        }
        let role_rows = q
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| read_error("snapshot.roles"))?;
        let raw_roles: Vec<(Vec<u8>, String, i64, i64, u64)> = role_rows
            .iter()
            .map(|r| -> Result<_, ControllerError> {
                Ok((
                    r.try_get("id").map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("name")
                        .map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("builtin")
                        .map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("archived")
                        .map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("revision")
                        .map_err(|_| read_error("snapshot.roles"))?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let ph = placeholders(role_ids.len());
        let perms_sql = ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph);
        let mut q = sqlx::query(&perms_sql);
        for rid in &role_ids {
            q = q.bind(rid.as_bytes().as_slice());
        }
        let perm_rows = q
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| read_error("snapshot.roles"))?;
        let raw_perms: Vec<(Vec<u8>, String)> = perm_rows
            .iter()
            .map(|r| -> Result<_, ControllerError> {
                Ok((
                    r.try_get("role_id")
                        .map_err(|_| read_error("snapshot.role_permission"))?,
                    r.try_get("permission")
                        .map_err(|_| read_error("snapshot.role_permission"))?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        decode_snapshot_roles(&raw_roles, &raw_perms)?
    };

    for role_id in &role_ids {
        if !roles.contains_key(role_id) {
            return Err(read_error("snapshot.role_missing"));
        }
    }
    Ok(ActorAuthorization {
        actor,
        authz_epoch,
        grants,
        roles,
    })
}

pub(crate) async fn load_device_snapshot_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    device_ids: &[[u8; 32]],
) -> Result<
    (
        HashMap<[u8; 32], HashSet<Uuid>>,
        HashMap<[u8; 32], DeviceAdminView>,
    ),
    ControllerError,
> {
    if device_ids.is_empty() {
        return Ok((HashMap::new(), HashMap::new()));
    }
    let ph = placeholders(device_ids.len());

    use sqlx::Row;
    let groups_sql = GROUPS_FOR_DEVICES_SQL_TEMPLATE.replace("{placeholders}", &ph);
    let mut q = sqlx::query(&groups_sql);
    for key in device_ids {
        q = q.bind(key.as_slice());
    }
    let group_rows = q
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.groups"))?;
    let raw_groups: Vec<(Vec<u8>, Vec<u8>, Option<i64>)> = group_rows
        .iter()
        .map(|r| -> Result<_, ControllerError> {
            let raw_exists: i64 = r
                .try_get("member_device_exists")
                .map_err(|_| read_error("snapshot.reference"))?;
            let exists = crate::integrity::raw_bool(raw_exists, "snapshot.reference")
                .map_err(|_| read_error("snapshot.reference"))?;
            if !exists {
                return Err(read_error("snapshot.reference"));
            }
            Ok((
                r.try_get("device_id")
                    .map_err(|_| read_error("snapshot.groups"))?,
                r.try_get("group_id")
                    .map_err(|_| read_error("snapshot.groups"))?,
                r.try_get("group_archived")
                    .map_err(|_| read_error("snapshot.groups"))?,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let device_groups = decode_snapshot_group_rows(&raw_groups)?;

    let devices_sql = DEVICES_BY_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph);
    let mut q = sqlx::query(&devices_sql);
    for key in device_ids {
        q = q.bind(key.as_slice());
    }
    let device_rows = q
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.devices"))?;
    let mut devices: HashMap<[u8; 32], DeviceAdminView> = HashMap::new();
    for r in &device_rows {
        let view = decode_device_admin_view(
            r.try_get("public_key")
                .map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("display_name")
                .map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("descriptor_json")
                .map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("admission_state")
                .map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("review_decision")
                .map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("revision")
                .map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("archived")
                .map_err(|_| read_error("snapshot.devices"))?,
        )?;
        devices.insert(view.public_key, view);
    }
    Ok((device_groups, devices))
}

pub(crate) async fn load_snapshot_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor_id: [u8; 16],
    device_ids: &[[u8; 32]],
) -> Result<AuthorizationSnapshot, ControllerError> {
    let actor_auth = load_actor_authorization_in_tx(tx, actor_id).await?;
    let (device_groups, devices) = load_device_snapshot_in_tx(tx, device_ids).await?;
    build_authorization_snapshot(
        actor_auth.actor,
        actor_auth.authz_epoch,
        actor_auth.grants,
        actor_auth.roles,
        device_groups,
        devices,
    )
}

pub(crate) async fn load_admin_target_snapshot_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    caller: &crate::auth::service::Session,
    target_user: [u8; 16],
    device_ids: &[[u8; 32]],
) -> Result<AuthorizationSnapshot, ControllerError> {
    ensure_actor_admin_readonly(tx, caller).await?;
    load_snapshot_in_tx(tx, target_user, device_ids).await
}

#[cfg(test)]
mod tests {
    use super::super::queries::DEVICES_BY_IDS_SQL_TEMPLATE;
    use super::super::sql_shape;

    /// 执行位门禁：设备批量读必须消费被上面回归钉住的模板常量，且本文件生产段内不得
    /// 出现第二条未被钉的 `descriptor_json` 读 SQL 字面量（内联副本会绕过形状回归）。
    #[test]
    fn device_snapshot_read_consumes_pinned_template_without_inline_copy() {
        let src = sql_shape::production_source(include_str!("snapshot_read.rs"));
        assert!(
            src.contains("DEVICES_BY_IDS_SQL_TEMPLATE.replace(\"{placeholders}\", &ph)"),
            "load_device_snapshot_in_tx 必须消费 DEVICES_BY_IDS_SQL_TEMPLATE 常量"
        );
        assert!(
            src.contains("r.try_get(\"descriptor_json\")"),
            "设备快照必须以 descriptor_json 列名取列，别名改名会静默改变解码输入"
        );
        let pinned = sql_shape::normalize_sql(DEVICES_BY_IDS_SQL_TEMPLATE);
        for literal in sql_shape::select_string_literals(src) {
            if literal.contains("descriptor_json") {
                assert_eq!(
                    literal, pinned,
                    "本文件生产段内的 descriptor_json 读 SQL 必须是 DEVICES_BY_IDS_SQL_TEMPLATE 本体"
                );
            }
        }
    }
}

mod codec;
mod model;
mod queries;
mod snapshot_codec;
mod traits;

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{
    GrantRecord, GrantScope, GrantSource, Permission, RoleRecord, SnapshotActor,
    check_admin_demotion_allowed,
};
use crate::db::DbPool;
use crate::devices::repository::DeviceAdminView;
use crate::error::ControllerError;

use self::codec::validate_role_restore;
pub(crate) use self::codec::{
    admin_change_removes_admin_state, ordered_user_ids, read_error, validate_group_name,
};
pub use self::codec::{
    decode_grant, decode_user, grant_columns, page_cursor, validate_display_name,
};
pub use self::model::{AuthorizationSnapshot, GrantColumns, GroupRecord, RepoFuture, UserRecord};
pub(crate) use self::queries::{
    ACTOR_READONLY_SQL, ADMIN_TARGET_DEVICE_KEYS_SQL, DEVICE_GROUPS_FOR_DEVICE_SQL,
    DEVICES_BY_IDS_SQL_TEMPLATE, FIND_GRANT_SQL, FIND_GROUP_SQL, FIND_ROLE_SQL, FIND_USER_SQL,
    GROUPS_FOR_DEVICES_SQL_TEMPLATE, LIST_ACTOR_GRANTS_SQL, LIST_GRANTS_FOR_USER_SQL,
    LIST_GRANTS_SQL, LIST_GROUP_DEVICE_IDS_SQL, LIST_GROUPS_FIRST_SQL, LIST_GROUPS_SQL,
    LIST_ROLES_FIRST_SQL, LIST_ROLES_SQL, LIST_USERS_FIRST_SQL, LIST_USERS_SQL,
    ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE, ROLES_BY_IDS_SQL_TEMPLATE, SNAPSHOT_META_SQL,
};
pub use self::snapshot_codec::{
    build_authorization_snapshot, decode_device_admin_view, decode_group_memberships_for_device,
    decode_snapshot_actor, decode_snapshot_group_rows, decode_snapshot_meta, decode_snapshot_roles,
    require_snapshot_isolation, validate_snapshot_grant_references,
};
pub use self::traits::{AuthorizationSnapshotRepository, AuthzRepository};

pub struct SqlxAuthzRepository {
    pub(crate) db: DbPool,
}

impl SqlxAuthzRepository {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

pub struct SqlxAuthorizationSnapshotRepository {
    pub(crate) db: DbPool,
}

impl SqlxAuthorizationSnapshotRepository {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

/// 目标为 users 行时的行锁快照（`FOR UPDATE`），解码为安全 `UserRecord`。
type UserRow = UserRecord;

async fn select_user_for_update(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    id: [u8; 16],
) -> Result<Option<UserRow>, ControllerError> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id,username,display_name,CAST(active AS SIGNED) AS active,CAST(is_admin AS SIGNED) AS is_admin,CAST(must_change_password AS SIGNED) AS must_change_password,revision FROM users WHERE id=? FOR UPDATE",
    )
    .bind(id.as_slice())
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| {
        decode_user(
            row.try_get("id")?,
            row.try_get("username")?,
            row.try_get("display_name")?,
            row.try_get("active")?,
            row.try_get("is_admin")?,
            row.try_get("must_change_password")?,
            row.try_get("revision")?,
        )
    })
    .transpose()
}

/// 管理写事务统一入口：begin 后立即取 schema guard 行锁（期望版本固定为
/// 编译期 `SUPPORTED_SCHEMA_VERSION`），串行化全部管理写。
async fn begin_guard(db: &DbPool) -> Result<sqlx::Transaction<'_, sqlx::MySql>, ControllerError> {
    let mut tx = db.0.begin().await?;
    crate::integrity::lock_integrity_guard(&mut tx).await?;
    Ok(tx)
}

/// 目标为 users 行时：guard → 按 UUID 字节序锁 actor/target → 用**锁定行**
/// 严格重验 actor 仍为有效管理员 → 返回目标行（缺失即 NotFound）。
async fn lock_user_pair_and_verify_actor(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor: [u8; 16],
    target: [u8; 16],
) -> Result<Option<UserRow>, ControllerError> {
    let (first, second) = ordered_user_ids(actor, target);
    let first_row = select_user_for_update(tx, first).await?;
    let second_row = match second {
        Some(id) => select_user_for_update(tx, id).await?,
        None => None,
    };
    let actor_row = if first == actor {
        first_row.as_ref()
    } else {
        second_row.as_ref()
    };
    let actor_row = actor_row.ok_or(ControllerError::PermissionDenied)?;
    if !(actor_row.active && actor_row.is_admin && !actor_row.must_change_password) {
        return Err(ControllerError::PermissionDenied);
    }
    Ok(if first == target {
        first_row
    } else {
        second_row
    })
}

/// `authz_epoch` checked CAS 自增；singleton 行已被 guard 锁定，CAS 仍防未走 guard 的写者。
pub(crate) async fn bump_authz_epoch(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
) -> Result<u64, ControllerError> {
    let current: u64 =
        sqlx::query_scalar("SELECT authz_epoch FROM schema_meta WHERE singleton = 1")
            .fetch_one(&mut **tx)
            .await?;
    let next = current
        .checked_add(1)
        .ok_or(ControllerError::RevisionConflict)?;
    let changed = sqlx::query(
        "UPDATE schema_meta SET authz_epoch = ? WHERE singleton = 1 AND authz_epoch = ?",
    )
    .bind(next)
    .bind(current)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(ControllerError::RevisionConflict);
    }
    Ok(next)
}

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

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
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

impl AuthorizationSnapshotRepository for SqlxAuthorizationSnapshotRepository {
    fn load_snapshot<'a>(
        &'a self,
        actor_id: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot> {
        let db = self.db.clone();
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection)
                .await
                .map_err(|_| read_error("snapshot.tx"))?;
            let snapshot = load_snapshot_in_tx(&mut tx, actor_id, device_ids).await?;
            tx.commit().await.map_err(|_| read_error("snapshot.tx"))?;
            Ok(snapshot)
        })
    }

    fn load_admin_target_snapshot<'a>(
        &'a self,
        caller: &'a crate::auth::service::Session,
        target_user: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot> {
        let db = self.db.clone();
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection)
                .await
                .map_err(|_| read_error("snapshot.tx"))?;
            let snapshot =
                load_admin_target_snapshot_in_tx(&mut tx, caller, target_user, device_ids).await?;
            tx.commit().await.map_err(|_| read_error("snapshot.tx"))?;
            Ok(snapshot)
        })
    }

    fn load_admin_target_page<'a>(
        &'a self,
        caller: &'a Session,
        target: [u8; 16],
        device_id: Option<[u8; 32]>,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> RepoFuture<'a, serde_json::Value> {
        let db = &self.db;
        Box::pin(async move {
            if !(1..=200).contains(&limit) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut connection = acquire_authorization_read_connection(db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            // 同连接同事务：caller 当前 admin（DB raw_bool），行缺失 → PermissionDenied。
            ensure_actor_admin_readonly(&mut tx, caller).await?;
            // target 存在性同 tx 检查：缺失是 protected 目标 404，不误映 PermissionDenied；
            // target inactive / must_change_password 属合法存在，来源/权限可为空。
            let exists = sqlx::query_scalar::<_, Vec<u8>>("SELECT id FROM users WHERE id=?")
                .bind(target.as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| read_error("snapshot.actor"))?;
            if exists.is_none() {
                return Err(ControllerError::NotFound);
            }
            let keys: Vec<[u8; 32]> = match device_id {
                Some(id) => vec![id],
                None => {
                    let scan_limit =
                        u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
                    sqlx::query_scalar::<_, Vec<u8>>(ADMIN_TARGET_DEVICE_KEYS_SQL)
                        .bind(cursor.map(|key| key.to_vec()))
                        .bind(cursor.map(|key| key.to_vec()))
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await?
                        .into_iter()
                        .map(|raw| raw.try_into().map_err(|_| read_error("snapshot.device_id")))
                        .collect::<Result<Vec<_>, _>>()?
                }
            };
            // 单快照：target 的 grants/roles/device_groups/devices 全部在同一 tx 内加载，
            // schema_meta 单例与版本防线由 load_snapshot_in_tx 承担，不另开第二套。
            let snapshot = load_snapshot_in_tx(&mut tx, target, &keys).await?;
            if let Some(id) = device_id
                && !snapshot.devices.get(&id).is_some_and(|view| !view.archived)
            {
                return Err(ControllerError::NotFound);
            }
            let has_next = device_id.is_none() && keys.len() > limit;
            let mut items = Vec::new();
            let mut last = None;
            for key in keys.iter().take(limit) {
                let permissions =
                    crate::authz::service::snapshot_effective_permissions(&snapshot, *key);
                let mut effective: Vec<_> = permissions
                    .iter()
                    .map(|permission| permission.as_str())
                    .collect();
                effective.sort_unstable();
                let sources = crate::authz::service::snapshot_permission_sources(&snapshot, *key)
                    .into_iter()
                    .map(|entry| {
                        let source = match entry.source {
                            crate::authz::policy::PermissionSourceKind::Admin => {
                                serde_json::json!({"kind":"admin"})
                            }
                            crate::authz::policy::PermissionSourceKind::Role(id) => {
                                serde_json::json!({"kind":"role","role_id":id.to_string()})
                            }
                            crate::authz::policy::PermissionSourceKind::Direct(id) => {
                                serde_json::json!({"kind":"direct","grant_id":id.to_string()})
                            }
                        };
                        serde_json::json!({"permission":entry.permission.as_str(),"source":source})
                    })
                    .collect::<Vec<_>>();
                items.push(serde_json::json!({
                    "device_id": hex::encode(key),
                    "effective_permissions": effective,
                    "sources": sources,
                }));
                last = Some(*key);
            }
            tx.commit().await?;
            // 内部 raw key（末项公钥 hex64）；出口签名归 HTTP 层。
            Ok(serde_json::json!({
                "items": items,
                "next_cursor": if has_next { last.map(hex::encode) } else { None },
            }))
        })
    }
}

impl AuthzRepository for SqlxAuthzRepository {
    fn find_user<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 16],
    ) -> RepoFuture<'a, Option<UserRecord>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let row = sqlx::query(FIND_USER_SQL)
                .bind(id.as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| read_error("users.find"))?;

            let res = match row {
                Some(r) => Some(decode_user(
                    r.try_get("id").map_err(|_| read_error("users.decode"))?,
                    r.try_get("username")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("display_name")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("active")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("is_admin")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("must_change_password")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("revision")
                        .map_err(|_| read_error("users.decode"))?,
                )?),
                None => None,
            };
            tx.commit().await?;
            Ok(res)
        })
    }

    fn list_users<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 16]>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<UserRecord>, Option<[u8; 16]>)> {
        Box::pin(async move {
            if !(1..=200).contains(&limit) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            let scan_limit =
                u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
            let rows = match cursor {
                Some(cur) => {
                    sqlx::query(LIST_USERS_SQL)
                        .bind(cur.as_slice())
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await
                }
                None => {
                    sqlx::query(LIST_USERS_FIRST_SQL)
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await
                }
            }
            .map_err(|_| read_error("users.list"))?;

            use sqlx::Row;
            let mut items = Vec::new();
            for r in rows {
                items.push(decode_user(
                    r.try_get("id").map_err(|_| read_error("users.decode"))?,
                    r.try_get("username")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("display_name")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("active")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("is_admin")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("must_change_password")
                        .map_err(|_| read_error("users.decode"))?,
                    r.try_get("revision")
                        .map_err(|_| read_error("users.decode"))?,
                )?);
            }

            let (take, next_cursor) = if items.len() > limit {
                (limit, Some(items[limit - 1].id))
            } else {
                (items.len(), None)
            };
            items.truncate(take);
            tx.commit().await?;
            Ok((items, next_cursor))
        })
    }

    fn find_role<'a>(&'a self, actor: &'a Session, id: Uuid) -> RepoFuture<'a, Option<RoleRecord>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let row = sqlx::query(FIND_ROLE_SQL)
                .bind(id.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| read_error("roles.find"))?;

            let res = match row {
                Some(r) => {
                    let perm_rows = sqlx::query("SELECT permission FROM role_permissions WHERE role_id = ? ORDER BY permission")
                        .bind(id.as_bytes().as_slice())
                        .fetch_all(&mut *tx)
                        .await
                        .map_err(|_| read_error("roles.permissions"))?;
                    let mut permissions = Vec::new();
                    for pr in perm_rows {
                        let text: String = pr
                            .try_get("permission")
                            .map_err(|_| read_error("roles.permissions"))?;
                        let p = Permission::parse(&text)
                            .ok_or_else(|| read_error("roles.permissions"))?;
                        permissions.push(p);
                    }
                    let is_archived = crate::integrity::raw_bool(
                        r.try_get("archived")
                            .map_err(|_| read_error("roles.archived"))?,
                        "roles.archived",
                    )?;
                    let is_builtin = crate::integrity::raw_bool(
                        r.try_get("builtin")
                            .map_err(|_| read_error("roles.builtin"))?,
                        "roles.builtin",
                    )?;
                    if !is_archived && permissions.is_empty() {
                        return Err(read_error("roles.permissions"));
                    }
                    Some(RoleRecord {
                        id,
                        name: r.try_get("name").map_err(|_| read_error("roles.name"))?,
                        builtin: is_builtin,
                        archived: is_archived,
                        revision: r
                            .try_get("revision")
                            .map_err(|_| read_error("roles.revision"))?,
                        permissions,
                    })
                }
                None => None,
            };
            tx.commit().await?;
            Ok(res)
        })
    }

    fn list_roles<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<RoleRecord>, Option<Uuid>)> {
        Box::pin(async move {
            if !(1..=200).contains(&limit) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            let scan_limit =
                u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
            let rows = match cursor {
                Some(cur) => {
                    sqlx::query(LIST_ROLES_SQL)
                        .bind(cur.as_bytes().as_slice())
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await
                }
                None => {
                    sqlx::query(LIST_ROLES_FIRST_SQL)
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await
                }
            }
            .map_err(|_| read_error("roles.list"))?;

            use sqlx::Row;
            let mut raw_roles = Vec::new();
            for r in rows {
                let id_raw: Vec<u8> = r.try_get("id").map_err(|_| read_error("roles.decode"))?;
                let id = Uuid::from_slice(&id_raw).map_err(|_| read_error("roles.decode"))?;
                let name: String = r.try_get("name").map_err(|_| read_error("roles.decode"))?;
                let builtin = crate::integrity::raw_bool(
                    r.try_get("builtin")
                        .map_err(|_| read_error("roles.decode"))?,
                    "roles.builtin",
                )?;
                let archived = crate::integrity::raw_bool(
                    r.try_get("archived")
                        .map_err(|_| read_error("roles.decode"))?,
                    "roles.archived",
                )?;
                let revision: u64 = r
                    .try_get("revision")
                    .map_err(|_| read_error("roles.decode"))?;
                raw_roles.push((id, name, builtin, archived, revision));
            }

            let mut items = Vec::new();
            for (id, name, builtin, archived, revision) in raw_roles {
                let perm_rows = sqlx::query(
                    "SELECT permission FROM role_permissions WHERE role_id = ? ORDER BY permission",
                )
                .bind(id.as_bytes().as_slice())
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| read_error("roles.permissions"))?;
                let mut permissions = Vec::new();
                for pr in perm_rows {
                    let text: String = pr
                        .try_get("permission")
                        .map_err(|_| read_error("roles.permissions"))?;
                    let p =
                        Permission::parse(&text).ok_or_else(|| read_error("roles.permissions"))?;
                    permissions.push(p);
                }
                if !archived && permissions.is_empty() {
                    return Err(read_error("roles.permissions"));
                }
                items.push(RoleRecord {
                    id,
                    name,
                    builtin,
                    archived,
                    revision,
                    permissions,
                });
            }

            let (take, next_cursor) = if items.len() > limit {
                (limit, Some(items[limit - 1].id))
            } else {
                (items.len(), None)
            };
            items.truncate(take);
            tx.commit().await?;
            Ok((items, next_cursor))
        })
    }

    fn find_grant<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
    ) -> RepoFuture<'a, Option<GrantRecord>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let row = sqlx::query(FIND_GRANT_SQL)
                .bind(id.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| read_error("grants.find"))?;

            let res = match row {
                Some(r) => {
                    let permissions_raw: Option<String> = r
                        .try_get("permissions")
                        .map_err(|_| read_error("grants.decode"))?;
                    let permissions = match permissions_raw {
                        Some(text) => Some(
                            serde_json::from_str(&text).map_err(|_| read_error("grants.decode"))?,
                        ),
                        None => None,
                    };
                    let grant = decode_grant(
                        r.try_get("id").map_err(|_| read_error("grants.decode"))?,
                        r.try_get("user_id")
                            .map_err(|_| read_error("grants.decode"))?,
                        r.try_get("source_kind")
                            .map_err(|_| read_error("grants.decode"))?,
                        r.try_get("role_id")
                            .map_err(|_| read_error("grants.decode"))?,
                        permissions,
                        r.try_get("scope_kind")
                            .map_err(|_| read_error("grants.decode"))?,
                        r.try_get("scope_group_id")
                            .map_err(|_| read_error("grants.decode"))?,
                        r.try_get("scope_device_id")
                            .map_err(|_| read_error("grants.decode"))?,
                        r.try_get("revision")
                            .map_err(|_| read_error("grants.decode"))?,
                    )
                    .map_err(|_| read_error("grants.decode"))?;
                    // 持久引用完整性：owner/role/group/device 任一缺失 = 污染，固定去敏 Config，
                    // 绝不把孤儿 grant 当合法行返回。
                    validate_snapshot_grant_references(
                        &grant,
                        r.try_get("owner_exists")
                            .map_err(|_| read_error("grants.reference"))?,
                        r.try_get("role_exists")
                            .map_err(|_| read_error("grants.reference"))?,
                        r.try_get("group_exists")
                            .map_err(|_| read_error("grants.reference"))?,
                        r.try_get("device_exists")
                            .map_err(|_| read_error("grants.reference"))?,
                    )
                    .map_err(|_| read_error("grants.reference"))?;
                    Some(grant)
                }
                None => None,
            };
            tx.commit().await?;
            Ok(res)
        })
    }

    fn list_grants<'a>(
        &'a self,
        actor: &'a Session,
        user_id: Option<[u8; 16]>,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<GrantRecord>, Option<Uuid>)> {
        Box::pin(async move {
            if !(1..=200).contains(&limit) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            let scan_limit =
                u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
            let u_raw = user_id.map(|u| u.to_vec());
            let c_raw = cursor.map(|c| c.as_bytes().to_vec());

            use sqlx::Row;
            let rows = sqlx::query(LIST_GRANTS_SQL)
                .bind(u_raw.as_deref())
                .bind(u_raw.as_deref())
                .bind(c_raw.as_deref())
                .bind(c_raw.as_deref())
                .bind(scan_limit)
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| read_error("grants.list"))?;

            let mut items = Vec::new();
            for r in rows {
                let permissions_raw: Option<String> = r
                    .try_get("permissions")
                    .map_err(|_| read_error("grants.decode"))?;
                let permissions = match permissions_raw {
                    Some(text) => {
                        Some(serde_json::from_str(&text).map_err(|_| read_error("grants.decode"))?)
                    }
                    None => None,
                };
                let grant = decode_grant(
                    r.try_get("id").map_err(|_| read_error("grants.decode"))?,
                    r.try_get("user_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("source_kind")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("role_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    permissions,
                    r.try_get("scope_kind")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("scope_group_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("scope_device_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("revision")
                        .map_err(|_| read_error("grants.decode"))?,
                )
                .map_err(|_| read_error("grants.decode"))?;
                // 持久引用完整性：owner/role/group/device 任一缺失 = 污染，固定去敏 Config，
                // 绝不把孤儿 grant 当合法行返回。
                validate_snapshot_grant_references(
                    &grant,
                    r.try_get("owner_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                    r.try_get("role_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                    r.try_get("group_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                    r.try_get("device_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                )
                .map_err(|_| read_error("grants.reference"))?;
                items.push(grant);
            }

            let (take, next_cursor) = if items.len() > limit {
                (limit, Some(items[limit - 1].id))
            } else {
                (items.len(), None)
            };
            items.truncate(take);
            tx.commit().await?;
            Ok((items, next_cursor))
        })
    }

    fn list_grants_for_user<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
    ) -> RepoFuture<'a, Vec<GrantRecord>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let rows = sqlx::query(LIST_GRANTS_FOR_USER_SQL)
                .bind(user_id.as_slice())
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| read_error("grants.list_for_user"))?;

            let mut items = Vec::new();
            for r in rows {
                let permissions_raw: Option<String> = r
                    .try_get("permissions")
                    .map_err(|_| read_error("grants.decode"))?;
                let permissions = match permissions_raw {
                    Some(text) => {
                        Some(serde_json::from_str(&text).map_err(|_| read_error("grants.decode"))?)
                    }
                    None => None,
                };
                let grant = decode_grant(
                    r.try_get("id").map_err(|_| read_error("grants.decode"))?,
                    r.try_get("user_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("source_kind")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("role_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    permissions,
                    r.try_get("scope_kind")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("scope_group_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("scope_device_id")
                        .map_err(|_| read_error("grants.decode"))?,
                    r.try_get("revision")
                        .map_err(|_| read_error("grants.decode"))?,
                )
                .map_err(|_| read_error("grants.decode"))?;
                // 持久引用完整性：owner/role/group/device 任一缺失 = 污染，固定去敏 Config，
                // 绝不把孤儿 grant 当合法行返回。
                validate_snapshot_grant_references(
                    &grant,
                    r.try_get("owner_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                    r.try_get("role_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                    r.try_get("group_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                    r.try_get("device_exists")
                        .map_err(|_| read_error("grants.reference"))?,
                )
                .map_err(|_| read_error("grants.reference"))?;
                items.push(grant);
            }
            tx.commit().await?;
            Ok(items)
        })
    }

    fn roles_by_id<'a>(
        &'a self,
        actor: &'a Session,
        ids: Vec<Uuid>,
    ) -> RepoFuture<'a, HashMap<Uuid, RoleRecord>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            if ids.is_empty() {
                tx.commit().await?;
                return Ok(HashMap::new());
            }

            let mut unique_ids: Vec<Uuid> = ids
                .into_iter()
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            unique_ids.sort();

            let ph = placeholders(unique_ids.len());
            let sql = ROLES_BY_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph);
            let mut q = sqlx::query(&sql);
            for rid in &unique_ids {
                q = q.bind(rid.as_bytes().as_slice());
            }
            let rows = q
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| read_error("roles.by_id"))?;

            use sqlx::Row;
            let mut raw_roles = Vec::new();
            for r in rows {
                let id_raw: Vec<u8> = r.try_get("id").map_err(|_| read_error("roles.decode"))?;
                let id = Uuid::from_slice(&id_raw).map_err(|_| read_error("roles.decode"))?;
                let name: String = r.try_get("name").map_err(|_| read_error("roles.decode"))?;
                let builtin = crate::integrity::raw_bool(
                    r.try_get("builtin")
                        .map_err(|_| read_error("roles.decode"))?,
                    "roles.builtin",
                )?;
                let archived = crate::integrity::raw_bool(
                    r.try_get("archived")
                        .map_err(|_| read_error("roles.decode"))?,
                    "roles.archived",
                )?;
                let revision: u64 = r
                    .try_get("revision")
                    .map_err(|_| read_error("roles.decode"))?;
                raw_roles.push((id, name, builtin, archived, revision));
            }

            let mut out = HashMap::new();
            for (id, name, builtin, archived, revision) in raw_roles {
                let perm_rows = sqlx::query(
                    "SELECT permission FROM role_permissions WHERE role_id = ? ORDER BY permission",
                )
                .bind(id.as_bytes().as_slice())
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| read_error("roles.permissions"))?;
                let mut permissions = Vec::new();
                for pr in perm_rows {
                    let text: String = pr
                        .try_get("permission")
                        .map_err(|_| read_error("roles.permissions"))?;
                    let p =
                        Permission::parse(&text).ok_or_else(|| read_error("roles.permissions"))?;
                    permissions.push(p);
                }
                if !archived && permissions.is_empty() {
                    return Err(read_error("roles.permissions"));
                }
                out.insert(
                    id,
                    RoleRecord {
                        id,
                        name,
                        builtin,
                        archived,
                        revision,
                        permissions,
                    },
                );
            }

            tx.commit().await?;
            Ok(out)
        })
    }

    fn device_groups_for_device<'a>(
        &'a self,
        actor: &'a Session,
        device_id: [u8; 32],
    ) -> RepoFuture<'a, HashSet<Uuid>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let rows = sqlx::query(DEVICE_GROUPS_FOR_DEVICE_SQL)
                .bind(device_id.as_slice())
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| read_error("groups.for_device"))?;

            let raw: Vec<(Vec<u8>, i64, i64, i64, i64)> = rows
                .iter()
                .map(|r| -> Result<_, ControllerError> {
                    Ok((
                        r.try_get("group_id")
                            .map_err(|_| read_error("groups.for_device"))?,
                        r.try_get("group_exists")
                            .map_err(|_| read_error("groups.for_device"))?,
                        r.try_get("group_archived")
                            .map_err(|_| read_error("groups.for_device"))?,
                        r.try_get("device_exists")
                            .map_err(|_| read_error("groups.for_device"))?,
                        r.try_get("device_archived")
                            .map_err(|_| read_error("groups.for_device"))?,
                    ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let out = decode_group_memberships_for_device(&raw)?;

            tx.commit().await?;
            Ok(out)
        })
    }

    fn find_group<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
    ) -> RepoFuture<'a, Option<GroupRecord>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let row = sqlx::query(FIND_GROUP_SQL)
                .bind(id.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| read_error("groups.find"))?;

            let res = match row {
                Some(r) => {
                    let name: Option<String> =
                        r.try_get("name").map_err(|_| read_error("groups.decode"))?;
                    match name {
                        Some(n) => Some(GroupRecord {
                            id,
                            name: n,
                            archived: crate::integrity::raw_bool(
                                r.try_get("archived")
                                    .map_err(|_| read_error("groups.decode"))?,
                                "groups.archived",
                            )?,
                            revision: r
                                .try_get("revision")
                                .map_err(|_| read_error("groups.decode"))?,
                            member_count: {
                                let c: i64 = r
                                    .try_get("member_count")
                                    .map_err(|_| read_error("groups.decode"))?;
                                c as u64
                            },
                        }),
                        None => None,
                    }
                }
                None => None,
            };
            tx.commit().await?;
            Ok(res)
        })
    }

    fn list_groups<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<GroupRecord>, Option<Uuid>)> {
        Box::pin(async move {
            if !(1..=200).contains(&limit) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            let scan_limit =
                u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
            let rows = match cursor {
                Some(cur) => {
                    sqlx::query(LIST_GROUPS_SQL)
                        .bind(cur.as_bytes().as_slice())
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await
                }
                None => {
                    sqlx::query(LIST_GROUPS_FIRST_SQL)
                        .bind(scan_limit)
                        .fetch_all(&mut *tx)
                        .await
                }
            }
            .map_err(|_| read_error("groups.list"))?;

            use sqlx::Row;
            let mut items = Vec::new();
            for r in rows {
                let id_raw: Vec<u8> = r.try_get("id").map_err(|_| read_error("groups.decode"))?;
                let id = Uuid::from_slice(&id_raw).map_err(|_| read_error("groups.decode"))?;
                let name: String = r.try_get("name").map_err(|_| read_error("groups.decode"))?;
                let archived = crate::integrity::raw_bool(
                    r.try_get("archived")
                        .map_err(|_| read_error("groups.decode"))?,
                    "groups.archived",
                )?;
                let revision: u64 = r
                    .try_get("revision")
                    .map_err(|_| read_error("groups.decode"))?;
                let member_count: i64 = r
                    .try_get("member_count")
                    .map_err(|_| read_error("groups.decode"))?;
                items.push(GroupRecord {
                    id,
                    name,
                    archived,
                    revision,
                    member_count: member_count as u64,
                });
            }

            let (take, next_cursor) = if items.len() > limit {
                (limit, Some(items[limit - 1].id))
            } else {
                (items.len(), None)
            };
            items.truncate(take);
            tx.commit().await?;
            Ok((items, next_cursor))
        })
    }

    fn list_group_device_ids<'a>(
        &'a self,
        actor: &'a Session,
        group_id: Uuid,
    ) -> RepoFuture<'a, Vec<[u8; 32]>> {
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&self.db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await?;
            verify_management_read(&mut tx, actor).await?;

            use sqlx::Row;
            let rows = sqlx::query(LIST_GROUP_DEVICE_IDS_SQL)
                .bind(group_id.as_bytes().as_slice())
                .fetch_all(&mut *tx)
                .await
                .map_err(|_| read_error("groups.device_ids"))?;

            let mut out = Vec::new();
            for r in rows {
                let raw: Vec<u8> = r
                    .try_get("device_id")
                    .map_err(|_| read_error("groups.device_ids"))?;
                let key: [u8; 32] = raw
                    .try_into()
                    .map_err(|_| read_error("groups.device_ids"))?;
                out.push(key);
            }
            tx.commit().await?;
            Ok(out)
        })
    }

    // ---- Task 4 管理写方法：统一事务骨架 guard → 行锁顺序 → CAS →
    // LastAdmin → 审计同事务插入 → commit；任一错误 drop 局部 tx 自动回滚。----
    fn create_user<'a>(
        &'a self,
        actor: &'a Session,
        username: &'a str,
        display_name: &'a str,
        is_admin: bool,
        next_hash: &'a str,
    ) -> RepoFuture<'a, UserRecord> {
        Box::pin(async move {
            // 门禁1：display_name 合法（复用既有 validate_display_name）。
            validate_display_name(display_name)?;
            // 门禁2：next_hash 非空（Argon2id 临时密码哈希，事务前即拒绝空串）。
            if next_hash.is_empty() {
                return Err(ControllerError::InvalidArgument);
            }
            // 门禁3：username 规范（复用 crate::db::valid_username：非空、无空白/控制符）。
            if !crate::db::valid_username(username) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut tx = begin_guard(&self.db).await?;
            crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
            // 新行插入无实体行锁（guard 行锁已串行化全部管理员写）。
            let id = Uuid::new_v4();
            if let Err(error) = sqlx::query(
                "INSERT INTO users (id, username, display_name, password_hash, active, is_admin, must_change_password, revision, created_time) VALUES (?, ?, ?, ?, TRUE, ?, TRUE, 1, NOW(6))",
            )
            .bind(id.as_bytes().as_slice())
            .bind(username)
            .bind(display_name)
            .bind(next_hash)
            .bind(is_admin)
            .execute(&mut *tx)
            .await
            {
                // 唯一冲突固定 InvalidState(409)，不回显 driver 原文；其他 SQL 错误透传
                // Database（既有 backend error 统一 503 去敏）。username 唯一键
                // uq_users_username 与 id 主键均为唯一冲突，不识别具体索引。
                let unique_conflict = error
                    .as_database_error()
                    .is_some_and(|e| e.is_unique_violation());
                return Err(if unique_conflict {
                    ControllerError::InvalidState
                } else {
                    error.into()
                });
            }
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.user.create",
                    target_kind: "user",
                    target_id: Some(hex::encode(id.as_bytes())),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            // commit 前构造安全 UserRecord 全部 7 字段（绝不含 password_hash）；仅 commit 成功才 Ok(record)。
            let record = UserRecord {
                id: id.into_bytes(),
                username: username.to_string(),
                display_name: display_name.to_string(),
                active: true,
                is_admin,
                must_change_password: true,
                revision: 1,
            };
            tx.commit().await?;
            Ok(record)
        })
    }

    fn patch_user<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        expected_revision: u64,
        active: Option<bool>,
        is_admin: Option<bool>,
        display_name: Option<&'a str>,
    ) -> RepoFuture<'a, UserRecord> {
        Box::pin(async move {
            let mut tx = begin_guard(&self.db).await?;
            let target = lock_user_pair_and_verify_actor(&mut tx, actor.user.id, user_id)
                .await?
                .ok_or(ControllerError::NotFound)?;
            if target.revision != expected_revision {
                return Err(ControllerError::RevisionConflict);
            }
            let next_active = active.unwrap_or(target.active);
            let next_admin = is_admin.unwrap_or(target.is_admin);
            let next_display = match display_name {
                Some(value) => {
                    validate_display_name(value)?;
                    value.to_string()
                }
                None => target.display_name.clone(),
            };
            if admin_change_removes_admin_state(
                target.active,
                target.is_admin,
                next_active,
                next_admin,
            ) {
                let count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM users WHERE active = TRUE AND is_admin = TRUE",
                )
                .fetch_one(&mut *tx)
                .await?;
                // guard 行锁已串行化全部管理员写，故此计数在本事务内稳定。
                if let Err(error) = check_admin_demotion_allowed(count) {
                    // 此时尚无业务UPDATE：只提交拒绝审计，不bump epoch/不撤销sessions；
                    // 错误路径唯一明确 commit 例外（其余错误靠 drop 局部 tx 自动回滚）。
                    crate::audit::insert_audit(
                        &mut tx,
                        &crate::audit::AuditInsert {
                            actor_kind: "user",
                            actor_user_id: Some(actor.user.id),
                            event_type: "identity.user.last_admin_blocked",
                            target_kind: "user",
                            target_id: Some(hex::encode(user_id)),
                            params_redacted: "{}",
                            outcome: "denied",
                        },
                    )
                    .await?;
                    tx.commit().await?;
                    return Err(error);
                }
            }
            let next_revision = target
                .revision
                .checked_add(1)
                .ok_or(ControllerError::RevisionConflict)?;
            let changed = sqlx::query(
                "UPDATE users SET active = ?, is_admin = ?, display_name = ?, revision = ? WHERE id = ? AND revision = ?",
            )
            .bind(next_active)
            .bind(next_admin)
            .bind(&next_display)
            .bind(next_revision)
            .bind(user_id.as_slice())
            .bind(expected_revision)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if changed != 1 {
                return Err(ControllerError::RevisionConflict);
            }
            if target.active && !next_active {
                // 停用必须在同事务撤销该用户全部会话。
                let revoked = sqlx::query(
                    "UPDATE sessions SET revoked = TRUE WHERE user_id = ? AND revoked = FALSE",
                )
                .bind(user_id.as_slice())
                .execute(&mut *tx)
                .await?
                .rows_affected();
                crate::audit::insert_audit(
                    &mut tx,
                    &crate::audit::AuditInsert {
                        actor_kind: "user",
                        actor_user_id: Some(actor.user.id),
                        event_type: "identity.sessions.revoke",
                        target_kind: "user",
                        target_id: Some(hex::encode(user_id)),
                        params_redacted: "{}",
                        outcome: if revoked > 0 {
                            "success"
                        } else {
                            "no_active_sessions"
                        },
                    },
                )
                .await?;
            }
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.user.patch",
                    target_kind: "user",
                    target_id: Some(hex::encode(user_id)),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            // commit 前构造安全 UserRecord 全部 7 字段（无 password_hash）；仅 commit 成功才 Ok(updated)。
            let updated = UserRecord {
                id: user_id,
                username: target.username,
                display_name: next_display,
                active: next_active,
                is_admin: next_admin,
                must_change_password: target.must_change_password,
                revision: next_revision,
            };
            tx.commit().await?;
            Ok(updated)
        })
    }

    fn reset_user_password<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        expected_revision: u64,
        next_hash: &'a str,
    ) -> RepoFuture<'a, UserRecord> {
        Box::pin(async move {
            // 空哈希直接 InvalidArgument，不进入事务。
            if next_hash.is_empty() {
                return Err(ControllerError::InvalidArgument);
            }
            let mut tx = begin_guard(&self.db).await?;
            // 按 UUID 字节序锁 actor 与 target 行，并由锁定行重验 actor 仍为有效管理员；行缺失 → NotFound。
            let target = lock_user_pair_and_verify_actor(&mut tx, actor.user.id, user_id)
                .await?
                .ok_or(ControllerError::NotFound)?;
            if target.revision != expected_revision {
                return Err(ControllerError::RevisionConflict);
            }
            let next_revision = target
                .revision
                .checked_add(1)
                .ok_or(ControllerError::RevisionConflict)?;
            let changed = sqlx::query(
                "UPDATE users SET password_hash = ?, must_change_password = TRUE, revision = ? WHERE id = ? AND revision = ?",
            )
            .bind(next_hash)
            .bind(next_revision)
            .bind(user_id.as_slice())
            .bind(expected_revision)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if changed != 1 {
                return Err(ControllerError::RevisionConflict);
            }
            // 同事务无条件强制撤销该目标用户全部活动会话。
            sqlx::query("UPDATE sessions SET revoked = TRUE WHERE user_id = ? AND revoked = FALSE")
                .bind(user_id.as_slice())
                .execute(&mut *tx)
                .await?;
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.user.reset_password",
                    target_kind: "user",
                    target_id: Some(hex::encode(user_id)),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            // commit 前构造安全 UserRecord 全部 7 字段（无 password_hash）；
            // must_change_password 恒 true；仅 commit 成功才 Ok(record)。
            let record = UserRecord {
                id: user_id,
                username: target.username,
                display_name: target.display_name,
                active: target.active,
                is_admin: target.is_admin,
                must_change_password: true,
                revision: next_revision,
            };
            tx.commit().await?;
            Ok(record)
        })
    }

    fn create_role<'a>(
        &'a self,
        actor: &'a Session,
        name: &'a str,
        permissions: &'a [Permission],
    ) -> RepoFuture<'a, RoleRecord> {
        Box::pin(async move {
            // 权限列表门禁复用 validate_permission_list（非空、无重复），不复制第二份 helper。
            crate::authz::policy::validate_permission_list(permissions)?;
            // name 门禁与 validate_group_name/validate_display_name 同款规则：非空、≤128 Unicode 字符、无控制字符。
            if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut tx = begin_guard(&self.db).await?;
            crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
            // 新行插入，无实体行锁；id 主键与 name 唯一键 uq_roles_name 均为唯一冲突来源。
            let id = Uuid::new_v4();
            if let Err(error) = sqlx::query(
                "INSERT INTO roles (id, name, builtin, archived, revision) VALUES (?, ?, FALSE, FALSE, 1)",
            )
            .bind(id.as_bytes().as_slice())
            .bind(name)
            .execute(&mut *tx)
            .await
            {
                // 唯一冲突固定 InvalidState(409)，不回显 driver 原文；其他 SQL 错误透传
                // Database。用驱动 unique-violation 分类（kind()==UniqueViolation）：不解析
                // driver message 文本，也不依赖 Postgres-only 的 constraint() 名称映射。
                let unique_conflict = error
                    .as_database_error()
                    .is_some_and(|e| e.is_unique_violation());
                return Err(if unique_conflict {
                    ControllerError::InvalidState
                } else {
                    error.into()
                });
            }
            // 全部权限逐条插入 role_permissions（permission 文本与 Permission::parse 互逆）。
            for permission in permissions {
                sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES (?, ?)")
                    .bind(id.as_bytes().as_slice())
                    .bind(permission.as_str())
                    .execute(&mut *tx)
                    .await?;
            }
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.role.create",
                    target_kind: "role",
                    target_id: Some(hex::encode(id.as_bytes())),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            // commit 前安全组装 RoleRecord 全部 6 字段；仅 commit 成功才 Ok(record)。
            let record = RoleRecord {
                id,
                name: name.to_string(),
                builtin: false,
                archived: false,
                revision: 1,
                permissions: permissions.to_vec(),
            };
            tx.commit().await?;
            Ok(record)
        })
    }

    fn patch_role<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        expected_revision: u64,
        name: Option<&'a str>,
        permissions: Option<&'a [Permission]>,
        archived: Option<bool>,
    ) -> RepoFuture<'a, RoleRecord> {
        Box::pin(async move {
            use sqlx::Row;
            if let Some(perms) = permissions {
                crate::authz::policy::validate_permission_list(perms)?;
            }
            if let Some(n) = name
                && (n.is_empty() || n.chars().count() > 128 || n.chars().any(char::is_control))
            {
                return Err(ControllerError::InvalidArgument);
            }
            let mut tx = begin_guard(&self.db).await?;
            crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
            let (locked_name, locked_builtin, locked_archived, current_revision) = sqlx::query(
                "SELECT name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles WHERE id = ? FOR UPDATE",
            )
            .bind(id.as_bytes().as_slice())
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| {
                Ok::<_, ControllerError>((
                    r.try_get::<String, _>("name")?,
                    crate::integrity::raw_bool(r.try_get("builtin")?, "roles.builtin")?,
                    crate::integrity::raw_bool(r.try_get("archived")?, "roles.archived")?,
                    r.try_get::<u64, _>("revision")?,
                ))
            })
            .transpose()?
            .ok_or(ControllerError::NotFound)?;

            if current_revision != expected_revision {
                return Err(ControllerError::RevisionConflict);
            }
            if locked_builtin && archived == Some(true) {
                return Err(ControllerError::InvalidState);
            }

            let next_name = name.map(str::to_string).unwrap_or(locked_name);
            let next_archived = archived.unwrap_or(locked_archived);
            // 恢复分支门禁（方案 A）：从归档恢复必须携带合法非空 permissions，否则 UPDATE 前
            // `InvalidArgument`(400) 拒绝，绝不留下 active 空权限非法状态。
            validate_role_restore(locked_archived, next_archived, permissions)?;
            let next_revision = current_revision
                .checked_add(1)
                .ok_or(ControllerError::RevisionConflict)?;

            if next_archived && !locked_archived {
                sqlx::query("DELETE FROM grants WHERE source_kind = 'role' AND role_id = ?")
                    .bind(id.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM role_permissions WHERE role_id = ?")
                    .bind(id.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await?;
            }

            let updated = sqlx::query(
                "UPDATE roles SET name = ?, archived = ?, revision = ? WHERE id = ? AND revision = ?",
            )
            .bind(&next_name)
            .bind(next_archived)
            .bind(next_revision)
            .bind(id.as_bytes().as_slice())
            .bind(expected_revision)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                // 驱动 unique-violation 分类（同 create_role 口径：不解析 message 文本）。
                if error
                    .as_database_error()
                    .is_some_and(|e| e.is_unique_violation())
                {
                    ControllerError::InvalidState
                } else {
                    error.into()
                }
            })?;
            if updated.rows_affected() != 1 {
                return Err(ControllerError::RevisionConflict);
            }

            if let Some(perms) = permissions
                && !next_archived
            {
                sqlx::query("DELETE FROM role_permissions WHERE role_id = ?")
                    .bind(id.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await?;
                for p in perms {
                    sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES (?, ?)")
                        .bind(id.as_bytes().as_slice())
                        .bind(p.as_str())
                        .execute(&mut *tx)
                        .await?;
                }
            }

            let permissions: Vec<Permission> = if next_archived {
                Vec::new()
            } else {
                sqlx::query("SELECT permission FROM role_permissions WHERE role_id = ?")
                    .bind(id.as_bytes().as_slice())
                    .fetch_all(&mut *tx)
                    .await?
                    .into_iter()
                    .map(|r| {
                        Permission::parse(&r.try_get::<String, _>("permission")?)
                            .ok_or(ControllerError::InvalidState)
                    })
                    .collect::<Result<_, _>>()?
            };

            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.role.patch",
                    target_kind: "role",
                    target_id: Some(hex::encode(id.as_bytes())),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;

            tx.commit().await?;
            Ok(RoleRecord {
                id,
                name: next_name,
                builtin: locked_builtin,
                archived: next_archived,
                revision: next_revision,
                permissions,
            })
        })
    }

    fn create_grant<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        source: GrantSource,
        scope: GrantScope,
    ) -> RepoFuture<'a, GrantRecord> {
        Box::pin(async move {
            // 前置校验：纯函数先做 source/scope 互斥与列映射，失败不进入事务。
            let cols = grant_columns(&source, &scope)?;
            let mut tx = begin_guard(&self.db).await?;
            // 锁 1：复用共享助手，按 UUID 字节序锁定 actor 与 target（同一用户仅锁一次），
            // 并在 guard 内重新从行核验 actor active && is_admin（对齐管理写事务契约）。
            let target = lock_user_pair_and_verify_actor(&mut tx, actor.user.id, user_id)
                .await?
                .ok_or(ControllerError::NotFound)?;
            if !target.active {
                return Err(ControllerError::InvalidState);
            }

            if cols.source_kind == "role" {
                let role_id = cols
                    .role_id
                    .expect("source=role 时 grant_columns 必给出 role_id");
                let archived: i64 = sqlx::query_scalar(
                    "SELECT CAST(archived AS SIGNED) FROM roles WHERE id = ? FOR UPDATE",
                )
                .bind(role_id.as_bytes().as_slice())
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(ControllerError::NotFound)?;
                if crate::integrity::raw_bool(archived, "roles.archived")? {
                    return Err(ControllerError::InvalidState);
                }
            }

            // 锁 2：依 scope_kind 锁定唯一对应实体行 FOR UPDATE；行缺失 → NotFound，archived → InvalidState。
            // （`grant_columns` 已按 scope 保证相应列恰有其一，此处不做第二种 scope 解析。）
            match cols.scope_kind {
                "all" => {
                    // All 范围无需锁实体行
                }
                "group" => {
                    let group_id = cols
                        .scope_group_id
                        .expect("scope=group 时 grant_columns 必给出 scope_group_id");
                    let archived: i64 = sqlx::query_scalar(
                        "SELECT CAST(archived AS SIGNED) FROM device_groups WHERE id = ? FOR UPDATE",
                    )
                    .bind(group_id.as_bytes().as_slice())
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(ControllerError::NotFound)?;
                    if crate::integrity::raw_bool(archived, "device_groups.archived")? {
                        return Err(ControllerError::InvalidState);
                    }
                }
                "device" => {
                    let device_id = cols
                        .scope_device_id
                        .expect("scope=device 时 grant_columns 必给出 scope_device_id");
                    let archived: i64 = sqlx::query_scalar(
                        "SELECT CAST(archived AS SIGNED) FROM devices WHERE public_key = ? FOR UPDATE",
                    )
                    .bind(device_id.as_slice())
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(ControllerError::NotFound)?;
                    if crate::integrity::raw_bool(archived, "devices.archived")? {
                        return Err(ControllerError::InvalidState);
                    }
                }
                _ => unreachable!("grant_columns 只产生 all/group/device 三种 scope_kind"),
            }

            // 写入：revision 恒 1；主键/唯一冲突按驱动分类固定 InvalidState(409)，
            // 其余 SQL 错误透传 Database（既有 backend error 统一 503 去敏）。
            let id = Uuid::new_v4();
            let role_binding = cols.role_id.map(|r| r.as_bytes().to_vec());
            let group_binding = cols.scope_group_id.map(|g| g.as_bytes().to_vec());
            let device_binding = cols.scope_device_id.map(|k| k.to_vec());
            if let Err(error) = sqlx::query(
                "INSERT INTO grants (id, user_id, source_kind, role_id, permissions, scope_kind, scope_group_id, scope_device_id, revision) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1)",
            )
            .bind(id.as_bytes().as_slice())
            .bind(user_id.as_slice())
            .bind(cols.source_kind)
            .bind(&role_binding)
            .bind(&cols.permissions_json)
            .bind(cols.scope_kind)
            .bind(&group_binding)
            .bind(&device_binding)
            .execute(&mut *tx)
            .await
            {
                // 用驱动 unique-violation 分类（同 create_group 口径：kind()==UniqueViolation，
                // 不解析 driver message、不依赖 constraint 名称）。
                let unique_conflict = error
                    .as_database_error()
                    .is_some_and(|e| e.is_unique_violation());
                return Err(if unique_conflict {
                    ControllerError::InvalidState
                } else {
                    error.into()
                });
            }
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.grant.create",
                    target_kind: "grant",
                    target_id: Some(hex::encode(id.as_bytes())),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            // commit 前安全组装（source/scope 为入参原样持有；user_id 与 INSERT 绑定同源）。
            let record = GrantRecord {
                id,
                user_id,
                source,
                scope,
                revision: 1,
            };
            tx.commit().await?;
            Ok(record)
        })
    }

    fn delete_grant<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        expected_revision: u64,
    ) -> RepoFuture<'a, ()> {
        Box::pin(async move {
            let mut tx = begin_guard(&self.db).await?;
            crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
            // `grants` 行 `FOR UPDATE`（按 `id.as_bytes().as_slice()` 定位）：行缺失 → `NotFound`，行锁持有至事务结束。
            let current_revision =
                sqlx::query_scalar::<_, u64>("SELECT revision FROM grants WHERE id = ? FOR UPDATE")
                    .bind(id.as_bytes().as_slice())
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(ControllerError::NotFound)?;
            // expected_revision CAS（与 DELETE 的 WHERE revision=? + rows_affected==1 双保险）。
            if current_revision != expected_revision {
                return Err(ControllerError::RevisionConflict);
            }
            // 物理删除：id + revision 双条件，影响行非恰 1 即视为并发修订漂移。
            let deleted = sqlx::query("DELETE FROM grants WHERE id = ? AND revision = ?")
                .bind(id.as_bytes().as_slice())
                .bind(expected_revision)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            if deleted != 1 {
                return Err(ControllerError::RevisionConflict);
            }
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.grant.delete",
                    target_kind: "grant",
                    target_id: Some(hex::encode(id.as_bytes())),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn create_group<'a>(
        &'a self,
        actor: &'a Session,
        name: &'a str,
    ) -> RepoFuture<'a, GroupRecord> {
        Box::pin(async move {
            validate_group_name(name)?;
            let mut tx = begin_guard(&self.db).await?;
            crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
            let id = Uuid::new_v4();
            if let Err(error) = sqlx::query(
                "INSERT INTO device_groups (id, name, archived, revision) VALUES (?, ?, FALSE, 1)",
            )
            .bind(id.as_bytes().as_slice())
            .bind(name)
            .execute(&mut *tx)
            .await
            {
                // 唯一冲突固定 InvalidState(409)，不回显 driver 原文；其他 SQL 错误透传
                // Database。group 名唯一键 uq_device_groups_name 与 id 主键均为唯一冲突来源，
                // 按驱动 is_unique_violation() 分类，不识别具体索引。
                let unique_conflict = error
                    .as_database_error()
                    .is_some_and(|e| e.is_unique_violation());
                return Err(if unique_conflict {
                    ControllerError::InvalidState
                } else {
                    error.into()
                });
            }
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.group.create",
                    target_kind: "group",
                    target_id: Some(hex::encode(id.as_bytes())),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            // commit 前构造安全 GroupRecord 全部 5 字段；仅 commit 成功才 Ok(record)。
            let record = GroupRecord {
                id,
                name: name.to_string(),
                archived: false,
                revision: 1,
                member_count: 0,
            };
            tx.commit().await?;
            Ok(record)
        })
    }

    fn patch_group<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        expected_revision: u64,
        name: Option<&'a str>,
        archived: Option<bool>,
    ) -> RepoFuture<'a, GroupRecord> {
        Box::pin(async move {
            use sqlx::Row;
            // 改名门禁复用 create_group 的 validate_group_name，不复制第二份 helper。
            if let Some(name) = name {
                validate_group_name(name)?;
            }
            let mut tx = begin_guard(&self.db).await?;
            crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
            // 1) 行锁快照当前 name/archived/revision，行缺失 → NotFound。
            let locked = sqlx::query(
                "SELECT name, CAST(archived AS SIGNED) AS archived, revision FROM device_groups WHERE id = ? FOR UPDATE",
            )
            .bind(id.as_bytes().as_slice())
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| {
                Ok::<_, ControllerError>((
                    r.try_get::<String, _>("name")?,
                    crate::integrity::raw_bool(r.try_get("archived")?, "device_groups.archived")?,
                    r.try_get::<u64, _>("revision")?,
                ))
            })
            .transpose()?
            .ok_or(ControllerError::NotFound)?;
            let (current_name, current_archived, current_revision) = locked;
            if current_revision != expected_revision {
                return Err(ControllerError::RevisionConflict);
            }
            let next_revision = current_revision
                .checked_add(1)
                .ok_or(ControllerError::RevisionConflict)?;
            let next_name: String = name.map(str::to_string).unwrap_or(current_name);
            let next_archived = archived.unwrap_or(current_archived);
            // 2) 归档时物理删成员与 group 作用域 grants（保留历史父 group 行、不删其它组）。
            if next_archived {
                sqlx::query("DELETE FROM group_members WHERE group_id = ?")
                    .bind(id.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM grants WHERE scope_kind = 'group' AND scope_group_id = ?")
                    .bind(id.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await?;
            }
            // 3) CAS UPDATE 影响恰 1 行，否则 RevisionConflict；改名唯一冲突 → InvalidState(409)。
            let updated = sqlx::query(
                "UPDATE device_groups SET name = ?, archived = ?, revision = ? WHERE id = ? AND revision = ?",
            )
            .bind(&next_name)
            .bind(next_archived)
            .bind(next_revision)
            .bind(id.as_bytes().as_slice())
            .bind(expected_revision)
            .execute(&mut *tx)
            .await
            .map_err(|error| {
                // 驱动 unique-violation 分类（同 create_group 口径：不解析 message 文本、不依赖 constraint 名称）。
                if error
                    .as_database_error()
                    .is_some_and(|e| e.is_unique_violation())
                {
                    ControllerError::InvalidState
                } else {
                    error.into()
                }
            })?;
            if updated.rows_affected() != 1 {
                return Err(ControllerError::RevisionConflict);
            }
            // 4) 同事务 COUNT(*) 真实回读 member_count（归档后恒 0），不从参数臆造。
            let member_count = sqlx::query(
                "SELECT COUNT(*) AS member_count FROM group_members WHERE group_id = ?",
            )
            .bind(id.as_bytes().as_slice())
            .fetch_one(&mut *tx)
            .await?
            .try_get::<u64, _>("member_count")?;
            bump_authz_epoch(&mut tx).await?;
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.group.patch",
                    target_kind: "group",
                    target_id: Some(hex::encode(id.as_bytes())),
                    params_redacted: "{}",
                    outcome: "success",
                },
            )
            .await?;
            // commit 前安全组装 GroupRecord 全部 5 字段；仅 commit 成功才 Ok(record)。
            let record = GroupRecord {
                id,
                name: next_name,
                archived: next_archived,
                revision: next_revision,
                member_count,
            };
            tx.commit().await?;
            Ok(record)
        })
    }
}

/// 测试专用 fixture：生产 SQL 常量的文本形状门禁。只解析字符串，不连引擎、不伪造
/// `MySqlRow`，因此不声称任何真实引擎行为（真库端到端由受控 executor 复跑）。
#[cfg(test)]
pub(crate) mod sql_shape;

#[cfg(test)]
mod tests {
    use super::*;

    /// 执行位门禁：设备批量读必须消费被上面回归钉住的模板常量，且本文件生产段内不得
    /// 出现第二条未被钉的 `descriptor_json` 读 SQL 字面量（内联副本会绕过形状回归）。
    #[test]
    fn device_snapshot_read_consumes_pinned_template_without_inline_copy() {
        let src = sql_shape::production_source(include_str!("sqlx_repo.rs"));
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

//! 授权仓储 SQLx 实现（管理写/读仓储与事务骨架）。
//!
//! 巨型 `impl AuthzRepository for SqlxAuthzRepository` 承载全部 21 个管理写/读方法，
//! 严格遵守事务骨架、UUID 锁序、LastAdmin 守卫、CAS revision、epoch CAS 与审计插入不变量。

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{
    GrantRecord, GrantScope, GrantSource, Permission, RoleRecord, check_admin_demotion_allowed,
};
use crate::db::DbPool;
use crate::error::ControllerError;

use super::codec::{
    admin_change_removes_admin_state, decode_grant, decode_user, grant_columns, read_error,
    validate_display_name, validate_group_name, validate_role_restore,
};
use super::model::{GroupRecord, RepoFuture, UserRecord};
use super::queries::{
    DEVICE_GROUPS_FOR_DEVICE_SQL, FIND_GRANT_SQL, FIND_GROUP_SQL, FIND_ROLE_SQL, FIND_USER_SQL,
    LIST_GRANTS_FOR_USER_SQL, LIST_GRANTS_SQL, LIST_GROUP_DEVICE_IDS_SQL, LIST_GROUPS_FIRST_SQL,
    LIST_GROUPS_SQL, LIST_ROLES_FIRST_SQL, LIST_ROLES_SQL, LIST_USERS_FIRST_SQL, LIST_USERS_SQL,
    ROLES_BY_IDS_SQL_TEMPLATE,
};
use super::snapshot_codec::{
    decode_group_memberships_for_device, validate_snapshot_grant_references,
};
use super::snapshot_read::{acquire_authorization_read_connection, verify_management_read};
use super::traits::AuthzRepository;
use super::tx_guard::{
    begin_guard, bump_authz_epoch, lock_user_pair_and_verify_actor, placeholders,
};

pub struct SqlxAuthzRepository {
    pub(crate) db: DbPool,
}

impl SqlxAuthzRepository {
    pub fn new(db: DbPool) -> Self {
        Self { db }
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

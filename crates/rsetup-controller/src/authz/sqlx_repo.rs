use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{
    GrantRecord, GrantScope, GrantSource, Permission, RoleRecord, SnapshotActor,
};
use crate::db::DbPool;
use crate::devices::repository::DeviceAdminView;
use crate::error::ControllerError;
use crate::model::{AdmissionState, ReviewDecision};

pub type RepoFuture<'a, T> =
    Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserRecord {
    pub id: [u8; 16],
    pub username: String,
    pub display_name: String,
    pub active: bool,
    pub is_admin: bool,
    pub must_change_password: bool,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupRecord {
    pub id: Uuid,
    pub name: String,
    pub archived: bool,
    pub revision: u64,
    pub member_count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantColumns {
    pub source_kind: &'static str,
    pub role_id: Option<Uuid>,
    pub permissions_json: Option<String>,
    pub scope_kind: &'static str,
    pub scope_group_id: Option<Uuid>,
    pub scope_device_id: Option<[u8; 32]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationSnapshot {
    pub actor: SnapshotActor,
    pub authz_epoch: u64,
    pub grants: Vec<GrantRecord>,
    pub roles: HashMap<Uuid, RoleRecord>,
    pub device_groups: HashMap<[u8; 32], HashSet<Uuid>>,
    pub devices: HashMap<[u8; 32], DeviceAdminView>,
}

pub trait AuthzRepository: Send + Sync {
    fn find_user<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 16],
    ) -> RepoFuture<'a, Option<UserRecord>>;
    fn list_users<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 16]>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<UserRecord>, Option<[u8; 16]>)>;
    fn find_role<'a>(&'a self, actor: &'a Session, id: Uuid) -> RepoFuture<'a, Option<RoleRecord>>;
    fn list_roles<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<RoleRecord>, Option<Uuid>)>;
    fn find_grant<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
    ) -> RepoFuture<'a, Option<GrantRecord>>;
    fn list_grants<'a>(
        &'a self,
        actor: &'a Session,
        user_id: Option<[u8; 16]>,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<GrantRecord>, Option<Uuid>)>;
    fn list_grants_for_user<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
    ) -> RepoFuture<'a, Vec<GrantRecord>>;
    fn roles_by_id<'a>(
        &'a self,
        actor: &'a Session,
        ids: Vec<Uuid>,
    ) -> RepoFuture<'a, HashMap<Uuid, RoleRecord>>;
    fn device_groups_for_device<'a>(
        &'a self,
        actor: &'a Session,
        device_id: [u8; 32],
    ) -> RepoFuture<'a, HashSet<Uuid>>;
    fn find_group<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
    ) -> RepoFuture<'a, Option<GroupRecord>>;
    fn list_groups<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> RepoFuture<'a, (Vec<GroupRecord>, Option<Uuid>)>;
    fn list_group_device_ids<'a>(
        &'a self,
        actor: &'a Session,
        group_id: Uuid,
    ) -> RepoFuture<'a, Vec<[u8; 32]>>;
}

pub trait AuthorizationSnapshotRepository: Send + Sync {
    fn load_snapshot<'a>(
        &'a self,
        actor_id: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot>;

    fn load_admin_target_snapshot<'a>(
        &'a self,
        caller: &'a Session,
        target_user: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot>;
}

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
    crate::integrity::validate_grant_fields(
        &source_kind,
        role_id.is_some(),
        permissions.as_ref(),
        &scope_kind,
        scope_group_id.is_some(),
        scope_device_id.is_some(),
    )?;
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

fn uuid16(value: Vec<u8>, code: &'static str) -> Result<Uuid, ControllerError> {
    let bytes: [u8; 16] = value.try_into().map_err(|_| read_error(code))?;
    Ok(Uuid::from_bytes(bytes))
}

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
            return Err(read_error("snapshot.role_permission"));
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
    validate_display_name(&display_name).map_err(|_| read_error("devices.display_name"))?;
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
            if !roles.contains_key(rid) {
                return Err(read_error("snapshot.role_ref"));
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

pub(crate) const ACTOR_READONLY_SQL: &str = "SELECT CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password FROM users WHERE id = ?";

pub(crate) const SNAPSHOT_META_SQL: &str = "SELECT CAST(singleton AS SIGNED) AS singleton, CAST(schema_version AS SIGNED) AS schema_version, authz_epoch FROM schema_meta LIMIT 2";

pub(crate) const LIST_ACTOR_GRANTS_SQL: &str = "SELECT g.id, g.user_id, g.source_kind, g.role_id, CAST(g.permissions AS CHAR) AS permissions, g.scope_kind, \
     g.scope_group_id, g.scope_device_id, g.revision, \
     CAST(u.id IS NOT NULL AS SIGNED) AS owner_exists, \
     CAST(r.id IS NOT NULL AS SIGNED) AS role_exists, \
     CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, \
     CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists \
     FROM grants g LEFT JOIN users u ON u.id = g.user_id \
     LEFT JOIN roles r ON r.id = g.role_id \
     LEFT JOIN device_groups dg ON dg.id = g.scope_group_id \
     LEFT JOIN devices d ON d.public_key = g.scope_device_id \
     WHERE g.user_id = ? ORDER BY g.id";

pub(crate) const ROLES_BY_IDS_SQL_TEMPLATE: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, \
     CAST(archived AS SIGNED) AS archived, revision \
     FROM roles WHERE id IN ({placeholders}) ORDER BY id";

pub(crate) const ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE: &str = "SELECT role_id, permission FROM role_permissions \
     WHERE role_id IN ({placeholders}) ORDER BY role_id, permission";

pub(crate) const GROUPS_FOR_DEVICES_SQL_TEMPLATE: &str = "SELECT gm.device_id, gm.group_id, CAST(dg.archived AS SIGNED) AS group_archived, \
     CAST(d.public_key IS NOT NULL AS SIGNED) AS member_device_exists \
     FROM group_members gm LEFT JOIN device_groups dg ON dg.id = gm.group_id \
     LEFT JOIN devices d ON d.public_key = gm.device_id \
     WHERE gm.device_id IN ({placeholders}) ORDER BY gm.device_id, gm.group_id";

pub(crate) const DEVICES_BY_IDS_SQL_TEMPLATE: &str = "SELECT public_key, display_name, descriptor_json, admission_state, \
     review_decision, revision, CAST(archived AS SIGNED) AS archived \
     FROM devices WHERE public_key IN ({placeholders}) ORDER BY public_key";

pub(crate) const LIST_USERS_SQL: &str = "SELECT id, username, display_name, CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password, revision FROM users WHERE id > ? ORDER BY id LIMIT ?";
pub(crate) const LIST_USERS_FIRST_SQL: &str = "SELECT id, username, display_name, CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password, revision FROM users ORDER BY id LIMIT ?";

pub(crate) const LIST_ROLES_SQL: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles WHERE id > ? ORDER BY id LIMIT ?";
pub(crate) const LIST_ROLES_FIRST_SQL: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles ORDER BY id LIMIT ?";

pub(crate) const LIST_GRANTS_SQL: &str = "SELECT id, user_id, source_kind, role_id, CAST(permissions AS CHAR) AS permissions, scope_kind, scope_group_id, scope_device_id, revision FROM grants WHERE (? IS NULL OR user_id = ?) AND (? IS NULL OR id > ?) ORDER BY id LIMIT ?";
pub(crate) const LIST_GRANTS_FOR_USER_SQL: &str = "SELECT id, user_id, source_kind, role_id, CAST(permissions AS CHAR) AS permissions, scope_kind, scope_group_id, scope_device_id, revision FROM grants WHERE user_id = ? ORDER BY id";

pub(crate) const LIST_GROUPS_SQL: &str = "SELECT g.id, g.name, CAST(g.archived AS SIGNED) AS archived, g.revision, COUNT(m.device_id) AS member_count FROM device_groups g LEFT JOIN group_members m ON m.group_id = g.id WHERE g.id > ? GROUP BY g.id ORDER BY g.id LIMIT ?";
pub(crate) const LIST_GROUPS_FIRST_SQL: &str = "SELECT g.id, g.name, CAST(g.archived AS SIGNED) AS archived, g.revision, COUNT(m.device_id) AS member_count FROM device_groups g LEFT JOIN group_members m ON m.group_id = g.id GROUP BY g.id ORDER BY g.id LIMIT ?";

pub(crate) const FIND_USER_SQL: &str = "SELECT id, username, display_name, CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password, revision FROM users WHERE id = ?";
pub(crate) const FIND_ROLE_SQL: &str = "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles WHERE id = ?";
pub(crate) const FIND_GRANT_SQL: &str = "SELECT id, user_id, source_kind, role_id, CAST(permissions AS CHAR) AS permissions, scope_kind, scope_group_id, scope_device_id, revision FROM grants WHERE id = ?";
pub(crate) const FIND_GROUP_SQL: &str = "SELECT g.id, g.name, CAST(g.archived AS SIGNED) AS archived, g.revision, COUNT(m.device_id) AS member_count FROM device_groups g LEFT JOIN group_members m ON m.group_id = g.id WHERE g.id = ? GROUP BY g.id";

pub(crate) const LIST_GROUP_DEVICE_IDS_SQL: &str = "SELECT m.device_id FROM group_members m JOIN devices d ON d.public_key = m.device_id JOIN device_groups g ON g.id = m.group_id WHERE m.group_id = ? AND d.archived = 0 AND g.archived = 0 ORDER BY m.device_id";

pub(crate) const DEVICE_GROUPS_FOR_DEVICE_SQL: &str = "SELECT gm.group_id, CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, CAST(dg.archived AS SIGNED) AS group_archived, CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists, CAST(d.archived AS SIGNED) AS device_archived FROM group_members gm LEFT JOIN device_groups dg ON dg.id = gm.group_id LEFT JOIN devices d ON d.public_key = gm.device_id WHERE gm.device_id = ? ORDER BY gm.group_id";

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
                    Some(decode_grant(
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
                    )?)
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
                items.push(decode_grant(
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
                items.push(decode_grant(
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
                )?);
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

            let mut out = HashSet::new();
            for r in rows {
                let dev_exists = crate::integrity::raw_bool(
                    r.try_get("device_exists")
                        .map_err(|_| read_error("groups.for_device"))?,
                    "groups.for_device",
                )?;
                let dev_archived = crate::integrity::raw_bool(
                    r.try_get("device_archived")
                        .map_err(|_| read_error("groups.for_device"))?,
                    "groups.for_device",
                )?;
                let grp_exists = crate::integrity::raw_bool(
                    r.try_get("group_exists")
                        .map_err(|_| read_error("groups.for_device"))?,
                    "groups.for_device",
                )?;
                let grp_archived = crate::integrity::raw_bool(
                    r.try_get("group_archived")
                        .map_err(|_| read_error("groups.for_device"))?,
                    "groups.for_device",
                )?;

                if !dev_exists || !grp_exists {
                    return Err(read_error("groups.for_device.dangling"));
                }
                if dev_archived || grp_archived {
                    continue;
                }
                let gid_raw: Vec<u8> = r
                    .try_get("group_id")
                    .map_err(|_| read_error("groups.for_device"))?;
                let gid =
                    Uuid::from_slice(&gid_raw).map_err(|_| read_error("groups.for_device"))?;
                out.insert(gid);
            }

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::policy::{GrantScope, GrantSource, Permission};
    use crate::devices::repository::{
        AUDIT_PAGE_SQL, DEVICE_PAGE_KEYS_FIRST_SQL, DEVICE_PAGE_KEYS_SQL,
    };

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
        assert_eq!(ok.active, true);
        assert_eq!(ok.is_admin, false);
        assert_eq!(ok.must_change_password, true);
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
    fn list_sql_binds_are_pinned_to_cursor_and_limit() {
        // 每页查询恰好两个绑定：(cursor, limit)。绑定数与列顺序漂移必须在离线就暴露。
        assert_eq!(LIST_USERS_SQL.matches('?').count(), 2);
        assert_eq!(LIST_ROLES_SQL.matches('?').count(), 2);
        assert_eq!(LIST_GRANTS_SQL.matches('?').count(), 5); // (? IS NULL OR user_id = ?) AND (? IS NULL OR id > ?) LIMIT ?
        assert_eq!(LIST_GROUPS_SQL.matches('?').count(), 2);
        assert_eq!(DEVICE_PAGE_KEYS_SQL.matches('?').count(), 2); // (cursor, limit+1)：候选公钥扫描，取代旧全列 DEVICE_PAGE_SQL
        assert_eq!(DEVICE_PAGE_KEYS_FIRST_SQL.matches('?').count(), 1);
        assert_eq!(AUDIT_PAGE_SQL.matches('?').count(), 11); // filter 4 + cursor 6 + limit 1
        // 内部快照层：固定 SQL 绑定数与模板占位符标记漂移必须离线暴露（占位符由同事务层按去重 id 数 1:1 展开，不拼进模板字面量）。
        assert_eq!(SNAPSHOT_META_SQL.matches('?').count(), 0);
        assert_eq!(LIST_ACTOR_GRANTS_SQL.matches('?').count(), 1); // (user_id)
        assert!(ROLES_BY_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(GROUPS_FOR_DEVICES_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(DEVICES_BY_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
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
        assert!(!SNAPSHOT_META_SQL.contains("WHERE"));
        assert!(SNAPSHOT_META_SQL.ends_with("LIMIT 2"));
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

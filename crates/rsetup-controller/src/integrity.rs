use crate::{ControllerError, db::DbPool};
use serde_json::Value;
use sqlx::Row;

fn scan_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("identity data {code}"))
}

const BOOLEAN_SCANS: &[(&str, &str)] = &[
    (
        "schema_meta.initialized",
        "SELECT CAST(initialized AS SIGNED) AS raw FROM schema_meta",
    ),
    (
        "users.active",
        "SELECT CAST(active AS SIGNED) AS raw FROM users",
    ),
    (
        "users.is_admin",
        "SELECT CAST(is_admin AS SIGNED) AS raw FROM users",
    ),
    (
        "users.must_change_password",
        "SELECT CAST(must_change_password AS SIGNED) AS raw FROM users",
    ),
    (
        "sessions.revoked",
        "SELECT CAST(revoked AS SIGNED) AS raw FROM sessions",
    ),
    (
        "roles.builtin",
        "SELECT CAST(builtin AS SIGNED) AS raw FROM roles",
    ),
    (
        "roles.archived",
        "SELECT CAST(archived AS SIGNED) AS raw FROM roles",
    ),
    (
        "device_groups.archived",
        "SELECT CAST(archived AS SIGNED) AS raw FROM device_groups",
    ),
    (
        "devices.archived",
        "SELECT CAST(archived AS SIGNED) AS raw FROM devices",
    ),
];
const ORPHAN_SCANS: &[(&str, &str)] = &[
    (
        "sessions",
        "SELECT 1 FROM sessions s LEFT JOIN users u ON u.id=s.user_id WHERE u.id IS NULL LIMIT 1",
    ),
    (
        "role_permissions",
        "SELECT 1 FROM role_permissions p LEFT JOIN roles r ON r.id=p.role_id WHERE r.id IS NULL LIMIT 1",
    ),
    (
        "group_members",
        "SELECT 1 FROM group_members m LEFT JOIN device_groups g ON g.id=m.group_id LEFT JOIN devices d ON d.public_key=m.device_id WHERE g.id IS NULL OR d.public_key IS NULL LIMIT 1",
    ),
    (
        "grants",
        "SELECT 1 FROM grants x LEFT JOIN users u ON u.id=x.user_id LEFT JOIN roles r ON r.id=x.role_id LEFT JOIN device_groups g ON g.id=x.scope_group_id LEFT JOIN devices d ON d.public_key=x.scope_device_id WHERE u.id IS NULL OR (x.role_id IS NOT NULL AND r.id IS NULL) OR (x.scope_group_id IS NOT NULL AND g.id IS NULL) OR (x.scope_device_id IS NOT NULL AND d.public_key IS NULL) LIMIT 1",
    ),
    (
        "admission_decisions",
        "SELECT 1 FROM admission_decisions a LEFT JOIN devices d ON d.public_key=a.device_id LEFT JOIN users u ON u.id=a.actor_id WHERE d.public_key IS NULL OR (a.actor_id IS NOT NULL AND u.id IS NULL) LIMIT 1",
    ),
    (
        "audit_events",
        "SELECT 1 FROM audit_events a LEFT JOIN users u ON u.id=a.actor_user_id WHERE a.actor_user_id IS NOT NULL AND u.id IS NULL LIMIT 1",
    ),
];

fn legacy_expected_version(version: i32) -> Result<i32, ControllerError> {
    if matches!(version, 1 | 2) {
        Ok(version)
    } else {
        Err(scan_error("meta.singleton"))
    }
}

fn validate_meta_row(
    singleton: i8,
    version: i32,
    expected_version: Option<i32>,
) -> Result<(), ControllerError> {
    if expected_version != Some(version) || singleton != 1 {
        Err(scan_error("meta.singleton"))
    } else {
        Ok(())
    }
}

fn validate_meta_count(count: usize, expected_version: Option<i32>) -> Result<(), ControllerError> {
    if count == usize::from(expected_version.is_some()) {
        Ok(())
    } else {
        Err(scan_error("meta.singleton"))
    }
}

type GuardRows = Result<Vec<Result<(i8, i32), sqlx::Error>>, sqlx::Error>;
type ActorColumns = (
    Result<i64, sqlx::Error>,
    Result<i64, sqlx::Error>,
    Result<i64, sqlx::Error>,
);

fn validate_guard_rows(rows: GuardRows) -> Result<(), ControllerError> {
    let rows = rows.map_err(|_| scan_error("meta.guard"))?;
    if rows.len() != 1 || !matches!(rows[0], Ok((1, 3))) {
        return Err(scan_error("meta.guard"));
    }
    Ok(())
}

pub(crate) async fn lock_integrity_guard(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
) -> Result<(), ControllerError> {
    let rows = sqlx::query(
        "SELECT singleton,schema_version FROM schema_meta ORDER BY singleton FOR UPDATE",
    )
    .fetch_all(&mut **tx)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| Ok((row.try_get("singleton")?, row.try_get("schema_version")?)))
            .collect()
    });
    validate_guard_rows(rows)
}

/// 版本化 guard 加锁：在同一事务内以 `FOR UPDATE` 读取 singleton 行，再交给
/// `validate_guard_rows_for_version` 校验。期望版本必须来自编译期配置
/// （`SUPPORTED_SCHEMA_VERSION`）并由调用方显式传入，**绝对禁止**先读数据库行
/// 的版本再拿它验证自己。
pub async fn lock_schema_guard_for_version(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    expected_version: i32,
) -> Result<(), ControllerError> {
    let rows = sqlx::query(
        "SELECT singleton,schema_version FROM schema_meta WHERE singleton = 1 FOR UPDATE",
    )
    .fetch_all(&mut **tx)
    .await
    .map(|rows| {
        rows.into_iter()
            .map(|row| Ok((row.try_get("singleton")?, row.try_get("schema_version")?)))
            .collect()
    });
    validate_guard_rows_for_version(rows, expected_version)
}

fn validate_actor_row(
    row: Result<Option<ActorColumns>, sqlx::Error>,
) -> Result<(), ControllerError> {
    let (active, is_admin, must_change_password) = row
        .map_err(|_| scan_error("actor.read"))?
        .ok_or(ControllerError::NotFound)?;
    let active = raw_bool(
        active.map_err(|_| scan_error("actor.decode"))?,
        "actor.boolean",
    )?;
    let is_admin = raw_bool(
        is_admin.map_err(|_| scan_error("actor.decode"))?,
        "actor.boolean",
    )?;
    let must_change_password = raw_bool(
        must_change_password.map_err(|_| scan_error("actor.decode"))?,
        "actor.boolean",
    )?;
    if !active || !is_admin || must_change_password {
        return Err(ControllerError::PermissionDenied);
    }
    Ok(())
}

pub(crate) async fn require_active_admin(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor: [u8; 16],
) -> Result<(), ControllerError> {
    let row = sqlx::query("SELECT CAST(active AS SIGNED) AS active,CAST(is_admin AS SIGNED) AS is_admin,CAST(must_change_password AS SIGNED) AS must_change_password FROM users WHERE id = ? FOR UPDATE")
        .bind(actor.as_slice())
        .fetch_optional(&mut **tx)
        .await
        .map(|row| {
            row.map(|row| {
                (
                    row.try_get("active"),
                    row.try_get("is_admin"),
                    row.try_get("must_change_password"),
                )
            })
        });
    validate_actor_row(row)
}

pub async fn check_identity_data(db: &DbPool) -> Result<(), ControllerError> {
    validate_identity_rows(db, true).await
}

/// v4 运行时数据只读扫描：任一违规返回 `Err(Config)`，登录/启动路径据此
/// fail-closed。全部 SQL 为函数内固定字面量，绝不从外部输入插值表名或列名；
/// 不加 `FOR UPDATE`、不写库。必须在版本 guard 通过后调用（此时
/// `version == SUPPORTED_SCHEMA_VERSION` 已被确认），更高版本由 guard 层拒绝。
pub async fn check_runtime_data(db: &DbPool, version: i32) -> Result<(), ControllerError> {
    // v1..=3 未建五张 runtime 表，直接放行。
    if version < SUPPORTED_SCHEMA_VERSION {
        return Ok(());
    }
    // 更高未知版本（>=5）已在上游 `validate_schema_version` 被拒绝，不会到达此处；
    // 此处保持 `Ok` 以维持本函数“仅 v4 做数据扫描”的契约。
    if version != SUPPORTED_SCHEMA_VERSION {
        return Ok(());
    }

    // (1) `sub_tasks.main_task_id` 无孤儿主任务
    const ORPHAN_SUB_TASKS: &str = "SELECT st.id FROM sub_tasks st \
        LEFT JOIN main_tasks mt ON st.main_task_id = mt.id \
        WHERE mt.id IS NULL LIMIT 1";
    if sqlx::query(ORPHAN_SUB_TASKS)
        .fetch_optional(&db.0)
        .await
        .map_err(|_| scan_error("runtime.sub_tasks_orphan_main_task_id"))?
        .is_some()
    {
        return Err(ControllerError::Config(
            "runtime data sub_tasks orphan main_task_id".into(),
        ));
    }

    // (2) `sub_tasks.state` 必须属于 10 个已知合法值（对齐 `tasks::model::SubTaskState`）
    const BAD_SUB_TASK_STATE: &str = "SELECT id FROM sub_tasks WHERE state NOT IN \
        ('queued', 'held', 'dispatching', 'accepted', 'verifying', \
         'succeeded', 'failed', 'unknown', 'cancelled', 'expired') LIMIT 1";
    if sqlx::query(BAD_SUB_TASK_STATE)
        .fetch_optional(&db.0)
        .await
        .map_err(|_| scan_error("runtime.sub_tasks_invalid_state"))?
        .is_some()
    {
        return Err(ControllerError::Config(
            "runtime data sub_tasks invalid state".into(),
        ));
    }

    // (3) `device_operation_locks.owner_sub_task_id` 必须指向存在的子任务
    const ORPHAN_DEVICE_LOCKS: &str = "SELECT l.device_id FROM device_operation_locks l \
        LEFT JOIN sub_tasks st ON l.owner_sub_task_id = st.id \
        WHERE st.id IS NULL LIMIT 1";
    if sqlx::query(ORPHAN_DEVICE_LOCKS)
        .fetch_optional(&db.0)
        .await
        .map_err(|_| scan_error("runtime.device_operation_locks_orphan_owner"))?
        .is_some()
    {
        return Err(ControllerError::Config(
            "runtime data device_operation_locks orphan owner_sub_task_id".into(),
        ));
    }

    // (4) `main_tasks.outcome` 在非 `completed` 状态时必须为 NULL
    const OUTCOME_NOT_NULL: &str = "SELECT id FROM main_tasks \
        WHERE state != 'completed' AND outcome IS NOT NULL LIMIT 1";
    if sqlx::query(OUTCOME_NOT_NULL)
        .fetch_optional(&db.0)
        .await
        .map_err(|_| scan_error("runtime.main_tasks_outcome_not_null"))?
        .is_some()
    {
        return Err(ControllerError::Config(
            "runtime data main_tasks outcome not null for non-completed state".into(),
        ));
    }

    // (5) `counts_json` 必须解析为合法 JSON 对象（固定语句 + 应用层扫描，精确区分“解析失败/非 object”）
    const COUNTS_JSON: &str = "SELECT id, counts_json FROM main_tasks";
    let rows = sqlx::query(COUNTS_JSON)
        .fetch_all(&db.0)
        .await
        .map_err(|_| scan_error("runtime.main_tasks_counts_json"))?;
    for row in &rows {
        let raw: String = row
            .try_get(1)
            .map_err(|_| scan_error("runtime.main_tasks_counts_json"))?;
        let value: Value = serde_json::from_str(&raw).map_err(|_| {
            ControllerError::Config("runtime data main_tasks invalid counts_json".into())
        })?;
        if !value.is_object() {
            return Err(ControllerError::Config(
                "runtime data main_tasks invalid counts_json".into(),
            ));
        }
    }

    Ok(())
}

pub(crate) async fn validate_identity_rows(
    db: &DbPool,
    require_meta: bool,
) -> Result<(), ControllerError> {
    scan_identity_rows(db, require_meta.then_some(3)).await
}

// Only the explicit legacy preflight may call this; normal v3 startup remains
// pinned to version 3 and fresh migration remains pinned to an empty meta table.
#[allow(dead_code)] // consumed by the following legacy preflight task
pub(crate) async fn validate_identity_rows_for_version(
    db: &DbPool,
    expected_version: i32,
) -> Result<(), ControllerError> {
    scan_identity_rows(db, Some(legacy_expected_version(expected_version)?)).await
}

async fn scan_identity_rows(
    db: &DbPool,
    expected_version: Option<i32>,
) -> Result<(), ControllerError> {
    // A single scoped transaction keeps all SELECTs on one connection. A
    // REPEATABLE READ session should offer a stable row snapshot; verify the
    // actual target engine and session isolation before relying on that property.
    let mut tx =
        db.0.begin()
            .await
            .map_err(|_| scan_error("transaction.begin"))?;
    // A stream borrows tx: exhaust/drop it before starting the next SELECT.
    // BoxStream exposes poll_next on its pinned trait object without a new dependency.
    macro_rules! scan {
        ($sql:expr, $read:expr, |$row:ident| $body:block) => {{
            let mut stream = sqlx::query($sql).fetch(&mut *tx);
            while let Some(item) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
                let $row = item.map_err(|_| scan_error($read))?;
                $body
            }
        }};
    }
    let mut meta_count = 0;
    scan!(
        "SELECT singleton,schema_version FROM schema_meta LIMIT 2",
        "meta.read",
        |row| {
            let singleton: i8 = row
                .try_get("singleton")
                .map_err(|_| scan_error("meta.decode"))?;
            let version: i32 = row
                .try_get("schema_version")
                .map_err(|_| scan_error("meta.decode"))?;
            meta_count += 1;
            if meta_count > 1 {
                return Err(scan_error("meta.singleton"));
            }
            validate_meta_row(singleton, version, expected_version)?;
        }
    );
    validate_meta_count(meta_count, expected_version)?;
    // The caller's strict shape check establishes a full UNIQUE(username) index;
    // valid lowercase ASCII usernames cannot have identical raw bytes under it.
    // Do not remove the separate v1/v2 migration collision preflight in db.rs.
    scan!(
        "SELECT CAST(username AS BINARY) AS username FROM users",
        "users.read",
        |row| {
            validate_username_bytes(row.try_get::<&[u8], _>("username"))?;
        }
    );
    for &(code, sql) in BOOLEAN_SCANS {
        scan!(sql, "boolean.read", |row| {
            let raw: i64 = row
                .try_get("raw")
                .map_err(|_| scan_error("boolean.decode"))?;
            raw_bool(raw, code)?;
        });
    }
    scan!(
        "SELECT admission_state,review_decision FROM devices",
        "devices.read",
        |row| {
            let state: &str = row
                .try_get("admission_state")
                .map_err(|_| scan_error("devices.decode"))?;
            let decision: &str = row
                .try_get("review_decision")
                .map_err(|_| scan_error("devices.decode"))?;
            validate_device_fields(state, decision).map_err(|_| scan_error("devices.state"))?;
        }
    );
    scan!(
        "SELECT permission FROM role_permissions",
        "role_permissions.read",
        |row| {
            let permission: &str = row
                .try_get("permission")
                .map_err(|_| scan_error("role_permissions.decode"))?;
            validate_permission_row(permission)?;
        }
    );
    // Only four short permission literals are allowed, so 4096 bytes is a
    // deliberately generous fixed bound for one JSON array. Check on the server
    // BEFORE fetching any JSON; SQL/guard failure is fail-closed, no truncation.
    if sqlx::query(
        "SELECT 1 FROM grants WHERE OCTET_LENGTH(CAST(permissions AS CHAR)) > 4096 LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| scan_error("grants.length_read"))?
    .is_some()
    {
        return Err(scan_error("grants.json_length"));
    }
    scan!(
        "SELECT source_kind,CAST(role_id IS NOT NULL AS SIGNED) AS has_role,CAST(permissions AS CHAR) AS permissions_json,scope_kind,CAST(scope_group_id IS NOT NULL AS SIGNED) AS has_group,CAST(scope_device_id IS NOT NULL AS SIGNED) AS has_device FROM grants",
        "grants.read",
        |row| {
            let source: &str = row
                .try_get("source_kind")
                .map_err(|_| scan_error("grants.decode"))?;
            let has_role: i64 = row
                .try_get("has_role")
                .map_err(|_| scan_error("grants.decode"))?;
            let permissions: Option<&str> = row
                .try_get("permissions_json")
                .map_err(|_| scan_error("grants.decode"))?;
            let scope: &str = row
                .try_get("scope_kind")
                .map_err(|_| scan_error("grants.decode"))?;
            let has_group: i64 = row
                .try_get("has_group")
                .map_err(|_| scan_error("grants.decode"))?;
            let has_device: i64 = row
                .try_get("has_device")
                .map_err(|_| scan_error("grants.decode"))?;
            validate_grant_row(source, has_role, permissions, scope, has_group, has_device)?;
        }
    );
    for &(code, sql) in ORPHAN_SCANS {
        if sqlx::query(sql)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| scan_error("reference.read"))?
            .is_some()
        {
            return Err(scan_error(code));
        }
    }
    // Dropping a read-only transaction rolls it back; explicit rollback exposes
    // transaction cleanup errors instead of silently accepting the scan.
    tx.rollback()
        .await
        .map_err(|_| scan_error("transaction.end"))?;
    Ok(())
}

fn validate_username_bytes(username: Result<&[u8], sqlx::Error>) -> Result<(), ControllerError> {
    let bytes = username.map_err(|_| scan_error("users.decode"))?;
    let username = std::str::from_utf8(bytes).map_err(|_| scan_error("users.decode"))?;
    validate_username_row(username)
}

fn validate_username_row(username: &str) -> Result<(), ControllerError> {
    if crate::db::valid_username(username) {
        Ok(())
    } else {
        Err(scan_error("users.username"))
    }
}

fn validate_permission_row(permission: &str) -> Result<(), ControllerError> {
    if valid_permission(permission) {
        Ok(())
    } else {
        Err(scan_error("role_permissions.permission"))
    }
}

fn validate_grant_row(
    source: &str,
    has_role: i64,
    permissions_json: Option<&str>,
    scope: &str,
    has_group: i64,
    has_device: i64,
) -> Result<(), ControllerError> {
    let has_role = raw_bool(has_role, "grants.role_presence")?;
    let has_group = raw_bool(has_group, "grants.group_presence")?;
    let has_device = raw_bool(has_device, "grants.device_presence")?;
    if permissions_json.is_some_and(|text| text.len() > 4096) {
        return Err(scan_error("grants.json_length"));
    }
    let permissions = permissions_json
        .map(|text| serde_json::from_str::<Value>(text).map_err(|_| scan_error("grants.json")))
        .transpose()?;
    validate_grant_fields(
        source,
        has_role,
        permissions.as_ref(),
        scope,
        has_group,
        has_device,
    )
    .map_err(|_| scan_error("grants.fields"))
}

pub fn validate_singleton_ids(ids: &[i8]) -> Result<(), ControllerError> {
    if ids == [1] {
        Ok(())
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

pub fn validate_device_fields(state: &str, decision: &str) -> Result<(), ControllerError> {
    if matches!(
        (state, decision),
        ("PENDING", "none" | "denied") | ("APPROVED", "approved") | ("REVOKED", "revoked")
    ) {
        Ok(())
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

pub fn validate_grant_fields(
    source: &str,
    has_role: bool,
    permissions: Option<&Value>,
    scope: &str,
    has_group: bool,
    has_device: bool,
) -> Result<(), ControllerError> {
    let valid_source = match source {
        "role" => has_role && permissions.is_none(),
        "direct" => {
            !has_role
                && permissions.and_then(Value::as_array).is_some_and(|values| {
                    let mut seen = std::collections::HashSet::new();
                    !values.is_empty()
                        && values.iter().all(|value| {
                            value.as_str().is_some_and(|permission| {
                                matches!(
                                    permission,
                                    "device.read"
                                        | "device.status.read"
                                        | "device.reboot"
                                        | "device.task.read"
                                ) && seen.insert(permission)
                            })
                        })
                })
        }
        _ => false,
    };
    let valid_scope = match scope {
        "all" => !has_group && !has_device,
        "group" => has_group && !has_device,
        "device" => !has_group && has_device,
        _ => false,
    };
    if valid_source && valid_scope {
        Ok(())
    } else {
        Err(ControllerError::InvalidArgument)
    }
}

#[cfg(test)]
struct IdentityRows {
    meta: Vec<(i8, i32)>,
    usernames: Vec<String>,
    booleans: Vec<(&'static str, Vec<i64>)>,
    devices: Vec<(String, String)>,
    permissions: Vec<String>,
    grants: Vec<GrantRow>,
    orphans: Vec<(&'static str, bool)>,
}
#[cfg(test)]
struct GrantRow {
    source: String,
    has_role: i64,
    permissions_json: Option<String>,
    scope: String,
    has_group: i64,
    has_device: i64,
}
#[cfg(test)]
fn validate_identity_fixture(
    rows: &IdentityRows,
    require_meta: bool,
) -> Result<(), ControllerError> {
    validate_identity_fixture_with_meta(rows, require_meta.then_some(3))
}
#[cfg(test)]
fn validate_identity_fixture_with_meta(
    rows: &IdentityRows,
    expected_version: Option<i32>,
) -> Result<(), ControllerError> {
    for &(singleton, version) in &rows.meta {
        validate_meta_row(singleton, version, expected_version)?;
    }
    validate_meta_count(rows.meta.len(), expected_version)?;
    let mut seen = std::collections::HashSet::new();
    for username in &rows.usernames {
        validate_username_row(username)?;
        if !seen.insert(username.as_bytes()) {
            return Err(scan_error("users.username"));
        }
    }
    for &(code, ref values) in &rows.booleans {
        for &value in values {
            raw_bool(value, code)?;
        }
    }
    for (state, decision) in &rows.devices {
        validate_device_fields(state, decision).map_err(|_| scan_error("devices.state"))?;
    }
    for permission in &rows.permissions {
        validate_permission_row(permission)?;
    }
    for row in &rows.grants {
        validate_grant_row(
            &row.source,
            row.has_role,
            row.permissions_json.as_deref(),
            &row.scope,
            row.has_group,
            row.has_device,
        )?;
    }
    for &(code, missing) in &rows.orphans {
        if missing {
            return Err(scan_error(code));
        }
    }
    Ok(())
}
pub const SUPPORTED_SCHEMA_VERSION: i32 = 4;
pub const MIN_SUPPORTED_SCHEMA_VERSION: i32 = 1;

/// 编译期可识别的 schema 版本区间：`MIN_SUPPORTED_SCHEMA_VERSION..=SUPPORTED_SCHEMA_VERSION`。
/// 区间外（0、负数、`SUPPORTED_SCHEMA_VERSION + 1` 及以上）一律不识别，fail-closed。
pub fn is_recognized_schema_version(version: i32) -> bool {
    (MIN_SUPPORTED_SCHEMA_VERSION..=SUPPORTED_SCHEMA_VERSION).contains(&version)
}

/// 版本化 Guard 纯函数：仅接受编译期配置 `SUPPORTED_SCHEMA_VERSION`。
/// `None`（缺失 singleton 行）、旧版 v1..=3、未知 v5+、0 与负数一律
/// `Err(SchemaNotReady)`；期望版本绝不从数据库行本身推断。
pub fn validate_schema_version(found: Option<i32>) -> Result<(), ControllerError> {
    if found == Some(SUPPORTED_SCHEMA_VERSION) {
        Ok(())
    } else {
        Err(ControllerError::SchemaNotReady {
            found,
            required: SUPPORTED_SCHEMA_VERSION,
        })
    }
}

/// 普通启动决策：与 `validate_schema_version` 同一 fail-closed 契约。
/// 旧版（v1..=3）仅在显式迁移前置检查（preflight）路径中按版本单独校验，
/// 普通启动不接受任何非编译期配置版本。
pub fn startup_guard_decision(db_version: Option<i32>) -> Result<(), ControllerError> {
    validate_schema_version(db_version)
}

/// 版本化 guard 行校验：恰好一行 `singleton = 1` 且版本与**期望版本**一致。
/// 期望版本由调用方从编译期配置传入（`SUPPORTED_SCHEMA_VERSION`），
/// 版本不匹配即拒绝，覆盖“库结构仍是 v3 但声称 v4”这类脏数据。
pub fn validate_guard_rows_for_version(
    rows: GuardRows,
    expected_version: i32,
) -> Result<(), ControllerError> {
    let rows = rows.map_err(|_| scan_error("meta.guard"))?;
    if rows.len() != 1 {
        return Err(scan_error("meta.guard"));
    }
    match rows[0] {
        Ok((singleton, version)) if singleton == 1 && version == expected_version => Ok(()),
        Ok(_) | Err(_) => Err(scan_error("meta.guard")),
    }
}

/// 运行时五表存在性校验：v4 必须五表齐备；v1..=3 必须五表皆无（历史库仍可
/// 只读通过）；未识别版本直接拒绝。存在性快照来自固定字面量的
/// information_schema 探测，本函数不接触数据库。
pub fn validate_runtime_tables_for_version(
    version: i32,
    present: &crate::db::RuntimeTablePresence,
) -> Result<(), ControllerError> {
    if !is_recognized_schema_version(version) {
        return Err(ControllerError::SchemaNotReady {
            found: Some(version),
            required: SUPPORTED_SCHEMA_VERSION,
        });
    }
    let tables = [
        ("main_tasks", present.main_tasks),
        ("sub_tasks", present.sub_tasks),
        ("task_previews", present.task_previews),
        ("device_operation_locks", present.device_operation_locks),
        ("recovery_checks", present.recovery_checks),
    ];
    if version == SUPPORTED_SCHEMA_VERSION {
        if let Some((name, _)) = tables.iter().find(|(_, exists)| !exists) {
            return Err(ControllerError::Config(format!(
                "runtime schema table missing: {name}"
            )));
        }
        return Ok(());
    }
    if let Some((name, _)) = tables.iter().find(|(_, exists)| *exists) {
        return Err(ControllerError::Config(format!(
            "runtime schema table {name} must not exist before v{}",
            SUPPORTED_SCHEMA_VERSION
        )));
    }
    Ok(())
}

pub(crate) fn raw_bool(raw: i64, code: &'static str) -> Result<bool, ControllerError> {
    match raw {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(scan_error(code)),
    }
}
fn valid_permission(permission: &str) -> bool {
    matches!(
        permission,
        "device.read" | "device.status.read" | "device.reboot" | "device.task.read"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn guard_requires_exactly_one_v3_singleton_and_fails_closed_on_decode_or_read() {
        assert!(validate_guard_rows(Ok(vec![Ok((1, 3))])).is_ok());
        for rows in [
            Ok(vec![]),
            Ok(vec![Ok((1, 3)), Ok((2, 3))]),
            Ok(vec![Ok((0, 3))]),
            Ok(vec![Ok((2, 3))]),
            Ok(vec![Ok((1, 2))]),
            Ok(vec![Ok((1, 4))]),
            Ok(vec![Err(sqlx::Error::RowNotFound)]),
            Err(sqlx::Error::RowNotFound),
        ] {
            assert_eq!(
                validate_guard_rows(rows).unwrap_err().to_string(),
                "configuration: identity data meta.guard"
            );
        }
    }

    fn guard_rows(pairs: &[(i8, i32)]) -> GuardRows {
        Ok(pairs.iter().map(|pair| Ok(*pair)).collect())
    }

    #[test]
    fn guard_accepts_supported_versions_and_rejects_unknown() {
        assert!(is_recognized_schema_version(MIN_SUPPORTED_SCHEMA_VERSION)); // 1
        assert!(is_recognized_schema_version(2));
        assert!(is_recognized_schema_version(3));
        assert!(is_recognized_schema_version(SUPPORTED_SCHEMA_VERSION)); // 4
        assert!(!is_recognized_schema_version(0));
        assert!(!is_recognized_schema_version(5));
        assert!(!is_recognized_schema_version(999));
        assert!(!is_recognized_schema_version(-1));
    }

    #[test]
    fn startup_guard_rejects_old_unknown_and_dirty_versions() {
        // 普通启动期望版本来自编译期配置 = 4，而不是从数据库行本身推断期望。
        assert!(startup_guard_decision(Some(SUPPORTED_SCHEMA_VERSION)).is_ok());
        // 旧版本（v1..=3）仅能用于显式迁移 preflight，普通启动坚决拒绝。
        assert!(startup_guard_decision(Some(3)).is_err());
        assert!(startup_guard_decision(Some(2)).is_err());
        assert!(startup_guard_decision(Some(1)).is_err());
        // 未知或非法版本/缺失行。
        assert!(startup_guard_decision(Some(5)).is_err());
        assert!(startup_guard_decision(Some(0)).is_err());
        assert!(startup_guard_decision(None).is_err());
        // 拒绝路径复用 SchemaNotReady，不新增第二套错误变体。
        for found in [None, Some(3), Some(5)] {
            assert!(
                matches!(
                    startup_guard_decision(found),
                    Err(ControllerError::SchemaNotReady { found: f, required: 4 }) if f == found
                ),
                "expected SchemaNotReady {{ found: {found:?}, required: 4 }}"
            );
        }
    }

    #[test]
    fn guard_row_validation_is_version_scoped_and_fail_closed() {
        // 恰好一行 singleton=1 且版本与期望一致。
        assert!(validate_guard_rows_for_version(guard_rows(&[(1, 3)]), 3).is_ok());
        assert!(validate_guard_rows_for_version(guard_rows(&[(1, 4)]), 4).is_ok());
        // singleton 不是 1。
        assert!(validate_guard_rows_for_version(guard_rows(&[(0, 3)]), 3).is_err());
        // 多行 / 空行。
        assert!(validate_guard_rows_for_version(guard_rows(&[(1, 3), (1, 3)]), 3).is_err());
        assert!(validate_guard_rows_for_version(guard_rows(&[]), 3).is_err());
        // 版本与期望不一致（例如 v3 结构残留但库声称 4）。
        assert!(validate_guard_rows_for_version(guard_rows(&[(1, 3)]), 4).is_err());
        // 行本身是失败行。
        assert!(
            validate_guard_rows_for_version(Err(sqlx::Error::RowNotFound), 4).is_err(),
            "scan error must fail closed"
        );
    }

    #[test]
    fn runtime_tables_are_required_exactly_from_v4() {
        let full = crate::db::RuntimeTablePresence {
            main_tasks: true,
            sub_tasks: true,
            task_previews: true,
            device_operation_locks: true,
            recovery_checks: true,
        };
        let empty = crate::db::RuntimeTablePresence {
            main_tasks: false,
            sub_tasks: false,
            task_previews: false,
            device_operation_locks: false,
            recovery_checks: false,
        };
        assert!(validate_runtime_tables_for_version(4, &full).is_ok());
        assert!(validate_runtime_tables_for_version(4, &empty).is_err());
        // v1..=v3 不得要求 runtime 表存在（历史库仍可只读通过）。
        assert!(validate_runtime_tables_for_version(3, &empty).is_ok());
        assert!(validate_runtime_tables_for_version(3, &full).is_err());
        // 未识别版本一律拒绝。
        assert!(validate_runtime_tables_for_version(5, &full).is_err());
        assert!(validate_runtime_tables_for_version(0, &full).is_err());
    }

    #[test]
    fn schema_version_guard_accepts_v4_and_rejects_others() {
        // 正例：编译期配置版本 v4 通过。
        assert!(validate_schema_version(Some(SUPPORTED_SCHEMA_VERSION)).is_ok());
        // 负例表：缺失行 None、旧版 v1..=3、未知 v5+、0 与负数一律 fail-closed。
        // `ControllerError` 未派生 `PartialEq`，统一用 `matches!` 断言变体与字段值。
        let rejected: [Option<i32>; 7] =
            [None, Some(3), Some(2), Some(1), Some(5), Some(0), Some(-1)];
        for found in rejected {
            assert!(
                matches!(
                    validate_schema_version(found),
                    Err(ControllerError::SchemaNotReady { found: f, required: 4 }) if f == found
                ),
                "expected SchemaNotReady {{ found: {found:?}, required: 4 }}"
            );
        }
    }

    #[test]
    fn actor_requires_existing_active_admin_with_changed_password() {
        assert_eq!(
            validate_actor_row(Ok(None)).unwrap_err().to_string(),
            "not found"
        );
        assert!(validate_actor_row(Ok(Some((Ok(1), Ok(1), Ok(0))))).is_ok());
        for columns in [(0, 1, 0), (1, 0, 0), (1, 1, 1)] {
            assert_eq!(
                validate_actor_row(Ok(Some((Ok(columns.0), Ok(columns.1), Ok(columns.2)))))
                    .unwrap_err()
                    .to_string(),
                "permission denied"
            );
        }
    }

    #[test]
    fn actor_invalid_boolean_decode_and_read_fail_closed_without_raw_values() {
        for columns in [(2, 1, 0), (1, -1, 0), (1, 1, 2)] {
            assert_eq!(
                validate_actor_row(Ok(Some((Ok(columns.0), Ok(columns.1), Ok(columns.2)))))
                    .unwrap_err()
                    .to_string(),
                "configuration: identity data actor.boolean"
            );
        }
        assert_eq!(
            validate_actor_row(Ok(Some((Err(sqlx::Error::RowNotFound), Ok(1), Ok(0)))))
                .unwrap_err()
                .to_string(),
            "configuration: identity data actor.decode"
        );
        assert_eq!(
            validate_actor_row(Err(sqlx::Error::RowNotFound))
                .unwrap_err()
                .to_string(),
            "configuration: identity data actor.read"
        );
    }

    #[test]
    fn application_shapes_reject_invalid_combinations() {
        assert!(validate_singleton_ids(&[1]).is_ok());
        for ids in [&[][..], &[2][..], &[1, 2][..]] {
            assert!(validate_singleton_ids(ids).is_err(), "singleton: {ids:?}");
        }
        for (state, decision) in [
            ("PENDING", "none"),
            ("PENDING", "denied"),
            ("APPROVED", "approved"),
            ("REVOKED", "revoked"),
        ] {
            assert!(validate_device_fields(state, decision).is_ok());
        }
        assert!(validate_device_fields("DENIED", "denied").is_err());
        assert!(validate_device_fields("APPROVED", "none").is_err());
        let p = json!(["device.read"]);
        assert!(validate_grant_fields("role", true, None, "all", false, false).is_ok());
        assert!(validate_grant_fields("direct", false, Some(&p), "device", false, true).is_ok());
        assert!(validate_grant_fields("direct", true, Some(&p), "all", false, false).is_err());
        assert!(validate_grant_fields("role", true, None, "all", true, false).is_err());
        for bad in [
            json!(null),
            json!([]),
            json!({}),
            json!([1]),
            json!(["unknown"]),
            json!(["device.read", "device.read"]),
        ] {
            assert!(
                validate_grant_fields("direct", false, Some(&bad), "all", false, false).is_err()
            );
        }
    }

    #[test]
    fn device_accepts_only_four_exact_pairs() {
        for state in ["PENDING", "APPROVED", "REVOKED", "DENIED", "", "pending"] {
            for decision in ["none", "approved", "denied", "revoked", "", "APPROVED"] {
                let valid = matches!(
                    (state, decision),
                    ("PENDING", "none" | "denied")
                        | ("APPROVED", "approved")
                        | ("REVOKED", "revoked")
                );
                assert_eq!(
                    validate_device_fields(state, decision).is_ok(),
                    valid,
                    "{state}/{decision}"
                );
            }
        }
    }

    #[test]
    fn grant_source_sql_null_and_permission_directory() {
        let all = json!([
            "device.read",
            "device.status.read",
            "device.reboot",
            "device.task.read"
        ]);
        assert!(validate_grant_fields("direct", false, Some(&all), "all", false, false).is_ok());
        let valid = json!(["device.reboot"]);
        for (source, role, permissions, expected) in [
            ("role", true, None, true),
            ("role", false, None, false),
            ("role", true, Some(&valid), false),
            ("direct", false, Some(&valid), true),
            ("direct", true, Some(&valid), false),
            ("direct", false, None, false),
            ("", false, Some(&valid), false),
            ("ROLE", true, None, false),
            ("other", true, None, false),
        ] {
            assert_eq!(
                validate_grant_fields(source, role, permissions, "all", false, false).is_ok(),
                expected,
                "{source}/{role}/{permissions:?}"
            );
        }
        for invalid in [
            json!(null),
            json!([]),
            json!({}),
            json!("device.read"),
            json!([null]),
            json!([1]),
            json!([true]),
            json!(["unknown"]),
            json!(["Device.read"]),
            json!(["device.read", "device.read"]),
        ] {
            assert!(
                validate_grant_fields("direct", false, Some(&invalid), "all", false, false)
                    .is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn grant_scope_requires_exactly_matching_id_presence() {
        let permissions = json!(["device.task.read"]);
        for scope in ["all", "group", "device", "", "ALL", "unknown"] {
            for has_group in [false, true] {
                for has_device in [false, true] {
                    let valid = matches!(
                        (scope, has_group, has_device),
                        ("all", false, false) | ("group", true, false) | ("device", false, true)
                    );
                    assert_eq!(
                        validate_grant_fields(
                            "direct",
                            false,
                            Some(&permissions),
                            scope,
                            has_group,
                            has_device
                        )
                        .is_ok(),
                        valid,
                        "{scope}/{has_group}/{has_device}"
                    );
                }
            }
        }
    }

    #[test]
    fn grant_json_over_safe_bound_fails_without_echoing_contents() {
        // A valid but oversized direct grant must be rejected before JSON parsing.
        let json = format!("[\"device.read\"{}]", " ".repeat(4096));
        let rows = IdentityRows {
            meta: vec![(1, 3)],
            usernames: vec![],
            booleans: vec![],
            devices: vec![],
            permissions: vec![],
            grants: vec![GrantRow {
                source: "direct".into(),
                has_role: 0,
                permissions_json: Some(json.clone()),
                scope: "all".into(),
                has_group: 0,
                has_device: 0,
            }],
            orphans: vec![],
        };
        let error = validate_identity_fixture(&rows, true).unwrap_err();
        assert_eq!(
            error.to_string(),
            "configuration: identity data grants.json_length"
        );
        assert!(!error.to_string().contains(&json));
    }

    #[test]
    fn username_bytes_accept_original_rule_boundaries() {
        for username in ["abc", "0._-", &"a".repeat(64)] {
            assert!(validate_username_bytes(Ok(username.as_bytes())).is_ok());
        }
    }

    #[test]
    fn username_bytes_reject_invalid_names_without_normalization() {
        for username in [
            "ab",
            &"a".repeat(65),
            ".ab",
            "_ab",
            "-ab",
            "Alice",
            " abc",
            "abc ",
            "a b",
            "ab\t",
            "ab\n",
            "ab\0",
            "éab",
        ] {
            assert_eq!(
                validate_username_bytes(Ok(username.as_bytes()))
                    .unwrap_err()
                    .to_string(),
                "configuration: identity data users.username"
            );
        }
    }

    #[test]
    fn username_bytes_reject_invalid_utf8_as_fixed_decode_error() {
        for username in [&b"ab\xff"[..], &b"ab\xe2\x82"[..], &b"ab\xc0\xaf"[..]] {
            assert_eq!(
                validate_username_bytes(Ok(username))
                    .unwrap_err()
                    .to_string(),
                "configuration: identity data users.decode"
            );
        }
    }

    #[test]
    fn username_bytes_redact_sqlx_errors_including_null() {
        for error in [
            sqlx::Error::ColumnDecode {
                index: "synthetic-sensitive-column".into(),
                source: Box::new(sqlx::error::UnexpectedNullError),
            },
            sqlx::Error::ColumnDecode {
                index: "synthetic-sensitive-column".into(),
                source: Box::new(std::io::Error::other("synthetic-sensitive-value")),
            },
            sqlx::Error::ColumnNotFound("synthetic-sensitive-column".into()),
            sqlx::Error::Decode(Box::new(std::io::Error::other("synthetic-sensitive-value"))),
        ] {
            assert_eq!(
                validate_username_bytes(Err(error)).unwrap_err().to_string(),
                "configuration: identity data users.decode"
            );
        }
    }

    #[test]
    fn row_validators_consume_lazy_high_count_and_fail_on_first_bad_row() {
        let mut visited = 0;
        (0..100_000)
            .try_for_each(|i| {
                visited += 1;
                let username = format!("user{i:06}");
                validate_username_bytes(Ok(username.as_bytes()))
            })
            .unwrap();
        assert_eq!(visited, 100_000);

        let mut visited = 0;
        let error = (0..100_000)
            .try_for_each(|i| {
                visited += 1;
                let username = if i == 10 {
                    "BadUser".to_owned()
                } else {
                    format!("user{i:06}")
                };
                validate_username_bytes(Ok(username.as_bytes()))
            })
            .unwrap_err();
        assert_eq!(visited, 11);
        assert_eq!(
            error.to_string(),
            "configuration: identity data users.username"
        );
        assert!(validate_grant_row("role", 1, None, "all", 0, 0).is_ok());
        assert_eq!(
            validate_grant_row("role", 1, Some("null"), "all", 0, 0)
                .unwrap_err()
                .to_string(),
            "configuration: identity data grants.fields"
        );
    }

    #[test]
    fn fixture_scan_rejects_corrupt_rows_without_echoing_values() {
        let base = || IdentityRows {
            meta: vec![(1, 3)],
            usernames: vec!["alice".into()],
            booleans: vec![("users.is_admin", vec![0, 1])],
            devices: vec![("PENDING".into(), "none".into())],
            permissions: vec!["device.read".into()],
            grants: vec![GrantRow {
                source: "role".into(),
                has_role: 1,
                permissions_json: None,
                scope: "all".into(),
                has_group: 0,
                has_device: 0,
            }],
            orphans: vec![],
        };
        assert!(validate_identity_fixture(&base(), true).is_ok());
        for bad in [vec![], vec![(0, 3)], vec![(1, 2)], vec![(1, 3), (1, 3)]] {
            let mut rows = base();
            rows.meta = bad;
            assert!(validate_identity_fixture(&rows, true).is_err());
        }
        let mut fresh = base();
        fresh.meta.clear();
        assert!(validate_identity_fixture(&fresh, false).is_ok());
        assert!(validate_identity_fixture(&base(), false).is_err());
        for name in ["ab", "Alice", "éric", "a".repeat(65).as_str()] {
            let mut rows = base();
            rows.usernames = vec![name.into()];
            assert!(validate_identity_fixture(&rows, true).is_err());
        }
        let mut rows = base();
        rows.usernames.push("alice".into());
        assert!(validate_identity_fixture(&rows, true).is_err());
        for raw in [-1, 2] {
            let mut rows = base();
            rows.booleans[0].1 = vec![raw];
            assert!(validate_identity_fixture(&rows, true).is_err());
        }
        for (state, decision) in [
            ("PENDING", "denied"),
            ("APPROVED", "approved"),
            ("REVOKED", "revoked"),
        ] {
            let mut rows = base();
            rows.devices = vec![(state.into(), decision.into())];
            assert!(validate_identity_fixture(&rows, true).is_ok());
        }
        for (state, decision) in [
            ("APPROVED", "none"),
            ("PENDING", "approved"),
            ("REVOKED", "denied"),
        ] {
            let mut rows = base();
            rows.devices = vec![(state.into(), decision.into())];
            assert!(validate_identity_fixture(&rows, true).is_err());
        }
        let mut rows = base();
        rows.permissions = vec!["wrong.permission".into()];
        assert!(validate_identity_fixture(&rows, true).is_err());
        for json in [
            Some("null"),
            Some("[\"device.read\"]"),
            Some("broken"),
            Some(""),
        ] {
            let mut rows = base();
            rows.grants[0].permissions_json = json.map(str::to_owned);
            assert!(validate_identity_fixture(&rows, true).is_err());
        }
        for json in [
            None,
            Some("null"),
            Some("[]"),
            Some("{}"),
            Some("[\"unknown\"]"),
        ] {
            let mut rows = base();
            rows.grants[0] = GrantRow {
                source: "direct".into(),
                has_role: 0,
                permissions_json: json.map(str::to_owned),
                scope: "all".into(),
                has_group: 0,
                has_device: 0,
            };
            assert!(validate_identity_fixture(&rows, true).is_err());
        }
        let mut rows = base();
        rows.grants[0] = GrantRow {
            source: "direct".into(),
            has_role: 0,
            permissions_json: Some("[\"device.read\"]".into()),
            scope: "group".into(),
            has_group: 1,
            has_device: 0,
        };
        assert!(validate_identity_fixture(&rows, true).is_ok());
        for presence in [(2, 0, 0), (0, 2, 0), (0, 0, 2)] {
            let mut rows = base();
            rows.grants[0].has_role = presence.0;
            rows.grants[0].has_group = presence.1;
            rows.grants[0].has_device = presence.2;
            assert!(validate_identity_fixture(&rows, true).is_err());
        }
        for category in [
            "sessions",
            "role_permissions",
            "group_members",
            "grants",
            "admission_decisions",
            "audit_events",
        ] {
            let mut rows = base();
            rows.orphans = vec![(category, true)];
            assert!(validate_identity_fixture(&rows, true).is_err());
            rows.orphans[0].1 = false; // inactive/archived parent exists
            assert!(validate_identity_fixture(&rows, true).is_ok());
        }
    }

    #[test]
    fn legacy_fixture_reuses_full_scan_with_exact_v1_v2_meta() {
        // The pre-existing v3 gate rejects clean legacy data. All cases below
        // exercise the same fixture row validators, not a database connection.
        fn legacy_scan(rows: &IdentityRows, expected_version: i32) -> Result<(), ControllerError> {
            let version = legacy_expected_version(expected_version)?;
            validate_identity_fixture_with_meta(rows, Some(version))
        }
        let clean = |version| IdentityRows {
            meta: vec![(1, version)],
            usernames: vec!["alice".into()],
            booleans: BOOLEAN_SCANS
                .iter()
                .map(|&(code, _)| (code, vec![0, 1]))
                .collect(),
            devices: vec![
                ("PENDING".into(), "none".into()),
                ("PENDING".into(), "denied".into()),
                ("APPROVED".into(), "approved".into()),
                ("REVOKED".into(), "revoked".into()),
            ],
            permissions: [
                "device.read",
                "device.status.read",
                "device.reboot",
                "device.task.read",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            grants: vec![
                GrantRow {
                    source: "role".into(),
                    has_role: 1,
                    permissions_json: None,
                    scope: "all".into(),
                    has_group: 0,
                    has_device: 0,
                },
                GrantRow {
                    source: "direct".into(),
                    has_role: 0,
                    permissions_json: Some("[\"device.read\"]".into()),
                    scope: "device".into(),
                    has_group: 0,
                    has_device: 1,
                },
            ],
            orphans: ORPHAN_SCANS
                .iter()
                .map(|&(code, _)| (code, false))
                .collect(),
        };
        for version in [1, 2] {
            let rows = clean(version);
            assert!(legacy_scan(&rows, version).is_ok(), "clean v{version}");
            assert!(validate_identity_fixture(&rows, true).is_err(), "v3 gate");
            assert!(
                validate_identity_fixture(&rows, false).is_err(),
                "fresh gate"
            );
            for wrong in [0, 3, 4, if version == 1 { 2 } else { 1 }] {
                assert!(legacy_scan(&rows, wrong).is_err(), "expected v{wrong}");
            }
            for meta in [
                vec![],
                vec![(0, version)],
                vec![(2, version)],
                vec![(1, version); 2],
            ] {
                let mut rows = clean(version);
                rows.meta = meta;
                assert!(
                    legacy_scan(&rows, version).is_err(),
                    "meta cardinality/value"
                );
            }
            let mut rows = clean(version);
            rows.booleans[0].1 = vec![2];
            assert_eq!(
                legacy_scan(&rows, version).unwrap_err().to_string(),
                "configuration: identity data schema_meta.initialized"
            );
            let mut rows = clean(version);
            rows.usernames[0] = "BadUser".into();
            assert!(legacy_scan(&rows, version).is_err());
            let mut rows = clean(version);
            rows.devices[0] = ("APPROVED".into(), "none".into());
            assert!(legacy_scan(&rows, version).is_err());
            let mut rows = clean(version);
            rows.permissions[0] = "unknown".into();
            assert!(legacy_scan(&rows, version).is_err());
            for invalid_json in ["broken", "null", "[]", "[\"unknown\"]"] {
                let mut rows = clean(version);
                rows.grants[1].permissions_json = Some(invalid_json.into());
                assert!(legacy_scan(&rows, version).is_err(), "invalid grant JSON");
            }
            let mut rows = clean(version);
            rows.grants[0].permissions_json = Some("null".into());
            assert!(
                legacy_scan(&rows, version).is_err(),
                "JSON null != SQL NULL"
            );
            for (role, group, device) in [(2, 0, 0), (0, 2, 0), (0, 0, 2)] {
                let mut rows = clean(version);
                rows.grants[1].has_role = role;
                rows.grants[1].has_group = group;
                rows.grants[1].has_device = device;
                assert!(legacy_scan(&rows, version).is_err(), "invalid presence");
            }
            for category in 0..ORPHAN_SCANS.len() {
                let mut rows = clean(version);
                rows.orphans[category].1 = true;
                assert!(legacy_scan(&rows, version).is_err(), "orphan {category}");
            }
        }
    }
}

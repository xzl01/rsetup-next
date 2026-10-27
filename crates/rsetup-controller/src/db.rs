use crate::{AdmissionSnapshot, AdmissionState, ControllerError, ReviewDecision};
use sqlx::Row;

mod pool;
mod schema_parse;
mod schema_shape;
mod username;

pub use pool::DbPool;
pub use schema_parse::v4_schema_statements;
pub(crate) use username::valid_username;

use schema_parse::*;
use schema_shape::*;

/// v4 唯一基线 DDL（`schema/v4.sql`，恰好 16 张表、无 FOREIGN KEY、无 CHECK）。
///
/// 数据库 schema 只有 v4 一个基线：空库经显式受控入口直接建完整 v4，已是 v4 只做只读
/// 校验，其余任何版本或半成品状态一律拒绝——仓库中不再有 0001..0004 历史执行链，也没有
/// 「先建旧版再 ALTER」的升级路径。本常量对外可见只用于离线契约测试；运行期绝不把整段
/// SQL 当作一条语句执行，所有写入都用 [`v4_schema_statements`] 的白名单解析结果。
pub const V4_SCHEMA_SQL: &str = include_str!("../schema/v4.sql");

/// v4 只读门禁探针：真实实现仅由 `DbPool` 提供，离线测试注入受控 fake 验证决策顺序。
/// 方法集合就是 v4 门禁的全部步骤，没有任何旧版本或续迁入口。
trait IdentitySchemaProbe {
    fn read_version(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<i32>, ControllerError>> + Send;
    /// v4 完整形状（16 表列/索引/字符集/无 CHECK）。
    fn validate_shape(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn validate_data(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    /// runtime 五表存在性快照：仅由固定字面量的 information_schema 探测填充。
    fn runtime_table_presence(
        &self,
    ) -> impl std::future::Future<Output = Result<RuntimeTablePresence, ControllerError>> + Send;
    /// runtime 数据只读扫描（期望版本取编译期 `SUPPORTED_SCHEMA_VERSION`）。
    fn validate_runtime_data(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn validate_fk(&self) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
}

/// v4 唯一门禁（普通启动与显式入口共用）：版本决策 → 16 表完整形状 →
/// runtime 五表齐备 → identity 数据 → runtime 数据 → 无 FK。期望版本恒为编译期
/// `SUPPORTED_SCHEMA_VERSION`，绝不从数据库行本身推断；任何缺失或漂移都 fail-closed。
async fn check_identity_schema_with_probe(
    probe: &impl IdentitySchemaProbe,
) -> Result<(), ControllerError> {
    identity_schema_decision(probe.read_version().await?)?;
    probe.validate_shape().await?;
    let presence = probe.runtime_table_presence().await?;
    crate::integrity::validate_runtime_tables_for_version(
        crate::integrity::SUPPORTED_SCHEMA_VERSION,
        &presence,
    )?;
    probe.validate_data().await?;
    probe.validate_runtime_data().await?;
    probe.validate_fk().await
}

/// Schema v4 运行时五表的存在性快照：仅由固定字面量的 information_schema
/// 探测填充，绝不接受外部输入拼装的表名。`integrity::validate_runtime_tables_for_version`
/// 依据它判定「v4 必须五表齐备」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeTablePresence {
    pub main_tasks: bool,
    pub sub_tasks: bool,
    pub task_previews: bool,
    pub device_operation_locks: bool,
    pub recovery_checks: bool,
}

/// 普通启动版本决策：委托 `integrity::startup_guard_decision`，仅接受编译期
/// 配置 `SUPPORTED_SCHEMA_VERSION`（v4）。旧版本、未知版本、缺失元数据一律
/// `SchemaNotReady`；空库由显式入口的 `ProductionMigrationPlan::CreateV4` 处理。
fn identity_schema_decision(version: Option<i32>) -> Result<(), ControllerError> {
    crate::integrity::startup_guard_decision(version)
}

#[derive(Clone)]
pub struct TestMigrationConfig {
    pub test_url: String,
    pub allow_destructive: bool,
    pub expected_database: String,
    pub backup_ref: String,
    pub migration_ack: String,
}
impl std::fmt::Debug for TestMigrationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestMigrationConfig")
            .field("test_url", &"[REDACTED]")
            .field("allow_destructive", &self.allow_destructive)
            .field("expected_database", &"[REDACTED]")
            .field("backup_ref", &"[REDACTED]")
            .field("migration_ack", &"[REDACTED]")
            .finish()
    }
}
impl TestMigrationConfig {
    #[cfg(test)]
    fn fixture(url: &str, name: &str) -> Self {
        Self {
            test_url: url.into(),
            allow_destructive: false,
            expected_database: name.into(),
            backup_ref: String::new(),
            migration_ack: String::new(),
        }
    }
    // Raw URL equality here only rejects literal reuse of the very same URL string;
    // different URLs do not prove physical isolation. The operator must confirm the target.
    pub fn from_test_env() -> Result<Self, ControllerError> {
        let test_url = std::env::var("CONTROLLER_TEST_DATABASE_URL").unwrap_or_default();
        if std::env::var("CONTROLLER_DATABASE_URL").is_ok_and(|service| service == test_url) {
            return Err(ControllerError::Config(
                "test migration cannot reuse service URL".into(),
            ));
        }
        Ok(Self {
            test_url,
            allow_destructive: std::env::var("CONTROLLER_TEST_ALLOW_DESTRUCTIVE")
                .is_ok_and(|v| v == "1"),
            expected_database: std::env::var("CONTROLLER_TEST_EXPECTED_DATABASE")
                .unwrap_or_default(),
            backup_ref: std::env::var("CONTROLLER_TEST_BACKUP_REF").unwrap_or_default(),
            migration_ack: std::env::var("CONTROLLER_TEST_MIGRATION_ACK").unwrap_or_default(),
        })
    }
    pub fn authorize(&self, actual: &str) -> Result<(), ControllerError> {
        authorize_test_migration(self, actual)
    }
}
fn authorize_test_migration(c: &TestMigrationConfig, actual: &str) -> Result<(), ControllerError> {
    let safe = !c.test_url.trim().is_empty()
        && c.allow_destructive
        && (1..=64).contains(&c.expected_database.len())
        && c.expected_database
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && c.expected_database == actual
        && !c.backup_ref.trim().is_empty()
        && c.migration_ack == "isolated-exclusive-backed-up-disposable";
    if safe {
        Ok(())
    } else {
        Err(ControllerError::Config(
            "test migration authorization required".into(),
        ))
    }
}
fn schema_metadata_privilege_error() -> ControllerError {
    ControllerError::Config("test schema metadata privileges unverified".into())
}

// Only the direct schema-wide ALL form observed for these isolated dev engines
// proves that the account can enumerate schema objects. Never parse account
// names into diagnostics; roles, partial revokes and unknown grant syntax deny.
fn grant_grantee_is_simple_account(grantee: &str) -> bool {
    fn quoted_part(input: &[u8]) -> Option<usize> {
        let quote = *input.first()?;
        if quote != b'`' && quote != b'\'' {
            return None;
        }
        let mut i = 1;
        while i < input.len() {
            match input[i] {
                b'\\' if quote == b'\'' && i + 1 < input.len() => i += 2,
                byte if byte == quote && input.get(i + 1) == Some(&quote) => i += 2,
                byte if byte == quote && i > 1 => return Some(i + 1),
                b'\n' | b'\r' | 0 => return None,
                _ => i += 1,
            }
        }
        None
    }
    let bytes = grantee.as_bytes();
    let Some(user_end) = quoted_part(bytes) else {
        return false;
    };
    if bytes.get(user_end) != Some(&b'@') {
        return false;
    }
    quoted_part(&bytes[user_end + 1..]) == Some(bytes.len() - user_end - 1)
}

fn validate_schema_metadata_grants(
    grants: &[Option<String>],
    expected_database: &str,
) -> Result<(), ControllerError> {
    if !(1..=64).contains(&expected_database.len())
        || !expected_database
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(schema_metadata_privilege_error());
    }
    let direct_prefix = format!("GRANT ALL PRIVILEGES ON `{expected_database}`.* TO ");
    let mut direct = false;
    let mut account: Option<&str> = None;
    for row in grants {
        let text = row.as_deref().ok_or_else(schema_metadata_privilege_error)?;
        let (is_direct, remainder) = if let Some(rest) = text.strip_prefix(&direct_prefix) {
            (true, rest)
        } else if let Some(rest) = text.strip_prefix("GRANT USAGE ON *.* TO ") {
            (false, rest)
        } else {
            return Err(schema_metadata_privilege_error());
        };
        let grantee = remainder
            .strip_suffix(" WITH GRANT OPTION")
            .unwrap_or(remainder);
        if !grant_grantee_is_simple_account(grantee)
            || account.is_some_and(|previous| previous != grantee)
        {
            return Err(schema_metadata_privilege_error());
        }
        account = Some(grantee);
        direct |= is_direct;
    }
    if direct {
        Ok(())
    } else {
        Err(schema_metadata_privilege_error())
    }
}

async fn require_schema_metadata_privilege(
    connection: &mut sqlx::pool::PoolConnection<sqlx::MySql>,
    expected_database: &str,
) -> Result<(), ControllerError> {
    let rows = sqlx::query("SHOW GRANTS")
        .fetch_all(&mut **connection)
        .await
        .map_err(|_| schema_metadata_privilege_error())?;
    let grants = rows
        .iter()
        .map(|row| {
            row.try_get::<Option<String>, _>(0)
                .map_err(|_| schema_metadata_privilege_error())
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_schema_metadata_grants(&grants, expected_database)
}

async fn run_authorized_migration_with<D, E, C, CF, G, GF, U, UF>(
    config: &TestMigrationConfig,
    connect_and_name: C,
    verify_grants: G,
    upgrade: U,
) -> Result<(), ControllerError>
where
    C: FnOnce() -> CF,
    CF: std::future::Future<Output = Result<(D, String), ControllerError>>,
    G: FnOnce(D, String) -> GF,
    GF: std::future::Future<Output = Result<E, ControllerError>>,
    U: FnOnce(E) -> UF,
    UF: std::future::Future<Output = Result<(), ControllerError>>,
{
    authorize_test_migration(config, &config.expected_database)?;
    let (db, actual) = connect_and_name().await?;
    authorize_test_migration(config, &actual)?;
    let db = verify_grants(db, actual).await?;
    upgrade(db).await
}

/// v4 无外键门禁：三条彼此独立的 information_schema 视角必须**同时**可信且为空。
/// 读失败/NULL/不完整一律视为"有外键或不可信"，绝不把错误当成空集。
pub(crate) async fn require_no_identity_foreign_keys(db: &DbPool) -> Result<(), ControllerError> {
    let constraints = sqlx::query(FK_CONSTRAINTS_QUERY)
        .fetch_optional(&db.0)
        .await
        .map_err(|_| ())
        .and_then(|row| {
            row.map(|row| {
                Ok((
                    row.try_get::<Option<String>, _>("table_name")
                        .map_err(|_| ())?,
                    row.try_get::<Option<String>, _>("constraint_name")
                        .map_err(|_| ())?,
                ))
            })
            .transpose()
        });
    let references: Result<Option<i64>, ()> = sqlx::query_scalar(FK_REFERENCES_QUERY)
        .fetch_one(&db.0)
        .await
        .map_err(|_| ());
    let columns: Result<Option<i64>, ()> = sqlx::query_scalar(FK_COLUMNS_QUERY)
        .fetch_one(&db.0)
        .await
        .map_err(|_| ());
    validate_fk_metadata(constraints, references, columns)
}

/// v4 完整形状校验（只读、无写入）：
/// 1. 表集合恰好是 11 identity + 5 runtime，未知表拒绝；
/// 2. 每张表的列名集合、逐列形状（类型/长度/UNSIGNED/NULL/字符集/collation/默认值）、
///    索引集合（名称、列顺序、唯一性、前缀长度）都必须与 `schema/v4.sql` 一致；
/// 3. 每张表都不得存在任何 CHECK 约束（v4 DDL 不声明 CHECK，出现即为漂移）；
///
/// 外键由 [`require_no_identity_foreign_keys`] 单独做全库校验，不在本函数内。
async fn validate_v4_shape(db: &DbPool) -> Result<(), ControllerError> {
    let present: Vec<String> =
        sqlx::query_scalar("SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE()")
            .fetch_all(&db.0).await?;
    validate_table_set(&present)?;
    // 形状解析（[`ddl_parts`]）与白名单解析（[`v4_schema_statements`]）消费同一份 DDL，
    // 两条路径都必须先过注释形态门禁：行内注释会让 [`ddl_parts_from`] 把注释文本并入下一个
    // 片段，从而报出"库里缺列"这种把源码缺陷伪装成数据库漂移的误导性诊断。
    v4_comment_form_check(&v4_schema_body())?;
    for &(table, columns, indexes) in TABLES.iter().chain(RUNTIME_TABLES) {
        let table_collation: String = sqlx::query_scalar("SELECT CAST(table_collation AS CHAR) AS table_collation FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = ?")
            .bind(table).fetch_one(&db.0).await?;
        validate_table_collation(table, &table_collation)?;
        let parts = ddl_parts(table);
        let actual_columns: Vec<String> = sqlx::query_scalar("SELECT CAST(column_name AS CHAR) AS column_name FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ?")
            .bind(table).fetch_all(&db.0).await?;
        validate_object_names(
            table,
            "column",
            &columns.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            &actual_columns,
        )?;
        for &column in columns {
            let declaration = parts
                .iter()
                .find_map(|part| part.strip_prefix(&format!("{column} ")))
                .ok_or_else(|| {
                    ControllerError::Config(format!(
                        "v4 schema DDL missing column {table}.{column}"
                    ))
                })?;
            let actual = read_column(db, table, column).await?;
            validate_column_shape(table, declaration, &actual)?;
            validate_column_charset(table, declaration, &actual, &table_collation)?;
        }
        let expected = expected_indexes(&parts, indexes)?;
        let rows = sqlx::query(STATISTICS_QUERY)
            .bind(table)
            .fetch_all(&db.0)
            .await?;
        let mut actual: Vec<IndexMeta> = Vec::new();
        for row in rows {
            let name: String = row.try_get("index_name")?;
            let column: String = row.try_get("column_name")?;
            let non_unique: i64 = row.try_get("non_unique")?;
            let sub_part: Option<i64> = row.try_get("sub_part")?;
            let seq: i64 = row.try_get("seq_in_index")?;
            if let Some(index) = actual.iter_mut().find(|entry| entry.name == name) {
                if seq != index.columns.len() as i64 + 1 || index.unique != (non_unique == 0) {
                    return Err(ControllerError::Config(format!(
                        "v4 schema incompatible index order {table}.{name}"
                    )));
                }
                index.columns.push(column);
                index.sub_parts.push(sub_part);
            } else {
                if seq != 1 {
                    return Err(ControllerError::Config(format!(
                        "v4 schema incompatible index order {table}.{name}"
                    )));
                }
                actual.push(IndexMeta {
                    name,
                    columns: vec![column],
                    unique: non_unique == 0,
                    sub_parts: vec![sub_part],
                });
            }
        }
        validate_indexes(table, &expected, &actual)?;
        // v4 DDL 不声明任何 CHECK 约束：实际存在一条即视为漂移，直接拒绝。
        let actual_checks: Vec<String> = sqlx::query_scalar("SELECT CAST(constraint_name AS CHAR) AS constraint_name FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND table_name = ? AND constraint_type = 'CHECK'")
            .bind(table).fetch_all(&db.0).await?;
        validate_object_names(table, "check", &[], &actual_checks)?;
    }
    Ok(())
}

/// 16 张 v4 表在元数据可见时必须全部为空（fresh 建库后置数据门禁）。
/// 表名只取自 [`TABLES`]/[`RUNTIME_TABLES`] 编译期常量，绝不来自数据库返回值。
async fn validate_v4_tables_are_empty(db: &DbPool) -> Result<(), ControllerError> {
    for &(table, ..) in TABLES.iter().chain(RUNTIME_TABLES) {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM `{table}`"))
            .fetch_one(&db.0)
            .await
            .map_err(|_| v4_schema_error("fresh schema data scan failed"))?;
        if rows != 0 {
            return Err(v4_schema_error("fresh schema must contain no rows"));
        }
    }
    Ok(())
}

async fn read_column(
    db: &DbPool,
    table: &str,
    column: &str,
) -> Result<ColumnMeta, ControllerError> {
    let row = sqlx::query("SELECT CAST(data_type AS CHAR) AS data_type, CAST(column_type AS CHAR) AS column_type, CAST(is_nullable AS CHAR) AS is_nullable, CAST(character_set_name AS CHAR) AS character_set_name, CAST(collation_name AS CHAR) AS collation_name, CAST(column_default AS CHAR) AS column_default FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?")
        .bind(table).bind(column).fetch_optional(&db.0).await?
        .ok_or_else(|| ControllerError::Config(format!("migration missing column {table}.{column}")))?;
    Ok(ColumnMeta {
        name: column.to_owned(),
        data_type: row.try_get("data_type")?,
        column_type: row.try_get("column_type")?,
        nullable: parse_nullable_metadata(
            table,
            column,
            row.try_get::<Option<String>, _>("is_nullable")?.as_deref(),
        )?,
        character_set_name: row.try_get("character_set_name")?,
        collation_name: row.try_get("collation_name")?,
        default: row.try_get("column_default")?,
    })
}

fn parse_nullable_metadata(
    table: &str,
    column: &str,
    value: Option<&str>,
) -> Result<bool, ControllerError> {
    match value {
        Some("YES") => Ok(true),
        Some("NO") => Ok(false),
        _ => Err(ControllerError::Config(format!(
            "migration incompatible nullability metadata {table}.{column}"
        ))),
    }
}

fn validate_false_default_metadata(
    table: &str,
    column: &str,
    default: Option<&str>,
) -> Result<(), ControllerError> {
    // BOOLEAN is a TINYINT alias; only numeric 0 is accepted here. Other
    // engine readback representations require independent verification.
    if default == Some("0") {
        Ok(())
    } else {
        Err(ControllerError::Config(format!(
            "migration incompatible default {table}.{column}"
        )))
    }
}

async fn read_version(db: &DbPool) -> Result<Option<i32>, ControllerError> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'schema_meta'")
        .fetch_one(&db.0).await?;
    if exists == 0 {
        return Ok(None);
    }
    if exists != 1 {
        return Err(ControllerError::Config(
            "damaged schema_meta table identity".into(),
        ));
    }
    let rows = sqlx::query("SELECT singleton, schema_version FROM schema_meta LIMIT 2")
        .fetch_all(&db.0)
        .await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] if row.try_get::<i8, _>("singleton")? == 1 => {
            Ok(Some(row.try_get("schema_version")?))
        }
        _ => Err(ControllerError::Config(
            "damaged schema_meta singleton".into(),
        )),
    }
}

impl IdentitySchemaProbe for DbPool {
    async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
        read_version(self).await
    }
    async fn validate_shape(&self) -> Result<(), ControllerError> {
        validate_v4_shape(self).await
    }
    async fn validate_data(&self) -> Result<(), ControllerError> {
        crate::integrity::check_identity_data(self).await
    }
    async fn runtime_table_presence(&self) -> Result<RuntimeTablePresence, ControllerError> {
        read_runtime_table_presence(self).await
    }
    async fn validate_runtime_data(&self) -> Result<(), ControllerError> {
        crate::integrity::check_runtime_data(self, crate::integrity::SUPPORTED_SCHEMA_VERSION).await
    }
    async fn validate_fk(&self) -> Result<(), ControllerError> {
        require_no_identity_foreign_keys(self).await
    }
}

pub async fn check_identity_schema(db: &DbPool) -> Result<(), ControllerError> {
    check_identity_schema_with_probe(db).await
}

/// The single accepted migration acknowledgement literal, published for
/// operators and documentation. The shared authorizer hard-codes the very same
/// value; the unit test
/// `production_gate_and_shared_authorizer_accept_the_same_ack` keeps the two
/// spellings from drifting apart. It states what the operator attests:
/// exclusive access to the target database plus a verifiable backup reference.
/// For a production target it must **not** be read as "the data may be
/// discarded" — that is not part of what this entry point does.
pub const MIGRATION_ACK_CONFIRMATION: &str = "isolated-exclusive-backed-up-disposable";

/// Fixed production-migration diagnostic: a static code only, never a URL,
/// database name, backup path or driver message.
fn production_migration_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("production migration {code}"))
}

/// The one rejection message for every missing or malformed gate. Collapsing
/// all failure branches onto it keeps the gate from revealing *which* variable
/// was wrong, and keeps any of their values out of the message.
fn production_migration_authorization_required() -> ControllerError {
    ControllerError::Config("production migration authorization required".into())
}

/// Gate-parsing half of [`migration_config_from_env`], factored out so the
/// logic is testable without mutating the process environment (which would race
/// across parallel tests). `lookup` maps a variable name to its value; the
/// production caller supplies `std::env::var`.
fn production_migration_config_from(
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<TestMigrationConfig, ControllerError> {
    let config = TestMigrationConfig {
        // The target is the service database itself: the same variable the
        // service reads, so a deployment never stores one DSN in two places.
        test_url: lookup("CONTROLLER_DATABASE_URL").unwrap_or_default(),
        allow_destructive: lookup("CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE")
            .is_some_and(|value| value == "1"),
        expected_database: lookup("CONTROLLER_MIGRATION_EXPECTED_DATABASE").unwrap_or_default(),
        backup_ref: lookup("CONTROLLER_MIGRATION_BACKUP_REF").unwrap_or_default(),
        migration_ack: lookup("CONTROLLER_MIGRATION_ACK").unwrap_or_default(),
    };
    // Reusing the shared authorizer with the declared database name as the
    // "actual" name checks exactly the shared criteria (non-empty DSN,
    // destructive flag, database-name charset and length, non-blank backup
    // reference, exact ack) instead of duplicating them.
    // `run_authorized_migration_with` re-runs the same check against the name
    // the server really reports, so a configuration pointing at another
    // database still fails closed.
    authorize_test_migration(&config, &config.expected_database)
        .map_err(|_| production_migration_authorization_required())?;
    Ok(config)
}

/// Reads the **production** migration configuration from the process
/// environment.
///
/// Target: `CONTROLLER_DATABASE_URL` (the very variable the service uses).
/// Gates: `CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE` (must be exactly `1`),
/// `CONTROLLER_MIGRATION_EXPECTED_DATABASE`, `CONTROLLER_MIGRATION_BACKUP_REF`,
/// `CONTROLLER_MIGRATION_ACK` (must equal [`MIGRATION_ACK_CONFIRMATION`]).
///
/// Every missing or illegal combination fails with one fixed, de-identified
/// [`ControllerError::Config`]: no URL, database name or backup reference is
/// ever echoed. `TestMigrationConfig` also redacts those fields in `Debug`.
pub fn migration_config_from_env() -> Result<TestMigrationConfig, ControllerError> {
    production_migration_config_from(|name| std::env::var(name).ok())
}

/// 显式 schema 入口的固定决策表：数据库 schema 版本 → 唯一允许的动作。
/// 纯函数，无数据库即可穷举单测；**只**承认「空库」与「已经是 v4」两种输入。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProductionMigrationPlan {
    /// 真正的空库（无 `schema_meta` 版本行）：直接创建完整 v4。
    CreateV4,
    /// 已是受支持版本 v4：只做只读闭合校验，零 DDL、零写入，因此可重复执行。
    VerifyOnly,
}

/// v4-only 版本决策：`None` → 建库，`Some(4)` → 只读校验，其余（v1/v2/v3、0、
/// 负数、v5+）一律拒绝。不存在"升级后续迁"或"就地修复"的任何路线。
fn production_migration_plan(
    version: Option<i32>,
) -> Result<ProductionMigrationPlan, ControllerError> {
    match version {
        None => Ok(ProductionMigrationPlan::CreateV4),
        Some(crate::integrity::SUPPORTED_SCHEMA_VERSION) => Ok(ProductionMigrationPlan::VerifyOnly),
        // 旧版本、未知版本与未来版本都不做投机处理：交由操作员核查。
        Some(_) => Err(production_migration_error("unsupported schema version")),
    }
}

/// 在已授权的连接上执行决策。DDL 非事务：任一步失败即原样返回，留下未就绪的
/// 库交由操作员核查，绝不自动修复、绝不伪造成功。
async fn run_production_migration_at(db: &DbPool) -> Result<(), ControllerError> {
    match production_migration_plan(read_version(db).await?)? {
        ProductionMigrationPlan::VerifyOnly => check_identity_schema(db).await,
        ProductionMigrationPlan::CreateV4 => create_v4_schema(db).await,
    }
}

/// Operator-facing production migration: authorized exactly like the reviewed
/// test migration (the declared database must equal the one the server reports,
/// and the connecting account must really see its schema metadata), then applies
/// [`production_migration_plan`].
///
/// This entry point does **not** reject a target equal to the service URL — the
/// service database is exactly the point (isolation is attested by the gates).
/// Isolation and exclusivity are attested through the gates, never inferred
/// from URL text.
pub async fn run_production_migration(config: &TestMigrationConfig) -> Result<(), ControllerError> {
    run_authorized_migration_with(
        config,
        || async {
            let db = DbPool(
                sqlx::MySqlPool::connect(&config.test_url)
                    .await
                    .map_err(|_| {
                        ControllerError::Config(
                            "production migration database connect failed".into(),
                        )
                    })?,
            );
            let mut connection = db.0.acquire().await.map_err(|_| {
                ControllerError::Config("production migration database identity failed".into())
            })?;
            let actual: Option<String> = sqlx::query_scalar("SELECT DATABASE()")
                .fetch_one(&mut *connection)
                .await
                .map_err(|_| {
                    ControllerError::Config("production migration database identity failed".into())
                })?;
            let actual = actual.ok_or_else(|| {
                ControllerError::Config("production migration database identity failed".into())
            })?;
            Ok(((db, connection), actual))
        },
        |(db, mut connection), actual| async move {
            require_schema_metadata_privilege(&mut connection, &actual).await?;
            Ok(db)
        },
        |db| async move { run_production_migration_at(&db).await },
    )
    .await
}

/// Read-only schema version probe for `migrate-controller --check`: the version
/// number, or `None` for an uninitialized schema. The return type carries no
/// database name, host or credentials.
pub async fn read_schema_version(db: &DbPool) -> Result<Option<i32>, ControllerError> {
    read_version(db).await
}

// 固定字面量的五表存在性探测：表名全部硬编码，绝不接受外部输入拼装。
const RUNTIME_TABLE_PRESENCE_QUERY: &str = "SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name IN ('main_tasks','sub_tasks','task_previews','device_operation_locks','recovery_checks')";

async fn read_runtime_table_presence(db: &DbPool) -> Result<RuntimeTablePresence, ControllerError> {
    let names: Vec<String> = sqlx::query_scalar(RUNTIME_TABLE_PRESENCE_QUERY)
        .fetch_all(&db.0)
        .await
        .map_err(|_| v4_schema_error("runtime table presence read failed"))?;
    let present = |name: &str| names.iter().any(|seen| seen == name);
    Ok(RuntimeTablePresence {
        main_tasks: present("main_tasks"),
        sub_tasks: present("sub_tasks"),
        task_previews: present("task_previews"),
        device_operation_locks: present("device_operation_locks"),
        recovery_checks: present("recovery_checks"),
    })
}

/// v4 fresh 建库探针：在只读门禁探针之上补上唯一的写路径三件事
/// （按固定 DDL 建一张表、确认 16 表皆空、写入 v4 元数据行）。
/// 真实实现仅由 `DbPool` 提供；离线测试注入受控 fake 验证顺序与失败即停。
trait FreshV4Probe: IdentitySchemaProbe {
    /// 当前库中已存在的全部表名（information_schema 视角）。
    fn schema_tables(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<String>, ControllerError>> + Send;
    /// 执行一条白名单内的固定 CREATE 语句（DDL 非事务）。
    fn create_table(
        &self,
        statement: &str,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    /// 16 张表必须全部零行。
    fn validate_tables_are_empty(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    /// 写入 `schema_meta` 单行：`singleton = 1`、`schema_version = 4`、未初始化。
    fn insert_v4_meta(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
}

/// 空库 → 完整 v4 的唯一建库流程（显式入口专用，普通启动路径绝不隐式调用）：
///
/// 1. 版本必须"不存在"——已有任何版本行（含 v4）都拒绝，绝不就地升级或续迁；
/// 2. **第一条 DDL 之前**必须确认库中零张表：部分建表、旧表、未知表一律拒绝，
///    因此本函数在任何拒绝路径上的写入次数恒为 0；
/// 3. 逐条执行 `schema/v4.sql` 的 16 条固定 CREATE，每条后复核已见表集合
///    恰为预期前缀（漂移即停，绝不自动修复）；
/// 4. 完整形状 + 无 FK + 16 表皆空；
/// 5. 写入 `schema_meta(schema_version = 4)`；结果不确定时绝不宣称成功；
/// 6. 复核版本落定为 4，再跑一次与普通启动完全相同的只读闭合门禁。
async fn create_v4_schema_with_probe(probe: &impl FreshV4Probe) -> Result<(), ControllerError> {
    if probe.read_version().await?.is_some() {
        return Err(v4_schema_error("fresh create requires an empty database"));
    }
    if !probe.schema_tables().await?.is_empty() {
        return Err(v4_schema_error(
            "fresh create requires zero existing tables before the first DDL",
        ));
    }
    let mut created: Vec<&'static str> = Vec::new();
    for (table, statement) in v4_schema_statements()? {
        probe.create_table(&statement).await?;
        created.push(table);
        let observed = probe.schema_tables().await?;
        let drift = observed.len() != created.len()
            || created
                .iter()
                .any(|name| !observed.iter().any(|seen| seen == name));
        if drift {
            return Err(v4_schema_error(
                "create result disagrees with schema metadata",
            ));
        }
    }
    probe.validate_shape().await?;
    probe.validate_fk().await?;
    probe.validate_tables_are_empty().await?;
    if probe.insert_v4_meta().await.is_err() {
        // 写入返回错误时结果本身不可信：只有读回**成功**并能证明版本行缺失或不是
        // 受支持版本，才允许固定为"被拒"；读回自身失败无法证明未提交，与"读回已是
        // 受支持版本"一样都只能报结果不确定。两种情形都绝不宣称成功、绝不自动重试。
        let outcome = match probe.read_version().await {
            Err(_) => "version write outcome uncertain",
            Ok(Some(crate::integrity::SUPPORTED_SCHEMA_VERSION)) => {
                "version write outcome uncertain"
            }
            Ok(_) => "version write rejected",
        };
        return Err(v4_schema_error(outcome));
    }
    if probe.read_version().await? != Some(crate::integrity::SUPPORTED_SCHEMA_VERSION) {
        return Err(v4_schema_error(
            "version readback is not the supported version",
        ));
    }
    check_identity_schema_with_probe(probe).await
}

impl FreshV4Probe for DbPool {
    async fn schema_tables(&self) -> Result<Vec<String>, ControllerError> {
        sqlx::query_scalar("SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE()")
            .fetch_all(&self.0)
            .await
            .map_err(|_| v4_schema_error("schema table presence read failed"))
    }
    async fn create_table(&self, statement: &str) -> Result<(), ControllerError> {
        sqlx::query(statement)
            .execute(&self.0)
            .await
            .map_err(|_| v4_schema_error("runtime DDL failed"))?;
        Ok(())
    }
    async fn validate_tables_are_empty(&self) -> Result<(), ControllerError> {
        validate_v4_tables_are_empty(self).await
    }
    async fn insert_v4_meta(&self) -> Result<(), ControllerError> {
        let result = sqlx::query("INSERT INTO schema_meta (singleton, schema_version, instance_id, initialized, authz_epoch, admin_guard_revision) VALUES (1, ?, ?, FALSE, 0, 0)")
            .bind(crate::integrity::SUPPORTED_SCHEMA_VERSION)
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice())
            .execute(&self.0)
            .await
            .map_err(|_| v4_schema_error("version write rejected"))?;
        if result.rows_affected() != 1 {
            return Err(v4_schema_error("version write rejected"));
        }
        Ok(())
    }
}

/// 显式 v4 建库入口（仅由 `run_production_migration` 在门禁与身份复核之后调用）。
async fn create_v4_schema(db: &DbPool) -> Result<(), ControllerError> {
    create_v4_schema_with_probe(db).await
}

pub trait AdmissionStore {
    fn load(
        &self,
        public_key: [u8; 32],
    ) -> impl std::future::Future<Output = Result<AdmissionSnapshot, ControllerError>> + Send;
    fn compare_and_set(
        &self,
        public_key: [u8; 32],
        expected_revision: u64,
        expected_state: AdmissionState,
        decision: ReviewDecision,
        actor_id: Option<[u8; 16]>,
        reason: Option<&str>,
    ) -> impl std::future::Future<Output = Result<AdmissionSnapshot, ControllerError>> + Send;
}

fn decode_snapshot(
    state: &str,
    decision: &str,
    revision: u64,
) -> Result<AdmissionSnapshot, ControllerError> {
    crate::validate_device_fields(state, decision).map_err(|_| {
        ControllerError::Config("invalid stored admission state/decision combination".into())
    })?;
    let admission_state = match state {
        "PENDING" => AdmissionState::Pending,
        "APPROVED" => AdmissionState::Approved,
        "REVOKED" => AdmissionState::Revoked,
        _ => {
            return Err(ControllerError::Config(
                "invalid stored admission state".into(),
            ));
        }
    };
    let review_decision = match decision {
        "none" => ReviewDecision::None,
        "approved" => ReviewDecision::Approved,
        "denied" => ReviewDecision::Denied,
        "revoked" => ReviewDecision::Revoked,
        _ => {
            return Err(ControllerError::Config(
                "invalid stored review decision".into(),
            ));
        }
    };
    Ok(AdmissionSnapshot {
        admission_state,
        review_decision,
        revision,
    })
}

fn next_snapshot(
    snapshot: AdmissionSnapshot,
    decision: ReviewDecision,
) -> Result<AdmissionSnapshot, ControllerError> {
    use AdmissionState::{Approved, Pending, Revoked};
    use ReviewDecision::{Approved as Yes, Denied, None, Revoked as No};
    let admission_state = match (snapshot.admission_state, snapshot.review_decision, decision) {
        (Pending, None, Yes) | (Revoked, No, Yes) => Approved,
        (Pending, None, Denied) => Pending,
        (Pending, Denied, None) => Pending,
        (Approved, Yes, No) => Revoked,
        _ => return Err(ControllerError::RevisionConflict),
    };
    Ok(AdmissionSnapshot {
        admission_state,
        review_decision: decision,
        revision: snapshot
            .revision
            .checked_add(1)
            .ok_or(ControllerError::RevisionConflict)?,
    })
}

pub(crate) fn system_fallback_time_evidence() -> String {
    static CLOCK_EPOCH: std::sync::OnceLock<uuid::Uuid> = std::sync::OnceLock::new();
    let wall = chrono::Utc::now().to_rfc3339();
    serde_json::json!({
        "system_wall_utc": wall,
        "reference_utc": wall,
        "quality": "system_fallback",
        "clock_epoch": CLOCK_EPOCH.get_or_init(uuid::Uuid::new_v4).to_string(),
        "source": null,
        "sample_age_ms": null,
        "offset_ms": null,
        "uncertainty_ms": null
    })
    .to_string()
}

pub(crate) fn next_event_seq(
    counter: &std::sync::atomic::AtomicU64,
) -> Result<u64, ControllerError> {
    let mut current = counter.load(std::sync::atomic::Ordering::Relaxed);
    loop {
        let next = current
            .checked_add(1)
            .ok_or(ControllerError::RevisionConflict)?;
        match counter.compare_exchange_weak(
            current,
            next,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(prev) => return Ok(prev),
            Err(actual) => current = actual,
        }
    }
}

impl AdmissionStore for DbPool {
    async fn load(&self, public_key: [u8; 32]) -> Result<AdmissionSnapshot, ControllerError> {
        let row = sqlx::query(
            "SELECT admission_state, review_decision, revision FROM devices WHERE public_key = ?",
        )
        .bind(public_key.as_slice())
        .fetch_optional(&self.0)
        .await?
        .ok_or(ControllerError::NotFound)?;
        decode_snapshot(
            row.try_get("admission_state")?,
            row.try_get("review_decision")?,
            row.try_get::<u64, _>("revision")?,
        )
    }

    async fn compare_and_set(
        &self,
        public_key: [u8; 32],
        expected_revision: u64,
        expected_state: AdmissionState,
        decision: ReviewDecision,
        actor_id: Option<[u8; 16]>,
        reason: Option<&str>,
    ) -> Result<AdmissionSnapshot, ControllerError> {
        use std::sync::{OnceLock, atomic::AtomicU64};
        static PROCESS_EPOCH: OnceLock<uuid::Uuid> = OnceLock::new();
        static EVENT_SEQ: AtomicU64 = AtomicU64::new(0);
        let mut tx = self.0.begin().await?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        if let Some(actor) = actor_id {
            crate::integrity::require_active_admin(&mut tx, actor).await?;
        }
        let row = sqlx::query("SELECT admission_state, review_decision, revision FROM devices WHERE public_key = ? FOR UPDATE")
            .bind(public_key.as_slice()).fetch_optional(&mut *tx).await?.ok_or(ControllerError::NotFound)?;
        let current = decode_snapshot(
            row.try_get("admission_state")?,
            row.try_get("review_decision")?,
            row.try_get::<u64, _>("revision")?,
        )?;
        if current.revision != expected_revision || current.admission_state != expected_state {
            return Err(ControllerError::RevisionConflict);
        }
        let next = next_snapshot(current, decision)?;
        let state = match next.admission_state {
            AdmissionState::Pending => "PENDING",
            AdmissionState::Approved => "APPROVED",
            AdmissionState::Revoked => "REVOKED",
        };
        let decision_value = match next.review_decision {
            ReviewDecision::None => "none",
            ReviewDecision::Approved => "approved",
            ReviewDecision::Denied => "denied",
            ReviewDecision::Revoked => "revoked",
        };
        let changed = sqlx::query("UPDATE devices SET admission_state = ?, review_decision = ?, revision = ? WHERE public_key = ? AND revision = ? AND admission_state = ? AND review_decision = ?")
            .bind(state).bind(decision_value).bind(next.revision).bind(public_key.as_slice()).bind(expected_revision)
            .bind(match current.admission_state { AdmissionState::Pending => "PENDING", AdmissionState::Approved => "APPROVED", AdmissionState::Revoked => "REVOKED" })
            .bind(match current.review_decision { ReviewDecision::None => "none", ReviewDecision::Approved => "approved", ReviewDecision::Denied => "denied", ReviewDecision::Revoked => "revoked" })
            .execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        let evidence = system_fallback_time_evidence();
        sqlx::query("INSERT INTO admission_decisions (id, device_id, actor_id, decision, previous_revision, new_revision, reason, time_evidence) VALUES (?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice()).bind(public_key.as_slice())
            .bind(actor_id.as_ref().map(|id| id.as_slice())).bind(decision_value)
            .bind(current.revision).bind(next.revision).bind(reason).bind(&evidence)
            .execute(&mut *tx).await?;
        let epoch = PROCESS_EPOCH.get_or_init(uuid::Uuid::new_v4);
        let seq = next_event_seq(&EVENT_SEQ)?;
        sqlx::query("INSERT INTO audit_events (id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted, outcome, time_evidence, process_epoch, event_seq) VALUES (?, ?, ?, ?, 'device', ?, '{}', 'success', ?, ?, ?)")
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice())
            .bind(if actor_id.is_some() { "user" } else { "system" })
            .bind(actor_id.as_ref().map(|id| id.as_slice())).bind(audit_code(decision, current.admission_state))
            .bind(hex::encode(public_key)).bind(&evidence).bind(epoch.as_bytes().as_slice()).bind(seq)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(next)
    }
}

fn audit_code(decision: ReviewDecision, previous: AdmissionState) -> &'static str {
    match (decision, previous) {
        (ReviewDecision::Approved, AdmissionState::Revoked) => "admission.reauthorize",
        (ReviewDecision::Approved, _) => "admission.approve",
        (ReviewDecision::Denied, _) => "admission.reject",
        (ReviewDecision::None, _) => "admission.reopen",
        (ReviewDecision::Revoked, _) => "admission.revoke",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ===== v4-only 基线：版本决策、DDL 解析、空库直建与表集合契约 =====

    #[test]
    fn production_migration_plan_accepts_only_empty_database_and_v4() {
        // 生产共享决策：空库 → 直接创建完整 v4。
        assert_eq!(
            production_migration_plan(None).unwrap(),
            ProductionMigrationPlan::CreateV4,
            "空库必须直接创建完整 v4"
        );
        // 已是 v4 → 只读校验（重复执行幂等）。
        assert_eq!(
            production_migration_plan(Some(crate::integrity::SUPPORTED_SCHEMA_VERSION)).unwrap(),
            ProductionMigrationPlan::VerifyOnly
        );
        // v1/v2/v3 与 0、负数、未来版本一律拒绝：不存在任何续迁路线。
        for version in [
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(5),
            Some(-1),
            Some(i32::MAX),
        ] {
            let error = production_migration_plan(version).expect_err("非空库且非 v4 必须拒绝");
            let message = error.to_string();
            assert!(message.is_ascii(), "固定诊断必须纯 ASCII，实际：{message}");
            assert!(
                !message.contains("mysql://") && !message.contains('@'),
                "固定诊断不得含连接标识，实际：{message}"
            );
        }
    }

    /// v4 fresh 建库流程的受控 fake：把「已存在的表」「建表次数」「元数据写入次数」
    /// 「只读门禁次数」显式暴露成断言目标，用于验证顺序与失败即停。
    #[derive(Default)]
    struct FreshState {
        created: Vec<String>,
        creates: usize,
        meta_writes: usize,
        readonly_checks: usize,
        version: Option<i32>,
    }

    #[derive(Default)]
    struct FakeFresh {
        version: Option<i32>,
        existing: Vec<String>,
        create_error_at: Option<usize>,
        create_drift: bool,
        shape_error: bool,
        fk_error: bool,
        empty_error: bool,
        meta_error: bool,
        meta_lands_anyway: bool,
        /// true 时写入返回成功，但读回版本恒为 3（版本落定失败）。
        stuck_readback: bool,
        /// true 时写入之后的版本读回**自身失败**（连"没落定"都无法证明）。
        readback_error: bool,
        state: std::sync::Mutex<FreshState>,
    }

    impl IdentitySchemaProbe for FakeFresh {
        async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
            let state = self.state.lock().unwrap();
            if state.meta_writes > 0 {
                if self.readback_error {
                    // 读回自身失败：既不能证明落定，也不能证明未落定。
                    return Err(v4_schema_error("injected readback failure"));
                }
                return Ok(if self.stuck_readback {
                    Some(3)
                } else {
                    state.version
                });
            }
            Ok(self.version)
        }
        async fn validate_shape(&self) -> Result<(), ControllerError> {
            if self.shape_error {
                return Err(v4_schema_error("injected shape drift"));
            }
            Ok(())
        }
        async fn validate_data(&self) -> Result<(), ControllerError> {
            self.state.lock().unwrap().readonly_checks += 1;
            Ok(())
        }
        async fn runtime_table_presence(&self) -> Result<RuntimeTablePresence, ControllerError> {
            Ok(full_presence())
        }
        async fn validate_runtime_data(&self) -> Result<(), ControllerError> {
            Ok(())
        }
        async fn validate_fk(&self) -> Result<(), ControllerError> {
            if self.fk_error {
                return Err(v4_schema_error("injected foreign key"));
            }
            Ok(())
        }
    }

    impl FreshV4Probe for FakeFresh {
        async fn schema_tables(&self) -> Result<Vec<String>, ControllerError> {
            let state = self.state.lock().unwrap();
            Ok(self
                .existing
                .iter()
                .cloned()
                .chain(state.created.iter().cloned())
                .collect())
        }
        async fn create_table(&self, statement: &str) -> Result<(), ControllerError> {
            let mut state = self.state.lock().unwrap();
            state.creates += 1;
            if self.create_error_at == Some(state.creates) {
                return Err(v4_schema_error("injected DDL failure"));
            }
            let table = statement
                .strip_prefix("CREATE TABLE IF NOT EXISTS ")
                .and_then(|rest| rest.split_once('('))
                .map(|(name, _)| name.trim())
                .ok_or_else(|| v4_schema_error("injected unparsable DDL"))?;
            if !self.create_drift {
                state.created.push(table.to_owned());
            }
            Ok(())
        }
        async fn validate_tables_are_empty(&self) -> Result<(), ControllerError> {
            if self.empty_error {
                return Err(v4_schema_error("injected nonempty table"));
            }
            Ok(())
        }
        async fn insert_v4_meta(&self) -> Result<(), ControllerError> {
            let mut state = self.state.lock().unwrap();
            state.meta_writes += 1;
            if self.meta_error {
                if self.meta_lands_anyway {
                    // 驱动报错但写入实际落定：结果不确定，绝不宣称成功。
                    state.version = Some(crate::integrity::SUPPORTED_SCHEMA_VERSION);
                }
                return Err(v4_schema_error("injected write failure"));
            }
            state.version = Some(crate::integrity::SUPPORTED_SCHEMA_VERSION);
            Ok(())
        }
    }

    #[tokio::test]
    async fn fresh_create_rejects_any_existing_version_before_touching_ddl() {
        for version in [
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(0),
            Some(-1),
        ] {
            let fake = FakeFresh {
                version,
                ..FakeFresh::default()
            };
            assert!(
                create_v4_schema_with_probe(&fake).await.is_err(),
                "已存在版本 {version:?} 必须拒绝建库"
            );
            let state = fake.state.lock().unwrap();
            assert_eq!(state.creates, 0, "拒绝路径必须零 DDL");
            assert_eq!(state.meta_writes, 0, "拒绝路径必须零元数据写入");
        }
    }

    #[tokio::test]
    async fn fresh_create_rejects_partial_schema_before_first_ddl() {
        // 部分建表 / 旧表残留 / 未知表：即便没有版本行也不允许"顺手补齐"。
        for existing in [
            vec!["users".to_owned()],
            vec!["schema_meta".to_owned()],
            vec!["main_tasks".to_owned(), "sub_tasks".to_owned()],
            vec!["legacy_users".to_owned()],
        ] {
            let fake = FakeFresh {
                existing,
                ..FakeFresh::default()
            };
            assert!(
                create_v4_schema_with_probe(&fake).await.is_err(),
                "非真正空库必须拒绝建库"
            );
            let state = fake.state.lock().unwrap();
            assert_eq!(state.creates, 0, "空库判定在首条 DDL 之前，拒绝必须零写入");
            assert_eq!(state.meta_writes, 0);
        }
    }

    #[tokio::test]
    async fn fresh_create_writes_meta_only_after_full_shape_and_empty_data_pass() {
        let fake = FakeFresh::default();
        create_v4_schema_with_probe(&fake)
            .await
            .expect("真正空库必须一次建满 v4");
        let state = fake.state.lock().unwrap();
        assert_eq!(state.creates, 16, "必须恰好执行 16 条 CREATE");
        assert_eq!(state.created.len(), 16, "16 张表都必须可见");
        assert_eq!(state.meta_writes, 1, "元数据只写一次");
        assert_eq!(
            state.version,
            Some(crate::integrity::SUPPORTED_SCHEMA_VERSION)
        );
        assert_eq!(state.readonly_checks, 1, "版本落定后必须跑一次只读闭合门禁");
    }

    #[tokio::test]
    async fn fresh_create_stops_at_first_failure_and_never_writes_meta() {
        // DDL 失败：立即返回，绝不自动修复或续跑。
        let fake = FakeFresh {
            create_error_at: Some(3),
            ..FakeFresh::default()
        };
        assert!(create_v4_schema_with_probe(&fake).await.is_err());
        assert_eq!(
            fake.state.lock().unwrap().meta_writes,
            0,
            "建表失败绝不写版本"
        );

        // 建表成功但元数据不承认（漂移）：同样停。
        let fake = FakeFresh {
            create_drift: true,
            ..FakeFresh::default()
        };
        assert!(create_v4_schema_with_probe(&fake).await.is_err());
        assert_eq!(
            fake.state.lock().unwrap().meta_writes,
            0,
            "建表结果与元数据不一致绝不写版本"
        );

        // 形状漂移 / 外键漂移 / 数据非空：都不写版本。
        for (shape, fk, empty) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let fake = FakeFresh {
                shape_error: shape,
                fk_error: fk,
                empty_error: empty,
                ..FakeFresh::default()
            };
            assert!(create_v4_schema_with_probe(&fake).await.is_err());
            assert_eq!(
                fake.state.lock().unwrap().meta_writes,
                0,
                "shape={shape} fk={fk} empty={empty} 时绝不写版本"
            );
        }
    }

    #[tokio::test]
    async fn fresh_create_never_claims_success_when_meta_write_is_untrusted() {
        // 写入被拒：报未就绪，且不跑成功门禁。
        let fake = FakeFresh {
            meta_error: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("元数据写入被拒必须报错");
        assert!(
            error.to_string().contains("version write rejected"),
            "写入被拒必须显式报未就绪，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().readonly_checks,
            0,
            "写入失败绝不跑成功门禁"
        );

        // 写入结果不确定（报错但实际落定）：同样绝不宣称成功。
        let fake = FakeFresh {
            meta_error: true,
            meta_lands_anyway: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("写入结果不确定必须报错");
        assert!(
            error
                .to_string()
                .contains("version write outcome uncertain"),
            "不确定结果必须显式报不确定，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().readonly_checks,
            0,
            "不确定结果绝不跑成功门禁"
        );
    }

    #[tokio::test]
    async fn fresh_create_reports_uncertain_when_failed_write_cannot_be_read_back() {
        // 写入报错之后版本读回也失败：连"确实没落定"都证明不了，只能报结果不确定。
        let fake = FakeFresh {
            meta_error: true,
            readback_error: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("写入失败且读回失败必须报错");
        assert!(
            error
                .to_string()
                .contains("version write outcome uncertain"),
            "读回自身失败不能证明未提交，必须报不确定，实际：{error}"
        );
        let (meta_writes, readonly_checks) = {
            let state = fake.state.lock().unwrap();
            (state.meta_writes, state.readonly_checks)
        };
        assert_eq!(meta_writes, 1, "不确定结果绝不自动重试");
        assert_eq!(readonly_checks, 0, "不确定结果绝不跑成功门禁");

        // 对照：读回成功且能证明版本行不是受支持版本，才允许固定为"被拒"。
        let fake = FakeFresh {
            meta_error: true,
            stuck_readback: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("读回确证非受支持版本必须报错");
        assert!(
            error.to_string().contains("version write rejected"),
            "读回确证非受支持版本才可报被拒，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().meta_writes,
            1,
            "被拒同样绝不自动重试"
        );
    }

    #[tokio::test]
    async fn fresh_create_requires_supported_version_readback_before_closure_check() {
        let fake = FakeFresh {
            stuck_readback: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("读回版本不是受支持版本必须报错");
        assert!(
            error
                .to_string()
                .contains("version readback is not the supported version"),
            "读回漂移必须显式报错，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().readonly_checks,
            0,
            "读回漂移绝不跑闭合门禁"
        );
    }

    #[test]
    fn v4_schema_error_messages_are_fixed_ascii_without_identifiers() {
        let message = v4_schema_error("injected shape drift").to_string();
        assert!(message.is_ascii(), "诊断必须纯 ASCII：{message}");
        assert!(message.starts_with("configuration: v4 schema baseline rejected: "));
        for secret in ["mysql://", "@", "password", "BEGIN", "COMMIT", "ROLLBACK"] {
            assert!(!message.contains(secret), "诊断去敏失败：{message}");
        }
    }

    // ===== 与 v4-only 契约仍然一致的通用门禁 / 授权 / 去敏测试（原样保留） =====

    fn production_gate_env(overrides: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        let mut vars: Vec<(String, Option<String>)> = vec![
            (
                "CONTROLLER_DATABASE_URL".into(),
                Some("mysql://svc:Fa1ePassw0rd@db.invalid:3306/rsc_prod_ctl".into()),
            ),
            (
                "CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE".into(),
                Some("1".into()),
            ),
            (
                "CONTROLLER_MIGRATION_EXPECTED_DATABASE".into(),
                Some("rsc_prod_ctl".into()),
            ),
            (
                "CONTROLLER_MIGRATION_BACKUP_REF".into(),
                Some("/srv/backup/FAKE-ref-hashes.sha256".into()),
            ),
            (
                "CONTROLLER_MIGRATION_ACK".into(),
                Some(MIGRATION_ACK_CONFIRMATION.into()),
            ),
        ];
        for (name, value) in overrides {
            match vars.iter_mut().find(|(seen, _)| seen == name) {
                Some(item) => item.1 = (*value).map(str::to_owned),
                None => vars.push(((*name).to_owned(), (*value).map(str::to_owned))),
            }
        }
        vars
    }

    fn production_gate_from(
        overrides: &[(&str, Option<&str>)],
    ) -> Result<TestMigrationConfig, ControllerError> {
        let vars = production_gate_env(overrides);
        production_migration_config_from(|name| {
            vars.iter()
                .find(|(seen, _)| seen == name)
                .and_then(|(_, value)| value.clone())
        })
    }

    fn production_gate_error_is_fixed(message: &str) {
        assert_eq!(
            message, "configuration: production migration authorization required",
            "门禁失败必须使用唯一固定文案"
        );
        for secret in [
            "Fa1ePassw0rd",
            "mysql://",
            "db.invalid",
            "3306",
            "rsc_prod_ctl",
            "/srv/backup/",
            "FAKE-ref",
        ] {
            assert!(
                !message.contains(secret),
                "固定错误不得含 {secret}，实际：{message}"
            );
        }
    }

    #[test]
    fn production_migration_gate_rejects_every_incomplete_gate_set() {
        // 完整门禁必须先被接受，否则下面的负例只是在测同一处笔误。
        let config = production_gate_from(&[]).expect("完整门禁必须通过");
        assert!(config.allow_destructive);
        assert_eq!(config.expected_database, "rsc_prod_ctl");
        for overrides in [
            vec![("CONTROLLER_DATABASE_URL", None)],
            vec![("CONTROLLER_DATABASE_URL", Some("   "))],
            vec![("CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE", None)],
            vec![("CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE", Some("0"))],
            vec![("CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE", Some("true"))],
            vec![("CONTROLLER_MIGRATION_EXPECTED_DATABASE", None)],
            vec![("CONTROLLER_MIGRATION_EXPECTED_DATABASE", Some(""))],
            vec![(
                "CONTROLLER_MIGRATION_EXPECTED_DATABASE",
                Some("rsc prod ctl"),
            )],
            vec![("CONTROLLER_MIGRATION_EXPECTED_DATABASE", Some("rsc`"))],
            vec![(
                "CONTROLLER_MIGRATION_EXPECTED_DATABASE",
                Some("rsc_prod_ctl WITH INSERT INTO users"),
            )],
            vec![("CONTROLLER_MIGRATION_BACKUP_REF", None)],
            vec![("CONTROLLER_MIGRATION_BACKUP_REF", Some(""))],
            vec![("CONTROLLER_MIGRATION_BACKUP_REF", Some("   "))],
            vec![("CONTROLLER_MIGRATION_ACK", None)],
            vec![("CONTROLLER_MIGRATION_ACK", Some("isolated"))],
            vec![(
                "CONTROLLER_MIGRATION_ACK",
                Some("ISOLATED-EXCLUSIVE-BACKED-UP-DISPOSABLE"),
            )],
            vec![
                ("CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE", Some("0")),
                ("CONTROLLER_MIGRATION_BACKUP_REF", None),
            ],
        ] {
            let error = production_gate_from(&overrides).expect_err("不完整门禁必须拒绝");
            production_gate_error_is_fixed(&error.to_string());
        }
    }

    #[test]
    fn production_gate_and_shared_authorizer_accept_the_same_ack() {
        // 生产入口复用共享授权器，因此常量必须被它原样接受：两处字面量一旦漂移，
        // 本用例即判红（生产入口不得悄悄拥有第二套更弱的确认语义）。
        let config = TestMigrationConfig {
            test_url: "mysql://svc:Fa1ePassw0rd@db.invalid:3306/rsc_prod_ctl".into(),
            allow_destructive: true,
            expected_database: "rsc_prod_ctl".into(),
            backup_ref: "/srv/backup/FAKE-ref-hashes.sha256".into(),
            migration_ack: MIGRATION_ACK_CONFIRMATION.into(),
        };
        authorize_test_migration(&config, "rsc_prod_ctl").expect("常量必须被共享授权器接受");
        // 声明库名与真实连接返回的库名不一致仍然拒绝，且固定错误不回显任一名。
        let message = authorize_test_migration(&config, "rsc_other_db")
            .expect_err("库名不符必须拒绝")
            .to_string();
        assert!(
            !message.contains("rsc_other_db") && !message.contains("rsc_prod_ctl"),
            "固定错误不得含库名，实际：{message}"
        );
        // Debug 同样不得外泄 DSN、库名或备份引用。
        let rendered = format!("{config:?}");
        for secret in ["Fa1ePassw0rd", "rsc_prod_ctl", "/srv/backup/"] {
            assert!(
                !rendered.contains(secret),
                "Debug 不得含 {secret}，实际：{rendered}"
            );
        }
    }

    #[test]
    fn schema_metadata_grants_require_exact_direct_schema_all() {
        let target = "dev_identity";
        let direct = "GRANT ALL PRIVILEGES ON `dev_identity`.* TO `tester`@`localhost`";
        let usage = "GRANT USAGE ON *.* TO `tester`@`localhost`";
        assert!(validate_schema_metadata_grants(&[Some(direct.into())], target).is_ok());
        assert!(
            validate_schema_metadata_grants(
                &[
                    Some(usage.into()),
                    Some(format!("{direct} WITH GRANT OPTION"))
                ],
                target,
            )
            .is_ok()
        );
        for invalid in [
            "GRANT ALL PRIVILEGES ON `dev_identity_backup`.* TO `tester`@`localhost`",
            "GRANT ALL PRIVILEGES ON `other_dev_identity`.* TO `tester`@`localhost`",
            "GRANT ALL PRIVILEGES ON *.* TO `tester`@`localhost`",
            "GRANT USAGE ON *.* TO `tester`@`localhost`",
            "GRANT `metadata_role`@`localhost` TO `tester`@`localhost`",
            "not a recognized SHOW GRANTS row",
            "GRANT ALL PRIVILEGES ON `dev_identity`.* TO `tester`@`localhost` arbitrary",
        ] {
            assert!(validate_schema_metadata_grants(&[Some(invalid.into())], target).is_err());
        }
        for grants in [
            vec![],
            vec![None],
            vec![Some(direct.into()), None],
            vec![
                Some(direct.into()),
                Some("REVOKE SELECT ON `dev_identity`.`hidden` FROM `tester`@`localhost`".into()),
            ],
            vec![
                Some(direct.into()),
                Some("GRANT USAGE ON *.* TO `different`@`localhost`".into()),
            ],
            vec![
                Some(direct.into()),
                Some("GRANT `metadata_role`@`localhost` TO `tester`@`localhost`".into()),
            ],
        ] {
            assert!(validate_schema_metadata_grants(&grants, target).is_err());
        }
        assert!(
            validate_schema_metadata_grants(&[Some(direct.into())], "dev_identity_bad").is_err()
        );
        assert!(validate_schema_metadata_grants(&[Some(direct.into())], "dev_identity`").is_err());
        assert!(
            !format!(
                "{}",
                validate_schema_metadata_grants(&[None], target).unwrap_err()
            )
            .contains(target)
        );
    }

    #[test]
    fn normal_startup_accepts_only_v4_and_rejects_old_or_unknown_versions() {
        // 普通启动期望版本来自编译期配置 = 4（`startup_guard_decision`）。
        assert!(identity_schema_decision(Some(4)).is_ok());
        // 缺失版本行或旧版一律拒绝普通启动；空库仅可显式初始化 v4，旧版无升级路线。
        for version in [None, Some(1), Some(2), Some(3)] {
            assert!(
                matches!(identity_schema_decision(version), Err(ControllerError::SchemaNotReady { found, required: 4 }) if found == version)
            );
        }
        // 未知版本（0、负数、v5+）同样 fail-closed，复用 SchemaNotReady 变体。
        for version in [Some(0), Some(5), Some(-1)] {
            assert!(
                matches!(identity_schema_decision(version), Err(ControllerError::SchemaNotReady { found, required: 4 }) if found == version)
            );
        }
    }

    #[test]
    fn test_migration_needs_all_independent_confirmations() {
        let mut config =
            TestMigrationConfig::fixture("mysql://fixture/test_identity", "test_identity");
        assert!(authorize_test_migration(&config, "test_identity").is_err());
        config.allow_destructive = true;
        assert!(authorize_test_migration(&config, "test_identity").is_err());
        config.backup_ref = "snapshot-42".into();
        config.migration_ack = "isolated-exclusive-backed-up-disposable".into();
        assert!(authorize_test_migration(&config, "production").is_err());
        assert!(authorize_test_migration(&config, "test_identity").is_ok());
    }

    #[test]
    fn explicitly_authorized_unprefixed_dev_database_is_allowed() {
        let mut config = TestMigrationConfig::fixture(
            "mysql://fixture/development_identity",
            "development_identity",
        );
        config.allow_destructive = true;
        config.backup_ref = "snapshot-42".into();
        config.migration_ack = "isolated-exclusive-backed-up-disposable".into();
        assert!(config.authorize("development_identity").is_ok());
        assert!(config.authorize("development_other").is_err());
        config.expected_database = "development-identity".into();
        assert!(config.authorize("development-identity").is_ok());
        assert!(config.authorize("development_identity").is_err());
        config.expected_database = "development_identity".into();
        for invalid in [
            TestMigrationConfig {
                allow_destructive: false,
                ..config.clone()
            },
            TestMigrationConfig {
                backup_ref: String::new(),
                ..config.clone()
            },
            TestMigrationConfig {
                migration_ack: String::new(),
                ..config.clone()
            },
            TestMigrationConfig {
                test_url: String::new(),
                ..config.clone()
            },
        ] {
            assert!(invalid.authorize("development_identity").is_err());
        }
        for invalid_name in ["", " development_identity", "développement"] {
            let invalid = TestMigrationConfig {
                expected_database: invalid_name.into(),
                ..config.clone()
            };
            assert!(invalid.authorize(invalid_name).is_err());
        }
    }

    #[test]
    fn test_migration_database_name_accepts_64_ascii_chars_but_rejects_65() {
        let name_64 = "a".repeat(64);
        let name_65 = "a".repeat(65);
        let mut config = TestMigrationConfig::fixture("mysql://fixture/dev", &name_64);
        config.allow_destructive = true;
        config.backup_ref = "snapshot-42".into();
        config.migration_ack = "isolated-exclusive-backed-up-disposable".into();
        assert!(config.authorize(&name_64).is_ok());
        config.expected_database = name_65.clone();
        assert!(config.authorize(&name_65).is_err());
    }

    #[test]
    fn test_migration_config_debug_hides_fake_password() {
        let mut config = TestMigrationConfig::fixture(
            "mysql://fixture:fake-password-for-debug@localhost/test_identity",
            "test_identity",
        );
        config.backup_ref = "fake-backup-reference".into();
        let output = format!("{config:?}");
        for sensitive in [
            "fake-password-for-debug",
            "test_identity",
            "fake-backup-reference",
        ] {
            assert!(!output.contains(sensitive));
        }
    }

    #[tokio::test]
    async fn unauthorized_command_never_invokes_upgrade() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let config = TestMigrationConfig::fixture("mysql://fixture/test_identity", "test_identity");
        let result = run_authorized_migration_with(
            &config,
            || async { Ok(((), "test_identity".into())) },
            |db, _| async move { Ok(db) },
            |_| async {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .await;
        assert!(result.is_err());
        assert_eq!(calls.get(), 0);
    }

    #[tokio::test]
    async fn every_failed_authorization_blocks_connection_or_upgrade() {
        use std::cell::Cell;
        let base = TestMigrationConfig {
            test_url: "mysql://fixture/test_identity".into(),
            allow_destructive: true,
            expected_database: "test_identity".into(),
            backup_ref: "snapshot-42".into(),
            migration_ack: "isolated-exclusive-backed-up-disposable".into(),
        };
        for config in [
            TestMigrationConfig {
                test_url: String::new(),
                ..base.clone()
            },
            TestMigrationConfig {
                allow_destructive: false,
                ..base.clone()
            },
            TestMigrationConfig {
                backup_ref: String::new(),
                ..base.clone()
            },
            TestMigrationConfig {
                migration_ack: String::new(),
                ..base.clone()
            },
            TestMigrationConfig {
                expected_database: String::new(),
                ..base.clone()
            },
        ] {
            let connected = Cell::new(0);
            let upgraded = Cell::new(0);
            assert!(
                run_authorized_migration_with(
                    &config,
                    || async {
                        connected.set(connected.get() + 1);
                        Ok(((), "test_identity".into()))
                    },
                    |db, _| async move { Ok(db) },
                    |_| async {
                        upgraded.set(upgraded.get() + 1);
                        Ok(())
                    }
                )
                .await
                .is_err()
            );
            assert_eq!((connected.get(), upgraded.get()), (0, 0));
        }
        let upgraded = Cell::new(0);
        assert!(
            run_authorized_migration_with(
                &base,
                || async { Ok(((), "test_other".into())) },
                |db, _| async move { Ok(db) },
                |_| async {
                    upgraded.set(upgraded.get() + 1);
                    Ok(())
                }
            )
            .await
            .is_err()
        );
        assert_eq!(upgraded.get(), 0);
        run_authorized_migration_with(
            &base,
            || async { Ok(((), "test_identity".into())) },
            |db, _| async move { Ok(db) },
            |_| async {
                upgraded.set(upgraded.get() + 1);
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(upgraded.get(), 1);
    }

    #[test]
    fn decode_snapshot_rejects_invalid_pairs_even_when_values_are_known() {
        for (state, decision) in [
            ("PENDING", "none"),
            ("PENDING", "denied"),
            ("APPROVED", "approved"),
            ("REVOKED", "revoked"),
        ] {
            assert_eq!(decode_snapshot(state, decision, 42).unwrap().revision, 42);
        }
        for (state, decision) in [
            ("PENDING", "approved"),
            ("APPROVED", "none"),
            ("REVOKED", "denied"),
            ("pending", "none"),
            ("PENDING", "NONE"),
        ] {
            assert!(
                decode_snapshot(state, decision, 42).is_err(),
                "{state}/{decision}"
            );
        }
    }

    #[test]
    fn audit_sequence_never_wraps() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let seq = AtomicU64::new(u64::MAX);
        assert!(next_event_seq(&seq).is_err());
        assert_eq!(seq.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn max_admission_revision_rejects_transition_without_wrap() {
        let current = AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: u64::MAX,
        };
        assert!(matches!(
            next_snapshot(current, ReviewDecision::Approved),
            Err(ControllerError::RevisionConflict)
        ));
        assert_eq!(current.revision, u64::MAX);
    }

    #[test]
    fn admission_revision_crosses_signed_boundary() {
        let current = AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: i64::MAX as _,
        };
        let next = next_snapshot(current, ReviewDecision::Approved).unwrap();
        assert_eq!(next.revision, i64::MAX as u64 + 1);
    }

    #[test]
    fn admission_decision_rejects_illegal_transition() {
        let snapshot = AdmissionSnapshot {
            admission_state: AdmissionState::Revoked,
            review_decision: ReviewDecision::Revoked,
            revision: 2,
        };
        assert!(next_snapshot(snapshot, ReviewDecision::Denied).is_err());
        assert_eq!(
            next_snapshot(snapshot, ReviewDecision::Approved)
                .unwrap()
                .revision,
            3
        );
    }

    #[test]
    fn denied_device_requires_reopen_before_approval() {
        let snapshot = AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::Denied,
            revision: 4,
        };
        assert!(matches!(
            next_snapshot(snapshot, ReviewDecision::Approved),
            Err(ControllerError::RevisionConflict)
        ));
    }

    #[test]
    fn admission_audit_codes_are_stable_and_neutral() {
        assert_eq!(
            audit_code(ReviewDecision::Approved, AdmissionState::Pending),
            "admission.approve"
        );
        assert_eq!(
            audit_code(ReviewDecision::Approved, AdmissionState::Revoked),
            "admission.reauthorize"
        );
        assert_eq!(
            audit_code(ReviewDecision::Denied, AdmissionState::Pending),
            "admission.reject"
        );
        assert_eq!(
            audit_code(ReviewDecision::None, AdmissionState::Pending),
            "admission.reopen"
        );
        assert_eq!(
            audit_code(ReviewDecision::Revoked, AdmissionState::Approved),
            "admission.revoke"
        );
    }

    #[test]
    fn fallback_evidence_has_stable_reference_and_unknown_fields() {
        let a: serde_json::Value = serde_json::from_str(&system_fallback_time_evidence()).unwrap();
        let b: serde_json::Value = serde_json::from_str(&system_fallback_time_evidence()).unwrap();
        assert_eq!(a["quality"], "system_fallback");
        let wall = a["system_wall_utc"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(wall).unwrap();
        assert_eq!(a["reference_utc"], wall);
        let epoch = uuid::Uuid::parse_str(a["clock_epoch"].as_str().unwrap()).unwrap();
        assert_eq!(epoch.get_version_num(), 4);
        assert_eq!(a["clock_epoch"], b["clock_epoch"]);
        for key in ["source", "sample_age_ms", "offset_ms", "uncertainty_ms"] {
            assert!(a.get(key).unwrap().is_null(), "{key} must be unknown");
        }
    }

    fn full_presence() -> RuntimeTablePresence {
        RuntimeTablePresence {
            main_tasks: true,
            sub_tasks: true,
            task_previews: true,
            device_operation_locks: true,
            recovery_checks: true,
        }
    }
}

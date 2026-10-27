use crate::{AdmissionSnapshot, AdmissionState, ControllerError, ReviewDecision};
use sqlx::Row;

mod fresh;
mod pool;
mod probe;
mod schema_parse;
mod schema_shape;
mod username;

pub use pool::DbPool;
pub use probe::RuntimeTablePresence;
pub(crate) use probe::require_no_identity_foreign_keys;
pub use schema_parse::v4_schema_statements;
pub(crate) use username::valid_username;

use fresh::*;
use probe::*;

/// v4 唯一基线 DDL（`schema/v4.sql`，恰好 16 张表、无 FOREIGN KEY、无 CHECK）。
///
/// 数据库 schema 只有 v4 一个基线：空库经显式受控入口直接建完整 v4，已是 v4 只做只读
/// 校验，其余任何版本或半成品状态一律拒绝——仓库中不再有 0001..0004 历史执行链，也没有
/// 「先建旧版再 ALTER」的升级路径。本常量对外可见只用于离线契约测试；运行期绝不把整段
/// SQL 当作一条语句执行，所有写入都用 [`v4_schema_statements`] 的白名单解析结果。
pub const V4_SCHEMA_SQL: &str = include_str!("../schema/v4.sql");

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
}

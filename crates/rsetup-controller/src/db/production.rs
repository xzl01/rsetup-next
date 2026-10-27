use crate::ControllerError;

use super::fresh::create_v4_schema;
use super::migration::{
    TestMigrationConfig, authorize_test_migration, require_schema_metadata_privilege,
    run_authorized_migration_with,
};
use super::pool::DbPool;
use super::probe::{check_identity_schema_with_probe, read_version};

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

#[cfg(test)]
mod tests {
    use super::*;

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
}

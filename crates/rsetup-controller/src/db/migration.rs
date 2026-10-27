use crate::ControllerError;
use sqlx::Row;

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

pub(super) fn authorize_test_migration(
    c: &TestMigrationConfig,
    actual: &str,
) -> Result<(), ControllerError> {
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

pub(super) async fn require_schema_metadata_privilege(
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

pub(super) async fn run_authorized_migration_with<D, E, C, CF, G, GF, U, UF>(
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

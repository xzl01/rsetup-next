use rsetup_controller::{ControllerConfig, DbPool, TestMigrationConfig};
use std::sync::Mutex;

pub async fn required_test_db() -> DbPool {
    let config = TestMigrationConfig::from_test_env()
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    config
        .authorize(&config.expected_database)
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    let db = DbPool::connect(&ControllerConfig {
        database_url: config.test_url.clone(),
        listen_address: String::new(),
    })
    .await
    .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    let name: String = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    config
        .authorize(&name)
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    db
}

pub async fn required_fresh_identity_db() -> DbPool {
    let db = required_test_db().await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_one(&db.0)
    .await
    .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(
        count, 0,
        "fresh disposable test schema required; never reset existing schema"
    );
    db
}

pub async fn assert_fresh_v3_is_check_and_fk_free(db: &DbPool) {
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(version, 3);
    let initialized: bool =
        sqlx::query_scalar("SELECT initialized FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert!(!initialized);
    let meta_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_meta")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(meta_rows, 1);
    let constraints: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type IN ('CHECK', 'FOREIGN KEY')")
        .fetch_one(&db.0).await.unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(constraints, 0);
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_one(&db.0)
    .await
    .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(tables, 11);
    rsetup_controller::check_identity_schema(db)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
}

pub async fn assert_actor_fixture_v3_without_check_or_fk(db: &DbPool) {
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(version, 3, "actor fixture must be on identity schema v3");
    let constraints: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type IN ('CHECK', 'FOREIGN KEY')")
        .fetch_one(&db.0).await.unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(constraints, 0, "actor fixture must have no CHECK or FK");
    rsetup_controller::check_identity_schema(db)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
}

#[derive(Clone, Copy, Debug)]
pub enum ExpectedIdentityEngine {
    MySql,
    TiDb,
}

fn version_matches_expected_engine(version: &str, expected: ExpectedIdentityEngine) -> bool {
    match expected {
        ExpectedIdentityEngine::MySql => {
            let lower = version.to_ascii_lowercase();
            version
                .strip_prefix("8.")
                .is_some_and(|tail| tail.starts_with(|c: char| c.is_ascii_digit()))
                && !lower.contains("tidb")
                && !lower.contains("mariadb")
        }
        ExpectedIdentityEngine::TiDb => version.contains("-TiDB-v"),
    }
}

#[test]
fn version_gate_rejects_wrong_engine_without_live_database() {
    use ExpectedIdentityEngine::{MySql, TiDb};
    for (version, expected) in [
        ("8.0.39", MySql),
        ("8.4.2-commercial", MySql),
        ("5.7.25-TiDB-v7.5.0", TiDb),
        ("8.0.11-TiDB-v8.5.0", TiDb),
    ] {
        assert!(version_matches_expected_engine(version, expected));
    }
    for (version, expected) in [
        ("5.7.25-TiDB-v7.5.0", MySql),
        ("8.0.11-TiDB-v8.5.0", MySql),
        ("10.11.6-MariaDB", MySql),
        ("5.7.44", MySql),
        ("9.0.0", MySql),
        ("8.0.39", TiDb),
        ("10.11.6-MariaDB", TiDb),
        ("", MySql),
        ("", TiDb),
    ] {
        assert!(!version_matches_expected_engine(version, expected));
    }
}

pub async fn assert_legacy_fixture_upgrades_to_v3(mode: &str, expected: ExpectedIdentityEngine) {
    assert!(matches!(mode, "fixture-v1" | "fixture-v2" | "mixed-v1"));
    let db = required_fresh_identity_db().await;
    let version: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(&db.0)
        .await
        .expect("read target engine identity before fixture DDL");
    assert!(
        version_matches_expected_engine(&version, expected),
        "fixture target engine does not match expected identity"
    );
    run_explicit_identity_test_command(if mode == "mixed-v1" {
        "fixture-v1"
    } else {
        mode
    });
    if mode == "mixed-v1" {
        run_explicit_identity_test_command("fixture-partial-v1");
    }
    let original_version = if mode == "fixture-v2" { 2 } else { 1 };
    let id: Vec<u8> = sqlx::query_scalar("SELECT instance_id FROM schema_meta WHERE singleton=1")
        .fetch_one(&db.0)
        .await
        .unwrap();
    let user_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO users (id,username,display_name,password_hash,active,is_admin,must_change_password,revision,created_time) VALUES (?, 'legacy_fixture', 'Fixture', 'fixture-placeholder', TRUE, TRUE, FALSE, 7, NOW(6))")
        .bind(user_id.as_bytes().as_slice()).execute(&db.0).await.unwrap();
    sqlx::query("UPDATE schema_meta SET initialized=TRUE WHERE singleton=1")
        .execute(&db.0)
        .await
        .unwrap();
    assert!(matches!(
        rsetup_controller::check_identity_schema(&db).await,
        Err(rsetup_controller::ControllerError::SchemaNotReady { found: Some(found), required: 3 }) if found == original_version
    ));
    run_explicit_identity_test_command("upgrade");
    rsetup_controller::check_identity_schema(&db).await.unwrap();
    let meta: (i32, Vec<u8>, bool) = sqlx::query_as(
        "SELECT schema_version,instance_id,initialized FROM schema_meta WHERE singleton=1",
    )
    .fetch_one(&db.0)
    .await
    .unwrap();
    assert_eq!(meta.0, 3);
    assert_eq!(meta.1, id);
    assert!(meta.2);
    let user: (String, u64) = sqlx::query_as("SELECT password_hash,revision FROM users WHERE id=?")
        .bind(user_id.as_bytes().as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(user, ("fixture-placeholder".into(), 7));
    let constraints: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema=DATABASE() AND constraint_type IN ('CHECK','FOREIGN KEY')")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(constraints, 0);
    run_explicit_identity_test_command("upgrade");
    let retried: (i32, Vec<u8>, bool) = sqlx::query_as(
        "SELECT schema_version,instance_id,initialized FROM schema_meta WHERE singleton=1",
    )
    .fetch_one(&db.0)
    .await
    .unwrap();
    assert_eq!(retried, meta);
}

pub async fn required_prepared_identity_db() -> DbPool {
    let db = required_test_db().await;
    rsetup_controller::check_identity_schema(&db)
        .await
        .expect("existing v3 identity schema");
    db
}

pub fn run_explicit_identity_test_command(mode: &str) {
    assert!(matches!(
        mode,
        "upgrade" | "fixture-v1" | "fixture-partial-v1" | "fixture-v2"
    ));
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
        .args(["--mode", mode])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap_or_else(|_| panic!("task4_migration_process_failed"));
    assert!(
        status.success(),
        "explicit test migration failed; inspect disposable schema manually"
    );
}

#[test]
fn fixture_authorization_requires_all_confirmations() {
    let base = TestMigrationConfig {
        test_url: "mysql://fixture/test_identity".into(),
        allow_destructive: true,
        expected_database: "test_identity".into(),
        backup_ref: "snapshot-42".into(),
        migration_ack: "isolated-exclusive-backed-up-disposable".into(),
    };
    assert!(base.authorize("test_identity").is_ok());
    for c in [
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
            expected_database: "production".into(),
            ..base.clone()
        },
        TestMigrationConfig {
            test_url: String::new(),
            ..base.clone()
        },
    ] {
        assert!(c.authorize("test_identity").is_err());
    }
    assert!(base.authorize("test_not_expected").is_err());
}

// Each caller must provide a different authorized, backed-up, empty database. Never
// run the entire ignored matrix against one URL; each invocation consumes its fixture.
const NEGATIVE_FIXTURES: [(&str, &str, &str, &str); 10] = [
    (
        "schema_meta",
        "authz_epoch",
        "UPDATE schema_meta SET authz_epoch = -1 WHERE singleton = 1",
        "SELECT authz_epoch FROM schema_meta WHERE singleton = 1",
    ),
    (
        "schema_meta",
        "admin_guard_revision",
        "UPDATE schema_meta SET admin_guard_revision = -1 WHERE singleton = 1",
        "SELECT admin_guard_revision FROM schema_meta WHERE singleton = 1",
    ),
    (
        "users",
        "revision",
        "INSERT INTO users (id,username,display_name,password_hash,active,is_admin,must_change_password,revision,created_time) VALUES (UNHEX(REPLACE(UUID(),'-','')),'alice','fixture','hash',TRUE,FALSE,FALSE,-1,NOW(6))",
        "SELECT revision FROM users",
    ),
    (
        "roles",
        "revision",
        "INSERT INTO roles (id,name,builtin,archived,revision) VALUES (UNHEX(REPLACE(UUID(),'-','')),'fixture',FALSE,FALSE,-1)",
        "SELECT revision FROM roles",
    ),
    (
        "device_groups",
        "revision",
        "INSERT INTO device_groups (id,name,archived,revision) VALUES (UNHEX(REPLACE(UUID(),'-','')),'fixture',FALSE,-1)",
        "SELECT revision FROM device_groups",
    ),
    (
        "devices",
        "revision",
        "INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (REPEAT('x',32),'fixture','PENDING','none',-1,FALSE)",
        "SELECT revision FROM devices",
    ),
    (
        "grants",
        "revision",
        "INSERT INTO grants (id,user_id,source_kind,permissions,scope_kind,revision) VALUES (UNHEX(REPLACE(UUID(),'-','')),?,'direct','[\"device.read\"]','all',-1)",
        "SELECT revision FROM grants",
    ),
    (
        "admission_decisions",
        "previous_revision",
        "INSERT INTO admission_decisions (id,device_id,decision,previous_revision,new_revision,time_evidence) VALUES (UNHEX(REPLACE(UUID(),'-','')),?,'approved',-1,0,'{}')",
        "SELECT previous_revision FROM admission_decisions",
    ),
    (
        "admission_decisions",
        "new_revision",
        "INSERT INTO admission_decisions (id,device_id,decision,previous_revision,new_revision,time_evidence) VALUES (UNHEX(REPLACE(UUID(),'-','')),?,'approved',0,-1,'{}')",
        "SELECT new_revision FROM admission_decisions",
    ),
    (
        "audit_events",
        "event_seq",
        "INSERT INTO audit_events (id,actor_kind,event_type,params_redacted,outcome,time_evidence,process_epoch,event_seq) VALUES (UNHEX(REPLACE(UUID(),'-','')),'system','fixture','{}','success','{}',UNHEX(REPLACE(UUID(),'-','')),-1)",
        "SELECT event_seq FROM audit_events",
    ),
];

// Every negative child references a real parent; all other row fields remain valid.
const NEGATIVE_PARENT_SEEDS: [Option<&str>; 10] = [
    None,
    None,
    None,
    None,
    None,
    None,
    Some(
        "INSERT INTO users (id,username,display_name,password_hash,active,is_admin,must_change_password,revision,created_time) VALUES (?,'negative_parent','Fixture','fixture-placeholder',TRUE,FALSE,FALSE,0,NOW(6))",
    ),
    Some(
        "INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (?,'negative parent','PENDING','none',0,FALSE)",
    ),
    Some(
        "INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (?,'negative parent','PENDING','none',0,FALSE)",
    ),
    None,
];

#[test]
fn negative_revision_fixtures_have_valid_existing_parents_and_grant_permissions() {
    assert_eq!(NEGATIVE_PARENT_SEEDS.len(), NEGATIVE_FIXTURES.len());
    for (index, table) in [(6, "users"), (7, "devices"), (8, "devices")] {
        let seed = NEGATIVE_PARENT_SEEDS[index].expect("negative child requires a parent");
        assert!(seed.starts_with(&format!("INSERT INTO {table} ")));
        assert!(seed.contains('?'), "parent id must be bound");
        assert!(
            NEGATIVE_FIXTURES[index].2.contains('?'),
            "child must bind the same id"
        );
    }
    assert!(
        NEGATIVE_PARENT_SEEDS
            .iter()
            .enumerate()
            .all(|(i, seed)| (6..=8).contains(&i) || seed.is_none())
    );
    let grant = NEGATIVE_FIXTURES[6].2;
    assert!(
        grant.contains("'[\"device.read\"]'"),
        "direct grant needs valid permission JSON"
    );
    assert!(!grant.contains("'{}'"));
    for (_, _, inject, _) in NEGATIVE_FIXTURES.iter().take(9).skip(6) {
        assert!(inject.contains("-1"));
    }
}

pub async fn negative_fixture_prevents_all_alters(index: usize) {
    let db = required_fresh_identity_db().await;
    run_explicit_identity_test_command("fixture-v1");
    let (table, column, inject, read_value) = NEGATIVE_FIXTURES[index];
    if let Some(parent_sql) = NEGATIVE_PARENT_SEEDS[index] {
        let parent_id = if index == 6 {
            uuid::Uuid::new_v4().as_bytes().to_vec()
        } else {
            unique_public_key().to_vec()
        };
        sqlx::query(parent_sql)
            .bind(parent_id.as_slice())
            .execute(&db.0)
            .await
            .unwrap();
        sqlx::query(inject)
            .bind(parent_id.as_slice())
            .execute(&db.0)
            .await
            .unwrap();
    } else {
        sqlx::query(inject).execute(&db.0).await.unwrap();
    }
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
        .args(["--mode", "upgrade"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "negative {table}.{column} must refuse migration"
    );
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    assert!(
        diagnostic.contains(&format!("negative identity column {table}.{column}")),
        "migration must reject injected {table}.{column}, not another error"
    );
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(version, 1);
    let value: i64 = sqlx::query_scalar(read_value)
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(value, -1, "original {table}.{column} must survive");
    for &(name, field, _, _) in &NEGATIVE_FIXTURES {
        let shape: String = sqlx::query_scalar("SELECT CAST(column_type AS CHAR) FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?")
            .bind(name).bind(field).fetch_one(&db.0).await.unwrap();
        assert_eq!(
            shape.to_ascii_lowercase(),
            "bigint",
            "ALTER occurred before preflight: {name}.{field}"
        );
    }
}

pub async fn later_column_interruption_is_resumable() {
    let db = required_fresh_identity_db().await;
    run_explicit_identity_test_command("fixture-v1");
    // Stop after the second exact 0002 ALTER in this authorized disposable DB.
    for statement in [
        "ALTER TABLE schema_meta MODIFY COLUMN authz_epoch BIGINT UNSIGNED NOT NULL DEFAULT 0",
        "ALTER TABLE schema_meta MODIFY COLUMN admin_guard_revision BIGINT UNSIGNED NOT NULL DEFAULT 0",
    ] {
        sqlx::query(statement).execute(&db.0).await.unwrap();
    }
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(version, 1);
    assert!(matches!(
        rsetup_controller::check_identity_schema(&db).await,
        Err(rsetup_controller::ControllerError::SchemaNotReady {
            found: Some(1),
            required: 3
        })
    ));
    run_explicit_identity_test_command("upgrade");
    rsetup_controller::check_identity_schema(&db).await.unwrap();
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(version, 3);
}

#[derive(Default)]
pub struct RecordingSecretSink(Mutex<Vec<String>>);
impl rsetup_controller::BootstrapSecretSink for RecordingSecretSink {
    fn emit(
        &self,
        _username: &str,
        password: &str,
    ) -> Result<(), rsetup_controller::ControllerError> {
        self.0.lock().unwrap().push(password.to_owned());
        Ok(())
    }
}
impl RecordingSecretSink {
    pub fn emissions(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

pub async fn user_hash(db: &DbPool, username: &str) -> String {
    sqlx::query_scalar("SELECT password_hash FROM users WHERE username = ?")
        .bind(username)
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"))
}

pub struct FailingSecretSink(Mutex<usize>);
impl FailingSecretSink {
    pub fn attempts(&self) -> usize {
        *self.0.lock().unwrap()
    }
}
impl Default for FailingSecretSink {
    fn default() -> Self {
        Self(Mutex::new(0))
    }
}
impl rsetup_controller::BootstrapSecretSink for FailingSecretSink {
    fn emit(
        &self,
        _username: &str,
        _password: &str,
    ) -> Result<(), rsetup_controller::ControllerError> {
        *self.0.lock().unwrap() += 1;
        Err(rsetup_controller::ControllerError::Config(
            "private secret write failed".into(),
        ))
    }
}

pub fn unique_public_key() -> [u8; 32] {
    use rand::RngCore;
    let mut key = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut key);
    key
}

pub async fn counts(db: &DbPool, public_key: &[u8; 32]) -> (i64, i64) {
    let history: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM admission_decisions WHERE device_id = ?")
            .bind(public_key.as_slice())
            .fetch_one(&db.0)
            .await
            .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    let audit: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE target_id = ?")
        .bind(hex::encode(public_key))
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    (history, audit)
}

fn assert_max_snapshot_unchanged(
    before: rsetup_controller::AdmissionSnapshot,
    after: rsetup_controller::AdmissionSnapshot,
) {
    assert_eq!(after, before, "MAX CAS must not change the snapshot");
}

#[test]
fn max_snapshot_assertion_detects_state_change_at_same_revision() {
    use rsetup_controller::{AdmissionSnapshot, AdmissionState, ReviewDecision};
    let before = AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::Approved,
        revision: u64::MAX,
    };
    let after = AdmissionSnapshot {
        admission_state: AdmissionState::Approved,
        ..before
    };
    assert!(
        std::panic::catch_unwind(|| assert_max_snapshot_unchanged(before, after)).is_err(),
        "MAX snapshot assertion must reject state changes with unchanged revision"
    );
}

#[test]
fn max_snapshot_assertion_detects_decision_change_at_same_revision() {
    use rsetup_controller::{AdmissionSnapshot, AdmissionState, ReviewDecision};
    let before = AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::Approved,
        revision: u64::MAX,
    };
    let after = AdmissionSnapshot {
        review_decision: ReviewDecision::Revoked,
        ..before
    };
    assert!(
        std::panic::catch_unwind(|| assert_max_snapshot_unchanged(before, after)).is_err(),
        "MAX snapshot assertion must reject decision changes with unchanged revision"
    );
}

fn assert_missing_actor_not_found(
    result: Result<rsetup_controller::AdmissionSnapshot, rsetup_controller::ControllerError>,
) {
    match result {
        Ok(_) => panic!("actor_red_unexpected_acceptance"),
        Err(rsetup_controller::ControllerError::NotFound) => (),
        Err(_) => panic!("actor_red_unexpected_error"),
    }
}

fn assert_missing_actor_panic_category(
    result: Result<rsetup_controller::AdmissionSnapshot, rsetup_controller::ControllerError>,
    expected: &str,
) {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_missing_actor_not_found(result);
    }));
    let payload = match caught {
        Ok(()) => panic!("actor_assertion_expected_panic"),
        Err(payload) => payload,
    };
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str));
    assert!(
        message == Some(expected),
        "actor_assertion_panic_category_mismatch"
    );
}

#[test]
fn missing_actor_assertion_accepts_not_found_offline() {
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_missing_actor_not_found(Err(rsetup_controller::ControllerError::NotFound));
    }));
    assert!(caught.is_ok(), "actor_assertion_rejected_not_found");
}

#[test]
fn missing_actor_assertion_classifies_acceptance_offline() {
    use rsetup_controller::{AdmissionSnapshot, AdmissionState, ReviewDecision};
    assert_missing_actor_panic_category(
        Ok(AdmissionSnapshot {
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::Approved,
            revision: 2,
        }),
        "actor_red_unexpected_acceptance",
    );
}

#[test]
fn missing_actor_assertion_classifies_config_error_offline() {
    assert_missing_actor_panic_category(
        Err(rsetup_controller::ControllerError::Config(String::new())),
        "actor_red_unexpected_error",
    );
}

#[test]
fn missing_actor_assertion_classifies_database_error_offline() {
    assert_missing_actor_panic_category(
        Err(rsetup_controller::ControllerError::Database(
            sqlx::Error::ColumnNotFound(String::new()),
        )),
        "actor_red_unexpected_error",
    );
}

#[test]
fn missing_actor_assertion_classifies_revision_conflict_offline() {
    assert_missing_actor_panic_category(
        Err(rsetup_controller::ControllerError::RevisionConflict),
        "actor_red_unexpected_error",
    );
}

pub async fn admission_missing_actor_is_atomic(db: &DbPool) {
    use rsetup_controller::{AdmissionSnapshot, AdmissionState, AdmissionStore, ReviewDecision};

    let public_key = unique_public_key();
    sqlx::query("INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (?,'actor-test','PENDING','none',1,FALSE)")
        .bind(public_key.as_slice())
        .execute(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    let before = fixture_ok(db.load(public_key).await);
    assert_eq!(
        before,
        AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: 1,
        },
        "actor fixture requires a valid PENDING/none device at revision 1"
    );
    let prior = counts(db, &public_key).await;
    assert_eq!(
        prior,
        (0, 0),
        "actor fixture must start without history or audit"
    );
    let missing_actor = *uuid::Uuid::new_v4().as_bytes();
    let actor_rows: i64 = fixture_ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = ?")
            .bind(missing_actor.as_slice())
            .fetch_one(&db.0)
            .await,
    );
    assert_eq!(actor_rows, 0, "the actor must not exist in users");
    let result = db
        .compare_and_set(
            public_key,
            1,
            AdmissionState::Pending,
            ReviewDecision::Approved,
            Some(missing_actor),
            None,
        )
        .await;
    assert_missing_actor_not_found(result);
    assert_eq!(
        fixture_ok(db.load(public_key).await),
        before,
        "missing actor must leave the admission snapshot unchanged"
    );
    assert_eq!(
        counts(db, &public_key).await,
        prior,
        "missing actor must not add admission history or audit events"
    );
}

// Never expose a driver error, row, password hash or generated secret in fixture failures.
fn fixture_ok<T, E>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("task4_fixture_operation_failed"),
    }
}

#[test]
fn fixture_failure_cannot_be_business_success_offline() {
    assert_eq!(fixture_ok::<_, ()>(Ok(7)), 7);
    let caught = std::panic::catch_unwind(|| fixture_ok::<(), _>(Err("private fixture failure")));
    let payload = caught.expect_err("task4_fixture_failure_was_accepted");
    assert!(
        payload.downcast_ref::<&str>() == Some(&"task4_fixture_operation_failed"),
        "task4_fixture_failure_not_redacted"
    );
}

#[derive(Clone, Copy)]
enum AdmissionFailure {
    Permission,
    StoredCombination,
    ReadinessCombination,
}

fn assert_admission_failure<T>(
    result: Result<T, rsetup_controller::ControllerError>,
    expected: AdmissionFailure,
) {
    use rsetup_controller::ControllerError;
    let matches = match (result, expected) {
        (Err(ControllerError::PermissionDenied), AdmissionFailure::Permission) => true,
        (Err(ControllerError::Config(message)), AdmissionFailure::StoredCombination) => {
            message == "invalid stored admission state/decision combination"
        }
        (Err(ControllerError::Config(message)), AdmissionFailure::ReadinessCombination) => {
            message == "identity data devices.state"
        }
        _ => false,
    };
    assert!(matches, "task4_unexpected_admission_result");
}

#[test]
fn admission_failure_assertion_rejects_wrong_causes_offline() {
    use rsetup_controller::ControllerError;
    for expected in [
        AdmissionFailure::Permission,
        AdmissionFailure::StoredCombination,
        AdmissionFailure::ReadinessCombination,
    ] {
        for result in [
            Ok(()),
            Err(ControllerError::NotFound),
            Err(ControllerError::RevisionConflict),
            Err(ControllerError::Config("unrelated".into())),
            Err(ControllerError::Database(sqlx::Error::PoolClosed)),
            Err(ControllerError::PermissionDenied),
            Err(ControllerError::Config(
                "invalid stored admission state/decision combination".into(),
            )),
            Err(ControllerError::Config(
                "identity data devices.state".into(),
            )),
        ] {
            let should_pass = matches!(
                (&result, expected),
                (
                    Err(ControllerError::PermissionDenied),
                    AdmissionFailure::Permission
                )
            ) || matches!(
                (&result, expected),
                (Err(ControllerError::Config(message)), AdmissionFailure::StoredCombination)
                    if message == "invalid stored admission state/decision combination"
            ) || matches!(
                (&result, expected),
                (Err(ControllerError::Config(message)), AdmissionFailure::ReadinessCombination)
                    if message == "identity data devices.state"
            );
            let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                assert_admission_failure(result, expected);
            }));
            assert!(
                caught.is_ok() == should_pass,
                "task4_error_classifier_mismatch"
            );
        }
    }
}

// Compare complete persisted event rows in process; never format their contents.
async fn task4_event_rows(db: &DbPool) -> (Vec<String>, Vec<String>) {
    let history = fixture_ok(sqlx::query_scalar("SELECT CAST(JSON_ARRAY(HEX(id),HEX(device_id),HEX(actor_id),decision,previous_revision,new_revision,reason,time_evidence) AS CHAR) AS row_value FROM admission_decisions ORDER BY id").fetch_all(&db.0).await);
    let audit = fixture_ok(sqlx::query_scalar("SELECT CAST(JSON_ARRAY(HEX(id),actor_kind,HEX(actor_user_id),event_type,target_kind,target_id,params_redacted,outcome,time_evidence,HEX(process_epoch),event_seq) AS CHAR) AS row_value FROM audit_events ORDER BY id").fetch_all(&db.0).await);
    (history, audit)
}

async fn task4_device(db: &DbPool) -> [u8; 32] {
    let key = unique_public_key();
    fixture_ok(sqlx::query("INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (?,'task4 fixture','PENDING','none',1,FALSE)")
        .bind(key.as_slice()).execute(&db.0).await);
    key
}

async fn assert_admission_actor(db: &DbPool, key: &[u8; 32], actor: Option<[u8; 16]>) {
    let history: (Option<Vec<u8>>, String, u64, u64) = fixture_ok(sqlx::query_as(
        "SELECT actor_id,CAST(decision AS CHAR) AS decision,previous_revision,new_revision FROM admission_decisions WHERE device_id=?",
    ).bind(key.as_slice()).fetch_one(&db.0).await);
    let audit: (String, Option<Vec<u8>>, String, String, String) = fixture_ok(sqlx::query_as(
        "SELECT CAST(actor_kind AS CHAR) AS actor_kind,actor_user_id,CAST(event_type AS CHAR) AS event_type,CAST(target_kind AS CHAR) AS target_kind,CAST(outcome AS CHAR) AS outcome FROM audit_events WHERE target_id=?",
    ).bind(hex::encode(key)).fetch_one(&db.0).await);
    let expected_actor = actor.map(|id| id.to_vec());
    assert!(history.0 == expected_actor, "task4_history_actor_mismatch");
    assert!(
        history.1 == "approved" && history.2 == 1 && history.3 == 2,
        "task4_history_transition_mismatch"
    );
    assert!(
        audit.0 == if actor.is_some() { "user" } else { "system" },
        "task4_audit_actor_kind_mismatch"
    );
    assert!(audit.1 == expected_actor, "task4_audit_actor_id_mismatch");
    assert!(
        audit.2 == "admission.approve" && audit.3 == "device" && audit.4 == "success",
        "task4_audit_event_mismatch"
    );
    assert_eq!(counts(db, key).await, (1, 1), "task4_actor_event_counts");
}

// Removing any individual actor guard must make its otherwise-valid case fail.
pub async fn admission_actor_permissions_are_atomic(db: &DbPool) {
    use rsetup_controller::{AdmissionSnapshot, AdmissionState, AdmissionStore, ReviewDecision};
    for (name, active, admin, change) in [
        ("inactive_actor", false, true, false),
        ("nonadmin_actor", true, false, false),
        ("password_actor", true, true, true),
        ("valid_actor", true, true, false),
    ] {
        let actor = *uuid::Uuid::new_v4().as_bytes();
        fixture_ok(sqlx::query("INSERT INTO users (id,username,display_name,password_hash,active,is_admin,must_change_password,revision,created_time) VALUES (?,?,'task4 fixture','fixture-placeholder',?,?,?,0,NOW(6))")
            .bind(actor.as_slice()).bind(name).bind(active).bind(admin).bind(change).execute(&db.0).await);
        let key = task4_device(db).await;
        fixture_ok(rsetup_controller::check_identity_schema(db).await);
        let before = fixture_ok(db.load(key).await);
        assert!(
            before
                == AdmissionSnapshot {
                    admission_state: AdmissionState::Pending,
                    review_decision: ReviewDecision::None,
                    revision: 1
                },
            "task4_actor_initial_snapshot"
        );
        assert_eq!(counts(db, &key).await, (0, 0), "task4_actor_initial_counts");
        let prior_events = task4_event_rows(db).await;
        let result = db
            .compare_and_set(
                key,
                1,
                AdmissionState::Pending,
                ReviewDecision::Approved,
                Some(actor),
                None,
            )
            .await;
        if active && admin && !change {
            let after = fixture_ok(result);
            assert!(
                after
                    == AdmissionSnapshot {
                        admission_state: AdmissionState::Approved,
                        review_decision: ReviewDecision::Approved,
                        revision: 2
                    },
                "task4_admin_cas_snapshot"
            );
            assert!(
                fixture_ok(db.load(key).await) == after,
                "task4_admin_persisted_snapshot"
            );
            assert_admission_actor(db, &key, Some(actor)).await;
        } else {
            assert_admission_failure(result, AdmissionFailure::Permission);
            assert!(
                fixture_ok(db.load(key).await) == before,
                "task4_denied_snapshot_changed"
            );
            assert_eq!(
                counts(db, &key).await,
                (0, 0),
                "task4_denied_events_changed"
            );
            assert!(
                task4_event_rows(db).await == prior_events,
                "task4_denied_event_rows_changed"
            );
        }
    }
    // None remains the trusted internal system path, not a missing user identity.
    let key = task4_device(db).await;
    fixture_ok(rsetup_controller::check_identity_schema(db).await);
    let after = fixture_ok(
        db.compare_and_set(
            key,
            1,
            AdmissionState::Pending,
            ReviewDecision::Approved,
            None,
            None,
        )
        .await,
    );
    assert!(
        after
            == AdmissionSnapshot {
                admission_state: AdmissionState::Approved,
                review_decision: ReviewDecision::Approved,
                revision: 2
            },
        "task4_system_cas_snapshot"
    );
    assert!(
        fixture_ok(db.load(key).await) == after,
        "task4_system_persisted_snapshot"
    );
    assert_admission_actor(db, &key, None).await;
    fixture_ok(rsetup_controller::check_identity_schema(db).await);
}

// Fixed casts avoid depending on AdmissionStore decoding to observe corrupt rows.
async fn raw_admission(db: &DbPool, key: &[u8; 32]) -> (String, String, u64, i64) {
    fixture_ok(sqlx::query_as("SELECT CAST(admission_state AS CHAR) AS admission_state,CAST(review_decision AS CHAR) AS review_decision,revision,CAST(archived AS SIGNED) AS archived FROM devices WHERE public_key=?")
        .bind(key.as_slice()).fetch_one(&db.0).await)
}

pub async fn admission_polluted_combinations_are_readonly_rejected(db: &DbPool) {
    use rsetup_controller::{AdmissionState, AdmissionStore, ReviewDecision};
    // All eight invalid tuples whose two enum values are individually legal.
    for (state, decision, expected_state) in [
        ("PENDING", "approved", AdmissionState::Pending),
        ("PENDING", "revoked", AdmissionState::Pending),
        ("APPROVED", "none", AdmissionState::Approved),
        ("APPROVED", "denied", AdmissionState::Approved),
        ("APPROVED", "revoked", AdmissionState::Approved),
        ("REVOKED", "none", AdmissionState::Revoked),
        ("REVOKED", "approved", AdmissionState::Revoked),
        ("REVOKED", "denied", AdmissionState::Revoked),
    ] {
        let key = task4_device(db).await;
        fixture_ok(rsetup_controller::check_identity_schema(db).await);
        let legal = fixture_ok(db.load(key).await);
        assert!(
            legal.admission_state == AdmissionState::Pending
                && legal.review_decision == ReviewDecision::None
                && legal.revision == 1,
            "task4_pollution_initial_snapshot"
        );
        assert_eq!(
            counts(db, &key).await,
            (0, 0),
            "task4_pollution_initial_counts"
        );
        let changed = fixture_ok(
            sqlx::query(
                "UPDATE devices SET admission_state=?,review_decision=? WHERE public_key=?",
            )
            .bind(state)
            .bind(decision)
            .bind(key.as_slice())
            .execute(&db.0)
            .await,
        )
        .rows_affected();
        assert_eq!(changed, 1, "task4_pollution_injection_count");
        let before = raw_admission(db, &key).await;
        assert!(
            before == (state.into(), decision.into(), 1, 0),
            "task4_pollution_injection_mismatch"
        );
        let prior_events = task4_event_rows(db).await;
        assert_admission_failure(db.load(key).await, AdmissionFailure::StoredCombination);
        assert!(
            raw_admission(db, &key).await == before,
            "task4_load_repaired_pollution"
        );
        assert_eq!(counts(db, &key).await, (0, 0), "task4_load_added_events");
        assert!(
            task4_event_rows(db).await == prior_events,
            "task4_load_changed_event_rows"
        );
        assert_admission_failure(
            db.compare_and_set(key, 1, expected_state, ReviewDecision::Approved, None, None)
                .await,
            AdmissionFailure::StoredCombination,
        );
        assert!(
            raw_admission(db, &key).await == before,
            "task4_cas_repaired_pollution"
        );
        assert_eq!(
            counts(db, &key).await,
            (0, 0),
            "task4_polluted_cas_added_events"
        );
        assert!(
            task4_event_rows(db).await == prior_events,
            "task4_cas_changed_event_rows"
        );
        assert_admission_failure(
            rsetup_controller::check_identity_schema(db).await,
            AdmissionFailure::ReadinessCombination,
        );
        assert!(
            raw_admission(db, &key).await == before,
            "task4_readiness_repaired_pollution"
        );
        assert_eq!(
            counts(db, &key).await,
            (0, 0),
            "task4_readiness_added_events"
        );
        assert!(
            task4_event_rows(db).await == prior_events,
            "task4_readiness_changed_event_rows"
        );
        // Explicit fixture cleanup only after every refusal/no-repair assertion passes.
        assert_eq!(
            fixture_ok(
                sqlx::query("DELETE FROM devices WHERE public_key=?")
                    .bind(key.as_slice())
                    .execute(&db.0)
                    .await
            )
            .rows_affected(),
            1,
            "task4_pollution_cleanup_count"
        );
        fixture_ok(rsetup_controller::check_identity_schema(db).await);
    }
}

// The atomic pins physical connection identity only. SQL supplies all locks under test.
async fn task4_participant_pool(db: &DbPool, engine: ExpectedIdentityEngine) -> DbPool {
    use sqlx::ConnectOptions;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use std::time::Duration;
    let config = fixture_ok(TestMigrationConfig::from_test_env());
    fixture_ok(config.authorize(&config.expected_database));
    let expected_database = config.expected_database;
    let connected = Arc::new(AtomicBool::new(false));
    let options =
        db.0.connect_options()
            .as_ref()
            .clone()
            .disable_statement_logging();
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .min_connections(1).max_connections(1)
        .idle_timeout(None).max_lifetime(None)
        .acquire_timeout(Duration::from_secs(10))
        .after_connect(move |connection, _| {
            let expected_database = expected_database.clone();
            let connected = connected.clone();
            Box::pin(async move {
                let checked = async {
                    let (id, version, database): (String, String, Option<String>) = sqlx::query_as(
                        "SELECT CAST(CONNECTION_ID() AS CHAR),CAST(VERSION() AS CHAR),CAST(DATABASE() AS CHAR)",
                    ).fetch_one(&mut *connection).await?;
                    let expected_version = match engine {
                        ExpectedIdentityEngine::MySql => "8.0.46",
                        ExpectedIdentityEngine::TiDb => "8.0.11-TiDB-v8.5.8",
                    };
                    if database.as_deref() != Some(expected_database.as_str()) || version != expected_version
                        || id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit())
                        || !id.parse::<u64>().is_ok_and(|value| value != 0) {
                        return Err(sqlx::Error::Protocol("task4_participant_identity_failed".into()));
                    }
                    match engine {
                        ExpectedIdentityEngine::MySql => {
                            let enabled: i64 = sqlx::query_scalar("SELECT CAST(@@performance_schema AS SIGNED)").fetch_one(&mut *connection).await?;
                            let mapped: i64 = sqlx::query_scalar("SELECT CAST(COUNT(*) AS SIGNED) FROM performance_schema.threads WHERE PROCESSLIST_ID=CONNECTION_ID()").fetch_one(&mut *connection).await?;
                            if enabled != 1 || mapped != 1 {
                                return Err(sqlx::Error::Protocol("task4_mysql_observation_prerequisite".into()));
                            }
                            sqlx::query("SELECT REQUESTING_THREAD_ID,BLOCKING_THREAD_ID,REQUESTING_ENGINE_LOCK_ID,ENGINE FROM performance_schema.data_lock_waits LIMIT 1").fetch_optional(&mut *connection).await?;
                            sqlx::query("SELECT ENGINE_LOCK_ID,ENGINE,OBJECT_SCHEMA,OBJECT_NAME,LOCK_STATUS FROM performance_schema.data_locks LIMIT 1").fetch_optional(&mut *connection).await?;
                        }
                        ExpectedIdentityEngine::TiDb => {
                            // v8.5.8 TiDBConfig GetGlobal returns config.GetJSONConfig();
                            // only this boolean is selected, never the full configuration.
                            let (mode, global_ids): (String, Option<String>) = sqlx::query_as(
                                "SELECT CAST(@@SESSION.tidb_txn_mode AS CHAR),JSON_UNQUOTE(JSON_EXTRACT(@@GLOBAL.tidb_config,'$.\"enable-global-kill\"'))",
                            ).fetch_one(&mut *connection).await?;
                            if mode != "pessimistic" || global_ids.as_deref() != Some("true") {
                                return Err(sqlx::Error::Protocol("task4_tidb_observation_prerequisite".into()));
                            }
                            // Real scans; LIMIT 0 / WHERE false could bypass PROCESS checks.
                            sqlx::query("SELECT TRX_ID,CURRENT_HOLDING_TRX_ID,KEY_INFO FROM information_schema.DATA_LOCK_WAITS LIMIT 1").fetch_optional(&mut *connection).await?;
                            sqlx::query("SELECT ID,SESSION_ID FROM information_schema.CLUSTER_TIDB_TRX WHERE SESSION_ID=CONNECTION_ID()").fetch_all(&mut *connection).await?;
                        }
                    }
                    // Check every physical connection, then reject any replacement.
                    if connected.swap(true, Ordering::SeqCst) {
                        return Err(sqlx::Error::Protocol("task4_participant_reconnected".into()));
                    }
                    Ok::<(), sqlx::Error>(())
                }.await;
                // SQLx may log callback errors: redact driver errors before returning.
                checked.map_err(|_| sqlx::Error::Protocol("task4_connection_prerequisite_failed".into()))
            })
        }).connect_with(options).await;
    DbPool(fixture_ok(pool))
}

async fn task4_connection_id(connection: &mut sqlx::MySqlConnection) -> String {
    let id: String = fixture_ok(
        sqlx::query_scalar("SELECT CAST(CONNECTION_ID() AS CHAR)")
            .fetch_one(connection)
            .await,
    );
    assert!(
        !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_digit())
            && id.parse::<u64>().is_ok_and(|value| value != 0),
        "task4_connection_id_invalid"
    );
    id
}

async fn task4_pool_connection_id(db: &DbPool) -> String {
    let mut connection = fixture_ok(db.0.acquire().await);
    task4_connection_id(&mut connection).await
}

async fn task4_deactivate_actor(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, actor: [u8; 16]) {
    let guard: Vec<(i32, i32)> = fixture_ok(
        sqlx::query_as(
            "SELECT singleton,schema_version FROM schema_meta ORDER BY singleton FOR UPDATE",
        )
        .fetch_all(&mut **tx)
        .await,
    );
    assert!(guard == [(1, 3)], "task4_deactivation_guard_invalid");
    let user: (bool, bool, bool, u64) = fixture_ok(
        sqlx::query_as(
            "SELECT active,is_admin,must_change_password,revision FROM users WHERE id=? FOR UPDATE",
        )
        .bind(actor.as_slice())
        .fetch_one(&mut **tx)
        .await,
    );
    assert!(
        user == (true, true, false, 7),
        "task4_deactivation_actor_invalid"
    );
    let changed = fixture_ok(sqlx::query("UPDATE users SET active=FALSE,revision=revision+1 WHERE id=? AND revision=7 AND active=TRUE").bind(actor.as_slice()).execute(&mut **tx).await).rows_affected();
    assert!(changed == 1, "task4_deactivation_count_invalid");
}

// Positive waiter -> holder samples on schema_meta, not timing guesses. Singleton=1
// follows from the validated fixture/fixed SQL, not independent lock-key parsing.
async fn task4_wait_edge(
    connection: &mut sqlx::MySqlConnection,
    engine: ExpectedIdentityEngine,
    waiter: &str,
    holder: &str,
) -> Result<bool, &'static str> {
    let query = match engine {
        ExpectedIdentityEngine::MySql => {
            "SELECT CAST(COUNT(*) AS SIGNED) FROM performance_schema.data_lock_waits AS w JOIN performance_schema.threads AS wt ON wt.THREAD_ID=w.REQUESTING_THREAD_ID JOIN performance_schema.threads AS ht ON ht.THREAD_ID=w.BLOCKING_THREAD_ID JOIN performance_schema.data_locks AS dl ON dl.ENGINE=w.ENGINE AND dl.ENGINE_LOCK_ID=w.REQUESTING_ENGINE_LOCK_ID WHERE w.ENGINE='INNODB' AND wt.PROCESSLIST_ID=CAST(? AS UNSIGNED) AND ht.PROCESSLIST_ID=CAST(? AS UNSIGNED) AND dl.OBJECT_SCHEMA=DATABASE() AND dl.OBJECT_NAME='schema_meta' AND dl.LOCK_STATUS='WAITING'"
        }
        ExpectedIdentityEngine::TiDb => {
            "SELECT CAST(COUNT(*) AS SIGNED) FROM information_schema.DATA_LOCK_WAITS AS w JOIN information_schema.CLUSTER_TIDB_TRX AS wt ON wt.ID=w.TRX_ID JOIN information_schema.CLUSTER_TIDB_TRX AS ht ON ht.ID=w.CURRENT_HOLDING_TRX_ID WHERE wt.SESSION_ID=CAST(? AS UNSIGNED) AND ht.SESSION_ID=CAST(? AS UNSIGNED) AND JSON_UNQUOTE(JSON_EXTRACT(CASE WHEN JSON_VALID(w.KEY_INFO) THEN w.KEY_INFO ELSE NULL END,'$.db_name'))=DATABASE() AND JSON_UNQUOTE(JSON_EXTRACT(CASE WHEN JSON_VALID(w.KEY_INFO) THEN w.KEY_INFO ELSE NULL END,'$.table_name'))='schema_meta'"
        }
    };
    if matches!(engine, ExpectedIdentityEngine::TiDb) {
        let mappings: Vec<(String, i64)> = sqlx::query_as("SELECT CAST(SESSION_ID AS CHAR),CAST(COUNT(*) AS SIGNED) FROM information_schema.CLUSTER_TIDB_TRX WHERE SESSION_ID IN (CAST(? AS UNSIGNED),CAST(? AS UNSIGNED)) GROUP BY SESSION_ID")
            .bind(waiter).bind(holder).fetch_all(&mut *connection).await.map_err(|_| "task4_tidb_transaction_mapping_failed")?;
        if mappings.iter().any(|(_, count)| *count != 1) {
            return Err("task4_tidb_transaction_mapping_ambiguous");
        }
        if !mappings.iter().any(|(id, _)| id == holder) {
            return Err("task4_tidb_holder_transaction_missing");
        }
        // Waiter may not have begun yet; only an actual positive edge is success.
        if !mappings.iter().any(|(id, _)| id == waiter) {
            return Ok(false);
        }
    }
    let edges: i64 = sqlx::query_scalar(query)
        .bind(waiter)
        .bind(holder)
        .fetch_one(connection)
        .await
        .map_err(|_| "task4_wait_observation_failed")?;
    Ok(edges > 0)
}

async fn task4_meta_values(db: &DbPool) -> String {
    fixture_ok(sqlx::query_scalar("SELECT CAST(JSON_ARRAY(singleton,schema_version,HEX(instance_id),initialized,authz_epoch,admin_guard_revision) AS CHAR) FROM schema_meta WHERE singleton=1").fetch_one(&db.0).await)
}

async fn task4_concurrency_fixture(db: &DbPool) -> ([u8; 16], [u8; 32]) {
    let actor = *uuid::Uuid::new_v4().as_bytes();
    fixture_ok(sqlx::query("INSERT INTO users (id,username,display_name,password_hash,active,is_admin,must_change_password,revision,created_time) VALUES (?,'concurrency_actor','task4 fixture','fixture-placeholder',TRUE,TRUE,FALSE,7,NOW(6))").bind(actor.as_slice()).execute(&db.0).await);
    let key = task4_device(db).await;
    fixture_ok(rsetup_controller::check_identity_schema(db).await);
    (actor, key)
}

async fn task4_assert_deactivated(db: &DbPool, actor: [u8; 16]) {
    let user: (bool, bool, bool, u64) = fixture_ok(
        sqlx::query_as(
            "SELECT active,is_admin,must_change_password,revision FROM users WHERE id=?",
        )
        .bind(actor.as_slice())
        .fetch_one(&db.0)
        .await,
    );
    assert!(
        user == (false, true, false, 8),
        "task4_actor_deactivation_not_persisted"
    );
}

pub async fn admission_deactivation_holds_guard_cas_waits_then_denied(
    db: &DbPool,
    engine: ExpectedIdentityEngine,
) {
    use rsetup_controller::{AdmissionSnapshot, AdmissionState, AdmissionStore, ReviewDecision};
    use std::time::Duration;
    use tokio::sync::{Barrier, oneshot};
    let a = task4_participant_pool(db, engine).await;
    let b = task4_participant_pool(db, engine).await;
    let mut a_connection = fixture_ok(a.0.acquire().await);
    let a_id = task4_connection_id(&mut a_connection).await;
    let b_id = task4_pool_connection_id(&b).await;
    assert!(a_id != b_id, "task4_participants_not_distinct");
    let (actor, key) = task4_concurrency_fixture(db).await;
    let before = fixture_ok(db.load(key).await);
    assert!(
        before
            == AdmissionSnapshot {
                admission_state: AdmissionState::Pending,
                review_decision: ReviewDecision::None,
                revision: 1
            },
        "task4_initial_admission_invalid"
    );
    assert!(
        counts(db, &key).await == (0, 0),
        "task4_initial_events_invalid"
    );
    let events = task4_event_rows(db).await;
    let meta = task4_meta_values(db).await;
    let barrier = Barrier::new(2);
    let (result_tx, mut result_rx) = oneshot::channel();
    // Structured futures, not detached tasks: timeout/error drops both futures
    // and their transactions. Every barrier/channel/observation wait is bounded.
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let holder = async {
            use sqlx::Connection;
            let mut tx = fixture_ok(a_connection.begin().await);
            task4_deactivate_actor(&mut tx, actor).await;
            barrier.wait().await;
            loop {
                tokio::select! {
                    biased;
                    _ = &mut result_rx => return Err("task4_cas_returned_before_wait_edge"),
                    edge = task4_wait_edge(&mut tx, engine, &b_id, &a_id) => {
                        if edge? { break; }
                    }
                }
            }
            assert!(
                task4_connection_id(&mut tx).await == a_id,
                "task4_holder_identity_changed"
            );
            assert!(
                matches!(
                    result_rx.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ),
                "task4_cas_returned_before_holder_commit"
            );
            fixture_ok(tx.commit().await);
            let result = result_rx
                .await
                .map_err(|_| "task4_cas_result_channel_closed")?;
            assert_admission_failure(result, AdmissionFailure::Permission);
            Ok::<(), &'static str>(())
        };
        let waiter = async {
            barrier.wait().await;
            let result = b
                .compare_and_set(
                    key,
                    1,
                    AdmissionState::Pending,
                    ReviewDecision::Approved,
                    Some(actor),
                    None,
                )
                .await;
            result_tx
                .send(result)
                .map_err(|_| "task4_cas_result_receiver_closed")?;
            Ok::<(), &'static str>(())
        };
        tokio::try_join!(holder, waiter)
    })
    .await;
    assert!(
        matches!(outcome, Ok(Ok(_))),
        "task4_wait_order_failed_or_timed_out"
    );
    assert!(
        task4_connection_id(&mut a_connection).await == a_id,
        "task4_holder_identity_changed"
    );
    assert!(
        task4_pool_connection_id(&b).await == b_id,
        "task4_waiter_identity_changed"
    );
    assert!(
        fixture_ok(db.load(key).await) == before,
        "task4_denied_admission_changed"
    );
    assert!(
        task4_event_rows(db).await == events,
        "task4_denied_events_changed"
    );
    task4_assert_deactivated(db, actor).await;
    assert!(
        task4_meta_values(db).await == meta,
        "task4_guard_business_values_changed"
    );
    drop(a_connection);
    a.0.close().await;
    b.0.close().await;
}

pub async fn admission_cas_commits_before_deactivation_then_new_cas_denied(
    db: &DbPool,
    engine: ExpectedIdentityEngine,
) {
    use rsetup_controller::{AdmissionSnapshot, AdmissionState, AdmissionStore, ReviewDecision};
    use std::time::Duration;
    use tokio::sync::{Barrier, oneshot};
    let a = task4_participant_pool(db, engine).await;
    let b = task4_participant_pool(db, engine).await;
    let mut a_connection = fixture_ok(a.0.acquire().await);
    let a_id = task4_connection_id(&mut a_connection).await;
    let b_id = task4_pool_connection_id(&b).await;
    assert!(a_id != b_id, "task4_participants_not_distinct");
    let (actor, key) = task4_concurrency_fixture(db).await;
    let meta = task4_meta_values(db).await;
    let initial = fixture_ok(db.load(key).await);
    assert!(
        initial
            == AdmissionSnapshot {
                admission_state: AdmissionState::Pending,
                review_decision: ReviewDecision::None,
                revision: 1
            },
        "task4_initial_admission_invalid"
    );
    assert!(
        counts(db, &key).await == (0, 0),
        "task4_initial_events_invalid"
    );
    let barrier = Barrier::new(2);
    let (committed_tx, committed_rx) = oneshot::channel();
    let outcome = tokio::time::timeout(Duration::from_secs(20), async {
        let deactivation = async {
            use sqlx::Connection;
            barrier.wait().await;
            let mut tx = fixture_ok(a_connection.begin().await);
            task4_deactivate_actor(&mut tx, actor).await;
            assert!(
                task4_connection_id(&mut tx).await == a_id,
                "task4_holder_identity_changed"
            );
            fixture_ok(tx.commit().await);
            committed_tx
                .send(())
                .map_err(|_| "task4_deactivation_receiver_closed")?;
            Ok::<(), &'static str>(())
        };
        let admissions = async {
            let approved = fixture_ok(
                b.compare_and_set(
                    key,
                    1,
                    AdmissionState::Pending,
                    ReviewDecision::Approved,
                    Some(actor),
                    None,
                )
                .await,
            );
            assert!(
                approved
                    == AdmissionSnapshot {
                        admission_state: AdmissionState::Approved,
                        review_decision: ReviewDecision::Approved,
                        revision: 2
                    },
                "task4_first_cas_snapshot_invalid"
            );
            // Production Ok is after COMMIT; persistence is verified before releasing A.
            assert!(
                fixture_ok(db.load(key).await) == approved,
                "task4_first_cas_not_persisted"
            );
            assert_admission_actor(db, &key, Some(actor)).await;
            let events = task4_event_rows(db).await;
            assert!(
                events.0.len() == 1 && events.1.len() == 1,
                "task4_first_cas_event_count_invalid"
            );
            assert!(
                task4_pool_connection_id(&b).await == b_id,
                "task4_waiter_identity_changed"
            );
            barrier.wait().await;
            committed_rx
                .await
                .map_err(|_| "task4_deactivation_channel_closed")?;
            assert_admission_failure(
                b.compare_and_set(
                    key,
                    2,
                    AdmissionState::Approved,
                    ReviewDecision::Revoked,
                    Some(actor),
                    None,
                )
                .await,
                AdmissionFailure::Permission,
            );
            assert!(
                fixture_ok(db.load(key).await) == approved,
                "task4_second_cas_changed_admission"
            );
            assert!(
                task4_event_rows(db).await == events,
                "task4_second_cas_changed_events"
            );
            Ok::<(), &'static str>(())
        };
        tokio::try_join!(deactivation, admissions)
    })
    .await;
    assert!(
        matches!(outcome, Ok(Ok(_))),
        "task4_commit_order_failed_or_timed_out"
    );
    assert!(
        task4_connection_id(&mut a_connection).await == a_id,
        "task4_holder_identity_changed"
    );
    assert!(
        task4_pool_connection_id(&b).await == b_id,
        "task4_waiter_identity_changed"
    );
    task4_assert_deactivated(db, actor).await;
    assert!(
        task4_meta_values(db).await == meta,
        "task4_guard_business_values_changed"
    );
    drop(a_connection);
    a.0.close().await;
    b.0.close().await;
}

pub async fn admission_cas_scenarios(db: &DbPool) {
    use rsetup_controller::{AdmissionState, AdmissionStore, ControllerError, ReviewDecision};
    let public_key = unique_public_key();
    sqlx::query("INSERT INTO devices (public_key, display_name, admission_state, review_decision, revision, archived) VALUES (?, 'test device', 'PENDING', 'none', 1, FALSE)")
        .bind(public_key.as_slice()).execute(&db.0).await.unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(counts(db, &public_key).await, (0, 0));
    let next = db
        .compare_and_set(
            public_key,
            1,
            AdmissionState::Pending,
            ReviewDecision::Approved,
            None,
            None,
        )
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(next.revision, 2);
    assert_eq!(
        db.load(public_key)
            .await
            .unwrap_or_else(|_| panic!("task4_fixture_operation_failed")),
        next
    );
    assert_eq!(counts(db, &public_key).await, (1, 1));
    assert!(matches!(
        db.compare_and_set(
            public_key,
            1,
            AdmissionState::Pending,
            ReviewDecision::Denied,
            None,
            None
        )
        .await,
        Err(ControllerError::RevisionConflict)
    ));
    assert_eq!(
        db.load(public_key)
            .await
            .unwrap_or_else(|_| panic!("task4_fixture_operation_failed")),
        next
    );
    assert_eq!(counts(db, &public_key).await, (1, 1));

    // Reserve the next real (process_epoch,event_seq) unique key: audit insertion then fails
    // after device UPDATE and admission_decisions INSERT, forcing a real transaction rollback.
    let (epoch, seq): (Vec<u8>, u64) = sqlx::query_as("SELECT process_epoch, event_seq FROM audit_events WHERE target_id = ? ORDER BY event_seq DESC LIMIT 1")
        .bind(hex::encode(public_key)).fetch_one(&db.0).await.unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    let next_seq = seq
        .checked_add(1)
        .unwrap_or_else(|| panic!("task4_fixture_operation_failed"));
    let collision_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO audit_events (id, actor_kind, event_type, params_redacted, outcome, time_evidence, process_epoch, event_seq) VALUES (?, 'system', 'test.collision', '{}', 'success', '{}', ?, ?)")
        .bind(collision_id.as_bytes().as_slice()).bind(&epoch).bind(next_seq).execute(&db.0).await.unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    let before = counts(db, &public_key).await;
    let event_rows = task4_event_rows(db).await;
    let collision_result = db
        .compare_and_set(
            public_key,
            2,
            AdmissionState::Approved,
            ReviewDecision::Revoked,
            None,
            None,
        )
        .await;
    let targeted_duplicate = match collision_result {
        Err(ControllerError::Database(sqlx::Error::Database(error))) => {
            error
                .try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>()
                .is_some_and(|mysql| mysql.number() == 1062)
                && error.message().contains("uq_audit_epoch_seq")
        }
        _ => false,
    };
    assert!(
        targeted_duplicate,
        "task4_expected_audit_epoch_seq_collision"
    );
    assert!(
        task4_event_rows(db).await == event_rows,
        "task4_collision_event_rows_changed"
    );
    assert_eq!(
        db.load(public_key)
            .await
            .unwrap_or_else(|_| panic!("task4_fixture_operation_failed")),
        next,
        "device UPDATE must roll back"
    );
    assert_eq!(
        counts(db, &public_key).await,
        before,
        "history and audit must roll back"
    );
    sqlx::query("DELETE FROM audit_events WHERE id = ?")
        .bind(collision_id.as_bytes().as_slice())
        .execute(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
}

pub async fn identity_unsigned_high_half_round_trip(db: &DbPool) {
    use rsetup_controller::{AdmissionState, AdmissionStore, ControllerError, ReviewDecision};
    use sqlx::Row;

    let key = unique_public_key();
    let high = i64::MAX as u64 + 9;
    sqlx::query("INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (?,'high','PENDING','none',?,FALSE)")
        .bind(key.as_slice()).bind(high).execute(&db.0).await.unwrap();
    assert_eq!(db.load(key).await.unwrap().revision, high);
    let next = db
        .compare_and_set(
            key,
            high,
            AdmissionState::Pending,
            ReviewDecision::Approved,
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(next.revision, high + 1);
    let row = sqlx::query("SELECT revision FROM devices WHERE public_key=?")
        .bind(key.as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(row.try_get::<u64, _>("revision").unwrap(), high + 1);
    let history = sqlx::query(
        "SELECT previous_revision,new_revision FROM admission_decisions WHERE device_id=?",
    )
    .bind(key.as_slice())
    .fetch_one(&db.0)
    .await
    .unwrap();
    assert_eq!(
        history.try_get::<u64, _>("previous_revision").unwrap(),
        high
    );
    assert_eq!(history.try_get::<u64, _>("new_revision").unwrap(), high + 1);
    let before = counts(db, &key).await;
    assert!(matches!(
        db.compare_and_set(
            key,
            high,
            AdmissionState::Pending,
            ReviewDecision::Denied,
            None,
            None
        )
        .await,
        Err(ControllerError::RevisionConflict)
    ));
    assert_eq!(db.load(key).await.unwrap(), next);
    assert_eq!(
        counts(db, &key).await,
        before,
        "stale CAS must not add history/audit"
    );

    sqlx::query("UPDATE schema_meta SET authz_epoch=?, admin_guard_revision=? WHERE singleton=1")
        .bind(high)
        .bind(high)
        .execute(&db.0)
        .await
        .unwrap();
    let meta =
        sqlx::query("SELECT authz_epoch,admin_guard_revision FROM schema_meta WHERE singleton=1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(meta.try_get::<u64, _>("authz_epoch").unwrap(), high);
    assert_eq!(
        meta.try_get::<u64, _>("admin_guard_revision").unwrap(),
        high
    );

    let user_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO users (id,username,display_name,password_hash,active,is_admin,must_change_password,revision,created_time) VALUES (?,'high_user','High','hash',TRUE,FALSE,FALSE,?,NOW(6))")
        .bind(user_id.as_bytes().as_slice()).bind(high).execute(&db.0).await.unwrap();
    let user = sqlx::query("SELECT revision FROM users WHERE id=?")
        .bind(user_id.as_bytes().as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(user.try_get::<u64, _>("revision").unwrap(), high);

    let role_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO roles (id,name,builtin,archived,revision) VALUES (?,'high_role',FALSE,FALSE,?)")
        .bind(role_id.as_bytes().as_slice()).bind(high).execute(&db.0).await.unwrap();
    let role = sqlx::query("SELECT revision FROM roles WHERE id=?")
        .bind(role_id.as_bytes().as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(role.try_get::<u64, _>("revision").unwrap(), high);

    let group_id = uuid::Uuid::new_v4();
    sqlx::query(
        "INSERT INTO device_groups (id,name,archived,revision) VALUES (?,'high_group',FALSE,?)",
    )
    .bind(group_id.as_bytes().as_slice())
    .bind(high)
    .execute(&db.0)
    .await
    .unwrap();
    let group = sqlx::query("SELECT revision FROM device_groups WHERE id=?")
        .bind(group_id.as_bytes().as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(group.try_get::<u64, _>("revision").unwrap(), high);

    let grant_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO grants (id,user_id,source_kind,permissions,scope_kind,revision) VALUES (?,?,'direct','{}','all',?)")
        .bind(grant_id.as_bytes().as_slice()).bind(user_id.as_bytes().as_slice()).bind(high)
        .execute(&db.0).await.unwrap();
    let grant = sqlx::query("SELECT revision FROM grants WHERE id=?")
        .bind(grant_id.as_bytes().as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(grant.try_get::<u64, _>("revision").unwrap(), high);

    let audit_id = uuid::Uuid::new_v4();
    let audit_epoch = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO audit_events (id,actor_kind,event_type,params_redacted,outcome,time_evidence,process_epoch,event_seq) VALUES (?,'system','high.fixture','{}','success','{}',?,?)")
        .bind(audit_id.as_bytes().as_slice()).bind(audit_epoch.as_bytes().as_slice()).bind(high)
        .execute(&db.0).await.unwrap();
    let audit = sqlx::query("SELECT event_seq FROM audit_events WHERE id=?")
        .bind(audit_id.as_bytes().as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(audit.try_get::<u64, _>("event_seq").unwrap(), high);

    // A second epoch isolates this maximum-value bind from the live process audit counter.
    let max_audit_id = uuid::Uuid::new_v4();
    let max_epoch = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO audit_events (id,actor_kind,event_type,params_redacted,outcome,time_evidence,process_epoch,event_seq) VALUES (?,'system','max.fixture','{}','success','{}',?,?)")
        .bind(max_audit_id.as_bytes().as_slice()).bind(max_epoch.as_bytes().as_slice()).bind(u64::MAX)
        .execute(&db.0).await.unwrap();
    let max_audit = sqlx::query("SELECT event_seq FROM audit_events WHERE id=?")
        .bind(max_audit_id.as_bytes().as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(max_audit.try_get::<u64, _>("event_seq").unwrap(), u64::MAX);

    // The limit is a real row and the evidence is persistent COUNT(*), not a fake call count.
    sqlx::query("UPDATE devices SET revision=? WHERE public_key=?")
        .bind(u64::MAX)
        .bind(key.as_slice())
        .execute(&db.0)
        .await
        .unwrap();
    let max_snapshot = db.load(key).await.unwrap();
    assert_eq!(max_snapshot.revision, u64::MAX);
    let before = counts(db, &key).await;
    assert!(matches!(
        db.compare_and_set(
            key,
            u64::MAX,
            AdmissionState::Approved,
            ReviewDecision::Revoked,
            None,
            None
        )
        .await,
        Err(ControllerError::RevisionConflict)
    ));
    assert_max_snapshot_unchanged(max_snapshot, db.load(key).await.unwrap());
    assert_eq!(
        counts(db, &key).await,
        before,
        "overflow must not write history/audit"
    );
}

pub async fn bootstrap_failure_does_not_reinitialize(db: &DbPool) {
    let sink = FailingSecretSink::default();
    assert!(
        matches!(
            rsetup_controller::bootstrap_admin(db, &sink).await,
            Err(rsetup_controller::ControllerError::Config(message))
                if message == "private secret write failed"
        ),
        "task4_expected_postcommit_sink_failure"
    );
    assert_eq!(sink.attempts(), 1, "task4_sink_first_attempt");
    let committed = task4_bootstrap_rows(db).await;
    assert_bootstrap_committed(db).await;
    fixture_ok(rsetup_controller::bootstrap_admin(db, &sink).await);
    assert_eq!(sink.attempts(), 1, "task4_sink_must_not_retry");
    let successful_sink = RecordingSecretSink::default();
    fixture_ok(rsetup_controller::bootstrap_admin(db, &successful_sink).await);
    assert!(
        successful_sink.emissions().is_empty(),
        "task4_secret_reemitted_after_failure"
    );
    assert!(
        task4_bootstrap_rows(db).await == committed,
        "task4_bootstrap_commit_changed"
    );
    fixture_ok(rsetup_controller::check_identity_schema(db).await);
}

async fn task4_bootstrap_rows(db: &DbPool) -> (Vec<String>, Vec<String>) {
    let users = fixture_ok(sqlx::query_scalar("SELECT CAST(JSON_ARRAY(HEX(id),username,display_name,password_hash,active,is_admin,must_change_password,revision,CAST(created_time AS CHAR)) AS CHAR) AS row_value FROM users ORDER BY id").fetch_all(&db.0).await);
    let meta = fixture_ok(sqlx::query_scalar("SELECT CAST(JSON_ARRAY(singleton,schema_version,initialized,HEX(instance_id),authz_epoch,admin_guard_revision) AS CHAR) AS row_value FROM schema_meta ORDER BY singleton").fetch_all(&db.0).await);
    (users, meta)
}

async fn assert_bootstrap_committed(db: &DbPool) {
    let users: i64 = fixture_ok(
        sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(&db.0)
            .await,
    );
    let admins: i64 = fixture_ok(sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username='admin' AND active=1 AND is_admin=1 AND must_change_password=1 AND revision=1").fetch_one(&db.0).await);
    let initialized: i64 = fixture_ok(
        sqlx::query_scalar(
            "SELECT CAST(initialized AS SIGNED) AS initialized FROM schema_meta WHERE singleton=1",
        )
        .fetch_one(&db.0)
        .await,
    );
    assert!(
        users == 1 && admins == 1 && initialized == 1,
        "task4_bootstrap_not_committed"
    );
}

pub async fn concurrent_bootstraps_emit_once(db: &DbPool) {
    let sink = RecordingSecretSink::default();
    let (a, b) = tokio::join!(
        rsetup_controller::bootstrap_admin(db, &sink),
        rsetup_controller::bootstrap_admin(db, &sink)
    );
    fixture_ok(a);
    fixture_ok(b);
    assert_bootstrap_committed(db).await;
    let emissions = sink.emissions();
    assert_eq!(emissions.len(), 1, "task4_bootstrap_emission_count");
    // The sole emitted secret must be the committed account's password.
    use argon2::{Argon2, PasswordHash, PasswordVerifier};
    let hash = user_hash(db, "admin").await;
    let parsed = fixture_ok(PasswordHash::new(&hash));
    fixture_ok(Argon2::default().verify_password(emissions[0].as_bytes(), &parsed));
    let committed = task4_bootstrap_rows(db).await;
    fixture_ok(rsetup_controller::bootstrap_admin(db, &sink).await);
    assert_eq!(sink.emissions().len(), 1, "task4_bootstrap_repeat_emission");
    assert!(
        task4_bootstrap_rows(db).await == committed,
        "task4_bootstrap_repeat_changed_rows"
    );
    fixture_ok(rsetup_controller::check_identity_schema(db).await);
}

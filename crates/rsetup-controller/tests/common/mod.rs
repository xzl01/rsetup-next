use rsetup_controller::{ControllerConfig, DbPool, TestMigrationConfig};
use std::sync::Mutex;

pub async fn required_test_db() -> DbPool {
    let config = TestMigrationConfig::from_test_env()
        .expect("explicit disposable test migration configuration");
    config
        .authorize(&config.expected_database)
        .expect("test database migration authorization");
    let db = DbPool::connect(&ControllerConfig {
        database_url: config.test_url.clone(),
        listen_address: String::new(),
    })
    .await
    .expect("test database connection");
    let name: String = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(&db.0)
        .await
        .expect("selected test database");
    config
        .authorize(&name)
        .expect("actual test database name must match authorization");
    db
}

pub async fn required_fresh_identity_db() -> DbPool {
    let db = required_test_db().await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_one(&db.0)
    .await
    .expect("count test schema tables");
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
            .unwrap();
    assert_eq!(version, 3);
    let initialized: bool =
        sqlx::query_scalar("SELECT initialized FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert!(!initialized);
    let meta_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schema_meta")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(meta_rows, 1);
    let constraints: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type IN ('CHECK', 'FOREIGN KEY')")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(constraints, 0);
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_one(&db.0)
    .await
    .unwrap();
    assert_eq!(tables, 11);
    rsetup_controller::check_identity_schema(db).await.unwrap();
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
        "upgrade" | "fixture-v1" | "fixture-partial-v1"
    ));
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
        .args(["--mode", mode])
        .status()
        .expect("explicit test migration binary");
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
        "INSERT INTO grants (id,user_id,source_kind,permissions,scope_kind,revision) VALUES (UNHEX(REPLACE(UUID(),'-','')),UNHEX(REPLACE(UUID(),'-','')),'direct','{}','all',-1)",
        "SELECT revision FROM grants",
    ),
    (
        "admission_decisions",
        "previous_revision",
        "INSERT INTO admission_decisions (id,device_id,decision,previous_revision,new_revision,time_evidence) VALUES (UNHEX(REPLACE(UUID(),'-','')),REPEAT('x',32),'approved',-1,0,'{}')",
        "SELECT previous_revision FROM admission_decisions",
    ),
    (
        "admission_decisions",
        "new_revision",
        "INSERT INTO admission_decisions (id,device_id,decision,previous_revision,new_revision,time_evidence) VALUES (UNHEX(REPLACE(UUID(),'-','')),REPEAT('x',32),'approved',0,-1,'{}')",
        "SELECT new_revision FROM admission_decisions",
    ),
    (
        "audit_events",
        "event_seq",
        "INSERT INTO audit_events (id,actor_kind,event_type,params_redacted,outcome,time_evidence,process_epoch,event_seq) VALUES (UNHEX(REPLACE(UUID(),'-','')),'system','fixture','{}','success','{}',UNHEX(REPLACE(UUID(),'-','')),-1)",
        "SELECT event_seq FROM audit_events",
    ),
];

pub async fn negative_fixture_prevents_all_alters(index: usize) {
    let db = required_fresh_identity_db().await;
    run_explicit_identity_test_command("fixture-v1");
    let (table, column, inject, read_value) = NEGATIVE_FIXTURES[index];
    sqlx::query(inject).execute(&db.0).await.unwrap();
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
        let shape: String = sqlx::query_scalar("SELECT column_type FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?")
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
            required: 2
        })
    ));
    run_explicit_identity_test_command("upgrade");
    rsetup_controller::check_identity_schema(&db).await.unwrap();
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(version, 2);
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
        .unwrap()
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
            .unwrap();
    let audit: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE target_id = ?")
        .bind(hex::encode(public_key))
        .fetch_one(&db.0)
        .await
        .unwrap();
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

pub async fn admission_cas_scenarios(db: &DbPool) {
    use rsetup_controller::{AdmissionState, AdmissionStore, ControllerError, ReviewDecision};
    let public_key = unique_public_key();
    sqlx::query("INSERT INTO devices (public_key, display_name, admission_state, review_decision, revision, archived) VALUES (?, 'test device', 'PENDING', 'none', 1, FALSE)")
        .bind(public_key.as_slice()).execute(&db.0).await.unwrap();
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
        .unwrap();
    assert_eq!(next.revision, 2);
    assert_eq!(db.load(public_key).await.unwrap(), next);
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
    assert_eq!(db.load(public_key).await.unwrap(), next);
    assert_eq!(counts(db, &public_key).await, (1, 1));

    // Reserve the next real (process_epoch,event_seq) unique key: audit insertion then fails
    // after device UPDATE and admission_decisions INSERT, forcing a real transaction rollback.
    let (epoch, seq): (Vec<u8>, u64) = sqlx::query_as("SELECT process_epoch, event_seq FROM audit_events WHERE target_id = ? ORDER BY event_seq DESC LIMIT 1")
        .bind(hex::encode(public_key)).fetch_one(&db.0).await.unwrap();
    let next_seq = seq
        .checked_add(1)
        .expect("collision fixture requires an audit sequence below u64::MAX");
    let collision_id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO audit_events (id, actor_kind, event_type, params_redacted, outcome, time_evidence, process_epoch, event_seq) VALUES (?, 'system', 'test.collision', '{}', 'success', '{}', ?, ?)")
        .bind(collision_id.as_bytes().as_slice()).bind(&epoch).bind(next_seq).execute(&db.0).await.unwrap();
    let before = counts(db, &public_key).await;
    assert!(
        db.compare_and_set(
            public_key,
            2,
            AdmissionState::Approved,
            ReviewDecision::Revoked,
            None,
            None
        )
        .await
        .is_err()
    );
    assert_eq!(
        db.load(public_key).await.unwrap(),
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
        .unwrap();
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
    assert!(rsetup_controller::bootstrap_admin(db, &sink).await.is_err());
    assert_eq!(sink.attempts(), 1);
    let hash = user_hash(db, "admin").await;
    let initialized: bool =
        sqlx::query_scalar("SELECT initialized FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert!(initialized);
    rsetup_controller::bootstrap_admin(db, &sink).await.unwrap();
    assert_eq!(sink.attempts(), 1);
    assert_eq!(user_hash(db, "admin").await, hash);
}

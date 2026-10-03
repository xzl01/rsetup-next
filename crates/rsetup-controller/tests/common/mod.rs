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

pub async fn required_prepared_identity_db() -> DbPool {
    let db = required_test_db().await;
    rsetup_controller::check_identity_schema(&db)
        .await
        .expect("existing v2 identity schema");
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
    let (epoch, seq): (Vec<u8>, i64) = sqlx::query_as("SELECT process_epoch, event_seq FROM audit_events WHERE target_id = ? ORDER BY event_seq DESC LIMIT 1")
        .bind(hex::encode(public_key)).fetch_one(&db.0).await.unwrap();
    let next_seq = seq + 1;
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

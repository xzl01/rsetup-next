mod common;
use common::{RecordingSecretSink, required_test_db, user_hash};
use rsetup_controller::{bootstrap_admin, migrate};

async fn fresh_bootstrap(db: &rsetup_controller::DbPool) {
    migrate(db).await.unwrap();
    sqlx::query("DELETE FROM users WHERE username = 'admin'")
        .execute(&db.0)
        .await
        .unwrap();
    sqlx::query("UPDATE schema_meta SET initialized = FALSE WHERE singleton = 1")
        .execute(&db.0)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn bootstrap_twice_preserves_password_and_emits_once() {
    let db = required_test_db().await;
    fresh_bootstrap(&db).await;
    let sink = RecordingSecretSink::default();
    bootstrap_admin(&db, &sink).await.unwrap();
    let first_hash = user_hash(&db, "admin").await;
    bootstrap_admin(&db, &sink).await.unwrap();
    assert_eq!(user_hash(&db, "admin").await, first_hash);
    assert_eq!(sink.emissions().len(), 1);
}

#[tokio::test]
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn concurrent_bootstraps_create_one_admin_and_emit_once() {
    let db = required_test_db().await;
    fresh_bootstrap(&db).await;
    let sink = RecordingSecretSink::default();
    let (a, b) = tokio::join!(bootstrap_admin(&db, &sink), bootstrap_admin(&db, &sink));
    a.unwrap();
    b.unwrap();
    let admins: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE username = 'admin' AND is_admin = TRUE",
    )
    .fetch_one(&db.0)
    .await
    .unwrap();
    assert_eq!(admins, 1);
    assert_eq!(sink.emissions().len(), 1);
}

#[tokio::test]
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn empty_users_do_not_reset_initialized_marker() {
    let db = required_test_db().await;
    fresh_bootstrap(&db).await;
    let sink = RecordingSecretSink::default();
    bootstrap_admin(&db, &sink).await.unwrap();
    sqlx::query("DELETE FROM users WHERE username = 'admin'")
        .execute(&db.0)
        .await
        .unwrap();
    bootstrap_admin(&db, &sink).await.unwrap();
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE username = 'admin'")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(admins, 0);
    assert_eq!(sink.emissions().len(), 1);
}

#[tokio::test]
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn migration_is_repeatable_and_repairs_interrupted_ddl() {
    let db = required_test_db().await;
    migrate(&db).await.unwrap();
    migrate(&db).await.unwrap();
    sqlx::query("DROP TABLE audit_events")
        .execute(&db.0)
        .await
        .unwrap();
    migrate(&db).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'audit_events'")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn admission_cas_persists_history_and_audit_atomically() {
    use rsetup_controller::{AdmissionState, AdmissionStore, ControllerError, ReviewDecision};
    let db = required_test_db().await;
    migrate(&db).await.unwrap();
    let public_key = [41u8; 32];
    sqlx::query("DELETE FROM devices WHERE public_key = ?")
        .bind(public_key.as_slice())
        .execute(&db.0)
        .await
        .unwrap();
    sqlx::query("INSERT INTO devices (public_key, display_name, admission_state, review_decision, revision, archived) VALUES (?, 'test device', 'PENDING', 'none', 1, FALSE)")
        .bind(public_key.as_slice()).execute(&db.0).await.unwrap();
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
        db.load(public_key).await.unwrap().review_decision,
        ReviewDecision::Approved
    );
    let history: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM admission_decisions WHERE device_id = ? AND new_revision = 2",
    )
    .bind(public_key.as_slice())
    .fetch_one(&db.0)
    .await
    .unwrap();
    let audited: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE target_id = ? AND event_type = 'admission.approve'")
        .bind(hex::encode(public_key)).fetch_one(&db.0).await.unwrap();
    assert_eq!(history, 1);
    assert_eq!(audited, 1);
}

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
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
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
async fn bootstrap_failure_is_not_retried_after_commit() {
    let db = required_test_db().await;
    fresh_bootstrap(&db).await;
    common::bootstrap_failure_does_not_reinitialize(&db).await;
}

#[tokio::test]
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn migration_is_repeatable_and_repairs_interrupted_ddl() {
    let db = required_test_db().await;
    migrate(&db).await.unwrap();
    migrate(&db).await.unwrap();
    sqlx::query("DROP TABLE role_permissions")
        .execute(&db.0)
        .await
        .unwrap();
    migrate(&db).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'role_permissions'")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn migration_rejects_wrong_named_table_before_bootstrap() {
    let db = required_test_db().await;
    migrate(&db).await.unwrap();
    // Dedicated destructive fixture: isolate a disposable test database before renaming.
    let saved = format!("role_permissions_saved_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("RENAME TABLE role_permissions TO `{saved}`"))
        .execute(&db.0)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE role_permissions (role_id BINARY(16) NOT NULL PRIMARY KEY, permission VARCHAR(128) NULL) CHARACTER SET utf8mb4")
        .execute(&db.0).await.unwrap();
    let result = migrate(&db).await;
    sqlx::query("DROP TABLE role_permissions")
        .execute(&db.0)
        .await
        .unwrap();
    sqlx::query(&format!("RENAME TABLE `{saved}` TO role_permissions"))
        .execute(&db.0)
        .await
        .unwrap();
    assert!(result.is_err(), "incompatible table must fail closed");
}

#[tokio::test]
#[ignore = "requires isolated CONTROLLER_TEST_DATABASE_URL and --ignored"]
async fn admission_cas_persists_history_and_audit_atomically() {
    let db = required_test_db().await;
    migrate(&db).await.unwrap();
    common::admission_cas_scenarios(&db).await;
}

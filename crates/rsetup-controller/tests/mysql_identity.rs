mod common;
use common::{RecordingSecretSink, user_hash};
use rsetup_controller::{ControllerError, bootstrap_admin, check_identity_schema};

async fn fresh_v2() -> rsetup_controller::DbPool {
    let db = common::required_fresh_identity_db().await;
    common::run_explicit_identity_test_command("upgrade");
    check_identity_schema(&db).await.unwrap();
    db
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn identity_contract_fresh_and_restart() {
    let db = fresh_v2().await;
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(version, 2);
    check_identity_schema(&db).await.unwrap();
    let _prepared = common::required_prepared_identity_db().await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn legacy_v1_is_readonly_rejected_then_explicitly_upgraded() {
    let db = common::required_fresh_identity_db().await;
    common::run_explicit_identity_test_command("fixture-v1");
    assert!(matches!(
        check_identity_schema(&db).await,
        Err(ControllerError::SchemaNotReady {
            found: Some(1),
            required: 2
        })
    ));
    common::run_explicit_identity_test_command("upgrade");
    check_identity_schema(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn partial_v1_column_is_resumed_only_by_explicit_command() {
    let db = common::required_fresh_identity_db().await;
    common::run_explicit_identity_test_command("fixture-v1");
    common::run_explicit_identity_test_command("fixture-partial-v1");
    assert!(matches!(
        check_identity_schema(&db).await,
        Err(ControllerError::SchemaNotReady {
            found: Some(1),
            required: 2
        })
    ));
    common::run_explicit_identity_test_command("upgrade");
    check_identity_schema(&db).await.unwrap();
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn bootstrap_twice_preserves_password_and_emits_once() {
    let db = fresh_v2().await;
    let sink = RecordingSecretSink::default();
    bootstrap_admin(&db, &sink).await.unwrap();
    let first_hash = user_hash(&db, "admin").await;
    bootstrap_admin(&db, &sink).await.unwrap();
    assert_eq!(user_hash(&db, "admin").await, first_hash);
    assert_eq!(sink.emissions().len(), 1);
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn concurrent_bootstraps_create_one_admin_and_emit_once() {
    let db = fresh_v2().await;
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
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn empty_users_do_not_reset_initialized_marker() {
    let db = fresh_v2().await;
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
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn bootstrap_failure_is_not_retried_after_commit() {
    let db = fresh_v2().await;
    common::bootstrap_failure_does_not_reinitialize(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn migration_does_not_repair_missing_table() {
    let db = fresh_v2().await;
    sqlx::query("DROP TABLE role_permissions")
        .execute(&db.0)
        .await
        .unwrap();
    assert!(check_identity_schema(&db).await.is_err());
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
        .args(["--mode", "upgrade"])
        .status()
        .unwrap();
    assert!(!status.success());
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'role_permissions'")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn migration_rejects_wrong_named_table_before_bootstrap() {
    let db = fresh_v2().await;
    sqlx::query("ALTER TABLE role_permissions MODIFY COLUMN permission VARCHAR(128) NULL")
        .execute(&db.0)
        .await
        .unwrap();
    assert!(check_identity_schema(&db).await.is_err());
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
        .args(["--mode", "upgrade"])
        .status()
        .unwrap();
    assert!(!status.success());
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn invalid_stored_usernames_prevent_all_alters() {
    let db = common::required_fresh_identity_db().await;
    common::run_explicit_identity_test_command("fixture-v1");
    for name in ["Éric", "Alice", "aa", "a".repeat(65).as_str()] {
        let id = uuid::Uuid::new_v4();
        sqlx::query("INSERT INTO users (id, username, display_name, password_hash, active, is_admin, must_change_password, revision, created_time) VALUES (?, ?, 'bad fixture', 'not-a-secret', TRUE, FALSE, FALSE, 0, NOW(6))")
            .bind(id.as_bytes().as_slice()).bind(name).execute(&db.0).await.unwrap();
        let result = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
            .args(["--mode", "upgrade"])
            .status()
            .unwrap();
        assert!(!result.success());
        let stored: String = sqlx::query_scalar("SELECT username FROM users WHERE id = ?")
            .bind(id.as_bytes().as_slice())
            .fetch_one(&db.0)
            .await
            .unwrap();
        assert_eq!(stored, name);
        let version: i32 =
            sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
                .fetch_one(&db.0)
                .await
                .unwrap();
        assert_eq!(version, 1);
        let column_type: String = sqlx::query_scalar("SELECT column_type FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = 'schema_meta' AND column_name = 'authz_epoch'")
            .fetch_one(&db.0).await.unwrap();
        assert_eq!(column_type.to_ascii_lowercase(), "bigint");
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id.as_bytes().as_slice())
            .execute(&db.0)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn negative_epoch_prevents_all_alters() {
    let db = common::required_fresh_identity_db().await;
    common::run_explicit_identity_test_command("fixture-v1");
    sqlx::query("UPDATE schema_meta SET authz_epoch = -1 WHERE singleton = 1")
        .execute(&db.0)
        .await
        .unwrap();
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
        .args(["--mode", "upgrade"])
        .status()
        .unwrap();
    assert!(!status.success());
    let (version, epoch): (i32, i64) =
        sqlx::query_as("SELECT schema_version, authz_epoch FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!((version, epoch), (1, -1));
    let column_type: String = sqlx::query_scalar("SELECT column_type FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = 'schema_meta' AND column_name = 'authz_epoch'")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(column_type.to_ascii_lowercase(), "bigint");
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn admission_cas_persists_history_and_audit_atomically() {
    let db = fresh_v2().await;
    common::admission_cas_scenarios(&db).await;
}

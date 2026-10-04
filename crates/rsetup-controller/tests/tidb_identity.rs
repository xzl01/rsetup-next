#[tokio::test]
#[ignore = "one independently backed-up isolated disposable EMPTY TiDB dev DB; never run without operator confirmation"]
async fn fresh_v3_single_authorized_tidb_case() {
    let db = common::required_fresh_identity_db().await;
    let engine: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert!(
        !engine.trim().is_empty(),
        "SELECT VERSION() must return a version"
    );
    common::run_explicit_identity_test_command("upgrade");
    common::assert_fresh_v3_is_check_and_fk_free(&db).await;
    let first_id: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM schema_meta WHERE singleton=1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    common::run_explicit_identity_test_command("upgrade");
    common::assert_fresh_v3_is_check_and_fk_free(&db).await;
    let second_id: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM schema_meta WHERE singleton=1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(
        first_id, second_id,
        "v3 read-only retry preserves instance identity"
    );
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable EMPTY TiDB dev DB; runner and FK visibility review required"]
async fn tidb_legacy_v1_to_v3_positive_single_case() {
    common::assert_legacy_fixture_upgrades_to_v3(
        "fixture-v1",
        common::ExpectedIdentityEngine::TiDb,
    )
    .await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable EMPTY TiDB dev DB; runner and FK visibility review required"]
async fn tidb_legacy_v2_to_v3_positive_single_case() {
    common::assert_legacy_fixture_upgrades_to_v3(
        "fixture-v2",
        common::ExpectedIdentityEngine::TiDb,
    )
    .await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable EMPTY TiDB dev DB; runner and FK visibility review required"]
async fn tidb_legacy_mixed_v1_to_v3_positive_single_case() {
    common::assert_legacy_fixture_upgrades_to_v3("mixed-v1", common::ExpectedIdentityEngine::TiDb)
        .await;
}

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
    assert_eq!(version, 3);
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
            required: 3
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
            required: 3
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

macro_rules! negative_signed_case {
    ($name:ident, $index:expr) => {
        #[tokio::test]
        #[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
        async fn $name() {
            common::negative_fixture_prevents_all_alters($index).await;
        }
    };
}
negative_signed_case!(negative_authz_epoch_prevents_all_alters, 0);
negative_signed_case!(negative_admin_guard_revision_prevents_all_alters, 1);
negative_signed_case!(negative_users_revision_prevents_all_alters, 2);
negative_signed_case!(negative_roles_revision_prevents_all_alters, 3);
negative_signed_case!(negative_device_groups_revision_prevents_all_alters, 4);
negative_signed_case!(negative_devices_revision_prevents_all_alters, 5);
negative_signed_case!(negative_grants_revision_prevents_all_alters, 6);
negative_signed_case!(negative_previous_revision_prevents_all_alters, 7);
negative_signed_case!(negative_new_revision_prevents_all_alters, 8);
negative_signed_case!(negative_audit_event_seq_prevents_all_alters, 9);

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn later_column_interruption_then_explicit_retry() {
    common::later_column_interruption_is_resumable().await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn identity_unsigned_high_half_round_trip() {
    let db = common::required_fresh_identity_db().await;
    common::run_explicit_identity_test_command("upgrade");
    check_identity_schema(&db).await.unwrap();
    common::identity_unsigned_high_half_round_trip(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable empty test DB and operator authorization required"]
async fn admission_cas_persists_history_and_audit_atomically() {
    let db = fresh_v2().await;
    common::admission_cas_scenarios(&db).await;
}

// 夹具基线：当前受支持身份 schema v4（11 张 identity 表 + 5 张 runtime 表，共 16 表，
// 无 FK/CHECK），v4 库由操作员预先迁移预置。用例只连接 v4 库并断言基线，不再在用例内
// 构造 v1/v2/v3 中间态，也不再调用显式 test migration 工具搭建夹具。
// 用例名保留历史名称（如 fresh_v3），避免破坏 --exact 调用与既有文档。

mod common;
use common::{RecordingSecretSink, user_hash};
use rsetup_controller::bootstrap_admin;

// v4 夹具入口：直接连接操作员预置的 v4 基线库；入口一次性核验目标引擎（TiDB）、
// schema_meta.schema_version=4、恰好 16 张表、零 CHECK/FOREIGN KEY，以及 v4 普通启动
// 门禁 check_identity_schema（common::assert_fresh_v4_is_check_and_fk_free）。
async fn fresh_v4() -> rsetup_controller::DbPool {
    common::required_v4_identity_db(common::ExpectedIdentityEngine::TiDb).await
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable TiDB dev DB pre-migrated to identity schema v4; never run without operator confirmation"]
async fn fresh_v3_single_authorized_tidb_case() {
    let db = fresh_v4().await;
    let engine: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert!(
        !engine.trim().is_empty(),
        "SELECT VERSION() must return a version"
    );
    let first_id: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM schema_meta WHERE singleton=1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    // 只读复核：v4 基线断言（含普通启动门禁）不得改写实例身份。
    common::assert_fresh_v4_is_check_and_fk_free(&db).await;
    let second_id: Vec<u8> =
        sqlx::query_scalar("SELECT instance_id FROM schema_meta WHERE singleton=1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(
        first_id, second_id,
        "v4 read-only gate retry preserves instance identity"
    );
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable TiDB dev DB pre-migrated to identity schema v4; never run without operator confirmation"]
async fn admission_missing_actor_is_atomic_on_fresh_v3_tidb() {
    let db = fresh_v4().await;
    common::assert_actor_fixture_v4_without_check_or_fk(&db).await;
    common::admission_missing_actor_is_atomic(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable TiDB dev DB pre-migrated to identity schema v4; never run without operator confirmation"]
async fn admission_actor_permissions_are_atomic_on_fresh_v3_tidb() {
    // v4 基线断言（版本/表集合/无 FK 无 CHECK/v4 门禁）由 fresh_v4 入口完成。
    let db = fresh_v4().await;
    common::admission_actor_permissions_are_atomic(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable TiDB dev DB pre-migrated to identity schema v4; never run without operator confirmation"]
async fn admission_polluted_combinations_are_readonly_rejected_on_fresh_v3_tidb() {
    let db = fresh_v4().await;
    common::admission_polluted_combinations_are_readonly_rejected(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable TiDB dev DB pre-migrated to identity schema v4; lock observation capability required"]
async fn admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb() {
    let db = fresh_v4().await;
    common::admission_deactivation_holds_guard_cas_waits_then_denied(
        &db,
        common::ExpectedIdentityEngine::TiDb,
    )
    .await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable TiDB dev DB pre-migrated to identity schema v4; lock observation capability required"]
async fn admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_tidb() {
    let db = fresh_v4().await;
    common::admission_cas_commits_before_deactivation_then_new_cas_denied(
        &db,
        common::ExpectedIdentityEngine::TiDb,
    )
    .await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn identity_contract_fresh_and_restart() {
    let db = fresh_v4().await;
    let version: i32 =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .unwrap();
    assert_eq!(version, 4);
    rsetup_controller::check_identity_schema(&db).await.unwrap();
    let _prepared = common::required_prepared_identity_db().await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn bootstrap_twice_preserves_password_and_emits_once() {
    let db = fresh_v4().await;
    let sink = RecordingSecretSink::default();
    bootstrap_admin(&db, &sink).await.unwrap();
    let first_hash = user_hash(&db, "admin").await;
    bootstrap_admin(&db, &sink).await.unwrap();
    assert_eq!(user_hash(&db, "admin").await, first_hash);
    assert_eq!(sink.emissions().len(), 1);
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn concurrent_bootstraps_create_one_admin_and_emit_once() {
    let db = fresh_v4().await;
    common::concurrent_bootstraps_emit_once(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn empty_users_do_not_reset_initialized_marker() {
    let db = fresh_v4().await;
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
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn bootstrap_failure_is_not_retried_after_commit() {
    let db = fresh_v4().await;
    common::bootstrap_failure_does_not_reinitialize(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn migration_does_not_repair_missing_table() {
    let db = fresh_v4().await;
    sqlx::query("DROP TABLE role_permissions")
        .execute(&db.0)
        .await
        .unwrap();
    // 被破坏的 v4 夹具必须由 v4 启动门禁的表集合/列形状校验拒绝：schema_version 行
    // 仍为 4，版本决策先放行，拒绝来自结构校验本身，而不是只凭版本号 fail-closed。
    assert!(rsetup_controller::check_identity_schema(&db).await.is_err());
    // v4-only 基线：`--check` 是纯只读入口，既不建表也不修表。它必须照常报告版本 4
    // 并成功退出，同时被破坏的结构原样保留——修复只可能来自操作员的显式核查。
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_rsetup-controller-migrate"))
        .arg("--check")
        .status()
        .unwrap();
    assert!(status.success(), "--check 只读入口必须成功退出");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'role_permissions'")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(count, 0);
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn migration_rejects_wrong_named_table_before_bootstrap() {
    let db = fresh_v4().await;
    sqlx::query("ALTER TABLE devices MODIFY COLUMN display_name VARCHAR(256) NOT NULL")
        .execute(&db.0)
        .await
        .unwrap();
    // 同 migration_does_not_repair_missing_table：版本行仍为 4，v4 启动门禁必须凭
    // 列形状校验拒绝被破坏的夹具。
    assert!(rsetup_controller::check_identity_schema(&db).await.is_err());
    // v4-only 基线：`--check` 是纯只读入口，既不建表也不修表。它必须照常报告版本 4
    // 并成功退出，同时被破坏的结构原样保留——修复只可能来自操作员的显式核查。
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_rsetup-controller-migrate"))
        .arg("--check")
        .status()
        .unwrap();
    assert!(status.success(), "--check 只读入口必须成功退出");
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn identity_unsigned_high_half_round_trip() {
    let db = fresh_v4().await;
    common::identity_unsigned_high_half_round_trip(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable v4-migrated test DB and operator authorization required"]
async fn admission_cas_persists_history_and_audit_atomically() {
    let db = fresh_v4().await;
    common::admission_cas_scenarios(&db).await;
}

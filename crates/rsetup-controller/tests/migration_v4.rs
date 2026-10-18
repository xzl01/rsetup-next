use rsetup_controller::{ControllerConfig, DbPool, TestMigrationConfig};

#[path = "common/mod.rs"]
mod common;

async fn required_v4_test_db() -> DbPool {
    let config = TestMigrationConfig::from_test_env().expect("v4 test db config");
    config
        .authorize(&config.expected_database)
        .expect("authorize test db");
    let db = DbPool::connect(&ControllerConfig {
        database_url: config.test_url.clone(),
        listen_address: String::new(),
    })
    .await
    .expect("connect test db");
    let name: String = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(&db.0)
        .await
        .expect("select database");
    config.authorize(&name).expect("authorize active db");
    db
}

#[tokio::test]
#[ignore = "requires operator confirmed physical isolation and backup"]
async fn test_migration_v4_success_and_verifies_16_tables_and_no_fk() {
    let db = required_v4_test_db().await;
    let engine: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(&db.0)
        .await
        .expect("read engine version");
    assert!(
        !engine.trim().is_empty(),
        "engine version must not be empty"
    );

    // 1. 若当前为空库，先通过官方测试迁移工具升级至 v3 基线
    let current_version: Option<i32> =
        sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_optional(&db.0)
            .await
            .ok()
            .flatten();
    if current_version.is_none() {
        common::run_explicit_identity_test_command("upgrade");
    }

    // 2. 显式运行 v3 -> v4 迁移
    rsetup_controller::db::run_schema_v4_migration(&db)
        .await
        .expect("v4 migration must succeed");

    // 3. schema_meta 存在且 singleton = 1, schema_version = 4
    let (singleton, version): (i32, i32) =
        sqlx::query_as("SELECT singleton, schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .expect("read schema_meta");
    assert_eq!(singleton, 1);
    assert_eq!(version, 4);

    // 4. 数据库恰好包含 16 张表（11 identity + 5 runtime）
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_one(&db.0)
    .await
    .expect("count tables");
    assert_eq!(count, 16, "v4 schema must have exactly 16 tables");

    // 5. 无任何外键定义
    let fk_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'FOREIGN KEY'",
    )
    .fetch_one(&db.0)
    .await
    .expect("count fk constraints");
    assert_eq!(fk_count, 0, "must have 0 foreign keys");

    // 6. 无任何 CHECK 约束
    let check_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'CHECK'",
    )
    .fetch_one(&db.0)
    .await
    .expect("count check constraints");
    assert_eq!(check_count, 0, "must have 0 check constraints");

    // 7. 校验 check_identity_schema（v4 完整性启动门禁）通过！
    rsetup_controller::check_identity_schema(&db)
        .await
        .expect("check_identity_schema v4 must succeed");
}

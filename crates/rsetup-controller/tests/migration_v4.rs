// v4 基线校验用例：测试库由操作员预先迁移到当前受支持版本 v4，本文件不再从空库或
// v3 基线升级（不调用 migrate-identity-test，也不调用 run_schema_v4_migration），
// 只连接当前 v4 库并做只读校验。
use rsetup_controller::{ControllerConfig, DbPool, TestMigrationConfig};

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

    // 1. schema_meta 存在且 singleton = 1, schema_version = 4（v4 为唯一基线，不做升级）
    let (singleton, version): (i32, i32) =
        sqlx::query_as("SELECT singleton, schema_version FROM schema_meta WHERE singleton = 1")
            .fetch_one(&db.0)
            .await
            .expect("read schema_meta");
    assert_eq!(singleton, 1);
    assert_eq!(version, 4);

    // 2. 数据库恰好包含 16 张表（11 identity + 5 runtime）
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()",
    )
    .fetch_one(&db.0)
    .await
    .expect("count tables");
    assert_eq!(count, 16, "v4 schema must have exactly 16 tables");

    // 3. 无任何外键定义
    let fk_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'FOREIGN KEY'",
    )
    .fetch_one(&db.0)
    .await
    .expect("count fk constraints");
    assert_eq!(fk_count, 0, "must have 0 foreign keys");

    // 4. 无任何 CHECK 约束
    let check_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND constraint_type = 'CHECK'",
    )
    .fetch_one(&db.0)
    .await
    .expect("count check constraints");
    assert_eq!(check_count, 0, "must have 0 check constraints");

    // 5. 校验 check_identity_schema（v4 完整性启动门禁）通过！
    rsetup_controller::check_identity_schema(&db)
        .await
        .expect("check_identity_schema v4 must succeed");
}

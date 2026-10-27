mod admission;
mod fresh;
mod migration;
mod pool;
mod probe;
mod production;
mod schema_parse;
mod schema_shape;
mod username;

pub use admission::AdmissionStore;
pub(crate) use admission::{next_event_seq, system_fallback_time_evidence};
pub use migration::TestMigrationConfig;
pub use pool::DbPool;
pub use probe::RuntimeTablePresence;
pub(crate) use probe::require_no_identity_foreign_keys;
pub use production::{
    MIGRATION_ACK_CONFIRMATION, ProductionMigrationPlan, check_identity_schema,
    migration_config_from_env, read_schema_version, run_production_migration,
};
pub use schema_parse::v4_schema_statements;
pub(crate) use username::valid_username;

/// v4 唯一基线 DDL（`schema/v4.sql`，恰好 16 张表、无 FOREIGN KEY、无 CHECK）。
///
/// 数据库 schema 只有 v4 一个基线：空库经显式受控入口直接建完整 v4，已是 v4 只做只读
/// 校验，其余任何版本或半成品状态一律拒绝——仓库中不再有 0001..0004 历史执行链，也没有
/// 「先建旧版再 ALTER」的升级路径。本常量对外可见只用于离线契约测试；运行期绝不把整段
/// SQL 当作一条语句执行，所有写入都用 [`v4_schema_statements`] 的白名单解析结果。
pub const V4_SCHEMA_SQL: &str = include_str!("../schema/v4.sql");

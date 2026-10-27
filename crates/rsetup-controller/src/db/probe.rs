use crate::ControllerError;
use sqlx::Row;

use super::pool::DbPool;
use super::schema_parse::{
    IndexMeta, ddl_parts, expected_indexes, v4_comment_form_check, v4_schema_body, v4_schema_error,
    validate_indexes,
};
use super::schema_shape::{
    ColumnMeta, FK_COLUMNS_QUERY, FK_CONSTRAINTS_QUERY, FK_REFERENCES_QUERY, RUNTIME_TABLES,
    STATISTICS_QUERY, TABLES, validate_column_charset, validate_column_shape, validate_fk_metadata,
    validate_object_names, validate_table_collation, validate_table_set,
};

/// v4 只读门禁探针：真实实现仅由 `DbPool` 提供，离线测试注入受控 fake 验证决策顺序。
/// 方法集合就是 v4 门禁的全部步骤，没有任何旧版本或续迁入口。
pub(super) trait IdentitySchemaProbe {
    fn read_version(
        &self,
    ) -> impl std::future::Future<Output = Result<Option<i32>, ControllerError>> + Send;
    /// v4 完整形状（16 表列/索引/字符集/无 CHECK）。
    fn validate_shape(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn validate_data(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    /// runtime 五表存在性快照：仅由固定字面量的 information_schema 探测填充。
    fn runtime_table_presence(
        &self,
    ) -> impl std::future::Future<Output = Result<RuntimeTablePresence, ControllerError>> + Send;
    /// runtime 数据只读扫描（期望版本取编译期 `SUPPORTED_SCHEMA_VERSION`）。
    fn validate_runtime_data(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    fn validate_fk(&self) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
}

/// v4 唯一门禁（普通启动与显式入口共用）：版本决策 → 16 表完整形状 →
/// runtime 五表齐备 → identity 数据 → runtime 数据 → 无 FK。期望版本恒为编译期
/// `SUPPORTED_SCHEMA_VERSION`，绝不从数据库行本身推断；任何缺失或漂移都 fail-closed。
pub(super) async fn check_identity_schema_with_probe(
    probe: &impl IdentitySchemaProbe,
) -> Result<(), ControllerError> {
    identity_schema_decision(probe.read_version().await?)?;
    probe.validate_shape().await?;
    let presence = probe.runtime_table_presence().await?;
    crate::integrity::validate_runtime_tables_for_version(
        crate::integrity::SUPPORTED_SCHEMA_VERSION,
        &presence,
    )?;
    probe.validate_data().await?;
    probe.validate_runtime_data().await?;
    probe.validate_fk().await
}

/// Schema v4 运行时五表的存在性快照：仅由固定字面量的 information_schema
/// 探测填充，绝不接受外部输入拼装的表名。`integrity::validate_runtime_tables_for_version`
/// 依据它判定「v4 必须五表齐备」。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuntimeTablePresence {
    pub main_tasks: bool,
    pub sub_tasks: bool,
    pub task_previews: bool,
    pub device_operation_locks: bool,
    pub recovery_checks: bool,
}

/// 普通启动版本决策：委托 `integrity::startup_guard_decision`，仅接受编译期
/// 配置 `SUPPORTED_SCHEMA_VERSION`（v4）。旧版本、未知版本、缺失元数据一律
/// `SchemaNotReady`；空库由显式入口的 `ProductionMigrationPlan::CreateV4` 处理。
fn identity_schema_decision(version: Option<i32>) -> Result<(), ControllerError> {
    crate::integrity::startup_guard_decision(version)
}

/// v4 无外键门禁：三条彼此独立的 information_schema 视角必须**同时**可信且为空。
/// 读失败/NULL/不完整一律视为"有外键或不可信"，绝不把错误当成空集。
pub(crate) async fn require_no_identity_foreign_keys(db: &DbPool) -> Result<(), ControllerError> {
    let constraints = sqlx::query(FK_CONSTRAINTS_QUERY)
        .fetch_optional(&db.0)
        .await
        .map_err(|_| ())
        .and_then(|row| {
            row.map(|row| {
                Ok((
                    row.try_get::<Option<String>, _>("table_name")
                        .map_err(|_| ())?,
                    row.try_get::<Option<String>, _>("constraint_name")
                        .map_err(|_| ())?,
                ))
            })
            .transpose()
        });
    let references: Result<Option<i64>, ()> = sqlx::query_scalar(FK_REFERENCES_QUERY)
        .fetch_one(&db.0)
        .await
        .map_err(|_| ());
    let columns: Result<Option<i64>, ()> = sqlx::query_scalar(FK_COLUMNS_QUERY)
        .fetch_one(&db.0)
        .await
        .map_err(|_| ());
    validate_fk_metadata(constraints, references, columns)
}

/// v4 完整形状校验（只读、无写入）：
/// 1. 表集合恰好是 11 identity + 5 runtime，未知表拒绝；
/// 2. 每张表的列名集合、逐列形状（类型/长度/UNSIGNED/NULL/字符集/collation/默认值）、
///    索引集合（名称、列顺序、唯一性、前缀长度）都必须与 `schema/v4.sql` 一致；
/// 3. 每张表都不得存在任何 CHECK 约束（v4 DDL 不声明 CHECK，出现即为漂移）；
///
/// 外键由 [`require_no_identity_foreign_keys`] 单独做全库校验，不在本函数内。
async fn validate_v4_shape(db: &DbPool) -> Result<(), ControllerError> {
    let present: Vec<String> =
        sqlx::query_scalar("SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE()")
            .fetch_all(&db.0).await?;
    validate_table_set(&present)?;
    // 形状解析（[`ddl_parts`]）与白名单解析（[`v4_schema_statements`]）消费同一份 DDL，
    // 两条路径都必须先过注释形态门禁：行内注释会让 [`ddl_parts_from`] 把注释文本并入下一个
    // 片段，从而报出"库里缺列"这种把源码缺陷伪装成数据库漂移的误导性诊断。
    v4_comment_form_check(&v4_schema_body())?;
    for &(table, columns, indexes) in TABLES.iter().chain(RUNTIME_TABLES) {
        let table_collation: String = sqlx::query_scalar("SELECT CAST(table_collation AS CHAR) AS table_collation FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = ?")
            .bind(table).fetch_one(&db.0).await?;
        validate_table_collation(table, &table_collation)?;
        let parts = ddl_parts(table);
        let actual_columns: Vec<String> = sqlx::query_scalar("SELECT CAST(column_name AS CHAR) AS column_name FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ?")
            .bind(table).fetch_all(&db.0).await?;
        validate_object_names(
            table,
            "column",
            &columns.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            &actual_columns,
        )?;
        for &column in columns {
            let declaration = parts
                .iter()
                .find_map(|part| part.strip_prefix(&format!("{column} ")))
                .ok_or_else(|| {
                    ControllerError::Config(format!(
                        "v4 schema DDL missing column {table}.{column}"
                    ))
                })?;
            let actual = read_column(db, table, column).await?;
            validate_column_shape(table, declaration, &actual)?;
            validate_column_charset(table, declaration, &actual, &table_collation)?;
        }
        let expected = expected_indexes(&parts, indexes)?;
        let rows = sqlx::query(STATISTICS_QUERY)
            .bind(table)
            .fetch_all(&db.0)
            .await?;
        let mut actual: Vec<IndexMeta> = Vec::new();
        for row in rows {
            let name: String = row.try_get("index_name")?;
            let column: String = row.try_get("column_name")?;
            let non_unique: i64 = row.try_get("non_unique")?;
            let sub_part: Option<i64> = row.try_get("sub_part")?;
            let seq: i64 = row.try_get("seq_in_index")?;
            if let Some(index) = actual.iter_mut().find(|entry| entry.name == name) {
                if seq != index.columns.len() as i64 + 1 || index.unique != (non_unique == 0) {
                    return Err(ControllerError::Config(format!(
                        "v4 schema incompatible index order {table}.{name}"
                    )));
                }
                index.columns.push(column);
                index.sub_parts.push(sub_part);
            } else {
                if seq != 1 {
                    return Err(ControllerError::Config(format!(
                        "v4 schema incompatible index order {table}.{name}"
                    )));
                }
                actual.push(IndexMeta {
                    name,
                    columns: vec![column],
                    unique: non_unique == 0,
                    sub_parts: vec![sub_part],
                });
            }
        }
        validate_indexes(table, &expected, &actual)?;
        // v4 DDL 不声明任何 CHECK 约束：实际存在一条即视为漂移，直接拒绝。
        let actual_checks: Vec<String> = sqlx::query_scalar("SELECT CAST(constraint_name AS CHAR) AS constraint_name FROM information_schema.table_constraints WHERE table_schema = DATABASE() AND table_name = ? AND constraint_type = 'CHECK'")
            .bind(table).fetch_all(&db.0).await?;
        validate_object_names(table, "check", &[], &actual_checks)?;
    }
    Ok(())
}

async fn read_column(
    db: &DbPool,
    table: &str,
    column: &str,
) -> Result<ColumnMeta, ControllerError> {
    let row = sqlx::query("SELECT CAST(data_type AS CHAR) AS data_type, CAST(column_type AS CHAR) AS column_type, CAST(is_nullable AS CHAR) AS is_nullable, CAST(character_set_name AS CHAR) AS character_set_name, CAST(collation_name AS CHAR) AS collation_name, CAST(column_default AS CHAR) AS column_default FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?")
        .bind(table).bind(column).fetch_optional(&db.0).await?
        .ok_or_else(|| ControllerError::Config(format!("migration missing column {table}.{column}")))?;
    Ok(ColumnMeta {
        name: column.to_owned(),
        data_type: row.try_get("data_type")?,
        column_type: row.try_get("column_type")?,
        nullable: parse_nullable_metadata(
            table,
            column,
            row.try_get::<Option<String>, _>("is_nullable")?.as_deref(),
        )?,
        character_set_name: row.try_get("character_set_name")?,
        collation_name: row.try_get("collation_name")?,
        default: row.try_get("column_default")?,
    })
}

fn parse_nullable_metadata(
    table: &str,
    column: &str,
    value: Option<&str>,
) -> Result<bool, ControllerError> {
    match value {
        Some("YES") => Ok(true),
        Some("NO") => Ok(false),
        _ => Err(ControllerError::Config(format!(
            "migration incompatible nullability metadata {table}.{column}"
        ))),
    }
}

pub(super) fn validate_false_default_metadata(
    table: &str,
    column: &str,
    default: Option<&str>,
) -> Result<(), ControllerError> {
    // BOOLEAN is a TINYINT alias; only numeric 0 is accepted here. Other
    // engine readback representations require independent verification.
    if default == Some("0") {
        Ok(())
    } else {
        Err(ControllerError::Config(format!(
            "migration incompatible default {table}.{column}"
        )))
    }
}

pub(super) async fn read_version(db: &DbPool) -> Result<Option<i32>, ControllerError> {
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name = 'schema_meta'")
        .fetch_one(&db.0).await?;
    if exists == 0 {
        return Ok(None);
    }
    if exists != 1 {
        return Err(ControllerError::Config(
            "damaged schema_meta table identity".into(),
        ));
    }
    let rows = sqlx::query("SELECT singleton, schema_version FROM schema_meta LIMIT 2")
        .fetch_all(&db.0)
        .await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] if row.try_get::<i8, _>("singleton")? == 1 => {
            Ok(Some(row.try_get("schema_version")?))
        }
        _ => Err(ControllerError::Config(
            "damaged schema_meta singleton".into(),
        )),
    }
}

impl IdentitySchemaProbe for DbPool {
    async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
        read_version(self).await
    }
    async fn validate_shape(&self) -> Result<(), ControllerError> {
        validate_v4_shape(self).await
    }
    async fn validate_data(&self) -> Result<(), ControllerError> {
        crate::integrity::check_identity_data(self).await
    }
    async fn runtime_table_presence(&self) -> Result<RuntimeTablePresence, ControllerError> {
        read_runtime_table_presence(self).await
    }
    async fn validate_runtime_data(&self) -> Result<(), ControllerError> {
        crate::integrity::check_runtime_data(self, crate::integrity::SUPPORTED_SCHEMA_VERSION).await
    }
    async fn validate_fk(&self) -> Result<(), ControllerError> {
        require_no_identity_foreign_keys(self).await
    }
}

// 保留薄根再导出路径 `crate::db::require_no_identity_foreign_keys` 的 crate 内引用，
// 满足 08-controller-db.md §4.1 清单「保路径/不降权」，并防止触发 unused_imports。
#[allow(dead_code)]
const _: () = {
    let _ = crate::db::require_no_identity_foreign_keys;
};

// 固定字面量的五表存在性探测：表名全部硬编码，绝不接受外部输入拼装。
const RUNTIME_TABLE_PRESENCE_QUERY: &str = "SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE() AND table_name IN ('main_tasks','sub_tasks','task_previews','device_operation_locks','recovery_checks')";

async fn read_runtime_table_presence(db: &DbPool) -> Result<RuntimeTablePresence, ControllerError> {
    let names: Vec<String> = sqlx::query_scalar(RUNTIME_TABLE_PRESENCE_QUERY)
        .fetch_all(&db.0)
        .await
        .map_err(|_| v4_schema_error("runtime table presence read failed"))?;
    let present = |name: &str| names.iter().any(|seen| seen == name);
    Ok(RuntimeTablePresence {
        main_tasks: present("main_tasks"),
        sub_tasks: present("sub_tasks"),
        task_previews: present("task_previews"),
        device_operation_locks: present("device_operation_locks"),
        recovery_checks: present("recovery_checks"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_startup_accepts_only_v4_and_rejects_old_or_unknown_versions() {
        // 普通启动期望版本来自编译期配置 = 4（`startup_guard_decision`）。
        assert!(identity_schema_decision(Some(4)).is_ok());
        // 缺失版本行或旧版一律拒绝普通启动；空库仅可显式初始化 v4，旧版无升级路线。
        for version in [None, Some(1), Some(2), Some(3)] {
            assert!(
                matches!(identity_schema_decision(version), Err(ControllerError::SchemaNotReady { found, required: 4 }) if found == version)
            );
        }
        // 未知版本（0、负数、v5+）同样 fail-closed，复用 SchemaNotReady 变体。
        for version in [Some(0), Some(5), Some(-1)] {
            assert!(
                matches!(identity_schema_decision(version), Err(ControllerError::SchemaNotReady { found, required: 4 }) if found == version)
            );
        }
    }
}

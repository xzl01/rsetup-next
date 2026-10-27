use crate::ControllerError;

use super::pool::DbPool;
use super::schema_parse::{v4_schema_error, v4_schema_statements};
use super::schema_shape::{RUNTIME_TABLES, TABLES};
use crate::db::probe::{IdentitySchemaProbe, check_identity_schema_with_probe};

/// 16 张 v4 表在元数据可见时必须全部为空（fresh 建库后置数据门禁）。
/// 表名只取自 [`TABLES`]/[`RUNTIME_TABLES`] 编译期常量，绝不来自数据库返回值。
async fn validate_v4_tables_are_empty(db: &DbPool) -> Result<(), ControllerError> {
    for &(table, ..) in TABLES.iter().chain(RUNTIME_TABLES) {
        let rows: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM `{table}`"))
            .fetch_one(&db.0)
            .await
            .map_err(|_| v4_schema_error("fresh schema data scan failed"))?;
        if rows != 0 {
            return Err(v4_schema_error("fresh schema must contain no rows"));
        }
    }
    Ok(())
}

/// v4 fresh 建库探针：在只读门禁探针之上补上唯一的写路径三件事
/// （按固定 DDL 建一张表、确认 16 表皆空、写入 v4 元数据行）。
/// 真实实现仅由 `DbPool` 提供；离线测试注入受控 fake 验证顺序与失败即停。
trait FreshV4Probe: IdentitySchemaProbe {
    /// 当前库中已存在的全部表名（information_schema 视角）。
    fn schema_tables(
        &self,
    ) -> impl std::future::Future<Output = Result<Vec<String>, ControllerError>> + Send;
    /// 执行一条白名单内的固定 CREATE 语句（DDL 非事务）。
    fn create_table(
        &self,
        statement: &str,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    /// 16 张表必须全部零行。
    fn validate_tables_are_empty(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
    /// 写入 `schema_meta` 单行：`singleton = 1`、`schema_version = 4`、未初始化。
    fn insert_v4_meta(
        &self,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;
}

/// 空库 → 完整 v4 的唯一建库流程（显式入口专用，普通启动路径绝不隐式调用）：
///
/// 1. 版本必须"不存在"——已有任何版本行（含 v4）都拒绝，绝不就地升级或续迁；
/// 2. **第一条 DDL 之前**必须确认库中零张表：部分建表、旧表、未知表一律拒绝，
///    因此本函数在任何拒绝路径上的写入次数恒为 0；
/// 3. 逐条执行 `schema/v4.sql` 的 16 条固定 CREATE，每条后复核已见表集合
///    恰为预期前缀（漂移即停，绝不自动修复）；
/// 4. 完整形状 + 无 FK + 16 表皆空；
/// 5. 写入 `schema_meta(schema_version = 4)`；结果不确定时绝不宣称成功；
/// 6. 复核版本落定为 4，再跑一次与普通启动完全相同的只读闭合门禁。
async fn create_v4_schema_with_probe(probe: &impl FreshV4Probe) -> Result<(), ControllerError> {
    if probe.read_version().await?.is_some() {
        return Err(v4_schema_error("fresh create requires an empty database"));
    }
    if !probe.schema_tables().await?.is_empty() {
        return Err(v4_schema_error(
            "fresh create requires zero existing tables before the first DDL",
        ));
    }
    let mut created: Vec<&'static str> = Vec::new();
    for (table, statement) in v4_schema_statements()? {
        probe.create_table(&statement).await?;
        created.push(table);
        let observed = probe.schema_tables().await?;
        let drift = observed.len() != created.len()
            || created
                .iter()
                .any(|name| !observed.iter().any(|seen| seen == name));
        if drift {
            return Err(v4_schema_error(
                "create result disagrees with schema metadata",
            ));
        }
    }
    probe.validate_shape().await?;
    probe.validate_fk().await?;
    probe.validate_tables_are_empty().await?;
    if probe.insert_v4_meta().await.is_err() {
        // 写入返回错误时结果本身不可信：只有读回**成功**并能证明版本行缺失或不是
        // 受支持版本，才允许固定为"被拒"；读回自身失败无法证明未提交，与"读回已是
        // 受支持版本"一样都只能报结果不确定。两种情形都绝不宣称成功、绝不自动重试。
        let outcome = match probe.read_version().await {
            Err(_) => "version write outcome uncertain",
            Ok(Some(crate::integrity::SUPPORTED_SCHEMA_VERSION)) => {
                "version write outcome uncertain"
            }
            Ok(_) => "version write rejected",
        };
        return Err(v4_schema_error(outcome));
    }
    if probe.read_version().await? != Some(crate::integrity::SUPPORTED_SCHEMA_VERSION) {
        return Err(v4_schema_error(
            "version readback is not the supported version",
        ));
    }
    check_identity_schema_with_probe(probe).await
}

impl FreshV4Probe for DbPool {
    async fn schema_tables(&self) -> Result<Vec<String>, ControllerError> {
        sqlx::query_scalar("SELECT CAST(table_name AS CHAR) AS table_name FROM information_schema.tables WHERE table_schema = DATABASE()")
            .fetch_all(&self.0)
            .await
            .map_err(|_| v4_schema_error("schema table presence read failed"))
    }
    async fn create_table(&self, statement: &str) -> Result<(), ControllerError> {
        sqlx::query(statement)
            .execute(&self.0)
            .await
            .map_err(|_| v4_schema_error("runtime DDL failed"))?;
        Ok(())
    }
    async fn validate_tables_are_empty(&self) -> Result<(), ControllerError> {
        validate_v4_tables_are_empty(self).await
    }
    async fn insert_v4_meta(&self) -> Result<(), ControllerError> {
        let result = sqlx::query("INSERT INTO schema_meta (singleton, schema_version, instance_id, initialized, authz_epoch, admin_guard_revision) VALUES (1, ?, ?, FALSE, 0, 0)")
            .bind(crate::integrity::SUPPORTED_SCHEMA_VERSION)
            .bind(uuid::Uuid::new_v4().as_bytes().as_slice())
            .execute(&self.0)
            .await
            .map_err(|_| v4_schema_error("version write rejected"))?;
        if result.rows_affected() != 1 {
            return Err(v4_schema_error("version write rejected"));
        }
        Ok(())
    }
}

/// 显式 v4 建库入口（仅由 `run_production_migration` 在门禁与身份复核之后调用）。
pub(super) async fn create_v4_schema(db: &DbPool) -> Result<(), ControllerError> {
    create_v4_schema_with_probe(db).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::probe::RuntimeTablePresence;

    /// v4 fresh 建库流程的受控 fake：把「已存在的表」「建表次数」「元数据写入次数」
    /// 「只读门禁次数」显式暴露成断言目标，用于验证顺序与失败即停。
    #[derive(Default)]
    struct FreshState {
        created: Vec<String>,
        creates: usize,
        meta_writes: usize,
        readonly_checks: usize,
        version: Option<i32>,
    }

    #[derive(Default)]
    struct FakeFresh {
        version: Option<i32>,
        existing: Vec<String>,
        create_error_at: Option<usize>,
        create_drift: bool,
        shape_error: bool,
        fk_error: bool,
        empty_error: bool,
        meta_error: bool,
        meta_lands_anyway: bool,
        /// true 时写入返回成功，但读回版本恒为 3（版本落定失败）。
        stuck_readback: bool,
        /// true 时写入之后的版本读回**自身失败**（连"没落定"都无法证明）。
        readback_error: bool,
        state: std::sync::Mutex<FreshState>,
    }

    impl IdentitySchemaProbe for FakeFresh {
        async fn read_version(&self) -> Result<Option<i32>, ControllerError> {
            let state = self.state.lock().unwrap();
            if state.meta_writes > 0 {
                if self.readback_error {
                    // 读回自身失败：既不能证明落定，也不能证明未落定。
                    return Err(v4_schema_error("injected readback failure"));
                }
                return Ok(if self.stuck_readback {
                    Some(3)
                } else {
                    state.version
                });
            }
            Ok(self.version)
        }
        async fn validate_shape(&self) -> Result<(), ControllerError> {
            if self.shape_error {
                return Err(v4_schema_error("injected shape drift"));
            }
            Ok(())
        }
        async fn validate_data(&self) -> Result<(), ControllerError> {
            self.state.lock().unwrap().readonly_checks += 1;
            Ok(())
        }
        async fn runtime_table_presence(&self) -> Result<RuntimeTablePresence, ControllerError> {
            Ok(full_presence())
        }
        async fn validate_runtime_data(&self) -> Result<(), ControllerError> {
            Ok(())
        }
        async fn validate_fk(&self) -> Result<(), ControllerError> {
            if self.fk_error {
                return Err(v4_schema_error("injected foreign key"));
            }
            Ok(())
        }
    }

    impl FreshV4Probe for FakeFresh {
        async fn schema_tables(&self) -> Result<Vec<String>, ControllerError> {
            let state = self.state.lock().unwrap();
            Ok(self
                .existing
                .iter()
                .cloned()
                .chain(state.created.iter().cloned())
                .collect())
        }
        async fn create_table(&self, statement: &str) -> Result<(), ControllerError> {
            let mut state = self.state.lock().unwrap();
            state.creates += 1;
            if self.create_error_at == Some(state.creates) {
                return Err(v4_schema_error("injected DDL failure"));
            }
            let table = statement
                .strip_prefix("CREATE TABLE IF NOT EXISTS ")
                .and_then(|rest| rest.split_once('('))
                .map(|(name, _)| name.trim())
                .ok_or_else(|| v4_schema_error("injected unparsable DDL"))?;
            if !self.create_drift {
                state.created.push(table.to_owned());
            }
            Ok(())
        }
        async fn validate_tables_are_empty(&self) -> Result<(), ControllerError> {
            if self.empty_error {
                return Err(v4_schema_error("injected nonempty table"));
            }
            Ok(())
        }
        async fn insert_v4_meta(&self) -> Result<(), ControllerError> {
            let mut state = self.state.lock().unwrap();
            state.meta_writes += 1;
            if self.meta_error {
                if self.meta_lands_anyway {
                    // 驱动报错但写入实际落定：结果不确定，绝不宣称成功。
                    state.version = Some(crate::integrity::SUPPORTED_SCHEMA_VERSION);
                }
                return Err(v4_schema_error("injected write failure"));
            }
            state.version = Some(crate::integrity::SUPPORTED_SCHEMA_VERSION);
            Ok(())
        }
    }

    #[tokio::test]
    async fn fresh_create_rejects_any_existing_version_before_touching_ddl() {
        for version in [
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(0),
            Some(-1),
        ] {
            let fake = FakeFresh {
                version,
                ..FakeFresh::default()
            };
            assert!(
                create_v4_schema_with_probe(&fake).await.is_err(),
                "已存在版本 {version:?} 必须拒绝建库"
            );
            let state = fake.state.lock().unwrap();
            assert_eq!(state.creates, 0, "拒绝路径必须零 DDL");
            assert_eq!(state.meta_writes, 0, "拒绝路径必须零元数据写入");
        }
    }

    #[tokio::test]
    async fn fresh_create_rejects_partial_schema_before_first_ddl() {
        // 部分建表 / 旧表残留 / 未知表：即便没有版本行也不允许"顺手补齐"。
        for existing in [
            vec!["users".to_owned()],
            vec!["schema_meta".to_owned()],
            vec!["main_tasks".to_owned(), "sub_tasks".to_owned()],
            vec!["legacy_users".to_owned()],
        ] {
            let fake = FakeFresh {
                existing,
                ..FakeFresh::default()
            };
            assert!(
                create_v4_schema_with_probe(&fake).await.is_err(),
                "非真正空库必须拒绝建库"
            );
            let state = fake.state.lock().unwrap();
            assert_eq!(state.creates, 0, "空库判定在首条 DDL 之前，拒绝必须零写入");
            assert_eq!(state.meta_writes, 0);
        }
    }

    #[tokio::test]
    async fn fresh_create_writes_meta_only_after_full_shape_and_empty_data_pass() {
        let fake = FakeFresh::default();
        create_v4_schema_with_probe(&fake)
            .await
            .expect("真正空库必须一次建满 v4");
        let state = fake.state.lock().unwrap();
        assert_eq!(state.creates, 16, "必须恰好执行 16 条 CREATE");
        assert_eq!(state.created.len(), 16, "16 张表都必须可见");
        assert_eq!(state.meta_writes, 1, "元数据只写一次");
        assert_eq!(
            state.version,
            Some(crate::integrity::SUPPORTED_SCHEMA_VERSION)
        );
        assert_eq!(state.readonly_checks, 1, "版本落定后必须跑一次只读闭合门禁");
    }

    #[tokio::test]
    async fn fresh_create_stops_at_first_failure_and_never_writes_meta() {
        // DDL 失败：立即返回，绝不自动修复或续跑。
        let fake = FakeFresh {
            create_error_at: Some(3),
            ..FakeFresh::default()
        };
        assert!(create_v4_schema_with_probe(&fake).await.is_err());
        assert_eq!(
            fake.state.lock().unwrap().meta_writes,
            0,
            "建表失败绝不写版本"
        );

        // 建表成功但元数据不承认（漂移）：同样停。
        let fake = FakeFresh {
            create_drift: true,
            ..FakeFresh::default()
        };
        assert!(create_v4_schema_with_probe(&fake).await.is_err());
        assert_eq!(
            fake.state.lock().unwrap().meta_writes,
            0,
            "建表结果与元数据不一致绝不写版本"
        );

        // 形状漂移 / 外键漂移 / 数据非空：都不写版本。
        for (shape, fk, empty) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            let fake = FakeFresh {
                shape_error: shape,
                fk_error: fk,
                empty_error: empty,
                ..FakeFresh::default()
            };
            assert!(create_v4_schema_with_probe(&fake).await.is_err());
            assert_eq!(
                fake.state.lock().unwrap().meta_writes,
                0,
                "shape={shape} fk={fk} empty={empty} 时绝不写版本"
            );
        }
    }

    #[tokio::test]
    async fn fresh_create_never_claims_success_when_meta_write_is_untrusted() {
        // 写入被拒：报未就绪，且不跑成功门禁。
        let fake = FakeFresh {
            meta_error: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("元数据写入被拒必须报错");
        assert!(
            error.to_string().contains("version write rejected"),
            "写入被拒必须显式报未就绪，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().readonly_checks,
            0,
            "写入失败绝不跑成功门禁"
        );

        // 写入结果不确定（报错但实际落定）：同样绝不宣称成功。
        let fake = FakeFresh {
            meta_error: true,
            meta_lands_anyway: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("写入结果不确定必须报错");
        assert!(
            error
                .to_string()
                .contains("version write outcome uncertain"),
            "不确定结果必须显式报不确定，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().readonly_checks,
            0,
            "不确定结果绝不跑成功门禁"
        );
    }

    #[tokio::test]
    async fn fresh_create_reports_uncertain_when_failed_write_cannot_be_read_back() {
        // 写入报错之后版本读回也失败：连"确实没落定"都证明不了，只能报结果不确定。
        let fake = FakeFresh {
            meta_error: true,
            readback_error: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("写入失败且读回失败必须报错");
        assert!(
            error
                .to_string()
                .contains("version write outcome uncertain"),
            "读回自身失败不能证明未提交，必须报不确定，实际：{error}"
        );
        let (meta_writes, readonly_checks) = {
            let state = fake.state.lock().unwrap();
            (state.meta_writes, state.readonly_checks)
        };
        assert_eq!(meta_writes, 1, "不确定结果绝不自动重试");
        assert_eq!(readonly_checks, 0, "不确定结果绝不跑成功门禁");

        // 对照：读回成功且能证明版本行不是受支持版本，才允许固定为"被拒"。
        let fake = FakeFresh {
            meta_error: true,
            stuck_readback: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("读回确证非受支持版本必须报错");
        assert!(
            error.to_string().contains("version write rejected"),
            "读回确证非受支持版本才可报被拒，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().meta_writes,
            1,
            "被拒同样绝不自动重试"
        );
    }

    #[tokio::test]
    async fn fresh_create_requires_supported_version_readback_before_closure_check() {
        let fake = FakeFresh {
            stuck_readback: true,
            ..FakeFresh::default()
        };
        let error = create_v4_schema_with_probe(&fake)
            .await
            .expect_err("读回版本不是受支持版本必须报错");
        assert!(
            error
                .to_string()
                .contains("version readback is not the supported version"),
            "读回漂移必须显式报错，实际：{error}"
        );
        assert_eq!(
            fake.state.lock().unwrap().readonly_checks,
            0,
            "读回漂移绝不跑闭合门禁"
        );
    }

    #[test]
    fn v4_schema_error_messages_are_fixed_ascii_without_identifiers() {
        let message = v4_schema_error("injected shape drift").to_string();
        assert!(message.is_ascii(), "诊断必须纯 ASCII：{message}");
        assert!(message.starts_with("configuration: v4 schema baseline rejected: "));
        for secret in ["mysql://", "@", "password", "BEGIN", "COMMIT", "ROLLBACK"] {
            assert!(!message.contains(secret), "诊断去敏失败：{message}");
        }
    }

    fn full_presence() -> RuntimeTablePresence {
        RuntimeTablePresence {
            main_tasks: true,
            sub_tasks: true,
            task_previews: true,
            device_operation_locks: true,
            recovery_checks: true,
        }
    }
}

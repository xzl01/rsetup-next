# 子规格 08：`crates/rsetup-controller/src/db.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 08 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码/测试、未编译（含最小 rustc 探针）、未运行测试、未读 `secret/`、未连接数据库、未提交。

撰写基线：分支 `dev-aghost`，头提交 `c7ff640`；实测 `git status` 当时 [src/db.rs](../../../../crates/rsetup-controller/src/db.rs) 与 [tests/v4_baseline.rs](../../../../crates/rsetup-controller/tests/v4_baseline.rs) 均为已修改未提交状态（总览 §3 并行改动注记）。本文全部行号为撰写会话对当时工作树 3,239 行文件的逐段实测（实测 `wc -l` = 3,239），实施时须按当时工作树重新核对。

**须由最终总览同步的登记项（本文不自行修改总览）**：总览 C6 规定各 crate 的 `tests/*.rs` 集成测试只作验证依据、不作重构对象；本规格的 §6 需要对 `tests/v4_baseline.rs` 做**源码收集接线**（把 db 侧扫描面从单文件等价扩展为 `src/db.rs` + `src/db/**/*.rs`），属该契约的窄例外。例外的边界：只改「读取哪些文件」，不改断言内容、token 表、计数值与拒绝语义，不重排或重写任何业务测试。总览 §4 的 08 号状态行与 §6.2 基线引用亦由总览维护方同步。

## 1. 实测现状与锚点

### 1.1 体量、公开面与消费方

- 全文件 3,239 行：实现区 1–1,695，测试区 1,696–3,239（`#[cfg(test)] mod tests` 1,696 起，`use super::*;`；实测 `#[test]` 31 个 + `#[tokio::test]` 9 个 = **40**，与总览 §3 快照「1,695 / 1,544」一致；全部离线、经 fake 不触库）。
- 模块声明与对外路径（拆分后必须逐字可用，C1）：
  - [lib.rs](../../../../crates/rsetup-controller/src/lib.rs) 第 8 行 `pub mod db;`、第 32 行 `pub use db::{AdmissionStore, DbPool, TestMigrationConfig, check_identity_schema};`——两行逐字不变。
  - `pub(crate)` 跨模块消费者（**不得降权限**，实测调用点）：`valid_username`（[authz/sqlx_repo.rs](../../../../crates/rsetup-controller/src/authz/sqlx_repo.rs) 249/2155、[auth/sqlx_repo.rs](../../../../crates/rsetup-controller/src/auth/sqlx_repo.rs) 594、[integrity.rs](../../../../crates/rsetup-controller/src/integrity.rs) 443）——[http_auth.rs](../../../../crates/rsetup-controller/src/http_auth.rs) 不是本项消费者：其 1167 调用的是本文件 1355 行的本地同名私有函数，33 行仅 import `DbPool`；`next_event_seq`（[audit.rs](../../../../crates/rsetup-controller/src/audit.rs) 25/97、auth/sqlx_repo.rs 531、[tasks/service.rs](../../../../crates/rsetup-controller/src/tasks/service.rs) 1595）；`system_fallback_time_evidence`（audit.rs 52、auth/sqlx_repo.rs 530、[transport/admission.rs](../../../../crates/rsetup-controller/src/transport/admission.rs) 58/224、tasks/service.rs 1308/1507）；`require_no_identity_foreign_keys`（实测仅 db.rs 内部使用，无外部消费者，权限仍不降）。
  - `pub` 且经 `crate::db::X` 路径消费（不随 lib.rs 再导出）：`v4_schema_statements`（[tests/v4_baseline.rs](../../../../crates/rsetup-controller/tests/v4_baseline.rs) 150/294）；`migration_config_from_env`/`read_schema_version`/`run_production_migration` + `db::{self}`（[src/bin/migrate-controller.rs](../../../../crates/rsetup-controller/src/bin/migrate-controller.rs) 25–26、210/229 用 `db::MIGRATION_ACK_CONFIRMATION`）；`RuntimeTablePresence`（integrity.rs 671/711/775/782，5 个 `pub` 字段直构）；`DbPool` 元组字段外部构造（[http_live.rs](../../../../crates/rsetup-controller/src/http_live.rs) 278、[tests/common/mod.rs](../../../../crates/rsetup-controller/tests/common/mod.rs) 891）；`TestMigrationConfig` 5 个 `pub` 字段集成测试字面量构造（tests/common/mod.rs 174/183–199）。
  - 仅类型/常量消费的 `DbPool`：authz/sqlx_repo.rs 10、auth/sqlx_repo.rs 101、admin_cli.rs 33、devices/repository.rs 11、http_auth.rs 33、transport/listener.rs 79、transport/manager.rs 73、integrity.rs 1、operations/retention.rs 41、http_live.rs 40。
- **源码文本门禁（本规格核心约束，§6 专章）**：`tests/v4_baseline.rs` 以 `read_source("src/db.rs")` 读源码文本做两类断言——`legacy_schema_carriers_are_gone`（76–103）对 6 个禁止 token 扫描；`startup_path_stays_read_only_and_the_entry_point_is_the_only_writer`（429–457）断言 `create_v4_schema(` 真实调用点**恰 1**（445 行过滤定义行与注释）。测试 crate 属独立 crate、只消费 `pub` 项；实现搬出 `src/db.rs` 后这两条断言的收集面必须等价扩展（§6），否则静默失效。

### 1.2 集成测试与预置库现状（仅验证依据，总览 C6）

- [tests/common/mod.rs](../../../../crates/rsetup-controller/tests/common/mod.rs)：`use rsetup_controller::{ControllerConfig, DbPool, TestMigrationConfig}`（10）、`TestMigrationConfig::from_test_env()`（14/827）、5 个 `pub` 字段字面量构造 `TestMigrationConfig { ..base.clone() }`（174/183–199）、`DbPool(fixture_ok(pool))` 元组构造（891）、`AdmissionStore::load/compare_and_set` 与 `RuntimeTablePresence` 相关断言（403/591/703/1090/1213/1344/1447）。它按 v4 基线预置开发库，真实 DB 轨道不默认运行（`#[ignore]`）。
- `tests/v4_baseline.rs` 实测 10 个 `#[test]`（全部离线：不连库、不读凭据），其中两条源码文本门禁见 §1.1 与 §6；其余 8 条（`v4_sql_*`、`sub_tasks_state_deadline_index_is_pinned_in_fresh_baseline`、`legacy_cli_modes_are_rejected_before_any_connection`、`non_utf8_argv_is_rejected_without_panic_or_echo`、`incomplete_gates_fail_fixed_before_connecting_and_never_echo_credentials`、`check_without_configuration_fails_without_echoing_anything_secret`、`help_documents_the_v4_only_contract`）以 `include_str!("../schema/v4.sql")`、`Cargo.toml` 与子进程 argv 为对象，**与 `src/db.rs` 路径无关、本规格零改动**。
- 其余真实库轨道（`mysql_identity`/`tidb_identity`/`v4_schema_readonly_verify`/`task_scheduler_sqlx`/`metrics_real_db`）只经 `tests/common` 消费 `pub` API，实现搬移对其透明（仅编译面路径不变即可）。

### 1.3 实现区职责锚点（实测行号区间）

- **池与入口** 4–10：`DbPool(pub sqlx::MySqlPool)`（5，元组字段 `pub` 保外部构造）+ `connect`（7–9，**零池参数**：`MySqlPool::connect(&config.database_url)` 原样，不调 max_connections 等）。
- **v4 契约与纯解析** 20–252 + 396–636：`V4_SCHEMA_SQL = include_str!("../schema/v4.sql")`（18；12–17 为其 doc 注释，12–18 整体留薄根，见 §3 薄根行与 §4.0）；`STATISTICS_QUERY`（20，异步形状侧消费）与 FK 三查询常量（21–23）；`TABLES` 11 表契约（24–147）；`RUNTIME_TABLES` 5 表（148–252）；`IndexMeta`（396）+ `validate_indexes`（403）+ `ddl_parts_from`（424）+ `v4_schema_body`（460）+ `v4_comment_form_check`（476）+ `v4_schema_statements`（539，唯一 `pub`）+ `parse_v4_statements`（546）+ `v4_schema_error`（585）+ `ddl_parts`（593）+ `expected_indexes`（597）。
- **形状校验纯函数** 254–263 + 276–395 + 938–1010：`ColumnMeta`（254，私有）+ `validate_column_shape`（277）/`validate_column_default`（346）/`validate_object_names`（381）；`FkConstraintsRead`（939）/`validate_fk_metadata`（941）/`require_no_identity_foreign_keys`（911，`pub(crate)`，911–935 归 probe 域，见探针域行）/FK 查询常量；`validate_table_collation`（958）/`validate_column_charset`（971）。
- **共享用户名规则** 264–275：`valid_username`（266，`pub(crate)`，前随 `#[allow(dead_code)]` 与注释，逐字携带）。
- **presence 与探针域** 638–688 + 716–721 + 911–935 + 1011–1090 + 1103–1201 + 1359–1374：`IdentitySchemaProbe` trait（638–658，6 个 RPITIT 方法）；`check_identity_schema_with_probe`（663–676，固定顺序）；`RuntimeTablePresence`（681–688，`pub` 字段保直构）；`validate_table_set`（692，692–714 归 schema_shape，见形状校验域行）/`identity_schema_decision`（719）；`read_column`（1104）/`parse_nullable_metadata`（1127）/`validate_false_default_metadata`（1141）/`validate_v4_shape`（1011）/`validate_v4_tables_are_empty`（1091，1091–1102 归 fresh，见 fresh 六步建库域行）；`impl IdentitySchemaProbe for DbPool`（1182–1201，单块，委托 `crate::integrity::check_identity_data`/`check_runtime_data`）；`RUNTIME_TABLE_PRESENCE_QUERY`（1359）/`read_runtime_table_presence`（1361）。
- **测试迁移授权域** 723–798 + 801–907：`TestMigrationConfig`（724–730，5 `pub` 字段）+ 手写 `Debug`（731–741，`test_url`/`expected_database`/`backup_ref` 显示 `[REDACTED]`）+ `#[cfg(test)] fn fixture`（743–752）+ `from_test_env`（755）+ `authorize`（772）；`authorize_test_migration`（776）；`schema_metadata_privilege_error`（794）/`grant_grantee_is_simple_account`（801）/`validate_schema_metadata_grants`（829）/`require_schema_metadata_privilege`（870）；泛型骨架 `run_authorized_migration_with`（888–907：声明名授权 → connect → **实际名二次授权** → privilege → upgrade）。
- **生产入口域** 1203–1356：`check_identity_schema`（1203，lib 再导出名）；`MIGRATION_ACK_CONFIRMATION`（1215）；`production_migration_error`（1219）/`production_migration_authorization_required`（1226，固定去敏合一）/`production_migration_config_from`（1234，复用共享授权器）/`migration_config_from_env`（1270）；`ProductionMigrationPlan`（1277，`CreateV4`/`VerifyOnly`）/`production_migration_plan`（1286：`None`→建库、`Some(4)`→只读、其余拒绝）/`run_production_migration_at`（1299，**唯一的 `create_v4_schema(` 调用点**，1302）/`run_production_migration`（1315，connect→`SELECT DATABASE()`→privilege→plan）/`read_schema_version`（1354）。
- **fresh 六步建库域** 1379–1491 + 1091–1102：`FreshV4Probe` trait（1379，继承 `IdentitySchemaProbe`）；`create_v4_schema_with_probe`（1409–1455，六步：版本必须不存在 → 首条 DDL 前零表 → 逐条 CREATE 且复核前缀 → 形状+FK+空表 → 写 meta（**失败三分支**：读回失败/读回已支持版本→`uncertain`，读回缺失→`rejected`，绝不宣称成功、绝不重试）→ 复核落定 4 → 跑与普通启动同一闭合门禁）；`impl FreshV4Probe for DbPool`（1457–1486）；`create_v4_schema`（1489–1491，薄封装，v4_baseline 计数的 1 个调用点即 1302）。
- **准入仓储域** 1493–1694：`AdmissionStore` trait（1493，lib 再导出名）；`decode_snapshot`（1509）/`next_snapshot`（1545，状态机 + `checked_add` revision）；`system_fallback_time_evidence`（1568，函数内 `static CLOCK_EPOCH: OnceLock` 唯一）；`next_event_seq`（1584，CAS 循环、溢出 `RevisionConflict`）；`impl AdmissionStore for DbPool`（1604–1684，`compare_and_set` 事务顺序：**begin → integrity guard → 可选 active admin → `SELECT ... FOR UPDATE` → revision/state 比对 → UPDATE 且 `changed == 1` → `admission_decisions` 插入 → `audit_events` 插入 → commit**；`PROCESS_EPOCH: OnceLock`/`EVENT_SEQ: AtomicU64` 为 `async fn` **函数体内静态**，全库唯一）；`audit_code`（1686）。

## 2. 行为与不变量（逐字保持，MUST NOT 借拆分改写）

1. **零池参数与脱敏**：`connect`/`run_production_migration` 的 `MySqlPool::connect` 形状、固定去敏错误文案（`production migration ...`、`v4_schema_error` 固定 ASCII）逐字不变；`TestMigrationConfig::Debug` 的 `[REDACTED]` 字段集不变。
2. **授权链路**：一律「声明库名 → 共享授权器校验 → connect → `SELECT DATABASE()` 实际名 → **同一授权器复核** → schema metadata privilege（`validate_schema_metadata_grants` 直连精确 GRANT 判定）→ plan」。无 **DB 迁移锁**原语：本模块没有任何锁表/咨询锁，隔离性仅由门禁断言（C2，不得虚构）。
3. **DDL 非事务**：`create_v4_schema_with_probe` 任一步失败即原样返回、绝不自动修复、绝不伪造成功；meta 写失败的三分支判定（§1.3）逐字保留；每条 DDL 后复核「已见表集合恰为预期前缀」。
4. **准入 CAS**：事务与语句顺序、`FOR UPDATE` 锁范围、`changed != 1 → RevisionConflict`、audit 双插入顺序不变；`PROCESS_EPOCH`/`EVENT_SEQ` 是函数内静态、全进程唯一，**不得移出函数、不得重建第二份**；`system_fallback_time_evidence` 的 `CLOCK_EPOCH: OnceLock` 同理唯一。
5. **无锁池、无 DB 迁移**：不存在迁移锁、重试或后台任务；拆分不引入任何新等待/超时。
6. `V4_SCHEMA_SQL` 的 `include_str!("../schema/v4.sql")`：本设计将其留在薄根 `db.rs`，相对路径逐字不改（若实施改入子文件，必须精确改写为 `../../../schema/v4.sql`，并以 §8-3 与 §9 风险 4 兜底；二选一、不得留断链）。
7. **契约表与校验器的配对**：`TABLES`（11 identity 表）/`RUNTIME_TABLES`（5 runtime 表）的「表名 + 列名清单 + 索引名清单」三元组与其消费方（`validate_table_set`/`validate_v4_shape`/`validate_v4_tables_are_empty`/`expected_indexes`）成对迁移，MUST NOT 为「消重」合并两张表或改写元组形状 `&[(&str, &[&str], &[&str])]`；表名插值进 `SELECT COUNT(*) FROM \`{table}\`` 的可信来源（编译期常量）不变。
8. **fail-closed 去敏文案**：`v4_schema_error`/`production_migration_*`/`schema_metadata_privilege_error` 的固定消息（不回显表名、库名、URL、GRANT 文本、驱动细节）逐字不变；错误变体（`Config`/`SchemaNotReady`/`RevisionConflict`/`NotFound`）选择不变。
9. **探测读序**：`check_identity_schema_with_probe` 的六步顺序（版本决策 → 形状 → runtime 五表 → identity 数据 → runtime 数据 → 无 FK）与「期望版本恒取编译期 `SUPPORTED_SCHEMA_VERSION`、绝不由库内行推断」不变；三条 FK 视角「读失败/NULL/不完整一律视为不可信」的判空逻辑不变。

## 3. 目标私有模块树（拟建，未创建）

```text
crates/rsetup-controller/src/db.rs             约 45   # 薄根：pub const V4_SCHEMA_SQL(include_str 原样) + pub mod 声明 + 再导出(§4)
crates/rsetup-controller/src/db/
  pool.rs            约 15   # 4–10：DbPool(pub 字段)+connect(零池参数)
  username.rs        约 35   # 264–275：valid_username(pub(crate)，含原 allow(dead_code) 与注释) + 1 测试(§5)
  schema_shape.rs    约 480  # 20–23(STATISTICS_QUERY 与 FK 三查询常量；12–18 V4_SCHEMA_SQL doc 注释与常量本体留薄根) 24–252(TABLES/RUNTIME_TABLES) 254–263 + 276–395(ColumnMeta+形状/默认值/对象名校验器) 692–714(validate_table_set) 938–1010(FkConstraintsRead/validate_fk_metadata/validate_table_collation/validate_column_charset) + 7 测试(§5)
  schema_parse.rs    约 490  # 396–636：IndexMeta/validate_indexes/ddl_parts(_from)/v4_schema_body/v4_comment_form_check/parse_v4_statements/v4_schema_statements(pub)/v4_schema_error/expected_indexes + 5 测试与 fixture(§5)
  probe.rs           约 225  # 638–688(IdentitySchemaProbe+编排+RuntimeTablePresence) + 716–721(identity_schema_decision) + 911–935(require_no_identity_foreign_keys) + 1011–1090 + 1103–1201(validate_v4_shape/read_column/impl for DbPool) + 1359–1374(presence 探测) + 1 测试(§5)
  fresh.rs           约 630  # 1091–1102(validate_v4_tables_are_empty) + 1379–1491：FreshV4Probe/六步流程/impl for DbPool/create_v4_schema + 8 测试与 FakeFresh/FreshState/full_presence(§5)
  migration.rs       约 430  # 723–798 + 801–907：TestMigrationConfig(5 pub 字段+Debug+fixture+from_test_env+authorize)/authorize_test_migration/schema_metadata_privilege_error/grant_grantee_is_simple_account/validate_schema_metadata_grants/require_schema_metadata_privilege/run_authorized_migration_with + 7 测试(§5)
  production.rs      约 340  # 1203–1356：check_identity_schema/MIGRATION_ACK_CONFIRMATION/门禁解析/ProductionMigrationPlan/run_production_migration_at(唯一 create_v4_schema 调用点)/run_production_migration/read_schema_version + 3 测试与 gate helpers(§5)
  admission.rs       约 385  # 1493–1694：AdmissionStore/decode/next_snapshot/fallback/next_event_seq/impl CAS/audit_code + 8 测试(§5)
```

**命名与声明约定**：薄根用 `src/db.rs` + `src/db/`（2018 edition 风格，非 `db/mod.rs`），薄根内按 §4.3 依赖序声明 `mod admission; mod fresh; mod migration; mod pool; mod probe; mod production; mod schema_parse; mod schema_shape; mod username;`（全部 private `mod`，无 `pub mod` 子模块）；每个子文件顶部保留原代码段的中文注释块（逐字随代码移动）；测试文件不单独建 `tests.rs` 汇总，一律 `<子文件>::tests`。**拆层依据**：v4 契约被切成「纯契约与形状谓词（schema_shape）/DDL 文本解析与索引期望（schema_parse）」两面，因为前者是 `information_schema` 读回值的判定谓词、后者是 `schema/v4.sql` 文本的白名单解析器，两者消费者不同（`validate_v4_shape` 用两面，`create_v4_schema_with_probe` 只用解析面）；探针域与被探的真实实现同文件（避免把 6 方法 trait 与其唯一真实 impl 拆到两个域造成读者跨文件拼接）；三条入口链（migration 授权骨架 → production 决策 → fresh DDL）各自独立成文件，使 §2 的授权链与 DDL 非事务边界在文件边界上可见。

行数为「实现 + 对应测试」的实测估算（各文件均 < 900，C4 无需例外申请）。不新增 crate、不改 `Cargo.toml`/`Cargo.lock`、不增删依赖；无新包，[Makefile](../../../../Makefile) `FMT_PACKAGES` 不动（总览 F1）。

## 4. 符号归属、再导出与可见性

### 4.0 对外可见符号归属表（既有对外可见项 → 目标子模块 → 新可见性 → 保持路径）

| 符号（原定义行） | 目标子模块 | 新可见性 | 保持的外部路径 |
| --- | --- | --- | --- |
| `DbPool`（5，含 `pub` 元组字段） | `db/pool.rs` | `pub` | `crate::db::DbPool`、`rsetup_controller::db::DbPool`、lib.rs 32、`DbPool(..)` 直构 |
| `DbPool::connect`（7） | `db/pool.rs` | `pub` | 同上 |
| `V4_SCHEMA_SQL`（18） | 薄根 `db.rs` | `pub` | `crate::db::V4_SCHEMA_SQL`（`include_str!` 相对路径零改动） |
| `v4_schema_statements`（539） | `db/schema_parse.rs` | `pub` | `rsetup_controller::db::v4_schema_statements`（v4_baseline 150/294） |
| `RuntimeTablePresence`（682，5 `pub` 字段） | `db/probe.rs` | `pub` | `crate::db::RuntimeTablePresence`、integrity.rs 671/711/775/782 |
| `TestMigrationConfig`（724，5 `pub` 字段）+`Debug`+`from_test_env`+`authorize` | `db/migration.rs` | `pub` | lib.rs 32、`tests/common/mod.rs` 10/14/174/827/891 |
| `MIGRATION_ACK_CONFIRMATION`（1215） | `db/production.rs` | `pub` | `db::MIGRATION_ACK_CONFIRMATION`（migrate-controller.rs 210/229） |
| `ProductionMigrationPlan`（1277） | `db/production.rs` | `pub` | `crate::db::ProductionMigrationPlan` |
| `check_identity_schema`（1203） | `db/production.rs` | `pub` | lib.rs 32、`crate::db::check_identity_schema`（main.rs 启动路径） |
| `migration_config_from_env`（1270） | `db/production.rs` | `pub` | migrate-controller.rs 25 |
| `run_production_migration`（1315） | `db/production.rs` | `pub` | migrate-controller.rs 25 |
| `read_schema_version`（1354） | `db/production.rs` | `pub` | migrate-controller.rs 25 |
| `AdmissionStore`（1493） | `db/admission.rs` | `pub` | lib.rs 32、`tests/common/mod.rs` 多处 |
| `valid_username`（266） | `db/username.rs` | `pub(crate)` | `crate::db::valid_username`（authz/auth/integrity；http_auth.rs 用的是其本文件 1355 行本地同名私有函数，非本项消费者） |
| `next_event_seq`（1584） | `db/admission.rs` | `pub(crate)` | `crate::db::next_event_seq`（audit/auth/tasks） |
| `system_fallback_time_evidence`（1568） | `db/admission.rs` | `pub(crate)` | `crate::db::system_fallback_time_evidence`（audit/auth/transport/tasks） |
| `require_no_identity_foreign_keys`（911） | `db/probe.rs` | `pub(crate)` | `crate::db::require_no_identity_foreign_keys`（无外部消费者，权限不降） |

本模块**没有** `authz/sqlx_repo.rs` 那样的跨模块 test fixture（04 号 `sql_shape`）：实测 `db.rs` 的测试区只被本文件消费，外部仅 `tests/v4_baseline.rs` 以源码文本消费（§6）。因此目录化不产生「外部测试设施路径必须逐字保持」的额外约束，约束集中在 §4.1 的路径面与 §6 的扫描面。

### 4.1 薄根再导出清单（`db.rs`，逐条对应现路径）

- `pub use`（原 `pub` 定义才可 `pub use`，父 re-export 不扩大子定义权限）：`AdmissionStore`、`DbPool`（含 `pub` 元组字段语义不变）、`RuntimeTablePresence`、`TestMigrationConfig`、`ProductionMigrationPlan`、`MIGRATION_ACK_CONFIRMATION`、`v4_schema_statements`、`check_identity_schema`、`migration_config_from_env`、`run_production_migration`、`read_schema_version`。
- `pub(crate) use`（既有外部消费者，不降）：`valid_username`、`next_event_seq`、`system_fallback_time_evidence`。
- `pub(crate) use`（保路径/不降权，1 项）：`require_no_identity_foreign_keys`——实测 grep 全仓仅 db.rs 内部使用、**无外部消费者**；按 C1「既有可见性不得收窄」与 `crate::db::X` 路径保持原 `pub(crate)` 与薄根再导出，不属「既有外部消费者」类。
- **无 dead-code 保活项**：grep 逐项核查，db.rs 全部 private 项在生产区（1–1,695 行）均有调用点，不存在生产侧零调用、仅测试消费的 private 项。`identity_schema_decision`（719）在 666 行被 `check_identity_schema_with_probe` 生产路径调用、与定义同在 probe 域，保持 private；`validate_table_set`（692）在 1015 行被 `validate_v4_shape` 生产调用但跨文件，属 §4.2 域内 `pub(super)` 提权问题而非存活问题。原稿「private 定义 + 薄根 `pub(crate) use` 保活」的写法**不可编译**（薄根是父模块，`pub(crate) use` 无法指称子文件里 private 的项），该机制已删除；薄根 MUST NOT 为此新增 `#[allow(dead_code)]` 或再导出。
- 模块外路径逐字不变：`crate::db::X`、`rsetup_controller::db::X`（migrate-controller.rs、v4_baseline.rs、common/mod.rs）、lib.rs 32 行 4 个再导出名。原 `pub(crate)` 项一律 `pub(crate) use`，MUST NOT 升为 `pub`；`pub` 符号即使无生产消费者也不收窄（C1）。

### 4.2 可见性变更清单（默认 private；仅两类偏离）

- **保持原权限**（含权限声明逐字随代码移动）：`pub` 11 项与 `pub(crate)` 4 项见 §4.1；`RuntimeTablePresence` 5 字段、`TestMigrationConfig` 5 字段、`DbPool` 1 字段（`pub` 元组字段）权限不变；保持 private 的代表性项：`validate_column_default`(346)、`ddl_parts_from`(424)、`parse_v4_statements`(546)、`identity_schema_decision`(719)、`schema_metadata_privilege_error`(794)、`grant_grantee_is_simple_account`(801)、`validate_schema_metadata_grants`(829)、`validate_v4_shape`(1011)、`validate_v4_tables_are_empty`(1091)、`read_column`(1104)、`parse_nullable_metadata`(1127)、`read_runtime_table_presence`(1361)、`RUNTIME_TABLE_PRESENCE_QUERY`(1359)、`FreshV4Probe`(1379)、`create_v4_schema_with_probe`(1409)、`production_migration_*`(1219–1299)、`decode_snapshot`/`next_snapshot`/`audit_code`、`TestMigrationConfig::fixture`（`#[cfg(test)]`，实测仅 migration 测试使用）、`full_presence`(3230，实测仅 fresh 测试 1965 使用)。判定依据：其全部消费点（含将被搬入的测试）与定义在拆分后同文件。
- **由 private 升为 `pub(super)`**（28 项，全部为 `db` 私有子树内跨文件消费、对外零新增 `pub`）。**`pub(super)` 论证**：每个域文件都以私有 `mod` 声明在薄根内，`crate::db::<域>::X` 路径对 `db` 子树之外本就不可达，故子树内 `pub(super)` 与 `pub(crate)` 的**有效可见性集完全等值**，两者均不新增对外可见面；本设计一律取最小书写 `pub(super)`。**「子文件 private 定义 + 薄根 re-export」不是合法形态**：薄根 `db.rs` 是各域文件的父模块，`pub(crate) use` 无法指称子文件里 private 的项（不可编译），故一切真实跨文件消费必须逐项提权；且 `pub(super)` 项 MUST NOT 在薄根再导出——再导出会把其有效可见性从 `db` 子树扩到整个 crate（新增 crate 内路径）。消费方一律 `use crate::db::<域>::X;`（兄弟域及其 `mod tests` 内同样可达）。
  - **表一：域内新增提权（23 项）**——格式：消费者为「行号→拆分后归属域」，行号均为当前工作树 db.rs 实测：

    | # | 符号（含随类型一并提权的字段） | 定义行→归属域 | 跨文件消费者→消费域 |
    | --- | --- | --- | --- |
    | 1 | `IdentitySchemaProbe`（trait） | 638→probe | `FreshV4Probe: IdentitySchemaProbe` 超 trait 界 1379→fresh；fake 实现 1938→fresh::tests |
    | 2 | `IndexMeta` + 4 个私有字段（name/columns/unique/sub_parts） | 396→schema_parse | `validate_v4_shape` 的 `Vec` 声明 1051、字面量构造 1072 与字段读 1058–1059→probe；字面量构造 2841→schema_shape::tests |
    | 3 | `ColumnMeta` + 7 个私有字段（name/data_type/column_type/nullable/character_set_name/collation_name/default） | 254→schema_shape | `read_column` 返回类型 1108、全字段字面量构造 1112–1124→probe；fixture 字面量构造/返回类型 2949/3014→schema_parse::tests |
    | 4 | `TABLES` | 24→schema_shape | 548/578→schema_parse（`parse_v4_statements`）；1020→probe；1092→fresh；1740/2932/3053→schema_parse::tests |
    | 5 | `RUNTIME_TABLES` | 148→schema_shape | 551/578→schema_parse；1020→probe；1092→fresh；1741/2932/3053→schema_parse::tests |
    | 6 | `STATISTICS_QUERY` | 20→schema_shape | 1047→probe（`validate_v4_shape` 索引读回） |
    | 7 | `FK_CONSTRAINTS_QUERY` | 21→schema_shape | 912→probe（`require_no_identity_foreign_keys`） |
    | 8 | `FK_REFERENCES_QUERY` | 22→schema_shape | 927→probe |
    | 9 | `FK_COLUMNS_QUERY` | 23→schema_shape | 931→probe |
    | 10 | `validate_table_set` | 692→schema_shape | `validate_v4_shape` 生产调用 1015→probe（可见性问题，非存活问题，见 §4.1/§9 风险 6） |
    | 11 | `validate_object_names` | 381→schema_shape | 1027/1084→probe |
    | 12 | `validate_column_shape` | 277→schema_shape | 1043→probe；3077/3087/3121/3129/3138→schema_parse::tests（fixture 与 16 表自洽网） |
    | 13 | `validate_column_charset` | 971→schema_shape | 1044→probe；3080/3139→schema_parse::tests |
    | 14 | `validate_table_collation` | 958→schema_shape | 1023→probe |
    | 15 | `validate_fk_metadata` 及其 `constraints` 参数类型别名 `FkConstraintsRead`（随函数一并提权，避免 `private_interfaces` 告警红线；返回类型即 `Result<(), ControllerError>`，审查清单所称「返回类型」实为参数类型） | 941/939→schema_shape | 935→probe（`require_no_identity_foreign_keys` 的返回表达式） |
    | 16 | `validate_false_default_metadata` | 1141→probe | 371→schema_shape（`validate_column_default` 的返回表达式）。审查清单未列，本轮 grep 复查补录 |
    | 17 | `v4_comment_form_check` | 476→schema_parse | 1019→probe（`validate_v4_shape` 前置注释形态门禁） |
    | 18 | `v4_schema_body` | 460→schema_parse | 1019→probe |
    | 19 | `ddl_parts` | 593→schema_parse | 1024→probe；2839/2876→schema_shape::tests |
    | 20 | `expected_indexes` | 597→schema_parse | 1046→probe；2877→schema_shape::tests |
    | 21 | `validate_indexes` | 403→schema_parse | 1080→probe；2847–2851→schema_shape::tests |
    | 22 | `run_authorized_migration_with` | 888→migration | 1316→production（`run_production_migration` 复用授权骨架） |
    | 23 | `require_schema_metadata_privilege` | 870→migration | 1343→production（`run_production_migration` 闭包内） |

  - **表二：原五项经复核成立，合并去重后 5 项**（与表一逐项比对：`v4_schema_error`、`authorize_test_migration` 属跨文件消费，本应即在本清单口径内，已并入表二统一计数，不与表一重复；`read_version`/`check_identity_schema_with_probe`/`create_v4_schema` 与表一零重叠）：

    | # | 符号 | 定义行→归属域 | 跨文件消费者→消费域 |
    | --- | --- | --- | --- |
    | 24 | `read_version` | 1157→probe | 1300→production（`run_production_migration_at`）、1355→production（`read_schema_version`）；1184 的调用在 probe 域 `impl IdentitySchemaProbe` 内，同域 |
    | 25 | `check_identity_schema_with_probe` | 663→probe | 1204→production（`check_identity_schema` 直调）、1454→fresh（六步末位闭合门禁）。1301（`VerifyOnly` 分支）调的是 production 域内同文件的 `pub` 包装 `check_identity_schema`，不是 with_probe 直调。原稿写作 `pub(crate)`，本轮统一为 `pub(super)`（等值，且避免 `pub(crate)` 签名引用 `pub(super)` 参数 trait 的 lint 风险） |
    | 26 | `create_v4_schema` | 1489→fresh | 1302→production 唯一调用点；**不删薄封装**，否则 §6 计数断言对象消失 |
    | 27 | `v4_schema_error` | 585→schema_parse | 1365→probe（`read_runtime_table_presence`）；1096/1098→fresh（`validate_v4_tables_are_empty`）、1411–1450（`create_v4_schema_with_probe`）、1462/1468/1480/1482（`impl FreshV4Probe for DbPool`）。原稿把 1096/1098 误记为「schema_shape 空表扫描/FK 侧」，本轮更正真实归属 |
    | 28 | `authorize_test_migration` | 776→migration | 1254→production（`production_migration_config_from` 复用同一共享授权器，是「声明名/实际名同一套判据」不变量的实现前提）；2374/2376→production::tests |

  - 提权合计 **28 项**（表一 23 + 表二 5；第 2/3 行的 struct 字段随类型计入所在行，字段共 11 个）；另有两项经核实**不成立/需修正**：`parse_nullable_metadata`(1127) 唯一调用 1116 在 `read_column` 内、与定义同在 probe 域，测试区零引用，**保持 private 不提权**（审查清单误列）；`validate_false_default_metadata`(1141) 为清单外补录（表一第 16 行）。上述均不新增对外 `pub`；若某项经核实实际无需跨文件（实施时编译器证明），保持 private 并回改。`TestMigrationConfig::from_test_env` 自身是 `pub` 成员方法，随类型迁入 migration.rs 后仍 `pub`，不需再导出即可经 `db::TestMigrationConfig::from_test_env` 使用（`tests/common/mod.rs` 14/827）。

### 4.3 依赖方向（单向、无环）

`username`、`pool` 为叶子；`schema_shape` → `pool`（FK/授权查询走 `db.0`）；`schema_parse` 不依赖兄弟；`probe` → `schema_shape`/`schema_parse`（`validate_v4_shape` 消费 `ddl_parts`/`expected_indexes`/`STATISTICS_QUERY`）；`fresh` → `probe`（`FreshV4Probe: IdentitySchemaProbe`、闭合门禁）；`production` → `probe`/`fresh`/`migration`；`migration` 不依赖 `production`（`MIGRATION_ACK_CONFIRMATION` 文本一致性由 `production.rs::tests` 域内消费（2277/2372）并跨域调用共享授权器 `authorize_test_migration`（2374/2376）验证，migration 域零引用 `production`，不构成生产侧反向依赖——若实施发现需要生产侧引用，则方向改记并复查无环）；`admission` → `pool`（另经 `crate::integrity`/`crate::validate_device_fields` 跨出模块树）；`db.rs` 薄根仅声明与再导出。**依赖核查**：`impl IdentitySchemaProbe for DbPool` 与 `impl FreshV4Probe for DbPool` 各为单一 impl 块，分别留在 probe.rs / fresh.rs（与所在 trait 同域），不因委托拆块。

## 5. 测试与 fixture 迁移映射（40 → 40，逐条，不整搬巨型 tests.rs）

原则（C3）：断言与被测行为同时位；各子文件私有 `mod tests` 以 `use super::*;` + 显式 `use crate::db::{...}` 承接原 glob 语义；测试名逐字不变，仅模块路径由 `db::tests::X` 变为 `db::<域>::tests::X`。fixture 保持 test-only、不晚于首消费者落地。

**test-only fixture 清单（全部随被测域同批落地，MUST NOT 进入非 test 编译面）**：`FakeFresh`（1921，`#[derive(Default)]` 的受控 fake，持有 `version`/`existing`/`create_error_at`/`create_drift`/`shape_error`/`fk_error`/`empty_error`/`meta_error`/`meta_lands_anyway`/`stuck_readback`/`readback_error` 等注入旗标与 `Mutex<FreshState>`，实现两个 probe trait，覆盖「失败即停」「结果不确定」「读回漂移」三类注入点）、`FreshState`（1912，`created`/`creates`/`meta_writes`/`readonly_checks`/`version` 的调用序列断言状态）、`full_presence`（3230，`RuntimeTablePresence` 五真构造）、`ideal_metadata_from_declaration`（2944，从 DDL 片段推导 `ColumnMeta` 期望）、`production_gate_env`/`production_gate_from`/`production_gate_error_is_fixed`（2257/2289/2300，环境变量注入式门禁夹具，避免并行改真实进程环境）、`TestMigrationConfig::fixture`（743，`#[cfg(test)]` 关联函数）。其中 `FakeFresh` 同时实现 `IdentitySchemaProbe` 与 `FreshV4Probe`，是 probe/fresh 两域必须能相互可见（§4.2 表一 `IdentitySchemaProbe`、表二 `check_identity_schema_with_probe` 行）与 fresh 域必须自带 presence 夹具（§4.2 `full_presence` 保持 private）的直接依据。

- **`username.rs::tests`（1）**：`username_rules_reject_noncanonical_input`(2855) → 被测 `valid_username` 随本文件。
- **`schema_shape.rs::tests`（7）**：`table_set_requires_exactly_the_sixteen_v4_tables`(1882 → `validate_table_set`+`TABLES`/`RUNTIME_TABLES`)、`closed_identity_metadata_rejects_extra_or_missing_objects`(2819 → `validate_object_names`/`validate_indexes`/`IndexMeta`)、`inline_primary_key_uses_column_name_not_type_width`(2875 → `ddl_parts`+`expected_indexes`)、`same_width_signed_counter_is_incompatible`(2883 → `validate_column_shape`+`ColumnMeta`)、`migration_checks_all_columns_including_audit_evidence`(2906 → `TABLES` 契约覆盖)、`fk_metadata_requires_three_independent_trustworthy_empty_views`(2453)、`fk_metadata_errors_never_echo_names_or_query_details`(2482)（→ `validate_fk_metadata`，实测两测试均不引用 `require_no_identity_foreign_keys` 的异步包装）。
- **`schema_parse.rs::tests`（5）**：`v4_baseline_sql_is_sixteen_tables_without_checks_or_foreign_keys`(1736 → `v4_schema_statements`)、`v4_baseline_parser_rejects_unknown_tables_and_constraints`(1765)、`v4_schema_comments_must_be_whole_line_only`(1813 → `v4_comment_form_check`+`v4_schema_body`)、`v4_baseline_sql_declares_every_contracted_table`(2929 → `v4_schema_statements`)、`v4_schema_contract_is_self_consistent_for_all_sixteen_tables`(3045)。fixture：`ideal_metadata_from_declaration`(2944–3044，构造 `ColumnMeta` 全字段并调用形状校验器)。
- **`probe.rs::tests`（1）**：`normal_startup_accepts_only_v4_and_rejects_old_or_unknown_versions`(2504 → `identity_schema_decision`)。
- **`fresh.rs::tests`（8）**：`fresh_create_rejects_any_existing_version_before_touching_ddl`(2026)、`fresh_create_rejects_partial_schema_before_first_ddl`(2051)、`fresh_create_writes_meta_only_after_full_shape_and_empty_data_pass`(2074)、`fresh_create_stops_at_first_failure_and_never_writes_meta`(2091)、`fresh_create_never_claims_success_when_meta_write_is_untrusted`(2138)、`fresh_create_reports_uncertain_when_failed_write_cannot_be_read_back`(2180)、`fresh_create_requires_supported_version_readback_before_closure_check`(2224)、`v4_schema_error_messages_are_fixed_ascii_without_identifiers`(2246，被测工厂 `v4_schema_error` 定义在 schema_parse，跨文件提权见 §4.2 表二对应行；用例本体与注入点在本域)。fixtures（全部 test-only、随本文件）：`FreshState`(1912)、`FakeFresh`(1921，同实现 `IdentitySchemaProbe`+`FreshV4Probe`，1938–2023)、`full_presence`(3230)。
- **`migration.rs::tests`（7）**：`schema_metadata_grants_require_exact_direct_schema_all`(2394)、`test_migration_needs_all_independent_confirmations`(2522)、`explicitly_authorized_unprefixed_dev_database_is_allowed`(2535)、`test_migration_database_name_accepts_64_ascii_chars_but_rejects_65`(2579)、`test_migration_config_debug_hides_fake_password`(2592)、`unauthorized_command_never_invokes_upgrade`(2609)、`every_failed_authorization_blocks_connection_or_upgrade`(2628)，共 7 条：2522/2535/2579 覆盖 `authorize`(772) 成员入口与库名规则，2394 覆盖 privilege 形状，2592 覆盖 `Debug` 去敏，2609/2628 验证 `run_authorized_migration_with` 的「授权失败即不 connect、不 upgrade」；helpers（test-only、随本文件）：`TestMigrationConfig::fixture`(743)。
- **`production.rs::tests`（3）**：`production_migration_plan_accepts_only_empty_database_and_v4`(1703 → `production_migration_plan`)、`production_migration_gate_rejects_every_incomplete_gate_set`(2322 → `production_migration_config_from`+`migration_config_from_env`，经本域 gate helper)、`production_gate_and_shared_authorizer_accept_the_same_ack`(2364 → 本域 `MIGRATION_ACK_CONFIRMATION` 对共享授权器，并经 §4.2 表二 `authorize_test_migration` 行跨域调用共享授权器)。helpers（test-only、随本文件）：`production_gate_env`(2257)、`production_gate_from`(2289)、`production_gate_error_is_fixed`(2300)——本轮复查从原稿「随 migration.rs」更正为随 production.rs：三者内部调用 `production_migration_config_from`(1234)，且唯一消费者是 2322 用例（2324/2358/2359），migration 测试零引用，置于 migration.rs 会形成反向跨文件依赖。
- **`admission.rs::tests`（8）**：`decode_snapshot_rejects_invalid_pairs_even_when_values_are_known`(2709)、`audit_sequence_never_wraps`(2733 → `next_event_seq`)、`max_admission_revision_rejects_transition_without_wrap`(2741)、`admission_revision_crosses_signed_boundary`(2755)、`admission_decision_rejects_illegal_transition`(2766)、`denied_device_requires_reopen_before_approval`(2782)、`admission_audit_codes_are_stable_and_neutral`(2795 → `audit_code`)、`fallback_evidence_has_stable_reference_and_unknown_fields`(3215 → `system_fallback_time_evidence`)。
- **计数总口径（DoD 核对式）**：1 + 7 + 5 + 1 + 8 + 7 + 3 + 8 = **40**；`#[tokio::test]` 9 = fresh 7 + migration 2（2609/2628），其余 31 为 `#[test]`，迁移前后各类型分别相等。若实施时对个别「双域被测」用例（如 2364/3045）定档到另一文件，只允许在两文件之间移动、总数与名单逐项不变。
- **筛选前缀的变化（§8-3 的核对口径）**：现 `cargo test -p rsetup-controller -- db::tests::` 命中 40；拆分后按域变为 `db::username::tests::`（1）、`db::schema_shape::tests::`（7）、`db::schema_parse::tests::`（5）、`db::probe::tests::`（1）、`db::fresh::tests::`（8）、`db::migration::tests::`（7）、`db::production::tests::`（3）、`db::admission::tests::`（8），故回归入口改用父前缀 `-- db::`；用例如 `next_event_seq` 的溢出用例由 `db::tests::audit_sequence_never_wraps` 变为 `db::admission::tests::audit_sequence_never_wraps`，**末段函数名逐字不变**。
- **测试体内跨域引用写法**：原 `use super::*;` 在子文件中解析不到其它兄弟域，需要显式绝对路径（例：`production.rs::tests` 的 2364 引 `MIGRATION_ACK_CONFIRMATION` 用 `use super::MIGRATION_ACK_CONFIRMATION;`（同文件）或 `use crate::db::MIGRATION_ACK_CONFIRMATION;`；`fresh.rs::tests` 的 `FakeFresh` 引兄弟域 probe 的 `IdentitySchemaProbe`（probe.rs 定义）用 `use crate::db::probe::IdentitySchemaProbe;`）。此类改动属 `use` 接线，断言文本零改动（C2/C3）。

## 6. 测试接线：v4_baseline 扫描合同的等价扩展（必要等价维护）

`tests/v4_baseline.rs` 是本模块唯一按源码文本设门禁的集成测试；实现搬入 `src/db/**/*.rs` 后，**同一原断言**必须作用于「`src/db.rs` + `src/db/**/*.rs` 全部生产实现文件」。本节为总览 C6「集成测试不作为重构对象」的**窄例外登记项**（只改源码收集接线、不重构业务测试、不减弱断言），须由最终总览同步登记。

1. **收集器**（新增测试辅助，返回绝对路径升序去重集合）：读 `manifest_dir()/src/db.rs`（缺失即 panic，消息 `missing src/db.rs root`）；递归遍历 `manifest_dir()/src/db/`（目录不存在返回空集），收集全部 `*.rs`（含 `mod.rs` 与嵌套子目录，**目录枚举天然不漏 scan**）；对每个收集到的文件 `read_to_string` 后按路径排序 `concatenate`。
2. **禁止 token 扫描（`legacy_schema_carriers_are_gone`）**：把 `("src/db.rs", &[TestMigrationMode, check_v3_ready, run_identity_test_migration, create_historical_v, 0001_identity_devices.sql, 0004_tasks.sql])` 的条目来源改为上述收集器（token 表逐字不变、拒绝语义不变：任一文件命中即失败并报告命中文件）；`src/integrity.rs` 与 `Cargo.toml`/`src/lib.rs` 断言原样不动。
3. **`create_v4_schema` 恰 1 调用（`startup_path_...only_writer`）**：`main.rs` 两条断言与 `schema/v4.sql` 存在性断言逐字不变；db 侧对收集器输出的**全部行**执行原过滤器 `line.contains("create_v4_schema(") && !line.contains("async fn ")` 并要求 `count == 1`。过滤正确性核对：定义行 `async fn create_v4_schema(db: &DbPool)` 含 `async fn ` 被滤除；唯一真实调用 `ProductionMigrationPlan::CreateV4 => create_v4_schema(db).await,` 计入 1；文档/模块注释对本函数的提及一律不带左括号（实测现状如此），故不计入且**无需改注释**；薄根 `db.rs` 只再导出名字、MUST NOT 写 `create_v4_schema(...)` 形式注释或占位调用。**不保留假调用**：薄封装 `create_v4_schema`（fresh.rs）与其唯一调用点（production.rs）原样保留，不新增第二调用、不为凑数补注释。
4. **反查与自检**：收集器结果非空、必含 `src/db.rs`；MUST NOT 采用「人工挑文件」的固定清单——若实施选择显式注册清单（备选），MUST 同测试内用目录枚举反查（注册清单 == 枚举结果）防新增文件漏检；本设计**默认目录枚举**，从不错过 scan。
5. **收集面扩展的边界**：`#[cfg(test)] mod tests` 内的 `create_v4_schema_with_probe` 调用在 `src/db/**/*.rs` 中同样被逐行扫描，但目标串为 `create_v4_schema_with_probe(`（带后缀、无左括号紧邻歧义：`contains("create_v4_schema(")` 不匹配 `create_v4_schema_with_probe(`），因此不会把测试 fake 调用计入恰 1 断言——接线后 MUST 实测该计数仍为 1。
6. **等价性验收**：接线改动仅限新增收集器函数与替换 db 侧读取来源（`main.rs`、`Cargo.toml`、`src/lib.rs`、`src/integrity.rs`、`schema/v4.sql` 的断言一律不动），断言条数、token 表、计数值（`== 1`）、拒绝语义零变化；`cargo test -p rsetup-controller --test v4_baseline --locked` 的 10 个用例（§1.2 名单）名称与数量不变。
7. **不新增门禁、不改门禁工具**：不新增 Makefile 目标、不改 CI 接线（总览 F8），因此不触发 `scripts/tests/controller-ci-contract.test.mjs` 的 CI 接线合同；本节的唯一落点是 `tests/v4_baseline.rs` 的源码收集接线。
8. **第三探测点等价维护（实施批次E补录）**：实施批次E发现并等价维护了规格未登记的第三个源码文本探测点——`crates/rsetup-controller/src/audit.rs` 的 `legacy_audit_writer_sources_remain_unchanged` 测试 `include_str` 探测（原指向 `src/db.rs` 的 `static PROCESS_EPOCH`/`EVENT_SEQ`/`INSERT INTO audit_events` 三断言），随 admission 域迁移将探测路径同步为 `db/admission.rs`，断言文本零增减（提交 `9c28dfc`）；该探测点与 `v4_baseline.rs` 并列登记。

## 7. 分批顺序与回滚（每批设计为可编译，本轮一律未编译验证）

单提交单批、逐批可独立 `git revert`；「暂留」= 仍留在薄根/上一文件的代码与其配套 `use`，「回改点」= 下一批收尾清理。

1. **批 A（骨架 + 扫描合同同批接线）**：`db.rs` 目录化为 `db.rs`(薄根) + `db/`，先抽 `pool.rs`、`username.rs`；同批修改 `tests/v4_baseline.rs` 收集器（§6-1），此时枚举结果 = `src/db.rs` + `db/pool.rs` + `db/username.rs`，两条门禁即刻等价。**暂留**：其余全部实现与除随 username.rs 本批落域的 1 条外的 39 条测试仍在薄根。**回改点**：无。
2. **批 B（schema 域）**：抽 `schema_shape.rs`（含 `validate_table_set`）与 `schema_parse.rs`，同批迁移 §5 的 7 + 5 = 12 条测试与 `ideal_metadata_from_declaration` fixture；薄根内剩余实现经 `use` 引入本作用域，其余 27 条测试（40 − username 1 − schema 12）暂留薄根并继续经 `use super::*` 解析（此时薄根仍是单文件，扫描收集器已含 `db/` 两文件，门禁等价）。**接线点（批 B）**：§4.2 表一第 2–15、17–21 行（schema 域类型/契约常量/校验器及其被 probe、fresh、schema_shape↔schema_parse 互消费面）与表二 `v4_schema_error` 行（定义 585 随 schema_parse）在本批随抽取提为 `pub(super)`；表一第 16 行 `validate_false_default_metadata` 定义属 probe 域（1141），本批预建 `probe.rs` 承载其为首份（同为 `pub(super)`），probe 域其余代码批 C 并入同文件——薄根作为父模块，暂留实现同样经 `use crate::db::<域>::X;` 消费已提权子项，暂留薄根的项不为消费方再导出或提权。**回改点（末批）**：薄根 `use` 收敛。
3. **批 C（probe 与 fresh 域）**：抽 `probe.rs`（trait + 编排 + presence + 异步 I/O + 单一 `impl` + 1 测试）与 `fresh.rs`（trait + 六步 + `impl` + `create_v4_schema` 薄封装 + 8 测试 + `FakeFresh`/`FreshState`/`full_presence`）。**暂留**：薄根只剩 migration/production/admission 与 `V4_SCHEMA_SQL`。**接线点**：§4.2 表一第 1 行（`IdentitySchemaProbe`）与表二 `read_version`/`check_identity_schema_with_probe`/`create_v4_schema` 提权在本批完成（dead-code 防护再导出已随 §4.1 更正取消，本批不存在该项）；§6-5 的调用点计数复核在本批完成——本批结束后定义在 `db/fresh.rs`（定义行含 `async fn ` 被原过滤器滤除），唯一真实调用点暂留薄根、批 D 移入 `db/production.rs`，收集器计数仍恰 1 且验证不漏 scan。
4. **批 D（migration 与 production 域）**：抽 `migration.rs`（`TestMigrationConfig::fixture` 保持 private）与 `production.rs`（test gate helpers 随 `production.rs`，见 §5），迁移两侧合计 10 测试（migration 7 + production 3）与 gate helpers。**接线点（批 D）**：§4.2 表一第 22/23 行（`run_authorized_migration_with`/`require_schema_metadata_privilege`）与表二 `authorize_test_migration` 行随抽取提权，`production.rs` 抽取后对三者形成兄弟域间的真实跨文件消费。**暂留**：薄根只剩 admission + `V4_SCHEMA_SQL` + 再导出。
5. **批 E（admission 收尾）**：抽 `admission.rs`（含 8 测试）；薄根收敛为 §3 约 45 行（`V4_SCHEMA_SQL` + `mod` 声明 + §4.1 再导出）。**回改点**：清中间批临时 glob 与暂留 `use`，只留 §4.1 最终清单；此后无暂留实现、无待回改 `use`。
6. **每批的编译前自检（实施会话执行，本规格不代跑）**：`make fmt-check` + `cargo clippy -p rsetup-controller --all-targets -- -D warnings`（F2 前提）+ `cargo test -p rsetup-controller --locked -- db::` 计数 40 + `--test v4_baseline` 10 用例；任一批出现 dead-code/未再导出即停在该批修复，不得带入下一批。
7. **回滚**：每批单提交，回滚即 `git revert`；批 A 的测试接线与骨架同批（避免「目录已建、扫描未扩」的中间态窗口）；无行为开关、无数据迁移、无 feature flag。

## 8. 验证命令（登记项，本轮一律未执行、不声称通过）

前提说明：`cargo` 侧命令均要求 web dist 在位（总览 F2：`rsetup-controller` 的 build.rs 在 `apps/controller-web/dist` 缺失时直接失败，先 `make web-dist-check` 或 `make web-build`）、工具链为 stable、`--locked` 属 cargo 参数（置于 `--` 之前）；F3 的「offline」只表示不初始化/不触碰数据库，干净检出下 `web-dist-check` 仍可能执行 `npm ci` 与 vite 构建（可 `NPM_CI_FLAGS=--offline` 复用预热缓存），依赖下载同样可能需要网络。DB 只影响第 6 条授权轨道：该轨道仅需按引擎逐条授权的专用开发库，本轮**未验证**且本任务**不得执行**。真实包名 `rsetup-controller`。

1. `make fmt-check`（F1 口径，禁用 `cargo fmt --all`）。
2. `cargo clippy -p rsetup-controller --all-targets -- -D warnings`。
3. `cargo test -p rsetup-controller --locked`；定向 `-- db::` 应命中 40 个用例（名称不变、路径按 §5 变化，逐条对照）；`cargo test -p rsetup-controller --test v4_baseline --locked` 覆盖 §6 扫描合同（用例数不变）。
4. 跨模块回归（`pub(crate)` 提权面）：`cargo test -p rsetup-controller --locked -- integrity::`、`-- audit::`、`-- authz::sqlx_repo::`、`-- transport::`、`-- tasks::`（覆盖 §1.1 的 `valid_username`/`next_event_seq`/`system_fallback_time_evidence` 调用点）。
5. 离线全量出口：`make controller-test-offline`（F2/F3）。
6. 真库轨道（总览 F6，**仅登记**：需操作员按引擎逐条授权专用开发库；本轮未运行、结果未验证，禁止未授权执行）：`cargo test -p rsetup-controller --locked --test mysql_identity -- --ignored`、`--test tidb_identity -- --ignored`、`--test v4_schema_readonly_verify -- --ignored`（`tests/common` 预置 v4 库；`TestMigrationConfig` 5 `pub` 字段字面量构造与 `DbPool` 元组构造是这些轨道的编译面）。

## 9. 模块专属风险与 DoD

- **风险 1（扫描合同失效，最高）**：§6 未接线或收集面只覆盖部分文件 → 禁止 token 扫描与「恰 1 调用」静默退化。DoD：批 A 同批接线、§6-6 核对。
- **风险 2（权限面）**：`valid_username`/`next_event_seq`/`system_fallback_time_evidence`/`require_no_identity_foreign_keys` 降权限即破坏 integrity/auth/authz/audit/tasks/transport；`DbPool` 元组字段、`TestMigrationConfig` 5 字段、`RuntimeTablePresence` 5 字段降权限即破坏 http_live 与集成测试编译面。DoD：§4.1/§4.2 清单逐项 + §8-4 定向。
- **风险 3（静态唯一性）**：`PROCESS_EPOCH`/`EVENT_SEQ`/`CLOCK_EPOCH` 若在搬移中被「提取为模块级静态」或复制，将改变进程内 epoch 语义或产生双份计数器。DoD：三处 `static` 仍在原函数体内（`grep -c` 位置核对），非新增。
- **风险 4（include_str 路径）**：`V4_SCHEMA_SQL` 留薄根则相对路径零改动；若移子文件必须精确改 `../../../schema/v4.sql`，由 `v4_baseline` 的 `include_str!("../schema/v4.sql")` 与 §8-3 兜底。
- **风险 5（impl 块唯一性）**：`IdentitySchemaProbe`/`FreshV4Probe`/`AdmissionStore` 各 `impl ... for DbPool` 为单一块整体移动（同 04 号子规格 §4.1 结论：一个类型对一个 trait 只能有一个 impl 块），不做「方法体分散实现」，也不借委托改写返回表达式。
- **风险 6（dead-code 与 clippy 红线）**：grep 逐项复查确认，db.rs 全部 private 项在生产区（1–1,695 行）均有调用点，不存在生产侧零调用、需要保活的项（原稿「两条 dead-code 防护再导出」的前提不实，且「private 定义 + 薄根 `pub(crate) use`」写法不可编译，已删除；`identity_schema_decision` 有 666 行生产调用保持 private，`validate_table_set` 有 1015 行生产调用、跨文件、归 §4.2 表一第 10 行）。拆分后若漏提某项 `pub(super)` 表现为编译失败（路径不可解析），属停批即修事件；MUST NOT 用 `#[allow(dead_code)]` 掩盖拆分导致的真实断链，MUST NOT 为保活提为 `pub`（否则违反 C1「不得新增 pub」）。
- **风险 7（跨模块测试契约）**：`tests/common` 与真实库轨道、`integrity.rs` 对 `RuntimeTablePresence` 5 字段的直构、`http_live.rs`/`common` 的 `DbPool(..)` 元组构造共同构成编译面回归网；任何再导出遗漏表现为编译失败（可接受），任何可见性越界由 §4.2 清单审查拦截。
- **阈值（C4）**：§3 各文件估算均 ≤ 900 行（最大 `fresh.rs` 约 630）。实施后若 `schema_shape.rs`（契约表 + 形状校验器 + 7 条测试 + 122 列/41 索引自洽性网）或 `fresh.rs` 超 900 且 ≤ 1,500，按 C4 在实现 PR 描述中提交内聚性论证（契约表与其自洽性测试、六步 DDL 流程与其 fake 必须同位）；> 1,500 才申请例外——本设计预计不发生。
- **DoD（全部满足方为完成）**：§4.1 路径与 lib.rs 8/32 行逐字一致；§5 映射 40 → 40（31/9 分类计数相等、名称零改写、断言零删改）；§6 两条门禁经收集器等价扩展且计数仍为 1；§2 九项不变量 diff 复核为零（`git diff --color-moved` 纯移动核对）；可见性变更严格限于 §4.2 清单 28 项 `pub(super)`（struct 字段随类型、共 11 个；既有 4 项 `pub(crate)` 不降、11 项 `pub use` 不变；编译器证明不需要者回退 private）；§8 命令按前提执行并如实报告（含未执行项）；未跑真库不声称真库通过；每批回滚点可用。

### 9.1 C1–C9 逐条自检

| 契约 | 本设计的落实方式 | 自检方式 |
| --- | --- | --- |
| C1 结构与可见性 | 仅 crate 内目录化；`lib.rs` 8/32 行不变；`pub use` 11 项不变；`pub(crate) use` 4 项不降（3 项既有外部消费者、1 项保路径/不降权）；域内提权 28 项全为 `pub(super)`（含随类型的 struct 字段 11 个），子树内等值 `pub(crate)`、无新增 `pub` | §4.1/§4.2 清单逐项 + 编译 |
| C2 行为与协议 | SQL 文本、事务边界、`FOR UPDATE` 锁范围、DDL 非事务、授权链路、去敏文案全部锁定 | §2 九项 + `git diff` 零改写 |
| C3 测试迁移 | 40 → 40 按被测符号逐条同位；fixture 保持 `#[cfg(test)]`、不晚于首消费者；无整搬 `tests.rs` | §5 名单计数 + `-- db::` 用例名对照 |
| C4 规模与阈值 | 最大文件约 630 行，无例外申请 | §3 估算 + 实施后 `wc -l` |
| C5 ui/app.js | 不适用（本模块非 `ui/`） | — |
| C6 范围排除 | 仅 `tests/v4_baseline.rs` 源码收集接线这一窄例外，业务测试与断言不重构、不减弱；须由最终总览同步登记 | §6 + 总览登记项 |
| C7 秘密与证据 | 本文无凭据、无 `secret/` 内容、未连库；示例命令去敏 | 全文检索确认 |
| C8 最低内容 | 状态/基线 §前言、锚点 §1、目标树 §3、符号表 §4、依赖 §4.3、测试 §5、批次回滚 §7、命令 §8、风险 DoD §9 | 逐节存在即通过 |
| C9 流程 | 单份子规格、只写文档、不改源码/测试、不提交 | 本任务声明 |

## 10. 本轮交付与未交付

- 已交付：本文一份（08 号子规格，状态：待用户审阅，尚未实施）。
- 未交付（本任务边界内刻意不做）：`src/db.rs` 与 `db/` 子树的任何源码改动；`tests/v4_baseline.rs` 收集器接线的实际落地（§6 为设计 + 必要维护登记）；任何编译（含最小 rustc 探针）、测试执行、`make` 目标运行；任何真库/`--ignored` 轨道执行；`Cargo.toml`/`Cargo.lock`/Makefile/CI 改动；总览 §4 中 08 号状态行的同步（由总览维护方按 §6 窄例外一并更新）。

## 11. 修订记录

- **2026-10-26 审查第 2 轮（已修订，待复核；不称 PASS）**：仅改可见性与计数口径，九文件域划分骨架不变；行号按当前工作树（db.rs 3,239 行，含既有未提交改动）逐项 read/grep 复核。
  - **C1**：§4.2「提权仅 5 项」严重失实，全部重写为「既有 4 项 `pub(crate)` 不降 + 11 项 `pub use` 不变 + 域内 `pub(super)` 提权 28 项」；写明薄根为父模块、不存在「子文件 private 定义 + 薄根再导出」合法形态，真实跨文件消费须逐项提权，且 `pub(super)` 在 `db` 私有子树内等值 `pub(crate)`、不新增对外可见面。新增表一 23 项 + 表二 5 项，逐项附定义行→归属域与消费者行→消费域；原五项经复核全部成立、与表一零重叠（`v4_schema_error`/`authorize_test_migration` 按新清单口径并入表二统一计数，无重复提权）。清单项修正两处：`parse_nullable_metadata` 经核实唯一调用 1116 与定义 1127 同在 probe 域、保持 private 不提权；`validate_false_default_metadata`(1141→probe) 为清单外补录（371→schema_shape 消费）。§4.1/§9 DoD/§9.1 C1 行同步。
  - **I1**：dead-code 保活前提不实，删除「private 定义 + 薄根 `pub(crate) use` 保活」机制（父模块无法 `pub(crate) use` 子文件 private 项，不可编译）；`identity_schema_decision`(719) 在 666 行被 `check_identity_schema_with_probe` 生产路径调用、消费同在 probe 域，保持 private 并移出保活清单；`validate_table_set` 在 1015 行被 `validate_v4_shape` 生产调用但跨文件，归 C1 可见性问题；grep 复查确认不存在生产侧零调用项（§4.1/§9 风险 6 明确写无）。
  - **I2**：§5 分域标称数更正 schema_shape 9→7、migration 9→7、production 4→3（与各域实际列名清单及全文件 40 测试分布核对一致）；总算式改 1+7+5+1+8+7+3+8=40；删除 migration「另 2 条」含混句；gate helpers（2257/2289/2300）从「随 migration.rs」更正为随 production.rs（内部调用 `production_migration_config_from`，唯一消费者为 2322 用例）；§5 筛选前缀计数与 §7 批 B–D 接线描述同步（12 条/暂留 27 条/10 条，接线点按 §4.2 表行引用）。
  - **M1**：§1.1 删除「另 http_auth.rs:1167 经 import 调用」——1167 调用的是 http_auth.rs 本文件 1355 行本地同名私有函数，该文件 33 行仅 import `DbPool`；真实消费者（authz 249/2155、auth 594、integrity 443）与 `pub(crate)` 不降结论保留。
  - **M2**：`require_no_identity_foreign_keys` 的 §4.1 类目从「既有外部消费者」改为「保路径/不降权」（grep 全仓核实无外部消费者）。
  - **M3**：§3 `probe.rs` 区间改为「638–688 + 716–721」，消除与 `validate_table_set`(692–714，schema_shape 归属) 的重叠。
  - **M4**：§4.2 `check_identity_schema_with_probe` 项依据更正为 fresh 1454 与 production 1204 跨文件直调 with_probe（1301 的 `VerifyOnly` 分支调的是同域 `pub` 包装 `check_identity_schema`），可见性书写统一为 `pub(super)`；`v4_schema_error` 项依据更正为真实跨文件消费 probe 1365 与 fresh 1096/1098/1411–1450/1462/1468/1480/1482（原稿把 1096/1098 误记为 schema_shape 空表扫描/FK 侧）。

- **2026-10-26 审查第 3 轮（已修订，待复核；不称 PASS）**：上轮复核已关闭 28 项清单、无保活、`pub(super)` 论证与 40 测试名单；本轮仅修 4 处文本残留，不改域划分、清单口径与批次顺序。
  - **F1**：§3 目标树三处分域标称随 I2（§5）同步——`schema_shape.rs`「+ 9 测试」→「+ 7 测试」、`migration.rs`「+ 9 测试与 gate helpers」→「+ 7 测试」、`production.rs`「+ 4 测试」→「+ 3 测试与 gate helpers」（gate helpers 2257/2289/2300 定档随 `production.rs`，与 §5 一致）。修后 §3 树内合计 1+7+5+1+8+7+3+8=**40**，与 §5 总口径及 §7 批次分布（A 1 / B 12 / C 9 / D 10 / E 8）逐项一致。
  - **M2**：区间标注去重叠（取精确化方案，归属仍以括号名单与 §4.2 为准）：§3 `probe.rs`「1011–1201」→「1011–1090 + 1103–1201」（`validate_v4_tables_are_empty` 1091–1102 归 fresh）；§3 `schema_shape.rs`「254–395」→「254–263 + 276–395」（`valid_username` 264–275 归 username）；§1.3 探针域「1354–1374」→「1359–1374」（`read_schema_version` 1354–1356 归 production）。
  - **M3**：§4.2「`DbPool` 0 字段权限不变」→「`DbPool` 1 字段（`pub` 元组字段）权限不变」，与 §1.1/§4.0 及 `DbPool(pub sqlx::MySqlPool)` 实测形状一致。
  - **M4**：§4.3 `migration` 不依赖 `production` 的论据更正——原「文本一致性由测试侧 `use super::super::production::` 引入」在 gate helpers 定档随 `production.rs` 后已不成立；改为由 `production.rs::tests` 域内消费 `MIGRATION_ACK_CONFIRMATION`（实测 2277/2372）并跨域调用共享授权器 `authorize_test_migration`（2374/2376）验证，migration 域零引用 `production`，无环结论不变。
  - **补修两处残留（已修订，待复核）**：§9 阈值处 `schema_shape.rs`「9 条测试」→「7 条测试」（与 §3 行58 及 §5 的 7 条口径一致）；§1.3 形状校验纯函数锚点「254–395」→「254–263 + 276–395」（`valid_username` 264–275 归 username，与 §3 口径一致）。
  - **复核5条残留补修（已修订，待复核）**：§7 批 A 暂留句「全部 40 测试」→「除随 username.rs 本批落域的 1 条外的 39 条测试」，与批 B 算式「40 − username 1 − schema 12 = 27」及本记录批次分布「A 1 / B 12 / C 9 / D 10 / E 8」三者互斥消除；§3 `schema_shape.rs` 区间「12–23」→「20–23」（12–17 doc 注释与 18 `V4_SCHEMA_SQL` 常量本体留薄根，与 §4.0 一致），§1.3「12–252」→「20–252」并就地注明 12–18 留薄根；§1.3 探针域区间随 §3 `probe.rs` 行（第 2 轮 M3 与第 3 轮 M2 的口径）同步为「638–688 + 716–721 + 911–935 + 1011–1090 + 1103–1201 + 1359–1374」，列举内注明 `validate_table_set`(692–714) 归 schema_shape、`validate_v4_tables_are_empty`(1091–1102) 归 fresh；§1.3 测试迁移授权域「723–936」→「723–798 + 801–907」（911–935 归 probe，与 §3 `migration.rs` 行一致）；§1.3 形状校验域列举注明 `require_no_identity_foreign_keys`(911) 归 probe 域（911–935，原属探针域区间）。

实施登记（补录，2026-10-28；批次实施期间未逐批追记，现据 git 历史补录）：
- `7b7bd9a` refactor(controller): 拆分 db pool 与 username 域并等价扩展基线扫描（批次 A，审查 PASS）
- `73c605e` refactor(controller): 拆分 db schema 契约与形状校验至 db/schema_shape（批次 B，审查 PASS）
- `f60f510` refactor(controller): 拆分 db schema 解析至 db/schema_parse（批次 B2，审查 PASS）
- `4d8cc2f` refactor(controller): 拆分 db probe 探针与 fresh 建库域至 db/probe、fresh（批次 C，审查 PASS）
- `e6b35da` fix(controller): 收敛 db 批次 C 清单外可见性并修正注释归属（批次 C 修复，审查 PASS）
- `036c5fe` refactor(controller): 拆分 db 迁移授权与生产迁移入口至 db/migration、production（批次 D，审查 PASS）
- `9c28dfc` refactor(controller): 拆分 db admission 域并完成 db 模块树收敛（批次 E，审查 PASS）
- `0b8bd9d` docs(controller): 同步 audit 探测注释路径并追记规格第三探测点（08 收尾，审查 PASS）


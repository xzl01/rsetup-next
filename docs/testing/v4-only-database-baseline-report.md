# v4-only 数据库基线（Task 1）收缩与离线回归报告

日期：2026-10-24 · 范围：`docs/superpowers/plans/2026-10-23-v4-only-database-baseline.md`
的 **Task 1（源码与离线测试收缩）**。本计划是本基线唯一的计划引用来源（仓库内不存在更早的
`2026-09-07-controller-v5.md`，早先版本的本报告误引，已在 §10.2 记为 F4 并修正）。

## 0. 一句话结论

数据库 schema 只有一个基线：**v4**。空库经显式入口直接建满 16 张表的 v4；已是 v4 只做只读
闭合校验；**其它一切版本在首次写入之前被拒绝**。v1–v3 的升级/续迁路线、旧版本夹具、逐条
SQL 的迁移目录与测试专用迁移 CLI 已全部移除，离线门禁与离线测试全绿。

## 1. 授权边界与"未触碰真实数据库"声明

- **未连接任何数据库**：本任务所有验证命令都不需要也不读取 `CONTROLLER_DATABASE_URL`；
  连接型用例（`tests/mysql_identity.rs`、`tests/tidb_identity.rs`、`tests/metrics_real_db.rs`、
  `tests/acceptance_runtime.rs`、`tests/v4_schema_readonly_verify.rs`）全部保持 `#[ignore]`，
  本轮共 **38 个 ignored 用例一个都没运行**。
- **未读取 `secret/`，未回显任何凭据**；新增的 CLI 去敏用例只使用编译期假口令
  `Fa1ePassw0rd-never-real`，并断言它从不出现在任何输出里。
- **未执行任何真实重建/迁移**：没有跑过 `--apply`，没有对任何库建表或改表。因此本报告中的
  "空库建满 v4"是**离线 fake 探针**验证的语义，不是真实 TiDB 上发生过的事实。真实重建属于
  Task 2/3，需操作员外部授权。
- 未 commit、未 push、未 stage、未重启服务、未启动任何 service；未改 `Cargo.lock`、
  根 `Cargo.toml`、`externals/`、`src/main.rs`、transport/protocol/board/tunnel 等业务源码。

## 2. v4 唯一契约

| 维度 | 契约 |
| --- | --- |
| 版本 | 编译期常量 `SUPPORTED_SCHEMA_VERSION = 4`；可识别版本**恰为 4**，v1/v2/v3/0/5/负数一律 `SchemaNotReady { found, required: 4 }` |
| 表集合 | 恰好 16 张：11 identity（`schema_meta,users,sessions,roles,role_permissions,device_groups,devices,group_members,grants,admission_decisions,audit_events`）+ 5 runtime（`task_previews,main_tasks,sub_tasks,device_operation_locks,recovery_checks`）；多表、少表、未知表、重名都拒绝 |
| 约束 | 0 个 FOREIGN KEY、0 个 CHECK（PK/UNIQUE/NOT NULL/UNSIGNED/索引/DATETIME(6) 保留） |
| DDL | 唯一文件 `crates/rsetup-controller/schema/v4.sql`，16 条 `CREATE TABLE IF NOT EXISTS` |
| 决策表 | `None → CreateV4`；`Some(4) → VerifyOnly`；`Some(其它) → Err` |
| 启动路径 | 普通服务启动仍然零 DDL：`main.rs` 只调用只读门禁 `check_identity_schema` |

版本语义提醒：**数据库 schema v4** 与隧道协议 v3、业务 DTO 的 `schema_version = 1` 是三个
互不相关的编号，本任务未触碰后两者（`scripts/*.py` 里的 `schema_version: 1` 属业务 DTO）。

## 3. 公共 API 变化

保留（签名不变）：`DbPool`、`AdmissionStore`、`check_identity_schema`、`TestMigrationConfig`
（+ `from_test_env`/`authorize`）、`migration_config_from_env`、`read_schema_version`、
`run_production_migration`、`MIGRATION_ACK_CONFIRMATION`（值仍为
`isolated-exclusive-backed-up-disposable`）、`valid_username`。

新增（`rsetup_controller::db` 下，crate 根不重导出）：

- `pub const V4_SCHEMA_SQL: &str` —— `include_str!("../schema/v4.sql")`；
- `pub fn v4_schema_statements() -> Result<Vec<(&'static str, String)>, ControllerError>`
  —— 白名单解析器：只接受 16 条已知表的 `CREATE TABLE IF NOT EXISTS`，重复表名、未知表名、
  `FOREIGN KEY`/`REFERENCES`/`CHECK (` 一律固定码拒绝；
- `pub enum ProductionMigrationPlan { CreateV4, VerifyOnly }` —— **`pub` 类型**；配套的决策表
  纯函数 `production_migration_plan(Option<i32>)` 是 **crate 内私有 `fn`，不属于 public API**，
  由 `src/db.rs` 内部单测 `production_migration_plan_accepts_only_empty_database_and_v4`
  离线覆盖（决策表可测性不依赖把它改成 `pub`，本次也未为此改动任何 `pub` 面）。

删除（crate 内 public/crate-visible）：`TestMigrationMode`、`run_identity_test_migration`、
`check_v3_ready`、`run_schema_v4_migration`、`upgrade_identity_schema`、`check_v3_ready`
导出、`SchemaV4MigrationProbe`、`FixtureV2Probe`、`create_historical_v1/v2_fixture`、
`IDENTITY_SCHEMA_VERSION`、`MIGRATION`/`V3_MIGRATION`/`TASKS_MIGRATION` 常量、
`integrity::MIN_SUPPORTED_SCHEMA_VERSION`、`integrity::legacy_expected_version`、
`validate_identity_rows_at_version`/`_for_version`。`src/lib.rs` 的导出行收缩为
`pub use db::{AdmissionStore, DbPool, TestMigrationConfig, check_identity_schema};`。

## 4. 删除 vs 保留

### 4.1 SQL

- **删除** `crates/rsetup-controller/migrations/` 整个目录（4 个文件）：
  `0001_identity_devices.sql`、`0002_identity_contract.sql`、
  `0003_identity_application_integrity.sql`、`0004_tasks.sql`。
- **新增** `crates/rsetup-controller/schema/v4.sql`：16 条建表语句（11 identity 段 +
  runtime 五表段），头部 14 行 `--` 注释写明契约与解析约定。

### 4.2 生产源码

| 文件 | 删除 | 新增/改写 |
| --- | --- | --- |
| `src/db.rs` | 9680 → **2751** 行。旧版本常量、legacy 列/形状/CHECK 校验、legacy 预检、按版本分支的升级链、测试迁移模式、夹具构造、`SchemaV4MigrationProbe`/`FixtureV2Probe` | `V4_SCHEMA_SQL` + 白名单解析器、`validate_table_set`（恰 16 表）、`validate_v4_shape`（逐表 collation/列名/列形状/索引/字符集/**0 CHECK**）、`validate_v4_tables_are_empty`、`ProductionMigrationPlan`、`FreshV4Probe` + `create_v4_schema_with_probe` |
| `src/integrity.rs` | 1486 → 1408 行。`MIN_SUPPORTED_SCHEMA_VERSION`、`legacy_expected_version`、`validate_identity_rows_at_version`/`_for_version`、"v1..=3 五表皆无即只读放行"分支 | `is_recognized_schema_version(v) = v == 4`；`validate_runtime_tables_for_version` 只认 v4 |
| `src/bin/migrate-identity-test.rs` | **整文件删除**（测试专用 legacy 迁移 CLI，靠自动发现构建） | — |
| `Cargo.toml` | bin 段注释不再提 `migrate-identity-test` | — |
| `src/bin/migrate-controller.rs` | 参数面不变（仍只有 `--check`/`--apply`） | 模块文档与 `USAGE` 改为 v4-only 语义：空库建满、v4 只读复核、其它版本拒绝、无升级路线 |
| `src/lib.rs` | 3 个 legacy 导出 | — |

`create_v4_schema_with_probe` 的顺序即门禁（每一步失败即停、绝不写 `schema_meta`）：
版本必须为 `None` → **首条 DDL 之前整库零表对象**（口径见 §10.3：`information_schema.tables`
中 `table_schema = DATABASE()` 的行数必须为 0，BASE TABLE 与 VIEW 都算；部分残留即拒绝）
→ 16 条白名单 CREATE，
每条之后复查元数据（漂移即停）→ 全库形状校验 → FK 校验 → 16 表数据必须为空 → 写
`schema_meta(schema_version = 4, initialized = FALSE)` → 读回必须为 4（写失败区分
`version write rejected` / `version write outcome uncertain`，绝不宣称成功）→ 最后跑一次
完整只读 `check_identity_schema_with_probe` 闭合。

### 4.3 测试

| 文件 | 处置 | 原因 |
| --- | --- | --- |
| `src/db.rs` 单测 | 139 → **37** 个：删除 114 个，保留 25 个，新增 12 个 | 删除的 114 个全部是"先造 v1/v2/v3 夹具再升级/放行"的用例（`TestMigrationMode` 矩阵、`create_historical_v*`、`check_v3_ready`、legacy CHECK/legacy 形状、`run_schema_v4_migration` 链）。保留的 25 个与版本无关：生产门禁 env/ACK、库名授权、grants 授权、FK 元数据、`decode_snapshot`、admission CAS/audit 码、用户名规则、DDL/索引/字符集形状助手 |
| `src/integrity.rs` 单测 | 19 个，其中 3 个改写 | `guard_accepts_supported_versions_and_rejects_unknown` → 只认 4；`runtime_tables_are_required_exactly_from_v4` 去掉 v3 放行分支；`legacy_fixture_reuses_full_scan_with_exact_v1_v2_meta` → `fixture_scan_uses_supported_version_and_rejects_other_meta_versions`（**同一套行校验器覆盖保留**，只是版本从 v1/v2 改为 v4，并新增"元数据声称 0/1/2/3/5/999 都拒绝"）。审计/授权/去敏/CAS 类用例一个没删 |
| `tests/migration_cli_args.rs` | **整文件删除** | 全部用例的主体是被删的 `CARGO_BIN_EXE_migrate-identity-test --mode upgrade/fixture-*`；argv 安全语义改由 `tests/v4_baseline.rs` 对**唯一入口**重新覆盖 |
| `tests/migration_v4.rs` | 重命名为 `tests/v4_schema_readonly_verify.rs` | 它本就是 `#[ignore]` 的真库只读核验；头部注释改为不再引用被删工具，并写明"通过与否不构成真实库已重建的证据" |
| `tests/v4_baseline.rs` | **新增 8 个离线用例** | 见 §5 |
| `tests/mysql_identity.rs`、`tests/tidb_identity.rs` | 各 2 处替换 | 旧的 `migrate-identity-test --mode upgrade` 前置步骤 → 改为 `rsetup-controller-migrate --check` 只读成功 + 断言"入口绝不补建被删表" |
| `tests/common/mod.rs` | 注释口径 | v3 链/`check_v3_ready` 表述 → v4-only 门禁表述 |
| `tests/metrics_real_db.rs` | 2 处注释 | 列名出处从 `migrations/0003..`/`0004..` 改指 `schema/v4.sql`；其 fixture 是通用 v4 admission/task 行，**不含旧版本矩阵，故无删除** |

### 4.4 CLI 与文档

- CLI：唯一 schema 入口 `rsetup-controller-migrate`（`--check` 只读版本、`--apply` 按决策表）；
  旧 `--mode upgrade|fixture-v1|fixture-partial-v1|fixture-v2` 全部成为未知参数。
- `README.md:504-509`、`README.zh.md:363-366`：删除"空库或 v1/v2/v3 先升到 v3 再升到 v4"，
  改为 v4-only 语义 + "旧版本库需操作员先做表级重置（第 2 步的真实备份是兜底）"。
- Makefile / `.github/workflows/*.yml` / `scripts/` / `debian/`：经 grep 核对**无需改动**——
  它们只引用 `rsetup-controller-migrate`（`make controller-migrate`、CI 静态链接检查与
  `--help` 断言、`debian/rules` 的 build/install），不引用被删的 legacy CLI 或 `migrations/`。
  CI 的 `--help | grep -q "rsetup-controller-migrate"` 断言与改写后的 USAGE 仍然一致。
- 未改写历史报告（`docs/testing/controller-v1-known-gaps.md`、
  `docs/testing/tunnel-controller-admission-report.md`、`docs/testing/q8b-*`）：它们是带日期的
  历史结论，不属于"当前操作文档"，改写会让历史验收记录失真。

## 5. 新增的 v4 离线证据

`tests/v4_baseline.rs`（8 个用例，全部不连库、不读环境）：

1. `legacy_schema_carriers_are_gone` —— `migrations/` 目录不存在、legacy CLI 源文件与
   Cargo.toml 注册不存在、`src/db.rs`/`src/integrity.rs` 源码内不再出现
   `TestMigrationMode`/`check_v3_ready`/`run_identity_test_migration`/`create_historical_v`/
   `0001_identity_devices.sql`/`0004_tasks.sql`/`MIN_SUPPORTED_SCHEMA_VERSION`/
   `legacy_expected_version`，`lib.rs` 不再导出 legacy 入口；
2. `v4_sql_is_the_only_baseline_and_is_fk_and_check_free` —— 独立解析 v4.sql 得 16 条
   CREATE、去注释后可执行文本无 FOREIGN KEY/REFERENCES/CHECK (/ALTER TABLE，并与生产解析器
   逐表逐序对账；
3. `legacy_cli_modes_are_rejected_before_any_connection` —— 上述 4 个 legacy 模式 + 两个模式
   同给 + `--dsn=…` + 空参数，全部非零退出、固定 ASCII 文案 `unexpected arguments`、
   不回显收到的 token；
4. `non_utf8_argv_is_rejected_without_panic_or_echo` —— 三段非法 UTF-8 argv（`OsStrExt`）
   非零退出且退出码 ≠ 101（证明走的是 `args_os()` + `OsStr` 字节比较，不是 `args()` panic）；
5. `incomplete_gates_fail_fixed_before_connecting_and_never_echo_credentials` —— 门禁 5 项
   的 5 种缺法子集（含 `env_clear()` 后只给部分）都在连接之前固定失败，且输出不含假口令、
   不含 `mysql://`、不含 panic 原文；
6. `check_without_configuration_fails_without_echoing_anything_secret`；
7. `help_documents_the_v4_only_contract` —— `--help` 必须含 `v4-only` 与 `no upgrade path`；
8. `startup_path_stays_read_only_and_the_entry_point_is_the_only_writer` —— `main.rs` 仍调用
   `check_identity_schema` 且不含 `run_production_migration`/`create_v4_schema`；`db.rs` 内
   `create_v4_schema(` 只有 1 个调用点；唯一 DDL 固定在 `schema/v4.sql`。

`src/db.rs` 新增 12 个 fake 探针用例覆盖建库顺序：
`production_migration_plan_accepts_only_empty_database_and_v4`、
`v4_baseline_sql_is_sixteen_tables_without_checks_or_foreign_keys`、
`v4_baseline_parser_rejects_unknown_tables_and_constraints`（含注入 FK/CHECK/未知表/重名）、
`table_set_requires_exactly_the_sixteen_v4_tables`、
`fresh_create_rejects_any_existing_version_before_touching_ddl`（任意非空版本 → 0 次 DDL）、
`fresh_create_rejects_partial_schema_before_first_ddl`（缺 1 张表 → 0 次 DDL）、
`fresh_create_writes_meta_only_after_full_shape_and_empty_data_pass`、
`fresh_create_stops_at_first_failure_and_never_writes_meta`、
`fresh_create_never_claims_success_when_meta_write_is_untrusted`、
`fresh_create_requires_supported_version_readback_before_closure_check`、
`v4_schema_error_messages_are_fixed_ascii_without_identifiers`、
`v4_baseline_sql_declares_every_contracted_table`。

## 6. 真实 RED → GREEN 记录

RED（实现收缩后先跑，全部是真实输出）：

```text
# 1) 删除 legacy 符号后，integrity 的旧单测立即编译失败（证明旧版本路线真的没了）
error[E0425]: cannot find value `MIN_SUPPORTED_SCHEMA_VERSION` in this scope
   --> crates/rsetup-controller/src/integrity.rs:708:46
error[E0425]: cannot find function `legacy_expected_version` in this scope
   --> crates/rsetup-controller/src/integrity.rs:1337:27

# 2) 新用例先红（fake 回报的表名带空格 → 生产的"建表结果与元数据不一致"漂移检查如实命中）
test db::tests::fresh_create_writes_meta_only_after_full_shape_and_empty_data_pass ... FAILED
  panicked at crates/rsetup-controller/src/db.rs:1887:
  真正空库必须一次建满 v4: Config("v4 schema baseline rejected: create result disagrees with schema metadata")
# 同期另有 v4_baseline_sql…/v4_baseline_parser…/runtime_tables…/2 个 fresh_create… 共 6 个失败
test result: FAILED. 890 passed; 6 failed; 0 ignored

# 3) v4_baseline 首轮 2 红（用例口径问题，如实暴露后修正，未改生产语义）
src/db.rs 仍含旧版本路线标记 run_identity_test_migration
v4 基线不得声明 FOREIGN KEY        # 命中的是文件头注释里的"无 FOREIGN KEY"字样
```

GREEN（修正 fixture 与断言口径后，真实树 `--all-targets`）：

```text
EXIT=0
通过: 1383 | 失败: 0 | ignored: 38
```

`cargo fmt` 之后原样重跑同一命令：`EXIT=0`、`通过: 1383 失败: 0`，与下表逐 target 结果一致。

逐 target 结果（真实工作区，`CARGO_TARGET_DIR=target/v4-scratch/live`，
`--locked --offline`，全部 `ok`）：

| target | 结果 |
| --- | --- |
| src/lib.rs | 914 passed / 0 failed |
| src/main.rs | 16 / 0 |
| src/bin/migrate-controller.rs | 5 / 0 |
| src/bin/reset-admin.rs | 8 / 0 |
| tests/v4_baseline.rs | **8 / 0** |
| tests/v4_schema_readonly_verify.rs | 0 / 0（1 ignored，真库用例未运行） |
| tests/mysql_identity.rs | 11 / 0（15 ignored） |
| tests/tidb_identity.rs | 11 / 0（15 ignored） |
| tests/metrics_real_db.rs | 11 / 0（3 ignored） |
| tests/task_scheduler.rs | 39 / 0 |
| tests/observer_contract.rs | 68 / 0 |
| tests/production_composition.rs | 14 / 0 |
| tests/gate_udp.rs | 25 / 0 |
| tests/events_sse.rs / sse_business_events.rs | 22 / 0 · 11 / 0 |
| tests/transport_admission.rs / _board_client.rs / _listener.rs / _manager.rs / _manager_fixes.rs | 32 / 15 / 17 / 27 / 22，全部 0 失败 |
| tests/acceptance_runtime.rs | 1 / 0（1 ignored） |
| tests/management_gateway_http.rs / reset_admin_cli.rs / time_consumption.rs / web_assets.rs | 3 · 6 · 9 · 88，全部 0 失败 |

其他门禁（真实工作区，本包范围）：

```text
cargo clippy -p rsetup-controller --all-targets --locked --offline   → CLIPPY_EXIT=0（0 warning）
cargo fmt --package rsetup-controller -- --check                     → FMT CLEAN
```

（fmt 差异只落在我改过的 5 个文件，用 workspace edition `2024` 就地格式化后复检干净；
未使用 `cargo fmt --all`，`externals/` 与他会话文件未被触碰。）

## 7. 校验环境说明与他会话在飞改动

- Task 1 中段，真实树的 `crates/rsetup-controller` 依赖另一会话正在编写的
  `crates/rsetup-tunnel`，当时无法编译（一次性记录，未修复、未轮询）：
  `crates/rsetup-tunnel/src/session/driver.rs:464` 的 `TrackSlot: Clone` 未满足、
  `:286` 的 `expected CallbackTracker<'_>, found JoinHandle<()>`。因此中途的门禁在
  `target/v4-scratch/ws2`（`git archive HEAD` + `externals` + 真实 `apps/controller-web/dist`
  副本 + 只叠加本任务改动文件 + HEAD 的 `Cargo.toml`/`lib.rs`）里跑，结果同样是
  20 target / 1246 通过 / 0 失败、clippy 0 warning。
- **本任务收尾时真实树已可编译**，于是上表全部改在真实树重跑（含他会话新增的 5 个
  `transport_*` 测试目标），并以真实树结果为准。隔离副本仅用于中途门禁，不构成额外产物。
- 真实树中 `crates/rsetup-controller/src/main.rs`、`src/transport/*`、`tests/transport_*.rs`、
  根 `Cargo.toml`/`Cargo.lock`、`.github/workflows/ci.yml` 等属其他会话的在飞改动，本任务未编辑。

## 8. 未验证与风险（不宣称零回归）

1. **测试总数下降**：`db.rs` 单测 139 → 37。删除的 114 个用例覆盖的是已不存在的代码路径
   （v1/v2/v3 夹具与升级链），不能等价保留；新增 12 个 v4 用例覆盖新语义。因此
   **不能宣称"零回归"**：与旧版本迁移相关的行为现在是"无此功能"，其证据是
   `tests/v4_baseline.rs` 的源码级缺席断言 + CLI 拒绝断言，而不是等价的功能测试。
2. **runtime 五表纳入严格形状校验是一项契约收紧**：`validate_v4_shape` 现在对 5 张 runtime
   表也逐列/逐索引比对 `schema/v4.sql`（此前 v4 门禁只对 11 张 identity 表做严格形状校验）。
   该收紧**未经真实 MySQL/TiDB 验证**（真库用例 ignored），若某环境的 runtime 表列定义与
   v4.sql 存在历史差异，会在启动门禁处 fail-closed。真实库验收（Task 3）必须先跑
   `tests/v4_schema_readonly_verify.rs` 与 `--check` 才能确认。
3. **已有旧版本库没有升级路线**：v1/v2/v3 库现在只能由操作员在外部授权下做表级重置后重建
   （AGENTS.md 允许的两个专用开发库范围内）。任何"本任务已重建真实库"的说法都不成立。
4. **`--apply` 仍要求四项真实门禁**（隔离/独占/已备份/ACK）。本报告未伪造任何备份引用，
   因此 `--apply` 一次都没运行。
5. `docs/testing/controller-v1-known-gaps.md:125` 仍记载"v4 正向迁移链无 CI 覆盖"——该行描述
   的是历史状态；v4-only 之后该链已不存在，留给 Task 2/3 的真实验收报告一并更新。

---

## 9. 追加：v4 基线 SQL 行内注释缺陷（2026-10-24 定点修复）

范围：`crates/rsetup-controller/schema/v4.sql`（仅注释布局）、`src/db.rs`（注释形态门禁 +
契约自闭合回归）、`tests/v4_baseline.rs`（新增 1 个离线用例）。**未**改任何列定义、类型、
默认值、字符集、索引；**未**改 controller 业务/tunnel/board/Cargo/lock/secret；**未**连接
任何数据库。§1–§8 的历史结论与 RED 记录保持原样，本节只追加事实，不改写旧叙述。

### 9.1 触发：真实 MySQL 8.0 只读核验如实失败

由**另一个**只读核验任务（SDD ledger 记为 `1bd64a6b`）在已授权 MySQL 8.0 开发库上复现，
本任务**没有重跑真实数据库**，只把它作为缺陷事实引用：

| 观测项 | 真实结果 |
| --- | --- |
| `schema_meta.schema_version` | 4 |
| 表数量 | 恰好 16 张 |
| FOREIGN KEY | 0（三个独立 information_schema 视图一致） |
| 列名 / 索引名独立对照 `TABLES`/`RUNTIME_TABLES` 期望常量 | **逐项完全相符** |
| `check_identity_schema` | **失败**：`Config("v4 schema DDL missing column task_previews.targets_json")` |
| `rsetup-controller-migrate --check` | 成功（`run_check` 只打印 `schema_version=<n>`，对**任何**版本值都返回 0，**不做形状校验**，成功不构成形状 PASS 证据） |

现库没错：库里确实有 `task_previews.targets_json` 这一列，名称与索引均与契约相符。失败的是
**编译期基线 SQL 与它自己的严格解析器不兼容**——这是纯源码缺陷，因此也绝不通过"放宽列校验、
跳过形状校验或改数据库"来处理。真实库未被写入任何字节，TiDB 重建继续暂缓。

### 9.2 根因（代码路径级）

1. `src/db.rs` 的 `v4_schema_body()` 只按**整行**过滤注释
   （`line.trim_start().starts_with("--")`）；
2. 修复前的 `schema/v4.sql` 第 84、85、87 行在列定义**行尾**写了行内 `--` 注释
   （`target_hash`、`targets_json`、`preview_generation` 三列）；
3. `ddl_parts_from()` 只按顶层逗号切分声明片段，行内注释文本被并入**下一个**片段，该片段
   变成 `-- 目标清单…\n  targets_json JSON NOT NULL`；
4. `validate_v4_shape` 逐列用 `part.strip_prefix("<列名> ")` 查找声明，于是查不到
   `targets_json`，抛出固定诊断 `v4 schema DDL missing column task_previews.targets_json`
   ——把源码缺陷伪装成"数据库缺列"的漂移。

诊断停在 `targets_json` 是因为它是注释所在行之后的第一个契约列（`target_hash` 自身的片段
仍然干净）；`preview_generation` 之后同一污染还会波及 `created_at`。

### 9.3 为什么 §6 的"离线全绿"没抓到（覆盖缺口的诚实解释）

§6 的 GREEN 是真的，但它从未让 16 张表的每一个契约列/索引真正走一遍形状解析：

1. **形状侧只有点测**：`db.rs` 的 37 个单测里逐列/逐索引消费 DDL 的只有 3 处（`users` 的索引
   集合、`devices` 的内联主键、`audit_events` 的 `BIGINT UNSIGNED`），都是手工挑一两个对象，
   没有任何用例遍历全部 122 个契约列与 40 个契约索引；
2. **两侧解析器都"看不见"行内注释**：`v4_sql_is_the_only_baseline_and_is_fk_and_check_free`
   与 `v4_baseline_sql_is_sixteen_tables_without_checks_or_foreign_keys` 只做"去整行注释 + 按
   `;` 切"的粗粒度检查（数 16 条 CREATE、查 FOREIGN KEY/REFERENCES/CHECK 关键字），行内注释
   既不影响条数也不触发关键字，因此缺陷在"全绿"下存活；
3. **唯一逐列消费 DDL 的路径需要连库**：`validate_v4_shape` 只在真实库上运行，其连接型用例
   全部 `#[ignore]`。于是"离线全绿"与"真库门禁失败"可以同时为真。

§8.2 当时把风险写成"runtime 五表纳入严格形状校验未经真实库验证"，方向正确，但没有识别出
"基线 SQL 与自身解析器的契约"这一项**本可离线证明**的缺口。本节补齐它。

### 9.4 修复（最小方案）

1. **`schema/v4.sql`：三条行内注释移到对应列的上一整行**（现第 87/89/92 行），列顺序、列
   定义、索引、`CHARACTER SET utf8mb4` 尾部全部逐字不变；文件头"解析约定"同步改写为"列的
   说明必须写在对应列的上一整行，行内/井号/块注释一律被门禁拒绝"。
2. **`src/db.rs` 新增 `v4_comment_form_check()` 严格门禁**：只支持整行 `--` 注释；行内 `--`
   → `v4 schema baseline rejected: unsupported inline comment`，井号注释与块注释
   → `unsupported comment form`，未闭合字符串字面量 → `unterminated string literal`。
   字符串字面量内的同名标记**不算**注释（`DEFAULT '--'` 不误判），注释体整段跳过（注释里的
   未配对引号不误判）。它接入两条消费同一份 DDL 的生产入口：`v4_schema_statements()`（白名单
   解析，建库路径）与 `validate_v4_shape()`（只读形状校验），后者原先会在解析器污染后给出
   误导性"库里缺列"诊断。
3. **不把解析器扩成通用 SQL 解释器**：门禁只识别三个注释起始标记，`ddl_parts_from` 的切分
   逻辑一字未改（此前 shell AST 因多轮补洞而收缩的教训不重演）。

### 9.5 语义等价证据（可执行 DDL 逐字节不变）

修复前的原始字节快照在 `target/v4-scratch/ws2/crates/rsetup-controller/schema/v4.sql`
（md5 `3f51c90d…`，与修改前工作区文件一致，本次只读使用）。对它和修复后文件各做一次
"剔除整行注释 + 截断行内注释 + 去行尾空白"的归一化：

| 项 | 修复前 | 修复后 |
| --- | --- | --- |
| 行内注释命中（字符串感知扫描） | **3**（第 84/85/87 行） | **0** |
| 归一化后可执行文本 SHA256 | `aae33ce49d415ac6b0d6e2a5539039c1e3634be207a30443209ba470ce2b0af9` | 同一值 |
| 归一化后字节数 | 6602 | 6602 |

`diff` 只有两类 hunk：头部解析约定注释扩写、三条注释从列尾移到列前。16 张表的独立解析结果
（片段数/列数/索引数，含内联主键）与新增回归的硬断言逐项吻合：
**16 表 / 122 列 / 40 索引**。

### 9.6 新增回归与真实 RED → GREEN

| 新增用例 | 位置 | 断言 |
| --- | --- | --- |
| `db::tests::v4_schema_contract_is_self_consistent_for_all_sixteen_tables` | `src/db.rs` 单测 | 对 16 张表的**每个契约列与每个契约索引**调用生产同一套 helper：`ddl_parts` → 与 `validate_v4_shape` 逐字相同的 `strip_prefix("<列名> ")` 查找（必须恰好一个）→ 由声明推导理想元数据后跑 `validate_column_shape`/`validate_column_charset`（连带 `validate_column_default` 真的执行类型/长度/UNSIGNED/NULL/字符集/collation/默认值）；**四类反向变异**（可空性取反、类型长度 ±1、默认值改 `1`、字符集/collation 改 latin1）都必须被拒绝，证明这个 `Ok` 不是空判定；`expected_indexes` 必须解析出列集合、唯一性（与 `PRIMARY`/`uq_` 前缀吻合）与无前缀长度，且篡改任一索引列集合后 `validate_indexes` 必须拒绝；顶层片段数必须等于 列数 + 独立索引数；无任何片段以 `--` 开头；覆盖规模硬断言 **122 列 / 40 索引 / 16 表**，防止靠缩小契约常量蒙过 |
| `db::tests::v4_schema_comments_must_be_whole_line_only` | `src/db.rs` 单测 | 基线两侧通过门禁；行内注释、井号注释、块注释、未闭合字面量分别按固定码拒绝；整行注释（含缩进、注释体含未配对引号）放行；`DEFAULT '--'` 不误判且其后的真行内注释仍被抓；把行内注释注回基线后门禁必须拒绝 |
| `v4_sql_comments_are_whole_line_only`（`tests/v4_baseline.rs` 第 9 个用例） | 集成测试 | **独立实现**的第二双眼睛：行内注释命中必须为 0，去整行注释后的文本不得残留 `--`/井号/块注释标记，生产解析器必须解析出 16 条且每条语句片段不含注释标记；先用"基线无字符串字面量"断言钉住自身前提 |

三个用例都消费生产 helper/生产解析器，**没有**为测试另写一个更聪明的解析器。

RED（第一步：只在 `db.rs` 加契约自闭合用例、SQL 与门禁都未改动时，真实输出）：

```console
$ CARGO_TARGET_DIR=target/tunnel-v4-parser-fix \
  cargo test -p rsetup-controller --lib --locked --offline \
  v4_schema_contract_is_self_consistent -- --nocapture
thread 'db::tests::v4_schema_contract_is_self_consistent_for_all_sixteen_tables' panicked at
  crates/rsetup-controller/src/db.rs:2862:17:
assertion `left == right` failed: v4 schema DDL missing column task_previews.targets_json（片段匹配数 0）
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 914 filtered out;
```

与真实 MySQL 8.0 上 `check_identity_schema` 的失败**同表同列同固定文案**。

RED 可复现（第二步：修复后的源码 + 修复前的 SQL，把 `target/v4-scratch/ws2/…/schema/v4.sql`
临时放回 `crates/rsetup-controller/schema/v4.sql`，随后按备份恢复并校验 md5）：

```console
db::tests::v4_schema_comments_must_be_whole_line_only ... FAILED
  schema/v4.sql 的注释必须全部是整行注释: Config("v4 schema baseline rejected: unsupported inline comment")
db::tests::v4_schema_contract_is_self_consistent_for_all_sixteen_tables ... FAILED
  v4 基线 SQL 必须可解析: Config("v4 schema baseline rejected: unsupported inline comment")
v4_sql_comments_are_whole_line_only ... FAILED
  assertion `left == right` failed: v4 基线只允许整行注释：列的说明必须写在对应列的上一整行
v4_sql_is_the_only_baseline_and_is_fk_and_check_free ... FAILED   # 既有用例经生产解析器一并变红
test result: FAILED. 1 passed; 2 failed   (lib)  ·  7 passed; 2 failed  (v4_baseline)
```

即新回归对真实缺陷敏感，不是恒绿的空断言。恢复修复后的 SQL（md5 `3fcfeb71…`）后同一批用例
全部通过。

> 复现时的真实陷阱：`v4.sql` 经 `include_str!` 编入二进制，而 cargo 的变更判定依赖 mtime。
> 用 `cp -p` 还原旧文件会保留旧 mtime，cargo 直接复用旧产物并报"全绿"（本次实测命中一次）。
> 复现 RED 必须让目标文件 mtime 前进（`cp` 不带 `-p`，或 `touch`）。

### 9.7 未修改的不变量与最终门禁结果

`None → CreateV4`、`Some(4) → VerifyOnly`、其它版本 → 固定错误、首条 DDL 之前的整库零表对象
预检（口径见 §10.3）、普通启动只走只读门禁、无任何 legacy 路线：这些语义均未触碰，对应用例
`production_migration_plan_accepts_only_empty_database_and_v4`、4 个 `fresh_create_*`、
`startup_path_stays_read_only_and_the_entry_point_is_the_only_writer`、
`legacy_schema_carriers_are_gone` 全部通过。最终态（真实工作区，
`CARGO_TARGET_DIR=target/tunnel-v4-parser-fix`，`--locked --offline`）：

```console
cargo test  -p rsetup-controller --lib --locked --offline              → 916 passed / 0 failed
cargo test  -p rsetup-controller --test v4_baseline --locked --offline  → 9 passed / 0 failed
cargo test  -p rsetup-controller --all-targets --locked --offline       → 25 个 target 全 ok：
                                                                          1386 passed / 0 failed / 38 ignored
cargo clippy -p rsetup-controller --all-targets --locked --offline       → 退出 0，0 warning
cargo clippy -p rsetup-controller --all-targets --locked --offline -- -D warnings → 退出 0（CI `make lint` 口径）
cargo fmt  --package rsetup-controller -- --check                        → 退出 0（未使用 --all）
```

1386 = 原报告的 1383 + 本次新增 3 个用例（`db.rs` 2 个 + `tests/v4_baseline.rs` 1 个）；
38 个 ignored 全部是连接型用例，一个都没运行。

### 9.8 其它发现（本任务如实报告，未扩大修复面）

1. **`ddl_parts_from` 存在 panic 路径**：对 `find(...).expect("table declared in migration")`、
   `strip_prefix(&prefix).unwrap()`、`rsplit_once(") CHARACTER SET").unwrap()` 三处，若今后有人
   把 v4.sql 的尾部写成 `) ENGINE=InnoDB CHARACTER SET …` 之类形态，只读门禁会以 panic 而非
   固定错误失败（fail-closed 方向正确，但诊断不可用）。新增的契约自闭合用例能在测试期先命中它，
   因此本任务不改；后续应收敛为 `ControllerError::Config` 固定码。
2. **`expected_indexes` 用子串匹配索引名**（`part.contains(" <name> (")`）：在片段可能含注释的
   旧形态下有误配空间；现由"无片段以 `--` 开头 + 片段数等于列数加索引数"两条断言约束，未改写。
3. **`rsetup-controller-migrate --check` 的成功不可当作形状一致性证据**：`run_check` 只打印
   `schema_version=<n>`（`src/bin/migrate-controller.rs`），§9.1 那次真实只读核验里形状校验失败
   时它依然返回 0。属既定的 CLI 语义，本任务不改，但真实库验收必须以
   `tests/v4_schema_readonly_verify.rs`（进程内 `check_identity_schema`）为准，不能只跑 `--check`。

### 9.9 边界声明

- 本任务**未连接任何数据库**、未读取 `secret/`、未回显任何凭据、未运行 `--apply`；新增用例
  全部离线。§9.1 的真实 MySQL 结论来自另一次只读核验任务，本任务未重跑，也不得据本节声称
  "真实库已通过"——真实库上的 `check_identity_schema` 通过与否仍待 Task 4 的两引擎验收。
- 未 commit / push / stage / clean，未重启或启动任何服务，未写工作区外路径（对照用的历史副本
  仅只读，备份副本落在被忽略的 `target/tunnel-v4-parser-fix/probe/`）。
- 本次改动文件只有 3 个：`crates/rsetup-controller/schema/v4.sql`、
  `crates/rsetup-controller/src/db.rs`、`crates/rsetup-controller/tests/v4_baseline.rs`，
  外加本报告的追加章节。

---

## 10. 追加：v4-only 整体独立审查（`57f41e76`）与收尾修复（2026-10-24）

范围：`crates/rsetup-controller/src/db.rs`（版本写入结果诊断 + 对应内部单测）、
`crates/rsetup-controller/src/integrity.rs`（不可达分支与过期注释）、本报告。
**未**改 `schema/v4.sql` 的任何字节、未改 CLI/`Cargo.toml`/`Cargo.lock`/其它 crate/`secret/`；
未连接任何数据库。§1–§9 的历史结论与 RED 记录保持原样，本节只追加事实并修正报告中
被独审判定为不准确的三处口径（F4）。

### 10.1 独审实跑数字（照抄独审记录，不加工）

独立审查为**只读审查 + 自行跑离线门禁**，全程零数据库连接：

| 门禁 | 独审实跑结果 |
| --- | --- |
| `cargo test -p rsetup-controller --lib` | **916 passed / 0 failed** |
| `cargo test -p rsetup-controller --test v4_baseline` | **9 passed / 0 failed** |
| `cargo test -p rsetup-controller --test v4_schema_readonly_verify` | **0 passed / 1 ignored**（真库用例未运行） |
| `cargo test -p rsetup-controller --bin rsetup-controller-migrate` | **5 passed / 0 failed** |
| `cargo test -p rsetup-controller --all-targets` | **1386 passed / 0 failed / 38 ignored** |
| `cargo fmt --package rsetup-controller -- --check` | 退出 0 |
| `cargo clippy -p rsetup-controller --all-targets -- -D warnings` | 退出 0 |

裁决：**Spec/Quality PASS、无阻断项**；§4.3 记录的 114 个旧用例删除被逐项判为合规，授权、
CAS、审计与数据扫描防护未被误删。

**独审的"全绿"不等于真实形状 PASS**：上表全部在零数据库环境下运行，覆盖的是离线语义。
真实 MySQL/TiDB 上的 `check_identity_schema` 闭合由各自授权任务的连接型用例记录承载，
本报告不代其宣称；`rsetup-controller-migrate --check` 返回 0 更不构成形状证据（§9.8.3）。

### 10.2 finding 清单：实际四项（F1–F4），不是三项

独审给出 **4 项非阻断 finding（Low/Info）**。早先把该列表转述为"3 个 Low"是错的，
本节按实际四项记录：

| 编号 | 位置 | 事实 | 本轮处置 |
| --- | --- | --- | --- |
| **F1** | `src/db.rs` `create_v4_schema_with_probe` 写版本之后 | `insert_v4_meta()` 返回 `Err` 后用 `read_version()` 判因，但 `match` 的兜底分支 `_ => "version write rejected"` 把**读回自身也失败**（连"确实没落定"都证明不了）也当成确证未提交 | 拆成三个分支：`Err(_) → "version write outcome uncertain"`（读回失败无法证明未提交）、`Ok(Some(4)) → uncertain`（可能已落定）、`Ok(None) \| Ok(Some(非 4)) → "version write rejected"`（唯一可确证的情形）。两种失败都绝不宣称成功、绝不自动重试、绝不跑成功门禁 |
| **F2** | `src/integrity.rs` `check_runtime_data` 首步之后 | `is_recognized_schema_version(v)` 的实现就是 `v == SUPPORTED_SCHEMA_VERSION`，其后的 `if version != SUPPORTED_SCHEMA_VERSION { return Ok(()) }` 不可达（唯一调用方 `db.rs` 恒传编译期常量） | 删除该不可达分支与其"v1..=3 只读放行"注释；函数文档改为"可识别版本恰为 4"。函数内 (1)–(5) 步 runtime 扫描（孤儿 `main_task_id`、非法 `sub_tasks.state`、锁孤儿 owner、非 completed 却带 outcome、`counts_json` 必须解析为 object）与 `SchemaNotReady` fail-closed **一条未减** |
| **F3** | `src/integrity.rs` `users.username` 扫描上方的注释 | 注释要求"不要移除 db.rs 里独立的 v1/v2 迁移碰撞预检"，而该预检已随 v4-only 收缩删除，指向不存在的代码 | 改写为真实保证：严格形状校验要求 `users` 上存在 `uq_users_username` 唯一索引（列本身是 `ascii` + `ascii_bin`，唯一性即原始字节唯一），故两行不可能携带相同 username 字节；逐行扫描负责的是每个 username 都符合合法小写 ASCII 规则 |
| **F4** | 本报告 | 头部引用仓库内不存在的 `2026-09-07-controller-v5.md`；§3 把 crate 内私有 `fn production_migration_plan` 与 `pub enum` 并列为 public API；"整库零表"口径不清 | 头部只引用实际存在的 `docs/superpowers/plans/2026-10-23-v4-only-database-baseline.md`；§3 明确 enum 为 `pub`、纯函数为私有 `fn`（可测性不靠扩大 `pub` 面）；§4.2 与 §9.7 的零表口径按 §10.3 改写 |

未采纳、未触碰：schema DDL、授权与门禁、CAS/审计、业务数据扫描、以及"非 4 一律拒绝"的
负向用例（`fresh_create_rejects_any_existing_version_before_touching_ddl` 等）全部原样保留。

### 10.3 "空库"的准确口径

`FreshV4Probe::schema_tables()` 的真实 SQL 是
`SELECT CAST(table_name AS CHAR) … FROM information_schema.tables WHERE table_schema = DATABASE()`，
所以"首条 DDL 之前必须为空"判定的是**该库的表对象集合为空**：

- `information_schema.tables` 同时含 BASE TABLE 与 VIEW，因此**视图也算残留**，有视图即拒绝建库；
- 它**不覆盖**独立的 ROUTINE（存储过程/函数）与 EVENT——这两类不在 `tables` 视图内，也不在
  v4 的 16 表契约内，本轮**未核验、不作任何主张**；本节只把口径写准确，不要求为
  routine/event 新增外部对象扫描。

### 10.4 新增回归与真实 RED → GREEN

新增 1 个离线用例（`src/db.rs` 单测，真走 `create_v4_schema_with_probe` 生产路径 +
受控 fake，不是把 `match` 抄进测试的纯函数断言）：

| 用例 | 断言 |
| --- | --- |
| `db::tests::fresh_create_reports_uncertain_when_failed_write_cannot_be_read_back` | fake 的 `insert_v4_meta()` 报 `Err` 且**之后的版本读回自身也报 `Err`** → 错误码必须是 `version write outcome uncertain`，且 `meta_writes == 1`（绝不自动重试）、`readonly_checks == 0`（绝不跑成功门禁）；对照用例：读回成功且确证版本为 3 → 才允许 `version write rejected` |

RED（实现未改、只加用例时的真实输出）：

```console
$ cargo test -p rsetup-controller --lib --locked --offline \
    --target-dir target/tunnel-v4-review-tail fresh_create_reports_uncertain
thread 'db::tests::fresh_create_reports_uncertain_when_failed_write_cannot_be_read_back' (283032) panicked at crates/rsetup-controller/src/db.rs:2187:9:
读回自身失败不能证明未提交，必须报不确定，实际：configuration: v4 schema baseline rejected: version write rejected
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 916 filtered out; finished in 0.00s
```

即旧代码确实把"读回失败"当成确证未提交并谎报 `rejected`，新断言对这一缺陷敏感。

GREEN（改完 F1/F2/F3 后）：`fresh_create_*` 7 个用例全部通过（原 6 个 + 新增 1 个），
`integrity::` 19 个单测在删除不可达分支后仍全部通过。

过程如实记录：新用例的第一版在 `let state = fake.state.lock()…` 之后仍有 `.await`，被
`clippy::await_holding_lock` 以 `-D warnings` 拦下（CI `make lint` 口径），已改为把 guard 收进
内层块后再取字段，**未使用 `#[allow]` 绕过门禁**。

### 10.5 本轮门禁结果（专用 target 目录）

真实工作区，`--target-dir target/tunnel-v4-review-tail`，全部 `--locked --offline`：

```console
cargo test -p rsetup-controller --lib --locked --offline            → 917 passed / 0 failed
cargo test -p rsetup-controller --test v4_baseline --locked --offline → 9 passed / 0 failed
cargo test -p rsetup-controller --all-targets --locked --offline     → 25 个 target 全 ok：
                                                                      1387 passed / 0 failed / 38 ignored
cargo fmt --package rsetup-controller -- --check                     → 退出 0（未使用 --all）
cargo clippy -p rsetup-controller --all-targets --locked --offline -- -D warnings → 退出 0
```

lib 917 = 独审的 916 + 本次新增 1 个用例；全 target 的 1387 = 独审的 1386 + 同 1 个新增用例，
ignored 仍是 38，全部是连接型用例，一个都没运行。未连库、未读 `secret/`、未回显凭据、
未 `--apply`、未 commit/push/stage/clean、未启停服务、未写工作区外路径；构建产物只落在
专用的 `target/tunnel-v4-review-tail`，不与其他会话共用的 `target/` 目录争锁。

证据出处（本节所有数字与源码引用都来自这一棵树，非任何 `target/` 下的副本）：

| 核验项 | 实测 |
| --- | --- |
| 运行目录 | `pwd` = `/home/aghost/workspace-ai/rsetup-next`（真实工作区，非 `target/v4-scratch/*`） |
| 测试二进制 | `Running unittests src/lib.rs (target/tunnel-v4-review-tail/debug/deps/rsetup_controller-c4d3f900ae7142c4)`，2026-10-24 02:45 复跑 917 passed / 0 failed |
| `schema/v4.sql` | `readlink -f` = `…/rsetup-next/crates/rsetup-controller/schema/v4.sql`；`sha256` = `ea7fe6b5fb4023e042a028258dd2043c7fc03369799a1f8b13169a6e66a9b298`，与真实 MySQL 完整只读 PASS 时同值；本轮只读，**未写入任何字节** |
| v4.sql 行内注释 | `grep -vE '^[[:space:]]*(--\|$)' v4.sql \| grep -- '--'` → **0 命中**，§9.4 的修复完整；`grep -rn "built_in\|last_seen_at" src/ schema/` → **0 命中**（这两个列名在本契约里不存在） |
| 本轮改动 | `git status`：` M crates/rsetup-controller/src/db.rs`、` M crates/rsetup-controller/src/integrity.rs`，本报告为本任务的文档产物；其余文件未被本轮触碰 |

### 10.6 与并行任务的边界

- 真实 MySQL 的修后只读重验、TiDB 的表级重建各由独立授权任务记录：本轮既不重跑、不读取其
  凭据，也不代其宣称结果。可作为既有事实引用的只有"TiDB 一致性备份门禁曾真实通过"，
  而**备份成功不等于重建完成**，重建结论仍以该任务自己的记录为准。
- 独审的离线全绿 + 本轮离线全绿，都不改变 §8.2 与 §9.9 的结论：真实两引擎验收尚未在本报告内完成。

## 11. 收尾的独立复审（28ca5405，2026-10-24）

**Spec PASS / Quality PASS，F1–F4 全部 ADDRESS，无阻断。** reviewer 只读核当前源码，
独立实跑 lib **917**、v4_baseline **9**、实际 target `rsetup-controller-migrate` **5**、
all-targets **1387 passed / 0 failed / 38 ignored**，包级格式通过；两个完整名称的精确回归
各通过一项。未跑 ignored、未连库，未复跑 clippy，不代称真实 SQL 验收。

reviewer 未获得精确 tail diff，按源码锚点与 schema SHA256 核查；历史 ws2 只作方向对照，
不是最终基线。此前 writer 的虚假 schema 告警已撤回，主代理和 reviewer 都实证这两个列名
不存在、DDL 哈希未变，告警不作为本复审证据。

剩余 Low/Info 已由主代理只改注释与文档收口：命令 target 更正为 `rsetup-controller-migrate`；
用户名规则和非 v4 启动拒绝的注释不再指导已删的旧迁移 preflight。生产逻辑与 DDL 未改。
真实 MySQL 完整只读门禁已由另一个授权任务实际通过，TiDB 重建结果仍须独立操作回执，
本报告不据离线 PASS 宣称两个真实引擎或生产双进程全部完成。

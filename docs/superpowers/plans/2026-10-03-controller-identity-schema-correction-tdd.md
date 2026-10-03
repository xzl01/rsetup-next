# Controller identity schema correction TDD 实施计划

> **历史计划，后续目标已修订：** 2026-10-03 用户确认不依赖外键/CHECK、由应用校验与事务保证关系。本文保留 identity v2/u64 修订过程；当前实施以[应用层完整性设计](../specs/2026-10-03-controller-application-integrity-design.md)和[新计划](2026-10-03-controller-application-integrity-tdd.md)为准：identity目标v3，未来Tasks0004/v4。本文“完整CHECK集合”“v2最终ready”“Tasks0003”仅代表历史阶段，不得用于放宽新数据预检或继续实现CHECK兼容层；0001/0002原文不变，现有真实库仍未全套验收。

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 分任务落实，使用 `- [ ]` 跟踪步骤。本计划不是生产迁移许可。

**Goal:** 保留已提交 0001，新增 identity 0002 修正 Revision/Counter 为 `BIGINT UNSIGNED`/u64、用户名为 `VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin`；未来 Tasks 顺延为 0003。

**Architecture:** 普通中控启动先只读检查目标 v2，空库/v1/混合态返回结构化 not ready，bootstrap/监听均不发生；普通启动不自动执行 0002 ALTER，也不执行 0001 CREATE/版本写入。只有独立显式的 `migrate-identity-test` 开发入口在操作者核对独占、隔离、真实备份、可丢弃测试库及多项本地门槛后执行 0001→0002：此前两库无实际备份的状态仅记录为用户可丢弃授权，不豁免迁移备份门槛；先用无需数据库的行为 RED 锁定边界，再全量预检、按元数据逐条恢复非事务 DDL，完整目标校验后升级版本。未来 HTTP 投影需把 u64 写成 JSON 十进制字符串。

**Tech Stack:** Rust 2024 / MSRV 1.85、SQLx MySQL driver；目标验收 MySQL 8.4 LTS、TiDB 8.5 LTS。已知本地 MySQL 实际版本为 8.0.46，不等于 8.4 目标版本的验收证据。

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md` §2；`docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md` §§1–2；`docs/superpowers/plans/2026-10-02-controller-01-identity-data-tdd.md` migration/Task 6；`docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`。

## Global Constraints

**本地双 dev 库补充（不改变迁移备份门槛）：** 用户仅授权 ignored secret JSON 指向的两个已核实、隔离、可丢弃的专用开发库内表级测试与串行复用；每例先核对 `SELECT DATABASE()` 与目标精确匹配，再只清理该目标库内的表，绝不 `DROP DATABASE`，本轮不 `CREATE DATABASE`，不扩展至生产或第三库。可丢弃授权不等于备份；运行前须核验表（含视图）、例程、触发器、事件均为空，实际导出空库逻辑备份，核对回执中的目标匹配并校验备份文件 SHA256；**实际备份和真实套件的执行结论须另以验证记录为准，不能由本计划推断已通过**。在真实备份及原有独占、隔离、停写等门槛满足前，不运行要求备份的迁移，不伪造 `backup_ref` 或 ACK。ignored JSON 只在 runner 进程内读取并注入专用 env；不输出凭据/派生值或写入可提交文件。URL 字符串不同不证明库隔离；普通启动只读、生产 ALTER 未授权的约束不变。

- 用户仅批准**开发/测试设计**，未授权真实生产库 ALTER、备份原件迁移或兼容性宣告。现有本地双 dev 库不构成两引擎真实验收通过证据；普通中控启动对空库、v1（包括中断混合态）默认拒绝为结构化 `SchemaNotReady { found: Option<i32>, required: 2 }`；先只读 metadata，v2 再只读严格校验，绝不执行 0001 CREATE/INSERT 或 0002 ALTER/UPDATE、不输出管理员秘密、不调用 bootstrap、不绑定业务监听。生产 ALTER 尚未批准；本文受控命令仅能用于已确认独占、隔离、有真实可恢复备份且可丢弃的**测试库**，上述两库的可丢弃授权不豁免真实备份门槛，不能推广至生产或其他库。破坏性 fixture 不得只凭 `required_test_db()` 的库名含 test 或单个 `CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1` 宣称安全；真实独占、隔离、停写及备份必须由操作者核验并记录，代码只能校验明确确认、目标身份及条件，无法证明没有其他写入者。不得触碰 `.cargo-home/`。
- `crates/rsetup-controller/migrations/0001_identity_devices.sql` 已提交且逐字保留。repo 当前只有 0001；新增 `0002_identity_contract.sql`，03 计划未来三处 `0002_tasks.sql` 更名为 `0003_tasks.sql`。实施前只读核查目标库/备份：如 Tasks 0002 已应用，绝不可重用/重命名，必须停下另审 `0003_identity_contract.sql` 并顺延 Tasks；默认编号方案不适用该情形。
- 所有十个业务 Revision/Counter SQL 列均 `BIGINT UNSIGNED NOT NULL` ↔ Rust u64：`schema_meta.authz_epoch,admin_guard_revision`、`users.revision`、`roles.revision`、`device_groups.revision`、`devices.revision`、`grants.revision`、`admission_decisions.previous_revision,new_revision`、`audit_events.event_seq`。`schema_meta.schema_version INT`、SQL `COUNT(*)`、索引 `non_unique` 的 i64 非业务 Counter；旧示例 display_name 宽度和 ix/idx 别名不属强制缺陷。
- 用户名 SQL `VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL`；业务校验 ASCII 小写 3–64，首字符字母/数字，其余字母/数字/点/下划线/连字符。Unicode/长度/非法字符或大写、目标 ascii_bin 唯一冲突、负 revision/epoch/event_seq，均须在第一次 DDL **之前** fail closed，绝不截断、折叠、覆盖。
- MySQL/TiDB DDL 非事务，不能依赖 `SELECT ... FOR UPDATE` 跨 DDL 保持预检；执行者必须先在测试库外停业务写和第二迁移执行者、校验独占及真实可恢复备份；此前两库无备份时仅可做符合授权的表级操作，不能运行要求备份的迁移，生产维护/备份恢复另审。只有显式 `migrate-identity-test` 能运行私有 `upgrade_identity_schema`：接受专用 `CONTROLLER_TEST_DATABASE_URL`、`CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1`、操作者记录真实备份引用和独占/隔离/可丢弃确认、准确的预期库名与 `SELECT DATABASE()` 一致，空库还需 `information_schema.tables` 计数=0；这些不是独占的自动证明，失败须在第一次 DDL 前拒绝。version1 的旧/目标混合列可能是中断状态：受控入口先按实时 metadata 分类、重新预检仍旧列、只 ALTER 旧列并逐条重读；旧 version1 完整严格校验不得阻碍混合列恢复。未知形状/无损升级不可能要报需人工处理，不能声称 DDL 自动回滚。完整目标校验成功后才条件推进 schema_version 1→2 并查一行，version2 普通启动只读校验目标保持2；版本写返回不确定必须重读 version/metadata。
- 每项先建立可编译、达到目标断言的行为 RED，再最小 GREEN。新增本地安全边界（版本决策、启动顺序、CLI 授权/真实库名、fixture）分别提供纯函数或可注入启动/命令依赖测试，先针对**现有自动 CREATE/migrate 行为**观察断言 RED 再实现；SQLx 编译失败、缺 helper/URL、连接错误、ignored 跳过或零用例均非行为 RED/PASS。静态 Rust 单测不等于 MySQL8.4/TiDB8.5 的真实 u64 bind/Row 解码、CAS/DDL 恢复验收；每引擎记录 `SELECT VERSION()`、实际用例数和失败数；有 URL 失败/0用例为失败，缺 URL 未验证。
- JSON 对外 Revision/Counter 是十进制**字符串**；现有 `build_router(DbPool)` 只有 healthz/readyz，先提供转换接口、后续 API 任务接线，不假称已存在管理端点。每项独立审查、频繁提交，不改无关文件。

## 文件结构、职责和接口

- Create `crates/rsetup-controller/migrations/0002_identity_contract.sql`：十个数字列、一个用户名列的固定 ALTER，仅由受控测试入口的安全预检驱动运行。
- Modify `crates/rsetup-controller/src/main.rs`：只将普通启动迁移调用改为只读就绪检查并使 bootstrap/监听在检查成功后运行；`src/lib.rs`：改导出；`src/error.rs`：结构化 `SchemaNotReady`。Create `src/bin/migrate-identity-test.rs`：独立受控测试命令，永不从普通服务入口调用。
- Modify `crates/rsetup-controller/Cargo.toml`（Task 2 执行时）：增加 `default-run = "rsetup-controller"`，确保新增 bin 后普通 `cargo run -p rsetup-controller` 仍只运行服务而不会意外进入迁移命令；普通 `--bin rsetup-controller` 同样只读。
- Modify `crates/rsetup-controller/src/db.rs`：只读 `check_identity_schema(&DbPool)`、私有 `upgrade_identity_schema`、受限命令入口与 information_schema/预检/恢复/version/AdmissionStore/audit；`src/model.rs`：快照 u64/JSON；`src/auth/service.rs`：IdentityUser/fake u64 checked +1。
- Modify/Test `crates/rsetup-controller/tests/common/mod.rs`：显式授权的隔离库 fixture、audit seq u64；`tests/mysql_identity.rs` 与 `tests/tidb_identity.rs`：更新**全部**旧 `migrate` 依赖并添加 ignored 双库场景。
- Modify（仅未来 Task 4）`docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`：三处编号/版本交接。本文撰写阶段不修改旧计划；永远不改 0001。

---

### Task 1: schema validator 与用户名行为

**Files:** Modify/Test `crates/rsetup-controller/src/db.rs`（现有 `ColumnMeta`、`validate_column_shape`、全部旧 `ColumnMeta` fixture 和测试）。

**Interfaces:** 保留 `validate_column_shape(table: &str, expected: &str, actual: &ColumnMeta) -> Result<(),ControllerError>`；`ColumnMeta` 新增 `character_set_name: Option<String>, collation_name: Option<String>`；新增 `pub(crate) fn valid_username(s: &str) -> bool` 给 Task 2 使用。

- [ ] **Step 1 测试。** 只加两个 metadata 字段并给旧 fixture 补 `None`，不加入新的比较；先给 `valid_username` 可编译桩 `fn valid_username(_: &str) -> bool { true }`。在 `db.rs::tests` 加：

```rust
#[test]
fn same_width_signed_counter_is_incompatible() {
    let signed = ColumnMeta { name: "event_seq".into(), data_type: "bigint".into(),
        column_type: "bigint".into(), nullable: false,
        character_set_name: None, collation_name: None };
    let unsigned = ColumnMeta { column_type: "bigint unsigned".into(), ..signed.clone() };
    assert!(validate_column_shape("audit_events", "BIGINT UNSIGNED NOT NULL", &unsigned).is_ok());
    assert!(validate_column_shape("audit_events", "BIGINT UNSIGNED NOT NULL", &signed).is_err());
}
#[test]
fn username_requires_ascii_bin_not_matching_width_only() {
    let good = ColumnMeta { name: "username".into(), data_type: "varchar".into(),
        column_type: "varchar(64)".into(), nullable: false,
        character_set_name: Some("ascii".into()), collation_name: Some("ascii_bin".into()) };
    assert!(validate_column_shape("users", "VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL", &good).is_ok());
    for bad in [ColumnMeta { character_set_name: Some("utf8mb4".into()), ..good.clone() },
                ColumnMeta { collation_name: Some("ascii_general_ci".into()), ..good.clone() }] {
        assert!(validate_column_shape("users", "VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL", &bad).is_err());
    }
}
#[test]
fn username_rules_reject_noncanonical_input() {
    let long_ok = "a".repeat(64);
    let long_bad = "a".repeat(65);
    for good in ["abc", "a.b_c-1", long_ok.as_str()] { assert!(valid_username(good)); }
    for bad in ["ab", long_bad.as_str(), "Éric", "Alice", "alice!", "-alice", "a b"] {
        assert!(!valid_username(bad), "{bad:?}");
    }
}
```

- [ ] **Step 2 RED。** 分别运行 `cargo test -p rsetup-controller same_width_signed_counter_is_incompatible`、`cargo test -p rsetup-controller username_requires_ascii_bin_not_matching_width_only`、`cargo test -p rsetup-controller username_rules_reject_noncanonical_input`。必须由旧 validator 错放 signed/charset 和桩错放非法名的**断言**红；编译失败先补 fixture，不能算 RED。
- [ ] **Step 3 GREEN。** `information_schema.columns` 查询改为：

```sql
SELECT data_type, column_type, is_nullable, character_set_name, collation_name
FROM information_schema.columns
WHERE table_schema = DATABASE() AND table_name = ? AND column_name = ?
```

`row.try_get::<Option<String>, _>("character_set_name")?` 与 `collation_name` 装入新字段；比 `DATA_TYPE`、`COLUMN_TYPE` 的 unsigned token（不能只看 bigint 同宽）、长度、nullable 和 username 确切 ascii/ascii_bin；非字符列字段为 None。用户名检查直接使用：

```rust
pub(crate) fn valid_username(s: &str) -> bool {
    let bytes = s.as_bytes();
    (3..=64).contains(&bytes.len())
        && bytes.first().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()
            || matches!(*b, b'.' | b'_' | b'-'))
}
```

首字节判别同时阻止 Unicode/非 ASCII；不做折叠/截断。Task 1 仅补纯函数/元数据测试与 `information_schema` 读取，**不得先把目标列的严格校验接进当前 `migrate()` 启动路径**：合法旧 v1 必须留给 Task 2 分类/升级，升级时旧形与目标形都可辨识，完整目标校验只在 ALTER 完成后启用；否则会在首次 ALTER 前将自身 0001 误判为不兼容。最终目标列由新 0002 声明覆盖，不能让旧 0001 signed/varchar128 作为最终预期。
- [ ] **Step 4 回归。** 重跑三个目标命令及 `cargo test -p rsetup-controller db::tests`；表驱动对十列各验证 signed 错、unsigned 对；保持 `schema_version INT`、BOOLEAN/JSON、BINARY 长度、索引/CHECK 老测试绿。`cargo fmt --all -- --check && cargo test -p rsetup-controller`。
- [ ] **Step 5 审查/提交。** 独立审查 metadata 字段在实际查询读取且未让所有字符列变 ascii；`git add crates/rsetup-controller/src/db.rs && git commit -m 'test(controller): enforce identity column metadata'`。

### Task 2: 默认只读拒绝、受控测试命令、全量预检与版本 2

**Files:** Create `crates/rsetup-controller/migrations/0002_identity_contract.sql`、`crates/rsetup-controller/src/bin/migrate-identity-test.rs`；Modify/Test `crates/rsetup-controller/Cargo.toml`、`crates/rsetup-controller/src/{main.rs,lib.rs,error.rs,db.rs}`、`crates/rsetup-controller/tests/common/mod.rs`、`tests/mysql_identity.rs`、`tests/tidb_identity.rs`。

**Interfaces:** 移除/废弃公开的自动建库 `migrate(&DbPool)`（`lib.rs` 不再导出）；`pub async fn check_identity_schema(db: &DbPool) -> Result<(), ControllerError>` **只读**，只有 v2 及目标列/索引/CHECK 合格才 Ok，空库/缺 singleton 返回 `ControllerError::SchemaNotReady { found: None, required: 2 }`，v1/混合态返回 `found: Some(1)`，未知/多行/损坏拒绝但不写 DDL；`pub async fn run_identity_test_migration(config: &TestMigrationConfig, mode: TestMigrationMode) -> Result<(), ControllerError>` 仅供独立 bin，`TestMigrationMode::{Upgrade, FixtureV1, FixturePartialV1}` 由显式 `--mode` 参数解析，在校验操作者确认及目标库身份后才调用私有 `async fn upgrade_identity_schema(db: &DbPool) -> Result<(), ControllerError>`。`TestMigrationConfig` 字段精确为 `test_url: String, allow_destructive: bool, expected_database: String, backup_ref: String, migration_ack: String`；`TestMigrationConfig::fixture(url, name)` 仅在 `#[cfg(test)]` 下构造 `allow_destructive=false, backup_ref="", migration_ack=""`；`TestMigrationConfig::from_test_env()` 仍是当前读取五个专用 env 的接口；本地双 dev runner 在进程内读取 ignored JSON 后注入这些专用 env，不能用服务 URL 替代测试 URL，`TestMigrationConfig::authorize(actual)` 调同一纯 `authorize_test_migration(self, actual)` 供 integration fixture 调用。`fn authorize_test_migration(config: &TestMigrationConfig, actual_database: &str) -> Result<(), ControllerError>` 是可单测的纯门槛，不能声称证明真实独占。`fn preflight_usernames<'a>(values: impl IntoIterator<Item=&'a str>) -> Result<(),ControllerError>` 调 Task 1 validator；`fn classify_identity_column(table: &str, name: &str, actual: &ColumnMeta) -> Result<bool,ControllerError>`，true=精确旧列待 ALTER，false=精确目标列。旧 utf8mb4 唯一键不能替代目标排序规则检查。

- [ ] **Step 1 先写可编译行为测试。** 先提供必需接口的**错误旧行为桩**（`check_identity_schema` 委托旧 `migrate`，`authorize_test_migration` 总是 Ok，`start_if_ready` 故意先执行后检查）；不得先实现安全行为。除原有 `preflight_usernames`、`classify_identity_column` 纯测外，`db.rs::tests` 加：

```rust
#[test]
fn empty_and_v1_are_not_ready_without_ddl() {
    for (version, expected) in [(None, None), (Some(1), Some(1))] {
        assert!(matches!(identity_schema_decision(version),
            Err(ControllerError::SchemaNotReady { found, required: 2 }) if found == expected));
    }
    assert!(identity_schema_decision(Some(2)).is_ok());
}
#[test]
fn test_migration_needs_all_independent_confirmations() {
    let mut config = TestMigrationConfig::fixture("mysql://fixture/test_identity", "test_identity");
    // fixture() 只构造测试输入，不赋予迁移权限；初始缺 allow/备份/独占确认。
    assert!(authorize_test_migration(&config, "test_identity").is_err());
    config.allow_destructive = true;
    assert!(authorize_test_migration(&config, "test_identity").is_err());
    config.backup_ref = "snapshot-42".into();
    config.migration_ack = "isolated-exclusive-backed-up-disposable".into();
    assert!(authorize_test_migration(&config, "production").is_err());
    assert!(authorize_test_migration(&config, "test_identity").is_ok());
}
#[test]
fn preflight_refuses_invalid_and_target_collision() {
    let long = "a".repeat(65);
    for names in [&["alice", "Éric"][..], &["alice", long.as_str()],
                  &["alice", "Alice"], &["alice", "alice"]] {
        assert!(preflight_usernames(names.iter().copied()).is_err(), "{names:?}");
    }
    assert!(preflight_usernames(["alice", "bob_1"]).is_ok());
}
#[test]
fn mixed_identity_columns_are_resumable() {
    let old = ColumnMeta { name: "revision".into(), data_type: "bigint".into(),
        column_type: "bigint".into(), nullable: false,
        character_set_name: None, collation_name: None };
    assert!(classify_identity_column("devices", "revision", &old).unwrap());
    let new = ColumnMeta { column_type: "bigint unsigned".into(), ..old.clone() };
    assert!(!classify_identity_column("devices", "revision", &new).unwrap());
    let drift = ColumnMeta { data_type: "varchar".into(), column_type: "varchar(20)".into(), ..old };
    assert!(classify_identity_column("devices", "revision", &drift).is_err());
}
```

只读探针也要真测普通检查的**调用边界**：把当前 `migrate` 的 SQLx 读取整理为 `IdentitySchemaProbe`（只暴露 `read_version()` 与 `validate_v2_shape()` 两类读取，返回 `Future<Output=Result<_,ControllerError>> + Send`；SQLx DbPool 实现仅 `SELECT`），生产 `check_identity_schema(db)` 调用 `check_identity_schema_with_probe(db)`；可编译 fake 记录调用并给 `None`/`Some(1)`/`Some(2)`，断言前二返回 `SchemaNotReady` 且 `validate_v2_shape` 计数0，v2 计数1、其形状失败仍拒绝。此 fake 不执行 SQL，但保证普通路径**不能调用迁移函数**；对 SELECT-only 的 SQLx 实现另作只读 SQL 清单审查。必须先用旧版调用图桩让 probe/fake 路径的断言 RED（不要拿无 URL 数据库错误作 RED），再独立实现读路径。

在 `main.rs::tests` 用同一生产 `start_if_ready(check_future, start_closure)` 注入不触网的假启动（具体可编译示例）：

```rust
#[tokio::test]
async fn startup_blocks_bootstrap_and_listener_when_schema_not_ready() {
    use std::cell::Cell;
    let calls = Cell::new(0);
    let result = start_if_ready(
        async { Err(rsetup_controller::ControllerError::SchemaNotReady { found: Some(1), required: 2 }) },
        || async { calls.set(calls.get() + 1); Ok(()) },
    ).await;
    assert!(matches!(result.unwrap_err().downcast_ref::<rsetup_controller::ControllerError>(),
        Some(rsetup_controller::ControllerError::SchemaNotReady { found: Some(1), required: 2 })));
    assert_eq!(calls.get(), 0);
    start_if_ready(async { Ok(()) }, || async {
        calls.set(calls.get() + 1);
        Ok(())
    }).await.unwrap();
    assert_eq!(calls.get(), 1);
}
```

生产 main 也必须调用同一 helper 包裹 `bootstrap_admin` + TcpListener/serve（不能只测未使用的 fake）；错误桩故意先执行闭包，测试才因调用次数 1 而红。命令调度的可编译注入边界如下（`D=()` 的 fake 不触网，生产适配器 `D=DbPool`，连接适配器返回查询到的真实库名）：

```rust
async fn run_authorized_migration_with<D, C, CF, U, UF>(
    config: &TestMigrationConfig, connect_and_name: C, upgrade: U,
) -> Result<(), ControllerError>
where
    C: FnOnce() -> CF,
    CF: std::future::Future<Output = Result<(D, String), ControllerError>>,
    U: FnOnce(D) -> UF,
    UF: std::future::Future<Output = Result<(), ControllerError>>,
{
    authorize_test_migration(config, &config.expected_database)?; // 先本地门槛
    let (db, actual) = connect_and_name().await?;
    authorize_test_migration(config, &actual)?; // 再核对 SELECT DATABASE()
    upgrade(db).await
}
#[tokio::test]
async fn unauthorized_command_never_invokes_upgrade() {
    use std::cell::Cell;
    let calls = Cell::new(0);
    let config = TestMigrationConfig::fixture("mysql://fixture/test_identity", "test_identity");
    let result = run_authorized_migration_with(&config,
        || async { Ok(((), "test_identity".into())) },
        |_| async { calls.set(calls.get() + 1); Ok(()) }).await;
    assert!(result.is_err());
    assert_eq!(calls.get(), 0);
}
```

这个示例展示 GREEN 目标：先令桩**绕过**两次 authorize 观察旧行为 RED，再实现；缺 URL/allow/备份/确认及实际库名不符各用单独 fake 断言计数0，全部通过才计数1。`run_identity_test_migration` 将真实 `DbPool::connect/SELECT DATABASE()/upgrade_identity_schema` 接到该 helper，不能让连接错误充 RED。
- [ ] **Step 2 RED。** `cargo test -p rsetup-controller --lib empty_and_v1_are_not_ready_without_ddl`、`cargo test -p rsetup-controller --lib test_migration_needs_all_independent_confirmations`、`cargo test -p rsetup-controller --bin rsetup-controller startup_blocks_bootstrap_and_listener_when_schema_not_ready`、`cargo test -p rsetup-controller --lib preflight_refuses_invalid_and_target_collision`、`cargo test -p rsetup-controller --lib mixed_identity_columns_are_resumable`、`cargo test -p rsetup-controller --lib unauthorized_command_never_invokes_upgrade`；须**各自旧行为断言失败**，编译失败先修桩，不能用 ignored/连接失败充 RED；GREEN 时逐一重跑。
- [ ] **Step 3 最小 GREEN：严格分离普通启动与测试迁移。** `lib.rs` 改为导出 `check_identity_schema` 和仅供专用 bin 的 `run_identity_test_migration, TestMigrationConfig, TestMigrationMode`，删掉旧 `migrate` 公开导出；`Cargo.toml` 为服务 binary 设置 `default-run = "rsetup-controller"`，静态检查默认 bin 指向服务（不因新增 bin 改为迁移命令）。`error.rs` 新增 `#[error("identity schema not ready: found {found:?}, required {required}")] SchemaNotReady { found: Option<i32>, required: i32 }`，不得拼接 URL/密码/管理员秘密。`db.rs` 把当前 `migrate()` 的 CREATE/INSERT/旧 v1 验证搬进**私有** `upgrade_identity_schema()`，移除普通启动路径中的 DDL；新增只读 `identity_schema_decision(version: Option<i32>)`（None/1 返回上述结构化错误，2 Ok，其余版本明确拒绝），`check_identity_schema` 仅查询表、singleton/version、metadata/索引/CHECK，在 version=2 下比对 0001 非目标部分和 0002 目标列，0 条或多条/缺表均不写入、fail closed。可用下列生产调用方式使 startup fake 测试确实覆盖同一顺序：

```rust
// src/main.rs: boot + bind 都放在通过 check 之后；helper 被生产和单测共同调用。
async fn start_if_ready<C, F, Fut>(
    check: C, start: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    C: std::future::Future<Output = Result<(), rsetup_controller::ControllerError>>,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    check.await?;
    start().await
}
// main() 获取 config/db 后，secret_path 取现有 CONTROLLER_BOOTSTRAP_SECRET_LOG 默认值：
let secret_path = std::env::var_os("CONTROLLER_BOOTSTRAP_SECRET_LOG")
    .map(std::path::PathBuf::from)
    .unwrap_or_else(|| "controller-bootstrap-secret.log".into());
start_if_ready(check_identity_schema(&db), || async {
    bootstrap_admin(&db, &LogSecretSink(secret_path)).await?;
    let listener = tokio::net::TcpListener::bind(&config.listen_address).await?;
    axum::serve(listener, build_router(db.clone())).await?;
    Ok::<(), Box<dyn std::error::Error>>(())
}).await
```

`main.rs::tests` 测例名 `startup_blocks_bootstrap_and_listener_when_schema_not_ready`；上例 fake 用 `Cell` 计数（不连 DB、不监听端口），Err 门槛计数0、Ok 计数1；`main` 不得打印 `secret_path` 的密码内容。`db.rs::tests` 的 `unauthorized_command_never_invokes_upgrade` 通过与生产共用的 `run_authorized_migration_with(config, connect_and_name, upgrade)` 注入假的实际库名及计数升级器；先 authorize（静态条件），再 SELECT DATABASE 校验实际名字，最后才调用升级器；任何失败计数0。`run_identity_test_migration` 是该 helper 的真实 `connect/SELECT DATABASE()/upgrade_identity_schema` 适配器，bin 仅负责从专用 env 构造 config 并调用，命令不能回退读取 `CONTROLLER_DATABASE_URL`。安全门槛伪代码（`fixture` 只为单测构造 config）：

```rust
fn authorize_test_migration(c: &TestMigrationConfig, actual: &str) -> Result<(), ControllerError> {
    let safe = !c.test_url.trim().is_empty()
        && c.allow_destructive
        && (1..=64).contains(&c.expected_database.len())
        && c.expected_database.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && c.expected_database == actual
        && !c.backup_ref.trim().is_empty()
        && c.migration_ack == "isolated-exclusive-backed-up-disposable";
    if safe { Ok(()) } else { Err(ControllerError::Config("test migration authorization required".into())) }
}
```

此处删除 `test_` 前缀限制不降低其他门槛：非空备份引用和现行 ACK `isolated-exclusive-backed-up-disposable` 仍须对应经操作者核验的**真实备份**；它们自身不是备份证明，用户允许数据可丢弃也不能代替备份。未完成真实备份前不得运行该迁移。

**库名门禁裁决：** 用户两个专用 dev 库库名含连字符；身份以 `SELECT DATABASE()` 与 expected 精确匹配为准，`expected_database` 不进入任何 SQL（固定白名单与参数绑定），故仅最小扩展允许连字符，1..=64 字节与 ASCII 不变；错判代价仅是多允许连字符，不改变任何隔离证明。

生产 bin 的专用环境字段为 `CONTROLLER_TEST_DATABASE_URL`、`CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1`、`CONTROLLER_TEST_EXPECTED_DATABASE`、`CONTROLLER_TEST_BACKUP_REF`、`CONTROLLER_TEST_MIGRATION_ACK=isolated-exclusive-backed-up-disposable`；本地双 dev runner 在进程内读取 ignored secret JSON 并注入这些专用 env，仍由现行 `from_test_env()` 构造配置，不更换接口，也不能将允许数据丢弃代替真实备份或伪造备份引用。空/错值、缺真实备份确认均先拒；同时提供 `CONTROLLER_DATABASE_URL` 且与专用 URL 字符串相同则拒绝（字符串不同**仍不能证明数据库隔离**，须核实实际目标和访问范围）；连库后用 `SELECT DATABASE()` 实际结果与 1..=64 字节、仅 ASCII 字母数字、下划线和连字符的精确期望库名比对，**不要求 `test_` 前缀**。执行者还须在库外确认独占、隔离、停写及真实可恢复备份；用户数据可丢弃授权不代替备份，确认口令/非空引用本身均不证明备份。命令仅支持显式 `--mode upgrade`、`--mode fixture-v1`（仅空库 0001/version1 后退出）或 `--mode fixture-partial-v1`（仅已授权 v1 测试库执行 **0002 中固定的一列**预检+ALTER，保留 version1 以验证可恢复混合态）；后两个模式仅作受控测试 fixture，同样走全部门槛，不接受默认 mode；不提供生产迁移命令。`preflight_usernames` 先逐一用 valid_username 检查现存 usernames，用 `HashSet<Vec<u8>>.insert(s.as_bytes().to_vec())` 拒绝目标字节重复，并以目标 ascii_bin 比较校验无损转换和目标 UNIQUE 冲突。对当前 `users` 的只读预检 SQL（必须在 ALTER 前执行）为：

```sql
SELECT username FROM users;
SELECT 1 FROM users WHERE BINARY username <> BINARY CONVERT(username USING ascii) LIMIT 1;
SELECT CONVERT(username USING ascii) COLLATE ascii_bin AS target_name, COUNT(*) AS n
FROM users GROUP BY target_name HAVING COUNT(*) > 1 LIMIT 1;
```

MySQL/TiDB 对尾空格等目标比较差异还须真库验证，不能只相信 Rust HashSet 或旧 UNIQUE。对十个**仍 signed** 的固定表/列各执行 `SELECT 1 FROM <固定表> WHERE <固定列> < 0 LIMIT 1`；命中报精确列。固定白名单 SQL，不拼用户标识符；全部名字、数值及 schema shape 预检成功前 0 次 ALTER，升级时无并发业务写。

```sql
ALTER TABLE schema_meta MODIFY COLUMN authz_epoch BIGINT UNSIGNED NOT NULL DEFAULT 0;
ALTER TABLE schema_meta MODIFY COLUMN admin_guard_revision BIGINT UNSIGNED NOT NULL DEFAULT 0;
ALTER TABLE users MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL;
ALTER TABLE roles MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL;
ALTER TABLE device_groups MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL;
ALTER TABLE devices MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL;
ALTER TABLE grants MODIFY COLUMN revision BIGINT UNSIGNED NOT NULL;
ALTER TABLE admission_decisions MODIFY COLUMN previous_revision BIGINT UNSIGNED NOT NULL;
ALTER TABLE admission_decisions MODIFY COLUMN new_revision BIGINT UNSIGNED NOT NULL;
ALTER TABLE audit_events MODIFY COLUMN event_seq BIGINT UNSIGNED NOT NULL;
ALTER TABLE users MODIFY COLUMN username VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL;
```

存为新 0002。Task 2 受控入口首次 DDL 之前先按 Global Constraints 核实目标库/备份未应用 Tasks 0002；若已应用即停止。空库先校验无任何表后仅受控入口执行原封不动的 0001 全部 CREATE 与 version1 singleton，再重新分类并执行 0002；`--mode fixture-v1` 在此结束，`--mode upgrade` 继续。对 `schema_meta` 两列额外读取 `COLUMN_DEFAULT` 并检查旧/目标都是 0（以及 nullability），避免 `MODIFY COLUMN` 意外改变默认值；未知默认值报人工处理。已有 version1 旧/混合列不能先跑旧版完整严格校验拦截部分恢复，仍需检查非目标表/索引/CHECK。按实时元数据旧列执行对应 DDL 并重读验证，新列跳过；异常形状/无法恢复报人工处理。完整目标列/索引/CHECK 成功后，`UPDATE schema_meta SET schema_version=2 WHERE singleton=1 AND schema_version=1` 要求影响 1 行并读回2。version2 普通启动仅只读校验目标、不 CREATE/ALTER/UPDATE；受控命令遇已 v2 也只读验证并返回。版本写入结果未知须重读 version/metadata，不能推断 DDL 回滚。普通 `bootstrap_admin` 永远在只读检查 Ok 之后才可能运行；v1/空库不得记录或输出管理员秘密，不得绑定业务监听。
- [ ] **Step 4 显式授权 fixture 与行为 RED→GREEN。** `tests/common/mod.rs` 先为 fixture 的纯门槛加可编译单测：缺 destructive、真实备份引用、确认口令、预期库名之一均拒；旧的仅“库名含 test”放行必须 RED，再 GREEN；数据库版本/metadata 只由不写入的 `check_identity_schema` 检验。`required_fresh_identity_db()` 先调用同一测试授权门槛并核验 `SELECT DATABASE()` 精确匹配、`information_schema.tables` 计数为0；`required_prepared_identity_db()` 核验同一门槛和 v2 只读检查、不负责迁移。下列 helper 是**示意完整调用顺序**（复用 Step 3 的 `TestMigrationConfig::from_test_env()` 和 `authorize_test_migration()`，公开只读入口必须可供 integration tests 调用）：

```rust
pub async fn required_fresh_identity_db() -> DbPool {
    let config = rsetup_controller::TestMigrationConfig::from_test_env().unwrap();
    let db = required_test_db().await;
    let actual: String = sqlx::query_scalar("SELECT DATABASE()")
        .fetch_one(&db.0).await.unwrap();
    config.authorize(&actual).unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(count, 0, "fresh disposable test schema required");
    db
}
pub fn run_explicit_identity_test_command(mode: &str) {
    assert!(matches!(mode, "upgrade" | "fixture-v1" | "fixture-partial-v1"));
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
        .args(["--mode", mode]).status().unwrap();
    assert!(status.success(), "explicit migration command rejected fixture");
}
```

调用命令的 test wrapper 必须先核对当前目标库身份与本地授权。本地 runner 按引擎串行复用两个专用 dev 库：每例先精确核对 `SELECT DATABASE()` 与 ignored JSON 所配目标，再仅在该已授权目标库内顺序清理/重置表并确认目标形状，不 `DROP DATABASE`、本轮不 `CREATE DATABASE`；完成真实备份和其余门槛前不执行要求备份的迁移，同一例中重启/恢复仍可复用同库。不能以单个 URL 字符串、fixture 的确认口令或 `test_` 前缀证明隔离/独占；绝不自动覆盖其他库。另有每例独立空库的测试方式也须满足同样真实备份门槛。旧 v1/混合负例先用该**显式** `--mode fixture-v1` 准备 v1 再写测试数据和目标列故障，先检查普通 `check_identity_schema` 结构化 not ready/无额外 DDL、bootstrap/监听不发生，再通过 `--mode upgrade` 预检或恢复；不在 ignored case 内直接运行私有 upgrade 或让 `migrate(&db)` 偷渡 DDL。新空库正例只由受控命令连续执行 0001→0002，再调用只读检查：

```rust
#[tokio::test]
#[ignore = "explicit isolated disposable DB URL and operator authorization required"]
async fn identity_contract_fresh_and_restart() {
    let db = common::required_fresh_identity_db().await;
    common::run_explicit_identity_test_command("upgrade");
    rsetup_controller::check_identity_schema(&db).await.unwrap();
    let v: i32 = sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton=1")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(v, 2);
    rsetup_controller::check_identity_schema(&db).await.unwrap();
    let again: i32 = sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton=1")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(again, 2);
}
```

两个 test target 均替换旧 `use rsetup_controller::migrate`；遍历现有 `fresh_bootstrap`、`bootstrap_twice_*`、`concurrent_bootstraps_*`、`empty_users_*`、`bootstrap_failure_*`、`migration_is_repeatable_and_repairs_interrupted_ddl`、`migration_rejects_wrong_named_table_before_bootstrap`、`admission_cas_*` 里的每个旧调用：前置显式授权 fresh fixture + 命令升级，v2 才由只读 `check_identity_schema(&db)` 检查；原“DROP TABLE 后 migrate 自修复”改为“普通检查拒绝且绝不重建表，受控命令对于非目标表缺失报人工处理而非悄悄 CREATE”，原重命名错误表必须只读拒绝且不自动覆盖；旧 bootstrap 用例不应再删/重置**已有库**的管理员凭据；本地仅在逐例核对后的两个可丢弃 dev 目标内重置，通用方案才使用每例新库已授权 fixture。两引擎另各自演练 legacy v1→2、先 ALTER 一列后重启、v2 只读重启；每个新旧库分别注入 Unicode/>64/非法/大写用户名和十个 signed 列的负值（不能在已 unsigned 的列注入），断言失败前 0 ALTER、version1、值无损；模拟后续 DDL 故障再试恢复或报明确人工处理。目标唯一冲突仅在旧唯一索引允许构造的引擎中用真实 fixture，否则记录不可构造、仍运行纯碰撞测试；设置失败不能冒充 RED。真库 RED 仅来自运行至值/metadata/version 断言，随后最小 GREEN；没有 URL 时只声明未验证。
- [ ] **Step 5 本地安全回归和双引擎门槛。** 逐个重跑 Step 2 的六个本地测试名，再跑 `cargo test -p rsetup-controller --lib db::tests && cargo test -p rsetup-controller --bin rsetup-controller && cargo fmt --all -- --check`；补测 fake 启动不调用 bootstrap/listen、CLI 缺 URL/错库/缺真实备份/缺独占确认都在 0 次 upgrade 前拒绝、空/v1/v2/未知/损坏版的只读判断。通用方案是操作者预置**独立空且已备份**测试库；本地双 dev 库按上文后续授权修订逐例顺序清表复用，在库外确认停写/独占/隔离、记录目标身份；迁移须先完成真实备份并保留真实回执，未完成则不运行。另有逐例独立空库的命令示例仅在该目标已具备真实备份与其余授权门槛时适用；本地双 dev runner 串行复用指定库，不直接使用示例中的逐例 URL/库名/备份引用，更不得据此填假备份（勿在日志打印 URL）：

```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1 \
CONTROLLER_TEST_EXPECTED_DATABASE=test_identity_001 CONTROLLER_TEST_BACKUP_REF="$MYSQL_BACKUP_REF" \
CONTROLLER_TEST_MIGRATION_ACK=isolated-exclusive-backed-up-disposable \
cargo test -p rsetup-controller --test mysql_identity identity_contract_fresh_and_restart -- --ignored --exact --test-threads=1
```

TiDB 的示例换对应 `$TIDB_TEST_URL`/`tidb_identity` 和精确库名/真实备份引用；本地双 dev runner 每例在指定已授权库顺序重置并核对目标，备份与其他门槛均满足后才调用当前入口。测试内由 `run_explicit_identity_test_command("upgrade")` 调用唯一迁移入口，直接运行命令时 `cargo run -p rsetup-controller --bin migrate-identity-test -- --mode upgrade` 也必须具备同样专用本地授权，绝不由普通 `cargo run -p rsetup-controller` 隐式运行；旧库负例用 `--mode fixture-v1` 之后再注入并用 `--mode upgrade`。每引擎每场景核查实际 1 用例/0 失败、`SELECT VERSION()`、DDL 后 metadata/version；尚无实际测试通过证据，缺 URL 未验证，有 URL 失败/零用例为失败。本地 fake 测试不证明数据库独占或生产许可。
- [ ] **Step 6 独立审查/提交。** 审核零截断、无损恢复、version 边界、普通 main 严格只读、`migrate` 旧导出/所有旧 ignored wrapper 不遗留自动修复、命令单独显式且只认测试 URL；`git add crates/rsetup-controller/Cargo.toml crates/rsetup-controller/migrations/0002_identity_contract.sql crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/lib.rs crates/rsetup-controller/src/error.rs crates/rsetup-controller/src/db.rs crates/rsetup-controller/src/bin/migrate-identity-test.rs crates/rsetup-controller/tests/common/mod.rs crates/rsetup-controller/tests/mysql_identity.rs crates/rsetup-controller/tests/tidb_identity.rs && git commit -m 'feat(controller): gate test-only identity migration'`。

### Task 3: 全业务 u64、高半区、溢出与 JSON

**Files:** Modify/Test `crates/rsetup-controller/src/model.rs`、`src/db.rs`、`src/auth/service.rs`、`tests/common/mod.rs`、`tests/mysql_identity.rs`、`tests/tidb_identity.rs`（省略前缀者均在 controller crate 内）。

**Interfaces:** `AdmissionSnapshot.revision: u64`；`AdmissionStore::compare_and_set(public_key: [u8;32],expected_revision: u64,expected_state: AdmissionState,decision: ReviewDecision,actor_id: Option<[u8;16]>,reason: Option<&str>) -> impl Future<Output=Result<AdmissionSnapshot,ControllerError>> + Send`；`IdentityUser.revision: u64`；`fn next_event_seq(counter: &AtomicU64) -> Result<u64,ControllerError>`；`pub fn decimal_u64_json(value: u64) -> serde_json::Value`。COUNT(*) 和 schema_version 的非业务整数类型保留。

- [ ] **Step 1 先行为 RED。** 在 `db.rs::tests` 用旧可编译类型写：

```rust
#[test]
fn admission_revision_crosses_signed_boundary() {
    let current = AdmissionSnapshot { admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::None, revision: i64::MAX as _ };
    let next = next_snapshot(current, ReviewDecision::Approved).unwrap();
    assert_eq!(next.revision as u64, i64::MAX as u64 + 1);
}
```

运行 `cargo test -p rsetup-controller admission_revision_crosses_signed_boundary`；须因旧 i64 checked_add 错拒高半区而断言 RED，不是签名/编译失败。
- [ ] **Step 2 最少 GREEN。** `model.rs` 快照、`db.rs` trait 参数/decode_snapshot/SQLx `row.try_get::<u64,_>("revision")?`/CAS UPDATE 和 admission_decisions `.bind(u64)`、`auth/service.rs` 的 IdentityUser/fake 快照同时改 u64。所有业务 revision +1 使用 `.checked_add(1).ok_or(ControllerError::RevisionConflict)?`。`tests/common/mod.rs` 审计 `(process_epoch,event_seq)` 解码 u64、next_seq checked；COUNT(*) 继续 i64。目标测试和 `cargo test -p rsetup-controller` 绿。
- [ ] **Step 3 audit/JSON 独立行为 RED。** 先新增可编译的错误桩 `next_event_seq` 返回 `Ok(counter.fetch_add(1, Ordering::Relaxed))`、`decimal_u64_json(value: u64) -> serde_json::Value { serde_json::json!(value) }`；再写：

```rust
#[test]
fn audit_sequence_never_wraps() {
    let seq = std::sync::atomic::AtomicU64::new(u64::MAX);
    assert!(next_event_seq(&seq).is_err());
    assert_eq!(seq.load(std::sync::atomic::Ordering::Relaxed), u64::MAX);
}
#[test]
fn revision_is_exact_decimal_json_string() {
    let v = i64::MAX as u64 + 9;
    assert_eq!(crate::model::decimal_u64_json(v), serde_json::json!(v.to_string()));
    assert_eq!(crate::model::decimal_u64_json(u64::MAX), serde_json::json!(u64::MAX.to_string()));
}
```

分别运行 `cargo test -p rsetup-controller audit_sequence_never_wraps` 与 `cargo test -p rsetup-controller revision_is_exact_decimal_json_string`；wrap/JSON number 必须行为断言红。
- [ ] **Step 4 GREEN。** `AtomicU64::fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))` 返回旧 seq、溢出映射 RevisionConflict；生产审计 `.bind(seq: u64)`，溢出时事务不提交。插审计失败可导致 seq 空洞，不伪称全序。`decimal_u64_json` 返回 `serde_json::Value::String(value.to_string())`；两目标测试绿。对 u64::MAX 准入转移另做 RED→GREEN，断言 RevisionConflict 与无 history/audit 新写入；后续 API 接线直接使用字符串契约。
- [ ] **Step 5 双库 SQLx bind/Row 实证。** `tests/common/mod.rs` 加共享 ignored 测试函数 `identity_unsigned_high_half_round_trip(db: &DbPool)`：独立新库迁移后使用现有 `unique_public_key()`，先以最小高半区场景证明**实际** bind/decode/CAS（文件顶端已有 DbPool；函数内导入 trait）：

```rust
pub async fn identity_unsigned_high_half_round_trip(db: &DbPool) {
    use rsetup_controller::{AdmissionState, AdmissionStore, ReviewDecision};
    use sqlx::Row;
    let key = unique_public_key();
    let high = i64::MAX as u64 + 9;
    sqlx::query("INSERT INTO devices (public_key,display_name,admission_state,review_decision,revision,archived) VALUES (?,'high','PENDING','none',?,FALSE)")
        .bind(key.as_slice()).bind(high).execute(&db.0).await.unwrap();
    assert_eq!(db.load(key).await.unwrap().revision, high);
    let next = db.compare_and_set(key, high, AdmissionState::Pending,
        ReviewDecision::Approved, None, None).await.unwrap();
    assert_eq!(next.revision, high + 1);
    let row = sqlx::query("SELECT revision FROM devices WHERE public_key=?")
        .bind(key.as_slice()).fetch_one(&db.0).await.unwrap();
    assert_eq!(row.try_get::<u64, _>("revision").unwrap(), high + 1);
}
```

同一 helper 继续以 Row `try_get::<u64,_>` 读 admission_decisions.previous_revision/new_revision；旧 expected_revision 再 CAS 冲突且无新增审计。对 schema_meta 两列、users/roles/device_groups/grants.revision、audit_events.event_seq **逐列** `.bind(high)`/Row u64 decode 比对（审计保持唯一 `(process_epoch,event_seq)`），另以 `u64::MAX` 核查 bind/decode 与 checked 溢出；这些逐列断言必须随 helper 一起完成，不能只测设备便宣称十列全过。两个 test target 各加 ignored wrapper：先 `common::required_fresh_identity_db().await`（同一多项授权/空库/真实库名检查），再 `common::run_explicit_identity_test_command("upgrade")` 令独立受控命令执行 0001→0002，再 `rsetup_controller::check_identity_schema(&db).await.unwrap()` 只读确认 v2，最后 `common::identity_unsigned_high_half_round_trip(&db).await`。绝不能以已改为只读的 `migrate(&db)` 作空库升级。下列逐例独立新库的命令示例只在该目标已有真实备份和其余门槛满足时适用；本地双 dev runner 仍按上述方式核对并串行清表复用指定库，不直接使用示例的 URL/库名/备份引用：

```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1 \
CONTROLLER_TEST_EXPECTED_DATABASE=test_identity_high_001 CONTROLLER_TEST_BACKUP_REF="$MYSQL_BACKUP_REF" \
CONTROLLER_TEST_MIGRATION_ACK=isolated-exclusive-backed-up-disposable \
cargo test -p rsetup-controller --test mysql_identity identity_unsigned_high_half_round_trip -- --ignored --exact --test-threads=1
```

TiDB 示例换对应 URL/target/精确库名和真实备份引用；本地双 dev runner 不要求逐例独立 URL，而是逐例在对应已授权库核对目标并顺序清表，真实备份门槛不变。两库各须记录 `SELECT VERSION()`、实际用例数与失败数；目前没有真实验收通过证据，缺 URL 未验证。Rust 静态测试绿不是数据库验收。
- [ ] **Step 6 独立审查/提交。** 审核十列 bind/decode、CAS、AtomicU64 与 JSON 精度；`cargo fmt --all -- --check && cargo test -p rsetup-controller && cargo clippy -p rsetup-controller --all-targets -- -D warnings`；`git add crates/rsetup-controller/src/model.rs crates/rsetup-controller/src/db.rs crates/rsetup-controller/src/auth/service.rs crates/rsetup-controller/tests/common/mod.rs crates/rsetup-controller/tests/mysql_identity.rs crates/rsetup-controller/tests/tidb_identity.rs && git commit -m 'fix(controller): use unsigned identity revisions'`。

### Task 4: 未来 Tasks 编号与版本交接

**Files:** Modify `docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`；只读核查 identity 0001/0002。

**Interfaces:** identity 0002 成功后 schema_version=2；仅确认无已应用 Tasks 0002，未来 03 Task 2 才建 `migrations/0003_tasks.sql`；Tasks 完整验证后版本 3、版本 3 重启稳定。

- [ ] **Step 1 编号 RED。** `git ls-files crates/rsetup-controller/migrations` 核对 repo；目标库/备份只读核查 schema_version/Tasks 表，任何已应用 Tasks 0002 停下另审 `0003_identity_contract`/Tasks 后移。运行 `rg -n '0002_tasks\.sql|0003_tasks\.sql|schema_version' docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`；现有文件清单、Task 2 Files、Task 2 git add 共三处旧名，为可检查的编号 RED。
- [ ] **Step 2 GREEN。** 三处都改 `0003_tasks.sql`；03 Global Constraints 与 Task 2 写清 identity v2 前置、Tasks 全目标验证后 v3、v3 重启校验。既有 Tasks v2 必须另审 identity 0003 和 Tasks 后移，绝不机械重命名。01 计划“后续迁移不改 0001”依旧自洽，不为旧示例 display_name/schema_version/index 差异改 01。
- [ ] **Step 3 独立审查/提交。** 重跑 Step 1 rg：零旧名、三处新名、版本交接一致；`git diff --check`；审查异常库边界。`git add docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md && git commit -m 'docs(controller): reserve task migration version three'`。

## 完成判据

Task 1 不依赖 DB 的 signed 同宽/字符集/用户名行为 RED→GREEN；Task 2 普通启动只读 `check_identity_schema`：空库/v1/混合态结构化 not ready，绝无 0001 CREATE/INSERT、0002 ALTER/UPDATE、bootstrap 及业务监听；只有 `migrate-identity-test` 在**显式专用**测试 URL、`CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1`、1..=64 字节且仅 ASCII 字母数字、下划线和连字符的精确库名、已完成且核验的真实备份（含目标匹配回执与备份文件 SHA256）、非空真实备份引用、精确 ACK 和独占/隔离/可丢弃人工确认并记录之后才调用私有升级函数，测试库先 0001→0002、旧 v1/部分 DDL 仅受控恢复、v2 普通重启只读。生产 ALTER 未批准，库名是否含 test、URL 字符串不同和单个 env 均不足以证明独占。Task 3 高半区/checked 加一、AtomicU64/JSON string 及两引擎真实 SQLx bind+Row；Task 4 03 文件名和 v2→v3 自洽。每个新安全门槛都有先旧行为 RED 再 GREEN 的可编译纯函数/fake/startup 注入测试，额外负例同样独立审查小提交。最终 `git diff --check`、`cargo test -p rsetup-controller`、`cargo test --workspace --locked` 都不运行 ignored 真实库用例；目前两库真实验收仍**未验证**，不能据此声称两库兼容或生产迁移获批。

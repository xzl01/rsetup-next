# Controller identity schema correction TDD 实施计划

> **致执行代理：** 单独获准实施后按 superpowers:executing-plans 逐项执行；本计划不是生产迁移许可。

**Goal:** 保留已提交 0001，新增 identity 0002 修正 Revision/Counter 为 `BIGINT UNSIGNED`/u64、用户名为 `VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin`；未来 Tasks 顺延为 0003。

**Architecture:** 先用无需数据库的行为 RED 锁定 metadata、用户名、u64 边界，再在独占可丢弃库全量预检、按元数据逐条恢复非事务 DDL；所有目标 schema 验证后才升级版本。未来 HTTP 投影需把 u64 写成 JSON 十进制字符串。

**Tech Stack:** Rust 2024 / MSRV 1.85、SQLx MySQL driver、MySQL 8.4 LTS、TiDB 8.5 LTS。

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md` §2；`docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md` §§1–2；`docs/superpowers/plans/2026-10-02-controller-01-identity-data-tdd.md` migration/Task 6；`docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`。

## Global Constraints

- 用户仅批准**开发/测试设计**，未授权真实生产库 ALTER、备份原件迁移或兼容性宣告。当前没有 `MYSQL_TEST_URL`/`TIDB_TEST_URL`，两引擎真实验收全部**未验证**。破坏性 fixture 只用独占、空、可丢弃测试实例/库；现有 `required_test_db()` 只检查库名含 test，绝非充分安全门槛。不得触碰 `.cargo-home/`。
- `crates/rsetup-controller/migrations/0001_identity_devices.sql` 已提交且逐字保留。repo 当前只有 0001；新增 `0002_identity_contract.sql`，03 计划未来三处 `0002_tasks.sql` 更名为 `0003_tasks.sql`。实施前只读核查目标库/备份：如 Tasks 0002 已应用，绝不可重用/重命名，必须停下另审 `0003_identity_contract.sql` 并顺延 Tasks；默认编号方案不适用该情形。
- 所有十个业务 Revision/Counter SQL 列均 `BIGINT UNSIGNED NOT NULL` ↔ Rust u64：`schema_meta.authz_epoch,admin_guard_revision`、`users.revision`、`roles.revision`、`device_groups.revision`、`devices.revision`、`grants.revision`、`admission_decisions.previous_revision,new_revision`、`audit_events.event_seq`。`schema_meta.schema_version INT`、SQL `COUNT(*)`、索引 `non_unique` 的 i64 非业务 Counter；旧示例 display_name 宽度和 ix/idx 别名不属强制缺陷。
- 用户名 SQL `VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL`；业务校验 ASCII 小写 3–64，首字符字母/数字，其余字母/数字/点/下划线/连字符。Unicode/长度/非法字符或大写、目标 ascii_bin 唯一冲突、负 revision/epoch/event_seq，均须在第一次 DDL **之前** fail closed，绝不截断、折叠、覆盖。
- MySQL/TiDB DDL 非事务，不能依赖 `SELECT ... FOR UPDATE` 跨 DDL 保持预检；测试实例必须停业务写和第二迁移执行者，生产维护/备份恢复另审。version1 的旧/目标混合列可能是中断状态：先按实时 metadata 分类、重新预检仍旧列、只 ALTER 旧列并逐条重读；旧 version1 完整严格校验不得阻碍混合列恢复。未知形状/无损升级不可能要报需人工处理，不能声称 DDL 自动回滚。完整目标校验成功后才条件推进 schema_version 1→2 并查一行，version2 重启校验目标保持2；版本写返回不确定必须重读 version/metadata。
- 每项先建立可编译、达到目标断言的行为 RED，再最小 GREEN。SQLx 编译失败、缺 helper/URL、连接错误、ignored 跳过或零用例均非行为 RED/PASS。静态 Rust 单测不等于 MySQL8.4/TiDB8.5 的真实 u64 bind/Row 解码、CAS/DDL 恢复验收；每引擎记录 `SELECT VERSION()`、实际用例数和失败数；有 URL 失败/0用例为失败，缺 URL 未验证。
- JSON 对外 Revision/Counter 是十进制**字符串**；现有 `build_router(DbPool)` 只有 healthz/readyz，先提供转换接口、后续 API 任务接线，不假称已存在管理端点。每项独立审查、频繁提交，不改无关文件。

## 文件结构、职责和接口

- Create `crates/rsetup-controller/migrations/0002_identity_contract.sql`：十个数字列、一个用户名列的固定 ALTER，仅由安全预检驱动运行。
- Modify `crates/rsetup-controller/src/db.rs`：information_schema、预检、恢复/version、AdmissionStore/audit；`src/model.rs`：快照 u64/JSON；`src/auth/service.rs`：IdentityUser/fake u64 checked +1。
- Modify/Test `crates/rsetup-controller/tests/common/mod.rs`：隔离库 fixture、audit seq u64；`tests/mysql_identity.rs` 与 `tests/tidb_identity.rs`：ignored 双库场景。
- Modify（仅未来 Task 4）`docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`：三处编号/版本交接。本文撰写阶段不修改旧计划；永远不改 0001。

---

### Task 1: schema validator 与用户名行为

**Files:** Modify/Test `crates/rsetup-controller/src/db.rs`（现有 `ColumnMeta`、`validate_column_shape`、全部旧 `ColumnMeta` fixture 和测试）。

**Interfaces:** 保留 `validate_column_shape(table: &str, expected: &str, actual: &ColumnMeta) -> Result<(),ControllerError>`；`ColumnMeta` 新增 `character_set_name: Option<String>, collation_name: Option<String>`；新增 `fn valid_username(s: &str) -> bool` 给 Task 2 使用。

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
fn valid_username(s: &str) -> bool {
    let bytes = s.as_bytes();
    (3..=64).contains(&bytes.len())
        && bytes.first().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()
            || matches!(*b, b'.' | b'_' | b'-'))
}
```

首字节判别同时阻止 Unicode/非 ASCII；不做折叠/截断。最终目标列由新 0002 声明覆盖，不能让旧 0001 signed/varchar128 作为最终预期。
- [ ] **Step 4 回归。** 重跑三个目标命令及 `cargo test -p rsetup-controller db::tests`；表驱动对十列各验证 signed 错、unsigned 对；保持 `schema_version INT`、BOOLEAN/JSON、BINARY 长度、索引/CHECK 老测试绿。`cargo fmt --all -- --check && cargo test -p rsetup-controller`。
- [ ] **Step 5 审查/提交。** 独立审查 metadata 字段在实际查询读取且未让所有字符列变 ascii；`git add crates/rsetup-controller/src/db.rs && git commit -m 'test(controller): enforce identity column metadata'`。

### Task 2: 全量预检、非事务 DDL 恢复与版本 2

**Files:** Create `crates/rsetup-controller/migrations/0002_identity_contract.sql`；Modify/Test `crates/rsetup-controller/src/db.rs`、`crates/rsetup-controller/tests/common/mod.rs`、`tests/mysql_identity.rs`、`tests/tidb_identity.rs`。

**Interfaces:** 保留 `pub async fn migrate(db: &DbPool) -> Result<(),ControllerError>`；新 `fn preflight_usernames<'a>(values: impl IntoIterator<Item=&'a str>) -> Result<(),ControllerError>` 调 Task 1 validator；新 `fn classify_identity_column(table: &str, name: &str, actual: &ColumnMeta) -> Result<bool,ControllerError>`，true=精确旧列待 ALTER，false=精确目标列。旧 utf8mb4 唯一键不能替代目标排序规则检查。

- [ ] **Step 1 纯行为测试。** 为两个函数先放可编译但错误“全部接受”桩，再在 `db.rs::tests` 加：

```rust
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

- [ ] **Step 2 RED。** `cargo test -p rsetup-controller preflight_refuses_invalid_and_target_collision`、`cargo test -p rsetup-controller mixed_identity_columns_are_resumable`；须因非法值/未知形状行为断言失败。无 URL 的 ignored case 不构成 RED。
- [ ] **Step 3 最小实现。** 先逐一用 valid_username 检查现存 usernames，用 `HashSet<Vec<u8>>.insert(s.as_bytes().to_vec())` 拒绝目标字节重复，并以目标 ascii_bin 比较校验无损转换和目标 UNIQUE 冲突。对当前 `users` 的只读预检 SQL（必须在 ALTER 前执行）为：

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

存为新 0002。Task 2 首次 DDL 之前先按 Global Constraints 核实目标库/备份未应用 Tasks 0002；若已应用即停止。对 `schema_meta` 两列额外读取 `COLUMN_DEFAULT` 并检查旧/目标都是 0（以及 nullability），避免 `MODIFY COLUMN` 意外改变默认值；未知默认值报人工处理。新库 0001 建表并插 version1 行→0002；已有 version1 旧/混合列不能先跑旧版完整严格校验拦截部分恢复，仍需检查非目标表/索引/CHECK。按实时元数据旧列执行对应 DDL 并重读验证，新列跳过；异常形状/无法恢复报人工处理。完整目标列/索引/CHECK 成功后，`UPDATE schema_meta SET schema_version=2 WHERE singleton=1 AND schema_version=1` 要求影响 1 行并读回2。version2 重启验证目标、不 ALTER；版本写入结果未知须重读 version/metadata，不能推断 DDL 回滚。
- [ ] **Step 4 隔离 DB fixture 和 RED→GREEN。** `tests/common/mod.rs` 定义 `required_fresh_identity_db()`：调用现有 `required_test_db()`，再要求 `CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1`、执行者外部确认独占可丢弃实例、目标 schema 的 `information_schema.tables` 计数为0；非空拒绝，不自动 DROP。`legacy_identity_schema(db: &DbPool)` 仅对全新空库执行 0001 并插入 version1 singleton；每个案例用各自独立新 disposable schema/URL，不复用已有数据。两个 helper 可直接落地为：

```rust
pub async fn required_fresh_identity_db() -> DbPool {
    assert!(std::env::var("CONTROLLER_TEST_ALLOW_DESTRUCTIVE")
        .as_deref().is_ok_and(|value| value == "1"));
    let db = required_test_db().await;
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(count, 0, "only a newly provisioned disposable schema is allowed");
    db
}
pub async fn legacy_identity_schema(db: &DbPool) {
    for statement in include_str!("../../migrations/0001_identity_devices.sql")
        .split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(statement).execute(&db.0).await.unwrap();
    }
    let id = uuid::Uuid::new_v4();
    sqlx::query("INSERT INTO schema_meta (singleton,schema_version,instance_id,initialized,authz_epoch,admin_guard_revision) VALUES (1,1,?,FALSE,0,0)")
        .bind(id.as_bytes().as_slice()).execute(&db.0).await.unwrap();
}
```

调用 legacy helper 前必须先通过 fresh helper 的门槛。

```rust
#[tokio::test]
#[ignore = "explicit isolated disposable DB URL required"]
async fn identity_contract_fresh_and_restart() {
    let db = common::required_fresh_identity_db().await;
    migrate(&db).await.unwrap();
    let v: i32 = sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton=1")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(v, 2);
    migrate(&db).await.unwrap();
    let again: i32 = sqlx::query_scalar("SELECT schema_version FROM schema_meta WHERE singleton=1")
        .fetch_one(&db.0).await.unwrap();
    assert_eq!(again, 2);
}
```

两个 test target 各加上例（已有 `use rsetup_controller::migrate`），还需各自演练 legacy version1→2、先 ALTER 一列后重启、version2 重启；每个新旧库分别注入 Unicode/>64/非法/大写用户名和十个 signed 列的负值（不能在已 unsigned 的列注入），断言失败前 0 ALTER、version1、值无损；模拟后续 DDL 故障再试恢复或报明确人工处理。目标唯一冲突仅在旧唯一索引允许构造的引擎中用真实 fixture，否则记录它不可构造、仍运行纯碰撞测试；设置失败不能冒充 RED。真实 RED 应来自运行至值/metadata/version 断言，随后最小实现 GREEN。
- [ ] **Step 5 双引擎门槛。** 每例独立预置空库，按例运行如 `CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1 CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test mysql_identity identity_contract_fresh_and_restart -- --ignored --exact --test-threads=1`；TiDB 换 TIDB_TEST_URL/`tidb_identity`，其他案例同样逐一 `--exact`。每引擎每场景检查实际 1 用例/0 失败、SELECT VERSION()、DDL 后 metadata/version；缺 URL 未验证，有 URL 失败/零用例为失败。`cargo test -p rsetup-controller db::tests && cargo fmt --all -- --check` 不替代两库。
- [ ] **Step 6 独立审查/提交。** 审核零截断、无损恢复、version 边界；`git add crates/rsetup-controller/migrations/0002_identity_contract.sql crates/rsetup-controller/src/db.rs crates/rsetup-controller/tests/common/mod.rs crates/rsetup-controller/tests/mysql_identity.rs crates/rsetup-controller/tests/tidb_identity.rs && git commit -m 'feat(controller): stage recoverable identity migration'`。

### Task 3: 全业务 u64、高半区、溢出与 JSON

**Files:** Modify/Test `crates/rsetup-controller/src/model.rs`、`src/db.rs`、`src/auth/service.rs`、`tests/common/mod.rs`、`tests/mysql_identity.rs`、`tests/tidb_identity.rs`（省略前缀者均在 controller crate 内）。

**Interfaces:** `AdmissionSnapshot.revision: u64`；`AdmissionStore::compare_and_set(public_key: [u8;32],expected_revision: u64,expected_state: AdmissionState,decision: ReviewDecision,actor_id: Option<[u8;16]>,reason: Option<&str>) -> impl Future<Output=Result<AdmissionSnapshot,ControllerError>> + Send`；`IdentityUser.revision: u64`；`fn next_event_seq(counter: &AtomicU64) -> Result<u64,ControllerError>`；`pub fn decimal_u64_json(value: u64) -> serde_json::Value`。COUNT(*) 和 schema_version 的非业务整数类型保留。

- [ ] **Step 1 先行为 RED。** 在 `db.rs::tests` 用旧可编译类型写：

```rust
#[test]
fn admission_revision_crosses_signed_boundary() {
    let current = AdmissionSnapshot { admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::None, revision: i64::MAX };
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

同一 helper 继续以 Row `try_get::<u64,_>` 读 admission_decisions.previous_revision/new_revision；旧 expected_revision 再 CAS 冲突且无新增审计。对 schema_meta 两列、users/roles/device_groups/grants.revision、audit_events.event_seq **逐列** `.bind(high)`/Row u64 decode 比对（审计保持唯一 `(process_epoch,event_seq)`），另以 `u64::MAX` 核查 bind/decode 与 checked 溢出；这些逐列断言必须随 helper 一起完成，不能只测设备便宣称十列全过。两个 test target 各加 ignored wrapper：先 `common::required_fresh_identity_db().await`，再 `migrate(&db).await.unwrap()`，最后 `common::identity_unsigned_high_half_round_trip(&db).await`。分别在独立新库执行 `CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1 CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test mysql_identity identity_unsigned_high_half_round_trip -- --ignored --exact --test-threads=1`，TiDB 换 URL/target；两库各记录 SELECT VERSION()、实际 1 用例/0 失败，缺 URL 未验证。Rust 静态测试绿不是数据库验收。
- [ ] **Step 6 独立审查/提交。** 审核十列 bind/decode、CAS、AtomicU64 与 JSON 精度；`cargo fmt --all -- --check && cargo test -p rsetup-controller && cargo clippy -p rsetup-controller --all-targets -- -D warnings`；`git add crates/rsetup-controller/src/model.rs crates/rsetup-controller/src/db.rs crates/rsetup-controller/src/auth/service.rs crates/rsetup-controller/tests/common/mod.rs crates/rsetup-controller/tests/mysql_identity.rs crates/rsetup-controller/tests/tidb_identity.rs && git commit -m 'fix(controller): use unsigned identity revisions'`。

### Task 4: 未来 Tasks 编号与版本交接

**Files:** Modify `docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`；只读核查 identity 0001/0002。

**Interfaces:** identity 0002 成功后 schema_version=2；仅确认无已应用 Tasks 0002，未来 03 Task 2 才建 `migrations/0003_tasks.sql`；Tasks 完整验证后版本 3、版本 3 重启稳定。

- [ ] **Step 1 编号 RED。** `git ls-files crates/rsetup-controller/migrations` 核对 repo；目标库/备份只读核查 schema_version/Tasks 表，任何已应用 Tasks 0002 停下另审 `0003_identity_contract`/Tasks 后移。运行 `rg -n '0002_tasks\.sql|0003_tasks\.sql|schema_version' docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md`；现有文件清单、Task 2 Files、Task 2 git add 共三处旧名，为可检查的编号 RED。
- [ ] **Step 2 GREEN。** 三处都改 `0003_tasks.sql`；03 Global Constraints 与 Task 2 写清 identity v2 前置、Tasks 全目标验证后 v3、v3 重启校验。既有 Tasks v2 必须另审 identity 0003 和 Tasks 后移，绝不机械重命名。01 计划“后续迁移不改 0001”依旧自洽，不为旧示例 display_name/schema_version/index 差异改 01。
- [ ] **Step 3 独立审查/提交。** 重跑 Step 1 rg：零旧名、三处新名、版本交接一致；`git diff --check`；审查异常库边界。`git add docs/superpowers/plans/2026-10-02-controller-03-tasks-runtime-tdd.md && git commit -m 'docs(controller): reserve task migration version three'`。

## 完成判据

Task 1 不依赖 DB 的 signed 同宽/字符集/用户名行为 RED→GREEN；Task 2 预检、空库 0001→0002、旧 v1/部分 DDL/v2 重启；Task 3 高半区/checked 加一、AtomicU64/JSON string 及两引擎真实 SQLx bind+Row；Task 4 03 文件名和 v2→v3 自洽。额外负例也逐项行为 RED→GREEN、独立审查小提交。最终 `git diff --check`、`cargo test -p rsetup-controller`、`cargo test --workspace --locked` 都不运行 ignored 真实库用例；缺 URL 时仍是**未验证**，不能据此声称两库兼容或生产迁移获批。

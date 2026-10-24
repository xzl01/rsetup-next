# fresh 单次同类生产 JSON 读取收口报告（tunnel-json-read-sweep）

* 日期：2026-10-24（Asia/Shanghai）
* 范围：`crates/rsetup-controller/src`（仅 controller 库的**生产 SELECT 投影**）
* 结论：**2 条读 SQL（共 3 个 JSON 列投影）的裸读已收口**——`devices::repository::AUDIT_PAGE_SQL` 的 `params_redacted` + `time_evidence`、`integrity` 全表 `main_tasks.counts_json` 扫描；另 **13 条读 SQL（12 条 JSON 列投影 + 1 条 `OCTET_LENGTH` 谓词）确认早已 `CAST(... AS CHAR)`**、**4 列确认无生产读路径**、**12 条写 SQL 确认不属于本 bug**。全量 controller `--lib` 929 通过、clippy `-D warnings` 干净、包内 `cargo fmt --check` 干净。
* 证据性质：**纯源码/离线合同证据 + 真实 RED→GREEN 回归**。本 RC 未连接任何真实数据库、未读 `secret/`，不声称真库已复跑。

---

## 1. 根因与判定口径（为什么不加 `json` feature 也必须 CAST）

1. `crates/rsetup-controller/schema/v4.sql` 把 12 个列声明为 `JSON`（见 §2 oracle）。
2. 根 `Cargo.toml:24`：`sqlx = { version = "0.8", default-features = false, features = ["mysql", "runtime-tokio", "tls-rustls", "chrono", "uuid"] }` —— **未启用 `json` feature**（`Cargo.lock` 锁定 sqlx 0.8.6）。
3. sqlx-mysql 0.8.6 `src/types/str.rs:19-31`：`Type<MySql> for str`（`String`/`&str` 同实现）的 `compatible()` 白名单只有
   `VarChar | Blob | TinyBlob | MediumBlob | LongBlob | String | VarString | Enum` 且不含 `BINARY` flag，
   **不含 `ColumnType::Json`（wire 245）**。`Json` 只在 `impl_type_checking!` 里由 `#[cfg(feature = "json")] sqlx::types::JsonValue` 承接（`sqlx-mysql/src/type_checking.rs:60`）。
4. 于是 `row.try_get::<String, _>("json_col")` / `try_get::<Option<String>, _>(..)` 在真实引擎上被 sqlx 以类型不匹配拒绝 → 各读取点的 `map_err` 固定去敏 `Config` → HTTP 503 `STORAGE_UNAVAILABLE`；**表内无该表非空行时不触发**（这解释了 RC1 的"设备出现前空页 200、出现后固定 503"时间线）。
5. 写入方向不同：`ParamChecking::Weak` + `Encode for str`，`INSERT/UPDATE` 绑 `String` 文本合法，**不是同一个 bug**，本 RC 一律不改。
6. 结论：无论是否启用 `json` feature，本项目现有 `String` 消费者都需要 `CAST(<col> AS CHAR) AS <旧别名>` 的文本投影（与 RC1 三条 descriptor 查询同口径）。

---

## 2. Oracle：`schema/v4.sql` JSON 列总表（唯一 oracle，12 列）

| # | 表 | 列 | v4.sql 行 | 可空 | 生产读路径（SELECT 投影） | 状态 |
|---|---|---|---|---|---|---|
| 1 | `devices` | `descriptor_json` | :51 | NULL | `APPROVAL_PAGE_SQL`、`DEVICE_ROW_FOR_UPDATE_SQL`、`DEVICES_BY_IDS_SQL_TEMPLATE` | 已 CAST（RC1，本 RC 未动） |
| 2 | `devices` | `first_seen` | :53 | NULL | 无 SELECT（仅 `REGISTER_NEW_IDENTITY_SQL` INSERT 绑定） | 无读路径 |
| 3 | `devices` | `last_seen` | :54 | NULL | 同上 | 无读路径 |
| 4 | `grants` | `permissions` | :62 | NULL | `LIST_ACTOR_GRANTS_SQL`、`LIST_GRANTS_SQL`、`LIST_GRANTS_FOR_USER_SQL`、`FIND_GRANT_SQL`、integrity grant 字段扫描；integrity 预算谓词 | 已 CAST（其中 grant 字段扫描别名 `permissions_json`，谓词只 `OCTET_LENGTH(CAST(...))` 不取列） |
| 5 | `admission_decisions` | `time_evidence` | :71 | NOT NULL | 无 SELECT（仅 `db.rs` INSERT） | 无读路径 |
| 6 | `audit_events` | `params_redacted` | :77 | NOT NULL | `AUDIT_PAGE_SQL` → `AuditEventRow.params_redacted: String` | **本 RC 修复** |
| 7 | `audit_events` | `time_evidence` | :77 | NOT NULL | `AUDIT_PAGE_SQL`（别名 `created_evidence`）→ `Option<String>` → `serde_json::Value` | **本 RC 修复** |
| 8 | `task_previews` | `targets_json` | :90 | NOT NULL | `tasks/service.rs:1003` 预览锁定读 | 已 CAST |
| 9 | `main_tasks` | `counts_json` | :111 | NOT NULL | `MAIN_TASK_COLUMNS`（`tasks/repository.rs:271`）、integrity 全表 counts 扫描 | 前者已 CAST；**本 RC 修复 integrity** |
| 10 | `main_tasks` | `created_time_json` | :112 | NOT NULL | `MAIN_TASK_COLUMNS`、`tasks/service.rs:1329` 时间复核读 | 已 CAST |
| 11 | `sub_tasks` | `evidence_json` | :135 | NULL | `SUB_TASK_COLUMNS`（`tasks/repository.rs:279`，`Option<String>` 消费者） | 已 CAST |
| 12 | `recovery_checks` | `detail_redacted` | :157 | NOT NULL | 无 SELECT（仅 `INSERT_RECOVERY_CHECK_SQL`） | 无读路径 |

统计：12 列中 **8 列有读路径**（1/4/6/7/8/9/10/11），**4 列纯写**（2/3/5/12）。修复前裸读点恰 2 条 SQL（3 个列投影）。

## 3. 逐 consumer 结论（读路径全覆盖）

| 读 SQL（常量） | 位置 | JSON 列投影 | 结论 |
|---|---|---|---|
| `AUDIT_PAGE_SQL` | [devices/repository.rs:32](../crates/rsetup-controller/src/devices/repository.rs) | `CAST(params_redacted AS CHAR) AS params_redacted` + `CAST(time_evidence AS CHAR) AS created_evidence` | **本 RC 修复**（11 绑定、keyset `WHERE`、`ORDER BY process_epoch, event_seq, id`、`LIMIT`、别名逐字不变） |
| `MAIN_TASKS_COUNTS_JSON_SQL` | [integrity.rs:183](../crates/rsetup-controller/src/integrity.rs) | `CAST(counts_json AS CHAR) AS counts_json` | **本 RC 修复**（由 `check_runtime_data` 内 fn-local const 提升为模块级私有常量，仅为了让形状回归能引用；全表只读、零绑定、`try_get(1)` 索引序号不变） |
| `APPROVAL_PAGE_SQL` / `DEVICE_ROW_FOR_UPDATE_SQL` | devices/repository.rs:44 / :52 | `CAST(descriptor_json AS CHAR)` | 已 CAST（RC1），本 RC 未改其查询与 384 行 fixture |
| `DEVICES_BY_IDS_SQL_TEMPLATE` | authz/sqlx_repo.rs:832 | `CAST(descriptor_json AS CHAR)` | 已 CAST（RC1） |
| `LIST_ACTOR_GRANTS_SQL` / `LIST_GRANTS_SQL` / `LIST_GRANTS_FOR_USER_SQL` / `FIND_GRANT_SQL` | authz/sqlx_repo.rs:803 / 844 / 845 / 852 | `CAST(g.permissions AS CHAR) AS permissions` | 已 CAST |
| integrity grant 字段扫描 | integrity.rs:394 | `CAST(permissions AS CHAR) AS permissions_json` | 已 CAST；`validate_grant_fields` 的对象/数组/null/坏 JSON 严校验保留（既有 `grant_json_over_safe_bound_*`、`fixture_scan_*` 用例仍绿） |
| integrity 权限预算谓词 | integrity.rs:384 | `OCTET_LENGTH(CAST(permissions AS CHAR)) > 4096` | 只作谓词不取列，已正确，未改 |
| 预览锁定读 | tasks/service.rs:1003 | `CAST(targets_json AS CHAR) AS targets_json` | 已 CAST |
| `MAIN_TASK_COLUMNS` | tasks/repository.rs:271 | `CAST(counts_json …)`、`CAST(created_time_json …)` | 已 CAST |
| 时间复核读 | tasks/service.rs:1329 | `CAST(created_time_json AS CHAR)` | 已 CAST |
| `SUB_TASK_COLUMNS` | tasks/repository.rs:279 | `CAST(evidence_json AS CHAR) AS evidence_json`（NULL 走 `Option<String>`） | 已 CAST |

写路径（**确认不是本 bug，未改**，共 12 条）：`transport/admission.rs:92`（devices `descriptor_json`/`first_seen`/`last_seen`）、`audit.rs:40`、`auth/sqlx_repo.rs:206`、`db.rs:1674`、`tasks/service.rs:1601`（audit 两列）、`db.rs:1667`（`admission_decisions.time_evidence`）、`authz/sqlx_repo.rs:2722`（`grants.permissions`）、`tasks/repository.rs:627`（`targets_json`）、`tasks/service.rs:773/844/1549`（`counts_json`/`created_time_json`）、`tasks/recovery.rs:202`（`detail_redacted`）。

其它排除项：controller/src 内 **无 `SELECT *`**、**无 `JSON_EXTRACT`/`JSON_UNQUOTE`/任何 `JSON_*` 函数**（因此无"已返回 text 需保持"或"suspect wire type 需固定 code"的项）；位置取列只有一处 `try_get(1)`（即本次 integrity 修复点）；`db.rs` 的 12 个列名常量属于 schema 期望列名清单，非 SQL 投影；`api/adapters.rs`、`events.rs`、`authz/sqlx_repo.rs` 中的 `permissions`/`time_evidence` 字面量是 wire 字段名与错误码文案。

## 4. 本次改动（生产 diff 极小）

**改动文件仅 2 个**：`crates/rsetup-controller/src/devices/repository.rs`、`crates/rsetup-controller/src/integrity.rs`。

生产侧：

1. `AUDIT_PAGE_SQL` 两列加 `CAST(... AS CHAR)`，并补常量文档说明口径（1 行常量 + 7 行注释）。
2. `decode_created_evidence(raw: Option<String>) -> Result<Value, ControllerError>`（[devices/repository.rs:84](../crates/rsetup-controller/src/devices/repository.rs)）：把 `audit_page` 解码闭包里的 `match … serde_json::from_str` 原样提取为纯函数，SQL `NULL`、空文本、坏 JSON 仍走**同一条**去敏码 `identity admin read audit.decode`；提取只为让这条 fail-closed 边界可离线断言，判定逐字不变。
3. `MAIN_TASKS_COUNTS_JSON_SQL` 提升为模块级私有常量并加 `CAST`（含 `counts_json` 别名，索引 1 不变）。
4. `validate_counts_json_text(raw: &str) -> Result<(), ControllerError>`（[integrity.rs:279](../crates/rsetup-controller/src/integrity.rs)）：counts 扫描的应用层门禁原样提取；仍**只接受 JSON 对象**，数组 / `null` / 标量 / 空文本 / 坏 JSON 一律同一条 `runtime data main_tasks invalid counts_json`。

未改（明确声明）：绑定参数数量与顺序、`WHERE` 游标 keyset、`ORDER BY`、`LIMIT`、`FOR UPDATE`/CAS/guard 前置、列别名与索引序号、nullable 语义（`created_evidence` 仍按 `Option<String>` 取列、`evidence_json` 仍 `Option`）、错误码与 503 语义、任何 `INSERT/UPDATE` 写语句、sqlx feature、`schema/v4.sql`、`Cargo.toml`/`Cargo.lock`；`main.rs`（RC2 writer406 在途）、transport/session/board/schema、全局配置、正常业务状态机均未触碰。

新增回归 6 条（复用 RC1 的 `authz::sqlx_repo::sql_shape` fixture，未引入第二个 SQL 解析器）：

| 测试 | 位置 | 钉住内容 |
|---|---|---|
| `audit_page_sql_projects_both_json_columns_as_char_text` | devices/repository.rs:1007 | 11 项 select 列表逐项（含两列 CAST 形态与 `created_evidence` 别名）、`FROM` 尾部逐字、11 绑定 |
| `audit_json_reads_consume_pinned_const_without_inline_duplicates` | devices/repository.rs:1051 | `audit_page` 必须 `sqlx::query(AUDIT_PAGE_SQL)`；生产段内不得有第二条读 `params_redacted`/`time_evidence` 的内联 SELECT |
| `audit_created_evidence_decode_rejects_null_empty_and_malformed_texts` | devices/repository.rs:1077 | 合法对象文本原样解析；`None`/空/坏 JSON → 同一条去敏 503（不得退化成 `Value::Null` 或空页） |
| `main_tasks_counts_scan_projects_counts_json_as_char_text` | integrity.rs:1428 | select 恰为 `id` + `CAST(counts_json AS CHAR) AS counts_json`、尾部 `FROM main_tasks`（无 WHERE/排序/LIMIT/行锁）、零绑定 |
| `counts_json_scan_consumes_pinned_const_without_inline_duplicates` | integrity.rs:1454 | `check_runtime_data` 必须消费被钉常量；生产段内不得有第二条 `counts_json` 读 SQL |
| `counts_json_gate_accepts_object_and_rejects_array_null_scalar_or_malformed` | integrity.rs:1478 | 生产唯一生产者 `tasks::service::empty_counts_json()` 的真实形状通过；`[]`/`[1]`/`null`/`1`/`"queued"`/空/`broken`/`{` 全部拒绝且码不变 |

## 5. 真实 RED → GREEN 证据

**RED（修复前，同一测试二进制，两条形状断言真实失败）**

```
thread 'integrity::tests::main_tasks_counts_scan_projects_counts_json_as_char_text' panicked at crates/rsetup-controller/src/integrity.rs:1426:9:
assertion `left == right` failed: counts 扫描的 select 列表必须恰是主键 + CHAR 文本投影（索引 1 即 counts_json）：["id", "counts_json"]
  left: ["id", "counts_json"]
 right: ["id", "CAST(counts_json AS CHAR) AS counts_json"]

thread 'devices::repository::tests::audit_page_sql_projects_both_json_columns_as_char_text' panicked at crates/rsetup-controller/src/devices/repository.rs:1006:9:
assertion `left == right` failed: 审计页 select 列表必须逐项一致（列顺序与两列 JSON 的 CHAR 文本投影口径）：[…, "params_redacted", "outcome", "process_epoch", "event_seq", "time_evidence AS created_evidence"]
 right: […, "CAST(params_redacted AS CHAR) AS params_redacted", "outcome", "process_epoch", "event_seq", "CAST(time_evidence AS CHAR) AS created_evidence"]

test result: FAILED. 0 passed; 2 failed; 0 ignored; 925 filtered out
```

**RED（解码点尚不存在的第二个 RED，逐字）**

```
error[E0425]: cannot find function `decode_created_evidence` in this scope  --> devices/repository.rs:1077:18 / :1085:25
error[E0425]: cannot find function `validate_counts_json_text` in this scope --> integrity.rs:1475/1476/1477/1479
```

（该 RED 是"纯解码点尚未提取"的编译期失败，不是断言失败；提取本身是行为不变的 refactor，其语义由既有同形码 + 新用例共同钉住。）

**GREEN（应用 CAST + 提取后，格式化后复跑）**

```
running 6 tests
test devices::repository::tests::audit_created_evidence_decode_rejects_null_empty_and_malformed_texts ... ok
test integrity::tests::counts_json_gate_accepts_object_and_rejects_array_null_scalar_or_malformed ... ok
test devices::repository::tests::audit_page_sql_projects_both_json_columns_as_char_text ... ok
test integrity::tests::main_tasks_counts_scan_projects_counts_json_as_char_text ... ok
test integrity::tests::counts_json_scan_consumes_pinned_const_without_inline_duplicates ... ok
test devices::repository::tests::audit_json_reads_consume_pinned_const_without_inline_duplicates ... ok
test result: ok. 6 passed; 0 failed
```

## 6. 验证矩阵（全部 `--locked --offline`，`CARGO_TARGET_DIR=target/tunnel-json-read-sweep`）

| 命令 | 结果 |
|---|---|
| `cargo test -p rsetup-controller --lib -- <focused: devices:: integrity:: audit:: api::audit>` | 78 passed / 0 failed |
| `cargo test -p rsetup-controller --lib`（全量，一次） | **929 passed / 0 failed / 0 ignored**（31.4s → 30.5s 复跑） |
| `cargo clippy --profile test -p rsetup-controller --lib -- -D warnings` | 干净（`Finished` 且零 warning；未用 `--all-targets`，避开在途 `main.rs`） |
| `cargo fmt --package rsetup-controller -- --check` | 首轮报出 4 处差异，**全部落在本 RC 新增区**（`devices/repository.rs:1075/1084`、`integrity.rs:277/1438`），`src/main.rs` 0 处；已用 `rustfmt --edition 2024` 只针对这两个文件修正后复检 **FMT CHECK CLEAN**（未执行 `cargo fmt --all`） |
| 修复前后全量 lib 用例数 | 923 → **929**（+本 RC 新增 6 条，无既有用例被删改） |

未运行：任何 `--test` 集成/真库 executor、`tests/` 目录、迁移、DB 连接。理由：本 RC 是纯投影口径收口，离线合同 + 形状回归即可判定；真库复验留给既有 executor（重启后复用现有 admin 显式密码 + 全新空 state，不新建库）。

## 7. Oracle 扫描方法（可复跑口径，未新增解析器）

一次性离线脚本（不落盘、不写 `/tmp`）：按大括号配对剥离每个 `.rs` 的 `#[cfg(test)]` 模块 → 还原 Rust 字符串 `\`+换行续行 → 抽取全部双引号字面量 → 命中 12 个 oracle 列名者分类为 `CAST / 写路径 / 非 SQL 投影（标识符·错误码·wire key）`。

结果（逐条穷举见 §3 表格）：`controller/src` 生产段内提及 oracle JSON 列的 **读 SQL 语句共 15 条**（14 条 JSON 列投影 + 1 条 `OCTET_LENGTH` 谓词），修复后 **100% 为 `CAST(... AS CHAR)` 文本投影，裸读 0 条**（修复前恰 2 条语句、3 个列投影裸读）；**写 SQL 语句共 12 条**（`transport/admission.rs:92`、`audit.rs:40`、`auth/sqlx_repo.rs:206`、`db.rs:1667/1674`、`authz/sqlx_repo.rs:2722`、`tasks/repository.rs:627`、`tasks/service.rs:773/844/1549/1601`、`tasks/recovery.rs:202`）按 `String` 绑定文本，**确认不属于本 bug 且一律未改**；另有约 20 处命中是标识符/错误码/wire 字段名（`db.rs` 的 schema 期望列名清单、`grants.permissions` 错误码、`permissions.changed` 事件名、`api/adapters.rs` 的 wire key）。`role_permissions.permission` 是 `VARCHAR`，同名不同列，已在扫描中显式排除。

## 8. 未确认 / 遗留项（如实记录）

1. **真库未跑**：本 RC 只有源码合同 + 离线回归。`CAST(col AS CHAR)` 在 MySQL/TiDB 预编译线路上的返回类型为 `VAR_STRING` 非 `BINARY`，与 RC1 三条 descriptor 查询同形（RC1 已有真库结论），本 RC 不重复声称为真库证据。
2. **`audit_events.time_evidence` 的字段级质量未校验**：读路径只做 `serde_json::from_str`（任意合法 JSON 值都接受，JSON `null` 文本会解析成 `Value::Null`）。这是**既有行为**，本 RC 按"严校验保留、不加严不放宽"处理，未改动；若需要 TimeEvidence schema 级严校验，属独立契约变更，建议另开 RC。
3. **`params_redacted` 下游无二次解析**：`AuditEventRow.params_redacted: String` → wire JSON string 逐字透传，出口形状由 `api::audit::tests`（`items[0]["params_redacted"] == "{}"`）既有断言钉住，本 RC 不重复覆盖。
4. **无 `JSON_*` 函数用例**：controller/src 生产段无 `JSON_EXTRACT`/`JSON_UNQUOTE`/`JSON_LENGTH` 等，故无"suspect wire type 需固定 code"的待确认项。
5. `src/main.rs` 由 RC2（writer406）在途修改，本 RC **未修改该文件**：扫描确认 `main.rs` 不含任何 JSON 列 SQL 读；其编译与格式状态归 RC2 收口（本 RC 全程只用 `--lib`，不受其中改影响）。

## 9. 范围合规

* 未派生子代理；未 `commit`/`push`/`clean`；未改 `Cargo.toml`/`Cargo.lock`/`schema/`/`main.rs`/transport/session/board；未改任何 SQL 写语句、状态机、secret、全局配置；未重启或操作任何服务；未写 `/tmp`；未连真实 DB、未读 `secret/`。
* 私有现况保持不动：TiDB `initialized=true`、admin 1、device 1、audit 4（未做任何读写）。后续重启复验沿用现有 admin 显式密码与全新空 state，不新建库。

## 10. 耗时

| 阶段 | 耗时 |
|---|---|
| 首次编译 + RED 运行 | 20.7s（test profile 编译）+ <0.1s 断言失败 |
| GREEN focused | 编译复用 + 0.02s（78 用例） |
| 全量 `--lib` | 31.4s（929 用例） |
| clippy（profile test, lib） | 12.0s 首跑 / 2.5s 复跑，零告警 |
| 本 RC diff 体量 | 生产侧：2 条 SQL 常量加 `CAST` + 1 处 const 提升 + 2 个纯函数提取，净生产行 +14；测试侧：6 个用例约 180 行（含文档注释），**零新增 fixture/解析器**（复用 RC1 的 `sql_shape`）。对照 RC1：RC1 引入 `sql_shape` fixture 本身（约 90 行）+ 3 条 descriptor 读 SQL 与其回归；本 RC 未改 RC1 那 3 条查询与其 fixture（其用例在全量 929 中继续通过） |
| Oracle 扫描工具 | 一次性离线脚本（未落盘、不进仓库），非可复用解析器 |

## 11. RC1 与 sweep 的独立复审（825dfa5e，2026-10-24）

**五条生产投影 SQL 全部 ADDRESS，无 Medium 及以上 finding，可进入后续真实验证。**
reviewer 独立核生产定义、消费、绑定、NULL／严格解码与排序／锁序，逐个重新核查 v4
全部 JSON 列和读写调用，不仅依赖 §7 扫描器；未发现同类裸 JSON→String 生产读取遗漏。

独立当前源码结果：lib **929 passed / 0 failed**、focused **109 passed**、lib test-profile
clippy `-D warnings` 与三个文件 rustfmt 均通过。reviewer 未连库、未读私有凭据；HEAD
裸列状态被直接读回核实，但未声称自己复现 writer 的历史 RED 或进行真实端到端重跑。

主代理仅补清了两处表述：String 的 JSON 类型不兼容与 `json` feature 无关，及 counts
回归实际经 `empty_counts_json()` 委托生产 `counts_json_of(&[])`。既有审计 time_evidence
字段级校验边界不变。工作树相对老 HEAD 的 numstat 含多轮累计改动，不全归本次 sweep。
后续实际启动必须用新空私有 state 与现有管理员显式凭据，保留现况库和旧失败证据；
不能复用已有非空 state 或将源码合同绿冒称真实 SQL／双进程已通过。

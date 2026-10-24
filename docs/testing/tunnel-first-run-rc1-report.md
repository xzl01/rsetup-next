# RC1 首次真实 controller+board 执行 · 设备 JSON 解码根因修复报告

- 范围：`crates/rsetup-controller/src/devices/repository.rs`、`crates/rsetup-controller/src/authz/sqlx_repo.rs`
  三条设备读 SQL 的 `descriptor_json` 投影，及配套内部回归测试。
- 不在本报告主张范围内的事情：RC2 supervisor（他人并行修改 `src/main.rs`）、真库端到端复跑
  （由另一可信 executor 负责）、任何数据库变更或数据清理。
- 本任务未执行：真实 DB/网络访问、服务启停、迁移、数据重置、主机改动、凭据读取、
  `cargo fmt --all`、子代理、commit/push/clean、工作区外或 `/tmp` 写入。

## 1. 现场与根因

### 1.1 确证部分（来自 b52 可信只读诊断与真库列事实）

- HTTP 时间线（确证）：设备身份出现之前 `GET /approvals` 返回 200 空页；设备出现之后同一
  路由连续 86 次 503 `STORAGE_UNAVAILABLE`。两条事实都来自 b52 私有只读诊断记录，本任务
  未重放、未复连该环境。
- 存储侧事实（确证）：`devices.descriptor_json` 列类型为 v4 `JSON`，实际列 wire 类型 245，
  非 NULL，内容为合法 JSON。
- 代码侧事实（本工作区实读）：三条生产查询把该列按 `Option<String>` 取列后喂给
  `decode_device_admin_view`（第三参 `Option<String>`，
  `src/authz/sqlx_repo.rs:669-703`）。workspace 的 sqlx 依赖未启用 `json` feature
  （根 `Cargo.toml:24` 的 `sqlx` features 只有 `mysql/runtime-tokio/tls-rustls/chrono/uuid`）。
- 错误形状（确证）：`ControllerError::Database(_) | ControllerError::Config(_) => "STORAGE_UNAVAILABLE"`
  （`src/api/adapters.rs:190`），与现场 503 码一致。

### 1.2 推断部分（如实标注，非直接记录）

- b52 诊断未直接记录被 `?` 抛出的 sqlx `Error` 变体与其 `Display`/source 链。本报告的
  "取列类型不匹配"结论是从 列 JSON wire 245 + 解码目标 `Option<String>` + 未启用 sqlx `json`
  feature + 503 映射 四者推出的最简一致解释：sqlx 的 `String`／`str` 类型兼容白名单不含
  JSON wire 类型，**与是否启用 `json` feature 无关**；启用该 feature 只增加 JSON 值类型映射，
  不会让此处 `Option<String>` 自动兼容 JSON。错误经 `?` 变成 `ControllerError::Database` 再固定去敏为 503。
  该推断未经真库复跑验证，端到端确认归后续可信 executor。
- 同一条推断也解释"设备出现前 200 空页"：`try_get::<Option<String>, _>` 对 SQL NULL 先行返回
  `None`，不进入文本解码，因此没有非 NULL descriptor 行时该缺陷不可见。

### 1.3 为什么既有 JSON schema 门禁没有拦住

原 schema 门禁只校验列的持久形状（`information_schema` 的 data_type/列类型必须为 `JSON`、
NULL 约束等），即"列必须是 JSON"。它不执行任何应用读路径的解码，所以"门禁要求 JSON 列"与
"读路径按文本解码 JSON 列"这两个互相矛盾的前提可以同时通过门禁。这是调用路径的解码类型错误，
不是 schema 缺陷：**不得**通过改列类型、改 DEFAULT/NULL 语义或放开校验来"消音"。

## 2. 修复（最小面：三处 SELECT 投影）

三条查询的 `descriptor_json` 一律改为显式 CHAR 文本投影 `CAST(descriptor_json AS CHAR) AS descriptor_json`，
与本仓其它 JSON 读（`grants.permissions`、`tasks` 各 JSON 列）以及已按真库修过的
`tests/transport_admission.rs:772` 同口径。别名、列顺序、`WHERE`/`ORDER BY`/`LIMIT`、
行锁 `FOR UPDATE`、绑定参数个数与顺序、guard/actor 前置、revision CAS 全部不变。

| # | 常量（修复后位置） | 生产消费点 | 修复后 SQL 关键片段 |
|---|---|---|---|
| 1 | `APPROVAL_PAGE_SQL`，`src/devices/repository.rs:37` | `approval_page` → `src/devices/repository.rs:379`，解码 `:396` | `CAST(descriptor_json AS CHAR) AS descriptor_json`；5 个绑定、`ORDER BY public_key LIMIT ?` 不变 |
| 2 | `DEVICE_ROW_FOR_UPDATE_SQL`，`src/devices/repository.rs:45`（本次从 `patch_device_display_name` 内联字面量提为常量） | `src/devices/repository.rs:674`，解码 `:683` | 同上；`FROM devices WHERE public_key = ? FOR UPDATE`、1 个绑定不变 |
| 3 | `DEVICES_BY_IDS_SQL_TEMPLATE`，`src/authz/sqlx_repo.rs:832` | `load_device_snapshot_in_tx` → `src/authz/sqlx_repo.rs:1169`，解码 `:1185` | 同上；`IN ({placeholders}) ORDER BY public_key`、模板内 0 个 `?` 不变 |

未改变的行为（回归已钉住）：

- SQL NULL 仍解码为 `descriptor = None`；
- 非法 JSON / 非法 descriptor 仍失败关闭为固定去敏 `Config`（`identity admin read devices.descriptor`），
  不返回空页、不降级、不跳过 descriptor 校验；
- `String`/`Option<String>` + `serde_json` 的严格解码 helper 本体未改；
- 第 2 处提取常量只为共享离线回归，事务骨架（`lock_integrity_guard` → `require_active_admin`
  → devices 行 `FOR UPDATE` → CAS → 审计同事务 → commit）逐字保留；
- 未新增 schema、未改 `Cargo.toml`、未加 sqlx `json` feature。

## 3. TDD 证据（真实 RED / GREEN）

### 3.1 新增测试

`src/authz/sqlx_repo.rs`

- `sql_shape` 测试专用 fixture（`:2995` 起）：`normalize_sql` / `split_select` /
  `production_source` / `select_string_literals` /
  `assert_device_descriptor_text_projection`。只做 SQL 文本形状解析，
  **不连引擎、不伪造 `MySqlRow`**；断言把整条 statement 纳入（select 列表逐项 + `FROM`
  之后尾部逐字一致 + 绑定数不漂移 + `descriptor_json` 必须恰为唯一一项 CHAR 文本投影），
  避免"只检查某字符串存在而整条语句未被检查"。
- `devices_by_ids_sql_template_projects_descriptor_json_as_char_text`（`:3282`）：查询 3 形状
  回归，另钉 `{placeholders}` 展开后绑定 1:1。
- `device_snapshot_read_consumes_pinned_template_without_inline_copy`（`:3308`）：执行位必须
  消费被钉常量（`include_str!` 自检生产段），且生产段内不得出现未被钉的 `descriptor_json`
  读 SQL 字面量。
- `descriptor_json_decoder_takes_char_text_and_fails_closed_on_malformed`（`:3335`）：以显式
  `fn(Vec<u8>, String, Option<String>, &str, &str, u64, i64)` 指针钉住解码器契约（SQL 侧必须
  交付 CHAR 文本）；补齐非 NULL 合法 JSON、空白/Unicode/嵌套、空对象、数组、合法标量、
  JSON `null` 文本 vs SQL NULL 的边界，以及空串/截断/非 JSON 一律固定去敏 `Config`。
  原有 `snapshot_device_decode_fails_closed_on_polluted_state_and_boolean`
  （NULL / 脏状态 / 脏布尔 / 脏 descriptor / 长度错误）全部保留。

`src/devices/repository.rs`

- `approval_page_sql_projects_descriptor_json_as_char_text`（`:916`）：查询 1 形状回归。
- `device_row_for_update_sql_keeps_lock_and_projects_descriptor_as_char_text`（`:938`）：查询 2
  形状回归 + `FOR UPDATE` 只能出现一次。
- `device_descriptor_reads_consume_pinned_consts_without_inline_duplicates`（`:965`）：两个执行位
  消费常量 + 生产段无内联副本。

三条回归全部消费生产常量本身（不是测试里复制的 query 文本），因此常量漂移即测试失败。

### 3.2 RED（修复前，实际输出摘录）

```
$ CARGO_TARGET_DIR=target/tunnel-real-rc1 \
  cargo test --locked --offline -p rsetup-controller --lib descriptor

test devices::repository::tests::approval_page_sql_projects_descriptor_json_as_char_text ... FAILED
test authz::sqlx_repo::tests::devices_by_ids_sql_template_projects_descriptor_json_as_char_text ... FAILED
test devices::repository::tests::device_row_for_update_sql_keeps_lock_and_projects_descriptor_as_char_text ... FAILED
test authz::sqlx_repo::tests::descriptor_json_decoder_takes_char_text_and_fails_closed_on_malformed ... ok
test devices::repository::tests::device_descriptor_reads_consume_pinned_consts_without_inline_duplicates ... ok

 left:  [..., "descriptor_json", ...]
 right: [..., "CAST(descriptor_json AS CHAR) AS descriptor_json", ...]

test result: FAILED. 2 passed; 3 failed; 0 ignored; 918 filtered out
```

RED 步骤先只做"把 `FOR UPDATE` 字面量提为常量 + 新增回归"，因此三条失败逐一对应三条生产 SQL。

### 3.3 GREEN 与全套验证（修复后，实际输出摘录）

```
$ export CARGO_TARGET_DIR=target/tunnel-real-rc1
$ cargo test --locked --offline -p rsetup-controller --lib descriptor
test result: ok. 5 passed; 0 failed; 918 filtered out
$ cargo test --locked --offline -p rsetup-controller --lib devices::
test result: ok. 38 passed; 0 failed
$ cargo test --locked --offline -p rsetup-controller --lib authz::
test result: ok. 46 passed; 0 failed
$ cargo test --locked --offline -p rsetup-controller --lib
test result: ok. 923 passed; 0 failed; finished in 30.61s
$ rustfmt --edition 2024 --check \
    crates/rsetup-controller/src/authz/sqlx_repo.rs \
    crates/rsetup-controller/src/devices/repository.rs ; echo $?
0
$ cargo clippy --locked --offline -p rsetup-controller --profile test --lib -- -D warnings
    Finished `test` profile [unoptimized + debuginfo] target(s)
```

本任务改动只落在上述两个源文件（`git diff --numstat`：`sqlx_repo.rs +288/-1`、
`repository.rs +96/-9`；被删除的行只有 3 条 SQL 常量/内联字面量与其文档行）。
格式化只对这两个文件执行 `rustfmt --edition 2024`，未执行 `cargo fmt --all`。

### 3.4 未由本任务覆盖的验证（如实说明）

- 真库（MySQL/TiDB）端到端未由本任务执行，本报告的 GREEN 只到"离线形状 + 解码契约"层，
  不声称真库已过；引擎端复跑由另一可信 executor 完成。
- 集成目标 `production_composition`、`management_gateway_http` 在 `src/main.rs`（RC2 supervisor，
  他人并行修改）处于可编译状态时曾通过（`management_gateway_http` + `production_composition`，
  末条汇总 `14 passed; 0 failed`）。此后 `cargo test --test ...` 会连带编译 bin 目标
  `rsetup-controller`，因该并行改动处于中间态而报 `src/main.rs` 编译错误；该失败属邻居在途改动，
  本任务按约定只报告一次、未重试，也未触碰 `src/main.rs`。
- `cargo fmt --package rsetup-controller -- --check` 在同一时刻同样只会因 `src/main.rs` 报差异；
  本任务两个文件单独 `rustfmt --check` 为 0（见上）。

## 4. 同类漏点排查（需新 scope，本任务未修）

按要求对全 `src` 做过 JSON 列 SELECT 的逐条比对（`descriptor_json`/`first_seen`/`last_seen`/
`permissions`/`time_evidence`/`params_redacted`/`targets_json`/`counts_json`/`created_time_json`/
`evidence_json`/`detail_redacted`）。`descriptor_json` 的生产读只有第 2 节那 3 条，已全部修复；
另有两条**同根因但不同语句**的读路径，属新 scope：

1. `src/devices/repository.rs:25` `AUDIT_PAGE_SQL`：`params_redacted`（`JSON NOT NULL`）与
   `time_evidence AS created_evidence`（`JSON NOT NULL`）按 `String` / `Option<String>` 取列，
   解码点在 `:531` 与 `:551-554`。审计表一旦出现任何行，审计分页读路径会以同样方式 503；
   真库现场已有 4 条 audit 行，因此该入口的暴露概率高于 descriptor 之外的其它候选。
2. `src/integrity.rs:255` `COUNTS_JSON`（`SELECT id, counts_json FROM main_tasks`）：
   `counts_json`（`JSON NOT NULL`）按 `String` 取列（`:261`）。`main_tasks` 非空时运行时完整性
   扫描会走同一条失败路径。

两者均未在本次改动中触碰（越界修复会扩大同一提交的影响面）。修它们需要在同一 `CAST(... AS CHAR)`
口径下补各自的形状回归，建议作为独立后续任务。

## 5. 结论边界（不要过度解读本次修复）

- 本次只修 RC1 现场暴露的 3 条设备读 SQL 的解码投影，**不等于整次 first-run 已通过**。
- RC2 启动阶段的 30s 计时器误武装是另一条独立缺陷，由 RC2 supervisor 的并行修复负责，
  本报告不涉及其状态。
- 首轮 Pending 阶段的 board open failure 具体分类（对端拒绝 / 连接建立 / 帧层 / 超时）**尚未明确**，
  需要下一次真实 executor 的分类证据；不能由本 JSON 修复的成功或失败代替判断。
- 真库现场保持原样：`init=true`、admin 1、devices 1、audit 4。**不建议**清理、重建或换库；
  后续验证应在该现场继续，以便对照同一条时间线。
- 后续 executor 复跑关注点（不在本任务范围）：`GET /approvals` 在非 NULL descriptor 行存在时
  返回 200 且 `descriptor` 字段逐字等于库内 JSON；随后 approve/rename（查询 2）与设备详情/授权读
  （查询 3）不再出现 503；如仍出现 503，需要抓取 sqlx 错误的 variant 与 source 链以确认是否为
  其它原因（本报告 1.2 的推断届时可被证实或推翻）。

## 6. 复跑命令（离线、专用 target）

```bash
export CARGO_TARGET_DIR=target/tunnel-real-rc1
cargo test --locked --offline -p rsetup-controller --lib descriptor
cargo test --locked --offline -p rsetup-controller --lib devices::
cargo test --locked --offline -p rsetup-controller --lib authz::
cargo test --locked --offline -p rsetup-controller --lib
cargo clippy --locked --offline -p rsetup-controller --profile test --lib -- -D warnings
```

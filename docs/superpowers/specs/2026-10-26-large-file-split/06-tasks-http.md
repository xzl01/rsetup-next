# 子规格 06：`crates/rsetup-controller/src/tasks/http.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 06 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未读 `secret/`、未连接数据库、未提交。

撰写基线：分支 `dev-aghost`，头提交 `c7ff640`，撰写时 `git status` 实测 [tasks/http.rs](../../../../crates/rsetup-controller/src/tasks/http.rs) 与 [tasks/mod.rs](../../../../crates/rsetup-controller/src/tasks/mod.rs) 无未提交改动。全部行号为撰写会话逐段实测；实施时按当时工作树重新核对（总览 §3 并行改动注记）。任务域串行约束：本规格属总览 §5.2 批次 D 的第二项，**12 号 `12-tasks-service.md` 的 service/状态机边界定稿在前**；本规格不设计 12 的内部，`tasks/adapter.rs`（TaskApi 生产实现）不在本文件作用域。

## 1. 实测现状

### 1.1 体量、测试属性精确统计与消费面

- 全文件实测 3,472 行 = 实现区 1–1,318 + `#[cfg(test)] mod tests`（1,320–3,472，共 2,154 行），与总览 §3 快照「1,318 / 2,154」一致。测试属性精确统计：**15 个 `#[test]` + 39 个 `#[tokio::test]` = 54 个测试函数**，单一测试模块、无第二测试块（初盘若按「多个测试块」口径猜测即与此不符，以本实测为准）。
- 模块声明：[tasks/mod.rs](../../../../crates/rsetup-controller/src/tasks/mod.rs) 第 6 行 `pub mod http;`、第 37 行 `pub use http::build_tasks_router;`。拆分前后逐字不变（C1）。
- 模块外消费面（实测，拆分后全部路径必须原样可用）：
  - 生产装配：[production.rs](../../../../crates/rsetup-controller/src/production.rs)（38 import `HttpTasksState`/`build_tasks_router`、58 字段、123 组 router）；controller [main.rs](../../../../crates/rsetup-controller/src/main.rs)（42、287 **结构体字面量直接构造** `HttpTasksState { api, auth, events, recovery }`，四字段名与可见性不得变）。
  - 跨线复用：[http_events.rs](../../../../crates/rsetup-controller/src/http_events.rs)（92）与 [http_device_status.rs](../../../../crates/rsetup-controller/src/http_device_status.rs)（68）均 import `crate::tasks::http::require_business_session`（现 `pub(crate)`，其余模块的同名符号均系本文件私有定义，实测无第三处消费者）。
  - 服务适配层：[tasks/adapter.rs](../../../../crates/rsetup-controller/src/tasks/adapter.rs)（51–58 import 22 个名字：`CancelTaskRequest`、`CancelTaskResponse`、`CancelledTarget`、`ChildEntry`、`ChildrenPage`、`ReleaseLockRequest`、`ReleaseLockResponse`、`TaskActions`、`TaskApi`、`TaskCounts`、`TaskCursorScope`、`TaskDetail`、`TaskFuture`、`TaskOperation`、`TaskPage`、`TimeReviewTaskRequest`、`ViewScope`、`cancel_permitted`、`decode_task_cursor`、`encode_task_cursor`、`parse_decimal_u64`、`project_cancel_result`；另有 783/786 全路径引用 `TimeReviewDecision`）。
  - 集成测试（总览 C6，仅作验证依据）：`tests/time_consumption.rs`（37：`TaskApi`、`TimeReviewDecision`、`TimeReviewTaskRequest`）、`tests/sse_business_events.rs`（73–76 import DTO/`HttpTasksState`/`build_tasks_router`，606 组装）、`tests/production_composition.rs`（123、440）。
- 本文件不含任何 SQL（实测仅测试内 1,996 行 `sqlx::Error::RowNotFound` 构造）；SQL/事务副作用归 12 号域。

### 1.2 实现区职责锚点（实测行号）

- **模块文档与 import** 1–52；**分页常量** 53–56：`DEFAULT_PAGE_LIMIT`、`MAX_PAGE_LIMIT`（`pub`，仅本文件 `clamp_page_limit` 消费；`src/api/mod.rs` 的同名常量是另一模块私有物）。**私有常量** 57–64：`IDEMPOTENCY_KEY_HEADER`、`TASK_CURSOR_DOMAIN`、`TASK_CURSOR_BYTES`、`TASK_CURSOR_TAG_BYTES`。
- **契约类型与 trait** 66–294：`TaskOperation`(69，derive 在 67)、`ViewScope`(76)、`TaskCounts`(84)、`TaskActions`(100)、`TaskDetail`(106)、`ChildEntry`(125)、`ChildItem`(137)、`TaskPage`(149)、`ChildrenPage`(160)、请求/响应 DTO `CreatePreviewRequest`(167)…`CancelTaskResponse`(223)、`CancelledTarget`(234)、`TaskFuture`(241)、`TaskApi`(248，8 方法，对象安全)。
- **状态装配** 296–399：`HttpTasksState`(308，四 pub 字段 `api/auth/events/recovery`)、`new`/`with_events`/`with_recovery`(319/329/337)、私有 `blocks_mutation_task`(347)、私有 `publish_task_update`(364，创建者受众 + 非管理员补投管理员受众)、两个 `FromRef` impl(389/395)。
- **门禁与错误映射（均 `pub(crate)`）**：`require_business_session`(405，临时密码 403 `PASSWORD_CHANGE_REQUIRED`，不要求 admin)、`controlled_recovery_response`(422，503 `CONTROLLED_RECOVERY_ACTIVE`)、`task_error_response`(433，逐变体映射、**无通用 500 兜底**、其余 `_ => not_ready()` 503；`Database` → 503 `STORAGE_UNAVAILABLE`；`ResourceExhausted` → `exhausted()`)。
- **解析/编码** 482–533：`strict_json`(484 私有)、`preview_expires_in_ms`(490 私有)、`parse_decimal_u64`(496 pub)、`clamp_page_limit`(509 pub)、`parse_idempotency_key`(519 pub)、`parse_path_id`(528 私有)。
- **判定与投影** 535–668：`cancel_permitted`(537)、`release_lock_permitted`(551)、`time_review_permitted`(561)、`project_cancel_result`(568)、`project_main_task_summary`(588，F06 由 `visible_states` 重算)、`child_item_json`(638 私有)、`project_task_children`(652)——后六者 `pub`。
- **游标编解码** 670–779：`TaskCursorScope`(672 pub，私有 `scope_byte`)、`hmac_sha256`(688 私有)、`constant_time_eq`(710 私有)、`encode_task_cursor`(723 pub)、`decode_task_cursor`(743 pub，**先 MAC 后字段**校验序)。消费方是 `tasks/adapter.rs`，不经 handler。
- **router 装配** 782–798：`build_tasks_router`，8 条路由白名单。
- **`mod routes`（私有）** 800–1,318：`PageQuery`(805)、严格查询串解析 `parse_page_query`(813，≤512B、禁 `%`、禁重复/未知/空值)、`project_detail`(872)、8 个 handler（下列行号）、202 信封 `accepted_response`(1311)。

### 1.3 handler 门禁序列（逐源码核实，非惯例推断）

共用骨架（模块文档 8–14 行钉死、逐 handler 源码复核）：① `authenticate_write`（5 个写端点）或 `authenticate_read` + `EndpointId::{TaskList,TaskDetail,TaskChildren}`（3 个读端点）——Host→Origin→严格单值 Cookie→活会话预检→权威 authenticate→CSRF→限速全在共享 gate（[http_gate.rs](../../../../crates/rsetup-controller/src/http_gate.rs)、[http_auth.rs](../../../../crates/rsetup-controller/src/http_auth.rs)），本文件不自写；② `require_business_session`；③ 参数/body 解析（`read_json_body` ≤1 MiB + `deny_unknown_fields`）；④ 管理员前置**仅** time-review / release-lock；⑤ `TaskApi`；⑥ `task_error_response`。逐端点实测差异（全部为安全语义，禁止顺手重排）：

| 端点（handler 行号） | 实测特有顺序 |
| --- | --- |
| `create_preview`(887) | body 后逐条 `parse_device_id`，逐条失败即 `task_error_response`；响应由路由层补 `expires_in_ms` |
| `create_task`(941) | **恢复闸门 953–955 在 ② 之后、Idempotency-Key 与 body 之前**；仅本端点校验 `Idempotency-Key`（956，在 body 读取之前）；`confirm` 检查在 body 之后（965）；成功 202 |
| `list_tasks`(998) | `parse_page_query(allow_state=true)` → `clamp_page_limit` → `TaskApi::list_tasks` |
| `task_detail`(1038) | 路径 ID 先于业务调用（1050） |
| `task_children`(1064) | 路径 ID → `parse_page_query(false)`（禁 `state`）→ clamp |
| `cancel_task`(1104) | 路径 ID(1117) 先于 body；`expected_revision` 解析(1125) 先于 `confirm` 检查(1129)；事件仅在 `cancelled` 非空时发(1148–1159) |
| `time_review`(1167) | body → revision 校验(1188) → **`require_admin_session`(1191，在 body 之后)**；`InvalidArgument` 时只读回读 `task_detail` 区分 `task.no_held` / 决策矛盾 / 404 / 503(1221–1237)，复核自身失败不得吞成 400 |
| `release_lock`(1243) | body → `ack`(1264) → `reason` 非空(1272) → revision(1280) → **admin(1284，最后)**；事件仅 `released` 时发、受众仅 admins(1295–1301) |

事件发布（`publish_task_update` 364 与 handler 内联）只在成功分支返回响应前执行、失败零发布、`events=None` 零行为；revision 语义逐端点不同（create 恒 1；cancel 用 `expected_revision.saturating_add(1)` 下界；time_review 用回读精确值且只投 admins；release_lock 用子任务 id + `expected+1` 下界且只投 admins）。本文件无 SSE/stream（事件线在 `http_events.rs`）。

## 2. 目标私有模块树（拟新增文件用 ```` ```text ```` 标注；无文件更名）

采用「根入口 + 同名目录」：`http.rs` 保留为根入口（Rust 2018 允许 `tasks/http.rs` 与 `tasks/http/` 并存），**避免任何对外更名**；`tasks::http` 外部路径、`build_tasks_router` 定义位置均不动。

```text
crates/rsetup-controller/src/tasks/
├── http.rs                    （根入口，保留：模块文档、mod 声明、§3 精确 re-export 清单、build_tasks_router(782) 定义、#[cfg(test)] mod fixtures）
└── http/
    ├── contract.rs            （§1.2「契约类型与 trait」全部 20 项，66–294）
    ├── state.rs               （HttpTasksState 及 impl、FromRef×2，296–399）
    ├── gate.rs                （require_business_session / controlled_recovery_response / task_error_response，401–480）
    ├── query.rs               （分页常量 + parse_decimal_u64 / clamp_page_limit / parse_idempotency_key / parse_path_id / preview_expires_in_ms / IDEMPOTENCY_KEY_HEADER，53–58、482–533 之解析子集）
    ├── projection.rs          （strict_json + 三个 *_permitted + project_cancel_result / project_main_task_summary / child_item_json / project_task_children，482–486、535–668）
    ├── cursor.rs              （游标三常量 + TaskCursorScope + hmac_sha256 + constant_time_eq + encode/decode_task_cursor，59–64、670–779；`IDEMPOTENCY_KEY_HEADER`(58) 归 query，不含）
    ├── fixtures.rs            （#[cfg(test)] 共享 fixture：fixture_time_evidence / fixture_session / fixture_detail + RAW_TOKEN / IDEMPOTENCY_KEY / FIXTURE_USER_ID；B3 首建时仅含 fixture_session + RAW_TOKEN，见 §4/§5）
    └── routes.rs              （mod routes 原样：PageQuery / parse_page_query / project_detail / 8 handler / accepted_response，800–1,318）
```

依赖方向（单向、无新增环）：`contract` ← `query`/`projection`/`state`（state 区 296–399 实测**不调用**任何 gate 响应函数，gate 独立于 `contract`/`query`/`projection`/`state` 四新模块，仅共用 http_gate/http_auth 通用件与 `ControllerError`/`RecoveryMode`）；对 gate 三符号的真实依赖在 `routes`（800–1,318 实测调用 `require_business_session`/`controlled_recovery_response`/`task_error_response`），即 `contract`/`state`/`gate`/`query`/`projection` ← `routes` ← 根 `http.rs`（装配点）。此依赖图下 B2（contract+state）与 B3（gate）互不依赖、先后皆可行。`cursor` 独立（仅被 `tasks/adapter.rs` 经根路径消费）。`fixtures.rs` 仅被各 `#[cfg(test)]` 引用。service trait 边界不反转：`http` 子树对 `tasks::service`/`adapter` 零依赖（现状 import 清单实测），`adapter → http` 单向成立，与 12 号边界无冲突。

## 3. 符号归属与最窄可见性精确表

规则：现 `pub` 项定义迁入子文件、可见性不变，根 `http.rs` 以**逐项 `pub use` 清单**（禁 glob）回挂，公开路径 `crate::tasks::http::<名>` 全量守恒；`pub(crate)` 项以根 `pub(crate) use` 回挂。下表按归属文件逐项登记；**可见性变更恰为其中 6 项 `private → pub(super)`**，表内其余行登记为原样项（C1 变更清单全量即此 6 项，无一为 `pub`/`pub(crate)` 升降）：

| 符号（现行→新） | 归属 | 理由 |
| --- | --- | --- |
| `strict_json`：private → `pub(super)` | projection.rs | 迁入后 routes 的 create_preview(924)/cancel(1160)/release_lock(1302) 需调用 |
| `preview_expires_in_ms`：private → `pub(super)` | query.rs | create_preview 需调用 |
| `parse_path_id`：private → `pub(super)` | query.rs | 5 个带路径 ID 的 handler 需调用（1050/1078/1117/1180/1256） |
| `IDEMPOTENCY_KEY_HEADER`：private → `pub(super)` | query.rs | create_task 需读取 |
| `blocks_mutation_task`：private → `pub(super)` | state.rs | 定义随 state 迁入，消费在 routes 的 create_task handler(953) |
| `publish_task_update`：private → `pub(super)` | state.rs | 定义随 state 迁入，消费在 routes 的 create_task(985)/cancel(1153) handler |
| `parse_page_query`/`project_detail`/`PageQuery`/`accepted_response`/8 个 handler | routes.rs 内原样 | 同文件互访，routes 模块保持私有、handler 保持 `pub` |
| `child_item_json`、`hmac_sha256`、`constant_time_eq`、`scope_byte`、`TASK_*` | 归属文件内 | 消费点与定义同文件，**保持 private** |

`require_business_session` 保持 `pub(crate)`（http_events/http_device_status 跨模块消费）；`controlled_recovery_response`、`task_error_response` 虽仅 http 子树内消费，为零 diff 保持 `pub(crate)` 不降档（降档属"顺手改"，违反 C2）。`HttpTasksState` 四字段保持 `pub`（main.rs 字面量构造）。`#[allow(clippy::result_large_err)]`、`#[allow(clippy::too_many_arguments)]` 随其 item 原样迁移。

## 4. 测试与 fixture 迁移（54 个测试名一个不改，按职责映射）

分组映射（组名 = 新测试宿主模块；括号为现行号；总数守恒 7+3+4+4+34+2=54）：

| 测试组（数量） | 现区 | 新宿主（`#[cfg(test)]`） |
| --- | --- | --- |
| 授权投影纯函数 7（`main_detail_summary_counts_are_decimal_strings_over_authorized_full_set`(1407)、`main_summary_recomputes_from_visible_full_set_ignoring_full_state`(1454)、`children_page_projection_is_independent_from_main_counts`(1519)、`cancel_permission_matrix_is_exact`(1589)、`cancel_result_projection_filters_unreadable_targets`(1607)、`release_lock_requires_admin_ack_and_nonempty_reason`(1649)、`time_review_requires_at_least_one_held_subtask`(1683)） | 1406–1688 | `http/projection.rs` → `http/projection/tests.rs` |
| 输入解析纯函数 3（`decimal_u64_rejects_negative_leading_zero_and_overflow`(1691)、`page_limit_defaults_to_50_and_caps_at_200`(1717)、`idempotency_key_must_be_canonical_lowercase_uuidv4`(1733)） | 1690–1757 | `http/query.rs` → `http/query/tests.rs` |
| 游标编解码 4（`task_cursor_round_trip_success`(1766)、`task_cursor_rejects_tampered_tag_with_invalid_argument`(1785)、`task_cursor_rejects_cross_endpoint_with_invalid_argument`(1802)、`task_cursor_rejects_malformed_length_or_hex`(1815)） | 1759–1835 | `http/cursor.rs` → `http/cursor/tests.rs`（`tie_break_id`(1761) 与 `SECRET`/`CREATED_AT_MS` 常量随迁此文件，仅本组消费） |
| 门禁与错误映射单测 4（`temporary_password_forbids_admin_and_plain_with_exact_envelope`(1839)、`normal_session_passes_for_plain_and_admin`(1879)、`error_mapping_is_exact_for_every_contract_variant`(1901)、`resource_exhausted_carries_retry_after_60`(1965)）+ helper `status_and_code`(1888) | 1838–1973 | `http/gate.rs` → `http/gate/tests.rs` |
| 路由集成 34（鉴权/CSRF/限速前置 7：2382、2406、2419、2436、2454、2473、`non_admin_malformed_write_requests_prefer_400_over_403`(3061)→`http/routes/tests/gate.rs`；create_preview 3：2660、2687、2705 + create_task 5：2497、2515、2533、2562(受控恢复 503)、2597(闸门豁免读线)→`create.rs`；读线 5：detail 2723、2747、invalid_path_id 2766 + children 2779 + list 2803→`read.rs`；写线 10：cancel 2834、2873 + time_review 2899、2936、2961、2985、3011、3036 + release_lock 3109、3152→`write.rs`；错误透传 4：3183、3199、3227、3247→`errors.rs`） | 2381–3264 | `http/routes.rs` → `#[cfg(test)] mod tests`（`http/routes/tests.rs` + 子文件） |
| task.updated 事件发布 2（`task_operations_publish_task_updated_events_when_event_bus_is_configured`(3283)、`task_cancel_without_cancelled_targets_publishes_no_task_updated`(3436)）+ `collect_events`(3269) | 3266–3471 | `http/routes/tests/events.rs` |

路由与 handler 归属合计 36 例（上表「路由集成 34 + task.updated 事件 2」）按端点核对（供 DoD 3 复算）：

| 端点 | create_preview | create_task | list_tasks | task_detail | task_children | cancel | time_review | release_lock | 跨端点 gate | 错误透传 | 事件 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 用例数 | 3 | 5 | 1 | 3 | 1 | 2 | 6 | 2 | 7 | 4 | 2 |

（`invalid_path_id_is_400_before_business`(2766) 计入 task_detail 行；`controlled_recovery_gate_spares_reads_and_reopens_after_exit`(2597) 计入 create_task 行；跨端点 gate 7 例覆盖 8 端点共用门禁序列。）

- 跨域共享 fixture → `http/fixtures.rs`（test-only、`#[cfg(test)]` 私有模块 + 内容 `pub(super)`）：`fixture_time_evidence`(1343)/`fixture_session`(1359)/`fixture_detail`(1378)、`RAW_TOKEN`(1337)、`IDEMPOTENCY_KEY`(1339)、`FIXTURE_USER_ID`(3280)。消费者跨 projection/gate/routes 三域，**必须在首个消费者批次（B3）同步建立，不得晚于消费者**：gate 4 测试在 1841/1882 已消费 `fixture_session`（其体内引用 `RAW_TOKEN`，1374），故 **B3 首建 `fixtures.rs` 并同批只迁入 `fixture_session` + `RAW_TOKEN`，不得等 B4**；`fixture_time_evidence`/`fixture_detail`/`IDEMPOTENCY_KEY` 随其消费者在 B4 补齐，`FIXTURE_USER_ID` 随其消费者（事件测试 3314/3330/3350）在 B5c 迁入。过渡期尚未迁移的根测试区测试经父模块临时显式引用（`use super::fixtures::…;`，禁 glob），其组在所属批次（B4 各组、B5b 路由集成组、B5c 事件组）迁移时随组收敛为域文件顶部 import，B6 终检无 glob 残留。`CSRF_TOKEN`(1338) 仅 fake 设施消费，留 `fakes.rs`。
- fake 设施（仅路由集成消费，test-only，不得转 `tests/` 集成测试——C3 反模式，且 `FakeAuth` 用 `unreachable!` 钉「任务路由不得触达 login」等边界）：`FakeFailure`(1978)、`FakeTaskApi`(2004，含 `calls` 记录与 `detail_failure`/`detail_ok` 故障注入)、`FakeAuth`(2202)、`app`/`app_with_events`/`app_with_recovery`/`build_app`(2274/2284/2296/2304)、`get_request`(2340)、`post_request`(2351)、`envelope`(2373)、`CREATE_TASK_BODY`(2379) → `http/routes/tests/fakes.rs`，成员 `pub(super)`。
- 私有 helper 测试留本模块：`status_and_code` 随 gate 测试、`collect_events` 随事件测试、`tie_break_id` 随游标测试；**不得整搬进巨型 `tests.rs`**（总览 C3）。
- 测试函数名 54 个逐字不变；模块路径由 `tasks::http::tests::<名>` 变为 `tasks::http::<域>::tests::<名>`，过滤器 `tasks::http` 仍全量命中（§7 以清单 diff 守恒核对）。

## 5. 分批顺序、暂留位置与回滚

每批 = 一个可独立回滚提交（提交标题中文，AGENTS.md）；「每批可编译」是设计断言，本规格会话未编译验证。

| 批次 | 内容 | 暂留与回改 |
| --- | --- | --- |
| B1 | `cursor.rs` + 其 4 测试与局部 fixture（`SECRET`/`CREATED_AT_MS`/`tie_break_id`） | 根 `http.rs` 加 `mod cursor;` + 3 项 `pub use`；旧测试区未迁部分经根 re-export 继续 `use super::*` 可见，无需临时 use |
| B2 | `contract.rs` + `state.rs`（0 测试） | 根逐项 `pub use`（含 §1.1 adapter 的 22+1 名）；根内 `use contract::...`/`use state::...` 为过渡形态，B6 收敛 |
| B3 | `gate.rs` + 其 4 测试 + `status_and_code`；**同批首建 `fixtures.rs`**（仅 `fixture_session` + `RAW_TOKEN`，根挂 `#[cfg(test)] mod fixtures;`） | 根 `pub(crate) use` 三符号，http_events/http_device_status 路径不变；gate 测试域文件顶部显式 import `fixtures` 项；根测试区仍留存且消费 `fixture_session`/`RAW_TOKEN` 的测试（路由集成组，B5b 迁）经父模块临时 `use super::fixtures::…` 引用，B5b 随组收敛 |
| B4 | `query.rs` + `projection.rs` + 其 10 测试；`fixtures.rs` 补齐 `fixture_time_evidence`/`fixture_detail`/`IDEMPOTENCY_KEY`（文件已于 B3 首建） | 旧测试区删除已迁组；其余未迁测试保持 `use super::fixtures::<项>;` 级父模块临时显式引用，B5b 随组收敛、B6 终检 |
| B5a | `routes.rs`（实现整体外移，根保留 `mod routes;` 声明与 `build_tasks_router` 定义） | 0 测试；根测试区剩余测试经根装配的 `build_tasks_router` 走路由器、不直接引用 routes 项，无需 `use routes::*` 类 glob 过渡 |
| B5b | `http/routes/tests/`：`tests.rs` + `fakes.rs` + `gate.rs`/`create.rs`/`read.rs`/`write.rs`/`errors.rs`（34 测试），删除根测试区 | 根 `#[cfg(test)] mod tests` 清空删除 |
| B5c | `http/routes/tests/events.rs`（2 测试 + `collect_events`），`FIXTURE_USER_ID` 入 `fixtures.rs` | 同批 |
| B6 | 根入口清理：临时 `use` 收敛为最终 import 表、re-export 清单与 §3 逐项核对、模块文档随文件拆分同步剪切（不改语义） | 无代码移动 |

回滚粒度：任一批 revert 即回到上一批可编译形态；B1–B4 互不依赖可独立回滚，B5b/B5c 依赖 B5a，B6 依赖全部。fixture 三段时序为 B3 首建（`fixtures.rs` 含 `fixture_session` + `RAW_TOKEN`）、B4 补齐（`fixture_time_evidence`/`fixture_detail`/`IDEMPOTENCY_KEY`）、B5c 收尾（`FIXTURE_USER_ID`），每段均不晚于当段消费者；fake 设施（含 `CSRF_TOKEN`）随 B5b 建立。

## 6. 风险、例外与边界

- **C4 阈值**：估算最大目标文件 `routes.rs` ≈ 520 行、`fakes.rs` ≈ 420 行、其余 < 320 行（含测试），全部 ≤ 900 行，**无需例外说明**。
- **安全门禁零重排**：§1.3 表中三处易踩点——time_review/release_lock 的 admin 检查在 body/参数校验**之后**（非前置）、create_task 的恢复闸门在 body **之前**、`InvalidArgument` 回读分诊不得吞错——迁移时只随代码移动、逐行不改；对应测试 3061、2562/2597、2961/2985/3011/3036 为回归哨兵。
- **游标安全序**：`decode_task_cursor` 先 MAC 后字段 + `constant_time_eq` 恒定时间比较（743–779），迁移前后校验序逐字不变；`scope_byte` 隔离两域端点。
- 与 12 号：`TaskApi`/`TaskFuture`/DTO 的**归属文件**以本规格为准（contract.rs）；若 12 号实施对契约类型提出迁移需求，按总览 §5.3 先行裁决，禁止并发触碰同一文件。12 先于 06 实施（总览 §5.2 批次 D）。
- 不新增依赖（`sha2`/`hex`/`uuid`/`serde_json` 均为既有）；不动 `Cargo.toml`/`Cargo.lock`/CI；无 SQL 故无 SQL 副作用风险；本文件不被任何静态门禁按路径 grep（实测 Makefile）。
- 已知观察（记录不改，C2）：`DEFAULT_PAGE_LIMIT`/`MAX_PAGE_LIMIT` 公开但仅本文件消费、`ChildItem` 仅投影内部使用——是否降可见性留待后续任务，本次不处理。

## 7. 验证命令（登记项，本轮一律未执行、不声称通过）

前提 F2：先 `make web-dist-check`（或 `make web-build`）备妥 `apps/controller-web/dist`；随后：

```bash
make fmt-check
cargo clippy -p rsetup-controller --all-targets -- -D warnings
cargo test -p rsetup-controller --locked
cargo test -p rsetup-controller --locked -- tasks::http --list   # --locked 在 `--` 前，list 过滤在 `--` 后
make controller-test-offline                                     # 全量出口（F2/F3）
```

- 上述命令全部走既有入口，本拆分不新增任何 Makefile 目标或 CI 接线（F8）；clippy 与 test 均承担编译校验（F4）。
- 测试映射守恒核对：拆分前后各执行一次 `cargo test -p rsetup-controller --locked -- tasks::http --list`，两侧 54 个测试名清单 diff 必须为空。
- 真库轨道仅登记未验证（F6，按 [AGENTS.md](../../../../AGENTS.md) 授权）：`crates/rsetup-controller/tests/task_scheduler_sqlx.rs` 等 `--ignored` 用例触 `tasks::http` 公开面（`build_tasks_router`），实施验收时按授权另行执行，不进本规格默认门禁。

## 8. DoD（逐条自检，缺一即不成立）

1. C1：§1.1 消费面逐文件路径原样可编译；`tasks/mod.rs` 两行逐字不变；§3 re-export 清单与本文件现 `pub`/`pub(crate)` 集逐项比对零缺漏；可见性变更恰为 §3 表 6 项 `private → pub(super)`，无 glob、无 `pub` 新增。
2. C2：`git diff` 审查确认改动仅限移动 + `mod`/`use`/`pub use`；§1.3 全部行号语义（门禁顺序、事件条件、错误映射、202/400/403/409/503 响应码）零变更。
3. C3：54 个测试名守恒（§7 清单 diff 为空）、断言零删改；fixture 与 fake 全部 `#[cfg(test)]`；无内联测试外迁 `tests/`。
4. C4：目标文件全部 ≤ 900 行（§6 估算，实施时复核）。
5. C5/C6：不适用（非 ui、未触碰排除区）。C7：本文档无凭据、未触库。
6. C8/C9：本文件八项内容齐备、路径为总览 §4 固定路径；12 号边界引用成立（§6）。
7. §7 命令按前提执行并如实报告（含未执行项）；回滚按 §5 逐批可用。

## 9. 修订记录

- 2026-10-26 审查修订（**已修订待复核，不称 PASS**；上轮确认的主体结构、54 测试归属、安全门禁序、12→06 串行约束均不变）：
  - C1：`blocks_mutation_task`(347→953)、`publish_task_update`(364→985/1153) 定义随 state 迁入、消费在 routes，改 `private → pub(super)`，§3 表/引言/DoD 1 同步为 6 项，删除其「消费点同文件保持 private」错误理由。
  - C2：`fixtures.rs` 改 B3 首建（同批仅 `fixture_session` + `RAW_TOKEN`，gate 测试 1841/1882 已消费，不等 B4）；其余 fixture B4 补齐、`FIXTURE_USER_ID` B5c；§4 首消费者 B4→B3，§5 对应批次与末段统一 B3/B4/B5c 时序，并写明未迁测试经父模块 `use super::fixtures::…` 临时引用、随组回改。
  - I1：§2 依赖图更正——state 区 296–399 无 gate 响应函数调用，真实 gate 三符号依赖在 routes；更正为 `contract` ← `query`/`projection`/`state`、gate 独立、routes → {contract, state, gate, query, projection}、根装配，无新增环，B2 先于 B3 可行。
  - M1：`parse_path_id` 消费 handler 更正为 5 个（1050/1078/1117/1180/1256）；`strict_json` 升档理由补 create_preview(924)，不再仅列 cancel/release。
  - M2：adapter `TimeReviewDecision` 全路径引用行号 783/785→783/786；`CancelTaskResponse`(223)、`TaskOperation`(69，derive 67)；cursor.rs 常量区间 57–64→59–64（`IDEMPOTENCY_KEY_HEADER`(58) 归 query）；B5a 删除非必要 `use routes::*` glob 过渡。

## 10. 实施登记

批次1实施登记（2026-10-28，提交afcaffc；审查PASS——游标域（常量59–64、实现670–779、测试常量1340–1341、测试1759–1834）自c7ff640基线零漂移逐字迁入新建tasks/http/cursor.rs（209行，含内联tests），实现段110行与测试本体75行机械diff逐字一致；游标安全序逐字在位：decode_task_cursor先MAC后字段、constant_time_eq恒定时间比较、scope_byte隔离0x01/0x02；根http.rs 3472→3279，经私有mod cursor加显式pub use回挂恰3名出口（TaskCursorScope/encode_task_cursor/decode_task_cursor），无glob；全diff零可见性升降（3名保持pub，scope_byte/hmac_sha256/constant_time_eq/TASK_CURSOR_*保持private，零pub(crate)/pub(super)）；tasks/mod.rs两行与adapter 22名import路径守恒；tasks::http 54测试守恒分布cursor::tests 4+根tests 50，multiset diff为空；IDEMPOTENCY_KEY_HEADER(58)留根归query；2项信息级minor：根测试区组分隔注释与段尾空行未随迁（纯边界装饰，新文件以自有模块文档开头）；唯一use适配为cursor::tests::use super::*（与原根tests模式一致）；验证五条全PASS（fmt-check零diff/clippy -D warnings/test全量0失败/tasks::http 54绿+list分布/rustdoc -D broken_intra_doc_links门禁））

批次2实施登记（2026-10-28，提交d16901d+修复520f761；审查首轮FAIL（1项major：fmt-check门禁红，根测试区2处use适配行不符rustfmt——http.rs:883 use std::sync::Arc应移std组、:888 crate::http_auth import超宽应竖排），经定点修复提交520f761（仅http.rs一个文件4+/2-，恰2处use行格式归位：Arc移std组、http_auth import竖排花括号，零语义变化）后复核五条全绿转PASS，d16901d+520f761合并视为批次2终态。迁移事实：契约段（基线63–291，20项含TimeReviewDecision）与状态段（基线293–396）自d16901d^零漂移逐字迁入新建tasks/http/contract.rs（241行）与state.rs（113行），根http.rs 3279→2957；唯一实质可见性差异为恰2处private→pub(super)（blocks_mutation_task/publish_task_update，§3清单5/6项）；TimeReviewDecision经核实为http.rs本文件定义（基线186–190，Deserialize+rename_all serde契约、TimeReviewTaskRequest.decision用），与tasks::service::time_policy::TimeReviewDecision（无serde）为独立两类型（adapter.rs:69 import service版与783/786全路径http版映射、tests/time_consumption.rs:36与:39 as ServiceDecision别名区分，双类型并存实证），迁contract.rs正确；根64–72逐项pub use回挂20契约名+HttpTasksState（零缺漏、无glob），adapter 22名import+全路径TimeReviewDecision、production.rs:38/58/123、main.rs:42/279四字段字面量构造、http_events.rs:92与http_device_status.rs:68（require_business_session仍在根:78 pub(crate)原样，B3才迁gate）、三集成测试消费面路径守恒；tasks/mod.rs零改动、0测试迁移，tasks::http 54测试守恒分布不变（cursor::tests 4+根tests 50，集合comm恒等）；根diff hunk仅3处：import区瘦身（随迁出删除FromRef/Deserialize/Arc/HttpAuthState等10名，EventBus改#[cfg(doc)]保模块文档链接）、契约+状态段（60–399）替换为mod声明+回挂、根测试区use适配（净+2行纯import，无测试体改动）；gate段/解析段/判定段/routes实现区零触碰；contract.rs头部#[cfg(doc)] use super::project_task_children与state.rs必要use（TimeEvidence等）属维持intra-doc链接的允许适配（rustdoc门禁证明链接有效）；修复后复核五条全绿（fmt-check exit 0/clippy -D warnings/test全量0失败/tasks::http 54绿+--list分布4+50不变/rustdoc -D broken_intra_doc_links零断链）；2项非阻断观察：state.rs头部use区与段首doc间多1空行（纯装饰rustfmt不报）、rustdoc 29条非断链类warning为既有项未由本批引入）



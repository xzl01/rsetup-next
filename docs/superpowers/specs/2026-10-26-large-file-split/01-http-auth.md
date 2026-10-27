# 子规格 01：`crates/rsetup-controller/src/http_auth.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 01 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未连接数据库、未提交。

撰写基线：分支 `dev-aghost`，工作树头提交 `c7ff640`。行号为撰写时实测，实施时须按当时工作树重新核对（总览 §3 并行改动注记）。

## 1. 实测基线与锚点

对 [crates/rsetup-controller/src/http_auth.rs](../../../../crates/rsetup-controller/src/http_auth.rs)（5,775 行）实测：

- 实现区 1–1,409 行（约 1,410），测试模块 1,411–5,775 行（约 4,365，单一 `#[cfg(test)] mod tests`），与总览 §3 快照「1,410 / 4,365」一致。
- 测试函数 **77 个**（12 个 `#[test]` + 65 个 `#[tokio::test]`，按属性实测）。任务简报所称「96 个」与实测不符，本文一律以 77 为准；差异不改变「内联测试约占 3/4、C3 为主要难度」的总览判断。
- 实现区主要锚点（符号 → 行区间）：

| 锚点 | 行区间 | 说明 |
| --- | --- | --- |
| `AuthFuture`、`SessionAuthority`、`trait AuthServiceApi` | 46–115 | 对象安全的认证契约边界（含 `session_authority`/`is_live_session`） |
| `MAX_CSRF_ENTRIES`、`CsrfBinding`、`MAX_PASSWORD_BYTES` | 117–131 | CSRF 上限/绑定值类型、口令字节上限 |
| `WindowRateLimitError`、`EndpointId`、限速常量、`RateWindowLimiter` | 133–261 | 1s 滑窗限流器（含 `#[cfg(test)] global_prune_count` 字段，定义 183–184，测试用例用于 4,874–4,914） |
| `HttpAuthState` + `new`、`AppState`、`impl FromRef` | 263–307 | handler 状态与 router 状态 |
| `build_http_router` | 314–329 | 7 条 `/api/v1/auth/*` 路由 + `crate::build_router` probes 合并 |
| `parse_sessions_query`、`sessions_list`、`revoke_others_handler`、`revoke_by_id` | 331–705 | 会话管理 handler 组 |
| `password`、`logout`、`me` | 707–1,047 | 凭据 handler 组 |
| `prune_expired_csrf`、`CSRF_PRUNE_THRESHOLD`、`prune_dead_csrf`、`prune_csrf_table` | 1,049–1,124 | CSRF 表修剪 helper（12h 单调 + 阈值触达的死会话扫描） |
| `login` | 1,126–1,228 | 登录（pre-session gate + LoginRateLimiter + CSRF 容量预检） |
| body 解析与错误响应 helpers | 1,230–1,331 | `bad_body`、`parse_password_body`、`parse_logout_body`、`bad_*_body`、`bad_sessions_*`、`invalid_sessions_request`、`sessions_not_found` |
| `parse_login_body`、`valid_username`、`login_success` | 1,333–1,409 | 登录请求校验与成功路径 |

- 测试区（1,412–5,775）：fixture 与共享 helper `FakeLogin/FakeSession/FakePassword/FakeLogout/FakeAuth`（1,456–1,889 区段）、`lazy_db/app_state/router/seed_live_csrf_bindings/login_request/me_request/call/assert_error`（1,891–2,047）、`password_request/logout_request/seed_session_and_csrf(_custom)`（3,826–3,905）、`sessions_list_request/revoke_others_req/revoke_by_id_req`（5,138–5,218）、`test_group_1..11`（5,220–5,775，sessions 域）。
- 唯一生产构造点（实测全仓 grep `HttpAuthState::new`）：[crates/rsetup-controller/src/main.rs](../../../../crates/rsetup-controller/src/main.rs) 第 241 行（`LiveAuth` over `AuthService<SqlxIdentityRepository>`）；其余构造点全部位于 `#[cfg(test)]` 代码或 `crates/rsetup-controller/tests/` 集成测试。`HttpAuthState::new` 作为 pub 构造器必须保持对外可见。

### 1.1 既有对外 API 面（C1 逐项，拆分后路径必须逐字不变）

`lib.rs` 第 13 行 `pub mod http_auth;`、第 34 行 `pub use http_auth::{AppState, HttpAuthState, build_http_router};`。crate 内/集成测试经 `crate::http_auth::…` / `rsetup_controller::http_auth::…` 实际导入的符号集合（实测 grep 全仓）：

- 类型/边界：`AuthFuture`、`AuthServiceApi`、`SessionAuthority`、`CsrfBinding`、`EndpointId`、`WindowRateLimitError`（api/、http_gate、http_events、http_device_status、tasks/http、production、tests/*.rs 共 20+ 文件引用）。
- 状态与装配：`HttpAuthState`（含 pub 字段与 `new`）、`AppState`、`build_http_router`（main.rs、production.rs、lib.rs 再导出）。
- 当前 `pub` 但 crate 外无引用、仍保留 `pub` 以免收窄公开面（C1「公开符号不变」从严解释）：`RateWindowLimiter`（经 `HttpAuthState` pub 字段暴露）、`MAX_CSRF_ENTRIES`、`CSRF_PRUNE_THRESHOLD`、`RATE_WINDOW_1S`、`MAX_RATE_LIMITER_KEYS`、`WRITE_RATE_LIMIT_PER_SEC`、`READ_RATE_LIMIT_PER_SEC`。
- 认证 service 内部（`auth::service`、`auth::session`、`http_live::LiveAuth`）由 11 号子规格负责，本拆分不触碰，只保持对其的单向依赖。

## 2. 目标文件树

纯职责拆分（C1）：`http_auth.rs` → `http_auth/` 私有模块树 + 薄门面 `mod.rs`。拟建路径（均尚不存在）：

```text
crates/rsetup-controller/src/http_auth/
├── mod.rs              # 模块 doc + #[cfg(test)] mod 声明 + 逐项 pub use 门面（< 60 行，仅 B5 终态；B1–B4 门面暂存尚未迁出的实现与测试，见 §7）
├── contract.rs         # AuthFuture / SessionAuthority / AuthServiceApi
├── rate_limit.rs       # WindowRateLimitError / EndpointId / 限速常量 / RateWindowLimiter
├── state.rs            # HttpAuthState(+new) / AppState / impl FromRef
├── csrf.rs             # MAX_CSRF_ENTRIES / CsrfBinding / CSRF_PRUNE_THRESHOLD / prune_* helpers
├── login.rs            # login / login_success / parse_login_body / valid_username / bad_body / MAX_PASSWORD_BYTES
├── me.rs               # me
├── password_logout.rs  # password / logout / parse_password_body / parse_logout_body / bad_{password,logout}_body
├── sessions.rs         # parse_sessions_query / sessions_list / revoke_others_handler / revoke_by_id / sessions 错误 helpers
├── router.rs           # build_http_router
└── tests_support.rs    # 仅测试：FakeAuth 家族 + app_state/router/请求构造/断言 helpers（经 cfg(test) 声明）
```

预计成品规模（实现 + 归属测试，含注释空行）：`rate_limit.rs` ≈ 620、`csrf.rs` ≈ 470、`login.rs` ≈ 960、`me.rs` ≈ 880、`password_logout.rs` ≈ 1,340、`sessions.rs` ≈ 1,120、其余各 < 250。C4 口径（900–1,500 须内聚论证）：

- `login.rs`：14 个登录测试中有 5 个（容量短路、锁内权威容量复检、512B 口令上限、限流计数）与 handler 门禁顺序不可分，测试与 `login_success` 同位是 C3 的直接要求，拆散会割裂「凭据检查前的门禁链」这一被测契约。
- `password_logout.rs`：两个 handler 共享同一 gate 链（Host/Origin → cookie → authenticate → 绑定 CSRF → 共享 write_limiter → body），`write_endpoints_*` 4 个测试对两者做参数化同测，`seed_session_and_csrf` fixture 为其共享；`parse_logout_body` 同时被 `revoke_others`/`revoke_by_id` 复用。
- `sessions.rs`：同一子资源（列表 + 两种撤销）的三个 handler 共享查询/体解析、错误 helper 与门禁顺序，`test_group_1..10` 与 handler 门禁注释逐段对应；本轮不再细分，后续如超限可先拆 `revoke_by_id`。

## 3. 符号 → 目标模块对应表

文件形态明确为“原子替换：http_auth.rs → http_auth/mod.rs（未迁实现与测试整体暂留mod.rs；对照authz/sqlx_repo批次E先例）——同一提交内删旧建新，禁二者并存（E0761）”。顶层符号逐一归属（函数/常量按上文锚点表移动，签名与实现体逐字不变）：

| 现符号（`crate::http_auth::` 下） | 目标文件 | 新可见性（模块树内） | 门面 `mod.rs` 出口 |
| --- | --- | --- | --- |
| `AuthFuture` / `SessionAuthority` / `AuthServiceApi` | contract.rs | `pub` | `pub use` 逐项 |
| `WindowRateLimitError` / `EndpointId` / `RateWindowLimiter` | rate_limit.rs | `pub` | `pub use` 逐项 |
| `RateWindowLimiter.entries` 字段 | rate_limit.rs | `#[cfg(test)] pub(super)`（仅测试构建；清单外第 1 项 / test-only 项；生产构建保持 private） | 不出口 |
| `RATE_WINDOW_1S` / `MAX_RATE_LIMITER_KEYS` / `WRITE_RATE_LIMIT_PER_SEC` / `READ_RATE_LIMIT_PER_SEC` | rate_limit.rs | `pub` | `pub use` 逐项 |
| `HttpAuthState`（含 `new` 与全部 pub 字段）/ `AppState` / `impl FromRef<AppState> for Arc<HttpAuthState>` | state.rs | `pub`（impl 随类型留在 state.rs） | `pub use` 逐项 |
| `MAX_CSRF_ENTRIES` / `CsrfBinding` / `CSRF_PRUNE_THRESHOLD` | csrf.rs | `pub` | `pub use` 逐项 |
| `prune_expired_csrf` / `prune_dead_csrf` | csrf.rs | 私有（仅 `prune_csrf_table` 与同模块测试调用） | 不出口 |
| `prune_csrf_table` | csrf.rs | `pub(super)`（login/me 调用） | 不出口 |
| `login` / `login_success` / `parse_login_body` / `valid_username` / `bad_body` / `MAX_PASSWORD_BYTES` | login.rs | 仅 `login` 为 `pub(super)`（router.rs 装配需要）；`login_success`（实测仅被同文件 `login` 与同模块测试引用）、`parse_login_body`、`valid_username`、`bad_body`（实测仅 `login` 内 1,148–1,168 五处调用）保持私有；`MAX_PASSWORD_BYTES` 为 `pub(super)`：定义在 login.rs（现 131 行，与 CSRF 上限常量同处 117–131 区段，归属随其唯一职责口令上限归 login.rs），被 login.rs 自身（`parse_login_body` 现 1,347 行）与 password_logout.rs（`parse_password_body` 现 1,256 行）引用 | 不出口 |
| `me` | me.rs | `pub(super)` | 不出口 |
| `password` / `logout` / `parse_password_body` / `bad_password_body` / `bad_logout_body` | password_logout.rs | `password`、`logout` 为 `pub(super)`；其余私有 | 不出口 |
| `parse_logout_body` | password_logout.rs | `pub(super)`（logout 与 sessions.rs 两 revoke handler 共用，既有事实） | 不出口 |
| `parse_sessions_query` / `sessions_list` / `revoke_others_handler` / `revoke_by_id` | sessions.rs | handler 为 `pub(super)`；`parse_sessions_query` 私有（实测仅 `sessions_list` 于现 435 行一处调用） | 不出口 |
| `bad_sessions_query` / `bad_sessions_id` / `bad_sessions_body` / `invalid_sessions_request` / `sessions_not_found` | sessions.rs | 私有 | 不出口 |
| `build_http_router` | router.rs | `pub` | `pub use` 逐项 |

可见性变更清单（C1 要求，逐项，共 10 项新增 `pub(super)`）：`prune_csrf_table`、`login`、`me`、`password`、`logout`、`parse_logout_body`、`sessions_list`、`revoke_others_handler`、`revoke_by_id`、`MAX_PASSWORD_BYTES`。前 9 项原本就是 `http_auth` 文件私有项，拆分后必须跨兄弟子模块可达（handler 供 router.rs 装配、`prune_csrf_table` 供 login/me 调用、`parse_logout_body` 供 sessions.rs 复用）；第 10 项 `MAX_PASSWORD_BYTES` 归 login.rs，但其唯一另一半调用方 `parse_password_body` 随 password_logout.rs 走，测试区实测对该常量零直接引用，不依赖测试可见性，故仅需 `pub(super)` 供兄弟模块使用。`pub(super)` 的可见范围恰为 `http_auth` 子树，不扩大 crate 公开面。`login_success`、`bad_body`、`parse_sessions_query`、`prune_expired_csrf`、`prune_dead_csrf`、sessions 错误 helpers 等经实测确认仅同模块（含同模块测试）使用的项一律保持私有、不进入本清单。各新增项的实际提升发生在其所属实现离开门面的批次（见 §7），提升前父项仍由子模块 `use super::…` 直访。

清单外第 1 项（test-only 字段项）：`RateWindowLimiter.entries` 字段登记为 `#[cfg(test)] pub(super)`（仅测试构建；清单外第 1 项）——理由：me/sessions 等测试跨模块观测/注入 limiter 内部 `entries` 队列（暂留门面测试 5 处直访 `state.auth.read_limiter.entries` 约 2934/3116/4771/4779/5017 行 + 终态 me.rs/sessions.rs 同需），生产构建保持 private。除此之外无任何可见性升降；不使用 `pub(crate)`，不使用通配 re-export，不新增 `pub`。

## 4. 依赖方向与装配（无循环证明）

`use` 方向（单向、自上而下的层序）：

```text
contract   ←（无出边；仅依赖 crate::auth::service、crate::error）
rate_limit ←（无出边；仅依赖 std）
csrf       → contract（prune_dead_csrf 以 &dyn AuthServiceApi 探测）、auth::session（仅 `ABSOLUTE`，prune_expired_csrf 现 1,061 行）
state      → contract, rate_limit, csrf, http_security, db
login      → state, contract, csrf, http_gate, http_security, http_api, auth::service（`Login` 等类型，现 31 行 use）
me         → state, contract, csrf, rate_limit, http_gate, http_api, auth::session（`token_digest`，现 982 行）
password_logout → state, contract, csrf, rate_limit, login（仅 `MAX_PASSWORD_BYTES` 常量，见 §3）, http_gate, http_security, http_api, auth::session（`token_digest`，现 741/823/873 行）
sessions   → state, contract, csrf, rate_limit, password_logout（仅 parse_logout_body）, http_gate, http_api, auth::session（`token_digest`，现 407/508/614 行）
router     → state + 全部 handler 模块
mod.rs     → 声明全部子模块并逐项 pub use
```

- 路由装配唯一点在 `router.rs::build_http_router`，与现状一致（原 314–329 行整体移动）：`crate::build_router(state.db.clone())` 合并 auth 路由，7 条注册路径与 method 逐字不变。
- 状态与 trait 归属：`AuthServiceApi` 留在 contract.rs（实现方 `http_live::LiveAuth` 与消费方 api/、tasks/http、http_events 全部经 `crate::http_auth::AuthServiceApi` 路径导入，不感知内部分层）；`HttpAuthState`/`AppState`/`FromRef` impl 三者同文件留在 state.rs，避免 impl 跨模块分散。
- 对 11 号的边界：本文件依赖 `auth::service`（Login/Session/SessionPage/IdentityUser）、`auth::session::ABSOLUTE`/`token_digest`、`http_live::LiveAuth`，方向为 http_auth → auth/http_live 单向；11 号对认证 service 的拆分不反向依赖本模块的新子文件，串行集成关系按总览 §5.2 批次 C（11 → 01）执行，边界以本节依赖表为唯一出处。
- rustdoc 跨模块链接：`[`prune_dead_csrf`]` intra-doc 链接在现源码出现两处跨模块断点——270 行（`HttpAuthState.csrf` 字段注释，B1 随 state.rs 走）与 958 行（`me` 函数头注释，B4 随 me.rs 走）；`prune_dead_csrf` 保持 csrf.rs 私有（B2 就位），原样链接届时必然无法解析。搬迁所在批次把这两处链接改为 rustdoc 可解析的路径形式或降级为纯 code 文本（只修链接指向，注释文字与语义不改）；csrf.rs 同模块的 1,067 行处不动。修复项在 §8 登记 `cargo doc` 断链验证。

## 5. 测试迁移计划（C3）

77 个测试按被测职责归位到各子文件的 `#[cfg(test)] mod tests`，禁止整个搬进巨型 `tests.rs`（总览 C3）。77 的守恒范围仅为 `http_auth` 模块（`cargo test -p rsetup-controller` 整包还含其余模块的测试，包级总数不得当作断言，比对方式见 §8）。逐组映射（数量合计 77）：

| 归属文件 | 测试（行号为现状） | 数量 |
| --- | --- | --- |
| login.rs | 2,050–2,724：`login_success_contract` … `login_success_keeps_authoritative_in_lock_capacity_check`（14，含 CSRF 容量/限流/512B 门禁） | 14 |
| router.rs | `probes_kept_and_other_auth_routes_registered`（2,726） | 1 |
| me.rs | 2,780–3,322 与 3,413–3,485：me 合同、gate、Task 3f 预检、epoch、读限流、容量复用 17 个（3,487–3,536 为 csrf 测试 helper，非 me 测试，见下行与下文） | 17 |
| csrf.rs | `me_and_login_succeed_when_full_map_is_only_expired_bindings`(3,325，被测语义是「容量检查前的 prune 释放」，其时钟受限 fallback 分支直接调用同模块私有 helper `prune_boundary_coverage`，故随 prune 测试归 csrf)、`prune_keeps_active_and_deletes_at_12h_boundary`(3,539)、`prune_deletes_future_issued_binding`(3,549)、`prune_dead_csrf_removes_only_dead_bindings`(3,577)、`csrf_table_full_of_dead_bindings_is_swept_and_login_succeeds`(3,639)、`me_sweeps_dead_bindings_at_cap_and_keeps_every_live_one`(3,699)、`prune_csrf_table_sweeps_only_at_the_threshold`(3,777) | 7 |
| password_logout.rs | password 8（3,908–4,176）+ logout 3（4,178–4,288）+ `write_endpoints_*` 4（4,290–4,705） | 15 |
| rate_limit.rs | `#[test]` 单元 8（4,706–4,920，用 `global_prune_count`）+ 端到端 oneshot gate 4（4,926–5,136） | 12 |
| sessions.rs | `test_group_1..10`（5,222–5,761） | 10 |
| contract.rs | `test_group_11_trait_object_safety_compilation_proof`（5,764，验证实为 `AuthServiceApi` 对象安全与 `LiveAuth` 适配，非 sessions 职责） | 1 |

跨模块私有访问策略：`prune_*` 单测随实现留在 csrf.rs 的 tests（同模块可见私有项）；`prune_boundary_coverage`（3,487–3,536，无 `#[test]` 属性、非测试用例）是 csrf.rs 测试模块的共享 helper，内部调用同文件私有 `prune_expired_csrf`，必须随 prune 测试整组留在 csrf.rs 的 `mod tests`，禁止放入 me.rs 或 tests_support.rs（放 tests_support 同样因 `prune_expired_csrf` 私有而无法编译）；`global_prune_count`（`#[cfg(test)] pub` 字段）随 `RateWindowLimiter` 留在 rate_limit.rs；`write_endpoints_*` 只经 router 与 pub 状态字段（`state.auth.csrf` 等）观察，无需额外可见性。

共享 fixture（test-only）：FakeAuth 家族（1,456–1,889）、`clone_controller_error`/`fake_token`（1,427–1,454）、`lazy_db`/`app_state`/`router`/`seed_live_csrf_bindings`/`login_request`/`body_json`/`valid_request`/`valid_cookie`/`me_request`/`call_status`/`call`/`assert_error`（1,891–2,047）、`password_request`/`logout_request`/`password_body`/`seed_session_and_csrf(_custom)`（3,826–3,905）、sessions 三个请求构造 helper（5,138–5,218）整体移入 `tests_support.rs`；`tests_support.rs` 在 B5 创建（与 §7 一致，B1–B4 期间 fixture 留在门面内联测试、依赖 fixture 的测试也暂留门面，见 §7 各批中间态）。`mod.rs` 以 `#[cfg(test)] mod tests_support;` 声明，item 一律 `pub(super)`（含 `FakeAuth` 对 `AuthServiceApi` 的 impl）。非 test 构建零产物、零公开面；不为其新增任何面向生产的接口，不把内联单测升级成 `tests/` 集成测试（C3）。测试代码对被测模块 `pub(super)` 项的引用方向与 §4 层序一致（测试 → 被测模块，不构成生产依赖边）；注意测试区实测对 `MAX_PASSWORD_BYTES` 零直接引用、对 `prune_csrf_table` 的引用（3,796/3,817）位于 csrf.rs 同模块测试内，二者都不构成跨模块测试引用场景，不进入上述方向示例。

## 6. 语义保持清单（C2，逐条不得变）

- 路由协议：7 条路径、method、`probes.merge(auth)` 合并方式逐字不变；`/healthz`、`/readyz` 仍由 `crate::build_router` 提供。
- 门禁顺序（以各 handler 内编号步骤注释为准，按源码实测逐段核准，禁止借拆分重排；各族顺序不同，不存在统一的一条链）：
  - 读族 `me`（961–1,046）：Host 单值精确匹配（GET 不查 Origin）→ 会话 cookie 单值 → Task 3f 内存预检 `is_live_session`（975–986：miss 即 401 并删除该 digest 的 CSRF 绑定，零后端调用）→ `authenticate`（988：仅 `InvalidArgument` 删绑定）→ 读限流 50/s 按 `(EndpointId::Me, digest)`（997，先于 epoch 与 CSRF）→ `authz_epoch`（1,009）→ 同一锁内双道 prune 后 CSRF 复用/签发（1,019–1,039）。
  - 读族 `sessions_list`（393–479）：Host →（GET 不查 Origin）→ cookie → Task 3f 预检（408）→ `authenticate`（414）→ 读限流 50/s 按 `(EndpointId::Sessions, digest)`（423）→ 手工 query 解析 → service。读族无 `check_session_write` CSRF 校验。
  - 写族 `password`（713–836）/`logout`（845–937）：Host → Origin → cookie → `authenticate`（733/865 直接 authenticate，无 Task 3f 预检）→ 绑定 CSRF 校验 `check_session_write`（747/879）→ 共享 write 限流 20/s 按 digest（760/892）→ Content-Type/CL 预检/1 MiB 受限读取 → body 形状 → service。写族是 CSRF 校验先于限流，与读族「限流在先」相反。
  - 写族 `revoke_others_handler`（487–583）/`revoke_by_id`（592–705）：Host → Origin → cookie → Task 3f 预检（509/615）→ `authenticate`（515/621）→ 绑定 CSRF 校验 `check_session_write`（524/630）→ 共享 write 限流 20/s 按 digest（537/643）→ body（`application/json`、≤1 MiB、恰好 `{}`）→ service。
  - `login`（1,132–1,222）：Host+Origin 预会话门禁 → JSON Content-Type、CL 预检与 1 MiB 受限读取 → body 形状 + username 规范化（512B 口令上限在此）→ 可信 ConnectInfo（缺失 503 `NOT_READY`）→ `LoginRateLimiter`（仅 `InvalidArgument` 计失败）→ CSRF 容量 peek（先双道 prune 再判满）→ 凭据检查 → `login_success`。
  - 各族共用的末端：service 调用与错误消歧（`InvalidArgument` 重认证二义区分；`PermissionDenied`→403 force-password；`NotFound`→404；`ResourceExhausted`→503）。
- 关键不变式：`me` 的读限流在 `authz_epoch` 读取与 CSRF 签发/复用**之前**；CSRF 表两道 prune 顺序（12h 单调 + 阈值 3/4 死会话扫描）及「锁内容量复检为准」；登录在凭据检查前做 CSRF 容量预检与 `LoginRateLimiter`（仅 `InvalidArgument` 计失败）；`login_success` 先验 cookie 再落绑定；口令 512 UTF-8 字节上限在限流与凭据检查之前。
- 错误信封：全部经 `http_api::{err_response, ok_response}`，固定 code/message_key/空 params；`login` 的 `Database`→503 `STORAGE_UNAVAILABLE`、缺 `ConnectInfo`→503 `NOT_READY` 等特殊映射不变。
- 凭据/令牌/原始后端错误不进响应与日志（文件 doc 契约随模块 doc 保留）。

## 7. 迁移批次与回滚（C8-7）

| 批次 | 内容 | 回滚单位 |
| --- | --- | --- |
| B1 | 原子替换：`http_auth.rs` → `http_auth/mod.rs`（未迁实现与测试整体暂留 mod.rs；对照 authz/sqlx_repo 批次 E 先例）——同一提交内删旧建新，禁二者并存（E0761）；拆出 contract.rs、rate_limit.rs、state.rs（含 FromRef）的实现；仅 rate_limit 的 8 个纯 unit 测试随迁<br><br>批次 1 实施收窄（2026-10-27，提交 99de683）：本批落地为原子替换+rate_limit 域（8 纯 unit 测试随迁）+RateWindowLimiter.entries #[cfg(test)] pub(super)（实施前修订第2项）；contract/state 顺延后续批次，§7 B1 原列 contract.rs/state.rs 未在本批，不构成越批或遗漏。 | 单提交 revert |
| B2 | csrf.rs + router.rs 的实现（`build_http_router` 从门面移入 router.rs）；本批不迁测试 | 单提交 revert |
| B3 | sessions.rs 的实现（含可见性提升与 `super::parse_logout_body` 临时引用）；不迁测试 | 单提交 revert |
| B4 | login.rs + me.rs 的实现（含可见性提升与本批 rustdoc 断链修复）；不迁测试 | 单提交 revert |
| B5 | password_logout.rs 的实现；创建 tests_support.rs，B1–B4 暂存测试全部按 §5 归位（rate_limit 4 oneshot、csrf prune 组 7、router 探测 1、login 14、me 17、password_logout 15、sessions 10、contract 1） | 单提交 revert |

各批中间态与回改（每批保持整树可编译，这是规格层面的设计属性，本会话未做实际编译验证）：

- B1：mod.rs 暂存全部未拆实现与内联测试；lib.rs 所需的 `AppState`/`HttpAuthState`/`build_http_router` 三符号全程不丢——前两者经 `pub use state::…` 出口、后者此时仍定义在门面自身出口。已拆子模块经 `use super::…` 访问仍留在门面的父项（子模块对父模块私有项可见），故 B1 不需要为任何暂留项预升可见性；门面对外仍按 §3 终态逐项 `pub use`。此批门面远超 60 行，< 60 行只是 B5 终态。270 行处 `[`prune_dead_csrf`]` rustdoc 断链随 state.rs 本批修复（见 §4）。4 个 oneshot/fixture 测试（4,926–5,136）依赖 FakeAuth/app_state 等 fixture，暂留门面内联测试（暂存位置），B5 迁回 rate_limit.rs。
- B2：csrf.rs、router.rs 实现就位；router.rs 经 `super::` 直引仍留在门面的 7 个 handler。FakeAuth 等 fixture 未形成，本批 0 个测试随迁：router 探测测试（2,726）、prune 纯单元与 helper（3,487–3,575）、依赖 FakeAuth 的 prune 测试（3,577–3,824）全部暂留门面，B5 随 §5 表迁 csrf.rs/router.rs。
- B3：sessions.rs 实现就位并提升 `sessions_list`/`revoke_others_handler`/`revoke_by_id` 为 `pub(super)`；`parse_logout_body` 此时仍在门面，sessions.rs 以 `use super::parse_logout_body` 引用（父项可见，无需为其提前提升可见性）。group 1–10（5,222–5,761）与 sessions 请求 helper（5,138–5,218）依赖 fixture，暂留门面。
- B4：login.rs、me.rs 实现就位；`login`/`me` 提升 `pub(super)`；`MAX_PASSWORD_BYTES` 随 login.rs 提升 `pub(super)`（门面临时留存的 `parse_password_body` 此时以 `super::MAX_PASSWORD_BYTES` 反向引用子模块项，父不能直访子私有项，故必须在本批提升）。958 行处 rustdoc 断链随 me.rs 本批修复（见 §4）。login 14 个与 me 相关测试依赖 fixture，暂留门面。
- B5：password_logout.rs 实现就位（`password`/`logout`/`parse_logout_body` 提升 `pub(super)`），同时回改 B3 遗留引用：sessions.rs 的 `super::parse_logout_body` 改为 `super::password_logout::parse_logout_body`；门面内 `parse_password_body` 等 helpers 拆入各自归属文件后，创建 tests_support.rs 收纳 §5 所列 fixture helper 各一份，再把 B1–B4 暂存测试按 §5 表逐组迁往归属子模块（含 rate_limit 的 4 个 oneshot、contract.rs 的 `test_group_11`——其 `lazy_db()` 属 fixture 依赖，故也到 B5 才归位）。跨批如曾产生同 fixture 的临时重复副本，本批只删除重复副本、保留唯一份；不合并形似 helper、不改任何断言（总览 C3）。迁移完成后门面只剩模块 doc、`mod` 声明与逐项 `pub use`，达 < 60 行终态。

每批完成后整树可编译、77 个测试不增不减不改断言——尚不能归位的测试一律留在门面内联测试暂存（不删除、不重复、不断言弱化），B5 全部归位后 `http_auth` 范围内仍为 77（比对方式见 §8）；批次边界即回滚点（单提交 revert），不跨批混入其他子规格的改动。B1 先行的理由：门面 re-export 一旦就位，B2–B5 对 crate 其余文件零 diff。

## 8. 验证命令与前提（总览 §6.2 基线，本轮未执行任何一条）

前提（F2）：`rsetup-controller` 的 build.rs 编译期要求 `apps/controller-web/dist` 在位，直接跑 cargo 前先 `make web-dist-check`（[Makefile](../../../../Makefile) 第 92 行，实测存在；干净环境下其可能触发 `npm ci`/vite，见总览 F3）。

- `make fmt-check`（Makefile 第 148 行；不得以 `cargo fmt --all --check` 判定，AGENTS.md 口径）。
- `cargo clippy -p rsetup-controller --all-targets -- -D warnings`（F2 前提）。
- `cargo test -p rsetup-controller --locked`（F2 前提）。定向回归命令：`cargo test -p rsetup-controller --locked -- http_auth::`（`http_auth` 是 `lib.rs:13` 顶层 `pub mod`，不在 `auth::` 命名空间下；测试过滤器由 `auth::http_auth::` 修正为 `http_auth::`，拆分前 77 条测试前缀为 `http_auth::tests::`，拆分后为 `http_auth::<子模块>::tests::`）。77 的守恒范围是 `http_auth` 模块而非整包——该包还有 api/、db/、tasks/ 等大量其他单元与集成测试，包级测试总数不得用来冒充 77 守恒。可核准比对方式：拆分前与 B5 完成后各跑一次 `cargo test -p rsetup-controller --lib --locked -- --list`（同 F2 前提），分别取 `http_auth::` 前缀的测试名集合——拆分前形如 `http_auth::tests::<name>`，拆分后形如 `http_auth::<子模块>::tests::<name>`；按「剥除中间子模块名与 `tests::` 段、保留末级测试名」建立重命名映射做集合比对，要求双向一一对应、两侧均为 77、无孤儿项；若某测试更名，映射须登记旧名→新名并经人工核准，不得默默吞改名。
- rustdoc 断链登记（M4 关联，见 §4）：`RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc -p rsetup-controller --no-deps`（F2 前提）——验证 `[`prune_dead_csrf`]` 两处跨模块链接修复后 intra-doc 链接零断链；此项只核对链接解析，不构成语义验证。
- 全量回归出口：`make controller-test-offline`（F2/F3 前提）。
- `tests/` 下 `events_sse.rs`、`production_composition.rs`、`recovery_routes.rs`、`support/management_http_fixture.rs` 等经 `rsetup_controller::http_auth::…` 导入的集成测试由上述命令覆盖，路径面不得变化。
- 真库轨道（F6：`mysql_identity.rs`、`tidb_identity.rs` 等 `--ignored`）不在本拆分默认验证内，不得默认执行；本拆分的测试全部为 FakeAuth/路由器级，不触库。

## 9. 风险与 DoD

风险：① 测试归属核对失准（尤其 `test_group_11` 属 contract 而非 sessions、`write_endpoints_*` 属 password/logout 共享门禁、`me_and_login_succeed_when_full_map_is_only_expired_bindings`(3,325) 属 csrf 而非 me、helper `prune_boundary_coverage` 只能留 csrf 测试模块）——以 §5 表逐条核对，77 总数守恒（`http_auth` 范围，比对方式按 §8）；② `parse_logout_body` 与 `seed_session_and_csrf` 的跨模块引用被误改为 pub——只允许 `pub(super)`/test-only；③ fixture 里 `DbPool` lazy 连接与 `#[cfg(test)] global_prune_count` 字段在移动中丢失导致行为差异；④ `mod.rs` 门面若用通配 re-export 会让 `pub use *` 隐性扩大路径面，违 C1；⑤ 行号漂移（工作树并行改动）——实施时重采锚点。

DoD：C1 门面逐项 re-export 与 §1.1 清单逐字一致（对照 `lib.rs` 与 grep 结果），可见性变更严格限于 §3/§4.2 所列（清单 10 项 `pub(super)` + 清单外第 1 项 `RateWindowLimiter.entries` test-only 字段项），逐一对应、清单外零升降，rustdoc 链接修复仅限链接指向且 §8 doc 命令零断链；C2 §6 各族门禁顺序逐段核对、diff 审查为零语义差；C3 每测试按 §5 同位、断言零改写、fixture 保持 test-only，B5 后按 §8 重命名映射比对 `http_auth` 范围 77 项一一对应（不以包级总数冒充）；C4 §2 三个 900–1,500 文件的内聚论证成立，无 >1,500 文件；C7 本文与产物无凭据；§8 命令按前提执行并如实报告（含未执行项）；回滚方案逐批可用。

后续事项（不属于本拆分，发现即登记）：无——撰写会话内未发现须先修语义缺陷才能拆分的阻断。

## 10. 修订记录（2026-10-25，独立审查后定点修订）

以下为已完成独立审查的 5 项 Important + 5 项局部 Minor 的定点修订；均为文档修订，未重排架构、未改其他模块任务。各项状态：已修订、待复核（不据此声称审查 PASS）。

1. I1 §3/§5：明确 `MAX_PASSWORD_BYTES` 归属 login.rs、可见性 `pub(super)` 及其依赖边（password_logout → login，仅该常量），可见性变更清单由 9 项更正为实际 10 项；删去「tests_support 引用 `MAX_PASSWORD_BYTES`/`prune_csrf_table`」的错误举例（实测测试区对该常量零引用；prune 测试在 csrf 同模块）。
2. I2 §3：`login_success` 更正为保持私有（仅 `login` 与同模块测试使用），表格与可见性清单不再含之；`login` 为 9 项 handler/复用项中唯一的 router 装配入口。
3. I3 §6：门禁顺序按源码逐段核准，拆为读族两链路、写族两链路与 login 的独立顺序；写族更正为「authenticate → CSRF 绑定校验 → write_limiter」（CSRF 先于限流）；明确 `password`（733）/`logout`（865）无 Task 3f 预检，预检仅 `me`/`sessions_list`/两个 revoke；未借机重排任何步骤。
4. I4 §5：me 测试区间更正为 2,780–3,485（其中 17 个归 me.rs）；`prune_boundary_coverage`（3,487–3,536）登记为 csrf 测试私有 helper，随 prune 测试整组留 csrf.rs，禁入 me.rs/tests_support.rs；调用它的 3,325 测试更正归 csrf.rs（csrf 6→7、me 18→17，合计仍 77）。
5. I5 §2/§7：B1 明确 `http_auth/mod.rs` 形成时未迁实现与测试暂留门面、lib.rs 所需符号全程不丢、子模块经 `use super::…` 访问父项、门面 < 60 行仅 B5 终态；逐批写明临时依赖与回改（B3 sessions 引父 `parse_logout_body`、B5 搬走后回改；B1 仅迁 rate_limit 8 个纯 unit，4 个 oneshot/fixture 测试留暂存位置、B5 迁回，77 守恒）；tests_support.rs 统一为 B5 创建（§5/§7 口径一致）。
6. M1 §4：依赖表补 `login → auth::service`（Login 等）、`password_logout`/`sessions` → `auth::session`（token_digest）、`csrf` → `auth::session`（ABSOLUTE）。
7. M2 §8/§9：77 守恒限定为 `http_auth` 范围；给出 `--lib -- --list` 前后测试名集合的重命名映射比对方式，不以包级总数冒充。
8. M3 §7：B5 的 fixture「去重」限定为只删除临时重复副本、保留唯一份，不合并形似 helper、不改断言。
9. M4 §4/§8/§9：登记 rustdoc `[`prune_dead_csrf`]` 在 270（state.rs）与 958（me.rs）两行的跨模块断链，随搬迁批次只修链接指向；§8 新增 `cargo doc` 断链验证（`-D rustdoc::broken_intra_doc_links`）。
10. M5 §3：明确 `bad_body` 仅被 `login` 调用、`parse_sessions_query` 仅被 `sessions_list` 调用，二者保持私有。
11. §8：`--lib -- --list --locked` 更正为 `--lib --locked -- --list`（Cargo 参数须置于 `--` 前）。已修订，待复核。
12. §7 B2：「仍留在门面的 6 个 handler」更正为 7 个（login/me/password/logout/sessions_list/revoke_others_handler/revoke_by_id）。已修订，待复核。

批次1实施前修订（2026-10-27；来自实施停止报告：entries字段test-only可见性accommodation、文件形态原子替换明确、测试过滤器前缀修正——三项登记后批次1方可实施）：
1. §7 批次 1 行与 §3 表：文件形态明确为“原子替换：http_auth.rs → http_auth/mod.rs（未迁实现与测试整体暂留mod.rs；对照authz/sqlx_repo批次E先例）——同一提交内删旧建新，禁二者并存（E0761）”；替换原“http_auth.rs变为http_auth/mod.rs”的含混表述。
2. §3 表与 §4.2 清单追加：`RateWindowLimiter.entries` 字段登记“#[cfg(test)] pub(super)（仅测试构建；清单外第 1 项）”——理由：me/sessions 等测试跨模块观测/注入 limiter 内部 entries 队列（暂留门面测试 5 处直访 `state.auth.read_limiter.entries`——约 2934/3116/4771/4779/5017 行；终态 me.rs/sessions.rs 同需），生产构建保持 private；同步 §9 DoD 可见性计数口径（清单 10 项 + 该 test-only 字段项）。
3. §8 验收命令：测试过滤器由 `auth::http_auth::` 修正为 `http_auth::`（`http_auth` 是 `lib.rs:13` 顶层 pub mod，不在 `auth::` 命名空间下；77 条测试前缀为 `http_auth::tests::`），相关定向命令全部同步。

批次1实施收窄追记（已修订，2026-10-27，提交99de683；审查PASS）

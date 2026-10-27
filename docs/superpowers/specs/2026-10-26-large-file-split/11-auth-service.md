# 子规格 11：`crates/rsetup-controller/src/auth/service.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 11 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未读取 `secret/`、未连接数据库、未提交。

撰写基线：分支 `dev-aghost`，工作树头提交 `c7ff640`（与 01 号同基线）。行号为撰写时实测，实施时须按当时工作树重新核对（总览 §3 并行改动注记）。

## 1. 实测基线与锚点

对 [crates/rsetup-controller/src/auth/service.rs](../../../../crates/rsetup-controller/src/auth/service.rs)（2,601 行）实测：

- 实现区 1–566 行（566，含末空行），测试区 567–2,601 行（2,035，单一 `#[cfg(test)] mod tests`），与总览 §3 快照「566 / 2,035」一致。
- 测试函数 **50 个**（`#[test]` 0 个 + `#[tokio::test]` 50 个，按属性实测）。fixture/共享 helper 区 568–1,013（446 行），测试用例区 1,014–2,601（1,588 行）。
- 实现区职责（grep 实测，本文件是 auth 域的**会话编排核心**，不含散列/令牌原语——那些在兄弟文件）：登录凭据核验与**登录尝试记录**（`record_login_failure` 钩子：失败计数/审计归仓储侧，HTTP 侧限速属 01 域的 `LoginRateLimiter`，本文件不含）、会话签发/校验/撤销、会话列表游标分页、口令改密编排。**密码散列**（argon2id）在兄弟 [password.rs](../../../../crates/rsetup-controller/src/auth/password.rs)（69 行，不拆），**令牌摘要/时钟/别名 HMAC** 在兄弟 [session.rs](../../../../crates/rsetup-controller/src/auth/session.rs)（457 行，不拆）。
- 实现区锚点（符号 → 行区间）：

| 锚点 | 行区间 | 说明 |
| --- | --- | --- |
| `use crate::{...}` 导入（`password::PasswordHasher`、`session::{SessionAliasKey, SessionClock, token_digest}`） | 1–8 | 仅依赖 `crate::ControllerError`、auth 域两兄弟与 std，无其他出边 |
| `IdentityUser` / `Session` / `Login` / `StoredSession` / `SessionItem` / `SessionPage` | 11–50 | 6 个 `pub` 数据类型（`Login` 无 derive，逐字保留） |
| `trait IdentityRepository` | 52–130 | RPITIT 契约：6 个必需方法（`find_user` 53、`insert_session` 57、`find_session` 63、`change_password` 71、`revoke_session` 77、`record_login_failure` 82）+ 3 个默认方法（`list_user_sessions` 86–96、`revoke_selected_session` 103–114、`revoke_other_sessions` 119–129，默认体固定 `Config("identity session management not wired")`） |
| `AuthService<R>` 结构体 | 131–137 | 5 个私有字段 `repo/clock/alias_key/hasher/dummy_hash` |
| `AuthService::new` | 139–149 | 构造 hasher、dummy 散列（固定串 `rsetup-identity-dummy-verification`，141 行）、时钟、别名密钥 |
| `list_sessions` | 151–340 | 游标分页（132-hex 游标 = alias64B-hex + limit4 + MAC64、8192 上限、行校验/排序/翻页）；函数级局部 `struct Candidate` 243–247 |
| `login` | 341–366 | 未知用户走 dummy 散列等时验证（345）、失败 `record_login_failure`（348–350）、OsRng 32B→64-hex 原始令牌（354–356）、`insert_session` + 时钟插入 |
| `authenticate` / `authenticate_with_now`（私有） | 367–386 | 注入 `now` 的私有校时钟差（测试缝） |
| `session_authority`（含 doc 387–398） | 387–405 | 权威（仓储侧）只读复核，不写时钟 |
| `change_password` | 406–423 | 验证当前口令 → `validate_new_password`（password.rs 37）→ 新散列交仓储 |
| `logout` | 424–428 | 仓储撤销确认后移除时钟项 |
| `is_live_session`（含 doc 429–436） | 429–439 | 纯内存预检，零续期 |
| `revoke_by_alias`（含 doc 440–443） | 440–513 | 按公开别名撤销单会话（own→logout 复用；目标解析→仓储复检→时钟移除） |
| `revoke_others`（含 doc 514–516） | 514–564 | 批量撤销其余会话（数量/容量（539–542）/去重/含当前 四类一致性校验先于任何时钟移除） |

- 兄弟与消费关系（实测）：[auth/mod.rs](../../../../crates/rsetup-controller/src/auth/mod.rs)（4 行，全部 `pub mod` 声明、无再导出，本拆分**不改该文件**）声明 `password/service/session/sqlx_repo` 四个子模块；[auth/sqlx_repo.rs](../../../../crates/rsetup-controller/src/auth/sqlx_repo.rs) 第 100 行 `use crate::auth::service::{IdentityRepository, IdentityUser, Session, StoredSession}` 实现本文件 trait（方向 sqlx_repo → service，反向零依赖）；[http_live.rs](../../../../crates/rsetup-controller/src/http_live.rs) 第 38/74/79 行以 `Arc<AuthService<SqlxIdentityRepository>>` 生产装配 `LiveAuth`，实测调用本文件全部 9 个 `pub` 方法（login/authenticate/change_password/logout/is_live_session/session_authority/list_sessions/revoke_by_alias/revoke_others）；[main.rs](../../../../crates/rsetup-controller/src/main.rs) 第 22/243 行是唯一生产构造点（`AuthService::new` → `LiveAuth::new`）。

### 1.1 既有对外 API 面（C1 逐项，拆分后路径必须逐字不变）

[lib.rs](../../../../crates/rsetup-controller/src/lib.rs) 第 4 行 `pub mod auth;`（无 `pub use auth::...`）；`auth/mod.rs` 第 2 行 `pub mod service;`。全仓（src + tests）经 `crate::auth::service::…` / `rsetup_controller::auth::service::…` 实际导入的符号集合（实测 grep）：

- 类型/契约（全部 `pub`，保持 `pub`）：`IdentityUser`（7 个 pub 字段）、`Session`、`Login`、`StoredSession`、`SessionItem`、`SessionPage`、`IdentityRepository`、`AuthService`（含 `new` 与全部 9 个 pub 方法；5 个结构体字段保持私有）。
- src 消费者（import 行号实测）：`http_auth.rs`(31,1414,5227,5770)、`http_live.rs`(38)、`http_gate.rs`(15,356)、`http_api.rs`(14)、`http_device_status.rs`(60,403)、`api/approvals.rs`(22)、`api/system.rs`(45)、`api/users.rs`(23,502,882)、`api/audit.rs`(26)、`api/groups.rs`(28)、`api/grants.rs`(29)、`api/roles.rs`(27)、`api/devices.rs`(23)、`api/adapters.rs`(21)、`devices/repository.rs`(5,1114)、`devices/service.rs`(3,204)、`tasks/http.rs`(36,1322)、`tasks/adapter.rs`(46)、`authz/service.rs`(8 真实 import、198 tests 的 `{IdentityUser, Session}`)、`authz/sqlx_repo.rs`(5、880、913、1220、1248 共 5 处 `crate::auth::service::Session` 引用)、`auth/sqlx_repo.rs`(100)、`main.rs`(22)；`tasks/service.rs`(366)、`tasks/repository.rs`(579 doc 注释提及 `IdentityRepository`) 为文档注释提及。
- 集成测试消费者：`tests/time_consumption.rs`(30)、`tests/events_sse.rs`(36)、`tests/production_composition.rs`(100)、`tests/support/management_http_fixture.rs`(37)、`tests/recovery_routes.rs`(34)、`tests/sse_business_events.rs`(63)、`tests/sse_time_events.rs`(40)。
- 与 01 号交叉验证（[01-http-auth.md](01-http-auth.md)）：`AuthServiceApi`/`SessionAuthority` 定义在 01 号所辖 [http_auth.rs](../../../../crates/rsetup-controller/src/http_auth.rs) 第 46–115 行（实测），**不在本文件**；01 §1.1 末条与 §4 已登记「认证 service 内部由 11 号负责，本拆分不触碰，只保持单向依赖」，其依赖面即本表的 `Login/Session/SessionPage/IdentityUser` 与 `auth::session::ABSOLUTE/token_digest`、`http_live::LiveAuth`。本设计只保持这些符号在 `crate::auth::service::` 路径下逐字可达，**不要求 01 号规格或实现做任何返工**；实施顺序按总览 §5.2 批次 C（11 → 01 同域串行）。
- 注意区分：[04-authz-sqlx-repo.md](04-authz-sqlx-repo.md) 主域依赖是 `authz::service`（授权域），另存在上述 5 处 `auth::service::Session` 引用（`authz/sqlx_repo.rs` 5、880、913、1220、1248，已在上表登记），拆分后经门面逐字保持、无需返工。

## 2. 目标文件树

纯职责拆分（C1）：`auth/service.rs` → `auth/service/` 私有模块树 + 薄入口 `mod.rs`（`foo.rs` → `foo/mod.rs` + 子文件，总览 C1 允许的标准形态；auth/ 已是目录模块，子文件进 `service/` 子树而非 auth/ 根，避免把 service 私有构件混入 auth 域的兄弟命名空间、稀释 `auth/mod.rs` 的四行清单）。拟建路径（均尚不存在）：

```text
crates/rsetup-controller/src/auth/service/
├── mod.rs           # 薄入口：mod 声明 + AuthService 结构体定义 + new + 逐项 pub use 门面（终态 ≈ 55 行）
├── model.rs         # IdentityUser / Session / Login / StoredSession / SessionItem / SessionPage
├── repository.rs    # trait IdentityRepository（含 3 个默认方法）
├── session_ops.rs   # login / authenticate / authenticate_with_now / session_authority / change_password / logout / is_live_session
├── pagination.rs    # list_sessions（含函数级 Candidate）
├── revoke.rs        # revoke_by_alias / revoke_others
└── tests_support.rs # 仅测试：FakeRepo 家族 + auth_fixture（经 #[cfg(test)] 声明）
```

方法分文件的可行性（结构体定义与 `impl` 块分离）：`AuthService` 定义在 `mod.rs`，各子文件携带本域 `impl<R: IdentityRepository> AuthService<R>` 块——Rust 允许同 crate 内多文件为同一类型追加 impl 块；结构体私有字段定义在 `mod.rs`，对全部后代模块（各子文件及其 `mod tests`）可见，**字段无需任何提升**。`mod.rs` 除 `mod` 声明外不出边引用任何子文件（inherent 方法挂在类型上，无需 import 即全局可达），装配方向天然无环（§4）。

预计成品规模（实现 + 归属测试 + 各自 use 头，含注释空行）：`session_ops.rs` ≈ 470、`pagination.rs` ≈ 560、`revoke.rs` ≈ 990、`repository.rs` ≈ 235、`tests_support.rs` ≈ 350、`model.rs` ≈ 48、`mod.rs` ≈ 55。全部 ≤ 900 或处于 900–1,500 需论证档；无 > 1,500 文件。C4 口径说明：

- `revoke.rs`（≈ 990，900–1,500 档内聚论证）：三个撤销面（自身 `logout` 复用、按别名单选、批量其余）共享同一职责「会话撤销」，共用同一 trait 撤销三方法、同一行校验/8192 上限语义（485 与 251 行同构）与「仓储确认后才移时钟」不变式（508–511、560–562），其 13 个测试（743 行）与 125 行实现强耦合，且独占 fixture（`PollutedListRepo`/`polluted_row_service`/`assert_audit_redacted`）只此一处消费。若再拆 `revoke_by_alias`/`revoke_others` 两文件，独占 helper 被迫升格为跨文件共享、I-2 漂移缝相关用例被切割，无内聚收益；后续若别名域继续膨胀，可按「自身/单选/批量」再拆，本轮不做。
- `session_ops.rs`（470，单文件不再细分）：登录→校验→权威复核→改密→登出→预检六法构成会话生命周期闭环，共享 `hasher/dummy_hash`（login/change_password）与 `clock`（authenticate/logout/is_live_session）两组字段语义，拆开任一法都会让「凭据核验等时性」「撤销后不可续期」两条不变式的测试分居两文件。

## 3. 符号 → 目标模块对应表

顶层符号逐一归属（签名、doc 注释、实现体逐字不变；`use` 头按目标文件实际需要增减，属 C2 允许的「必要 use」）：

| 现符号（`crate::auth::service::` 下） | 目标文件 | 新可见性 | 门面 `mod.rs` 出口 |
| --- | --- | --- | --- |
| `IdentityUser` / `Session` / `Login` / `StoredSession` / `SessionItem` / `SessionPage` | model.rs | `pub` | `pub use` 逐项（6 项） |
| `IdentityRepository`（9 方法签名 + 3 默认体） | repository.rs | `pub` | `pub use` 逐项（1 项） |
| `AuthService`（5 私有字段）+ `new` | mod.rs（留在入口） | `pub` | 定义处即出口 |
| `login` / `authenticate` / `session_authority` / `change_password` / `logout` / `is_live_session` | session_ops.rs | `pub`（inherent，路径不变） | 无需出口（类型在入口） |
| `authenticate_with_now` | session_ops.rs | 私有（现状即私有，测试同文件） | 不出口 |
| `list_sessions` | pagination.rs | `pub`（inherent） | 无需出口 |
| `Candidate`（list_sessions 函数级局部类型，243–247） | 随 `list_sessions` 整体移动 | 函数私有 | 不出口 |
| `revoke_by_alias` / `revoke_others` | revoke.rs | `pub`（inherent） | 无需出口 |

可见性变更清单（C1 口径）：**生产区零新增 `pub`/`pub(crate)`/`pub(super)`**——现有 `pub` 面全部原样保持（inherent 方法按文件分块移动不构成可见性变更：inherent 方法的可见性随类型，而类型定义位置不变，09 先例同口径）；私有项 `authenticate_with_now`、`Candidate`、`AuthService` 的 5 个字段全部保持私有。`pub(super)` 仅出现在 test-only 的 `tests_support.rs`（§5），与既有先例一致：各域文件以私有 `mod` 声明在薄根内，`crate::auth::service::<子模块>::X` 对子树之外本就不可达，故子树内 `pub(super)` 与 `pub(crate)` 的**有效可见性集完全等值**，一律取最小书写 `pub(super)`（08/09 论证）；`pub(super)` 项 MUST NOT 在薄根再导出（09 §4.2）。不使用通配 re-export。

**结构体位置前提（偏离即停批）**：`AuthService` 定义必须留在 `mod.rs`。若把它移入任何子文件，其余子文件变成其**兄弟**而非后代，5 个私有字段将不可见，测试中的 `AuthService { repo, clock, alias_key, hasher, dummy_hash }` 字面量构造（994–1000、1749–1755、1771–1777）与 `svc.clock`/`svc.alias_key`/`svc.dummy_hash` 直读（40+ 处）会迫使字段提权——属设计错误，按 09 风险 R3 同法停批修正。

## 4. 依赖方向与装配（无循环证明）

`use` 方向（单向、自上而下）：

```text
model       → 无（仅 std/chrono 类型）
repository  → model（方法签名引用 6 类型）、crate::ControllerError
session_ops → mod（AuthService 与私有字段）、repository（trait 方法调用）、
              crate::auth::password（本边仅 session_ops 自身：validate_new_password（418 全路径调用）；
              PasswordHasher 类型名 session_ops 不引用——PasswordHasher::new() 实际在 mod.rs::new（140）、
              tests_support:954 与 pagination/revoke 测试（998/1753/1775））、
              crate::auth::session（token_digest）
pagination  → mod、repository、crate::auth::session（SessionAliasKey::alias/sign_cursor 经字段调用）、hex
revoke      → mod、repository（crate::ControllerError 等经 mod 作用域）
mod.rs      → 声明 6 个子模块；pub use model 6 项 + repository 1 项（对子文件零 use 出边；new 依赖
              crate::auth::password/session 与 crate::ControllerError，属域外兄弟边）
tests_support → mod（字段字面量）、repository（为 FakeRepo 实现 trait）、password（auth_fixture 真实散列 954）、
                crate::auth::session（IDLE，FakeRepo::find_session 699 推进时钟需要）
各子模块 ::tests → 被测 mod + tests_support
```

- `tests_support` 的 use 头不追求穷举：除上列出边外其余 `use` 按 §3 总则按实际需要增减；新列 `crate::auth::session::IDLE` 属域外兄弟边（`session.rs`/`password.rs` 对 `auth::service` 零反向依赖），本节无环结论不变。
- 子文件之间**零生产 use 边**：`revoke_by_alias` 复用的 `self.logout` 是同一类型的 inherent 方法（定义在 session_ops.rs），挂在 `AuthService` 类型上，`revoke.rs` 无需 import session_ops；`list_sessions` 的行校验语义与 revoke 共用的是**行为约定**（§6），不是代码边。`mod.rs` 只声明子模块、不引用子文件的任何实现符号，故不存在「父 ⇄ 子」环。
- trait 对象安全边界不动：`AuthServiceApi`/`AuthFuture`/`SessionAuthority` 定义与对象安全证明测试都在 01 号辖区（http_auth.rs，见 01 §5 `test_group_11`）；本文件只提供 `AuthService` 的 9 个 inherent 方法供 `http_live::LiveAuth` 逐个转发（实测 9/9 全量消费），拆分不增删改任何方法签名。
- 域外消费者（`api/`、`devices/`、`tasks/`、`http_*`、`tests/`，§1.1 名单）只 import `crate::auth::service::` 顶层名，经薄入口逐项 `pub use` 后路径逐字不变；`auth/mod.rs` 与 `lib.rs` 零 diff。

## 5. 测试迁移计划（C3）

50 个测试按被测职责归位到各子文件 `#[cfg(test)] mod tests`，**不整搬巨型 `tests.rs`**（总览 C3）；断言与被测行为同时位，测试名逐字不变（模块路径由 `auth::service::tests::X` 变为 `auth::service::<子模块>::tests::X`）。守恒范围仅为 `auth::service` 模块（该包还有 api/、db/、tasks/ 等测试，包级总数不得冒充；比对方式见 §8）。逐组映射（数量合计 50 = 26 + 9 + 2 + 13，名单与计数逐文件相等）：

| 归属文件 | 数量 | 名单（属性行号） |
| --- | --- | --- |
| session_ops.rs（is_live 3 + authenticate 4 + change_password 9 + login 10） | 26 | is_live_session_unknown_digest_is_false(1014)、is_live_session_fresh_login_is_true_and_peek_renews_nothing(1019)、is_live_session_idle_expired_digest_is_false(1038)、slow_lookup_cannot_extend_session_past_old_idle_deadline(1063)、missing_session_does_not_extend_idle_on_recovery(1082)、lookup_error_does_not_extend_idle_on_recovery(1097)、inactive_user_cannot_authenticate_existing_session_or_extend_idle(1112)、logged_out_session_snapshot_cannot_change_password(1126)、separately_revoked_snapshot_cannot_change_password_or_revoke_live_session(1139)、inactive_snapshot_cannot_change_password(1155)、old_process_epoch_snapshot_cannot_change_password(1167)、session_for_different_user_id_cannot_change_password(1179)、wrong_current_password_preserves_account_and_sessions(1191)、password_change_revokes_existing_session(1203)、overflowing_password_revision_preserves_live_session_and_account(1217)、concurrent_password_change_does_not_overwrite_new_hash_or_revoke_new_session(1233)、stale_login_snapshot_with_drifted_revision_never_creates_session(1251)、stale_login_snapshot_with_drifted_hash_never_creates_session(1260)、inactive_snapshot_never_creates_session(1269)、inactive_current_row_rejects_stale_active_snapshot(1278)、inactive_current_row_with_drifted_revision_rejects_stale_active_snapshot(1289)、matching_active_snapshot_creates_session(1301)、unknown_bad_password_and_inactive_user_record_only_anonymous_failures(1312)、unknown_account_really_runs_dummy_argon2_verify(1334)、audit_write_failure_is_not_swallowed_as_invalid_credentials(1343)、successful_login_creates_single_session_and_records_no_failure(1354) |
| pagination.rs（list_sessions 组） | 9 | list_sessions_two_sessions_and_current_marker(1365)、list_sessions_does_not_contain_other_user_sessions(1389)、list_sessions_idle_expiry_without_renewal(1411)、list_sessions_limits_and_pagination(1443)、list_sessions_corrupted_foreign_or_tampered_cursor_is_invalid_argument(1487)、list_sessions_capacity_8192_passes_8193_resource_exhausted(1543)、list_sessions_rejects_inactive_actor_session_digest(1575)、list_sessions_rejects_must_change_password_and_inactive(1588)、list_sessions_rejects_polluted_repo_row(1678，测试体内联 `struct PollutedRepo` 随体移动) |
| repository.rs（trait 默认方法直测：不经 service、直接对仓储对象调 trait 默认方法） | 2 | list_sessions_default_trait_implementation_rejects_with_config(1610，体内联 DefaultRepo)、revoke_default_trait_methods_reject_with_fixed_config(2529，体内联 RevokeDefaultRepo)——被测职责是 `IdentityRepository` 的默认契约而非 service 方法，归属 repository.rs（01 先例：`test_group_11` 归 contract.rs） |
| revoke.rs（revoke_by_alias 10 + revoke_others 3） | 13 | revoke_by_alias_other_session_revokes_only_target_and_counts_one_call(1786)、revoke_by_alias_self_session_logs_out_current_and_leaves_other_live(1835)、revoke_by_alias_unknown_foreign_expired_and_revoked_aliases_are_indistinguishable_not_found(1880)、revoke_by_alias_malformed_alias_is_invalid_argument_without_repo_change(1937)、revoke_by_alias_denies_unqualified_actor_before_any_repository_change(1964)、revoke_by_alias_forged_owner_or_epoch_fails_closed(2011)、revoke_by_alias_concurrent_drift_before_revalidation_is_fixed_not_found(2074)、revoke_by_alias_polluted_row_after_target_fails_closed(2164)、revoke_by_alias_repo_errors_preserve_every_clock_and_claim_no_success(2265)、revoke_others_revokes_all_others_keeps_current_and_repeat_returns_zero(2318)、revoke_others_inconsistent_repo_result_is_fixed_config_with_zero_clock_removals(2365)、revoke_others_retry_after_repository_failure_keeps_current_and_completes(2457)、revoke_by_alias_8193_rows_is_resource_exhausted_before_target_enumeration(2494) |

跨文件私有访问与可达路径（拆分后逐条成立，均为后代可见性，零提权）：

- `authenticate_with_now` 私有校时缝仅被 authenticate 组（1063/1082/1097/1112）调用，随被测方法同留 session_ops.rs 的 tests（同模块私有可达）；若被误提为 `pub(super)` 即偏离设计。
- `AuthService` 私有字段直读/字面量：`svc.clock`（1032/1057/1069/1087/1102/1117/1144/1256 等）、`svc.dummy_hash` 写（1337）、`base.clock`/`base.alias_key`/`base.dummy_hash` 字面量（994–1000、1749–1777）——字段定义在 `mod.rs`，session_ops/pagination/revoke 的 tests 与 tests_support 都是 service 后代，按 Rust 作用域规则直接可见。
- `revoke_by_alias` 组经 `svc.alias_key.alias(...)`（1791/1840/1894/1902/1941/1969/2018/2044/2513 等）与 `svc.clock.epoch` 取别名/epoch，同上为后代可见。
- `list_sessions_rejects_polluted_repo_row` 内联 `PollutedRepo`（1680）与其后的两段 `AuthService { repo, clock: svc_orig.clock, ... }` 部分移动构造（1749–1777）随测试整体进 pagination.rs 的 tests（同为后代，零提权）。

fixture 迁移（test-only，不晚首消费者）：

- 共享 fixture 进 `tests_support.rs`（`#[cfg(test)] mod tests_support;` 声明在薄根，B3 随第一个需要它的测试组同批创建）：`AuthStateSnapshot`(576)、`TestStoredSession`(584)、`RepoFailKind`(593)、`SelectedDriftKind`(611)、`SelectedDrift`(621)、`FakeRepo`(626，含 `auth_state()` 650 与 `impl IdentityRepository` 668–877)、`auth_fixture`(953)。`injected_error`(600) 仅被 FakeRepo 默认失败注入调用（806/852），**保持私有**、不进清单。
- revoke 独占 fixture 随其唯一消费者进 revoke.rs 的 `mod tests`（非共享、不入 tests_support）：`PollutedListRepo`(882–952)、`polluted_row_service`(985–1001，仅 2177/2224 两处调用，同属 2164 测试)、`assert_audit_redacted`(1002–1013，仅 1824/2351 两处调用，均属 revoke 组)。
- `tests_support.rs` 项 `pub(super)` 清单（test-only，23 处书写）：类型/别名 6（`FakeRepo`、`AuthStateSnapshot`、`TestStoredSession`（584，保留 `pub(super)` 系随 `pub(super)` 字段 `state` 的类型签名暴露，测试体对其零命名引用）、`RepoFailKind`、`SelectedDrift`、`SelectedDriftKind`）+ 函数 1（`auth_fixture`）+ 方法 1（`FakeRepo::auth_state`）+ 字段 15（`FakeRepo` 全部 12 字段——测试直接读写 `state/audit/selected_calls/bulk_calls/selected_fail/bulk_fail/bulk_override/lookup_error/lookup_advance/selected_drift` 等，实测跨组；`AuthStateSnapshot.sessions`——唯一字段级读取点 1208，其余 5 字段保持私有仅供整体 `assert_eq!` 比较；`SelectedDrift.digest`/`.kind`——测试构造点 2086）。`FakeRepo` 字面量仅 `auth_fixture` 一处（955），无测试直接构造。各子模块 tests 以 `use super::tests_support::{...}`（session_ops 等）/ `use super::{tests_support, ...}` 显式取用，不为 fixture 新增任何面向生产的接口，不把内联单测升级成 `tests/` 集成测试（C3）。

## 6. 语义保持清单（C2，逐条不得变）

安全不变式（撰写会话按源码逐条核对；拆分只做移动，下列语义要素全部随所属函数逐字走）：

1. **密码散列参数**：本文件不实现散列，仅经 `PasswordHasher`（password.rs：argon2id v0x13、m=65536 KiB、t=3、p=1、OsRng 盐）调用；`new` 的 dummy 口令固定串 `rsetup-identity-dummy-verification`（141）逐字不变，`unknown_account_really_runs_dummy_argon2_verify`(1334) 在 service 层守住该等时性。
2. **未知账号等时验证与不泄露存在性**：`login` 对 `None` 用户以 dummy 散列消耗同量 CPU（345），`!verified || !active` 统一 `InvalidArgument`（347–351）；未知账号失败以 `user_id: None` 记匿名失败（348–350，1312 用例）；`change_password` 当前口令错误同返回 `InvalidArgument`（416）。错误不区分「用户不存在/口令错/未激活」。
3. **令牌熵源与格式**：`rand::rngs::OsRng` 填 32 字节 → 小写 hex 64 字符（354–356），持久层只存 SHA-256 摘要（`token_digest`，357）；原始令牌仅 `Login.raw_token` 一次性返回。
4. **常量时间比较**：游标 MAC 校验为逐字节 XOR 累积比较（218–224）；公开别名匹配经 `SessionAliasKey::matches`（session.rs 内 `constant_time_eq_32`）——本文件只调用，不改实现。
5. **会话 TTL 与轮换时机**：闲置 30 min 滚动续期、绝对 12 h 不续（session.rs 的 `check`），仅 `authenticate` 路径经 `clock.check` 续期（384）；`is_live_session`（437–439）与 `session_authority`（399–405）严格只读零续期（1019/1411 用例分别钉死「peek 不续期」「列表不续期」）；进程 `epoch` 取 uuid v4（session.rs），重启即全量失效。
6. **时间源注入**：私有 `authenticate_with_now(raw, now: impl Fn() -> Instant)`（370–374）是唯二校时缝（另一处在 session.rs 的显式 `now` 参数），测试以 `clock.insert(digest, 过去时刻)` 与手工 `now` 闭包构造过期态，拆分后保持私有且不改变调用语义。
7. **失败计数与锁定**：本文件仅经 `record_login_failure(Option<user_id>)` 把失败事实交给仓储侧（348–350）；audit 写失败直接上抛、不被吞成凭据错误（`audit_write_failure_is_not_swallowed_...` 1343）；HTTP 侧登录限速属 01 域（http_security::LoginRateLimiter），本文件不含计数器，不得借拆分引入。
8. **撤销与游标 fail-closed 门禁顺序**：`list_sessions` 门禁链——actor 资格（active→must_change_password）→ limit 1..=200 → 自身时钟存活 → 游标格式（长度 132/小写 hex/limit 绑定）→ MAC 常量时间校验 → 仓储读取 → 8192 行上限（超限 `ResourceExhausted`）→ 全行 owner/epoch 污染校验（固定 `Config("polluted session row")`）→ 排序/翻页/MAC 绑定 `(user_id, epoch, alias, limit)`（205–209、310–314）；`revoke_by_alias`/`revoke_others` 资格与存活守卫先于任何仓储变更（451–460、518–527），别名格式 64-hex 先于查询（462），跨用户/不存在/已撤销/并发漂移一律收敛 `NotFound`（500、505–507），`revoke_others` 的数量一致/容量（>8192 超限，539–542）/去重/含当前 四类校验全部先于任何 `clock.remove`（534–556），**时钟移除一律在仓储确认 `Ok` 之后**（425–427、508–511、560–562，批量失败保留全部时钟）。
9. **trait 契约**：`IdentityRepository` 三个默认方法的固定文案 `identity session management not wired`、`revoke_selected_session` 复检失败报 `Ok(false)` 不报错、`revoke_other_sessions` 返回 `(count, digests)` 成对语义（97–129 doc 逐字随迁）。
10. **错误与审计面**：全部错误为 `ControllerError` 固定变体、无动态用户输入拼串（9 处固定文案 Config 逐字保留：252/486 `polluted session row`、93/111/126 trait 默认 `identity session management not wired`、535/540/547/553 bulk 四文案 count mismatch/capacity exceeded/duplicate digest/includes current digest）；本文件自身不写日志、不透出原始令牌/散列/别名（审计投影由仓储侧与 01 域负责，`assert_audit_redacted` 在测试侧守边界）。

## 7. 迁移批次与回滚（C8-7）

| 批次 | 内容 | 回滚单位 |
| --- | --- | --- |
| B1 | `service.rs` 变为 `service/mod.rs`（全部实现与测试暂留入口，保持可编译）；抽 `model.rs`（6 类型，纯移动）；入口加 `mod model;` + `pub use model::{…}` 6 项（实施合并：B1+B2 已合并为单提交 487a6f1 一次落位，回滚面为合并后整体，见修订记录） | 单提交 revert |
| B2 | 抽 `repository.rs`（trait 整体 + 2 个默认契约测试随行）；入口 `pub use repository::IdentityRepository;`（实施合并：B1+B2 已合并为单提交 487a6f1 一次落位，回滚面为合并后整体，见修订记录） | 单提交 revert |
| B3 | 创建 `tests_support.rs`（共享 fixture，§5 清单；**不晚于首消费者**）；抽 `session_ops.rs`（6 pub 方法 + 私有 `authenticate_with_now` + 26 测试）；暂留入口的 pagination/revoke 测试新增 `use super::tests_support::{auth_fixture, FakeRepo, RepoFailKind, SelectedDrift, SelectedDriftKind}` 取用共享 fixture | 单提交 revert |
| B4 | 抽 `pagination.rs`（`list_sessions` + 9 测试，`Candidate` 随体） | 单提交 revert |
| B5 | 抽 `revoke.rs`（2 方法 + 13 测试；`PollutedListRepo`/`polluted_row_service`/`assert_audit_redacted` 从入口 tests 迁入其 tests）；删除入口空 tests 模块与 B3 的临时 `use super::tests_support::{...}`；入口收敛为终态（mod 声明 + `AuthService`+`new` + 7 项 `pub use`，≈ 55 行） | 单提交 revert |

各批中间态与回改（每批保持整树可编译，这是规格层面的设计属性，本会话未做实际编译验证）：

- B1/B2：暂留实现经入口作用域消费已迁符号（子项 `pub`，入口 `pub use` 后同名可达；父可直接引用后代 `pub` 项）；入口 tests 以既有 `use super::*` 语义不变。对外路径在 B1 起即为终态出口，B2–B5 对 crate 其余文件零 diff。
- B3：`authenticate_with_now` 与 authenticate 组同文件，缝不丢；session_ops 的 tests 用 `super::tests_support::…`（tests_support 是其兄弟、pub(super) 项对全体后代可见）。
- B4：`list_sessions_rejects_polluted_repo_row` 的两段 `AuthService { clock: svc_orig.clock, .. }` 部分移动构造随体迁入，仍为 service 后代，零提权。
- B5 收口回改点：入口 tests 删除后，B3 的临时 `use super::tests_support::{...}` 一并删除（终态入口不引用任何 test-only 符号）；`tests_support` 的 `#[cfg(test)] mod` 声明保留在入口。
- 每批完成后 50 个测试不增不减不改断言——尚不能归位的测试一律留在入口 tests 暂存（不删除、不重复、不断言弱化），B5 后按 §8 名称映射复核仍为 50。批次边界即回滚点（单提交 revert），不跨批混入其他子规格的改动；与 01 号同域实施须按总览 §5.2 串行（11 全部批次定稿后 01 才实施）。

## 8. 验证命令与前提（总览 §6.2 基线，本轮未执行任何一条）

前提（F2）：`rsetup-controller` 的 build.rs 编译期要求 `apps/controller-web/dist` 在位，直接跑 cargo 前先 `make web-dist-check`（[Makefile](../../../../Makefile) 第 92 行，实测存在；干净环境下可能触发 `npm ci`/vite，总览 F3）。

- `make fmt-check`（Makefile 第 148 行；不得以 `cargo fmt --all --check` 判定，[AGENTS.md](../../../../AGENTS.md) 口径）。
- `cargo clippy -p rsetup-controller --all-targets -- -D warnings`（F2 前提）。
- `cargo test -p rsetup-controller --locked`（F2 前提）。50 的守恒范围是 `auth::service` 模块：拆分前与 B5 完成后各跑一次 `cargo test -p rsetup-controller --lib --locked -- --list`，分别取 `auth::service::` 前缀测试名集合，按「剥除中间子模块名与 `tests::` 段、保留末级测试名」建立重命名映射做双向一一对应比对，两侧均为 50、无孤儿项；包级总数不得冒充。
- rustdoc 零断链核对：本文件唯一跨符号 intra-doc 链接是 `session_authority` doc 内的 `[AuthService::authenticate]`/`[AuthService::is_live_session]`（390/396，拆分后同类型同文件、`AuthService` 在作用域内，预期零断链）；以 `RUSTDOCFLAGS="-D rustdoc::broken_intra_doc_links" cargo doc -p rsetup-controller --no-deps` 复核（F2 前提），只核对链接解析。
- 全量回归出口：`make controller-test-offline`（F2/F3 前提）；§1.1 的集成测试消费者（events_sse/production_composition/recovery_routes/sse_* /support/management_http_fixture 等）由其覆盖，路径面不得变化。
- 真库轨道仅登记（F6，本拆分不运行、不声称通过）：`tests/mysql_identity.rs`、`tests/tidb_identity.rs`（identity schema v4 全链路，`#[ignore]`，操作员逐库授权后以 `cargo test -p rsetup-controller --test mysql_identity -- --ignored` 类命令手工执行，见该文件 230 行注释；其验证目标是仓储/ schema 侧，本文件拆分不触碰 SQL）。本文件 50 个测试全部为 FakeRepo 内存级，不触库。

## 9. 风险与 DoD

风险：① 测试归属核对失准（尤其 1610/2529 两条归 repository 而非 pagination/revoke、2494 归 revoke 而非 pagination、1678 的体内联 `PollutedRepo` 随体走）——以 §5 表逐条点名核对，50 = 26 + 9 + 2 + 13；② `AuthService` 被移出 `mod.rs` 导致字段被迫提权（§3 结构体位置前提，09 R3 同法停批）；③ `tests_support` 的 23 处 `pub(super)` 被误升 `pub(crate)`/`pub` 或在薄根再导出，污染生产可见面；④ `authenticate_with_now` 被误提升可见性（缝外泄）；⑤ 把 `http_live`/`main.rs` 的消费路径当内部依赖改动（应零 diff）；⑥ 与 01 号同域并发实施造成边界双改（总览 §5.2 批次 C 串行）；⑦ 行号漂移（工作树并行改动）——实施时重采锚点。

DoD：C1 出口 7 项 `pub use` 与 §1.1 清单逐字一致、`lib.rs`/`auth/mod.rs` 零 diff、可见性变更清单为「生产零新增 + test-only 23 处 `pub(super)`」且逐项对得上 §3/§5；C2 §6 十条逐条 diff 复核为零（`git diff --color-moved` 纯移动核对，含 9 处固定文案逐字保留（§6.10）与 4 处门禁顺序）；C3 50 个测试按 §5 同位、断言零改写、fixture 保持 test-only 且不晚首消费者，B5 后按 §8 映射 50 项双向一一对应；C4 仅 `revoke.rs` 处于 900–1,500 档且 §2 论证成立、无 > 1,500 文件；C7 本文与产物无凭据、未触库；§8 命令按前提执行并如实报告（含未执行项，无据不声称通过）；每批单提交、`git revert` 可用；不新增 CI/Makefile 目标（F8 链路不触发）、不改任何 `Cargo.toml`/`Cargo.lock`。

后续事项（不属于本拆分，发现即登记）：无——撰写会话内未发现须先修语义缺陷才能拆分的阻断。

## 10. 修订记录（独立审查后定点修订，逐项已修订，待复核，不构成 PASS 结论）

本轮（2026-10-26；上轮审查核心全部成立，修 1 Important + 3 Minor，行号均经当时工作树 read/grep 复核）：

- **I1**（§1.1）：消费面经全量 grep 复核后补登 3 个漏登文件——`authz/service.rs`（8 真实 import、198 tests 的 `{IdentityUser, Session}`）、`authz/sqlx_repo.rs`（5、880、913、1220、1248 共 5 处 `crate::auth::service::Session` 引用）、`tasks/repository.rs`（579 doc 注释提及 `IdentityRepository`）；末条「与 04 无关」表述改为「04 号文件主域依赖是 `authz::service`，另存在上述 5 处 `auth::service::Session` 引用，拆分后经门面逐字保持、无需返工」。补登后 §1.1 全仓（src + tests）「实测 grep 全量」自称成立。
- **M1**（§6）：第 10 条固定文案 Config 由「两处」更正为 9 处逐条枚举（252/486 `polluted session row`、93/111/126 trait 默认 `identity session management not wired`、535/540/547/553 bulk 四文案）；第 8 条 `revoke_others`「三类校验(534–556)」更正为四类（补 539–542 容量校验）。
- **M2**（§5）：`TestStoredSession`(584) 保留 `pub(super)` 书写并补注「随 `pub(super)` 字段 `state` 的类型签名暴露，测试体零命名引用」，23 处计数不变。
- **M3**（§4）：依赖表 `tests_support` 行补 `crate::auth::session::IDLE` 出边（FakeRepo::find_session 699 需要），并注明其余 use 按 §3 总则按实际需要增减；`session_ops → password` 的 `PasswordHasher` 边收敛为仅 session_ops 自身（`validate_new_password` 418 全路径调用），注记 `PasswordHasher::new()` 实际在 tests_support:954 与 pagination/revoke 测试（140 处属入口行既登记边），方向无环结论保持。
- **补修两处残留（已修订，待复核）**：M1 的两处漏网点——§1 锚点表 `revoke_others` 行「三类一致性校验」更正为四类（补容量 539–542，与 §6.8 同口径）；§9 DoD「3 处错误文案」更正为「9 处固定文案逐字保留（§6.10）」（与 §6.10 同口径）。
- 批次1实施登记（已修订，2026-10-27，提交487a6f1；B1+B2合并提交，另model.rs类型间空行为rustfmt规范化——审查M-1备注）。
- 批次2实施登记（已修订，2026-10-27，提交76eb97a；审查PASS——record_login_failure签名rustfmt折叠与tests两级super书写，均非偏离）：1. tests_support.rs:210 record_login_failure签名由4行折叠为1行——缩进层级变化（旧tests模块8空格→新文件顶层4空格）的rustfmt规范化，参数/类型/语义逐字不变，与批次1登记的M-1备注同口径（提交76eb97a，审查PASS）。2. session_ops.rs:114 tests内use super::super::tests_support::auth_fixture两级super书写系模块层级所致，规格§5未固定书写形式，非偏离。
- 批次3实施登记（待审阅，2026-10-27）：按§7 B4抽 pagination.rs（list_sessions + 9 测试，Candidate 随体）；mod.rs 声明 mod pagination 并移除迁出的 list_sessions 实现与 9 条测试；tests 内 use super::super::tests_support::auth_fixture 按模块层级书写。50 条测试全过（分布：repository 2 + session_ops 26 + pagination 9 + revoke 13）。
- 批次4实施登记（2026-10-27，本提交；revoke域迁出与薄根收敛完成，11模块实施闭环）：按§7 B5新建 auth/service/revoke.rs，迁入 revoke_by_alias 与 revoke_others 2方法 + 13条测试 + 3项独占fixture（PollutedListRepo/polluted_row_service/assert_audit_redacted）；mod.rs 收敛为 §3 终态薄根（全量 mod 声明、AuthService 结构体定义 + new、7 项 pub use 门面、无实现体与 tests 残留，41 行）；tests_support.rs 补入 Instant 引用以配合薄根未再使用 Instant。全量 50 条测试全过（终态分布：repository 2 + session_ops 26 + pagination 9 + revoke 13 + 薄根 0），11-auth-service 实施闭环。
- 批次4实施登记（已修订，2026-10-27，提交cb0e0de；审查PASS——revoke.rs两处格式备注：方法间空行与显式类型标注，均非偏离）：1. revoke.rs:79两方法间新增一个空行——纯格式非逐字，无语义影响（与批次1 model.rs空行备注同口径）。2. revoke.rs:195 PollutedListRepo::list_user_sessions委托行新增显式类型标注Vec<StoredSession>——与推断一致、断言零变化，非断言改写（同文件其余7处委托未加标注，类型推断非必需）。本批次4完成后11模块实施闭环（批次1–4）。

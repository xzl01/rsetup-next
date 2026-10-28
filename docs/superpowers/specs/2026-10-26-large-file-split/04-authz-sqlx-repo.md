# 子规格 04：`crates/rsetup-controller/src/authz/sqlx_repo.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 04 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未读 `secret/`、未连接数据库、未提交。
>
> **修订记录（2026-10-25）：已按独立审查意见修订、待复核（不称 PASS）。** 修订项：方法计数更正为 21（12 读 + 9 写，§1.2/4.1/5/8/9）；SQL 常量更正为 22 条并在 §1.2 附全名清单供 DoD 核对（§1.2/2/3/4.2）；可见性提升收敛为 5 项、`uuid_from`/`device_from` 保持 private（§3/4.2/8 DoD）；`authz/mod.rs` 锚点更正为第 3 行（§1.1/3）；集成测试属独立 crate、只消费 `pub` 项的说明（§4.2）；依赖图补 `tx_guard`→`codec`（`ordered_user_ids` 378）与 `snapshot_repo`→`crate::authz::service`（1321/1327）互引注记（§4.3）；批 C `queries.rs::tests` 显式 `use crate::authz::sqlx_repo::sql_shape;` 接线（§5/6）。仅文档级修改，27 测试映射、单一 impl、SQL/事务不变量与批次骨架不变。

撰写基线：分支 `dev-aghost`，工作树头提交 `c7ff640`；实测 `git status` 当时 [authz/](../../../../crates/rsetup-controller/src/authz/) 目录无未提交改动。行号为撰写会话对 4,097 行文件逐段实测，实施时须按当时工作树重新核对（总览 §3 并行改动注记）。

## 1. 实测现状与锚点

### 1.1 体量、公开面与消费方

- 全文件 4,097 行：实现区 1–2,993（约 2,992 行有效实现），测试区 2,994–4,097（`#[cfg(test)] pub(crate) mod sql_shape` 2,994–3,135 fixture + `#[cfg(test)] mod tests` 3,136–4,097，27 个 `#[test]`，全同步、无 async、不触库）。与总览 §3 快照「约 2,992 / 1,105」一致。
- 模块声明：[authz/mod.rs](../../../../crates/rsetup-controller/src/authz/mod.rs) 第 3 行 `pub mod sqlx_repo;`（`pub`），第 15–18 行选择性再导出恰 7 个名字：`AuthorizationSnapshot`、`AuthorizationSnapshotRepository`、`AuthzRepository`、`RepoFuture`、`SqlxAuthorizationSnapshotRepository`、`SqlxAuthzRepository`、`UserRecord`。拆分前后 mod.rs 该清单与 `pub mod sqlx_repo;` 声明逐字不变（C1）。
- 模块外直接经 `authz::sqlx_repo::` 路径消费（实测 17 个文件，不含上条 mod.rs 的再导出；逐一登记，拆分后全部路径必须原样可用）：
  - 同域：[authz/service.rs](../../../../crates/rsetup-controller/src/authz/service.rs)（13、202 行 import：两个 trait、快照与记录类型）。
  - 数据访问方：[devices/repository.rs](../../../../crates/rsetup-controller/src/devices/repository.rs)（7–10 import；389/483/610 `verify_management_read`；410/688 `decode_device_admin_view`；702 `validate_display_name`；723/842 `bump_authz_epoch`；925/974/1008/1052 `sql_shape` 形状断言）、[integrity.rs](../../../../crates/rsetup-controller/src/integrity.rs)（1429/1455 `sql_shape`）。
  - 任务与设备状态域：[tasks/service.rs](../../../../crates/rsetup-controller/src/tasks/service.rs)（30、2468）、[tasks/scheduler_store.rs](../../../../crates/rsetup-controller/src/tasks/scheduler_store.rs)（21 `load_snapshot_in_tx`）、[tasks/adapter.rs](../../../../crates/rsetup-controller/src/tasks/adapter.rs)（48）、[http_device_status.rs](../../../../crates/rsetup-controller/src/http_device_status.rs)（63、406）。
  - API/HTTP 装配层：[http_api.rs](../../../../crates/rsetup-controller/src/http_api.rs)（15）、[admin_cli.rs](../../../../crates/rsetup-controller/src/admin_cli.rs)（545 `bump_authz_epoch`）、[api/groups.rs](../../../../crates/rsetup-controller/src/api/groups.rs)（29 `validate_group_name`）、[api/mod.rs](../../../../crates/rsetup-controller/src/api/mod.rs)（45 `RepoFuture`、844）、[api/devices.rs](../../../../crates/rsetup-controller/src/api/devices.rs)（25 `validate_display_name`）、[api/adapters.rs](../../../../crates/rsetup-controller/src/api/adapters.rs)（24）、controller [main.rs](../../../../crates/rsetup-controller/src/main.rs)（24）。
  - 集成测试（只作验证依据，总览 C6）：`tests/time_consumption.rs`（31）、`tests/production_composition.rs`（102）、`tests/transport_admission.rs`（452）。
  - 注意：`crates/rsetup-controller/src/auth/sqlx_repo.rs` 是**另一个模块**（身份仓储），同名不同物、与本文件无关、不在本规格范围。

### 1.2 实现区职责锚点（实测行号区间）

- **头部与记录类型** 1–56：`RepoFuture`（15）、`UserRecord`（19）、`GroupRecord`（30）、`GrantColumns`（39）、`AuthorizationSnapshot`（49）。
- **trait 定义** 58–213：`AuthzRepository`（58–185，21 个方法签名：12 读 + 9 写；121–122 行注释钉死写侧统一事务骨架「guard → 行锁顺序 → CAS → LastAdmin → 审计同事务插入 → commit；任一错误 drop 局部 tx 自动回滚」）；`AuthorizationSnapshotRepository`（187–213，3 方法，`load_admin_target_page` 带 fail-closed 默认实现 203–212）。
- **两仓储结构** 215–233：`SqlxAuthzRepository`（215，字段 `db: DbPool` `pub(crate)`）与 `SqlxAuthorizationSnapshotRepository`（225）各带 `new`。
- **校验与小工具** 235–334：`read_error`（235，固定去敏 Config 词表入口）、`decode_user`（239）、`page_cursor`（268，恰好一行多余才出 cursor）、`validate_display_name`（276）、`validate_group_name`（286）、`ordered_user_ids`（297，UUID 字节序锁序）、`admin_change_removes_admin_state`（307，LastAdmin 触发条件）、`validate_role_restore`（320，私有）。
- **管理写事务骨架** 336–423：`type UserRow`（336，私有别名）、`select_user_for_update`（338，**内联 SQL 字面量** 344，不常量化、原样走）、`begin_guard`（365，begin 后立刻 `integrity::lock_integrity_guard` 行锁）、`lock_user_pair_and_verify_actor`（373，按 `ordered_user_ids` 锁 actor/target 并用锁定行重验 actor）、`bump_authz_epoch`（401，checked CAS）。
- **grant 编解码** 426–550：`grant_columns`（426，source/scope 成对 + `validate_grant_fields`）、`decode_grant`（473，污染统一换固定去敏）、`uuid_from`/`device_from`/`uuid16`（532/540/547，私有）。
- **快照解码层** 552–797：`decode_snapshot_actor`（552）、`decode_snapshot_roles`（571，重复/dangling/active 空权限 fail-closed）、`decode_snapshot_group_rows`（616）、`decode_group_memberships_for_device`（642）、`decode_device_admin_view`（669，`descriptor_json` 入参 `Option<String>` CHAR 文本契约）、`build_authorization_snapshot`（716）、`validate_snapshot_grant_references`（755）、`require_snapshot_isolation`（781，仅 REPEATABLE-READ 放行）、`decode_snapshot_meta`（789，singleton+版本+u64 epoch）。
- **SQL 常量区** 799–861：22 条 `pub(crate)` SQL 常量，逐条全名如下（DoD 逐项核对清单；SQL 本体逐字不改）：`ACTOR_READONLY_SQL`、`SNAPSHOT_META_SQL`、`LIST_ACTOR_GRANTS_SQL`、`ROLES_BY_IDS_SQL_TEMPLATE`、`ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE`、`GROUPS_FOR_DEVICES_SQL_TEMPLATE`、`DEVICES_BY_IDS_SQL_TEMPLATE`（四条 `*_SQL_TEMPLATE` 的 `{placeholders}` 由调用侧 1:1 展开）、`LIST_USERS_SQL`、`LIST_USERS_FIRST_SQL`、`LIST_ROLES_SQL`、`LIST_ROLES_FIRST_SQL`、`LIST_GRANTS_SQL`、`LIST_GRANTS_FOR_USER_SQL`、`LIST_GROUPS_SQL`、`LIST_GROUPS_FIRST_SQL`、`FIND_USER_SQL`、`FIND_ROLE_SQL`、`FIND_GRANT_SQL`、`FIND_GROUP_SQL`、`LIST_GROUP_DEVICE_IDS_SQL`、`DEVICE_GROUPS_FOR_DEVICE_SQL`、`ADMIN_TARGET_DEVICE_KEYS_SQL`。
- **读事务守卫与快照装载** 863–1226：`acquire_authorization_read_connection`（863，独立连接 + `@@session.transaction_isolation` 校验）、`ensure_actor_admin_readonly`（878）、`verify_management_read`（911，meta 校验 + actor 校验）、`ActorAuthorization`（937）、`placeholders`（944，私有）、`load_actor_authorization_in_tx`（948，单事务内 actor→meta→grants→roles→perms 固定读序）、`load_device_snapshot_in_tx`（1121，groups→devices）、`load_snapshot_in_tx`（1201）、`load_admin_target_snapshot_in_tx`（1218）。
- **`impl AuthorizationSnapshotRepository for SqlxAuthorizationSnapshotRepository`** 1228–1359：3 方法体（load_snapshot/load_admin_target_snapshot 各自开专用读事务并显式 commit；load_admin_target_page 1265–1358 同事务 keyset 扫描 + 单快照投影，cursor 双绑定 1298–1299）。
- **巨型 `impl AuthzRepository for SqlxAuthzRepository`** 1361–2993：21 方法逐个实现，两块不可分割（§4.1）。
  - 读方法（1362–2137，12 个：`find_user` 1362、`list_users` 1402、`find_role` 1466、`list_roles` 1526、`find_grant` 1623、`list_grants` 1693、`list_grants_for_user` 1782、`roles_by_id` 1850、`device_groups_for_device` 1944、`find_group` 1985、`list_groups` 2035、`list_group_device_ids` 2105）统一形状「acquire 专用连接 → begin → `verify_management_read` → 消费被钉 SQL 常量 → 解码 → 显式 commit」。
  - 写方法（2139–2993，9 个：`create_user` 2139、`patch_user` 2214、`reset_user_password` 2347、`create_role` 2419、`patch_role` 2492、`create_grant` 2640、`delete_grant` 2773、`create_group` 2822、`patch_group` 2879）统一形状「事务前门禁 → `begin_guard` → actor/行锁 → CAS revision → LastAdmin → `bump_authz_epoch` → 审计同事务插入 → commit；错误路径 drop tx 回滚」，内联 DML 字面量与唯一冲突映射（409/503 去敏）逐方法内嵌。

### 1.3 测试区与 sql_shape fixture（实测）

- **`sql_shape` fixture**（2994–3,135，`#[cfg(test)] pub(crate) mod`，5 函数：`normalize_sql`、`production_source`（以 `#[cfg(test)]` 分界截生产段）、`split_select`、`select_string_literals`、`assert_device_descriptor_text_projection`）。它是**跨模块测试设施**：`devices/repository.rs` 内 4 处测试（925/974/1008/1052）与 `integrity.rs` 内 2 处测试（1429/1455）经 `crate::authz::sqlx_repo::sql_shape` 路径使用。拆分后该模块路径 MUST 逐字保持。
- **`mod tests`**（3,136–4,097，私有，`use super::*;` + `crate::authz::policy::{GrantScope, GrantSource, Permission}` + **跨模块常量** `crate::devices::repository::{APPROVAL_PAGE_SQL, AUDIT_PAGE_SQL, DEVICE_FILTER_KEYS_SQL}`）：27 个 `#[test]`，逐条映射见 §5。其中 `device_snapshot_read_consumes_pinned_template_without_inline_copy`（3308）以 `include_str!("sqlx_repo.rs")` 自扫描本文件生产段，是对文件位置敏感的测试（§8 风险 2）。

## 2. SQL、事务与锁不变量（逐字保持，MUST NOT 借拆分改写）

1. **SQL 字节与绑定**：22 条常量逐字符原样（全名清单见 §1.2；含 `\` 续行与空白）；绑定顺序敏感处——`load_admin_target_page` 的 cursor 两次绑定 + `limit+1`、四条 `*_SQL_TEMPLATE` 的 `{placeholders}` 按去重 id 数 1:1 展开不拼字面量、`ADMIN_TARGET_DEVICE_KEYS_SQL` 3 绑定。绑定数/形状由 §5 的 3 个 queries 测试离线钉死。
2. **事务 guard 与 borrow lifetime**：写事务一律经 `begin_guard`（begin 后立即 integrity guard 行锁，`Transaction<'_, sqlx::MySql>` 以 `&mut *tx` 可变借用贯穿 guard→行锁→CAS→epoch→审计→commit）；快照/管理读事务经 `acquire_authorization_read_connection`（隔离级非 REPEATABLE-READ fail-closed）。拆分只是文本搬移，MUST NOT 拆事务、合事务、改 commit/rollback 位置（错误路径依赖 drop-tx 隐式回滚）。
3. **锁次序**：users 行锁按 `ordered_user_ids` 的 UUID 字节序（3,136 后测试锁定）；写侧 LastAdmin 判定 `admin_change_removes_admin_state` + `policy::check_admin_demotion_allowed` 顺序不变。
4. **epoch bump**：`bump_authz_epoch` 的 checked CAS（读-加一-条件 UPDATE-行数校验）为所有管理写提交前最后一步之一，位置与失败语义（`RevisionConflict`）不变；它是 `admin_cli`/`devices` 复用的 `pub(crate)` 入口。
5. **快照读一致性**：`load_snapshot_in_tx` 在同一事务内依序装载 actor→meta→grants→roles/perms→groups→devices，epoch 取自 `decode_snapshot_meta` 的 singleton 校验；`load_admin_target_page` 复用同一 `load_snapshot_in_tx`，不另开第二套快照。
6. **fail-closed 去敏**：持久污染一律 `read_error` 固定 `Config`（503 形状），行缺失/权限不足 `PermissionDenied`/`NotFound` 区分不变；解码函数入参形状（如 `descriptor_json: Option<String>`）不变。

## 3. 目标私有模块树（拟建，未创建）

```text
crates/rsetup-controller/src/authz/sqlx_repo/        # 由 sqlx_repo.rs 目录化；authz/mod.rs 第 3 行不变
  mod.rs              约 90   # mod 声明 + §1.1 全部对外路径的再导出（7 个 pub use + pub(crate) use + cfg(test) sql_shape 声明）
  model.rs            约 60   # 15–56：RepoFuture + 4 记录类型
  traits.rs           约 170  # 58–213：两个 trait 全签名（纯移动）
  codec.rs            约 490  # 235–334 + 426–550：校验/cursor + user/grant 编解码（uuid_from/device_from 仅 decode_grant 内部调用，保持 private）+ 9 测试
  snapshot_codec.rs   约 880  # 552–797：快照解码 9 函数 + 14 测试（内聚论证见 §9）
  queries.rs          约 190  # 799–861：22 条 SQL 常量 + 3 个绑定形状测试
  tx_guard.rs         约 100  # 336–423 + 944–946：UserRow/select_user_for_update/begin_guard/lock_user_pair/bump_authz_epoch/placeholders
  snapshot_read.rs    约 400  # 863–942 + 948–1226：读守卫 + ActorAuthorization + load_*_in_tx ×4 + 1 测试
  snapshot_repo.rs    约 150  # 225–233 + 1228–1359：快照仓储结构 + 唯一 trait impl
  authz_repo.rs       约 1650 # 215–223 + 1361–2993：仓储结构 + 唯一巨型 trait impl（C4 例外见 §9）
  sql_shape.rs        约 145  # 2994–3135：#[cfg(test)] fixture，路径 crate::authz::sqlx_repo::sql_shape 不变
```

不新增 crate、不改 `Cargo.toml`/`Cargo.lock`、不增删依赖；无新包，[Makefile](../../../../Makefile) `FMT_PACKAGES` 不动（总览 F1）。

## 4. 符号归属与单一 trait impl 设计

### 4.1 trait impl 唯一落点（不假设 Rust 允许分散实现）

Rust 中一个类型对一个 trait 的实现**只能有一个 `impl Trait for Type` 语法块**，块本身不可跨文件/跨模块分割；「同一 trait 的 impl 拆到多个文件各写一段」不合法，本方案不依赖它。两条可选路径的取舍：

- **窄 helper 委托**（impl 留在一处、方法体转发到窄 helper 文件）：可行但需把 21 个方法体逐一改写成对新函数的转发（动返回表达式与 `Box::pin` 边界、动 `&self` 借用形状），属业务改动，违反 C2「纯移动」，本轮拒绝；MUST NOT 借委托顺手合并/重排业务方法。
- **单一 impl 块整体移动**（本设计采纳）：`impl AuthzRepository`（21 方法逐字）整体进 `authz_repo.rs`；`impl AuthorizationSnapshotRepository`（3 方法逐字）整体进 `snapshot_repo.rs`。代价是 `authz_repo.rs` 超阈值，以 §9 内聚论证 + 后续再拆方案覆盖。

### 4.2 可见性设计（默认 private，逐项清单）

- **保持 `pub`**（随文件迁入子模块，仍声明 `pub`）：7 个再导出名 + `GroupRecord`、`GrantColumns`、`validate_display_name`、`decode_user`、`page_cursor`、`grant_columns`、`decode_grant`、`decode_snapshot_actor`、`decode_snapshot_roles`、`decode_snapshot_group_rows`、`decode_group_memberships_for_device`、`decode_device_admin_view`、`build_authorization_snapshot`、`validate_snapshot_grant_references`、`require_snapshot_isolation`、`decode_snapshot_meta`。`mod.rs` 以 `pub use` 原样透出，`crate::authz::sqlx_repo::X` 全路径不变。
- **保持 `pub(crate)`**：`read_error`、`validate_group_name`、`ordered_user_ids`、`admin_change_removes_admin_state`、`bump_authz_epoch`、22 条 SQL 常量、`ActorAuthorization`、`acquire_authorization_read_connection`、`ensure_actor_admin_readonly`、`verify_management_read`、4 个 `load_*_in_tx`、`sql_shape`。其中跨文件消费者（devices/service/api/main，见 §1.1）全部经 `mod.rs` 的 `pub(crate) use` 回到原路径；集成测试属独立 crate，无法消费 `pub(crate)` 项——`tests/` 下各文件只消费 `pub` 项，其 `crate::authz::sqlx_repo::X` 原路径由 `mod.rs` 的 `pub use` 保证不变。
- **由 private 升为 `pub(super)`**（仅因跨文件访问，共 5 个，全部无对外路径）：`begin_guard`、`lock_user_pair_and_verify_actor`、`validate_role_restore`（被 `authz_repo.rs` 调用）；`placeholders`（被 `snapshot_read.rs`/`authz_repo.rs` 调用）；`uuid16`（被 `snapshot_codec.rs` 调用）。
- **保持 private**：`UserRow`、`select_user_for_update`（均只在 `tx_guard.rs` 内使用）；`uuid_from`、`device_from`（实测仅被 `codec.rs` 内 `decode_grant` 调用，不提升）。
- 各子模块自身默认 private（`mod xxx;`），对外只经 `mod.rs` 透出；无 `pub` 子模块。

### 4.3 依赖方向（无环）

`model` ← `traits`；`codec`、`queries` 不依赖兄弟；`snapshot_codec` → `codec`（uuid16/read_error）；`tx_guard` → `codec`（decode_user 350、ordered_user_ids 378）；`snapshot_read` → `codec`/`snapshot_codec`/`queries`/`tx_guard`；`snapshot_repo` → `traits`/`model`/`snapshot_read`；`authz_repo` → 以上全部；`mod.rs` 汇总。单向、无回边、不成环。另有一处既有 crate 互引（非本次拆分新增、不构成树内部环）：`snapshot_repo` 的 `load_admin_target_page` 调用 `crate::authz::service::snapshot_effective_permissions`/`snapshot_permission_sources`（源码 1321/1327），方向为跨出本模块树，`service` 侧消费仍只经 `mod.rs` 透出路径。

## 5. 测试与 fixture 迁移映射（27 → 27，逐条，不整块移巨型 tests.rs）

断言与被测行为同时位；`use super::*` 语义由目标文件自身作用域承接；policy/devices 引用原样保留（devices 三常量 `APPROVAL_PAGE_SQL`/`AUDIT_PAGE_SQL`/`DEVICE_FILTER_KEYS_SQL` 保持 `pub(crate)` 于原处，不搬迁）。

- **`codec.rs::tests`（9 个，被测符号均在 codec.rs）**：
  - `user_decode_fails_closed_on_polluted_boolean_and_username`(3143) → `decode_user`
  - `page_cursor_reports_exactly_one_extra_row_and_never_leaks_total`(3166) → `page_cursor`
  - `grant_columns_pair_source_with_scope_strictly`(3179) → `grant_columns`
  - `decode_grant_maps_persistent_field_pollution_to_fixed_config`(3585) → `decode_grant`
  - `user_lock_order_is_deterministic_by_uuid_bytes`(4023) → `ordered_user_ids`
  - `only_removing_active_admin_state_triggers_last_admin_rule`(4033) → `admin_change_removes_admin_state`
  - `permission_list_must_be_non_empty_and_unique`(4046) → 被测函数在 `policy` 域，但它是本模块校验器族（`validate_role_restore`）的直接前置门禁，按「与校验器同位」归此，MUST NOT 借此搬进 policy（那是 01/11 的边界）
  - `role_restore_from_archive_requires_valid_non_empty_permissions`(4056) → `validate_role_restore`
  - `validate_group_name_boundaries`(4074) → `validate_group_name`
- **`snapshot_codec.rs::tests`（14 个，被测符号均在 snapshot_codec.rs）**：
  - `descriptor_json_decoder_takes_char_text_and_fails_closed_on_malformed`(3335) → `decode_device_admin_view` 入参形状
  - `snapshot_actor_decode_fails_closed_on_polluted_boolean_and_missing_row`(3418)
  - `snapshot_roles_decode_fails_closed_on_duplicate_and_empty_permissions`(3437)
  - `snapshot_group_rows_decode_dangling_member_and_filter_archived`(3481)
  - `snapshot_device_decode_fails_closed_on_polluted_state_and_boolean`(3507)
  - `snapshot_build_fails_closed_on_missing_role_reference`(3554)
  - `snapshot_build_fails_closed_on_active_role_empty_or_duplicate_permissions`(3680)
  - `snapshot_build_fails_closed_on_archived_role_duplicate_permissions`(3750)
  - `group_memberships_for_device_fails_closed_on_archived_or_missing_device`(3819)
  - `snapshot_build_passes_and_preserves_actor_grants_verbatim`(3860)
  - `archived_role_empty_permissions_are_legal_but_active_empty_is_corruption`(3908)
  - `snapshot_grant_reference_flags_fail_closed_and_direct_all_is_legal`(3964)
  - `snapshot_meta_rejects_extra_wrong_or_unknown_rows_and_preserves_u64_epoch`(3987)
  - `snapshot_requires_verified_repeatable_read_configuration`(4011) → `require_snapshot_isolation`
- **`queries.rs::tests`（3 个）**：
  - `list_sql_binds_are_pinned_to_cursor_and_limit`(3205，含 devices `AUDIT_PAGE_SQL` 等跨模块常量断言)
  - `management_page_filter_sql_binds_are_pinned_to_seven_five_three`(3253，含 devices `DEVICE_FILTER_KEYS_SQL`/`APPROVAL_PAGE_SQL`)
  - `devices_by_ids_sql_template_projects_descriptor_json_as_char_text`(3282，经 `sql_shape::assert_device_descriptor_text_projection`(3283)——拆分后 `queries.rs::tests` 须显式 `use crate::authz::sqlx_repo::sql_shape;`，断言文本逐字不改，接线见 §6 批 C)
- **`snapshot_read.rs::tests`（1 个）**：
  - `device_snapshot_read_consumes_pinned_template_without_inline_copy`(3308)：`include_str!("sqlx_repo.rs")` 改为 `include_str!("snapshot_read.rs")`（扫描对象 = `load_device_snapshot_in_tx` 所在文件生产段，三条断言——消费模板常量、按列名取 `descriptor_json`、生产段无内联副本——在目标文件均逐字成立；模板常量本体在 `queries.rs` 不在扫描范围，等价性由 §8 风险 2 核对）
- **`sql_shape.rs`**：fixture 本体 5 函数原样，路径保持 `crate::authz::sqlx_repo::sql_shape`（`mod.rs` 内 `#[cfg(test)] pub(crate) mod sql_shape;`），`devices/repository.rs` 4 处、`integrity.rs` 2 处调用点零改动。
- 巨型 `authz_repo.rs` 无内联测试可迁（21 方法体无测试）；其形状防线由上述 27 个测试与真库轨道（§7）承担。**MUST NOT** 把 27 测试整块塞进独立 `tests.rs`（总览 C3）。

## 6. 分批顺序与回滚（每批设计为可编译，本轮未编译验证）

各批 = 纯移动 + `mod`/`use`/再导出接线，单提交单批，逐批可独立回滚（`git revert` 该提交即回到上一批形态；无行为开关）。每批完成后的临时引用点在下批回改，登记如下：

1. **批 A（fixture 先走）**：`sqlx_repo.rs` → `sqlx_repo/mod.rs`，抽出 `sql_shape.rs`（模块路径不变）。
   暂留：其余全部仍在 mod.rs，无任何调用点改动。
2. **批 B（类型层）**：抽 `model.rs`、`traits.rs`；`mod.rs` 加 `pub use`/`pub(crate) use` 透出。
   暂留点：mod.rs 内实现体经再导出（`use` 引入本作用域）解析，不改调用处。
3. **批 C（编解码与 SQL 常量）**：抽 `codec.rs`、`snapshot_codec.rs`、`queries.rs` 及各自 9/14/3 测试同批迁移。
   暂留点：mod.rs 剩余实现与剩余测试（批 D 的 1 个）继续经 mod.rs 再导出 + `use super::*` 解析；`use crate::devices::repository::{…}` 三常量引用随 `queries.rs::tests` 原样携带。接线补记：原 `mod tests` 经 `use super::*` 访问 `sql_shape`，拆到 `queries.rs::tests` 后该 glob 不再覆盖，须显式加 `use crate::authz::sqlx_repo::sql_shape;`（3283 行调用点，断言文本逐字不改）。
4. **批 D（事务骨架与快照装载）**：抽 `tx_guard.rs`、`snapshot_read.rs`、`snapshot_repo.rs` 及其 1 测试（`include_str!` 同批改指 `snapshot_read.rs`）。
   暂留点：mod.rs 只剩 `SqlxAuthzRepository` + 巨型 impl，其调用符号按各自可见性在 mod.rs 作用域内 `use` 引入（`verify_management_read` 等 `pub(crate)` 项走再导出，`begin_guard`/`placeholders` 等 `pub(super)` 项只作本作用域 `use`、不再导出）；`sql_shape` fixture 已全部就位、无暂留。
5. **批 E（巨型 impl 收尾）**：抽 `authz_repo.rs`（结构体 + 唯一 impl 块逐字），mod.rs 收敛为最终约 90 行（§3）。
   回改点：清理中间批引入的临时 glob，仅保留 §4.2 最终再导出清单；此后无暂留实现。

## 7. 验证命令（登记项，本轮一律未执行、不声称通过）

前提（总览 F2）：`rsetup-controller` 编译期要求 web dist 在位，先 `make web-dist-check`（或 `make web-build`）；F4：clippy/test 均编译。命令（真实包名 `rsetup-controller`；`--locked` 置于 `--` 之前）：

1. `make fmt-check`（F1 口径，不用 `cargo fmt --all`）。
2. `cargo clippy -p rsetup-controller --all-targets -- -D warnings`。
3. `cargo test -p rsetup-controller --locked`（全量）；定向回归 `cargo test -p rsetup-controller --locked -- authz::sqlx_repo`。拆分后用例全名由 `authz::sqlx_repo::tests::X` 变为 `authz::sqlx_repo::{codec|snapshot_codec|queries|snapshot_read}::tests::X`，按 §5 清单逐一对照，总数 27 不减、名称不变。
4. 跨模块 fixture 回归：`cargo test -p rsetup-controller --locked -- devices::repository::` 与 `-- integrity::`（覆盖 `sql_shape` 6 处调用点与 devices SQL 形状断言）。
5. 真 MySQL/TiDB 轨道（总览 F6，**仅登记**：需操作员按引擎逐条授权专用开发库，本轮未运行、结果未验证，禁止未授权执行）：`cargo test -p rsetup-controller --locked --test mysql_identity -- --ignored`、`--test tidb_identity -- --ignored`，以及 `task_scheduler_sqlx`/`metrics_real_db`/`v4_schema_readonly_verify` 同形命令。

## 8. 模块专属风险与 DoD

- **风险 1（阈值）**：`authz_repo.rs` 约 1,650 行超阈值，C4 例外论证见 §9；除此之外任何「再瘦一点」的诱惑（合并方法、提取公共查询）都违反 C2，拒绝。
- **风险 2（位置敏感测试）**：3308 的 `include_str!` 自扫描若误指向 `mod.rs` 或 `queries.rs` 会静默改变门禁语义（恒真/恒假）；实施批 D 须核对三条断言在新文件逐字成立，且新文件生产段确含 `DEVICES_BY_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph)` 与 `r.try_get("descriptor_json")`。
- **风险 3（跨模块契约）**：`sql_shape` 路径与 devices 三常量、integrity 两条 SQL 形状断言构成跨模块回归网；任何再导出遗漏都会编译失败（可接受），任何可见性提升越界则由 §4.2 清单审查拦截。
- **风险 4（fixture 泄漏）**：`sql_shape` 必须保持 `#[cfg(test)]`，MUST NOT 为便于跨模块引用而进入非 test 编译面。
- **风险 5（行为面）**：本模块被 devices/tasks/http_device_status/api 广泛复用（§1.1），任何路径漂移在批间接线时集中暴露；每批以 §7-2 编译级检查推进（实施会话执行，本规格不代跑）。
- **DoD**：C1 对外 7 再导出名 + §1.1 消费点路径逐项比对通过；22 条 SQL 常量（§1.2 全名清单逐项）与 21 方法体经 `git diff --color-moved` 复核为纯移动（normalize 后逐字节一致）；事务/锁/epoch 顺序 diff 为零；可见性提升严格限于 §4.2 所列 5 个 `pub(super)` 项、`uuid_from`/`device_from` 保持 private；测试 27 → 27 按 §5 同位、断言零改写；`sql_shape` 6 处外部调用零改动；§7 命令按前提执行并如实报告（含未执行项）；未跑真库则不声称真库通过；每批回滚点可用。

## 9. 例外说明（C4 超阈值内聚论证）

- **`authz_repo.rs`（约 1,650 行，> 1,500）**：理由——同一类型对 `AuthzRepository` 的实现只能是一个不可跨文件分割的 impl 块（§4.1）；21 个方法共享同一事务骨架、锁次序、epoch/审计顺序与错误映射（§2），方法间成对共享内联 DML 形状，属同一「管理写/读仓储」职责内聚体。后续再拆方案：若审阅后需要更薄的 impl，下一轮按「每方法一个独立变更」把方法体提取为同实体域自由函数 + impl 内一行薄封装（`Box::pin(async move { f(&self.db, …).await })`），每次一个方法、单独审查形状等价（SQL/绑定/commit 位点零 diff）；本轮不做，因它逐方法改动业务代码体，超出拆分边界。
- **`snapshot_codec.rs`（约 880 行，900 内不申请例外）**：9 个快照 fail-closed 解码器共享同一「原始列元组形状 → 固定去敏」契约与同形 fixture，14 个回归测试与其同文件同位（C3），再拆会复制元组构造工具、破坏断言同位。

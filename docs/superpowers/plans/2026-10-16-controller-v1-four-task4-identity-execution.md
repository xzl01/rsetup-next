# Controller V1 四专题 Task 4 · Identity Task 2/3/4 实施计划（TDD 执行版）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实；用 `- [ ]` 复选框跟踪步骤。本文是**条件性后续实施计划**：不是实施授权、真库操作许可或生产发布批准。文中所有 `cargo test` 命令均为**未来执行步骤**，本轮文档修订不运行任何代码、不连接任何数据库、不提交。
>
> **与既有计划的关系：** 本文**取代** `docs/superpowers/plans/2026-10-16-controller-v1-remaining-identity-tdd.md` 的 Task 2、Task 3、Task 4 三节的全部实施细节（该三节降级为历史草案，其余 Task 1/5/6/7/8 仍有效且不在本文范围内）。出现冲突时以本文 + 规格为准。

**Goal:** 为 Controller V1 交付 Identity Task 2/3/4 的可审查实施：真实 SQLx 管理读/写仓储（统一锁序、同事务脱敏审计、`authz_epoch` CAS、严格最后管理员防线）、授权与设备服务整合、共享可信认证门禁，以及 26 条管理路由从真实仓储装配的生产网关，并把 fixture 证据与真库证据严格分层记录。

**Architecture:** 维持既有分层：HTTP 安全与认证门禁（单值 Host/Cookie/Origin/CSRF/限速/JSON 1 MiB）→ 管理 API handler（薄，只做解析、权限前置、投影）→ 服务/策略（纯函数求值 + 来源归因）→ SQLx 事务仓储（`schema_meta` guard → users(UUID 序) → roles → device_groups → devices(公钥序) → 依赖行；revision CAS + `authz_epoch` checked CAS + 11 字段脱敏审计同事务提交）。认证门禁从 `http_auth.rs` 抽取为 `http_gate.rs` 中的共享可信函数，管理路由与 Runtime 任务路由共用同一边界，禁止各自复制 cookie/Host 解析器。生产SQLx网关由独立集成owner从同一DbPool构造，显式传入ManagementState与真实probe/sink；不从旧AppState读取不存在的业务字段；任何 handler 不得返回恒定 200、空集或 `None` 冒充闭环。

**Tech Stack:** Rust 2024 / MSRV 1.85；Axum 0.8、Tokio、SQLx 0.8（MySQL）、Argon2id、SHA-256、UUIDv4、Chrono、serde/serde_json、tower（dev-dependency，仅 `ServiceExt::oneshot`）；MySQL 8.4 LTS 与 TiDB 8.5 LTS 作为两个隔离开发引擎。**零新增 Cargo 依赖**：`sha2`、`hex`、`uuid`、`rand`、`serde` 均已在本 crate 声明，`cargo build --offline --locked` 必须继续成立。

**Spec:**
- [四专题 Task 4 收敛设计](../specs/2026-10-16-controller-v1-four-task4-design.md)（**权威**：第 3 节身份事务/HTTP 边界、第 6 节备份与分级验收）
- [01 身份与授权规格](../specs/2026-09-23-controller-v1-01-identity-access.md)（§4 权限点、§5 动态授权、§6 可见性、§7 特殊管理操作、§8 专项验收）
- [02 数据模型与管理 API 规格](../specs/2026-09-23-controller-v1-02-data-api.md)（§3 公共契约、§4–5 路由表、§7 系统端点、§8 验收）
- [05 运行与资源参数](../specs/2026-09-23-controller-v1-05-runtime-operations.md)（§6 管理 API body/限速）
- [应用层完整性设计](../specs/2026-10-03-controller-application-integrity-design.md)
- [会话管理设计](../specs/2026-10-05-controller-session-management-design.md)
- [Runtime 剩余计划](2026-10-16-controller-v1-remaining-runtime-tdd.md)（Task 3 v4 迁移/版本化 guard、Task 6 任务 HTTP）
- [Web 剩余计划](2026-10-16-controller-v1-remaining-web-tdd.md)（Task 2/3/4/7 消费本计划路由）
- [跨专题集成与验收计划](2026-10-16-controller-v1-integration-acceptance-tdd.md)（G5、AT-01..18 证据分层）

---

## Global Constraints

1. **当前代码事实基线（执行者必须先核对这些事实，不得按本计划臆测已存在的东西）：**
   - `src/error.rs` 只有 `InvalidArgument, RevisionConflict, PermissionDenied, NotFound, ResourceExhausted, Config(String), SchemaNotReady{..}, Database(#[from] sqlx::Error), Crypto`；**没有** `LastAdmin`、**没有** `InvalidState`。
   - `src/authz/mod.rs` 只注册 `pub mod policy;`（Task 1 已完成）；**不存在** `authz/sqlx_repo.rs`、`authz/service.rs`、`devices/repository.rs`、`api/` 模块。
   - `src/http_auth.rs` 中 `single_header`、`session_cookie_value`、`get_single_origin`、`json_content_type`、`backend_error`、`auth_required`、`origin_host_invalid`、`csrf_invalid`、`not_ready`、`exhausted`、`rate_limited`、`parse_login_body`、`valid_username` 均为**私有函数**；`EndpointId` 只有 `Me, Sessions`。
   - `src/integrity.rs::validate_guard_rows` **硬编码** `matches!(rows[0], Ok((1, 3)))`；`scan_identity_rows(db, Some(3))` 亦硬编码 3。`lock_integrity_guard`、`require_active_admin` 是 `pub(crate)`，`raw_bool` 是**私有**（管理仓储要用必须提升为 `pub(crate)` 或等价复用）。
   - `src/auth/sqlx_repo.rs` 有自己的 `static AUTH_PROCESS_EPOCH`/`AUTH_EVENT_SEQ`（第 109–110 行）与本文件内 `struct AuditInsert`/`insert_audit`；`src/db.rs::compare_and_set` 另有自己的 `static PROCESS_EPOCH`/`EVENT_SEQ`。二者刻意"完全独立"以免 `uq_audit_epoch_seq` 冲突（见 `auth/sqlx_repo.rs` 第 105–107 行注释）。
   - `src/devices/service.rs` 已有 `DeviceService<S: AdmissionStore, K: AdmissionChangeSink>` 与 `approve/reject/reopen/revoke/reauthorize`；`DbPool` 已实现 `AdmissionStore`。
   - `src/model.rs` 有 `AdmissionState/ReviewDecision/AdmissionSnapshot/decimal_u64_json`；`src/authz/policy.rs` 有 `Permission/GrantSource/GrantScope/GrantRecord/RoleRecord/evaluate_grants/minimum_device_projection`。
   - `migrations/0003_identity_application_integrity.sql` 已建立 `schema_meta, users, sessions, roles, role_permissions, device_groups, devices, group_members, grants, admission_decisions, audit_events`；`users.display_name VARCHAR(255) NOT NULL` 已存在，但 `auth::service::IdentityUser` **不含** `display_name`。
   - `src/http_live.rs` 已有 `LiveAuth::new(Arc<AuthService<SqlxIdentityRepository>>, DbPool)` 实现 `AuthServiceApi`；`src/main.rs` 仍只启动 `build_router`（health-only）。
2. **禁止事项：** 不读取或复制 `secret/` 内容到任何产物；不连接非授权数据库；不伪造备份引用/ACK/验收；不执行 `DROP DATABASE`；不请求或执行需要 DB root 权限的锁验证；不运行真实硬件 reboot；不 `git add -f`；不把凭据、token、token_hash、密码哈希、临时密码写入日志、报告、fixture、审计 `params_redacted` 或提交。
3. **TDD 铁律：** RED 必须是**可编译、可运行、因行为断言失败**的测试（运行数恰好为 1 或声明的 N，失败来自断言而非编译错误、缺模块、`#[ignore]`、0 用例）。桩允许"可编译但行为错误"，禁止以空 handler/恒真断言冒充。每个任务末尾给出精确过滤命令与期望输出。
4. **fixture / 真库分层（证据账本）：** ①纯函数/内存组件（可离线 GREEN）；②进程内 `tower::ServiceExt::oneshot` + fake `AuthServiceApi` + fixture 网关（只证明 handler 行为，**不得称生产 GREEN**）；③真实双引擎 SQLx 事务（`#[ignore]`，仅在操作员核验隔离目标、权限与真实备份后串行执行）；④浏览器/线协议/硬件（不在本计划）。缺前置时标 `BLOCKED`/`未验证`，**不得**用 `return`、空 fixture 或"只读 fake"冒充第 ③ 层。
5. **认证边界唯一：** 所有管理读写必须先经 `http_gate` 的共享门禁（单值 Host → 单值/严格 Cookie → `is_live_session` 预检 → DB 权威 `authenticate` → CSRF/限速/body）。Gate 内不得出现任何业务判断；handler 不得自行解析 cookie/Host/Origin。无有效会话时对业务网关的调用数必须为 0。
6. **权限点封闭：** 只允许 `device.read`、`device.status.read`、`device.reboot`、`device.task.read` 四个枚举。规格中 `task.read` 简写统一指 `device.task.read`，**严禁新增权限点**（包括管理专用权限点）。
7. **管理写事务契约（每个写方法都必须满足，缺一即审查不通过）：** `tx.begin` → `integrity::lock_integrity_guard(&mut tx)`（`schema_meta` singleton 行锁 + 版本校验）→ 锁序 `users`(UUID 字节序, actor 与 target 同序, 同一用户只锁一行) → `roles`(UUID 序) → `device_groups`(UUID 序) → `devices`(公钥字节序) → 依赖行；guard 内**重新从行**核验 actor `active && is_admin && !must_change_password`（布尔严格 0/1，脏值失败关闭）；revision CAS 且 `rows_affected == 1`；`authz_epoch` checked 自增 CAS；同事务 11 字段脱敏审计；任一步失败整体回滚。
8. **最后管理员：** 停用或降级"当前活跃管理员"时，事务内 `SELECT COUNT(*) FROM users WHERE active = TRUE AND is_admin = TRUE`，`count <= 1` 返回 `ControllerError::LastAdmin` → HTTP **409 `LAST_ADMIN`**（message_key `identity.last_admin`）。降权/停用不得绕过；并发降权由 guard 行锁串行化。
9. **契约常量（本计划需实现的，规格明确的照抄，规格未明确的标"待审默认"）：** 基础路径 `/api/v1`；成功 `{data, request_id}`；失败 `{error:{code,message_key,params},request_id}`；`request_id` 每响应新 UUIDv4；body ≤ 1 MiB；会话写 20/s；端点读 50/s；错误码枚举见 02 §3（本计划新增 `LAST_ADMIN`、`INVALID_STATE` 映射）；`revision` 线上为十进制字符串。**待审默认**：分页 `limit` 默认 50、上限 200；`cursor`/`next_cursor` 一律使用「统一签名 cursor」（定义见 Task 6「统一签名分页 cursor 契约」小节），废弃裸内部键直传（UUID 32 hex / 设备 64 hex），任何管理分页端点不得签发或接受裸 hex cursor。
10. **锁与幂等：** 管理写禁止在事务内做网络 I/O、Argon2 哈希或任何不可控耗时工作（临时密码生成与哈希在事务**之前**完成，事务只接收 `next_hash`）。
11. **版本化 guard 协调（与 Runtime）：** 本计划所有读写路径调用 `integrity::lock_integrity_guard` 与只读版本检查，**不得**在管理仓储内自建 `schema_version == 3` 硬编码。Runtime Task 3 必须把 `validate_guard_rows`/`scan_identity_rows` 从硬编码 3 改为"启动只读检查确认的期望版本"（见 Task 13 协调接口）；在 v4 下管理路由与登录/改密/准入/任务提交必须共用同一版本化 guard。
12. **路径与命名：** 新增模块 `src/http_gate.rs`、`src/audit.rs`、`src/authz/sqlx_repo.rs`、`src/authz/service.rs`、`src/devices/repository.rs`、`src/api/{mod,users,roles,grants,groups,devices,approvals,audit,system}.rs`。设备 ID 线上/路径一律小写 hex64；UUID 一律规范小写 36 字符（无花括号）。
13. **静态检查与提交边界：** 每个任务结束运行 `cargo fmt -p rsetup-controller -- --check` 与 `cargo clippy --offline --locked -p rsetup-controller -- -D warnings`；提交前 `git status --short` + `git diff --staged` 人工核对不含秘密与无关文件。本文档本身**不产生任何提交**。
14. **偏差记录义务：** 若实现中发现规格与本计划冲突，按安全规范裁决并在代码注释 + 提交信息 + 审查记录中写明偏差（例如 G15：新增管理审计写者使用独立 epoch 命名空间，旧写者 epoch 不统一）。

---

## 文件清单与职责

| 文件 | 动作 | 职责 |
| :--- | :--- | :--- |
| `crates/rsetup-controller/src/http_gate.rs` | Create | 共享可信认证门禁：Host/Origin/Cookie/CSRF/限速/body 读取；管理路由与 Runtime 任务路由共用 |
| `crates/rsetup-controller/src/http_auth.rs` | Modify | 删除已迁出的私有解析函数改为 `use crate::http_gate::*`；`EndpointId` 扩展 14 个读变体（10 管理 + 3 任务 + 1 独立设备状态读，路由模板 → 变体映射表见 Task 1）；行为零变化 |
| `crates/rsetup-controller/src/error.rs` | Modify | 新增 `LastAdmin`、`InvalidState` 变体 |
| `crates/rsetup-controller/src/audit.rs` | Create | 管理/任务写者独立命名空间（`ADMIN_PROCESS_EPOCH`/`ADMIN_EVENT_SEQ`）的脱敏审计写入器，仅服务新管理/任务写者 |
| `crates/rsetup-controller/src/auth/sqlx_repo.rs` | 不变 | AUTH 审计命名空间保留：本地 `AUTH_PROCESS_EPOCH`/`AUTH_EVENT_SEQ` 与 `insert_audit` 逻辑原样 |
| `crates/rsetup-controller/src/db.rs` | 不变 | CAS 审计路径保留：函数内局部 `PROCESS_EPOCH`/`EVENT_SEQ` 与审计 INSERT 原样；未来 v4 版本化不等于重写旧审计 |
| `crates/rsetup-controller/src/integrity.rs` | Modify | `raw_bool` 提升 `pub(crate)`；不改版本常量逻辑（由 Runtime 任务 3 负责版本化） |
| `crates/rsetup-controller/src/authz/sqlx_repo.rs` | Create | `UserRecord`、`AuthzRepository` trait、`SqlxAuthzRepository`（读 + 管理写事务）；**内部授权快照层**（`AuthorizationSnapshot` DTO、`AuthorizationSnapshotRepository` trait、`SqlxAuthorizationSnapshotRepository`、`pub(crate)` 同事务核心 `load_snapshot_in_tx`/`load_actor_authorization_in_tx`/`load_device_snapshot_in_tx` 与快照 SQL 常量） |
| `crates/rsetup-controller/src/authz/service.rs` | Create | `AuthzService`：有效权限求值（经内部快照层同事务读 DB 当前快照后纯求值，不再依赖 `Session.user` 授权字段）、来源归因、临时密码生成（事务外）、管理员写编排 |
| `crates/rsetup-controller/src/authz/mod.rs` | Modify | 注册 `pub mod service; pub mod sqlx_repo;` 并 re-export（含内部快照层与 policy 新增纯函数/类型） |
| `crates/rsetup-controller/src/authz/policy.rs` | Modify | 新增纯函数 `permission_sources` 与 `PermissionSourceKind/PermissionSourceEntry`；新增内部快照纯求值 `SnapshotActor`、`ALL_DEVICE_PERMISSIONS`、`is_current_admin`、`eval_memory_permissions` |
| `crates/rsetup-controller/src/devices/repository.rs` | Create | `DeviceAdminView`、`DeviceRepository` trait、`SqlxDeviceRepository`（列表可见性过滤、详情、组成员替换、档案改名；候选公钥 keyset 扫描 `DEVICE_PAGE_KEYS_SQL`/`DEVICE_PAGE_KEYS_FIRST_SQL` 为 `pub(crate)`，列表/详情读路径复用内部快照同事务核心，普通分支不调 admin-readonly helper） |
| `crates/rsetup-controller/src/devices/mod.rs` | Modify | 注册 `pub mod repository;` |
| `crates/rsetup-controller/src/api/mod.rs` | Create | `ManagementState`、错误映射 `map_error`、分页/游标/revision 解析、`require_admin_session`、`build_management_router` |
| `crates/rsetup-controller/src/api/users.rs` | Create | 5 条 users 路由 + `UsersGateway` trait + fixture |
| `crates/rsetup-controller/src/api/roles.rs` | Create | 3 条 roles 路由 + `RolesGateway` |
| `crates/rsetup-controller/src/api/grants.rs` | Create | 3 条 grants 路由 + `GrantsGateway` |
| `crates/rsetup-controller/src/api/groups.rs` | Create | 4 条 groups 路由 + `GroupsGateway` |
| `crates/rsetup-controller/src/api/devices.rs` | Create | 3 条 devices 路由 + `DevicesGateway` + `DeviceLiveProbe` |
| `crates/rsetup-controller/src/api/approvals.rs` | Create | 6 条 approvals/devices 审批路由 + `ApprovalsGateway` + `ManagementNotificationSink` |
| `crates/rsetup-controller/src/api/audit.rs` | Create | 1 条 `GET /api/v1/audit` + `AuditGateway` |
| `crates/rsetup-controller/src/api/system.rs` | Create | 1 条 `GET /api/v1/system/status` + `SystemGateway` + `RecoveryModeProbe` |
| `crates/rsetup-controller/src/http_api.rs` | Modify | 新增 `user_admin_public`、`page_data` 投影helper |
| `crates/rsetup-controller/src/lib.rs` | Modify | 注册 `pub mod http_gate; pub mod audit; pub mod api;`；仅模块导出，不拥有顶层builder |
| `crates/rsetup-controller/src/main.rs` | 独立集成owner | 唯一顶层生产构造/启动调用；Identity各Task不修改 |
| `crates/rsetup-controller/tests/identity_admin_db.rs` | Create | 真实双引擎管理写事务/LastAdmin/CAS 验收（全部 `#[ignore]` + 门禁） |
| `crates/rsetup-controller/tests/common/mod.rs` | Modify | 仅在 Task 14 增加去敏 fixture 辅助（不写死目标；从既有 `required_test_db` 扩展） |

---

## 依赖图与任务顺序

```
Task 1 共享认证门禁抽取 ──┐
Task 2 错误映射/LastAdmin ─┤
Task 3 只读 SQLx 仓储 ─────┼──► Task 5 服务与来源归因 ──► Task 6 路由骨架+生产网关
Task 4 审计+写事务仓储 ────┘                                      │
                                                                  ├─► Task 7  users   (5 路由)
                                                                  ├─► Task 8  roles+grants (6)
                                                                  ├─► Task 9  groups (4)
                                                                  ├─► Task 10 devices (3)
                                                                  ├─► Task 11 approvals (6)
                                                                  └─► Task 12 audit+system (2)
                                                                              │
                                                              Task 13 生产挂载 + v4 协调 + 26 路由核对
                                                                              │
                                                              Task 14 真实双引擎验收（受控，#[ignore]）

Task 1 必须先于 Task 6..12（门禁是它们的认证前置）。
Task 3 必须先于 Task 5；Task 4 必须先于 Task 5 的写编排与 Task 7..12 的写路径。
Task 13 依赖 Runtime Task 3（版本化 guard）与 Runtime Task 6 的路由入口签名；未就绪时 Task 13 的挂载步骤保持未完成。
```

---


---

## 专题带编号拆分模块索引（单文件 700~2700 行）

为保障可维护性并消除单文件过大问题，完整执行细节按职责已拆分到对应子目录中（保留本主计划在 `docs/superpowers/plans/` 作为执行总索引）：

- [01-http-gate-and-repo.md](2026-10-16-identity/01-http-gate-and-repo.md)：Task 1–3: 共享认证门禁、错误变体、只读 SQLx 仓储（~2664 行）
- [02-audit-and-service.md](2026-10-16-identity/02-audit-and-service.md)：Task 4–5: 管理写事务仓储（锁序+CAS+LastAdmin）、授权服务扩展（~2336 行）
- [03-management-routes.md](2026-10-16-identity/03-management-routes.md)：Task 6–10: 管理路由骨架、users/roles/grants/groups/devices 资源管理（~2158 行）
- [04-approvals-and-acceptance.md](2026-10-16-identity/04-approvals-and-acceptance.md)：Task 11–14: approvals/audit 路由、生产适配器与真实双引擎事务验收（~1283 行）
- [05-adapters-matrix.md](2026-10-16-identity/05-adapters-matrix.md)：管理适配器与 26 路由矩阵专用规范（~488 行）

---


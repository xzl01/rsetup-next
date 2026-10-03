# Controller 01 身份与数据面 TDD 实施计划（条件性）

> **致执行代理：** 本文是待审阅的条件性计划，不是实施授权；实施前完成审阅与协议安全关卡，随后使用 superpowers:subagent-driven-development 或 superpowers:executing-plans 逐项执行。

**Goal:** 新增独立 crates/rsetup-controller Rust crate，负责 MySQL/TiDB schema/migration/初始化、认证会话、动态并集授权与设备可见性、设备档案/审批管理 HTTP 和审计；不做板端传输握手、重启任务及前端。

**Architecture:** 独立模块化单体，HTTP 输入/安全校验→服务策略→repository 短事务；revision CAS、authz_epoch、变更和审计同事务。设备身份是公钥；每条 grant 的 source/scope 成对匹配后并集。

**Tech Stack:** Rust edition 2024、MSRV 1.85；已有 Tokio/Axum/Serde/Chrono/UUID/Thiserror/Tracing；SQLx、Argon2、随机、SHA-256、hex 均是待审阅候选，适配未验证；tower 已是现有 dev-dependency，新增 controller crate 的 router 测试可在审阅后复用。现有测试约定为模块内 #[cfg(test)]、#[test]/#[tokio::test]、Axum tower::ServiceExt::oneshot。

**Spec:** docs/superpowers/specs/2026-09-22-controller-design.md；2026-09-23-controller-v1-00-index.md；2026-09-23-controller-v1-01-identity-access.md；2026-09-23-controller-v1-02-data-api.md；跨专题契约还需参阅 2026-09-23-controller-v1-03-device-protocol.md、2026-09-23-controller-v1-04-task-lifecycle.md、2026-09-23-controller-v1-05-runtime-operations.md、2026-09-23-controller-v1-06-web-acceptance.md；docs/architecture.md；根 Cargo.toml。

## 审阅与协议安全关卡

1. controller-v1/draft-1 是待审阅草案；用户只批准以之作为条件性规划依据，未批准实施、安装依赖、定稿接口或发布。设计/安全/数据库负责人审阅 01/02 的字段、密码会话、权限、API、DDL/事务语义；若结论变化先改计划。
2. 00 §6 发布阻断：服务端签名未覆盖影响重连的 reason_code；验签失败“立即断开”和“返回拒绝”冲突；AEAD 失败后连接终止策略不明确。独立协议/密码学审查修订并做板端/中控双端负向/互操作测试；本计划不做握手，也不能把审批记录当安全接入证明。
3. PENDING 明文保活不能认证中控；HTTP 管理链路本身不加密，CSRF、Argon2、HttpOnly 不替代 TLS。须审阅可信受控网络或 TLS 反代及可信代理/Host/Origin 配置；HTTP 直连 cookie 不设置 Secure；安全关卡未过不得宣称生产可用。
4. 拟议而非获批：opaque cookie、≥256-bit token、SHA-256 digest、重启失效、30 min idle/12 h absolute、Argon2id m=65536 KiB/t=3/p=1、用户名密码边界、reset-admin 恢复路径、四权限点、viewer/operator、MySQL 8.4 LTS/TiDB 8.5 LTS、分页 50/200、审批 1024、body 1 MiB、登录 15 min/5 次。审阅前不得固化。
5. 只有对真实 MySQL 与 TiDB 分别测试迁移、唯一约束、CAS/并发和恢复后才能报告具体被测版本；未运行/失败必须写未验证/失败，不得说已支持两库，也不能用 mock 代替。

## 全局约束及文件清单

- 根 Rust workspace 目前只有 crates/rsetup-core 与 crates/rsetup-app 两个 members，apps/desktop/src-tauri 被 exclude；只建议新增 crates/rsetup-controller，不改现有 crates/rsetup-app/src/server.rs、板端握手或前端。
- 新增依赖须在同一 Task 将 manifest 与根 Cargo.lock 一起提交；候选依赖须通过 MSRV 1.85 与 CI 的 aarch64/x86_64 多 target 检查。`make test` 用 `cargo test --workspace --locked`；现有 CI 的 MSRV、多 target、离线打包均检查 `--locked`，新增依赖不得只改 manifest。
- DB fixture 的 `$MYSQL_TEST_URL`/`$TIDB_TEST_URL` 由未来实施时的本地隔离容器或 CI 显式提供，镜像/服务配置和版本随 G1 审批的 MySQL 8.4 LTS/TiDB 8.5 LTS 建议版本确定，不预设现有 CI 已有对应 service。真实 DB 测试标 `#[ignore]`，以 `CONTROLLER_TEST_DATABASE_URL` 显式门控；未来修改 `.github/workflows/ci.yml`，分别按对应 URL 执行 MySQL/TiDB 的 `-- --ignored --test-threads=1`，并核查各自实际执行用例数非零（零用例不得算 PASS）。缺某引擎 URL 标该引擎「未验证」（不记 PASS、不以缺 URL 阻塞普通 CI），URL 存在而测试失败或执行用例为零则 CI fail；本次只修计划，不配置 CI/服务。
- DeviceId 是 Ed25519 公钥 32B，HTTP lower hex64、DB BINARY(32)，SN/IP 不得作身份；UUIDv4 API 小写/DB BINARY(16)；revision/counter JSON 十进制字符串；未知字段/枚举 400，expected_revision 冲突 409。
- utf8mb4、显式唯一键/索引、短事务；不能假定 MySQL/TiDB DDL 回滚、锁、自增、JSON 查询一致。migration 与首次 admin 事务分离；初始化持久标记/唯一约束防重复，随机密码只在成功提交后写一次受限日志，DB 只存哈希，不能因用户表为空或重启重置。
- 管理员身份与设备角色分离；最后一个 active && is_admin 不得停用/降级，锁共同保护行；普通用户不可转授权。grant 每条 source(role/direct) 与 scope(all/group/device) 成对匹配后取并集、无 deny；all 覆盖新设备。
- 任一设备权限只给最低识别列表；reboot 不隐含 status/read；不可见和不存在同 404，列表无过滤前总数；设备多组，批准不自动分组，REVOKED 恢复须板端人工重置。
- 变更 revision/authz_epoch/审核决定历史/审计同事务；密码、cookie/token、token digest、私钥不入普通日志/审计；事务内无网络 I/O。排除设备连接/状态轮询/SSE、主子任务/重启、NTP、UI；无真实连接不得伪报在线。
- 所有 Task 的 Step 4/5 中提到的额外边界与并发用例，**每个都须单独重复**“编写测试 → 运行确认因行为缺失而失败 → 最小实现 → 运行确认通过”；只有全部测试已绿才做 Step 5 的纯重构与完整回归。不得把 Step 5 当作先实现后补测的许可。
- 认证成功/失败、登出、强制/自助/管理员改密、管理员本机恢复、session 撤销、用户/角色/grant/组修改、最后管理员保护命中、审批/拒绝/reopen/revoke/reauthorize 必须有语言中立审计事件码；资源拒绝不能伪记为人工拒绝。审计保留原始墙钟、参考/质量/代际及进程序号的接口；时间来源尚未实现时质量必须明确 `system_fallback`，不伪装为 `ntp_valid`。
- 初始化已提交但首次密码日志输出前进程崩溃存在恢复窗口；拟议 `reset-admin` 仅本机受控 CLI、服务账号/root、交互终端展示临时密码、撤销 session、强制改密并审计，该路径必须单独安全审阅；不得以重新初始化替代。

**未来新增文件（仅获实施授权后）：** crates/rsetup-controller/Cargo.toml；src/{lib.rs,main.rs,config.rs,error.rs,model.rs,db.rs,bootstrap.rs,audit.rs}；src/auth/{mod.rs,password.rs,session.rs,service.rs}；src/authz/{mod.rs,policy.rs,service.rs}；src/devices/{mod.rs,repository.rs,service.rs}；src/api/{mod.rs,auth.rs,users.rs,roles.rs,grants.rs,groups.rs,devices.rs,approvals.rs}；migrations/0001_identity_devices.sql；tests/{common/mod.rs,mysql_identity.rs,tidb_identity.rs}（`tests/` 集成测试目录是本项目新增 pattern，现有 crate 尚无）。migration 建 schema_meta、users、sessions、roles、role_permissions、device_groups、group_members、grants、devices、admission_decisions、audit_events；不建 task/telemetry 表。

**未来仅修改：** 根 Cargo.toml 加 workspace member 与审阅后依赖，根 Cargo.lock 随依赖变更；`.github/workflows/ci.yml` 增加真实 DB ignored 测试的按引擎门控、非零用例及失败判定（Task 6；所需 CI 服务与版本仍待审批）。不修改其他既有文件。main.rs 根据 ControllerConfig::from_env、DbPool::connect、migrate、bootstrap_admin 启动；/healthz 仅探活，/readyz 未就绪 503。readyz 当前仅 DB 就绪；NTP 关卡由 03 计划按设计 §5.2 扩展，本计划不固化最终就绪判据。lib.rs 导出 build_router(AppState)->axum::Router。

**Migration 最小形态（按审阅后引擎 DDL 校准，不能凭示例宣称已验证）：**

```sql
CREATE TABLE schema_meta (
  singleton_id TINYINT NOT NULL PRIMARY KEY,
  schema_version BIGINT NOT NULL,
  instance_id BINARY(16) NOT NULL,
  initialized BOOLEAN NOT NULL,
  authz_epoch BIGINT UNSIGNED NOT NULL,
  admin_guard_revision BIGINT UNSIGNED NOT NULL
);
CREATE TABLE users (
  id BINARY(16) PRIMARY KEY,
  username VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  display_name VARCHAR(512) NOT NULL,
  password_hash VARCHAR(512) NOT NULL,
  active BOOLEAN NOT NULL,
  is_admin BOOLEAN NOT NULL,
  must_change_password BOOLEAN NOT NULL,
  revision BIGINT UNSIGNED NOT NULL,
  created_time DATETIME(6) NOT NULL,
  UNIQUE KEY uq_users_username (username)
);
CREATE TABLE group_members (
  group_id BINARY(16) NOT NULL,
  device_id BINARY(32) NOT NULL,
  PRIMARY KEY (group_id, device_id),
  KEY idx_group_members_device (device_id)
);
```

`users.display_name` 的 `VARCHAR(512)` 是宽松 DDL 上限；服务层按 02 §1 校验 1–128 字符且 ≤512 UTF-8 字节。

余下表字段/索引按 02 §2：sessions(token_hash BINARY(32) 主键、user_id 索引、process_epoch、revoked)、roles(name 唯一)、role_permissions(role_id+permission 唯一)、device_groups(name 唯一)、grants(user_id 索引、source/scope 互斥)、devices(public_key 主键、admission_state/decision/revision)、admission_decisions(追加记录)、audit_events(process_epoch+event_seq 唯一，actor/target 索引)。关键互斥由服务层验证并 DB 可行时加约束。迁移后检查信息模式中的列、索引和重复运行的版本记录；不得误以 `CREATE TABLE IF NOT EXISTS` 足以验证既有 schema 正确。

接口草案：IdentityRepository::{user_by_username(&str)->Result<Option<UserRecord>,ControllerError>,create_session(UserId,[u8;32])->Result<(),ControllerError>,revoke_user_sessions(UserId)->Result<(),ControllerError>}（sessions 以 token_hash 为主键，不传无对应列的 Uuid）；AuthzRepository::{effective_permissions(UserId,DeviceId)->Result<EffectivePermissions,ControllerError>,visible_devices(UserId,DeviceFilter)->Result<Vec<DeviceProjection>,ControllerError>}；DeviceRepository::{get_device(DeviceId)->Result<Option<DeviceRecord>,ControllerError>,decide_admission(DeviceId,u64,ReviewDecision,UserId,Option<String> /* reason */)->Result<DeviceRecord,ControllerError>}（reject 必须传非空 reason，reject/revoke 的 reason 写入 admission_decisions.reason）；`AdmissionStore::{load,compare_and_set}` 的返回值为持久完整准入快照（可命名 `AdmissionSnapshot`：`admission_state`、`review_decision`、`revision`），按公钥读取，CAS 使用公钥与 `expected_revision`、校验预期状态及合法状态/决定转换，更新完整快照，失败返回冲突；`PENDING+none` 与 `PENDING+denied` 必须可区分，传输层据此区分继续等待与 `APPROVAL_DENIED`。DB adapter 由本 crate 提供并注入 02 计划传输层；接口形状需与 02 实施时共同审阅，不新增 HTTP API。model.rs 定义这些类型；auth/service.rs 输出 login/authenticate/change_password/logout/revoke_all；authz/service.rs 输出 effective_permissions/visible_devices；devices/service.rs 输出 patch_profile/replace_group_members/decide_admission；audit.rs 提供语言中立事件与事务内脱敏写入。审批服务不直接做网络 I/O，但事务提交后的 `{device_id,revision}` 准入变更通知须明确接线给 02 的隧道管理：通知不在事务内、失败不能回滚或掩盖已提交决定；02 在通知失败时失败关闭相应活动连接，并核对最新准入状态与连接代际后再决定是否接纳/重连。通知仅为唤醒，接纳仍读最新持久状态，不以通知代替 DB 真相；不引入持久 outbox。内部接线契约为注入的 `AdmissionChangeSink::after_commit(device_id,revision)`：仅在 repository 已提交返回后调用；02 的实现负责有界投递到 `on_admission_committed`，投递失败则触发该设备 fail-closed/补偿核对，不将通知失败作为 DB 操作失败返给调用者。该 trait 在 01 的 `devices/service.rs` 定义，controller 内的生产 adapter 调用 02 暴露的通知/定向关闭入口；02 Task 9 联调时在 controller 的 AppState/启动组装处注入，避免让 protocol 反向依赖 controller。01 阶段尚无活动隧道，测试使用记录型 sink；不得把该测试 sink 当真实连接管理器。

测试示例的 fixture 名称属于 `tests/common/mod.rs` 或模块内 `#[cfg(test)]` helper 合约，不是仓库已有函数：`required_test_db()` 从 `CONTROLLER_TEST_DATABASE_URL` 取得独占空库并拒绝生产库；`u/r/g/d` 把稳定 fixture 字符串映射成 `UserId/RoleId/GroupId/DeviceId`（`d` 生成合法固定 32B 公钥而非把字母当真实 DeviceId）；`fixture_device_hex(name)` 将 `d(name)` 的 32B 公钥编码为 lowercase hex64，分别生成不可见与不存在的路径 ID；`roles/groups/no_groups` 返回已定义角色权限/组关联；`auth_fixture` 返回 `(AuthService, FakeIdentityRepository)`；`device_fixture` 返回 `(DeviceService, FakeDeviceRepository)`；`test_router` 返回注入 fake repository 的 `Router`（`d("invisible")` 已存在但对测试用户不可见，`d("missing")` 不存在）；`req/send/approval_req` 构造 Axum `Request<Body>` 并调用 `oneshot`；`sample_grant` 构造 alice 的 all+device.read；`inject_audit_failure` 使同事务 audit insert 失败。每个 helper 与使用它的首个测试同一步落地，不依赖未定义的仓库基础设施。

## Task 1：独立启动骨架、schema migration 与首次 admin

**Files:** 根/新 crate Cargo（本 Task 按候选依赖审阅结果一次引入 SQLx、Argon2id、random、SHA-256、hex、tower dev-dependency）、根 Cargo.lock、`src/{main.rs,lib.rs,config.rs,error.rs,model.rs,db.rs,bootstrap.rs}`、`migrations/0001_identity_devices.sql`、`tests/{common/mod.rs,mysql_identity.rs,tidb_identity.rs}`。

**Interfaces:** `ControllerConfig::from_env() -> Result<Self,ControllerError>`；`DbPool::connect(&ControllerConfig)`；`migrate(&DbPool)`；`bootstrap_admin(&DbPool,&dyn BootstrapSecretSink)`；`AdmissionStore::{load,compare_and_set}` 的 DB adapter 由本 crate 承担（完整 `admission_state/review_decision/revision` 快照，公钥 + expected_revision CAS 并验证转换，供 02 计划注入）。bootstrap_admin 重复调用不能改 hash 或输出第二个密码。

- [ ] **Step 1：先写红测试**

```rust
#[test]
fn device_id_rejects_serial_and_noncanonical_hex() {
    assert!(parse_device_id(&"ab".repeat(32)).is_ok());
    assert!(parse_device_id("serial-001").is_err());
    assert!(parse_device_id(&"AB".repeat(32)).is_err());
}
#[tokio::test]
#[ignore] // 真实 DB fixture：需显式 CONTROLLER_TEST_DATABASE_URL 与 --ignored
async fn bootstrap_twice_preserves_password_and_emits_once() {
    let db = required_test_db().await;
    let sink = RecordingSecretSink::default();
    bootstrap_admin(&db, &sink).await.unwrap();
    let first_hash = user_hash(&db, "admin").await;
    bootstrap_admin(&db, &sink).await.unwrap();
    assert_eq!(user_hash(&db, "admin").await, first_hash);
    assert_eq!(sink.emissions().len(), 1);
}
```

- [ ] **Step 2：运行并确认失败。** 最小空 crate/manifest、fixture 与待测类型/API 的可编译 stub 建立后运行 `cargo test -p rsetup-controller device_id_rejects_serial_and_noncanonical_hex`；必须看到断言因错误接受非规范 ID 而失败；缺 `DeviceId` 等类型/API 的编译错误或 package 不存在都不是有效行为 RED。`required_test_db()` 必须使用显式 `CONTROLLER_TEST_DATABASE_URL`，隔离测试库。另在有 `$MYSQL_TEST_URL` 时显式跑 `CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test mysql_identity bootstrap_twice_preserves_password_and_emits_once -- --ignored --exact --test-threads=1`，有 `$TIDB_TEST_URL` 时以 `CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test -p rsetup-controller --test tidb_identity bootstrap_twice_preserves_password_and_emits_once -- --ignored --exact --test-threads=1`；核对各引擎确实执行 1 个用例并因 bootstrap 行为缺失而 RED，无 URL 的引擎标未验证，不用零用例或环境错误冒充 RED。
- [ ] **Step 3：最小实现。** schema_meta 单例 initialized、username 唯一约束、版本记录；DDL migration 与账号初始化分开；短事务设置初始化标记和 admin hash，冲突时重读不覆盖；准入 DB adapter 按公钥读/持久化完整 `admission_state/review_decision/revision` 快照，CAS 核验 `expected_revision`、预期状态及合法转换，不将持久准入逻辑留给 02 计划传输层。成功提交后受限日志 sink 输出一次随机密码；持久事实不能因重启/空用户表被重置。

```rust
pub fn parse_device_id(s: &str) -> Result<[u8; 32], ControllerError> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(ControllerError::InvalidArgument);
    }
    hex::decode(s).map_err(|_| ControllerError::InvalidArgument)?
        .try_into().map_err(|_| ControllerError::InvalidArgument)
}
```

- [ ] **Step 4：运行并确认通过。** `cargo test -p rsetup-controller device_id_rejects_serial_and_noncanonical_hex`；分别仅在提供 `$MYSQL_TEST_URL`/`$TIDB_TEST_URL` 时运行 `CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test mysql_identity -- --ignored --test-threads=1` / `CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test -p rsetup-controller --test tidb_identity -- --ignored --test-threads=1`。预期 unit PASS；真实数据库 migration/初始化测试非零且 PASS 才分别记录被测引擎版本；缺 URL 标该引擎「未验证」，不作 PASS/FAIL、也不因缺 URL 阻塞普通 CI；URL 已提供但测试失败或零用例则 CI fail。
- [ ] **Step 5：重构回归。** 并发两启动只一 admin/一次输出；空库、重复迁移、部分 DDL 失败重启的真实 DB 用例按 Step 4 URL 显式运行；healthz 不等于 readyz；`cargo fmt --all -- --check && cargo test -p rsetup-controller`（此命令不运行 `#[ignore]` DB 用例）。
- [ ] **Step 6：小提交。** `git add Cargo.toml Cargo.lock crates/rsetup-controller && git commit -m "feat(controller): scaffold identity storage"`。

## Task 2：密码、认证 session、强制改密与账号管理

**Files:** `src/auth/{mod.rs,password.rs,session.rs,service.rs}`、`src/api/{mod.rs,auth.rs,users.rs}`、`src/{main.rs,db.rs,audit.rs}`；main.rs 接线 `reset-admin` 本机子命令，测试与代码在同模块。

**Interfaces:** `PasswordHasher::{hash,verify}`；`AuthService::{login,authenticate,change_password(session,current_password,new_password),logout,revoke_all,reset_admin}`；`change_password` 显式验证当前密码后才更新新哈希并撤销会话，对应 02 §4 `/auth/password` 的 `current_password,new_password`；`reset_admin` 仅本机受控 CLI、服务账号/root、交互终端一次性展示临时密码，撤销全部 session、设置强制改密并写无密码审计；该路径单独安全审阅。`IdentityRepository` 的会话 token hash/用户查找/撤销；普通用户不能绕过 `must_change_password` 白名单。

- [ ] **Step 1：先写红测试。**

```rust
#[test]
fn password_hash_rejects_wrong_secret() {
    let h = PasswordHasher::for_tests();
    let hash = h.hash("correct password").unwrap();
    assert!(h.verify("correct password", &hash).unwrap());
    assert!(!h.verify("wrong password", &hash).unwrap());
}
#[tokio::test]
async fn password_change_revokes_existing_session() {
    let (svc, repo) = auth_fixture().await;
    let login = svc.login("alice", "old password").await.unwrap();
    assert_ne!(repo.stored_session_digest(), login.raw_token.as_bytes());
    svc.change_password(&login.session, "old password", "new password").await.unwrap();
    assert!(svc.authenticate(&login.raw_token).await.is_err());
}
#[tokio::test]
async fn wrong_current_password_preserves_account_and_sessions() {
    let (svc, repo) = auth_fixture().await;
    let login = svc.login("alice", "old password").await.unwrap();
    let before = repo.auth_state("alice").await; // 未来 fixture：拥有值的 hash/账号字段+revision+排序后的 session digest 快照
    let result = svc.change_password(&login.session, "wrong password", "new password").await;
    assert_eq!(repo.auth_state("alice").await, before);
    assert!(svc.authenticate(&login.raw_token).await.is_ok());
    assert!(result.is_err());
}
```

`auth_state` 与 `AuthStateSnapshot` 是本 Step 随测试建立的未来 fake fixture：快照包含 `password_hash`、`active`、`is_admin`、`must_change_password`、`revision` 与排序后的 session digest/revoked 状态（不包含审计），可用 `assert_eq!` 比较；拒绝错误旧密码时原哈希、原会话及账号状态不变，但允许写脱敏的认证/改密失败审计事件，不断言审计表完全不变。fixture 中 alice 的原密码为 `old password`，调用时传入的 `wrong password` 不等于它，且新密码 `new password` 不等于原密码。生产密码长度等边界在审阅后配置；测试用 fixture 应选满足最终密码策略的值，避免测试因长度校验而伪 RED。

同在 Step 1 单独写 RED 测试 `reset_admin_requires_local_root_and_revokes_sessions`：非本机或非服务账号/root、无交互终端均拒绝；成功后旧 session 失效、设置强制改密，临时密码只展示一次且审计不含密码。

- [ ] **Step 2：运行确认失败。** 在可编译的 auth fixture、`AuthStateSnapshot`/`auth_state` 和 `change_password(session,current_password,new_password)` stub 就绪后，分别运行 `cargo test -p rsetup-controller password_hash_rejects_wrong_secret`、`cargo test -p rsetup-controller wrong_current_password_preserves_account_and_sessions` 与 `cargo test -p rsetup-controller reset_admin_requires_local_root_and_revokes_sessions`；错误旧密码用例须在能走到更新分支的可编译行为基线上，由原哈希、会话或账号状态被错误改动的断言而 RED；若 stub 一概返回错误让用例直接通过，则不构成 RED，须调整基线以暴露漏验旧密码的行为。不能以签名不匹配、helper/模块缺失、纯输入长度校验等编译或设置错误充数。正确改密的 `password_change_revokes_existing_session` 也单独确认行为 RED，随后 GREEN。
- [ ] **Step 3：最小实现。** CSPRNG token、DB 仅存 digest/process_epoch；单调空闲/绝对截止；不存在用户名执行 dummy hash；cookie `HttpOnly; SameSite=Strict; Path=/`，HTTP 直连无 Secure；强制改密仅 me/password/logout；`change_password(session,current_password,new_password)` 先校验旧密码及新密码规则，只有旧密码正确才在短事务中替换哈希、撤销全部 session 并提交所需审计；错误旧密码不改密码哈希、session 或账号状态（允许脱敏失败审计）。若验证与提交之间另一请求改了密码，不能仅凭先前验证覆盖更新：在事务内锁定/重新验证当前 hash 或以所验证的 hash/revision 做条件更新并检查影响行数；冲突则拒绝或重新校验，不撤销另一请求会话、不覆盖较新密码；失败审计不得泄露密码。不扩展 HTTP 错误码，按审阅后的既有 02 §3 错误契约映射。改密、停用、重置撤销全部 session，管理员 Web 临时密码只在成功响应展示一次；`reset-admin` 临时密码只在本机交互终端展示一次并审计。算法、期限按通过的审阅记录设定。

```rust
pub fn token_digest(raw: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(raw).into()
}
```

- [ ] **Step 4：运行确认通过。** 分别运行 `cargo test -p rsetup-controller wrong_current_password_preserves_account_and_sessions` 和 `cargo test -p rsetup-controller password_change_revokes_existing_session`，再运行 `cargo test -p rsetup-controller auth::` 与 `cargo test -p rsetup-controller reset_admin_requires_local_root_and_revokes_sessions`；错误旧密码不得改 hash/账号状态或撤销会话，正确旧密码须成功改密且旧 cookie 失效；另确认正误密码、DB 不存明文 token、初次改密非白名单 403、进程代际不符拒绝；本机恢复非授权来源拒绝，成功后旧 session 失效、临时密码仅终端展示一次。
- [ ] **Step 5：重构回归。** 用户名 ASCII 规则、密码 Unicode 字符数与 UTF-8 byte 双限制、不等于旧密码；并发改密时旧 hash 验证与提交间被另一请求更新不得覆盖较新密码或撤销其新会话（此用例也须按全局约束单独 RED→GREEN）；不存在用户响应/耗时形态、按账号和来源限速；最后管理员并发降级/停用；session 过期与重启失效；秘密不落普通日志/审计（脱敏失败审计允许）。`cargo test -p rsetup-controller && cargo clippy -p rsetup-controller --all-targets -- -D warnings`。
- [ ] **Step 6：小提交。** `git add crates/rsetup-controller/src/auth crates/rsetup-controller/src/api/mod.rs crates/rsetup-controller/src/api/auth.rs crates/rsetup-controller/src/api/users.rs crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/db.rs crates/rsetup-controller/src/audit.rs && git commit -m "feat(controller): add local auth and accounts"`。

## Task 3：动态并集授权与可见性、role/grant 管理

**Files:** `src/authz/{mod.rs,policy.rs,service.rs}`、`src/api/{roles.rs,grants.rs}`、`src/{model.rs,db.rs,audit.rs}`。

**Interfaces:** `evaluate_grants(user,device,grants,roles,groups) -> HashSet<Permission>`；服务层 `effective_permissions` 与 `visible_devices`；数据库变更同事务递增 authz_epoch 并写审计。

- [ ] **Step 1：先写红测试。**

```rust
#[test]
fn role_permissions_never_cross_grant_scope() {
    let gs = vec![Grant::role(u("u"), r("viewer"), Scope::Device(d("A"))),
                  Grant::role(u("u"), r("operator"), Scope::Device(d("B")))];
    let p = evaluate_grants(u("u"), d("A"), &gs, &roles(), &groups());
    assert!(p.contains(&Permission::DeviceRead));
    assert!(!p.contains(&Permission::DeviceReboot));
}
#[test]
fn removing_group_preserves_independent_all_scope() {
    let gs = vec![Grant::direct(u("u"), vec![Permission::DeviceRead], Scope::All),
                  Grant::direct(u("u"), vec![Permission::DeviceReboot], Scope::Group(g("ops")))];
    assert_eq!(evaluate_grants(u("u"), d("A"), &gs, &roles(), &no_groups()),
               HashSet::from([Permission::DeviceRead]));
}
```

- [ ] **Step 2：运行确认失败。** 建立可编译的 evaluator stub（如先返回空集合）及所需类型/helper 后运行 `cargo test -p rsetup-controller role_permissions_never_cross_grant_scope`；须由 `DeviceRead` 正断言失败确认行为 RED，缺 evaluator/类型的编译错误不算有效 RED。
- [ ] **Step 3：最小实现。** 对每条 `user && scope.covers(device,groups)` 的 grant，只展开该 grant 的 role 权限或 direct 权限，再求 HashSet union。默认拒绝、未知权限拒绝、无 deny；角色不能授予 is_admin；按 authz_epoch 失效缓存。

```rust
pub fn evaluate_grants(user: UserId, device: DeviceId, gs: &[Grant],
    roles: &RoleMap, groups: &HashSet<GroupId>) -> HashSet<Permission> {
    gs.iter().filter(|g| g.user_id == user && g.scope.covers(device, groups))
        .flat_map(|g| g.source.permissions(roles)).collect()
}
```

- [ ] **Step 4：运行确认通过。** `cargo test -p rsetup-controller authz::`；all 覆盖新增设备；组移除动态撤权但多组/其它 grant 仍有效；删除 device grant 不抵消 all/group；reboot-only 没有 status；无授权拒绝。
- [ ] **Step 5：重构回归。** 角色权限/组成员/grant 修改与 epoch/audit 原子提交；并发读/写只得同一 epoch 视图；管理员最后身份与普通设备角色区分；不可见/不存在统一 404、列表无隐藏总数；MySQL 与 TiDB 分别验证 CAS。ACL-02 的“组变更与任务提交并发、执行前重验及已发任务不能假称撤回”必须与 03 计划 Task 2/3 联合测试，不能用本 Task 的授权读写测试代替。`cargo test -p rsetup-controller`。
- [ ] **Step 6：小提交。** `git add crates/rsetup-controller/src/authz crates/rsetup-controller/src/api/roles.rs crates/rsetup-controller/src/api/grants.rs crates/rsetup-controller/src/model.rs crates/rsetup-controller/src/db.rs crates/rsetup-controller/src/audit.rs && git commit -m "feat(controller): enforce dynamic authorization"`。

## Task 4：设备档案、分组与人工审批事务

**Files:** `src/devices/{mod.rs,repository.rs,service.rs}`、`src/api/{groups.rs,devices.rs,approvals.rs}`、`src/{audit.rs,model.rs}`。

**Interfaces:** `DeviceService::{patch_profile,replace_group_members,decide_admission}`；审批仅显式公钥 targets + expected_revision，状态变更、admission_decisions、audit 同 CAS 事务；服务本身不调用板端传输，但提交后发 `{device_id,revision}` 给 02 隧道管理；通知失败不撤销决定，02 须失败关闭活动连接并重查最新状态/代际。

- [ ] **Step 1：先写红测试。**

```rust
#[tokio::test]
async fn approve_is_cas_audited_and_does_not_auto_group() {
    let (svc, repo) = device_fixture().await;
    let rev = repo.device(d("A")).await.revision;
    let result = svc.approve(admin(), d("A"), rev).await.unwrap();
    assert_eq!(result.admission_state, AdmissionState::Approved);
    assert!(repo.groups_for(d("A")).await.is_empty());
    assert_eq!(repo.audit_events().await.len(), 1);
    assert!(matches!(svc.approve(admin(), d("A"), rev).await,
                     Err(ControllerError::RevisionConflict)));
}
```

另在 Step 1 逐个落地可编译 fake/真实 DB 用例（helper 均为未来 fixture）：`assert_eq!(store.load(d("A")).await?.review_decision, ReviewDecision::None)` 与拒绝后 `assert_eq!(store.load(d("A")).await?.review_decision, ReviewDecision::Denied)` 均保持 `AdmissionState::Pending`；旧 `expected_revision` CAS 断言 `RevisionConflict`，成功后 `revision` 恰加一且不出现非法状态/决定组合。用通知 spy 断言事务提交前 `notifications().is_empty()`，提交后恰有 `(d("A"), result.revision)`；模拟通知失败后重读 DB 仍为已提交的 denied/revoked 决定，并以 02 联合用例验证相应活动连接失败关闭、以最新状态与代际决定是否接纳。不得把 mock 通知成功当实际隧道已断开。另测直接调用审批服务时普通用户、停用管理员及未改密管理员均被拒绝，状态/revision/审核决定不变且无提交后通知；不依赖 HTTP admin-only 代替服务事务内授权。

- [ ] **Step 2：运行确认失败。** 先补可编译的设备服务/fixture stub 与类型，再运行 `cargo test -p rsetup-controller approve_is_cas_audited_and_does_not_auto_group`；须由 `Approved`/审计等行为断言失败确认 RED，不把服务/类型缺失造成的编译错误算作 RED。
- [ ] **Step 3：最小实现。** 设备公钥唯一，组多对多；短事务内重查 actor 为 active、is_admin 且已改密，与身份/授权变更按 epoch 保护序列化；再读当前 revision、校验允许转换、CAS、插决定与审计。新身份 PENDING+none，PENDING 超配额时不创建 admission 记录（资源拒绝，不得记 denied/revoked；配额值待审，见 05 规格）；approve APPROVED+approved 不自动组；reject PENDING+denied 并写 reason；reopen PENDING+none；revoke REVOKED+revoked；reauthorize APPROVED+approved 且标记需板端人工重置。资源不足不能写 denied/revoked；提交后才发 `{device_id,revision}` 通知给 02 隧道管理，通知失败不改已提交决定，由 02 失败关闭活动连接并重查最新准入状态/连接代际；不得宣称设备已断开或已在线。

```rust
pub async fn approve(&self, actor: UserId, id: DeviceId, rev: u64)
    -> Result<DeviceRecord, ControllerError> {
    // repository内重查管理员资格，状态/决定/revision/审计在短事务中提交。
    let committed = self.repo
        .decide_admission(id, rev, ReviewDecision::Approved, actor, None).await?;
    self.admission_changes.after_commit(id, committed.revision).await;
    Ok(committed)
}
```

- [ ] **Step 4：运行确认通过。** `cargo test -p rsetup-controller devices::`；确认批准不入组、旧 revision 409 无半写、显式清单批量不扩大、替换组成员去重/多组、reopen/reauthorize 提示重置。
- [ ] **Step 5：重构回归。** 并发同 revision approve/revoke 只一胜者且仅一审计；审计失败时状态/决定一起回滚；公钥更换产生新身份，SN/IP 不合并；Task 6 共享 `#[ignore]` 用例落地后在两个数据库各按 URL 门控运行相同事务测试。
- [ ] **Step 6：小提交。** `git add crates/rsetup-controller/src/devices crates/rsetup-controller/src/api/groups.rs crates/rsetup-controller/src/api/devices.rs crates/rsetup-controller/src/api/approvals.rs crates/rsetup-controller/src/model.rs crates/rsetup-controller/src/audit.rs && git commit -m "feat(controller): manage device approvals"`。

## Task 5：管理 HTTP API 安全及字段可见性闭环

**Files:** `src/api/{mod.rs,auth.rs,users.rs,roles.rs,grants.rs,groups.rs,devices.rs,approvals.rs}`、`src/{lib.rs,error.rs}` 与模块内 router 测试。

**Interfaces:** `build_router(AppState) -> axum::Router`；成功 `{data,request_id}`，错误 `{error:{code,message_key,params},request_id}`；HTTP 路径基于经审阅的 02 `/api/v1`；服务端强制 Host/Origin/JSON/CSRF/session/改密/权限。admin-only 用户、角色、授权、组、审核；普通用户按每台设备投影。

- [ ] **Step 1：先写红 router 测试。**

```rust
#[tokio::test]
async fn invisible_and_missing_device_return_same_404() {
    let app = test_router().await;
    let a = app.clone().oneshot(req(&format!("/api/v1/devices/{}", fixture_device_hex("invisible")))).await.unwrap();
    let b = app.oneshot(req(&format!("/api/v1/devices/{}", fixture_device_hex("missing")))).await.unwrap();
    assert_eq!(a.status(), StatusCode::NOT_FOUND);
    assert_eq!(b.status(), StatusCode::NOT_FOUND);
}
#[tokio::test]
async fn approval_requires_admin_and_csrf() {
    let app = test_router().await;
    assert_eq!(send(&app, approval_req("alice", None)).await.status(), StatusCode::FORBIDDEN);
    assert_eq!(send(&app, approval_req("alice", Some("csrf"))).await.status(), StatusCode::FORBIDDEN);
    assert_eq!(send(&app, approval_req("admin", Some("csrf"))).await.status(), StatusCode::OK);
}
#[tokio::test]
async fn session_write_rate_limit_has_retry_after() {
    let app = test_router_with_clock_and_limits().await; // 未来 fixture：可控时钟，已认证会话/端点
    for _ in 0..write_limit() { assert!(send(&app, session_write_req("admin")).await.status().is_success()); }
    let write = send(&app, session_write_req("admin")).await;
    assert_eq!(write.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(write.headers().contains_key(header::RETRY_AFTER));
    assert_ne!(send(&app, valid_login_req("other-user")).await.status(), StatusCode::TOO_MANY_REQUESTS);
}
#[tokio::test]
async fn endpoint_read_rate_limit_has_retry_after() {
    let app = test_router_with_clock_and_limits().await;
    for _ in 0..read_limit() { assert!(send(&app, endpoint_read_req("/api/v1/devices")).await.status().is_success()); }
    let read = send(&app, endpoint_read_req("/api/v1/devices")).await;
    assert_eq!(read.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(read.headers().contains_key(header::RETRY_AFTER));
}
```

`test_router_with_clock_and_limits`、`session_write_req`、`endpoint_read_req`、`valid_login_req` 及阈值 helper 是未来 fixture（同首个测试落地），不是现有 API；可控时钟避免 sleep，选用明确有效的写操作/读取端点并隔离账号和配额桶。按会话写与按端点读分别单独做行为 RED→GREEN，包含超过各自窗口、等待 Retry-After 后恢复、不同会话/端点互不错误串限额；登录按账号和来源的独立限速仍由 Task 2 验证。02 §3、05 §6 提议会话写 20/s、端点读 50/s，均待审阅压测，不固化为批准值。

- [ ] **Step 2：运行确认失败。** `cargo test -p rsetup-controller invisible_and_missing_device_return_same_404`；预期未挂路由、缺鉴权或响应内容泄漏导致行为失败。另分别运行 `cargo test -p rsetup-controller session_write_rate_limit_has_retry_after` 和 `cargo test -p rsetup-controller endpoint_read_rate_limit_has_retry_after`；须从超过限额未返回 429/Retry-After 的断言看到 RED，不能以缺 helper/路由编译失败充数。
- [ ] **Step 3：最小实现。** 路由注册经批准的 auth/users/roles/grants/groups/devices/approvals；非安全方法 JSON + 精确 Host/Origin + session-bound X-CSRF-Token；登录没有先验 session 仍检查同源/JSON；无任意来源带凭据 CORS；只把已认证 principal 注入 extensions，handler 不接触 raw cookie。无 Origin 客户端须有效 session/CSRF/允许 Host，不能松绑浏览器规则。另将登录按账号/来源失败限速与管理 API 按会话写、按端点读两个配额分开；超过管理限额返回 429 `RATE_LIMITED` + `Retry-After`，阈值按获批配置。

```rust
pub fn build_router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/api/v1/auth/login", axum::routing::post(api::auth::login))
        .route("/api/v1/devices", axum::routing::get(api::devices::list))
        .route("/api/v1/approvals/approve", axum::routing::post(api::approvals::approve))
        .with_state(state)
}
```

- [ ] **Step 4：运行确认通过。** `cargo test -p rsetup-controller api::`；未登录 401、强制改密非白名单 403、错误 Host/Origin/CSRF 拒绝、合法 lower hex64 的不可见/不存在设备同 404、reboot-only 不泄漏 status/read 字段、管理员可用；GET 详情无变更副作用，列表排序以 ID 为稳定决胜键（见 02 §3）。另分别跑按会话写/按端点读限速用例，确认 429 和 `Retry-After`、窗口恢复且不误限登录。
- [ ] **Step 5：重构回归。** 未知字段/枚举及 body 超限 400；revision 409；登录按账号和来源限速 429+Retry-After，管理 API 按会话写/按端点读独立 429+Retry-After（02 §3、05 §6 建议 20/s、50/s，待审）；已撤 session 拒绝；分页 cursor 绑定用户/筛选/排序、列表无隐藏总数；批量明确清单逐项结果；全部 admin endpoints 正反授权。`cargo test -p rsetup-controller`。
- [ ] **Step 6：小提交。** `git add crates/rsetup-controller/src/api crates/rsetup-controller/src/lib.rs crates/rsetup-controller/src/error.rs && git commit -m "feat(controller): expose secured management api"`。

## Task 6：真实 MySQL/TiDB 契约、并发与最终回归

**Files:** `tests/{common/mod.rs,mysql_identity.rs,tidb_identity.rs}`、未来 `.github/workflows/ci.yml`（为两引擎真实 DB ignored 用例加显式接线）；schema 修订只新增后续 migration，不改已应用迁移。测试隔离数据库，不清空用户现存库。共享 DB 验收 helper 放 `tests/common/mod.rs`，`mysql_identity.rs`、`tidb_identity.rs` 各调用一次；均为 `#[ignore]`，按对应 URL 显式运行。CI 服务/镜像与精确版本须未来审批与配置，本次不修改 CI。

- [ ] **Step 1：先写红 DB 测试。**

```rust
#[tokio::test]
#[ignore] // 显式 DB URL 门控的共享测试
async fn concurrent_decision_has_one_winner_one_audit() {
    let db = required_test_db().await;
    let (a, b) = tokio::join!(approve(&db, admin(), d("A"), 7),
                              revoke(&db, admin(), d("A"), 7));
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(device_revision(&db, d("A")).await, 8);
    assert_eq!(decision_audit_count(&db, d("A")).await, 1);
}
#[tokio::test]
#[ignore] // 显式 DB URL 门控的共享测试
async fn failed_audit_rolls_back_grant_and_epoch() {
    let db = required_test_db().await;
    let epoch = authz_epoch(&db).await;
    inject_audit_failure(&db).await;
    assert!(add_grant(&db, admin(), sample_grant()).await.is_err());
    assert_eq!(authz_epoch(&db).await, epoch);
    assert!(!grant_exists(&db, sample_grant()).await);
}
```

`tests/common/mod.rs` 增加共享 DB 测试 helper：初始化并发只一 admin/一次输出、最后管理员并发停用/降级保护（不能只用 Task 2 fake 测试）、revision CAS 同预期只一胜者、审计失败时状态/决定/授权/epoch 回滚；`mysql_identity.rs` 与 `tidb_identity.rs` 分别运行同一组真实 DB 用例，另测唯一性及关联一致性（DB-01）。

- [ ] **Step 2：运行确认失败。** URL 由未来本地隔离容器/获批 CI service 显式提供；存在 `$MYSQL_TEST_URL` 时运行 MySQL：`CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test -p rsetup-controller --test mysql_identity -- --ignored --test-threads=1`；存在 `$TIDB_TEST_URL` 时运行 TiDB：`CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test -p rsetup-controller --test tidb_identity -- --ignored --test-threads=1`。分别核查实际执行用例数非零，零用例非 PASS；缺某引擎 URL 标「未验证」（不记 PASS/FAIL、不因缺 URL 阻塞普通 CI）；URL 已提供但测试失败或零用例则 CI fail，mock 测试不得冒充 DB 验收；红灯须来自真实未完成保证而非环境误设。
- [ ] **Step 3：最小修复。** 按真实失败补 revision CAS 的受影响行数检测、唯一冲突映射、事务重试边界与部分 DDL 恢复/清晰报错；重试不重复密码输出/事件或设备网络。

```rust
let changed = sqlx::query("UPDATE devices SET revision=revision+1 WHERE public_key=? AND revision=?")
    .bind(key).bind(expected).execute(&mut *tx).await?.rows_affected();
if changed != 1 { return Err(ControllerError::RevisionConflict); }
```

- [ ] **Step 4：运行确认通过。** 按 Step 2 的显式 URL 命令在 MySQL 与 TiDB 各跑一次共享 DB 测试，并检查每引擎实际执行用例数非零后才分别记录精确 server/version/driver 与 PASS；缺 URL 的引擎标「未验证」，URL 已提供但失败或零用例标「失败」并使 CI fail，不宣称两库兼容。
- [ ] **Step 5：重构回归。** 未来在 `.github/workflows/ci.yml` 明确按 `$MYSQL_TEST_URL`/`$TIDB_TEST_URL` 分别设置 `CONTROLLER_TEST_DATABASE_URL`，运行 `cargo test -p rsetup-controller --test mysql_identity -- --ignored --test-threads=1` / `cargo test -p rsetup-controller --test tidb_identity -- --ignored --test-threads=1`，解析各引擎实际执行用例非零；缺 URL 不运行并报告「未验证」而非 PASS，有 URL 时命令失败或零用例使 CI fail。所需 service/版本在获批实施时配置，不假设现有 CI 已提供；`cargo test --workspace --locked` 仍不覆盖 ignored DB。另运行 `cargo fmt --all -- --check && cargo test --workspace --locked && cargo clippy --workspace --all-targets -- -D warnings`。核对 git diff 白名单，确认 `crates/rsetup-app/src/server.rs`、`docs/protocol_spec.md` 与前端无变更；复查 AUTH-01/02、ACL-01/02、ADMIN-01、DB-01、API-01、ADM-01。任务/SSE/握手仍属其他计划。
- [ ] **Step 6：小提交。** `git add crates/rsetup-controller/tests crates/rsetup-controller/migrations crates/rsetup-controller/src .github/workflows/ci.yml && git commit -m "test(controller): verify mysql and tidb contracts"`（src 以 Step 3 实际改动为准，CI 仅未来授权后改动）；交付说明列实测版本、安全审阅及未验证项。

## 验收映射与条件性参数

Tasks 1–5 覆盖 C-ARCH/C-DATA/C-AUTH/C-ACL 及 C-ADMIT 的中控侧（准入决定与记录）；C-ARCH 由设计评审与部署形态确认，不作为功能验收项，C-ADMIT 的连接/期限语义属 02/05。Tasks 1–2、5 对应 AUTH-01/02；Tasks 3、6 覆盖 ACL-01、ADMIN-01 及 ACL-02 的授权存储/epoch 部分；ACL-02（与 03 计划 Task 2/3 联合验收）的组变更与任务提交并发、执行前重验和已发任务的撤权边界不属 01 单独验收。Tasks 1、3、4、6 对应 DB-01；Tasks 4–6 对应 API-01/ADM-01。ACL-03（普通用户任务汇总与 SSE 隐藏设备/计数、撤权后排队消息不再发出，规格 01 §8）的任务/SSE 侧与 03 计划 Task 5 联合验收，本计划保证服务层 visible_devices 与 authz_epoch 失效，不单独宣称 ACL-03 完成。AT-01←AUTH-01；AT-02←AUTH-02/ADMIN-01；AT-03←ACL-01..03（本计划承担 ACL-01、ACL-02 授权侧与最低识别；ACL-02 任务侧、ACL-03 任务/SSE 侧联合 03）；AT-14←DB-01。AT-01..03 与 04 计划联合验收（见 04 计划覆盖/退出关卡）。网络 dispatch 撤权、task/SSE、传输握手安全和前端不属本计划，不可暗示完成。

审阅前不得固化 Argon2id `65536 KiB/3/1`、密码 12–128 字符/512 UTF-8 bytes、session 30 min/12 h、登录 15 min/5 次、cookie/CSRF/Host/Origin 细节、内置角色、MySQL 8.4 LTS/TiDB 8.5 LTS、审批最大 1024、分页 50/max 200、body ≤1 MiB、draft-1 API 字段、SQLx/hash/random crate 版本。它们是拟议值，不是已批准实施决策。

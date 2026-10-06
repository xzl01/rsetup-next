# 计划：Controller Auth 生产 SQLx IdentityRepository + AuthService 快照门禁 / 固定脱敏审计（2026-10-03）

## Goal

在 `rsetup-controller` 内为四个 auth 端点（登录/鉴权/改密/登出）提供**生产 SQLx 仓储**与
`AuthService` 快照门禁：登录成功后由 AuthService 同次验证的 `IdentityUser`（含
password_hash/revision）传入仓储，仓储在事务内于 users 锁下重新核验后才插入 session；
全部 auth 事件只写**固定脱敏审计 insert**（无明文、URL、token、digest）。
**范围外**：HTTP 路由/中间件、端点 handler 与错误映射、前端、速率限制器、真实数据库执行
（另计划，需用户核对目标库+原备份/迁移门禁后单独批准）。

## Arch

- 分层不变：HTTP（未来）→ `AuthService<R: IdentityRepository>`（纯业务，`src/auth/service.rs`）
  → `IdentityRepository` trait → 生产实现 `SqlxIdentityRepository`（新文件 `src/auth/sqlx_repo.rs`，持
  `DbPool`）与测试用 `FakeRepo`（留在 `service.rs` tests 内）。
- 仓储是**事务边界**：每个写操作 `pool.begin()` → `integrity::lock_integrity_guard`
  （`src/integrity.rs:117`，schema_meta 行锁 + 版本校验）→ users `FOR UPDATE` 锁内重验快照
  → 写 sessions/users → 固定审计 insert（同事务）→ commit；失败整体回滚。
- 内存侧 `SessionClock`（epoch 绑定、空闲/绝对上限）语义不变；`process_epoch` 进程级
  `OnceLock<uuid::Uuid>` 与 `db.rs:2679` 既有模式一致。
- 审计**不新建框架**：复用 `audit_events` 表与既有 insert 形态（参考 `db.rs:2724` 字段序
  `id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted, outcome,
  time_evidence, process_epoch, event_seq`），只新增 auth 固定事件值；**不得**把既有 CAS
  （admission）审计 insert 当作 auth 审计等价物或复用其 `audit_code`。

## Tech

- 纯 SQLx（`sqlx::query` + `Row::try_get`），MySQL；布尔列严格解码，遵循 `integrity.rs` 的
  `CAST(... AS SIGNED)` + 严格布尔校验模式（非 0/1 值 → 错误，失败关闭）。
- `sessions.created_time` 用固定 SQL `UTC_TIMESTAMP(6)` 写 `DATETIME(6)`；审计 JSON `time_evidence` 单独使用 `system_fallback_time_evidence()`，序号单独使用 `next_event_seq(&AUTH_EVENT_SEQ)`，其中 `AUTH_EVENT_SEQ` 是 auth 仓储私有原子计数器、搭配其私有随机进程 epoch。这两个现有 helper 在 `db.rs` 当前为私有函数，Task 2 只把它们改为 `pub(crate)` 供 auth 仓储调用，不更改实现、CAS 的既有 epoch/seq 或其审计行为。
- 实现集中在 `src/auth/sqlx_repo.rs`；Task 1 修改 `service.rs` 的快照接口；Task 2 在 `db.rs` 仅将现有两个纯审计 helper 收窄开放为 `pub(crate)`，不改它们的行为或 CAS 路径。

## Spec 锚点

- 01 规格 `docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md` L18–34：A-02
  改密撤销全部会话、must_change_password；A-03 token 只存 SHA-256 摘要、epoch 绑定、token
  不入 URL/日志；登录失败对不存在账号执行 dummy Argon2id 校验、同样失败形态。
- 02 规格 `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md`：L37–47 users/sessions
  表结构与生产会话创建契约——guard→users 锁内重验 active 与登录已验证的 hash/revision，改密/重置
  同事务撤销 sessions，**事务外已验密码不能单独授权新会话**；L81 必审事件最小集，params_redacted
  不记密码/token；L105–110 四 auth 端点契约（本计划仅覆盖其仓储/服务侧）。
- 迁移 `crates/rsetup-controller/migrations/0003_identity_application_integrity.sql` L1–17：
  `users`/`sessions` 真实列（token_hash PK、user_id、process_epoch、created_time、revoked）。

## Security prerequisites for Task 1

- `AuthService::new(repo)` becomes `Result<Self, ControllerError>`: construct one dummy Argon2id hash at startup with the same parameters used for real users. All existing fake tests/callers must handle the result. Unknown username runs exactly one `verify(password, &dummy_hash)`; no per-attempt `hash+verify` timing difference. If dummy initialization fails, service startup fails closed.
- `record_login_failure` takes `Option<[u8;16]>` rather than an untrusted username; audit fields are fixed and sanitized. Unknown account and invalid password/inactive known account must each call it; if auditing fails, return a fixed service error, never claim an audit record was written.

## Global constraints

1. 本任务**不触真实数据库**：SQLx 实现只要求编译通过 + 可运行的纯内存（fake）行为测试；
   真实 DB 验证在用户核对指定目标库 + 原备份/迁移门禁后另行批准执行。
2. 只写本计划所规划的实现；**禁止**超出下述清单新增文件、禁止 secret 读取/回显、禁止联网、禁止 commit、禁止派代理。
3. 审计 insert 一律固定字段与固定事件值：`params_redacted = '{}'`，不含密码明文、URL、raw token、token digest、username 原文；target_id 仅允许 user id 的 hex 或 NULL。
4. 所有 RED→GREEN 循环使用统一命令（在 worktree 根目录执行）：

   ```bash
   CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
     cargo +1.85.0 test --offline --locked -p rsetup-controller
   ```

5. 纯 SQLx 仓储**没有旧生产仓库可对照**：先落可编译 stub（签名正确、行为返回
   `ControllerError::InvalidArgument`），用行为测试打 RED；**编译失败不计为测试失败**，不得用 `#[ignore]` 掩盖。
6. 每个任务完成后做一次**独立审查**（由未参与该任务实现的审查方，仅读 diff 与本计划，
   对照审查清单输出结论，不允许"顺带修"）。


## Task 1：`IdentityRepository::insert_session` 改接 `&IdentityUser` + AuthService 快照门禁 + 登录失败 dummy 校验

**文件**：`crates/rsetup-controller/src/auth/service.rs`（唯一改动文件）。

**接口变更**（trait 与 `FakeRepo` 同步改签名）：

```rust
fn insert_session(
    &self,
    user: &IdentityUser,          // 原 user_id: [u8; 16]
    digest: [u8; 32],
    epoch: [u8; 16],
) -> impl Future<Output = Result<(), ControllerError>> + Send;
```

新增第 6 个最小 trait 方法（登录失败固定审计，无事务写）：

```rust
fn record_login_failure(
    &self, user_id: Option<[u8; 16]>, // 未知账号为 None；不传/持久化用户名
) -> impl Future<Output = Result<(), ControllerError>> + Send;
```

**行为（RED）**：先加测试，再放可编译 stub（`insert_session` 忽略 `user` 参数直接成功）→ 行为 RED：

```rust
#[tokio::test]
async fn stale_login_snapshot_never_creates_session() {
    let (svc, repo) = auth_fixture();
    let stale = repo.state.lock().unwrap().0.clone();
    repo.state.lock().unwrap().0.revision = stale.revision + 1;
    let result = repo.insert_session(&stale, [7; 32], svc.clock.epoch).await;
    assert!(matches!(result, Err(ControllerError::RevisionConflict)));
    assert!(repo.state.lock().unwrap().1.is_empty());
}
#[tokio::test]
async fn unknown_bad_password_and_inactive_user_record_only_anonymous_failures() {
    let (svc, repo) = auth_fixture();
    assert!(matches!(svc.login("ghost", "wrong").await, Err(ControllerError::InvalidArgument)));
    assert!(matches!(svc.login("alice", "wrong").await, Err(ControllerError::InvalidArgument)));
    repo.state.lock().unwrap().0.active = false;
    assert!(matches!(svc.login("alice", "old password").await, Err(ControllerError::InvalidArgument)));
    assert_eq!(*repo.failures.lock().unwrap(), vec![None, Some([1; 16]), Some([1; 16])]);
    assert!(repo.state.lock().unwrap().1.is_empty());
}
#[tokio::test]
async fn unknown_account_really_runs_dummy_argon2_verify() {
    let (mut svc, _) = auth_fixture();
    svc.dummy_hash = "synthetic-invalid-phc".into(); // 仅测试内部故障注入，无真实密码
    assert!(matches!(svc.login("ghost", "wrong").await, Err(ControllerError::Crypto)));
}
#[tokio::test]
async fn audit_write_failure_is_not_swallowed_as_invalid_credentials() {
    let (svc, repo) = auth_fixture();
    repo.audit_error.store(true, std::sync::atomic::Ordering::SeqCst);
    let result = svc.login("ghost", "wrong").await;
    assert!(matches!(result, Err(ControllerError::Config(ref message)) if message == "audit unavailable"));
    assert!(repo.state.lock().unwrap().1.is_empty());
}
#[tokio::test]
async fn inactive_current_row_rejects_stale_active_snapshot() {
    let (svc, repo) = auth_fixture();
    let stale = repo.state.lock().unwrap().0.clone();
    repo.state.lock().unwrap().0.active = false;
    assert!(matches!(repo.insert_session(&stale, [8; 32], svc.clock.epoch).await,
        Err(ControllerError::InvalidArgument)));
    assert!(repo.state.lock().unwrap().1.is_empty());
}
```

测试夹具 `FakeRepo` 在 Task 1 新增 `failures: Mutex<Vec<Option<[u8;16]>>>` 与 `audit_error: AtomicBool`（`auth_fixture` 分别初始化空 Vec / false）；可编译 `record_login_failure` 桩先返回 Ok 但不记录，第一测试因期望三条失败事件得到空 Vec 而行为 RED，随后最小 GREEN 才记录纯 user id/None。stub 忽略 `&IdentityUser` 直接插入时首测和停用快照测试均 RED。损坏 dummy PHC 的故障注入使旧未知账号快返 InvalidArgument、新代码必须真正调用 Argon2 verify 并固定返回 Crypto；此 Crypto 不是错误密码，不记失败审计，不返回 raw token。审计写失败另用 FakeRepo 可控 `audit_error` 返回固定 ControllerError::Config("audit unavailable")，断言 login 不再返 InvalidArgument，也不创建 session。暂停竞争测试如增加，只在 FakeRepo::insert_session 获取当前行锁前以 oneshot/barrier 放行更新快照，不能让额外线程在持锁时等待，避免伪死锁；不把没有暂停钩子的测试算 RED。

**GREEN**：`AuthService::new(repo) -> Result<Self, ControllerError>` 启动时仅生成一次合法 dummy Argon2id hash，测试夹具和后续 HTTP 组装改用 `?`/`unwrap()`；`login` 的成功路径仍把同次验证的 `user` 快照交给仓储。未知、错误密码与 inactive 以同一固定错误/审计形状拒绝：

```rust
let found = self.repo.find_user(username).await?;
let valid = match &found {
    Some(user) => self.hasher.verify(password, &user.password_hash)?,
    None => self.hasher.verify(password, &self.dummy_hash)?,
};
if !valid || !found.as_ref().is_some_and(|user| user.active) {
    self.repo.record_login_failure(found.as_ref().map(|user| user.id)).await?;
    return Err(ControllerError::InvalidArgument);
}
let user = found.ok_or(ControllerError::InvalidArgument)?;
// 仅在此处分配raw token；不可在失败审计之前创建会话或回显token。
// 后续计算digest后：
self.repo.insert_session(&user, digest, self.clock.epoch).await?;
```

`FakeRepo::insert_session` 在锁内重验**当前行**：`state.0.id == user.id && state.0.active && user.active && state.0.revision == user.revision && state.0.password_hash == user.password_hash`；当前已停用（即使快照仍 active）→ `InvalidArgument`，hash/revision 漂移→`RevisionConflict`，均不追加 session。未知用户名仅走一次 dummy verify；失败审计返回错误时固定服务失败，绝不吞错声称已记审计。

**验证**：Global 4 命令，新增快照、当前停用、失败审计、审计写失败传播与 dummy verify 测试各自按行为先 RED 后 GREEN，既有 auth 测试全绿。
**独立审查清单**：① trait 五+一方法签名与 FakeRepo/SQLx 实现一致；② 无测试绕过 dummy 校验路径；
③ `service.rs` 外无改动；④ 无日志/审计打印 token、digest、password、username 原文。


## Task 2：`SqlxIdentityRepository` find/insert/revoke（纯 SQLx 编译 + 可运行 decode 测试）

**文件**：新增 `crates/rsetup-controller/src/auth/sqlx_repo.rs`；修改 `src/auth/mod.rs` 导出生产仓储；仅将 `src/db.rs` 现有 `system_fallback_time_evidence` 和 `next_event_seq` 改为 `pub(crate)`，不改既有行为。

**RED**：先建可编译 stub（方法全部返回 `InvalidArgument`）+ 两条**无 DB 可运行**的 decode/契约测试：

```rust
#[test]
fn scan_revoked_strict_boolean() { assert!(scan_revoked(2u8).is_err()); } // 0/1=>Ok，其余 Err
#[test]
fn session_join_contract_sql() {
    // WHERE仅按token_hash/process_epoch精确定位；投影CAST(s.revoked AS SIGNED)，
    // 由严格布尔解析拒绝2/-1，不在SQL WHERE用 revoked=FALSE 隐藏污染。
}
```

**GREEN**：关键 SQL（生产形态，本任务只要求编译与上述可运行测试通过）：

```rust
// find_session：digest/epoch/!revoked join users，严格 active/boolean（见 RED 契约）
// SELECT u.id,u.username,u.display_name,u.password_hash,
//        CAST(u.active AS SIGNED) AS active, CAST(u.is_admin AS SIGNED) AS is_admin,
//        CAST(u.must_change_password AS SIGNED) AS mcp, u.revision,
//        CAST(s.revoked AS SIGNED) AS revoked
// FROM sessions s JOIN users u ON u.id=s.user_id
// WHERE s.token_hash=? AND s.process_epoch=?
// 对 revoked、active、is_admin、mcp 分别严格只收0/1；revoked!=0或active!=1拒绝，
// 不能在 WHERE 先筛掉 revoked=2 而默默将污染当作不存在。

// insert_session（事务内快照门禁）
let mut tx = self.pool.0.begin().await?;
crate::integrity::lock_integrity_guard(&mut tx).await?;
// SELECT ... FROM users WHERE id = ? FOR UPDATE → 严格解码，active 必须 true，
// 且 password_hash/revision 与传入快照一致，否则回滚
sqlx::query("INSERT INTO sessions (token_hash,user_id,process_epoch,created_time,revoked) \
             VALUES (?, ?, ?, UTC_TIMESTAMP(6), FALSE)")
    .bind(digest.as_slice()).bind(user.id.as_slice()).bind(epoch.as_slice())
    .execute(&mut *tx).await?;
// 固定审计 insert（auth.login.succeeded，同事务）→ commit
// revoke_session：UPDATE sessions SET revoked = TRUE WHERE token_hash = ?
// rows_affected==1 才记 auth.logout 审计；0 行仍 Ok（幂等，无审计）
```

**验证**：Global 4 命令，crate 编译通过、两条 decode/契约测试 GREEN、既有测试不回归。
**不得**在本任务连接任何数据库；真实 DB 调用只能留在 `#[ignore]` 测试门后（注释注明"另行批准后启用"）。

**独立审查清单**：① 每个写事务均以 `lock_integrity_guard` 开头且 commit/回滚完整；
② `find_session` 返回的快照绝不用于授权新会话（契约 L47）；
③ 无 `println!`、无 token/digest 进入 SQL 字面量或错误信息；④ 事件值/字段为 auth 独立固定集合。


## Task 3：`change_password` 事务 + auth 固定脱敏审计全量枚举

**文件**：`crates/rsetup-controller/src/auth/sqlx_repo.rs`（实现）+ `service.rs` tests
（FakeRepo 增加审计记录断言，RED 载体）。

**auth 固定审计事件表**（本任务唯一新增的"审计面"，全部 `params_redacted='{}'`、
`target_kind='user'`，不新建框架）：

| event_type | 触发 | actor | target_id | outcome |
| --- | --- | --- | --- | --- |
| `auth.login.succeeded` | insert_session 提交 | user | user id hex | success |
| `auth.login.failed` | record_login_failure（含未知账号） | system | NULL | failure |
| `auth.logout` | revoke_session 命中 1 行 | user | user id hex | success |
| `auth.password.changed` | change_password 提交 | user | user id hex | success |
| `auth.session.revoked` | change_password 内撤销该用户全部 sessions 行数>0 | user | user id hex | success |

（02 规格 L81 "停用/重置/重启"引发的会话撤销属管理员/运维路径，本计划不覆盖其入口，仅保证消费同一 `auth.session.revoked` 事件值。）

**change_password 事务顺序**（trait 已收 `&Session`，快照即登录/鉴权时刻的 user 行）：

```text
begin → lock_integrity_guard
→ users WHERE id = session.user.id FOR UPDATE：严格布尔解码、当前 active，重验
   u.password_hash/revision == 传入已鉴权快照；不一致先回滚（绝不锁较低序的其他对象）
→ sessions WHERE token_hash = ? FOR UPDATE：严格检查 user_id==u.id、!revoked、process_epoch==epoch；
   session缺失、已撤、脏布尔或用户错配均 fail closed，不延长内存期限
→ UPDATE users SET password_hash=?, revision=revision+1, must_change_password=FALSE
   WHERE id=? AND revision=?（带原 revision）；rows_affected != 1 => 回滚（RevisionConflict）
→ UPDATE sessions SET revoked = TRUE WHERE user_id = ?（撤销该用户全部）
→ 固定去敏审计 auth.password.changed（+ auth.session.revoked 若撤销行数>0）→ commit
```

**RED**：FakeRepo 增加 `audits: Mutex<Vec<&'static str>>`；测试
`change_password_writes_fixed_audit_and_revokes_all`、`logout_writes_fixed_audit`、
`login_failure_audits_without_username`（断言事件序列/固定值、审计串不含 digest hex 片段）在 stub 下 RED。

**GREEN**：按上表实现固定 insert（逐条 `sqlx::query`，字段序与 `db.rs:2724` 一致），FakeRepo 同步记录事件使测试 GREEN。
**验证**：Global 4 命令全绿（含 Task 1/2 测试）；`grep` 自检：`sqlx_repo.rs` 字符串字面量不含
`raw_token`/`hex::encode(digest`/密码变量名。

**独立审查清单**：① 事件表五值与 spec L81 最小集对应、无第 6 个隐式事件；
② change_password 锁序 guard→user→session 与 01 §5/02 §2.1 一致，且失败不能撤销另一次改密产生的新会话；
③ 审计不携带 username/参数明细；④ 未触碰 CAS 审计（`db.rs` admission 路径零 diff）。


## 验收与后续门禁

- 本计划完成 = Global 4 命令全绿 + 三个任务审查结论通过；产物**未连接任何数据库、未写 HTTP 路由/前端**。
- 后续（不在本计划）：真实目标库核对 + 原备份/迁移门禁 → 单独批准后启用 `#[ignore]` 真实 DB 门；
  四 auth 端点 HTTP handler/限速/CSRF 另立计划（用户已批准业务侧范围，本计划仅仓储+服务层）。

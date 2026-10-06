# 计划：Task 3 — SQLx change_password 写事务（固定脱敏审计）（2026-10-03）

> 致执行代理：需 superpowers:executing-plans 或 subagent-driven-development 逐项落实，用 `- [ ]` 跟踪。

**Goal:** 为 `SqlxIdentityRepository::change_password` 落实改密写事务（**仅此方法**；`revoke_session` 维持固定拒绝、`main` 维持 health-only）。顺序 begin→guard→users `FOR UPDATE` 严格重验当前行（active/id 优先于 hash/revision，须与 `Session.user` 快照一致）→sessions `token_hash FOR UPDATE` 严格 `user_id`/`epoch`/`revoked==0`→`revision` checked_add→`UPDATE users`（hash/revision/must_change_password=FALSE，rows_affected==1）→严格读该用户全部 sessions（raw bool 无污染）再撤销全部→固定 `auth.password.changed`（+`auth.session.revoked` 仅有撤销行时）→同事务 CAS 递增 `schema_meta.authz_epoch`→commit；任一失败/审计错误整体回滚、固定去敏。**不宣称**真实库原子性/行锁顺序/回滚一致性已验收。

**Architecture:** 事务边界同 `insert_session`：`db.0.begin()`→`integrity::lock_integrity_guard`（integrity.rs:117）→写→固定审计（同事务）→commit；失败丢弃 `tx`（回滚）；驱动错误一律映射固定去敏 `Config`。复用既有 `validate_current_user`/`strict_bool`/`insert_audit`/`write_error`/`decode_user`/`scan_user_fields`，新增 5 个纯函数与 6 条 SQL 常量。

**Tech Stack:** 纯 SQLx（`sqlx::query` + `Row::try_get`），MySQL；布尔列 `CAST(... AS SIGNED)` + 严格 0/1 解码；离线编译 + 纯内存行为测试。

**Spec:** `specs/2026-09-23-controller-v1-01-identity-access.md` §2/§5、`specs/2026-09-23-controller-v1-02-data-api.md` §2.1/§2.3/§4、`migrations/0003_identity_application_integrity.sql`、`plans/2026-10-03-controller-auth-repository.md` Task 3（事件表 + 事务顺序）。

## Global Constraints

1. **不触真实数据库**：仅要求编译通过 + 可运行纯内存测试；真库验证在确认目标库 + 备份/迁移门禁后另批。
2. 只写本计划规划的实现；禁新增文件、禁 secret 读取/回显、禁联网、禁 commit、禁派代理；`service.rs` 零改动（trait/`AuthService` 已就绪）。
3. 审计固定字段、固定事件值：`params_redacted='{}'`，不含密码/URL/raw token/digest/username 原文；`target_id` 仅 user id hex 或 NULL。
4. RED→GREEN 统一命令（worktree 根目录）：`CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo +1.85.0 test --offline --locked -p rsetup-controller`。
5. 锁序（01 §5/02 §2.1）：guard(schema_meta)→users(id)→sessions；sessions 先按 `token_hash` 点锁当前会话、再按 `user_id` 范围 `ORDER BY token_hash FOR UPDATE`；users 行锁先行 ⇒ 同用户写串行，无跨事务死锁。

## 锚点（已核，无阻断）

02 §2.1 L47（guard→users 锁内重验、改密同事务撤销 sessions、同类按字节序）与 §2.3 L81（必审最小集含改密与会话撤销，params 不记密码）；01 §2 L18（A-02 改密撤销全部会话、must_change_password）与 §5 L61（先锁 schema_meta 再 users→…→依赖行，同类按 ID 排序）；迁移 0003 L6–17 users/sessions 真实列、L55–63 audit_events；仓储计划 Task 3 L218–260 事件表与顺序；integrity.rs:117–131 guard。

## Task：change_password 事务 + auth 固定脱敏审计

**Files:** Modify: `crates/rsetup-controller/src/auth/sqlx_repo.rs`（唯一实现文件）。

- [ ] **Step 1: RED 载体（可编译桩，保证测试 RED 而非编译失败）**

新增 SQL 常量与纯函数桩（`wrong_audit` 仅 RED 期存在，Step 3 删除）：

```rust
const LOCK_SESSION_SQL: &str = "SELECT user_id, CAST(revoked AS SIGNED) AS revoked,
    process_epoch FROM sessions WHERE token_hash = ? FOR UPDATE";
const UPDATE_USERS_SQL: &str = "UPDATE users SET password_hash = ?, revision = ?,
    must_change_password = FALSE WHERE id = ? AND revision = ?";
const SELECT_USER_SESSIONS_SQL: &str = "SELECT token_hash, CAST(revoked AS SIGNED) AS revoked
    FROM sessions WHERE user_id = ? ORDER BY token_hash FOR UPDATE";
const REVOKE_USER_SESSIONS_SQL: &str = "UPDATE sessions SET revoked = TRUE WHERE user_id = ?";
const READ_AUTHZ_EPOCH_SQL: &str = "SELECT authz_epoch FROM schema_meta WHERE singleton = 1";
const BUMP_AUTHZ_EPOCH_SQL: &str = "UPDATE schema_meta SET authz_epoch = ? WHERE singleton = 1 AND authz_epoch = ?";
fn next_authz_epoch(_current:u64) -> Result<u64,ControllerError> { Err(ControllerError::RevisionConflict) } // 可编译RED桩
fn wrong_audit() -> AuditInsert { AuditInsert { actor_kind:"wrong", actor_user_id:None,
    event_type:"wrong", target_kind:None, target_id:None, params_redacted:"wrong", outcome:"wrong" } }
fn password_changed_audit(_user_id: [u8;16]) -> AuditInsert { wrong_audit() }
fn session_revoked_audit(_user_id: [u8;16]) -> AuditInsert { wrong_audit() }
fn next_password_revision(_r: u64) -> Result<u64, ControllerError> { Err(ControllerError::RevisionConflict) }
fn validate_change_password_session(_owner:[u8;16], _exp:[u8;16], _se:[u8;16], _ee:[u8;16], _revoked:i64)
    -> Result<(), ControllerError> { Err(ControllerError::InvalidArgument) }
```

`change_password` 暂维持 `writes_not_ready()` 桩。既有 `write_methods_refuse_fixed_config_without_sql` 拆为仅断言 `revoke_session` 固定拒绝；`offline_pool()` 只能用于**触池前直返**的 `revoke_session` 用例。

- [ ] **Step 2: 行为 RED + SQL 结构守卫（真实断言；空体 GREEN 无效）**

```rust
#[test] fn change_password_session_strict() { // session owner/epoch/revoked 污染
  let e = [9u8;16];
  assert!(matches!(validate_change_password_session([1;16],[2;16],e,e,0), Err(ControllerError::InvalidArgument))); // owner 错配
  assert!(matches!(validate_change_password_session([1;16],[1;16],[1;16],e,0), Err(ControllerError::InvalidArgument))); // epoch 错配
  assert!(matches!(validate_change_password_session([1;16],[1;16],e,e,1), Err(ControllerError::InvalidArgument))); // 已撤
  for raw in [2i64,-1] { assert_fixed_redacted_config(&validate_change_password_session([1;16],[1;16],e,e,raw).unwrap_err(), &[]); } // 污染->固定 Config
  assert!(validate_change_password_session([1;16],[1;16],e,e,0).is_ok()); // 干净 -> RED(桩恒 Err)
}
#[test] fn revision_overflow_checked_add() { // u64::MAX 溢出
  assert!(matches!(next_password_revision(u64::MAX), Err(ControllerError::RevisionConflict)));
  assert_eq!(next_password_revision(5).unwrap(), 6); // RED(桩恒 Err)
}
#[test] fn authz_epoch_overflow_checked_add() {
  assert!(matches!(next_authz_epoch(u64::MAX), Err(ControllerError::RevisionConflict)));
  assert_eq!(next_authz_epoch(7).unwrap(), 8); // RED(桩恒Err)，既有validate_current_user路径不改
}
#[test] fn password_changed_audit_pinned() { let a = password_changed_audit([7;16]); // RED(wrong_audit)
  assert_eq!(a.actor_kind,"user"); assert_eq!(a.actor_user_id,Some([7;16]));
  assert_eq!(a.event_type,"auth.password.changed"); assert_eq!(a.target_kind,Some("user"));
  assert_eq!(a.target_id.as_deref(),Some(hex::encode([7;16]).as_str()));
  assert_eq!(a.params_redacted,"{}"); assert_eq!(a.outcome,"success"); }
#[test] fn session_revoked_audit_pinned() { let a = session_revoked_audit([7;16]); // 同上，event_type "auth.session.revoked"
  assert_eq!(a.event_type,"auth.session.revoked"); assert_eq!(a.actor_kind,"user");
  assert_eq!(a.target_id.as_deref(),Some(hex::encode([7;16]).as_str())); assert_eq!(a.outcome,"success"); }
#[test] fn change_password_sql_gate_fixed_strings() { // GREEN-by-construction，不计 RED
  let norm = |s:&str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_uppercase();
  let compact = |s:&str| s.replace(char::is_whitespace,"").to_ascii_uppercase();
  assert!(norm(LOCK_SESSION_SQL).ends_with("FOR UPDATE")); assert_eq!(LOCK_SESSION_SQL.matches('?').count(),1);
  assert!(compact(UPDATE_USERS_SQL).contains("MUST_CHANGE_PASSWORD=FALSE") && compact(UPDATE_USERS_SQL).contains("WHEREID=?ANDREVISION=?")); assert_eq!(UPDATE_USERS_SQL.matches('?').count(),4);
  assert!(compact(SELECT_USER_SESSIONS_SQL).contains("ORDERBYTOKEN_HASHFORUPDATE")); assert_eq!(SELECT_USER_SESSIONS_SQL.matches('?').count(),1);
  assert!(compact(REVOKE_USER_SESSIONS_SQL).contains("SETREVOKED=TRUEWHEREUSER_ID=?")); assert_eq!(REVOKE_USER_SESSIONS_SQL.matches('?').count(),1);
  assert!(compact(READ_AUTHZ_EPOCH_SQL).contains("FROMSCHEMA_METAWHERESINGLETON=1"));
  assert!(compact(BUMP_AUTHZ_EPOCH_SQL).contains("WHERESINGLETON=1ANDAUTHZ_EPOCH=?"));
  assert_eq!(BUMP_AUTHZ_EPOCH_SQL.matches('?').count(), 2);
  for s in [LOCK_SESSION_SQL,UPDATE_USERS_SQL,SELECT_USER_SESSIONS_SQL,REVOKE_USER_SESSIONS_SQL,READ_AUTHZ_EPOCH_SQL,BUMP_AUTHZ_EPOCH_SQL] { assert!(!s.contains(';')); }
}
```

真正 RED 仅前五项（复用既有 `assert_fixed_redacted_config`）；SQL 结构守卫 Step 1 已正确即 **GREEN-by-construction，不计 RED，不能据以推断事务锁序**。**current row 漂移/停用复用既有 `validate_current_user`**（已 GREEN，`snapshot_validation_pure` 覆盖），change_password 直接消费、不新增重复 RED。

- [ ] **Step 3: GREEN 实现**

1. `next_password_revision`：`revision.checked_add(1).ok_or(ControllerError::RevisionConflict)`。
2. `validate_change_password_session`：先 `strict_bool(revoked,"sessions.revoked")?` 拒污染（固定 Config）；`revoked!=0`→`InvalidArgument`（已撤）；`owner!=exp`→`InvalidArgument`；`se!=ee`→`InvalidArgument`；否则 Ok。
3. 两个审计投影按 Step 2 固定值；删除 `wrong_audit()`，不留私有死代码。
4. `change_password` 事务（驱动错误固定码；失败丢弃 `tx` 回滚）：begin→guard→`READ_AUTHZ_EPOCH_SQL` 在已锁 guard 下 `fetch_one` 并严格 `try_get::<u64>`、`next_authz_epoch`（溢出先拒，不能更改账号）→`LOCK_USER_SQL`(bind `session.user.id`)→`scan_user_fields`+`decode_user`→`validate_current_user(Some(&cur), &session.user)`→`LOCK_SESSION_SQL`(bind `session.digest`)：缺行→固定 `InvalidArgument`，有行解 `user_id`/`revoked`/`process_epoch`（BINARY16 长度严格）→`validate_change_password_session(owner,session.user.id,stored_epoch,epoch,revoked)`→`next_password_revision(session.user.revision)`→`UPDATE_USERS_SQL`(bind next_hash/next_revision/id/old_revision)，`rows_affected!=1`→`RevisionConflict`→`SELECT_USER_SESSIONS_SQL`(bind id)：逐行 `strict_bool` 拒污染、计 `to_revoke`(revoked==0)→`REVOKE_USER_SESSIONS_SQL`(bind id)→`insert_audit(password_changed_audit(id))` +（`to_revoke>0` 时）`insert_audit(session_revoked_audit(id))`→`BUMP_AUTHZ_EPOCH_SQL` bind(next_epoch,old_epoch)，rows_affected!=1→`RevisionConflict`→commit。错误码涵盖 `tx.begin`/`epoch.read`/`epoch.decode`/`epoch.bump`/`users.lock`/`session.lock`/`users.update`/`sessions.read`/`sessions.revoke`/`audit.insert`/`tx.commit`，均为固定去敏文案。

5. 同步更新 `sqlx_repo.rs` 模块注释、`writes_not_ready` 函数注释及 `write_methods_refuse_fixed_config_without_sql` 测试注释：Task 3 后只有 `revoke_session` 仍是固定拒绝，不能继续声称 `change_password` 尚未实现。

- [ ] **Step 4: 验证**

- Global 4 命令全绿（含既有 auth/service 测试与拆分后的固定拒绝测试）。
- grep 自检：`sqlx_repo.rs` 字符串字面量不含 username/密码变量/raw token/digest hex；无新增 `println!`。
- 验证限于纯快照函数、固定审计投影、SQL 字符串与 Rust 离线编译；**不得 await 会 `pool.begin()` 的 `change_password` 对 `offline_pool`**（会尝试 loopback TCP，不称离线）。真实事务锁序/原子性/回滚由源码审查 + 获准真库 `#[ignore]` 用例验证。

## 后续门禁（本任务不实现，需单独批准）

- 真实 dev DB `#[ignore]`（另行确认目标库 + 备份/迁移门禁后启用）：① 全事务成功（users 行 hash/revision+1/mcp=FALSE、该用户 sessions 全 revoked、至多两条审计行、`schema_meta.authz_epoch` 同事务恰+1、`uq_audit_epoch_seq` 唯一）；② current row 漂移/停用→回滚，无 users/sessions/audit/epoch 变更；③ `revision=u64::MAX` 或 `authz_epoch=u64::MAX`→`RevisionConflict` 且整体回滚；④ 并发改密锁序/死锁回归（MySQL8.4 与 TiDB8.5 各一）。`revoke_session` 落实与 HTTP 端点接线另立任务。

## 独立审查清单

① 锁序恒为 guard→users→sessions，users 行锁先行；② 失败/审计错误整体回滚、固定去敏，无 username/密码/token/digest；③ 审计恰为 `auth.password.changed`（+`auth.session.revoked` 仅有撤销行时），无第 6 个隐式事件；④ `service.rs` 零改动、`main` 仍 health-only、无路由接线；⑤ `revoke_session` 仍固定拒绝；⑥ 文档不宣称真实库原子性已验收；⑦ 按 01 §5 L59 `schema_meta.authz_epoch` 与账号改密和审计**同事务**恰增1，溢出失败回滚，不允许沉默跳过。

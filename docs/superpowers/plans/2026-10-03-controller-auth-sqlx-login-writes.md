# 计划：Task 2b — SQLx 登录写事务（insert_session + record_login_failure 固定脱敏审计）（2026-10-03）

## Goal

为 `SqlxIdentityRepository` 落实两个登录写方法（其余两个不动）：`insert_session` 事务内
guard→users `FOR UPDATE` 严格重验当前行（active 优先、hash/revision 必须与登录快照一致）后
`INSERT sessions`（`created_time = UTC_TIMESTAMP(6)`）+ 固定 `auth.login.succeeded` 审计→
commit；`record_login_failure(Option<id>)` guard→固定匿名 `auth.login.failed` 审计→commit，
审计写失败原样传播、绝不吞错。**不宣称**真实数据库原子性/行锁顺序/回滚一致性已验收
（SQLx 事务仅编译到驱动层）。`change_password`/`revoke_session` 维持固定拒绝，`main` 维持
health-only，不接任何生产路由。

## Arch（沿用既有形态，不新建框架）

- 事务边界：`db.0.begin()` → `integrity::lock_integrity_guard`（integrity.rs:117）→ 写 →
  固定审计（同事务）→ commit；失败整体回滚；驱动错误一律映射固定去敏 `Config`。
- auth 专属 `static AUTH_PROCESS_EPOCH: OnceLock<uuid::Uuid>` + `static AUTH_EVENT_SEQ:
  AtomicU64`，均为 `sqlx_repo.rs` 私有，与 `db.rs` CAS 路径（`compare_and_set` 函数局部静态）
  的 epoch/seq 完全独立，`uq_audit_epoch_seq` 下互不冲突。
- 审计投影由纯函数构造；`auth.login.failed` 对 `None`/`Some(id)` 两种入参**同形**：
  `actor_kind='system'`、`actor_user_id=NULL`、`target_id=NULL`、`params_redacted='{}'`；
  用户名/密码/token/digest 不进函数签名，更不进 SQL（事件值与仓储计划 Task 3 事件表一致）。

## 文件

- Modify: `crates/rsetup-controller/src/auth/sqlx_repo.rs`（唯一实现文件；以冻结的 trait 为准，
  `service.rs` 零改动；find_session Fix2 落定后再改本文件，避免同文件并发改动）
- Modify: `crates/rsetup-controller/src/db.rs:2627,2643` — 仅把 `system_fallback_time_evidence`
  与 `next_event_seq` 改 `pub(crate)`，CAS 路径零行为变化
- 新增文件仅本计划文档；不碰 secret、不触 DB、不联网

## 审查结论（已核，无阻断）

0003 迁移 L55–63 的 `audit_events` 容纳全部固定值（`actor_kind`/`event_type` 列宽、
`params_redacted JSON` 收 `'{}'`、`target_id VARCHAR(128) NULL` 收 hex(32) 或 NULL），auth
独立 epoch 不与 CAS 审计争 `(process_epoch,event_seq)` 唯一键，无需新增事件值。锚点：02 规格
§2.1（L37–47 guard→users 锁内重验契约）与 §2.3（L68–81 audit_events 与必审最小集）、迁移
L1–17 users/sessions 真实列、仓储计划 Task 2–3（L183–260）。

- [ ] **Step 1: RED 载体（stub 先行，保证测试 RED 而非编译失败）**

`sqlx_repo.rs` 新增（stub 一律返回固定拒绝/占位值）：

```rust
const LOCK_USER_SQL: &str = "SELECT id, username, password_hash,
    CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin,
    CAST(must_change_password AS SIGNED) AS must_change_password, revision
    FROM users WHERE id = ? FOR UPDATE";
const INSERT_SESSION_SQL: &str = "INSERT INTO sessions
    (token_hash, user_id, process_epoch, created_time, revoked)
    VALUES (?, ?, ?, UTC_TIMESTAMP(6), FALSE)";
const AUDIT_INSERT_SQL: &str = "INSERT INTO audit_events
    (id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted,
     outcome, time_evidence, process_epoch, event_seq) VALUES (?,?,?,?,?,?,?,?,?,?,?)";
// write_error 仅在 Step 3 开始实现 SQLx 写方法时加入，RED 阶段不留下私有死代码。
#[derive(Debug, PartialEq, Eq)]
struct AuditInsert {
    actor_kind: &'static str, actor_user_id: Option<[u8; 16]>, event_type: &'static str,
    target_kind: Option<&'static str>, target_id: Option<String>,
    params_redacted: &'static str, outcome: &'static str,
}
fn wrong_audit() -> AuditInsert {
    AuditInsert { actor_kind: "wrong", actor_user_id: None, event_type: "wrong",
        target_kind: None, target_id: None, params_redacted: "wrong", outcome: "wrong" }
}
fn login_succeeded_audit(_user_id: [u8; 16]) -> AuditInsert { wrong_audit() }
fn login_failure_audit(_user_id: Option<[u8; 16]>) -> AuditInsert { wrong_audit() }
fn validate_current_user(_current: Option<&IdentityUser>, _snapshot: &IdentityUser)
    -> Result<(), ControllerError> { Err(ControllerError::InvalidArgument) } // 可编译桩；有效快照断言必 RED
// 不新增仅供测试的 WriteStep 枚举：常量顺序不能证明运行时锁序，按真实事务源码审查。
```

两个写方法暂维持 `writes_not_ready()` stub；既有测试 `write_methods_refuse_fixed_config_without_sql` 拆为仅断言 `change_password`/`revoke_session` 仍返回固定拒绝。`offline_pool()` 只能在这两个**触池前直返**的 stub 用例中使用；一旦 await 实现后的真实写方法，它会尝试 loopback TCP，不得称为离线无网络。新增 Step 2 三个行为 RED 测试与一个 SQL 守卫。

- [ ] **Step 2: 三项行为 RED + 一项 SQL 结构守卫（用例必须有真实断言；空体 GREEN 无效）**

```rust
#[test]
fn snapshot_validation_pure() {
    let mut snap = sample_user();
    snap.revision = u64::MAX;
    let cur = snap.clone();
    assert!(validate_current_user(Some(&cur), &snap).is_ok()); // stub InvalidArgument -> RED
    assert!(matches!(validate_current_user(None, &snap), Err(ControllerError::InvalidArgument)));
    let mut inactive = cur.clone();
    inactive.active = false;
    inactive.revision -= 1; // active 拒绝优先于 revision 冲突
    assert!(matches!(validate_current_user(Some(&inactive), &snap), Err(ControllerError::InvalidArgument)));
    let mut wrong_id = cur.clone(); wrong_id.id = [9; 16];
    assert!(matches!(validate_current_user(Some(&wrong_id), &snap), Err(ControllerError::InvalidArgument)));
    let mut changed_hash = cur.clone(); changed_hash.password_hash.push('x');
    assert!(matches!(validate_current_user(Some(&changed_hash), &snap), Err(ControllerError::RevisionConflict)));
    let mut old_revision = cur.clone(); old_revision.revision = u64::MAX - 1;
    assert!(matches!(validate_current_user(Some(&old_revision), &snap), Err(ControllerError::RevisionConflict)));
    let mut zero = snap.clone(); zero.revision = 0;
    let mut one = zero.clone(); one.revision = 1;
    assert!(matches!(validate_current_user(Some(&one), &zero), Err(ControllerError::RevisionConflict)));
}
#[test]
fn login_failed_audit_isomorphic_for_both_shapes() {
    let unknown = login_failure_audit(None);
    assert_eq!(unknown, login_failure_audit(Some([9; 16]))); // wrong_audit stub -> next assertions RED
    assert_eq!(unknown.actor_kind, "system");
    assert_eq!(unknown.actor_user_id, None);
    assert_eq!(unknown.event_type, "auth.login.failed");
    assert_eq!(unknown.target_kind, Some("user"));
    assert_eq!(unknown.target_id, None);
    assert_eq!(unknown.params_redacted, "{}");
    assert_eq!(unknown.outcome, "failure");
}
#[test]
fn login_succeeded_audit_pinned_projection() {
    let id = [7; 16];
    let success = login_succeeded_audit(id);
    assert_eq!(success.actor_kind, "user");
    assert_eq!(success.actor_user_id, Some(id));
    assert_eq!(success.event_type, "auth.login.succeeded");
    assert_eq!(success.target_kind, Some("user"));
    let encoded_id = hex::encode(id);
    assert_eq!(success.target_id.as_deref(), Some(encoded_id.as_str()));
    assert_eq!(success.params_redacted, "{}");
    assert_eq!(success.outcome, "success");
}
#[test]
fn write_sql_gate_fixed_strings() { // Step1 SQL 常量已正确：GREEN-by-construction，不计 RED
    let lock = LOCK_USER_SQL.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_uppercase();
    assert!(lock.starts_with("SELECT ") && lock.contains("WHERE ID = ? FOR UPDATE"));
    assert_eq!(lock.matches('?').count(), 1);
    assert!(!lock.contains(';'));
    let session = INSERT_SESSION_SQL.split_whitespace().collect::<Vec<_>>().join("").to_ascii_uppercase();
    assert!(session.contains("(TOKEN_HASH,USER_ID,PROCESS_EPOCH,CREATED_TIME,REVOKED)"));
    assert!(session.contains("UTC_TIMESTAMP(6)"));
    assert_eq!(session.matches('?').count(), 3);
    let audit = AUDIT_INSERT_SQL.split_whitespace().collect::<Vec<_>>().join("").to_ascii_uppercase();
    assert!(audit.contains("(ID,ACTOR_KIND,ACTOR_USER_ID,EVENT_TYPE,TARGET_KIND,TARGET_ID,PARAMS_REDACTED,OUTCOME,TIME_EVIDENCE,PROCESS_EPOCH,EVENT_SEQ)"));
    assert_eq!(audit.matches('?').count(), 11);
}
```

真正 RED 仅前三项（复用 `sqlx_repo.rs` 既有 `#[cfg(test)]::sample_user()` 合成夹具）；事务调用顺序需读**真实实现** begin→guard→users FOR UPDATE→session INSERT→audit INSERT→commit，并由获准真库用例验证，不能从 SQL 字符串测试推断锁序。

- [ ] **Step 3: GREEN 实现**

1. `db.rs` 两个 helper 改 `pub(crate)`（本任务对 `db.rs` 的全部改动）。仅到此GREEN步骤才加供事务调用的错误构造器，避免RED私有死代码：
   ```rust
   fn write_error(code: &'static str) -> ControllerError {
       ControllerError::Config(format!("identity auth write {code}"))
   }
   ```
2. `validate_current_user`：`None`→`InvalidArgument`；当前行 id!=快照 id 或
   `!current.active`→`InvalidArgument`（active 优先于 hash/revision）；`password_hash` 或
   `revision` 不等→`RevisionConflict`；否则 Ok。
3. 两个审计投影按 Step 2 固定值实现；失败审计忽略入参，两形态同形；替换占位实现时删除只供RED桩调用的 `wrong_audit()`，最终不留私有死代码。
4. `insert_session`：begin→guard→`LOCK_USER_SQL`（bind `user.id`，`fetch_optional`）→
   `scan_user_fields`+`decode_user` 严格解码→`validate_current_user`→`INSERT_SESSION_SQL`
   （bind digest/`user.id`/epoch）→审计 insert→commit；每步驱动错误映射固定码
   （`tx.begin`/`users.lock`/`sessions.insert`/`audit.insert`/`tx.commit`）。
5. `record_login_failure`：begin→guard→审计 insert→commit；任一步失败即回滚并返回固定
   错误，服务侧 `?` 传播为固定服务失败。
6. 共用 `insert_audit(tx, &AuditInsert)`：evidence 用 `system_fallback_time_evidence()`，
   seq 用 `next_event_seq(&AUTH_EVENT_SEQ)`（溢出→`RevisionConflict` 传播），epoch 用
   `AUTH_PROCESS_EPOCH.get_or_init(Uuid::new_v4)`，字段序与 `db.rs:2724` 一致。

- [ ] **Step 4: 验证**

- Global 4 命令全绿（含既有 auth/service 测试与拆分后的固定拒绝测试）。
- grep 自检：`sqlx_repo.rs` 字符串字面量不含 username/密码变量/raw token/digest hex；
  无新增 `println!`/`eprintln!`；`db.rs` diff 恰好两处 `pub(crate)`，CAS 路径零 diff。
- 验证限于纯快照函数、固定审计投影、SQL 字符串与 Rust 离线编译；不得调用会 `await pool.acquire()` 的测试。`offline_pool` 构造虽 lazy，await 写方法会尝试 loopback TCP，不能把它称为无网络测试。真实事务错误映射由源码审查与获准真库用例验证。

## 后续门禁（本任务不实现，需单独批准）

- 真实 dev DB `#[ignore]` 用例（另行确认目标库 + 备份/迁移门禁后启用）：① insert_session
  全事务成功路径（session+audit 行、seq 唯一）；② 快照漂移/当前停用→回滚、无 session/
  audit 行；③ record_login_failure 两形态同形行 + 唯一键；④ 并发登录锁序/死锁回归
  （MySQL8.4 与 TiDB8.5 各一）；`change_password`/`revoke_session` 落实与 HTTP 端点接线另立任务。

## 独立审查清单

① 锁序恒为 guard→users（仅 insert_session）→审计，不锁其他对象；② 失败审计任何字段与
错误信息无用户名/密码/token；③ `db.rs` diff 恰好两处可见性；④ `main` 仍 health-only、
无路由接线；⑤ 文档与提交说明均不宣称真实 DB 原子性已验收。

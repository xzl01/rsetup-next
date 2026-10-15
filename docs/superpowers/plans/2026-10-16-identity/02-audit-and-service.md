### Task 4: 管理任务独立审计命名空间与管理写事务仓储（锁序 + CAS + LastAdmin）

**Files:**
- Create: `crates/rsetup-controller/src/audit.rs`
- 不变: `crates/rsetup-controller/src/auth/sqlx_repo.rs`（AUTH 审计命名空间保留：本地 `AUTH_PROCESS_EPOCH`/`AUTH_EVENT_SEQ` 与 `insert_audit` 逻辑原样，不委托 `crate::audit`）
- 不变: `crates/rsetup-controller/src/db.rs`（CAS 审计路径保留：函数内局部 `PROCESS_EPOCH`/`EVENT_SEQ` 与审计 INSERT 原样；未来 v4 版本化不等于重写旧审计）
- Modify: `crates/rsetup-controller/src/authz/sqlx_repo.rs`（追加写方法）
- Modify: `crates/rsetup-controller/src/devices/repository.rs`（追加 `patch_device_display_name`、`replace_group_members`）
- Modify: `crates/rsetup-controller/src/authz/policy.rs`（`validate_permission_list`）
- Modify: `crates/rsetup-controller/src/lib.rs`（注册 `pub mod audit;`）

**Interfaces:**
- Consumes: Task 3 的全部类型；`crate::integrity::{lock_integrity_guard, require_active_admin, raw_bool, validate_grant_fields}`；`crate::authz::policy::check_admin_demotion_allowed`。
- Produces:
  - `audit::admin_process_epoch() -> Uuid`（**仅 ADMIN namespace**：由 `ADMIN_PROCESS_EPOCH: OnceLock<Uuid>` 承载，同一新写者内稳定，UUIDv4 合法；无需全 crate 集中统一分配器，但各写者必须持独立命名空间，新 writer 不得复用旧 writer 的 epoch/static）
  - `audit::next_admin_event_seq() -> Result<u64, ControllerError>`（ADMIN namespace 序列：复用既有 `db::next_event_seq(&ADMIN_EVENT_SEQ)`，辅助函数主体不改；严格递增、checked 溢出返回 `RevisionConflict` 不 wrap）
  - `audit::AuditInsert<'a> { actor_kind, actor_user_id: Option<[u8;16]>, event_type, target_kind, target_id: Option<String>, params_redacted, outcome }`
  - `audit::insert_audit(&mut Transaction<'_, MySql>, &AuditInsert<'_>) -> Result<(), ControllerError>`（11 字段，`time_evidence` 非空，`process_epoch` 为 ADMIN namespace 的 `ADMIN_PROCESS_EPOCH`，`event_seq` 为 `ADMIN_EVENT_SEQ` 严格递增；业务同事务失败整体回滚；只透传 `Database`/`RevisionConflict`，由调用方决定是否窄化映射）
  - `AuthzRepository` 写方法：
    - `create_user(&self, actor, username: &str, display_name: &str, is_admin: bool, next_hash: &str) -> RepoFuture<UserRecord>`
    - `patch_user(&self, actor, user_id: [u8;16], expected_revision: u64, active: Option<bool>, is_admin: Option<bool>, display_name: Option<&str>) -> RepoFuture<UserRecord>`
    - `reset_user_password(&self, actor, user_id: [u8;16], expected_revision: u64, next_hash: &str) -> RepoFuture<UserRecord>`
    - `create_role(&self, actor, name: &str, permissions: &[Permission]) -> RepoFuture<RoleRecord>`
    - `patch_role(&self, actor, id: Uuid, expected_revision: u64, name: Option<&str>, permissions: Option<&[Permission]>, archived: Option<bool>) -> RepoFuture<RoleRecord>`
    - `create_grant(&self, actor, user_id: [u8;16], source: GrantSource, scope: GrantScope) -> RepoFuture<GrantRecord>`
    - `delete_grant(&self, actor, id: Uuid, expected_revision: u64) -> RepoFuture<()>`
    - `create_group(&self, actor, name: &str) -> RepoFuture<GroupRecord>`
    - `patch_group(&self, actor, id: Uuid, expected_revision: u64, name: Option<&str>, archived: Option<bool>) -> RepoFuture<GroupRecord>`
  - `DeviceRepository` 写方法：
    - `patch_device_display_name(&self, actor, device_id: [u8;32], expected_revision: u64, display_name: &str) -> RepoFuture<DeviceAdminView>`
    - `replace_group_members(&self, actor, group_id: Uuid, expected_revision: u64, device_ids: &[[u8;32]]) -> RepoFuture<GroupRecord>`
  - `policy::validate_permission_list(permissions: &[Permission]) -> Result<(), ControllerError>`（非空、无重复）
  - 纯函数：`fn ordered_user_ids(actor: [u8;16], target: [u8;16]) -> ([u8;16], Option<[u8;16]>)`、`fn admin_change_removes_admin_state(target_active: bool, target_is_admin: bool, next_active: bool, next_is_admin: bool) -> bool`
  - 审计事件名固定集合：`identity.user.create|patch|reset_password`、`identity.role.create|patch`、`identity.grant.create|delete`、`identity.group.create|patch|members`、`identity.device.patch`；`target_kind` ∈ `user|role|grant|group|device`；`target_id` 为小写 hex；`params_redacted` 恒 `"{}"`。

**偏差记录（必须写入代码注释、提交信息与审查记录）：** 权威四专题设计 §3 只要求"审计 `time_evidence` 非空、`process_epoch` 与 `event_seq` 合法且唯一；审计失败连同业务修改回滚"，**不要求每 process 全 crate 唯一**；正式 gemini 审查已否决"不必要的全 crate 统一 epoch"。既有 `auth/sqlx_repo.rs` 第 105–107 行注释（AUTH 独立）与 `db.rs` 函数内局部静态是两个**已独立且安全的 namespace**（epoch 各自由 CSPRNG 独立采样，随机碰撞概率极低但非数学零碰撞；DB `uq_audit_epoch_seq (process_epoch, event_seq)` UNIQUE 失败关闭，冲突必须使业务+审计同事务失败，不静默覆盖），故本任务将其**完全保留原样**（baseline 事实）；新管理/任务写者在 `src/audit.rs` 使用自己的 `ADMIN_PROCESS_EPOCH`/`ADMIN_EVENT_SEQ`。第三个独立 UUID epoch 不构成"第三套"冲突（唯一键只约束 (epoch, seq) 对），也**无需撤回设计 §3 的解释**。

- [ ] **Step 0: 建可编译桩**

`audit.rs` 的分配器先实现为**可编译但行为错误**的桩：`admin_process_epoch()` 每次调用都返回新的 `Uuid::new_v4()`（错误：未在同一新写者内稳定），`next_admin_event_seq()` 恒返回 `Ok(7)`（错误：常量 seq，不严格递增、无 checked 溢出）；`insert_audit` 先实现为空 `Ok(())`（不写审计、不递增序列），写方法先实现为"仅返回 `RevisionConflict`"或直接 `Err(ControllerError::InvalidState)` 的可编译桩 —— 桩的目的只是让纯函数测试可编译并让行为断言失败。

- [ ] **Step 1: 编写失败测试（纯规则 + 审计身份唯一性 + 回滚语义的可离线部分）**

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs（追加）
#[test]
fn user_lock_order_is_deterministic_by_uuid_bytes() {
    let low = [1u8; 16];
    let high = [2u8; 16];
    assert_eq!(ordered_user_ids(high, low), (low, Some(high)));
    assert_eq!(ordered_user_ids(low, high), (low, Some(high)));
    // 同一用户只锁一行，禁止自锁两次。
    assert_eq!(ordered_user_ids(low, low), (low, None));
}

#[test]
fn only_removing_active_admin_state_triggers_last_admin_rule() {
    // 停用活跃管理员 / 降级活跃管理员 → 需要检查
    assert!(admin_change_removes_admin_state(true, true, false, true));
    assert!(admin_change_removes_admin_state(true, true, true, false));
    // 已是停用或非管理员 → 不触发（不得误报 LAST_ADMIN）
    assert!(!admin_change_removes_admin_state(false, true, false, true));
    assert!(!admin_change_removes_admin_state(true, false, true, false));
    // 不变更或升级 → 不触发
    assert!(!admin_change_removes_admin_state(true, true, true, true));
    assert!(!admin_change_removes_admin_state(false, false, true, true));
}

#[test]
fn permission_list_must_be_non_empty_and_unique() {
    use crate::authz::policy::Permission::*;
    assert!(crate::authz::policy::validate_permission_list(&[]).is_err());
    assert!(crate::authz::policy::validate_permission_list(&[DeviceRead, DeviceRead]).is_err());
    assert!(crate::authz::policy::validate_permission_list(&[DeviceRead, DeviceReboot]).is_ok());
}

#[test]
fn normalize_device_member_ids_pure_logic() {
    use crate::devices::repository::normalize_device_member_ids;
    assert_eq!(normalize_device_member_ids(&[]).unwrap(), Vec::<[u8; 32]>::new());
    let (d1, d2, d3) = ([1u8; 32], [2u8; 32], [3u8; 32]);
    assert_eq!(
        normalize_device_member_ids(&[d3, d1, d2, d1, d3]).unwrap(),
        vec![d1, d2, d3]
    );
    let mut batch_1024 = Vec::with_capacity(1024);
    for i in 0..1024u16 {
        let mut key = [0u8; 32];
        key[..2].copy_from_slice(&i.to_be_bytes());
        batch_1024.push(key);
    }
    assert_eq!(normalize_device_member_ids(&batch_1024).unwrap().len(), 1024);
    let mut batch_1025 = batch_1024.clone();
    let mut extra = [0u8; 32];
    extra[..2].copy_from_slice(&1024u16.to_be_bytes());
    batch_1025.push(extra);
    assert!(matches!(
        normalize_device_member_ids(&batch_1025),
        Err(ControllerError::InvalidArgument)
    ));
}
```

```rust
// crates/rsetup-controller/src/audit.rs
#[cfg(test)]
mod tests {
    use super::*;

    // 真实 ADMIN 分配器行为失败测试（非源码字符串检查）。注意 `ADMIN_EVENT_SEQ` 为进程级单例、测试间不重置：
    // 自增部分直接测 `next_admin_event_seq()`，只断言相对严格递增（不断言绝对初值）；
    // overflow/nonwrap 不经 ADMIN 单例，改用可控 `AtomicU64::new(u64::MAX)` 直接复用 `crate::db::next_event_seq` 辅助验证。
    #[test]
    fn admin_epoch_is_stable_within_writer_and_uuid_v4() {
        let first = admin_process_epoch();
        let second = admin_process_epoch();
        assert_eq!(first, second, "same admin writer must reuse one epoch");
        assert_eq!(first.get_version_num(), 4, "epoch must be a legal UUIDv4");
    }

    #[test]
    fn admin_event_seq_strictly_increases_and_checked_overflow_does_not_wrap() {
        let first = next_admin_event_seq().expect("no overflow");
        let second = next_admin_event_seq().expect("no overflow");
        assert!(second > first, "event_seq must strictly increase");
        // checked 溢出：必须返回 RevisionConflict，不得 wrap 回 0。
        let counter = std::sync::atomic::AtomicU64::new(u64::MAX);
        assert!(matches!(crate::db::next_event_seq(&counter), Err(ControllerError::RevisionConflict)));
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            u64::MAX,
            "counter must not wrap past u64::MAX"
        );
    }

    // 结构核查（非行为 RED）：防实现顺手改动旧写者；不得凭源码字符串把数据库审计当通过。
    #[test]
    fn legacy_audit_writer_sources_remain_unchanged() {
        let auth = include_str!("auth/sqlx_repo.rs");
        assert!(auth.contains("static AUTH_PROCESS_EPOCH"));
        assert!(auth.contains("static AUTH_EVENT_SEQ"));
        let db = include_str!("db.rs");
        assert!(db.contains("static PROCESS_EPOCH"));
        assert!(db.contains("static EVENT_SEQ"));
        assert!(db.contains("INSERT INTO audit_events"));
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller user_lock_order_is_deterministic_by_uuid_bytes
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller only_removing_active_admin_state_triggers_last_admin_rule
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller permission_list_must_be_non_empty_and_unique
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller normalize_device_member_ids_pure_logic
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller audit::tests::admin_epoch_is_stable_within_writer_and_uuid_v4
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller audit::tests::admin_event_seq_strictly_increases_and_checked_overflow_does_not_wrap
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller audit::tests::legacy_audit_writer_sources_remain_unchanged
```
预期：各运行恰好 1 个测试（单测用完整路径，命令匹配非零用例）。前四条因桩返回错误值而断言失败；两条 ADMIN 分配器测试为**行为 RED**——桩 epoch 每次返回新值，`first == second` 断言失败；桩 seq 恒为 7，`second > first` 断言失败。结构核查在桩阶段即为 GREEN（旧写者源码尚未动过），只算**结构核查**而非行为 RED；不得凭源码字符串把数据库审计当通过。

- [ ] **Step 3: 实现管理审计命名空间写入器与全部写方法**

```rust
// crates/rsetup-controller/src/audit.rs
//! 新管理/任务写者的独立审计命名空间：与 `auth/sqlx_repo.rs` 的
//! `AUTH_PROCESS_EPOCH`/`AUTH_EVENT_SEQ`、`db.rs` 函数内局部
//! `PROCESS_EPOCH`/`EVENT_SEQ` 完全独立——`uq_audit_epoch_seq
//! (process_epoch, event_seq)` 下三个 epoch 各自由 CSPRNG 独立采样，随机碰撞
//! 概率极低但非数学零碰撞；DB 唯一约束 `uq_audit_epoch_seq` 失败关闭，
//! 冲突必须使业务+审计同事务失败，不静默覆盖。
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use uuid::Uuid;

use crate::ControllerError;

static ADMIN_PROCESS_EPOCH: OnceLock<Uuid> = OnceLock::new();
static ADMIN_EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn admin_process_epoch() -> Uuid {
    *ADMIN_PROCESS_EPOCH.get_or_init(Uuid::new_v4)
}

pub fn next_admin_event_seq() -> Result<u64, ControllerError> {
    // 复用既有辅助（主体不改）；checked 溢出返回 RevisionConflict。
    crate::db::next_event_seq(&ADMIN_EVENT_SEQ)
}

pub struct AuditInsert<'a> {
    pub actor_kind: &'a str,
    pub actor_user_id: Option<[u8; 16]>,
    pub event_type: &'a str,
    pub target_kind: &'a str,
    pub target_id: Option<String>,
    pub params_redacted: &'a str,
    pub outcome: &'a str,
}

const AUDIT_INSERT_SQL: &str = "INSERT INTO audit_events \
 (id, actor_kind, actor_user_id, event_type, target_kind, target_id, \
  params_redacted, outcome, time_evidence, process_epoch, event_seq) \
 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

pub async fn insert_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    insert: &AuditInsert<'_>,
) -> Result<(), ControllerError> {
    // `time_evidence` 必须非空：复用既有 system_fallback_time_evidence（含 clock_epoch，主体不改）。
    let evidence = crate::db::system_fallback_time_evidence();
    // ADMIN namespace 序列；溢出返回 RevisionConflict，业务同事务失败整体回滚。
    let seq = next_admin_event_seq()?;
    let epoch = admin_process_epoch();
    sqlx::query(AUDIT_INSERT_SQL)
        .bind(Uuid::new_v4().as_bytes().as_slice())
        .bind(insert.actor_kind)
        .bind(insert.actor_user_id.as_ref().map(|id| id.as_slice()))
        .bind(insert.event_type)
        .bind(insert.target_kind)
        .bind(insert.target_id.as_deref())
        .bind(insert.params_redacted)
        .bind(insert.outcome)
        .bind(&evidence)
        .bind(epoch.as_bytes().as_slice())
        .bind(seq)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
```

旧写者保留（下列两文件**不改**；baseline 事实的 AUTH 独立注释保留）：

- `auth/sqlx_repo.rs`：本地 `static AUTH_PROCESS_EPOCH`/`static AUTH_EVENT_SEQ`（第 109–110 行）与登录写事务内的 `insert_audit`（第 526–549 行：11 字段、`system_fallback_time_evidence`、`next_event_seq(&AUTH_EVENT_SEQ)`、错误映射 `.map_err(|_| write_error("audit.insert"))`）全部原样保留，不委托 `crate::audit`。
- `db.rs`：`compare_and_set` 内函数局部 `static PROCESS_EPOCH`/`static EVENT_SEQ`（第 2681–2682 行）与审计 INSERT（第 2724–2731 行）全部原样保留；未来 v4 版本化（Runtime Task 3 改 `validate_guard_rows`/`scan_identity_rows`）只涉及 guard，**不等于旧审计路径可重写**。

写方法统一骨架（每个方法都必须走这条骨架，差异只在锁对象与 SQL）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs
// UserRow只是本模块已定义安全UserRecord别名，无password_hash字段。
type UserRow = UserRecord;
async fn select_user_for_update(tx:&mut sqlx::Transaction<'_,sqlx::MySql>,id:[u8;16])->Result<Option<UserRow>,ControllerError> {
    use sqlx::Row;
    let row=sqlx::query("SELECT id,username,display_name,CAST(active AS SIGNED) AS active,CAST(is_admin AS SIGNED) AS is_admin,CAST(must_change_password AS SIGNED) AS must_change_password,revision FROM users WHERE id=? FOR UPDATE")
        .bind(id.as_slice()).fetch_optional(&mut **tx).await?;
    row.map(|row|decode_user(row.try_get("id")?,row.try_get("username")?,row.try_get("display_name")?,row.try_get("active")?,row.try_get("is_admin")?,row.try_get("must_change_password")?,row.try_get("revision")?))
        .transpose()
}

async fn begin_guard<'a>(
    db: &'a DbPool,
) -> Result<sqlx::Transaction<'a, sqlx::MySql>, ControllerError> {
    let mut tx = db.0.begin().await?;
    crate::integrity::lock_integrity_guard(&mut tx).await?;
    Ok(tx)
}

/// 目标为 users 行时：guard → 按 UUID 字节序锁 actor/target → 用**锁定行**
/// 严格重验 actor 仍为有效管理员 → 返回目标行（缺失即 NotFound）。
async fn lock_user_pair_and_verify_actor(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor: [u8; 16],
    target: [u8; 16],
) -> Result<Option<UserRow>, ControllerError> {
    let (first, second) = ordered_user_ids(actor, target);
    let first_row = select_user_for_update(tx, first).await?;
    let second_row = match second {
        Some(id) => select_user_for_update(tx, id).await?,
        None => None,
    };
    let actor_row = if first == actor { first_row.as_ref() } else { second_row.as_ref() };
    let actor_row = actor_row.ok_or(ControllerError::PermissionDenied)?;
    if !(actor_row.active && actor_row.is_admin && !actor_row.must_change_password) {
        return Err(ControllerError::PermissionDenied);
    }
    Ok(if first == target { first_row } else { second_row })
}
```

`patch_user` 槽位完整方法（其余写方法同构，仅替换实体、CAS 列与审计事件名）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：生产 impl AuthzRepository for
// SqlxAuthzRepository 的 patch_user 槽位完整方法（本计划代码，非实际实现）。
// 其余 trait 方法（12 读槽位 + 本方法外 8 写槽位）代码体未齐：本块是"槽位级完整方法"，
// 不得把缺其余方法的"独立完整 impl"冒称可编译。
// 局部 tx：sqlx::Transaction<'a, sqlx::MySql>（owned，begin_guard 返回）；方法内直接 SQL
// 用 &mut *tx（Transaction Deref 到连接）；helper 参数为 &mut sqlx::Transaction<'_,
// sqlx::MySql>，本方法一律传 &mut tx（局部 tx 本体）；Session 即 crate::auth::service::Session（既有桩同简写；不新增 use，不复制第二份方法）。
fn patch_user<'a>(
    &'a self,
    actor: &'a Session,
    user_id: [u8; 16],
    expected_revision: u64,
    active: Option<bool>,
    is_admin: Option<bool>,
    display_name: Option<&'a str>,
) -> RepoFuture<'a, UserRecord> {
    Box::pin(async move {
        let mut tx = begin_guard(&self.db).await?;
        let target = lock_user_pair_and_verify_actor(&mut tx, actor.user.id, user_id)
            .await?
            .ok_or(ControllerError::NotFound)?;
        if target.revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }
        let next_active = active.unwrap_or(target.active);
        let next_admin = is_admin.unwrap_or(target.is_admin);
        let next_display = match display_name {
            Some(value) => {
                validate_display_name(value)?;
                value.to_string()
            }
            None => target.display_name.clone(),
        };
        if admin_change_removes_admin_state(target.active, target.is_admin, next_active, next_admin) {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM users WHERE active = TRUE AND is_admin = TRUE",
            )
            .fetch_one(&mut *tx)
            .await?;
            // guard 行锁已串行化全部管理员写，故此计数在本事务内稳定。
            if let Err(error) = check_admin_demotion_allowed(count) {
                // 此时尚无业务UPDATE：只提交拒绝审计，不bump epoch/不撤销sessions；
                // 错误路径唯一明确 commit 例外（其余错误靠 drop 局部 tx 自动回滚）。
                crate::audit::insert_audit(
                    &mut tx,
                    &crate::audit::AuditInsert {
                        actor_kind: "user",
                        actor_user_id: Some(actor.user.id),
                        event_type: "identity.user.last_admin_blocked",
                        target_kind: "user",
                        target_id: Some(hex::encode(user_id)),
                        params_redacted: "{}",
                        outcome: "denied",
                    },
                )
                .await?;
                tx.commit().await?;
                return Err(error);
            }
        }
        let next_revision = target
            .revision
            .checked_add(1)
            .ok_or(ControllerError::RevisionConflict)?;
        let changed = sqlx::query(
            "UPDATE users SET active = ?, is_admin = ?, display_name = ?, revision = ? \
             WHERE id = ? AND revision = ?",
        )
        .bind(next_active)
        .bind(next_admin)
        .bind(&next_display)
        .bind(next_revision)
        .bind(user_id.as_slice())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        if target.active && !next_active {
            // 停用必须在同事务撤销该用户全部会话。
            let revoked = sqlx::query(
                "UPDATE sessions SET revoked = TRUE WHERE user_id = ? AND revoked = FALSE",
            )
            .bind(user_id.as_slice())
            .execute(&mut *tx)
            .await?
            .rows_affected();
            crate::audit::insert_audit(
                &mut tx,
                &crate::audit::AuditInsert {
                    actor_kind: "user",
                    actor_user_id: Some(actor.user.id),
                    event_type: "identity.sessions.revoke",
                    target_kind: "user",
                    target_id: Some(hex::encode(user_id)),
                    params_redacted: "{}",
                    outcome: if revoked > 0 { "success" } else { "no_active_sessions" },
                },
            )
            .await?;
        }
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.user.patch",
                target_kind: "user",
                target_id: Some(hex::encode(user_id)),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;
        // commit 前构造安全 UserRecord 全部 7 字段（无 password_hash）；
        // 仅 commit 成功才 Ok(updated)。
        let updated = UserRecord {
            id: user_id,
            username: target.username,
            display_name: next_display,
            active: next_active,
            is_admin: next_admin,
            must_change_password: target.must_change_password,
            revision: next_revision,
        };
        tx.commit().await?;
        Ok(updated)
    })
}
```

本方法复用已有完整定义，不新增函数/trait/权限点/action 字段：`check_admin_demotion_allowed`（Task 1 完整定义：`count <= 1` → `Err(ControllerError::LastAdmin)`）、`validate_display_name`（Task 3 完整定义）、`begin_guard`/`select_user_for_update`/`lock_user_pair_and_verify_actor`/`bump_authz_epoch`（上方写方法统一骨架与既有完整定义）、`audit::insert_audit`/`AuditInsert`（Step 3 完整定义）；`read_error`/`write_error` 亦仅 Task 3 Step 3 第 2 条文字声明（固定去敏 `Config` 文案，无代码块），本方法经 `select_user_for_update`/`decode_user` 间接依赖、不直接调用，属 Task 3 装配范围，不代其定义。`ordered_user_ids`/`admin_change_removes_admin_state` 在本计划目前仅有 Task 4 Interfaces 文字签名与 Step 1 RED 测试、生产 fn 体未齐（实记）；其最小必要定义由既有 RED 断言唯一导出，补于此（非新增 API）：

```rust
fn ordered_user_ids(actor: [u8; 16], target: [u8; 16]) -> ([u8; 16], Option<[u8; 16]>) {
    if actor <= target {
        (actor, (actor != target).then_some(target))
    } else {
        (target, Some(actor))
    }
}

fn admin_change_removes_admin_state(
    target_active: bool,
    target_is_admin: bool,
    next_active: bool,
    next_is_admin: bool,
) -> bool {
    // 仅"当前活跃管理员且本次变更确实移除该状态"才触发（与 Step 1 六条断言一致）。
    target_active && target_is_admin && (!next_active || !next_is_admin)
}
```

```rust
/// `authz_epoch` checked CAS 自增；singleton 行已被 guard 锁定，CAS 仍防未走 guard 的写者。
pub(crate) async fn bump_authz_epoch(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
) -> Result<u64, ControllerError> {
    let current: u64 = sqlx::query_scalar("SELECT authz_epoch FROM schema_meta WHERE singleton = 1")
        .fetch_one(&mut **tx)
        .await?;
    let next = current.checked_add(1).ok_or(ControllerError::RevisionConflict)?;
    let changed = sqlx::query(
        "UPDATE schema_meta SET authz_epoch = ? WHERE singleton = 1 AND authz_epoch = ?",
    )
    .bind(next)
    .bind(current)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(ControllerError::RevisionConflict);
    }
    Ok(next)
}
```

其余写方法必须遵守的**逐项实体规则**（与上方同骨架，缺项即审查不通过）：

| 方法 | 锁对象与顺序 | 事务内附加校验 | 附加清理 | `event_type` |
| :--- | :--- | :--- | :--- | :--- |
| `create_user` | guard → actor 行（`require_active_admin`） | username 规范（`db::valid_username`）、display_name 合法、`next_hash` 非空；`INSERT`；`uq_users_username` 冲突 → `InvalidState` | 无（`must_change_password=TRUE`、`revision=1`） | `identity.user.create` |
| `reset_user_password` | guard → actor/target 按字节序 | target revision CAS；`password_hash=next_hash`、`must_change_password=TRUE` | `UPDATE sessions SET revoked=TRUE WHERE user_id=? AND revoked=FALSE` | `identity.user.reset_password` |
| `create_role` | guard → actor 行 → 无实体锁（新行） | `validate_permission_list`；name 非空 ≤128 且无控制字符；`builtin=FALSE`、`revision=1` | 插入 `role_permissions` 全部权限 | `identity.role.create` |
| `patch_role` | guard → actor 行 → `roles` 行 `FOR UPDATE`（按 id） | revision CAS；`builtin && archived==Some(true)` → `InvalidState`；改名不破坏 `uq_roles_name`；从归档恢复（`locked_archived` 且 `next_archived=false`）必须携带合法非空 `permissions`：`None` → `InvalidArgument`(400)，`Some(合法非空)` 同事务重建 `role_permissions`（active ⇒ 权限非空，绝不产生 active 空权限角色） | `archived=true` 时：`DELETE FROM grants WHERE source_kind='role' AND role_id=?` 且 `DELETE FROM role_permissions WHERE role_id=?` | `identity.role.patch` |
| `create_grant` | guard → actor 行 → `users`(target) → `roles`/`device_groups`/`devices` 依 scope 按序 `FOR UPDATE` | `grant_columns` 先做 source/scope 互斥；target 用户必须 active；role/group/device 必须存在且未归档；`revision=1` | 无 | `identity.grant.create` |
| `delete_grant` | guard → actor 行 → `grants` 行 `FOR UPDATE` | revision CAS；缺失 → `NotFound` | 物理删除该 grant | `identity.grant.delete` |
| `create_group` | guard → actor 行 | name 非空 ≤128 且无控制字符；`archived=FALSE`、`revision=1` | 无 | `identity.group.create` |
| `patch_group` | guard → actor 行 → `device_groups` 行 `FOR UPDATE` | revision CAS | `archived=true` 时：`DELETE FROM group_members WHERE group_id=?` 且 `DELETE FROM grants WHERE scope_kind='group' AND scope_group_id=?` | `identity.group.patch` |
| `replace_group_members` | guard → actor 行 → `device_groups` 行 → `devices` 按公钥字节序 `FOR UPDATE` | 去重；空集合合法（清空成员）；**上限 1024，超限 `InvalidArgument`**（沿用既有协调决定）；每个设备必须存在且未归档 | `DELETE FROM group_members WHERE group_id=?` 后按公钥序插入 | `identity.group.members` |
| `patch_device_display_name` | guard → actor 行 → `devices` 行 `FOR UPDATE` | revision CAS；`validate_display_name` | 无（**仅改中控档案，不得改 `admission_state`/`review_decision`**；设备归档无独立路由） | `identity.device.patch` |

`reset_user_password` 槽位完整方法（与 `patch_user` 同骨架，目标表 `users`；锁序 guard → `lock_user_pair_and_verify_actor`（actor/target 按 UUID 字节序 `FOR UPDATE`，并由锁定行重验 actor 仍为有效管理员），行缺失 → `NotFound`；`next_hash` 空串 → `InvalidArgument`（不进入事务）；`expected_revision` CAS + `checked_add`；`UPDATE` 强制 `password_hash=next_hash`、`must_change_password=TRUE` 并递增 revision，影响恰 1 行否则 `RevisionConflict`；同事务无条件强制撤销该目标用户全部活动会话（`UPDATE sessions SET revoked=TRUE WHERE user_id=? AND revoked=FALSE`）；`bump_authz_epoch` 与 `identity.user.reset_password` 成功审计（target_kind `user`、target_id 小写 hex、params_redacted 恒 `"{}"`）；commit 前安全构造 7 字段 `UserRecord`（`must_change_password=true`、绝不含 `password_hash`），仅 commit 成功才 `Ok(record)`；真实 CAS/行锁/级联撤销/审计只能由 Task 14 受门禁真库用例补齐，本切片不冒充 SQL 层 GREEN）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：reset_user_password 槽位完整方法（本计划代码，非实际实现；
// 其余 trait 方法未齐，不得把缺其余方法的"独立完整 impl"冒称可编译）。锁序 guard → actor/target 按字节序；
// 局部 tx 为 owned（begin_guard），方法内直接 SQL 用 &mut *tx，helper 参数一律传 &mut tx；
// Session/hex/audit 复用既有引用，不新增 use，不复制第二份方法。
fn reset_user_password<'a>(
    &'a self,
    actor: &'a Session,
    user_id: [u8; 16],
    expected_revision: u64,
    next_hash: &'a str,
) -> RepoFuture<'a, UserRecord> {
    Box::pin(async move {
        // 空哈希直接 InvalidArgument，不进入事务。
        if next_hash.is_empty() {
            return Err(ControllerError::InvalidArgument);
        }
        let mut tx = begin_guard(&self.db).await?;
        // 按 UUID 字节序锁 actor 与 target 行，并由锁定行重验 actor 仍为有效管理员；行缺失 → NotFound。
        let target = lock_user_pair_and_verify_actor(&mut tx, actor.user.id, user_id)
            .await?
            .ok_or(ControllerError::NotFound)?;
        if target.revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }
        let next_revision = target
            .revision
            .checked_add(1)
            .ok_or(ControllerError::RevisionConflict)?;
        let changed = sqlx::query(
            "UPDATE users SET password_hash = ?, must_change_password = TRUE, revision = ? \
             WHERE id = ? AND revision = ?",
        )
        .bind(next_hash)
        .bind(next_revision)
        .bind(user_id.as_slice())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        // 同事务无条件强制撤销该目标用户全部活动会话。
        sqlx::query("UPDATE sessions SET revoked = TRUE WHERE user_id = ? AND revoked = FALSE")
            .bind(user_id.as_slice())
            .execute(&mut *tx)
            .await?;
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.user.reset_password",
                target_kind: "user",
                target_id: Some(hex::encode(user_id)),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;
        // commit 前构造安全 UserRecord 全部 7 字段（无 password_hash）；
        // must_change_password 恒 true；仅 commit 成功才 Ok(record)。
        let record = UserRecord {
            id: user_id,
            username: target.username,
            display_name: target.display_name,
            active: target.active,
            is_admin: target.is_admin,
            must_change_password: true,
            revision: next_revision,
        };
        tx.commit().await?;
        Ok(record)
    })
}
```

`create_group` 槽位完整方法（与上方同骨架，目标表 `device_groups`；该表无 `created_time` 列，新组 `member_count=0`；INSERT 唯一键冲突——group 名唯一键 `uq_device_groups_name` 与 `id` 主键均为唯一冲突——按驱动 `is_unique_violation()` 分类、按实体规则固定 `InvalidState`(409)，不回显 driver 原文、不识别具体索引；若该"不可识别索引"语义须专家裁决则为待裁决边界，不宣称此驱动分类已在 MySQL/TiDB 双库验证；不新增错误字段/权限/依赖；任一错误 drop 局部 tx 自动回滚）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：create_group 槽位完整方法（本计划代码，非实际实现；
// 其余 trait 方法未齐，不得把缺其余方法的"独立完整 impl"冒称可编译）。局部 tx 为 owned（begin_guard），
// 方法内直接 SQL 用 &mut *tx，helper 参数一律传 &mut tx；Session/Uuid/hex/audit 复用既有引用，不新增 use。
fn create_group<'a>(
    &'a self,
    actor: &'a Session,
    name: &'a str,
) -> RepoFuture<'a, GroupRecord> {
    Box::pin(async move {
        validate_group_name(name)?;
        let mut tx = begin_guard(&self.db).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
        let id = Uuid::new_v4();
        if let Err(error) = sqlx::query(
            "INSERT INTO device_groups (id, name, archived, revision) VALUES (?, ?, FALSE, 1)",
        )
        .bind(id.as_bytes().as_slice())
        .bind(name)
        .execute(&mut *tx)
        .await
        {
            // 唯一冲突固定 InvalidState(409)，不回显 driver 原文；其他 SQL 错误透传 Database（既有 backend error 统一 503 去敏）。
            // 用驱动 unique-violation 分类（SQLx 0.8.6 `DatabaseError::is_unique_violation()`：
            // `kind()==UniqueViolation`；MySQL/TiDB 同走 MySQL 协议，重复条目错误码映射为该 kind）：
            // 不解析 driver message 文本，也不依赖 Postgres-only 的 `constraint()` 名称映射。
            let unique_conflict = error
                .as_database_error()
                .is_some_and(|e| e.is_unique_violation());
            return Err(if unique_conflict {
                ControllerError::InvalidState
            } else {
                error.into()
            });
        }
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.group.create",
                target_kind: "group",
                target_id: Some(hex::encode(id.as_bytes())),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;
        // commit 前构造安全 GroupRecord 全部 5 字段；仅 commit 成功才 Ok(record)。
        let record = GroupRecord {
            id,
            name: name.to_string(),
            archived: false,
            revision: 1,
            member_count: 0,
        };
        tx.commit().await?;
        Ok(record)
    })
}
```

`patch_group` 槽位完整方法（与 create_group 同骨架，目标表 `device_groups`；锁序 guard → actor 行 → `device_groups` 行 `FOR UPDATE`，行缺失 → `NotFound`；`name` 为 `Some` 时先过既有 `validate_group_name` 门禁，不复制 helper；`expected_revision` CAS + `checked_add`；`archived=true` 时物理删除 `DELETE FROM group_members WHERE group_id=?` 与 `DELETE FROM grants WHERE scope_kind='group' AND scope_group_id=?`（保留历史父 group 行、不删其它组）；`UPDATE ... AND revision=?` 影响恰 1 行否则 `RevisionConflict`；改名唯一冲突按驱动 `is_unique_violation()` 分类固定 `InvalidState`(409)、不回显 driver 原文；`member_count` 同事务 `COUNT(*)` 真实回读（归档后恒 0）、返回 `GroupRecord` 在 commit 前安全组装；真实 CAS/行锁/级联清理/审计只能由 Task 14 受门禁真库用例补齐，本切片不冒充 SQL 层 GREEN）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：patch_group 槽位完整方法（本计划代码，非实际实现；
// 锁序 guard → actor 行 → device_groups 行 FOR UPDATE；expected_revision CAS；
// archived=true 物理删 group_members 与 group 作用域 grants（保留历史父 group 行、不删其它组）；
// UPDATE 影响恰 1 行否则 RevisionConflict；改名唯一冲突按 is_unique_violation() 分类 → InvalidState(409)、不回显 driver 原文；
// member_count 同事务 COUNT(*) 真实回读（归档后恒 0）；真实 CAS/行锁/级联清理/审计只能由 Task 14 受门禁真库用例补齐，本切片不冒充 SQL 层 GREEN）
fn patch_group<'a>(
    &'a self,
    actor: &'a Session,
    id: Uuid,
    expected_revision: u64,
    name: Option<&'a str>,
    archived: Option<bool>,
) -> RepoFuture<'a, GroupRecord> {
    Box::pin(async move {
        // 改名门禁复用 create_group 的 validate_group_name，不复制第二份 helper。
        if let Some(name) = name {
            validate_group_name(name)?;
        }
        let mut tx = begin_guard(&self.db).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
        // 1) 行锁快照当前 name/archived/revision，行缺失 → NotFound。
        let locked = sqlx::query(
            "SELECT name, CAST(archived AS SIGNED) AS archived, revision \
             FROM device_groups WHERE id = ? FOR UPDATE",
        )
        .bind(id.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| {
            Ok::<_, ControllerError>((
                r.try_get::<String, _>("name")?,
                crate::integrity::raw_bool(r.try_get("archived")?, "device_groups.archived")?,
                r.try_get::<u64, _>("revision")?,
            ))
        })
        .transpose()?
        .ok_or(ControllerError::NotFound)?;
        let (current_name, current_archived, current_revision) = locked;
        if current_revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }
        let next_revision =
            current_revision.checked_add(1).ok_or(ControllerError::RevisionConflict)?;
        let next_name: String = name.map(str::to_string).unwrap_or(current_name);
        let next_archived = archived.unwrap_or(current_archived);
        // 2) 归档时物理删成员与 group 作用域 grants（保留历史父 group 行、不删其它组）。
        if next_archived {
            sqlx::query("DELETE FROM group_members WHERE group_id = ?")
                .bind(id.as_bytes().as_slice())
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM grants WHERE scope_kind = 'group' AND scope_group_id = ?")
                .bind(id.as_bytes().as_slice())
                .execute(&mut *tx)
                .await?;
        }
        // 3) CAS UPDATE 影响恰 1 行，否则 RevisionConflict；改名唯一冲突 → InvalidState(409)。
        let updated = sqlx::query(
            "UPDATE device_groups SET name = ?, archived = ?, revision = ? WHERE id = ? AND revision = ?",
        )
        .bind(&next_name)
        .bind(next_archived)
        .bind(next_revision)
        .bind(id.as_bytes().as_slice())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            // 驱动 unique-violation 分类（同 create_group 口径：不解析 message 文本、不依赖 constraint 名称）。
            if error
                .as_database_error()
                .is_some_and(|e| e.is_unique_violation())
            {
                ControllerError::InvalidState
            } else {
                error.into()
            }
        })?;
        if updated.rows_affected() != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        // 4) 同事务 COUNT(*) 真实回读 member_count（归档后恒 0），不从参数臆造。
        let member_count = sqlx::query(
            "SELECT COUNT(*) AS member_count FROM group_members WHERE group_id = ?",
        )
        .bind(id.as_bytes().as_slice())
        .fetch_one(&mut *tx)
        .await?
        .try_get::<u64, _>("member_count")?;
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.group.patch",
                target_kind: "group",
                target_id: Some(hex::encode(id.as_bytes())),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;
        // commit 前安全组装 GroupRecord 全部 5 字段；仅 commit 成功才 Ok(record)。
        let record = GroupRecord {
            id,
            name: next_name,
            archived: next_archived,
            revision: next_revision,
            member_count,
        };
        tx.commit().await?;
        Ok(record)
    })
}
```

`create_role` 槽位完整方法（与 create_group 同骨架，目标表 `roles`；锁序 guard → actor 行 → 无实体锁（新行插入）；先过 `validate_permission_list` 门禁（非空、无重复，复用 Task 4 Step 3 完整定义）与 name 门禁（非空、≤128 Unicode 字符、无控制字符，与 `validate_group_name`/`validate_display_name` 同款规则，内联实现不复制第二份 helper）；`INSERT INTO roles (id, name, builtin, archived, revision) VALUES (?, ?, FALSE, FALSE, 1)`，name 唯一键 `uq_roles_name` 与 `id` 主键冲突均按驱动 `is_unique_violation()` 分类固定 `InvalidState`(409)、不回显 driver 原文、不识别具体索引；随后逐条插入 `role_permissions (role_id, permission)` 全部权限（`permission.as_str()` 文本与 Task 3 `Permission::parse` 互逆）；`bump_authz_epoch` 与 `identity.role.create` 成功审计（target_kind `role`、target_id 小写 hex、params_redacted 恒 `"{}"`）；commit 前安全组装 `RoleRecord`（builtin=false、archived=false、revision=1、permissions 为入参去重集合），仅 commit 成功才 `Ok(record)`；真实唯一冲突/审计只能由 Task 14 受门禁真库用例补齐，本切片不冒充 SQL 层 GREEN）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：create_role 槽位完整方法（本计划代码，非实际实现；
// 其余 trait 方法未齐，不得把缺其余方法的"独立完整 impl"冒称可编译）。局部 tx 为 owned（begin_guard），
// 锁序 guard → actor 行 → 无实体锁（新行插入）；方法内直接 SQL 用 &mut *tx，helper 参数一律传 &mut tx。
fn create_role<'a>(
    &'a self,
    actor: &'a Session,
    name: &'a str,
    permissions: &'a [Permission],
) -> RepoFuture<'a, RoleRecord> {
    Box::pin(async move {
        // 权限列表门禁复用 Task 4 Step 3 的 validate_permission_list（非空、无重复），不复制第二份 helper。
        crate::authz::policy::validate_permission_list(permissions)?;
        // name 门禁与 validate_group_name/validate_display_name 同款规则：非空、≤128 Unicode 字符、无控制字符。
        if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
            return Err(ControllerError::InvalidArgument);
        }
        let mut tx = begin_guard(&self.db).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
        // 新行插入，无实体行锁；id 主键与 name 唯一键 uq_roles_name 均为唯一冲突来源。
        let id = Uuid::new_v4();
        if let Err(error) = sqlx::query(
            "INSERT INTO roles (id, name, builtin, archived, revision) VALUES (?, ?, FALSE, FALSE, 1)",
        )
        .bind(id.as_bytes().as_slice())
        .bind(name)
        .execute(&mut *tx)
        .await
        {
            // 唯一冲突固定 InvalidState(409)，不回显 driver 原文；其他 SQL 错误透传 Database（既有 backend error 统一 503 去敏）。
            // 用驱动 unique-violation 分类（SQLx 0.8.6 `DatabaseError::is_unique_violation()`：
            // `kind()==UniqueViolation`；MySQL/TiDB 同走 MySQL 协议，重复条目错误码映射为该 kind）：
            // 不解析 driver message 文本，也不依赖 Postgres-only 的 `constraint()` 名称映射。
            let unique_conflict = error
                .as_database_error()
                .is_some_and(|e| e.is_unique_violation());
            return Err(if unique_conflict {
                ControllerError::InvalidState
            } else {
                error.into()
            });
        }
        // 全部权限逐条插入 role_permissions（permission 文本与 Task 3 解码 Permission::parse 互逆）。
        for permission in permissions {
            sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES (?, ?)")
                .bind(id.as_bytes().as_slice())
                .bind(permission.as_str())
                .execute(&mut *tx)
                .await?;
        }
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.role.create",
                target_kind: "role",
                target_id: Some(hex::encode(id.as_bytes())),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;
        // commit 前安全组装 RoleRecord 全部 6 字段；仅 commit 成功才 Ok(record)。
        let record = RoleRecord {
            id,
            name: name.to_string(),
            builtin: false,
            archived: false,
            revision: 1,
            permissions: permissions.iter().copied().collect(),
        };
        tx.commit().await?;
        Ok(record)
    })
}
```

`patch_role` 槽位完整方法：

```rust
// patch_role 槽位完整方法
fn patch_role<'a>(
    &'a self,
    actor: &'a Session,
    id: Uuid,
    expected_revision: u64,
    name: Option<&'a str>,
    permissions: Option<&'a [Permission]>,
    archived: Option<bool>,
) -> RepoFuture<'a, RoleRecord> {
    Box::pin(async move {
        if let Some(perms) = permissions {
            crate::authz::policy::validate_permission_list(perms)?;
        }
        if let Some(n) = name {
            if n.is_empty() || n.chars().count() > 128 || n.chars().any(char::is_control) {
                return Err(ControllerError::InvalidArgument);
            }
        }
        let mut tx = begin_guard(&self.db).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
        let (locked_name, locked_builtin, locked_archived, current_revision) = sqlx::query(
            "SELECT name, CAST(builtin AS SIGNED) AS builtin, CAST(archived AS SIGNED) AS archived, revision FROM roles WHERE id = ? FOR UPDATE",
        )
        .bind(id.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| {
            Ok::<_, ControllerError>((
                r.try_get::<String, _>("name")?,
                crate::integrity::raw_bool(r.try_get("builtin")?, "roles.builtin")?,
                crate::integrity::raw_bool(r.try_get("archived")?, "roles.archived")?,
                r.try_get::<u64, _>("revision")?,
            ))
        })
        .transpose()?
        .ok_or(ControllerError::NotFound)?;

        if current_revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }
        if locked_builtin && archived == Some(true) {
            return Err(ControllerError::InvalidState);
        }

        let next_name = name.map(str::to_string).unwrap_or(locked_name);
        let next_archived = archived.unwrap_or(locked_archived);
        // 恢复分支门禁（方案 A）：从归档恢复必须携带合法非空 permissions，否则 UPDATE 前
        // `InvalidArgument`(400 `INVALID_ARGUMENT`) 拒绝，绝不留下 active 空权限非法状态。
        validate_role_restore(locked_archived, next_archived, permissions)?;
        let next_revision = current_revision.checked_add(1).ok_or(ControllerError::RevisionConflict)?;

        if next_archived && !locked_archived {
            sqlx::query("DELETE FROM grants WHERE source_kind = 'role' AND role_id = ?")
                .bind(id.as_bytes().as_slice())
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM role_permissions WHERE role_id = ?")
                .bind(id.as_bytes().as_slice())
                .execute(&mut *tx)
                .await?;
        }

        let updated = sqlx::query(
            "UPDATE roles SET name = ?, archived = ?, revision = ? WHERE id = ? AND revision = ?",
        )
        .bind(&next_name)
        .bind(next_archived)
        .bind(next_revision)
        .bind(id.as_bytes().as_slice())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            if error.as_database_error().is_some_and(|e| e.is_unique_violation()) {
                ControllerError::InvalidState
            } else {
                error.into()
            }
        })?;
        if updated.rows_affected() != 1 {
            return Err(ControllerError::RevisionConflict);
        }

        if let Some(perms) = permissions {
            if !next_archived {
                sqlx::query("DELETE FROM role_permissions WHERE role_id = ?")
                    .bind(id.as_bytes().as_slice())
                    .execute(&mut *tx)
                    .await?;
                for p in perms {
                    sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES (?, ?)")
                        .bind(id.as_bytes().as_slice())
                        .bind(p.as_str())
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }

        let permissions: Vec<crate::authz::Permission> = if next_archived {
            Vec::new()
        } else {
            sqlx::query("SELECT permission FROM role_permissions WHERE role_id = ?")
                .bind(id.as_bytes().as_slice())
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .map(|r| crate::authz::Permission::parse(&r.try_get::<String, _>("permission")?).ok_or(ControllerError::InvalidState))
                .collect::<Result<_, _>>()?
        };

        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.role.patch",
                target_kind: "role",
                target_id: Some(hex::encode(id.as_bytes())),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;

        tx.commit().await?;
        Ok(RoleRecord {
            id,
            name: next_name,
            builtin: locked_builtin,
            archived: next_archived,
            revision: next_revision,
            permissions,
        })
    })
}

// 模块级纯 helper（置于 impl 外，与本槽位一同提取，可离线 RED→GREEN）：归档恢复（`locked_archived && !next_archived`）
// 必须携带合法非空权限集——`None` → `InvalidArgument`(400 `INVALID_ARGUMENT`)；`Some(空/重复)` 由 `validate_permission_list` 拒绝。
// 非恢复分支（归档、active 原地编辑、保持归档）不受限；与同事务重建+回读共同保证永不产生 active 空权限角色。
fn validate_role_restore(
    locked_archived: bool,
    next_archived: bool,
    permissions: Option<&[Permission]>,
) -> Result<(), ControllerError> {
    if locked_archived && !next_archived {
        match permissions {
            None => Err(ControllerError::InvalidArgument),
            Some(perms) => crate::authz::policy::validate_permission_list(perms),
        }
    } else {
        Ok(())
    }
}
```

`patch_role` 恢复分支专项增量（正反例；纯门禁离线 RED→GREEN，真库闭环留 Task 14）：

```rust
// authz/sqlx_repo.rs tests内追加；policy类型用全路径，避免额外导入。
#[test]
fn role_restore_from_archive_requires_valid_non_empty_permissions() {
    use crate::authz::policy::Permission::*;
    // 正例：合法非空权限恢复 → 门禁过；同事务重建+回读得 active 非空权限，snapshot 角色解码不再 503，授权读取正常。
    assert!(validate_role_restore(true, false, Some(&[DeviceRead, DeviceReboot])).is_ok());
    // 反例：无权限恢复 → InvalidArgument(400)，绝不产生 active 空权限角色。
    assert!(matches!(
        validate_role_restore(true, false, None),
        Err(ControllerError::InvalidArgument)
    ));
    // 反例：空权限恢复 → validate_permission_list 非空门禁拒绝（同为 400 形状）。
    assert!(validate_role_restore(true, false, Some(&[])).is_err());
    // 非恢复分支不受限：归档、active 原地编辑、保持归档。
    assert!(validate_role_restore(false, true, None).is_ok());
    assert!(validate_role_restore(false, false, None).is_ok());
    assert!(validate_role_restore(true, true, None).is_ok());
}
```

未来选择器`cargo test --offline --locked -p rsetup-controller role_restore_from_archive_requires_valid_non_empty_permissions`恰好1方法；RED首反例（无权限恢复）因门禁缺失放行而失败，GREEN正反矩阵全过。真库闭环（归档→携合法非空权限恢复成功→`load_snapshot_in_tx`角色解码不再503、授权读取恢复；无权限/空权限恢复被拒400且事务零修改）由Task 14受门禁真库用例补齐，本切片不冒充SQL层GREEN。

`create_grant` 槽位完整方法（与上方同骨架，目标表 `grants`；先以纯函数 `grant_columns(&source, &scope)?` 完成 source/scope 互斥校验与列映射（不进入事务，互斥失败按 `grant_columns` 既有错误透传）；锁序 guard → actor 行 → `users`(target) 行 `FOR UPDATE` → 依 `scope_kind` 锁定 `roles`/`device_groups`/`devices` 中唯一对应行 `FOR UPDATE`；target 用户行缺失 → `NotFound`、`active=FALSE` → `InvalidState`；scope 对应实体行缺失 → `NotFound`、`archived=TRUE` → `InvalidState`；`INSERT` 新行 `revision=1`，主键与唯一约束冲突按驱动 `is_unique_violation()` 分类固定 `InvalidState`(409)、不回显 driver 原文、不识别具体索引；同事务 `bump_authz_epoch` 与 `identity.grant.create` 成功审计（target_kind `grant`、target_id 为新 grant id 小写 hex、params_redacted 恒 `"{}"`）；commit 前安全组装 `GrantRecord { id, user_id, source, scope, revision: 1 }`，仅 commit 成功才 `Ok(record)`；任一错误 drop 局部 tx 自动回滚；真实行锁/唯一冲突/审计只能由 Task 14 受门禁真库用例补齐，本切片不冒充 SQL 层 GREEN）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：create_grant 槽位完整方法（本计划代码，非实际实现；
// 锁序 guard → actor 行 → users(target) → 依 scope 的 roles/device_groups/devices，全部 FOR UPDATE）。
fn create_grant<'a>(
    &'a self,
    actor: &'a Session,
    user_id: [u8; 16],
    source: GrantSource,
    scope: GrantScope,
) -> RepoFuture<'a, GrantRecord> {
    Box::pin(async move {
        // 前置校验：纯函数先做 source/scope 互斥与列映射，失败不进入事务。
        let cols = grant_columns(&source, &scope)?;
        let mut tx = begin_guard(&self.db).await?;
        // 锁 1：复用共享助手，按 UUID 字节序锁定 actor 与 target（同一用户仅锁一次），
        // 并在 guard 内重新从行核验 actor active && is_admin（对齐管理写事务契约第 7 条）。
        let target = lock_user_pair_and_verify_actor(&mut tx, actor.user.id, user_id)
            .await?
            .ok_or(ControllerError::NotFound)?;
        if !target.active {
            return Err(ControllerError::InvalidState);
        }

        if cols.source_kind == "role" {
            let role_id = cols.role_id.expect("source=role 时 grant_columns 必给出 role_id");
            let archived: i32 =
                sqlx::query_scalar("SELECT CAST(archived AS SIGNED) FROM roles WHERE id = ? FOR UPDATE")
                    .bind(role_id.as_bytes().as_slice())
                    .fetch_optional(&mut *tx)
                    .await?
                    .ok_or(ControllerError::NotFound)?;
            if archived != 0 {
                return Err(ControllerError::InvalidState);
            }
        }

        // 锁 2：依 scope_kind 锁定唯一对应实体行 FOR UPDATE；行缺失 → NotFound，archived → InvalidState。
        // （`grant_columns` 已按 scope 保证相应列恰有其一，此处不做第二种 scope 解析。）
        match cols.scope_kind {
            "all" => {
                // All 范围无需锁实体行
            }
            "group" => {
                let group_id = cols.scope_group_id.expect("scope=group 时 grant_columns 必给出 scope_group_id");
                let archived: i32 =
                    sqlx::query_scalar("SELECT CAST(archived AS SIGNED) FROM device_groups WHERE id = ? FOR UPDATE")
                        .bind(group_id.as_bytes().as_slice())
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or(ControllerError::NotFound)?;
                if archived != 0 {
                    return Err(ControllerError::InvalidState);
                }
            }
            "device" => {
                let device_id = cols.scope_device_id.expect("scope=device 时 grant_columns 必给出 scope_device_id");
                let archived: i32 =
                    sqlx::query_scalar("SELECT CAST(archived AS SIGNED) FROM devices WHERE public_key = ? FOR UPDATE")
                        .bind(device_id.as_slice())
                        .fetch_optional(&mut *tx)
                        .await?
                        .ok_or(ControllerError::NotFound)?;
                if archived != 0 {
                    return Err(ControllerError::InvalidState);
                }
            }
            _ => unreachable!("grant_columns 只产生 all/group/device 三种 scope_kind"),
        }

        // 写入：revision 恒 1；主键/唯一冲突按驱动分类固定 InvalidState(409)，
        // 其余 SQL 错误透传 Database（既有 backend error 统一 503 去敏）。
        let id = Uuid::new_v4();
        let role_binding = cols.role_id.map(|r| r.as_bytes().to_vec());
        let group_binding = cols.scope_group_id.map(|g| g.as_bytes().to_vec());
        let device_binding = cols.scope_device_id.map(|k| k.to_vec());
        if let Err(error) = sqlx::query(
            "INSERT INTO grants (id, user_id, source_kind, role_id, permissions, scope_kind, scope_group_id, scope_device_id, revision) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1)",
        )
        .bind(id.as_bytes().as_slice())
        .bind(user_id.as_slice())
        .bind(cols.source_kind)
        .bind(&role_binding)
        .bind(&cols.permissions_json)
        .bind(cols.scope_kind)
        .bind(&group_binding)
        .bind(&device_binding)
        .execute(&mut *tx)
        .await
        {
            // 用驱动 unique-violation 分类（同 create_group 口径：
            // `kind()==UniqueViolation`，不解析 driver message、不依赖 constraint 名称）。
            let unique_conflict = error
                .as_database_error()
                .is_some_and(|e| e.is_unique_violation());
            return Err(if unique_conflict {
                ControllerError::InvalidState
            } else {
                error.into()
            });
        }
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.grant.create",
                target_kind: "grant",
                target_id: Some(hex::encode(id.as_bytes())),
                outcome: "success",
                params_redacted: "{}",
            },
        )
        .await?;
        // commit 前安全组装（source/scope 为入参原样持有；user_id 与 INSERT 绑定同源）：
        let record = GrantRecord { id, user_id, source, scope, revision: 1 };
        tx.commit().await?;
        Ok(record)
    })
}
```

`delete_grant` 槽位完整方法（与上方同骨架，目标表 `grants`；锁序 guard → actor 行 → `grants` 行 `FOR UPDATE`（按 `id` 字节定位）；行缺失 → `NotFound`；`current_revision != expected_revision` → `RevisionConflict`；物理删除 `DELETE FROM grants WHERE id = ? AND revision = ?` 并以 `rows_affected == 1` 双保险，不符 → `RevisionConflict`；同事务 `bump_authz_epoch` 与 `identity.grant.delete` 成功审计（target_kind `grant`、target_id 为被删 grant id 的小写 hex、params_redacted 恒 `"{}"`、outcome: `"success"`）；仅 commit 成功才 `Ok(())`；任一错误 drop 局部 tx 自动回滚；真实行锁/CAS/审计只能由 Task 14 受门禁真库用例补齐，本切片不冒充 SQL 层 GREEN）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：delete_grant 槽位完整方法（本计划代码，非实际实现；
// 其余 trait 方法未齐，不得把缺其余方法的"独立完整 impl"冒称可编译）。局部 tx 为 owned（begin_guard），
// 锁序 guard → actor 行 → `grants` 行（FOR UPDATE，按 id 字节定位）；方法内直接 SQL 用 &mut *tx，helper 参数一律传 &mut tx；
// Session/Uuid/hex/audit 复用既有引用，不新增 use。
fn delete_grant<'a>(
    &'a self,
    actor: &'a Session,
    id: Uuid,
    expected_revision: u64,
) -> RepoFuture<'a, ()> {
    Box::pin(async move {
        let mut tx = begin_guard(&self.db).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
        // `grants` 行 `FOR UPDATE`（按 `id.as_bytes().as_slice()` 定位）：行缺失 → `NotFound`，行锁持有至事务结束
        let current_revision = sqlx::query_scalar::<_, u64>(
            "SELECT revision FROM grants WHERE id = ? FOR UPDATE",
        )
        .bind(id.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ControllerError::NotFound)?;
        // expected_revision CAS（与 DELETE 的 WHERE revision=? + rows_affected==1 双保险）
        if current_revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }
        // 物理删除：id + revision 双条件，影响行非恰 1 即视为并发修订漂移
        let deleted = sqlx::query(
            "DELETE FROM grants WHERE id = ? AND revision = ?",
        )
        .bind(id.as_bytes().as_slice())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if deleted != 1 {
            return Err(ControllerError::RevisionConflict);
        }
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.grant.delete",
                target_kind: "grant",
                target_id: Some(hex::encode(id.as_bytes())),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;
        tx.commit().await?;
        Ok(())
    })
}
```

`create_user` 槽位完整方法（与上方同骨架，目标表 `users`；锁序 guard → actor 行 → 无实体锁（新行插入）；先过三门禁：`crate::db::valid_username`（非空、无空白/控制符，复用 Task 3 既有校验，不复制第二份 helper）、`validate_display_name`（复用 Task 3 完整定义）、`next_hash` 非空；`INSERT` 初始行状态 `active=TRUE`、`must_change_password=TRUE`（临时密码首登必改）、`revision=1`；INSERT 唯一键冲突——username 唯一键 `uq_users_username` 与 `id` 主键均为唯一冲突——按驱动 `is_unique_violation()` 分类、按实体规则固定 `InvalidState`(409)，不回显 driver 原文、不识别具体索引；随后 `bump_authz_epoch` 与 `identity.user.create` 成功审计（target_kind `user`、target_id 为新 user id 的小写 hex、params_redacted 恒 `"{}"`）；commit 前安全组装 `UserRecord` 全部 7 字段（绝不含 `password_hash`），仅 commit 成功才 `Ok(record)`；任一错误 drop 局部 tx 自动回滚；真实唯一冲突/审计只能由 Task 14 受门禁真库用例补齐，本切片不冒充 SQL 层 GREEN）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs：create_user 槽位完整方法（本计划代码，非实际实现；
// 其余 trait 方法未齐，不得把缺其余方法的"独立完整 impl"冒称可编译）。局部 tx 为 owned（begin_guard），
// 锁序 guard → actor 行 → 无实体锁（新行插入）；方法内直接 SQL 用 &mut *tx，helper 参数一律传 &mut tx；
// Session/Uuid/hex/audit 复用既有引用，不新增 use。
fn create_user<'a>(
    &'a self,
    actor: &'a Session,
    username: &'a str,
    display_name: &'a str,
    is_admin: bool,
    next_hash: &'a str,
) -> RepoFuture<'a, UserRecord> {
    Box::pin(async move {
        // 门禁1：display_name 合法（复用 Task 3 完整 validate_display_name）。
        validate_display_name(display_name)?;
        // 门禁2：next_hash 非空（Argon2id 临时密码哈希，事务前即拒绝空串）。
        if next_hash.is_empty() {
            return Err(ControllerError::InvalidArgument);
        }
        // 门禁3：username 规范（复用 Task 3 既有 crate::db::valid_username：非空、无空白/控制符）。
        if !crate::db::valid_username(username) {
            return Err(ControllerError::InvalidArgument);
        }
        let mut tx = begin_guard(&self.db).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
        // 新行插入无实体行锁（guard 行锁已串行化全部管理员写）。
        let id = Uuid::new_v4();
        if let Err(error) = sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash, active, is_admin, must_change_password, revision, created_time) VALUES (?, ?, ?, ?, TRUE, ?, TRUE, 1, NOW(6))",
        )
        .bind(id.as_bytes().as_slice())
        .bind(username)
        .bind(display_name)
        .bind(next_hash)
        .bind(is_admin)
        .execute(&mut *tx)
        .await
        {
            // 唯一冲突固定 InvalidState(409)，不回显 driver 原文；其他 SQL 错误透传 Database（既有 backend error 统一 503 去敏）。
            // 用驱动 unique-violation 分类（SQLx 0.8.6 `DatabaseError::is_unique_violation()`：
            // `kind()==UniqueViolation`；MySQL/TiDB 同走 MySQL 协议，重复条目错误码映射为该 kind）：
            // 不解析 driver message 文本，也不依赖 Postgres-only 的 `constraint()` 名称映射；
            // username 唯一键 uq_users_username 与 id 主键均为唯一冲突，不识别具体索引。
            let unique_conflict = error
                .as_database_error()
                .is_some_and(|e| e.is_unique_violation());
            return Err(if unique_conflict {
                ControllerError::InvalidState
            } else {
                error.into()
            });
        }
        bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.user.create",
                target_kind: "user",
                target_id: Some(hex::encode(id.as_bytes())),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;
        // commit 前构造安全 UserRecord 全部 7 字段（绝不含 password_hash）；仅 commit 成功才 Ok(record)。
        let record = UserRecord {
            id: id.into_bytes(),
            username: username.to_string(),
            display_name: display_name.to_string(),
            active: true,
            is_admin,
            must_change_password: true,
            revision: 1,
        };
        tx.commit().await?;
        Ok(record)
    })
}
```

`validate_group_name` 完整纯 helper（create_group 的 name 门禁；`VARCHAR(128)+utf8mb4` 为 Unicode 字符数，与 `validate_display_name` 的字节 255 规则不同；不 trim 后改名、不自造新 API 字段；已核无同语义既有 helper，不重复定义）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs
pub(crate) fn validate_group_name(value: &str) -> Result<(), ControllerError> {
    if value.trim().is_empty()
        || value.chars().count() > 128
        || value.chars().any(|c| c.is_control())
    {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(())
}
```

该 helper 的 lib 测试（表驱动、纯函数、无需真库；Step 0 桩阶段若 helper 桩为恒 `Ok(())`，首坏输入 Err 断言即 RED，按上方实现真实 helper 后 GREEN；本测试只验证 `validate_group_name` 命名规则，不是 SQL 唯一冲突映射的实证——驱动 `is_unique_violation()` 分类在 MySQL/TiDB 的实际验证仍待门禁后真库跑，不为本测试伪造真实数据库错误回执；本例属 `--lib` 测试二进制，不改变 Task 14 `identity_admin_db` 默认 `13 passed; 4 ignored`；本轮仅写定片段与命令，不运行）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs（追加到 mod tests）
#[test]
fn validate_group_name_boundaries() {
    let legal_128: String = "群".repeat(128); // 128 Unicode 字符 / 384 字节
    assert_eq!(legal_128.chars().count(), 128);
    let over_129: String = "群".repeat(129); // 129 字符
    let cjk_300_bytes: String = "群".repeat(100); // 100 字符 / 300 字节：字符数规则合法、字节 255 规则会拒
    let cases: [(&str, bool); 8] = [
        ("ops-eu-1", true),
        (legal_128.as_str(), true),
        (cjk_300_bytes.as_str(), true),
        ("", false),
        ("   ", false),
        (over_129.as_str(), false),
        ("a\tb", false),
        ("\u{1}", false), // 第 8 例：另一明确控制字符负例（U+0001），与 [;& 8] 长度声明一致
    ];
    for (input, expect_ok) in cases {
        assert_eq!(validate_group_name(input).is_ok(), expect_ok, "input = {input:?}");
    }
}
```

运行命令（`--lib` 完整 path + `--exact` 恰好锁定该 1 例；本轮不运行）：

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib -- authz::sqlx_repo::tests::validate_group_name_boundaries --exact
```

```rust
// crates/rsetup-controller/src/devices/repository.rs
// 成员替换纯规整 helper：显式去重、字节序排定、上限 1024 校验（0 为合法清空）
pub(crate) fn normalize_device_member_ids(
    device_ids: &[[u8; 32]],
) -> Result<Vec<[u8; 32]>, ControllerError> {
    let mut sorted: Vec<[u8; 32]> = device_ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() > 1024 {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(sorted)
}

// SqlxDeviceRepository::patch_device_display_name 完整写事务实现
fn patch_device_display_name<'a>(
    &'a self,
    actor: &'a Session,
    device_id: [u8; 32],
    expected_revision: u64,
    display_name: &'a str,
) -> RepoFuture<'a, DeviceAdminView> {
    Box::pin(async move {
        use sqlx::Row;
        let mut tx = self.db.0.begin().await?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;
        // 1. 锁 devices 行（锁序止于 devices 行，guard → actor 行 → devices 行 FOR UPDATE）
        let row = sqlx::query(
            "SELECT public_key, display_name, descriptor_json, admission_state, \
             review_decision, revision, CAST(archived AS SIGNED) AS archived \
              FROM devices WHERE public_key = ? FOR UPDATE",
        )
        .bind(device_id.as_slice())
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ControllerError::NotFound)?;
        // 2. 严格 decode：复用同文件既有去敏解码（raw_bool/枚举映射/descriptor_json 去敏错误同既有 code），不复制第二份解码器
        let locked = crate::authz::sqlx_repo::decode_device_admin_view(
            row.try_get("public_key")?,
            row.try_get("display_name")?,
            row.try_get("descriptor_json")?,
            row.try_get("admission_state")?,
            row.try_get("review_decision")?,
            row.try_get::<u64, _>("revision")?,
            row.try_get::<i64, _>("archived")?,
        )?;

        // 3. expected_revision CAS（与 UPDATE 的 WHERE revision=? + rows_affected==1 双保险）
        if locked.revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }
        crate::authz::sqlx_repo::validate_display_name(display_name)?;
        let next_revision = locked.revision
            .checked_add(1)
            .ok_or(ControllerError::RevisionConflict)?;
        let changed = sqlx::query(
            "UPDATE devices SET display_name = ?, revision = ? WHERE public_key = ? AND revision = ?",
        )
        .bind(display_name)
        .bind(next_revision)
        .bind(device_id.as_slice())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(ControllerError::RevisionConflict);
        }

        // 4. bump authz_epoch 与 ADMIN 脱敏审计（target_id 为 public_key 的 hex）
        crate::authz::sqlx_repo::bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.device.patch",
                target_kind: "device",
                target_id: Some(hex::encode(device_id)),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;

        // 5. commit 前基于锁定行值构造全部 7 字段 DeviceAdminView：display_name 为新值、revision 为 checked+1；
        //    admission_state/review_decision/descriptor/first_seen/last_seen/archived 保持锁定行值，绝无二次读取。
        //    归档设备能否改名：规格无明确依据，待整稿裁决（不自造权限/路由/请求字段）
        tx.commit().await?;

        Ok(DeviceAdminView {
            public_key: locked.public_key,
            display_name: display_name.to_string(),
            admission_state: locked.admission_state,
            review_decision: locked.review_decision,
            revision: next_revision,
            descriptor: locked.descriptor,
            archived: locked.archived,
        })
    })
}
// 真实CAS/行锁/审计/回滚只能由Task14受门禁真库用例补齐，本切片不冒充SQL层GREEN。

// SqlxDeviceRepository::replace_group_members 完整写事务实现
fn replace_group_members<'a>(
    &'a self,
    actor: &'a Session,
    group_id: Uuid,
    expected_revision: u64,
    device_ids: &'a [[u8; 32]],
) -> RepoFuture<'a, GroupRecord> {
    Box::pin(async move {
        use sqlx::Row;
        let sorted_ids = normalize_device_member_ids(device_ids)?;
        let mut tx = self.db.0.begin().await?;
        crate::integrity::lock_integrity_guard(&mut tx).await?;
        crate::integrity::require_active_admin(&mut tx, actor.user.id).await?;

        // 1. 锁 device_groups 行，重验未归档且 CAS revision 匹配
        let group_row = sqlx::query(
            "SELECT name, CAST(archived AS SIGNED) AS archived, revision \
             FROM device_groups WHERE id = ? FOR UPDATE",
        )
        .bind(group_id.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await?;
        let (locked_name, group_archived, current_revision) = group_row
            .map(|r| {
                Ok::<_, ControllerError>((
                    r.try_get::<String, _>("name")?,
                    crate::integrity::raw_bool(r.try_get("archived")?, "device_groups.archived")?,
                    r.try_get::<u64, _>("revision")?,
                ))
            })
            .transpose()?
            .ok_or(ControllerError::NotFound)?;
        if group_archived {
            return Err(ControllerError::NotFound);
        }
        if current_revision != expected_revision {
            return Err(ControllerError::RevisionConflict);
        }

        // 2. 按公钥字节序逐个锁 devices 行，重验全部设备存在且未归档
        for device_id in &sorted_ids {
            let dev_row = sqlx::query(
                "SELECT CAST(archived AS SIGNED) AS archived FROM devices WHERE public_key = ? FOR UPDATE",
            )
            .bind(device_id.as_slice())
            .fetch_optional(&mut *tx)
            .await?;
            let is_archived = dev_row
                .map(|r| crate::integrity::raw_bool(r.try_get("archived")?, "devices.archived"))
                .transpose()?
                .ok_or(ControllerError::NotFound)?;
            if is_archived {
                return Err(ControllerError::NotFound);
            }
        }

        // 3. DELETE 旧成员，并按公钥序 INSERT 固定去重集
        sqlx::query("DELETE FROM group_members WHERE group_id = ?")
            .bind(group_id.as_bytes().as_slice())
            .execute(&mut *tx)
            .await?;
        for device_id in &sorted_ids {
            sqlx::query("INSERT INTO group_members (group_id, device_id) VALUES (?, ?)")
                .bind(group_id.as_bytes().as_slice())
                .bind(device_id.as_slice())
                .execute(&mut *tx)
                .await?;
        }

        // 4. group revision checked+1 CAS 更新
        let next_revision = current_revision
            .checked_add(1)
            .ok_or(ControllerError::RevisionConflict)?;
        let changed = sqlx::query(
            "UPDATE device_groups SET revision = ? WHERE id = ? AND revision = ?",
        )
        .bind(next_revision)
        .bind(group_id.as_bytes().as_slice())
        .bind(expected_revision)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(ControllerError::RevisionConflict);
        }

        // 5. bump authz_epoch 与 ADMIN 脱敏审计
        crate::authz::sqlx_repo::bump_authz_epoch(&mut tx).await?;
        crate::audit::insert_audit(
            &mut tx,
            &crate::audit::AuditInsert {
                actor_kind: "user",
                actor_user_id: Some(actor.user.id),
                event_type: "identity.group.members",
                target_kind: "group",
                target_id: Some(hex::encode(group_id.as_bytes())),
                params_redacted: "{}",
                outcome: "success",
            },
        )
        .await?;

        // 6. 提交事务；成功后直接基于已锁字段构造返回，绝无二次读取；commit 未知错误直接透传不谎称回滚
        tx.commit().await?;

        let member_count = u64::try_from(sorted_ids.len()).map_err(|_| ControllerError::InvalidArgument)?;
        Ok(GroupRecord {
            id: group_id,
            name: locked_name,
            archived: false,
            revision: next_revision,
            member_count,
        })
    })
}
```

- [ ] **Step 4: 运行并确认 GREEN + 既有写路径回归**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller audit::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller authz::sqlx_repo::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib
```
预期：全绿。第三条是**旧写路径保留的回归证据**（保留已有行为，不称"统一改造"）：本任务不改 `auth::sqlx_repo`/`db.rs`，其既有全部单元测试（含审计列序/绑定数断言、CAS 事务测试）必须与执行本任务前一致全绿。任何失败都说明管理写路径顺带碰到了旧写路径，必须修复实现而非改断言。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/audit.rs crates/rsetup-controller/src/authz/sqlx_repo.rs crates/rsetup-controller/src/authz/policy.rs crates/rsetup-controller/src/devices/repository.rs crates/rsetup-controller/src/lib.rs
git commit -m "feat(controller): 新增管理任务独立审计命名空间与管理员写事务"
```

---

### Task 5: 授权服务、设备服务扩展与来源归因纯函数

**Files:**
- Create: `crates/rsetup-controller/src/authz/service.rs`
- Modify: `crates/rsetup-controller/src/authz/mod.rs`（注册 `pub mod service;` 并 re-export `AuthzService`；policy re-export 行追加 `permission_sources, PermissionSourceKind, PermissionSourceEntry`）
- Modify: `crates/rsetup-controller/src/authz/policy.rs`（`permission_sources` 纯函数与 `PermissionSourceKind`/`PermissionSourceEntry` 类型）
- 不变: `crates/rsetup-controller/src/devices/service.rs`（保持准入单目标转换核心不变；生产 DeviceService 仅含 store 与 sink 负责准入，管理 CRUD 生产适配器直接消费 `DeviceRepository`，不向 DeviceService 强塞不存在的 devices 字段或多余泛型委托）
- Test: `src/authz/service.rs` 与 `src/authz/policy.rs` 的测试模块

**Interfaces:**
- Consumes: Task 3/4 的 `AuthzRepository`、`DeviceRepository`；**Task 3 内部快照层** `crate::authz::sqlx_repo::{AuthorizationSnapshot, AuthorizationSnapshotRepository, SqlxAuthorizationSnapshotRepository}`（本轮新增读依赖：求值路径经同事务核心读 actor 的 DB 当前快照，不依赖 `Session.user` 的授权字段；ordinary `effective_permissions` 不调用 admin-readonly helper，sources 经独立 protected 入口同 tx 重验 caller）；`PasswordHasher`；`crate::auth::service::Session`（仅认证身份，非授权真相）；`crate::authz::policy::*`（含本轮新增 `SnapshotActor`、`ALL_DEVICE_PERMISSIONS`、`is_current_admin`、`eval_memory_permissions`）。
- Produces:
  - `policy::permission_sources(user_id: [u8;16], device_id: [u8;32], grants: &[GrantRecord], roles: &HashMap<Uuid, RoleRecord>, device_groups: &HashSet<Uuid>, is_admin: bool) -> Vec<PermissionSourceEntry>`
    - `pub struct PermissionSourceEntry { pub permission: Permission, pub source: PermissionSourceKind }`
    - `pub enum PermissionSourceKind { Admin, Role(Uuid), Direct(Uuid) }`（确定性排序：permission 字典序 → kind 排序 → id 字节序；`effective-permissions` 路由直接使用）
  - `pub struct AuthzService<S: AuthorizationSnapshotRepository, W: AuthzRepository> { snapshots: Arc<S>, repo: Arc<W> }`（**本轮修订：两个泛型**——S 是内部快照读侧（C1 闭合：求值路径不再读 `Session.user` 的授权字段；ordinary `effective_permissions` 不调用 admin-readonly helper，sources 经 protected 入口同 tx 重验 caller）；W 是管理写仓储（Task 4 写方法）。原草案的 `devices: Arc<DeviceRepository>`/`hasher` 字段在新设计下是死字段（求值只读快照；Argon2id 哈希走自由函数 `generate_temporary_password` 内按需 `PasswordHasher::new()`），删除以避免 `-D warnings` 下 clippy `dead_code`）
    - `pub fn new(snapshots: Arc<S>, repo: Arc<W>) -> Self`
    - `pub async fn effective_permissions(&self, actor: &Session, device_id: [u8;32]) -> Result<HashSet<Permission>, ControllerError>`（同事务快照读 → 纯函数求值：`self.snapshots.load_snapshot(actor.user.id, &[device_id]).await` → `snapshot_effective_permissions(&snapshot, device_id)`（同次当前actor及设备生命周期，目标missing/archived同样空集）；admin shortcut 由 `is_current_admin(&snapshot.actor)`（active && is_admin && !must_change_password）判定，不满足则 admin 亦 0 权限（fail-closed））
    - `pub async fn require_permission(&self, actor: &Session, device_id: [u8;32], required: Permission) -> Result<(), ControllerError>`（`effective_permissions` 后收敛：缺失 → `PermissionDenied`）
    - `pub async fn effective_permissions_with_sources(&self, actor: &Session, target_user: [u8;16], device_id: [u8;32]) -> Result<Vec<PermissionSourceEntry>, ControllerError>`（protected 管理读取：`load_admin_target_snapshot(actor, target_user, &[device_id])`，快照按 **`target_user`** 加载——同一读事务内先以 DB raw_bool 重验 `actor` 为当前 admin，HTTP Users 的 `require_admin_session` 只是前置，服务/仓储权威检查不省；授权真相是目标用户的 DB 当前快照；目标快照经`snapshot_permission_sources`：target inactive/must-change及missing/archived设备先空集合，无论target admin；其余按同快照effective集合归因，来源permission集合恰等于effective）
    - `pub async fn create_user(&self, actor: &Session, username: &str, display_name: &str, is_admin: bool) -> Result<(UserRecord, String), ControllerError>`（**在进入仓储事务前**用 OsRng 生成 32 字节 CSPRNG 临时密码并用 Argon2id 哈希，事务只耗纳秒级 SQL）
    - `pub async fn reset_user_password(&self, actor: &Session, user_id: [u8;16], expected_revision: u64) -> Result<(UserRecord, String), ControllerError>`（同上，哈希在事务外）
  - 管理设备 CRUD 架构对齐：原死委托 `DeviceService::patch_profile`/`replace_group_members` 泛型概括删除，生产由 `DbPool` + `NotificationBridge` 构造的 `DeviceService<DbPool, NotificationBridge>` 原样负责准入；管理 CRUD 生产适配器直接委托 `Arc<DeviceRepository>`（`SqlxDeviceRepository`），不访问不存在的 `devices` 字段，职责清晰且不产生死代码。

生命周期与来源集合统一纯helper（service.rs，Task5新增目标；均为内部投影，不新增HTTP字段）：

```rust
pub fn snapshot_effective_permissions(snapshot: &AuthorizationSnapshot, device_id: [u8; 32]) -> HashSet<Permission> {
    if !snapshot.actor.active || snapshot.actor.must_change_password
        || !snapshot.devices.get(&device_id).is_some_and(|device| !device.archived)
    {
        return HashSet::new();
    }
    eval_memory_permissions(
        &snapshot.actor, device_id, &snapshot.grants, &snapshot.roles,
        &snapshot.device_groups.get(&device_id).cloned().unwrap_or_default(),
    )
}

pub fn snapshot_permission_sources(snapshot: &AuthorizationSnapshot, device_id: [u8; 32]) -> Vec<PermissionSourceEntry> {
    let effective = snapshot_effective_permissions(snapshot, device_id);
    if effective.is_empty() { return vec![]; }
    let mut entries = permission_sources(
        snapshot.actor.id, device_id, &snapshot.grants, &snapshot.roles,
        &snapshot.device_groups.get(&device_id).cloned().unwrap_or_default(),
        is_current_admin(&snapshot.actor),
    );
    entries.retain(|entry| effective.contains(&entry.permission));
    entries
}
```

普通effective_permissions经ordinary仓储后调用第一helper；protected sources经当前caller重验后调用第二helper。目标inactive/must-change以及missing/archived设备都无有效集合/来源；caller当前admin的资格绝不能使目标“变活跃”。sources保留全部合法归因但其permission集合必须恰等于effective集合，不只做子集断言。HTTP显式requested missing/archived由网关同事务检查统一404；内部纯helper空集不充当持久dangling成功，引用污染仍在仓储/build校验503。

RED前置两个新helper暂恒空集合/空vec，签名完整；新增测试以下先有权正例必失败，再由上方GREEN替换，不能以原sources错误ordinary桩覆盖本增量的RED。

```rust
// service.rs tests内追加；imports复用同mod的AuthorizationSnapshot/policy/std types。
fn source_device_fixture(id: [u8; 32], archived: bool) -> crate::devices::repository::DeviceAdminView {
    crate::devices::repository::DeviceAdminView {
        public_key: id, display_name: "synthetic-device".into(),
        admission_state: crate::model::AdmissionState::Approved,
        review_decision: crate::model::ReviewDecision::Approved,
        revision: 1, descriptor: None, archived,
    }
}

#[test]
fn sources_permission_set_equals_effective_for_target_lifecycle() {
    let device_id = [0xa1; 32];
    for (active, must_change, is_admin, exists, archived) in [
        (true, false, false, true, false), // direct成功阳性，禁止恒空假绿
        (true, false, true, true, false),  // admin全集阳性
        (false, false, false, true, false),
        (false, false, true, true, false),
        (true, true, false, true, false),
        (true, true, true, true, false),
        (true, false, false, false, false),
        (true, false, true, false, false),
        (true, false, false, true, true),
        (true, false, true, true, true),
    ] {
        let mut devices = HashMap::new();
        if exists { devices.insert(device_id, source_device_fixture(device_id, archived)); }
        let snapshot = AuthorizationSnapshot {
            actor: SnapshotActor { id: [2; 16], active, is_admin, must_change_password: must_change },
            authz_epoch: 1,
            grants: vec![GrantRecord {
                id: Uuid::new_v4(), user_id: [2; 16],
                source: GrantSource::Direct(vec![Permission::DeviceRead]),
                scope: GrantScope::All, revision: 1,
            }],
            roles: HashMap::new(), device_groups: HashMap::new(), devices,
        };
        let effective = snapshot_effective_permissions(&snapshot, device_id);
        let sources = snapshot_permission_sources(&snapshot, device_id);
        let source_set: HashSet<_> = sources.iter().map(|entry| entry.permission).collect();
        assert_eq!(source_set, effective);
        let valid = active && !must_change && exists && !archived;
        if valid && is_admin {
            assert_eq!(effective, HashSet::from(ALL_DEVICE_PERMISSIONS));
        } else if valid {
            assert_eq!(effective, HashSet::from([Permission::DeviceRead]));
        } else {
            assert!(effective.is_empty());
            assert!(sources.is_empty());
        }
    }
}
```

未来新增选择器`cargo test --offline --locked -p rsetup-controller sources_permission_set_equals_effective_for_target_lifecycle`从设计根带主CARGO_HOME/私有target运行，恰好1方法/10场景；RED首direct阳性expected DeviceRead但空，GREEN十场景均过。Task5 service由6变7方法，policy另1不计入service。原三类caller测试fixture必须有实际合成设备行：`SourceSnapshotRepo::fresh_target_snapshot` devices包含a1；普通组授权测试devices包含a1/a2；不得让空devices遮盖新生命周期检查导致旧阳性失败。

- [ ] **Step 1: 编写失败测试（来源归因纯函数 + 临时密码在事务外生成 + 快照化求值路径）**

RED 阶段先写下列**可编译但行为错误**的桩（与 Step 2 预期一致；Step 3 逐项以真实实现替换），然后写断言。纯类型 `PermissionSourceKind`/`PermissionSourceEntry` 可为真实定义（仅类型、无行为）；行为桩：

- `permission_sources`（policy.rs）：恒 `vec![]`（不区分 admin/role/direct、不排序去重）——admin 分支 4 条断言失败；
- `generate_temporary_password`（service.rs）：恒 `Ok(("0".repeat(64), "argon2id$not-a-real-hash".to_string()))`——测试的 `verify` 对非 Argon2 串解析失败；
- `AuthzService`（service.rs，见代码块）：`effective_permissions` 恒返回 `ALL_DEVICE_PERMISSIONS` 全量（不读快照、不判定 `is_current_admin`/`must_change_password`、不过滤 grants）——"改密后 admin 0 权限"与"普通用户设备 A 仅 1 权限"断言失败；`effective_permissions_with_sources` 故意忽略 caller，走 ordinary `load_snapshot(target_user, &[device_id])` 后返回空 vec（可编译，新增三类测试实际调用）；其余 3 方法 `todo!()`（可编译，本步测试不调用）。
- 三个测试 fixture 仓储是 fixture 而非行为桩（完整定义，见代码块）：`StubSnapshotRepo`（固定快照，断言调用方只读本人快照——自隔离边界）、`SourceSnapshotRepo`（独立当前 caller / 合成 target / ordinary 与 protected 调用 spy）、`StubAuthzRepo`（trait 方法全 `unreachable!`，本步测试的读路径不触碰它）。

```rust
// crates/rsetup-controller/src/authz/policy.rs（追加）
#[test]
fn permission_sources_attributes_admin_role_and_direct_deterministically() {
    let user = [1u8; 16];
    let dev = [2u8; 32];
    // 管理员：每个权限的来源都是 Admin。
    let entries = permission_sources(user, dev, &[], &HashMap::new(), &HashSet::new(), true);
    assert_eq!(entries.len(), 4);
    assert!(entries.iter().all(|e| matches!(e.source, PermissionSourceKind::Admin)));

    // 普通用户：归因到具体的 grant.id（direct）或 role_id（role）。
    let grant_id = uuid::Uuid::from_bytes([5u8; 16]);
    let direct = GrantRecord {
        id: grant_id,
        user_id: user,
        source: GrantSource::Direct(vec![Permission::DeviceRead]),
        scope: GrantScope::All,
        revision: 1,
    };
    let entries = permission_sources(user, dev, &[direct], &HashMap::new(), &HashSet::new(), false);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].permission, Permission::DeviceRead);
    assert_eq!(entries[0].source, PermissionSourceKind::Direct(grant_id));
}
```

```rust
// crates/rsetup-controller/src/authz/service.rs（RED 桩：全部"可编译但行为错误"，Step 3 以真实实现替换）
use std::collections::HashSet;
use std::sync::Arc;

use crate::auth::service::Session;
use crate::authz::policy::{Permission, PermissionSourceEntry, ALL_DEVICE_PERMISSIONS};
use crate::authz::sqlx_repo::{AuthzRepository, AuthorizationSnapshotRepository, UserRecord};
use crate::ControllerError;

pub struct AuthzService<S: AuthorizationSnapshotRepository, W: AuthzRepository> {
    pub(crate) snapshots: Arc<S>,
    pub(crate) repo: Arc<W>,
}

impl<S: AuthorizationSnapshotRepository, W: AuthzRepository> AuthzService<S, W> {
    pub fn new(snapshots: Arc<S>, repo: Arc<W>) -> Self {
        Self { snapshots, repo }
    }

    /// RED 桩：恒返回 4 权限全量（不读快照、不判定 admin/must_change_password、不过滤 grants）。
    pub async fn effective_permissions(
        &self,
        _actor: &Session,
        _device_id: [u8; 32],
    ) -> Result<HashSet<Permission>, ControllerError> {
        Ok(HashSet::from(ALL_DEVICE_PERMISSIONS))
    }

    pub async fn require_permission(
        &self,
        _actor: &Session,
        _device_id: [u8; 32],
        _required: Permission,
    ) -> Result<(), ControllerError> {
        todo!("RED 桩：Step 3 替换为真实实现")
    }

    pub async fn effective_permissions_with_sources(
        &self,
        _actor: &Session,
        target_user: [u8; 16],
        device_id: [u8; 32],
    ) -> Result<Vec<PermissionSourceEntry>, ControllerError> {
        // 故意错误：忽略 caller，绕过 protected 入口，并丢弃目标来源。
        let _snapshot = self.snapshots.load_snapshot(target_user, &[device_id]).await?;
        Ok(Vec::new())
    }

    pub async fn create_user(
        &self,
        _actor: &Session,
        _username: &str,
        _display_name: &str,
        _is_admin: bool,
    ) -> Result<(UserRecord, String), ControllerError> {
        todo!("RED 桩：Step 3 替换为真实实现")
    }

    pub async fn reset_user_password(
        &self,
        _actor: &Session,
        _user_id: [u8; 16],
        _expected_revision: u64,
    ) -> Result<(UserRecord, String), ControllerError> {
        todo!("RED 桩：Step 3 替换为真实实现")
    }
}

/// RED 桩：固定 64 字符零串 + 非 Argon2 哈希串（测试的 `verify` 必须失败）。
pub fn generate_temporary_password() -> Result<(String, String), ControllerError> {
    Ok(("0".repeat(64), "argon2id$not-a-real-hash".to_string()))
}
```

```rust
// crates/rsetup-controller/src/authz/service.rs（测试：Step 1 一次写好，Step 3 不改测试）
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use uuid::Uuid;

    use crate::auth::password::PasswordHasher;
    use crate::auth::service::{IdentityUser, Session};
    use crate::authz::policy::{
        GrantRecord, GrantScope, GrantSource, Permission, RoleRecord, SnapshotActor,
    };
    use crate::authz::sqlx_repo::{
        AuthzRepository, AuthorizationSnapshot, AuthorizationSnapshotRepository, GroupRecord,
        RepoFuture, UserRecord,
    };

    // ── 测试 fixture：两个桩仓储（完整定义；本步测试的读路径只走 StubSnapshotRepo）──

    /// 固定快照桩：`load_snapshot` 断言调用方只读本人快照（自隔离边界），返回预置快照。
    struct StubSnapshotRepo {
        snapshot: AuthorizationSnapshot,
    }
    impl AuthorizationSnapshotRepository for StubSnapshotRepo {
        fn load_snapshot<'a>(
            &'a self,
            actor_id: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async move {
                assert_eq!(actor_id, self.snapshot.actor.id, "自隔离：只读本人快照");
                Ok(self.snapshot.clone())
            })
        }

        /// protected 管理读取桩：当前 fixture 只覆盖 ordinary effective 场景，这里明确 `PermissionDenied`
        /// 拒绝管理分支（非未定义/unreachable 占位）；三类 caller 边界由下方 SourceSnapshotRepo 覆盖。
        fn load_admin_target_snapshot<'a>(
            &'a self,
            _caller: &'a crate::auth::service::Session,
            _target_user: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
    }

    /// 管理写仓储桩：本步测试的读路径不触碰（完整定义、全方法 unreachable、不留 undefined fn）。
    struct StubAuthzRepo;
    impl AuthzRepository for StubAuthzRepo {
        fn find_user<'a>(&'a self, _actor: &'a Session, _id: [u8; 16]) -> RepoFuture<'a, Option<UserRecord>> {
            Box::pin(async { unreachable!("桩：读路径不触写仓储") })
        }
        fn list_users<'a>(&'a self, _actor: &'a Session, _cursor: Option<[u8; 16]>, _limit: usize) -> RepoFuture<'a, (Vec<UserRecord>, Option<[u8; 16]>)> {
            Box::pin(async { unreachable!() })
        }
        fn find_role<'a>(&'a self, _actor: &'a Session, _id: Uuid) -> RepoFuture<'a, Option<RoleRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn list_roles<'a>(&'a self, _actor: &'a Session, _cursor: Option<Uuid>, _limit: usize) -> RepoFuture<'a, (Vec<RoleRecord>, Option<Uuid>)> {
            Box::pin(async { unreachable!() })
        }
        fn find_grant<'a>(&'a self, _actor: &'a Session, _id: Uuid) -> RepoFuture<'a, Option<GrantRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn list_grants<'a>(&'a self, _actor: &'a Session, _user_id: Option<[u8; 16]>, _cursor: Option<Uuid>, _limit: usize) -> RepoFuture<'a, (Vec<GrantRecord>, Option<Uuid>)> {
            Box::pin(async { unreachable!() })
        }
        fn list_grants_for_user<'a>(&'a self, _actor: &'a Session, _user_id: [u8; 16]) -> RepoFuture<'a, Vec<GrantRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn roles_by_id<'a>(&'a self, _actor: &'a Session, _ids: Vec<Uuid>) -> RepoFuture<'a, HashMap<Uuid, RoleRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn device_groups_for_device<'a>(&'a self, _actor: &'a Session, _device_id: [u8; 32]) -> RepoFuture<'a, HashSet<Uuid>> {
            Box::pin(async { unreachable!() })
        }
        fn find_group<'a>(&'a self, _actor: &'a Session, _id: Uuid) -> RepoFuture<'a, Option<GroupRecord>> {
            Box::pin(async { unreachable!() })
        }
        fn list_groups<'a>(&'a self, _actor: &'a Session, _cursor: Option<Uuid>, _limit: usize) -> RepoFuture<'a, (Vec<GroupRecord>, Option<Uuid>)> {
            Box::pin(async { unreachable!() })
        }
        fn list_group_device_ids<'a>(&'a self, _actor: &'a Session, _group_id: Uuid) -> RepoFuture<'a, Vec<[u8; 32]>> {
            Box::pin(async { unreachable!() })
        }
        // Task 4 写方法（本步测试同样不触碰）
        fn create_user<'a>(&'a self, _actor: &'a Session, _username: &'a str, _display_name: &'a str, _is_admin: bool, _next_hash: &'a str) -> RepoFuture<'a, UserRecord> {
            Box::pin(async { unreachable!() })
        }
        fn patch_user<'a>(&'a self, _actor: &'a Session, _user_id: [u8; 16], _expected_revision: u64, _active: Option<bool>, _is_admin: Option<bool>, _display_name: Option<&'a str>) -> RepoFuture<'a, UserRecord> {
            Box::pin(async { unreachable!() })
        }
        fn reset_user_password<'a>(&'a self, _actor: &'a Session, _user_id: [u8; 16], _expected_revision: u64, _next_hash: &'a str) -> RepoFuture<'a, UserRecord> {
            Box::pin(async { unreachable!() })
        }
        fn create_role<'a>(&'a self, _actor: &'a Session, _name: &'a str, _permissions: &'a [Permission]) -> RepoFuture<'a, RoleRecord> {
            Box::pin(async { unreachable!() })
        }
        fn patch_role<'a>(&'a self, _actor: &'a Session, _id: Uuid, _expected_revision: u64, _name: Option<&'a str>, _permissions: Option<&'a [Permission]>, _archived: Option<bool>) -> RepoFuture<'a, RoleRecord> {
            Box::pin(async { unreachable!() })
        }
        fn create_grant<'a>(&'a self, _actor: &'a Session, _user_id: [u8; 16], _source: GrantSource, _scope: GrantScope) -> RepoFuture<'a, GrantRecord> {
            Box::pin(async { unreachable!() })
        }
        fn delete_grant<'a>(&'a self, _actor: &'a Session, _id: Uuid, _expected_revision: u64) -> RepoFuture<'a, ()> {
            Box::pin(async { unreachable!() })
        }
        fn create_group<'a>(&'a self, _actor: &'a Session, _name: &'a str) -> RepoFuture<'a, GroupRecord> {
            Box::pin(async { unreachable!() })
        }
        fn patch_group<'a>(&'a self, _actor: &'a Session, _id: Uuid, _expected_revision: u64, _name: Option<&'a str>, _archived: Option<bool>) -> RepoFuture<'a, GroupRecord> {
            Box::pin(async { unreachable!() })
        }
    }

    #[test]
    fn temporary_password_is_at_least_32_bytes_hex_and_hashed_with_argon2id() {
        let (raw, hash) = generate_temporary_password().expect("entropy available");
        assert_eq!(raw.len(), 64, "32 bytes hex = 64 chars");
        assert!(raw.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert!(PasswordHasher::new().verify(&raw, &hash).unwrap());
    }

    /// C1 关键行为：Session 声称 admin，但 DB 当前 `must_change_password` 才是权威（fail-closed → 0 权限）。
    #[tokio::test]
    async fn must_change_password_user_has_zero_effective_permissions() {
        let user = IdentityUser {
            id: [1u8; 16], username: "u".into(), password_hash: "x".into(),
            active: true, is_admin: true, must_change_password: false, revision: 1, // Session 陈旧数据声称"未需改密"
        };
        let session = Session { user, digest: [0u8; 32] };
        // DB 当前快照（桩的固定返回）：must_change_password = true。
        let snapshot = AuthorizationSnapshot {
            actor: SnapshotActor { id: [1u8; 16], active: true, is_admin: true, must_change_password: true },
            authz_epoch: 1,
            grants: vec![],
            roles: HashMap::new(),
            device_groups: HashMap::new(),
            devices: HashMap::new(),
        };
        let service = AuthzService::new(
            Arc::new(StubSnapshotRepo { snapshot }),
            Arc::new(StubAuthzRepo),
        );
        // Session 说"admin 且未需改密"，DB 当前"必须改密" → 0 权限。
        let perms = service.effective_permissions(&session, [0u8; 32]).await.expect("读成功");
        assert!(perms.is_empty(), "must_change_password 的 admin 必须 fail-closed 到 0 权限");
    }

    /// 普通用户：快照 group 范围授权只覆盖自身组成员（不跨设备）；组外设备 0 权限。
    #[tokio::test]
    async fn regular_user_effective_permissions_come_from_snapshot_and_never_cross_devices() {
        let user = IdentityUser {
            id: [1u8; 16], username: "u".into(), password_hash: "x".into(),
            active: true, is_admin: false, must_change_password: false, revision: 1,
        };
        let session = Session { user, digest: [0u8; 32] };
        let group = Uuid::from_bytes([0x67; 16]);
        let grant = GrantRecord {
            id: Uuid::from_bytes([0x0a; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Direct(vec![Permission::DeviceRead]),
            scope: GrantScope::Group(group),
            revision: 1,
        };
        let mut device_groups = HashMap::new();
        let mut g = HashSet::new();
        g.insert(group);
        device_groups.insert([0xa1; 32], g); // 仅设备 A 属于 G
        let snapshot = AuthorizationSnapshot {
            actor: SnapshotActor { id: [1u8; 16], active: true, is_admin: false, must_change_password: false },
            authz_epoch: 1,
            grants: vec![grant],
            roles: HashMap::new(),
            device_groups,
            devices: HashMap::from([
                ([0xa1; 32], source_device_fixture([0xa1; 32], false)),
                ([0xa2; 32], source_device_fixture([0xa2; 32], false)),
            ]),
        };
        let service = AuthzService::new(
            Arc::new(StubSnapshotRepo { snapshot }),
            Arc::new(StubAuthzRepo),
        );
        // 设备 A 属于 G → 有权限。
        assert_eq!(
            service.effective_permissions(&session, [0xa1; 32]).await.expect("读成功"),
            HashSet::from([Permission::DeviceRead])
        );
        // 设备 B 不在 G → 0 权限（同一快照，B 的自身 group 集为空）。
        assert!(service.effective_permissions(&session, [0xa2; 32]).await.expect("读成功").is_empty());
    }

    // 验证caller边界的service fixture；合成现存设备保持跨用户成功正例，
    // 不证明真实SQL隔离/HTTP接线；target生命周期另由上方10场景纯测试覆盖。
    struct SourceSnapshotRepo {
        current_caller: SnapshotActor, // 独立于 Session 的旧 flags，模拟当前 DB caller。
        target_is_admin: bool,
        ordinary_calls: std::sync::atomic::AtomicUsize,
        protected_calls: std::sync::atomic::AtomicUsize,
    }
    impl SourceSnapshotRepo {
        fn new(current_caller: SnapshotActor, target_is_admin: bool) -> Self {
            Self {
                current_caller, target_is_admin,
                ordinary_calls: std::sync::atomic::AtomicUsize::new(0),
                protected_calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn fresh_target_snapshot(&self) -> AuthorizationSnapshot {
            AuthorizationSnapshot {
                actor: SnapshotActor {
                    id: [2u8; 16], active: true, is_admin: self.target_is_admin,
                    must_change_password: false,
                },
                authz_epoch: 1,
                grants: vec![GrantRecord {
                    id: Uuid::from_bytes([0x0a; 16]), user_id: [2u8; 16],
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::All, revision: 1,
                }],
                roles: HashMap::new(), device_groups: HashMap::new(),
                devices: HashMap::from([([0xa1; 32], source_device_fixture([0xa1; 32], false))]),
            } // 每次新建，不要求 AuthorizationSnapshot: Clone。
        }
        fn assert_protected_only(&self) {
            use std::sync::atomic::Ordering::SeqCst;
            assert_eq!(self.protected_calls.load(SeqCst), 1);
            assert_eq!(self.ordinary_calls.load(SeqCst), 0);
        }
    }
    impl AuthorizationSnapshotRepository for SourceSnapshotRepo {
        fn load_snapshot<'a>(
            &'a self, _actor_id: [u8; 16], _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async move {
                self.ordinary_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(self.fresh_target_snapshot())
            })
        }
        fn load_admin_target_snapshot<'a>(
            &'a self, caller: &'a Session, target_user: [u8; 16],
            _device_ids: &'a [[u8; 32]],
        ) -> RepoFuture<'a, AuthorizationSnapshot> {
            Box::pin(async move {
                self.protected_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if caller.user.id != self.current_caller.id
                    || !crate::authz::policy::is_current_admin(&self.current_caller)
                    || target_user != [2u8; 16]
                {
                    return Err(ControllerError::PermissionDenied);
                }
                Ok(self.fresh_target_snapshot())
            })
        }
    }
    fn source_caller_session(is_admin: bool) -> Session {
        Session {
            user: IdentityUser {
                id: [1u8; 16], username: "caller".into(), password_hash: "fixture-only".into(),
                active: true, is_admin, must_change_password: false, revision: 1,
            },
            digest: [9u8; 32],
        }
    }

    #[tokio::test]
    async fn stale_admin_sources_are_rejected_by_current_caller() {
        let session = source_caller_session(true); // 旧 Session 仍声称有效 admin。
        for (active, is_admin, must_change_password) in [
            (true, false, false), (false, true, false), (true, true, true),
        ] {
            let repo = Arc::new(SourceSnapshotRepo::new(SnapshotActor {
                id: [1u8; 16], active, is_admin, must_change_password,
            }, false));
            let service = AuthzService::new(repo.clone(), Arc::new(StubAuthzRepo));
            let result = service.effective_permissions_with_sources(&session, [2u8; 16], [0xa1; 32]).await;
            assert!(matches!(result, Err(ControllerError::PermissionDenied)));
            repo.assert_protected_only(); // 每 case protected=1 / ordinary=0；for 不增加用例数。
        }
    }

    #[tokio::test]
    async fn admin_target_does_not_authorize_nonadmin_caller() {
        let session = source_caller_session(false);
        let repo = Arc::new(SourceSnapshotRepo::new(SnapshotActor {
            id: [1u8; 16], active: true, is_admin: false, must_change_password: false,
        }, true)); // target admin 不给 caller 授权。
        let service = AuthzService::new(repo.clone(), Arc::new(StubAuthzRepo));
        let result = service.effective_permissions_with_sources(&session, [2u8; 16], [0xa1; 32]).await;
        assert!(matches!(result, Err(ControllerError::PermissionDenied)));
        repo.assert_protected_only(); // 直接调用 service，不声称普通用户可穿过 HTTP gate。
    }

    #[tokio::test]
    async fn current_admin_can_read_other_user_sources() {
        let session = source_caller_session(true);
        let repo = Arc::new(SourceSnapshotRepo::new(SnapshotActor {
            id: [1u8; 16], active: true, is_admin: true, must_change_password: false,
        }, false)); // target 为不同的普通用户，只有 Direct DeviceRead。
        let service = AuthzService::new(repo.clone(), Arc::new(StubAuthzRepo));
        let entries = service.effective_permissions_with_sources(&session, [2u8; 16], [0xa1; 32])
            .await.expect("当前 admin 可以读取其他用户来源；恒拒绝不可假绿");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].permission, Permission::DeviceRead);
        assert!(matches!(&entries[0].source, crate::authz::policy::PermissionSourceKind::Direct(id)
            if *id == Uuid::from_bytes([0x0a; 16])));
        repo.assert_protected_only();
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED**

Rust 命令统一使用主工作区共享的 `CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home`（依赖缓存，只读复用）与设计树私有 `CARGO_TARGET_DIR=$PWD/.cargo-target`（`$PWD` = 设计树根目录；构建产物隔离，不污染主树）；`--offline --locked` 保持零新依赖（与 Task 3 同约定；注意设计树内无 `.cargo-home`，不得写 `$PWD/.cargo-home`）。

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller permission_sources_attributes_admin_role_and_direct_deterministically
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller temporary_password_is_at_least_32_bytes_hex_and_hashed_with_argon2id
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller must_change_password_user_has_zero_effective_permissions
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller regular_user_effective_permissions_come_from_snapshot_and_never_cross_devices
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib authz::service::tests::stale_admin_sources_are_rejected_by_current_caller
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib authz::service::tests::admin_target_does_not_authorize_nonadmin_caller
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib authz::service::tests::current_admin_can_read_other_user_sources
```
预期：各命令运行恰好 1 个测试且因断言失败（全部**可编译行为 RED**）：
- 第一条：`permission_sources` 桩恒空 vec，admin 分支 `entries.len() == 4` 失败；
- 第二条：`generate_temporary_password` 桩返回非 Argon2 哈希串，`verify` 解析失败（测试 `unwrap()` panic）；
- 第三条：`effective_permissions` 桩恒返回 4 权限全量，"改密 admin → 0 权限"断言失败（桩不读快照的 `must_change_password`）；
- 第四条：桩恒返回全量，"普通用户设备 A → 仅 `DeviceRead`"断言失败（桩不读快照的 group 范围）。
- 第五、六条：错误 sources 实现返回 `Ok([])`，负例 `Err(PermissionDenied)` 断言失败（stale 三 case 仍只算 1 个测试）；不能以 panic/TODO、缺 trait method 或 0 用例记 RED。
- 第七条：`entries.len() == 1` 失败；即使误补了来源，protected=1 / ordinary=0 spy 也会拒绝 ordinary 旁路。合计caller阶段service6方法；加上新增生命周期纯测试后service7方法，policy另1不计入service。
任何一条报编译错误或 0 用例都不得记为 RED。

- [ ] **Step 3: 最小实现**

```rust
// crates/rsetup-controller/src/authz/policy.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PermissionSourceKind {
    Admin,
    Role(Uuid),
    Direct(Uuid),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PermissionSourceEntry {
    pub permission: Permission,
    pub source: PermissionSourceKind,
}

pub fn permission_sources(
    user_id: [u8; 16],
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
    is_admin: bool,
) -> Vec<PermissionSourceEntry> {
    if is_admin {
        return vec![
            PermissionSourceEntry { permission: Permission::DeviceRead, source: PermissionSourceKind::Admin },
            PermissionSourceEntry { permission: Permission::DeviceStatusRead, source: PermissionSourceKind::Admin },
            PermissionSourceEntry { permission: Permission::DeviceReboot, source: PermissionSourceKind::Admin },
            PermissionSourceEntry { permission: Permission::DeviceTaskRead, source: PermissionSourceKind::Admin },
        ];
    }
    let mut entries = Vec::new();
    for grant in grants {
        if grant.user_id != user_id { continue; }
        let scope_matches = match &grant.scope {
            GrantScope::All => true,
            GrantScope::Group(gid) => device_groups.contains(gid),
            GrantScope::Device(did) => *did == device_id,
        };
        if !scope_matches { continue; }
        match &grant.source {
            GrantSource::Role(rid) => {
                if let Some(role) = roles.get(rid) {
                    if !role.archived {
                        for p in &role.permissions {
                            entries.push(PermissionSourceEntry {
                                permission: *p,
                                source: PermissionSourceKind::Role(*rid),
                            });
                        }
                    }
                }
            }
            GrantSource::Direct(perms) => {
                for p in perms {
                    entries.push(PermissionSourceEntry {
                        permission: *p,
                        source: PermissionSourceKind::Direct(grant.id),
                    });
                }
            }
        }
    }
    // 确定性排序并去重
    entries.sort_unstable_by(|a, b| {
        a.permission.as_str().cmp(b.permission.as_str())
            .then_with(|| a.source.cmp(&b.source))
    });
    entries.dedup();
    entries
}
```

```rust
// crates/rsetup-controller/src/authz/service.rs（以真实实现替换 Step 1 的 RED 桩；Step 1 一次写好的测试不改）
use std::collections::HashSet;
use std::sync::Arc;

use crate::auth::password::PasswordHasher;
use crate::auth::service::Session;
use crate::authz::policy::{
    is_current_admin, eval_memory_permissions, permission_sources, Permission,
    PermissionSourceEntry, PermissionSourceKind, ALL_DEVICE_PERMISSIONS,
};
use crate::authz::sqlx_repo::{AuthzRepository, AuthorizationSnapshotRepository, UserRecord};
use crate::ControllerError;

pub struct AuthzService<S: AuthorizationSnapshotRepository, W: AuthzRepository> {
    snapshots: Arc<S>,
    repo: Arc<W>,
}

impl<S: AuthorizationSnapshotRepository, W: AuthzRepository> AuthzService<S, W> {
    pub fn new(snapshots: Arc<S>, repo: Arc<W>) -> Self {
        Self { snapshots, repo }
    }

    /// 有效权限集（C1：DB 当前快照读 → 纯求值；`Session.user` 字段只是认证身份，不参与授权）。
    pub async fn effective_permissions(
        &self,
        actor: &Session,
        device_id: [u8; 32],
    ) -> Result<HashSet<Permission>, ControllerError> {
        let snapshot = self.snapshots.load_snapshot(actor.user.id, &[device_id]).await?;
        Ok(snapshot_effective_permissions(&snapshot, device_id))
    }

    pub async fn require_permission(
        &self,
        actor: &Session,
        device_id: [u8; 32],
        required: Permission,
    ) -> Result<(), ControllerError> {
        if !self.effective_permissions(actor, device_id).await?.contains(&required) {
            return Err(ControllerError::PermissionDenied);
        }
        Ok(())
    }

    /// 来源归因（`effective-permissions` 路由直接消费）：走 protected 管理读取 `load_admin_target_snapshot(actor, target_user, &[device_id])`——
    /// 同一读事务内先以 DB raw_bool 重验 `actor` 为当前 admin（HTTP Users 的 `require_admin_session` 只是前置，服务/仓储权威检查不省），
    /// 再按 `target_user` 加载目标快照；`actor` 只是认证身份，授权真相是目标用户的 DB 当前快照。
    pub async fn effective_permissions_with_sources(
        &self,
        actor: &Session,
        target_user: [u8; 16],
        device_id: [u8; 32],
    ) -> Result<Vec<PermissionSourceEntry>, ControllerError> {
        let snapshot = self.snapshots.load_admin_target_snapshot(actor, target_user, &[device_id]).await?;
        Ok(snapshot_permission_sources(&snapshot, device_id))
    }

    /// 管理员写编排：临时密码在进入仓储事务**前**生成并哈希（事务只拿 `hash` 串，只耗纳秒级 SQL）。
    pub async fn create_user(
        &self,
        actor: &Session,
        username: &str,
        display_name: &str,
        is_admin: bool,
    ) -> Result<(UserRecord, String), ControllerError> {
        let (plain, password_hash) = generate_temporary_password()?;
        let record =
            self.repo
                .create_user(actor, username, display_name, is_admin, &password_hash)
                .await?;
        Ok((record, plain))
    }

    pub async fn reset_user_password(
        &self,
        actor: &Session,
        user_id: [u8; 16],
        expected_revision: u64,
    ) -> Result<(UserRecord, String), ControllerError> {
        let (plain, password_hash) = generate_temporary_password()?;
        let record = self
            .repo
            .reset_user_password(actor, user_id, expected_revision, &password_hash)
            .await?;
        Ok((record, plain))
    }
}
```

```rust
// crates/rsetup-controller/src/authz/service.rs
use rand::RngCore;

pub fn generate_temporary_password() -> Result<(String, String), ControllerError> {
    let mut entropy = [0u8; 32];
    rand::rngs::OsRng.try_fill_bytes(&mut entropy).map_err(|_| ControllerError::Crypto)?;
    let raw = hex::encode(entropy);
    let hash = PasswordHasher::new().hash(&raw)?;
    Ok((raw, hash))
}
```

`create_user` 与 `reset_user_password` 先调 `generate_temporary_password()` 取得 `(raw, hash)`，再传 `&hash` 给仓储写事务，最后向调用方返回 `(record, raw)`（raw 仅在响应中展示一次，永不落库，永不写入脱敏审计）。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller authz::service::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller permission_sources_attributes_admin_role_and_direct_deterministically
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib authz::service::tests::stale_admin_sources_are_rejected_by_current_caller
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib authz::service::tests::admin_target_does_not_authorize_nonadmin_caller
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib authz::service::tests::current_admin_can_read_other_user_sources
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib
```
预期：全绿。第一条运行 `authz/service.rs` 恰好 7 个测试（原3+caller3+来源生命周期纯测试1；该生命周期方法10场景，for不多算）；第二条 policy 恰好 1 个，不计入 service；第三至五条各精确运行 1 个新增 service 测试；第六条是全 crate 单测回归（Task 3/4 纯函数与既有测试仍全绿）。这些均为未来验证预期，本次仅编辑计划，未运行、不得记录 PASS。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/authz/service.rs crates/rsetup-controller/src/authz/mod.rs crates/rsetup-controller/src/authz/policy.rs
git commit -m "feat(controller): 授权服务：权限来源归因与纯函数临时密码"
```

---


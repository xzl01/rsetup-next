# Controller V1 剩余身份、动态授权与准入管理实施计划（TDD 条件性）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实；使用 `- [ ]` 复选框跟踪步骤。本文是**条件性后续实施计划**，不是实施授权、真库操作许可或生产发布批准。

**Goal:** 基于已完成的身份基线（v3 schema、Argon2id、认证会话模型与准入 CAS），增量补全中控 V1 尚缺的动态并集授权、用户/角色/分组/设备管理 API、本机 root `reset-admin` 恢复 CLI、安全生产路由器接线，并建立认证会话事务与受限 Observer 锁观察的真实双引擎验收规格。

**Architecture:** 维持单体模块化与分层边界：HTTP 输入与安全门禁（Host/Origin/JSON/CSRF/限速）→ 业务服务与策略（可见性过滤/缓存与 `authz_epoch` 校验）→ SQLx 事务仓储。事务严格遵守统一锁序：`schema_meta(singleton=1) FOR UPDATE` → `users`（UUID 字节序）→ `roles`（UUID 字节序）→ `device_groups`（UUID 字节序）→ `devices`（公钥字节序）→ 依赖行；同事务更新 revision、`authz_epoch` 并写入脱敏审计。

**Tech Stack:** Rust 2024 / MSRV 1.85；Axum 0.8、Tokio、SQLx 0.8.6、Argon2id、SHA-256、UUIDv4、Chrono、serde_json；MySQL 8.4 LTS / TiDB 8.5 LTS（草案建议版本）；`crates/rsetup-controller/Cargo.toml` 需经审查显式声明工作区已有的 `serde.workspace = true` 与 `libc.workspace = true` 并离线更新核验 `Cargo.lock`，零外部未审 Cargo 新依赖。

**Spec:**
- [00 总目录与范围](../specs/2026-09-23-controller-v1-00-index.md)
- [01 身份与授权规格](../specs/2026-09-23-controller-v1-01-identity-access.md)
- [02 数据模型与管理 API 规格](../specs/2026-09-23-controller-v1-02-data-api.md)
- [06 Web UI 与验收规格](../specs/2026-09-23-controller-v1-06-web-acceptance.md)
- [应用层完整性设计](../specs/2026-10-03-controller-application-integrity-design.md)
- [Task4 隔离 observer 正式规格](../specs/2026-10-03-controller-task4-isolated-observer-design.md)
- [会话管理设计](../specs/2026-10-05-controller-session-management-design.md)

---

## Global Constraints

1. **当前代码事实基线与真实缺口说明：**
   - `main.rs` 现状：当前 `main.rs` 仅启动探活路由 `build_router`（仅包含 `/healthz` 与 `/readyz`，即 **current main health-only**），尚未接线认证与管理路由，亦无任何 CLI 子命令。
   - `http_live.rs` 与 `http_auth.rs` 现状：仅作为已编译单元存在（**auth/session compile-only**），`LiveAuth` 与 `build_http_router` 尚未与 `main.rs` 组装，四个基础 auth 端点与会话端点未暴露在生产监听端口。
   - 数据库现状：`0003_identity_application_integrity.sql` 确立了 v3 基线；`tests/mysql_identity.rs` 与 `tests/tidb_identity.rs` 仅覆盖了 DDL 升级、只读存量校验与准入 CAS 事务；`SqlxIdentityRepository` 的会话写入事务、并发最后管理员保护与动态授权未在真实双引擎上运行过真实集成测试（**真实 DB 验收缺口**）。
2. **安全与受控操作前置条件：**
   - 严禁在未经人工确认、无真实备份证明或未提供经批准隔离目标的情况下运行任何数据库迁移或 `#[ignore]` 测试。
   - 不得读取 `secret/`、不得连接非授权数据库、不得运行危险命令、不得阶段性修改既有 spec/plan/代码、不得执行 `git add` / `git commit`。
   - 本文档绝不声称任何测试“已经运行”或“已验证通过”。计划中提及的执行命令和步骤均属于**未来执行步骤（本轮修订不执行；后续需批准）**，文档作者保持严格约束，不与未来执行步骤混淆。
3. **接口契约与输入输出硬约束：**
   - 基础路径固定 `/api/v1`；成功响应统一 `{data: T, request_id: UUIDv4}`，失败响应统一 `{error: {code, message_key, params: {}}, request_id: UUIDv4}`（[02 §3](../specs/2026-09-23-controller-v1-02-data-api.md#3-http公共契约)）。
   - 错误码严格取自 02 §3 枚举：`INVALID_ARGUMENT`, `AUTH_REQUIRED`, `INVALID_CREDENTIALS`, `PERMISSION_DENIED`, `PASSWORD_CHANGE_REQUIRED`, `CSRF_INVALID`, `NOT_FOUND`, `REVISION_CONFLICT`, `IDEMPOTENCY_CONFLICT`, `LAST_ADMIN`, `INVALID_STATE`, `RATE_LIMITED`, `NOT_READY`, `STORAGE_UNAVAILABLE`, `RESOURCE_EXHAUSTED`。
   - 设备 ID 必须为小写 hex64（32 字节 Ed25519 公钥），数据库 `BINARY(32)`；禁止以 SN 或 IP 作为设备标识。不可见设备与不存在设备统一返回 404 `NOT_FOUND`，不得泄露过滤前设备总数（[01 A-06](../specs/2026-09-23-controller-v1-01-identity-access.md#6-读取任务与订阅可见性)）。
   - 权限点仅允许四个标准枚举：`device.read`, `device.status.read`, `device.reboot`, `device.task.read`（[01 §4](../specs/2026-09-23-controller-v1-01-identity-access.md#4-设备权限点)）。`device.reboot` 不隐含 `device.status.read` 或 `device.read`。
   - 仅管理员可见全局；普通用户仅可见授权并集设备。动态授权每条 grant 的 source（role/direct）与 scope（all/group/device）成对匹配，无 deny。
   - 变更 revision、`authz_epoch` 与脱敏审计同事务；审计中严禁写入密码、明文 token、token_hash、HMAC key 或敏感凭据。
4. **TDD 执行铁律：**
   - 每项任务按安全前置顺序排列，步骤细化为 RED → 确认失败 → GREEN → 确认通过 → 条件性提交。
   - RED 阶段必须是可编译、可运行但断言行为缺失而失败的有效测试，不得以编译错误、缺失模块或测试忽略（ignore）冒充 RED。

---

## 现状审计与未结缺口基线

| 模块 / 领域 | 既有代码与已完成计划 | 当前真实状态 | 本计划需增量交付项 |
| :--- | :--- | :--- | :--- |
| **基础 Schema & CAS** | `0003_identity_application_integrity.sql`, `integrity.rs`, `db.rs` | 已落地 v3 结构校验与 `AdmissionStore::compare_and_set` | 保持 schema 不变，新增动态授权表与管理表消费仓储 |
| **认证与会话服务** | `auth/password.rs`, `auth/session.rs`, `auth/service.rs`, `auth/sqlx_repo.rs` | 纯内存单元测试完备，SQLx 仓储已编写 9 个方法 | 缺真实双引擎并发与会话撤销事务验收证明 |
| **HTTP Auth 路由** | `http_auth.rs`, `http_api.rs`, `http_security.rs`, `http_live.rs` | 离线 tower 测试通过，未连真实库 | `main.rs health-only`，`LiveAuth` 为 compile-only，未注册到生产路由 |
| **动态并集授权** | 仅规格 01 §4–5 与 02 §4 | **代码完全缺失**（无 `authz/` 模块） | Task 1（策略引擎）、Task 2（仓储）、Task 3（服务） |
| **管理 HTTP API** | 仅规格 02 §4–5 与 02 §7 | **代码完全缺失**（无 users/roles/grants/groups/approvals 路由） | Task 4（API Handlers 与路由） |
| **Root 恢复 CLI** | 仅规格 01 §1（`reset-admin` 约束） | **代码完全缺失**（`main.rs` 无子命令） | Task 5（本机交互 TTY、双 UID=0、原子重置） |
| **生产路由器接线** | `lib.rs::build_router` 仅探活 | 生产就绪度断开 | Task 6（组装完整路由与 AppState 接线） |
| **双引擎事务验收** | `mysql_identity.rs`, `tidb_identity.rs` 仅测迁移与 CAS | 真实 DB 验收缺口 | Task 7（双库会话事务、最后管理员保护、授权 CAS 测试） |
| **Observer 锁观察** | `tests/observer_contract.rs` 仅离线语法 | 真实 DB 锁等待未实测 | Task 8（受限只读 Observer 真实锁等待观察计划） |

---

## 任务分解

```
Task 1 (动态授权策略引擎) ──► Task 2 (动态授权与管理仓储) ──► Task 3 (服务层与可见性整合)
                                                                 │
                                                                 ▼
Task 5 (Root CLI reset-admin) ◄── Task 6 (安全生产接线) ◄── Task 4 (管理 HTTP API 闭环)
                                          │
                                          ▼
                         Task 7 (双引擎会话/授权事务验收)
                                          │
                                          ▼
                         Task 8 (受限 Observer 锁观察验收)
```

---

### Task 1: 动态授权核心策略与内存评估引擎

**Files:**
- Create: `crates/rsetup-controller/src/authz/mod.rs`
- Create: `crates/rsetup-controller/src/authz/policy.rs`
- Modify: `crates/rsetup-controller/src/lib.rs:1-15`

**Interfaces:**
- Consumes:
  - `crate::model::decimal_u64_json` ([model.rs:21](../../../crates/rsetup-controller/src/model.rs#L21))
  - `crate::error::ControllerError` ([error.rs:1](../../../crates/rsetup-controller/src/error.rs#L1))
- Produces:
  - `Permission` enum: `DeviceRead`, `DeviceStatusRead`, `DeviceReboot`, `DeviceTaskRead`
  - `GrantSource` enum: `Role(uuid::Uuid)`, `Direct(Vec<Permission>)`
  - `GrantScope` enum: `All`, `Group(uuid::Uuid)`, `Device([u8; 32])`
  - `GrantRecord`: `{ id: uuid::Uuid, user_id: [u8; 16], source: GrantSource, scope: GrantScope, revision: u64 }`
  - `RoleRecord`: `{ id: uuid::Uuid, name: String, builtin: bool, archived: bool, revision: u64, permissions: Vec<Permission> }`
  - `evaluate_grants(user_id: [u8; 16], device_id: [u8; 32], grants: &[GrantRecord], roles: &std::collections::HashMap<uuid::Uuid, RoleRecord>, device_groups: &std::collections::HashSet<uuid::Uuid>) -> std::collections::HashSet<Permission>`
  - `minimum_device_projection(device_id: [u8; 32], display_name: &str, perms: &std::collections::HashSet<Permission>) -> serde_json::Value`

- [ ] **Step 0: 依赖审查与模块注册桩**

在未来执行前经审查在 `crates/rsetup-controller/Cargo.toml` 中显式添加工作区已声明的 `serde.workspace = true`（提供 `Serialize`/`Deserialize` derive 支持），并离线更新核验 `Cargo.lock`。不能仅因根 manifest 存在依赖就声称子 crate 在 `--locked` 下直接可用；本轮修订不修改 Cargo.toml 或执行 Cargo，后续落实需经专门批准。

先在 `crates/rsetup-controller/src/lib.rs` 注册 `pub mod authz;`，在 `crates/rsetup-controller/src/authz/mod.rs` 注册 `pub mod policy;`，并在 `crates/rsetup-controller/src/authz/policy.rs` 中提供类型定义及断言失败的行为 RED 桩：
```rust
// crates/rsetup-controller/src/authz/policy.rs
use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "device.read")]
    DeviceRead,
    #[serde(rename = "device.status.read")]
    DeviceStatusRead,
    #[serde(rename = "device.reboot")]
    DeviceReboot,
    #[serde(rename = "device.task.read")]
    DeviceTaskRead,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeviceRead => "device.read",
            Self::DeviceStatusRead => "device.status.read",
            Self::DeviceReboot => "device.reboot",
            Self::DeviceTaskRead => "device.task.read",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantSource {
    Role(Uuid),
    Direct(Vec<Permission>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantScope {
    All,
    Group(Uuid),
    Device([u8; 32]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRecord {
    pub id: Uuid,
    pub user_id: [u8; 16],
    pub source: GrantSource,
    pub scope: GrantScope,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleRecord {
    pub id: Uuid,
    pub name: String,
    pub builtin: bool,
    pub archived: bool,
    pub revision: u64,
    pub permissions: Vec<Permission>,
}

// 可编译 RED 桩：返回空集，使正向用例在编译通过后发生断言失败
pub fn evaluate_grants(
    _user_id: [u8; 16],
    _device_id: [u8; 32],
    _grants: &[GrantRecord],
    _roles: &HashMap<Uuid, RoleRecord>,
    _device_groups: &HashSet<Uuid>,
) -> HashSet<Permission> {
    HashSet::new()
}

pub fn minimum_device_projection(
    _device_id: [u8; 32],
    _display_name: &str,
    _perms: &HashSet<Permission>,
) -> serde_json::Value {
    serde_json::Value::Null
}
```

- [ ] **Step 1: 编写失败测试**

```rust
// crates/rsetup-controller/src/authz/policy.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn role_permissions_never_cross_grant_scope_and_take_union() {
        let user = [1u8; 16];
        let dev_a = [0x0au8; 32];
        let dev_b = [0x0bu8; 32];
        let role_viewer_id = uuid::Uuid::new_v4();
        let role_operator_id = uuid::Uuid::new_v4();

        let mut roles = HashMap::new();
        roles.insert(role_viewer_id, RoleRecord {
            id: role_viewer_id,
            name: "viewer".into(),
            builtin: true,
            archived: false,
            revision: 1,
            permissions: vec![Permission::DeviceRead, Permission::DeviceStatusRead],
        });
        roles.insert(role_operator_id, RoleRecord {
            id: role_operator_id,
            name: "operator".into(),
            builtin: true,
            archived: false,
            revision: 1,
            permissions: vec![Permission::DeviceReboot],
        });

        // Grant 1: user has viewer role on Device A only
        // Grant 2: user has operator role on Device B only
        let grants = vec![
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Role(role_viewer_id),
                scope: GrantScope::Device(dev_a),
                revision: 1,
            },
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Role(role_operator_id),
                scope: GrantScope::Device(dev_b),
                revision: 1,
            },
        ];

        let perms_a = evaluate_grants(user, dev_a, &grants, &roles, &HashSet::new());
        assert!(perms_a.contains(&Permission::DeviceRead));
        assert!(perms_a.contains(&Permission::DeviceStatusRead));
        assert!(!perms_a.contains(&Permission::DeviceReboot), "Device A must not inherit Device B reboot role");

        let perms_b = evaluate_grants(user, dev_b, &grants, &roles, &HashSet::new());
        assert!(perms_b.contains(&Permission::DeviceReboot));
        assert!(!perms_b.contains(&Permission::DeviceRead), "Device B must not inherit Device A viewer role");
    }

    #[test]
    fn group_removal_preserves_independent_all_scope() {
        let user = [2u8; 16];
        let dev = [0x0cu8; 32];
        let group_id = uuid::Uuid::new_v4();

        let grants = vec![
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Direct(vec![Permission::DeviceRead]),
                scope: GrantScope::All,
                revision: 1,
            },
            GrantRecord {
                id: uuid::Uuid::new_v4(),
                user_id: user,
                source: GrantSource::Direct(vec![Permission::DeviceReboot]),
                scope: GrantScope::Group(group_id),
                revision: 1,
            },
        ];

        // Device is NOT in the group
        let no_groups = HashSet::new();
        let perms = evaluate_grants(user, dev, &grants, &HashMap::new(), &no_groups);
        assert_eq!(perms, HashSet::from([Permission::DeviceRead]));

        // Device IS in the group
        let mut in_group = HashSet::new();
        in_group.insert(group_id);
        let perms_in = evaluate_grants(user, dev, &grants, &HashMap::new(), &in_group);
        assert_eq!(perms_in, HashSet::from([Permission::DeviceRead, Permission::DeviceReboot]));
    }
}
```

- [ ] **Step 2: 运行并确认失败（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::policy::tests::role_permissions_never_cross_grant_scope_and_take_union -- --exact
```
预期输出：可编译通过，运行恰好 1 个测试；FAIL（断言失败：`assert!(perms_a.contains(&Permission::DeviceRead))` 失败，桩函数返回空集，行为 RED）。检查运行测试数恰好为 1。

- [ ] **Step 3: 最小实现**

```rust
// crates/rsetup-controller/src/authz/policy.rs
use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Permission {
    #[serde(rename = "device.read")]
    DeviceRead,
    #[serde(rename = "device.status.read")]
    DeviceStatusRead,
    #[serde(rename = "device.reboot")]
    DeviceReboot,
    #[serde(rename = "device.task.read")]
    DeviceTaskRead,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeviceRead => "device.read",
            Self::DeviceStatusRead => "device.status.read",
            Self::DeviceReboot => "device.reboot",
            Self::DeviceTaskRead => "device.task.read",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "device.read" => Some(Self::DeviceRead),
            "device.status.read" => Some(Self::DeviceStatusRead),
            "device.reboot" => Some(Self::DeviceReboot),
            "device.task.read" => Some(Self::DeviceTaskRead),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantSource {
    Role(Uuid),
    Direct(Vec<Permission>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrantScope {
    All,
    Group(Uuid),
    Device([u8; 32]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRecord {
    pub id: Uuid,
    pub user_id: [u8; 16],
    pub source: GrantSource,
    pub scope: GrantScope,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleRecord {
    pub id: Uuid,
    pub name: String,
    pub builtin: bool,
    pub archived: bool,
    pub revision: u64,
    pub permissions: Vec<Permission>,
}

pub fn evaluate_grants(
    user_id: [u8; 16],
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
) -> HashSet<Permission> {
    let mut effective = HashSet::new();

    for grant in grants {
        if grant.user_id != user_id {
            continue;
        }

        let scope_matches = match &grant.scope {
            GrantScope::All => true,
            GrantScope::Group(gid) => device_groups.contains(gid),
            GrantScope::Device(did) => *did == device_id,
        };

        if !scope_matches {
            continue;
        }

        match &grant.source {
            GrantSource::Role(rid) => {
                if let Some(role) = roles.get(rid) {
                    if !role.archived {
                        effective.extend(role.permissions.iter().copied());
                    }
                }
            }
            GrantSource::Direct(perms) => {
                effective.extend(perms.iter().copied());
            }
        }
    }

    effective
}

pub fn minimum_device_projection(
    device_id: [u8; 32],
    display_name: &str,
    perms: &HashSet<Permission>,
) -> serde_json::Value {
    let mut perm_strings: Vec<&'static str> = perms.iter().map(|p| p.as_str()).collect();
    perm_strings.sort_unstable();

    serde_json::json!({
        "device_id": hex::encode(device_id),
        "display_name": display_name,
        "effective_permissions": perm_strings,
    })
}
```

```rust
// crates/rsetup-controller/src/authz/mod.rs
pub mod policy;
pub use policy::{
    GrantRecord, GrantScope, GrantSource, Permission, RoleRecord, evaluate_grants,
    minimum_device_projection,
};
```

- [ ] **Step 4: 运行并确认通过（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::policy::tests::role_permissions_never_cross_grant_scope_and_take_union -- --exact
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::policy::tests::group_removal_preserves_independent_all_scope -- --exact
```
预期输出：每个命令各 `test result: ok. 1 passed; 0 failed`，运行测试数均为 1。

- [ ] **Step 5: 条件性提交（本轮修订不执行；后续需批准）**

```bash
git add crates/rsetup-controller/src/authz/policy.rs crates/rsetup-controller/src/authz/mod.rs crates/rsetup-controller/src/lib.rs
git commit -m "feat(controller): implement dynamic authorization policy evaluator"
```

---

### Task 2: 动态授权与管理对象 SQLx 仓储层

**Files:**
- Create: `crates/rsetup-controller/src/authz/sqlx_repo.rs`
- Create: `crates/rsetup-controller/src/devices/repository.rs`
- Modify: `crates/rsetup-controller/src/error.rs:1-21`
- Modify: `crates/rsetup-controller/src/authz/mod.rs`
- Modify: `crates/rsetup-controller/src/devices/mod.rs`

**Interfaces:**
- Consumes:
  - `crate::db::DbPool`
  - `crate::integrity::{lock_integrity_guard, validate_grant_fields}`
  - `crate::authz::policy::{GrantRecord, GrantScope, GrantSource, Permission, RoleRecord}`
  - `crate::model::decimal_u64_json`
  - `crate::auth::service::Session` (可信 actor 凭据)
- Produces:
  - `AuthzRepository` trait & `SqlxAuthzRepository`（提供 `new(db: DbPool) -> Self` 供服务层及 Task 7 真库测试复用）：
    - `find_role(id: Uuid) -> Result<Option<RoleRecord>, ControllerError>`
    - `list_roles(cursor: Option<Uuid>, limit: usize) -> Result<Vec<RoleRecord>, ControllerError>`
    - `create_role(actor: &Session, name: &str, permissions: &[Permission]) -> Result<RoleRecord, ControllerError>`
    - `patch_role(actor: &Session, id: Uuid, expected_revision: u64, name: Option<&str>, permissions: Option<&[Permission]>, archived: Option<bool>) -> Result<RoleRecord, ControllerError>`
    - `create_grant(actor: &Session, user_id: [u8; 16], source: GrantSource, scope: GrantScope) -> Result<GrantRecord, ControllerError>`
    - `delete_grant(actor: &Session, id: Uuid, expected_revision: u64) -> Result<(), ControllerError>`
    - `list_grants_for_user(user_id: [u8; 16]) -> Result<Vec<GrantRecord>, ControllerError>`
    - `patch_user_active_or_admin(actor: &Session, user_id: [u8; 16], expected_revision: u64, active: Option<bool>, is_admin: Option<bool>, display_name: Option<&str>) -> Result<IdentityUser, ControllerError>` (实现最后管理员保护与并发撤权防御)
  - `DeviceRepository` trait & `SqlxDeviceRepository`:
    - `get_device(device_id: [u8; 32]) -> Result<Option<DeviceRecordView>, ControllerError>`
    - `get_device_groups(device_id: [u8; 32]) -> Result<HashSet<Uuid>, ControllerError>`
    - `replace_group_members(actor: &Session, group_id: Uuid, expected_revision: u64, device_ids: &[[u8; 32]]) -> Result<usize, ControllerError>`
    - `patch_device_display_name(actor: &Session, device_id: [u8; 32], expected_revision: u64, display_name: &str) -> Result<(), ControllerError>`

**统一锁序与同事务审计规范：**
所有管理写仓储操作严禁仅在 HTTP handler 层判权，必须将可信 `actor: &Session` 传入仓储事务，在事务内先获取 `lock_integrity_guard`，再按严格锁序 `users` (按 UUID 字节序排序 actor 与 target，若为同一用户则锁单行) -> `roles` -> `device_groups` -> `devices` 锁定行。在 guard 下重新从数据库行严格验证 actor 处于有效管理员状态（`active == 1 && is_admin == 1 && must_change_password == 0`，严格拒绝 0/1 之外的布尔脏值）；布尔读取必须严格 0/1 拒污染（复用 `raw_bool` 规则）。若 actor 在此期间已被并发撤权或停用，立即返回 `PermissionDenied` 并回滚。
同时，修改操作必须同事务写入脱敏审计记录（复用 `0003` 结构标准 11 字段，不杜撰不存在的 public 函数，直接使用参数化 SQL `INSERT INTO audit_events`），并在提交前对 `authz_epoch` 执行 checked 自增及 CAS/受影响行数验证（`rows_affected == 1`），确保并发写无缝失效旧授权缓存。

- [ ] **Step 0: 错误变体先行与模块注册桩**

先在 `crates/rsetup-controller/src/error.rs` 中增加 `LastAdmin` 错误变体，确保类型系统完整可编译；并在 `crates/rsetup-controller/src/authz/mod.rs` 注册 `pub mod sqlx_repo;`：
```rust
// crates/rsetup-controller/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum ControllerError {
    #[error("invalid argument")]
    InvalidArgument,
    #[error("revision conflict")]
    RevisionConflict,
    #[error("permission denied")]
    PermissionDenied,
    #[error("not found")]
    NotFound,
    #[error("resource exhausted")]
    ResourceExhausted,
    #[error("last administrator cannot be deactivated or demoted")]
    LastAdmin,
    #[error("configuration: {0}")]
    Config(String),
    #[error("identity schema not ready: found {found:?}, required {required}")]
    SchemaNotReady { found: Option<i32>, required: i32 },
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("cryptography error")]
    Crypto,
}
```

并在 `crates/rsetup-controller/src/authz/sqlx_repo.rs` 中提供桩实现，使行为 RED 测试在编译通过后因行为断言而失败：
```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs
use crate::error::ControllerError;

pub fn check_admin_demotion_allowed(current_admin_count: i64) -> Result<(), ControllerError> {
    // 可编译 RED 桩：无论当前有几个管理员均允许，使边界断言失败
    let _ = current_admin_count;
    Ok(())
}
```

- [ ] **Step 1: 编写失败测试**

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_admin_deactivation_is_prevented_by_error() {
        let err = ControllerError::LastAdmin;
        assert_eq!(err.to_string(), "last administrator cannot be deactivated or demoted");
        // 行为断言：当 admin_count <= 1 时必须拒绝并返回 ControllerError::LastAdmin
        let res = check_admin_demotion_allowed(1);
        assert!(matches!(res, Err(ControllerError::LastAdmin)));
    }

    #[test]
    fn grant_field_validation_rejects_empty_direct_permissions() {
        let perms = serde_json::json!([]);
        let err = crate::integrity::validate_grant_fields("direct", false, Some(&perms), "all", false, false);
        assert!(err.is_err(), "empty direct permissions must be rejected fail-closed");
    }
}
```

- [ ] **Step 2: 运行并确认失败（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::sqlx_repo::tests::last_admin_deactivation_is_prevented_by_error -- --exact
```
预期输出：可编译通过，运行恰好 1 个测试；FAIL（断言 `matches!(res, Err(ControllerError::LastAdmin))` 失败，桩函数返回了 `Ok(())`，行为 RED）。检查运行测试数恰好为 1。

- [ ] **Step 3: 最小实现规范与事务锁序契约**

在 `crates/rsetup-controller/src/authz/sqlx_repo.rs` 中实现规则判定与仓储契约：
```rust
pub fn check_admin_demotion_allowed(current_admin_count: i64) -> Result<(), ControllerError> {
    if current_admin_count <= 1 {
        Err(ControllerError::LastAdmin)
    } else {
        Ok(())
    }
}
```

实现 `SqlxAuthzRepository::patch_user_active_or_admin`，明确**不可直接复制的非完整事务顺序契约**：
1. **全局 Guard 串行化：** 开启事务后，第一步调用 `crate::integrity::lock_integrity_guard(&mut tx).await?` 锁定 `schema_meta(singleton=1)`，防止任何并发管理写交叉执行。
2. **Actor 与 Target 排序行锁：**
   - 比较 `actor.user.id` 与目标 `user_id` 的 16 字节 UUID 字典序。
   - 若不同，按升序先后执行 `SELECT ... FROM users WHERE id = ? FOR UPDATE`；若相同，仅锁一行。
   - 严禁乱序锁行避免死锁风险。
3. **Guard 内强验 Actor 管理员权限：**
   - 解码 actor 的 `active`, `is_admin`, `must_change_password` 列。
   - 严格要求布尔值在数据库中为 0 或 1，出现任何脏值（非 0/1）直接失败关闭（`Config("invalid boolean")`）。
   - 校验 actor 当前仍为 `active == 1 && is_admin == 1 && must_change_password == 0`；若已被其他并发事务停用或降级，立即返回 `ControllerError::PermissionDenied` 并回滚，不能仅凭调用方传入的内存 session 判权。
4. **Target 状态校验与最后管理员保护：**
   - 校验 target 的 `revision == expected_revision`，不匹配返回 `RevisionConflict`。
   - 严格以 0/1 校验 target 的既有状态。
   - 若 target 为活跃管理员且本次修改试图停用（`active = false`）或降级（`is_admin = false`），统计 `SELECT COUNT(*) FROM users WHERE active = 1 AND is_admin = 1`；当 count <= 1 时拒绝并返回 `ControllerError::LastAdmin`。
5. **Checked 计算与更新：**
   - target revision 通过 `rev.checked_add(1)` 递增，溢出则返回错误。
   - 执行 `UPDATE users SET ... WHERE id = ? AND revision = ?`，断言 `rows_affected == 1`。
   - 若 target 被停用，同事务更新 `UPDATE sessions SET revoked = 1 WHERE user_id = ? AND revoked = 0`。
6. **同事务 CAS 自增 `authz_epoch`：**
   - 读取当前 `authz_epoch`，checked 自增 `current_epoch.checked_add(1)`。
   - 执行 `UPDATE schema_meta SET authz_epoch = ? WHERE singleton = 1 AND authz_epoch = ?`，断言 `rows_affected == 1`。
7. **同事务脱敏审计写入：**
   - 生成全新 UUIDv4 作为 `audit_events.id`；`actor_user_id` 仅绑定当前已重验管理员的 16B ID，`target_id` 为目标用户 UUID 的脱敏稳定标识，`params_redacted='{}'`，不携带密码、token、digest 或原始错误。
   - **不得把 `time_evidence` 绑定 SQL NULL、把 `process_epoch`/`event_seq` 固定写成 0**：现有 v3 迁移中 `time_evidence JSON NOT NULL`、`process_epoch BINARY(16) NOT NULL`、`event_seq BIGINT UNSIGNED NOT NULL` 且 `(process_epoch,event_seq)` 唯一。后续实现应先审查复用 `auth/sqlx_repo.rs::insert_audit` 的窄化事务内接口，或按既有 `system_fallback_time_evidence`、进程 UUID 与 checked `next_event_seq` 语义提供受审等价的固定 11 字段参数化 insert；序号溢出和审计失败均回滚目标修改、会话撤销与 epoch 变化。当前仅为契约，**不提供带假证据/固定序号的可复制 SQL**。
   - 增加同一事务两次管理变更、审计插入故障与 `event_seq` 溢出测试；断言唯一键不冲突、证据非空、失败时持久业务数据与 epoch 原样保留。
8. **事务提交：**
   - `tx.commit().await?` 成功后返回更新后的 `IdentityUser`。

类似地，`create_role`, `patch_role`, `create_grant`, `delete_grant` 均遵循该严格锁序：`lock_integrity_guard` -> 锁 actor 用户行验证有效管理员 -> 锁 roles/grants 相关行 -> CAS 更新 `authz_epoch` -> 写入脱敏审计 -> 提交。

**并发撤权防御测试方案（并发测试）：**
编写测试验证：管理员 A 正在尝试修改角色或停用用户 B，但在获得锁前，管理员 C 已在并发事务中先将管理员 A 停用/撤销管理员；管理员 A 的事务获得锁后，guard 内行重验识别到 A 已非管理员，事务立即以 `PermissionDenied` 失败且不触动 target，确保并发撤权无法产生越权窗口。

- [ ] **Step 4: 运行并确认通过（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::sqlx_repo::tests::last_admin_deactivation_is_prevented_by_error -- --exact
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::sqlx_repo::tests::grant_field_validation_rejects_empty_direct_permissions -- --exact
```
预期输出：每个命令各 `test result: ok. 1 passed; 0 failed`，运行测试数均为 1。

- [ ] **Step 5: 条件性提交（本轮修订不执行；后续需批准）**

```bash
git add crates/rsetup-controller/src/authz/sqlx_repo.rs crates/rsetup-controller/src/devices/repository.rs crates/rsetup-controller/src/error.rs crates/rsetup-controller/src/authz/mod.rs crates/rsetup-controller/src/devices/mod.rs
git commit -m "feat(controller): implement sqlx repositories for authz and last-admin protection"
```

---

### Task 3: 授权服务与设备管理服务整合

**Files:**
- Create: `crates/rsetup-controller/src/authz/service.rs`
- Modify: `crates/rsetup-controller/src/devices/service.rs:1-120`
- Modify: `crates/rsetup-controller/src/authz/mod.rs`

**Interfaces:**
- Consumes:
  - `crate::authz::policy::{Permission, evaluate_grants}`
  - `crate::authz::sqlx_repo::SqlxAuthzRepository`
  - `crate::devices::repository::SqlxDeviceRepository`
  - `crate::auth::service::Session`
- Produces:
  - `AuthzService`:
    - `effective_permissions(actor: &Session, device_id: [u8; 32]) -> Result<HashSet<Permission>, ControllerError>`: 管理员直接返回全集；普通用户先核验 `active && !must_change_password`，再按最新 `authz_epoch` 计算并集。
    - `require_permission(actor: &Session, device_id: [u8; 32], required: Permission) -> Result<(), ControllerError>`
  - `DeviceService` 扩展方法：
    - `patch_profile(actor: &Session, device_id: [u8; 32], expected_revision: u64, display_name: &str) -> Result<(), ControllerError>`
    - `replace_group_members(actor: &Session, group_id: uuid::Uuid, expected_revision: u64, device_ids: &[[u8; 32]]) -> Result<usize, ControllerError>`

- [ ] **Step 0: 建立服务可编译桩与模块注册**

在 `crates/rsetup-controller/src/authz/mod.rs` 注册 `pub mod service;`，并在 `crates/rsetup-controller/src/authz/service.rs` 提供结构体定义与行为 RED 桩：
```rust
// crates/rsetup-controller/src/authz/service.rs
use std::collections::{HashMap, HashSet};
use crate::error::ControllerError;
use crate::auth::service::Session;
use crate::authz::policy::{Permission, GrantRecord, RoleRecord};
use uuid::Uuid;

pub struct AuthzService;

impl AuthzService {
    // 可编译 RED 桩：无论传入何种 session 均返回空集，使得后续 admin 权限测试在编译通过后因行为断言失败
    pub fn eval_session_permissions(
        _session: &Session,
        _grants: &[GrantRecord],
        _roles: &HashMap<Uuid, RoleRecord>,
        _device_groups: &HashSet<Uuid>,
        _device_id: [u8; 32],
    ) -> HashSet<Permission> {
        HashSet::new()
    }
}
```

- [ ] **Step 1: 编写失败测试**

```rust
// crates/rsetup-controller/src/authz/service.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::service::{IdentityUser, Session};

    #[tokio::test]
    async fn admin_has_full_permissions_regardless_of_grants() {
        let admin_user = IdentityUser {
            id: [1u8; 16],
            username: "admin".into(),
            password_hash: "".into(),
            active: true,
            is_admin: true,
            must_change_password: false,
            revision: 1,
        };
        let session = Session {
            user: admin_user,
            digest: [0u8; 32],
        };

        // Even with no grants loaded, admin must have all 4 permissions
        let perms = AuthzService::eval_session_permissions(&session, &[], &std::collections::HashMap::new(), &std::collections::HashSet::new(), [0u8; 32]);
        assert_eq!(perms.len(), 4);
        assert!(perms.contains(&Permission::DeviceRead));
        assert!(perms.contains(&Permission::DeviceStatusRead));
        assert!(perms.contains(&Permission::DeviceReboot));
        assert!(perms.contains(&Permission::DeviceTaskRead));
    }

    #[tokio::test]
    async fn user_with_must_change_password_has_no_device_permissions() {
        let pending_user = IdentityUser {
            id: [2u8; 16],
            username: "alice".into(),
            password_hash: "".into(),
            active: true,
            is_admin: false,
            must_change_password: true,
            revision: 1,
        };
        let session = Session {
            user: pending_user,
            digest: [1u8; 32],
        };

        let perms = AuthzService::eval_session_permissions(&session, &[], &std::collections::HashMap::new(), &std::collections::HashSet::new(), [0u8; 32]);
        assert!(perms.is_empty(), "user requiring password change must have zero device permissions");
    }
}
```

- [ ] **Step 2: 运行并确认失败（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::service::tests::admin_has_full_permissions_regardless_of_grants -- --exact
```
预期输出：可编译通过，运行恰好 1 个测试；FAIL（断言 `assert_eq!(perms.len(), 4)` 失败，桩函数返回了 0 个权限，行为 RED）。检查运行测试数恰好为 1。

- [ ] **Step 3: 最小实现**

```rust
// crates/rsetup-controller/src/authz/service.rs
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use crate::error::ControllerError;
use crate::auth::service::Session;
use crate::authz::policy::{Permission, GrantRecord, RoleRecord, evaluate_grants};
use uuid::Uuid;

pub struct AuthzService {
    // 依赖注入 repo / cache
}

impl AuthzService {
    pub fn eval_session_permissions(
        session: &Session,
        grants: &[GrantRecord],
        roles: &HashMap<Uuid, RoleRecord>,
        device_groups: &HashSet<Uuid>,
        device_id: [u8; 32],
    ) -> HashSet<Permission> {
        if !session.user.active || session.user.must_change_password {
            return HashSet::new();
        }

        if session.user.is_admin {
            return HashSet::from([
                Permission::DeviceRead,
                Permission::DeviceStatusRead,
                Permission::DeviceReboot,
                Permission::DeviceTaskRead,
            ]);
        }

        evaluate_grants(session.user.id, device_id, grants, roles, device_groups)
    }

    pub fn check_permission(
        session: &Session,
        grants: &[GrantRecord],
        roles: &HashMap<Uuid, RoleRecord>,
        device_groups: &HashSet<Uuid>,
        device_id: [u8; 32],
        required: Permission,
    ) -> Result<(), ControllerError> {
        let perms = Self::eval_session_permissions(session, grants, roles, device_groups, device_id);
        if perms.contains(&required) {
            Ok(())
        } else {
            Err(ControllerError::PermissionDenied)
        }
    }
}
```

- [ ] **Step 4: 运行并确认通过（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::service::tests::admin_has_full_permissions_regardless_of_grants -- --exact
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller authz::service::tests::user_with_must_change_password_has_no_device_permissions -- --exact
```
预期输出：每个命令各 `test result: ok. 1 passed; 0 failed`，运行测试数均为 1。

- [ ] **Step 5: 条件性提交（本轮修订不执行；后续需批准）**

```bash
git add crates/rsetup-controller/src/authz/service.rs crates/rsetup-controller/src/devices/service.rs crates/rsetup-controller/src/authz/mod.rs
git commit -m "feat(controller): integrate authorization service with device permission checks"
```

---

### Task 4: 管理 HTTP API 与字段投影路由闭环

**Files:**
- Create: `crates/rsetup-controller/src/api/mod.rs`
- Create: `crates/rsetup-controller/src/api/users.rs`
- Create: `crates/rsetup-controller/src/api/roles.rs`
- Create: `crates/rsetup-controller/src/api/grants.rs`
- Create: `crates/rsetup-controller/src/api/groups.rs`
- Create: `crates/rsetup-controller/src/api/devices.rs`
- Create: `crates/rsetup-controller/src/api/approvals.rs`
- Create: `crates/rsetup-controller/src/api/audit.rs`
- Create: `crates/rsetup-controller/src/api/system.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- Consumes:
  - `crate::http_api::{ok_response, err_response}`
  - `crate::http_security::check_session_write` 与现有 `crate::http_auth` 的单值 Host/Origin/Cookie、CSRF、限速及错误映射边界（当前 `session_cookie_value`、`single_header` 等是私有函数；先经审查抽取/复用，不复制一套解析器）
  - `crate::authz::policy::{Permission, minimum_device_projection}`
  - `crate::auth::service::Session`（只能由可信认证边界产生，禁止在 handler 中伪造）
  - `crate::http_auth::{AppState, HttpAuthState}`
  - Task 2 `SqlxAuthzRepository`、`SqlxDeviceRepository` 与 Task 3 `AuthzService` 的真实查询接口；若缺少设备详情读取/角色与分组读取，先补齐经审查的仓储接口，不以常量替代
- Produces:
  - `build_management_router(state: Arc<AppState>) -> axum::Router`:
    - 返回已绑定状态的 `Router<()>`，保持 Task 6 `auth_router.merge(management_router)` 契约；不要另外声称存在未实现的 `build_management_router_with_state`。
    - 完整管理路由清单（严格契约）：
      - `GET /api/v1/users`
      - `POST /api/v1/users`
      - `PATCH /api/v1/users/{id}`
      - `POST /api/v1/users/{id}/reset-password`
      - `GET /api/v1/users/{id}/effective-permissions`
      - `GET /api/v1/roles`
      - `POST /api/v1/roles`
      - `PATCH /api/v1/roles/{id}`
      - `GET /api/v1/grants`
      - `POST /api/v1/grants`
      - `DELETE /api/v1/grants/{id}`
      - `GET /api/v1/groups`
      - `POST /api/v1/groups`
      - `PATCH /api/v1/groups/{id}`
      - `PUT /api/v1/groups/{id}/members`
      - `GET /api/v1/devices`
      - `GET /api/v1/devices/{id}`
      - `PATCH /api/v1/devices/{id}`
      - `GET /api/v1/approvals`
      - `POST /api/v1/approvals/approve`
      - `POST /api/v1/approvals/reject`
      - `POST /api/v1/approvals/{device_id}/reopen`
      - `POST /api/v1/devices/{id}/revoke`
      - `POST /api/v1/devices/{id}/reauthorize`
      - `GET /api/v1/audit`
      - `GET /api/v1/system/status`

**设计原则与状态解耦：**
1. **真实 AppState 零破坏：** `crate::http_auth::AppState` 只有 `db: DbPool` 与 `auth: Arc<HttpAuthState>`。管理状态另含这同一个 `HttpAuthState` 和真实网关；生产从 `state.db` 装配，测试仅替换依赖，不更换认证入口。`build_management_router(Arc<AppState>) -> Router<()>` 须与 Task 6 的 merge 无状态路由契约一致。
2. **生产与测试同一认证边界：** `GET /api/v1/devices/{id}` 必须先核验单值合法 Host 与唯一有效 `rsc_session` Cookie，以已有 `http_auth.rs` 的严格 cookie 解析、`is_live_session` 预检及 DB 权威 `authenticate` 取得 Session；无 Cookie、重复/畸形 Cookie、过期/撤销/停用会话返回 401 `AUTH_REQUIRED`（后端不确定错误映射 503，绝不当作匿名），`must_change_password` 返回 403 `PASSWORD_CHANGE_REQUIRED`。只有经同一层可信中间件注入的 Session 才可作为替代方案；缺失立即 401，严禁直接相信客户端 Header/可由任意调用方注入的 Extension。当前 `session_cookie_value`、`single_header`、`get_single_origin` 是 `http_auth.rs` 私有函数；须先经审查共享它们或抽取统一认证门禁并使 auth 与管理路由共用，严禁复制新的未审 cookie 解析器。单值 Host 校验在任何投影/仓储调用前完成；GET 不检查 Origin，但需共享只读限速，不把 UI 隐藏视作鉴权。
3. **管理端点安全矩阵：** users/roles/grants/groups/approvals/audit/system 等管理范围按规格逐一应用 admin-only 或有效权限；写端点还要单值 Host、Origin（缺失仅豁免 Origin）、绑定 Session digest 的 CSRF、共享写限速、`application/json` 与 1 MiB 上限，且验证 expected revision。未实现真实业务/门禁的路径不应注册或宣称可用；禁止恒定 200、空集/None 网关、或在认证前先返回静态 403 来伪装闭环。GET 的无权限/不存在资源同形 404 只在已认证后发生。
4. **明确分阶段门禁：** Task 2 所列 `DeviceRepository` 尚无 `get_device` 详情接口，Task 3 示例仅实现内存权限求值，不等于生产实时仓储接线。先交付安全认证与 fixture 投影 RED→GREEN（仅证明 handler 行为，不能声称生产 GREEN）；随后在 Task 2/3 仓储与服务补齐后，将 `ProductionAuthzGateway` 真正从最新 grant/role/group/epoch 求有效权限、`ProductionDeviceGateway` 按设备 ID 查真实档案及状态。两种网关须传播 DB 错误，不可 `Ok(HashSet::new())`、`Ok(None)` 固定返回；生产构建入口和 Task 6 merge 在它们接通并通过验证前保持未完成/不接线。

- [ ] **Step 0: 先复用经审查的认证门禁，再建立可编译的同一 Handler RED 桩**

先在实现阶段把 `http_auth.rs` 已有的严格单值 Host/Cookie 提取与 `HttpAuthState.auth.is_live_session` + `authenticate`、错误映射整理为 auth 与管理路由共用的可信门禁（或在同一层可信中间件注入不可由外部伪造的认证上下文）。管理状态必须携带 `Arc<HttpAuthState>`；测试装配须提供 fake `AuthServiceApi`，只有显式有效 cookie 经该 fake authenticate 才得到测试 Session。无认证时不论 RED/GREEN 均返回 401、零网关调用。只在通过认证后使用可编译但尚未落实投影的 RED 行为，使同一 handler 的授权/投影断言按预期失败。不得在 Step 0 就装配空生产网关或以缺 Cookie 时的默认 actor 维持编译。

下列是接口约束伪代码，**非可直接复制的完整实现**（复用门禁的签名、错误映射和 Axum 提取器由实现时根据已审代码完成）：

```rust
// crates/rsetup-controller/src/api/devices.rs — RED 和 GREEN 共用的入口
#[derive(Clone)]
pub struct DeviceApiState {
    pub auth: Arc<crate::http_auth::HttpAuthState>,
    pub authz: Arc<dyn ManagementAuthzGateway>,
    pub devices: Arc<dyn ManagementDeviceGateway>,
}

pub async fn get_device(
    State(state): State<DeviceApiState>,
    Path(device_id_hex): Path<String>,
    headers: HeaderMap,
) -> Response {
    // 必须由与 http_auth 共用的可信函数实施单值 Host/Cookie、clock gate、
    // DB-authoritative authenticate、active、must_change_password 和只读限速。
    let actor = match authenticate_management_read(&state.auth, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response, // 缺失/失效为固定 401，不能调用网关
    };
    // RED: 仅在已认证之后暂时返回未经投影的 200；随 Step 3 原地替换。
    // 解析 ID、调用 state.authz.effective_permissions(&actor, id)、
    // state.devices.get_device(id) 均在认证成功后。
    let _ = (actor, device_id_hex);
    StatusCode::OK.into_response()
}

pub fn devices_router(state: DeviceApiState) -> Router<()> {
    Router::new().route("/api/v1/devices/{id}", get(get_device)).with_state(state)
}
```

`ManagementDeviceGateway::get_device(id) -> Result<Option<DeviceRecordView>, ControllerError>` 与 `ManagementAuthzGateway::effective_permissions(actor: &Session, id) -> Result<HashSet<Permission>, ControllerError>` 保留为注入接口。`DeviceRecordView` 的字段及来源必须匹配真实仓储投影；例如 `display_name: String`，`admission_state`、`review_decision` 为经验证的持有型类型/值，`revision: u64`。fixture 可静态返回，生产不可。阶段一仅导出测试用的 `devices_router`，`pub mod api` 可在依赖齐备后注册；不要把仅有 fixture 的路由标为生产装配已完成。

**生产装配前置（不能用恒空实现跨越）：** `build_management_router(state: Arc<AppState>) -> Router<()>` 在 Task 2/3 真实读接口落地后定义：保留 `state.auth.clone()`，用 `state.db.clone()` 构造实际 `SqlxAuthzRepository`、`SqlxDeviceRepository`/`AuthzService` 适配器；`effective_permissions` 每次按当前用户授权/设备分组计算并集（依最新 DB/epoch，不能从前端权限表推断），`get_device` 从设备仓储按 public key 读真实记录并区分 `None` 与 DB 故障。不允许 `ProductionAuthzGateway` 固定空集合或 `ProductionDeviceGateway` 固定 `None`；仓储缺少上述读方法时先补齐并 review，不能写一个貌似可编译的 GREEN 桩。其他模块 `users/roles/grants/groups/approvals/audit/system` 不得以空文件/总是成功的路由充数，按安全矩阵与真仓储逐端点交付。对外签名和 `Router<()>` 保持 Task 6 merge 契约，未就绪时不接入生产路由。


- [ ] **Step 1: 先写未鉴权 401，再写已鉴权的可见性与字段投影失败测试**

全部测试 `devices_router(DeviceApiState { auth: fake_http_auth_state, authz: fixture, devices: fixture })` 的**同一个 `get_device`**，fixture 只替换认证服务/仓储网关，不替换认证门禁或 handler。测试 fake `AuthServiceApi` 仅在其显式登记的合法 token 上 `is_live_session == true` 且 `authenticate` 返回由 fake 登记的真实语义 actor；测试不能靠请求扩展伪造 Session，也不能让 fake 对任意 token/缺失 cookie 认证成功。fixture 权限网关断言 `actor.user.id` 为已登记用户；否则失败，以防空 actor 潜入。

测试顺序与断言：

1. **先验证无鉴权拒绝：** 合法 Host、可见设备 ID、无 Cookie 请求得到 401 `AUTH_REQUIRED` / `auth.required`；验证权限/设备网关调用均为 0。重复或畸形 `rsc_session`、仅有其他 Cookie、已撤销/不活跃 Session 同样 401；错误 Host 固定 403 `CSRF_INVALID`。`must_change_password` 的已认证 actor 返回 403 `PASSWORD_CHANGE_REQUIRED`，同样没有网关调用；后端鉴权故障是 503 而不是 401/404。避免测试以裸请求期待 404/200。
2. **再验证授权资源与权限门禁：** 每个不可见、缺失和 reboot-only 请求都带合法 Host 与同一 fake 明确认可的单值 cookie，确保 `authenticate` 被调用且权限网关接到该 actor。不可见设备存在于 fixture 但有效权限为空；缺失设备 `get_device` 返回 `None` 且 fixture 可给有效权限以确保走到存在性判断。两者状态均 404、`error.code == "NOT_FOUND"`、`message_key == "device.not_found"`，比较去掉 request_id 后的同形响应，不泄露档案。
3. **详情权限门禁（02 §5）：** `GET /api/v1/devices/{id}` 访问设备详情，依规格必须具备 `device.read` 权限。若已鉴权 actor 仅有 `Permission::DeviceReboot`（即 reboot-only 用户，缺少 `device.read`），访问 `GET /api/v1/devices/{id}` 必须严格拒绝（返回 403 `PERMISSION_DENIED` 或不可见同形 404，遵从 01/02 规格；最低识别投影 `minimum_device_projection` 仅适用于 `GET /api/v1/devices` 设备列表，不可用于详情绕过 `device.read`）。只有具备 `device.read` 权限的 actor 才能读取设备档案详情（200），返回包含 `admission_state`、`review_decision`、`revision` 等字段。补测用例：`reboot_only_device_detail_is_denied_without_device_read`。
4. **管理写门禁补测：** 合法 session 但缺/错 CSRF、重复 Host/Origin、错误 Origin、错误 JSON Content-Type、超 1 MiB body、非 admin（管理范围）及 `must_change_password` 均不触发写网关；单值合法 Host、可信 Origin 或缺 Origin、绑定 CSRF、JSON 与授权 actor 才可进入业务层。按规格断言固定错误代码及限速行为。每个路由使用同一 auth 边界，不做只测纯策略函数的替代测试。

RED 的目标是：未鉴权 401 测试从第一版可编译桩即通过；在明确通过身份校验后，不可见/不存在 404 和 reboot-only 详情拒绝断言因同一个 handler 暂返回 200 而失败。若先失败在编译、认证边界或 fixture 设置，则不能谎称得到行为 RED。


- [ ] **Step 2: 按安全顺序运行并确认 RED（仅实施时；本次计划修订不运行代码）**

先运行未鉴权 401 测试，必须通过且零网关调用；随后运行已鉴权的 404 与 reboot-only 详情拒绝测试，必须在同一 handler 的业务行为断言失败，而非编译错误或认证失败。示例过滤器（落实时与实际测试函数名对齐）：

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller api::devices::tests::unauthenticated_get_device_returns_401 -- --exact
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller api::devices::tests::invisible_and_missing_device_return_same_404 -- --exact
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller api::devices::tests::reboot_only_device_detail_is_denied_without_device_read -- --exact
```

记录每项实际执行数量及断言位置；未确认真实 RED 不进入 GREEN。


- [ ] **Step 3: 在同一 Handler 原地实现已鉴权投影与详情权限门禁（fixture GREEN，不冒充生产）**

认证前置保留 Step 0 的共用 `authenticate_management_read(&state.auth, &headers).await`，**认证成功后**再解析 ID、查权限、查设备。根据 02 §5 规格，`GET /api/v1/devices/{id}` 必须具备 `device.read`，无 `device.read` 拒绝；最低投影仅用于 `GET /api/v1/devices` 列表：

```rust
// 同一个 get_device 内，actor 来自共用认证门禁；无 Cookie 不会进入这里。
let device_id = match crate::parse_device_id(&device_id_hex) {
    Ok(id) => id,
    Err(_) => return err_response(StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT", "device.invalid_id", json!({})),
};
let perms = match state.authz.effective_permissions(&actor, device_id).await {
    Ok(p) => p,
    Err(ControllerError::Database(_)) => return err_response(StatusCode::SERVICE_UNAVAILABLE,
        "STORAGE_UNAVAILABLE", "system.storage_unavailable", json!({})),
    Err(_) => return err_response(StatusCode::SERVICE_UNAVAILABLE,
        "NOT_READY", "system.not_ready", json!({})),
};
if perms.is_empty() {
    return err_response(StatusCode::NOT_FOUND, "NOT_FOUND", "device.not_found", json!({}));
}
// 02 §5: GET /devices/{id} 必须具备 device.read；无 device.read 则拒绝，不可返回 200 详情
if !perms.contains(&Permission::DeviceRead) {
    return err_response(StatusCode::FORBIDDEN, "PERMISSION_DENIED", "permission.denied", json!({}));
}
let device_record = match state.devices.get_device(device_id).await {
    Ok(Some(d)) => d,
    Ok(None) => return err_response(StatusCode::NOT_FOUND,
        "NOT_FOUND", "device.not_found", json!({})),
    Err(ControllerError::Database(_)) => return err_response(StatusCode::SERVICE_UNAVAILABLE,
        "STORAGE_UNAVAILABLE", "system.storage_unavailable", json!({})),
    Err(_) => return err_response(StatusCode::SERVICE_UNAVAILABLE,
        "NOT_READY", "system.not_ready", json!({})),
};
let mut perm_strings: Vec<_> = perms.iter().map(|p| p.as_str()).collect();
perm_strings.sort_unstable();
ok_response(json!({
    "device_id": hex::encode(device_id),
    "display_name": device_record.display_name,
    "admission_state": device_record.admission_state,
    "review_decision": device_record.review_decision,
    "revision": device_record.revision.to_string(),
    "effective_permissions": perm_strings,
}))
```

本阶段只允许说测试注入的认证服务与仓储 fixture 下投影与门禁 GREEN。最低投影用于列表，详情无 read 严格拒绝。生产网关尚未接入则不能称管理 API 已可生产使用。


- [ ] **Step 4: 分两级验证 GREEN 与生产接线门禁（实施时执行，本次未运行）**

第一阶段复跑 Step 2 三项测试，以及 Cookie 畸形/重复、已撤销与停用、必须改密、Host、读限速、DB 故障；严格确认实际测试数与状态。`fixture GREEN` 只表示共用认证入口后同一 handler 的 401、404、无 `device.read` 详情拒绝及有权 200 可用。

第二阶段须完成并验证 Task 2/3 的实际仓储读接口：`SqlxAuthzRepository` 读取最新 grants/roles、`SqlxDeviceRepository` 读取设备分组及设备详情，`AuthzService` 在当次请求中计算有效权限；生产网关委托这些真实实现，异常传播为 503 而非空权限/不存在。用生产构建入口（可注入隔离仓储或获授权的隔离数据库集成测试）确认未鉴权 401、已鉴权且有 `device.read` 可见 200、不可见/不存在同形 404、reboot-only 访问详情严格拒收（403）及 DB 故障 503，并审查管理写完整安全矩阵。缺一项则保持 Task 4 和 Task 6 merge 前置未完成，不能把纯 fixture 测试通过描述为生产 GREEN。

- [ ] **Step 5: 条件性交付（本次不 stage/commit）**

只有共用认证边界、所有实际管理路由的安全矩阵、真实生产网关、验证证据与 Task 6 `build_management_router(Arc<AppState>) -> Router<()>` merge 契约均完成后，才进入后续提交/接线审查；不以未实现端点/桩代替功能。

---

### Task 5: Root CLI `reset-admin` 本机管理员恢复机制

**Files:**
- Create: `crates/rsetup-controller/src/admin_cli.rs`
- Modify: `crates/rsetup-controller/src/main.rs:1-60`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- Consumes:
  - `crate::db::DbPool`
  - `crate::integrity::lock_integrity_guard`
  - `crate::auth::password::PasswordHasher`
  - `std::io::IsTerminal`（标准库交互 TTY 校验）
- Produces:
  - `run_reset_admin(db: &DbPool) -> Result<(), ControllerError>`:
    1. 校验 OS 实际 `real UID == 0` 且 `effective UID == 0`（不信任任何环境变量）。
    2. 校验 controlling TTY：三标准流 `stdin`, `stdout`, `stderr` 各自 `is_terminal()` 仅是必要条件，不保证 controlling TTY。**在修改数据库前必须先行打开、核验并持有 `/dev/tty` 的只写文件句柄**（绝对不能 commit 后才尝试打开）；若打开失败或不是合法 TTY，必须在任何写库操作前直接拒绝退出，防止写库成功后因终端无法输出导致密码丢失。
    3. 操作且仅操作引导账号 `username == "admin"`；该账号必须存在且仍为 `active && is_admin`，否则在修改数据库前拒绝。
    4. 在单个事务内锁定 `schema_meta(singleton=1)` 保护行与 admin 用户行：
       - 生成至少 128 位熵的随机新密码。
       - 计算 Argon2id 哈希。
       - 更新 `users.password_hash`，设置 `must_change_password = true`，自增 `revision`，断言受影响行数 `rows_affected == 1`。
       - 撤销该用户全部既有 sessions。
       - 写入脱敏审计（参数化 `INSERT INTO audit_events`，标准 11 字段：`event_type = "auth.reset-admin"`, `actor_kind = "system"`, `target_kind = "user"`, `target_id = hex(admin_user_id)`，参数全脱敏 `{}`，零明文秘密）。
       - checked CAS 递增 `authz_epoch`：由于账号密码重置导致会话失效与全局鉴权状态变更，同事务递增 `schema_meta.authz_epoch`，断言受影响行数 `rows_affected == 1`。
    5. 仅在事务确认 commit 成功后：
       - **仅通过预先持有验证的 `/dev/tty` 句柄**输出一次性临时凭据，严格 `flush` 同步后关闭。
       - 绝不输出至标准输出、标准错误、常规日志或审计表。
       - 保持未知 commit（如 commit 返回网络/连接错误时）绝对不输出秘密，防止未落库密码被用户误用；若提交后 TTY 写失败，绝不重复输出或尝试读取旧密码，由 root 受控再次触发 reset。
       - 后续验证门禁：该 CLI 实现后必须经由独立安全审查，并在**真实 PTY + 独立隔离 DB** 环境下通过真实流程验证。

- [ ] **Step 0: 依赖审查说明与 CLI 模块可编译桩**

说明：`crates/rsetup-controller/Cargo.toml` 当前无直接 `libc` 依赖。`libc` 与 `clap` 虽均已在根 workspace `Cargo.toml` 声明且在 `Cargo.lock` 中锁定（根锁文件已有 clap 4.5 与 libc 0.2），但子 crate 仍须在自身 `Cargo.toml` 显式声明 `libc.workspace = true`，并离线更新核验 `Cargo.lock`，不能声称仅根依赖存在即 `--locked` 可用（本轮修订不修改 Cargo 文件；后续需批准）。CLI 模块明确不引入 `clap` 繁重依赖，保持轻量无外部新库设计。

在未来执行阶段经审查将 `libc.workspace = true` 加入 `crates/rsetup-controller/Cargo.toml`：
```toml
# crates/rsetup-controller/Cargo.toml (未来执行经审查增补)
[dependencies]
argon2.workspace = true
axum.workspace = true
chrono.workspace = true
hex.workspace = true
libc.workspace = true
rand.workspace = true
serde.workspace = true
serde_json.workspace = true
sha2.workspace = true
sqlx.workspace = true
thiserror.workspace = true
tokio.workspace = true
uuid.workspace = true
```

在 `crates/rsetup-controller/src/lib.rs` 注册 `pub mod admin_cli;`，并在 `crates/rsetup-controller/src/admin_cli.rs` 中提供可编译 RED 桩：
```rust
// crates/rsetup-controller/src/admin_cli.rs
use crate::error::ControllerError;

// 可编译 RED 桩：无论传入何种 UID 均返回 Ok(())，使非 root UID 拒绝断言在编译后失败
pub fn verify_root_credentials(_ruid: u32, _euid: u32) -> Result<(), ControllerError> {
    Ok(())
}

// 可编译 RED 桩：恒返回 Ok(())，使非交互流断言失败
pub fn verify_interactive_tty(_stdin_tty: bool, _stdout_tty: bool, _stderr_tty: bool) -> Result<(), ControllerError> {
    Ok(())
}
```

- [ ] **Step 1: 编写失败测试**

```rust
// crates/rsetup-controller/src/admin_cli.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_admin_rejects_non_root_uid() {
        // 单元断言：非 0 UID 必须被立即拒绝
        let res = verify_root_credentials(1000, 1000);
        assert!(matches!(res, Err(ControllerError::PermissionDenied)));

        let res_spoofed = verify_root_credentials(1000, 0);
        assert!(matches!(res_spoofed, Err(ControllerError::PermissionDenied)));

        let res_root = verify_root_credentials(0, 0);
        assert!(res_root.is_ok());
    }

    #[test]
    fn reset_admin_rejects_non_interactive_streams() {
        assert!(verify_interactive_tty(false, true, true).is_err());
        assert!(verify_interactive_tty(true, false, true).is_err());
        assert!(verify_interactive_tty(true, true, false).is_err());
        assert!(verify_interactive_tty(true, true, true).is_ok());
    }
}
```

- [ ] **Step 2: 运行并确认失败（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller admin_cli::tests::reset_admin_rejects_non_root_uid -- --exact
```
预期输出：可编译通过，运行恰好 1 个测试；FAIL（断言 `matches!(res, Err(ControllerError::PermissionDenied))` 失败，桩函数返回了 `Ok(())`，行为 RED）。检查运行测试数恰好为 1。

- [ ] **Step 3: 最小实现契约与 TTY 安全持有规范**

明确：下列逻辑为不可直接复制的事务与执行顺序规范契约，执行阶段严格落实：

```rust
// crates/rsetup-controller/src/admin_cli.rs
use std::io::{IsTerminal, Write};
use std::fs::OpenOptions;
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;
use sqlx::Row;
use crate::error::ControllerError;
use crate::db::DbPool;
use crate::auth::password::PasswordHasher;

// 独立于 auth/admission 事件生产者，且每次进程启动随机产生；不要使用固定 epoch/seq。
static RESET_ADMIN_PROCESS_EPOCH: OnceLock<uuid::Uuid> = OnceLock::new();
static RESET_ADMIN_EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

pub fn verify_root_credentials(ruid: u32, euid: u32) -> Result<(), ControllerError> {
    if ruid == 0 && euid == 0 {
        Ok(())
    } else {
        Err(ControllerError::PermissionDenied)
    }
}

pub fn verify_interactive_tty(stdin_tty: bool, stdout_tty: bool, stderr_tty: bool) -> Result<(), ControllerError> {
    if stdin_tty && stdout_tty && stderr_tty {
        Ok(())
    } else {
        Err(ControllerError::Config("controlling TTY and interactive stdin/stdout/stderr required".into()))
    }
}

pub async fn run_reset_admin(db: &DbPool) -> Result<(), Box<dyn std::error::Error>> {
    // 1. 获取真实 UID 与 Effective UID，必须双 0
    let ruid = unsafe { libc::getuid() };
    let euid = unsafe { libc::geteuid() };
    verify_root_credentials(ruid, euid)?;

    // 2. 检查三标准流是否均为交互终端
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    let stderr_tty = std::io::stderr().is_terminal();
    verify_interactive_tty(stdin_tty, stdout_tty, stderr_tty)?;

    // 3. 在写库前，先行打开、校验并持有 /dev/tty 只写文件句柄；打开失败在修改 DB 前直接拒绝
    let mut tty = OpenOptions::new()
        .write(true)
        .open("/dev/tty")
        .map_err(|e| ControllerError::Config(format!("cannot open /dev/tty before transaction: {e}")))?;

    // 4. 生成 128-bit 随机密码并计算 Argon2id 哈希
    let raw_bytes: [u8; 16] = rand::random();
    let temporary_password = hex::encode(raw_bytes);
    let hasher = PasswordHasher::default();
    let password_hash = hasher.hash(&temporary_password)?;

    // 5. 数据库原子提交（统一锁序：guard -> users(admin)）
    let mut tx = db.0.begin().await?;
    crate::integrity::lock_integrity_guard(&mut tx).await?;

    let user_row = sqlx::query(
        "SELECT id, CAST(active AS SIGNED) as active, CAST(is_admin AS SIGNED) as is_admin, revision \
         FROM users WHERE username = 'admin' FOR UPDATE"
    )
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ControllerError::Config("admin user not found".into()))?;

    // 严格 0/1 校验
    let active_raw: i64 = user_row.try_get("active")?;
    let is_admin_raw: i64 = user_row.try_get("is_admin")?;
    if (active_raw != 0 && active_raw != 1) || (is_admin_raw != 0 && is_admin_raw != 1) {
        return Err(ControllerError::Config("corrupt boolean in admin user row".into()).into());
    }
    let is_active = active_raw == 1;
    let is_admin = is_admin_raw == 1;
    let user_id: Vec<u8> = user_row.try_get("id")?;
    let rev = user_row.try_get::<u64, _>("revision")?;

    if !is_active || !is_admin {
        return Err(ControllerError::Config("admin account is inactive or not an admin; cannot reset".into()).into());
    }

    let next_rev = rev.checked_add(1).ok_or_else(|| ControllerError::Config("revision overflow".into()))?;

    let update_res = sqlx::query("UPDATE users SET password_hash = ?, must_change_password = 1, revision = ? WHERE username = 'admin' AND revision = ?")
        .bind(&password_hash).bind(next_rev).bind(rev)
        .execute(&mut *tx).await?;
    if update_res.rows_affected() != 1 {
        return Err(ControllerError::RevisionConflict.into());
    }

    sqlx::query("UPDATE sessions SET revoked = 1 WHERE user_id = ? AND revoked = 0")
        .bind(&user_id)
        .execute(&mut *tx).await?;

    // checked CAS 更新 authz_epoch
    let old_epoch: u64 = sqlx::query_scalar("SELECT authz_epoch FROM schema_meta WHERE singleton = 1")
        .fetch_one(&mut *tx).await?;
    let next_epoch = old_epoch.checked_add(1).ok_or_else(|| ControllerError::Config("epoch overflow".into()))?;
    let epoch_res = sqlx::query("UPDATE schema_meta SET authz_epoch = ? WHERE singleton = 1 AND authz_epoch = ?")
        .bind(next_epoch).bind(old_epoch)
        .execute(&mut *tx).await?;
    if epoch_res.rows_affected() != 1 {
        return Err(ControllerError::RevisionConflict.into());
    }

    // 同事务脱敏审计：三个非空证据分别来自独立进程 UUID、checked 序号、
    // 明确标记 system_fallback 的时间来源；审计失败必须回滚全部业务修改。
    let audit_id = *uuid::Uuid::new_v4().as_bytes();
    let target_id_hex = hex::encode(&user_id);
    let time_evidence = crate::db::system_fallback_time_evidence();
    let process_epoch = RESET_ADMIN_PROCESS_EPOCH.get_or_init(uuid::Uuid::new_v4);
    let event_seq = crate::db::next_event_seq(&RESET_ADMIN_EVENT_SEQ)?;
    sqlx::query(
        "INSERT INTO audit_events (id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted, outcome, time_evidence, process_epoch, event_seq) \
         VALUES (?, 'system', NULL, 'auth.reset-admin', 'user', ?, '{}', 'success', ?, ?, ?)"
    )
    .bind(&audit_id[..])
    .bind(&target_id_hex)
    .bind(&time_evidence)
    .bind(process_epoch.as_bytes().as_slice())
    .bind(event_seq)
    .execute(&mut *tx).await?;

    // 提交事务。若提交报错，结果未知，直接向调用方传播错误，绝对不得输出密码
    tx.commit().await?;

    // 6. 仅在 commit 明确成功后，使用写库前已持有的 tty 句柄输出；禁止输出至 stdout/stderr/日志
    writeln!(tty, "\n[EMERGENCY RESET] Administrator password reset successfully.")?;
    writeln!(tty, "Username: admin")?;
    writeln!(tty, "Temporary Password: {temporary_password}")?;
    writeln!(tty, "Notice: Password must be changed upon first login.\n")?;
    tty.flush()?;

    Ok(())
}
```

在 `crates/rsetup-controller/src/main.rs` 中加入子命令分发逻辑：
```rust
// main.rs
if std::env::args().nth(1).as_deref() == Some("reset-admin") {
    let config = ControllerConfig::from_env()?;
    let db = DbPool::connect(&config).await?;
    rsetup_controller::admin_cli::run_reset_admin(&db).await?;
    return Ok(());
}
```

- [ ] **Step 4: 运行并确认通过（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller admin_cli::tests::reset_admin_rejects_non_root_uid -- --exact
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller admin_cli::tests::reset_admin_rejects_non_interactive_streams -- --exact
```
预期输出：每个命令各 `test result: ok. 1 passed; 0 failed`，运行测试数均为 1。

- [ ] **Step 5: 条件性提交（本轮修订不执行；后续需批准）**

```bash
git add crates/rsetup-controller/src/admin_cli.rs crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/lib.rs
git commit -m "feat(controller): implement local root reset-admin cli recovery tool"
```

---

### Task 6: 安全生产路由器接线与服务就绪检查

**Files:**
- Modify: `crates/rsetup-controller/src/main.rs:1-65`
- Modify: `crates/rsetup-controller/src/lib.rs:35-56`
- Modify: `crates/rsetup-controller/src/http_live.rs:1-35`

**Interfaces:**
- Consumes:
  - `crate::http_auth::{AppState, HttpAuthState, build_http_router}`
  - `crate::http_security::HttpSecurityConfig`
  - `crate::http_live::LiveAuth`
  - `crate::api::build_management_router`
  - `crate::db::check_identity_schema`
- Produces:
  - `build_production_router(state: AppState) -> axum::Router`:
    - 复用 `build_http_router(state.clone())`（内置 `/healthz`、`/readyz` 探活与 `/api/v1/auth/*` 认证会话端点，严禁外部重复定义探活路由）
    - 合并 `build_management_router(Arc::new(state))` 管理路由（通过 `Router::merge` 组合无状态 `Router<()>`，路由互斥无冲突）
  - `main.rs`: 启动前校验安全配置与 v3 schema，未就绪则服务退出且不监听端口；就绪后完成 `LiveAuth` 生产适配器注入、组装 `AppState`、构建生产路由并绑定监听。
  - **关键生产装配契约（ConnectInfo）：** 生产真实监听绑定必须使用 `axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>())`。因为生产 `http_auth.rs` 中 `login` 依赖 `Option<Extension<ConnectInfo<SocketAddr>>>` 获取可信对端 IP 用于限速防爆破；若缺少 `into_make_service_with_connect_info`，登录端点将因缺少 ConnectInfo 直接返回 503 `NOT_READY`。

**分层与职责明确（安全前置 vs DB 真验收）：**
- **启动安全前置与接线：** `main.rs` 负责启动前安全门禁（`ControllerConfig`, `HttpSecurityConfig`, `check_identity_schema`, `bootstrap_admin`）与生产适配器 `LiveAuth` 注入，属于启动接线职责。生产装配必须注入对端 `SocketAddr`。
- **离线依赖注入测试：** Task 6 路由装配测试通过依赖注入（`TestAuthStub` 实现 `AuthServiceApi`，`connect_lazy` 虚拟连接池）纯进程内离线运行，不读写秘密、不建立物理数据库连接。测试不能只 assert 非 404，必须显式测试合法 Host/Origin/JSON 请求在注入 `ConnectInfo` peer 下的真实生产装配行为，以及缺少 peer 时的 503 失败边界。
- **DB 真实验收严格解耦：** 真实 MySQL/TiDB 数据库的双引擎事务一致性、并发最后管理员保护、锁等待观察等严格隔离在 Task 7 与 Task 8 门禁测试中，不与 Task 6 路由接线及单元测试混淆。

- [ ] **Step 0: 生产路由器单一被测函数可编译 RED 桩**

在 `crates/rsetup-controller/src/lib.rs` 中提供与生产目标完全一致的单一被测函数 `build_production_router`（此时为可编译的 RED 桩实现，仅调用既有探活路由）：
```rust
// crates/rsetup-controller/src/lib.rs
pub fn build_production_router(state: AppState) -> axum::Router {
    // 可编译 RED 桩：仅挂载基础探活路由，未挂载 auth 路由与管理路由
    build_router(state.db)
}
```

- [ ] **Step 1: 编写失败测试（依赖注入，无需外部服务）**

在 `crates/rsetup-controller/src/lib.rs` 中编写单元测试。测试通过依赖注入提供离线 `TestAuthStub` 与 `connect_lazy` 虚拟连接池，不连接真实数据库、不读写真实凭据。测试不能只以裸请求 assert 非 404，必须提供合法 Host、Origin、Content-Type 与合规 JSON 体，并显式覆盖两项边界：
1. 缺少 `ConnectInfo` 时，调用 `POST /api/v1/auth/login` 必须返回 503 `NOT_READY`（`system.not_ready`）。
2. 在请求 extensions 中显式注入 `ConnectInfo(peer)` 后，装配的生产路由进入认证服务，返回由 fake stub 定义的业务响应（如 403 `PERMISSION_DENIED` 或对应测试认证结果），验证真实生产装配连通性且非 404。

```rust
// crates/rsetup-controller/src/lib.rs
#[cfg(test)]
mod prod_router_tests {
    use super::*;
    use std::sync::Arc;
    use std::net::SocketAddr;
    use axum::extract::ConnectInfo;
    use axum::http::{header, Request, StatusCode};
    use tower::ServiceExt;
    use crate::http_auth::{AuthFuture, AuthServiceApi, HttpAuthState};
    use crate::http_security::HttpSecurityConfig;
    use crate::auth::service::{Login, Session, SessionPage};

    struct TestAuthStub;
    impl AuthServiceApi for TestAuthStub {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, _raw: &'a str) -> AuthFuture<'a, Session> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
        fn change_password<'a>(&'a self, _s: &'a Session, _c: &'a str, _n: &'a str) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
        fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            Box::pin(async { Ok(1) })
        }
        fn is_live_session(&self, _digest: &[u8; 32]) -> bool {
            false
        }
        fn list_sessions<'a>(&'a self, _s: &'a Session, _cursor: Option<&'a str>, _limit: usize) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
        fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
        fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async { Err(ControllerError::PermissionDenied) })
        }
    }

    fn test_app_state() -> AppState {
        let db = DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(100))
                .connect_lazy_with(sqlx::mysql::MySqlConnectOptions::new()
                    .host("127.0.0.1").port(1).username("synthetic")
                    .database("unreachable_fixture")),
        );
        let sec_config = HttpSecurityConfig {
            allowed_hosts: vec!["127.0.0.1:8080".into()],
            allowed_origin: Some("http://127.0.0.1:8080".into()),
            trust_tls: false,
        };
        let auth_state = Arc::new(HttpAuthState::new(Arc::new(TestAuthStub), sec_config));
        AppState { db, auth: auth_state }
    }

    #[tokio::test]
    async fn production_router_mounts_auth_and_verifies_peer_boundaries() {
        let state = test_app_state();
        let app = build_production_router(state);

        // 1. Probes: /healthz 必须就绪可用
        let req_health = Request::builder().uri("/healthz").body(axum::body::Body::empty()).unwrap();
        let resp_health = app.clone().oneshot(req_health).await.unwrap();
        assert_eq!(resp_health.status(), StatusCode::OK);

        // 2. 缺少 ConnectInfo 边界：合规报文在无 peer 时必须失败关闭为 503 NOT_READY
        let req_no_peer = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header(header::HOST, "127.0.0.1:8080")
            .header("origin", "http://127.0.0.1:8080")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("{\"username\":\"admin\",\"password\":\"pass\"}"))
            .unwrap();
        let resp_no_peer = app.clone().oneshot(req_no_peer).await.unwrap();
        // RED 桩下仅挂载 probe，返回 404 NOT_FOUND，断言失败
        assert_eq!(resp_no_peer.status(), StatusCode::SERVICE_UNAVAILABLE, "missing ConnectInfo must return 503");

        // 3. 注入 ConnectInfo peer：必须进入认证逻辑，fake 明确返回 InvalidArgument，handler 将其映射为 401 INVALID_CREDENTIALS；不是只检验“非404”。
        let peer: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let mut req_with_peer = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header(header::HOST, "127.0.0.1:8080")
            .header("origin", "http://127.0.0.1:8080")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from("{\"username\":\"admin\",\"password\":\"pass\"}"))
            .unwrap();
        req_with_peer.extensions_mut().insert(ConnectInfo(peer));
        let resp_with_peer = app.oneshot(req_with_peer).await.unwrap();
        assert_ne!(resp_with_peer.status(), StatusCode::NOT_FOUND, "/api/v1/auth/login must be mounted in production router");
        assert_eq!(resp_with_peer.status(), StatusCode::UNAUTHORIZED, "reached fake auth login -> 401 INVALID_CREDENTIALS");
    }
}
```

- [ ] **Step 2: 运行并确认失败（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller prod_router_tests::production_router_mounts_auth_and_verifies_peer_boundaries -- --exact
```
预期输出：可编译通过，运行恰好 1 个测试；FAIL（断言 `assert_eq!(resp_no_peer.status(), StatusCode::SERVICE_UNAVAILABLE)` 失败，桩函数仅挂载探活路由，`/api/v1/auth/login` 返回 404 NOT_FOUND，行为 RED）。检查运行测试数恰好为 1。

- [ ] **Step 3: 最小实现与安全前置接线**

在 `crates/rsetup-controller/src/lib.rs` 中将 `build_production_router` 替换为完整实现，直接复用已合并探活的 `build_http_router` 并安全 merge 管理路由，不重复定义探活路由、不产生路由或状态冲突：
```rust
// crates/rsetup-controller/src/lib.rs
pub fn build_production_router(state: AppState) -> axum::Router {
    // build_http_router 已包含 crate::build_router(state.db) 提供的 /healthz 与 /readyz，
    // 以及 /api/v1/auth/* 认证会话端点，返回已绑定状态的 Router<()>。
    let auth_router = crate::http_auth::build_http_router(state.clone());

    // 管理路由（由 Task 4 产出）绑定自身所需状态后同样为 Router<()>，
    // 路由路径互斥，安全合并，严禁重复注册探活路由或认证路由。
    let management_router = crate::api::build_management_router(Arc::new(state));

    auth_router.merge(management_router)
}
```

在 `crates/rsetup-controller/src/http_live.rs` 顶层文档中更新架构状态，声明 `LiveAuth` 现已正式作为生产适配器在 `main.rs` 中完成装配与安全前置接线（真实数据库事务验收与锁观测仍由 Task 7 与 Task 8 门禁测试负责，测试与真实 DB 验收严格解耦）：
```rust
// crates/rsetup-controller/src/http_live.rs 顶部文档更新
//! Production `LiveAuth` adapter over the object-safe `AuthServiceApi`
//! boundary.
//!
//! Wired into `main.rs` via `build_production_router(app_state)`. Startup
//! enforces schema v3 verification and security config prerequisites before
//! serving. Real-database transaction acceptance and lock observation are
//! decoupled into gated test suites (Task 7 & 8).
```

在 `crates/rsetup-controller/src/main.rs` 中完整装配所有依赖并启动监听，显式使用 `into_make_service_with_connect_info::<SocketAddr>()`：
```rust
// crates/rsetup-controller/src/main.rs
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("reset-admin") {
        let config = ControllerConfig::from_env()?;
        let db = DbPool::connect(&config).await?;
        rsetup_controller::admin_cli::run_reset_admin(&db).await?;
        return Ok(());
    }

    let config = ControllerConfig::from_env()?;
    let security_config = rsetup_controller::http_security::HttpSecurityConfig::from_env()?;
    let db = DbPool::connect(&config).await?;
    let secret_path = std::env::var_os("CONTROLLER_BOOTSTRAP_SECRET_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "controller-bootstrap-secret.log".into());

    start_if_ready(check_identity_schema(&db), || async {
        bootstrap_admin(&db, &LogSecretSink(secret_path)).await?;

        // 装配核心仓储与生产 LiveAuth 适配器（安全前置就绪）
        let identity_repo = Arc::new(rsetup_controller::auth::sqlx_repo::SqlxIdentityRepository::new(db.clone()));
        let auth_service = Arc::new(rsetup_controller::auth::service::AuthService::new(identity_repo)?);
        let live_auth: Arc<dyn rsetup_controller::http_auth::AuthServiceApi> = Arc::new(
            rsetup_controller::http_live::LiveAuth::new(auth_service.clone(), db.clone())
        );

        let http_auth_state = Arc::new(rsetup_controller::http_auth::HttpAuthState::new(live_auth, security_config));
        let app_state = rsetup_controller::http_auth::AppState {
            db: db.clone(),
            auth: http_auth_state,
        };
        let router = rsetup_controller::build_production_router(app_state);

        let listener = tokio::net::TcpListener::bind(&config.listen_address).await?;
        // 关键：生产必须装配 ConnectInfo 提取器供登录限速器判定客户端对端地址
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await
}
```

- [ ] **Step 4: 运行并确认通过（本轮修订不执行；后续需批准）**

运行命令（未来执行）：
```bash
CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo test --offline --locked -p rsetup-controller prod_router_tests::production_router_mounts_auth_and_verifies_peer_boundaries -- --exact
```
预期输出：`test result: ok. 1 passed; 0 failed`，运行测试数恰好为 1，确认被测函数 `build_production_router` 在合并后成功挂载探活路由与认证会话路由，且对端 IP 缺失/存在边界符合安全契约，测试由 RED 变 GREEN。

- [ ] **Step 5: 条件性提交（本轮修订不执行；后续需批准）**

```bash
git add crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/lib.rs crates/rsetup-controller/src/http_live.rs
git commit -m "feat(controller): wire production router with live auth adapter and schema gate"
```

---

### Task 7: 认证会话仓储真实双引擎事务验收计划（受控）

**Files:**
- Create: `crates/rsetup-controller/tests/common/auth_acceptance.rs`
- Modify: `crates/rsetup-controller/tests/common/mod.rs`
- Modify: `crates/rsetup-controller/tests/mysql_identity.rs`
- Modify: `crates/rsetup-controller/tests/tidb_identity.rs`

**Interfaces:**
- Consumes:
  - 操作员明确授权与独占开发库保证
  - 目标数据库隔离确认与真实独立备份（严禁伪造备份引用）
  - 显式环境变量：`CONTROLLER_TEST_DATABASE_URL`（或通过 `TestMigrationConfig::from_test_env` 要求的各项门禁变量，绝不硬编码敏感地址与凭据）
  - `SqlxIdentityRepository` ([auth/sqlx_repo.rs](../../../crates/rsetup-controller/src/auth/sqlx_repo.rs))
  - `IdentityRepository`, `IdentityUser`, `Session` ([auth/service.rs](../../../crates/rsetup-controller/src/auth/service.rs))
- Produces:
  - 真实双引擎事务验收测试套件（全部标注明确说明门禁原因的 `#[ignore]`）：
    - `auth_session_creation_guard_and_user_lock`: 验证验证快照过期的登录写操作被原子拒绝，不创建任何 session。
    - `change_password_atomicity_and_epoch_bump`: 验证改密原子撤销全量 sessions、自增 user revision 与 CAS 递增 `authz_epoch`。
    - `concurrent_last_admin_protection`: 两个并发连接尝试同时停用或降级仅剩的管理员，恰有一方成功，另一方返回 `LastAdmin`，数据库中保留恰好 1 个有效管理员。
    - `revoke_selected_and_other_sessions_isolation`: 验证会话按公开 alias 撤销与“撤销其他”在物理行级锁下的隔离性。

**TDD 模式说明与判定原则：**
- **回归与验收用例：** 对于 `change_password`、`insert_session` 等在现有 `SqlxIdentityRepository` 生产实现中已包含完整锁序与事务逻辑的场景，新增真实双引擎测试属于**验证既有实现与回归**。**严禁在生产或测试桩代码中刻意插入 `panic!("red_stub")` 冒充行为 RED**；此类测试若在具备门禁条件的真实库上初次运行即 PASS，如实记录为验收/回归通过；若因真实 DB 行为差异（如隔离级别、锁等待超时、死锁）失败，则作为真实 BUG 驱动修复。
- **缺失行为 TDD：** 只有在当前实现确实缺失的功能（例如并发最后管理员保护 `patch_user_active_or_admin` 尚未落库或未包含 CAS/锁保护）时，才遵循标准 TDD：先编写断言该未实现行为的失败测试（RED，真实行为断言失败而非人工插桩 panic）→ 编写生产实现（GREEN）→ 验证通过。
- **门禁阻断状态：** 若操作员授权、真实备份凭证或隔离开发库任一前置条件未就绪，相关测试命令不得执行，状态如实标记为 `BLOCKED / 未验证`。

- [ ] **Step 0: 建立双引擎验收辅助模块与数据夹具**

核对真实 Schema 与领域类型：
- 真实 `users` 表结构（`0003_identity_application_integrity.sql`）：包含 `id BINARY(16)`, `username VARCHAR(64)`, `display_name VARCHAR(255)`, `password_hash VARCHAR(255)`, `active BOOLEAN`, `is_admin BOOLEAN`, `must_change_password BOOLEAN`, `revision BIGINT UNSIGNED`, `created_time DATETIME(6)`。
- 真实 `IdentityUser` 结构（`src/auth/service.rs`）：无 `display_name` 与 `created_time` 字段；因此测试夹具插入必须显式提供合规的 `display_name` 与 `created_time`（如 `NOW(6)`），避免 DB 约束失败。

在 `crates/rsetup-controller/tests/common/mod.rs` 中声明 `pub mod auth_acceptance;` 并补充合规的用户夹具插入函数：
```rust
// crates/rsetup-controller/tests/common/mod.rs 增补
pub mod auth_acceptance;

pub async fn insert_fixture_user(
    db: &DbPool,
    user: &rsetup_controller::auth::service::IdentityUser,
    display_name: &str,
) {
    sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash, active, is_admin, must_change_password, revision, created_time) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NOW(6))"
    )
    .bind(&user.id[..])
    .bind(&user.username)
    .bind(display_name)
    .bind(&user.password_hash)
    .bind(user.active)
    .bind(user.is_admin)
    .bind(user.must_change_password)
    .bind(user.revision)
    .execute(&db.0)
    .await
    .unwrap();
}
```

- [ ] **Step 1: 编写双引擎会话改密原子性与事务验收测试**

编写真实断言的验收测试逻辑（在 `crates/rsetup-controller/tests/common/auth_acceptance.rs` 中）：
```rust
// crates/rsetup-controller/tests/common/auth_acceptance.rs
use rsetup_controller::db::DbPool;
use rsetup_controller::auth::sqlx_repo::SqlxIdentityRepository;
use rsetup_controller::auth::service::{IdentityRepository, IdentityUser, Session};
use rsetup_controller::authz::sqlx_repo::SqlxAuthzRepository;
use rsetup_controller::ControllerError;

pub async fn run_auth_change_password_atomicity_test(db: &DbPool) {
    let repo = SqlxIdentityRepository::new(db.clone());

    // 1. 插入测试用户与 2 个有效会话
    let user_id = *uuid::Uuid::new_v4().as_bytes();
    let epoch = *uuid::Uuid::new_v4().as_bytes();
    let user = IdentityUser {
        id: user_id,
        username: format!("u_{}", hex::encode(&user_id[..4])),
        password_hash: "hash_old".into(),
        active: true,
        is_admin: false,
        must_change_password: false,
        revision: 1,
    };
    super::insert_fixture_user(db, &user, "Auth Acceptance User").await;

    let sess_1 = [0x11u8; 32];
    let sess_2 = [0x22u8; 32];
    repo.insert_session(&user, sess_1, epoch).await.expect("insert_session 1 failed");
    repo.insert_session(&user, sess_2, epoch).await.expect("insert_session 2 failed");

    let session_obj = Session {
        user: user.clone(),
        digest: sess_1,
    };

    let initial_epoch: u64 = sqlx::query_scalar("SELECT authz_epoch FROM schema_meta WHERE singleton = 1")
        .fetch_one(&db.0)
        .await
        .expect("read initial authz_epoch failed");

    // 2. 执行改密（生产事务逻辑：锁 guard -> 锁 user -> 锁 session -> update user -> 撤销全量 sessions -> CAS 递增 authz_epoch -> audit）
    repo.change_password(&session_obj, epoch, "hash_new")
        .await
        .expect("change_password failed");

    // 3. 断言该用户所有会话均被标记为 revoked
    let revoked_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sessions WHERE user_id = ? AND revoked = 1"
    )
    .bind(&user_id[..])
    .fetch_one(&db.0)
    .await
    .expect("count revoked sessions failed");
    assert_eq!(revoked_count, 2, "all user sessions must be revoked upon password change");

    // 4. 断言 authz_epoch 恰自增 1
    let next_epoch: u64 = sqlx::query_scalar("SELECT authz_epoch FROM schema_meta WHERE singleton = 1")
        .fetch_one(&db.0)
        .await
        .expect("read next authz_epoch failed");
    assert_eq!(next_epoch, initial_epoch + 1, "authz_epoch must be incremented by exactly 1");

    // 5. 故障注入/冲突验证：使用过期的旧 revision 尝试再次改密，预期必须以 RevisionConflict 失败且不触动任何状态
    let stale_session = Session {
        user: user.clone(), // revision 仍为 1，而库中已升至 2
        digest: sess_1,
    };
    let conflict_err = repo.change_password(&stale_session, epoch, "hash_stale")
        .await
        .expect_err("stale change_password must fail");
    assert!(
        matches!(conflict_err, rsetup_controller::ControllerError::RevisionConflict),
        "expected RevisionConflict on stale revision, got {conflict_err:?}"
    );
}

pub async fn run_concurrent_last_admin_protection_test(db: &DbPool) {
    // 起始恰有两位有效管理员：并发停用不同账号，最终只能成功停用其中一位。
    // 夹具必须在已授权的独占 v3 开发库中准备，且不包含其他 active admin。
    let make_admin = |id: [u8; 16]| IdentityUser {
        id,
        username: format!("adm_{}", hex::encode(&id[..4])),
        password_hash: "synthetic-hash".into(),
        active: true,
        is_admin: true,
        must_change_password: false,
        revision: 1,
    };
    let a = make_admin(*uuid::Uuid::new_v4().as_bytes());
    let b = make_admin(*uuid::Uuid::new_v4().as_bytes());
    super::insert_fixture_user(db, &a, "Admin A").await;
    super::insert_fixture_user(db, &b, "Admin B").await;
    let active_admins: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE is_admin = 1 AND active = 1"
    ).fetch_one(&db.0).await.unwrap();
    assert_eq!(active_admins, 2, "test requires exactly two active admins");

    // Task 2 产出的生产仓储方法必须在同一 guard 下重验管理员数量。
    let repo = SqlxAuthzRepository::new(db.clone());
    // 两位各自用有效管理员会话停用自己：无论谁先提交，后者仍是有效 actor，
    // 只能因成为最后一名管理员而返回 LastAdmin；若互相停用，后者反而可能
    // 因 actor 已被撤权返回 PermissionDenied，不能证明最后管理员保护。
    let sess_a = Session { user: a.clone(), digest: [0x11u8; 32] };
    let sess_b = Session { user: b.clone(), digest: [0x22u8; 32] };
    let (left, right) = tokio::join!(
        repo.patch_user_active_or_admin(&sess_a, a.id, 1, Some(false), None, None),
        repo.patch_user_active_or_admin(&sess_b, b.id, 1, Some(false), None, None),
    );
    assert_ne!(left.is_ok(), right.is_ok(), "exactly one demotion must commit");
    let rejected = if left.is_err() { left } else { right };
    assert!(matches!(rejected, Err(ControllerError::LastAdmin)));
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE is_admin = 1 AND active = 1"
    ).fetch_one(&db.0).await.unwrap();
    assert_eq!(remaining, 1);
}
```

并在 `tests/mysql_identity.rs` 与 `tests/tidb_identity.rs` 挂接：
```rust
#[tokio::test]
#[ignore = "one independently backed-up isolated disposable dev DB; never run without operator confirmation and backup proof"]
async fn mysql_auth_change_password_atomicity() {
    let db = common::required_test_db().await;
    common::auth_acceptance::run_auth_change_password_atomicity_test(&db).await;
}

#[tokio::test]
#[ignore = "one independently backed-up isolated disposable dev DB; never run without operator confirmation and backup proof"]
async fn mysql_concurrent_last_admin_protection() {
    let db = common::required_test_db().await;
    common::auth_acceptance::run_concurrent_last_admin_protection_test(&db).await;
}
```

- [ ] **Step 2: 受控执行准备与门禁检查（本轮修订不执行；后续需批准）**

执行原则与安全防护：
1. 严禁在未获操作员显式授权、缺少真实备份凭证或非隔离丢弃库的环境中运行。
2. 避免无意义的自赋值命令（如 `VAR="$VAR"`）。运行命令须使用已配置环境变量并在执行前确认。
3. 严禁在测试日志、终端回显或版本控制中输出真实连接凭据或连接串明文。

受控执行命令示例（未来执行：当且仅当操作员批准并配置环境后由操作员或受权会话按需发起）：
```bash
# 须确保环境变量在进程内安全载入，不开启 shell tracing (set -x)
cargo test -p rsetup-controller --test mysql_identity mysql_auth_change_password_atomicity -- --ignored --exact --test-threads=1
```

- [ ] **Step 3: 结果判定与记录**

- **既有功能验收断言：** 若用例在真实库执行且断言全部通过，记录为：
  `PASS (Regression/Acceptance): change_password atomicity and epoch bump verified on live target.`
- **真实行为缺陷发现：** 若用例因真实 DB 死锁/锁等待/隔离机制产生非预期报错，记入缺陷跟踪并按 TDD 修复实现。
- **环境门禁未就绪：** 若缺少独立备份、操作员确认或测试库变量，严禁执行并记录为：
  `BLOCKED: Database credentials or operator backup proof not available. Kept in ignored status.`

- [ ] **Step 4: 条件性归档记录（本轮修订不执行；后续需批准）**

在所有修改确认审查通过后，由上级代理按受控流程合并，当前实施阶段禁止直接 git commit。

---

### Task 8: Observer 锁观察受控验证路线（BLOCKED 状态；接线待专门审阅）

**Files:**
- Reference: `crates/rsetup-controller/tests/observer_contract.rs`
- Reference: `crates/rsetup-controller/tests/support/observer/mod.rs`
- Reference: `crates/rsetup-controller/tests/common/mod.rs` (Task 4 并发锁等待契约)
- Target: `crates/rsetup-controller/tests/mysql_identity.rs`
- Target: `crates/rsetup-controller/tests/tidb_identity.rs`

**现状事实与接线缺口分析：**
- **旧 wrapper 未接线事实：** 现有 `tests/mysql_identity.rs` 与 `tests/tidb_identity.rs` 中的四个旧用例：
  - `admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_mysql`
  - `admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_mysql`
  - `admission_deactivation_holds_guard_cas_waits_then_denied_on_fresh_v3_tidb`
  - `admission_cas_commits_before_deactivation_then_new_cas_denied_on_fresh_v3_tidb`
  其实际实现位于 `tests/common/mod.rs`（调用 `task4_admission_deactivation_holds_guard_cas_waits_then_denied` 等）。其内部调用 `task4_participant_pool`，目前**仅建立了两个参与连接 A 与 B，并由连接 A 自身进行锁等待轮询（writer A 自观察），且连接池硬编码校验 MySQL 8.0.46 与 TiDB 8.0.11-TiDB-v8.5.8**；这些用例**完全没有消费独立 Observer 连接 O**。
- **验证路线判定：** 直接执行上述旧 wrapper 用例**不能**作为独立 Observer (O) 的验收证明。当前测试代码接线尚未就绪，因此 Task 8 在本计划中明确标记为 **BLOCKED**。
- **约束边界：** 不恢复已暂停的复杂 observer 工具设计，不自动扩权（严禁申请超越最小授权的业务表权限），保留操作员书面授权、独立只读诊断账号、真实备份证明与引擎版本门禁。未来所需的最小 Observer 接线（将 O 连接注入锁等待观察循环、替代 A 自观察）需另行单独安全审阅。

**最小权限契约与门禁保留（未来路线）：**
- **MySQL Observer 账号**：仅限 `GRANT SELECT ON performance_schema.data_lock_waits`, `GRANT SELECT ON performance_schema.data_locks`, `GRANT SELECT ON performance_schema.threads`，可选 `GRANT USAGE ON *.*`。严格禁止任何业务库读写权限或管理权限。
- **TiDB Observer 账号**：仅限 `GRANT PROCESS ON *.*`，可选 `GRANT USAGE ON *.*`。严格禁止业务库读写或全局 SELECT。
- **连接约束**：必须连接时不带默认数据库（`get_database() == None`），严格禁止对业务数据表进行扫描。
- **门禁凭据与信封核对**：操作员时间窗口信封（Envelope）核验、双 PIN（Writer PIN 与 Observer PIN）不匹配保护、单次会话拓扑证明（无跨集群路由）。

**受控路线与状态说明：**

- [ ] **Step 0: 门禁与真实前置条件检查（Preflight Checks）**

在考虑执行真实 Observer 锁观察测试前，必须完成以下硬性前置核查：
1. **操作员明确授权：** 操作员确认当前时间窗口、执行目标与受限诊断账号。
2. **只读诊断账号独立性：** Observer 账号与 Writer 账号绝对分离（不同用户名、不同 PIN），且 Observer 仅具备上述最小授权。
3. **真实备份与环境隔离：** 目标库为获批的独立可丢弃开发环境，具备有效的真实物理备份引用。
4. **有效传输策略：** 数据库连接传输安全策略明确，禁止在不可信网络上回退明文凭据。

*当前判定：* 鉴于既有用例接线尚未接通独立 Observer 连接 O，且缺乏现场真实授权与备份环境，**本用例路线处于 BLOCKED 状态，禁止假借直接执行旧测试声称完成 Observer 验收**。

- [ ] **Step 1: 接线就绪度核对与保持阻断**

核对当前 `tests/common/mod.rs` 的 `task4_participant_pool` 与 `task4_wait_edge`：确认其仍为参与者 A 自观察，未接收 O。保持既有用例上的 `#[ignore]` 标注，在没有经审查的最小接线补丁前，不触发任何真实 DB 验收命令。

- [ ] **Step 2: 结果状态记录**

如实记录当前状态为：
`BLOCKED: Existing task4 tests in common/mod.rs remain writer A self-observation without consuming Observer O. Wiring not ready; direct cargo test of legacy wrappers does not constitute Observer acceptance. Kept in ignored/blocked status pending reviewed minimal wiring.`

---

## 验收矩阵映射与阻断追踪

| 验收编号 | 规格来源 | 对应任务 | 验证目标与判定条件 |
| :--- | :--- | :--- | :--- |
| **AT-01 / AUTH-01** | [01 §8](../specs/2026-09-23-controller-v1-01-identity-access.md#8-专项验收) | Task 5, 6 | 初始化仅一次；`reset-admin` 仅限本地 root 双 UID=0 与完整交互 TTY，密码仅输出至 `/dev/tty` 一次；未改密用户访问非白名单全部拒绝 403。 |
| **AT-02 / AUTH-02 / ADMIN-01** | [01 §8](../specs/2026-09-23-controller-v1-01-identity-access.md#8-专项验收) | Task 2, 4, 7 | 会话停用/改密/重启即刻失效；CSRF/Host/Origin 与登录限速；并发停用/降级不能删除最后管理员（`LAST_ADMIN` 409）。 |
| **AT-03 / ACL-01..03** | [01 §8](../specs/2026-09-23-controller-v1-01-identity-access.md#8-专项验收) | Task 1, 2, 3, 4 | 动态权限并集、撤权、最小设备识别投影（reboot-only 不暴露状态数据）；不可见与不存在统一 404；无过滤前总数泄露。 |
| **AT-04 / ADM-01** | [02 §8](../specs/2026-09-23-controller-v1-02-data-api.md#8-专项验收) | Task 2, 4 | 批量审批严格按显式目标清单，不可确认后吸收新目标；资源不足不记人工拒绝；reopen/reauthorize 明确提示设备重置需求。 |
| **AT-14 / DB-01** | [02 §8](../specs/2026-09-23-controller-v1-02-data-api.md#8-专项验收) | Task 7, 8 | 真实 MySQL 8.4 LTS 与 TiDB 8.5 LTS 分别验证唯一性、CAS、关联一致性、会话事务与受限 Observer 锁等待观测。 |

---

## 协调摘要与接口冲突预警

1. **`build_router` 与路由器协调统一 (Router Unification & Merge Coordination)：**
   - **现有真实接口基线：**
     - `src/lib.rs` 导出 `build_router(state: DbPool) -> axum::Router`，挂载 `/healthz` 与 `/readyz`。
     - `src/http_auth.rs` 实际签名是 `build_http_router(state: AppState) -> Router`，其中 `AppState` 定义为 `pub struct AppState { pub db: DbPool, pub auth: Arc<HttpAuthState> }`，且**内部已经调用了 `crate::build_router(state.db.clone())` 完成了与 probes 探活路由的合并**，并由 `FromRef<AppState> for Arc<HttpAuthState>` 提取认证状态。
     - 严禁误传 `build_http_router` 接收 `Arc<HttpAuthState>` 或杜撰多余的 `state.auth_state` 字段。
   - **协调与安全合并方案：**
     - Task 6 采用单一被测函数 `build_production_router(state: AppState) -> axum::Router`。
     - 探活与认证路由直接复用 `crate::http_auth::build_http_router(state.clone())`（已产出绑定好状态的 `Router<()>`），**严禁在外部再次手动构建或重复注册 `/healthz`、`/readyz`**，防止路由重复注册 panic 或行为漂移。
     - 管理 Router（Task 4 产出的 `build_management_router`）由 `crates/rsetup-controller/src/api` 负责定义各管理子路由（如 `devices_router`、`users_router` 等），在其导出入口绑定 `Arc<AppState>` 后统一作为 `Router<()>` 暴露；路由路径挂载于 `/api/v1/users`、`/api/v1/devices` 等互斥前缀，与 `/api/v1/auth/*` 及 `/healthz`、`/readyz` 完全正交。
     - `build_production_router` 通过 `auth_router.merge(management_router)` 安全组合两个无状态 `Router<()>`，杜绝状态冲突与路由重复。
   - **职责分离原则：**
     - Task 6 的 `main.rs` 负责启动前的全套安全前置检查（配置、schema v3、bootstrap）与生产 `LiveAuth` 适配器接线。
     - 单元测试与 TDD RED/GREEN 通过纯内存/进程内依赖注入桩（`TestAuthStub` 与 `connect_lazy`）进行，不触碰网络、不连接真实数据库。
     - 真实 MySQL/TiDB 的双引擎事务一致性、并发操作与锁等待观测严格由 Task 7 和 Task 8 的门禁验收测试承担，二者物理隔离，边界清晰。
2. **`ControllerError` 变体扩展与错误映射：**
   - 冲突点：既有 `error.rs` 中缺少 `LastAdmin` 错误变体。
   - 解决方案：在 Task 2 中新增 `ControllerError::LastAdmin`，并在 `http_auth.rs::backend_error` 中显式增加该变体映射至 HTTP 409 `LAST_ADMIN`，避免被默认通配符降级为 503 `NOT_READY`。
3. **`DeviceService` 的仓储解耦与批量准入上限：**
   - 冲突点：当前 `DeviceService` 仅定义了单目标状态跃迁，未暴露档案修改 `patch_profile` 与组成员替换 `replace_group_members`。
   - 解决方案：Task 3 将 `SqlxDeviceRepository` 注入 `DeviceService`，并严格实现单次批量处理至多 1024 台公钥的硬上限约束（超限直接 400 `INVALID_ARGUMENT`）。
4. **`reset-admin` 依赖与命令行解析：**
   - 事实纠正：当前根 `Cargo.lock` 已存在 `clap` 4.5 与 `libc` 0.2，但 `crates/rsetup-controller/Cargo.toml` 尚未直接声明 `libc` 或 `serde`。不能仅因根存在就声称子 crate `--locked` 可用；必须在未来经审查在子 crate 添加 `libc.workspace = true` 与 `serde.workspace = true` 并离线更新检查 `Cargo.lock`（本轮修订不执行修改；后续需批准）。
   - 为保持 root 恢复工具最小攻击面与确定性，CLI 解析明确不引入 `clap`，严格使用标准库 `std::env::args()` 做轻量级子命令匹配，TTY 交互性检查使用标准库 `std::io::IsTerminal` 与持有 `/dev/tty`，UID 检查调用 `libc::getuid` / `libc::geteuid`。
5. **发布阻断维持声明：**
   - 本计划为后续开发及真实 DB 验收提供了严格的 TDD 路径；在传输安全外部专家决议未落地、真实双引擎测试未全部执行前，中控 V1 仍保持未批准生产发布状态。

### Task 11: approvals 与设备准入状态机路由（6 条）

**Files:**
- Modify: `crates/rsetup-controller/src/api/approvals.rs`
- Test: 同文件的 `#[cfg(test)] mod tests`

**契约矩阵：**

| 路径与方法 | 权限 | 输入 | 响应 `data` 形状 | 异常状态 |
| :--- | :--- | :--- | :--- | :--- |
| `GET /api/v1/approvals` | admin | `cursor?`, `limit?`, `decision?` | `{ items: [{ device_id, fingerprint, descriptor, connection_state, decision, revision }], next_cursor: String? }` | 401, 403, 400 |
| `POST /api/v1/approvals/approve` | admin | `{ targets: [{ device_id, expected_revision }], confirm: true }`（1..=1024） | `{ results: [{ device_id, ok: bool, revision?: String, error?: { code } }] }`（逐项结果，HTTP 200） | 400(未 confirm/超限/空), 401, 403 |
| `POST /api/v1/approvals/reject` | admin | `{ targets: [...], reason: String, confirm: true }` | `{ results: [...] }` | 400(reason 非空且 ≤1024 无控制字符), 401, 403 |
| `POST /api/v1/approvals/{device_id}/reopen` | admin | `{ expected_revision, confirm: true }` | `{ decision: "none", requires_device_reset: true }` | 400, 404, 409 `REVISION_CONFLICT` |
| `POST /api/v1/devices/{id}/revoke` | admin | `{ expected_revision, reason: String, confirm: true }` | `{ state: "REVOKED", requires_device_reset: true }` | 400, 404, 409 `REVISION_CONFLICT` |
| `POST /api/v1/devices/{id}/reauthorize` | admin | `{ expected_revision, confirm: true }` | `{ state: "APPROVED", requires_device_reset: true }`（仅限原为 REVOKED） | 400, 404, 409 `REVISION_CONFLICT` |

**Interfaces:**
- Produces: `ApprovalsGateway`（完整定义见Task6）；`ManagementNotificationSink`只消费已commit的typed `AdmissionChangeEvent`，不接受客户端伪造事件；`pub fn approvals_router(ApprovalsApiState) -> Router<()>`。

```rust
// api/approvals.rs
pub trait ManagementNotificationSink: Send + Sync {
    fn after_commit(&self, event: &crate::devices::service::AdmissionChangeEvent)
        -> Result<(), ControllerError>;
}
```

这是与既有同步`AdmissionChangeSink`接线兼容的typed可失败边界，不要求所有sink都异步；future方法仅Gateway/probe读取需要。production bridge在DeviceService提交后调用一次sink，传真实CAS后的snapshot及previous_state；失败不能回滚已经提交的准入决定、不能改变revision/决定为旧值或返回伪“未提交”。需要断线的真实Transport消费者收到提示后重读持久准入真相并核当前代际；桥接只将失败转为既有notification_queued=false，不把false说成实际断线成功，不增加未批HTTP字段。缺真实sink不装配管理生产router。

- [ ] **Step 1: 编写失败测试（批量 approve 逐项结果、confirm 必须为 true、超 1024 拒收 400、revoke 提示 reset 200）**

```rust
// crates/rsetup-controller/src/api/approvals.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_fixture::*;
    use tower::ServiceExt;
    use axum::http::StatusCode;

    // fixture_approvals_state()/fixture_approvals_state_with_mixed_results() 在本区 tests 内定义，
    // 返回 ApprovalsApiState { auth: fixture_auth(fixture_session(true)), gateway/sink: Arc::new(私有桩) }；
    // auth_json_post 为局部写 helper（形状同 Task 8 grants 区，Host/Cookie/CSRF 头口径与共享层一致）。
    // RED 为骨架未挂载路由的 404 行为断言失败，不得是编译错误。

    // GET list cursor 测试的局部密钥注入（与 users 区先例同型，强制约束）：生产 list_approvals
    // 恒只经 management_cursor_secret() 取密钥，单元测试进程内全局 OnceLock 未安装，若直接挂
    // fixture_approvals_state() 返回的生产 approvals_router，GET /api/v1/approvals 恒 503
    // STORAGE_UNAVAILABLE，cursor 阳性用例的 200 断言必然失败。因此 GET list cursor 正负例
    // （含后文「审批页探针与 meta 防线」小节 approvals_list_projects_live_connection_state 与
    // approvals_list_returns_503_when_probe_fails）不得挂生产 router：本区 tests 内定义
    // SignedApprovalsCursorTestState { approvals: ApprovalsApiState, secret_slot: Arc<OnceLock<[u8; 32]>> }
    // 与 test-only wrapper，wrapper 内部调用同一个 list_approvals_with_secret_provider（真实
    // gate/query/codec/gateway 顺序不绕过），provider 经
    // move || crate::api::cursor_secret_of(&slot).map(|secret| *secret) 从本测试局部槽读取；
    // fixture helper signed_approvals_cursor_router(secret: Option<[u8; 32]>) 在本区定义
    // SYNTHETIC_SECRET = [7u8; 32]（与 users 区同值的测试合成 key），Some 时经
    // crate::api::install_cursor_secret_into 装入局部槽、None 时留空作网关前 503 负例；
    // 测试 router 只挂 wrapper 的 GET 路由并 .with_state(state) 归约。上方 5 条 POST 写路由
    // 用例继续用 fixture_approvals_state() 挂生产 router 不变（不经 cursor 密钥）。
    // 红线：不得对全局 MANAGEMENT_CURSOR_SECRET 调用 set，不得给 ApprovalsApiState 扩 secret
    // 字段，不得用 test 专属假 handler 或自签 token 绕过真实链路。

    #[tokio::test]
    async fn approve_without_confirm_is_rejected_with_400() {
        let router = approvals_router(fixture_approvals_state());
        let req = auth_json_post("/api/v1/approvals/approve", serde_json::json!({
            "targets": [{ "device_id": "a".repeat(64), "expected_revision": "1" }],
            "confirm": false,
        }));
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn batch_approve_returns_200_with_per_item_results_even_with_partial_conflict() {
        let router = approvals_router(fixture_approvals_state_with_mixed_results());
        let req = auth_json_post("/api/v1/approvals/approve", serde_json::json!({
            "targets": [
                { "device_id": "a".repeat(64), "expected_revision": "1" },
                { "device_id": "b".repeat(64), "expected_revision": "99" }, // 冲突
            ],
            "confirm": true,
        }));
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        let results = val["data"]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["ok"], true);
        assert_eq!(results[1]["ok"], false);
        assert_eq!(results[1]["error"]["code"], "REVISION_CONFLICT");
    }

    #[tokio::test]
    async fn revoke_indicates_requires_device_reset() {
        let router = approvals_router(fixture_approvals_state());
        let req = auth_json_post(&format!("/api/v1/devices/{}/revoke", "a".repeat(64)), serde_json::json!({
            "expected_revision": "1", "reason": "compromised", "confirm": true,
        }));
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["data"]["requires_device_reset"], true);
        assert_eq!(val["data"]["state"], "REVOKED");
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::approvals::tests
```
预期：3 个测试全部 FAIL（骨架未挂载路由返回 404）。

- [ ] **Step 3: 最小实现**

挂载 6 条路由；`GET /approvals` 传 `EndpointId::ApprovalsRead`（读桶），5 条写路由（POST）只按 `session.digest` 走 `write_limiter`，不进读桶；批量 approve/reject 限制 `1 <= targets.len() <= 1024`，且必须 `confirm == true`；逐项调用底层 `DeviceService` 的单目标状态转换，把单个 CAS 成功/失败收集进 `results` 数组，HTTP 状态码固定 200；`reopen`/`revoke`/`reauthorize` 返回包含 `requires_device_reset: true` 的固定契约。`fingerprint` 格式按计划约定为 `SHA-256(public_key)[..16]` 的小写 hex32 字符串（待审展示格式）。

- **GET /api/v1/approvals cursor 分页真实接线与 decision 规范筛选绑定（本单元范围）**：`list_approvals` 为 approvals 唯一 GET list handler；**唯一注册锚点**为 `approvals_router` 的 `GET /api/v1/approvals` 入口中唯一的 `get(list_approvals)`，不新增第二处注册、不改动 5 条 POST 写路由。本小节即 roles 单元范围边界中「approvals decision filter 为独立后续」的兑现，该边界原文保留不改写。与 devices 相反，本端点为 admin 端点，**必须保留 `require_admin_session` 门禁**。调用顺序：`authenticate_read(EndpointId::ApprovalsRead)` → `require_admin_session`（401/403 先于 query 解析）→ `Query::try_from_uri`（门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400）→ limit 域校验（默认 50、合法域 `1..=200`，`0` 或 `>200` 一律 400）→ `decision` 严格解析（仅接受小写规范字符串 `none`/`approved`/`denied`/`revoked`，映射 `ReviewDecision::{None,Approved,Denied,Revoked}`，与 Task 3 既有 wire 映射同源；其余值——含大小写变体、空串与 `pending`/`rejected` 等非规范词——一律 `InvalidArgument` 400，不做大小写不敏感或前缀匹配）→ `CursorFilterCtx::from_ordered_fields`（filter 唯一构造路径：tag 1 = `decision` 规范字符串字节，schema 恒含该字段，存在 → `[(1, Some(bytes))]`、缺省 → `[(1, None)]`；`limit` 不进 schema、不参与 MAC 绑定）→ 取密钥（即使 `cursor` 为 `None` 也必须先调 `management_cursor_secret()`，未安装 → `Config` → `map_error` 精确 503，不得因 `None` 短路跳过）→ `decode_management_list_cursor`（`EndpointId::ApprovalsRead` → `Device` profile：`CursorKey::DeviceKey(raw32)` 为合法形态；`CursorKey::Uuid` 在本 profile 不可达，仍 fail-closed 400；篡改/跨端点/跨用户/换筛选复用 → `InvalidArgument` 400）→ `state.gateway.list(&actor, decision, cursor, limit)`（同一 actor、同一请求内规范化的 `decision`）→ 逐可见项各调用一次 `state.probe.snapshot(&id).await` 填充 `connection_state`（消费通路同「真实管理读取补全」小节审批连接投影描述：`id` 由该项 `device_id` 按小写 hex64 口径反解为 `[u8;32]`，实现侧复用 `parse_device_id_path` 同款字节级校验但失败按后端形状错误映射 503；反解失败或 probe `Err` → 整页失败经 `map_error` 精确 503 `STORAGE_UNAVAILABLE`，不得静默降级 `offline`、跳过该项或返回 200；items 为空时不调用 probe）→ `sign_management_list_page`（同一 secret/endpoint/actor/同一 ctx 实例，入口与出口必须复用同一请求内同一 `CursorFilterCtx` 实例，否则下一页 token 解不回；下一页请求改写或缺省 `decision` → filter 不一致 → MAC 校验失败 400）→ `ok_response`；配置/后端错误经 `map_error` 映射（client 400、config 503、backend 503），绝不吞 `Err` 或返回未签名页面。

- **严格 query DTO（本模块局部，与 `UsersListQuery`/`GrantsListQuery`/`DevicesListQuery` 同型且相互独立，无悬空 import）**：

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovalsListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) decision: Option<String>,
}
```

- **导入（与 `api/approvals.rs` 顶层既有 import 合并、不重复）**：`axum::extract::{Query, State}`、`axum::http::{HeaderMap, Uri}`、`axum::response::Response`、`serde::Deserialize`、`crate::error::ControllerError`、`crate::http_api::ok_response`、`crate::http_auth::EndpointId`、`crate::http_gate::{authenticate_read, require_admin_session}`（admin 端点两者皆导入，与 devices 的 import 层权限例外相反）、`crate::model::ReviewDecision`、`super::{decode_management_list_cursor, management_cursor_secret, map_error, sign_management_list_page, CursorFilterCtx, CursorKey}`，全部真实使用。
- **handler（inner + 生产 wrapper，与 `list_devices_with_secret_provider` 同型）**：`secret_provider` 为唯一密钥入口，测试注入规则与 users/roles/grants/devices 相同（test-only wrapper 调同一 inner，provider 从测试局部 `OnceLock` 或 `move || Ok(local_key)` 闭包读取，不触碰全局 `MANAGEMENT_CURSOR_SECRET` OnceLock、不给 `ApprovalsApiState` 扩 secret 字段）；生产 `list_approvals` 恒只经 env 安装的 accessor `management_cursor_secret()`：

```rust
const APPROVALS_FILTER_TAG_DECISION: u8 = 1;

async fn list_approvals_with_secret_provider<F>(
    state: &ApprovalsApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: Fn() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::ApprovalsRead).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<ApprovalsListQuery>::try_from_uri(uri) {
        Ok(query) => query,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(limit) if (1..=200).contains(&limit) => limit,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let decision = match query.decision.as_deref() {
        None => None,
        Some("none") => Some(("none", ReviewDecision::None)),
        Some("approved") => Some(("approved", ReviewDecision::Approved)),
        Some("denied") => Some(("denied", ReviewDecision::Denied)),
        Some("revoked") => Some(("revoked", ReviewDecision::Revoked)),
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let filter = match CursorFilterCtx::from_ordered_fields(&[(
        APPROVALS_FILTER_TAG_DECISION,
        decision.map(|(name, _)| name.as_bytes()),
    )]) {
        Ok(filter) => filter,
        Err(error) => return map_error(error),
    };
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(&secret, EndpointId::ApprovalsRead, actor.user.id, &filter, query.cursor.as_deref()) {
        Ok(None) => None,
        Ok(Some(CursorKey::DeviceKey(raw))) => Some(raw),
        Ok(Some(CursorKey::Uuid(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let mut page = match state.gateway.list(&actor, decision.map(|(_, value)| value), cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    let items = match page.get_mut("items").and_then(|items| items.as_array_mut()) {
        Some(items) => items,
        None => return map_error(ControllerError::Config("approvals page shape".into())),
    };
    for item in items.iter_mut() {
        let encoded = match item.get("device_id").and_then(|value| value.as_str()) {
            Some(encoded) => encoded,
            None => return map_error(ControllerError::Config("approvals page item shape".into())),
        };
        let id = match hex::decode(encoded).ok().and_then(|raw| raw.try_into().ok()) {
            Some(id) => id,
            None => return map_error(ControllerError::Config("approvals page device id".into())),
        };
        let view = match state.probe.snapshot(&id).await {
            Ok(view) => view,
            Err(error) => return map_error(error),
        };
        item["connection_state"] = serde_json::json!(view.connection_state);
    }
    match sign_management_list_page(&secret, EndpointId::ApprovalsRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_approvals(
    State(state): State<ApprovalsApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_approvals_with_secret_provider(&state, &headers, &uri, || management_cursor_secret().map(|secret| *secret)).await
}
```

- **分页与 decision 筛选测试（selector 级，沿用 users 组断言口径，仅 Device profile 长度不同：外部 96 hex、内部 raw 64 hex）**：无 key → 精确 503 `STORAGE_UNAVAILABLE` 且 `gateway.list` 计数 0；第一页 `next_cursor` 恰 96 小写 hex（`assert_eq!(token.len(), 96)`，40B payload + 8B tag），且不等于网关内部 raw 64 hex（完整 32 字节设备公钥的 hex）；第二页原样回传 token 并换 `limit=7` 仍 200 且网关收到 `Some(<完整 32 字节设备公钥>)`；篡改 tag 末字节 → 400 且计数不增长；另加 `approvals_list_rejects_unknown_decision`（`?decision=pending` → 400 `INVALID_ARGUMENT` 且计数 0）与 `approvals_list_rejects_filter_swap`（第二页 token 复用改写 `decision` → 400）。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::approvals::tests
```
预期：PASS（3 个测试通过）。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/api/approvals.rs
git commit -m "feat(controller): approvals 与设备准入状态机路由（6 条端点）"
```

---

### Task 12: audit 与 system/status 路由（2 条）

**Files:**
- Modify: `crates/rsetup-controller/src/api/audit.rs`
- Modify: `crates/rsetup-controller/src/api/system.rs`
- Test: 两文件的 `#[cfg(test)] mod tests`

**契约矩阵：**

| 路径与方法 | 权限 | 输入 | 响应 `data` 形状 | 异常状态 |
| :--- | :--- | :--- | :--- | :--- |
| `GET /api/v1/audit` | admin | query `actor_id?`, `device_id?`, `cursor?`, `limit?` | `{ items: [AuditEvent], next_cursor: String? }`（脱敏，禁止任何凭据/token） | 401, 403, 400 |
| `GET /api/v1/system/status` | admin | 无 | `{ ready: bool, schema_version: i32, version: String, recovery_mode: String, counts: { users, devices, roles, grants, groups } }` | 401, 403, 503 `NOT_READY`（探针失败） |

**Interfaces:**
- Produces: `AuditGateway`、`SystemGateway`（完整dyn-safe定义见Task6）；`pub fn audit_router(AuditApiState) -> Router<()>`；`pub fn system_router(SystemApiState) -> Router<()>`。恢复probe是Runtime真实控制器的可失败读取，不是静态字符串默认值：

```rust
// api/system.rs
#[derive(Clone, Copy, Debug)]
pub struct RecoveryView { pub ready: bool, pub mode: &'static str }
pub trait RecoveryModeProbe: Send + Sync {
    fn current_mode<'a>(&'a self) -> super::ManagementFuture<'a, RecoveryView>;
}
```

返回RecoveryView的ready和mode必须来自Runtime当前启动/DB/恢复控制器真实快照，不能把恢复模式等同调度放行；未接线时不构造probe、不挂载router。Runtime不可读返回固定去敏Config，handler映射503 NOT_READY，不成功返回常量normal。

- [ ] **Step 1: 编写失败测试（审计脱敏断言、系统状态结构断言）**

两文件测试均置于各自 `#[cfg(test)] mod tests`，头部 `use super::*; use crate::api::test_fixture::*; use tower::ServiceExt; use axum::http::StatusCode;`。`fixture_audit_state()`/`fixture_system_state()` 在本区 tests 内定义（`auth: fixture_auth(fixture_session(true))` + 私有网关桩：audit 桩 `items[0]` 含 `params_redacted`，system 桩返回固定计数）。`authenticated_get`/`body_json` 取自共享前置层（签名返回 `Request`，测试侧 `router.oneshot`），Host/Cookie/CSRF 口径沿用共享层。RED 为骨架未挂载路由的 404 行为断言失败，不得是编译错误。

```rust
// crates/rsetup-controller/src/api/audit.rs
#[tokio::test]
async fn audit_endpoint_returns_redacted_events_never_tokens() {
    let router = audit_router(fixture_audit_state());
    let req = authenticated_get("/api/v1/audit");
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = body_json(res).await;
    let text = val.to_string();
    assert!(!text.contains("password_hash"));
    assert!(!text.contains("token_hash"));
    assert_eq!(val["data"]["items"][0]["params_redacted"], "{}");
}
```

```rust
// crates/rsetup-controller/src/api/system.rs
#[tokio::test]
async fn system_status_returns_counts_and_recovery_mode() {
    let router = system_router(fixture_system_state());
    let req = authenticated_get("/api/v1/system/status");
    let res = router.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let val: serde_json::Value = body_json(res).await;
    assert_eq!(val["data"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(val["data"]["recovery_mode"], "normal");
    assert_eq!(val["data"]["counts"]["users"], "1");
}
```

- [ ] **Step 2: 运行并确认行为 RED**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::audit::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::system::tests
```
预期：各 1 个测试 FAIL（骨架未挂载路由返回 404）。

- [ ] **Step 3: 最小实现**

挂载两路由；`GET /audit` 传 `EndpointId::AuditRead`、`GET /system/status` 传 `EndpointId::SystemStatusRead`（各读桶独立，50/s 按 `(variant, session.digest)`）；`GET /api/v1/system/status` 通过 `state.gateway.status(&actor)` 获取真实聚合（schema_version 来自 `schema_meta`，版本来自 `env!("CARGO_PKG_VERSION")`，各计数来自 `COUNT(*)`，`recovery_mode` 来自 `RecoveryModeProbe`）；若探针报错返回 503 `NOT_READY`。

- [ ] **Step 4: 运行并确认 GREEN**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::audit::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller api::system::tests
```
预期：PASS。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/api/audit.rs crates/rsetup-controller/src/api/system.rs
git commit -m "feat(controller): audit 与 system/status 路由（2 条端点）"
```

---

### 真实管理读取补全：过滤设备/审批/来源分页

下列实现属于Task3/4/5必要代码，补齐Task13生产adapter消费接口。只新增内部方法，不新增路由、权限点或HTTP响应字段。SQL固定模板且全部绑定；search用LOCATE文字子串避免LIKE通配误扩展，group_id按当前设备成员关系真实过滤。所有页在同连接RR tx内当前actor/引用/meta校验，实际额外可见项才给next，不借COUNT泄漏。

```rust
// devices/repository.rs；新增const，controller Gate输入limit1..=200。
const DEVICE_FILTER_KEYS_SQL: &str = "SELECT d.public_key FROM devices d WHERE (? IS NULL OR d.public_key > ?) AND (? IS NULL OR LOCATE(?, d.display_name) > 0) AND (? IS NULL OR EXISTS (SELECT 1 FROM group_members gm JOIN device_groups dg ON dg.id=gm.group_id WHERE gm.device_id=d.public_key AND gm.group_id=? AND dg.archived=FALSE)) ORDER BY d.public_key LIMIT ?";
```

完整trait新增：`device_page_filtered<'a>(&'a self, actor:&'a Session,cursor:Option<[u8;32]>,limit:usize,group_id:Option<Uuid>,search:Option<&'a str>)->RepoFuture<'a,(Vec<(DeviceAdminView,HashSet<Permission>)>,Option<[u8;32]>)>`；基础device_page委托filtered(actor,cursor,limit,None,None)，仅保留一套扫描实现。将上文完整device_page方法改名device_page_filtered并加两个参数，循环内raw_keys match after替换为：

```rust
let raw_keys = sqlx::query_scalar::<_,Vec<u8>>(DEVICE_FILTER_KEYS_SQL)
    .bind(after.map(|key|key.to_vec())).bind(after.map(|key|key.to_vec()))
    .bind(search).bind(search)
    .bind(group_id.map(|id|id.as_bytes().to_vec())).bind(group_id.map(|id|id.as_bytes().to_vec()))
    .bind(scan_limit).fetch_all(&mut *tx).await?;
```

过滤应在请求层规范化：search缺省None，非空1..=128 Unicode字符/≤512B、无control，否则400；不得默默truncate/忽略group_id/search。group范围父行missing仅该过滤无成员结果，触及persisted member缺父行仍503。新增SQL绑定测试精确7，不自称source text证明真实过滤。

```rust
// DeviceRepository新增，SqlxDeviceRepository实现内。
fn approval_page<'a>(&'a self,actor:&'a Session,decision:Option<crate::model::ReviewDecision>,cursor:Option<[u8;32]>,limit:usize)->RepoFuture<'a,serde_json::Value> {
    Box::pin(async move {
        if !(1..=200).contains(&limit){return Err(ControllerError::InvalidArgument);}
        let mut connection=crate::authz::sqlx_repo::acquire_authorization_read_connection(&self.db).await?;
        let mut tx=sqlx::Connection::begin(&mut *connection).await?;
        crate::authz::sqlx_repo::verify_management_read(&mut tx,actor).await?;
        let decision=decision.map(|value|match value{crate::model::ReviewDecision::None=>"none",crate::model::ReviewDecision::Approved=>"approved",crate::model::ReviewDecision::Denied=>"denied",crate::model::ReviewDecision::Revoked=>"revoked"});
        let rows=sqlx::query("SELECT public_key,display_name,descriptor_json,admission_state,review_decision,revision,CAST(archived AS SIGNED) AS archived FROM devices WHERE (? IS NULL OR public_key > ?) AND (? IS NULL OR review_decision = ?) ORDER BY public_key LIMIT ?")
            .bind(cursor.map(|key|key.to_vec())).bind(cursor.map(|key|key.to_vec()))
            .bind(decision).bind(decision).bind(u64::try_from(limit+1).map_err(|_|ControllerError::InvalidArgument)?)
            .fetch_all(&mut *tx).await?;
        let mut items=Vec::new();let mut last=None;let has_next=rows.len()>limit;
        for row in rows.iter().take(limit) {
            let view=crate::authz::sqlx_repo::decode_device_admin_view(row.try_get("public_key")?,row.try_get("display_name")?,row.try_get("descriptor_json")?,row.try_get("admission_state")?,row.try_get("review_decision")?,row.try_get("revision")?,row.try_get("archived")?)?;
            last=Some(view.public_key);
            let decision=match view.review_decision{crate::model::ReviewDecision::None=>"none",crate::model::ReviewDecision::Approved=>"approved",crate::model::ReviewDecision::Denied=>"denied",crate::model::ReviewDecision::Revoked=>"revoked"};
            items.push(serde_json::json!({"device_id":hex::encode(view.public_key),"fingerprint":hex::encode(view.public_key),"descriptor":view.descriptor,"decision":decision,"revision":view.revision.to_string()}));
        }
        tx.commit().await?;
        Ok(serde_json::json!({"items":items,"next_cursor":if has_next{last.map(hex::encode)}else{None}}))
    })
}
```

审批页 meta 防线（同 lease 同事务）：`approval_page` 的前置从裸 `ensure_actor_admin_readonly` 升级为既有 helper `crate::authz::sqlx_repo::verify_management_read(&mut tx,actor)`（本计划已把该 helper 提升为 `pub(crate)` 供 devices 侧 admin 审批页复用），其内部在同一 RR lease、同一 tx 内执行 `SNAPSHOT_META_SQL` 全读（无 WHERE、`LIMIT 2`）+ `decode_snapshot_meta(&raw, crate::integrity::SUPPORTED_SCHEMA_VERSION)`，与普通 snapshot 的 `load_snapshot_in_tx` 共用同一 schema_meta 单例+版本判定，不新增第二套 meta SQL 或第二套期望版本常量。`schema_version` 不等于编译期支持版本（wrong meta）与出现额外 singleton 行（extra meta）都由 `decode_snapshot_meta` 返回固定去敏 `read_error` `Config`，HTTP 精确 503 `STORAGE_UNAVAILABLE`，不得退化为 400/404、不得被 `LIMIT 2` 静默吞掉。普通 `device_page` 仍严禁借用该 admin 读 helper。

审批连接投影在handler逐可见项读取同次真实DeviceLiveProbe，错误503；不要把connection_state常量写入repo或假从描述推在线。消费通路：`GET /api/v1/approvals` 的 items 契约要求每项含 `connection_state`，仓储页只返回持久真相，handler 在 `state.gateway.list(...)` 成功后逐可见项各调用一次 `state.probe.snapshot(&id).await`（`id` 由该项 `device_id` 的 hex 反解为 `[u8;32]`），把 `DeviceLiveView.connection_state` 以 snake_case 写入该项，值域与设备详情端点共用同一 `DeviceConnectionState`。probe 返回 `Err` 或 `device_id` 反解失败 → 整页失败并经 `map_error` 统一映射 503 `STORAGE_UNAVAILABLE`（与设备详情 probe 错误同源同码），不得静默降级为 `offline` 常量、跳过该项或返回 200；items 为空时不调用 probe。此Page内部next为raw key，HTTP层签名后才发送。fingerprint目前仅公钥可辨标识，若既有批准 fingerprint 算法是SHA256而非hex公钥，统一复用已有实现；不得凭此引入新协议身份。

- [ ] **审批页探针与 meta 防线用例（selector 级）：** `api::approvals::tests::approvals_list_projects_live_connection_state`（fixture probe 返回 `Online`，断言 `data.items[0].connection_state == "online"` 且 probe 调用次数等于可见项数）；`api::approvals::tests::approvals_list_returns_503_when_probe_fails`（probe 返回 `Err` → 503 `STORAGE_UNAVAILABLE`，响应体既不含 `connection_state` 也不含 `"offline"`）；真库门后（沿用 Task14 既有受控门禁与逐引擎串行，不改其区段）`approval_page_rejects_wrong_schema_meta`（`schema_meta.schema_version` 改为非支持版本 → 精确 503 `STORAGE_UNAVAILABLE`）与 `approval_page_rejects_extra_meta_row`（额外 singleton 行 → 精确 503）；四条均先以常量 offline 投影/裸 `ensure_actor_admin_readonly` 前置跑目标 RED，再以上实现 GREEN，不用常量 fake 替代真实 probe 与 SQL。

```rust
// service.rs；来源Page只用一次protected同连接RR tx，不逐设备另开service事务。
pub async fn effective_permissions_page(&self,caller:&Session,target:[u8;16],device_id:Option<[u8;32]>,cursor:Option<[u8;32]>,limit:usize)->Result<serde_json::Value,ControllerError> {
    if !(1..=200).contains(&limit){return Err(ControllerError::InvalidArgument);}
    self.snapshots.load_admin_target_page(caller,target,device_id,cursor,limit).await
}
```

AuthorizationSnapshotRepository新增方法的可编译默认如下，Sqlx实现必须覆盖；未调用该方法的旧fixture可使用失败关闭默认，不产生成功空页：

```rust
fn load_admin_target_page<'a>(&'a self,_caller:&'a crate::auth::service::Session,_target:[u8;16],_device_id:Option<[u8;32]>,_cursor:Option<[u8;32]>,_limit:usize)->RepoFuture<'a,serde_json::Value> {
    Box::pin(async{Err(ControllerError::PermissionDenied)})
}
```

Sqlx实现如下：

```rust
fn load_admin_target_page<'a>(&'a self,caller:&'a Session,target:[u8;16],device_id:Option<[u8;32]>,cursor:Option<[u8;32]>,limit:usize)->RepoFuture<'a,serde_json::Value> {
    Box::pin(async move {
        let mut connection=acquire_authorization_read_connection(&self.db).await?;
        let mut tx=sqlx::Connection::begin(&mut *connection).await?;
        ensure_actor_admin_readonly(&mut tx,caller).await?;
        let exists=sqlx::query_scalar::<_,Vec<u8>>("SELECT id FROM users WHERE id=?")
            .bind(target.as_slice()).fetch_optional(&mut *tx).await.map_err(|_|read_error("snapshot.actor"))?;
        if exists.is_none(){return Err(ControllerError::NotFound);}
        let keys:Vec<[u8;32]>=match device_id {
            Some(id)=>vec![id],
            None=>sqlx::query_scalar::<_,Vec<u8>>("SELECT public_key FROM devices WHERE archived=FALSE AND (? IS NULL OR public_key > ?) ORDER BY public_key LIMIT ?")
                .bind(cursor.map(|key|key.to_vec())).bind(cursor.map(|key|key.to_vec())).bind(u64::try_from(limit+1).map_err(|_|ControllerError::InvalidArgument)?)
                .fetch_all(&mut *tx).await?.into_iter().map(|raw|raw.try_into().map_err(|_|read_error("snapshot.device_id"))).collect::<Result<Vec<_>,_>>()?,
        };
        let snapshot=load_snapshot_in_tx(&mut tx,target,&keys).await?;
        if let Some(id)=device_id {
            if !snapshot.devices.get(&id).is_some_and(|view|!view.archived){return Err(ControllerError::NotFound);}
        }
        let has_next=device_id.is_none()&&keys.len()>limit;
        let mut items=Vec::new();let mut last=None;
        for key in keys.iter().take(limit) {
            let permissions=crate::authz::service::snapshot_effective_permissions(&snapshot,*key);
            let mut effective:Vec<_>=permissions.iter().map(|permission|permission.as_str()).collect();effective.sort_unstable();
            let sources=crate::authz::service::snapshot_permission_sources(&snapshot,*key).into_iter().map(|entry|{
                let source=match entry.source{crate::authz::policy::PermissionSourceKind::Admin=>serde_json::json!({"kind":"admin"}),crate::authz::policy::PermissionSourceKind::Role(id)=>serde_json::json!({"kind":"role","role_id":id.to_string()}),crate::authz::policy::PermissionSourceKind::Direct(id)=>serde_json::json!({"kind":"direct","grant_id":id.to_string()})};
                serde_json::json!({"permission":entry.permission.as_str(),"source":source})
            }).collect::<Vec<_>>();
            items.push(serde_json::json!({"device_id":hex::encode(key),"effective_permissions":effective,"sources":sources}));last=Some(*key);
        }
        tx.commit().await?;
        Ok(serde_json::json!({"items":items,"next_cursor":if has_next{last.map(hex::encode)}else{None}}))
    })
}
```

target missing按protected目标用户加载映射NotFound，不误映PermissionDenied；caller行missing仍PermissionDenied。为此load_admin_target_page在load_snapshot前同tx`SELECT id FROM users WHERE id=?`存在性检查，行缺失404，不另开仓储。target inactive/must-change合法存在，来源/权限空，不污染或扩权。

- [ ] **验证：** 纯SQL绑定检查filtered7/approval5/sourcePage3；真实门后seed3设备两组不同名称及不同decision，调用真实filtered页确认筛选不是忽略参数；来源limit=1两页稳定、单device missing404、持久dangling503、target生命周期空集合；每个断言基于实际仓储结果和DB事务，不用常量fake替代SQL。新增行为先以可编译未过滤/错误caller桩跑目标RED，再上述实现GREEN，资源fixture不等双库验收。

- [ ] **成员替换实现补充：** 返回同事务GroupRecord，不能只返回u64再另查。持guard、actor→group→sorted devices锁后从已锁group严格decode name/archived/revision；expected_revision CAS更新group.revision checked+1、固定集合DELETE/INSERT、bump_epoch及ADMIN审计；commit前构造`GroupRecord{id:group_id,name:locked_name,archived:false,revision:next_revision,member_count:u64::try_from(sorted_ids.len())?}`，commit成功返回该record。row解析/计数转换错误固定Config或InvalidArgument，所有traits/service/fixture签名同步为GroupRecord。空集合清空合法、archived/missing父设备拒绝、审计失败回滚。不存在第二次group读取。

### Task 13: 管理生产适配器与26路由网关证据

完整实施内容见[管理适配器与调用矩阵子计划](05-adapters-matrix.md)，作为本执行计划的必要组成；主Task6为唯一State/Gateway/probe契约来源。Identity不修改main或创建顶层builder，唯一顶层装配归集成Task0。

---

### Task 14: 真实双引擎 SQLx 事务验收（受控，`#[ignore]`）

**Files:**
- Create: `crates/rsetup-controller/tests/identity_admin_db.rs`
- Modify: `crates/rsetup-controller/tests/common/mod.rs`（仅追加 `pub(super)` 纯校验 `assert_single_v4_meta_row` 与去敏 v4 fixture 门禁 `required_migrated_v4_db()`：二者同模块、helper 自包含并以 `pub(super)` 对父测试 crate 开放；`identity_admin_db.rs` 的 seed 与纯表测试以显式 `common::assert_single_v4_meta_row` 路径调用，`common` 不新增 `use super::*` 引入父根项，其他真实测试不新增占位函数；`required_migrated_v4_db` 从既有 `required_test_db()` 环境门禁扩展，只加只读核验；不修改现有凭据与目标逻辑及其他 helper）

**硬性安全门禁（Preflight Checklist，缺一即阻断，不得运行任何测试）：**
1. **操作员明确许可与专属开发库：** 仅限 `secret/mysql.json` 与 `secret/tidb.json` 授权的专用开发库。
2. **只读前置校验：** 串行、逐引擎验证 `SELECT DATABASE()`、`schema_version = 3`（或升级后的 4）、无孤儿外键、数据完整性检查通过。
3. **真实备份证明：** 在执行写测试前，操作员对目标库完成物理/逻辑真实备份，核实备份文件 SHA256 与可读性，保留去敏回执；未备份时测试保持 `#[ignore]` 状态。
4. **不执行危险操作：** 不执行 `DROP DATABASE`、不请求 DB root 锁（需 root 权限的锁观察可跳过）、不在日志回显凭据。
5. **本任务 v4 fixture 门禁（测试内强制，只读）：** `last_admin_protection_prevents_deactivation_and_demotion` 使用 `common::required_migrated_v4_db()`：串行、逐引擎只读校验 `schema_meta` 全表恰 1 行且 `(singleton, schema_version) = (1, 4)`（无 WHERE 全读、不隐藏余行、已显式迁移 v4）、全库恰 16 张表（11 identity + 5 runtime）、`information_schema.REFERENTIAL_CONSTRAINTS` 0 行、`information_schema.CHECK_CONSTRAINTS` 0 行、初始业务集合 `users`/`sessions`/`audit_events` 全空；任一不满足即测试显式失败（去敏 panic）。门禁本身只 SELECT：不写、不迁移、不整库删清；`backup_ref`/`MIGRATION_ACK` 取自既有环境门禁，不在测试内伪造。

- [ ] **Step 1: 编写受控真库集成测试（全部标 `#[ignore]`）**

本轮只把 4 个用例中的 `last_admin_protection_prevents_deactivation_and_demotion` 从注释骨架补全为完整可执行代码（含本用例独有的 fixture/回读 helper）；其余 3 个保持注释骨架，Task 14 尚未完成。

```rust
// crates/rsetup-controller/tests/identity_admin_db.rs
// 真实双引擎管理写事务验收：全部用例 `#[ignore]`，仅在 Preflight Checklist 齐备后
// 由操作员按引擎显式串行调用；默认 `cargo test` 不触真库（见 Task 14 Step 2）。
mod common;

use rsetup_controller::auth::service::{IdentityUser, Session};
use rsetup_controller::authz::{AuthzRepository, AuthorizationSnapshotRepository, SqlxAuthzRepository};
use rsetup_controller::ControllerError;
use rsetup_controller::DbPool;
use sqlx::Row;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires operator authorization, dedicated disposable dev DB and verified backup proof"]
async fn admin_write_tx_enforces_lock_order_and_actor_recheck() {
    // 安全隔离 fixture：`required_migrated_v4_db` 只读核验"已迁移 v4 + 初始业务集合空"，
    // 再在持有完整性 guard 的单一事务内只播种本用例新 UUID 行：不 DROP/迁移/整库清理，
    // 不伪造 backup/ACK（backup_ref 与 MIGRATION_ACK 取自既有环境门禁，不在本测试内构造）。
    // B1 闭环：门禁前受控重置（先核验环境门禁目标与表名再表级清空），退出经 `_reset_guard` 复位空态，保证同引擎顺序执行 1→2→3→4 与 RED 通过后再执行 GREEN 均能再次通过空表门禁。
    reset_task4_business_tables().await;
    let db = common::required_migrated_v4_db().await;
    let _reset_guard = Task4ResetGuard;
    let repo = SqlxAuthzRepository::new(db.clone());

    // ── 阶段 0 fixture：活跃管理员 A + B（随后降权为普通用户），状态逐字段受控 ──
    let admin_a = seed_guarded_admin(&db, "fixture_admin_a", None, false).await;
    let user_b = seed_guarded_admin(&db, "fixture_user_b", Some(admin_a), false).await;
    {
        let mut tx = db.0.begin().await.unwrap();
        sqlx::query("UPDATE users SET is_admin = false WHERE id = ?")
            .bind(user_b.to_vec())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // ── 记录初始状态：authz_epoch + 用户 B 的 active/revision（回滚后比对的基线）──
    let epoch_before = read_authz_epoch(&db).await;
    let (b_active_before, b_is_admin_before, _, b_revision_before) = read_user_state(&db, user_b).await;
    assert!(
        (b_active_before, b_is_admin_before, b_revision_before) == (true, false, 1),
        "fixture baseline must be an active regular user at revision 1"
    );

    // ── 阶段 A（并发锁序实证，历史 6a）：事务 T_A 先锁定 actor 行（与 `select_user_for_update`
    // 同款 `SELECT … FOR UPDATE`）持有不提交；事务 T_B 并发走真实 `patch_user` 写路径，按
    // `lock_user_pair_and_verify_actor` 规定顺序（经 `ordered_user_ids` 的 UUID 字节序 first→second；
    // actor == target 时 second 为 None、同一用户仅锁一次，本用例为两个不同行各锁一次）加锁 ──
    assert_ne!(admin_a, user_b, "actor and target must be two distinct rows, each locked exactly once");
    let (first, second) = if admin_a < user_b { (admin_a, user_b) } else { (user_b, admin_a) };
    let actor_is_first = first == admin_a;
    let mut tx_a = db.0.begin().await.unwrap();
    sqlx::query("SELECT id FROM users WHERE id = ? FOR UPDATE")
        .bind(admin_a.as_slice())
        .fetch_one(&mut *tx_a)
        .await
        .unwrap();
    let actor_b = admin_actor(admin_a, "fixture_admin_a");
    let handle_b = tokio::spawn(async move {
        repo.patch_user(&actor_b, user_b, b_revision_before, Some(false), None, None).await
    });
    for _ in 0..20 {
        assert!(!handle_b.is_finished(), "patch_user must block while T_A holds the actor row lock");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    // 锁序与 helper 一致：T_B 被阻塞时只可能持有字节序 first 行（T_A 持 second 时）或不持行
    // （T_A 持 first 时），绝不触及 second；若按 target 先加锁，下面探测结果恰好相反
    let (first_held, second_held) = (probe_row_lock_held(&db, first).await, probe_row_lock_held(&db, second).await);
    assert!(first_held, "byte-order first row must already be locked (by T_A or T_B) while T_B is blocked");
    assert_eq!(second_held, !actor_is_first, "blocked T_B must hold exactly the byte-order rows before the one held by T_A");
    // T_A 提交（停用 A）后 T_B 完成：行重验识别 A 已非活跃管理员 → PermissionDenied 并回滚
    sqlx::query("UPDATE users SET active = false WHERE id = ?")
        .bind(admin_a.as_slice())
        .execute(&mut *tx_a)
        .await
        .unwrap();
    tx_a.commit().await.unwrap();
    let err = handle_b.await.unwrap().unwrap_err();
    assert!(
        matches!(err, ControllerError::PermissionDenied),
        "actor recheck must catch deactivated admin A, got: {err:?}"
    );

    // ── 阶段 C：断言 —— B 的 active/revision 保持不变；authz_epoch 未发生变化；PermissionDenied
    // 路径零审计（actor 重验先于任何审计写入失败，无悬挂行，不得硬塞 LastAdmin 审计）──
    let epoch_after = read_authz_epoch(&db).await;
    assert_eq!(epoch_after, epoch_before, "PermissionDenied 回滚绝不递增 authz_epoch");
    let (b_active, b_is_admin, _, b_rev) = read_user_state(&db, user_b).await;
    assert_eq!((b_active, b_is_admin, b_rev), (true, false, b_revision_before), "用户 B 状态与 revision 必须保持原值");
    assert_eq!(read_audit_total(&db).await, 0, "PermissionDenied 路径必须零审计行（无悬挂、不得硬塞 LastAdmin 审计）");
}

#[tokio::test]
#[ignore = "requires operator authorization, dedicated disposable dev DB and verified backup proof"]
async fn last_admin_protection_prevents_deactivation_and_demotion() {
    // 安全隔离 fixture：`required_migrated_v4_db` 只读核验"已迁移 v4 + 初始业务集合空"，
    // 再在持有完整性 guard 的单一事务内只播种本用例新 UUID 行：不 DROP/迁移/整库清理，
    // 不伪造 backup/ACK（backup_ref 与 MIGRATION_ACK 取自既有环境门禁，不在本测试内构造）。
    // B1 闭环：门禁前受控重置（先核验环境门禁目标与表名再表级清空），退出经 `_reset_guard` 复位空态，保证同引擎顺序执行 1→2→3→4 与 RED 通过后再执行 GREEN 均能再次通过空表门禁。
    reset_task4_business_tables().await;
    let db = common::required_migrated_v4_db().await;
    let _reset_guard = Task4ResetGuard;
    let repo = SqlxAuthzRepository::new(db.clone());
    let epoch_before = read_authz_epoch(&db).await;

    // ── 阶段 0 fixture：唯一活跃管理员 admin + 其 1 条未撤销会话（可证 fixture，状态逐字段受控）──
    let admin = seed_guarded_admin(&db, "fixture_admin_primary", None, true).await;
    let actor = admin_actor(admin, "fixture_admin_primary");

    // ── 阶段 A：仅 1 个活跃管理员，停用与降权均被 LastAdmin 拒绝（业务不变 + 拒绝审计恰好对应行）──
    let first = repo
        .patch_user(&actor, admin, 1, Some(false), None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(first, ControllerError::LastAdmin),
        "deactivation of the only active admin must be exactly LastAdmin, got: {first:?}"
    );
    assert_rejected_unchanged(&db, admin, epoch_before, 1, vec![blocked_row(admin, admin)]).await;

    let second = repo
        .patch_user(&actor, admin, 1, None, Some(false), None)
        .await
        .unwrap_err();
    assert!(
        matches!(second, ControllerError::LastAdmin),
        "demotion of the only active admin must be exactly LastAdmin, got: {second:?}"
    );
    assert_rejected_unchanged(
        &db,
        admin,
        epoch_before,
        2,
        vec![blocked_row(admin, admin), blocked_row(admin, admin)],
    )
    .await;

    // ── 阶段 B：可证 fixture 增加第二管理员；真实仓储停用第一个管理员成功 ──
    let admin2 = seed_guarded_admin(&db, "fixture_admin_second", Some(admin), false).await;
    let actor2 = admin_actor(admin2, "fixture_admin_second");
    let patched = repo
        .patch_user(&actor2, admin, 1, Some(false), None, None)
        .await
        .unwrap_or_else(|err| panic!("deactivation with a second admin must succeed, got: {err:?}"));
    assert_eq!(
        (patched.active, patched.is_admin, patched.revision),
        (false, true, 2),
        "successful deactivation must persist revision+1"
    );
    assert_eq!(
        read_user_state(&db, admin).await,
        (false, true, false, 2),
        "successful deactivation must persist the full user row"
    );
    assert_eq!(read_authz_epoch(&db).await, epoch_before + 1, "successful deactivation must bump authz_epoch by exactly 1");
    assert_eq!(read_active_sessions(&db, admin).await, 0, "successful deactivation must revoke the target's active sessions");
    assert_eq!(read_audit_total(&db).await, 4, "successful deactivation must add exactly two success audits, no extra rows");
    assert_eq!(
        read_admin_audit_rows(&db).await,
        vec![
            blocked_row(admin, admin),
            blocked_row(admin, admin),
            revoke_success_row(admin2, admin),
            patch_success_row(admin2, admin),
        ],
        "success path audits must be sessions.revoke + user.patch in commit order, ADMIN-namespace only"
    );

    // ── 阶段 C：仍活跃的第二管理员以 actor 身份停用自身 → LastAdmin（不得与已停用 actor 的 PermissionDenied 混淆）──
    let third = repo
        .patch_user(&actor2, admin2, 1, Some(false), None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(third, ControllerError::LastAdmin),
        "self-deactivation of the last remaining active admin must be exactly LastAdmin (not PermissionDenied), got: {third:?}"
    );
    assert_eq!(
        read_user_state(&db, admin2).await,
        (true, true, false, 1),
        "rejected self-deactivation must leave the second admin's row unchanged"
    );
    assert_eq!(
        read_user_state(&db, admin).await,
        (false, true, false, 2),
        "rejected self-deactivation must leave the first admin's row unchanged"
    );
    assert_eq!(read_authz_epoch(&db).await, epoch_before + 1, "rejected self-deactivation must not bump authz_epoch");
    assert_eq!(read_active_sessions(&db, admin2).await, 0, "no session row may appear for the second admin");
    assert_eq!(read_audit_total(&db).await, 5, "rejected self-deactivation must add exactly one more audit row");
    assert_eq!(
        read_admin_audit_rows(&db).await,
        vec![
            blocked_row(admin, admin),
            blocked_row(admin, admin),
            revoke_success_row(admin2, admin),
            patch_success_row(admin2, admin),
            blocked_row(admin2, admin2),
        ],
        "final rejection audit must target the second admin, ADMIN-namespace only"
    );

    // 已停用的第一个管理员再发起写：恰好 PermissionDenied（两种失败模式必须可区分）；
    // actor 重验先于任何审计写入失败，故审计总数仍为 5（无悬挂行）。
    let stale = repo
        .patch_user(&actor, admin, 2, Some(true), None, None)
        .await
        .unwrap_err();
    assert!(
        matches!(stale, ControllerError::PermissionDenied),
        "a deactivated actor must be exactly PermissionDenied, got: {stale:?}"
    );
    assert_eq!(read_audit_total(&db).await, 5, "a PermissionDenied rejection must not leave a dangling audit row");
}

#[tokio::test]
#[ignore = "requires operator authorization, dedicated disposable dev DB and verified backup proof"]
async fn grant_creation_and_union_evaluation_real_db() {
    // 安全隔离 fixture：`required_migrated_v4_db` 只读核验"已迁移 v4 + 初始业务集合空"，
    // 单一事务播种新 UUID 行：不 DROP/迁移/整库清理。
    // B1 闭环：门禁前受控重置（先核验环境门禁目标与表名再表级清空），退出经 `_reset_guard` 复位空态，保证同引擎顺序执行 1→2→3→4 与 RED 通过后再执行 GREEN 均能再次通过空表门禁。
    reset_task4_business_tables().await;
    let db = common::required_migrated_v4_db().await;
    let _reset_guard = Task4ResetGuard;
    let repo = SqlxAuthzRepository::new(db.clone());

    let admin = seed_guarded_admin(&db, "fixture_admin_grant", None, false).await;
    let actor = admin_actor(admin, "fixture_admin_grant");

    // 播种目标普通用户
    let target_user = seed_guarded_admin(&db, "fixture_grantee", Some(admin), false).await;
    {
        let mut tx = db.0.begin().await.unwrap();
        sqlx::query("UPDATE users SET is_admin = false WHERE id = ?")
            .bind(target_user.to_vec())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // 1. 创建角色并赋予 device.reboot
    let role = repo
        .create_role(&actor, "rebooter_role", &[rsetup_controller::authz::Permission::DeviceReboot])
        .await
        .unwrap();

    // 2. 授权 1：通过角色授予 target_user
    let _ = repo
        .create_grant(
            &actor,
            target_user,
            rsetup_controller::authz::GrantSource::Role(role.id),
            rsetup_controller::authz::GrantScope::All,
        )
        .await
        .unwrap();

    // 3. 授权 2：直接权限授予 target_user (device.status.read)
    let _ = repo
        .create_grant(
            &actor,
            target_user,
            rsetup_controller::authz::GrantSource::Direct(vec![rsetup_controller::authz::Permission::DeviceStatusRead]),
            rsetup_controller::authz::GrantScope::All,
        )
        .await
        .unwrap();

    // 4. 加载快照并验证动态并集：恰好拥有两个权限，未授权权限（如 device.write）不泄露
    let snapshot_repo = rsetup_controller::authz::SqlxAuthorizationSnapshotRepository::new(db.clone());
    // 固定测试设备键：库中无该设备行与持久悬空引用（devices map 无 key 属合法），scope All 的 grant 对任意设备生效
    let key: [u8; 32] = [9; 32];
    let snapshot = snapshot_repo.load_snapshot(target_user, &[key]).await.unwrap();
    let device_groups = snapshot.device_groups.get(&key).cloned().unwrap_or_default();
    let perms = rsetup_controller::authz::policy::eval_memory_permissions(
        &snapshot.actor,
        key,
        &snapshot.grants,
        &snapshot.roles,
        &device_groups,
    );
    assert_eq!(perms.len(), 2, "权限并集必须恰好包含 2 个权限");
    assert!(perms.contains(&rsetup_controller::authz::Permission::DeviceReboot));
    assert!(perms.contains(&rsetup_controller::authz::Permission::DeviceStatusRead));
}

#[tokio::test]
#[ignore = "requires operator authorization, dedicated disposable dev DB and verified backup proof"]
async fn authz_epoch_cas_increments_monotonically_and_admin_audit_epoch_unique() {
    // 安全隔离 fixture：`required_migrated_v4_db` 只读核验"已迁移 v4 + 初始业务集合空"，
    // 再在持有完整性 guard 的单一事务内播种新 UUID 行：不 DROP/迁移/整库清理。
    // B1 闭环：门禁前受控重置（先核验环境门禁目标与表名再表级清空），退出经 `_reset_guard` 复位空态，保证同引擎顺序执行 1→2→3→4 与 RED 通过后再执行 GREEN 均能再次通过空表门禁。
    reset_task4_business_tables().await;
    let db = common::required_migrated_v4_db().await;
    let _reset_guard = Task4ResetGuard;
    let repo = SqlxAuthzRepository::new(db.clone());

    let admin = seed_guarded_admin(&db, "fixture_admin_epoch", None, false).await;
    let actor = admin_actor(admin, "fixture_admin_epoch");

    // 记录基线 epoch
    let epoch_start = read_authz_epoch(&db).await;

    // 1. 管理写 1：create_group
    let group = repo.create_group(&actor, "epoch_test_group").await.unwrap();
    let epoch_after_1 = read_authz_epoch(&db).await;
    assert_eq!(epoch_after_1, epoch_start + 1, "create_group 成功必须递增 authz_epoch");

    // 2. 管理写 2：patch_group
    let _ = repo.patch_group(&actor, group.id, group.revision, Some("epoch_renamed"), None).await.unwrap();
    let epoch_after_2 = read_authz_epoch(&db).await;
    assert_eq!(epoch_after_2, epoch_after_1 + 1, "patch_group 成功必须再次递增 authz_epoch");

    // 3. 管理写 3：create_role
    let role = repo.create_role(&actor, "epoch_role", &[rsetup_controller::authz::Permission::DeviceReboot]).await.unwrap();
    let epoch_after_3 = read_authz_epoch(&db).await;
    assert_eq!(epoch_after_3, epoch_after_2 + 1, "create_role 成功必须递增 authz_epoch");

    // 4. 冲突回滚负例：revision CAS 不匹配导致失败，断言 epoch 绝不递增
    let conflict_res = repo.patch_group(&actor, group.id, 9999, Some("conflict_name"), None).await;
    assert!(matches!(conflict_res, Err(ControllerError::RevisionConflict)));
    let epoch_after_conflict = read_authz_epoch(&db).await;
    assert_eq!(epoch_after_conflict, epoch_after_3, "事务失败回滚绝不递增 authz_epoch");

    // 5. 校验审计行：全部事件属于 ADMIN namespace，(process_epoch, event_seq) 唯一键无冲突且单调
    let audit_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events WHERE target_kind IN ('group', 'role')")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(audit_count, 3, "恰好生成 3 条成功管理写审计记录");

    let rows = sqlx::query("SELECT process_epoch, event_seq FROM audit_events WHERE target_kind IN ('group', 'role') ORDER BY event_seq ASC")
        .fetch_all(&db.0)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    // 验证全部事件归属 ADMIN process epoch，且 event_seq 严格单调递增
    let mut prev_seq: Option<u64> = None;
    for row in &rows {
        let process_epoch: Vec<u8> = row.try_get("process_epoch").unwrap();
        let event_seq: u64 = row.try_get("event_seq").unwrap();
        assert_eq!(
            process_epoch.as_slice(),
            rsetup_controller::audit::admin_process_epoch().as_bytes(),
            "必须归属 ADMIN process epoch"
        );
        if let Some(ps) = prev_seq {
            assert!(event_seq > ps, "event_seq 必须严格单调递增");
        }
        prev_seq = Some(event_seq);
    }
}

// ── v4 singleton 门禁纯表测试（helper 位于 `tests/common/mod.rs` 本任务文末追加块，`pub(super)` 对父测试 crate 开放；
//    本文件经显式 `common::assert_single_v4_meta_row` 路径调用，不依赖单一测试 crate 根私有项）──

#[test]
fn v4_singleton_gate_fails_closed_on_empty_extra_or_wrong_rows() {
    common::assert_single_v4_meta_row(&[(1, 4)]);
    // 空 / 多行 / 错 singleton / 错 version（v3）——必须全部失败；空例写作 `&[]`，
    // 元素类型依声明的 `[&[(i32, i32)]; 4]` 推断（类型名不能作重复表达式的值）。
    let failing: [&[(i32, i32)]; 4] = [
        &[],
        &[(1, 4), (1, 4)],
        &[(2, 4)],
        &[(1, 3)],
    ];
    for rows in failing {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            common::assert_single_v4_meta_row(rows)
        }));
        assert!(caught.is_err(), "v4 singleton gate must fail closed");
    }
}

// ── 本用例独有 fixture helper：只播种本用例新 UUID 行，全部在持有完整性 guard 的单一事务内；
//    失败信息统一去敏（与 common/mod.rs 一致），不暴露 driver 错误、行值或占位哈希 ──

fn fixture_fail() -> ! {
    panic!("task4_fixture_operation_failed")
}

/// Task 14 用例独占受控重置（B1 闭环）：先经既有 `common::required_test_db()` 环境门禁（五项环境变量 +
/// `SELECT DATABASE()` 库名核验，确认目标是获授权、隔离且可丢弃的专用开发库），再核验表名（16 张表），随后对
/// 用例涉及业务表做表级受控清空（仅 `TRUNCATE` 列出表；`schema_meta` 不动，guard 行与 `authz_epoch` 保留，
/// 用例断言均取相对基线）。不 DROP、不删除数据库本身、不迁移，不伪造备份或备份引用（`CONTROLLER_TEST_BACKUP_REF`/
/// `CONTROLLER_TEST_MIGRATION_ACK` 等既有迁移/备份门禁保持在 `required_test_db`/`required_migrated_v4_db` 不变）。
async fn reset_task4_business_tables() {
    let db = common::required_test_db().await;
    let tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()").fetch_one(&db.0).await.unwrap_or_else(|_| fixture_fail());
    assert_eq!(tables, 16, "fixture reset requires the 16-table v4 target database");
    for table in ["users", "sessions", "roles", "role_permissions", "device_groups", "group_members", "grants", "audit_events"] {
        sqlx::query(&format!("TRUNCATE TABLE {table}")).execute(&db.0).await.unwrap_or_else(|_| fixture_fail());
    }
}

/// B1 闭环 guard：用例退出（正常返回或 panic 展开）时再执行一次受控重置复位门禁要求的空态；
/// `#[tokio::test]` 为 current_thread 运行时、运行时内嵌套 `block_on` 会 panic，故重置在独立线程的独立
/// 运行时上执行（内部重走 `required_test_db()` 目标核验）；重置失败不再抛出（避免展开期双重 panic），
/// 下一用例门禁前的重置是权威兜底。
struct Task4ResetGuard;

impl Drop for Task4ResetGuard {
    fn drop(&mut self) {
        let handle = std::thread::spawn(|| {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap_or_else(|_| fixture_fail());
            rt.block_on(reset_task4_business_tables());
        });
        let _ = handle.join();
    }
}

/// 探测用户行当前是否被锁：独立连接以 1 秒 `innodb_lock_wait_timeout`（MySQL 8.4 / TiDB 均支持的会话变量）
/// 尝试 `SELECT … FOR UPDATE`，超时即被锁；返回前恢复默认值，避免会话变量泄漏给池内连接。
async fn probe_row_lock_held(db: &DbPool, user_id: [u8; 16]) -> bool {
    let mut tx = db.0.begin().await.unwrap_or_else(|_| fixture_fail());
    sqlx::query("SET innodb_lock_wait_timeout = 1").execute(&mut *tx).await.unwrap_or_else(|_| fixture_fail());
    let got = sqlx::query("SELECT id FROM users WHERE id = ? FOR UPDATE").bind(user_id.as_slice()).fetch_optional(&mut *tx).await;
    sqlx::query("SET innodb_lock_wait_timeout = DEFAULT").execute(&mut *tx).await.unwrap_or_else(|_| fixture_fail());
    tx.rollback().await.unwrap_or_else(|_| fixture_fail());
    let held = got.is_err();
    if held {
        assert!(got.unwrap_err().to_string().to_lowercase().contains("lock wait timeout"), "task4_fixture_operation_failed");
    }
    held
}

/// 构造真实进程内 actor `Session`（生产类型 `auth::service::Session`）：
/// 写事务内实现会按行重验 actor 当前状态，Session 字段只需携带正确身份。
fn admin_actor(user_id: [u8; 16], username: &str) -> Session {
    Session {
        user: IdentityUser {
            id: user_id,
            username: username.to_owned(),
            password_hash: "fixture-placeholder".to_owned(),
            active: true,
            is_admin: true,
            must_change_password: false,
            revision: 1,
        },
        digest: [0u8; 32],
    }
}

/// 受 guard 保护的播种：单事务内联 `integrity::lock_integrity_guard` 同款锁与校验
/// （该函数 `pub(crate)`，集成测试不可见），可选重验授权管理员行；只 INSERT 本用例
/// 新 UUID 的 user 行（`must_change_password=FALSE` 使其为有效 actor，`revision=1`），
/// `with_session` 控制是否同时播种 1 条未撤销会话行。提交后返回新用户 id。
async fn seed_guarded_admin(
    db: &DbPool,
    username: &str,
    authorizing_admin: Option<[u8; 16]>,
    with_session: bool,
) -> [u8; 16] {
    let mut tx = db.0.begin().await.unwrap_or_else(|_| fixture_fail());
    let guard: Vec<(i32, i32)> = sqlx::query_as(
        "SELECT singleton, schema_version FROM schema_meta ORDER BY singleton FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await
    .unwrap_or_else(|_| fixture_fail());
    common::assert_single_v4_meta_row(&guard);
    if let Some(admin) = authorizing_admin {
        let (active, is_admin, must_change_password): (bool, bool, bool) = sqlx::query_as(
            "SELECT CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password FROM users WHERE id = ? FOR UPDATE",
        )
        .bind(admin.as_slice())
        .fetch_one(&mut *tx)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
        assert!(
            active && is_admin && !must_change_password,
            "fixture authorizing admin must remain an active admin"
        );
    }
    let user_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO users (id, username, display_name, password_hash, active, is_admin, must_change_password, revision, created_time) VALUES (?, ?, ?, 'fixture-placeholder', TRUE, TRUE, FALSE, 1, NOW(6))",
    )
    .bind(user_id.as_bytes().as_slice())
    .bind(username)
    .bind(username)
    .execute(&mut *tx)
    .await
    .unwrap_or_else(|_| fixture_fail());
    if with_session {
        use rand::RngCore;
        let mut token_hash = [0u8; 32];
        let mut session_epoch = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut token_hash);
        rand::rngs::OsRng.fill_bytes(&mut session_epoch);
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, process_epoch, created_time, revoked) VALUES (?, ?, ?, NOW(6), FALSE)",
        )
        .bind(token_hash.as_slice())
        .bind(user_id.as_bytes().as_slice())
        .bind(session_epoch.as_slice())
        .execute(&mut *tx)
        .await
        .unwrap_or_else(|_| fixture_fail());
    }
    tx.commit().await.unwrap_or_else(|_| fixture_fail());
    user_id.into_bytes()
}

// ── 去敏回读 helper：纯 SELECT，不复制业务逻辑（业务只经 `AuthzRepository` 真实方法）──

/// 回读用户行：`(active, is_admin, must_change_password, revision)`。
async fn read_user_state(db: &DbPool, user_id: [u8; 16]) -> (bool, bool, bool, u64) {
    sqlx::query_as("SELECT active, is_admin, must_change_password, revision FROM users WHERE id = ?")
        .bind(user_id.as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| fixture_fail())
}

/// 回读当前 `authz_epoch`（schema_meta singleton 行）。
async fn read_authz_epoch(db: &DbPool) -> u64 {
    sqlx::query_scalar("SELECT authz_epoch FROM schema_meta WHERE singleton = 1")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| fixture_fail())
}

/// 回读该用户未撤销会话行数。
async fn read_active_sessions(db: &DbPool, user_id: [u8; 16]) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = ? AND revoked = FALSE")
        .bind(user_id.as_slice())
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| fixture_fail())
}

/// 回读审计总行数（全表；与 ADMIN namespace 行数相等即证明无其他命名空间行、无悬挂行）。
async fn read_audit_total(db: &DbPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM audit_events")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| fixture_fail())
}

/// 按 event_seq 序回读本测试进程 ADMIN namespace 的审计行：
/// `(actor_user_id hex, event_type, outcome, target_id hex)`。
/// 测试进程即写者进程，`audit::admin_process_epoch()` 与写者使用同一进程级 `OnceLock`，
/// 该查询直接证明本用例全部审计行属于 ADMIN namespace 且顺序与提交顺序一致。
async fn read_admin_audit_rows(db: &DbPool) -> Vec<(String, String, String, String)> {
    let epoch = rsetup_controller::audit::admin_process_epoch();
    let rows: Vec<(Vec<u8>, String, String, Option<String>)> = sqlx::query_as(
        "SELECT actor_user_id, event_type, outcome, target_id FROM audit_events WHERE process_epoch = ? ORDER BY event_seq",
    )
    .bind(epoch.as_bytes().as_slice())
    .fetch_all(&db.0)
    .await
    .unwrap_or_else(|_| fixture_fail());
    rows.into_iter()
        .map(|(actor, event_type, outcome, target)| {
            (
                hex::encode(actor),
                event_type,
                outcome,
                // std 签名 `unwrap_or_else<F: FnOnce() -> T>` 的 `T` 由接收者 `Option<String>` 定为 `String`；
                // 裸 `fixture_fail: fn() -> !` 返回型固定为 `!`，泛型返回约束不因零参自动成立；
                // 包成闭包后闭包体 never 在预期 `String` 上下文中 coerce（与 Result 单参回调 `|_| fixture_fail()` 分开）。
                target.unwrap_or_else(|| fixture_fail()),
            )
        })
        .collect()
}

/// 最后一次拒绝（LastAdmin）后的完整"未变更"回读断言：业务行、epoch、会话、审计总数与
/// ADMIN namespace 审计行序列全部符合预期（用于阶段 A 的两次拒绝）。
async fn assert_rejected_unchanged(
    db: &DbPool,
    admin: [u8; 16],
    epoch_before: u64,
    audit_total: i64,
    expected_rows: Vec<(String, String, String, String)>,
) {
    assert_eq!(
        read_user_state(db, admin).await,
        (true, true, false, 1),
        "rejected write must leave the business row unchanged"
    );
    assert_eq!(read_authz_epoch(db).await, epoch_before, "rejected write must not bump authz_epoch");
    assert_eq!(read_active_sessions(db, admin).await, 1, "rejected write must not revoke sessions");
    assert_eq!(read_audit_total(db).await, audit_total, "rejected write must add exactly the expected audit rows, no dangling rows");
    assert_eq!(read_admin_audit_rows(db).await, expected_rows, "rejection audits must be the exact ADMIN-namespace last_admin_blocked rows in commit order");
}

/// 期望审计行：LastAdmin 拒绝（denied）。
fn blocked_row(actor: [u8; 16], target: [u8; 16]) -> (String, String, String, String) {
    (
        hex::encode(actor),
        "identity.user.last_admin_blocked".to_owned(),
        "denied".to_owned(),
        hex::encode(target),
    )
}

/// 期望审计行：停用会话语撤销成功（success）。
fn revoke_success_row(actor: [u8; 16], target: [u8; 16]) -> (String, String, String, String) {
    (
        hex::encode(actor),
        "identity.sessions.revoke".to_owned(),
        "success".to_owned(),
        hex::encode(target),
    )
}

/// 期望审计行：用户补丁成功（success）。
fn patch_success_row(actor: [u8; 16], target: [u8; 16]) -> (String, String, String, String) {
    (
        hex::encode(actor),
        "identity.user.patch".to_owned(),
        "success".to_owned(),
        hex::encode(target),
    )
}
```

`tests/common/mod.rs` 文末追加（新增两项：`pub(super)` 纯校验 `assert_single_v4_meta_row`——与 `required_migrated_v4_db` 同模块自包含、对父测试 crate 开放，`identity_admin_db.rs` 经显式 `common::assert_single_v4_meta_row` 路径调用，`common` 不新增 `use super::*` 引入所有父根项——与本函数；既有 `fixture_ok` 为私有，本任务不扩大其可见性）：

```rust
// crates/rsetup-controller/tests/common/mod.rs（文末追加）

/// 纯校验：`schema_meta` 全表必须恰 1 行且该行 `(singleton, schema_version) = (1, 4)`；
/// 空表 / 多行 / 错 singleton / 错 version 一律 fail-closed（v4 门禁不得混回 v3 允许的 `(1, 3)`）。
/// `pub(super)` 对父测试 crate 开放：`identity_admin_db.rs` 的 seed 与纯表测试经显式 `common::` 路径调用。
pub(super) fn assert_single_v4_meta_row(rows: &[(i32, i32)]) {
    assert_eq!(
        *rows,
        [(1, 4)],
        "schema_meta must hold exactly one row (singleton, schema_version) = (1, 4)"
    );
}

/// Task 14 v4 fixture 门禁：在既有 `required_test_db()` 环境门禁（`CONTROLLER_TEST_DATABASE_URL` /
/// `CONTROLLER_TEST_ALLOW_DESTRUCTIVE` / `CONTROLLER_TEST_EXPECTED_DATABASE` /
/// `CONTROLLER_TEST_BACKUP_REF` / `CONTROLLER_TEST_MIGRATION_ACK` 五项，备份引用与 ACK
/// 由操作员从真实备份提供）之上只做只读核验——`schema_meta` 全表恰 1 行且
/// `(singleton, schema_version) = (1, 4)`（无 WHERE 全读、不隐藏余行、已显式迁移 v4）、全库恰 16 张表（11 identity + 5 runtime）、无外键
/// （`information_schema.REFERENTIAL_CONSTRAINTS` 0 行）、无 CHECK
/// （`information_schema.CHECK_CONSTRAINTS` 0 行）、初始业务集合
/// `users`/`sessions`/`audit_events` 全空；任一不满足即显式失败（去敏 panic）。
/// 门禁只 SELECT：不写、不迁移、不整库删清，不修改既有凭据与目标逻辑。
pub async fn required_migrated_v4_db() -> DbPool {
    let db = required_test_db().await;
    let meta_rows: Vec<(i32, i32)> = sqlx::query_as("SELECT singleton, schema_version FROM schema_meta")
        .fetch_all(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_single_v4_meta_row(&meta_rows);
    let tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.tables WHERE table_schema = DATABASE()")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(tables, 16, "fixture requires exactly 16 v4 tables (11 identity + 5 runtime)");
    let foreign_keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.REFERENTIAL_CONSTRAINTS WHERE constraint_schema = DATABASE()")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(foreign_keys, 0, "fixture requires a foreign-key-free v4 schema");
    let checks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM information_schema.CHECK_CONSTRAINTS WHERE constraint_schema = DATABASE()")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(checks, 0, "fixture requires a CHECK-free v4 schema");
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(users, 0, "fixture requires an empty initial business set (users)");
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(sessions, 0, "fixture requires an empty initial business set (sessions)");
    let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_events")
        .fetch_one(&db.0)
        .await
        .unwrap_or_else(|_| panic!("task4_fixture_operation_failed"));
    assert_eq!(audits, 0, "fixture requires an empty initial business set (audit_events)");
    db
}
```

- [ ] **Step 2: 确认默认命令不触发真库（离线安全保护）**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --test identity_admin_db
```
预期：输出 `test result: ok. 13 passed; 0 failed; 4 ignored`。确认无网络请求、无数据库连接。
证据范围（默认命令只运行 `identity_admin_db` 单一测试二进制）：
- `mod common;` 会把 `tests/common/mod.rs` 的 12 个纯离线 `#[test]` 编译进本二进制（逐一核证为纯函数/panic 捕获，无真库连接、无网络、无环境读取：`version_gate_rejects_wrong_engine_without_live_database`、`fixture_authorization_requires_all_confirmations`、`negative_revision_fixtures_have_valid_existing_parents_and_grant_permissions`、`max_snapshot_assertion_detects_state_change_at_same_revision`、`max_snapshot_assertion_detects_decision_change_at_same_revision`、`missing_actor_assertion_*` 5 个、`fixture_failure_cannot_be_business_success_offline`、`admission_failure_assertion_rejects_wrong_causes_offline`），不能靠筛掉 common 测试虚报"全文件 0 passed"（既有两个同样 `mod common;` 的 `mysql_identity`/`tidb_identity` 测试二进制同含这 12 例）；
- 第 13 个 passed 是本任务新增的纯表驱动测试 `v4_singleton_gate_fails_closed_on_empty_extra_or_wrong_rows`（不触真库）；
- 本文件 4 个用例全部 `#[ignore]`（含本轮唯一补全完整代码的 `last_admin_protection_prevents_deactivation_and_demotion`），默认命令不触真库；
- Step 3/4 的真库验收 selector 仍为 `-- --ignored <精确测试名> --test-threads=1`：子串过滤在 4 个 `#[ignore]` 用例中恰好命中 1 例、不命中 12 个非 ignored common 用例，单用例语义不变。

- [ ] **Step 3: `last_admin_protection_prevents_deactivation_and_demotion` 的 RED→GREEN 未来顺序（本轮仅写定顺序，不运行、不声称编译或实测）**

**编译前置（更新）：** 本测试文件的编译依赖 Task 4 `SqlxAuthzRepository` 的 `patch_user` 方法装配：Step 0 先装同签名可编译 RED 桩，Step 3 真实实现后由 Task 4 Step 3 的 `patch_user` 槽位完整方法（同签名：`&Session, [u8; 16], u64, Option<bool>, Option<bool>, Option<&str>` → `RepoFuture<'a, UserRecord>`）替换，二者同一方法槽位、不复制第二份。本轮仅文档修订：未编译、未跑测试，不得称"已可执行"或"编译通过"；真实 LastAdmin selector 仍恰好 1 方法，默认 `13 passed; 4 ignored` 计数不变，无需新增测试。

1. **RED（Task 4 Step 0 桩阶段）：** 桩 `patch_user` 为同签名可编译的恒定 `Err(ControllerError::InvalidState)`（或 `RevisionConflict`）；Preflight 齐备后由操作员串行执行任一引擎的该 selector：

```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored last_admin_protection_prevents_deactivation_and_demotion --test-threads=1
```

预期：恰好执行 1 个用例（不得 0 用例）；**第一个**失败断言是阶段 A 的 `matches!(first, ControllerError::LastAdmin)`——panic 信息含桩的实际变体（InvalidState/RevisionConflict），证明测试真实走通了写路径（不是门禁未满足的假绿、不是断言未命中）。
2. **GREEN（Task 4 Step 3 真实实现后）：** 同一 selector、同一前置 → 恰好 1 个用例且 PASS；阶段 A/B/C 的全部回读断言（用户行 `active/is_admin/must_change_password/revision`、`authz_epoch` 增量、会话撤销、审计行序列与 ADMIN namespace 归属）均成立。
3. **记账：** `#[ignore]` 不等于 GREEN；缺任一前置时验收账本如实记 `未验证`/`BLOCKED`，不得凭"已编写"声称通过。

- [ ] **Step 4: 仅在门禁具备后由操作员显式串行执行（本次计划不执行）**

```bash
# 五项环境变量须由统一门禁执行器从既有本地授权资料（secret/mysql.json / secret/tidb.json）
# 进程内载入并经 `TestMigrationConfig::from_test_env` 读取：CONTROLLER_TEST_DATABASE_URL /
# CONTROLLER_TEST_ALLOW_DESTRUCTIVE=1 / CONTROLLER_TEST_EXPECTED_DATABASE /
# CONTROLLER_TEST_BACKUP_REF / CONTROLLER_TEST_MIGRATION_ACK。
# 本轮声明统一门禁执行器为前置依赖（未验证）；URL 只经环境变量传递，不作命令参数、不回显（无 set -x）。
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored admin_write_tx_enforces_lock_order_and_actor_recheck --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored admin_write_tx_enforces_lock_order_and_actor_recheck --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored last_admin_protection_prevents_deactivation_and_demotion --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored last_admin_protection_prevents_deactivation_and_demotion --test-threads=1
# 四用例按 1→2→3→4 顺序串行执行；每用例门禁前受控重置与退出复位保证顺序执行及 RED→GREEN 复跑均再过空表门禁。
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored grant_creation_and_union_evaluation_real_db --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored grant_creation_and_union_evaluation_real_db --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored authz_epoch_cas_increments_monotonically_and_admin_audit_epoch_unique --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test identity_admin_db -- --ignored authz_epoch_cas_increments_monotonically_and_admin_audit_epoch_unique --test-threads=1
```
核对各引擎每个 selector 执行恰好 1 个测试且 PASS；缺前置时在验收账本如实记录 `未验证` 或 `BLOCKED`。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/tests/identity_admin_db.rs crates/rsetup-controller/tests/common/mod.rs
git commit -m "test(controller): 新增门禁化双引擎身份管理集成测试"
```

---

## 验收矩阵（AT 映射与分层证据）

| 验收项 | 规格来源 | 对应任务 | 层级 ① 纯逻辑/组件 | 层级 ② 进程内 HTTP Oneshot | 层级 ③ 真实双引擎事务（#[ignore]） |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **AT-01 / AUTH-01**（初始化与门禁） | 01 §8 | Task 1, 6 | 共享门禁单值 Host/Cookie 校验 | 26 路由未鉴权全部 401 | 真实会话过期/撤销即刻失效 |
| **AT-02 / AUTH-02 / ADMIN-01**（最后管理员） | 01 §8 | Task 2, 4, 7, 14 | `check_admin_demotion_allowed` 纯函数 | `PATCH /users/{id}` 返回 409 `LAST_ADMIN` | 真实并发降权/停用 CAS 拦截，回滚会话与审计 |
| **AT-03 / ACL-01..03**（动态授权并集） | 01 §8 | Task 3, 5, 8, 10 | `permission_sources` 来源归因；`device_is_visible` | `GET /devices` 最低识别投影；`GET /devices/{id}` 缺 read 403；不可见 404 | 真实 DB 角色归档/成员变更级联失效，`authz_epoch` 递增 |
| **AT-04 / ADM-01**（批量准入与重置提示） | 02 §8 | Task 11 | 批量上限 1024 校验；reason 字符校验 | `POST /approvals/approve` 逐项结果 200；revoke 提示 `requires_device_reset` | 真实 CAS 状态跃迁，通知队列落盘 |
| **AT-14 / DB-01**（双引擎一致性） | 02 §8 | Task 14 | SQL 文本与绑定数断言 | 不适用 | MySQL 8.4 LTS 与 TiDB 8.5 LTS 分别验证唯一性、CAS、无 FK/CHECK、审计 (process_epoch, event_seq) 唯一键与 ADMIN namespace 序列严格递增 |

---

## 阻断与未验证项追踪

| 项目 | 当前状态 | 阻断原因与解除条件 |
| :--- | :--- | :--- |
| **26 网关动态零调用证明** | **待实现** | 26 路由测试只证明"已挂载 + 门禁 401/403"；"无有效会话时业务网关 0 调用"依赖 `ManagementState` 生产/fixture 网关装配切片提供网关计数器；认证计数器（`auth_calls`）不能替代，恒 0 spy 不可接受。 |
| **`EndpointId::DeviceStatusRead` 归属** | **已定责待实施** | Runtime独立缓存读路由/handler，Identity新增独立读桶；不计26管理/8任务。真实缓存及当前授权未接线时不挂载、整体Task4未完成。 |
| **真实双引擎事务验收** | **未验证** | 依赖操作员对两个隔离专用开发库核验真实备份与环境授权；缺前置时保持 `#[ignore]`，不冒充 PASS。 |
| **Runtime v4 Schema 协调** | **待协调** | 本计划管理仓储写操作依赖 Runtime Task 3 将 `lock_integrity_guard` 升级为版本化；在 Runtime v4 落地前中控仅运行于 v3 基线。 |
| **板端连接与状态实时探针** | **未连接** | `DeviceLiveProbe` 在本计划使用 fixture；生产接入依赖 Transport Task 3/4 的设备监听与 Runtime Task 5 的 `SnapshotStore`。 |
| **受限 Observer 锁观察** | **BLOCKED** | 旧 writer 自观察不是独立 Observer 证据；无独立 Observer 账号与性能架构最小权限时保持阻断。 |
| **生产发布与线协议** | **BLOCKED** | 等待线协议安全专家书面决议、双端负例与获批 Protobuf codec；不以此计划作为发布许可。 |

---

## 与 Runtime / Transport / Web 的跨专题接口协调摘要

1. **`GET /api/v1/system/status` 单一归属确立（已解决冲突）：**
   - 冲突事实：既有 Identity 计划（26 路由 #26）与 Runtime 计划（Task 6 `http_system.rs`）均声明实现该端点。
   - 裁决：以最新四专题收敛设计 §1 为准，**该端点归属 Identity Task 4**（本计划 Task 12 实现）；Runtime 仅负责为其提供 `RecoveryModeProbe` 适配器。本次Runtime只提供8任务及独立device status缓存读路由；`/api/v1/system/time`与完整SSE是非当前后续索引，**不得重复注册 `/api/v1/system/status`**，以防路由冲突 panic。
2. **`GET /api/v1/devices/{id}/status` 已定责待实施：**
   - 规格02既有端点，权限仍 `device.status.read`；Runtime 提供独立路由/handler及真实缓存源，Identity 本Task1提供独立 `EndpointId::DeviceStatusRead`。不计Identity26管理或Runtime8任务，GET不触发采集/执行。
   - 缺typed DTO/真实快照或当前授权接线时不挂载，不返回常量null/offline假PASS，整体Task4仍未完成；Web只消费，不转移到Web实现后端。
3. **8 条最小任务 HTTP 路由归属与合并接口：**
   - 任务端点（`POST /task-previews`, `GET/POST /tasks`, `GET /tasks/{id}`, `GET /tasks/{id}/children`, `POST /tasks/{id}/cancel`, `POST /tasks/{id}/time-review`, `POST /subtasks/{id}/release-lock`）全属 **Runtime Task 6** 范围。
   - 接口契约：Runtime Task 6 必须共用本计划的 `http_gate::authenticate_read` / `authenticate_write`，并导出 `pub fn build_tasks_router(state: HttpTasksState) -> Router<()>` 供 Task 13 的 `build_production_router` 进行无冲突 `.merge()`。3 条任务 GET 路由分别传 `EndpointId::TaskList` / `TaskDetail` / `TaskChildren`（变体由 Identity 在 `http_auth.rs` 定义、Runtime 消费，Runtime 不得自行新增读变体）；5 条任务 POST 路由只按 `session.digest` 走 `write_limiter`，不进读桶。
4. **`build_production_router` 唯一归属：**
   - 归属唯一独立集成任务（见集成验收计划），不是Identity Task13；Identity只交付 `build_management_router(ManagementState)->Router<()>`，Runtime交付 `build_tasks_router(HttpTasksState)->Router<()>`及独立status子router，Web交付资源子router。lib/main最终组装只由集成owner串行修改，各线只按顺序注册模块导出，不各建顶层builder。
5. **审计 `process_epoch` 命名空间分离（偏差记录，已裁决）：**
   - 正式设计 §3 只要求"审计 `time_evidence` 非空、`process_epoch` 与 `event_seq` 合法且唯一；审计失败连同业务修改回滚"，不要求每 process 全 crate 唯一；正式审查已否决"不必要的全 crate 统一 epoch"。本计划 Task 4 保留旧写者 epoch 原样（auth 的 `AUTH_PROCESS_EPOCH`/`AUTH_EVENT_SEQ`、`db.rs` 函数内局部 `PROCESS_EPOCH`/`EVENT_SEQ`），新管理/任务写者改由 `crate::audit::admin_process_epoch()`（`ADMIN_PROCESS_EPOCH`）与 `ADMIN_EVENT_SEQ` 发放；三个独立 UUID epoch 各自由 CSPRNG 独立采样，随机碰撞概率极低但非数学零碰撞；DB `uq_audit_epoch_seq (process_epoch, event_seq)` UNIQUE 失败关闭，冲突必须使业务+审计同事务失败，不静默覆盖，"第三套"不构成冲突，无需撤回设计 §3 的解释。
6. **前端消息键契约：**
   - 本计划 Task 6 导出的 16 个新 `message_key`（如 `identity.last_admin`, `identity.role.builtin_archived`, `request.revision_invalid` 等）构成与 Web 专题双语字典的硬契约；Web 实施者必须在 `apps/controller-web` 语言包中全量补齐。
7. **Transport Wire DTO 缺失：**
   - 板端 `Capabilities`、`DeviceStatus`、`ClockSample`、`RebootTicket`、`TaskRecord` 尚未落地于 `rsetup-protocol::wire`；本计划在设备详情中通过 `DeviceLiveProbe` 抽象隔离，不直接引用未定型的 wire 类型。
8. **内部授权快照层不跨线（本轮 C1 修订偏差记录）：**
   - 本轮 C1 修复在 Identity 内部新增 `AuthorizationSnapshotRepository`（Task 3 `authz/sqlx_repo.rs`）：同一读事务内按固定顺序（actor 行 → `schema_meta` epoch → 本人 grants → 被引用 roles → 每设备自身 groups → 设备行）重验后构造 `AuthorizationSnapshot`；消费方仅限 `DeviceRepository::device_page`/`find_device` 与 Task 5 `AuthzService` 的求值/归因路径。
   - **不跨线**：不导出给 Runtime/Transport/Web，不新增 HTTP 路由、权限点、响应字段（26 路由矩阵与 `EndpointId` 均不变）；`device_page` 不复用 admin readonly helper、不放开管理 CRUD。Runtime 线无需构造该仓储；Task 13 生产装配新增一个构造点：`AuthzService::new(Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone())), Arc::new(SqlxAuthzRepository::new(db.clone())))`（`DbPool` 已 `Clone`）。`SqlxDeviceRepository` 构造方式不变（其列表/详情读路径直接复用 `pub(crate)` 同事务核心 helper，不依赖该仓储）。

---

## 执行交接

计划已保存到 `docs/superpowers/plans/2026-10-16-controller-v1-four-task4-identity-execution.md`。有两种执行方式：
1. **子代理驱动（推荐）：** 每项派新的子代理，任务间审查，快速迭代。使用 `superpowers:subagent-driven-development`。
2. **当前会话执行：** 使用 `superpowers:executing-plans`，分批落实并设置审查检查点。
你选哪种？


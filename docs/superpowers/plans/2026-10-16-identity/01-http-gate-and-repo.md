### Task 1: 共享可信认证门禁抽取（`src/http_gate.rs`）

**Files:**
- Create: `crates/rsetup-controller/src/http_gate.rs`
- Modify: `crates/rsetup-controller/src/http_auth.rs`（仅删除已迁出函数体、改为 `use crate::http_gate::...`；**不改任何 handler 的分步顺序**）
- Modify: `crates/rsetup-controller/src/lib.rs`
- Test: `crates/rsetup-controller/src/http_gate.rs`（新增 `#[cfg(test)] mod tests`）
- 回归证据：`crates/rsetup-controller/src/http_auth.rs` 既有全部测试必须原样通过

**Interfaces:**
- Consumes: `crate::http_auth::{HttpAuthState, EndpointId, WindowRateLimitError}`、`crate::http_security::{check_session_write, WriteDeny}`、`crate::auth::session::token_digest`、`crate::http_api::{ok_response, err_response}`、`crate::error::ControllerError`、`axum`。
- Produces（全部 `pub(crate)`）：
  - 纯解析/映射：`single_header(&HeaderMap, HeaderName) -> Option<&str>`、`session_cookie_value(&HeaderMap) -> Option<String>`、`get_single_origin(&HeaderMap) -> Result<Option<&str>, ()>`、`json_content_type(Option<&str>) -> bool`
  - 固定错误响应：`auth_required()`(401 AUTH_REQUIRED/auth.required)、`origin_host_invalid()`(403 CSRF_INVALID/security.origin_host)、`csrf_invalid()`、`password_change_required()`(403 PASSWORD_CHANGE_REQUIRED/auth.password_change_required)、`not_ready()`(503 NOT_READY/system.not_ready)、`storage_unavailable()`(503 STORAGE_UNAVAILABLE/system.storage_unavailable)、`exhausted()`(503 RESOURCE_EXHAUSTED/system.resource_exhausted + Retry-After: 60)、`rate_limited(Duration)`(429 RATE_LIMITED/auth.rate_limited + Retry-After)
  - `backend_error(&ControllerError) -> Response`
  - `async fn authenticate_read(&HttpAuthState, &HeaderMap, EndpointId) -> Result<Session, Response>`
  - `async fn authenticate_write(&HttpAuthState, &HeaderMap) -> Result<Session, Response>`
  - `async fn read_json_body<T: serde::de::DeserializeOwned>(&HeaderMap, Body) -> Result<T, Response>`
  - `fn require_admin_session(&Session) -> Result<(), Response>`
  - `const MAX_BODY_BYTES: usize = 1_048_576`（迁出，`pub(crate)`）
- **`crate::http_auth::EndpointId` 变体设计（唯一 owner：Identity 本计划）：** 保留既有 `Me, Sessions`（现有 auth 路由行为不动，不挪现有 `AuthMe`/`Sessions` 历史 code 行为）；新增 14 个读变体 = 10 个管理读 + 3 个任务读 + 1 个独立 `DeviceStatusRead`。路由（方法 + 模板）→ 变体映射表：

  | 路由（方法 + 模板） | `EndpointId` 变体 | 读桶键 |
  | :--- | :--- | :--- |
  | `GET /api/v1/auth/me`（既有） | `Me`（保留） | `(Me, session.digest)` |
  | `GET /api/v1/auth/sessions`（既有） | `Sessions`（保留） | `(Sessions, session.digest)` |
  | `GET /api/v1/users` | `UsersRead` | `(UsersRead, session.digest)` |
  | `GET /api/v1/users/{id}/effective-permissions` | `EffectivePermissionsRead` | `(EffectivePermissionsRead, session.digest)` |
  | `GET /api/v1/roles` | `RolesRead` | `(RolesRead, session.digest)` |
  | `GET /api/v1/grants` | `GrantsRead` | `(GrantsRead, session.digest)` |
  | `GET /api/v1/groups` | `GroupsRead` | `(GroupsRead, session.digest)` |
  | `GET /api/v1/devices` | `DevicesRead` | `(DevicesRead, session.digest)` |
  | `GET /api/v1/devices/{id}` | `DeviceDetailRead` | `(DeviceDetailRead, session.digest)` |
  | `GET /api/v1/approvals` | `ApprovalsRead` | `(ApprovalsRead, session.digest)` |
  | `GET /api/v1/audit` | `AuditRead` | `(AuditRead, session.digest)` |
  | `GET /api/v1/system/status` | `SystemStatusRead` | `(SystemStatusRead, session.digest)` |
  | `GET /api/v1/tasks`（Runtime Task 6 消费） | `TaskList` | `(TaskList, session.digest)` |
  | `GET /api/v1/tasks/{id}`（Runtime Task 6 消费） | `TaskDetail` | `(TaskDetail, session.digest)` |
  | `GET /api/v1/tasks/{id}/children`（Runtime Task 6 消费） | `TaskChildren` | `(TaskChildren, session.digest)` |
   | `GET /api/v1/devices/{id}/status`（Runtime 独立缓存读，不计26管理/8任务） | `DeviceStatusRead` | `(DeviceStatusRead, session.digest)` |

  - **读**：`read_limiter` 键恒为 `(EndpointId, session.digest)`（50/s，滑动 1s），每个变体一个独立桶；同一模板下不同对象 id（如 `GET /users/{idA}/effective-permissions` 与 `GET /users/{idB}/effective-permissions`）共享同一桶——handler 只传模板变体、不传 id，禁止按 id 拆无限桶。
  - **写**：全部 16 条管理写路由（POST/PATCH/PUT/DELETE）与 5 条任务 POST 只按 `session.digest` 走 `write_limiter`（20/s），不进入任何读桶；**禁止**共享管理读桶、**禁止**借用 `Me`/`Sessions` 桶、**禁止**按写 endpoint 分桶。
  - 3 个任务变体由 Identity 定义、Runtime Task 6 消费；Runtime 不得自行新增读变体（见文末跨专题协调第 3 条）。
  - 后续 `GET /api/v1/devices/{id}/status` 对应独立的 `DeviceStatusRead` 变体，已定责待实施：Runtime 提供独立路由/handler及当前授权缓存读，Identity 本Task1提供读桶；不计26管理/8任务，缺真实缓存源不挂载、不伪完整验收（见跨专题协调第 2 条），**不冒充 26 route**。
  - （历史备注：本任务初稿曾提出单一共享 `ManagementRead` 变体，已按"每模板一桶"原则替换为上表 10 个管理读变体；除历史备注外，本文不得再出现 `ManagementRead` 待实施引用。）

- [ ] **Step 1: 编写新共享门禁的失败测试（行为 RED）**

先在 `src/http_gate.rs` 建立可编译桩（`authenticate_read`/`authenticate_write` 直接返回固定 401，不校验 Host/Cookie；RED 阶段用 `http_api::err_response` 本地构建固定 401，不引用 `http_auth` 私有 `auth_required`），同时向 `http_auth.rs` 的 `EndpointId` 追加 14 个读变体（测试引用的 `EndpointId::UsersRead` 等必须存在才能编译；仅追加 enum 变体不改变既有 `Me`/`Sessions` 行为，属 RED 可编译前置而非行为实现），并在同文件写测试。测试必须可编译通过、运行后因断言失败：

```rust
// crates/rsetup-controller/src/http_gate.rs
// 顶层 imports = 生产代码实际引用的集合（桩签名 + Step 3 搬入的函数）。
// `Duration` 供 Step 3 搬入的 `rate_limited(remaining: Duration)`（http_auth.rs:1337）、
// `HeaderValue` 供 `exhausted()` 的 `HeaderValue::from_static("60")`（http_auth.rs:1323）：
// 二者都是搬移后的生产依赖，明确保留在顶层、不随仅 test 所需 imports 迁入 test 模块
// （RED 阶段 `Duration` 暂未被引用，仅产生 unused-import 警告而非错误）。
// 仅 test 所需的 `Arc`/`AtomicUsize`/`Ordering`、`IdentityUser`/`Login`/`SessionPage`、
// `AuthFuture`/`AuthServiceApi`、`HttpSecurityConfig` 与 `to_bytes` 一律改在下方 `mod tests`
// 内声明（生产代码只以全限定路径引用 `axum::body::Body`/`axum::body::to_bytes`，顶层无需 import
// 该模块）；全程不加 `#[allow(unused_imports)]`。
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use crate::auth::service::Session;
use crate::error::ControllerError;
use crate::http_api::err_response;
use crate::http_auth::{EndpointId, HttpAuthState};

// ── 可编译 RED 桩：不校验 Host/Cookie，恒 401 ─────────────────────────────
// RED 阶段的本地固定 401（与 `http_auth` 私有 `auth_required` 行为一致）；
// Step 3 把 `auth_required` 原样搬入本文件后删除本函数并切换调用点。
fn fixed_auth_required() -> axum::response::Response {
    err_response(
        StatusCode::UNAUTHORIZED,
        "AUTH_REQUIRED",
        "auth.required",
        serde_json::json!({}),
    )
}

pub(crate) async fn authenticate_read(
    _state: &HttpAuthState,
    _headers: &HeaderMap,
    _endpoint: EndpointId,
) -> Result<Session, axum::response::Response> {
    Err(fixed_auth_required())
}

pub(crate) async fn authenticate_write(
    _state: &HttpAuthState,
    _headers: &HeaderMap,
) -> Result<Session, axum::response::Response> {
    Err(fixed_auth_required())
}

// RED 桩：恒 Ok；Step 3 以真实 403 区分逻辑替换（must_change_password 与普通
// 无权限必须返回不同 403 code）。
pub(crate) fn require_admin_session(_session: &Session) -> Result<(), axum::response::Response> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::to_bytes;
    use crate::auth::service::{IdentityUser, Login, SessionPage};
    use crate::http_auth::{AuthFuture, AuthServiceApi};
    use crate::http_security::HttpSecurityConfig;

    /// `calls` 为真实 `Arc<AtomicUsize>` 认证 spy 计数器：每次实际
    /// `authenticate` 调用递增 1；测试侧持有独立 clone 读取。它不能永远
    /// 返回 0——阳性控制（合法单 Host/cookie 恰好 1）就是抓"恒 0"假计数
    /// 的。所有 fixture 构造（每个测试内的 `StubAuth { .. }` 字面量）都
    /// 必须带上该字段。
    struct StubAuth {
        token: String,
        session: Session,
        calls: Arc<AtomicUsize>,
    }

    fn fixture_session() -> Session {
        Session {
            user: IdentityUser {
                id: [7u8; 16],
                username: "operator".into(),
                password_hash: "not-a-real-hash".into(),
                active: true,
                is_admin: true,
                must_change_password: false,
                revision: 3,
            },
            digest: [9u8; 32],
        }
    }

    impl AuthServiceApi for StubAuth {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
            // 在实际 authenticate 入口递增（无论随后成功/失败）：
            // 门禁在 Host/Cookie 步被拒时这里根本不会执行，计数器保持 0。
            self.calls.fetch_add(1, Ordering::SeqCst);
            let ok = raw == self.token;
            let session = self.session.clone();
            Box::pin(async move {
                if ok { Ok(session) } else { Err(ControllerError::InvalidArgument) }
            })
        }
        fn change_password<'a>(&'a self, _s: &'a Session, _c: &'a str, _n: &'a str) -> AuthFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> { Box::pin(async { Ok(()) }) }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> { Box::pin(async { Ok(0) }) }
        fn is_live_session(&self, _d: &[u8; 32]) -> bool { true }
        fn list_sessions<'a>(&'a self, _s: &'a Session, _c: Option<&'a str>, _l: usize) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _i: &'a str) -> AuthFuture<'a, bool> { Box::pin(async { Ok(false) }) }
        fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> { Box::pin(async { Ok(0) }) }
    }

    fn state_with(stub: StubAuth) -> HttpAuthState {
        HttpAuthState::new(
            Arc::new(stub),
            HttpSecurityConfig::from_values(Some("controller.local"), Some("http://controller.local"), None).unwrap(),
        )
    }

    fn headers(host: &str, cookie: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(c) = cookie {
            h.insert(header::COOKIE, HeaderValue::from_str(c).unwrap());
        }
        h
    }

    #[tokio::test]
    async fn gate_rejects_host_and_cookie_failures_without_authenticating() {
        let token = "a".repeat(64);
        let calls = Arc::new(AtomicUsize::new(0));
        let stub = StubAuth { token: token.clone(), session: fixture_session(), calls: calls.clone() };
        let state = state_with(stub);
        let cookie = format!("rsc_session={token}");

        // 重复 Host：必须 403 CSRF_INVALID，且不进入 authenticate。
        let mut h = headers("controller.local", Some(&cookie));
        h.append(header::HOST, HeaderValue::from_static("controller.local"));
        let response = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect_err("duplicate Host must be rejected");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "duplicate Host must not reach authenticate");

        // 错误 Host：必须 403 CSRF_INVALID，且不进入 authenticate。
        let response = authenticate_read(&state, &headers("wrong.host", Some(&cookie)), EndpointId::UsersRead)
            .await
            .expect_err("wrong Host must be rejected");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "wrong Host must not reach authenticate");

        // 缺失 Host：必须 403 CSRF_INVALID，且不进入 authenticate。
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
        let response = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect_err("missing Host must be rejected");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "missing Host must not reach authenticate");

        // 缺失 cookie：必须 401 AUTH_REQUIRED，且不进入 authenticate。
        let response = authenticate_read(&state, &headers("controller.local", None), EndpointId::UsersRead)
            .await
            .expect_err("missing cookie must be rejected");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "missing cookie must not reach authenticate");

        // 畸形 cookie（长度不足）：必须 401，且不进入 authenticate。
        let response = authenticate_read(&state, &headers("controller.local", Some("rsc_session=short")), EndpointId::UsersRead)
            .await
            .expect_err("malformed cookie must be rejected");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "malformed cookie must not reach authenticate");

        // 重复 cookie（两个 rsc_session）：必须 401，且不进入 authenticate。
        let response = authenticate_read(&state, &headers("controller.local", Some(&format!("{cookie}; {cookie}"))), EndpointId::UsersRead)
            .await
            .expect_err("duplicate cookie must be rejected");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(calls.load(Ordering::SeqCst), 0, "duplicate cookie must not reach authenticate");
    }

    #[tokio::test]
    async fn gate_accepts_exactly_one_valid_cookie_and_returns_authenticated_session() {
        let token = "b".repeat(64);
        let calls = Arc::new(AtomicUsize::new(0));
        let state = state_with(StubAuth { token: token.clone(), session: fixture_session(), calls: calls.clone() });
        let h = headers("controller.local", Some(&format!("rsc_session={token}")));
        let session = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect("valid single cookie must authenticate");
        assert_eq!(session.user.id, [7u8; 16]);
        // 阳性控制：合法单 Host/cookie 时权威 authenticate 恰好被调用 1 次
        // （计数器不能恒 0；一个"恒 0 spy"会通过所有拒绝断言却在这里失败）。
        // 数据响应的 error code/message_key 按已有契约断言，不因本计数器"顺便"更改。
        assert_eq!(calls.load(Ordering::SeqCst), 1, "exactly one authoritative authenticate for a valid request");
    }

    #[tokio::test]
    async fn read_buckets_are_independent_per_endpoint_variant_and_session() {
        let token = "c".repeat(64);
        let state = state_with(StubAuth { token: token.clone(), session: fixture_session(), calls: Arc::new(AtomicUsize::new(0)) });
        let h = headers("controller.local", Some(&format!("rsc_session={token}")));

        // 同一有权 session：UsersRead 第 1..=50 次通过，第 51 次 429。
        for i in 1..=50 {
            authenticate_read(&state, &h, EndpointId::UsersRead)
                .await
                .unwrap_or_else(|_| panic!("UsersRead #{i} must pass within 50/s"));
        }
        let limited = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect_err("the 51st UsersRead call must be rate limited");
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);

        // RolesRead 桶相互独立：第 1 次成功（未被 UsersRead 耗尽）。
        authenticate_read(&state, &h, EndpointId::RolesRead)
            .await
            .expect("first RolesRead call must pass (independent bucket)");

        // 既有 Me 桶不被管理读变体借用：第 1 次成功。
        authenticate_read(&state, &h, EndpointId::Me)
            .await
            .expect("first Me call must pass (Me is not borrowed)");
    }

    #[tokio::test]
    async fn same_template_with_different_ids_shares_one_read_bucket() {
        let token = "d".repeat(64);
        let state = state_with(StubAuth { token: token.clone(), session: fixture_session(), calls: Arc::new(AtomicUsize::new(0)) });
        let h = headers("controller.local", Some(&format!("rsc_session={token}")));

        // handler 只传模板变体（EffectivePermissionsRead）而不传对象 id：
        // GET /users/{idA}/effective-permissions 与 GET /users/{idB}/effective-permissions
        // 共享同一 (variant, session.digest) 桶——不是按 id 的无限桶。
        // 这里以同一变体直接调门禁，证明同模板第 51 次调用（换个 id 也一样）耗尽桶。
        for i in 1..=50 {
            authenticate_read(&state, &h, EndpointId::EffectivePermissionsRead)
                .await
                .unwrap_or_else(|_| panic!("call #{i} must pass within 50/s"));
        }
        let limited = authenticate_read(&state, &h, EndpointId::EffectivePermissionsRead)
            .await
            .expect_err("51st same-template call must be rate limited even with a different id");
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn require_admin_session_distinguishes_force_password_change_from_plain_denial() {
        let mut user = fixture_session().user;
        user.must_change_password = true;
        let session = Session { user, digest: [0u8; 32] };
        let response = require_admin_session(&session).expect_err("must change password is refused");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "PASSWORD_CHANGE_REQUIRED");
        assert_eq!(body["error"]["message_key"], "auth.password_change_required");

        let mut user = fixture_session().user;
        user.is_admin = false;
        let session = Session { user, digest: [0u8; 32] };
        let response = require_admin_session(&session).expect_err("non-admin is refused");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
        assert_eq!(body["error"]["message_key"], "permission.denied");

        assert!(require_admin_session(&fixture_session()).is_ok());
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（命令退出码必须非零）**

模块过滤（不带 `--exact`）一次跑全部 5 个新测试：

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller http_gate::tests
```
预期：编译通过（若编译失败，先检查 Step 1 的 13 个 `EndpointId` 变体是否已加——缺变体属编译错误，不是行为 RED）；运行恰好 5 个测试；**整体退出码非零**，且失败全部来自行为断言（RED 必须由命令非零退出确认）：
- `gate_rejects_host_and_cookie_failures_without_authenticating` **FAIL**：桩恒返回 401，重复/错误/缺失 Host 的 403 断言先失败；
- `gate_accepts_exactly_one_valid_cookie_and_returns_authenticated_session` **FAIL**：断言 `expect("valid single cookie must authenticate")` 失败（桩恒返回 401），阳性控制 `auth_calls == 1` 亦失败（桩未调 `authenticate`，计数器为 0——证明计数器不是恒 0 spy）；
- `read_buckets_are_independent_per_endpoint_variant_and_session` 与 `same_template_with_different_ids_shares_one_read_bucket` **FAIL**：桩恒返回 401，第 1 次 `expect`/`unwrap` 即失败；
- `require_admin_session_distinguishes_force_password_change_from_plain_denial` **FAIL**：桩返回 `Ok(())`，第一个 `expect_err` 即失败（RED 只断言 Err）；403 状态与 `error.code`/`message_key` body 断言（`to_bytes` + `serde_json`）在 RED 阶段尚未执行到位，不得记为已执行证据。
若失败原因是编译错误或 0 用例，必须修正后再继续，不得记为 RED。

- [ ] **Step 3: 最小实现（搬移 + 组合，行为保持）**

1. 把 `http_auth.rs` 中 `session_cookie_value`、`single_header`、`get_single_origin`、`json_content_type`、`backend_error`、`auth_required`、`origin_host_invalid`、`csrf_invalid`、`password_change_required`（http_auth.rs:1272；`require_admin_session` 的 must_change_password 分支调用它，是 403 精确断言 `PASSWORD_CHANGE_REQUIRED`/`auth.password_change_required` 的实现来源，Interfaces 已列、搬移清单此前漏列）、`not_ready`、`exhausted`、`rate_limited`、`MAX_BODY_BYTES` **整段剪切**到 `http_gate.rs`，可见性改为 `pub(crate)`，函数体**逐字不变**（不"顺便"改顺序、不"顺便"改 message_key）。搬移后删除 RED 阶段的本地 `fixed_auth_required` 并把调用点切换到 `auth_required()`，同时以本步骤第 3 项的 `require_admin_session` 真实逻辑替换恒 `Ok(())` 桩。
2. 在 `http_auth.rs` 顶部加 `use crate::http_gate::{...};`，其余 handler 代码保持原样（既有 handler 的自身分步顺序不动，例如 `password` 仍不调用 `is_live_session` 预检）。
3. 实现新组合门禁（新代码，供管理路由使用）：

```rust
// crates/rsetup-controller/src/http_gate.rs
// 顶层 imports = Step 1 的生产 import 集合（保持不变），本步仅新增下列项
// （与 Step 1 同步，避免两份 import 片段各自漂移；不重复罗列 Step 1 已有行，
// 不加 `#[allow(unused_imports)]`）：
use std::time::Instant;

use axum::http::HeaderName;
use axum::response::Response;
use crate::auth::session::token_digest;
use crate::http_auth::WindowRateLimitError;
use crate::http_security::check_session_write;

/// 管理读边界：单值 Host → 单值严格 cookie → 进程内活会话预检 →
/// DB 权威 authenticate → 50/s 端点读限速。任一步失败都不得触达业务网关。
pub(crate) async fn authenticate_read(
    state: &HttpAuthState,
    headers: &HeaderMap,
    endpoint: EndpointId,
) -> Result<Session, Response> {
    let host = single_header(headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return Err(origin_host_invalid());
    }
    let Some(raw) = session_cookie_value(headers) else {
        return Err(auth_required());
    };
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return Err(auth_required());
    }
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(session) => session,
        Err(ControllerError::InvalidArgument) => {
            state.csrf.lock().unwrap().remove(&digest);
            return Err(auth_required());
        }
        Err(error) => return Err(backend_error(&error)),
    };
    let now = Instant::now();
    let mut limiter = state.read_limiter.lock().unwrap();
    match limiter.check_and_record(now, &(endpoint, session.digest)) {
        Err(WindowRateLimitError::Exhausted) => return Err(exhausted()),
        Err(WindowRateLimitError::Limited(remaining)) => return Err(rate_limited(remaining)),
        Ok(()) => {}
    }
    Ok(session)
}

/// 管理写边界：单值 Host → Origin（缺失仅豁免 Origin）→ 单值严格 cookie →
/// 活会话预检 → DB 权威 authenticate → 绑定 session.digest 的 CSRF →
/// 20/s 会话写限速。body 由 handler 在其后调用 `read_json_body` 读取。
pub(crate) async fn authenticate_write(
    state: &HttpAuthState,
    headers: &HeaderMap,
) -> Result<Session, Response> {
    let host = single_header(headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return Err(origin_host_invalid());
    }
    let origin = match get_single_origin(headers) {
        Ok(origin) => origin,
        Err(()) => return Err(origin_host_invalid()),
    };
    if origin.is_some_and(|o| state.config.allowed_origin.as_deref() != Some(o)) {
        return Err(origin_host_invalid());
    }
    let Some(raw) = session_cookie_value(headers) else {
        return Err(auth_required());
    };
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return Err(auth_required());
    }
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(session) => session,
        Err(ControllerError::InvalidArgument) => {
            state.csrf.lock().unwrap().remove(&digest);
            return Err(auth_required());
        }
        Err(error) => return Err(backend_error(&error)),
    };
    let csrf_header = single_header(headers, HeaderName::from_static("x-csrf-token"));
    let binding = state.csrf.lock().unwrap().get(&session.digest).cloned();
    let Some(binding) = binding else {
        return Err(csrf_invalid());
    };
    if check_session_write(host, origin, csrf_header, &binding.token, &state.config).is_err() {
        return Err(csrf_invalid());
    }
    let now = Instant::now();
    let mut limiter = state.write_limiter.lock().unwrap();
    match limiter.check_and_record(now, &session.digest) {
        Err(WindowRateLimitError::Exhausted) => return Err(exhausted()),
        Err(WindowRateLimitError::Limited(remaining)) => return Err(rate_limited(remaining)),
        Ok(()) => {}
    }
    Ok(session)
}

/// `application/json` + ≤1 MiB + serde 结构校验；任何失败都是固定 400
/// `INVALID_ARGUMENT` / `request.invalid_body`，不回显原始 body 文本。
pub(crate) async fn read_json_body<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    body: axum::body::Body,
) -> Result<T, Response> {
    if !json_content_type(single_header(headers, header::CONTENT_TYPE)) {
        return Err(invalid_body());
    }
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return Err(invalid_body());
    }
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return Err(invalid_body()),
    };
    serde_json::from_slice(&bytes).map_err(|_| invalid_body())
}

fn invalid_body() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "request.invalid_body",
        serde_json::json!({}),
    )
}

/// 管理范围前置过滤：`must_change_password` 与普通无权限必须返回不同
/// 403 code。**这只是廉价前置**；权威判定在仓储事务 guard 内重新核验。
pub(crate) fn require_admin_session(session: &Session) -> Result<(), Response> {
    if session.user.must_change_password {
        return Err(password_change_required());
    }
    if session.user.active && session.user.is_admin {
        return Ok(());
    }
    Err(err_response(
        StatusCode::FORBIDDEN,
        "PERMISSION_DENIED",
        "permission.denied",
        serde_json::json!({}),
    ))
}
```

4. 确认 `RateWindowLimiter` 键类型仍为 `(EndpointId, [u8; 32])`（14 个读变体已在 Step 1 作为 RED 可编译前置加入 `http_auth.rs`；本步只确认键类型，并按 Interfaces 映射表确认每个变体独立桶的语义已被测试覆盖——无共享桶、无借 `Me`、不按写 endpoint 分桶）。
5. `http_gate.rs` 新增函数全部 `pub(crate)` 并引用 `crate::http_auth::{EndpointId, HttpAuthState, WindowRateLimitError}`：Rust 同 crate 内模块互相引用自由，循环引用不是编译错误，无需提升 `pub` 可见性或拆独立模块大重构。测试需要读 `state.read_limiter`/`state.config`/`state.auth`——真实 `HttpAuthState` 这些字段本就是 `pub`（已对 `src/http_auth.rs` 基线核实）；spy 计数器经 `Arc<AtomicUsize>` 共享 clone 读出，无需归属可见性调整、不得写影子类型（若确需访问私有字段，必须在本计划明示归属与可见性变更）。
6. `src/lib.rs` 注册 `pub mod http_gate;`。

- [ ] **Step 4: 运行并确认 GREEN + 认证边界回归**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller http_gate::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller http_auth::tests
```
预期：第一条运行恰好 5 个测试全部 PASS（退出码零；其中 `auth_calls` 阳性控制断言 1 次、各拒绝用例断言 0 次、两个独立桶用例断言 50 过 51 限 + 跨桶第 1 次成功、`require_admin` 用例断言强制改密与普通拒绝各自的 403 `error.code`/`message_key`）；第二条为 `http_auth` 既有全部测试，**必须与搬移前同样全绿**（这是"抽取未改变认证行为"的唯一证据）。任何 `http_auth` 测试失败都说明搬移改变了行为，必须修复而不是修改断言。

**50 过 51 限快速门禁用例的证据范围（不得外推）**：这两个用例只证明容量（50/s）、按 `(EndpointId 变体, session.digest)` 的桶隔离、以及同模板不按对象 id 建桶三点；它们**未**测试限流后等待自然窗口恢复（衰减）的行为，也**未**覆盖经完整 HTTP 集成路径（router → handler）触达门禁的端到端行为——这两项在本切片记为"未测试"，不得表述为已验证。自然窗口衰减的既有覆盖是 `http_auth::tests::test_rate_window_recovers_after_one_second`（`src/http_auth.rs:4514–4534`：以注入的 `Instant` 与 `Instant + 1s` 驱动 `check_and_record`，无 `sleep`），它属于第二条命令的 `http_auth` 既有回归、不是本切片新增测试，本切片不新增任何 sleep 测试。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/http_gate.rs crates/rsetup-controller/src/http_auth.rs crates/rsetup-controller/src/lib.rs
git commit -m "refactor(controller): 提取共享可信 HTTP 认证门禁（http_gate）"
```

---

### Task 2: 错误变体、LastAdmin 纯规则与 409 映射

**Files:**
- Modify: `crates/rsetup-controller/src/error.rs`
- Modify: `crates/rsetup-controller/src/authz/policy.rs`
- Modify: `crates/rsetup-controller/src/http_gate.rs`（`backend_error` 增加两个 409 分支 + 测试）
- Test: 同上两文件的 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `ControllerError`、`crate::http_gate::backend_error`。
- Produces:
  - `ControllerError::LastAdmin`（`#[error("last administrator cannot be deactivated or demoted")]`）
  - `ControllerError::InvalidState`（`#[error("invalid state for this operation")]`）
  - `crate::authz::policy::check_admin_demotion_allowed(current_admin_count: i64) -> Result<(), ControllerError>`
  - `http_gate::backend_error` 新增 `LastAdmin` → 409 `LAST_ADMIN` / `identity.last_admin`，`InvalidState` → 409 `INVALID_STATE` / `identity.invalid_state`

- [ ] **Step 1: 编写失败测试**

```rust
// crates/rsetup-controller/src/authz/policy.rs  （追加到既有 tests 模块）
#[test]
fn last_admin_rule_rejects_demoting_the_only_active_admin() {
    assert!(matches!(
        super::check_admin_demotion_allowed(1),
        Err(crate::ControllerError::LastAdmin)
    ));
    assert!(matches!(
        super::check_admin_demotion_allowed(0),
        Err(crate::ControllerError::LastAdmin)
    ));
    assert!(super::check_admin_demotion_allowed(2).is_ok());
}
```

```rust
// crates/rsetup-controller/src/http_gate.rs  （追加到既有 tests 模块）
#[tokio::test]
async fn last_admin_and_invalid_state_map_to_409_not_503() {
    let last_admin = backend_error(&ControllerError::LastAdmin);
    assert_eq!(last_admin.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(last_admin.into_body(), 64 * 1024).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "LAST_ADMIN");
    assert_eq!(body["error"]["message_key"], "identity.last_admin");

    let invalid_state = backend_error(&ControllerError::InvalidState);
    assert_eq!(invalid_state.status(), StatusCode::CONFLICT);
    let bytes = to_bytes(invalid_state.into_body(), 64 * 1024).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"]["code"], "INVALID_STATE");
}
```

- [ ] **Step 2: 运行并确认失败**

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller last_admin_rule_rejects_demoting_the_only_active_admin
```
预期：编译失败是**不允许**的（`ControllerError::LastAdmin` 尚不存在 ⇒ 编译错误）。因此本任务 Step 1 必须先按 Step 3 的最小实现补上两个**变体定义**与返回 `Ok(())` 的**行为桩**，再运行测试得到行为 RED：

```rust
// 第一次可编译但行为错误的桩（随后在 Step 4 替换）
pub fn check_admin_demotion_allowed(_current_admin_count: i64) -> Result<(), ControllerError> {
    Ok(())
}
```
```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller last_admin_rule_rejects_demoting_the_only_active_admin
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller last_admin_and_invalid_state_map_to_409_not_503
```
预期：各运行 1 个测试；两条均 **FAIL**——第一条因桩返回 `Ok(())` 而断言失败；第二条因 `backend_error` 通配分支返回 503 `NOT_READY` 而断言 `left == 503, right == 409` 失败。确认失败来自断言，而非 0 用例。

- [ ] **Step 3: 变体与映射最小实现**

```rust
// crates/rsetup-controller/src/error.rs
    #[error("last administrator cannot be deactivated or demoted")]
    LastAdmin,
    #[error("invalid state for this operation")]
    InvalidState,
```

```rust
// crates/rsetup-controller/src/http_gate.rs（backend_error 内，Database 分支之后）
        ControllerError::LastAdmin => err_response(
            StatusCode::CONFLICT,
            "LAST_ADMIN",
            "identity.last_admin",
            serde_json::json!({}),
        ),
        ControllerError::InvalidState => err_response(
            StatusCode::CONFLICT,
            "INVALID_STATE",
            "identity.invalid_state",
            serde_json::json!({}),
        ),
```

- [ ] **Step 4: 纯规则最小实现并确认 GREEN**

```rust
// crates/rsetup-controller/src/authz/policy.rs
/// 严格最后管理员规则：当前活跃管理员数 ≤ 1 时，任何"停用或降级活跃
/// 管理员"的操作必须拒绝。调用方必须只在 target 当前 `active && is_admin`
/// 且本次变更确实移除该状态时才调用本函数（见 Task 4 的事务契约）。
pub fn check_admin_demotion_allowed(current_admin_count: i64) -> Result<(), ControllerError> {
    if current_admin_count <= 1 {
        Err(ControllerError::LastAdmin)
    } else {
        Ok(())
    }
}
```

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller last_admin_rule_rejects_demoting_the_only_active_admin
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller last_admin_and_invalid_state_map_to_409_not_503
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller http_gate::tests
```
预期：前两条各 1 个测试 PASS；第三条 `http_gate` 全部测试 PASS。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/error.rs crates/rsetup-controller/src/authz/policy.rs crates/rsetup-controller/src/http_gate.rs
git commit -m "feat(controller): 新增 LastAdmin 与 InvalidState 规则及 409 映射"
```

---

### Task 3: 管理对象只读 SQLx 仓储（users / roles / grants / groups / devices）

**Files:**
- Create: `crates/rsetup-controller/src/authz/sqlx_repo.rs`
- Create: `crates/rsetup-controller/src/devices/repository.rs`
- Modify: `crates/rsetup-controller/src/authz/policy.rs`（新增纯类型 `SnapshotActor`、常量 `ALL_DEVICE_PERMISSIONS` 与纯函数 `eval_memory_permissions`——内部快照求值的可离线 RED→GREEN 目标，不依赖 DB）
- Modify: `crates/rsetup-controller/src/authz/mod.rs`（注册 `pub mod sqlx_repo;` 并 re-export `UserRecord, AuthzRepository, SqlxAuthzRepository, RepoFuture, AuthorizationSnapshot, AuthorizationSnapshotRepository, SqlxAuthorizationSnapshotRepository`；policy re-export 行追加 `SnapshotActor, ALL_DEVICE_PERMISSIONS, eval_memory_permissions`）
- Modify: `crates/rsetup-controller/src/devices/mod.rs`（注册 `pub mod repository;`）
- Modify: `crates/rsetup-controller/src/integrity.rs`（`raw_bool` → `pub(crate)`）
- Test: 两文件内的 `#[cfg(test)] mod tests`（纯解码/纯映射/分页/SQL 绑定数 + 内部快照纯求值的可编译行为测试）

**Interfaces:**
- Consumes: `crate::db::DbPool`、`crate::db::valid_username`（`pub(crate)`）、`crate::integrity::{raw_bool, validate_grant_fields}`、`crate::auth::service::Session`、`crate::model::{AdmissionState, ReviewDecision}`、`crate::authz::policy::{Permission, GrantRecord, GrantScope, GrantSource, RoleRecord, evaluate_grants, minimum_device_projection}`。
- Produces:
  - `pub type RepoFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;`
  - `pub struct UserRecord { pub id: [u8;16], pub username: String, pub display_name: String, pub active: bool, pub is_admin: bool, pub must_change_password: bool, pub revision: u64 }`（**故意不复用 `auth::service::IdentityUser`**：后者无 `display_name`，改动它会波及 auth 服务、`http_api::user_public` 与数百个既有测试；`UserRecord` 是管理侧投影，永不携带 `password_hash`）
  - `pub struct GroupRecord { pub id: Uuid, pub name: String, pub archived: bool, pub revision: u64, pub member_count: u64 }`
  - `pub struct DeviceAdminView { pub public_key: [u8;32], pub display_name: String, pub admission_state: AdmissionState, pub review_decision: ReviewDecision, pub revision: u64, pub descriptor: Option<serde_json::Value>, pub archived: bool }`（`Clone + Debug + PartialEq + Eq`，供单快照同事务详情投影克隆视图与比较）
  - `pub struct AuditEventRow { pub id: Uuid, pub actor_kind: String, pub actor_user_id: Option<[u8;16]>, pub event_type: String, pub target_kind: String, pub target_id: Option<String>, pub params_redacted: String, pub outcome: String, pub process_epoch: Uuid, pub event_seq: u64, pub created_evidence: serde_json::Value }`
  - `pub struct AuditFilter { pub actor_id: Option<[u8;16]>, pub device_id: Option<[u8;32]> }`
  - `pub trait AuthzRepository: Send + Sync`（全部方法返回 `RepoFuture`，`actor: &'a Session` 必填，实现内**必须**重新核验 actor 当前管理员状态）：
    - `find_user(&self, actor, id: [u8;16]) -> RepoFuture<Option<UserRecord>>`
    - `list_users(&self, actor, cursor: Option<[u8;16]>, limit: usize) -> RepoFuture<(Vec<UserRecord>, Option<[u8;16]>)>`
    - `find_role(&self, actor, id: Uuid) -> RepoFuture<Option<RoleRecord>>`
    - `list_roles(&self, actor, cursor: Option<Uuid>, limit: usize) -> RepoFuture<(Vec<RoleRecord>, Option<Uuid>)>`
    - `find_grant(&self, actor, id: Uuid) -> RepoFuture<Option<GrantRecord>>`
    - `list_grants(&self, actor, user_id: Option<[u8;16]>, cursor: Option<Uuid>, limit: usize) -> RepoFuture<(Vec<GrantRecord>, Option<Uuid>)>`
    - `list_grants_for_user(&self, actor, user_id: [u8;16]) -> RepoFuture<Vec<GrantRecord>>`
    - `roles_by_id(&self, actor, ids: Vec<Uuid>) -> RepoFuture<HashMap<Uuid, RoleRecord>>`
    - `device_groups_for_device(&self, actor, device_id: [u8;32]) -> RepoFuture<HashSet<Uuid>>`（group member 是 device 不是 user：查询 `group_members` 时绑定**当前目标** `device_id = ?`，得到该 device 所属 group 集合——不是 actor 全部 grant 中出现过的 group 集合；过滤 archived group 与 archived device；未知/污染关联（member 指向不存在/已归档的 device 行、或 group 行缺失）报固定去敏 `Config` 错误（503 `STORAGE_UNAVAILABLE` 形状），不静默返回空集当"无权限"）
    - `find_group(&self, actor, id: Uuid) -> RepoFuture<Option<GroupRecord>>`
    - `list_groups(&self, actor, cursor: Option<Uuid>, limit: usize) -> RepoFuture<(Vec<GroupRecord>, Option<Uuid>)>`
    - `list_group_device_ids(&self, actor, group_id: Uuid) -> RepoFuture<Vec<[u8;32]>>`
  - `pub trait DeviceRepository: Send + Sync`：
    - `device_page(&self, actor, cursor: Option<[u8;32]>, limit: usize) -> RepoFuture<(Vec<(DeviceAdminView, HashSet<Permission>)>, Option<[u8;32]>)>`（**只返回 actor 当前可见设备**；无全量计数泄露）
    - `find_device(&self, actor, id: [u8;32]) -> RepoFuture<Option<(DeviceAdminView, HashSet<Permission>)>>`
    - `audit_page(&self, actor, filter: AuditFilter, cursor: Option<AuditCursor>, limit: usize) -> RepoFuture<(Vec<AuditEventRow>, Option<AuditCursor>)>`（稳定排序键 `(process_epoch字节序,event_seq,id字节序)`；保留AUTH/CAS/ADMIN命名空间，不把不同epoch同seq合并为同一行。签名/SQL/投影/测试均消费本Task3下方完整复合游标定义）
    - `system_counts(&self, actor) -> RepoFuture<SystemCounts>`，`pub struct SystemCounts { pub users: u64, pub devices: u64, pub roles: u64, pub grants: u64, pub groups: u64, pub schema_version: i32 }`
  - **内部授权快照层（本轮修订新增，闭合 C1："内部普通用户 `AuthorizationSnapshotRepository` 缺失"）**——消费方为 `DeviceRepository::device_page`/`find_device` 与 Task 5 `AuthzService` 的有效权限求值；**不新增任何 HTTP 权限点、响应字段或路由**（HTTP admin 列查仍走上面的 `AuthzRepository` admin helper 原样不变，普通用户只经本层内部读取**本人**快照）：
    - `policy::SnapshotActor { pub id: [u8;16], pub active: bool, pub is_admin: bool, pub must_change_password: bool }`（**DB 当前** actor 行的严格 0/1 布尔投影，`Clone + Copy + Debug + PartialEq + Eq`；HTTP 层 `Session`/客户端数据只是廉价前置过滤，不是当前授权真相——admin shortcut 以本结构判定：当前 actor `active && is_admin && !must_change_password` 才全量）
    - `policy::ALL_DEVICE_PERMISSIONS: [Permission; 4]`（`[DeviceRead, DeviceStatusRead, DeviceReboot, DeviceTaskRead]`，规格 01 §1"管理员具有全局及全部设备权限"的封闭全集）
    - `pub struct AuthorizationSnapshot { pub actor: SnapshotActor, pub authz_epoch: u64, pub grants: Vec<GrantRecord>, pub roles: HashMap<Uuid, RoleRecord>, pub device_groups: HashMap<[u8; 32], HashSet<Uuid>>, pub devices: HashMap<[u8; 32], DeviceAdminView> }`（`Clone + Debug + PartialEq + Eq`；语义逐字段：
      - `actor`：同一读事务内 DB 当前 actor 行（行缺失 → `PermissionDenied`，与 admin helper 同形、**不是 `NotFound`**）；
      - `authz_epoch`：`schema_meta` guard 行 `authz_epoch`，与 actor 行同事务同刻读取（0 行或 >1 行 = 污染 → `Config`；供调用方做 epoch 失效/重验，规格 01 §5）；
      - `grants`：**仅 actor 本人** grants（`user_id = actor.id` 全量 source/scope；他人 grant 不入此字段）；
      - `roles`：上述 grants 引用的全部 role 行（含 `role_permissions`；**引用缺失 = 污染**，见 `build_authorization_snapshot`）；
      - `device_groups`：**按 device key 的自身 group 集合**（候选设备公钥 → 该设备所属**未归档** group id 集合；dangling member 行（group 行缺失）= 污染；**禁止**把 actor grant 中出现过的 group 全集套到所有 device）；
      - `devices`：候选设备当前行（公钥 → 行；请求了但行缺失 = 该设备不存在，**不是污染**）。
      ）
    - `pub trait AuthorizationSnapshotRepository: Send + Sync`（对象安全，复用 `RepoFuture` 即 `Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send>>`）：
      - `fn load_snapshot<'a>(&'a self, actor_id: [u8; 16], device_ids: &'a [[u8; 32]]) -> RepoFuture<'a, AuthorizationSnapshot>`——**一次读事务**读齐上述全部字段并组装 DTO；`device_ids` 由调用方去重（列表场景 = 候选设备集，≤ limit+1；详情/有效权限场景 = 单设备集）；**不接 `Session` 参数**（actor 只以已认证的 `actor_id` 定位，当前授权状态全部取自同事务 DB 行）。
      - `fn load_admin_target_snapshot<'a>(&'a self, caller: &'a crate::auth::service::Session, target_user: [u8; 16], device_ids: &'a [[u8; 32]]) -> RepoFuture<'a, AuthorizationSnapshot>`——**独立的 protected 管理读取**（与 `load_snapshot` 并列的第二 trait 方法，不进 admin CRUD 写路径）：同一读事务内先以 DB raw_bool 重验 `caller` 为当前 admin（复用同模块 `ensure_actor_admin_readonly`，而非 session flags），再读 `target_user` 快照；`Session` 仅携带 caller 定位，target admin 不授权 caller；**绝不另开 admin 仓储事务**。
    - `pub struct SqlxAuthorizationSnapshotRepository { db: DbPool }` + `pub fn new(db: DbPool) -> Self`：实现 = `acquire_authorization_read_connection`先核同连接RR → 在同lease begin → 同事务核心 `load_snapshot_in_tx` → commit；失败关闭，无读事务外部副作用。
    - 同事务核心（`pub(crate)` 低层 helper，`devices/repository.rs` 的列表/详情读路径**必须**复用它，不得改调自开事务 + admin 重验的 `AuthzRepository` 方法，不得"声称同 snapshot 却二次 query"）：
      - `pub(crate) async fn load_snapshot_in_tx(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, actor_id: [u8; 16], device_ids: &[[u8; 32]]) -> Result<AuthorizationSnapshot, ControllerError>`——固定读取顺序（不得换序）：actor 行（复用 `ACTOR_READONLY_SQL`，1 绑定 `(actor_id)`）→ `SNAPSHOT_META_SQL` → `LIST_ACTOR_GRANTS_SQL` → 角色引用集非空时 `ROLES_BY_IDS_SQL_TEMPLATE` + `ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE` → `device_ids` 非空时 `GROUPS_FOR_DEVICES_SQL_TEMPLATE` + `DEVICES_BY_IDS_SQL_TEMPLATE` → `build_authorization_snapshot` 组装。全部解码错误统一映射为固定去敏 `read_error` `Config`（503 `STORAGE_UNAVAILABLE` 形状），**不降级为空权限、不把 `InvalidArgument` 透传成 400**。空 `device_ids` 只短路最后两条 SQL（`device_groups`/`devices` 为空 map），**不得**跳过 actor/grants/roles/epoch 读取（空集不得绕过 actor 重验）。
      - 同事务核心拆分实现（`pub(crate)` 低层 helper，`load_snapshot_in_tx` 由两段组合而成，三段不得换序、不得拆到别的连接；`devices/repository.rs` 的列表/详情读路径**必须**复用下列 helper，不得改调自开事务 + admin 重验的 `AuthzRepository` 方法）：
      - `pub(crate) struct ActorAuthorization { pub actor: SnapshotActor, pub authz_epoch: u64, pub grants: Vec<GrantRecord>, pub roles: HashMap<Uuid, RoleRecord> }` + `pub(crate) async fn load_actor_authorization_in_tx(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, actor_id: [u8; 16]) -> Result<ActorAuthorization, ControllerError>`——actor 行（复用 `ACTOR_READONLY_SQL`，1 绑定；行缺失 → `PermissionDenied`）→ `SNAPSHOT_META_SQL`（恰好 1 行）→ `LIST_ACTOR_GRANTS_SQL`（`ORDER BY id` 稳定序，空集合法）→ 被引用 roles 去重 + 排序绑定（空引用集短路，不拼 `IN ()`）；一个读事务**至多读一次**（`device_page` 多候选批次间复用，不重读）。
      - `pub(crate) async fn load_device_snapshot_in_tx(tx: &mut sqlx::Transaction<'_, sqlx::MySql>, device_ids: &[[u8; 32]]) -> Result<(HashMap<[u8; 32], HashSet<Uuid>>, HashMap<[u8; 32], DeviceAdminView>), ControllerError>`——`GROUPS_FOR_DEVICES_SQL_TEMPLATE`（每设备自身 group 集）→ `DEVICES_BY_IDS_SQL_TEMPLATE`（设备当前行）；空 `device_ids` 两条都短路（返回空 map）——调用方不得借此跳过 actor 段；`device_ids` 由调用方去重（占位符 1:1）。
      - 绑定/解码契约（占位符 = 去重 id 数，1:1 固定数量拼接；空集必须在同事务层短路，不得拼进 SQL）：
        - `SNAPSHOT_META_SQL`：0绑定，无WHERE、LIMIT2；decode `(singleton:i64, schema_version:i64, authz_epoch:u64)`，恰好一行singleton=1且匹配编译期expected_version才成功；额外singleton行不得被SQL过滤。
        - `LIST_ACTOR_GRANTS_SQL`：1 绑定 `(user_id)`；每行走既有 `decode_grant`，错误统一 `.map_err(|_| read_error("snapshot.grants"))`。
        - `ROLES_BY_IDS_SQL_TEMPLATE`：绑定 = 去重 role id 字节串；解码 `id: Vec<u8>`→`Uuid`、`name: String`、`builtin/archived: i64`→`raw_bool`（code `snapshot.role_bool`）、`revision: u64`；任何失败 → `read_error("snapshot.roles")`。
        - `ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE`：同绑定；解码 `role_id: Vec<u8>`→`Uuid`、`permission: String`→`Permission::parse`（`None` = 未知权限 → `read_error("snapshot.role_permission")`）。
        - `GROUPS_FOR_DEVICES_SQL_TEMPLATE`：绑定 = 去重候选设备公钥；解码 `device_id: Vec<u8>`→`[u8;32]`、`group_id: Vec<u8>`→`Uuid`、`group_archived: Option<i64>`——`None` = dangling member 行（group 行缺失）→ `read_error("snapshot.group_missing")`；`Some` 走 `raw_bool`（code `snapshot.group_archived`），archived group 不入集合（过滤）。
        - `DEVICES_BY_IDS_SQL_TEMPLATE`：绑定 = 去重候选设备公钥；每行走 `decode_device_admin_view`，错误 → `read_error("snapshot.devices")`。
    - 纯函数（可离线 RED→GREEN 的测试目标，**必须有**）：
      - `pub fn build_authorization_snapshot(actor: SnapshotActor, authz_epoch: u64, grants: Vec<GrantRecord>, roles: HashMap<Uuid, RoleRecord>, device_groups: HashMap<[u8; 32], HashSet<Uuid>>, devices: HashMap<[u8; 32], DeviceAdminView>) -> Result<AuthorizationSnapshot, ControllerError>`——引用污染校验：`grants` 中每个 `GrantSource::Role(rid)` 必须命中 `roles`（缺失 → 固定去敏 `Config`，不得按"该 grant 无权限"继续）；被引用的当前有效role要求合法非空permissions；archived role可合法清空permissions并保留父行，无权而非污染；未知/重复权限和缺父引用仍失败关闭。校验通过 → `Ok(AuthorizationSnapshot { ... })`，不做任何其他规范化（不排序、不去重、不丢弃 archived role）。
  - 纯函数（可离线 RED→GREEN 的测试目标，**必须有**）：
    - `pub fn decode_user(id: Vec<u8>, username: String, display_name: String, active: i64, is_admin: i64, must_change_password: i64, revision: u64) -> Result<UserRecord, ControllerError>`
    - `pub fn page_cursor<T: Copy + PartialEq>(rows: &[T], limit: usize) -> (usize, Option<T>)`
    - `pub fn grant_columns(source: &GrantSource, scope: &GrantScope) -> Result<GrantColumns, ControllerError>`，`pub struct GrantColumns { pub source_kind: &'static str, pub role_id: Option<Uuid>, pub permissions_json: Option<String>, pub scope_kind: &'static str, pub scope_group_id: Option<Uuid>, pub scope_device_id: Option<[u8;32]> }`
    - `pub fn validate_display_name(value: &str) -> Result<(), ControllerError>`
    - `pub fn decode_grant(id: Vec<u8>, user_id: Vec<u8>, source_kind: String, role_id: Option<Vec<u8>>, permissions: Option<serde_json::Value>, scope_kind: String, scope_group_id: Option<Vec<u8>>, scope_device_id: Option<Vec<u8>>, revision: u64) -> Result<GrantRecord, ControllerError>`
    - `pub fn decode_device_admin_view(public_key: Vec<u8>, display_name: String, descriptor_json: Option<String>, admission_state: &str, review_decision: &str, revision: u64, archived: i64) -> Result<DeviceAdminView, ControllerError>`（设备行解码：`public_key` 必须恰 32 字节、`display_name` 走 `validate_display_name`、`admission_state`/`review_decision` 组合走 `crate::integrity::validate_device_fields`（`InvalidArgument` 统一映射 `read_error("devices.state")`）、`descriptor_json` 为 `None` → `None`、为 `Some` 时 `serde_json::from_str` 失败 → `read_error("devices.descriptor")`、`archived` 走 `raw_bool`（code `devices.archived`）；枚举映射固定：`PENDING/APPROVED/REVOKED` → `Pending/Approved/Revoked`、`none/approved/denied/revoked` → `None/Approved/Denied/Revoked`。admin 读路径与快照同事务层共用此解码）
    - `policy::eval_memory_permissions(actor: &SnapshotActor, device_id: [u8; 32], grants: &[GrantRecord], roles: &HashMap<Uuid, RoleRecord>, device_groups: &HashSet<Uuid>) -> HashSet<Permission>`（**纯求值**，离线 RED→GREEN 目标；`device_groups` = 目标设备**自身**所属 group 集。语义固定：`!actor.active || actor.must_change_password` → 空集（fail-closed，**admin 未改密同样是 0 权限**）；`actor.active && actor.is_admin` → `ALL_DEVICE_PERMISSIONS` 全量（admin shortcut，不看 grants）；普通用户 → `evaluate_grants(actor.id, device_id, grants, roles, device_groups)`。与 `AuthorizationSnapshot` 的组合约定：`eval_memory_permissions(&snapshot.actor, key, &snapshot.grants, &snapshot.roles, &snapshot.device_groups.get(&key).cloned().unwrap_or_default())`）
    - `devices::repository::project_device_detail(snapshot: &AuthorizationSnapshot, id: [u8; 32]) -> Result<Option<(DeviceAdminView, HashSet<Permission>)>, ControllerError>`（**单快照设备详情投影纯 helper**，离线 RED→GREEN 目标，闭合 I1 缺陷：同一已加载快照上同时完成授权与视图返回，无双 snapshot；语义固定：
      - DB 当前 actor 非活动或须改密（`!snapshot.actor.active || snapshot.actor.must_change_password`）→ 零权限且无视图，返回 `Ok(None)`（404，不泄露设备存在）；
      - 设备未在 snapshot.devices 中（missing）或已归档（`view.archived`）→ `Ok(None)`（404；当前 admin 对 missing/archived 同样无特权、不提前报 403 亦不泄露存在）；
      - 设备存在且未归档：求值 `perms = eval_memory_permissions(&snapshot.actor, id, &snapshot.grants, &snapshot.roles, &snapshot.device_groups.get(&id).cloned().unwrap_or_default())`；
      - 若 `perms.is_empty()` → `Ok(None)`（404，不可见与不存在同形）；
      - 若 `!perms.is_empty() && !perms.contains(&Permission::DeviceRead)` → `Err(ControllerError::PermissionDenied)`（403，持其他设备权限如 reboot-only 但缺 DeviceRead 时拒绝）；
      - 若 `perms.contains(&Permission::DeviceRead)` → `Ok(Some((view.clone(), perms)))`（200，克隆视图并返回同次计算所得权限，禁止双求值/假绿）。
      ）

审计复合游标完整定义（Task3附属增量；`AuditEventRow.process_epoch`必须保留，旧AUTH/CAS/ADMIN序列不合并）：

**Files:** Modify `crates/rsetup-controller/src/devices/repository.rs`，`crates/rsetup-controller/src/api/audit.rs`；Test 两文件各自单测。Consumes 既有`SessionAliasKey::new/sign_cursor`，不新增依赖、不改既有会话域。

- [ ] RED前置：先定义下列AuditCursor/SQL常量；`encode_audit_cursor`桩返回空字符串，`decode_audit_cursor`桩返回NotFound固定错误。目标测试`audit_cursor_binds_actor_filters_and_rejects_tampering`必须先因有权roundtrip失败而RED，不能缺类型/编译错误。
- [ ] GREEN：替换两个桩为如下实现。游标只是HTTP上下文认证，不改变审计时间、不宣称跨epoch绝对因果全序。新key每进程独立CSPRNG，重启旧cursor400；不复用session token或旧会话key实例。

```rust
// devices/repository.rs；Uuid与ControllerError沿用本模块import。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuditCursor {
    pub process_epoch: Uuid,
    pub event_seq: u64,
    pub id: Uuid,
}

pub(crate) const AUDIT_PAGE_SQL: &str = "SELECT id, actor_kind, actor_user_id, event_type, target_kind, target_id, params_redacted, outcome, process_epoch, event_seq, time_evidence AS created_evidence FROM audit_events WHERE (? IS NULL OR actor_user_id = ?) AND (? IS NULL OR (target_kind = 'device' AND target_id = ?)) AND (? IS NULL OR process_epoch > ? OR (process_epoch = ? AND (event_seq > ? OR (event_seq = ? AND id > ?)))) ORDER BY process_epoch, event_seq, id LIMIT ?";
```

Audit SQL固定11绑定，按次序：actor_filter两次（二进制16B/null）、device_filter两次（lowercase hex64/null）、cursor_epoch三次（二进制16B/null）、cursor_seq两次（u64/null）、cursor_id一次（二进制16B/null）、`limit+1`一次。首批cursor为None且对应六个cursor绑定全NULL；后批用三元键，不能用created_time或单seq。decode每行严格UUID字节长度、u64 seq>0与time_evidence非空/合法，污染固定去敏503；limit1..=200，实际读到额外一行才给next_cursor。使用同一受控授权读事务并重验actor当前管理员；先取limit+1，返回前limit条，next_cursor为最后已返回三元键，不是lookahead行。

```rust
// api/audit.rs；不修改SessionAliasKey实现，仅使用其公开sign_cursor方法。
use crate::auth::session::SessionAliasKey;
use crate::devices::repository::{AuditCursor, AuditFilter};
use crate::error::ControllerError;
use std::sync::OnceLock;
use uuid::Uuid;

static AUDIT_CURSOR_KEY: OnceLock<SessionAliasKey> = OnceLock::new();

fn audit_cursor_mac_input(actor: [u8; 16], filter: &AuditFilter, raw: &[u8]) -> Vec<u8> {
    let mut input = b"rsetup-management-audit-v1/epoch-seq-id".to_vec();
    input.extend_from_slice(&actor);
    match filter.actor_id {
        Some(id) => { input.push(1); input.extend_from_slice(&id); }
        None => input.push(0),
    }
    match filter.device_id {
        Some(id) => { input.push(1); input.extend_from_slice(&id); }
        None => input.push(0),
    }
    input.extend_from_slice(raw);
    input
}

fn encode_audit_cursor(key: &SessionAliasKey, actor: [u8; 16], filter: &AuditFilter, cursor: AuditCursor) -> String {
    let mut raw = Vec::with_capacity(72);
    raw.extend_from_slice(cursor.process_epoch.as_bytes());
    raw.extend_from_slice(&cursor.event_seq.to_be_bytes());
    raw.extend_from_slice(cursor.id.as_bytes());
    let tag = key.sign_cursor(&audit_cursor_mac_input(actor, filter, &raw));
    raw.extend_from_slice(&tag);
    hex::encode(raw)
}

fn decode_audit_cursor(key: &SessionAliasKey, actor: [u8; 16], filter: &AuditFilter, encoded: &str) -> Result<AuditCursor, ControllerError> {
    if encoded.len() != 144 || !encoded.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(ControllerError::InvalidArgument);
    }
    let raw = hex::decode(encoded).map_err(|_| ControllerError::InvalidArgument)?;
    let tag = key.sign_cursor(&audit_cursor_mac_input(actor, filter, &raw[..40]));
    let diff = tag.iter().zip(&raw[40..]).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if diff != 0 { return Err(ControllerError::InvalidArgument); }
    let process_epoch = Uuid::from_slice(&raw[..16]).map_err(|_| ControllerError::InvalidArgument)?;
    let event_seq = u64::from_be_bytes(raw[16..24].try_into().map_err(|_| ControllerError::InvalidArgument)?);
    let id = Uuid::from_slice(&raw[24..40]).map_err(|_| ControllerError::InvalidArgument)?;
    if event_seq == 0 || process_epoch.get_version_num() != 4 || id.get_version_num() != 4 {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(AuditCursor { process_epoch, event_seq, id })
}
```

handler使用`AUDIT_CURSOR_KEY.get_or_init(SessionAliasKey::new)`，请求中actor从已认证Session定位，不接受客户端actor；filter从严格query DTO解析并规范化，完整过滤参数传repo，next_cursor编码绑定同一actor/filter/排序/隐式audit端点。

```rust
// api/audit.rs tests；本例不依赖cfg(test)才公开的from_test_bytes，不向生产导出fake。
#[test]
fn audit_cursor_binds_actor_filters_and_rejects_tampering() {
    let key = SessionAliasKey::new();
    let filter = AuditFilter { actor_id: None, device_id: None };
    let cursor = AuditCursor { process_epoch: Uuid::new_v4(), event_seq: u64::MAX, id: Uuid::new_v4() };
    let token = encode_audit_cursor(&key, [1; 16], &filter, cursor);
    assert_eq!(token.len(), 144);
    assert_eq!(decode_audit_cursor(&key, [1; 16], &filter, &token).unwrap(), cursor);
    assert!(decode_audit_cursor(&key, [2; 16], &filter, &token).is_err());
    let changed = AuditFilter { actor_id: Some([3; 16]), device_id: None };
    assert!(decode_audit_cursor(&key, [1; 16], &changed, &token).is_err());
    let changed = AuditFilter { actor_id: None, device_id: Some([4; 32]) };
    assert!(decode_audit_cursor(&key, [1; 16], &changed, &token).is_err());
    let other_key = SessionAliasKey::new();
    assert!(decode_audit_cursor(&other_key, [1; 16], &filter, &token).is_err());
    let mut tampered = token.into_bytes();
    tampered[0] = if tampered[0] == b'0' { b'1' } else { b'0' };
    assert!(decode_audit_cursor(&key, [1; 16], &filter, std::str::from_utf8(&tampered).unwrap()).is_err());
}

// devices/repository.rs tests；三namespace同seq仍是三个稳定不同排序键。
#[test]
fn audit_keys_keep_namespaces_distinct_and_sql_has_exact_bindings() {
    let mut keys = vec![
        AuditCursor { process_epoch: Uuid::from_u128(0x00000000000040008000000000000003), event_seq: 7, id: Uuid::new_v4() },
        AuditCursor { process_epoch: Uuid::from_u128(0x00000000000040008000000000000001), event_seq: 7, id: Uuid::new_v4() },
        AuditCursor { process_epoch: Uuid::from_u128(0x00000000000040008000000000000002), event_seq: 7, id: Uuid::new_v4() },
    ];
    keys.sort();
    assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(keys.len(), 3);
    assert_eq!(AUDIT_PAGE_SQL.matches('?').count(), 11);
}
```

- [ ] 从设计根运行未来RED/GREEN命令：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller audit_cursor_binds_actor_filters_and_rejects_tampering`，恰好1方法，RED因roundtrip失败，GREEN因真实上下文绑定全过；第二个排序/绑定方法是schema契约回归，不自称其恒真实类型/SQL曾有新增行为RED。
- [ ] 双引擎真实分页门禁后在Task14 seed AUTH/CAS/ADMIN各两行同seq、分页limit=2连续三页，断言每id恰见一次/no skip/no duplicate；未实际运行时不写PASS。纯排序测试不等SQL实测。
- [ ] 条件提交只暂存上述两资源文件及实际接口消费者，中文描述；本轮仅计划不执行commit。

真实仓储类型/构造器与管理读事务前置（Task3，避免new()/db字段未定义）：

```rust
// authz/sqlx_repo.rs
pub struct SqlxAuthzRepository { pub(crate) db: DbPool }
impl SqlxAuthzRepository { pub fn new(db:DbPool)->Self{Self{db}} }
pub(crate) async fn verify_management_read(tx:&mut sqlx::Transaction<'_,sqlx::MySql>,actor:&crate::auth::service::Session)->Result<(),ControllerError>{
    use sqlx::Row;
    let rows=sqlx::query(SNAPSHOT_META_SQL).fetch_all(&mut **tx).await.map_err(|_|read_error("snapshot.epoch"))?;
    let raw=rows.iter().map(|row|->Result<(i64,i64,u64),ControllerError>{Ok((row.try_get("singleton")?,row.try_get("schema_version")?,row.try_get("authz_epoch")?))}).collect::<Result<Vec<_>,_>>()?;
    decode_snapshot_meta(&raw,crate::integrity::SUPPORTED_SCHEMA_VERSION)?;
    ensure_actor_admin_readonly(tx,actor).await
}
// devices/repository.rs，DbPool由crate::db导入。
pub struct SqlxDeviceRepository { pub(crate) db: DbPool }
impl SqlxDeviceRepository { pub fn new(db:DbPool)->Self{Self{db}} }
```

所有AuthzRepository管理只读方法：同连接RR lease→begin→verify_management_read→固定SQL/decode→commit；请求missing返回None，persisted引用缺失/解码污染固定Config503；不能把管理读helper借给普通device_page。类型/构造器先交付，再逐方法实际SQL实现，严禁fixture替代生产。

**可测性约束（必须遵守，否则会写出无法 RED 的测试）：** `sqlx::MySqlRow` 无公开构造函数，故本任务的行为 RED **只允许**落在上述纯解码/纯映射/分页/快照纯求值（`build_authorization_snapshot`、`eval_memory_permissions`、`project_device_detail`、`decode_device_admin_view`）函数与 SQL 绑定数断言上；任何需要真实驱动行的断言一律推迟到 Task 14 的 `#[ignore]` 真库测试。禁止为了"能测"而伪造可构造的驱动行，也禁止把 `#[ignore]` 当作本任务的 GREEN 证据。内部快照层的真 SQL 隔离/事务证据（单事务一致性、dangling member 真库制造、并发下 epoch 稳定）同样留在 Task 14，本任务不以 spy/fixture 宣称真库。

- [ ] **Step 1: 编写失败测试（纯解码/映射/分页 + 绑定数 + 内部快照纯求值与详情投影）**

先把纯函数写成**可编译但行为错误**的桩，然后写断言（以下测试必须在编译通过后因断言失败）。内部快照层的桩行为（供 Step 2 对照，全部"可编译但行为错误"）：`decode_snapshot_actor` 恒 `Ok(SnapshotActor { id: user_id, active: true, is_admin: false, must_change_password: false })`（行缺失/脏布尔都不拦）；`decode_snapshot_roles` 恒 `Ok(HashMap::new())`；`decode_snapshot_group_rows` 恒 `Ok(HashMap::new())`；`decode_device_admin_view` 恒 `Ok(DeviceAdminView { public_key: 解码后值, display_name: 入参, admission_state: Pending, review_decision: None, revision: 入参, descriptor: None, archived: false })`（合法行断言即失败）；`build_authorization_snapshot` 恒 `Err(ControllerError::InvalidArgument)`（不校验引用）；`eval_memory_permissions` 恒返回 `ALL_DEVICE_PERMISSIONS` 全量（不区分 admin/普通/改密/他人 grant）；`project_device_detail` 桩行为：可编译故意返回 `Ok(None)`（对有权设备断言失败，非恒拒绝假绿桩，可编译且必失败）。

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::policy::{GrantScope, GrantSource, Permission};
    use crate::devices::repository::{AUDIT_PAGE_SQL, DEVICE_PAGE_KEYS_FIRST_SQL, DEVICE_PAGE_KEYS_SQL}; // 设备侧固定 SQL 常量（pub(crate)），仅供本模块绑定数测试引用

    #[test]
    fn user_decode_fails_closed_on_polluted_boolean_and_username() {
        // 脏布尔（2）必须失败关闭，不得当作 true。
        assert!(decode_user(
            vec![1u8; 16], "alice".into(), "Alice".into(), 2, 1, 0, 1
        )
        .is_err());
        // 非规范用户名必须失败关闭。
        assert!(decode_user(
            vec![1u8; 16], "Alice".into(), "Alice".into(), 1, 1, 0, 1
        )
        .is_err());
        // 合法行必须成功且 revision 可到 u64 高位。
        let ok = decode_user(
            vec![1u8; 16], "alice".into(), "Alice".into(), 1, 0, 1, u64::MAX,
        )
        .expect("valid row must decode");
        assert_eq!(ok.active, true);
        assert_eq!(ok.is_admin, false);
        assert_eq!(ok.must_change_password, true);
        assert_eq!(ok.revision, u64::MAX);
    }

    #[test]
    fn page_cursor_reports_exactly_one_extra_row_and_never_leaks_total() {
        let rows = [1u8, 2, 3];
        // limit=2 时应返回 2 项并把第 2 项作为下一页游标。
        let (take, cursor) = page_cursor(&rows, 2);
        assert_eq!(take, 2);
        assert_eq!(cursor, Some(2u8));
        // 恰好取完时不得给出游标。
        let (take, cursor) = page_cursor(&rows, 3);
        assert_eq!(take, 3);
        assert_eq!(cursor, None);
    }

    #[test]
    fn grant_columns_pair_source_with_scope_strictly() {
        let role = uuid::Uuid::nil();
        let direct = grant_columns(
            &GrantSource::Direct(vec![Permission::DeviceRead]),
            &GrantScope::All,
        )
        .expect("direct/all is valid");
        assert_eq!(direct.source_kind, "direct");
        assert!(direct.role_id.is_none());
        assert_eq!(direct.permissions_json.as_deref(), Some(r#"["device.read"]"#));
        assert_eq!(direct.scope_kind, "all");
        assert!(direct.scope_group_id.is_none() && direct.scope_device_id.is_none());

        let by_role = grant_columns(&GrantSource::Role(role), &GrantScope::Group(role))
            .expect("role/group is valid");
        assert_eq!(by_role.source_kind, "role");
        assert_eq!(by_role.role_id, Some(role));
        assert!(by_role.permissions_json.is_none());
        assert_eq!(by_role.scope_kind, "group");
        assert_eq!(by_role.scope_group_id, Some(role));
    }

    #[test]
    fn list_sql_binds_are_pinned_to_cursor_and_limit() {
        // 每页查询恰好两个绑定：(cursor, limit)。绑定数与列顺序漂移必须在离线就暴露。
        assert_eq!(LIST_USERS_SQL.matches('?').count(), 2);
        assert_eq!(LIST_ROLES_SQL.matches('?').count(), 2);
        assert_eq!(LIST_GRANTS_SQL.matches('?').count(), 3); // (user_id, cursor, limit)
        assert_eq!(LIST_GROUPS_SQL.matches('?').count(), 2);
        assert_eq!(DEVICE_PAGE_KEYS_SQL.matches('?').count(), 2); // (cursor, limit+1)：候选公钥扫描，取代旧全列 DEVICE_PAGE_SQL
        assert_eq!(DEVICE_PAGE_KEYS_FIRST_SQL.matches('?').count(), 1);
        assert_eq!(AUDIT_PAGE_SQL.matches('?').count(), 11); // filter 4 + cursor 6 + limit 1
        // 内部快照层：固定 SQL 绑定数与模板占位符标记漂移必须离线暴露（占位符由同事务层按去重 id 数 1:1 展开，不拼进模板字面量）。
        assert_eq!(SNAPSHOT_META_SQL.matches('?').count(), 0);
        assert_eq!(LIST_ACTOR_GRANTS_SQL.matches('?').count(), 1); // (user_id)
        assert!(ROLES_BY_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(GROUPS_FOR_DEVICES_SQL_TEMPLATE.contains("IN ({placeholders})"));
        assert!(DEVICES_BY_IDS_SQL_TEMPLATE.contains("IN ({placeholders})"));
    }

    #[test]
    fn snapshot_actor_decode_fails_closed_on_polluted_boolean_and_missing_row() {
        // 行缺失 → PermissionDenied（与 admin helper 同形，不得当 NotFound 泄露账号存在性）。
        assert!(matches!(
            decode_snapshot_actor([1u8; 16], None),
            Err(ControllerError::PermissionDenied)
        ));
        // 脏布尔（2）→ 固定去敏 Config（503 形状），不得当 true。
        let polluted = decode_snapshot_actor([1u8; 16], Some((2, 1, 0))).unwrap_err();
        assert!(
            matches!(&polluted, ControllerError::Config(m) if m == "identity admin read snapshot.actor_active"),
            "dirty boolean must be the exact fixed redacted Config, got {polluted:?}",
        );
        // 合法行严格解码。
        let ok = decode_snapshot_actor([1u8; 16], Some((1, 1, 0)))
            .expect("valid actor row decodes");
        assert!(ok.active && ok.is_admin && !ok.must_change_password);
    }

    #[test]
    fn snapshot_roles_decode_fails_closed_on_duplicate_and_empty_permissions() {
        // 重复 role 行 / 被引用 role 的 role_permissions 行数为 0 = 污染 → 固定去敏 Config。
        let role = uuid::Uuid::from_bytes([0x7a; 16]);
        let role_bytes = role.as_bytes().to_vec();
        let duplicate = decode_snapshot_roles(
            &[(role_bytes.clone(), "viewer".into(), 1, 0, 1), (role_bytes.clone(), "viewer".into(), 1, 0, 1)],
            &[(role_bytes.clone(), "device.read".into())],
        )
        .unwrap_err();
        assert!(matches!(
            &duplicate,
            ControllerError::Config(m) if m == "identity admin read snapshot.roles"
        ));
        let no_perm = decode_snapshot_roles(
            &[(role_bytes.clone(), "viewer".into(), 1, 0, 1)],
            &[],
        )
        .unwrap_err();
        assert!(matches!(
            &no_perm,
            ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
        ));
        // 合法行解码：archived role 保留（是否生效由求值层判定），未知权限字符串失败关闭。
        let ok = decode_snapshot_roles(
            &[(role_bytes.clone(), "viewer".into(), 1, 1, 3)],
            &[(role_bytes.clone(), "device.read".into())],
        )
        .expect("valid role rows decode");
        let record = ok.get(&role).expect("referenced role present");
        assert!(record.builtin && record.archived && record.permissions == vec![Permission::DeviceRead]);
        let unknown_perm = decode_snapshot_roles(
            &[(role_bytes.clone(), "viewer".into(), 0, 0, 1)],
            &[(role_bytes.clone(), "device.fly".into())],
        )
        .unwrap_err();
        assert!(matches!(
            &unknown_perm,
            ControllerError::Config(m) if m == "identity admin read snapshot.role_permission"
        ));
    }

    #[test]
    fn snapshot_group_rows_decode_dangling_member_and_filter_archived() {
        // dangling member 行（LEFT JOIN 后 group 行缺失 → archived 为 None）= 污染 → 固定去敏 Config。
        let device = vec![9u8; 32];
        let group = uuid::Uuid::from_bytes([0x67; 16]);
        let group_bytes = group.as_bytes().to_vec();
        let dangling = decode_snapshot_group_rows(&[(device.clone(), group_bytes.clone(), None)]).unwrap_err();
        assert!(matches!(
            &dangling,
            ControllerError::Config(m) if m == "identity admin read snapshot.group_missing"
        ));
        // 合法行：archived group 被过滤（不入集合），未归档 group 保留；同设备多组取并集。
        let other_group = uuid::Uuid::from_bytes([0x68; 16]);
        let filtered = decode_snapshot_group_rows(&[
            (device.clone(), group_bytes.clone(), Some(1)), // archived
            (device.clone(), group_bytes.clone(), Some(0)), // 未归档
            (device, other_group.as_bytes().to_vec(), Some(0)),
        ])
        .expect("legal rows decode");
        assert_eq!(filtered.get(&[9u8; 32]), Some(&std::collections::HashSet::from([group, other_group])));
    }

    #[test]
    fn snapshot_device_decode_fails_closed_on_polluted_state_and_boolean() {
        // 合法设备行解码。
        let view = decode_device_admin_view(
            vec![5u8; 32], "dev-1".into(), None, "APPROVED", "approved", 3, 0,
        )
        .expect("legal device row decodes");
        assert_eq!(view.public_key, [5u8; 32]);
        assert_eq!(view.admission_state, crate::model::AdmissionState::Approved);
        assert_eq!(view.review_decision, crate::model::ReviewDecision::Approved);
        assert!(!view.archived);
        // 非法状态组合 / 脏布尔 / 脏 descriptor JSON 均失败关闭为固定去敏 Config。
        let bad_state = decode_device_admin_view(vec![5u8; 32], "dev-1".into(), None, "PENDING", "approved", 1, 0);
        assert!(matches!(&bad_state, Err(ControllerError::Config(_))));
        let bad_bool = decode_device_admin_view(vec![5u8; 32], "dev-1".into(), None, "PENDING", "none", 1, 2);
        assert!(matches!(&bad_bool, Err(ControllerError::Config(_))));
        let bad_descriptor = decode_device_admin_view(
            vec![5u8; 32], "dev-1".into(), Some("{not json".into()), "PENDING", "none", 1, 0,
        );
        assert!(matches!(&bad_descriptor, Err(ControllerError::Config(_))));
        // 公钥长度错误同样失败关闭。
        let bad_key = decode_device_admin_view(vec![5u8; 31], "dev-1".into(), None, "PENDING", "none", 1, 0);
        assert!(matches!(&bad_key, Err(ControllerError::Config(_))));
    }

    #[test]
    fn snapshot_build_fails_closed_on_missing_role_reference() {
        // 引用污染：grant 指向不在 roles 集的 role → 固定去敏 Config（503 形状），不得按"该 grant 无权限"继续。
        let missing_role = uuid::Uuid::from_bytes([0x77; 16]);
        let grants = vec![GrantRecord {
            id: uuid::Uuid::from_bytes([0x0c; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Role(missing_role),
            scope: GrantScope::All,
            revision: 1,
        }];
        let err = build_authorization_snapshot(
            SnapshotActor { id: [1u8; 16], active: true, is_admin: false, must_change_password: false },
            7,
            grants,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
        )
        .unwrap_err();
        assert!(
            matches!(&err, ControllerError::Config(m) if m == "identity admin read snapshot.role_ref"),
            "missing role reference must be the exact fixed redacted Config, got {err:?}",
        );
    }

    #[test]
    fn snapshot_build_passes_and_preserves_actor_grants_verbatim() {
        // 校验通过 → 原样组装（不排序/去重/丢弃），epoch 透传。
        let role = uuid::Uuid::from_bytes([0x7a; 16]);
        let grants = vec![GrantRecord {
            id: uuid::Uuid::from_bytes([0x0d; 16]),
            user_id: [1u8; 16],
            source: GrantSource::Role(role),
            scope: GrantScope::All,
            revision: 9,
        }];
        let mut roles = std::collections::HashMap::new();
        roles.insert(
            role,
            RoleRecord { id: role, name: "viewer".into(), builtin: true, archived: false, revision: 1, permissions: vec![Permission::DeviceRead] },
        );
        let mut device_groups = std::collections::HashMap::new();
        device_groups.insert([0xa1; 32], {
            let mut set = std::collections::HashSet::new();
            set.insert(uuid::Uuid::from_bytes([0x67; 16]));
            set
        });
        let snapshot = build_authorization_snapshot(
            SnapshotActor { id: [1u8; 16], active: true, is_admin: false, must_change_password: false },
            42,
            grants.clone(),
            roles,
            device_groups,
            std::collections::HashMap::new(),
        )
        .expect("valid inputs assemble");
        assert_eq!(snapshot.authz_epoch, 42);
        assert_eq!(snapshot.grants, grants);
        assert!(snapshot.grants.iter().all(|g| g.user_id == [1u8; 16]));
    }
}
```

```rust
// crates/rsetup-controller/src/authz/policy.rs（追加到既有 tests 模块；
// 既有 mod tests 已有 `use super::*;` 与 `use std::collections::{HashMap, HashSet};`，下列测试不新增 import）
#[test]
fn snapshot_eval_regular_user_group_scope_never_crosses_devices() {
    // 普通 group 范围不串设备：group 授权只覆盖**该设备自身所属**的 group——
    // device A 属于 G（有权限），device B 属于另一组（0 权限）；
    // 不得把 G 套到两个设备，也不得用 actor grant 的 group 全集。
    let group = uuid::Uuid::from_bytes([0x67; 16]);
    let actor = SnapshotActor { id: [1u8; 16], active: true, is_admin: false, must_change_password: false };
    let grants = vec![GrantRecord {
        id: uuid::Uuid::from_bytes([0x0a; 16]),
        user_id: [1u8; 16],
        source: GrantSource::Direct(vec![Permission::DeviceRead]),
        scope: GrantScope::Group(group),
        revision: 1,
    }];
    let roles = HashMap::new();
    let mut groups_a = HashSet::new();
    groups_a.insert(group);
    let mut groups_b = HashSet::new();
    groups_b.insert(uuid::Uuid::from_bytes([0x68; 16]));
    assert_eq!(
        eval_memory_permissions(&actor, [0xa1; 32], &grants, &roles, &groups_a),
        HashSet::from([Permission::DeviceRead])
    );
    assert!(
        eval_memory_permissions(&actor, [0xa2; 32], &grants, &roles, &groups_b).is_empty(),
        "device B is not in G: the group grant must not cross devices"
    );
}

#[test]
fn snapshot_eval_only_actor_own_grants_apply() {
    // 本人隔离：他人（即使 All scope 的 direct/reboot）grant 不给本 actor 任何权限。
    let actor = SnapshotActor { id: [1u8; 16], active: true, is_admin: false, must_change_password: false };
    let grants = vec![GrantRecord {
        id: uuid::Uuid::from_bytes([0x0b; 16]),
        user_id: [2u8; 16], // ← 他人的 grant
        source: GrantSource::Direct(vec![Permission::DeviceReboot]),
        scope: GrantScope::All,
        revision: 1,
    }];
    let roles = HashMap::new();
    let groups = HashSet::new();
    assert!(
        eval_memory_permissions(&actor, [0xb1; 32], &grants, &roles, &groups).is_empty(),
        "another user's grant must not grant permissions to this actor"
    );
}

#[test]
fn snapshot_eval_admin_must_change_password_has_zero_permissions() {
    // 管理员当前改密：DB 当前 must_change_password 的 admin 仍是 0 权限（fail-closed）；
    // admin shortcut 只在 DB 当前 active && is_admin && !must_change_password 时全量。
    let blocked = SnapshotActor { id: [1u8; 16], active: true, is_admin: true, must_change_password: true };
    let inactive = SnapshotActor { id: [1u8; 16], active: false, is_admin: true, must_change_password: false };
    let roles = HashMap::new();
    let groups = HashSet::new();
    assert!(eval_memory_permissions(&blocked, [0u8; 32], &[], &roles, &groups).is_empty());
    assert!(eval_memory_permissions(&inactive, [0u8; 32], &[], &roles, &groups).is_empty());
    let admin = SnapshotActor { id: [1u8; 16], active: true, is_admin: true, must_change_password: false };
    assert_eq!(
        eval_memory_permissions(&admin, [0u8; 32], &[], &roles, &groups),
        HashSet::from(ALL_DEVICE_PERMISSIONS)
    );
}

#[test]
fn snapshot_eval_no_grants_regular_user_has_legal_empty_permissions() {
    // 无 grant 是合法空权限（不是错误、不是 503、不降级为"未知"）——
    // 与"引用污染"（503 Config）严格区分：污染在 build/decode 层拦，空集在求值层放行。
    let actor = SnapshotActor { id: [1u8; 16], active: true, is_admin: false, must_change_password: false };
    let roles = HashMap::new();
    let groups = HashSet::new();
    assert!(eval_memory_permissions(&actor, [0xc1; 32], &[], &roles, &groups).is_empty());
}
```

```rust
// crates/rsetup-controller/src/devices/repository.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn device_visibility_requires_a_permission_for_the_specific_device() {
        // 纯判定函数：group 授权只覆盖该组内设备，device 授权只覆盖目标设备。
        let group = uuid::Uuid::from_bytes([3u8; 16]);
        let target = [7u8; 32];
        let other = [8u8; 32];
        let grants = vec![crate::authz::policy::GrantRecord {
            id: uuid::Uuid::nil(),
            user_id: [1u8; 16],
            source: crate::authz::policy::GrantSource::Direct(vec![
                crate::authz::policy::Permission::DeviceRead,
            ]),
            scope: crate::authz::policy::GrantScope::Group(group),
            revision: 1,
        }];
        let roles = std::collections::HashMap::new();
        let mut groups = HashSet::new();
        groups.insert(group);
        assert!(device_is_visible([1u8; 16], target, &grants, &roles, &groups, false));
        let mut empty = HashSet::new();
        empty_insert_nothing(&mut empty);
        assert!(!device_is_visible([1u8; 16], other, &grants, &roles, &empty, false));
        // 管理员永远可见全部设备。
        assert!(device_is_visible([2u8; 16], other, &[], &roles, &empty, true));
    }

    #[test]
    fn project_device_detail_table_driven_cases() {
        // 表驱动覆盖详情单快照投影全矩阵，无 undefined fixtures，禁止恒拒绝假绿
        use crate::authz::policy::{GrantScope, GrantSource, Permission, SnapshotActor};
        use crate::authz::sqlx_repo::AuthorizationSnapshot;
        use crate::devices::repository::DeviceAdminView;
        use crate::model::{AdmissionState, ReviewDecision};
        use std::collections::{HashMap, HashSet};

        let target_dev = [0x11u8; 32];
        let missing_dev = [0x99u8; 32];
        let archived_dev = [0x22u8; 32];

        let active_admin = SnapshotActor { id: [1u8; 16], active: true, is_admin: true, must_change_password: false };
        let active_user = SnapshotActor { id: [2u8; 16], active: true, is_admin: false, must_change_password: false };
        let inactive_user = SnapshotActor { id: [3u8; 16], active: false, is_admin: false, must_change_password: false };
        let must_change_user = SnapshotActor { id: [4u8; 16], active: true, is_admin: false, must_change_password: true };

        let make_view = |pk: [u8; 32], archived: bool| DeviceAdminView {
            public_key: pk,
            display_name: "dev-test".into(),
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::Approved,
            revision: 10,
            descriptor: None,
            archived,
        };

        let mut base_devices = HashMap::new();
        base_devices.insert(target_dev, make_view(target_dev, false));
        base_devices.insert(archived_dev, make_view(archived_dev, true));

        enum CaseExpect {
            OkNone,
            Forbidden,
            OkSome(HashSet<Permission>),
        }

        struct TestCase {
            name: &'static str,
            actor: SnapshotActor,
            grants: Vec<crate::authz::policy::GrantRecord>,
            query_id: [u8; 32],
            expect: CaseExpect,
        }

        let cases = vec![
            TestCase {
                name: "1. regular user read success returns Some(view, perms)",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::Device(target_dev),
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::OkSome([Permission::DeviceRead].into_iter().collect()),
            },
            TestCase {
                name: "2. regular user reboot-only returns 403 PermissionDenied",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceReboot]),
                    scope: GrantScope::Device(target_dev),
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::Forbidden,
            },
            TestCase {
                name: "3. regular user zero permission on target returns Ok(None) 404",
                actor: active_user,
                grants: vec![],
                query_id: target_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "4. missing target with all grant returns Ok(None) 404 without leaking existence",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead, Permission::DeviceReboot]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: missing_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "5. archived target with all grant returns Ok(None) 404 without leaking existence",
                actor: active_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead, Permission::DeviceReboot]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: archived_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "6. current admin missing target returns Ok(None) 404 without 403 or leaking",
                actor: active_admin,
                grants: vec![],
                query_id: missing_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "7. current admin archived target returns Ok(None) 404 without 403 or leaking",
                actor: active_admin,
                grants: vec![],
                query_id: archived_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "8. stale session admin degraded in snapshot to reboot-only returns 403 PermissionDenied",
                actor: active_user, // session was old admin, but current DB snapshot is regular user with reboot-only
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: active_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceReboot]),
                    scope: GrantScope::Device(target_dev),
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::Forbidden,
            },
            TestCase {
                name: "9. snapshot inactive actor with grant has zero view returns Ok(None) 404",
                actor: inactive_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: inactive_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "10. snapshot must_change_password actor with grant has zero view returns Ok(None) 404",
                actor: must_change_user,
                grants: vec![crate::authz::policy::GrantRecord {
                    id: uuid::Uuid::nil(),
                    user_id: must_change_user.id,
                    source: GrantSource::Direct(vec![Permission::DeviceRead]),
                    scope: GrantScope::All,
                    revision: 1,
                }],
                query_id: target_dev,
                expect: CaseExpect::OkNone,
            },
            TestCase {
                name: "11. current admin active on existing non-archived target returns Some with all permissions",
                actor: active_admin,
                grants: vec![],
                query_id: target_dev,
                expect: CaseExpect::OkSome(crate::authz::policy::ALL_DEVICE_PERMISSIONS.iter().copied().collect()),
            },
        ];

        for tc in cases {
            let snap = AuthorizationSnapshot {
                actor: tc.actor,
                authz_epoch: 1,
                grants: tc.grants,
                roles: HashMap::new(),
                device_groups: HashMap::new(),
                devices: base_devices.clone(),
            };
            let res = project_device_detail(&snap, tc.query_id);
            match tc.expect {
                CaseExpect::OkNone => {
                    assert!(matches!(res, Ok(None)), "case {} expected Ok(None), got {:?}", tc.name, res);
                }
                CaseExpect::Forbidden => {
                    assert!(matches!(res, Err(ControllerError::PermissionDenied)), "case {} expected PermissionDenied, got {:?}", tc.name, res);
                }
                CaseExpect::OkSome(expected_perms) => {
                    let (view, perms) = res.expect("must succeed").expect("must be Some");
                    assert_eq!(view.public_key, tc.query_id, "case {} view public_key mismatch", tc.name);
                    assert_eq!(perms, expected_perms, "case {} perms mismatch", tc.name);
                }
            }
        }
    }

    fn empty_insert_nothing(_set: &mut HashSet<uuid::Uuid>) {}
}
```

- [ ] **Step 2: 运行并确认行为 RED**

Rust 命令统一使用主工作区共享的 `CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home`（依赖缓存，只读复用）与设计树私有 `CARGO_TARGET_DIR=$PWD/.cargo-target`（`$PWD` = 设计树根目录；构建产物隔离，不污染主树）；`--offline --locked` 保持零新依赖。

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller user_decode_fails_closed_on_polluted_boolean_and_username
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller page_cursor_reports_exactly_one_extra_row_and_never_leaks_total
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller grant_columns_pair_source_with_scope_strictly
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller device_visibility_requires_a_permission_for_the_specific_device
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller project_device_detail_table_driven_cases
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller authz::sqlx_repo::tests::snapshot_
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller authz::policy::tests::snapshot_eval_
```
预期：前五条各运行恰好 1 个测试，且**因断言失败**：`decode_user` 桩恒 `Ok`（脏布尔未拦截）、`page_cursor` 桩恒返回 `(rows.len(), None)`、`grant_columns` 桩恒返回 `source_kind:"role"` 错配、`device_is_visible` 桩恒 `true`、`project_device_detail_table_driven_cases` 因 `project_device_detail` 桩恒返回 `Ok(None)` 在第 1 个有权用例处断言失败。第六条运行恰好 6 个快照层测试（`snapshot_actor_decode_*`、`snapshot_roles_decode_*`、`snapshot_group_rows_decode_*`、`snapshot_device_decode_*`、`snapshot_build_fails_closed_*`、`snapshot_build_passes_*`）且全部因断言失败（桩行为见 Step 1 前置说明）；第七条运行恰好 4 个快照求值测试（`snapshot_eval_regular_user_group_scope_never_crosses_devices`、`snapshot_eval_only_actor_own_grants_apply`、`snapshot_eval_admin_must_change_password_has_zero_permissions`、`snapshot_eval_no_grants_regular_user_has_legal_empty_permissions`）且全部因断言失败（`eval_memory_permissions` 桩恒全量 4 权限）。任何一条报编译错误或 0 用例都不得记为 RED。

- [ ] **Step 3: 最小实现（纯函数 + SQL + 只读方法）**

1. 纯函数：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs
pub fn decode_user(
    id: Vec<u8>,
    username: String,
    display_name: String,
    active: i64,
    is_admin: i64,
    must_change_password: i64,
    revision: u64,
) -> Result<UserRecord, ControllerError> {
    let id: [u8; 16] = id.try_into().map_err(|_| read_error("users.id"))?;
    if !crate::db::valid_username(&username) {
        return Err(read_error("users.username"));
    }
    validate_display_name(&display_name).map_err(|_| read_error("users.display_name"))?;
    Ok(UserRecord {
        id,
        username,
        display_name,
        active: crate::integrity::raw_bool(active, "users.active")?,
        is_admin: crate::integrity::raw_bool(is_admin, "users.is_admin")?,
        must_change_password: crate::integrity::raw_bool(
            must_change_password,
            "users.must_change_password",
        )?,
        revision,
    })
}

/// 分页：SQL 绑定 `limit + 1`，本函数只接受"是否多取到一行"的判定结果。
pub fn page_cursor<T: Copy + PartialEq>(rows: &[T], limit: usize) -> (usize, Option<T>) {
    if rows.len() > limit {
        (limit, Some(rows[limit - 1]))
    } else {
        (rows.len(), None)
    }
}

pub fn validate_display_name(value: &str) -> Result<(), ControllerError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || value.len() > 255
        || value.chars().any(|c| c.is_control())
    {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(())
}

/// source 与 scope 必须成对；`validate_grant_fields` 是唯一权威判定。
pub fn grant_columns(
    source: &GrantSource,
    scope: &GrantScope,
) -> Result<GrantColumns, ControllerError> {
    let (source_kind, role_id, permissions_json) = match source {
        GrantSource::Role(role_id) => ("role", Some(*role_id), None),
        GrantSource::Direct(permissions) => {
            let mut values: Vec<&'static str> = permissions.iter().map(|p| p.as_str()).collect();
            values.sort_unstable();
            values.dedup();
            if values.len() != permissions.len() || values.is_empty() {
                return Err(ControllerError::InvalidArgument);
            }
            (
                "direct",
                None,
                Some(serde_json::Value::Array(
                    values.iter().map(|p| serde_json::Value::from(*p)).collect(),
                )),
            )
        }
    };
    let permissions_value = permissions_json.as_ref();
    let (scope_kind, scope_group_id, scope_device_id) = match scope {
        GrantScope::All => ("all", None, None),
        GrantScope::Group(group_id) => ("group", Some(*group_id), None),
        GrantScope::Device(device_id) => ("device", None, Some(*device_id)),
    };
    crate::integrity::validate_grant_fields(
        source_kind,
        role_id.is_some(),
        permissions_value,
        scope_kind,
        scope_group_id.is_some(),
        scope_device_id.is_some(),
    )?;
    Ok(GrantColumns {
        source_kind,
        role_id,
        permissions_json: permissions_json.map(|value| value.to_string()),
        scope_kind,
        scope_group_id,
        scope_device_id,
    })
}

pub fn decode_grant(
    id: Vec<u8>,
    user_id: Vec<u8>,
    source_kind: String,
    role_id: Option<Vec<u8>>,
    permissions: Option<serde_json::Value>,
    scope_kind: String,
    scope_group_id: Option<Vec<u8>>,
    scope_device_id: Option<Vec<u8>>,
    revision: u64,
) -> Result<GrantRecord, ControllerError> {
    crate::integrity::validate_grant_fields(
        &source_kind,
        role_id.is_some(),
        permissions.as_ref(),
        &scope_kind,
        scope_group_id.is_some(),
        scope_device_id.is_some(),
    )?;
    let source = match source_kind.as_str() {
        "role" => GrantSource::Role(uuid_from(role_id)?),
        "direct" => {
            let mut parsed = Vec::new();
            for value in permissions
                .as_ref()
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| read_error("grants.permissions"))?
            {
                let text = value.as_str().ok_or_else(|| read_error("grants.permissions"))?;
                parsed.push(Permission::parse(text).ok_or_else(|| read_error("grants.permissions"))?);
            }
            GrantSource::Direct(parsed)
        }
        _ => return Err(read_error("grants.source")),
    };
    let scope = match scope_kind.as_str() {
        "all" => GrantScope::All,
        "group" => GrantScope::Group(uuid_from(scope_group_id)?),
        "device" => GrantScope::Device(device_from(scope_device_id)?),
        _ => return Err(read_error("grants.scope")),
    };
    Ok(GrantRecord {
        id: uuid_from(Some(id))?,
        user_id: user_id.try_into().map_err(|_| read_error("grants.user_id"))?,
        source,
        scope,
        revision,
    })
}

fn uuid_from(value: Option<Vec<u8>>) -> Result<uuid::Uuid, ControllerError> {
    let bytes: [u8; 16] = value
        .ok_or_else(|| read_error("grants.uuid"))?
        .try_into()
        .map_err(|_| read_error("grants.uuid"))?;
    Ok(uuid::Uuid::from_bytes(bytes))
}

fn device_from(value: Option<Vec<u8>>) -> Result<[u8; 32], ControllerError> {
    value
        .ok_or_else(|| read_error("grants.device_id"))?
        .try_into()
        .map_err(|_| read_error("grants.device_id"))
}

// ── 内部快照层纯解码/纯组装（同本模块 read_error，固定去敏 `identity admin read {code}`）──
// 输入均为 SQL 层 `row.try_get` 已取出的原始列值（无 SqlxRow 依赖，可离线 RED→GREEN）。

fn uuid16(value: Vec<u8>, code: &'static str) -> Result<Uuid, ControllerError> {
    let bytes: [u8; 16] = value.try_into().map_err(|_| read_error(code))?;
    Ok(uuid::Uuid::from_bytes(bytes))
}

/// actor 行严格解码：`row` 为 SQL 层取出的 `(active, is_admin, must_change_password)` 或 `None`。
/// 行缺失 → `PermissionDenied`（与 `ensure_actor_admin_readonly` 同形，不当 NotFound）；
/// 任一脏布尔 → 固定去敏 `Config`（503 形状），不短路跳过。
pub fn decode_snapshot_actor(
    user_id: [u8; 16],
    row: Option<(i64, i64, i64)>,
) -> Result<SnapshotActor, ControllerError> {
    let (active, is_admin, must_change_password) = row.ok_or(ControllerError::PermissionDenied)?;
    Ok(SnapshotActor {
        id: user_id,
        active: crate::integrity::raw_bool(active, "snapshot.actor_active")
            .map_err(|_| read_error("snapshot.actor_active"))?,
        is_admin: crate::integrity::raw_bool(is_admin, "snapshot.actor_is_admin")
            .map_err(|_| read_error("snapshot.actor_is_admin"))?,
        must_change_password: crate::integrity::raw_bool(must_change_password, "snapshot.actor_must_change")
            .map_err(|_| read_error("snapshot.actor_must_change"))?,
    })
}

/// role 行 + `role_permissions` 行纯解码：
/// `rows: (id, name, builtin, archived, revision)`、`permissions: (role_id, permission)`。
/// 重复role行、当前有效role空permissions、重复/未知权限串、脏布尔 = 固定去敏Config；
/// archived父行合法存在且permissions可因归档清理为空，不丢弃父行。
/// 全部 role 处理完后仍有未认领的 permission 行（dangling `role_permissions` 行）→ 同样污染。
/// archived role **保留**（是否生效由求值层判定，本层不做任何"丢弃"）。
pub fn decode_snapshot_roles(
    rows: &[(Vec<u8>, String, i64, i64, u64)],
    permissions: &[(Vec<u8>, String)],
) -> Result<HashMap<Uuid, RoleRecord>, ControllerError> {
    let mut perms_by_role: HashMap<Uuid, Vec<Permission>> = HashMap::new();
    for (role_id, name) in permissions {
        let rid = uuid16(role_id.clone(), "snapshot.role_permission")?;
        let permission = Permission::parse(name)
            .ok_or_else(|| read_error("snapshot.role_permission"))?;
        perms_by_role.entry(rid).or_default().push(permission);
    }
    let mut roles: HashMap<Uuid, RoleRecord> = HashMap::new();
    for (id, name, builtin, archived, revision) in rows {
        let rid = uuid16(id.clone(), "snapshot.roles")?;
        if roles.contains_key(&rid) {
            return Err(read_error("snapshot.roles")); // 重复 role 行 = 污染
        }
        let is_archived = crate::integrity::raw_bool(*archived, "snapshot.role_bool")
            .map_err(|_| read_error("snapshot.role_bool"))?;
        let permissions = perms_by_role.remove(&rid).unwrap_or_default();
        // 归档合法清理role_permissions后保留父行；仅当前有效角色要求非空。
        if (!is_archived && permissions.is_empty())
            || permissions.iter().copied().collect::<HashSet<_>>().len() != permissions.len()
        {
            return Err(read_error("snapshot.role_permission"));
        }
        roles.insert(
            rid,
            RoleRecord {
                id: rid,
                name: name.clone(),
                builtin: crate::integrity::raw_bool(*builtin, "snapshot.role_bool")
                    .map_err(|_| read_error("snapshot.role_bool"))?,
                archived: is_archived,
                revision: *revision,
                permissions,
            },
        );
    }
    if !perms_by_role.is_empty() {
        return Err(read_error("snapshot.role_permission")); // dangling permission 行
    }
    Ok(roles)
}

```

归档角色空权限专项增量（Task3，复用本节decode函数）：

```rust
// authz/sqlx_repo.rs tests内追加；policy类型用全路径，避免额外导入。
#[test]
fn archived_role_empty_permissions_are_legal_but_active_empty_is_corruption() {
    let id = Uuid::new_v4();
    let archived = vec![(id.as_bytes().to_vec(), "archived-role".to_string(), 0, 1, 1)];
    let roles = decode_snapshot_roles(&archived, &[]).expect("合法历史父行不能当污染");
    let role = roles.get(&id).expect("必须保留父行");
    assert!(role.archived);
    assert!(role.permissions.is_empty());
    let actor = crate::authz::policy::SnapshotActor {
        id: [1; 16], active: true, is_admin: false, must_change_password: false,
    };
    let grants = vec![crate::authz::policy::GrantRecord {
        id: Uuid::new_v4(), user_id: actor.id,
        source: crate::authz::policy::GrantSource::Role(id),
        scope: crate::authz::policy::GrantScope::All, revision: 1,
    }];
    assert!(crate::authz::policy::eval_memory_permissions(&actor, [2; 32], &grants, &roles, &HashSet::new()).is_empty());
    let active = vec![(id.as_bytes().to_vec(), "active-role".to_string(), 0, 0, 1)];
    assert!(matches!(decode_snapshot_roles(&active, &[]), Err(ControllerError::Config(_))));
    let unknown = vec![(id.as_bytes().to_vec(), "invalid.permission".to_string())];
    assert!(matches!(decode_snapshot_roles(&archived, &unknown), Err(ControllerError::Config(_))));
    let duplicate = vec![(id.as_bytes().to_vec(), "device.read".to_string()); 2];
    assert!(matches!(decode_snapshot_roles(&archived, &duplicate), Err(ControllerError::Config(_))));
    let dangling = vec![(Uuid::new_v4().as_bytes().to_vec(), "device.read".to_string())];
    assert!(matches!(decode_snapshot_roles(&archived, &dangling), Err(ControllerError::Config(_))));
}
```

先以本增量RED行为（归档空权限错误返回固定Config，保持签名可编译）运行`cargo test --offline --locked -p rsetup-controller archived_role_empty_permissions_are_legal_but_active_empty_is_corruption`，恰好1方法首合法正例失败；再用上方GREEN decode实现确认正反矩阵。该方法独立于原snapshot六方法选择器，必须另跑且另记实际数；本轮不运行，不把静态新增方法记PASS。

```rust
/// 每设备自身 group 集合纯解码：`rows: (device_id, group_id, group_archived)`
/// （`GROUPS_FOR_DEVICES_SQL_TEMPLATE` 的 LEFT JOIN 结果，`group_archived = None` 即 group 行缺失）。
/// dangling member 行 → 固定去敏 `Config`；archived group 过滤（不入集合）；同设备多组取并集。
pub fn decode_snapshot_group_rows(
    rows: &[(Vec<u8>, Vec<u8>, Option<i64>)],
) -> Result<HashMap<[u8; 32], HashSet<Uuid>>, ControllerError> {
    let mut out: HashMap<[u8; 32], HashSet<Uuid>> = HashMap::new();
    for (device_id, group_id, archived) in rows {
        let key: [u8; 32] = device_id
            .clone()
            .try_into()
            .map_err(|_| read_error("snapshot.device_id"))?;
        let gid = uuid16(group_id.clone(), "snapshot.group_missing")?;
        let archived = match archived {
            None => return Err(read_error("snapshot.group_missing")),
            Some(raw) => crate::integrity::raw_bool(*raw, "snapshot.group_archived")
                .map_err(|_| read_error("snapshot.group_archived"))?,
        };
        if !archived {
            out.entry(key).or_default().insert(gid);
        }
    }
    Ok(out)
}

/// 设备行纯解码（admin 读路径与快照同事务层共用）；枚举映射与状态组合规则见 Interfaces 纯函数清单。
pub fn decode_device_admin_view(
    public_key: Vec<u8>,
    display_name: String,
    descriptor_json: Option<String>,
    admission_state: &str,
    review_decision: &str,
    revision: u64,
    archived: i64,
) -> Result<DeviceAdminView, ControllerError> {
    let key: [u8; 32] = public_key.try_into().map_err(|_| read_error("devices.public_key"))?;
    validate_display_name(&display_name).map_err(|_| read_error("devices.display_name"))?;
    crate::integrity::validate_device_fields(admission_state, review_decision)
        .map_err(|_| read_error("devices.state"))?;
    let admission_state = match admission_state {
        "PENDING" => AdmissionState::Pending,
        "APPROVED" => AdmissionState::Approved,
        "REVOKED" => AdmissionState::Revoked,
        _ => return Err(read_error("devices.state")),
    };
    let review_decision = match review_decision {
        "none" => ReviewDecision::None,
        "approved" => ReviewDecision::Approved,
        "denied" => ReviewDecision::Denied,
        "revoked" => ReviewDecision::Revoked,
        _ => return Err(read_error("devices.state")),
    };
    let descriptor = match descriptor_json {
        None => None,
        Some(text) => Some(
            serde_json::from_str::<serde_json::Value>(&text)
                .map_err(|_| read_error("devices.descriptor"))?,
        ),
    };
    Ok(DeviceAdminView {
        public_key: key,
        display_name,
        admission_state,
        review_decision,
        revision,
        descriptor,
        archived: crate::integrity::raw_bool(archived, "devices.archived")
            .map_err(|_| read_error("devices.archived"))?,
    })
}

/// DTO 组装 + 引用污染校验（唯一 DTO 一致性校验点）：
/// `grants` 中每个 `GrantSource::Role(rid)` 必须命中 `roles`（缺失 → `Config` 固定去敏，
/// 不得按"该 grant 无权限"继续）；通过 → 原样组装，不做任何其他规范化（不排序、不去重、不丢弃）。
pub fn build_authorization_snapshot(
    actor: SnapshotActor,
    authz_epoch: u64,
    grants: Vec<GrantRecord>,
    roles: HashMap<Uuid, RoleRecord>,
    device_groups: HashMap<[u8; 32], HashSet<Uuid>>,
    devices: HashMap<[u8; 32], DeviceAdminView>,
) -> Result<AuthorizationSnapshot, ControllerError> {
    for grant in &grants {
        if let GrantSource::Role(rid) = &grant.source {
            if !roles.contains_key(rid) {
                return Err(read_error("snapshot.role_ref"));
            }
        }
    }
    Ok(AuthorizationSnapshot {
        actor,
        authz_epoch,
        grants,
        roles,
        device_groups,
        devices,
    })
}
```

1a. `policy.rs` 纯条目（追加到该文件末尾；policy 是纯叶子，不依赖 DB/serde_json。所需 `HashMap/HashSet/Uuid/Permission/GrantRecord/RoleRecord/evaluate_grants` 在该模块内或经既有 import 可见；`Uuid` 若未 import 则一律用全路径 `uuid::Uuid`）：

```rust
// crates/rsetup-controller/src/authz/policy.rs（追加）

/// DB 当前 actor 的严格授权状态（同一读事务内读取；HTTP 层 `Session` 只是廉价前置过滤）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapshotActor {
    pub id: [u8; 16],
    pub active: bool,
    pub is_admin: bool,
    pub must_change_password: bool,
}

/// 封闭的 4 权限全集（规格 01 §1：管理员具有全局及全部设备权限）。
pub const ALL_DEVICE_PERMISSIONS: [Permission; 4] = [
    Permission::DeviceRead,
    Permission::DeviceStatusRead,
    Permission::DeviceReboot,
    Permission::DeviceTaskRead,
];

/// DB 当前管理员判定（admin shortcut 条件）：三个严格布尔全部满足。
pub fn is_current_admin(actor: &SnapshotActor) -> bool {
    actor.active && actor.is_admin && !actor.must_change_password
}

/// 目标设备的有效权限集（纯求值，离线 RED→GREEN 目标）。
/// 语义固定：`!active || must_change_password` → 空集（fail-closed，admin 未改密同样 0 权限）；
/// `active && is_admin` → `ALL_DEVICE_PERMISSIONS` 全量（admin shortcut，不看 grants）；
/// 普通用户 → `evaluate_grants(actor.id, device_id, grants, roles, device_groups)`，
/// 其中 `device_groups` 必须是**目标设备自身**所属 group 集（不得用 actor grant 的 group 全集）。
pub fn eval_memory_permissions(
    actor: &SnapshotActor,
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
) -> HashSet<Permission> {
    if !actor.active || actor.must_change_password {
        return HashSet::new();
    }
    if actor.is_admin {
        return HashSet::from(ALL_DEVICE_PERMISSIONS);
    }
    evaluate_grants(actor.id, device_id, grants, roles, device_groups)
}
```

2. `read_error` / `write_error` 使用与既有 `auth/sqlx_repo.rs` 同构的固定去敏 `Config` 文案（`identity admin read {code}` / `identity admin write {code}`），**不携带行内容**。
3. 固定 SQL 常量（不拼接字符串）：每类实体一对 `LIST_*_SQL` / `LIST_*_FIRST_SQL`，绑定 `?` 只有 `(cursor, limit)`；`LIMIT` 绑定 `limit + 1` 以判定下一页；`WHERE id > ?` 依赖 `BINARY(16)` 字节序，正好等于规格要求的 UUID 字节序。
4. 每个读方法骨架：`let mut tx = db.0.begin().await?` → `ensure_actor_admin_readonly(&mut tx, actor).await?` → 分页查询 → `tx.commit().await?`。`ensure_actor_admin_readonly`（完整定义见本步第 7 条，不留 undefined fn）用 `SELECT CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password FROM users WHERE id = ?` + `raw_bool` 严格判定；非管理员/停用/必须改密/行缺失一律 `PermissionDenied`（行缺失不得当 NotFound，避免泄露账号是否存在）。
5. `roles_by_id` 用 `WHERE id IN (?, ...)` 逐 id 构造固定数量占位符（ids 来自已读页，去重后 ≤200，占位符数量与去重后 ids 一一对应）；空 ids 只有在 actor 授权检查通过后才短路 `Ok(HashMap::new())`——不拼 `IN ()` 空占位列表，也不以"空就跳过"绕过认证：

```rust
let mut tx = db.0.begin().await?;
ensure_actor_admin_readonly(&mut tx, actor).await?; // 先过 actor 检查，空 ids 也不例外
if ids.is_empty() { tx.commit().await?; return Ok(HashMap::new()); } // 授权后提交再短路，不生成 SQL
```

即 `begin → actor 检查 → 空 ids 提交短路` 的三行顺序；此处仍是 admin 层方法，普通用户内部 snapshot（内部 `AuthorizationSnapshotRepository`）与 admin CRUD 的分层由**本步第 8 条**闭合（普通用户经 `load_actor_authorization_in_tx`/`load_device_snapshot_in_tx` 同事务核心读取，**不改调**本条的 admin helper）——这里不得借空 ids 短路放开 admin 检查（即放开管理员列表）。`list_roles`/`list_grants` 的角色权限集合按 role_id 逐行查询 `role_permissions`。
6. `device_page` / `find_device` 只读方法（`devices/repository.rs`，**本轮修订：admin/普通用户统一走内部分片快照，普通分支不得调用 `ensure_actor_admin_readonly`**）：

```rust
pub fn device_is_visible(
    user_id: [u8; 16],
    device_id: [u8; 32],
    grants: &[GrantRecord],
    roles: &HashMap<Uuid, RoleRecord>,
    device_groups: &HashSet<Uuid>,
    is_admin: bool,
) -> bool {
    if is_admin {
        return true;
    }
    !crate::authz::policy::evaluate_grants(user_id, device_id, grants, roles, device_groups).is_empty()
}
```

`device_page`完整可见分页核心（单个受控同连接RR读事务；actor级授权只读一次；实际extra-visible而不是page.len决定next_cursor）：

```rust
// devices/repository.rs；DeviceAdminView/Permission/RepoFuture/Session/SqlxDeviceRepository
// 按Task3本模块定义/import；低层snapshot helpers为authz/sqlx_repo pub(crate)。
struct VisibleDevicePage {
    limit: usize,
    items: Vec<(DeviceAdminView, HashSet<Permission>)>,
    extra_visible: bool,
}
impl VisibleDevicePage {
    fn new(limit: usize) -> Result<Self, ControllerError> {
        if !(1..=200).contains(&limit) { return Err(ControllerError::InvalidArgument); }
        Ok(Self { limit, items: Vec::new(), extra_visible: false })
    }
    fn observe(&mut self, view: DeviceAdminView, permissions: HashSet<Permission>) {
        if view.archived || permissions.is_empty() { return; }
        if self.items.len() < self.limit {
            self.items.push((view, permissions));
        } else {
            self.extra_visible = true;
        }
    }
    fn finish(self) -> (Vec<(DeviceAdminView, HashSet<Permission>)>, Option<[u8; 32]>) {
        let next = if self.extra_visible { self.items.last().map(|(view, _)| view.public_key) } else { None };
        (self.items, next)
    }
}

// impl DeviceRepository for SqlxDeviceRepository 内的方法，替换旧伪代码。
fn device_page<'a>(
    &'a self, actor: &'a Session, cursor: Option<[u8; 32]>, limit: usize,
) -> RepoFuture<'a, (Vec<(DeviceAdminView, HashSet<Permission>)>, Option<[u8; 32]>)> {
    Box::pin(async move {
        let mut page = VisibleDevicePage::new(limit)?;
        let mut connection = crate::authz::sqlx_repo::acquire_authorization_read_connection(&self.db).await?;
        let mut tx = sqlx::Connection::begin(&mut *connection).await?;
        let auth = crate::authz::sqlx_repo::load_actor_authorization_in_tx(&mut tx, actor.user.id).await?;
        // load_actor_authorization_in_tx已严格校验role/scope引用，分页拆分不绕过build契约。
        let scan_limit = u64::try_from(limit + 1).map_err(|_| ControllerError::InvalidArgument)?;
        let mut after = cursor;
        loop {
            let raw_keys = match after {
                Some(key) => sqlx::query_scalar::<_, Vec<u8>>(DEVICE_PAGE_KEYS_SQL)
                    .bind(key.as_slice()).bind(scan_limit).fetch_all(&mut *tx).await?,
                None => sqlx::query_scalar::<_, Vec<u8>>(DEVICE_PAGE_KEYS_FIRST_SQL)
                    .bind(scan_limit).fetch_all(&mut *tx).await?,
            };
            if raw_keys.is_empty() { break; }
            let keys = raw_keys.into_iter().map(|raw| {
                <[u8; 32]>::try_from(raw).map_err(|_| ControllerError::Config("identity admin read snapshot.device_id".into()))
            }).collect::<Result<Vec<_>, _>>()?;
            let (groups, devices) = crate::authz::sqlx_repo::load_device_snapshot_in_tx(&mut tx, &keys).await?;
            for key in &keys {
                let Some(view) = devices.get(key) else { continue; };
                let permissions = crate::authz::policy::eval_memory_permissions(
                    &auth.actor, *key, &auth.grants, &auth.roles,
                    &groups.get(key).cloned().unwrap_or_default(),
                );
                page.observe(view.clone(), permissions); // 一次求值同时供可见性及响应
                if page.extra_visible { break; }
            }
            if page.extra_visible || keys.len() < limit + 1 { break; }
            after = keys.last().copied(); // 以最后扫描候选推进，跨隐藏批次继续寻找lookahead
        }
        tx.commit().await?;
        Ok(page.finish())
    })
}
```

内部RED前置`VisibleDevicePage::finish`故意仅按items.len==limit产生next，不查看extra_visible；函数签名及observe/new都完整，不以缺函数失败。GREEN用上方finish替换。测试追加到devices/repository.rs tests内：

```rust
#[test]
fn visible_device_page_cursor_requires_actual_extra_visible_across_batches() {
    let make = |id: u8, archived: bool| DeviceAdminView {
        public_key: [id; 32], display_name: "synthetic".into(),
        admission_state: crate::model::AdmissionState::Approved,
        review_decision: crate::model::ReviewDecision::Approved,
        revision: 1, descriptor: None, archived,
    };
    let cases = [
        (vec![vec![(1, true, false), (2, true, false)]], None), // exact limit无下一项
        (vec![vec![(1, true, false), (2, true, false), (3, false, false)]], None),
        (vec![vec![(1, true, false), (2, true, false), (3, true, true)]], None),
        (vec![vec![(1, true, false), (2, true, false)], vec![(3, false, false)], vec![(4, false, false)]], None),
        (vec![vec![(1, true, false), (2, true, false)], vec![(3, false, false)], vec![(4, true, false)]], Some([2; 32])),
    ];
    for (batches, expected) in cases {
        let mut page = VisibleDevicePage::new(2).unwrap();
        for batch in batches {
            for (id, visible, archived) in batch {
                let permissions = if visible { HashSet::from([Permission::DeviceRead]) } else { HashSet::new() };
                page.observe(make(id, archived), permissions);
            }
        }
        let (items, next) = page.finish();
        assert_eq!(items.len(), 2);
        assert_eq!(next, expected);
    }
}
```

未来单方法选择器`cargo test --offline --locked -p rsetup-controller visible_device_page_cursor_requires_actual_extra_visible_across_batches`，RED首exact-limit期望None但错误Some失败，GREEN五场景过。纯accumulator测试不证明SQL扫描/隔离/过滤；Task14实际seed两可见+多批隐藏尾和末批额外可见，调用真实仓储分别断言None/Some，并核第二页没有重漏。group_id/search筛选必须在对应仓储接口补齐后传入同次候选扫描，不能把本无筛选基础方法冒称已覆盖过滤。

契约不变式：① 每个候选**恰好求值一次**（`eval_memory_permissions` 结果同时供响应与可见性判定，不重复求值）；② 每候选的 group 集是**该设备自身所属**集合（`groups.get(&key)`），禁止把 actor grant 出现过的 group 全集套到所有设备；③ `next_cursor = Some` 当且仅当"页满且其后确有可见设备"（由 `extra_visible_after_page` 确认，**禁止** `COUNT(*)`）；④ 当前设备授权列表过滤archived设备，admin shortcut不能重新使已归档设备获得当前授权；历史父行仍保留供审计/任务引用，不能把“父行存在”当当前有效。⑤ actor 级数据（actor 行/epoch/grants/roles）**整个事务只读一次**，后续候选批次只追加设备部分（新 key 集是不同数据，不是对同一快照的二次 query）；⑥ 任一解码/引用污染 → 固定去敏 `Config`（503 形状）整体回滚，不降级空页。`device_is_visible` 保留（签名不变）：列表与测试 oracle 的可复用纯判定。`minimum_device_projection` 只在 handler 层用于列表投影。

`project_device_detail` 纯实现（`devices/repository.rs`）：

导入按目标模块去重：`devices/repository.rs` 顶层需 `std::collections::HashSet`、`crate::authz::policy::{Permission, eval_memory_permissions}`、`crate::authz::sqlx_repo::{AuthorizationSnapshot, load_snapshot_in_tx}`、`crate::error::ControllerError`、`crate::auth::service::Session`；`DeviceAdminView`/`RepoFuture` 本模块定义，不重复导入。测试函数内的 SnapshotActor/GrantSource/GrantScope、model 枚举和 HashMap/HashSet 已在测试块明确；复用已有 use，不复制同名导入。sqlx_repo 中 DeviceAdminView 从 devices::repository 导入，AdmissionState/ReviewDecision 从 model 导入；不把新增模块描述为现有源码已交付。


```rust
pub fn project_device_detail(
    snapshot: &AuthorizationSnapshot,
    id: [u8; 32],
) -> Result<Option<(DeviceAdminView, HashSet<Permission>)>, ControllerError> {
    if !snapshot.actor.active || snapshot.actor.must_change_password {
        return Ok(None);
    }
    let view = match snapshot.devices.get(&id) {
        Some(v) if !v.archived => v,
        _ => return Ok(None),
    };
    let device_groups = snapshot.device_groups.get(&id).cloned().unwrap_or_default();
    let perms = eval_memory_permissions(
        &snapshot.actor,
        id,
        &snapshot.grants,
        &snapshot.roles,
        &device_groups,
    );
    if perms.is_empty() {
        Ok(None)
    } else if !perms.contains(&Permission::DeviceRead) {
        Err(ControllerError::PermissionDenied)
    } else {
        Ok(Some((view.clone(), perms)))
    }
}
```

`find_device` 实现（**同连接受控RR读事务**，先配置只读预检再begin；字符串预检不是实际隔离证据，双引擎barrier仍须门禁后验）：

```rust
// SqlxDeviceRepository::find_device
fn find_device<'a>(
    &'a self,
    actor: &'a Session,
    id: [u8; 32],
) -> RepoFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>> {
    Box::pin(async move {
        let mut connection = crate::authz::sqlx_repo::acquire_authorization_read_connection(&self.db).await?;
        let mut tx = sqlx::Connection::begin(&mut *connection).await?;
        let snapshot = match load_snapshot_in_tx(&mut tx, actor.user.id, &[id]).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        let projected = match project_device_detail(&snapshot, id) {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e);
            }
        };
        tx.commit().await?;
        Ok(projected)
    })
}
```

契约说明：一次 `begin` → `load_snapshot_in_tx(actor.user.id, &[id])` → `project_device_detail` → `commit`。在一次快照中同时授权并传 view/perms；begin 失败无事务可回滚，load/project 失败显式 rollback，commit 消费 tx 后若失败则返回错误并由 SQLx Transaction Drop best-effort 回滚，不在 commit 后访问已移动的 tx；任何失败都不返回详情数据。不再另调 `effective_permissions` 或二次读库；actor 行缺失统一由 `load_snapshot_in_tx` 内部映射为 `PermissionDenied`，保持认证错误合同。

7. 只读 actor 门禁 helper 完整定义（不留 undefined fn）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs 本模块内定义（与本步第 2 条的 read_error 同模块）；
// devices/repository.rs 如需复用，经 crate::authz::sqlx_repo::ensure_actor_admin_readonly 调用（pub(crate)）。
// imports 来源说明（不重复影子 Session/User，签名内不新增 use）：
// - sqlx::Row：sqlx crate 的 Row trait，为 row.try_get 提供方法；本模块需 `use sqlx::Row;`（MySqlRow 已实现该 trait，不另造类型）。
// - ControllerError：crate::error::ControllerError，本模块既有 import（decode_user 等已使用），不新增。
// - Session：参数签名用全路径 crate::auth::service::Session，不新增 use，避免与本模块既有 Session/User 导入重复影子。
// - raw_bool：crate::integrity::raw_bool，真实签名 fn(raw: i64, code: &'static str) -> Result<bool, ControllerError>
//   （integrity.rs:518，当前私有，本任务已要求提升 pub(crate)），不另造副本。
const ACTOR_READONLY_SQL: &str = "SELECT CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin, CAST(must_change_password AS SIGNED) AS must_change_password FROM users WHERE id = ?";

pub(crate) async fn ensure_actor_admin_readonly(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor: &crate::auth::service::Session,
) -> Result<(), ControllerError> {
    let row = sqlx::query(ACTOR_READONLY_SQL)
        .bind(actor.user.id.as_slice())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| read_error("users.actor"))?; // 查询错误固定去敏，不携带行
    let row = row.ok_or(ControllerError::PermissionDenied)?; // 行缺失一律 PermissionDenied，不当 NotFound
    // 三个 raw_bool 分开先解析再合并，避免 && 短路略过被污染字段
    let active = crate::integrity::raw_bool(
        row.try_get::<i64, _>("active").map_err(|_| read_error("users.active"))?,
        "users.active",
    )?;
    let is_admin = crate::integrity::raw_bool(
        row.try_get::<i64, _>("is_admin").map_err(|_| read_error("users.is_admin"))?,
        "users.is_admin",
    )?;
    let must_change_password = crate::integrity::raw_bool(
        row.try_get::<i64, _>("must_change_password").map_err(|_| read_error("users.must_change_password"))?,
        "users.must_change_password",
    )?;
    if active && is_admin && !must_change_password {
        Ok(())
    } else {
        Err(ControllerError::PermissionDenied)
    }
}
```

8. **内部快照层 SQL 常量 + 同事务低层 helper + `SqlxAuthorizationSnapshotRepository`（完整定义，不留 undefined fn；C1 闭合的实现体）**：

```rust
// ── authz/sqlx_repo.rs：内部快照层 SQL 常量（固定、不拼接字符串；占位符 1:1 展开规则见下方说明）──
const SNAPSHOT_META_SQL: &str =
    "SELECT CAST(singleton AS SIGNED) AS singleton, CAST(schema_version AS SIGNED) AS schema_version, authz_epoch FROM schema_meta LIMIT 2";

// 同一物理连接先只读确认RR，再在该连接begin；不改变服务器/会话隔离配置。
pub(crate) async fn acquire_authorization_read_connection(db: &DbPool)
    -> Result<sqlx::pool::PoolConnection<sqlx::MySql>, ControllerError>
{
    let mut connection = db.0.acquire().await.map_err(|_| read_error("snapshot.tx"))?;
    let isolation: String = sqlx::query_scalar("SELECT @@session.transaction_isolation")
        .fetch_one(&mut *connection).await.map_err(|_| read_error("snapshot.isolation"))?;
    require_snapshot_isolation(&isolation)?;
    Ok(connection)
}

pub fn require_snapshot_isolation(isolation: &str) -> Result<(), ControllerError> {
    // MySQL/TiDB已确认模式以受控同连接RR事务读取；未知/RC/无法读配置一律503。
    if isolation == "REPEATABLE-READ" || isolation == "REPEATABLE READ" {
        Ok(())
    } else {
        Err(read_error("snapshot.isolation"))
    }
}

pub fn decode_snapshot_meta(rows: &[(i64, i64, u64)], expected_version: i32)
    -> Result<u64, ControllerError>
{
    match rows {
        [(1, version, epoch)] if *version == i64::from(expected_version) => Ok(*epoch),
        _ => Err(read_error("snapshot.epoch")),
    }
}
const LIST_ACTOR_GRANTS_SQL: &str =
    "SELECT g.id, g.user_id, g.source_kind, g.role_id, g.permissions, g.scope_kind, \
     g.scope_group_id, g.scope_device_id, g.revision, \
     CAST(u.id IS NOT NULL AS SIGNED) AS owner_exists, \
     CAST(r.id IS NOT NULL AS SIGNED) AS role_exists, \
     CAST(dg.id IS NOT NULL AS SIGNED) AS group_exists, \
     CAST(d.public_key IS NOT NULL AS SIGNED) AS device_exists \
     FROM grants g LEFT JOIN users u ON u.id = g.user_id \
     LEFT JOIN roles r ON r.id = g.role_id \
     LEFT JOIN device_groups dg ON dg.id = g.scope_group_id \
     LEFT JOIN devices d ON d.public_key = g.scope_device_id \
     WHERE g.user_id = ? ORDER BY g.id";
const ROLES_BY_IDS_SQL_TEMPLATE: &str =
    "SELECT id, name, CAST(builtin AS SIGNED) AS builtin, \
     CAST(archived AS SIGNED) AS archived, revision \
     FROM roles WHERE id IN ({placeholders}) ORDER BY id";
const ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE: &str =
    "SELECT role_id, permission FROM role_permissions \
     WHERE role_id IN ({placeholders}) ORDER BY role_id, permission";
const GROUPS_FOR_DEVICES_SQL_TEMPLATE: &str =
    "SELECT gm.device_id, gm.group_id, CAST(dg.archived AS SIGNED) AS group_archived, \
     CAST(d.public_key IS NOT NULL AS SIGNED) AS member_device_exists \
     FROM group_members gm LEFT JOIN device_groups dg ON dg.id = gm.group_id \
     LEFT JOIN devices d ON d.public_key = gm.device_id \
     WHERE gm.device_id IN ({placeholders}) ORDER BY gm.device_id, gm.group_id";
const DEVICES_BY_IDS_SQL_TEMPLATE: &str =
    "SELECT public_key, display_name, descriptor_json, admission_state, \
     review_decision, revision, CAST(archived AS SIGNED) AS archived \
     FROM devices WHERE public_key IN ({placeholders}) ORDER BY public_key";

// ── devices/repository.rs：候选公钥扫描（keyset；BINARY(32) 字节序 = 规格要求的设备 ID 序）──
// （本常量与 `AUDIT_PAGE_SQL` 同为 `pub(crate)`：`authz/sqlx_repo.rs` 测试模块的绑定数测试经 use 引用。）
// 取代旧全列版 DEVICE_PAGE_SQL：候选扫描只取 public_key，全列统一经
// DEVICES_BY_IDS_SQL_TEMPLATE 在同事务内读取（同一事务同刻，无二次快照问题）。
pub(crate) const DEVICE_PAGE_KEYS_FIRST_SQL: &str =
    "SELECT public_key FROM devices ORDER BY public_key LIMIT ?";
pub(crate) const DEVICE_PAGE_KEYS_SQL: &str =
    "SELECT public_key FROM devices WHERE public_key > ? ORDER BY public_key LIMIT ?";
```

   - `{placeholders}` = 固定数量 `?` 占位符串（如 `"?, ?"`），由同事务层用 `vec!["?"; n].join(", ")` 构造，**与去重 id 数 1:1**；空集必须在同事务层短路，**不得**拼进 SQL（禁 `IN ()`）。
   - 解码链（SQL 层只做 `row.try_get` 取原始列值，语义判定全部落在上面第 1 条的纯函数里，离线可测）：
     - `SNAPSHOT_META_SQL`：0绑定；query取最多2行，严格decode singleton/version/u64 epoch并交decode_snapshot_meta，不使用query_scalar忽略额外元数据。
     - `LIST_ACTOR_GRANTS_SQL`：1 绑定 `(actor_id 字节)`；每行 `try_get` 出 `(Vec<u8>, Vec<u8>, String, Option<Vec<u8>>, Option<serde_json::Value>, String, Option<Vec<u8>>, Option<Vec<u8>>, u64)` → 既有 `decode_grant`，错误统一 `.map_err(|_| read_error("snapshot.grants"))`。
     - `ROLES_BY_IDS_SQL_TEMPLATE`：绑定 = 去重 role id 字节串；每行 → `(Vec<u8>, String, i64, i64, u64)`。
     - `ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE`：同绑定；每行 → `(Vec<u8>, String)`。两路行一起交 `decode_snapshot_roles`。
     - `GROUPS_FOR_DEVICES_SQL_TEMPLATE`：绑定 = 去重候选公钥；每行 → `(Vec<u8>, Vec<u8>, Option<i64>)`（`group_archived` 为 `Option`：LEFT JOIN NULL = dangling member 行）→ `decode_snapshot_group_rows`。
     - `DEVICES_BY_IDS_SQL_TEMPLATE`：绑定 = 去重候选公钥；每行 → `(Vec<u8>, String, Option<String>, String, String, u64, i64)` → `decode_device_admin_view`，错误 → `read_error("snapshot.devices")`。

   - 同事务核心（`authz/sqlx_repo.rs`，`pub(crate)`；`load_snapshot_in_tx` = actor 部分 + 设备部分 + `build_authorization_snapshot`，三段不得换序、不得拆到别的连接）：

```rust
// crates/rsetup-controller/src/authz/sqlx_repo.rs（本模块内追加；内部快照层同事务核心。
// imports：crate::db::DbPool（本模块既有 import，SqlxAuthzRepository 已依赖）；
// sqlx::Row（既有 import，见第 7 条注释）；GrantSource/GrantRecord/RoleRecord/SnapshotActor 等
// 经本模块既有 use 可见（第 1 条纯函数已使用），不新增 use。）

/// actor 级授权数据：actor 行 + epoch + 本人 grants + 被引用 roles。
/// 一个读事务**至多读一次**（device_page 多候选批次间复用，不重读）。
pub(crate) struct ActorAuthorization {
    pub actor: SnapshotActor,
    pub authz_epoch: u64,
    pub grants: Vec<GrantRecord>,
    pub roles: HashMap<Uuid, RoleRecord>,
}

/// 固定数量 `?` 占位符串（与去重 id 数 1:1）；空集必须在调用方短路，不得传入构造。
fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// actor 级段（固定顺序：actor 行 → epoch → 本人 grants → 被引用 roles，不得换序）：
/// - actor 行复用 `ACTOR_READONLY_SQL`（与 `ensure_actor_admin_readonly` 同形；行缺失 → `PermissionDenied`）；
/// - `SNAPSHOT_META_SQL` 行数必须恰好 1（0 行或 >1 行 = 污染）；
/// - `LIST_ACTOR_GRANTS_SQL` 按 `ORDER BY id` 稳定序（空集合法 = 无授权来源）；
/// - 被引用 roles 去重 + 排序后绑定（空引用集短路，不拼 `IN ()`）；
/// - 全部解码错误统一映射固定去敏 `read_error`（503 `STORAGE_UNAVAILABLE` 形状）。
pub(crate) async fn load_actor_authorization_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor_id: [u8; 16],
) -> Result<ActorAuthorization, ControllerError> {
    // 1) actor 行（行缺失 → PermissionDenied，不当 NotFound；三个布尔各自 raw_bool 严格解析，不短路略过）
    let row = sqlx::query(ACTOR_READONLY_SQL)
        .bind(actor_id.as_slice())
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.actor"))?;
    let actor = decode_snapshot_actor(
        actor_id,
        match row {
            None => None,
            Some(r) => Some((
                r.try_get::<i64, _>("active").map_err(|_| read_error("snapshot.actor"))?,
                r.try_get::<i64, _>("is_admin").map_err(|_| read_error("snapshot.actor"))?,
                r.try_get::<i64, _>("must_change_password")
                    .map_err(|_| read_error("snapshot.actor"))?,
            )),
        },
    )?;

    // 2) authz_epoch（恰好 1 行；0 行或 >1 行 = 污染）
    let meta_rows = sqlx::query(SNAPSHOT_META_SQL)
        .fetch_all(&mut **tx).await.map_err(|_| read_error("snapshot.epoch"))?;
    let raw_meta = meta_rows.iter().map(|row| -> Result<(i64, i64, u64), ControllerError> {
        Ok((
            row.try_get("singleton").map_err(|_| read_error("snapshot.epoch"))?,
            row.try_get("schema_version").map_err(|_| read_error("snapshot.epoch"))?,
            row.try_get("authz_epoch").map_err(|_| read_error("snapshot.epoch"))?,
        ))
    }).collect::<Result<Vec<_>, _>>()?;
    // 期望版本来自Runtime交付的编译期支持版本，不从数据库版本自我验证。
    let authz_epoch = decode_snapshot_meta(&raw_meta, crate::integrity::SUPPORTED_SCHEMA_VERSION)?;

    // 3) actor 本人 grants（ORDER BY id 稳定序；空集合法 = 无授权来源）
    let grant_rows = sqlx::query(LIST_ACTOR_GRANTS_SQL)
        .bind(actor_id.as_slice())
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.grants"))?;
    let grants: Vec<GrantRecord> = grant_rows
        .iter()
        .map(|r| -> Result<GrantRecord, ControllerError> {
            let grant = decode_grant(
                r.try_get("id").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("user_id").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("source_kind").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("role_id").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("permissions").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("scope_kind").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("scope_group_id").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("scope_device_id").map_err(|_| read_error("snapshot.grants"))?,
                r.try_get("revision").map_err(|_| read_error("snapshot.grants"))?,
            ).map_err(|_| read_error("snapshot.grants"))?;
            validate_snapshot_grant_references(
                &grant,
                r.try_get("owner_exists").map_err(|_| read_error("snapshot.reference"))?,
                r.try_get("role_exists").map_err(|_| read_error("snapshot.reference"))?,
                r.try_get("group_exists").map_err(|_| read_error("snapshot.reference"))?,
                r.try_get("device_exists").map_err(|_| read_error("snapshot.reference"))?,
            )?;
            Ok(grant)
        })
        .collect::<Result<Vec<_>, _>>()?;

    // 4) 被引用 roles（去重 + 稳定排序；空引用集短路，不拼 IN()）
    let mut role_ids: Vec<Uuid> = grants
        .iter()
        .filter_map(|g| match &g.source {
            GrantSource::Role(rid) => Some(*rid),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    role_ids.sort();
    let roles: HashMap<Uuid, RoleRecord> = if role_ids.is_empty() {
        HashMap::new()
    } else {
        let ph = placeholders(role_ids.len());
        let roles_sql = ROLES_BY_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph);
        let mut q = sqlx::query(&roles_sql);
        for rid in &role_ids {
            q = q.bind(rid.as_bytes());
        }
        let role_rows = q
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| read_error("snapshot.roles"))?;
        let raw_roles: Vec<(Vec<u8>, String, i64, i64, u64)> = role_rows
            .iter()
            .map(|r| -> Result<_, ControllerError> {
                Ok((
                    r.try_get("id").map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("name").map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("builtin").map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("archived").map_err(|_| read_error("snapshot.roles"))?,
                    r.try_get("revision").map_err(|_| read_error("snapshot.roles"))?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let ph = placeholders(role_ids.len());
        let perms_sql = ROLE_PERMISSIONS_BY_ROLE_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph);
        let mut q = sqlx::query(&perms_sql);
        for rid in &role_ids {
            q = q.bind(rid.as_bytes());
        }
        let perm_rows = q
            .fetch_all(&mut **tx)
            .await
            .map_err(|_| read_error("snapshot.roles"))?;
        let raw_perms: Vec<(Vec<u8>, String)> = perm_rows
            .iter()
            .map(|r| -> Result<_, ControllerError> {
                Ok((
                    r.try_get("role_id").map_err(|_| read_error("snapshot.role_permission"))?,
                    r.try_get("permission").map_err(|_| read_error("snapshot.role_permission"))?,
                ))
            })
            .collect::<Result<Vec<_>, _>>()?;

        decode_snapshot_roles(&raw_roles, &raw_perms)?
    };

    for role_id in &role_ids {
        if !roles.contains_key(role_id) {
            return Err(read_error("snapshot.role_missing"));
        }
    }
    Ok(ActorAuthorization { actor, authz_epoch, grants, roles })
}

/// 设备级段（固定顺序：每设备自身 group 集 → 设备行）：
/// `GROUPS_FOR_DEVICES_SQL_TEMPLATE`（LEFT JOIN，`group_archived = None` = dangling member 行）
/// → `DEVICES_BY_IDS_SQL_TEMPLATE`；空 `device_ids` 两条都短路（返回空 map）——
/// 调用方不得借此跳过 actor 段（空集不得绕过 actor 重验）。`device_ids` 由调用方去重（占位符 1:1）。
pub(crate) async fn load_device_snapshot_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    device_ids: &[[u8; 32]],
) -> Result<(HashMap<[u8; 32], HashSet<Uuid>>, HashMap<[u8; 32], DeviceAdminView>), ControllerError> {
    if device_ids.is_empty() {
        return Ok((HashMap::new(), HashMap::new()));
    }
    let ph = placeholders(device_ids.len());

    // 每设备自身 group 集（dangling member 行 / 脏布尔 → 固定去敏 Config，见 decode_snapshot_group_rows）
    let groups_sql = GROUPS_FOR_DEVICES_SQL_TEMPLATE.replace("{placeholders}", &ph);
    let mut q = sqlx::query(&groups_sql);
    for key in device_ids {
        q = q.bind(key.as_slice());
    }
    let group_rows = q
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.groups"))?;
    let raw_groups: Vec<(Vec<u8>, Vec<u8>, Option<i64>)> = group_rows
        .iter()
        .map(|r| -> Result<_, ControllerError> {
            let raw_exists: i64 = r.try_get("member_device_exists")
                .map_err(|_| read_error("snapshot.reference"))?;
            let exists = crate::integrity::raw_bool(raw_exists, "snapshot.reference")
                .map_err(|_| read_error("snapshot.reference"))?;
            if !exists { return Err(read_error("snapshot.reference")); }
            Ok((
                r.try_get("device_id").map_err(|_| read_error("snapshot.groups"))?,
                r.try_get("group_id").map_err(|_| read_error("snapshot.groups"))?,
                r.try_get("group_archived").map_err(|_| read_error("snapshot.groups"))?,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let device_groups = decode_snapshot_group_rows(&raw_groups)?;

    // 候选设备当前行（请求了但行缺失 = 该设备不存在，不是污染——map 中无该 key）
    let devices_sql = DEVICES_BY_IDS_SQL_TEMPLATE.replace("{placeholders}", &ph);
    let mut q = sqlx::query(&devices_sql);
    for key in device_ids {
        q = q.bind(key.as_slice());
    }
    let device_rows = q
        .fetch_all(&mut **tx)
        .await
        .map_err(|_| read_error("snapshot.devices"))?;
    let mut devices: HashMap<[u8; 32], DeviceAdminView> = HashMap::new();
    for r in &device_rows {
        let view = decode_device_admin_view(
            r.try_get("public_key").map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("display_name").map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("descriptor_json").map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("admission_state").map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("review_decision").map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("revision").map_err(|_| read_error("snapshot.devices"))?,
            r.try_get("archived").map_err(|_| read_error("snapshot.devices"))?,
        )?;
        devices.insert(view.public_key, view);
    }
    Ok((device_groups, devices))
}

/// 同事务快照核心（`AuthorizationSnapshot` 唯一组装点）：
/// actor 段 → 设备段 → `build_authorization_snapshot`；
/// 固定读取顺序（不得换序、不得拆到别的连接）：
/// actor 行 → epoch → 本人 grants → 被引用 roles → 每设备自身 groups → 设备行 → 组装。
pub(crate) async fn load_snapshot_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    actor_id: [u8; 16],
    device_ids: &[[u8; 32]],
) -> Result<AuthorizationSnapshot, ControllerError> {
    let actor_auth = load_actor_authorization_in_tx(tx, actor_id).await?;
    let (device_groups, devices) = load_device_snapshot_in_tx(tx, device_ids).await?;
    build_authorization_snapshot(
        actor_auth.actor,
        actor_auth.authz_epoch,
        actor_auth.grants,
        actor_auth.roles,
        device_groups,
        devices,
    )
}

// 完整定义（C1 部分前置，不闭合整个 C1）：`load_admin_target_snapshot_in_tx` 是普通层外 protected 管理分支——
// 准确复用同模块 `ensure_actor_admin_readonly`（从 DB 读 caller 三个 raw_bool：当前 active/is_admin/!must_change_password，
// 而非 session flags），通过后在同一 `tx` 内读 target；不自 begin/commit、不另开连接。
// 普通 `load_snapshot_in_tx` 原样不动，本 helper 只此管理分支、不用于普通 device_page；target admin 不授权 caller。
pub(crate) async fn load_admin_target_snapshot_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    caller: &crate::auth::service::Session,
    target_user: [u8; 16],
    device_ids: &[[u8; 32]],
) -> Result<AuthorizationSnapshot, ControllerError> {
    ensure_actor_admin_readonly(tx, caller).await?;
    load_snapshot_in_tx(tx, target_user, device_ids).await
}
// 边界：C1 前置 helper/repo/service 的 protected 管理读取通道纸面已接（同事务 raw_bool 重验 caller → 同 tx 读 target，绝不另开 admin 仓储事务）；stale caller / target admin / admin success 三类 service 行为 tests 与可编译错误 RED 桩已在计划定义、未落真实代码且未运行，C1 待正式限定复审；稳定读隔离、SQL 引用与其他 8 项 Important 未闭合，完整 Identity 仍 CHANGES_REQUIRED（不视为 APPROVED、不视为测试通过）；真库另未验。

/// 内部快照层公开入口：先核同lease RR配置再begin，保持该tx全部SELECT一致；
/// 配置预检不是双引擎实测，Task14 barrier缺失时不得宣称隔离验收。
/// 任何一步失败整体回滚（读事务失败不产生副作用）。
/// `device_ids` 由调用方去重（列表场景 = 候选设备集 ≤ limit+1；详情/有效权限场景 = 单设备集）。
pub struct SqlxAuthorizationSnapshotRepository {
    db: DbPool,
}

impl SqlxAuthorizationSnapshotRepository {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl AuthorizationSnapshotRepository for SqlxAuthorizationSnapshotRepository {
    fn load_snapshot<'a>(
        &'a self,
        actor_id: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot> {
        let db = self.db.clone();
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await
                .map_err(|_| read_error("snapshot.tx"))?;
            let snapshot = load_snapshot_in_tx(&mut tx, actor_id, device_ids).await?;
            tx.commit().await.map_err(|_| read_error("snapshot.tx"))?;
            Ok(snapshot)
        })
    }

    fn load_admin_target_snapshot<'a>(
        &'a self,
        caller: &'a crate::auth::service::Session,
        target_user: [u8; 16],
        device_ids: &'a [[u8; 32]],
    ) -> RepoFuture<'a, AuthorizationSnapshot> {
        let db = self.db.clone();
        Box::pin(async move {
            let mut connection = acquire_authorization_read_connection(&db).await?;
            let mut tx = sqlx::Connection::begin(&mut *connection).await
                .map_err(|_| read_error("snapshot.tx"))?;
            let snapshot = load_admin_target_snapshot_in_tx(
                &mut tx,
                caller,
                target_user,
                device_ids,
            )
            .await?;
            tx.commit().await.map_err(|_| read_error("snapshot.tx"))?;
            Ok(snapshot)
        })
    }
}
```

持久引用存在性验证（authz/sqlx_repo.rs，与decode_grant同模块；不把合法inactive/archived历史父行当missing）：

```rust
pub fn validate_snapshot_grant_references(
    grant: &GrantRecord, owner_exists: i64, role_exists: i64,
    group_exists: i64, device_exists: i64,
) -> Result<(), ControllerError> {
    // 四个raw flag先全部严格解析，避免&&短路漏掉污染值。
    let owner = crate::integrity::raw_bool(owner_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    let role = crate::integrity::raw_bool(role_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    let group = crate::integrity::raw_bool(group_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    let device = crate::integrity::raw_bool(device_exists, "snapshot.reference")
        .map_err(|_| read_error("snapshot.reference"))?;
    if !owner
        || (matches!(&grant.source, GrantSource::Role(_)) && !role)
        || (matches!(&grant.scope, GrantScope::Group(_)) && !group)
        || (matches!(&grant.scope, GrantScope::Device(_)) && !device)
    {
        Err(read_error("snapshot.reference"))
    } else {
        Ok(())
    }
}
```

GROUPS_FOR_DEVICES_SQL_TEMPLATE另LEFT JOIN devices，以`CAST(d.public_key IS NOT NULL AS SIGNED) AS member_device_exists`读取；同一tx先严格校验每条持久member的两端存在，再交decode_snapshot_group_rows做archived过滤。requested设备缺行且没有持久悬空关联，devices map无key由详情404；若存在持久grant/member引用该缺父设备，必须上述固定Config503，不能把污染伪成404/空权限。load_snapshot_in_tx最后仍必须调用build_authorization_snapshot，不能在列表拆分路径跳过被引用role存在性检查。

```rust
#[test]
fn snapshot_grant_reference_flags_fail_closed_and_direct_all_is_legal() {
    let mut grant = GrantRecord {
        id: Uuid::new_v4(), user_id: [1; 16],
        source: GrantSource::Direct(vec![Permission::DeviceRead]),
        scope: GrantScope::All, revision: 1,
    };
    assert!(validate_snapshot_grant_references(&grant, 1, 0, 0, 0).is_ok());
    assert!(validate_snapshot_grant_references(&grant, 0, 0, 0, 0).is_err());
    assert!(validate_snapshot_grant_references(&grant, 1, 2, 0, 0).is_err());
    grant.source = GrantSource::Role(Uuid::new_v4());
    assert!(validate_snapshot_grant_references(&grant, 1, 0, 0, 0).is_err());
    assert!(validate_snapshot_grant_references(&grant, 1, 1, 0, 0).is_ok());
    grant.scope = GrantScope::Group(Uuid::new_v4());
    assert!(validate_snapshot_grant_references(&grant, 1, 1, 0, 0).is_err());
    assert!(validate_snapshot_grant_references(&grant, 1, 1, 1, 0).is_ok());
    grant.scope = GrantScope::Device([2; 32]);
    assert!(validate_snapshot_grant_references(&grant, 1, 1, 1, 0).is_err());
    assert!(validate_snapshot_grant_references(&grant, 1, 1, 1, 1).is_ok());
}
```

RED前置helper签名完整、恒Ok，未来选择器`cargo test --offline --locked -p rsetup-controller snapshot_grant_reference_flags_fail_closed_and_direct_all_is_legal`恰好1方法，缺owner负例首次目标失败；GREEN改用上方实现。纯flag矩阵只证明校验规则；真实LEFT JOIN/consumer线路需Task14在门禁后注入dangling并断言固定503且事务无业务修改。无联通测试不得宣称I3全实测闭合。

### 普通/受保护snapshot的稳定读与meta专项步骤

此段补齐I2/I4计划合同，不证明真实引擎已验。SQLx读取实现必须先使用上述`acquire_authorization_read_connection`，再在**同一lease**上`sqlx::Connection::begin(&mut *connection)`，所有actor/meta/grants/role/member/device SELECT只借该tx。既有普通device_page不调用admin-helper；protected sources仍在同tx重验caller再load target。连接配置未知/非RR时固定去敏503，禁止临时更改session隔离或退回多连接查询。

**编译前置：** Runtime Task3先交付`crate::integrity::SUPPORTED_SCHEMA_VERSION: i32`及版本化guard（目标4，v3只用于显式preflight）；本Task3的纯decode/policy TDD可先执行，但包含SQLx读实现的模块在该版本接口未交付前不注册/不运行选择器，缺接口不是RED。MySQL与TiDB须在安全门后各用barrier验证上述同连接RR真实快照；变量字符串不是实际隔离证据，若引擎/config的RR语义或真实barrier不符合，读源保持BLOCKED，不能自动换RC或标支持。

```rust
// authz/sqlx_repo.rs tests内追加；RED前置两纯helper签名完整，分别恒Ok()/Ok(0)。
#[test]
fn snapshot_meta_rejects_extra_wrong_or_unknown_rows_and_preserves_u64_epoch() {
    assert_eq!(decode_snapshot_meta(&[(1, 4, u64::MAX)], 4).unwrap(), u64::MAX);
    assert_eq!(decode_snapshot_meta(&[(1, 4, 0)], 4).unwrap(), 0); // epoch零可为尚无变更，不猜成污染
    for rows in [vec![], vec![(0, 4, 1)], vec![(2, 4, 1)], vec![(1, 3, 1)],
                 vec![(1, 5, 1)], vec![(1, 4, 1), (2, 4, 2)]] {
        assert!(matches!(decode_snapshot_meta(&rows, 4), Err(ControllerError::Config(_))));
    }
    assert!(!SNAPSHOT_META_SQL.contains("WHERE"));
    assert!(SNAPSHOT_META_SQL.ends_with("LIMIT 2"));
}

#[test]
fn snapshot_requires_verified_repeatable_read_configuration() {
    assert!(require_snapshot_isolation("REPEATABLE-READ").is_ok());
    assert!(require_snapshot_isolation("REPEATABLE READ").is_ok());
    for bad in ["", "READ-COMMITTED", "SERIALIZABLE", "unknown"] {
        assert!(matches!(require_snapshot_isolation(bad), Err(ControllerError::Config(_))));
    }
}
```

未来命令各恰好1方法：`cargo test --offline --locked -p rsetup-controller snapshot_meta_rejects_extra_wrong_or_unknown_rows_and_preserves_u64_epoch`（RED首u64Max正例返回0失败），`cargo test --offline --locked -p rsetup-controller snapshot_requires_verified_repeatable_read_configuration`（RED在首bad配置仍Ok失败）；从设计根统一主CARGO_HOME/私有target。GREEN用上方真实pure helper，不运行真库。Task14新增逐引擎barrier场景：读事务先取得当前actor/meta/grants快照后暂停，合法写仓储撤权/改密/归档并commit，再恢复role/member/device读取；断言一个tx内只得到一致旧或新投影，随后新tx必须看到撤权，不允许actor旧+grants新。各case必须真实seed、两个独立lease及实际投影断言，不能以配置SELECT或注释代替。

- [ ] **Step 4: 运行并确认 GREEN + 回归**

命令沿用 Step 2 约定（`CARGO_HOME` = 主工作区共享依赖缓存，只读复用；`CARGO_TARGET_DIR=$PWD/.cargo-target` = 设计树私有构建目录，`$PWD` = 设计树根目录）：

```bash
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller authz::sqlx_repo::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller devices::repository::tests
CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR=$PWD/.cargo-target cargo test --offline --locked -p rsetup-controller --lib
```
预期：前两条全绿；第三条为 crate 全部单元测试，必须与改动前同样全绿（尤其 `integrity`、`auth`、`devices::service`、`http_auth`）。**本步骤不产生任何真库 GREEN**；"只读仓储真实可用"的证据在 Task 14。

- [ ] **Step 5: 静态检查与条件性提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/authz/sqlx_repo.rs crates/rsetup-controller/src/authz/mod.rs crates/rsetup-controller/src/authz/policy.rs crates/rsetup-controller/src/devices/repository.rs crates/rsetup-controller/src/devices/mod.rs crates/rsetup-controller/src/integrity.rs
git commit -m "feat(controller): 新增管理员只读仓储并带 actor 复验证"
```

---

//! users 资源管理路由：网关契约与 5 条管理端点（Identity Task 7）。
//!
//! 契约（spec 02 §3/§4）：全部端点先过 `http_gate` 会话门禁，再过 `require_admin_session`
//! 管理前置，权威判定仍在仓储事务 guard 内重验；`POST`/`PATCH` 只按 `session.digest` 走
//! 写限速桶，两条 `GET` 按 `(EndpointId, session.digest)` 走读桶。
//! 响应 `data` 一律由网关的固定投影函数构造，handler 不拼 SQL、不接受任意字段。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use axum::routing::{get, patch, post};
use serde::Deserialize;

use crate::auth::service::Session;
use crate::error::ControllerError;
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{
    authenticate_read, authenticate_write, read_json_body, require_admin_session,
};

use super::{
    CursorFilterCtx, CursorKey, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, ManagementFuture,
    decode_management_list_cursor, management_cursor_secret, map_error, parse_device_id_path,
    parse_revision, parse_uuid_path, sign_management_list_page,
};

#[derive(Clone)]
pub struct UsersApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn UsersGateway>,
}

/// users 资源网关契约：所有方法显式借用 actor，返回已校验的 HTTP 响应投影。
/// `Value` 不是让 HTTP 传任意 SQL/任意操作的入口；生产 adapter 用固定投影函数构造。
pub trait UsersGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 16]>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(
        &'a self,
        actor: &'a Session,
        username: &'a str,
        display_name: &'a str,
        is_admin: bool,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 16],
        revision: u64,
        active: Option<bool>,
        is_admin: Option<bool>,
        display_name: Option<&'a str>,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn reset_password<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 16],
        revision: u64,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn effective_permissions<'a>(
        &'a self,
        actor: &'a Session,
        target: [u8; 16],
        device_id: Option<[u8; 32]>,
        cursor: Option<[u8; 32]>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

// ---------------------------------------------------------------------------
// 严格 query / body DTO
// ---------------------------------------------------------------------------

/// `GET /api/v1/users` 的严格 query DTO：`deny_unknown_fields` 拒绝任何未知参数；
/// `Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsersListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
}

/// `GET /api/v1/users/{id}/effective-permissions` 的严格 query DTO。
///
/// 本端点不在 `management_cursor_profile` 的封闭白名单内，没有签名 cursor 契约，
/// 而裸内部键直传被计划禁止，因此 `cursor` 属未知字段 → 400，网关侧 cursor 恒 `None`；
/// 该端点的分页游标契约属后续修订，不在本任务伪造。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EffectivePermissionsQuery {
    pub(crate) device_id: Option<String>,
    pub(crate) limit: Option<usize>,
}

/// `POST /api/v1/users` 输入：三个字段全必填，未知字段 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateUserInput {
    pub(crate) username: String,
    pub(crate) display_name: String,
    pub(crate) is_admin: bool,
}

/// `PATCH /api/v1/users/{id}` 输入：`expected_revision` 必填十进制字符串（spec 02 §1），
/// 其余为可选 CAS 补丁字段；未知字段 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchUserInput {
    pub(crate) expected_revision: String,
    pub(crate) active: Option<bool>,
    pub(crate) is_admin: Option<bool>,
    pub(crate) display_name: Option<String>,
}

/// `POST /api/v1/users/{id}/reset-password` 输入：仅 CAS 版本号。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResetPasswordInput {
    pub(crate) expected_revision: String,
}

/// 列表 `limit` 合法域：缺省 50，`1..=200`；`0`、`>200` 一律 400（spec 02 L16）。
///
/// `Err` 侧是 axum `Response`（≥128 字节）：与 `api/mod.rs` 同族解析辅助口径一致，
/// 失败路径直接返回固定错误响应、不在热路径装箱，故按函数抑制该 lint。
#[allow(clippy::result_large_err)]
fn resolve_limit(raw: Option<usize>) -> Result<usize, Response> {
    match raw {
        None => Ok(DEFAULT_PAGE_LIMIT),
        Some(value) if (1..=MAX_PAGE_LIMIT).contains(&value) => Ok(value),
        Some(_) => Err(map_error(ControllerError::InvalidArgument)),
    }
}

// ---------------------------------------------------------------------------
// 5 条管理 handler
// ---------------------------------------------------------------------------

/// `GET /api/v1/users` 的 inner：`secret_provider` 真正调用发生在
/// `authenticate_read` → `require_admin_session` → query/limit 校验之后，且 `cursor`
/// 为 `None` 也必须先取密钥（未安装 → `Config` → `map_error` 精确 503，不得短路跳过）。
/// 客户端侧（query 非法、limit 越域、token 篡改/跨端点/跨用户）→ `InvalidArgument` 400；
/// 配置/后端侧按 Task 6 约定映射，绝不吞 `Err` 成 generic 500 或返回未签名页面。
async fn list_users_with_secret_provider<F>(
    state: &UsersApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::UsersRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<UsersListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match resolve_limit(query.limit) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let filter = CursorFilterCtx::empty();
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(
        &secret,
        EndpointId::UsersRead,
        actor.user.id,
        &filter,
        query.cursor.as_deref(),
    ) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(id))) => Some(id),
        // `DeviceKey` 分支在 `UsersRead` profile 下不可达，仍 fail-closed 400。
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(&secret, EndpointId::UsersRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_users(
    State(state): State<UsersApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_users_with_secret_provider(&state, &headers, &uri, || {
        // 计划原文写作 `.map(|secret| *secret)`：语义就是 32B 数组拷贝，
        // 在 `-D warnings` 下按 clippy 口径收敛为等价的 `copied()`。
        management_cursor_secret().copied()
    })
    .await
}

/// `POST /api/v1/users`：创建用户并一次性返回临时密码（不落审计、不二次展示）。
/// 重名由网关映射为 `InvalidState` → 409 `INVALID_STATE`。
async fn create_user(
    State(state): State<UsersApiState>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let input = match read_json_body::<CreateUserInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let created = match state
        .gateway
        .create(&actor, &input.username, &input.display_name, input.is_admin)
        .await
    {
        Ok(created) => created,
        Err(error) => return map_error(error),
    };
    ok_response(created)
}

/// `PATCH /api/v1/users/{id}`：CAS 补丁。降级/停用唯一管理员由网关返回 `LastAdmin`
/// → 409 `LAST_ADMIN`；版本不符 → 409 `REVISION_CONFLICT`；不存在 → 404。
async fn patch_user(
    State(state): State<UsersApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let target = match parse_uuid_path(&id) {
        Ok(target) => target,
        Err(response) => return response,
    };
    let input = match read_json_body::<PatchUserInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    let patched = match state
        .gateway
        .patch(
            &actor,
            target.into_bytes(),
            revision,
            input.active,
            input.is_admin,
            input.display_name.as_deref(),
        )
        .await
    {
        Ok(patched) => patched,
        Err(error) => return map_error(error),
    };
    ok_response(patched)
}

/// `POST /api/v1/users/{id}/reset-password`：重置并一次性返回临时密码；
/// 撤销目标会话与 LastAdmin 门禁都在网关事务内完成。
async fn reset_password(
    State(state): State<UsersApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Response {
    let actor = match authenticate_write(&state.auth, &headers).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let target = match parse_uuid_path(&id) {
        Ok(target) => target,
        Err(response) => return response,
    };
    let input = match read_json_body::<ResetPasswordInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    let rotated = match state
        .gateway
        .reset_password(&actor, target.into_bytes(), revision)
        .await
    {
        Ok(rotated) => rotated,
        Err(error) => return map_error(error),
    };
    ok_response(rotated)
}

/// `GET /api/v1/users/{id}/effective-permissions`：目标用户有效权限及来源归因。
/// 目标不存在/不可见由网关统一 404；`device_id` 必须恰 64 位小写 hex，否则 400。
async fn effective_permissions(
    State(state): State<UsersApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let actor = match authenticate_read(&state.auth, &headers, EndpointId::EffectivePermissionsRead)
        .await
    {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<EffectivePermissionsQuery>::try_from_uri(&uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match resolve_limit(query.limit) {
        Ok(limit) => limit,
        Err(response) => return response,
    };
    let target = match parse_uuid_path(&id) {
        Ok(target) => target,
        Err(response) => return response,
    };
    let device_id = match query.device_id.as_deref() {
        None => None,
        Some(raw) => match parse_device_id_path(raw) {
            Ok(device_id) => Some(device_id),
            Err(response) => return response,
        },
    };
    let page = match state
        .gateway
        .effective_permissions(&actor, target.into_bytes(), device_id, None, limit)
        .await
    {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    ok_response(page)
}

/// 挂载 users 的 5 条管理路由：集合级 `GET`/`POST` 同 path 共存，
/// 单资源 `PATCH` 与两个子资源端点各一处注册，无第二处重复挂载。
pub fn users_router(state: UsersApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/users", get(list_users).post(create_user))
        .route("/api/v1/users/{id}", patch(patch_user))
        .route("/api/v1/users/{id}/reset-password", post(reset_password))
        .route(
            "/api/v1/users/{id}/effective-permissions",
            get(effective_permissions),
        )
        .with_state(state)
}

// ---------------------------------------------------------------------------
// 资源测试共享前置层（Identity 计划 Task 7–12 复用）
// ---------------------------------------------------------------------------
//
// 计划原文把本层放在 `src/api/mod.rs` 文末；本任务写作用域严格限定为 `api/users.rs`，
// 故先在本文件落地 users/请求 helper 部分，路径为 `crate::api::users::test_fixture`。
// 后续任务若改由 `api/mod.rs` 统一持有，可整体平移本模块，consumer 只改一行 `use`。
// 纯内存 fixture：不连数据库、不读环境变量、不构造任何备份或备份引用。
#[cfg(test)]
pub(crate) mod test_fixture {
    use super::*;
    use crate::auth::service::{IdentityUser, Login, SessionPage};
    use crate::error::ControllerError;
    use crate::http_auth::{AuthFuture, AuthServiceApi, CsrfBinding};
    use crate::http_security::HttpSecurityConfig;
    use axum::body::Body;
    use axum::http::{HeaderName, Request, header};
    use std::time::Instant;

    /// fixture Host：与下方 allowlist 精确匹配（Task 7–12 测试请求 Host）。
    pub(crate) const FIXTURE_HOST: &str = "localhost:3080";

    fn hex64(fill: char) -> String {
        fill.to_string().repeat(64)
    }

    /// 进程内 actor `Session`（同 Task 14 `admin_actor` 形状，`is_admin` 可控）。
    pub(crate) fn fixture_session(is_admin: bool) -> Session {
        Session {
            user: IdentityUser {
                id: [7u8; 16],
                username: "fixture_admin".into(),
                password_hash: "fixture-placeholder".into(),
                active: true,
                is_admin,
                must_change_password: false,
                revision: 1,
            },
            digest: [9u8; 32],
        }
    }

    /// `must_change_password` 的 admin 会话：管理前置门禁必须与无权限 403 分码。
    pub(crate) fn fixture_session_must_change_password() -> Session {
        let mut session = fixture_session(true);
        session.user.must_change_password = true;
        session
    }

    /// `FixtureAuth`：Task 1 `StubAuth`（`http_gate.rs` tests）的 test_fixture 版克隆。
    pub(crate) struct FixtureAuth {
        token: String,
        session: Session,
    }

    impl AuthServiceApi for FixtureAuth {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
            let ok = raw == self.token;
            let session = self.session.clone();
            Box::pin(async move {
                if ok {
                    Ok(session)
                } else {
                    Err(ControllerError::InvalidArgument)
                }
            })
        }
        fn change_password<'a>(
            &'a self,
            _s: &'a Session,
            _c: &'a str,
            _n: &'a str,
        ) -> AuthFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            Box::pin(async { Ok(0) })
        }
        fn is_live_session(&self, _d: &[u8; 32]) -> bool {
            true
        }
        fn list_sessions<'a>(
            &'a self,
            _s: &'a Session,
            _c: Option<&'a str>,
            _l: usize,
        ) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _i: &'a str) -> AuthFuture<'a, bool> {
            Box::pin(async { Ok(false) })
        }
        fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async { Ok(0) })
        }
    }

    /// 最小装配：合法 Host allowlist + 固定会话 + 绑定 `session.digest` 的 CSRF。
    pub(crate) fn fixture_auth(session: Session) -> Arc<HttpAuthState> {
        let auth = Arc::new(HttpAuthState::new(
            Arc::new(FixtureAuth {
                token: hex64('a'),
                session: session.clone(),
            }),
            HttpSecurityConfig::from_values(
                Some("localhost:3080,controller.test"),
                Some("http://localhost:3080"),
                None,
            )
            .unwrap(),
        ));
        auth.csrf.lock().unwrap().insert(
            session.digest,
            CsrfBinding {
                token: hex64('b'),
                issued_at: Instant::now(),
            },
        );
        auth
    }

    /// users 资源前置：返回 `(Router, 测试状态)`；入口只差 Session 形态与网关行为。
    pub(crate) fn users_state_with(
        session: Session,
        gateway: Arc<dyn UsersGateway>,
    ) -> (axum::Router<()>, Arc<HttpAuthState>) {
        let auth = fixture_auth(session);
        (
            users_router(UsersApiState {
                auth: auth.clone(),
                gateway,
            }),
            auth,
        )
    }

    pub(crate) fn fixture_state() -> (axum::Router<()>, Arc<HttpAuthState>) {
        users_state_with(fixture_session(true), Arc::new(FixtureUsersGateway::Allow))
    }

    pub(crate) fn fixture_state_with_non_admin() -> (axum::Router<()>, Arc<HttpAuthState>) {
        users_state_with(fixture_session(false), Arc::new(FixtureUsersGateway::Allow))
    }

    pub(crate) fn fixture_state_with_last_admin_rejection() -> (axum::Router<()>, Arc<HttpAuthState>)
    {
        users_state_with(
            fixture_session(true),
            Arc::new(FixtureUsersGateway::LastAdmin),
        )
    }

    /// 指定网关行为的 admin 前置（404/409/回显等用例复用）。
    pub(crate) fn admin_state_with(
        gateway: FixtureUsersGateway,
    ) -> (axum::Router<()>, Arc<HttpAuthState>) {
        users_state_with(fixture_session(true), Arc::new(gateway))
    }

    pub(crate) fn fixture_state_must_change_password() -> (axum::Router<()>, Arc<HttpAuthState>) {
        users_state_with(
            fixture_session_must_change_password(),
            Arc::new(FixtureUsersGateway::Allow),
        )
    }

    /// 纯内存 `UsersGateway`：`Allow` 固定投影、`Echo` 回显 handler 传入的已解析参数，
    /// 其余变体把指定方法映射到指定 `ControllerError`。
    #[derive(Clone, Copy)]
    pub(crate) enum FixtureUsersGateway {
        Allow,
        Echo,
        LastAdmin,
        DuplicateUsername,
        RevisionConflict,
        Missing,
    }

    impl FixtureUsersGateway {
        fn reject(self) -> Option<ControllerError> {
            match self {
                Self::Allow | Self::Echo => None,
                Self::LastAdmin => Some(ControllerError::LastAdmin),
                Self::DuplicateUsername => Some(ControllerError::InvalidState),
                Self::RevisionConflict => Some(ControllerError::RevisionConflict),
                Self::Missing => Some(ControllerError::NotFound),
            }
        }
    }

    impl UsersGateway for FixtureUsersGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            _cursor: Option<[u8; 16]>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Ok(serde_json::json!({ "items": [], "next_cursor": null })) })
        }
        fn create<'a>(
            &'a self,
            _actor: &'a Session,
            username: &'a str,
            display_name: &'a str,
            is_admin: bool,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if let Some(error) = gateway.reject() {
                    return Err(error);
                }
                if matches!(gateway, Self::Echo) {
                    return Ok(serde_json::json!({
                        "username": username,
                        "display_name": display_name,
                        "is_admin": is_admin,
                        "user": null,
                        "temporary_password": "fixture",
                    }));
                }
                Ok(serde_json::json!({ "user": null, "temporary_password": "fixture" }))
            })
        }
        fn patch<'a>(
            &'a self,
            _actor: &'a Session,
            id: [u8; 16],
            revision: u64,
            active: Option<bool>,
            is_admin: Option<bool>,
            display_name: Option<&'a str>,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if let Some(error) = gateway.reject() {
                    return Err(error);
                }
                if matches!(gateway, Self::Echo) {
                    return Ok(serde_json::json!({
                        "id": hex::encode(id),
                        "revision": revision,
                        "active": active,
                        "is_admin": is_admin,
                        "display_name": display_name,
                    }));
                }
                Ok(serde_json::json!({}))
            })
        }
        fn reset_password<'a>(
            &'a self,
            _actor: &'a Session,
            id: [u8; 16],
            revision: u64,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if let Some(error) = gateway.reject() {
                    return Err(error);
                }
                if matches!(gateway, Self::Echo) {
                    return Ok(serde_json::json!({
                        "id": hex::encode(id),
                        "revision": revision,
                        "user": null,
                        "temporary_password": "fixture",
                    }));
                }
                Ok(serde_json::json!({ "user": null, "temporary_password": "fixture" }))
            })
        }
        fn effective_permissions<'a>(
            &'a self,
            _actor: &'a Session,
            target: [u8; 16],
            device_id: Option<[u8; 32]>,
            cursor: Option<[u8; 32]>,
            limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if let Some(error) = gateway.reject() {
                    return Err(error);
                }
                if matches!(gateway, Self::Echo) {
                    return Ok(serde_json::json!({
                        "items": [],
                        "target": hex::encode(target),
                        "device_id": device_id.map(hex::encode),
                        "cursor": cursor.map(hex::encode),
                        "limit": limit,
                    }));
                }
                Ok(serde_json::json!({ "items": [] }))
            })
        }
    }

    /// 请求 helper：自动携带 Host/Cookie（不发 Origin，缺失仅豁免 Origin 步）。
    pub(crate) fn authenticated_get(uri: &str) -> Request<Body> {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header(header::HOST, FIXTURE_HOST)
            .header(header::COOKIE, format!("rsc_session={}", hex64('a')))
            .body(Body::empty())
            .unwrap()
    }

    /// 写请求 helper：Host/Cookie + 绑定 `session.digest` 的 CSRF + JSON body。
    pub(crate) fn authenticated_json_write(
        method: &str,
        uri: &str,
        body: serde_json::Value,
    ) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, FIXTURE_HOST)
            .header(header::COOKIE, format!("rsc_session={}", hex64('a')))
            .header(HeaderName::from_static("x-csrf-token"), hex64('b'))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    pub(crate) fn authenticated_json_patch(uri: &str, body: serde_json::Value) -> Request<Body> {
        authenticated_json_write("PATCH", uri, body)
    }

    pub(crate) fn authenticated_json_post(uri: &str, body: serde_json::Value) -> Request<Body> {
        authenticated_json_write("POST", uri, body)
    }

    /// 响应体 JSON 解析断言 helper。
    pub(crate) async fn body_json(res: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(res.into_body(), 1_048_576)
            .await
            .expect("响应体");
        serde_json::from_slice(&bytes).expect("JSON 响应体")
    }
}

#[cfg(test)]
mod tests {
    use super::test_fixture::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    /// canonical 小写 UUIDv4 路径样本（`067e6162-…-0b3c`）。
    const TARGET_ID: &str = "067e6162-ea94-4431-836c-7d8fa2010b3c";
    const TARGET_ID_RAW: &str = "067e6162ea944431836c7d8fa2010b3c";
    /// 64 位小写 hex 设备 ID 路径/query 样本。
    const TARGET_DEVICE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const TARGET_DEVICE_RAW: &str = TARGET_DEVICE;

    async fn error_code(res: axum::response::Response) -> (StatusCode, String, String) {
        let status = res.status();
        let value = body_json(res).await;
        (
            status,
            value["error"]["code"].as_str().unwrap_or("").to_string(),
            value["error"]["message_key"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        )
    }

    // RED 前置：骨架 `users_router` 挂载 0 条路由，以下断言在实现前必须失败于
    // `404`（行为 RED），不得是编译错误。

    #[tokio::test]
    async fn unauthenticated_read_is_rejected_with_401() {
        let (router, _state) = fixture_state();
        let req = Request::builder()
            .uri("/api/v1/users")
            .header(header::HOST, FIXTURE_HOST)
            .body(Body::empty())
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn unauthenticated_write_is_rejected_with_401() {
        let (router, _state) = fixture_state();
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/users")
            .header(header::HOST, FIXTURE_HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "username": "ops",
                    "display_name": "Ops",
                    "is_admin": false,
                })
                .to_string(),
            ))
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_admin_is_rejected_with_403_permission_denied() {
        let (router, _state) = fixture_state_with_non_admin();
        let res = router
            .oneshot(authenticated_get("/api/v1/users"))
            .await
            .unwrap();
        let (status, code, key) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
        assert_eq!(key, "permission.denied");
    }

    #[tokio::test]
    async fn must_change_password_admin_gets_distinct_403_code() {
        let (router, _state) = fixture_state_must_change_password();
        let res = router
            .oneshot(authenticated_get("/api/v1/users"))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PASSWORD_CHANGE_REQUIRED");
    }

    #[tokio::test]
    async fn last_admin_demotion_returns_409_last_admin() {
        let (router, _state) = fixture_state_with_last_admin_rejection();
        let body = serde_json::json!({ "expected_revision": "1", "is_admin": false });
        let req = authenticated_json_patch(&format!("/api/v1/users/{TARGET_ID}"), body);
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "LAST_ADMIN");
    }

    #[tokio::test]
    async fn duplicate_username_returns_409_invalid_state() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::DuplicateUsername);
        let body =
            serde_json::json!({ "username": "ops", "display_name": "Ops", "is_admin": false });
        let req = authenticated_json_post("/api/v1/users", body);
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "INVALID_STATE");
    }

    #[tokio::test]
    async fn missing_user_returns_404_on_patch_reset_and_permissions() {
        let requests = [
            authenticated_json_patch(
                &format!("/api/v1/users/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "1", "display_name": "Ops" }),
            ),
            authenticated_json_post(
                &format!("/api/v1/users/{TARGET_ID}/reset-password"),
                serde_json::json!({ "expected_revision": "1" }),
            ),
            authenticated_get(&format!("/api/v1/users/{TARGET_ID}/effective-permissions")),
        ];
        // `Router` 不 `Clone`：每个请求装配一份等价 fixture router。
        for req in requests {
            let (router, _state) = admin_state_with(FixtureUsersGateway::Missing);
            let res = router.oneshot(req).await.unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(code, "NOT_FOUND");
        }
    }

    #[tokio::test]
    async fn revision_conflict_returns_409_on_reset_password() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::RevisionConflict);
        let req = authenticated_json_post(
            &format!("/api/v1/users/{TARGET_ID}/reset-password"),
            serde_json::json!({ "expected_revision": "9" }),
        );
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "REVISION_CONFLICT");
    }

    #[tokio::test]
    async fn create_user_returns_user_and_one_time_password() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let req = authenticated_json_post(
            "/api/v1/users",
            serde_json::json!({ "username": "ops", "display_name": "Ops 运维", "is_admin": true }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["temporary_password"], "fixture");
        assert!(value["data"].get("user").is_some());
        assert!(value["request_id"].is_string());
        // 网关必须收到 handler 解析后的原始字段（不夹带、不改写）。
        assert_eq!(value["data"]["username"], "ops");
        assert_eq!(value["data"]["display_name"], "Ops 运维");
        assert_eq!(value["data"]["is_admin"], true);
    }

    #[tokio::test]
    async fn patch_user_passes_canonical_id_parsed_revision_and_flags() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let req = authenticated_json_patch(
            &format!("/api/v1/users/{TARGET_ID}"),
            serde_json::json!({
                "expected_revision": "7",
                "active": false,
                "display_name": "Ops",
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["id"], TARGET_ID_RAW);
        assert_eq!(value["data"]["revision"], 7);
        assert_eq!(value["data"]["active"], false);
        assert!(value["data"]["is_admin"].is_null());
        assert_eq!(value["data"]["display_name"], "Ops");
    }

    #[tokio::test]
    async fn reset_password_returns_user_and_one_time_password() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let req = authenticated_json_post(
            &format!("/api/v1/users/{TARGET_ID}/reset-password"),
            serde_json::json!({ "expected_revision": "12" }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["temporary_password"], "fixture");
        assert!(value["data"].get("user").is_some());
        assert_eq!(value["data"]["id"], TARGET_ID_RAW);
        assert_eq!(value["data"]["revision"], 12);
    }

    #[tokio::test]
    async fn effective_permissions_projects_items_and_parses_device_id() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let req = authenticated_get(&format!(
            "/api/v1/users/{TARGET_ID}/effective-permissions?device_id={TARGET_DEVICE}&limit=20"
        ));
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["items"], serde_json::json!([]));
        assert_eq!(value["data"]["target"], TARGET_ID_RAW);
        assert_eq!(value["data"]["device_id"], TARGET_DEVICE_RAW);
        // 本端点无签名 cursor 契约：网关侧 cursor 恒 None，limit 走严格域。
        assert!(value["data"]["cursor"].is_null());
        assert_eq!(value["data"]["limit"], 20);
    }

    #[tokio::test]
    async fn effective_permissions_defaults_limit_and_rejects_bad_inputs() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let defaults = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/users/{TARGET_ID}/effective-permissions"
            )))
            .await
            .unwrap();
        assert_eq!(defaults.status(), StatusCode::OK);
        let value = body_json(defaults).await;
        assert_eq!(value["data"]["limit"], 50);
        assert!(value["data"]["device_id"].is_null());

        for query in [
            format!("device_id={}0", TARGET_DEVICE),
            format!("device_id={}", TARGET_DEVICE.to_uppercase()),
            "device_id=not-a-hex-id".to_string(),
            "limit=0".to_string(),
            "limit=201".to_string(),
            "limit=abc".to_string(),
            "cursor=0123456789abcdef".to_string(),
            "unknown=1".to_string(),
        ] {
            let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
            let res = router
                .oneshot(authenticated_get(&format!(
                    "/api/v1/users/{TARGET_ID}/effective-permissions?{query}"
                )))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
        }
    }

    #[tokio::test]
    async fn list_rejects_unknown_query_and_out_of_range_limit_with_400() {
        for query in ["limit=0", "limit=201", "limit=abc", "foo=1", "device_id=aa"] {
            let (router, _state) = fixture_state();
            let res = router
                .oneshot(authenticated_get(&format!("/api/v1/users?{query}")))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
        }
    }

    #[tokio::test]
    async fn patch_rejects_non_canonical_uuid_and_malformed_revision_with_400() {
        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let bad_id = router
            .oneshot(authenticated_json_patch(
                "/api/v1/users/067E6162-EA94-4431-836C-7D8FA2010B3C",
                serde_json::json!({ "expected_revision": "1", "display_name": "Ops" }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(bad_id).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.invalid_id");

        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let bad_revision = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/users/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "+1", "display_name": "Ops" }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(bad_revision).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.revision_invalid");

        let (router, _state) = admin_state_with(FixtureUsersGateway::Echo);
        let bad_body = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/users/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "1", "unknown": true }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(bad_body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.invalid_body");
    }

    #[tokio::test]
    async fn write_route_without_csrf_token_is_rejected_with_403() {
        let (router, _state) = fixture_state();
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/users")
            .header(header::HOST, FIXTURE_HOST)
            .header(header::COOKIE, format!("rsc_session={}", "a".repeat(64)))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "username": "ops", "display_name": "Ops", "is_admin": false })
                    .to_string(),
            ))
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "CSRF_INVALID");
    }

    #[tokio::test]
    async fn unmounted_methods_on_users_paths_are_not_allowed() {
        let (router, _state) = fixture_state();
        let res = router
            .oneshot(authenticated_json_write(
                "PUT",
                "/api/v1/users",
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
        let (router, _state) = fixture_state();
        let res = router
            .oneshot(authenticated_json_write(
                "DELETE",
                &format!("/api/v1/users/{TARGET_ID}"),
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

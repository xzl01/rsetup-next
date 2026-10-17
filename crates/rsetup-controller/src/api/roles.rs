//! roles 资源管理路由：网关契约与 3 条管理端点（Identity Task 8）。
//!
//! 契约（spec 02 §3/§4）：全部端点先过 `http_gate` 会话门禁，再过 `require_admin_session`
//! 管理前置，权威判定仍在仓储事务 guard 内重验；`POST`/`PATCH` 只按 `session.digest` 走
//! 写限速桶，`GET /api/v1/roles` 按 `(EndpointId::RolesRead, session.digest)` 走读桶。
//! `GET` 列表的 `cursor`/`next_cursor` 一律经 `api/mod.rs` 的统一签名 codec 对签发与校验。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use axum::routing::{get, patch};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{Permission, validate_permission_list};
use crate::error::ControllerError;
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{
    authenticate_read, authenticate_write, read_json_body, require_admin_session,
};

use super::{
    CursorFilterCtx, CursorKey, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, ManagementFuture,
    decode_management_list_cursor, management_cursor_secret, map_error, parse_revision,
    parse_uuid_path, sign_management_list_page,
};

#[derive(Clone)]
pub struct RolesApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn RolesGateway>,
}

pub trait RolesGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(
        &'a self,
        actor: &'a Session,
        name: &'a str,
        permissions: &'a [Permission],
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
        name: Option<&'a str>,
        permissions: Option<&'a [Permission]>,
        archived: Option<bool>,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

// ---------------------------------------------------------------------------
// 严格 query / body DTO
// ---------------------------------------------------------------------------

/// `GET /api/v1/roles` 的严格 query DTO：`deny_unknown_fields` 拒绝任何未知参数；
/// `Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RolesListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
}

/// `POST /api/v1/roles` 输入：两个字段全必填，未知字段 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRoleInput {
    pub(crate) name: String,
    pub(crate) permissions: Vec<String>,
}

/// `PATCH /api/v1/roles/{id}` 输入：`expected_revision` 必填十进制字符串（spec 02 §1），
/// 其余为可选 CAS 补丁字段；未知字段 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchRoleInput {
    pub(crate) expected_revision: String,
    pub(crate) name: Option<String>,
    pub(crate) permissions: Option<Vec<String>>,
    pub(crate) archived: Option<bool>,
}

/// 权限点文本列表解析与门禁：每项必须是合法 `Permission` 且整体非空、无重复
/// （`validate_permission_list` 唯一口径），任何违规一律 400 `INVALID_ARGUMENT`。
#[allow(clippy::result_large_err)]
fn parse_permissions(raw: &[String]) -> Result<Vec<Permission>, Response> {
    let mut parsed = Vec::with_capacity(raw.len());
    for value in raw {
        let permission =
            Permission::parse(value).ok_or_else(|| map_error(ControllerError::InvalidArgument))?;
        parsed.push(permission);
    }
    validate_permission_list(&parsed).map_err(map_error)?;
    Ok(parsed)
}

// ---------------------------------------------------------------------------
// 3 条管理 handler
// ---------------------------------------------------------------------------

/// `GET /api/v1/roles` 的 inner：`secret_provider` 真正调用发生在
/// `authenticate_read` → `require_admin_session` → query/limit 校验之后，且 `cursor`
/// 为 `None` 也必须先取密钥（未安装 → `Config` → `map_error` 精确 503，不得短路跳过）。
/// 客户端侧（query 非法、limit 越域、token 篡改/跨端点/跨用户）→ `InvalidArgument` 400；
/// 配置/后端侧按 Task 6 约定映射，绝不吞 `Err` 成 generic 500 或返回未签名页面。
async fn list_roles_with_secret_provider<F>(
    state: &RolesApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::RolesRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<RolesListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => DEFAULT_PAGE_LIMIT,
        Some(value) if (1..=MAX_PAGE_LIMIT).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    // roles 无筛选参数：`empty()` 上下文，`limit` 不参与 MAC 绑定。
    let filter = CursorFilterCtx::empty();
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(
        &secret,
        EndpointId::RolesRead,
        actor.user.id,
        &filter,
        query.cursor.as_deref(),
    ) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(raw))) => Some(Uuid::from_bytes(raw)),
        // `DeviceKey` 分支在 `RolesRead` 的 `Uuid` profile 下不可达，仍 fail-closed 400。
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(&secret, EndpointId::RolesRead, actor.user.id, &filter, page) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_roles(
    State(state): State<RolesApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_roles_with_secret_provider(&state, &headers, &uri, || {
        management_cursor_secret().copied()
    })
    .await
}

/// `POST /api/v1/roles`：创建角色。`permissions` 非空且去重由 `parse_permissions` 前置，
/// 非法 400；重名由网关映射为 `InvalidState` → 409 `INVALID_STATE`。
async fn create_role(
    State(state): State<RolesApiState>,
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
    let input = match read_json_body::<CreateRoleInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let permissions = match parse_permissions(&input.permissions) {
        Ok(permissions) => permissions,
        Err(response) => return response,
    };
    let created = match state
        .gateway
        .create(&actor, &input.name, &permissions)
        .await
    {
        Ok(created) => created,
        Err(error) => return map_error(error),
    };
    ok_response(created)
}

/// `PATCH /api/v1/roles/{id}`：CAS 补丁。内置角色不可归档由网关返回 `InvalidState`
/// → 409 `INVALID_STATE`；版本不符 → 409 `REVISION_CONFLICT`；不存在 → 404。
/// 从归档恢复（`archived: false`）必须携带合法非空 `permissions`，否则 400。
async fn patch_role(
    State(state): State<RolesApiState>,
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
    let id = match parse_uuid_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let input = match read_json_body::<PatchRoleInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    let permissions = match input.permissions.as_deref() {
        None => None,
        Some(raw) => match parse_permissions(raw) {
            Ok(permissions) => Some(permissions),
            Err(response) => return response,
        },
    };
    if input.archived == Some(false) && permissions.as_deref().is_none_or(<[Permission]>::is_empty)
    {
        return map_error(ControllerError::InvalidArgument);
    }
    let patched = match state
        .gateway
        .patch(
            &actor,
            id,
            revision,
            input.name.as_deref(),
            permissions.as_deref(),
            input.archived,
        )
        .await
    {
        Ok(patched) => patched,
        Err(error) => return map_error(error),
    };
    ok_response(patched)
}

/// 挂载 roles 的 3 条管理路由：集合级 `GET`/`POST` 同 path 共存且各只注册一次，
/// 单资源 `PATCH` 一处注册，无第二处重复挂载。
pub fn roles_router(state: RolesApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/roles", get(list_roles).post(create_role))
        .route("/api/v1/roles/{id}", patch(patch_role))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::test_fixture::*;
    use crate::error::ControllerError;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use std::sync::{Mutex, OnceLock};
    use tower::ServiceExt;

    /// canonical 小写 UUID 路径样本。
    const TARGET_ID: &str = "067e6162-ea94-4431-836c-7d8fa2010b3c";

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

    /// 纯内存 `RolesGateway`：`Echo` 回显 handler 传入的已解析参数，
    /// `BuiltinReject` 对 patch 恒返 `InvalidState`（内置角色不可归档）。
    #[derive(Clone, Copy)]
    enum FixtureRolesGateway {
        Echo,
        BuiltinReject,
    }

    impl RolesGateway for FixtureRolesGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            _cursor: Option<Uuid>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Ok(serde_json::json!({ "items": [], "next_cursor": null })) })
        }
        fn create<'a>(
            &'a self,
            _actor: &'a Session,
            name: &'a str,
            permissions: &'a [Permission],
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move {
                Ok(serde_json::json!({
                    "name": name,
                    "permissions": permissions
                        .iter()
                        .map(|permission| permission.as_str())
                        .collect::<Vec<_>>(),
                }))
            })
        }
        fn patch<'a>(
            &'a self,
            _actor: &'a Session,
            id: Uuid,
            revision: u64,
            name: Option<&'a str>,
            permissions: Option<&'a [Permission]>,
            archived: Option<bool>,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if matches!(gateway, Self::BuiltinReject) {
                    return Err(ControllerError::InvalidState);
                }
                Ok(serde_json::json!({
                    "id": id.to_string(),
                    "revision": revision,
                    "name": name,
                    "permissions": permissions.map(|list| {
                        list.iter()
                            .map(|permission| permission.as_str())
                            .collect::<Vec<_>>()
                    }),
                    "archived": archived,
                }))
            })
        }
    }

    fn roles_state_with_session(session: Session, gateway: FixtureRolesGateway) -> RolesApiState {
        RolesApiState {
            auth: fixture_auth(session),
            gateway: Arc::new(gateway),
        }
    }

    fn roles_state_with(gateway: FixtureRolesGateway) -> RolesApiState {
        roles_state_with_session(fixture_session(true), gateway)
    }

    /// 内置角色桩：patch 恒 `InvalidState` → 归档内置角色必须 409 `INVALID_STATE`。
    fn fixture_builtin_role_state() -> RolesApiState {
        roles_state_with(FixtureRolesGateway::BuiltinReject)
    }

    // RED 前置：骨架 `roles_router` 挂载 0 条路由，以下断言在实现前必须失败于
    // `404`（行为 RED），不得是编译错误。

    #[tokio::test]
    async fn builtin_role_archiving_is_rejected_with_409_invalid_state() {
        let router = roles_router(fixture_builtin_role_state());
        let req = authenticated_json_patch(
            "/api/v1/roles/00000000-0000-0000-0000-000000000001",
            serde_json::json!({ "expected_revision": "1", "archived": true }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        let val = body_json(res).await;
        assert_eq!(val["error"]["code"], "INVALID_STATE");
    }

    #[tokio::test]
    async fn create_role_rejects_empty_duplicate_or_unknown_permissions_with_400() {
        let cases = [
            serde_json::json!({ "name": "ops", "permissions": [] }),
            serde_json::json!({
                "name": "ops",
                "permissions": ["device.read", "device.read"],
            }),
            serde_json::json!({
                "name": "ops",
                "permissions": ["not.a.permission"],
            }),
            serde_json::json!({
                "name": "ops",
                "permissions": ["device.read"],
                "unknown": 1,
            }),
        ];
        for body in cases {
            let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
            let res = router
                .oneshot(authenticated_json_post("/api/v1/roles", body))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(code, "INVALID_ARGUMENT");
        }
    }

    #[tokio::test]
    async fn create_role_echoes_name_and_parsed_permissions() {
        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let req = authenticated_json_post(
            "/api/v1/roles",
            serde_json::json!({
                "name": "ops",
                "permissions": ["device.read", "device.reboot"],
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["name"], "ops");
        assert_eq!(
            value["data"]["permissions"],
            serde_json::json!(["device.read", "device.reboot"])
        );
        assert!(value["request_id"].is_string());
    }

    #[tokio::test]
    async fn restore_from_archive_without_valid_permissions_is_rejected_with_400() {
        let cases = [
            serde_json::json!({ "expected_revision": "1", "archived": false }),
            serde_json::json!({
                "expected_revision": "1",
                "archived": false,
                "permissions": [],
            }),
        ];
        for body in cases {
            let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
            let res = router
                .oneshot(authenticated_json_patch(
                    &format!("/api/v1/roles/{TARGET_ID}"),
                    body,
                ))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(code, "INVALID_ARGUMENT");
        }
    }

    #[tokio::test]
    async fn patch_role_echoes_parsed_cas_arguments() {
        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let req = authenticated_json_patch(
            &format!("/api/v1/roles/{TARGET_ID}"),
            serde_json::json!({
                "expected_revision": "7",
                "name": "ops2",
                "permissions": ["device.status.read"],
                "archived": true,
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["id"], TARGET_ID);
        assert_eq!(value["data"]["revision"], 7);
        assert_eq!(value["data"]["name"], "ops2");
        assert_eq!(
            value["data"]["permissions"],
            serde_json::json!(["device.status.read"])
        );
        assert_eq!(value["data"]["archived"], true);

        // 归档恢复（archived=false）携带合法非空 permissions 必须放行。
        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/roles/{TARGET_ID}"),
                serde_json::json!({
                    "expected_revision": "8",
                    "archived": false,
                    "permissions": ["device.task.read"],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["archived"], false);
        assert!(value["data"]["name"].is_null());
    }

    #[tokio::test]
    async fn patch_role_rejects_non_canonical_id_and_malformed_revision_with_400() {
        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let bad_id = router
            .oneshot(authenticated_json_patch(
                "/api/v1/roles/067E6162-EA94-4431-836C-7D8FA2010B3C",
                serde_json::json!({ "expected_revision": "1", "name": "ops" }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(bad_id).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.invalid_id");

        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let bad_revision = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/roles/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "+1", "name": "ops" }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(bad_revision).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.revision_invalid");
    }

    #[tokio::test]
    async fn list_rejects_unknown_query_and_out_of_range_limit_with_400() {
        for query in ["limit=0", "limit=201", "limit=abc", "foo=1", "user_id=aa"] {
            let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
            let res = router
                .oneshot(authenticated_get(&format!("/api/v1/roles?{query}")))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
        }
    }

    #[tokio::test]
    async fn unauthenticated_roles_requests_are_rejected_with_401() {
        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/roles")
                    .header(header::HOST, FIXTURE_HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/roles")
                    .header(header::HOST, FIXTURE_HOST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "name": "ops", "permissions": ["device.read"] })
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_admin_is_rejected_with_403_permission_denied() {
        let router = roles_router(roles_state_with_session(
            fixture_session(false),
            FixtureRolesGateway::Echo,
        ));
        let res = router
            .oneshot(authenticated_get("/api/v1/roles"))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn roles_write_without_csrf_token_is_rejected_with_403() {
        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/roles")
            .header(header::HOST, FIXTURE_HOST)
            .header(header::COOKIE, format!("rsc_session={}", "a".repeat(64)))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "name": "ops", "permissions": ["device.read"] }).to_string(),
            ))
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "CSRF_INVALID");
    }

    #[tokio::test]
    async fn unmounted_methods_on_roles_paths_are_not_allowed() {
        let router = roles_router(roles_state_with(FixtureRolesGateway::Echo));
        let res = router
            .oneshot(authenticated_json_write(
                "DELETE",
                &format!("/api/v1/roles/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "1" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // -----------------------------------------------------------------------
    // GET list 签名 cursor 真实接线（test-only wrapper + 局部 OnceLock 密钥槽）
    // -----------------------------------------------------------------------
    //
    // 生产 `list_roles` 恒只经 `management_cursor_secret()`；测试进程内全局
    // `MANAGEMENT_CURSOR_SECRET` 未安装，故 GET cursor 正负例不挂生产 router，改挂
    // 只包 `list_roles_with_secret_provider` 的 test-only wrapper：真实 gate/query/
    // codec/gateway 顺序不绕过，密钥经 `cursor_secret_of` 从本测试局部槽读取。
    // 红线：不 set 全局 OnceLock、不给 `RolesApiState` 扩 secret 字段。

    /// 与 users 区同值的测试合成 key，不引用任何 secret 文件。
    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];
    /// 网关内部 raw 32 hex（canonical v4 形态），出口必须被替换为 64 字符签名 token。
    const NEXT_RAW_HEX: &str = "067e6162ea944431836c7d8fa2010b3c";

    /// 记录每次 `list` 收到的 typed cursor 的页面桩：`cursor = None` 返回带 raw
    /// `next_cursor` 的第一页，否则返回末页。
    #[derive(Default)]
    struct PageRolesGateway {
        calls: Mutex<Vec<Option<Uuid>>>,
    }

    impl RolesGateway for PageRolesGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            cursor: Option<Uuid>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move {
                self.calls.lock().unwrap().push(cursor);
                let next_cursor = if cursor.is_none() {
                    serde_json::json!(NEXT_RAW_HEX)
                } else {
                    serde_json::Value::Null
                };
                Ok(serde_json::json!({
                    "items": [{ "id": NEXT_RAW_HEX }],
                    "next_cursor": next_cursor,
                }))
            })
        }
        fn create<'a>(
            &'a self,
            _actor: &'a Session,
            _name: &'a str,
            _permissions: &'a [Permission],
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn patch<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _revision: u64,
            _name: Option<&'a str>,
            _permissions: Option<&'a [Permission]>,
            _archived: Option<bool>,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    #[derive(Clone)]
    struct SignedRolesCursorTestState {
        roles: RolesApiState,
        secret_slot: Arc<OnceLock<[u8; 32]>>,
    }

    async fn list_roles_with_test_secret(
        State(state): State<SignedRolesCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = state.secret_slot.clone();
        list_roles_with_secret_provider(&state.roles, &headers, &uri, move || {
            crate::api::cursor_secret_of(&slot).copied()
        })
        .await
    }

    /// `Some` 时把 synthetic key 装入本 router 私有槽，`None` 留空作网关前 503 负例。
    fn signed_roles_cursor_router(
        secret: Option<[u8; 32]>,
        gateway: Arc<PageRolesGateway>,
    ) -> axum::Router<()> {
        let secret_slot = Arc::new(OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret)
                .expect("install synthetic secret");
        }
        let state = SignedRolesCursorTestState {
            roles: RolesApiState {
                auth: fixture_auth(fixture_session(true)),
                gateway,
            },
            secret_slot,
        };
        axum::Router::new()
            .route("/api/v1/roles", get(list_roles_with_test_secret))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_without_installed_secret_returns_503_before_gateway() {
        let gateway = Arc::new(PageRolesGateway::default());
        let router = signed_roles_cursor_router(None, gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/roles"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let value = body_json(res).await;
        assert_eq!(value["error"]["code"], "STORAGE_UNAVAILABLE");
        assert_eq!(
            gateway.calls.lock().unwrap().len(),
            0,
            "取密钥必须在 gateway.list 之前"
        );
    }

    #[tokio::test]
    async fn list_signs_first_page_and_decodes_second_page_cursor() {
        let gateway = Arc::new(PageRolesGateway::default());
        let router = signed_roles_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/roles"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        assert_eq!(token.len(), 64);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_ne!(token, NEXT_RAW_HEX, "裸 raw key 不得直传出口");
        // 第二页：原样回传 token 并换 limit=7——MAC 不绑定 limit，仍须 200。
        let router = signed_roles_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/roles?cursor={token}&limit=7"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert!(value["data"]["next_cursor"].is_null());
        let calls = gateway.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], None);
        let expected = Uuid::try_parse("067e6162-ea94-4431-836c-7d8fa2010b3c").expect("uuid");
        assert_eq!(
            calls[1],
            Some(expected),
            "网关必须收到 token 解回的 typed UUID"
        );
    }

    #[tokio::test]
    async fn list_rejects_tampered_cursor_before_gateway() {
        let gateway = Arc::new(PageRolesGateway::default());
        let router = signed_roles_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/roles"))
            .await
            .unwrap();
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        let mut raw = hex::decode(&token).expect("lowercase hex");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = hex::encode(&raw);
        let before = gateway.calls.lock().unwrap().len();
        let router = signed_roles_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/roles?cursor={tampered}"
            )))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(
            gateway.calls.lock().unwrap().len(),
            before,
            "客户端 400 不得触达网关"
        );
    }
}

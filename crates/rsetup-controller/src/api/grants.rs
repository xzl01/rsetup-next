//! grants 资源管理路由：网关契约与 3 条管理端点（Identity Task 8）。
//!
//! 契约（spec 02 §3/§4）：全部端点先过 `http_gate` 会话门禁，再过 `require_admin_session`
//! 管理前置，权威判定仍在仓储事务 guard 内重验；`POST`/`DELETE` 只按 `session.digest` 走
//! 写限速桶（`DELETE` 同样执行 CSRF 与单值 Origin 校验并读取 bounded JSON body），
//! `GET /api/v1/grants` 按 `(EndpointId::GrantsRead, session.digest)` 走读桶。
//! `GET` 列表的 `cursor`/`next_cursor` 一律经 `api/mod.rs` 的统一签名 codec 对签发与校验，
//! `user_id` 筛选以固定 schema 绑定进 cursor MAC，`limit` 不参与绑定。
//!
//! 事件线（06 §4 `permissions.changed`）：只有**目标用户明确**时才发布。
//! - `POST /api/v1/grants`：请求体的 `user_id` 即受影响用户，写成功（仓储同事务
//!   `bump_authz_epoch`）后向**该目标用户本人**发布一条提示；
//! - `DELETE /api/v1/grants/{id}`：同样 bump epoch，但网关契约只回显 `{ deleted: true }`、
//!   **不回显授权归属人**，本层无从确定受影响用户，因此**不发布**——把提示发给发起删除的
//!   管理员本人只是指向错人的虚假提示（他未必是该授权的归属人，真正的归属人反而收不到）。
//!   撤权安全性由全局 `authz_epoch` 递增 + SSE 读线每帧按连接 `authz_epoch` 权威复核直接
//!   关流保证（06 §4），不依赖这条提示。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use axum::routing::{delete, get};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::{GrantScope, GrantSource, Permission, validate_permission_list};
use crate::error::ControllerError;
use crate::events::EventBus;
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{
    authenticate_read, authenticate_write, read_json_body, require_admin_session,
};

use super::{
    CursorFilterCtx, CursorKey, DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, ManagementFuture,
    decode_management_list_cursor, management_cursor_secret, map_error, parse_device_id_path,
    parse_revision, parse_uuid_path, publish_permissions_changed, sign_management_list_page,
};

#[derive(Clone)]
pub struct GrantsApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn GrantsGateway>,
    /// 进程内事件总线句柄（06 §4 `permissions.changed`）：与 SSE 读线 `EventsState.bus`
    /// 是同一通道句柄。`None` 表示本装配未开启事件线，写端点行为与接入总线之前完全
    /// 一致（不发布、不报错、不阻塞）；发布返回值一律忽略，不得据此改变响应。
    pub events: Option<EventBus>,
}

/// `GrantSource`/`GrantScope` 来自唯一 policy 定义，不另造权限点。
pub trait GrantsGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        user_id: Option<[u8; 16]>,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn create<'a>(
        &'a self,
        actor: &'a Session,
        user_id: [u8; 16],
        source: GrantSource,
        scope: GrantScope,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn delete<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
    ) -> ManagementFuture<'a, ()>;
}

// ---------------------------------------------------------------------------
// 严格 query / body DTO
// ---------------------------------------------------------------------------

/// `GET /api/v1/grants` 的严格 query DTO：`deny_unknown_fields` 拒绝任何未知参数；
/// `Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantsListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) limit: Option<usize>,
}

/// `POST /api/v1/grants` 输入：三个字段全必填，未知字段 400；`source`/`scope`
/// 的成对校验（kind 与携带字段互锁）在 handler 内完成。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateGrantInput {
    pub(crate) user_id: String,
    pub(crate) source: GrantSourceInput,
    pub(crate) scope: GrantScopeInput,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantSourceInput {
    pub(crate) kind: String,
    pub(crate) role_id: Option<String>,
    pub(crate) permissions: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantScopeInput {
    pub(crate) kind: String,
    pub(crate) group_id: Option<String>,
    pub(crate) device_id: Option<String>,
}

/// `DELETE /api/v1/grants/{id}` 输入：仅 CAS 版本号（DELETE 仍需 JSON body/CSRF）。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteGrantInput {
    pub(crate) expected_revision: String,
}

/// 权限点文本列表解析与门禁：与 `api/roles.rs` 同口径（`validate_permission_list`
/// 唯一真源），任何违规一律 400 `INVALID_ARGUMENT`。
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

/// `source` 成对校验：`role` 必须且只能携带 `role_id`（canonical UUID）；
/// `direct` 必须且只能携带合法非空去重的 `permissions`；其余一律 400。
#[allow(clippy::result_large_err)]
fn parse_grant_source(input: &GrantSourceInput) -> Result<GrantSource, Response> {
    match input.kind.as_str() {
        "role" => {
            if input.permissions.is_some() {
                return Err(map_error(ControllerError::InvalidArgument));
            }
            let Some(raw) = input.role_id.as_deref() else {
                return Err(map_error(ControllerError::InvalidArgument));
            };
            let role_id = parse_uuid_path(raw)?;
            Ok(GrantSource::Role(role_id))
        }
        "direct" => {
            if input.role_id.is_some() {
                return Err(map_error(ControllerError::InvalidArgument));
            }
            let Some(raw) = input.permissions.as_ref() else {
                return Err(map_error(ControllerError::InvalidArgument));
            };
            let permissions = parse_permissions(raw)?;
            Ok(GrantSource::Direct(permissions))
        }
        _ => Err(map_error(ControllerError::InvalidArgument)),
    }
}

/// `scope` 成对校验：`all` 不携带任何 id；`group` 必须且只能携带 `group_id`
/// （canonical UUID）；`device` 必须且只能携带 64 位小写 hex 的 `device_id`；其余一律 400。
#[allow(clippy::result_large_err)]
fn parse_grant_scope(input: &GrantScopeInput) -> Result<GrantScope, Response> {
    match input.kind.as_str() {
        "all" => {
            if input.group_id.is_some() || input.device_id.is_some() {
                return Err(map_error(ControllerError::InvalidArgument));
            }
            Ok(GrantScope::All)
        }
        "group" => {
            if input.device_id.is_some() {
                return Err(map_error(ControllerError::InvalidArgument));
            }
            let Some(raw) = input.group_id.as_deref() else {
                return Err(map_error(ControllerError::InvalidArgument));
            };
            let group_id = parse_uuid_path(raw)?;
            Ok(GrantScope::Group(group_id))
        }
        "device" => {
            if input.group_id.is_some() {
                return Err(map_error(ControllerError::InvalidArgument));
            }
            let Some(raw) = input.device_id.as_deref() else {
                return Err(map_error(ControllerError::InvalidArgument));
            };
            let device_id = parse_device_id_path(raw)?;
            Ok(GrantScope::Device(device_id))
        }
        _ => Err(map_error(ControllerError::InvalidArgument)),
    }
}

// ---------------------------------------------------------------------------
// 3 条管理 handler
// ---------------------------------------------------------------------------

/// grants query schema 唯一筛选字段 `user_id` 的固定 tag；`limit` 不进 schema、
/// 不参与 MAC 绑定。
const GRANTS_FILTER_TAG_USER_ID: u8 = 1;

/// `GET /api/v1/grants` 的 inner：`secret_provider` 真正调用发生在
/// `authenticate_read` → `require_admin_session` → query/limit/`user_id` 校验之后，
/// 且 `cursor` 为 `None` 也必须先取密钥（未安装 → `Config` → `map_error` 精确 503，
/// 不得短路跳过）。入口与出口复用同一请求内同一 `CursorFilterCtx` 实例：下一页必须
/// 携带同一 `user_id` 文本，改写或缺省 → filter 不一致 → MAC 校验失败 400。
async fn list_grants_with_secret_provider<F>(
    state: &GrantsApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::GrantsRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<GrantsListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => DEFAULT_PAGE_LIMIT,
        Some(value) if (1..=MAX_PAGE_LIMIT).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    // `user_id` 复用唯一 UUID helper：非规范小写 36 字符一律 400。
    let user_id = match query.user_id.as_deref() {
        None => None,
        Some(raw) => match parse_uuid_path(raw) {
            Ok(id) => Some(*id.as_bytes()),
            Err(response) => return response,
        },
    };
    // filter 只经 `from_ordered_fields` 构造：缺省 → `[(1, None)]`（canonical 单个
    // `0` 字节，与存在态可区分），不用 `empty()`。
    let filter = match user_id {
        None => CursorFilterCtx::from_ordered_fields(&[(GRANTS_FILTER_TAG_USER_ID, None)]),
        Some(raw) => CursorFilterCtx::from_ordered_fields(&[(
            GRANTS_FILTER_TAG_USER_ID,
            Some(raw.as_slice()),
        )]),
    };
    let filter = match filter {
        Ok(filter) => filter,
        Err(error) => return map_error(error),
    };
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(
        &secret,
        EndpointId::GrantsRead,
        actor.user.id,
        &filter,
        query.cursor.as_deref(),
    ) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(raw))) => Some(Uuid::from_bytes(raw)),
        // `DeviceKey` 分支在 `GrantsRead` 的 `Uuid` profile 下不可达，仍 fail-closed 400。
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, user_id, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(
        &secret,
        EndpointId::GrantsRead,
        actor.user.id,
        &filter,
        page,
    ) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_grants(
    State(state): State<GrantsApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_grants_with_secret_provider(&state, &headers, &uri, || {
        management_cursor_secret().copied()
    })
    .await
}

/// `POST /api/v1/grants`：创建授权。`user_id`/`source`/`scope` 的成对与有效性校验
/// 全部先于网关，非法 400；目标用户不存在由网关统一 404。
async fn create_grant(
    State(state): State<GrantsApiState>,
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
    let input = match read_json_body::<CreateGrantInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let user_id = match parse_uuid_path(&input.user_id) {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let source = match parse_grant_source(&input.source) {
        Ok(source) => source,
        Err(response) => return response,
    };
    let scope = match parse_grant_scope(&input.scope) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let user_id_raw = user_id.into_bytes();
    let created = match state
        .gateway
        .create(&actor, user_id_raw, source, scope)
        .await
    {
        Ok(created) => created,
        Err(error) => return map_error(error),
    };
    // 新增授权只改变该目标用户的有效权限：受众即他本人（与载荷 `id` 同一份字节）。
    if let Some(ref bus) = state.events {
        publish_permissions_changed(bus, &state.auth, user_id_raw).await;
    }
    ok_response(created)
}

/// `DELETE /api/v1/grants/{id}`：读取 bounded JSON body 取得 `expected_revision`，
/// CSRF 与单值 Origin 校验由 `authenticate_write` 完成；版本不符 → 409
/// `REVISION_CONFLICT`；不存在 → 404；成功固定投影 `{ deleted: true }`。
/// 删除成功同样 bump 授权 epoch：网关契约不回显授权归属人，本层拿不到受影响用户，因此
/// **不发布** `permissions.changed`（发给操作者管理员即是指向错人的虚假提示）；撤权安全性
/// 由全局 `authz_epoch` 递增 + SSE 读线的权威复核关流保证（06 §4）。
async fn delete_grant(
    State(state): State<GrantsApiState>,
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
    let input = match read_json_body::<DeleteGrantInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    match state.gateway.delete(&actor, id, revision).await {
        Ok(()) => ok_response(serde_json::json!({ "deleted": true })),
        Err(error) => map_error(error),
    }
}

/// 挂载 grants 的 3 条管理路由：集合级 `GET`/`POST` 同 path 共存且各只注册一次，
/// 单资源 `DELETE` 一处注册，无第二处重复挂载。
pub fn grants_router(state: GrantsApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/grants", get(list_grants).post(create_grant))
        .route("/api/v1/grants/{id}", delete(delete_grant))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::test_fixture::*;
    use crate::error::ControllerError;
    use crate::events::{EventAudience, EventEnvelope, EventName, EventViewer};
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use std::sync::{Mutex, OnceLock};
    use tower::ServiceExt;

    /// canonical 小写 UUID 路径/query 样本。
    const TARGET_ID: &str = "067e6162-ea94-4431-836c-7d8fa2010b3c";
    const TARGET_ID_RAW: &str = "067e6162ea944431836c7d8fa2010b3c";
    const OTHER_USER: &str = "067e6162-ea94-4431-836c-7d8fa2010b3d";
    /// 64 位小写 hex 设备 ID 样本。
    const TARGET_DEVICE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

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

    /// 纯内存 `GrantsGateway`：`Allow` 固定投影（400 必须由 handler 的 body 校验先于
    /// 网关产生）、`Echo` 回显已解析参数、`RevisionConflict` 对 delete 恒 409。
    #[derive(Clone, Copy)]
    enum FixtureGrantsGateway {
        Allow,
        Echo,
        RevisionConflict,
    }

    impl GrantsGateway for FixtureGrantsGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            _user_id: Option<[u8; 16]>,
            _cursor: Option<Uuid>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Ok(serde_json::json!({ "items": [], "next_cursor": null })) })
        }
        fn create<'a>(
            &'a self,
            _actor: &'a Session,
            user_id: [u8; 16],
            source: GrantSource,
            scope: GrantScope,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move {
                if matches!(self, Self::Allow) {
                    return Ok(serde_json::json!({}));
                }
                let source = match source {
                    GrantSource::Role(role_id) => serde_json::json!({
                        "kind": "role",
                        "role_id": role_id.to_string(),
                    }),
                    GrantSource::Direct(permissions) => serde_json::json!({
                        "kind": "direct",
                        "permissions": permissions
                            .iter()
                            .map(|permission| permission.as_str())
                            .collect::<Vec<_>>(),
                    }),
                };
                let scope = match scope {
                    GrantScope::All => serde_json::json!({ "kind": "all" }),
                    GrantScope::Group(group_id) => serde_json::json!({
                        "kind": "group",
                        "group_id": group_id.to_string(),
                    }),
                    GrantScope::Device(device_id) => serde_json::json!({
                        "kind": "device",
                        "device_id": hex::encode(device_id),
                    }),
                };
                Ok(serde_json::json!({
                    "user_id": hex::encode(user_id),
                    "source": source,
                    "scope": scope,
                }))
            })
        }
        fn delete<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _revision: u64,
        ) -> ManagementFuture<'a, ()> {
            let gateway = *self;
            Box::pin(async move {
                if matches!(gateway, Self::RevisionConflict) {
                    return Err(ControllerError::RevisionConflict);
                }
                Ok(())
            })
        }
    }

    fn grants_state_with_session(
        session: Session,
        gateway: FixtureGrantsGateway,
    ) -> GrantsApiState {
        grants_state_with_events(session, gateway, None)
    }

    /// 事件总线可选的装配：`Some(bus)` 与 SSE 读线 `EventsState.bus` 是同一通道句柄，
    /// `None`（既有全部用例的基线）表示本装配未开启事件线。
    fn grants_state_with_events(
        session: Session,
        gateway: FixtureGrantsGateway,
        events: Option<EventBus>,
    ) -> GrantsApiState {
        GrantsApiState {
            auth: fixture_auth(session),
            gateway: Arc::new(gateway),
            events,
        }
    }

    fn grants_state_with(gateway: FixtureGrantsGateway) -> GrantsApiState {
        grants_state_with_session(fixture_session(true), gateway)
    }

    /// 恒 `Ok` 桩：400 一律由 body 校验先于网关产生。
    fn fixture_grants_state() -> GrantsApiState {
        grants_state_with(FixtureGrantsGateway::Allow)
    }

    // RED 前置：骨架 `grants_router` 挂载 0 条路由，以下断言在实现前必须失败于
    // `404`（行为 RED），不得是编译错误。

    #[tokio::test]
    async fn grant_creation_rejects_empty_direct_permissions_with_400() {
        let router = grants_router(fixture_grants_state());
        let req = authenticated_json_post(
            "/api/v1/grants",
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "direct", "permissions": [] },
                "scope": { "kind": "all" },
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn grant_creation_rejects_unpaired_source_or_scope_with_400() {
        let cases = [
            // source：role 缺 role_id / role 夹带 permissions / direct 夹带 role_id /
            // direct 缺 permissions / 未知 kind。
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "role" },
                "scope": { "kind": "all" },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": {
                    "kind": "role",
                    "role_id": OTHER_USER,
                    "permissions": ["device.read"],
                },
                "scope": { "kind": "all" },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": {
                    "kind": "direct",
                    "role_id": OTHER_USER,
                    "permissions": ["device.read"],
                },
                "scope": { "kind": "all" },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "direct" },
                "scope": { "kind": "all" },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "unknown" },
                "scope": { "kind": "all" },
            }),
            // scope：group 缺 group_id / group 夹带 device_id / device 非法 hex /
            // all 夹带 group_id / 未知 kind。
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "role", "role_id": OTHER_USER },
                "scope": { "kind": "group" },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "role", "role_id": OTHER_USER },
                "scope": { "kind": "group", "group_id": OTHER_USER, "device_id": TARGET_DEVICE },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "role", "role_id": OTHER_USER },
                "scope": { "kind": "device", "device_id": "AB" },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "role", "role_id": OTHER_USER },
                "scope": { "kind": "all", "group_id": OTHER_USER },
            }),
            serde_json::json!({
                "user_id": TARGET_ID,
                "source": { "kind": "role", "role_id": OTHER_USER },
                "scope": { "kind": "unknown" },
            }),
            // user_id 非 canonical 小写。
            serde_json::json!({
                "user_id": TARGET_ID.to_uppercase(),
                "source": { "kind": "role", "role_id": OTHER_USER },
                "scope": { "kind": "all" },
            }),
        ];
        for body in cases {
            let router = grants_router(fixture_grants_state());
            let res = router
                .oneshot(authenticated_json_post("/api/v1/grants", body))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(code, "INVALID_ARGUMENT");
        }
    }

    #[tokio::test]
    async fn grant_creation_echoes_parsed_user_source_and_scope() {
        let cases = [
            (
                serde_json::json!({
                    "user_id": TARGET_ID,
                    "source": { "kind": "role", "role_id": OTHER_USER },
                    "scope": { "kind": "group", "group_id": OTHER_USER },
                }),
                serde_json::json!({
                    "kind": "role",
                    "role_id": OTHER_USER,
                }),
                serde_json::json!({
                    "kind": "group",
                    "group_id": OTHER_USER,
                }),
            ),
            (
                serde_json::json!({
                    "user_id": TARGET_ID,
                    "source": { "kind": "direct", "permissions": ["device.read"] },
                    "scope": { "kind": "all" },
                }),
                serde_json::json!({
                    "kind": "direct",
                    "permissions": ["device.read"],
                }),
                serde_json::json!({ "kind": "all" }),
            ),
            (
                serde_json::json!({
                    "user_id": TARGET_ID,
                    "source": { "kind": "role", "role_id": OTHER_USER },
                    "scope": { "kind": "device", "device_id": TARGET_DEVICE },
                }),
                serde_json::json!({
                    "kind": "role",
                    "role_id": OTHER_USER,
                }),
                serde_json::json!({
                    "kind": "device",
                    "device_id": TARGET_DEVICE,
                }),
            ),
        ];
        for (body, source, scope) in cases {
            let router = grants_router(grants_state_with(FixtureGrantsGateway::Echo));
            let res = router
                .oneshot(authenticated_json_post("/api/v1/grants", body))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let value = body_json(res).await;
            assert_eq!(value["data"]["user_id"], TARGET_ID_RAW);
            assert_eq!(value["data"]["source"], source);
            assert_eq!(value["data"]["scope"], scope);
        }
    }

    #[tokio::test]
    async fn delete_grant_with_csrf_returns_deleted_true() {
        let router = grants_router(fixture_grants_state());
        let req = authenticated_json_write(
            "DELETE",
            &format!("/api/v1/grants/{TARGET_ID}"),
            serde_json::json!({ "expected_revision": "3" }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["deleted"], true);
        assert!(value["request_id"].is_string());
    }

    #[tokio::test]
    async fn delete_grant_without_csrf_is_rejected_with_403() {
        let router = grants_router(fixture_grants_state());
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/grants/{TARGET_ID}"))
            .header(header::HOST, FIXTURE_HOST)
            .header(header::COOKIE, format!("rsc_session={}", "a".repeat(64)))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "expected_revision": "3" }).to_string(),
            ))
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "CSRF_INVALID");
    }

    #[tokio::test]
    async fn delete_grant_revision_conflict_returns_409() {
        let router = grants_router(grants_state_with(FixtureGrantsGateway::RevisionConflict));
        let req = authenticated_json_write(
            "DELETE",
            &format!("/api/v1/grants/{TARGET_ID}"),
            serde_json::json!({ "expected_revision": "9" }),
        );
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "REVISION_CONFLICT");
    }

    #[tokio::test]
    async fn delete_grant_rejects_non_canonical_id_and_malformed_revision_with_400() {
        let router = grants_router(fixture_grants_state());
        let bad_id = router
            .oneshot(authenticated_json_write(
                "DELETE",
                "/api/v1/grants/067E6162-EA94-4431-836C-7D8FA2010B3C",
                serde_json::json!({ "expected_revision": "1" }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(bad_id).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.invalid_id");

        let router = grants_router(fixture_grants_state());
        let bad_revision = router
            .oneshot(authenticated_json_write(
                "DELETE",
                &format!("/api/v1/grants/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "" }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(bad_revision).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.revision_invalid");
    }

    #[tokio::test]
    async fn list_rejects_bad_query_user_id_and_limit_with_400() {
        for query in [
            "limit=0",
            "limit=201",
            "foo=1",
            &format!("user_id={}", TARGET_ID.to_uppercase()),
            "user_id=",
            "user_id=067e6162ea944431836c7d8fa2010b3c",
        ] {
            let router = grants_router(fixture_grants_state());
            let res = router
                .oneshot(authenticated_get(&format!("/api/v1/grants?{query}")))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
        }
    }

    #[tokio::test]
    async fn unauthenticated_grants_requests_are_rejected_with_401() {
        let router = grants_router(fixture_grants_state());
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/grants")
                    .header(header::HOST, FIXTURE_HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let router = grants_router(fixture_grants_state());
        let res = router
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/v1/grants/{TARGET_ID}"))
                    .header(header::HOST, FIXTURE_HOST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "expected_revision": "1" }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn non_admin_is_rejected_with_403_permission_denied() {
        let router = grants_router(grants_state_with_session(
            fixture_session(false),
            FixtureGrantsGateway::Allow,
        ));
        let res = router
            .oneshot(authenticated_get("/api/v1/grants"))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn unmounted_methods_on_grants_paths_are_not_allowed() {
        let router = grants_router(fixture_grants_state());
        let res = router
            .oneshot(authenticated_json_write(
                "PATCH",
                &format!("/api/v1/grants/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "1" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    // -----------------------------------------------------------------------
    // GET list 签名 cursor 真实接线与 user_id 筛选绑定
    // （test-only wrapper + 局部 OnceLock 密钥槽）
    // -----------------------------------------------------------------------
    //
    // 生产 `list_grants` 恒只经 `management_cursor_secret()`；测试进程内全局
    // `MANAGEMENT_CURSOR_SECRET` 未安装，故 GET cursor 正负例不挂生产 router，改挂
    // 只包 `list_grants_with_secret_provider` 的 test-only wrapper：真实 gate/query/
    // codec/gateway 顺序不绕过，密钥经 `cursor_secret_of` 从本测试局部槽读取。
    // 红线：不 set 全局 OnceLock、不给 `GrantsApiState` 扩 secret 字段。

    /// 与 users/roles 区同值的测试合成 key，不引用任何 secret 文件。
    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];
    /// 网关内部 raw 32 hex（canonical v4 形态），出口必须被替换为 64 字符签名 token。
    const NEXT_RAW_HEX: &str = "067e6162ea944431836c7d8fa2010b3c";

    /// 网关记录形状 `(user_id, cursor)` 的类型别名（规避 `clippy::type_complexity`）。
    type GrantsListCall = (Option<[u8; 16]>, Option<Uuid>);

    /// 记录每次 `list` 收到的 `(user_id, cursor)` 的页面桩：`cursor = None` 返回带
    /// raw `next_cursor` 的第一页，否则返回末页。
    #[derive(Default)]
    struct PageGrantsGateway {
        calls: Mutex<Vec<GrantsListCall>>,
    }

    impl GrantsGateway for PageGrantsGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            user_id: Option<[u8; 16]>,
            cursor: Option<Uuid>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move {
                self.calls.lock().unwrap().push((user_id, cursor));
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
            _user_id: [u8; 16],
            _source: GrantSource,
            _scope: GrantScope,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn delete<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _revision: u64,
        ) -> ManagementFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    #[derive(Clone)]
    struct SignedGrantsCursorTestState {
        grants: GrantsApiState,
        secret_slot: Arc<OnceLock<[u8; 32]>>,
    }

    async fn list_grants_with_test_secret(
        State(state): State<SignedGrantsCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = state.secret_slot.clone();
        list_grants_with_secret_provider(&state.grants, &headers, &uri, move || {
            crate::api::cursor_secret_of(&slot).copied()
        })
        .await
    }

    /// `Some` 时把 synthetic key 装入本 router 私有槽，`None` 留空作网关前 503 负例。
    fn signed_grants_cursor_router(
        secret: Option<[u8; 32]>,
        gateway: Arc<PageGrantsGateway>,
    ) -> axum::Router<()> {
        let secret_slot = Arc::new(OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret)
                .expect("install synthetic secret");
        }
        let state = SignedGrantsCursorTestState {
            grants: GrantsApiState {
                auth: fixture_auth(fixture_session(true)),
                gateway,
                events: None,
            },
            secret_slot,
        };
        axum::Router::new()
            .route("/api/v1/grants", get(list_grants_with_test_secret))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_without_installed_secret_returns_503_before_gateway() {
        let gateway = Arc::new(PageGrantsGateway::default());
        let router = signed_grants_cursor_router(None, gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/grants"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let value = body_json(res).await;
        assert_eq!(value["error"]["code"], "STORAGE_UNAVAILABLE");
        assert_eq!(
            gateway.calls.lock().unwrap().len(),
            0,
            "取密钥必须在 gateway.list 之前（含 cursor=None 路径）"
        );
    }

    #[tokio::test]
    async fn list_signs_first_page_and_decodes_second_page_with_same_user_id() {
        let gateway = Arc::new(PageGrantsGateway::default());
        let router = signed_grants_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/grants?user_id={TARGET_ID}"
            )))
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
        // 第二页：携带同一 user_id 文本 + token 并换 limit=7——MAC 不绑定 limit，仍须 200。
        let router = signed_grants_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/grants?user_id={TARGET_ID}&cursor={token}&limit=7"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert!(value["data"]["next_cursor"].is_null());
        let calls = gateway.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        let expected_user = Uuid::try_parse(TARGET_ID).expect("uuid").into_bytes();
        let expected_cursor =
            Uuid::try_parse("067e6162-ea94-4431-836c-7d8fa2010b3c").expect("uuid");
        assert_eq!(calls[0], (Some(expected_user), None));
        assert_eq!(
            calls[1],
            (Some(expected_user), Some(expected_cursor)),
            "网关必须收到规范化的 user_id 与 token 解回的 typed UUID"
        );
    }

    #[tokio::test]
    async fn list_cursor_rejects_changed_or_missing_user_id_with_400() {
        let gateway = Arc::new(PageGrantsGateway::default());
        let router = signed_grants_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/grants?user_id={TARGET_ID}"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        // 下一页改写或缺省 user_id → filter canonical 不一致 → MAC 失败 400，且不得触达网关。
        for query in [
            format!("user_id={OTHER_USER}&cursor={token}"),
            format!("cursor={token}"),
        ] {
            let router = signed_grants_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
            let res = router
                .oneshot(authenticated_get(&format!("/api/v1/grants?{query}")))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
        }
        assert_eq!(
            gateway.calls.lock().unwrap().len(),
            1,
            "筛选不匹配的 400 不得触达网关"
        );
    }

    // -----------------------------------------------------------------------
    // 授权变更的 `permissions.changed` 事件发布（06 §4）
    // -----------------------------------------------------------------------

    /// 收集订阅通道当前待读的事件（订阅只收「之后」的事件，不回放历史）。
    fn collect_events(
        rx: &mut tokio::sync::broadcast::Receiver<EventEnvelope>,
    ) -> Vec<EventEnvelope> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// 带事件总线的 grants 装配：`epoch` 是本装配报告的全局 authz_epoch。
    fn grants_router_with_bus(
        gateway: FixtureGrantsGateway,
        bus: Option<EventBus>,
        epoch: u64,
    ) -> axum::Router<()> {
        grants_router(GrantsApiState {
            auth: fixture_auth_with_epoch(fixture_session(true), epoch),
            gateway: Arc::new(gateway),
            events: bus,
        })
    }

    /// 正例：`POST /api/v1/grants` 的受影响用户由请求体 `user_id` 唯一确定，受众必须是
    /// **该目标用户本人**（而不是发起变更的管理员），载荷 `id` 与他同一份字节。
    #[tokio::test]
    async fn create_grant_publishes_permissions_changed_to_target_user() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = grants_router_with_bus(FixtureGrantsGateway::Echo, Some(bus), 42);
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/grants",
                serde_json::json!({
                    "user_id": TARGET_ID,
                    "source": { "kind": "direct", "permissions": ["device.read"] },
                    "scope": { "kind": "all" },
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1, "新增授权必须恰好发布一条提示");
        let event = &events[0];
        assert_eq!(event.name(), EventName::PermissionsChanged);
        assert_eq!(event.data()["kind"], serde_json::json!("permissions"));
        assert_eq!(event.data()["id"], serde_json::json!(TARGET_ID));
        assert_eq!(event.data()["epoch"], serde_json::json!("42"));
        assert!(event.data()["epoch"].is_string());
        let target =
            <[u8; 16]>::try_from(hex::decode(TARGET_ID_RAW).expect("hex").as_slice()).expect("16B");
        assert_eq!(event.audience(), EventAudience::User(target));
        // 撤权者本人（管理员）看不到这条属于目标用户的私有事实。
        assert!(!event.visible_to(EventViewer {
            user_id: fixture_session(true).user.id,
            is_admin: true,
        }));
        assert!(event.visible_to(EventViewer {
            user_id: target,
            is_admin: false,
        }));
    }

    /// 语义收敛（审查反馈 Medium-1）：`DELETE /{id}` 成功同样 bump epoch，但网关不回显
    /// 授权归属人，本层拿不到受影响用户，因此**不得**向操作者管理员发一条关于其本人的
    /// `permissions.changed`。撤权安全性由全局 `authz_epoch` 递增 + SSE 读线每帧按连接
    /// `authz_epoch` 权威复核直接关流保证（06 §4）。
    #[tokio::test]
    async fn delete_grant_publishes_no_permissions_changed_without_target_user() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = grants_router_with_bus(FixtureGrantsGateway::Allow, Some(bus.clone()), 9);
        let res = router
            .oneshot(authenticated_json_write(
                "DELETE",
                &format!("/api/v1/grants/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "1" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            collect_events(&mut rx).len(),
            0,
            "受影响用户不可确定时不得向操作者发布 permissions.changed"
        );
        // 反向证据：同一订阅者确实能收到事件，说明上面的 0 条是「未发布」而非「订阅失效」。
        assert_eq!(
            bus.publish(EventEnvelope::permissions_changed(
                fixture_session(true).user.id,
                9,
            )),
            1
        );
    }

    /// 负例：`events: None` 的既有装配、以及写失败（409）都不得有任何事件进入通道。
    #[tokio::test]
    async fn grant_writes_without_bus_or_on_failure_publish_nothing() {
        // 1) `events: None`：创建成功也不得有任何事件进入通道。
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = grants_router(grants_state_with(FixtureGrantsGateway::Echo));
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/grants",
                serde_json::json!({
                    "user_id": TARGET_ID,
                    "source": { "kind": "direct", "permissions": ["device.read"] },
                    "scope": { "kind": "all" },
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(collect_events(&mut rx).len(), 0, "events: None 不得有事件");

        // 2) 有总线但 CAS 失败（409）。
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = grants_router_with_bus(FixtureGrantsGateway::RevisionConflict, Some(bus), 42);
        let res = router
            .oneshot(authenticated_json_write(
                "DELETE",
                &format!("/api/v1/grants/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "1" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
        assert_eq!(collect_events(&mut rx).len(), 0, "409 未发生变更，不得发布");
    }
}

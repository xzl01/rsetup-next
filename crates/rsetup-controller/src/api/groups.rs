//! groups 资源管理路由：网关契约与 4 条管理端点（Identity Task 9）。
//!
//! 契约（spec 02 §3/§4）：全部端点先过 `http_gate` 会话门禁，再过 `require_admin_session`
//! 管理前置，权威判定仍在仓储事务 guard 内重验；`POST`/`PATCH`/`PUT` 只按 `session.digest`
//! 走写限速桶，`GET /api/v1/groups` 按 `(EndpointId::GroupsRead, session.digest)` 走读桶。
//! `GET` 列表的 `cursor`/`next_cursor` 一律经 `api/mod.rs` 的统一签名 codec 对签发与校验；
//! `PUT /{id}/members` 限制 `device_ids.len() <= 1024`，逐项校验小写 hex64 并去重后全量替换。
//!
//! 事件线（06 §4 `permissions.changed`）：本文件**不发布**任何 `permissions.changed`。
//! 该事件的受众是**单个受影响用户**（受众与载荷 `id` 同一份字节），而 `PATCH /{id}`
//! （改名/归档）影响的是**引用该组的授权可见范围**内的用户集合（仓储同事务
//! `bump_authz_epoch`），本层拿不到该集合，把提示发给操作者管理员即是虚假提示（他未必
//! 在受影响范围内）。`POST /api/v1/groups` 新建的空组此刻尚未被任何授权引用；
//! `PUT /{id}/members` 改的是设备成员集合，仓储在该路径**不**递增 `authz_epoch`，也没有
//! 可信的新 epoch 可发布。撤权安全性由全局 `authz_epoch` 递增 + SSE 读线每帧按连接
//! `authz_epoch` 权威复核直接关流保证（06 §4），不依赖这条提示。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use axum::routing::{get, patch, put};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::sqlx_repo::validate_group_name;
use crate::error::ControllerError;
use crate::events::EventBus;
use crate::http_api::{err_response, ok_response};
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
pub struct GroupsApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn GroupsGateway>,
    /// 进程内事件总线句柄（06 §4）：与 SSE 读线 `EventsState.bus` 是同一通道句柄，
    /// 保留以与其它 management 条线同形装配（生产组装统一注入）。groups 条线本身
    /// **不发布任何事件**（见模块头的受众口径），因此该句柄在本文件内不被读取。
    pub events: Option<EventBus>,
}

pub trait GroupsGateway: Send + Sync {
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
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
        name: Option<&'a str>,
        archived: Option<bool>,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn replace_members<'a>(
        &'a self,
        actor: &'a Session,
        id: Uuid,
        revision: u64,
        devices: &'a [[u8; 32]],
    ) -> ManagementFuture<'a, serde_json::Value>;
}

/// 单次全量替换的组成员数上限（spec 02 §3：超限 400，不进网关）。
const MAX_GROUP_MEMBERS: usize = 1024;

// ---------------------------------------------------------------------------
// 严格 query / body DTO
// ---------------------------------------------------------------------------

/// `GET /api/v1/groups` 的严格 query DTO：`deny_unknown_fields` 拒绝任何未知参数；
/// `Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GroupsListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
}

/// `POST /api/v1/groups` 输入：`name` 必填，未知字段 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateGroupInput {
    pub(crate) name: String,
}

/// `PATCH /api/v1/groups/{id}` 输入：`expected_revision` 必填十进制字符串（spec 02 §1），
/// 其余为可选 CAS 补丁字段；未知字段 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchGroupInput {
    pub(crate) expected_revision: String,
    pub(crate) name: Option<String>,
    pub(crate) archived: Option<bool>,
}

/// `PUT /api/v1/groups/{id}/members` 输入：CAS 版本号 + 全量成员设备 ID 列表。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReplaceMembersInput {
    pub(crate) expected_revision: String,
    pub(crate) device_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// 4 条管理 handler
// ---------------------------------------------------------------------------

/// `GET /api/v1/groups` 的 inner：`secret_provider` 真正调用发生在
/// `authenticate_read` → `require_admin_session` → query/limit 校验之后，且 `cursor`
/// 为 `None` 也必须先取密钥（未安装 → `Config` → `map_error` 精确 503，不得短路跳过）。
/// 客户端侧（query 非法、limit 越域、token 篡改/跨端点/跨用户）→ `InvalidArgument` 400；
/// 配置/后端侧按 Task 6 约定映射，绝不吞 `Err` 成 generic 500 或返回未签名页面。
async fn list_groups_with_secret_provider<F>(
    state: &GroupsApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::GroupsRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<GroupsListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => DEFAULT_PAGE_LIMIT,
        Some(value) if (1..=MAX_PAGE_LIMIT).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    // groups 无筛选参数：`empty()` 上下文，`limit` 不参与 MAC 绑定。
    let filter = CursorFilterCtx::empty();
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(
        &secret,
        EndpointId::GroupsRead,
        actor.user.id,
        &filter,
        query.cursor.as_deref(),
    ) {
        Ok(None) => None,
        Ok(Some(CursorKey::Uuid(raw))) => Some(Uuid::from_bytes(raw)),
        // `DeviceKey` 分支在 `GroupsRead` 的 `Uuid` profile 下不可达，仍 fail-closed 400。
        Ok(Some(CursorKey::DeviceKey(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state.gateway.list(&actor, cursor, limit).await {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(
        &secret,
        EndpointId::GroupsRead,
        actor.user.id,
        &filter,
        page,
    ) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_groups(
    State(state): State<GroupsApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_groups_with_secret_provider(&state, &headers, &uri, || {
        management_cursor_secret().copied()
    })
    .await
}

/// `POST /api/v1/groups`：创建组。`name` 门禁复用仓储唯一真源
/// `validate_group_name`（非空、≤128 Unicode 字符、无控制字符），非法 400；
/// 重名由网关唯一键映射为 `InvalidState` → 409 `INVALID_STATE`。
async fn create_group(
    State(state): State<GroupsApiState>,
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
    let input = match read_json_body::<CreateGroupInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    if let Err(error) = validate_group_name(&input.name) {
        return map_error(error);
    }
    let created = match state.gateway.create(&actor, &input.name).await {
        Ok(created) => created,
        Err(error) => return map_error(error),
    };
    ok_response(created)
}

/// `PATCH /api/v1/groups/{id}`：CAS 补丁（归档或改名）。改名复用 `validate_group_name`
/// 前置 400；版本不符 → 409 `REVISION_CONFLICT`；不存在 → 404。
/// 写成功即引用该组的授权可见范围变化（仓储同事务 `bump_authz_epoch`），但本层**不发布**
/// `permissions.changed`：该事件的受众是**单个**受影响用户，这里受影响的是本层拿不到的用
/// 户集合，发给操作者管理员即是指向错人的虚假提示。撤权安全性由全局 `authz_epoch` 递增 +
/// SSE 读线每帧的权威复核关流保证（06 §4）。
async fn patch_group(
    State(state): State<GroupsApiState>,
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
    let input = match read_json_body::<PatchGroupInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    if let Some(name) = input.name.as_deref() {
        if let Err(error) = validate_group_name(name) {
            return map_error(error);
        }
    }
    let patched = match state
        .gateway
        .patch(&actor, id, revision, input.name.as_deref(), input.archived)
        .await
    {
        Ok(patched) => patched,
        Err(error) => return map_error(error),
    };
    ok_response(patched)
}

/// `PUT /api/v1/groups/{id}/members`：全量替换组成员。先查 `device_ids.len() <= 1024`
/// （超限 400 `INVALID_ARGUMENT`/`identity.group.members_exceeded`），再逐项
/// `parse_device_id_path` 校验小写 hex64（`device.invalid_id` 400），按首次出现顺序
/// 去重后传给网关；版本不符 → 409；不存在 → 404。
async fn replace_group_members(
    State(state): State<GroupsApiState>,
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
    let input = match read_json_body::<ReplaceMembersInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    if input.device_ids.len() > MAX_GROUP_MEMBERS {
        return err_response(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "identity.group.members_exceeded",
            serde_json::json!({}),
        );
    }
    let mut devices: Vec<[u8; 32]> = Vec::with_capacity(input.device_ids.len());
    for raw in &input.device_ids {
        let parsed = match parse_device_id_path(raw) {
            Ok(parsed) => parsed,
            Err(response) => return response,
        };
        if !devices.contains(&parsed) {
            devices.push(parsed);
        }
    }
    let replaced = match state
        .gateway
        .replace_members(&actor, id, revision, &devices)
        .await
    {
        Ok(replaced) => replaced,
        Err(error) => return map_error(error),
    };
    ok_response(replaced)
}

/// 挂载 groups 的 4 条管理路由：集合级 `GET`/`POST` 同 path 共存且各只注册一次，
/// 单资源 `PATCH` 与 `PUT /{id}/members` 各一处注册，无第二处重复挂载。
pub fn groups_router(state: GroupsApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/groups", get(list_groups).post(create_group))
        .route("/api/v1/groups/{id}", patch(patch_group))
        .route("/api/v1/groups/{id}/members", put(replace_group_members))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::test_fixture::*;
    use crate::error::ControllerError;
    use crate::events::EventEnvelope;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use std::sync::{Mutex, OnceLock};
    use tower::ServiceExt;

    /// canonical 小写 UUID 路径样本。
    const TARGET_ID: &str = "067e6162-ea94-4431-836c-7d8fa2010b3c";
    /// 64 位小写 hex 设备 ID 样本。
    const TARGET_DEVICE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER_DEVICE: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";

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

    /// 纯内存 `GroupsGateway`：`Echo` 回显 handler 传入的已解析参数，其余变体把指定
    /// 方法映射到指定 `ControllerError`（重名 409、版本冲突 409、缺失 404）。
    #[derive(Clone, Copy)]
    enum FixtureGroupsGateway {
        Echo,
        DuplicateName,
        RevisionConflict,
        Missing,
    }

    impl FixtureGroupsGateway {
        fn reject(self) -> Option<ControllerError> {
            match self {
                Self::Echo => None,
                Self::DuplicateName => Some(ControllerError::InvalidState),
                Self::RevisionConflict => Some(ControllerError::RevisionConflict),
                Self::Missing => Some(ControllerError::NotFound),
            }
        }
    }

    impl GroupsGateway for FixtureGroupsGateway {
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
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if let Some(error) = gateway.reject() {
                    return Err(error);
                }
                Ok(serde_json::json!({ "name": name }))
            })
        }
        fn patch<'a>(
            &'a self,
            _actor: &'a Session,
            id: Uuid,
            revision: u64,
            name: Option<&'a str>,
            archived: Option<bool>,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if let Some(error) = gateway.reject() {
                    return Err(error);
                }
                Ok(serde_json::json!({
                    "id": id.to_string(),
                    "revision": revision,
                    "name": name,
                    "archived": archived,
                }))
            })
        }
        fn replace_members<'a>(
            &'a self,
            _actor: &'a Session,
            id: Uuid,
            revision: u64,
            devices: &'a [[u8; 32]],
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                if let Some(error) = gateway.reject() {
                    return Err(error);
                }
                Ok(serde_json::json!({
                    "group": {
                        "id": id.to_string(),
                        "revision": revision,
                        "device_ids": devices
                            .iter()
                            .map(hex::encode)
                            .collect::<Vec<_>>(),
                    },
                    "member_count": devices.len(),
                }))
            })
        }
    }

    fn groups_state_with_session(
        session: Session,
        gateway: FixtureGroupsGateway,
    ) -> GroupsApiState {
        groups_state_with_events(session, gateway, None)
    }

    /// 事件总线可选的装配：`Some(bus)` 与 SSE 读线 `EventsState.bus` 是同一通道句柄，
    /// `None`（既有全部用例的基线）表示本装配未开启事件线。
    fn groups_state_with_events(
        session: Session,
        gateway: FixtureGroupsGateway,
        events: Option<EventBus>,
    ) -> GroupsApiState {
        GroupsApiState {
            auth: fixture_auth(session),
            gateway: Arc::new(gateway),
            events,
        }
    }

    fn groups_state_with(gateway: FixtureGroupsGateway) -> GroupsApiState {
        groups_state_with_session(fixture_session(true), gateway)
    }

    /// 恒回显桩：400 一律由 handler 校验先于网关产生。
    fn fixture_groups_state() -> GroupsApiState {
        groups_state_with(FixtureGroupsGateway::Echo)
    }

    /// PUT 写请求 helper：形状同 grants 区写 helper，仅方法为 PUT，
    /// Host/Cookie/CSRF 头口径与共享层一致。
    fn auth_json_put(uri: &str, body: serde_json::Value) -> Request<Body> {
        authenticated_json_write("PUT", uri, body)
    }

    // RED 前置：骨架 `groups_router` 挂载 0 条路由，以下断言在实现前必须失败于
    // `404`（行为 RED），不得是编译错误。

    #[tokio::test]
    async fn member_replacement_exceeding_1024_is_rejected_with_400() {
        let router = groups_router(fixture_groups_state());
        let ids: Vec<String> = (0..1025).map(|_| "a".repeat(64)).collect();
        let req = auth_json_put(
            "/api/v1/groups/00000000-0000-0000-0000-000000000001/members",
            serde_json::json!({ "expected_revision": "1", "device_ids": ids }),
        );
        let res = router.oneshot(req).await.unwrap();
        let (status, code, key) = error_code(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "identity.group.members_exceeded");
    }

    #[tokio::test]
    async fn member_replacement_echoes_deduplicated_devices_and_member_count() {
        let router = groups_router(fixture_groups_state());
        let req = auth_json_put(
            &format!("/api/v1/groups/{TARGET_ID}/members"),
            serde_json::json!({
                "expected_revision": "5",
                "device_ids": [TARGET_DEVICE, TARGET_DEVICE, OTHER_DEVICE],
            }),
        );
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["member_count"], 2, "重复项必须去重后传给网关");
        assert_eq!(value["data"]["group"]["id"], TARGET_ID);
        assert_eq!(value["data"]["group"]["revision"], 5);
        assert_eq!(
            value["data"]["group"]["device_ids"],
            serde_json::json!([TARGET_DEVICE, OTHER_DEVICE])
        );
        assert!(value["request_id"].is_string());
    }

    #[tokio::test]
    async fn member_replacement_rejects_malformed_device_id_with_400() {
        let bad_cases = [
            "ABCD".to_string(),
            "a".repeat(63),
            "a".repeat(64).to_uppercase(),
        ];
        for bad in bad_cases {
            let router = groups_router(fixture_groups_state());
            let res = router
                .oneshot(auth_json_put(
                    &format!("/api/v1/groups/{TARGET_ID}/members"),
                    serde_json::json!({
                        "expected_revision": "1",
                        "device_ids": [bad],
                    }),
                ))
                .await
                .unwrap();
            let (status, code, key) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {bad}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {bad}");
            assert_eq!(key, "device.invalid_id", "case {bad}");
        }
    }

    #[tokio::test]
    async fn member_replacement_rejects_non_canonical_id_and_bad_revision_with_400() {
        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(auth_json_put(
                "/api/v1/groups/067E6162-EA94-4431-836C-7D8FA2010B3C/members",
                serde_json::json!({ "expected_revision": "1", "device_ids": [TARGET_DEVICE] }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.invalid_id");

        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(auth_json_put(
                &format!("/api/v1/groups/{TARGET_ID}/members"),
                serde_json::json!({ "expected_revision": "+1", "device_ids": [TARGET_DEVICE] }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.revision_invalid");
    }

    #[tokio::test]
    async fn create_group_rejects_blank_or_overlong_name_with_400() {
        let cases = [
            serde_json::json!({ "name": "" }),
            serde_json::json!({ "name": "   " }),
            serde_json::json!({ "name": "x".repeat(129) }),
            serde_json::json!({ "name": "ops\u{1}" }),
            serde_json::json!({ "name": "ops", "unknown": 1 }),
            serde_json::json!({}),
        ];
        for body in cases {
            let router = groups_router(fixture_groups_state());
            let res = router
                .oneshot(authenticated_json_post("/api/v1/groups", body.clone()))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {body}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {body}");
        }
    }

    #[tokio::test]
    async fn create_group_echoes_name_and_duplicate_returns_409_invalid_state() {
        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/groups",
                serde_json::json!({ "name": "ops" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["name"], "ops");

        let router = groups_router(groups_state_with(FixtureGroupsGateway::DuplicateName));
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/groups",
                serde_json::json!({ "name": "ops" }),
            ))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "INVALID_STATE");
    }

    #[tokio::test]
    async fn patch_group_echoes_cas_fields_and_rejects_invalid_name_with_400() {
        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/groups/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "7", "name": "ops2", "archived": true }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["id"], TARGET_ID);
        assert_eq!(value["data"]["revision"], 7);
        assert_eq!(value["data"]["name"], "ops2");
        assert_eq!(value["data"]["archived"], true);

        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/groups/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "7", "name": "  " }),
            ))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
    }

    #[tokio::test]
    async fn patch_group_revision_conflict_and_missing_map_to_409_and_404() {
        let router = groups_router(groups_state_with(FixtureGroupsGateway::RevisionConflict));
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/groups/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "9", "archived": true }),
            ))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "REVISION_CONFLICT");

        let router = groups_router(groups_state_with(FixtureGroupsGateway::Missing));
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/groups/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "1", "archived": true }),
            ))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(code, "NOT_FOUND");
    }

    #[tokio::test]
    async fn list_rejects_unknown_query_and_out_of_range_limit_with_400() {
        for query in ["limit=0", "limit=201", "limit=abc", "foo=1", "group_id=aa"] {
            let router = groups_router(fixture_groups_state());
            let res = router
                .oneshot(authenticated_get(&format!("/api/v1/groups?{query}")))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
        }
    }

    #[tokio::test]
    async fn unauthenticated_groups_requests_are_rejected_with_401() {
        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/groups")
                    .header(header::HOST, FIXTURE_HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/v1/groups/{TARGET_ID}/members"))
                    .header(header::HOST, FIXTURE_HOST)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "expected_revision": "1", "device_ids": [] })
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
        let router = groups_router(groups_state_with_session(
            fixture_session(false),
            FixtureGroupsGateway::Echo,
        ));
        let res = router
            .oneshot(authenticated_get("/api/v1/groups"))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn groups_write_without_csrf_token_is_rejected_with_403() {
        let router = groups_router(fixture_groups_state());
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/groups")
            .header(header::HOST, FIXTURE_HOST)
            .header(header::COOKIE, format!("rsc_session={}", "a".repeat(64)))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::json!({ "name": "ops" }).to_string()))
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "CSRF_INVALID");
    }

    #[tokio::test]
    async fn unmounted_methods_on_groups_paths_are_not_allowed() {
        let router = groups_router(fixture_groups_state());
        let res = router
            .oneshot(authenticated_json_write(
                "DELETE",
                &format!("/api/v1/groups/{TARGET_ID}"),
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
    // 生产 `list_groups` 恒只经 `management_cursor_secret()`；测试进程内全局
    // `MANAGEMENT_CURSOR_SECRET` 未安装，故 GET cursor 正负例不挂生产 router，改挂
    // 只包 `list_groups_with_secret_provider` 的 test-only wrapper：真实 gate/query/
    // codec/gateway 顺序不绕过，密钥经 `cursor_secret_of` 从本测试局部槽读取。
    // 红线：不 set 全局 OnceLock、不给 `GroupsApiState` 扩 secret 字段。

    /// 与 users/roles/grants 区同值的测试合成 key，不引用任何 secret 文件。
    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];
    /// 网关内部 raw 32 hex（canonical v4 形态），出口必须被替换为 64 字符签名 token。
    const NEXT_RAW_HEX: &str = "067e6162ea944431836c7d8fa2010b3c";

    /// 记录每次 `list` 收到的 typed cursor 的页面桩：`cursor = None` 返回带 raw
    /// `next_cursor` 的第一页，否则返回末页。
    #[derive(Default)]
    struct PageGroupsGateway {
        calls: Mutex<Vec<Option<Uuid>>>,
    }

    impl GroupsGateway for PageGroupsGateway {
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
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn patch<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _revision: u64,
            _name: Option<&'a str>,
            _archived: Option<bool>,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn replace_members<'a>(
            &'a self,
            _actor: &'a Session,
            _id: Uuid,
            _revision: u64,
            _devices: &'a [[u8; 32]],
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    #[derive(Clone)]
    struct SignedGroupsCursorTestState {
        groups: GroupsApiState,
        secret_slot: Arc<OnceLock<[u8; 32]>>,
    }

    async fn list_groups_with_test_secret(
        State(state): State<SignedGroupsCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = state.secret_slot.clone();
        list_groups_with_secret_provider(&state.groups, &headers, &uri, move || {
            crate::api::cursor_secret_of(&slot).copied()
        })
        .await
    }

    /// `Some` 时把 synthetic key 装入本 router 私有槽，`None` 留空作网关前 503 负例。
    fn signed_groups_cursor_router(
        secret: Option<[u8; 32]>,
        gateway: Arc<PageGroupsGateway>,
    ) -> axum::Router<()> {
        let secret_slot = Arc::new(OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret)
                .expect("install synthetic secret");
        }
        let state = SignedGroupsCursorTestState {
            groups: GroupsApiState {
                auth: fixture_auth(fixture_session(true)),
                gateway,
                events: None,
            },
            secret_slot,
        };
        axum::Router::new()
            .route("/api/v1/groups", get(list_groups_with_test_secret))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_without_installed_secret_returns_503_before_gateway() {
        let gateway = Arc::new(PageGroupsGateway::default());
        let router = signed_groups_cursor_router(None, gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/groups"))
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
    async fn list_signs_first_page_and_decodes_second_page_cursor() {
        let gateway = Arc::new(PageGroupsGateway::default());
        let router = signed_groups_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/groups"))
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
        let router = signed_groups_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/groups?cursor={token}&limit=7"
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
        let gateway = Arc::new(PageGroupsGateway::default());
        let router = signed_groups_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/groups"))
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
        let router = signed_groups_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/groups?cursor={tampered}"
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

    // -----------------------------------------------------------------------
    // 组写操作的 `permissions.changed` **零发布**（06 §4，受众不可确定即不发布）
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

    /// 带事件总线的 groups 装配：`epoch` 是本装配报告的全局 authz_epoch。
    fn groups_router_with_bus(
        gateway: FixtureGroupsGateway,
        bus: Option<EventBus>,
        epoch: u64,
    ) -> axum::Router<()> {
        groups_router(GroupsApiState {
            auth: fixture_auth_with_epoch(fixture_session(true), epoch),
            gateway: Arc::new(gateway),
            events: bus,
        })
    }

    /// 语义收敛（审查反馈 Medium-1）：`PATCH /{id}`（改名/归档）改动的是**引用该组的
    /// 授权可见范围内全部用户**的有效权限，本层拿不到该集合，因此**不得**向操作者管理员
    /// 发一条关于其本人的 `permissions.changed`。撤权安全性由全局 `authz_epoch` 递增 +
    /// SSE 读线每帧按连接 `authz_epoch` 权威复核直接关流保证（06 §4）。
    #[tokio::test]
    async fn patch_group_publishes_no_permissions_changed_without_target_user() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = groups_router_with_bus(FixtureGroupsGateway::Echo, Some(bus.clone()), 42);
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/groups/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "2", "archived": true }),
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
                42,
            )),
            1
        );
    }

    /// 负例：新建空组与 `PUT /{id}/members`（仓储在该路径不递增 authz_epoch）即便有总线
    /// 也不发布；`events: None` 与写失败同样零发布。
    #[tokio::test]
    async fn group_creation_member_replacement_unwired_or_failed_bus_publish_nothing() {
        // 1) 新建空组。
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = groups_router_with_bus(FixtureGroupsGateway::Echo, Some(bus.clone()), 42);
        let res = router
            .oneshot(authenticated_json_post(
                "/api/v1/groups",
                serde_json::json!({ "name": "ops" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(collect_events(&mut rx).len(), 0, "新建空组不得发布");

        // 2) 全量替换成员。
        let router = groups_router_with_bus(FixtureGroupsGateway::Echo, Some(bus.clone()), 42);
        let res = router
            .oneshot(authenticated_json_write(
                "PUT",
                &format!("/api/v1/groups/{TARGET_ID}/members"),
                serde_json::json!({
                    "expected_revision": "2",
                    "device_ids": [TARGET_DEVICE],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            collect_events(&mut rx).len(),
            0,
            "成员替换无可信新 epoch，不得发布"
        );

        // 3) patch 成功但 `events: None`。
        let router = groups_router_with_bus(FixtureGroupsGateway::Echo, None, 42);
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/groups/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "2", "name": "renamed" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(collect_events(&mut rx).len(), 0, "events: None 不得有事件");

        // 4) 有总线但写失败（404）。
        let router = groups_router_with_bus(FixtureGroupsGateway::Missing, Some(bus), 42);
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/groups/{TARGET_ID}"),
                serde_json::json!({ "expected_revision": "2", "archived": true }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        assert_eq!(collect_events(&mut rx).len(), 0, "404 未发生变更，不得发布");
    }
}

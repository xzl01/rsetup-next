//! devices 资源管理路由：网关/实时探针契约与 3 条管理端点（Identity Task 10）。
//!
//! 契约（spec 02 §2/§3）：`GET /api/v1/devices` 面向所有 ≥1 有效设备权限的已认证用户
//! （**不套用 `require_admin_session`**，权限例外在 import 层面固化），响应为最低识别投影
//! （`device_id`/`display_name`/`effective_permissions`），无权限设备完全排除、绝不泄露总数；
//! `GET /api/v1/devices/{id}` 必须持有 `device.read`（reboot-only 严格 403），不可见与不存在
//! 同形 404，单快照后经同一次 `live_probe.snapshot` 取得连接/双流健康/能力，probe 失败 503；
//! `PATCH /api/v1/devices/{id}` admin 专用，CAS `expected_revision` 校验。
//! `GET` 列表的 `cursor`/`next_cursor` 一律经 `api/mod.rs` 统一签名 codec（DeviceKey 96 hex
//! profile）签发与校验，绑定 `group_id`/`search` 规范筛选。

use std::collections::HashSet;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::authz::policy::Permission;
use crate::authz::sqlx_repo::validate_display_name;
use crate::devices::repository::DeviceAdminView;
use crate::error::ControllerError;
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{
    authenticate_read, authenticate_write, password_change_required, read_json_body,
    require_admin_session,
};
use crate::model::{AdmissionState, ReviewDecision, decimal_u64_json};

use super::{
    CursorFilterCtx, CursorKey, ManagementFuture, decode_management_list_cursor,
    management_cursor_secret, map_error, parse_device_id_path, parse_revision, parse_uuid_path,
    sign_management_list_page,
};

/// 仅 HTTP 视图，不创建第二套 wire DTO。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceConnectionState {
    Online,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStreamHealth {
    Starting,
    Healthy,
    Degraded,
    Offline,
}

#[derive(Clone, Debug)]
pub struct DeviceLiveView {
    pub connection_state: DeviceConnectionState,
    pub control_health: DeviceStreamHealth,
    pub data_health: DeviceStreamHealth,
    /// Transport 真实 typed 缓存适配器按唯一共享 DTO 做 02 语义投影；未知样本为 `None`。
    /// `Value` 不是原始 wire 入口，也不是未经验证的任意设备 JSON。
    pub capabilities: Option<serde_json::Value>,
}

/// 实时视图探针：同一次调用取得连接状态与双流健康，读取失败必须失败上抛（503），
/// 不得降级为 offline/空能力 200。
pub trait DeviceLiveProbe: Send + Sync {
    fn snapshot<'a>(&'a self, id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView>;
}

#[derive(Clone)]
pub struct DevicesApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn DevicesGateway>,
    pub live_probe: Arc<dyn DeviceLiveProbe>,
}

/// `DeviceAdminView`/`HashSet` 来自 repository/std，列表筛选不得静默忽略。
pub trait DevicesGateway: Send + Sync {
    fn list_devices_visible<'a>(
        &'a self,
        actor: &'a Session,
        cursor: Option<[u8; 32]>,
        limit: usize,
        group_id: Option<Uuid>,
        search: Option<&'a str>,
    ) -> ManagementFuture<'a, serde_json::Value>;
    fn find_device<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
    ) -> ManagementFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>>;
    fn patch<'a>(
        &'a self,
        actor: &'a Session,
        id: [u8; 32],
        revision: u64,
        display_name: &'a str,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

// ---------------------------------------------------------------------------
// 严格 query / body DTO
// ---------------------------------------------------------------------------

/// `GET /api/v1/devices` 的严格 query DTO：`deny_unknown_fields` 拒绝任何未知参数；
/// `Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权前短路 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DevicesListQuery {
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
    pub(crate) group_id: Option<String>,
    pub(crate) search: Option<String>,
}

/// `PATCH /api/v1/devices/{id}` 输入：`expected_revision` 必填十进制字符串（spec 02 §1），
/// `display_name` 必填（仅中控档案改名）；未知字段 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchDeviceInput {
    pub(crate) expected_revision: String,
    pub(crate) display_name: String,
}

// ---------------------------------------------------------------------------
// GET /api/v1/devices：最低识别投影 + 签名 cursor
// ---------------------------------------------------------------------------

/// devices 列表 filter schema 的固定 tag 表：tag 1 = `group_id` UUID raw 16 字节、
/// tag 2 = `search` utf-8 字节切片；schema 恒含两字段、缺省 → `None`，`limit` 不进 schema。
const DEVICES_FILTER_TAG_GROUP_ID: u8 = 1;
const DEVICES_FILTER_TAG_SEARCH: u8 = 2;

/// `GET /api/v1/devices` 的 inner：`secret_provider` 为唯一密钥入口（生产 wrapper 恒只经
/// `management_cursor_secret()`，测试 wrapper 从局部 `OnceLock` 槽读取）。调用顺序固定：
/// `authenticate_read(DevicesRead)` → 临时密码前置 403 → query/limit/group_id/search 校验 →
/// filter ctx 构造 → 取密钥（`cursor = None` 也不得短路跳过）→ cursor 解码 → 网关单快照 →
/// 出口签名。**严禁 `require_admin_session`**——spec01 允许 ≥1 权限的已认证普通用户读取
/// 最低识别投影；可见范围与投影字段集由网关 `list_devices_visible` 单快照保证。
async fn list_devices_with_secret_provider<F>(
    state: &DevicesApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: FnOnce() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::DevicesRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    // spec01 权限例外：此处不得调用 require_admin_session——≥1 权限普通用户可读最低投影。
    // 但临时密码会话（01 身份规格 L18）仅允许 auth/me、password、logout，其余端点一律 403。
    if actor.user.must_change_password {
        return password_change_required();
    }
    let query = match Query::<DevicesListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let group_id = match query.group_id.as_deref() {
        None => None,
        Some(raw) => match parse_uuid_path(raw) {
            Ok(id) => Some(id),
            Err(response) => return response,
        },
    };
    let search = match query.search.as_deref() {
        None => None,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.chars().count() > 128
                || trimmed.len() > 512
                || trimmed.chars().any(|c| c.is_control())
            {
                return map_error(ControllerError::InvalidArgument);
            } else {
                Some(trimmed)
            }
        }
    };
    let filter = match CursorFilterCtx::from_ordered_fields(&[
        (
            DEVICES_FILTER_TAG_GROUP_ID,
            group_id.as_ref().map(|id| id.as_bytes().as_slice()),
        ),
        (
            DEVICES_FILTER_TAG_SEARCH,
            search.map(|value| value.as_bytes()),
        ),
    ]) {
        Ok(filter) => filter,
        Err(error) => return map_error(error),
    };
    let secret = match secret_provider() {
        Ok(secret) => secret,
        Err(error) => return map_error(error),
    };
    let cursor = match decode_management_list_cursor(
        &secret,
        EndpointId::DevicesRead,
        actor.user.id,
        &filter,
        query.cursor.as_deref(),
    ) {
        Ok(None) => None,
        Ok(Some(CursorKey::DeviceKey(raw))) => Some(raw),
        // `Uuid` 分支在 `DevicesRead` 的 `Device` profile 下不可达，仍 fail-closed 400。
        Ok(Some(CursorKey::Uuid(_))) => return map_error(ControllerError::InvalidArgument),
        Err(error) => return map_error(error),
    };
    let page = match state
        .gateway
        .list_devices_visible(&actor, cursor, limit, group_id, search)
        .await
    {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    match sign_management_list_page(
        &secret,
        EndpointId::DevicesRead,
        actor.user.id,
        &filter,
        page,
    ) {
        Ok(body) => ok_response(body),
        Err(error) => map_error(error),
    }
}

pub(crate) async fn list_devices(
    State(state): State<DevicesApiState>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    list_devices_with_secret_provider(&state, &headers, &uri, || {
        management_cursor_secret().copied()
    })
    .await
}

// ---------------------------------------------------------------------------
// GET /api/v1/devices/{id}：device.read 详情 + 单次实时探针
// ---------------------------------------------------------------------------

/// 传输准入状态按 spec 02 §2 固定为大写 `PENDING`/`APPROVED`/`REVOKED`；
/// 小写形式仅属于应用审核决定（`review_decision_str`），两者不得混用。
fn admission_state_str(state: AdmissionState) -> &'static str {
    match state {
        AdmissionState::Pending => "PENDING",
        AdmissionState::Approved => "APPROVED",
        AdmissionState::Revoked => "REVOKED",
    }
}

fn review_decision_str(decision: ReviewDecision) -> &'static str {
    match decision {
        ReviewDecision::None => "none",
        ReviewDecision::Approved => "approved",
        ReviewDecision::Denied => "denied",
        ReviewDecision::Revoked => "revoked",
    }
}

/// 详情 200 投影：单快照 `DeviceAdminView` + 有效权限 + 同次 probe 实时视图，仅 HTTP 视图，
/// 不创建第二套 wire DTO。`effective_permissions` 排序保证输出确定性。
fn device_detail_json(
    view: &DeviceAdminView,
    permissions: &HashSet<Permission>,
    live: &DeviceLiveView,
) -> serde_json::Value {
    let mut effective: Vec<&'static str> = permissions.iter().map(Permission::as_str).collect();
    effective.sort_unstable();
    serde_json::json!({
        "device_id": hex::encode(view.public_key),
        "display_name": view.display_name,
        "admission_state": admission_state_str(view.admission_state),
        "review_decision": review_decision_str(view.review_decision),
        "revision": decimal_u64_json(view.revision),
        "descriptor": view.descriptor,
        "connection_state": live.connection_state,
        "control_health": live.control_health,
        "data_health": live.data_health,
        "capabilities": live.capabilities,
        "effective_permissions": effective,
    })
}

/// `GET /api/v1/devices/{id}`：`authenticate_read(DeviceDetailRead)` → 独立临时密码前置
/// （与 list 同口径，绝不套用 admin 门禁）→ 路径 ID 严格解析 → 网关单快照 `find_device`：
/// `Ok(None)` 统一 404（不可见与不存在同形，不泄露设备存在性）；`Err(PermissionDenied)`
/// 统一 403（存在但缺 `device.read`）；`Ok(Some)` 后经**恰好一次** `live_probe.snapshot`
/// 同次取得连接/双流健康/能力，probe 错误 503，不降级 offline/空能力 200。
/// 严禁在此端点额外调用 `effective_permissions`/`require_permission` 造成双快照读。
async fn get_device(
    State(state): State<DevicesApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match authenticate_read(&state.auth, &headers, EndpointId::DeviceDetailRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if actor.user.must_change_password {
        return password_change_required();
    }
    let id = match parse_device_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let found = match state.gateway.find_device(&actor, id).await {
        Ok(found) => found,
        Err(error) => return map_error(error),
    };
    let (view, permissions) = match found {
        Some(found) => found,
        None => return map_error(ControllerError::NotFound),
    };
    let live = match state.live_probe.snapshot(&id).await {
        Ok(live) => live,
        Err(error) => return map_error(error),
    };
    ok_response(device_detail_json(&view, &permissions, &live))
}

// ---------------------------------------------------------------------------
// PATCH /api/v1/devices/{id}：admin 专用 CAS 改名
// ---------------------------------------------------------------------------

/// `PATCH /api/v1/devices/{id}`：`authenticate_write` → `require_admin_session` →
/// 路径 ID 严格解析 → CAS `expected_revision` 严格十进制 → `validate_display_name`
/// 门禁（仓储唯一真源）→ 网关。版本不符 409 `REVISION_CONFLICT`；不存在 404。
async fn patch_device(
    State(state): State<DevicesApiState>,
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
    let id = match parse_device_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let input = match read_json_body::<PatchDeviceInput>(&headers, body).await {
        Ok(input) => input,
        Err(response) => return response,
    };
    let revision = match parse_revision(&input.expected_revision) {
        Ok(revision) => revision,
        Err(response) => return response,
    };
    if let Err(error) = validate_display_name(&input.display_name) {
        return map_error(error);
    }
    let patched = match state
        .gateway
        .patch(&actor, id, revision, &input.display_name)
        .await
    {
        Ok(patched) => patched,
        Err(error) => return map_error(error),
    };
    ok_response(patched)
}

/// 挂载 devices 的 3 条管理路由：集合级 `GET` 唯一注册锚点为 `get(list_devices)`；
/// 单资源 `GET`/`PATCH` 同 path 共存且各只注册一次，无第二处重复挂载。
pub fn devices_router(state: DevicesApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/devices", get(list_devices))
        .route("/api/v1/devices/{id}", get(get_device).patch(patch_device))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::test_fixture::*;
    use crate::error::ControllerError;
    use crate::model::{AdmissionState, ReviewDecision};
    use axum::http::StatusCode;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;

    /// 仓储中不存在的设备 ID（与不可见同形 404 的对照样本）。
    const DEVICE_MISSING: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    /// 在仓储中但 actor 无任何权限的设备 ID。
    const DEVICE_INVISIBLE: &str =
        "0000000000000000000000000000000000000000000000000000000000000002";
    /// actor 仅有 `device.reboot` 等无 read 权限的设备 ID。
    const DEVICE_REBOOT_ONLY: &str =
        "0000000000000000000000000000000000000000000000000000000000000003";
    /// actor 持有 `device.read` 的设备 ID。
    const DEVICE_READABLE: &str =
        "0000000000000000000000000000000000000000000000000000000000000004";
    /// canonical 小写 UUID `group_id` 样本。
    const GROUP_ID: &str = "067e6162-ea94-4431-836c-7d8fa2010b3c";

    /// 纯内存 `DevicesGateway`：按用例参数化 `find_device`/`patch` 的三态结果；
    /// `list_devices_visible` 恒返回含 `device_id`/`display_name`/`effective_permissions`
    /// 的最低识别投影页。
    #[derive(Clone, Copy)]
    enum FixtureDevicesGateway {
        /// `find_device` 恒 `Ok(None)`：缺失与不可见同形。
        Invisible,
        /// `find_device` 恒 `Err(PermissionDenied)`：存在但缺 `device.read`。
        RebootOnly,
        /// `find_device` 返回 `Ok(Some((view, {device.read})))`。
        Readable,
        /// `patch` 回显 handler 传入的已解析参数。
        PatchEcho,
        /// `patch` 恒 `Err(RevisionConflict)`。
        PatchConflict,
    }

    fn fixture_view() -> DeviceAdminView {
        DeviceAdminView {
            public_key: [0x22u8; 32],
            display_name: "fixture-device".into(),
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::None,
            revision: 7,
            descriptor: Some(serde_json::json!({"serial": "fixture"})),
            archived: false,
        }
    }

    impl DevicesGateway for FixtureDevicesGateway {
        fn list_devices_visible<'a>(
            &'a self,
            _actor: &'a Session,
            _cursor: Option<[u8; 32]>,
            _limit: usize,
            _group_id: Option<Uuid>,
            _search: Option<&'a str>,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async {
                Ok(serde_json::json!({
                    "items": [{
                        "device_id": hex::encode([0x11u8; 32]),
                        "display_name": "fixture-device",
                        "effective_permissions": ["device.read"],
                    }],
                    "next_cursor": null,
                }))
            })
        }
        fn find_device<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
        ) -> ManagementFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>> {
            let gateway = *self;
            Box::pin(async move {
                match gateway {
                    Self::Invisible | Self::PatchEcho | Self::PatchConflict => Ok(None),
                    Self::RebootOnly => Err(ControllerError::PermissionDenied),
                    Self::Readable => Ok(Some((
                        fixture_view(),
                        HashSet::from([Permission::DeviceRead]),
                    ))),
                }
            })
        }
        fn patch<'a>(
            &'a self,
            _actor: &'a Session,
            id: [u8; 32],
            revision: u64,
            display_name: &'a str,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let gateway = *self;
            Box::pin(async move {
                match gateway {
                    Self::PatchEcho => Ok(serde_json::json!({
                        "device_id": hex::encode(id),
                        "revision": revision.to_string(),
                        "display_name": display_name,
                    })),
                    Self::PatchConflict => Err(ControllerError::RevisionConflict),
                    _ => Err(ControllerError::InvalidArgument),
                }
            })
        }
    }

    fn live_view() -> DeviceLiveView {
        DeviceLiveView {
            connection_state: DeviceConnectionState::Online,
            control_health: DeviceStreamHealth::Healthy,
            data_health: DeviceStreamHealth::Degraded,
            capabilities: Some(serde_json::json!({"model": "fixture"})),
        }
    }

    /// `Ok` 返回固定实时视图；`Fail` 模拟 probe 读取失败（必须 503，不得降级 200）。
    #[derive(Clone, Copy)]
    enum FixtureLiveProbe {
        Ok,
        Fail,
    }

    impl DeviceLiveProbe for FixtureLiveProbe {
        fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
            let fail = matches!(self, Self::Fail);
            Box::pin(async move {
                if fail {
                    Err(ControllerError::Config("fixture probe down".into()))
                } else {
                    Ok(live_view())
                }
            })
        }
    }

    /// 记录 `snapshot` 调用次数的实时桩：详情必须恰好调用一次。
    struct CountingLiveProbe {
        calls: Arc<AtomicUsize>,
    }

    impl DeviceLiveProbe for CountingLiveProbe {
        fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(live_view()) })
        }
    }

    fn devices_router_with(
        gateway: FixtureDevicesGateway,
        probe: FixtureLiveProbe,
        session: Session,
    ) -> (axum::Router<()>, Arc<HttpAuthState>) {
        let auth = fixture_auth(session);
        (
            devices_router(DevicesApiState {
                auth: auth.clone(),
                gateway: Arc::new(gateway),
                live_probe: Arc::new(probe),
            }),
            auth,
        )
    }

    /// 详情用例前置：`find_device` 恒 `Ok(None)`（缺失/不可见）。
    fn fixture_devices_state() -> (axum::Router<()>, Arc<HttpAuthState>) {
        devices_router_with(
            FixtureDevicesGateway::Invisible,
            FixtureLiveProbe::Ok,
            fixture_session(true),
        )
    }

    /// reboot-only 前置：`find_device` 恒 `Err(PermissionDenied)`。
    fn fixture_reboot_only_state() -> (axum::Router<()>, Arc<HttpAuthState>) {
        devices_router_with(
            FixtureDevicesGateway::RebootOnly,
            FixtureLiveProbe::Ok,
            fixture_session(true),
        )
    }

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

    // RED 前置：骨架 `devices_router` 挂载 0 条路由，以下断言在实现前必须失败于
    // 未挂载路由的 404 行为（行为 RED），不得是编译错误。

    #[tokio::test]
    async fn invisible_and_missing_device_return_identical_404_envelope() {
        let (router, _state) = fixture_devices_state();
        let router_invisible = router.clone();
        // dev-missing 不在仓储中
        let req_missing = authenticated_get(&format!("/api/v1/devices/{DEVICE_MISSING}"));
        let res_missing = router.oneshot(req_missing).await.unwrap();
        // dev-invisible 在仓储中但 actor 无任何权限
        let req_invisible = authenticated_get(&format!("/api/v1/devices/{DEVICE_INVISIBLE}"));
        let res_invisible = router_invisible.oneshot(req_invisible).await.unwrap();
        assert_eq!(res_missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(res_invisible.status(), StatusCode::NOT_FOUND);
        let mut val_m: serde_json::Value = body_json(res_missing).await;
        let mut val_i: serde_json::Value = body_json(res_invisible).await;
        val_m.as_object_mut().unwrap().remove("request_id");
        val_i.as_object_mut().unwrap().remove("request_id");
        assert_eq!(
            val_m, val_i,
            "identical 404 envelope must not leak device existence"
        );
    }

    #[tokio::test]
    async fn reboot_only_actor_cannot_read_device_detail() {
        let (router, _state) = fixture_reboot_only_state();
        // actor 仅有 device.reboot 权限
        let req = authenticated_get(&format!("/api/v1/devices/{DEVICE_REBOOT_ONLY}"));
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["error"]["code"], "PERMISSION_DENIED");
    }

    // -----------------------------------------------------------------------
    // GET list 签名 cursor 真实接线（test-only wrapper + 局部 OnceLock 密钥槽）
    // -----------------------------------------------------------------------
    //
    // 生产 `list_devices` 恒只经 `management_cursor_secret()`；测试进程内全局
    // `MANAGEMENT_CURSOR_SECRET` 未安装，挂生产 router 的 GET list 恒 503（该 503 已在
    // GREEN 过程实证：取密钥先于网关、绝不降级未签名页面）。因此 list 正例不挂生产
    // router，改挂只包 `list_devices_with_secret_provider` 的 test-only wrapper：真实
    // gate/query/codec/gateway 顺序不绕过，密钥经 `cursor_secret_of` 从本测试局部槽读取。
    // 红线：不 set 全局 OnceLock、不给 `DevicesApiState` 扩 secret 字段、不用假 handler
    // 或自签 token 绕过真实链路。

    /// 与 users/roles/grants/groups 区同值的测试合成 key，不引用任何 secret 文件。
    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];
    /// 网关内部 raw 64 hex（Device profile 32B），出口必须被替换为 96 字符签名 token。
    const NEXT_RAW_HEX: &str = "abababababababababababababababababababababababababababababababab";

    /// `list_devices_visible` 单次调用的记录形状（规避 clippy::type_complexity）。
    type PageCall = (Option<[u8; 32]>, Option<Uuid>, Option<String>);

    /// 记录每次 `list_devices_visible` 收到的 typed cursor 与规范化筛选的页面桩：
    /// `cursor = None` 返回带 raw `next_cursor` 的第一页，否则返回末页。
    #[derive(Default)]
    struct PageDevicesGateway {
        calls: std::sync::Mutex<Vec<PageCall>>,
    }

    impl PageDevicesGateway {
        fn recorded(&self) -> Vec<PageCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl DevicesGateway for PageDevicesGateway {
        fn list_devices_visible<'a>(
            &'a self,
            _actor: &'a Session,
            cursor: Option<[u8; 32]>,
            _limit: usize,
            group_id: Option<Uuid>,
            search: Option<&'a str>,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .unwrap()
                    .push((cursor, group_id, search.map(str::to_string)));
                let next_cursor = if cursor.is_none() {
                    serde_json::json!(NEXT_RAW_HEX)
                } else {
                    serde_json::Value::Null
                };
                Ok(serde_json::json!({
                    "items": [{
                        "device_id": hex::encode([0x11u8; 32]),
                        "display_name": "fixture-device",
                        "effective_permissions": ["device.read"],
                    }],
                    "next_cursor": next_cursor,
                }))
            })
        }
        fn find_device<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
        ) -> ManagementFuture<'a, Option<(DeviceAdminView, HashSet<Permission>)>> {
            Box::pin(async { Ok(None) })
        }
        fn patch<'a>(
            &'a self,
            _actor: &'a Session,
            _id: [u8; 32],
            _revision: u64,
            _display_name: &'a str,
        ) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    #[derive(Clone)]
    struct SignedDevicesCursorTestState {
        devices: DevicesApiState,
        secret_slot: Arc<std::sync::OnceLock<[u8; 32]>>,
    }

    /// test-only wrapper handler：内部调用同一个 `list_devices_with_secret_provider`，
    /// 真实 gate/query/codec/gateway 顺序不绕过，provider 经 `cursor_secret_of` 从本
    /// 测试局部槽读取。
    async fn list_devices_with_test_secret(
        State(state): State<SignedDevicesCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = state.secret_slot.clone();
        list_devices_with_secret_provider(&state.devices, &headers, &uri, move || {
            crate::api::cursor_secret_of(&slot).copied()
        })
        .await
    }

    /// fixture helper：只挂 wrapper 的 GET list 路由并 `.with_state(state)` 归约；
    /// `Some` 经 `install_cursor_secret_into` 装入测试合成 key、`None` 留空作网关前
    /// 503 负例。
    fn signed_devices_cursor_router(
        secret: Option<[u8; 32]>,
        session: Session,
        gateway: Arc<PageDevicesGateway>,
    ) -> axum::Router<()> {
        let secret_slot = Arc::new(std::sync::OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret)
                .expect("synthetic key 安装必须成功");
        }
        let state = SignedDevicesCursorTestState {
            devices: DevicesApiState {
                auth: fixture_auth(session),
                gateway,
                live_probe: Arc::new(FixtureLiveProbe::Ok),
            },
            secret_slot,
        };
        axum::Router::new()
            .route("/api/v1/devices", get(list_devices_with_test_secret))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_devices_returns_minimum_projection_only() {
        let gateway = Arc::new(PageDevicesGateway::default());
        let router = signed_devices_cursor_router(
            Some(SYNTHETIC_SECRET),
            fixture_session(true),
            gateway.clone(),
        );
        let req = authenticated_get("/api/v1/devices");
        let res = router.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        let item = &val["data"]["items"][0];
        assert!(item.get("device_id").is_some());
        assert!(item.get("display_name").is_some());
        assert!(item.get("effective_permissions").is_some());
        assert!(
            item.get("descriptor").is_none(),
            "descriptor must be hidden in list"
        );
        assert!(
            item.get("admission_state").is_none(),
            "admission_state hidden in list"
        );
    }

    #[tokio::test]
    async fn list_without_installed_secret_returns_503_before_gateway() {
        let gateway = Arc::new(PageDevicesGateway::default());
        let router = signed_devices_cursor_router(None, fixture_session(true), gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/devices"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
        let value = body_json(res).await;
        assert_eq!(value["error"]["code"], "STORAGE_UNAVAILABLE");
        assert!(
            gateway.recorded().is_empty(),
            "取密钥必须在 gateway.list_devices_visible 之前（含 cursor=None 路径）"
        );
    }

    #[tokio::test]
    async fn list_signs_96_hex_token_and_decodes_second_page_device_key_cursor() {
        let gateway = Arc::new(PageDevicesGateway::default());
        let router = signed_devices_cursor_router(
            Some(SYNTHETIC_SECRET),
            fixture_session(true),
            gateway.clone(),
        );
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/devices?group_id={GROUP_ID}&search=ops"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        assert_eq!(token.len(), 96, "DeviceKey profile 签名 token 恒 96 hex");
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_ne!(token, NEXT_RAW_HEX, "裸 raw key 不得直传出口");
        // 第二页：同一 group_id/search 文本 + 换 limit=7——limit 不进 schema，仍须 200。
        let router = signed_devices_cursor_router(
            Some(SYNTHETIC_SECRET),
            fixture_session(true),
            gateway.clone(),
        );
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/devices?cursor={token}&group_id={GROUP_ID}&search=ops&limit=7"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert!(value["data"]["next_cursor"].is_null());
        let calls = gateway.recorded();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (None, group_uuid(), Some("ops".into())));
        assert_eq!(
            calls[1],
            (Some([0xabu8; 32]), group_uuid(), Some("ops".into())),
            "网关必须收到 token 解回的 DeviceKey raw32"
        );
    }

    fn group_uuid() -> Option<Uuid> {
        Some(Uuid::try_parse(GROUP_ID).expect("canonical uuid"))
    }

    #[tokio::test]
    async fn list_cursor_rejects_changed_or_missing_filter_before_gateway() {
        let gateway = Arc::new(PageDevicesGateway::default());
        let router = signed_devices_cursor_router(
            Some(SYNTHETIC_SECRET),
            fixture_session(true),
            gateway.clone(),
        );
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/devices?group_id={GROUP_ID}&search=ops"
            )))
            .await
            .unwrap();
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        // 下一页改写或缺省 group_id/search → filter 不一致 → MAC 校验失败 400，不触网关。
        for query in [
            format!("cursor={token}&search=ops"),
            format!("cursor={token}&group_id={GROUP_ID}"),
            format!("cursor={token}&group_id={GROUP_ID}&search=other"),
            format!("cursor={token}"),
        ] {
            let before = gateway.recorded().len();
            let router = signed_devices_cursor_router(
                Some(SYNTHETIC_SECRET),
                fixture_session(true),
                gateway.clone(),
            );
            let res = router
                .oneshot(authenticated_get(&format!("/api/v1/devices?{query}")))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
            assert_eq!(
                gateway.recorded().len(),
                before,
                "客户端 400 不得触达网关：case {query}"
            );
        }
    }

    #[tokio::test]
    async fn list_rejects_out_of_range_limit_malformed_filters_with_400() {
        for query in [
            "limit=0",
            "limit=201",
            "limit=abc",
            "foo=1",
            "group_id=not-a-uuid",
            "group_id=067E6162-EA94-4431-836C-7D8FA2010B3C",
            "search=%00hidden",
            &format!("search={}", "x".repeat(129)),
        ] {
            let gateway = Arc::new(PageDevicesGateway::default());
            let router = signed_devices_cursor_router(
                Some(SYNTHETIC_SECRET),
                fixture_session(true),
                gateway.clone(),
            );
            let res = router
                .oneshot(authenticated_get(&format!("/api/v1/devices?{query}")))
                .await
                .unwrap();
            let (status, code, _) = error_code(res).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {query}");
            assert_eq!(code, "INVALID_ARGUMENT", "case {query}");
            assert!(
                gateway.recorded().is_empty(),
                "400 不得触达网关：case {query}"
            );
        }
    }

    #[tokio::test]
    async fn list_trims_search_to_none_and_reboot_only_user_gets_projection() {
        // trim 后为空 → None：filter 两字段均缺省，仍 200。
        let gateway = Arc::new(PageDevicesGateway::default());
        let router = signed_devices_cursor_router(
            Some(SYNTHETIC_SECRET),
            fixture_session(true),
            gateway.clone(),
        );
        let res = router
            .oneshot(authenticated_get("/api/v1/devices?search=%20%20"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            gateway.recorded(),
            vec![(None, None, None)],
            "trim 后空白的 search 必须规范为 None"
        );
        // 权限例外阳性对照：非 admin（reboot-only 类普通用户）200 + 最低投影。
        let gateway = Arc::new(PageDevicesGateway::default());
        let router = signed_devices_cursor_router(
            Some(SYNTHETIC_SECRET),
            fixture_session(false),
            gateway.clone(),
        );
        let res = router
            .oneshot(authenticated_get("/api/v1/devices"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert!(
            value["data"]["items"][0]
                .get("effective_permissions")
                .is_some()
        );
    }

    #[tokio::test]
    async fn list_must_change_password_returns_distinct_403_code() {
        let gateway = Arc::new(PageDevicesGateway::default());
        let router = signed_devices_cursor_router(
            Some(SYNTHETIC_SECRET),
            fixture_session_must_change_password(),
            gateway.clone(),
        );
        let res = router
            .oneshot(authenticated_get("/api/v1/devices"))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PASSWORD_CHANGE_REQUIRED");
        assert!(gateway.recorded().is_empty());
    }

    #[tokio::test]
    async fn device_detail_merges_single_snapshot_and_single_probe_call() {
        let calls = Arc::new(AtomicUsize::new(0));
        let auth = fixture_auth(fixture_session(true));
        let router = devices_router(DevicesApiState {
            auth,
            gateway: Arc::new(FixtureDevicesGateway::Readable),
            live_probe: Arc::new(CountingLiveProbe {
                calls: calls.clone(),
            }),
        });
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/devices/{DEVICE_READABLE}"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "详情必须恰好调用一次 probe"
        );
        let value = body_json(res).await;
        assert_eq!(value["data"]["device_id"], hex::encode([0x22u8; 32]));
        assert_eq!(value["data"]["display_name"], "fixture-device");
        assert_eq!(value["data"]["admission_state"], "APPROVED");
        assert_eq!(value["data"]["review_decision"], "none");
        assert_eq!(value["data"]["revision"], "7");
        assert_eq!(
            value["data"]["descriptor"],
            serde_json::json!({"serial": "fixture"})
        );
        assert_eq!(value["data"]["connection_state"], "online");
        assert_eq!(value["data"]["control_health"], "healthy");
        assert_eq!(value["data"]["data_health"], "degraded");
        assert_eq!(
            value["data"]["capabilities"],
            serde_json::json!({"model": "fixture"})
        );
        assert_eq!(
            value["data"]["effective_permissions"],
            serde_json::json!(["device.read"])
        );
    }

    #[test]
    fn admission_state_str_matches_uppercase_wire_contract() {
        // spec 02 §2：传输准入为大写；小写仅属于 review_decision。
        assert_eq!(admission_state_str(AdmissionState::Pending), "PENDING");
        assert_eq!(admission_state_str(AdmissionState::Approved), "APPROVED");
        assert_eq!(admission_state_str(AdmissionState::Revoked), "REVOKED");
    }

    #[tokio::test]
    async fn device_detail_probe_failure_maps_to_503_not_offline_200() {
        let (router, _state) = devices_router_with(
            FixtureDevicesGateway::Readable,
            FixtureLiveProbe::Fail,
            fixture_session(true),
        );
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/devices/{DEVICE_READABLE}"
            )))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "STORAGE_UNAVAILABLE");
    }

    #[tokio::test]
    async fn must_change_password_detail_returns_distinct_403_code() {
        let (router, _state) = devices_router_with(
            FixtureDevicesGateway::Readable,
            FixtureLiveProbe::Ok,
            fixture_session_must_change_password(),
        );
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/devices/{DEVICE_READABLE}"
            )))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PASSWORD_CHANGE_REQUIRED");
    }

    #[tokio::test]
    async fn patch_device_echoes_cas_fields_for_admin() {
        let (router, _state) = devices_router_with(
            FixtureDevicesGateway::PatchEcho,
            FixtureLiveProbe::Ok,
            fixture_session(true),
        );
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/devices/{DEVICE_READABLE}"),
                serde_json::json!({ "expected_revision": "7", "display_name": "renamed" }),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["device_id"], DEVICE_READABLE);
        assert_eq!(value["data"]["revision"], "7");
        assert_eq!(value["data"]["display_name"], "renamed");
    }

    #[tokio::test]
    async fn patch_device_rejects_non_admin_and_invalid_input_with_403_400() {
        let (router, _state) = devices_router_with(
            FixtureDevicesGateway::PatchEcho,
            FixtureLiveProbe::Ok,
            fixture_session(false),
        );
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/devices/{DEVICE_READABLE}"),
                serde_json::json!({ "expected_revision": "7", "display_name": "renamed" }),
            ))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");

        let (router, _state) = fixture_devices_state();
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/devices/{DEVICE_READABLE}"),
                serde_json::json!({ "expected_revision": "+7", "display_name": "renamed" }),
            ))
            .await
            .unwrap();
        let (status, code, key) = error_code(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        assert_eq!(key, "request.revision_invalid");

        let (router, _state) = fixture_devices_state();
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/devices/{DEVICE_READABLE}"),
                serde_json::json!({ "expected_revision": "7", "display_name": "   " }),
            ))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
    }

    #[tokio::test]
    async fn patch_device_revision_conflict_maps_to_409() {
        let (router, _state) = devices_router_with(
            FixtureDevicesGateway::PatchConflict,
            FixtureLiveProbe::Ok,
            fixture_session(true),
        );
        let res = router
            .oneshot(authenticated_json_patch(
                &format!("/api/v1/devices/{DEVICE_READABLE}"),
                serde_json::json!({ "expected_revision": "3", "display_name": "renamed" }),
            ))
            .await
            .unwrap();
        let (status, code, _) = error_code(res).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(code, "REVISION_CONFLICT");
    }
}

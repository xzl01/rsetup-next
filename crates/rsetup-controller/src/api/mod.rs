//! 管理路由骨架（Identity Task 6）：共享解析辅助、统一错误映射器、统一签名
//! 分页 cursor，以及 8 个资源网关 trait 的类型前置。
//!
//! 边界（spec 02 §3、Identity 计划 Task 6）：
//! - 本模块只组合管理子 router，不组合 auth/tasks/status/assets 或探活，也不改写旧 `AppState`；
//!   顶层装配唯一归属集成 Task 0。
//! - 骨架阶段各资源子路由挂载 0 条路由：未实现的路由不注册，Task 7+ 逐资源接线。
//! - 所有管理分页端点的 `cursor`/`next_cursor` 一律经本模块的签名函数对签发与校验，
//!   废弃裸内部键直传；密钥只经第一调用参数传入，codec 内不读取任何环境变量。

pub mod adapters;
pub mod approvals;
pub mod audit;
pub mod devices;
pub mod grants;
pub mod groups;
pub mod roles;
pub mod system;
pub mod users;

use std::sync::{Arc, OnceLock};

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::json;
use sha2::Digest as _;
use uuid::Uuid;

use crate::error::ControllerError;
use crate::events::{EventBus, EventEnvelope};
use crate::http_api::{err_response, ok_response};
use crate::http_auth::{EndpointId, HttpAuthState};

use self::approvals::{ApprovalsApiState, ApprovalsGateway, ManagementNotificationSink};
use self::audit::{AuditApiState, AuditGateway};
use self::devices::{DeviceLiveProbe, DevicesApiState, DevicesGateway};
use self::grants::{GrantsApiState, GrantsGateway};
use self::groups::{GroupsApiState, GroupsGateway};
use self::roles::{RolesApiState, RolesGateway};
use self::system::{RecoveryModeProbe, SystemApiState, SystemGateway};
use self::users::{UsersApiState, UsersGateway};

/// 各资源网关共用的 dyn-safe 异步返回：与仓储 `RepoFuture` 同一 BoxFuture 形态，
/// 不用 async-fn trait 再转 `dyn`。
pub type ManagementFuture<'a, T> = crate::authz::sqlx_repo::RepoFuture<'a, T>;

/// 管理 router 的显式装配状态：全部网关/probe/sink 由集成 owner 显式提供真实实例，
/// 不提供默认常量源；不扩展 cursor 密钥字段。唯一允许的 `Option` 是
/// [`ManagementState::events`]（未装配事件线时不发布），它不是任何真实源的常量兜底。
#[derive(Clone)]
pub struct ManagementState {
    pub auth: Arc<HttpAuthState>,
    pub users: Arc<dyn UsersGateway>,
    pub roles: Arc<dyn RolesGateway>,
    pub grants: Arc<dyn GrantsGateway>,
    pub groups: Arc<dyn GroupsGateway>,
    pub devices: Arc<dyn DevicesGateway>,
    pub approvals: Arc<dyn ApprovalsGateway>,
    pub audit: Arc<dyn AuditGateway>,
    pub system: Arc<dyn SystemGateway>,
    pub device_probe: Arc<dyn DeviceLiveProbe>,
    pub recovery_probe: Arc<dyn RecoveryModeProbe>,
    pub notification_sink: Arc<dyn ManagementNotificationSink>,
    /// 进程内事件总线句柄（06 §4）：与 SSE 读线 `EventsState.bus` 必须是同一通道句柄
    /// （[`EventBus::clone`] 不新建通道）。`None` 表示本装配未开启事件线，各资源写端点
    /// 的行为与接入总线之前完全一致（不发布、不报错、不阻塞）；发布返回值一律忽略，
    /// 不得据此改变响应。由 [`build_management_router`] 分发给需要发布的那几个子
    /// router state（`AuditApiState` 无发布职责，故不持有）。
    pub events: Option<EventBus>,
    /// 时间门禁句柄：`GET /api/v1/system/time` 读取真实时间证据与重试状态的来源。
    pub time_source: Option<crate::time::TimeGateHandle>,
    /// 进程内唯一的恢复模式控制器句柄（受控恢复触发/解除管理路由使用）。
    pub recovery_controller: Option<Arc<crate::operations::RuntimeRecoveryController>>,
    /// 生产传输管理器（受控恢复进入时用于会话吊销与断开连接）。
    pub transport_manager: Option<Arc<crate::transport::TransportManager>>,
}

/// 生产装配签名（Task 13 打通真实仓储 adapter）。骨架阶段 8 个子 router 各挂载 0 条
/// 路由，因此本函数返回的 router 不含任何可命中路由——类型前置，不计为 API 验收。
///
/// 事件总线按 `Option` 逐个子 router 分发（唯一的 `Option` 例外，见
/// [`ManagementState::events`]）：`None` 即本装配未开启事件线，写端点行为与接入总线
/// 之前完全一致，不构造常量源也不伪造发布成功。
pub fn build_management_router(state: ManagementState) -> axum::Router<()> {
    users::users_router(UsersApiState {
        auth: state.auth.clone(),
        gateway: state.users,
        events: state.events.clone(),
    })
    .merge(roles::roles_router(RolesApiState {
        auth: state.auth.clone(),
        gateway: state.roles,
        events: state.events.clone(),
    }))
    .merge(grants::grants_router(GrantsApiState {
        auth: state.auth.clone(),
        gateway: state.grants,
        events: state.events.clone(),
    }))
    .merge(groups::groups_router(GroupsApiState {
        auth: state.auth.clone(),
        gateway: state.groups,
        events: state.events.clone(),
    }))
    .merge(devices::devices_router(DevicesApiState {
        auth: state.auth.clone(),
        gateway: state.devices,
        live_probe: state.device_probe.clone(),
        events: state.events.clone(),
    }))
    .merge(approvals::approvals_router(ApprovalsApiState {
        auth: state.auth.clone(),
        gateway: state.approvals,
        notification_sink: state.notification_sink,
        probe: state.device_probe,
        events: state.events.clone(),
    }))
    .merge(audit::audit_router(AuditApiState {
        auth: state.auth.clone(),
        gateway: state.audit,
    }))
    .merge(system::system_router(SystemApiState {
        auth: state.auth,
        gateway: state.system,
        recovery_probe: state.recovery_probe,
        events: state.events,
        time_source: state.time_source,
        recovery_controller: state.recovery_controller,
        transport_manager: state.transport_manager,
    }))
}

// ---------------------------------------------------------------------------
// 授权变更事件发布（06 §4 `permissions.changed`）
// ---------------------------------------------------------------------------

/// 授权写成功后的统一 `permissions.changed` 发布入口，**只**供目标用户明确的写线复用：
/// `users.rs` 的 `PATCH /{id}` 与 `reset-password`、`grants.rs` 的 `create_grant`
/// （请求体 `user_id` 即受影响用户）。
///
/// - `user_id` 是**受影响用户**的 16B UUID。受众与载荷 `id` 由
///   [`EventEnvelope::permissions_changed`] 用同一份字节生成，调用方在 API 上写不出
///   「把 Alice 的授权变化投给 Bob」的配对；也正因受众被这份字节绑死，**当受影响用户
///   集合在本层不可确定时（改角色权限集、改组、删授权不回显归属人）一律不调用本函数**
///   ——传操作者管理员本人并不是「合法下界」而是虚假提示：他未必在受影响集合内，真正
///   受影响的持有者反而收不到，客户端还会据 `epoch` 误判自己的授权投影已刷新；
///   这类变更的安全性由全局 `authz_epoch` 递增 + SSE 读线每帧按连接 `authz_epoch`
///   权威复核直接关流保证（见下条），本就不需要这条提示；
/// - epoch 取写入返回之后重读的全局 `schema_meta.authz_epoch`（与各认证端点同源，
///   不是自造计数器），因此恒 ≥ 本次变更 bump 后的值，客户端据此判定旧授权投影失效；
/// - epoch 读取失败即**不发布**：宁可不发提示，也不伪造 epoch。正确性本就不依赖这条
///   提示——SSE 投递侧对每一帧都按连接的 `authz_epoch` 复核（06 §4「发送前验证 session
///   与 authz_epoch，撤权关闭旧订阅」），事件只是让**受影响的那个**客户端更早主动重鉴权
///   的优化；受众不可确定时宁可不发；
/// - [`EventBus::publish`] 永不阻塞、永不上抛，返回值（在线订阅者数）在此丢弃。
pub(crate) async fn publish_permissions_changed(
    bus: &EventBus,
    auth: &HttpAuthState,
    user_id: [u8; 16],
) {
    let Ok(epoch) = auth.auth.authz_epoch().await else {
        return;
    };
    bus.publish(EventEnvelope::permissions_changed(user_id, epoch));
}

// ---------------------------------------------------------------------------
// 公共解析辅助与错误映射器
// ---------------------------------------------------------------------------
//
// 以下辅助与下方 cursor codec 的首个生产消费者是 Task 7+ 逐资源接线的 handler，
// 本任务内只被单元测试调用；沿用 `http_gate.rs` 既有约定，以待接线 `allow(dead_code)`
// 标注，handler 接线后逐项移除，不做静默删除也不改签名。

/// 默认分页大小（待审默认，spec 02 §3）。
const DEFAULT_PAGE_LIMIT: usize = 50;
/// 分页上限（spec 02 §3：管理列表 limit 1..=200）。
const MAX_PAGE_LIMIT: usize = 200;

/// `expected_revision` 解析：仅接受严格纯 ASCII 十进制（拒绝空串、`+` 前缀、
/// 符号、空白、非数字与 u64 溢出），失败 400 `INVALID_ARGUMENT`。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) fn parse_revision(raw: &str) -> Result<u64, Response> {
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_revision());
    }
    raw.parse::<u64>().map_err(|_| invalid_revision())
}

#[allow(dead_code)]
fn invalid_revision() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "request.revision_invalid",
        json!({}),
    )
}

/// 路径设备 ID 解析：恰 64 位小写 hex → 32B；其余 400 `INVALID_ARGUMENT`。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) fn parse_device_id_path(raw: &str) -> Result<[u8; 32], Response> {
    crate::parse_device_id(raw).map_err(|_| {
        err_response(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "device.invalid_id",
            json!({}),
        )
    })
}

/// 唯一 UUID 文本 parser：仅接受规范小写连字符 36 字符。
///
/// `Uuid::try_parse` 同时接受花括号/紧凑/urn 等宽松形态，故解析成功后
/// 必须与 `to_string()` 的规范小写字符串逐字符相等，一次性闭合接受域，
/// 拒大写、花括号、紧凑、空白、尾换行、非 ASCII 与全部畸形。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) fn parse_uuid_path(raw: &str) -> Result<Uuid, Response> {
    let parsed = Uuid::try_parse(raw).map_err(|_| invalid_uuid_id())?;
    if parsed.to_string() != raw {
        return Err(invalid_uuid_id());
    }
    Ok(parsed)
}

#[allow(dead_code)]
fn invalid_uuid_id() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "request.invalid_id",
        json!({}),
    )
}

/// 列表 `limit` 解析：缺省 50，合法域 `1..=200`；`0`、`>200`、非十进制一律
/// 400 `INVALID_ARGUMENT`。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) fn parse_limit(raw: Option<&str>) -> Result<usize, Response> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_PAGE_LIMIT);
    };
    if raw.is_empty() || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid_limit());
    }
    let parsed = raw.parse::<usize>().map_err(|_| invalid_limit())?;
    if !(1..=MAX_PAGE_LIMIT).contains(&parsed) {
        return Err(invalid_limit());
    }
    Ok(parsed)
}

#[allow(dead_code)]
fn invalid_limit() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "request.invalid_limit",
        json!({}),
    )
}

/// `ControllerError` → 02 错误码信封（spec 02 §3）。
///
/// 映射表：`InvalidArgument` 400；`PermissionDenied` 403；`NotFound` 404；
/// `RevisionConflict`/`LastAdmin`/`InvalidState`/`TaskIdempotencyConflict`/
/// `TaskPreviewInvalidState` 409；`ResourceExhausted` 与 `Database`/`Config` 503
/// （后者固定去敏为 `STORAGE_UNAVAILABLE`，cursor 密钥未安装即走此分支）；
/// `SchemaNotReady`/`Crypto` 503 `NOT_READY`。错误串与输入片段一律不进 body。
#[allow(dead_code)]
pub(crate) fn map_error(error: ControllerError) -> Response {
    match error {
        ControllerError::InvalidArgument => err_response(
            StatusCode::BAD_REQUEST,
            "INVALID_ARGUMENT",
            "request.invalid_argument",
            json!({}),
        ),
        ControllerError::PermissionDenied => err_response(
            StatusCode::FORBIDDEN,
            "PERMISSION_DENIED",
            "permission.denied",
            json!({}),
        ),
        ControllerError::NotFound => err_response(
            StatusCode::NOT_FOUND,
            "NOT_FOUND",
            "system.not_found",
            json!({}),
        ),
        ControllerError::RevisionConflict => err_response(
            StatusCode::CONFLICT,
            "REVISION_CONFLICT",
            "system.revision_conflict",
            json!({}),
        ),
        ControllerError::LastAdmin => err_response(
            StatusCode::CONFLICT,
            "LAST_ADMIN",
            "identity.last_admin",
            json!({}),
        ),
        ControllerError::InvalidState => err_response(
            StatusCode::CONFLICT,
            "INVALID_STATE",
            "identity.invalid_state",
            json!({}),
        ),
        ControllerError::TaskIdempotencyConflict => err_response(
            StatusCode::CONFLICT,
            "IDEMPOTENCY_CONFLICT",
            "task.idempotency_conflict",
            json!({}),
        ),
        ControllerError::TaskPreviewInvalidState => err_response(
            StatusCode::CONFLICT,
            "INVALID_STATE",
            "task.preview_invalid_state",
            json!({}),
        ),
        ControllerError::ResourceExhausted => err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "RESOURCE_EXHAUSTED",
            "system.resource_exhausted",
            json!({}),
        ),
        ControllerError::Database(_) | ControllerError::Config(_) => err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "STORAGE_UNAVAILABLE",
            "system.storage_unavailable",
            json!({}),
        ),
        ControllerError::SchemaNotReady { .. } | ControllerError::Crypto => err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "NOT_READY",
            "system.not_ready",
            json!({}),
        ),
    }
}

/// 列表页成功信封：`data = {items, next_cursor}`；`next_cursor` 缺省投影为 `null`。
#[allow(dead_code)]
pub(crate) fn page_response<T: serde::Serialize>(
    items: &[T],
    next_cursor: Option<String>,
) -> Response {
    ok_response(json!({
        "items": items,
        "next_cursor": next_cursor,
    }))
}

// ---------------------------------------------------------------------------
// 统一签名分页 cursor
// ---------------------------------------------------------------------------

const CURSOR_MAC_DOMAIN: &[u8] = b"rsetup-management-cursor-v1/";
const CURSOR_KIND_UUID: u8 = 1;
const CURSOR_KIND_DEVICE: u8 = 2;
const CURSOR_TAG_LEN: usize = 8;
const CURSOR_UUID_HEX: usize = 64;
const CURSOR_DEVICE_HEX: usize = 96;

/// Task 1 映射表的稳定 cursor 序号（1..=16，与表行序一致），仅用于 MAC 输入。
#[allow(dead_code)]
pub(crate) fn endpoint_cursor_code(endpoint: EndpointId) -> u16 {
    match endpoint {
        EndpointId::Me => 1,
        EndpointId::Sessions => 2,
        EndpointId::UsersRead => 3,
        EndpointId::EffectivePermissionsRead => 4,
        EndpointId::RolesRead => 5,
        EndpointId::GrantsRead => 6,
        EndpointId::GroupsRead => 7,
        EndpointId::DevicesRead => 8,
        EndpointId::DeviceDetailRead => 9,
        EndpointId::ApprovalsRead => 10,
        EndpointId::AuditRead => 11,
        EndpointId::SystemStatusRead => 12,
        EndpointId::TaskList => 13,
        EndpointId::TaskDetail => 14,
        EndpointId::TaskChildren => 15,
        EndpointId::DeviceStatusRead => 16,
        // SSE 事件流不分页、不签发 cursor，故不占 Task 1 的 1..=16 映射表；
        // 该分支只为穷尽性服务，任何 cursor 使用都是缺陷。
        EndpointId::EventsRead => 0,
    }
}

/// cursor 绑定的内部键：UUID 键走 24B payload profile，设备公钥走 40B profile。
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CursorKey {
    Uuid([u8; 16]),
    DeviceKey([u8; 32]),
}

/// 严格 query DTO 规范化后的筛选/排序参数集；`limit` 不参与绑定，故不在其中。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct CursorFilterCtx {
    fields: Vec<(u8, Option<Vec<u8>>)>,
}

#[allow(dead_code)]
impl CursorFilterCtx {
    /// 无筛选/排序参数（例如只带 `limit`）时的固定上下文。
    pub(crate) fn empty() -> Self {
        Self { fields: Vec::new() }
    }

    /// 由**已按固定 schema 字段序**给出的参数构造：`tag` 取自该 query DTO 的 schema 常量表，
    /// 必须 `> 0`、严格升序且唯一；`None` 表示该参数缺省。长度超过 `u16::MAX`、`tag = 0`、
    /// 重复或乱序一律 `InvalidArgument` 拒绝——绝不截断、绝不静默重排，避免长度/排序歧义。
    pub(crate) fn from_ordered_fields(
        fields: &[(u8, Option<&[u8]>)],
    ) -> Result<Self, ControllerError> {
        let mut stored: Vec<(u8, Option<Vec<u8>>)> = Vec::with_capacity(fields.len());
        let mut prev_tag: u8 = 0;
        for (tag, value) in fields {
            if *tag == 0 || *tag <= prev_tag {
                return Err(ControllerError::InvalidArgument);
            }
            match value {
                None => stored.push((*tag, None)),
                Some(value) => {
                    if value.len() > u16::MAX as usize {
                        return Err(ControllerError::InvalidArgument);
                    }
                    stored.push((*tag, Some(value.to_vec())));
                }
            }
            prev_tag = *tag;
        }
        Ok(Self { fields: stored })
    }

    /// 规范化字节串：缺省参数写单个 `0`（`tag = 0` 保留为缺省标记），
    /// 其余 `tag(1B) ‖ len(u16 BE) ‖ bytes`；字段顺序即构造顺序，不做二次排序。
    pub(crate) fn canonical(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (tag, value) in &self.fields {
            match value {
                None => out.push(0u8),
                Some(value) => {
                    out.push(*tag);
                    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
                    out.extend_from_slice(value);
                }
            }
        }
        out
    }
}

/// 管理 cursor 域 MAC：`HMAC-SHA256(secret, domain ‖ endpoint(2B BE) ‖ actor(16B) ‖ filter_canon ‖ payload)`。
/// 与 `auth/session.rs` 的私有实现同形（`sha2` 手工 ipad/opad），但域常量与输入账不同，故独立实现。
#[allow(dead_code)]
fn management_cursor_mac(
    secret: &[u8; 32],
    endpoint_code: u16,
    actor: &[u8; 16],
    filter_canon: &[u8],
    payload: &[u8],
) -> [u8; 32] {
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= secret[i];
        opad[i] ^= secret[i];
    }
    let mut inner = sha2::Sha256::new();
    inner.update(ipad);
    inner.update(CURSOR_MAC_DOMAIN);
    inner.update(endpoint_code.to_be_bytes());
    inner.update(actor);
    inner.update(filter_canon);
    inner.update(payload);
    let inner_hash = inner.finalize();
    let mut outer = sha2::Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

/// 固定长 xor 累积比较（8B tag）：不短路、不提前返回，不泄露首个差异位置。
#[allow(dead_code)]
fn management_cursor_tag_eq(a: &[u8; CURSOR_TAG_LEN], b: &[u8; CURSOR_TAG_LEN]) -> bool {
    let mut diff = 0u8;
    for i in 0..CURSOR_TAG_LEN {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[allow(dead_code)]
fn management_cursor_all_zero(bytes: &[u8]) -> bool {
    let mut acc = 0u8;
    for b in bytes {
        acc |= b;
    }
    acc == 0
}

/// payload 字节账：UUID `2+1+1+16+4 = 24B`，设备公钥 `2+1+1+32+4 = 40B`；`reserved` 与 4B 填充恒写全零。
#[allow(dead_code)]
fn management_cursor_payload(endpoint_code: u16, key: &CursorKey) -> Vec<u8> {
    let mut payload = Vec::with_capacity(40);
    payload.extend_from_slice(&endpoint_code.to_be_bytes());
    match key {
        CursorKey::Uuid(uuid) => {
            payload.push(CURSOR_KIND_UUID);
            payload.push(0u8);
            payload.extend_from_slice(uuid);
            payload.extend_from_slice(&[0u8; 4]);
        }
        CursorKey::DeviceKey(public_key) => {
            payload.push(CURSOR_KIND_DEVICE);
            payload.push(0u8);
            payload.extend_from_slice(public_key);
            payload.extend_from_slice(&[0u8; 4]);
        }
    }
    payload
}

#[allow(dead_code)]
pub(crate) fn encode_signed_cursor(
    secret: &[u8; 32],
    endpoint: EndpointId,
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    key: CursorKey,
) -> String {
    let endpoint_code = endpoint_cursor_code(endpoint);
    let payload = management_cursor_payload(endpoint_code, &key);
    let mac = management_cursor_mac(
        secret,
        endpoint_code,
        &user_id,
        &filter.canonical(),
        &payload,
    );
    let mut raw = Vec::with_capacity(payload.len() + CURSOR_TAG_LEN);
    raw.extend_from_slice(&payload);
    raw.extend_from_slice(&mac[..CURSOR_TAG_LEN]);
    hex::encode(raw)
}

#[allow(dead_code)]
pub(crate) fn decode_signed_cursor(
    secret: &[u8; 32],
    endpoint: EndpointId,
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    token: &str,
) -> Result<CursorKey, ControllerError> {
    // ① 严格长度与全小写 hex：长度不符、大写 hex、非 hex 一律先拒，不进入 MAC。
    if token.len() != CURSOR_UUID_HEX && token.len() != CURSOR_DEVICE_HEX {
        return Err(ControllerError::InvalidArgument);
    }
    if !token
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ControllerError::InvalidArgument);
    }
    let raw = hex::decode(token).map_err(|_| ControllerError::InvalidArgument)?;
    if raw.len() != CURSOR_UUID_HEX / 2 && raw.len() != CURSOR_DEVICE_HEX / 2 {
        return Err(ControllerError::InvalidArgument);
    }
    let (payload, tag_bytes) = raw.split_at(raw.len() - CURSOR_TAG_LEN);
    let mut tag = [0u8; CURSOR_TAG_LEN];
    tag.copy_from_slice(tag_bytes);
    // ② 以当前 endpoint / user / filter 上下文重算 MAC，固定长比较 8B tag。
    let endpoint_code = endpoint_cursor_code(endpoint);
    let mac = management_cursor_mac(
        secret,
        endpoint_code,
        &user_id,
        &filter.canonical(),
        payload,
    );
    let mut expected = [0u8; CURSOR_TAG_LEN];
    expected.copy_from_slice(&mac[..CURSOR_TAG_LEN]);
    if !management_cursor_tag_eq(&tag, &expected) {
        return Err(ControllerError::InvalidArgument);
    }
    // ③ tag 通过后才解析字段：MAC 覆盖 payload，字段篡改必已在 ② 失败；以下为第二道防线，
    //    负向 signed-malformed 输入仍逐项显式校验。任一步失败都是同一个去敏 InvalidArgument。
    if payload.len() != 24 && payload.len() != 40 {
        return Err(ControllerError::InvalidArgument);
    }
    let kind = payload[2];
    if u16::from_be_bytes([payload[0], payload[1]]) != endpoint_code || payload[3] != 0u8 {
        return Err(ControllerError::InvalidArgument);
    }
    match kind {
        CURSOR_KIND_UUID => {
            if payload.len() != 24 || !management_cursor_all_zero(&payload[20..24]) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut uuid = [0u8; 16];
            uuid.copy_from_slice(&payload[4..20]);
            // UUIDv4：version nibble = 4，RFC 4122 variant = 10xx。
            if uuid[6] >> 4 != 0x4 || uuid[8] & 0b1100_0000 != 0b1000_0000 {
                return Err(ControllerError::InvalidArgument);
            }
            Ok(CursorKey::Uuid(uuid))
        }
        CURSOR_KIND_DEVICE => {
            if payload.len() != 40 || !management_cursor_all_zero(&payload[36..40]) {
                return Err(ControllerError::InvalidArgument);
            }
            let mut public_key = [0u8; 32];
            public_key.copy_from_slice(&payload[4..36]);
            Ok(CursorKey::DeviceKey(public_key))
        }
        _ => Err(ControllerError::InvalidArgument),
    }
}

// ---------------------------------------------------------------------------
// cursor 密钥生产链
// ---------------------------------------------------------------------------
//
// 32B 密钥的唯一来源是环境变量 `CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX`：只接受恰 64 个
// 小写 hex 字符并解码成 32B，缺失/非法/长度不符一律固定去敏 `ControllerError::Config`，
// 错误串不回显输入片段，任何路径都不得打印或记录取值。读取与安装只发生在集成 Task 0
// 注册管理 router **之前**（load → install → build_management_router），任一步 `Err` 即
// 管理 router 不注册、进程失败关闭，绝不以「无 cursor 密钥」降级启动。

/// 管理 cursor 密钥的唯一进程内安装位：只允许一次性写入，不提供 clear/reset，
/// 也不给 `ManagementState` 或各资源 `ApiState` 扩密钥字段。
pub(crate) static MANAGEMENT_CURSOR_SECRET: OnceLock<[u8; 32]> = OnceLock::new();

/// 解析环境变量取值：仅接受恰 64 个小写 hex 字符，其余固定去敏 `Config`。
pub fn parse_management_cursor_secret(raw: &str) -> Result<[u8; 32], ControllerError> {
    if raw.len() != 64
        || !raw
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ControllerError::Config(
            "management cursor secret format".into(),
        ));
    }
    let mut secret = [0u8; 32];
    hex::decode_to_slice(raw, &mut secret)
        .map_err(|_| ControllerError::Config("management cursor secret format".into()))?;
    Ok(secret)
}

/// 从环境变量读取并解析密钥；变量缺失同样是固定去敏 `Config`，不回显变量名之外的信息。
pub fn load_management_cursor_secret_from_env() -> Result<[u8; 32], ControllerError> {
    let raw = std::env::var("CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX")
        .map_err(|_| ControllerError::Config("management cursor secret missing".into()))?;
    parse_management_cursor_secret(&raw)
}

/// 一次性安装语义：未安装→写入成功；重复安装同一 key→幂等 `Ok`；重复安装不同 key→
/// `Config` 失败并保留既有 key。
///
/// `OnceLock::set` 的 `Err` 返回的是被拒绝的**本次入参**而非已安装值，拿返回值比较恒为真、
/// 会误接受不同 key；因此失败后经 `slot.get()` 比较（`set` 失败即已填充，`get` 保证可读）。
fn install_cursor_secret_into(
    slot: &OnceLock<[u8; 32]>,
    secret: &[u8; 32],
) -> Result<(), ControllerError> {
    if slot.set(*secret).is_ok() {
        return Ok(());
    }
    match slot.get() {
        Some(installed) if installed == secret => Ok(()),
        _ => Err(ControllerError::Config(
            "management cursor secret already installed".into(),
        )),
    }
}

/// 生产安装入口（集成 Task 0 在注册管理 router 前调用一次）。
pub fn install_management_cursor_secret(secret: &[u8; 32]) -> Result<(), ControllerError> {
    install_cursor_secret_into(&MANAGEMENT_CURSOR_SECRET, secret)
}

fn cursor_secret_of(slot: &OnceLock<[u8; 32]>) -> Result<&[u8; 32], ControllerError> {
    slot.get()
        .ok_or_else(|| ControllerError::Config("management cursor secret not installed".into()))
}

/// handler 侧唯一取密钥入口；未安装 → `Config` → 既有 `map_error` 精确 503
/// `STORAGE_UNAVAILABLE`，绝不回退未签名 cursor。
#[allow(dead_code)]
pub(crate) fn management_cursor_secret() -> Result<&'static [u8; 32], ControllerError> {
    cursor_secret_of(&MANAGEMENT_CURSOR_SECRET)
}

// ---------------------------------------------------------------------------
// HTTP 边界 cursor helper
// ---------------------------------------------------------------------------
//
// handler 不得直接调用 `encode_signed_cursor`/`decode_signed_cursor` 裸函数对，只经本节
// 两个 helper；参数一律显式传入 `(secret, endpoint, user_id, filter)`，不引入 state 参数。
// 失败分层：客户端侧（非法/篡改/跨端点/跨用户/换筛选 token、profile 不符）→ `InvalidArgument`
// 400；后端侧（page 形状、raw key 长度/大小写/v4 shape、端点不在白名单）→ 固定去敏
// `Config` 503，两条路径都不得返回未校验的内部 `CursorKey`。

/// 管理分页端点的 cursor 键形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ManagementCursorProfile {
    Uuid,
    Device,
}

/// endpoint → profile 的封闭白名单（唯一真源，失败关闭）：只有 7 个管理分页读端点有
/// profile，其余端点（含 `AuditRead` 独立 MAC profile）一律固定去敏 `Config`，不写默认
/// 兜底掩盖装配错误。`EffectivePermissionsRead` 自 Task 13a 起按 spec 02 §3/§121 支持
/// `cursor?` + Page：其分页键是设备公钥 `[u8;32]`，故复用统一 `Device` profile（外部
/// 96 hex），cursor 的 filter 绑定 target 用户 + `device_id` 筛选——不另造第二套 MAC codec。
#[allow(dead_code)]
pub(crate) fn management_cursor_profile(
    endpoint: EndpointId,
) -> Result<ManagementCursorProfile, ControllerError> {
    match endpoint {
        EndpointId::UsersRead
        | EndpointId::RolesRead
        | EndpointId::GrantsRead
        | EndpointId::GroupsRead => Ok(ManagementCursorProfile::Uuid),
        EndpointId::DevicesRead
        | EndpointId::ApprovalsRead
        | EndpointId::EffectivePermissionsRead => Ok(ManagementCursorProfile::Device),
        _ => Err(ControllerError::Config(
            "management cursor endpoint profile".to_string(),
        )),
    }
}

/// 入口 helper：HTTP `cursor` → 网关 typed key。`None` 直接 `Ok(None)`（不触碰 MAC）；
/// `Some` 必须经真实 `decode_signed_cursor`，再校验解码结果落在本 endpoint 预期 profile，
/// profile 不符或 token 非法一律 `InvalidArgument` → 400，无回退、不降级全量。
#[allow(dead_code)]
pub(crate) fn decode_management_list_cursor(
    secret: &[u8; 32],
    endpoint: EndpointId,
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    token: Option<&str>,
) -> Result<Option<CursorKey>, ControllerError> {
    let profile = management_cursor_profile(endpoint)?;
    let raw = match token {
        None => return Ok(None),
        Some(raw) => raw,
    };
    let key = decode_signed_cursor(secret, endpoint, user_id, filter, raw)?;
    let bound = matches!(
        (profile, &key),
        (ManagementCursorProfile::Uuid, CursorKey::Uuid(_))
            | (ManagementCursorProfile::Device, CursorKey::DeviceKey(_))
    );
    if !bound {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(Some(key))
}

fn management_page_shape_error() -> ControllerError {
    ControllerError::Config("management cursor page shape".to_string())
}

/// 内部 raw key 的严格字节账：长度恰 `N * 2` 且全小写 hex，否则 `None`（不截断、不回显）。
fn management_page_raw_bytes<const N: usize>(raw: &str) -> Option<[u8; N]> {
    if raw.len() != N * 2 || !raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let bytes = hex::decode(raw).ok()?;
    bytes.as_slice().try_into().ok()
}

/// 后端内部 raw string → `CursorKey`，长度与 endpoint profile 互锁，
/// UUID 另校验 canonical v4 shape；任何不符都是同一个去敏 `Config`。
fn management_page_internal_key(
    endpoint: EndpointId,
    raw: &str,
) -> Result<CursorKey, ControllerError> {
    match management_cursor_profile(endpoint)? {
        ManagementCursorProfile::Uuid => {
            let uuid =
                management_page_raw_bytes::<16>(raw).ok_or_else(management_page_shape_error)?;
            if uuid[6] >> 4 != 4 || uuid[8] & 0xC0 != 0x80 {
                return Err(management_page_shape_error());
            }
            Ok(CursorKey::Uuid(uuid))
        }
        ManagementCursorProfile::Device => {
            let key =
                management_page_raw_bytes::<32>(raw).ok_or_else(management_page_shape_error)?;
            Ok(CursorKey::DeviceKey(key))
        }
    }
}

/// 出口 helper：网关内部 raw key → 外部签名 token。只接受 `Value::Object`，`items` 必须是
/// array，`next_cursor` 必须存在且为 `null` 或 `String`；第一行先过与入口成对的封闭白名单，
/// 因此 `next_cursor` 为 `null` 的非白名单端点同样被拦截。投影守恒：只替换 `next_cursor`
/// 一个字段，`items` 与其余字段原样保留，不把裸内部 key 写进响应。
#[allow(dead_code)]
pub(crate) fn sign_management_list_page(
    secret: &[u8; 32],
    endpoint: EndpointId,
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    page: serde_json::Value,
) -> Result<serde_json::Value, ControllerError> {
    management_cursor_profile(endpoint)?;
    let mut map = match page {
        serde_json::Value::Object(map) => map,
        _ => return Err(management_page_shape_error()),
    };
    if !matches!(map.get("items"), Some(serde_json::Value::Array(_))) {
        return Err(management_page_shape_error());
    }
    let raw_next = match map.get("next_cursor") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(raw)) => Some(raw.clone()),
        _ => return Err(management_page_shape_error()),
    };
    let next_cursor = match raw_next {
        None => serde_json::Value::Null,
        Some(raw) => {
            let key = management_page_internal_key(endpoint, &raw)?;
            serde_json::Value::String(encode_signed_cursor(secret, endpoint, user_id, filter, key))
        }
    };
    map.insert("next_cursor".to_string(), next_cursor);
    Ok(serde_json::Value::Object(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::sqlx_repo::UserRecord;

    #[test]
    fn revision_parser_fails_closed_on_plus_prefix_and_overflow() {
        assert_eq!(parse_revision("42").unwrap(), 42);
        assert_eq!(parse_revision(&u64::MAX.to_string()).unwrap(), u64::MAX);
        assert!(parse_revision("+42").is_err(), "leading plus is forbidden");
        assert!(parse_revision("").is_err(), "empty revision is forbidden");
        assert!(parse_revision("not-digits").is_err());
        assert!(
            parse_revision("18446744073709551616").is_err(),
            "u64 overflow must be 400"
        );
    }

    #[test]
    fn limit_parser_defaults_to_50_and_caps_at_200() {
        assert_eq!(parse_limit(None).unwrap(), 50);
        assert_eq!(parse_limit(Some("100")).unwrap(), 100);
        assert_eq!(parse_limit(Some("200")).unwrap(), 200);
        assert!(parse_limit(Some("0")).is_err(), "0 is invalid");
        assert!(parse_limit(Some("201")).is_err(), ">200 is invalid");
        assert!(parse_limit(Some("abc")).is_err());
    }

    #[test]
    fn user_admin_public_never_exposes_password_hash_and_uses_decimal_revision() {
        let user = UserRecord {
            id: [0x42u8; 16],
            username: "admin".into(),
            display_name: "Root".into(),
            active: true,
            is_admin: true,
            must_change_password: false,
            revision: 7,
        };
        let value = crate::http_api::user_admin_public(&user);
        assert_eq!(value["revision"], "7");
        assert_eq!(value["display_name"], "Root");
        assert!(value.get("password_hash").is_none());
        assert_eq!(value["id"], "42424242-4242-4242-4242-424242424242");
    }
}

#[cfg(test)]
mod response_mapping_tests {
    use super::*;
    use axum::body::to_bytes;

    async fn error_envelope(resp: Response) -> (StatusCode, String, String) {
        let status = resp.status();
        let bytes = to_bytes(resp.into_body(), 64 * 1024).await.expect("body");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        (
            status,
            value["error"]["code"].as_str().unwrap_or("").to_string(),
            value["error"]["message_key"]
                .as_str()
                .unwrap_or("")
                .to_string(),
        )
    }

    #[tokio::test]
    async fn map_error_covers_every_spec02_branch() {
        let cases: Vec<(ControllerError, StatusCode, &str, &str)> = vec![
            (
                ControllerError::InvalidArgument,
                StatusCode::BAD_REQUEST,
                "INVALID_ARGUMENT",
                "request.invalid_argument",
            ),
            (
                ControllerError::PermissionDenied,
                StatusCode::FORBIDDEN,
                "PERMISSION_DENIED",
                "permission.denied",
            ),
            (
                ControllerError::NotFound,
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "system.not_found",
            ),
            (
                ControllerError::RevisionConflict,
                StatusCode::CONFLICT,
                "REVISION_CONFLICT",
                "system.revision_conflict",
            ),
            (
                ControllerError::LastAdmin,
                StatusCode::CONFLICT,
                "LAST_ADMIN",
                "identity.last_admin",
            ),
            (
                ControllerError::InvalidState,
                StatusCode::CONFLICT,
                "INVALID_STATE",
                "identity.invalid_state",
            ),
            (
                ControllerError::TaskIdempotencyConflict,
                StatusCode::CONFLICT,
                "IDEMPOTENCY_CONFLICT",
                "task.idempotency_conflict",
            ),
            (
                ControllerError::TaskPreviewInvalidState,
                StatusCode::CONFLICT,
                "INVALID_STATE",
                "task.preview_invalid_state",
            ),
            (
                ControllerError::ResourceExhausted,
                StatusCode::SERVICE_UNAVAILABLE,
                "RESOURCE_EXHAUSTED",
                "system.resource_exhausted",
            ),
            (
                ControllerError::Config("synthetic".into()),
                StatusCode::SERVICE_UNAVAILABLE,
                "STORAGE_UNAVAILABLE",
                "system.storage_unavailable",
            ),
            (
                ControllerError::SchemaNotReady {
                    found: Some(1),
                    required: 2,
                },
                StatusCode::SERVICE_UNAVAILABLE,
                "NOT_READY",
                "system.not_ready",
            ),
            (
                ControllerError::Crypto,
                StatusCode::SERVICE_UNAVAILABLE,
                "NOT_READY",
                "system.not_ready",
            ),
        ];
        for (error, status, code, key) in cases {
            let (actual, actual_code, actual_key) = error_envelope(map_error(error)).await;
            assert_eq!(actual, status, "status for {code}");
            assert_eq!(actual_code, code);
            assert_eq!(actual_key, key);
        }
    }

    #[tokio::test]
    async fn map_error_never_echoes_error_text() {
        let (status, code, _) = error_envelope(map_error(ControllerError::Config(
            "synthetic-secret-like-fragment".into(),
        )))
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "STORAGE_UNAVAILABLE");
    }

    #[tokio::test]
    async fn page_response_projects_items_and_next_cursor() {
        let with_cursor = page_response(&["a", "b"], Some("token".to_string()));
        assert_eq!(with_cursor.status(), StatusCode::OK);
        let bytes = to_bytes(with_cursor.into_body(), 64 * 1024)
            .await
            .expect("body");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["data"]["items"], serde_json::json!(["a", "b"]));
        assert_eq!(value["data"]["next_cursor"], "token");
        assert!(value["request_id"].is_string());

        let without_cursor = page_response::<u8>(&[], None);
        let bytes = to_bytes(without_cursor.into_body(), 64 * 1024)
            .await
            .expect("body");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["data"]["items"], serde_json::json!([]));
        assert!(value["data"]["next_cursor"].is_null());
    }

    #[tokio::test]
    async fn device_id_path_accepts_only_lowercase_hex64() {
        let good = "ab".repeat(32);
        let parsed = parse_device_id_path(&good).expect("lowercase hex64 accepted");
        assert_eq!(parsed, [0xabu8; 32]);
        for raw in [
            String::new(),
            "AB".repeat(32),
            "serial-001".to_string(),
            "ab".repeat(31),
            "ab".repeat(33),
            "0x".to_string() + &"ab".repeat(31),
        ] {
            let resp = parse_device_id_path(&raw).expect_err("malformed device id rejected");
            let (status, code, key) = error_envelope(resp).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "case {raw:?}");
            assert_eq!(code, "INVALID_ARGUMENT");
            assert_eq!(key, "device.invalid_id");
        }
    }
}

#[cfg(test)]
mod uuid_path_tests {
    use super::*;

    const CANONICAL: &str = "067e6162-ea94-4431-836c-7d8fa2010b3c";

    #[test]
    fn uuid_path_accepts_canonical_lowercase_only() {
        let parsed = parse_uuid_path(CANONICAL).expect("canonical lowercase must be accepted");
        assert_eq!(parsed.to_string(), CANONICAL);
    }

    #[tokio::test]
    async fn uuid_path_rejects_noncanonical_forms_with_400_invalid_id() {
        for raw in [
            "",
            "067E6162-EA94-4431-836C-7D8FA2010B3C",
            "067e6162-EA94-4431-836C-7D8FA2010b3c",
            "{067e6162-ea94-4431-836c-7d8fa2010b3c}",
            "067e6162ea944431836c7d8fa2010b3c",
            "urn:uuid:067e6162-ea94-4431-836c-7d8fa2010b3c",
            " 067e6162-ea94-4431-836c-7d8fa2010b3c",
            "067e6162-ea94-4431-836c-7d8fa2010b3c ",
            "067e6162-ea94-4431-836c-7d8fa2010b3c\n",
            "067e6162-ea94-4431-836c-7d8fa2010b3",
            "067e6162-ea94-4431-836c-7d8fa2010b3cc",
            "067e6162-ea94-4431-836c-7d8fa2010b3г",
        ] {
            let resp = parse_uuid_path(raw).expect_err("non-canonical form must be rejected");
            assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "case {raw:?}");
            let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024)
                .await
                .expect("error body");
            let text = String::from_utf8(bytes.to_vec()).expect("utf-8 json");
            assert!(text.contains("INVALID_ARGUMENT"), "case {raw:?}: {text}");
            assert!(text.contains("request.invalid_id"), "case {raw:?}: {text}");
        }
    }
}

#[cfg(test)]
mod signed_cursor_tests {
    use super::*;
    const SECRET: [u8; 32] = [7u8; 32];
    const ACTOR: [u8; 16] = [1u8; 16];
    const OTHER_ACTOR: [u8; 16] = [2u8; 16];
    fn fixture_uuid_v4() -> [u8; 16] {
        let raw = hex::decode("067e6162ea944431836c7d8fa2010b3c").expect("fixture hex");
        <[u8; 16]>::try_from(raw).expect("fixture 16 bytes")
    }
    /// `from_ordered_fields` 入参形状别名，仅测试内使用（规避 clippy::type_complexity）。
    type FilterFields<'a> = Vec<(u8, Option<&'a [u8]>)>;
    #[test]
    fn signed_cursor_roundtrip_encodes_and_decodes_bound_key() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let uuid = fixture_uuid_v4();
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, CursorKey::Uuid(uuid));
        assert_eq!(token.len(), CURSOR_UUID_HEX);
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        let raw = hex::decode(&token).expect("lowercase hex");
        assert_eq!(raw[2], CURSOR_KIND_UUID);
        assert_eq!(raw[3], 0u8);
        assert!(management_cursor_all_zero(&raw[20..24]));
        let decoded = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token)
            .expect("uuid profile roundtrip");
        assert_eq!(decoded, CursorKey::Uuid(uuid));
        for public_key in [[0u8; 32], [9u8; 32]] {
            let endpoint = EndpointId::DevicesRead;
            let key = CursorKey::DeviceKey(public_key);
            let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
            assert_eq!(token.len(), CURSOR_DEVICE_HEX);
            let raw = hex::decode(&token).expect("lowercase hex");
            assert_eq!(raw[2], CURSOR_KIND_DEVICE);
            assert_eq!(raw[3], 0u8);
            assert!(management_cursor_all_zero(&raw[36..40]));
            let decoded = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token)
                .expect("device profile roundtrip");
            assert_eq!(decoded, CursorKey::DeviceKey(public_key));
        }
    }
    #[test]
    fn signed_cursor_rejects_tampered_tag_with_invalid_argument() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("valid token");
        let mut raw = hex::decode(&token).expect("lowercase hex");
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        let tampered = hex::encode(&raw);
        assert_ne!(tampered, token);
        let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &tampered)
            .expect_err("tampered tag rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_rejects_other_user() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("same user");
        let err = decode_signed_cursor(&SECRET, endpoint, OTHER_ACTOR, &filter, &token)
            .expect_err("other user rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_rejects_changed_filter() {
        let endpoint = EndpointId::RolesRead;
        let filter_asc = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&b"asc"[..]))])
            .expect("fixture filter");
        let filter_desc = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&b"desc"[..]))])
            .expect("fixture filter");
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter_asc, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter_asc, &token).expect("same filter");
        let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter_desc, &token)
            .expect_err("changed filter value rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_rejects_cross_endpoint() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("same endpoint");
        let err = decode_signed_cursor(&SECRET, EndpointId::RolesRead, ACTOR, &filter, &token)
            .expect_err("cross endpoint rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    /// 用真实 `management_cursor_mac` 对任意 payload 重新签名：只改字段，不改 MAC 算法与 tag 截断规则。
    fn resign(payload: &[u8], endpoint: EndpointId, filter: &CursorFilterCtx) -> String {
        let mac = management_cursor_mac(
            &SECRET,
            endpoint_cursor_code(endpoint),
            &ACTOR,
            &filter.canonical(),
            payload,
        );
        let mut raw = payload.to_vec();
        raw.extend_from_slice(&mac[..CURSOR_TAG_LEN]);
        hex::encode(&raw)
    }
    #[test]
    fn signed_cursor_rejects_invalid_input_shapes() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::empty();
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key.clone());
        let control =
            decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("control token");
        assert_eq!(control, key);
        let upper = token.to_uppercase();
        assert!(
            upper.bytes().any(|b| (b'A'..=b'F').contains(&b)),
            "大写样本须真含 A-F"
        );
        for bad in [
            token[..CURSOR_UUID_HEX - 1].to_owned(),
            format!("{token}00"),
            upper,
            format!("g{}", &token[1..]),
        ] {
            let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &bad)
                .expect_err("invalid input shape rejected");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
    }
    #[test]
    fn signed_cursor_rejects_resigned_malformed_fields() {
        let endpoint = EndpointId::UsersRead;
        let filter = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&b"asc"[..]))])
            .expect("fixture filter");
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key);
        decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token).expect("valid token");
        let raw = hex::decode(&token).expect("lowercase hex");
        let (payload, tag) = raw.split_at(raw.len() - CURSOR_TAG_LEN);
        assert_eq!(payload.len() + tag.len(), CURSOR_UUID_HEX / 2);
        assert!(
            matches!(
                decode_signed_cursor(
                    &SECRET,
                    endpoint,
                    ACTOR,
                    &filter,
                    &resign(payload, endpoint, &filter)
                ),
                Ok(CursorKey::Uuid(_))
            ),
            "resign 未篡改 payload 必须仍可解码，否则 helper 没走真实 MAC"
        );
        // (payload 下标, 篡改值)：reserved / 4B 填充 / 未知 kind / kind=2 却 24B / UUID version / variant。
        for (index, value) in [
            (3usize, 1u8),
            (20, 1),
            (2, 3),
            (2, CURSOR_KIND_DEVICE),
            (10, 0x34),
            (12, 0x03),
        ] {
            let mut mutated = payload.to_vec();
            mutated[index] = value;
            assert_ne!(mutated, payload, "mutation {index} 未改变 payload");
            let err = decode_signed_cursor(
                &SECRET,
                endpoint,
                ACTOR,
                &filter,
                &resign(&mutated, endpoint, &filter),
            )
            .expect_err("resigned malformed payload rejected");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
        // payload[0..2] 端点码：改成另一端点序号后用当前端点的正确 MAC 重签，仍须失败。
        let mut cross = payload.to_vec();
        cross[..2].copy_from_slice(&endpoint_cursor_code(EndpointId::RolesRead).to_be_bytes());
        assert_ne!(cross, payload);
        let err = decode_signed_cursor(
            &SECRET,
            endpoint,
            ACTOR,
            &filter,
            &resign(&cross, endpoint, &filter),
        )
        .expect_err("payload endpoint code rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
    #[test]
    fn signed_cursor_filter_context_rejects_noncanonical_fields() {
        let oversized = vec![0u8; u16::MAX as usize + 1];
        let cases: Vec<FilterFields<'_>> = vec![
            vec![(0u8, Some(&b"x"[..]))],
            vec![(1u8, Some(&b"x"[..])), (1u8, Some(&b"y"[..]))],
            vec![(2u8, None), (1u8, None)],
            vec![(1u8, Some(oversized.as_slice()))],
        ];
        for fields in cases {
            let err = CursorFilterCtx::from_ordered_fields(&fields)
                .expect_err("non-canonical filter rejected");
            assert!(matches!(err, ControllerError::InvalidArgument));
        }
        // 合法 fixed-tag：None 写单个 0，Some 写 tag ‖ len(u16 BE) ‖ bytes，顺序即构造顺序。
        let filter = CursorFilterCtx::from_ordered_fields(&[(1u8, None), (2u8, Some(&b"asc"[..]))])
            .expect("canonical filter");
        assert_eq!(
            filter.canonical(),
            vec![0u8, 2u8, 0u8, 3u8, b'a', b's', b'c']
        );
        assert!(CursorFilterCtx::empty().canonical().is_empty());
        let endpoint = EndpointId::RolesRead;
        let key = CursorKey::Uuid(fixture_uuid_v4());
        let token = encode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, key.clone());
        assert_eq!(token.len(), CURSOR_UUID_HEX);
        let control = decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &token)
            .expect("canonical filter roundtrip");
        assert_eq!(control, key);
        let other = CursorFilterCtx::from_ordered_fields(&[
            (1u8, None),
            (2u8, Some(&b"asc"[..])),
            (3u8, None),
        ])
        .expect("other filter");
        assert_ne!(other.canonical(), filter.canonical());
        let err = decode_signed_cursor(&SECRET, endpoint, ACTOR, &other, &token)
            .expect_err("changed filter context rejected");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }
}

/// cursor 密钥生产链：安装/读取语义经**局部 `OnceLock` 注入的私有 helper** 验证，
/// 生产 wrapper 调用同一 helper，避免全局静态互相污染；不在测试里 `set_var`，
/// `SYNTHETIC_HEX` 是仅测试内使用的固定 synthetic key，不引用任何 secret 文件。
#[cfg(test)]
mod cursor_secret_tests {
    use super::*;
    const SYNTHETIC_HEX: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    #[test]
    fn parse_management_cursor_secret_rejects_invalid_input() {
        assert_eq!(
            parse_management_cursor_secret(SYNTHETIC_HEX).expect("64 位小写 hex 必须通过"),
            {
                let mut k = [0u8; 32];
                k[31] = 1;
                k
            }
        );
        // SYNTHETIC_HEX 仅含数字 0/1，to_uppercase 仍返回同一合法值，不能充当大写拒绝样本；
        // 改用确实 64 字符且含大写 A-F 的 synthetic hex，并断言其形状以免未来退化。
        let uppercase_hex = "A".repeat(64);
        assert_eq!(uppercase_hex.len(), 64, "大写样本必须恰好 64 字符");
        assert!(
            uppercase_hex.chars().any(|c| matches!(c, 'A'..='F')),
            "大写样本必须至少含一个大写 hex 字符"
        );
        let bad = [
            SYNTHETIC_HEX[..63].to_owned(),
            uppercase_hex,
            format!("{SYNTHETIC_HEX}0"),
            format!("{}g", &SYNTHETIC_HEX[..63]),
            "z".repeat(64),
            String::new(),
        ];
        for case in &bad {
            let err = parse_management_cursor_secret(case).expect_err("非法输入必须拒绝");
            assert!(
                matches!(&err, ControllerError::Config(m) if m == "management cursor secret format")
                    && (case.len() < 8 || !format!("{err}").contains(&case[..8])),
                "非法输入须固定去敏拒绝且不回显密钥片段"
            );
        }
    }
    #[test]
    fn cursor_secret_accessor_requires_installed_slot() {
        let err = cursor_secret_of(&OnceLock::<[u8; 32]>::new()).expect_err("未安装必须失败");
        assert!(matches!(
            &err,
            ControllerError::Config(m) if m == "management cursor secret not installed"
        ));
        assert_eq!(
            map_error(err).status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }
    #[test]
    fn install_cursor_secret_is_idempotent_only_for_same_key() {
        let (slot, key) = (
            OnceLock::<[u8; 32]>::new(),
            parse_management_cursor_secret(SYNTHETIC_HEX).expect("合法 key"),
        );
        assert!(install_cursor_secret_into(&slot, &key).is_ok());
        assert!(
            install_cursor_secret_into(&slot, &key).is_ok(),
            "同 key 重复安装应幂等成功"
        );
        let err = install_cursor_secret_into(&slot, &[7u8; 32]).expect_err("不同 key 必须失败");
        assert!(matches!(
            &err,
            ControllerError::Config(m) if m == "management cursor secret already installed"
        ));
        assert_eq!(
            cursor_secret_of(&slot).expect("已安装"),
            &key,
            "不同 key 不得覆盖既有 key"
        );
    }
}

/// HTTP 边界 cursor helper：6 个分页端点的 profile 封闭互锁、入口/出口转换与后端脏页防御。
#[cfg(test)]
mod management_cursor_helper_tests {
    use super::*;
    const SECRET: [u8; 32] = [3u8; 32];
    const ACTOR: [u8; 16] = [1u8; 16];
    /// canonical v4 的 16B 内部 raw key（32 小写 hex）。
    const UUID_RAW_HEX: &str = "067e6162ea944431836c7d8fa2010b3c";
    /// 32B 设备公钥内部 raw key（64 小写 hex）。
    const DEVICE_RAW_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn fixture_uuid() -> [u8; 16] {
        <[u8; 16]>::try_from(hex::decode(UUID_RAW_HEX).expect("fixture hex")).expect("16B")
    }
    fn fixture_device_key() -> [u8; 32] {
        <[u8; 32]>::try_from(hex::decode(DEVICE_RAW_HEX).expect("fixture hex")).expect("32B")
    }
    /// 后端侧失败必须是固定去敏 `Config`，经 `map_error` 精确 503，不回显片段、不返回 400。
    fn assert_backend_fail_closed(error: ControllerError) {
        assert!(
            matches!(error, ControllerError::Config(_)),
            "后端侧失败须为 Config"
        );
        assert_eq!(
            map_error(error).status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "后端侧失败须精确 503"
        );
    }
    fn page(next_cursor: Option<&str>) -> serde_json::Value {
        json!({
            "items": [{"id": UUID_RAW_HEX}],
            "next_cursor": next_cursor,
            "total": 2,
        })
    }

    #[test]
    fn management_cursor_profile_is_closed_whitelist_of_seven_read_endpoints() {
        for endpoint in [
            EndpointId::UsersRead,
            EndpointId::RolesRead,
            EndpointId::GrantsRead,
            EndpointId::GroupsRead,
        ] {
            assert_eq!(
                management_cursor_profile(endpoint).expect("uuid profile"),
                ManagementCursorProfile::Uuid
            );
        }
        // `EffectivePermissionsRead` 的分页键是设备公钥 `[u8;32]` → 复用 Device profile，
        // filter 绑定 target 用户 + `device_id` 筛选（spec 02 §121），不另造第二套 codec。
        for endpoint in [
            EndpointId::DevicesRead,
            EndpointId::ApprovalsRead,
            EndpointId::EffectivePermissionsRead,
        ] {
            assert_eq!(
                management_cursor_profile(endpoint).expect("device profile"),
                ManagementCursorProfile::Device
            );
        }
        // 其余 9 个端点全部落 `_ =>` 固定去敏 Config：无默认兜底。
        for endpoint in [
            EndpointId::Me,
            EndpointId::Sessions,
            EndpointId::DeviceDetailRead,
            EndpointId::AuditRead,
            EndpointId::SystemStatusRead,
            EndpointId::TaskList,
            EndpointId::TaskDetail,
            EndpointId::TaskChildren,
            EndpointId::DeviceStatusRead,
        ] {
            let err = management_cursor_profile(endpoint).expect_err("非白名单端点必须失败关闭");
            assert!(matches!(
                &err,
                ControllerError::Config(m) if m == "management cursor endpoint profile"
            ));
            assert_backend_fail_closed(err);
        }
    }

    #[test]
    fn decode_management_list_cursor_binds_endpoint_profile() {
        let filter = CursorFilterCtx::empty();
        // `None` 直接 Ok(None)，不触碰 MAC。
        assert_eq!(
            decode_management_list_cursor(&SECRET, EndpointId::UsersRead, ACTOR, &filter, None)
                .expect("无 cursor 直接放行"),
            None
        );
        let uuid = fixture_uuid();
        let uuid_token = encode_signed_cursor(
            &SECRET,
            EndpointId::UsersRead,
            ACTOR,
            &filter,
            CursorKey::Uuid(uuid),
        );
        assert_eq!(uuid_token.len(), CURSOR_UUID_HEX);
        assert_eq!(
            decode_management_list_cursor(
                &SECRET,
                EndpointId::UsersRead,
                ACTOR,
                &filter,
                Some(&uuid_token)
            )
            .expect("uuid profile roundtrip"),
            Some(CursorKey::Uuid(uuid))
        );
        let public_key = fixture_device_key();
        let device_token = encode_signed_cursor(
            &SECRET,
            EndpointId::DevicesRead,
            ACTOR,
            &filter,
            CursorKey::DeviceKey(public_key),
        );
        assert_eq!(device_token.len(), CURSOR_DEVICE_HEX);
        assert_eq!(
            decode_management_list_cursor(
                &SECRET,
                EndpointId::DevicesRead,
                ACTOR,
                &filter,
                Some(&device_token)
            )
            .expect("device profile roundtrip"),
            Some(CursorKey::DeviceKey(public_key))
        );
        // 客户端侧负向：跨端点、跨用户、非法形状一律 400，无回退、不降级全量。
        for (endpoint, token) in [
            (EndpointId::RolesRead, uuid_token.as_str()),
            (EndpointId::UsersRead, device_token.as_str()),
            (EndpointId::DevicesRead, uuid_token.as_str()),
            (EndpointId::UsersRead, &uuid_token[..CURSOR_UUID_HEX - 1]),
        ] {
            let err = decode_management_list_cursor(&SECRET, endpoint, ACTOR, &filter, Some(token))
                .expect_err("客户端侧非法 cursor 必须拒绝");
            assert!(matches!(err, ControllerError::InvalidArgument));
            assert_eq!(map_error(err).status(), StatusCode::BAD_REQUEST);
        }
        let err = decode_management_list_cursor(
            &SECRET,
            EndpointId::UsersRead,
            [9u8; 16],
            &filter,
            Some(&uuid_token),
        )
        .expect_err("跨用户 cursor 必须拒绝");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }

    #[test]
    fn decode_management_list_cursor_rejects_key_kind_outside_profile() {
        // 用真实 MAC 造「UsersRead 端点码 + 设备 kind」的合法签名 token：codec 层能解出
        // DeviceKey，入口 helper 必须按 profile 互锁拒绝，不得放行跨 profile 的内部 key。
        let filter = CursorFilterCtx::empty();
        let endpoint = EndpointId::UsersRead;
        let endpoint_code = endpoint_cursor_code(endpoint);
        let mut payload: Vec<u8> = Vec::new();
        payload.extend_from_slice(&endpoint_code.to_be_bytes());
        payload.push(CURSOR_KIND_DEVICE);
        payload.push(0u8);
        payload.extend_from_slice(&fixture_device_key());
        payload.extend_from_slice(&[0u8; 4]);
        let mac = management_cursor_mac(
            &SECRET,
            endpoint_code,
            &ACTOR,
            &filter.canonical(),
            &payload,
        );
        let mut raw = payload;
        raw.extend_from_slice(&mac[..CURSOR_TAG_LEN]);
        let crossed = hex::encode(&raw);
        assert!(matches!(
            decode_signed_cursor(&SECRET, endpoint, ACTOR, &filter, &crossed)
                .expect("codec 层可解出 DeviceKey"),
            CursorKey::DeviceKey(_)
        ));
        let err = decode_management_list_cursor(&SECRET, endpoint, ACTOR, &filter, Some(&crossed))
            .expect_err("profile 不符必须拒绝");
        assert!(matches!(err, ControllerError::InvalidArgument));
        assert_eq!(map_error(err).status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn sign_management_list_page_signs_both_profiles_and_preserves_projection() {
        let filter = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&b"asc"[..]))])
            .expect("fixture filter");
        // UUID profile：内部 32 小写 hex（16B）→ 外部 64 小写 hex（24B payload + 8B tag）。
        let signed = sign_management_list_page(
            &SECRET,
            EndpointId::UsersRead,
            ACTOR,
            &filter,
            page(Some(UUID_RAW_HEX)),
        )
        .expect("uuid 页签名");
        let token = signed["next_cursor"]
            .as_str()
            .expect("签名 token")
            .to_owned();
        assert_eq!(token.len(), CURSOR_UUID_HEX);
        assert_eq!(
            decode_management_list_cursor(
                &SECRET,
                EndpointId::UsersRead,
                ACTOR,
                &filter,
                Some(&token)
            )
            .expect("出口 token 可用同一 filter 解回"),
            Some(CursorKey::Uuid(fixture_uuid()))
        );
        // 投影守恒：只替换 next_cursor，items 与其余字段原样保留。
        assert_eq!(signed["items"], page(Some(UUID_RAW_HEX))["items"]);
        assert_eq!(signed["total"], 2);
        assert_eq!(signed.as_object().expect("object").len(), 3);
        // 设备 profile：内部 64 小写 hex（32B）→ 外部 96 小写 hex（40B payload + 8B tag）。
        let signed = sign_management_list_page(
            &SECRET,
            EndpointId::DevicesRead,
            ACTOR,
            &filter,
            page(Some(DEVICE_RAW_HEX)),
        )
        .expect("device 页签名");
        let token = signed["next_cursor"]
            .as_str()
            .expect("签名 token")
            .to_owned();
        assert_eq!(token.len(), CURSOR_DEVICE_HEX);
        assert_eq!(
            decode_management_list_cursor(
                &SECRET,
                EndpointId::DevicesRead,
                ACTOR,
                &filter,
                Some(&token)
            )
            .expect("设备出口 token 解回"),
            Some(CursorKey::DeviceKey(fixture_device_key()))
        );
        // null 保持 null，且仍不触碰 MAC。
        let signed =
            sign_management_list_page(&SECRET, EndpointId::RolesRead, ACTOR, &filter, page(None))
                .expect("null 页");
        assert!(signed["next_cursor"].is_null());
        assert_eq!(signed["items"], page(None)["items"]);
    }

    #[test]
    fn sign_management_list_page_fails_closed_on_dirty_backend_pages() {
        let filter = CursorFilterCtx::empty();
        let cases = vec![
            json!("not-an-object"),
            json!({"next_cursor": null}),
            json!({"items": {}, "next_cursor": null}),
            json!({"items": []}),
            json!({"items": [], "next_cursor": 7}),
            json!({"items": [], "next_cursor": UUID_RAW_HEX.to_uppercase()}),
            json!({"items": [], "next_cursor": &UUID_RAW_HEX[..31]}),
            json!({"items": [], "next_cursor": format!("{UUID_RAW_HEX}0")}),
            json!({"items": [], "next_cursor": DEVICE_RAW_HEX}),
            json!({"items": [], "next_cursor": "000000000000000000000000000000ff"}),
            json!({"items": [], "next_cursor": "0000000000000000f0000000000000ff"}),
        ];
        for case in cases {
            let err =
                sign_management_list_page(&SECRET, EndpointId::UsersRead, ACTOR, &filter, case)
                    .expect_err("后端脏页必须失败关闭");
            assert!(matches!(
                &err,
                ControllerError::Config(m) if m == "management cursor page shape"
            ));
            assert_backend_fail_closed(err);
        }
        // 长度与 profile 互锁：UUID 的 32 hex 用在设备端点同样 503，而非跨 profile 误签。
        let err = sign_management_list_page(
            &SECRET,
            EndpointId::DevicesRead,
            ACTOR,
            &filter,
            page(Some(UUID_RAW_HEX)),
        )
        .expect_err("跨 profile 长度必须拒绝");
        assert_backend_fail_closed(err);
    }

    #[test]
    fn management_cursor_helpers_fail_closed_on_non_whitelisted_endpoints() {
        let filter = CursorFilterCtx::empty();
        // 入口：cursor 为 None 也必须先过端点白名单，不得短路放行。
        let err =
            decode_management_list_cursor(&SECRET, EndpointId::AuditRead, ACTOR, &filter, None)
                .expect_err("非白名单端点不得放行");
        assert_backend_fail_closed(err);
        // 出口：next_cursor 为 null 同样被第一行白名单拦截，不得回吐未校验的原始 page。
        // `AuditRead` 保持独立 MAC profile（audit cursor 自有 codec），本链路必须继续拒绝它。
        let err =
            sign_management_list_page(&SECRET, EndpointId::AuditRead, ACTOR, &filter, page(None))
                .expect_err("非白名单端点不得签名放行");
        assert_backend_fail_closed(err);
    }

    #[test]
    fn effective_permissions_read_is_device_profile_whitelisted_end_to_end() {
        // Task 13a 契约修订：`EffectivePermissionsRead` 是 Device profile 白名单端点，
        // 入口/出口共用同一 codec；filter 绑定 target 用户 + device_id 筛选。
        let filter = CursorFilterCtx::from_ordered_fields(&[
            (1u8, Some(&fixture_uuid()[..])),
            (2u8, Some(&fixture_device_key()[..])),
        ])
        .expect("fixture filter");
        let signed = sign_management_list_page(
            &SECRET,
            EndpointId::EffectivePermissionsRead,
            ACTOR,
            &filter,
            page(Some(DEVICE_RAW_HEX)),
        )
        .expect("有效权限页必须可签名");
        let token = signed["next_cursor"]
            .as_str()
            .expect("签名 token")
            .to_owned();
        assert_eq!(token.len(), CURSOR_DEVICE_HEX);
        assert_eq!(
            decode_management_list_cursor(
                &SECRET,
                EndpointId::EffectivePermissionsRead,
                ACTOR,
                &filter,
                Some(&token),
            )
            .expect("同一 filter 解回"),
            Some(CursorKey::DeviceKey(fixture_device_key()))
        );
        // 换筛选（去掉 device_id 一项）→ 同一 token 必须 400，不得跨 filter 复用。
        let changed = CursorFilterCtx::from_ordered_fields(&[(1u8, Some(&fixture_uuid()[..]))])
            .expect("changed filter");
        let err = decode_management_list_cursor(
            &SECRET,
            EndpointId::EffectivePermissionsRead,
            ACTOR,
            &changed,
            Some(&token),
        )
        .expect_err("换筛选必须拒绝");
        assert!(matches!(err, ControllerError::InvalidArgument));
        assert_eq!(map_error(err).status(), StatusCode::BAD_REQUEST);
    }
}

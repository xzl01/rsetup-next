//! 管理路由骨架（Identity Task 6）：共享解析辅助、统一错误映射器、统一签名
//! 分页 cursor，以及 8 个资源网关 trait 的类型前置。
//!
//! 边界（spec 02 §3、Identity 计划 Task 6）：
//! - 本模块只组合管理子 router，不组合 auth/tasks/status/assets 或探活，也不改写旧 `AppState`；
//!   顶层装配唯一归属集成 Task 0。
//! - 骨架阶段各资源子路由挂载 0 条路由：未实现的路由不注册，Task 7+ 逐资源接线。
//! - 所有管理分页端点的 `cursor`/`next_cursor` 一律经本模块的签名函数对签发与校验，
//!   废弃裸内部键直传；密钥只经第一调用参数传入，codec 内不读取任何环境变量。

pub mod approvals;
pub mod audit;
pub mod devices;
pub mod grants;
pub mod groups;
pub mod roles;
pub mod system;
pub mod users;

use std::sync::Arc;

use axum::http::StatusCode;
use axum::response::Response;
use serde_json::json;
use sha2::Digest as _;
use uuid::Uuid;

use crate::error::ControllerError;
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
/// 不提供默认常量源或 `Option` 缺省；不扩展 cursor 密钥字段。
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
}

/// 生产装配签名（Task 13 打通真实仓储 adapter）。骨架阶段 8 个子 router 各挂载 0 条
/// 路由，因此本函数返回的 router 不含任何可命中路由——类型前置，不计为 API 验收。
pub fn build_management_router(state: ManagementState) -> axum::Router<()> {
    users::users_router(UsersApiState {
        auth: state.auth.clone(),
        gateway: state.users,
    })
    .merge(roles::roles_router(RolesApiState {
        auth: state.auth.clone(),
        gateway: state.roles,
    }))
    .merge(grants::grants_router(GrantsApiState {
        auth: state.auth.clone(),
        gateway: state.grants,
    }))
    .merge(groups::groups_router(GroupsApiState {
        auth: state.auth.clone(),
        gateway: state.groups,
    }))
    .merge(devices::devices_router(DevicesApiState {
        auth: state.auth.clone(),
        gateway: state.devices,
        live_probe: state.device_probe.clone(),
    }))
    .merge(approvals::approvals_router(ApprovalsApiState {
        auth: state.auth.clone(),
        gateway: state.approvals,
        notification_sink: state.notification_sink,
        probe: state.device_probe,
    }))
    .merge(audit::audit_router(AuditApiState {
        auth: state.auth.clone(),
        gateway: state.audit,
    }))
    .merge(system::system_router(SystemApiState {
        auth: state.auth,
        gateway: state.system,
        recovery_probe: state.recovery_probe,
    }))
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

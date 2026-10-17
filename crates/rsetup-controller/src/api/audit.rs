//! audit 资源管理路由：网关契约与 1 条管理端点（Identity Task 12）。
//!
//! 契约（spec 02 §3/§4）：`GET /api/v1/audit` 先过 `http_gate` 会话门禁
//! （`(EndpointId::AuditRead, session.digest)` 读桶），再过 `require_admin_session`
//! 管理前置；query 严格 `deny_unknown_fields`：`actor_id` 仅规范小写 UUID、
//! `device_id` 恰 64 位小写 hex、`limit` 缺省 50 且 `1..=200`。
//!
//! 审计游标是 `AuditRead` 的**独立 MAC profile**（`api/mod.rs` 封闭白名单不含本
//! 端点）：内部键为 Task 3 定义的复合键 `(process_epoch, event_seq, id)`，出口
//! token 经 HMAC-SHA256 签名并绑定 `(endpoint, actor, filter)`，废弃裸复合键直传。
//! 密钥只在确需签验（请求带 `cursor`，或网关页含非 null raw key）时经
//! `management_cursor_secret()` 获取，未安装 → 固定去敏 `Config` → 503，绝不回吐
//! 未签名页。`items` 逐字透传网关脱敏投影（`params_redacted` 列），handler 不新增
//! 字段，禁止回显任何凭据、token 或密码哈希。

use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use axum::routing::get;
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::service::Session;
use crate::devices::repository::{AuditCursor, AuditFilter};
use crate::error::ControllerError;
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{authenticate_read, require_admin_session};

use super::{
    CursorFilterCtx, ManagementFuture, endpoint_cursor_code, management_cursor_mac,
    management_cursor_secret, map_error, parse_device_id_path, parse_uuid_path,
};

#[derive(Clone)]
pub struct AuditApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn AuditGateway>,
}

/// `AuditCursor` 是 Task 3 审计游标段定义的复合键（process_epoch/event_seq/id），
/// 不能仅 event_seq。
pub trait AuditGateway: Send + Sync {
    fn list<'a>(
        &'a self,
        actor: &'a Session,
        filter: AuditFilter,
        cursor: Option<AuditCursor>,
        limit: usize,
    ) -> ManagementFuture<'a, serde_json::Value>;
}

// ---------------------------------------------------------------------------
// AuditRead 独立 MAC profile 的签名游标
// ---------------------------------------------------------------------------

/// 审计游标独立 payload kind：与共享 codec 的 UUID=1/Device=2 不重叠。
const AUDIT_CURSOR_KIND: u8 = 3;
/// 签名 tag 长度与共享 codec 一致：MAC 前 8B。
const AUDIT_CURSOR_TAG_LEN: usize = 8;
/// 出口 token 字节账：payload `2+1+1+16+8+16 = 44B` + tag 8B = 52B → 104 hex。
const AUDIT_CURSOR_TOKEN_HEX: usize = 104;
/// 网关内部 raw key 字节账：epoch 16B ‖ event_seq 8B BE ‖ id 16B = 40B → 80 hex。
const AUDIT_RAW_KEY_HEX: usize = 80;
/// filter schema 的固定 tag 表：tag 1 = `actor_id` 16B、tag 2 = `device_id` 32B；
/// schema 恒含两字段（存在 → `Some(bytes)`、缺省 → `None`），`limit` 不进 schema。
const AUDIT_FILTER_TAG_ACTOR: u8 = 1;
const AUDIT_FILTER_TAG_DEVICE: u8 = 2;

fn audit_cursor_payload(endpoint_code: u16, cursor: &AuditCursor) -> Vec<u8> {
    let mut payload = Vec::with_capacity(44);
    payload.extend_from_slice(&endpoint_code.to_be_bytes());
    payload.push(AUDIT_CURSOR_KIND);
    payload.push(0u8);
    payload.extend_from_slice(cursor.process_epoch.as_bytes());
    payload.extend_from_slice(&cursor.event_seq.to_be_bytes());
    payload.extend_from_slice(cursor.id.as_bytes());
    payload
}

fn sign_audit_cursor(
    secret: &[u8; 32],
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    cursor: &AuditCursor,
) -> String {
    let endpoint_code = endpoint_cursor_code(EndpointId::AuditRead);
    let payload = audit_cursor_payload(endpoint_code, cursor);
    let mac = management_cursor_mac(
        secret,
        endpoint_code,
        &user_id,
        &filter.canonical(),
        &payload,
    );
    let mut raw = payload;
    raw.extend_from_slice(&mac[..AUDIT_CURSOR_TAG_LEN]);
    hex::encode(raw)
}

/// 固定长 xor 累积比较：不短路、不提前返回，不泄露首个差异位置。
fn audit_cursor_tag_eq(a: &[u8; AUDIT_CURSOR_TAG_LEN], b: &[u8; AUDIT_CURSOR_TAG_LEN]) -> bool {
    let mut diff = 0u8;
    for i in 0..AUDIT_CURSOR_TAG_LEN {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn is_lower_hex(token: &str) -> bool {
    !token.is_empty()
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_uuid_v4(bytes: &[u8; 16]) -> bool {
    bytes[6] >> 4 == 0x4 && bytes[8] & 0b1100_0000 == 0b1000_0000
}

/// 入口：HTTP `cursor` token → typed 复合键。客户端侧失败（长度、大写 hex、非 hex、
/// tag 不符、kind/endpoint_code/reserved 篡改、UUID 非 v4）一律同一个去敏
/// `InvalidArgument` → 400，无回退、不降级全量。
#[allow(clippy::result_large_err)]
fn decode_audit_cursor(
    secret: &[u8; 32],
    user_id: [u8; 16],
    filter: &CursorFilterCtx,
    token: &str,
) -> Result<AuditCursor, ControllerError> {
    if token.len() != AUDIT_CURSOR_TOKEN_HEX || !is_lower_hex(token) {
        return Err(ControllerError::InvalidArgument);
    }
    let raw = hex::decode(token).map_err(|_| ControllerError::InvalidArgument)?;
    let (payload, tag_bytes) = raw.split_at(raw.len() - AUDIT_CURSOR_TAG_LEN);
    let mut tag = [0u8; AUDIT_CURSOR_TAG_LEN];
    tag.copy_from_slice(tag_bytes);
    let endpoint_code = endpoint_cursor_code(EndpointId::AuditRead);
    let mac = management_cursor_mac(
        secret,
        endpoint_code,
        &user_id,
        &filter.canonical(),
        payload,
    );
    let mut expected = [0u8; AUDIT_CURSOR_TAG_LEN];
    expected.copy_from_slice(&mac[..AUDIT_CURSOR_TAG_LEN]);
    if !audit_cursor_tag_eq(&tag, &expected) {
        return Err(ControllerError::InvalidArgument);
    }
    // tag 通过后的字段防线：MAC 已覆盖 payload，此处闭合接受域。
    if payload.len() != 44
        || u16::from_be_bytes([payload[0], payload[1]]) != endpoint_code
        || payload[2] != AUDIT_CURSOR_KIND
        || payload[3] != 0u8
    {
        return Err(ControllerError::InvalidArgument);
    }
    let mut epoch = [0u8; 16];
    epoch.copy_from_slice(&payload[4..20]);
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&payload[20..28]);
    let mut id = [0u8; 16];
    id.copy_from_slice(&payload[28..44]);
    if !is_uuid_v4(&epoch) || !is_uuid_v4(&id) {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(AuditCursor {
        process_epoch: Uuid::from_bytes(epoch),
        event_seq: u64::from_be_bytes(seq),
        id: Uuid::from_bytes(id),
    })
}

fn audit_page_shape_error() -> ControllerError {
    ControllerError::Config("audit cursor page shape".into())
}

/// 网关内部 raw 80 hex（epoch ‖ seq BE ‖ id）→ typed 复合键；任何形状不符都是
/// 同一个去敏 `Config` → 503，不把裸 key 写进响应也不截断。
fn parse_audit_raw_key(raw: &str) -> Result<AuditCursor, ControllerError> {
    if raw.len() != AUDIT_RAW_KEY_HEX || !is_lower_hex(raw) {
        return Err(audit_page_shape_error());
    }
    let bytes = hex::decode(raw).map_err(|_| audit_page_shape_error())?;
    let mut epoch = [0u8; 16];
    epoch.copy_from_slice(&bytes[0..16]);
    let mut seq = [0u8; 8];
    seq.copy_from_slice(&bytes[16..24]);
    let mut id = [0u8; 16];
    id.copy_from_slice(&bytes[24..40]);
    if !is_uuid_v4(&epoch) || !is_uuid_v4(&id) {
        return Err(audit_page_shape_error());
    }
    Ok(AuditCursor {
        process_epoch: Uuid::from_bytes(epoch),
        event_seq: u64::from_be_bytes(seq),
        id: Uuid::from_bytes(id),
    })
}

// ---------------------------------------------------------------------------
// 严格 query DTO 与 handler
// ---------------------------------------------------------------------------

/// `GET /api/v1/audit` 的严格 query DTO：`deny_unknown_fields` 拒绝任何未知参数；
/// `Query::try_from_uri` 由 handler 在门禁之后手动调用，不让 Axum extractor 在鉴权
/// 前短路 400。
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditListQuery {
    pub(crate) actor_id: Option<String>,
    pub(crate) device_id: Option<String>,
    pub(crate) cursor: Option<String>,
    pub(crate) limit: Option<usize>,
}

/// `GET /api/v1/audit` 的 inner：`secret_provider` 为唯一密钥入口（生产 wrapper 恒
/// 只经 `management_cursor_secret()`，测试 wrapper 从局部 `OnceLock` 槽读取）。
/// 调用顺序固定：`authenticate_read(AuditRead)` → `require_admin_session`（401/403
/// 先于 query 解析）→ query/limit/actor_id/device_id 校验 → filter ctx 构造 →
/// 按需取密钥并解码 cursor → 网关单页 → 形状校验 → 按需取密钥并签名出口。
/// 与 approvals 的差异：审计游标是独立 MAC profile，密钥只在确需签验时获取
/// （`cursor` 存在，或网关页含非 null raw key），纯 null 末页不依赖密钥。
async fn list_audit_with_secret_provider<F>(
    state: &AuditApiState,
    headers: &HeaderMap,
    uri: &Uri,
    secret_provider: F,
) -> Response
where
    F: Fn() -> Result<[u8; 32], ControllerError>,
{
    let actor = match authenticate_read(&state.auth, headers, EndpointId::AuditRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let query = match Query::<AuditListQuery>::try_from_uri(uri) {
        Ok(query) => query.0,
        Err(_) => return map_error(ControllerError::InvalidArgument),
    };
    let limit = match query.limit {
        None => 50usize,
        Some(value) if (1..=200).contains(&value) => value,
        Some(_) => return map_error(ControllerError::InvalidArgument),
    };
    let actor_id = match query.actor_id.as_deref() {
        None => None,
        Some(raw) => match parse_uuid_path(raw) {
            Ok(uuid) => Some(*uuid.as_bytes()),
            Err(response) => return response,
        },
    };
    let device_id = match query.device_id.as_deref() {
        None => None,
        Some(raw) => match parse_device_id_path(raw) {
            Ok(id) => Some(id),
            Err(response) => return response,
        },
    };
    let filter = match CursorFilterCtx::from_ordered_fields(&[
        (
            AUDIT_FILTER_TAG_ACTOR,
            actor_id.as_ref().map(|id| id.as_slice()),
        ),
        (
            AUDIT_FILTER_TAG_DEVICE,
            device_id.as_ref().map(|id| id.as_slice()),
        ),
    ]) {
        Ok(filter) => filter,
        Err(error) => return map_error(error),
    };
    let mut cursor = None;
    if let Some(token) = query.cursor.as_deref() {
        let secret = match secret_provider() {
            Ok(secret) => secret,
            Err(error) => return map_error(error),
        };
        cursor = match decode_audit_cursor(&secret, actor.user.id, &filter, token) {
            Ok(cursor) => Some(cursor),
            Err(error) => return map_error(error),
        };
    }
    let page = match state
        .gateway
        .list(
            &actor,
            AuditFilter {
                actor_id,
                device_id,
            },
            cursor,
            limit,
        )
        .await
    {
        Ok(page) => page,
        Err(error) => return map_error(error),
    };
    // 投影守恒：只替换 `next_cursor` 一个字段，`items` 与其余字段原样保留；
    // 裸 raw 复合键绝不直传出口，签名失败（含密钥未安装）整页 503。
    let mut map = match page {
        serde_json::Value::Object(map) => map,
        _ => return map_error(audit_page_shape_error()),
    };
    if !matches!(map.get("items"), Some(serde_json::Value::Array(_))) {
        return map_error(audit_page_shape_error());
    }
    let raw_next = match map.get("next_cursor") {
        Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(raw)) => Some(raw.clone()),
        _ => return map_error(audit_page_shape_error()),
    };
    let next_cursor = match raw_next {
        None => serde_json::Value::Null,
        Some(raw) => {
            let secret = match secret_provider() {
                Ok(secret) => secret,
                Err(error) => return map_error(error),
            };
            let next = match parse_audit_raw_key(&raw) {
                Ok(next) => next,
                Err(error) => return map_error(error),
            };
            serde_json::Value::String(sign_audit_cursor(&secret, actor.user.id, &filter, &next))
        }
    };
    map.insert("next_cursor".to_string(), next_cursor);
    ok_response(serde_json::Value::Object(map))
}

async fn list_audit(State(state): State<AuditApiState>, headers: HeaderMap, uri: Uri) -> Response {
    list_audit_with_secret_provider(&state, &headers, &uri, || {
        management_cursor_secret().copied()
    })
    .await
}

/// 挂载 audit 的 1 条管理路由：`GET /api/v1/audit` 唯一注册锚点，无第二处注册。
pub fn audit_router(state: AuditApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/audit", get(list_audit))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::test_fixture::*;
    use axum::http::StatusCode;
    use tower::ServiceExt;

    /// canonical v4 样本（version nibble=4、variant=10xx）。
    const EPOCH_V4: &str = "067e6162ea944431836c7d8fa2010b3c";
    const ID_V4: &str = "067e6162ea944431836c7d8fa2010b3d";
    /// 网关内部 raw 80 hex：epoch 16B ‖ event_seq=7 (8B BE) ‖ id 16B。
    const RAW_NEXT: &str =
        "067e6162ea944431836c7d8fa2010b3c0000000000000007067e6162ea944431836c7d8fa2010b3d";
    /// 与 users/roles/grants/groups/devices/approvals 区同值的测试合成 key。
    const SYNTHETIC_SECRET: [u8; 32] = [7u8; 32];

    fn audit_page(next_cursor: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "items": [{
                "id": "067e6162-ea94-4431-836c-7d8fa2010b3c",
                "actor_kind": "admin",
                "actor_user_id": "067e6162-ea94-4431-836c-7d8fa2010b3c",
                "event_type": "user.create",
                "target_kind": "user",
                "target_id": null,
                "params_redacted": "{}",
                "outcome": "success",
                "process_epoch": "067e6162-ea94-4431-836c-7d8fa2010b3c",
                "event_seq": "1",
            }],
            "next_cursor": next_cursor,
        })
    }

    /// 私有 audit 网关桩：固定脱敏页，`next_cursor` 由用例指定。
    struct FixtureAuditGateway {
        next_cursor: serde_json::Value,
    }

    impl AuditGateway for FixtureAuditGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            _filter: AuditFilter,
            _cursor: Option<AuditCursor>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let page = audit_page(self.next_cursor.clone());
            Box::pin(async { Ok(page) })
        }
    }

    fn fixture_audit_state() -> AuditApiState {
        AuditApiState {
            auth: fixture_auth(fixture_session(true)),
            gateway: Arc::new(FixtureAuditGateway {
                next_cursor: serde_json::Value::Null,
            }),
        }
    }

    async fn error_code(res: axum::response::Response) -> (StatusCode, String) {
        let status = res.status();
        let value = body_json(res).await;
        (
            status,
            value["error"]["code"].as_str().unwrap_or("").to_string(),
        )
    }

    #[tokio::test]
    async fn audit_endpoint_returns_redacted_events_never_tokens() {
        let router = audit_router(fixture_audit_state());
        let res = router
            .oneshot(authenticated_get("/api/v1/audit"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        let text = val.to_string();
        assert!(!text.contains("password_hash"));
        assert!(!text.contains("token_hash"));
        assert_eq!(val["data"]["items"][0]["params_redacted"], "{}");
        assert!(val["data"]["next_cursor"].is_null());
    }

    #[tokio::test]
    async fn audit_rejects_non_admin_with_403() {
        let router = audit_router(AuditApiState {
            auth: fixture_auth(fixture_session(false)),
            gateway: Arc::new(FixtureAuditGateway {
                next_cursor: serde_json::Value::Null,
            }),
        });
        let (status, code) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/audit"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn audit_rejects_unknown_and_malformed_query_with_400() {
        let cases = [
            "/api/v1/audit?unknown=1",
            "/api/v1/audit?limit=0",
            "/api/v1/audit?limit=201",
            "/api/v1/audit?limit=abc",
            "/api/v1/audit?actor_id=067E6162-ea94-4431-836c-7d8fa2010b3c",
            "/api/v1/audit?actor_id=not-a-uuid",
            "/api/v1/audit?device_id=abc",
            "/api/v1/audit?device_id=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
        ];
        for uri in cases {
            let router = audit_router(fixture_audit_state());
            let (status, code) =
                error_code(router.oneshot(authenticated_get(uri)).await.unwrap()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} 必须 400");
            assert_eq!(code, "INVALID_ARGUMENT", "{uri}");
        }
    }

    #[tokio::test]
    async fn audit_list_without_installed_cursor_secret_returns_503() {
        // 生产 router 恒只经全局 `management_cursor_secret()`；单元测试进程内全局
        // OnceLock 未安装，网关返回非 null raw key 时必须 503 且不回吐裸 key。
        let router = audit_router(AuditApiState {
            auth: fixture_auth(fixture_session(true)),
            gateway: Arc::new(FixtureAuditGateway {
                next_cursor: serde_json::json!(RAW_NEXT),
            }),
        });
        let (status, code) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/audit"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "STORAGE_UNAVAILABLE");
    }

    // -----------------------------------------------------------------------
    // GET list 签名 cursor 真实接线（test-only wrapper + 局部 OnceLock 密钥槽）
    // -----------------------------------------------------------------------
    //
    // 红线与 approvals 区一致：不 set 全局 OnceLock、不给 `AuditApiState` 扩 secret
    // 字段、不用假 handler 或自签 token 绕过真实链路。

    /// 记录每次 `list` 收到的 typed cursor/limit 的页面桩：`cursor = None` 返回带
    /// raw `next_cursor` 的第一页，否则返回末页。
    #[derive(Default)]
    struct PageAuditGateway {
        calls: std::sync::Mutex<Vec<(Option<AuditCursor>, usize)>>,
    }

    impl PageAuditGateway {
        fn recorded(&self) -> Vec<(Option<AuditCursor>, usize)> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl AuditGateway for PageAuditGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            _filter: AuditFilter,
            cursor: Option<AuditCursor>,
            limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            self.calls.lock().unwrap().push((cursor, limit));
            let page = audit_page(if cursor.is_none() {
                serde_json::json!(RAW_NEXT)
            } else {
                serde_json::Value::Null
            });
            Box::pin(async move { Ok(page) })
        }
    }

    #[derive(Clone)]
    struct SignedAuditCursorTestState {
        audit: AuditApiState,
        secret_slot: Arc<std::sync::OnceLock<[u8; 32]>>,
    }

    /// test-only wrapper handler：内部调用同一个 `list_audit_with_secret_provider`，
    /// 真实 gate/query/codec/gateway 顺序不绕过，provider 经 `cursor_secret_of` 从本
    /// 测试局部槽读取。
    async fn list_audit_with_test_secret(
        State(state): State<SignedAuditCursorTestState>,
        headers: HeaderMap,
        uri: Uri,
    ) -> Response {
        let slot = state.secret_slot.clone();
        list_audit_with_secret_provider(&state.audit, &headers, &uri, move || {
            crate::api::cursor_secret_of(&slot).copied()
        })
        .await
    }

    /// fixture helper：只挂 wrapper 的 GET list 路由并 `.with_state(state)` 归约；
    /// `Some` 经 `install_cursor_secret_into` 装入测试合成 key。
    fn signed_audit_cursor_router(
        secret: Option<[u8; 32]>,
        gateway: Arc<PageAuditGateway>,
    ) -> Router<()> {
        let secret_slot = Arc::new(std::sync::OnceLock::new());
        if let Some(secret) = secret {
            crate::api::install_cursor_secret_into(&secret_slot, &secret)
                .expect("synthetic key 安装必须成功");
        }
        let state = SignedAuditCursorTestState {
            audit: AuditApiState {
                auth: fixture_auth(fixture_session(true)),
                gateway,
            },
            secret_slot,
        };
        Router::new()
            .route("/api/v1/audit", get(list_audit_with_test_secret))
            .with_state(state)
    }

    #[tokio::test]
    async fn list_signs_104_hex_token_and_decodes_composite_cursor() {
        let gateway = Arc::new(PageAuditGateway::default());
        let router = signed_audit_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/audit?limit=7"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        assert_eq!(
            token.len(),
            AUDIT_CURSOR_TOKEN_HEX,
            "复合键签名 token 恒 104 hex"
        );
        assert!(is_lower_hex(&token));
        assert_ne!(token, RAW_NEXT, "裸 raw 复合键不得直传出口");
        // 第二页：同一筛选（无参数）+ 换 limit=9——limit 不进 schema，仍须 200。
        let router = signed_audit_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get(&format!(
                "/api/v1/audit?cursor={token}&limit=9"
            )))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert!(value["data"]["next_cursor"].is_null());
        let calls = gateway.recorded();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (None, 7));
        let expected = AuditCursor {
            process_epoch: Uuid::parse_str(EPOCH_V4).expect("fixture epoch"),
            event_seq: 7,
            id: Uuid::parse_str(ID_V4).expect("fixture id"),
        };
        assert_eq!(
            calls[1],
            (Some(expected), 9),
            "网关必须收到 token 解回的 typed 复合键"
        );
    }

    #[tokio::test]
    async fn cursor_is_bound_to_filters_and_rejects_tampering() {
        let gateway = Arc::new(PageAuditGateway::default());
        let router = signed_audit_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let res = router
            .oneshot(authenticated_get("/api/v1/audit"))
            .await
            .unwrap();
        let value = body_json(res).await;
        let token = value["data"]["next_cursor"]
            .as_str()
            .expect("signed token")
            .to_string();
        // 换筛选（追加 actor_id）后旧 token 必须 400：filter 绑定进 MAC 输入。
        let router = signed_audit_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let (status, code) = error_code(
            router
                .oneshot(authenticated_get(&format!(
                    "/api/v1/audit?cursor={token}&actor_id=067e6162-ea94-4431-836c-7d8fa2010b3c"
                )))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        // 篡改 token 末位 hex：tag 校验失败，同一个去敏 400。
        let mut tampered = token.clone();
        tampered.replace_range(
            tampered.len() - 1..,
            if token.ends_with('0') { "1" } else { "0" },
        );
        let router = signed_audit_cursor_router(Some(SYNTHETIC_SECRET), gateway.clone());
        let (status, code) = error_code(
            router
                .oneshot(authenticated_get(&format!(
                    "/api/v1/audit?cursor={tampered}"
                )))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(code, "INVALID_ARGUMENT");
        // 跨用户（不同 actor 的 MAC 输入）同样 400：以另一 fixture actor 上下文重算必失败。
        let other_actor = [8u8; 16];
        let filter = CursorFilterCtx::empty();
        let err = decode_audit_cursor(&SYNTHETIC_SECRET, other_actor, &filter, &token)
            .expect_err("跨用户 cursor 必须拒绝");
        assert!(matches!(err, ControllerError::InvalidArgument));
    }

    #[tokio::test]
    async fn dirty_gateway_pages_fail_closed_with_503() {
        let cases = [
            serde_json::json!({"next_cursor": null}),
            serde_json::json!({"items": {}, "next_cursor": null}),
            serde_json::json!({"items": []}),
            serde_json::json!({"items": [], "next_cursor": 7}),
            // raw key 形状脏：大写 hex、长度不符、epoch 非 v4（version nibble 改 5）。
            serde_json::json!({"items": [], "next_cursor": RAW_NEXT.to_uppercase()}),
            serde_json::json!({"items": [], "next_cursor": &RAW_NEXT[..79]}),
            serde_json::json!({"items": [], "next_cursor": format!("{RAW_NEXT}0")}),
            serde_json::json!({
                "items": [],
                "next_cursor": format!("{}{}{}", &RAW_NEXT[..12], "5", &RAW_NEXT[13..]),
            }),
        ];
        for page in cases {
            let router = audit_router(AuditApiState {
                auth: fixture_auth(fixture_session(true)),
                gateway: Arc::new(DirtyPageGateway(page)),
            });
            let (status, code) = error_code(
                router
                    .oneshot(authenticated_get("/api/v1/audit"))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(code, "STORAGE_UNAVAILABLE");
        }
    }

    struct DirtyPageGateway(serde_json::Value);

    impl AuditGateway for DirtyPageGateway {
        fn list<'a>(
            &'a self,
            _actor: &'a Session,
            _filter: AuditFilter,
            _cursor: Option<AuditCursor>,
            _limit: usize,
        ) -> ManagementFuture<'a, serde_json::Value> {
            let page = self.0.clone();
            Box::pin(async move { Ok(page) })
        }
    }
}

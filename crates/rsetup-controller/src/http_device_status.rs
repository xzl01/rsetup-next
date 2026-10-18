//! 设备状态读组件（集成验收 Task 0b；《04 board-client-and-http》第 1305–1345 行）。
//!
//! 本模块只提供**独立组件子 router** [`build_device_status_router`]：
//! - 不计入 Runtime 的 8 条任务路由，也不混入 Identity 的 26 条管理路由；
//! - 路径集合只有 `GET /api/v1/devices/{id}/status` 一条，不注册任何写路由、
//!   SSE 或 `/system/time`；
//! - 顶层生产装配唯一归属 `crate::production::build_production_router`。
//!
//! 数据来源契约（02 §1）：handler **只读当前进程真实缓存**的设备状态快照，
//! GET 绝不触发执行/采集。缓存无样本时状态源返回 `Ok(None)`，响应投影为
//! `snapshot=null`、`freshness="unknown"`、`age_ms=null`，**不得**以常量伪 PASS。
//! [`EmptyDeviceStatusSnapshotSource`] 是「缓存中确实没有任何样本」这一真实
//! 状态的实现（transport 线 typed 缓存适配器接入前的唯一合法语义：恒无样本），
//! 它不伪造任何快照、时间证据或新鲜度。
//!
//! handler 固定顺序与 `src/http_auth.rs`、`src/tasks/http.rs` 完全一致，
//! **不自写** Host/Cookie/CSRF：
//! ①`authenticate_read(EndpointId::DeviceStatusRead)`（Host → 严格单值会话
//! Cookie → 活会话预检 → 权威 authenticate → 读限速 50/s）
//! → ②[`require_business_session`]（临时密码一律 403 `PASSWORD_CHANGE_REQUIRED`，
//! admin 同样，不要求 `is_admin`）→ ③路径 ID 严格解析 → ④读真实缓存快照
//! → ⑤统一错误映射。不新增业务 permission。

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRef, Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ControllerError;
use crate::api::{map_error, parse_device_id_path};
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::authenticate_read;
use crate::model::decimal_u64_json;
use crate::tasks::http::require_business_session;
use crate::time::evidence::TimeEvidence;

/// 设备状态读路由的显式状态：共享认证上下文 + 真实缓存状态源。
///
/// `auth` 必须与 auth/management/tasks 三线来自**同一个** `Arc<HttpAuthState>`
/// （共享 session/CSRF/限流上下文），由 `build_production_router` 以
/// `Arc::ptr_eq` 强制校验。
#[derive(Clone)]
pub struct DeviceStatusState {
    pub auth: Arc<HttpAuthState>,
    pub snapshot: Arc<dyn DeviceStatusSnapshotSource>,
}

impl FromRef<DeviceStatusState> for Arc<HttpAuthState> {
    fn from_ref(state: &DeviceStatusState) -> Self {
        Arc::clone(&state.auth)
    }
}

/// dyn-safe 异步返回：与 `ManagementFuture`/`TaskFuture` 同一 BoxFuture 形态，
/// 不用 `async fn` in trait 再转 `dyn`。
pub type DeviceStatusFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 状态源：读**当前进程真实缓存**的设备状态快照。
///
/// GET 不得触发执行/采集；缓存无样本返回 `Ok(None)`（响应 `snapshot=null`、
/// `freshness="unknown"`、`age_ms=null`），**不得**常量伪 PASS。读取失败必须
/// `Err` 上抛，由统一映射转 503，绝不降级为 `unknown` 200。
pub trait DeviceStatusSnapshotSource: Send + Sync {
    fn snapshot<'a>(
        &'a self,
        device_id: &'a [u8; 32],
    ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>>;
}

/// 缓存条目：真实 typed 快照 + 接收时刻证据 + 新鲜度判定 + 去敏错误码。
pub struct DeviceStatusCacheEntry {
    pub snapshot: rsetup_protocol::wire::DeviceStatus,
    pub received_time: TimeEvidence,
    pub freshness: DeviceStatusFreshness,
    /// 固定去敏稳定码，无原始 driver 日志。
    pub last_error: Option<String>,
}

/// 新鲜度四态（02 §1）：`unknown`/`fresh`/`stale`/`error`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceStatusFreshness {
    Unknown,
    Fresh,
    Stale,
    Error,
}

/// 恒无样本状态源：真实反映「本进程缓存中没有任何设备状态样本」。
///
/// 这是 transport 线 typed 缓存适配器交付前唯一不伪造数据的合法实现——它对任何
/// 设备都返回 `Ok(None)`，因此响应恒为 `snapshot=null`、`freshness="unknown"`、
/// `age_ms=null`；不返回常量快照、不返回常量 `fresh`、也不返回错误。
#[derive(Clone, Copy, Debug, Default)]
pub struct EmptyDeviceStatusSnapshotSource;

impl DeviceStatusSnapshotSource for EmptyDeviceStatusSnapshotSource {
    fn snapshot<'a>(
        &'a self,
        _device_id: &'a [u8; 32],
    ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>> {
        Box::pin(async { Ok(None) })
    }
}

/// 设备状态读子路由：恰 1 条端点。
pub fn build_device_status_router(state: DeviceStatusState) -> Router<()> {
    Router::new()
        .route("/api/v1/devices/{id}/status", get(get_device_status))
        .with_state(state)
}

/// `GET /api/v1/devices/{id}/status`：见模块头的固定顺序。
async fn get_device_status(
    State(state): State<DeviceStatusState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match authenticate_read(&state.auth, &headers, EndpointId::DeviceStatusRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_business_session(&actor) {
        return response;
    }
    let id = match parse_device_id_path(&id) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let entry = match state.snapshot.snapshot(&id).await {
        Ok(entry) => entry,
        Err(error) => return map_error(error),
    };
    ok_response(device_status_data(entry.as_ref(), Utc::now()))
}

/// 02 §1 响应投影：`{snapshot, received_time, freshness, age_ms}`，
/// `error` 且保留最后好样本时另带 `last_error`。
///
/// `age_ms` 是无符号 u64 的**十进制字符串**（与 `sample_age_ms`、`revision`
/// 同一仓库口径）；无样本恒 `null`。`now` 由调用方注入，纯函数可测。
fn device_status_data(entry: Option<&DeviceStatusCacheEntry>, now: DateTime<Utc>) -> Value {
    let Some(entry) = entry else {
        return json!({
            "snapshot": Value::Null,
            "received_time": Value::Null,
            "freshness": DeviceStatusFreshness::Unknown,
            "age_ms": Value::Null,
        });
    };
    let mut data = json!({
        "snapshot": device_status_json(&entry.snapshot),
        "received_time": entry.received_time,
        "freshness": entry.freshness,
        // error 保留最后好样本时同样输出其真实 age，绝不因错误态清零。
        "age_ms": decimal_u64_json(age_ms(&entry.received_time, now)),
    });
    if let Some(last_error) = &entry.last_error {
        data["last_error"] = json!(last_error);
    }
    data
}

/// 样本接收时刻到 `now` 的整毫秒差；负偏（时钟回拨）按 0 收口，不产生负数。
fn age_ms(received_time: &TimeEvidence, now: DateTime<Utc>) -> u64 {
    u64::try_from(
        now.signed_duration_since(received_time.system_wall_utc)
            .num_milliseconds(),
    )
    .unwrap_or(0)
}

/// `rsetup_protocol::wire::DeviceStatus` 的 02 语义投影。
///
/// wire DTO 本身不含 serde 派生（协议层不预设 JSON 字节格式），因此这里显式
/// 逐字段投影；`u64` 一律十进制字符串，避免超过 2^53 的 `sample_seq` /
/// `sampled_monotonic_ns` 在 JS 侧失真。本函数不新增、不推断任何 wire 字段。
fn device_status_json(status: &rsetup_protocol::wire::DeviceStatus) -> Value {
    json!({
        "schema_version": status.schema_version,
        "boot_id": status.boot_id,
        "agent_epoch": status.agent_epoch,
        "sample_seq": decimal_u64_json(status.sample_seq),
        "sampled_unix_ms": status.sampled_unix_ms,
        "sampled_monotonic_ns": decimal_u64_json(status.sampled_monotonic_ns),
        "clock_epoch": status.clock_epoch,
        "metrics": status
            .metrics
            .iter()
            .map(|metric| json!({
                "key": metric.key,
                "value": metric.value,
                "unit": metric.unit,
                "quality": metric.quality,
            }))
            .collect::<Vec<Value>>(),
        "clock_quality": status.clock_quality,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::service::{Login, Session, SessionPage};
    use crate::http_auth::{AuthFuture, AuthServiceApi};
    use crate::http_security::HttpSecurityConfig;
    use crate::time::evidence::TimeQuality;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use chrono::TimeZone;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::ServiceExt;
    use uuid::Uuid;

    const HOST: &str = "controller.test";
    /// 合法设备 ID 形状（64 位小写 hex），只在门禁之后才会被解析。
    const DEVICE_PATH: &str =
        "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001/status";

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 18, 12, 0, 0).unwrap()
    }

    /// 只为把 router 立起来的最小认证桩：`is_live_session` 恒 false，因此
    /// `authenticate_read` 在触达任何仓储或状态源之前严格 401。本模块只断言
    /// 门禁顺序与响应投影，不认证任何真实会话。
    struct NoSessionAuth;

    impl AuthServiceApi for NoSessionAuth {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, _raw: &'a str) -> AuthFuture<'a, Session> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn change_password<'a>(
            &'a self,
            _s: &'a Session,
            _c: &'a str,
            _n: &'a str,
        ) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn is_live_session(&self, _digest: &[u8; 32]) -> bool {
            false
        }
        fn list_sessions<'a>(
            &'a self,
            _s: &'a Session,
            _c: Option<&'a str>,
            _l: usize,
        ) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
    }

    /// 记录被调用次数的状态源：用于证明 GET 在门禁通过前绝不读缓存。
    struct CountingSource {
        calls: Arc<AtomicUsize>,
    }

    impl DeviceStatusSnapshotSource for CountingSource {
        fn snapshot<'a>(
            &'a self,
            _device_id: &'a [u8; 32],
        ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(None) })
        }
    }

    fn test_state(snapshot: Arc<dyn DeviceStatusSnapshotSource>) -> Router<()> {
        build_device_status_router(DeviceStatusState {
            auth: Arc::new(HttpAuthState::new(
                Arc::new(NoSessionAuth),
                HttpSecurityConfig {
                    allowed_hosts: vec![HOST.into()],
                    allowed_origin: Some("http://controller.test".into()),
                    trust_tls: false,
                },
            )),
            snapshot,
        })
    }

    fn evidence_ago(millis: i64) -> TimeEvidence {
        let at = now() - chrono::Duration::milliseconds(millis);
        TimeEvidence {
            system_wall_utc: at,
            reference_utc: at,
            quality: TimeQuality::SystemFallback,
            clock_epoch: Uuid::new_v4(),
            source: None,
            sample_age_ms: None,
            offset_ms: None,
            uncertainty_ms: None,
        }
    }

    fn status_sample() -> rsetup_protocol::wire::DeviceStatus {
        rsetup_protocol::wire::DeviceStatus {
            schema_version: 1,
            boot_id: "boot-1".into(),
            agent_epoch: "epoch-1".into(),
            // 2^53 + 1：JSON number 会失真，必须投影为十进制字符串。
            sample_seq: 9_007_199_254_740_993,
            sampled_unix_ms: 1_760_000_000_000,
            sampled_monotonic_ns: 42,
            clock_epoch: "clock-1".into(),
            metrics: vec![rsetup_protocol::wire::Metric {
                key: "cpu.load1".into(),
                value: Some(0.5),
                unit: "ratio".into(),
                quality: "fresh".into(),
            }],
            clock_quality: "ntp_valid".into(),
        }
    }

    fn entry(freshness: DeviceStatusFreshness, last_error: Option<&str>) -> DeviceStatusCacheEntry {
        DeviceStatusCacheEntry {
            snapshot: status_sample(),
            received_time: evidence_ago(60_000),
            freshness,
            last_error: last_error.map(str::to_string),
        }
    }

    async fn get(router: Router<()>, uri: &str) -> (StatusCode, Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .header("host", HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 65_536).await.unwrap();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    #[tokio::test]
    async fn empty_source_reports_no_sample_without_fabricating_snapshot() {
        let source = EmptyDeviceStatusSnapshotSource;
        assert!(
            source.snapshot(&[7u8; 32]).await.unwrap().is_none(),
            "空状态源必须 Ok(None)，不得伪造样本"
        );
        let data = device_status_data(None, now());
        assert!(data["snapshot"].is_null());
        assert!(data["received_time"].is_null());
        assert_eq!(data["freshness"], json!("unknown"));
        assert!(data["age_ms"].is_null());
        assert!(data.get("last_error").is_none());
    }

    #[test]
    fn sample_projection_keeps_wire_semantics_and_decimal_u64_strings() {
        let mut entry = entry(DeviceStatusFreshness::Fresh, None);
        entry.received_time = evidence_ago(1_500);
        let data = device_status_data(Some(&entry), now());
        assert_eq!(data["snapshot"]["schema_version"], json!(1));
        assert_eq!(data["snapshot"]["boot_id"], json!("boot-1"));
        assert_eq!(data["snapshot"]["agent_epoch"], json!("epoch-1"));
        assert_eq!(
            data["snapshot"]["sample_seq"],
            json!("9007199254740993"),
            "u64 必须十进制字符串，避免 JS 侧精度失真"
        );
        assert_eq!(data["snapshot"]["sampled_monotonic_ns"], json!("42"));
        assert_eq!(
            data["snapshot"]["sampled_unix_ms"],
            json!(1_760_000_000_000i64)
        );
        assert_eq!(data["snapshot"]["clock_quality"], json!("ntp_valid"));
        assert_eq!(data["snapshot"]["metrics"][0]["key"], json!("cpu.load1"));
        assert_eq!(data["snapshot"]["metrics"][0]["value"], json!(0.5));
        assert_eq!(data["freshness"], json!("fresh"));
        assert_eq!(data["age_ms"], json!("1500"));
        assert!(data.get("last_error").is_none());
    }

    #[test]
    fn error_freshness_keeps_last_good_sample_age_and_reports_last_error() {
        let data = device_status_data(
            Some(&entry(
                DeviceStatusFreshness::Error,
                Some("TRANSPORT_UNAVAILABLE"),
            )),
            now(),
        );
        assert_eq!(data["freshness"], json!("error"));
        assert_eq!(
            data["age_ms"],
            json!("60000"),
            "错误态不得把好样本 age 清零"
        );
        assert_eq!(data["last_error"], json!("TRANSPORT_UNAVAILABLE"));
        assert!(!data["snapshot"].is_null(), "错误态保留最后好样本");
    }

    #[test]
    fn stale_freshness_is_the_fourth_wire_value() {
        let data = device_status_data(Some(&entry(DeviceStatusFreshness::Stale, None)), now());
        assert_eq!(data["freshness"], json!("stale"));
        assert!(data.get("last_error").is_none());
    }

    #[test]
    fn age_never_negative_on_clock_skew() {
        assert_eq!(age_ms(&evidence_ago(-5_000), now()), 0);
    }

    #[tokio::test]
    async fn route_is_registered_and_read_gate_runs_before_any_snapshot_read() {
        let calls = Arc::new(AtomicUsize::new(0));
        let router = test_state(Arc::new(CountingSource {
            calls: calls.clone(),
        }));
        let (status, body) = get(router, DEVICE_PATH).await;
        // 未认证必须严格 401（路由已注册，否则 404），且状态源一次都没被读。
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
        assert_eq!(calls.load(Ordering::SeqCst), 0, "GET 不得在门禁前触达缓存");
    }

    #[tokio::test]
    async fn component_registers_only_the_status_route() {
        let router = test_state(Arc::new(EmptyDeviceStatusSnapshotSource));
        for path in [
            "/api/v1/devices",
            "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001",
            "/api/v1/tasks",
            "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001/status/retry",
        ] {
            let (status, _) = get(router.clone(), path).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "path {path} 不应由本组件注册"
            );
        }
    }

    #[test]
    fn snapshot_source_error_maps_to_503_never_unknown_200() {
        // 状态源 Err 走统一映射：503，绝不降级成 freshness=unknown 的 200。
        let response = map_error(ControllerError::Config("device status cache read".into()));
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

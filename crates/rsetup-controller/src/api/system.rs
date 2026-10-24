//! system 资源管理路由：网关/恢复模式探针契约与 2 条管理端点（Identity Task 12）。
//!
//! `GET /api/v1/system/time`（spec 02 §7）：无请求体，管理员专属（先过
//! `authenticate_read(SystemStatusRead)` 再 `require_admin_session`），响应 `data`
//! 固定为 `{ evidence: TimeEvidence, retry_active: bool }`——证据来自装配注入的
//! `TimeGateHandle::current()`（读取侧动态派生 age/stale 降级），`retry_active` 是
//! 后台 NTP 退避重试是否激活（05 的「NTP 状态/下次重试」的机器可读形态）。
//! 未装配时间源时返回当前系统墙钟的 `SystemFallback` 证据、`retry_active: false`，
//! 不伪造 NTP 有效；响应不含任何凭据。
//!
//! 契约（spec 02 §3/§4）：`GET /api/v1/system/status` 先过 `http_gate` 会话门禁
//! （`(EndpointId::SystemStatusRead, session.digest)` 读桶），再过
//! `require_admin_session` 管理前置。响应 `data` 形状固定为
//! `{ ready, schema_version, version, recovery_mode, counts: { users, devices,
//! roles, grants, groups } }`：`schema_version` 与五个 `COUNT(*)` 来自网关真实聚合
//! （`schema_meta` 单例 + 计数，十进制字符串），`version` 来自 `CARGO_PKG_VERSION`，
//! `ready`/`recovery_mode` 来自 `RecoveryModeProbe` 的 Runtime 当前快照。
//! 网关**内部投影**是扁平形状 `{schema_version, version, users, devices, roles,
//! grants, groups}`（对齐 Task 13 `SqlxManagementGateway::status` 与 05 适配器矩阵）：
//! handler 在顶层逐个校验五个十进制计数后组装进嵌套 `counts`，多余键忽略，因此内部
//! 扁平形状与线上嵌套契约互不耦合。
//! 探针可失败：Runtime 不可读时上抛固定去敏错误，handler 映射 503 `NOT_READY`，
//! 不用常量 `normal` 兜底、不把恢复模式等同调度放行。

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::get;

use crate::auth::service::Session;
use crate::error::ControllerError;
use crate::events::{EventBus, EventEnvelope};
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{authenticate_read, not_ready, require_admin_session};
use crate::time::TimeGateHandle;
use crate::time::evidence::{TimeEvidence, TimeQuality};

use super::{ManagementFuture, map_error};

#[derive(Clone, Copy, Debug)]
pub struct RecoveryView {
    pub ready: bool,
    pub mode: &'static str,
}

/// 恢复模式探针是 Runtime 真实控制器的可失败读取，不是静态字符串默认值：
/// `ready`/`mode` 必须来自 Runtime 当前启动/DB/恢复控制器真实快照，不可读时上抛错误
/// 由 handler 映射 503 `NOT_READY`。
pub trait RecoveryModeProbe: Send + Sync {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView>;
}

#[derive(Clone)]
pub struct SystemApiState {
    pub auth: Arc<HttpAuthState>,
    pub gateway: Arc<dyn SystemGateway>,
    pub recovery_probe: Arc<dyn RecoveryModeProbe>,
    /// 进程内事件总线句柄（06 §4 `system.time.changed`）：与 SSE 读线 `EventsState.bus`
    /// 是同一通道句柄。`None` 表示本装配未开启事件线，[`Self::publish_system_time_changed`]
    /// 即空操作（不发布、不报错、不阻塞）。
    pub events: Option<EventBus>,
    /// 时间门禁句柄（spec 02 §7 `GET /system/time`）：与任务线共用**同一个**
    /// `TimeGateHandle`（内部状态全在 `Arc` 后，克隆共享同一权威快照）。`None` 表示本
    /// 装配未接时间线，`GET /system/time` 回退为当前系统墙钟的 `SystemFallback` 证据。
    pub time_source: Option<TimeGateHandle>,
}

impl SystemApiState {
    /// 系统时间证据变化时的**唯一发布入口**（06 §4 `system.time.changed`）：调用方是时间
    /// 采样/校准侧（`time::gate` 的新快照被业务消费的时点），管理线不自己造第二套时间源，
    /// 也不在 `GET /system/status` 这类读路径上伪造时间事件。
    ///
    /// 本切片内**尚无进程内调用方**：时间线接入前本入口不被调用，事件流上因此不会出现任何
    /// `system.time.changed`（宁可不发，也不伪造一次没有发生过的时间变化）。
    ///
    /// 受众不接受调用方给出：[`EventEnvelope::system_time_changed`] 用 `TimeEvidence`
    /// 一份数据同时钉死载荷与 [`crate::events::EventAudience::Admins`]（06 §4「时间事件仅
    /// 管理员」）。`events = None` 即本装配未接事件线，直接返回；[`EventBus::publish`]
    /// 永不阻塞、永不上抛，无订阅者即丢弃，返回值在此丢弃。
    pub fn publish_system_time_changed(&self, evidence: &TimeEvidence) {
        if let Some(ref bus) = self.events {
            bus.publish(EventEnvelope::system_time_changed(evidence));
        }
    }
}

/// 即使只是统计也必须把 actor 送到仓储的当前管理员检查。
/// 网关内部投影是**扁平**形状（对齐 Task 13 `SqlxManagementGateway::status`）：
/// `{ schema_version: <number>, users, devices, roles, grants, groups }`，五个计数为
/// 顶层十进制字符串；缺任一键、非字符串或非十进制都是去敏 `Config` → 503。网关对象
/// 的多余键（如自带的 `version`）由 handler 忽略，HTTP 响应契约不受其影响。
pub trait SystemGateway: Send + Sync {
    fn status<'a>(&'a self, actor: &'a Session) -> ManagementFuture<'a, serde_json::Value>;
}

/// 网关扁平投影的顶层计数键：五个必须齐全且各为十进制字符串。
const SYSTEM_COUNT_KEYS: [&str; 5] = ["users", "devices", "roles", "grants", "groups"];

fn system_status_shape_error() -> ControllerError {
    ControllerError::Config("system status page shape".into())
}

/// 顶层十进制计数：非字符串、空串、带符号、负数、非 ASCII 数字一律 `None`。
fn decimal_count(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    map.get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|raw| !raw.is_empty() && raw.bytes().all(|b| b.is_ascii_digit()))
        .map(str::to_string)
}

/// `GET /api/v1/system/status` handler：调用顺序固定为
/// `authenticate_read(SystemStatusRead)` → `require_admin_session` →
/// `recovery_probe.current_mode()`（探针先行，Err → 固定 503 `NOT_READY`，
/// 不回显错误片段）→ `gateway.status(&actor)`（错误经 `map_error`）→
/// 扁平聚合页形状 fail-closed 校验 → 组装固定嵌套契约。
async fn get_system_status(State(state): State<SystemApiState>, headers: HeaderMap) -> Response {
    let actor = match authenticate_read(&state.auth, &headers, EndpointId::SystemStatusRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let view = match state.recovery_probe.current_mode().await {
        Ok(view) => view,
        // Runtime 快照不可读：固定去敏 503 `NOT_READY`，不进入 `map_error` 的
        // `STORAGE_UNAVAILABLE` 分支，也不降级为常量 `normal`。
        Err(_) => return not_ready(),
    };
    let status = match state.gateway.status(&actor).await {
        Ok(status) => status,
        Err(error) => return map_error(error),
    };
    let map = match &status {
        serde_json::Value::Object(map) => map,
        _ => return map_error(system_status_shape_error()),
    };
    let schema_version = match map
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
    {
        Some(version) if i64::from(i32::MIN) <= version && version <= i64::from(i32::MAX) => {
            version
        }
        _ => return map_error(system_status_shape_error()),
    };
    // 扁平投影 → 嵌套响应：五个计数在网关对象顶层逐个校验后组装进 `counts`；
    // 多余键忽略，`version` 只取 `CARGO_PKG_VERSION`，`ready`/`recovery_mode` 只来自探针。
    let mut counts = serde_json::Map::new();
    for key in SYSTEM_COUNT_KEYS {
        match decimal_count(map, key) {
            Some(value) => {
                counts.insert((*key).to_string(), serde_json::Value::String(value));
            }
            None => return map_error(system_status_shape_error()),
        }
    }
    ok_response(serde_json::json!({
        "ready": view.ready,
        "schema_version": schema_version,
        "version": env!("CARGO_PKG_VERSION"),
        "recovery_mode": view.mode,
        "counts": counts,
    }))
}

/// 未装配时间源（`time_source: None`）时的缺省证据：以当前系统墙钟构造
/// `SystemFallback`（对齐 `TimeGateHandle::new` 的初始证据形状：无采样来源、无
/// age/offset/uncertainty、`clock_epoch` 取 nil 表示本装配无时钟换代纪元）。
/// 这是对系统时钟的诚实回退，不伪造 NTP 有效，也不引入第二套时间源。
fn fallback_time_evidence() -> TimeEvidence {
    let now = chrono::Utc::now();
    TimeEvidence {
        system_wall_utc: now,
        reference_utc: now,
        quality: TimeQuality::SystemFallback,
        clock_epoch: uuid::Uuid::nil(),
        source: None,
        sample_age_ms: None,
        offset_ms: None,
        uncertainty_ms: None,
    }
}

/// `GET /api/v1/system/time` handler（spec 02 §7）：调用顺序固定为
/// `authenticate_read(SystemStatusRead)` → `require_admin_session`（非管理员 403）→
/// 时间源分流：装配了 `time_source` 时取 `gate.current()`（读取侧动态派生 age、
/// stale 即时降级）与 `background_retry_active()`；未装配时回退当前系统墙钟的
/// `SystemFallback` 证据、`retry_active: false`。响应 `data` 恰为
/// `{ evidence, retry_active }`，不含任何凭据。
async fn get_system_time(State(state): State<SystemApiState>, headers: HeaderMap) -> Response {
    let actor = match authenticate_read(&state.auth, &headers, EndpointId::SystemStatusRead).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if let Err(response) = require_admin_session(&actor) {
        return response;
    }
    let (evidence, retry_active) = match &state.time_source {
        Some(gate) => (gate.current().await, gate.background_retry_active()),
        None => (fallback_time_evidence(), false),
    };
    ok_response(serde_json::json!({
        "evidence": evidence,
        "retry_active": retry_active,
    }))
}

/// 挂载 system 的 2 条管理路由：`GET /api/v1/system/status` 与
/// `GET /api/v1/system/time` 的唯一注册锚点。
/// 本函数只接受显式装配的 `SystemApiState`，其中恢复探针是可失败读取。
/// 生产恢复控制器尚未接线时仍挂载路由，由占位探针返回错误，
/// `/system/status` handler 将其映射为 503 `NOT_READY`，不伪造恢复模式。
pub fn system_router(state: SystemApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/system/status", get(get_system_status))
        .route("/api/v1/system/time", get(get_system_time))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::test_fixture::*;
    use crate::events::{EventAudience, EventEnvelope, EventName, EventViewer};
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    /// 私有 system 网关桩：**扁平**投影（对齐 Task 13 `SqlxManagementGateway::status`
    /// 的 `{schema_version, version, users, devices, roles, grants, groups}`），计数为
    /// 十进制字符串；`version` 是 handler 不消费的多余键，必须被忽略。
    struct FixtureSystemGateway;

    impl SystemGateway for FixtureSystemGateway {
        fn status<'a>(&'a self, _actor: &'a Session) -> ManagementFuture<'a, serde_json::Value> {
            Box::pin(async {
                Ok(serde_json::json!({
                    "schema_version": 4,
                    "version": "gateway-version-must-be-ignored",
                    "users": "1",
                    "devices": "2",
                    "roles": "3",
                    "grants": "4",
                    "groups": "5",
                }))
            })
        }
    }

    /// 恢复模式探针桩：`Ok` 返回固定 `ready = true`、`mode = "normal"`；
    /// `Fail` 模拟 Runtime 不可读（固定去敏 `Config`），必须 503 `NOT_READY`。
    #[derive(Clone, Copy)]
    enum FixtureRecoveryProbe {
        Ok,
        Fail,
    }

    impl RecoveryModeProbe for FixtureRecoveryProbe {
        fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
            let fail = matches!(self, Self::Fail);
            Box::pin(async move {
                if fail {
                    Err(ControllerError::Config("fixture recovery down".into()))
                } else {
                    Ok(RecoveryView {
                        ready: true,
                        mode: "normal",
                    })
                }
            })
        }
    }

    /// 脏聚合页桩：任何形状偏离固定契约都必须 fail-closed 503。
    struct DirtyStatusGateway(serde_json::Value);

    impl SystemGateway for DirtyStatusGateway {
        fn status<'a>(&'a self, _actor: &'a Session) -> ManagementFuture<'a, serde_json::Value> {
            let page = self.0.clone();
            Box::pin(async move { Ok(page) })
        }
    }

    fn system_state(
        gateway: Arc<dyn SystemGateway>,
        probe: FixtureRecoveryProbe,
    ) -> SystemApiState {
        system_state_with_events(gateway, probe, None)
    }

    /// 事件总线可选的装配：`Some(bus)` 与 SSE 读线 `EventsState.bus` 是同一通道句柄，
    /// `None`（既有全部用例的基线）表示本装配未开启事件线。
    fn system_state_with_events(
        gateway: Arc<dyn SystemGateway>,
        probe: FixtureRecoveryProbe,
        events: Option<EventBus>,
    ) -> SystemApiState {
        SystemApiState {
            auth: fixture_auth(fixture_session(true)),
            gateway,
            recovery_probe: Arc::new(probe),
            events,
            time_source: None,
        }
    }

    fn fixture_system_state() -> SystemApiState {
        system_state(Arc::new(FixtureSystemGateway), FixtureRecoveryProbe::Ok)
    }

    async fn error_code(res: axum::response::Response) -> (StatusCode, String, String) {
        let status = res.status();
        let value = body_json(res).await;
        (
            status,
            value["error"]["code"].as_str().unwrap_or("").to_string(),
            value.to_string(),
        )
    }

    #[tokio::test]
    async fn system_status_returns_counts_and_recovery_mode() {
        let router = system_router(fixture_system_state());
        let res = router
            .oneshot(authenticated_get("/api/v1/system/status"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let val: serde_json::Value = body_json(res).await;
        assert_eq!(val["data"]["version"], env!("CARGO_PKG_VERSION"));
        assert_ne!(
            val["data"]["version"].as_str(),
            Some("gateway-version-must-be-ignored"),
            "version 只取 CARGO_PKG_VERSION，不得透传网关版本字段"
        );
        assert_eq!(val["data"]["recovery_mode"], "normal");
        assert_eq!(val["data"]["counts"]["users"], "1");
        assert_eq!(val["data"]["ready"], true);
        assert_eq!(val["data"]["schema_version"], 4);
        assert_eq!(val["data"]["counts"]["devices"], "2");
        assert_eq!(val["data"]["counts"]["roles"], "3");
        assert_eq!(val["data"]["counts"]["grants"], "4");
        assert_eq!(val["data"]["counts"]["groups"], "5");
        // 响应契约保持嵌套：五个计数在 `data.counts` 下，网关顶层计数不外泄。
        assert_eq!(
            val["data"]
                .as_object()
                .expect("data 对象")
                .keys()
                .collect::<std::collections::BTreeSet<_>>()
                .iter()
                .copied()
                .cloned()
                .collect::<Vec<String>>(),
            vec![
                "counts".to_string(),
                "ready".to_string(),
                "recovery_mode".to_string(),
                "schema_version".to_string(),
                "version".to_string(),
            ],
            "data 恰为五字段"
        );
        assert!(
            val["data"].get("users").is_none(),
            "网关扁平计数键不得出现在响应顶层"
        );
    }

    #[tokio::test]
    async fn system_status_unauthenticated_is_rejected_with_401() {
        let router = system_router(fixture_system_state());
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/status")
                    .header(header::HOST, FIXTURE_HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let value = body_json(res).await;
        assert_eq!(value["error"]["code"].as_str(), Some("AUTH_REQUIRED"));
        assert!(
            !value.to_string().contains("counts"),
            "401 响应不得携带聚合投影"
        );
    }

    #[tokio::test]
    async fn system_status_returns_503_not_ready_when_probe_fails() {
        let router = system_router(system_state(
            Arc::new(FixtureSystemGateway),
            FixtureRecoveryProbe::Fail,
        ));
        let (status, code, text) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/system/status"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code, "NOT_READY");
        assert!(
            !text.contains("counts") && !text.contains("normal"),
            "探针失败响应不得携带聚合或常量恢复模式回显"
        );
    }

    #[tokio::test]
    async fn system_status_rejects_non_admin_with_403() {
        let router = system_router(SystemApiState {
            auth: fixture_auth(fixture_session(false)),
            gateway: Arc::new(FixtureSystemGateway),
            recovery_probe: Arc::new(FixtureRecoveryProbe::Ok),
            events: None,
            time_source: None,
        });
        let (status, code, _) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/system/status"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
    }

    /// 合法扁平网关投影：`schema_version` + 顶层五个十进制计数 + handler 不消费的
    /// 多余 `version` 键。
    fn flat_status() -> serde_json::Map<String, serde_json::Value> {
        let mut map = serde_json::Map::new();
        map.insert("schema_version".into(), serde_json::json!(4));
        map.insert("version".into(), serde_json::json!("1.0.0"));
        for (key, value) in [
            ("users", "1"),
            ("devices", "2"),
            ("roles", "3"),
            ("grants", "4"),
            ("groups", "5"),
        ] {
            map.insert(key.into(), serde_json::Value::String(value.into()));
        }
        map
    }

    /// 在合法扁平页上覆盖一个键；`None` 表示删除该键。
    fn flat_status_with(key: &str, value: Option<serde_json::Value>) -> serde_json::Value {
        let mut map = flat_status();
        match value {
            None => {
                map.remove(key);
            }
            Some(value) => {
                map.insert(key.into(), value);
            }
        }
        serde_json::Value::Object(map)
    }

    /// 网关多余键（`version` 及任意未知键）不再是拒绝条件，也不得进入响应。
    #[tokio::test]
    async fn system_status_ignores_extra_gateway_keys() {
        let mut map = flat_status();
        map.insert("extra".into(), serde_json::json!("6"));
        map.insert(
            "time_evidence".into(),
            serde_json::json!({ "source": "fixture" }),
        );
        let router = system_router(system_state(
            Arc::new(DirtyStatusGateway(serde_json::Value::Object(map))),
            FixtureRecoveryProbe::Ok,
        ));
        let res = router
            .oneshot(authenticated_get("/api/v1/system/status"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["counts"]["users"], "1");
        assert_eq!(value["data"]["counts"]["groups"], "5");
        assert!(value["data"].get("extra").is_none());
        assert!(value["data"].get("time_evidence").is_none());
    }

    /// 扁平投影脏页逐项 fail-closed：缺任一计数键、非字符串、非 ASCII 十进制、
    /// 带符号、负数、空串，以及 `schema_version` 缺失/非数字/i32 越界。
    /// 旧的嵌套 `counts` 形状在扁平契约下同样必须拒绝（跨任务契约锚点）。
    #[tokio::test]
    async fn system_status_fails_closed_on_dirty_gateway_pages() {
        let mut cases = vec![
            serde_json::json!("not-an-object"),
            serde_json::json!([]),
            serde_json::json!({}),
            serde_json::json!({"schema_version": 4, "counts": {
                "users": "1", "devices": "2", "roles": "3", "grants": "4", "groups": "5"
            }}),
            flat_status_with("schema_version", None),
            flat_status_with("schema_version", Some(serde_json::json!("4"))),
            flat_status_with("schema_version", Some(serde_json::json!(true))),
            flat_status_with("schema_version", Some(serde_json::json!(4.0))),
            flat_status_with(
                "schema_version",
                Some(serde_json::json!(i64::from(i32::MAX) + 1)),
            ),
            flat_status_with(
                "schema_version",
                Some(serde_json::json!(i64::from(i32::MIN) - 1)),
            ),
        ];
        for key in SYSTEM_COUNT_KEYS {
            cases.push(flat_status_with(key, None));
            cases.push(flat_status_with(key, Some(serde_json::json!(1))));
            cases.push(flat_status_with(key, Some(serde_json::Value::Null)));
            cases.push(flat_status_with(key, Some(serde_json::json!({ "v": "1" }))));
            for bad in ["+1", "-1", "", " 1", "1.0", "0x1", "1e0", "abc", "١"] {
                cases.push(flat_status_with(
                    key,
                    Some(serde_json::Value::String(bad.into())),
                ));
            }
        }
        for page in cases {
            let router = system_router(system_state(
                Arc::new(DirtyStatusGateway(page.clone())),
                FixtureRecoveryProbe::Ok,
            ));
            let (status, code, _) = error_code(
                router
                    .oneshot(authenticated_get("/api/v1/system/status"))
                    .await
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{page}");
            assert_eq!(code, "STORAGE_UNAVAILABLE", "{page}");
        }
    }

    // -----------------------------------------------------------------------
    // `system.time.changed` 发布入口（06 §4，受众恒管理员）
    // -----------------------------------------------------------------------

    /// 固定形状的时间证据桩：不接任何真实时钟或 NTP 源。
    fn fixture_time_evidence() -> TimeEvidence {
        TimeEvidence {
            system_wall_utc: chrono::Utc::now(),
            reference_utc: chrono::Utc::now(),
            quality: TimeQuality::NtpValid,
            clock_epoch: uuid::Uuid::nil(),
            source: Some("ntp.example".into()),
            sample_age_ms: Some(1_500),
            offset_ms: Some(-5),
            uncertainty_ms: Some(15),
        }
    }

    /// 正例：`events` 配置了总线时，发布入口投一条 `system.time.changed`，载荷 `kind`
    /// 固定、`evidence` 就是传入的那一份 `TimeEvidence`（不新增第二套时间字段），受众
    /// 恒管理员：普通用户会话即便持有同一 user id 也不可见。
    #[tokio::test]
    async fn publish_system_time_changed_publishes_admin_only_event_when_bus_configured() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let state = system_state_with_events(
            Arc::new(FixtureSystemGateway),
            FixtureRecoveryProbe::Ok,
            Some(bus),
        );
        let evidence = fixture_time_evidence();
        state.publish_system_time_changed(&evidence);
        let event: EventEnvelope = rx.try_recv().expect("时间证据变化必须投出一条事件");
        assert_eq!(event.name(), EventName::SystemTimeChanged);
        assert_eq!(event.data()["kind"], serde_json::json!("system_time"));
        assert_eq!(
            event.data()["evidence"]["source"],
            serde_json::json!("ntp.example")
        );
        assert_eq!(
            event.data()["evidence"]["quality"],
            serde_json::json!("ntp_valid")
        );
        assert_eq!(
            event.data()["evidence"]["sample_age_ms"],
            serde_json::json!("1500")
        );
        assert_eq!(event.audience(), EventAudience::Admins);
        assert!(event.visible_to(EventViewer {
            user_id: [7u8; 16],
            is_admin: true,
        }));
        assert!(!event.visible_to(EventViewer {
            user_id: [7u8; 16],
            is_admin: false,
        }));
    }

    /// 负例：`events: None`（既有装配形状）时发布入口是空操作——不 panic、不构造通道；
    /// 读端点 `GET /system/status` 也不得顺带发布任何时间事件（不伪造变化）。
    #[tokio::test]
    async fn publish_system_time_changed_without_bus_is_noop_and_reads_publish_nothing() {
        let state = fixture_system_state();
        assert!(state.events.is_none(), "基线装配不得持有总线句柄");
        state.publish_system_time_changed(&fixture_time_evidence());

        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let router = system_router(system_state_with_events(
            Arc::new(FixtureSystemGateway),
            FixtureRecoveryProbe::Ok,
            None,
        ));
        let res = router
            .oneshot(authenticated_get("/api/v1/system/status"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            matches!(
                rx.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ),
            "读路径与未接事件线的装配都不得有任何事件进入通道"
        );
    }

    // -----------------------------------------------------------------------
    // `GET /api/v1/system/time`（spec 02 §7：TimeEvidence + 后台重试状态，管理员专属）
    // -----------------------------------------------------------------------

    /// 门禁夹具：`TimeGateHandle::new` 不 spawn 任何采样任务，直接预置权威快照并显式
    /// 指定后台重试状态；`sample_instant = None` 让 `current()` 原样返回预置证据
    /// （不派生 age、不动态降级），断言完全确定。
    async fn fixture_time_gate(evidence: TimeEvidence, retry_active: bool) -> TimeGateHandle {
        let gate = TimeGateHandle::new(
            &crate::time::TimeGateConfig::default(),
            uuid::Uuid::new_v4(),
        );
        {
            let mut snapshot = gate.snapshot.write().await;
            snapshot.evidence = evidence;
            snapshot.sample_instant = None;
        }
        gate.retry_active
            .store(retry_active, std::sync::atomic::Ordering::SeqCst);
        gate
    }

    fn system_state_with_time_source(time_source: Option<TimeGateHandle>) -> SystemApiState {
        SystemApiState {
            time_source,
            ..fixture_system_state()
        }
    }

    /// 正例：管理员 + 装配时间源 → 200，`data.evidence` 就是门禁权威快照（source /
    /// quality / 十进制字符串 age / offset / uncertainty / 可解析 UUID 的 clock_epoch /
    /// RFC3339 墙钟），`retry_active` 反映门禁状态；响应不含任何凭据字段。
    #[tokio::test]
    async fn system_time_returns_gate_evidence_and_retry_state() {
        let evidence = fixture_time_evidence();
        let gate = fixture_time_gate(evidence, false).await;
        let router = system_router(system_state_with_time_source(Some(gate)));
        let res = router
            .oneshot(authenticated_get("/api/v1/system/time"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["retry_active"], serde_json::json!(false));
        let ev = &value["data"]["evidence"];
        assert_eq!(ev["quality"], "ntp_valid");
        assert_eq!(ev["source"], "ntp.example");
        assert_eq!(ev["sample_age_ms"], "1500");
        assert_eq!(ev["offset_ms"], serde_json::json!(-5));
        assert_eq!(ev["uncertainty_ms"], serde_json::json!(15));
        assert!(ev["system_wall_utc"].is_string());
        assert!(ev["reference_utc"].is_string());
        assert!(
            uuid::Uuid::parse_str(ev["clock_epoch"].as_str().expect("clock_epoch 字符串")).is_ok()
        );
        // data 恰为两字段；且响应不含任何凭据形态的字段。
        assert_eq!(
            value["data"]
                .as_object()
                .expect("data 对象")
                .keys()
                .collect::<std::collections::BTreeSet<_>>()
                .iter()
                .copied()
                .cloned()
                .collect::<Vec<String>>(),
            vec!["evidence".to_string(), "retry_active".to_string()],
            "data 恰为 evidence + retry_active"
        );
        let text = value.to_string().to_lowercase();
        for forbidden in ["digest", "token", "password", "cookie", "csrf", "secret"] {
            assert!(
                !text.contains(forbidden),
                "响应不得出现凭据字段 {forbidden}"
            );
        }
    }

    /// 后台 NTP 退避重试激活时，`retry_active` 必须如实为 `true`（05「下次重试」状态）。
    #[tokio::test]
    async fn system_time_reports_background_retry_active() {
        let gate = fixture_time_gate(fixture_time_evidence(), true).await;
        let router = system_router(system_state_with_time_source(Some(gate)));
        let res = router
            .oneshot(authenticated_get("/api/v1/system/time"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        assert_eq!(value["data"]["retry_active"], serde_json::json!(true));
    }

    /// 未装配时间源：回退为当前系统墙钟的 `SystemFallback` 证据（无 source/age/offset/
    /// uncertainty，`retry_active: false`），不伪造 NTP 有效。
    #[tokio::test]
    async fn system_time_without_time_source_falls_back_to_system_clock() {
        let router = system_router(system_state_with_time_source(None));
        let res = router
            .oneshot(authenticated_get("/api/v1/system/time"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let value = body_json(res).await;
        let ev = &value["data"]["evidence"];
        assert_eq!(ev["quality"], "system_fallback");
        assert!(ev["source"].is_null());
        assert!(ev["sample_age_ms"].is_null());
        assert!(ev["offset_ms"].is_null());
        assert!(ev["uncertainty_ms"].is_null());
        assert!(ev["system_wall_utc"].is_string());
        assert_eq!(value["data"]["retry_active"], serde_json::json!(false));
    }

    /// 非管理员：403 `PERMISSION_DENIED`，且不泄露任何时间证据。
    #[tokio::test]
    async fn system_time_rejects_non_admin_with_403() {
        let router = system_router(SystemApiState {
            auth: fixture_auth(fixture_session(false)),
            gateway: Arc::new(FixtureSystemGateway),
            recovery_probe: Arc::new(FixtureRecoveryProbe::Ok),
            events: None,
            time_source: Some(fixture_time_gate(fixture_time_evidence(), true).await),
        });
        let (status, code, text) = error_code(
            router
                .oneshot(authenticated_get("/api/v1/system/time"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(code, "PERMISSION_DENIED");
        assert!(
            !text.contains("evidence") && !text.contains("ntp.example"),
            "403 响应不得携带时间证据投影"
        );
    }

    /// 未认证：401 `AUTH_REQUIRED`，不泄露任何时间证据。
    #[tokio::test]
    async fn system_time_unauthenticated_is_rejected_with_401() {
        let router = system_router(system_state_with_time_source(Some(
            fixture_time_gate(fixture_time_evidence(), true).await,
        )));
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/system/time")
                    .header(header::HOST, FIXTURE_HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let value = body_json(res).await;
        assert_eq!(value["error"]["code"].as_str(), Some("AUTH_REQUIRED"));
        assert!(
            !value.to_string().contains("evidence"),
            "401 响应不得携带时间证据投影"
        );
    }
}

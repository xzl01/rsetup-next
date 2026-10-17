//! system 资源管理路由：网关/恢复模式探针契约与 1 条管理端点（Identity Task 12）。
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
use crate::http_api::ok_response;
use crate::http_auth::{EndpointId, HttpAuthState};
use crate::http_gate::{authenticate_read, not_ready, require_admin_session};

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

/// 挂载 system 的 1 条管理路由：`GET /api/v1/system/status` 唯一注册锚点。
/// 恢复探针是 Runtime 真实控制器的可失败读取：未接线时不构造 probe、不挂载 router，
/// 本函数只接受显式装配的 `SystemApiState`。
pub fn system_router(state: SystemApiState) -> Router<()> {
    Router::new()
        .route("/api/v1/system/status", get(get_system_status))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::users::test_fixture::*;
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
        SystemApiState {
            auth: fixture_auth(fixture_session(true)),
            gateway,
            recovery_probe: Arc::new(probe),
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
}

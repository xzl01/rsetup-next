//! 生产路由装配测试（Task 0a 纯拓扑合并 + Task 0b typed 生产装配）。
//!
//! 分两层，互不冒充：
//! 1. [`compose_component_routers`] 的 synthetic 合并拓扑（可达性、无覆盖、无重复
//!    探活）——只证明合并能力；
//! 2. [`build_production_router`] 的 typed 装配——五线状态全部来自各条线**生产**
//!    构造器（`build_sqlx_management_state`、`SqlxTaskApi`、`build_device_status_router`、
//!    `web_router`），只证明「同一 `Arc<HttpAuthState>` 才能装配」与「五线路由都在
//!    同一顶层 router 上可达」；认证桩与 lazy 连接池只用于把 router 立起来，
//!    **不**证明认证语义、SQL 来源、真实设备数据或资源内容。

use std::sync::Arc;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    routing::get,
};
use rsetup_controller::production::compose_component_routers;
use tower::ServiceExt;

fn component(path: &str, marker: &'static str) -> Router<()> {
    Router::new().route(path, get(move || async move { marker }))
}

fn composed() -> Router<()> {
    let auth = component("/healthz", "auth-health").merge(component("/readyz", "auth-ready"));
    compose_component_routers(
        auth,
        component("/api/v1/users", "management"),
        component("/api/v1/tasks", "tasks"),
        component("/api/v1/devices/test/status", "status"),
        component("/metrics", "metrics"),
        component("/", "web"),
    )
}

#[tokio::test]
async fn every_component_is_reachable_without_duplicate_probes() {
    let router = composed();
    for (path, marker) in [
        ("/healthz", "auth-health"),
        ("/readyz", "auth-ready"),
        ("/api/v1/users", "management"),
        ("/api/v1/tasks", "tasks"),
        ("/api/v1/devices/test/status", "status"),
        ("/metrics", "metrics"),
        ("/", "web"),
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "component {path}");
        let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(bytes.as_ref(), marker.as_bytes(), "component {path}");
    }
}

#[test]
#[should_panic(expected = "Overlapping method route")]
fn duplicate_business_path_panics_instead_of_being_hidden_by_override() {
    // management 与 tasks 注册了同一个业务路径：合并必须 panic，
    // 不能让后注册的组件静默覆盖先注册的组件。
    let _router = compose_component_routers(
        component("/healthz", "auth-health"),
        component("/api/v1/users", "management"),
        component("/api/v1/users", "tasks"),
        component("/api/v1/devices/test/status", "status"),
        component("/metrics", "metrics"),
        component("/", "web"),
    );
}

// ---------------------------------------------------------------------------
// Task 0b：typed 生产装配与共享认证上下文
// ---------------------------------------------------------------------------

mod typed {
    use super::*;

    use axum::http::{HeaderMap, HeaderValue, header};
    use rsetup_controller::AppState;
    use rsetup_controller::ControllerError;
    use rsetup_controller::DbPool;
    use rsetup_controller::HttpAuthState;
    use rsetup_controller::api::ManagementFuture;
    use rsetup_controller::api::adapters::build_sqlx_management_state;
    use rsetup_controller::api::approvals::ManagementNotificationSink;
    use rsetup_controller::api::devices::{
        DeviceConnectionState, DeviceLiveProbe, DeviceLiveView, DeviceStreamHealth,
    };
    use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
    use rsetup_controller::auth::service::{Login, Session, SessionPage};
    use rsetup_controller::authz::sqlx_repo::SqlxAuthorizationSnapshotRepository;
    use rsetup_controller::devices::service::AdmissionChangeEvent;
    use rsetup_controller::http_auth::{AuthFuture, AuthServiceApi};
    use rsetup_controller::http_device_status::{
        DeviceStatusState, EmptyDeviceStatusSnapshotSource,
    };
    use rsetup_controller::http_security::HttpSecurityConfig;
    use rsetup_controller::metrics::collect::{
        MAX_CONCURRENT_SCRAPE, MetricsFuture, MetricsRuntime, MetricsSnapshot,
        MetricsSnapshotSource,
    };
    use rsetup_controller::metrics::config::{BasicCredentials, MetricsConfig, RESERVED_PATHS};
    use rsetup_controller::metrics::model::{DeviceCounts, TaskCounts};
    use rsetup_controller::production::{
        MetricsLineInputs, ProductionInputs, build_production_router,
        build_production_router_with_metrics_source,
    };
    use rsetup_controller::tasks::SqlxTaskApi;
    use rsetup_controller::tasks::http::HttpTasksState;
    use rsetup_controller::tasks::repository::SqlxTaskRepository;
    use rsetup_controller::tasks::service::TaskService;
    use rsetup_controller::web::{ASSET_DIR, SPA_EXACT_ROUTES, SPA_PARAM_ROUTES};
    use rsetup_controller::web_router;
    use serde_json::{Value, json};
    use sqlx::mysql::MySqlPoolOptions;
    use std::time::Duration;

    const HOST: &str = "controller.test";
    /// 合法设备 ID 形状（64 位小写 hex）。
    const DEVICE_STATUS_PATH: &str =
        "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001/status";
    /// synthetic 游标密钥：仅用于把生产 `SqlxTaskApi` 立起来，本文件不签发任何游标。
    const SYNTHETIC_CURSOR_SECRET: [u8; 32] = [7u8; 32];

    /// 认证桩：`is_live_session` 恒 false，因此共享 gate 在触达仓储前严格 401。
    /// 本文件只用它把 router 立起来并断言「路由已注册」，不认证任何真实会话。
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

    /// synthetic probe/通知源：形状真实、取值 synthetic，且本文件的断言从不触达它们
    /// （所有业务路径都在共享 gate 处 401）。不证明真实设备数据或恢复模式。
    struct SyntheticSources;

    impl DeviceLiveProbe for SyntheticSources {
        fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
            Box::pin(async {
                Ok(DeviceLiveView {
                    connection_state: DeviceConnectionState::Offline,
                    control_health: DeviceStreamHealth::Offline,
                    data_health: DeviceStreamHealth::Offline,
                    capabilities: None,
                })
            })
        }
    }

    impl RecoveryModeProbe for SyntheticSources {
        fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
            Box::pin(async {
                Ok(RecoveryView {
                    ready: false,
                    mode: "synthetic",
                })
            })
        }
    }

    impl ManagementNotificationSink for SyntheticSources {
        fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
            Ok(())
        }
    }

    /// 永不连库的 lazy 池：只用于把生产仓储对象立起来；本文件不执行任何 SQL。
    fn lazy_db() -> DbPool {
        DbPool(
            MySqlPoolOptions::new()
                .acquire_timeout(Duration::from_millis(100))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/rsetup_composition")
                .expect("lazy pool construction must not connect"),
        )
    }

    fn auth_state() -> Arc<HttpAuthState> {
        Arc::new(HttpAuthState::new(
            Arc::new(NoSessionAuth),
            HttpSecurityConfig {
                allowed_hosts: vec![HOST.into()],
                allowed_origin: Some("http://controller.test".into()),
                trust_tls: false,
            },
        ))
    }

    /// 指标线装配项的 fixture：`enabled` 与端点可按用例指定。
    fn metrics_inputs(enabled: bool) -> MetricsLineInputs {
        MetricsLineInputs {
            config: Arc::new(MetricsConfig {
                enabled,
                endpoint: "/metrics".to_string(),
                credentials: enabled.then(|| BasicCredentials {
                    username: "prom_admin".to_string(),
                    password: "metrics-composition-fixture".to_string(),
                }),
            }),
            runtime: Arc::new(MetricsRuntime::new()),
        }
    }

    /// 用**指定**池装配六线输入（§7.6 第 6/10 条要观察同一个池的读数）。
    fn inputs_with_db(
        db: &DbPool,
        auth: Arc<HttpAuthState>,
        management_auth: Arc<HttpAuthState>,
        tasks_auth: Arc<HttpAuthState>,
        status_auth: Arc<HttpAuthState>,
        metrics: MetricsLineInputs,
    ) -> ProductionInputs {
        let transport = Arc::new(SyntheticSources);
        ProductionInputs {
            auth: AppState {
                db: db.clone(),
                auth,
            },
            management: build_sqlx_management_state(
                db.clone(),
                management_auth,
                transport.clone(),
                transport.clone(),
                transport,
            ),
            tasks: HttpTasksState {
                api: Arc::new(SqlxTaskApi::new(
                    Arc::new(TaskService::new(SqlxTaskRepository::new(db.0.clone()))),
                    Arc::new(SqlxTaskRepository::new(db.0.clone())),
                    Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone())),
                    SYNTHETIC_CURSOR_SECRET,
                )),
                auth: tasks_auth,
            },
            status: DeviceStatusState {
                auth: status_auth,
                snapshot: Arc::new(EmptyDeviceStatusSnapshotSource),
            },
            metrics,
            web: web_router(),
        }
    }

    /// 原四参数版本保留为薄壳，供既有 3 条负例复用；指标线取禁用态。
    fn inputs(
        auth: Arc<HttpAuthState>,
        management_auth: Arc<HttpAuthState>,
        tasks_auth: Arc<HttpAuthState>,
        status_auth: Arc<HttpAuthState>,
    ) -> ProductionInputs {
        let db = lazy_db();
        inputs_with_db(
            &db,
            auth,
            management_auth,
            tasks_auth,
            status_auth,
            metrics_inputs(false),
        )
    }

    async fn request_get(router: &Router<()>, uri: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(if uri == "/api/v1/auth/login" {
                        "POST"
                    } else {
                        "GET"
                    })
                    .uri(uri)
                    .header("host", HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 262_144).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned())),
        )
    }

    fn assert_mismatch(error: ControllerError) {
        match error {
            ControllerError::Config(message) => assert_eq!(
                message, "production auth context mismatch",
                "必须是固定去敏的指针不一致错误，不回显任何地址"
            ),
            other => panic!("预期 Config(production auth context mismatch)，实得 {other:?}"),
        }
    }

    #[tokio::test]
    async fn mismatched_auth_context_is_rejected_on_every_line() {
        // 任一条线换了 HttpAuthState 实例（哪怕只有一条），装配必须失败关闭。
        for line in ["management", "tasks", "status"] {
            let shared = auth_state();
            let other = auth_state();
            let (management, tasks, status) = match line {
                "management" => (Arc::clone(&other), Arc::clone(&shared), Arc::clone(&shared)),
                "tasks" => (Arc::clone(&shared), Arc::clone(&other), Arc::clone(&shared)),
                _ => (Arc::clone(&shared), Arc::clone(&shared), Arc::clone(&other)),
            };
            match build_production_router(inputs(shared, management, tasks, status)) {
                // 只断言「必须 Err」；不 Debug 打印 Router，避免失败输出被整张路由表淹没。
                Ok(_) => panic!("{line} 线认证上下文不一致时必须 Err"),
                Err(error) => assert_mismatch(error),
            }
        }
    }

    #[tokio::test]
    async fn shared_auth_context_composes_all_six_lines() {
        // 原五线断言全部保留，另加指标线：四件事同时成立才是单端口六线合并。
        let shared = auth_state();
        let db = lazy_db();
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &db,
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("同一 Arc<HttpAuthState> 必须装配成功");

        // auth 线：探活由 auth 线注册，且只有一个实现。
        let (status, _) = request_get(&router, "/healthz").await;
        assert_eq!(status, StatusCode::OK, "/healthz");
        // lazy 池不可达：readyz 必须 503，证明它接的是真实池而非常量 200。
        let (status, _) = request_get(&router, "/readyz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "/readyz");
        let (status, body) = request_get(&router, "/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "/api/v1/auth/me");
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));

        // metrics 线：无凭据 401；带 Basic 凭据 200，读数来自注入的合成源。
        let (status, _) = request_with_headers(&router, "GET", "/metrics", &HeaderMap::new()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "/metrics 无凭据必须 401");
        let (status, body) =
            request_with_headers(&router, "GET", "/metrics", &authorized_metrics_headers()).await;
        assert_eq!(status, StatusCode::OK, "/metrics 带凭据必须 200");
        let text = String::from_utf8(body).expect("UTF-8 文本");
        assert!(text.contains("rsetup_controller_devices_total{admission=\"APPROVED\"} 3"));

        // management / tasks / status 三线：路由已注册（否则 404），且都在共享
        // gate 处严格 401，未触达任何仓储。含 percent-decode 非法 UTF-8 的状态
        // 路径：路径提取失败绝不先于认证门禁短路（修复轮 1 · F2）。
        for path in [
            "/api/v1/users",
            "/api/v1/roles",
            "/api/v1/audit",
            "/api/v1/system/status",
            "/api/v1/tasks",
            "/api/v1/tasks/00000000-0000-4000-8000-000000000001",
            DEVICE_STATUS_PATH,
            "/api/v1/devices/%FF/status",
        ] {
            let (status, body) = request_get(&router, path).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "业务路径 {path} 必须 401");
            assert_eq!(
                body["error"]["code"],
                json!("AUTH_REQUIRED"),
                "业务路径 {path} 必须走同一共享 gate"
            );
        }

        // web 线：SPA 入口由嵌入资源服务。
        let (status, _) = request_get(&router, "/").await;
        assert_eq!(status, StatusCode::OK, "web 线 / 必须 200");

        // 未注册路径不得被任何一线兜底。
        let (status, _) = request_get(&router, "/api/v1/not-registered").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn composed_router_registers_status_route_behind_shared_gate() {
        // 设备状态线在装配后归属顶层生产 router：未认证请求（含合法 ID 与
        // percent-decode 非法的路径段）都在共享 gate 处严格 401，绝不 404、
        // 也绝不先于认证返回提取器 400。本测试只证明路由归属与 gate 优先，
        // **不**读取状态源；空样本投影与已认证响应由 lib 单测
        // `live_session_empty_source_returns_unknown_with_time_evidence_object`
        // 覆盖（EmptyDeviceStatusSnapshotSource 语义：snapshot=null/unknown/
        // age_ms=null，received_time 为 TimeEvidence 对象，不伪造 PASS）。
        let shared = auth_state();
        let router = build_production_router(inputs(
            Arc::clone(&shared),
            Arc::clone(&shared),
            Arc::clone(&shared),
            Arc::clone(&shared),
        ))
        .expect("装配必须成功");
        for path in [
            "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001",
            DEVICE_STATUS_PATH,
            "/api/v1/devices/%FF/status",
        ] {
            let (status, body) = request_get(&router, path).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "路径 {path} 必须 401 而非 404/400"
            );
            assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"), "{path}");
        }
    }

    // -----------------------------------------------------------------
    // Task 6：指标线六线装配与 §7.6 第 1–10 条
    // -----------------------------------------------------------------

    /// 合成汇总源：两条聚合的取值由测试给定，池 gauge 仍走真实内存读数。
    struct SyntheticMetricsSnapshotSource;

    impl MetricsSnapshotSource for SyntheticMetricsSnapshotSource {
        fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
            Box::pin(async {
                Ok(MetricsSnapshot {
                    devices: DeviceCounts {
                        approved: 3,
                        pending: 2,
                        rejected: 1,
                    },
                    tasks: TaskCounts {
                        pending: 1,
                        running: 2,
                        succeeded: 4,
                        failed: 5,
                    },
                })
            })
        }
    }

    /// 永久挂起的合成源（§7.6 第 10 条）。
    struct HangingMetricsSnapshotSource;

    impl MetricsSnapshotSource for HangingMetricsSnapshotSource {
        fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
            Box::pin(std::future::pending())
        }
    }

    fn authorized_metrics_headers() -> HeaderMap {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Basic {}",
                STANDARD.encode("prom_admin:metrics-composition-fixture")
            ))
            .unwrap(),
        );
        headers
    }

    async fn request_with_headers(
        router: &Router<()>,
        method: &str,
        uri: &str,
        headers: &HeaderMap,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let response = router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 262_144).await.unwrap();
        (status, bytes.to_vec())
    }

    /// 从编译期嵌入的 `index.html` 取第一个真实资产路径（与 `web.rs` 同源，不
    /// 硬编码哈希名）：未嵌入的 `/assets/…` 按 web 线契约坚决 404（不回退 HTML），
    /// 「资源前缀仍由 web 线应答」只能用真实嵌入资产证明。
    fn embedded_asset_path() -> String {
        let html = include_str!("../../../apps/controller-web/dist/index.html");
        let at = html
            .find(ASSET_DIR)
            .expect("index.html 必须引用 /assets/ 资产");
        let end = html[at..]
            .find('"')
            .map(|offset| at + offset)
            .expect("资产引用必须以引号收尾");
        html[at..end].to_string()
    }

    #[test]
    #[should_panic(expected = "Overlapping method route")]
    fn metrics_path_colliding_with_reserved_route_panics_on_merge() {
        // 配置期拦截（§3.4）之外的第二道网：真把两条 /metrics 递进合并，仍必须 panic。
        let _router = compose_component_routers(
            component("/healthz", "auth-health"),
            component("/api/v1/users", "management"),
            component("/api/v1/tasks", "tasks"),
            component("/api/v1/devices/test/status", "status"),
            component("/metrics", "metrics"),
            component("/metrics", "duplicate-metrics"),
        );
    }

    #[tokio::test]
    async fn metrics_gauges_read_the_shared_pool_only() {
        // 两条聚合走合成源，池 gauge 仍是真实内存读数：lazy 池未建任何连接 → 全 0，
        // 且抓取前后 lazy 池都不得产生连接（证明读的是 auth.db 那个池且不发查询）。
        let shared = auth_state();
        let db = lazy_db();
        assert_eq!(db.0.size(), 0, "lazy 池构造不连接");
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &db,
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        let (status, body) =
            request_with_headers(&router, "GET", "/metrics", &authorized_metrics_headers()).await;
        assert_eq!(status, StatusCode::OK);
        let text = String::from_utf8(body).expect("UTF-8 文本");
        assert!(
            text.contains("rsetup_controller_db_pool_connections{state=\"active\"} 0"),
            "lazy 池无活动连接：{text}"
        );
        assert!(
            text.contains("rsetup_controller_db_pool_connections{state=\"idle\"} 0"),
            "lazy 池无空闲连接：{text}"
        );
        assert_eq!(
            db.0.size(),
            0,
            "抓取前后 lazy 池都不得建立连接（指标线不发查询）"
        );
    }

    #[tokio::test]
    async fn metrics_line_never_touches_session_boundary() {
        // 完整合法会话 Cookie 打 /metrics → 仍 401（会话不是指标线的凭据）；
        // 随后打 /api/v1/auth/me 仍按会话语义工作 → 抓取未污染会话/CSRF/限速。
        // 注意：401 只在**启用态**成立（禁用态按 §4.5 恒 404、与路径不存在不可
        // 区分），因此本用例显式传入 metrics_inputs(true)，不套薄壳 inputs()。
        let shared = auth_state();
        let db = lazy_db();
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &db,
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("rsc_session=composition-fixture-session"),
        );
        let (status, _) = request_with_headers(&router, "GET", "/metrics", &headers).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "会话 Cookie 不是指标线凭据"
        );
        let (status, body) = request_get(&router, "/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "会话线路语义不变");
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
    }

    #[tokio::test]
    async fn reserved_paths_stay_registered() {
        // 保留集合内的路径必须仍由所属条线应答，不被指标线抢占或退化为 404。
        let shared = auth_state();
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &lazy_db(),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        for path in [
            "/healthz",
            "/readyz",
            "/",
            "/login",
            "/password",
            "/sessions",
        ] {
            let (status, _) = request_get(&router, path).await;
            assert_ne!(
                status,
                StatusCode::NOT_FOUND,
                "保留路径 {path} 不得退化为 404"
            );
        }
        let asset = embedded_asset_path();
        let (status, _) = request_with_headers(&router, "GET", &asset, &HeaderMap::new()).await;
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "资源前缀仍由 web 线应答（{asset}）"
        );
        // 保留集合不与代码漂移：web.rs 的三张表逐条被 RESERVED_PATHS 覆盖。
        for path in SPA_EXACT_ROUTES {
            assert!(
                RESERVED_PATHS.contains(path),
                "SPA 精确路由 {path} 未登记进 RESERVED_PATHS"
            );
        }
        // ASSET_DIR 是 "/assets/"，登记形态是前缀段 "/assets"（§6.5）。
        let asset_prefix = ASSET_DIR.trim_end_matches('/');
        assert!(
            RESERVED_PATHS.contains(&asset_prefix),
            "资源目录前缀 {asset_prefix} 未登记（ASSET_DIR = {ASSET_DIR}）"
        );
        for template in SPA_PARAM_ROUTES {
            // "/devices/{id}" → 字面段 "/devices"（§6.5 的前缀段）。
            let literal = format!(
                "/{}",
                template
                    .trim_start_matches('/')
                    .split('/')
                    .next()
                    .unwrap_or_default()
            );
            assert!(
                RESERVED_PATHS.contains(&literal.as_str()),
                "SPA 参数路由字面段 {literal} 未登记（模板 {template}）"
            );
        }
    }

    #[test]
    fn production_inputs_exposes_no_second_pool() {
        // 结构性断言：装配项只有 config 与 runtime，写全字段，杜绝 ..Default 兜底。
        let inputs = MetricsLineInputs {
            config: Arc::new(MetricsConfig {
                enabled: false,
                endpoint: "/metrics".to_string(),
                credentials: None,
            }),
            runtime: Arc::new(MetricsRuntime::new()),
        };
        // 下面两行是"字段集合恰为 {config, runtime}"的编译期证明：多一个字段就编译不过。
        let _config: Arc<MetricsConfig> = Arc::clone(&inputs.config);
        let _runtime: Arc<MetricsRuntime> = Arc::clone(&inputs.runtime);
        // 源码级静态门禁（§6.3、§7.8）：main.rs 只调用 build_production_router，
        // 真实源与测试注入缝只出现在 production.rs。
        let main = include_str!("../src/main.rs");
        assert!(
            main.contains("build_production_router("),
            "main.rs 必须经唯一入口装配"
        );
        assert!(
            !main.contains("build_production_router_with_metrics_source"),
            "测试注入缝绝不被生产路径调用"
        );
        assert!(
            !main.contains("SqlxMetricsSnapshotSource"),
            "真实汇总源只能在 production.rs 内构造"
        );
        let production = include_str!("../src/production.rs");
        assert!(
            production.contains("SqlxMetricsSnapshotSource::new(inputs.auth.db.clone())"),
            "汇总源必须由 auth.db 派生（I2 架构契约）"
        );
        let api = include_str!("../src/lib.rs");
        assert!(!api.contains("SqlxMetricsSnapshotSource::new"));
        let http = include_str!("../src/metrics/http.rs");
        // 只看 `#[cfg(test)]` 之前的实现段：Task 5 的单测用真实源（lazy 池、只读、
        // 不建池）验证失败/超时映射，那是测试段而非生产路径。
        let http_impl = http.split("#[cfg(test)]").next().unwrap_or(http);
        assert!(!http_impl.contains("SqlxMetricsSnapshotSource::new"));
    }

    #[tokio::test]
    async fn metrics_timeout_does_not_starve_business_routes() {
        // L3 侧的**结构**部分：合成源永久挂起 + 并发闸占满，业务线路由形状不受影响。
        // 真实慢查询下的池可用性不在此证明范围（§7.7 L4，Task 7）。
        let shared = auth_state();
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &lazy_db(),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(HangingMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        let pending: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let router = router.clone();
                let headers = authorized_metrics_headers();
                tokio::spawn(async move {
                    let _ = request_with_headers(&router, "GET", "/metrics", &headers).await;
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let (status, _) = request_get(&router, "/healthz").await;
        assert_eq!(status, StatusCode::OK, "指标线挂起不得拖住探活");
        let (status, body) = request_get(&router, "/api/v1/auth/me").await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "指标线挂起不得改变会话语义"
        );
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
        let (status, _) = request_with_headers(&router, "GET", "/metrics", &HeaderMap::new()).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "认证门禁先于并发闸，不受挂起影响"
        );
        for task in pending {
            task.abort();
        }
        // 无新增池构造、无 detach：源码级复核。
        let production = include_str!("../src/production.rs");
        assert!(!production.contains("detach"), "指标线不得把关闭甩到后台");
        assert_eq!(
            production.matches("MySqlPoolOptions::new").count(),
            0,
            "production.rs 不得新建连接池"
        );
    }
}

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
    use rsetup_controller::auth::service::{IdentityUser, Login, Session, SessionPage};
    use rsetup_controller::auth::session::token_digest;
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
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::Semaphore;

    const HOST: &str = "controller.test";
    /// 合法设备 ID 形状（64 位小写 hex）。
    const DEVICE_STATUS_PATH: &str =
        "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001/status";
    /// synthetic 游标密钥：仅用于把生产 `SqlxTaskApi` 立起来，本文件不签发任何游标。
    const SYNTHETIC_CURSOR_SECRET: [u8; 32] = [7u8; 32];
    /// 合法 live session 的会话令牌：64 位**小写 hex**，是 `session_cookie_value`
    /// 严格 cookie 门禁唯一放行的形状（§7.6 第 7 条要求"完整合法会话 Cookie"——
    /// 非 hex 占位符只能证明匿名请求被拒，证明不了合法会话不被抓取污染）。
    const LIVE_SESSION_TOKEN: &str =
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const LIVE_SESSION_USERNAME: &str = "alice-metrics-isolation";
    const LIVE_SESSION_USER_ID: [u8; 16] = [7u8; 16];
    /// `user_public` 期望投影的 canonical UUID：取值**独立于桩对象**写出（不从
    /// `LIVE_SESSION_USER_ID` 现场派生），避免"用实现验证实现"。
    const LIVE_SESSION_USER_UUID: &str = "07070707-0707-0707-0707-070707070707";
    /// 业务侧单次调用的超时护栏：并发闸占满期间业务请求必须**快速**返回；一旦超时
    /// 直接失败，绝不让测试永久挂起。
    const BUSINESS_CALL_BUDGET: Duration = Duration::from_secs(5);

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

    /// 合法 live session 认证桩（§7.6 第 7 条要求的 `LiveSessionAuth` 形态）：
    /// `is_live_session` 只对 `LIVE_SESSION_TOKEN` 的 digest 返回 true，
    /// `authenticate` 只认同同一枚令牌并返回有效 `Session`，`authz_epoch` 返回固定
    /// epoch——三者合起来让 `/api/v1/auth/me` 真正走到 200，从而**先证明会话合法**，
    /// 再证明指标抓取对它零影响。它只在内存里持有"这条会话有效"这一个事实，不连
    /// 任何仓储，也**不**代表真实认证语义。
    struct LiveSessionAuth {
        digest: [u8; 32],
    }

    impl LiveSessionAuth {
        fn new() -> Self {
            Self {
                digest: token_digest(LIVE_SESSION_TOKEN.as_bytes()),
            }
        }

        fn session(&self) -> Session {
            Session {
                user: IdentityUser {
                    id: LIVE_SESSION_USER_ID,
                    username: LIVE_SESSION_USERNAME.into(),
                    password_hash: "$argon2id$=synthetic-test-only=".into(),
                    active: true,
                    is_admin: false,
                    must_change_password: false,
                    revision: 1,
                },
                digest: self.digest,
            }
        }
    }

    impl AuthServiceApi for LiveSessionAuth {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
            let session = self.session();
            Box::pin(async move {
                if raw == LIVE_SESSION_TOKEN {
                    Ok(session)
                } else {
                    Err(ControllerError::InvalidArgument)
                }
            })
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
            Box::pin(async { Ok(7) })
        }
        fn is_live_session(&self, digest: &[u8; 32]) -> bool {
            digest == &self.digest
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

    fn security_config() -> HttpSecurityConfig {
        HttpSecurityConfig {
            allowed_hosts: vec![HOST.into()],
            allowed_origin: Some("http://controller.test".into()),
            trust_tls: false,
        }
    }

    fn auth_state() -> Arc<HttpAuthState> {
        Arc::new(HttpAuthState::new(
            Arc::new(NoSessionAuth),
            security_config(),
        ))
    }

    /// 带**合法 live session** 的共享认证上下文：`/api/v1/auth/me` 携
    /// `LIVE_SESSION_TOKEN` 能拿到 200，用于 §7.6 第 7 条的会话隔离闭环。
    fn live_auth_state() -> Arc<HttpAuthState> {
        Arc::new(HttpAuthState::new(
            Arc::new(LiveSessionAuth::new()),
            security_config(),
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

    /// 永久挂起的合成源（§7.6 第 10 条）+ **确定性同步面**，让"并发闸已占满"成为
    /// 可等待、可断言的前置条件，而不是 `sleep` 猜出来的时序：
    ///
    /// - `entered`：真正进入 `snapshot()` 的请求数——"闸被占满"的唯一直接证据；
    /// - `all_entered`：0 许可信号量，第 `MAX_CONCURRENT_SCRAPE` 路进入时发放恰好 1 枚
    ///   许可；测试 `acquire()` 成功即证明 4 路全部落在采集内部、许可已全部耗尽；
    /// - `release`：0 许可信号量。测试 `add_permits` 放行后，已挂起与其后进入的调用都
    ///   立即返回——许可是**持久状态**，不存在 `Notify::notify_waiters` 那种"先通知后
    ///   等待"的丢唤醒竞态。
    ///
    /// 刻意不用 `tokio::sync::Barrier`：其 `wait()` 明确**不是** cancel-safe，测试加
    /// 超时兜底后可能在超时留下不确定状态；`Semaphore::acquire()` 是 cancel-safe 的，
    /// 因此等待既能在"实现退化为进不满 4 路"时**直接失败**（而非永久挂起），又不引入
    /// 取消后的状态污染。
    struct HangingMetricsSnapshotSource {
        entered: AtomicUsize,
        all_entered: Semaphore,
        release: Semaphore,
    }

    impl HangingMetricsSnapshotSource {
        fn new() -> Self {
            Self {
                entered: AtomicUsize::new(0),
                all_entered: Semaphore::new(0),
                release: Semaphore::new(0),
            }
        }

        /// 已进入 `snapshot()` 的请求数。
        fn entered(&self) -> usize {
            self.entered.load(Ordering::SeqCst)
        }

        /// 等待"并发闸已被 `MAX_CONCURRENT_SCRAPE` 路在途抓取完全占满"。
        async fn wait_until_full(&self) {
            let _ = tokio::time::timeout(BUSINESS_CALL_BUDGET, self.all_entered.acquire())
                .await
                .expect("并发闸必须在预算内被 MAX_CONCURRENT_SCRAPE 路抓取完全占满")
                .expect("同步信号量不会关闭");
        }

        /// 放行全部挂起的采集：多给 1 枚许可，供测试收尾时再发一次抓取验证闸位恢复。
        fn release_all(&self) {
            self.release.add_permits(MAX_CONCURRENT_SCRAPE + 1);
        }
    }

    impl MetricsSnapshotSource for HangingMetricsSnapshotSource {
        fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
            let entered = self.entered.fetch_add(1, Ordering::SeqCst) + 1;
            if entered == MAX_CONCURRENT_SCRAPE {
                self.all_entered.add_permits(1);
            }
            Box::pin(async move {
                let _ = self.release.acquire().await;
                Err(ControllerError::Config(
                    "synthetic hanging source released".into(),
                ))
            })
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
        let (status, _, bytes) = request_returning_headers(router, method, uri, headers).await;
        (status, bytes)
    }

    /// 与 [`request_with_headers`] 同源，但把**响应头**一并交出：§7.6 第 7 条的隔离
    /// 断言必须能观察 `Set-Cookie` 是否出现，且不做任何 JSON 解析（指标体不是 JSON）。
    async fn request_returning_headers(
        router: &Router<()>,
        method: &str,
        uri: &str,
        headers: &HeaderMap,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
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
        let response_headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 262_144).await.unwrap();
        (status, response_headers, bytes.to_vec())
    }

    /// 给业务侧调用套统一超时护栏：并发闸占满期间，业务请求一旦被拖住就直接失败，
    /// 而不是让整个测试套件挂到 CI 超时。
    async fn within_business_budget<T>(what: &str, fut: impl Future<Output = T>) -> T {
        tokio::time::timeout(BUSINESS_CALL_BUDGET, fut)
            .await
            .unwrap_or_else(|_| panic!("业务调用 {what} 在 {BUSINESS_CALL_BUDGET:?} 内未返回"))
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
        // §7.6 第 7 条：在全量顶层 router 上用**一条真正合法的 live session** 走完
        // 四步闭环，缺一即伪阳性：
        // 1. 携合法 `rsc_session` 打业务线 → 200 且拿到合法用户（先证明这条会话真的
        //    有效：Cookie 形状与语义都合法，不是"匿名被拒"的替身）；
        // 2. 同一 Cookie 打 `/metrics`（不带 Basic）→ 401 且无 `Set-Cookie`（会话不是
        //    指标线的凭据，指标线也不下发 Cookie）；
        // 3. Basic 凭据 + 同一 Cookie 抓取成功 → 200 且响应头**绝不**含 `Set-Cookie`
        //    （抓取不建会话、不轮换 Cookie）；
        // 4. 原有 Cookie 再打业务线 → 仍 200 且身份与 CSRF 逐字段不变（抓取对活跃
        //    会话零副作用，没有污染会话/CSRF/限速状态）。
        // 注意：401 只在**启用态**成立（禁用态按 §4.5 恒 404、与路径不存在不可
        // 区分），因此本用例显式传入 metrics_inputs(true)，不套薄壳 inputs()。
        let shared = live_auth_state();
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

        // 同一份请求头既打业务线（`/api/v1/auth/me` 要求唯一且在允许名单内的 Host），
        // 也打指标线（指标线不读 Cookie、也不看 Host）。
        let cookie = HeaderValue::from_str(&format!("rsc_session={LIVE_SESSION_TOKEN}"))
            .expect("64 位 hex 必成合法头值");
        let mut session_headers = HeaderMap::new();
        session_headers.insert(header::HOST, HeaderValue::from_static(HOST));
        session_headers.insert(header::COOKIE, cookie.clone());

        // 1. 合法会话在业务线必须是 200，且投影出完整用户信息。
        let (status, _, body) =
            request_returning_headers(&router, "GET", "/api/v1/auth/me", &session_headers).await;
        let first_text = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(
            status,
            StatusCode::OK,
            "合法 live session 在业务线必须 200（否则后续 401 只是匿名被拒）：{first_text}"
        );
        let before: Value = serde_json::from_slice(&body).expect("业务线必须返回 JSON 信封");
        assert_eq!(
            before["data"]["user"],
            json!({
                "id": LIVE_SESSION_USER_UUID,
                "username": LIVE_SESSION_USERNAME,
                "active": true,
                "is_admin": false,
                "must_change_password": false,
                "revision": "1",
            }),
            "合法会话必须投影出完整用户信息：{first_text}"
        );
        assert!(
            before["data"]["csrf_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64),
            "合法会话必须拿到 64 位 hex CSRF 绑定：{first_text}"
        );
        assert!(
            !first_text.contains("argon2"),
            "响应绝不回显口令哈希材料：{first_text}"
        );

        // 2. 同一 Cookie 打指标线：会话不是凭据 → 401，且不下发任何 Cookie。
        let (status, headers, body) =
            request_returning_headers(&router, "GET", "/metrics", &session_headers).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "合法会话 Cookie 不是指标线凭据，必须 401"
        );
        assert!(body.is_empty(), "指标线 401 响应体必须为空");
        assert!(
            !headers.contains_key(header::SET_COOKIE),
            "指标线 401 绝不下发 Set-Cookie"
        );

        // 3. Basic 凭据 + 同一合法会话 Cookie：按指标线语义 200，且不建会话。
        let mut scrape_headers = authorized_metrics_headers();
        scrape_headers.insert(header::COOKIE, cookie);
        let (status, headers, body) =
            request_returning_headers(&router, "GET", "/metrics", &scrape_headers).await;
        let scrape_text = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(
            status,
            StatusCode::OK,
            "携带合法会话 Cookie 不影响 Basic 抓取：{scrape_text}"
        );
        assert!(
            scrape_text.contains("rsetup_controller_devices_total{admission=\"APPROVED\"} 3"),
            "抓取的读数必须来自注入的合成源：{scrape_text}"
        );
        assert!(
            !headers.contains_key(header::SET_COOKIE),
            "成功抓取绝不下发 Set-Cookie（指标线不建会话）"
        );

        // 4. 抓取之后原会话仍然有效，且身份/CSRF/epoch 逐字段不变。
        let (status, _, body) =
            request_returning_headers(&router, "GET", "/api/v1/auth/me", &session_headers).await;
        let after_text = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(
            status,
            StatusCode::OK,
            "指标抓取必须对现有活跃会话零副作用：{after_text}"
        );
        let after: Value = serde_json::from_slice(&body).expect("业务线必须返回 JSON 信封");
        assert_eq!(
            after["data"]["user"], before["data"]["user"],
            "身份投影必须逐字段一致"
        );
        assert_eq!(
            after["data"]["csrf_token"], before["data"]["csrf_token"],
            "同一 digest 的 CSRF 绑定必须被复用：抓取既不得清除它，也不得轮换它"
        );
        assert_eq!(
            after["data"]["authz_epoch"], before["data"]["authz_epoch"],
            "授权 epoch 读数不随抓取变化"
        );
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

    /// 闸位恢复的**有限重试**次数：许可随采集任务结束才 Drop，`join` 与 Drop 之间
    /// 存在调度窗口，因此放行后的首次抓取可能仍撞上"上一路许可尚未归还"。这里用
    /// cancel-safe 的 `yield_now` 让出调度权，而不是 `sleep` 猜时序。
    const GATE_RECOVERY_ATTEMPTS: usize = 1_000;

    #[tokio::test]
    async fn metrics_timeout_does_not_starve_business_routes() {
        // L3 侧的**结构**部分：合成源永久挂起把并发闸**确定性占满**，业务线仍必须
        // 快速返回。真实慢查询下的池可用性不在此证明范围（§7.7 L4，Task 7）。
        // 四步闭环，缺一即伪阳性：
        // 1. 4 路带凭据抓取进入挂起源 → `wait_until_full()` 等到"闸已被 4 路完全
        //    占满"这一事实成立（`sleep` 猜不出这个前置条件）；
        // 2. 第 5 路带凭据抓取 → 立即 503，证明闸真的满了（否则后面三条业务断言
        //    什么都没证明）；
        // 3. 闸满期间 `/healthz`、合法会话的 `/api/v1/auth/me`、无凭据 `/metrics`
        //    都在业务预算内返回，且无凭据仍是 401 而不是 503——认证门禁先于并发闸；
        // 4. `release_all()` 放行 4 路并 join，杜绝任务泄漏，再证明闸位恢复。
        let shared = live_auth_state();
        let hanging = Arc::new(HangingMetricsSnapshotSource::new());
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &lazy_db(),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            hanging.clone(),
        )
        .expect("装配必须成功");

        // 1. 恰好 `MAX_CONCURRENT_SCRAPE` 路带有效凭据的抓取：闸满前全部进入采集。
        let pending: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let router = router.clone();
                let headers = authorized_metrics_headers();
                tokio::spawn(async move {
                    request_with_headers(&router, "GET", "/metrics", &headers).await
                })
            })
            .collect();
        hanging.wait_until_full().await;
        assert_eq!(
            hanging.entered(),
            MAX_CONCURRENT_SCRAPE,
            "并发闸必须由且仅由 {MAX_CONCURRENT_SCRAPE} 路在途抓取占满"
        );

        // 2. 第 5 路：认证通过后取不到许可 → 立即 503（不排队），闸满这一前置条件
        //    由此成为被断言的事实，而不是测试自己的假设。
        let (status, _) = within_business_budget(
            "GET /metrics（第 5 路带凭据）",
            request_with_headers(&router, "GET", "/metrics", &authorized_metrics_headers()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "闸被 {MAX_CONCURRENT_SCRAPE} 路占满后第 5 路必须立即 503"
        );

        // 3. 业务线未被饥饿：全部套超时护栏，一旦被拖住直接失败而非挂到 CI 超时。
        let (status, _) =
            within_business_budget("GET /healthz", request_get(&router, "/healthz")).await;
        assert_eq!(status, StatusCode::OK, "指标线挂起不得拖住探活");

        // 合法 live session 仍走通业务认证线并拿到 200：闸满期间业务线不只是"有响应
        // 形状"，而是**真的服务完成**。
        let cookie = HeaderValue::from_str(&format!("rsc_session={LIVE_SESSION_TOKEN}"))
            .expect("64 位 hex 必成合法头值");
        let mut session_headers = HeaderMap::new();
        session_headers.insert(header::HOST, HeaderValue::from_static(HOST));
        session_headers.insert(header::COOKIE, cookie);
        let (status, _, body) = within_business_budget(
            "GET /api/v1/auth/me",
            request_returning_headers(&router, "GET", "/api/v1/auth/me", &session_headers),
        )
        .await;
        let text = String::from_utf8_lossy(&body).into_owned();
        assert_eq!(
            status,
            StatusCode::OK,
            "闸满期间合法会话必须正常进入并完成业务认证线：{text}"
        );
        let me: Value = serde_json::from_slice(&body).expect("业务认证线必须返回 JSON 信封");
        assert_eq!(
            me["data"]["user"]["username"],
            json!(LIVE_SESSION_USERNAME),
            "业务认证线必须投影出该会话的用户：{text}"
        );

        // 无凭据抓取仍是 401：认证门禁（②）先于并发闸（④），挂起的采集改变不了顺序。
        let (status, _) = within_business_budget(
            "GET /metrics（无凭据）",
            request_with_headers(&router, "GET", "/metrics", &HeaderMap::new()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "认证门禁先于并发闸，闸满也不得把无凭据请求变成 503"
        );

        // 4. 放行全部挂起采集并 join：绝不用 `abort()` 掩盖任务泄漏。
        hanging.release_all();
        for handle in pending {
            let (status, _) = within_business_budget("在途抓取收尾", handle)
                .await
                .unwrap();
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "合成源放行后返回 Err，按 §4.5 映射 503"
            );
        }

        // 闸位恢复：再发一次有效抓取必须能**重新进入采集**。放行后源立即返回 Err，
        // 采集失败与闸满都是 503，唯一可靠证据是 `entered()` 是否增长。
        let entered_before = hanging.entered();
        let mut recovered = false;
        for _ in 0..GATE_RECOVERY_ATTEMPTS {
            let (status, _) = within_business_budget(
                "GET /metrics（放行后复抓）",
                request_with_headers(&router, "GET", "/metrics", &authorized_metrics_headers()),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "放行后源返回 Err → 503"
            );
            if hanging.entered() > entered_before {
                recovered = true;
                break;
            }
            // 上一路的许可随其采集任务结束才 Drop：让出调度权后重试，不 sleep 猜时序。
            tokio::task::yield_now().await;
        }
        assert!(
            recovered,
            "放行后并发闸必须恢复可用（{GATE_RECOVERY_ATTEMPTS} 次尝试内没有一路重新进入采集）"
        );
        assert_eq!(
            hanging.entered(),
            entered_before + 1,
            "闸位恢复后恰好一路重新进入采集"
        );

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

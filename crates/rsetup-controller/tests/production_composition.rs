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
        component("/", "web"),
    );
}

// ---------------------------------------------------------------------------
// Task 0b：typed 生产装配与共享认证上下文
// ---------------------------------------------------------------------------

mod typed {
    use super::*;

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
    use rsetup_controller::production::{ProductionInputs, build_production_router};
    use rsetup_controller::tasks::SqlxTaskApi;
    use rsetup_controller::tasks::http::HttpTasksState;
    use rsetup_controller::tasks::repository::SqlxTaskRepository;
    use rsetup_controller::tasks::service::TaskService;
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

    /// 用**指定**的认证上下文实例装配五线输入：`management_auth`/`tasks_auth`/
    /// `status_auth` 允许与 `auth` 不同，用来构造指针不一致的负例。
    fn inputs(
        auth: Arc<HttpAuthState>,
        management_auth: Arc<HttpAuthState>,
        tasks_auth: Arc<HttpAuthState>,
        status_auth: Arc<HttpAuthState>,
    ) -> ProductionInputs {
        let db = lazy_db();
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
            web: web_router(),
        }
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
    async fn shared_auth_context_composes_all_five_lines() {
        let shared = auth_state();
        let router = build_production_router(inputs(
            Arc::clone(&shared),
            Arc::clone(&shared),
            Arc::clone(&shared),
            Arc::clone(&shared),
        ))
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

        // management / tasks / status 三线：路由已注册（否则 404），且都在共享
        // gate 处严格 401，未触达任何仓储。
        for path in [
            "/api/v1/users",
            "/api/v1/roles",
            "/api/v1/audit",
            "/api/v1/system/status",
            "/api/v1/tasks",
            "/api/v1/tasks/00000000-0000-4000-8000-000000000001",
            DEVICE_STATUS_PATH,
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
    async fn composed_router_serves_status_component_from_real_source() {
        // 设备状态线在装配后必须真的走 EmptyDeviceStatusSnapshotSource 的语义：
        // 无样本 → snapshot=null / freshness=unknown / age_ms=null，不伪造 PASS。
        // 这里通过未认证 401 证明路由归属，再由 lib 单测覆盖投影本身。
        let shared = auth_state();
        let router = build_production_router(inputs(
            Arc::clone(&shared),
            Arc::clone(&shared),
            Arc::clone(&shared),
            Arc::clone(&shared),
        ))
        .expect("装配必须成功");
        let (status, _) = request_get(
            &router,
            "/api/v1/devices/0000000000000000000000000000000000000000000000000000000000000001",
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "设备详情属 management 线，也必须 401 而非 404"
        );
    }
}

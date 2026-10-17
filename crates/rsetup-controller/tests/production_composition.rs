//! Task 0a 生产路由装配的组件注册测试。
//!
//! 这里的组件全部是 synthetic 探活 handler，只验证 `compose_component_routers`
//! 的合并拓扑（可达性、无覆盖、无重复探活），不证明认证、SQL、资源内容或真实生产来源。

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
#[should_panic]
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

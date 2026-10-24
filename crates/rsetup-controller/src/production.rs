//! 生产路由装配（Task 0a 纯拓扑合并 + Task 0b typed 生产装配 + 指标线六线合并）。
//!
//! 本模块是中控七线（auth / management / tasks / status / events / metrics / web）
//! **唯一**的顶层装配点：
//! - [`compose_component_routers`] 做纯拓扑合并 **+ 统一基础安全响应头层**（只补
//!   `X-Content-Type-Options: nosniff` 与 `Referrer-Policy: no-referrer`，既有同名头
//!   一律不覆盖，不改路由集合、状态码与错误信封）；不额外注册探活路径，路径集合
//!   归各条线所有，组件合并顺序无业务语义；
//! - [`build_production_router`] 接收各条线已构造好的 typed 状态，先强制校验原四线
//!   共享**同一个** `Arc<HttpAuthState>`，再合并。指标线**没有** `HttpAuthState`，
//!   不参与也无法违反该校验（I3：独立认证边界）。
//!
//! 指针一致性是硬门禁（保持原样，只覆盖原四线）。指标线的连接池唯一性由**结构性
//! 收缩 + 契约 + 静态门禁**保证：`MetricsLineInputs` 里既没有 `DbPool` 也没有汇总源，
//! 两者都只能在装配函数内部由 `inputs.auth.db` 派生（I2）。诚实界定：这是装配约定
//! 加上 §7.6 第 9 条的源码级回归与 §7.8 的 grep 静态门禁共同执法的**架构契约**，
//! 不是"编译期不可能违反"的绝对结论。

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::header::{REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;

use crate::ControllerError;
use crate::api::{ManagementState, build_management_router};
use crate::http_auth::{self, AppState};
use crate::http_device_status::{DeviceStatusState, build_device_status_router};
use crate::http_events::{EventsState, build_events_router};
use crate::metrics::collect::{
    MAX_CONCURRENT_SCRAPE, MetricsRuntime, MetricsSnapshotSource, SqlxMetricsSnapshotSource,
};
use crate::metrics::config::MetricsConfig;
use crate::metrics::http::{MetricsState, build_metrics_router};
use crate::tasks::http::{HttpTasksState, build_tasks_router};

/// 指标线的**全部**外部装配输入：配置与运行时锚点。
///
/// 字段集合**只有这两项**（§7.6 第 9 条用写全字段的构造点把它钉死）：汇总源与池
/// 读数都在 `build_production_router` 内部由 `inputs.auth.db` 派生，外部既递不进
/// `DbPool`，也递不进自建的汇总源。
pub struct MetricsLineInputs {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
}

/// 生产顶层 router 的全部 typed 输入：七线状态各由所属条线的生产适配器构造，
/// 本模块不构造任何默认源、测试网关或占位池。
pub struct ProductionInputs {
    /// Identity 认证线（含 `/healthz`、`/readyz` 探活）。
    pub auth: AppState,
    /// Identity 管理线（26 条管理路由）。
    pub management: ManagementState,
    /// Runtime 任务线（8 条任务路由）。
    pub tasks: HttpTasksState,
    /// 设备状态读线（1 条状态路由）。
    pub status: DeviceStatusState,
    /// SSE 事件读线（1 条 `GET /api/v1/events` 路由 + 事件总线订阅面）。
    ///
    /// 其 `bus` 与生产进程内唯一的 [`crate::events::EventBus`] 是同一通道句柄；各条线的发布方
    /// 通过同一个句柄投递已接线的任务与管理事件；心跳与 reset 由 SSE 读线自身处理，
    /// 不把尚未接线的业务变化伪造为已发布事件。
    pub events: EventsState,
    /// Prometheus 指标线（1 条路由，独立 Basic 认证边界）。
    pub metrics: MetricsLineInputs,
    /// Web 嵌入静态资源线。
    pub web: Router<()>,
}

/// 唯一生产装配入口：校验共享认证上下文指针一致后合并七线子 router。
///
/// 真实汇总源只能在本函数内部由 `auth.db` 构造（I2）；不一致时返回去敏固定
/// `Config`（不回显任何指针/地址值），进程应在触达监听套接字之前失败关闭。
pub fn build_production_router(inputs: ProductionInputs) -> Result<Router<()>, ControllerError> {
    build_router(inputs, None)
}

/// 仅供 `crates/rsetup-controller/tests/` 的装配测试注入合成汇总源。
/// `#[doc(hidden)]` 且 MUST NOT 被 `main.rs` 或任何生产路径调用（§7.8 第 6 条 grep
/// 门禁）。它接收的是 `Arc<dyn MetricsSnapshotSource>`：类型里没有 `DbPool` 只是
/// **收缩违规面**，MUST NOT 据此宣称"该缝在类型上绝不可能引入第二池"。无论走哪个
/// 入口，`MetricsState.pool` 恒为 `inputs.auth.db.clone()`。
#[doc(hidden)]
pub fn build_production_router_with_metrics_source(
    inputs: ProductionInputs,
    snapshot: Arc<dyn MetricsSnapshotSource>,
) -> Result<Router<()>, ControllerError> {
    build_router(inputs, Some(snapshot))
}

/// 唯一实现：`snapshot_override` 只允许测试注入，`None` 即真实源。
fn build_router(
    inputs: ProductionInputs,
    snapshot_override: Option<Arc<dyn MetricsSnapshotSource>>,
) -> Result<Router<()>, ControllerError> {
    if !Arc::ptr_eq(&inputs.auth.auth, &inputs.management.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.tasks.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.status.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.events.auth)
    {
        return Err(ControllerError::Config(
            "production auth context mismatch".into(),
        ));
    }
    // 连接池唯一派生点：指标线的池与汇总源都只能来自 inputs.auth.db（I2）。
    let snapshot = snapshot_override
        .unwrap_or_else(|| Arc::new(SqlxMetricsSnapshotSource::new(inputs.auth.db.clone())));
    let metrics_state = MetricsState {
        config: inputs.metrics.config,
        runtime: inputs.metrics.runtime,
        snapshot,
        pool: inputs.auth.db.clone(),
        guard: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SCRAPE)),
    };
    let metrics = build_metrics_router(metrics_state);
    let auth = inputs.auth; // 在指标线借用 db 之后再移交给认证线。
    Ok(compose_component_routers(
        http_auth::build_http_router(auth),
        build_management_router(inputs.management),
        build_tasks_router(inputs.tasks),
        build_device_status_router(inputs.status),
        build_events_router(inputs.events),
        metrics,
        inputs.web,
    ))
}

/// 把 auth / management / tasks / status / events / metrics / web 七个组件子 router
/// 合并为顶层路由。`metrics` 固定排在 `events` 之后、`web` 之前（web 含 SPA 兜底，
/// 必须最后合并）。
///
/// 合并结果统一挂 [`add_base_security_headers`] 后置头层：未匹配任何路由的请求由
/// axum 默认 fallback 直接返回，不经过任何条线（也不经过 web.rs 的 `serve_static`），
/// 该层保证这类响应同样带上基础安全头。
///
/// **诚实界定作用域**：本层只覆盖**经本函数产出的顶层 Router** 的响应。测试里单独
/// 调用 `build_metrics_router` / `build_http_router` 等单条线 router 时，安全头仍由
/// 各条线自己负责（metrics 自带 `nosniff`，web.rs 自带 CSP/XFO）。
pub fn compose_component_routers(
    auth: Router<()>,
    management: Router<()>,
    tasks: Router<()>,
    status: Router<()>,
    events: Router<()>,
    metrics: Router<()>,
    web: Router<()>,
) -> Router<()> {
    auth.merge(management)
        .merge(tasks)
        .merge(status)
        .merge(events)
        .merge(metrics)
        .merge(web)
        // 只加一层后置头，不改路由集合、不改状态码、不改错误信封。
        .layer(axum::middleware::from_fn(add_base_security_headers))
}

/// 顶层统一后置安全头中间件。
///
/// 未匹配路由（如 `/nope`、`/api/v1/not-found`、不存在的 `/assets/*`）由 axum `Router`
/// 的默认 fallback 返回，既不进 web.rs 的 `serve_static`，也不进任何条线的中间件，
/// 头集合为空。本中间件挂在 [`compose_component_routers`] 的合并结果上，对**所有经过
/// 顶层 Router 的响应**补齐：
/// - `X-Content-Type-Options: nosniff`
/// - `Referrer-Policy: no-referrer`
///
/// 覆盖规则：只在响应**尚未设置**同名头时插入（`HeaderMap::entry(..).or_insert(..)`），
/// 绝不覆盖已有值——metrics 线自己设定的 `nosniff`、web.rs HTML 响应设定的 CSP/XFO、
/// 任何条线已设定的 `Cache-Control`/`Content-Type` 全部保持不变。
async fn add_base_security_headers(request: Request<Body>, next: Next) -> Response {
    let response = next.run(request).await;
    insert_base_security_headers(response)
}

/// 为响应补齐基础安全头；已存在的同名头保持不变。
fn insert_base_security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers
        .entry(X_CONTENT_TYPE_OPTIONS)
        .or_insert_with(|| HeaderValue::from_static("nosniff"));
    headers
        .entry(REFERRER_POLICY)
        .or_insert_with(|| HeaderValue::from_static("no-referrer"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{HeaderMap, Method, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt;

    /// 指定路径的最小占位组件：body 就是标记串，便于断言命中的是哪条线。
    fn component(path: &str, marker: &'static str) -> Router<()> {
        Router::new().route(path, get(move || async move { marker }))
    }

    /// 七线最小合成合并：不连数据库、不构造真实 auth。
    fn composed() -> Router<()> {
        compose_component_routers(
            component("/healthz", "auth-health"),
            component("/api/v1/devices", "management-devices"),
            component("/api/v1/tasks", "tasks-list"),
            component("/api/v1/device-status", "status-read"),
            component("/api/v1/events", "events-stream"),
            component("/metrics", "metrics-scrape"),
            Router::new(),
        )
    }

    /// 对顶层 router 发一次请求，返回（状态码，响应头，响应体）。
    async fn request(
        router: Router<()>,
        method: Method,
        uri: &str,
    ) -> (StatusCode, HeaderMap, String) {
        let response = router
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .expect("构造测试请求"),
            )
            .await
            .expect("router 不返回错误");
        let status = response.status();
        let headers = response.headers().clone();
        let body = String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("读取响应体")
                .to_vec(),
        )
        .expect("UTF-8 响应体");
        (status, headers, body)
    }

    fn header(headers: &HeaderMap, name: &str) -> Option<String> {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    /// 基础断言：两个安全头都在且是约定值。
    fn assert_base_headers(headers: &HeaderMap, uri: &str) {
        assert_eq!(
            header(headers, "x-content-type-options").as_deref(),
            Some("nosniff"),
            "{uri} 应带 X-Content-Type-Options: nosniff"
        );
        assert_eq!(
            header(headers, "referrer-policy").as_deref(),
            Some("no-referrer"),
            "{uri} 应带 Referrer-Policy: no-referrer"
        );
    }

    /// 未匹配路径：状态码语义不变（404），但必须带上两个基础安全头。
    #[tokio::test]
    async fn unmatched_paths_return_404_with_base_security_headers() {
        for uri in ["/nope", "/api/v1/not-found", "/assets/does-not-exist.js"] {
            let (status, headers, _) = request(composed(), Method::GET, uri).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri} 的 404 语义不得改变");
            assert_base_headers(&headers, uri);
        }
    }

    /// 七线全空的极端合并也必须带基础头（证明该层不依赖任何条线是否存在）。
    #[tokio::test]
    async fn empty_composition_returns_404_with_base_security_headers() {
        let router = compose_component_routers(
            Router::new(),
            Router::new(),
            Router::new(),
            Router::new(),
            Router::new(),
            Router::new(),
            Router::new(),
        );
        let (status, headers, _) = request(router, Method::GET, "/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_base_headers(&headers, "/nope");
    }

    /// 命中的探活与 API 响应同样带两个头，且响应体归属不变。
    #[tokio::test]
    async fn matched_probe_and_api_responses_carry_base_security_headers() {
        for (uri, marker) in [
            ("/healthz", "auth-health"),
            ("/api/v1/devices", "management-devices"),
            ("/api/v1/tasks", "tasks-list"),
            ("/metrics", "metrics-scrape"),
        ] {
            let (status, headers, body) = request(composed(), Method::GET, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert_eq!(body, marker, "{uri} 的归属不得改变");
            assert_base_headers(&headers, uri);
        }
    }

    /// 方法不匹配（405）同样带基础头，状态码语义不变。
    #[tokio::test]
    async fn unmatched_method_returns_405_with_base_security_headers() {
        let (status, headers, _) = request(composed(), Method::DELETE, "/healthz").await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
        assert_base_headers(&headers, "DELETE /healthz");
    }

    /// 同名头与非安全头（Cache-Control / Content-Type / CSP / XFO）都不得被覆盖。
    #[tokio::test]
    async fn preexisting_headers_are_never_overwritten() {
        let router = compose_component_routers(
            Router::new().route(
                "/markers",
                get(|| async {
                    Response::builder()
                        .status(StatusCode::IM_A_TEAPOT)
                        .header("x-content-type-options", "do-not-touch")
                        .header("referrer-policy", "same-origin")
                        .header("cache-control", "no-store, max-age=0")
                        .header("content-type", "application/vnd.custom+json")
                        .header("content-security-policy", "component-owned-csp")
                        .header("x-frame-options", "SAMEORIGIN")
                        .body(Body::from("marker"))
                        .expect("构造带自定义头的响应")
                }),
            ),
            Router::new(),
            Router::new(),
            Router::new(),
            Router::new(),
            Router::new(),
            Router::new(),
        );
        let (status, headers, body) = request(router, Method::GET, "/markers").await;
        assert_eq!(status, StatusCode::IM_A_TEAPOT);
        assert_eq!(body, "marker");
        for (name, expected) in [
            ("x-content-type-options", "do-not-touch"),
            ("referrer-policy", "same-origin"),
            ("cache-control", "no-store, max-age=0"),
            ("content-type", "application/vnd.custom+json"),
            ("content-security-policy", "component-owned-csp"),
            ("x-frame-options", "SAMEORIGIN"),
        ] {
            assert_eq!(
                header(&headers, name).as_deref(),
                Some(expected),
                "{name} 已有值不得被覆盖"
            );
        }
    }
}

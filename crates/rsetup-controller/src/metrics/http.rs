//! 指标线路由与 handler（规范 §6.2/§4.5）。
//!
//! handler 内部顺序 MUST 固定为：①`enabled` 门禁 → ②Basic 认证 → ③`HEAD` 短路
//! （200 空体，不取闸、不采集）→ ④并发闸 `try_acquire_owned`（`OwnedPermit` 移交
//! 采集任务持有，直到连接完全关闭才释放，§5.5.1）→ ⑤快照采集 → ⑥渲染与上限校验
//! → ⑦响应。HTTP 方法匹配由路由层在此之前完成；`HEAD` 之外没有第二种方法能进入本
//! handler（`MethodRouter::get` 把 `["GET", "HEAD"]` 指向同一 handler，且成功的 200
//! 响应不携带 `Allow`——`Allow` 只由 405 fallback 挂载）。
//!
//! 本模块 MUST NOT 写日志（含 401）：抓取的正常节奏会淹没日志，失败已可由 Prometheus
//! 的 `up == 0` 与 `scrape_duration_seconds` 观测（§6.2 第 6 条）。

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::{Semaphore, oneshot};

use crate::DbPool;
use crate::metrics::auth::{parse_authorization, verify, www_authenticate};
use crate::metrics::collect::{MetricsRuntime, MetricsSnapshotSource, pool_counts};
use crate::metrics::config::MetricsConfig;
use crate::metrics::render::{CONTENT_TYPE, render};

/// 指标线状态。**没有**认证上下文、会话边界类型与审计/限速状态（I3），因此这里在
/// 类型上就不可能触达会话边界；`pool` 只用于 §5.3 第 5 条的内存读取（`size()` /
/// `num_idle()`），MUST NOT 用它发起查询。
#[derive(Clone)]
pub struct MetricsState {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
    pub snapshot: Arc<dyn MetricsSnapshotSource>,
    pub pool: DbPool,
    pub guard: Arc<Semaphore>,
}

/// 指标线子 router：按配置路径注册恰好 1 条 GET 路由（Axum 0.8 隐含同时接受 HEAD）。
///
/// MUST 始终注册（含 `enabled = false` 时），由 handler 依 §4.5 顺序返回 404：这样
/// 禁用态响应与路径配置项完全无关，也不受后续合并顺序或 SPA 白名单演变影响。
/// MUST NOT 用独立的 `.head(handler)` 注册第二条 handler——单 handler + `Method`
/// 分支才能保证两个方法的认证与门禁不会各自漂移。
pub fn build_metrics_router(state: MetricsState) -> Router<()> {
    let endpoint = state.config.endpoint.clone();
    Router::new()
        .route(&endpoint, get(scrape))
        .with_state(state)
}

/// 401：固定挑战值 + 空体 + 无 `Content-Type`（MUST NOT 复用业务 JSON 信封，§4.4）。
///
/// 三个响应头都写成 `(HeaderName, String)`：数组元素类型必须同构，而挑战值由
/// `www_authenticate()` 动态拼装（非 `'static`），因此键名统一取常量而非字符串字面量。
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (WWW_AUTHENTICATE, www_authenticate()),
            (CACHE_CONTROL, "no-store".to_string()),
            (X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
    )
        .into_response()
}

/// 采集失败（超时、映射未知取值、渲染超上限、闸满）→ 503 空体 + `Retry-After: 5`；
/// 不记日志、不回显错误内容（§4.5）。
fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("retry-after", "5"),
        ],
    )
        .into_response()
}

async fn scrape(State(state): State<MetricsState>, method: Method, headers: HeaderMap) -> Response {
    // ① enabled 门禁：禁用态恒 404 且与"路径不存在"不可区分（无 WWW-Authenticate、
    //    无 Content-Type、无 Cache-Control），无论是否携带凭据。
    if !state.config.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    // ② Basic 认证：解析失败或不匹配一律同一形状 401（不区分原因，不写日志）。
    //    配置门禁保证启用态必有凭据（§3.6 分支 5）；缺失只能是装配错误 → fail-closed。
    let Some(credentials) = state.config.credentials.as_ref() else {
        return unavailable();
    };
    let Some(parsed) = parse_authorization(&headers) else {
        return unauthorized();
    };
    if !verify(&parsed.username, &parsed.password, credentials) {
        return unauthorized();
    }
    // ③ HEAD 短路：认证通过后 200 空体，不取并发闸、不采集（HEAD 的用途是"探活 +
    //    检查认证配置"，为一个被丢弃的 body 支付两次 GROUP BY 是纯粹的可用性风险）。
    if method == Method::HEAD {
        return (
            StatusCode::OK,
            [
                ("content-type", CONTENT_TYPE),
                ("cache-control", "no-store"),
                ("x-content-type-options", "nosniff"),
            ],
        )
            .into_response();
    }
    // ④ 并发闸：闸满立即 503（不排队、不重试），MUST NOT 进入采集。
    let Ok(permit) = state.guard.clone().try_acquire_owned() else {
        return unavailable();
    };
    // ⑤ 采集：许可与采集调用一起移交独立任务，用 oneshot 等结果。许可只在任务返回
    //    （= 连接已完全关闭）时随 Drop 释放；客户端断线只取消本 handler，MUST NOT
    //    取消采集任务、MUST NOT 提前释放许可（§5.5.1 规则 3）。
    let (tx, rx) = oneshot::channel();
    let source = Arc::clone(&state.snapshot);
    tokio::spawn(async move {
        let _permit = permit; // 任务结束（连接已完全关闭）才随 Drop 释放（I7）。
        let _ = tx.send(source.snapshot().await); // handler 已断线时发送失败，取值直接丢弃。
    });
    let Ok(Ok(snapshot)) = rx.await else {
        return unavailable();
    };
    // ⑥ 渲染与上限校验（渲染层内部 fail-closed，这里只映射 503）。
    let values = state
        .runtime
        .scrape_values(&snapshot, pool_counts(&state.pool));
    match render(&values) {
        Ok(body) => (
            StatusCode::OK,
            [
                ("content-type", CONTENT_TYPE),
                ("cache-control", "no-store"),
                ("x-content-type-options", "nosniff"),
            ],
            body,
        )
            .into_response(),
        Err(_) => unavailable(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DbPool;
    use crate::metrics::collect::{
        FakeMetricsSnapshotSource, MAX_CONCURRENT_SCRAPE, MetricsSnapshot,
        SqlxMetricsSnapshotSource,
    };
    use crate::metrics::config::{BasicCredentials, DEFAULT_ENDPOINT};
    use crate::metrics::model::{DeviceCounts, TaskCounts};
    use crate::metrics::render::CONTENT_TYPE;
    use axum::body::{Body, to_bytes};
    use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
    use std::time::{Duration, Instant};
    use tower::ServiceExt;

    const USER: &str = "prom_admin";
    const PASSWORD: &str = "secure_password";

    fn config_with(endpoint: &str, enabled: bool, password: Option<&str>) -> Arc<MetricsConfig> {
        Arc::new(MetricsConfig {
            enabled,
            endpoint: endpoint.to_string(),
            credentials: password.map(|password| BasicCredentials {
                username: USER.to_string(),
                password: password.to_string(),
            }),
        })
    }

    /// §5.6 的 fixture 计数（uptime 由 runtime 锚点决定，因此响应体用**同锚点**重渲染
    /// 后逐字节比对；固定 uptime 的逐字节 golden 由 Task 3 的 L1 用例钉住）。
    fn fixture_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
        }
    }

    struct Harness {
        router: Router<()>,
        runtime: Arc<MetricsRuntime>,
        source: FakeMetricsSnapshotSource,
        pool: DbPool,
        guard: Arc<Semaphore>,
    }

    impl Harness {
        fn enabled() -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn with_password(password: &str) -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(password)),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn disabled() -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, false, None),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn failing(message: &'static str) -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                FakeMetricsSnapshotSource::failing(message),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn with_capacity(capacity: usize, source: FakeMetricsSnapshotSource) -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                source,
                capacity,
            )
        }
        fn with(
            config: Arc<MetricsConfig>,
            source: FakeMetricsSnapshotSource,
            capacity: usize,
        ) -> Self {
            let runtime = Arc::new(MetricsRuntime::new());
            let pool = lazy_pool();
            let guard = Arc::new(Semaphore::new(capacity));
            let router = build_metrics_router(MetricsState {
                config,
                runtime: Arc::clone(&runtime),
                snapshot: Arc::new(source.clone()),
                pool: pool.clone(),
                guard: Arc::clone(&guard),
            });
            Self {
                router,
                runtime,
                source,
                pool,
                guard,
            }
        }
        /// 自定义端点（证明 §4.5 的 404/405 形状只跟配置路径有关）。
        fn at_endpoint(endpoint: &str) -> Self {
            Self::with(
                config_with(endpoint, true, Some(PASSWORD)),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        /// 用外部汇总源替换（`source_timeout_maps_to_503` 走真实源）。
        fn with_source(source: Arc<dyn MetricsSnapshotSource>) -> Self {
            let runtime = Arc::new(MetricsRuntime::new());
            let pool = lazy_pool();
            let guard = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPE));
            let router = build_metrics_router(MetricsState {
                config: config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                runtime: Arc::clone(&runtime),
                snapshot: source,
                pool: pool.clone(),
                guard: Arc::clone(&guard),
            });
            Self {
                router,
                runtime,
                source: FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                pool,
                guard,
            }
        }
        async fn send(&self, method: Method, path: &str, headers: HeaderMap) -> Response {
            let mut builder = Request::builder().method(method).uri(path);
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
            self.router
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .expect("router 不得 panic 或拒绝请求")
        }
        async fn get(&self, path: &str, headers: HeaderMap) -> Response {
            self.send(Method::GET, path, headers).await
        }
        async fn get_ok(&self) -> Response {
            self.get(DEFAULT_ENDPOINT, authorized(USER, PASSWORD)).await
        }
    }

    fn authorized(user: &str, password: &str) -> HeaderMap {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let mut headers = HeaderMap::new();
        let token = STANDARD.encode(format!("{user}:{password}"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {token}")).unwrap(),
        );
        headers
    }

    fn raw_authorization(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    async fn body_of(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 262_144)
            .await
            .unwrap()
            .to_vec()
    }

    /// 头名固定为字面量，因此可直接走零分配的 `from_static`（借用不越过本函数）。
    fn header<'a>(response: &'a Response, name: &'static str) -> Option<&'a str> {
        response
            .headers()
            .get(HeaderName::from_static(name))
            .map(|value| value.to_str().expect("ASCII 响应头"))
    }

    fn lazy_pool() -> DbPool {
        DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(Duration::from_millis(100))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .expect("lazy 池构造不连接"),
        )
    }

    /// 断言响应只带 §5.1 第 8 条允许的三个响应头（200 与 HEAD 形状）。
    fn assert_success_headers(response: &Response) {
        assert_eq!(header(response, "content-type"), Some(CONTENT_TYPE));
        assert_eq!(header(response, "cache-control"), Some("no-store"));
        assert_eq!(
            header(response, "x-content-type-options"),
            Some("nosniff"),
            "§5.1 第 8 条"
        );
        assert!(
            header(response, "allow").is_none(),
            "200 绝不携带 Allow（§6.2）"
        );
        assert!(
            header(response, "set-cookie").is_none(),
            "I3：不写会话 Cookie"
        );
        assert!(header(response, "etag").is_none() && header(response, "vary").is_none());
    }

    // ---------------------------------------------------------------------
    // 200
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn scrape_with_valid_credentials_returns_200() {
        let harness = Harness::enabled();
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_success_headers(&response);
        let text = String::from_utf8(body_of(response).await).expect("UTF-8 文本");
        let expected = render(
            &harness
                .runtime
                .scrape_values(&fixture_snapshot(), pool_counts(&harness.pool)),
        )
        .expect("fixture 必须可渲染");
        assert_eq!(text, expected, "响应体必须与渲染器输出逐字节一致");
        assert!(text.starts_with("# HELP rsetup_controller_build_info "));
        assert!(text.contains("rsetup_controller_devices_total{admission=\"APPROVED\"} 7"));
        assert!(text.contains("rsetup_controller_tasks_total{status=\"succeeded\"} 40"));
        assert!(text.ends_with('\n'));
        assert_eq!(harness.source.calls(), 1);
    }

    /// P1 的端到端回归网：RFC 7617 合法密码不得被解析层或配置层拒绝。
    #[tokio::test]
    async fn scrape_with_password_containing_colons_returns_200() {
        let harness = Harness::with_password("a:b:c");
        let response = harness
            .get(DEFAULT_ENDPOINT, authorized(USER, "a:b:c"))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "content-type"), Some(CONTENT_TYPE));
        assert_eq!(harness.source.calls(), 1);
    }

    // ---------------------------------------------------------------------
    // HEAD 短路
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn head_with_valid_credentials_returns_200_without_collection() {
        let harness = Harness::enabled();
        let response = harness
            .send(Method::HEAD, DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_success_headers(&response);
        assert!(body_of(response).await.is_empty(), "HEAD 响应体必须为空");
        assert_eq!(harness.source.calls(), 0, "HEAD 绝不调用汇总源（§6.2）");
        assert_eq!(
            harness.guard.available_permits(),
            MAX_CONCURRENT_SCRAPE,
            "HEAD 不占并发闸"
        );
    }

    #[tokio::test]
    async fn head_without_credentials_returns_401_like_get() {
        let harness = Harness::enabled();
        let head = harness
            .send(Method::HEAD, DEFAULT_ENDPOINT, HeaderMap::new())
            .await;
        let get = harness.get(DEFAULT_ENDPOINT, HeaderMap::new()).await;
        assert_eq!(head.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(head.status(), get.status());
        assert_eq!(
            header(&head, "www-authenticate"),
            header(&get, "www-authenticate"),
            "HEAD 与 GET 的挑战值逐字节相同"
        );
        assert!(body_of(head).await.is_empty());
        assert_eq!(harness.source.calls(), 0, "HEAD 绝不绕过认证");
    }

    #[tokio::test]
    async fn head_when_disabled_returns_404() {
        let harness = Harness::disabled();
        for headers in [HeaderMap::new(), authorized(USER, PASSWORD)] {
            let response = harness.send(Method::HEAD, DEFAULT_ENDPOINT, headers).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(header(&response, "www-authenticate").is_none());
            assert!(header(&response, "content-type").is_none());
            assert!(body_of(response).await.is_empty());
        }
        assert_eq!(harness.source.calls(), 0);
    }

    // ---------------------------------------------------------------------
    // 401
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn scrape_without_credentials_returns_401() {
        let harness = Harness::enabled();
        let response = harness.get(DEFAULT_ENDPOINT, HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            header(&response, "www-authenticate"),
            Some("Basic realm=\"rsetup-controller-metrics\"")
        );
        assert_eq!(header(&response, "cache-control"), Some("no-store"));
        assert_eq!(header(&response, "x-content-type-options"), Some("nosniff"));
        assert!(
            header(&response, "content-type").is_none(),
            "401 不得带 Content-Type（§4.4）"
        );
        assert!(body_of(response).await.is_empty());
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn scrape_with_wrong_password_returns_401() {
        let harness = Harness::enabled();
        let response = harness
            .get(DEFAULT_ENDPOINT, authorized(USER, "wrong_password"))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn scrape_with_wrong_username_returns_401() {
        let harness = Harness::enabled();
        let response = harness
            .get(DEFAULT_ENDPOINT, authorized("wrong_user", PASSWORD))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn scrape_with_malformed_header_returns_401() {
        let harness = Harness::enabled();
        let mut two_headers = authorized(USER, PASSWORD);
        two_headers.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwd3dvZA=="),
        );
        let cases = [
            raw_authorization("Bearer abc"),
            raw_authorization("Basic nothex!!"),
            raw_authorization("Basic dXNlcg"),
            raw_authorization("Basic cHJvbV9hZG1pbjo="),
            raw_authorization("OnBzc2R3ZA=="),
            two_headers,
        ];
        for (index, headers) in cases.into_iter().enumerate() {
            let response = harness.get(DEFAULT_ENDPOINT, headers).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "第 {index} 个畸形头必须 401"
            );
            assert_eq!(
                header(&response, "www-authenticate"),
                Some("Basic realm=\"rsetup-controller-metrics\"")
            );
        }
        assert_eq!(harness.source.calls(), 0);
    }

    // ---------------------------------------------------------------------
    // 404 / 405
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn disabled_endpoint_returns_404_regardless_of_credentials() {
        let harness = Harness::disabled();
        for headers in [HeaderMap::new(), authorized(USER, PASSWORD)] {
            let response = harness.get(DEFAULT_ENDPOINT, headers).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(
                header(&response, "www-authenticate").is_none(),
                "禁用态不宣告端点存在（§4.5）"
            );
            assert!(header(&response, "content-type").is_none());
            assert!(header(&response, "cache-control").is_none());
            assert!(body_of(response).await.is_empty());
        }
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn other_methods_are_rejected_with_allow_header() {
        let harness = Harness::enabled();
        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ] {
            let response = harness
                .send(method.clone(), DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
                .await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} 必须 405"
            );
            assert_eq!(
                header(&response, "allow"),
                Some("GET,HEAD"),
                "Axum 0.8.9 逐字节事实：append_allow_header 以 `,` 连接且无空格"
            );
            assert!(header(&response, "content-type").is_none());
        }
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn other_paths_are_not_registered_by_metrics_line() {
        let harness = Harness::enabled();
        for path in ["/", "/healthz", "/metrics.txt", "/metrics/deep"] {
            let response = harness.get(path, authorized(USER, PASSWORD)).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{path} 不得由本线应答"
            );
        }
        let custom = Harness::at_endpoint("/internal/metrics");
        assert_eq!(
            custom
                .get("/internal/metrics", authorized(USER, PASSWORD))
                .await
                .status(),
            StatusCode::OK,
            "配置路径必须生效"
        );
        assert_eq!(
            custom
                .send(Method::POST, "/internal/metrics", HeaderMap::new())
                .await
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            custom
                .get(DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
                .await
                .status(),
            StatusCode::NOT_FOUND,
            "配置路径之外的 /metrics 必须由其他条线或默认响应处置"
        );
    }

    // ---------------------------------------------------------------------
    // 会话边界隔离（§4.1）
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn metrics_ignores_session_cookie() {
        let harness = Harness::enabled();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("rsc_session=valid-looking-session-value"),
        );
        let response = harness.get(DEFAULT_ENDPOINT, headers).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn metrics_success_does_not_create_session() {
        let harness = Harness::enabled();
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            header(&response, "set-cookie").is_none(),
            "MUST NOT 写会话 Cookie（I3）"
        );
        // 结构证明：实现部分（`#[cfg(test)]` 之前）根本不出现会话/CSRF/限速/审计
        // 边界类型，也不写日志——本线是独立认证边界（§4.1）。
        let source = include_str!("http.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("本文件必须有实现部分");
        let code = code_only(implementation);
        for forbidden in [
            "HttpAuthState",
            "Session",
            "EndpointId",
            "Csrf",
            "RateLimit",
            "audit",
            "axum_extra",
            "tracing",
        ] {
            assert!(
                !code.contains(forbidden),
                "指标线实现不得引用会话/CSRF/限速/审计边界或写日志：{forbidden}"
            );
        }
    }

    // ---------------------------------------------------------------------
    // 503
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn source_error_maps_to_503_not_partial_zero() {
        let harness = Harness::failing("metrics fixture failure");
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header(&response, "retry-after"), Some("5"));
        assert_eq!(header(&response, "cache-control"), Some("no-store"));
        assert!(
            header(&response, "content-type").is_none(),
            "503 不得带 Content-Type"
        );
        let body = body_of(response).await;
        assert!(body.is_empty(), "503 必须空体，绝不输出部分指标（G4）");
        assert_eq!(harness.source.calls(), 1);
    }

    /// 真实源路径的失败/超时映射：lazy 池指向不可达端口，采集必须在
    /// `POOL_ACQUIRE_TIMEOUT`（500ms）量级内失败并映射 503，MUST NOT 排队 30 秒。
    #[tokio::test]
    async fn source_timeout_maps_to_503() {
        let source = Arc::new(SqlxMetricsSnapshotSource::new(lazy_pool()));
        let harness = Harness::with_source(source);
        let started = Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            harness.get(DEFAULT_ENDPOINT, authorized(USER, PASSWORD)),
        )
        .await
        .expect("必须在 3 秒内定性（不得退化为 30 秒 acquire 排队）");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header(&response, "retry-after"), Some("5"));
        assert!(
            started.elapsed() < Duration::from_millis(1_500),
            "真实源失败必须在 POOL_ACQUIRE_TIMEOUT 量级内返回，实测 {:?}",
            started.elapsed()
        );
    }

    // ---------------------------------------------------------------------
    // 并发闸与许可生命周期（I7 的进程内可证部分）
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn scrape_concurrency_cap_is_enforced() {
        // 闸容量取满配：本例要**预占满 MAX_CONCURRENT_SCRAPE 个许可**再验证闸满
        // 行为（断言的 `available_permits()` 满值同为该常量）。
        let harness = Harness::with_capacity(
            MAX_CONCURRENT_SCRAPE,
            FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
        );
        let permits: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                Arc::clone(&harness.guard)
                    .try_acquire_owned()
                    .expect("预占许可")
            })
            .collect();
        assert_eq!(harness.guard.available_permits(), 0);
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header(&response, "retry-after"), Some("5"));
        assert_eq!(harness.source.calls(), 0, "闸满时 MUST NOT 进入采集");
        // HEAD 不占闸：闸满仍 200（§6.2）。
        let head = harness
            .send(Method::HEAD, DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
            .await;
        assert_eq!(head.status(), StatusCode::OK);
        // 归还许可后 GET 立即可用。
        drop(permits);
        assert_eq!(harness.guard.available_permits(), MAX_CONCURRENT_SCRAPE);
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(harness.source.calls(), 1);
    }

    /// handler 侧许可绑定：许可由 `tokio::spawn` 的采集任务持有，**客户端断线
    /// （handler 被取消）既不取消采集任务，也不提前释放许可**（§5.5.1 规则 3）。
    /// 这是 I7 在进程内可指认的那一半；"close 完成前 permit 绝不回落"的确定性
    /// 证明由 Task 7 §7.7 第 4 步的受控关闭屏障完成。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn permit_stays_held_after_client_disconnects() {
        let harness = Harness::with_capacity(1, FakeMetricsSnapshotSource::hanging());
        let handler = {
            let router = harness.router.clone();
            // 该请求必须**带凭据**：认证在并发闸之前（§4.5），不带凭据会在 401 处
            // 返回、根本不占许可，本例就证不到"采集在途期间许可不回落"。
            let headers = authorized(USER, PASSWORD);
            tokio::spawn(async move {
                let mut builder = Request::builder().method(Method::GET).uri(DEFAULT_ENDPOINT);
                for (name, value) in &headers {
                    builder = builder.header(name, value);
                }
                let _ = router
                    .clone()
                    .oneshot(builder.body(Body::empty()).unwrap())
                    .await;
            })
        };
        // 等采集任务真正进入挂起态（许可已从闸里取走）。
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            harness.guard.available_permits(),
            0,
            "采集任务在途期间许可不得回落"
        );
        handler.abort();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            harness.guard.available_permits(),
            0,
            "客户端断线不得提前释放许可（§5.5.1 规则 3）"
        );
        // 采集任务仍在持有许可：第二条请求立即 503（闸满），且未进入采集。
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn credentials_are_not_echoed_in_error_paths() {
        let harness = Harness::enabled();
        let responses = [
            harness
                .get(
                    DEFAULT_ENDPOINT,
                    authorized("prom_admin", "supersensitive-secret"),
                )
                .await,
            harness
                .get(DEFAULT_ENDPOINT, raw_authorization("Basic bm90LWEtY29sb24"))
                .await,
            harness.get(DEFAULT_ENDPOINT, HeaderMap::new()).await,
        ];
        for response in responses {
            let mut text = format!("{:?}", response.headers());
            text.push_str(&String::from_utf8_lossy(&body_of(response).await));
            for marker in ["supersensitive-secret", "prom_admin", "c3VwZXJzZW5zaXRpdmU"] {
                assert!(!text.contains(marker), "错误响应回显了 {marker:?}：{text}");
            }
        }
    }

    /// 去掉行注释与文档注释后的纯代码文本（结构门禁看代码，不看注释）。
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

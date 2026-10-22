//! 前端生产静态资源嵌入与 SPA 精确路由（Web 嵌入交付）。
//!
//! 契约：
//! - 编译期由 `build.rs` 读取 `apps/controller-web/dist` 的**真实**构建产物
//!   （`index.html` 与 `assets/` 下实际存在的文件），生成 `OUT_DIR/web_assets.rs`
//!   资产表，本模块 `include!` 后把 HTML 与资产字节嵌入二进制；资产哈希只存在于
//!   生成产物里，不写死在本仓源码，前端重新构建后无需改任何 Rust 代码。
//!   缺 `dist` 或产物不合法时 `build.rs` 直接失败关闭（release 同样失败）。
//! - 运行时绝不读磁盘，也不依赖进程工作目录：二进制脱离源码目录即可运行。
//! - SPA 回退仅限已知页面白名单，且仅 GET/HEAD；缺失的 `/assets/*` 坚决 404，
//!   绝不回退 HTML 掩盖；不注册任何 `/api/` 路由，也不带全局 fallback，
//!   因此可与 API 子 router 直接 `merge` 而不抢占已匹配路由。
//! - 本模块**自身产出**的每个响应（成功的资产/HTML、`not_found()` 的 404、405）都带
//!   `X-Content-Type-Options: nosniff` 与 `Referrer-Policy: no-referrer`；HTML 另加
//!   `X-Frame-Options: DENY` 与 CSP（L4 纵深防御）。完全不匹配本模块任何路由的路径由
//!   axum `Router` 的默认 404 服务返回，其响应头只能由顶层组装补，见
//!   `tests/web_assets.rs` 末尾的「已知边界」。

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;

const MIME_HTML: &str = "text/html; charset=utf-8";
const CACHE_HTML: &str = "no-cache";
const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// HTML 文档（入口 `index.html`）的 CSP。
///
/// 前端是同源、无外部 CDN 的 Vite 构建：脚本与样式都只允许 `'self'`，两侧都不
/// 放开 `'unsafe-inline'`。**当前构建产物只使用外部样式**（`dist/index.html` 的
/// 样式只有 `<link rel=stylesheet href="/assets/…">`，模板里也没有内联 `style=`
/// 属性）；若未来模板/构建引入内联 style，必须显式评审并同步本策略与测试
/// （`tests/web_assets.rs` 的 `real_dist_html_has_no_inline_script_or_style`
/// 会在产物出现内联 `<script>` 或 `style=` 时直接变红）。`frame-ancestors 'none'` +
/// `X-Frame-Options: DENY` 双写以覆盖老浏览器；`base-uri`/`form-action`
/// 收紧到自身，杜绝注入 `<base>` 或改写表单提交目标。
const CSP_HTML: &str = "default-src 'self'; script-src 'self'; \
                        style-src 'self'; img-src 'self' data:; \
                        connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
                        form-action 'self'";

/// 一份编译期内嵌的前端构建产物。
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedAsset {
    /// 对外暴露的绝对请求路径，同时是 `index.html` 中引用的 URL。
    pub path: &'static str,
    /// 与扩展名一致的响应 MIME。
    pub mime: &'static str,
    /// 编译期嵌入的真实产物字节。
    pub bytes: &'static [u8],
}

// 由 `build.rs` 依据前端真实构建产物生成：`INDEX_HTML` 与 `ASSETS` 两项常量。
// 生成器的输入校验（路径合法性、MIME、非空、index.html 与产物一致）全部在构建期完成。
include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));

/// 静态资源目录前缀（改为 `pub` 只为 §7.6 第 8 条的一致性断言；值逐字不变）。
pub const ASSET_DIR: &str = "/assets/";

/// 已知 SPA 页面路由白名单（精确路径）。
pub const SPA_EXACT_ROUTES: &[&str] = &[
    "/",
    "/login",
    "/password",
    "/sessions",
    "/devices",
    "/tasks",
    "/admin",
];

/// 已知 SPA 页面路由白名单（带路径参数，axum 0.8 语法）。
pub const SPA_PARAM_ROUTES: &[&str] = &["/devices/{id}", "/tasks/{id}", "/admin/{*rest}"];

/// 路由要交付的那一份构建产物。
#[derive(Debug, Clone, Copy)]
struct EmbeddedBundle {
    index_html: &'static str,
    assets: &'static [EmbeddedAsset],
}

/// 前端静态资源与 SPA 页面路由（编译期嵌入的那一份构建）。
///
/// 只注册精确路径（无全局 fallback、无 `/api/` 路由），因此可与认证/管理 API
/// 子 router 直接 `merge`：未命中的路径交回外层 router 决定 404/405。
pub fn web_router() -> Router {
    web_router_from(INDEX_HTML, ASSETS)
}

/// 用给定的那一份构建产物装配路由。
///
/// 生产路径只经 [`web_router`] 传入编译期 `include!` 的资产表；本入口让集成测试
/// 能把「另一次构建」的资产表注入同一套路由逻辑，从而在不重编译的前提下证明
/// 换一次前端构建就换一份字节，而不是只服务某两条写死的 index 资产。
pub fn web_router_from(index_html: &'static str, assets: &'static [EmbeddedAsset]) -> Router {
    let mut router: Router<EmbeddedBundle> = Router::new();
    for asset in assets {
        assert!(
            asset.path.starts_with(ASSET_DIR) && !asset.bytes.is_empty(),
            "资产表条目非法（必须以 {ASSET_DIR} 开头且非空）：{}",
            asset.path
        );
        router = router.route(asset.path, any(serve_static));
    }
    for path in SPA_EXACT_ROUTES {
        router = router.route(path, any(serve_static));
    }
    for pattern in SPA_PARAM_ROUTES {
        router = router.route(pattern, any(serve_static));
    }
    router.with_state(EmbeddedBundle { index_html, assets })
}

async fn serve_static(State(bundle): State<EmbeddedBundle>, req: Request<Body>) -> Response {
    let method = req.method();
    if method != Method::GET && method != Method::HEAD {
        return method_not_allowed();
    }
    let head_only = method == Method::HEAD;
    let path = req.uri().path();
    if let Some(asset) = find_asset(bundle.assets, path) {
        return bytes_response(asset.mime, CACHE_IMMUTABLE, asset.bytes, head_only);
    }
    if is_known_spa_path(path) {
        return bytes_response(
            MIME_HTML,
            CACHE_HTML,
            bundle.index_html.as_bytes(),
            head_only,
        );
    }
    not_found()
}

fn find_asset<'a>(assets: &'a [EmbeddedAsset], path: &str) -> Option<&'a EmbeddedAsset> {
    assets.iter().find(|asset| asset.path == path)
}

/// 已知 SPA 页面路径（与 [`SPA_EXACT_ROUTES`] / [`SPA_PARAM_ROUTES`] 白名单等价）。
fn is_known_spa_path(path: &str) -> bool {
    if SPA_EXACT_ROUTES.contains(&path) {
        return true;
    }
    if path
        .strip_prefix("/admin/")
        .is_some_and(|rest| !rest.is_empty())
    {
        return true;
    }
    for prefix in ["/devices/", "/tasks/"] {
        if let Some(rest) = path.strip_prefix(prefix)
            && !rest.is_empty()
            && !rest.contains('/')
        {
            return true;
        }
    }
    false
}

/// 统一附加安全响应头（L4 纵深防御）。
///
/// - `X-Content-Type-Options: nosniff`：静态资产、入口 HTML 以及 404/405 一律带上，
///   杜绝浏览器把 JS/CSS 误判成可执行内容。
/// - `Referrer-Policy: no-referrer`：任何 web 层响应都不把 URL（可能含路径参数
///   `/devices/{id}`）外带成 Referer。
/// - `X-Frame-Options: DENY` 与 CSP **只加给 HTML**：资产本身不可被框架化渲染，
///   逐条给上万个哈希 chunk 重复两个头也没有收益。
///
/// 本函数不触碰 `Content-Type`/`Cache-Control`/`Content-Length`，也不改变状态码。
fn push_security_headers(headers: &mut HeaderMap, html: bool) {
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if html {
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CSP_HTML),
        );
    }
}

fn bytes_response(
    content_type: &str,
    cache_control: &str,
    bytes: &[u8],
    head_only: bool,
) -> Response {
    let body = if head_only {
        Body::empty()
    } else {
        Body::from(bytes.to_vec())
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("静态资源 MIME 必须是合法 header value"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_str(cache_control).expect("缓存头必须是合法 header value"),
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from(bytes.len() as u64),
    );
    push_security_headers(&mut headers, content_type == MIME_HTML);
    (StatusCode::OK, headers, body).into_response()
}

fn method_not_allowed() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
    push_security_headers(&mut headers, false);
    (StatusCode::METHOD_NOT_ALLOWED, headers, Body::empty()).into_response()
}

fn not_found() -> Response {
    // 不伪造 API 错误包络：统一 NOT_FOUND 信封由 API 层负责，这里只坚决 404。
    let mut headers = HeaderMap::new();
    push_security_headers(&mut headers, false);
    (StatusCode::NOT_FOUND, headers, Body::empty()).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 独立拼装的预期 CSP（逐项列出，作为 `CSP_HTML` 的第二预期源）：
    /// 任何一处指令顺序、分号后空格或指令被删改，本用例即红。
    const EXPECTED_CSP: &str = concat!(
        "default-src 'self'; ",
        "script-src 'self'; ",
        "style-src 'self'; ",
        "img-src 'self' data:; ",
        "connect-src 'self'; ",
        "frame-ancestors 'none'; ",
        "base-uri 'none'; ",
        "form-action 'self'",
    );

    #[test]
    fn csp_is_the_reviewed_directive_set() {
        assert_eq!(CSP_HTML, EXPECTED_CSP);
    }

    #[test]
    fn html_response_carries_all_four_security_headers() {
        let res = bytes_response(MIME_HTML, CACHE_HTML, b"<!doctype html>", false);
        assert_eq!(res.status(), StatusCode::OK);
        let headers = res.headers();
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], EXPECTED_CSP);
        // 既有语义逐字不变。
        assert_eq!(headers[header::CONTENT_TYPE], MIME_HTML);
        assert_eq!(headers[header::CACHE_CONTROL], CACHE_HTML);
        assert_eq!(headers[header::CONTENT_LENGTH], "15");
    }

    #[test]
    fn asset_response_carries_nosniff_but_no_html_only_headers() {
        let res = bytes_response(
            "text/javascript",
            CACHE_IMMUTABLE,
            b"console.log(1);",
            false,
        );
        assert_eq!(res.status(), StatusCode::OK);
        let headers = res.headers();
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
        assert!(
            headers.get(header::CONTENT_SECURITY_POLICY).is_none(),
            "CSP 只加给 HTML"
        );
        assert!(
            headers.get(header::X_FRAME_OPTIONS).is_none(),
            "XFO 只加给 HTML"
        );
        assert_eq!(headers[header::CONTENT_TYPE], "text/javascript");
        assert_eq!(headers[header::CACHE_CONTROL], CACHE_IMMUTABLE);
        assert_eq!(headers[header::CONTENT_LENGTH], "15");
    }

    #[test]
    fn head_response_keeps_security_headers_and_real_content_length() {
        // HEAD 只把 body 换成空，长度与安全头都必须照常。
        let res = bytes_response(MIME_HTML, CACHE_HTML, b"<!doctype html>", true);
        let headers = res.headers();
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::CONTENT_SECURITY_POLICY], EXPECTED_CSP);
        assert_eq!(headers[header::CONTENT_LENGTH], "15");
    }

    #[test]
    fn error_responses_carry_base_headers_without_html_directives() {
        // `not_found()` 在当前路由表下不可达（未匹配路径由 axum Router 默认服务返回），
        // 白名单内部落空的形态一旦引入即由本用例锁定其头契约。
        let not_found = not_found();
        assert_eq!(not_found.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            not_found.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(not_found.headers()[header::REFERRER_POLICY], "no-referrer");
        assert!(
            not_found
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none()
        );

        let denied = method_not_allowed();
        assert_eq!(denied.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(denied.headers()[header::ALLOW], "GET, HEAD");
        assert_eq!(denied.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(denied.headers()[header::REFERRER_POLICY], "no-referrer");
        assert!(
            denied
                .headers()
                .get(header::CONTENT_SECURITY_POLICY)
                .is_none()
        );
        assert!(denied.headers().get(header::X_FRAME_OPTIONS).is_none());
    }
}

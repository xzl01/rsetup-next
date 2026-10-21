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
        if let Some(rest) = path.strip_prefix(prefix) {
            if !rest.is_empty() && !rest.contains('/') {
                return true;
            }
        }
    }
    false
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
    (StatusCode::OK, headers, body).into_response()
}

fn method_not_allowed() -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
    (StatusCode::METHOD_NOT_ALLOWED, headers, Body::empty()).into_response()
}

fn not_found() -> Response {
    // 不伪造 API 错误包络：统一 NOT_FOUND 信封由 API 层负责，这里只坚决 404。
    (StatusCode::NOT_FOUND, Body::empty()).into_response()
}

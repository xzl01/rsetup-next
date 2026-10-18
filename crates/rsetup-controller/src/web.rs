//! 前端生产静态资源嵌入与 SPA 精确路由（Web 嵌入交付）。
//!
//! 契约：
//! - 编译期把 `apps/controller-web/dist/index.html` 与其引用的哈希 JS/CSS 字节
//!   嵌入二进制；运行时绝不读磁盘，也不依赖进程工作目录。
//! - SPA 回退仅限已知页面白名单，且仅 GET/HEAD；缺失的 `/assets/*` 坚决 404，
//!   绝不回退 HTML 掩盖；不注册任何 `/api/` 路由，也不带全局 fallback，
//!   因此可与 API 子 router 直接 `merge` 而不抢占已匹配路由。

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::any;

/// 编译期嵌入的 Vite 入口 HTML。缺失 `dist` 时 `include_str!` 直接编译失败（fail-closed）。
const INDEX_HTML: &str = include_str!("../../../apps/controller-web/dist/index.html");

/// 构建产物资产的 URL 前缀。
const ASSET_DIR: &str = "/assets/";

const MIME_HTML: &str = "text/html; charset=utf-8";
const MIME_JS: &str = "text/javascript";
const MIME_CSS: &str = "text/css";
const CACHE_HTML: &str = "no-cache";
const CACHE_IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// 一份编译期内嵌的前端构建产物。
struct EmbeddedAsset {
    /// 对外暴露的绝对请求路径，同时是 `index.html` 中引用的 URL。
    path: &'static str,
    /// 与扩展名一致的响应 MIME。
    mime: &'static str,
    /// 编译期嵌入的真实产物字节。
    bytes: &'static [u8],
}

/// 编译期资产表。
///
/// Vite 的文件名带内容哈希，因此这里不写死「上一轮构建」的语义：`verify_asset_table`
/// 在编译期把下表与 `index.html` 的真实引用集合做双向核对，任一侧漂移都会让
/// `cargo build` 直接失败，绝不静默服务旧产物或空占位：
/// - 表内条目未被 `index.html` 引用（哈希已变、条目多余）；
/// - `index.html` 引用的资产不在表内（新增资产未嵌入）；
/// - MIME 与扩展名不符，或嵌入的产物为空文件。
const ASSETS: &[EmbeddedAsset] = &[
    EmbeddedAsset {
        path: "/assets/index-BPj7dGBJ.js",
        mime: MIME_JS,
        bytes: include_bytes!("../../../apps/controller-web/dist/assets/index-BPj7dGBJ.js"),
    },
    EmbeddedAsset {
        path: "/assets/index-BCZGT6CR.css",
        mime: MIME_CSS,
        bytes: include_bytes!("../../../apps/controller-web/dist/assets/index-BCZGT6CR.css"),
    },
];

/// 已知 SPA 页面路由白名单（精确路径）。
const SPA_EXACT_ROUTES: &[&str] = &[
    "/",
    "/login",
    "/password",
    "/sessions",
    "/devices",
    "/tasks",
    "/admin",
];

/// 已知 SPA 页面路由白名单（带路径参数，axum 0.8 语法）。
const SPA_PARAM_ROUTES: &[&str] = &["/devices/{id}", "/tasks/{id}", "/admin/{*rest}"];

// ---------------------------------------------------------------------------
// 编译期资产表校验（const 断言）
// ---------------------------------------------------------------------------

/// 从 `from` 起在 `haystack` 中查找 `needle` 的首个下标。
const fn find_sub(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() || from > haystack.len() - needle.len() {
        return None;
    }
    let mut i = from;
    while i + needle.len() <= haystack.len() {
        let mut j = 0;
        while j < needle.len() && haystack[i + j] == needle[j] {
            j += 1;
        }
        if j == needle.len() {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 比较 `a[a_start..a_start + len]` 与 `b[b_start..b_start + len]`。
/// const fn 不支持区间切片（`Index` 尚未 const 化），故统一用下标比较。
const fn spans_eq(a: &[u8], a_start: usize, b: &[u8], b_start: usize, len: usize) -> bool {
    if a_start + len > a.len() || b_start + len > b.len() {
        return false;
    }
    let mut i = 0;
    while i < len {
        if a[a_start + i] != b[b_start + i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `haystack` 自 `offset` 起是否与 `needle` 逐字节相等。
const fn bytes_eq_at(haystack: &[u8], offset: usize, needle: &[u8]) -> bool {
    spans_eq(haystack, offset, needle, 0, needle.len())
}

const fn bytes_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bytes_eq_at(a, 0, b)
}

const fn bytes_ends_with(haystack: &[u8], suffix: &[u8]) -> bool {
    if suffix.len() > haystack.len() {
        return false;
    }
    bytes_eq_at(haystack, haystack.len() - suffix.len(), suffix)
}

/// `html[start..end]` 是否为资产表已知资产名（不含 `/assets/` 前缀）。
const fn is_known_asset_span(html: &[u8], start: usize, end: usize) -> bool {
    if end < start {
        return false;
    }
    let dir = ASSET_DIR.as_bytes();
    let name_len = end - start;
    let mut i = 0;
    while i < ASSETS.len() {
        let path = ASSETS[i].path.as_bytes();
        if path.len() == dir.len() + name_len
            && bytes_eq_at(path, 0, dir)
            && spans_eq(path, dir.len(), html, start, name_len)
        {
            return true;
        }
        i += 1;
    }
    false
}

/// `html[start..end]` 是否以 `suffix` 结尾。
const fn span_ends_with(html: &[u8], start: usize, end: usize, suffix: &[u8]) -> bool {
    if end < start || suffix.len() > end - start {
        return false;
    }
    spans_eq(html, end - suffix.len(), suffix, 0, suffix.len())
}

/// 编译期双向核对资产表与 `index.html` 引用集合。
const fn verify_asset_table() {
    let html = INDEX_HTML.as_bytes();
    let dir = ASSET_DIR.as_bytes();

    // 方向一：表内每个条目都必须被 index.html 真实引用，且 MIME/扩展名一致、产物非空。
    let mut i = 0;
    while i < ASSETS.len() {
        let asset = &ASSETS[i];
        let path = asset.path.as_bytes();
        assert!(
            bytes_eq_at(path, 0, dir),
            "资产表条目的 path 必须以 /assets/ 开头"
        );
        assert!(
            find_sub(html, path, 0).is_some(),
            "资产表条目未被 index.html 引用：前端已重新构建（哈希漂移）或条目多余，请同步资产表"
        );
        assert!(
            (bytes_ends_with(path, b".js") && bytes_eq(asset.mime.as_bytes(), MIME_JS.as_bytes()))
                || (bytes_ends_with(path, b".css")
                    && bytes_eq(asset.mime.as_bytes(), MIME_CSS.as_bytes())),
            "资产 MIME 必须与扩展名一致（.js -> text/javascript, .css -> text/css）"
        );
        assert!(
            !asset.bytes.is_empty(),
            "嵌入的构建产物为空，禁止以空占位充当实现"
        );
        i += 1;
    }

    // 方向二：index.html 引用的每个 /assets/* 都必须已嵌入，且至少含一个 JS 与一个 CSS。
    let mut pos = 0;
    let mut js_count = 0;
    let mut css_count = 0;
    loop {
        let found = find_sub(html, dir, pos);
        match found {
            None => break,
            Some(at) => {
                let start = at + dir.len();
                let mut end = start;
                while end < html.len() && html[end] != b'"' {
                    end += 1;
                }
                assert!(
                    end < html.len(),
                    "index.html 中的资产引用缺少闭合引号，无法确定资产名"
                );
                assert!(
                    is_known_asset_span(html, start, end),
                    "index.html 引用的资产未嵌入编译产物，请同步资产表"
                );
                if span_ends_with(html, start, end, b".js") {
                    js_count += 1;
                }
                if span_ends_with(html, start, end, b".css") {
                    css_count += 1;
                }
                pos = end;
            }
        }
    }
    assert!(
        js_count >= 1 && css_count >= 1,
        "index.html 必须至少引用一个哈希 JS 与一个哈希 CSS 资产"
    );
}

const _: () = verify_asset_table();

// ---------------------------------------------------------------------------
// 运行时路由
// ---------------------------------------------------------------------------

/// 前端静态资源与 SPA 页面路由。
///
/// 只注册精确路径（无全局 fallback、无 `/api/` 路由），因此可与认证/管理 API
/// 子 router 直接 `merge`：未命中的路径交回外层 router 决定 404/405。
pub fn web_router() -> Router {
    let mut router = Router::new();
    for asset in ASSETS {
        router = router.route(asset.path, any(serve_static));
    }
    for path in SPA_EXACT_ROUTES {
        router = router.route(path, any(serve_static));
    }
    for pattern in SPA_PARAM_ROUTES {
        router = router.route(pattern, any(serve_static));
    }
    router
}

async fn serve_static(req: Request<Body>) -> Response {
    let method = req.method();
    if method != Method::GET && method != Method::HEAD {
        return method_not_allowed();
    }
    let head_only = method == Method::HEAD;
    let path = req.uri().path();
    if let Some(asset) = find_asset(path) {
        return bytes_response(asset.mime, CACHE_IMMUTABLE, asset.bytes, head_only);
    }
    if is_known_spa_path(path) {
        return bytes_response(MIME_HTML, CACHE_HTML, INDEX_HTML.as_bytes(), head_only);
    }
    not_found()
}

fn find_asset(path: &str) -> Option<&'static EmbeddedAsset> {
    ASSETS.iter().find(|asset| asset.path == path)
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

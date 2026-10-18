use axum::{
    body::{Body, to_bytes},
    http::{Method, Request, StatusCode},
};
use tower::ServiceExt;

/// 读取磁盘上 `apps/controller-web/dist` 内的真实构建产物。
///
/// 这是独立于被测路由的**预期源**：测试不通过解析响应来推导「应该是什么」，
/// 而是先固定当前构建的真实字节，再要求路由逐字节交付同一份内容。
fn read_dist_file(relative_url: &str) -> Vec<u8> {
    let path = format!(
        "{}/../../apps/controller-web/dist{}",
        env!("CARGO_MANIFEST_DIR"),
        relative_url
    );
    std::fs::read(&path).unwrap_or_else(|err| {
        panic!("built dist{relative_url} must exist on disk at {path}: {err}");
    })
}

#[tokio::test]
async fn web_assets_mime_cache_and_404_boundaries() {
    let router = rsetup_controller::web::web_router();

    // 0. 独立预期源：直接读取磁盘上的真实构建入口 dist/index.html。
    let expected_html_bytes = read_dist_file("/index.html");
    let expected_html_str = String::from_utf8(expected_html_bytes.clone())
        .expect("dist/index.html must be valid utf-8");
    assert!(
        expected_html_str.contains("<div id=\"app\">"),
        "the built entry on disk must mount the SPA root"
    );

    // 1. SPA 页面路由返回 200 OK + no-cache，且 HTML 字节与磁盘构建入口逐字节全等。
    for uri in ["/devices", "/"] {
        let req = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let res = router.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8"
        );
        assert_eq!(res.headers().get("cache-control").unwrap(), "no-cache");
        let html_bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            html_bytes.as_ref(),
            expected_html_bytes.as_slice(),
            "GET {uri} must serve exactly the bytes of the built dist/index.html"
        );
    }

    // 2. 哈希资产 URL 从磁盘真实入口提取；任一缺失即失败，不能 if let 跳过。
    let js_url = expected_html_str
        .split('"')
        .find(|s| s.starts_with("/assets/") && s.ends_with(".js"))
        .expect("Vite index must reference a hashed JS asset");
    let css_url = expected_html_str
        .split('"')
        .find(|s| s.starts_with("/assets/") && s.ends_with(".css"))
        .expect("Vite index must reference a hashed CSS asset");
    for (url, mime) in [(js_url, "text/javascript"), (css_url, "text/css")] {
        let expected = read_dist_file(url);
        assert!(!expected.is_empty(), "asset is not allowed to be empty");
        let req_asset = Request::builder()
            .method(Method::GET)
            .uri(url)
            .body(Body::empty())
            .unwrap();
        let res_asset = router.clone().oneshot(req_asset).await.unwrap();
        assert_eq!(res_asset.status(), StatusCode::OK);
        assert_eq!(res_asset.headers().get("content-type").unwrap(), mime);
        assert_eq!(
            res_asset.headers().get("cache-control").unwrap(),
            "public, max-age=31536000, immutable"
        );
        let asset_bytes = to_bytes(res_asset.into_body(), usize::MAX).await.unwrap();
        assert_eq!(
            asset_bytes.as_ref(),
            expected.as_slice(),
            "serve the actual build bytes"
        );
    }

    // 3. 缺失静态资产坚决 404，绝不回退 HTML
    let req_missing_asset = Request::builder()
        .method(Method::GET)
        .uri("/assets/missing.js")
        .body(Body::empty())
        .unwrap();
    let res_missing_asset = router.clone().oneshot(req_missing_asset).await.unwrap();
    assert_eq!(res_missing_asset.status(), StatusCode::NOT_FOUND);

    // 4. API 404 JSON 由另一个测试在完整生产合并 router 上断言，
    // 此孤立 web_router 用例只验证不能把 API 当 SPA 成功回退。
    let req_api = Request::builder()
        .method(Method::GET)
        .uri("/api/v1/not-found")
        .body(Body::empty())
        .unwrap();
    let res_api = router.clone().oneshot(req_api).await.unwrap();
    assert_ne!(res_api.status(), StatusCode::OK);

    // 5. 非 GET/HEAD 尝试访问 SPA 页面路由返回 405 Method Not Allowed 或 404
    let req_post_spa = Request::builder()
        .method(Method::POST)
        .uri("/devices")
        .body(Body::empty())
        .unwrap();
    let res_post_spa = router.oneshot(req_post_spa).await.unwrap();
    assert_ne!(res_post_spa.status(), StatusCode::OK);
}

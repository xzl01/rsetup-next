//! 26 管理路由动态网关矩阵（Identity Task 13b，05 子计划 Step 1–4）。
//!
//! 证据边界：本文件只证明「router → 业务 Gateway 方法」的动态接线（spy 计数）
//! 与认证/Host/CSRF/临时密码拒绝路径的**零业务调用**；不证明生产 SQLx、真实
//! 数据源、真实 probe 或真库验收（归 Task 14 受控门禁）。

#[path = "support/management_http_fixture.rs"]
mod fixture;

use axum::body::to_bytes;
use axum::http::StatusCode;
use tower::ServiceExt;

#[tokio::test]
async fn all_26_missing_sessions_return_401_and_zero_business_calls() {
    assert_eq!(fixture::cases().len(), 26);
    for case in fixture::cases() {
        let (router, counters) = fixture::fixture(false);
        let response = router
            .oneshot(fixture::request(&case, false, true, true))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{} {}",
            case.method,
            case.path
        );
        let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "AUTH_REQUIRED");
        assert_eq!(counters.total(), 0, "拒绝路径所有网关调用必须恒零");
    }
}

#[tokio::test]
async fn all_26_allowed_requests_really_reach_their_business_gateway() {
    for case in fixture::cases() {
        let (router, counters) = fixture::fixture(false);
        let response = router
            .oneshot(fixture::request(&case, true, true, true))
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{} {} -> {}",
            case.method,
            case.path,
            response.status()
        );
        assert_eq!(
            counters.count(case.gateway),
            1,
            "目标网关计数必须恰好递增 1：{} {}",
            case.method,
            case.path
        );
        assert_eq!(
            counters.total(),
            1,
            "非目标网关必须零调用：{} {}",
            case.method,
            case.path
        );
    }
}

#[tokio::test]
async fn host_and_temporary_password_and_write_csrf_reject_before_gateway() {
    for case in fixture::cases() {
        for mode in [0, 1, 2] {
            if mode == 2 && case.method == "GET" {
                continue;
            }
            let (router, counters) = fixture::fixture(mode == 1);
            let response = router
                .oneshot(fixture::request(&case, true, mode != 0, mode != 2))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::FORBIDDEN,
                "{} {} mode {mode}",
                case.method,
                case.path
            );
            let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                body["error"]["code"],
                if mode == 1 {
                    "PASSWORD_CHANGE_REQUIRED"
                } else {
                    "CSRF_INVALID"
                },
                "{} {} mode {mode}",
                case.method,
                case.path
            );
            assert_eq!(counters.total(), 0, "拒绝路径所有网关调用必须恒零");
        }
    }
}

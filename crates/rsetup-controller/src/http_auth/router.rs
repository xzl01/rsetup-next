use axum::Router;
use axum::routing::{get, post};

use super::login::login;
use super::me::me;
use super::password_logout::{logout, password};
use super::sessions::{revoke_by_id, revoke_others_handler, sessions_list};
use super::state::AppState;

/// Partial auth router: `POST /api/v1/auth/login`, `GET /api/v1/auth/me`,
/// and the Task 3c fake-backed `POST /api/v1/auth/password` and
/// `POST /api/v1/auth/logout`.
/// Merged with `crate::build_router` so the existing `/healthz` and
/// `/readyz` implementations are retained verbatim. NOT wired to `main`.
pub fn build_http_router(state: AppState) -> Router {
    let probes = crate::build_router(state.db.clone());
    let auth = Router::new()
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/auth/password", post(password))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/sessions", get(sessions_list))
        .route(
            "/api/v1/auth/sessions/revoke-others",
            post(revoke_others_handler),
        )
        .route("/api/v1/auth/sessions/{id}/revoke", post(revoke_by_id))
        .with_state(state);
    probes.merge(auth)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};

    use super::super::tests_support::{HOST, call_status, router};

    #[tokio::test]
    async fn probes_kept_and_other_auth_routes_registered() {
        let router = router();
        assert_eq!(
            call_status(
                &router,
                Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())
                    .unwrap()
            )
            .await,
            StatusCode::OK,
            "merged /healthz must stay 200"
        );
        for (method, uri) in [
            ("GET", "/api/v1/auth/password"),
            ("GET", "/api/v1/auth/logout"),
        ] {
            let status = call_status(
                &router,
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::HOST, HOST)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} {uri} must be 405 (registered as POST-only in Task 3c)"
            );
        }
        // The login path exists only for POST: GET must be 405.
        let status = call_status(
            &router,
            Request::builder()
                .method("GET")
                .uri("/api/v1/auth/login")
                .header(header::HOST, HOST)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "GET on the login path must be 405 (POST-only route)"
        );
    }
}

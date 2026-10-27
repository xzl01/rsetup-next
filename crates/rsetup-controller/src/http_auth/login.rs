use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Extension, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde_json::json;

use super::csrf::{CsrfBinding, MAX_CSRF_ENTRIES, prune_csrf_table};
use super::state::HttpAuthState;
use crate::auth::service::Login;
use crate::error::ControllerError;
use crate::http_api::{err_response, ok_response, user_public};
use crate::http_gate::{
    MAX_BODY_BYTES, exhausted, json_content_type, not_ready, rate_limited, single_header,
};
use crate::http_security::{RateLimitError, check_pre_session, session_cookie_header};

/// Password cap (spec 01 A-02): at most 512 UTF-8 bytes, never truncated.
pub(super) const MAX_PASSWORD_BYTES: usize = 512;

pub(super) async fn login(
    State(state): State<Arc<HttpAuthState>>,
    headers: HeaderMap,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    body: Body,
) -> Response {
    // 1. Pre-session gate: exact allowed Host and exact allowed Origin.
    //    Both must appear exactly once (repeated/multi-valued headers are
    //    rejected, never reduced to the first value).
    let host = single_header(&headers, header::HOST);
    let origin = single_header(&headers, header::ORIGIN);
    if check_pre_session(host, origin, &state.config).is_err() {
        return err_response(
            StatusCode::FORBIDDEN,
            "CSRF_INVALID",
            "security.origin_host",
            json!({}),
        );
    }
    // 2. JSON Content-Type, then the bounded 1 MiB body (Content-Length
    //    precheck first; the bounded read also catches absent/lying CL).
    if !json_content_type(single_header(&headers, header::CONTENT_TYPE)) {
        return bad_body();
    }
    let declared_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return bad_body();
    }
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return bad_body(),
    };
    // 3. Exactly {username, password}; the username must be canonical ASCII
    //    BEFORE the rate map and before any credential check.
    let (username, password) = match parse_login_body(&bytes) {
        Some(pair) => pair,
        None => return bad_body(),
    };
    if !valid_username(&username) {
        return bad_body();
    }
    // 4. The source must come from a trusted ConnectInfo; never guessed.
    let Some(Extension(ConnectInfo(peer))) = connect else {
        return not_ready();
    };
    // 5. Rate gate: one monotonic Instant per request, the limiter map is
    //    locked only for the check and (later) the record.
    let now = Instant::now();
    {
        let mut rate = state.rate.lock().unwrap();
        match rate.retry_after(now, &username, peer.ip()) {
            Err(RateLimitError::Exhausted) => return exhausted(),
            Ok(Some(remaining)) => return rate_limited(remaining),
            Ok(None) => {}
        }
    }
    // 6. CSRF capacity peek: first prune bindings past their absolute
    //    lifetime, then — at the soft threshold — the bindings whose session
    //    the process clock already reports dead, then fail closed if the map
    //    is STILL at the cap, BEFORE any credential work (the authoritative
    //    check for the concurrent case stays inside the insert lock in
    //    `login_success`).
    {
        let mut csrf = state.csrf.lock().unwrap();
        prune_csrf_table(&mut csrf, state.auth.as_ref(), now);
        if csrf.len() >= MAX_CSRF_ENTRIES {
            return exhausted();
        }
    }
    // 7. Credential check. Only InvalidArgument is a counted failure;
    //    backend faults are fixed 503s that never touch the limiter.
    match state.auth.login(&username, &password).await {
        Ok(login) => login_success(&state, login),
        Err(ControllerError::InvalidArgument) => {
            match state
                .rate
                .lock()
                .unwrap()
                .record_failure(now, &username, peer.ip())
            {
                Err(RateLimitError::Exhausted) => exhausted(),
                Ok(()) => err_response(
                    StatusCode::UNAUTHORIZED,
                    "INVALID_CREDENTIALS",
                    "auth.invalid_credentials",
                    json!({}),
                ),
            }
        }
        Err(error) => match error {
            ControllerError::Database(_) => err_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "STORAGE_UNAVAILABLE",
                "system.storage_unavailable",
                json!({}),
            ),
            _ => not_ready(),
        },
    }
}

/// Fixed 400 for every malformed login body; params stay redacted/empty.
fn bad_body() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "auth.login.bad_body",
        json!({}),
    )
}

/// Exactly `{username: string, password: string}`, both non-empty.
fn parse_login_body(bytes: &[u8]) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    if object.len() != 2 || !object.contains_key("username") || !object.contains_key("password") {
        return None;
    }
    let username = object.get("username")?.as_str()?.to_string();
    let password = object.get("password")?.as_str()?.to_string();
    if username.is_empty() || password.is_empty() {
        return None;
    }
    // Spec 01 A-02: at most 512 UTF-8 bytes; overruns are a fixed 400
    // (bad body), checked before the rate map and any credential work.
    if password.len() > MAX_PASSWORD_BYTES {
        return None;
    }
    Some((username, password))
}

/// Canonical username: 3-64 ASCII bytes from `[a-z0-9._-]`, first byte
/// alphanumeric.
fn valid_username(username: &str) -> bool {
    let bytes = username.as_bytes();
    (3..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// Success: validate the raw session token FIRST (a failed validation
/// must not leave a CSRF binding behind), then mint a 32-byte OsRng CSRF
/// token, prune past-absolute bindings (plus the threshold-gated
/// dead-session sweep), bind the token to the session digest under the
/// 4096 cap, and set the validated `rsc_session` cookie.
/// The in-lock capacity check below stays authoritative for the concurrent
/// case: after both prunes the table may still be full of LIVE bindings,
/// and then a NEW digest keeps failing closed without evicting or
/// overwriting anything.
fn login_success(state: &HttpAuthState, login: Login) -> Response {
    let digest = login.session.digest;
    let cookie = match session_cookie_header(&login.raw_token, &state.config) {
        Ok(cookie) => cookie,
        Err(_) => return not_ready(),
    };
    let cookie_header = match HeaderValue::from_str(&cookie) {
        Ok(value) => value,
        Err(_) => return not_ready(),
    };
    let mut csrf_raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut csrf_raw);
    let csrf_token = hex::encode(csrf_raw);
    let issued_at = Instant::now();
    {
        let mut csrf = state.csrf.lock().unwrap();
        prune_csrf_table(&mut csrf, state.auth.as_ref(), issued_at);
        if csrf.len() >= MAX_CSRF_ENTRIES && !csrf.contains_key(&digest) {
            return exhausted();
        }
        csrf.insert(
            digest,
            CsrfBinding {
                token: csrf_token.clone(),
                issued_at,
            },
        );
    }
    let mut response = ok_response(json!({
        "user": user_public(&login.session.user),
        "csrf_token": csrf_token,
    }));
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie_header);
    response
}

#[cfg(test)]
mod tests {
    use super::super::router::build_http_router;
    use super::super::tests_support::{
        FakeAuth, FakeLogin, HOST, ORIGIN, PASSWORD, PEER, PEER2, USERNAME, app_state,
        assert_error, body_json, call, fake_token, login_request, router, seed_live_csrf_bindings,
        valid_request,
    };
    use super::*;
    use axum::http::{HeaderValue, StatusCode, header};

    #[tokio::test]
    async fn login_success_contract() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let router = build_http_router(state);
        let (status, headers, value, text) = call(&router, valid_request()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value.as_object().map(|o| o.len()),
            Some(2),
            "envelope must be exactly data + request_id: {text}"
        );
        assert!(value["request_id"].is_string(), "text: {text}");
        let data = value["data"].as_object().unwrap();
        assert_eq!(
            data.len(),
            2,
            "data must be exactly user + csrf_token: {text}"
        );
        let user = &data["user"];
        assert_eq!(user["username"], json!(USERNAME));
        assert!(
            user["active"].is_boolean()
                && user["is_admin"].is_boolean()
                && user["must_change_password"].is_boolean(),
            "user flags must be strict booleans: {text}"
        );
        assert!(
            !user.as_object().unwrap().contains_key("password_hash"),
            "projection must not carry password_hash: {text}"
        );
        let csrf = data["csrf_token"].as_str().unwrap();
        assert_eq!(csrf.len(), 64, "csrf_token must be 64 hex chars: {text}");
        assert!(
            csrf.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "csrf_token must be lowercase hex: {text}"
        );
        let cookie = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(cookie.starts_with("rsc_session="), "cookie: {cookie}");
        for part in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=43200"] {
            assert!(
                cookie.contains(part),
                "cookie must contain {part:?}: {cookie}"
            );
        }
        assert!(
            !cookie.contains("Secure"),
            "HTTP-direct cookie must not set Secure: {cookie}"
        );
        assert!(
            cookie.contains(&fake_token()),
            "cookie must carry the token: {cookie}"
        );
        assert!(
            headers.get(header::AUTHORIZATION).is_none(),
            "login must not emit an Authorization header"
        );
        assert!(
            !text.contains(PASSWORD),
            "response must not leak the password"
        );
        assert!(
            !text.contains("argon2"),
            "response must not leak hash material"
        );
        let bound = auth_state.csrf.lock().unwrap();
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert_eq!(
            bound.get(&digest).map(|binding| binding.token.as_str()),
            Some(csrf),
            "CSRF must be bound to the session digest"
        );
        assert_eq!(bound.len(), 1, "exactly one binding: {bound:?}");
    }

    #[tokio::test]
    async fn login_invalid_raw_token_is_503_and_csrf_map_untouched() {
        // The backend hands back a well-formed Login whose raw token the
        // cookie contract rejects: the handler must answer a fixed 503
        // WITHOUT leaving a CSRF binding behind (fail-closed, no rollback
        // leak).
        let auth = FakeAuth::new(FakeLogin::BadToken);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let router = build_http_router(state);
        let (status, _headers, _value, text) = call(&router, valid_request()).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "invalid raw session token must be a fixed 503: {text}"
        );
        assert_error(&text, "NOT_READY", "system.not_ready");
        assert!(
            !text.contains(PASSWORD),
            "no password leak on bad token: {text}"
        );
        let bound = auth_state.csrf.lock().unwrap();
        assert_eq!(
            bound.len(),
            0,
            "invalid raw token must not leave a CSRF binding: {bound:?}"
        );
    }

    #[tokio::test]
    async fn login_rejects_bad_host_or_origin() {
        let router = router();
        let cases = [
            (Some("127.0.0.1:9999"), Some(ORIGIN), "wrong host"),
            (None, Some(ORIGIN), "missing host"),
            (Some(HOST), None, "missing origin"),
            (
                Some(HOST),
                Some("HTTP://127.0.0.1:5173"),
                "case-different origin",
            ),
            (Some(HOST), Some("http://127.0.0.1:5174"), "wrong origin"),
        ];
        for (host, origin, label) in cases {
            let (status, _headers, _value, text) = call(
                &router,
                login_request(
                    body_json(USERNAME, PASSWORD),
                    host,
                    origin,
                    Some("application/json"),
                    Some(PEER.parse().unwrap()),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{label} must be 403");
            assert_error(&text, "CSRF_INVALID", "security.origin_host");
            assert!(!text.contains(PASSWORD), "{label}: no password leak");
        }
    }

    #[tokio::test]
    async fn login_rejects_repeated_gate_headers_not_just_first_value() {
        let router = router();
        // Repeated Origin (allowed first, evil second): the gate must not
        // silently trust the first value — fixed 403.
        let mut req = valid_request();
        let origin_name: axum::http::HeaderName = "origin".parse().unwrap();
        req.headers_mut()
            .append(origin_name, HeaderValue::from_static("http://evil.example"));
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "repeated Origin must be 403: {text}"
        );
        assert_error(&text, "CSRF_INVALID", "security.origin_host");
        // Repeated Content-Type (application/json first, text/plain second):
        // fixed 400, not the first value's acceptance.
        let mut req = valid_request();
        req.headers_mut()
            .append(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "repeated Content-Type must be 400: {text}"
        );
        assert_error(&text, "INVALID_ARGUMENT", "auth.login.bad_body");
        // Multi-valued Host (first value allowed): also rejected fixed.
        let mut req = valid_request();
        req.headers_mut()
            .append(header::HOST, HeaderValue::from_static(HOST));
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "multi-valued Host must be 403: {text}"
        );
        assert_error(&text, "CSRF_INVALID", "security.origin_host");
    }

    #[tokio::test]
    async fn login_rejects_bad_body_shapes() {
        let router = router();
        let cases = [
            (
                format!("{{\"username\":\"{USERNAME}\",\"password\":\"{PASSWORD}\",\"extra\":1}}"),
                Some("application/json"),
                "extra field",
            ),
            (
                format!("{{\"username\":\"{USERNAME}\"}}"),
                Some("application/json"),
                "missing password",
            ),
            (
                format!("{{\"username\":1,\"password\":\"{PASSWORD}\"}}"),
                Some("application/json"),
                "non-string username",
            ),
            (
                format!("{{\"username\":\"\",\"password\":\"{PASSWORD}\"}}"),
                Some("application/json"),
                "empty username",
            ),
            (
                format!("{{\"username\":\"{USERNAME}\",\"password\":\"\"}}"),
                Some("application/json"),
                "empty password",
            ),
            (
                "not-json".into(),
                Some("application/json"),
                "malformed json",
            ),
            (
                "{}".to_string(),
                Some("application/json"),
                "missing both fields",
            ),
            (
                body_json(USERNAME, PASSWORD),
                Some("text/plain"),
                "non-json content type",
            ),
            (body_json(USERNAME, PASSWORD), None, "missing content type"),
        ];
        for (body, content_type, label) in cases {
            let (status, _headers, _value, text) = call(
                &router,
                login_request(
                    body,
                    Some(HOST),
                    Some(ORIGIN),
                    content_type,
                    Some(PEER.parse().unwrap()),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{label} must be 400");
            assert_error(&text, "INVALID_ARGUMENT", "auth.login.bad_body");
        }
    }

    #[tokio::test]
    async fn login_invalid_username_rejected_before_rate_limiter() {
        let auth = FakeAuth::new(FakeLogin::InvalidArgument);
        let router = build_http_router(app_state(auth.clone()));
        let long_username = "a".repeat(65);
        let bad_usernames: Vec<&str> = vec![
            "Admin",
            &long_username,
            ".admin",
            "ab",
            "a b",
            "ad-min!",
            "_x",
        ];
        for username in &bad_usernames {
            let (status, _headers, _value, text) = call(
                &router,
                login_request(
                    body_json(username, PASSWORD),
                    Some(HOST),
                    Some(ORIGIN),
                    Some("application/json"),
                    Some(PEER.parse().unwrap()),
                ),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "invalid username {username:?} must be 400"
            );
            assert_error(&text, "INVALID_ARGUMENT", "auth.login.bad_body");
        }
        // None of the 400s may have touched the limiter: the first real
        // credential failure is still a plain 401, and the pair is blocked
        // only after five recorded failures.
        for i in 0..5u32 {
            let (status, _headers, _value, text) = call(
                &router,
                login_request(
                    body_json(USERNAME, "wrong"),
                    Some(HOST),
                    Some(ORIGIN),
                    Some("application/json"),
                    Some(PEER.parse().unwrap()),
                ),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "failure #{i} must be 401, not rate-limited"
            );
            assert_error(&text, "INVALID_CREDENTIALS", "auth.invalid_credentials");
            assert!(!text.contains(USERNAME), "401 must not echo the username");
        }
        let (status, headers, _value, text) = call(
            &router,
            login_request(
                body_json(USERNAME, "wrong"),
                Some(HOST),
                Some(ORIGIN),
                Some("application/json"),
                Some(PEER.parse().unwrap()),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "6th attempt must be 429"
        );
        assert_error(&text, "RATE_LIMITED", "auth.rate_limited");
        let retry = headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        assert!(
            (1..=900).contains(&retry),
            "Retry-After must be 1..=900s, got {retry}"
        );
    }

    #[tokio::test]
    async fn login_rate_limited_after_five_failures() {
        let router = build_http_router(app_state(FakeAuth::new(FakeLogin::InvalidArgument)));
        let request = || {
            login_request(
                body_json(USERNAME, "wrong"),
                Some(HOST),
                Some(ORIGIN),
                Some("application/json"),
                Some(PEER.parse().unwrap()),
            )
        };
        for i in 0..5u32 {
            let (status, _headers, _value, text) = call(&router, request()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "failure #{i} must be 401");
            assert_error(&text, "INVALID_CREDENTIALS", "auth.invalid_credentials");
        }
        let (status, headers, _value, text) = call(&router, request()).await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "6th attempt must be 429"
        );
        assert_error(&text, "RATE_LIMITED", "auth.rate_limited");
        let retry = headers
            .get(header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        assert!(
            (1..=900).contains(&retry),
            "Retry-After must be 1..=900s, got {retry}"
        );
        assert!(!text.contains(USERNAME), "429 must not echo the username");
    }

    #[tokio::test]
    async fn backend_failure_maps_503_and_never_counts_as_failure() {
        let auth = FakeAuth::new(FakeLogin::Database);
        let router = build_http_router(app_state(auth.clone()));
        let request = || {
            login_request(
                body_json(USERNAME, "wrong"),
                Some(HOST),
                Some(ORIGIN),
                Some("application/json"),
                Some(PEER.parse().unwrap()),
            )
        };
        for _ in 0..5 {
            let (status, _headers, _value, text) = call(&router, request()).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_error(&text, "STORAGE_UNAVAILABLE", "system.storage_unavailable");
        }
        for (behavior, code, key) in [
            (FakeLogin::Crypto, "NOT_READY", "system.not_ready"),
            (FakeLogin::Config, "NOT_READY", "system.not_ready"),
            (FakeLogin::SchemaNotReady, "NOT_READY", "system.not_ready"),
        ] {
            auth.set_behavior(behavior);
            let (status, _headers, _value, text) = call(&router, request()).await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{behavior:?} must be 503"
            );
            assert_error(&text, code, key);
        }
        // The seven 503s must not have incremented the limiter.
        auth.set_behavior(FakeLogin::InvalidArgument);
        let (status, _headers, _value, text) = call(&router, request()).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "backend 503s must never be counted as credential failures"
        );
        assert_error(&text, "INVALID_CREDENTIALS", "auth.invalid_credentials");
    }

    #[tokio::test]
    async fn login_missing_connect_info_is_503_not_ready() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let router = build_http_router(app_state(auth.clone()));
        let no_peer = login_request(
            body_json(USERNAME, "wrong"),
            Some(HOST),
            Some(ORIGIN),
            Some("application/json"),
            None,
        );
        let (status, _headers, _value, text) = call(&router, no_peer).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "missing ConnectInfo must be 503"
        );
        assert_error(&text, "NOT_READY", "system.not_ready");
        // The 503 must not have been recorded in the limiter: the first real
        // credential failure is still a plain 401, not a 429.
        let wrong = login_request(
            body_json(USERNAME, "wrong"),
            Some(HOST),
            Some(ORIGIN),
            Some("application/json"),
            Some(PEER.parse().unwrap()),
        );
        let (status, _headers, _value, text) = call(&router, wrong).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "next real attempt must be 401"
        );
        assert_error(&text, "INVALID_CREDENTIALS", "auth.invalid_credentials");
    }

    #[tokio::test]
    async fn login_oversized_body_is_fixed_400() {
        let router = router();
        // Declared Content-Length above the 1 MiB cap: rejected before any
        // body read.
        let mut pre = valid_request();
        pre.headers_mut()
            .insert(header::CONTENT_LENGTH, "2097152".parse().unwrap());
        let (status, _headers, _value, text) = call(&router, pre).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "CL precheck must be 400");
        assert_error(&text, "INVALID_ARGUMENT", "auth.login.bad_body");
        // No Content-Length, actual body above the cap: bounded read fails.
        let oversized = "a".repeat(1_048_577);
        let req = login_request(
            oversized,
            Some(HOST),
            Some(ORIGIN),
            Some("application/json"),
            Some(PEER.parse().unwrap()),
        );
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "oversized chunked-style body must be 400"
        );
        assert_error(&text, "INVALID_ARGUMENT", "auth.login.bad_body");
        // Exactly at the cap: the bounded read succeeds, the payload is
        // simply not a valid login body — still the fixed 400.
        let at_cap = "a".repeat(1_048_576);
        let req = login_request(
            at_cap,
            Some(HOST),
            Some(ORIGIN),
            Some("application/json"),
            Some(PEER.parse().unwrap()),
        );
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&text, "INVALID_ARGUMENT", "auth.login.bad_body");
    }

    #[tokio::test]
    async fn login_password_over_512_bytes_is_400_before_auth_and_limiter() {
        let auth = FakeAuth::new(FakeLogin::InvalidArgument);
        let state = app_state(auth.clone());
        let router = build_http_router(state);

        // Exactly 512 UTF-8 bytes (spec 01 A-02 cap): the new limit must
        // NOT reject it — it reaches the backend (401 under the fake).
        let at_cap = "a".repeat(512);
        let (status, _headers, _value, text) = call(
            &router,
            login_request(
                body_json(USERNAME, &at_cap),
                Some(HOST),
                Some(ORIGIN),
                Some("application/json"),
                Some(PEER.parse().unwrap()),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "512-byte password must not be rejected by the cap: {text}"
        );
        assert_error(&text, "INVALID_CREDENTIALS", "auth.invalid_credentials");

        // 513 UTF-8 bytes: fixed 400, never reaching the backend or the
        // limiter (fresh user + fresh source, so both dimensions are clean).
        let over = "a".repeat(513);
        let (status, _headers, _value, text) = call(
            &router,
            login_request(
                body_json("user2", &over),
                Some(HOST),
                Some(ORIGIN),
                Some("application/json"),
                Some(PEER2.parse().unwrap()),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "513-byte password must be a fixed 400: {text}"
        );
        assert_error(&text, "INVALID_ARGUMENT", "auth.login.bad_body");
        assert_eq!(
            auth.login_calls(),
            1,
            "only the 512-byte attempt may reach AuthServiceApi::login"
        );

        // The 400 must not have touched the limiter either: user2 (PEER2)
        // is blocked only after its own five real failures.
        let wrong = || {
            login_request(
                body_json("user2", "wrong"),
                Some(HOST),
                Some(ORIGIN),
                Some("application/json"),
                Some(PEER2.parse().unwrap()),
            )
        };
        for i in 0..5u32 {
            let (status, _headers, _value, text) = call(&router, wrong()).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "user2 failure #{i} must be 401, not rate-limited: {text}"
            );
            assert_error(&text, "INVALID_CREDENTIALS", "auth.invalid_credentials");
        }
        let (status, _headers, _value, text) = call(&router, wrong()).await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "user2 6th attempt must be 429: {text}"
        );
        assert_error(&text, "RATE_LIMITED", "auth.rate_limited");
    }

    #[tokio::test]
    async fn csrf_capacity_exhausted_is_503_retry_after_60() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        {
            // Every slot is LIVE (see `seed_live_csrf_bindings`): the hard
            // cap must fail closed even though dead slots would be swept.
            let mut csrf = state.auth.csrf.lock().unwrap();
            seed_live_csrf_bindings(&auth, &mut csrf, MAX_CSRF_ENTRIES);
        }
        let router = build_http_router(state);
        let (status, headers, _value, text) = call(&router, valid_request()).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "CSRF map at capacity must fail closed with 503"
        );
        assert_error(&text, "RESOURCE_EXHAUSTED", "system.resource_exhausted");
        assert_eq!(
            headers
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("60"),
            "resource exhaustion must carry Retry-After: 60"
        );
        assert!(!text.contains(PASSWORD), "no credential leak at capacity");
        assert!(!text.contains(USERNAME), "no username leak at capacity");
    }

    #[tokio::test]
    async fn csrf_capacity_full_short_circuits_before_auth_login() {
        // The map is already at the 4096 cap: the handler must fail closed
        // with 503 RESOURCE_EXHAUSTED + Retry-After: 60 BEFORE calling
        // AuthServiceApi::login — even when the backend would otherwise
        // answer with its own Database 503.
        let auth = FakeAuth::new(FakeLogin::Database);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            seed_live_csrf_bindings(&auth, &mut csrf, MAX_CSRF_ENTRIES);
        }
        let router = build_http_router(state);
        let (status, headers, _value, text) = call(&router, valid_request()).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "CSRF map at capacity must fail closed with 503: {text}"
        );
        assert_error(&text, "RESOURCE_EXHAUSTED", "system.resource_exhausted");
        assert_eq!(
            headers
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("60"),
            "capacity fail-closed must carry Retry-After: 60"
        );
        assert!(
            !text.contains(PASSWORD),
            "no credential leak at capacity: {text}"
        );
        assert_eq!(
            auth.login_calls(),
            0,
            "capacity peek must run BEFORE AuthServiceApi::login"
        );
        assert_eq!(
            auth_state.csrf.lock().unwrap().len(),
            MAX_CSRF_ENTRIES,
            "the full map must stay untouched"
        );
    }

    #[tokio::test]
    async fn login_success_keeps_authoritative_in_lock_capacity_check() {
        // Regression guard for the concurrent case: even when the
        // handler-level peek is bypassed, the in-lock check inside
        // `login_success` must still fail closed for a NEW digest at the
        // cap, without touching the map. Every seeded slot is LIVE, so
        // neither prune can free anything.
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        {
            let mut csrf = state.auth.csrf.lock().unwrap();
            seed_live_csrf_bindings(&auth, &mut csrf, MAX_CSRF_ENTRIES);
        }
        let response = login_success(state.auth.as_ref(), FakeAuth::fixed_login());
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "in-lock cap check must still fail closed: {text}"
        );
        assert_error(&text, "RESOURCE_EXHAUSTED", "system.resource_exhausted");
        assert_eq!(
            headers
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("60"),
            "in-lock cap check must carry Retry-After: 60"
        );
        assert_eq!(
            state.auth.csrf.lock().unwrap().len(),
            MAX_CSRF_ENTRIES,
            "a rejected binding must not grow the full map"
        );
    }
}

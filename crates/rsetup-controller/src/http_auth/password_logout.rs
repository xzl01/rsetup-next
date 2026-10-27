use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde_json::json;

use super::login::MAX_PASSWORD_BYTES;
use super::rate_limit::WindowRateLimitError;
use super::state::HttpAuthState;
use crate::auth::session::token_digest;
use crate::error::ControllerError;
use crate::http_api::{err_response, ok_response};
use crate::http_gate::{
    MAX_BODY_BYTES, auth_required, backend_error, csrf_invalid, exhausted, get_single_origin,
    json_content_type, origin_host_invalid, rate_limited, session_cookie_value, single_header,
};
use crate::http_security::{check_session_write, clear_session_cookie_header};

/// POST /api/v1/auth/password (Task 3c, fake-backed).
pub(super) async fn password(
    State(state): State<Arc<HttpAuthState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // 1. Host single & exact match.
    let host = single_header(&headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return origin_host_invalid();
    }

    // 2. Origin: exactly 0 (exempt) or exactly 1 matching allowed_origin.
    let origin = match get_single_origin(&headers) {
        Ok(o) => o,
        Err(()) => return origin_host_invalid(),
    };
    if origin.is_some_and(|o| state.config.allowed_origin.as_deref() != Some(o)) {
        return origin_host_invalid();
    }

    // 3. Exactly one valid session cookie.
    let Some(raw) = session_cookie_value(&headers) else {
        return auth_required();
    };

    // 4. Authenticate session.
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(s) => s,
        Err(ControllerError::InvalidArgument) => {
            state
                .csrf
                .lock()
                .unwrap()
                .remove(&token_digest(raw.as_bytes()));
            return auth_required();
        }
        Err(error) => return backend_error(&error),
    };

    // 5. X-CSRF-Token and bound CSRF check.
    let csrf_header = single_header(&headers, HeaderName::from_static("x-csrf-token"));
    let binding = {
        let csrf_map = state.csrf.lock().unwrap();
        csrf_map.get(&session.digest).cloned()
    };
    let Some(binding) = binding else {
        return csrf_invalid();
    };
    if check_session_write(host, origin, csrf_header, &binding.token, &state.config).is_err() {
        return csrf_invalid();
    }

    // 6. Rate gate: authenticated session write 20/s.
    let now = Instant::now();
    {
        let mut limiter = state.write_limiter.lock().unwrap();
        match limiter.check_and_record(now, &session.digest) {
            Err(WindowRateLimitError::Exhausted) => return exhausted(),
            Err(WindowRateLimitError::Limited(remaining)) => return rate_limited(remaining),
            Ok(()) => {}
        }
    }

    // 7. Content-Type and bounded body read.
    if !json_content_type(single_header(&headers, header::CONTENT_TYPE)) {
        return bad_password_body();
    }
    let declared_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return bad_password_body();
    }
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return bad_password_body(),
    };

    // 7. Parse body shape.
    let (current, new_password) = match parse_password_body(&bytes) {
        Some(pair) => pair,
        None => return bad_password_body(),
    };

    // 8. Service call & error resolution.
    match state
        .auth
        .change_password(&session, &current, &new_password)
        .await
    {
        Ok(()) => {
            // Success: clear session cookie and remove CSRF binding.
            state.csrf.lock().unwrap().remove(&session.digest);
            let mut response = ok_response(json!({ "changed": true }));
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&clear_session_cookie_header()).unwrap(),
            );
            response
        }
        Err(ControllerError::InvalidArgument) => {
            // Ambiguity resolution: re-check session validity.
            match state.auth.authenticate(raw.as_str()).await {
                Ok(_) => err_response(
                    StatusCode::BAD_REQUEST,
                    "INVALID_ARGUMENT",
                    "auth.password.change_failed",
                    json!({}),
                ),
                Err(ControllerError::InvalidArgument) => {
                    state
                        .csrf
                        .lock()
                        .unwrap()
                        .remove(&token_digest(raw.as_bytes()));
                    auth_required()
                }
                Err(error) => backend_error(&error),
            }
        }
        Err(ControllerError::RevisionConflict) => err_response(
            StatusCode::CONFLICT,
            "REVISION_CONFLICT",
            "auth.password.revision_conflict",
            json!({}),
        ),
        Err(error) => backend_error(&error),
    }
}

/// POST /api/v1/auth/logout (Task 3c, fake-backed).
pub(super) async fn logout(
    State(state): State<Arc<HttpAuthState>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    // 1. Host single & exact match.
    let host = single_header(&headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return origin_host_invalid();
    }

    // 2. Origin: exactly 0 (exempt) or exactly 1 matching allowed_origin.
    let origin = match get_single_origin(&headers) {
        Ok(o) => o,
        Err(()) => return origin_host_invalid(),
    };
    if origin.is_some_and(|o| state.config.allowed_origin.as_deref() != Some(o)) {
        return origin_host_invalid();
    }

    // 3. Exactly one valid session cookie.
    let Some(raw) = session_cookie_value(&headers) else {
        return auth_required();
    };

    // 4. Authenticate session.
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(s) => s,
        Err(ControllerError::InvalidArgument) => {
            state
                .csrf
                .lock()
                .unwrap()
                .remove(&token_digest(raw.as_bytes()));
            return auth_required();
        }
        Err(error) => return backend_error(&error),
    };

    // 5. X-CSRF-Token and bound CSRF check.
    let csrf_header = single_header(&headers, HeaderName::from_static("x-csrf-token"));
    let binding = {
        let csrf_map = state.csrf.lock().unwrap();
        csrf_map.get(&session.digest).cloned()
    };
    let Some(binding) = binding else {
        return csrf_invalid();
    };
    if check_session_write(host, origin, csrf_header, &binding.token, &state.config).is_err() {
        return csrf_invalid();
    }

    // 6. Rate gate: authenticated session write 20/s.
    let now = Instant::now();
    {
        let mut limiter = state.write_limiter.lock().unwrap();
        match limiter.check_and_record(now, &session.digest) {
            Err(WindowRateLimitError::Exhausted) => return exhausted(),
            Err(WindowRateLimitError::Limited(remaining)) => return rate_limited(remaining),
            Ok(()) => {}
        }
    }

    // 7. Content-Type and bounded body read.
    if !json_content_type(single_header(&headers, header::CONTENT_TYPE)) {
        return bad_logout_body();
    }
    let declared_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return bad_logout_body();
    }
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return bad_logout_body(),
    };

    // 7. Parse body shape: exactly `{}`.
    if !parse_logout_body(&bytes) {
        return bad_logout_body();
    }

    // 8. Service call & error resolution.
    match state.auth.logout(&session).await {
        Ok(()) => {
            // Success: clear session cookie and remove CSRF binding.
            state.csrf.lock().unwrap().remove(&session.digest);
            let mut response = ok_response(json!({ "logged_out": true }));
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&clear_session_cookie_header()).unwrap(),
            );
            response
        }
        Err(error) => backend_error(&error),
    }
}

/// Exactly `{current_password: string, new_password: string}`, both non-empty,
/// each at most 512 UTF-8 bytes.
fn parse_password_body(bytes: &[u8]) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    if object.len() != 2
        || !object.contains_key("current_password")
        || !object.contains_key("new_password")
    {
        return None;
    }
    let current = object.get("current_password")?.as_str()?.to_string();
    let new_password = object.get("new_password")?.as_str()?.to_string();
    if current.is_empty() || new_password.is_empty() {
        return None;
    }
    if current.len() > MAX_PASSWORD_BYTES || new_password.len() > MAX_PASSWORD_BYTES {
        return None;
    }
    Some((current, new_password))
}

/// Exactly `{}` (empty JSON object).
pub(super) fn parse_logout_body(bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    value.as_object().map(|o| o.is_empty()).unwrap_or(false)
}

fn bad_password_body() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "auth.password.bad_body",
        json!({}),
    )
}

fn bad_logout_body() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "auth.logout.bad_body",
        json!({}),
    )
}

#[cfg(test)]
mod tests {
    use super::super::csrf::CsrfBinding;
    use super::super::router::build_http_router;
    use super::super::tests_support::{
        FakeAuth, FakeLogin, FakeLogout, FakePassword, FakeSession, HOST, ORIGIN, app_state,
        assert_error, call, fake_token, logout_request, password_body, password_request,
        seed_session_and_csrf, seed_session_and_csrf_custom,
    };
    use super::*;
    use axum::http::{HeaderName, HeaderValue, StatusCode, header};

    #[tokio::test]
    async fn password_success_contract() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = password_request(
            password_body("correct-old-password", "new-valid-password-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, value, text) = call(&router, req).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "password success must be 200: {text}"
        );
        assert_eq!(
            value.as_object().map(|o| o.len()),
            Some(2),
            "envelope must be data + request_id: {text}"
        );
        assert!(value["request_id"].is_string(), "text: {text}");
        let data = value["data"].as_object().unwrap();
        assert_eq!(
            data.len(),
            1,
            "data must contain exactly changed:true: {text}"
        );
        assert_eq!(data["changed"], json!(true));
        assert!(
            !data.contains_key("user"),
            "data must not contain user: {text}"
        );
        assert!(!text.contains("correct-old-password"));
        assert!(!text.contains("new-valid-password-1234"));
        assert!(!text.contains("argon2"));

        let set_cookie = headers
            .get(header::SET_COOKIE)
            .expect("must set Set-Cookie")
            .to_str()
            .unwrap();
        assert!(
            set_cookie.contains("Max-Age=0"),
            "Set-Cookie must clear session: {set_cookie}"
        );
        assert!(set_cookie.contains("rsc_session="));

        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(
            !state.auth.csrf.lock().unwrap().contains_key(&digest),
            "CSRF binding must be removed on success"
        );
        assert_eq!(auth.authenticate_calls(), 1);
        assert_eq!(auth.change_password_calls(), 1);
    }

    #[tokio::test]
    async fn password_wrong_old_password_is_400_keeps_state() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_password_behavior(FakePassword::WrongOld);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = password_request(
            password_body("wrong-old", "new-valid-password-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "text: {text}");
        assert_error(&text, "INVALID_ARGUMENT", "auth.password.change_failed");
        assert!(headers.get(header::SET_COOKIE).is_none());

        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(
            state.auth.csrf.lock().unwrap().contains_key(&digest),
            "binding must be preserved on 400"
        );
        assert_eq!(
            auth.authenticate_calls(),
            2,
            "must call authenticate twice (initial + re-check)"
        );
        assert_eq!(auth.change_password_calls(), 1);
    }

    #[tokio::test]
    async fn password_wrong_old_vs_concurrent_revocation_distinguished() {
        // Case 1: WrongOld -> 400 + binding kept
        let auth1 = FakeAuth::new(FakeLogin::Ok);
        auth1.set_password_behavior(FakePassword::WrongOld);
        let state1 = app_state(auth1.clone());
        let (cookie1, csrf1) = seed_session_and_csrf(&state1);
        let router1 = build_http_router(state1.clone());
        let req1 = password_request(
            password_body("wrong-old", "new-valid-password-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie1),
            Some(&csrf1),
            Some("application/json"),
        );
        let (status1, _headers1, _value1, text1) = call(&router1, req1).await;
        assert_eq!(status1, StatusCode::BAD_REQUEST);
        assert_error(&text1, "INVALID_ARGUMENT", "auth.password.change_failed");
        let digest1 = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state1.auth.csrf.lock().unwrap().contains_key(&digest1));

        // Case 2: ConcurrentlyRevoked -> 401 + binding removed
        let auth2 = FakeAuth::new(FakeLogin::Ok);
        auth2.set_password_behavior(FakePassword::ConcurrentlyRevoked);
        let state2 = app_state(auth2.clone());
        let (cookie2, csrf2) = seed_session_and_csrf(&state2);
        let router2 = build_http_router(state2.clone());
        let req2 = password_request(
            password_body("correct-old", "new-valid-password-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie2),
            Some(&csrf2),
            Some("application/json"),
        );
        let (status2, _headers2, _value2, text2) = call(&router2, req2).await;
        assert_eq!(status2, StatusCode::UNAUTHORIZED);
        assert_error(&text2, "AUTH_REQUIRED", "auth.required");
        let digest2 = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(!state2.auth.csrf.lock().unwrap().contains_key(&digest2));
    }

    #[tokio::test]
    async fn password_new_password_invalid_is_400() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_password_behavior(FakePassword::NewInvalid);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = password_request(
            password_body("correct-old", "short"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&text, "INVALID_ARGUMENT", "auth.password.change_failed");
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
        assert_eq!(auth.authenticate_calls(), 2);
    }

    #[tokio::test]
    async fn password_recheck_db_fault_is_503_keeps_binding() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_password_behavior(FakePassword::WrongOld);
        // push re-check behavior to return Database fault
        auth.push_authenticate_step(FakeSession::Ok); // 1st call
        auth.push_authenticate_step(FakeSession::Database); // 2nd call (re-check)
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = password_request(
            password_body("old", "new-valid-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "STORAGE_UNAVAILABLE", "system.storage_unavailable");
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
        assert_eq!(auth.authenticate_calls(), 2);
    }

    #[tokio::test]
    async fn password_db_fault_is_503_without_recheck() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_password_behavior(FakePassword::Database);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = password_request(
            password_body("old", "new-valid-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "STORAGE_UNAVAILABLE", "system.storage_unavailable");
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
        assert_eq!(
            auth.authenticate_calls(),
            1,
            "must NOT re-check on direct Database error"
        );
    }

    #[tokio::test]
    async fn password_revision_conflict_is_409() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_password_behavior(FakePassword::RevisionConflict);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = password_request(
            password_body("old", "new-valid-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_error(
            &text,
            "REVISION_CONFLICT",
            "auth.password.revision_conflict",
        );
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
        assert_eq!(auth.authenticate_calls(), 1);
    }

    #[tokio::test]
    async fn password_crypto_is_503_not_ready() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_password_behavior(FakePassword::Crypto);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = password_request(
            password_body("old", "new-valid-1234"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "NOT_READY", "system.not_ready");
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
        assert_eq!(auth.authenticate_calls(), 1);
    }

    #[tokio::test]
    async fn logout_success_contract() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            value.as_object().map(|o| o.len()),
            Some(2),
            "envelope must be data + request_id: {text}"
        );
        let data = value["data"].as_object().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data["logged_out"], json!(true));

        let set_cookie = headers
            .get(header::SET_COOKIE)
            .expect("must set Set-Cookie")
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("Max-Age=0"));

        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(!state.auth.csrf.lock().unwrap().contains_key(&digest));
        assert_eq!(auth.logout_calls(), 1);

        // Idempotency: second sequential call with same cookie -> 401 AUTH_REQUIRED
        // Re-seed CSRF binding just in case to verify authenticate stops it
        state.auth.csrf.lock().unwrap().insert(
            digest,
            CsrfBinding {
                token: csrf.clone(),
                issued_at: Instant::now(),
            },
        );
        let req2 = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status2, _headers2, _value2, text2) = call(&router, req2).await;
        assert_eq!(status2, StatusCode::UNAUTHORIZED);
        assert_error(&text2, "AUTH_REQUIRED", "auth.required");
        assert_eq!(
            auth.logout_calls(),
            1,
            "must NOT call logout again on dead session"
        );
    }

    #[tokio::test]
    async fn logout_db_fault_is_503_no_claim() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_logout_behavior(FakeLogout::Database);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "STORAGE_UNAVAILABLE", "system.storage_unavailable");
        assert!(headers.get(header::SET_COOKIE).is_none());
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
    }

    #[tokio::test]
    async fn logout_crypto_is_503_not_ready() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_logout_behavior(FakeLogout::Crypto);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, _value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "NOT_READY", "system.not_ready");
        assert!(headers.get(header::SET_COOKIE).is_none());
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
    }

    #[tokio::test]
    async fn write_endpoints_require_bound_csrf() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        // 1. Missing X-CSRF-Token -> 403
        let req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            None,
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.csrf");

        // 2. Wrong X-CSRF-Token -> 403
        let req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some("wrong-csrf-token"),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.csrf");

        // 3. Repeated X-CSRF-Token header -> 403
        let mut req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        req.headers_mut()
            .append("x-csrf-token", HeaderValue::from_static("other-token"));
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.csrf");

        // 4. Valid session but no binding (e.g. pruned) -> 403
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        state.auth.csrf.lock().unwrap().remove(&digest);
        let req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.csrf");

        assert_eq!(
            auth.change_password_calls(),
            0,
            "CSRF failure must happen before service call"
        );
    }

    #[tokio::test]
    async fn write_endpoints_host_origin_single_value() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        // 1. Repeated Host -> 403
        let mut req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        req.headers_mut()
            .append(header::HOST, HeaderValue::from_static(HOST));
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.origin_host");

        // 2. Repeated Origin -> 403
        let mut req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        req.headers_mut()
            .append("origin", HeaderValue::from_static("http://evil.example"));
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.origin_host");
        assert_eq!(auth.change_password_calls(), 0);

        // 3. Non-ASCII Origin -> 403
        let mut req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            None,
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        req.headers_mut().insert(
            HeaderName::from_static("origin"),
            HeaderValue::from_bytes(b"http://\xff\xfe").unwrap(),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.origin_host");

        // 4. Wrong Origin -> 403
        let req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            Some("http://wrong.example"),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_error(&text, "CSRF_INVALID", "security.origin_host");

        // 5. Missing Origin -> Accepted (non-browser client)
        let req = password_request(
            password_body("old", "new1234567890"),
            Some(HOST),
            None,
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "missing Origin must be accepted: {text}"
        );
        assert_eq!(auth.change_password_calls(), 1);
    }

    #[tokio::test]
    async fn write_endpoints_invalid_session_before_service() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let router = build_http_router(state.clone());

        // 1. Missing cookie -> 401
        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            None,
            Some("csrf"),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_error(&text, "AUTH_REQUIRED", "auth.required");

        // 2. Malformed cookie -> 401
        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some("rsc_session=short"),
            Some("csrf"),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_error(&text, "AUTH_REQUIRED", "auth.required");

        // 3. Repeated cookie -> 401
        let token = fake_token();
        let cookie_rep = format!("rsc_session={token}; rsc_session={token}");
        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie_rep),
            Some("csrf"),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_error(&text, "AUTH_REQUIRED", "auth.required");

        // 4. Authenticate Database -> 503 STORAGE_UNAVAILABLE, keeps binding
        let (cookie, csrf) = seed_session_and_csrf(&state);
        auth.set_session_behavior(FakeSession::Database);
        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "STORAGE_UNAVAILABLE", "system.storage_unavailable");
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));

        // 5. Authenticate Crypto -> 503 NOT_READY, keeps binding
        auth.set_session_behavior(FakeSession::Crypto);
        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "NOT_READY", "system.not_ready");
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));

        assert_eq!(auth.logout_calls(), 0);
    }

    #[tokio::test]
    async fn write_endpoints_bad_body_is_400() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        // Password bad body cases
        let pass_cases = [
            ("{}".to_string(), "empty object"),
            (
                "{\"current_password\":\"old\"}".to_string(),
                "missing new_password",
            ),
            (
                "{\"new_password\":\"new\"}".to_string(),
                "missing current_password",
            ),
            (
                "{\"current_password\":\"old\",\"new_password\":\"new\",\"extra\":1}".to_string(),
                "extra field",
            ),
            (
                "{\"current_password\":\"\",\"new_password\":\"new\"}".to_string(),
                "empty current_password",
            ),
            (
                "{\"current_password\":\"old\",\"new_password\":\"\"}".to_string(),
                "empty new_password",
            ),
            (
                "{\"current_password\":123,\"new_password\":\"new\"}".to_string(),
                "non-string current_password",
            ),
            (
                format!(
                    "{{\"current_password\":\"{}\",\"new_password\":\"new\"}}",
                    "a".repeat(513)
                ),
                "current_password > 512B",
            ),
            (
                format!(
                    "{{\"current_password\":\"old\",\"new_password\":\"{}\"}}",
                    "b".repeat(513)
                ),
                "new_password > 512B",
            ),
            ("not-json".to_string(), "malformed json"),
        ];

        for (body, label) in pass_cases {
            let req = password_request(
                body,
                Some(HOST),
                Some(ORIGIN),
                Some(&cookie),
                Some(&csrf),
                Some("application/json"),
            );
            let (status, _h, _v, text) = call(&router, req).await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "password bad body: {label}"
            );
            assert_error(&text, "INVALID_ARGUMENT", "auth.password.bad_body");
        }

        // Password non-json Content-Type
        let req = password_request(
            password_body("old", "new"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("text/plain"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&text, "INVALID_ARGUMENT", "auth.password.bad_body");

        // Password CL precheck > 1MiB
        let mut req = password_request(
            password_body("old", "new"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        req.headers_mut()
            .insert(header::CONTENT_LENGTH, "2097152".parse().unwrap());
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&text, "INVALID_ARGUMENT", "auth.password.bad_body");

        // Password chunked without CL > 1MiB
        let oversized = "a".repeat(1_048_577);
        let req = password_request(
            oversized,
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&text, "INVALID_ARGUMENT", "auth.password.bad_body");

        // Use a fresh session/cookie/csrf for logout test so 20/s write rate limit is not hit
        let logout_raw_token = hex::encode([0x77_u8; 32]);
        auth.extra_valid_tokens
            .lock()
            .unwrap()
            .insert(logout_raw_token.clone());
        let (logout_cookie, logout_csrf) = seed_session_and_csrf_custom(&state, &logout_raw_token);

        // Logout bad body cases (must be strictly `{}`)
        let logout_cases = [
            ("{\"extra\":1}".to_string(), "non-empty object"),
            ("[]".to_string(), "array"),
            ("\"string\"".to_string(), "scalar string"),
            ("123".to_string(), "scalar int"),
            ("true".to_string(), "scalar bool"),
            ("null".to_string(), "scalar null"),
            ("not-json".to_string(), "malformed json"),
        ];

        for (body, label) in logout_cases {
            let req = logout_request(
                body,
                Some(HOST),
                Some(ORIGIN),
                Some(&logout_cookie),
                Some(&logout_csrf),
                Some("application/json"),
            );
            let (status, _h, _v, text) = call(&router, req).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "logout bad body: {label}");
            assert_error(&text, "INVALID_ARGUMENT", "auth.logout.bad_body");
        }

        // Logout non-json Content-Type
        let req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&logout_cookie),
            Some(&logout_csrf),
            Some("text/plain"),
        );
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&text, "INVALID_ARGUMENT", "auth.logout.bad_body");

        // Logout CL precheck > 1MiB
        let mut req = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&logout_cookie),
            Some(&logout_csrf),
            Some("application/json"),
        );
        req.headers_mut()
            .insert(header::CONTENT_LENGTH, "2097152".parse().unwrap());
        let (status, _h, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_error(&text, "INVALID_ARGUMENT", "auth.logout.bad_body");

        assert_eq!(auth.change_password_calls(), 0);
        assert_eq!(auth.logout_calls(), 0);
    }
}

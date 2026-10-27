use std::sync::Arc;
use std::time::Instant;

use axum::body::{Body, to_bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde_json::json;

use super::password_logout::parse_logout_body;
use super::rate_limit::{EndpointId, WindowRateLimitError};
use super::state::HttpAuthState;
use crate::auth::session::token_digest;
use crate::error::ControllerError;
use crate::http_api::{err_response, ok_response};
use crate::http_gate::{
    MAX_BODY_BYTES, auth_required, backend_error, csrf_invalid, exhausted, get_single_origin,
    json_content_type, origin_host_invalid, password_change_required, rate_limited,
    session_cookie_value, single_header,
};
use crate::http_security::{check_session_write, clear_session_cookie_header};

/// Parse GET /api/v1/auth/sessions query string manually.
/// Strictly: total bytes <= 512, no '%', only known params limit and cursor,
/// limit must be decimal without leading zero in 1..=200, cursor exactly 132 lowercase hex.
fn parse_sessions_query(raw_query: Option<&str>) -> Result<(usize, Option<String>), ()> {
    let Some(q) = raw_query else {
        return Ok((50, None));
    };
    if q.is_empty() {
        return Ok((50, None));
    }
    if q.len() > 512 || q.contains('%') {
        return Err(());
    }

    let mut limit: Option<usize> = None;
    let mut cursor: Option<String> = None;

    for part in q.split('&') {
        if part.is_empty() {
            return Err(());
        }
        let (k, v) = part.split_once('=').ok_or(())?;
        match k {
            "limit" => {
                if limit.is_some() || v.is_empty() {
                    return Err(());
                }
                // Disallow leading zeros: "0" or starts with '0' when len > 1
                if v.starts_with('0') {
                    return Err(());
                }
                if !v.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(());
                }
                let val: usize = v.parse().map_err(|_| ())?;
                if !(1..=200).contains(&val) {
                    return Err(());
                }
                limit = Some(val);
            }
            "cursor" => {
                if cursor.is_some() || v.len() != 132 {
                    return Err(());
                }
                if !v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    return Err(());
                }
                cursor = Some(v.to_string());
            }
            _ => return Err(()),
        }
    }

    Ok((limit.unwrap_or(50), cursor))
}

/// GET /api/v1/auth/sessions
pub(super) async fn sessions_list(
    State(state): State<Arc<HttpAuthState>>,
    raw_query: RawQuery,
    headers: HeaderMap,
) -> Response {
    // 1. Host single & exact match.
    let host = single_header(&headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return origin_host_invalid();
    }

    // 2. GET does not check Origin.

    // 3. Exactly one valid session cookie.
    let Some(raw) = session_cookie_value(&headers) else {
        return auth_required();
    };

    // 4. Task 3f pre-filter (in-memory peek).
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return auth_required();
    }

    // 5. Authenticate session.
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(s) => s,
        Err(ControllerError::InvalidArgument) => {
            state.csrf.lock().unwrap().remove(&digest);
            return auth_required();
        }
        Err(error) => return backend_error(&error),
    };

    // 6. Rate gate: read_limiter (EndpointId::Sessions, key=(Sessions, digest), 50/s).
    let now = Instant::now();
    {
        let mut limiter = state.read_limiter.lock().unwrap();
        match limiter.check_and_record(now, &(EndpointId::Sessions, session.digest)) {
            Err(WindowRateLimitError::Exhausted) => return exhausted(),
            Err(WindowRateLimitError::Limited(remaining)) => return rate_limited(remaining),
            Ok(()) => {}
        }
    }

    // 7. Manual query parse (after auth and rate limiter).
    let (limit, cursor) = match parse_sessions_query(raw_query.0.as_deref()) {
        Ok(res) => res,
        Err(()) => return bad_sessions_query(),
    };

    // 8. Service call & error resolution.
    match state
        .auth
        .list_sessions(&session, cursor.as_deref(), limit)
        .await
    {
        Ok(page) => {
            let items: Vec<serde_json::Value> = page
                .items
                .into_iter()
                .map(|item| {
                    json!({
                        "id": item.id,
                        "current": item.current,
                        "created_time": item.created_time,
                    })
                })
                .collect();
            ok_response(json!({
                "items": items,
                "next_cursor": page.next_cursor,
            }))
        }
        Err(ControllerError::InvalidArgument) => {
            // Disambiguation: re-authenticate session
            match state.auth.authenticate(raw.as_str()).await {
                Ok(_) => bad_sessions_query(),
                Err(ControllerError::InvalidArgument) => {
                    state.csrf.lock().unwrap().remove(&session.digest);
                    auth_required()
                }
                Err(err) => backend_error(&err),
            }
        }
        Err(ControllerError::PermissionDenied) => password_change_required(),
        Err(ControllerError::NotFound) => sessions_not_found(),
        Err(ControllerError::ResourceExhausted) => exhausted(),
        Err(error) => backend_error(&error),
    }
}

/// POST /api/v1/auth/sessions/revoke-others
pub(super) async fn revoke_others_handler(
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

    // 4. Task 3f pre-filter.
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return auth_required();
    }

    // 5. Authenticate session.
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(s) => s,
        Err(ControllerError::InvalidArgument) => {
            state.csrf.lock().unwrap().remove(&digest);
            return auth_required();
        }
        Err(error) => return backend_error(&error),
    };

    // 6. CSRF & write limiter (key=digest, 20/s, shared).
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

    let now = Instant::now();
    {
        let mut limiter = state.write_limiter.lock().unwrap();
        match limiter.check_and_record(now, &session.digest) {
            Err(WindowRateLimitError::Exhausted) => return exhausted(),
            Err(WindowRateLimitError::Limited(remaining)) => return rate_limited(remaining),
            Ok(()) => {}
        }
    }

    // 7. Body check: application/json, <= 1MiB, exactly `{}`.
    if !json_content_type(single_header(&headers, header::CONTENT_TYPE)) {
        return bad_sessions_body();
    }
    let declared_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return bad_sessions_body();
    }
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return bad_sessions_body(),
    };
    if !parse_logout_body(&bytes) {
        return bad_sessions_body();
    }

    // 8. Service call & error resolution.
    match state.auth.revoke_others(&session).await {
        Ok(count) => ok_response(json!({ "revoked_count": count })),
        Err(ControllerError::InvalidArgument) => {
            match state.auth.authenticate(raw.as_str()).await {
                Ok(_) => invalid_sessions_request(),
                Err(ControllerError::InvalidArgument) => {
                    state.csrf.lock().unwrap().remove(&session.digest);
                    auth_required()
                }
                Err(err) => backend_error(&err),
            }
        }
        Err(ControllerError::PermissionDenied) => password_change_required(),
        Err(ControllerError::NotFound) => sessions_not_found(),
        Err(ControllerError::ResourceExhausted) => exhausted(),
        Err(error) => backend_error(&error),
    }
}

/// POST /api/v1/auth/sessions/{id}/revoke
pub(super) async fn revoke_by_id(
    State(state): State<Arc<HttpAuthState>>,
    Path(id): Path<String>,
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

    // 4. Task 3f pre-filter.
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return auth_required();
    }

    // 5. Authenticate session.
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(s) => s,
        Err(ControllerError::InvalidArgument) => {
            state.csrf.lock().unwrap().remove(&digest);
            return auth_required();
        }
        Err(error) => return backend_error(&error),
    };

    // 6. CSRF & write limiter (key=digest, 20/s, shared).
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

    let now = Instant::now();
    {
        let mut limiter = state.write_limiter.lock().unwrap();
        match limiter.check_and_record(now, &session.digest) {
            Err(WindowRateLimitError::Exhausted) => return exhausted(),
            Err(WindowRateLimitError::Limited(remaining)) => return rate_limited(remaining),
            Ok(()) => {}
        }
    }

    // 7. Body check: application/json, <= 1MiB, exactly `{}`.
    if !json_content_type(single_header(&headers, header::CONTENT_TYPE)) {
        return bad_sessions_body();
    }
    let declared_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared_length.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return bad_sessions_body();
    }
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return bad_sessions_body(),
    };
    if !parse_logout_body(&bytes) {
        return bad_sessions_body();
    }

    // Path {id} validation (after all gates, before service call): exactly 64 lowercase hex.
    if id.len() != 64 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return bad_sessions_id();
    }

    // 8. Service call & error resolution.
    match state.auth.revoke_by_alias(&session, &id).await {
        Ok(is_self) => {
            let mut response = ok_response(json!({ "revoked": true }));
            if is_self {
                state.csrf.lock().unwrap().remove(&session.digest);
                response.headers_mut().insert(
                    header::SET_COOKIE,
                    HeaderValue::from_str(&clear_session_cookie_header()).unwrap(),
                );
            }
            response
        }
        Err(ControllerError::InvalidArgument) => {
            match state.auth.authenticate(raw.as_str()).await {
                Ok(_) => invalid_sessions_request(),
                Err(ControllerError::InvalidArgument) => {
                    state.csrf.lock().unwrap().remove(&session.digest);
                    auth_required()
                }
                Err(err) => backend_error(&err),
            }
        }
        Err(ControllerError::PermissionDenied) => password_change_required(),
        Err(ControllerError::NotFound) => sessions_not_found(),
        Err(ControllerError::ResourceExhausted) => exhausted(),
        Err(error) => backend_error(&error),
    }
}

fn bad_sessions_query() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "auth.sessions.bad_query",
        json!({}),
    )
}

fn bad_sessions_id() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "auth.sessions.bad_id",
        json!({}),
    )
}

fn bad_sessions_body() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "auth.sessions.bad_body",
        json!({}),
    )
}

fn invalid_sessions_request() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "auth.sessions.invalid_request",
        json!({}),
    )
}

fn sessions_not_found() -> Response {
    err_response(
        StatusCode::NOT_FOUND,
        "NOT_FOUND",
        "system.not_found",
        json!({}),
    )
}

#[cfg(test)]
mod tests {
    use super::super::build_http_router;
    use super::super::tests::{
        FakeAuth, FakeLogin, FakeSession, HOST, ORIGIN, app_state, assert_error, call, fake_token,
        seed_session_and_csrf, valid_cookie,
    };
    use super::*;
    use crate::auth::service::SessionPage;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};

    // ---------------- Task 4: GET /api/v1/auth/sessions, POST revoke-others, POST {id}/revoke ----------------

    fn sessions_list_request(
        query: Option<&str>,
        cookie: Option<&str>,
        host: Option<&str>,
    ) -> Request<Body> {
        let uri = match query {
            Some(q) => format!("/api/v1/auth/sessions?{q}"),
            None => "/api/v1/auth/sessions".to_string(),
        };
        let mut builder = Request::builder().method("GET").uri(uri);
        if let Some(h) = host {
            builder = builder.header(header::HOST, h);
        } else {
            builder = builder.header(header::HOST, HOST);
        }
        if let Some(c) = cookie {
            builder = builder.header(header::COOKIE, c);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn revoke_others_req(
        body: String,
        host: Option<&str>,
        origin: Option<&str>,
        cookie: Option<&str>,
        csrf: Option<&str>,
        content_type: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/sessions/revoke-others");
        if let Some(h) = host {
            builder = builder.header(header::HOST, h);
        }
        if let Some(o) = origin {
            builder = builder.header("origin", o);
        }
        if let Some(c) = cookie {
            builder = builder.header(header::COOKIE, c);
        }
        if let Some(token) = csrf {
            builder = builder.header("x-csrf-token", token);
        }
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        builder.body(Body::from(body)).unwrap()
    }

    fn revoke_by_id_req(
        id: &str,
        body: String,
        host: Option<&str>,
        origin: Option<&str>,
        cookie: Option<&str>,
        csrf: Option<&str>,
        content_type: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/auth/sessions/{id}/revoke"));
        if let Some(h) = host {
            builder = builder.header(header::HOST, h);
        }
        if let Some(o) = origin {
            builder = builder.header("origin", o);
        }
        if let Some(c) = cookie {
            builder = builder.header(header::COOKIE, c);
        }
        if let Some(token) = csrf {
            builder = builder.header("x-csrf-token", token);
        }
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        builder.body(Body::from(body)).unwrap()
    }

    // 1. list 200 合同

    #[tokio::test]
    async fn test_group_1_list_sessions_200_contract() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let id_hex = "a".repeat(64);
        let cursor_hex = "b".repeat(132);
        auth.set_list_behavior(Ok(SessionPage {
            items: vec![crate::auth::service::SessionItem {
                id: id_hex.clone(),
                current: true,
                created_time: "2026-10-05T12:00:00Z".to_string(),
            }],
            next_cursor: Some(cursor_hex.clone()),
        }));
        let state = app_state(auth.clone());
        let router = build_http_router(state);

        // Default limit (omitted) -> 50
        let req = sessions_list_request(None, Some(&valid_cookie()), None);
        let (status, headers, value, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::OK, "text: {text}");
        assert_eq!(auth.last_list_limit(), 50);
        assert_eq!(headers.get(header::SET_COOKIE), None);
        let top_keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(top_keys, vec!["data", "request_id"]);
        let data = &value["data"];
        assert_eq!(data["next_cursor"], json!(cursor_hex));
        let items = data["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], json!(id_hex));
        assert_eq!(items[0]["current"], json!(true));
        assert_eq!(items[0]["created_time"], json!("2026-10-05T12:00:00Z"));
        // items shape check: strict bool
        assert!(items[0]["current"].is_boolean());

        // Explicit limit and next_cursor None -> null
        auth.set_list_behavior(Ok(SessionPage {
            items: vec![],
            next_cursor: None,
        }));
        let req2 = sessions_list_request(Some("limit=20"), Some(&valid_cookie()), None);
        let (status2, _h2, value2, text2) = call(&router, req2).await;
        assert_eq!(status2, StatusCode::OK, "text: {text2}");
        assert_eq!(auth.last_list_limit(), 20);
        assert_eq!(value2["data"]["next_cursor"], serde_json::Value::Null);
    }

    // 2. list 401 三态
    #[tokio::test]
    async fn test_group_2_list_sessions_401_three_states() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let router = build_http_router(state.clone());

        // 2a. No cookie
        let req_no_cookie = sessions_list_request(None, None, None);
        let (status1, _, _, text1) = call(&router, req_no_cookie).await;
        assert_eq!(status1, StatusCode::UNAUTHORIZED);
        assert_error(&text1, "AUTH_REQUIRED", "auth.required");

        // 2b. Prefilter miss (is_live_session = false)
        let digest = token_digest(fake_token().as_bytes());
        auth.set_digest_live(digest, false);
        let req_miss = sessions_list_request(None, Some(&valid_cookie()), None);
        let (status2, _, _, text2) = call(&router, req_miss).await;
        assert_eq!(status2, StatusCode::UNAUTHORIZED);
        assert_error(&text2, "AUTH_REQUIRED", "auth.required");
        assert_eq!(
            auth.list_calls(),
            0,
            "must short-circuit before service call"
        );

        // 2c. Session invalid (authenticate -> InvalidArgument), stale csrf removed
        auth.set_digest_live(digest, true);
        auth.set_session_behavior(FakeSession::InvalidArgument);
        let (_, csrf) = seed_session_and_csrf(&state);
        assert!(!csrf.is_empty());
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest));
        let req_dead = sessions_list_request(None, Some(&valid_cookie()), None);
        let (status3, _, _, text3) = call(&router, req_dead).await;
        assert_eq!(status3, StatusCode::UNAUTHORIZED);
        assert_error(&text3, "AUTH_REQUIRED", "auth.required");
        assert!(
            !state.auth.csrf.lock().unwrap().contains_key(&digest),
            "stale binding must be removed"
        );
    }

    // 3. list 403
    #[tokio::test]
    async fn test_group_3_list_sessions_403_force_password() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        auth.set_list_behavior(Err(ControllerError::PermissionDenied));
        let state = app_state(auth.clone());
        let router = build_http_router(state);

        let req = sessions_list_request(None, Some(&valid_cookie()), None);
        let (status, _, _, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "text: {text}");
        assert_error(
            &text,
            "PASSWORD_CHANGE_REQUIRED",
            "auth.password_change_required",
        );
    }

    // 4. list 400
    #[tokio::test]
    async fn test_group_4_list_sessions_400_query_validation_and_disambiguation() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let router = build_http_router(state.clone());
        let cookie = valid_cookie();
        let digest = token_digest(fake_token().as_bytes());

        let bad_queries = [
            "limit=0",
            "limit=201",
            "limit=abc",
            "limit=05",
            "limit=50&limit=50",
            "cursor=abc", // not 132 hex
            &format!("cursor={}&cursor={}", "a".repeat(132), "a".repeat(132)),
            "unknown=1",
            "limit=50%20",                           // % encoded
            &format!("unknown={}", "x".repeat(513)), // > 512 bytes
        ];

        for q in bad_queries {
            let req = sessions_list_request(Some(q), Some(&cookie), None);
            let (status, _, _, text) = call(&router, req).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "query: {q}, text: {text}");
            assert_error(&text, "INVALID_ARGUMENT", "auth.sessions.bad_query");
        }

        // Verify read quota was consumed by these bad query calls for authenticated session
        let entries = state.auth.read_limiter.lock().unwrap().entries.clone();
        assert!(entries.contains_key(&(EndpointId::Sessions, digest)));

        // Bad cursor returned by fake service as InvalidArgument -> disambiguate: recheck authenticate Ok -> 400 bad_query
        auth.set_list_behavior(Err(ControllerError::InvalidArgument));
        let valid_cursor = "c".repeat(132);
        let req_service_bad_cursor =
            sessions_list_request(Some(&format!("cursor={valid_cursor}")), Some(&cookie), None);
        let (status, _, _, text) = call(&router, req_service_bad_cursor).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "text: {text}");
        assert_error(&text, "INVALID_ARGUMENT", "auth.sessions.bad_query");

        // Session dead + fake InvalidArgument -> recheck authenticate fails -> 401
        auth.push_authenticate_step(FakeSession::InvalidArgument);
        let req_dead_cursor =
            sessions_list_request(Some(&format!("cursor={valid_cursor}")), Some(&cookie), None);
        let (status_dead, _, _, text_dead) = call(&router, req_dead_cursor).await;
        assert_eq!(status_dead, StatusCode::UNAUTHORIZED, "text: {text_dead}");
        assert_error(&text_dead, "AUTH_REQUIRED", "auth.required");
    }

    // 5. list 503/429
    #[tokio::test]
    async fn test_group_5_list_sessions_503_and_429() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let router = build_http_router(state);
        let cookie = valid_cookie();

        // 5a. ResourceExhausted -> 503 + Retry-After: 60
        auth.set_list_behavior(Err(ControllerError::ResourceExhausted));
        let req_ex = sessions_list_request(None, Some(&cookie), None);
        let (status, headers, _, text) = call(&router, req_ex).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "RESOURCE_EXHAUSTED", "system.resource_exhausted");
        assert_eq!(
            headers.get(header::RETRY_AFTER).unwrap().to_str().unwrap(),
            "60"
        );

        // 5b. Database error -> 503 STORAGE_UNAVAILABLE
        auth.set_list_behavior(Err(ControllerError::Database(sqlx::Error::Configuration(
            std::io::Error::other("synthetic db fault").into(),
        ))));
        let req_db = sessions_list_request(None, Some(&cookie), None);
        let (status_db, _, val_db, text_db) = call(&router, req_db).await;
        assert_eq!(status_db, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(
            &text_db,
            "STORAGE_UNAVAILABLE",
            "system.storage_unavailable",
        );
        assert!(val_db.get("data").is_none());

        // 5c. Rate limit: 50 requests in 1s succeed, 51st -> 429
        auth.set_list_behavior(Ok(SessionPage {
            items: vec![],
            next_cursor: None,
        }));
        let auth_fresh = FakeAuth::new(FakeLogin::Ok);
        let state_fresh = app_state(auth_fresh.clone());
        let router_fresh = build_http_router(state_fresh);

        for _ in 0..50 {
            let req = sessions_list_request(None, Some(&cookie), None);
            let (status, _, _, text) = call(&router_fresh, req).await;
            assert_eq!(status, StatusCode::OK, "text: {text}");
        }
        let req_51 = sessions_list_request(None, Some(&cookie), None);
        let (status_51, headers_51, _, text_51) = call(&router_fresh, req_51).await;
        assert_eq!(status_51, StatusCode::TOO_MANY_REQUESTS, "text: {text_51}");
        assert_error(&text_51, "RATE_LIMITED", "auth.rate_limited");
        assert_eq!(
            headers_51
                .get(header::RETRY_AFTER)
                .unwrap()
                .to_str()
                .unwrap(),
            "1"
        );
    }

    // 6. revoke-by-id 200
    #[tokio::test]
    async fn test_group_6_revoke_by_id_200() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let digest = token_digest(fake_token().as_bytes());
        let router = build_http_router(state.clone());
        let target_id = "f".repeat(64);

        // 6a. Ok(true) -> revoked: true + Set-Cookie clear + csrf binding removed
        auth.set_revoke_alias_result(Ok(true));
        let req_self = revoke_by_id_req(
            &target_id,
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, value, text) = call(&router, req_self).await;
        assert_eq!(status, StatusCode::OK, "text: {text}");
        assert_eq!(value["data"]["revoked"], json!(true));
        let cookie_hdr = headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(cookie_hdr.contains("Max-Age=0") || cookie_hdr.contains("rsc_session="));
        assert!(!state.auth.csrf.lock().unwrap().contains_key(&digest));

        // 6b. Ok(false) -> revoked: true + no Set-Cookie + csrf binding kept
        let (cookie2, csrf2) = seed_session_and_csrf(&state);
        let digest2 = token_digest(fake_token().as_bytes());
        auth.set_revoke_alias_result(Ok(false));
        let req_other = revoke_by_id_req(
            &target_id,
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie2),
            Some(&csrf2),
            Some("application/json"),
        );
        let (status2, headers2, value2, text2) = call(&router, req_other).await;
        assert_eq!(status2, StatusCode::OK, "text: {text2}");
        assert_eq!(value2["data"]["revoked"], json!(true));
        assert_eq!(headers2.get(header::SET_COOKIE), None);
        assert!(state.auth.csrf.lock().unwrap().contains_key(&digest2));
    }

    // 7. revoke-by-id 400/404
    #[tokio::test]
    async fn test_group_7_revoke_by_id_400_and_404() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state.clone());

        // 7a. id malformed (63 chars, 65 chars, uppercase, non-hex) -> 400 bad_id, 0 revoke calls
        let bad_ids = [
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            format!("{}g", "a".repeat(63)),
        ];
        for bid in bad_ids {
            let req = revoke_by_id_req(
                &bid,
                "{}".into(),
                Some(HOST),
                Some(ORIGIN),
                Some(&cookie),
                Some(&csrf),
                Some("application/json"),
            );
            let (status, _, _, text) = call(&router, req).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "id: {bid}, text: {text}");
            assert_error(&text, "INVALID_ARGUMENT", "auth.sessions.bad_id");
            assert_eq!(auth.revoke_alias_calls(), 0);
        }

        // 7b. service InvalidArgument + authenticate recheck Ok -> 400 auth.sessions.invalid_request
        let valid_id = "c".repeat(64);
        auth.set_revoke_alias_result(Err(ControllerError::InvalidArgument));
        let req_inval = revoke_by_id_req(
            &valid_id,
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status_inv, _, _, text_inv) = call(&router, req_inval).await;
        assert_eq!(status_inv, StatusCode::BAD_REQUEST, "text: {text_inv}");
        assert_error(
            &text_inv,
            "INVALID_ARGUMENT",
            "auth.sessions.invalid_request",
        );

        // 7c. fake NotFound -> 404 NOT_FOUND
        auth.set_revoke_alias_result(Err(ControllerError::NotFound));
        let req_nf = revoke_by_id_req(
            &valid_id,
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status_nf, _, _, text_nf) = call(&router, req_nf).await;
        assert_eq!(status_nf, StatusCode::NOT_FOUND, "text: {text_nf}");
        assert_error(&text_nf, "NOT_FOUND", "system.not_found");
    }

    // 8. revoke-by-id 403/400 body
    #[tokio::test]
    async fn test_group_8_revoke_by_id_403_and_400_body() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state);
        let id = "e".repeat(64);

        // Missing CSRF -> 403
        let req_no_csrf = revoke_by_id_req(
            &id,
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            None,
            Some("application/json"),
        );
        let (status1, _, _, text1) = call(&router, req_no_csrf).await;
        assert_eq!(status1, StatusCode::FORBIDDEN);
        assert_error(&text1, "CSRF_INVALID", "security.csrf");
        assert_eq!(auth.revoke_alias_calls(), 0);

        // Bad body shapes -> 400 auth.sessions.bad_body
        let bad_bodies = ["{\"foo\":1}", "", "[]", "not json"];
        for b in bad_bodies {
            let req_bad = revoke_by_id_req(
                &id,
                b.into(),
                Some(HOST),
                Some(ORIGIN),
                Some(&cookie),
                Some(&csrf),
                Some("application/json"),
            );
            let (status, _, _, text) = call(&router, req_bad).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "body: {b}, text: {text}");
            assert_error(&text, "INVALID_ARGUMENT", "auth.sessions.bad_body");
        }

        // Missing Content-Type -> 400
        let req_no_ct = revoke_by_id_req(
            &id,
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            None,
        );
        let (status_ct, _, _, text_ct) = call(&router, req_no_ct).await;
        assert_eq!(status_ct, StatusCode::BAD_REQUEST);
        assert_error(&text_ct, "INVALID_ARGUMENT", "auth.sessions.bad_body");
    }

    // 9. revoke-by-id 503
    #[tokio::test]
    async fn test_group_9_revoke_by_id_503() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state);
        let id = "d".repeat(64);

        auth.set_revoke_alias_result(Err(ControllerError::Database(sqlx::Error::Configuration(
            std::io::Error::other("synthetic db fault").into(),
        ))));
        let req = revoke_by_id_req(
            &id,
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, _, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "text: {text}");
        assert_error(&text, "STORAGE_UNAVAILABLE", "system.storage_unavailable");
        assert_eq!(headers.get(header::SET_COOKIE), None);
    }

    // 10. revoke-others
    #[tokio::test]
    async fn test_group_10_revoke_others() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state);

        // 10a. fake Ok(2) -> revoked_count: 2, no Set-Cookie
        auth.set_revoke_others_result(Ok(2));
        let req_ok = revoke_others_req(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, value, text) = call(&router, req_ok).await;
        assert_eq!(status, StatusCode::OK, "text: {text}");
        assert_eq!(value["data"]["revoked_count"], json!(2));
        assert_eq!(headers.get(header::SET_COOKIE), None);
        assert_eq!(auth.revoke_others_calls(), 1);

        // 10b. 403 force_password
        auth.set_revoke_others_result(Err(ControllerError::PermissionDenied));
        let req_perm = revoke_others_req(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status_perm, _, _, text_perm) = call(&router, req_perm).await;
        assert_eq!(status_perm, StatusCode::FORBIDDEN, "text: {text_perm}");
        assert_error(
            &text_perm,
            "PASSWORD_CHANGE_REQUIRED",
            "auth.password_change_required",
        );

        // 10c. 400 bad body
        let req_bad_body = revoke_others_req(
            "{\"extra\":1}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status_bb, _, _, text_bb) = call(&router, req_bad_body).await;
        assert_eq!(status_bb, StatusCode::BAD_REQUEST, "text: {text_bb}");
        assert_error(&text_bb, "INVALID_ARGUMENT", "auth.sessions.bad_body");

        // 10d. 503 db fault
        auth.set_revoke_others_result(Err(ControllerError::Database(sqlx::Error::Configuration(
            std::io::Error::other("synthetic db fault").into(),
        ))));
        let req_503 = revoke_others_req(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status_503, headers_503, _, text_503) = call(&router, req_503).await;
        assert_eq!(
            status_503,
            StatusCode::SERVICE_UNAVAILABLE,
            "text: {text_503}"
        );
        assert_error(
            &text_503,
            "STORAGE_UNAVAILABLE",
            "system.storage_unavailable",
        );
        assert_eq!(headers_503.get(header::SET_COOKIE), None);

        // 10e. rate limiter shared with password/logout (20/s, key=digest)
        let auth_rl = FakeAuth::new(FakeLogin::Ok);
        let state_rl = app_state(auth_rl.clone());
        let (cookie_rl, csrf_rl) = seed_session_and_csrf(&state_rl);
        let router_rl = build_http_router(state_rl);
        auth_rl.set_revoke_others_result(Ok(0));

        for _ in 0..20 {
            let req = revoke_others_req(
                "{}".into(),
                Some(HOST),
                Some(ORIGIN),
                Some(&cookie_rl),
                Some(&csrf_rl),
                Some("application/json"),
            );
            let (status, _, _, text) = call(&router_rl, req).await;
            assert_eq!(status, StatusCode::OK, "text: {text}");
        }
        let req_21 = revoke_others_req(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie_rl),
            Some(&csrf_rl),
            Some("application/json"),
        );
        let (status_21, headers_21, _, text_21) = call(&router_rl, req_21).await;
        assert_eq!(status_21, StatusCode::TOO_MANY_REQUESTS, "text: {text_21}");
        assert_error(&text_21, "RATE_LIMITED", "auth.rate_limited");
        assert_eq!(
            headers_21
                .get(header::RETRY_AFTER)
                .unwrap()
                .to_str()
                .unwrap(),
            "1"
        );
    }
}

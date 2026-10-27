use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use serde_json::json;

use super::csrf::{CsrfBinding, MAX_CSRF_ENTRIES, prune_csrf_table};
use super::rate_limit::{EndpointId, WindowRateLimitError};
use super::state::HttpAuthState;
use crate::auth::session::token_digest;
use crate::error::ControllerError;
use crate::http_api::{err_response, ok_response, user_public};
use crate::http_gate::{
    auth_required, backend_error, exhausted, rate_limited, session_cookie_value, single_header,
};

/// GET /api/v1/auth/me (Task 3b, fake-backed; Task 3f adds the pre-auth
/// clock gate; production wiring belongs to the later `LiveAuth` adapter).
/// Read-only: an exact allowed single `Host` is required; `Origin` and
/// `X-CSRF` are NOT. Exactly one well-formed `rsc_session` cookie (64
/// lowercase hex) across ALL `Cookie` headers; other cookie names are
/// ignored; zero or two-or-more occurrences fail closed as 401
/// `AUTH_REQUIRED`. Before any backend call, the cookie digest is checked
/// against the current process clock (`is_live_session`, a pure in-memory
/// peek): a miss is the same fixed 401 as a dead session and removes any
/// stale CSRF binding without touching the backend. A hit is only a
/// necessary condition — `auth.authenticate` remains the authority for
/// validity: only a definitive `InvalidArgument` removes the digest's stale
/// CSRF binding — transient backend faults are fixed 503s that keep it.
/// After successful authentication the current `authz_epoch` is read on
/// EVERY request (the production `schema_meta` count, never the process
/// `SessionClock` epoch), then the CSRF bound to `session.digest` is
/// reused or issued under the 4096 cap, after a monotonic prune of
/// past-`ABSOLUTE` bindings plus the threshold-gated dead-session sweep
/// (`prune_dead_csrf`). No raw cookie, hash or DB error ever reaches
/// the body or a log.
pub(super) async fn me(State(state): State<Arc<HttpAuthState>>, headers: HeaderMap) -> Response {
    // 1. Exact single allowed Host (missing/duplicated/wrong all fail).
    let host = single_header(&headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return err_response(
            StatusCode::FORBIDDEN,
            "CSRF_INVALID",
            "security.origin_host",
            json!({}),
        );
    }
    // 2. Exactly one well-formed session cookie value.
    let Some(raw) = session_cookie_value(&headers) else {
        return auth_required();
    };
    // 2b. (Task 3f) Pre-auth clock gate: a pure in-memory peek. A digest
    // missing from the current process clock gets the same fixed 401 as a
    // dead session and drops any stale CSRF binding — WITHOUT any backend
    // call. A hit is only a necessary condition: `authenticate` below stays
    // DB-authoritative, and the 50/s read gate, epoch and CSRF keep their
    // original order. This is not a substitute for generic network DoS
    // defenses.
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return auth_required();
    }
    // 3. Authenticate before trusting the user.
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(session) => session,
        Err(ControllerError::InvalidArgument) => {
            // A dead session must not keep its stale CSRF binding.
            state.csrf.lock().unwrap().remove(&digest);
            return auth_required();
        }
        Err(error) => return backend_error(&error),
    };
    // 4. Rate gate: authenticated endpoint read 50/s per (EndpointId, session.digest).
    //    Must be checked BEFORE reading authz_epoch and issuing/reusing CSRF.
    let now = Instant::now();
    {
        let mut limiter = state.read_limiter.lock().unwrap();
        match limiter.check_and_record(now, &(EndpointId::Me, session.digest)) {
            Err(WindowRateLimitError::Exhausted) => return exhausted(),
            Err(WindowRateLimitError::Limited(remaining)) => return rate_limited(remaining),
            Ok(()) => {}
        }
    }
    // 5. Per-request authorization epoch; never the process UUID.
    let epoch = match state.auth.authz_epoch().await {
        Ok(epoch) => epoch,
        Err(error) => return backend_error(&error),
    };
    // 6. Reuse or issue the CSRF bound to this digest. Both prunes run
    //    first under the same lock (monotonic 12h, plus the dead-session
    //    sweep once the table reaches the soft threshold); an existing
    //    binding is reused WITHOUT refreshing its issue instant. A table
    //    that is still at the cap after the sweep is full of LIVE bindings,
    //    so a new digest keeps failing closed with 503.
    let mut csrf = state.csrf.lock().unwrap();
    prune_csrf_table(&mut csrf, state.auth.as_ref(), now);
    let csrf_token = match csrf.get(&session.digest) {
        Some(binding) => binding.token.clone(),
        None => {
            if csrf.len() >= MAX_CSRF_ENTRIES {
                return exhausted();
            }
            let mut csrf_raw = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut csrf_raw);
            let token = hex::encode(csrf_raw);
            csrf.insert(
                session.digest,
                CsrfBinding {
                    token: token.clone(),
                    issued_at: now,
                },
            );
            token
        }
    };
    let user = user_public(&session.user);
    drop(csrf);
    ok_response(json!({
        "user": user,
        "csrf_token": csrf_token,
        "authz_epoch": epoch.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::super::router::build_http_router;
    use super::super::tests_support::{
        FakeAuth, FakeLogin, FakeSession, USERNAME, app_state, assert_error, call, fake_token,
        me_request, router, seed_live_csrf_bindings, valid_cookie,
    };
    use super::*;
    use axum::http::{StatusCode, header};

    // ---------------- Task 3b: GET /api/v1/auth/me (fake-backed) ----------------

    #[tokio::test]
    async fn me_missing_cookie_is_401_auth_required() {
        let router = router();
        let (status, _headers, _value, text) = call(&router, me_request(None)).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a missing rsc_session cookie must be 401, never the 503 stub: {text}"
        );
        assert_error(&text, "AUTH_REQUIRED", "auth.required");
    }

    #[tokio::test]
    async fn me_rejects_wrong_host_with_fixed_403() {
        let router = router();
        let mut req = me_request(Some(&valid_cookie()));
        req.headers_mut()
            .insert(header::HOST, "127.0.0.1:9999".parse().unwrap());
        let (status, _headers, _value, text) = call(&router, req).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a Host outside the allowed list must be a fixed 403: {text}"
        );
        assert_error(&text, "CSRF_INVALID", "security.origin_host");
    }

    #[tokio::test]
    async fn me_duplicate_or_malformed_session_cookie_is_401() {
        let router = router();
        let token = fake_token();
        let cookie_cases = [
            // Two rsc_session values in one Cookie header.
            &format!("rsc_session={token}; rsc_session={token}"),
            // 63 chars.
            &format!("rsc_session={}", &token[..63]),
            // 65 chars.
            &format!("rsc_session={token}0"),
            // Uppercase hex.
            &format!("rsc_session={}", token.to_uppercase()),
            // Non-hex first byte.
            &format!("rsc_session=z{}", &token[..63]),
            // Empty value.
            "rsc_session=",
            // Only other cookie names present.
            "other=abc",
            // Bare name without '=' must stay 401 once empty pairs are
            // skipped (guard against over-permissive parsing).
            "rsc_session",
            // An empty pair between two duplicates must not mask the
            // duplicate — two values is still two.
            &format!("rsc_session={token}; ; rsc_session={token}"),
        ];
        for (i, cookie) in cookie_cases.iter().enumerate() {
            let (status, _headers, _value, text) = call(&router, me_request(Some(cookie))).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "malformed/duplicated session cookie case #{i} must be 401: {text}"
            );
            assert_error(&text, "AUTH_REQUIRED", "auth.required");
        }
        // Two separate Cookie headers each carrying a session value must not
        // be reduced to one.
        let mut multi_header = me_request(Some(&valid_cookie()));
        multi_header.headers_mut().append(
            header::COOKIE,
            format!("rsc_session={token}").parse().unwrap(),
        );
        let (status, _headers, _value, text) = call(&router, multi_header).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "two Cookie headers with session values must be 401: {text}"
        );
        assert_error(&text, "AUTH_REQUIRED", "auth.required");
    }

    #[tokio::test]
    async fn me_valid_session_returns_exact_data_with_strict_bool_user() {
        let state = app_state(FakeAuth::new(FakeLogin::Ok));
        let router = build_http_router(state);
        // Other cookie names are ignored; the request carries NO Origin
        // header — a read-only GET must not require one.
        let cookie = format!("other=abc; rsc_session={}", fake_token());
        let (status, _headers, value, text) = call(&router, me_request(Some(&cookie))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a valid synthetic session must be 200: {text}"
        );
        assert_eq!(
            value.as_object().map(|o| o.len()),
            Some(2),
            "envelope must be exactly data + request_id: {text}"
        );
        assert!(value["request_id"].is_string(), "text: {text}");
        let data = value["data"].as_object().unwrap();
        assert_eq!(
            data.len(),
            3,
            "data must be exactly user + csrf_token + authz_epoch: {text}"
        );
        let user = &data["user"];
        assert_eq!(user["username"], json!(USERNAME));
        assert!(
            user["active"].is_boolean()
                && user["is_admin"].is_boolean()
                && user["must_change_password"].is_boolean(),
            "user flags must be strict booleans: {text}"
        );
        assert_eq!(
            user["must_change_password"],
            json!(false),
            "must_change_password must be the strict JSON false: {text}"
        );
        assert!(
            !user.as_object().unwrap().contains_key("password_hash"),
            "projection must not carry password_hash: {text}"
        );
        assert_eq!(
            data["authz_epoch"],
            json!("7"),
            "the fake per-request epoch must be the decimal string 7: {text}"
        );
        let csrf = data["csrf_token"].as_str().unwrap();
        assert_eq!(csrf.len(), 64, "csrf_token must be 64 hex chars: {text}");
        assert!(
            csrf.bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "csrf_token must be lowercase hex: {text}"
        );
        assert!(!text.contains("argon2"), "no hash material: {text}");
    }

    /// A trailing semicolon after the session cookie is a common real-world
    /// `Cookie` shape and must not abort parsing of the whole list.
    #[tokio::test]
    async fn me_accepts_valid_session_cookie_with_trailing_semicolon() {
        let router = router();
        let cookie = format!("rsc_session={};", fake_token());
        let (status, _headers, value, text) = call(&router, me_request(Some(&cookie))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a valid session cookie followed by a trailing semicolon must be 200: {text}"
        );
        assert_eq!(
            value["data"]["csrf_token"].as_str().map(str::len),
            Some(64),
            "the trailing-semicolon request must carry the 64-hex csrf_token: {text}"
        );
    }

    #[tokio::test]
    async fn me_reuses_csrf_and_reads_epoch_on_each_request() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let router = build_http_router(app_state(auth.clone()));
        let request = || me_request(Some(&valid_cookie()));
        let (status, _headers, first, text) = call(&router, request()).await;
        assert_eq!(status, StatusCode::OK, "first me must be 200: {text}");
        let csrf = first["data"]["csrf_token"].as_str().unwrap().to_string();
        assert_eq!(
            first["data"]["authz_epoch"],
            json!("7"),
            "first read must be the fake epoch 7: {text}"
        );
        // The epoch is read on EVERY request: bump the fake to 8.
        auth.set_epoch(8);
        let (status, _headers, second, text) = call(&router, request()).await;
        assert_eq!(status, StatusCode::OK, "second me must be 200: {text}");
        assert_eq!(
            second["data"]["authz_epoch"],
            json!("8"),
            "authz_epoch must be read per request, not cached: {text}"
        );
        assert_eq!(
            second["data"]["csrf_token"],
            json!(&csrf),
            "repeated me must reuse the bound CSRF token: {text}"
        );
    }

    #[tokio::test]
    async fn me_invalid_session_is_401_and_removes_stale_binding() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            csrf.insert(
                digest,
                CsrfBinding {
                    token: "0".repeat(64),
                    issued_at: Instant::now(),
                },
            );
        }
        // The session is definitively invalid (revoked/unknown/expired).
        auth.set_session_behavior(FakeSession::InvalidArgument);
        let router = build_http_router(state);
        let (status, _headers, _value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "an invalidated session must be 401: {text}"
        );
        assert_error(&text, "AUTH_REQUIRED", "auth.required");
        assert_eq!(
            auth_state.csrf.lock().unwrap().len(),
            0,
            "the InvalidArgument path must remove the stale digest binding"
        );
    }

    #[tokio::test]
    async fn me_unknown_well_formed_token_is_401_without_binding() {
        let state = app_state(FakeAuth::new(FakeLogin::Ok));
        let auth_state = state.auth.clone();
        let router = build_http_router(state);
        // Well-formed 64-hex value for a token the backend never minted.
        let unknown = format!("rsc_session={}", hex::encode([0x7c_u8; 32]));
        let (status, _headers, _value, text) = call(&router, me_request(Some(&unknown))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "an unknown token must be 401, not 200 or 503: {text}"
        );
        assert_error(&text, "AUTH_REQUIRED", "auth.required");
        assert!(
            auth_state.csrf.lock().unwrap().is_empty(),
            "an unknown token must not create a CSRF binding"
        );
    }

    // ---------------- Task 3f: pre-auth clock gate on GET /me ----------------

    /// The gate's core property: a well-formed cookie whose digest is NOT
    /// live in the process clock is a fixed 401 WITHOUT any backend call —
    /// no `authenticate`, no `authz_epoch` read, no read-quota entry.
    #[tokio::test]
    async fn me_forged_well_formed_cookie_is_401_without_backend_calls() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let router = build_http_router(state);
        // Random well-formed 64-lowercase-hex cookie for a digest the fake
        // process clock has never minted.
        let mut raw = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
        let forged_cookie_value = hex::encode(raw);
        let forged = format!("rsc_session={}", forged_cookie_value);
        let (status, _headers, _value, text) = call(&router, me_request(Some(&forged))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a forged well-formed cookie must be the fixed 401: {text}"
        );
        assert_error(&text, "AUTH_REQUIRED", "auth.required");
        assert_eq!(
            auth.authenticate_calls(),
            0,
            "the pre-auth clock gate must short-circuit before any backend authenticate"
        );
        assert_eq!(
            auth.authz_epoch_calls(),
            0,
            "a short-circuited forged cookie must not read the authz epoch"
        );
        let digest = token_digest(forged_cookie_value.as_bytes());
        assert!(
            !auth_state
                .read_limiter
                .lock()
                .unwrap()
                .entries
                .contains_key(&(EndpointId::Me, digest)),
            "a short-circuited forged cookie must not consume read quota"
        );
        assert!(
            auth_state.csrf.lock().unwrap().is_empty(),
            "a short-circuited forged cookie must not create a CSRF binding"
        );
    }

    /// A clock hit is only a necessary condition: it must still reach the
    /// DB-authoritative `authenticate` and return 200 only after success.
    #[tokio::test]
    async fn me_probe_hit_reaches_db_authenticate_and_returns_200() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let router = build_http_router(app_state(auth.clone()));
        let (status, _headers, value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a clock hit must still be DB-authoritative: {text}"
        );
        assert_eq!(
            auth.authenticate_calls(),
            1,
            "a clock hit is only a necessary condition and must reach authenticate"
        );
        assert_eq!(
            auth.authz_epoch_calls(),
            1,
            "the epoch is read exactly once per successful me"
        );
        assert!(
            value["data"]["user"].is_object(),
            "200 must carry the user: {text}"
        );
    }

    /// A clock hit must NOT mask a DB revocation: the authoritative
    /// `authenticate` decides, the fixed 401 applies, and the stale CSRF
    /// binding is removed exactly like the existing InvalidArgument branch.
    #[tokio::test]
    async fn me_probe_hit_but_db_revoked_is_401_and_removes_stale_binding() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let digest = token_digest(fake_token().as_bytes());
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            csrf.insert(
                digest,
                CsrfBinding {
                    token: "0".repeat(64),
                    issued_at: Instant::now(),
                },
            );
        }
        // The fake clock says live (default model) but the authoritative DB
        // says revoked.
        auth.set_session_behavior(FakeSession::InvalidArgument);
        let router = build_http_router(state);
        let (status, _headers, _value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a clock hit with a DB-revoked session must be 401: {text}"
        );
        assert_error(&text, "AUTH_REQUIRED", "auth.required");
        assert_eq!(
            auth.authenticate_calls(),
            1,
            "the probe must let the request reach the backend authenticate"
        );
        assert_eq!(
            auth_state.csrf.lock().unwrap().len(),
            0,
            "a DB-revoked session must drop its stale CSRF binding"
        );
    }

    /// A clock hit with a transient backend fault keeps the existing 503
    /// semantics: fixed error, binding preserved, no 401 remap.
    #[tokio::test]
    async fn me_probe_hit_but_backend_fault_is_503_and_keeps_binding() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let digest = token_digest(fake_token().as_bytes());
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            csrf.insert(
                digest,
                CsrfBinding {
                    token: "0".repeat(64),
                    issued_at: Instant::now(),
                },
            );
        }
        let router = build_http_router(state);
        for behavior in [FakeSession::Database, FakeSession::Crypto] {
            auth.set_session_behavior(behavior);
            let (status, _headers, _value, text) =
                call(&router, me_request(Some(&valid_cookie()))).await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "a clock hit with {behavior:?} must stay a fixed 503: {text}"
            );
            assert!(
                auth_state.csrf.lock().unwrap().len() == 1,
                "a transient 503 must preserve the digest binding: {behavior:?}"
            );
        }
        assert_eq!(
            auth.authenticate_calls(),
            2,
            "both fault paths must have reached the backend authenticate"
        );
    }

    /// A flood of forged cookies must not consume another valid session's
    /// 50/s read budget: every forged cookie is a fixed 401 short of the
    /// backend and of the limiter, and the other session still gets exactly
    /// 50 reads before its own 429.
    #[tokio::test]
    async fn me_forged_flood_does_not_consume_other_session_read_budget() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        // A second, independently valid session.
        let mut other_raw = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut other_raw);
        let other_token = hex::encode(other_raw);
        auth.extra_valid_tokens
            .lock()
            .unwrap()
            .insert(other_token.clone());
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let router = build_http_router(state);
        let other_cookie = format!("rsc_session={other_token}");

        // 1. Sixty distinct forged well-formed cookies, all fixed 401.
        for _ in 0..60 {
            let mut raw = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
            let forged = format!("rsc_session={}", hex::encode(raw));
            let (status, _h, _v, text) = call(&router, me_request(Some(&forged))).await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "a forged cookie must be the fixed 401: {text}"
            );
        }
        assert_eq!(
            auth.authenticate_calls(),
            0,
            "forged cookies must never reach the backend"
        );

        // 2. The other valid session still owns its full 50/s read budget.
        for i in 0..50 {
            let (status, _h, _v, _text) = call(&router, me_request(Some(&other_cookie))).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "read {i} of the other session must be 200"
            );
        }
        let (status, headers, _v, text) = call(&router, me_request(Some(&other_cookie))).await;
        assert_eq!(
            status,
            StatusCode::TOO_MANY_REQUESTS,
            "the other session's 51st read must be 429: {text}"
        );
        assert_error(&text, "RATE_LIMITED", "auth.rate_limited");
        assert_eq!(
            headers.get(header::RETRY_AFTER).unwrap().to_str().unwrap(),
            "1"
        );
        assert_eq!(auth.authenticate_calls(), 51);
        // Only the other session's key may exist in the read limiter.
        let entries = auth_state.read_limiter.lock().unwrap().entries.clone();
        assert_eq!(
            entries.len(),
            1,
            "no forged digest may hold a read-limiter key"
        );
        assert!(
            entries.contains_key(&(EndpointId::Me, token_digest(other_token.as_bytes()))),
            "the other session's own key must be the only entry"
        );
    }

    /// The fake's clock model must not mask a missing gate: a digest
    /// explicitly marked dead (simulated logout) is short-circuited even
    /// though the DB fake would still serve it.
    #[tokio::test]
    async fn me_explicitly_dead_digest_is_short_circuited_before_backend() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let router = build_http_router(state);
        // Simulate a logout: the clock entry is gone.
        auth.set_digest_live(token_digest(fake_token().as_bytes()), false);
        let (status, _headers, _value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "a digest no longer live in the process clock must be 401: {text}"
        );
        assert_error(&text, "AUTH_REQUIRED", "auth.required");
        assert_eq!(
            auth.authenticate_calls(),
            0,
            "a clock-dead digest must not reach the backend authenticate"
        );
    }

    #[tokio::test]
    async fn me_backend_fault_is_fixed_503_and_keeps_binding() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            csrf.insert(
                digest,
                CsrfBinding {
                    token: "0".repeat(64),
                    issued_at: Instant::now(),
                },
            );
        }
        let router = build_http_router(state);
        let request = || me_request(Some(&valid_cookie()));
        for (behavior, code, key) in [
            (
                FakeSession::Database,
                "STORAGE_UNAVAILABLE",
                "system.storage_unavailable",
            ),
            (FakeSession::Crypto, "NOT_READY", "system.not_ready"),
            (FakeSession::Config, "NOT_READY", "system.not_ready"),
            (FakeSession::SchemaNotReady, "NOT_READY", "system.not_ready"),
        ] {
            auth.set_session_behavior(behavior);
            let (status, _headers, _value, text) = call(&router, request()).await;
            assert_eq!(
                status,
                StatusCode::SERVICE_UNAVAILABLE,
                "{behavior:?} must be a fixed 503: {text}"
            );
            assert_error(&text, code, key);
            assert!(
                !text.contains("synthetic"),
                "no raw fault text may reach the body: {text}"
            );
            assert_eq!(
                auth_state.csrf.lock().unwrap().len(),
                1,
                "a transient 503 must not remove the digest binding: {behavior:?}"
            );
        }
    }

    #[tokio::test]
    async fn me_at_live_capacity_for_new_digest_is_503_retry_after_60() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        {
            // Every slot is LIVE, so neither the absolute prune nor the
            // threshold dead prune can free anything for a new digest.
            let mut csrf = auth_state.csrf.lock().unwrap();
            seed_live_csrf_bindings(&auth, &mut csrf, MAX_CSRF_ENTRIES);
        }
        let router = build_http_router(state);
        let (status, headers, _value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "a live full map must fail closed for a new digest: {text}"
        );
        assert_error(&text, "RESOURCE_EXHAUSTED", "system.resource_exhausted");
        assert_eq!(
            headers
                .get(header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some("60"),
            "live-cap exhaustion must carry Retry-After: 60"
        );
        assert_eq!(
            auth_state.csrf.lock().unwrap().len(),
            MAX_CSRF_ENTRIES,
            "no new binding may be created at the live cap"
        );
    }

    #[tokio::test]
    async fn me_at_live_capacity_reuses_existing_digest_binding() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        let seeded = "f".repeat(64);
        {
            // MAX-1 LIVE filler slots + the caller's own binding: reuse must
            // still work at the cap and must not touch any other slot.
            let mut csrf = auth_state.csrf.lock().unwrap();
            seed_live_csrf_bindings(&auth, &mut csrf, MAX_CSRF_ENTRIES - 1);
            csrf.insert(
                digest,
                CsrfBinding {
                    token: seeded.clone(),
                    issued_at: Instant::now(),
                },
            );
        }
        let router = build_http_router(state);
        let (status, _headers, value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "an already-bound digest must reuse its CSRF even at the cap: {text}"
        );
        assert_eq!(
            value["data"]["csrf_token"],
            json!(&seeded),
            "the seeded token must be returned, not reissued: {text}"
        );
        assert_eq!(
            auth_state.csrf.lock().unwrap().len(),
            MAX_CSRF_ENTRIES,
            "reuse must not grow the map"
        );
    }
}

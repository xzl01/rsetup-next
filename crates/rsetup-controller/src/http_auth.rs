//! HTTP auth handlers (Task 3a: `login`; Task 3b: `me`; Task 3c: `password`
//! and `logout`; Task 3e: session-write and endpoint-read rate limiting;
//! Task 3f: pre-auth `me` clock gate).
//!
//! The partial router is NEVER wired to `main`: all auth endpoints stay
//! unregistered in `main` until the production `LiveAuth` adapter and the
//! six-method SQLx repository review land. See
//! `.superpowers/sdd/2026-10-03-controller-auth-http/`.
//!
//! Contract: envelopes come from `crate::http_api`; fixed allowed
//! code/message_key with empty redacted `params`; credentials, tokens and
//! raw backend errors never reach responses or logs.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::hash::Hash;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{ConnectInfo, Extension, FromRef, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, post};
use serde_json::json;

use crate::auth::service::{Login, Session, SessionPage};
use crate::auth::session::{ABSOLUTE, token_digest};
use crate::db::DbPool;
use crate::error::ControllerError;
use crate::http_api::{err_response, ok_response, user_public};
use crate::http_security::{
    HttpSecurityConfig, LoginRateLimiter, RateLimitError, check_pre_session, check_session_write,
    clear_session_cookie_header, session_cookie_header,
};

/// Bounded boxed future so `AuthServiceApi` stays object-safe.
pub type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// Object-safe auth boundary consumed by the HTTP layer. The production
/// adapter (Task 3b) wraps `AuthService<R: IdentityRepository>`; `authz_epoch`
/// is the per-request `schema_meta` count, never the process epoch.
pub trait AuthServiceApi: Send + Sync {
    fn login<'a>(&'a self, username: &'a str, password: &'a str) -> AuthFuture<'a, Login>;
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session>;
    fn change_password<'a>(
        &'a self,
        session: &'a Session,
        current: &'a str,
        new_password: &'a str,
    ) -> AuthFuture<'a, ()>;
    fn logout<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, ()>;
    fn authz_epoch(&self) -> AuthFuture<'_, u64>;
    /// Synchronous, pure in-memory precheck (Task 3f): does `digest` still
    /// exist and is live in the current process session clock?
    ///
    /// No async I/O, no database access, and no idle renewal. A `true`
    /// result is only a necessary condition — the caller must still run the
    /// DB-authoritative `authenticate` and every downstream gate. This
    /// method is REQUIRED (no default): an always-true default would
    /// silently mask a missing gate behind the object-safe boundary.
    fn is_live_session(&self, digest: &[u8; 32]) -> bool;
    fn list_sessions<'a>(
        &'a self,
        session: &'a Session,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> AuthFuture<'a, SessionPage>;
    fn revoke_by_alias<'a>(&'a self, session: &'a Session, id: &'a str) -> AuthFuture<'a, bool>;
    fn revoke_others<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, u64>;
}

/// Hard cap on in-memory CSRF bindings; at the cap, new bindings fail closed.
pub const MAX_CSRF_ENTRIES: usize = 4096;

/// One bounded CSRF binding: the issued 64-hex token plus the monotonic
/// issue instant. Bindings are pruned once `auth::session::ABSOLUTE` (12h)
/// has elapsed; a repeated `me` reuses the token and must NOT refresh
/// `issued_at`.
#[derive(Debug, Clone)]
pub struct CsrfBinding {
    pub token: String,
    pub issued_at: Instant,
}

/// JSON write-body cap (spec 02 §3): 1 MiB, any overrun is a fixed 400.
const MAX_BODY_BYTES: usize = 1_048_576;

/// Password cap (spec 01 A-02): at most 512 UTF-8 bytes, never truncated.
const MAX_PASSWORD_BYTES: usize = 512;

/// Rate gate result for sliding window checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowRateLimitError {
    /// Request rate exceeded; client should retry after the duration.
    Limited(Duration),
    /// Limiter map is at capacity and cannot admit a new key; fail closed.
    Exhausted,
}

/// Identifiers for read-limited endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointId {
    Me,
    Sessions,
}

/// Fixed sliding rate-window duration: 1 second.
pub const RATE_WINDOW_1S: Duration = Duration::from_secs(1);

/// Maximum active keys per rate window map (hard cap).
pub const MAX_RATE_LIMITER_KEYS: usize = 4096;

/// Maximum requests per second for authenticated session writes.
pub const WRITE_RATE_LIMIT_PER_SEC: usize = 20;

/// Maximum requests per second for endpoint reads per session.
pub const READ_RATE_LIMIT_PER_SEC: usize = 50;

/// Sliding 1-second monotonic window limiter bounded to `MAX_RATE_LIMITER_KEYS`.
pub struct RateWindowLimiter<K: Hash + Eq> {
    max_requests: usize,
    entries: HashMap<K, VecDeque<Instant>>,
    #[cfg(test)]
    pub global_prune_count: usize,
}

impl<K: Hash + Eq + Clone> RateWindowLimiter<K> {
    pub fn new(max_requests: usize) -> Self {
        Self {
            max_requests,
            entries: HashMap::new(),
            #[cfg(test)]
            global_prune_count: 0,
        }
    }

    /// Prune expired timestamps across all keys, dropping keys left empty.
    /// A timestamp t is valid if t <= now && now - t < RATE_WINDOW_1S.
    /// Anomalous future-dated timestamps (t > now) fail closed by being dropped.
    fn prune_all(&mut self, now: Instant) {
        #[cfg(test)]
        {
            self.global_prune_count += 1;
        }
        for dq in self.entries.values_mut() {
            while let Some(&front) = dq.front() {
                if front > now || now.saturating_duration_since(front) >= RATE_WINDOW_1S {
                    dq.pop_front();
                } else {
                    break;
                }
            }
        }
        self.entries.retain(|_, dq| !dq.is_empty());
    }

    /// Prune expired timestamps for a specific queue.
    fn prune_deque(dq: &mut VecDeque<Instant>, now: Instant) {
        while let Some(&front) = dq.front() {
            if front > now || now.saturating_duration_since(front) >= RATE_WINDOW_1S {
                dq.pop_front();
            } else {
                break;
            }
        }
    }

    /// Check and record an attempt at `now` for `key`.
    pub fn check_and_record(&mut self, now: Instant, key: &K) -> Result<(), WindowRateLimitError> {
        if let Some(dq) = self.entries.get_mut(key) {
            Self::prune_deque(dq, now);
            if dq.len() >= self.max_requests {
                let oldest = *dq.front().unwrap();
                let remaining = if oldest > now {
                    RATE_WINDOW_1S
                } else {
                    let elapsed = now.saturating_duration_since(oldest);
                    RATE_WINDOW_1S.saturating_sub(elapsed)
                };
                return Err(WindowRateLimitError::Limited(remaining));
            }
            dq.push_back(now);
            return Ok(());
        }

        // Key is not present. If table is at capacity, run global prune across all keys
        // to self-heal and reclaim slots from expired keys.
        if self.entries.len() >= MAX_RATE_LIMITER_KEYS {
            self.prune_all(now);
        }

        if self.entries.len() >= MAX_RATE_LIMITER_KEYS {
            return Err(WindowRateLimitError::Exhausted);
        }

        let mut dq = VecDeque::with_capacity(self.max_requests);
        dq.push_back(now);
        self.entries.insert(key.clone(), dq);
        Ok(())
    }
}

/// Handler state for the auth routes.
pub struct HttpAuthState {
    pub auth: Arc<dyn AuthServiceApi>,
    pub config: HttpSecurityConfig,
    /// Session digest -> CSRF binding, bounded to `MAX_CSRF_ENTRIES` (fail
    /// closed instead of evicting or overwriting live bindings).
    pub csrf: Mutex<HashMap<[u8; 32], CsrfBinding>>,
    /// Login failure limiter (account + source dimensions), serialised by
    /// this `Mutex`; the limiter itself is lock-free.
    pub rate: Mutex<LoginRateLimiter>,
    /// Authenticated session writes limiter (20/s across password + logout).
    pub write_limiter: Mutex<RateWindowLimiter<[u8; 32]>>,
    /// Authenticated endpoint reads limiter (50/s per (EndpointId, session.digest)).
    pub read_limiter: Mutex<RateWindowLimiter<(EndpointId, [u8; 32])>>,
}

impl HttpAuthState {
    pub fn new(auth: Arc<dyn AuthServiceApi>, config: HttpSecurityConfig) -> Self {
        Self {
            auth,
            config,
            csrf: Mutex::new(HashMap::new()),
            rate: Mutex::new(LoginRateLimiter::new()),
            write_limiter: Mutex::new(RateWindowLimiter::new(WRITE_RATE_LIMIT_PER_SEC)),
            read_limiter: Mutex::new(RateWindowLimiter::new(READ_RATE_LIMIT_PER_SEC)),
        }
    }
}

/// Router state: the shared DB pool (for the merged probe routes) plus the
/// auth handler state.
#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub auth: Arc<HttpAuthState>,
}

/// Derive the shared auth handler state from the router state.
impl FromRef<AppState> for Arc<HttpAuthState> {
    fn from_ref(state: &AppState) -> Self {
        state.auth.clone()
    }
}

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
async fn sessions_list(
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
async fn revoke_others_handler(
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
async fn revoke_by_id(
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

/// POST /api/v1/auth/password (Task 3c, fake-backed).
async fn password(
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
async fn logout(
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
/// past-`ABSOLUTE` bindings. No raw cookie, hash or DB error ever reaches
/// the body or a log.
async fn me(State(state): State<Arc<HttpAuthState>>, headers: HeaderMap) -> Response {
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
    // 6. Reuse or issue the CSRF bound to this digest. The prune runs
    //    first under the same lock; an existing binding is reused WITHOUT
    //    refreshing its issue instant.
    let mut csrf = state.csrf.lock().unwrap();
    prune_expired_csrf(&mut csrf, now);
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

/// Exactly one `rsc_session` cookie value (exactly 64 lowercase hex chars)
/// across all `Cookie` headers. Other cookie names are ignored; empty
/// pairs (e.g. a trailing `;`) carry no cookie and are skipped; a missing
/// header, a non-ASCII header, a pair without `=`, an empty/short/long/
/// non-hex/uppercase value, or two-or-more `rsc_session` occurrences all
/// fail closed as `None`.
fn session_cookie_value(headers: &HeaderMap) -> Option<String> {
    let mut found: Option<String> = None;
    for value in headers.get_all(header::COOKIE).iter() {
        let text = value.to_str().ok()?;
        for pair in text.split(';') {
            // An empty pair has no name and no value; skipping it must not
            // mask a duplicate, because every non-empty `rsc_session` pair
            // is still counted by the fail-closed check below.
            if pair.trim().is_empty() {
                continue;
            }
            let (name, cookie) = pair.split_once('=')?;
            if name.trim() != "rsc_session" {
                continue;
            }
            if found.is_some() {
                return None;
            }
            if cookie.len() != 64
                || !cookie
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return None;
            }
            found = Some(cookie.to_string());
        }
    }
    found
}

/// Fixed 401 for every missing/duplicated/malformed/invalid session.
fn auth_required() -> Response {
    err_response(
        StatusCode::UNAUTHORIZED,
        "AUTH_REQUIRED",
        "auth.required",
        json!({}),
    )
}

/// Fixed 503 for backend faults that are NOT a definitive
/// `InvalidArgument` (which the caller maps to 401): Database is
/// `STORAGE_UNAVAILABLE`, every other variant is `NOT_READY`. No raw error
/// text ever reaches the body.
fn backend_error(error: &ControllerError) -> Response {
    match error {
        ControllerError::Database(_) => err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "STORAGE_UNAVAILABLE",
            "system.storage_unavailable",
            json!({}),
        ),
        _ => not_ready(),
    }
}

/// Remove CSRF bindings whose 12h absolute lifetime
/// (`auth::session::ABSOLUTE`) has elapsed under the monotonic clock.
/// A binding whose issue instant lies AFTER `now` is anomalous (a
/// monotonic clock never moves backwards; only an adversarial construct
/// through the public `CsrfBinding` fields could do this) and is treated
/// as expired — fail closed, never a permanent resident of the map.
/// Called at the login peek, the authoritative `login_success` insert and
/// the `me` bind; live bindings (and their issue instants) are never
/// touched.
fn prune_expired_csrf(csrf: &mut HashMap<[u8; 32], CsrfBinding>, now: Instant) -> usize {
    let before = csrf.len();
    csrf.retain(|_, binding| {
        binding.issued_at <= now && now.saturating_duration_since(binding.issued_at) < ABSOLUTE
    });
    before - csrf.len()
}

async fn login(
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
    //    lifetime, then fail closed if the map is still at the cap,
    //    BEFORE any credential work (the authoritative check for the
    //    concurrent case stays inside the insert lock in `login_success`).
    {
        let mut csrf = state.csrf.lock().unwrap();
        prune_expired_csrf(&mut csrf, now);
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
fn parse_logout_body(bytes: &[u8]) -> bool {
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

fn csrf_invalid() -> Response {
    err_response(
        StatusCode::FORBIDDEN,
        "CSRF_INVALID",
        "security.csrf",
        json!({}),
    )
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

fn password_change_required() -> Response {
    err_response(
        StatusCode::FORBIDDEN,
        "PASSWORD_CHANGE_REQUIRED",
        "auth.password_change_required",
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

fn origin_host_invalid() -> Response {
    err_response(
        StatusCode::FORBIDDEN,
        "CSRF_INVALID",
        "security.origin_host",
        json!({}),
    )
}

/// Checked origin header: 0 occurrences is Ok(None); exactly 1 ASCII value
/// is Ok(Some(&str)); 2+ occurrences or non-ASCII fails as Err(()).
fn get_single_origin(headers: &HeaderMap) -> Result<Option<&str>, ()> {
    let mut values = headers.get_all(header::ORIGIN).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(());
    }
    let text = first.to_str().map_err(|_| ())?;
    Ok(Some(text))
}

fn not_ready() -> Response {
    err_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "NOT_READY",
        "system.not_ready",
        json!({}),
    )
}

/// Fixed 503 + `Retry-After: 60` for a fail-closed resource cap.
fn exhausted() -> Response {
    let mut response = err_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "RESOURCE_EXHAUSTED",
        "system.resource_exhausted",
        json!({}),
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    response
}

/// Fixed 429 + `Retry-After` in whole seconds (ceil, clamped to 1..=900).
fn rate_limited(remaining: Duration) -> Response {
    let seconds = (remaining.as_secs_f64().ceil() as u64).clamp(1, 900);
    let mut response = err_response(
        StatusCode::TOO_MANY_REQUESTS,
        "RATE_LIMITED",
        "auth.rate_limited",
        json!({}),
    );
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
    response
}

/// Exactly one header value, as a str. A missing header, a duplicated
/// header (any number of values above one), or a non-ASCII value all
/// fail closed as `None`; the caller maps that to its fixed rejection
/// (403 for Host/Origin, 400 for Content-Type). Repeated values must
/// never be reduced to "take the first".
fn single_header(headers: &HeaderMap, key: HeaderName) -> Option<&str> {
    let mut values = headers.get_all(key).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

/// JSON Content-Type: the media type must be exactly `application/json`
/// (parameters such as charset are tolerated).
fn json_content_type(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .map(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false)
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
/// token, prune past-absolute bindings, bind the token to the session
/// digest under the 4096 cap, and set the validated `rsc_session` cookie.
/// The in-lock capacity check below stays authoritative for the concurrent
/// case.
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
        prune_expired_csrf(&mut csrf, issued_at);
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
    use super::*;
    use crate::auth::service::IdentityUser;
    use axum::body::to_bytes;
    use axum::http::{HeaderMap, Request, header};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use tower::ServiceExt;

    const HOST: &str = "127.0.0.1:8080";
    const ORIGIN: &str = "http://127.0.0.1:5173";
    const PEER: &str = "10.1.1.1:5000";
    const PEER2: &str = "10.1.1.2:5000";
    const USERNAME: &str = "admin";
    const PASSWORD: &str = "correct-horse-battery";

    fn fake_token() -> String {
        hex::encode([0x5a_u8; 32])
    }

    fn clone_controller_error(e: &ControllerError) -> ControllerError {
        match e {
            ControllerError::InvalidArgument => ControllerError::InvalidArgument,
            ControllerError::RevisionConflict => ControllerError::RevisionConflict,
            ControllerError::PermissionDenied => ControllerError::PermissionDenied,
            ControllerError::NotFound => ControllerError::NotFound,
            ControllerError::ResourceExhausted => ControllerError::ResourceExhausted,
            ControllerError::Config(s) => ControllerError::Config(s.clone()),
            ControllerError::SchemaNotReady { found, required } => {
                ControllerError::SchemaNotReady {
                    found: *found,
                    required: *required,
                }
            }
            ControllerError::Database(_) => ControllerError::Database(sqlx::Error::Configuration(
                std::io::Error::other("synthetic db fault").into(),
            )),
            ControllerError::Crypto => ControllerError::Crypto,
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FakeLogin {
        Ok,
        InvalidArgument,
        Database,
        Crypto,
        Config,
        SchemaNotReady,
        /// A well-formed `Login` whose raw token `session_cookie_header`
        /// must reject (invalid for the cookie contract).
        BadToken,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FakeSession {
        Ok,
        InvalidArgument,
        Database,
        Crypto,
        Config,
        SchemaNotReady,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FakePassword {
        Ok,
        WrongOld,
        NewInvalid,
        ConcurrentlyRevoked,
        RevisionConflict,
        Database,
        Crypto,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum FakeLogout {
        Ok,
        Database,
        Crypto,
    }

    struct FakeAuth {
        behavior: Mutex<FakeLogin>,
        session_behavior: Mutex<FakeSession>,
        password_behavior: Mutex<FakePassword>,
        logout_behavior: Mutex<FakeLogout>,
        session_dead: std::sync::atomic::AtomicBool,
        extra_valid_tokens: Mutex<std::collections::HashSet<String>>,
        authenticate_sequence: Mutex<std::collections::VecDeque<FakeSession>>,
        epoch: AtomicU64,
        login_calls: AtomicUsize,
        authenticate_calls: AtomicUsize,
        change_password_calls: AtomicUsize,
        logout_calls: AtomicUsize,
        authz_epoch_calls: AtomicUsize,
        list_calls: AtomicUsize,
        revoke_alias_calls: AtomicUsize,
        revoke_others_calls: AtomicUsize,
        last_list_limit: AtomicUsize,
        list_behavior: Mutex<Option<Result<SessionPage, ControllerError>>>,
        revoke_alias_behavior: Mutex<Option<Result<bool, ControllerError>>>,
        revoke_others_behavior: Mutex<Option<Result<u64, ControllerError>>>,
        /// Task 3f: digests the fake models as live in its in-process
        /// session clock. A digest with an explicit entry uses that entry;
        /// otherwise the default model applies (signed-in known digests are
        /// live, unknown/forged digests are not).
        live_overrides: Mutex<std::collections::HashMap<[u8; 32], bool>>,
    }

    impl FakeAuth {
        fn new(behavior: FakeLogin) -> Arc<Self> {
            Arc::new(Self {
                behavior: Mutex::new(behavior),
                session_behavior: Mutex::new(FakeSession::Ok),
                password_behavior: Mutex::new(FakePassword::Ok),
                logout_behavior: Mutex::new(FakeLogout::Ok),
                session_dead: std::sync::atomic::AtomicBool::new(false),
                extra_valid_tokens: Mutex::new(std::collections::HashSet::new()),
                authenticate_sequence: Mutex::new(std::collections::VecDeque::new()),
                epoch: AtomicU64::new(7),
                login_calls: AtomicUsize::new(0),
                authenticate_calls: AtomicUsize::new(0),
                change_password_calls: AtomicUsize::new(0),
                logout_calls: AtomicUsize::new(0),
                authz_epoch_calls: AtomicUsize::new(0),
                list_calls: AtomicUsize::new(0),
                revoke_alias_calls: AtomicUsize::new(0),
                revoke_others_calls: AtomicUsize::new(0),
                last_list_limit: AtomicUsize::new(50),
                list_behavior: Mutex::new(None),
                revoke_alias_behavior: Mutex::new(None),
                revoke_others_behavior: Mutex::new(None),
                live_overrides: Mutex::new(std::collections::HashMap::new()),
            })
        }
        fn set_list_behavior(&self, result: Result<SessionPage, ControllerError>) {
            *self.list_behavior.lock().unwrap() = Some(result);
        }
        fn set_revoke_alias_result(&self, result: Result<bool, ControllerError>) {
            *self.revoke_alias_behavior.lock().unwrap() = Some(result);
        }
        fn set_revoke_others_result(&self, result: Result<u64, ControllerError>) {
            *self.revoke_others_behavior.lock().unwrap() = Some(result);
        }
        /// Task 3f test seam: explicitly mark a digest live/dead in the fake
        /// in-process clock (e.g. model a simulated logout).
        fn set_digest_live(&self, digest: [u8; 32], live: bool) {
            self.live_overrides.lock().unwrap().insert(digest, live);
        }
        fn set_behavior(&self, behavior: FakeLogin) {
            *self.behavior.lock().unwrap() = behavior;
        }
        fn set_session_behavior(&self, behavior: FakeSession) {
            *self.session_behavior.lock().unwrap() = behavior;
        }
        fn set_password_behavior(&self, behavior: FakePassword) {
            *self.password_behavior.lock().unwrap() = behavior;
        }
        fn set_logout_behavior(&self, behavior: FakeLogout) {
            *self.logout_behavior.lock().unwrap() = behavior;
        }
        fn push_authenticate_step(&self, behavior: FakeSession) {
            self.authenticate_sequence
                .lock()
                .unwrap()
                .push_back(behavior);
        }
        fn set_epoch(&self, epoch: u64) {
            self.epoch.store(epoch, Ordering::SeqCst);
        }
        fn login_calls(&self) -> usize {
            self.login_calls.load(Ordering::SeqCst)
        }
        fn authenticate_calls(&self) -> usize {
            self.authenticate_calls.load(Ordering::SeqCst)
        }
        fn authz_epoch_calls(&self) -> usize {
            self.authz_epoch_calls.load(Ordering::SeqCst)
        }
        fn change_password_calls(&self) -> usize {
            self.change_password_calls.load(Ordering::SeqCst)
        }
        fn logout_calls(&self) -> usize {
            self.logout_calls.load(Ordering::SeqCst)
        }
        fn list_calls(&self) -> usize {
            self.list_calls.load(Ordering::SeqCst)
        }
        fn revoke_alias_calls(&self) -> usize {
            self.revoke_alias_calls.load(Ordering::SeqCst)
        }
        fn revoke_others_calls(&self) -> usize {
            self.revoke_others_calls.load(Ordering::SeqCst)
        }
        fn last_list_limit(&self) -> usize {
            self.last_list_limit.load(Ordering::SeqCst)
        }
        fn fixed_error(behavior: FakeLogin) -> ControllerError {
            match behavior {
                FakeLogin::InvalidArgument => ControllerError::InvalidArgument,
                FakeLogin::Database => ControllerError::Database(sqlx::Error::Configuration(
                    std::io::Error::other("synthetic db fault").into(),
                )),
                FakeLogin::Crypto => ControllerError::Crypto,
                FakeLogin::Config => ControllerError::Config("synthetic config fault".into()),
                FakeLogin::SchemaNotReady => ControllerError::SchemaNotReady {
                    found: None,
                    required: 1,
                },
                FakeLogin::Ok => unreachable!("Ok has no fixed error"),
                FakeLogin::BadToken => unreachable!("BadToken yields a Login, not an error"),
            }
        }
        fn fixed_session_for_token(raw: &str) -> Session {
            Session {
                user: Self::fixed_user(),
                digest: crate::auth::session::token_digest(raw.as_bytes()),
            }
        }
        fn fixed_session() -> Session {
            Self::fixed_session_for_token(&fake_token())
        }
        fn fixed_session_error(behavior: FakeSession) -> ControllerError {
            match behavior {
                FakeSession::InvalidArgument => ControllerError::InvalidArgument,
                FakeSession::Database => ControllerError::Database(sqlx::Error::Configuration(
                    std::io::Error::other("synthetic db fault").into(),
                )),
                FakeSession::Crypto => ControllerError::Crypto,
                FakeSession::Config => ControllerError::Config("synthetic config fault".into()),
                FakeSession::SchemaNotReady => ControllerError::SchemaNotReady {
                    found: None,
                    required: 1,
                },
                FakeSession::Ok => unreachable!("Ok yields a Session, not an error"),
            }
        }
        fn fixed_user() -> IdentityUser {
            IdentityUser {
                id: [3; 16],
                username: USERNAME.into(),
                password_hash: "$argon2id$=synthetic-test-only=".into(),
                active: true,
                is_admin: false,
                must_change_password: false,
                revision: 1,
            }
        }
        fn fixed_login() -> Login {
            let raw_token = fake_token();
            let digest = crate::auth::session::token_digest(raw_token.as_bytes());
            Login {
                raw_token,
                session: Session {
                    user: Self::fixed_user(),
                    digest,
                },
            }
        }
        /// A `Login` whose raw token is NOT a 64-char lowercase hex string,
        /// so `session_cookie_header` must fail on it.
        fn bad_login() -> Login {
            let raw_token = "invalid-raw-token-for-3a-fix1".to_string();
            let digest = crate::auth::session::token_digest(raw_token.as_bytes());
            Login {
                raw_token,
                session: Session {
                    user: Self::fixed_user(),
                    digest,
                },
            }
        }
    }

    impl AuthServiceApi for FakeAuth {
        fn login<'a>(&'a self, username: &'a str, password: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async move {
                self.login_calls.fetch_add(1, Ordering::SeqCst);
                let behavior = *self.behavior.lock().unwrap();
                match behavior {
                    FakeLogin::Ok if username == USERNAME && password == PASSWORD => {
                        Ok(Self::fixed_login())
                    }
                    FakeLogin::Ok => Err(ControllerError::InvalidArgument),
                    FakeLogin::BadToken => Ok(Self::bad_login()),
                    other => Err(Self::fixed_error(other)),
                }
            })
        }
        fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
            Box::pin(async move {
                self.authenticate_calls.fetch_add(1, Ordering::SeqCst);
                if let Some(queued) = self.authenticate_sequence.lock().unwrap().pop_front() {
                    return match queued {
                        FakeSession::Ok => {
                            if raw == fake_token().as_str() {
                                Ok(Self::fixed_session())
                            } else {
                                Err(ControllerError::InvalidArgument)
                            }
                        }
                        other => Err(Self::fixed_session_error(other)),
                    };
                }
                if self.session_dead.load(Ordering::SeqCst) {
                    return Err(ControllerError::InvalidArgument);
                }
                let behavior = *self.session_behavior.lock().unwrap();
                if behavior == FakeSession::Ok {
                    // The synthetic session exists only for fake_token or extra registered tokens.
                    let is_valid = raw == fake_token().as_str()
                        || self.extra_valid_tokens.lock().unwrap().contains(raw);
                    return if is_valid {
                        Ok(Self::fixed_session_for_token(raw))
                    } else {
                        Err(ControllerError::InvalidArgument)
                    };
                }
                Err(Self::fixed_session_error(behavior))
            })
        }
        fn change_password<'a>(
            &'a self,
            _session: &'a Session,
            _current: &'a str,
            _new_password: &'a str,
        ) -> AuthFuture<'a, ()> {
            Box::pin(async move {
                self.change_password_calls.fetch_add(1, Ordering::SeqCst);
                let behavior = *self.password_behavior.lock().unwrap();
                match behavior {
                    FakePassword::Ok => {
                        self.session_dead.store(true, Ordering::SeqCst);
                        Ok(())
                    }
                    FakePassword::WrongOld | FakePassword::NewInvalid => {
                        Err(ControllerError::InvalidArgument)
                    }
                    FakePassword::ConcurrentlyRevoked => {
                        self.session_dead.store(true, Ordering::SeqCst);
                        Err(ControllerError::InvalidArgument)
                    }
                    FakePassword::RevisionConflict => Err(ControllerError::RevisionConflict),
                    FakePassword::Database => {
                        Err(ControllerError::Database(sqlx::Error::Configuration(
                            std::io::Error::other("synthetic db fault").into(),
                        )))
                    }
                    FakePassword::Crypto => Err(ControllerError::Crypto),
                }
            })
        }
        fn logout<'a>(&'a self, _session: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async move {
                self.logout_calls.fetch_add(1, Ordering::SeqCst);
                let behavior = *self.logout_behavior.lock().unwrap();
                match behavior {
                    FakeLogout::Ok => {
                        self.session_dead.store(true, Ordering::SeqCst);
                        Ok(())
                    }
                    FakeLogout::Database => {
                        Err(ControllerError::Database(sqlx::Error::Configuration(
                            std::io::Error::other("synthetic db fault").into(),
                        )))
                    }
                    FakeLogout::Crypto => Err(ControllerError::Crypto),
                }
            })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            // Synthetic per-request epoch (default 7), configurable so the
            // handler can be proven to read it on EVERY request.
            Box::pin(async move {
                self.authz_epoch_calls.fetch_add(1, Ordering::SeqCst);
                Ok(self.epoch.load(Ordering::SeqCst))
            })
        }
        fn is_live_session(&self, digest: &[u8; 32]) -> bool {
            // Task 3f fake in-process clock model. An explicit test mark
            // (e.g. a simulated logout) wins over the default; otherwise a
            // signed-in known digest (the fake login token or an
            // extra-registered valid token) is live. Never a hardcoded
            // always-true: that could mask a missing gate.
            if let Some(&live) = self.live_overrides.lock().unwrap().get(digest) {
                return live;
            }
            let default = token_digest(fake_token().as_bytes());
            if *digest == default {
                return true;
            }
            self.extra_valid_tokens
                .lock()
                .unwrap()
                .iter()
                .any(|token| token_digest(token.as_bytes()) == *digest)
        }
        fn list_sessions<'a>(
            &'a self,
            session: &'a Session,
            _cursor: Option<&'a str>,
            limit: usize,
        ) -> AuthFuture<'a, SessionPage> {
            Box::pin(async move {
                self.list_calls.fetch_add(1, Ordering::SeqCst);
                self.last_list_limit.store(limit, Ordering::SeqCst);
                if session.user.must_change_password {
                    return Err(ControllerError::PermissionDenied);
                }
                if let Some(res) = self.list_behavior.lock().unwrap().as_ref() {
                    return match res {
                        Ok(page) => Ok(page.clone()),
                        Err(e) => Err(clone_controller_error(e)),
                    };
                }
                Ok(SessionPage {
                    items: vec![],
                    next_cursor: None,
                })
            })
        }
        fn revoke_by_alias<'a>(
            &'a self,
            session: &'a Session,
            _id: &'a str,
        ) -> AuthFuture<'a, bool> {
            Box::pin(async move {
                self.revoke_alias_calls.fetch_add(1, Ordering::SeqCst);
                if session.user.must_change_password {
                    return Err(ControllerError::PermissionDenied);
                }
                if let Some(res) = self.revoke_alias_behavior.lock().unwrap().as_ref() {
                    return match res {
                        Ok(v) => Ok(*v),
                        Err(e) => Err(clone_controller_error(e)),
                    };
                }
                Ok(false)
            })
        }
        fn revoke_others<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async move {
                self.revoke_others_calls.fetch_add(1, Ordering::SeqCst);
                if session.user.must_change_password {
                    return Err(ControllerError::PermissionDenied);
                }
                if let Some(res) = self.revoke_others_behavior.lock().unwrap().as_ref() {
                    return match res {
                        Ok(v) => Ok(*v),
                        Err(e) => Err(clone_controller_error(e)),
                    };
                }
                Ok(0)
            })
        }
    }

    fn lazy_db() -> DbPool {
        DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(100))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .unwrap(),
        )
    }

    fn app_state(auth: Arc<FakeAuth>) -> AppState {
        let config = HttpSecurityConfig {
            allowed_hosts: vec![HOST.into()],
            allowed_origin: Some(ORIGIN.into()),
            trust_tls: false,
        };
        AppState {
            db: lazy_db(),
            auth: Arc::new(HttpAuthState::new(auth, config)),
        }
    }

    fn router() -> Router {
        build_http_router(app_state(FakeAuth::new(FakeLogin::Ok)))
    }

    fn login_request(
        body: String,
        host: Option<&str>,
        origin: Option<&str>,
        content_type: Option<&str>,
        peer: Option<SocketAddr>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri("/api/v1/auth/login");
        if let Some(h) = host {
            builder = builder.header(header::HOST, h);
        }
        if let Some(o) = origin {
            builder = builder.header("origin", o);
        }
        if let Some(ct) = content_type {
            builder = builder.header(header::CONTENT_TYPE, ct);
        }
        let mut req = builder.body(Body::from(body)).unwrap();
        if let Some(p) = peer {
            req.extensions_mut().insert(ConnectInfo(p));
        }
        req
    }

    fn body_json(username: &str, password: &str) -> String {
        format!("{{\"username\":\"{username}\",\"password\":\"{password}\"}}")
    }

    fn valid_request() -> Request<Body> {
        login_request(
            body_json(USERNAME, PASSWORD),
            Some(HOST),
            Some(ORIGIN),
            Some("application/json"),
            Some(PEER.parse().unwrap()),
        )
    }

    /// The session cookie for the token minted by the fake login.
    fn valid_cookie() -> String {
        format!("rsc_session={}", fake_token())
    }

    /// GET /api/v1/auth/me with exactly one allowed Host header and NO
    /// Origin header: a read-only GET must not require Origin/X-CSRF.
    fn me_request(cookie: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("GET")
            .uri("/api/v1/auth/me")
            .header(header::HOST, HOST);
        if let Some(cookie) = cookie {
            builder = builder.header(header::COOKIE, cookie);
        }
        builder.body(Body::empty()).unwrap()
    }

    async fn call_status(router: &Router, req: Request<Body>) -> StatusCode {
        router.clone().oneshot(req).await.unwrap().status()
    }

    async fn call(
        router: &Router,
        req: Request<Body>,
    ) -> (StatusCode, HeaderMap, serde_json::Value, String) {
        let resp = router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = to_bytes(resp.into_body(), 8 * 1024 * 1024).await.unwrap();
        let text = String::from_utf8_lossy(&bytes).to_string();
        let value = serde_json::from_slice(&bytes).unwrap();
        (status, headers, value, text)
    }

    fn assert_error(text: &str, code: &str, message_key: &str) {
        let value: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(value["error"]["code"], json!(code), "text: {text}");
        assert_eq!(
            value["error"]["message_key"],
            json!(message_key),
            "text: {text}"
        );
        assert_eq!(value["error"]["params"], json!({}), "text: {text}");
        let error_keys: Vec<&str> = value
            .as_object()
            .and_then(|o| o.get("error"))
            .and_then(serde_json::Value::as_object)
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(
            error_keys,
            vec!["code", "message_key", "params"],
            "text: {text}"
        );
        let top_keys: Vec<&str> = value
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        assert_eq!(top_keys, vec!["error", "request_id"], "text: {text}");
        assert!(
            value["request_id"].is_string(),
            "request_id must be a string: {text}"
        );
    }

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
        let state = app_state(FakeAuth::new(FakeLogin::Ok));
        {
            let mut csrf = state.auth.csrf.lock().unwrap();
            for i in 0..MAX_CSRF_ENTRIES {
                let mut digest = [0u8; 32];
                digest[..4].copy_from_slice(&(i as u32).to_be_bytes());
                csrf.insert(
                    digest,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
            }
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
            for i in 0..MAX_CSRF_ENTRIES {
                let mut digest = [0u8; 32];
                digest[..4].copy_from_slice(&(i as u32).to_be_bytes());
                csrf.insert(
                    digest,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
            }
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
        // cap, without touching the map.
        let state = app_state(FakeAuth::new(FakeLogin::Ok));
        {
            let mut csrf = state.auth.csrf.lock().unwrap();
            for i in 0..MAX_CSRF_ENTRIES {
                let mut digest = [0u8; 32];
                digest[..4].copy_from_slice(&(i as u32).to_be_bytes());
                csrf.insert(
                    digest,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
            }
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
    async fn me_and_login_succeed_when_full_map_is_only_expired_bindings() {
        // Task 3b fix1 (I1): the original `Instant::now() - ABSOLUTE - 1s`
        // used the panicking std `Sub`, so on any host whose monotonic
        // baseline is below 12h1s (e.g. a freshly booted CI runner) this
        // test failed DETERMINISTICALLY with an underflow panic. `checked_sub`
        // returns `None` there; instead of silently skipping (which would
        // masquerade as E2E coverage) the test reports the clock limitation
        // and runs the host-independent pure prune boundary coverage.
        let Some(expired_at) = (Instant::now() - Duration::from_secs(1)).checked_sub(ABSOLUTE)
        else {
            eprintln!(
                "clock-limited host: monotonic baseline < 12h1s; cannot build \
                 a genuine 12h-old Instant, so the E2E expired-cap scenario \
                 (prune freeing the full map) is NOT covered on this host; \
                 running the pure prune boundary coverage instead"
            );
            prune_boundary_coverage();
            return;
        };
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        let live_digest = crate::auth::session::token_digest(b"still-live-binding");
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            for i in 0..MAX_CSRF_ENTRIES - 1 {
                let mut entry = [0u8; 32];
                entry[..4].copy_from_slice(&(i as u32).to_be_bytes());
                csrf.insert(
                    entry,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: expired_at,
                    },
                );
            }
            csrf.insert(
                live_digest,
                CsrfBinding {
                    token: "1".repeat(64),
                    issued_at: Instant::now(),
                },
            );
        }
        let router = build_http_router(state);
        // me: the monotonic prune frees the expired cap, so a new binding
        // is issued for the valid session.
        let (status, _headers, value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "expired bindings must be pruned before the cap check: {text}"
        );
        assert!(
            value["data"]["csrf_token"].as_str().is_some(),
            "me must carry a fresh csrf_token: {text}"
        );
        // login: the peek prune must also free capacity for a new login.
        let (status, _headers, value, text) = call(&router, valid_request()).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the login peek must prune expired bindings before failing closed: {text}"
        );
        let login_csrf = value["data"]["csrf_token"].as_str().unwrap().to_string();
        let bound = auth_state.csrf.lock().unwrap();
        assert_eq!(
            bound.len(),
            2,
            "only the live seeded binding + the session binding may remain"
        );
        assert!(
            bound.contains_key(&live_digest),
            "live bindings must survive the prune"
        );
        assert_eq!(
            bound.get(&digest).map(|b| b.token.as_str()),
            Some(login_csrf.as_str()),
            "the login must have (re)bound the session digest"
        );
    }

    #[tokio::test]
    async fn me_at_live_capacity_for_new_digest_is_503_retry_after_60() {
        let state = app_state(FakeAuth::new(FakeLogin::Ok));
        let auth_state = state.auth.clone();
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            for i in 0..MAX_CSRF_ENTRIES {
                let mut entry = [0u8; 32];
                entry[..4].copy_from_slice(&(i as u32).to_be_bytes());
                csrf.insert(
                    entry,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
            }
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
        let state = app_state(FakeAuth::new(FakeLogin::Ok));
        let auth_state = state.auth.clone();
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        let seeded = "f".repeat(64);
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            for i in 0..MAX_CSRF_ENTRIES - 1 {
                let mut entry = [0u8; 32];
                entry[..4].copy_from_slice(&(i as u32).to_be_bytes());
                csrf.insert(
                    entry,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
            }
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

    // ---------------- Task 3b fix1: prune semantics (pure) ----------------

    /// Host-independent prune coverage (Task 3b fix1, I1/I2). At a synthetic
    /// `now` exactly 12h after `t0`: an active (1s-old) binding and a
    /// 12h-minus-1s binding survive; a binding issued exactly at the 12h
    /// boundary is deleted. Runs on every host — including ones whose
    /// monotonic baseline is below 12h where a genuine past `Instant` cannot
    /// be constructed.
    fn prune_boundary_coverage() {
        let t0 = Instant::now();
        let mut csrf: HashMap<[u8; 32], CsrfBinding> = HashMap::new();
        csrf.insert(
            [0u8; 32],
            CsrfBinding {
                token: "0".repeat(64),
                issued_at: t0,
            },
        );
        csrf.insert(
            [1u8; 32],
            CsrfBinding {
                token: "1".repeat(64),
                issued_at: t0 + Duration::from_secs(1),
            },
        );
        csrf.insert(
            [2u8; 32],
            CsrfBinding {
                token: "2".repeat(64),
                issued_at: t0 + ABSOLUTE - Duration::from_secs(1),
            },
        );
        let pruned = prune_expired_csrf(&mut csrf, t0 + ABSOLUTE);
        assert_eq!(
            pruned, 1,
            "exactly the 12h-boundary binding must be deleted"
        );
        assert!(
            csrf.contains_key(&[1u8; 32]),
            "the active binding must survive the boundary prune"
        );
        assert!(
            csrf.contains_key(&[2u8; 32]),
            "the 12h-minus-1s binding must survive the boundary prune"
        );
        assert!(
            !csrf.contains_key(&[0u8; 32]),
            "the binding at the exact 12h boundary must be deleted"
        );
    }

    #[test]
    fn prune_keeps_active_and_deletes_at_12h_boundary() {
        prune_boundary_coverage();
    }

    /// A monotonic clock never moves backwards: a binding whose issue
    /// instant lies AFTER the prune instant can only come from an
    /// adversarial construct through the public `CsrfBinding` fields, and
    /// must be treated as expired (fail closed) instead of living forever
    /// and starving the 4096 cap.
    #[test]
    fn prune_deletes_future_issued_binding() {
        let now = Instant::now();
        let mut csrf: HashMap<[u8; 32], CsrfBinding> = HashMap::new();
        csrf.insert(
            [0u8; 32],
            CsrfBinding {
                token: "0".repeat(64),
                issued_at: now + Duration::from_secs(1),
            },
        );
        let pruned = prune_expired_csrf(&mut csrf, now);
        assert_eq!(
            pruned, 1,
            "a future-issued binding must be pruned as anomalous"
        );
        assert!(
            csrf.is_empty(),
            "a future-issued binding must not survive the prune"
        );
    }

    // ---------------- Task 3c: POST /api/v1/auth/password & POST /api/v1/auth/logout ----------------

    fn password_request(
        body: String,
        host: Option<&str>,
        origin: Option<&str>,
        cookie: Option<&str>,
        csrf: Option<&str>,
        content_type: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/password");
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

    fn logout_request(
        body: String,
        host: Option<&str>,
        origin: Option<&str>,
        cookie: Option<&str>,
        csrf: Option<&str>,
        content_type: Option<&str>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method("POST").uri("/api/v1/auth/logout");
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

    fn password_body(current: &str, new_pass: &str) -> String {
        serde_json::to_string(&json!({
            "current_password": current,
            "new_password": new_pass,
        }))
        .unwrap()
    }

    fn seed_session_and_csrf_custom(state: &AppState, raw_token: &str) -> (String, String) {
        let digest = crate::auth::session::token_digest(raw_token.as_bytes());
        let csrf_token = hex::encode([0x3c_u8; 32]);
        state.auth.csrf.lock().unwrap().insert(
            digest,
            CsrfBinding {
                token: csrf_token.clone(),
                issued_at: Instant::now(),
            },
        );
        (format!("rsc_session={raw_token}"), csrf_token)
    }

    fn seed_session_and_csrf(state: &AppState) -> (String, String) {
        seed_session_and_csrf_custom(state, &fake_token())
    }

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

    // ---------------- Task 3e: Rate Window Limiter unit tests ----------------

    #[test]
    fn test_session_write_rate_allows_20_and_rejects_21st() {
        let mut limiter = RateWindowLimiter::<[u8; 32]>::new(WRITE_RATE_LIMIT_PER_SEC);
        let key = [1u8; 32];
        let now = Instant::now();

        for i in 0..20 {
            assert_eq!(
                limiter.check_and_record(now, &key),
                Ok(()),
                "request {i} within limit should succeed"
            );
        }
        let err = limiter.check_and_record(now, &key);
        assert!(
            matches!(err, Err(WindowRateLimitError::Limited(_))),
            "21st request must be rate limited, got {err:?}"
        );
    }

    #[test]
    fn test_endpoint_read_rate_allows_50_and_rejects_51st() {
        let mut limiter = RateWindowLimiter::<(EndpointId, [u8; 32])>::new(READ_RATE_LIMIT_PER_SEC);
        let key = (EndpointId::Me, [2u8; 32]);
        let now = Instant::now();

        for i in 0..50 {
            assert_eq!(
                limiter.check_and_record(now, &key),
                Ok(()),
                "read {i} within limit should succeed"
            );
        }
        let err = limiter.check_and_record(now, &key);
        assert!(
            matches!(err, Err(WindowRateLimitError::Limited(_))),
            "51st read must be rate limited, got {err:?}"
        );
    }

    #[test]
    fn test_rate_window_recovers_after_one_second() {
        let mut limiter = RateWindowLimiter::<[u8; 32]>::new(WRITE_RATE_LIMIT_PER_SEC);
        let key = [3u8; 32];
        let now = Instant::now();

        for _ in 0..20 {
            assert_eq!(limiter.check_and_record(now, &key), Ok(()));
        }
        assert!(matches!(
            limiter.check_and_record(now, &key),
            Err(WindowRateLimitError::Limited(_))
        ));

        // Exactly 1 second later (or slightly after), quota reopens
        let later = now + Duration::from_secs(1);
        assert_eq!(
            limiter.check_and_record(later, &key),
            Ok(()),
            "quota must reopen after 1 second"
        );
    }

    #[test]
    fn test_capacity_exhaustion_fails_closed_503() {
        let mut limiter = RateWindowLimiter::<usize>::new(WRITE_RATE_LIMIT_PER_SEC);
        let now = Instant::now();

        for i in 0..MAX_RATE_LIMITER_KEYS {
            assert_eq!(limiter.check_and_record(now, &i), Ok(()));
        }

        // 4097th new key must fail closed with Exhausted
        let new_key = MAX_RATE_LIMITER_KEYS;
        let err = limiter.check_and_record(now, &new_key);
        assert_eq!(
            err,
            Err(WindowRateLimitError::Exhausted),
            "4097th new key must be Exhausted"
        );

        // Existing key still works within its limit
        assert_eq!(limiter.check_and_record(now, &0), Ok(()));
    }

    #[test]
    fn test_expired_keys_pruned_and_heals_capacity() {
        let mut limiter = RateWindowLimiter::<usize>::new(WRITE_RATE_LIMIT_PER_SEC);
        let now = Instant::now();

        for i in 0..MAX_RATE_LIMITER_KEYS {
            assert_eq!(limiter.check_and_record(now, &i), Ok(()));
        }

        // Step forward past 1s window: all 4096 keys expire
        let later = now + Duration::from_secs(2);

        // A new key can now be admitted because expired keys prune away
        let new_key = MAX_RATE_LIMITER_KEYS + 1;
        assert_eq!(
            limiter.check_and_record(later, &new_key),
            Ok(()),
            "new key must be admitted after expired keys prune"
        );
    }

    #[test]
    fn test_independent_keys_and_endpoints_do_not_interfere() {
        let mut write_limiter = RateWindowLimiter::<[u8; 32]>::new(WRITE_RATE_LIMIT_PER_SEC);
        let now = Instant::now();
        let key1 = [10u8; 32];
        let key2 = [20u8; 32];

        for _ in 0..20 {
            assert_eq!(write_limiter.check_and_record(now, &key1), Ok(()));
        }
        assert!(matches!(
            write_limiter.check_and_record(now, &key1),
            Err(WindowRateLimitError::Limited(_))
        ));
        assert_eq!(
            write_limiter.check_and_record(now, &key2),
            Ok(()),
            "key2 quota must be unaffected by key1"
        );

        let mut read_limiter =
            RateWindowLimiter::<(EndpointId, [u8; 32])>::new(READ_RATE_LIMIT_PER_SEC);
        let digest = [30u8; 32];
        for _ in 0..50 {
            assert_eq!(
                read_limiter.check_and_record(now, &(EndpointId::Me, digest)),
                Ok(())
            );
        }
        assert!(matches!(
            read_limiter.check_and_record(now, &(EndpointId::Me, digest)),
            Err(WindowRateLimitError::Limited(_))
        ));
    }

    #[test]
    fn test_future_dated_timestamp_pruned_safely() {
        let mut limiter = RateWindowLimiter::<[u8; 32]>::new(WRITE_RATE_LIMIT_PER_SEC);
        let key = [40u8; 32];
        let now = Instant::now();

        // Adversarial future-dated instant
        limiter
            .entries
            .entry(key)
            .or_default()
            .push_back(now + Duration::from_secs(10));

        // Checking at now must prune anomalous future timestamp and not panic
        assert_eq!(limiter.check_and_record(now, &key), Ok(()));
    }

    #[test]
    fn test_normal_request_does_not_prune_other_keys_until_full_capacity_with_new_key() {
        let mut limiter = RateWindowLimiter::<usize>::new(WRITE_RATE_LIMIT_PER_SEC);
        let now = Instant::now();

        // 1. Populate 4096 keys at time `now`
        for i in 0..MAX_RATE_LIMITER_KEYS {
            assert_eq!(limiter.check_and_record(now, &i), Ok(()));
        }

        // Reset probe count after setup
        limiter.global_prune_count = 0;

        // Advance 2 seconds: all keys are now expired
        let later = now + Duration::from_secs(2);

        // 2. Normal request on existing key 0:
        // Must succeed without doing a full-table O(N) global prune across the other 4095 keys.
        assert_eq!(limiter.check_and_record(later, &0), Ok(()));
        assert_eq!(
            limiter.global_prune_count, 0,
            "normal request on existing key must not trigger full-table global prune"
        );
        // And the map should still have entries for the unvisited keys until global prune or access
        assert!(
            limiter.entries.len() > 1,
            "unvisited keys should not have been swept by existing key check"
        );

        // 3. Normal request on new key when under capacity:
        // Let's create a limiter with only 10 keys
        let mut small_limiter = RateWindowLimiter::<usize>::new(WRITE_RATE_LIMIT_PER_SEC);
        for i in 0..10 {
            assert_eq!(small_limiter.check_and_record(now, &i), Ok(()));
        }
        small_limiter.global_prune_count = 0;
        // Request with new key 99 under capacity (10 < 4096):
        assert_eq!(small_limiter.check_and_record(later, &99), Ok(()));
        assert_eq!(
            small_limiter.global_prune_count, 0,
            "new key under capacity must not trigger full-table global prune"
        );

        // 4. Full table + new key:
        // Back to `limiter` which has 4096 entries (now all expired except key 0).
        // Requesting a new key (e.g. 99999) triggers the capacity check, which MUST perform global prune,
        // self-healing the expired keys and admitting the new key.
        assert_eq!(limiter.global_prune_count, 0);
        let new_key = 99999;
        assert_eq!(limiter.check_and_record(later, &new_key), Ok(()));
        assert_eq!(
            limiter.global_prune_count, 1,
            "full capacity encountering a new key must trigger exactly one global prune"
        );
        // After global prune, only key 0 (timestamp `later`) and new_key `99999` remain
        assert_eq!(
            limiter.entries.len(),
            2,
            "expired 4095 keys must be pruned away, leaving only key 0 and new_key"
        );
    }

    #[tokio::test]
    async fn password_session_write_rate_gate_oneshot() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state);

        // Precheck: missing/invalid CSRF before 20 valid requests must NOT consume quota
        for _ in 0..5 {
            let req = password_request(
                password_body("wrong-old", "new-pass"),
                Some(HOST),
                Some(ORIGIN),
                Some(&cookie),
                Some("wrong-csrf-token"),
                Some("application/json"),
            );
            let (status, _h, _v, text) = call(&router, req).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            assert_error(&text, "CSRF_INVALID", "security.csrf");
        }

        // Configure auth to return WrongOld so requests fail with 400 (business error) without revoking session
        auth.set_password_behavior(FakePassword::WrongOld);

        // 20 valid-CSRF requests consume the quota
        for i in 0..20 {
            let req = password_request(
                password_body("wrong-old", "new-pass"),
                Some(HOST),
                Some(ORIGIN),
                Some(&cookie),
                Some(&csrf),
                Some("application/json"),
            );
            let (status, _h, _v, text) = call(&router, req).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "attempt {i} should be 400");
            assert_error(&text, "INVALID_ARGUMENT", "auth.password.change_failed");
        }
        assert_eq!(auth.change_password_calls(), 20);

        // 21st valid-CSRF request must be 429 RATE_LIMITED, Retry-After: 1
        let req = password_request(
            password_body("wrong-old", "new-pass"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_error(&text, "RATE_LIMITED", "auth.rate_limited");
        assert_eq!(
            headers.get(header::RETRY_AFTER).unwrap().to_str().unwrap(),
            "1"
        );
        assert_eq!(
            auth.change_password_calls(),
            20,
            "blocked request must not reach service"
        );

        // Logout shares the same session write budget: 22nd request to logout must also 429
        let req_logout = logout_request(
            "{}".into(),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status_logout, headers_logout, _v, text_logout) = call(&router, req_logout).await;
        assert_eq!(status_logout, StatusCode::TOO_MANY_REQUESTS);
        assert_error(&text_logout, "RATE_LIMITED", "auth.rate_limited");
        assert_eq!(
            headers_logout
                .get(header::RETRY_AFTER)
                .unwrap()
                .to_str()
                .unwrap(),
            "1"
        );
        assert_eq!(
            auth.logout_calls(),
            0,
            "logout must not be called when rate limited"
        );
    }

    #[tokio::test]
    async fn me_endpoint_read_rate_gate_oneshot() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let router = build_http_router(state);
        let cookie = valid_cookie();

        // 1. Missing cookie requests do not consume read quota
        for _ in 0..10 {
            let req = me_request(None);
            let (status, _h, _v, text) = call(&router, req).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_error(&text, "AUTH_REQUIRED", "auth.required");
        }

        // 2. 50 authenticated requests on this session digest
        for i in 0..50 {
            let req = me_request(Some(&cookie));
            let (status, _h, _v, _text) = call(&router, req).await;
            assert_eq!(status, StatusCode::OK, "read {i} should be 200 OK");
        }
        assert_eq!(auth.authenticate_calls(), 50);

        // 3. 51st request must be 429 RATE_LIMITED, Retry-After: 1
        let req = me_request(Some(&cookie));
        let (status, headers, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_error(&text, "RATE_LIMITED", "auth.rate_limited");
        assert_eq!(
            headers.get(header::RETRY_AFTER).unwrap().to_str().unwrap(),
            "1"
        );
        // authenticate is called to verify session before gate
        assert_eq!(auth.authenticate_calls(), 51);
    }
    #[tokio::test]
    async fn password_session_write_rate_gate_bad_json_consumes_quota() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let (cookie, csrf) = seed_session_and_csrf(&state);
        let router = build_http_router(state);

        // 20 requests with malformed JSON body AFTER valid CSRF
        for _ in 0..20 {
            let req = password_request(
                "invalid-json".into(),
                Some(HOST),
                Some(ORIGIN),
                Some(&cookie),
                Some(&csrf),
                Some("application/json"),
            );
            let (status, _h, _v, text) = call(&router, req).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_error(&text, "INVALID_ARGUMENT", "auth.password.bad_body");
        }

        // 21st request must be 429 RATE_LIMITED even if body is valid
        let req = password_request(
            password_body("correct-old", "new-pass"),
            Some(HOST),
            Some(ORIGIN),
            Some(&cookie),
            Some(&csrf),
            Some("application/json"),
        );
        let (status, headers, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert_error(&text, "RATE_LIMITED", "auth.rate_limited");
        assert_eq!(
            headers.get(header::RETRY_AFTER).unwrap().to_str().unwrap(),
            "1"
        );
    }

    #[tokio::test]
    async fn me_endpoint_read_capacity_exhaustion_503() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let router = build_http_router(state.clone());

        // Pre-fill read_limiter to MAX_RATE_LIMITER_KEYS with dummy keys
        let now = Instant::now();
        {
            let mut read_limiter = state.auth.read_limiter.lock().unwrap();
            for i in 0..MAX_RATE_LIMITER_KEYS {
                let mut dummy_digest = [0u8; 32];
                dummy_digest[0..8].copy_from_slice(&(i as u64).to_le_bytes());
                read_limiter
                    .check_and_record(now, &(EndpointId::Me, dummy_digest))
                    .unwrap();
            }
        }

        // Now a request with valid session token whose digest is not in the map
        // Note: call(&router, req) creates a fresh router with now = Instant::now().
        // If some small execution time passed since `now` in the pre-fill above,
        // we must make sure the dummy entries are recorded at a time that does NOT expire immediately.
        let now = Instant::now();
        {
            let mut read_limiter = state.auth.read_limiter.lock().unwrap();
            read_limiter.entries.clear();
            for i in 0..MAX_RATE_LIMITER_KEYS {
                let mut dummy_digest = [0u8; 32];
                dummy_digest[0..8].copy_from_slice(&(i as u64).to_le_bytes());
                // Push an instant that is now (within 1s)
                let mut dq = VecDeque::new();
                dq.push_back(now);
                read_limiter
                    .entries
                    .insert((EndpointId::Me, dummy_digest), dq);
            }
        }
        let req = me_request(Some(&valid_cookie()));
        let (status, headers, _v, text) = call(&router, req).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_error(&text, "RESOURCE_EXHAUSTED", "system.resource_exhausted");
        assert_eq!(
            headers.get(header::RETRY_AFTER).unwrap().to_str().unwrap(),
            "60"
        );
    }

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

    // 11. 对象安全编译证明
    #[tokio::test]
    async fn test_group_11_trait_object_safety_compilation_proof() {
        // Must compile and coerce to Arc<dyn AuthServiceApi>
        let db = lazy_db();
        let repo = Arc::new(crate::auth::sqlx_repo::SqlxIdentityRepository::new(
            db.clone(),
        ));
        let service = crate::auth::service::AuthService::new(repo).unwrap();
        let live = crate::http_live::LiveAuth::new(Arc::new(service), db);
        let api: Arc<dyn AuthServiceApi> = Arc::new(live);
        assert!(!api.is_live_session(&[0x7c; 32]));
    }
}

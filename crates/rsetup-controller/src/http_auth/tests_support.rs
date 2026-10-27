use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode, header};
use serde_json::json;
use tower::ServiceExt;

use crate::auth::service::{IdentityUser, Login, Session, SessionPage};
use crate::auth::session::token_digest;
use crate::db::DbPool;
use crate::error::ControllerError;
use crate::http_auth::contract::{AuthFuture, AuthServiceApi, SessionAuthority};
use crate::http_auth::csrf::CsrfBinding;
use crate::http_auth::router::build_http_router;
use crate::http_auth::state::{AppState, HttpAuthState};
use crate::http_security::HttpSecurityConfig;

pub(super) const HOST: &str = "127.0.0.1:8080";
pub(super) const ORIGIN: &str = "http://127.0.0.1:5173";
pub(super) const PEER: &str = "10.1.1.1:5000";
pub(super) const PEER2: &str = "10.1.1.2:5000";
pub(super) const USERNAME: &str = "admin";
pub(super) const PASSWORD: &str = "correct-horse-battery";

pub(super) fn fake_token() -> String {
    hex::encode([0x5a_u8; 32])
}

pub(super) fn clone_controller_error(e: &ControllerError) -> ControllerError {
    match e {
        ControllerError::InvalidArgument => ControllerError::InvalidArgument,
        ControllerError::RevisionConflict => ControllerError::RevisionConflict,
        ControllerError::PermissionDenied => ControllerError::PermissionDenied,
        ControllerError::NotFound => ControllerError::NotFound,
        ControllerError::ResourceExhausted => ControllerError::ResourceExhausted,
        ControllerError::Config(s) => ControllerError::Config(s.clone()),
        ControllerError::SchemaNotReady { found, required } => ControllerError::SchemaNotReady {
            found: *found,
            required: *required,
        },
        ControllerError::Database(_) => ControllerError::Database(sqlx::Error::Configuration(
            std::io::Error::other("synthetic db fault").into(),
        )),
        ControllerError::Crypto => ControllerError::Crypto,
        ControllerError::LastAdmin => ControllerError::LastAdmin,
        ControllerError::InvalidState => ControllerError::InvalidState,
        ControllerError::TaskIdempotencyConflict => ControllerError::TaskIdempotencyConflict,
        ControllerError::TaskPreviewInvalidState => ControllerError::TaskPreviewInvalidState,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FakeLogin {
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
pub(super) enum FakeSession {
    Ok,
    InvalidArgument,
    Database,
    Crypto,
    Config,
    SchemaNotReady,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FakePassword {
    Ok,
    WrongOld,
    NewInvalid,
    ConcurrentlyRevoked,
    RevisionConflict,
    Database,
    Crypto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FakeLogout {
    Ok,
    Database,
    Crypto,
}

pub(super) struct FakeAuth {
    behavior: Mutex<FakeLogin>,
    session_behavior: Mutex<FakeSession>,
    password_behavior: Mutex<FakePassword>,
    logout_behavior: Mutex<FakeLogout>,
    session_dead: std::sync::atomic::AtomicBool,
    pub(super) extra_valid_tokens: Mutex<std::collections::HashSet<String>>,
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
    pub(super) fn new(behavior: FakeLogin) -> Arc<Self> {
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
    pub(super) fn set_list_behavior(&self, result: Result<SessionPage, ControllerError>) {
        *self.list_behavior.lock().unwrap() = Some(result);
    }
    pub(super) fn set_revoke_alias_result(&self, result: Result<bool, ControllerError>) {
        *self.revoke_alias_behavior.lock().unwrap() = Some(result);
    }
    pub(super) fn set_revoke_others_result(&self, result: Result<u64, ControllerError>) {
        *self.revoke_others_behavior.lock().unwrap() = Some(result);
    }
    /// Task 3f test seam: explicitly mark a digest live/dead in the fake
    /// in-process clock (e.g. model a simulated logout).
    pub(super) fn set_digest_live(&self, digest: [u8; 32], live: bool) {
        self.live_overrides.lock().unwrap().insert(digest, live);
    }
    pub(super) fn set_behavior(&self, behavior: FakeLogin) {
        *self.behavior.lock().unwrap() = behavior;
    }
    pub(super) fn set_session_behavior(&self, behavior: FakeSession) {
        *self.session_behavior.lock().unwrap() = behavior;
    }
    pub(super) fn set_password_behavior(&self, behavior: FakePassword) {
        *self.password_behavior.lock().unwrap() = behavior;
    }
    pub(super) fn set_logout_behavior(&self, behavior: FakeLogout) {
        *self.logout_behavior.lock().unwrap() = behavior;
    }
    pub(super) fn push_authenticate_step(&self, behavior: FakeSession) {
        self.authenticate_sequence
            .lock()
            .unwrap()
            .push_back(behavior);
    }
    pub(super) fn set_epoch(&self, epoch: u64) {
        self.epoch.store(epoch, Ordering::SeqCst);
    }
    pub(super) fn login_calls(&self) -> usize {
        self.login_calls.load(Ordering::SeqCst)
    }
    pub(super) fn authenticate_calls(&self) -> usize {
        self.authenticate_calls.load(Ordering::SeqCst)
    }
    pub(super) fn authz_epoch_calls(&self) -> usize {
        self.authz_epoch_calls.load(Ordering::SeqCst)
    }
    pub(super) fn change_password_calls(&self) -> usize {
        self.change_password_calls.load(Ordering::SeqCst)
    }
    pub(super) fn logout_calls(&self) -> usize {
        self.logout_calls.load(Ordering::SeqCst)
    }
    pub(super) fn list_calls(&self) -> usize {
        self.list_calls.load(Ordering::SeqCst)
    }
    pub(super) fn revoke_alias_calls(&self) -> usize {
        self.revoke_alias_calls.load(Ordering::SeqCst)
    }
    pub(super) fn revoke_others_calls(&self) -> usize {
        self.revoke_others_calls.load(Ordering::SeqCst)
    }
    pub(super) fn last_list_limit(&self) -> usize {
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
    pub(super) fn fixed_login() -> Login {
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
                FakePassword::Database => Err(ControllerError::Database(
                    sqlx::Error::Configuration(std::io::Error::other("synthetic db fault").into()),
                )),
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
                FakeLogout::Database => Err(ControllerError::Database(sqlx::Error::Configuration(
                    std::io::Error::other("synthetic db fault").into(),
                ))),
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
    fn session_authority<'a>(&'a self, digest: &'a [u8; 32]) -> AuthFuture<'a, SessionAuthority> {
        // Fake 权威库：会话有效性沿用 `is_live_session` 的同一套模型（显式
        // mark 优先），epoch 直接读 fake 的合成 epoch（不累加 authz_epoch_calls，
        // 以免扰动 `me` 的「每请求读一次」断言）。刻意不恒真。
        let session_live = self.is_live_session(digest);
        let is_admin = Self::fixed_user().is_admin;
        let authz_epoch = self.epoch.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(SessionAuthority {
                session_live,
                is_admin,
                authz_epoch,
            })
        })
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
    fn revoke_by_alias<'a>(&'a self, session: &'a Session, _id: &'a str) -> AuthFuture<'a, bool> {
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

pub(super) fn lazy_db() -> DbPool {
    DbPool(
        sqlx::mysql::MySqlPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(100))
            .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
            .unwrap(),
    )
}

pub(super) fn app_state(auth: Arc<FakeAuth>) -> AppState {
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

pub(super) fn router() -> Router {
    build_http_router(app_state(FakeAuth::new(FakeLogin::Ok)))
}

/// Seed `count` synthetic CSRF bindings whose digests the fake process
/// clock reports as LIVE (`set_digest_live(.., true)`).
///
/// Capacity tests must fill the table with live bindings: the handlers
/// now sweep slots whose digest the in-memory clock already declares
/// dead, so seeding with plain unknown digests would no longer model a
/// table that genuinely cannot admit a new binding. The fake marks the
/// digests live explicitly and never defaults to `true` for unknown
/// digests, so a missing mark shows up as a pruned slot, not as a
/// silently passing test.
pub(super) fn seed_live_csrf_bindings(
    auth: &FakeAuth,
    csrf: &mut HashMap<[u8; 32], CsrfBinding>,
    count: usize,
) {
    for i in 0..count {
        let mut digest = [0u8; 32];
        digest[..4].copy_from_slice(&(i as u32).to_be_bytes());
        auth.set_digest_live(digest, true);
        csrf.insert(
            digest,
            CsrfBinding {
                token: "0".repeat(64),
                issued_at: Instant::now(),
            },
        );
    }
}

pub(super) fn login_request(
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

pub(super) fn body_json(username: &str, password: &str) -> String {
    format!("{{\"username\":\"{username}\",\"password\":\"{password}\"}}")
}

pub(super) fn valid_request() -> Request<Body> {
    login_request(
        body_json(USERNAME, PASSWORD),
        Some(HOST),
        Some(ORIGIN),
        Some("application/json"),
        Some(PEER.parse().unwrap()),
    )
}

/// The session cookie for the token minted by the fake login.
pub(super) fn valid_cookie() -> String {
    format!("rsc_session={}", fake_token())
}

/// GET /api/v1/auth/me with exactly one allowed Host header and NO
/// Origin header: a read-only GET must not require Origin/X-CSRF.
pub(super) fn me_request(cookie: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri("/api/v1/auth/me")
        .header(header::HOST, HOST);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    builder.body(Body::empty()).unwrap()
}

pub(super) async fn call_status(router: &Router, req: Request<Body>) -> StatusCode {
    router.clone().oneshot(req).await.unwrap().status()
}

pub(super) async fn call(
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

pub(super) fn assert_error(text: &str, code: &str, message_key: &str) {
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

pub(super) fn password_request(
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

pub(super) fn logout_request(
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

pub(super) fn password_body(current: &str, new_pass: &str) -> String {
    serde_json::to_string(&json!({
        "current_password": current,
        "new_password": new_pass,
    }))
    .unwrap()
}

pub(super) fn seed_session_and_csrf_custom(state: &AppState, raw_token: &str) -> (String, String) {
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

pub(super) fn seed_session_and_csrf(state: &AppState) -> (String, String) {
    seed_session_and_csrf_custom(state, &fake_token())
}

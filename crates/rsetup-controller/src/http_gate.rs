// 顶层 imports = 生产代码实际引用的集合（桩签名 + Step 3 搬入的函数）。
// `Duration` 供搬入的 `rate_limited(remaining: Duration)`、`HeaderValue` 供
// `exhausted()` 的 `HeaderValue::from_static("60")`：二者都是搬移后的生产依赖，
// 明确保留在顶层、不随仅 test 所需 imports 迁入 test 模块。
// 仅 test 所需的 `Arc`/`AtomicUsize`/`Ordering`、`IdentityUser`/`Login`/`SessionPage`、
// `AuthFuture`/`AuthServiceApi`、`HttpSecurityConfig` 与 `to_bytes` 一律改在下方 `mod tests`
// 内声明（生产代码只以全限定路径引用 `axum::body::Body`/`axum::body::to_bytes`，顶层无需 import
// 该模块）；全程不加 `#[allow(unused_imports)]`。
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde_json::json;

use crate::auth::service::Session;
use crate::auth::session::token_digest;
use crate::error::ControllerError;
use crate::http_api::err_response;
use crate::http_auth::{EndpointId, HttpAuthState, WindowRateLimitError};
use crate::http_security::check_session_write;

/// JSON write-body cap (spec 02 §3): 1 MiB, any overrun is a fixed 400.
pub(crate) const MAX_BODY_BYTES: usize = 1_048_576;

/// Exactly one header value, as a str. A missing header, a duplicated
/// header (any number of values above one), or a non-ASCII value all
/// fail closed as `None`; the caller maps that to its fixed rejection
/// (403 for Host/Origin, 400 for Content-Type). Repeated values must
/// never be reduced to "take the first".
pub(crate) fn single_header(headers: &HeaderMap, key: HeaderName) -> Option<&str> {
    let mut values = headers.get_all(key).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()
}

/// Exactly one `rsc_session` cookie value (exactly 64 lowercase hex chars)
/// across all `Cookie` headers. Other cookie names are ignored; empty
/// pairs (e.g. a trailing `;`) carry no cookie and are skipped; a missing
/// header, a non-ASCII header, a pair without `=`, an empty/short/long/
/// non-hex/uppercase value, or two-or-more `rsc_session` occurrences all
/// fail closed as `None`.
pub(crate) fn session_cookie_value(headers: &HeaderMap) -> Option<String> {
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

/// Checked origin header: 0 occurrences is Ok(None); exactly 1 ASCII value
/// is Ok(Some(&str)); 2+ occurrences or non-ASCII fails as Err(()).
pub(crate) fn get_single_origin(headers: &HeaderMap) -> Result<Option<&str>, ()> {
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

/// JSON Content-Type: the media type must be exactly `application/json`
/// (parameters such as charset are tolerated).
pub(crate) fn json_content_type(content_type: Option<&str>) -> bool {
    content_type
        .and_then(|value| value.split(';').next())
        .map(|media_type| media_type.trim().eq_ignore_ascii_case("application/json"))
        .unwrap_or(false)
}

/// Fixed 401 for every missing/duplicated/malformed/invalid session.
pub(crate) fn auth_required() -> Response {
    err_response(
        StatusCode::UNAUTHORIZED,
        "AUTH_REQUIRED",
        "auth.required",
        json!({}),
    )
}

/// Fixed responses for backend faults that are NOT a definitive
/// `InvalidArgument` (which the caller maps to 401): Database is
/// `STORAGE_UNAVAILABLE` (503), `LastAdmin` and `InvalidState` are state
/// conflicts (409), every other variant is `NOT_READY` (503). No raw error
/// text ever reaches the body.
pub(crate) fn backend_error(error: &ControllerError) -> Response {
    match error {
        ControllerError::Database(_) => err_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "STORAGE_UNAVAILABLE",
            "system.storage_unavailable",
            json!({}),
        ),
        ControllerError::LastAdmin => err_response(
            StatusCode::CONFLICT,
            "LAST_ADMIN",
            "identity.last_admin",
            json!({}),
        ),
        ControllerError::InvalidState => err_response(
            StatusCode::CONFLICT,
            "INVALID_STATE",
            "identity.invalid_state",
            json!({}),
        ),
        _ => not_ready(),
    }
}

pub(crate) fn csrf_invalid() -> Response {
    err_response(
        StatusCode::FORBIDDEN,
        "CSRF_INVALID",
        "security.csrf",
        json!({}),
    )
}

pub(crate) fn password_change_required() -> Response {
    err_response(
        StatusCode::FORBIDDEN,
        "PASSWORD_CHANGE_REQUIRED",
        "auth.password_change_required",
        json!({}),
    )
}

pub(crate) fn origin_host_invalid() -> Response {
    err_response(
        StatusCode::FORBIDDEN,
        "CSRF_INVALID",
        "security.origin_host",
        json!({}),
    )
}

pub(crate) fn not_ready() -> Response {
    err_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "NOT_READY",
        "system.not_ready",
        json!({}),
    )
}

/// Fixed 503 + `Retry-After: 60` for a fail-closed resource cap.
pub(crate) fn exhausted() -> Response {
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
pub(crate) fn rate_limited(remaining: Duration) -> Response {
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

/// 管理读边界：单值 Host → 单值严格 cookie → 进程内活会话预检 →
/// DB 权威 authenticate → 50/s 端点读限速。任一步失败都不得触达业务网关。
// 本切片只交付门禁本身：26 条管理路由与 Runtime 任务路由在后续任务接入，
// 接入前 crate 内没有生产调用方，dead_code 属预期而非遗漏。
// Err 变体是 axum `Response`（≥128 字节）：签名由 Identity 计划固定，
// 失败路径直接返回固定错误响应、不在热路径装箱，故按函数抑制该 lint。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) async fn authenticate_read(
    state: &HttpAuthState,
    headers: &HeaderMap,
    endpoint: EndpointId,
) -> Result<Session, Response> {
    let host = single_header(headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return Err(origin_host_invalid());
    }
    let Some(raw) = session_cookie_value(headers) else {
        return Err(auth_required());
    };
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return Err(auth_required());
    }
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(session) => session,
        Err(ControllerError::InvalidArgument) => {
            state.csrf.lock().unwrap().remove(&digest);
            return Err(auth_required());
        }
        Err(error) => return Err(backend_error(&error)),
    };
    let now = Instant::now();
    let mut limiter = state.read_limiter.lock().unwrap();
    match limiter.check_and_record(now, &(endpoint, session.digest)) {
        Err(WindowRateLimitError::Exhausted) => return Err(exhausted()),
        Err(WindowRateLimitError::Limited(remaining)) => return Err(rate_limited(remaining)),
        Ok(()) => {}
    }
    Ok(session)
}

/// 管理写边界：单值 Host → Origin（缺失仅豁免 Origin）→ 单值严格 cookie →
/// 活会话预检 → DB 权威 authenticate → 绑定 session.digest 的 CSRF →
/// 20/s 会话写限速。body 由 handler 在其后调用 `read_json_body` 读取。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) async fn authenticate_write(
    state: &HttpAuthState,
    headers: &HeaderMap,
) -> Result<Session, Response> {
    let host = single_header(headers, header::HOST);
    if !host.is_some_and(|h| state.config.allowed_hosts.iter().any(|a| a == h)) {
        return Err(origin_host_invalid());
    }
    let origin = match get_single_origin(headers) {
        Ok(origin) => origin,
        Err(()) => return Err(origin_host_invalid()),
    };
    if origin.is_some_and(|o| state.config.allowed_origin.as_deref() != Some(o)) {
        return Err(origin_host_invalid());
    }
    let Some(raw) = session_cookie_value(headers) else {
        return Err(auth_required());
    };
    let digest = token_digest(raw.as_bytes());
    if !state.auth.is_live_session(&digest) {
        state.csrf.lock().unwrap().remove(&digest);
        return Err(auth_required());
    }
    let session = match state.auth.authenticate(raw.as_str()).await {
        Ok(session) => session,
        Err(ControllerError::InvalidArgument) => {
            state.csrf.lock().unwrap().remove(&digest);
            return Err(auth_required());
        }
        Err(error) => return Err(backend_error(&error)),
    };
    let csrf_header = single_header(headers, HeaderName::from_static("x-csrf-token"));
    let binding = state.csrf.lock().unwrap().get(&session.digest).cloned();
    let Some(binding) = binding else {
        return Err(csrf_invalid());
    };
    if check_session_write(host, origin, csrf_header, &binding.token, &state.config).is_err() {
        return Err(csrf_invalid());
    }
    let now = Instant::now();
    let mut limiter = state.write_limiter.lock().unwrap();
    match limiter.check_and_record(now, &session.digest) {
        Err(WindowRateLimitError::Exhausted) => return Err(exhausted()),
        Err(WindowRateLimitError::Limited(remaining)) => return Err(rate_limited(remaining)),
        Ok(()) => {}
    }
    Ok(session)
}

/// `application/json` + ≤1 MiB + serde 结构校验；任何失败都是固定 400
/// `INVALID_ARGUMENT` / `request.invalid_body`，不回显原始 body 文本。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) async fn read_json_body<T: serde::de::DeserializeOwned>(
    headers: &HeaderMap,
    body: axum::body::Body,
) -> Result<T, Response> {
    if !json_content_type(single_header(headers, header::CONTENT_TYPE)) {
        return Err(invalid_body());
    }
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_BODY_BYTES as u64) {
        return Err(invalid_body());
    }
    let bytes = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => return Err(invalid_body()),
    };
    serde_json::from_slice(&bytes).map_err(|_| invalid_body())
}

#[allow(dead_code)]
fn invalid_body() -> Response {
    err_response(
        StatusCode::BAD_REQUEST,
        "INVALID_ARGUMENT",
        "request.invalid_body",
        serde_json::json!({}),
    )
}

/// 管理范围前置过滤：`must_change_password` 与普通无权限必须返回不同
/// 403 code。**这只是廉价前置**；权威判定在仓储事务 guard 内重新核验。
#[allow(dead_code, clippy::result_large_err)]
pub(crate) fn require_admin_session(session: &Session) -> Result<(), Response> {
    if session.user.must_change_password {
        return Err(password_change_required());
    }
    if session.user.active && session.user.is_admin {
        return Ok(());
    }
    Err(err_response(
        StatusCode::FORBIDDEN,
        "PERMISSION_DENIED",
        "permission.denied",
        serde_json::json!({}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::auth::service::{IdentityUser, Login, SessionPage};
    use crate::http_auth::{AuthFuture, AuthServiceApi};
    use crate::http_security::HttpSecurityConfig;
    use axum::body::to_bytes;

    /// `calls` 为真实 `Arc<AtomicUsize>` 认证 spy 计数器：每次实际
    /// `authenticate` 调用递增 1；测试侧持有独立 clone 读取。它不能永远
    /// 返回 0——阳性控制（合法单 Host/cookie 恰好 1）就是抓"恒 0"假计数
    /// 的。所有 fixture 构造（每个测试内的 `StubAuth { .. }` 字面量）都
    /// 必须带上该字段。
    struct StubAuth {
        token: String,
        session: Session,
        calls: Arc<AtomicUsize>,
    }

    fn fixture_session() -> Session {
        Session {
            user: IdentityUser {
                id: [7u8; 16],
                username: "operator".into(),
                password_hash: "not-a-real-hash".into(),
                active: true,
                is_admin: true,
                must_change_password: false,
                revision: 3,
            },
            digest: [9u8; 32],
        }
    }

    impl AuthServiceApi for StubAuth {
        fn login<'a>(&'a self, _u: &'a str, _p: &'a str) -> AuthFuture<'a, Login> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session> {
            // 在实际 authenticate 入口递增（无论随后成功/失败）：
            // 门禁在 Host/Cookie 步被拒时这里根本不会执行，计数器保持 0。
            self.calls.fetch_add(1, Ordering::SeqCst);
            let ok = raw == self.token;
            let session = self.session.clone();
            Box::pin(async move {
                if ok {
                    Ok(session)
                } else {
                    Err(ControllerError::InvalidArgument)
                }
            })
        }
        fn change_password<'a>(
            &'a self,
            _s: &'a Session,
            _c: &'a str,
            _n: &'a str,
        ) -> AuthFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn logout<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn authz_epoch(&self) -> AuthFuture<'_, u64> {
            Box::pin(async { Ok(0) })
        }
        fn is_live_session(&self, _d: &[u8; 32]) -> bool {
            true
        }
        fn list_sessions<'a>(
            &'a self,
            _s: &'a Session,
            _c: Option<&'a str>,
            _l: usize,
        ) -> AuthFuture<'a, SessionPage> {
            Box::pin(async { Err(ControllerError::InvalidArgument) })
        }
        fn revoke_by_alias<'a>(&'a self, _s: &'a Session, _i: &'a str) -> AuthFuture<'a, bool> {
            Box::pin(async { Ok(false) })
        }
        fn revoke_others<'a>(&'a self, _s: &'a Session) -> AuthFuture<'a, u64> {
            Box::pin(async { Ok(0) })
        }
    }

    fn state_with(stub: StubAuth) -> HttpAuthState {
        HttpAuthState::new(
            Arc::new(stub),
            HttpSecurityConfig::from_values(
                Some("controller.local"),
                Some("http://controller.local"),
                None,
            )
            .unwrap(),
        )
    }

    fn headers(host: &str, cookie: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        if let Some(c) = cookie {
            h.insert(header::COOKIE, HeaderValue::from_str(c).unwrap());
        }
        h
    }

    #[tokio::test]
    async fn gate_rejects_host_and_cookie_failures_without_authenticating() {
        let token = "a".repeat(64);
        let calls = Arc::new(AtomicUsize::new(0));
        let stub = StubAuth {
            token: token.clone(),
            session: fixture_session(),
            calls: calls.clone(),
        };
        let state = state_with(stub);
        let cookie = format!("rsc_session={token}");

        // 重复 Host：必须 403 CSRF_INVALID，且不进入 authenticate。
        let mut h = headers("controller.local", Some(&cookie));
        h.append(header::HOST, HeaderValue::from_static("controller.local"));
        let response = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect_err("duplicate Host must be rejected");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "duplicate Host must not reach authenticate"
        );

        // 错误 Host：必须 403 CSRF_INVALID，且不进入 authenticate。
        let response = authenticate_read(
            &state,
            &headers("wrong.host", Some(&cookie)),
            EndpointId::UsersRead,
        )
        .await
        .expect_err("wrong Host must be rejected");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "wrong Host must not reach authenticate"
        );

        // 缺失 Host：必须 403 CSRF_INVALID，且不进入 authenticate。
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
        let response = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect_err("missing Host must be rejected");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "missing Host must not reach authenticate"
        );

        // 缺失 cookie：必须 401 AUTH_REQUIRED，且不进入 authenticate。
        let response = authenticate_read(
            &state,
            &headers("controller.local", None),
            EndpointId::UsersRead,
        )
        .await
        .expect_err("missing cookie must be rejected");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "missing cookie must not reach authenticate"
        );

        // 畸形 cookie（长度不足）：必须 401，且不进入 authenticate。
        let response = authenticate_read(
            &state,
            &headers("controller.local", Some("rsc_session=short")),
            EndpointId::UsersRead,
        )
        .await
        .expect_err("malformed cookie must be rejected");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "malformed cookie must not reach authenticate"
        );

        // 重复 cookie（两个 rsc_session）：必须 401，且不进入 authenticate。
        let response = authenticate_read(
            &state,
            &headers("controller.local", Some(&format!("{cookie}; {cookie}"))),
            EndpointId::UsersRead,
        )
        .await
        .expect_err("duplicate cookie must be rejected");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "duplicate cookie must not reach authenticate"
        );
    }

    #[tokio::test]
    async fn gate_accepts_exactly_one_valid_cookie_and_returns_authenticated_session() {
        let token = "b".repeat(64);
        let calls = Arc::new(AtomicUsize::new(0));
        let state = state_with(StubAuth {
            token: token.clone(),
            session: fixture_session(),
            calls: calls.clone(),
        });
        let h = headers("controller.local", Some(&format!("rsc_session={token}")));
        let session = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect("valid single cookie must authenticate");
        assert_eq!(session.user.id, [7u8; 16]);
        // 阳性控制：合法单 Host/cookie 时权威 authenticate 恰好被调用 1 次
        // （计数器不能恒 0；一个"恒 0 spy"会通过所有拒绝断言却在这里失败）。
        // 数据响应的 error code/message_key 按已有契约断言，不因本计数器"顺便"更改。
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "exactly one authoritative authenticate for a valid request"
        );
    }

    #[tokio::test]
    async fn read_buckets_are_independent_per_endpoint_variant_and_session() {
        let token = "c".repeat(64);
        let state = state_with(StubAuth {
            token: token.clone(),
            session: fixture_session(),
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let h = headers("controller.local", Some(&format!("rsc_session={token}")));

        // 同一有权 session：UsersRead 第 1..=50 次通过，第 51 次 429。
        for i in 1..=50 {
            authenticate_read(&state, &h, EndpointId::UsersRead)
                .await
                .unwrap_or_else(|_| panic!("UsersRead #{i} must pass within 50/s"));
        }
        let limited = authenticate_read(&state, &h, EndpointId::UsersRead)
            .await
            .expect_err("the 51st UsersRead call must be rate limited");
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);

        // RolesRead 桶相互独立：第 1 次成功（未被 UsersRead 耗尽）。
        authenticate_read(&state, &h, EndpointId::RolesRead)
            .await
            .expect("first RolesRead call must pass (independent bucket)");

        // 既有 Me 桶不被管理读变体借用：第 1 次成功。
        authenticate_read(&state, &h, EndpointId::Me)
            .await
            .expect("first Me call must pass (Me is not borrowed)");
    }

    #[tokio::test]
    async fn same_template_with_different_ids_shares_one_read_bucket() {
        let token = "d".repeat(64);
        let state = state_with(StubAuth {
            token: token.clone(),
            session: fixture_session(),
            calls: Arc::new(AtomicUsize::new(0)),
        });
        let h = headers("controller.local", Some(&format!("rsc_session={token}")));

        // handler 只传模板变体（EffectivePermissionsRead）而不传对象 id：
        // GET /users/{idA}/effective-permissions 与 GET /users/{idB}/effective-permissions
        // 共享同一 (variant, session.digest) 桶——不是按 id 的无限桶。
        // 这里以同一变体直接调门禁，证明同模板第 51 次调用（换个 id 也一样）耗尽桶。
        for i in 1..=50 {
            authenticate_read(&state, &h, EndpointId::EffectivePermissionsRead)
                .await
                .unwrap_or_else(|_| panic!("call #{i} must pass within 50/s"));
        }
        let limited = authenticate_read(&state, &h, EndpointId::EffectivePermissionsRead)
            .await
            .expect_err("51st same-template call must be rate limited even with a different id");
        assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn require_admin_session_distinguishes_force_password_change_from_plain_denial() {
        let mut user = fixture_session().user;
        user.must_change_password = true;
        let session = Session {
            user,
            digest: [0u8; 32],
        };
        let response =
            require_admin_session(&session).expect_err("must change password is refused");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "PASSWORD_CHANGE_REQUIRED");
        assert_eq!(
            body["error"]["message_key"],
            "auth.password_change_required"
        );

        let mut user = fixture_session().user;
        user.is_admin = false;
        let session = Session {
            user,
            digest: [0u8; 32],
        };
        let response = require_admin_session(&session).expect_err("non-admin is refused");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "PERMISSION_DENIED");
        assert_eq!(body["error"]["message_key"], "permission.denied");

        assert!(require_admin_session(&fixture_session()).is_ok());
    }

    #[tokio::test]
    async fn last_admin_and_invalid_state_map_to_409_not_503() {
        let last_admin = backend_error(&ControllerError::LastAdmin);
        assert_eq!(last_admin.status(), StatusCode::CONFLICT);
        let bytes = to_bytes(last_admin.into_body(), 64 * 1024).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "LAST_ADMIN");
        assert_eq!(body["error"]["message_key"], "identity.last_admin");

        let invalid_state = backend_error(&ControllerError::InvalidState);
        assert_eq!(invalid_state.status(), StatusCode::CONFLICT);
        let bytes = to_bytes(invalid_state.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "INVALID_STATE");
    }
}

//! HTTP security primitives for the controller auth endpoints (pure
//! functions, no I/O, no persistence).
//!
//! - Exact-allowlist `Host` / `Origin` checks: pre-session writes (login)
//!   require an allowed `Origin`; established-session writes tolerate a
//!   missing `Origin` (non-browser clients) but never relax the
//!   session-bound `X-CSRF-Token` check. A `CONTROLLER_ALLOWED_ORIGIN` of
//!   the whole-entry `*` relaxes only the `Origin` check — never the CSRF
//!   binding.
//! - Session-bound CSRF comparison is constant time (`subtle::ConstantTimeEq`,
//!   length checked first because the token is fixed 64-hex) and fails closed on
//!   an empty bound — see [`csrf_token_matches`].
//! - Session cookie header construction: the token must be exactly 64
//!   lowercase hex characters (header-injection safe); attribute set is
//!   fixed by spec 01 A-03 — `HttpOnly; SameSite=Strict; Path=/`, `Secure`
//!   only when `trust_tls` is configured.
//! - In-memory login rate limiter: two independent dimensions (account,
//!   source IP), 5 failures within a 15-minute window block for the
//!   remaining window; each dimension holds at most 4096 active keys and
//!   fails closed (`RateLimitError::Exhausted`) instead of evicting keys
//!   that are still in force.
//!
//! The 429/503 + `Retry-After` HTTP mapping is a handler concern (see the
//! auth HTTP plan); this module only provides the pure decision values.
//! Error messages never echo host, origin or token values.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;

use crate::error::ControllerError;

/// Session cookie name (spec 01 A-03).
pub const COOKIE_NAME: &str = "rsc_session";

/// Login-failure sliding window: 15 minutes.
pub const RATE_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Login failures allowed per (account) and per (source) inside the window.
pub const RATE_MAX_FAILURES: usize = 5;

/// Hard cap on active tracked keys per limiter dimension (fail closed).
pub const LOGIN_RATE_MAX_KEYS: usize = 4096;

/// Static security configuration, parsed from values (pure) or the
/// environment (thin wrapper around `from_values`).
#[derive(Clone, Debug)]
pub struct HttpSecurityConfig {
    /// Exact allowed `Host` header strings, including port.
    pub allowed_hosts: Vec<String>,
    /// Exact allowed `Origin` header string, or the literal `*` to admit any
    /// `Origin`; `None` only when the struct is built directly in tests —
    /// `from_values` always sets `Some`.
    pub allowed_origin: Option<String>,
    /// Append `Secure` to the session cookie only when explicitly trusted.
    pub trust_tls: bool,
}

impl HttpSecurityConfig {
    /// Pure constructor. `hosts` is the comma-separated exact `Host` list
    /// (`CONTROLLER_ALLOWED_HOSTS`), `origin` the exact `Origin` or the
    /// whole-entry `*` (`CONTROLLER_ALLOWED_ORIGIN`), `trust_tls` the
    /// `CONTROLLER_TRUST_TLS` value (`"1"`/`"true"`, default false).
    /// Missing/empty values and unrecognised `trust_tls` values are fixed
    /// `Config` errors that never echo the supplied values.
    pub fn from_values(
        hosts: Option<&str>,
        origin: Option<&str>,
        trust_tls: Option<&str>,
    ) -> Result<Self, ControllerError> {
        let hosts = hosts.filter(|h| !h.is_empty()).ok_or_else(|| {
            ControllerError::Config("CONTROLLER_ALLOWED_HOSTS is required".into())
        })?;
        let allowed_hosts: Vec<String> = hosts.split(',').map(str::to_string).collect();
        if allowed_hosts.iter().any(|h| h.is_empty()) {
            return Err(ControllerError::Config(
                "CONTROLLER_ALLOWED_HOSTS must not contain an empty entry".into(),
            ));
        }
        let origin = origin.filter(|o| !o.is_empty()).ok_or_else(|| {
            ControllerError::Config("CONTROLLER_ALLOWED_ORIGIN is required".into())
        })?;
        let trust_tls = match trust_tls {
            None => false,
            Some("1") | Some("true") => true,
            Some(_) => {
                return Err(ControllerError::Config(
                    "CONTROLLER_TRUST_TLS must be \"1\" or \"true\"".into(),
                ));
            }
        };
        Ok(Self {
            allowed_hosts,
            allowed_origin: Some(origin.to_string()),
            trust_tls,
        })
    }

    /// Production entry point: read the three environment variables and
    /// delegate to `from_values`. Never mutates the process environment.
    pub fn from_env() -> Result<Self, ControllerError> {
        let hosts = std::env::var("CONTROLLER_ALLOWED_HOSTS").ok();
        let origin = std::env::var("CONTROLLER_ALLOWED_ORIGIN").ok();
        let trust_tls = std::env::var("CONTROLLER_TRUST_TLS").ok();
        Self::from_values(hosts.as_deref(), origin.as_deref(), trust_tls.as_deref())
    }

    /// Host 白名单判定：逐字精确匹配，或白名单含整项 `*` 时放行任意 Host；Host 缺失恒拒。
    pub fn host_allowed(&self, host: Option<&str>) -> bool {
        host.is_some_and(|h| self.allowed_hosts.iter().any(|a| a == h || a == "*"))
    }

    /// pre-session 语义：配置恰为整项 `*` 时放开所有 Origin（含缺失）；否则要求
    /// Origin 存在且逐字精确匹配。`*.example`、`http://*` 等都不是通配。
    pub fn origin_allowed(&self, origin: Option<&str>) -> bool {
        match self.allowed_origin.as_deref() {
            Some("*") => true,
            allowed => origin.is_some() && allowed == origin,
        }
    }

    /// session 写语义：缺失 Origin 豁免（非浏览器客户端）；携带时必须逐字精确
    /// 匹配，或配置为整项 `*`。
    pub fn origin_allowed_if_present(&self, origin: Option<&str>) -> bool {
        match origin {
            None => true,
            Some(o) => {
                self.allowed_origin.as_deref() == Some("*")
                    || self.allowed_origin.as_deref() == Some(o)
            }
        }
    }
}

/// Which static check denied the write, for the HTTP layer's 403 mapping.
#[derive(Debug, PartialEq)]
pub enum WriteDeny {
    Host,
    Origin,
    Csrf,
}

/// Exact membership, or any host when the allowlist contains the literal `*`:
/// no case folding, no trimming, no pattern matching beyond the whole-entry `*`.
fn host_ok(host: Option<&str>, cfg: &HttpSecurityConfig) -> bool {
    cfg.host_allowed(host)
}

/// Pre-session (login) gate: exact allowed `Host` **and** allowed
/// `Origin`. A missing `Origin` is rejected — CLI logins must send the
/// allowed `Origin` explicitly. An `allowed_origin` of the literal `*`
/// admits any `Origin`, present or missing; nothing else is relaxed.
pub fn check_pre_session(
    host: Option<&str>,
    origin: Option<&str>,
    cfg: &HttpSecurityConfig,
) -> Result<(), WriteDeny> {
    if !host_ok(host, cfg) {
        return Err(WriteDeny::Host);
    }
    if !cfg.origin_allowed(origin) {
        return Err(WriteDeny::Origin);
    }
    Ok(())
}

/// Session-bound CSRF token 的**常量时间**比较。
///
/// - 缺失的 `X-CSRF-Token`（`None`）一律不匹配。
/// - **空 `bound` 一律不匹配**（fail-closed）：生产 bound 恒为 64 位小写 hex，但本
///   helper 是公开入口，绝不允许「bound 为空 + provided 为空」被判成匹配——那等于
///   会话没绑到任何 token 时任何请求都能通过。
/// - 长度不等直接判否：token 是固定 64 位小写 hex，长度不是秘密，提前返回不会
///   泄露任何秘密字节的内容；反过来若先做 `ct_eq`，`subtle` 对不等长切片也只保证
///   按较短长度比较，必须自己先挡掉长度差。
/// - 长度相等时整体走 `subtle::ConstantTimeEq`，不逐字节短路（等价于
///   `src/metrics/auth.rs` 的口令比较口径）。
pub fn csrf_token_matches(provided: Option<&str>, bound: &str) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    // 空 bound 视为「该会话没有绑定任何 token」：只能拒，不能与空 token 相等放行。
    if bound.is_empty() || provided.len() != bound.len() {
        return false;
    }
    let equal: bool = provided.as_bytes().ct_eq(bound.as_bytes()).into();
    equal
}

/// Established-session write gate: exact allowed `Host`; a present
/// `Origin` must match exactly, or the `allowed_origin` must be the
/// literal `*`; the `csrf` header must equal the token bound to the
/// session. A missing `Origin` exempts only the `Origin`
/// check — the bound CSRF check still applies. An empty `bound` never
/// matches (see [`csrf_token_matches`]), so a session without a bound
/// token is always denied with `WriteDeny::Csrf`.
pub fn check_session_write(
    host: Option<&str>,
    origin: Option<&str>,
    csrf: Option<&str>,
    bound: &str,
    cfg: &HttpSecurityConfig,
) -> Result<(), WriteDeny> {
    if !host_ok(host, cfg) {
        return Err(WriteDeny::Host);
    }
    // A present Origin must match exactly (unless the whole entry is `*`); a
    // missing Origin only exempts this check, never the CSRF binding below.
    if !cfg.origin_allowed_if_present(origin) {
        return Err(WriteDeny::Origin);
    }
    if !csrf_token_matches(csrf, bound) {
        return Err(WriteDeny::Csrf);
    }
    Ok(())
}

/// Build the `Set-Cookie` header for a session token. The token must be
/// exactly 64 lowercase hex characters; anything else is a fixed
/// `Config` error (no echo of the token) before any header text is built.
pub fn session_cookie_header(
    token: &str,
    cfg: &HttpSecurityConfig,
) -> Result<String, ControllerError> {
    if token.len() != 64
        || !token
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(ControllerError::Config("invalid session token".into()));
    }
    Ok(format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=43200{}",
        if cfg.trust_tls { "; Secure" } else { "" }
    ))
}

/// `Set-Cookie` header that expires the session cookie.
pub fn clear_session_cookie_header() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

/// Limiter overflow: a dimension is at its 4096 active-key cap and the
/// requested key is new. The caller maps this to a fixed 503
/// `RESOURCE_EXHAUSTED` + `Retry-After: 60` and must reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateLimitError {
    Exhausted,
}

/// In-memory login failure limiter: (account) and (source IP) dimensions,
/// each a plain map of in-window failure timestamps. The owning handler
/// serialises access with an outer `Mutex`; the maps are not locked
/// internally. No persistence — a restart clears the limiter, which is
/// spec-acceptable (it must not permanently lock anyone out).
#[derive(Default)]
pub struct LoginRateLimiter {
    by_account: HashMap<String, VecDeque<Instant>>,
    by_source: HashMap<IpAddr, VecDeque<Instant>>,
}

impl LoginRateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop timestamps whose window has fully elapsed and drop the keys
    /// left empty, so expired accounts/sources free their key slots.
    fn prune(&mut self, now: Instant) {
        for dq in self.by_account.values_mut() {
            while dq.front().is_some_and(|t| *t + RATE_WINDOW <= now) {
                dq.pop_front();
            }
        }
        self.by_account.retain(|_, dq| !dq.is_empty());
        for dq in self.by_source.values_mut() {
            while dq.front().is_some_and(|t| *t + RATE_WINDOW <= now) {
                dq.pop_front();
            }
        }
        self.by_source.retain(|_, dq| !dq.is_empty());
    }

    /// Record one failed login attempt. Fails closed with `Exhausted` when
    /// a new key would exceed a dimension's 4096 active-key cap; never
    /// evicts an in-force key to make room. Each key keeps at most
    /// `RATE_MAX_FAILURES` (the most recent) timestamps.
    pub fn record_failure(
        &mut self,
        now: Instant,
        account: &str,
        source: IpAddr,
    ) -> Result<(), RateLimitError> {
        self.prune(now);
        if self.by_account.len() >= LOGIN_RATE_MAX_KEYS && !self.by_account.contains_key(account) {
            return Err(RateLimitError::Exhausted);
        }
        if self.by_source.len() >= LOGIN_RATE_MAX_KEYS && !self.by_source.contains_key(&source) {
            return Err(RateLimitError::Exhausted);
        }
        for dq in [
            self.by_account.entry(account.to_string()).or_default(),
            self.by_source.entry(source).or_default(),
        ] {
            dq.push_back(now);
            while dq.len() > RATE_MAX_FAILURES {
                dq.pop_front();
            }
        }
        Ok(())
    }

    /// How long the caller must wait before a login attempt for this
    /// (account, source) pair, or `None` when no dimension is currently
    /// blocked. Expired entries are pruned first. When both dimensions
    /// block, the longer remaining duration wins. `0 < d <= RATE_WINDOW`.
    /// A new key at a full dimension fails closed with `Exhausted`.
    pub fn retry_after(
        &mut self,
        now: Instant,
        account: &str,
        source: IpAddr,
    ) -> Result<Option<Duration>, RateLimitError> {
        self.prune(now);
        if !self.by_account.contains_key(account) && self.by_account.len() >= LOGIN_RATE_MAX_KEYS {
            return Err(RateLimitError::Exhausted);
        }
        if !self.by_source.contains_key(&source) && self.by_source.len() >= LOGIN_RATE_MAX_KEYS {
            return Err(RateLimitError::Exhausted);
        }
        let remaining = |dq: Option<&VecDeque<Instant>>| -> Option<Duration> {
            dq.filter(|dq| dq.len() >= RATE_MAX_FAILURES)
                .and_then(|dq| dq.front().map(|t| *t + RATE_WINDOW - now))
        };
        let by_account = remaining(self.by_account.get(account));
        let by_source = remaining(self.by_source.get(&source));
        Ok(by_account.max(by_source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HttpSecurityConfig {
        HttpSecurityConfig {
            allowed_hosts: vec!["127.0.0.1:8080".into()],
            allowed_origin: Some("http://127.0.0.1:5173".into()),
            trust_tls: false,
        }
    }

    fn tls_cfg() -> HttpSecurityConfig {
        let mut c = cfg();
        c.trust_tls = true;
        c
    }

    #[test]
    fn pre_session_requires_exact_host_and_origin() {
        let c = cfg();
        // Both exact → allowed.
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8080"), Some("http://127.0.0.1:5173"), &c),
            Ok(())
        );
        // Wrong or missing host → Host.
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8081"), Some("http://127.0.0.1:5173"), &c),
            Err(WriteDeny::Host)
        );
        assert_eq!(
            check_pre_session(None, Some("http://127.0.0.1:5173"), &c),
            Err(WriteDeny::Host)
        );
        // Missing origin (initial CLI login) → Origin.
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8080"), None, &c),
            Err(WriteDeny::Origin)
        );
        // Origin comparison is exact and case-sensitive.
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8080"), Some("HTTP://127.0.0.1:5173"), &c),
            Err(WriteDeny::Origin)
        );
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8080"), Some("http://127.0.0.1:5174"), &c),
            Err(WriteDeny::Origin)
        );
    }

    #[test]
    fn session_write_requires_bound_csrf() {
        let c = cfg();
        let bound = "bound-csrf-token-5a5a";
        // No Origin (non-browser client) + bound CSRF + allowed host → allowed.
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some(bound), bound, &c),
            Ok(())
        );
        // Missing Origin does NOT exempt the CSRF binding.
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, None, bound, &c),
            Err(WriteDeny::Csrf)
        );
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some("wrong"), bound, &c),
            Err(WriteDeny::Csrf)
        );
        // Wrong Origin → Origin even with a correct CSRF token.
        assert_eq!(
            check_session_write(
                Some("127.0.0.1:8080"),
                Some("http://evil.example"),
                Some(bound),
                bound,
                &c
            ),
            Err(WriteDeny::Origin)
        );
        // Host not in the allowlist → Host.
        assert_eq!(
            check_session_write(
                Some("example.org"),
                Some("http://127.0.0.1:5173"),
                Some(bound),
                bound,
                &c
            ),
            Err(WriteDeny::Host)
        );
    }

    #[test]
    fn wildcard_host_entry_admits_any_present_host() {
        // 白名单含整项 `*`：任意 Host 头通过 Host 检查。
        let mut c = cfg();
        c.allowed_hosts = vec!["*".into()];
        // Origin 仍必须精确匹配：合法 origin 才能整体通过。
        assert_eq!(
            check_pre_session(Some("evil.example:443"), Some("http://127.0.0.1:5173"), &c),
            Ok(())
        );
        // Origin 不合法仍被 Origin 拒绝，`*` 只放开 Host。
        assert_eq!(
            check_pre_session(Some("evil.example:443"), Some("http://evil.example"), &c),
            Err(WriteDeny::Origin)
        );
        // 缺失 Host 不被通配放行（请求线永远带 Host，这里是纵深防御）。
        assert_eq!(
            check_pre_session(None, Some("http://127.0.0.1:5173"), &c),
            Err(WriteDeny::Host)
        );
        assert!(!host_ok(None, &c));
    }

    #[test]
    fn wildcard_host_entry_admits_any_host() {
        // 直接对 host_allowed 方法断言：这是所有鉴权第一道共用的唯一判定入口。
        let mut c = cfg();
        c.allowed_hosts = vec!["*".into()];
        // 整项 `*`：任意 Host 放行，Host 值本身是 `*` 也按命中通配处理。
        assert!(c.host_allowed(Some("evil.example:443")));
        assert!(c.host_allowed(Some("127.0.0.1:8080")));
        assert!(c.host_allowed(Some("*")));
        // Host 缺失恒拒，与是否配通配无关。
        assert!(!c.host_allowed(None));

        // 非 `*` 精确列表：逐字命中才放行，缺失恒拒。
        let c = cfg();
        assert!(c.host_allowed(Some("127.0.0.1:8080")));
        assert!(!c.host_allowed(Some("127.0.0.1:8081")));
        assert!(!c.host_allowed(Some("*")));
        assert!(!c.host_allowed(None));
    }

    #[test]
    fn wildcard_entry_beside_exact_entries_admits_any_host() {
        // 精确项与 `*` 并存时，`*` 生效：任意 host 通过。
        let mut c = cfg();
        c.allowed_hosts = vec!["127.0.0.1:8080".into(), "*".into()];
        let bound = "bound-csrf-token-5a5a";
        assert_eq!(
            check_session_write(
                Some("evil.example"),
                Some("http://127.0.0.1:5173"),
                Some(bound),
                bound,
                &c
            ),
            Ok(())
        );
        assert_eq!(
            check_session_write(Some("anything.invalid:9"), None, Some(bound), bound, &c),
            Ok(())
        );
    }

    #[test]
    fn without_wildcard_entry_mismatched_host_still_denied() {
        // 防回归：白名单不含 `*` 时，Host 仍是逐字精确匹配。
        let c = cfg();
        let bound = "bound-csrf-token-5a5a";
        assert_eq!(
            check_session_write(
                Some("evil.example"),
                Some("http://127.0.0.1:5173"),
                Some(bound),
                bound,
                &c
            ),
            Err(WriteDeny::Host)
        );
        // `*` 只按整项匹配，不是子串/模式：host 值里带 `*` 也不特殊。
        assert!(!host_ok(Some("*"), &c));
        let mut c = cfg();
        c.allowed_hosts = vec!["*.example".into()];
        assert!(!host_ok(Some("a.example"), &c));
    }

    #[test]
    fn wildcard_origin_entry_admits_any_origin() {
        // `CONTROLLER_ALLOWED_ORIGIN=*`：任意 Origin（含缺失）通过 Origin 检查。
        let mut c = cfg();
        c.allowed_origin = Some("*".into());
        // pre-session：Origin 缺失也放行（Host 仍须合法）。
        assert_eq!(check_pre_session(Some("127.0.0.1:8080"), None, &c), Ok(()));
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8080"), Some("https://evil.example"), &c),
            Ok(())
        );
        // `*` 只放开 Origin：Host 不合法仍是 Host 拒绝。
        assert_eq!(
            check_pre_session(Some("evil.example"), Some("https://evil.example"), &c),
            Err(WriteDeny::Host)
        );
        // session 写：任意 Origin 与缺失 Origin 均通过，CSRF 仍用合法 bound。
        let bound = "bound-csrf-token-5a5a";
        assert_eq!(
            check_session_write(
                Some("127.0.0.1:8080"),
                Some("https://evil.example"),
                Some(bound),
                bound,
                &c
            ),
            Ok(())
        );
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some(bound), bound, &c),
            Ok(())
        );
        // `*` 不放行 CSRF：bound 不符仍是 Csrf 拒绝。
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some("wrong"), bound, &c),
            Err(WriteDeny::Csrf)
        );
    }

    #[test]
    fn origin_without_wildcard_still_requires_exact_match() {
        // 防回归：非 `*` 配置的三条既有语义一字不改。
        let c = cfg();
        let bound = "bound-csrf-token-5a5a";
        // 携带但不逐字相等 → Origin（pre-session 与 session 写都是）。
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8080"), Some("http://evil.example"), &c),
            Err(WriteDeny::Origin)
        );
        assert_eq!(
            check_session_write(
                Some("127.0.0.1:8080"),
                Some("http://evil.example"),
                Some(bound),
                bound,
                &c
            ),
            Err(WriteDeny::Origin)
        );
        // pre-session 缺失 Origin → 拒绝；session 写缺失 Origin 仍豁免。
        assert_eq!(
            check_pre_session(Some("127.0.0.1:8080"), None, &c),
            Err(WriteDeny::Origin)
        );
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some(bound), bound, &c),
            Ok(())
        );
    }

    #[test]
    fn partial_wildcard_forms_are_not_wildcards() {
        // 只有整项 `*` 是通配：`http://*`、`*.example` 都是普通精确值。
        for allowed in ["http://*", "*.example"] {
            let mut c = cfg();
            c.allowed_origin = Some(allowed.into());
            assert!(!c.origin_allowed(Some("http://evil")));
            assert!(!c.origin_allowed(Some("https://a.example")));
            // 缺失 Origin 在 pre-session 仍拒绝（`*` 才豁免）。
            assert!(!c.origin_allowed(None));
            // session 写：缺失仍豁免，携带必须逐字相等。
            assert!(c.origin_allowed_if_present(None));
            assert!(!c.origin_allowed_if_present(Some("http://evil")));
            assert!(!c.origin_allowed_if_present(Some("https://a.example")));
            // 逐字相等才放行。
            assert!(c.origin_allowed(Some(allowed)));
            assert!(c.origin_allowed_if_present(Some(allowed)));
        }
        // `*` 是合法配置值：from_values 原样接受。
        let c = HttpSecurityConfig::from_values(Some("127.0.0.1:8080"), Some("*"), None)
            .expect("`*` must be a valid CONTROLLER_ALLOWED_ORIGIN");
        assert_eq!(c.allowed_origin.as_deref(), Some("*"));
    }

    #[test]
    fn csrf_token_matches_length_then_constant_time() {
        // 与生产会话 token 同形：64 位小写 hex。
        let bound = hex::encode([0x5a_u8; 32]);
        assert_eq!(bound.len(), 64);

        // 等长且逐字节相同 → 匹配。
        assert!(csrf_token_matches(Some(&bound), &bound));
        let equal = bound.clone();
        assert!(csrf_token_matches(Some(&equal), &bound));

        // 等长但只有最后一个字节不同 → 不匹配（短路比较最危险的一格）。
        let mut last_diff = bound.clone();
        last_diff.replace_range(bound.len() - 1.., "0");
        assert_eq!(last_diff.len(), bound.len());
        assert!(!csrf_token_matches(Some(&last_diff), &bound));

        // 等长但首个字节不同 → 不匹配。
        let mut first_diff = bound.clone();
        first_diff.replace_range(..1, "0");
        assert!(!csrf_token_matches(Some(&first_diff), &bound));

        // 长度不同（前缀、空串、多一字节）→ 一律不匹配。
        assert!(!csrf_token_matches(Some(&bound[..63]), &bound));
        assert!(!csrf_token_matches(Some(""), &bound));
        assert!(!csrf_token_matches(Some(&format!("{bound}0")), &bound));
        // 缺失的 token 永远不匹配，即便 bound 本身是空串。
        assert!(!csrf_token_matches(None, &bound));
        assert!(!csrf_token_matches(None, ""));
        // 空 bound fail-closed：公开 helper 不接受「两边都是空串」这种平凡相等。
        // 生产 bound 恒 64 位小写 hex，这里钉的是 helper 自身的契约。
        assert!(!csrf_token_matches(Some(""), ""));
        assert!(!csrf_token_matches(Some("0"), ""));
        assert!(!csrf_token_matches(Some("anything"), ""));

        // 接线：check_session_write 的既有语义不变——等长的错误 token 仍是 Csrf。
        let c = cfg();
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some(&last_diff), &bound, &c),
            Err(WriteDeny::Csrf)
        );
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some(&bound), &bound, &c),
            Ok(())
        );
        // 接线：空 bound（会话没绑 token）一律 Csrf，绝不因空 token 相等而放行。
        assert_eq!(
            check_session_write(Some("127.0.0.1:8080"), None, Some(""), "", &c),
            Err(WriteDeny::Csrf)
        );
    }

    /// 去掉行注释后的纯代码文本（本文件的注释会解释「不许写什么」，直接扫原文会误判）。
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 源码级回归门禁：CSRF 比较必须落在 `subtle::ConstantTimeEq` 上，且不得退回可
    /// 短路的相等/前缀比较。
    ///
    /// 诚实界定：本用例只防**实现回退**（把 `ct_eq` 换回 `!=`/`starts_with` 时测试变
    /// 红），不构成「机器码恒定时间」的证明——那需要按 `subtle` 的实现与编译选项另行
    /// 审计。目标串一律用 `concat!` 拆分：编译后仍是逐字相同的目标串，但 `include_str!`
    /// 到的本测试自身不会成为自己 grep 的命中项（否则正向断言会被自指恒真满足）。
    #[test]
    fn csrf_comparison_uses_subtle_constant_time_eq() {
        let code = code_only(include_str!("http_security.rs"));
        assert!(
            code.contains(concat!("use subtle::", "ConstantTimeEq")),
            "CSRF 比较必须由 subtle::ConstantTimeEq 实现（去掉注释后的代码里必须 import）"
        );
        assert!(
            code.contains(concat!(".", "ct_eq(")),
            "csrf_token_matches 必须调用 ct_eq，不得改回逐字节/整体短路比较"
        );
        for forbidden in [
            concat!("csrf != ", "Some(bound)"),
            concat!("csrf == ", "Some(bound)"),
            concat!("provided != ", "bound"),
            concat!("provided == ", "bound"),
            concat!("starts_with(", "bound"),
        ] {
            assert!(
                !code.contains(forbidden),
                "MUST NOT 用可短路的比较判定 CSRF：{forbidden:?}"
            );
        }
    }

    #[test]
    fn cookie_headers() {
        let token = hex::encode([0x5a_u8; 32]);
        assert_eq!(token.len(), 64);
        // Valid lowercase 64-hex token, trust_tls=false.
        let header = session_cookie_header(&token, &cfg())
            .expect("valid lowercase 64-hex token must be accepted");
        for part in [
            "rsc_session=",
            "Path=/",
            "HttpOnly",
            "SameSite=Strict",
            "Max-Age=43200",
        ] {
            assert!(
                header.contains(part),
                "cookie header must contain {part:?}, got: {header}"
            );
        }
        assert!(
            header.contains(&token),
            "cookie header must carry the token value"
        );
        assert!(
            !header.contains("Secure"),
            "HTTP-direct cookie must not set Secure, got: {header}"
        );
        // trust_tls=true appends Secure.
        let secure = session_cookie_header(&token, &tls_cfg())
            .expect("valid token with trust_tls must be accepted");
        assert!(secure.contains("Secure"), "got: {secure}");
        // Clearing cookie expires it.
        let clear = clear_session_cookie_header();
        for part in [
            "rsc_session=",
            "Path=/",
            "HttpOnly",
            "SameSite=Strict",
            "Max-Age=0",
        ] {
            assert!(
                clear.contains(part),
                "clear cookie must contain {part:?}, got: {clear}"
            );
        }
        // Invalid tokens: CR/LF injection, wrong length, uppercase, non-hex
        // — fixed Config rejection, and the error must never echo the token.
        let bad = [
            format!("{}{}", &token[..62], "\r\n"),
            token[..63].to_string(),
            format!("{token}ab"),
            token.to_uppercase(),
            format!("{}x", &token[..63]),
        ];
        for bad_token in &bad {
            let err = session_cookie_header(bad_token, &cfg())
                .expect_err("invalid token must be rejected");
            assert!(
                matches!(err, ControllerError::Config(_)),
                "invalid token must be a fixed Config error"
            );
            assert!(
                !err.to_string().contains(bad_token.as_str()),
                "error must not echo the offending token"
            );
        }
    }

    #[test]
    fn rate_limiter_five_failures_in_window() {
        let mut rl = LoginRateLimiter::new();
        let t0 = Instant::now();
        let ip = "10.0.0.1".parse::<IpAddr>().unwrap();
        for i in 0..5 {
            rl.record_failure(t0 + Duration::from_secs(i), "admin", ip)
                .expect("within capacity");
        }
        // Same account, in window: blocked; remaining = oldest failure
        // (t0) + window - now = 900 - 30.
        assert_eq!(
            rl.retry_after(t0 + Duration::from_secs(30), "admin", ip)
                .unwrap(),
            Some(Duration::from_secs(870))
        );
        // Same source, different account: also blocked (source dimension).
        assert!(
            rl.retry_after(t0 + Duration::from_secs(30), "other", ip)
                .unwrap()
                .is_some(),
            "source dimension must block independently of account"
        );
        // After the window the failures expire and the pair is released.
        assert_eq!(
            rl.retry_after(t0 + RATE_WINDOW + Duration::from_secs(1), "admin", ip)
                .unwrap(),
            None
        );
    }

    #[test]
    fn config_from_values() {
        // Missing hosts / missing origin → fixed Config error, no value echo.
        let err = HttpSecurityConfig::from_values(None, Some("http://127.0.0.1:5173"), None)
            .expect_err("missing hosts must be rejected");
        assert!(matches!(err, ControllerError::Config(_)));
        assert!(!err.to_string().contains("http://127.0.0.1:5173"));
        let err = HttpSecurityConfig::from_values(Some("127.0.0.1:8080"), None, None)
            .expect_err("missing origin must be rejected");
        assert!(matches!(err, ControllerError::Config(_)));
        assert!(!err.to_string().contains("127.0.0.1:8080"));
        // Empty strings and empty list entries are rejected the same way.
        assert!(
            HttpSecurityConfig::from_values(Some(""), Some("http://127.0.0.1:5173"), None).is_err()
        );
        assert!(HttpSecurityConfig::from_values(Some("127.0.0.1:8080"), Some(""), None).is_err());
        assert!(
            HttpSecurityConfig::from_values(
                Some("127.0.0.1:8080,,[::1]:8080"),
                Some("http://127.0.0.1:5173"),
                None
            )
            .is_err()
        );
        assert!(
            HttpSecurityConfig::from_values(
                Some("127.0.0.1:8080,"),
                Some("http://127.0.0.1:5173"),
                None
            )
            .is_err()
        );
        // Complete values, trust_tls unset → Ok with trust_tls=false.
        let c = HttpSecurityConfig::from_values(
            Some("127.0.0.1:8080"),
            Some("http://127.0.0.1:5173"),
            None,
        )
        .expect("complete values must parse");
        assert_eq!(c.allowed_hosts, vec!["127.0.0.1:8080".to_string()]);
        assert_eq!(c.allowed_origin.as_deref(), Some("http://127.0.0.1:5173"));
        assert!(!c.trust_tls);
        // Multiple exact hosts, in order.
        let c = HttpSecurityConfig::from_values(
            Some("127.0.0.1:8080,[::1]:8080"),
            Some("http://127.0.0.1:5173"),
            None,
        )
        .unwrap();
        assert_eq!(
            c.allowed_hosts,
            vec!["127.0.0.1:8080".to_string(), "[::1]:8080".to_string()]
        );
        // trust_tls: "1"/"true" → true; anything else is rejected (fail closed).
        let c = HttpSecurityConfig::from_values(
            Some("127.0.0.1:8080"),
            Some("http://127.0.0.1:5173"),
            Some("1"),
        )
        .unwrap();
        assert!(c.trust_tls);
        let c = HttpSecurityConfig::from_values(
            Some("127.0.0.1:8080"),
            Some("http://127.0.0.1:5173"),
            Some("true"),
        )
        .unwrap();
        assert!(c.trust_tls);
        for bad_value in ["0", "yes", "TRUE"] {
            assert!(
                HttpSecurityConfig::from_values(
                    Some("127.0.0.1:8080"),
                    Some("http://127.0.0.1:5173"),
                    Some(bad_value),
                )
                .is_err(),
                "trust_tls={bad_value:?} must be rejected"
            );
        }
    }

    #[test]
    fn limiter_fails_closed_at_key_capacity_and_recovers_after_expiry() {
        let mut rl = LoginRateLimiter::new();
        let t0 = Instant::now();
        let ip = "10.0.0.9".parse::<IpAddr>().unwrap();
        // Fill the account dimension with 4096 distinct accounts (one
        // source key only).
        for i in 0..LOGIN_RATE_MAX_KEYS {
            rl.record_failure(t0, &format!("acc-{i}"), ip)
                .expect("within capacity until the cap");
        }
        // 4097th new account: fail closed on both entry points.
        assert!(
            matches!(
                rl.record_failure(t0 + Duration::from_secs(1), "acc-overflow", ip),
                Err(RateLimitError::Exhausted)
            ),
            "new account at full capacity must fail closed"
        );
        assert!(
            matches!(
                rl.retry_after(t0 + Duration::from_secs(1), "acc-overflow", ip),
                Err(RateLimitError::Exhausted)
            ),
            "retry_after for a new key at full capacity must fail closed"
        );
        // Existing keys are unaffected and still block by window; here the
        // shared source (5 trimmed failures, oldest t0) is the blocking one.
        assert_eq!(
            rl.retry_after(t0 + Duration::from_secs(1), "acc-0", ip)
                .unwrap(),
            Some(RATE_WINDOW - Duration::from_secs(1))
        );
        // After the window, expired keys are reclaimed and a new account
        // can be recorded again.
        let t1 = t0 + RATE_WINDOW + Duration::from_secs(1);
        assert!(rl.record_failure(t1, "acc-overflow", ip).is_ok());
        assert_eq!(
            rl.retry_after(t1 + Duration::from_secs(1), "acc-overflow", ip)
                .unwrap(),
            None
        );

        // Both dimensions blocking at once: the LONGER remaining window wins.
        let mut rl2 = LoginRateLimiter::new();
        let ip2 = "10.0.0.7".parse::<IpAddr>().unwrap();
        let tb = Instant::now();
        for i in 0..5 {
            rl2.record_failure(tb + Duration::from_secs(i), "bob", ip2)
                .expect("within capacity");
        }
        // A different account shares the source with later failures, so
        // the source key's oldest in-window timestamp is later than
        // "bob"'s oldest.
        for i in 0..5 {
            rl2.record_failure(tb + Duration::from_secs(30 + i), "dave", ip2)
                .expect("within capacity");
        }
        let now = tb + Duration::from_secs(60);
        // Account "bob": oldest tb → 840s remaining.
        // Source ip2: oldest tb+30 → 870s remaining. Longer wins.
        assert_eq!(
            rl2.retry_after(now, "bob", ip2).unwrap(),
            Some(Duration::from_secs(870)),
            "when both dimensions block, the longer remaining window must win"
        );
    }
}

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::FromRef;

use super::CsrfBinding;
use super::contract::AuthServiceApi;
use super::rate_limit::{
    EndpointId, READ_RATE_LIMIT_PER_SEC, RateWindowLimiter, WRITE_RATE_LIMIT_PER_SEC,
};
use crate::db::DbPool;
use crate::http_security::{HttpSecurityConfig, LoginRateLimiter};

/// Handler state for the auth routes.
pub struct HttpAuthState {
    pub auth: Arc<dyn AuthServiceApi>,
    pub config: HttpSecurityConfig,
    /// Session digest -> CSRF binding, bounded to `MAX_CSRF_ENTRIES` (fail
    /// closed instead of evicting or overwriting live bindings). Slots whose
    /// session the process clock already reports dead are swept at
    /// `CSRF_PRUNE_THRESHOLD` — see `prune_dead_csrf`.
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

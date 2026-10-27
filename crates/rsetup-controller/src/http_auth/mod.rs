//! HTTP auth handlers (Task 3a: `login`; Task 3b: `me`; Task 3c: `password`
//! and `logout`; Task 3e: session-write and endpoint-read rate limiting;
//! Task 3f: pre-auth `me` clock gate).
//!
//! The auth router IS wired into production: `main.rs` is the one and only
//! construction point of the shared `HttpAuthState`, built on top of the
//! `LiveAuth` adapter (`AuthService` over `SqlxIdentityRepository`) and
//! cloned into every line's state, so the handlers in this module back the registered
//! `/auth/*` endpoints. See `crates/rsetup-controller/src/main.rs`.
//!
//! Contract: envelopes come from `crate::http_api`; fixed allowed
//! code/message_key with empty redacted `params`; credentials, tokens and
//! raw backend errors never reach responses or logs.

// Submodules
pub mod contract;
pub mod csrf;
pub mod login;
pub mod me;
pub mod password_logout;
pub mod rate_limit;
pub mod router;
pub mod sessions;
pub mod state;

#[cfg(test)]
mod tests_support;

pub use contract::{AuthFuture, AuthServiceApi, SessionAuthority};
pub use csrf::{CSRF_PRUNE_THRESHOLD, CsrfBinding, MAX_CSRF_ENTRIES};
pub use rate_limit::{
    EndpointId, MAX_RATE_LIMITER_KEYS, RATE_WINDOW_1S, READ_RATE_LIMIT_PER_SEC, RateWindowLimiter,
    WRITE_RATE_LIMIT_PER_SEC, WindowRateLimitError,
};
pub use router::build_http_router;
pub use state::{AppState, HttpAuthState};

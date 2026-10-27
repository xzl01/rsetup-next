use std::collections::HashMap;
use std::time::Instant;

use super::contract::AuthServiceApi;
use crate::auth::session::ABSOLUTE;

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

/// Prune expired CSRF bindings under an already-held lock: any binding
/// whose monotonic issue instant plus the absolute session lifetime
/// (`auth::session::ABSOLUTE`) has elapsed under the monotonic clock.
/// A binding whose issue instant lies AFTER `now` is anomalous (a
/// monotonic clock never moves backwards; only an adversarial construct
/// through the public `CsrfBinding` fields could do this) and is treated
/// as expired — fail closed, never a permanent resident of the map.
/// Called (through [`prune_csrf_table`]) at the login peek, the authoritative
/// `login_success` insert and the `me` bind; live bindings (and their issue
/// instants) are never touched.
fn prune_expired_csrf(csrf: &mut HashMap<[u8; 32], CsrfBinding>, now: Instant) -> usize {
    let before = csrf.len();
    csrf.retain(|_, binding| {
        binding.issued_at <= now && now.saturating_duration_since(binding.issued_at) < ABSOLUTE
    });
    before - csrf.len()
}

/// Soft threshold that arms the dead-binding sweep
/// ([`prune_dead_csrf`]): 3/4 of the hard cap. Below it the handlers never
/// pay the O(n) per-entry clock probe; at or above it one sweep runs per
/// request under the same lock that already guards the capacity check, so a
/// table poisoned by revoked sessions still self-heals long before the
/// 12-hour absolute lifetime would.
pub const CSRF_PRUNE_THRESHOLD: usize = MAX_CSRF_ENTRIES * 3 / 4;

/// Remove CSRF bindings whose session the current process session clock has
/// ALREADY declared dead, i.e. every digest for which the synchronous,
/// pure in-memory [`AuthServiceApi::is_live_session`] precheck returns
/// `false` (idle-expired, absolute-expired, removed from the clock by this
/// process's own logout, or belonging to a replaced epoch). Returns the
/// number of removed bindings.
///
/// Semantics boundary — read this before extending it:
/// - the only source of truth here is the in-memory clock. A binding whose
///   digest is unknown to that clock is reported `false` and IS removed:
///   `false` means "not live in this process", never "unknown, keep it".
///   This can never evict a usable binding, because `SessionClock::is_live`
///   (behind `is_live_session`) and `SessionClock::check` (inside the
///   DB-authoritative `authenticate`) evaluate the SAME deadline predicate
///   (`now < idle && now < absolute`, a missing entry is dead). A digest
///   reported `false` therefore also fails `authenticate` with the fixed 401
///   on every session-bound route, so its binding was already unreachable;
/// - a session revoked on the database (revoke-others, admin password reset
///   or deactivation) whose clock entry has NOT yet expired keeps its
///   binding here — this sweep is NOT database-authoritative cleanup and
///   must not be described as clearing every revoked session. Such a binding
///   is dropped by the existing path on that session's next request: the
///   pre-auth gate or the DB-authoritative `authenticate` returns the fixed
///   401 and that branch removes the digest's binding;
/// - live bindings and their issue instants are never touched, existing
///   bindings are never overwritten, and the hard `MAX_CSRF_ENTRIES` cap
///   stays enforced by the callers after this sweep returns.
fn prune_dead_csrf(csrf: &mut HashMap<[u8; 32], CsrfBinding>, auth: &dyn AuthServiceApi) -> usize {
    let before = csrf.len();
    // `is_live_session` is contractually synchronous and in-memory (no
    // async I/O, no DB round trip, no re-entry into `HttpAuthState`), so
    // probing it while holding the CSRF lock cannot deadlock or stall.
    csrf.retain(|digest, _binding| auth.is_live_session(digest));
    before - csrf.len()
}

/// Sweep the CSRF table under an already-held lock: first the monotonic
/// 12h prune, then — only once the table reaches [`CSRF_PRUNE_THRESHOLD`],
/// so ordinary traffic never pays the O(n) clock probe — the dead-session
/// sweep. Callers must still enforce `MAX_CSRF_ENTRIES` afterwards: a table
/// full of live bindings keeps failing closed, it is never evicted from.
pub(super) fn prune_csrf_table(
    csrf: &mut HashMap<[u8; 32], CsrfBinding>,
    auth: &dyn AuthServiceApi,
    now: Instant,
) {
    prune_expired_csrf(csrf, now);
    if csrf.len() >= CSRF_PRUNE_THRESHOLD {
        prune_dead_csrf(csrf, auth);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use axum::http::StatusCode;

    use super::super::build_http_router;
    use super::super::tests::{
        FakeAuth, FakeLogin, app_state, call, fake_token, me_request, valid_cookie, valid_request,
    };
    use super::*;
    use crate::auth::session::ABSOLUTE;

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
        // The scenario models a LIVE (not yet expired) binding, so the fake
        // clock has to agree; otherwise this digest would just be another
        // dead slot for the threshold sweep.
        auth.set_digest_live(live_digest, true);
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

    // ---------------- M3: dead-binding sweep (threshold, live protection) ----------------

    /// `prune_dead_csrf` deletes exactly the bindings the in-memory clock
    /// reports as not live — an explicitly revoked/expired one AND an
    /// unknown digest (never "unknown, keep it") — while a live binding
    /// survives untouched, token and issue instant included.
    #[test]
    fn prune_dead_csrf_removes_only_dead_bindings() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let live = [1u8; 32];
        let revoked = [2u8; 32];
        let unknown = [3u8; 32];
        auth.set_digest_live(live, true);
        auth.set_digest_live(revoked, false);
        let now = Instant::now();
        let mut csrf: HashMap<[u8; 32], CsrfBinding> = HashMap::new();
        csrf.insert(
            live,
            CsrfBinding {
                token: "1".repeat(64),
                issued_at: now,
            },
        );
        csrf.insert(
            revoked,
            CsrfBinding {
                token: "2".repeat(64),
                issued_at: now,
            },
        );
        csrf.insert(
            unknown,
            CsrfBinding {
                token: "3".repeat(64),
                issued_at: now,
            },
        );
        let removed = prune_dead_csrf(&mut csrf, auth.as_ref());
        assert_eq!(
            removed, 2,
            "the revoked and the unknown binding must be reported as removed"
        );
        assert_eq!(csrf.len(), 1, "only the live binding may remain");
        let binding = csrf.get(&live).expect("a live binding must never be swept");
        assert_eq!(
            binding.token,
            "1".repeat(64),
            "a live binding is never overwritten"
        );
        assert_eq!(
            binding.issued_at, now,
            "a live binding keeps its original issue instant"
        );
        assert!(
            !csrf.contains_key(&revoked),
            "the clock-dead binding must be deleted"
        );
        assert!(
            !csrf.contains_key(&unknown),
            "`false` means dead, not unknown-keep"
        );
    }

    /// The poisoned-table scenario behind M3: every one of the 4096 slots is
    /// held by a binding whose session the clock already dropped, and the
    /// next login must NOT keep answering 503 (previously: until the 12h
    /// absolute lifetime expired). The sweep runs under the same lock as the
    /// capacity check, so the freed slots are visible to the insert.
    #[tokio::test]
    async fn csrf_table_full_of_dead_bindings_is_swept_and_login_succeeds() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let dead: Vec<[u8; 32]> = (0..MAX_CSRF_ENTRIES as u32)
            .map(|index| {
                let mut digest = [0u8; 32];
                digest[..4].copy_from_slice(&index.to_be_bytes());
                digest
            })
            .collect();
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            for digest in &dead {
                // Explicitly `false`: the clock dropped this session.
                auth.set_digest_live(*digest, false);
                csrf.insert(
                    *digest,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
            }
            assert_eq!(csrf.len(), MAX_CSRF_ENTRIES);
        }
        let router = build_http_router(state);
        let (status, _headers, value, text) = call(&router, valid_request()).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "a table of dead bindings must be swept, not answered 503: {text}"
        );
        let csrf_token = value["data"]["csrf_token"]
            .as_str()
            .expect("login must carry a csrf_token")
            .to_string();
        assert_eq!(csrf_token.len(), 64, "the token stays a 32-byte hex");
        let bound = auth_state.csrf.lock().unwrap();
        assert_eq!(
            bound.len(),
            1,
            "every dead slot must be reclaimed, leaving only the new binding"
        );
        let digest = crate::auth::session::token_digest(fake_token().as_bytes());
        assert_eq!(
            bound.get(&digest).map(|binding| binding.token.as_str()),
            Some(csrf_token.as_str()),
            "the new login must be bound to its own digest"
        );
        assert!(
            dead.iter().all(|digest| !bound.contains_key(digest)),
            "no swept digest may survive the login"
        );
    }

    /// Half live + half dead at the hard cap: `me` reclaims only the dead
    /// half, keeps every live binding (token and issue instant) and issues
    /// its own binding instead of failing closed.
    #[tokio::test]
    async fn me_sweeps_dead_bindings_at_cap_and_keeps_every_live_one() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let state = app_state(auth.clone());
        let auth_state = state.auth.clone();
        let caller = crate::auth::session::token_digest(fake_token().as_bytes());
        let half = MAX_CSRF_ENTRIES / 2;
        let mut live_digests = Vec::with_capacity(half);
        let mut dead_digests = Vec::with_capacity(half);
        {
            let mut csrf = auth_state.csrf.lock().unwrap();
            for index in 0..half as u32 {
                let mut digest = [0u8; 32];
                digest[..4].copy_from_slice(&index.to_be_bytes());
                auth.set_digest_live(digest, true);
                csrf.insert(
                    digest,
                    CsrfBinding {
                        token: "1".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
                live_digests.push(digest);
            }
            for index in 0..(MAX_CSRF_ENTRIES - half) as u32 {
                let mut digest = [0u8; 32];
                digest[..4].copy_from_slice(&index.to_be_bytes());
                // Disjoint keyspace from the live half.
                digest[4] = 0x7f;
                auth.set_digest_live(digest, false);
                csrf.insert(
                    digest,
                    CsrfBinding {
                        token: "0".repeat(64),
                        issued_at: Instant::now(),
                    },
                );
                dead_digests.push(digest);
            }
            assert_eq!(csrf.len(), MAX_CSRF_ENTRIES);
        }
        let router = build_http_router(state);
        let (status, _headers, value, text) =
            call(&router, me_request(Some(&valid_cookie()))).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "the dead half must free room for the caller: {text}"
        );
        let csrf_token = value["data"]["csrf_token"]
            .as_str()
            .expect("me must carry a csrf_token")
            .to_string();
        let bound = auth_state.csrf.lock().unwrap();
        assert_eq!(
            bound.get(&caller).map(|binding| binding.token.as_str()),
            Some(csrf_token.as_str()),
            "me must bind its own digest"
        );
        assert_eq!(bound.len(), half + 1, "only the dead half may be reclaimed");
        for digest in &live_digests {
            let binding = bound
                .get(digest)
                .expect("a live binding must survive the sweep");
            assert_eq!(binding.token, "1".repeat(64), "live token intact");
        }
        assert!(
            dead_digests
                .iter()
                .all(|digest| !bound.contains_key(digest)),
            "every dead binding must be gone"
        );
    }

    /// The sweep is armed only at the soft threshold, never below it, so an
    /// ordinary request never pays the O(n) clock probe: one dead slot under
    /// the threshold stays put, and the same table at the threshold is
    /// reclaimed in full while the same call under the threshold is a no-op.
    #[test]
    fn prune_csrf_table_sweeps_only_at_the_threshold() {
        let auth = FakeAuth::new(FakeLogin::Ok);
        let now = Instant::now();
        let below = CSRF_PRUNE_THRESHOLD - 1;
        let mut csrf: HashMap<[u8; 32], CsrfBinding> = HashMap::new();
        for index in 0..below as u32 {
            let mut digest = [0u8; 32];
            digest[0] = 0xaa;
            digest[1..5].copy_from_slice(&index.to_be_bytes());
            auth.set_digest_live(digest, false);
            csrf.insert(
                digest,
                CsrfBinding {
                    token: "0".repeat(64),
                    issued_at: now,
                },
            );
        }
        assert_eq!(csrf.len(), below);
        prune_csrf_table(&mut csrf, auth.as_ref(), now);
        assert_eq!(
            csrf.len(),
            below,
            "below the soft threshold the sweep must stay disarmed"
        );
        let extra = {
            let mut digest = [0u8; 32];
            digest[0] = 0xaa;
            digest[1..5].copy_from_slice(&(below as u32).to_be_bytes());
            auth.set_digest_live(digest, false);
            digest
        };
        csrf.insert(
            extra,
            CsrfBinding {
                token: "0".repeat(64),
                issued_at: now,
            },
        );
        assert_eq!(csrf.len(), CSRF_PRUNE_THRESHOLD);
        prune_csrf_table(&mut csrf, auth.as_ref(), now);
        assert!(
            csrf.is_empty(),
            "at the soft threshold every dead slot must be reclaimed"
        );
    }
}

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::ControllerError;

pub const IDLE: Duration = Duration::from_secs(30 * 60);
pub const ABSOLUTE: Duration = Duration::from_secs(12 * 60 * 60);

pub fn token_digest(raw: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    sha2::Sha256::digest(raw).into()
}

fn hmac_sha256(key: &[u8; 32], domain: &[u8], payload: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..32 {
        ipad[i] ^= key[i];
        opad[i] ^= key[i];
    }

    let mut inner = sha2::Sha256::new();
    inner.update(ipad);
    inner.update(domain);
    inner.update(payload);
    let inner_hash = inner.finalize();

    let mut outer = sha2::Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    outer.finalize().into()
}

fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

const ALIAS_DOMAIN: &[u8] = b"rsetup-session-id-v1";
const CURSOR_DOMAIN: &[u8] = b"rsetup-session-cursor-v1";

pub struct SessionAliasKey {
    secret: [u8; 32],
}

impl Default for SessionAliasKey {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionAliasKey {
    pub fn new() -> Self {
        use rand::RngCore;
        let mut secret = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        Self { secret }
    }

    #[cfg(test)]
    pub fn from_test_bytes(secret: [u8; 32]) -> Self {
        Self { secret }
    }

    pub fn alias(&self, digest: &[u8; 32]) -> String {
        let mac = hmac_sha256(&self.secret, ALIAS_DOMAIN, digest);
        hex::encode(mac)
    }

    pub fn matches(&self, digest: &[u8; 32], id: &str) -> bool {
        if id.len() != 64 {
            return false;
        }
        if !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return false;
        }
        let expected_mac = hmac_sha256(&self.secret, ALIAS_DOMAIN, digest);
        let mut actual_mac = [0u8; 32];
        if hex::decode_to_slice(id, &mut actual_mac).is_err() {
            return false;
        }
        constant_time_eq_32(&expected_mac, &actual_mac)
    }

    pub fn sign_cursor(&self, payload: &[u8]) -> [u8; 32] {
        hmac_sha256(&self.secret, CURSOR_DOMAIN, payload)
    }
}

const INSERT_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

struct SessionState {
    deadlines: HashMap<[u8; 32], (Instant, Instant)>,
    last_insert_sweep: Option<Instant>,
}

impl SessionState {
    fn prune_expired(&mut self, now: Instant) -> usize {
        let before = self.deadlines.len();
        self.deadlines
            .retain(|_, (idle, absolute)| now < *idle && now < *absolute);
        before - self.deadlines.len()
    }
}

pub struct SessionClock {
    pub epoch: [u8; 16],
    state: Mutex<SessionState>,
}
impl Default for SessionClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionClock {
    pub fn new() -> Self {
        Self {
            epoch: *uuid::Uuid::new_v4().as_bytes(),
            state: Mutex::new(SessionState {
                deadlines: HashMap::new(),
                last_insert_sweep: None,
            }),
        }
    }
    pub fn insert(&self, digest: [u8; 32], now: Instant) {
        let mut state = self.state.lock().unwrap();
        if state
            .last_insert_sweep
            .is_none_or(|last| now.saturating_duration_since(last) >= INSERT_SWEEP_INTERVAL)
        {
            state.prune_expired(now);
            state.last_insert_sweep = Some(now);
        }
        state.deadlines.insert(digest, (now + IDLE, now + ABSOLUTE));
    }
    /// Explicit maintenance scan; unlike insertion-triggered scans, this is never throttled
    /// and does not change the insertion sweep schedule.
    // Reserved for a future lifecycle-managed caller; the current phase has no timer.
    #[allow(dead_code)]
    pub(crate) fn prune_expired(&self, now: Instant) -> usize {
        self.state.lock().unwrap().prune_expired(now)
    }
    pub fn check(
        &self,
        digest: [u8; 32],
        epoch: &[u8; 16],
        now: Instant,
    ) -> Result<(), ControllerError> {
        if epoch != &self.epoch {
            return Err(ControllerError::InvalidArgument);
        }
        let mut state = self.state.lock().unwrap();
        let deadlines = &mut state.deadlines;
        let (idle, absolute) = deadlines
            .get_mut(&digest)
            .ok_or(ControllerError::InvalidArgument)?;
        if now >= *idle || now >= *absolute {
            deadlines.remove(&digest);
            return Err(ControllerError::InvalidArgument);
        }
        *idle = now
            .checked_add(IDLE)
            .ok_or(ControllerError::InvalidArgument)?;
        Ok(())
    }
    pub fn remove(&self, digest: &[u8; 32]) {
        self.state.lock().unwrap().deadlines.remove(digest);
    }
    pub fn is_live(&self, digest: &[u8; 32], now: Instant) -> bool {
        let state = self.state.lock().unwrap();
        match state.deadlines.get(digest) {
            Some(&(idle, absolute)) => now < idle && now < absolute,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn digest_is_one_way_and_distinct() {
        assert_ne!(token_digest(b"alpha"), [0; 32]);
        assert_ne!(token_digest(b"alpha"), token_digest(b"beta"));
    }
    #[test]
    fn session_rejects_restart_idle_and_absolute_expiry() {
        let clock = SessionClock::new();
        let now = Instant::now();
        let one = token_digest(b"one");
        clock.insert(one, now);
        assert!(clock.check(one, &[42; 16], now).is_err());
        assert!(
            clock
                .check(one, &clock.epoch, now + IDLE + Duration::from_secs(1))
                .is_err()
        );
        let two = token_digest(b"two");
        clock.insert(two, now);
        for n in 1..24 {
            assert!(
                clock
                    .check(two, &clock.epoch, now + Duration::from_secs(n * 1799))
                    .is_ok()
            );
        }
        assert!(clock.check(two, &clock.epoch, now + ABSOLUTE).is_err());
    }
    #[test]
    fn later_insert_prunes_untouched_expired_session_but_keeps_live_entries() {
        let clock = SessionClock::new();
        let start = Instant::now();
        let old = token_digest(b"expired-unvisited");
        let live = token_digest(b"still-live");
        let newest = token_digest(b"newest");
        clock.insert(old, start);
        clock.insert(live, start + Duration::from_secs(1));
        let sweep_at = start + IDLE;
        clock.insert(newest, sweep_at);

        let state = clock.state.lock().unwrap();
        let deadlines = &state.deadlines;
        assert!(
            !deadlines.contains_key(&old),
            "insert must reclaim untouched expiry"
        );
        assert!(deadlines.contains_key(&live));
        assert!(deadlines.contains_key(&newest));
        drop(state);
        assert!(clock.check(live, &clock.epoch, sweep_at).is_ok());
        assert!(clock.check(old, &clock.epoch, sweep_at).is_err());
    }
    #[test]
    fn insert_sweeps_at_most_once_per_sixty_seconds() {
        let clock = SessionClock::new();
        let start = Instant::now();
        let old = token_digest(b"old-for-throttle");
        clock.insert(old, start);
        let before_expiry = start + IDLE - Duration::from_nanos(1);
        clock.insert(token_digest(b"before-expiry"), before_expiry);
        clock.insert(token_digest(b"just-expired"), start + IDLE);
        assert!(clock.state.lock().unwrap().deadlines.contains_key(&old));
        clock.insert(
            token_digest(b"before-next-sweep"),
            before_expiry + Duration::from_secs(60) - Duration::from_nanos(1),
        );
        assert!(clock.state.lock().unwrap().deadlines.contains_key(&old));
        clock.insert(
            token_digest(b"next-sweep"),
            before_expiry + Duration::from_secs(60),
        );
        assert!(!clock.state.lock().unwrap().deadlines.contains_key(&old));
    }
    #[test]
    fn explicit_prune_honors_idle_nanosecond_boundary_and_keeps_insert_schedule() {
        let clock = SessionClock::new();
        let start = Instant::now();
        let old = token_digest(b"idle-boundary");
        clock.insert(old, start);
        assert_eq!(
            clock.prune_expired(start + IDLE - Duration::from_nanos(1)),
            0
        );
        assert!(clock.state.lock().unwrap().deadlines.contains_key(&old));
        assert_eq!(clock.prune_expired(start + IDLE), 1);
        assert_eq!(clock.prune_expired(start + IDLE), 0);
        assert_eq!(clock.state.lock().unwrap().last_insert_sweep, Some(start));
        clock.insert(token_digest(b"after-explicit"), start + IDLE);
        assert_eq!(
            clock.state.lock().unwrap().last_insert_sweep,
            Some(start + IDLE),
            "explicit prune must not postpone an insertion-triggered sweep"
        );
    }

    #[test]
    fn absolute_deadline_cannot_be_extended_by_idle_renewals() {
        let clock = SessionClock::new();
        let start = Instant::now();
        let renewed = token_digest(b"renewed-until-absolute");
        let live = token_digest(b"later-live");
        clock.insert(renewed, start);
        for n in 1..24 {
            assert!(
                clock
                    .check(renewed, &clock.epoch, start + Duration::from_secs(n * 1799))
                    .is_ok()
            );
        }
        assert!(
            clock
                .check(
                    renewed,
                    &clock.epoch,
                    start + ABSOLUTE - Duration::from_secs(1200),
                )
                .is_ok()
        );
        clock.insert(live, start + ABSOLUTE - Duration::from_secs(10));
        assert!(
            clock
                .check(
                    renewed,
                    &clock.epoch,
                    start + ABSOLUTE - Duration::from_nanos(1)
                )
                .is_ok()
        );
        assert_eq!(clock.prune_expired(start + ABSOLUTE), 1);
        let state = clock.state.lock().unwrap();
        assert!(!state.deadlines.contains_key(&renewed));
        assert!(state.deadlines.contains_key(&live));
        drop(state);
        assert!(clock.check(live, &clock.epoch, start + ABSOLUTE).is_ok());
    }

    #[test]
    fn remove_reinsert_and_renewal_are_serialized_with_prune() {
        let clock = SessionClock::new();
        let start = Instant::now();
        let digest = token_digest(b"reinserted");
        clock.insert(digest, start);
        clock.remove(&digest);
        let reinserted_at = start + IDLE - Duration::from_secs(1);
        clock.insert(digest, reinserted_at);
        assert_eq!(clock.prune_expired(start + IDLE), 0);
        assert!(clock.check(digest, &clock.epoch, start + IDLE).is_ok());
        assert_eq!(clock.prune_expired(reinserted_at + IDLE), 0);
        assert!(clock.state.lock().unwrap().deadlines.contains_key(&digest));
        assert_eq!(clock.prune_expired(start + 2 * IDLE), 1);
        assert!(clock.check(digest, &clock.epoch, start + 2 * IDLE).is_err());
        clock.insert(digest, start + 2 * IDLE);
        assert!(clock.check(digest, &clock.epoch, start + 2 * IDLE).is_ok());
        clock.remove(&digest);
        assert!(!clock.state.lock().unwrap().deadlines.contains_key(&digest));
    }

    #[test]
    fn alias_key_generates_stable_lowercase_hex_and_rejects_raw_digest_exposure() {
        let key = SessionAliasKey::from_test_bytes([7; 32]);
        let digest = token_digest(b"fixture-not-a-cookie");
        let alias = key.alias(&digest);
        assert_eq!(alias.len(), 64);
        assert!(
            alias
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "alias must be lowercase hex"
        );
        assert_ne!(
            alias,
            hex::encode(digest),
            "alias must never expose raw digest hex"
        );
        let same_alias = key.alias(&digest);
        assert_eq!(
            alias, same_alias,
            "same key and digest must yield identical alias"
        );

        let other_digest = token_digest(b"other-digest");
        let other_alias = key.alias(&other_digest);
        assert_ne!(
            alias, other_alias,
            "different digests must yield different aliases"
        );

        let other_key = SessionAliasKey::from_test_bytes([8; 32]);
        assert_ne!(
            other_key.alias(&digest),
            alias,
            "different process keys must yield different aliases"
        );
    }

    #[test]
    fn alias_matches_verifies_correct_alias_and_rejects_invalid_or_foreign_ids() {
        let key = SessionAliasKey::from_test_bytes([7; 32]);
        let digest = token_digest(b"fixture-not-a-cookie");
        let alias = key.alias(&digest);

        assert!(key.matches(&digest, &alias));
        assert!(!key.matches(&token_digest(b"other"), &alias));

        // Format checks: rejecting uppercase, malformed hex, wrong lengths
        let upper_alias = alias.to_ascii_uppercase();
        assert!(!key.matches(&digest, &upper_alias));
        assert!(!key.matches(&digest, ""));
        assert!(!key.matches(&digest, &alias[..63]));
        assert!(!key.matches(&digest, &format!("{alias}0")));
        let mut malformed = alias.clone();
        malformed.replace_range(0..1, "g");
        assert!(!key.matches(&digest, &malformed));

        // Foreign key
        let foreign_key = SessionAliasKey::from_test_bytes([8; 32]);
        assert!(!foreign_key.matches(&digest, &alias));
    }

    #[test]
    fn domain_separation_between_session_alias_and_cursor_signature() {
        let key = SessionAliasKey::from_test_bytes([7; 32]);
        let digest = token_digest(b"fixture-payload");
        let alias = key.alias(&digest);
        let cursor_mac = key.sign_cursor(&digest);
        assert_ne!(
            alias,
            hex::encode(cursor_mac),
            "cursor mac and session alias must have distinct domain separation"
        );
    }

    #[test]
    fn clock_is_live_peeks_without_extending_idle_and_honors_deadlines() {
        let clock = SessionClock::new();
        let t0 = Instant::now();
        let digest = token_digest(b"clock-peek-session");

        assert!(
            !clock.is_live(&digest, t0),
            "non-existent session cannot be live"
        );

        clock.insert(digest, t0);
        // Just before idle boundary
        assert!(clock.is_live(&digest, t0 + IDLE - Duration::from_nanos(1)));
        // Exactly at idle boundary (should be expired, not live)
        assert!(!clock.is_live(&digest, t0 + IDLE));
        // After idle boundary
        assert!(!clock.is_live(&digest, t0 + IDLE + Duration::from_secs(1)));

        // Verify peek did not extend idle time:
        // inserting another session at t0 + IDLE/2 and checking is_live doesn't renew
        let digest2 = token_digest(b"clock-peek-idle-not-renewed");
        clock.insert(digest2, t0);
        let mid = t0 + Duration::from_secs(15 * 60);
        assert!(clock.is_live(&digest2, mid));
        // If is_live renewed, it would be live at t0 + IDLE + Duration::from_secs(10).
        // Since is_live must NOT renew, at t0 + IDLE it must be expired:
        assert!(!clock.is_live(&digest2, t0 + IDLE));

        // Removal reflects immediately
        let digest3 = token_digest(b"clock-peek-removed");
        clock.insert(digest3, t0);
        assert!(clock.is_live(&digest3, t0));
        clock.remove(&digest3);
        assert!(!clock.is_live(&digest3, t0));
    }
}

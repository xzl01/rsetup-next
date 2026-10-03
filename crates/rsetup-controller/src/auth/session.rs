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
}

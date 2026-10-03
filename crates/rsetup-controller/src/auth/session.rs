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

pub struct SessionClock {
    pub epoch: [u8; 16],
    deadlines: Mutex<HashMap<[u8; 32], (Instant, Instant)>>,
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
            deadlines: Mutex::new(HashMap::new()),
        }
    }
    pub fn insert(&self, digest: [u8; 32], now: Instant) {
        self.deadlines
            .lock()
            .unwrap()
            .insert(digest, (now + IDLE, now + ABSOLUTE));
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
        let mut deadlines = self.deadlines.lock().unwrap();
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
        self.deadlines.lock().unwrap().remove(digest);
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
}

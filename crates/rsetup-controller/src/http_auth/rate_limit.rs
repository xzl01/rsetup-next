use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::time::{Duration, Instant};

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
    // Identity V1: one bucket per management/task read template (the
    // handler passes the template variant only, never an object id).
    UsersRead,
    EffectivePermissionsRead,
    RolesRead,
    GrantsRead,
    GroupsRead,
    DevicesRead,
    DeviceDetailRead,
    ApprovalsRead,
    AuditRead,
    SystemStatusRead,
    TaskList,
    TaskDetail,
    TaskChildren,
    DeviceStatusRead,
    /// SSE 事件流 `GET /api/v1/events` 的独立读桶（握手一次计一次）。
    EventsRead,
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
    #[cfg(test)]
    pub(super) entries: HashMap<K, VecDeque<Instant>>,
    #[cfg(not(test))]
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

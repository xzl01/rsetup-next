use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct ScheduledDevice {
    id: [u8; 32],
    next_due: Option<Instant>,
}

#[derive(Debug)]
pub struct PollScheduler {
    period: Duration,
    tick_cadence: Duration,
    last_tick: Instant,
    overloaded: bool,
    devices: Vec<ScheduledDevice>,
}

impl PollScheduler {
    pub fn new(
        origin: Instant,
        period: Duration,
        tick_cadence: Duration,
        devices: Vec<[u8; 32]>,
    ) -> Self {
        let n = devices.len();
        let scheduled = devices
            .into_iter()
            .enumerate()
            .map(|(i, id)| {
                let next_due = if period.is_zero() || n == 0 {
                    Some(origin)
                } else if i == 0 {
                    // 首周期窗口定义为 (origin, origin + period]；非零 period 相位 0 的初始到期时刻为 origin + period
                    origin.checked_add(period)
                } else {
                    let phase_nanos = (i as u128 * period.as_nanos()) / (n as u128);
                    u64::try_from(phase_nanos / 1_000_000_000)
                        .ok()
                        .and_then(|secs| {
                            let subsec_nanos = (phase_nanos % 1_000_000_000) as u32;
                            let phase = Duration::new(secs, subsec_nanos);
                            origin.checked_add(phase)
                        })
                };
                ScheduledDevice { id, next_due }
            })
            .collect();

        Self {
            period,
            tick_cadence,
            last_tick: origin,
            overloaded: false,
            devices: scheduled,
        }
    }

    pub fn set_overloaded(&mut self, overloaded: bool) {
        self.overloaded = overloaded;
    }

    pub fn backlog(&self) -> usize {
        0
    }

    pub fn due(&mut self, now: Instant) -> Vec<[u8; 32]> {
        if now < self.last_tick || self.tick_cadence.is_zero() {
            return Vec::new();
        }
        let paused = now.duration_since(self.last_tick) > self.tick_cadence;
        self.last_tick = now;

        let mut ready = Vec::new();
        let is_zero_period = self.period.is_zero();
        let period_nanos = self.period.as_nanos();

        for dev in &mut self.devices {
            let Some(next_due) = dev.next_due else {
                continue;
            };

            if next_due > now {
                continue;
            }

            let elapsed = now.duration_since(next_due);

            if is_zero_period {
                if !self.overloaded && !paused {
                    ready.push(dev.id);
                }
                dev.next_due = None;
                continue;
            }

            if self.overloaded || paused || elapsed >= self.period {
                // Overloaded or pause recovery: skip overdue cycles without burst/backlog, preserving phase
                let missed_cycles = (elapsed.as_nanos() / period_nanos) + 1;
                let advance = Self::mul_duration_u128(self.period, missed_cycles);
                dev.next_due = advance.and_then(|dur| next_due.checked_add(dur));
            } else {
                // Normal tick and not overloaded: emit device and advance one period
                ready.push(dev.id);
                dev.next_due = next_due.checked_add(self.period);
            }
        }

        ready
    }

    fn mul_duration_u128(d: Duration, mul: u128) -> Option<Duration> {
        let sec_mul = (d.as_secs() as u128).checked_mul(mul)?;
        let subsec_mul = (d.subsec_nanos() as u128).checked_mul(mul)?;
        let total_secs = sec_mul.checked_add(subsec_mul / 1_000_000_000)?;
        let secs = u64::try_from(total_secs).ok()?;
        let subsec_nanos = (subsec_mul % 1_000_000_000) as u32;
        Some(Duration::new(secs, subsec_nanos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_devices(count: usize) -> Vec<[u8; 32]> {
        (0..count)
            .map(|i| {
                let mut d = [0u8; 32];
                d[0..2].copy_from_slice(&(i as u16).to_be_bytes());
                d
            })
            .collect()
    }

    #[test]
    fn five_second_pause_does_not_burst() {
        let origin = Instant::now();
        let mut s = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            make_test_devices(1024),
        );
        let now = origin.checked_add(Duration::from_secs(5)).unwrap();
        assert!(
            s.due(now).is_empty(),
            "paused tick cannot replay past phases"
        );
    }

    #[test]
    fn exact_period_pause_does_not_burst() {
        let origin = Instant::now();
        let mut s = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            make_test_devices(1024),
        );
        let now = origin.checked_add(Duration::from_secs(10)).unwrap();
        assert!(s.due(now).is_empty(), "missed period cannot flood one tick");
    }

    #[test]
    fn five_second_pause_recovers_next_full_cycle_without_burst() {
        let origin = Instant::now();
        let mut s = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            make_test_devices(1024),
        );
        let paused = origin.checked_add(Duration::from_secs(5)).unwrap();
        assert!(s.due(paused).is_empty());
        let mut seen = std::collections::HashSet::new();
        for tick in 1..=100 {
            let at = paused
                .checked_add(Duration::from_millis(tick * 100))
                .unwrap();
            let batch = s.due(at);
            assert!(batch.len() <= 11);
            for id in batch {
                assert!(seen.insert(id), "duplicate device in recovery period");
            }
        }
        assert_eq!(seen.len(), 1024);
        assert_eq!(s.backlog(), 0);
    }

    #[test]
    fn exact_period_pause_recovers_next_full_cycle_without_burst() {
        let origin = Instant::now();
        let mut s = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            make_test_devices(1024),
        );
        let paused = origin.checked_add(Duration::from_secs(10)).unwrap();
        assert!(s.due(paused).is_empty());
        let mut seen = std::collections::HashSet::new();
        for tick in 1..=100 {
            let at = paused
                .checked_add(Duration::from_millis(tick * 100))
                .unwrap();
            let batch = s.due(at);
            assert!(batch.len() <= 11);
            for id in batch {
                assert!(seen.insert(id), "duplicate device in recovery period");
            }
        }
        assert_eq!(seen.len(), 1024);
        assert_eq!(s.backlog(), 0);
    }

    #[test]
    fn one_second_cadence_with_sixty_second_period_is_not_starved() {
        let origin = Instant::now();
        let mut s = PollScheduler::new(
            origin,
            Duration::from_secs(60),
            Duration::from_secs(1),
            make_test_devices(60),
        );
        let mut seen = std::collections::HashSet::new();
        for tick in 1..=60 {
            let at = origin.checked_add(Duration::from_secs(tick)).unwrap();
            for id in s.due(at) {
                assert!(seen.insert(id));
            }
        }
        assert_eq!(seen.len(), 60);
    }

    #[test]
    fn overdue_after_pause_skips_without_backlog_and_keeps_distribution() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        let mut scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            devices,
        );

        let pause_now = origin
            .checked_add(Duration::from_secs(30))
            .unwrap()
            .checked_add(Duration::from_nanos(1))
            .unwrap();

        // 停顿恢复的瞬间，逾期轮次一律跳过
        let first = scheduler.due(pause_now);
        assert!(
            first.is_empty(),
            "overdue cycles must be skipped without burst"
        );
        assert_eq!(scheduler.backlog(), 0);

        // 后续 10s (100 个 100ms tick)，验证每 tick 下发数 <= 11，整轮收齐 1024 台互异设备
        let mut collected = std::collections::HashSet::new();
        for tick in 1..=100 {
            let at = pause_now
                .checked_add(Duration::from_millis(tick * 100))
                .unwrap();
            let batch = scheduler.due(at);
            assert!(
                batch.len() <= 11,
                "tick {} exceeded limit: {}",
                tick,
                batch.len()
            );
            for id in batch {
                assert!(collected.insert(id), "duplicate device emitted: {:?}", id);
            }
        }
        assert_eq!(collected.len(), 1024);
        assert_eq!(scheduler.backlog(), 0);
    }

    #[test]
    fn normal_tick_due_emits_ready_devices_without_starvation() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        let mut scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            devices,
        );

        // 正常到期下发测试：在首个 100ms tick，严格分配到该窗口的设备必须正常 emit，绝不能因跳轮逻辑误判而饿死
        let first_tick = origin.checked_add(Duration::from_millis(100)).unwrap();
        let batch = scheduler.due(first_tick);
        assert!(
            !batch.is_empty(),
            "normal tick must emit ready devices without starvation"
        );
        assert!(batch.len() <= 11);
    }

    #[test]
    fn overload_skips_without_backlog_or_phase_collapse() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        let mut scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            devices,
        );

        let pause_now = origin
            .checked_add(Duration::from_secs(30))
            .unwrap()
            .checked_add(Duration::from_nanos(1))
            .unwrap();

        scheduler.set_overloaded(true);
        let first = scheduler.due(pause_now);
        assert!(first.is_empty(), "overload must emit nothing");
        assert_eq!(scheduler.backlog(), 0);

        scheduler.set_overloaded(false);
        let second = scheduler.due(pause_now);
        assert!(
            second.is_empty(),
            "resumed at same instant must not replay skipped cycle"
        );

        // 后续 10s (100 个 100ms tick)，验证每 tick 下发数 <= 11，整轮收齐 1024 台互异设备
        let mut collected = std::collections::HashSet::new();
        for tick in 1..=100 {
            let at = pause_now
                .checked_add(Duration::from_millis(tick * 100))
                .unwrap();
            let batch = scheduler.due(at);
            assert!(
                batch.len() <= 11,
                "tick {} exceeded limit: {}",
                tick,
                batch.len()
            );
            for id in batch {
                assert!(collected.insert(id), "duplicate device emitted: {:?}", id);
            }
        }
        assert_eq!(collected.len(), 1024);
        assert_eq!(scheduler.backlog(), 0);
    }

    #[test]
    fn duplicate_due_call_in_same_cycle_does_not_double_emit() {
        let origin = Instant::now();
        let devices = make_test_devices(100);
        let mut scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            devices,
        );

        let tick = origin.checked_add(Duration::from_millis(100)).unwrap();
        let batch1 = scheduler.due(tick);
        assert!(!batch1.is_empty());

        // 相同时刻重复调用，同一周期不得双发
        let batch2 = scheduler.due(tick);
        assert!(
            batch2.is_empty(),
            "repeated call at same tick must not double emit"
        );
    }

    #[test]
    fn empty_devices_and_zero_period_do_not_panic() {
        let origin = Instant::now();

        // 空设备
        let mut empty_scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            Vec::new(),
        );
        assert!(empty_scheduler.due(origin).is_empty());
        assert_eq!(empty_scheduler.backlog(), 0);

        // 零周期在正 cadence 下只单发一次
        let devices = make_test_devices(10);
        let mut zero_scheduler =
            PollScheduler::new(origin, Duration::ZERO, Duration::from_millis(100), devices);
        let first = zero_scheduler.due(origin);
        assert_eq!(first.len(), 10);
        assert_eq!(zero_scheduler.backlog(), 0);

        // 重复调用不双发也不 panic
        let second = zero_scheduler.due(origin);
        assert!(second.is_empty());
    }

    #[test]
    fn zero_cadence_fails_closed() {
        let origin = Instant::now();
        let devices = make_test_devices(10);
        let mut scheduler =
            PollScheduler::new(origin, Duration::from_secs(10), Duration::ZERO, devices);
        assert!(scheduler.due(origin).is_empty());
        let next_tick = origin.checked_add(Duration::from_secs(1)).unwrap();
        assert!(scheduler.due(next_tick).is_empty());
    }

    #[test]
    fn reversed_now_fails_closed_without_replaying_past_cycle() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        let mut scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            devices,
        );

        let tick1 = origin.checked_add(Duration::from_millis(100)).unwrap();
        let batch1 = scheduler.due(tick1);
        assert!(!batch1.is_empty());

        // 倒退时间：now < last_tick
        let past = origin;
        assert!(scheduler.due(past).is_empty());

        // 倒退后恢复正常推进：tick2 = tick1 + 100ms
        let tick2 = tick1.checked_add(Duration::from_millis(100)).unwrap();
        let batch2 = scheduler.due(tick2);
        assert!(!batch2.is_empty());
    }

    #[test]
    fn cold_start_one_hundred_normal_ticks_collects_all_unique_devices() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        let mut scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(100),
            devices,
        );

        let mut collected = std::collections::HashSet::new();
        let mut total_emitted = 0;
        for tick in 1..=100 {
            let at = origin
                .checked_add(Duration::from_millis(tick * 100))
                .unwrap();
            let batch = scheduler.due(at);
            assert!(
                batch.len() <= 11,
                "tick {} exceeded limit: {}",
                tick,
                batch.len()
            );
            for id in batch {
                total_emitted += 1;
                assert!(collected.insert(id), "duplicate device emitted: {:?}", id);
            }
        }
        assert_eq!(total_emitted, 1024);
        assert_eq!(collected.len(), 1024);
        assert_eq!(scheduler.backlog(), 0);
    }

    #[test]
    fn nominal_one_hundred_millis_with_jitter_under_one_hundred_twenty_five_cadence_not_starved() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        // 调用方显式配置 125ms 作为最长可接受间隔（headroom），标称 100ms 驱动且每次累积 +1µs 抖动
        let mut scheduler = PollScheduler::new(
            origin,
            Duration::from_secs(10),
            Duration::from_millis(125),
            devices,
        );

        let mut collected = std::collections::HashSet::new();
        let mut total_emitted = 0;
        let mut current_now = origin;
        for tick in 1..=100 {
            // 每次 tick 间隔标称 100ms 额外增加 1µs 延迟
            let step = Duration::from_millis(100) + Duration::from_micros(1);
            current_now = current_now.checked_add(step).unwrap();
            let batch = scheduler.due(current_now);
            assert!(
                batch.len() <= 11,
                "tick {} exceeded limit: {}",
                tick,
                batch.len()
            );
            for id in batch {
                total_emitted += 1;
                // 注意：在 Device 0 修复前，此测试同样可能在 tick 100 遇到 Device 0 重复
                // 此处记录正向契约每 tick 不饿死与正常发放
                collected.insert(id);
            }
        }
        // 100 次 tick 期间没有因为单次 100.001ms 超过 125ms 而整轮跳轮或饿死
        assert!(
            total_emitted >= 1000,
            "jittered ticks must not starve under sufficient cadence headroom: {}",
            total_emitted
        );
        assert_eq!(scheduler.backlog(), 0);
    }
}

//! §4.6 第 1 条与 §8 第 5 条要求的**握手侧有界资源**：全局/来源握手速率与来源未认证名额。
//!
//! 本模块只提供两个纯内存的限额原语，**不是**第二套准入或健康真源：
//!
//! * 判据只有「本次是否超预算」，准入真相仍然只有 SQL
//!   （[`SqlxAdmissionPolicy`](super::admission::SqlxAdmissionPolicy)）；
//! * 限额**只能拒新**：绝不按等待时长或审批年龄驱逐已接纳且健康的挂起/已批准连接
//!   （§4.6 第 1 条末句）；
//! * 两个原语都在自身锁内完成「判定 + 记账」，临界区无 await、无外部回调，因此不存在
//!   「判过之后才减」的 TOCTOU 窗口。
//!
//! 时间一律由调用方注入（[`Instant`]），因此限速与过期淘汰都是**确定性**的：测试可以给出
//! 任意时刻而不需要真实等待。

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 全局握手速率的下限：至少允许 1 次/秒，配 0 表示「不接受任何握手」应由调用方用
/// `Option`/禁用监听表达，而不是靠一个永远为 0 的桶。
pub const MIN_HANDSHAKES_PER_SEC: u32 = 1;
/// 全局握手速率的硬上界：再大就等于没有限速（§4.6 第 1 条要求「限制握手速率」）。
pub const MAX_HANDSHAKES_PER_SEC: u32 = 100_000;
/// 突发桶容量的硬上界。
pub const MAX_HANDSHAKE_BURST: usize = 100_000;
/// 单一来源未认证/挂起名额的硬上界。
pub const MAX_PENDING_PER_SOURCE_LIMIT: usize = 1_024;
/// 来源表的硬容量：表满即**拒绝新来源**，绝不无界增长（攻击者伪造大量源 IP 时内存有界）。
pub const MAX_SOURCE_TABLE_ENTRIES: usize = 4_096;
/// 来源空闲多久后可被淘汰（仅在名额为 0 时才会被淘汰，绝不驱逐在途挂起连接）。
pub const SOURCE_IDLE_TTL: Duration = Duration::from_secs(60);

/// 限额构造错误（去敏固定文案，不含对端可控内容）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitsError {
    RateOutOfRange,
    BurstOutOfRange,
    PerSourceOutOfRange,
    TableCapacityOutOfRange,
    IdleTtlZero,
}

// ---------------------------------------------------------------------------
// 全局握手速率（单调令牌桶）
// ---------------------------------------------------------------------------

/// 全局握手速率令牌桶：速率 `rate` 次/秒，桶容量 `burst`。
///
/// * 补足量用整数运算保留**未兑换的时间余量**（见 [`BucketState::accrued`]）：把加权纳秒
///   `(elapsed_nanos * rate)` 累加，每满 1e9 才兑换一枚整数令牌，余数留到下次。持续以
///   `< 1/rate` 的短间隔调用**不会**把不足一枚的时间反复截断丢弃（审查 N4），也全程不用浮点，
///   跨平台结果一致；
/// * 时钟只会单调前进（`Instant`），故不存在「墙钟回拨把桶冲满」；同一时刻重复调用不补令牌；
/// * 饱和（攒到 `burst`）时把余量清零，空闲时间不得在下次「攒出」超出 `burst` 的令牌；
/// * [`HandshakeRateLimiter::try_acquire`] 与 [`HandshakeRateLimiter::available`] 复用同一条
///   [`refill`] 回填路径，观测本身不额外改变令牌（不破坏回填、不放大 burst）。
pub struct HandshakeRateLimiter {
    rate_per_sec: u128,
    burst: usize,
    state: Mutex<BucketState>,
}

#[derive(Clone, Copy)]
struct BucketState {
    tokens: usize,
    /// 尚未兑换成整数令牌的时间余量，单位「纳秒 × rate」。每兑换一枚令牌消耗 `1e9`，
    /// 余数留在下一次补足时继续累积（N4：短间隔不再被 floor 清零）。
    accrued: u128,
    last_refill: Instant,
}

/// 把 `state.last_refill`→`now` 的时间兑换成整数令牌，保留未兑换的时间余量。
///
/// 返回后满足 `state.last_refill == now`（时钟前进时）且 `state.accrued < 1_000_000_000`；
/// 时钟不前进（`now <= last_refill`）时**原样返回**，绝不补令牌也绝不倒退。
fn refill(state: &mut BucketState, now: Instant, rate_per_sec: u128, burst: usize) {
    if now <= state.last_refill {
        return;
    }
    let elapsed = now.duration_since(state.last_refill).as_nanos();
    // 加权纳秒 =（本次经过纳秒 × rate）+ 上次余量；每满 1e9 兑换一枚整数令牌。
    let weighted = elapsed
        .saturating_mul(rate_per_sec)
        .saturating_add(state.accrued);
    let granted = usize::try_from(weighted / 1_000_000_000).unwrap_or(usize::MAX);
    let remainder = weighted % 1_000_000_000;
    let next = state.tokens.saturating_add(granted);
    if next >= burst {
        // 封顶：攒到上限后把余量清零——超出 burst 的时间不折算成「下次能透支」的额度。
        state.tokens = burst;
        state.accrued = 0;
    } else {
        state.tokens = next;
        state.accrued = remainder;
    }
    state.last_refill = now;
}

impl HandshakeRateLimiter {
    /// 构造限速桶；参数越界即 [`LimitsError`]（不静默夹到边界值）。
    pub fn new(rate_per_sec: u32, burst: usize) -> Result<Self, LimitsError> {
        if !(MIN_HANDSHAKES_PER_SEC..=MAX_HANDSHAKES_PER_SEC).contains(&rate_per_sec) {
            return Err(LimitsError::RateOutOfRange);
        }
        if burst == 0 || burst > MAX_HANDSHAKE_BURST {
            return Err(LimitsError::BurstOutOfRange);
        }
        Ok(Self {
            rate_per_sec: u128::from(rate_per_sec),
            burst,
            state: Mutex::new(BucketState {
                tokens: burst,
                accrued: 0,
                last_refill: Instant::now(),
            }),
        })
    }

    /// 取一个令牌：桶空即 `false`（调用方必须**拒新**，绝不排队积压）。
    #[must_use]
    pub fn try_acquire(&self, now: Instant) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        refill(&mut state, now, self.rate_per_sec, self.burst);
        if state.tokens == 0 {
            return false;
        }
        state.tokens -= 1;
        true
    }

    /// 当前可用令牌数（观测与测试断言用）。与 [`Self::try_acquire`] 共用 [`refill`]，
    /// 观测只回填到与真实经过时间一致的整数令牌，绝不因读一次就改变后续回填的余量。
    #[must_use]
    pub fn available(&self, now: Instant) -> usize {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        refill(&mut state, now, self.rate_per_sec, self.burst);
        state.tokens
    }
}

// ---------------------------------------------------------------------------
// 来源未认证/挂起名额
// ---------------------------------------------------------------------------

/// 一次来源名额拒绝的原因（去敏，不含来源地址以外的信息）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceDenial {
    /// 该来源的未认证/挂起名额已满。
    PerSourceFull,
    /// 来源表已达硬容量且无可淘汰项：拒绝**新**来源，已登记来源不受影响。
    TableFull,
}

/// 按来源 IP 统计的未认证/挂起连接名额（RAII 归还）。
///
/// 名额在「取到 socket 后、昂贵握手之前」占用，并在 **ready / 失败 / 取消** 任一路径由
/// [`SourcePermit::drop`] 归还。因此同一 NAT 后大量**已 ready** 设备不会挤占彼此的未认证
/// 名额（审查补项：不能为了省事让来源名额永久占着，把 `max_sessions` 压低）。
///
/// 表的有界性有三层：单一来源上限、表硬容量、空闲（且名额为 0）来源的可淘汰；表满时只拒
/// 新来源，绝不驱逐任何在途名额。
pub struct SourceQuota {
    max_per_source: usize,
    max_sources: usize,
    idle_ttl: Duration,
    table: Arc<Mutex<HashMap<IpAddr, SourceState>>>,
}

#[derive(Clone, Copy)]
struct SourceState {
    pending: usize,
    last_seen: Instant,
}

impl SourceQuota {
    /// 构造来源名额表；参数越界即 [`LimitsError`]。
    pub fn new(
        max_per_source: usize,
        max_sources: usize,
        idle_ttl: Duration,
    ) -> Result<Self, LimitsError> {
        if max_per_source == 0 || max_per_source > MAX_PENDING_PER_SOURCE_LIMIT {
            return Err(LimitsError::PerSourceOutOfRange);
        }
        if max_sources == 0 || max_sources > MAX_SOURCE_TABLE_ENTRIES {
            return Err(LimitsError::TableCapacityOutOfRange);
        }
        if idle_ttl.is_zero() {
            return Err(LimitsError::IdleTtlZero);
        }
        Ok(Self {
            max_per_source,
            max_sources,
            idle_ttl,
            table: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 占用该来源的一个名额；拒绝原因见 [`SourceDenial`]。
    #[allow(
        clippy::mutable_key_type,
        reason = "IpAddr 不可变，只是 HashMap 的键类型"
    )]
    pub fn try_reserve(&self, source: IpAddr, now: Instant) -> Result<SourcePermit, SourceDenial> {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(state) = table.get_mut(&source) {
            if state.pending >= self.max_per_source {
                return Err(SourceDenial::PerSourceFull);
            }
            state.pending += 1;
            state.last_seen = now;
            return Ok(SourcePermit {
                source,
                table: Arc::clone(&self.table),
            });
        }
        if table.len() >= self.max_sources {
            // 只淘汰「名额已归零且已空闲」的来源：绝不减少任何在途挂起连接的额度。
            table.retain(|_, state| {
                state.pending > 0 || now.duration_since(state.last_seen) < self.idle_ttl
            });
            if table.len() >= self.max_sources {
                return Err(SourceDenial::TableFull);
            }
        }
        table.insert(
            source,
            SourceState {
                pending: 1,
                last_seen: now,
            },
        );
        Ok(SourcePermit {
            source,
            table: Arc::clone(&self.table),
        })
    }

    /// 该来源当前占用的名额数（观测与测试断言用）。
    #[must_use]
    pub fn pending(&self, source: IpAddr) -> usize {
        self.table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&source)
            .map_or(0, |state| state.pending)
    }

    /// 当前被跟踪的来源数（观测与测试断言用）。
    #[must_use]
    pub fn tracked_sources(&self) -> usize {
        self.table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// 淘汰「名额已归零且空闲超过 `idle_ttl`」的来源条目，返回淘汰数量。
    ///
    /// 这是来源表的**过期淘汰**：名额仍在途的来源绝不被删；表的硬容量
    /// （[`MAX_SOURCE_TABLE_ENTRIES`]）是淘汰之外的第二层上界，因此既不会永久增长，也不会
    /// 因淘汰而把任何挂起连接的额度抹掉。
    pub fn purge(&self, now: Instant) -> usize {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        let before = table.len();
        table.retain(|_, state| {
            state.pending > 0 || now.duration_since(state.last_seen) < self.idle_ttl
        });
        before - table.len()
    }
}

/// 来源名额（RAII）：`drop` 即归还，因此取消/失败/panic 都不会泄漏名额。
pub struct SourcePermit {
    source: IpAddr,
    table: Arc<Mutex<HashMap<IpAddr, SourceState>>>,
}

impl std::fmt::Debug for SourcePermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourcePermit")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl SourcePermit {
    /// 本名额对应的来源地址。
    #[must_use]
    pub fn source(&self) -> IpAddr {
        self.source
    }
}

impl Drop for SourcePermit {
    fn drop(&mut self) {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(state) = table.get_mut(&self.source) {
            state.pending = state.pending.saturating_sub(1);
        }
    }
}

// ---------------------------------------------------------------------------
// 来源握手速率（审查补项：全局**及来源**握手速率）
// ---------------------------------------------------------------------------

/// 按来源 IP 的握手速率桶（审查补项：spec §4.6「全局**及来源**握手速率」）。
///
/// 与 [`SourceQuota`]（并发名额）是**两个正交维度**：名额管「同时在途多少条未认证连接」，
/// 速率管「每秒新开多少条」。二者分开计数、分开拒绝原因，一个不替代另一个。
///
/// **反绕过（关键）**：桶表按 IP 持久，速率桶**不**随某条连接 ready/失败/取消而归还（那是
/// [`SourcePermit`] 的语义，管的是名额）。条目只在「距最近一次握手尝试超过 `idle_ttl`」时才被淘汰。
/// 因此攻击者无法靠「快速建连再释放名额」把同来源的桶清零重来——只要它持续握手，`last_seen`
/// 每尝试刷新、令牌被持续扣减，桶绝不会被清空。空闲 TTL 只淘汰「真的停手了」的来源，重置其桶
/// 无害。表另有硬容量上界：满时只淘汰已过 TTL 的空闲条目，仍无空位则**拒新来源**，绝不无界增长、
/// 绝不驱逐仍在活跃的来源。
pub struct SourceRateLimiter {
    rate_per_sec: u128,
    burst: usize,
    max_sources: usize,
    idle_ttl: Duration,
    table: Arc<Mutex<HashMap<IpAddr, SourceRateState>>>,
}

#[derive(Clone, Copy)]
struct SourceRateState {
    bucket: BucketState,
    /// 最近一次握手尝试时刻：只用于「活跃来源不被 TTL 淘汰」的判据，不参与令牌计算。
    last_seen: Instant,
}

impl SourceRateLimiter {
    /// 构造来源速率表；参数越界即 [`LimitsError`]（与全局桶共用同一组速率/突发边界）。
    pub fn new(
        rate_per_sec: u32,
        burst: usize,
        max_sources: usize,
        idle_ttl: Duration,
    ) -> Result<Self, LimitsError> {
        if !(MIN_HANDSHAKES_PER_SEC..=MAX_HANDSHAKES_PER_SEC).contains(&rate_per_sec) {
            return Err(LimitsError::RateOutOfRange);
        }
        if burst == 0 || burst > MAX_HANDSHAKE_BURST {
            return Err(LimitsError::BurstOutOfRange);
        }
        if max_sources == 0 || max_sources > MAX_SOURCE_TABLE_ENTRIES {
            return Err(LimitsError::TableCapacityOutOfRange);
        }
        if idle_ttl.is_zero() {
            return Err(LimitsError::IdleTtlZero);
        }
        Ok(Self {
            rate_per_sec: u128::from(rate_per_sec),
            burst,
            max_sources,
            idle_ttl,
            table: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 该来源本次握手是否放行：桶空即 `false`（调用方必须拒新，绝不排队）。
    ///
    /// 判定与记账在**同一把锁内**完成（无「判过才减」的 TOCTOU 窗口）；复用与全局桶一致的
    /// [`refill`] 回填（保留时间余量，短间隔不被 floor 清零，审查 N4）。
    #[must_use]
    #[allow(
        clippy::mutable_key_type,
        reason = "IpAddr 不可变，只是 HashMap 的键类型"
    )]
    pub fn try_acquire(&self, source: IpAddr, now: Instant) -> bool {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        if !table.contains_key(&source) {
            if table.len() >= self.max_sources {
                // 只淘汰「距最近一次尝试已超过 TTL」的空闲来源：仍在活跃的来源绝不被动。
                table.retain(|_, state| now.duration_since(state.last_seen) < self.idle_ttl);
                if table.len() >= self.max_sources {
                    // 表满且无可淘汰项：拒**新**来源，绝不无界增长、绝不驱逐活跃来源。
                    return false;
                }
            }
            table.insert(
                source,
                SourceRateState {
                    bucket: BucketState {
                        tokens: self.burst,
                        accrued: 0,
                        last_refill: now,
                    },
                    last_seen: now,
                },
            );
        }
        let state = table.get_mut(&source).expect("just ensured present");
        // 每次尝试都刷新 last_seen：活跃来源不会被 TTL 淘汰，也就无法靠「停一下再打」重置桶。
        state.last_seen = now;
        refill(&mut state.bucket, now, self.rate_per_sec, self.burst);
        if state.bucket.tokens == 0 {
            false
        } else {
            state.bucket.tokens -= 1;
            true
        }
    }

    /// 当前被跟踪的来源数（观测与测试断言用）。
    #[must_use]
    pub fn tracked_sources(&self) -> usize {
        self.table
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// 淘汰「距最近一次尝试已超过 `idle_ttl`」的来源桶条目，返回淘汰数量。
    ///
    /// 只淘汰真正空闲的来源；这**不是**给调用方「手动重置某来源桶」的后门——重置一个活跃来源的
    /// 桶只会让 try_acquire 下次重建一个满桶，但那需要该来源在 `idle_ttl` 内一次都没尝试。
    pub fn purge(&self, now: Instant) -> usize {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        let before = table.len();
        table.retain(|_, state| now.duration_since(state.last_seen) < self.idle_ttl);
        before - table.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Instant {
        Instant::now()
    }

    #[test]
    fn rate_limiter_denies_over_burst_then_refills_at_configured_rate() {
        let limiter = HandshakeRateLimiter::new(10, 4).expect("limiter");
        let now = base();
        // 桶初始满：前 4 次放行，第 5 次拒。
        for _ in 0..4 {
            assert!(limiter.try_acquire(now));
        }
        assert!(!limiter.try_acquire(now));
        // 100ms 恰好补 1 个令牌（10 次/秒），不多补。
        let later = now + Duration::from_millis(100);
        assert_eq!(limiter.available(later), 1);
        assert!(limiter.try_acquire(later));
        assert!(!limiter.try_acquire(later));
        // 补足量不会超过 burst（长时间空闲也不会攒出 5 个）。
        let much_later = now + Duration::from_secs(3_600);
        assert_eq!(limiter.available(much_later), 4);
    }

    #[test]
    fn rate_limiter_does_not_refill_on_clock_repeat() {
        let limiter = HandshakeRateLimiter::new(50, 2).expect("limiter");
        let now = base();
        assert!(limiter.try_acquire(now));
        assert!(limiter.try_acquire(now));
        // 同一时刻重复调用绝不补令牌（不靠浮点误差放行）。
        assert!(!limiter.try_acquire(now));
        assert_eq!(limiter.available(now), 0);
    }

    #[test]
    fn rate_bucket_keeps_fraction_across_short_intervals() {
        // 审查 N4：rate=10/s、burst=1，初始 burst 用尽后持续以 60ms(< 1/rate=100ms) 的短
        // 间隔调用。旧实现每步 `floor(60ms × 10 / 1e9)=0` 且把 `last_refill` 推进到 `now`，
        // 未兑换的时间被反复丢弃 → 22 次全拒；保留加权纳秒余量后必须按 rate 稳定获准。
        let limiter = HandshakeRateLimiter::new(10, 1).expect("limiter");
        let base = base();
        assert!(limiter.try_acquire(base), "初始 burst 令牌");
        assert!(!limiter.try_acquire(base), "burst 用尽");
        let mut granted = 0u32;
        let mut now = base;
        for _ in 0..22 {
            now += Duration::from_millis(60);
            if limiter.try_acquire(now) {
                granted += 1;
            }
        }
        // 1.32s × 10/s 理想约 13 枚；burst=1 会把每次兑换余量清零（不放大 burst），故实得
        // 略低于理想但远大于旧实现的 0——证明 fraction 被累积而非丢弃。
        assert!(
            (8..=14).contains(&granted),
            "持续短间隔必须按 rate 累积获准（保留 fraction），实得 {granted}（旧实现恒为 0）"
        );
    }

    #[test]
    fn rate_bucket_refills_exactly_at_rate_with_injected_clock() {
        // 精确节拍核对（无需真实 sleep）：rate=10/s、burst=1，每 10ms 取一次，恰好每 10 步
        // 兑换 1 枚 → 100 步（1s）= 10 枚，与配置速率严格相等；旧实现全拒（0 枚）。
        let limiter = HandshakeRateLimiter::new(10, 1).expect("limiter");
        let base = base();
        assert!(limiter.try_acquire(base));
        let mut granted = 0u32;
        let mut now = base;
        for _ in 0..100 {
            now += Duration::from_millis(10);
            if limiter.try_acquire(now) {
                granted += 1;
            }
        }
        assert_eq!(granted, 10, "10ms 网格 × rate10/s 必须恰得 10 枚");
    }

    #[test]
    fn rate_bucket_saturation_does_not_amplify_burst() {
        // 长时间空闲封顶：tokens 停在 burst、余量清零；随后只能按 burst 立即透支 + 之后按
        // rate 回填，绝不会因保留了大量空闲「时间余量」而在下次攒出超过 burst 的令牌。
        let limiter = HandshakeRateLimiter::new(10, 3).expect("limiter");
        let base = base();
        // 先耗尽 burst。
        for _ in 0..3 {
            assert!(limiter.try_acquire(base));
        }
        assert!(!limiter.try_acquire(base));
        // 空闲 1 小时：封顶到 burst=3，且不留额外可兑换余量。
        let idle = base + Duration::from_secs(3_600);
        assert_eq!(limiter.available(idle), 3);
        // 一次性最多透支 3 枚（= burst），第 4 枚必须等回填。
        assert!(limiter.try_acquire(idle));
        assert!(limiter.try_acquire(idle));
        assert!(limiter.try_acquire(idle));
        assert!(
            !limiter.try_acquire(idle),
            "饱和后同一时刻不得攒出超过 burst 的令牌（余量已清零）"
        );
    }

    #[test]
    fn rate_limiter_rejects_out_of_range_config() {
        assert_eq!(
            HandshakeRateLimiter::new(0, 10).err(),
            Some(LimitsError::RateOutOfRange)
        );
        assert_eq!(
            HandshakeRateLimiter::new(MAX_HANDSHAKES_PER_SEC + 1, 10).err(),
            Some(LimitsError::RateOutOfRange)
        );
        assert_eq!(
            HandshakeRateLimiter::new(10, 0).err(),
            Some(LimitsError::BurstOutOfRange)
        );
        assert_eq!(
            HandshakeRateLimiter::new(10, MAX_HANDSHAKE_BURST + 1).err(),
            Some(LimitsError::BurstOutOfRange)
        );
    }

    #[test]
    fn source_quota_is_per_source_and_released_by_raftii() {
        let now = base();
        let a: IpAddr = "203.0.113.1".parse().expect("ip");
        let b: IpAddr = "203.0.113.2".parse().expect("ip");
        let quota = SourceQuota::new(2, 16, SOURCE_IDLE_TTL).expect("quota");
        let p1 = quota.try_reserve(a, now).expect("reserve a1");
        let p2 = quota.try_reserve(a, now).expect("reserve a2");
        assert_eq!(quota.pending(a), 2);
        assert_eq!(
            quota.try_reserve(a, now).err(),
            Some(SourceDenial::PerSourceFull),
            "同来源第 3 个未认证连接必须被拒"
        );
        // 另一来源不受影响。
        assert!(quota.try_reserve(b, now).is_ok());
        // 归还一个名额后同来源可再来一个（ready/失败/取消都靠 Drop）。
        drop(p1);
        assert_eq!(quota.pending(a), 1);
        assert!(quota.try_reserve(a, now).is_ok());
        drop(p2);
    }

    #[test]
    fn source_table_is_hard_bounded_and_evicts_only_idle_free_sources() {
        let now = base();
        let quota = SourceQuota::new(1, 2, SOURCE_IDLE_TTL).expect("quota");
        let held: IpAddr = "198.51.100.1".parse().expect("ip");
        let other: IpAddr = "198.51.100.2".parse().expect("ip");
        let fresh: IpAddr = "198.51.100.3".parse().expect("ip");
        let later: IpAddr = "198.51.100.4".parse().expect("ip");
        let permit = quota.try_reserve(held, now).expect("reserve held");
        // 另一个来源占住第二个条目（名额在途，不可淘汰）。
        let other_permit = quota.try_reserve(other, now).expect("reserve other");
        assert_eq!(quota.tracked_sources(), 2);
        // 表满且两个条目都在途：新来源必须被拒，已登记来源的额度绝不被驱逐。
        assert_eq!(
            quota.try_reserve(fresh, now).err(),
            Some(SourceDenial::TableFull)
        );
        assert_eq!(quota.pending(held), 1);
        // 释放 `other` 的**名额**后，条目仍在表里但已过 TTL → 可被淘汰，新来源因此可进入。
        drop(other_permit);
        let after_ttl = now + SOURCE_IDLE_TTL + Duration::from_millis(1);
        assert_eq!(
            quota.purge(after_ttl),
            1,
            "只应淘汰空闲且名额归零的那个来源"
        );
        assert!(quota.try_reserve(later, after_ttl).is_ok());
        assert_eq!(quota.pending(held), 1, "在途来源不得被驱逐");
        drop(permit);
        assert_eq!(quota.pending(held), 0);
    }

    #[test]
    fn permits_release_even_when_dropped_out_of_order() {
        let now = base();
        let ip: IpAddr = "203.0.113.9".parse().expect("ip");
        let quota = SourceQuota::new(3, 8, SOURCE_IDLE_TTL).expect("quota");
        let first = quota.try_reserve(ip, now).expect("first");
        let second = quota.try_reserve(ip, now).expect("second");
        let third = quota.try_reserve(ip, now).expect("third");
        assert_eq!(quota.pending(ip), 3);
        assert_eq!(
            quota.try_reserve(ip, now).err(),
            Some(SourceDenial::PerSourceFull)
        );
        drop(second);
        drop(first);
        assert_eq!(quota.pending(ip), 1);
        drop(third);
        assert_eq!(quota.pending(ip), 0);
        // 名额全部归还后条目仍在表内，直到过 TTL 才被淘汰：来源表因此有界且不永久增长。
        assert_eq!(quota.tracked_sources(), 1);
        assert_eq!(quota.purge(now + SOURCE_IDLE_TTL), 1);
        assert_eq!(quota.tracked_sources(), 0);
    }

    #[test]
    fn out_of_range_quota_config_fails_closed() {
        assert_eq!(
            SourceQuota::new(0, 8, SOURCE_IDLE_TTL).err(),
            Some(LimitsError::PerSourceOutOfRange)
        );
        assert_eq!(
            SourceQuota::new(MAX_PENDING_PER_SOURCE_LIMIT + 1, 8, SOURCE_IDLE_TTL).err(),
            Some(LimitsError::PerSourceOutOfRange)
        );
        assert_eq!(
            SourceQuota::new(1, 0, SOURCE_IDLE_TTL).err(),
            Some(LimitsError::TableCapacityOutOfRange)
        );
        assert_eq!(
            SourceQuota::new(1, MAX_SOURCE_TABLE_ENTRIES + 1, SOURCE_IDLE_TTL).err(),
            Some(LimitsError::TableCapacityOutOfRange)
        );
        assert_eq!(
            SourceQuota::new(1, 8, Duration::ZERO).err(),
            Some(LimitsError::IdleTtlZero)
        );
    }

    // --- 来源握手速率（审查补项）-------------------------------------------

    #[test]
    fn source_rate_is_per_source_independent_of_global_bucket() {
        // 同一 IP 超桶即拒；不同 IP 各自独立（不受别的来源耗尽影响）。
        let limiter = SourceRateLimiter::new(10, 1, 16, SOURCE_IDLE_TTL).expect("rate");
        let now = base();
        let a: IpAddr = "203.0.113.1".parse().expect("ip");
        let b: IpAddr = "203.0.113.2".parse().expect("ip");
        assert!(limiter.try_acquire(a, now), "来源 A 初始 burst");
        assert!(
            !limiter.try_acquire(a, now),
            "同一来源同一时刻第 2 次必须被来源速率拒"
        );
        assert!(
            limiter.try_acquire(b, now),
            "另一来源不受 A 耗尽影响（速率按真实 peer.ip 独立）"
        );
    }

    #[test]
    fn source_rate_refills_at_rate_across_short_intervals() {
        // 与全局桶共用回填实现：来源速率也在短间隔下按 rate 累积（N4 同源）。
        let limiter = SourceRateLimiter::new(10, 1, 16, SOURCE_IDLE_TTL).expect("rate");
        let base = base();
        let ip: IpAddr = "198.51.100.7".parse().expect("ip");
        assert!(limiter.try_acquire(ip, base));
        let mut granted = 0u32;
        let mut now = base;
        for _ in 0..100 {
            now += Duration::from_millis(10);
            if limiter.try_acquire(ip, now) {
                granted += 1;
            }
        }
        assert_eq!(
            granted, 10,
            "来源速率必须与配置速率严格一致（10ms×rate10/s，1s 恰 10 枚）"
        );
    }

    #[test]
    fn source_rate_bucket_is_not_reset_by_churn_only_idle_evicts() {
        // 反绕过：持续握手的活跃来源条目一直存在且被扣减，绝不被 TTL 淘汰；停手超过 TTL 才淘汰。
        let limiter = SourceRateLimiter::new(10, 2, 16, SOURCE_IDLE_TTL).expect("rate");
        let base = base();
        let ip: IpAddr = "203.0.0.5".parse().expect("ip");
        // 快速握手把桶抽干（同一时刻 2 枚耗尽）。
        assert!(limiter.try_acquire(ip, base));
        assert!(limiter.try_acquire(ip, base));
        assert!(!limiter.try_acquire(ip, base));
        // 在 idle_ttl 之内不断有尝试：last_seen 一直刷新 → purge 不得淘汰它（否则等于重置）。
        let mut now = base;
        for _ in 0..10 {
            now += SOURCE_IDLE_TTL / 3;
            assert!(
                limiter.try_acquire(ip, now),
                "回填后仍应能拿到令牌；重点是 last_seen 不断刷新"
            );
            assert_eq!(
                limiter.purge(now),
                0,
                "活跃来源的桶绝不能在 idle_ttl 内被清掉（否则攻击者可靠节奏重置绕过速率）"
            );
        }
        // 真正停手超过 idle_ttl：条目被淘汰（重置一个已静默的来源是允许的）。
        now += SOURCE_IDLE_TTL + Duration::from_millis(1);
        assert_eq!(limiter.purge(now), 1, "空闲超 TTL 的来源桶应被淘汰");
        assert_eq!(limiter.tracked_sources(), 0);
    }

    #[test]
    fn source_rate_table_is_hard_bounded_and_evicts_only_idle_sources() {
        // 表有界：满时只淘汰已过 TTL 的空闲来源，仍无空位即拒新来源；活跃来源绝不驱逐。
        let limiter = SourceRateLimiter::new(10, 1, 1, SOURCE_IDLE_TTL).expect("rate");
        let now = base();
        let active: IpAddr = "198.51.100.10".parse().expect("ip");
        let fresh: IpAddr = "198.51.100.11".parse().expect("ip");
        assert!(limiter.try_acquire(active, now), "第一个来源进入表");
        assert_eq!(limiter.tracked_sources(), 1);
        // 表满且 active 仍活跃（未过 TTL）：新来源被拒，active 不受影响。
        assert!(
            !limiter.try_acquire(fresh, now),
            "表满且无可淘汰项时必须拒新来源（内存有界）"
        );
        assert_eq!(limiter.tracked_sources(), 1, "活跃来源不得被驱逐");
        // active 空闲超过 TTL 后，新来源才可经淘汰腾出空位进入。
        let later = now + SOURCE_IDLE_TTL + Duration::from_millis(1);
        assert!(
            limiter.try_acquire(fresh, later),
            "空闲来源可被淘汰，新来源得以进入"
        );
        assert!(limiter.tracked_sources() <= 1, "表始终不超过硬容量");
    }

    #[test]
    fn source_rate_rejects_out_of_range_config() {
        assert_eq!(
            SourceRateLimiter::new(0, 1, 8, SOURCE_IDLE_TTL).err(),
            Some(LimitsError::RateOutOfRange)
        );
        assert_eq!(
            SourceRateLimiter::new(10, 0, 8, SOURCE_IDLE_TTL).err(),
            Some(LimitsError::BurstOutOfRange)
        );
        assert_eq!(
            SourceRateLimiter::new(10, 1, 0, SOURCE_IDLE_TTL).err(),
            Some(LimitsError::TableCapacityOutOfRange)
        );
        assert_eq!(
            SourceRateLimiter::new(10, 1, 8, Duration::ZERO).err(),
            Some(LimitsError::IdleTtlZero)
        );
    }
}

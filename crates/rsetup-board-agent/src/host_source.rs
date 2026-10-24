//! 生产本机只读来源：真实读数 + **读数不可知即失败关闭**，绝不伪造本机身份。
//!
//! 本模块从 `bin/rsetup-board-agent.rs` 抽出，供本地模式与生产 `--connect` 共用，
//! 因此两条路径共用同一份「不可伪造」约束：
//!
//! * `boot_id` 只来自 `/proc/sys/kernel/random/boot_id` 的**真实内核值**。读不到、
//!   不是规范 UUIDv4 一律 [`HostSourceError`] 失败关闭（构造期即拒绝），**绝不生成
//!   随机 UUID 冒认真实 boot**：boot_id 是任务 journal、票据与 fresh-boot 校验的
//!   证据锚点，伪造一个 boot_id 等于让「是否发生过 boot transition」这一判定失去意义。
//! * `agent_epoch` 与 `clock_epoch` 每个**进程**只生成一次（UUIDv4）；重连、重开流
//!   都不会重置它们，因此中控不会看到「同一个 agent_epoch 但 sample_seq 变小」。
//! * `sample_seq` 进程内单调自增，取号是一次原子的 read-modify-write：到达 `u64::MAX`
//!   后计数器**饱和**（不回绕，也不重复发出同一个值）并进入永久耗尽态：此后 `status()`
//!   返回 `sample_seq = 0` 的哨兵 DTO，它无法通过 `DeviceStatus::validate()`，
//!   `BoardAgent` 出站前置校验会把它映射成 `InternalError`（1009），
//!   即「不再产出任何样本」，绝不回绕复用旧序号。
//! * `clock_quality` 恒为 `observed`：本来源只是**观察**到本机墙钟，不是 NTP 校时结论，
//!   因此绝不声明 `ntp_valid`。
//! * 指标读数（load/mem/uptime）读不到就是**没有这条指标**，绝不回填 0 或假值。
//!
//! 能力申报诚实性：`supported_actions` 只有在**真实 reboot 调度器已注入**时才包含
//! `device.reboot.prepare` / `device.reboot.execute`。本 crate 的
//! [`crate::dispatch::HostRebootOs`] 仍返回 `Unsupported`（本切片不实现真实主机重启），
//! 因此生产装配 `reboot_execution = false`，能力清单只报 4 项可读动作。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, Capabilities, ClockSample, DeviceStatus, Metric, is_canonical_uuid_v4,
};

use crate::actions::ActionKind;
use crate::read_only::ReadOnlySource;

/// 内核 boot_id 的唯一真实来源（Linux）。
pub const KERNEL_BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
/// `Capabilities.max_status_bytes`：与本 crate 历史值一致（64 KiB，03 §2 建议上限）。
pub const MAX_STATUS_BYTES: u32 = 65_536;
/// 时钟质量：只声明「本机观察」，不是 NTP 有效。
pub const CLOCK_QUALITY_OBSERVED: &str = "observed";
/// `sample_seq` 耗尽后回给 `BoardAgent` 的哨兵值：DTO 自检必然失败 → 1009。
const SAMPLE_SEQ_EXHAUSTED: u64 = 0;

/// 本机来源构造期错误：只含固定类别，不含路径内容之外的敏感信息。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HostSourceError {
    /// 读不到内核 boot_id（文件缺失、不可读、内容为空）。
    #[error("kernel boot id is not readable; refusing to fabricate one")]
    BootIdUnreadable,
    /// boot_id 内容不是规范小写 UUIDv4：不是本机真实 boot 标识。
    #[error("kernel boot id is not a canonical UUIDv4; refusing to use it")]
    BootIdMalformed,
    /// journal epoch 不是规范 UUIDv4（journal 目录内容异常）。
    #[error("journal epoch is not a canonical UUIDv4")]
    JournalEpochMalformed,
}

/// 读真实内核 boot_id：**失败即 Err，绝不 fallback 到随机 UUID**。
///
/// 与 `is_canonical_uuid_v4` 一致要求小写规范形式：中控按同一规则校验，
/// 任一环节放宽都会让 fresh-boot 比较退化为字符串巧合。
pub fn read_kernel_boot_id() -> Result<String, HostSourceError> {
    let raw = std::fs::read_to_string(KERNEL_BOOT_ID_PATH)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase());
    match raw {
        None => Err(HostSourceError::BootIdUnreadable),
        Some(value) if !is_canonical_uuid_v4(&value) => Err(HostSourceError::BootIdMalformed),
        Some(value) => Ok(value),
    }
}

/// 生产/本地共用的本机只读来源。
///
/// 生命周期 = **进程**：`boot_id`/`agent_epoch`/`clock_epoch` 在构造时确定，
/// `sample_seq` 跨重连单调，因此同一 agent 实例的所有会话看到的是同一条序号轴。
#[derive(Debug)]
pub struct SystemReadOnlySource {
    boot_id: String,
    agent_epoch: String,
    clock_epoch: String,
    journal_epoch: String,
    /// 真实 reboot 调度器是否已注入（决定能力申报，不决定白名单本身）。
    reboot_execution: bool,
    /// journal 是否真实以 `RealFs` 耐久打开（诚实申报 `durable_task_journal`）。
    durable_task_journal: bool,
    sample_seq: AtomicU64,
    seq_exhausted: AtomicBool,
    started: Instant,
}

impl SystemReadOnlySource {
    /// 显式装配（生产入口与测试都用它）：`boot_id` 必须是真实读到的规范 UUIDv4，
    /// 否则构造失败。`journal_epoch` 同样必须是 UUIDv4（journal 持久 epoch）。
    pub fn new(
        boot_id: impl Into<String>,
        journal_epoch: impl Into<String>,
        reboot_execution: bool,
        durable_task_journal: bool,
    ) -> Result<Self, HostSourceError> {
        let boot_id = boot_id.into();
        if !is_canonical_uuid_v4(&boot_id) {
            return Err(HostSourceError::BootIdMalformed);
        }
        let journal_epoch = journal_epoch.into();
        if !is_canonical_uuid_v4(&journal_epoch) {
            return Err(HostSourceError::JournalEpochMalformed);
        }
        Ok(Self {
            boot_id,
            // 每进程一次：agent_epoch 与 clock_epoch 都不随重连变化。
            agent_epoch: uuid::Uuid::new_v4().to_string(),
            clock_epoch: uuid::Uuid::new_v4().to_string(),
            journal_epoch,
            reboot_execution,
            durable_task_journal,
            sample_seq: AtomicU64::new(0),
            seq_exhausted: AtomicBool::new(false),
            started: Instant::now(),
        })
    }

    /// 便捷构造：真实读取内核 boot_id（失败即 Err）。
    pub fn from_system(
        journal_epoch: impl Into<String>,
        reboot_execution: bool,
        durable_task_journal: bool,
    ) -> Result<Self, HostSourceError> {
        Self::new(
            read_kernel_boot_id()?,
            journal_epoch,
            reboot_execution,
            durable_task_journal,
        )
    }

    pub fn boot_id(&self) -> &str {
        &self.boot_id
    }
    pub fn agent_epoch(&self) -> &str {
        &self.agent_epoch
    }
    pub fn clock_epoch(&self) -> &str {
        &self.clock_epoch
    }
    /// 真实主机重启是否已接线（只影响能力申报）。
    pub fn reboot_execution(&self) -> bool {
        self.reboot_execution
    }
    /// `sample_seq` 是否已耗尽（耗尽后不再产出可出站样本）。
    pub fn sample_seq_exhausted(&self) -> bool {
        self.seq_exhausted.load(Ordering::SeqCst)
    }

    /// 单调取号：一次原子 read-modify-write 里完成「比较 + 推进」，在 `u64::MAX` 处
    /// **饱和不回绕**：到达上界后计数器保持不变，后续调用一律返回哨兵 0
    /// （DTO 自检必定拒绝），因此序号轴永不把旧序号冒成新样本。
    ///
    /// 为什么不能是 `fetch_add` + 独立耗尽标志：`fetch_add` 在上界处先把计数器**回绕**
    /// 成 0 再返回旧值，而耗尽标志是另一次写；已过早检、尚未推进的并发线程会读到
    /// 回绕后的 0 并返回 1，既复用了旧序号又让计数器离开上界。
    ///
    /// CAS 循环里「比较」与「推进」是同一次原子操作，`checked_add` 在上界返回 `None`
    /// 时不写回任何值，因此「计数器停在上界」与「耗尽」不存在可被并发插入的中间态。
    /// 使用显式 CAS 循环，不依赖当前 stable 已弃用的 `fetch_update` 名称；
    /// 不为此引入旧版本工具链兼容承诺。
    fn next_sample_seq(&self) -> u64 {
        loop {
            let current = self.sample_seq.load(Ordering::SeqCst);
            let Some(next) = current.checked_add(1) else {
                // 计数器已停在上界：本次没有号可发，永久进入耗尽态。
                self.seq_exhausted.store(true, Ordering::SeqCst);
                return SAMPLE_SEQ_EXHAUSTED;
            };
            // 成功即独占本次序号；失败说明有并发线程已推进，重读后重试。
            if self
                .sample_seq
                .compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return next;
            }
        }
    }

    fn monotonic_ns(&self) -> u64 {
        self.started
            .elapsed()
            .as_nanos()
            .try_into()
            .unwrap_or(u64::MAX)
    }

    /// 入站白名单里**真正可执行**的动作清单（能力申报的唯一来源）。
    fn advertised_actions(&self) -> Vec<String> {
        crate::actions::ACTION_ALLOWLIST
            .iter()
            .filter(|spec| {
                self.reboot_execution
                    || !matches!(
                        spec.kind,
                        ActionKind::RebootPrepare | ActionKind::RebootExecute
                    )
            })
            .map(|spec| spec.name.to_string())
            .collect()
    }
}

impl ReadOnlySource for SystemReadOnlySource {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: self.boot_id.clone(),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            supported_actions: self.advertised_actions(),
            durable_task_journal: self.durable_task_journal,
            max_status_bytes: MAX_STATUS_BYTES,
            journal_epoch: self.journal_epoch.clone(),
        }
    }

    fn status(&self) -> DeviceStatus {
        DeviceStatus {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: self.boot_id.clone(),
            agent_epoch: self.agent_epoch.clone(),
            // 耗尽后为 0：DTO 自检失败 → 1009，绝不回绕复用旧序号。
            sample_seq: self.next_sample_seq(),
            sampled_unix_ms: unix_ms(),
            sampled_monotonic_ns: self.monotonic_ns(),
            clock_epoch: self.clock_epoch.clone(),
            metrics: read_metrics(),
            clock_quality: CLOCK_QUALITY_OBSERVED.to_string(),
        }
    }

    fn clock(&self) -> ClockSample {
        let now = unix_ms();
        ClockSample {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: self.boot_id.clone(),
            clock_epoch: self.clock_epoch.clone(),
            receive_unix_ms: now,
            send_unix_ms: now,
            receive_monotonic_ns: self.monotonic_ns(),
            send_monotonic_ns: self.monotonic_ns(),
            quality: CLOCK_QUALITY_OBSERVED.to_string(),
        }
    }
}

/// 墙钟毫秒：只用于**观测**字段（`sampled_unix_ms` 等），期限判定不使用墙钟。
pub fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn metric(key: &str, value: f64, unit: &str) -> Metric {
    Metric {
        key: key.to_string(),
        value: Some(value),
        unit: unit.to_string(),
        quality: "ok".to_string(),
    }
}

/// 只读指标集合：每项单独读取，读不到就不产出该项（**绝不回填 0**）。
pub fn read_metrics() -> Vec<Metric> {
    let mut metrics = Vec::new();
    if let Some(value) = read_loadavg() {
        metrics.push(metric("cpu.load1", value, "1m"));
    }
    if let Some(value) = read_mem_used_percent() {
        metrics.push(metric("memory.used_percent", value, "%"));
    }
    if let Some(value) = read_uptime_secs() {
        metrics.push(metric("system.uptime", value, "s"));
    }
    metrics
}

fn read_loadavg() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn read_uptime_secs() -> Option<f64> {
    std::fs::read_to_string("/proc/uptime")
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

fn read_mem_used_percent() -> Option<f64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in info.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("MemTotal:") => total = fields.next().and_then(|v| v.parse::<f64>().ok()),
            Some("MemAvailable:") => available = fields.next().and_then(|v| v.parse::<f64>().ok()),
            _ => {}
        }
    }
    let (total, available) = (total?, available?);
    if total <= 0.0 || available < 0.0 || available > total {
        return None;
    }
    let value = (total - available) / total * 100.0;
    value.is_finite().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsetup_protocol::wire::validate_dto;
    use std::sync::Arc;

    const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
    const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

    #[test]
    fn malformed_or_missing_boot_id_is_refused_not_fabricated() {
        assert_eq!(
            SystemReadOnlySource::new("", EPOCH, false, true).unwrap_err(),
            HostSourceError::BootIdMalformed
        );
        // 大写形式不是本仓规范 UUIDv4；随机伪造更不允许。
        assert_eq!(
            SystemReadOnlySource::new(BOOT.to_uppercase(), EPOCH, false, true).unwrap_err(),
            HostSourceError::BootIdMalformed
        );
        assert_eq!(
            SystemReadOnlySource::new(BOOT, "not-a-uuid", false, true).unwrap_err(),
            HostSourceError::JournalEpochMalformed
        );
    }

    #[test]
    fn real_boot_id_is_a_canonical_uuid() {
        // 本机真实读数：读不到就 Err，绝不返回随机值。
        match read_kernel_boot_id() {
            Ok(value) => assert!(is_canonical_uuid_v4(&value)),
            Err(error) => assert!(matches!(
                error,
                HostSourceError::BootIdUnreadable | HostSourceError::BootIdMalformed
            )),
        }
    }

    #[test]
    fn epochs_are_stable_for_the_process_and_seq_monotonic() {
        let source = SystemReadOnlySource::new(BOOT, EPOCH, false, true).unwrap();
        let first = source.status();
        let second = source.status();
        assert_eq!(first.agent_epoch, second.agent_epoch);
        assert_eq!(first.clock_epoch, second.clock_epoch);
        assert_eq!(first.sample_seq, 1);
        assert_eq!(second.sample_seq, 2);
        assert_eq!(first.boot_id, BOOT);
        // clock_quality 只声明观察，不声明 NTP 有效。
        assert_eq!(first.clock_quality, CLOCK_QUALITY_OBSERVED);
        assert_eq!(source.clock().quality, CLOCK_QUALITY_OBSERVED);
        validate_dto(&first).unwrap();
        validate_dto(&source.capabilities()).unwrap();
        validate_dto(&source.clock()).unwrap();
    }

    #[test]
    fn capabilities_do_not_advertise_reboot_without_a_dispatcher() {
        let without = SystemReadOnlySource::new(BOOT, EPOCH, false, true).unwrap();
        let actions = without.capabilities().supported_actions;
        assert!(!actions.iter().any(|a| a.starts_with("device.reboot.")));
        assert_eq!(actions.len(), 4);
        let with = SystemReadOnlySource::new(BOOT, EPOCH, true, true).unwrap();
        assert!(
            with.capabilities()
                .supported_actions
                .iter()
                .any(|a| a == "device.reboot.execute")
        );
    }

    #[test]
    fn sample_seq_exhaustion_produces_no_valid_sample() {
        let source = SystemReadOnlySource::new(BOOT, EPOCH, false, true).unwrap();
        // 把「最后一次已发号」推到上界前一位，模拟溢出边界（不真的数到 u64::MAX）。
        source.sample_seq.store(u64::MAX - 1, Ordering::SeqCst);
        let last = source.status();
        // 最后一个合法样本仍可出站，且不会回绕。
        assert_eq!(last.sample_seq, u64::MAX);
        assert!(validate_dto(&last).is_ok());
        let after = source.status();
        assert!(source.sample_seq_exhausted());
        // 耗尽后样本必然通不过自检（sample_seq = 0），即不再出站。
        assert_eq!(after.sample_seq, SAMPLE_SEQ_EXHAUSTED);
        assert!(validate_dto(&after).is_err());
        assert_eq!(source.status().sample_seq, SAMPLE_SEQ_EXHAUSTED);
    }

    /// 并发取号在 `u64::MAX` 边界**必须饱和**：计数器永不回绕成小值，已发出的序号永不复用。
    ///
    /// 这里预置到边界（不真的数 2^64 次）并用 barrier 让所有线程同时进入取号点，
    /// 因此「已过 exhausted 早检、尚未原子推进」这段窗口被确定性地打开，
    /// 而不是靠调度概率复现。
    #[test]
    fn concurrent_numbering_saturates_at_u64_max_without_wrapping() {
        const THREADS: usize = 8;
        let source = Arc::new(SystemReadOnlySource::new(BOOT, EPOCH, false, true).unwrap());
        // 预置：MAX 已经发出（下一个合法值不存在），因此任何线程都不该再拿到正数序号。
        source.sample_seq.store(u64::MAX, Ordering::SeqCst);
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let taken: Vec<u64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let source = Arc::clone(&source);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        // 多次取号：耗尽是一次性的永久态，第二次调用同样不得回绕。
                        (source.next_sample_seq(), source.next_sample_seq())
                    })
                })
                .collect();
            let mut values = Vec::new();
            for handle in handles {
                let (first, second) = handle.join().unwrap();
                values.extend([first, second]);
            }
            values
        });
        // 只允许哨兵 0：MAX 之后的任何正数都意味着序号轴回绕并复用了旧序号。
        for value in &taken {
            assert_eq!(
                *value, SAMPLE_SEQ_EXHAUSTED,
                "MAX 之后取到 {value} 意味着序号回绕复用"
            );
        }
        // 计数器保持饱和在 MAX（不回零），耗尽位为真且不可逆。
        assert_eq!(source.sample_seq.load(Ordering::SeqCst), u64::MAX);
        assert!(source.sample_seq_exhausted());
        // 出站形态同样不可伪造：耗尽后的样本必然通不过 DTO 自检。
        assert!(validate_dto(&source.status()).is_err());
    }

    /// 边界「最后一个合法值」只可能被发出一次：预置 `MAX-1` 并发取号，
    /// 至多一个线程拿到 `u64::MAX`，其余必须是哨兵。
    #[test]
    fn the_last_legal_number_is_handed_out_at_most_once() {
        const THREADS: usize = 8;
        let source = Arc::new(SystemReadOnlySource::new(BOOT, EPOCH, false, true).unwrap());
        source.sample_seq.store(u64::MAX - 1, Ordering::SeqCst);
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let taken: Vec<u64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let source = Arc::clone(&source);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        source.next_sample_seq()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let legal_last = taken.iter().filter(|value| **value == u64::MAX).count();
        assert!(
            legal_last <= 1,
            "u64::MAX 被发出 {legal_last} 次，序号被复用: {taken:?}"
        );
        for value in &taken {
            assert!(
                *value == u64::MAX || *value == SAMPLE_SEQ_EXHAUSTED,
                "边界取号出现回绕值 {value}"
            );
        }
        assert_eq!(source.sample_seq.load(Ordering::SeqCst), u64::MAX);
        assert!(source.sample_seq_exhausted());
    }

    /// 正常区间并发取号仍须**唯一且单调可用**（每条序号只发一次，不回绕、不耗尽）。
    #[test]
    fn concurrent_numbering_stays_unique_below_the_boundary() {
        const THREADS: usize = 8;
        const PER_THREAD: u64 = 500;
        let source = Arc::new(SystemReadOnlySource::new(BOOT, EPOCH, false, true).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let taken: Vec<u64> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    let source = Arc::clone(&source);
                    let barrier = Arc::clone(&barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        let mut local = Vec::with_capacity(PER_THREAD as usize);
                        for _ in 0..PER_THREAD {
                            local.push(source.next_sample_seq());
                        }
                        local
                    })
                })
                .collect();
            let mut values = Vec::new();
            for handle in handles {
                values.extend(handle.join().unwrap());
            }
            values
        });
        let expected = (THREADS as u64) * PER_THREAD;
        let unique: std::collections::BTreeSet<u64> = taken.iter().copied().collect();
        assert_eq!(unique.len(), taken.len(), "并发取号出现重复序号");
        assert_eq!(taken.iter().copied().max(), Some(expected));
        assert_eq!(taken.iter().copied().min(), Some(1));
        assert_eq!(source.sample_seq.load(Ordering::SeqCst), expected);
        assert!(!source.sample_seq_exhausted());
        // 每条序号都能出站（DTO 自检通过），说明取号与出站一致。
        for record in unique.iter().take(8) {
            let mut status = source.status();
            status.sample_seq = *record;
            assert!(validate_dto(&status).is_ok());
        }
    }

    #[test]
    fn metric_readings_come_from_the_real_system_or_are_absent() {
        let metrics = read_metrics();
        for metric in &metrics {
            assert!(metric.value.unwrap().is_finite());
            assert_eq!(metric.quality, "ok");
        }
    }
}

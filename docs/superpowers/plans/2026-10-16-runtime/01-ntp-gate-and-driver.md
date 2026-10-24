### Task 1: 完整 NTP 30s 门禁与真请求关联 (Correlated NTP Client & 30s Boot Gate)

**Files:**
- Import: `crates/rsetup-controller/src/time/ntp.rs`（来源分支 `controller-v1-runtime-ntp-parser`）
- Create: `crates/rsetup-controller/src/time/socket.rs`
- Create: `crates/rsetup-controller/src/time/gate.rs`
- Modify: `crates/rsetup-controller/src/time/mod.rs`
- Modify: `crates/rsetup-controller/src/config.rs`
- Test: `crates/rsetup-controller/tests/gate_udp.rs`

**Interfaces:**
- Consumes: 导入的 `parse_sntp_response`、`NtpError`；Task 1（Runtime 计划）的 `TimeEvidence`、`TimeQuality`。不再重用 `board_clock::ClockEstimate` 的 board_offset 模型以免混淆。
- Produces:
  - `pub const DEFAULT_NTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(3)`（05 §2 待审建议默认）
  - `pub const NTP_SINGLE_REQUEST_TIMEOUT_CAP: Duration = Duration::from_secs(3)`（规格 05 §2 建议“单次请求上限3s”；本计划明确采用该建议作为实现策略——规格原文是建议，不是强制契约）
  - `pub const NTP_STARTUP_BUDGET: Duration = Duration::from_secs(30)`（00 §3 C-TIME **已确认约束**）
  - `pub const DEFAULT_NTP_MAX_RTT_MS: i64 = 2000`（05 §4 板端时间校验建议“RTT上限2s”；本计划选定策略——原§4属于board_clock，不能自动替代SNTP定义，待审）
  - `pub const NTP_EPOCH_OFFSET_MS: i128 = 2_208_988_800 * 1000`（NTP 纪元 1900-01-01 到 UNIX 纪元 1970-01-01 的固定毫秒偏差）
  - `pub struct NtpSample { pub offset_ms: i64, pub rtt_ms: i64, pub reference_utc: DateTime<Utc>, pub uncertainty_ms: u64 }`
  - `pub struct LocalStamp { pub wall_utc: DateTime<Utc>, pub mono: Instant }`（同一瞬间捕获的本地墙钟与单调快照）
  - `pub struct NtpCorrelationRequest { pub server_addr: SocketAddr, pub request_tx: u64, pub t1_wall_utc: DateTime<Utc>, pub t1_mono: Instant }`（T1 包含请求实际解析发送的 SocketAddr、64位唯一发送时戳/nonce，及本地墙钟与单调起点）
  - `pub fn correlate_sntp_response(packet: &[u8], request: &NtpCorrelationRequest, response_source: SocketAddr, t4: LocalStamp, max_rtt_ms: i64) -> Result<NtpSample, NtpError>`
  - `pub fn gate_should_close(elapsed: Duration, budget: Duration, correlated: bool) -> bool`
  - `pub fn next_backoff(attempt: u32) -> Duration`
  - `pub fn select_request_timeout(configured: Duration, remaining: Duration) -> Duration`（单次请求超时选择：`min(configured, NTP_SINGLE_REQUEST_TIMEOUT_CAP, remaining)`，纯函数，由 `run_startup_sample` 消费）
  - `pub trait SntpResolver: Send + Sync { fn resolve<'a>(&'a self, host: &'a str) -> SntpResolveFuture<'a>; }`（`SntpResolveFuture` 为 `Pin<Box<dyn Future<Output=Result<SocketAddr,NtpError>>+Send+'a>>`；DNS必须异步可取消且纳入整体预算，不使用同步ToSocketAddrs）
  - `pub async fn query_sntp(socket: &tokio::net::UdpSocket, resolver: &dyn SntpResolver, server: &str, request_nonce: u64, timeout: Duration, max_rtt_ms: i64) -> Result<NtpSample, NtpError>`
  - `pub type SntpBindFuture = Pin<Box<dyn Future<Output = Result<tokio::net::UdpSocket, NtpError>> + Send>>`（owned “绑定 UDP socket” future，与 `SntpResolveFuture` 同形；生产路径为 `UdpSocket::bind` 异步，测试注入受控 bind）
  - `pub async fn run_startup_sample(bind: SntpBindFuture, resolver: &dyn SntpResolver, servers: &[String], request_timeout: Duration, budget: Duration, max_rtt_ms: i64, now: tokio::time::Instant) -> Option<(NtpSample, String)>`（启动总预算唯一函数：只创建一次绝对 deadline、硬 ≤30s；socket bind/DNS/多源/全部重试同处一个绝对 `timeout_at`；单次请求由 `select_request_timeout(request_timeout, remaining)` 选定 = `min(configured, 05 §2 建议 3s 上限, 剩余预算)`，不按源/阶段重建 deadline；空源有界等待 deadline；成功返回 (样本, 产生源) 供 `start_time_gate` 写 `TimeEvidence`，超时/空源返回 `None` 由调用方回退 `SystemFallback`）
  - `pub struct TimeGateHandle` + `pub fn start_time_gate(config: TimeGateConfig, clock_epoch: Uuid) -> TimeGateHandle`、`impl TimeGateHandle { pub async fn wait_ready(&self) -> TimeEvidence; pub async fn current(&self) -> TimeEvidence; pub fn background_retry_active(&self) -> bool }`
  - `NtpError` 新增变体：`Io`、`SourceMismatch`、`OriginateMismatch`、`ZeroReceiveTimestamp`、`RttTooLarge`、`TimestampOverflow`，并**保留**导入分支的 `PacketTooShort`、`Unsynchronized`、`InvalidStratum`、`InvalidVersion`、`InvalidMode`、`ZeroTransmitTimestamp`。

- [ ] **Step 0: 导入既有纯解析器并在导入处扩充 NtpError 枚举**

```bash
git show controller-v1-runtime-ntp-parser:crates/rsetup-controller/src/time/ntp.rs > crates/rsetup-controller/src/time/ntp.rs
git show controller-v1-runtime-ntp-parser:crates/rsetup-controller/src/time/mod.rs
```
把 `ntp.rs` 落地，并在 `src/time/mod.rs` 增加 `pub mod ntp;` 与 `pub use ntp::{NtpError, parse_sntp_response};`（保留现有 `board_clock`/`evidence` 导出）。
**关键前置（解决导入 parser 尚无 Io/SourceMismatch 导致桩无法编译）**：在 `crates/rsetup-controller/src/time/ntp.rs` 内直接为 `pub enum NtpError` 追加变体（保留 `#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]`）：
```rust
    #[error("io error")]
    Io,
    #[error("response source mismatch")]
    SourceMismatch,
    #[error("originate timestamp mismatch")]
    OriginateMismatch,
    #[error("zero receive timestamp")]
    ZeroReceiveTimestamp,
    #[error("round trip time exceeds maximum allowable threshold")]
    RttTooLarge,
    #[error("timestamp arithmetic overflow")]
    TimestampOverflow,
```
此项扩充仅追加变体与 `#[error]` 显示，绝不改动已导入的 `parse_sntp_response` 解析逻辑和已有 6 个变体。核对离线编译：`cargo check --offline --locked -p rsetup-controller`。若该分支尚未合入且用户不允许 cherry-pick/merge，则本任务标 `BLOCKED`，不得另写解析器。

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

创建 `crates/rsetup-controller/src/time/socket.rs`，只放**签名真实、行为错误**的桩：

```rust
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use chrono::{DateTime, Utc};
use tokio::net::UdpSocket;

use crate::time::ntp::NtpError;

pub const NTP_PACKET_LEN: usize = 48;
pub const NTP_EPOCH_OFFSET_MS: i128 = 2_208_988_800 * 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtpSample {
    pub offset_ms: i64,
    pub rtt_ms: i64,
    pub reference_utc: DateTime<Utc>,
    pub uncertainty_ms: u64,
}

pub type SntpResolveFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<SocketAddr, NtpError>> + Send + 'a>>;

pub trait SntpResolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str) -> SntpResolveFuture<'a>;
}

/// Owned “绑定 UDP socket” future（生产：`UdpSocket::bind` 异步；测试注入受控 bind），与 `SntpResolveFuture` 同形。
pub type SntpBindFuture = std::pin::Pin<Box<dyn std::future::Future<Output = Result<UdpSocket, NtpError>> + Send>>;

#[derive(Clone, Copy, Debug)]
pub struct LocalStamp {
    pub wall_utc: DateTime<Utc>,
    pub mono: Instant,
}

#[derive(Clone, Debug)]
pub struct NtpCorrelationRequest {
    pub server_addr: SocketAddr,
    pub request_tx: u64,
    pub t1_wall_utc: DateTime<Utc>,
    pub t1_mono: Instant,
}

/// 可编译 RED 桩：恒返回 Err(Io)，使关联/门禁断言全部 RED。
pub async fn query_sntp(
    _socket: &UdpSocket,
    _resolver: &dyn SntpResolver,
    _server: &str,
    _request_nonce: u64,
    _timeout: Duration,
    _max_rtt_ms: i64,
) -> Result<NtpSample, NtpError> {
    Err(NtpError::Io)
}

/// 可编译 RED 桩：只做结构校验，绝不校验源/originate/RTT/时戳。
pub fn correlate_sntp_response(
    packet: &[u8],
    _request: &NtpCorrelationRequest,
    _response_source: SocketAddr,
    _t4: LocalStamp,
    _max_rtt_ms: i64,
) -> Result<NtpSample, NtpError> {
    crate::time::ntp::parse_sntp_response(packet)?;
    Ok(NtpSample {
        offset_ms: 0,
        rtt_ms: 0,
        reference_utc: DateTime::UNIX_EPOCH,
        uncertainty_ms: 0,
    })
}
```

创建 `crates/rsetup-controller/src/time/gate.rs`，同样先放可编译桩：

```rust
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use chrono::Utc;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::time::evidence::{TimeEvidence, TimeQuality};

pub const NTP_STARTUP_BUDGET: Duration = Duration::from_secs(30); // 00 §3 C-TIME 已确认
pub const DEFAULT_NTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(3); // 05 §2 待审建议
pub const NTP_SINGLE_REQUEST_TIMEOUT_CAP: Duration = Duration::from_secs(3); // 05 §2 建议 3s 上限；本计划明确采用该建议作为实现策略（建议，非规格强制）
pub const DEFAULT_NTP_MAX_RTT_MS: i64 = 2000; // 05 §4 board_clock 建议 RTT上限2s；本计划采用为 SNTP 默认（原§4属于board_clock，不能自动替代SNTP定义）
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(60);

pub const DEFAULT_REVALIDATE_INTERVAL: Duration = Duration::from_secs(60);
pub const DEFAULT_MAX_EVIDENCE_AGE_MS: u64 = 300_000;

#[derive(Clone, Debug)]
pub struct TimeGateConfig {
    pub servers: Vec<String>,
    pub request_timeout: Duration,
    pub startup_budget: Duration,
    pub max_rtt_ms: i64,
    /// 周期复验间隔：消费点 = `TimeGateDriver` NtpValid/Stale 分支的 tick 等待（契约 2）。
    /// 默认 `DEFAULT_REVALIDATE_INTERVAL`（60s）；合法性：必须 > 0。
    pub revalidate_interval: Duration,
    /// 证据时效上限（ms）：消费点 = `TimeGateDriver::evidence_stale()`（契约 2/4），
    /// `sample_age_ms > max_evidence_age_ms` 判定 stale。默认 `DEFAULT_MAX_EVIDENCE_AGE_MS`（300000）；合法性：必须 > 0。
    pub max_evidence_age_ms: u64,
}

impl TimeGateConfig {
    /// 合法性校验：`start_time_gate` 启动前调用一次，失败即拒绝启动（与 `request_timeout` 3s 上限校验同级）。
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.revalidate_interval.is_zero() { return Err("revalidate_interval 必须 > 0"); }
        if self.max_evidence_age_ms == 0 { return Err("max_evidence_age_ms 必须 > 0"); }
        Ok(())
    }
}

/// 可编译 RED 桩：忽略 budget 与 correlated，恒返回 true。
pub fn gate_should_close(_elapsed: Duration, _budget: Duration, _correlated: bool) -> bool {
    true
}

/// 可编译 RED 桩：常返回 BACKOFF_MIN，无指数增长与封顶。
pub fn next_backoff(_attempt: u32) -> Duration { BACKOFF_MIN }

/// 可编译 RED 桩：忽略 3s 上限与剩余预算，恒返回 `configured`。
pub fn select_request_timeout(configured: Duration, _remaining: Duration) -> Duration { configured }

/// RT-R12-1（单元 1）：原子时间快照——时间证据与其配对的单调采样锚点由同一把 RwLock 保护，
/// 读侧在同一读锁内原子取回、写侧在同一写锁内原子写入，杜绝双锁分读时"旧 reference 配新锚点"
/// （或反之）的不一致组合；handle 与 driver 共享同一 `Arc<RwLock<TimeGateSnapshot>>`。
#[derive(Clone)]
pub(crate) struct TimeGateSnapshot {
    pub evidence: TimeEvidence,
    pub sample_instant: Option<tokio::time::Instant>,
}

pub struct TimeGateHandle {
    pub(crate) snapshot: Arc<RwLock<TimeGateSnapshot>>,
    ready: Arc<tokio::sync::Notify>,
    is_ready: Arc<AtomicBool>,
    retry_attempt: Arc<AtomicU32>,
    retry_active: Arc<AtomicBool>,
    /// RT-R11-4（单元 B）：由 `start_time_gate` 从 `config.max_evidence_age_ms` 注入的配置阈值；
    /// `current()` 据此动态判定降级（自定义阈值如 180s 与默认 300s 均即时生效，不再固定取默认值）。
    max_evidence_age_ms: u64,
}

impl TimeGateHandle {
    /// GREEN 实现：已 ready 立即返回快照；否则等待 ready 通知，最长 30s 超时兜底返回当前快照。
    pub async fn wait_ready(&self) -> TimeEvidence {
        // RT-R7-4：修复检查-注册竞态——tokio 的 Notified 首次 poll 才入队，且 Notify 不为未来的
        // waiter 保留 permit；若 store(true) + notify_waiters() 恰在 load(false) 之后、监听器入队
        // 之前发生，通知即丢失，调用方死等满 30s。故每轮必须先创建监听器并 enable() 立即注册，
        // 再复查 is_ready。超时（Err 分支被 `let _ =` 吸收）不 panic，落到下方返回当前快照兜底。
        let _ = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                // 先创建通知监听器并注册，防止通知在检查后发出而丢失
                let notified = self.ready.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                // 一进门已 ready 则立即 break 返回快照（耗时 0）；未 ready 则挂上监听再查状态
                if self.is_ready.load(Ordering::SeqCst) {
                    break;
                }
                notified.await;
            }
        })
        .await;
        self.current().await
    }
    /// RT-R8-1：sample_age_ms 不冻结在写入时刻——读取时按采样单调锚点动态派生
    /// （快照存储 age + 锚点经过时间 Instant::now().duration_since(anchor).as_millis() as u64），
    /// 使读取与 stale 判定看到的 age 随时间推进；无锚点（从未采样成功）时原样返回快照。
    /// RT-R10-2（05 规格 L26）/ RT-R11-4（单元 B）：派生 age 超过 `self.max_evidence_age_ms`
    /// （由 `start_time_gate` 从 `config.max_evidence_age_ms` 注入，不再固定取默认阈值
    /// `DEFAULT_MAX_EVIDENCE_AGE_MS`）且快照为 `TimeQuality::NtpValid` 时，读取侧即时把返回值降级为
    /// `TimeQuality::SystemFallback`——driver 下一周期 tick 前也不得把超龄样本以 NtpValid 漏出；
    /// 任何自定义有效阈值（如 180s）与默认 300s 均即时降级；只降级返回值、不回写快照，
    /// 持久降级、attempt 复位与审计仍由 driver 契约 2 承接。
    pub async fn current(&self) -> TimeEvidence {
        // RT-R12-1：evidence 与 sample_instant 在同一读锁内原子克隆，派生 age 必对该证据配对的
        // 锚点计算，杜绝旧 reference 配新锚点的竞态。
        let snap = self.snapshot.read().await.clone();
        let mut evidence = snap.evidence;
        if let Some(instant) = snap.sample_instant {
            let elapsed_ms = tokio::time::Instant::now().duration_since(instant).as_millis() as u64;
            let derived_age_ms = evidence.sample_age_ms.unwrap_or(0).saturating_add(elapsed_ms);
            evidence.sample_age_ms = Some(derived_age_ms);
            if derived_age_ms > self.max_evidence_age_ms
                && matches!(evidence.quality, TimeQuality::NtpValid)
            {
                evidence.quality = TimeQuality::SystemFallback;
            }
        }
        // RT-R12-3（05 规格 L26–30）：快照质量处于 SystemFallback/Stale，或刚被动态降级为
        // SystemFallback 时，把返回值刷新为当前系统墙钟，杜绝时间冻结与排队寿命提前消耗。
        if matches!(evidence.quality, TimeQuality::SystemFallback | TimeQuality::Stale) {
            let now_wall = Utc::now();
            evidence.system_wall_utc = now_wall;
            evidence.reference_utc = now_wall;
        }
        evidence
    }
    pub fn background_retry_active(&self) -> bool { self.retry_active.load(Ordering::SeqCst) }
}

/// 可编译 RED 桩：直接以 Stale 置 ready，既不查询也不退避。
pub fn start_time_gate(config: TimeGateConfig, clock_epoch: Uuid) -> TimeGateHandle {
    let now = Utc::now();
    TimeGateHandle {
        // RT-R12-1：证据与锚点合并为单一原子快照；RT-R8-1：尚无采样成功，锚点为空；
        // start_time_gate 启动采样成功后在同一写锁内原子写 evidence 与 sample_instant（见 Step 3 第 6 条）。
        snapshot: Arc::new(RwLock::new(TimeGateSnapshot {
            evidence: TimeEvidence {
                system_wall_utc: now, reference_utc: now,
                quality: TimeQuality::Stale, clock_epoch,
                source: None, sample_age_ms: None, offset_ms: None, uncertainty_ms: None,
            },
            sample_instant: None,
        })),
        ready: Arc::new(tokio::sync::Notify::new()),
        is_ready: Arc::new(AtomicBool::new(true)),
        retry_attempt: Arc::new(AtomicU32::new(0)),
        retry_active: Arc::new(AtomicBool::new(false)),
        // RT-R11-4（单元 B）：注入配置阈值，current() 据此动态判定降级。
        max_evidence_age_ms: config.max_evidence_age_ms,
    }
}

/// 可编译 RED 桩：忽略全部参数恒返回 `None`（未获得样本），且不绑定 socket、不轮询 DNS、不等待 deadline——
/// 启动总预算断言（正例 `Some`、等待至 deadline、pending DNS 被取消、多源不扩容、空源有界等待）全部 RED。
pub async fn run_startup_sample(
    _bind: crate::time::socket::SntpBindFuture,
    _resolver: &dyn crate::time::socket::SntpResolver,
    _servers: &[String],
    _request_timeout: Duration,
    _budget: Duration,
    _max_rtt_ms: i64,
    _now: tokio::time::Instant,
) -> Option<(crate::time::socket::NtpSample, String)> {
    None
}
```

在 `crates/rsetup-controller/tests/gate_udp.rs` 编写**真实请求关联**与门禁边界测试。测试使用受控 fake 响应构造（标准 48B NTP 报文，无需真实网络）：

```rust
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use chrono::{DateTime, TimeZone, Utc};
use rsetup_controller::time::gate::{
    DEFAULT_NTP_MAX_RTT_MS, NTP_STARTUP_BUDGET, gate_should_close, next_backoff,
    select_request_timeout,
};
use rsetup_controller::time::ntp::NtpError;
use rsetup_controller::time::socket::{
    LocalStamp, NtpCorrelationRequest, SntpResolver, correlate_sntp_response, NTP_EPOCH_OFFSET_MS,
};

const REQUEST_TX_NONCE: u64 = 0xd00d_cafe_1234_5678; // 64位唯一发送时戳/nonce

struct FixedResolver(SocketAddr);
impl SntpResolver for FixedResolver {
    fn resolve<'a>(&'a self, _host: &'a str) -> rsetup_controller::time::socket::SntpResolveFuture<'a> {
        Box::pin(async move { Ok(self.0) })
    }
}

/// 构造标准的 48 字节 SNTP 服务器响应。
/// Originate（字节 24..32）全量回显请求发送时戳/nonce；Receive (32..40) 与 Transmit (40..48) 为服务端时钟。
fn make_server_response(
    echoed_originate: u64,
    receive_timestamp: u64,
    transmit_timestamp: u64,
) -> [u8; 48] {
    let mut packet = [0u8; 48];
    packet[0] = (0u8 << 6) | (4u8 << 3) | 4u8; // LI=0, VN=4, Mode=4 (Server)
    packet[1] = 2; // stratum
    packet[12..16].copy_from_slice(b"LOCL"); // reference id
    packet[24..32].copy_from_slice(&echoed_originate.to_be_bytes());
    packet[32..40].copy_from_slice(&receive_timestamp.to_be_bytes());
    packet[40..48].copy_from_slice(&transmit_timestamp.to_be_bytes());
    packet
}

fn request(server_addr: SocketAddr, tx_nonce: u64, t1_wall: DateTime<Utc>, t1_mono: Instant) -> NtpCorrelationRequest {
    NtpCorrelationRequest {
        server_addr,
        request_tx: tx_nonce,
        t1_wall_utc: t1_wall,
        t1_mono,
    }
}

#[test]
fn sntp_response_from_unrequested_source_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let wrong_addr: SocketAddr = "203.0.113.7:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    // 构造合法的服务器报文
    let packet = make_server_response(REQUEST_TX_NONCE, 0x0001_0000_0000_0000, 0x0001_0000_0000_0001);
    let t4 = LocalStamp { wall_utc: t1_wall + chrono::Duration::milliseconds(10), mono: t1_mono + Duration::from_millis(10) };
    let err = correlate_sntp_response(&packet, &req, wrong_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::SourceMismatch)));
}

#[test]
fn sntp_response_with_mismatched_originate_nonce_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    // Originate 错位 1 比特或回显他方时戳
    let packet = make_server_response(REQUEST_TX_NONCE ^ 0x1, 0x0001_0000_0000_0000, 0x0001_0000_0000_0001);
    let t4 = LocalStamp { wall_utc: t1_wall + chrono::Duration::milliseconds(10), mono: t1_mono + Duration::from_millis(10) };
    let err = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::OriginateMismatch)));
}

#[test]
fn sntp_response_with_zero_receive_timestamp_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    let packet = make_server_response(REQUEST_TX_NONCE, 0, 0x0001_0000_0000_0001);
    let t4 = LocalStamp { wall_utc: t1_wall + chrono::Duration::milliseconds(10), mono: t1_mono + Duration::from_millis(10) };
    let err = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::ZeroReceiveTimestamp)));
}

#[test]
fn correlated_sample_is_accepted_despite_large_local_wall_offset() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    // 本地墙钟严重漂移（如比真实时间落后 10 年），只要关联正确、RTT 合格，仍必须接受样本
    let skewed_wall = Utc.timestamp_opt(1_420_070_400, 0).unwrap(); // 2015-01-01
    let req = request(target_addr, REQUEST_TX_NONCE, skewed_wall, t1_mono);

    // 服务端时间：2026-10-16（NTP 秒约 3_999_999_999）
    let server_ntp_sec = 3_999_999_999u64;
    let t2_raw = (server_ntp_sec << 32) | 0x4000_0000; // +250ms
    let t3_raw = (server_ntp_sec << 32) | 0x8000_0000; // +500ms
    let packet = make_server_response(REQUEST_TX_NONCE, t2_raw, t3_raw);

    // 往返单调耗时 50ms（合格）
    let t4 = LocalStamp {
        wall_utc: skewed_wall + chrono::Duration::milliseconds(50),
        mono: t1_mono + Duration::from_millis(50),
    };

    let sample = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS)
        .expect("correlated sample must be accepted even with large wall skew");
    assert_eq!(sample.rtt_ms, 50, "rtt measured by monotonic span");
    assert!(sample.offset_ms > 300_000_000 * 1000, "large positive offset expected due to 10yr skew");
}

#[test]
fn rtt_over_local_monotonic_bound_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    let packet = make_server_response(REQUEST_TX_NONCE, 0x0001_0000_0000_0000, 0x0001_0000_0000_0001);
    // 单调耗时 2500ms，超过 DEFAULT_NTP_MAX_RTT_MS (2000ms)
    let t4 = LocalStamp {
        wall_utc: t1_wall + chrono::Duration::milliseconds(2500),
        mono: t1_mono + Duration::from_millis(2500),
    };
    let err = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::RttTooLarge)));
}

#[test]
fn gate_budget_is_enforced_at_30_seconds() {
    // 00 §3 C-TIME 已确认约束：预算 30s 是强制上限，未取得关联样本也必须关闭。
    assert!(!gate_should_close(Duration::from_millis(29_999), NTP_STARTUP_BUDGET, false));
    assert!(gate_should_close(Duration::from_secs(30), NTP_STARTUP_BUDGET, false));
    assert!(gate_should_close(Duration::from_secs(31), NTP_STARTUP_BUDGET, false));
    // 关联成功可提前关闭
    assert!(gate_should_close(Duration::from_millis(5), NTP_STARTUP_BUDGET, true));
}

#[test]
fn background_backoff_is_bounded_and_monotonic() {
    assert_eq!(next_backoff(0), Duration::from_secs(1));
    assert_eq!(next_backoff(1), Duration::from_secs(2));
    assert_eq!(next_backoff(2), Duration::from_secs(4));
    assert!(next_backoff(6) <= Duration::from_secs(60));
    assert_eq!(next_backoff(30), Duration::from_secs(60));
}

#[test]
fn single_request_timeout_selection_caps_at_3s_and_clamps_to_remaining() {
    // 表驱动证明选择规则：min(configured, 05 §2 建议 3s 上限, 剩余预算)。
    // 规格 05 §2 原文是建议（非强制契约），本计划明确采用该建议作为实现策略。
    // 本纯 helper 由真实 `run_startup_sample` 消费（Step 3）；真实请求超时证据在
    // `startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger`
    // （事件驱动本地 pending/多源路径），不单以纯 helper 充当真实超时证据。
    let cases = [
        // (configured, remaining, expected)
        (Duration::from_secs(10), Duration::from_secs(30), Duration::from_secs(3)),    // configured > 3s：裁到 3s，不突破
        (Duration::from_secs(3), Duration::from_secs(30), Duration::from_secs(3)),     // configured == 上限：保持
        (Duration::from_secs(1), Duration::from_secs(30), Duration::from_secs(1)),     // configured < 3s：不放大
        (Duration::from_secs(10), Duration::from_millis(500), Duration::from_millis(500)), // 剩余 < 3s：用剩余
        (Duration::from_secs(1), Duration::from_millis(200), Duration::from_millis(200)),  // 剩余 < configured < 3s：用剩余
        (Duration::from_secs(10), Duration::ZERO, Duration::ZERO),                     // 剩余耗尽：0
        (Duration::ZERO, Duration::from_secs(10), Duration::ZERO),                     // configured 为 0：0
    ];
    for (configured, remaining, expected) in cases {
        assert_eq!(
            select_request_timeout(configured, remaining),
            expected,
            "configured={configured:?} remaining={remaining:?}",
        );
    }
}

/// 启动总预算 `run_startup_sample` 受控测试：注入 bind/DNS future + 进程内 fake UDP 服务器，无外部 NTP、无新增依赖。
/// 时间控制 = 真实时间 + 短预算（300ms）+ `#[tokio::test]` 当前线程运行时（创建运行时并 block_on 测试 future，不自动设默认超时）：若实现忙循环（无 await）会独占唯一 worker 并阻塞同线程 timer，不能自己检测、可能永久卡住；忙循环检测须另有进程级外部 watchdog（强制终止 + 清理前置，见 Step 2/Step 4 规范：执行命令须以 `timeout --kill-after=5s 35s` 等外部看门狗包装器拉起，超时强制杀灭并以非 0 退出码记 FAIL；实际命令包装留待执行阶段落实，本计划切片不冒称测试已运行）；不靠任意 sleep 推锁序。
/// `startup_single_request_never_exceeds_3s_cap_when_configured_larger` 是本模块唯一 3s 量级用例（选定的单次上限本身为 3s，必须真实时间运行约 3s）；其余用例仍为 300ms 短预算。
/// 前置：本计划已声明的 tokio dev-dependency（workspace 特性 + `test-util`，见协作点 9；`#[tokio::test]` 既有 `main.rs` 已用、`net` 为 Task 1 生产代码既有前置）；缺特性属 manifest 缺口而非本段 RED；本模块不用 `start_paused`。
mod startup_budget {
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use chrono::Utc;
    use rsetup_controller::time::gate::{run_startup_sample, DEFAULT_NTP_MAX_RTT_MS};
    use rsetup_controller::time::ntp::NtpError;
    use rsetup_controller::time::socket::{SntpBindFuture, SntpResolveFuture, SntpResolver, NTP_EPOCH_OFFSET_MS};

    const BUDGET: Duration = Duration::from_millis(300); // 可测试短预算；生产默认 30s 不变
    const PER_REQ: Duration = Duration::from_secs(3);    // 配置的单次请求超时（此组用例中 == 3s 上限）；实际选择为 min(configured, 3s 上限, remaining)，此处总被 min 裁到 remaining
    const SLOW_CI_SLACK: Duration = Duration::from_millis(500);

    /// 被 drop 时标记 “DNS future 已被取消”。
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
    }

    /// 可注入 DNS future 选择器：`Resolve` 立即解析；`Pending` 永不完成（带 drop 标记）。
    #[derive(Clone, Copy)]
    enum DnsBehavior { Resolve, Pending }

    struct SpyResolver {
        behavior: DnsBehavior,
        server_addr: SocketAddr,
        calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    impl SntpResolver for SpyResolver {
        fn resolve<'a>(&'a self, _host: &'a str) -> SntpResolveFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                DnsBehavior::Resolve => {
                    let addr = self.server_addr;
                    Box::pin(async move { Ok(addr) })
                }
                DnsBehavior::Pending => {
                    let flag = self.dropped.clone();
                    Box::pin(async move {
                        let _guard = DropFlag(flag);
                        std::future::pending::<()>().await;
                        Err(NtpError::Io)
                    })
                }
            }
        }
    }

    /// 构造 spy resolver，返回查询 spy 与 drop 标记句柄。
    fn spy(behavior: DnsBehavior, addr: SocketAddr) -> (SpyResolver, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        (
            SpyResolver { behavior, server_addr: addr, calls: calls.clone(), dropped: dropped.clone() },
            calls,
            dropped,
        )
    }

    /// 受控 fake NTP 服务器：收 48B 请求（客户端地址取自 recv_from），全量回显 Originate，回标准 48B 响应（t3 = t2，本地短驻留标准响应）。
    async fn fake_ntp_server(sock: tokio::net::UdpSocket) {
        let mut buf = [0u8; 512];
        let (len, client) = sock.recv_from(&mut buf).await.expect("fake server recv");
        assert_eq!(len, 48, "请求必须是 48B SNTP 报文");
        let nonce = u64::from_be_bytes(buf[40..48].try_into().unwrap());
        let now_ms = Utc::now().timestamp_millis();
        let t2 = ((now_ms / 1000) as u64).wrapping_add(NTP_EPOCH_OFFSET_MS as u64 / 1000) << 32;
        // t3 = t2：本地短驻留标准响应（替代原先不合理的 t3 = t2 + 1s 人为 1s 驻留）；
        // SNTP 的 RTT 上限由本地 T1/T4 单调跨度严格度量（本计划已确立的 SNTP 模型）；
        // 规格 05 板端时间校验（board_clock）的 `rtt=(T4-T1)-(T3-T2)` 负 RTT 定义不混入本 SNTP 模型。
        let t3 = t2;
        let resp = super::make_server_response(nonce, t2, t3);
        sock.send_to(&resp, client).await.expect("fake server send");
    }

    /// 受控 bind future：本地 127.0.0.1 UDP socket，bind 计数 spy。
    fn bind_spy() -> (SntpBindFuture, Arc<AtomicUsize>) {
        let binds = Arc::new(AtomicUsize::new(0));
        let bind_calls = binds.clone();
        (
            Box::pin(async move {
                bind_calls.fetch_add(1, Ordering::SeqCst);
                tokio::net::UdpSocket::bind("127.0.0.1:0").await.map_err(|_| NtpError::Io)
            }),
            binds,
        )
    }

    /// 按源地址 DNS 选择器：`srv0` → 静默（绑定但永不响应），`srv1` → 应答 fake 服务器；
    /// 供“源 0 单次请求被 3s 上限裁剪、源 1 在预算内完成”的多源真实路径使用。
    struct SplitResolver {
        addrs: Vec<(String, SocketAddr)>,
        calls: Arc<AtomicUsize>,
    }

    impl SntpResolver for SplitResolver {
        fn resolve<'a>(&'a self, host: &'a str) -> SntpResolveFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let addr = self.addrs.iter().find(|(h, _)| h == host).map(|(_, a)| *a).unwrap();
            Box::pin(async move { Ok(addr) })
        }
    }

    #[tokio::test]
    async fn startup_success_returns_sample_and_source_before_deadline() {
        // 正例：首源立即返回合格样本 → 门禁必须提前返回，query spy > 0。
        let server_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            fake_ntp_server(server_sock).await;
        });
        let (bind, binds) = bind_spy();
        let (resolver, calls, _dropped) = spy(DnsBehavior::Resolve, server_addr);
        let servers = vec!["srv0".to_string(), "srv1".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(bind, &resolver, &servers, PER_REQ, BUDGET, DEFAULT_NTP_MAX_RTT_MS, now).await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        let (sample, source) = res.expect("deadline 前应获得合格样本");
        assert_eq!(source, "srv0", "样本来自首个源");
        assert!(sample.uncertainty_ms >= 1, "不确定度至少 1ms");
        assert!(calls.load(Ordering::SeqCst) > 0, "query spy 必须被调用");
        assert_eq!(binds.load(Ordering::SeqCst), 1, "socket 只绑定一次");
        assert!(elapsed < BUDGET, "成功路径必须提前返回，不等待整个预算");
        server_task.abort();
    }

    #[tokio::test]
    async fn startup_pending_dns_is_cancelled_by_total_budget() {
        // DNS 永不完成：必须被总预算取消（drop），门禁以 None 关闭。
        let (bind, _binds) = bind_spy();
        let (resolver, _calls, dropped) = spy(DnsBehavior::Pending, "127.0.0.1:123".parse().unwrap());
        let servers = vec!["srv0".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(bind, &resolver, &servers, PER_REQ, BUDGET, DEFAULT_NTP_MAX_RTT_MS, now).await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        assert!(res.is_none(), "pending DNS 在总预算内不可能产出样本");
        assert!(elapsed >= BUDGET, "必须等到绝对 deadline，不得提前返回");
        assert!(dropped.load(Ordering::SeqCst), "预算到期必须取消（drop）pending DNS future");
    }

    #[tokio::test]
    async fn startup_multi_source_total_budget_is_not_extended() {
        // 三源全 pending：若按源重建 deadline 则总时长 ≈ 3×3s；同一绝对 deadline 下总时长 ≈ BUDGET。
        let (bind, _binds) = bind_spy();
        let (resolver, calls, _dropped) = spy(DnsBehavior::Pending, "127.0.0.1:123".parse().unwrap());
        let servers = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(bind, &resolver, &servers, PER_REQ, BUDGET, DEFAULT_NTP_MAX_RTT_MS, now).await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        assert!(res.is_none());
        assert!(elapsed >= BUDGET, "首源占满剩余预算，门禁在 deadline 关闭");
        assert!(elapsed < BUDGET + SLOW_CI_SLACK, "总预算不得按源扩容");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "首源占满全部剩余预算，后续源不再查询");
    }

    #[tokio::test]
    async fn startup_empty_source_waits_deadline_without_busy_loop() {
        // 空源列表：必须有界等待（唯一 await）至绝对 deadline 后返回 None，不忙循环（若实现忙循环，当前线程运行时不能自检测；忙循环依赖独立进程级外部 watchdog 强制终止，见 Step 2/Step 4 规范包装器）。
        let (bind, binds) = bind_spy();
        let (resolver, calls, _dropped) = spy(DnsBehavior::Resolve, "127.0.0.1:123".parse().unwrap());

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(bind, &resolver, &[], PER_REQ, BUDGET, DEFAULT_NTP_MAX_RTT_MS, now).await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        assert!(res.is_none(), "空源列表视不可用，得不到样本");
        assert!(elapsed >= BUDGET, "空源必须等到绝对 deadline 再回退");
        assert!(elapsed < BUDGET + SLOW_CI_SLACK, "有界等待，不超发");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "空源不查询");
        assert_eq!(binds.load(Ordering::SeqCst), 1, "socket 只绑定一次，无每轮重绑（忙循环标记）");
    }

    #[tokio::test]
    async fn startup_single_request_never_exceeds_3s_cap_when_configured_larger() {
        // configured = 10s（> 05 §2 选定的 3s 上限），总预算 5s（> 上限），真实时间、无 paused clock：
        // 源 0 绑定但永不响应，源 1 为标准 fake 服务器（t3 = t2 短驻留响应）。
        // 若单次请求选择不裁上限，源 0 吞掉整个 5s 预算 → None；
        // 裁上限后源 0 在 ~3s 被截断，源 1 在剩余预算内完成 → Some("srv1")。
        // 本模块唯一 3s 量级用例（上限本身为 3s，需真实时间运行约 3s）；若实现忙循环（无 await）
        // 当前线程运行时独占唯一 worker 使本用例可能永久卡住（`#[tokio::test]` 创建运行时并 block_on
        // 测试 future，不自动设默认超时，同线程 timer 被阻塞不能自检测）；忙循环防御依赖独立进程级外部
        // watchdog（见 Step 2/Step 4 规范：执行阶段必须以 `timeout --kill-after=5s 35s` 包装，强制 SIGKILL 杀灭孤儿进程并以非 0 退出码记 FAIL）。
        let silent_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let silent_addr = silent_sock.local_addr().unwrap();
        let server_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            fake_ntp_server(server_sock).await;
        });
        let (bind, _binds) = bind_spy();
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = SplitResolver {
            addrs: vec![("srv0".to_string(), silent_addr), ("srv1".to_string(), server_addr)],
            calls: calls.clone(),
        };
        let servers = vec!["srv0".to_string(), "srv1".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(
            bind, &resolver, &servers,
            Duration::from_secs(10), Duration::from_secs(5),
            DEFAULT_NTP_MAX_RTT_MS, now,
        )
        .await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        let (_sample, source) = res.expect("源 0 单次请求被 3s 上限截断，源 1 必须在预算内成功");
        assert_eq!(source, "srv1", "样本必须来自源 1");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "每源恰好解析一次 DNS");
        assert!(elapsed >= Duration::from_secs(3), "源 0 单次请求必须真实等到 3s 上限被截断（上限生效而非 0）");
        assert!(elapsed < Duration::from_secs(3) + SLOW_CI_SLACK, "若不裁上限，源 0 会吞掉整个 5s 预算");
        server_task.abort();
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

**进程级外部 Watchdog 执行规范：**
由于 `#[tokio::test]` 当前线程运行时无默认超时，若实现存在无 await 忙循环会独占唯一 worker 线程并阻塞同线程 timer，导致测试进程永久挂起，故严禁依赖 DSH bash 超时或测试框架自测。在后续执行阶段，启动测试命令必须强制挂接进程级独立看门狗（如标准包装：`timeout --kill-after=5s 35s <cargo test 命令>`）：
- 硬上限 35s（覆盖 30s 启动预算 + 5s 容限；短预算测试亦可按需使用 `timeout --kill-after=2s 10s`），超时先发 SIGTERM，5s 后仍未退出则发 SIGKILL 强制杀掉测试进程组与孤儿进程；
- 故障截断语义：看门狗超时终止退出码必须为非 0（如 124/137），明确标记为 FAIL 阻断流水线，绝不可作为无响应挂起或静默通过。

（下述命令为执行基线，执行阶段按上述看门狗规范包装执行）：
```bash
cargo test --offline --locked -p rsetup-controller --test gate_udp sntp_response_from_unrequested_source_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp sntp_response_with_mismatched_originate_nonce_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp sntp_response_with_zero_receive_timestamp_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp correlated_sample_is_accepted_despite_large_local_wall_offset --
cargo test --offline --locked -p rsetup-controller --test gate_udp rtt_over_local_monotonic_bound_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp gate_budget_is_enforced_at_30_seconds --
cargo test --offline --locked -p rsetup-controller --test gate_udp background_backoff_is_bounded_and_monotonic --
cargo test --offline --locked -p rsetup-controller --test gate_udp single_request_timeout_selection_caps_at_3s_and_clamps_to_remaining --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_success_returns_sample_and_source_before_deadline --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_pending_dns_is_cancelled_by_total_budget --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_multi_source_total_budget_is_not_extended --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_empty_source_waits_deadline_without_busy_loop --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger --
```
（选择器说明：`startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger` 为嵌套模块**全名**过滤——裸名亦可子串匹配，此处用全名保证唯一；若追加 `--exact`，嵌套 module 选择器必须用该全名，裸名将匹配 0 用例。）
预期：十三条命令各**恰好执行 1 个用例**并因业务断言失败 —— 桩恒返回 `Ok(NtpSample { 0, 0, .. })`，因此 `SourceMismatch`、`OriginateMismatch`、`RttTooLarge` 断言失败；`sntp_response_with_zero_receive_timestamp_is_rejected` 中 `ZeroReceiveTimestamp` 断言失败（桩只做 48B/时戳结构校验，不校验接收时戳非零）；`correlated_sample_is_accepted_despite_large_local_wall_offset` 中 `offset > 300_000_000` 断言失败；`gate_should_close` 桩恒 `true`，因此 `29_999ms` 断言失败；`next_backoff` 桩恒 1s，因此 `2s/4s/60s` 断言失败；`select_request_timeout` 桩恒返回 `configured`（忽略 3s 上限与 remaining），因此表驱动用例 `(10s,30s)→3s`、`(10s,500ms)→500ms`、`(1s,200ms)→200ms`、`(10s,0)→0` 失败（其余 3 条用例桩下即过，用例集合非 0 条、非常真）；`run_startup_sample` 桩恒返回 `None` 且立即返回（不绑 socket、不轮询 DNS、不等待 deadline），因此正例 `res.expect(...)` 失败（得到 None）、pending DNS/多源/空源三条的 `elapsed >= BUDGET` 断言失败（提前返回）、drop 标记与查询 spy 断言失败（DNS future 从未被轮询/丢弃、查询次数 0）、`startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger` 的第一处失败断言即 `res.expect(...)`（桩恒 `None`）。**不是编译错误，也不是 0 用例。**

- [ ] **Step 3: 最小实现**

1. `correlate_sntp_response` 按严格优先级 fail-closed 实现：
```rust
use chrono::TimeZone;

pub fn correlate_sntp_response(
    packet: &[u8],
    request: &NtpCorrelationRequest,
    response_source: SocketAddr,
    t4: LocalStamp,
    max_rtt_ms: i64,
) -> Result<NtpSample, NtpError> {
    // 1) 48B 纯结构（导入自 parser 分支，绝不升级为 NtpValid）
    crate::time::ntp::parse_sntp_response(packet)?;
    // 2) 响应源必须是请求实际解析/发送到的 SocketAddr（禁止与 host 字符串盲目比对）
    if response_source != request.server_addr {
        return Err(NtpError::SourceMismatch);
    }
    // 3) 请求 T1 的 64 位唯一发送时戳/nonce 仅由响应 Originate 全量回显关联
    let originate = u64::from_be_bytes(packet[24..32].try_into().unwrap());
    if originate != request.request_tx {
        return Err(NtpError::OriginateMismatch);
    }
    // 4) 响应 T2/T3 是服务端时间，不得当作回显；T2 必须非零
    let t2_raw = u64::from_be_bytes(packet[32..40].try_into().unwrap());
    let t3_raw = u64::from_be_bytes(packet[40..48].try_into().unwrap());
    if t2_raw == 0 {
        return Err(NtpError::ZeroReceiveTimestamp);
    }
    // 5) RTT 上限由本地 T1/T4 单调跨度（monotonic span）严格度量
    let rtt_duration = t4.mono.checked_duration_since(request.t1_mono)
        .ok_or(NtpError::TimestampOverflow)?;
    let rtt_ms = i64::try_from(rtt_duration.as_millis())
        .map_err(|_| NtpError::TimestampOverflow)?;
    if max_rtt_ms < 0 || rtt_ms > max_rtt_ms {
        return Err(NtpError::RttTooLarge);
    }
    // 6) RFC 四时戳公式：((T2 - T1) + (T3 - T4)) / 2，UTC/NTP epoch checked i128 精确换算
    let t1_ms = to_ntp_epoch_ms(request.t1_wall_utc);
    let t2_ms = ntp_u64_to_ms(t2_raw);
    let t3_ms = ntp_u64_to_ms(t3_raw);
    let t4_ms = to_ntp_epoch_ms(t4.wall_utc);

    let offset_ms_i128 = ((t2_ms.checked_sub(t1_ms).ok_or(NtpError::TimestampOverflow)?)
        .checked_add(t3_ms.checked_sub(t4_ms).ok_or(NtpError::TimestampOverflow)?)
        .ok_or(NtpError::TimestampOverflow)?) / 2;
    let offset_ms = i64::try_from(offset_ms_i128).map_err(|_| NtpError::TimestampOverflow)?;

    let ref_unix_ms = t3_ms.checked_sub(NTP_EPOCH_OFFSET_MS).ok_or(NtpError::TimestampOverflow)?;
    let ref_unix_ms_i64 = i64::try_from(ref_unix_ms).map_err(|_| NtpError::TimestampOverflow)?;
    let reference_utc = Utc.timestamp_millis_opt(ref_unix_ms_i64)
        .single()
        .ok_or(NtpError::TimestampOverflow)?;

    Ok(NtpSample {
        offset_ms,
        rtt_ms,
        reference_utc,
        uncertainty_ms: u64::try_from((rtt_ms / 2).max(1))
            .map_err(|_| NtpError::TimestampOverflow)?,
    })
}

fn to_ntp_epoch_ms(wall: DateTime<Utc>) -> i128 {
    (wall.timestamp_millis() as i128) + NTP_EPOCH_OFFSET_MS
}

fn ntp_u64_to_ms(raw: u64) -> i128 {
    let sec = (raw >> 32) as i128;
    let frac = (raw & 0xffff_ffff) as i128;
    (sec * 1000) + ((frac * 1000) / (1i128 << 32))
}
```
2. `write_sntp_request`、异步生产DNS及`query_sntp`完整计划代码（socket.rs，与Step1同名桩替换，不同时保留两定义）：

```rust
pub fn write_sntp_request(buffer: &mut [u8; 48], nonce: u64) {
    buffer.fill(0);
    buffer[0] = (4 << 3) | 3;
    buffer[40..48].copy_from_slice(&nonce.to_be_bytes());
}

pub struct TokioSntpResolver;
impl SntpResolver for TokioSntpResolver {
    fn resolve<'a>(&'a self, host: &'a str) -> SntpResolveFuture<'a> {
        Box::pin(async move {
            let mut addresses = tokio::net::lookup_host(host).await.map_err(|_| NtpError::Io)?;
            addresses.next().ok_or(NtpError::Io)
        })
    }
}

pub async fn query_sntp(
    socket: &tokio::net::UdpSocket,
    resolver: &dyn SntpResolver,
    server: &str,
    request_nonce: u64,
    timeout: Duration,
    max_rtt_ms: i64,
) -> Result<NtpSample, NtpError> {
    // 单次预算包住DNS、send、recv整个future，绝不每个阶段重置预算。
    tokio::time::timeout(timeout, async {
        let target_addr = resolver.resolve(server).await?;
        let mut packet = [0u8; 48];
        write_sntp_request(&mut packet, request_nonce);
        let request = NtpCorrelationRequest {
            server_addr: target_addr,
            request_tx: request_nonce,
            t1_wall_utc: Utc::now(),
            t1_mono: Instant::now(),
        };
        let sent = socket.send_to(&packet, target_addr).await.map_err(|_| NtpError::Io)?;
        if sent != packet.len() { return Err(NtpError::Io); }
        let mut response = [0u8; 512];
        let (length, source) = socket.recv_from(&mut response).await.map_err(|_| NtpError::Io)?;
        let t4 = LocalStamp { wall_utc: Utc::now(), mono: Instant::now() };
        correlate_sntp_response(&response[..length], &request, source, t4, max_rtt_ms)
    }).await.map_err(|_| NtpError::Io)?
}
```

3. 启动总预算函数 `run_startup_sample`（gate.rs，替换 Step 1 同名桩；绝对 deadline **只创建一次**，socket bind/DNS/多源/全部重试同处一个绝对 `timeout_at` 之下）。gate.rs 需新增 `use std::sync::atomic::AtomicU64;`（`Ordering`、`Duration`、`NTP_STARTUP_BUDGET` 已在既有导入/同文件内），完整计划代码如下：

```rust
use std::sync::atomic::AtomicU64;
use crate::time::socket::{NtpSample, SntpBindFuture, SntpResolver, query_sntp};

/// 64位唯一发送时戳/nonce：当前墙钟纳秒与进程内唯一序列混合，实践中不重复（与 query_sntp 的 request_nonce 同参）。
fn startup_nonce() -> u64 {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ SEQ.fetch_add(0xBF58_476D_1CE4_E5B9, Ordering::Relaxed)
}

/// 单次请求超时选择：`min(configured, 05 §2 建议 3s 上限, 剩余总预算)`。
/// 规格 05 §2 原文“建议单次请求上限3s”是建议、非强制契约；本计划明确采用该建议作为实现策略：
/// configured > 3s 不放大突破上限、剩余 < 3s 用剩余、configured < 3s 不放大。
pub fn select_request_timeout(configured: Duration, remaining: Duration) -> Duration {
    configured.min(NTP_SINGLE_REQUEST_TIMEOUT_CAP).min(remaining)
}

/// 启动 NTP 总预算采样（启动路径只调用一次；后台重读/重试是其他任务）：
/// 绝对 deadline 只创建一次 = now + min(budget, NTP_STARTUP_BUDGET)（硬 ≤30s），socket bind、DNS、多源与全部重试
/// 都在同一个 `timeout_at(deadline, ...)` 内完成；每次请求超时由 `select_request_timeout(request_timeout, remaining)`
/// 选定 = min(configured, 05 §2 建议 3s 上限, remaining)，
/// 不按源/阶段重建 deadline；空源或一轮全部失败有界等待至 deadline（不忙循环）。
/// 成功返回 (样本, 产生该样本的源服务器字符串)，由调用方写 NtpValid `TimeEvidence`；
/// 超时/空源/全部失败返回 `None`，由调用方回退 `SystemFallback`。
pub async fn run_startup_sample(
    bind: SntpBindFuture,
    resolver: &dyn SntpResolver,
    servers: &[String],
    request_timeout: Duration,
    budget: Duration,
    max_rtt_ms: i64,
    now: tokio::time::Instant,
) -> Option<(NtpSample, String)> {
    // 绝对 deadline 只创建一次：budget 硬封顶 30s，之后绝不按源/阶段重建。
    let deadline = now + budget.min(NTP_STARTUP_BUDGET);

    match tokio::time::timeout_at(deadline, async move {
        let socket = bind.await.ok()?; // socket bind 也在同一绝对 deadline 内
        for server in servers {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            // 单次请求 = min(configured, 05 §2 建议 3s 上限, 剩余预算)
            let per = select_request_timeout(request_timeout, remaining);
            match query_sntp(&socket, resolver, server, startup_nonce(), per, max_rtt_ms).await {
                Ok(sample) => return Some((sample, server.clone())),
                Err(_) => continue,
            }
        }
        // 空源或一轮全部失败：有界等待至绝对 deadline（唯一 await，不忙循环），重读节奏即退避节奏（后台重读是其他任务）。
        tokio::time::sleep_until(deadline).await;
        None
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => None, // 绝对 deadline 到期：本轮 DNS/发送/接收全部取消，调用方回退 SystemFallback
    }
}
```

后台 TimeGateHandle 持久状态/后台 driver（源列表重读、stale 复验、墙钟跳变失效、审计事件写入）**完整驱动实现规范与说明代码见下方补齐段落**；其实现与受控测试仍由其他后续任务交付；本条仍只交付启动总预算函数与其受控 DNS/UDP 测试，**不自报整个 Task 1 或验收账本条目 P-RT-05 完成**（本段为规范补齐，不当作整个 Task 1 已闭合）。

**后台 TimeGateHandle 完整驱动实现规范（`TimeGateDriver`）：** 以下为后台 driver 的完整实现契约与计划参考代码（源列表重读、stale 复验、单调退避、跳钟失效、脱敏审计、线程安全通知）。本规范属于本条 Task 1 的后台 driver 段落；执行阶段按此契约替换 Step 1 桩并补受控测试用例，不改变上方声明的启动总预算切片交付边界。

**实现契约（各条均须满足，且每条在执行阶段有对应受控测试用例）：**

1. **源列表重读：** 每个循环开头重读最新 NTP 源列表 `servers`（后台 driver 每周期从配置源重读源列表，而非启动时固化）；空列表或读取全部失败时与 `run_startup_sample` 的空源处理一致，做有界等待至本轮 deadline，不忙循环。
2. **周期复验（stale 复验）：** 若当前状态为 `TimeQuality::NtpValid`，等待 `config.revalidate_interval` 后重新触发一轮复验（复用 `run_startup_sample` 的总预算采样逻辑）；复验成功则刷新 `TimeEvidence`（更新 `offset_ms`/`uncertainty_ms`/`reference_utc` 并重置 `sample_age_ms = 0`）；复验失败则保留当前证据，但 `sample_age_ms` 超过 `config.max_evidence_age_ms` 时判定 stale，失效并降级 `SystemFallback` 且 `attempt = 0`，随后按退避继续重试。当前 quality 为 `TimeQuality::Stale`（含 `start_time_gate` 初始态）时同样视为不可信：维持当前 gate 状态、不降级放行，按 `config.revalidate_interval` 复验；复验失败且 `sample_age_ms` 超过 `config.max_evidence_age_ms` 时失效为 `SystemFallback`（与本条同一判定标准）。
3. **单调退避：** 处于 `TimeQuality::SystemFallback` 时，采样失败每轮 `attempt += 1`，等待 `next_backoff(attempt)`（即 `min(BACKOFF_MIN << attempt.min(6), BACKOFF_MAX)`）后重试；重试间隔严格单调不降、上限受 `BACKOFF_MAX` 控制，采样成功后 `attempt` 归零。
4. **跳钟失效：** 每轮采样前读取 `std::time::SystemTime::now()` 并与上一轮 `last_wall` 比较：若向后回跳超过 `JUMP_BACK_EPS`（默认 2s）或向前异常跳变超过 `JUMP_FORWARD_MAX`（默认 1h），立即使当前 `TimeEvidence` 失效（清空 `source`/`offset_ms`/`uncertainty_ms`、置 `quality = SystemFallback`），回退至 `SystemFallback` 且 `attempt = 0`，按退避继续重试；`last_wall` 无论是否检测到跳变，每轮读取后均更新。
5. **脱敏审计：** 每次状态变迁（`SystemFallback -> NtpValid` 恢复、`NtpValid -> SystemFallback` 降级/失效、`NtpValid -> NtpValid` 复验刷新、`Stale -> NtpValid` 复验恢复）均通过 `audit.record(...)` 写入一条时间审计事件；事件载荷只含 `kind`/`from`/`to`/`reason`/`offset_ms`/`uncertainty_ms`/`reference_utc`/`clock_epoch`/`attempt`（即下方 `TimeAuditEvent` 字段），不含 NTP 源原始地址、nonce、socket 端口等未脱敏信息；账本列的 `process_epoch`/`event_seq`/`time_evidence` 仍由唯一 producer `audit::insert_audit`（Identity 拥有）内部取值，driver 事件不携带、不另立第二 producer，与已闭合的 R1/R2 审计接线不冲突。
6. **线程安全与通知：** 权威状态 = Step 1 现有 `TimeGateHandle.snapshot: Arc<RwLock<TimeGateSnapshot>>` 快照实现（RT-R12-1 合并后字段，本契约权威，不改为 watch）；driver 持有与 handle 共享的 `snapshot`/`is_ready`/`ready` 的 `Arc` 克隆，每个 tick 开头 `snapshot.read().await` 读快照决定分支，每次变迁 `snapshot.write().await` 在同一写锁内原子写入更新后的 `evidence` 与 `sample_instant`；转 ready 时额外 `is_ready.store(true, SeqCst)` + `ready.notify_waiters()`（与 Step 3 第 6 条 `start_time_gate` 的语义一致）；`wait_ready` 采用先注册后复查模式（RT-R7-4）：每轮先创建 `ready.notified()` 监听器并 `enable()` 注册，再复查 `is_ready`——已 ready 立即返回（零等待），否则 await 该监听器（最长 30s 超时兜底返回当前快照），消除 Notify 不为未来 waiter 保留 permit 导致的检查-注册竞态丢通知；`current` 则保持直接读取快照。

**计划参考代码（planning code；所涉类型与函数以 Step 1 桩与本步已交付接口为准，执行阶段以最终代码替换）：**

```rust
/// 时间审计事件：每次状态变迁写入一条（契约 5），载荷脱敏，不含 NTP 源原始地址/nonce/socket 端口。
pub struct TimeAuditEvent {
    pub kind: &'static str,      // 事件种类："ntp_recovered" | "ntp_revalidated" | "stale_revalidated" | "evidence_stale" | "wall_clock_jump"
    pub from: TimeQuality,       // 变迁前 quality
    pub to: TimeQuality,         // 变迁后 quality
    pub reason: &'static str,    // 脱敏变迁原因（通常与 kind 一致）
    pub offset_ms: Option<i64>,
    pub uncertainty_ms: Option<i64>,
    pub reference_utc: DateTime<Utc>,
    pub clock_epoch: Uuid,       // 进程时钟纪元，取自 TimeEvidence.clock_epoch（非账本 process_epoch）
    pub attempt: u32,            // 当前退避尝试计数
}

/// driver 侧审计 sink 抽象：账本唯一 producer 仍为 `audit::insert_audit`（audit.rs 归 Identity 拥有，
/// process_epoch/event_seq/time_evidence 由其内部取值，driver 不传）。生产装配用薄桥接实现把事件写入独立
/// 事务并调用该 producer（复用 R1/R2 审计接线，不新增 producer）；受控测试用内存 Vec sink 断言事件序列。
pub trait TimeAuditSink: Send + Sync {
    fn record(&self, event: TimeAuditEvent);
}

pub struct TimeGateDriver {
    /// RT-R12-1（单元 1）：与 TimeGateHandle 共享的权威快照（契约 6，Step 1 RwLock 实现）——
    /// 证据与锚点合并为单一 `TimeGateSnapshot`，启动采样与周期复验成功时在同一个 write 锁内
    /// 原子写入 `evidence` 与 `sample_instant = Some(Instant::now())`，杜绝双锁竞态；
    /// current() 读取时在同一读锁内原子取回两者并派生 sample_age_ms（age 随读取推进）。
    snapshot: Arc<RwLock<TimeGateSnapshot>>,
    is_ready: Arc<AtomicBool>,
    ready: Arc<Notify>,
    audit: Arc<dyn TimeAuditSink>,
    config: TimeGateConfig,
    attempt: u32,                    // 退避尝试计数（契约 3）
    last_wall: Option<SystemTime>,   // 跳钟检测基线（契约 4）
}

impl TimeGateDriver {
    pub async fn run(mut self) {
        loop {
            if self.wall_jumped() {
                // RT-R8-1（规格 L44 跳钟换代）：跳钟即原时钟参考系彻底失效——不仅 invalidate 降级
                // SystemFallback，且重新生成 clock_epoch = Uuid::new_v4() 写入权威快照；旧 clock_epoch
                // 的 TimeEvidence 与一切期限/寿命比较（含 time_review 的 RT-R8-2 创建时证据核验）不得跨纪元继续信任。
                self.invalidate("wall_clock_jump").await;
                self.snapshot.write().await.evidence.clock_epoch = Uuid::new_v4();
            } // 对比 last_wall：回跳 > JUMP_BACK_EPS 或前跳 > JUMP_FORWARD_MAX（契约 4）
            let servers = self.read_source_list().await;                  // 源列表重读（契约 1）
            let quality = self.snapshot.read().await.evidence.quality;    // 每 tick 读快照定分支（契约 6）
            match quality {
                TimeQuality::NtpValid => {
                    tokio::time::sleep(self.config.revalidate_interval).await; // 周期复验（契约 2）
                    match self.resample(servers).await {
                        Some((sample, source)) => self.transition(sample, source, "ntp_revalidated").await,
                        None if self.evidence_stale() => self.invalidate("evidence_stale").await,
                        None => {} // 保留当前证据，下轮再复验
                    }
                }
                TimeQuality::Stale => {
                    // Stale 视为不可信：维持当前 gate 状态（不放行、不降级放行），按 revalidate_interval 复验；
                    // 复验失败且证据时效超 max_evidence_age_ms 才失效为 SystemFallback（与契约 2 同一判定）
                    tokio::time::sleep(self.config.revalidate_interval).await;
                    match self.resample(servers).await {
                        Some((sample, source)) => self.transition(sample, source, "stale_revalidated").await,
                        None if self.evidence_stale() => self.invalidate("evidence_stale").await,
                        None => {} // 维持 Stale，下轮再复验
                    }
                }
                TimeQuality::SystemFallback => {
                    self.attempt += 1;
                    tokio::time::sleep(next_backoff(self.attempt)).await; // 单调退避，上限 BACKOFF_MAX（契约 3）
                    match self.resample(servers).await {
                        Some((sample, source)) => {
                            self.attempt = 0;
                            self.transition(sample, source, "ntp_recovered").await;
                        }
                        None => {} // 失败则下轮按 attempt+1 继续退避
                    }
                }
            }
        }
    }
    // evidence_stale()（RT-R8-1 动态 age）：判定值 = 快照存储 sample_age_ms + 锚点经过时间
    // （last_sample_instant 为 Some 时取 Instant::now().duration_since(anchor).as_millis() as u64，
    // 为 None 时按 0），age 随读取动态推进；判定标准：派生值 > config.max_evidence_age_ms（契约 2/4）。
    // 测试说明：driver_evidence_over_max_age_invalidates_to_fallback 须断言复验失败后仅靠锚点经过时间
    // （无新写入）即令 age 越过 max_evidence_age_ms 并降级 SystemFallback、attempt = 0。
    // 测试说明（RT-R12-2 / RT-R13-2）：断言 NTP 来源切换（old_source != new_source）或同源阶跃校正
    // （|new_ref - 单调外推 expected_ref| > 1s，回跳与快进双向均须触发）时 transition 触发换代
    // （clock_epoch 更新为 Uuid::new_v4() 并写换代审计），同源且外推偏差 <= 1s 连续平滑时沿用旧
    // clock_epoch；换代后之前的 held 任务在 time_review 中因纪元不连续返回 Inconclusive 且零状态变更。
    // invalidate(reason)：写锁 snapshot（清空 evidence.source/offset_ms/uncertainty_ms、quality = SystemFallback）、attempt = 0，随后 audit.record（契约 5）
    // transition(sample, source, reason)：RT-R12-2 / RT-R13-2（05 规格 L22/L41 参考切换/不连续校正换代）
    //   ——更新 TimeEvidence 前先读旧快照，基于单调外推判定阶跃，杜绝同源负阶跃或正阶跃校正漏掉换代：
    //   let elapsed_ms = old_snapshot.sample_instant
    //       .map(|inst| tokio::time::Instant::now().duration_since(inst).as_millis() as i64)
    //       .unwrap_or(0);
    //   let expected_ref = old_snapshot.evidence.reference_utc + chrono::Duration::milliseconds(elapsed_ms);
    //   let step_ms = (new_ref - expected_ref).num_milliseconds().abs();
    //   let discontinuous = old_source != new_source || step_ms > 1000; // 偏差 > 1s 判定为不连续阶跃校正
    //   if discontinuous { clock_epoch = Uuid::new_v4(); }
    //   仅同源且 step_ms <= 1000（连续平滑）时沿用旧 clock_epoch。随后在同一个
    //   self.snapshot.write().await 写锁内原子写入更新后的
    //   evidence 与 sample_instant = Some(tokio::time::Instant::now())（契约 6，杜绝双锁竞态）
    //             → audit.record(TimeAuditEvent { kind, from, to, reason, ... })（契约 5）
    //             → 转 NtpValid 时额外 is_ready.store(true, SeqCst) + ready.notify_waiters()
}
```

**driver 契约受控测试计划（对应上方契约 1–6 与 Stale 分支；随实现 driver 的后续任务交付，本切片 driver 实现不存在，以下命令标记为 RED 不可运行，但步骤定义完整）：** 全部置于 `tests/gate_udp.rs` 的 `mod driver_contracts`（嵌套模块全名 selector；若追加 `--exact` 必须用全名），执行阶段按 Step 2/Step 4 进程级 watchdog 规范（`timeout --kill-after=5s 35s`）包装：

```bash
cargo test --offline --locked -p rsetup-controller --test gate_udp driver_contracts::driver_revalidates_at_revalidate_interval --
cargo test --offline --locked -p rsetup-controller --test gate_udp driver_contracts::driver_evidence_over_max_age_invalidates_to_fallback --
cargo test --offline --locked -p rsetup-controller --test gate_udp driver_contracts::driver_stale_is_untrusted_and_revalidated_without_release --
cargo test --offline --locked -p rsetup-controller --test gate_udp driver_contracts::driver_handle_current_matches_driver_snapshot --
cargo test --offline --locked -p rsetup-controller --test gate_udp driver_contracts::driver_emits_redacted_audit_event_on_each_transition --
cargo test --offline --locked -p rsetup-controller --test gate_udp driver_contracts::driver_wall_clock_jump_falls_back_with_monotonic_backoff --
```

覆盖映射：① `driver_revalidates_at_revalidate_interval` 覆盖契约 1+2（注入源列表变更，断言每轮重读源列表、`revalidate_interval` 到期恰好触发一次复验且成功复验重置 `sample_age_ms = 0`）；② `driver_evidence_over_max_age_invalidates_to_fallback` 覆盖契约 2（复验失败且 `sample_age_ms > max_evidence_age_ms` → 降级 `SystemFallback`、`attempt = 0`）；③ `driver_stale_is_untrusted_and_revalidated_without_release` 覆盖 Stale 分支（初始 `Stale` 不放行，按 `revalidate_interval` 复验，成功转 `NtpValid`、失败维持 `Stale`，时效超 `max_evidence_age_ms` 才失效为 `SystemFallback`）；④ `driver_handle_current_matches_driver_snapshot` 覆盖契约 6（每次变迁后 `TimeGateHandle::current()` 与 driver 写入的 `RwLock` 快照一致，转 ready 后 `wait_ready` 返回）；⑤ `driver_emits_redacted_audit_event_on_each_transition` 覆盖契约 5（内存 sink 断言事件序列与 `kind`/`from`/`to`/`reason`/`offset_ms`/`uncertainty_ms`/`reference_utc`/`clock_epoch`/`attempt` 字段，并断言载荷不含源原始地址）；⑥ `driver_wall_clock_jump_falls_back_with_monotonic_backoff` 覆盖契约 3+4（注入回跳 > `JUMP_BACK_EPS` 与前跳 > `JUMP_FORWARD_MAX` → 失效为 `SystemFallback`，重试间隔单调不降且封顶 `BACKOFF_MAX`）。GREEN 判据：六条命令各恰好执行 1 个用例、全部 PASS（未实际运行前不得记 PASS）。

`wait_ready` 就绪等待受控断言（RT-N4，配合契约 6）：① 提前唤醒——driver 尚未转 ready 时调用方 `wait_ready()` 不得立即返回 Stale 快照；测试任务先挂起 `wait_ready()`，待 driver 完成采样并执行 `is_ready.store(true)` + `ready.notify_waiters()` 后，断言调用方被唤醒提前返回（耗时显著小于 30s）且返回值为转 ready 后的 `NtpValid` 快照；② 超时兜底——driver 始终不转 ready 时，断言 `wait_ready()` 在 30s 超时后返回当前快照（`Stale`/`SystemFallback` 兜底证据）而非永久挂起；③ 已 ready 短路——先转 ready 再调用 `wait_ready()`，断言无等待直接返回，覆盖通知先于等待发出的 Notify 丢失通知场景，消灭"启动期间 DNS/采样在途时立即拿到 Stale 直接退出"问题（对应规格 05 §1 最长 ≤30s 等待就绪）；④ 检查-注册竞态（RT-R7-4）——并发挂起多个 `wait_ready()` 调用方后由 driver 执行 `is_ready.store(true)` + `ready.notify_waiters()`（通知落在检查之后、监听器注册之前的窗口），断言全部调用方均被唤醒提前返回 `NtpValid` 快照（耗时显著小于 30s），无一因丢通知死等超时。

4. `gate_should_close(elapsed, budget, correlated) = correlated || elapsed >= budget`（`>=` 实现 30s 处强制关闭）。
5. `next_backoff(attempt) = min(BACKOFF_MIN << attempt.min(6), BACKOFF_MAX)`。
6. `start_time_gate`（`run_startup_sample` 的启动侧消费者，**明确调用此函数**，每次进程启动只调用一次）：构造 bind future `Box::pin(async { tokio::net::UdpSocket::bind("0.0.0.0:0").await.map_err(|_| NtpError::Io) })`，调用 `run_startup_sample(bind, &TokioSntpResolver, &config.servers, config.request_timeout, config.startup_budget, config.max_rtt_ms, tokio::time::Instant::now())`；`Some((sample, source))` → 写 `TimeQuality::NtpValid` 的 `TimeEvidence`（`source = Some(source)`、`offset_ms = Some(sample.offset_ms)`、`uncertainty_ms = Some(sample.uncertainty_ms)`、`reference_utc = sample.reference_utc`、`sample_age_ms = Some(0)`）并 `is_ready = true` + `ready.notify_waiters()`；`None` → 写 `SystemFallback` 并进入后台退避。**startup gate 结束只表示 NTP 门结束**：ready 还需数据库可用、初始化完成、已恢复持久设备锁/未决任务并确定恢复模式（05 §1），不替代 DB/bootstrap/恢复的 ready。

- [ ] **Step 4: 运行并确认 GREEN**

（执行阶段同 Step 2 规范，必须挂接进程级外部 watchdog 包装器如 `timeout --kill-after=5s 35s` 启动，防止回归引入忙循环导致挂起）：
```bash
cargo test --offline --locked -p rsetup-controller --test gate_udp sntp_response_from_unrequested_source_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp sntp_response_with_mismatched_originate_nonce_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp sntp_response_with_zero_receive_timestamp_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp correlated_sample_is_accepted_despite_large_local_wall_offset --
cargo test --offline --locked -p rsetup-controller --test gate_udp rtt_over_local_monotonic_bound_is_rejected --
cargo test --offline --locked -p rsetup-controller --test gate_udp gate_budget_is_enforced_at_30_seconds --
cargo test --offline --locked -p rsetup-controller --test gate_udp background_backoff_is_bounded_and_monotonic --
cargo test --offline --locked -p rsetup-controller --test gate_udp single_request_timeout_selection_caps_at_3s_and_clamps_to_remaining --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_success_returns_sample_and_source_before_deadline --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_pending_dns_is_cancelled_by_total_budget --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_multi_source_total_budget_is_not_extended --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_empty_source_waits_deadline_without_busy_loop --
cargo test --offline --locked -p rsetup-controller --test gate_udp startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger --
```
（选择器说明同 Step 2：最后一条为嵌套模块全名过滤；若追加 `--exact` 必须用该全名。）
预期：十三条命令各执行 1 个用例，全部 PASS（共 13 用例 PASS，0 失败）。`startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger` 为 3s 量级用例（真实时间运行约 3s），其余用例均为 300ms 短预算。

- [ ] **Step 5: 静态检查与提交**

```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
git add crates/rsetup-controller/src/time/ntp.rs crates/rsetup-controller/src/time/socket.rs \
  crates/rsetup-controller/src/time/gate.rs crates/rsetup-controller/src/time/mod.rs \
  crates/rsetup-controller/src/config.rs crates/rsetup-controller/tests/gate_udp.rs
git commit -m "feat(controller): 关联式 SNTP 客户端与强制 30s 启动门禁实现"
```

---

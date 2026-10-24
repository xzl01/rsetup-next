//! 生产监听装配：真实 `TcpListener` → 已审 SQL 准入握手 → 记录层 → 会话 driver。
//!
//! 边界（务必先读）：
//!
//! * 只接受**已验签公钥**作为设备身份：握手返回的 `peer_public_key` 是唯一的登记键，
//!   绝不从 packet 的 deviceId/serial 或任何 payload 字段绑定身份。
//! * 准入真相只有 SQL：每次握手都经 [`SqlxAdmissionPolicy`] 读真实 `devices` 表（零缓存），
//!   本模块不缓存批准、不自建第二套准入判断。
//! * 未 ready 的 socket 直接关闭（不回 `SERVER_AUTH_RESPONSE`），符合 §4.5。
//! * 不监听 HTTP 端口：监听地址与私钥路径来自本模块**独立**的 [`TransportConfig`]，
//!   与 `ControllerConfig::listen_address` 无关，绝不复用/顶替。
//! * **有界资源（§4.6 第 1 条 / §8 第 5 条）**：accept 后、昂贵握手之前依次过三道闸——
//!   ① 全局握手速率令牌桶（[`HandshakeRateLimiter`]）② 单一来源未认证/挂起名额
//!   （[`SourceQuota`]）③ 全局半开名额（[`TransportManager::half_open_permit`]）。限额只能
//!   **拒新**，绝不按审批年龄或等待时长驱逐已接纳且健康的挂起连接。
//! * **本模块不做全量准入复查**：复查的唯一 owner 是
//!   [`TransportManager::spawn_admission_recheck`]（受控并发 + 单条期限 + 整轮上界 + 可取消）。
//!   旧实现在 accept 循环里 inline `await` 一条 SQL，慢库会把 accept 与关停一起堵死（审查 P1-9）。
//!
//! # 环境变量（本模块自带解析，未新增 `ControllerConfig` 字段）
//!
//! | 变量 | 默认值 | 含义 |
//! | --- | --- | --- |
//! | `RSETUP_TUNNEL_LISTEN` | `127.0.0.1:9443` | 板端接入监听地址 |
//! | `RSETUP_CONTROLLER_KEY_FILE` | **未设置＝不启用板端接入** | 中控长期身份 PKCS#8 种子文件（0600） |
//! | `RSETUP_TUNNEL_MAX_HALF_OPEN` | `256` | 半开（握手中/未 ready）连接全局上限 |
//! | `RSETUP_TUNNEL_MAX_SESSIONS` | `1024` | 已认证 ready 会话上限 |
//! | `RSETUP_TUNNEL_MAX_IDENTITIES` | `8192` | 新身份登记预算（只能收缩，不能超过硬上限） |
//! | `RSETUP_TUNNEL_DB_TIMEOUT_MS` | `5000` | 单次准入 SQL 临界区预算 |
//! | `RSETUP_TUNNEL_HANDSHAKES_PER_SEC` | `50` | 全局握手速率（令牌桶/秒） |
//! | `RSETUP_TUNNEL_HANDSHAKE_BURST` | `100` | 全局握手速率桶容量 |
//! | `RSETUP_TUNNEL_MAX_PENDING_PER_SOURCE` | `8` | 单一来源未认证/挂起连接上限 |
//! | `RSETUP_TUNNEL_ADMISSION_READ_TIMEOUT_MS` | `2000` | 单次准入真相读取预算 |
//! | `RSETUP_TUNNEL_ADMISSION_SWEEP_MS` | `10000` | 一轮全量准入复查总上界 |
//! | `RSETUP_TUNNEL_ADMISSION_RECHECK_INTERVAL_MS` | `30000` | 全量复查节拍 |
//! | `RSETUP_TUNNEL_POLL_PERIOD_MS` | `10000` | 轮询周期 |
//! | `RSETUP_TUNNEL_MAX_CONCURRENT_POLLS` | `128` | 在途轮询请求上限 |
//!
//! 解析口径（审查 P2-13）：
//! * **缺失**与**非法**是两回事：只有 `RSETUP_CONTROLLER_KEY_FILE` 完全不存在才表达「不启用
//!   板端接入」（[`TransportEnv::Disabled`]）；其它变量缺失取默认值，**存在但非法**（非 UTF-8、
//!   空串、非数字、0、超硬上界）一律 `Err(ListenerError::Config)`，由顶层失败关闭；
//! * 不存在 `.ok().and_then(...)` 式的「解析失败当没配」——旧实现会把非 UTF-8（`NotUnicode`）
//!   静默当成未配置，现在明确报错；
//! * 所有预算都有硬上界，并由 [`TransportConfig::validate`] 二次校验；连接任务上限是
//!   `max_half_open + max_sessions` 的**受检和**（[`ManagerConfig::connection_task_budget`]，
//!   溢出即报错，绝不 `saturating_add`）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use uuid::Uuid;

use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::{
    BusinessPayload, BusinessPayloadKind, TaskRecord, approved_codec_witness, decode_business,
};
use rsetup_tunnel::envelope::{StreamKind, TunnelPacket};
use rsetup_tunnel::handshake::{
    AdmissionPolicy, HandshakeError, HandshakeResult, ServerHandshakeConfig, server_handshake,
};
use rsetup_tunnel::identity::IdentityKey;
use rsetup_tunnel::secure_io::SecureIo;
use rsetup_tunnel::session::config::SessionConfig;
use rsetup_tunnel::session::driver::start_server_session;
use rsetup_tunnel::session::handle::{
    HandlerFuture, RequestContext, SessionCloseReason, SessionHandle, SessionHandler,
    SessionOutcome, SessionState, SystemSignal,
};
use rsetup_tunnel::session::types::{Reply, RequestError};

use crate::db::DbPool;
use crate::tasks::evidence::{ApplyOutcome, TaskEvidenceApplier};
use crate::transport::admission::{
    DEFAULT_DB_TIMEOUT, DEFAULT_MAX_IDENTITIES, MAX_IDENTITY_BUDGET, SqlxAdmissionPolicy,
};
use crate::transport::board_client::RpcReply;
use crate::transport::limits::{
    HandshakeRateLimiter, MAX_SOURCE_TABLE_ENTRIES, SOURCE_IDLE_TTL, SourcePermit, SourceQuota,
    SourceRateLimiter,
};
use crate::transport::manager::{
    CallError, CancellableAdmissionPolicy, ConnectionFuture, ConnectionState,
    DEFAULT_ADMISSION_READ_TIMEOUT, DeviceConnection, MAX_MANAGER_DURATION, ManagerConfig,
    NewSession, RegisterError, SystemNotice, TaskResultContext, TransportManager,
};

/// 关停宽限期余量：会话 driver 自身 `drain_timeout`（默认 5s，§6.6）之外再留 1s 收尾。
pub const SHUTDOWN_GRACE_MARGIN: Duration = Duration::from_secs(1);
/// 「信令 + 延迟切断」收尾任务的关停宽限期（任务本身至多 500ms + 一次入队）。
pub const CLOSE_OUT_GRACE: Duration = Duration::from_secs(2);
/// 03 §4.4 状态码 `UNSUPPORTED_ACTION`：中控不接板端主动 request。
pub const STATUS_UNSUPPORTED_ACTION: i32 = 1002;
/// 本切片唯一被承认的板端主动事件动作。
pub const ACTION_TASK_RESULT: &str = "device.task.result";

const DEFAULT_LISTEN: &str = "127.0.0.1:9443";
const ENV_LISTEN: &str = "RSETUP_TUNNEL_LISTEN";
const ENV_KEY_FILE: &str = "RSETUP_CONTROLLER_KEY_FILE";
const ENV_MAX_HALF_OPEN: &str = "RSETUP_TUNNEL_MAX_HALF_OPEN";
const ENV_MAX_SESSIONS: &str = "RSETUP_TUNNEL_MAX_SESSIONS";
const ENV_MAX_IDENTITIES: &str = "RSETUP_TUNNEL_MAX_IDENTITIES";
const ENV_DB_TIMEOUT_MS: &str = "RSETUP_TUNNEL_DB_TIMEOUT_MS";
const ENV_HANDSHAKES_PER_SEC: &str = "RSETUP_TUNNEL_HANDSHAKES_PER_SEC";
const ENV_HANDSHAKE_BURST: &str = "RSETUP_TUNNEL_HANDSHAKE_BURST";
const ENV_MAX_PENDING_PER_SOURCE: &str = "RSETUP_TUNNEL_MAX_PENDING_PER_SOURCE";
/// 单一来源握手速率（次/秒）的环境变量：与全局速率、来源并发名额**分开计数与原因**（审查补项）。
const ENV_MAX_HANDSHAKES_PER_SOURCE: &str = "RSETUP_TUNNEL_HANDSHAKES_PER_SOURCE";
/// 单一来源握手速率桶容量的环境变量。
const ENV_HANDSHAKE_SOURCE_BURST: &str = "RSETUP_TUNNEL_HANDSHAKE_SOURCE_BURST";
const ENV_ADMISSION_READ_TIMEOUT_MS: &str = "RSETUP_TUNNEL_ADMISSION_READ_TIMEOUT_MS";
const ENV_ADMISSION_SWEEP_MS: &str = "RSETUP_TUNNEL_ADMISSION_SWEEP_MS";
const ENV_ADMISSION_RECHECK_INTERVAL_MS: &str = "RSETUP_TUNNEL_ADMISSION_RECHECK_INTERVAL_MS";
const ENV_POLL_PERIOD_MS: &str = "RSETUP_TUNNEL_POLL_PERIOD_MS";
const ENV_MAX_CONCURRENT_POLLS: &str = "RSETUP_TUNNEL_MAX_CONCURRENT_POLLS";

/// 默认全局握手速率（次/秒）。取舍：远高于 1024 台设备按 §7 退避重连的稳态需求，又足以把
/// 握手洪水（每次含一段 SQL 临界区，是握手最贵的一段）削平在 50/s。
pub const DEFAULT_HANDSHAKES_PER_SEC: u64 = 50;
/// 默认全局握手速率桶容量。
pub const DEFAULT_HANDSHAKE_BURST: usize = 100;
/// 默认单一来源未认证/挂起上限。取舍：同一 NAT 后允许 8 台设备**同时**握手/挂起；名额在
/// ready 时释放，因此 NAT 后的 ready 设备数只受 `max_sessions` 约束（审查补项）。
pub const DEFAULT_MAX_PENDING_PER_SOURCE: usize = 8;
/// 默认**单一来源**握手速率（次/秒，审查补项：spec §4.6 要求「全局**及来源**握手速率」）。
/// 取舍：单来源正常重连远低于此，但足以把单一 NAT/单一攻击源的握手洪水削平。与并发名额
/// （[`DEFAULT_MAX_PENDING_PER_SOURCE`]）分开计量：名额管「同时在途多少条」，速率管「每秒新开多少条」。
pub const DEFAULT_MAX_HANDSHAKES_PER_SOURCE: u64 = 8;
/// 默认单一来源握手速率桶容量。
pub const DEFAULT_HANDSHAKE_SOURCE_BURST: usize = 8;
/// 默认一轮全量准入复查的总上界。
pub const DEFAULT_ADMISSION_SWEEP_DEADLINE: Duration = Duration::from_secs(10);
/// 默认全量复查节拍。
pub const DEFAULT_ADMISSION_RECHECK_INTERVAL: Duration = Duration::from_secs(30);
/// 默认轮询周期。
pub const DEFAULT_POLL_PERIOD: Duration = Duration::from_secs(10);
/// 默认轮询 tick 节奏（必须 < `poll_period`，见 `PollScheduler` 的「暂停」语义）。
pub const DEFAULT_POLL_TICK: Duration = Duration::from_millis(500);
/// 默认在途轮询请求上限。
pub const DEFAULT_MAX_CONCURRENT_POLLS: usize = 128;
/// 单次准入 SQL 临界区预算上界。
pub const MAX_DB_TIMEOUT: Duration = Duration::from_secs(600);
/// 一轮复查的节拍上界（超过即等于「不再复查」）。
pub const MAX_RECHECK_INTERVAL: Duration = Duration::from_secs(3_600);
/// 一轮复查并发度：1024 台既不串成一轮跑不完，也不齐发打爆连接池。
pub const ADMISSION_RECHECK_CONCURRENCY: usize = 16;

/// 装配错误：去敏固定文案，绝不含私钥材料、SQL 文本或对端可控内容。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ListenerError {
    #[error("transport config: {0}")]
    Config(String),
    #[error("identity key unavailable")]
    Identity,
    #[error("listen address unavailable")]
    ListenAddress,
}

/// 板端接入的环境装配结果（**缺失**与**非法**分开表达，审查 P2-13）。
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // 配置只在启动时构造一次；Box 化只增噪音
pub enum TransportEnv {
    /// 配置完整且合法：板端接入应启动。
    Enabled(TransportConfig),
    /// `RSETUP_CONTROLLER_KEY_FILE` **完全未设置**：这是明确的「不启用板端接入」语义，不是
    /// 配置错误。身份文件存在但不可读/权限不符属于 `Err`，**不**走这个分支。
    Disabled,
}

/// 生产监听的全部预算与身份（**同一份预算的唯一真源**，审查 P1-5）。
#[derive(Clone)]
pub struct TransportConfig {
    /// 板端接入监听地址。
    pub listen: SocketAddr,
    /// 中控长期身份密钥（只以路径来自环境变量；本模块不读取、不打印其内容）。
    pub identity: Arc<IdentityKey>,
    /// 管理器预算（半开/会话/复查期限/轮询）：**同一个结构**同时交给
    /// [`TransportManager::try_new`]、本模块的连接任务硬上界与轮询 driver。
    pub manager: ManagerConfig,
    /// 新身份登记预算（只收缩 [`MAX_IDENTITY_BUDGET`]）。
    pub max_identities: usize,
    /// 单次握手准入 SQL 的临界区预算。
    pub db_timeout: Duration,
    /// 全局握手速率（次/秒）。
    pub handshakes_per_sec: u64,
    /// 全局握手速率桶容量。
    pub handshake_burst: usize,
    /// 单一来源未认证/挂起连接上限（并发名额）。
    pub max_pending_per_source: usize,
    /// 单一来源握手速率（次/秒，审查补项）：与全局速率、来源并发名额分开计数/原因。
    pub max_handshakes_per_source: u64,
    /// 单一来源握手速率桶容量。
    pub handshake_source_burst: usize,
    /// 会话 driver 配置。
    pub session: SessionConfig,
    /// 关停时等待已接纳会话完成 GOAWAY 排空的宽限期。
    pub drain_grace: Duration,
    /// 关停时等待「信令 + 延迟切断」收尾任务的宽限期。
    pub close_out_grace: Duration,
}

impl std::fmt::Debug for TransportConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportConfig")
            .field("listen", &self.listen)
            .field("identity", &"IdentityKey(<redacted>)")
            .field("manager", &self.manager)
            .field("max_identities", &self.max_identities)
            .field("db_timeout", &self.db_timeout)
            .field("handshakes_per_sec", &self.handshakes_per_sec)
            .field("handshake_burst", &self.handshake_burst)
            .field("max_pending_per_source", &self.max_pending_per_source)
            .field("max_handshakes_per_source", &self.max_handshakes_per_source)
            .field("handshake_source_burst", &self.handshake_source_burst)
            .field("drain_grace", &self.drain_grace)
            .finish()
    }
}

impl TransportConfig {
    /// 从环境变量装配并要求板端接入可用（不可用即 `Err`；需要「缺失即不启用」语义的调用方
    /// 用 [`TransportEnv::from_env`]）。
    pub fn from_env() -> Result<Self, ListenerError> {
        match TransportEnv::from_env()? {
            TransportEnv::Enabled(config) => Ok(config),
            TransportEnv::Disabled => Err(ListenerError::Config(format!(
                "{ENV_KEY_FILE} is not set: board access is not enabled"
            ))),
        }
    }
}

impl TransportEnv {
    /// 从环境装配板端接入（进程启动只调用一次；同一份结果同时决定管理器预算与监听预算）。
    ///
    /// 审查 N5：`Disabled`（不启用）的唯一判据是 `RSETUP_CONTROLLER_KEY_FILE` **完全不存在**，
    /// 但这个判定的生效时机必须在**其它所有显式配置项严格校验之后**——否则一个非法的
    /// `RSETUP_TUNNEL_*`（空串、非数字、0、超硬上界、非 UTF-8、非法监听地址）会因为 key 未设置
    /// 而被静默吞成「未启用」。因此本函数先把 key 的存在性单独捕获（key 自身的非 UTF-8 仍即时
    /// 报错，那是「显式非法」不是「缺失」），再逐项校验其余变量；全部合法后才据 key 有无决定
    /// `Disabled` / `Enabled`。走 `Disabled` 时绝不加载身份、不生成密钥、不开监听。
    pub fn from_env() -> Result<Self, ListenerError> {
        // 只捕获存在性：`raw_env` 自身的 `Err` 表示 key 变量存在但非 UTF-8——那是显式非法值，
        // 必须即时失败，绝不降级成「当没配」。
        let key_file_raw = raw_env(ENV_KEY_FILE)?;
        // —— 以下把「与 key 无关」的显式项全部严格校验（缺失即默认，存在即合法或报错）——
        let listen = match raw_env(ENV_LISTEN)? {
            None => DEFAULT_LISTEN.parse().map_err(|_| {
                ListenerError::Config(format!("{ENV_LISTEN} default is not a socket address"))
            })?,
            Some(raw) => non_empty(raw, ENV_LISTEN)?.parse().map_err(|_| {
                ListenerError::Config(format!("{ENV_LISTEN} is not a socket address"))
            })?,
        };
        let manager = ManagerConfig {
            max_half_open: bounded_usize(&raw_env(ENV_MAX_HALF_OPEN)?, 256, 4_096)?,
            max_sessions: bounded_usize(&raw_env(ENV_MAX_SESSIONS)?, 1_024, 16_384)?,
            admission_recheck_interval: bounded_duration(
                &raw_env(ENV_ADMISSION_RECHECK_INTERVAL_MS)?,
                DEFAULT_ADMISSION_RECHECK_INTERVAL,
                MAX_RECHECK_INTERVAL,
            )?,
            admission_read_timeout: bounded_duration(
                &raw_env(ENV_ADMISSION_READ_TIMEOUT_MS)?,
                DEFAULT_ADMISSION_READ_TIMEOUT,
                MAX_DB_TIMEOUT,
            )?,
            admission_sweep_deadline: bounded_duration(
                &raw_env(ENV_ADMISSION_SWEEP_MS)?,
                DEFAULT_ADMISSION_SWEEP_DEADLINE,
                MAX_RECHECK_INTERVAL,
            )?,
            admission_recheck_concurrency: ADMISSION_RECHECK_CONCURRENCY,
            poll_period: bounded_duration(
                &raw_env(ENV_POLL_PERIOD_MS)?,
                DEFAULT_POLL_PERIOD,
                MAX_MANAGER_DURATION,
            )?,
            poll_tick_cadence: DEFAULT_POLL_TICK,
            max_concurrent_polls: bounded_usize(
                &raw_env(ENV_MAX_CONCURRENT_POLLS)?,
                DEFAULT_MAX_CONCURRENT_POLLS,
                2_048,
            )?,
        };
        let max_identities = bounded_usize(
            &raw_env(ENV_MAX_IDENTITIES)?,
            DEFAULT_MAX_IDENTITIES,
            MAX_IDENTITY_BUDGET,
        )?;
        let db_timeout = bounded_duration(
            &raw_env(ENV_DB_TIMEOUT_MS)?,
            DEFAULT_DB_TIMEOUT,
            MAX_DB_TIMEOUT,
        )?;
        let handshakes_per_sec = bounded_u64(
            &raw_env(ENV_HANDSHAKES_PER_SEC)?,
            DEFAULT_HANDSHAKES_PER_SEC,
            100_000,
        )?;
        let handshake_burst = bounded_usize(
            &raw_env(ENV_HANDSHAKE_BURST)?,
            DEFAULT_HANDSHAKE_BURST,
            100_000,
        )?;
        let max_pending_per_source = bounded_usize(
            &raw_env(ENV_MAX_PENDING_PER_SOURCE)?,
            DEFAULT_MAX_PENDING_PER_SOURCE,
            1_024,
        )?;
        let max_handshakes_per_source = bounded_u64(
            &raw_env(ENV_MAX_HANDSHAKES_PER_SOURCE)?,
            DEFAULT_MAX_HANDSHAKES_PER_SOURCE,
            100_000,
        )?;
        let handshake_source_burst = bounded_usize(
            &raw_env(ENV_HANDSHAKE_SOURCE_BURST)?,
            DEFAULT_HANDSHAKE_SOURCE_BURST,
            100_000,
        )?;
        // —— 显式项全部合法后，才据 key 的存在性决定 Disabled / Enabled ——
        // 唯一的「禁用」语义：key **完全不存在**且其它项均合法。此时不加载身份、不生成密钥、
        // 不建监听；管理/状态线照常服务（`TransportConfig::from_env` 才对缺失报错）。
        let Some(raw_key) = key_file_raw else {
            return Ok(TransportEnv::Disabled);
        };
        // 存在但为空：仍是配置错误（不是「未设置」），且错误文本不回显路径。
        let key_file = non_empty(raw_key, ENV_KEY_FILE)?;
        let identity = Arc::new(
            IdentityKey::load_or_generate(&key_file).map_err(|_| ListenerError::Identity)?,
        );
        let session = SessionConfig::default();
        let config = TransportConfig {
            listen,
            identity,
            max_identities,
            db_timeout,
            handshakes_per_sec,
            handshake_burst,
            max_pending_per_source,
            max_handshakes_per_source,
            handshake_source_burst,
            manager,
            drain_grace: session.drain_timeout + SHUTDOWN_GRACE_MARGIN,
            close_out_grace: CLOSE_OUT_GRACE,
            session,
        };
        config.validate()?;
        Ok(TransportEnv::Enabled(config))
    }
}

impl TransportConfig {
    /// 二次校验：预算之间必须自洽（管理器硬上界 + 连接任务受检和 + 限额原语可构造）。
    pub fn validate(&self) -> Result<(), ListenerError> {
        self.manager
            .validate()
            .map_err(|reason| ListenerError::Config(format!("manager budget: {reason}")))?;
        if self
            .connection_task_budget()
            .ok_or_else(|| ListenerError::Config("connection task budget overflows".into()))?
            == 0
        {
            return Err(ListenerError::Config(
                "connection task budget must be non-zero".into(),
            ));
        }
        if self.max_identities == 0 || self.max_identities > MAX_IDENTITY_BUDGET {
            return Err(ListenerError::Config("max_identities out of range".into()));
        }
        if self.db_timeout.is_zero() || self.db_timeout > MAX_DB_TIMEOUT {
            return Err(ListenerError::Config("db timeout out of range".into()));
        }
        if self.drain_grace < self.session.drain_timeout {
            return Err(ListenerError::Config(
                "drain grace must not be shorter than the session drain timeout".into(),
            ));
        }
        self.handshake_limiter()?;
        self.source_quota()?;
        self.source_rate_limiter()?;
        self.session
            .validate()
            .map_err(|reason| ListenerError::Config(format!("session config: {reason}")))
    }

    /// 交给 [`TransportManager::try_new`] 的**同一份**预算（不存在第二处默认值）。
    #[must_use]
    pub fn manager_config(&self) -> ManagerConfig {
        self.manager
    }

    /// 连接任务硬上界（半开 + 活跃会话的**受检和**，溢出即 `None`）。
    #[must_use]
    pub fn connection_task_budget(&self) -> Option<usize> {
        self.manager.connection_task_budget()
    }

    /// 全局握手速率令牌桶（与配置同源；accept 循环构造一次共享 `Arc`）。
    pub fn handshake_limiter(&self) -> Result<Arc<HandshakeRateLimiter>, ListenerError> {
        let rate = u32::try_from(self.handshakes_per_sec)
            .map_err(|_| ListenerError::Config(format!("{ENV_HANDSHAKES_PER_SEC} too large")))?;
        HandshakeRateLimiter::new(rate, self.handshake_burst)
            .map(Arc::new)
            .map_err(|reason| ListenerError::Config(format!("handshake rate: {reason:?}")))
    }

    /// 来源未认证/挂起名额表（ready 即释放，见 [`SourceQuota`]）。
    pub fn source_quota(&self) -> Result<Arc<SourceQuota>, ListenerError> {
        SourceQuota::new(
            self.max_pending_per_source,
            MAX_SOURCE_TABLE_ENTRIES,
            SOURCE_IDLE_TTL,
        )
        .map(Arc::new)
        .map_err(|reason| ListenerError::Config(format!("source quota: {reason:?}")))
    }

    /// 单一来源握手速率桶表（审查补项：spec §4.6「全局**及来源**握手速率」）。
    ///
    /// 与并发名额 [`Self::source_quota`] 是**两张独立的有界表**：速率桶**不**随连接 ready/失败
    /// 而归还（那是「名额」的语义），只按真实握手尝试回填/扣减，空闲超过 TTL 才淘汰。因此攻击者
    /// 无法靠「快速建连再释放名额」重置同来源速率桶来绕过限速；只要持续握手，其桶就被持续消耗。
    pub fn source_rate_limiter(&self) -> Result<Arc<SourceRateLimiter>, ListenerError> {
        let rate = u32::try_from(self.max_handshakes_per_source).map_err(|_| {
            ListenerError::Config(format!("{ENV_MAX_HANDSHAKES_PER_SOURCE} too large"))
        })?;
        SourceRateLimiter::new(
            rate,
            self.handshake_source_burst,
            MAX_SOURCE_TABLE_ENTRIES,
            SOURCE_IDLE_TTL,
        )
        .map(Arc::new)
        .map_err(|reason| ListenerError::Config(format!("source rate: {reason:?}")))
    }

    /// 用**已审**生产准入策略（真实 `devices` 表）构造握手策略注入点。
    #[must_use]
    pub fn admission_policy(&self, db: &DbPool) -> Arc<dyn AdmissionPolicy> {
        Arc::new(
            SqlxAdmissionPolicy::new(db.clone(), self.max_identities)
                .with_db_timeout(self.db_timeout),
        )
    }
}

/// 读取环境变量原始值：存在但不是合法 UTF-8 → `Err`（审查 P2-13「吞 `NotUnicode`」）。
///
/// 返回 `None` **只**表示变量不存在；空串/全空白原样交回调用方判错。
fn raw_env(key: &str) -> Result<Option<String>, ListenerError> {
    match std::env::var_os(key) {
        None => Ok(None),
        Some(raw) => raw
            .into_string()
            .map(Some)
            .map_err(|_| ListenerError::Config(format!("{key} is not valid UTF-8"))),
    }
}

/// 要求「存在即非空」：空串是配置错误，不是「未配置」。
fn non_empty(raw: String, key: &str) -> Result<String, ListenerError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ListenerError::Config(format!(
            "{key} is set but empty (remove the variable to use the default)"
        )));
    }
    Ok(trimmed.to_string())
}

/// 正整数预算：缺失 → 默认；存在但空/非数字/0/超上界 → `Err`。
fn bounded_usize(raw: &Option<String>, default: usize, max: usize) -> Result<usize, ListenerError> {
    let Some(raw) = raw else { return Ok(default) };
    let trimmed = non_empty_raw(raw)?;
    let value = trimmed
        .parse::<usize>()
        .map_err(|_| ListenerError::Config(format!("value is not a positive integer: {raw}")))?;
    bounded(value, max)
}

/// 同 [`bounded_usize`]，但目标是 `u64`（速率类）。
fn bounded_u64(raw: &Option<String>, default: u64, max: u64) -> Result<u64, ListenerError> {
    let Some(raw) = raw else { return Ok(default) };
    let trimmed = non_empty_raw(raw)?;
    let value = trimmed
        .parse::<u64>()
        .map_err(|_| ListenerError::Config(format!("value is not a positive integer: {raw}")))?;
    if value == 0 {
        return Err(ListenerError::Config(
            "value must be a positive integer (0 is not accepted)".into(),
        ));
    }
    if value > max {
        return Err(ListenerError::Config(format!(
            "value exceeds the hard upper bound {max}"
        )));
    }
    Ok(value)
}

fn non_empty_raw(raw: &str) -> Result<&str, ListenerError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ListenerError::Config(
            "value is set but empty (remove the variable to use the default)".into(),
        ));
    }
    Ok(trimmed)
}

fn bounded(value: usize, max: usize) -> Result<usize, ListenerError> {
    if value == 0 {
        return Err(ListenerError::Config(
            "value must be a positive integer (0 is not accepted)".into(),
        ));
    }
    if value > max {
        return Err(ListenerError::Config(format!(
            "value exceeds the hard upper bound {max}"
        )));
    }
    Ok(value)
}

/// 毫秒期限 → [`Duration`]：缺失 → 默认；存在但空/非数字/0/超上界 → `Err`。
fn bounded_duration(
    raw: &Option<String>,
    default: Duration,
    max: Duration,
) -> Result<Duration, ListenerError> {
    let Some(raw) = raw else { return Ok(default) };
    let trimmed = non_empty_raw(raw)?;
    let millis = trimmed
        .parse::<u64>()
        .map_err(|_| ListenerError::Config(format!("value is not a millisecond count: {raw}")))?;
    let value = Duration::from_millis(millis);
    if value.is_zero() {
        return Err(ListenerError::Config(
            "duration must be non-zero (0 is not accepted)".into(),
        ));
    }
    if value > max {
        return Err(ListenerError::Config(format!(
            "duration exceeds the hard upper bound {}s",
            max.as_secs()
        )));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// SessionHandle → DeviceConnection 适配
// ---------------------------------------------------------------------------

/// 会话 driver 的 [`SessionHandle`] 到管理器 [`DeviceConnection`] 的**真实**适配。
///
/// 业务动作固定 Control 流；`RequestError` 逐项映射 typed [`CallError`]（超时不重发，
/// 关闭/排空→离线）。系统信令经 driver 的 `send_system` 走 `TYPE_SYSTEM`，不用 request 冒充。
pub struct SessionConnection {
    handle: SessionHandle,
}

impl std::fmt::Debug for SessionConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionConnection")
            .field("state", &self.state())
            .finish()
    }
}

impl SessionConnection {
    #[must_use]
    pub fn new(handle: SessionHandle) -> Self {
        Self { handle }
    }

    #[must_use]
    pub fn handle(&self) -> &SessionHandle {
        &self.handle
    }
}

impl DeviceConnection for SessionConnection {
    fn request(
        &self,
        action: &'static str,
        payload: Vec<u8>,
    ) -> ConnectionFuture<'_, Result<RpcReply, CallError>> {
        Box::pin(async move {
            let reply = self
                .handle
                .request(StreamKind::Control, action, payload)
                .await
                .map_err(map_request_error)?;
            Ok(RpcReply {
                status_code: reply.status_code,
                payload: reply.payload.to_vec(),
            })
        })
    }

    fn state(&self) -> ConnectionState {
        match *self.handle.subscribe_state().borrow() {
            SessionState::Connecting => ConnectionState::Connecting,
            SessionState::Ready => ConnectionState::Ready,
            SessionState::Draining => ConnectionState::Draining,
            SessionState::Closed => ConnectionState::Closed,
        }
    }

    fn send_system(&self, notice: SystemNotice) -> ConnectionFuture<'_, Result<(), ()>> {
        let signal = match notice {
            SystemNotice::Kick => SystemSignal::Kick,
            SystemNotice::Revoke => SystemSignal::Revoke,
        };
        Box::pin(async move {
            // `Ok(())` 只表示已入队：调用方据此决定「单调 500ms 后切断」还是「立即切断」。
            self.handle.send_system(signal).await.map_err(|_| ())
        })
    }

    fn graceful_shutdown(&self) {
        self.handle.graceful_shutdown();
    }

    fn abort(&self) {
        self.handle.abort();
    }
}

fn map_request_error(error: RequestError) -> CallError {
    match error {
        RequestError::Timeout => CallError::Timeout,
        RequestError::Closed | RequestError::StreamClosed => CallError::Closed,
        RequestError::Busy => CallError::Busy,
        RequestError::ProtocolMismatch | RequestError::TraceExhausted => CallError::Protocol,
    }
}

// ---------------------------------------------------------------------------
// SessionHandler 桥接
// ---------------------------------------------------------------------------

/// 板端主动事件（`device.task.result`）的业务入口 seam。
///
/// **必须**携带本地会话上下文 [`TaskResultContext`]（审查 P2-12）：它只携带中控本地产生、
/// 可核验的信息（已验签公钥 + 本地代际 + 事件闸门），消费方在**自己的写入边界**调用
/// [`TaskResultContext::is_current`] 复核，才不会被旧会话或已撤销会话注入。身份绝不从
/// payload 的 deviceId/serial 推断。
pub trait TaskResultSink: Send + Sync + 'static {
    /// 返回 `false` 表示本条**未被受理**（写入口未接入，或本条会话在复核时已失效）：
    /// 调用方计数丢弃，绝不静默成功。`true` 的确切语义由实现说明（见
    /// [`SqlxTaskResultSink`]：受理 = 会话有效且写入已调度，不代表 SQL 已提交）。
    fn on_task_result(&self, session: &TaskResultContext, record: &TaskRecord) -> bool;
}

/// fail-closed 实现：没有真实任务写入口时一律拒绝，计数可观测。
///
/// 它是**没有写入口的装配**（含既有测试入口）的默认值：任务结果推进的唯一 owner 是
/// [`TaskEvidenceApplier`](crate::tasks::evidence::TaskEvidenceApplier)，没有它就不伪造推进。
/// 生产装配应注入 [`SqlxTaskResultSink`]。
pub struct RejectNoTaskResultSink;

impl TaskResultSink for RejectNoTaskResultSink {
    fn on_task_result(&self, _session: &TaskResultContext, _record: &TaskRecord) -> bool {
        false
    }
}

/// [`SqlxTaskResultSink`] 的进程内去敏计数（不含任何 payload 内容）。
#[derive(Debug, Default)]
pub struct TaskResultSinkStats {
    /// 会话有效且持久化写入**已调度**（不代表 SQL 已提交）。
    pub dispatched: AtomicU64,
    /// 同步复核 [`TaskResultContext::is_current`] 即失败（旧代际/已撤销/闸门关闭），未写入。
    pub rejected_stale: AtomicU64,
    /// 无法调度（调用点不在 tokio 执行上下文中）：失败关闭，未写入。
    pub dispatch_failed: AtomicU64,
    /// 写入任务内、真正落库前复核 `is_current()` 才发现失效，未写入。
    pub dropped_stale_at_write: AtomicU64,
    /// 应用器返回 `Ok(Applied)`。
    pub applied: AtomicU64,
    /// 应用器返回 `Ok(IgnoredStale | RejectedMismatch | NotFound)`（如实计数，不称已推进）。
    pub ignored: AtomicU64,
    /// 应用器返回 `Err`（SQL/契约失败）。
    pub failed: AtomicU64,
}

/// 生产 sink：把板端主动上报的 `device.task.result` 交给持久化
/// [`TaskEvidenceApplier`](crate::tasks::evidence::TaskEvidenceApplier)。
///
/// # 身份与会话
///
/// 设备身份只取会话上下文的**已验签公钥**（[`TaskResultContext::public_key`]），绝不读
/// payload 的 deviceId/serial。`on_task_result` 先在自己的边界复核
/// [`TaskResultContext::is_current`]，失效即返回 `false`；会话上下文随写入任务一起移动，
/// 落库前**再核一次**（一次 `claim` 只保证「取到上下文那一刻」有效），因此被顶替/撤销的会话
/// 即使已让 `on_task_result` 返回 `true`，也不会真的写库。
///
/// # 为什么返回「已调度」而不是「已写完」
///
/// [`TaskResultSink::on_task_result`] 是同步的，而落库是 SQL。在会话 driver 里阻塞等待会把
/// 整条 Control 流堵在数据库上，因此本实现把写入调度到**当前** tokio 执行上下文（也就是这条
/// 会话所在 runtime），返回 `true` 只表示「会话有效且写入已受理调度」；SQL 的真实结果经
/// [`TaskResultSinkStats`] 计数并记去敏日志，绝不谎称已提交。取不到 runtime 上下文时**不排队、
/// 不自建线程**，直接失败关闭返回 `false`。
pub struct SqlxTaskResultSink {
    applier: Arc<dyn TaskEvidenceApplier>,
    stats: Arc<TaskResultSinkStats>,
}

impl SqlxTaskResultSink {
    /// 以默认（进程内）计数装配。
    #[must_use]
    pub fn new(applier: Arc<dyn TaskEvidenceApplier>) -> Arc<Self> {
        Self::with_stats(applier, Arc::default())
    }

    /// 由调用方提供**观测句柄**（两条路径共用同一实现，句柄不是行为开关）。
    #[must_use]
    pub fn with_stats(
        applier: Arc<dyn TaskEvidenceApplier>,
        stats: Arc<TaskResultSinkStats>,
    ) -> Arc<Self> {
        Arc::new(Self { applier, stats })
    }

    #[must_use]
    pub fn stats(&self) -> Arc<TaskResultSinkStats> {
        Arc::clone(&self.stats)
    }

    fn bump(&self, counter: &AtomicU64) -> u64 {
        counter.fetch_add(1, Ordering::AcqRel)
    }
}

impl TaskResultSink for SqlxTaskResultSink {
    fn on_task_result(&self, session: &TaskResultContext, record: &TaskRecord) -> bool {
        // 1) 本 sink 的提交边界复核：不是「本公钥当前已 ready 的 Control 代际」就拒，
        //    旧会话与已撤销会话都写不进任务。
        if !session.is_current() {
            self.bump(&self.stats.rejected_stale);
            return false;
        }
        // 2) 只调度到**当前**执行上下文；拿不到 runtime 即失败关闭（不排队、不后台兜底）。
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.bump(&self.stats.dispatch_failed);
            return false;
        };
        let applier = Arc::clone(&self.applier);
        let stats = Arc::clone(&self.stats);
        // 会话上下文随写入任务一起移动：它在 `Drop` 时归还在途名额，任务结束即归还。
        let session = session.clone();
        let device_id = session.public_key();
        let record = record.clone();
        runtime.spawn(async move {
            // 3) 落库前**再核一次**：调度期间本代际可能已被顶替/撤销/闸门关闭。
            if !session.is_current() {
                stats.dropped_stale_at_write.fetch_add(1, Ordering::AcqRel);
                return;
            }
            match applier.apply_task_record(&device_id, &record).await {
                Ok(ApplyOutcome::Applied {
                    subtask_id,
                    new_state,
                }) => {
                    stats.applied.fetch_add(1, Ordering::AcqRel);
                    eprintln!(
                        "controller task result sink: applied sub_task_id={subtask_id} \
                         state={new_state:?}"
                    );
                }
                Ok(outcome) => {
                    stats.ignored.fetch_add(1, Ordering::AcqRel);
                    eprintln!(
                        "controller task result sink: ignored sub_task_id={} outcome={outcome:?}",
                        record.sub_task_id
                    );
                }
                Err(error) => {
                    stats.failed.fetch_add(1, Ordering::AcqRel);
                    eprintln!(
                        "controller task result sink: apply failed sub_task_id={} error={error}",
                        record.sub_task_id
                    );
                }
            }
        });
        self.bump(&self.stats.dispatched);
        true
    }
}

/// 桥接观测计数（全部为进程内去敏计数）。
#[derive(Debug, Default)]
pub struct BridgeStats {
    /// 板端主动 request 被以 1002 拒绝的次数（§6.3）。
    pub requests_rejected: AtomicU64,
    /// 收到的事件总数。
    pub events_total: AtomicU64,
    /// 因**不是 Control 流**被丢弃（Data 流业务事件不得进任务链）。
    pub events_dropped_stream: AtomicU64,
    /// 因动作不受支持被丢弃。
    pub events_dropped_action: AtomicU64,
    /// 因 payload 无法经真实 codec 解码被丢弃。
    pub events_dropped_decode: AtomicU64,
    /// 因「本会话不是当前已 ready 的 Control 代际」被丢弃（未 ready/旧代际/已撤销）。
    pub events_dropped_stale: AtomicU64,
    /// 交给 sink 后被拒（未注入写入口的装配恒为拒绝；注入了写入口仍被拒 = 会话已失效）。
    pub events_unwired: AtomicU64,
    /// sink 已执行但提交边界复核发现会话已被摘除（**如实计数**，不称未发生）。
    pub events_stale_after_delivery: AtomicU64,
    pub sessions_ready: AtomicU64,
    pub sessions_closed: AtomicU64,
}

/// 会话 handler 桥：把「这条会话属于哪个已验签公钥 + 哪个本地代际」绑定在**外层**，
/// handler 只拿到已验签公钥与 packet，绝不从 packet 推断设备身份。
pub struct TunnelSessionHandler {
    manager: Arc<TransportManager>,
    /// 握手产出的已验签公钥（本会话唯一的登记键）。
    public_key: [u8; 32],
    /// 本会话的本地代际：`register_ready` 成功后**一次性**写入；写入前到达的事件一律丢弃
    /// 计数（此刻本会话还不是当前会话，接受它等于接受一个尚未登记的代际）。
    generation: OnceLock<Uuid>,
    sink: Arc<dyn TaskResultSink>,
    stats: Arc<BridgeStats>,
}

impl TunnelSessionHandler {
    #[must_use]
    pub fn new(
        manager: Arc<TransportManager>,
        public_key: [u8; 32],
        sink: Arc<dyn TaskResultSink>,
        stats: Arc<BridgeStats>,
    ) -> Arc<Self> {
        Arc::new(Self {
            manager,
            public_key,
            generation: OnceLock::new(),
            sink,
            stats,
        })
    }

    /// `register_ready` 成功后绑定本地代际（只允许一次；重复调用被忽略）。
    fn bind_generation(&self, generation: Uuid) {
        let _ = self.generation.set(generation);
    }

    fn bump(&self, counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::AcqRel);
    }
}

impl SessionHandler for TunnelSessionHandler {
    fn on_request(&self, ctx: RequestContext) -> HandlerFuture {
        // §4/04：板端不得主动发业务 request。协议层已在双流内接收，业务层一律 1002，
        // 不猜测动作、不代为执行、不从 payload 读身份。
        let _ = ctx;
        self.stats.requests_rejected.fetch_add(1, Ordering::AcqRel);
        Box::pin(async move {
            Reply {
                status_code: STATUS_UNSUPPORTED_ACTION,
                error_message: String::new(),
                payload: Vec::new().into(),
            }
        })
    }

    fn on_event(&self, stream: StreamKind, packet: TunnelPacket) {
        self.stats.events_total.fetch_add(1, Ordering::AcqRel);

        // 1) **流约束**：本切片只承认 Control 流上的板端主动事件；Data 流业务事件一律丢弃
        //    （审查 P2-12：旧实现忽略 `stream`，Data 流业务事件也能进任务链）。
        if stream != StreamKind::Control {
            self.bump(&self.stats.events_dropped_stream);
            return;
        }
        // 2) **动作约束**：只承认 `device.task.result`。
        if packet.action != ACTION_TASK_RESULT {
            self.bump(&self.stats.events_dropped_action);
            return;
        }
        // 3) **真实 codec**：经已审 `decode_business` 解码；形状不符即丢弃计数（不猜、不部分接受）。
        let witness = approved_codec_witness();
        let Ok(BusinessPayload::TaskRecord(record)) = decode_business(
            &BusinessCodec,
            Some(&witness),
            BusinessPayloadKind::TaskRecord,
            &packet.payload,
        ) else {
            self.bump(&self.stats.events_dropped_decode);
            return;
        };
        // 4) **代际绑定**：只有「本公钥当前已 ready 的 Control 会话」才拿得到投递上下文。
        //    判据在管理器锁内一次完成，不取 payload 身份；未 ready / 旧代际 / 已撤销 /
        //    闸门已关闭都在这里被拒。
        let Some(generation) = self.generation.get().copied() else {
            self.bump(&self.stats.events_dropped_stale);
            return;
        };
        let Some(session) = self.manager.claim_task_result(&self.public_key, generation) else {
            self.bump(&self.stats.events_dropped_stale);
            return;
        };
        // 5) **提交边界**：sink 是同步调用（不进任何队列）；返回后立刻复核上下文是否仍然有效
        //    并如实计数。真正写任务的一方必须在自己的写入边界再核
        //    `TaskResultContext::is_current`——这正是 sink 必须携带代际上下文的原因。
        let accepted = self.sink.on_task_result(&session, &record);
        if !accepted {
            self.stats.events_unwired.fetch_add(1, Ordering::AcqRel);
        }
        if !session.is_current() {
            self.stats
                .events_stale_after_delivery
                .fetch_add(1, Ordering::AcqRel);
        }
    }

    fn on_system(&self, _signal: SystemSignal) {
        // 中控是信令的**发送**方；收到对端同名信令只计数，不据此改本机准入真相。
    }

    fn on_ready(&self) {
        self.stats.sessions_ready.fetch_add(1, Ordering::AcqRel);
    }

    fn on_closed(&self, _reason: SessionCloseReason) {
        self.stats.sessions_closed.fetch_add(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------------------
// 监听循环
// ---------------------------------------------------------------------------

/// 监听线运行计数（装配健康判据）。
#[derive(Debug, Default)]
pub struct ListenerStats {
    pub accepted: AtomicU64,
    pub half_open_rejected: AtomicU64,
    pub handshake_failed: AtomicU64,
    pub handshake_policy_rejected: AtomicU64,
    pub ready: AtomicU64,
    pub register_rejected: AtomicU64,
    pub tasks_active: AtomicUsize,
    /// 因**全局握手速率**被拒新（§4.6 第 1 条：资源不足只拒新，不写永久决定）。
    pub handshake_rate_limited: AtomicU64,
    /// 因**来源未认证名额**被拒新。
    pub source_quota_rejected: AtomicU64,
    /// 因**来源握手速率**被拒新（审查补项：与全局速率、来源名额分开计数/原因）。
    pub source_rate_limited: AtomicU64,
    /// 因超过连接任务硬上界（半开 + 会话）被拒新。
    pub tasks_dropped: AtomicU64,
    /// 实际绑定成功的地址（`127.0.0.1:0` 之类由内核分配端口的装配需要公布它）。
    bound: std::sync::Mutex<Option<SocketAddr>>,
}

impl ListenerStats {
    /// 公布绑定成功的地址（仅由 `run_transport_listener_with_stats` 在 bind 成功后写入）。
    fn publish_bound(&self, address: SocketAddr) {
        let mut bound = self
            .bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *bound = Some(address);
    }

    /// 已绑定地址；`None` 表示尚未绑定或绑定失败。
    #[must_use]
    pub fn bound(&self) -> Option<SocketAddr> {
        *self
            .bound
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 当前活跃连接任务数（关停收敛的可观测证据）。
    #[must_use]
    pub fn tasks_active(&self) -> usize {
        self.tasks_active.load(Ordering::Acquire)
    }
}

/// 未注入任务写入口时的默认 sink：fail-closed 拒绝，不伪造任务推进。
fn reject_no_sink() -> Arc<dyn TaskResultSink> {
    Arc::new(RejectNoTaskResultSink)
}

/// 启动生产监听，直到 `shutdown` 置真。
///
/// 返回 `Err` 表示监听未能建立或自身失败——由**顶层**统一传播并关停其它后台任务；本函数
/// 内部绝不 `std::process::exit`（审查 P1-3）。
///
/// 本入口不注入任务结果写入口，因此任务结果一律被拒（[`RejectNoTaskResultSink`]）；生产
/// 装配应走 [`run_controller_listener_with_sink`] 并注入 [`SqlxTaskResultSink`]。
pub async fn run_controller_listener(
    config: TransportConfig,
    db: DbPool,
    manager: Arc<TransportManager>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), ListenerError> {
    run_controller_listener_with_sink(config, db, manager, shutdown, reject_no_sink()).await
}

/// 同 [`run_controller_listener`]，但由调用方注入**任务结果写入口**。
///
/// 这是生产装配把板端 `device.task.result` 接到
/// [`TaskEvidenceApplier`](crate::tasks::evidence::TaskEvidenceApplier) 的入口：sink 只在这
/// 一条路径上被使用，本模块不因此产生第二套任务状态推进逻辑。
pub async fn run_controller_listener_with_sink(
    config: TransportConfig,
    db: DbPool,
    manager: Arc<TransportManager>,
    shutdown: watch::Receiver<bool>,
    sink: Arc<dyn TaskResultSink>,
) -> Result<(), ListenerError> {
    let policy = config.admission_policy(&db);
    run_transport_listener_with_sink(config, policy, manager, shutdown, Arc::default(), sink).await
}

/// 可注入**已审** [`AdmissionPolicy`] 的监听入口。
///
/// 这个参数不是「测试专用开关」：它是策略的注入点，生产由
/// [`TransportConfig::admission_policy`] 注入 [`SqlxAdmissionPolicy`]（真实 SQL），测试可注入
/// 内存策略以在无库条件下覆盖握手/取消/排空路径。
pub async fn run_transport_listener(
    config: TransportConfig,
    policy: Arc<dyn AdmissionPolicy>,
    manager: Arc<TransportManager>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), ListenerError> {
    run_transport_listener_with_stats(config, policy, manager, shutdown, Arc::default()).await
}

/// 同 [`run_transport_listener`]，但由调用方提供**观测句柄**（计数与绑定地址）。
///
/// 这不是行为开关：两条路径共用同一实现，句柄只是将运行期计数暴露给调用方（生产可用于
/// 周期日志，测试用于断言拒新/配额/收敛）。任务结果写入口取 fail-closed 默认值。
pub async fn run_transport_listener_with_stats(
    config: TransportConfig,
    policy: Arc<dyn AdmissionPolicy>,
    manager: Arc<TransportManager>,
    shutdown: watch::Receiver<bool>,
    stats: Arc<ListenerStats>,
) -> Result<(), ListenerError> {
    run_transport_listener_with_sink(config, policy, manager, shutdown, stats, reject_no_sink())
        .await
}

/// 同 [`run_transport_listener_with_stats`]，但由调用方注入**任务结果写入口**（第 6 个参数）。
pub async fn run_transport_listener_with_sink(
    config: TransportConfig,
    policy: Arc<dyn AdmissionPolicy>,
    manager: Arc<TransportManager>,
    mut shutdown: watch::Receiver<bool>,
    stats: Arc<ListenerStats>,
    sink: Arc<dyn TaskResultSink>,
) -> Result<(), ListenerError> {
    config.validate()?;
    let task_budget = config
        .connection_task_budget()
        .ok_or_else(|| ListenerError::Config("connection task budget overflows".into()))?;
    let limiter = config.handshake_limiter()?;
    let quota = config.source_quota()?;
    let source_rate = config.source_rate_limiter()?;
    let handler_stats = Arc::new(BridgeStats::default());
    let identity = Arc::clone(&config.identity);
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|_| ListenerError::ListenAddress)?;
    if let Ok(address) = listener.local_addr() {
        stats.publish_bound(address);
    }
    let _ = task_budget;

    let mut tasks: JoinSet<()> = JoinSet::new();
    loop {
        if *shutdown.borrow_and_update() {
            break;
        }
        // 先收割已退出的连接任务（释放 JoinSet 名额），再判连接任务硬上界。
        while tasks.try_join_next().is_some() {}
        if tasks.len() >= task_budget {
            // 连接任务总量硬上界：背压时**不 accept**（不排队、不无限增任务）。
            stats.tasks_dropped.fetch_add(1, Ordering::AcqRel);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(5)) => {}
                changed = shutdown.changed() => { if changed.is_err() { break; } }
            }
            continue;
        }
        let accept = tokio::select! {
            result = listener.accept() => result,
            changed = shutdown.changed() => {
                if changed.is_err() { break; }
                continue;
            }
        };
        let (stream, peer) = match accept {
            Ok(value) => value,
            Err(_) => continue,
        };
        // ① 全局握手速率：在**昂贵握手之前**判定，桶空即拒新（直接关闭 socket）。
        if !limiter.try_acquire(std::time::Instant::now()) {
            stats.handshake_rate_limited.fetch_add(1, Ordering::AcqRel);
            continue;
        }
        // ①.5 单一来源握手速率（审查补项）：按真实 `peer.ip` 判定，与全局速率、来源名额分开
        // 计数与原因。攻击者释放/新建连接不会重置本来源桶（桶按 IP 持久，只在空闲 TTL 后淘汰），
        // 因此「快速建连再释放名额」绕不过来源速率。NAT 后 ready 设备不占未认证名额，但**新握手
        // 仍受本来源速率约束**。
        if !source_rate.try_acquire(peer.ip(), std::time::Instant::now()) {
            stats.source_rate_limited.fetch_add(1, Ordering::AcqRel);
            continue;
        }
        // ② 来源未认证/挂起名额（RAII：失败/取消/ready 都靠 Drop 归还）。
        let Ok(source) = quota.try_reserve(peer.ip(), std::time::Instant::now()) else {
            stats.source_quota_rejected.fetch_add(1, Ordering::AcqRel);
            continue;
        };
        stats.accepted.fetch_add(1, Ordering::AcqRel);
        stats.tasks_active.fetch_add(1, Ordering::AcqRel);
        let manager = Arc::clone(&manager);
        let policy = Arc::clone(&policy);
        let identity = Arc::clone(&identity);
        let handler_stats = Arc::clone(&handler_stats);
        let sink = Arc::clone(&sink);
        let stats = Arc::clone(&stats);
        let session = config.session;
        let shutdown = shutdown.clone();
        tasks.spawn(async move {
            let _guard = TaskGuard(Arc::clone(&stats));
            run_connection(
                stream,
                source,
                manager,
                policy,
                identity,
                session,
                handler_stats,
                sink,
                stats,
                shutdown,
            )
            .await;
        });
    }

    // 关停顺序固定：① 停止接新连接 → ② 请求全部会话排空（h2 GOAWAY，板端按 §7 重连新进程）
    // → ③ 在宽限期内等连接任务**自己跑完** driver 的 `drain_timeout`（默认 ≥5s，§6.6）
    // → ④ 超期才强制 abort 并 await 确认退出 → ⑤ 收尾「信令 + 延迟切断」任务。
    // 直接 abort 会让板端看到连接被硬断而不是 GOAWAY，因此它只是超期兜底，不是首选路径。
    drop(listener);
    manager.shutdown_all();
    let deadline = tokio::time::Instant::now() + config.drain_grace;
    while !tasks.is_empty() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::select! {
            joined = tasks.join_next() => {
                if joined.is_none() { break; }
            }
            () = tokio::time::sleep(remaining) => break,
        }
    }
    // `JoinSet::shutdown`：终止剩余任务并**等待其真正结束**（不是「发个信号就走」）。
    tasks.shutdown().await;
    // ⑤ 「信令 + 延迟切断」收尾任务同样有 owner：有界 await，超期才 abort。
    manager.drain_close_outs(config.close_out_grace).await;
    Ok(())
}

/// `wait_ready` 阶段的四种结局（都必须在**同一次** `select!` 里与撤销/关停竞争）。
enum ReadyPhase {
    Ready,
    Failed,
    Cancelled,
    ShuttingDown,
}

/// 单条连接的生命周期任务。
#[allow(
    clippy::too_many_arguments,
    reason = "装配参数就是这条连接需要的全部上下文"
)]
async fn run_connection<S>(
    stream: S,
    source: SourcePermit,
    manager: Arc<TransportManager>,
    policy: Arc<dyn AdmissionPolicy>,
    identity: Arc<IdentityKey>,
    session_config: SessionConfig,
    handler_stats: Arc<BridgeStats>,
    sink: Arc<dyn TaskResultSink>,
    stats: Arc<ListenerStats>,
    mut shutdown: watch::Receiver<bool>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // ③ 半开名额：**握手前**取得，**登记成功**才转移释放（`NewSession::half_open`）。
    // 失败/取消/panic 全部经 `Drop` 归还，因此未 ready 的连接始终计入半开预算（审查 P1-4）。
    let half_open = match manager.half_open_permit() {
        Ok(permit) => permit,
        // 半开预算已满：直接关闭 socket（资源拒绝，不写任何永久决定，§4.6 第 1 条）。
        Err(_) => {
            stats.half_open_rejected.fetch_add(1, Ordering::AcqRel);
            return;
        }
    };
    let handshake_policy = CancellableAdmissionPolicy::new(&manager, Arc::clone(&policy));
    let server_config = ServerHandshakeConfig { identity };
    let result = tokio::select! {
        // 握手按值接管 socket（成功时经 `HandshakeResult.io` 原样交回）；若关停分支获胜，
        // 这个 future 连同 socket 一起被丢弃 = 直接关闭连接，不回任何认证响应（§4.5）。
        result = server_handshake(stream, &server_config, &handshake_policy) => result,
        _ = shutdown.changed() => {
            // 关停触发：取消本挂起握手并关闭 socket，不回任何认证响应。
            handshake_policy.cancel_current();
            return;
        }
    };
    let handshake = match result {
        Ok(handshake) => handshake,
        Err(error) => {
            // 失败只分类计数，不外泄错误文本（`HandshakeError` 本身去敏）。
            if matches!(error, HandshakeError::Policy) {
                stats
                    .handshake_policy_rejected
                    .fetch_add(1, Ordering::AcqRel);
            } else {
                stats.handshake_failed.fetch_add(1, Ordering::AcqRel);
            }
            return;
        }
    };
    // 身份只取握手产出的已验签公钥；socket 由握手结果原样交回（不再有别的借用）。
    let HandshakeResult {
        io,
        session_keys,
        peer_public_key: public_key,
        ..
    } = handshake;
    let io = SecureIo::new(io, session_keys.into_server_record_layers());

    let handler = TunnelSessionHandler::new(
        Arc::clone(&manager),
        public_key,
        sink,
        Arc::clone(&handler_stats),
    );
    // `start_server_session` 是同步入口：它只 spawn owner 任务并返回句柄，
    // 真正的 ready 由下面的 `wait_ready` 等（双流首 probe 有效 pong）。
    let (handle, session_task) = match start_server_session(
        io,
        Arc::clone(&handler) as Arc<dyn SessionHandler>,
        session_config,
    ) {
        Ok(value) => value,
        Err(_) => {
            stats.handshake_failed.fetch_add(1, Ordering::AcqRel);
            return;
        }
    };
    let pending = match handshake_policy.into_pending() {
        Some(ticket) => ticket,
        None => {
            // 没有挂起凭证说明握手未经过准入（不可能同时又是 Approved）：失败关闭。
            handle.abort();
            let _ = session_task.await;
            return;
        }
    };

    // 审查 P1-7：从握手结束到 ready 的**整段**都必须与「挂起握手被撤销」和「进程关停」竞争，
    // 且取消后立即 `abort` + **await** 会话任务（不是只发信号就走：否则 driver 会活到首
    // probe 超时，把这条连接的句柄留在途）。
    let phase = loop {
        tokio::select! {
            biased;
            () = pending.cancelled() => break ReadyPhase::Cancelled,
            outcome = handle.wait_ready() => {
                break if outcome.is_ok() { ReadyPhase::Ready } else { ReadyPhase::Failed };
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow_and_update() {
                    break ReadyPhase::ShuttingDown;
                }
            }
        }
    };
    if !matches!(phase, ReadyPhase::Ready) {
        if matches!(phase, ReadyPhase::Failed) {
            stats.handshake_failed.fetch_add(1, Ordering::AcqRel);
        }
        handle.abort();
        let _ = session_task.await;
        return;
    }

    let connection: Arc<dyn DeviceConnection> = Arc::new(SessionConnection::new(handle.clone()));
    let registered = manager.register_ready(NewSession {
        public_key,
        connection,
        pending,
        half_open,
    });
    let generation = match registered {
        Ok(outcome) => {
            stats.ready.fetch_add(1, Ordering::AcqRel);
            // 名额转移完成：半开名额由 `register_ready` 释放，来源名额在此释放——
            // 从此本连接由「活跃会话预算」计量（审查 P1-4 + 补项）。
            drop(source);
            // 事件闸门在此打开：此刻之后到达的事件才属于一个已登记的当代际会话。
            handler.bind_generation(outcome.generation);
            outcome.generation
        }
        Err(error) => {
            // `Revoked`：撤销先落库，本会话从未成为当前会话；`StaleHandshake`：更旧的握手
            // 试图顶替更新的会话（§4.6 第 3 条）。两者都必须终止这条连接。
            debug_assert!(!matches!(error, RegisterError::TicketMismatch));
            stats.register_rejected.fetch_add(1, Ordering::AcqRel);
            handle.abort();
            let _ = session_task.await;
            return;
        }
    };

    // 会话 owner 任务退出（对端关闭/GOAWAY/故障/abort）后才摘除，且只按当代际摘除；
    // `SessionOutcome` 的计数与原因属 driver 的观测面，本切片不消费（不伪造二次聚合）。
    let outcome: SessionOutcome = match session_task.await {
        Ok(outcome) => outcome,
        Err(_) => return,
    };
    let _ = outcome;
    manager.deregister(&public_key, generation);
}

/// 连接任务退出时递减活跃计数（「关闭后无泄漏」的可验证证据）。
struct TaskGuard(Arc<ListenerStats>);

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.0.tasks_active.fetch_sub(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------------------
// 单元测试：生产 sink 的会话复核与持久化调度
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;

    use chrono::Utc;

    use crate::ControllerError;
    use crate::api::ManagementFuture;
    use crate::api::system::{RecoveryModeProbe, RecoveryView};
    use crate::polling::SnapshotStore;
    use crate::tasks::adapter::{TaskTimeSource, TimeEvidenceFuture};
    use crate::tasks::model::SubTaskState;
    use crate::time::evidence::{TimeEvidence, TimeQuality};
    use crate::transport::manager::{AdmissionDecisionFuture, AdmissionTruth};

    // --- 替身：只造「装配一条 ready 会话」所需的最小上下文，不引入第二套判定 ---

    struct NoTruth;

    impl AdmissionTruth for NoTruth {
        fn load_current(&self, _public_key: [u8; 32]) -> AdmissionDecisionFuture<'_> {
            Box::pin(async { Err(ControllerError::NotFound) })
        }
    }

    struct FixedTime;

    impl TaskTimeSource for FixedTime {
        fn current_evidence(&self) -> TimeEvidenceFuture<'_> {
            Box::pin(async {
                let now = Utc::now();
                TimeEvidence {
                    system_wall_utc: now,
                    reference_utc: now,
                    quality: TimeQuality::SystemFallback,
                    clock_epoch: Uuid::new_v4(),
                    source: None,
                    sample_age_ms: None,
                    offset_ms: None,
                    uncertainty_ms: None,
                }
            })
        }
    }

    struct NoRecovery;

    impl RecoveryModeProbe for NoRecovery {
        fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
            Box::pin(async { Err(ControllerError::Config("recovery probe not wired".into())) })
        }
    }

    /// 恒 ready 的连接替身：`claim_task_result` 要求当前会话处于 ready。
    struct ReadyConnection;

    impl DeviceConnection for ReadyConnection {
        fn request(
            &self,
            _action: &'static str,
            _payload: Vec<u8>,
        ) -> ConnectionFuture<'_, Result<RpcReply, CallError>> {
            Box::pin(async { Err(CallError::Closed) })
        }

        fn state(&self) -> ConnectionState {
            ConnectionState::Ready
        }

        fn send_system(&self, _notice: SystemNotice) -> ConnectionFuture<'_, Result<(), ()>> {
            Box::pin(async { Ok(()) })
        }

        fn graceful_shutdown(&self) {}

        fn abort(&self) {}
    }

    // --- 替身：任务证据应用器（记录调用身份，按预定裁决返回） ---

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum ApplierOutcome {
        Applied,
        Ignored,
        Failed,
    }

    struct RecordingApplier {
        outcome: ApplierOutcome,
        /// 每次**真正进入落库**时记下 `(device_id, sub_task_id)`。
        calls: Mutex<Vec<([u8; 32], String)>>,
    }

    impl RecordingApplier {
        fn new(outcome: ApplierOutcome) -> Arc<Self> {
            Arc::new(Self {
                outcome,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<([u8; 32], String)> {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
        }
    }

    impl TaskEvidenceApplier for RecordingApplier {
        fn apply_task_record(
            &self,
            device_id: &[u8; 32],
            record: &TaskRecord,
        ) -> Pin<Box<dyn Future<Output = Result<ApplyOutcome, ControllerError>> + Send + '_>>
        {
            self.calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((*device_id, record.sub_task_id.clone()));
            let result = match self.outcome {
                ApplierOutcome::Applied => Ok(ApplyOutcome::Applied {
                    subtask_id: Uuid::new_v4(),
                    new_state: SubTaskState::Verifying,
                }),
                ApplierOutcome::Ignored => Ok(ApplyOutcome::NotFound),
                ApplierOutcome::Failed => Err(ControllerError::NotFound),
            };
            Box::pin(async move { result })
        }
    }

    // --- 夹具：一条真实的当代际投递上下文 ---

    struct Fixture {
        manager: Arc<TransportManager>,
        key: [u8; 32],
        generation: Uuid,
        session: TaskResultContext,
    }

    fn fixture() -> Fixture {
        let manager = TransportManager::new(
            Arc::new(NoTruth),
            Arc::new(SnapshotStore::new()),
            Arc::new(NoRecovery),
            Arc::new(FixedTime),
            ManagerConfig::default(),
        );
        let key = [0x5a; 32];
        let pending = manager.track_pending(key);
        let half_open = manager.half_open_permit().expect("半开名额");
        let registered = manager
            .register_ready(NewSession {
                public_key: key,
                connection: Arc::new(ReadyConnection),
                pending,
                half_open,
            })
            .expect("登记 ready 会话");
        let session = manager
            .claim_task_result(&key, registered.generation)
            .expect("当代际投递上下文");
        Fixture {
            manager,
            key,
            generation: registered.generation,
            session,
        }
    }

    fn record(sub_task_id: Uuid) -> TaskRecord {
        TaskRecord {
            schema_version: rsetup_protocol::wire::BUSINESS_SCHEMA_VERSION,
            sub_task_id: sub_task_id.to_string(),
            journal_epoch: Uuid::new_v4().to_string(),
            record_version: 1,
            state: "verifying".into(),
            pre_boot_id: "boot-1".into(),
            observed_boot_id: None,
            reason_code: None,
            updated_unix_ms: None,
            evidence_kind: None,
        }
    }

    fn load(counter: &AtomicU64) -> u64 {
        counter.load(Ordering::Acquire)
    }

    async fn wait_until(timeout: Duration, cond: &dyn Fn() -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if cond() {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return cond();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test]
    async fn applies_record_with_verified_key_while_session_is_current() {
        let applier = RecordingApplier::new(ApplierOutcome::Applied);
        let sink = SqlxTaskResultSink::new(Arc::clone(&applier) as Arc<dyn TaskEvidenceApplier>);
        let stats = sink.stats();
        let fixture = fixture();
        let sub_task_id = Uuid::new_v4();

        assert!(
            sink.on_task_result(&fixture.session, &record(sub_task_id)),
            "会话有效时必须受理，绝不静默丢弃"
        );
        assert_eq!(load(&stats.dispatched), 1);
        assert!(
            wait_until(Duration::from_secs(2), &|| load(&stats.applied) == 1).await,
            "已受理的写入必须真正走到应用器"
        );

        // 身份只取会话的已验签公钥，不是 payload 里的 deviceId/serial。
        assert_eq!(
            applier.calls(),
            vec![(fixture.key, sub_task_id.to_string())],
            "落库身份必须是本会话的已验签公钥"
        );
        assert_eq!(load(&stats.rejected_stale), 0);
        assert_eq!(load(&stats.dropped_stale_at_write), 0);
        assert_eq!(load(&stats.failed), 0);
    }

    #[tokio::test]
    async fn rejects_when_session_is_no_longer_current() {
        let applier = RecordingApplier::new(ApplierOutcome::Applied);
        let sink = SqlxTaskResultSink::new(Arc::clone(&applier) as Arc<dyn TaskEvidenceApplier>);
        let stats = sink.stats();
        let fixture = fixture();
        assert!(fixture.manager.deregister(&fixture.key, fixture.generation));
        assert!(!fixture.session.is_current(), "摘除后会话必须失效");

        assert!(
            !sink.on_task_result(&fixture.session, &record(Uuid::new_v4())),
            "已失效会话必须被拒"
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(applier.calls().is_empty(), "失效会话不得写库");
        assert_eq!(load(&stats.rejected_stale), 1);
        assert_eq!(load(&stats.dispatched), 0);
    }

    #[tokio::test]
    async fn drops_at_write_boundary_when_session_dies_after_dispatch() {
        // `#[tokio::test]` 的单线程 runtime：spawn 的任务在测试任务让出前不会执行，因此可以
        // 确定性地复现「已受理，但落库前会话被摘除」。
        let applier = RecordingApplier::new(ApplierOutcome::Applied);
        let sink = SqlxTaskResultSink::new(Arc::clone(&applier) as Arc<dyn TaskEvidenceApplier>);
        let stats = sink.stats();
        let fixture = fixture();

        assert!(sink.on_task_result(&fixture.session, &record(Uuid::new_v4())));
        assert!(fixture.manager.deregister(&fixture.key, fixture.generation));
        assert!(
            wait_until(Duration::from_secs(2), &|| load(
                &stats.dropped_stale_at_write
            ) == 1)
            .await,
            "落库前复核必须发现会话已失效"
        );

        assert!(applier.calls().is_empty(), "复核失效后不得写库");
        assert_eq!(load(&stats.dispatched), 1, "受理计数如实（返回过 true）");
    }

    #[tokio::test]
    async fn counts_ignored_and_failed_outcomes_as_they_are() {
        let fixture = fixture();
        let failed = SqlxTaskResultSink::new(
            RecordingApplier::new(ApplierOutcome::Failed) as Arc<dyn TaskEvidenceApplier>
        );
        let ignored = SqlxTaskResultSink::new(
            RecordingApplier::new(ApplierOutcome::Ignored) as Arc<dyn TaskEvidenceApplier>
        );

        assert!(failed.on_task_result(&fixture.session, &record(Uuid::new_v4())));
        assert!(ignored.on_task_result(&fixture.session, &record(Uuid::new_v4())));
        let (failed_stats, ignored_stats) = (failed.stats(), ignored.stats());

        assert!(
            wait_until(Duration::from_secs(2), &|| load(&failed_stats.failed) == 1).await,
            "SQL 失败必须如实计数"
        );
        assert!(
            wait_until(Duration::from_secs(2), &|| load(&ignored_stats.ignored)
                == 1)
            .await,
            "未推进（忽略）必须与已推进分开计数"
        );
        assert_eq!(load(&failed_stats.applied), 0);
        assert_eq!(load(&ignored_stats.applied), 0);
    }

    #[test]
    fn refuses_without_a_tokio_runtime_context() {
        // 没有 runtime 就不排队、不自建线程：失败关闭，让调用方计数丢弃。
        let applier = RecordingApplier::new(ApplierOutcome::Applied);
        let sink = SqlxTaskResultSink::new(Arc::clone(&applier) as Arc<dyn TaskEvidenceApplier>);
        let stats = sink.stats();
        let fixture = fixture();
        assert!(fixture.session.is_current());

        assert!(
            !sink.on_task_result(&fixture.session, &record(Uuid::new_v4())),
            "取不到执行上下文必须返回 false"
        );

        assert!(applier.calls().is_empty());
        assert_eq!(load(&stats.dispatch_failed), 1);
        assert_eq!(load(&stats.dispatched), 0);
    }

    #[test]
    fn reject_no_sink_still_fails_closed() {
        let fixture = fixture();
        assert!(!RejectNoTaskResultSink.on_task_result(&fixture.session, &record(Uuid::new_v4())));
    }
}

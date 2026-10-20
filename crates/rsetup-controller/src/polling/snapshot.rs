//! 内存最新设备状态快照存储（05 §6 RUN-01 与 R-02；计划 Task 5 快照部分）。
//!
//! 只驻留内存、每台设备仅保留**最新一份**可信状态样本，进程重启不恢复（05 §9：
//! 「不恢复旧进程/流代际、online 或内存快照」）。写入侧的可信入口只有四个，且调用方
//! 只能是认证连接管理器或轮询驱动：[`SnapshotStore::confirm_generation`]（确认当前
//! 代际，唯一的代际推进入口）、[`SnapshotStore::update`]（该代际流上收到的
//! `device.status.get` 应答）、[`SnapshotStore::record_error`]（该代际上的采集失败）
//! 与 [`SnapshotStore::disconnect_generation`]（该代际的连接断开）；
//! 读取侧 [`SnapshotStore::read`] 供 HTTP 只读缓存投影（GET 绝不触发采集）。
//!
//! 四条不可让渡的语义：
//! 1. **代际只由认证连接管理器推进**（03 §139、05 R-02、计划 Task 5）：
//!    `session_epoch`/`stream_epoch`/`boot_id`/`agent_epoch` 都是随机 UUIDv4，
//!    **不存在任何可比的大小关系**。因此本 store 绝不比较 UUID 字节序来判新旧，
//!    只承认「与当前确认代际相等」这一条：任何不相等的回调（旧会话、旧流、旧
//!    boot、旧 agent）一律 `StaleGeneration` 拒绝，绝不覆盖当前快照。新代际只能
//!    由连接管理器在双流完成认证与健康确认后显式 [`SnapshotStore::confirm_generation`] 推进。
//!    因此设备**尚未确认任何代际**时，[`SnapshotStore::update`] 与 [`SnapshotStore::record_error`] 一律
//!    `NoActiveGeneration` fail-closed 拒绝：本 store 绝不因为「收到一条自洽的样本」
//!    就自行建立代际，否则任何能伪造首条 payload 的路径都绕过了认证连接管理器。
//! 2. **同作用域内 `sample_seq` 严格递增**（03 §139：`sample_seq` 从 1 起单调增长，
//!    作用域 `(boot_id, agent_epoch)`）：作用域内更低或相同的序号一律丢弃，最后好
//!    样本原地保留；`boot_id` 或 `agent_epoch` 变化才是作用域切换，此时显式确认
//!    后序号下界归零，新作用域从 1 重新起算，同时丢弃旧 boot 的样本（不得冒充
//!    重启后的当前状态）。同 `(boot_id, agent_epoch)` 内的重连只换 session/stream，
//!    作用域不变，序号下界与最后好样本都必须保留，否则重连前那条流的迟到样本会
//!    重新覆盖新值。
//! 3. **新鲜度只按单调接收年龄**（05 R-02、§6「status 过期阈值 30s：单调接收
//!    年龄，无样本 unknown」）：年龄只由 `Instant` 锚点差算出，宿主墙钟前跳/回拨
//!    都不会让年龄倒退、清零或变负（`saturating_duration_since`）；`TimeEvidence`
//!    只随快照携带供展示，绝不参与年龄判定。
//! 4. **连接清理只复位连接标记，绝不抹掉作用域**：连接断开走
//!    [`SnapshotStore::disconnect_generation`]，且**只有传入代际与当前确认代际完全相等**时才生效
//!    ——`(boot_id, agent_epoch)`、`seq_floor` 与最后好样本必须原样保留，否则同作用域
//!    重连后序号下界丢失，断线前那条流的迟到旧样本会重新覆盖新值；旧连接延迟到达的
//!    清理回调因代际不相等而零副作用，绝不误删新连接刚确认的代际。
//!    [`SnapshotStore::forget`] 只表示设备从系统中**彻底移除**，是唯一允许丢弃作用域与下界的入口。
//!
//! 采集失败走 [`SnapshotStore::record_error`]：**保留最后好样本与其单调年龄**，只附加去敏
//! 稳定错误码，绝不把未知指标回填 0；下一条好样本到达即自动清除。
//!
//! 并发：单把 `RwLock<HashMap<..>>`。写入是「校验 + 覆盖一条目」的纯内存常数时间
//! 操作（1024 台 @10s ≈ 102 写/s），读取只克隆一条快照；临界区内不调用任何外部
//! 代码、不持锁跨 `.await`，因此读写互斥即可保证「一条样本整体原子替换」，读者
//! 永远不会看到 `sample_seq` 与 payload 撕裂的组合。锁中毒只可能来自本模块内部
//! panic（临界区内无回调），读路径选择恢复内值而不是 panic，保证状态读永不 500。

use std::collections::HashMap;
use std::sync::{PoisonError, RwLock};
use std::time::{Duration, Instant};

use rsetup_protocol::wire::{BusinessMessage, DeviceStatus, Metric};
use uuid::Uuid;

use crate::http_device_status::{
    DeviceStatusCacheEntry, DeviceStatusFreshness, DeviceStatusFuture, DeviceStatusSnapshotSource,
};
use crate::time::evidence::TimeEvidence;

/// 状态过期阈值（05 §6 表 + RUN-01 已确认界线）：单调接收年龄超过 30s 即 `stale`。
/// 边界含 30s 整点（`<=`），跨过一整纳秒才转 `stale`，绝不用墙钟参与判定。
pub const STATUS_STALE_AFTER: Duration = Duration::from_secs(30);

/// 设备状态样本的代际坐标：session/stream 来自已认证连接，boot/agent 来自板端自证。
///
/// 四个字段都是**无大小关系**的随机标识，只做相等比较；`Eq` 是「同一代际」的唯一
/// 判据，任何排序比较都属于本模块的契约违反。`boot_id`/`agent_epoch` 保持 wire 的
/// `String` 形态（03 规定为 canonical 小写 UUIDv4），与 `DeviceStatus` 原样对齐，
/// 不做有损转换。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotGeneration {
    pub session_epoch: Uuid,
    pub stream_epoch: Uuid,
    pub boot_id: String,
    pub agent_epoch: String,
}

impl SnapshotGeneration {
    pub fn new(
        session_epoch: Uuid,
        stream_epoch: Uuid,
        boot_id: impl Into<String>,
        agent_epoch: impl Into<String>,
    ) -> Self {
        Self {
            session_epoch,
            stream_epoch,
            boot_id: boot_id.into(),
            agent_epoch: agent_epoch.into(),
        }
    }

    /// `sample_seq` 的作用域（03 §139）：`boot_id` 或 `agent_epoch` 变化才会让
    /// 序号从 1 重新起算；仅 session/stream 变化不改变作用域。
    fn seq_scope_matches(&self, other: &Self) -> bool {
        self.boot_id == other.boot_id && self.agent_epoch == other.agent_epoch
    }
}

/// [`SnapshotStore::confirm_generation`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GenerationOutcome {
    /// 该设备首次确认代际：序号下界 0、无样本、无错误、连接存活。
    Established,
    /// 与当前确认代际完全相同：幂等确认。除把连接标记复位为存活（连接管理器重申该代际
    /// 仍然健康）外不改任何状态——序号下界、最后好样本与错误一概不动。
    AlreadyActive,
    /// 代际推进。`scope_changed = true` 表示 `boot_id`/`agent_epoch` 变化，序号下界
    /// 归零且丢弃旧 boot 样本；`false` 表示同 `(boot_id, agent_epoch)` 内重连，
    /// 序号下界与最后好样本都必须保留。
    Replaced {
        previous: SnapshotGeneration,
        scope_changed: bool,
        seq_floor: u64,
    },
}

/// [`SnapshotStore::disconnect_generation`] 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisconnectOutcome {
    /// 该设备没有任何已确认代际（从未 confirm，或已被 [`SnapshotStore::forget`] 移除）。
    NoActiveGeneration,
    /// 传入代际与当前确认代际不相等：这是**旧连接延迟到达**的清理回调，当前确认的新
    /// 代际、序号下界与最后好样本必须原样保留，零副作用。
    StaleGeneration,
    /// 代际完全匹配：连接标记已复位为断开。作用域 `(boot_id, agent_epoch)`、
    /// `seq_floor`（`kept_seq_floor`）与最后好样本（`kept_sample_seq`）全部保留，
    /// 同作用域重连后序号下界继续生效。
    Disconnected {
        kept_seq_floor: u64,
        kept_sample_seq: Option<u64>,
    },
}

/// 被接受样本的回执：`previous_seq` 是本次覆盖掉的上一条样本序号（首条为 `None`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpdateAccepted {
    pub previous_seq: Option<u64>,
}

/// 写入被拒的原因。全部为**正常背压**语义（旧回调、乱序样本），不是内部错误，
/// 因此不映射 HTTP 状态码；调用方按原因做去敏计数即可。
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum UpdateRejected {
    /// 该设备还没有任何**由认证连接管理器确认过**的代际（从未 `confirm_generation`，
    /// 或已被 [`SnapshotStore::forget`] 移除）。这是 fail-closed 的正常拒绝：代际不由
    /// 样本自己建立。
    #[error("device generation has not been confirmed by the authenticated connection manager")]
    NoActiveGeneration,
    /// 回调所属代际与当前确认代际不相等（旧 session/stream/boot/agent）。
    #[error("sample or error belongs to a generation that is not the current confirmed one")]
    StaleGeneration,
    /// 同作用域内序号未严格递增（乱序或重复样本）。
    #[error("sample_seq {incoming} does not strictly exceed accepted {accepted}")]
    StaleSampleSeq { accepted: u64, incoming: u64 },
    /// payload 自带的 boot_id/agent_epoch 与所声称的代际不一致：来源不可信，fail-closed。
    #[error("status payload boot_id/agent_epoch disagrees with the claimed generation")]
    PayloadGenerationMismatch,
    /// payload 违反 03 wire 合同（版本、UUID 形状、`sample_seq > 0`、指标词表等）。
    #[error("status payload violates the wire contract: {reason}")]
    InvalidPayload { reason: String },
}

/// 一次读取看到的最后好样本视图（含错误态下保留的最后好样本）。
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotView {
    /// 产出该样本的代际（错误态保留的样本可能来自上一条流）。
    pub generation: SnapshotGeneration,
    pub status: DeviceStatus,
    /// 接收时刻的墙钟证据，仅用于展示；**不参与**年龄与新鲜度判定。
    pub received_time: TimeEvidence,
    /// 接收时刻的单调锚点，年龄与新鲜度的唯一依据。
    pub received_at: Instant,
    /// 去敏稳定错误码；`Some` 时新鲜度恒为 `Error`，但样本与年龄原样保留。
    pub last_error: Option<String>,
}

impl SnapshotView {
    pub fn sample_seq(&self) -> u64 {
        self.status.sample_seq
    }

    pub fn metrics(&self) -> &[Metric] {
        &self.status.metrics
    }

    /// 自单调接收锚点起经过的整毫秒数：只按单调时钟，永不回退，也不会因
    /// 「查询时刻早于接收时刻」变负（`saturating_duration_since` 饱和于 0）；
    /// 进程存活超过 u64 毫秒才饱和于 `u64::MAX`，不回绕。
    pub fn age_ms_at(&self, now: Instant) -> u64 {
        u64::try_from(now.saturating_duration_since(self.received_at).as_millis())
            .unwrap_or(u64::MAX)
    }

    pub fn age_ms(&self) -> u64 {
        self.age_ms_at(Instant::now())
    }

    /// 新鲜度四态投影（02 §1）：`error` 优先（保留最后好样本），其余只看单调
    /// 接收年龄对 [`STATUS_STALE_AFTER`]。`unknown` 由「没有样本」即
    /// [`SnapshotStore::read`] 返回 `None` 表达，本视图永不返回 `Unknown`。
    pub fn freshness_at(&self, now: Instant) -> DeviceStatusFreshness {
        if self.last_error.is_some() {
            return DeviceStatusFreshness::Error;
        }
        if now.saturating_duration_since(self.received_at) <= STATUS_STALE_AFTER {
            DeviceStatusFreshness::Fresh
        } else {
            DeviceStatusFreshness::Stale
        }
    }

    pub fn is_fresh(&self, now: Instant) -> bool {
        self.freshness_at(now) == DeviceStatusFreshness::Fresh
    }
}

/// 每台设备最多一份条目：当前确认代际 + 序号下界 + 最后好样本 + 去敏错误码。
pub struct SnapshotStore {
    entries: RwLock<HashMap<[u8; 32], Entry>>,
}

#[derive(Clone, Debug)]
struct Entry {
    /// 认证连接管理器确认的当前代际；样本与错误都必须来自它才被接受。
    active: SnapshotGeneration,
    /// 当前 `sample_seq` 作用域内已接受的最大序号；作用域切换时归零。
    seq_floor: u64,
    /// 最后可信样本。`seq_floor > 0` 与 `sample.is_some()` 同生同灭。
    sample: Option<StoredSample>,
    last_error: Option<String>,
    /// 该确认代际对应的连接是否仍然存活：`confirm_generation` 置真、
    /// [`SnapshotStore::disconnect_generation`] 置假。只作连接态观测，**不参与**样本
    /// 与错误的准入判定（准入只看代际相等 + 序号严格递增）。
    connected: bool,
}

#[derive(Clone, Debug)]
struct StoredSample {
    generation: SnapshotGeneration,
    status: DeviceStatus,
    received_time: TimeEvidence,
    received_at: Instant,
}

impl SnapshotStore {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// 认证连接管理器确认设备当前代际（双流已建立并通过认证与健康确认时调用）。
    ///
    /// 这是设备首次接入与代际变更的**唯一**入口：本 store 不会因收到样本而自行建立
    /// 代际（见 [`UpdateRejected::NoActiveGeneration`]）。
    ///
    /// 这也是唯一能让 `sample_seq` 下界归零的入口：只有 `(boot_id, agent_epoch)`
    /// 变化才归零并丢弃旧 boot 样本；同作用域内的重连保留下界与最后好样本。
    pub fn confirm_generation(
        &self,
        device: [u8; 32],
        generation: SnapshotGeneration,
    ) -> GenerationOutcome {
        let mut entries = self.write();
        let Some(entry) = entries.get_mut(&device) else {
            entries.insert(
                device,
                Entry {
                    active: generation,
                    seq_floor: 0,
                    sample: None,
                    last_error: None,
                    connected: true,
                },
            );
            return GenerationOutcome::Established;
        };
        entry.connected = true;
        if entry.active == generation {
            return GenerationOutcome::AlreadyActive;
        }
        let scope_changed = !entry.active.seq_scope_matches(&generation);
        let previous = std::mem::replace(&mut entry.active, generation);
        let seq_floor = if scope_changed {
            entry.sample = None;
            entry.last_error = None;
            entry.seq_floor = 0;
            0
        } else {
            entry.seq_floor
        };
        GenerationOutcome::Replaced {
            previous,
            scope_changed,
            seq_floor,
        }
    }

    /// 写入一条状态样本：payload 先过 03 wire 合同与代际自洽，再要求它来自当前
    /// 确认代际，最后要求 `sample_seq` 严格超过作用域下界。三步任一失败都零副作用。
    ///
    /// 设备尚未由认证连接管理器确认代际时**必须** `Err(NoActiveGeneration)` 拒绝：
    /// 代际只由 [`Self::confirm_generation`] 建立与推进，本方法绝不因「payload 自洽」
    /// 就自行建立 active generation，否则首条样本就成了绕过认证连接管理器的代际入口。
    ///
    /// 连接断开（[`Self::disconnect_generation`] 之后）不改变本方法的准入判定：同一
    /// 已认证代际上迟到但仍严格递增的样本仍是可信数据，序号下界继续挡住旧样本。
    pub fn update(
        &self,
        device: &[u8; 32],
        generation: &SnapshotGeneration,
        status: &DeviceStatus,
        received_time: TimeEvidence,
        now: Instant,
    ) -> Result<UpdateAccepted, UpdateRejected> {
        if let Err(reason) = status.validate() {
            return Err(UpdateRejected::InvalidPayload {
                reason: reason.to_string(),
            });
        }
        if status.boot_id != generation.boot_id || status.agent_epoch != generation.agent_epoch {
            return Err(UpdateRejected::PayloadGenerationMismatch);
        }

        let mut entries = self.write();
        let entry = match entries.get_mut(device) {
            Some(entry) => entry,
            None => return Err(UpdateRejected::NoActiveGeneration),
        };
        if &entry.active != generation {
            return Err(UpdateRejected::StaleGeneration);
        }

        if status.sample_seq <= entry.seq_floor {
            return Err(UpdateRejected::StaleSampleSeq {
                accepted: entry.seq_floor,
                incoming: status.sample_seq,
            });
        }

        let previous_seq = entry.sample.as_ref().map(|prev| prev.status.sample_seq);
        entry.seq_floor = status.sample_seq;
        entry.last_error = None;
        entry.sample = Some(StoredSample {
            generation: generation.clone(),
            status: status.clone(),
            received_time,
            received_at: now,
        });
        Ok(UpdateAccepted { previous_seq })
    }

    /// 记录一次采集失败：只附加去敏错误码，保留最后好样本、序号下界与单调年龄。
    /// 被拒（旧代际/未确认代际）时零副作用，绝不把当前好样本降级。
    pub fn record_error(
        &self,
        device: &[u8; 32],
        generation: &SnapshotGeneration,
        code: &str,
    ) -> Result<(), UpdateRejected> {
        let mut entries = self.write();
        let Some(entry) = entries.get_mut(device) else {
            return Err(UpdateRejected::NoActiveGeneration);
        };
        if &entry.active != generation {
            return Err(UpdateRejected::StaleGeneration);
        }
        entry.last_error = Some(code.to_string());
        Ok(())
    }

    /// 连接断开时的**受控**清理：只有 `generation` 与当前确认代际**完全相等**时，
    /// 才把该代际的连接标记复位为断开。
    ///
    /// 绝不做的事（这是本方法存在的全部理由）：
    /// - **不删除条目、不清 `seq_floor`、不清最后好样本**：作用域 `(boot_id, agent_epoch)`
    ///   跨重连不变，序号下界一旦丢失，断线前那条流的迟到旧样本就能重新覆盖新值；
    /// - **不推进也不回退代际**：代际只由 [`Self::confirm_generation`] 推进；
    /// - **不误伤新代际**：传入代际与当前确认代际不相等（旧连接延迟到达的清理回调）
    ///   时返回 `StaleGeneration` 且零副作用。
    ///
    /// 内存回收不属本方法职责：条目只随设备总数增长，设备下线由 [`Self::forget`] 释放。
    pub fn disconnect_generation(
        &self,
        device: &[u8; 32],
        generation: &SnapshotGeneration,
    ) -> DisconnectOutcome {
        let mut entries = self.write();
        let Some(entry) = entries.get_mut(device) else {
            return DisconnectOutcome::NoActiveGeneration;
        };
        if &entry.active != generation {
            return DisconnectOutcome::StaleGeneration;
        }
        entry.connected = false;
        DisconnectOutcome::Disconnected {
            kept_seq_floor: entry.seq_floor,
            kept_sample_seq: entry.sample.as_ref().map(|prev| prev.status.sample_seq),
        }
    }

    /// 当前确认代际的连接标记：`None` 表示该设备没有任何已确认代际（unknown）。
    /// 只作连接态观测（`Some(false)` = 已确认代际但连接已断开，最后好样本仍在缓存里，
    /// 由单调年龄自然转 `stale`），不参与任何准入判定。
    pub fn is_connected(&self, device: &[u8; 32]) -> Option<bool> {
        self.read_guard().get(device).map(|entry| entry.connected)
    }

    /// 读取该设备的最后好样本快照（HTTP 只读缓存路径）。
    ///
    /// 返回 `None` 的**唯一**含义是「本进程当前代际下还没有收到任何可信样本」——
    /// 包含已确认代际但首样本未到、以及作用域切换后旧 boot 样本已被丢弃两种情形，
    /// HTTP 侧据此投影 `snapshot=null`、`freshness=unknown`、`age_ms=null`。
    pub fn read(&self, device: &[u8; 32]) -> Option<SnapshotView> {
        let entries = self.read_guard();
        let entry = entries.get(device)?;
        let sample = entry.sample.as_ref()?;
        Some(SnapshotView {
            generation: sample.generation.clone(),
            status: sample.status.clone(),
            received_time: sample.received_time.clone(),
            received_at: sample.received_at,
            last_error: entry.last_error.clone(),
        })
    }

    /// 设备从系统中**彻底移除**（退役、删除设备记录）时释放条目。返回是否真的删掉了。
    ///
    /// 这是唯一丢弃作用域与 `seq_floor` 的入口：调用后该设备回到「没有任何已确认代际」
    /// 的状态，后续样本必须先由认证连接管理器重新 [`Self::confirm_generation`]，
    /// 否则一律 `NoActiveGeneration` 拒绝。
    ///
    /// **连接断开不得调用本方法**：同 `(boot_id, agent_epoch)` 的重连要沿用旧的序号
    /// 下界，误用本方法会让断线前那条流的迟到旧样本重新覆盖新值。断线清理请用
    /// [`Self::disconnect_generation`]。
    pub fn forget(&self, device: &[u8; 32]) -> bool {
        self.write().remove(device).is_some()
    }

    /// 已建立条目（已确认代际）的设备数。
    pub fn len(&self) -> usize {
        self.read_guard().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 中毒锁恢复内值：临界区内只有本模块的纯内存操作，不调用外部代码，中毒只可能
    /// 来自本模块 bug；读路径不得因此 panic（否则 GET 读缓存在无故障时也会 500）。
    fn read_guard(&self) -> std::sync::RwLockReadGuard<'_, HashMap<[u8; 32], Entry>> {
        self.entries.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<[u8; 32], Entry>> {
        self.entries.write().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new()
    }
}

/// 把内存快照源挂载到 `http_device_status::DeviceStatusState` 的真实实现。
///
/// GET 路径只做一次「读锁 + 克隆一条快照」，绝不触发采集或执行；无样本时
/// `Ok(None)`（响应 `unknown`），绝不降级成常量 `fresh`，也不伪造 `TimeEvidence`。
/// `received_at` 原样带出，由 HTTP 侧按同一单调锚点计算 `age_ms`。
impl DeviceStatusSnapshotSource for SnapshotStore {
    fn snapshot<'a>(
        &'a self,
        device_id: &'a [u8; 32],
    ) -> DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>> {
        let view = self.read(device_id);
        let now = Instant::now();
        Box::pin(async move {
            Ok(view.map(|view| {
                let freshness = view.freshness_at(now);
                let last_error = view.last_error.clone();
                DeviceStatusCacheEntry {
                    snapshot: view.status,
                    received_time: view.received_time,
                    received_at: view.received_at,
                    freshness,
                    last_error,
                }
            }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use rsetup_protocol::wire::Metric;
    use std::sync::Arc;
    use std::time::Duration;

    const BOOT_A: &str = "11111111-1111-4111-8111-111111111111";
    const BOOT_B: &str = "22222222-2222-4222-8222-222222222222";
    const AGENT_1: &str = "33333333-3333-4333-8333-333333333333";
    const AGENT_2: &str = "44444444-4444-4444-8444-444444444444";
    const CLOCK_1: &str = "55555555-5555-4555-8555-555555555555";
    const DEVICE: [u8; 32] = [0x77u8; 32];

    fn session(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    fn stream(n: u8) -> Uuid {
        Uuid::from_bytes([n.wrapping_add(0x80); 16])
    }

    fn make_generation(
        session_byte: u8,
        stream_byte: u8,
        boot: &str,
        agent: &str,
    ) -> SnapshotGeneration {
        SnapshotGeneration::new(session(session_byte), stream(stream_byte), boot, agent)
    }

    fn evidence(roll_forward: bool) -> TimeEvidence {
        let base = Utc.with_ymd_and_hms(2026, 10, 20, 12, 0, 0).unwrap();
        let wall = if roll_forward {
            base + chrono::Duration::hours(1)
        } else {
            base
        };
        TimeEvidence {
            system_wall_utc: wall,
            reference_utc: base,
            quality: crate::time::evidence::TimeQuality::SystemFallback,
            clock_epoch: Uuid::new_v4(),
            source: None,
            sample_age_ms: None,
            offset_ms: None,
            uncertainty_ms: None,
        }
    }

    /// 合同内 wire `DeviceStatus`：`sample_seq` 与首个指标值同源，便于断言「保留的是哪一份」。
    fn status(boot: &str, agent: &str, sample_seq: u64) -> DeviceStatus {
        DeviceStatus {
            schema_version: rsetup_protocol::wire::BUSINESS_SCHEMA_VERSION,
            boot_id: boot.into(),
            agent_epoch: agent.into(),
            sample_seq,
            sampled_unix_ms: 1_760_000_000_000,
            sampled_monotonic_ns: sample_seq,
            clock_epoch: CLOCK_1.into(),
            metrics: vec![Metric {
                key: "cpu.load1".into(),
                value: Some(sample_seq as f64),
                unit: "ratio".into(),
                quality: "ok".into(),
            }],
            clock_quality: "observed".into(),
        }
    }

    fn accepted(store: &SnapshotStore, g: &SnapshotGeneration, sample_seq: u64, at: Instant) {
        let outcome = store.update(
            &DEVICE,
            g,
            &status(&g.boot_id, &g.agent_epoch, sample_seq),
            evidence(false),
            at,
        );
        assert!(
            outcome.is_ok(),
            "sample_seq {sample_seq} 必须被接受，实得 {outcome:?}"
        );
    }

    /// 认证连接管理器确认代际：`update`/`record_error` 的**硬前置条件**。
    /// 除专测「未确认即拒绝」的用例外，任何写入前都必须先确认成功。
    fn confirm(store: &SnapshotStore, g: &SnapshotGeneration) -> GenerationOutcome {
        store.confirm_generation(DEVICE, g.clone())
    }

    fn confirmed(store: &SnapshotStore, g: &SnapshotGeneration) {
        let outcome = confirm(store, g);
        assert!(
            matches!(
                outcome,
                GenerationOutcome::Established | GenerationOutcome::Replaced { .. }
            ),
            "确认代际必须成功，实得 {outcome:?}"
        );
        assert_eq!(
            store.is_connected(&DEVICE),
            Some(true),
            "确认后连接必须存活"
        );
    }

    /// P1：未确认代际时 `update` 必须 `NoActiveGeneration` fail-closed，且零副作用。
    #[test]
    fn update_without_confirmed_generation_is_rejected_without_side_effects() {
        let store = SnapshotStore::new();
        let g = make_generation(20, 20, BOOT_A, AGENT_1);
        // payload 完全合同内且与声称代际自洽：唯一能被拒的理由就是「代际未经确认」。
        let outcome = store.update(
            &DEVICE,
            &g,
            &status(BOOT_A, AGENT_1, 1),
            evidence(false),
            Instant::now(),
        );
        assert_eq!(outcome.err(), Some(UpdateRejected::NoActiveGeneration));
        assert!(store.is_empty(), "被拒的 update 绝不得建立条目或代际");
        assert_eq!(store.len(), 0);
        assert!(store.read(&DEVICE).is_none());
        assert_eq!(
            store.is_connected(&DEVICE),
            None,
            "unknown 设备无连接态可言"
        );

        // record_error 同样不得自行建立代际。
        assert_eq!(
            store
                .record_error(&DEVICE, &g, "TRANSPORT_UNAVAILABLE")
                .err(),
            Some(UpdateRejected::NoActiveGeneration)
        );
        assert!(store.is_empty(), "被拒的 record_error 也不得建立条目");

        // 断开连接同样不建立条目。
        assert_eq!(
            store.disconnect_generation(&DEVICE, &g),
            DisconnectOutcome::NoActiveGeneration
        );
        assert!(store.is_empty());

        // 只有显式确认才建立代际；建立后同一条样本才被接受。
        assert_eq!(confirm(&store, &g), GenerationOutcome::Established);
        assert!(store.read(&DEVICE).is_none(), "确认代际本身不产生样本");
        accepted(&store, &g, 1, Instant::now());
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 1);
    }

    /// 单调新鲜度边界（05 §6：30s 单调接收年龄，含 30s 整点）。
    #[test]
    fn first_sample_after_confirm_reads_back_with_fresh_monotonic_age() {
        let store = SnapshotStore::new();
        let g = make_generation(1, 1, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &g);
        accepted(&store, &g, 1, t0);

        let view = store.read(&DEVICE).expect("首样本必须可读");
        assert_eq!(view.sample_seq(), 1);
        assert_eq!(view.status.boot_id, BOOT_A);
        assert_eq!(view.metrics().len(), 1);
        assert_eq!(view.last_error, None);
        assert_eq!(view.age_ms_at(t0), 0, "接收当刻年龄必须为 0");
        assert_eq!(
            view.freshness_at(t0.checked_add(Duration::from_secs(30)).unwrap()),
            DeviceStatusFreshness::Fresh,
            "单调年龄恰 30s 仍属 fresh"
        );
        assert_eq!(
            view.freshness_at(
                t0.checked_add(Duration::from_nanos(30_000_000_001))
                    .unwrap()
            ),
            DeviceStatusFreshness::Stale,
            "越过 30s 必须转 stale"
        );
        assert!(
            !view.is_fresh(t0.checked_add(Duration::from_secs(31)).unwrap()),
            "越过 30s 后 is_fresh 必须为 false"
        );
    }

    /// 单调年龄只按 `Instant` 锚点算：墙钟前跳/回拨、以及「查询时刻早于接收时刻」
    /// 都不得让年龄变负或倒退。
    #[test]
    fn age_is_monotonic_only_and_saturates_instead_of_going_negative() {
        let store = SnapshotStore::new();
        let g = make_generation(2, 2, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &g);
        accepted(&store, &g, 1, t0);

        // 携带「墙钟前跳 1 小时」证据的第二条样本：锚点必须是传入的单调时刻，
        // 年龄必须只由锚点差算出，与墙钟跳变完全无关。
        let t_jump = t0.checked_add(Duration::from_secs(5)).unwrap();
        assert!(
            store
                .update(
                    &DEVICE,
                    &g,
                    &status(BOOT_A, AGENT_1, 2),
                    evidence(true),
                    t_jump
                )
                .is_ok()
        );
        let view = store.read(&DEVICE).expect("样本必须可读");
        assert_eq!(
            view.received_time.system_wall_utc,
            view.received_time.reference_utc + chrono::Duration::hours(1),
            "接收证据必须原样保留"
        );
        assert_eq!(view.received_at, t_jump, "单调锚点必须原样保留");
        assert_eq!(
            view.age_ms_at(t_jump),
            0,
            "墙钟前跳 1 小时不得凭空造出 1 小时年龄"
        );
        assert_eq!(
            view.age_ms_at(t0),
            0,
            "年龄绝不为负（查询时刻早于接收时刻）"
        );
        assert_eq!(
            view.age_ms_at(t_jump.checked_add(Duration::from_secs(5)).unwrap()),
            5_000
        );
        let first = view.age_ms_at(t_jump);
        let later = view.age_ms_at(t_jump.checked_add(Duration::from_millis(1)).unwrap());
        assert!(later >= first, "年龄只增不减：{first} -> {later}");
    }

    /// 同代际内乱序/重复 `sample_seq` 必须被拒，且不得覆盖已存的更新快照。
    #[test]
    fn out_of_order_and_duplicate_sample_seq_never_overwrite_a_newer_sample() {
        let store = SnapshotStore::new();
        let g = make_generation(3, 3, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &g);
        accepted(&store, &g, 5, t0);

        let at = t0.checked_add(Duration::from_millis(10)).unwrap();
        for stale in [5u64, 1, 4] {
            let rejected = store
                .update(
                    &DEVICE,
                    &g,
                    &status(BOOT_A, AGENT_1, stale),
                    evidence(false),
                    at,
                )
                .expect_err(&format!("重复或更旧的 sample_seq {stale} 必须被拒"));
            assert!(
                matches!(rejected, UpdateRejected::StaleSampleSeq { accepted: 5, incoming } if incoming == stale),
                "拒绝原因必须是 StaleSampleSeq(accepted=5, incoming={stale})，实得 {rejected:?}"
            );
        }
        let view = store.read(&DEVICE).expect("样本必须可读");
        assert_eq!(view.sample_seq(), 5, "旧序号不得覆盖当前快照");
        assert_eq!(
            view.metrics()[0].value,
            Some(5.0),
            "保留的必须还是 seq=5 那份 payload"
        );

        accepted(
            &store,
            &g,
            6,
            t0.checked_add(Duration::from_millis(20)).unwrap(),
        );
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 6);
    }

    /// 代际由认证连接管理器唯一确认：任何「不等于当前代际」的回调（旧 session、旧流、
    /// 旧 boot、旧 agent）一律拒；且判定绝不按随机 UUID 的字节序大小。
    #[test]
    fn callback_from_a_different_generation_is_rejected_regardless_of_uuid_ordering() {
        let store = SnapshotStore::new();
        // 当前代际的 session UUID 字节序取最小，使「更大 UUID」成为旧代际回调的代理场景：
        // 只要实现偷偷按 UUID 大小判新旧，本用例就会放行旧回调而失败。
        let current = make_generation(0x01, 0x01, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &current);
        accepted(&store, &current, 9, t0);

        let stale_callbacks = [
            // (说明, 旧代际, 旧代际 payload 的 boot/agent)
            (
                "更高 session 字节序的旧会话",
                make_generation(0xff, 0x01, BOOT_A, AGENT_1),
                BOOT_A,
                AGENT_1,
            ),
            (
                "旧流代际",
                make_generation(0x01, 0xff, BOOT_A, AGENT_1),
                BOOT_A,
                AGENT_1,
            ),
            (
                "旧 boot 代际",
                make_generation(0x01, 0x01, BOOT_B, AGENT_1),
                BOOT_B,
                AGENT_1,
            ),
            (
                "旧 agent 代际",
                make_generation(0x01, 0x01, BOOT_A, AGENT_2),
                BOOT_A,
                AGENT_2,
            ),
        ];
        for (label, stale_gen, boot, agent) in stale_callbacks {
            let outcome = store.update(
                &DEVICE,
                &stale_gen,
                &status(boot, agent, 9_999),
                evidence(false),
                t0.checked_add(Duration::from_secs(1)).unwrap(),
            );
            assert!(
                matches!(outcome, Err(UpdateRejected::StaleGeneration)),
                "{label} 的回调必须 StaleGeneration 拒绝，实得 {outcome:?}"
            );
        }
        let view = store.read(&DEVICE).expect("当前快照必须仍在");
        assert_eq!(view.sample_seq(), 9);
        assert_eq!(view.generation, current);
        assert_eq!(store.len(), 1, "被拒回调不得凭空创建设备条目");
    }

    /// payload 自带的 boot_id/agent_epoch 与声称代际不一致时 fail-closed 拒绝。
    #[test]
    fn payload_must_agree_with_the_generation_it_claims() {
        let store = SnapshotStore::new();
        let g = make_generation(4, 4, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        let outcome = store.update(
            &DEVICE,
            &g,
            &status(BOOT_B, AGENT_1, 1),
            evidence(false),
            t0,
        );
        assert_eq!(
            outcome.err(),
            Some(UpdateRejected::PayloadGenerationMismatch)
        );
        assert!(store.is_empty(), "不匹配的 payload 不得建立条目");

        // 已确认代际后同理：不自洽 payload 只可能被拒，绝不动当前快照。
        confirmed(&store, &g);
        accepted(&store, &g, 9, t0);
        let outcome = store.update(
            &DEVICE,
            &g,
            &status(BOOT_B, AGENT_1, 10),
            evidence(false),
            t0.checked_add(Duration::from_millis(1)).unwrap(),
        );
        assert_eq!(
            outcome.err(),
            Some(UpdateRejected::PayloadGenerationMismatch)
        );
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 9, "零副作用");
    }

    /// 合同外 payload（sample_seq=0）不得进入缓存。
    #[test]
    fn contract_violating_payload_is_rejected_without_touching_store() {
        let store = SnapshotStore::new();
        let g = make_generation(5, 5, BOOT_A, AGENT_1);
        let bad = status(BOOT_A, AGENT_1, 0);
        let outcome = store.update(&DEVICE, &g, &bad, evidence(false), Instant::now());
        assert!(
            matches!(outcome, Err(UpdateRejected::InvalidPayload { .. })),
            "sample_seq=0 必须按合同拒绝，实得 {outcome:?}"
        );
        assert!(store.is_empty());

        // 已确认代际且已有好样本时同样零副作用：合同外 payload 不得降级当前快照。
        confirmed(&store, &g);
        accepted(&store, &g, 4, Instant::now());
        let outcome = store.update(&DEVICE, &g, &bad, evidence(false), Instant::now());
        assert!(
            matches!(outcome, Err(UpdateRejected::InvalidPayload { .. })),
            "已确认代际下 sample_seq=0 仍须按合同拒绝，实得 {outcome:?}"
        );
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 4, "零副作用");
    }

    /// boot/agent 变更 = sample_seq 作用域切换：显式确认后序号从 1 重启，且旧 boot 的
    /// 快照不得继续冒充当前状态。
    #[test]
    fn boot_or_agent_restart_resets_the_sample_seq_scope_explicitly() {
        let store = SnapshotStore::new();
        let old_gen = make_generation(6, 6, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &old_gen);
        accepted(&store, &old_gen, 500, t0);

        let rebooted = make_generation(7, 7, BOOT_B, AGENT_2);
        let outcome = store.confirm_generation(DEVICE, rebooted.clone());
        assert!(
            matches!(&outcome, GenerationOutcome::Replaced { scope_changed: true, seq_floor: 0, previous } if previous == &old_gen),
            "boot/agent 变更必须重置序号作用域，实得 {outcome:?}"
        );
        assert!(
            store.read(&DEVICE).is_none(),
            "旧 boot 的样本不得继续充当当前状态"
        );

        // 新作用域内从 1 起正常递增。
        let t1 = t0.checked_add(Duration::from_secs(1)).unwrap();
        accepted(&store, &rebooted, 1, t1);
        accepted(&store, &rebooted, 2, t1);
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 2);
    }

    /// 同 boot/agent 内的重连（仅 session/stream 变更）属于同一 sample_seq 作用域：
    /// 已接受的下界与最后好样本都必须保留，否则重连前的旧样本能重新覆盖。
    #[test]
    fn reconnect_within_same_seq_scope_keeps_seq_floor_and_last_good_sample() {
        let store = SnapshotStore::new();
        let before = make_generation(8, 8, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &before);
        accepted(&store, &before, 500, t0);

        let after = make_generation(9, 9, BOOT_A, AGENT_1);
        let outcome = store.confirm_generation(DEVICE, after.clone());
        assert!(
            matches!(
                &outcome,
                GenerationOutcome::Replaced {
                    scope_changed: false,
                    seq_floor: 500,
                    ..
                }
            ),
            "同作用域重连必须保留下界 500，实得 {outcome:?}"
        );
        assert_eq!(
            store.read(&DEVICE).unwrap().sample_seq(),
            500,
            "重连不得清空最后好样本"
        );

        // 重连前那条流的迟到旧样本（seq 更小）必须仍被拒。
        let rejected = store.update(
            &DEVICE,
            &after,
            &status(BOOT_A, AGENT_1, 499),
            evidence(false),
            t0.checked_add(Duration::from_secs(1)).unwrap(),
        );
        assert!(matches!(
            rejected,
            Err(UpdateRejected::StaleSampleSeq {
                accepted: 500,
                incoming: 499
            })
        ));
        accepted(
            &store,
            &after,
            501,
            t0.checked_add(Duration::from_secs(1)).unwrap(),
        );
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 501);
    }

    /// 幂等确认与未知设备：未确认且从未更新的设备没有快照（HTTP 侧投影 unknown）。
    #[test]
    fn confirming_same_generation_twice_is_idempotent_and_unknown_device_has_no_snapshot() {
        let store = SnapshotStore::new();
        let g = make_generation(10, 10, BOOT_A, AGENT_1);
        assert_eq!(
            store.confirm_generation(DEVICE, g.clone()),
            GenerationOutcome::Established
        );
        assert_eq!(store.is_connected(&DEVICE), Some(true));
        assert_eq!(
            store.confirm_generation(DEVICE, g.clone()),
            GenerationOutcome::AlreadyActive
        );
        assert_eq!(store.is_connected(&DEVICE), Some(true));
        assert!(
            store.read(&DEVICE).is_none(),
            "已确认但尚无样本时必须 unknown"
        );
        assert_eq!(store.read(&[0x00u8; 32]), None);
        assert!(matches!(
            store.record_error(&[0x00u8; 32], &g, "TRANSPORT_UNAVAILABLE"),
            Err(UpdateRejected::NoActiveGeneration)
        ));
    }

    /// 采集失败：保留最后好样本与指标（绝不回填 0）、标记 error、好样本年龄不清零。
    #[test]
    fn record_error_keeps_last_good_sample_and_its_age_then_clears_on_next_good_sample() {
        let store = SnapshotStore::new();
        let g = make_generation(11, 11, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &g);
        accepted(&store, &g, 7, t0);

        store
            .record_error(&DEVICE, &g, "TRANSPORT_UNAVAILABLE")
            .expect("已确认代际必须可记录错误");
        let at = t0.checked_add(Duration::from_secs(60)).unwrap();
        let view = store.read(&DEVICE).expect("错误态必须保留最后好样本");
        assert_eq!(view.sample_seq(), 7);
        assert_eq!(view.metrics().len(), 1, "指标不得被清空或回填");
        assert_eq!(view.metrics()[0].value, Some(7.0), "未知值不得填 0");
        assert_eq!(view.last_error.as_deref(), Some("TRANSPORT_UNAVAILABLE"));
        assert_eq!(view.freshness_at(at), DeviceStatusFreshness::Error);
        assert_eq!(view.age_ms_at(at), 60_000, "错误态不得把好样本年龄清零");

        accepted(&store, &g, 8, at);
        let recovered = store.read(&DEVICE).expect("恢复后必须可读");
        assert_eq!(recovered.last_error, None, "好样本必须清除 error");
        assert_eq!(recovered.freshness_at(at), DeviceStatusFreshness::Fresh);
    }

    /// 状态源契约：未知设备 `Ok(None)`（不伪造）；已知设备投影 typed 快照 + 锚点 +
    /// error 保留 + 接收 TimeEvidence。
    #[tokio::test]
    async fn snapshot_source_projects_real_entry_and_none_never_fabricated() {
        let store = SnapshotStore::new();
        let source = &store;
        assert!(
            source
                .snapshot(&[0x00u8; 32])
                .await
                .expect("读取不得 Err")
                .is_none()
        );

        let g = make_generation(12, 12, BOOT_A, AGENT_1);
        confirmed(&store, &g);
        accepted(&store, &g, 3, Instant::now());
        let entry = source
            .snapshot(&DEVICE)
            .await
            .expect("读取不得 Err")
            .expect("已知设备必须有快照");
        assert_eq!(entry.snapshot.sample_seq, 3);
        assert_eq!(entry.freshness, DeviceStatusFreshness::Fresh);
        assert_eq!(entry.last_error, None);
        assert_eq!(
            entry.received_time.quality,
            crate::time::evidence::TimeQuality::SystemFallback
        );
        // 单调锚点必须由 update 注入并原样带出（HTTP 侧据此计算 age）。
        assert!(entry.received_at.elapsed().as_secs() < 5);

        store
            .record_error(&DEVICE, &g, "TRANSPORT_UNAVAILABLE")
            .unwrap();
        let errored = source
            .snapshot(&DEVICE)
            .await
            .unwrap()
            .expect("错误保留最后好样本");
        assert_eq!(errored.freshness, DeviceStatusFreshness::Error);
        assert_eq!(errored.snapshot.sample_seq, 3);
        assert_eq!(errored.last_error.as_deref(), Some("TRANSPORT_UNAVAILABLE"));
    }

    /// 状态源可原样挂载到 `DeviceStatusState`：`Send + Sync` + dyn 兼容。
    #[test]
    fn store_is_object_safe_shared_state_for_http_wiring() {
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<SnapshotStore>();
        assert_send_sync::<dyn DeviceStatusSnapshotSource>();
        // 唯一的 dyn 兼容性证明：Arc<SnapshotStore> 直接强制转换成 Arc<dyn Trait>。
        fn assert_future_send<'a>(f: DeviceStatusFuture<'a, Option<DeviceStatusCacheEntry>>) {
            fn requires_send<T: Send>(_: &T) {}
            requires_send(&f);
        }
        let dyn_source: Arc<dyn DeviceStatusSnapshotSource> = Arc::new(SnapshotStore::new());
        assert_future_send(dyn_source.snapshot(&DEVICE));
    }

    /// 并发读写：多写者交错写 1..=N，最终必须恰好是最大值；任意读者的观察序列
    /// 必须单调不减（无回退、无撕裂）。
    #[test]
    fn concurrent_writers_and_readers_never_regress_or_tear() {
        const WRITERS: u64 = 8;
        const PER_WRITER: u64 = 200;
        let store = Arc::new(SnapshotStore::new());
        let g = make_generation(13, 13, BOOT_A, AGENT_1);
        confirmed(&store, &g);

        let mut threads = Vec::new();
        for w in 0..WRITERS {
            let store = Arc::clone(&store);
            let g = g.clone();
            threads.push(std::thread::spawn(move || {
                for i in 0..PER_WRITER {
                    let seq = 1 + w * PER_WRITER + i;
                    let _ = store.update(
                        &DEVICE,
                        &g,
                        &status(BOOT_A, AGENT_1, seq),
                        evidence(false),
                        Instant::now(),
                    );
                }
            }));
        }
        for _ in 0..2 {
            let store = Arc::clone(&store);
            threads.push(std::thread::spawn(move || {
                let mut seen = 0u64;
                for _ in 0..2_000 {
                    if let Some(view) = store.read(&DEVICE) {
                        assert!(
                            view.sample_seq() >= seen,
                            "读者观察到序号回退：{seen} -> {}",
                            view.sample_seq()
                        );
                        seen = view.sample_seq();
                        // payload 与 sample_seq 必须同源，否则即为撕裂写。
                        assert_eq!(view.metrics()[0].value, Some(view.sample_seq() as f64));
                    }
                }
            }));
        }
        for t in threads {
            t.join().expect("线程不得 panic");
        }
        let final_seq = store.read(&DEVICE).expect("最终必须有快照").sample_seq();
        assert_eq!(
            final_seq,
            WRITERS * PER_WRITER,
            "最后写入的最大 seq 必须胜出"
        );
        assert_eq!(store.len(), 1);
    }

    /// P2：断开连接只复位连接标记，作用域 `(boot_id, agent_epoch)`、`seq_floor` 与
    /// 最后好样本必须跨「断开 → 同作用域重连」保留，否则迟到的旧 `sample_seq` 会重新
    /// 被接受。
    #[test]
    fn disconnect_keeps_seq_floor_so_late_sample_after_reconnect_is_still_rejected() {
        let store = SnapshotStore::new();
        let g1 = make_generation(21, 21, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &g1);
        accepted(&store, &g1, 500, t0);

        // 断连：只允许复位连接标记，必须原样交出下界与最后好样本。
        let at = t0.checked_add(Duration::from_secs(1)).unwrap();
        assert_eq!(
            store.disconnect_generation(&DEVICE, &g1),
            DisconnectOutcome::Disconnected {
                kept_seq_floor: 500,
                kept_sample_seq: Some(500),
            }
        );
        assert_eq!(
            store.is_connected(&DEVICE),
            Some(false),
            "连接必须标记为断开"
        );
        assert_eq!(
            store.read(&DEVICE).unwrap().sample_seq(),
            500,
            "断连不得清空最后好样本"
        );

        // 同 boot/agent 重连（仅 session/stream 变更）：下界 500 必须继续生效。
        let g2 = make_generation(22, 22, BOOT_A, AGENT_1);
        assert_eq!(
            confirm(&store, &g2),
            GenerationOutcome::Replaced {
                previous: g1.clone(),
                scope_changed: false,
                seq_floor: 500,
            },
            "同作用域重连必须保留下界 500"
        );
        assert_eq!(
            store.is_connected(&DEVICE),
            Some(true),
            "重连后连接必须复活"
        );

        // 断线前那条流的迟到旧样本必须仍被 StaleSampleSeq 拒绝。
        let rejected = store.update(
            &DEVICE,
            &g2,
            &status(BOOT_A, AGENT_1, 499),
            evidence(false),
            at,
        );
        assert!(
            matches!(
                rejected,
                Err(UpdateRejected::StaleSampleSeq {
                    accepted: 500,
                    incoming: 499
                })
            ),
            "seq_floor 被断连冲掉才会放行 499，实得 {rejected:?}"
        );
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 500, "零副作用");
        accepted(&store, &g2, 501, at);
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 501);
    }

    /// P2：旧连接延迟到达的清理回调绝不得误删或降级新连接刚确认的代际与快照。
    #[test]
    fn delayed_cleanup_from_previous_generation_never_touches_the_confirmed_one() {
        let store = SnapshotStore::new();
        let other_device = [0x66u8; 32];
        let g1 = make_generation(23, 23, BOOT_A, AGENT_1);
        let g2 = make_generation(24, 24, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &g1);
        accepted(&store, &g1, 500, t0);
        // 新连接已确认（同作用域重连），此时旧连接的清理才姗姗来迟。
        assert!(matches!(
            confirm(&store, &g2),
            GenerationOutcome::Replaced { .. }
        ));

        assert_eq!(
            store.disconnect_generation(&DEVICE, &g1),
            DisconnectOutcome::StaleGeneration,
            "旧代际的清理回调必须整体被拒"
        );
        assert_eq!(
            store.disconnect_generation(&other_device, &g2),
            DisconnectOutcome::NoActiveGeneration,
            "别的设备没有已确认代际"
        );
        assert_eq!(
            store.is_connected(&DEVICE),
            Some(true),
            "新代际连接不得被误标断开"
        );
        assert_eq!(
            store.read(&DEVICE).unwrap().sample_seq(),
            500,
            "新代际已确认的快照不得被旧清理抹掉"
        );
        assert_eq!(store.len(), 1, "被拒清理不得凭空增删条目");

        // 新代际照常可写，证明条目完好。
        let at = t0.checked_add(Duration::from_secs(1)).unwrap();
        accepted(&store, &g2, 501, at);
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 501);

        // 匹配当前代际的清理才生效，且仍保留作用域与样本。
        assert_eq!(
            store.disconnect_generation(&DEVICE, &g2),
            DisconnectOutcome::Disconnected {
                kept_seq_floor: 501,
                kept_sample_seq: Some(501),
            }
        );
        // 断开后同代际迟到的、序号仍严格递增的样本仍是可信数据（下界继续挡旧样本）。
        accepted(&store, &g2, 502, at);
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 502);
        assert_eq!(store.is_connected(&DEVICE), Some(false));
    }

    /// 审查补充：旧代际的 `record_error` 必须被拒，绝不把新代际的好样本降级为 error。
    #[test]
    fn record_error_from_stale_generation_is_rejected_without_downgrading_current_sample() {
        let store = SnapshotStore::new();
        let g1 = make_generation(25, 25, BOOT_A, AGENT_1);
        let g2 = make_generation(26, 26, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &g1);
        accepted(&store, &g1, 7, t0);
        assert!(matches!(
            confirm(&store, &g2),
            GenerationOutcome::Replaced { .. }
        ));

        assert_eq!(
            store
                .record_error(&DEVICE, &g1, "TRANSPORT_UNAVAILABLE")
                .err(),
            Some(UpdateRejected::StaleGeneration),
            "旧代际的错误回调必须拒绝"
        );
        let view = store.read(&DEVICE).expect("好样本必须仍在");
        assert_eq!(view.sample_seq(), 7);
        assert_eq!(view.last_error, None, "被拒的错误回调不得污染当前快照");
        assert_eq!(view.freshness_at(t0), DeviceStatusFreshness::Fresh);

        // 当前代际的错误仍可记录，证明条目未被这次拒绝损坏。
        store
            .record_error(&DEVICE, &g2, "TRANSPORT_UNAVAILABLE")
            .expect("当前确认代际必须可记录错误");
        assert_eq!(
            store.read(&DEVICE).unwrap().freshness_at(t0),
            DeviceStatusFreshness::Error
        );
    }

    /// 审查补充：仅 `boot_id` 变化就是作用域切换——下界归零、旧 boot 样本丢弃。
    #[test]
    fn boot_only_change_switches_the_sample_seq_scope() {
        let store = SnapshotStore::new();
        let before = make_generation(27, 27, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &before);
        accepted(&store, &before, 500, t0);

        // 同 agent、同 session/stream 字节，只有 boot 变化（板端重启）。
        let rebooted = make_generation(27, 27, BOOT_B, AGENT_1);
        assert_eq!(
            confirm(&store, &rebooted),
            GenerationOutcome::Replaced {
                previous: before.clone(),
                scope_changed: true,
                seq_floor: 0,
            },
            "仅 boot 变化必须判定为作用域切换"
        );
        assert!(
            store.read(&DEVICE).is_none(),
            "旧 boot 的样本不得继续冒充重启后的当前状态"
        );
        // 新作用域从 1 重新起算；旧作用域的 500 不得继续压制。
        accepted(
            &store,
            &rebooted,
            1,
            t0.checked_add(Duration::from_secs(1)).unwrap(),
        );
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 1);
    }

    /// 审查补充：仅 `agent_epoch` 变化（agent 升级重启）同样是作用域切换。
    #[test]
    fn agent_only_change_switches_the_sample_seq_scope() {
        let store = SnapshotStore::new();
        let before = make_generation(28, 28, BOOT_A, AGENT_1);
        let t0 = Instant::now();
        confirmed(&store, &before);
        accepted(&store, &before, 500, t0);

        let upgraded = make_generation(28, 28, BOOT_A, AGENT_2);
        assert_eq!(
            confirm(&store, &upgraded),
            GenerationOutcome::Replaced {
                previous: before.clone(),
                scope_changed: true,
                seq_floor: 0,
            },
            "仅 agent 变化必须判定为作用域切换"
        );
        assert!(store.read(&DEVICE).is_none(), "旧 agent 的样本必须丢弃");
        accepted(
            &store,
            &upgraded,
            1,
            t0.checked_add(Duration::from_secs(1)).unwrap(),
        );
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 1);
    }

    /// `forget` 的语义是「设备从系统中彻底移除」：条目、代际、下界与样本一并释放，
    /// 之后一切写入都必须重新经认证连接管理器确认，否则 fail-closed 拒绝。
    #[test]
    fn forget_removes_device_completely_and_requires_reconfirm() {
        let store = SnapshotStore::new();
        let g = make_generation(14, 14, BOOT_A, AGENT_1);
        confirmed(&store, &g);
        accepted(&store, &g, 1, Instant::now());

        assert!(store.forget(&DEVICE));
        assert!(!store.forget(&DEVICE), "重复移除必须返回 false");
        assert!(store.is_empty());
        assert!(store.read(&DEVICE).is_none());
        assert_eq!(
            store.is_connected(&DEVICE),
            None,
            "彻底移除后连接态也不存在"
        );

        // 已移除的设备：连同一代际的样本也不得再被接受（代际不复存在）。
        assert_eq!(
            store
                .update(
                    &DEVICE,
                    &g,
                    &status(BOOT_A, AGENT_1, 2),
                    evidence(false),
                    Instant::now(),
                )
                .err(),
            Some(UpdateRejected::NoActiveGeneration)
        );
        assert_eq!(
            store.disconnect_generation(&DEVICE, &g),
            DisconnectOutcome::NoActiveGeneration
        );
        assert!(store.is_empty(), "移除后的任何写入都不得复活条目");

        // 重新接入必须重新确认代际，且作用域从零起算。
        assert_eq!(
            confirm(&store, &g),
            GenerationOutcome::Established,
            "彻底移除后重新接入按首接入处理"
        );
        accepted(&store, &g, 1, Instant::now());
        assert_eq!(store.read(&DEVICE).unwrap().sample_seq(), 1);
    }
}

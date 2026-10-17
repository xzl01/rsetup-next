//! 30s 单调票据：每 controller 至多一个活跃票据，一次性消费，绑定入站来源代际。
//!
//! 期限判定只用 `std::time::Instant`（单调时钟），绝不使用 `SystemTime`，
//! 因此墙钟回拨、NTP 校正都不能延长票据有效期。
//! 本模块不含任何密码学实现，也不触达文件系统。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rsetup_protocol::wire::{BUSINESS_SCHEMA_VERSION, RebootTicket};

use crate::dispatch::BusinessStatus;

/// 票据有效期（秒）：03 §5 待审默认，G0 审阅后才可定稿。
pub const TICKET_TTL_SECS: u64 = 30;
/// 与 `TICKET_TTL_SECS` 同一合同的毫秒表达，随票据回给中控。
pub const TICKET_TTL_MS: u32 = 30_000;
/// 执行令牌定长字节数。
pub const TICKET_TOKEN_BYTES: usize = 32;

/// prepare 绑定：controller 公钥 + sub_task_id + expected_boot_id + 连接代际 + agent 实例。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TicketBinding {
    pub controller_key: [u8; 32],
    pub sub_task_id: String,
    pub expected_boot_id: String,
    pub connection_id: String,
    pub agent_instance_id: String,
}

/// consume 入站来源承载：连接代际 + agent 实例，两者都必须与 prepare 时绑定一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketOrigin<'a> {
    pub connection_id: &'a str,
    pub agent_instance_id: &'a str,
}

/// 一次性执行令牌。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TicketToken(pub [u8; TICKET_TOKEN_BYTES]);

/// 每 controller 一个活跃票据槽位（私有结构，不从 crate 导出）。
struct ActiveTicket {
    token: TicketToken,
    binding: TicketBinding,
    issued_at: Instant,
    consumed: bool,
}

/// 每 `controller_key` 至多一个活跃票据；已消费条目保留（拒绝重复）不计活跃。
pub struct TicketManager {
    map: Mutex<BTreeMap<[u8; 32], ActiveTicket>>,
}

impl TicketManager {
    pub fn new() -> Self {
        Self {
            map: Mutex::new(BTreeMap::new()),
        }
    }

    /// 覆盖该 controller 的活跃票据并返回票据（03 §4）。
    ///
    /// **32B token producer（熵门禁诚实声明）**：本 crate 允许的直接依赖
    /// （`rsetup-protocol`/`rsetup-core`/`tokio`/`uuid`/`thiserror`/`serde`/`serde_json`）内
    /// 没有经批准的真熵源 helper，故取两个 `Uuid::new_v4()` 的 16B 原始字节拼接构造 32B，
    /// 即 **244-bit effective entropy from dual UUIDv4**（每个 UUIDv4 固定 4 位 version +
    /// 2 位 variant，各 122 随机位），**不是 256-bit 全熵**。若日后上游导出经批准真熵源 helper，
    /// 接入点就是下方唯一 producer 位置，替换后须同步更新本注释。
    ///
    /// **poison 契约（fail-closed）**：`self.map.lock()` 中毒 → `panic!` 记录进程故障边界，
    /// 绝不吞毒后继续授票（tuple 签名无 `Result` 可回传，panic 即进程级故障边界）。
    pub fn prepare(&self, binding: TicketBinding, now: Instant) -> (TicketToken, RebootTicket) {
        // producer 唯一位置：dual UUIDv4 字节拼接（244-bit effective entropy，非 256-bit 全熵）。
        let first = uuid::Uuid::new_v4().into_bytes();
        let second = uuid::Uuid::new_v4().into_bytes();
        let mut raw = [0u8; TICKET_TOKEN_BYTES];
        raw[..16].copy_from_slice(&first);
        raw[16..].copy_from_slice(&second);
        let token = TicketToken(raw);
        let mut map = match self.map.lock() {
            Ok(guard) => guard,
            Err(_) => panic!("ticket map mutex poisoned: 进程故障边界（非安全业务状态）"),
        };
        // 覆盖写入：每 controller 至多一个活跃票据槽位（03 §4）。
        map.insert(
            binding.controller_key,
            ActiveTicket {
                token,
                binding: binding.clone(),
                issued_at: now,
                consumed: false,
            },
        );
        let ticket = RebootTicket {
            schema_version: BUSINESS_SCHEMA_VERSION,
            sub_task_id: binding.sub_task_id,
            boot_id: binding.expected_boot_id,
            execution_token: token.0,
            valid_for_ms: TICKET_TTL_MS,
        };
        (token, ticket)
    }

    /// 一次性消费（match-before-mutation）：同一临界区先只读定位，按
    /// controller（键）→ 未消费 → 单调期限 → `token` → 连接代际 → agent 实例
    /// → `sub_task_id` → `expected_boot_id` 严格匹配；任一不匹配即
    /// `Err(ExecutionTokenInvalid)` 且**保留原票据**（失败绝不消耗，错 task/错 boot
    /// 的入站请求不得丢票据）；全部通过才标记已消费（不删除条目，重复调用同样被拒）。
    ///
    /// `now` 早于 `issued_at`（客户端任意早的 `Instant`）由 `checked_duration_since`
    /// 拒绝，绝不靠 `duration_since` 饱和为 0 当合法。
    ///
    /// **poison 契约（fail-closed）**：毒锁 → 直接返回 `Err(ExecutionTokenInvalid)`
    /// （不 panic、不吞毒继续消耗）。std `Mutex` 中毒是持久的，本模块未实现
    /// `clear_poison`/重建恢复路径：该 manager 在本进程内保持 fail-closed，
    /// 需受控重建或进程重启，重建前不得 `execute`。
    pub fn consume(
        &self,
        controller_key: &[u8; 32],
        origin: TicketOrigin<'_>,
        sub_task_id: &str,
        expected_boot_id: &str,
        token: TicketToken,
        now: Instant,
    ) -> Result<TicketBinding, BusinessStatus> {
        let TicketOrigin {
            connection_id,
            agent_instance_id,
        } = origin;
        let mut map = match self.map.lock() {
            Ok(guard) => guard,
            // fail-closed：毒锁按「无有效票据」处理，不 panic、不吞毒继续消耗
            Err(_) => return Err(BusinessStatus::ExecutionTokenInvalid),
        };
        // (a) 只读定位（不 remove）：不存在或已消费 → 失败，保留现状
        let slot = match map.get(controller_key) {
            Some(slot) if !slot.consumed => slot,
            _ => return Err(BusinessStatus::ExecutionTokenInvalid),
        };
        // (b) 严格校验：此刻尚无任何状态变更
        let elapsed = match now.checked_duration_since(slot.issued_at) {
            Some(elapsed) => elapsed,
            // `now` 早于 `issued_at`：拒绝
            None => return Err(BusinessStatus::ExecutionTokenInvalid),
        };
        if slot.token != token
            || slot.binding.connection_id != connection_id
            || slot.binding.agent_instance_id != agent_instance_id
            || slot.binding.sub_task_id != sub_task_id
            || slot.binding.expected_boot_id != expected_boot_id
            || elapsed >= Duration::from_secs(TICKET_TTL_SECS)
        {
            return Err(BusinessStatus::ExecutionTokenInvalid);
        }
        // (c) 全部通过才变更状态：标记已消费并返回绑定（不删除条目）
        let slot = map.get_mut(controller_key).expect("slot present in (b)");
        slot.consumed = true;
        Ok(slot.binding.clone())
    }

    /// 连接断开或 agent 实例更换时作废。infallible 接口：毒锁只能 panic
    /// （可能的 task 线程 panic，非安全业务状态），绝不吞毒后称作废成功。
    pub fn invalidate(&self, controller_key: &[u8; 32]) {
        let mut map = self
            .map
            .lock()
            .expect("ticket map mutex poisoned: 进程故障边界（非安全业务状态）");
        map.remove(controller_key);
    }

    /// 活跃票据数：仅计 `consumed == false` 的条目（已消费但保留的条目不计活跃）。
    /// infallible 接口：毒锁只能 panic（可能的 task 线程 panic，非安全业务状态）。
    pub fn active_ticket_count(&self) -> usize {
        let map = self
            .map
            .lock()
            .expect("ticket map mutex poisoned: 进程故障边界（非安全业务状态）");
        map.values().filter(|ticket| !ticket.consumed).count()
    }
}

impl Default for TicketManager {
    fn default() -> Self {
        Self::new()
    }
}

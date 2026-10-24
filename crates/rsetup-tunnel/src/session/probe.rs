//! 逐流15s应用探测纯状态机。调用方必须驱动next_deadline并实际有界入队，
//! 不能因正常业务流量跳过tick，也不能重放过期窗口。
//!
//! Driver衔接顺序：收到响应先validate(stream)，再由PendingTable::match_response
//! 成功取出Probe条目，最后调用accept_pong。不得仅凭nonce跳过status/action/流校验。
//! RST/重开先close_stream(旧stream,旧epoch)结清所有旧等待者，再on_reset推进
//! ProbeState并插入新代际probe；新槽不能依赖旧条目到期才回收。
use super::pending::next_trace_id;
use super::types::StreamEpoch;
use crate::envelope::StreamKind;
use ring::rand::{SecureRandom, SystemRandom};
use std::time::Duration;
use tokio::time::Instant;

pub const PROBE_WINDOW: Duration = Duration::from_secs(15);
const FAILURE_LIMIT: u8 = 3;

#[derive(Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    pub stream: StreamKind,
    pub epoch: StreamEpoch,
    pub trace_id: u64,
    pub nonce: [u8; 16],
    pub deadline: Instant,
}
impl std::fmt::Debug for ProbeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProbeRequest")
            .field("stream", &self.stream)
            .field("epoch", &self.epoch)
            .field("trace_id", &self.trace_id)
            .field("nonce", &"<redacted>")
            .field("deadline", &self.deadline)
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeDecision {
    Idle,
    Send(ProbeRequest),
    Reopen { epoch: StreamEpoch },
    CloseConnection,
}

pub struct ProbeState {
    stream: StreamKind,
    epoch: StreamEpoch,
    opening_deadline: Option<Instant>,
    next_at: Instant,
    inflight: Option<ProbeRequest>,
    healthy: bool,
    consecutive_failures: u8,
    reopen_failures: u8,
    draining: bool,
    closed: bool,
}
impl ProbeState {
    pub fn new(stream: StreamKind, epoch: StreamEpoch, now: Instant) -> Self {
        Self {
            stream,
            epoch,
            opening_deadline: Some(now + PROBE_WINDOW),
            next_at: now,
            inflight: None,
            healthy: false,
            consecutive_failures: 0,
            reopen_failures: 0,
            draining: false,
            closed: false,
        }
    }
    pub fn epoch(&self) -> StreamEpoch {
        self.epoch
    }
    pub fn is_healthy(&self) -> bool {
        self.healthy && !self.closed && !self.draining && self.opening_deadline.is_none()
    }
    pub fn reopen_failures(&self) -> u8 {
        self.reopen_failures
    }
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.draining || self.closed {
            None
        } else {
            self.opening_deadline
                .or_else(|| self.inflight.as_ref().map(|p| p.deadline))
                .or(Some(self.next_at))
        }
    }
    /// 仅本代际、期限内开流能开始首探测；open本身不清重开失败计数。
    pub fn on_opened(&mut self, epoch: StreamEpoch, now: Instant) -> bool {
        if self.draining || self.closed || epoch != self.epoch {
            return false;
        }
        let Some(deadline) = self.opening_deadline else {
            return false;
        };
        if now >= deadline {
            return false;
        }
        self.opening_deadline = None;
        self.next_at = now;
        self.inflight = None;
        self.healthy = false;
        self.consecutive_failures = 0;
        true
    }
    /// 调用方先完成信封字段校验及PendingTable同流/代际匹配；此处只核验当前窗口证据。
    pub fn accept_pong(
        &mut self,
        trace_id: u64,
        nonce: &[u8; 16],
        epoch: StreamEpoch,
        now: Instant,
    ) -> bool {
        if self.draining || self.closed || epoch != self.epoch {
            return false;
        }
        let Some(p) = self.inflight.as_ref() else {
            return false;
        };
        if p.trace_id != trace_id || &p.nonce != nonce || now >= p.deadline {
            return false;
        }
        self.next_at = p.deadline;
        self.inflight = None;
        self.healthy = true;
        self.consecutive_failures = 0;
        self.reopen_failures = 0;
        true
    }
    fn close(&mut self) -> ProbeDecision {
        self.closed = true;
        self.healthy = false;
        self.inflight = None;
        self.opening_deadline = None;
        ProbeDecision::CloseConnection
    }
    fn reopen(&mut self, now: Instant) -> ProbeDecision {
        let Ok(epoch) = self.epoch.checked_next() else {
            return self.close();
        };
        self.epoch = epoch;
        self.healthy = false;
        self.inflight = None;
        self.consecutive_failures = 0;
        self.opening_deadline = Some(now + PROBE_WINDOW);
        self.next_at = now;
        ProbeDecision::Reopen { epoch }
    }
    fn failed_attempt(&mut self, now: Instant) -> ProbeDecision {
        self.reopen_failures = self.reopen_failures.saturating_add(1);
        if self.reopen_failures >= FAILURE_LIMIT {
            self.close()
        } else {
            self.reopen(now)
        }
    }
    fn missed_window(&mut self, now: Instant) -> Option<ProbeDecision> {
        if !self.healthy {
            return Some(self.failed_attempt(now));
        }
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= FAILURE_LIMIT {
            Some(self.reopen(now))
        } else {
            None
        }
    }
    pub fn tick(&mut self, now: Instant) -> ProbeDecision {
        if self.draining || self.closed {
            return ProbeDecision::Idle;
        }
        if let Some(deadline) = self.opening_deadline {
            return if now >= deadline {
                self.failed_attempt(now)
            } else {
                ProbeDecision::Idle
            };
        }
        if let Some(p) = self.inflight.as_ref() {
            if now < p.deadline {
                return ProbeDecision::Idle;
            }
            self.next_at = p.deadline;
            self.inflight = None;
            if let Some(recovery) = self.missed_window(now) {
                return recovery;
            }
        }
        // 至多三次miss即恢复，不随无限晚tick循环/补发历史probe。
        while now >= self.next_at + PROBE_WINDOW {
            self.next_at += PROBE_WINDOW;
            if let Some(recovery) = self.missed_window(now) {
                return recovery;
            }
        }
        if now < self.next_at {
            return ProbeDecision::Idle;
        }
        let mut nonce = [0u8; 16];
        if SystemRandom::new().fill(&mut nonce).is_err() {
            return self.close();
        }
        let Ok(trace_id) = next_trace_id() else {
            return self.close();
        };
        let p = ProbeRequest {
            stream: self.stream,
            epoch: self.epoch,
            trace_id,
            nonce,
            deadline: self.next_at + PROBE_WINDOW,
        };
        self.inflight = Some(p.clone());
        ProbeDecision::Send(p)
    }
    /// RST只影响当前代际。未过首次健康的流失败计为重开失败，已健康流立即重开。
    pub fn on_reset(&mut self, epoch: StreamEpoch, now: Instant) -> ProbeDecision {
        if self.draining || self.closed || epoch != self.epoch {
            return ProbeDecision::Idle;
        }
        if self.healthy {
            self.reopen(now)
        } else {
            self.failed_attempt(now)
        }
    }
    pub fn begin_drain(&mut self) {
        self.draining = true;
        self.inflight = None;
    }
}

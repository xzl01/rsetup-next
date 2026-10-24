//! 按连接计数的有界未决表；不预分配1024项、不保存无限墓碑。
use super::types::{RequestError, StreamEpoch};
use crate::envelope::{PacketType, StreamKind, TunnelPacket};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::time::Instant;

pub const MAX_PENDING: usize = 1024;
pub const PROBE_RESERVATION: usize = 2;
static NEXT_TRACE: AtomicU64 = AtomicU64::new(1);

/// MAX作为耗尽哨兵，不回绕；进程所有连接/角色使用同一分配器。
pub fn next_trace_id() -> Result<u64, RequestError> {
    allocate_trace(&NEXT_TRACE)
}
fn allocate_trace(counter: &AtomicU64) -> Result<u64, RequestError> {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
        .map_err(|_| RequestError::TraceExhausted)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestKey {
    pub trace_id: u64,
    pub stream: StreamKind,
    pub epoch: StreamEpoch,
    pub action: String,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Business,
    Probe { nonce: [u8; 16] },
}
impl std::fmt::Debug for PendingKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Business => f.write_str("Business"),
            Self::Probe { .. } => f.write_str("Probe(<redacted>)"),
        }
    }
}
pub struct Matched<T> {
    pub value: T,
    pub kind: PendingKind,
}
struct Entry<T> {
    key: RequestKey,
    deadline: Instant,
    kind: PendingKind,
    value: T,
}
pub struct PendingTable<T> {
    entries: HashMap<u64, Entry<T>>,
    limit: usize,
    business: usize,
}
impl<T> PendingTable<T> {
    pub fn new(limit: usize) -> Result<Self, RequestError> {
        if !(PROBE_RESERVATION..=MAX_PENDING).contains(&limit) {
            return Err(RequestError::ProtocolMismatch);
        }
        Ok(Self {
            entries: HashMap::new(),
            limit,
            business: 0,
        })
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn insert_business(
        &mut self,
        key: RequestKey,
        deadline: Instant,
        value: T,
    ) -> Result<(), RequestError> {
        self.insert(key, deadline, PendingKind::Business, value)
    }
    pub fn insert_probe(
        &mut self,
        key: RequestKey,
        deadline: Instant,
        nonce: [u8; 16],
        value: T,
    ) -> Result<(), RequestError> {
        if key.action != "tunnel.ping" {
            return Err(RequestError::ProtocolMismatch);
        }
        self.insert(key, deadline, PendingKind::Probe { nonce }, value)
    }
    fn insert(
        &mut self,
        key: RequestKey,
        deadline: Instant,
        kind: PendingKind,
        value: T,
    ) -> Result<(), RequestError> {
        if key.trace_id == 0
            || key.action.is_empty()
            || key.action.len() > 64
            || self.entries.contains_key(&key.trace_id)
        {
            return Err(RequestError::ProtocolMismatch);
        }
        if self.entries.len() >= self.limit {
            return Err(RequestError::Busy);
        }
        match kind {
            PendingKind::Business => {
                if self.business >= self.limit - PROBE_RESERVATION {
                    return Err(RequestError::Busy);
                }
                self.business += 1;
            }
            PendingKind::Probe { .. } => {
                if self.entries.values().any(|e| {
                    e.key.stream == key.stream && matches!(e.kind, PendingKind::Probe { .. })
                }) {
                    return Err(RequestError::Busy);
                }
            }
        }
        self.entries.insert(
            key.trace_id,
            Entry {
                key,
                deadline,
                kind,
                value,
            },
        );
        Ok(())
    }
    /// 未知、迟到、错流/错代际/错action响应不交付。过期项由expire统一结清，
    /// 非法已知probe不会取走等待者或刷新原deadline。
    pub fn match_response(
        &mut self,
        packet: &TunnelPacket,
        stream: StreamKind,
        epoch: StreamEpoch,
        now: Instant,
    ) -> Option<Matched<T>> {
        if packet.packet_type != PacketType::Response || packet.validate(stream).is_err() {
            return None;
        }
        let entry = self.entries.get(&packet.trace_id)?;
        if entry.key.stream != stream
            || entry.key.epoch != epoch
            || entry.key.action != packet.action
            || now >= entry.deadline
        {
            return None;
        }
        if let PendingKind::Probe { nonce } = entry.kind
            && (packet.payload.as_slice() != nonce
                || packet.status_code != 0
                || !packet.error_message.is_empty()
                || !packet.metadata.is_empty())
        {
            return None;
        }
        self.remove(packet.trace_id).map(|entry| Matched {
            value: entry.value,
            kind: entry.kind,
        })
    }
    fn remove(&mut self, id: u64) -> Option<Entry<T>> {
        let entry = self.entries.remove(&id)?;
        if matches!(entry.kind, PendingKind::Business) {
            self.business -= 1;
        }
        Some(entry)
    }
    pub fn expire(&mut self, now: Instant) -> Vec<T> {
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| now >= e.deadline)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.remove(id).map(|e| e.value))
            .collect()
    }
    pub fn close_stream(&mut self, stream: StreamKind, epoch: StreamEpoch) -> Vec<T> {
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| e.key.stream == stream && e.key.epoch == epoch)
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.remove(id).map(|e| e.value))
            .collect()
    }
    pub fn close_all(&mut self) -> Vec<T> {
        self.business = 0;
        self.entries.drain().map(|(_, entry)| entry.value).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocator_does_not_wrap_after_last_allocatable_id() {
        let c = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_trace(&c), Ok(u64::MAX - 1));
        assert_eq!(allocate_trace(&c), Err(RequestError::TraceExhausted));
        assert_eq!(allocate_trace(&c), Err(RequestError::TraceExhausted));
        assert_eq!(c.load(Ordering::Relaxed), u64::MAX);
    }
}

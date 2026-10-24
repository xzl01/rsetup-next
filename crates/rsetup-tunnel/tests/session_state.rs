use rsetup_tunnel::envelope::{PacketType, StreamKind, TunnelPacket};
use rsetup_tunnel::session::pending::{PendingTable, RequestKey, next_trace_id};
use rsetup_tunnel::session::probe::{PROBE_WINDOW, ProbeDecision, ProbeState};
use rsetup_tunnel::session::types::{RequestError, StreamEpoch};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::time::Instant;

fn key(id: u64, stream: StreamKind, epoch: u64, action: &str) -> RequestKey {
    RequestKey {
        trace_id: id,
        stream,
        epoch: StreamEpoch(epoch),
        action: action.into(),
    }
}
fn reply(id: u64, action: &str, payload: Vec<u8>) -> TunnelPacket {
    TunnelPacket {
        trace_id: id,
        packet_type: PacketType::Response,
        action: action.into(),
        status_code: 0,
        error_message: String::new(),
        payload,
        metadata: BTreeMap::new(),
    }
}
#[test]
fn pending_reserves_two_slots_for_the_two_stream_probes() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(5);
    let mut t = PendingTable::new(1024).unwrap();
    for i in 1..=1022 {
        t.insert_business(
            key(i, StreamKind::Control, 1, "device.status.get"),
            deadline,
            i,
        )
        .unwrap();
    }
    assert_eq!(
        t.insert_business(
            key(1023, StreamKind::Data, 1, "device.status.get"),
            deadline,
            0
        ),
        Err(RequestError::Busy)
    );
    t.insert_probe(
        key(2000, StreamKind::Control, 1, "tunnel.ping"),
        deadline,
        [1; 16],
        2000,
    )
    .unwrap();
    t.insert_probe(
        key(2001, StreamKind::Data, 1, "tunnel.ping"),
        deadline,
        [2; 16],
        2001,
    )
    .unwrap();
    assert_eq!(t.len(), 1024);
    assert_eq!(
        t.insert_probe(
            key(2002, StreamKind::Data, 1, "tunnel.ping"),
            deadline,
            [3; 16],
            2002
        ),
        Err(RequestError::Busy)
    );
    assert_eq!(t.expire(deadline).len(), 1024);
    assert!(t.is_empty());
}
#[test]
fn duplicate_trace_cannot_replace_an_existing_waiter() {
    let now = Instant::now();
    let mut t = PendingTable::new(8).unwrap();
    t.insert_business(
        key(1, StreamKind::Control, 1, "device.status.get"),
        now + Duration::from_secs(5),
        "first",
    )
    .unwrap();
    assert_eq!(
        t.insert_business(
            key(1, StreamKind::Data, 2, "device.clock.get"),
            now + Duration::from_secs(5),
            "replacement"
        ),
        Err(RequestError::ProtocolMismatch)
    );
    assert_eq!(t.close_all(), vec!["first"]);
    assert!(t.close_all().is_empty());
}
#[test]
fn response_requires_same_stream_epoch_action_and_deadline() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(5);
    let mut t = PendingTable::new(8).unwrap();
    t.insert_business(
        key(1, StreamKind::Data, 4, "device.status.get"),
        deadline,
        "waiting",
    )
    .unwrap();
    let p = reply(1, "device.status.get", vec![]);
    assert!(
        t.match_response(&p, StreamKind::Control, StreamEpoch(4), now)
            .is_none()
    );
    assert!(
        t.match_response(&p, StreamKind::Data, StreamEpoch(3), now)
            .is_none()
    );
    assert!(
        t.match_response(
            &reply(1, "device.clock.get", vec![]),
            StreamKind::Data,
            StreamEpoch(4),
            now
        )
        .is_none()
    );
    assert!(
        t.match_response(&p, StreamKind::Data, StreamEpoch(4), deadline)
            .is_none()
    );
    assert_eq!(t.len(), 1);
    let matched = t
        .match_response(&p, StreamKind::Data, StreamEpoch(4), now)
        .unwrap();
    assert_eq!(matched.value, "waiting");
    assert!(t.is_empty());
    assert!(
        t.match_response(&p, StreamKind::Data, StreamEpoch(4), now)
            .is_none()
    );
}
#[test]
fn malformed_known_probe_does_not_take_or_refresh_waiter() {
    let now = Instant::now();
    let deadline = now + Duration::from_secs(15);
    let mut t = PendingTable::new(8).unwrap();
    t.insert_probe(
        key(8, StreamKind::Control, 5, "tunnel.ping"),
        deadline,
        [7; 16],
        8,
    )
    .unwrap();
    let wrong = reply(8, "tunnel.ping", vec![9; 16]);
    assert!(
        t.match_response(&wrong, StreamKind::Control, StreamEpoch(5), now)
            .is_none()
    );
    assert_eq!(t.len(), 1);
    let right = reply(8, "tunnel.ping", vec![7; 16]);
    assert_eq!(
        t.match_response(&right, StreamKind::Control, StreamEpoch(5), now)
            .unwrap()
            .value,
        8
    );
}
#[test]
fn old_generation_cleanup_does_not_touch_new_generation() {
    let now = Instant::now();
    let mut t = PendingTable::new(8).unwrap();
    t.insert_business(
        key(1, StreamKind::Control, 1, "device.status.get"),
        now + Duration::from_secs(5),
        1,
    )
    .unwrap();
    t.insert_business(
        key(2, StreamKind::Control, 2, "device.status.get"),
        now + Duration::from_secs(5),
        2,
    )
    .unwrap();
    t.insert_business(
        key(3, StreamKind::Data, 1, "device.status.get"),
        now + Duration::from_secs(5),
        3,
    )
    .unwrap();
    assert_eq!(t.close_stream(StreamKind::Control, StreamEpoch(1)), vec![1]);
    assert!(
        t.close_stream(StreamKind::Control, StreamEpoch(1))
            .is_empty()
    );
    assert_eq!(t.len(), 2);
    let mut rest = t.close_all();
    rest.sort();
    assert_eq!(rest, vec![2, 3]);
}
#[test]
fn trace_ids_are_unique_across_concurrent_allocators() {
    let workers: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                (0..128)
                    .map(|_| next_trace_id().unwrap())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let mut all = Vec::new();
    for w in workers {
        all.extend(w.join().unwrap());
    }
    assert!(all.iter().all(|id| *id > 0));
    all.sort_unstable();
    all.dedup();
    assert_eq!(all.len(), 1024);
}
fn send(state: &mut ProbeState, now: Instant) -> rsetup_tunnel::session::probe::ProbeRequest {
    match state.tick(now) {
        ProbeDecision::Send(p) => p,
        other => panic!("expected probe, got {other:?}"),
    }
}
#[test]
fn both_streams_send_first_probe_immediately_and_success_is_local() {
    let now = Instant::now();
    for stream in [StreamKind::Control, StreamKind::Data] {
        let mut s = ProbeState::new(stream, StreamEpoch(1), now);
        assert!(s.on_opened(StreamEpoch(1), now));
        let p = send(&mut s, now);
        assert_eq!(p.deadline, now + PROBE_WINDOW);
        assert!(!s.is_healthy());
        assert!(s.accept_pong(p.trace_id, &p.nonce, StreamEpoch(1), now));
        assert!(s.is_healthy());
        assert!(matches!(s.tick(now), ProbeDecision::Idle));
        let next = send(&mut s, now + PROBE_WINDOW);
        assert!(next.trace_id > p.trace_id);
        assert!(!s.accept_pong(p.trace_id, &p.nonce, StreamEpoch(1), now + PROBE_WINDOW));
    }
}
#[test]
fn probe_three_misses_reopen_only_that_stream() {
    let now = Instant::now();
    let mut s = ProbeState::new(StreamKind::Data, StreamEpoch(9), now);
    assert!(s.on_opened(StreamEpoch(9), now));
    let p = send(&mut s, now);
    assert!(s.accept_pong(p.trace_id, &p.nonce, StreamEpoch(9), now));
    let first = send(&mut s, now + PROBE_WINDOW);
    assert!(matches!(s.tick(first.deadline), ProbeDecision::Send(_)));
    assert!(matches!(
        s.tick(first.deadline + PROBE_WINDOW),
        ProbeDecision::Send(_)
    ));
    assert!(matches!(
        s.tick(first.deadline + PROBE_WINDOW * 2),
        ProbeDecision::Reopen {
            epoch: StreamEpoch(10)
        }
    ));
    assert_eq!(s.epoch(), StreamEpoch(10));
    assert!(!s.is_healthy());
}
#[test]
fn three_open_or_first_health_failures_escalate_connection() {
    let now = Instant::now();
    let mut s = ProbeState::new(StreamKind::Control, StreamEpoch(1), now);
    assert!(matches!(
        s.tick(now + PROBE_WINDOW),
        ProbeDecision::Reopen {
            epoch: StreamEpoch(2)
        }
    ));
    assert!(s.on_opened(StreamEpoch(2), now + PROBE_WINDOW));
    let p = send(&mut s, now + PROBE_WINDOW);
    assert!(matches!(
        s.tick(p.deadline),
        ProbeDecision::Reopen {
            epoch: StreamEpoch(3)
        }
    ));
    assert!(matches!(
        s.tick(p.deadline + PROBE_WINDOW),
        ProbeDecision::CloseConnection
    ));
}
#[test]
fn successful_first_pong_resets_reopen_failure_count_not_open_alone() {
    let now = Instant::now();
    let mut s = ProbeState::new(StreamKind::Data, StreamEpoch(1), now);
    assert!(matches!(
        s.tick(now + PROBE_WINDOW),
        ProbeDecision::Reopen { .. }
    ));
    assert_eq!(s.reopen_failures(), 1);
    assert!(s.on_opened(StreamEpoch(2), now + PROBE_WINDOW));
    assert_eq!(s.reopen_failures(), 1);
    let p = send(&mut s, now + PROBE_WINDOW);
    let bad = [0; 16];
    assert!(!s.accept_pong(p.trace_id, &bad, StreamEpoch(2), now + PROBE_WINDOW));
    assert_eq!(s.reopen_failures(), 1);
    assert!(s.accept_pong(p.trace_id, &p.nonce, StreamEpoch(2), now + PROBE_WINDOW));
    assert_eq!(s.reopen_failures(), 0);
}
#[test]
fn delayed_tick_does_not_replay_history_and_drain_never_reopens() {
    let now = Instant::now();
    let mut s = ProbeState::new(StreamKind::Data, StreamEpoch(1), now);
    assert!(s.on_opened(StreamEpoch(1), now));
    let p = send(&mut s, now);
    assert!(s.accept_pong(p.trace_id, &p.nonce, StreamEpoch(1), now));
    assert!(matches!(
        s.tick(now + PROBE_WINDOW * 4),
        ProbeDecision::Reopen { .. }
    ));
    s.begin_drain();
    assert!(matches!(
        s.tick(now + PROBE_WINDOW * 100),
        ProbeDecision::Idle
    ));
    assert!(matches!(
        s.on_reset(s.epoch(), now + PROBE_WINDOW * 100),
        ProbeDecision::Idle
    ));
}

#[test]
fn budget_is_reclaimed_without_tombstones_and_epoch_never_wraps() {
    let now = Instant::now();
    assert_eq!(
        StreamEpoch(u64::MAX).checked_next(),
        Err(RequestError::Closed)
    );
    assert!(PendingTable::<u8>::new(1).is_err());
    assert!(PendingTable::<u8>::new(1025).is_err());
    let mut table = PendingTable::new(3).unwrap();
    for id in 1..100 {
        table
            .insert_business(key(id, StreamKind::Data, 1, "device.status.get"), now, id)
            .unwrap();
        assert_eq!(table.expire(now), vec![id]);
        assert!(table.is_empty());
    }
}

#[test]
fn draining_stream_is_not_reported_business_healthy() {
    let now = Instant::now();
    let mut state = ProbeState::new(StreamKind::Control, StreamEpoch(1), now);
    assert!(state.on_opened(StreamEpoch(1), now));
    let probe = send(&mut state, now);
    assert!(state.accept_pong(probe.trace_id, &probe.nonce, StreamEpoch(1), now));
    assert!(state.is_healthy());
    state.begin_drain();
    assert!(!state.is_healthy());
    assert!(state.next_deadline().is_none());
}

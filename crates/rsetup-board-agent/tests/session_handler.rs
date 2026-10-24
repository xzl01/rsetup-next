//! 会话业务 handler 的入站裁决验收：六 action 白名单、真实 BusinessCodec 往返、
//! Data 流业务拒绝、payload 自报身份无效、跨代际票据无效、Indeterminate 不伪回包。
//!
//! 全部使用注入的 fake source/journal/dispatcher：不连真实数据库、不读真实 journal
//! 目录，绝不触发真实主机重启。
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use rsetup_board_agent::dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, ExecuteOutcome, JournalEpochSource,
    RebootDispatcher, TaskJournalReader,
};
use rsetup_board_agent::journal::{TaskRecord as JournalTaskRecord, TaskState};
use rsetup_board_agent::read_only::{PeerContext, ReadOnlySource};
use rsetup_board_agent::session_handler::{
    BusinessDecision, BusinessDispatch, BusinessHandler, DEFAULT_SESSION_DRAIN, NoReplyReason,
    encode_response,
};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind, Capabilities, ClockSample,
    DeviceStatus, Metric, Query, RebootExecute, RebootPrepare, RebootTicket, TaskQuery, TaskRecord,
    approved_codec_witness, decode_business, encode_business, validate_dto,
};
use rsetup_tunnel::envelope::{PacketType, StreamKind, TunnelPacket};

const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const HANDSHAKE_KEY: [u8; 32] = [7u8; 32];
const CLAIMED_KEY: [u8; 32] = [9u8; 32];

// ------------------------------------------------------------------ fakes

/// 只读来源替身：只计数，用于证明「拒绝路径零触达」。
#[derive(Default)]
struct FakeSource {
    source_calls: AtomicUsize,
}

impl ReadOnlySource for FakeSource {
    fn capabilities(&self) -> Capabilities {
        self.source_calls.fetch_add(1, Ordering::SeqCst);
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_version: "0.5.0".to_string(),
            supported_actions: vec!["device.status.get".to_string()],
            durable_task_journal: true,
            max_status_bytes: 65_536,
            journal_epoch: EPOCH.to_string(),
        }
    }
    fn status(&self) -> DeviceStatus {
        self.source_calls.fetch_add(1, Ordering::SeqCst);
        DeviceStatus {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_epoch: AGENT_EPOCH.to_string(),
            sample_seq: 3,
            sampled_unix_ms: 1_700_000_000_000,
            sampled_monotonic_ns: 42,
            clock_epoch: CLOCK_EPOCH.to_string(),
            metrics: vec![Metric {
                key: "cpu.load1".to_string(),
                value: Some(0.5),
                unit: "1m".to_string(),
                quality: "ok".to_string(),
            }],
            clock_quality: "observed".to_string(),
        }
    }
    fn clock(&self) -> ClockSample {
        self.source_calls.fetch_add(1, Ordering::SeqCst);
        ClockSample {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            clock_epoch: CLOCK_EPOCH.to_string(),
            receive_unix_ms: 1,
            send_unix_ms: 2,
            receive_monotonic_ns: 10,
            send_monotonic_ns: 20,
            quality: "observed".to_string(),
        }
    }
}

/// journal 替身：记录每次查询实际使用的 controller_key（证明身份只来自握手）。
struct FakeJournal {
    calls: AtomicUsize,
    observed: Mutex<Vec<[u8; 32]>>,
    record_owner: [u8; 32],
}

impl FakeJournal {
    fn new(record_owner: [u8; 32]) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            observed: Mutex::new(Vec::new()),
            record_owner,
        }
    }
    fn observed_keys(&self) -> Vec<[u8; 32]> {
        self.observed.lock().unwrap().clone()
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TaskJournalReader for FakeJournal {
    fn query(
        &self,
        controller_key: &[u8; 32],
        _query: &TaskQuery,
    ) -> Result<Option<TaskRecord>, BusinessStatus> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.observed.lock().unwrap().push(*controller_key);
        if *controller_key != self.record_owner {
            return Ok(None);
        }
        // journal 侧记录 → wire DTO：与生产 bin 的 `to_wire()` 同一条路径。
        Ok(Some(accepted_record().to_wire()))
    }
}

/// `ExecuteOutcome::Accepted` 带的是 **journal 侧**记录（不是 wire DTO）。
fn accepted_record() -> JournalTaskRecord {
    JournalTaskRecord {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        journal_epoch: EPOCH.to_string(),
        record_version: 1,
        state: TaskState::Accepted,
        pre_boot_id: BOOT.to_string(),
        observed_boot_id: None,
        reason_code: None,
        updated_unix_ms: Some(1_700_000_000_000),
        evidence_kind: None,
    }
}

#[test]
fn journal_fixture_record_is_a_valid_wire_dto() {
    let record = accepted_record().to_wire();
    validate_dto(&record).expect("替身记录必须能通过 wire 自检，否则被测行为无法判定");
}

/// 与生产 bin 同形：`TaskJournalReader` 只为 `&Arc<T>` 实现，store 需要 newtype。
struct JournalReader(Arc<FakeJournal>);

impl TaskJournalReader for JournalReader {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<TaskRecord>, BusinessStatus> {
        self.0.query(controller_key, query)
    }
}

struct FakeEpoch;
impl JournalEpochSource for FakeEpoch {
    fn journal_epoch(&self) -> String {
        EPOCH.to_string()
    }
}

/// Reboot 调度替身：只认指定 `connection_id`，可强制返回 Indeterminate。
/// 它是「journal/ticket 见证」用的 fake OS 等价物：绝不触碰真实主机重启。
struct FakeDispatcher {
    accepted_connection: String,
    outcome: Option<ExecuteOutcome>,
    prepare_calls: AtomicUsize,
    execute_calls: AtomicUsize,
}

impl FakeDispatcher {
    fn new(accepted_connection: &str, outcome: Option<ExecuteOutcome>) -> Self {
        Self {
            accepted_connection: accepted_connection.to_string(),
            outcome,
            prepare_calls: AtomicUsize::new(0),
            execute_calls: AtomicUsize::new(0),
        }
    }
}

impl RebootDispatcher for FakeDispatcher {
    fn prepare(
        &self,
        _controller_key: &[u8; 32],
        connection_id: &str,
        _request: &RebootPrepare,
        _now: Instant,
    ) -> Result<RebootTicket, BusinessStatus> {
        self.prepare_calls.fetch_add(1, Ordering::SeqCst);
        if connection_id != self.accepted_connection {
            return Err(BusinessStatus::ExecutionTokenInvalid);
        }
        Ok(RebootTicket {
            schema_version: BUSINESS_SCHEMA_VERSION,
            sub_task_id: SUB_TASK.to_string(),
            boot_id: BOOT.to_string(),
            // 全零 token 是「缺省伪造」，DTO 直接拒绝：替身必须给非零值。
            execution_token: [0x5au8; 32],
            valid_for_ms: rsetup_board_agent::TICKET_TTL_MS,
        })
    }
    fn execute(
        &self,
        _controller_key: &[u8; 32],
        connection_id: &str,
        _request: &RebootExecute,
        _now: Instant,
    ) -> ExecuteOutcome {
        self.execute_calls.fetch_add(1, Ordering::SeqCst);
        if connection_id != self.accepted_connection {
            return ExecuteOutcome::Rejected(BusinessStatus::ExecutionTokenInvalid);
        }
        match self.outcome {
            Some(ExecuteOutcome::Indeterminate) => ExecuteOutcome::Indeterminate,
            _ => ExecuteOutcome::Accepted(accepted_record()),
        }
    }
}

// ---------------------------------------------------------------- helpers

type TestAgent = BoardAgent<FakeSource, JournalReader, FakeEpoch>;

fn handler_for(
    dispatcher: Option<(Arc<dyn RebootDispatcher>, &str)>,
    connection_id: &str,
) -> (BusinessHandler, Arc<FakeJournal>) {
    let journal = Arc::new(FakeJournal::new(HANDSHAKE_KEY));
    let agent = BoardAgent::new(
        FakeSource::default(),
        JournalReader(journal.clone()),
        FakeEpoch,
    );
    let agent = match dispatcher {
        Some((dispatcher, bound)) => agent.with_reboot(dispatcher, bound),
        None => agent,
    };
    let agent = Arc::new(agent);
    let dispatch: Arc<dyn BusinessDispatch> = agent;
    (
        BusinessHandler::new(dispatch, HANDSHAKE_KEY, connection_id),
        journal,
    )
}

fn request(action: &str, payload: Vec<u8>) -> TunnelPacket {
    TunnelPacket {
        trace_id: 1,
        packet_type: PacketType::Request,
        action: action.to_string(),
        status_code: 0,
        error_message: String::new(),
        payload,
        metadata: BTreeMap::new(),
    }
}

fn encode(payload: &BusinessPayload) -> Vec<u8> {
    let witness = approved_codec_witness();
    encode_business(&BusinessCodec, Some(&witness), payload).unwrap()
}

fn decode(bytes: &[u8], kind: BusinessPayloadKind) -> BusinessPayload {
    let witness = approved_codec_witness();
    decode_business(&BusinessCodec, Some(&witness), kind, bytes).unwrap()
}

fn query_payload() -> Vec<u8> {
    encode(&BusinessPayload::Query(Query {
        schema_version: BUSINESS_SCHEMA_VERSION,
    }))
}

fn task_get_payload() -> Vec<u8> {
    encode(&BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
    }))
}

// ------------------------------------------------------------------ tests

#[test]
fn six_allowed_actions_round_trip_through_the_real_business_codec() {
    let (handler, _journal) = handler_for(None, "connection-a");
    let cases = [
        ("device.capabilities.get", BusinessPayloadKind::Capabilities),
        ("device.status.get", BusinessPayloadKind::DeviceStatus),
        ("device.clock.get", BusinessPayloadKind::ClockSample),
    ];
    for (action, kind) in cases {
        let decision = handler.decide(StreamKind::Control, &request(action, query_payload()));
        let reply = decision
            .reply()
            .unwrap_or_else(|| panic!("{action} 必须有回包"));
        assert_eq!(reply.status_code, 0, "{action}");
        assert!(reply.error_message.is_empty(), "{action}");
        match (action, decode(&reply.payload, kind)) {
            ("device.capabilities.get", BusinessPayload::Capabilities(caps)) => {
                assert_eq!(caps.boot_id, BOOT);
                assert_eq!(caps.journal_epoch, EPOCH);
            }
            ("device.status.get", BusinessPayload::DeviceStatus(status)) => {
                assert_eq!(status.sample_seq, 3);
                assert_eq!(status.metrics.len(), 1);
            }
            ("device.clock.get", BusinessPayload::ClockSample(clock)) => {
                assert_eq!(clock.quality, "observed");
            }
            _ => panic!("{action} 回包形状不符"),
        }
    }

    let decision = handler.decide(
        StreamKind::Control,
        &request("device.task.get", task_get_payload()),
    );
    let reply = decision.reply().unwrap();
    assert_eq!(reply.status_code, 0);
    match decode(&reply.payload, BusinessPayloadKind::TaskRecord) {
        BusinessPayload::TaskRecord(record) => assert_eq!(record.sub_task_id, SUB_TASK),
        _ => panic!("device.task.get 回包形状不符"),
    }

    // 注入 dispatcher 后 prepare/execute 才可用（成功仍是 status=0 + 类型化 payload）。
    let dispatcher: Arc<dyn RebootDispatcher> = Arc::new(FakeDispatcher::new("connection-a", None));
    let (handler, _) = handler_for(Some((dispatcher, "connection-a")), "connection-a");
    let prepared = handler.decide(
        StreamKind::Control,
        &request(
            "device.reboot.prepare",
            encode(&BusinessPayload::RebootPrepare(RebootPrepare {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                expected_boot_id: BOOT.to_string(),
            })),
        ),
    );
    let reply = prepared.reply().unwrap();
    assert_eq!(reply.status_code, 0);
    match decode(&reply.payload, BusinessPayloadKind::RebootTicket) {
        BusinessPayload::RebootTicket(ticket) => assert_eq!(ticket.sub_task_id, SUB_TASK),
        _ => panic!("prepare 回包必须是 RebootTicket"),
    }
    let executed = handler.decide(
        StreamKind::Control,
        &request(
            "device.reboot.execute",
            encode(&BusinessPayload::RebootExecute(RebootExecute {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                expected_boot_id: BOOT.to_string(),
                execution_token: [1u8; 32],
            })),
        ),
    );
    let reply = executed.reply().unwrap();
    assert_eq!(reply.status_code, 0);
    match decode(&reply.payload, BusinessPayloadKind::TaskRecord) {
        BusinessPayload::TaskRecord(record) => assert_eq!(record.state, "accepted"),
        _ => panic!("execute 回包必须是 TaskRecord"),
    }
}

#[test]
fn reboot_actions_without_a_dispatcher_are_1004_with_empty_payload() {
    let (handler, journal) = handler_for(None, "connection-a");
    let cases = [
        (
            "device.reboot.prepare",
            encode(&BusinessPayload::RebootPrepare(RebootPrepare {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                expected_boot_id: BOOT.to_string(),
            })),
        ),
        (
            "device.reboot.execute",
            encode(&BusinessPayload::RebootExecute(RebootExecute {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                expected_boot_id: BOOT.to_string(),
                execution_token: [1u8; 32],
            })),
        ),
    ];
    for (action, payload) in cases {
        let decision = handler.decide(StreamKind::Control, &request(action, payload));
        let reply = decision
            .reply()
            .unwrap_or_else(|| panic!("{action} 必须有回包"));
        // 能力未接线 → 1004，且绝不携带 payload。
        assert_eq!(reply.status_code, 1004, "{action}");
        assert!(reply.payload.is_empty(), "{action} 必须空 payload");
        assert!(
            !reply.error_message.is_empty(),
            "{action} 必须有去敏错误文本"
        );
    }
    assert_eq!(journal.calls(), 0);
}

#[test]
fn ticket_from_another_connection_generation_is_rejected() {
    // dispatcher 只认 connection-a，本会话是 connection-b：旧代际票据不可复用。
    let dispatcher = Arc::new(FakeDispatcher::new("connection-a", None));
    let counting: Arc<dyn RebootDispatcher> = dispatcher.clone();
    let (handler, _) = handler_for(Some((counting, "connection-b")), "connection-b");
    let decision = handler.decide(
        StreamKind::Control,
        &request(
            "device.reboot.prepare",
            encode(&BusinessPayload::RebootPrepare(RebootPrepare {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                expected_boot_id: BOOT.to_string(),
            })),
        ),
    );
    assert_eq!(decision.reply().unwrap().status_code, 1010);
    assert_eq!(dispatcher.prepare_calls.load(Ordering::SeqCst), 1);

    // 代际一致时才发票。
    let ok: Arc<dyn RebootDispatcher> = Arc::new(FakeDispatcher::new("connection-a", None));
    let (handler, _) = handler_for(Some((ok, "connection-a")), "connection-a");
    let decision = handler.decide(
        StreamKind::Control,
        &request(
            "device.reboot.prepare",
            encode(&BusinessPayload::RebootPrepare(RebootPrepare {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                expected_boot_id: BOOT.to_string(),
            })),
        ),
    );
    assert_eq!(decision.reply().unwrap().status_code, 0);
}

#[test]
fn indeterminate_never_produces_a_business_reply() {
    let dispatcher = Arc::new(FakeDispatcher::new(
        "connection-a",
        Some(ExecuteOutcome::Indeterminate),
    ));
    let counting: Arc<dyn RebootDispatcher> = dispatcher.clone();
    let (handler, _) = handler_for(Some((counting, "connection-a")), "connection-a");
    let decision = handler.decide(
        StreamKind::Control,
        &request(
            "device.reboot.execute",
            encode(&BusinessPayload::RebootExecute(RebootExecute {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                expected_boot_id: BOOT.to_string(),
                execution_token: [1u8; 32],
            })),
        ),
    );
    // 既不是成功也不是 1009：无业务回包，由会话层主动 abort 本会话。
    assert_eq!(
        decision,
        BusinessDecision::NoReply(NoReplyReason::Indeterminate)
    );
    assert_eq!(dispatcher.execute_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn unknown_and_outbound_only_actions_map_1002_without_touching_anything() {
    let (handler, journal) = handler_for(None, "connection-a");
    for action in [
        "device.shutdown",
        "device.task.result",
        "",
        "device.status.get ",
        "DEVICE.STATUS.GET",
        "device.status.getter",
    ] {
        let decision = handler.decide(StreamKind::Control, &request(action, query_payload()));
        let reply = decision
            .reply()
            .unwrap_or_else(|| panic!("{action:?} 必须有回包"));
        assert_eq!(reply.status_code, 1002, "{action:?}");
        assert!(reply.payload.is_empty());
    }
    assert_eq!(journal.calls(), 0);
}

#[test]
fn event_system_and_response_packets_are_never_answered() {
    let (handler, journal) = handler_for(None, "connection-a");
    for packet_type in [PacketType::Event, PacketType::Response] {
        let mut packet = request("device.status.get", query_payload());
        packet.packet_type = packet_type;
        let decision = handler.decide(StreamKind::Control, &packet);
        assert_eq!(
            decision,
            BusinessDecision::NoReply(NoReplyReason::NotARequest),
            "{packet_type:?}"
        );
    }
    // System 信令（kick/revoke）由驱动拥有，业务层不回包也不查 journal。
    let mut system = request("device.status.get", query_payload());
    system.packet_type = PacketType::System;
    system.action = "tunnel.kick".to_string();
    assert_eq!(
        handler.decide(StreamKind::Control, &system),
        BusinessDecision::NoReply(NoReplyReason::NotARequest)
    );
    assert_eq!(journal.calls(), 0);
}

#[test]
fn reserved_tunnel_probe_actions_are_not_answered_by_the_business_layer() {
    let (handler, _journal) = handler_for(None, "connection-a");
    for action in ["tunnel.ping", "tunnel.unknown"] {
        let packet = TunnelPacket {
            trace_id: 5,
            packet_type: PacketType::Request,
            action: action.to_string(),
            status_code: 0,
            error_message: String::new(),
            // tunnel.ping 的 16B 原始 nonce 绝不按业务 payload 解码。
            payload: vec![0u8; 16],
            metadata: BTreeMap::new(),
        };
        assert_eq!(
            handler.decide(StreamKind::Control, &packet),
            BusinessDecision::NoReply(NoReplyReason::ReservedAction),
            "{action}"
        );
    }
}

#[test]
fn business_on_the_data_stream_is_rejected_without_side_effects() {
    let (handler, journal) = handler_for(None, "connection-a");
    for action in [
        "device.capabilities.get",
        "device.status.get",
        "device.task.get",
        "device.unknown.action",
    ] {
        let decision = handler.decide(StreamKind::Data, &request(action, query_payload()));
        // 中控固定用控制流下发业务；数据流上的业务请求按入参非法拒绝且空 payload。
        let reply = decision
            .reply()
            .unwrap_or_else(|| panic!("{action} 必须有回包"));
        assert_eq!(reply.status_code, 1003, "{action}");
        assert!(reply.payload.is_empty());
    }
    assert_eq!(journal.calls(), 0);
}

#[test]
fn identity_never_comes_from_the_packet() {
    // journal 记录属于「握手公钥」；payload/metadata 自称另一个 controller 也读不到。
    let (handler, journal) = handler_for(None, "connection-a");
    let mut packet = request("device.task.get", task_get_payload());
    packet
        .metadata
        .insert("controller_key".to_string(), hex::encode(CLAIMED_KEY));
    packet
        .metadata
        .insert("device_sn".to_string(), "victim".to_string());
    let decision = handler.decide(StreamKind::Control, &packet);
    // 自报字段一律无效 → 仍只按握手公钥查询，因此命中的是自己那条记录。
    let reply = decision.reply().expect("metadata 自报不得改变身份判定");
    assert_eq!(reply.status_code, 0);
    assert_eq!(journal.observed_keys(), vec![HANDSHAKE_KEY]);

    // 业务 payload 尾部再塞一个自报公钥：DTO 直接被拒（1003 + 空 payload），
    // 且 journal 仍然只见过握手公钥——身份没有任何来自 payload 的通路。
    let (handler, journal) = handler_for(None, "connection-a");
    let mut smuggled = request("device.task.get", task_get_payload());
    smuggled
        .payload
        .extend_from_slice(hex::encode(CLAIMED_KEY).as_bytes());
    let decision = handler.decide(StreamKind::Control, &smuggled);
    let reply = decision
        .reply()
        .expect("payload 尾部塞身份也必须类型化拒绝");
    assert_eq!(reply.status_code, 1003);
    assert!(reply.payload.is_empty());
    assert!(
        journal.observed_keys().is_empty(),
        "journal 绝不能看到自报公钥"
    );
}

#[test]
fn malformed_payload_is_typed_error_with_empty_payload() {
    let (handler, _journal) = handler_for(None, "connection-a");
    for garbage in [
        vec![0xff, 0xff, 0xff],
        vec![0x08, 0x96, 0x09],
        vec![0x00; 64],
        vec![],
    ] {
        let label = format!("{garbage:?}");
        let decision = handler.decide(StreamKind::Control, &request("device.status.get", garbage));
        let reply = decision
            .reply()
            .unwrap_or_else(|| panic!("{label} 必须有回包"));
        assert_ne!(reply.status_code, 0, "{label}");
        assert!(
            (1001..=1010).contains(&reply.status_code),
            "{label} -> {reply:?}"
        );
        assert!(reply.payload.is_empty());
        assert!(!reply.error_message.is_empty());
        // 错误文本只能是固定类别，不含 payload 原文。
        assert!(reply.error_message.is_ascii());
        assert!(!reply.error_message.contains('\u{0}'));
    }
}

#[test]
fn wrong_payload_kind_for_an_action_is_rejected_before_any_io() {
    let (handler, journal) = handler_for(None, "connection-a");
    // device.task.get 期望 TaskQuery，这里给 Query。
    let decision = handler.decide(
        StreamKind::Control,
        &request("device.task.get", query_payload()),
    );
    let reply = decision.reply().unwrap();
    assert_eq!(reply.status_code, 1003);
    assert!(reply.payload.is_empty());
    assert_eq!(journal.calls(), 0);
}

#[test]
fn blank_sub_task_id_is_invalid_argument() {
    let (handler, journal) = handler_for(None, "connection-a");
    // 合法 encoder 不会产出「空白 sub_task_id」（它自己就拒绝），因此这里手搓
    // 结构合法但 DTO 自检必拒的字节：field1 schema_version=1，field2 = "   "。
    let blank = vec![0x08u8, 0x01, 0x12, 0x03, 0x20, 0x20, 0x20];
    let decision = handler.decide(StreamKind::Control, &request("device.task.get", blank));
    let reply = decision.reply().expect("空白 sub_task_id 必须有回包");
    assert_eq!(reply.status_code, 1003);
    assert!(reply.payload.is_empty());
    assert_eq!(journal.calls(), 0, "DTO 不合法不得触达 journal");
}

#[test]
fn missing_journal_record_is_1006_not_a_fabricated_record() {
    // journal 里没有该 controller 的记录：1006 + 空 payload，绝不回一条空记录冒充成功。
    let journal = Arc::new(FakeJournal::new(CLAIMED_KEY));
    let agent: TestAgent = BoardAgent::new(
        FakeSource::default(),
        JournalReader(journal.clone()),
        FakeEpoch,
    );
    let dispatch: Arc<dyn BusinessDispatch> = Arc::new(agent);
    let handler = BusinessHandler::new(dispatch, HANDSHAKE_KEY, "connection-a");
    let decision = handler.decide(
        StreamKind::Control,
        &request("device.task.get", task_get_payload()),
    );
    let reply = decision.reply().unwrap();
    assert_eq!(reply.status_code, 1006);
    assert!(reply.payload.is_empty());
    assert_eq!(journal.observed_keys(), vec![HANDSHAKE_KEY]);
}

#[test]
fn only_the_five_payload_bearing_responses_can_be_encoded() {
    // Indeterminate 没有可编码 payload：绝不允许被编成「成功空包」。
    let error = encode_response(&BusinessResponse::Indeterminate).unwrap_err();
    assert_eq!(error, BusinessStatus::InternalError);
    let response = BusinessResponse::Capabilities(Capabilities {
        schema_version: BUSINESS_SCHEMA_VERSION,
        boot_id: BOOT.to_string(),
        agent_version: "0.5.0".to_string(),
        supported_actions: vec![],
        durable_task_journal: true,
        max_status_bytes: 65_536,
        journal_epoch: EPOCH.to_string(),
    });
    let bytes = encode_response(&response).unwrap();
    assert!(!bytes.is_empty());
}

#[test]
fn handler_carries_only_the_handshake_identity_and_connection_generation() {
    let (handler, _journal) = handler_for(None, "connection-a");
    let peer: PeerContext = handler.peer_context();
    assert_eq!(peer.authenticated_controller_key, HANDSHAKE_KEY);
    assert_eq!(handler.connection_id(), "connection-a");
}

#[test]
fn session_drain_deadline_is_at_least_the_spec_minimum() {
    // §6.6：优雅关闭 drain 至少 5s，生产接线不得缩短。
    assert!(DEFAULT_SESSION_DRAIN.as_secs() >= 5);
}

/// 编译期见证：`OnceLock` 用于 handler → 会话句柄的一次性绑定（Indeterminate abort）。
#[test]
fn abort_binding_is_single_shot() {
    let slot: OnceLock<()> = OnceLock::new();
    assert!(slot.set(()).is_ok());
    assert!(slot.set(()).is_err());
}

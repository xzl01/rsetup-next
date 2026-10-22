mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, RebootExecutor, RecordingOs,
    TaskJournalReader,
};
use rsetup_board_agent::journal::{JournalStore, RealFs, TaskRecord};
use rsetup_board_agent::read_only::{PeerContext, ReadOnlyMutationGuard, ReadOnlySource};
use rsetup_board_agent::tickets::TicketManager;
use rsetup_board_agent::tunnel_binding::{
    ApprovalGate, DualStreamBinding, StreamError, StreamState, StreamType,
};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, ClockSample, DeviceStatus, NoCodec,
    Query, TaskQuery, approved_codec_witness, encode_business,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";
/// 所有者授权记录的留档路径（与 `ApprovalGate::default()` 一致）。
const OWNER_AUTHORIZATION: &str =
    "docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md";

/// 只读数据源替身：`reads` 记录业务读取被触达次数，`mutations` 记录写路径触达次数。
/// 计数器放在 `Arc` 内，便于测试夹具在把替身交给 agent 后仍能观察。
#[derive(Clone, Default)]
struct Source {
    mutations: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

impl Source {
    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

impl ReadOnlySource for Source {
    fn capabilities(&self) -> Capabilities {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_version: "0.5.0".to_string(),
            supported_actions: vec!["device.reboot.execute".to_string()],
            durable_task_journal: true,
            max_status_bytes: 65_536,
            journal_epoch: EPOCH.to_string(),
        }
    }
    fn status(&self) -> DeviceStatus {
        self.reads.fetch_add(1, Ordering::SeqCst);
        DeviceStatus {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_epoch: AGENT_EPOCH.to_string(),
            sample_seq: 1,
            sampled_unix_ms: 1,
            sampled_monotonic_ns: 1,
            clock_epoch: CLOCK_EPOCH.to_string(),
            metrics: vec![],
            clock_quality: "observed".to_string(),
        }
    }
    fn clock(&self) -> ClockSample {
        self.reads.fetch_add(1, Ordering::SeqCst);
        ClockSample {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            clock_epoch: CLOCK_EPOCH.to_string(),
            receive_unix_ms: 1,
            send_unix_ms: 2,
            receive_monotonic_ns: 1,
            send_monotonic_ns: 2,
            quality: "observed".to_string(),
        }
    }
}

impl ReadOnlyMutationGuard for Source {
    fn mutation_calls(&self) -> usize {
        self.mutations.load(Ordering::SeqCst)
    }
}

struct JournalReader(Arc<JournalStore<RealFs>>);

impl TaskJournalReader for JournalReader {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<rsetup_protocol::wire::TaskRecord>, BusinessStatus> {
        self.0
            .get(controller_key, &query.sub_task_id)
            .map(|found| found.map(|record| record.to_wire()))
            .map_err(|_| BusinessStatus::InternalError)
    }
}

struct FixedEpoch(Arc<JournalStore<RealFs>>);

impl JournalEpochSource for FixedEpoch {
    fn journal_epoch(&self) -> String {
        self.0.journal_epoch().to_string()
    }
}

struct Fixture {
    _dir: TempDir,
    journal: Arc<JournalStore<RealFs>>,
    source: Source,
    binding: DualStreamBinding<Source, JournalReader, FixedEpoch, RecordingOs, RealFs>,
}

fn fixture(gate: ApprovalGate) -> Fixture {
    let dir = TempDir::new("dual");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("journal"));
    let source = Source::default();
    let agent = BoardAgent::new(
        source.clone(),
        JournalReader(Arc::clone(&journal)),
        FixedEpoch(Arc::clone(&journal)),
    );
    let os = RecordingOs::new();
    let executor = RebootExecutor::new(
        Arc::clone(&journal),
        os,
        TicketManager::new(),
        Box::new(|| BOOT.to_string()),
        "agent-1",
    );
    let (binding, _outbound) = DualStreamBinding::new(agent, executor, gate);
    Fixture {
        _dir: dir,
        journal,
        source,
        binding,
    }
}

fn peer(key: u8) -> PeerContext {
    PeerContext {
        authenticated_controller_key: [key; 32],
    }
}

/// 用**所有者授权启用的** [`BusinessCodec`] 产出合法的 `Query` 业务字节。
fn encoded_query() -> Vec<u8> {
    encode_business(
        &BusinessCodec,
        Some(&approved_codec_witness()),
        &BusinessPayload::Query(Query {
            schema_version: BUSINESS_SCHEMA_VERSION,
        }),
    )
    .expect("owner-approved codec must encode a valid Query")
}

/// 断言一次调用完全零触达：OS 重启入口、journal 记录、只读数据源读写。
fn assert_zero_touch(f: &Fixture, os_calls: usize, journal_records: usize, source_reads: usize) {
    assert_eq!(f.binding.os().call_count(), os_calls, "OS 调用次数不得变化");
    assert_eq!(
        f.journal.record_count(),
        journal_records,
        "journal 记录数不得变化"
    );
    assert_eq!(f.source.reads(), source_reads, "只读数据源不得被额外触达");
    assert_eq!(f.source.mutation_calls(), 0, "只读数据源写路径不得被触达");
}

#[test]
fn data_stream_is_blocked_until_control_stream_is_open() {
    let mut f = fixture(ApprovalGate::default());
    assert_eq!(
        f.binding.stream_state(StreamType::Control),
        StreamState::NotOpened
    );
    assert_eq!(
        f.binding.stream_state(StreamType::Data),
        StreamState::NotOpened
    );
    assert_eq!(
        f.binding.open_data(),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(
        f.binding.stream_state(StreamType::Data),
        StreamState::NotOpened
    );

    f.binding.open_control().expect("open control");
    assert_eq!(
        f.binding.stream_state(StreamType::Control),
        StreamState::Open
    );
    f.binding.open_data().expect("open data after control");
    assert_eq!(f.binding.stream_state(StreamType::Data), StreamState::Open);
}

#[test]
fn business_requests_are_rejected_when_control_is_not_open_or_closed() {
    let mut f = fixture(ApprovalGate::default());
    let peer = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };
    let query = BusinessPayload::Query(Query {
        schema_version: BUSINESS_SCHEMA_VERSION,
    });

    // 1) NotOpened: typed 与 wire 入口均被拒，不触碰 source/journal
    assert_eq!(
        f.binding.handle_typed(
            StreamType::Control,
            &peer,
            "device.capabilities.get",
            &query
        ),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(
        f.binding
            .handle_wire(StreamType::Control, &peer, "device.capabilities.get", &[]),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(f.binding.os().call_count(), 0);

    // 2) Open 后正常访问
    f.binding.open_control().expect("open control");
    assert!(
        f.binding
            .handle_typed(
                StreamType::Control,
                &peer,
                "device.capabilities.get",
                &query
            )
            .is_ok()
    );

    // 3) Closed 后再访问同样被拒
    f.binding.close_control();
    assert_eq!(
        f.binding.stream_state(StreamType::Control),
        StreamState::Closed
    );
    assert_eq!(
        f.binding.handle_typed(
            StreamType::Control,
            &peer,
            "device.capabilities.get",
            &query
        ),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(
        f.binding
            .handle_wire(StreamType::Control, &peer, "device.capabilities.get", &[]),
        Err(StreamError::ControlStreamRequired)
    );
    assert_eq!(f.binding.os().call_count(), 0);
}

#[test]
fn typed_readonly_round_trip_uses_real_journal_and_zero_os_calls() {
    let mut f = fixture(ApprovalGate::default());
    f.binding.open_control().expect("control");
    f.binding.open_data().expect("data");
    let peer = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };

    let caps = f
        .binding
        .handle_typed(
            StreamType::Control,
            &peer,
            "device.capabilities.get",
            &BusinessPayload::Query(Query {
                schema_version: BUSINESS_SCHEMA_VERSION,
            }),
        )
        .expect("capabilities");
    let StreamState::Open = f.binding.stream_state(StreamType::Control) else {
        panic!()
    };
    match caps {
        BusinessResponse::Capabilities(caps) => assert_eq!(caps.journal_epoch, EPOCH),
        other => panic!("expected capabilities, got {other:?}"),
    }

    // 真实 journal：先落一条 accepted，再经查询路径读回
    let record: TaskRecord = f
        .journal
        .insert_accepted(&[1u8; 32], SUB_TASK, &[0u8; 32], BOOT)
        .expect("accepted")
        .0;
    let queried = f
        .binding
        .handle_typed(
            StreamType::Control,
            &peer,
            "device.task.get",
            &BusinessPayload::TaskQuery(TaskQuery {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
            }),
        )
        .expect("task get");
    match queried {
        BusinessResponse::TaskRecord(returned) => {
            assert_eq!(returned, record.to_wire());
            assert_eq!(returned.journal_epoch, f.journal.journal_epoch());
        }
        other => panic!("expected task record, got {other:?}"),
    }

    // 另一个认证身份不得看到该记录
    let stranger = PeerContext {
        authenticated_controller_key: [2u8; 32],
    };
    assert_eq!(
        f.binding.handle_typed(
            StreamType::Control,
            &stranger,
            "device.task.get",
            &BusinessPayload::TaskQuery(TaskQuery {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
            }),
        ),
        Err(StreamError::AgentRejected(BusinessStatus::TaskNotFound))
    );
    assert_eq!(
        f.binding.gate().require_network_io(),
        Err(StreamError::NetworkApprovalRequired)
    );
    // 默认门：codec 已按所有者 2026-10-22 授权启用，network 仍未批准
    assert_eq!(f.binding.gate().require_codec(), Ok(()));
    assert!(f.binding.gate().codec_approved);
    assert!(!f.binding.gate().network_approved);
    assert_eq!(
        f.binding.gate().evidence_path,
        "docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md"
    );
    assert_eq!(
        f.binding.os().call_count(),
        0,
        "readonly round-trip must never touch the OS"
    );
}

#[test]
fn not_allowlisted_action_is_rejected_before_any_stream_work() {
    let mut f = fixture(ApprovalGate::default());
    f.binding.open_control().expect("control");
    let peer = peer(1);
    assert_eq!(
        f.binding.handle_typed(
            StreamType::Control,
            &peer,
            "device.fan.apply",
            &BusinessPayload::Query(Query {
                schema_version: BUSINESS_SCHEMA_VERSION,
            }),
        ),
        Err(StreamError::AgentRejected(
            BusinessStatus::UnsupportedAction
        ))
    );
    // 白名单外动作不消耗 codec：agent 内先查白名单，codec 虽已授权启用也不被触达
    assert_eq!(
        f.binding
            .handle_wire(StreamType::Control, &peer, "device.fan.apply", &[]),
        Err(StreamError::AgentRejected(
            BusinessStatus::UnsupportedAction
        ))
    );
    // 同样对**合法编码字节**成立：动作不在白名单时解码路径根本不会执行
    assert_eq!(
        f.binding.handle_wire(
            StreamType::Control,
            &peer,
            "device.fan.apply",
            &encoded_query()
        ),
        Err(StreamError::AgentRejected(
            BusinessStatus::UnsupportedAction
        ))
    );
    assert_zero_touch(&f, 0, 0, 0);
}

#[test]
fn wire_round_trip_with_owner_approved_codec_matches_typed() {
    let mut f = fixture(ApprovalGate::default());
    let peer = peer(1);
    let bytes = encoded_query();

    // Open 之前：codec 门已开，但流守卫仍然先拒绝，绝不因 codec 启用而放行
    assert_eq!(
        f.binding.handle_wire(
            StreamType::Control,
            &peer,
            "device.capabilities.get",
            &bytes
        ),
        Err(StreamError::ControlStreamRequired)
    );
    assert_zero_touch(&f, 0, 0, 0);

    f.binding.open_control().expect("control");
    let via_wire = f
        .binding
        .handle_wire(
            StreamType::Control,
            &peer,
            "device.capabilities.get",
            &bytes,
        )
        .expect("owner-approved codec wire round trip");
    match &via_wire {
        BusinessResponse::Capabilities(caps) => assert_eq!(caps.journal_epoch, EPOCH),
        other => panic!("expected capabilities, got {other:?}"),
    }

    // 与类型化入口逐字节等价：wire 入口没有第二条业务语义
    let via_typed = f
        .binding
        .handle_typed(
            StreamType::Control,
            &peer,
            "device.capabilities.get",
            &BusinessPayload::Query(Query {
                schema_version: BUSINESS_SCHEMA_VERSION,
            }),
        )
        .expect("typed capabilities");
    assert_eq!(via_wire, via_typed);

    // 业务 action 落在 Data 流仍被拒（codec 启用不改变流归属）
    f.binding.open_data().expect("data");
    assert_eq!(
        f.binding
            .handle_wire(StreamType::Data, &peer, "device.capabilities.get", &bytes),
        Err(StreamError::ActionNotAllowedOnStream)
    );
    // 只读路径不触达 OS/journal 写/数据源写
    assert_eq!(f.binding.os().call_count(), 0);
    assert_eq!(f.journal.record_count(), 0);
    assert_eq!(f.source.mutation_calls(), 0);
    assert_eq!(f.source.reads(), 2, "capabilities 只应被读两次");
}

#[test]
fn malformed_bytes_are_invalid_argument_once_the_codec_is_enabled() {
    let mut f = fixture(ApprovalGate::default());
    f.binding.open_control().expect("control");
    let peer = peer(1);
    let valid = encoded_query();

    // canonical 编码非空；截去尾字节、字段号 0、错误 wire type 等都不是合法输入
    assert!(!valid.is_empty());
    let mut truncated = valid.clone();
    truncated.pop();
    for (label, bytes) in [
        ("zero field number", vec![0x00u8]),
        ("truncated varint", vec![0x08u8]),
        ("non-minimal varint", vec![0x08u8, 0x81, 0x00]),
        ("wrong wire type", vec![0x09u8, 0x00, 0x00]),
        ("length beyond remainder", vec![0x0au8, 0x05, 0x01]),
        ("truncated canonical encoding", truncated),
    ] {
        assert_eq!(
            f.binding.handle_wire(
                StreamType::Control,
                &peer,
                "device.capabilities.get",
                &bytes,
            ),
            Err(StreamError::AgentRejected(BusinessStatus::InvalidArgument)),
            "case={label}"
        );
    }
    assert_zero_touch(&f, 0, 0, 0);
}

#[test]
/// codec 门关闭态（显式构造，默认值已按所有者授权启用 codec）：
/// 合法编码字节与 raw 字节都必须在门处被拒，绝不进入解码、流守卫与任何下层。
fn raw_wire_entries_fail_closed_without_an_approved_codec() {
    let mut f = fixture(ApprovalGate::new(false, false, OWNER_AUTHORIZATION));
    f.binding.open_control().expect("control");
    let peer = peer(1);
    let valid = encoded_query();

    // 合法编码 + 门关闭：门先于一切返回 CodecApprovalRequired
    for action in [
        "device.capabilities.get",
        "device.status.get",
        "device.clock.get",
        "device.task.get",
    ] {
        assert_eq!(
            f.binding
                .handle_wire(StreamType::Control, &peer, action, &valid),
            Err(StreamError::CodecApprovalRequired),
            "action={action} must be refused while the codec gate is closed"
        );
        assert_eq!(
            f.binding
                .handle_wire(StreamType::Control, &peer, action, b"raw-bytes"),
            Err(StreamError::CodecApprovalRequired),
            "action={action} must be refused before decoding raw bytes"
        );
    }
    // codec 门先于 agent 内的动作白名单：白名单外动作同样返回 CodecApprovalRequired
    assert_eq!(
        f.binding
            .handle_wire(StreamType::Control, &peer, "device.fan.apply", &valid),
        Err(StreamError::CodecApprovalRequired)
    );
    assert_eq!(
        f.binding.gate().require_codec(),
        Err(StreamError::CodecApprovalRequired)
    );
    assert_eq!(
        f.binding.gate().require_network_io(),
        Err(StreamError::NetworkApprovalRequired)
    );
    // 类型化入口不经 codec：门关闭不影响只读查询（证明拒绝原因确属 codec 门）
    assert!(
        f.binding
            .handle_typed(
                StreamType::Control,
                &peer,
                "device.capabilities.get",
                &BusinessPayload::Query(Query {
                    schema_version: BUSINESS_SCHEMA_VERSION,
                }),
            )
            .is_ok()
    );
    // 门关闭期间 codec 路径零触达：唯一一次 source 读取来自上面的类型化入口
    assert_zero_touch(&f, 0, 0, 1);

    // 未获批 codec：解密/编码路径根本不可达，绝不产生「假 codec 通过」
    assert_eq!(
        rsetup_protocol::wire::decode_business(
            &NoCodec,
            None,
            rsetup_protocol::wire::BusinessPayloadKind::TaskRecord,
            b"raw-bytes",
        ),
        Err(rsetup_protocol::wire::CodecError::CodecUnapproved(
            "no approved business codec"
        ))
    );
}

#[test]
fn system_frames_are_confined_to_the_control_stream() {
    let mut f = fixture(ApprovalGate::default());
    assert_eq!(
        f.binding.handle_system(StreamType::Data, "tunnel.kick"),
        Err(StreamError::ControlStreamRequired),
        "TYPE_SYSTEM frames must never travel on the data stream"
    );
    f.binding.open_control().expect("control");
    assert_eq!(
        f.binding.handle_system(StreamType::Control, "tunnel.kick"),
        Ok(())
    );
    assert_eq!(
        f.binding.handle_system(StreamType::Control, "tunnel.ping"),
        Err(StreamError::ActionNotAllowedOnStream),
        "tunnel.ping is a probe action, not a system action"
    );
}

#[test]
fn real_network_binding_is_refused_while_the_approval_gate_is_closed() {
    let f = fixture(ApprovalGate::default());
    assert_eq!(
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(f.binding.bind_real_network("127.0.0.1:0")),
        Err(StreamError::NetworkApprovalRequired)
    );
    assert_eq!(
        f.binding.gate().require_network_io(),
        Err(StreamError::NetworkApprovalRequired)
    );
}

#[test]
fn task_result_event_is_emitted_only_after_control_is_open() {
    let mut f = fixture(ApprovalGate::default());
    let record = f
        .journal
        .insert_accepted(&[1u8; 32], SUB_TASK, &[0u8; 32], BOOT)
        .expect("accepted")
        .0;
    assert_eq!(
        f.binding.emit_task_result(&record),
        Err(StreamError::ControlStreamRequired)
    );
    f.binding.open_control().expect("control");
    let payload = f.binding.emit_task_result(&record).expect("event");
    assert!(matches!(payload, BusinessPayload::TaskRecord(_)));
    // 板端事件不得回包
    assert!(matches!(
        f.binding
            .handle_system(StreamType::Control, "device.task.result"),
        Err(StreamError::ActionNotAllowedOnStream)
    ));
}

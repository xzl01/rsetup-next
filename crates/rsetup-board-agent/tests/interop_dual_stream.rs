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
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, ClockSample, DeviceStatus, NoCodec,
    Query, TaskQuery,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";

#[derive(Default)]
struct Source {
    mutations: AtomicUsize,
}

impl ReadOnlySource for Source {
    fn capabilities(&self) -> Capabilities {
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
    binding: DualStreamBinding<Source, JournalReader, FixedEpoch, RecordingOs, RealFs>,
}

fn fixture(gate: ApprovalGate) -> Fixture {
    let dir = TempDir::new("dual");
    let journal = Arc::new(JournalStore::open(dir.path(), RealFs).expect("journal"));
    let agent = BoardAgent::new(
        Source::default(),
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
        binding,
    }
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
    assert_eq!(
        f.binding.gate().require_codec(),
        Err(StreamError::CodecApprovalRequired)
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
    let peer = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };
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
    // 白名单外动作不消耗 codec：即使没有获批 codec，也报告动作不被允许
    assert_eq!(
        f.binding
            .handle_wire(StreamType::Control, &peer, "device.fan.apply", &[]),
        Err(StreamError::AgentRejected(
            BusinessStatus::UnsupportedAction
        ))
    );
}

#[test]
fn raw_wire_entries_fail_closed_without_an_approved_codec() {
    let mut f = fixture(ApprovalGate::default());
    f.binding.open_control().expect("control");
    let peer = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };

    for action in ["device.status.get", "device.clock.get", "device.task.get"] {
        assert_eq!(
            f.binding
                .handle_wire(StreamType::Control, &peer, action, b"raw-bytes"),
            Err(StreamError::AgentRejected(
                BusinessStatus::UnsupportedVersion
            )),
            "action={action} must fail closed without an approved codec"
        );
    }
    assert_eq!(
        f.binding.gate().require_codec(),
        Err(StreamError::CodecApprovalRequired)
    );
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

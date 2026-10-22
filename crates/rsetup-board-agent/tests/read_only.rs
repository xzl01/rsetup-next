use rsetup_board_agent::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, PeerContext,
    ReadOnlyMutationGuard, ReadOnlySource, TaskJournalReader, allowed_action_names,
};
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Capabilities, ClockSample, DeviceStatus, NoCodec,
    TaskQuery, TaskRecord,
};
use std::sync::atomic::{AtomicUsize, Ordering};

const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";

#[derive(Default)]
struct FakeSource {
    mutations: AtomicUsize,
}

impl ReadOnlySource for FakeSource {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            schema_version: BUSINESS_SCHEMA_VERSION,
            boot_id: BOOT.to_string(),
            agent_version: "0.5.0".to_string(),
            supported_actions: vec!["device.reboot.prepare".to_string()],
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
            sampled_unix_ms: 1_700_000_000_000,
            sampled_monotonic_ns: 42,
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
            receive_monotonic_ns: 10,
            send_monotonic_ns: 20,
            quality: "observed".to_string(),
        }
    }
}

impl ReadOnlyMutationGuard for FakeSource {
    fn mutation_calls(&self) -> usize {
        self.mutations.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct FakeJournal {
    queries: AtomicUsize,
}

impl TaskJournalReader for FakeJournal {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<TaskRecord>, BusinessStatus> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        if query.schema_version != BUSINESS_SCHEMA_VERSION {
            return Err(BusinessStatus::UnsupportedVersion);
        }
        if query.sub_task_id.trim().is_empty() {
            return Err(BusinessStatus::InvalidArgument);
        }
        if controller_key == &[1u8; 32] && query.sub_task_id == SUB_TASK {
            Ok(Some(TaskRecord {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                journal_epoch: EPOCH.to_string(),
                record_version: 1,
                state: "accepted".to_string(),
                pre_boot_id: BOOT.to_string(),
                observed_boot_id: None,
                reason_code: None,
                updated_unix_ms: None,
                evidence_kind: None,
            }))
        } else {
            Ok(None)
        }
    }
}

struct FixedEpoch;
impl JournalEpochSource for FixedEpoch {
    fn journal_epoch(&self) -> String {
        EPOCH.to_string()
    }
}

fn agent() -> BoardAgent<FakeSource, FakeJournal, FixedEpoch> {
    BoardAgent::new(FakeSource::default(), FakeJournal::default(), FixedEpoch)
}

#[test]
fn allowlist_is_closed_and_reserved_prefix_is_never_allowlisted() {
    let names = allowed_action_names();
    assert_eq!(names.len(), 6);
    assert!(names.contains(&"device.task.get"));
    assert!(!names.contains(&"device.task.result"));
    assert!(!names.contains(&"device.fan.apply"));
    for name in names {
        assert!(
            !name.starts_with("tunnel."),
            "{name} must not use the reserved prefix"
        );
    }
}

#[test]
fn unknown_and_mutation_actions_return_unsupported_action_with_zero_mutations() {
    let agent = agent();
    let peer = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };
    for action in [
        "device.fan.apply",
        "device.power.off",
        "system.shell",
        "tunnel.ping",
        "tunnel.kick",
        "device.task.result",
        "",
    ] {
        let result = agent.handle_wire(&NoCodec, &peer, action, &[]);
        assert_eq!(
            result,
            Err(BusinessStatus::UnsupportedAction),
            "action={action}"
        );
    }
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn known_actions_with_unapproved_raw_bytes_fail_closed() {
    let agent = agent();
    let peer = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };
    // 无获批 codec：即使 action 合法，也不得把原始字节当作业务消息接受；
    // 这是本端部署/门禁故障，映射 1009 INTERNAL_ERROR，不冒充对端版本不匹配。
    assert_eq!(
        agent.handle_wire(&NoCodec, &peer, "device.task.get", b"not_valid_query"),
        Err(BusinessStatus::InternalError)
    );
    assert_eq!(
        agent.handle_wire(&NoCodec, &peer, "device.status.get", &[]),
        Err(BusinessStatus::InternalError)
    );
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn typed_task_get_validates_schema_and_isolates_controller_namespace() {
    let agent = agent();
    let owner = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };
    let stranger = PeerContext {
        authenticated_controller_key: [2u8; 32],
    };

    let good = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
    });
    match agent.handle_typed_action(&owner, "device.task.get", &good) {
        Ok(BusinessResponse::TaskRecord(record)) => {
            assert_eq!(record.state, "accepted");
            assert_eq!(record.journal_epoch, EPOCH);
        }
        other => panic!("expected TaskRecord, got {other:?}"),
    }

    // 另一把已认证公钥不得看到同一 sub_task_id 的记录
    assert_eq!(
        agent.handle_typed_action(&stranger, "device.task.get", &good),
        Err(BusinessStatus::TaskNotFound)
    );

    // schema_version 未知必须拒绝
    let bad_version = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: 2,
        sub_task_id: SUB_TASK.to_string(),
    });
    assert_eq!(
        agent.handle_typed_action(&owner, "device.task.get", &bad_version),
        Err(BusinessStatus::UnsupportedVersion)
    );

    // 空 sub_task_id 必须拒绝
    let empty = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: "   ".to_string(),
    });
    assert_eq!(
        agent.handle_typed_action(&owner, "device.task.get", &empty),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn typed_read_only_actions_return_validated_typed_dtos() {
    let agent = agent();
    let peer = PeerContext {
        authenticated_controller_key: [1u8; 32],
    };
    let query = BusinessPayload::Query(rsetup_protocol::wire::Query { schema_version: 1 });

    match agent.handle_typed_action(&peer, "device.capabilities.get", &query) {
        Ok(BusinessResponse::Capabilities(caps)) => {
            assert!(caps.durable_task_journal);
            assert_eq!(caps.journal_epoch, EPOCH);
        }
        other => panic!("expected Capabilities, got {other:?}"),
    }
    assert!(matches!(
        agent.handle_typed_action(
            &peer,
            "device.status.get",
            &BusinessPayload::DeviceStatus(agent.source().status())
        ),
        Ok(BusinessResponse::DeviceStatus(_))
    ));
    assert!(matches!(
        agent.handle_typed_action(
            &peer,
            "device.clock.get",
            &BusinessPayload::ClockSample(agent.source().clock())
        ),
        Ok(BusinessResponse::ClockSample(_))
    ));
    // 规范实现统一接受 Query 载荷（不校验 DTO 与 action 的对应关系），
    // 因此 clock.get + Query 与 status.get + Query 一样应被接受并返回正确类型
    assert!(matches!(
        agent.handle_typed_action(&peer, "device.clock.get", &query),
        Ok(BusinessResponse::ClockSample(_))
    ));
    // 板端事件类型不得由远程请求触发（未在入站白名单中，返回 UnsupportedAction）
    assert_eq!(
        agent.handle_typed_action(
            &peer,
            "device.task.result",
            &BusinessPayload::TaskRecord(TaskRecord {
                schema_version: BUSINESS_SCHEMA_VERSION,
                sub_task_id: SUB_TASK.to_string(),
                journal_epoch: EPOCH.to_string(),
                record_version: 1,
                state: "accepted".to_string(),
                pre_boot_id: BOOT.to_string(),
                observed_boot_id: None,
                reason_code: None,
                updated_unix_ms: None,
                evidence_kind: None,
            })
        ),
        Err(BusinessStatus::UnsupportedAction)
    );
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[test]
fn action_lookup_covers_every_kind_and_rejects_reserved_prefix() {
    use rsetup_board_agent::{ActionKind, is_reserved, lookup_action};
    assert_eq!(
        lookup_action("device.capabilities.get").unwrap().kind,
        ActionKind::ReadOnly
    );
    assert_eq!(
        lookup_action("device.task.get").unwrap().kind,
        ActionKind::JournalRead
    );
    assert_eq!(
        lookup_action("device.reboot.prepare").unwrap().kind,
        ActionKind::RebootPrepare
    );
    assert_eq!(
        lookup_action("device.reboot.execute").unwrap().kind,
        ActionKind::RebootExecute
    );
    assert!(lookup_action("device.task.result").is_none());
    assert!(lookup_action("device.fan.apply").is_none());
    assert!(is_reserved("tunnel.ping"));
    assert!(!is_reserved("device.task.get"));
}

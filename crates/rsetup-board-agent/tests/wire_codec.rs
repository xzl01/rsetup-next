//! WIRE-CODEC 互操作切片：真实 [`BusinessCodec`] 编码 → `BoardAgent::handle_wire` 真实解码 →
//! 类型化分发，端到端在**同一进程内**闭环。
//!
//! 授权性质（与 `rsetup_protocol::wire::approved_codec_witness` 保持一致）：业务 codec 由
//! **项目所有者于 2026-10-22 授权启用**，不是外部专家决议，也不构成对 03 规格「待审阅」状态的批准。
//! 本文件证明的是本仓 encode/decode 与板端分发的互操作性，**不**构成跨库固定测试向量、
//! 两进程互操作或跨库一致性证据。
//!
//! 隔离纪律：全程进程内、不联网、不重启；journal 用真实 `JournalStore<RealFs>` + 独立临时目录，
//! reboot 路径不注入 dispatcher，因此永不存在 OS 重启入口。
//!
//! ## 本文件锁定的 codec 错误 → `status_code` 映射
//!
//! 映射唯一实现在 `dispatch::codec_error_status`（穷尽匹配，无 `_` 兜底）；本文件是其
//! 端到端见证，每条都同时断言「拒绝发生在任何 source/journal 触达之前」：
//!
//! | `CodecError` 变体 | `status_code` | 语义 | 见证用例 |
//! | --- | --- | --- | --- |
//! | `UnsupportedSchemaVersion` | 1001 `UnsupportedVersion` | 对端版本不受支持，可协商 | `wire_unsupported_schema_version_...` |
//! | `Malformed` / `UnknownField` / `DuplicateField` / `MissingField` / `InvalidUtf8` / `InvalidDto` | 1003 `InvalidArgument` | 这条请求自身的编码缺陷 | `wire_truncated_bytes_...` 等 |
//! | `TooManyMetrics` | 1003 `InvalidArgument` | `metrics` 条目数超 schema 上限（集合超限，非整包长度） | 仅映射层锁定：当前入站白名单没有 `expected = DeviceStatus` 的动作，端到端不可触达 |
//! | `LengthLimit`（> 512 KiB 整包） | 1008 `ResourceExhausted` | 资源超限 | `wire_oversized_payload_...` |
//! | `CodecUnapproved`（`NoCodec` / 无见证） | 1009 `InternalError` | **本端部署/门禁故障**，非对端版本问题 | `wire_without_approved_codec_...` |
//!
//! 反向不变量同样被锁定：恰好达到上限不报 1008；未获批 codec 不得再冒充 1001；
//! 白名单未命中（1002）与未注入 reboot 调度器（1004）都先于 codec 判定。

mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::journal::{JournalStore, RealFs};
use rsetup_board_agent::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, PeerContext,
    ReadOnlyMutationGuard, ReadOnlySource, TaskJournalReader,
};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::codec::MAX_BUSINESS_MESSAGE_BYTES;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessMessage, BusinessPayload, Capabilities, ClockSample,
    DeviceStatus, NoCodec, Query, RebootExecute, RebootPrepare, TaskQuery, approved_codec_witness,
    encode_business,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";
const SUB_TASK_MISSING: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const JOURNAL_EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const AGENT_EPOCH: &str = "1b4e28ba-2fa1-4d4b-9c1a-2b6b9d0e0c11";
const CLOCK_EPOCH: &str = "2c1f9d3a-5b6e-4f70-8a91-0d2e3f4a5b6c";
const KEY: [u8; 32] = [1u8; 32];
/// 本部署不支持的业务 schema 版本：用于验证「版本不受支持」是可协商的 1001，
/// 而不是 1003 参数错误。`Query` 的 canonical 编码为 `[0x08, 0x02]`。
const UNSUPPORTED_VERSION: u32 = BUSINESS_SCHEMA_VERSION + 1;

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

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
            supported_actions: vec![
                "device.capabilities.get".to_string(),
                "device.status.get".to_string(),
                "device.clock.get".to_string(),
                "device.task.get".to_string(),
            ],
            durable_task_journal: true,
            max_status_bytes: 65_536,
            journal_epoch: JOURNAL_EPOCH.to_string(),
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

/// 真实 journal 的只读适配器：额外记录查询次数，作为「解码确实成功并触达 journal」的见证。
#[derive(Clone)]
struct JournalReader {
    store: Arc<JournalStore<RealFs>>,
    queries: Arc<AtomicUsize>,
}

impl TaskJournalReader for JournalReader {
    fn query(
        &self,
        controller_key: &[u8; 32],
        query: &TaskQuery,
    ) -> Result<Option<rsetup_protocol::wire::TaskRecord>, BusinessStatus> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        self.store
            .get(controller_key, &query.sub_task_id)
            .map_err(|_| BusinessStatus::InternalError)
            .map(|record| record.map(|record| record.to_wire()))
    }
}

struct FixedEpoch;

impl JournalEpochSource for FixedEpoch {
    fn journal_epoch(&self) -> String {
        JOURNAL_EPOCH.to_string()
    }
}

type Agent = BoardAgent<FakeSource, JournalReader, FixedEpoch>;

struct Fixture {
    _dir: TempDir,
    store: Arc<JournalStore<RealFs>>,
    reader: JournalReader,
    agent: Agent,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let store = Arc::new(JournalStore::open(dir.path(), RealFs).expect("open journal"));
        let reader = JournalReader {
            store: Arc::clone(&store),
            queries: Arc::new(AtomicUsize::new(0)),
        };
        // 刻意不调用 `with_reboot`：两个 reboot 动作必须在解码成功后落回
        // CapabilityUnavailable，本文件因此完全不存在 OS 重启入口。
        let agent = BoardAgent::new(FakeSource::default(), reader.clone(), FixedEpoch);
        Fixture {
            _dir: dir,
            store,
            reader,
            agent,
        }
    }
    fn queries(&self) -> usize {
        self.reader.queries.load(Ordering::SeqCst)
    }
    fn mutations(&self) -> usize {
        self.agent.source().mutation_calls()
    }
}

fn peer(key: &[u8; 32]) -> PeerContext {
    PeerContext {
        authenticated_controller_key: *key,
    }
}

/// 用真实 codec + 所有者授权见证编码；返回 canonical 字节。
fn encode(payload: &BusinessPayload) -> Vec<u8> {
    encode_business(&BusinessCodec, Some(&approved_codec_witness()), payload)
        .expect("approved codec must encode a valid DTO")
}

fn query_bytes() -> Vec<u8> {
    encode(&BusinessPayload::Query(Query {
        schema_version: BUSINESS_SCHEMA_VERSION,
    }))
}

fn task_query_bytes(sub_task_id: &str) -> Vec<u8> {
    encode(&BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: sub_task_id.to_string(),
    }))
}

// ---------------------------------------------------------------------------
// 1) 三条只读动作：真实 encode → handle_wire → 类型化响应
// ---------------------------------------------------------------------------

#[test]
fn wire_capabilities_get_decodes_query_and_returns_typed_capabilities() {
    let fx = Fixture::new("codec-caps");
    let payload = BusinessPayload::Query(Query {
        schema_version: BUSINESS_SCHEMA_VERSION,
    });

    let wired = fx.agent.handle_wire(
        &BusinessCodec,
        &peer(&KEY),
        "device.capabilities.get",
        &encode(&payload),
    );
    match &wired {
        Ok(BusinessResponse::Capabilities(caps)) => {
            caps.validate().expect("出站 DTO 必须自校验通过");
            assert_eq!(caps.boot_id, BOOT);
            assert_eq!(caps.journal_epoch, JOURNAL_EPOCH);
        }
        other => panic!("expected Capabilities, got {other:?}"),
    }
    // 真实 wire 路径与类型化入口必须给出完全一致的结果（codec 不改变分发语义）
    assert_eq!(
        wired,
        fx.agent
            .handle_typed_action(&peer(&KEY), "device.capabilities.get", &payload)
    );
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.queries(), 0);
}

#[test]
fn wire_status_get_decodes_query_and_returns_typed_device_status() {
    let fx = Fixture::new("codec-status");
    let wired = fx.agent.handle_wire(
        &BusinessCodec,
        &peer(&KEY),
        "device.status.get",
        &query_bytes(),
    );
    match &wired {
        Ok(BusinessResponse::DeviceStatus(status)) => {
            status.validate().expect("出站 DTO 必须自校验通过");
            assert_eq!(status.agent_epoch, AGENT_EPOCH);
            assert_eq!(status.sample_seq, 1);
        }
        other => panic!("expected DeviceStatus, got {other:?}"),
    }
    assert_eq!(fx.mutations(), 0);
}

#[test]
fn wire_clock_get_decodes_query_and_returns_typed_clock_sample() {
    let fx = Fixture::new("codec-clock");
    let wired = fx.agent.handle_wire(
        &BusinessCodec,
        &peer(&KEY),
        "device.clock.get",
        &query_bytes(),
    );
    match &wired {
        Ok(BusinessResponse::ClockSample(clock)) => {
            clock.validate().expect("出站 DTO 必须自校验通过");
            assert_eq!(clock.clock_epoch, CLOCK_EPOCH);
            assert_eq!(clock.quality, "observed");
        }
        other => panic!("expected ClockSample, got {other:?}"),
    }
    assert_eq!(fx.mutations(), 0);
}

// ---------------------------------------------------------------------------
// 2) journal 读：解码成功后才触达 journal
// ---------------------------------------------------------------------------

#[test]
fn wire_task_query_reaches_journal_and_reports_not_found() {
    let fx = Fixture::new("codec-task-miss");
    // 无持久记录 → TaskNotFound：解码成功（否则会是 1003/1001），且查询真的落到 journal
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.task.get",
            &task_query_bytes(SUB_TASK_MISSING),
        ),
        Err(BusinessStatus::TaskNotFound)
    );
    assert_eq!(fx.queries(), 1, "解码成功才允许触达 journal");
    assert_eq!(fx.store.record_count(), 0);
}

#[test]
fn wire_task_query_returns_persisted_record_over_real_wire_bytes() {
    let fx = Fixture::new("codec-task-hit");
    fx.store
        .insert_accepted(&KEY, SUB_TASK, &[7u8; 32], BOOT)
        .expect("seed accepted record");

    match fx.agent.handle_wire(
        &BusinessCodec,
        &peer(&KEY),
        "device.task.get",
        &task_query_bytes(SUB_TASK),
    ) {
        Ok(BusinessResponse::TaskRecord(record)) => {
            record.validate().expect("出站记录必须自校验通过");
            assert_eq!(record.sub_task_id, SUB_TASK);
            assert_eq!(record.state, "accepted");
            assert_eq!(record.pre_boot_id, BOOT);
        }
        other => panic!("expected TaskRecord, got {other:?}"),
    }
    assert_eq!(fx.queries(), 1);
}

// ---------------------------------------------------------------------------
// 3) reboot：解码成功但未注入 dispatcher → 1004，且零持久写
// ---------------------------------------------------------------------------

#[test]
fn wire_reboot_prepare_without_dispatcher_is_capability_unavailable() {
    let fx = Fixture::new("codec-prepare-1004");
    let bytes = encode(&BusinessPayload::RebootPrepare(RebootPrepare {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT.to_string(),
    }));
    // 解码成功（否则会先报 1003/1001），但未注入调度器 → 1004，且绝不签发票据、绝不写盘
    assert_eq!(
        fx.agent
            .handle_wire(&BusinessCodec, &peer(&KEY), "device.reboot.prepare", &bytes,),
        Err(BusinessStatus::CapabilityUnavailable)
    );
    assert_eq!(fx.store.record_count(), 0);
    assert_eq!(fx.mutations(), 0);
}

#[test]
fn wire_reboot_execute_without_dispatcher_is_capability_unavailable() {
    let fx = Fixture::new("codec-execute-1004");
    let bytes = encode(&BusinessPayload::RebootExecute(RebootExecute {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        expected_boot_id: BOOT.to_string(),
        execution_token: [0x11u8; 32],
    }));
    // 载荷已完整解码（含 32B execution_token）；未注入调度器 → 1004，OS 边界根本不存在
    assert_eq!(
        fx.agent
            .handle_wire(&BusinessCodec, &peer(&KEY), "device.reboot.execute", &bytes,),
        Err(BusinessStatus::CapabilityUnavailable)
    );
    assert_eq!(fx.store.record_count(), 0);
    assert_eq!(fx.queries(), 0);
}

// ---------------------------------------------------------------------------
// 4) 畸形字节与非法 DTO → 1003（这条请求自身的编码缺陷）
// ---------------------------------------------------------------------------

#[test]
fn wire_truncated_bytes_map_to_invalid_argument() {
    let fx = Fixture::new("codec-truncated");
    let canonical = query_bytes();
    assert_eq!(
        canonical.as_slice(),
        &[0x08, 0x01],
        "Query 的 canonical 编码"
    );
    // 截断的 varint 载荷（只剩 tag）
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.capabilities.get",
            &canonical[..1],
        ),
        Err(BusinessStatus::InvalidArgument)
    );
    // 空字节：schema_version 缺失同样不得当作 proto3 默认兼容
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.capabilities.get",
            &query_bytes()[..0],
        ),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.queries(), 0);
}

#[test]
fn wire_unknown_field_number_maps_to_invalid_argument() {
    let fx = Fixture::new("codec-unknown-field");
    // Query 只定义字段 1；字段 2 属 schema 之外 → 首版直接拒绝
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.capabilities.get",
            &[0x08, 0x01, 0x10, 0x02],
        ),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.mutations(), 0);
}

#[test]
fn wire_duplicate_singular_field_maps_to_invalid_argument() {
    let fx = Fixture::new("codec-duplicate-field");
    // canonical 编码不可能重复写 singular 字段
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.task.get",
            &[0x08, 0x01, 0x08, 0x01],
        ),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.queries(), 0);
}

#[test]
fn wire_invalid_utf8_string_field_maps_to_invalid_argument() {
    let fx = Fixture::new("codec-invalid-utf8");
    // TaskQuery{ schema_version=1, sub_task_id=<非 UTF-8> }
    let mut bytes = vec![0x08, 0x01, 0x12, 0x04];
    bytes.extend_from_slice(&[0xff, 0xfe, 0xfd, 0xfc]);
    assert_eq!(
        fx.agent
            .handle_wire(&BusinessCodec, &peer(&KEY), "device.task.get", &bytes),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.queries(), 0);
}

// ---------------------------------------------------------------------------
// 4a) 版本不受支持（1001）与参数非法（1003）的严格分流 + 两条入口 parity
// ---------------------------------------------------------------------------

#[test]
fn wire_unsupported_schema_version_maps_to_unsupported_version() {
    let fx = Fixture::new("codec-version-1001");
    // 结构完全合法（字段 1 varint = 2），只有 schema_version 不受支持：
    // 协议层把它分类为可协商的 `UnsupportedSchemaVersion`，板端必须回 1001，
    // 绝不再降级成 1003「参数非法」——中控据此才可能触发版本/能力协商。
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.capabilities.get",
            &[0x08, 0x02],
        ),
        Err(BusinessStatus::UnsupportedVersion)
    );
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.queries(), 0, "版本拒绝必须在触达 source/journal 之前");
    assert_eq!(fx.store.record_count(), 0);
}

#[test]
fn wire_blank_sub_task_id_maps_to_invalid_argument() {
    let fx = Fixture::new("codec-blank-1003");
    // 与版本用例拆开：空白 sub_task_id 是**这条请求自身的参数缺陷**，
    // 版本 1 完全受支持，因此必须是 1003 而不是 1001。
    let mut blank = vec![0x08, 0x01, 0x12, 0x03];
    blank.extend_from_slice(b"   ");
    assert_eq!(
        fx.agent
            .handle_wire(&BusinessCodec, &peer(&KEY), "device.task.get", &blank),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.queries(), 0, "契约失败必须在触达 journal 之前");
    assert_eq!(fx.store.record_count(), 0);
}

#[test]
fn wire_and_typed_entries_agree_on_unsupported_schema_version() {
    let fx = Fixture::new("codec-version-parity");
    // parity：同一「版本 2」语义在两条入口上必须得到同一个 status_code。
    // wire 入口经 `CodecError::UnsupportedSchemaVersion`，typed 入口经
    // `DtoError::UnsupportedVersion`；两条路径若分叉，中控按入口不同会看到不同错误码。
    let typed = BusinessPayload::Query(Query {
        schema_version: UNSUPPORTED_VERSION,
    });
    // 入站字节手工构造（编码侧对非法 DTO fail-closed，永远产不出版本 2）：
    // `Query{schema_version=2}` 的 canonical 编码恒为 `[0x08, 0x02]`。
    let wire_bytes = vec![0x08u8, UNSUPPORTED_VERSION as u8];
    assert_eq!(wire_bytes, vec![0x08, 0x02]);
    let from_wire = fx.agent.handle_wire(
        &BusinessCodec,
        &peer(&KEY),
        "device.capabilities.get",
        &wire_bytes,
    );
    let from_typed = fx
        .agent
        .handle_typed_action(&peer(&KEY), "device.capabilities.get", &typed);
    assert_eq!(from_wire, Err(BusinessStatus::UnsupportedVersion));
    assert_eq!(from_typed, Err(BusinessStatus::UnsupportedVersion));
    assert_eq!(
        from_wire, from_typed,
        "wire 与 typed 对同一版本违规必须同码（1001）"
    );
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.queries(), 0);
}

// ---------------------------------------------------------------------------
// 4b) 资源上限：整包 > 512 KiB → 1008
// ---------------------------------------------------------------------------

#[test]
fn wire_oversized_payload_maps_to_resource_exhausted() {
    let fx = Fixture::new("codec-length-limit-1008");
    // 白名单与 expected kind 都在 codec 之前完成判定；本例 action 合法、
    // expected=Query 也已确定，因此唯一的拒绝原因就是 codec 的整包长度守卫。
    let mut oversized = vec![0x08u8, 0x01];
    oversized.resize(MAX_BUSINESS_MESSAGE_BYTES + 1, 0x00);
    assert!(oversized.len() > MAX_BUSINESS_MESSAGE_BYTES);
    assert_eq!(
        fx.agent
            .handle_wire(&BusinessCodec, &peer(&KEY), "device.status.get", &oversized,),
        Err(BusinessStatus::ResourceExhausted)
    );
    assert_eq!(fx.mutations(), 0);
    assert_eq!(
        fx.queries(),
        0,
        "长度守卫必须在任何 source/journal 触达之前"
    );
    assert_eq!(fx.store.record_count(), 0);
}

#[test]
fn wire_payload_at_the_size_limit_is_not_resource_exhausted() {
    let fx = Fixture::new("codec-length-boundary");
    // 边界对照：**恰好** 512 KiB 不越界，因此绝不能报 1008；
    // 它随后按字节继续解析并因非 canonical 尾部落到 1003。
    // 这条断言证明 1008 的触发条件是「超过」上限，而不是「达到」上限。
    let mut at_limit = vec![0x08u8, 0x01];
    at_limit.resize(MAX_BUSINESS_MESSAGE_BYTES, 0x00);
    assert_eq!(at_limit.len(), MAX_BUSINESS_MESSAGE_BYTES);
    assert_eq!(
        fx.agent
            .handle_wire(&BusinessCodec, &peer(&KEY), "device.status.get", &at_limit,),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.queries(), 0);
}

#[test]
fn wire_payload_kind_mismatch_maps_to_invalid_argument() {
    let fx = Fixture::new("codec-kind-mismatch");
    // 只读动作期望 Query，却送来 TaskQuery（字段 2 对 Query 是未知字段）
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.capabilities.get",
            &task_query_bytes(SUB_TASK),
        ),
        Err(BusinessStatus::InvalidArgument)
    );
    // 反向：device.task.get 期望 TaskQuery，却送来 Query（sub_task_id 缺失）
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.task.get",
            &query_bytes(),
        ),
        Err(BusinessStatus::InvalidArgument)
    );
    assert_eq!(fx.queries(), 0);
}

// ---------------------------------------------------------------------------
// 5) 部署侧没有获批 codec（1009）与白名单优先级（1002）
// ---------------------------------------------------------------------------

#[test]
fn wire_without_approved_codec_implementation_is_internal_error() {
    let fx = Fixture::new("codec-unapproved");
    // 字节完全合法：拒绝原因只能是「本次没有获批 codec 见证」（CodecUnapproved），
    // 即本端部署/门禁故障 → 1009 InternalError。
    // 不再断言 1001：那会把「本端没有获批 codec」伪装成「对端版本不匹配」，
    // 诱导中控去做永远不可能成功的版本协商。
    assert_eq!(
        fx.agent.handle_wire(
            &NoCodec,
            &peer(&KEY),
            "device.capabilities.get",
            &query_bytes(),
        ),
        Err(BusinessStatus::InternalError)
    );
    assert_eq!(
        fx.agent.handle_wire(
            &NoCodec,
            &peer(&KEY),
            "device.task.get",
            &task_query_bytes(SUB_TASK),
        ),
        Err(BusinessStatus::InternalError)
    );
    // fail-closed 不变：未 codec 成功前绝不触达 journal/source
    assert_eq!(fx.queries(), 0);
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.store.record_count(), 0);
}

#[test]
fn wire_allowlist_still_wins_before_any_codec_work() {
    let fx = Fixture::new("codec-allowlist");
    let valid = query_bytes();
    for action in ["device.task.result", "device.fan.apply", "tunnel.ping", ""] {
        assert_eq!(
            fx.agent
                .handle_wire(&BusinessCodec, &peer(&KEY), action, &valid),
            Err(BusinessStatus::UnsupportedAction),
            "action={action}"
        );
    }
    assert_eq!(fx.mutations(), 0);
    assert_eq!(fx.queries(), 0);
}

#[test]
fn wire_entry_matches_typed_entry_for_the_same_payload() {
    let fx = Fixture::new("codec-typed-parity");
    let payload = BusinessPayload::TaskQuery(TaskQuery {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
    });
    fx.store
        .insert_accepted(&KEY, SUB_TASK, &[7u8; 32], BOOT)
        .expect("seed accepted record");
    // 类型化入口仍可直接使用（绕过 wire），与真实编解码路径结果一致
    assert_eq!(
        fx.agent.handle_wire(
            &BusinessCodec,
            &peer(&KEY),
            "device.task.get",
            &encode(&payload),
        ),
        fx.agent
            .handle_typed_action(&peer(&KEY), "device.task.get", &payload)
    );
}

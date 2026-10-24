//! 板端主动事件 `device.task.result` 的**生产发送链路**验收。
//!
//! 链路是真实的双端加密会话：TCP → v3 双向 pin 握手 → 加密记录层（`SecureIo`）→
//! h2 双流会话。板端业务处理走生产同款 [`BoardSessionHandler::on_request`]，
//! 只有 `BusinessDispatch` 是替身（不触达真实 journal 目录、不触真实主机重启）。
//!
//! 断言方向（缺一项都不算打通）：
//! * `device.reboot.execute` 裁决出成功 `TaskRecord` 后，中控一侧在**控制流**上真的
//!   收到一帧 `TYPE_EVENT`，动作 = `device.task.result`，载荷可解码为同一份记录；
//! * 事件是独立帧：不回包、不占用响应的 trace，业务响应本身仍是 status=0 的 `TaskRecord`；
//! * 只读动作、journal 读动作与非零（拒绝）裁决**一律不产生**事件；
//! * 会话句柄尚未 attach 时事件被**跳过并计数**，绝不改写已裁决的业务回包。
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rsetup_board_agent::dispatch::{BusinessResponse, BusinessStatus};
use rsetup_board_agent::journal::{TaskRecord as JournalTaskRecord, TaskState};
use rsetup_board_agent::read_only::PeerContext;
use rsetup_board_agent::session_handler::{BoardSessionHandler, BusinessDispatch, BusinessHandler};
use rsetup_board_agent::{REBOOT_EXECUTE_ACTION, TASK_RESULT_EVENT_ACTION};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, BusinessPayloadKind, TaskRecord,
    approved_codec_witness, decode_business,
};
use rsetup_tunnel::envelope::{PacketType, StreamKind, TunnelPacket};
use rsetup_tunnel::handshake::{
    ClientHandshakeConfig, ServerHandshakeConfig, client_handshake, server_handshake,
};
use rsetup_tunnel::identity::IdentityKey;
use rsetup_tunnel::secure_io::SecureIo;
use rsetup_tunnel::session::{
    HandlerFuture, RequestContext, SessionConfig, SessionHandle, SessionHandler, SessionOutcome,
    StreamEpoch, start_client_session, start_server_session,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

const BOOT: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const EPOCH: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

/// 替身分发的脚本选择：载荷就是这个字面值时判定「确证拒绝」（无事件）。
const REJECT_MARKER: &[u8] = b"reject";

// ------------------------------------------------------------------ 记录夹具

/// 用真实 journal 记录类型造 wire 记录：`state` 由 `TaskState` 枚举保证是合法字面值。
fn wire_record(state: TaskState) -> TaskRecord {
    let failed = state == TaskState::Failed;
    JournalTaskRecord {
        schema_version: BUSINESS_SCHEMA_VERSION,
        sub_task_id: SUB_TASK.to_string(),
        journal_epoch: EPOCH.to_string(),
        record_version: 1,
        state,
        pre_boot_id: BOOT.to_string(),
        observed_boot_id: None,
        reason_code: if failed {
            Some("OS_ERROR".to_string())
        } else {
            None
        },
        updated_unix_ms: Some(1_700_000_000_000),
        evidence_kind: None,
    }
    .to_wire()
}

// ------------------------------------------------------------------ 替身分发

/// 记录每一次触达，并按动作给出**固定裁决**：
/// * `device.reboot.execute` → 成功 `TaskRecord`（默认）/ `DeviceBusy`（`REJECT_MARKER`）；
/// * `device.capabilities.get` → 成功 `Capabilities`；
/// * `device.task.get` → 成功 `TaskRecord`（**但不是 execute**，必须无事件）。
struct ScriptedDispatch {
    calls: AtomicUsize,
    actions: Mutex<Vec<String>>,
    record_state: TaskState,
}

impl ScriptedDispatch {
    fn new(record_state: TaskState) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            actions: Mutex::new(Vec::new()),
            record_state,
        })
    }
}

impl BusinessDispatch for ScriptedDispatch {
    fn handle_business(
        &self,
        _peer: &PeerContext,
        action: &str,
        payload: &[u8],
    ) -> Result<BusinessResponse, BusinessStatus> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.actions.lock().unwrap().push(action.to_string());
        match action {
            "device.reboot.execute" if payload == REJECT_MARKER => Err(BusinessStatus::DeviceBusy),
            "device.reboot.execute" => {
                Ok(BusinessResponse::TaskRecord(wire_record(self.record_state)))
            }
            "device.task.get" => Ok(BusinessResponse::TaskRecord(wire_record(self.record_state))),
            "device.capabilities.get" => Ok(BusinessResponse::Capabilities(
                rsetup_protocol::wire::Capabilities {
                    schema_version: BUSINESS_SCHEMA_VERSION,
                    boot_id: BOOT.to_string(),
                    agent_version: "0.5.0".to_string(),
                    supported_actions: vec![REBOOT_EXECUTE_ACTION.to_string()],
                    durable_task_journal: true,
                    max_status_bytes: 65_536,
                    journal_epoch: EPOCH.to_string(),
                },
            )),
            _ => Err(BusinessStatus::UnsupportedAction),
        }
    }
}

// ------------------------------------------------------------------ 双端夹具

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "rsetup-board-task-result-{tag}-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(&dir).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&dir, permissions).unwrap();
    dir
}

async fn wait_for(what: &str, condition: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while !condition() {
        assert!(std::time::Instant::now() < deadline, "等待「{what}」超时");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 中控侧 handler：只观察**入站事件**（板端→中控），请求一律不回业务。
#[derive(Default)]
struct EventRecorder {
    events: Mutex<Vec<(StreamKind, TunnelPacket)>>,
}

impl EventRecorder {
    fn snapshot(&self) -> Vec<(StreamKind, TunnelPacket)> {
        self.events.lock().unwrap().clone()
    }
}

impl SessionHandler for EventRecorder {
    fn on_request(&self, _ctx: RequestContext) -> HandlerFuture {
        Box::pin(async {
            rsetup_tunnel::session::Reply {
                status_code: 1002,
                error_message: "controller serves no business".to_string(),
                payload: Vec::new().into(),
            }
        })
    }
    fn on_event(&self, stream: StreamKind, packet: TunnelPacket) {
        self.events.lock().unwrap().push((stream, packet));
    }
}

/// 真实中控：TCP + v3 服务端握手 + 服务端会话；入站事件记在 [`EventRecorder`] 上。
struct Controller {
    address: SocketAddr,
    controller_key: [u8; 32],
    recorder: Arc<EventRecorder>,
    sessions: tokio::sync::Mutex<mpsc::UnboundedReceiver<SessionHandle>>,
}

impl Controller {
    async fn start(board_key: [u8; 32]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let identity =
            Arc::new(IdentityKey::load_or_generate(temp_dir("controller").join("id.pk8")).unwrap());
        let controller_key = identity.public_key();
        let recorder = Arc::new(EventRecorder::default());
        let callback = Arc::clone(&recorder);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let _ = stream.set_nodelay(true);
                let config = ServerHandshakeConfig {
                    identity: Arc::clone(&identity),
                };
                let Ok(verified) = server_handshake(stream, &config, &AllowAll { board_key }).await
                else {
                    continue;
                };
                let secure = SecureIo::new(
                    verified.io,
                    verified.session_keys.into_server_record_layers(),
                );
                let Ok((handle, driver)) = start_server_session(
                    secure,
                    Arc::clone(&callback) as Arc<dyn SessionHandler>,
                    SessionConfig::default(),
                ) else {
                    continue;
                };
                if sender.send(handle).is_err() {
                    return;
                }
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(60), driver).await;
                });
            }
        });
        Self {
            address,
            controller_key,
            recorder,
            sessions: tokio::sync::Mutex::new(receiver),
        }
    }
    async fn next_session(&self) -> SessionHandle {
        tokio::time::timeout(Duration::from_secs(25), self.sessions.lock().await.recv())
            .await
            .expect("等待板端建连超时")
            .expect("会话通道已关闭")
    }
    fn task_result_events(&self) -> Vec<(StreamKind, TunnelPacket)> {
        self.recorder
            .snapshot()
            .into_iter()
            .filter(|(_, packet)| packet.action == TASK_RESULT_EVENT_ACTION)
            .collect()
    }
}

struct AllowAll {
    board_key: [u8; 32],
}

impl rsetup_tunnel::handshake::AdmissionPolicy for AllowAll {
    fn evaluate<'a>(
        &'a self,
        peer: &'a rsetup_tunnel::handshake::VerifiedPeer,
    ) -> rsetup_tunnel::handshake::AdmissionFuture<'a> {
        let approved = peer.public_key == self.board_key;
        Box::pin(async move {
            if approved {
                Ok(rsetup_tunnel::handshake::AdmissionDecision::Approved)
            } else {
                Err(rsetup_tunnel::handshake::HandshakeError::Policy)
            }
        })
    }
}

/// 一条真实板端会话（句柄 + 驱动 + 绑定槽 + 业务 handler），默认已 attach。
struct Board {
    handler: Arc<BoardSessionHandler>,
    handle: SessionHandle,
    driver: tokio::task::JoinHandle<SessionOutcome>,
}

impl Board {
    async fn connect(
        address: SocketAddr,
        controller_key: [u8; 32],
        board_identity: Arc<IdentityKey>,
        dispatch: Arc<ScriptedDispatch>,
    ) -> Self {
        let stream = TcpStream::connect(address).await.unwrap();
        let _ = stream.set_nodelay(true);
        let handshake = ClientHandshakeConfig {
            identity: board_identity,
            trusted_servers: vec![controller_key],
            descriptor: DeviceDescriptor::new("sn-event", "model-test", "fw-1", "event-test"),
        };
        let result = client_handshake(stream, &handshake)
            .await
            .expect("真实握手必须成功");
        let secure = SecureIo::new(result.io, result.session_keys.into_client_record_layers());
        let handler = BoardSessionHandler::new(Arc::new(BusinessHandler::new(
            dispatch,
            result.peer_public_key,
            "conn-event",
        )));
        let binding = handler.binding();
        let session_handler: Arc<dyn SessionHandler> = handler.clone();
        let (handle, driver) =
            start_client_session(secure, session_handler, SessionConfig::default())
                .expect("会话驱动必须启动");
        assert!(binding.attach(handle.clone()), "首代际必须绑定成功");
        Self {
            handler,
            handle,
            driver,
        }
    }

    async fn wait_ready(&self) -> Result<(), rsetup_tunnel::session::RequestError> {
        self.handle.wait_ready().await
    }
}

/// 双端装配好的会话对。
struct Pair {
    controller: Controller,
    peer: SessionHandle,
    board: Board,
    /// 临时目录必须活到用例结束（身份密钥文件所在目录）。
    _board_dir: PathBuf,
}

async fn pair(record_state: TaskState) -> Pair {
    let board_dir = temp_dir("board");
    let board_identity = Arc::new(IdentityKey::load_or_generate(board_dir.join("id.pk8")).unwrap());
    let controller = Controller::start(board_identity.public_key()).await;
    let dispatch = ScriptedDispatch::new(record_state);
    let board = Board::connect(
        controller.address,
        controller.controller_key,
        Arc::clone(&board_identity),
        Arc::clone(&dispatch),
    )
    .await;
    let peer = controller.next_session().await;
    // 两侧都必须 ready（§6.6：两条流各自首 probe 有效 pong），否则不算会话打开。
    tokio::time::timeout(Duration::from_secs(25), board.wait_ready())
        .await
        .expect("板端 ready 超时")
        .expect("板端 ready 失败");
    tokio::time::timeout(Duration::from_secs(25), peer.wait_ready())
        .await
        .expect("中控 ready 超时")
        .expect("中控 ready 失败");
    Pair {
        controller,
        peer,
        board,
        _board_dir: board_dir,
    }
}

impl Pair {
    /// 中控在控制流上发一次业务请求（板端回包）。
    async fn call(&self, action: &str, payload: Vec<u8>) -> rsetup_tunnel::session::Reply {
        self.peer
            .request(StreamKind::Control, action, payload)
            .await
            .expect("中控请求必须得到回包")
    }
    async fn shutdown(self) {
        self.board.handle.abort();
        let _ = tokio::time::timeout(Duration::from_secs(30), self.board.driver).await;
        self.peer.abort();
    }
}

// ------------------------------------------------------------------ 用例

/// 核心链路：execute 成功 → 控制流上多出恰好一帧 `device.task.result` 事件。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execute_on_open_session_emits_task_result_event() {
    for state in [
        TaskState::Accepted,
        TaskState::Attempted,
        TaskState::Succeeded,
        TaskState::Failed,
        TaskState::Unknown,
    ] {
        let pair = pair(state).await;
        let reply = pair
            .call(REBOOT_EXECUTE_ACTION, b"execute-once".to_vec())
            .await;
        // 业务响应本身：status=0 + 同一份 TaskRecord。
        assert_eq!(reply.status_code, 0, "{state:?} 必须回成功");
        let BusinessPayload::TaskRecord(record) = decode_business(
            &BusinessCodec,
            Some(&approved_codec_witness()),
            BusinessPayloadKind::TaskRecord,
            &reply.payload,
        )
        .expect("响应载荷必须是可解码的 TaskRecord") else {
            panic!("响应载荷类型必须是 TaskRecord");
        };
        assert_eq!(record.sub_task_id, SUB_TASK);

        wait_for("板端发出 device.task.result 事件", || {
            !pair.controller.task_result_events().is_empty()
        })
        .await;
        let events = pair.controller.task_result_events();
        assert_eq!(
            events.len(),
            1,
            "一次 execute 只允许一帧结果事件，实际 {events:#?}"
        );
        let (stream, packet) = &events[0];
        // 包类型 = TYPE_EVENT、流 = Control、动作 = device.task.result。
        assert_eq!(packet.packet_type, PacketType::Event, "必须是 TYPE_EVENT");
        assert_eq!(*stream, StreamKind::Control, "事件必须在控制流");
        assert_eq!(packet.action, TASK_RESULT_EVENT_ACTION);
        // 事件禁止回包：非 Response 不得携带 status_code / error_message。
        assert_eq!(packet.status_code, 0);
        assert!(packet.error_message.is_empty());
        // 载荷与响应是同一份记录的同一编码路径。
        let BusinessPayload::TaskRecord(from_event) = decode_business(
            &BusinessCodec,
            Some(&approved_codec_witness()),
            BusinessPayloadKind::TaskRecord,
            &packet.payload,
        )
        .expect("事件载荷必须是可解码的 TaskRecord") else {
            panic!("事件载荷类型必须是 TaskRecord");
        };
        assert_eq!(from_event, record, "事件与响应必须是同一份记录");
        // 板端计数：交出 1、跳过 0。
        assert_eq!(pair.board.handler.events_sent(), 1);
        assert_eq!(pair.board.handler.events_skipped(), 0);
        pair.shutdown().await;
    }
}

/// 只读动作与 `device.task.get` 都不产生结果事件；被拒绝的 execute 也不产生。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn other_adjudications_emit_no_event() {
    let pair = pair(TaskState::Accepted).await;

    let reply = pair.call("device.capabilities.get", b"caps".to_vec()).await;
    assert_eq!(reply.status_code, 0);
    let reply = pair.call("device.task.get", b"query".to_vec()).await;
    assert_eq!(reply.status_code, 0);
    let reply = pair
        .call(REBOOT_EXECUTE_ACTION, REJECT_MARKER.to_vec())
        .await;
    assert_eq!(reply.status_code, BusinessStatus::DeviceBusy as i32);
    assert!(
        reply.payload.is_empty(),
        "拒绝回包必须空 payload，绝不带半截记录"
    );

    // 给可能误发的事件一点时间：仍未产生任何一帧。
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        pair.controller.task_result_events().is_empty(),
        "非 execute 成功裁决不得发事件：{:?}",
        pair.controller.task_result_events()
    );
    assert_eq!(pair.board.handler.events_sent(), 0);
    pair.shutdown().await;
}

/// 句柄尚未 attach（本 connection 还没就位）时，事件被跳过并计数，
/// 业务回包本身不受影响，也不得有事件被排进未来。
#[tokio::test]
async fn event_is_skipped_while_the_session_is_not_attached() {
    let dispatch = ScriptedDispatch::new(TaskState::Accepted);
    let handler = BoardSessionHandler::new(Arc::new(BusinessHandler::new(
        dispatch,
        [3u8; 32],
        "conn-unattached",
    )));
    assert!(!handler.binding().is_attached());

    let packet = TunnelPacket {
        trace_id: 1,
        packet_type: PacketType::Request,
        action: REBOOT_EXECUTE_ACTION.to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: b"execute-once".to_vec(),
        metadata: Default::default(),
    };
    let ctx = RequestContext {
        stream: StreamKind::Control,
        epoch: StreamEpoch(1),
        packet,
    };
    let reply = handler.on_request(ctx).await;

    assert_eq!(reply.status_code, 0, "回包裁决绝不因事件未投递而改变");
    assert_eq!(handler.events_sent(), 0, "未 attach 不得谎称已发送");
    assert_eq!(handler.events_skipped(), 1, "跳过必须可观察");
}

/// 裁决层的门：只有 execute 的成功 `TaskRecord` 产出事件；其余一律空。
#[test]
fn adjudication_produces_events_only_for_successful_execute() {
    let dispatch = ScriptedDispatch::new(TaskState::Accepted);
    let handler = BusinessHandler::new(
        Arc::clone(&dispatch) as Arc<dyn BusinessDispatch>,
        [4u8; 32],
        "conn-pure",
    );
    let request = |action: &str, payload: &[u8]| TunnelPacket {
        trace_id: 7,
        packet_type: PacketType::Request,
        action: action.to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: payload.to_vec(),
        metadata: Default::default(),
    };

    let ok = handler.adjudicate(StreamKind::Control, &request(REBOOT_EXECUTE_ACTION, b"ok"));
    assert_eq!(ok.events.len(), 1);
    assert_eq!(ok.events[0].action, TASK_RESULT_EVENT_ACTION);
    assert_eq!(ok.events[0].stream, StreamKind::Control);
    assert!(ok.decision.reply().is_some());

    // 被拒绝的 execute：非零 status、空 payload → 无事件。
    let busy = handler.adjudicate(
        StreamKind::Control,
        &request(REBOOT_EXECUTE_ACTION, REJECT_MARKER),
    );
    assert_eq!(
        busy.decision.reply().map(|reply| reply.status_code),
        Some(BusinessStatus::DeviceBusy as i32)
    );
    assert!(busy.events.is_empty());

    // 同一份 TaskRecord 经 device.task.get 返回 → 不是结果事件。
    let read = handler.adjudicate(StreamKind::Control, &request("device.task.get", b"q"));
    assert!(read.decision.reply().is_some());
    assert!(read.events.is_empty(), "device.task.get 不是结果事件来源");

    // 数据流上的 execute：入参非法，同样无事件。
    let on_data = handler.adjudicate(StreamKind::Data, &request(REBOOT_EXECUTE_ACTION, b"ok"));
    assert!(on_data.events.is_empty());
    assert_eq!(
        on_data.decision.reply().map(|reply| reply.status_code),
        Some(BusinessStatus::InvalidArgument as i32)
    );
}

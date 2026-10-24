//! `Indeterminate` 的「必须主动关闭当前会话」验收（首轮修复 F1）。
//!
//! 这里没有伪网络 handler：链路是**真实** TCP → v3 双向 pin 握手 → 加密记录层
//! （`SecureIo`）→ h2 双流会话，「结果不可知」由注入的 [`BusinessDispatch`] 产生，
//! 走的是生产同款 `BoardSessionHandler::on_request` 裁决路径。断言方向：
//!
//! * 中止意图发生在 `attach` **之前**也必须生效（确定性延迟 attach，不靠调度概率）；
//! * 对端的在途请求以「关闭」结束，**不是**任何业务回包（既无成功也无 1009）；
//! * 驱动任务与对端 handler 任务都收口（join 收齐），不留下挂着的 handler future；
//! * 绑定槽只服务一条 connection：新代际句柄不能顶替绑定，也不会被旧意图误关。
//!
//! 不使用真实凭据、不触达真实主机重启（本文件甚至不需要任何本机只读来源）。
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rsetup_board_agent::connect::{
    ConnectEvent, ConnectEventKind, ReconnectPolicy, RunOutcome, SharedAgentFactory, Shutdown,
    TunnelConnector, run_board_connection,
};
use rsetup_board_agent::connect_config::BoardConnectConfig;
use rsetup_board_agent::dispatch::{BusinessResponse, BusinessStatus};
use rsetup_board_agent::read_only::PeerContext;
use rsetup_board_agent::session_handler::{
    BoardSessionHandler, BusinessDispatch, BusinessHandler, SessionBinding,
};
use rsetup_protocol::BusinessCodec;
use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_protocol::wire::{
    BUSINESS_SCHEMA_VERSION, BusinessPayload, Query, approved_codec_witness, encode_business,
};
use rsetup_tunnel::envelope::StreamKind;
use rsetup_tunnel::handshake::{
    AdmissionDecision, AdmissionFuture, AdmissionPolicy, ClientHandshakeConfig, HandshakeError,
    ServerHandshakeConfig, VerifiedPeer, client_handshake, server_handshake,
};
use rsetup_tunnel::identity::IdentityKey;
use rsetup_tunnel::secure_io::SecureIo;
use rsetup_tunnel::session::{
    HandlerFuture, Reply, RequestContext, RequestError, SessionCloseReason, SessionConfig,
    SessionHandle, SessionHandler, start_client_session, start_server_session,
};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// 业务分发的替身：把**每一次**触达都记下来，并一律判定「结果不可知」。
///
/// 记录 `peer.authenticated_controller_key` 是为了证明身份仍只来自握手验签结果。
struct IndeterminateDispatch {
    calls: AtomicUsize,
    peers: std::sync::Mutex<Vec<[u8; 32]>>,
    actions: std::sync::Mutex<Vec<String>>,
}

impl IndeterminateDispatch {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            peers: std::sync::Mutex::new(Vec::new()),
            actions: std::sync::Mutex::new(Vec::new()),
        })
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn peers(&self) -> Vec<[u8; 32]> {
        self.peers.lock().unwrap().clone()
    }
    fn actions(&self) -> Vec<String> {
        self.actions.lock().unwrap().clone()
    }
}

impl BusinessDispatch for IndeterminateDispatch {
    fn handle_business(
        &self,
        peer: &PeerContext,
        action: &str,
        payload: &[u8],
    ) -> Result<BusinessResponse, BusinessStatus> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.peers
            .lock()
            .unwrap()
            .push(peer.authenticated_controller_key);
        self.actions.lock().unwrap().push(action.to_string());
        // 载荷必须真的穿过真实 codec 才可能到这里（非空 = 业务请求形状正确）。
        assert!(!payload.is_empty(), "业务分发必须收到真实编码后的载荷");
        Ok(BusinessResponse::Indeterminate)
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rsetup-board-abort-{tag}-{}", uuid::Uuid::new_v4()));
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

/// 生产装配的退避：抖动固定 0，基数/上限压到毫秒级（抖动另有专项断言，见 tests/connect.rs）。
fn production_policy() -> ReconnectPolicy {
    ReconnectPolicy::with_jitter(Duration::from_millis(5), Duration::from_millis(40), || {
        Duration::from_secs(0)
    })
}

fn events_collector() -> (
    Arc<std::sync::Mutex<Vec<ConnectEvent>>>,
    rsetup_board_agent::connect::Observer,
) {
    let events: Arc<std::sync::Mutex<Vec<ConnectEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let observer: rsetup_board_agent::connect::Observer =
        Arc::new(move |event| sink.lock().unwrap().push(event));
    (events, observer)
}

fn ready_count(events: &Arc<std::sync::Mutex<Vec<ConnectEvent>>>) -> usize {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.kind == ConnectEventKind::Ready)
        .count()
}

fn query_bytes() -> Vec<u8> {
    encode_business(
        &BusinessCodec,
        Some(&approved_codec_witness()),
        &BusinessPayload::Query(Query {
            schema_version: BUSINESS_SCHEMA_VERSION,
        }),
    )
    .unwrap()
}

struct TestAdmission {
    board_key: [u8; 32],
}

impl AdmissionPolicy for TestAdmission {
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        if peer.public_key != self.board_key {
            return Box::pin(async { Err(HandshakeError::Policy) });
        }
        Box::pin(async { Ok(AdmissionDecision::Approved) })
    }
}

/// 中控侧不回应业务：本文件只由中控发请求，回包与否完全由板端裁决决定。
struct NullHandler;

impl SessionHandler for NullHandler {
    fn on_request(&self, _ctx: RequestContext) -> HandlerFuture {
        Box::pin(async {
            Reply {
                status_code: 1002,
                error_message: "not served here".to_string(),
                payload: Vec::new().into(),
            }
        })
    }
}

/// 真实中控：TCP + v3 服务端握手 + 服务端会话；每条 connection 的驱动任务都留下收口证据。
struct Controller {
    address: SocketAddr,
    controller_key: [u8; 32],
    sessions: tokio::sync::Mutex<mpsc::UnboundedReceiver<SessionHandle>>,
    /// 已结束的**服务端**驱动任务数（对端 handler/驱动任务收齐的证据）。
    closed_drivers: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Controller {
    async fn start(board_key: [u8; 32]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let closed_drivers = Arc::new(AtomicUsize::new(0));
        let identity =
            Arc::new(IdentityKey::load_or_generate(temp_dir("controller").join("id.pk8")).unwrap());
        let controller_key = identity.public_key();
        let closed_moves = Arc::clone(&closed_drivers);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let _ = stream.set_nodelay(true);
                let config = ServerHandshakeConfig {
                    identity: Arc::clone(&identity),
                };
                let admission = TestAdmission { board_key };
                let Ok(result) = server_handshake(stream, &config, &admission).await else {
                    continue;
                };
                let secure =
                    SecureIo::new(result.io, result.session_keys.into_server_record_layers());
                let Ok((handle, driver)) =
                    start_server_session(secure, Arc::new(NullHandler), SessionConfig::default())
                else {
                    continue;
                };
                if sender.send(handle).is_err() {
                    return;
                }
                let closed = Arc::clone(&closed_moves);
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(60), driver).await;
                    closed.fetch_add(1, Ordering::SeqCst);
                });
            }
        });
        Self {
            address,
            controller_key,
            sessions: tokio::sync::Mutex::new(receiver),
            closed_drivers,
            task,
        }
    }
    async fn next_session(&self) -> SessionHandle {
        tokio::time::timeout(Duration::from_secs(25), self.sessions.lock().await.recv())
            .await
            .expect("等待板端建连超时")
            .expect("会话通道已关闭")
    }
    fn stop(&self) {
        self.task.abort();
    }
}

/// 板端一条**已建立**的真实会话：句柄、驱动任务与（可能尚未 attach 的）绑定槽。
struct BoardSession {
    binding: SessionBinding,
    handle: SessionHandle,
    driver: JoinHandle<rsetup_tunnel::session::SessionOutcome>,
}

impl BoardSession {
    async fn connect(
        address: SocketAddr,
        controller_key: [u8; 32],
        board_identity: Arc<IdentityKey>,
        dispatch: Arc<IndeterminateDispatch>,
        connection_id: &str,
    ) -> Self {
        let stream = TcpStream::connect(address).await.unwrap();
        let _ = stream.set_nodelay(true);
        let handshake = ClientHandshakeConfig {
            identity: board_identity,
            trusted_servers: vec![controller_key],
            descriptor: DeviceDescriptor::new("sn-abort", "model-test", "fw-1", "abort-test"),
        };
        let result = client_handshake(stream, &handshake)
            .await
            .expect("真实握手必须成功");
        let secure = SecureIo::new(result.io, result.session_keys.into_client_record_layers());
        let handler = BoardSessionHandler::new(Arc::new(BusinessHandler::new(
            dispatch,
            result.peer_public_key,
            connection_id,
        )));
        let binding = handler.binding();
        let session_handler: Arc<dyn SessionHandler> = handler;
        let (handle, driver) =
            start_client_session(secure, session_handler, SessionConfig::default())
                .expect("会话驱动必须启动");
        Self {
            binding,
            handle,
            driver,
        }
    }
}

/// 一条完整的双端真实会话（默认**不 attach**，由用例决定 attach 时机）。
struct Pairwise {
    controller: Controller,
    board_identity: Arc<IdentityKey>,
    board_dir: PathBuf,
    dispatch: Arc<IndeterminateDispatch>,
}

impl Pairwise {
    async fn new() -> Self {
        let board_dir = temp_dir("board");
        let board_identity =
            Arc::new(IdentityKey::load_or_generate(board_dir.join("id.pk8")).unwrap());
        let controller = Controller::start(board_identity.public_key()).await;
        Self {
            controller,
            board_identity,
            board_dir,
            dispatch: IndeterminateDispatch::new(),
        }
    }

    async fn open_board_session(&self, connection_id: &str) -> BoardSession {
        BoardSession::connect(
            self.controller.address,
            self.controller.controller_key,
            Arc::clone(&self.board_identity),
            Arc::clone(&self.dispatch),
            connection_id,
        )
        .await
    }

    /// 双方两条流各自首 probe 成功（§6.6 ready 判据）。
    async fn both_ready(&self, board: &BoardSession, peer: &SessionHandle) {
        tokio::time::timeout(Duration::from_secs(25), board.handle.wait_ready())
            .await
            .expect("板端 wait_ready 必须有界")
            .expect("板端两条流首次健康");
        tokio::time::timeout(Duration::from_secs(25), peer.wait_ready())
            .await
            .expect("中控 wait_ready 必须有界")
            .expect("中控两条流首次健康");
    }
}

/// 对端在途请求的收口形态：只允许「因关闭而失败」。
/// 出现任何 `Reply` 都是伪造回包；`Timeout` 只表示「等不到结果」，同样不能当作
/// 本端已主动关闭会话的证据——本层要求的是关闭，不是放弃等待。
fn assert_closed_without_business_reply(outcome: Result<Reply, RequestError>, what: &str) {
    match outcome {
        Ok(reply) => panic!(
            "{what}：Indeterminate 绝不能产出业务回包，实际 status={}",
            reply.status_code
        ),
        Err(RequestError::Timeout) => {
            panic!("{what}：必须以连接/流关闭结清，而不是业务超时后结果不可知")
        }
        Err(_) => {}
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_requested_before_attach_still_closes_the_real_session() {
    let fixture = Pairwise::new().await;
    // 关键时序：会话已在其它 runtime 线程上开始服务请求，但外层**尚未 attach** 句柄。
    let board = fixture.open_board_session("conn-before-attach").await;
    let peer = fixture.controller.next_session().await;
    fixture.both_ready(&board, &peer).await;
    assert!(
        !board.binding.is_attached(),
        "本用例必须在 attach 之前完成 Indeterminate 裁决"
    );

    let in_flight = tokio::spawn(async move {
        peer.request(StreamKind::Control, "device.status.get", query_bytes())
            .await
    });
    wait_for("业务分发已判定 Indeterminate", || {
        fixture.dispatch.calls() >= 1
    })
    .await;
    // 裁决已发生，绑定槽仍然是空的：旧实现在这里把 abort 静默丢掉。
    assert!(!board.binding.is_attached());

    // attach 必须**补发**已记录的中止意图（幂等：之后任何 abort 都不会产生第二个会话）。
    let handle = board.handle.clone();
    assert!(board.binding.attach(handle.clone()), "首次 attach 必须成功");

    // 期限刻意压在中控侧 5s 业务超时之前：只有在 attach 时真的补发了中止，会话才会
    // **因本端意图**结束（`Abort`）；靠对端请求超时把连接拆掉属于另一种原因。
    let outcome = tokio::time::timeout(Duration::from_secs(3), board.driver)
        .await
        .expect("attach 之后必须立即真的关闭会话（旧实现里 attach 前的 abort 已丢失）")
        .expect("驱动任务不得 panic");
    assert_eq!(outcome.reason, SessionCloseReason::Abort);

    // 对端在途请求以关闭结束，而不是任何业务回包。
    let reply = tokio::time::timeout(Duration::from_secs(25), in_flight)
        .await
        .expect("在途请求必须收口，不得永挂")
        .expect("请求任务不得 panic");
    assert_closed_without_business_reply(reply, "attach 前 abort 的会话");

    // 身份只来自握手验签结果。
    assert_eq!(
        fixture.dispatch.peers(),
        vec![fixture.controller.controller_key],
        "业务分发只能看到握手认证的中控公钥"
    );
    assert_eq!(fixture.dispatch.actions(), vec!["device.status.get"]);

    // 对端驱动/ handler 任务也必须收口。
    let closed = Arc::clone(&fixture.controller.closed_drivers);
    wait_for("对端驱动任务结束", || {
        closed.load(Ordering::SeqCst) >= 1
    })
    .await;
    fixture.controller.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn abort_after_attach_closes_the_session_without_any_reply() {
    // 与上一个用例只差 attach 时机：两种交错都必须真的关闭会话。
    let fixture = Pairwise::new().await;
    let board = fixture.open_board_session("conn-after-attach").await;
    let peer = fixture.controller.next_session().await;
    fixture.both_ready(&board, &peer).await;
    let handle = board.handle.clone();
    assert!(board.binding.attach(handle.clone()));

    let in_flight = tokio::spawn(async move {
        peer.request(StreamKind::Control, "device.status.get", query_bytes())
            .await
    });
    let outcome = tokio::time::timeout(Duration::from_secs(25), board.driver)
        .await
        .expect("Indeterminate 必须关闭已绑定的会话")
        .expect("驱动任务不得 panic");
    assert_eq!(outcome.reason, SessionCloseReason::Abort);
    let reply = tokio::time::timeout(Duration::from_secs(25), in_flight)
        .await
        .expect("在途请求必须收口")
        .expect("请求任务不得 panic");
    assert_closed_without_business_reply(reply, "attach 后 abort 的会话");
    // 关闭后句柄不可再用：任何后续请求也只能得到错误，不会伪装成在线。
    let later = handle
        .request(StreamKind::Control, "device.status.get", query_bytes())
        .await;
    assert!(later.is_err(), "已关闭会话不得再产出回包");
    let closed = Arc::clone(&fixture.controller.closed_drivers);
    wait_for("对端驱动任务结束", || {
        closed.load(Ordering::SeqCst) >= 1
    })
    .await;
    fixture.controller.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_binding_never_rebinds_to_a_new_generation_or_aborts_an_unbound_session() {
    let fixture = Pairwise::new().await;
    let first = fixture.open_board_session("conn-first").await;
    let peer_first = fixture.controller.next_session().await;
    fixture.both_ready(&first, &peer_first).await;
    let first_handle = first.handle.clone();
    assert!(first.binding.attach(first_handle));

    // 同一个绑定槽不得接受第二条 connection 的句柄（连接层据此 abort 新会话并停止：
    // 顶替绑定等于把两条代际的会话状态混在一个 handler 上）。
    let second = fixture.open_board_session("conn-second").await;
    let peer_second = fixture.controller.next_session().await;
    fixture.both_ready(&second, &peer_second).await;
    assert!(
        !first.binding.attach(second.handle.clone()),
        "同一绑定槽不得接受第二条 connection 的句柄"
    );

    // 第一条会话按绑定关闭；第二条从未被绑定，因此**不得**被第一条的意图误关。
    first.binding.abort();
    let outcome = tokio::time::timeout(Duration::from_secs(25), first.driver)
        .await
        .expect("已绑定的会话必须被关闭")
        .expect("驱动任务不得 panic");
    assert_eq!(outcome.reason, SessionCloseReason::Abort);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !second.driver.is_finished(),
        "旧 connection 的 abort 不得关闭未绑定的新会话"
    );
    // 用例自清理：未绑定会话由调用方（连接层的 attach 失败分支）自己 abort。
    second.handle.abort();
    let second_outcome = tokio::time::timeout(Duration::from_secs(25), second.driver)
        .await
        .expect("未绑定会话必须能被调用方自己关闭")
        .expect("驱动任务不得 panic");
    assert_eq!(second_outcome.reason, SessionCloseReason::Abort);
    fixture.controller.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn indeterminate_on_the_production_path_closes_the_session_without_a_reply() {
    // 走**生产装配**：`TunnelConnector`（真实 TCP + 握手 + SecureIo + 会话驱动）
    // 与 `run_board_connection` 循环，业务裁决来自注入的 Indeterminate 分发。
    // 断言方向：会话真的被关闭（不是等 5s 业务超时）、没有任何业务回包、
    // 循环按退避重开新代际，收尾 join 收齐。
    let fixture = Pairwise::new().await;
    let state_dir = temp_dir("state");
    let key_file = state_dir.join("identity.pk8");
    let config = BoardConnectConfig::from_values(
        &fixture.controller.address.to_string(),
        &key_file,
        &fixture.board_dir,
        &hex::encode(fixture.controller.controller_key),
    )
    .expect("生产配置必须通过 validated 构造");
    let connector = TunnelConnector::new(
        config,
        Arc::clone(&fixture.board_identity),
        DeviceDescriptor::new("sn-abort-prod", "model-test", "fw-1", "abort-test"),
        Arc::new(SharedAgentFactory::new(
            Arc::clone(&fixture.dispatch) as Arc<dyn BusinessDispatch>
        )),
    );
    let (events, observer) = events_collector();
    let shutdown = Shutdown::new();
    let run = tokio::spawn(run_board_connection(
        Arc::new(connector),
        production_policy(),
        shutdown.clone(),
        observer,
    ));

    let peer = fixture.controller.next_session().await;
    wait_for("板端 Ready", || ready_count(&events) >= 1).await;
    tokio::time::timeout(Duration::from_secs(25), peer.wait_ready())
        .await
        .expect("中控 wait_ready 必须有界")
        .expect("中控两条流首次健康");

    let in_flight = tokio::spawn(async move {
        peer.request(StreamKind::Control, "device.status.get", query_bytes())
            .await
    });
    wait_for("生产 handler 判定 Indeterminate", || {
        fixture.dispatch.calls() >= 1
    })
    .await;
    // 3s 远小于会话业务期限（5s）：能在这里收口，只可能是会话被主动关闭。
    let reply = tokio::time::timeout(Duration::from_secs(3), in_flight)
        .await
        .expect("在途请求必须因会话关闭而收口，不能等到业务超时")
        .expect("请求任务不得 panic");
    let error = match reply {
        Ok(reply) => panic!(
            "生产路径上 Indeterminate 绝不能产出业务回包，实际 status={}",
            reply.status_code
        ),
        Err(error) => error,
    };
    assert_ne!(
        error,
        RequestError::Timeout,
        "必须以「关闭」结清，而不是超时后结果不可知"
    );

    // 关闭后循环按退避重开新代际（第二条真实会话、新 connection_id）。
    let _second = fixture.controller.next_session().await;
    wait_for("第二条会话板端 Ready", || ready_count(&events) >= 2).await;
    let closed = Arc::clone(&fixture.controller.closed_drivers);
    wait_for("第一条会话的对端驱动收口", || {
        closed.load(Ordering::SeqCst) >= 1
    })
    .await;

    // 身份只来自握手验签；每条 connection 都重新绑定。
    assert_eq!(
        fixture.dispatch.peers(),
        vec![fixture.controller.controller_key],
        "业务分发只能看到握手认证的中控公钥"
    );
    assert_eq!(fixture.dispatch.actions(), vec!["device.status.get"]);

    shutdown.signal();
    let outcome = tokio::time::timeout(Duration::from_secs(25), run)
        .await
        .expect("生产循环必须收口（说明驱动任务已 join）")
        .expect("循环任务不得 panic");
    assert_eq!(outcome, RunOutcome::Shutdown);
    let events = events.lock().unwrap().clone();
    assert!(
        events
            .iter()
            .filter(|event| event.kind == ConnectEventKind::SessionEnded)
            .count()
            >= 1,
        "被中止的会话必须作为结束事件报出: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == ConnectEventKind::DriverUnreclaimed),
        "真实驱动的收尾必须可确认: {events:?}"
    );
    fixture.controller.stop();
    let _ = std::fs::remove_dir_all(&state_dir);
    let _ = std::fs::remove_dir_all(&fixture.board_dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binding_state_is_observable_before_any_handle_exists() {
    // 绑定槽在没有任何会话时必须能如实回答「已请求中止」，否则上层无法区分
    // 「还没人要求停」与「停的要求已被丢掉」。
    let binding = SessionBinding::new();
    assert!(!binding.is_attached());
    assert!(
        !binding.abort_requested(),
        "没人要求停时必须能被区分出来（不能恒真掩盖丢包）"
    );
    binding.abort();
    binding.abort();
    assert!(
        binding.abort_requested(),
        "attach 之前的 abort 意图必须被记住，不能静默 no-op"
    );
    assert!(!binding.is_attached());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_attach_and_abort_always_ends_the_session() {
    // 两种顺序都可靠之后，还要证明**并发**交错也一样：反复 abort 与 attach 抢同一条
    // 全序，任何一轮都不允许出现「意图与句柄互相看不见」而把会话留在在线状态。
    let fixture = Pairwise::new().await;
    const ROUNDS: usize = 12;
    for round in 0..ROUNDS {
        let board = fixture
            .open_board_session(&format!("conn-race-{round}"))
            .await;
        let binding = board.binding.clone();
        let handle = board.handle.clone();
        let aborter = tokio::spawn(async move {
            for _ in 0..64 {
                binding.abort();
                tokio::task::yield_now().await;
            }
        });
        assert!(
            board.binding.attach(handle),
            "第 {round} 轮首次 attach 必须成功"
        );
        aborter.await.expect("abort 任务不得 panic");
        let outcome = tokio::time::timeout(Duration::from_secs(5), board.driver)
            .await
            .unwrap_or_else(|_| {
                panic!("第 {round} 轮：attach 与 abort 并发时会话必须被关闭（意图被丢掉）")
            })
            .expect("驱动任务不得 panic");
        assert_eq!(outcome.reason, SessionCloseReason::Abort);
    }
    fixture.controller.stop();
}

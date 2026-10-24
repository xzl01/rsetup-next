//! Task 2: 真实双向 Ed25519 签名握手测试（protocol_spec.md §4.1–§4.6、计划 Task 2）。
//!
//! 全部走 `tokio::io::duplex` 的真实异步管道与真实 ring 签名/密钥协商；
//! 时序断言在 `start_paused` 虚拟时钟下进行。测试不落盘、不含任何固定生产密钥。

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use rsetup_protocol::crypto::EphemeralKeyPair;
use rsetup_protocol::frame::{FRAME_HEADER_LEN, FRAME_MAGIC, FRAME_VERSION, FrameType};
use rsetup_protocol::handshake_adapter::{
    HandshakeMessage, decode_handshake_frame, encode_handshake_frame,
};
use rsetup_protocol::handshake_msg::{
    ClientAuthRequest, ClientHello, DeviceDescriptor, PendingPong, ServerAuthResponse,
    ServerChallenge, ServerPending,
};
use rsetup_protocol::sig::{client_signature_input, server_signature_input};
use rsetup_tunnel::handshake::{
    AdmissionDecision, AdmissionFuture, AdmissionPolicy, AuthenticatedRejection,
    ClientHandshakeConfig, HandshakeError, HandshakeResult, HandshakeStage, PROBE_PERIOD,
    ProtocolViolation, ServerHandshakeConfig, VerifiedPeer, client_handshake, server_handshake,
};
use rsetup_tunnel::identity::IdentityKey;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::task::JoinHandle;

// ============================================================ 夹具

fn identity() -> Arc<IdentityKey> {
    let doc = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("generate pkcs8");
    Arc::new(IdentityKey::from_pkcs8(doc.as_ref()).expect("from_pkcs8"))
}

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor::new("SN-TEST-0001", "MODEL-X", "1.2.3", "unit-test")
}

fn pipe() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(64 * 1024)
}

type HandshakeOutcome = Result<HandshakeResult<DuplexStream>, HandshakeError>;

/// 按脚本给出准入决定，并记录调用次数与看到的 peer。
struct ScriptPolicy {
    calls: AtomicUsize,
    script: Mutex<VecDeque<AdmissionDecision>>,
    seen: Mutex<Vec<VerifiedPeer>>,
    tail: AdmissionDecision,
    fail_at: Vec<usize>,
}

impl ScriptPolicy {
    fn new(script: Vec<AdmissionDecision>) -> Self {
        let tail = script
            .last()
            .copied()
            .unwrap_or(AdmissionDecision::Approved);
        Self {
            calls: AtomicUsize::new(0),
            script: Mutex::new(script.into()),
            seen: Mutex::new(Vec::new()),
            tail,
            fail_at: Vec::new(),
        }
    }
    fn failing(mut self, at: usize) -> Self {
        self.fail_at.push(at);
        self
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn peers(&self) -> Vec<VerifiedPeer> {
        self.seen.lock().expect("lock").clone()
    }
}

impl AdmissionPolicy for ScriptPolicy {
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let decision = self
            .script
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or(self.tail);
        self.seen.lock().expect("lock").push(peer.clone());
        let fail = self.fail_at.contains(&n);
        Box::pin(async move {
            if fail {
                Err(HandshakeError::Policy)
            } else {
                Ok(decision)
            }
        })
    }
}

/// 首次调用必须 Pending（进入挂起的判定），翻牌后返回 Approved。
struct FlipPolicy {
    flip: AtomicBool,
    calls: AtomicUsize,
}

impl FlipPolicy {
    fn new() -> Self {
        Self {
            flip: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }
    fn approve(&self) {
        self.flip.store(true, Ordering::SeqCst);
    }
}

impl AdmissionPolicy for FlipPolicy {
    fn evaluate<'a>(&'a self, _peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let approved = n > 0 && self.flip.load(Ordering::SeqCst);
        Box::pin(async move {
            Ok(if approved {
                AdmissionDecision::Approved
            } else {
                AdmissionDecision::Pending
            })
        })
    }
}

/// 永不返回的策略，用来验证 10s 步骤期限包含准入工作。
struct HangingPolicy;

impl AdmissionPolicy for HangingPolicy {
    fn evaluate<'a>(&'a self, _peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(AdmissionDecision::Approved)
        })
    }
}

async fn run_client(
    io: DuplexStream,
    client_id: Arc<IdentityKey>,
    trusted: Vec<[u8; 32]>,
    desc: DeviceDescriptor,
) -> HandshakeOutcome {
    let config = ClientHandshakeConfig {
        identity: client_id,
        trusted_servers: trusted,
        descriptor: desc,
    };
    client_handshake(io, &config).await
}

fn spawn_server<P: AdmissionPolicy + Send + Sync + 'static>(
    io: DuplexStream,
    server_id: Arc<IdentityKey>,
    policy: Arc<P>,
) -> JoinHandle<HandshakeOutcome> {
    tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: server_id,
        };
        server_handshake(io, &config, policy.as_ref()).await
    })
}

fn spawn_client(
    io: DuplexStream,
    client_id: Arc<IdentityKey>,
    trusted: Vec<[u8; 32]>,
    desc: DeviceDescriptor,
) -> JoinHandle<HandshakeOutcome> {
    tokio::spawn(async move {
        let config = ClientHandshakeConfig {
            identity: client_id,
            trusted_servers: trusted,
            descriptor: desc,
        };
        client_handshake(io, &config).await
    })
}

// ============================================================ raw 帧工具

async fn read_frame(io: &mut DuplexStream) -> (FrameType, HandshakeMessage) {
    let mut header = [0u8; FRAME_HEADER_LEN];
    io.read_exact(&mut header).await.expect("read header");
    let len = u32::from_be_bytes(header[4..8].try_into().expect("be32")) as usize;
    assert!(len <= 16_384, "超限帧声明");
    let mut buf = vec![0u8; FRAME_HEADER_LEN + len];
    buf[..FRAME_HEADER_LEN].copy_from_slice(&header);
    io.read_exact(&mut buf[FRAME_HEADER_LEN..])
        .await
        .expect("read payload");
    decode_handshake_frame(&buf).expect("strict decode")
}

async fn write_frame(io: &mut DuplexStream, message: &HandshakeMessage) {
    let frame_type = match message {
        HandshakeMessage::ClientHello(_) => FrameType::ClientHello,
        HandshakeMessage::ServerChallenge(_) => FrameType::ServerChallenge,
        HandshakeMessage::ClientAuthRequest(_) => FrameType::ClientAuthRequest,
        HandshakeMessage::ServerPending(_) => FrameType::ServerPending,
        HandshakeMessage::PendingPong(_) => FrameType::PendingPong,
        HandshakeMessage::ServerAuthResponse(_) => FrameType::ServerAuthResponse,
    };
    let bytes = encode_handshake_frame(frame_type, message).expect("encode frame");
    io.write_all(&bytes).await.expect("write frame");
    io.flush().await.expect("flush");
}

/// 对端必须已关闭连接，且没有回出任何字节。
async fn expect_closed(io: &mut DuplexStream) {
    let mut buf = [0u8; 32];
    let n = io.read(&mut buf).await.expect("read after close");
    assert_eq!(n, 0, "失败路径必须直接关闭 TCP 且不得回字节");
}

fn client_sign(
    id: &IdentityKey,
    client_random: &[u8; 32],
    server_random: &[u8; 32],
    eph: &[u8; 32],
    descriptor_raw: &[u8],
) -> [u8; 64] {
    let input = client_signature_input(
        client_random,
        server_random,
        &id.public_key(),
        eph,
        descriptor_raw,
    )
    .expect("client sig input");
    id.sign(&input)
}

fn server_sign(
    id: &IdentityKey,
    ctx: &RawServerCtx,
    server_eph: &[u8; 32],
    status: u8,
    reason: u32,
) -> [u8; 64] {
    let input = server_signature_input(
        &ctx.server_random,
        &ctx.client_random,
        &ctx.client_pub,
        &ctx.client_eph,
        &ctx.descriptor_raw,
        server_eph,
        status,
        reason,
    )
    .expect("server sig input");
    id.sign(&input)
}

// ------------------------------------------------------------ raw 客户端

/// raw 客户端走到 CLIENT_AUTH_REQUEST，返回继续握手所需上下文。
/// 固定随机值：便于断言「真实实现必须每次新建随机」。
async fn raw_client_up_to_auth(
    io: &mut DuplexStream,
    client_id: &IdentityKey,
    desc: &DeviceDescriptor,
    break_signature: bool,
) -> [u8; 32] {
    write_frame(
        io,
        &HandshakeMessage::ClientHello(ClientHello::new([0x11u8; 32])),
    )
    .await;
    let (ft, msg) = read_frame(io).await;
    assert_eq!(ft, FrameType::ServerChallenge);
    let server_random = match msg {
        HandshakeMessage::ServerChallenge(c) => c.server_random,
        _ => panic!("期望 SERVER_CHALLENGE"),
    };

    let client_eph = [0x22u8; 32];
    let descriptor_raw = desc.encode();
    let mut signature = client_sign(
        client_id,
        &[0x11u8; 32],
        &server_random,
        &client_eph,
        &descriptor_raw,
    );
    if break_signature {
        signature[0] ^= 0xff;
    }
    let request =
        ClientAuthRequest::new(client_id.public_key(), client_eph, desc.clone(), signature)
            .expect("build ClientAuthRequest");
    write_frame(io, &HandshakeMessage::ClientAuthRequest(request)).await;
    server_random
}

// ------------------------------------------------------------ raw 服务端

struct RawServerCtx {
    client_random: [u8; 32],
    server_random: [u8; 32],
    client_pub: [u8; 32],
    client_eph: [u8; 32],
    descriptor_raw: Vec<u8>,
    client_signature: [u8; 64],
}

/// raw 服务端：HELLO→CHALLENGE→AUTH 后返回上下文，供后续注入各种响应。
async fn raw_server_up_to_auth(io: &mut DuplexStream, salt: u8) -> RawServerCtx {
    let (ft, msg) = read_frame(io).await;
    assert_eq!(ft, FrameType::ClientHello);
    let client_random = match msg {
        HandshakeMessage::ClientHello(h) => h.client_random,
        _ => panic!("期望 CLIENT_HELLO"),
    };
    let server_random = {
        let mut b = [0u8; 32];
        b[0] = salt;
        b[1] = 0x5c;
        b
    };
    write_frame(
        io,
        &HandshakeMessage::ServerChallenge(ServerChallenge::new(server_random)),
    )
    .await;

    let (ft, msg) = read_frame(io).await;
    assert_eq!(ft, FrameType::ClientAuthRequest);
    match msg {
        HandshakeMessage::ClientAuthRequest(req) => RawServerCtx {
            client_random,
            server_random,
            client_pub: *req.client_ed25519_pub(),
            client_eph: *req.client_x25519_eph_pub(),
            descriptor_raw: req.descriptor_raw().to_vec(),
            client_signature: *req.signature(),
        },
        _ => panic!("期望 CLIENT_AUTH_REQUEST"),
    }
}

/// raw 服务端发出合法 APPROVED（真实 X25519 临时公钥 + 真实中控签名）。
async fn raw_server_approve(io: &mut DuplexStream, server_id: &IdentityKey, ctx: &RawServerCtx) {
    let eph = EphemeralKeyPair::generate().expect("eph");
    let public = eph.public_key();
    let signature = server_sign(server_id, ctx, &public, 0, 0);
    let response = ServerAuthResponse::approved(public, signature).expect("approved");
    write_frame(io, &HandshakeMessage::ServerAuthResponse(response)).await;
}

async fn raw_server_reject(
    io: &mut DuplexStream,
    server_id: &IdentityKey,
    ctx: &RawServerCtx,
    signed_reason: u32,
    wire_reason: u32,
) {
    let zeros = [0u8; 32];
    let signature = server_sign(server_id, ctx, &zeros, 1, signed_reason);
    let response = ServerAuthResponse::rejected(wire_reason, signature).expect("rejected");
    write_frame(io, &HandshakeMessage::ServerAuthResponse(response)).await;
}

// ============================================================ 1. 批准路径

#[tokio::test]
async fn approved_handshake_produces_mutually_decryptable_session_keys() {
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let desc = descriptor();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]));

    let client = spawn_client(
        client_io,
        Arc::clone(&client_id),
        vec![server_id.public_key()],
        desc.clone(),
    );
    let server = spawn_server(server_io, Arc::clone(&server_id), Arc::clone(&policy));

    let client_result = client.await.expect("task").expect("客户端握手应成功");
    let server_result = server.await.expect("task").expect("服务端握手应成功");

    assert_eq!(client_result.peer_public_key, server_id.public_key());
    assert_eq!(server_result.peer_public_key, client_id.public_key());
    assert_eq!(server_result.descriptor, Some(desc));
    assert_eq!(client_result.descriptor, None);

    let mut client_layers = client_result.session_keys.into_client_record_layers();
    let mut server_layers = server_result.session_keys.into_server_record_layers();

    let msg = b"handshake-bound session keys";
    let sealed = client_layers.tx.seal(msg).expect("c2s seal");
    assert_eq!(server_layers.rx.open(&sealed).expect("c2s open"), msg);

    let reply = b"server to client";
    let sealed = server_layers.tx.seal(reply).expect("s2c seal");
    assert_eq!(client_layers.rx.open(&sealed).expect("s2c open"), reply);
}

#[tokio::test]
async fn approval_is_rechecked_against_policy_before_the_final_response() {
    // 计划：最终批准前必须重新核验策略（撤销优先）。
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]));

    let client = spawn_client(
        client_io,
        client_id,
        vec![server_id.public_key()],
        descriptor(),
    );
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    let _client_result = client.await.expect("task").expect("握手成功");
    let _server_result = server.await.expect("task").expect("握手成功");
    assert_eq!(policy.calls(), 2, "最终批准前必须重新核验一次准入策略");
}

#[tokio::test]
async fn peer_public_key_reported_must_be_the_matched_trusted_key() {
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let other = identity();

    let client = spawn_client(
        client_io,
        client_id,
        vec![other.public_key(), server_id.public_key()],
        descriptor(),
    );
    let server = spawn_server(
        server_io,
        server_id.clone(),
        Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved])),
    );
    let result = client.await.expect("task").expect("列表内可信公钥必须放行");
    assert_eq!(result.peer_public_key, server_id.public_key());
    let _ = server.await;
}

// ============================================================ 2. 信任与签名绑定

#[tokio::test]
async fn client_rejects_server_key_not_in_trusted_list() {
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let other = identity();

    let client = spawn_client(client_io, client_id, vec![other.public_key()], descriptor());
    let server = spawn_server(
        server_io,
        server_id,
        Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved])),
    );

    let err = client
        .await
        .expect("task")
        .expect_err("必须拒绝不受信服务端");
    assert!(matches!(err, HandshakeError::UntrustedServer), "{err:?}");
    assert!(!err.stops_auto_reconnect(), "不受信不得停止重连");
    let _ = server.await;
}

#[tokio::test]
async fn empty_trusted_list_is_refused_without_any_io() {
    // 无 TOFU：没有可信中控公钥时必须直接失败，不得先发握手帧。
    let (mut client_io, mut server_io) = pipe();
    let config = ClientHandshakeConfig {
        identity: identity(),
        trusted_servers: Vec::new(),
        descriptor: descriptor(),
    };

    let err = client_handshake(&mut client_io, &config)
        .await
        .expect_err("空可信列表必须失败");
    assert!(matches!(err, HandshakeError::NoTrustedServers), "{err:?}");

    // 关闭本端后对端读到的必须是 EOF（此前未写出任何字节）。
    drop(client_io);
    let mut buf = [0u8; 8];
    assert_eq!(
        server_io.read(&mut buf).await.expect("read"),
        0,
        "不得向网络发出任何字节"
    );
}

#[tokio::test]
async fn bad_client_signature_closes_connection_without_calling_policy() {
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]));
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), true).await;

    let err = server.await.expect("task").expect_err("坏签名必须失败");
    assert!(
        matches!(err, HandshakeError::ClientSignature),
        "实际错误: {err:?}"
    );
    assert_eq!(policy.calls(), 0, "验签失败前绝不得调用准入策略");
    expect_closed(&mut client_io).await;
}

#[tokio::test]
async fn tampered_reason_code_breaks_the_signature_binding() {
    // 签名覆盖 reason=2 而消息写 reason=3：必须报验签失败，且不得当作可信拒绝。
    let (client_io, mut server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let client = spawn_client(
        client_io,
        client_id,
        vec![server_id.public_key()],
        descriptor(),
    );

    let ctx = raw_server_up_to_auth(&mut server_io, 0x01).await;
    raw_server_reject(&mut server_io, &server_id, &ctx, 2, 3).await;

    let err = client.await.expect("task").expect_err("必须拒绝");
    assert!(matches!(err, HandshakeError::UntrustedServer), "{err:?}");
    assert!(
        !err.stops_auto_reconnect(),
        "未认证 reason 绝不能停止自动重连"
    );
}

#[tokio::test]
async fn unsigned_rejection_never_becomes_a_permanent_rejection() {
    // 中间人可以伪造未签名挂起/拒绝，但不得据此让板端停止重连。
    for reason in [2u32, 3] {
        let (client_io, mut server_io) = pipe();
        let client_id = identity();
        let server_id = identity();
        let impostor = identity();
        let client = spawn_client(
            client_io,
            client_id,
            vec![server_id.public_key()],
            descriptor(),
        );

        let ctx = raw_server_up_to_auth(&mut server_io, 0x02).await;
        // 用冒充者私钥签名：不在可信列表内。
        raw_server_reject(&mut server_io, &impostor, &ctx, reason, reason).await;

        let err = client.await.expect("task").expect_err("必须拒绝");
        assert!(matches!(err, HandshakeError::UntrustedServer), "{err:?}");
        assert!(!err.stops_auto_reconnect());
    }
}

#[tokio::test]
async fn old_version_frame_is_rejected_without_reply() {
    let (mut client_io, server_io) = pipe();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]));
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    let payload = ClientHello::new([0x01u8; 32])
        .encode()
        .expect("encode hello");
    let mut bytes = vec![
        FRAME_MAGIC[0],
        FRAME_MAGIC[1],
        0x02,
        FrameType::ClientHello as u8,
    ];
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&payload);
    client_io.write_all(&bytes).await.expect("write v2");
    client_io.flush().await.expect("flush");

    let err = server.await.expect("task").expect_err("v2 必须拒绝");
    assert!(
        matches!(err, HandshakeError::Protocol(ProtocolViolation::Frame(_))),
        "实际错误: {err:?}"
    );
    assert_eq!(policy.calls(), 0);
    expect_closed(&mut client_io).await;
}

// ============================================================ 3. 严格审计先于副作用

#[tokio::test]
async fn non_canonical_descriptor_bytes_are_signed_verbatim() {
    // §4.2：验签只能用帧内 descriptor 原字节。这里发送「字段乱序」的嵌套
    // DeviceDescriptor 原字节并对其签名；若服务端重编码验签就必然失败。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]));
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    let client_random = [0x41u8; 32];
    write_frame(
        &mut client_io,
        &HandshakeMessage::ClientHello(ClientHello::new(client_random)),
    )
    .await;
    let (_, msg) = read_frame(&mut client_io).await;
    let server_random = match msg {
        HandshakeMessage::ServerChallenge(c) => c.server_random,
        _ => panic!("期望 SERVER_CHALLENGE"),
    };

    let mut raw = Vec::new();
    write_string_field(&mut raw, 4, "extra");
    write_string_field(&mut raw, 1, "SN-WEIRD");
    write_string_field(&mut raw, 2, "MODEL-Y");
    write_string_field(&mut raw, 3, "0.0.1");

    let client_eph = [0x52u8; 32];
    let signature = client_sign(
        &client_id,
        &client_random,
        &server_random,
        &client_eph,
        &raw,
    );
    let request = ClientAuthRequest::new(
        client_id.public_key(),
        client_eph,
        DeviceDescriptor::new("SN-WEIRD", "MODEL-Y", "0.0.1", "extra"),
        signature,
    )
    .expect("build request")
    .with_descriptor_raw(&raw)
    .expect("乱序原字节必须被保留");
    write_frame(
        &mut client_io,
        &HandshakeMessage::ClientAuthRequest(request),
    )
    .await;

    let (ft, msg) = read_frame(&mut client_io).await;
    assert_eq!(ft, FrameType::ServerAuthResponse);
    match msg {
        HandshakeMessage::ServerAuthResponse(resp) => assert_eq!(resp.status, 0),
        _ => panic!("期望 SERVER_AUTH_RESPONSE"),
    }
    assert_eq!(
        policy.peers()[0].descriptor,
        DeviceDescriptor::new("SN-WEIRD", "MODEL-Y", "0.0.1", "extra"),
        "策略必须看到解析后的 descriptor，而验签用的是原字节"
    );
    let _ = server.await;
}

fn write_string_field(buf: &mut Vec<u8>, field: u32, value: &str) {
    buf.push(((field << 3) | 2) as u8);
    buf.push(value.len() as u8);
    buf.extend_from_slice(value.as_bytes());
}

#[tokio::test]
async fn out_of_order_frame_closes_connection_before_any_policy_call() {
    let (mut client_io, server_io) = pipe();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]));
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    // 第一步就发 CLIENT_AUTH_REQUEST。
    let request = ClientAuthRequest::new([0u8; 32], [0u8; 32], descriptor(), [0u8; 64]).expect("b");
    write_frame(
        &mut client_io,
        &HandshakeMessage::ClientAuthRequest(request),
    )
    .await;

    let err = server.await.expect("task").expect_err("乱序必须失败");
    assert!(matches!(err, HandshakeError::Protocol(_)), "{err:?}");
    assert_eq!(policy.calls(), 0, "审计失败不得触发准入流程");
    expect_closed(&mut client_io).await;
}

#[tokio::test]
async fn oversized_frame_declaration_closes_connection_without_allocating() {
    let (mut client_io, server_io) = pipe();
    let server_id = identity();
    let server = spawn_server(
        server_io,
        server_id,
        Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved])),
    );

    let mut header = [0u8; FRAME_HEADER_LEN];
    header[..2].copy_from_slice(&FRAME_MAGIC);
    header[2] = FRAME_VERSION;
    header[3] = FrameType::ClientHello as u8;
    header[4..8].copy_from_slice(&(16_385u32).to_be_bytes());
    client_io.write_all(&header).await.expect("write header");
    client_io.flush().await.expect("flush");

    let err = server.await.expect("task").expect_err("超限必须拒绝");
    assert!(matches!(err, HandshakeError::Protocol(_)), "{err:?}");
    expect_closed(&mut client_io).await;
}

#[tokio::test]
async fn trailing_bytes_after_handshake_message_are_rejected() {
    let (mut client_io, server_io) = pipe();
    let server_id = identity();
    let server = spawn_server(
        server_io,
        server_id,
        Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved])),
    );

    let mut frame = encode_handshake_frame(
        FrameType::ClientHello,
        &HandshakeMessage::ClientHello(ClientHello::new([0x09u8; 32])),
    )
    .expect("encode");
    // 篡改 Payload Length 使其多吞一个尾随字节。
    let declared = u32::from_be_bytes(frame[4..8].try_into().expect("be32")) + 1;
    frame[4..8].copy_from_slice(&declared.to_be_bytes());
    frame.push(0x00);
    client_io.write_all(&frame).await.expect("write");
    client_io.flush().await.expect("flush");

    let err = server.await.expect("task").expect_err("尾随字节必须拒绝");
    assert!(matches!(err, HandshakeError::Protocol(_)), "{err:?}");
    expect_closed(&mut client_io).await;
}

// ============================================================ 4. reason 语义

#[tokio::test]
async fn signed_rejection_reasons_drive_the_reconnect_decision() {
    for (reason, expected, stops) in [
        (2u32, AuthenticatedRejection::Revoked, true),
        (3, AuthenticatedRejection::ApprovalDenied, true),
        (5, AuthenticatedRejection::ServerError, false),
        (0, AuthenticatedRejection::Unspecified, false),
        (4, AuthenticatedRejection::HandshakeTimeout, false),
    ] {
        let (client_io, mut server_io) = pipe();
        let client_id = identity();
        let server_id = identity();
        let client = spawn_client(
            client_io,
            client_id,
            vec![server_id.public_key()],
            descriptor(),
        );

        let ctx = raw_server_up_to_auth(&mut server_io, 0x03).await;
        raw_server_reject(&mut server_io, &server_id, &ctx, reason, reason).await;

        let err = client.await.expect("task").expect_err("REJECTED 必须失败");
        match err {
            HandshakeError::SignedRejection { reason: got } => assert_eq!(got, expected),
            other => panic!("期望 SignedRejection，实际 {other:?}"),
        }
        assert_eq!(
            err.stops_auto_reconnect(),
            stops,
            "reason={reason} 语义错误"
        );
    }
}

#[tokio::test]
async fn server_sends_real_signed_rejection_from_policy() {
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Rejected(2)]));
    let server = spawn_server(server_io, Arc::clone(&server_id), Arc::clone(&policy));

    let server_random =
        raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let (ft, msg) = read_frame(&mut client_io).await;
    assert_eq!(ft, FrameType::ServerAuthResponse);
    let response = match msg {
        HandshakeMessage::ServerAuthResponse(r) => r,
        _ => panic!("期望 SERVER_AUTH_RESPONSE"),
    };
    assert_eq!(response.status, 1);
    assert_eq!(response.reason_code, 2);
    assert_eq!(
        response.server_x25519_eph_pub, [0u8; 32],
        "REJECTED 必须全零占位"
    );

    let input = server_signature_input(
        &server_random,
        &[0x11u8; 32],
        &client_id.public_key(),
        &[0x22u8; 32],
        &descriptor().encode(),
        &response.server_x25519_eph_pub,
        1,
        2,
    )
    .expect("sig input");
    rsetup_protocol::ed25519::verify_ed25519(&input, &response.signature, &server_id.public_key())
        .expect("拒绝必须由中控长期私钥签名并覆盖 reason_code");

    let err = server.await.expect("task").expect_err("服务端应报签名拒绝");
    assert!(
        matches!(
            err,
            HandshakeError::SignedRejection {
                reason: AuthenticatedRejection::Revoked
            }
        ),
        "{err:?}"
    );
    assert!(err.stops_auto_reconnect());
}

#[tokio::test]
async fn policy_error_is_propagated_and_connection_closes() {
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]).failing(0));
    let server = spawn_server(server_io, server_id, policy);

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let err = server.await.expect("task").expect_err("策略失败必须传播");
    assert!(matches!(err, HandshakeError::Policy), "{err:?}");
    expect_closed(&mut client_io).await;
}

#[tokio::test]
async fn illegal_reason_is_never_sent() {
    // §4.3：v3 绝不发出 reason=1（SIGNATURE_INVALID）。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Rejected(1)]));
    let server = spawn_server(server_io, server_id, policy);

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let err = server
        .await
        .expect("task")
        .expect_err("非法 reason 必须失败");
    assert!(matches!(err, HandshakeError::Policy), "{err:?}");
    expect_closed(&mut client_io).await;
}

// ============================================================ 5. PENDING 时序

#[tokio::test(start_paused = true)]
async fn first_pending_is_immediate_then_probe_period_is_5s() {
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Pending]));
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    let sent_at = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;

    let mut nonce_first = None;
    let mut last_token: Option<[u8; 16]> = None;
    let mut prev: Option<tokio::time::Instant> = None;
    for expected_seq in 1u64..=3 {
        let (ft, msg) = read_frame(&mut client_io).await;
        let now = tokio::time::Instant::now();
        assert_eq!(ft, FrameType::ServerPending);
        let pending = match msg {
            HandshakeMessage::ServerPending(p) => p,
            _ => panic!("期望 SERVER_PENDING"),
        };
        assert_eq!(pending.probe_seq, expected_seq);
        assert_eq!(pending.wait_hint_ms, 5_000);
        if let Some(first) = nonce_first {
            assert_eq!(first, pending.status_nonce, "status_nonce 挂起期内不得变化");
        } else {
            nonce_first = Some(pending.status_nonce);
            assert!(
                now.saturating_duration_since(sent_at) < Duration::from_millis(50),
                "首个 SERVER_PENDING 必须立即发送，实测 {:?}",
                now.saturating_duration_since(sent_at)
            );
        }
        if let Some(previous) = prev {
            assert_eq!(
                now.saturating_duration_since(previous),
                Duration::from_secs(5),
                "probe 名义周期必须 5s"
            );
        }
        prev = Some(now);

        // 每轮 token 必须重新随机。
        if let Some(previous) = last_token {
            assert_ne!(
                pending.pending_token, previous,
                "pending_token 必须每轮重新生成"
            );
        }
        last_token = Some(pending.pending_token);

        write_frame(
            &mut client_io,
            &HandshakeMessage::PendingPong(
                PendingPong::new(
                    pending.pending_token,
                    pending.status_nonce,
                    pending.probe_seq,
                )
                .expect("pong"),
            ),
        )
        .await;
    }

    drop(client_io);
    let err = server.await.expect("task").expect_err("对端关闭后必须失败");
    assert!(matches!(err, HandshakeError::Io(_)), "{err:?}");
}

#[tokio::test(start_paused = true)]
async fn pending_without_pong_times_out_at_10s() {
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Pending]));
    let server = spawn_server(server_io, server_id, policy);

    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let (ft, _) = read_frame(&mut client_io).await;
    assert_eq!(ft, FrameType::ServerPending);

    // 故意不回 pong。
    let err = server.await.expect("task").expect_err("pong 超时必须失败");
    assert!(
        matches!(err, HandshakeError::Timeout(HandshakeStage::PendingPong)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(10),
        "pong 期限必须 10s"
    );
}

#[tokio::test(start_paused = true)]
async fn pending_has_no_total_deadline_and_approval_still_works() {
    // 20+ 轮 probe（>100s 虚拟时间）后批准仍必须生效。
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let flip = Arc::new(FlipPolicy::new());
    let flip_task = Arc::clone(&flip);
    let server_id_for_check = Arc::clone(&server_id);
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id_for_check),
        };
        server_handshake(server_io, &config, flip_task.as_ref()).await
    });

    let started = tokio::time::Instant::now();
    let client = spawn_client(
        client_io,
        Arc::clone(&client_id),
        vec![server_id.public_key()],
        descriptor(),
    );

    tokio::time::sleep(Duration::from_secs(20 * 5)).await;
    flip.approve();

    let result = client.await.expect("task").expect("长时间挂起后必须批准");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(100),
        "挂起必须真的持续 100s 以上，实测 {elapsed:?}"
    );
    let server_result = server.await.expect("task").expect("服务端批准");
    assert_eq!(server_result.peer_public_key, client_id.public_key());

    let mut client_layers = result.session_keys.into_client_record_layers();
    let mut server_layers = server_result.session_keys.into_server_record_layers();
    let sealed = client_layers.tx.seal(b"after long pending").expect("seal");
    assert_eq!(
        server_layers.rx.open(&sealed).expect("open"),
        b"after long pending"
    );
}

#[tokio::test(start_paused = true)]
async fn approval_finalizes_only_after_settling_the_inflight_probe() {
    // 批准与在途 probe 竞争：结清当前 probe 后直接发最终响应，不得再发新 probe。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let flip = Arc::new(FlipPolicy::new());
    let flip_task = Arc::clone(&flip);
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id),
        };
        server_handshake(server_io, &config, flip_task.as_ref()).await
    });

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let (ft, msg) = read_frame(&mut client_io).await;
    assert_eq!(ft, FrameType::ServerPending);
    let pending = match msg {
        HandshakeMessage::ServerPending(p) => p,
        _ => panic!("期望 SERVER_PENDING"),
    };

    // 在途 probe 期间翻牌批准。
    flip.approve();
    let pong_at = tokio::time::Instant::now();
    write_frame(
        &mut client_io,
        &HandshakeMessage::PendingPong(
            PendingPong::new(
                pending.pending_token,
                pending.status_nonce,
                pending.probe_seq,
            )
            .expect("pong"),
        ),
    )
    .await;

    let (ft, msg) = read_frame(&mut client_io).await;
    // §4.1：决定已持久化后结清当前 probe 即发最终响应，不被 5s 周期推迟。
    assert!(
        pong_at.elapsed() < PROBE_PERIOD,
        "最终响应不得等满 5s probe 周期，实测 {:?}",
        pong_at.elapsed()
    );
    assert_eq!(
        ft,
        FrameType::ServerAuthResponse,
        "结清在途 probe 后必须直接发最终响应"
    );
    match msg {
        HandshakeMessage::ServerAuthResponse(r) => assert_eq!(r.status, 0),
        _ => panic!("期望 SERVER_AUTH_RESPONSE"),
    }
    let result = server.await.expect("task").expect("批准");
    assert_eq!(result.peer_public_key, client_id.public_key());
}

#[tokio::test(start_paused = true)]
async fn revocation_wins_over_a_stale_pending_approval() {
    // 挂起期间被吊销：最终必须是签名 REJECTED(2)。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![
        AdmissionDecision::Pending,
        AdmissionDecision::Approved,
        AdmissionDecision::Rejected(2),
    ]));
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let (ft, msg) = read_frame(&mut client_io).await;
    assert_eq!(ft, FrameType::ServerPending);
    let pending = match msg {
        HandshakeMessage::ServerPending(p) => p,
        _ => panic!("期望 SERVER_PENDING"),
    };
    write_frame(
        &mut client_io,
        &HandshakeMessage::PendingPong(
            PendingPong::new(
                pending.pending_token,
                pending.status_nonce,
                pending.probe_seq,
            )
            .expect("pong"),
        ),
    )
    .await;

    let (ft, msg) = read_frame(&mut client_io).await;
    assert_eq!(ft, FrameType::ServerAuthResponse);
    let response = match msg {
        HandshakeMessage::ServerAuthResponse(r) => r,
        _ => panic!("期望 SERVER_AUTH_RESPONSE"),
    };
    assert_eq!(
        (response.status, response.reason_code),
        (1, 2),
        "撤销必须优先"
    );

    let err = server.await.expect("task").expect_err("应报签名拒绝");
    assert!(
        matches!(
            err,
            HandshakeError::SignedRejection {
                reason: AuthenticatedRejection::Revoked
            }
        ),
        "{err:?}"
    );
    assert!(err.stops_auto_reconnect());
}

#[tokio::test(start_paused = true)]
async fn pending_pong_field_mismatches_close_the_connection() {
    for field in ["token", "nonce", "seq"] {
        let (mut client_io, server_io) = pipe();
        let client_id = identity();
        let server_id = identity();
        let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Pending]));
        let server = spawn_server(server_io, server_id, policy);

        raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
        let (ft, msg) = read_frame(&mut client_io).await;
        assert_eq!(ft, FrameType::ServerPending);
        let pending = match msg {
            HandshakeMessage::ServerPending(p) => p,
            _ => panic!("期望 SERVER_PENDING"),
        };

        let (token, nonce, seq) = match field {
            "token" => ([0xeeu8; 16], pending.status_nonce, pending.probe_seq),
            "nonce" => (pending.pending_token, [0xddu8; 16], pending.probe_seq),
            _ => (
                pending.pending_token,
                pending.status_nonce,
                pending.probe_seq + 1,
            ),
        };
        write_frame(
            &mut client_io,
            &HandshakeMessage::PendingPong(PendingPong::new(token, nonce, seq).expect("pong")),
        )
        .await;

        let err = server
            .await
            .expect("task")
            .expect_err(&format!("{field} 失配必须失败"));
        assert!(
            matches!(err, HandshakeError::Protocol(ProtocolViolation::Pending(_))),
            "[{field}] 实际错误: {err:?}"
        );
        expect_closed(&mut client_io).await;
    }
}

#[tokio::test(start_paused = true)]
async fn client_goes_silent_after_15s_without_new_pending() {
    let (client_io, mut server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let client = spawn_client(
        client_io,
        client_id,
        vec![server_id.public_key()],
        descriptor(),
    );

    let started = tokio::time::Instant::now();
    let _ctx = raw_server_up_to_auth(&mut server_io, 0x04).await;
    let nonce = [0x77u8; 16];
    write_frame(
        &mut server_io,
        &HandshakeMessage::ServerPending(
            ServerPending::new([0x01u8; 16], nonce, 1).expect("pending"),
        ),
    )
    .await;
    let (ft, msg) = read_frame(&mut server_io).await;
    assert_eq!(ft, FrameType::PendingPong, "客户端必须立即回送 pong");
    match msg {
        HandshakeMessage::PendingPong(pong) => {
            assert_eq!(pong.status_nonce, nonce);
            assert_eq!(pong.pending_token, [0x01u8; 16], "pong 必须逐字回显 token");
            assert_eq!(pong.probe_seq, 1);
        }
        _ => panic!("期望 PENDING_PONG"),
    }

    let err = client.await.expect("task").expect_err("静默必须超时");
    assert!(
        matches!(err, HandshakeError::Timeout(HandshakeStage::ClientSilence)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(15),
        "静默期限必须 15s"
    );
}

#[tokio::test(start_paused = true)]
async fn client_rejects_repeated_probe_seq_and_changed_nonce() {
    // (a) probe_seq 重复；(b) status_nonce 变化。两者都必须断开。
    let cases = [("repeat-seq", true, false), ("changed-nonce", false, true)];
    for (name, repeat_seq, change_nonce) in cases {
        let (client_io, mut server_io) = pipe();
        let client_id = identity();
        let server_id = identity();
        let client = spawn_client(
            client_io,
            client_id,
            vec![server_id.public_key()],
            descriptor(),
        );

        let _ctx = raw_server_up_to_auth(&mut server_io, 0x05).await;
        let nonce = [0x0au8; 16];
        write_frame(
            &mut server_io,
            &HandshakeMessage::ServerPending(
                ServerPending::new([0x01u8; 16], nonce, 1).expect("pending 1"),
            ),
        )
        .await;
        read_frame(&mut server_io).await;

        let second_nonce = if change_nonce { [0x0bu8; 16] } else { nonce };
        let second_seq = if repeat_seq { 1 } else { 2 };
        write_frame(
            &mut server_io,
            &HandshakeMessage::ServerPending(
                ServerPending::new([0x02u8; 16], second_nonce, second_seq).expect("pending 2"),
            ),
        )
        .await;

        let err = client
            .await
            .expect("task")
            .expect_err(&format!("{name} 必须被客户端拒绝"));
        assert!(
            matches!(err, HandshakeError::Protocol(_)),
            "[{name}] 实际错误: {err:?}"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn client_times_out_waiting_for_challenge_at_10s() {
    let (client_io, mut server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let client = spawn_client(
        client_io,
        client_id,
        vec![server_id.public_key()],
        descriptor(),
    );

    // 只读掉 CLIENT_HELLO，之后不回任何东西。
    read_frame(&mut server_io).await;
    let started = tokio::time::Instant::now();
    let err = client.await.expect("task").expect_err("中控沉默必须超时");
    assert!(
        matches!(
            err,
            HandshakeError::Timeout(HandshakeStage::ServerChallenge)
        ),
        "实际错误: {err:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(10),
        "初始步骤期限 10s"
    );
}

#[tokio::test(start_paused = true)]
async fn admission_work_is_covered_by_the_10s_step_deadline() {
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id),
        };
        server_handshake(server_io, &config, &HangingPolicy).await
    });

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let started = tokio::time::Instant::now();
    let err = server.await.expect("task").expect_err("策略挂起必须超时");
    assert!(
        matches!(
            err,
            HandshakeError::Timeout(HandshakeStage::AdmissionDecision)
        ),
        "实际错误: {err:?}"
    );
    assert_eq!(started.elapsed(), Duration::from_secs(10), "步骤期限 10s");
}

// ============================================================ 6. 随机值新鲜度

#[tokio::test]
async fn client_random_and_ephemeral_are_fresh_per_connection() {
    let client_id = identity();
    let server_id = identity();

    let mut randoms = Vec::new();
    let mut ephs = Vec::new();
    let mut signatures = Vec::new();
    for salt in 0..2u8 {
        let (client_io, mut server_io) = pipe();
        let client = spawn_client(
            client_io,
            Arc::clone(&client_id),
            vec![server_id.public_key()],
            descriptor(),
        );
        let ctx = raw_server_up_to_auth(&mut server_io, salt).await;
        randoms.push(ctx.client_random);
        ephs.push(ctx.client_eph);
        signatures.push(ctx.client_signature);
        raw_server_approve(&mut server_io, &server_id, &ctx).await;
        let _result = client.await.expect("task").expect("握手应成功");
    }

    assert_ne!(randoms[0], randoms[1], "client_random 必须每连接新生成");
    assert_ne!(ephs[0], ephs[1], "临时 X25519 公钥必须每连接新生成");
    assert_ne!(signatures[0], signatures[1], "签名必须随新鲜随机变化");
}

#[tokio::test]
async fn server_random_and_ephemeral_are_fresh_per_connection() {
    let client_id = identity();
    let server_id = identity();

    let mut server_randoms = Vec::new();
    let mut server_ephs = Vec::new();
    for _ in 0..2 {
        let (mut client_io, server_io) = pipe();
        let server = spawn_server(
            server_io,
            Arc::clone(&server_id),
            Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved])),
        );
        let server_random =
            raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
        server_randoms.push(server_random);

        let (ft, msg) = read_frame(&mut client_io).await;
        assert_eq!(ft, FrameType::ServerAuthResponse);
        match msg {
            HandshakeMessage::ServerAuthResponse(r) => {
                assert_eq!(r.status, 0);
                server_ephs.push(r.server_x25519_eph_pub);
            }
            _ => panic!("期望 SERVER_AUTH_RESPONSE"),
        }
        let _ = server.await;
    }

    assert_ne!(
        server_randoms[0], server_randoms[1],
        "server_random 必须每连接新生成"
    );
    assert_ne!(
        server_ephs[0], server_ephs[1],
        "服务端临时公钥必须每连接新生成"
    );
}

#[tokio::test]
async fn no_session_keys_are_delivered_when_the_peer_is_untrusted() {
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Approved]));
    let server = spawn_server(server_io, identity(), Arc::clone(&policy));

    let result: HandshakeOutcome = run_client(
        client_io,
        client_id,
        vec![identity().public_key()],
        descriptor(),
    )
    .await;
    match result {
        Ok(_) => panic!("不受信服务端不得成功交付 SessionKeys"),
        Err(err) => assert!(matches!(err, HandshakeError::UntrustedServer), "{err:?}"),
    }
    let _ = server.await;
}

// ============================================================ 7. 写出阻塞夹具

/// 写出阻塞夹具：第 `stall_on_write`（1 起）次**完成**的写出先阻塞 `stall` 虚拟时长。
/// 读方向逐字透传；用来精确复现「probe 提交到有界队列时被背压挡住」与
/// 「pong 写出被挡住」这两类真实排队耗时，不靠真实 sleep。
#[derive(Debug)]
struct StallWrite<S> {
    inner: S,
    stall_on_write: usize,
    stall: Duration,
    writes: usize,
    blocked_until: Option<tokio::time::Instant>,
    armed: bool,
}

impl<S> StallWrite<S> {
    fn new(inner: S, stall_on_write: usize, stall: Duration) -> Self {
        Self {
            inner,
            stall_on_write,
            stall,
            writes: 0,
            blocked_until: None,
            armed: false,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for StallWrite<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for StallWrite<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.blocked_until.is_none() && this.writes + 1 == this.stall_on_write {
            this.blocked_until = Some(tokio::time::Instant::now() + this.stall);
        }
        if let Some(until) = this.blocked_until {
            let now = tokio::time::Instant::now();
            if now < until {
                if !this.armed {
                    this.armed = true;
                    let waker = cx.waker().clone();
                    let remaining = until - now;
                    // 到期唤醒本任务：既不忙等，也不改用真实时间计时。
                    tokio::spawn(async move {
                        tokio::time::sleep(remaining).await;
                        waker.wake();
                    });
                }
                return Poll::Pending;
            }
            this.blocked_until = None;
            this.armed = false;
        }
        match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                this.writes += 1;
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

// ============================================================ 8. 准入挂起策略

/// 首次立即 Pending；之后每次 evaluate 挂起 `hang` 再取最新决定。
/// `flip` 置位后的调用返回 Approved，用来检验「决定就绪 → 停发新 probe → 结清在途 → final」。
struct SlowPendingPolicy {
    hang: Duration,
    /// `Some(n)`：只有第 1..=n 次调用挂起，其后立即返回。用于让「最终接纳前的短临界区
    /// 复查」（§4.1 要求它是快速状态读取）保持快速，同时把前几轮审批查询拖成长挂起。
    hang_calls: Option<usize>,
    flip: AtomicBool,
    calls: AtomicUsize,
}

impl SlowPendingPolicy {
    fn new(hang: Duration) -> Self {
        Self {
            hang,
            hang_calls: None,
            flip: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }
    fn with_hang_calls(hang: Duration, hang_calls: usize) -> Self {
        Self {
            hang,
            hang_calls: Some(hang_calls),
            flip: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        }
    }
    fn approve(&self) {
        self.flip.store(true, Ordering::SeqCst);
    }
}

impl AdmissionPolicy for SlowPendingPolicy {
    fn evaluate<'a>(&'a self, _peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        // 初判立即返回 Pending（首个 SERVER_PENDING 必须立即发），其后再挂起。
        let hang = if n == 0 || self.hang_calls.is_some_and(|limit| n > limit) {
            Duration::ZERO
        } else {
            self.hang
        };
        Box::pin(async move {
            if !hang.is_zero() {
                tokio::time::sleep(hang).await;
            }
            Ok(if self.flip.load(Ordering::SeqCst) {
                AdmissionDecision::Approved
            } else {
                AdmissionDecision::Pending
            })
        })
    }
}

async fn read_pending(io: &mut DuplexStream) -> ServerPending {
    let (ft, msg) = read_frame(io).await;
    assert_eq!(ft, FrameType::ServerPending, "期望 SERVER_PENDING");
    match msg {
        HandshakeMessage::ServerPending(pending) => pending,
        _ => panic!("期望 SERVER_PENDING"),
    }
}

async fn answer_pong(io: &mut DuplexStream, pending: &ServerPending) {
    write_frame(
        io,
        &HandshakeMessage::PendingPong(
            PendingPong::new(
                pending.pending_token,
                pending.status_nonce,
                pending.probe_seq,
            )
            .expect("pong"),
        ),
    )
    .await;
}

// ============================================================ 9. F1：准入挂起期间 probe 不停发

#[tokio::test(start_paused = true)]
async fn probes_keep_firing_while_admission_evaluate_hangs() {
    // F1：准入 evaluate 挂起 20s 期间，名义 5s probe 必须继续按时发出，
    // 且 60s 以后也不存在任何「审批总期限」把连接掐掉。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(SlowPendingPolicy::new(Duration::from_secs(20)));
    let server = spawn_server(server_io, Arc::clone(&server_id), Arc::clone(&policy));

    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;

    for seq in 1u64..=13 {
        let pending =
            tokio::time::timeout(Duration::from_millis(5_200), read_pending(&mut client_io))
                .await
                .unwrap_or_else(|_| {
                    panic!("probe {seq} 未在名义 5s 周期内到达：evaluate 挂起期间 probe 停发")
                });
        assert_eq!(pending.probe_seq, seq, "probe_seq 必须严格递增");
        assert_eq!(
            started.elapsed(),
            PROBE_PERIOD * (seq - 1) as u32,
            "第 {seq} 轮 probe 的发送时刻必须仍是 t+{}s（evaluate 未决定不得拖走 probe 调度）",
            seq - 1
        );
        answer_pong(&mut client_io, &pending).await;
    }

    assert_eq!(
        started.elapsed(),
        Duration::from_secs(60),
        "13 轮探测必须真实持续 60s"
    );
    assert!(
        !server.is_finished(),
        "挂起 60s 后服务端不得自行结束：§4.1 已取消审批总等待期限"
    );
    drop(client_io);
    let _ = server.await;
}

#[tokio::test(start_paused = true)]
async fn decision_after_long_hang_stops_new_probes_and_settles_inflight() {
    // F1：决定就绪后不得再发新 probe；在途 probe 按原期限结清后才发最终响应。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    // 挂起 18s：审批查询在 t=18 返回，落在第 4 轮（t=15）与下一轮名义时刻（t=20）之间。
    let policy = Arc::new(SlowPendingPolicy::with_hang_calls(
        Duration::from_secs(18),
        1,
    ));
    let server = spawn_server(server_io, Arc::clone(&server_id), Arc::clone(&policy));

    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;

    // t=0 首轮；evaluate 正挂起。
    let first = read_pending(&mut client_io).await;
    answer_pong(&mut client_io, &first).await;
    for expected in 2u64..=4 {
        let pending = read_pending(&mut client_io).await;
        assert_eq!(pending.probe_seq, expected);
        assert_eq!(started.elapsed(), PROBE_PERIOD * (expected - 1) as u32);
        answer_pong(&mut client_io, &pending).await;
    }
    // 第 4 轮在 t=15 结清；此后 evaluate 挂起中，probe 调度不得因此停发。
    assert_eq!(started.elapsed(), Duration::from_secs(15), "第 4 轮在 t=15");
    policy.approve();

    let (ft, msg) = tokio::time::timeout(Duration::from_secs(20), read_frame(&mut client_io))
        .await
        .expect("决定就绪后必须发出最终响应");
    assert_eq!(
        ft,
        FrameType::ServerAuthResponse,
        "决定就绪后到达的下一帧必须是最终响应，而不是新一轮 probe"
    );
    match msg {
        HandshakeMessage::ServerAuthResponse(response) => assert_eq!(response.status, 0),
        _ => panic!("期望 SERVER_AUTH_RESPONSE"),
    }
    // 决定在 t=18 就绪，此刻没有在途 probe → 立刻结清并发最终响应；
    // 既不能等到 t=20 再发第 5 轮 probe，也不能拖到下一个周期。
    assert_eq!(started.elapsed(), Duration::from_secs(18));
    let _result = server.await.expect("task").expect("批准应成功");
}

#[tokio::test(start_paused = true)]
async fn real_client_survives_long_admission_hang_without_total_deadline() {
    // F1（真实板端侧）：中控 evaluate 长时间未决定期间，板端不得因 15s 静默误断线。
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    // 前两轮审批查询各挂 20s；其后的临界区复查按 §4.1 属快速状态读取。
    let policy = Arc::new(SlowPendingPolicy::with_hang_calls(
        Duration::from_secs(20),
        2,
    ));
    let policy_task = Arc::clone(&policy);
    let server_id_for_task = Arc::clone(&server_id);
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id_for_task),
        };
        server_handshake(server_io, &config, policy_task.as_ref()).await
    });

    let started = tokio::time::Instant::now();
    let client = spawn_client(
        client_io,
        Arc::clone(&client_id),
        vec![server_id.public_key()],
        descriptor(),
    );

    // 挂起期间跨过近 3 个 15s 静默窗口再批准：t=40 置位，正在挂起的第二轮审批查询
    // 于 t=45 返回，因此批准时刻确定（不与翻转时刻同刻竞态）。
    tokio::time::sleep(Duration::from_secs(40)).await;
    policy.approve();

    let result = tokio::time::timeout(Duration::from_secs(60), client)
        .await
        .expect("批准必须在有限时间内完成，且绝不能被任何总期限提前掐掉")
        .expect("task")
        .expect("evaluate 长时间挂起后板端必须仍能完成握手");
    // 批准不得早于翻转时刻：翻转之前策略始终 Pending，握手也必须真的成功。
    // （结清在途 probe 会立刻重新查询准入状态，因此批准落在翻转之后的哪一刻由策略
    // 的返回时刻决定，不钉死到具体秒。）
    assert!(
        started.elapsed() >= Duration::from_secs(40),
        "批准只应在挂起的 evaluate 返回之后生效，实测 {:?}",
        started.elapsed()
    );
    assert_eq!(result.peer_public_key, server_id.public_key());
    let server_result = server.await.expect("task").expect("服务端握手应成功");
    assert_eq!(server_result.peer_public_key, client_id.public_key());
}

// ============================================================ 10. F2：pong 期限含 probe 写阻塞

#[tokio::test(start_paused = true)]
async fn pong_deadline_includes_the_blocked_probe_write() {
    // F2：10s 从 probe 提交时开始计；写出阻塞 8s 后只剩 2s，不得再新给 10s。
    let (mut client_io, server_io) = pipe();
    let stalled = StallWrite::new(server_io, 2, Duration::from_secs(8));
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Pending]));
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id),
        };
        server_handshake(stalled, &config, policy.as_ref()).await
    });

    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let _pending = read_pending(&mut client_io).await;
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(8),
        "夹具必须真的把 SERVER_PENDING 的写出挡住 8s"
    );

    // 故意不回 pong：总期限仍是「提交 + 10s」，不是「写完 + 10s」。
    let err = server.await.expect("task").expect_err("pong 超时必须失败");
    assert!(
        matches!(err, HandshakeError::Timeout(HandshakeStage::PendingPong)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(10),
        "写阻塞 8s 后 pong 窗口只剩 2s（总期限 10s，不得写完再给 10s）"
    );
}

#[tokio::test(start_paused = true)]
async fn pong_inside_the_remaining_window_is_still_accepted() {
    // F2 正向：写阻塞 8s 后 t=9.5s 的 pong 仍在剩余 2s 窗口内，必须结清并批准。
    let (mut client_io, server_io) = pipe();
    let stalled = StallWrite::new(server_io, 2, Duration::from_secs(8));
    let client_id = identity();
    let server_id = identity();
    let flip = Arc::new(FlipPolicy::new());
    let flip_task = Arc::clone(&flip);
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id),
        };
        server_handshake(stalled, &config, flip_task.as_ref()).await
    });

    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let pending = read_pending(&mut client_io).await;
    flip.approve();
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    answer_pong(&mut client_io, &pending).await;

    let (ft, msg) = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut client_io))
        .await
        .expect("剩余 pong 窗口内到达的合法 pong 必须被结清");
    assert_eq!(ft, FrameType::ServerAuthResponse);
    match msg {
        HandshakeMessage::ServerAuthResponse(response) => assert_eq!(response.status, 0),
        _ => panic!("期望 SERVER_AUTH_RESPONSE"),
    }
    assert_eq!(started.elapsed(), Duration::from_millis(9_500));
    let _result = server.await.expect("task").expect("批准应成功");
}

// ============================================================ 11. F3：pong 写阻塞不刷新静默期限

#[tokio::test(start_paused = true)]
async fn blocked_pong_write_does_not_extend_client_silence() {
    // F3：15s 静默从最后一条合法 SERVER_PENDING 的**到达时刻**起算；
    // pong 写出阻塞 8s 后恢复，静默仍在 t=15s 到期，而不是 t=23s。
    let (client_io, mut server_io) = pipe();
    let stalled = StallWrite::new(client_io, 3, Duration::from_secs(8));
    let client_id = identity();
    let server_id = identity();
    let client = tokio::spawn(async move {
        let config = ClientHandshakeConfig {
            identity: Arc::clone(&client_id),
            trusted_servers: vec![server_id.public_key()],
            descriptor: descriptor(),
        };
        client_handshake(stalled, &config).await
    });

    let _ctx = raw_server_up_to_auth(&mut server_io, 0x06).await;
    let started = tokio::time::Instant::now();
    write_frame(
        &mut server_io,
        &HandshakeMessage::ServerPending(
            ServerPending::new([0x11u8; 16], [0x22u8; 16], 1).expect("pending"),
        ),
    )
    .await;

    let err = client.await.expect("task").expect_err("静默必须超时");
    assert!(
        matches!(err, HandshakeError::Timeout(HandshakeStage::ClientSilence)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(15),
        "静默期限必须锚在最后一条合法帧的到达时刻，写出 pong 不得刷新它"
    );
}

#[tokio::test(start_paused = true)]
async fn new_pending_refreshes_silence_own_pong_does_not() {
    // F3 的另一半：只有新的合法 SERVER_PENDING 能刷新 15s 静默；自己回 pong 不算刷新。
    let (client_io, mut server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let client = spawn_client(
        client_io,
        Arc::clone(&client_id),
        vec![server_id.public_key()],
        descriptor(),
    );

    let _ctx = raw_server_up_to_auth(&mut server_io, 0x07).await;
    let started = tokio::time::Instant::now();
    let nonce = [0x33u8; 16];

    // 第 1 轮：t=0 合法 pending，客户端必须立刻逐字回 pong。
    write_frame(
        &mut server_io,
        &HandshakeMessage::ServerPending(
            ServerPending::new([0x01u8; 16], nonce, 1).expect("pending 1"),
        ),
    )
    .await;
    expect_pong(&mut server_io, [0x01u8; 16], nonce, 1).await;

    // 第 2 轮：t=10 的合法 pending 把静默期限刷新到 t=25。
    tokio::time::sleep(Duration::from_secs(10)).await;
    write_frame(
        &mut server_io,
        &HandshakeMessage::ServerPending(
            ServerPending::new([0x02u8; 16], nonce, 2).expect("pending 2"),
        ),
    )
    .await;
    expect_pong(&mut server_io, [0x02u8; 16], nonce, 2).await;

    // 此后中控静默：必须在 t=25 到期，而不是 t=15，也不是 pong 写出后的 t=25-8。
    let err = client.await.expect("task").expect_err("中控停止后必须超时");
    assert!(
        matches!(err, HandshakeError::Timeout(HandshakeStage::ClientSilence)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(25),
        "t=10 的合法 pending 必须刷新静默期限 → t=25 到期"
    );
}

async fn expect_pong(io: &mut DuplexStream, token: [u8; 16], nonce: [u8; 16], seq: u64) {
    let (ft, msg) = read_frame(io).await;
    assert_eq!(
        ft,
        FrameType::PendingPong,
        "客户端必须立即回送 PENDING_PONG"
    );
    match msg {
        HandshakeMessage::PendingPong(pong) => {
            assert_eq!(pong.pending_token, token, "pong 必须逐字回显 token");
            assert_eq!(pong.status_nonce, nonce, "pong 必须逐字回显 status_nonce");
            assert_eq!(pong.probe_seq, seq, "pong 必须逐字回显 probe_seq");
        }
        _ => panic!("期望 PENDING_PONG"),
    }
}

// ============================================================ 12. round2 R1：无在途旧 pong 不得随批准交付

/// 延迟可读夹具：字节**到达**后延迟 `delay` 虚拟时长才对外可读。
/// 用来确定性地构造「数据与已到期 pong 定时器同刻就绪、biased 读分支先被轮询」，
/// 把「select 分支就绪顺序」与「时间证明」分开验证。
#[derive(Debug)]
struct DelayRead<S> {
    inner: S,
    delay: Duration,
    buf: Vec<u8>,
    release_at: Option<tokio::time::Instant>,
    armed: bool,
}

impl<S> DelayRead<S> {
    fn new(inner: S, delay: Duration) -> Self {
        Self {
            inner,
            delay,
            buf: Vec::new(),
            release_at: None,
            armed: false,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for DelayRead<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        loop {
            if !this.buf.is_empty() {
                let now = tokio::time::Instant::now();
                if let Some(until) = this.release_at
                    && now < until
                {
                    if !this.armed {
                        this.armed = true;
                        let waker = cx.waker().clone();
                        let remaining = until - now;
                        tokio::spawn(async move {
                            tokio::time::sleep(remaining).await;
                            waker.wake();
                        });
                    }
                    // 字节尚未「到达」：对读侧表现为无数据，而不是 0 长度 EOF。
                    return Poll::Pending;
                }
                this.armed = false;
                this.release_at = None;
                let n = buf.remaining().min(this.buf.len());
                let chunk: Vec<u8> = this.buf.drain(..n).collect();
                buf.put_slice(&chunk);
                return Poll::Ready(Ok(()));
            }
            let mut tmp = [0u8; 256];
            let mut rb = ReadBuf::new(&mut tmp);
            match Pin::new(&mut this.inner).poll_read(cx, &mut rb) {
                Poll::Ready(Ok(())) => {
                    let n = rb.filled().len();
                    if n == 0 {
                        return Poll::Ready(Ok(()));
                    }
                    this.buf.extend_from_slice(rb.filled());
                    if this.release_at.is_none() {
                        this.release_at = Some(tokio::time::Instant::now() + this.delay);
                    }
                }
                Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for DelayRead<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[tokio::test(start_paused = true)]
async fn stale_pending_pong_is_never_delivered_with_approved_io() {
    // R1 反例：t=1 无在途 probe 时到达一条旧的重复 pong；策略 t=2 给出 Approved。
    // 只要「无在途就关读」，这条已到达的明文 pong 就会留在连接里，随 Ok 交付给
    // 加密层——本用例断言：不允许批准，且绝不把旧帧带进 io。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    // 初判立即 Pending；之后的审批查询各挂 2s（含最终临界区复查），保证批准落在 t=2。
    let policy = Arc::new(SlowPendingPolicy::new(Duration::from_secs(2)));
    let server = spawn_server(server_io, Arc::clone(&server_id), Arc::clone(&policy));

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let first = read_pending(&mut client_io).await;
    answer_pong(&mut client_io, &first).await;

    // 批准在 t=2 就绪、最终复查后 t=4 交付，第 2 轮 probe（t=5）不会发出，无需后台应答器。
    tokio::time::sleep(Duration::from_secs(1)).await;
    policy.approve();
    // 在途 probe 已结清、决定尚未就绪：这条旧三元组 pong 是协议错误，不是可容忍的残包。
    write_frame(
        &mut client_io,
        &HandshakeMessage::PendingPong(
            PendingPong::new(first.pending_token, first.status_nonce, first.probe_seq)
                .expect("pong"),
        ),
    )
    .await;

    let result = server.await.expect("task");
    match result {
        Ok(delivered) => {
            let mut io = delivered.io;
            let mut probe = [0u8; 64];
            let leaked = tokio::time::timeout(Duration::from_secs(1), io.read(&mut probe)).await;
            let leaked = match leaked {
                Ok(Ok(n)) => n,
                _ => {
                    panic!("Approved 已把 io 交付给加密层，且交付里没有任何残留可读——测试夹具失效")
                }
            };
            panic!(
                "无在途时到达的旧 pong 必须当场按协议违例关闭；实际批准被交付且 io 里带着 {leaked} 字节明文旧帧"
            );
        }
        Err(err) => {
            assert!(
                matches!(
                    err,
                    HandshakeError::Protocol(ProtocolViolation::UnexpectedFrame { .. })
                ),
                "无在途入站帧必须是 UnexpectedFrame 协议违例，实际: {err:?}"
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn stale_pending_pong_fails_at_arrival_between_probes() {
    // 错 phase 的 pong 单独到达（探测间隙、策略始终 Pending）：到达即违例，不等到下一轮 probe。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(ScriptPolicy::new(vec![AdmissionDecision::Pending]));
    let server = spawn_server(server_io, server_id, policy);

    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let first = read_pending(&mut client_io).await;
    answer_pong(&mut client_io, &first).await;

    tokio::time::sleep(Duration::from_secs(1)).await;
    write_frame(
        &mut client_io,
        &HandshakeMessage::PendingPong(
            PendingPong::new(first.pending_token, first.status_nonce, first.probe_seq)
                .expect("pong"),
        ),
    )
    .await;

    let err = server
        .await
        .expect("task")
        .expect_err("无在途的旧 pong 必须当场违例");
    assert!(
        matches!(
            err,
            HandshakeError::Protocol(ProtocolViolation::UnexpectedFrame { .. })
        ),
        "实际错误: {err:?}"
    );
    assert_eq!(
        started.elapsed(),
        Duration::from_secs(1),
        "违例必须发生在帧到达的那一刻，而不是等下一轮 probe 才被顺带发现"
    );
}

// ============================================================ 13. round2 R2：pong 记账前核对原绝对期限

/// pong 期限边界骨架：FlipPolicy 预批准（初判 Pending、其后 Approved），单在途
/// probe1；调用方在指定虚拟时刻写入回显 pong，`read_delay` 非零时用 DelayRead
/// 把「线上写入」与「读侧可读」分开，构造与到期定时器同刻就绪的竞态。
/// 每个边界情形必须是独立的 `#[tokio::test]`：同一 runtime 内跨用例共享虚拟时钟，
/// 循环复用会让后续用例的 t=0 基准被上一用例推进污染。
async fn pong_boundary_fixture(
    read_delay: Duration,
) -> (
    JoinHandle<Result<HandshakeResult<DelayRead<DuplexStream>>, HandshakeError>>,
    DuplexStream,
    Arc<IdentityKey>,
    Arc<IdentityKey>,
    tokio::time::Instant,
) {
    let (client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let flip = Arc::new(FlipPolicy::new());
    flip.approve();
    let started = tokio::time::Instant::now();
    let delayed = DelayRead::new(server_io, read_delay);
    let flip_task = Arc::clone(&flip);
    let server_id_for_task = Arc::clone(&server_id);
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id_for_task),
        };
        server_handshake(delayed, &config, flip_task.as_ref()).await
    });
    (server, client_io, client_id, server_id, started)
}

/// 写入不假定成功：用于「对端可能已经关闭」的场景（迟到帧救不回已关闭的连接）。
async fn write_frame_tolerant(
    io: &mut DuplexStream,
    message: &HandshakeMessage,
) -> std::io::Result<()> {
    let frame_type = match message {
        HandshakeMessage::ClientHello(_) => FrameType::ClientHello,
        HandshakeMessage::ServerChallenge(_) => FrameType::ServerChallenge,
        HandshakeMessage::ClientAuthRequest(_) => FrameType::ClientAuthRequest,
        HandshakeMessage::ServerPending(_) => FrameType::ServerPending,
        HandshakeMessage::PendingPong(_) => FrameType::PendingPong,
        HandshakeMessage::ServerAuthResponse(_) => FrameType::ServerAuthResponse,
    };
    let bytes = encode_handshake_frame(frame_type, message).expect("encode frame");
    io.write_all(&bytes).await?;
    io.flush().await
}

#[tokio::test(start_paused = true)]
async fn pong_before_deadline_is_still_credited_and_approved() {
    // 合法控制组：写阻塞夹具之外的简化版——t=9.5s 的 pong 必须结清并批准。
    let (server, mut client_io, client_id, _server_id, started) =
        pong_boundary_fixture(Duration::ZERO).await;
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let first = read_pending(&mut client_io).await;
    // 一切期限以 probe 的实际提交时刻为锚（§4.1：从 probe 提交起计 10s）。
    let probe_at = tokio::time::Instant::now();
    let _ = started;
    tokio::time::sleep_until(probe_at + Duration::from_millis(9_500)).await;
    answer_pong(&mut client_io, &first).await;

    let (ft, msg) = tokio::time::timeout(Duration::from_secs(1), read_frame(&mut client_io))
        .await
        .expect("提交后 9.5s 的合法 pong 必须当场结清并批准");
    assert_eq!(ft, FrameType::ServerAuthResponse);
    match msg {
        HandshakeMessage::ServerAuthResponse(response) => assert_eq!(response.status, 0),
        _ => panic!("期望 SERVER_AUTH_RESPONSE"),
    }
    assert_eq!(
        probe_at.elapsed(),
        Duration::from_millis(9_500),
        "批准必须落在 pong 到达的「提交后 9.5s」"
    );
    let _ = server.await.expect("task").expect("批准应成功");
}

#[tokio::test(start_paused = true)]
async fn pong_at_exact_deadline_is_not_credited() {
    // 边界：pong 于 t=10.0s **可读**（线上写入 t=9.9s，DelayRead 延迟 100ms），
    // 与到期定时器同刻就绪、biased 读分支先被轮询——旧实现就此结清批准。
    // 期限语义以 `timeout_at` 为准：`now >= sent_at + 10s` 一律超时，不靠分支顺序。
    let (server, mut client_io, client_id, _server_id, started) =
        pong_boundary_fixture(Duration::from_millis(100)).await;
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let first = read_pending(&mut client_io).await;
    let probe_at = tokio::time::Instant::now();
    let _ = started;
    tokio::time::sleep_until(probe_at + Duration::from_millis(9_900)).await;
    answer_pong(&mut client_io, &first).await;

    let err = server
        .await
        .expect("task")
        .expect_err("整点到达的 pong 不得结清批准（now >= deadline 即超时）");
    assert!(
        matches!(err, HandshakeError::Timeout(HandshakeStage::PendingPong)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        probe_at.elapsed(),
        Duration::from_secs(10),
        "超时必须落在「提交 + 10s」的原期限，而不是随 pong 可读时刻漂走"
    );
    expect_closed(&mut client_io).await;
}

#[tokio::test(start_paused = true)]
async fn pong_arriving_after_deadline_fails_as_timeout_not_approval() {
    // 迟到的 pong（t=11s 才写）绝不能把「已应超时」的连接救回来：
    // 服务端必须在原期限 t=10s 就按 Timeout(PendingPong) 关闭。
    let (server, mut client_io, client_id, _server_id, started) =
        pong_boundary_fixture(Duration::ZERO).await;
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let first = read_pending(&mut client_io).await;
    let probe_at = tokio::time::Instant::now();
    let _ = started;
    tokio::time::sleep_until(probe_at + Duration::from_secs(11)).await;
    assert!(
        server.is_finished(),
        "服务端必须在「提交 + 10s」自行超时关闭，而不是等迟到的 pong"
    );
    // 迟到的 pong 只能在已关闭的连接上失败；写入结果本身不是本用例的判据。
    let _ = write_frame_tolerant(
        &mut client_io,
        &HandshakeMessage::PendingPong(
            PendingPong::new(first.pending_token, first.status_nonce, first.probe_seq)
                .expect("pong"),
        ),
    )
    .await;

    let err = server
        .await
        .expect("task")
        .expect_err("迟到的 pong 不得结清批准");
    assert!(
        matches!(err, HandshakeError::Timeout(HandshakeStage::PendingPong)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        probe_at.elapsed(),
        Duration::from_secs(11),
        "本用例观察时刻为「提交后 11s」；服务端在「提交后 10s」已经关闭"
    );
    expect_closed(&mut client_io).await;
}

// ============================================================ 14. round2 R3：Pending↔Approved 交替不忙循环

/// 初判 Pending、其后 P/A 严格交替的策略；`done` 置位后一律 Approved。
/// 自带忙环检测：**同一虚拟瞬时** evaluate 超过阈值直接 panic（0ns 连轴转的判据）。
struct AlternatingPolicy {
    calls: AtomicUsize,
    done: AtomicBool,
    busy: Mutex<Option<(tokio::time::Instant, usize)>>,
}

impl AlternatingPolicy {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            done: AtomicBool::new(false),
            busy: Mutex::new(None),
        }
    }
    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl AdmissionPolicy for AlternatingPolicy {
    fn evaluate<'a>(&'a self, _peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let now = tokio::time::Instant::now();
        {
            let mut slot = self.busy.lock().expect("lock");
            match *slot {
                Some((at, count)) if at == now => {
                    assert!(
                        count < 15,
                        "同一虚拟瞬时 t={:?} evaluate 第 {} 次：回到 Pending 的状态转换在 0ns 忙循环",
                        now,
                        count + 1
                    );
                    *slot = Some((now, count + 1));
                }
                _ => *slot = Some((now, 1)),
            }
        }
        let decision = if self.done.load(Ordering::SeqCst) || n.is_multiple_of(2) {
            AdmissionDecision::Approved
        } else {
            AdmissionDecision::Pending
        };
        Box::pin(async move { Ok(decision) })
    }
}

#[tokio::test(start_paused = true)]
async fn alternating_pending_approved_backoff_never_busy_loops_and_keeps_probing() {
    // R3 反例：Pending→Approved→(final recheck)Pending→Approved→… 若「回到 Pending」
    // 只清决定不设复查退避，交替策略会让服务端在同一个 0ns 瞬时无限重查。
    // 要求：每轮回 PENDING 都有有界退避（时间必须推进）、probe 节奏不停、>100 轮后
    // 仍能正常批准，且不存在总审批期限。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let policy = Arc::new(AlternatingPolicy::new());
    let policy_task = Arc::clone(&policy);
    let server_id_for_task = Arc::clone(&server_id);
    let server = tokio::spawn(async move {
        let config = ServerHandshakeConfig {
            identity: Arc::clone(&server_id_for_task),
        };
        server_handshake(server_io, &config, policy_task.as_ref()).await
    });

    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;

    let mut nonce_first = None;
    let mut server = Some(server);
    for expected_seq in 1u64..=100 {
        let pending =
            match tokio::time::timeout(Duration::from_millis(5_500), read_pending(&mut client_io))
                .await
            {
                Ok(pending) => pending,
                Err(_) => {
                    if server.as_ref().is_some_and(|s| s.is_finished()) {
                        let handle = server.take().expect("take once");
                        let _ = handle.await.expect("task"); // 传播忙环检测等 panic
                        panic!("第 {expected_seq} 轮前服务端已正常结束，与交替 Pending 矛盾");
                    }
                    panic!("第 {expected_seq} 轮 probe 必须照常到达（退避不得停 probe）");
                }
            };
        assert_eq!(pending.probe_seq, expected_seq);
        if let Some(first) = nonce_first {
            assert_eq!(first, pending.status_nonce, "status_nonce 挂起期内不得变化");
        } else {
            nonce_first = Some(pending.status_nonce);
        }
        answer_pong(&mut client_io, &pending).await;
    }
    // 100 轮交替必须真实推进了虚拟时间（每轮回到 Pending 都有有界退避），不是 0ns。
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_secs(495),
        "100 轮 Pending/Approved 交替至少推进 99×5s=495s，实测 {elapsed:?}（≈0 即忙循环）"
    );
    assert!(
        policy.calls() <= 400,
        "evaluate 调用次数必须与轮数同阶（每轮 ≤4 次），实测 {} 次 / 100 轮",
        policy.calls()
    );

    // 之后一律 Approved：必须收敛到最终批准，且没有任何「审批总期限」曾掐断连接。
    policy.finish();
    let mut final_frame = None;
    for _ in 0..10 {
        let (ft, msg) =
            tokio::time::timeout(Duration::from_millis(11_000), read_frame(&mut client_io))
                .await
                .expect("收敛后的最终响应必须在 11s 内到达");
        match msg {
            HandshakeMessage::ServerPending(pending) => {
                assert_eq!(ft, FrameType::ServerPending);
                answer_pong(&mut client_io, &pending).await;
            }
            HandshakeMessage::ServerAuthResponse(response) => {
                assert_eq!(ft, FrameType::ServerAuthResponse);
                assert_eq!(response.status, 0, "交替退避后必须仍能批准");
                final_frame = Some(response);
                break;
            }
            _ => panic!("期望 SERVER_PENDING 或最终响应"),
        }
    }
    assert!(final_frame.is_some(), "done 之后必须收敛到最终批准");
    let _result = server
        .expect("task handle")
        .await
        .expect("task")
        .expect("批准应成功");
}

// ============================================================ 15. round2 回归：partial 读取消安全

#[tokio::test(start_paused = true)]
async fn partial_frame_survives_read_cancellation() {
    // 半截 pong 在途时，准入事件的唤醒会丢弃未完成的读帧 future（select 重入）；
    // 字节必须留在 FrameReader 缓冲区，补齐后仍能正确结清并进入下一轮 probe。
    let (mut client_io, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    // c2 挂 2s：t=2 的 Pending 返回就是在途读帧被丢弃重入的时刻。
    let policy = Arc::new(SlowPendingPolicy::new(Duration::from_secs(2)));
    let server = spawn_server(server_io, server_id, Arc::clone(&policy));

    raw_client_up_to_auth(&mut client_io, &client_id, &descriptor(), false).await;
    let first = read_pending(&mut client_io).await;
    let pong_bytes = encode_handshake_frame(
        FrameType::PendingPong,
        &HandshakeMessage::PendingPong(
            PendingPong::new(first.pending_token, first.status_nonce, first.probe_seq)
                .expect("pong"),
        ),
    )
    .expect("encode");

    let half = pong_bytes.len() / 2;
    client_io
        .write_all(&pong_bytes[..half])
        .await
        .expect("half 1");
    client_io.flush().await.expect("flush half");

    // 跨过 t=2 的准入唤醒（读 future 被丢弃重入），再补齐剩余字节。
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    client_io
        .write_all(&pong_bytes[half..])
        .await
        .expect("half 2");

    // 丢字节的话：这里只会读到被错帧边界毁掉的下一帧（Protocol 错误或永远等不到 seq 2）。
    let second = tokio::time::timeout(Duration::from_millis(3_000), read_pending(&mut client_io))
        .await
        .expect("结清半截 pong 后 t=5 的 probe 2 必须照常到达");
    assert_eq!(
        second.probe_seq, 2,
        "probe_seq 必须严格递增（半截帧不得污染流边界）"
    );
    assert_eq!(
        second.status_nonce, first.status_nonce,
        "status_nonce 挂起期内不得变化"
    );

    drop(client_io);
    let err = server.await.expect("task").expect_err("对端关闭后必须失败");
    assert!(matches!(err, HandshakeError::Io(_)), "{err:?}");
}

#[derive(Debug)]
struct LimitedRead<S> {
    inner: S,
    max: usize,
}
impl<S: AsyncRead + Unpin> AsyncRead for LimitedRead<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut bytes = [0u8; 64];
        let cap = buf.remaining().min(this.max).min(bytes.len());
        let mut small = ReadBuf::new(&mut bytes[..cap]);
        match Pin::new(&mut this.inner).poll_read(cx, &mut small) {
            Poll::Ready(Ok(())) => {
                buf.put_slice(small.filled());
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for LimitedRead<S> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, b)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
#[tokio::test(start_paused = true)]
async fn already_queued_duplicate_pong_is_rejected_even_with_short_reads() {
    for max in [1, 2, 7, 16] {
        let (mut client, server_io) = pipe();
        let client_id = identity();
        let server_id = identity();
        let policy = Arc::new(ScriptPolicy::new(vec![
            AdmissionDecision::Pending,
            AdmissionDecision::Approved,
        ]));
        let p = policy.clone();
        let config = ServerHandshakeConfig {
            identity: server_id,
        };
        let server = tokio::spawn(async move {
            server_handshake(
                LimitedRead {
                    inner: server_io,
                    max,
                },
                &config,
                p.as_ref(),
            )
            .await
        });
        raw_client_up_to_auth(&mut client, &client_id, &descriptor(), false).await;
        let pending = read_pending(&mut client).await;
        tokio::task::yield_now().await;
        assert_eq!(policy.calls(), 2, "批准必须已缓存后再写pong");
        let pong = encode_handshake_frame(
            FrameType::PendingPong,
            &HandshakeMessage::PendingPong(
                PendingPong::new(
                    pending.pending_token,
                    pending.status_nonce,
                    pending.probe_seq,
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let mut both = pong.clone();
        both.extend_from_slice(&pong);
        client.write_all(&both).await.unwrap();
        let result = server.await.unwrap();
        assert!(
            matches!(result, Err(HandshakeError::Protocol(_))),
            "maxread={max}: 已排队重复pong竟被批准，handoff仍有明文字节"
        );
    }
}
#[tokio::test(start_paused = true)]
async fn short_read_approval_with_no_extra_input_does_not_add_a_silence_wait() {
    let (mut client, server_io) = pipe();
    let client_id = identity();
    let server_id = identity();
    let p = ScriptPolicy::new(vec![
        AdmissionDecision::Pending,
        AdmissionDecision::Approved,
    ]);
    let config = ServerHandshakeConfig {
        identity: server_id,
    };
    let server = tokio::spawn(async move {
        server_handshake(
            LimitedRead {
                inner: server_io,
                max: 1,
            },
            &config,
            &p,
        )
        .await
    });
    let started = tokio::time::Instant::now();
    raw_client_up_to_auth(&mut client, &client_id, &descriptor(), false).await;
    let pending = read_pending(&mut client).await;
    answer_pong(&mut client, &pending).await;
    assert!(server.await.unwrap().is_ok());
    assert_eq!(started.elapsed(), Duration::ZERO);
}

struct GatedFinalCheck {
    calls: AtomicUsize,
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
impl AdmissionPolicy for GatedFinalCheck {
    fn evaluate<'a>(&'a self, _peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            match n {
                0 => Ok(AdmissionDecision::Pending),
                1 => Ok(AdmissionDecision::Approved),
                _ => {
                    self.started.notify_one();
                    self.release.notified().await;
                    Ok(AdmissionDecision::Approved)
                }
            }
        })
    }
}
#[tokio::test(start_paused = true)]
async fn input_arriving_during_final_recheck_is_rejected_before_signed_approval() {
    let (mut client, server_io) = pipe();
    let client_id = identity();
    let config = ServerHandshakeConfig {
        identity: identity(),
    };
    let policy = Arc::new(GatedFinalCheck {
        calls: AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let p = policy.clone();
    let server = tokio::spawn(async move {
        server_handshake(
            LimitedRead {
                inner: server_io,
                max: 1,
            },
            &config,
            p.as_ref(),
        )
        .await
    });
    raw_client_up_to_auth(&mut client, &client_id, &descriptor(), false).await;
    let pending = read_pending(&mut client).await;
    answer_pong(&mut client, &pending).await;
    policy.started.notified().await;
    client.write_all(&[0x53]).await.unwrap();
    policy.release.notify_one();
    assert!(matches!(
        server.await.unwrap(),
        Err(HandshakeError::Protocol(_))
    ));
}
#[tokio::test(start_paused = true)]
async fn eof_during_final_recheck_is_not_treated_as_an_empty_healthy_socket() {
    let (mut client, server_io) = pipe();
    let client_id = identity();
    let config = ServerHandshakeConfig {
        identity: identity(),
    };
    let policy = Arc::new(GatedFinalCheck {
        calls: AtomicUsize::new(0),
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    let p = policy.clone();
    let server =
        tokio::spawn(async move { server_handshake(server_io, &config, p.as_ref()).await });
    raw_client_up_to_auth(&mut client, &client_id, &descriptor(), false).await;
    let pending = read_pending(&mut client).await;
    answer_pong(&mut client, &pending).await;
    policy.started.notified().await;
    drop(client);
    policy.release.notify_one();
    assert!(matches!(
        server.await.unwrap(),
        Err(HandshakeError::Io(io::ErrorKind::UnexpectedEof))
    ));
}

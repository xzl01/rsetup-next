use bytes::Bytes;
use rsetup_tunnel::envelope::{PacketType, StreamKind, TunnelPacket};
use rsetup_tunnel::session::streams::{
    CONTROL_PATH, DATA_PATH, StreamError, StreamReceiver, StreamSender, accept_server_stream,
    open_client_stream,
};
use rsetup_tunnel::session::types::StreamEpoch;
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

fn packet(trace_id: u64, payload: Vec<u8>) -> TunnelPacket {
    TunnelPacket {
        trace_id,
        packet_type: PacketType::Request,
        action: "device.status.get".into(),
        status_code: 0,
        error_message: String::new(),
        payload,
        metadata: BTreeMap::new(),
    }
}
struct Stack {
    client: h2::client::SendRequest<Bytes>,
    accepted: mpsc::Receiver<(StreamKind, StreamSender, StreamReceiver)>,
    client_task: JoinHandle<()>,
    server_task: JoinHandle<()>,
}
impl Drop for Stack {
    fn drop(&mut self) {
        self.client_task.abort();
        self.server_task.abort();
    }
}
async fn stack(window: u32) -> Stack {
    let (a, b) = tokio::io::duplex(512);
    let (tx, accepted) = mpsc::channel(4);
    let server_task = tokio::spawn(async move {
        let mut conn = h2::server::Builder::new()
            .initial_window_size(window)
            .initial_connection_window_size(1024 * 1024)
            .handshake::<_, Bytes>(b)
            .await
            .unwrap();
        while let Some(result) = conn.accept().await {
            let (req, resp) = match result {
                Ok(v) => v,
                Err(_) => break,
            };
            if let Some((kind, send, recv)) =
                accept_server_stream(req, resp, StreamEpoch(1)).unwrap()
                && tx.send((kind, send, recv)).await.is_err()
            {
                break;
            }
        }
    });
    let (client, conn) = h2::client::Builder::new()
        .initial_window_size(1024 * 1024)
        .initial_connection_window_size(1024 * 1024)
        .handshake(a)
        .await
        .unwrap();
    let client_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    Stack {
        client,
        accepted,
        client_task,
        server_task,
    }
}
#[tokio::test]
async fn streams_roundtrip_both_directions_with_real_h2() {
    let mut s = stack(1024 * 1024).await;
    for kind in [StreamKind::Control, StreamKind::Data] {
        let (mut cs, mut cr) = open_client_stream(&mut s.client, kind, StreamEpoch(1))
            .await
            .unwrap();
        let (got, mut ss, mut sr) = s.accepted.recv().await.unwrap();
        assert_eq!(got, kind);
        let p = packet(1, b"request".to_vec());
        let (sent, received) = tokio::join!(cs.send_packet(&p), sr.recv_packet());
        sent.unwrap();
        assert_eq!(received.unwrap(), Some(p.clone()));
        let mut response = p;
        response.packet_type = PacketType::Response;
        let (sent, received) = tokio::join!(ss.send_packet(&response), cr.recv_packet());
        sent.unwrap();
        assert_eq!(received.unwrap(), Some(response));
    }
}
#[tokio::test]
async fn large_message_crosses_stream_flow_control_without_deadlock() {
    let mut s = stack(32768).await;
    let (mut cs, _cr) = open_client_stream(&mut s.client, StreamKind::Data, StreamEpoch(1))
        .await
        .unwrap();
    let (_, _ss, mut sr) = s.accepted.recv().await.unwrap();
    let p = packet(1, vec![0x5a; 512 * 1024 - 128]);
    let work = async {
        let (sent, received) = tokio::join!(cs.send_packet(&p), sr.recv_packet());
        sent.unwrap();
        assert_eq!(received.unwrap(), Some(p));
    };
    tokio::time::timeout(Duration::from_secs(3), work)
        .await
        .unwrap();
}
#[tokio::test]
async fn unknown_method_path_returns_unimplemented_grpc_trailer() {
    let mut s = stack(1024 * 1024).await;
    std::future::poll_fn(|cx| s.client.poll_ready(cx))
        .await
        .unwrap();
    let req = http::Request::builder()
        .method("POST")
        .uri("http://rsetup/custom_tunnel.TunnelService/Unknown")
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .unwrap();
    let (response, _send) = s.client.send_request(req, false).unwrap();
    let mut body = response.await.unwrap().into_body();
    while let Some(data) = body.data().await {
        let data = data.unwrap();
        body.flow_control().release_capacity(data.len()).unwrap();
    }
    let trailers = body.trailers().await.unwrap().unwrap();
    assert_eq!(trailers["grpc-status"], "12");
}
#[tokio::test]
async fn malformed_http_headers_reset_only_the_stream() {
    let mut s = stack(1024 * 1024).await;
    std::future::poll_fn(|cx| s.client.poll_ready(cx))
        .await
        .unwrap();
    let req = http::Request::builder()
        .method("GET")
        .uri(format!("http://rsetup{CONTROL_PATH}"))
        .body(())
        .unwrap();
    let (response, _send) = s.client.send_request(req, false).unwrap();
    let error = response.await.unwrap_err();
    assert!(error.is_reset());
    let (_cs, _cr) = open_client_stream(&mut s.client, StreamKind::Control, StreamEpoch(1))
        .await
        .unwrap();
    assert_eq!(s.accepted.recv().await.unwrap().0, StreamKind::Control);
}
#[tokio::test]
async fn cancelled_partial_send_poison_resets_stream_not_next_packet() {
    let mut s = stack(32).await;
    let (mut cs, _cr) = open_client_stream(&mut s.client, StreamKind::Data, StreamEpoch(1))
        .await
        .unwrap();
    let (_, _ss, mut sr) = s.accepted.recv().await.unwrap();
    let p = packet(1, vec![1; 100000]);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), cs.send_packet(&p))
            .await
            .is_err()
    );
    assert!(cs.is_closed());
    assert_eq!(
        cs.send_packet(&packet(2, vec![])).await.unwrap_err(),
        StreamError::Closed
    );
    assert!(sr.recv_packet().await.is_err());
}
#[tokio::test]
async fn stream_reset_does_not_close_other_stream() {
    let mut s = stack(1024 * 1024).await;
    let (mut cc, _ccr) = open_client_stream(&mut s.client, StreamKind::Control, StreamEpoch(1))
        .await
        .unwrap();
    let (_, _sc, mut scr) = s.accepted.recv().await.unwrap();
    let (_cd, mut cdr) = open_client_stream(&mut s.client, StreamKind::Data, StreamEpoch(1))
        .await
        .unwrap();
    let (_, mut sd, _sdr) = s.accepted.recv().await.unwrap();
    sd.reset(h2::Reason::REFUSED_STREAM);
    assert!(matches!(
        cdr.recv_packet().await,
        Err(StreamError::Reset(_))
    ));
    let p = packet(3, b"control remains usable".to_vec());
    let (sent, received) = tokio::join!(cc.send_packet(&p), scr.recv_packet());
    sent.unwrap();
    assert_eq!(received.unwrap(), Some(p));
}
#[tokio::test]
async fn grpc_status_belongs_to_stream_eos_not_each_rpc() {
    let mut s = stack(1024 * 1024).await;
    let (_cs, mut cr) = open_client_stream(&mut s.client, StreamKind::Control, StreamEpoch(1))
        .await
        .unwrap();
    let (_, mut ss, _sr) = s.accepted.recv().await.unwrap();
    let mut p = packet(1, vec![]);
    p.packet_type = PacketType::Response;
    let (sent, received) = tokio::join!(ss.send_packet(&p), cr.recv_packet());
    sent.unwrap();
    assert_eq!(received.unwrap(), Some(p));
    ss.finish().unwrap();
    assert_eq!(cr.recv_packet().await.unwrap(), None);
}
#[test]
fn canonical_service_paths_are_stable() {
    assert_eq!(CONTROL_PATH, "/custom_tunnel.TunnelService/OpenControl");
    assert_eq!(DATA_PATH, "/custom_tunnel.TunnelService/OpenData");
}

struct RawPeer {
    opened: Option<Result<(StreamSender, StreamReceiver), StreamError>>,
    send: h2::SendStream<Bytes>,
    recv: h2::RecvStream,
    client_task: JoinHandle<()>,
    server_task: JoinHandle<()>,
}
impl Drop for RawPeer {
    fn drop(&mut self) {
        self.client_task.abort();
        self.server_task.abort();
    }
}
async fn raw_peer(header_status: Option<&'static str>, headers_eos: bool) -> RawPeer {
    let (a, b) = tokio::io::duplex(512);
    let (tx, mut rx) = mpsc::channel(1);
    let server_task = tokio::spawn(async move {
        let mut conn = h2::server::handshake(b).await.unwrap();
        while let Some(Ok((req, mut respond))) = conn.accept().await {
            let mut response = http::Response::builder()
                .status(200)
                .header("content-type", "application/grpc");
            if let Some(status) = header_status {
                response = response.header("grpc-status", status);
            }
            let send = respond
                .send_response(response.body(()).unwrap(), headers_eos)
                .unwrap();
            if tx.send((send, req.into_body())).await.is_err() {
                break;
            }
        }
    });
    let (mut client, conn) = h2::client::handshake(a).await.unwrap();
    let client_task = tokio::spawn(async move {
        let _ = conn.await;
    });
    let opened = open_client_stream(&mut client, StreamKind::Control, StreamEpoch(1)).await;
    let (send, recv) = rx.recv().await.unwrap();
    RawPeer {
        opened: Some(opened),
        send,
        recv,
        client_task,
        server_task,
    }
}
#[tokio::test]
async fn half_closed_sender_can_still_reset_the_entire_stream() {
    let mut peer = raw_peer(None, false).await;
    let (mut send, mut recv) = peer.opened.take().unwrap().unwrap();
    send.finish().unwrap();
    while let Some(data) = peer.recv.data().await {
        let data = data.unwrap();
        peer.recv
            .flow_control()
            .release_capacity(data.len())
            .unwrap();
    }
    peer.send
        .send_data(Bytes::from_static(&[1, 0, 0, 0, 0]), false)
        .unwrap();
    assert_eq!(recv.recv_packet().await.unwrap_err(), StreamError::Grpc);
    send.reset(h2::Reason::PROTOCOL_ERROR);
    let reason = tokio::time::timeout(
        Duration::from_millis(50),
        std::future::poll_fn(|cx| peer.send.poll_reset(cx)),
    )
    .await
    .expect("half-closed sender did not send RST")
    .unwrap();
    assert_eq!(reason, h2::Reason::PROTOCOL_ERROR);
}
#[tokio::test]
async fn receiver_failure_remains_the_same_failure_not_eof() {
    let mut peer = raw_peer(None, false).await;
    let (_send, mut recv) = peer.opened.take().unwrap().unwrap();
    peer.send
        .send_data(Bytes::from_static(&[1, 0, 0, 0, 0]), false)
        .unwrap();
    assert_eq!(recv.recv_packet().await.unwrap_err(), StreamError::Grpc);
    assert_eq!(recv.recv_packet().await.unwrap_err(), StreamError::Grpc);
}
#[tokio::test]
async fn trailers_only_response_preserves_zero_and_nonzero_status() {
    let mut zero = raw_peer(Some("0"), true).await;
    let (_send, mut recv) = zero.opened.take().unwrap().unwrap();
    assert_eq!(recv.recv_packet().await.unwrap(), None);
    let mut denied = raw_peer(Some("12"), true).await;
    match denied.opened.take().unwrap() {
        Err(error) => assert_eq!(error, StreamError::GrpcStatus(12)),
        Ok((_send, mut recv)) => assert_eq!(
            recv.recv_packet().await.unwrap_err(),
            StreamError::GrpcStatus(12)
        ),
    }
}
#[tokio::test]
async fn grpc_status_rejects_noncanonical_plus_and_leading_zeros() {
    for status in ["+0", "00", "01", "", "-1", "4294967296"] {
        let mut peer = raw_peer(None, false).await;
        let (_send, mut recv) = peer.opened.take().unwrap().unwrap();
        let mut trailers = http::HeaderMap::new();
        trailers.insert("grpc-status", http::HeaderValue::from_static(status));
        peer.send.send_trailers(trailers).unwrap();
        assert_eq!(
            recv.recv_packet().await.unwrap_err(),
            StreamError::BadResponse,
            "{status:?}"
        );
        assert_eq!(
            recv.recv_packet().await.unwrap_err(),
            StreamError::BadResponse
        );
    }
}

#[tokio::test]
async fn ordinary_initial_status_cannot_end_a_non_eos_response() {
    let mut peer = raw_peer(Some("0"), false).await;
    let (_send, mut recv) = peer.opened.take().unwrap().unwrap();
    let mut p = packet(33, b"response still alive".to_vec());
    p.packet_type = PacketType::Response;
    peer.send
        .send_data(
            Bytes::from(rsetup_tunnel::grpc::encode_message(&p).unwrap()),
            false,
        )
        .unwrap();
    assert_eq!(recv.recv_packet().await.unwrap(), Some(p));
    let mut trailers = http::HeaderMap::new();
    trailers.insert("grpc-status", http::HeaderValue::from_static("13"));
    peer.send.send_trailers(trailers).unwrap();
    assert_eq!(
        recv.recv_packet().await.unwrap_err(),
        StreamError::GrpcStatus(13)
    );
    assert_eq!(
        recv.recv_packet().await.unwrap_err(),
        StreamError::GrpcStatus(13)
    );
}
#[tokio::test]
async fn reset_and_missing_status_remain_sticky_failures() {
    let mut peer = raw_peer(None, false).await;
    let (_send, mut recv) = peer.opened.take().unwrap().unwrap();
    peer.send.send_reset(h2::Reason::REFUSED_STREAM);
    for _ in 0..2 {
        assert_eq!(recv.recv_packet().await.unwrap_err(), StreamError::Reset(7));
    }
    let mut peer = raw_peer(None, false).await;
    let (_send, mut recv) = peer.opened.take().unwrap().unwrap();
    peer.send.send_data(Bytes::new(), true).unwrap();
    for _ in 0..2 {
        assert_eq!(
            recv.recv_packet().await.unwrap_err(),
            StreamError::BadResponse
        );
    }
}

/// 用原始 h2 客户端在指定 gRPC 路径上写出**任意完整 gRPC 帧**（绕开 `send_packet`
/// 的发送侧 validate），用来观察接收端如何给失败归类。
async fn send_raw_frame(stack: &mut Stack, path: &'static str, frame: &[u8]) {
    let request = http::Request::builder()
        .method(http::Method::POST)
        .version(http::Version::HTTP_2)
        .uri(format!("http://rsetup{path}"))
        .header("content-type", "application/grpc")
        .header("te", "trailers")
        .body(())
        .expect("构造请求");
    let (response, mut send) = stack
        .client
        .send_request(request, false)
        .expect("打开原始流");
    send.send_data(Bytes::from(frame.to_vec()), true)
        .expect("写出原始 gRPC 帧");
    // 必须让响应侧继续存活：直接丢弃 ResponseFuture 等于取消这条流，
    // 服务端就看不到完整请求了。
    tokio::spawn(async move {
        let _ = response.await;
    });
}

/// 失败归属必须由**类型**给出，而不是错误文本或调用方自己维护的 bool：
/// * 载荷根本不是 envelope（含 §6.7.2 的非 16B `tunnel.ping`）= 应用包内容非法
///   → `StreamProtocol`，`connection_fatal() == false`，只允许终结本条流；
/// * 能解码但破坏 §6.3 流路由（System 出现在数据流）= 结构性违规
///   → 仍是 `Protocol`，`connection_fatal() == true`，不得被降级成流级。
#[tokio::test]
async fn error_ownership_is_typed_not_inferred_from_text() {
    let mut stack = stack(1024 * 1024).await;

    // 1) 帧框架合法、载荷解不出 envelope。
    let mut junk_frame = vec![0u8; 5];
    junk_frame[1..5].copy_from_slice(&5u32.to_be_bytes());
    junk_frame.extend_from_slice(&[0xff_u8; 5]);
    send_raw_frame(&mut stack, CONTROL_PATH, &junk_frame).await;
    let (kind, _sender, mut receiver) = stack.accepted.recv().await.expect("控制流已受理");
    assert_eq!(kind, StreamKind::Control);
    let error = tokio::time::timeout(Duration::from_secs(3), receiver.recv_packet())
        .await
        .expect("必须有结论，不得悬空")
        .expect_err("非法应用包必须以错误结清");
    assert_eq!(error, StreamError::StreamProtocol);
    assert!(
        !error.connection_fatal(),
        "应用包内容非法只允许终结本条流，不得升级成连接级"
    );

    // 2) 能解码但路由违规：System 包出现在数据流。
    let mut illegal = packet(9, Vec::new());
    illegal.packet_type = PacketType::System;
    illegal.action = "tunnel.kick".into();
    // `encode_message` 产出的就是完整 gRPC 帧（含 5B 前缀），直接原样写出。
    let frame = rsetup_tunnel::grpc::encode_message(&illegal).expect("编码合法信封");
    send_raw_frame(&mut stack, DATA_PATH, &frame).await;
    let (kind, _sender, mut receiver) = stack.accepted.recv().await.expect("数据流已受理");
    assert_eq!(kind, StreamKind::Data);
    let error = tokio::time::timeout(Duration::from_secs(3), receiver.recv_packet())
        .await
        .expect("必须有结论，不得悬空")
        .expect_err("路由违规必须以错误结清");
    assert_eq!(error, StreamError::Protocol, "§6.3 路由违规必须仍是连接级");
    assert!(error.connection_fatal());
}

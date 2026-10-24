//! Task 4c: 有界 HTTP/2 排空控制与 GOAWAY 观察接口（`H2WireControl`）。
//!
//! 范围声明：本文件是**纯 wire 级**判别测试（手工帧 + duplex + 可编程底层）
//! 加**真 h2 双端**互操作测试。它不声称 session 层的 5s drain、双进程接线或
//! 业务 pending 结清已完成——那些属于 Task 4 的 session driver。
//!
//! 关键验收（协议规范 §6.6/§6.7、计划 Task4 裁决、RFC9113 §3.4/§4.3/§6.8）：
//! * 句柄在 IO 被 h2 移走前取得，之后不再接触具体泛型 S 也能发 GOAWAY；
//! * client GOAWAY：type=7、stream=0、payload 恰 8B 全零（last_stream_id=0、
//!   NO_ERROR、debug 空）——RFC9113 §6.8 的 last ID 是 **peer-initiated** 流，
//!   本协议禁 server push，客户端恒 0，不取自己最大奇数 ID；
//! * server 角色 `request_client_goaway` → `WrongRole`（server 走 h2 官方 graceful）；
//! * 单槽幂等：重复请求不插多个 GOAWAY；`wait_goaway_sent` 要整帧 + flush 确认；
//! * 注入只落在完整帧与 HEADERS/CONTINUATION 块边界，且在 24B 前导与**首条
//!   SETTINGS**（RFC9113 §3.4）之后；在途半帧 PING 必须先原样写完；
//! * 空闲 `poll_read` 的 driver 必须被句柄唤醒；fatal/Drop 唤醒 waiter 为终态；
//! * 入站 GOAWAY：只读结构化 8B 字段进 watch（有界 latest、sequence 递增），
//!   原帧逐字给 h2，不吞帧、不代 ACK；结构性非法（len<8、stream≠0）不通知。

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use rsetup_tunnel::h2_keepalive::{
    H2KeepaliveIo, H2Role, H2WireControl, KEEPALIVE_WINDOW, PeerGoaway, WireControlError,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream, ReadBuf};
use tokio::task::unconstrained;
use tokio::time::advance;

const PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const TYPE_DATA: u8 = 0;
const TYPE_HEADERS: u8 = 0x1;
const TYPE_SETTINGS: u8 = 0x4;
const TYPE_PING: u8 = 0x6;
const TYPE_GOAWAY: u8 = 0x7;
const TYPE_CONTINUATION: u8 = 0x9;
const FLAG_ACK: u8 = 0x1;
const FLAG_END_HEADERS: u8 = 0x4;
const WINDOW: Duration = KEEPALIVE_WINDOW;

/// h2 0.4.20 内部保留的 SHUTDOWN PING token（`h2/src/frame/ping.rs`）。
const H2_SHUTDOWN_TOKEN: [u8; 8] = [0x0b, 0x7b, 0xa2, 0xf0, 0x8b, 0x9b, 0xfe, 0x54];

// ===== 手工帧工具 =====

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    ftype: u8,
    flags: u8,
    stream: u32,
    payload: Vec<u8>,
}

fn encode_frame(ftype: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut out = Vec::with_capacity(9 + payload.len());
    out.push((len >> 16) as u8);
    out.push((len >> 8) as u8);
    out.push(len as u8);
    out.push(ftype);
    out.push(flags);
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn take_frames(buf: &mut Vec<u8>) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while buf.len() - off >= 9 {
        let len =
            ((buf[off] as usize) << 16) | ((buf[off + 1] as usize) << 8) | buf[off + 2] as usize;
        if buf.len() - off - 9 < len {
            break;
        }
        let stream = ((buf[off + 5] as u32 & 0x7f) << 24)
            | ((buf[off + 6] as u32) << 16)
            | ((buf[off + 7] as u32) << 8)
            | buf[off + 8] as u32;
        let start = off + 9;
        out.push(Frame {
            ftype: buf[off + 3],
            flags: buf[off + 4],
            stream,
            payload: buf[start..start + len].to_vec(),
        });
        off = start + len;
    }
    buf.drain(..off);
    out
}

fn empty_settings() -> Vec<u8> {
    encode_frame(TYPE_SETTINGS, 0, 0, &[])
}

fn goaway_bytes(last_stream_id: u32, error_code: u32, debug: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + debug.len());
    payload.extend_from_slice(&last_stream_id.to_be_bytes());
    payload.extend_from_slice(&error_code.to_be_bytes());
    payload.extend_from_slice(debug);
    encode_frame(TYPE_GOAWAY, 0, 0, &payload)
}

// ===== 手工驱动的 harness =====

fn test_waker() -> Waker {
    Waker::noop().clone()
}

fn drain_raw<T: AsyncRead + Unpin>(peer: &mut T, waker: &Waker) -> Vec<u8> {
    let mut cx = Context::from_waker(waker);
    let mut out = Vec::new();
    let mut tmp = [0u8; 16 * 1024];
    loop {
        let n = {
            let mut rb = ReadBuf::new(&mut tmp);
            match Pin::new(&mut *peer).poll_read(&mut cx, &mut rb) {
                Poll::Ready(Ok(())) => rb.filled().len(),
                Poll::Ready(Err(_)) | Poll::Pending => return out,
            }
        };
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&tmp[..n]);
    }
}

fn feed_raw<T: AsyncWrite + Unpin>(peer: &mut T, waker: &Waker, bytes: &[u8]) {
    let mut cx = Context::from_waker(waker);
    let mut off = 0usize;
    while off < bytes.len() {
        match Pin::new(&mut *peer).poll_write(&mut cx, &bytes[off..]) {
            Poll::Ready(Ok(0)) => panic!("对端写入被拒"),
            Poll::Ready(Ok(n)) => off += n,
            Poll::Ready(Err(e)) => panic!("对端写出错: {e}"),
            Poll::Pending => panic!("对端缓冲已满（测试未预期）"),
        }
    }
}

fn strip_preface(buf: &mut Vec<u8>) {
    if buf.starts_with(PREFACE.as_slice()) {
        buf.drain(..PREFACE.len());
    }
}

/// 只 poll 一次，绝不 sleep/advance 伪成功。
fn poll_once<F: Future>(f: F) -> Option<F::Output> {
    let mut f = Box::pin(f);
    let waker = test_waker();
    let mut cx = Context::from_waker(&waker);
    match f.as_mut().poll(&mut cx) {
        Poll::Ready(v) => Some(v),
        Poll::Pending => None,
    }
}

struct Harness {
    io: H2KeepaliveIo<DuplexStream>,
    ctl: H2WireControl,
    peer: DuplexStream,
    waker: Waker,
    /// 已观测但未凑成整帧的出站字节。
    out_tail: Vec<u8>,
}

impl Harness {
    fn new(role: H2Role, cap: usize) -> Self {
        let (a, b) = tokio::io::duplex(cap);
        let io = H2KeepaliveIo::new(a, role);
        // 句柄必须在 io 被交给 h2 之前取得——之后“交给 h2”的用例里 io 会被移走。
        let ctl = io.control_handle();
        Self {
            io,
            ctl,
            peer: b,
            waker: test_waker(),
            out_tail: Vec::new(),
        }
    }

    fn write(&mut self, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        Pin::new(&mut self.io).poll_write(&mut cx, bytes)
    }

    fn write_all(&mut self, bytes: &[u8]) {
        let mut off = 0usize;
        while off < bytes.len() {
            match self.write(&bytes[off..]) {
                Poll::Ready(Ok(0)) => panic!("底层拒绝写入"),
                Poll::Ready(Ok(n)) => off += n,
                Poll::Ready(Err(e)) => panic!("写出错: {e}"),
                Poll::Pending => panic!("写出被阻塞（测试未预期）"),
            }
        }
    }

    fn read(&mut self, want: usize) -> Poll<io::Result<Vec<u8>>> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        let mut buf = vec![0u8; want];
        let n = {
            let mut rb = ReadBuf::new(&mut buf);
            match Pin::new(&mut self.io).poll_read(&mut cx, &mut rb) {
                Poll::Ready(Ok(())) => rb.filled().len(),
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
        };
        buf.truncate(n);
        Poll::Ready(Ok(buf))
    }

    fn read_exact(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < n {
            match self.read(n - out.len()) {
                Poll::Ready(Ok(v)) if v.is_empty() => panic!("意外 EOF"),
                Poll::Ready(Ok(got)) => out.extend_from_slice(&got),
                Poll::Ready(Err(e)) => panic!("读出错: {e}"),
                Poll::Pending => panic!("读被阻塞（测试未预期）"),
            }
        }
        out
    }

    fn read_until_pending(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match self.read(8 * 1024) {
                Poll::Ready(Ok(v)) if v.is_empty() => return out,
                Poll::Ready(Ok(n)) => out.extend_from_slice(&n),
                Poll::Ready(Err(e)) => panic!("读出错: {e}"),
                Poll::Pending => return out,
            }
        }
    }

    fn flush(&mut self) {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        let _ = Pin::new(&mut self.io).poll_flush(&mut cx);
    }

    fn pump(&mut self) {
        self.flush();
        let _ = self.read(4096);
    }

    fn feed_peer(&mut self, bytes: &[u8]) {
        feed_raw(&mut self.peer, &self.waker, bytes);
    }

    /// 抽干出向原始字节（保留残尾给帧解析）。
    fn drain_out(&mut self) -> Vec<u8> {
        let raw = drain_raw(&mut self.peer, &self.waker);
        self.out_tail.extend_from_slice(&raw);
        raw
    }

    /// 出向整帧（自动剥离一次客户端前导）。
    fn out_frames(&mut self) -> Vec<Frame> {
        self.drain_out();
        strip_preface(&mut self.out_tail);
        take_frames(&mut self.out_tail)
    }

    fn out_types(&mut self) -> Vec<u8> {
        self.out_frames().iter().map(|f| f.ftype).collect()
    }
}

/// 客户端角色 + 已写前导与首条 SETTINGS（满足 RFC9113 §3.4 的首帧要求），
/// 并排空首窗探测，使后续出站字节只由用例自身决定。
fn ready_client() -> Harness {
    let mut h = Harness::new(H2Role::Client, 256 * 1024);
    h.write_all(PREFACE);
    h.write_all(&empty_settings());
    h.pump();
    let _ = h.out_frames();
    h
}

// ===== 可编程底层（故障路径）=====

#[derive(Default)]
struct MockWire {
    emitted: Vec<u8>,
}

#[derive(Clone)]
struct MockHandle {
    wire: Arc<Mutex<MockWire>>,
    drops: Arc<AtomicUsize>,
}

impl MockHandle {
    fn emitted(&self) -> Vec<u8> {
        self.wire.lock().unwrap().emitted.clone()
    }
    fn drops(&self) -> usize {
        self.drops.load(Ordering::SeqCst)
    }
}

struct MockIo {
    wire: Arc<Mutex<MockWire>>,
    drops: Arc<AtomicUsize>,
    out_budget: usize,
    write_err: bool,
    /// true → 底层 `poll_flush` 恒 Pending（模拟只入队未上线）。
    pending_flush: bool,
    inbound: VecDeque<u8>,
}

fn mock_io(out_budget: usize) -> (MockIo, MockHandle) {
    let wire = Arc::new(Mutex::new(MockWire::default()));
    let drops = Arc::new(AtomicUsize::new(0));
    (
        MockIo {
            wire: wire.clone(),
            drops: drops.clone(),
            out_budget,
            write_err: false,
            pending_flush: false,
            inbound: VecDeque::new(),
        },
        MockHandle { wire, drops },
    )
}

impl Drop for MockIo {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl AsyncRead for MockIo {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if me.inbound.is_empty() {
            return Poll::Pending;
        }
        let n = buf.remaining().min(me.inbound.len());
        let chunk: Vec<u8> = me.inbound.drain(..n).collect();
        buf.put_slice(&chunk);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for MockIo {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        if me.write_err {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "可编程底层写错误",
            )));
        }
        if me.out_budget == 0 {
            return Poll::Pending;
        }
        let n = buf.len().min(me.out_budget);
        me.wire.lock().unwrap().emitted.extend_from_slice(&buf[..n]);
        me.out_budget -= n;
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        if me.pending_flush {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn yield_ticks(n: usize) {
    for _ in 0..n {
        tokio::task::yield_now().await;
    }
}

// ===== A: GOAWAY 注入的边界与幂等 =====

/// GOAWAY 绝不允许出现在 24B 前导或首条 SETTINGS 之前（RFC9113 §3.4），
/// 且形状必须是 type7/stream0/8B 全零 payload（§6.8 + 禁 push ⇒ last=0）。
#[tokio::test]
async fn goaway_never_precedes_preface_or_first_settings() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = Harness::new(H2Role::Client, 256 * 1024);
        // begin_drain 只为排除探测干扰，让出向断言只盯 GOAWAY
        h.ctl.begin_drain().expect("排空");
        // 什么都没写就请求 GOAWAY：必须安静地排队，不许吐任何字节
        h.ctl.request_client_goaway().expect("client 可请求");
        h.pump();
        assert!(h.drain_out().is_empty(), "前导之前不得有任何出站字节");

        h.write_all(PREFACE);
        h.pump();
        let types = h.out_types();
        assert_eq!(
            types,
            vec![],
            "前导已写但首条 SETTINGS 未写：GOAWAY（以及 PING）都不得抢在 §3.4 的 SETTINGS 前"
        );

        h.write_all(&empty_settings());
        h.pump();
        let frames = h.out_frames();
        let types: Vec<u8> = frames.iter().map(|f| f.ftype).collect();
        assert_eq!(types, vec![TYPE_SETTINGS, TYPE_GOAWAY], "{frames:?}");
        let g = &frames[1];
        assert_eq!(g.stream, 0, "GOAWAY 属于连接控制流 0");
        assert_eq!(g.flags, 0);
        assert_eq!(
            g.payload,
            vec![0u8; 8],
            "last_stream_id=0（peer-initiated 语义，禁 push）且 NO_ERROR、debug 空"
        );
    })
    .await;
}

/// 单槽幂等：重复请求不插多个 GOAWAY；句柄可 clone 且跨线程可用（Send）。
#[tokio::test]
async fn client_goaway_is_idempotent_single_slot() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = ready_client();
        h.ctl.request_client_goaway().expect("第一次请求");
        // 克隆句柄并跨线程调用（句柄必须 Send + Clone，不持 S）
        let ctl2 = h.ctl.clone();
        std::thread::spawn(move || ctl2.request_client_goaway())
            .join()
            .expect("线程内请求")
            .expect("克隆句柄同样幂等");
        h.ctl.request_client_goaway().expect("第三次仍幂等");
        h.pump();
        let goaways: Vec<Frame> = h
            .out_frames()
            .into_iter()
            .filter(|f| f.ftype == TYPE_GOAWAY)
            .collect();
        assert_eq!(goaways.len(), 1, "单槽幂等：{goaways:?}");
        assert_eq!(goaways[0].payload, vec![0u8; 8]);
        h.pump();
        h.pump();
        assert_eq!(
            h.out_types().iter().filter(|t| **t == TYPE_GOAWAY).count(),
            0,
            "幂等不得在后续 poll 再插 GOAWAY"
        );
    })
    .await;
}

/// server 角色请求 client GOAWAY 必须 `WrongRole`：server 走 h2 官方 graceful，
/// 本组件不代 server 发连接级 GOAWAY。入站观察与 begin_drain 不受角色限制。
#[tokio::test]
async fn server_role_rejects_client_goaway_with_wrong_role() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = Harness::new(H2Role::Server, 256 * 1024);
        assert_eq!(
            h.ctl.request_client_goaway(),
            Err(WireControlError::WrongRole)
        );
        h.feed_peer(PREFACE);
        let _ = h.read_exact(PREFACE.len());
        // 真实 h2 server 会立刻写 SETTINGS（§3.4）：fixture 不违反该规则，
        // 才不会掩盖注入门控。
        h.write_all(&empty_settings());
        let _ = h.drain_out();
        // 推进两个窗口（3 个未 ACK 会如实判死，与本用例无关）：
        // 错误角色绝不因“重试”而吐 GOAWAY
        for _ in 0..2 {
            advance(WINDOW).await;
            h.pump();
            assert_eq!(
                h.ctl.request_client_goaway(),
                Err(WireControlError::WrongRole)
            );
        }
        let raw_all = h.drain_out();
        // 保活 PING 允许；GOAWAY 绝不允许（server 的关闭由 h2 官方 graceful 负责）
        assert!(
            raw_all
                .windows(4)
                .all(|w| !(w[0] == 0 && w[1] == 0 && w[2] == 8 && w[3] == 7)),
            "server 角色不得自发 GOAWAY: {raw_all:?}"
        );
        // server 的排空句柄仍可用，入站 GOAWAY 观察与角色无关
        h.ctl.begin_drain().expect("server 可 begin_drain");
        let raw = goaway_bytes(3, 0, b"from-peer");
        h.feed_peer(&raw);
        let got = h.read_exact(raw.len());
        assert_eq!(got, raw, "GOAWAY 必须原样交 h2");
        assert!(h.ctl.subscribe_peer_goaway().borrow().is_some());
    })
    .await;
}

/// 注入只能落在完整帧边界与 HEADERS/CONTINUATION 块结束后；
/// 半写的业务帧头/帧体内绝不插 GOAWAY，跨窗口也不得插。
#[tokio::test]
async fn goaway_waits_for_frame_and_header_block_boundaries() {
    unconstrained(async move {
        tokio::time::pause();
        // 1) HEADERS 块（无 END_HEADERS）→ CONTINUATION(END_HEADERS)：
        //    GOAWAY 只能在终帧之后。
        let mut h = ready_client();
        h.write_all(&encode_frame(TYPE_HEADERS, 0, 1, b"hdr-frag-a"));
        h.pump();
        let mid = h.out_types();
        assert_eq!(mid, vec![TYPE_HEADERS], "先只应看到刚写的 HEADERS");
        // header block 已开：此刻才请求 GOAWAY；随后进入排空（真实调用序列：
        // 排空 + GOAWAY 一起发起），排空冻结窗口失败计数，跨多个窗口也不会
        // 把“未 ACK 的探测”误判成死链——边界断言本身不受影响。
        h.ctl.request_client_goaway().expect("请求");
        h.ctl.begin_drain().expect("排空");
        advance(WINDOW * 3).await;
        h.pump();
        assert!(h.out_types().is_empty(), "header block 未结束不得插帧");
        h.write_all(&encode_frame(
            TYPE_CONTINUATION,
            FLAG_END_HEADERS,
            1,
            b"hdr-frag-b",
        ));
        h.pump();
        let after = h.out_types();
        assert_eq!(
            after,
            vec![TYPE_CONTINUATION, TYPE_GOAWAY],
            "GOAWAY 只能出现在 END_HEADERS 终帧之后"
        );

        // 2) 帧头写到一半：GOAWAY 不得插进帧内
        let mut h2 = ready_client();
        let data = encode_frame(TYPE_DATA, 0, 1, b"0123456789");
        h2.write_all(&data[..5]);
        h2.ctl.request_client_goaway().expect("请求");
        h2.ctl.begin_drain().expect("排空");
        h2.pump();
        advance(WINDOW * 3).await;
        h2.pump();
        let raw = h2.drain_out();
        assert_eq!(
            raw,
            data[..5],
            "帧头写到一半时除刚写出的字节外不得有任何新增"
        );
        h2.write_all(&data[5..]);
        h2.pump();
        let frames = h2.out_frames();
        let types: Vec<u8> = frames.iter().map(|f| f.ftype).collect();
        assert_eq!(types, vec![TYPE_DATA, TYPE_GOAWAY], "{frames:?}");
    })
    .await;
}

/// 在途半帧 PING 与 GOAWAY 的先后：旧半帧必须**原样写完**，GOAWAY 在其后；
/// 整条字节流必须恰好凑成整帧（不重发、不覆盖、不丢尾）。
#[tokio::test]
async fn goaway_never_covers_a_partially_written_probe() {
    unconstrained(async move {
        tokio::time::pause();
        // cap=40：前导 24 + SETTINGS 9 占住缓冲，PING 17 只能先上 7 字节
        let mut h = Harness::new(H2Role::Client, 40);
        h.write_all(PREFACE);
        h.write_all(&empty_settings());
        advance(WINDOW).await;
        h.pump();
        let partial = h.drain_out();
        assert_eq!(partial.len(), 40, "PING 应部分写出（7B）");
        assert_eq!(&partial[33..40], &[0, 0, 8, TYPE_PING, 0, 0, 0]);

        h.ctl.request_client_goaway().expect("请求");
        // 腾空缓冲后反复推动：PING 剩余 10B → （可能再来一条当前窗口 PING）→ GOAWAY
        let mut rest = Vec::new();
        for _ in 0..8 {
            h.pump();
            rest.extend_from_slice(&h.drain_out());
        }
        let mut all = partial;
        all.extend_from_slice(&rest);
        let mut tail = all[24..].to_vec(); // 去前导
        let frames = take_frames(&mut tail);
        assert!(tail.is_empty(), "字节流必须恰好凑成整帧: {tail:?}");
        let types: Vec<u8> = frames.iter().map(|f| f.ftype).collect();
        assert_eq!(
            types.last(),
            Some(&TYPE_GOAWAY),
            "GOAWAY 必须排在所有半帧补全之后: {frames:?}"
        );
        assert_eq!(
            types.iter().filter(|t| **t == TYPE_GOAWAY).count(),
            1,
            "GOAWAY 恰一条: {frames:?}"
        );
        // 旧半帧的连续性：首帧仍是完整 SETTINGS，第二条是完整 PING（8B token）
        assert_eq!(frames[0].ftype, TYPE_SETTINGS);
        assert_eq!(frames[1].ftype, TYPE_PING);
        assert_eq!(frames[1].payload.len(), 8, "PING 帧必须完整");
        let g = frames.last().expect("GOAWAY");
        assert_eq!(g.payload, vec![0u8; 8]);
        // GOAWAY 之前的非 GOAWAY 帧只可能是 SETTINGS/PING
        for f in &frames[..frames.len() - 1] {
            assert!(
                f.ftype == TYPE_SETTINGS || f.ftype == TYPE_PING,
                "{frames:?}"
            );
        }
    })
    .await;
}

// ===== B: begin_drain 语义 =====

/// begin_drain 停的是**新周期自发 probe**：已开窗口的半帧照写完，窗口不再新
/// 开、不再假阳性计失败；不阻止业务帧继续写出；幂等。
#[tokio::test]
async fn begin_drain_stops_new_probes_but_not_business_bytes() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = ready_client();
        // 先来一个正常窗口（发出探测）
        advance(WINDOW).await;
        h.pump();
        let probes = h.out_types().iter().filter(|t| **t == TYPE_PING).count();
        assert_eq!(probes, 1);
        let opened = h.io.stats().windows_opened;

        h.ctl.begin_drain().expect("排空");
        h.ctl.begin_drain().expect("幂等");

        // 长时间推进：不再开新窗、不再发探测、不判死链
        for _ in 0..5 {
            advance(WINDOW).await;
            h.pump();
            assert!(h.out_types().iter().all(|t| *t != TYPE_PING));
        }
        let st = h.io.stats();
        assert_eq!(st.windows_opened, opened, "排空不得再开新窗口");
        assert_eq!(st.consecutive_failures, 1, "排空前的窗口照常计一次失败");
        assert!(!st.failed, "排空不得把后续窗口算成死链");

        // 业务 DATA 仍必须原样透传（不停止已接流输出）
        h.write_all(&encode_frame(TYPE_DATA, 0, 1, b"trailing-body"));
        let raw = h.drain_out();
        assert_eq!(raw, encode_frame(TYPE_DATA, 0, 1, b"trailing-body"));
    })
    .await;
}

// ===== C: wait_goaway_sent 的确认与终态 =====

/// 只有**整帧通过底层 poll_write 且 poll_flush 确认**后 wait 才成功：
/// 底层 flush Pending 时不得伪成功；fatal 错误必须唤醒 waiter 为终态 Closed。
#[tokio::test(flavor = "current_thread")]
async fn wait_goaway_sent_needs_write_and_flush_and_fails_fatal() {
    unconstrained(async move {
        tokio::time::pause();
        let (mut lower, obs) = mock_io(64);
        lower.pending_flush = true;
        let mut io = H2KeepaliveIo::new(lower, H2Role::Client);
        let ctl = io.control_handle();
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);

        // begin_drain 排除探测干扰，出向字节只由 SETTINGS + GOAWAY 决定
        ctl.begin_drain().expect("排空");
        // 前导 + 首 SETTINGS
        let pre: &[u8] = PREFACE;
        let mut off = 0usize;
        while off < pre.len() {
            match Pin::new(&mut io).poll_write(&mut cx, &pre[off..]) {
                Poll::Ready(Ok(n)) => off += n,
                other => panic!("写前导意外 {other:?}"),
            }
        }
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, &empty_settings()),
            Poll::Ready(Ok(_))
        ));

        // 未请求：Pending，不许凭空成功
        assert!(
            poll_once(ctl.wait_goaway_sent()).is_none(),
            "未请求不得成功"
        );

        ctl.request_client_goaway().expect("请求");
        for _ in 0..4 {
            let _ = Pin::new(&mut io).poll_flush(&mut cx);
            let mut buf = [0u8; 64];
            let _ = Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut buf));
        }
        // 整帧已写出，但底层 flush 恒 Pending：只入队不算已写
        assert_eq!(obs.emitted().len(), 24 + 9 + 17, "{:?}", obs.emitted());
        assert!(
            poll_once(ctl.wait_goaway_sent()).is_none(),
            "flush 未确认不得报成功"
        );

        // fatal：底层写错误 → 终态，waiter 立即唤醒为 Closed
        io.get_mut().expect("底层尚未释放").write_err = true;
        let _ = Pin::new(&mut io).poll_write(&mut cx, b"x");
        assert_eq!(
            poll_once(ctl.wait_goaway_sent()),
            Some(Err(WireControlError::Closed))
        );
        assert_eq!(obs.drops(), 1, "fatal 必须释放底层");
    })
    .await;
}

/// 底层 flush 恢复后（整帧 + flush 双确认），wait 才成功；wire 上恰一条 GOAWAY。
#[tokio::test(flavor = "current_thread")]
async fn wait_goaway_sent_ok_after_flushed_frame() {
    unconstrained(async move {
        tokio::time::pause();
        let (mut lower, obs) = mock_io(4096);
        lower.pending_flush = true;
        let mut io = H2KeepaliveIo::new(lower, H2Role::Client);
        let ctl = io.control_handle();
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        ctl.begin_drain().expect("排空");
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, PREFACE),
            Poll::Ready(Ok(24))
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, &empty_settings()),
            Poll::Ready(Ok(_))
        ));
        ctl.request_client_goaway().expect("请求");
        let _ = Pin::new(&mut io).poll_flush(&mut cx);
        assert_eq!(obs.emitted().len(), 24 + 9 + 17);
        assert!(poll_once(ctl.wait_goaway_sent()).is_none(), "flush 未确认");
        // 恢复 flush，再驱动一次：整帧 + flush 双确认
        io.get_mut().expect("底层尚未释放").pending_flush = false;
        let _ = Pin::new(&mut io).poll_flush(&mut cx);
        assert_eq!(poll_once(ctl.wait_goaway_sent()), Some(Ok(())));
        let emitted = obs.emitted();
        let goaway = &emitted[24 + 9..];
        assert_eq!(goaway, &[0, 0, 8, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    })
    .await;
}

/// Drop 也必须唤醒 Pending 的 waiter（终态 Closed，不得永挂）。
#[tokio::test(flavor = "current_thread")]
async fn dropping_io_wakes_pending_waiter_with_closed() {
    unconstrained(async move {
        tokio::time::pause();
        let h = ready_client();
        h.ctl.request_client_goaway().expect("请求");
        // 刻意不 pump：GOAWAY 尚未上线
        let ctl = h.ctl.clone();
        let waiter = tokio::spawn(async move { ctl.wait_goaway_sent().await });
        yield_ticks(2).await;
        drop(h);
        assert_eq!(
            waiter.await.expect("waiter 任务"),
            Err(WireControlError::Closed)
        );
    })
    .await;
}

/// 空闲 driver（挂在 poll_read 上的任务）必须被句柄唤醒并完成注入——
/// 否则“空闲连接无法排空”就是空头支票。
#[tokio::test(flavor = "current_thread")]
async fn request_wakes_idle_poll_read_driver() {
    unconstrained(async move {
        let (inner, mut peer) = tokio::io::duplex(256 * 1024);
        let mut io = H2KeepaliveIo::new(inner, H2Role::Client);
        let ctl = io.control_handle();
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        for b in PREFACE.iter().chain(empty_settings().iter()) {
            match Pin::new(&mut io).poll_write(&mut cx, &[*b]) {
                Poll::Ready(Ok(_)) => {}
                other => panic!("写首字节意外 {other:?}"),
            }
        }

        // 模拟空闲的 h2 Connection：任务只挂起在 poll_read
        let driver = tokio::spawn(async move {
            let mut buf = [0u8; 256];
            loop {
                if io.read(&mut buf[..]).await.is_err() {
                    break;
                }
            }
        });
        yield_ticks(4).await;
        let mut raw = drain_raw(&mut peer, &waker);
        strip_preface(&mut raw);
        // 先清掉首窗探测（合法行为，与本用例无关）
        let _ = take_frames(&mut raw);

        ctl.request_client_goaway().expect("请求");
        // 真实唤醒：句柄的 request 必须 call 已注册 waker；这里不 sleep 伪成功
        yield_ticks(8).await;
        let mut raw = drain_raw(&mut peer, &waker);
        strip_preface(&mut raw);
        let frames = take_frames(&mut raw);
        assert!(
            frames
                .iter()
                .any(|f| f.ftype == TYPE_GOAWAY && f.payload == vec![0u8; 8]),
            "空闲 poll_read 任务必须被唤醒并注入 GOAWAY: {frames:?}"
        );
        driver.abort();
    })
    .await;
}

// ===== D: 入站 GOAWAY 观察 =====

/// peer GOAWAY 到达即在 watch 可见（**不需要**未来再 send_request），
/// 原帧逐字给 h2，不吞帧、不截 debug。
#[tokio::test]
async fn peer_goaway_watch_is_immediate_and_bytes_are_verbatim() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = ready_client();
        let mut sub = h.ctl.subscribe_peer_goaway();
        assert!(sub.borrow().is_none(), "初值必须是无通知");

        let raw = goaway_bytes(5, 0x2, &[0xde, 0xad, 0xbe, 0xef]);
        h.feed_peer(&raw);
        let got = h.read_until_pending();
        assert_eq!(got, raw, "GOAWAY 必须逐字交 h2（含 debug 尾）");
        sub.changed().await.expect("watch 更新");
        assert_eq!(
            *sub.borrow_and_update(),
            Some(PeerGoaway {
                last_stream_id: 5,
                error_code: 0x2,
                sequence: 1,
            }),
            "只取有界 8B 字段，debug 不进通知"
        );
    })
    .await;
}

/// 分片到达：观察只依赖已交付的前 8B payload；debug 大尾流式透传不落整帧缓冲；
/// 多条 GOAWAY：watch 保 latest，sequence 单调递增（不累积 peer 日志）。
#[tokio::test]
async fn peer_goaway_fragmented_debug_and_multi_notify() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = ready_client();
        let sub = h.ctl.subscribe_peer_goaway();

        let debug: Vec<u8> = (0..8192).map(|i| (i % 251) as u8).collect();
        let g1 = goaway_bytes(7, 0x1, &debug);
        let mut got = Vec::new();
        let mut off = 0usize;
        while off < g1.len() {
            let end = (off + 3).min(g1.len());
            h.feed_peer(&g1[off..end]);
            got.extend_from_slice(&h.read_until_pending());
            off = end;
        }
        assert_eq!(got, g1, "debug 大尾必须逐字流式透传");
        assert_eq!(
            *sub.borrow(),
            Some(PeerGoaway {
                last_stream_id: 7,
                error_code: 0x1,
                sequence: 1,
            })
        );

        // 第二条：latest + sequence 递增，不累积历史
        let g2 = goaway_bytes(0, 0x1, &[]);
        h.feed_peer(&g2);
        let got2 = h.read_until_pending();
        assert_eq!(got2, g2);
        assert_eq!(
            *sub.borrow(),
            Some(PeerGoaway {
                last_stream_id: 0,
                error_code: 0x1,
                sequence: 2,
            })
        );
    })
    .await;
}

/// 结构性非法的“像 GOAWAY”不通知：len<8 或 stream≠0。合法性是**结构**判定，
/// 不替 h2 做协议终裁；但字节必须依旧原样透传。
#[tokio::test]
async fn structurally_invalid_goaway_is_not_notified_but_forwarded() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = ready_client();
        let sub = h.ctl.subscribe_peer_goaway();

        // len=4：不足 8B 结构化字段
        let short = encode_frame(TYPE_GOAWAY, 0, 0, &[0, 0, 0, 1]);
        h.feed_peer(&short);
        assert_eq!(h.read_until_pending(), short);
        assert!(sub.borrow().is_none(), "len<8 结构性非法不得通知");

        // stream≠0：连接级帧放错流
        let bad = encode_frame(TYPE_GOAWAY, 0, 3, &[0, 0, 0, 1, 0, 0, 0, 0]);
        h.feed_peer(&bad);
        assert_eq!(h.read_until_pending(), bad);
        assert!(sub.borrow().is_none(), "stream≠0 不得通知");

        // 对照：合法帧确实通知
        let ok = goaway_bytes(1, 0, &[]);
        h.feed_peer(&ok);
        assert_eq!(h.read_until_pending(), ok);
        assert!(sub.borrow().is_some());
    })
    .await;
}

// ===== E: 真 h2 双端互操作 =====

type TapLog = Arc<Mutex<Vec<u8>>>;

struct TapIo<T> {
    inner: T,
    tx: TapLog,
    rx: TapLog,
}

impl<T> TapIo<T> {
    fn new(inner: T) -> (Self, TapLog, TapLog) {
        let tx = Arc::new(Mutex::new(Vec::new()));
        let rx = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner,
                tx: tx.clone(),
                rx: rx.clone(),
            },
            tx,
            rx,
        )
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for TapIo<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut me.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {}
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        }
        let got = &buf.filled()[before..];
        if !got.is_empty() {
            me.rx.lock().unwrap().extend_from_slice(got);
        }
        Poll::Ready(Ok(()))
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for TapIo<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let me = self.get_mut();
        let n = match Pin::new(&mut me.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => n,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        };
        if n > 0 {
            me.tx.lock().unwrap().extend_from_slice(&buf[..n]);
        }
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

fn drain_log(log: &TapLog) -> Vec<u8> {
    std::mem::take(&mut *log.lock().unwrap())
}

async fn roundtrip(sender: &mut h2::client::SendRequest<bytes::Bytes>) -> Result<Vec<u8>, ()> {
    let req = http::Request::builder()
        .method(http::Method::POST)
        .uri("http://rsetup.local/tunnel.RTunnel/OpenControl")
        .body(())
        .expect("request");
    let (resp, mut send_stream) = sender.send_request(req, false).map_err(|_| ())?;
    send_stream
        .send_data(bytes::Bytes::from_static(b"ping-body"), true)
        .map_err(|_| ())?;
    let mut resp = resp.await.map_err(|_| ())?;
    let mut body = Vec::new();
    while let Some(d) = resp.body_mut().data().await {
        body.extend_from_slice(&d.map_err(|_| ())?);
    }
    Ok(body)
}

/// 真 h2：client 经 control 句柄发 GOAWAY——
/// * 已开的在途流正常 EOS（响应体完好）；
/// * 之后的新流被拒（server 收到 GOAWAY 后拒绝新流）；
/// * server 侧的 wire-control 句柄即时看到 peer GOAWAY（不靠未来 send_request），
///   且 last=0 符合 RFC9113 §68 的 peer-initiated 语义（本协议禁 push）。
#[tokio::test(flavor = "current_thread")]
async fn real_h2_client_goaway_finishes_stream_and_bars_new_streams() {
    unconstrained(async move {
        tokio::time::pause();
        let (a_end, b_end) = tokio::io::duplex(256 * 1024);

        // client: h2 → keepalive(带句柄) → duplex
        let a_keep = H2KeepaliveIo::new(a_end, H2Role::Client);
        let a_ctl = a_keep.control_handle();
        let (mut sender, conn) = h2::client::handshake(a_keep).await.expect("h2 client");
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        // server: keepalive(带句柄) 交给 h2 server；句柄留在测试侧观察
        let b_keep = H2KeepaliveIo::new(b_end, H2Role::Server);
        let b_ctl = b_keep.control_handle();
        let mut b_rx = b_ctl.subscribe_peer_goaway();
        let srv = tokio::spawn(async move {
            let mut server = h2::server::handshake(b_keep).await.expect("h2 server");
            let mut served = 0usize;
            while let Some(item) = server.accept().await {
                let (mut req, mut respond) = match item {
                    Ok(item) => item,
                    Err(_) => break,
                };
                while let Some(d) = req.body_mut().data().await {
                    if d.is_err() {
                        return (served, false);
                    }
                }
                match respond.send_response(http::Response::new(()), false) {
                    Ok(mut stream) => {
                        if stream
                            .send_data(bytes::Bytes::from_static(b"pong"), true)
                            .is_err()
                        {
                            return (served, false);
                        }
                        served += 1;
                    }
                    Err(_) => return (served, false),
                }
            }
            (served, true)
        });

        assert_eq!(roundtrip(&mut sender).await.expect("首个往返"), b"pong");

        // 在途流上先挂一个请求，再请求 GOAWAY：当前流必须照常 EOS
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri("http://rsetup.local/tunnel.RTunnel/OpenData")
            .body(())
            .expect("request");
        let (resp_fut, mut send_stream) = sender.send_request(req, false).expect("在途请求");
        send_stream
            .send_data(bytes::Bytes::from_static(b"ping-body"), true)
            .expect("在途流体照常写出");
        // 本用例覆盖请求先EOS的时序；这不是GOAWAY的协议前提。
        // midstream专项在GOAWAY已写出/对端已观察后才发送原流DATA/EOS，
        // 并以独立handler持续驱动Connection，覆盖两种响应头时机。
        yield_ticks(8).await;
        a_ctl.request_client_goaway().expect("client 请求 GOAWAY");
        let mut resp_fut = std::pin::pin!(resp_fut);
        let wait = {
            let ctl = a_ctl.clone();
            async move { ctl.wait_goaway_sent().await }
        };
        let mut wait = std::pin::pin!(wait);
        let pwaker = test_waker();
        let mut pcx = Context::from_waker(&pwaker);
        let mut sent = None;
        let mut body: Option<Vec<u8>> = None;
        for _ in 0..64 {
            if sent.is_none()
                && let Poll::Ready(r) = wait.as_mut().poll(&mut pcx)
            {
                sent = Some(r);
            }
            if body.is_none()
                && let Poll::Ready(r) = resp_fut.as_mut().poll(&mut pcx)
            {
                let mut resp = r.expect("在途响应到达");
                let mut b = Vec::new();
                while let Some(d) = resp.body_mut().data().await {
                    b.extend_from_slice(&d.expect("body"));
                }
                body = Some(b);
            }
            if sent.is_some() && body.is_some() {
                break;
            }
            yield_ticks(2).await;
        }
        assert_eq!(sent.expect("GOAWAY 写出+flush 确认"), Ok(()));
        assert_eq!(body.expect("在途流必须正常 EOS"), b"pong");

        // server 侧句柄即时可见 peer GOAWAY（last=0、NO_ERROR、seq=1）
        for _ in 0..64 {
            if b_rx.borrow_and_update().is_some() {
                break;
            }
            yield_ticks(2).await;
        }
        assert_eq!(
            *b_rx.borrow(),
            Some(PeerGoaway {
                last_stream_id: 0,
                error_code: 0,
                sequence: 1,
            }),
            "server 必须即时感知 client GOAWAY（RFC9113 §6.8：peer-initiated ⇒ last=0）"
        );

        // GOAWAY 之后新流必须被拒（server 已 drain；旧流不受影响）
        let mut barred = false;
        for _ in 0..64 {
            yield_ticks(2).await;
            if roundtrip(&mut sender).await.is_err() {
                barred = true;
                break;
            }
        }
        assert!(barred, "GOAWAY 之后的新流必须被拒绝");

        drop(sender);
        let _ = conn_task.await;
        let (served, ok) = srv.await.expect("server 任务");
        assert!(ok, "server 侧不得出现半路 IO 错误");
        assert_eq!(served, 2, "GOAWAY 前的两个流都必须正常结清");
    })
    .await;
}

/// 真 h2：server 走 **h2 官方 graceful**（本组件绝不替 server 注入 GOAWAY）；
/// h2 原生 graceful 的 GOAWAY 与 SHUTDOWN PING/ACK 必须逐字透过本组件：
/// client 侧 wire-control watch 可感知 GOAWAY，PING/ACK 不得被吞或代答。
#[tokio::test(flavor = "current_thread")]
async fn real_h2_server_graceful_shutdown_ping_ack_not_swallowed() {
    unconstrained(async move {
        tokio::time::pause();
        let (a_end, b_end) = tokio::io::duplex(256 * 1024);

        // client 侧：keepalive 之下垫一层 wire tap，验证透传字节
        let (a_wire, a_tx, a_rx) = TapIo::new(a_end);
        let a_keep = H2KeepaliveIo::new(a_wire, H2Role::Client);
        let a_ctl = a_keep.control_handle();
        let mut a_rx_peer = a_ctl.subscribe_peer_goaway();
        let (mut sender, conn) = h2::client::handshake(a_keep).await.expect("h2 client");
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        // server 侧：官方 graceful 由驱动任务在收到停止信号时触发
        let b_keep = H2KeepaliveIo::new(b_end, H2Role::Server);
        let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
        let srv = tokio::spawn(async move {
            let mut server = h2::server::handshake(b_keep).await.expect("h2 server");
            loop {
                tokio::select! {
                    item = server.accept() => {
                        let Some(item) = item else { break };
                        let (mut req, mut respond) = match item {
                            Ok(item) => item,
                            Err(_) => break,
                        };
                        tokio::spawn(async move {
                            while let Some(d) = req.body_mut().data().await {
                                if d.is_err() {
                                    break;
                                }
                            }
                            if let Ok(mut s) =
                                respond.send_response(http::Response::new(()), false)
                            {
                                let _ = s.send_data(bytes::Bytes::from_static(b"pong"), true);
                            }
                        });
                    }
                    _ = stop_rx.changed(), if !(*stop_rx.borrow_and_update()) => {}
                }
                // 停止信号在 arm 之外消费：guard 保持 false，信号到达后这里退出并 graceful
                if *stop_rx.borrow_and_update() {
                    server.graceful_shutdown();
                    break;
                }
            }
            // 让 GOAWAY/SHUTDOWN PING 有机会发出并等 ACK，直到 conn 自己结束
            // （h2 0.4 的 server::Connection 不是 Future，用 poll_closed 驱动收尾）
            let _ = std::future::poll_fn(|cx| server.poll_closed(cx)).await;
        });

        assert_eq!(roundtrip(&mut sender).await.expect("往返"), b"pong");
        stop_tx.send(true).expect("通知 server graceful");

        for _ in 0..64 {
            if a_rx_peer.borrow_and_update().is_some() {
                break;
            }
            yield_ticks(2).await;
        }
        {
            let seen = a_rx_peer.borrow();
            let peer = seen.expect("client 必须经 watch 感知 server 官方 GOAWAY");
            assert_eq!(peer.error_code, 0, "graceful 必须 NO_ERROR: {peer:?}");
            assert_eq!(peer.sequence, 1);
        }

        // 官方 graceful 关闭收尾会发 SHUTDOWN PING 并等 ACK：
        // 观察 client 入向 wire 有 PING(SHUTDOWN)、出向 wire 有对应 ACK——
        // 两者都必须来自 h2 本体，本组件既不能吞也不能代答。
        let mut saw_ping = false;
        let mut saw_ack = false;
        let mut inb = Vec::new();
        let mut out = Vec::new();
        for _ in 0..128 {
            inb.extend_from_slice(&drain_log(&a_rx));
            out.extend_from_slice(&drain_log(&a_tx));
            saw_ping |= inb
                .windows(H2_SHUTDOWN_TOKEN.len() + 9)
                .any(|w| w[3] == TYPE_PING && w[4] & FLAG_ACK == 0 && w[9..] == H2_SHUTDOWN_TOKEN);
            saw_ack |= out
                .windows(H2_SHUTDOWN_TOKEN.len() + 9)
                .any(|w| w[3] == TYPE_PING && w[4] & FLAG_ACK != 0 && w[9..] == H2_SHUTDOWN_TOKEN);
            if saw_ping && saw_ack {
                break;
            }
            yield_ticks(4).await;
        }
        assert!(
            saw_ping,
            "graceful 的 SHUTDOWN PING 不得被吞: 入向未见 PING"
        );
        assert!(
            saw_ack,
            "SHUTDOWN PING 必须由 h2 正常 ACK（本组件不代答也不拦截）"
        );

        drop(sender);
        let _ = conn_task.await;
        let _ = srv.await;
    })
    .await;
}

// ===== F: 句柄类型约束 =====

/// 句柄不持底层 S：Send + Sync + Clone；io 被移进擦除泛型的“黑盒”（模拟交给
/// h2）后，仅凭句柄仍能完成请求→唤醒→注入的完整闭环。
#[tokio::test]
async fn handle_is_send_sync_and_independent_of_generic_io() {
    fn assert_send_sync_clone<T: Send + Sync + Clone>() {}
    assert_send_sync_clone::<H2WireControl>();
    assert_send_sync_clone::<PeerGoaway>();

    unconstrained(async move {
        tokio::time::pause();
        let h = Harness::new(H2Role::Client, 256 * 1024);
        let ctl = h.ctl.clone();
        let mut peer = h.peer;
        let waker = h.waker.clone();
        // 把 io 移进黑盒：外部只剩 dyn Any，无法按泛型访问
        let mut boxed: Box<dyn std::any::Any> = Box::new(h.io);
        let io = boxed
            .as_mut()
            .downcast_mut::<H2KeepaliveIo<DuplexStream>>()
            .expect("downcast");
        let mut cx = Context::from_waker(&waker);
        for b in PREFACE.iter().chain(empty_settings().iter()) {
            match Pin::new(&mut *io).poll_write(&mut cx, &[*b]) {
                Poll::Ready(Ok(_)) => {}
                other => panic!("写首字节意外 {other:?}"),
            }
        }
        ctl.request_client_goaway().expect("黑盒外仅凭句柄请求");
        // 黑盒内照常 poll：句柄驱动注入，无需任何泛型访问
        let _ = Pin::new(&mut *io).poll_flush(&mut cx);
        let raw = drain_raw(&mut peer, &waker);
        assert!(
            raw.windows(4)
                .any(|w| w[0] == 0 && w[1] == 0 && w[2] == 8 && w[3] == TYPE_GOAWAY),
            "仅凭句柄完成注入: {raw:?}"
        );
    })
    .await;
}

/// RFC9113: GOAWAY後既有流仍可完成；Connection必須與handler並行驅動。
#[tokio::test]
async fn real_h2_midstream_goaway_preserves_both_response_header_timings() {
    for early_headers in [false, true] {
        let (a, b) = tokio::io::duplex(4096);
        let client_io = H2KeepaliveIo::new(a, H2Role::Client);
        let client_ctl = client_io.control_handle();
        let server_io = H2KeepaliveIo::new(b, H2Role::Server);
        let server_ctl = server_io.control_handle();
        let mut peer_goaway = server_ctl.subscribe_peer_goaway();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server_task = tokio::spawn(async move {
            let mut conn = h2::server::handshake(server_io).await.unwrap();
            let mut tasks = tokio::task::JoinSet::new();
            let mut accepted = Some(accepted_tx);
            loop {
                tokio::select! {
                    request=conn.accept()=>match request {
                        Some(Ok((request,mut response)))=>{
                            let signal=accepted.take();
                            tasks.spawn(async move {
                                let mut send=if early_headers {Some(response.send_response(http::Response::new(()),false).unwrap())}else{None};
                                if let Some(signal)=signal {let _=signal.send(());}
                                let mut recv=request.into_body();let mut body=Vec::new();
                                while let Some(data)=recv.data().await {let data=data.unwrap();body.extend_from_slice(&data);recv.flow_control().release_capacity(data.len()).unwrap();}
                                assert_eq!(body,b"body-after-goaway");
                                let send=send.get_or_insert_with(||response.send_response(http::Response::new(()),false).unwrap());
                                send.send_data(bytes::Bytes::from_static(b"response-after-goaway"),true).unwrap();
                            });
                        }
                        Some(Err(error))=>panic!("server connection: {error}"),
                        None=>break,
                    },
                    done=tasks.join_next(),if !tasks.is_empty()=>{done.unwrap().unwrap();}
                }
            }
            while let Some(done) = tasks.join_next().await {
                done.unwrap();
            }
        });
        let (mut client, conn) = h2::client::handshake(client_io).await.unwrap();
        let client_task = tokio::spawn(conn);
        let request = http::Request::builder()
            .method("POST")
            .uri("http://rsetup.local/hold")
            .body(())
            .unwrap();
        let (response, mut send) = client.send_request(request, false).unwrap();
        let mut response_future = Some(response);
        tokio::time::timeout(std::time::Duration::from_secs(2), accepted_rx)
            .await
            .unwrap()
            .unwrap();
        // 早头分支必须证实客户端已收到HEADERS，而不止server API已入队。
        let early_response = if early_headers {
            Some(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    response_future.take().unwrap(),
                )
                .await
                .expect("early response headers were not received before GOAWAY")
                .unwrap(),
            )
        } else {
            None
        };
        client_ctl.begin_drain().unwrap();
        client_ctl.request_client_goaway().unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client_ctl.wait_goaway_sent(),
        )
        .await
        .unwrap()
        .unwrap();
        if peer_goaway.borrow().is_none() {
            tokio::time::timeout(std::time::Duration::from_secs(2), peer_goaway.changed())
                .await
                .unwrap()
                .unwrap();
        }
        assert_eq!(peer_goaway.borrow().as_ref().unwrap().last_stream_id, 0);
        // GOAWAY已经写出且对端已观察；现在才发原流DATA/EOS。
        send.send_data(bytes::Bytes::from_static(b"body-after-goaway"), true)
            .unwrap();
        let response = match early_response {
            Some(response) => response,
            None => tokio::time::timeout(
                std::time::Duration::from_secs(2),
                response_future.take().unwrap(),
            )
            .await
            .expect("existing stream lost after GOAWAY")
            .unwrap(),
        };
        let mut response = response.into_body();
        let body = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            let mut body = Vec::new();
            while let Some(data) = response.data().await {
                let data = data.unwrap();
                body.extend_from_slice(&data);
                response
                    .flow_control()
                    .release_capacity(data.len())
                    .unwrap();
            }
            body
        })
        .await
        .expect("existing response DATA/EOS did not complete after GOAWAY");
        assert_eq!(
            body, b"response-after-goaway",
            "early_headers={early_headers}"
        );
        drop(send);
        drop(response);
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(2), client_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), server_task)
            .await
            .unwrap()
            .unwrap();
    }
}

// ===== G: 出站 GOAWAY 的观察计数（session 排空窗口的锚点依据）=====

/// `subscribe_outbound_goaway` 只在**整帧经底层 poll_write 且 poll_flush 已确认**
/// 后 +1：只入队未 flush 时绝不增长（这正是「排空窗口不能提前起算」所需的负向信号）。
/// 两种角色同构：client 的自发注入与 server 侧 h2 自己写出的 GOAWAY 都只走
/// 「写出 + flush 确认」这一条观察路径；本组件不因观察而多写/少写一个字节。
#[tokio::test(flavor = "current_thread")]
async fn outbound_goaway_counts_only_flushed_frames() {
    unconstrained(async move {
        tokio::time::pause();

        // ---- client 角色：自发注入的那一帧
        let (mut lower, obs) = mock_io(4096);
        lower.pending_flush = true;
        let mut io = H2KeepaliveIo::new(lower, H2Role::Client);
        let ctl = io.control_handle();
        let mut seen = ctl.subscribe_outbound_goaway();
        assert_eq!(*seen.borrow_and_update(), 0, "初始必须为 0");
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        ctl.begin_drain().expect("排空");
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, PREFACE),
            Poll::Ready(Ok(24))
        ));
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, &empty_settings()),
            Poll::Ready(Ok(_))
        ));
        ctl.request_client_goaway().expect("请求");
        for _ in 0..4 {
            let _ = Pin::new(&mut io).poll_flush(&mut cx);
            let mut buf = [0u8; 64];
            let _ = Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut buf));
        }
        assert_eq!(obs.emitted().len(), 24 + 9 + 17, "GOAWAY 已交给底层");
        assert_eq!(
            *seen.borrow_and_update(),
            0,
            "flush 未确认就计数，等于谎称 GOAWAY 已上线"
        );
        // flush 恢复：整帧 + flush 双确认 → 恰好 +1
        io.get_mut().expect("底层尚未释放").pending_flush = false;
        let _ = Pin::new(&mut io).poll_flush(&mut cx);
        assert_eq!(*seen.borrow_and_update(), 1, "双确认后必须 +1");
        for _ in 0..4 {
            let _ = Pin::new(&mut io).poll_flush(&mut cx);
        }
        assert_eq!(*seen.borrow_and_update(), 1, "同一帧不得重复计数");

        // ---- server 角色：h2 自己写出的 GOAWAY（适配器只观察，不注入）
        let (mut lower, obs) = mock_io(4096);
        lower.pending_flush = true;
        let mut io = H2KeepaliveIo::new(lower, H2Role::Server);
        let ctl = io.control_handle();
        let mut seen = ctl.subscribe_outbound_goaway();
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        ctl.begin_drain().expect("排空");
        assert!(matches!(
            Pin::new(&mut io).poll_write(&mut cx, &empty_settings()),
            Poll::Ready(Ok(_))
        ));
        let frame = goaway_bytes(u32::MAX, 0, &[]);
        let mut off = 0usize;
        while off < frame.len() {
            match Pin::new(&mut io).poll_write(&mut cx, &frame[off..]) {
                Poll::Ready(Ok(n)) => off += n,
                other => panic!("写 GOAWAY 意外 {other:?}"),
            }
        }
        let _ = Pin::new(&mut io).poll_flush(&mut cx);
        assert_eq!(
            *seen.borrow_and_update(),
            0,
            "server 侧同理：flush 未确认不得计数"
        );
        io.get_mut().expect("底层尚未释放").pending_flush = false;
        let _ = Pin::new(&mut io).poll_flush(&mut cx);
        assert_eq!(
            *seen.borrow_and_update(),
            1,
            "server 侧 h2 自发 GOAWAY 也必须被观察"
        );
        // 观察是纯被动的：出向字节恰好等于 SETTINGS + GOAWAY，适配器没有插任何帧。
        let mut expected = empty_settings();
        expected.extend_from_slice(&frame);
        assert_eq!(
            obs.emitted(),
            expected.as_slice(),
            "不得因观察而多写一个字节"
        );
    })
    .await;
}

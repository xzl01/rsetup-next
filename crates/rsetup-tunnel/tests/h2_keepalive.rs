//! Task 4a: 透明 HTTP/2 连接级 PING 保活适配器测试（协议规范 §6.7.1）。
//!
//! 正常路径使用 `tokio::io::duplex` 作为底层、`tokio::time::pause()` 的虚拟时钟；
//! 故障路径另用可编程底层 `MockIo`（写出预算/写错误/EOF/`poll_shutdown` Pending/
//! Drop 计数）与真实 `tokio::net::TcpStream`（验证底层 fd 真的被释放）。
//! 适配器不启后台任务：窗口推进只能由 `poll_read` / `poll_write` / `poll_flush`
//! 推动，正好覆盖“驱动 h2 Connection 的任务到点必须被唤醒”的要求。
//! 底层对端用手工 poll 读写原始字节，以便逐字校验 HTTP/2 帧边界。

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};

use rsetup_tunnel::h2_keepalive::{
    H2KeepaliveIo, H2Role, KEEPALIVE_WINDOW, MAX_CONSECUTIVE_WINDOW_FAILURES, is_keepalive_timeout,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream, ReadBuf};
use tokio::task::unconstrained;
use tokio::time::advance;

const PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const TYPE_PING: u8 = 6;
const TYPE_SETTINGS: u8 = 4;
const FLAG_ACK: u8 = 0x1;
const WINDOW: Duration = KEEPALIVE_WINDOW;

// ===== 测试基础设施 =====

/// 手动驱动 poll 时使用的空 waker。
///
/// “到点能把挂起的任务叫醒”这一条由
/// `window_deadline_wakes_driver_task_without_background_task` 用真实 task 覆盖。
fn test_waker() -> Waker {
    Waker::noop().clone()
}

/// 非阻塞抽干 tokio 字节的可读部分（不驱动被观测对象）。
fn drain_raw<T>(peer: &mut T, waker: &Waker) -> Vec<u8>
where
    T: AsyncRead + Unpin,
{
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

fn feed_raw<T>(peer: &mut T, waker: &Waker, bytes: &[u8])
where
    T: AsyncWrite + Unpin,
{
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

/// 手工驱动的适配器 + 底层对端字节口。
struct Harness<S, P> {
    io: H2KeepaliveIo<S>,
    peer: P,
    waker: Waker,
    /// 出站方向尚未跳过的连接前导字节数（客户端出站有 24B 前导）。
    out_preface_left: usize,
    /// 已观测但尚未凑成整帧的出站字节。
    out_tail: Vec<u8>,
}

impl<S, P> Harness<S, P>
where
    S: AsyncRead + AsyncWrite + Unpin,
    P: AsyncRead + AsyncWrite + Unpin,
{
    fn new(io: H2KeepaliveIo<S>, peer: P, role: H2Role) -> Self {
        let waker = test_waker();
        Self {
            io,
            peer,
            waker,
            out_preface_left: match role {
                H2Role::Client => PREFACE.len(),
                H2Role::Server => 0,
            },
            out_tail: Vec::new(),
        }
    }

    fn stats(&self) -> rsetup_tunnel::h2_keepalive::KeepaliveStats {
        self.io.stats()
    }

    // ===== 适配器的上层（h2 视角）=====

    fn write(&mut self, bytes: &[u8]) -> Poll<io::Result<usize>> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        Pin::new(&mut self.io).poll_write(&mut cx, bytes)
    }

    /// 断言可立即全部写出（底层缓冲足够时）。
    fn write_all(&mut self, bytes: &[u8]) {
        let mut off = 0usize;
        while off < bytes.len() {
            match self.write(&bytes[off..]) {
                Poll::Ready(Ok(0)) => panic!("底层拒绝写入"),
                Poll::Ready(Ok(n)) => off += n,
                Poll::Ready(Err(e)) => panic!("写出错: {e}"),
                Poll::Pending => panic!(
                    "写出被阻塞（测试未预期）off={off}/{} stats={:?}",
                    bytes.len(),
                    self.stats(),
                ),
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

    /// 读到恰好 n 字节，被阻塞直接 panic。
    fn read_exact(&mut self, n: usize) -> Vec<u8> {
        let mut out = Vec::new();
        while out.len() < n {
            match self.read(n - out.len()) {
                Poll::Ready(Ok(v)) if v.is_empty() => {
                    panic!("意外 EOF，已读 {} / {n}", out.len())
                }
                Poll::Ready(Ok(got)) => out.extend_from_slice(&got),
                Poll::Ready(Err(e)) => panic!("读出错: {e}"),
                Poll::Pending => panic!("读被阻塞（测试未预期）"),
            }
        }
        out
    }

    /// 读直到 Pending，返回累计交付给上层（h2 视角）的字节。
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

    fn read_err(&mut self) -> io::Error {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        let mut buf = [0u8; 16];
        let mut rb = ReadBuf::new(&mut buf);
        match Pin::new(&mut self.io).poll_read(&mut cx, &mut rb) {
            Poll::Ready(Err(e)) => e,
            Poll::Ready(Ok(())) => panic!("期望错误，却读到 {} 字节", rb.filled().len()),
            Poll::Pending => panic!("期望错误，却返回 Pending"),
        }
    }

    fn flush(&mut self) -> Poll<io::Result<()>> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        Pin::new(&mut self.io).poll_flush(&mut cx)
    }

    fn shutdown(&mut self) -> Poll<io::Result<()>> {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        Pin::new(&mut self.io).poll_shutdown(&mut cx)
    }

    /// 只通过 poll_flush / poll_read 推动调度（模拟空闲 h2 Connection 的任务）。
    fn pump(&mut self) {
        let _ = self.flush();
        let _ = self.read(256);
    }

    fn shutdown_blocking(&mut self) {
        for _ in 0..16 {
            match self.shutdown() {
                Poll::Ready(Ok(())) => return,
                Poll::Ready(Err(e)) => panic!("shutdown 出错: {e}"),
                Poll::Pending => {}
            }
        }
        panic!("shutdown 长时间 Pending");
    }

    // ===== 底层对端（模拟 h2 或任意 TCP 对端）=====

    /// 抽干底层可读原始字节（含前导，用于逐字校验）。
    fn drain_peer_raw(&mut self) -> Vec<u8> {
        self.drain_out().0
    }

    /// 出向字节唯一的“离开 harness”出口：同时推进前导剩余量，
    /// 这样无论谁先抽干，`out_frames` 都不会重复剥离前导。
    fn drain_out(&mut self) -> (Vec<u8>, usize) {
        let raw = drain_raw(&mut self.peer, &self.waker);
        let skip = self.out_preface_left.min(raw.len());
        self.out_preface_left -= skip;
        (raw, skip)
    }

    /// 抽干底层出向字节，同时把它们保留给后续 `out_frames` 解析。
    /// 用于“中途看一眼但不错过字节”的场景。
    fn drain_peer_keep(&mut self) -> Vec<u8> {
        let (raw, skip) = self.drain_out();
        self.out_tail.extend_from_slice(&raw[skip..]);
        raw[skip..].to_vec()
    }

    /// 抽干底层出向字节、剥掉连接前导，凑成完整帧后返回。
    fn out_frames(&mut self) -> Vec<Frame> {
        let (raw, skip) = self.drain_out();
        self.out_tail.extend_from_slice(&raw[skip..]);
        take_frames(&mut self.out_tail)
    }

    /// 出向的自发 PING（token, 是否 ACK）。
    fn out_probes(&mut self) -> Vec<Probe> {
        ping_probes(&self.out_frames())
    }

    fn last_out_token(&mut self) -> [u8; 8] {
        let probes = self.out_probes();
        let last = probes
            .last()
            .unwrap_or_else(|| panic!("没有观测到自发 PING: {probes:?}"));
        let mut out = [0u8; 8];
        out.copy_from_slice(&last.token);
        out
    }

    fn feed_peer(&mut self, bytes: &[u8]) {
        feed_raw(&mut self.peer, &self.waker, bytes);
    }

    /// 底层对端是否已观察到 EOF（适配器关闭底层 io 的判据）。
    fn peer_sees_eof(&mut self) -> bool {
        let waker = self.waker.clone();
        let mut cx = Context::from_waker(&waker);
        let mut tmp = [0u8; 8];
        let mut rb = ReadBuf::new(&mut tmp);
        match Pin::new(&mut self.peer).poll_read(&mut cx, &mut rb) {
            Poll::Ready(Ok(())) => rb.filled().is_empty(),
            Poll::Ready(Err(_)) => true,
            Poll::Pending => false,
        }
    }
}

type Dual = Harness<DuplexStream, DuplexStream>;

fn dual(cap: usize, role: H2Role) -> Dual {
    let (a, b) = tokio::io::duplex(cap);
    Harness::new(H2KeepaliveIo::new(a, role), b, role)
}

/// 客户端角色：出站含 24B 前导。
fn client() -> Dual {
    dual(256 * 1024, H2Role::Client)
}

/// 服务端角色：入站含 24B 前导。
fn server() -> Dual {
    dual(256 * 1024, H2Role::Server)
}

/// 推进 n 个窗口并推动 poll，使调度真实发生。
async fn tick(h: &mut Dual, windows: u32) {
    for _ in 0..windows {
        advance(WINDOW).await;
        h.pump();
    }
}

// ===== HTTP/2 帧工具 =====

#[derive(Debug, Clone, PartialEq, Eq)]
struct Frame {
    ftype: u8,
    flags: u8,
    stream: u32,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Probe {
    token: Vec<u8>,
    ack: bool,
}

fn push_frame(out: &mut Vec<u8>, ftype: u8, flags: u8, stream: u32, payload: &[u8]) {
    let len = payload.len() as u32;
    out.push((len >> 16) as u8);
    out.push((len >> 8) as u8);
    out.push(len as u8);
    out.push(ftype);
    out.push(flags);
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
}

fn encode_frame(ftype: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    push_frame(&mut out, ftype, flags, stream, payload);
    out
}

fn frame(ftype: u8, payload: &[u8]) -> Frame {
    Frame {
        ftype,
        flags: 0,
        stream: 0,
        payload: payload.to_vec(),
    }
}

fn encode(f: &Frame) -> Vec<u8> {
    encode_frame(f.ftype, f.flags, f.stream, &f.payload)
}

fn data_frame(payload: &[u8]) -> Frame {
    Frame {
        ftype: 0,
        flags: 0,
        stream: 1,
        payload: payload.to_vec(),
    }
}

/// 真实 h2 状态机的第一个出站帧：空 SETTINGS（RFC9113 §3.4）。
/// 本组件在首条 SETTINGS 完整写出之前禁止注入任何自发帧，fixture 必须先写它。
fn settings_frame() -> Vec<u8> {
    encode_frame(TYPE_SETTINGS, 0, 0, &[])
}

fn ping_bytes(token: &[u8], ack: bool) -> Vec<u8> {
    encode_frame(TYPE_PING, if ack { FLAG_ACK } else { 0 }, 0, token)
}

/// 严格 HTTP/2 帧解析；有残尾即 panic，用于证明“逐字透明 + 只在帧边界插帧”。
fn parse_frames(bytes: &[u8]) -> Vec<Frame> {
    let mut buf = bytes.to_vec();
    let frames = take_frames(&mut buf);
    assert!(
        buf.is_empty(),
        "解析后仍有 {} 个残缺字节，说明帧边界被破坏",
        buf.len()
    );
    frames
}

/// 尽可能解析完整帧，残尾留在 buf。
fn take_frames(buf: &mut Vec<u8>) -> Vec<Frame> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while buf.len() - off >= 9 {
        let len =
            ((buf[off] as usize) << 16) | ((buf[off + 1] as usize) << 8) | buf[off + 2] as usize;
        if buf.len() - off - 9 < len {
            break;
        }
        let ftype = buf[off + 3];
        let flags = buf[off + 4];
        let stream = ((buf[off + 5] as u32 & 0x7f) << 24)
            | ((buf[off + 6] as u32) << 16)
            | ((buf[off + 7] as u32) << 8)
            | buf[off + 8] as u32;
        let start = off + 9;
        out.push(Frame {
            ftype,
            flags,
            stream,
            payload: buf[start..start + len].to_vec(),
        });
        off = start + len;
    }
    buf.drain(..off);
    out
}

fn ping_probes(frames: &[Frame]) -> Vec<Probe> {
    frames
        .iter()
        .filter(|f| f.ftype == TYPE_PING)
        .map(|f| Probe {
            token: f.payload.clone(),
            ack: f.flags & FLAG_ACK != 0,
        })
        .collect()
}

fn hex_of(bytes: &[u8]) -> String {
    let mut out = String::new();
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

// ===== 逐字透明 =====

#[tokio::test]
async fn client_outbound_starts_with_verbatim_preface() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        let raw = h.drain_peer_raw();
        assert!(
            raw.starts_with(PREFACE.as_slice()),
            "客户端出站前导必须逐字原样: {raw:?}"
        );
        // 前导之后要么没有字节，要么是完整帧
        parse_frames(&raw[PREFACE.len()..]);
    })
    .await;
}

#[tokio::test]
async fn outbound_frames_are_byte_exact_without_window_advance() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        let frames = vec![
            frame(TYPE_SETTINGS, &[0, 0, 0, 0, 0, 0, 0, 5]),
            data_frame(b"hello tunnel"),
            Frame {
                ftype: 1,
                flags: 0x25,
                stream: 3,
                payload: vec![9u8; 13],
            },
            frame(8, &[]),
            frame(7, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]),
        ];
        let mut want = PREFACE.to_vec();
        for f in &frames {
            want.extend_from_slice(&encode(f));
        }
        h.write_all(&want);
        let got = h.drain_peer_raw();
        assert_eq!(got, want, "未到窗口边界时不得插入 PING，且必须逐字透传");
        assert_eq!(parse_frames(&got[PREFACE.len()..]), frames);
    })
    .await;
}

#[tokio::test]
async fn single_byte_writes_keep_stream_intact() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        let mut want = PREFACE.to_vec();
        want.extend_from_slice(&settings_frame());
        want.extend_from_slice(&encode(&data_frame(&[0u8; 300])));
        for byte in &want {
            h.write_all(&[*byte]);
        }
        let got = h.drain_peer_raw();
        assert!(
            has_injected_probe(&got),
            "窗口边界应注入自投 PING，否则这个用例没有覆盖注入: {got:?}"
        );
        assert_eq!(
            strip_injected_probes(&got),
            want,
            "剔除自投 PING 后，逐字节写出的结果必须与写出内容逐字一致"
        );
    })
    .await;
}

#[tokio::test]
async fn inbound_preface_and_frames_are_verbatim_for_server() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = server();
        let frames = vec![
            frame(TYPE_SETTINGS, &[0, 0, 4, 0, 0, 0, 10]),
            data_frame(b"pong-ish payload"),
            frame(8, &[]),
        ];
        let mut raw = PREFACE.to_vec();
        for f in &frames {
            raw.extend_from_slice(&encode(f));
        }
        // 聚合投喂
        h.feed_peer(&raw);
        let mut got = Vec::new();
        while got.len() < raw.len() {
            got.extend_from_slice(&h.read_exact(raw.len() - got.len()));
        }
        assert_eq!(got, raw, "服务端入站前导与全部帧必须逐字交付");
        assert!(got.starts_with(PREFACE.as_slice()));
        assert_eq!(parse_frames(&got[PREFACE.len()..]), frames);
    })
    .await;
}

#[tokio::test]
async fn inbound_fragmented_delivery_is_still_verbatim() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = server();
        let frames = vec![
            frame(TYPE_SETTINGS, &[0, 0, 0, 0, 0, 0, 0, 1]),
            data_frame(b"abcdefghijklmnopqrstuvwxyz"),
        ];
        let mut raw = PREFACE.to_vec();
        for f in &frames {
            raw.extend_from_slice(&encode(f));
        }
        let mut got = Vec::new();
        let cuts = [1usize, 5, 13, raw.len()];
        let mut off = 0usize;
        for cut in cuts {
            let end = cut.min(raw.len());
            if end > off {
                h.feed_peer(&raw[off..end]);
                off = end;
            }
            got.extend_from_slice(&h.read_until_pending());
        }
        assert_eq!(got, raw, "碎片到达也必须逐字一致");
    })
    .await;
}

#[tokio::test]
async fn client_inbound_has_no_preface() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        let f = frame(TYPE_SETTINGS, &[0, 0, 0, 0, 0, 0, 0, 3]);
        h.feed_peer(&encode(&f));
        let got = h.read_exact(17);
        assert_eq!(got, encode(&f), "客户端入站首帧不得被当成前导吞掉");
    })
    .await;
}

// ===== 帧边界注入 =====

#[tokio::test]
async fn probe_emitted_at_frame_boundary_with_expected_shape() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let frames = h.out_frames();
        let probes = ping_probes(&frames);
        assert_eq!(
            probes.len(),
            1,
            "帧边界应注入一个自发 PING，实际 {frames:?}"
        );
        let p = frames
            .iter()
            .find(|f| f.ftype == TYPE_PING)
            .expect("PING 帧");
        assert_eq!(p.flags, 0, "自发 PING 是请求帧，flags 必须为 0");
        assert_eq!(p.stream, 0, "PING 必须属于连接控制流 0");
        assert_eq!(p.payload.len(), 8, "PING opaque data 固定 8B");
        assert_ne!(p.payload.as_slice(), &[0u8; 8], "不得使用固定/全零 token");
    })
    .await;
}

#[tokio::test]
async fn probe_is_never_inserted_inside_a_frame() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let first = h.out_frames();
        assert_eq!(ping_probes(&first).len(), 1, "边界上应有一个 PING");

        // 帧头写出到一半时推进窗口：PING 只能等帧边界
        let payload: Vec<u8> = b"0123456789abcdef0123456789abcdef".to_vec();
        let encoded = encode(&data_frame(&payload));
        h.write_all(&encoded[..5]);
        let mid1 = h.drain_peer_keep();
        assert_eq!(
            mid1,
            encoded[..5],
            "帧中途除了刚写出的 5 个字节，不得有任何额外输出"
        );
        tick(&mut h, 1).await;
        let mid2 = h.drain_peer_keep();
        assert!(
            mid2.is_empty(),
            "帧头写了一半就推进窗口，PING 绝不允许插进帧内: {mid2:?}"
        );

        // 帧写完后才允许出现
        h.write_all(&encoded[5..]);
        h.pump();
        let frames = h.out_frames();
        let pos_data = frames
            .iter()
            .position(|f| f.ftype == 0)
            .unwrap_or_else(|| panic!("DATA 帧必须完整: {frames:?}"));
        assert_eq!(
            frames[pos_data].payload, payload,
            "DATA payload 不得被 PING 打断"
        );
        let pos_ping = frames
            .iter()
            .rposition(|f| f.ftype == TYPE_PING)
            .expect("窗口推进后应在边界发 PING");
        assert!(pos_ping > pos_data, "PING 只能出现在完整 DATA 帧之后");
    })
    .await;
}

#[tokio::test]
async fn large_frame_payload_is_streamed_not_buffered() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.out_frames();
        let big: Vec<u8> = (0..(96 * 1024)).map(|i| (i % 251) as u8).collect();
        let want = encode(&data_frame(&big));
        for chunk in want.chunks(8 * 1024) {
            h.write_all(chunk);
        }
        let mut got = Vec::new();
        got.extend_from_slice(&h.drain_peer_raw());
        assert_eq!(got, want, "大帧必须流式透传且不被截断或缓冲改写");
        let frames = parse_frames(&got);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload.len(), big.len());
    })
    .await;
}

// ===== 窗口与 token =====

#[tokio::test]
async fn token_changes_between_windows() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let t1 = h.last_out_token();
        tick(&mut h, 1).await;
        let t2 = h.last_out_token();
        tick(&mut h, 1).await;
        let t3 = h.last_out_token();
        assert_ne!(t1, t2, "跨窗口 token 必须不同");
        assert_ne!(t2, t3, "跨窗口 token 必须不同");
        assert_ne!(t1, t3);
        let st = h.stats();
        assert!(st.windows_opened >= 3, "应开满 3 个窗口: {st:?}");
        assert_eq!(st.probes_sent, 3, "每窗口至多一个在途 PING");
    })
    .await;
}

#[tokio::test]
async fn at_most_one_probe_per_window_no_backlog() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        // 先取走首窗探测
        let first = h.out_probes();
        assert_eq!(first.len(), 1, "首窗应恰好一个探测");
        let mut tokens: Vec<Vec<u8>> = first.iter().map(|p| p.token.clone()).collect();
        // 每窗口回一个匹配 ACK 保持健康；每窗口都必须只出现一个新探测
        for i in 1..=4u32 {
            tick(&mut h, 1).await;
            let probes = h.out_probes();
            assert_eq!(
                probes.len(),
                1,
                "第 {i} 个窗口应恰好一个新探测，过期窗口不得补发积压"
            );
            assert!(
                probes.iter().all(|p| !p.ack),
                "自投帧必须是 PING 请求（flags=0）"
            );
            tokens.push(probes[0].token.clone());
            let mut token = [0u8; 8];
            token.copy_from_slice(&probes[0].token);
            h.feed_peer(&ping_bytes(&token, true));
            h.pump();
            assert_eq!(h.stats().consecutive_failures, 0, "匹配 ACK 后失败数应归零");
        }
        let st = h.stats();
        assert_eq!(st.probes_sent, st.windows_opened, "探测数不得超过窗口数");
        let mut uniq = tokens.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), tokens.len(), "各窗口 token 必须互不相同");
    })
    .await;
}

#[tokio::test]
async fn blocked_write_counts_window_and_no_backfill() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        // 小缓冲：写出很快阻塞
        let mut h = dual(16, H2Role::Client);
        // 真实 h2 会紧接前导写 SETTINGS（§3.4）；缓冲 16B 连这 24+9B 都装不下
        let mut want = PREFACE.to_vec();
        want.extend_from_slice(&settings_frame());
        let mut off = 0usize;
        while off < want.len() {
            match h.write(&want[off..]) {
                Poll::Ready(Ok(n)) => off += n,
                Poll::Ready(Err(e)) => panic!("写前导/SETTINGS 失败: {e}"),
                Poll::Pending => break,
            }
        }
        assert!(off < want.len(), "缓冲应已写满");
        h.drain_peer_raw();

        // 写阻塞中过去两个窗口：计时不停
        advance(WINDOW).await;
        h.pump();
        advance(WINDOW).await;
        h.pump();
        let st = h.stats();
        assert_eq!(st.consecutive_failures, 2, "发送阻塞也计入窗口，不停止计时");
        assert_eq!(st.probes_sent, 0, "未到帧边界时不得写出探测");

        // 腾空缓冲后只允许当前窗口一个在途 PING，不补发历史窗口。
        // 缓冲只有 16B：边抽干对端边补写，直到前导 + SETTINGS 全部上线。
        let mut off2 = off;
        for _ in 0..16 {
            if off2 >= want.len() {
                break;
            }
            match h.write(&want[off2..]) {
                Poll::Ready(Ok(n)) => off2 += n,
                Poll::Ready(Err(e)) => panic!("补写失败: {e}"),
                Poll::Pending => {
                    // 保留式抽干：探测可能已在边界写出，不能当“腾缓冲”丢掉
                    h.drain_peer_keep();
                }
            }
        }
        assert_eq!(off2, want.len(), "缓冲应能逐步腾空写完前导+SETTINGS");
        // 小缓冲下探测可能只写出一半：反复抽干对端并推动，直到当前窗口探测完整出现
        let mut probes = h.out_probes();
        for _ in 0..8 {
            if !probes.is_empty() {
                break;
            }
            h.pump();
            probes = h.out_probes();
        }
        assert_eq!(
            probes.len(),
            1,
            "过期窗口不得补发，只允许当前窗口一个: {probes:?}"
        );
        assert_eq!(h.stats().probes_sent, 1, "至多当前逻辑在途");
    })
    .await;
}

// ===== ACK 匹配 =====

#[tokio::test]
async fn matching_ack_clears_failures_and_is_intercepted() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.last_out_token();
        tick(&mut h, 1).await;
        assert_eq!(h.stats().consecutive_failures, 1, "首窗未 ACK 计一次失败");
        let t2 = h.last_out_token();

        // 当前窗口 token 的 ACK：清除失败，并被拦截不交 h2
        h.feed_peer(&ping_bytes(&t2, true));
        let delivered = h.read_until_pending();
        assert!(
            delivered.is_empty(),
            "匹配 ACK 必须被拦截消费，不交 h2: {delivered:?}"
        );
        let st = h.stats();
        assert_eq!(st.consecutive_failures, 0, "匹配 ACK 必须清零连续失败");
        assert_eq!(st.acked_windows, 1);
        assert_eq!(st.intercepted_acks, 1);
        assert!(!st.failed);

        // 之后未 ACK 的窗口重新计数；已 ACK 的窗口不重复计失败
        tick(&mut h, 2).await;
        assert_eq!(
            h.stats().consecutive_failures,
            1,
            "刚 ACK 的窗口不得计失败，只结算之后到期的窗口"
        );
        tick(&mut h, 1).await;
        assert_eq!(h.stats().consecutive_failures, 2);
        assert!(!h.stats().failed, "连续失败重新计数未满 3 不得关闭");
    })
    .await;
}

#[tokio::test]
async fn late_ack_does_not_credit_and_is_forwarded() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let t1 = h.last_out_token();
        tick(&mut h, 1).await;
        assert_eq!(h.stats().consecutive_failures, 1);

        // 上一窗口的迟到 ACK：不得刷新，且必须原样交 h2
        h.feed_peer(&ping_bytes(&t1, true));
        let delivered = h.read_until_pending();
        assert_eq!(
            delivered,
            ping_bytes(&t1, true),
            "迟到 ACK 必须原样交给 h2，不能拦截"
        );
        let st = h.stats();
        assert_eq!(st.consecutive_failures, 1, "迟到 ACK 不算成功");
        assert_eq!(st.acked_windows, 0);
        assert_eq!(st.intercepted_acks, 0);
    })
    .await;
}

#[tokio::test]
async fn duplicate_ack_does_not_refresh() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        tick(&mut h, 1).await;
        let t2 = h.last_out_token();
        assert_eq!(h.stats().consecutive_failures, 1);

        h.feed_peer(&ping_bytes(&t2, true));
        assert!(h.read_until_pending().is_empty(), "首个匹配 ACK 被拦截");
        assert_eq!(h.stats().consecutive_failures, 0);
        assert_eq!(h.stats().acked_windows, 1);

        // 重复 ACK 不得再次刷新
        h.feed_peer(&ping_bytes(&t2, true));
        let dup = h.read_until_pending();
        assert_eq!(dup, ping_bytes(&t2, true), "重复 ACK 原样交 h2");
        let st = h.stats();
        assert_eq!(st.acked_windows, 1, "重复 ACK 不得计入成功");
        assert_eq!(st.intercepted_acks, 1);
    })
    .await;
}

#[tokio::test]
async fn foreign_ack_and_peer_ping_do_not_credit() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        tick(&mut h, 2).await;
        let fails = h.stats().consecutive_failures;
        assert_eq!(fails, 2);
        let cur = h.last_out_token();

        // 陌生 token 的 ACK：全零，以及 h2 内部保留的 SHUTDOWN token
        // （`h2-0.4.20/src/frame/ping.rs:16`，并非全零）
        for token in [[0u8; 8], H2_SHUTDOWN_TOKEN] {
            h.feed_peer(&ping_bytes(&token, true));
            let foreign = h.read_until_pending();
            assert_eq!(
                foreign,
                ping_bytes(&token, true),
                "陌生 ACK 不得拦截，交 h2 自行忽略"
            );
            assert_eq!(h.stats().consecutive_failures, fails, "陌生 ACK 不算成功");
        }

        // 携带本端 token 但未置 ACK 标志（PING 请求）也不算 ACK
        h.feed_peer(&ping_bytes(&cur, false));
        let echoed = h.read_until_pending();
        assert_eq!(echoed, ping_bytes(&cur, false));
        assert_eq!(h.stats().consecutive_failures, fails, "PING 请求不是 ACK");
        assert_eq!(h.stats().intercepted_acks, 0);
    })
    .await;
}

#[tokio::test]
async fn peer_ping_request_is_forwarded_without_extra_ack() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.out_probes();

        let token = [9u8, 8, 7, 6, 5, 4, 3, 2];
        h.feed_peer(&ping_bytes(&token, false));
        let delivered = h.read_until_pending();
        assert_eq!(
            delivered,
            ping_bytes(&token, false),
            "对端 PING 请求必须原样交 h2 应答"
        );
        // 适配器自身不得再产生 ACK（h2 是唯一应答者）
        for p in h.out_probes() {
            assert!(!p.ack, "适配器不得替 h2 回应 PING ACK");
        }
    })
    .await;
}

/// h2 0.4.20 内部保留的两种连接级 PING payload，取自
/// `h2-0.4.20/src/frame/ping.rs:16-17`（`SHUTDOWN_PAYLOAD` / `USER_PAYLOAD`，
/// 未开 `unstable` feature 时不对外可见，只能在此复制字面常量）。
const H2_SHUTDOWN_TOKEN: [u8; 8] = [0x0b, 0x7b, 0xa2, 0xf0, 0x8b, 0x9b, 0xfe, 0x54];
const H2_USER_TOKEN: [u8; 8] = [0x3b, 0x7c, 0xdb, 0x7a, 0x0b, 0x87, 0x16, 0xb4];

/// 陌生 token 的连接级 PING 请求/ACK 必须逐字透传，适配器一律不拦截、不代答。
///
/// **这是手工构造的 wire 帧 mock**：payload 用的是 h2 内部保留常量的字面值，
/// 但帧由测试自己写出，本用例**没有**驱动 h2 库自身的 shutdown/keepalive 流程，
/// 因此它只证明“非本适配器 token 不改写、不拦截”，不能当作 h2 自身 PING 交互
/// （含其 GOAWAY/shutdown 时序）的验收；那部分未覆盖，见测试报告 §7。
#[tokio::test]
async fn foreign_token_ping_pair_is_forwarded() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.out_probes();
        for (name, token) in [
            ("h2 SHUTDOWN 保留 token", H2_SHUTDOWN_TOKEN),
            ("h2 USER 保留 token", H2_USER_TOKEN),
            ("全零 token", [0u8; 8]),
        ] {
            let mut want = ping_bytes(&token, false);
            want.extend_from_slice(&ping_bytes(&token, true));
            h.feed_peer(&want);
            let got = h.read_exact(want.len());
            assert_eq!(got, want, "{name}：请求与 ACK 都必须原样交付");
            assert_eq!(
                h.stats().intercepted_acks,
                0,
                "{name}：非本适配器 token 不得拦截"
            );
            assert_eq!(h.stats().acked_windows, 0, "{name}：不得 credit");
        }
    })
    .await;
}

#[tokio::test]
async fn business_traffic_cannot_replace_ping_ack() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.out_probes();
        // 双向大量业务帧，但对端从不回 PING ACK
        let traffic = encode(&data_frame(&vec![7u8; 4096]));
        let mut inflow = Vec::new();
        push_frame(&mut inflow, TYPE_SETTINGS, 0, 0, &[0, 0, 0, 0, 0, 0, 0, 1]);
        for _ in 0..32 {
            h.write_all(&traffic);
            h.feed_peer(&inflow);
            let _ = h.read_until_pending();
            let _ = h.out_probes();
            advance(Duration::from_millis(500)).await;
            h.pump();
        }
        let st = h.stats();
        assert!(
            st.acked_windows == 0 && st.consecutive_failures >= 1,
            "业务流量不能代替 ACK: {st:?}"
        );
        // 继续到第 3 个窗口即判死链
        let mut guard = 0;
        while h.stats().consecutive_failures < MAX_CONSECUTIVE_WINDOW_FAILURES && guard < 32 {
            advance(WINDOW).await;
            h.pump();
            guard += 1;
        }
        let err = h.read_err();
        assert!(is_keepalive_timeout(&err), "应报保活超时，实际 {err:?}");
    })
    .await;
}

// ===== 死链关闭 =====

#[tokio::test]
async fn three_windows_without_matching_ack_close_underlying_io() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        for i in 1..=MAX_CONSECUTIVE_WINDOW_FAILURES {
            tick(&mut h, 1).await;
            if i < MAX_CONSECUTIVE_WINDOW_FAILURES {
                assert!(!h.stats().failed, "第 {i} 次失败不应关闭连接");
            }
        }
        let st = h.stats();
        assert_eq!(st.consecutive_failures, MAX_CONSECUTIVE_WINDOW_FAILURES);
        assert!(st.failed, "连续 3 窗口必须判死链");

        let err = h.read_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "错误类型: {err:?}");
        assert!(is_keepalive_timeout(&err), "错误须可辨识: {err:?}");
        // 读侧持续报同一错误
        assert!(is_keepalive_timeout(&h.read_err()));
        // 写侧同样终止
        match h.write(b"x") {
            Poll::Ready(Err(e)) => assert!(is_keepalive_timeout(&e), "写侧须报同一失败: {e:?}"),
            other => panic!("写侧应报死链错误，实际 {other:?}"),
        }
        // 底层被真正关闭：先把已缓冲字节读空，再要求对端观察到 EOF
        let mut saw_eof = false;
        for _ in 0..8 {
            h.drain_peer_raw();
            if h.peer_sees_eof() {
                saw_eof = true;
                break;
            }
        }
        assert!(saw_eof, "判死链必须关闭整个底层 io");
    })
    .await;
}

#[tokio::test]
async fn windows_only_advance_when_polled() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.out_probes();
        // 时钟已过 3 个窗口但没有任何 poll：不得凭空发帧
        advance(WINDOW * 3).await;
        let raw = h.drain_peer_raw();
        assert!(raw.is_empty(), "未被 poll 时不得发帧，实际 {raw:?}");
        // 一旦被 poll：立刻按调度结算并判死链
        h.pump();
        assert!(h.stats().failed, "poll 后须补算已过的 3 个窗口");
        assert!(is_keepalive_timeout(&h.read_err()));
    })
    .await;
}

// ===== shutdown =====

#[tokio::test]
async fn shutdown_stops_probing_and_backfill() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.out_probes();
        // 等一个窗口使探测与调度真实发生
        tick(&mut h, 1).await;
        let _ = h.out_probes();

        h.shutdown_blocking();
        let after = h.drain_peer_raw();
        assert!(after.is_empty(), "关闭后不得再写帧: {after:?}");

        // 长时间推进：不得补发历史窗口探测，也不得改判死链
        advance(WINDOW * 6).await;
        h.pump();
        let late = h.drain_peer_raw();
        assert!(late.is_empty(), "shutdown 后不得补发探测: {late:?}");
        let st = h.stats();
        assert!(st.stopped, "stats 应反映已关闭: {st:?}");
        assert!(!st.failed, "关闭后的窗口不再判死链");
    })
    .await;
}

#[tokio::test]
async fn shutdown_server_side_stops_probing() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = server();
        h.feed_peer(PREFACE);
        let got = h.read_exact(PREFACE.len());
        assert_eq!(got, PREFACE.as_slice());
        h.write_all(&settings_frame());
        h.pump();
        let probes = h.out_probes();
        assert_eq!(probes.len(), 1, "服务端出站首窗也应发一个探测");
        h.shutdown_blocking();
        assert!(h.stats().stopped);
        advance(WINDOW * 3).await;
        h.pump();
        assert!(!h.stats().failed, "已关闭连接不再判死链");
        assert!(h.drain_peer_raw().is_empty(), "关闭后不得再有出向字节");
    })
    .await;
}

// ===== 定时器唤醒（真实任务）=====

#[tokio::test(flavor = "current_thread")]
async fn window_deadline_wakes_driver_task_without_background_task() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let (inner, mut peer) = tokio::io::duplex(64 * 1024);
        let mut io = H2KeepaliveIo::new(inner, H2Role::Client);
        let waker = test_waker();

        // 先把前导 + 首条 SETTINGS（§3.4）写到帧边界
        let want: Vec<u8> = PREFACE.iter().copied().chain(settings_frame()).collect();
        let mut off = 0usize;
        while off < want.len() {
            let mut cx = Context::from_waker(&waker);
            match Pin::new(&mut io).poll_write(&mut cx, &want[off..]) {
                Poll::Ready(Ok(n)) => off += n,
                Poll::Ready(Err(e)) => panic!("写前导失败: {e}"),
                Poll::Pending => panic!("缓冲充足不应 Pending"),
            }
        }

        // 任务模拟空闲的 h2 Connection：只挂起在适配器的 poll_read 上
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
        let first = ping_probes(&take_frames(&mut raw));
        assert_eq!(first.len(), 1, "首窗探测应由任务的一次 poll 发出");

        // 推进一个窗口：适配器不启后台 task，必须由定时器唤醒该任务才能发出下一探测
        advance(WINDOW).await;
        park_driver().await;
        let mut raw2 = drain_raw(&mut peer, &waker);
        strip_preface(&mut raw2);
        let second = ping_probes(&take_frames(&mut raw2));
        assert!(
            !second.is_empty(),
            "窗口到点必须唤醒驱动 h2 Connection 的任务并发出探测（定时器驱动，无后台 task）"
        );
        assert!(
            second.iter().all(|p| !p.ack && p.token.len() == 8),
            "唤醒后发出的必须是自投 PING 请求: {second:?}"
        );
        assert_ne!(
            first[0].token, second[0].token,
            "唤醒后的探测 token 必须与上一窗不同"
        );
        assert!(
            second
                .iter()
                .map(|p| p.token.clone())
                .collect::<std::collections::HashSet<_>>()
                .len()
                == second.len(),
            "同一批探测的 token 也必须互不重复: {second:?}"
        );
        driver.abort();
    })
    .await;
}

// ===== 诊断 API 不得泄密 =====

#[tokio::test]
async fn debug_and_stats_never_expose_token() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let token = h.last_out_token();
        let dbg = format!("{:?}", h.io);
        let hex = hex_of(&token);
        assert!(!dbg.contains(&hex), "Debug 不得含 token 十六进制: {dbg}");
        assert!(
            !dbg.contains(&hex.to_uppercase()),
            "Debug 不得含 token 大写十六进制: {dbg}"
        );
        let raw = String::from_utf8_lossy(&token);
        if raw.chars().all(|c| c.is_ascii_graphic()) {
            assert!(
                !dbg.contains(&raw.to_string()),
                "Debug 不得含 token 原文: {dbg}"
            );
        }
        let st = h.stats();
        let st_dbg = format!("{st:?}");
        assert!(!st_dbg.contains(&hex), "stats Debug 不得含 token: {st_dbg}");
    })
    .await;
}

// ===== 真 h2 互操作 =====

/// `TapIo::new` 的返回：tap 本体 + 出向日志 + 入向日志。
type TapIoNew<T> = (TapIo<T>, Arc<Mutex<Vec<u8>>>, Arc<Mutex<Vec<u8>>>);

/// 记录双向原始字节的 tap，用于证明注入/拦截发生在正确的一侧。
struct TapIo<T> {
    inner: T,
    tx: Arc<Mutex<Vec<u8>>>,
    rx: Arc<Mutex<Vec<u8>>>,
}

impl<T> TapIo<T> {
    fn new(inner: T) -> TapIoNew<T> {
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

/// 只剔除“结构完全合法的自投 PING 请求”（len=8、type=6、flags=0、stream=0），
/// 其余字节逐字保留；用于证明除注入点之外完全透明。
fn strip_injected_probes(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut off = 0usize;
    while off < raw.len() {
        if raw.len() - off >= 17
            && raw[off] == 0
            && raw[off + 1] == 0
            && raw[off + 2] == 8
            && raw[off + 3] == TYPE_PING
            && raw[off + 4] == 0
            && raw[off + 5..off + 9] == [0, 0, 0, 0]
        {
            off += 17;
            continue;
        }
        out.push(raw[off]);
        off += 1;
    }
    out
}

fn has_injected_probe(raw: &[u8]) -> bool {
    strip_injected_probes(raw).len() != raw.len()
}

fn drain_log(log: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
    std::mem::take(&mut *log.lock().unwrap())
}

fn strip_preface(buf: &mut Vec<u8>) {
    if buf.starts_with(PREFACE.as_slice()) {
        buf.drain(..PREFACE.len());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn real_h2_stack_acks_injected_probe_and_survives_windows() {
    // tokio 的协作调度预算会在单个任务内累计若干次 poll 后强制挂起任务，
    // 这与本组件无关；这些测试用手动 Context 驱动适配器，故显式关闭协作预算。
    unconstrained(async move {
        tokio::time::pause();
        let (a_end, b_end) = tokio::io::duplex(256 * 1024);

        // 客户端侧：h2 client -> tap(上层) -> H2KeepaliveIo -> tap(底层) -> duplex
        let (a_wire, a_wire_tx, a_wire_rx) = TapIo::new(a_end);
        let a_keep = H2KeepaliveIo::new(a_wire, H2Role::Client);
        let (a_up, _a_up_tx, a_up_rx) = TapIo::new(a_keep);
        let (mut sender, conn) = h2::client::handshake(a_up).await.expect("h2 client");
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        // 服务端侧：h2 server 由任务驱动，其下同样垫一层适配器
        let (b_wire, _b_wire_tx, _b_wire_rx) = TapIo::new(b_end);
        let b_keep = H2KeepaliveIo::new(b_wire, H2Role::Server);
        let srv = tokio::spawn(async move {
            let mut server = h2::server::handshake(b_keep).await.expect("h2 server");
            let mut served = 0usize;
            // 一直服务到客户端结束连接（不预设次数，避免过早退出导致对端 broken pipe）
            while let Some(item) = server.accept().await {
                let (mut req, mut respond) = match item {
                    Ok(item) => item,
                    Err(_) => break,
                };
                let mut body = Vec::new();
                while let Some(d) = req.body_mut().data().await {
                    body.extend_from_slice(&d.expect("body data"));
                }
                assert_eq!(body, b"ping-body", "h2 业务体必须完好");
                let mut stream = respond
                    .send_response(http::Response::new(()), false)
                    .expect("send_response");
                stream
                    .send_data(bytes::Bytes::from_static(b"pong"), true)
                    .expect("send data");
                served += 1;
            }
            served
        });

        async fn roundtrip(sender: &mut h2::client::SendRequest<bytes::Bytes>) -> Vec<u8> {
            let req = http::Request::builder()
                .method(http::Method::POST)
                .uri("http://rsetup.local/tunnel.RTunnel/OpenControl")
                .body(())
                .expect("request");
            let (resp, mut send_stream) = sender.send_request(req, false).expect("send_request");
            send_stream
                .send_data(bytes::Bytes::from_static(b"ping-body"), true)
                .expect("send body");
            let mut resp = resp.await.expect("response");
            let mut body = Vec::new();
            while let Some(d) = resp.body_mut().data().await {
                body.extend_from_slice(&d.expect("resp data"));
            }
            body
        }

        assert_eq!(
            roundtrip(&mut sender).await,
            b"pong",
            "h2 请求/响应必须完好"
        );

        // 底层出向应出现自投 PING；对端 h2 必须自动 ACK
        yield_ticks(6).await;
        let mut wire = drain_log(&a_wire_tx);
        strip_preface(&mut wire);
        let probes = ping_probes(&take_frames(&mut wire));
        assert!(!probes.is_empty(), "适配器必须注入自投 PING");
        let own: Vec<_> = probes.iter().filter(|p| !p.ack).collect();
        assert!(!own.is_empty(), "出向必须出现自投 PING 请求");
        assert_eq!(
            own[0].token.len(),
            8,
            "自投帧必须是 len=8 的 PING 请求: {probes:?}"
        );
        let token = own[0].token.clone();
        assert_eq!(token.len(), 8);

        let mut acks = drain_log(&a_wire_rx);
        strip_preface(&mut acks);
        let acks = ping_probes(&take_frames(&mut acks));
        assert!(
            acks.iter().any(|p| p.ack && p.token == token),
            "对端 h2 应对注入的 PING 回 ACK: {acks:?}"
        );

        // 匹配 ACK 被拦截：不得出现在交给上层 h2 的字节里
        let mut up_in = drain_log(&a_up_rx);
        for f in take_frames(&mut up_in) {
            if f.ftype == TYPE_PING && f.flags & FLAG_ACK != 0 {
                assert_ne!(f.payload, token, "匹配 ACK 必须被拦截，不得交 h2");
            }
        }

        // 推进超过 3 个窗口：双方 ACK 均匹配，连接不得判死链。
        // 虚拟时钟下 ACK 必须落在同一个窗口内，故每次推进后给两侧任务充分的轮次。
        for i in 0..(MAX_CONSECUTIVE_WINDOW_FAILURES + 1) {
            advance(WINDOW).await;
            yield_ticks(64).await;
            assert_eq!(
                roundtrip(&mut sender).await,
                b"pong",
                "第 {i} 个窗口后连接仍须可用"
            );
            yield_ticks(16).await;
        }
        assert_eq!(
            roundtrip(&mut sender).await,
            b"pong",
            "跨窗口后连接仍须可用"
        );

        // 先关掉客户端连接，服务端 accept() 才会结束
        drop(sender);
        let _ = conn_task.await;
        let served = srv.await.expect("服务端任务");
        assert_eq!(
            served,
            1 + (MAX_CONSECUTIVE_WINDOW_FAILURES as usize + 1) + 1,
            "跨窗口期间每个请求都应被正常服务"
        );
    })
    .await;
}

/// 让当前线程真正 park 一轮。
///
/// tokio 暂停时钟下 `advance` 只推进虚拟时间；到期的 `Sleep` 要等 runtime 的
/// time driver 被驱动（线程 park 一次）才会触发 waker。真实运行没有这个问题。
async fn park_driver() {
    tokio::time::sleep(Duration::from_millis(1)).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn yield_ticks(n: usize) {
    for _ in 0..n {
        tokio::task::yield_now().await;
    }
}

// ===== 故障路径基础设施：可编程底层 =====
//
// 上面全部用例的底层是 `tokio::io::duplex`，它不会部分写出到指定长度、不会
// 立即释放、也不会让 `poll_shutdown` 永久 Pending。故障路径需要精确控制这些
// 行为，因此这里另配一个可编程底层：写出预算、写出错误、`Ok(0)`、入站字节、
// EOF、`poll_shutdown` 是否 Pending、以及 **Drop 计数**（Arc 共享，适配器把
// 底层释放掉之后依然能读到）。

/// 底层 io 的可观察记录，`Arc` 共享：底层被适配器 drop 后仍可核验。
#[derive(Default)]
struct MockWire {
    /// 每次 `poll_write` 收到的请求切片（含被拒的），用于断言写出顺序。
    attempts: Vec<Vec<u8>>,
    /// 真正离开适配器的字节。
    emitted: Vec<u8>,
    shutdown_calls: usize,
}

#[derive(Clone)]
struct MockHandle {
    wire: Arc<Mutex<MockWire>>,
    drops: Arc<AtomicUsize>,
}

impl MockHandle {
    fn attempts(&self) -> Vec<Vec<u8>> {
        self.wire.lock().unwrap().attempts.clone()
    }
    fn emitted(&self) -> Vec<u8> {
        self.wire.lock().unwrap().emitted.clone()
    }
    fn shutdown_calls(&self) -> usize {
        self.wire.lock().unwrap().shutdown_calls
    }
    fn drops(&self) -> usize {
        self.drops.load(Ordering::SeqCst)
    }
}

/// 可编程底层。字段默认“健康”，由用例通过 `io.get_mut()` 调整。
struct MockIo {
    wire: Arc<Mutex<MockWire>>,
    drops: Arc<AtomicUsize>,
    /// 还能向底层写出的字节数；0 → `poll_write` 返回 Pending。
    out_budget: usize,
    /// Some → `poll_write` 直接返回该错误。
    write_err: Option<io::ErrorKind>,
    /// Some → `poll_read` 直接返回该错误（P3：底层读 Err 的同口径验证）。
    read_err: Option<io::ErrorKind>,
    /// true → `poll_write` 返回 `Ok(0)`。
    write_zero: bool,
    /// true → 底层 `poll_shutdown` 永远 Pending。
    pending_shutdown: bool,
    /// 底层入站字节；读空后按 `inbound_eof` 决定 EOF 还是 Pending。
    inbound: VecDeque<u8>,
    inbound_eof: bool,
}

fn mock_io(out_budget: usize) -> (MockIo, MockHandle) {
    let wire = Arc::new(Mutex::new(MockWire::default()));
    let drops = Arc::new(AtomicUsize::new(0));
    (
        MockIo {
            wire: wire.clone(),
            drops: drops.clone(),
            out_budget,
            write_err: None,
            read_err: None,
            write_zero: false,
            pending_shutdown: false,
            inbound: VecDeque::new(),
            inbound_eof: false,
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
        if let Some(kind) = me.read_err {
            return Poll::Ready(Err(io::Error::new(kind, "可编程底层读错误")));
        }
        if me.inbound.is_empty() {
            return if me.inbound_eof {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            };
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
        me.wire.lock().unwrap().attempts.push(buf.to_vec());
        if let Some(kind) = me.write_err {
            return Poll::Ready(Err(io::Error::new(kind, "可编程底层写错误")));
        }
        if me.write_zero {
            return Poll::Ready(Ok(0));
        }
        if me.out_budget == 0 {
            return Poll::Pending;
        }
        let n = buf.len().min(me.out_budget);
        {
            let mut wire = me.wire.lock().unwrap();
            wire.emitted.extend_from_slice(&buf[..n]);
        }
        me.out_budget -= n;
        Poll::Ready(Ok(n))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let me = self.get_mut();
        me.wire.lock().unwrap().shutdown_calls += 1;
        if me.pending_shutdown {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

// ===== A: 跨窗口在途半帧 =====

/// 逐 cut 复现并验证：旧 PING 只写出前 `cut` 字节后跨窗口，恢复时必须先把旧帧
/// 的**剩余字节**原样写完，之后才允许出现新窗口的 PING；整条出向字节流必须恰好
/// 解析成 [旧帧, 新帧] 两个完整 PING，token 不同，一个字节都不许丢。
#[tokio::test]
async fn partial_probe_across_window_finishes_old_frame_first() {
    unconstrained(async move {
        tokio::time::pause();
        for cut in 1..=16usize {
            // 预算 = 首条 SETTINGS 的 9B + 探测前缀 cut 字节（§3.4 后才有自发帧）
            let (lower, obs) = mock_io(9 + cut);
            let mut io = H2KeepaliveIo::new(lower, H2Role::Server);
            let waker = test_waker();
            let mut cx = Context::from_waker(&waker);
            match Pin::new(&mut io).poll_write(&mut cx, &settings_frame()) {
                Poll::Ready(Ok(9)) => {}
                other => panic!("写首条 SETTINGS 失败: {other:?}"),
            }
            assert!(
                matches!(Pin::new(&mut io).poll_flush(&mut cx), Poll::Pending),
                "cut={cut}：写出预算耗尽应 Pending"
            );
            // 离开适配器的字节（Poll 尝试不计）：SETTINGS + 正好 cut 个旧帧前缀字节
            let emitted = obs.emitted();
            assert_eq!(
                emitted.len(),
                9 + cut,
                "cut={cut}：预算耗尽时只应离开 9+cut 字节，实际 {emitted:?}"
            );
            assert_eq!(&emitted[..9], &settings_frame()[..]);
            // 适配器发起的第一条探测帧请求切片（整帧 17B，供逐字比对）
            let old = obs.attempts()[1].clone();
            assert_eq!(old.len(), 17, "探测帧固定 17B");
            assert_eq!(&old[..9], &[0, 0, 8, 6, 0, 0, 0, 0, 0]);
            assert_eq!(emitted[9..], old[..cut], "cut={cut}：只写出前 cut 字节");

            advance(WINDOW).await;
            io.get_mut().expect("底层尚未释放").out_budget = 4096;
            assert!(
                matches!(Pin::new(&mut io).poll_flush(&mut cx), Poll::Ready(Ok(()))),
                "cut={cut}：放开底层后 flush 应完成"
            );

            // 恢复后：线路上必须是“旧帧剩余字节 → 旧帧结束 → 新窗口整帧”，
            // 也就是前 17B 仍是旧帧原样，第 17B 之后才是新帧头。
            let emitted = obs.emitted();
            assert_eq!(
                &emitted[9..18],
                &[0, 0, 8, 6, 0, 0, 0, 0, 0],
                "cut={cut}：SETTINGS 之后必须是旧 PING 帧头"
            );
            assert_eq!(
                &emitted[26..35],
                &[0, 0, 8, 6, 0, 0, 0, 0, 0],
                "cut={cut}：旧帧 17B 完整之后才允许新窗口 PING 的帧头"
            );

            let emitted = obs.emitted();
            assert_eq!(
                emitted.len(),
                9 + 34,
                "cut={cut}：SETTINGS + 旧帧 17B + 新帧 17B，不得丢尾或重复"
            );
            assert_eq!(
                &emitted[9..26],
                old.as_slice(),
                "cut={cut}：旧帧必须完整连续"
            );
            let frames = parse_frames(&emitted[9..]);
            assert_eq!(frames.len(), 2, "cut={cut}：出向流必须恰好两个完整帧");
            assert!(frames.iter().all(|f| f.ftype == TYPE_PING && f.stream == 0));
            assert_ne!(frames[0].payload, frames[1].payload);
            let st = io.stats();
            assert_eq!(st.windows_opened, 2, "cut={cut}：窗口应前进一个");
            assert_eq!(
                st.consecutive_failures, 1,
                "cut={cut}：旧窗口未 ACK 计一次失败"
            );
            assert_eq!(st.probes_sent, 2, "cut={cut}：完整写出两个探测帧");
        }
    })
    .await;
}

/// 堵塞期间跨过的中间窗口：其探测从未上线，必须直接丢弃（不补发、不积压），
/// 线速恢复后只允许出现“旧帧剩余 + 当前窗口最新探测”。
#[tokio::test]
async fn partial_probe_across_windows_never_backlogs_dropped_probes() {
    unconstrained(async move {
        tokio::time::pause();
        let (lower, obs) = mock_io(9 + 5);
        let mut io = H2KeepaliveIo::new(lower, H2Role::Server);
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        match Pin::new(&mut io).poll_write(&mut cx, &settings_frame()) {
            Poll::Ready(Ok(9)) => {}
            other => panic!("写首条 SETTINGS 失败: {other:?}"),
        }
        assert!(Pin::new(&mut io).poll_flush(&mut cx).is_pending());
        let old = obs.attempts()[1].clone();

        // 堵塞中再跨 2 个窗口：中间窗口的探测从未上线
        for _ in 0..2 {
            advance(WINDOW).await;
            assert!(
                Pin::new(&mut io).poll_flush(&mut cx).is_pending(),
                "在途半帧未写完前不得写任何其他字节"
            );
        }
        let mut want_prefix = settings_frame();
        want_prefix.extend_from_slice(&old[..5]);
        assert_eq!(obs.emitted(), want_prefix, "堵塞期间不得有任何新增字节");
        let opened = io.stats().windows_opened;
        assert_eq!(opened, 3, "窗口计时不因堵塞暂停");
        assert_eq!(io.stats().consecutive_failures, 2);

        io.get_mut().expect("底层尚未释放").out_budget = 4096;
        assert!(matches!(
            Pin::new(&mut io).poll_flush(&mut cx),
            Poll::Ready(Ok(()))
        ));
        let emitted = obs.emitted();
        let frames = parse_frames(&emitted[9..]);
        assert_eq!(
            frames.len(),
            2,
            "只允许旧帧 + 当前窗口最新探测，历史窗口探测不得补发: {frames:?}"
        );
        assert_eq!(frames[0].payload, &old[9..]);
        assert_ne!(frames[0].payload, frames[1].payload);
        let st = io.stats();
        assert_eq!(st.probes_sent, 2, "从未上线的中间窗口探测不得计入已发");
        assert_eq!(st.consecutive_failures, 2, "窗口计时不因堵塞暂停");
    })
    .await;
}

/// 在途旧帧对应窗口的 ACK 属于迟到 ACK：不得 credit 新窗口，且必须原样交 h2。
#[tokio::test]
async fn in_flight_old_probe_ack_does_not_credit_new_window() {
    unconstrained(async move {
        tokio::time::pause();
        let (lower, obs) = mock_io(9 + 9);
        let mut io = H2KeepaliveIo::new(lower, H2Role::Server);
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        match Pin::new(&mut io).poll_write(&mut cx, &settings_frame()) {
            Poll::Ready(Ok(9)) => {}
            other => panic!("写首条 SETTINGS 失败: {other:?}"),
        }
        assert!(Pin::new(&mut io).poll_flush(&mut cx).is_pending());
        let old = obs.attempts()[1].clone();
        let mut old_token = [0u8; 8];
        old_token.copy_from_slice(&old[9..]);

        advance(WINDOW).await;
        io.get_mut().expect("底层尚未释放").out_budget = 4096;
        assert!(matches!(
            Pin::new(&mut io).poll_flush(&mut cx),
            Poll::Ready(Ok(()))
        ));
        assert_eq!(io.stats().consecutive_failures, 1);

        // 旧窗口的 ACK 迟到：不得刷新健康
        io.get_mut()
            .expect("底层尚未释放")
            .inbound
            .extend(ping_bytes(&old_token, true));
        let mut buf = vec![0u8; 32];
        let got = {
            let mut rb = ReadBuf::new(&mut buf);
            match Pin::new(&mut io).poll_read(&mut cx, &mut rb) {
                Poll::Ready(Ok(())) => rb.filled().len(),
                other => panic!("迟到 ACK 应原样交付，实际 {other:?}"),
            }
        };
        let delivered = buf[..got].to_vec();
        assert_eq!(
            delivered,
            ping_bytes(&old_token, true),
            "在途旧帧的 ACK 必须原样交 h2"
        );
        let st = io.stats();
        assert_eq!(st.consecutive_failures, 1, "旧窗口 ACK 不得 credit 新窗口");
        assert_eq!(st.acked_windows, 0);
        assert_eq!(st.intercepted_acks, 0);
    })
    .await;
}

// ===== C: 终态必须释放底层 io =====

/// 底层 `poll_shutdown` 永远 Pending 也要在**首次上报错误之前**就把底层释放掉：
/// 关掉整条连接靠的是所有权释放（drop），不是把 `poll_shutdown` 轮询一次。
#[tokio::test]
async fn fatal_timeout_releases_inner_before_first_error() {
    unconstrained(async move {
        tokio::time::pause();
        let (lower, obs) = mock_io(4096);
        let mut lower = lower;
        lower.pending_shutdown = true;
        let mut io = H2KeepaliveIo::new(lower, H2Role::Server);
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(
            Pin::new(&mut io).poll_flush(&mut cx),
            Poll::Ready(Ok(()))
        ));

        advance(WINDOW * MAX_CONSECUTIVE_WINDOW_FAILURES).await;
        let err = match Pin::new(&mut io).poll_flush(&mut cx) {
            Poll::Ready(Err(e)) => e,
            other => panic!("应报死链错误，实际 {other:?}"),
        };
        assert!(is_keepalive_timeout(&err), "{err:?}");
        assert_eq!(obs.drops(), 1, "首次上报错误时就须释放底层 io");
        assert_eq!(
            obs.shutdown_calls(),
            0,
            "关闭靠 drop 所有权，不靠把 poll_shutdown 轮一次（Pending 即被丢弃）"
        );
        assert!(io.get_ref().is_none(), "终态不得再暴露底层");
        assert!(io.get_mut().is_none());

        // 错误 sticky：四种 poll 一律同一错误，且不会反复释放或复活
        for _ in 0..3 {
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_flush(&mut cx)
            )));
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_write(&mut cx, b"x")
            )));
            let mut buf = [0u8; 8];
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut buf))
            )));
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_shutdown(&mut cx)
            )));
        }
        assert_eq!(obs.drops(), 1, "终态不得重复释放");
        assert_eq!(obs.shutdown_calls(), 0);
        assert!(
            io.into_inner().is_none(),
            "终态 into_inner 不得 panic，返回 None"
        );
    })
    .await;
}

fn poll_err<T: std::fmt::Debug>(p: Poll<io::Result<T>>) -> io::Error {
    match p {
        Poll::Ready(Err(e)) => e,
        other => panic!("期望 Ready(Err)，实际 {other:?}"),
    }
}

/// 底层写错误 / `Ok(0)` 也必须当场释放底层，终态访问器不 panic。
#[tokio::test]
async fn underlying_write_failure_releases_inner_immediately() {
    unconstrained(async move {
        tokio::time::pause();
        for (name, kind) in [
            ("poll_write 返回 Err", Some(io::ErrorKind::ConnectionReset)),
            ("poll_write 返回 Ok(0)", None),
        ] {
            let (mut lower, obs) = mock_io(4096);
            match kind {
                Some(k) => lower.write_err = Some(k),
                None => lower.write_zero = true,
            }
            let mut io = H2KeepaliveIo::new(lower, H2Role::Server);
            let waker = test_waker();
            let mut cx = Context::from_waker(&waker);
            // §3.4 注入门控后，探测不会先于首条 SETTINGS 上线；这里用上层的第一
            // 次业务写（首条 SETTINGS）命中同一条写错误路径，断言完全不变。
            let err = poll_err(Pin::new(&mut io).poll_write(&mut cx, &settings_frame()));
            let want = kind.unwrap_or(io::ErrorKind::BrokenPipe);
            assert_eq!(err.kind(), want, "{name}: {err:?}");
            assert_eq!(obs.drops(), 1, "{name}：首次错误即须释放底层");
            assert_eq!(obs.shutdown_calls(), 0, "{name}：不靠 poll_shutdown 兜底");
            assert!(io.get_ref().is_none() && io.get_mut().is_none());
            let again = poll_err(Pin::new(&mut io).poll_flush(&mut cx));
            assert_eq!(again.kind(), want, "{name}：错误须 sticky 同口径");
            assert_eq!(obs.drops(), 1);
            assert!(io.into_inner().is_none(), "{name}：终态不得交还底层");
        }
    })
    .await;
}

/// 真实 TCP：首次上报死链错误时必须已经释放底层 `TcpStream`。
///
/// 只用“底层 `poll_shutdown` 被调用了几次”证明不了整条 TCP 关闭：真实 socket
/// 要么看 `Drop`（fd 是否真的关闭），要么看对端是否观察到 FIN。这里两个都看。
#[tokio::test(flavor = "current_thread")]
async fn fatal_timeout_over_real_tcp_closes_socket() {
    unconstrained(async move {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = TcpStream::connect(addr).await.expect("connect");
        let (mut peer, _) = listener.accept().await.expect("accept");
        let drops = Arc::new(AtomicUsize::new(0));
        let shutdowns = Arc::new(AtomicUsize::new(0));
        let watched = TcpWatch {
            inner: client,
            drops: drops.clone(),
            shutdowns: shutdowns.clone(),
        };
        let mut io = H2KeepaliveIo::new(watched, H2Role::Server);
        tokio::time::pause();
        let waker = test_waker();
        let mut cx = Context::from_waker(&waker);

        // 上层先写出首条 SETTINGS（§3.4）：自发帧只能出现在它之后
        match Pin::new(&mut io).poll_write(&mut cx, &settings_frame()) {
            Poll::Ready(Ok(9)) => {}
            other => panic!("写首条 SETTINGS 失败: {other:?}"),
        }
        assert!(
            matches!(Pin::new(&mut io).poll_flush(&mut cx), Poll::Ready(Ok(()))),
            "首窗探测应能写出"
        );
        let mut first = [0u8; 9];
        peer.read_exact(&mut first)
            .await
            .expect("读取 SETTINGS 帧头");
        assert_eq!(&first, &[0, 0, 0, 4, 0, 0, 0, 0, 0]);
        let mut frame = [0u8; 17];
        peer.read_exact(&mut frame).await.expect("读取探测帧");
        assert_eq!(&frame[..9], &[0, 0, 8, 6, 0, 0, 0, 0, 0]);

        advance(WINDOW * MAX_CONSECUTIVE_WINDOW_FAILURES).await;
        let err = poll_err(Pin::new(&mut io).poll_flush(&mut cx));
        assert!(is_keepalive_timeout(&err), "{err:?}");
        // 首次上报错误的那一刻：底层已被释放
        assert_eq!(
            drops.load(Ordering::SeqCst),
            1,
            "首次死链错误必须当场释放底层 TcpStream，不能等到下一轮 poll"
        );
        assert_eq!(
            shutdowns.load(Ordering::SeqCst),
            0,
            "关闭靠所有权释放，不靠把 poll_shutdown 轮一次"
        );
        // 整条 TCP（不是只关写半边）真关了的两个硬证据：上面的 Drop 计数与这里
        // 的对端 FIN/EOF。刻意不拿 `fcntl(fd)` 当判据：真正 close 由 runtime 的 IO
        // driver 收尾，fd 号还可能被后续打开的 fd 复用，判据不稳定；反过来，若实现
        // 只 `poll_shutdown` 半个方向（fd 仍开、仍能收对端字节），`drops == 1`
        // 会直接失败——这正是这次修复前真实 TCP 上观察到的状态。
        let mut sink = [0u8; 8];
        assert_eq!(
            peer.read(&mut sink).await.expect("peer read"),
            0,
            "对端必须观察到 FIN/EOF，而不是仍能读到 WouldBlock"
        );

        // 终态：怎么 poll 都不复活、不重复释放
        for _ in 0..3 {
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut sink))
            )));
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_write(&mut cx, b"x")
            )));
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_flush(&mut cx)
            )));
            assert!(is_keepalive_timeout(&poll_err(
                Pin::new(&mut io).poll_shutdown(&mut cx)
            )));
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(shutdowns.load(Ordering::SeqCst), 0);
        assert!(io.get_ref().is_none() && io.get_mut().is_none());
        assert!(
            io.into_inner().is_none(),
            "终态 into_inner 返回 None 而非 panic"
        );
    })
    .await;
}

/// 带 Drop 计数与 shutdown 计数的真实 TCP 包装。
struct TcpWatch {
    inner: TcpStream,
    drops: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
}

impl Drop for TcpWatch {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl AsyncRead for TcpWatch {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for TcpWatch {
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
        let me = self.get_mut();
        me.shutdowns.fetch_add(1, Ordering::SeqCst);
        Pin::new(&mut me.inner).poll_shutdown(cx)
    }
}

// ===== EOF 截断语义 =====

/// 关闭 duplex 对端的写半边：本端下一次 `poll_read` 观察到 EOF。
fn shutdown_peer<D: AsyncWrite + Unpin>(peer: &mut D, waker: &Waker) {
    let mut cx = Context::from_waker(waker);
    match Pin::new(peer).poll_shutdown(&mut cx) {
        Poll::Ready(Ok(())) => {}
        Poll::Ready(Err(e)) => panic!("关闭对端写半边失败: {e}"),
        Poll::Pending => panic!("duplex shutdown 不应 Pending"),
    }
}

#[tokio::test]
async fn eof_inside_partial_frame_is_forwarded_not_swallowed() {
    unconstrained(async move {
        tokio::time::pause();
        // 1..=8：帧头只收了一半就 EOF（client 角色：入站没有前导，直接进帧头）
        for frag in 1..=8usize {
            let mut h = client();
            let raw = encode(&frame(TYPE_SETTINGS, &[0, 0, 0, 0, 0, 0, 0, 1]));
            h.feed_peer(&raw[..frag]);
            let waker = h.waker.clone();
            shutdown_peer(&mut h.peer, &waker);
            let got = h.read_until_pending();
            assert_eq!(
                got,
                raw[..frag],
                "frag={frag}：残缺帧头必须原样透传，不得吞字节后假 EOF"
            );
            match h.read(16) {
                Poll::Ready(Ok(v)) => {
                    assert!(v.is_empty(), "frag={frag}：透传残头后应报干净 EOF")
                }
                other => panic!("frag={frag}：期望 EOF，实际 {other:?}"),
            }
            assert!(
                !h.stats().failed,
                "frag={frag}：EOF 不是死链: {:?}",
                h.stats()
            );
        }
        // 10..=16：整条 17B 连接级 PING 只到一半就 EOF（帧头已定完，缺 payload 尾部）
        for frag in 10..=16usize {
            let mut h = client();
            let raw = ping_bytes(&[0x0b, 0x7b, 0xa2, 0xf0, 0x8b, 0x9b, 0xfe, 0x54], true);
            h.feed_peer(&raw[..frag]);
            let waker = h.waker.clone();
            shutdown_peer(&mut h.peer, &waker);
            let got = h.read_until_pending();
            assert_eq!(got, raw[..frag], "frag={frag}：残缺 PING 必须原样透传");
            match h.read(16) {
                Poll::Ready(Ok(v)) => {
                    assert!(v.is_empty(), "frag={frag}：期望 EOF，实际内容为空")
                }
                other => panic!("frag={frag}：期望 EOF，实际 {other:?}"),
            }
        }
    })
    .await;
}

#[tokio::test]
async fn eof_inside_inbound_preface_is_forwarded_not_swallowed() {
    unconstrained(async move {
        tokio::time::pause();
        // 服务端角色：入站前 24B 是客户端连接前导。前导中途 EOF 时，已到达的
        // 字节必须原样交付，然后才是干净 EOF——不得吞字节假装对端规矩收场。
        for frag in [1usize, 7, 12, 23] {
            let mut h = server();
            h.feed_peer(&PREFACE[..frag]);
            let waker = h.waker.clone();
            shutdown_peer(&mut h.peer, &waker);
            let got = h.read_until_pending();
            assert_eq!(got, PREFACE[..frag], "frag={frag}：残缺前导必须原样透传");
            match h.read(16) {
                Poll::Ready(Ok(v)) => {
                    assert!(v.is_empty(), "frag={frag}：期望 EOF，实际内容为空")
                }
                other => panic!("frag={frag}：期望 EOF，实际 {other:?}"),
            }
            assert!(
                !h.stats().failed,
                "frag={frag}：EOF 不是死链: {:?}",
                h.stats()
            );
        }
    })
    .await;
}

// ===== header block 不可交错（RFC 9113 §4.3 / §6.10）=====
//
// “A sender MUST NOT allow a logical sequence of header blocks to be interleaved
//  with other frame sequences”：HEADERS/PUSH_PROMISE 未置 END_HEADERS 时，后续必须
// 紧跟 CONTINUATION 直到 END_HEADERS，中间不得插入任何其它帧（连接级 PING 也是帧）。
// 单帧 payload 结束（payload_left 为 None）**不是**可注入边界。
// 依据 https://www.rfc-editor.org/rfc/rfc9113.html#section-4.3

const TYPE_HEADERS: u8 = 0x1;
const TYPE_PUSH_PROMISE: u8 = 0x5;
const TYPE_CONTINUATION: u8 = 0x9;
const FLAG_END_HEADERS: u8 = 0x4;

#[tokio::test]
async fn probe_is_not_inserted_inside_a_header_block() {
    unconstrained(async move {
        tokio::time::pause();
        let mut h = client();
        h.write_all(PREFACE);
        h.write_all(&settings_frame());
        h.pump();
        let _ = h.out_frames();

        // header block 序列：HEADERS(无 END_HEADERS) + CONTINUATION(无 END_HEADERS)
        h.write_all(&encode_frame(
            TYPE_HEADERS,
            0,
            1,
            b"header-block-fragment-1",
        ));
        h.write_all(&encode_frame(TYPE_CONTINUATION, 0, 1, b"fragment-2"));
        let _ = h.drain_peer_keep(); // 先取走刚写出的两帧本身
        // 跨一个窗口：header block 未结束，探测不得插进序列
        tick(&mut h, 1).await;
        let mid = h.drain_peer_keep();
        assert!(
            mid.is_empty(),
            "HEADERS/CONTINUATION 未 END_HEADERS 时不得插入任何帧: {mid:?}"
        );
        assert!(
            !h.stats().failed,
            "序列内探测被抑制不等于死链: {:?}",
            h.stats()
        );

        // 终帧：END_HEADERS，此后才是可注入边界
        h.write_all(&encode_frame(
            TYPE_CONTINUATION,
            FLAG_END_HEADERS,
            1,
            b"fragment-3",
        ));
        let _ = h.drain_peer_keep(); // 先取走终帧本身
        tick(&mut h, 1).await;
        h.pump();
        // 出向序列必须恰好是：HEADERS → CONT → CONT(END) → PING
        let kinds: Vec<(u8, u8)> = h.out_frames().iter().map(|f| (f.ftype, f.flags)).collect();
        assert_eq!(
            kinds,
            vec![
                (TYPE_HEADERS, 0),
                (TYPE_CONTINUATION, 0),
                (TYPE_CONTINUATION, FLAG_END_HEADERS),
                (TYPE_PING, 0),
            ],
            "PING 只能出现在 END_HEADERS 终帧之后"
        );

        // PUSH_PROMISE 同样受约束
        let mut pp = client();
        pp.write_all(PREFACE);
        pp.write_all(&settings_frame());
        pp.pump();
        let _ = pp.out_frames();
        pp.write_all(&encode_frame(TYPE_PUSH_PROMISE, 0, 1, b"pp-no-end-headers"));
        let _ = pp.drain_peer_keep();
        tick(&mut pp, 1).await;
        assert!(
            pp.drain_peer_keep().is_empty(),
            "PUSH_PROMISE 无 END_HEADERS 同样不得插帧"
        );

        // 反向对照：HEADERS 带 END_HEADERS 的普通帧边界仍可注入
        let mut ok3 = client();
        ok3.write_all(PREFACE);
        ok3.write_all(&settings_frame());
        ok3.pump();
        let _ = ok3.out_frames();
        ok3.write_all(&encode_frame(
            TYPE_HEADERS,
            FLAG_END_HEADERS,
            1,
            b"complete-header-block",
        ));
        let _ = ok3.drain_peer_keep();
        tick(&mut ok3, 1).await;
        ok3.pump();
        assert!(
            !ok3.out_probes().is_empty(),
            "END_HEADERS 已置位时帧边界必须可注入，否则保活被永久抑制"
        );
    })
    .await;
}

// ===== G: 首条 SETTINGS 注入门控 + 底层读 Err 同口径（Task 4c）=====

/// 首条 SETTINGS 帧之前禁止自发 PING（RFC9113 §3.4：HTTP/2 连接首帧双方都是
/// SETTINGS，preface 与首 SETTINGS 之间不得插任何帧）。旧实现把“前导一写完就
/// 可发 PING”当合法——这是对协议的违反，本用例是直接回归断言：client 与
/// server 两侧在首条 SETTINGS 完整写出之前，哪怕窗口计时走满也**一个字节**都
/// 不许自发；首条 SETTINGS 落地后注入立即恢复（不是永久抑制）。
#[tokio::test]
async fn probe_never_precedes_first_settings_frame() {
    unconstrained(async move {
        tokio::time::pause();
        // client：只写前导，跨 2 个窗口计时（第 3 窗会如实判死链，先停在 2）
        let mut h = client();
        h.write_all(PREFACE);
        h.pump();
        advance(WINDOW).await;
        h.pump();
        advance(WINDOW).await;
        h.pump();
        let raw = h.drain_peer_raw();
        assert_eq!(
            raw,
            PREFACE.as_slice(),
            "首条 SETTINGS 之前不得有任何自发字节"
        );
        assert_eq!(h.stats().probes_sent, 0, "计时走满也不得在门控前发探测");

        // 首条 SETTINGS 完整写出后：边界注入立即恢复
        h.write_all(&settings_frame());
        let mut probes = h.out_probes();
        for _ in 0..8 {
            if !probes.is_empty() {
                break;
            }
            h.pump();
            probes = h.out_probes();
        }
        assert_eq!(
            probes.len(),
            1,
            "首条 SETTINGS 后应立刻可注入探测: {probes:?}"
        );

        // server 侧同样受 §3.4 约束
        let mut s = server();
        s.feed_peer(PREFACE);
        let _ = s.read_exact(PREFACE.len());
        tick(&mut s, 2).await;
        let raw = s.drain_peer_raw();
        assert!(
            raw.is_empty(),
            "server 侧首条 SETTINGS 前不得自发任何帧: {raw:?}"
        );
        assert_eq!(s.stats().probes_sent, 0);
        s.write_all(&settings_frame());
        let mut probes = s.out_probes();
        for _ in 0..8 {
            if !probes.is_empty() {
                break;
            }
            s.pump();
            probes = s.out_probes();
        }
        assert_eq!(
            probes.len(),
            1,
            "server 侧首条 SETTINGS 后注入恢复: {probes:?}"
        );
    })
    .await;
}

/// P3：底层读 Err（帧头 / 暂存检视 / payload 流式段三条分支）与写 Err **同口径**
/// ——首次判定即记录、当次同步释放底层（`S::drop` 立刻发生）、四类 poll sticky。
/// 不得只记日志留着 `get_ref` 继续探测/调度。EOF（`Ok(0)`）不在此列：那是截断
/// 透传语义，另有专门用例覆盖。
#[tokio::test]
async fn underlying_read_failure_releases_inner_immediately() {
    unconstrained(async move {
        tokio::time::pause();
        let kind = io::ErrorKind::ConnectionReset;

        // —— 分支 1：帧头读取段 ——
        {
            let (mut lower, obs) = mock_io(4096);
            lower.read_err = Some(kind);
            let mut io = H2KeepaliveIo::new(lower, H2Role::Client);
            let waker = test_waker();
            let mut cx = Context::from_waker(&waker);
            let err =
                poll_err(Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut [0u8; 32])));
            assert_eq!(err.kind(), kind, "帧头读 Err 原样上报 kind");
            assert_eq!(obs.drops(), 1, "读 Err 必须当次释放底层（与写 Err 同口径）");
            assert_eq!(
                obs.shutdown_calls(),
                0,
                "释放靠所有权，不靠 poll_shutdown 兜底"
            );
            assert!(io.get_ref().is_none() && io.get_mut().is_none());
            let st = io.stats();
            assert!(st.failed && st.stopped, "读 Err 必须记入终态: {st:?}");
            assert_eq!(st.probes_sent, 0, "终态后不得再有探测调度");
            for _ in 0..3 {
                let mut buf = [0u8; 8];
                assert_eq!(
                    poll_err(Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut buf)))
                        .kind(),
                    kind,
                    "poll_read sticky"
                );
                assert_eq!(
                    poll_err(Pin::new(&mut io).poll_write(&mut cx, b"x")).kind(),
                    kind,
                    "poll_write sticky"
                );
                assert_eq!(
                    poll_err(Pin::new(&mut io).poll_flush(&mut cx)).kind(),
                    kind,
                    "poll_flush sticky"
                );
                assert_eq!(
                    poll_err(Pin::new(&mut io).poll_shutdown(&mut cx)).kind(),
                    kind,
                    "poll_shutdown sticky"
                );
            }
            assert_eq!(obs.drops(), 1, "sticky 后不得二次释放");
            assert!(io.into_inner().is_none(), "终态不得交还底层");
        }

        // —— 分支 2：17B 连接级 PING 的定长暂存检视段 ——
        {
            let (lower, obs) = mock_io(4096);
            let mut io = H2KeepaliveIo::new(lower, H2Role::Client);
            let waker = test_waker();
            let mut cx = Context::from_waker(&waker);
            // 喂满 9B PING 帧头进入 17B 暂存检视（还差 8B token）
            io.get_mut()
                .expect("底层尚未释放")
                .inbound
                .extend([0, 0, 8, 6, 0, 0, 0, 0, 0]);
            let first = Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut [0u8; 32]));
            assert!(
                matches!(first, Poll::Pending),
                "PING 帧头已收到、token 未齐时应 Pending: {first:?}"
            );
            io.get_mut().expect("底层尚未释放").read_err = Some(kind);
            let err =
                poll_err(Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut [0u8; 32])));
            assert_eq!(err.kind(), kind, "暂存检视内的读 Err 同口径失败");
            assert_eq!(obs.drops(), 1, "暂存段的读 Err 也必须当次释放底层");
            assert!(io.get_ref().is_none());
            for _ in 0..3 {
                let mut buf = [0u8; 8];
                assert_eq!(
                    poll_err(Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut buf)))
                        .kind(),
                    kind,
                    "staged 段终态后 poll_read sticky"
                );
                assert_eq!(poll_err(Pin::new(&mut io).poll_flush(&mut cx)).kind(), kind);
            }
            assert_eq!(obs.drops(), 1);
        }

        // —— 分支 3：payload 流式转发段（DATA 大帧转发到一半）——
        {
            let (lower, obs) = mock_io(4096);
            let mut io = H2KeepaliveIo::new(lower, H2Role::Client);
            let waker = test_waker();
            let mut cx = Context::from_waker(&waker);
            let mut feed = encode_frame(0, 0, 1, &[7u8; 100]);
            let head = feed.len().min(19); // 帧头 9B + payload 前 10B
            let rest = feed.split_off(head); // 先只喂前半，制造“payload 中段”
            assert_eq!(feed.len() + rest.len(), 9 + 100);
            io.get_mut().expect("底层尚未释放").inbound.extend(&feed);
            // 读走已喂的部分，直到 Pending（payload 还差 90B）
            let mut got = Vec::new();
            for _ in 0..8 {
                let mut buf = [0u8; 32];
                let mut rb = ReadBuf::new(&mut buf);
                match Pin::new(&mut io).poll_read(&mut cx, &mut rb) {
                    Poll::Ready(Ok(())) if rb.filled().is_empty() => break,
                    Poll::Ready(Ok(())) => got.extend_from_slice(rb.filled()),
                    Poll::Pending => break,
                    Poll::Ready(Err(e)) => panic!("预期无错: {e:?}"),
                }
            }
            assert_eq!(got.len(), 19, "应先收到帧头 + payload 前 10B: {got:?}");
            assert!(matches!(
                Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut [0u8; 32])),
                Poll::Pending
            ));
            io.get_mut().expect("底层尚未释放").read_err = Some(kind);
            let err =
                poll_err(Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut [0u8; 32])));
            assert_eq!(err.kind(), kind, "payload 段的读 Err 同口径失败");
            assert_eq!(obs.drops(), 1, "payload 段的读 Err 也必须当次释放底层");
            assert!(io.get_ref().is_none());
            // sticky：payload 中断后不得靠重试“续上”
            for _ in 0..3 {
                let mut buf = [0u8; 8];
                assert_eq!(
                    poll_err(Pin::new(&mut io).poll_read(&mut cx, &mut ReadBuf::new(&mut buf)))
                        .kind(),
                    kind
                );
                assert_eq!(
                    poll_err(Pin::new(&mut io).poll_shutdown(&mut cx)).kind(),
                    kind
                );
            }
            assert_eq!(obs.drops(), 1);
        }
    })
    .await;
}

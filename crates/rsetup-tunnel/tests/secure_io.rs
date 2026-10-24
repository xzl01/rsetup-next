//! Task 1：`SecureIo` 加密记录异步 I/O 行为测试（protocol_spec.md §5.2）。
//!
//! 覆盖（先红后绿）：
//! * duplex 分片 / 多条 / 大于单记录的明文 / 空记录后仍有数据 / 真实 h2 互操作；
//! * **buffered AsyncWrite 契约**：`Ready(Ok(n))` 即"本次入参前 n 字节已被复制
//!   接管"（之后调用方可立刻复用/改写该 buffer）；`Pending` 即"本次 0 字节被
//!   接受"，该次入参不留痕，取消这次写入不会让"另一份 buffer"被静默替代；
//!   缓冲满且底层受阻 ⇒ `Pending`（背压），`flush` 只发明确接受过的字节；
//! * 坏 tag ⇒ 零明文交付 + 双向熔断 + **立即释放底层传输**（真实 TCP 上对端
//!   能观察到连接已关闭、本端不再持有 socket）；截断不误作 EOF；合法 EOF 正常结束；
//! * partial read 固定 30s deadline：**每条记录独立锚定**（首字节起算、同一
//!   chunk 里跨记录也必须重锚、同记录碎片不刷新）、无首字节不启动；
//! * 写阻塞 30s deadline（同一记录重入不刷新；记录 commit 后重新锚定）；
//! * `poll_write` 重入不重复 seal（同一条记录不被二次封箱、nonce 不重用）；
//! * 末有效序号（`2^64-2`）发完后主动 flush + shutdown：收尾 **Pending 必须
//!   如实传播**（不得假报成功），read/write/flush/shutdown 四个入口都能推进
//!   收尾，`Done` 后释放底层；
//! * 单方向致命 ⇒ 另一方向也终止，且被 parked 的对向能被唤醒。
//!
//! 时间相关的用例统一用 `tokio::time::pause()`（关闭自动推进）+ 手工 `poll`，
//! 这样"到点即致命"与"没到点仍 Pending"都是确定的，不依赖真实调度。
//!
//! 关于"取消安全"：本文件不使用任何"调用方必须重传同一 buffer"的额外契约。
//! Tokio 的 `AsyncWrite` 只保证"返回 `Ready(Ok(n))` 的前 n 字节已被接受"，
//! 并不要求 `Pending` 之后调用方继续提供同一块 buffer；因此下面所有用例都按
//! "Pending ⇒ 本次未接受"这一条判定，而不是把调用方的取消当成非法用法。

use std::collections::VecDeque;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use rsetup_protocol::crypto::{
    MAX_PLAINTEXT_LEN, RecordLayerPair, SessionKeys, TxRecordLayer,
    client_record_layers_at_seq_for_test, server_record_layers_at_seq_for_test,
};
use rsetup_protocol::handshake_adapter::MAX_RECORD_BUFFER;
use rsetup_tunnel::secure_io::{RECORD_IO_DEADLINE, SecureIo, SecureIoFailure, secure_io_failure};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

const CLIENT_RANDOM: [u8; 32] = [0x11; 32];
const SERVER_RANDOM: [u8; 32] = [0x22; 32];
const SHARED: [u8; 32] = [0x5a; 32];

/// 同一会话密钥：**每次调用重新 derive**（HKDF 确定性 ⇒ key 材料一致、
/// seq 从 0 起、fatal 位独立），因为 `SessionKeys` 是一次性消耗品。
fn client_pair() -> RecordLayerPair {
    SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM)
        .expect("derive")
        .into_client_record_layers()
}

fn server_pair() -> RecordLayerPair {
    SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM)
        .expect("derive")
        .into_server_record_layers()
}

// =====================================================================
// 手工 poll 基础设施：paused time 下需要"推进时钟"与"驱动 future"解耦
// =====================================================================

#[derive(Default)]
struct WakeFlag(AtomicBool);

impl Wake for WakeFlag {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// 记录"是否被唤醒"的一次性 waker（手工驱动 future 用）。
fn test_waker() -> (Waker, Arc<WakeFlag>) {
    let flag = Arc::new(WakeFlag::default());
    (Waker::from(Arc::clone(&flag)), flag)
}

/// 手工对 `SecureIo`（或任意 `AsyncRead`）执行一次 `poll_read`。
fn poll_read_once<R: AsyncRead + Unpin>(io: &mut R, cap: usize) -> Poll<io::Result<Vec<u8>>> {
    let (waker, _flag) = test_waker();
    let mut cx = Context::from_waker(&waker);
    let mut data = vec![0u8; cap];
    let mut rb = ReadBuf::new(&mut data);
    let poll = AsyncRead::poll_read(Pin::new(io), &mut cx, &mut rb);
    // `rb` 借用了 `data`：它的最后一次使用到这里就结束了（`ReadBuf` 不实现
    // `Drop`，写 `drop(rb)` 没有作用，只会让借用看起来"需要"显式结束）。
    let filled = match poll {
        Poll::Pending => return Poll::Pending,
        Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
        Poll::Ready(Ok(())) => rb.filled().len(),
    };
    data.truncate(filled);
    Poll::Ready(Ok(data))
}

fn poll_write_once<W: AsyncWrite + Unpin>(io: &mut W, data: &[u8]) -> Poll<io::Result<usize>> {
    let (waker, _flag) = test_waker();
    let mut cx = Context::from_waker(&waker);
    AsyncWrite::poll_write(Pin::new(io), &mut cx, data)
}

/// 手工执行一次 `poll_flush`（`Pending` 与 `Ready` 都要能原样观察到）。
fn poll_flush_once<W: AsyncWrite + Unpin>(io: &mut W) -> Poll<io::Result<()>> {
    let (waker, _flag) = test_waker();
    let mut cx = Context::from_waker(&waker);
    AsyncWrite::poll_flush(Pin::new(io), &mut cx)
}

/// 手工执行一次 `poll_shutdown`。
fn poll_shutdown_once<W: AsyncWrite + Unpin>(io: &mut W) -> Poll<io::Result<()>> {
    let (waker, _flag) = test_waker();
    let mut cx = Context::from_waker(&waker);
    AsyncWrite::poll_shutdown(Pin::new(io), &mut cx)
}

/// 反复 `poll_flush` 直到 `Ready`，中间允许调用 `pump`（把 duplex 之类的底层
/// 腾空）。`max` 次仍不完成即 panic（防止用例静默卡死）。
#[track_caller]
fn flush_to_completion<W: AsyncWrite + Unpin>(io: &mut W, max: usize, mut pump: impl FnMut()) {
    for _ in 0..max {
        pump();
        match poll_flush_once(io) {
            Poll::Ready(Ok(())) => return,
            Poll::Ready(Err(e)) => panic!("flush 报错：{e}"),
            Poll::Pending => {}
        }
    }
    panic!("flush 在 {max} 次推进后仍未完成");
}

/// `io::Error` 没有 `PartialEq`，`Poll<io::Result<..>>` 因此不可直接 `assert_eq!`：
/// 期望 `Ready(Ok(..))` 的用例统一走这两个 helper（`Pending`/`Err` 一律 panic）。
#[track_caller]
fn ready_bytes(poll: Poll<io::Result<Vec<u8>>>) -> Vec<u8> {
    match poll {
        Poll::Ready(Ok(bytes)) => bytes,
        Poll::Ready(Err(e)) => panic!("期望 Ready(Ok(..))，实得 Err({e})"),
        Poll::Pending => panic!("期望 Ready(Ok(..))，实得 Pending"),
    }
}

#[track_caller]
fn ready_written(poll: Poll<io::Result<usize>>) -> usize {
    match poll {
        Poll::Ready(Ok(n)) => n,
        Poll::Ready(Err(e)) => panic!("期望 Ready(Ok(..))，实得 Err({e})"),
        Poll::Pending => panic!("期望 Ready(Ok(..))，实得 Pending"),
    }
}

// =====================================================================
// 可控底层 IO mock
// =====================================================================

#[derive(Clone)]
struct Shared {
    log: Arc<Mutex<Vec<u8>>>,
    readable: Arc<Mutex<VecDeque<u8>>>,
    eof: Arc<AtomicBool>,
    stall_write: Arc<AtomicBool>,
    fail_write: Arc<AtomicBool>,
    max_chunk: usize,
    stall_every: usize,
    flushes: Arc<AtomicUsize>,
    shutdowns: Arc<AtomicUsize>,
    /// 还有多少次 `poll_shutdown` 必须先返回 `Pending`（用来构造"关闭动作本身
    /// 会 Pending"的真实情形，检查上层有没有假报收尾成功）。
    shutdown_pending: Arc<AtomicUsize>,
    /// 还有多少次 `poll_flush` 必须先返回 `Pending`（构造收尾停在 **Flushing**
    /// 阶段的真实情形；`shutdown_pending` 构造的是停在 **ShuttingDown** 阶段）。
    flush_pending: Arc<AtomicUsize>,
    write_polls: Arc<AtomicUsize>,
    dropped: Arc<AtomicBool>,
    wakers: Arc<Mutex<Vec<Waker>>>,
}

struct MockIo {
    shared: Shared,
    read_buf: Vec<u8>,
}

impl MockIo {
    fn new(max_chunk: usize) -> (Self, Shared) {
        Self::with_stall(max_chunk, 0)
    }

    /// `stall_every = N > 0` ⇒ 每第 N 次底层 `poll_write` 返回 `Pending`，
    /// 用来逼出 `poll_write` 重入路径（不重新 seal）。
    fn with_stall(max_chunk: usize, stall_every: usize) -> (Self, Shared) {
        let shared = Shared {
            log: Arc::new(Mutex::new(Vec::new())),
            readable: Arc::new(Mutex::new(VecDeque::new())),
            eof: Arc::new(AtomicBool::new(false)),
            stall_write: Arc::new(AtomicBool::new(false)),
            fail_write: Arc::new(AtomicBool::new(false)),
            max_chunk,
            stall_every,
            flushes: Arc::new(AtomicUsize::new(0)),
            shutdowns: Arc::new(AtomicUsize::new(0)),
            shutdown_pending: Arc::new(AtomicUsize::new(0)),
            flush_pending: Arc::new(AtomicUsize::new(0)),
            write_polls: Arc::new(AtomicUsize::new(0)),
            dropped: Arc::new(AtomicBool::new(false)),
            wakers: Arc::new(Mutex::new(Vec::new())),
        };
        (
            Self {
                shared: shared.clone(),
                read_buf: Vec::new(),
            },
            shared,
        )
    }
}

impl Drop for MockIo {
    fn drop(&mut self) {
        self.shared.dropped.store(true, Ordering::SeqCst);
    }
}

impl AsyncRead for MockIo {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.read_buf.clear();
        {
            let mut q = this.shared.readable.lock().expect("readable lock");
            while this.read_buf.len() < buf.remaining() {
                match q.pop_front() {
                    Some(b) => this.read_buf.push(b),
                    None => break,
                }
            }
        }
        if this.read_buf.is_empty() && buf.remaining() > 0 {
            if !this.shared.eof.load(Ordering::SeqCst) {
                // 还没数据也没 EOF：登记 waker 后 Pending（由测试 feed()/wake_all() 唤醒）。
                this.shared
                    .wakers
                    .lock()
                    .expect("wakers lock")
                    .push(cx.waker().clone());
                return Poll::Pending;
            }
            return Poll::Ready(Ok(())); // 0 字节 = 干净 EOF
        }
        let take = this.read_buf.len().min(buf.remaining());
        buf.put_slice(&this.read_buf[..take]);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for MockIo {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        let polls = this.shared.write_polls.fetch_add(1, Ordering::SeqCst) + 1;
        if this.shared.stall_every > 0 && polls.is_multiple_of(this.shared.stall_every) {
            this.shared
                .wakers
                .lock()
                .expect("wakers lock")
                .push(cx.waker().clone());
            return Poll::Pending;
        }
        if this.shared.fail_write.load(Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::other("mock write failure")));
        }
        if this.shared.stall_write.load(Ordering::SeqCst) {
            this.shared
                .wakers
                .lock()
                .expect("wakers lock")
                .push(cx.waker().clone());
            return Poll::Pending;
        }
        let n = if this.shared.max_chunk == 0 {
            data.len()
        } else {
            data.len().min(this.shared.max_chunk)
        };
        if n == 0 {
            return Poll::Ready(Ok(0));
        }
        this.shared
            .log
            .lock()
            .expect("log lock")
            .extend_from_slice(&data[..n]);
        Poll::Ready(Ok(n))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.shared.flushes.fetch_add(1, Ordering::SeqCst);
        // 同 shutdown：可指定"接下来 n 次 flush 必须先 Pending"，用来构造收尾
        // 停在 Flushing 阶段的真实情形。
        if Shared::spend(&this.shared.flush_pending) {
            this.shared
                .wakers
                .lock()
                .expect("wakers lock")
                .push(cx.waker().clone());
            return Poll::Pending;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        // 先消耗"必须 Pending 的次数"：构造真实的 shutdown-Pending（例如 TCP
        // 发送缓冲未满时 shutdown 会推迟到内核返回），上层不得据此假报完成。
        if Shared::spend(&this.shared.shutdown_pending) {
            this.shared
                .wakers
                .lock()
                .expect("wakers lock")
                .push(cx.waker().clone());
            return Poll::Pending;
        }
        this.shared.shutdowns.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(Ok(()))
    }
}

impl Shared {
    fn feed(&self, bytes: &[u8]) {
        self.readable
            .lock()
            .expect("readable lock")
            .extend(bytes.iter().copied());
        self.wake_all();
    }
    fn set_eof(&self) {
        self.eof.store(true, Ordering::SeqCst);
        self.wake_all();
    }
    fn stall_writes(&self, stall: bool) {
        self.stall_write.store(stall, Ordering::SeqCst);
        self.wake_all();
    }
    fn fail_writes(&self, fail: bool) {
        self.fail_write.store(fail, Ordering::SeqCst);
        self.wake_all();
    }
    fn wake_all(&self) {
        let pending: Vec<Waker> = self.wakers.lock().expect("wakers lock").drain(..).collect();
        for w in pending {
            w.wake();
        }
    }
    /// 只唤醒**最后登记**的那一个 waker（= 最近一次进入 Pending 的那个任务）。
    /// 用来构造"只有读方向被底层叫醒"的偏斜调度，检验实现有没有把另一向搁死在
    /// 已经释放的底层对象上。
    fn wake_last(&self) {
        let mut wakers = self.wakers.lock().expect("wakers lock");
        if let Some(w) = wakers.pop() {
            drop(wakers);
            w.wake();
        }
    }
    /// 消耗一次"必须返回 Pending"的配额（无配额则 false）。
    fn spend(counter: &AtomicUsize) -> bool {
        loop {
            let cur = counter.load(Ordering::SeqCst);
            if cur == 0 {
                return false;
            }
            if counter
                .compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }
    fn logged(&self) -> Vec<u8> {
        self.log.lock().expect("log lock").clone()
    }
    fn flushes(&self) -> usize {
        self.flushes.load(Ordering::SeqCst)
    }
    fn shutdowns(&self) -> usize {
        self.shutdowns.load(Ordering::SeqCst)
    }
    fn write_polls(&self) -> usize {
        self.write_polls.load(Ordering::SeqCst)
    }
    fn dropped(&self) -> bool {
        self.dropped.load(Ordering::SeqCst)
    }
    /// 让接下来 `n` 次 `poll_shutdown` 返回 `Pending`。
    fn stall_shutdown(&self, n: usize) {
        self.shutdown_pending.store(n, Ordering::SeqCst);
        self.wake_all();
    }
    /// 立刻放行 shutdown（配额清零）但**不**唤醒任何已登记的 waker：
    /// 把"什么时候叫醒谁"完全交给测试自己控制。
    fn release_shutdown(&self) {
        self.shutdown_pending.store(0, Ordering::SeqCst);
    }
    /// 让接下来 `n` 次 `poll_flush` 返回 `Pending`（收尾停在 Flushing 阶段）。
    fn stall_flush(&self, n: usize) {
        self.flush_pending.store(n, Ordering::SeqCst);
        self.wake_all();
    }
}

// =====================================================================
// 真实 TCP + "是否被释放"探针
// =====================================================================

/// 包一层真实 `TcpStream`，只为了观察**本端是否真的把底层传输对象释放掉**
/// （drop = 关闭整条 TCP；只 `poll_shutdown(Write)` 不会 drop）。
#[derive(Debug)]
struct WatchedTcp {
    inner: TcpStream,
    dropped: Arc<AtomicBool>,
}

impl Drop for WatchedTcp {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl AsyncRead for WatchedTcp {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for WatchedTcp {
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

// =====================================================================
// 线上记录工具（模拟"对端只做记录层、不做 SecureIo"）
// =====================================================================

/// 把 wire 字节切成**恰好一条一条**的记录（4B BE 前缀 + 记录体）。
/// 末尾不足一条的部分作为 `tail` 返回，用于截断类断言。
fn split_records(bytes: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut out = Vec::new();
    let mut at = 0usize;
    while bytes.len() - at >= 4 {
        let len =
            u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        let total = 4 + len;
        if bytes.len() - at < total {
            break;
        }
        out.push(bytes[at..at + total].to_vec());
        at += total;
    }
    (out, bytes[at..].to_vec())
}

// =====================================================================
// 1. 基本往返：分片、多条、超过单记录的明文
// =====================================================================

#[tokio::test]
async fn duplex_roundtrip_fragmented_mixed_and_oversized() {
    let (a_io, b_io) = tokio::io::duplex(4096);
    let mut a = SecureIo::new(a_io, client_pair());
    let mut b = SecureIo::new(b_io, server_pair());

    let payloads: Vec<Vec<u8>> = vec![
        b"first".to_vec(),
        vec![0xaau8; 4095],                      // 与 duplex 容量错开的分片
        Vec::new(),                              // 空明文（0 字节 write 不封箱，见下条用例）
        vec![0x55u8; MAX_PLAINTEXT_LEN],         // 恰一条满记录
        vec![0x33u8; MAX_PLAINTEXT_LEN * 2 + 7], // 跨多条记录的大明文
    ];

    // 期望明文先算好（payloads 随后被 move 进写任务）。
    let expect: Vec<u8> = payloads.concat();
    let total = expect.len();
    let writer = tokio::spawn(async move {
        for p in &payloads {
            if p.is_empty() {
                // 0 字节写不产生记录，也不报错，并且必须报"接受 0 字节"。
                assert_eq!(
                    a.write(&[]).await.expect("empty write ok"),
                    0,
                    "空写入不得接受任何字节"
                );
                continue;
            }
            a.write_all(p).await.expect("write_all");
        }
        a.flush().await.expect("flush");
        a.shutdown().await.expect("shutdown");
    });

    let mut got = Vec::new();
    // 小缓冲循环读：把分片/跨记录/跨 poll 路径压一遍。
    let mut tmp = [0u8; 4096];
    while got.len() < total {
        let n = b.read(&mut tmp).await.expect("read");
        assert_ne!(n, 0, "对端在预期数据读满前提前 EOF");
        got.extend_from_slice(&tmp[..n]);
    }
    writer.await.expect("writer task");

    assert_eq!(got.len(), expect.len());
    // 抽样比对，避免 200KB 全量不等时刷屏。
    for (i, (g, e)) in got.iter().zip(expect.iter()).enumerate() {
        assert_eq!(g, e, "第 {i} 字节不等");
    }
    // 数据读满后对端已 shutdown ⇒ 干净 EOF。
    assert_eq!(b.read(&mut tmp).await.expect("eof read"), 0);
}

// =====================================================================
// 2. 零明文记录不得被误当 EOF；空记录之后的数据必须交付
// =====================================================================

#[tokio::test]
async fn empty_record_is_not_eof_and_data_after_it_is_delivered() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();

    // 明文 0 字节的合法记录（恰 16B Tag）：线长 20B。
    let empty_rec = peer.tx.seal(&[]).expect("seal empty");
    assert_eq!(empty_rec.len(), 20);
    let data_rec = peer.tx.seal(b"after empty").expect("seal data");

    shared.feed(&empty_rec);

    // 只喂了空记录 ⇒ 读必须继续等（Pending），**不能**返回 0 表示 EOF。
    match poll_read_once(&mut io, 64) {
        Poll::Pending => {}
        Poll::Ready(Ok(bytes)) => {
            panic!("空记录被误判为 EOF / 交付了 {:?} 字节", bytes.len());
        }
        Poll::Ready(Err(e)) => panic!("空记录读取报错：{e}"),
    }

    shared.feed(&data_rec);
    match poll_read_once(&mut io, 64) {
        Poll::Ready(Ok(bytes)) => assert_eq!(bytes, b"after empty"),
        other => panic!("空记录之后的数据未交付：{other:?}"),
    }
    assert!(!io.is_fatal(), "正常路径不得 fatal");
}

// =====================================================================
// 3. 坏 tag：零明文交付 + 双向熔断 + 底层被 shutdown
// =====================================================================

#[tokio::test]
async fn bad_tag_delivers_no_plaintext_and_terminates_both_directions() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();

    let mut good = peer.tx.seal(b"never delivered").expect("seal seq0");
    let tail_start = good.len() - 1;
    good[tail_start] ^= 0x80; // 破坏 tag
    // 坏记录之后再放一条对本端（seq 1）**完全合法**的记录：熔断之后它一个
    // 字节的明文都不许交付（先对照证明它在未熔断的 pair 上确实可解）。
    let after = peer.tx.seal(b"must never surface").expect("seal seq1");
    // 对照层与本端序号一致（rx 从 1 起），否则对照本身就失去意义。
    let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let mut control = server_record_layers_at_seq_for_test(keys, 0, 1);
    assert_eq!(
        control.rx.open(&after).expect("对照：该记录本身完全合法"),
        b"must never surface"
    );

    let mut wire = good.clone();
    wire.extend_from_slice(&after);
    shared.feed(&wire);

    let err = match poll_read_once(&mut io, 4096) {
        Poll::Ready(Err(e)) => e,
        Poll::Ready(Ok(bytes)) => panic!("坏 tag 却交付了 {} 字节明文", bytes.len()),
        Poll::Pending => panic!("坏 tag 未产生错误"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::AuthFailed),
        "错误原因应为认证失败：{err}"
    );
    assert!(io.is_fatal());

    // 粘滞：后续读仍是同一错误，不重同步、不交付后面的合法记录。
    match poll_read_once(&mut io, 4096) {
        Poll::Ready(Err(e)) => assert_eq!(secure_io_failure(&e), Some(SecureIoFailure::AuthFailed)),
        other => panic!("fatal 后读取应持续报错：{other:?}"),
    }
    // 对向（写）同样终止：双向失效，不允许"只停用读方向"。
    match poll_write_once(&mut io, b"still alive?") {
        Poll::Ready(Err(e)) => assert_eq!(secure_io_failure(&e), Some(SecureIoFailure::AuthFailed)),
        other => panic!("读侧致命后写侧必须终止：{other:?}"),
    }
    // R2：致命必须**立即释放底层传输对象**（drop ⇒ 整条 TCP 关闭）。
    // 只 `poll_shutdown(Write)` 时本断言为 false —— 本端仍持有整条 TCP。
    assert!(
        shared.dropped(),
        "致命后底层传输必须已被释放（drop），而不是只 shutdown 写半边"
    );
}

// =====================================================================
// 4. 截断不是 EOF；干净 EOF 仍是 EOF
// =====================================================================

#[tokio::test]
async fn truncated_record_is_fatal_not_eof() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();

    let full = peer.tx.seal(b"full record").expect("seal");
    let partial = peer.tx.seal(b"half record").expect("seal 1");
    let cut = &partial[..partial.len() - 5];

    let mut wire = full.clone();
    wire.extend_from_slice(cut);
    shared.feed(&wire);
    shared.set_eof();

    // 第一条完整可解 ⇒ 先交付，然后才在截断处终止。
    assert_eq!(
        ready_bytes(poll_read_once(&mut io, 4096)),
        b"full record".to_vec()
    );
    match poll_read_once(&mut io, 4096) {
        Poll::Ready(Err(e)) => {
            assert_eq!(
                secure_io_failure(&e),
                Some(SecureIoFailure::Truncated),
                "{e}"
            )
        }
        Poll::Ready(Ok(bytes)) => panic!("截断被当成 EOF/交付 {:?}", bytes),
        Poll::Pending => panic!("截断处应立刻终止而不是继续等"),
    }
    assert!(io.is_fatal());
}

#[tokio::test]
async fn clean_eof_after_last_record_is_ok() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();

    let full = peer.tx.seal(b"last one").expect("seal");
    shared.feed(&full);
    shared.set_eof();

    assert_eq!(
        ready_bytes(poll_read_once(&mut io, 4096)),
        b"last one".to_vec()
    );
    // 干净 EOF：0 字节（而不是错误、也不是 Pending）。
    assert_eq!(ready_bytes(poll_read_once(&mut io, 4096)), Vec::new());
    assert!(!io.is_fatal(), "干净 EOF 不是致命错误");
}

// =====================================================================
// 5. 超限 / 非法长度前缀：到达即拒绝
// =====================================================================

#[tokio::test]
async fn oversized_and_undersized_prefix_rejected_immediately() {
    // 声明 65537（>65536）：不 payload、只 4B 前缀也必须立刻终止。
    for (name, prefix, want) in [
        (
            "65537",
            (65_537u32).to_be_bytes().to_vec(),
            SecureIoFailure::Framing,
        ),
        (
            "15",
            (15u32).to_be_bytes().to_vec(),
            SecureIoFailure::Framing,
        ),
        ("0", (0u32).to_be_bytes().to_vec(), SecureIoFailure::Framing),
    ] {
        let (mock, shared) = MockIo::new(0);
        let mut io = SecureIo::new(mock, server_pair());
        shared.feed(&prefix);
        match poll_read_once(&mut io, 4096) {
            Poll::Ready(Err(e)) => assert_eq!(
                secure_io_failure(&e),
                Some(want),
                "{name} 前缀应立刻判致命：{e}"
            ),
            other => panic!("{name} 前缀应立刻判致命，实得 {other:?}"),
        }
        assert!(io.is_fatal(), "{name} 前缀后必须 fatal");
    }
}

#[tokio::test]
async fn buffered_bytes_never_exceed_record_budget() {
    // 逐字节喂入一条满记录 + 一条超预算的不完整记录：SecureIo 侧的未完成
    // 缓冲必须始终 ≤ 65540（协议 §5.2 接收缓冲上限），由 RecordReassembler
    // 的同一实现兜住。
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();
    let rec = peer
        .tx
        .seal(&vec![0x9cu8; MAX_PLAINTEXT_LEN])
        .expect("seal max");
    assert_eq!(rec.len(), MAX_RECORD_BUFFER);
    let mut got = Vec::new();
    for byte in &rec {
        shared.feed(&[*byte]);
        if let Poll::Ready(Ok(bytes)) = poll_read_once(&mut io, 8) {
            got.extend_from_slice(&bytes);
        }
        assert!(!io.is_fatal(), "逐字节喂满记录不该致命");
    }
    while got.len() < MAX_PLAINTEXT_LEN {
        if let Poll::Ready(Ok(bytes)) = poll_read_once(&mut io, MAX_PLAINTEXT_LEN + 1) {
            got.extend_from_slice(&bytes);
        }
    }
    assert_eq!(got, vec![0x9cu8; MAX_PLAINTEXT_LEN]);
}

// =====================================================================
// 6. partial read 固定 30s deadline（paused time）
// =====================================================================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn read_partial_deadline_is_30s_from_first_byte_and_not_reset() {
    // 时钟由属性冻结，只按显式 advance 推进。
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();

    let full = peer.tx.seal(b"never completes").expect("seal");
    let (records, _) = split_records(&full);
    let rec = &records[0];
    // 首字节到达 ⇒ 计时起点。
    shared.feed(&rec[..1]);

    let deadline = RECORD_IO_DEADLINE;
    assert_eq!(
        deadline,
        Duration::from_secs(30),
        "记录 partial deadline 固定 30s"
    );

    // 中途碎片**不得**重置计时：在 t=10s 再喂一点。
    let step = Duration::from_millis(500);
    let mut fired_at: Option<Duration> = None;
    let mut elapsed = Duration::ZERO;
    let mut fed_mid = false;
    while fired_at.is_none() && elapsed < deadline * 3 {
        tokio::time::advance(step).await;
        elapsed += step;
        if !fed_mid && elapsed >= Duration::from_secs(10) {
            fed_mid = true;
            shared.feed(&rec[1..rec.len() / 2]);
        }
        if let Poll::Ready(Err(e)) = poll_read_once(&mut io, 4096) {
            assert_eq!(
                secure_io_failure(&e),
                Some(SecureIoFailure::ReadDeadline),
                "{e}"
            );
            fired_at = Some(elapsed);
        }
    }
    let fired_at = fired_at.expect("partial read 必须在 30s 期限到点致命");
    // 首字节起算 30s：误差只允许一个 advance 步长。
    assert!(
        fired_at > deadline - step && fired_at <= deadline + step,
        "期限应为 30s（首字节起算、碎片不重置），实测 {fired_at:?}"
    );
    // 到点后记录层已作废：写方向同样终止。
    match poll_write_once(&mut io, b"x") {
        Poll::Ready(Err(_)) => {}
        other => panic!("读期限致命后写方向必须终止：{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn no_first_byte_starts_no_partial_read_deadline() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();

    // 完全没有首字节：等待 10 倍期限也不得触发 partial read deadline。
    let step = Duration::from_secs(5);
    for _ in 0..8 {
        tokio::time::advance(step).await;
        match poll_read_once(&mut io, 4096) {
            Poll::Pending => {}
            other => panic!("无首字节时不得启动 partial read deadline：{other:?}"),
        }
    }
    assert!(!io.is_fatal(), "静默 40s 不得 fatal");

    // 之后来了完整记录 ⇒ 正常交付，证明连接确实还活着。
    let rec = peer.tx.seal(b"late but fine").expect("seal");
    shared.feed(&rec);
    assert_eq!(
        ready_bytes(poll_read_once(&mut io, 4096)),
        b"late but fine".to_vec()
    );
}

/// 回归：期限到点必须**唤醒**已 park 的任务（新武装的 `Sleep` 若未登记
/// waker，任务会永久挂起而不是在 30s 终止）。用真实任务 + 手工 `advance`
/// 验证，不靠测试自己反复 poll。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn read_deadline_wakes_a_parked_reader_task() {
    let (mock, shared) = MockIo::new(0);
    let io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();
    let rec = peer.tx.seal(b"partial").expect("seal");
    shared.feed(&rec[..1]); // 只有首字节 ⇒ 武装 30s 部分记录期限

    let (mut r, _w) = tokio::io::split(io);
    let handle = tokio::spawn(async move {
        let mut buf = vec![0u8; 64];
        r.read(&mut buf).await
    });
    tokio::task::yield_now().await; // 让读任务 poll 一次后 park
    assert!(
        !handle.is_finished(),
        "首字节未凑齐记录时应 Pending，不该提前结束"
    );

    tokio::time::advance(RECORD_IO_DEADLINE + Duration::from_millis(1)).await;
    tokio::task::yield_now().await; // 计时到点 ⇒ 必须有人唤醒它
    assert!(
        handle.is_finished(),
        "部分记录期限到点必须唤醒 park 中的读任务（否则计时形同虚设）"
    );
    let err = handle
        .await
        .expect("读任务不 panic")
        .expect_err("期限到点是错误而不是 EOF");
    assert_eq!(secure_io_failure(&err), Some(SecureIoFailure::ReadDeadline));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn write_deadline_wakes_a_parked_writer_task() {
    let (mock, shared) = MockIo::new(0);
    let io = SecureIo::new(mock, client_pair());
    shared.stall_writes(true);

    let (_r, mut w) = tokio::io::split(io);
    let handle = tokio::spawn(async move {
        // 第一次写入会被"接受即 Ready"（有界缓冲，至多一条记录）；此后再写
        // 就落到 Pending 上，本用例检查的正是 park 之后计时到点能否唤醒它。
        let n = w.write(&[0x33u8; 8192][..]).await?;
        assert_eq!(n, 8192, "首次写入应被整段接受");
        w.write_all(b"one byte too many").await
    });
    tokio::task::yield_now().await; // 首次受阻 ⇒ 武装 30s 写期限后 park
    assert!(
        !handle.is_finished(),
        "接受缓冲已满且底层被 stall 时应 Pending"
    );

    tokio::time::advance(RECORD_IO_DEADLINE + Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(
        handle.is_finished(),
        "写出受阻期限到点必须唤醒 park 中的写任务"
    );
    let err = handle
        .await
        .expect("写任务不 panic")
        .expect_err("期限到点是错误而不是完成");
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::WriteDeadline)
    );
    // 期限致命 ⇒ 已封箱但未确认写出的记录不得留下"可重发"的假象。
    assert!(shared.logged().is_empty(), "stall 期间不应有字节落到底层");
}

// =====================================================================
// 7. 写阻塞 30s deadline
// =====================================================================

#[tokio::test(start_paused = true)]
async fn blocked_write_deadline_is_30s() {
    // buffered 写方向：底层被 stall 时，先"接受"到上限（每次都是 Ready），
    // 缓冲满且推不动之后才 Pending；30s 期限从**首次写出受阻**起算，重入不刷新。
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, client_pair());
    shared.stall_writes(true);

    let step = Duration::from_millis(500);
    let payload = vec![0x7fu8; 2048];
    // 底层被 stall：第一条记录仍可被**接受**（Ready），此后在途记录推不动 ⇒
    // 后续写入只可能 Pending（背压），绝不报错、也绝不报 0 字节。
    let mut accepted = 0usize;
    let mut pending_seen = 0usize;
    for _ in 0..16 {
        match poll_write_once(&mut io, &payload) {
            Poll::Ready(Ok(n)) => {
                assert_ne!(n, 0, "非空入参不得报 0 字节");
                accepted += n;
            }
            Poll::Pending => pending_seen += 1,
            Poll::Ready(Err(e)) => panic!("有界接受阶段不得报错：{e}"),
        }
    }
    assert!(
        accepted > 0 && accepted <= MAX_PLAINTEXT_LEN,
        "必须能接受不超过一条记录的明文，实测 {accepted}"
    );
    assert!(pending_seen > 0, "在途记录未推完时必须背压（Pending）");
    assert!(
        shared.logged().is_empty(),
        "stall 期间不应有字节落到底层（接受 ≠ 上线）"
    );

    let mut fired_at: Option<Duration> = None;
    let mut elapsed = Duration::ZERO;
    let mut polls = 1usize;
    while fired_at.is_none() && elapsed < RECORD_IO_DEADLINE * 3 {
        tokio::time::advance(step).await;
        elapsed += step;
        polls += 1;
        match poll_write_once(&mut io, &payload) {
            // 缓冲仍满、底层仍 stall ⇒ 只可能 Pending（不得接受、也不得报 0）
            Poll::Ready(Ok(n)) => panic!("缓冲已满且底层 stall 时不得接受 {n} 字节"),
            Poll::Ready(Err(e)) => {
                assert_eq!(
                    secure_io_failure(&e),
                    Some(SecureIoFailure::WriteDeadline),
                    "{e}"
                );
                fired_at = Some(elapsed);
            }
            Poll::Pending => {}
        }
    }
    let fired_at = fired_at.expect("写阻塞必须在 30s 期限到点致命");
    assert!(
        fired_at > RECORD_IO_DEADLINE - step && fired_at <= RECORD_IO_DEADLINE + step,
        "写期限应为首次受阻起 30s，实测 {fired_at:?}（重入 {polls} 次）"
    );
    assert!(io.is_fatal());
}

#[tokio::test(start_paused = true)]
async fn blocked_write_deadline_arms_at_first_pending() {
    // "写入阻塞出现的第一瞬时"起算：前 10s 写得动、之后卡住 ⇒ 期限从卡住那刻算。
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, client_pair());
    let payload = vec![0x11u8; 128];
    assert_eq!(ready_written(poll_write_once(&mut io, &payload)), 128);

    shared.stall_writes(true);
    let step = Duration::from_millis(500);
    let mut elapsed = Duration::ZERO;
    let mut fired_at: Option<Duration> = None;
    while fired_at.is_none() && elapsed < RECORD_IO_DEADLINE * 2 {
        tokio::time::advance(step).await;
        elapsed += step;
        if let Poll::Ready(Err(e)) = poll_write_once(&mut io, &payload) {
            assert_eq!(
                secure_io_failure(&e),
                Some(SecureIoFailure::WriteDeadline),
                "{e}"
            );
            fired_at = Some(elapsed);
        }
    }
    let fired_at = fired_at.expect("写阻塞必须在 30s 到点致命");
    assert!(
        fired_at > RECORD_IO_DEADLINE - step && fired_at <= RECORD_IO_DEADLINE + step,
        "期限应从出现阻塞的第一瞬时起算，实测 {fired_at:?}"
    );
}

// =====================================================================
// 8. 写出未完成不得重复 seal（nonce 不重用）
// =====================================================================

#[tokio::test]
async fn pending_write_reentry_does_not_reseal_or_reuse_nonce() {
    // 底层每次只吃 7 字节、且每第 2 次 poll 返回 Pending ⇒ 一条记录要重入
    // 很多次才能写完；重入期间绝不允许重新 seal（否则 nonce 重用 + 对端
    // 序号永久错位）。
    let (mock, shared) = MockIo::with_stall(7, 2);
    let mut io = SecureIo::new(mock, client_pair());

    let payloads: [&[u8]; 3] = [b"alpha", b"bravo", b"charlie"];
    for p in payloads {
        let mut off = 0usize;
        let mut guard = 0usize;
        loop {
            guard += 1;
            assert!(guard < 500, "写出重入次数异常，疑似无法推进");
            match poll_write_once(&mut io, &p[off..]) {
                Poll::Ready(Ok(n)) => {
                    assert_ne!(n, 0, "poll_write 不得在未完成时报 0 字节");
                    off += n;
                    if off == p.len() {
                        break;
                    }
                }
                Poll::Ready(Err(e)) => panic!("分片写出报错：{e}"),
                Poll::Pending => {}
            }
        }
    }
    // 确实发生了多次重入（否则本用例是空转）。
    assert!(
        shared.write_polls() > 6,
        "底层 poll 次数过少：{}",
        shared.write_polls()
    );
    // buffered 契约：`Ready(Ok(n))` 只代表"接受"。最后一条记录可能只推了
    // 7 字节；要确认"整条上线"必须 flush（这正是 flush 的职责）。
    flush_to_completion(&mut io, 500, || {});
    let wire = shared.logged();
    let (records, tail) = split_records(&wire);
    assert!(tail.is_empty(), "写出尾部残留：{tail:?}");
    assert_eq!(records.len(), 3, "每条明文应恰有一条记录");
    // 记录必须互不相同（nonce/seq 单调），且能被对端按 seq 0,1,2 依次解封。
    assert_ne!(records[0], records[1]);
    let mut peer = server_pair();
    for (i, rec) in records.iter().enumerate() {
        let pt = peer
            .rx
            .open(rec)
            .unwrap_or_else(|e| panic!("第 {i} 条记录解封失败：{e}"));
        assert_eq!(pt, payloads[i].to_vec(), "第 {i} 条记录明文不等");
    }
    // 线上总字节数 == 三条记录之和 ⇒ 没有任何记录被重复写出/重复 seal。
    let expect_bytes: usize = payloads.iter().map(|p| p.len() + 16 + 4).sum();
    assert_eq!(
        wire.len(),
        expect_bytes,
        "线上字节数不等说明重复写出或重复 seal"
    );
}

#[tokio::test]
async fn underlying_write_failure_is_fatal_and_sticky() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, client_pair());
    shared.fail_writes(true);
    let err = match poll_write_once(&mut io, b"boom") {
        Poll::Ready(Err(e)) => e,
        other => panic!("底层写失败应报错：{other:?}"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::WriteFailed),
        "{err}"
    );
    assert!(io.is_fatal());
    match poll_write_once(&mut io, b"again") {
        Poll::Ready(Err(e)) => {
            assert_eq!(secure_io_failure(&e), Some(SecureIoFailure::WriteFailed))
        }
        other => panic!("fatal 后必须持续报错：{other:?}"),
    }
    // 对向读取同样终止 + 底层被释放。
    match poll_read_once(&mut io, 64) {
        Poll::Ready(Err(_)) => {}
        other => panic!("写侧致命后读方向必须终止：{other:?}"),
    }
    assert!(shared.dropped(), "写侧致命后必须立即释放底层传输");
}

// =====================================================================
// 9. 末有效序号：写完主动 flush + shutdown
// =====================================================================

#[tokio::test]
async fn last_valid_sequence_flushes_and_shuts_down_without_another_write() {
    // 发送方向从末有效序号 2^64-2 开始：写完这一条就必须 flush + 关闭，
    // 不能等下一次 write 才收尾。
    let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let pair = client_record_layers_at_seq_for_test(keys, u64::MAX - 1, 0);
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, pair);

    let payload = b"the very last record";
    let n = match poll_write_once(&mut io, payload) {
        Poll::Ready(Ok(n)) => n,
        other => panic!("末有效序号仍可写出：{other:?}"),
    };
    assert_eq!(n, payload.len());

    assert!(
        shared.flushes() >= 1,
        "末有效序号发完必须主动 flush，实测 {}",
        shared.flushes()
    );
    assert!(
        shared.shutdowns() >= 1,
        "末有效序号发完必须主动 shutdown，实测 {}",
        shared.shutdowns()
    );
    // §5.2 要求"平滑关闭整条 TCP"：收尾 Done 之后底层必须被释放。
    assert!(shared.dropped(), "收尾完成（Done）后必须释放底层传输");
    // 不再允许发送任何记录。
    match poll_write_once(&mut io, b"one too many") {
        Poll::Ready(Err(e)) => assert_eq!(
            secure_io_failure(&e),
            Some(SecureIoFailure::SequenceExhausted),
            "{e}"
        ),
        other => panic!("末有效序号之后不得再发记录：{other:?}"),
    }
    // 该记录可被对端按 seq 2^64-2 正常解封。
    let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let mut peer = server_record_layers_at_seq_for_test(keys, 0, u64::MAX - 1);
    let (records, tail) = split_records(&shared.logged());
    assert!(tail.is_empty());
    assert_eq!(records.len(), 1);
    assert_eq!(
        peer.rx.open(&records[0]).expect("open last seq"),
        b"the very last record"
    );
}

#[tokio::test]
async fn receive_side_sequence_exhausted_terminates_both_ways() {
    // 期望序号已到 2^64-1：收到任何记录都立即终止整条连接。
    let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let pair = server_record_layers_at_seq_for_test(keys, 0, u64::MAX);
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, pair);
    let mut peer = client_pair();
    let rec = peer.tx.seal(b"too late").expect("seal");
    shared.feed(&rec);
    match poll_read_once(&mut io, 4096) {
        Poll::Ready(Err(e)) => assert_eq!(
            secure_io_failure(&e),
            Some(SecureIoFailure::SequenceExhausted),
            "{e}"
        ),
        other => panic!("序号耗尽应立即终止：{other:?}"),
    }
    match poll_write_once(&mut io, b"x") {
        Poll::Ready(Err(_)) => {}
        other => panic!("序号耗尽后写方向也必须终止：{other:?}"),
    }
    assert!(shared.dropped(), "接收侧序号耗尽同样要释放底层");
}

// =====================================================================
// 10. 双向熔断 + parked 对向可被唤醒
// =====================================================================

#[tokio::test(flavor = "current_thread")]
async fn fatal_terminates_parked_other_direction() {
    let (a_io, b_io) = tokio::io::duplex(32); // 容量很小：写侧很快卡住
    let secure = SecureIo::new(a_io, client_pair());
    let (mut r, mut w) = tokio::io::split(secure);

    let mut read_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match r.read(&mut buf).await {
                Ok(0) => return Ok::<_, io::Error>(()),
                Ok(_) => {}
                Err(e) => return Err(e),
            }
        }
    });

    // 写入量必须**超过有界接受缓冲**（65520B），写侧才会真正 park：
    // 缓冲区内的写入是"接受即 Ready"，不构成 park 点。
    let mut write_task =
        tokio::spawn(async move { w.write_all(&[0u8; MAX_PLAINTEXT_LEN * 4]).await });

    // 让两侧都先进入 Pending（写侧一定卡满 32B 缓冲）。
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // 从对端注入一条 tag 被破坏的记录 ⇒ 读侧认证失败。
    let mut peer = server_pair();
    let mut rec = peer.tx.seal(b"boom").expect("seal");
    let last = rec.len() - 1;
    rec[last] ^= 0x01;
    let mut b_io = b_io;
    b_io.write_all(&rec).await.expect("inject");

    let read_res = tokio::time::timeout(Duration::from_secs(5), &mut read_task)
        .await
        .expect("读侧应立刻得到认证错误")
        .expect("读任务不 panic")
        .expect_err("读侧必须是错误而不是 EOF");
    assert_eq!(
        secure_io_failure(&read_res),
        Some(SecureIoFailure::AuthFailed),
        "{read_res}"
    );

    // 关键断言：写侧此前 parked，也必须因双向熔断被**唤醒并报错**，而不是永远卡住。
    let write_res = tokio::time::timeout(Duration::from_secs(5), &mut write_task)
        .await
        .expect("写侧必须被唤醒（双向终止）")
        .expect("写任务不 panic");
    assert!(write_res.is_err(), "写侧应返回错误：{write_res:?}");
}

// =====================================================================
// 11. flush / shutdown 语义
// =====================================================================

#[tokio::test]
async fn shutdown_flushes_buffered_record_then_closes() {
    let (mock, shared) = MockIo::new(7); // 分片：shutdown 前必须把半截记录推完
    let mut io = SecureIo::new(mock, client_pair());

    let payload = vec![0x4au8; 900];
    let mut off = 0usize;
    while off < payload.len() {
        match poll_write_once(&mut io, &payload[off..]) {
            Poll::Ready(Ok(n)) => off += n,
            other => panic!("写出异常：{other:?}"),
        }
    }
    let before = shared.shutdowns();
    let (waker, _flag) = test_waker();
    let mut cx = Context::from_waker(&waker);
    match AsyncWrite::poll_shutdown(Pin::new(&mut io), &mut cx) {
        Poll::Ready(Ok(())) => {}
        other => panic!("shutdown 应完成：{other:?}"),
    }
    assert!(shared.shutdowns() > before, "shutdown 必须下传底层");
    let wire = shared.logged();
    assert_eq!(wire.len(), 4 + 900 + 16, "半截记录必须被推完");
    let (records, tail) = split_records(&wire);
    assert!(tail.is_empty() && records.len() == 1);
    // 且明文完整 ⇒ 不存在"shutdown 截断半条记录"。
    let mut peer = server_pair();
    assert_eq!(peer.rx.open(&records[0]).expect("open"), payload);
}

#[tokio::test]
async fn flush_drains_buffered_record() {
    let (mock, shared) = MockIo::new(5);
    let mut io = SecureIo::new(mock, client_pair());
    let payload = vec![0x22u8; 400];
    let mut off = 0usize;
    while off < payload.len() {
        match poll_write_once(&mut io, &payload[off..]) {
            Poll::Ready(Ok(n)) => off += n,
            other => panic!("写出异常：{other:?}"),
        }
    }
    let (waker, _flag) = test_waker();
    let mut cx = Context::from_waker(&waker);
    match AsyncWrite::poll_flush(Pin::new(&mut io), &mut cx) {
        Poll::Ready(Ok(())) => {}
        other => panic!("flush 应完成：{other:?}"),
    }
    assert!(shared.flushes() >= 1);
    let (records, tail) = split_records(&shared.logged());
    assert!(tail.is_empty());
    assert_eq!(records.len(), 1);
}

// =====================================================================
// 12. drop ⇒ 底层被关闭（不在旧连接重同步）
// =====================================================================

#[tokio::test]
async fn dropping_secure_io_drops_underlying_transport() {
    let (mock, shared) = MockIo::new(0);
    let io = SecureIo::new(mock, client_pair());
    assert!(!shared.dropped());
    drop(io);
    assert!(
        shared.dropped(),
        "SecureIo 必须拥有并在 drop 时释放底层（关闭 TCP）"
    );
}

#[tokio::test]
async fn constructed_over_fatal_pair_is_fatal_immediately() {
    let mut pair = client_pair();
    pair.tx.seal(&vec![0u8; MAX_PLAINTEXT_LEN + 1]).ok();
    assert!(pair.is_fatal());

    let (mock, _shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, pair);
    assert!(io.is_fatal(), "已熔断的记录层不得被当成新连接使用");
    match poll_read_once(&mut io, 64) {
        Poll::Ready(Err(e)) => assert_eq!(
            secure_io_failure(&e),
            Some(SecureIoFailure::PairFatal),
            "{e}"
        ),
        other => panic!("熔断 pair 构造出的 SecureIo 应立即报错：{other:?}"),
    }
    match poll_write_once(&mut io, b"x") {
        Poll::Ready(Err(_)) => {}
        other => panic!("熔断 pair 的写方向也必须终止：{other:?}"),
    }
}

// =====================================================================
// 13. R1：buffered AsyncWrite 契约（接受即 Ready；Pending 不接受；取消不串数据）
// =====================================================================

/// `Poll<io::Result<_>>` 不可 `assert_eq!`（`io::Error` 没有 `PartialEq`），
/// 期望 `Pending` 的用例统一走这个 helper。
#[track_caller]
fn assert_pending<T: std::fmt::Debug>(what: &str, poll: Poll<io::Result<T>>) {
    match poll {
        Poll::Pending => {}
        Poll::Ready(Ok(v)) => panic!("{what} 应返回 Pending，实得 Ready(Ok({v:?}))"),
        Poll::Ready(Err(e)) => panic!("{what} 应返回 Pending，实得 Err({e})"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_pending_write_cannot_be_answered_with_another_buffer() {
    // 审查反例 R1：真实 duplex + 真实任务 + 真实取消（abort）。
    // 已 Ready 接受的记录写不进小 duplex ⇒ 之后任何新写入只可能 `Pending`；
    // 那次 `Pending` 的写入被取消之后，同一份新 buffer 再写才允许上线，
    // 且线上顺序必须仍是"旧数据 → 新数据"，绝不允许把新数据提前或替换旧数据。
    let (a_io, mut b) = tokio::io::duplex(64);
    let mut w = SecureIo::new(a_io, client_pair());

    let old = vec![0xa1u8; MAX_PLAINTEXT_LEN];
    let new: &[u8] = b"NEW-3";

    // 1) 一次 write_all：整段被"接受"（Ready），但一条满记录（65540B 线上）
    //    写不进 64B 的 duplex ⇒ 该记录仍在途，后续写入必须背压。
    w.write_all(&old).await.expect("首条记录应被整段接受");

    // 2) 第二次写入必然 park ⇒ 50ms 后由调用方**取消**（timeout 触发即 drop
    //    掉这个 `Pending` 的 future，这是真实的写入取消路径）。
    let cancelled = tokio::time::timeout(Duration::from_millis(50), w.write_all(new)).await;
    assert!(
        cancelled.is_err(),
        "在途记录未推完时新写入必须停在 Pending（被接受或报错都不对），实得 {cancelled:?}"
    );

    // 3) 一边把对端读空（真实 await），一边把 NEW 写完并 flush。
    let total = 4 + MAX_PLAINTEXT_LEN + 16 + 4 + new.len() + 16;
    let (write_res, wire) = tokio::join!(
        async {
            w.write_all(new).await?;
            w.flush().await
        },
        async {
            let mut wire = Vec::new();
            let mut tmp = [0u8; 8192];
            while wire.len() < total {
                match b.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => wire.extend_from_slice(&tmp[..n]),
                    Err(e) => panic!("对端读取失败：{e}"),
                }
            }
            wire
        }
    );
    write_res.expect("腾空底层后 NEW 必须可被接受，并且 flush 必须完成");

    // 4) 线上明文必须恰是 OLD 紧接 NEW。
    let (records, tail) = split_records(&wire);
    assert!(tail.is_empty(), "线上残留不完整记录：{} 字节", tail.len());
    assert_eq!(records.len(), 2, "OLD 一条满记录 + NEW 一条");
    let mut peer = server_pair();
    let p0 = peer.rx.open(&records[0]).expect("open r0");
    let p1 = peer.rx.open(&records[1]).expect("open r1");
    assert_eq!(p0, old, "已 Ready 接受的 OLD 必须完整上线，不得被替代");
    assert_eq!(p1, new.to_vec(), "NEW 只能在真正被接受之后出现一次");
}

#[tokio::test]
async fn flush_after_pending_write_sends_only_accepted_bytes() {
    // 同一条性质的 mock 版本：Pending 的那次写入连一个字节都不许上线。
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, client_pair());
    shared.stall_writes(true);

    let a = vec![0xa1u8; MAX_PLAINTEXT_LEN];
    let mut accepted = 0usize;
    for _ in 0..100 {
        match poll_write_once(&mut io, &a[accepted..]) {
            Poll::Ready(Ok(n)) => accepted += n,
            Poll::Pending => break,
            Poll::Ready(Err(e)) => panic!("接受阶段报错：{e}"),
        }
    }
    assert_eq!(accepted, MAX_PLAINTEXT_LEN);

    assert_pending("缓冲满时的新写入", poll_write_once(&mut io, b"B7"));
    assert_pending("stall 期间的 flush", poll_flush_once(&mut io));

    // 放行底层 ⇒ flush 只能把**此前已接受**的 A 推完。
    shared.stall_writes(false);
    match poll_flush_once(&mut io) {
        Poll::Ready(Ok(())) => {}
        other => panic!("放行后 flush 应完成：{other:?}"),
    }
    let (records, tail) = split_records(&shared.logged());
    assert!(tail.is_empty());
    assert_eq!(records.len(), 1, "未接受的 B7 不得被 flush 发出");
    let mut peer = server_pair();
    assert_eq!(peer.rx.open(&records[0]).expect("open"), a);

    // B7 只有在这一次真的被接受之后才允许出现在线上。
    assert_eq!(ready_written(poll_write_once(&mut io, b"B7")), 2);
    match poll_flush_once(&mut io) {
        Poll::Ready(Ok(())) => {}
        other => panic!("flush 应完成：{other:?}"),
    }
    let (records, _) = split_records(&shared.logged());
    assert_eq!(records.len(), 2);
    assert_eq!(
        peer.rx.open(&records[1]).expect("open b7"),
        b"B7".to_vec(),
        "B7 只能出现一次且必须在被接受之后"
    );
}

#[tokio::test]
async fn caller_may_reuse_or_resize_buffer_after_ready_accept() {
    // `Ready(Ok(n))` 之后入参就归调用方了：立刻改写同一块 buffer、或下一次
    // 传更短的 buffer，都是完全合法的用法（旧实现把"换了 buffer/变短"当成
    // 契约违规直接拉闸，属于额外契约）。
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, client_pair());

    let mut buf = b"alpha".to_vec();
    assert_eq!(ready_written(poll_write_once(&mut io, &buf)), 5);
    for b in buf.iter_mut() {
        *b = b'z';
    }
    assert_eq!(ready_written(poll_write_once(&mut io, b"x")), 1);
    flush_to_completion(&mut io, 50, || {});

    let (records, tail) = split_records(&shared.logged());
    assert!(tail.is_empty());
    assert_eq!(records.len(), 2);
    let mut peer = server_pair();
    assert_eq!(
        peer.rx.open(&records[0]).expect("open r0"),
        b"alpha".to_vec(),
        "接受时必须已复制入参：调用方随后改写不得影响上线内容"
    );
    assert_eq!(peer.rx.open(&records[1]).expect("open r1"), b"x".to_vec());
    assert!(!io.is_fatal(), "入参变短/改写不是契约违规，不得拉闸");
}

// =====================================================================
// 14. R2：致命必须立即释放底层（真实 TCP 可观察）
// =====================================================================

#[tokio::test]
async fn fatal_over_real_tcp_releases_socket_and_closes_connection() {
    // 只 `poll_shutdown(Write)` 的后果：对端读到 FIN，但仍能继续写，本端也
    // 仍持有整条 TCP。正确的致命是**立即 drop 底层**（整条连接消失）。
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let mut peer = TcpStream::connect(addr).await.expect("connect");
    let (server_side, _) = listener.accept().await.expect("accept");

    let dropped = Arc::new(AtomicBool::new(false));
    let watched = WatchedTcp {
        inner: server_side,
        dropped: Arc::clone(&dropped),
    };
    let mut io = SecureIo::new(watched, server_pair());

    let mut peer_layer = client_pair();
    let mut rec = peer_layer.tx.seal(b"bad tag").expect("seal");
    let last = rec.len() - 1;
    rec[last] ^= 0x40;
    peer.write_all(&rec).await.expect("inject");

    // 用真实的 await 读（TCP 首个 poll 可能还没有数据），拿到错误即可。
    let mut probe = [0u8; 4096];
    let err = tokio::time::timeout(Duration::from_secs(5), io.read(&mut probe))
        .await
        .expect("等待认证失败超时")
        .expect_err("坏 tag 必须是错误而不是明文/EOF");
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::AuthFailed),
        "{err}"
    );
    assert!(
        dropped.load(Ordering::SeqCst),
        "致命后必须立即 drop 底层 TcpStream（只 shutdown 写半边不算关闭整条 TCP）"
    );

    // 对端可观察：读到 EOF，且继续写最终必然失败（RST/EPIPE）。
    let mut buf = [0u8; 32];
    let n = tokio::time::timeout(Duration::from_secs(5), peer.read(&mut buf))
        .await
        .expect("等待对端 EOF 超时")
        .expect("peer read");
    assert_eq!(n, 0, "本端释放底层后对端必须读到 EOF");

    let mut saw_write_error = false;
    for _ in 0..100 {
        if peer.write_all(b"still open?").await.is_err() {
            saw_write_error = true;
            break;
        }
        match tokio::time::timeout(Duration::from_millis(200), peer.read(&mut buf)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                saw_write_error = true;
                break;
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(saw_write_error, "本端已释放 socket，对端最终必须写失败");

    // 本端保持 sticky 同一错误，且不再持有任何可读端。
    let again = io
        .read(&mut probe)
        .await
        .expect_err("致命后读方向必须持续报错");
    assert_eq!(
        secure_io_failure(&again),
        Some(SecureIoFailure::AuthFailed),
        "{again}"
    );
}

// =====================================================================
// 15. R3：closing 收尾必须传播 Pending、Done 才释放底层
// =====================================================================

#[tokio::test]
async fn last_sequence_close_propagates_pending_shutdown() {
    let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let pair = client_record_layers_at_seq_for_test(keys, u64::MAX - 1, 0);
    let (mock, shared) = MockIo::new(0);
    // write/flush/shutdown各入口都会推进close，四次受阻分别属于四次真实poll。
    shared.stall_shutdown(4);
    let mut io = SecureIo::new(mock, pair);

    assert_eq!(ready_written(poll_write_once(&mut io, b"last record")), 11);
    // 末有效序号已 commit：本次不接受任何明文，并真实等待close完成。
    assert_pending("收尾中的新write", poll_write_once(&mut io, b"more"));
    assert_eq!(io.tx_buffered_len(), 0);
    // 收尾未完成 ⇒ 不得假报成功，也不得提前释放底层。
    assert_pending("收尾中的 flush", poll_flush_once(&mut io));
    assert_pending("收尾中的 shutdown", poll_shutdown_once(&mut io));
    assert!(
        !shared.dropped(),
        "shutdown 还没成功，收尾未完成，不得释放底层"
    );

    let mut done = false;
    for _ in 0..10 {
        if let Poll::Ready(Ok(())) = poll_flush_once(&mut io) {
            done = true;
            break;
        }
    }
    assert!(done, "底层 shutdown 的 Pending 预算耗尽后 flush 必须完成");
    assert!(shared.shutdowns() >= 1, "收尾必须真正下传 shutdown");
    assert!(shared.dropped(), "收尾完成（Done）后必须释放底层整条连接");
}

#[tokio::test]
async fn close_completes_from_read_side_without_another_write() {
    // 不能假设"write 返回 Ready 之后一定还会被 poll 写方向"：owner 完全可能
    // 写完最后一句就只读。收尾状态机必须在 `poll_read` 里同样被驱动。
    let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
    let pair = client_record_layers_at_seq_for_test(keys, u64::MAX - 1, 0);
    let (mock, shared) = MockIo::new(0);
    shared.stall_shutdown(2);
    let mut io = SecureIo::new(mock, pair);

    assert_eq!(ready_written(poll_write_once(&mut io, b"tail")), 4);
    assert!(!shared.dropped(), "收尾仍在 Pending");

    // 此后再没有任何 write/flush/shutdown：只 poll 读也必须把收尾推完。
    for _ in 0..10 {
        let _ = poll_read_once(&mut io, 64);
        if shared.dropped() {
            break;
        }
    }
    assert!(shared.dropped(), "只读侧也必须能推进 closing 收尾");
    assert!(shared.shutdowns() >= 1);
}

// =====================================================================
// 16. R4：每条记录独立 30s deadline（跨记录必须重锚）
// =====================================================================

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn read_deadline_reanchors_per_record_even_within_one_chunk() {
    // 审查反例 R4：record0 在 t=29s 凑齐，同一个 read chunk 里紧跟着 record1
    // 的首字节 ⇒ record1 的 30s 必须从 t=29s **重新锚定**（到 t≈59s），
    // 不得继续沿用 record0 的 t=30s 把新记录提前杀掉了。
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();
    let r0 = peer.tx.seal(b"record-zero").expect("seal r0");
    let r1 = peer.tx.seal(b"record-one").expect("seal r1");

    // t=0：record0 首字节到达 ⇒ 锚定 t=30s。
    shared.feed(&r0[..1]);
    assert_pending("record0 未收完时的读", poll_read_once(&mut io, 4096));

    // t=29s：一个 chunk 同时带来 record0 的结尾与 record1 的开头。
    tokio::time::advance(Duration::from_secs(29)).await;
    let mut chunk = r0[1..].to_vec();
    chunk.extend_from_slice(&r1[..6]);
    shared.feed(&chunk);
    assert_eq!(
        ready_bytes(poll_read_once(&mut io, 4096)),
        b"record-zero".to_vec()
    );

    // t=31s：沿用旧 deadline 的话这里已经致命；正确行为是仍然 Pending。
    tokio::time::advance(Duration::from_secs(2)).await;
    match poll_read_once(&mut io, 4096) {
        Poll::Pending => {}
        other => panic!("record1 的期限必须从自己的首字节（t=29s）重新锚定，实得 {other:?}"),
    }

    // t≈59s：record1 自己也没收完 ⇒ 到它自己的 30s 才致命。
    let step = Duration::from_millis(500);
    let mut elapsed = Duration::from_secs(31);
    let mut fired_at: Option<Duration> = None;
    while fired_at.is_none() && elapsed < Duration::from_secs(75) {
        tokio::time::advance(step).await;
        elapsed += step;
        if let Poll::Ready(Err(e)) = poll_read_once(&mut io, 4096) {
            assert_eq!(
                secure_io_failure(&e),
                Some(SecureIoFailure::ReadDeadline),
                "{e}"
            );
            fired_at = Some(elapsed);
        }
    }
    let fired_at = fired_at.expect("record1 最终也要在自己的 30s 期限到点致命");
    assert!(
        fired_at >= Duration::from_secs(59) && fired_at <= Duration::from_secs(60),
        "record1 应在 t≈59s 到点（29s + 30s），实测 {fired_at:?}"
    );
}

// =====================================================================
// 17. 真实 h2 互操作（buffered 写方向不得让 h2 卡住或错位）
// =====================================================================

/// 服务端单条流的 echo：读完请求体（并归还 h2 流控容量）后原样回写。
async fn echo_stream(
    req: http::Request<h2::RecvStream>,
    mut send_response: h2::server::SendResponse<bytes::Bytes>,
) {
    let (_parts, mut body) = req.into_parts();
    let mut buf = Vec::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk.expect("request body chunk");
        buf.extend_from_slice(&chunk);
        // 不归还接收容量，h2 自己的流控窗口会把后续分片卡死。
        let _ = body.flow_control().release_capacity(chunk.len());
    }
    let resp = http::Response::builder()
        .status(200)
        .body(())
        .expect("resp");
    let mut send_stream = send_response
        .send_response(resp, false)
        .expect("send response");
    send_stream
        .send_data(bytes::Bytes::from(buf), true)
        .expect("send body");
}

/// 一轮完整的真实 h2 echo（不是自造帧）：3 条流，请求体 1B / 20001B / 40001B
/// （⇒ 跨多条记录），底层是 `duplex(buf)`；返回 `(服务端完成流数, 客户端完成请求数)`。
///
/// `buf` 越小，一条记录越写不完（必须停在记录中间等对端腾空），写方向的
/// buffered 语义、`poll_flush` 的"已接受字节必须最终上线"、以及末记录收尾
/// 时 flush → shutdown 的串联都会被压到。
async fn h2_echo_roundtrip(buf: usize) -> (usize, usize) {
    let (a_io, b_io) = tokio::io::duplex(buf);
    let client_io = SecureIo::new(a_io, client_pair());
    let mut server_io = SecureIo::new(b_io, server_pair());

    let server = tokio::spawn(async move {
        let mut h2 = h2::server::handshake(&mut server_io)
            .await
            .expect("h2 server handshake");
        // **`h2::server::Connection` 只在被 poll 时才推进**（它实现的是 `Stream`，
        // 不是 `Future`；`accept()` 内部驱动它）。因此本任务的 `accept()` 循环
        // 必须是这条连接唯一的驱动点，每条流的处理一律 spawn 出去：如果在同一
        // 任务里 inline `await body`，"等 body"的那段时间没人驱动连接，而底层
        // 的读 waker 已在上一次 `Pending` 时被底层取走 ⇒ 分片到达的记录永远
        // 读不上来。这一点与 `SecureIo` 无关：把这里换成裸 `duplex`（不套
        // `SecureIo`）用同样的 inline 写法也复现同样的卡死。
        let mut streams = Vec::new();
        loop {
            match h2.accept().await {
                Some(Ok((req, send_response))) => {
                    streams.push(tokio::spawn(echo_stream(req, send_response)));
                }
                Some(Err(e)) => panic!("accept 出错：{e}"),
                None => break, // 对端 GOAWAY + 干净 EOF ⇒ 连接已结束
            }
        }
        // 收尾自检：连接必须已"自己走到关闭"（GOAWAY 与最后一条响应都 flush 上线）。
        // 直接 drop 底层 `SecureIo` 会把最后一条响应截断在记录缓冲里，所以这里
        // 必须显式确认，而不是依赖 drop 的副作用。
        let closed = tokio::time::timeout(
            Duration::from_millis(500),
            std::future::poll_fn(|cx| h2.poll_closed(cx)),
        )
        .await;
        assert!(
            matches!(closed, Ok(Ok(()))),
            "server 侧 h2 连接必须能自行驱动到关闭（GOAWAY + flush 全部上线），实得 {closed:?}"
        );
        let mut served = 0usize;
        for stream in streams {
            stream.await.expect("echo task 不得 panic 或被取消");
            served += 1;
        }
        served
    });

    let client = tokio::spawn(async move {
        // split+join 让 h2 的 Connection future 拿到 'static 的所有权，
        // 顺带压一遍 `tokio::io::split` 这种上层常见包装与 SecureIo 的组合。
        let (r, w) = tokio::io::split(client_io);
        let joined = tokio::io::join(r, w);
        let (mut send_req, conn) = h2::client::handshake(joined)
            .await
            .expect("h2 client handshake");
        // 客户端侧同样把连接 future 交给独立任务持续驱动（`Connection` 是 Future）。
        let conn = tokio::spawn(conn);
        let mut done = 0usize;
        for i in 0..3usize {
            let payload = vec![0x5au8; 1 + i * 20_000];
            let req = http::Request::builder()
                .method("POST")
                .uri("http://example.invalid/echo")
                .body(())
                .expect("req");
            let (resp_fut, mut send_stream) =
                send_req.send_request(req, false).expect("send_request");
            send_stream
                .send_data(bytes::Bytes::from(payload.clone()), true)
                .expect("send request body");
            // await 得到 Result<Response<RecvStream>, h2::Error>：一次 unwrap 即可
            let mut resp = resp_fut.await.expect("响应");
            let body = resp.body_mut();
            let mut got = Vec::new();
            while let Some(chunk) = body.data().await {
                let chunk = chunk.expect("response chunk");
                got.extend_from_slice(&chunk);
                // 不释放接收容量会让 h2 自己的流控窗口把响应卡死。
                let _ = body.flow_control().release_capacity(chunk.len());
            }
            assert_eq!(got, payload, "第 {i} 次 echo 的响应体不等");
            done += 1;
        }
        // 优雅关闭：丢掉最后一个 `SendRequest` 句柄 ⇒ h2 自动发 GOAWAY 并把
        // 缓冲 flush 干净；随后把连接 future 驱动到结束（不能直接 drop，
        // 否则最后一句响应/GOAWAY 还躺在我们的记录缓冲里就被截断了）。
        drop(send_req);
        let _ = tokio::time::timeout(Duration::from_secs(2), conn).await;
        done
    });

    // 整体超时：h2 互操作一旦因写方向"接受 ≠ 上线"处理不当而卡死，必须是用例
    // 失败，而不是把整个测试挂在这里。
    let outcome = tokio::time::timeout(Duration::from_secs(30), async move {
        let (served, done) = tokio::join!(server, client);
        (served.expect("server task"), done.expect("client task"))
    })
    .await;
    match outcome {
        Ok(pair) => pair,
        Err(_) => panic!("真实 h2 互操作在 30s 内没有跑完（写方向卡死），duplex={buf}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_h2_client_and_server_interoperate_over_secure_io() {
    // 16KB：一条满记录（65540B 线上）远大于底层缓冲 ⇒ 每条记录都要经历"停在
    // 记录中间 ⇒ 重入续写"，40001B 的请求体还要跨多条记录。
    let (served, done) = h2_echo_roundtrip(16 * 1024).await;
    assert_eq!(served, 3, "服务端必须恰好服务 3 条流");
    assert_eq!(done, 3, "客户端必须恰好完成 3 次 echo");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_h2_interoperates_under_extreme_backpressure() {
    // 极端背压：底层缓冲只有 64B，比一条记录小三个数量级。任何"已 `Ready`
    // 接受的字节最终不会上线"或"某次 `Pending` 未接受的写入被当成已接受"都会
    // 在这里先表现成卡死或响应体错位。
    let (served, done) = h2_echo_roundtrip(64).await;
    assert_eq!(served, 3, "极端背压下服务端必须恰好服务 3 条流");
    assert_eq!(done, 3, "极端背压下客户端必须恰好完成 3 次 echo");
}

// =====================================================================
// 18. 类型/接口约束：SecureIo 自身就是 Unpin 的 AsyncRead+AsyncWrite
// =====================================================================

#[allow(dead_code)]
fn secure_io_is_async_read_write_and_unpin() {
    fn assert_io<T: AsyncRead + AsyncWrite + Unpin>() {}
    assert_io::<SecureIo<tokio::io::DuplexStream>>();
    assert_io::<SecureIo<MockIo>>();
    // 方向类型仍只暴露各自那一半能力（Tx 只有 seal）。
    let _: fn(&mut TxRecordLayer, &[u8]) -> Result<Vec<u8>, rsetup_protocol::crypto::CryptoError> =
        TxRecordLayer::seal;
}

// Fix round2: entry-state transitions must be checked after driving old work.
#[tokio::test]
async fn final_record_commit_never_accepts_a_new_buffer() {
    let (mock, shared) = MockIo::with_stall(8, 2);
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let mut io = SecureIo::new(mock, pair);
    assert_eq!(ready_written(poll_write_once(&mut io, b"LAST")), 4);
    let mut refused = false;
    for _ in 0..10 {
        match poll_write_once(&mut io, b"NEW") {
            Poll::Pending => {}
            Poll::Ready(Err(err)) => {
                assert_eq!(
                    secure_io_failure(&err),
                    Some(SecureIoFailure::SequenceExhausted)
                );
                refused = true;
                break;
            }
            Poll::Ready(Ok(n)) => {
                panic!("final sequence already committed but NEW was accepted: {n}")
            }
        }
    }
    assert!(refused);
    assert_eq!(io.tx_buffered_len(), 0);
    assert_eq!(shared.logged().len(), 24);
    assert!(shared.dropped());
}
#[tokio::test]
async fn writes_drive_pending_final_shutdown_before_refusing_new_input() {
    let (mock, shared) = MockIo::new(0);
    shared.stall_shutdown(100);
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let mut io = SecureIo::new(mock, pair);
    assert_eq!(ready_written(poll_write_once(&mut io, b"LAST")), 4);
    assert!(
        poll_write_once(&mut io, b"NEW").is_pending(),
        "closing must be driven, not early-exhausted"
    );
    shared.stall_shutdown(0);
    let err = match poll_write_once(&mut io, b"NEW") {
        Poll::Ready(Err(e)) => e,
        other => panic!("expected refusal after completed close: {other:?}"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::SequenceExhausted)
    );
    assert!(shared.dropped());
    assert_eq!(io.tx_buffered_len(), 0);
}
#[tokio::test]
async fn read_cannot_report_clean_eof_while_final_shutdown_is_pending() {
    let (mock, shared) = MockIo::new(0);
    shared.stall_shutdown(100);
    shared.set_eof();
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let mut io = SecureIo::new(mock, pair);
    assert_eq!(ready_written(poll_write_once(&mut io, b"LAST")), 4);
    assert!(
        poll_read_once(&mut io, 1).is_pending(),
        "EOF must not conceal pending close"
    );
    shared.stall_shutdown(0);
    assert!(poll_read_once(&mut io, 1).is_ready());
    assert!(shared.dropped());
}
#[tokio::test]
async fn read_entry_enforces_the_existing_write_deadline() {
    tokio::time::pause();
    let (mock, shared) = MockIo::new(0);
    shared.stall_writes(true);
    let mut io = SecureIo::new(mock, client_pair());
    assert_eq!(ready_written(poll_write_once(&mut io, b"ACCEPTED")), 8);
    assert!(poll_read_once(&mut io, 1).is_pending());
    tokio::time::advance(Duration::from_secs(31)).await;
    let err = match poll_read_once(&mut io, 1) {
        Poll::Ready(Err(e)) => e,
        other => panic!("read did not enforce write deadline: {other:?}"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::WriteDeadline)
    );
    assert!(shared.dropped());
}
#[tokio::test]
async fn write_deadline_wakes_reader_when_only_reader_remains_parked() {
    tokio::time::pause();
    let (mock, shared) = MockIo::new(0);
    shared.stall_writes(true);
    let mut io = SecureIo::new(mock, client_pair());
    assert_eq!(ready_written(poll_write_once(&mut io, b"ACCEPTED")), 8);
    let task = tokio::spawn(async move {
        let mut b = [0];
        io.read(&mut b).await
    });
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(31)).await;
    let err = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("reader timer waker was not registered")
        .unwrap()
        .unwrap_err();
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::WriteDeadline)
    );
    assert!(shared.dropped());
}

#[tokio::test]
async fn real_duplex_final_sequence_backpressure_refuses_new_bytes() {
    let (a, mut peer) = tokio::io::duplex(8);
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let mut io = SecureIo::new(a, pair);
    assert_eq!(ready_written(poll_write_once(&mut io, b"LAST")), 4);
    assert!(poll_write_once(&mut io, b"NEW").is_pending());
    let mut wire = Vec::new();
    let mut refused = false;
    for _ in 0..3 {
        let mut scratch = [0; 8];
        let n = peer.read(&mut scratch).await.unwrap();
        wire.extend_from_slice(&scratch[..n]);
        match poll_write_once(&mut io, b"NEW") {
            Poll::Pending => {}
            Poll::Ready(Err(e)) => {
                assert_eq!(
                    secure_io_failure(&e),
                    Some(SecureIoFailure::SequenceExhausted)
                );
                refused = true;
                break;
            }
            Poll::Ready(Ok(n)) => panic!("accepted {n} NEW bytes after final record"),
        }
    }
    assert!(refused);
    io.flush().await.unwrap();
    assert_eq!(io.tx_buffered_len(), 0);
    tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut wire))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(wire.len(), 24);
    let mut rx = server_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        0,
        u64::MAX - 1,
    );
    assert_eq!(rx.rx.open(&wire).unwrap(), b"LAST");
}
#[tokio::test]
async fn final_shutdown_wakeup_finishes_a_parked_reader_before_eof() {
    let (mock, shared) = MockIo::new(0);
    shared.stall_shutdown(100);
    shared.set_eof();
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let mut io = SecureIo::new(mock, pair);
    assert_eq!(ready_written(poll_write_once(&mut io, b"LAST")), 4);
    let task = tokio::spawn(async move {
        let mut b = [0];
        io.read(&mut b).await
    });
    tokio::task::yield_now().await;
    assert!(!task.is_finished());
    assert!(!shared.dropped());
    shared.stall_shutdown(0);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        0
    );
    assert!(shared.dropped());
}

// =====================================================================
// 19. Fix round3：计时 waker 的**所有权**与"关闭前统一核验期限"
// =====================================================================
//
// 一个 `tokio::time::Sleep` 只保留**最后一次 poll 它的那个 waker**。读写两个方向
// 常常来自不同任务（`tokio::io::split` + 各自 `spawn` 是常态），因此"谁最后 poll
// 谁就当计时的 owner"必然漏：
//
// * 读方向顺手推进写方向计时 ⇒ parked writer 的 waker 被读任务的 waker 覆盖；
//   读任务随后 `Ready` 退出 ⇒ 到点只唤醒一个已经结束的任务；
// * 固定挑读侧或写侧当 owner 也不对：那一方被取消或换任务会再次漏。
//
// 下面的用例全部用**真实任务 park / 真实唤醒**（除必要的对端喂字节外不做手工
// poll），断言"到点必须把还活着的那一侧叫醒"，并保留取消读方向、换读任务与
// writer-only / reader-only 的对照（后两者见 §6/§15 的同名用例）。

/// 主反例：真实 `duplex(8)` + `tokio::io::split`，写任务先 park，读任务读到合法
/// reply 后 `Ready` 结束，再推进时钟 ⇒ 写任务必须被唤醒并 `WriteDeadline`，且
/// 整条底层传输随之释放（对端 drain 掉已上线的 8 字节后读到 EOF）。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn parked_writer_wakes_after_the_read_task_has_left() {
    let (a_io, peer) = tokio::io::duplex(8);
    let io = SecureIo::new(a_io, client_pair());
    let (mut r, mut w) = tokio::io::split(io);

    // t=0：整段被"接受"（Ready），但 4+8+16=28B 的线上记录只有 8B 进得了 duplex
    // ⇒ 该记录仍在途（写期限此刻锚定），此后任何新写入只可能 Pending。
    assert_eq!(
        w.write(b"ACCEPTED").await.expect("首次写入不得报错"),
        8,
        "8 字节的入参必须被整段接受"
    );

    // 真实写任务：NEW 只能停在 Pending ⇒ park（此刻 Sleep 的 owner 是它）。
    let writer = tokio::spawn(async move { w.write_all(b"NEW-3").await });
    tokio::task::yield_now().await;
    assert!(
        !writer.is_finished(),
        "在途记录未推完时新写入必须 park（Ready 或报错都不对）"
    );

    // 真实读任务：稍后读到对端一条合法记录 ⇒ Ready 并结束（读方向"换人/退场"）。
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; 64];
        let n = r.read(&mut buf).await.expect("读 reply 不得报错");
        buf[..n].to_vec()
    });
    tokio::task::yield_now().await;

    // 对端往**另一个方向**喂一条合法记录（duplex 两个方向独立：a→peer 始终是满的，
    // 所以那条在途记录不可能被推完，写任务也不可能被"顺带"唤醒）。
    let mut peer_layer = server_pair();
    let reply = peer_layer.tx.seal(b"reply").expect("seal reply");
    let (mut peer_r, mut peer_w) = tokio::io::split(peer);
    let injected = tokio::spawn(async move {
        peer_w.write_all(&reply).await.expect("对端写入 reply");
    });
    let got = tokio::time::timeout(Duration::from_millis(200), reader)
        .await
        .expect("读任务应在 200ms 内读到合法 reply")
        .expect("读任务不得 panic");
    assert_eq!(got, b"reply".to_vec(), "读方向必须正常交付认证通过的明文");
    tokio::time::timeout(Duration::from_millis(200), injected)
        .await
        .expect("对端写入应完成")
        .expect("对端任务不得 panic");

    // 此刻那条在途记录的 `Sleep` 若记的是"最后一次 poll 它的任务"，owner 已经是
    // **刚刚结束的读任务**。先到 29.999s：还没到点，写任务必须仍 park。
    tokio::time::advance(Duration::from_millis(29_999)).await;
    tokio::task::yield_now().await;
    assert!(
        !writer.is_finished(),
        "30s 期限未到点时写任务必须继续 park（不得提前结束）"
    );

    // 再 +2ms ⇒ 过点。**不做任何额外 poll**，只让运行时把被唤醒的任务跑一轮：
    // 写任务必须自己醒过来并以 WriteDeadline 结束。
    tokio::time::advance(Duration::from_millis(2)).await;
    tokio::task::yield_now().await;
    let err = match tokio::time::timeout(Duration::from_millis(200), writer).await {
        Ok(res) => res
            .expect("写任务不得 panic")
            .expect_err("写期限到点是错误，不是完成"),
        Err(_) => panic!(
            "写期限到点没有唤醒 park 中的写任务：Sleep 的 waker 被已结束的读任务覆盖了 \
             （必须用双向广播 waker 登记计时）"
        ),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::WriteDeadline),
        "{err}"
    );

    // 整条传输释放：对端 drain 掉此前已上线的 8 字节后必须读到 EOF（不是只关写半边）。
    let mut drained = Vec::new();
    tokio::time::timeout(Duration::from_millis(200), peer_r.read_to_end(&mut drained))
        .await
        .expect("对端必须在有限时间内读到 EOF（底层传输已整体释放）")
        .expect("peer read_to_end");
    assert_eq!(
        drained.len(),
        8,
        "致命时线上只可能留下此前已写出并被底层接纳的 8 字节，不得有 NEW"
    );
}

/// 对照一（真实取消）：读方向被**真实取消后彻底退场**，此后没有任何人再 poll 读
/// 方向 ⇒ 只剩写方向一个任务，计时到点它必须**自己**被唤醒。
///
/// 这一条与上一条是同一根因的两个侧面：上一条是"owner 被活着的读任务覆盖"，本条
/// 是"owner 属于一个已经不存在的读任务"。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn cancelling_the_read_task_does_not_lose_the_write_deadline() {
    let (a_io, mut peer) = tokio::io::duplex(8);
    let io = SecureIo::new(a_io, client_pair());
    let (mut r, mut w) = tokio::io::split(io);
    assert_eq!(w.write(b"ACCEPTED").await.expect("首次写入"), 8);

    let writer = tokio::spawn(async move { w.write_all(b"NEW-3").await });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished(), "在途记录未推完时 NEW 必须 park");

    // 读方向被**取消**：timeout 到点即 drop 掉处于 Pending 的读 future（真实的读
    // 取消路径），随后读半边退场 ⇒ 再没有第二个任务能替写方向"顺带"发现致命。
    let mut buf = vec![0u8; 16];
    assert!(
        tokio::time::timeout(Duration::from_millis(50), r.read(&mut buf))
            .await
            .is_err(),
        "没有数据时读必须停在 Pending（本用例的前提）"
    );
    drop(r);

    tokio::time::advance(Duration::from_millis(30_500)).await;
    tokio::task::yield_now().await;
    let err = match tokio::time::timeout(Duration::from_millis(200), writer).await {
        Ok(res) => res
            .expect("写任务不得 panic")
            .expect_err("写期限到点是错误，不是完成"),
        Err(_) => panic!("读方向被取消后，写期限到点没有唤醒仅剩的写任务"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::WriteDeadline),
        "{err}"
    );
    let mut drained = Vec::new();
    tokio::time::timeout(Duration::from_millis(200), peer.read_to_end(&mut drained))
        .await
        .expect("对端必须读到 EOF（整条底层已释放）")
        .expect("peer read_to_end");
    assert_eq!(drained.len(), 8, "线上只可能留有已上线的那 8 字节");
}

/// 对照二（换任务）：读方向由**另一个任务**接手（owner 又换了一次），写方向的计时
/// 唤醒同样不得丢。与 writer-only（§6 `write_deadline_wakes_a_parked_writer_task`）、
/// reader-only（§15 `write_deadline_wakes_reader_when_only_reader_remains_parked`）
/// 一起构成"任一侧取消/换人都不丢另一向"的对照面。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn replacing_the_read_task_keeps_the_write_deadline() {
    let (a_io, mut peer) = tokio::io::duplex(8);
    let io = SecureIo::new(a_io, client_pair());
    let (mut r, mut w) = tokio::io::split(io);
    assert_eq!(w.write(b"ACCEPTED").await.expect("首次写入"), 8);

    let writer = tokio::spawn(async move { w.write_all(b"NEW-3").await });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished(), "NEW 必须 park");

    // 第一个读任务：没有数据 ⇒ Pending，随后被取消。
    let mut buf = vec![0u8; 16];
    assert!(
        tokio::time::timeout(Duration::from_millis(50), r.read(&mut buf))
            .await
            .is_err(),
        "没有数据时读必须停在 Pending（本用例的前提）"
    );

    // 换**另一个任务**接手同一条读半边。
    let reader2 = tokio::spawn(async move {
        let mut b = vec![0u8; 16];
        r.read(&mut b).await
    });
    tokio::task::yield_now().await;
    assert!(!reader2.is_finished(), "无数据时新读任务也应 park");

    // 过点：写任务必须被唤醒（读方向换任务不得弄丢另一向的计时）。
    tokio::time::advance(Duration::from_millis(30_500)).await;
    tokio::task::yield_now().await;
    let err = match tokio::time::timeout(Duration::from_millis(200), writer).await {
        Ok(res) => res.expect("写任务不得 panic").expect_err("到点应是错误"),
        Err(_) => panic!("读方向换任务后，写期限到点没有唤醒写任务"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::WriteDeadline),
        "{err}"
    );
    // 另一向也得到同一个 fatal（双向终止，不留下"还在等数据"的读任务）。
    let read_err = tokio::time::timeout(Duration::from_millis(200), reader2)
        .await
        .expect("读任务也必须被同一次广播唤醒")
        .expect("读任务不得 panic")
        .expect_err("致命后读方向必须报错");
    assert_eq!(
        secure_io_failure(&read_err),
        Some(SecureIoFailure::WriteDeadline),
        "{read_err}"
    );
    let mut drained = Vec::new();
    tokio::time::timeout(Duration::from_millis(200), peer.read_to_end(&mut drained))
        .await
        .expect("对端必须读到 EOF")
        .expect("peer read_to_end");
    assert_eq!(drained.len(), 8, "线上只可能留有已上线的那 8 字节");
}

/// 状态-期限优先级：**已存在的 RX 首字节期限不得被 closing 的 Pending 遮蔽**。
///
/// `stall_flush` 选择收尾停在哪一段（`true` = 底层 flush 一直 Pending，停在
/// Flushing；`false` = shutdown 一直 Pending，停在 ShuttingDown）。两段的结论必须
/// 相同：末有效序号后"收尾推不动"既不能让读假报 EOF，也不能把已经到点的部分记录
/// 期限遮成 Pending。
async fn rx_deadline_vs_pending_close(stall_flush: bool) {
    let phase = if stall_flush {
        "Flushing"
    } else {
        "ShuttingDown"
    };
    let (mock, shared) = MockIo::new(0);
    if stall_flush {
        shared.stall_flush(1000);
    } else {
        shared.stall_shutdown(1000);
    }
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let mut io = SecureIo::new(mock, pair);

    // t=0：RX 首字节先到（此刻尚未 closing）⇒ 该条记录 30s 期限锚定在 t=0。
    let mut peer = server_pair();
    let rec = peer.tx.seal(b"never completes").expect("seal partial");
    shared.feed(&rec[..1]);
    assert_pending("RX 首字节后记录未收完", poll_read_once(&mut io, 4096));

    // t=0：末有效序号记录上线并 commit ⇒ 进入 closing，收尾动作持续 Pending。
    assert_eq!(ready_written(poll_write_once(&mut io, b"LAST")), 4);
    assert!(!shared.dropped(), "收尾未完成时不得释放底层（{phase}）");

    // t=29.999s：期限未到 ⇒ 只能是 Pending（既不得假报 EOF，也不得提前致命）。
    tokio::time::advance(Duration::from_millis(29_999)).await;
    assert_pending(
        "closing Pending 期间的正容量读",
        poll_read_once(&mut io, 4096),
    );

    // 零容量读：依约**不驱动**（不得推进收尾状态机），也未到点 ⇒ Ready(Ok(()))。
    let flushes = shared.flushes();
    let shutdowns = shared.shutdowns();
    assert!(
        matches!(poll_read_once(&mut io, 0), Poll::Ready(Ok(bytes)) if bytes.is_empty()),
        "零容量读应返回 Ready(Ok(0 字节))"
    );
    assert_eq!(shared.flushes(), flushes, "零容量读不得推进底层 flush");
    assert_eq!(
        shared.shutdowns(),
        shutdowns,
        "零容量读不得推进底层 shutdown"
    );

    // t=30.001s：RX 期限到点 ⇒ **立即** ReadDeadline 并释放底层，而不是因为
    // "收尾还 Pending"继续挂 30s。这里**第一次**观察故意用零容量读：依约它不消费
    // 底层、不推进收尾，但"期限已过"属于已经发生的连接级事实，必须如实报出，
    // 不得回 `Ready(Ok(0 字节))` 让调用方误判成"只是还没数据"。
    tokio::time::advance(Duration::from_millis(2)).await;
    let err = match poll_read_once(&mut io, 0) {
        Poll::Ready(Err(e)) => e,
        other => panic!(
            "RX 首字节期限被 closing（{phase}）的 Pending 遮蔽了：{other:?}；\
             已到期的连接期限必须在推进收尾之前核验，且零容量读也必须看到它"
        ),
    };
    assert_eq!(secure_io_failure(&err), Some(SecureIoFailure::ReadDeadline));
    assert!(
        shared.dropped(),
        "RX 期限致命必须立即释放底层传输（{phase}）"
    );
    // 正容量读同样是这条期限错误（不是 EOF、也不是收尾放行后的成功）。
    match poll_read_once(&mut io, 4096) {
        Poll::Ready(Err(e)) => assert_eq!(
            secure_io_failure(&e),
            Some(SecureIoFailure::ReadDeadline),
            "正容量读必须给出同一个 sticky 期限错误"
        ),
        other => panic!("致命后读应持续报错：{other:?}"),
    }

    // 期限已经致命：此后放行底层收尾也改不回成功，两个方向同一 sticky 错误。
    if stall_flush {
        shared.stall_flush(0);
    } else {
        shared.stall_shutdown(0);
    }
    // fatal 优先于"零容量读直接返回 Ok"这条约定（同一 sticky 错误，不重复报成功）。
    match poll_read_once(&mut io, 0) {
        Poll::Ready(Err(e)) => assert_eq!(
            secure_io_failure(&e),
            Some(SecureIoFailure::ReadDeadline),
            "零容量读必须给出同一个 sticky 期限错误"
        ),
        other => panic!("致命后零容量读应报同一错误：{other:?}"),
    }
    match poll_write_once(&mut io, b"x") {
        Poll::Ready(Err(e)) => assert_eq!(
            secure_io_failure(&e),
            Some(SecureIoFailure::ReadDeadline),
            "写方向同样继承同一条致命"
        ),
        other => panic!("致命后写方向必须报错：{other:?}"),
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn rx_first_byte_deadline_is_not_shadowed_by_pending_flush() {
    rx_deadline_vs_pending_close(true).await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn rx_first_byte_deadline_is_not_shadowed_by_pending_shutdown() {
    rx_deadline_vs_pending_close(false).await;
}

/// 同一优先级的另一半：**写/flush/shutdown 入口推进 IO 之前**也必须观察到已经
/// 到期的连接期限（这里是 RX 首字节期限），不得因"收尾推不动"永远 Pending。
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn write_entries_observe_an_expired_rx_deadline() {
    let (mock, shared) = MockIo::new(0);
    shared.stall_shutdown(1000);
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let mut io = SecureIo::new(mock, pair);

    let mut peer = server_pair();
    let rec = peer.tx.seal(b"partial rx").expect("seal partial");
    shared.feed(&rec[..1]);
    assert_pending("RX 首字节后未收完", poll_read_once(&mut io, 4096));
    assert_eq!(ready_written(poll_write_once(&mut io, b"LAST")), 4);

    tokio::time::advance(Duration::from_millis(30_001)).await;
    let err = match poll_write_once(&mut io, b"NEW") {
        Poll::Ready(Err(e)) => e,
        other => panic!("写入口没有核验已到期的 RX 期限：{other:?}"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::ReadDeadline),
        "{err}"
    );
    assert!(shared.dropped(), "连接级期限致命必须立即释放底层");
    // flush/shutdown 同样继承这条 sticky 错误（不得假报收尾完成）。
    for what in ["flush", "shutdown"] {
        let err = if what == "flush" {
            match poll_flush_once(&mut io) {
                Poll::Ready(Err(e)) => e,
                other => panic!("致命后 flush 必须报错：{other:?}"),
            }
        } else {
            match poll_shutdown_once(&mut io) {
                Poll::Ready(Err(e)) => e,
                other => panic!("致命后 shutdown 必须报错：{other:?}"),
            }
        };
        assert_eq!(
            secure_io_failure(&err),
            Some(SecureIoFailure::ReadDeadline),
            "{what} 必须报同一条期限致命"
        );
    }
}

/// 收尾由**读方向**完成时，park 在底层 `poll_shutdown` 上的写方向必须被同一个
/// 中心叫醒：收尾 `Done` 之后底层对象已被释放，写方向若还 park 在"已经消失的底层
/// waker"上，就没有任何东西能再唤醒它。
///
/// 调度是精确构造的：`release_shutdown` 只放行、不唤醒，`wake_last` 只叫醒最后登记
/// 的那个 waker（= 读方向），因此"写方向醒来"只能来自实现的广播，而不是底层的顺带
/// 唤醒，也不依赖两个任务的先后竞争。
#[tokio::test(flavor = "current_thread")]
async fn close_completed_by_read_side_wakes_a_parked_writer() {
    let (mock, shared) = MockIo::new(0);
    // 三次 Pending：末记录 write 一次、park 中的写任务一次、park 中的读任务一次。
    shared.stall_shutdown(3);
    let pair = client_record_layers_at_seq_for_test(
        SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).unwrap(),
        u64::MAX - 1,
        0,
    );
    let io = SecureIo::new(mock, pair);
    let (mut r, mut w) = tokio::io::split(io);

    // 末有效序号上线 ⇒ 进入 closing，收尾停在 ShuttingDown 的 Pending 上。
    assert_eq!(
        w.write(b"LAST").await.expect("末有效序号仍可被接受"),
        4,
        "收尾停在 Pending 不影响「已接受」这个结论"
    );
    assert!(!shared.dropped(), "收尾未完成时不得释放底层");

    // 写方向：再写只能停在 closing 的 Pending 上（登记底层 shutdown 的 waker）。
    let writer = tokio::spawn(async move { w.write_all(b"NEW").await });
    tokio::task::yield_now().await;
    assert!(!writer.is_finished(), "closing Pending 期间写方向必须 park");

    // 读方向：同样 Pending，登记在底层 shutdown 上（它随后负责把收尾推到底）。
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; 16];
        match r.read(&mut buf).await.expect("读方向不得报错") {
            0 => 1usize,
            _ => 0usize,
        }
    });
    tokio::task::yield_now().await;
    assert!(!reader.is_finished(), "收尾未完成时读方向也必须 park");
    assert!(
        !shared.dropped(),
        "三次 shutdown 都还 Pending，不得释放底层"
    );

    // 放行收尾，但**只叫醒读方向** ⇒ 收尾由读侧驱动到 Done 并释放底层。
    shared.release_shutdown();
    shared.wake_last();
    tokio::task::yield_now().await;
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), reader)
            .await
            .expect("读方向应把收尾推到完成并读到 EOF")
            .expect("读任务不得 panic"),
        1,
        "读方向必须看到 EOF（收尾完成 ⇒ 释放底层）"
    );
    assert!(shared.dropped(), "收尾完成（Done）后必须释放底层");

    // 关键断言：写方向不得被搁死在已释放的底层上。
    let err = match tokio::time::timeout(Duration::from_secs(1), writer).await {
        Ok(res) => res
            .expect("写任务不得 panic")
            .expect_err("末有效序号之后写方向必须报错"),
        Err(_) => panic!("收尾由读侧完成后，写方向必须被唤醒（底层已释放，没人会再叫它）"),
    };
    assert_eq!(
        secure_io_failure(&err),
        Some(SecureIoFailure::SequenceExhausted),
        "{err}"
    );
}

/// 保留项（本轮重排了 `poll_read` 内部的优先级，这条路径必须不变）：
/// **已认证但未交付完的明文**跨多次 poll 必须按原顺序继续交付，先交 `A` 再交 `BC`，
/// 不得因为"这次容量小"就把残余丢掉、重排，或在下一次 poll 先去读底层而误报 EOF。
#[tokio::test]
async fn authenticated_residual_plaintext_is_delivered_in_order_across_polls() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();
    shared.feed(&peer.tx.seal(b"ABC").expect("seal ABC"));

    // 第一次只给 1 字节容量 ⇒ 恰好交付残余的第一段。
    assert_eq!(ready_bytes(poll_read_once(&mut io, 1)), b"A".to_vec());
    assert_eq!(io.rx_seq(), Some(1), "整条ABC应已经认证且只消费一条记录");
    assert!(
        shared.readable.lock().expect("readable lock").is_empty(),
        "第二次BC必须来自已认证残余，不能再从底层读一条记录"
    );
    // 第二次必须把**剩余已认证明文**先交付（而不是返回 EOF 或再等底层）。
    assert_eq!(ready_bytes(poll_read_once(&mut io, 16)), b"BC".to_vec());
    assert!(!io.is_fatal(), "正常交付不得 fatal");

    // 明文交付干净之后，底层干净 EOF 才允许报 0 字节。
    shared.set_eof();
    assert_eq!(
        ready_bytes(poll_read_once(&mut io, 16)).len(),
        0,
        "残余交付完后才允许 EOF"
    );
}

/// 保留项：**owner 显式 `poll_shutdown` 只关闭写方向**——底层不得被释放，读方向
/// 之后仍要能交付已认证明文；写方向则一律 [`SecureIoFailure::Closed`]。
#[tokio::test]
async fn explicit_shutdown_keeps_the_read_half_and_refuses_new_writes() {
    let (mock, shared) = MockIo::new(0);
    let mut io = SecureIo::new(mock, server_pair());
    let mut peer = client_pair();
    shared.feed(&peer.tx.seal(b"still readable").expect("seal first"));
    assert_eq!(
        ready_bytes(poll_read_once(&mut io, 64)),
        b"still readable".to_vec()
    );

    assert!(matches!(poll_shutdown_once(&mut io), Poll::Ready(Ok(()))));
    assert_eq!(shared.shutdowns(), 1, "显式 shutdown 必须下传底层");
    assert!(
        !shared.dropped(),
        "显式 shutdown 只关写半边：底层必须保留给读方向（不是末有效序号的整连接关闭）"
    );

    // 读半边仍然可用：之后到达的记录照常认证交付。
    shared.feed(&peer.tx.seal(b"after shutdown").expect("seal second"));
    assert_eq!(
        ready_bytes(poll_read_once(&mut io, 64)),
        b"after shutdown".to_vec(),
        "显式 shutdown 之后读方向必须仍能交付已认证明文"
    );

    // 写半边已关闭：不接受任何明文。
    match poll_write_once(&mut io, b"x") {
        Poll::Ready(Err(e)) => {
            assert_eq!(secure_io_failure(&e), Some(SecureIoFailure::Closed), "{e}")
        }
        other => panic!("显式 shutdown 后写方向必须拒绝任何明文：{other:?}"),
    }
    assert!(!io.is_fatal(), "显式 shutdown 不是致命，不得拉闸");

    // 只有本对象被 drop 时底层才被释放。
    drop(io);
    assert!(shared.dropped(), "SecureIo drop 后底层必须释放");
}

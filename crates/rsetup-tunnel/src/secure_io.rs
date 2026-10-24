//! v3 加密记录异步 I/O（protocol_spec.md §5.2）。
//!
//! [`SecureIo`] 把 [`RecordLayerPair`] 架在任意 Tokio `AsyncRead + AsyncWrite`
//! 传输（TCP、Unix socket、duplex、TLS 流等）之上：对上层表现为普通的字节流
//! （h2 直接跑在上面，同一条 TCP 承载），对下层表现为**一条一条完整记录**的
//! 读写。它**不起后台任务**：所有加解密、分帧、计时都发生在 `poll_*` 里，
//! 因此没有跨任务共享状态、没有隐藏 spawn，也不会因为上层不再 poll 就悄悄
//! 推进序号。
//!
//! # 写方向是**有界 buffered** AsyncWrite（owner 必须驱动 flush/shutdown）
//!
//! `poll_write` 采用 Tokio `AsyncWrite` 允许、也是 `tokio::io::BufWriter` 等
//! 标准实现采用的那一套契约，并且缓冲区上界固定为**一条记录**（65520B 明文）：
//!
//! * `Ready(Ok(n))`：`n` 字节的**本次调用入参**已被本对象复制接管（进入内部
//!   有界明文缓冲，尽量随即封箱下发）。这只表示"已接受"，**不**表示已上线；
//!   要确认上线由 `poll_flush` 负责。因为入参已被复制，调用方在拿到
//!   `Ready(Ok(n))` 之后可以立即复用、改写甚至释放这块 buffer。
//! * `Pending`：本次调用**一个字节都没有接受**。本类型不会保存、记住或"稍后
//!   再确认"这份入参 buffer 的任何字节；被接受的只有此前某次 `Ready(Ok(n))`
//!   明确报出的那 `n` 字节。因此"上一次 `Pending` 的写入被后续另一次写入静默
//!   替代"在本实现里结构上不可能发生，`poll_write` 也**不要求**调用方在
//!   `Pending` 之后重传同一份 buffer（那是 `write_all` 这类 adapter 自己的
//!   循环职责）。
//! * 写入 future 被**取消**（drop）完全安全：`Ready(Ok(n))` 的字节已归本对象
//!   所有并一定会被写出（或由连接级致命丢弃）；`Pending` 的调用没有留下任何
//!   状态，取消它不会让"另一份 buffer"在稍后被误当成已接受的数据。
//! * 正因为"接受 ≠ 上线"，**owner 必须驱动 flush/shutdown**：只在
//!   `poll_write` 与 `poll_read` 之间来回、从不 `poll_flush` 的调用方，其
//!   `Ready` 接受的字节只会在后续任一次 `poll_write`/`poll_read`/
//!   `poll_flush`/`poll_shutdown` 里被封箱下发（本类型在四个入口都会尽力
//!   推进写方向，不会让已接受的字节饿死），但"已接受字节全部上线"这个结论
//!   只能由 `poll_flush` 返回 `Ready(Ok(()))` 给出。
//!
//! # 与 §5.2 的对应关系
//!
//! * **认证前不交付明文**：读方向先把到达字节交给 [`RecordReassembler`] 成帧，
//!   只有 `open` 成功的记录才进入待交付明文队列；坏 tag/坏帧 ⇒ 一个字节都不
//!   交付（`Ready(Ok(0))` 只用于表示底层干净 EOF）。
//! * **空明文记录不是 EOF**：明文 0 字节的合法记录解封后队列仍为空，读方向
//!   继续等待下一条记录，绝不返回 0 字节。
//! * **两阶段发送**：`prepare` 预封箱 ⇒ `wire` **整条**推到底层 ⇒
//!   [`TxRecordLayer::commit`] 才推进 `TxSeq`。任何 `Pending`/`Err` 都不
//!   commit（序号不前进 ⇒ 不可能出现"本地序号已用、对端没收到"的错位）；一次
//!   `prepare` 只消耗一份 nonce（在途记录保存在内部，重入续写同一条，绝不
//!   重封箱、绝不重发）；写出结果不确定（底层报错、底层报 0 字节、SecureIo 被
//!   drop）⇒ 整对拉闸（`PreparedRecord::Drop` 兜底）。
//! * **每条记录独立 30s 期限**：读方向从**该记录第一个字节到达**起算
//!   [`RECORD_IO_DEADLINE`]，同一记录的后续碎片**不刷新**；上一条记录一凑齐
//!   就立刻撤掉旧 deadline，下一条记录的首字节（哪怕与上一条的最后一个字节
//!   来自同一个 read chunk）重新锚定 30s。没有首字节不启动。写方向从**该记录
//!   第一次写出受阻**起算同样 30s，重入不刷新，整条记录 commit 后撤除。两条
//!   期限都向内部的 `WakeCenter` 唤醒中心登记（见下一节），并且**在任何入口推进
//!   底层与收尾状态机之前**核验——已到期是连接级事实，不会被"收尾正 Pending"遮掉。
//! * **单侧失败 = 双向终止并释放底层**：任何记录级/计时/底层错误都置 sticky
//!   fatal、丢弃在途记录与已接受明文、作废重组缓冲与整对 key/IV/序号，并
//!   **立即 take/drop 底层传输对象**（不是只 `poll_shutdown` 写半边——那只
//!   会让对端读到 FIN 而本端继续持有 TCP）。同时唤醒此前 park 在另一方向的
//!   调用者；此后两个方向的一切 `poll_*` 返回同一错误。
//! * **末有效序号主动收尾**：commit 返回 `2^64-2`（末有效序号）后进入
//!   `closing`：不再接受任何明文，并由 `poll_write`/`poll_read`/
//!   `poll_flush`/`poll_shutdown` 四个入口共同推进 flush → shutdown 收尾。
//!   收尾未完成时这些入口一律如实返回 `Pending`（绝不假报 `Ready(Ok)`），
//!   收尾完成（`Done`）后**释放底层**（整条 TCP 关闭）。后续写一律
//!   [`SecureIoFailure::SequenceExhausted`]。
//!
//! # 计时 waker 的所有权（谁会被到点的期限叫醒）
//!
//! 一个 `tokio::time::Sleep` **只保留最后一次 poll 它的那个 waker**。读写两个方向
//! 常常来自不同任务（`tokio::io::split` + 各自 `spawn` 是常态），因此"期限跟着最近
//! 一次 poll 的任务走"必然漏：读方向顺手推进写方向的计时 ⇒ parked writer 的 waker
//! 被覆盖 ⇒ 读任务 `Ready` 退出 ⇒ 到点只叫醒一个已经结束的任务；换成"固定算读侧"
//! 或"固定算写侧"也一样，那一方被取消或换任务就会再漏。
//!
//! 本类型因此持有一个**稳定 `Arc` 唤醒中心**（`WakeCenter`，私有）：
//!
//! * 两条 `Sleep` 只登记中心生成的**广播 waker**（同一 `Arc` ⇒ `will_wake` 命中，
//!   不会反复重登记）；中心只保留**两个有界槽**——读方向与写方向最近一次 `poll_*`
//!   的任务 waker；
//! * 期限到点、连接致命、以及收尾 `Done` 释放底层，都是**同一次广播**：临界区内
//!   只取出两个 `Option`，**释放锁之后**才依次 `wake()`（不锁内 wake、不跨 poll
//!   持锁、无 `unsafe`、无后台任务）；
//! * 取消任一侧、单侧先结束、换另一个任务接手，都不会让另一侧失去唤醒；每个槽
//!   只留最新一份 ⇒ 旧任务 waker 最多两份，不会累积。
//!
//! # 状态 × 入口的优先级（谁先看谁）
//!
//! | 状态 | 正容量 `poll_read` | `poll_write` | `poll_flush` / `poll_shutdown` |
//! | --- | --- | --- | --- |
//! | sticky fatal | 同一 sticky 错误（**先于**零容量约定） | 同一 sticky 错误 | 同一 sticky 错误 |
//! | 期限已到期（RX 首字节 / TX 受阻） | **立即 fatal**，先于交付明文、EOF 与收尾 | **立即 fatal**，先于接受与收尾 | **立即 fatal**，先于 flush/shutdown 推进 |
//! | 零容量 read | `Ready(Ok(0 字节))`：不消费底层、不推进收尾 | — | — |
//! | `closing`（末有效序号）收尾 Pending | `Pending`（不假报 EOF），但**先**核验期限 | `Pending`（绝不接受 NEW），但**先**核验期限 | `Pending`（绝不假报成功），但**先**核验期限 |
//! | 收尾 `Done` 且 `closing` | 已认证剩余明文 → EOF | `SequenceExhausted` | `Ready(Ok(()))`，底层已释放 |
//!
//! "先核验期限"不等于"重锚期限"：核验只 poll 既有 `Sleep`，绝不新建、绝不改锚；
//! 原 deadline 依旧只在首字节/首次受阻处锚定，只在凑齐/commit 处撤除。收尾自身也
//! **不引入任何新期限**（不新造 close timeout）——它只是不得再遮蔽既有期限。
//!
//! # 缓冲上界
//!
//! * 接收方向：未完成记录由 [`RecordReassembler`] 持有，残留字节任何时刻
//!   ≤ 65540；长度前缀一到 4 字节就判定，超限前缀不需要等 payload 就能终止。
//! * 发送方向：未封箱明文 ≤ [`MAX_PLAINTEXT_LEN`]（65520B，一条记录）+ 至多
//!   一条已封箱未确认的 wire 记录（≤ 65540B）。缓冲满且底层受阻时
//!   `poll_write` 返回 `Pending`，这就是写方向的背压。

use std::error::Error;
use std::fmt;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep, sleep_until};

use rsetup_protocol::crypto::{CryptoError, MAX_PLAINTEXT_LEN, PreparedRecord, RecordLayerPair};
use rsetup_protocol::handshake_adapter::{AdapterError, RecordReassembler};

/// 单条记录"部分到达/部分写出"的固定期限（§5.2 记录层部分 I/O deadline）。
///
/// 读方向从**该记录首字节到达**起算，写方向从**该记录首次写出受阻**起算；
/// 同一记录期间到达的碎片、重入的 `poll_*` **都不刷新**计时；记录结束（读向
/// 凑齐 / 写向 commit）后撤除，下一条记录重新锚定。到点仍未凑齐/推完 ⇒ 连接
/// 级致命。
pub const RECORD_IO_DEADLINE: Duration = Duration::from_secs(30);

/// 一次底层读的最大取用字节数（只是 poll 粒度，不构成协议缓冲：未完成记录
/// 的保留量由 [`RecordReassembler`] 的 65540 上界约束）。
const READ_SCRATCH: usize = 16 * 1024;

/// 发送方向允许使用的最后一个序号（`2^64-2`，§5.2：`2^64-1` 为禁用哨兵）。
const LAST_VALID_SEQ: u64 = u64::MAX - 1;

/// `SecureIo` 的连接级终止原因（去敏，不含明文/密钥/对端数据）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SecureIoFailure {
    /// 记录认证失败（tag/AAD 不符）：不交付任何明文。
    AuthFailed,
    /// 分帧/长度非法（含重组器致命：`<16`、`>65536`、非法前缀）。
    Framing,
    /// 对端在记录未收完时干净结束（截断）：不能当成正常 EOF。
    Truncated,
    /// 部分记录读取超过 [`RECORD_IO_DEADLINE`]（该记录首字节起算）。
    ReadDeadline,
    /// 记录写出受阻超过 [`RECORD_IO_DEADLINE`]（该记录首次受阻起算）。
    WriteDeadline,
    /// 序号空间耗尽（发送方向已发出末有效序号，或接收方向期望 `2^64-1`）。
    SequenceExhausted,
    /// 记录层对本已 fatal（构造时即如此，或对向/自身失败后拉闸）。
    PairFatal,
    /// 底层写出失败：已封箱记录的写出结果不确定，禁止重发。
    WriteFailed,
    /// 底层其它错误（读侧 IO 错误等）。
    Underlying,
    /// 本端已关闭写方向（`poll_shutdown` 已完成）：不得再写入任何明文。
    Closed,
}

impl fmt::Display for SecureIoFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthFailed => f.write_str("record authentication failed"),
            Self::Framing => f.write_str("record framing violation"),
            Self::Truncated => f.write_str("connection ended inside a partial record"),
            Self::ReadDeadline => f.write_str("partial record read deadline exceeded"),
            Self::WriteDeadline => f.write_str("record write deadline exceeded"),
            Self::SequenceExhausted => f.write_str("record sequence space exhausted"),
            Self::PairFatal => f.write_str("record layer pair is fatal"),
            Self::WriteFailed => f.write_str("underlying write failed with uncertain result"),
            Self::Underlying => f.write_str("underlying transport error"),
            Self::Closed => f.write_str("write side of this connection is closed"),
        }
    }
}

impl Error for SecureIoFailure {}

/// 从 `io::Error` 取回 [`SecureIoFailure`]（不是本模块产生的错误返回 `None`）。
///
/// `SecureIo` 的 `poll_*` 错误一律以 `io::Error::other(kind)` 形式返回，调用方
/// （h2 适配层、握手编排）据此区分"记录级致命"与普通 IO 错误，且不需要匹配
/// `ErrorKind`（`Other` 太宽，无法区分认证失败与计时到点）。
pub fn secure_io_failure(err: &io::Error) -> Option<SecureIoFailure> {
    err.get_ref()?.downcast_ref::<SecureIoFailure>().copied()
}

fn fatal_error(kind: SecureIoFailure) -> io::Error {
    io::Error::other(kind)
}

// ----------------------------------------------------------------------
// 计时 waker 的所有权：稳定双向唤醒中心
// ----------------------------------------------------------------------

/// 唤醒中心的两个 owner 槽（读方向、写方向各一个）。
#[derive(Debug, Default)]
struct OwnerSlots {
    read: Option<Waker>,
    write: Option<Waker>,
}

/// **稳定的双向唤醒中心**：本对象内所有 `Sleep`（读期限、写期限）都只向它登记的
/// 广播 waker 注册，绝不向"最近一次 poll 的调用任务"注册。
///
/// 为什么必须由它持有计时（而不是各方向的 `cx`）：一个 `tokio::time::Sleep`
/// **只保留最后一次 poll 它的那个 waker**。读写两个方向经常来自不同任务
/// （`tokio::io::split` + 各自 `spawn` 是常态，单线程运行时用锁也无法改变这一点），
/// 于是"谁最后 poll 谁当 owner"一定会漏：
///
/// * 读方向顺手推进写方向的计时 ⇒ parked writer 的 waker 被读任务的 waker 覆盖；
///   读任务随后 `Ready` 退出 ⇒ 计时到点只叫醒一个已经结束的任务，写方向永久 park；
/// * 固定挑读侧或写侧当 owner 同样错：那一方被取消、drop 或换任务，就会再漏一次。
///
/// 本类型只保留**两个有界槽**（读写各一，保存该方向最近一次 poll 的 waker），
/// 计时到点或连接致命时把两个槽**一次性取出、释放锁后再依次 `wake()`**：
///
/// * 绝不在持锁时 `wake`，也不跨 `poll` 持锁；临界区只有两次 `Option` 赋值；
/// * 每槽只留该方向**最新**的 waker ⇒ 旧任务/被取消任务的 waker 最多两个，
///   不会无限累积；
/// * 取消任一侧、单侧结束、换任务接手，都不会让另一侧失去计时唤醒；
/// * 由同一 `Arc` 克隆出的 `Waker` 满足 `Waker::will_wake`，`Sleep` 不会因为
///   每次 poll 新建 `Waker` 而反复重登记。
#[derive(Debug, Default)]
struct WakeCenter {
    slots: Mutex<OwnerSlots>,
}

impl WakeCenter {
    /// 锁的中毒只可能由本模块内在持锁期间 panic 造成（临界区只有 `Option`
    /// 赋值），此时直接 panic 是最诚实的处置。
    const LOCK_POISON: &'static str = "SecureIo 唤醒中心锁中毒";

    fn register_read(&self, waker: &Waker) {
        let mut slots = self.slots.lock().expect(Self::LOCK_POISON);
        if slots.read.as_ref().is_none_or(|old| !old.will_wake(waker)) {
            slots.read = Some(waker.clone());
        }
    }

    fn register_write(&self, waker: &Waker) {
        let mut slots = self.slots.lock().expect(Self::LOCK_POISON);
        if slots.write.as_ref().is_none_or(|old| !old.will_wake(waker)) {
            slots.write = Some(waker.clone());
        }
    }

    /// 广播唤醒两个方向：先在临界区内**取出**双方，**释放锁**之后才 `wake`。
    fn wake_both(&self) {
        let (read, write) = {
            let mut slots = self.slots.lock().expect(Self::LOCK_POISON);
            (slots.read.take(), slots.write.take())
        };
        // 锁在此处已释放：wake() 可能立刻把对向任务跑起来并再次进入本模块。
        if let Some(waker) = read {
            waker.wake();
        }
        if let Some(waker) = write {
            waker.wake();
        }
    }

    /// 交给 `Sleep` 登记的稳定广播 waker。
    fn waker(self: &Arc<Self>) -> Waker {
        Waker::from(Arc::clone(self))
    }
}

impl Wake for WakeCenter {
    fn wake(self: Arc<Self>) {
        self.wake_both();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wake_both();
    }
}

/// 用**中心广播 waker** poll 一个已存在的期限，返回是否已到点。
///
/// 本函数只会：把 `Sleep` 的 waker 保持成中心（幂等，`will_wake` 命中 ⇒ 不重登记）、
/// 读出"是否到点"。它**不新建**期限、**不改锚**（重锚只发生在读首字节到达 / 写
/// 首次受阻，撤除只发生在记录凑齐 / commit），因此反复核验绝不会延长原期限。
fn poll_deadline(wake: &Arc<WakeCenter>, slot: &mut Option<Pin<Box<Sleep>>>) -> bool {
    let Some(sleep) = slot.as_mut() else {
        return false;
    };
    let waker = wake.waker();
    let mut cx = Context::from_waker(&waker);
    sleep.as_mut().poll(&mut cx).is_ready()
}

/// 接收方向的内部状态。
#[derive(Debug)]
struct RxState {
    /// 分帧重组：残留字节上界由它自己保证（≤ 65540）。
    reassembler: RecordReassembler,
    /// **已通过认证但尚未交付**的明文字节（`plain_pos` 之后为未交付部分）。
    plain: Vec<u8>,
    plain_pos: usize,
    /// **当前这条未完成记录**的部分读取期限（首字节到达时锚定，凑齐即撤除，
    /// 下一条记录重新锚定；碎片不刷新）。
    deadline: Option<Pin<Box<Sleep>>>,
    /// 底层已干净 EOF。
    eof: bool,
}

impl RxState {
    fn new() -> Self {
        Self {
            reassembler: RecordReassembler::new(),
            plain: Vec::new(),
            plain_pos: 0,
            deadline: None,
            eof: false,
        }
    }
}

/// 发送方向的内部状态。
#[derive(Debug)]
struct TxState {
    /// 已 `Ready(Ok(n))` **接受但尚未封箱**的明文字节（有界 ≤ 65520B）。
    ///
    /// 这是"buffered AsyncWrite"的接受点：`poll_write` 一旦把入参前 `n` 字节
    /// 复制进来就立刻返回 `Ready(Ok(n))`，返回 `Pending` 时本字段**不因本次
    /// 调用而改变**（未接受的入参不留痕）。
    buffered: Vec<u8>,
    /// 已封箱、**尚未确认真正写完整**的记录（至多一条）。
    pending: Option<PreparedRecord>,
    /// `pending` 已写到底层的字节数（同一记录重入时从此处续写，绝不重封箱）。
    written: usize,
    /// **当前这条记录**首次受阻起算的写出期限（commit 后撤除）。
    deadline: Option<Pin<Box<Sleep>>>,
}

impl TxState {
    fn new() -> Self {
        Self {
            buffered: Vec::new(),
            pending: None,
            written: 0,
            deadline: None,
        }
    }
}

/// `push_pending` 单次底层写的结果（先把结论取出，再统一处理借用）。
enum WriteStep {
    Wrote(usize),
    Blocked,
    Broken,
}

/// 在途记录是否被推完。
enum Block {
    Clear,
    Blocked,
}

/// 主动收尾（末有效序号后，或 owner 显式 `poll_shutdown`）的推进阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClosePhase {
    Open,
    Flushing,
    ShuttingDown,
    Done,
}

/// 把 [`RecordLayerPair`] 架在 `AsyncRead + AsyncWrite` 传输上的加密记录流。
///
/// 构造即拥有 `io`：`SecureIo` 被 drop 时底层一并 drop（关闭连接），**没有
/// 后台任务**；连接级致命则**立即**释放底层（见模块文档）。上层（h2）看到的
/// 仍然是一条普通的字节流，全部记录边界与序号都在本类型内部处理。
///
/// 写方向的契约（接受即 `Ready`、`Pending` 不接受、owner 驱动 flush）见模块
/// 文档"写方向是**有界 buffered** AsyncWrite"一节。
#[derive(Debug)]
pub struct SecureIo<S> {
    /// 底层传输。`None` = 已释放（连接级致命，或末有效序号收尾完成）：此后
    /// 不可能再有任何字节上线或到达。
    io: Option<S>,
    /// 记录层对。`None` = 已作废并释放双向 key/IV/序号状态（连接级致命）。
    pair: Option<RecordLayerPair>,
    rx: RxState,
    tx: TxState,
    /// sticky 终止原因：一旦 Some，两个方向的一切操作都返回它。
    fatal: Option<SecureIoFailure>,
    /// 末有效序号已发出 ⇒ 不再接受任何明文，并推进关闭。
    closing: bool,
    close_phase: ClosePhase,
    /// **双向唤醒中心**：两个方向的 owner 槽，以及两条期限 `Sleep` 登记的广播
    /// waker（所有权规则见 `WakeCenter` 的文档）。
    wake: Arc<WakeCenter>,
}

impl<S> SecureIo<S> {
    /// 用已建立的记录层对包装底层传输。
    ///
    /// `pair` 已 fatal（例如上一段连接失败后被误用）时，本对象立即进入 fatal
    /// 状态并**当场释放**记录层与底层（绝不把熔断过的密钥/序号当成新连接继续
    /// 持有）：两方向的一切操作都返回 [`SecureIoFailure::PairFatal`]。
    pub fn new(io: S, pair: RecordLayerPair) -> Self {
        if pair.is_fatal() {
            // 先丢弃 pair（含两份 key/IV），再丢弃底层：不保留任何会话材料。
            drop(pair);
            return Self {
                io: None,
                pair: None,
                rx: RxState::new(),
                tx: TxState::new(),
                fatal: Some(SecureIoFailure::PairFatal),
                closing: false,
                close_phase: ClosePhase::Done,
                wake: Arc::new(WakeCenter::default()),
            };
        }
        Self {
            io: Some(io),
            pair: Some(pair),
            rx: RxState::new(),
            tx: TxState::new(),
            fatal: None,
            closing: false,
            close_phase: ClosePhase::Open,
            wake: Arc::new(WakeCenter::default()),
        }
    }

    /// 是否已连接级终止（sticky）。为 true 后本对象不再可能交付或写出任何
    /// 明文，记录层密钥与底层传输都已释放（调用方无需再做任何收尾）。
    pub fn is_fatal(&self) -> bool {
        self.fatal.is_some()
    }

    /// 发送方向当前待写序号（观测用；`commit` 之后才前进）。
    /// 连接级终止后记录层已释放 ⇒ `None`。
    pub fn tx_seq(&self) -> Option<u64> {
        self.pair.as_ref().map(|pair| pair.tx.tx_seq())
    }

    /// 接收方向期望序号（观测用）。连接级终止后 ⇒ `None`。
    pub fn rx_seq(&self) -> Option<u64> {
        self.pair.as_ref().map(|pair| pair.rx.rx_seq())
    }

    /// 借用内部记录层对（只读观测用；不得用它绕过两阶段发送）。
    /// 连接级终止后已释放 ⇒ `None`。
    pub fn pair(&self) -> Option<&RecordLayerPair> {
        self.pair.as_ref()
    }

    /// **已 `Ready` 接受但尚未封箱**的明文字节数（0..=65520）。
    ///
    /// 观测"buffered 写方向"的背压位置：本值等于上限时 `poll_write` 只会返回
    /// `Pending`（本次不接受任何字节）。
    pub fn tx_buffered_len(&self) -> usize {
        self.tx.buffered.len()
    }

    /// 本端写方向是否已关闭（末有效序号收尾完成，或 `poll_shutdown` 已完成）。
    pub fn is_write_closed(&self) -> bool {
        self.closing || self.close_phase != ClosePhase::Open
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> SecureIo<S> {
    // ------------------------------------------------------------------
    // 内部工具
    // ------------------------------------------------------------------

    /// 把本方向**最近一次 poll 的任务**登记进唤醒中心的对应槽位。
    ///
    /// 登记只影响"谁会被期限/致命唤醒"，绝不改变任何状态机；两个槽位各只保存
    /// 一份 waker（同任务重复 poll 由 `will_wake` 免克隆），因此被取消或换人的
    /// 任务不会在中心里累积。
    fn register_reader(&self, cx: &Context<'_>) {
        self.wake.register_read(cx.waker());
    }

    fn register_writer(&self, cx: &Context<'_>) {
        self.wake.register_write(cx.waker());
    }

    /// 连接级终止（§5.2"立即终止整条 TCP 连接，使双向连接密钥、IV、序号、
    /// 接收缓冲全部失效"）：
    ///
    /// 1. sticky 原因（首个原因胜出，去敏固定文案）；
    /// 2. 丢弃在途已封箱记录与**已接受但未封箱的明文**（写出结果不确定的记录
    ///    禁止重发，`PreparedRecord::Drop` 亦会拉闸）；
    /// 3. 丢弃已认证未交付的明文与重组缓冲（不得再交付任何字节、不得重同步）；
    /// 4. **释放记录层对**（双向 key/IV/序号状态一并 drop）；
    /// 5. **立即 take/drop 底层传输**：只 `poll_shutdown(Write)` 是不够的
    ///    （对端只读到 FIN，本端仍持有整条 TCP）；drop 才是关闭整条连接；
    /// 6. 唤醒此前 park 在另一方向的任务。
    ///
    /// 返回可直接上抛的 `io::Error`（`secure_io_failure` 可取回原因）。
    fn terminate(&mut self, kind: SecureIoFailure) -> io::Error {
        if self.fatal.is_none() {
            self.fatal = Some(kind);
        }
        self.tx.pending = None;
        self.tx.buffered.clear();
        self.tx.written = 0;
        self.tx.deadline = None;
        self.rx.plain.clear();
        self.rx.plain_pos = 0;
        self.rx.deadline = None;
        self.rx.reassembler = RecordReassembler::new();
        self.rx.eof = true;
        self.closing = true;
        self.close_phase = ClosePhase::Done;
        // 顺序：先作废密钥，再释放底层（drop 顺序即关闭整条 TCP 的时刻）。
        self.pair = None;
        self.io = None;
        // 同一次广播唤醒两个方向（含调用方自己）：取出后释放锁才 wake。
        self.wake.wake_both();
        fatal_error(kind)
    }

    /// 写方向是否已不可用（致命之外的两种"关闭"：末有效序号 / 显式 shutdown）。
    fn write_closed_kind(&self) -> Option<SecureIoFailure> {
        if self.closing {
            return Some(SecureIoFailure::SequenceExhausted);
        }
        if self.close_phase != ClosePhase::Open || self.io.is_none() {
            return Some(SecureIoFailure::Closed);
        }
        None
    }

    /// 记录层/适配层错误 ⇒ [`SecureIoFailure`]（保持"错误码不同、处置相同"）。
    fn classify_crypto(err: CryptoError) -> SecureIoFailure {
        match err {
            CryptoError::AuthFailed => SecureIoFailure::AuthFailed,
            CryptoError::SequenceExhausted => SecureIoFailure::SequenceExhausted,
            CryptoError::Fatal => SecureIoFailure::PairFatal,
            CryptoError::BadFrame | CryptoError::RecordTooSmall | CryptoError::RecordTooLarge => {
                SecureIoFailure::Framing
            }
            // 状态机错乱（在途未 commit / commit 归属不符 / 原语内部错误）：
            // 无法判定 nonce 是否已被消耗，只能整对作废。
            CryptoError::PendingOutstanding
            | CryptoError::CommitMismatch
            | CryptoError::Internal => SecureIoFailure::PairFatal,
            // 密钥协商/派生不可能在记录层出现（握手阶段的错误），保守归为致命。
            _ => SecureIoFailure::PairFatal,
        }
    }

    fn classify_adapter(err: AdapterError) -> SecureIoFailure {
        match err {
            AdapterError::RecordTooSmall(_)
            | AdapterError::RecordTooLarge(_)
            | AdapterError::ReassemblerFatal => SecureIoFailure::Framing,
            AdapterError::Crypto(c) => Self::classify_crypto(c),
            _ => SecureIoFailure::Framing,
        }
    }

    // ------------------------------------------------------------------
    // 写方向
    // ------------------------------------------------------------------

    /// 当前记录的写出期限：首次受阻武装（同一记录重入**不刷新**，原期限绝不
    /// 重锚）；没有在途记录可推 ⇒ 撤除（下一条记录重新锚定）。
    ///
    /// 新建的 `Sleep` 立刻用**中心广播 waker** poll 一次：`Sleep` 只有在被 poll
    /// 之后才登记 waker，否则底层长期不可写时计时到点无人唤醒（登记对象是中心，
    /// 不是本次调用任务 ⇒ 之后读方向怎么 poll 都不会顶掉写方向的唤醒权）。
    fn arm_write_deadline(&mut self) {
        if self.tx.pending.is_none() {
            self.tx.deadline = None;
            return;
        }
        if self.tx.deadline.is_none() {
            self.tx.deadline = Some(Box::pin(sleep_until(Instant::now() + RECORD_IO_DEADLINE)));
            let Self { wake, tx, .. } = self;
            let _ = poll_deadline(wake, &mut tx.deadline);
        }
    }

    /// 统一核验**已存在**的连接期限（读、写两向）：任一到点 ⇒ 连接级致命，返回
    /// 可直接上抛的错误。
    ///
    /// 调用位置规则（四个入口一致）：**在推进底层 IO 与收尾状态机之前**核验。
    /// 理由是期限是连接级事实，而"收尾动作正 Pending"只是某一次 poll 的临时结论：
    /// 若先 `ready!(close)`，末有效序号后底层 flush/shutdown 长期推不动时，已经
    /// 到点的部分记录期限会被永久遮成 `Pending`（实测被遮蔽 30s 以上）。
    ///
    /// 两条期限同时到点时先报读向（`ReadDeadline`）：读期限由首字节锚定，通常也
    /// 是更早的那一条；两者都是同一条连接的 fatal，码位只影响去敏文案。
    fn check_connection_deadline(&mut self) -> Option<io::Error> {
        let (rx_fired, tx_fired) = {
            let Self { wake, rx, tx, .. } = self;
            (
                poll_deadline(wake, &mut rx.deadline),
                poll_deadline(wake, &mut tx.deadline),
            )
        };
        if rx_fired {
            return Some(self.terminate(SecureIoFailure::ReadDeadline));
        }
        if tx_fired {
            return Some(self.terminate(SecureIoFailure::WriteDeadline));
        }
        None
    }

    /// 把已接受明文封成**一条**记录（本类型唯一消耗 nonce 的位置）。
    ///
    /// 同一时刻至多一条在途记录，因此不存在"同序号封两次"；已封箱记录绝不被
    /// 重新封箱或重发。
    fn seal_buffered(&mut self) -> Result<(), SecureIoFailure> {
        let prepared = {
            let Self { pair, tx, .. } = self;
            if tx.pending.is_some() || tx.buffered.is_empty() {
                return Ok(());
            }
            let Some(pair) = pair.as_mut() else {
                return Err(SecureIoFailure::PairFatal);
            };
            match pair.tx.prepare(&tx.buffered[..]) {
                Ok(prepared) => prepared,
                Err(err) => return Err(Self::classify_crypto(err)),
            }
        };
        self.tx.pending = Some(prepared);
        self.tx.written = 0;
        self.tx.buffered.clear();
        Ok(())
    }

    /// 把在途记录**剩余**的 wire 字节推到底层。本函数不 commit，因此不存在
    /// "部分写出却推进序号"的状态。
    fn push_pending(&mut self, cx: &mut Context<'_>) -> Result<Block, SecureIoFailure> {
        loop {
            // 分离借用：`wire` 只读借用 `tx`，底层写只借用 `io`；重入时直接
            // 切片剩余字节，不必每次 poll 复制一份（满记录 65540B）。
            let step = {
                let Self { io, tx, .. } = self;
                let Some(pending) = tx.pending.as_ref() else {
                    return Ok(Block::Clear);
                };
                let wire = pending.wire();
                let written = tx.written;
                if written >= wire.len() {
                    return Ok(Block::Clear);
                }
                let Some(io) = io.as_mut() else {
                    // 底层已被释放（致命或收尾完成）：这条已封箱记录不可能再
                    // 确认写出 ⇒ 结果不确定，只能整对作废。
                    return Err(SecureIoFailure::WriteFailed);
                };
                match Pin::new(io).poll_write(cx, &wire[written..]) {
                    Poll::Pending => WriteStep::Blocked,
                    Poll::Ready(Err(_)) | Poll::Ready(Ok(0)) => WriteStep::Broken,
                    Poll::Ready(Ok(n)) => WriteStep::Wrote(n),
                }
            };
            match step {
                WriteStep::Wrote(n) => self.tx.written += n,
                WriteStep::Blocked => return Ok(Block::Blocked),
                WriteStep::Broken => {
                    // 写出失败、底层报 0 字节、或底层已被释放：这条已封箱记录
                    // 的写出结果无法确认（对端可能只收到半条）⇒ 整对作废、
                    // 禁止重发（§5.2）。
                    return Err(SecureIoFailure::WriteFailed);
                }
            }
        }
    }

    /// 在途记录已完整上线 ⇒ 推进 `TxSeq`（**唯一**推进点），并撤除本条记录的
    /// 写出期限。发出末有效序号后进入 `closing`。
    fn commit_pending(&mut self) -> Result<(), SecureIoFailure> {
        let Some(pending) = self.tx.pending.take() else {
            return Ok(());
        };
        self.tx.written = 0;
        self.tx.deadline = None;
        let result = match self.pair.as_mut() {
            Some(pair) => pair.tx.commit(pending),
            None => return Err(SecureIoFailure::PairFatal),
        };
        match result {
            Ok(seq) => {
                if seq == LAST_VALID_SEQ {
                    if !self.tx.buffered.is_empty() {
                        // 在途期间不得另接受明文。若该不变量失守，必须报错，
                        // 不能清空已接受字节后让flush成功。
                        return Err(SecureIoFailure::WriteFailed);
                    }
                    self.closing = true;
                }
                Ok(())
            }
            Err(err) => Err(Self::classify_crypto(err)),
        }
    }

    /// 本次 `poll_write` 可以合法接受的字节数。
    ///
    /// 上界是**一条记录的明文上限**（65520B），并且**有在途记录时为 0**：
    /// 记录必须按序上线，绝不插队、绝不同时攒两条记录，因此"已接受但未上线"
    /// 的明文任何时刻至多一条记录。缓冲满（或在途未推完）且底层受阻 ⇒
    /// `poll_write` 返回 `Pending`，这就是写方向的背压。
    fn accept_room(&self) -> usize {
        if self.tx.pending.is_some() {
            0
        } else {
            MAX_PLAINTEXT_LEN.saturating_sub(self.tx.buffered.len())
        }
    }

    /// 尽力推进写方向：先 drain 在途记录，再把已接受明文封箱下发。
    ///
    /// * `Ok(Poll::Ready(()))` = 没有在途记录、也没有已接受未发送的明文；
    /// * `Ok(Poll::Pending)` = 底层受阻（已武装**本条记录**的 30s 期限，登记到
    ///   唤醒中心）；调用方**不得**据此宣称本次接受了任何新数据。
    /// * `Err` = 已 [`Self::terminate`]。
    ///
    /// 期限的**核验**由各入口在推进之前统一完成（[`Self::check_connection_deadline`]），
    /// 本函数只负责"受阻 ⇒ 武装本条记录"与"推进/commit ⇒ 撤除"。
    fn drive_tx(&mut self, cx: &mut Context<'_>) -> Result<Poll<()>, io::Error> {
        loop {
            if self.fatal.is_some() || self.io.is_none() {
                return Ok(Poll::Ready(()));
            }
            if self.tx.pending.is_some() {
                match self.push_pending(cx) {
                    Err(kind) => return Err(self.terminate(kind)),
                    Ok(Block::Blocked) => {
                        self.arm_write_deadline();
                        return Ok(Poll::Pending);
                    }
                    Ok(Block::Clear) => {
                        if let Err(kind) = self.commit_pending() {
                            return Err(self.terminate(kind));
                        }
                    }
                }
                continue;
            }
            if self.closing {
                self.tx.deadline = None;
                return Ok(Poll::Ready(()));
            }
            if self.tx.buffered.is_empty() {
                // 没有待发记录 ⇒ 当前记录没有"受阻"可言，撤掉计时。
                self.tx.deadline = None;
                return Ok(Poll::Ready(()));
            }
            if let Err(kind) = self.seal_buffered() {
                return Err(self.terminate(kind));
            }
        }
    }

    /// 推进"末有效序号之后"（或 owner 显式 shutdown）的收尾：flush → shutdown。
    ///
    /// **如实传播 `Pending`**：收尾没完成就返回 `Pending`，绝不返回
    /// `Ready(Ok(()))`（否则调用方会把"半关闭/未关闭"误当成连接已平滑收尾）。
    /// 四个入口（read/write/flush/shutdown）都会调用本函数，因此不依赖"写完
    /// 之后一定还会被 poll 写方向"这个假设。
    ///
    /// `Done` 之后：若处于 `closing`（序号耗尽，§5.2 要求关闭整条 TCP）则
    /// **释放底层**；owner 显式 `poll_shutdown` 只关闭写方向，读方向仍可由
    /// `poll_read` 交付已认证明文。
    /// 只在已进入关闭流程时推进；所有入口共享Pending/错误传播规则。
    fn drive_started_close(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.closing || self.close_phase != ClosePhase::Open {
            self.drive_close(cx)
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn drive_close(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        loop {
            match self.close_phase {
                ClosePhase::Open => self.close_phase = ClosePhase::Flushing,
                ClosePhase::Flushing => {
                    let poll = match self.io.as_mut() {
                        Some(io) => Pin::new(io).poll_flush(cx),
                        None => Poll::Ready(Ok(())),
                    };
                    match poll {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(())) => self.close_phase = ClosePhase::ShuttingDown,
                        Poll::Ready(Err(_)) => {
                            return Poll::Ready(Err(self.terminate(SecureIoFailure::Underlying)));
                        }
                    }
                }
                ClosePhase::ShuttingDown => {
                    let poll = match self.io.as_mut() {
                        Some(io) => Pin::new(io).poll_shutdown(cx),
                        None => Poll::Ready(Ok(())),
                    };
                    match poll {
                        Poll::Pending => return Poll::Pending,
                        Poll::Ready(Ok(())) => self.close_phase = ClosePhase::Done,
                        Poll::Ready(Err(_)) => {
                            return Poll::Ready(Err(self.terminate(SecureIoFailure::Underlying)));
                        }
                    }
                }
                ClosePhase::Done => {
                    if self.closing && self.io.take().is_some() {
                        // 末有效序号收尾完成 ⇒ 整条 TCP 关闭（释放底层）。底层
                        // 对象已经不存在：此刻 park 在它上面的对向永远不会再被
                        // 底层唤醒，必须由同一个中心叫醒（同一次只发生一次）。
                        self.wake.wake_both();
                    }
                    return Poll::Ready(Ok(()));
                }
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for SecureIo<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.register_reader(cx);
        if let Some(kind) = this.fatal {
            return Poll::Ready(Err(fatal_error(kind)));
        }
        // 1) 已存在的连接期限**优先于收尾推进**：末有效序号后底层 flush/shutdown
        //    一直 Pending 时，也绝不能把已经到点的部分记录期限遮成 Pending。
        if let Some(err) = this.check_connection_deadline() {
            return Poll::Ready(Err(err));
        }
        // 2) 零容量读取不消费底层、也不作为驱动 close 的有效读需求（fatal 与到期
        //    期限已在上面优先处理：依约"不驱动"不等于"看不到已发生的致命"）。
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        // owner 完全可以只 poll 读：这里同样推进写方向与 closing 收尾，否则
        // "已 Ready 接受的明文"与"末有效序号后的关闭动作"会因为没有第二次
        // poll_write 而永远卡住。
        if let Err(err) = this.drive_tx(cx) {
            return Poll::Ready(Err(err));
        }
        std::task::ready!(this.drive_started_close(cx))?;

        loop {
            // 1) 计时优先：本条记录（或写方向）过期 ⇒ 连接级致命（不交付任何明文）。
            if let Some(err) = this.check_connection_deadline() {
                return Poll::Ready(Err(err));
            }
            // 2) 已认证明文优先交付（可能一次交不完，下次继续）。
            if this.deliver_plain(buf) {
                return Poll::Ready(Ok(()));
            }
            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            // 3) 干净 EOF（含末有效序号收尾后释放底层）⇒ 报 0 字节。
            if this.rx.eof || this.io.is_none() {
                this.rx.eof = true;
                return Poll::Ready(Ok(()));
            }
            // 4) 向底层要字节：上限不越过当前记录边界，避免"同一个 read 里
            //    上一条记录结束 + 下一条记录首字节"这种时间归属含糊的情形。
            let limit = this.rx.reassembler.read_limit(READ_SCRATCH).max(1);
            let mut scratch = [0u8; READ_SCRATCH];
            let mut read_buf = ReadBuf::new(&mut scratch[..limit]);
            let poll = match this.io.as_mut() {
                Some(io) => Pin::new(io).poll_read(cx, &mut read_buf),
                None => {
                    this.rx.eof = true;
                    return Poll::Ready(Ok(()));
                }
            };
            let filled = match poll {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => {
                    // 底层读错误：连接级终止（不在旧连接重同步、不重放任何明文）。
                    return Poll::Ready(Err(this.terminate(SecureIoFailure::Underlying)));
                }
                Poll::Ready(Ok(())) => read_buf.filled().len(),
            };
            if filled == 0 {
                // 底层干净结束：收完一半 ⇒ 截断致命；一条都没起头 ⇒ 正常 EOF。
                if this.rx.reassembler.buffered_len() > 0 {
                    return Poll::Ready(Err(this.terminate(SecureIoFailure::Truncated)));
                }
                this.rx.eof = true;
                this.rx.deadline = None;
                return Poll::Ready(Ok(()));
            }
            match this.rx.reassembler.push(&scratch[..filled]) {
                Ok(records) => {
                    // 本 chunk 是否凑齐过记录（= 越过记录边界）：必须在解封
                    // 之前记下，`ingest_records` 会消费掉这个 Vec。
                    let crossed_record = !records.is_empty();
                    if let Err(kind) = this.ingest_records(records) {
                        return Poll::Ready(Err(this.terminate(kind)));
                    }
                    // 每条记录独立 deadline：凑齐 ⇒ 立刻撤除旧 deadline；随后
                    // 新记录首字节（哪怕来自同一个 chunk）重新锚定 30s。
                    if crossed_record {
                        this.rx.deadline = None;
                    }
                    this.arm_read_deadline();
                    // 继续循环：可能已经交付明文，也可能这条记录还没收完。
                }
                Err(err) => {
                    let kind = Self::classify_adapter(err);
                    return Poll::Ready(Err(this.terminate(kind)));
                }
            }
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> SecureIo<S> {
    // ------------------------------------------------------------------
    // 读方向内部工具
    // ------------------------------------------------------------------

    /// 交付已认证明文（最多填满 `buf`）；返回是否交付了字节。
    fn deliver_plain(&mut self, buf: &mut ReadBuf<'_>) -> bool {
        let start = self.rx.plain_pos;
        let end = (start + buf.remaining()).min(self.rx.plain.len());
        if end == start {
            return false;
        }
        buf.put_slice(&self.rx.plain[start..end]);
        self.rx.plain_pos = end;
        if end == self.rx.plain.len() {
            self.rx.plain.clear();
            self.rx.plain_pos = 0;
        }
        true
    }

    /// **当前这条记录**的部分读取期限：有残留字节且未武装 ⇒ 以"现在"为首字节
    /// 时刻锚定；无残留 ⇒ 撤除；已武装时**绝不刷新**（碎片不重置计时）。
    ///
    /// 新建的 `Sleep` 立刻用**中心广播 waker** poll 一次完成登记（`Sleep` 只在被
    /// poll 后才登记 waker）。登记给中心而不是本次调用任务 ⇒ 之后写方向的 poll
    /// 不会顶掉读方向的唤醒权，反之亦然。
    fn arm_read_deadline(&mut self) {
        if self.rx.reassembler.buffered_len() == 0 {
            self.rx.deadline = None;
            return;
        }
        if self.rx.deadline.is_none() {
            self.rx.deadline = Some(Box::pin(sleep_until(Instant::now() + RECORD_IO_DEADLINE)));
            let Self { wake, rx, .. } = self;
            let _ = poll_deadline(wake, &mut rx.deadline);
        }
    }

    /// 处理重组器交付的完整记录：逐条解封，只有认证通过的明文才进待交付队列。
    fn ingest_records(&mut self, records: Vec<Vec<u8>>) -> Result<(), SecureIoFailure> {
        for record in records {
            match self.pair.as_mut() {
                Some(pair) => match pair.rx.open(&record) {
                    Ok(plaintext) => self.rx.plain.extend_from_slice(&plaintext),
                    Err(err) => return Err(Self::classify_crypto(err)),
                },
                None => return Err(SecureIoFailure::PairFatal),
            }
        }
        Ok(())
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncWrite for SecureIo<S> {
    /// 接受 `data` 的前 `n` 字节（有界 buffered 语义，见模块文档）。
    ///
    /// * 先尽力 drain 此前**已接受**的字节（在途记录优先，绝不插队）；
    /// * 之后把 `data` 的前 `n = min(data.len(), 65520 - 已缓冲)` 字节复制进
    ///   内部缓冲并**立刻**返回 `Ready(Ok(n))`，随即尽量封箱下发；
    /// * 缓冲满且底层受阻 ⇒ `Pending`：本次调用**不接受**任何字节，也不保存
    ///   `data`（调用方可改传任意别的 buffer，或复用同一块）。
    ///
    /// `data` 为空 ⇒ `Ready(Ok(0))`（不产生记录、不消耗序号）。
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.register_writer(cx);
        if let Some(kind) = this.fatal {
            return Poll::Ready(Err(fatal_error(kind)));
        }
        // 已到点的连接期限（读向与写向）优先：既不能被"收尾正 Pending"吞掉，
        // 也不能被"背压 Pending"吞掉。
        if let Some(err) = this.check_connection_deadline() {
            return Poll::Ready(Err(err));
        }
        // 已有close即便拒绝当前明文，也须先真实驱动到Pending/完成。
        std::task::ready!(this.drive_started_close(cx))?;
        if let Some(kind) = this.write_closed_kind() {
            return Poll::Ready(Err(fatal_error(kind)));
        }

        // 1) 先推进此前明确接受的字节（保证 `Pending` 一定意味着"本次 0 接受"）。
        if this.drive_tx(cx)?.is_pending() && this.accept_room() == 0 && !data.is_empty() {
            // 在途记录还没推完 ⇒ 不接受新明文（绝不插队、绝不替旧数据背书），
            // 并保证这条记录的计时已武装。
            this.arm_write_deadline();
            if let Some(err) = this.check_connection_deadline() {
                return Poll::Ready(Err(err));
            }
            return Poll::Pending;
        }
        // drain可以刚刚提交末有效seq；必须重新判状态，不能成功接受后再丢弃NEW。
        std::task::ready!(this.drive_started_close(cx))?;
        if let Some(kind) = this.write_closed_kind() {
            return Poll::Ready(Err(fatal_error(kind)));
        }
        // 2) 空写入：不接受任何字节（不产生记录、不消耗序号）。
        if data.is_empty() {
            return Poll::Ready(Ok(0));
        }
        // 3) 有界接受：把入参前 n 字节复制进内部缓冲。
        let room = this.accept_room();
        if room == 0 {
            this.arm_write_deadline();
            if let Some(err) = this.check_connection_deadline() {
                return Poll::Ready(Err(err));
            }
            return Poll::Pending;
        }
        let n = data.len().min(room);
        this.tx.buffered.extend_from_slice(&data[..n]);
        // 4) 立即尽力下发（能推多远推多远；推不动的由后续 poll 继续）。
        if let Err(err) = this.drive_tx(cx) {
            return Poll::Ready(Err(err));
        }
        // 5) 若这一步恰好发出了末有效序号：在**同一次调用**内就把收尾推到底
        //    （能推多远推多远）。不能假设"write 返回 Ready 之后一定还会被
        //    poll"——收尾动作在 read/flush/shutdown 里同样会被继续驱动。
        if (this.closing || this.close_phase != ClosePhase::Open)
            && let Poll::Ready(Err(err)) = this.drive_close(cx)
        {
            return Poll::Ready(Err(err));
        }
        Poll::Ready(Ok(n))
    }

    /// 只有"已 `Ready` 接受的字节全部封箱、整条整条写出并 commit"且底层
    /// `poll_flush` 成功后才返回 `Ready(Ok(()))`。
    ///
    /// 处于 `closing`（末有效序号已发出）时，flush **必须**把整个收尾状态机
    /// 推到 `Done` 才算成功；收尾没完成一律 `Pending`（不假报成功）。
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.register_writer(cx);
        if let Some(kind) = this.fatal {
            return Poll::Ready(Err(fatal_error(kind)));
        }
        if let Some(err) = this.check_connection_deadline() {
            return Poll::Ready(Err(err));
        }
        // 先把已接受的字节推完（半条记录不算 flush 成功）。
        if this.drive_tx(cx)?.is_pending() {
            return Poll::Pending;
        }
        if this.closing || this.close_phase != ClosePhase::Open {
            return this.drive_close(cx);
        }
        match this.io.as_mut() {
            None => Poll::Ready(Ok(())),
            Some(io) => Pin::new(io).poll_flush(cx),
        }
    }

    /// 先推完所有已接受字节（shutdown 不得截断半条记录），再推进
    /// flush → shutdown 收尾；收尾未完成时如实返回 `Pending`。
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.register_writer(cx);
        if let Some(kind) = this.fatal {
            return Poll::Ready(Err(fatal_error(kind)));
        }
        if let Some(err) = this.check_connection_deadline() {
            return Poll::Ready(Err(err));
        }
        if this.drive_tx(cx)?.is_pending() {
            return Poll::Pending;
        }
        if this.close_phase == ClosePhase::Open {
            this.close_phase = ClosePhase::Flushing;
        }
        this.drive_close(cx)
    }
}

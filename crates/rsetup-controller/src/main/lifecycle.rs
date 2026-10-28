//! 进程生命周期与停机监督（批次 C）。
//!
//! 包含关停信号监听、板端与 HTTP 生命周期统一监督、优雅停机与后台任务收敛逻辑。

use std::future::Future;
use std::time::Duration;

use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use rsetup_controller::transport::listener::ListenerError;

/// 板端监听关停的**外部**硬上界：监听内部已有自己的排空宽限期（`drain_grace` ≥ §6.6 的
/// 5s + 余量）与收尾 await，本常量只兜底「内部排空也卡住」的异常，绝不能小于内部宽限期。
pub(super) const BOARD_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

/// 关停信号：Ctrl-C 或 SIGTERM 任一到达即置真 shutdown watch（唯一的关停真源）。
///
/// 关停只做**本进程侧**的收敛：停止接新连接、请求已认证会话排空（h2 GOAWAY，板端按 §7
/// 重连到新进程）、取消后台任务。绝不重发任何已发出的变更，也不写任何持久决定。
pub(super) async fn shutdown_signal(shutdown: watch::Sender<bool>) {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        // `ctrl_c` 的产出是 `io::Result<()>`：忽略结果即可（任一信号到达都走同一条关停路径）。
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    eprintln!("controller 收到关停信号，开始优雅关停");
    let _ = shutdown.send(true);
}

// ---------------------------------------------------------------------------
// 顶层生命周期监督（审查 N1 / N2、RC2）：**生产 main 与测试共用同一实现**，不是在测试里复制逻辑。
//
// 关停宽限期只允许在**关停意图确立之后**武装（`supervise_shutdown` 的关停阶段）；把已经起表的
// `finish_board` future 在监听阶段交给监督并发 poll 就是 RC2 的真实缺陷（正常运行满宽限期即杀掉
// 健康的板端监听）。签名因此只收 `BoardTask` + `grace`，不收「已开始的结清 future」。
// ---------------------------------------------------------------------------

/// 装箱后的错误（`Send + 'static`，便于跨任务/监督边界传递；`main` 顶层再收敛为 `Box<dyn Error>`）。
pub(super) type BoxedError = Box<dyn std::error::Error + Send + 'static>;

/// 把具体错误装箱为 [`BoxedError`]。
pub(super) fn boxed<E>(error: E) -> BoxedError
where
    E: std::error::Error + Send + 'static,
{
    Box::new(error)
}

/// 板端监听任务的**所有权凭证**（生产由 `main` 构造，监督全程持有直到结清）。
///
/// 契约：监听任务体把最终结果写回 `done` 的**下一步就是结束**（见 `main` 的 spawn 站点），
/// 监督因此在结果到达后仍 `await` 原始句柄确认它真的退出（`JoinHandle` 被 `drop` 只会
/// detach，绝不等于「已退出」）。
pub(super) struct BoardTask {
    /// **原始**监听任务的句柄：结清时要么 join，要么 `abort` + `await` 确认。
    handle: JoinHandle<()>,
    /// 结果回执。监听阶段一旦被监督观察到就置 `None`（结果转入 `observed`），从结构上杜绝
    /// 「重复 poll 已完成的接收端」。
    done: Option<oneshot::Receiver<Result<(), ListenerError>>>,
    /// 监督在监听阶段观察到的**原始**回执（外层 `Err` ＝ 发送端被丢弃＝任务 panic/被取消）。
    observed: Option<BoardOutcome>,
}

/// 监听回执的原始形态（外层 `Err(RecvError)` ＝ 监听任务 panic / 被取消）。
pub(super) type BoardOutcome = Result<Result<(), ListenerError>, oneshot::error::RecvError>;

impl BoardTask {
    pub(super) fn new(
        handle: JoinHandle<()>,
        done: oneshot::Receiver<Result<(), ListenerError>>,
    ) -> Self {
        Self {
            handle,
            done: Some(done),
            observed: None,
        }
    }

    /// 监听阶段是否仍需观察这条任务（回执已被取走 ⇒ 不再观察）。
    fn watchable(&self) -> bool {
        self.done.is_some()
    }

    /// 监听阶段的观察分支：等**原始回执**，**不武装任何超时**。回执已被取走时永远 pending。
    async fn observe(&mut self) -> BoardOutcome {
        match self.done.as_mut() {
            Some(done) => done.await,
            None => std::future::pending::<BoardOutcome>().await,
        }
    }
}

/// [`BoardTask::observe`] 的 `Option` 包装，供 `select!` 直接使用。
///
/// `select!` 每轮都会**构造**所有分支的 future（只是不 poll 被禁用的分支），所以构造必须无
/// 副作用：这里对 `None` 只返回一个 pending future，绝不 unwrap。
async fn observe_board(board: Option<&mut BoardTask>) -> BoardOutcome {
    match board {
        Some(task) => task.observe().await,
        None => std::future::pending::<BoardOutcome>().await,
    }
}

/// 已武装时等完**剩余**宽限期；未武装（监听阶段）时永远 pending。
async fn sleep_or_pending(remaining: Option<Duration>) {
    if let Some(remaining) = remaining {
        tokio::time::sleep(remaining).await;
    }
}

/// `watch` 变 `true`（或发送端被丢弃）时结束。作为 HTTP `with_graceful_shutdown` 的驱动：让
/// 「外部关停信号」与「board 意外退出」两条关停来源**共用同一个优雅退出通道**（审查 N1）。
/// 发送端被关闭按「已关停」处理＝fail-closed，绝不永久挂起。
pub(super) async fn watch_turns_true(mut rx: watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        // 发送端被丢弃：没有任何一方会再置真，按「已关停」处理，绝不永久挂起。
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// 结清板端监听（审查 P1-3 / N1 / RC2）：`grace` 是**从关停意图确立时刻**算起的**剩余**宽限期，
/// 因此只允许在关停意图确立之后调用（[`supervise_shutdown`] 的关停阶段）；监听阶段调用它就是
/// RC2 根因。产出 `Some(err)` 交给顶层返回，`None` 表示正常结束。
pub(super) async fn finish_board(board: Option<BoardTask>, grace: Duration) -> Option<BoxedError> {
    // `?` 而不是 `let..else`：`None`＝「没有板端监听需要结清」，与产出类型同为 `Option`。
    let BoardTask {
        handle,
        done,
        observed,
    } = board?;
    // 回执已被监督取走（`observed`）⇒ **绝不**重复 poll。否则在剩余宽限期内等它**自己**跑完
    // GOAWAY 排空与收尾：`timeout` 先 poll 内部 future，所以宽限期剩 0 时刚到达的结果仍被采纳。
    let outcome = match (done, observed) {
        (Some(done), None) => match tokio::time::timeout(grace, done).await {
            Ok(outcome) => outcome,
            // 超期：内部排空本身也卡住，兜底 abort **原始任务** + await 确认退出。
            Err(_elapsed) => {
                handle.abort();
                let _ = handle.await;
                return Some(boxed(std::io::Error::other(
                    "板端接入监听未在剩余宽限期内完成排空（已 abort 原始任务并确认退出）",
                )));
            }
        },
        (None, Some(outcome)) => outcome,
        // 结构上不可达（`done`/`observed` 由监督成对维护）；真到此处按「回执不可用」失败关闭：
        // 结清原始任务后上抛错误，既不 panic 也不静默放行。
        (Some(_), Some(_)) | (None, None) => {
            handle.abort();
            let _ = handle.await;
            return Some(boxed(std::io::Error::other(
                "板端接入监听回执不可用（已 abort 原始任务并确认退出）",
            )));
        }
    };
    match outcome {
        // board 自己返回：`Err` 是失败，`Ok` 是收到关停后的正常收尾。结果已产出**仍要 join
        // 原始任务**（任务体在写回结果后立刻结束），drop 句柄只是 detach、不确认它已退出。
        Ok(result) => {
            let error = result.err().map(boxed);
            match handle.await {
                Ok(()) => error,
                Err(join) => error.or_else(|| {
                    Some(boxed(std::io::Error::other(format!(
                        "板端接入监听任务未正常退出（{join}）"
                    ))))
                }),
            }
        }
        // 发送端被丢弃：监听任务 panic 或被外部取消。句柄仍要结清（abort + await 确认）。
        Err(_recv) => {
            handle.abort();
            let _ = handle.await;
            Some(boxed(std::io::Error::other(
                "板端接入监听任务提前结束（panic 或被取消）",
            )))
        }
    }
}

/// N1 / RC2：把 HTTP 生命周期与板端监听放进**同一次受控监督**，两阶段收尾。
///
/// **监听阶段**（`armed == None`）只观察三路事件，**不武装任何关停超时**：
/// * HTTP 自身结束（正常关停或失败）；
/// * 板端监听的**原始回执**（`Err`、发送端被丢弃＝panic/取消、或未经请求的提前 `Ok`）——任一
///   结束都**立即**广播关停，驱动仍在跑的 HTTP 走优雅退出（HTTP 的 `with_graceful_shutdown`
///   由同一个 watch 驱动，见 [`watch_turns_true`]），绝不「静默半死」；
/// * OS 关停 watch 变 `true`（真实唯一的外部关停源；发送端被关闭按 fail-closed 关停处理）。
///
/// **关停阶段**：关停意图确立的**那一刻**才起表（RC2 根因正是旧接线在监听器启动时就把带宽限期
/// 的 [`finish_board`] future 交给本函数并发 poll，于是进程正常运行满 `BOARD_SHUTDOWN_GRACE`
/// 就 abort 掉完全健康的板端监听、广播关停、HTTP 拒连并非 0 退出）。board 侧在**剩余**宽限期内
/// 由 [`finish_board`] 结清，超期 `abort` **原始任务**并 `await` 确认。
///
/// 其余不变量：
/// * 宽限期锚在**关停意图**上而不是「HTTP 全部等完」：HTTP 仍有活跃长请求时，板端的有界清理
///   照常超期 abort，不被延后；
/// * HTTP 的原始 future 始终被 `Box::pin` **持有**，`select!` 只借用；板端结清后继续 await
///   同一个 future 直到收尾完成，绝不丢弃/detach 未收尾的 HTTP；
/// * 每个分支一旦观察完即被禁用（回执从 `BoardTask` 上摘走），绝不重复 poll 同一 future、
///   绝不双消费、绝不吞异常；即使「外部关停」与「board 结果」同轮 ready 也只消费一次；
/// * 板端提前 `Ok` 算不算「未经请求」，只在 board 完成分支**自己广播关停之前**取外部证据（已
///   武装，或外部关停 watch 的当前值已为 `true`）：`biased` 把该分支排在 watch 分支之前，同轮
///   ready 时 `armed` 尚未起表，用自己刚写进去的 `true` 判定会把正常的 SIGTERM 关停误记成失败；
///   反过来，也没有任何外部证据的提前 `Ok` 一律 fail-closed 上抛（见 `rc2_board_ok_*` 两条用例）；
/// * 没有板端监听（`None`）时路径同样正确：HTTP 结束或关停信号 → 广播 → 返回。
///
/// 返回 `(http 结果, board 错误)`；错误优先级由调用方决定（HTTP 自身失败优先，board 随后）。
pub(super) async fn supervise_shutdown<H>(
    http: H,
    mut board: Option<BoardTask>,
    grace: Duration,
    shutdown_tx: &watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> (Result<(), BoxedError>, Option<BoxedError>)
where
    H: Future<Output = Result<(), BoxedError>>,
{
    let mut http = Box::pin(http);
    // 关停意图确立的时刻：`None`＝仍在监听阶段，不武装 board 宽限期。进入时 watch 已为 `true`
    // （SIGTERM 落在绑端口与进入监督之间的真实竞态）即按「意图已确立」起表。
    let mut armed: Option<tokio::time::Instant> = if *shutdown_rx.borrow() {
        Some(tokio::time::Instant::now())
    } else {
        None
    };
    // HTTP 原始 future 的结果（`None`＝仍在途）。
    let mut http_result: Option<Result<(), BoxedError>> = None;
    // 板端在**任何关停意图之前**就自行返回 `Ok` 的 fail-closed 错误（监听已死但没有错误上抛）。
    let mut unprompted: Option<BoxedError> = None;

    loop {
        let board_watchable = board.as_ref().is_some_and(BoardTask::watchable);
        // 已武装 ⇒ 剩余宽限期；未武装 ⇒ `None`（监听阶段不设任何超时）。
        let drain_wait = armed.map(|at| {
            let elapsed = tokio::time::Instant::now()
                .checked_duration_since(at)
                .unwrap_or_default();
            grace.saturating_sub(elapsed)
        });
        tokio::select! {
            biased;
            result = http.as_mut(), if http_result.is_none() => {
                http_result = Some(result);
                // HTTP 自身结束（正常或失败）也要关停：驱动 board 与后台三任务退出。
                let _ = shutdown_tx.send(true);
                armed.get_or_insert_with(tokio::time::Instant::now);
            }
            outcome = observe_board(board.as_mut()), if board_watchable => {
                // 摘走回执并记下**原始**结果：之后这一分支被禁用，绝不再 poll 同一条 future。
                //
                // 「关停意图是否已确立」必须在**本分支广播关停之前**取证据，而且只认**本分支之外**
                // 的来源：本分支下一行的 `send(true)` 是**自己**广播的关停，若拿它之后的 watch 值来
                // 判定，就把真正未经请求的提前 `Ok` 也当成意图（fail-open）。两条外部证据：
                // ① 之前轮次已武装（HTTP 先结束，或 watch 分支已处理过关停）；
                // ② 进入本分支**此刻** watch 的当前值已为 `true`。`biased` 顺序把本分支排在 watch
                //    分支**之前**：外部关停（SIGTERM → 同一个 watch）与板端回执在同一轮 select 之前
                //    同时 ready 时，先执行的是这里，watch 分支还没机会起表，`armed` 仍是 `None`——
                //    这一轮读到的当前值就是那条缺失的外部证据（本轮 Low 的根因）。
                // 发送端被关闭的 fail-closed 约定只作用在 [`watch_turns_true`]：本函数按**引用**持有
                // 发送端（`&watch::Sender`），它在整个监督期间必然活着，`closed` 在此不可达，因此
                // 不把它列成证据来源（真到 `closed` 也只会落回 fail-closed 一侧）。
                let external_shutdown = armed.is_some() || *shutdown_rx.borrow();
                let _ = shutdown_tx.send(true);
                if let Some(task) = board.as_mut() {
                    task.done = None;
                    if !external_shutdown && matches!(outcome, Ok(Ok(()))) {
                        unprompted = Some(boxed(std::io::Error::other(
                            "板端接入监听在收到任何关停意图前提前结束（已广播关停）",
                        )));
                    }
                    task.observed = Some(outcome);
                }
                armed.get_or_insert_with(tokio::time::Instant::now);
            }
            _ = watch_turns_true(shutdown_rx.clone()), if armed.is_none() => {
                // OS 关停信号 ⇒ **此刻**才起表等 board 有界排空（监听阶段永不到点）。
                armed = Some(tokio::time::Instant::now());
            }
            _ = sleep_or_pending(drain_wait), if board_watchable && drain_wait.is_some() => {
                // 剩余宽限期到点而 board 仍在途：立刻去结清（abort 原任务 + await 确认），
                // 不因 HTTP 仍有活跃长请求而延后板端的有界清理。
                break;
            }
            else => break,
        }
    }

    // ===== 关停阶段 =====
    // 到这里关停意图必然已确立（HTTP 结束/失败、OS 关停信号、board 结束三者之一），宽限期从
    // **意图确立时刻**起算，这里取剩余量（HTTP 仍在收尾也照常倒计时）。
    let remaining = armed.map_or(grace, |at| {
        let elapsed = tokio::time::Instant::now()
            .checked_duration_since(at)
            .unwrap_or_default();
        grace.saturating_sub(elapsed)
    });
    let board_error = finish_board(board, remaining).await;
    // HTTP 原始 future 若仍在途，板端结清后仍要把它收尾完成：`http` 全程被 `Box::pin` 持有，
    // 这里继续 await **同一个** future，绝不 detach。
    let http_result = match http_result {
        Some(result) => result,
        None => http.as_mut().await,
    };
    (http_result, board_error.or(unprompted))
}

/// **生产生命周期接线**（`main` 与回归测试共用同一个函数）：HTTP serve future + 板端任务凭证 +
/// 关停 watch → 监督结果；`BOARD_SHUTDOWN_GRACE` 也只在这里绑定一次。
///
/// 之所以要有这层，而不是让测试各自拼 `supervise_shutdown`：上一轮独立复审的盲点正是「helper
/// 单测全绿，但 `main` 的接线仍是旧的」。接线唯一化后，谁把已起表的 `finish_board` future 塞回
/// 生产路径，直接跑本函数的 `rc2_*` 用例就会变红。
pub(super) async fn run_supervised_lifecycle<H>(
    http: H,
    board: Option<BoardTask>,
    shutdown_tx: &watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
) -> (Result<(), BoxedError>, Option<BoxedError>)
where
    H: Future<Output = Result<(), BoxedError>>,
{
    supervise_shutdown(http, board, BOARD_SHUTDOWN_GRACE, shutdown_tx, shutdown_rx).await
}

/// 后台任务收敛（审查 N2）：先给宽限期让它们**自己**退出（三者只等 watch，不做 I/O），超期则
/// **直接 abort 原始 `JoinHandle` 再 await 确认其真正结束**。
///
/// 关键：绝不把 owner 的 `JoinHandle` 塞进一个 wrapper 任务、再 abort wrapper——那只会 `drop`
/// 内层句柄＝detach 原任务，原任务从未被 abort、也没被 join（「wrapper 状态」不是「owner 状态」）。
pub(super) async fn settle_background_tasks(mut handles: Vec<JoinHandle<()>>, grace: Duration) {
    if handles.is_empty() {
        return;
    }
    // 宽限期内轮询**原始句柄**的完成状态：后台任务正常应随关停广播很快自行退出。
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        handles.retain(|handle| !handle.is_finished());
        if handles.is_empty() {
            return;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(10))).await;
    }
    // 超期仍在途：abort **原始句柄**并 await 确认退出（`abort` 本身不等待，必须再 await；await
    // 一个已 abort 的句柄会在其 future 真正被 drop 后返回 `Err(Cancelled)`，即原任务确已结束）。
    for handle in handles {
        if !handle.is_finished() {
            handle.abort();
        }
        let _ = handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;
    use std::time::Instant;
    use tokio::task::AbortHandle;

    // -----------------------------------------------------------------------
    // 审查 N1 / N2：生命周期监督 helper 的常驻回归
    //
    // 这些用例跑的是 **main 生产路径调用的同一组 helper**（`watch_turns_true` /
    // `finish_board` / `supervise_shutdown` / `settle_background_tasks`），不是在测试里
    // 复制一份同样的 select 逻辑。断言的是「原任务真的被 abort 并 await」「HTTP 这个
    // **永不自返**的 future 也被完整收尾」，而不是「wrapper 返回了」。
    // -----------------------------------------------------------------------

    /// 放入被测任务内部的「原任务已被 drop」见证：只有原 future 真正结束（正常收尾或被
    /// abort 后展开）才会置位；`JoinHandle` 被 drop（detach）不会让它结束。
    struct DropWitness(Arc<AtomicBool>);

    impl DropWitness {
        fn new() -> (Self, Arc<AtomicBool>) {
            let flag = Arc::new(AtomicBool::new(false));
            (Self(Arc::clone(&flag)), flag)
        }
    }

    impl Drop for DropWitness {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// 生产 HTTP 侧的形状：**除了关停广播之外没有任何返回路径**（`with_graceful_shutdown`
    /// 的驱动就是 [`watch_turns_true`]）。用它证明「board 意外退出」能驱动 HTTP 收尾。
    async fn http_that_never_returns_by_itself(
        shutdown: watch::Receiver<bool>,
        exited: Arc<AtomicBool>,
    ) -> Result<(), BoxedError> {
        watch_turns_true(shutdown).await;
        exited.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// 健康的板端监听：只等**同一个**关停 watch，收到后才收尾（生产 `run_controller_listener`
    /// 的关停入口形状：watch 变真 → GOAWAY 排空 → 返回 `Ok`）。
    async fn healthy_board_listener(shutdown: watch::Receiver<bool>) -> Result<(), ListenerError> {
        watch_turns_true(shutdown).await;
        Ok(())
    }

    /// 板端监听原任务的只读探针：`AbortHandle` 只用于观测（测试绝不 abort），外加两个见证——
    /// `finished`＝原任务**自己**跑到任务体结尾（被 abort 不会置位），`dropped`＝原 future 真的
    /// 被展开（abort 与正常结束都会置位；仅 drop `JoinHandle` 不会）。
    struct BoardProbe {
        abort: AbortHandle,
        finished: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
    }

    impl BoardProbe {
        /// 原任务仍在途（既没被 abort，也没自己跑完）。
        fn alive(&self) -> bool {
            !self.abort.is_finished() && !self.dropped.load(Ordering::SeqCst)
        }
    }

    /// 生产 `main` 的板端监听形状：真实 `tokio::spawn` 任务 + `done` 回执，监听体把结果写回
    /// `done` 的**下一步就是结束任务体**（生产同序，见 `BoardTask` 的契约）。
    fn spawn_board_task<F>(listener: F) -> (BoardTask, BoardProbe)
    where
        F: Future<Output = Result<(), ListenerError>> + Send + 'static,
    {
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            let _witness = witness;
            let outcome = listener.await;
            let _ = done_tx.send(outcome);
            finished_flag.store(true, Ordering::SeqCst);
        });
        let probe = BoardProbe {
            abort: handle.abort_handle(),
            finished,
            dropped,
        };
        (BoardTask::new(handle, done_rx), probe)
    }

    #[tokio::test]
    async fn n1_board_error_broadcasts_shutdown_and_finishes_the_http_that_never_returns() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        // board 侧：真实监听任务把失败写回 `done`（生产同一路径，错误经 `finish_board` 上交）。
        let (board_task, probe) =
            spawn_board_task(async { Err(ListenerError::Config("board listen failed".into())) });

        // HTTP 自身永不返回 ⇒ 若监督不广播关停，本用例只会超时失败（即「静默半死」）。
        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("board 结束后监督必须返回（HTTP 永不自行返回）");

        let error = board_error.expect("board 的失败必须原样交给顶层，不得只存在 done 回执里");
        assert!(
            error.to_string().contains("board listen failed"),
            "监听自身的错误要原样可见，实测 {error}"
        );
        assert!(http_result.is_ok(), "HTTP 走优雅退出，不是失败");
        assert!(
            http_exited.load(Ordering::SeqCst),
            "HTTP future 必须被**收尾完成**（select 丢弃未完成 future 也算「退出」，故断言其自身的完成标志）"
        );
        assert!(
            *shutdown_rx.borrow(),
            "board 意外退出必须把关停广播到同一个 watch（后台任务共用它）"
        );
        assert!(
            probe.finished.load(Ordering::SeqCst) && probe.dropped.load(Ordering::SeqCst),
            "原任务已自行返回结果，监督必须 join 它确认退出（不是留下 detach 的在途任务）"
        );
    }

    #[tokio::test]
    async fn n1_board_task_panic_is_observed_and_its_original_task_is_aborted() {
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        // 真实常驻任务：**永不返回**，只因为 `done_tx` 被丢弃（＝监听任务 panic/被取消）而
        // 让 done_rx 结束。句柄属于 `finish_board`，观测只经预先取得的 `AbortHandle`。
        let handle = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let abort_handle = handle.abort_handle();
        let task = BoardTask::new(handle, done_rx);
        drop(done_tx);

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            finish_board(Some(task), Duration::from_millis(200)),
        )
        .await
        .expect("done 结束即应返回，不等宽限期")
        .expect("panic/取消必须产出错误");
        assert!(!error.to_string().is_empty(), "错误文本去敏但必须存在");
        assert!(
            abort_handle.is_finished(),
            "原任务必须被 abort 并 await 确认，而不是留下在途任务"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "原 future 必须真的被展开（drop 见证置位）"
        );
    }

    #[tokio::test]
    async fn n1_board_task_overrunning_the_grace_is_aborted_not_detached() {
        let (_done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let handle = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let abort_handle = handle.abort_handle();
        let task = BoardTask::new(handle, done_rx);

        let error = tokio::time::timeout(
            Duration::from_secs(2),
            finish_board(Some(task), Duration::from_millis(50)),
        )
        .await
        .expect("宽限期到点必须返回")
        .expect("超期未排空必须产出错误");
        assert!(!error.to_string().is_empty());
        assert!(
            abort_handle.is_finished(),
            "超期兜底必须 abort **原始任务**并 await 确认退出"
        );
        assert!(dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        // `_done_tx` 在此随作用域结束被丢弃：监听仍在途时不得让 `finish_board` 永久挂起。
    }

    #[tokio::test]
    async fn n1_supervise_joins_both_sides_when_board_task_hangs_after_panic() {
        // 端到端（**生产同一条接线**）：board = 永不返回且回执被关闭（＝监听任务 panic/取消）的
        // 真实原任务，HTTP = 永不自返。监督必须即时广播关停、收尾 HTTP，并 abort **原始任务**后
        // 把 board 的错误交回顶层。
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let handle = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let abort_handle = handle.abort_handle();
        let board_task = BoardTask::new(handle, done_rx);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        drop(done_tx);

        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            supervise_shutdown(
                http,
                Some(board_task),
                Duration::from_millis(80),
                &shutdown_tx,
                shutdown_rx.clone(),
            ),
        )
        .await
        .expect("两侧都必须结清");
        assert!(board_error.is_some(), "board 的取消错误必须上交");
        assert!(http_result.is_ok());
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须收尾完成");
        assert!(
            abort_handle.is_finished(),
            "board 原任务必须被 abort 并 await 确认（回执已关闭 ⇒ 不必等满宽限期）"
        );
        assert!(dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        assert!(*shutdown_rx.borrow(), "关停必须已广播给后台任务");
    }

    #[tokio::test]
    async fn n2_settle_aborts_the_original_owner_tasks_not_a_wrapper() {
        // ① 会自己退出的任务：宽限期内正常收尾，不得被 abort。
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let graceful_exited = Arc::new(AtomicBool::new(false));
        let graceful_flag = Arc::clone(&graceful_exited);
        let graceful = tokio::spawn(async move {
            let mut rx = shutdown_rx;
            while !*rx.borrow_and_update() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
            graceful_flag.store(true, Ordering::SeqCst);
        });

        // ② **不响应关停**的真实 pending 任务：只有 abort 原任务才会结束它。
        let (witness, dropped) = DropWitness::new();
        let stubborn = tokio::spawn(async move {
            let _witness = witness;
            std::future::pending::<()>().await;
        });
        let stubborn_abort = stubborn.abort_handle();
        // 生产的先后顺序：关停广播**已经**由 `supervise_shutdown` 发出，`settle` 只负责结清。
        let _ = shutdown_tx.send(true);

        let started = Instant::now();
        tokio::time::timeout(
            Duration::from_secs(5),
            settle_background_tasks(vec![graceful, stubborn], Duration::from_millis(120)),
        )
        .await
        .expect("收敛必须有界");
        let elapsed = started.elapsed();

        assert!(
            graceful_exited.load(Ordering::SeqCst),
            "会自行退出的任务必须在宽限期内正常收尾"
        );
        assert!(
            stubborn_abort.is_finished(),
            "不响应关停的原任务必须在超期后被 abort 并 await 确认（不是 wrapper 状态）"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "原 future 必须被展开；仅 drop `JoinHandle`（detach）永远不会置位"
        );
        assert!(
            elapsed >= Duration::from_millis(100),
            "②必须真的用满宽限期（实测 {elapsed:?}）"
        );
    }

    // -----------------------------------------------------------------------
    // RC2：单次生产生命周期的监听期（真实进程 b52 复现的根因）
    //
    // 根因口径：生产 main 在**监听器刚启动**时就把 `finish_board(Some(task), BOARD_SHUTDOWN_GRACE)`
    // 这个 future 交给 `supervise_shutdown` 并发 poll，于是宽限期计时器在监听阶段就 armed；
    // 进程正常运行满 30s（无任何 SIGTERM、board 监听完全健康）即 abort 原任务 → 广播关停 →
    // HTTP 拒连 → 非 0 退出。下面这组用例跑的是**生产同一条接线**（同一个
    // `supervise_shutdown` + 同一个 `BOARD_SHUTDOWN_GRACE` 常量 + 真实 `tokio::spawn` 监听任务），
    // 不是在测试里复制一份 select。
    // -----------------------------------------------------------------------

    /// 把生产监督跑到后台任务里（虚拟时钟用例共用）：参数与 `main` 的调用**逐位一致**。
    fn spawn_supervisor<H>(
        http: H,
        board: Option<BoardTask>,
        shutdown_tx: &watch::Sender<bool>,
        shutdown_rx: &watch::Receiver<bool>,
    ) -> JoinHandle<(Result<(), BoxedError>, Option<BoxedError>)>
    where
        H: Future<Output = Result<(), BoxedError>> + Send + 'static,
    {
        let tx = shutdown_tx.clone();
        let rx = shutdown_rx.clone();
        // 与 `main` **同一个函数、同一份宽限期常量**：测试不另选 grace，也不复制接线。
        tokio::spawn(async move { run_supervised_lifecycle(http, board, &tx, rx).await })
    }

    /// 让已 spawn 的监督/监听任务真的被 poll（在虚拟时钟上建立它自己的等待或计时器）。
    async fn drive(rounds: usize) {
        for _ in 0..rounds {
            tokio::task::yield_now().await;
        }
    }

    /// **RC2 主用例**：监听阶段推进 **2× 生产宽限期**且不发任何关停信号 ⇒ 监督必须仍在监听：
    /// board 原任务在途、HTTP 未收尾、关停 watch 仍为 `false`。
    ///
    /// 旧接线（监听器启动即把带宽限期的 `finish_board` future 交给监督并发 poll）在本用例上 **RED**：
    /// 虚拟时间一到 30s 就 abort 健康监听 → 广播关停 → HTTP 收尾 → 监督返回。
    #[tokio::test(start_paused = true)]
    async fn rc2_listening_phase_does_not_arm_the_board_grace() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        tokio::time::advance(BOARD_SHUTDOWN_GRACE * 2).await;
        drive(4).await;

        let facts = format!(
            "board 原任务在途={} HTTP 已收尾={} 关停已广播={} 监督已返回={}",
            probe.alive(),
            http_exited.load(Ordering::SeqCst),
            *shutdown_rx.borrow(),
            supervisor.is_finished(),
        );
        assert!(
            probe.alive(),
            "监听期（无任何关停意图）绝不得 abort 仍然健康的板端监听原任务；实测 {facts}"
        );
        assert!(
            !http_exited.load(Ordering::SeqCst),
            "监听期绝不得关闭 HTTP（正常服务 2× 宽限期后拒连就是 RC2）；实测 {facts}"
        );
        assert!(
            !*shutdown_rx.borrow(),
            "监听期绝不得广播关停（后台三任务与 board 共用这同一个 watch）；实测 {facts}"
        );
        assert!(
            !supervisor.is_finished(),
            "没有任何关停意图时监督必须一直停在监听阶段；实测 {facts}"
        );
    }

    /// 关停意图确立的**那一刻**才起表：监听期推进 2× 宽限期无动作，`send(true)` 之后健康监听在
    /// **远小于**宽限期的时间内自行排空收尾，两侧都以「无错误」结束（RC2 之后的正常关停）。
    #[tokio::test(start_paused = true)]
    async fn rc2_shutdown_intent_arms_the_grace_and_a_healthy_board_drains_by_itself() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        tokio::time::advance(BOARD_SHUTDOWN_GRACE * 2).await;
        drive(4).await;
        assert!(
            probe.alive() && !supervisor.is_finished(),
            "关停意图确立前不得动 board，也不得结束监督"
        );

        // 生产里这一步是 SIGTERM → 同一个 watch；**此刻**宽限期才起表。
        let armed_at = tokio::time::Instant::now();
        let _ = shutdown_tx.send(true);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("关停后必须在宽限期内结清（两侧都 join）")
            .expect("监督任务不得 panic");
        let elapsed = tokio::time::Instant::now()
            .checked_duration_since(armed_at)
            .unwrap_or_default();

        assert!(http_result.is_ok(), "HTTP 走优雅退出，不是失败");
        assert!(board_error.is_none(), "健康监听自行排空收尾不得被记成失败");
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 原任务必须**自行**收尾（被 abort 不会置位这个见证）"
        );
        assert!(probe.dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        assert!(
            probe.abort.is_finished(),
            "监督返回时原任务必须确已终止；自行结束还是被 abort 由上面的 `finished` 见证区分"
        );
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须收尾完成");
        assert!(
            elapsed < BOARD_SHUTDOWN_GRACE,
            "关停后应在远小于宽限期内完成（实测 {elapsed:?}）"
        );
    }

    /// 宽限期锚在**关停意图**而不是「HTTP 全部等完」：关停后 HTTP 仍在处理活跃长请求时，板端的
    /// 有界清理到期即 **abort 原始任务**（drop 见证置位＝不是 detach），随后 HTTP 仍被收尾完成。
    #[tokio::test(start_paused = true)]
    async fn rc2_grace_is_anchored_at_shutdown_intent_not_at_http_completion() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        // board：完全不响应关停（既不返回也不结束任务体）＝内部排空卡死。
        let (board_task, probe) =
            spawn_board_task(std::future::pending::<Result<(), ListenerError>>());
        // HTTP：关停广播后仍在处理活跃长请求，只有 release 结束才收尾（生产的活跃长连接形状）。
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let http_exited = Arc::new(AtomicBool::new(false));
        let http_flag = Arc::clone(&http_exited);
        let http = async move {
            let _ = release_rx.await;
            http_flag.store(true, Ordering::SeqCst);
            Ok(())
        };
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        let _ = shutdown_tx.send(true);
        drive(4).await;
        // 越过宽限期：板端必须有界清理，HTTP 的长请求不得把它延后。
        tokio::time::advance(BOARD_SHUTDOWN_GRACE + Duration::from_millis(10)).await;
        drive(8).await;

        let facts = format!(
            "board 原 future 已展开={} board 自行收尾={} HTTP 已收尾={} 监督已返回={}",
            probe.dropped.load(Ordering::SeqCst),
            probe.finished.load(Ordering::SeqCst),
            http_exited.load(Ordering::SeqCst),
            supervisor.is_finished(),
        );
        assert!(
            probe.dropped.load(Ordering::SeqCst),
            "关停意图后越过宽限期必须 abort **原始** board 任务并 await 确认（detach 不会展开原 future）；实测 {facts}"
        );
        assert!(
            !probe.finished.load(Ordering::SeqCst),
            "这条路径是 abort 退出，不得被记成自行收尾；实测 {facts}"
        );
        assert!(
            !http_exited.load(Ordering::SeqCst),
            "本用例前提：HTTP 仍在活跃长请求中（宽限期不得锚在它上面）；实测 {facts}"
        );

        // 放行 HTTP：监督必须收尾返回，并把板端超期的错误交回顶层。
        drop(release_tx);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("HTTP 收尾后监督必须返回（HTTP 原 future 必须持有到收尾，不得 detach）")
            .expect("监督任务不得 panic");
        assert!(http_result.is_ok(), "HTTP 走的是优雅退出，不是失败");
        assert!(
            board_error.is_some(),
            "板端排空超期的错误必须上交，不得静默成功（fail-open）"
        );
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须被收尾完成");
    }

    /// 没有板端监听（`TransportEnv::Disabled`）：监听期同样不得自行结束；关停信号到来才收尾，
    /// 且不得凭空产出 board 错误。
    #[tokio::test(start_paused = true)]
    async fn rc2_without_board_task_the_supervisor_still_shuts_down_cleanly() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, None, &shutdown_tx, &shutdown_rx);
        drive(4).await;
        tokio::time::advance(BOARD_SHUTDOWN_GRACE * 2).await;
        drive(4).await;
        assert!(
            !supervisor.is_finished() && !*shutdown_rx.borrow(),
            "无 board 时监听期同样没有任何超时可武装，监督必须一直在监听"
        );

        let _ = shutdown_tx.send(true);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("关停后必须返回")
            .expect("监督任务不得 panic");
        assert!(http_result.is_ok(), "HTTP 优雅退出");
        assert!(board_error.is_none(), "没有板端监听不得凭空产出错误");
        assert!(http_exited.load(Ordering::SeqCst), "HTTP 必须收尾完成");
    }

    /// 关停意图在进入监督**之前**就已确立（SIGTERM 落在绑端口与进入监督之间的真实竞态）：立刻
    /// 起表、正常收尾，不静默等满 30s，也不把「监听自行返回 `Ok`」误记成未经请求的提前结束。
    #[tokio::test(start_paused = true)]
    async fn rc2_shutdown_already_established_before_supervise_starts_arms_immediately() {
        let (shutdown_tx, shutdown_rx) = watch::channel(true);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let started = tokio::time::Instant::now();
        let (http_result, board_error) = tokio::time::timeout(
            BOARD_SHUTDOWN_GRACE,
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("关停已确立 ⇒ 必须立刻收尾，不等满宽限期");
        let elapsed = tokio::time::Instant::now()
            .checked_duration_since(started)
            .unwrap_or_default();

        assert!(http_result.is_ok());
        assert!(board_error.is_none(), "已确立关停后的正常收尾不得产出错误");
        assert!(probe.finished.load(Ordering::SeqCst), "board 必须自行收尾");
        assert!(http_exited.load(Ordering::SeqCst));
        assert!(
            elapsed < BOARD_SHUTDOWN_GRACE,
            "关停已确立时不得静默等满宽限期（实测 {elapsed:?}）"
        );
    }

    /// 外部关停与 board 失败**同一轮** ready：只消费一次、不重复 poll、错误不被吞。
    #[tokio::test(start_paused = true)]
    async fn rc2_shutdown_and_board_failure_in_the_same_round_are_consumed_once() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) =
            spawn_board_task(async { Err(ListenerError::Config("board listen failed".into())) });
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));
        let supervisor = spawn_supervisor(http, Some(board_task), &shutdown_tx, &shutdown_rx);
        drive(4).await;
        // 两条关停来源同时到位（board 的失败在上一轮就已 ready）。
        let _ = shutdown_tx.send(true);
        let (http_result, board_error) = tokio::time::timeout(BOARD_SHUTDOWN_GRACE, supervisor)
            .await
            .expect("监督必须返回（不得重复 poll 已完成的分支而永久挂起）")
            .expect("监督任务不得 panic");

        let error = board_error.expect("board 的失败必须原样上交，不得被同轮关停信号吞掉");
        assert!(error.to_string().contains("board listen failed"), "{error}");
        assert!(http_result.is_ok(), "HTTP 走优雅退出");
        assert!(http_exited.load(Ordering::SeqCst));
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 原任务必须被 join"
        );
    }

    /// 监听在**任何关停意图之前**就自行返回 `Ok`：监听已死但没有错误上抛，必须 fail-closed 上抛
    /// （不再静默 exit 0），同时仍立即广播关停让 HTTP 收尾。
    #[tokio::test]
    async fn rc2_board_ok_without_any_shutdown_request_is_reported_not_swallowed() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(async { Ok(()) });
        let http_exited = Arc::new(AtomicBool::new(false));
        let http = http_that_never_returns_by_itself(shutdown_rx.clone(), Arc::clone(&http_exited));

        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("board 提前结束必须驱动监督返回");

        assert!(
            board_error.is_some(),
            "未经请求的提前结束必须 fail-closed 上抛，不得静默 exit 0"
        );
        assert!(http_result.is_ok(), "HTTP 走优雅退出");
        assert!(
            *shutdown_rx.borrow(),
            "仍要立即广播关停：HTTP 与后台三任务不得对着已死的监听继续服务"
        );
        assert!(http_exited.load(Ordering::SeqCst));
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 原任务必须被 join"
        );
    }

    // -----------------------------------------------------------------------
    // RC2-Low：board 完成分支排在 watch 分支**之前**造成的同轮竞态
    //
    // 窗口（`tokio::select! { biased; .. }`，分支顺序 HTTP → board 完成 → watch → 宽限期）：
    // SIGTERM 把外部关停 watch 置真与板端提前 `Ok` 在**同一次 select 之前**同时到位，而 HTTP 因
    // 活跃长请求仍 pending（HTTP 分支不 ready，不会先替 `armed` 起表）时，`biased` 顺序下先执行
    // 的是 board 完成分支：它**自己** `send(true)` 之后只看 `armed.is_none()`，而 watch 分支还没
    // 机会起表 ⇒ 把「外部早已确立的关停意图」当成未经请求的提前结束 ⇒ 顶层拿着一条本该是正常
    // 优雅关停的路径以非 0 退出。
    //
    // 这组用例把外部事件**精确安排在监督的两次 poll 之间**（手动 poll，不交给调度器排序，也不
    // 复制 select、不用 sleep 碰时序）；被测对象仍是生产 `run_supervised_lifecycle`，宽限期由
    // 生产 `BOARD_SHUTDOWN_GRACE` 决定。
    // -----------------------------------------------------------------------

    /// **手动 poll 一次**（不经调度器）：用于把外部事件精确安排在监督两次 poll 之间，让这条
    /// 「同轮两个分支同时 ready」的竞态可确定性复现，而不是靠任务先后顺序赌一次。
    /// 唤醒走 `Waker::noop()`：监督只在被 poll 时才前进，唤醒本身没有意义。
    fn poll_once<F>(future: &mut F) -> Poll<F::Output>
    where
        F: Future + Unpin,
    {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        std::pin::Pin::new(future).poll(&mut cx)
    }

    /// 手动 poll 到就绪：监督挂在**别的真实任务**（板端原任务的收尾、对它的 join）上时让出一次，
    /// 让 runtime 去 poll 那个任务，然后再继续手动驱动监督。
    async fn poll_until_ready<F>(future: &mut F) -> F::Output
    where
        F: Future + Unpin,
    {
        loop {
            if let Poll::Ready(output) = poll_once(future) {
                return output;
            }
            tokio::task::yield_now().await;
        }
    }

    /// 与 `spawn_board_task` **同形状**的板端原任务，区别只有「写回 `done`」这一步由测试执行：
    /// 真实 `tokio::spawn` 原任务、只等收尾门、放行后**立刻**结束任务体（＝生产契约「写回结果的
    /// 下一步就是结束」），`finish_board` 照旧 join 这个真实任务。回执交给测试是为了把「回执
    /// 已 ready」与「外部关停已置真」钉进监督同两次 poll 之间的同一段同步代码里。
    struct RacyBoardFixture {
        task: BoardTask,
        probe: BoardProbe,
        report: oneshot::Sender<Result<(), ListenerError>>,
        finish: oneshot::Sender<()>,
    }

    fn spawn_racy_board_task() -> RacyBoardFixture {
        let (report, done_rx) = oneshot::channel();
        let (finish, finish_rx) = oneshot::channel::<()>();
        let (witness, dropped) = DropWitness::new();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            let _witness = witness;
            let _ = finish_rx.await;
            finished_flag.store(true, Ordering::SeqCst);
        });
        let probe = BoardProbe {
            abort: handle.abort_handle(),
            finished,
            dropped,
        };
        RacyBoardFixture {
            task: BoardTask::new(handle, done_rx),
            probe,
            report,
            finish,
        }
    }

    /// **本竞态主用例**：监督已在监听阶段 poll 过 ⇒ 两次 poll 之间「外部关停 watch 先为真、随后
    /// 板端提前 `Ok`」⇒ 收尾必须 `(Ok, None)`：外部意图已在板端回执之前确立，健康监听的收尾不得
    /// 记成未经请求的提前结束（修复前这里产出 `Some(..)`，顶层据此非 0 退出 ⇒ RED）。
    #[tokio::test(start_paused = true)]
    async fn rc2_external_shutdown_arriving_before_the_board_receipt_is_not_unprompted() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let board = spawn_racy_board_task();
        // 人为的活跃长请求 HTTP：除了放行之外没有任何返回路径，因而**不** ready（否则 `biased`
        // 顺序下 HTTP 分支会先给 `armed` 起表，本竞态就不成立）。
        let (http_release, http_release_rx) = oneshot::channel::<()>();
        let http_released = Arc::new(AtomicBool::new(false));
        let http_flag = Arc::clone(&http_released);
        let http = async move {
            let _ = http_release_rx.await;
            http_flag.store(true, Ordering::SeqCst);
            Ok(())
        };
        tokio::pin!(http);
        let mut supervised = Box::pin(run_supervised_lifecycle(
            http,
            Some(board.task),
            &shutdown_tx,
            shutdown_rx.clone(),
        ));

        // ① 监督先在监听阶段 poll 过：不起表、不广播、板端原任务仍在途。
        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：监督必须仍在监听阶段挂起"
        );
        assert!(!*shutdown_rx.borrow(), "前提：监听阶段绝不得广播关停");
        assert!(board.probe.alive(), "前提：板端原任务必须还在途");

        // ② **两次 poll 之间**：外部关停先到（生产＝SIGTERM → 同一个 watch），**随后**板端提前 `Ok`。
        // 两者都先于下一次 poll 成立 ⇒ 同一次 select 里 board 完成分支与 watch 分支同时 ready，
        // 而 `biased` 顺序下先执行的是 board 完成分支。
        let _ = shutdown_tx.send(true);
        board.report.send(Ok(())).expect("板端回执必须写回成功");

        // ③ 下一次 poll（旧代码在这里把外部已确立的关停误记成「未经请求的提前结束」）。
        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：HTTP 仍在活跃长请求中，监督不得已返回"
        );

        // ④ 收尾：放行板端原任务的收尾与 HTTP 的活跃长请求。
        drop(board.finish);
        drop(http_release);
        let (http_result, board_error) = poll_until_ready(&mut supervised).await;

        let facts = format!(
            "board 错误={} HTTP 结果={} 板端自行收尾={} 板端原 future 已展开={} HTTP 已收尾={}",
            board_error
                .as_ref()
                .map(|error| error.to_string())
                .unwrap_or_else(|| "无".to_owned()),
            http_result.is_ok(),
            board.probe.finished.load(Ordering::SeqCst),
            board.probe.dropped.load(Ordering::SeqCst),
            http_released.load(Ordering::SeqCst),
        );
        assert!(
            http_result.is_ok(),
            "HTTP 走的是优雅退出，不是失败；实测 {facts}"
        );
        assert!(
            board_error.is_none(),
            "关停意图在板端回执**之前**就已由外部确立（同一个 watch 已为真），健康监听随后的收尾不得记成未经请求的提前结束（那会让正常的 SIGTERM 关停非 0 退出）；实测 {facts}"
        );
        assert!(
            board.probe.finished.load(Ordering::SeqCst),
            "板端原任务必须**自行**收尾（被 abort 不会置位这个见证）；实测 {facts}"
        );
        assert!(
            board.probe.dropped.load(Ordering::SeqCst),
            "板端原 future 必须被展开；实测 {facts}"
        );
        assert!(
            board.probe.abort.is_finished(),
            "监督返回时板端原任务必须确已终止"
        );
        assert!(
            http_released.load(Ordering::SeqCst),
            "HTTP 必须被收尾完成（不是 detach 掉未收尾的 future）"
        );
        assert!(
            *shutdown_rx.borrow(),
            "关停仍必须广播给后台三任务（共用同一个 watch）"
        );
    }

    /// **反向用例（同一驱动机制）**：两次 poll 之间只有板端提前 `Ok`、没有任何外部关停证据
    /// （watch 仍 `false`、`armed` 仍 `None`）⇒ 仍必须 fail-closed 上抛。它钉住修复的边界：
    /// board 完成分支**自己**广播的那次 `send(true)` 不能自证关停意图，否则真正的「监听已死却
    /// 无错误上抛」会被放行（fail-open）。
    #[tokio::test(start_paused = true)]
    async fn rc2_board_ok_between_two_polls_is_still_fail_closed_without_external_evidence() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let board = spawn_racy_board_task();
        // 人为的活跃长请求 HTTP：除了放行之外没有任何返回路径，因而**不** ready（否则 `biased`
        // 顺序下 HTTP 分支会先给 `armed` 起表，本竞态就不成立）。
        let (http_release, http_release_rx) = oneshot::channel::<()>();
        let http_released = Arc::new(AtomicBool::new(false));
        let http_flag = Arc::clone(&http_released);
        let http = async move {
            let _ = http_release_rx.await;
            http_flag.store(true, Ordering::SeqCst);
            Ok(())
        };
        tokio::pin!(http);
        let mut supervised = Box::pin(run_supervised_lifecycle(
            http,
            Some(board.task),
            &shutdown_tx,
            shutdown_rx.clone(),
        ));

        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：监督必须仍在监听阶段挂起"
        );
        // 两次 poll 之间**只有**板端提前 `Ok`：没有任何外部关停证据。
        board.report.send(Ok(())).expect("板端回执必须写回成功");
        assert!(
            poll_once(&mut supervised).is_pending(),
            "前提：HTTP 仍在活跃长请求中，监督不得已返回"
        );

        drop(board.finish);
        drop(http_release);
        let (http_result, board_error) = poll_until_ready(&mut supervised).await;

        let error = board_error.expect("没有任何外部关停证据的提前 `Ok` 必须 fail-closed 上抛");
        assert!(
            error.to_string().contains("关停意图前"),
            "错误必须如实说明是未经请求的提前结束，实测 {error}"
        );
        assert!(http_result.is_ok(), "HTTP 走优雅退出");
        assert!(
            http_released.load(Ordering::SeqCst),
            "HTTP 必须被收尾完成（select 丢弃未收尾 future 也算失败）"
        );
        assert!(
            *shutdown_rx.borrow(),
            "仍要立即广播关停：HTTP 与后台三任务不得对着已死的监听继续服务"
        );
    }

    /// HTTP 自身失败（`axum::serve` 返回 `Err`）：必须广播关停，board 侧仍自行排空收尾，
    /// 顶层优先拿到 HTTP 的错误。
    #[tokio::test]
    async fn rc2_http_failure_broadcasts_shutdown_and_board_drains_by_itself() {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (board_task, probe) = spawn_board_task(healthy_board_listener(shutdown_rx.clone()));
        let http = async { Err::<(), BoxedError>(boxed(std::io::Error::other("serve failed"))) };

        let (http_result, board_error) = tokio::time::timeout(
            Duration::from_secs(5),
            run_supervised_lifecycle(http, Some(board_task), &shutdown_tx, shutdown_rx.clone()),
        )
        .await
        .expect("HTTP 失败后监督必须返回");

        assert!(http_result.is_err(), "HTTP 自身的失败要交回顶层");
        assert!(
            board_error.is_none(),
            "board 是收到关停后自行收尾，不得记成失败"
        );
        assert!(
            *shutdown_rx.borrow(),
            "HTTP 失败必须广播关停（board 与后台三任务共用同一个 watch）"
        );
        assert!(
            probe.finished.load(Ordering::SeqCst),
            "board 必须自行排空收尾（不是被 abort）"
        );
    }

    /// `finish_board` 拿到结果后仍要 **join 原始任务**：结果写回后任务体还在收尾时，结清必须等
    /// 它真正结束（旧实现直接返回并把句柄 drop＝detach，在本用例上是 RED）。
    #[tokio::test(start_paused = true)]
    async fn rc2_finish_board_joins_the_original_task_after_its_result_arrives() {
        let (done_tx, done_rx) = oneshot::channel();
        let (witness, dropped) = DropWitness::new();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_flag = Arc::clone(&finished);
        let handle = tokio::spawn(async move {
            let _witness = witness;
            // 结果已产出（生产里 send 就是任务体最后一步，这里刻意延长收尾以便观测所有权）。
            let _ = done_tx.send(Ok(()));
            tokio::time::sleep(Duration::from_millis(50)).await;
            finished_flag.store(true, Ordering::SeqCst);
        });
        let abort = handle.abort_handle();

        let no_error = tokio::time::timeout(
            Duration::from_secs(5),
            finish_board(
                Some(BoardTask::new(handle, done_rx)),
                Duration::from_secs(30),
            ),
        )
        .await
        .expect("结果已产出 ⇒ 结清不得等满宽限期")
        .is_none();
        assert!(no_error, "正常收尾不得产出错误");
        assert!(
            finished.load(Ordering::SeqCst),
            "原任务必须被 **join** 到真正结束；只拿结果就返回＝drop 句柄 detach"
        );
        assert!(dropped.load(Ordering::SeqCst), "原 future 必须被展开");
        assert!(
            abort.is_finished(),
            "结清返回时原任务必须确已终止（自行跑完，而不是留下 detach 的在途任务）"
        );
    }

    #[tokio::test]
    async fn n2_settle_returns_early_when_every_owner_exits_by_itself() {
        let handles = (0..3)
            .map(|_| {
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                })
            })
            .collect();
        let started = Instant::now();
        settle_background_tasks(handles, Duration::from_secs(30)).await;
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(5),
            "全部 owner 自行退出时不得等满 30s 宽限期（实测 {elapsed:?}）"
        );
    }
}

# session driver（Task4）阶段报告

范围：新增 `crates/rsetup-tunnel/src/session/{config.rs,handle.rs,driver.rs}`、`crates/rsetup-tunnel/tests/session_driver.rs`，并在 `src/session/mod.rs` 登记模块与re-export。未提交、未推送，未触碰 `session/{pending,probe,types,streams}.rs`、crypto/handshake、`h2_keepalive.rs`、根 `lib.rs` 及 controller/board。规模：driver 1709 行、handle 609 行、config 152 行、集成测试 1745 行（22 个用例）。

## 验证命令与结果（本机实跑）

- `CARGO_TARGET_DIR=target/tunnel-session-driver cargo test -p rsetup-tunnel --test session_driver --locked -- --test-threads=1` → 22 passed / 0 failed。
- `cargo test -p rsetup-tunnel --locked` → exit 0，15 个 target 全绿，合计 286 个用例（含并行 writer 的 session_state/session_streams/h2_keepalive/secure_io 等，确认 `session/mod.rs` 登记未破坏邻居）。
- `cargo fmt --package rsetup-tunnel -- --check` → exit 0（按本仓包级口径，未使用 `--all`）。
- `cargo clippy -p rsetup-tunnel --all-targets --locked -- -D warnings` → exit 0（lib + 全部 test target，含本任务 span）。

真实 controller/board 双进程、真实 TCP socket、RSS 内存实测**未做**：本任务边界内只有同进程的 `SecureIo<DuplexStream>` 与真实 h2。不得据 22 个用例宣称生产完成。

## 架构（为什么是这样）

- 单一 owner task 持有全部协议状态（pending/probe/streams/drain/system），**永不 inline poll h2**；handler 通过 `JoinSet` + 并发预算（≤16）异步执行，owner 只从 `done_rx` 收割。
- 连接 task 拥有并 poll h2 `Connection`（client 是 Future，server 用 `poll_accept`+`poll_closed`）；连接结束后 **park**（等 owner 退出）而不是释放 io，否则 ≥5s 排空会被 h2 提前结束掐断。这是评审纠正「h2 GOAWAY 后无法完成响应」误结论的落点：只要连接 task 持续被 poll，GOAWAY 就能真正写上线。
- 每代流（epoch）一对 reader/writer task；teardown 只 abort 本代并保留句柄用于回收确认。
- 每代流的命令通道分两条：control 容量 2、business 容量 `business_queue` 且共享 KiB 级字节预算（`ByteBudget` 用 `try_reserve`/Drop 归还），业务写阻塞不会挡住控制流。
- 关停意图走独立 `watch::Sender<ControlIntent>`（`None<Shutdown<Abort` 单调取强），与业务队列余量无关；owner 的 `select!` 有 `intent_rx.changed()` 分支，且 `pre_work` 每轮第一件事就是读意图。
- 状态只由 owner 发布：`Ready` 必须两侧各自在 Control+Data 上首次探测成功；server 侧不主动重开，只 RST 并等 client 重开；`ProbeDecision::CloseConnection` → `SessionCloseReason::StreamFlapping`。

## 稳定 API（供 manager/Task5/6 使用）

- `start_client_session(io: SecureIo<S>, handler: Arc<dyn SessionHandler>, config: SessionConfig) -> Result<(SessionHandle, JoinHandle<SessionOutcome>), SessionError>`；`start_server_session` 同形。io 交出去之前先 `control_handle()`。
- `SessionHandle`（`Clone + Send + Sync`）：`request(stream, action, payload) -> Result<Reply, RequestError>`；`send_event(stream, action, payload) -> Result<(), RequestError>`；`send_system(SystemSignal) -> Result<(), RequestError>`；`wait_ready()`；`graceful_shutdown()`；`abort()`；`subscribe_state() -> watch::Receiver<SessionState>`。
- `send_event`：TYPE_EVENT，自带非零 trace，**不建未决项、不等响应、不自动重放**，因此不占 `max_pending`（`max_pending=2` 时 `request()` 已 Busy，Event 仍可发，已测）；与业务共用同一字节预算与有界队列；排空后返回 `Closed`。
- `send_system`：只允许 Control，载荷必须为空，未知 action → `ProtocolMismatch`；走控制小通道（业务写阻塞时仍能上线）；成功=已入队（不谎称已送达对端）；不周期发探测。
- `SessionHandler`（dyn-safe，`HandlerFuture = Pin<Box<dyn Future<Output=Reply>+Send+'static>>`）：`handle`（必须 spawn，不许 inline await）+ 默认空实现 `on_event/on_system/on_ready/on_closed`；`into_shared`/`into_dyn` 辅助。
- `SessionOutcome { reason, probes_served, probe_missions, handler_panics, dropped_events, cleaned }`；`cleaned` 为真表示 handler 任务、reader/writer、连接 task 全部确认退出且 io 引用释放（测试用 `Arc::strong_count` 断言 socket 已无人持有）。
- `SessionConfig::validate()` 硬门禁：`ordinary_timeout ∈ (0,15s]`、`max_pending ∈ [2,1024]`、`business_queue ∈ [1,32]`、`business_queue_bytes ∈ [512KiB,64MiB]`、`handler_concurrency ∈ [1,16]`、`drain_timeout ≥ 5s`、`event_queue ∈ [1,1024]`、`notify_queue ∈ [1,64]`；越界一律 `InvalidConfig`，不做静默收窄。

## 22 个用例覆盖的真实事实（不是 mock 路由）

配置门禁；双向前置探测→Ready 且 `on_closed` 恰好一次；双向 RPC + 32KiB 窗口下的 32KiB 级信封流控；超限包被拒且会话不受影响；未决预算预留 2 个探测名额；handler 预算耗尽直接回 1002 过载（不无界排队）且被挂住的调用方仍按时限结清；写侧阻塞时超时仍从**入队时刻**起算（在调用任务内测量，落在 [5s,5.2s)）；中途 GOAWAY 期间新请求被拒、对端进入 Draining、在途请求恰好结清一次、发起侧 ≥5s 后才以 Local 退出、对端以 PeerGoaway 退出；h2 优雅关闭判为 PeerGoaway；无帧断线判为 Transport 且未决以 Closed 结清一次；handler panic 回固定 1009 且会话存活；业务队列打满时 shutdown/abort 仍生效；最后一个 handle drop 真的断掉 socket（两条流的 `wait_closed` 均为真）；Event 单向不占未决名额且排空后拒发；入站 Event 洪峰按 `dropped_events` 计数且不拖垮会话；System 只出现在 Control、载荷为空、发出侧会话继续服务；入站 System 到达 `on_system` 后两侧收场（收侧 Revoked、发侧 Transport）；Data 上出现 System 判 Protocol 且不交给业务回调；Data 先于 Control 到达被 RST 且会话既不 Ready 也不 Closed；单流 RST 只结清该流未决、另一条流照常拿到响应；同一条流反复被重置最终升级 StreamFlapping（Control 先行，至少 3 次重开）；无人认领的 RESPONSE 被静默丢弃且会话继续可用。

时钟：除 Event 洪峰一例外全部 `current_thread + start_paused`，期限用显式 `sleep(limit ± margin)` 证明；洪峰例外需要多 worker，因为 `on_event` 是同步回调，只能在多字节运行时里占住一个 worker 来模拟慢消费者。

## 测试发现并修掉的真实缺陷

1. **`cleaned` 假阳性**：原实现对刚 `abort()` 的句柄立刻 `is_finished()`，单线程运行时里 aborted 任务尚未被 poll → 恒 false。第一版改成 `join_next()` 循环后**真挂死**（暂停时钟下 await 立即就绪、循环从不让出，被 abort 的任务永远得不到 poll），最终改为：摘出 `JoinSet` → `abort_all()` → `timeout(2s, 排空 join_next)` + 带真实 `sleep(1ms)` 的确认循环；reader/writer 的句柄进入 `retired` 以便同样可观测。
2. **GOAWAY 从未上线（假 Local 通过）**：`begin_drain` 用共享 `draining` 标志判幂等，而 `graceful_shutdown()` 已在句柄侧同步置位 → owner 直接早退，既没调 `request_client_goaway()` 也没冻结探测，对端只在 5s 结束那一刻看到 EOF。改为幂等只看 owner 自己的记账（`goaway_requested`/`drain_started_at`）。这个缺陷此前让「排空」用例拿到了**错误的通过结论**。
3. **peer GOAWAY 与连接结束竞态**：h2 server 侧 `Connection` 几乎与 GOAWAY 同时结束，事件顺序决定结论，出现 `Ready → Closed` 越过 Draining。改为额外持有 `subscribe_peer_goaway()` 的 watch 副本：只要观测到对端 GOAWAY，即使它晚于/同时于连接结束也判为排空。

## 评审意见的根因收口（不依赖业务队列余量）

评审指出 `abort()`、最后一个 `SessionShared::Drop` 用 `let _ = cmd_tx.try_send(Abort)`，`graceful_shutdown()` 同样 `try_send(Shutdown)`，与业务命令共享队列，队列满时静默丢关停（违反「Indeterminate 必须真的关掉」，也让 controller 的 500ms 终局 abort 失效）。收口：`SessionCommand::{Shutdown,Abort}` 删除，关停意图改为 `SessionStatus` 内的持久 `watch`（幂等、单调取强、两条 ready 路径都会醒），公开 API 不变；`abort` 真的取消 handler/reader/writer/连接并丢 socket，`graceful_shutdown` 仍驱动满 ≥5s 而不只是置 Draining。

新增两个确定性用例：`business_queue=1` 下 64 个并发 `request()`，凡观察到 `Busy` 的调用**在同一 poll 内**（`try_send(Full)` 之后无任何 await）立即 `graceful_shutdown()+abort()`，从而证明意图确实是在队列全满时下达；断言 owner 以 Abort 退出且 `cleaned`、已受理请求不伪造回执（`calls() >= 成功数`）、失败只能是 Busy/Closed/StreamClosed/Timeout、socket 引用归零。**变异验证**：临时把意图投递改成丢弃（模拟队列满丢弃），两用例立刻在同一断言处失败（`owner 必须退出`），还原后 22 全绿——证明用例对该缺陷敏感，不是恒真断言。

## 中途 GOAWAY 的实测语义（纠正误结论）

- 「h2 0.4.20 在 client GOAWAY 后无法完成在途响应」是误判：真实原因是不再持续 poll `Connection`。连接 task 持续被 poll 时 GOAWAY 能正常写上线、在已 accept 的流上继续工作、≥5s 排空真实成立。
- **主代理复核纠正**：client GOAWAY 的 `last_stream_id=0` 是本协议禁用 server push 后的正确值，字段针对**对端发起**的流，不是 client 自己的 Control/Data 奇数流；不能据此声称全部既有请求都未处理，也不需要为此开放 last_stream_id 参数。主代理已在当前源码上重跑 `h2_wire_control::real_h2_midstream_goaway_preserves_both_response_header_timings`，GOAWAY 写出并被观察后才发送既有流 DATA/EOS，两种 response-header 时机均通过。原交付 driver 中途测试允许成功或 Closed/StreamClosed/Timeout，只证明结清与存活窗口，**不能据此证明存量响应成功排空**；若 driver 仍丢失该受控存量响应，应继续定位 driver 的驱动/关闭顺序，而非归因 last=0 或给已审 wire 层开错误接口。
- 主代理随后将 driver 中途测试收紧为**必须收到精确在途响应载荷**，不再接受 Closed/StreamClosed/Timeout；在当前源码上 exact 测试实际通过（GOAWAY 已被 peer 观察后才释放 handler，响应仍成功，且本地排空满 5s）。这进一步反证原 last=0 归因，不是生产逻辑修复；测试及报告修正仍待独立 reviewer 核查。
- 用例中 h2 客户端主动优雅结束会观测到 GOAWAY，按 PeerGoaway 分类；零帧 socket 断裂按 Transport。此为这些具体路径的证据，不外推任意 EOF 都必先有 GOAWAY。

## 其他实测语义与设计决策（本任务文件内）

- 入站分发在 handler 并发预算耗尽时直接回 `status_code=1002`（"handler concurrency exhausted"）而非无界排队，接收队列因此始终有界。
- 出站 System **不**杀掉发起侧会话：收/发义务分离，收侧按 Kicked/Revoked 收场，发侧的结论来自对端真的消失（Transport）。
- 入站 System 优先于随后的连接死亡：`on_system` 回调先按序送达，再整连接收场（kick/revoke 不受 5s 排空约束）。
- 开流失败（对端 accept 后立即 RST）不改判探测，等满 15s 开流窗口才计一次重开失败，三次才升级 StreamFlapping ⇒ 最坏 ~45s 升级，这是「15s 窗口」的直接后果，不是死循环（实测原始 h2 server 反复 RST 时客户端按 Control 优先持续重开）。
- 无人认领/过期 RESPONSE 静默丢弃；Data-first、Data 上的 System、System 载荷非空、未知 action 均按协议违规处理（前者 RST 该流不杀会话，后者整连接 Protocol 收场）。
  - 2026-10-24 收口更正（只改口径，不改分类逻辑）：这句把两条不同归属混在一起了。当前公共 `TunnelPacket::decode`（`validate_fields`）会先拒「System 载荷非空」「未知 System action」这类**字段/前缀违规**，它们在 reader 侧归 `StreamError::StreamProtocol`，**只 RST 本流、不杀连接**；当前真正连接级可达的只有 §6.3 的路由违规——Data 上出现 System、以及 Data-first。driver `on_packet` 里的 System 分支是**已解码路由**上的纵深防御（对当前 decode 不可达），不是第二条判定通路。

## 已知缺口（本任务不改，供后续决策）

1. 当前 client GOAWAY 的 last_stream_id=0 符合 peer-initiated 流语义，无需因此修改 `H2WireControl`；主代理收紧 driver 在途响应成功断言后 exact 测试已通过，待独立复核并扩真实 TCP/双进程证据。
2. h2 0.4.20 不暴露内层 `io::Error` 链：连接结束一律塌缩成 `Transport`，无法区分 TLS 记录错误/超时/对端 RST。要区分需上游在 `StreamEvent`/`H2WireControl` 侧透出错误分类。
3. `send_event`/`send_system` 的「成功」只等于已入队（有界队列 + 字节预算内），不等待对端 ACK；若 Task5/6 需要「kick 必须上线」级别的保证，需要新增显式确认原语，不能靠轮询探测替代。
4. `dropped_events` 只统计入站事件被有界队列丢弃的数量，不区分 action；Event 目前无优先级分级。
5. 未做双进程/真实 socket/RSS 实测；未验证真实 controller 500ms 终局 abort 的端到端时序（本任务只在同进程内证明关停意图在队列全满时仍生效）。

---

# Task4 独立评审后的修复轮（F1–F8，2026-10-23）

评审核心结论：22 个用例、wire exact、fmt、clippy 当时都是绿的，但存在 **F1/F2 规范偏差**；本轮按 finding 逐条 TDD（先 RED → 最小根因修复 → 常驻回归用例），旧用例一个未删、一个未放宽。改动仍只落在 `session/{driver,handle}.rs`、`tests/session_driver.rs`，以及 brief 明确允许的 **typed 错误传播**（`streams.rs`）与 **GOAWAY flush 观察接口**（`h2_keepalive.rs`）最小扩展及其对应 tests；`session/config.rs`、`pending.rs`、`probe.rs`、`types.rs`、`session/mod.rs`、`secure_io`、crypto/handshake、其它 crate、`Cargo.toml`/lock 均**零改动**（`diff -q` 复核 config.rs 与快照一致）。

## 接口变更（单列，全部新增式，无旁路开关）

公开 API：

1. `StreamError::StreamProtocol`（新变体，`crates/rsetup-tunnel/src/session/streams.rs`）——「对端送来的**应用包**内容非法（含 §6.7.2 非 16B 的 `tunnel.ping`）」，流级归属。
2. `StreamError::connection_fatal(&self) -> bool`（同上）——归属由**类型**给出，唯一 `true` 的是 `Self::Protocol`（§6.3 流路由结构性违规）。调用方不得再靠 Display 文本或自维护 bool 猜安全级别。
3. `H2WireControl::subscribe_outbound_goaway(&self) -> watch::Receiver<u64>`（`src/h2_keepalive.rs`）——**纯观察**计数：一条完整 GOAWAY 帧已通过底层 `poll_write` 且 `poll_flush` 得到确认时 +1。角色无关（client 自发注入走 `mark_sent()` 补记，server 侧 h2 自己写出的那一帧走出站帧记账 + flush 确认）；不因观察多写/少写一个字节；不改 `last_stream_id`、不改注入时机。**语义边界**：它证明「已离开本进程发送路径」，不等于对端已收到。
4. `SessionOutcome::dropped_replies: u64`（`session/handle.rs`）——因有界背压（byte 预算不足 / 业务队列满）被丢弃的**回包**计数，与 `dropped_events`（handler 回调队列丢弃）严格分开；只计数，不带 action/payload 等对端可控文本；不含「旧代际 handler 回包」那类非过载丢弃。

crate 内但类型可见：

5. `StreamEvent::StreamClosed { kind, epoch, protocol: bool }` → `{ kind, epoch, error: Option<StreamError> }`（`session/handle.rs`）。原来的 `bool` 就是 F2 的病灶：归属信息在 reader→owner 这一跳被压成一位，owner 只能猜。
6. `ServerConnection` 消费关停命令由 `cmd_rx.try_recv()` 改为 poll 上下文内的 `poll_recv`（**唤醒语义**修正，无签名变化）。这是 F1 server 侧的根因，见下。

## 逐条 RED → 根因 → 修复 → 绿

### F1：≥5s 排空必须锚在 GOAWAY 真正写出并 flush 之后（两侧都要）

- RED（client，`local_graceful_drain_is_anchored_at_actual_goaway_flush`）：`GOAWAY 上线后必须再维持至少 drain_timeout（5s）；旧实现按「请求发 GOAWAY」起算，此时早已退出`。
- RED（client 写方向不可写，`unwritable_goaway_fails_bounded_without_claiming_local`）：`GOAWAY 一个字节都没上线，结论绝不能是本地优雅排空  left: Local  right: Local`。
- RED（server，`board_graceful_shutdown_drains_after_actual_goaway_flush`）：`观测点 中控已进入 Draining 在 5s 虚拟期限内未成立`；调试快照 `controller=Ready board=Draining board_running=true controller_running=true` —— 对端**从未看到任何 GOAWAY 字节**。
- 根因（两条独立缺陷）：① 排空窗口按「owner 请求发 GOAWAY」而非「GOAWAY 实际 flush」起算，写方向被背压按住时 5s 已经在倒计时；② server 侧 `ServerConnection` 用 `try_recv()` 取关停命令，任务 park 后没有注册 waker，命令**永远不会被消费**，`graceful_shutdown()` 从未被调用，h2 一个 GOAWAY 都没写。
- 修复：`H2WireControl::subscribe_outbound_goaway()` 提供 flush 级观察点；owner 侧 `begin_drain` 只置意图并记录 `goaway_wait = now + drain_timeout`，`run_driver` 的 select 观察该 watch，收到确认才在 `settle_goaway_anchor` 里把 `drain_deadline` 锚到**实际 flush 时刻**；等待超时（GOAWAY 上不了线）→ 记 `goaway_failed`，结论走非 Local（Transport 或对端原因），**绝不谎称 Local**；peer-initiated 排空仍按收到的观测点锚定。server 侧改用 `poll_recv` 消费 Graceful 命令。
- 断言口径（诚实标注）：对端「读到 GOAWAY」比真正的 flush 晚至多一个轮询间隔，因此时序断言取带宽 `[4.95s, 5.4s)` 并在其中说明滞后；另用「GOAWAY 上线后 +4.5s 仍必须活着」这一条做「没有提前起算」的判别。
- 绿：31 passed。

### F2：非法应用探测只能终止该流；归属必须类型化；错误/迟到 pong 不得刷新健康

- 可复现性核查（必须如实说）：在**当前公共 `TunnelPacket::decode`** 下，非 16B 的 `tunnel.ping` 已在 decode 阶段被拒，错误塌缩为 `StreamError::Grpc` → 恰好走流级路径，所以「非法探测杀整条连接」这一后果 today **不可达**；`illegal_probe_resets_only_that_stream` 在旧源码上是绿的。也就是说 F2 的偏差是**分类靠巧合 + 接收端没有 §6.7.2 自身核验**，不是当前可观察的行为缺陷。
- 类型层面的 RED 是硬编译证据：新用例 `error_ownership_is_typed_not_inferred_from_text` 引用 `StreamError::StreamProtocol` / `connection_fatal()`，在旧 `streams.rs` 上不存在该 API（接口级 RED，非行为级）。
- 修复（不改行为、只封死回归路径）：`recv_open` 把 `GrpcError::EnvelopeDecode(_)` 归为 `StreamProtocol`（流级）、帧框架失败保持 `Grpc`（流级）、能解码但 `validate(kind)` 失败保持 `Protocol`（连接级）；owner 的 `on_stream_closed` 只看 `error.connection_fatal()`，不再有 bool。新增接收端 `probe_request_current`（Request + 恰好 16B + 空 metadata + 非零 trace + **代际匹配**）与 `fail_stream`（只 RST 本流、按既有 ProbeState 走重开），`probes_served` 只对**合法**探测自增——公共 decode 的强校验一个都没放宽，也不依赖上游恰好先拒。
- 错误 pong 不刷新健康：`wrong_nonce_pong_does_not_refresh_health` —— 控制流只回「trace/action 都对、nonce 被改坏一字节」的形似合法 pong，数据流每个窗口正确代答；结果控制流按**既有** ProbeState 连续 3 个未答窗口升级重开（对端看到本流被终结），数据流仍完成一次载荷精确的真实往返，会话不是 Closed。未新建第二套健康状态机。
- 附带澄清：`probe_missions` 只统计探测**发出侧**失败（登记/投递不出去），不表示「窗口未获有效 pong」，因此不能用它做本项断言——这是本用例改写一次的直接原因。

### F3：byte 预算必须整连接一份

- RED：`两条流必须共用同一份 byte 预算，实测 Ok(())`（旧实现每条流各建一份 `ByteBudget`）。
- 根因：`register()` 每次为每条流新建预算；重开（新 epoch）同样重新发配额，等于把 `business_queue_bytes` 上限偷偷乘以流数与重开次数。
- 修复：预算在 `Driver::new` 里按 `config.business_queue_bytes` 创建**一次**，`register()` 只 `clone()` 同一个 `Arc` 给各代际 Slot。
- 用例：`byte_budget_is_one_per_connection_not_per_stream` —— 对端只声明 16KiB 流窗口，**数据流**上压一个 400KB 请求（`request(Data, 400_000)`）使 writer 卡在 h2 流控里（预留不归还），此时**控制流**上的 200KB **Event**（`send_event(Control, 200_000)`）必须 `Busy`。预算耗尽只回 Busy，不是会话故障；用例结尾以 `abort()` 收场并断言 `cleaned`。
  - 2026-10-24 更正：本条之前的表述把两条流写反了（写成「控制流压 400KB、另一条流 200KB 包 Busy」），并多写了一个用例里并不存在的「放开写方向后两个包照常送达」步骤。按代码更正为此处的方向；共享配额的另一半证据是 `dropped_replies_are_counted_not_faked`（回包侧共用同一份额度）。

### F4：回包被丢必须留下可观察的 sanitized 计数

- 接口级 RED：`SessionOutcome::dropped_replies` 字段在旧实现不存在，用例无法编译（不是运行时断言，如实标注）。
- 修复：`send_reply` 在预算 `try_reserve` 失败与 `biz_tx.try_send` 失败两处自增 `dropped_replies`，不伪造响应；对端只能按自己的期限结清。
- 用例：`dropped_replies_are_counted_not_faked` —— 小请求 → 固定 400KB 回包，16KiB 流窗口把第一个回包卡在流控里，第二个 echo 回包放不下被丢：`dropped_replies == 1`、`dropped_events == 0`，两个请求都**没有**拿到伪造的成功回执。

### F5：退役表必须剪掉已确认结束的句柄（但绝不 detach 未结束的）

- 现状缺陷：`retired` 只增不减（teardown 把 abort 后的句柄 push 进去，从不剪枝），长会话多轮重开会无界堆积。
- 修复：新增 `prune_finished(&mut Vec<JoinHandle<()>>)`（只 `retain(!is_finished())`），在 `teardown` push 被 abort 的句柄**之前**先剪枝；未确认结束的一个都不许丢出表外。
- 常驻单测（`src/session/driver.rs`）：`prune_finished_only_drops_confirmed_handles`（已结束的被摘掉、未结束的必须留在表内）、`prune_finished_bounds_the_retired_table`（64 个全部结束后收敛到空表）。

### F6：notify + join 整段必须有界；确认不了就 cleaned=false

- RED（旧实现实跑）：`整个 notify+join 阶段必须有界（旧实现要等 6s 的慢回调跑完），实测 5.779597164s`。
- 退出上界的**真实构成**（本轮复核更正，之前写成「0.5s + 2s」是低估）：通知投递 ≤`SYSTEM_GRACE` 0.5s + 等完成信号 ≤0.5s + `cleanup()` 最坏 ~4s（handler JoinSet 排空 ≤`CLEANUP_GRACE` 2s **之后**，附属/退役句柄的确认轮询再给 ≤2s）+ 回调确认 ≤2s ⇒ 最坏 ≈7s；无堆积时 cleanup 一轮 1ms 返回，典型 ≈3s；均再加调度余量。同步回调不可抢占，所以这里只能承诺「等满期限」，不能承诺「让回调停下来」。
- 本节的「abort 后 timeout 确认」在**句柄处理**上仍有缺陷（超时会消费 `JoinHandle`）：见文末《F6 收口》。本节其余结论（有界、`cleaned` 如实、`on_closed` 不伪造）仍然成立。
- 用例本身的敏感性教训（如实记录）：第一版只把 64 个事件一次性灌进去，**在旧实现上也是绿的**——消费者取走队首后腾出一个空位，`Closed` 直接入队，无界 await 没被触发。改成「先确认回调真的进入 handler，再把队列灌满」并断言前置条件 `dropped_events > 0` 之后才拿到 RED。
- 修复：`finish()` 三段有界——`timeout(SYSTEM_GRACE, notify_tx.send(Closed))` 取投递结果；再 `timeout(SYSTEM_GRACE, callback_done)` 等**完成信号**（`oneshot`，因为 `JoinHandle` 不可 clone，且 tokio 的 `blocking_recv` 在 task 内必 panic，不能拿来当确认手段）；`cleanup()` 之后若任务仍未确认结束 → `abort()` + `timeout(CLEANUP_GRACE, join)` 确认，确认不了就 `cleaned &= false`。
- 绿：新实现 `stuck_callback_exits_bounded_and_reports_cleaned_honestly` 断言 `elapsed < 4s`、`!cleaned`、`on_closed` 次数为 0（没送达就是没送达，不伪造；正常路径的「恰好一次」仍由既有用例守住）。
- 顺带修掉的自伤：`Ok(Err(_))`（回调任务 panic 结束）**也算已确认结束**——旧实现把它当结束、我第一版误当没结束，导致 `inbound_event_burst_is_dropped_with_counter` 的 `cleaned` 回归，已按语义修正。

### F7：饱和用例的注释不得夸大

- `business_queue=1` 那条饱和用例注释写的是「命令通道容量 = business_queue = 1，Busy 只可能来自 try_send(Full)」——两处不实：cmd 通道容量是 `business_queue.max(4)`（本例 4），Busy 也可能来自每条流 `biz_tx` 容量 1。已改为如实描述饱和来源（并保留「排除 max_pending 这一来源」的推理），同时把「没有伪造应答」那条断言标注为**必要条件检查而非充分证据**。
- `driver.rs` 里 cmd 通道容量处补注释。2026-10-24 更正：该注释当时的写法（「保证**关停/探测**这类控制意图不会因为业务队列被压到 1 就完全发不出去」）不实——`SessionCommand` 只有 `Enqueue`/`SendEvent` 两条业务投递；`graceful_shutdown()`/`abort()` 走独立的 `SessionStatus::intent` watch，探测由 owner 自己按窗口经每流 `ctl_tx` 小队列发出，**都不经过 cmd 通道**。注释已按事实改写：`business_queue.max(4)` 只是给业务投递留基本余量，硬上限仍是每流 `biz_tx` 与整连接 byte 预算，两处都以 Busy 结清。

### F8：server 发起的优雅关停缺证据

- 新增 `board_graceful_shutdown_drains_after_actual_goaway_flush`：设备侧 `graceful_shutdown()`，在途请求必须拿到**精确载荷**的响应（不接受 Closed/Timeout 兜底），对端进入 Draining 的观测点来自真实 GOAWAY 字节，排空从实际 flush 起算满 5s。它与 F1 server 侧是同一处根因（`try_recv` 永不唤醒），RED 同上一条。
- 另加 `board_graceful_without_writable_goaway_is_not_local`（server 写方向完全按住：有界退出、结论非 Local、对端不得被判成 PeerGoaway）。

## 本轮验证命令与结果（本机实跑，`CARGO_TARGET_DIR=target/tunnel-session-fix --locked --offline`）

- `cargo test -p rsetup-tunnel --test session_driver -- --test-threads=1` → **31 passed / 0 failed**（原 22 个一个未删，新增 9 个）。
- `cargo test -p rsetup-tunnel --locked --offline` → 全部 15 个 target 绿；其中 `session_streams` 15 passed（新增 1 个类型归属用例）、`h2_wire_control` 18 passed（新增 1 个出站 GOAWAY 观察用例）、`rsetup-tunnel` lib 单测新增 2 个 `prune_finished` 用例。
- `cargo fmt --package rsetup-tunnel -- --check` → exit 0（本仓包级口径，未用 `--all`）。
- `cargo clippy -p rsetup-tunnel --all-targets --locked --offline -- -D warnings` → exit 0。
- `cargo check --workspace --all-targets --locked --offline` → exit 0（公开新增未破坏其它 crate 编译）。
- RED 取证方式：把改动前快照源码换入（`target/tunnel-session-fix/snapshot/`），只跑新用例，记录失败消息，再把新源码换回并复跑全量 31 绿（双向 `diff -q` 校验还原）。

## 本轮未验证 / 不得外推

1. 仍未做真实 controller/board 双进程、真实 TCP socket、RSS 实测；观察点仍是同进程 `SecureIo<DuplexStream>` + 真实 h2。
2. `subscribe_outbound_goaway` 证明的是「字节已过底层 `poll_write` 且 `poll_flush` 确认」，**不等于对端已收到**；文档与用例都按这个口径写。
3. F2 的连接级误杀在当前公共 decode 下不可达，本轮修复是类型化归属 + 接收端 §6.7.2 自身核验的**防回归**，未构造「decode 变宽」的假设场景；`error_ownership_is_typed_not_inferred_from_text` 是接口级 RED，不是行为级 RED。
4. F4 只覆盖 byte 预算耗尽这一条丢弃路径；队列满路径共用同一计数器，未单独构造。
5. F5 只有 `prune_finished` 不变量单测，未构造长会话多轮重开的端到端堆积曲线；「不 detach 未结束任务」靠该不变量 + `cleaned` 语义共同守住。
6. F3 的「重开不重新发配额」半边由结构性事实保证（预算在 `Driver::new` 创建一次、`register()` 只 clone 同一个 `Arc`）+ 双流共享行为用例；未单独构造跨 epoch 的配额续用用例。
7. `board_graceful_without_writable_goaway_is_not_local` 在旧实现上也是绿的，因此它只 pin 住新语义，**不构成本轮的敏感性证据**；「GOAWAY 写不出去绝不判 Local」的敏感性证据来自 client 侧镜像用例（`left: Local`）。
8. 时序断言的观察点滞后：对端读到 GOAWAY 晚于真正 flush 至多一个轮询间隔，故用 `[4.95s, 5.4s)` 带宽；这不改变「绝不在 flush 之前起算」的判别力（另有 +4.5s 仍在排空的断言）。

---

# F6 收口：确认不了的回调句柄转交进程级追踪表（复核 dd155a4d 的 Low-1，2026-10-24）

范围：只改 `src/session/driver.rs`、`src/session/streams.rs`（**仅注释**）、`tests/session_driver.rs` 与本报告。未提交、未推送；未触碰 crypto/handshake/wire/pending/probe、`Cargo.toml`/`Cargo.lock`、其它 crate、真实 DB 与服务端。改动前源码快照在 `target/tunnel-session-callback-fix/snapshot/`。

## 缺陷（复核成立，上一轮的「已闭合」不成立）

`finish()` 里是 `callback_join.abort(); timeout(CLEANUP_GRACE, callback_join).await`。`JoinHandle` 不可 clone，`timeout` 到点即**消费并丢弃**该 `JoinHandle`，而丢弃未完成的 `JoinHandle` 语义上就是 detach；`abort()` 只让任务在**下一个 await 点**取消，同步阻塞在 std 调用里的 handler 根本到不了那个点。于是「有界退出」是真的，但代价是把还在跑的原句柄扔掉、把它谎称为「已经没有残留」——正是 brief 指出的「不能 timeout 消费 JoinHandle 后 drop 变 detach」。

## 做法（不 detach、不无界、不新增下游必须改构造的公开 API）

- **进程级硬容量追踪表**：`OnceLock<Mutex<CallbackRegistry>>` + `Vec<TrackSlot>`，`pub const CALLBACK_TRACK_CAPACITY: usize = 1024`；槽位状态 `Free | Reserved | Tracked(JoinHandle<()>)`。锁只在进入瞬间持有，**绝不跨 `.await`**；中毒用 `into_inner()` 继续（表里只有一个状态机，不携带跨任务不变量）。
- **`CallbackTracker`（私有 RAII）**：`start()` 在**任何 spawn 之前**先 `reserve()` 预留名额，预留不到就直接失败关闭，转交因此**不可能**因为表满而被迫 detach；spawn 后 `attach(handle)`；owner 收尾时要么 `release_confirmed()`（已确认结束 → 立刻归还名额），要么 `abort()` + `settle(grace)` 轮询 `is_finished()`，仍未确认就由 `Drop` 把**原句柄** `commit` 回表。整条路径上没有任何一处 `drop(handle)`。
- **只追踪未确认结束的那一个回调**（每会话一个）：已确认结束立刻归还；`prune`（只有 `is_finished()` 为真才置 `Free`）在 reserve / commit / 外部观察三处顺带做，**不引入后台轮询 task**。
- **公开面只多一个只读观察函数** `pub fn quarantined_callbacks() -> usize`（内部类型仍私有，下游无需改构造），既给运维一个可见量，也让用例能直接断言「句柄真的被追踪」而不用读日志。
- **不永久泄漏 handler**：被追踪的是 `JoinHandle`，任务一结束 runtime 就释放它捕获的 `Arc<dyn SessionHandler>`；集成用例用 `Arc::strong_count` 断言这一步真的发生（见下）。
- **名额语义与失败关闭**：容量的真实口径是「**同一进程内正在进行的会话数**」，不是「只算未确认结束的回调」（本条上一版写成后者，属 Minor-1，本轮已更正，详见文末《两处 Minor 更正》）：**每会话从 `start()` 里 `reserve()` 那一刻起独占一槽，直到收尾才归还**——正常会话占「启动 → `release_confirmed()`」整段，转交过的会话继续占「转交 → 任务结束后被下一次 `prune()` 顺带回收」，且这张表进程级唯一，client 与 server 会话在同进程里**共用**这 1024 槽（1024 条正常存活的会话同样能把表占满，不需要任何回调卡住）。满表时**新会话启动直接失败**（`SessionError::InvalidConfig("callback tracking slots exhausted")`——`config.rs` 不在本轮范围，故复用既有变体并如实写明原因，不新增公开枚举），而不是退化成无界 detach。被永久卡住的用户代码会占住一个**可见**名额，但不能凭空多出会话。
- **如实边界（不得外推）**：这**不是强制取消**——被卡住的 handler 代码不会因为 owner 退出而停下来；owner 有界返回与 runtime/进程退出是**两条边界**，追踪表是 `static`，随进程结束消失，不构成「用户代码已被杀死」的保证。
- 顺带收紧：`finish()` 的投递判定改为按**真实结果**取（`matches!(.., Ok(Ok(())))`）。旧写法 `timeout(send).is_ok()` 会把 `Ok(Err(_))`（接收端已关闭=回调任务早已消亡）当成「on_closed 已送达」，从而跳过后面的完成确认，让 `cleaned` 失去依据。
- 修正 `finish()` 的注释口径：退出上界的真实构成是「通知 ≤0.5s + 完成信号 ≤0.5s + `cleanup()` 最坏 ~4s（handler JoinSet ≤2s **之后**附属/退役句柄确认再 ≤2s）+ 回调确认 ≤2s」，最坏 ≈7s、典型 ≈3s，不是「0.5 + 2」。
- `streams.rs` 注释更正：`StreamError::Protocol` 不再声称「未知 System action」当前可在连接级到达——公共 `TunnelPacket::decode` 的 `validate_fields` 会先拒它并归 `StreamProtocol`（只 RST 本流）；driver 的 System 分支是**已解码路由**上的纵深防御，Data 上的 System 仍按 §6.3 属连接致命（分类逻辑未改，只改注释与报告口径）。

## 测试（新增 4 个 driver 内部单测 + 2 个集成用例）

- `callback_track_quota_is_hard_bounded_and_returned`：**独立 fixture（容量 2，不动生产 1024）**，第三次预留必须 `None`；归还后可再用。
- `unconfirmed_callback_handle_is_handed_over_then_reclaimed`：容量 1 的 fixture；任务未结束 → `settle` 超时 → 名额进入 `Tracked` 且此时再预留 `None`（转交成功、不 detach）→ 放行任务后下一次观察回收，名额重新可预留。
- `tracker_dropped_without_settling_still_tracks_handle`：`Drop` 兜底路径同样转交，不丢句柄。
- `confirmed_callback_releases_its_slot_immediately`：确认结束立即归还（正常清理把名额还回去）。
- `uncancellable_callback_is_tracked_then_reclaimed`（multi_thread 真实时钟）：`on_closed` 里用 `std::sync::Condvar` **真实阻塞**（不是 future，取消信号对它无效）。断言：owner 有界返回（`elapsed < 4.5s`）、`on_closed` 恰 1 次且此刻仍卡在 handler 里、`!cleaned`（确认不了就绝不谎称已清理）、`quarantined_callbacks() == 1`（**原句柄仍在追踪**）、`Arc::strong_count == 2`（追踪中的任务确实还持有 handler）；`gate.open()` 后追踪表回收回 0 且 `Arc::strong_count == 1`（**已完成的回调不永久扣着 handler**）。
  - 两个自伤已修：`Arc::strong_count` 的释放晚于 `is_finished()`，故该断言改为**有界轮询**而非「立刻看」；用例加了 `GateGuard` 兜底守卫——失败路径若不打开闸门会留下**永久阻塞的 worker** 并挂死测试进程（实测挂过一次，已消除）。
- `dead_callback_task_is_confirmed_not_claimed_delivered`：回调在 handler 里 panic → 通知通道关闭 → 后续事件只被丢弃计数；断言 `dropped_events > 0`、`on_closed` 次数为 0（不伪造送达）、`cleaned == true`（任务确认可了结）、`quarantined_callbacks() == 0`（确认了结不占名额）。**如实标注**：本用例在新旧实现上都绿，它 pin 的是收紧后的语义与「名额不泄漏」，不单独作为敏感性证据。
- 原 31 个用例的断言一个未改、一个未删（含 F6 那条保留 6s 慢回调的断言）。

## 本轮验证命令与结果（本机实跑，`CARGO_TARGET_DIR=target/tunnel-session-callback-fix --locked --offline`）

- `cargo test -p rsetup-tunnel --lib` → **12 passed / 0 failed**（原 8 个 + 新 4 个追踪表单测）。
- `cargo test -p rsetup-tunnel --test session_driver -- --test-threads=1` → **33 passed / 0 failed**（原 31 个一个未改 + 新 2 个）。
- `cargo test -p rsetup-tunnel --locked --offline` → 15 个 target 全绿：lib 12、envelope 26、grpc 19、grpc_memory_budget 6、h2_keepalive 37、h2_wire_control 18、handshake 48、identity 22、parser_stress 6、secure_io 47、session_driver 33、session_state 13、session_streams 15、tcp_stack 3。
- `rustfmt --edition 2024 <本次 3 个文件>` + `cargo fmt --package rsetup-tunnel -- --check` → exit 0（本仓包级口径，未用 `--all`）。
- `cargo clippy -p rsetup-tunnel --all-targets -- -D warnings` → exit 0。
- `cargo check --workspace --all-targets --locked --offline` → exit 0。
- **敏感性（RED）取证方式**：本轮修复点无法用「换回旧源码」取 RED（旧写法同样有界、同样 `cleaned=false`，只多一个 detach），故改用**变异取证**：临时把 `CallbackTracker::Drop` 的转交改回丢弃句柄（等价于旧的 detach 行为），新用例立刻按语义失败——driver 内部 2 例 `left: (1, 0) / right: (0, 1)`（「丢弃未确认的句柄等于 detach，必须由 Drop 转交追踪表」），集成用例 `left: 0 / right: 1`（「超时的原回调句柄必须仍在追踪表里」，2.51s 内失败不挂起）；随后从快照 `cp` 还原 + `diff -q` 校验一致，复跑 12/33 全绿。除本次三个源文件外无任何文件被写入，快照本身未改。
  - 上一轮 RED 取证方式的一处自伤也如实记录：`wait_until(..)` 漏 `.await` 时被 `-D warnings` 的 `unused_must_use` 挡下（future 不 poll 等于什么都没做），clippy 的严格门禁这次起到了兜底作用。

## 本轮未验证 / 不得外推

1. 追踪表的**进程级**语义只能在同进程内验证；跨进程/多实例、真实 socket、RSS 实测仍未做（口径同上轮）。
2. `CALLBACK_TRACK_CAPACITY = 1024` 是**人为选定的显式上限**（对齐 `max_pending` 上限），未依据真实部署会话数做过容量论证。本条上一版还写着「用例一律用独立小容量 fixture」，属 Minor-2，本轮更正为：**只有 4 个 driver 内部单测**用小容量独立 fixture（容量 1/2，不动生产 1024）；两条集成用例（`uncancellable_callback_is_tracked_then_reclaimed`、`dead_callback_task_is_confirmed_not_claimed_delivered`）以及 `stuck_callback_*` 走的是**生产全局表**，它们的隔离手段是**测试内的显式互斥 + 释放互斥前确认自己那份回调已结束**（见文末新章节），既不是小 fixture，也不该只是命令行 `--test-threads=1`。
3. 「回调永久阻塞 → 名额被永久占用」只验证到「名额被占住 + 新会话失败关闭 + 释放后回收」，未验证长期真实部署下的名额耗尽运维处置（无告警/指标接入）。
4. `Ok(Err(_))` 的投递判定收紧在旧实现上不会变红（同判「已了结」），因此它属于**语义 pin**，敏感性证据来自上面的变异取证与 `dropped_events > 0` 前置断言。

---

# 追踪表用例的测试内串行化（复核 cf45c4c7 指出的理论 flaky，2026-10-24）

范围：**只改 `tests/session_driver.rs` 与本报告**。生产源码、公开 API、`Cargo.toml`/`Cargo.lock`、`Makefile`、config、其它 crate 与其它测试文件、`secret/` 一律未动；未提交、未推送、未 clean、未起真实服务或 DB。构建/测试目录 `target/tunnel-callback-isolation`（`--locked --offline`，本机 `nproc=32`）。

## 上一轮的 Low-1 结论不变

Low-1 的修复本身仍然成立，其证据是 reviewer（cf45c4c7）独立实跑的 **lib 12 + driver 33 在默认并行与串行两种口径下均绿**，本轮不扩大 workspace、不做双进程/RSS 实测，也不重复那批取证。本轮只收口 reviewer 另行指出的**共享全局状态**问题。

## 根因（窗口推算，未跑出失败）

`quarantined_callbacks()` 读的是**进程级**追踪表（`OnceLock<Mutex<CallbackRegistry>>`），而 3 个集成用例对它做裸 `== 0 / == 1` 断言。`stuck_callback_exits_bounded_and_reports_cleaned_honestly` 的 6s 同步 `std::thread::sleep` 在 owner **有界退出之后**仍在跑，期间名额一直是 1：实测该 owner 约 2.5s 返回、回调 6.0s 才结束，即**约 3.5s 的窗口**里表值是 1。默认并行时 `uncancellable_callback_*` 与 `dead_callback_*` 的 `== 0` 前置/收尾断言若落进这个窗口就会红。

**如实记录：本轮没有复现出失败**，reviewer 也只做窗口推算。改前基线默认并行 33/33 绿（下表第 1 行）属于**时序恰好错开**，不是正确性证据；而 `make test` 走默认并行，不能靠命令行 `--test-threads=1` 规避。

## 做法（互斥只在测试里，不进生产）

- 测试文件内新增 `static QUARANTINE_TABLE: tokio::sync::Mutex<()> = Mutex::const_new(())`，上述 **3 个**用例第一件事就是 `let _serial = QUARANTINE_TABLE.lock().await;`。选 `tokio::sync::Mutex` 而不是 `std::sync::Mutex`：各用例跑在各自独立的 runtime 上，跨 runtime 唤醒可靠，且失败时不中毒（std 的中毒会让后续用例级联报错、掩盖真实失败原因）。无新依赖（tokio 本就是 dev 依赖）。
- **释放互斥前必须确认自己那份阻塞已经结束、表回到基线**，这是本轮的重点，owner 一返回就 drop guard 等于把同一份残留甩给下一个用例：
  - `stuck_callback_*`：owner 退出后先观察 `== 1`（证明残留真实存在，也证明下面的清场不是空转），再在两行 owner 都确认退出后**等满**那份 6s 回调、表回 0 才放锁；6s 是旧 F6 敏感性的前提，**没有缩短**，代价是本用例墙钟由「与 owner 退出重叠」变成「等满 6s」。
  - `uncancellable_callback_*`：`gate.open()` 后等表回 0（沿用既有 `GateGuard` 兜底——失败路径也保证开闸，不留永久卡死的 worker），再加一道放锁前的收口观察。
  - `dead_callback_*`：无闸门（回调是 panic 消亡，可确认了结），放锁前同样再看一次表为 0。
- 新增 `async fn wait_quarantined_is(expected, budget, evidence)`：**精确值**的有界等待（每 20ms 观察一次，`quarantined_callbacks()` 自身会顺带 `prune`，无需额外驱动），期限预算与既有 `wait_until` 的 5s 分开（慢回调的回收窗口本就长于 5s）。入口预算 10s、收尾预算 30s。
- **核心断言一个都没放宽**：中途「未确认结束的句柄必须仍在表里」仍是 `assert_eq!(.., 1)`，收尾「确认了结不得占名额」仍是 `assert_eq!(.., 0)`，没有改成 `>=` 或任意差值；生产侧容量 `CALLBACK_TRACK_CAPACITY` 未改，也没有新增任何 test setter。
- 只串行这 3 个，其余 30 例照常并行：它们的回调全程只占 `Reserved` 名额（都断言 `cleaned == true`，走 `release_confirmed()`），不进入 `tracked()` 计数，彼此不干扰。
- 原 33 个用例的行为断言逐条未改、未删；本轮只做「加互斥 + 放锁前确认 + 入口断言改有界等待」三件事，其中新增的观察 `== 1` 走有界等待而非裸 assert，是为了不把自己的成败押在 owner 任务 drop glue 与唤醒之间的微秒级顺序上。

## 本轮验证命令与结果（本机实跑，`CARGO_TARGET_DIR=target/tunnel-callback-isolation --locked --offline`）

| # | 命令 | 结果 |
| - | ---- | ---- |
| 1 | 改前基线 `cargo test -p rsetup-tunnel --test session_driver -- --test-threads=8` | 33 passed / 0 failed，**6.02s**（绿=窗口未踩中，见上） |
| 2 | 改后默认并行 `cargo test -p rsetup-tunnel --test session_driver` | 33 passed / 0 failed，**8.69s** |
| 3 | 改后受控并行 `cargo test -p rsetup-tunnel --test session_driver -- callback`（只 3 例，默认线程） | 3 passed / 0 failed，**8.68s**；连续 3 轮 8.68 / 8.67 / 8.68s |
| 4 | 三例单独 `-- --exact <名>` | 6.03s + 2.53s + 0.13s = **8.69s** |
| 5 | 改后串行 `... --test session_driver -- --test-threads=1` | 33 passed / 0 failed，**8.94s** |
| 6 | 改后整包 `cargo test -p rsetup-tunnel` | 15 个 target 全绿：lib 12、envelope 26、grpc 19、grpc_memory_budget 6、h2_keepalive 37、h2_wire_control 18、handshake 48、identity 22、parser_stress 6、secure_io 47、session_driver 33、session_state 13、session_streams 15、tcp_stack 3（doc 0） |
| 7 | `rustfmt --edition 2024 crates/rsetup-tunnel/tests/session_driver.rs`；`cargo fmt --package rsetup-tunnel -- --check` | 均 exit 0（本仓包级口径，未用 `--all`） |
| 8 | `cargo clippy -p rsetup-tunnel --all-targets --locked --offline -- -D warnings` | exit 0 |

- **串行性的可重复证据**：第 4 步三例单独耗时相加 **8.69s** ≈ 第 3 步三者并跑的总墙钟 **8.68s**，即三者被互斥真正串成一条、**零重叠**；这不是「跑很多次没红」，而是同一份时间等式的复现（3 轮分别 8.68/8.67/8.68s）。因此不需要额外加一个「自检 helper」用例，也不做随机 100 次这类碰时序的验证。
- **耗时增长如实记录**：`session_driver` 这个 target 的墙钟 6.02s → 8.69s（**+2.67s，约 +44%**），整包 `-p rsetup-tunnel` 为 8.67s；增长全部来自「`stuck_callback_*` 现在要等自己那份 6s 同步 sleep 结束才放锁」。不缩短 6s，是为了不破坏旧 F6「慢回调不能把 owner 钉死」的敏感性。
- 本轮未跑 `cargo check --workspace --all-targets`：生产源码零改动、公开面零变化，改动面只有那个测试文件与本报告。

## 本轮边界 / 不得外推

1. **没有红色可交**：本轮任何一次运行都是绿的，也没有做变异测试（未临时改生产源码去验证用例敏感性）。这是**按共享状态根因做的预防性 flaky 收口**，不是 RED→GREEN，也不声称复现过失败。
2. 互斥是**测试内约定**，编译期不强制：以后再有断言 `quarantined_callbacks()` 的用例必须自己 `QUARANTINE_TABLE.lock()`（夹具注释已写明），漏了就等于重新引入同类 flaky。
3. 精确计数的成立依赖一个事实：其余 30 例都断言 `cleaned == true`（走 `release_confirmed()`，不产生 `Tracked` 名额）。将来若新增会得出 `cleaned == false` 的用例，必须一并纳入这把互斥，否则本收口失效。
4. 入口的 `== 0` 前置由裸 assert 改成**有界等待**（10s 预算，到期按实测值报错）：它只兜住「上一个用例**自身失败**后留下的、终将自然结束」的残留；真正永不结束的回调会到期直接失败，因此不构成对真实资源泄漏的容忍或放宽。
5. 时间等式证明的是「三例不再重叠」，不是「生产语义变强了」：追踪表容量、跨进程语义、真实 socket 与 RSS 的边界与上一轮完全一致（见上文两条 Minor 更正与其原始口径）。

## 独立复核结论（ad3af232，2026-10-24）

**Spec PASS / Quality PASS，原测试隔离与容量文案 Minor 全部 ADDRESS，无新阻断。**
复核者只读检查三例命名互斥 guard 的全函数生命周期、放锁前回调完成及归零、GateGuard
失败开闸和其它用例不产生 `Tracked` 的前提。控制流证明为主，耗时等式仅为佐证。
独立默认并行全套 **33 passed**、callback filter **3 passed**，均约 8.68s；包级格式通过。
未重审已由 cf45c4c7 验收的生产追踪表、未跑 workspace 或变异验证，不外推双进程/RSS。
本轮 session 实现与测试侧修复完成组件审查，整体生产验收仍待数据库与实际进程验证。

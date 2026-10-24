# Tunnel Task 4a（修复轮）：HTTP/2 连接级唯一 token PING 保活适配器测试报告

本报告**取代**同一分支上 Task 4a 首轮报告的内容：首轮结论中有三条被复核推翻（CSPRNG 兜底、
“窗口推进时已开写的帧一定写完”、header block 可交错性），本轮按
`.superpowers/sdd/2026-10-23-production-v3-tunnel/task-4a-fix-brief.md` 重新实现并重新验证。
首轮报告仍然成立的部分（§2 自建 PING 的依据、透明性/内存上界结构、真 h2 互操作用例）在下面保留。

## 1. 本轮目标与涉及文件

依据 `docs/protocol_spec.md` §6.7.1，修复评审确认的三项缺陷（A 在途半帧被新窗口覆盖、
B CSPRNG 失败退化成确定性 token、C 首次错误不释放底层 io）与四项 Low，并补齐 RFC 9113
§4.3/§6.10 的 header block 不可交错约束；全程 RED 先于 GREEN。

涉及文件（本轮只改这三个，未碰 `secure_io.rs`/`crypto.rs`/`grpc.rs`/`handshake.rs`/`lib.rs`/manifest）：

- `crates/rsetup-tunnel/src/h2_keepalive.rs`（1459 行，含 5 个 `#[cfg(test)]` 单测）
- `crates/rsetup-tunnel/tests/h2_keepalive.rs`（2272 行，35 个集成用例）
- `docs/testing/tunnel-task-4a-report.md`（本文件）

`crates/rsetup-tunnel/src/lib.rs:5` 的 `pub mod h2_keepalive;` 是并行任务预先加入的；全仓
`grep H2KeepaliveIo` 除本模块与其测试外**零命中**，即会话层尚未消费该组件，本轮的
accessor 签名变化没有下游调用方需要跟进（见 §4）。

## 2. 修复前基线与 RED 证据

1. 改动前基线（同一 target 目录）：
   `CARGO_TARGET_DIR=target/tunnel-task-4a-fix cargo test -p rsetup-tunnel --test h2_keepalive --locked`
   → `test result: ok. 26 passed; 0 failed`。这 26 项在修复后全部保留且仍然通过。
2. 评审复现驱动 `target/tunnel-review-4a-extra/driver.rs` + `run.log`（gitignore 目录，只读参考）
   在修复前实测：
   - `A cut=9: old 9B header followed by new PING header bytes as old 8B payload; 9B trailing fragment remains`
   - `A reproduced for ALL cuts 1..=16: old prefix + new full 17B PING, not old remainder + new PING`
   - `C first timeout: shutdown_calls=0 drops=0 fd_open=true peer=WouldBlock (no FIN)`
   - `C second poll: peer FIN observed, BUT drops=0 fd_open=true and underlying TcpStream still receives peer bytes`
   - `C pending lower shutdown: first error calls=0; five subsequent polls calls=1 (Pending discarded permanently)`

   该驱动断言的是**修复前**行为，并使用旧的“accessor 必返回 `&mut S`”接口；修复后它既不再成立也
   不再可编译，已由仓库内用例取代，本轮不做适配。
3. 修复后把每一项修复单独改回旧语义，确认对应用例真的会红（RED 反向验证）：

| 临时回退的修复 | 变红用例 |
| --- | --- |
| 窗口推进时丢弃在途半帧（`inflight = None` 后开新窗） | `partial_probe_across_window_finishes_old_frame_first`、`partial_probe_across_windows_never_backlogs_dropped_probes` |
| CSPRNG 失败退化为“上一 token +1” | `csprng_failure_closes_connection_without_deterministic_token`、`csprng_failure_mid_window_fails_closed` |
| 终态不释放底层（去掉 `self.io = None`） | `fatal_timeout_releases_inner_before_first_error`、`underlying_write_failure_releases_inner_immediately`、`fatal_timeout_over_real_tcp_closes_socket` |
| 注入边界不再看 header block 状态 | `probe_is_not_inserted_inside_a_header_block` |
| 底层 EOF 直接报干净 EOF（不先透传残头） | `eof_inside_partial_frame_is_forwarded_not_swallowed` |
| `recent_len` 按写入位置而非饱和计数 | `token_history_holds_four_slots`（并连带 `token_resample_exhaustion_fails_closed`） |

   验证方式：备份当前 `src` → 单点回退 → 跑对应用例 → 还原备份；六项全部按预期变红。

## 3. 缺陷与修复

### 3.1 A：跨窗口的在途半帧被新探测覆盖（正确性缺陷）

旧实现把“逻辑窗口推进”和“线路上正在写的探测帧”当成同一件事：`close_due_windows()` 每到 15s 就把
探测缓冲替换成新 token 的整帧。写出预算被切碎时（`cut = 1..=16`，即旧帧只写出去 cut 字节）窗口跨过
15s，剩下的 `17-cut` 字节被**新帧整帧顶替**：线路变成“旧帧前缀 + 新帧”，cut=9 时新帧头正好被对端当作
旧帧的 8B payload 解析（`len=8,type=6` 之后再接 8B 随机数据），旧帧尾部永久丢失，且旧探测的 ACK 迟到后
对不上任何在途窗口。

修复（`src/h2_keepalive.rs`）：把两者拆成两个独立状态。

- `inflight: Option<Probe>`：已经碰过线路、尚未写满的帧。`push_probe()` 先把它排空，排空前**任何**其他
  字节（包括新窗口探测与上层 h2 字节）都不交给底层：`poll_write` 此时返回 `Poll::Pending`，
  `poll_flush` 也保持 `Pending`（不谎报“已全部 flush”）。
- `pending: Option<Probe>`：当前窗口尚未上线的探测。只在 `outbound_boundary()` 成立且 `inflight` 为空时
  出队；`open_window()` 只替换 `pending`，绝不触碰 `inflight`。
- 从未上线的过期探测按窗口**丢弃**（`pending` 被新窗口替换），因此不积压历史窗口；旧帧写完后最多再发
  “当前仍有效窗口”那一条探测。
- 旧帧对应窗口的 ACK 属迟到 ACK：原样交 h2，不 credit 新窗口（`in_flight_old_probe_ack_does_not_credit_new_window`）。
- 失败计数与窗口推进不受影响：旧窗口照常计 1 次失败（`windows_opened`/`consecutive_failures` 与
  `probes_sent` 分离统计）。

### 3.2 B：CSPRNG 失败必须失败关闭（安全缺陷）

旧实现在 `SystemRandom::fill` 返回 `Err` 时退化为“上一个 token 确定性 +1”，把唯一 token 语义变成可预测
序列，等价于放弃 §6.7.1 的隔离性。

- 现在 `next_token()` 只有两条出路：拿到 CSPRNG 的新 token，或**判定 token 源不可用**并
  `fail(Failure::Token)`：置 `failed`/`stopped`、清空定时器与全部探测状态、释放底层 io，并返回去敏错误
  （`io::ErrorKind::Other` + `KeepaliveTokenError`，固定文案“连接保活 token 的 CSPRNG 不可用，已按失败关闭
  终止连接”，不含底层错误细节、不含 token、不含重试线索），由 `is_keepalive_token_error()` 识别，与
  `is_keepalive_timeout()` 互斥。
- 去重重采样（与最近 `TOKEN_HISTORY = 4` 个 token 比较，上限 `TOKEN_RESAMPLE_LIMIT = 16`）耗尽同样按
  token 源不可用处理：历史环**第 4 槽**参与查重，`recent_len` 改为饱和计数后，这条路径才真正可达。
- 随机源可测性：新增私有 `TokenRng` 枚举 + `#[cfg(test)] fn with_token_rng(...)` 构造器，测试注入
  `TestFail`/`TestScript`。**生产路径没有任何绕过 CSPRNG 的开关或常量分支**（`TokenRng::System` 是唯一
  非测试变体，构造器为 `#[cfg(test)]` 私有）。

### 3.3 C：首次错误即释放底层 io（资源/活性缺陷）

旧实现只在“下一次被 poll 时”走一遍 `poll_shutdown`：真实 TCP 上首次 45s 死链错误既没关 fd 也没 Drop
（对端只看到 `WouldBlock`，没有 FIN），第二次 poll 才对端观察到 FIN 但 fd 仍开（只关了写半边），
半关闭的 socket 还能继续收对端字节；底层 `poll_shutdown` 返回 `Pending` 时 shutdown 状态被丢弃，之后
每轮重复调用一次却永不完成。

- 底层改为 `io: Option<S>`。`fail()` 在**判定失败的当次 poll 内** `self.io = None`，靠 `S::drop` 关闭整条
  socket（读写两半 + fd 释放），不依赖 `poll_shutdown`：终止路径**不调用** `poll_shutdown`（它需要被继续
  轮询到完成，而报错后组件不再保留状态机去续它）。
- 失败后统一 sticky：`window_opened_at`/`timer`/`staged`/`pending`/`inflight` 全清，四类 `poll_*` 一律返回
  同一错误（`failure_err()`），不复活、不重复释放。
- 因此 `get_ref()`/`get_mut()`/`into_inner()` 改为返回 `Option<...>`：终态没有底层可借，直接返回 `None`
  而不是 panic。`get_mut()` 文档写明风险：绕过适配器直接写底层会破坏本组件的帧边界与 header block 记账。
- `poll_shutdown`（调用方主动关闭）仍按正常语义推进：先写完在途探测，再 flush + shutdown，`Pending`
  时保留状态等下一轮，绝不丢弃进度。

### 3.4 header block 不可交错（RFC 9113 §4.3 / §6.10，本轮补漏）

**修复前不存在任何 header block 保护**：`outbound_boundary()` 只看“单帧是否结束”（前导剩余、帧头 9B、
payload 剩余计数），HEADERS/PUSH_PROMISE 未置 `END_HEADERS` 时，其 payload 结束点被当成合法注入点。
按 RFC 9113 §4.3（“A sender MUST NOT allow a logical sequence of header blocks to be interleaved with
other frame sequences”）与 §6.10，该序列后续必须紧跟 CONTINUATION 直到 END_HEADERS，中间不得插入任何
其它帧——连接级 PING 也是帧（<https://www.rfc-editor.org/rfc/rfc9113.html#section-4.3>）。

- `out_account()` 现在额外跟踪 `out_frame_type`/`out_frame_flags`/`out_in_header_block`：HEADERS(0x1) 或
  PUSH_PROMISE(0x5) 未置 `END_HEADERS`(0x4) 时进入 header block，CONTINUATION(0x9) 置 `END_HEADERS` 时退出。
- `outbound_boundary()` 增加 `&& !self.out_in_header_block`；DATA 等普通帧边界仍可注入。
- 抑制探测不等于死链：header block 期间窗口照常计失败（超时保护仍在，序列被永久卡住时连接照样按
  §6.7.1 判死）。

### 3.5 Low 四项

1. **`recent_len` 只有 3 个真实槽位**：旧代码用写入位置推长度，环回绕后长度缩水，第 4 个 token 不参与
   查重。改为 `recent_len = (recent_len + 1).min(TOKEN_HISTORY)` 饱和计数，单测
   `token_history_holds_four_slots` 覆盖第 5 次写入回绕后的 A 驱逐/B、E 保留。
2. **EOF 吞 1..8B 残头**：底层 EOF 落在帧头中间时旧实现直接报干净 EOF，吞掉已到达字节。新增
   `flush_partial_inbound_header()`：先把 1..8B 残头按截断语义原样透传，下一次 poll 才报干净 EOF
   （`eof_inside_partial_frame_is_forwarded_not_swallowed` 覆盖 `frag = 1..=8` 与 17B PING 的
   `frag = 10..=16`）。写这条用例时发现**首轮 EOF 用例本身用错了角色**：它用 `server()`，而服务端角色的
   入站方向先吃 24B 客户端前导，1..16B 的“残帧”实际全被前导分支透传，修复回退也测不红（RED 反向验证
   当场暴露）。改为 `client()` 入站（无前导）后用例才真正命中帧头分支；另补
   `eof_inside_inbound_preface_is_forwarded_not_swallowed`（服务端角色前导被截断在 1/7/12/23B 时同样必须
   先透传再报 EOF）。
3. **首次错误延后一轮才关连接**：见 §3.3，现为当次 poll 内释放。
4. **把全零 mock 误称“原生 h2 内部 PING”**：`h2-0.4.20/src/frame/ping.rs:16-17` 的真实常量是
   `SHUTDOWN_PAYLOAD = [0x0b,0x7b,0xa2,0xf0,0x8b,0x9b,0xfe,0x54]`、
   `USER_PAYLOAD = [0x3b,0x7c,0xdb,0x7a,0x0b,0x87,0x16,0xb4]`，**都不是全零**（首轮报告 §4 第 4 组的说法
   有误）。用例改名为 `foreign_token_ping_pair_is_forwarded`，用真实常量并明确标注这是**手工构造的 wire
   帧 mock**（未驱动 h2 库自身的 shutdown 流程）；首轮 §6.3 关于“`advance()` 不触发 `Sleep` waker”的
   因果结论也一并纠正（见 §7 第 3 条）。

## 4. 公开 API 变化（相对首轮报告）

| 项 | 首轮 | 本轮 |
| --- | --- | --- |
| `get_ref` / `get_mut` / `into_inner` | `&S` / `&mut S` / `S` | `Option<&S>` / `Option<&mut S>` / `Option<S>`（终态无底层可交，返回 `None`） |
| token 源不可用 | 静默退化为确定性递增 | `pub struct KeepaliveTokenError` + `pub fn is_keepalive_token_error(&io::Error) -> bool` |
| 随机源测试注入 | 无 | `TokenRng` 私有枚举 + `#[cfg(test)]` 私有构造器（生产不可用） |
| `KeepaliveStats` / `Debug` | 不含 token | 不变（新增终态字段仍去敏，`debug_and_stats_never_expose_token`） |

`new`、`role`、`stats`、`H2Role`、`KEEPALIVE_WINDOW`、`MAX_CONSECUTIVE_WINDOW_FAILURES`、
`H2_CLIENT_PREFACE`、`is_keepalive_timeout` 未变。

## 5. 测试套件（40 项，全部通过）

集成 35 项（`tests/h2_keepalive.rs`）+ 单测 5 项（`src` 内 `#[cfg(test)]`）。底层用
`tokio::io::duplex`/真实 `TcpStream`，时钟用 `tokio::time::pause()` + `advance()`，手工 `Context`
驱动以便逐字校验，全部包在 `tokio::task::unconstrained` 内（原因见 §7 第 2 条）。

1. 逐字透明（6 项，保留）：`client_outbound_starts_with_verbatim_preface`、
   `outbound_frames_are_byte_exact_without_window_advance`、`single_byte_writes_keep_stream_intact`、
   `inbound_preface_and_frames_are_verbatim_for_server`、
   `inbound_fragmented_delivery_is_still_verbatim`、`client_inbound_has_no_preface`。
2. 帧边界注入（4 项，1 项新增）：`probe_emitted_at_frame_boundary_with_expected_shape`、
   `probe_is_never_inserted_inside_a_frame`、`large_frame_payload_is_streamed_not_buffered`、
   **`probe_is_not_inserted_inside_a_header_block`**（HEADERS/CONTINUATION/PUSH_PROMISE 未 END_HEADERS
   时出向零字节；END_HEADERS 之后必须恢复注入，否则保活被永久抑制）。
3. 窗口与 token（3 项，保留）：`token_changes_between_windows`、
   `at_most_one_probe_per_window_no_backlog`、`blocked_write_counts_window_and_no_backfill`。
4. ACK 匹配（7 项，1 项改名）：`matching_ack_clears_failures_and_is_intercepted`、
   `late_ack_does_not_credit_and_is_forwarded`、`duplicate_ack_does_not_refresh`、
   `foreign_ack_and_peer_ping_do_not_credit`（补真实 SHUTDOWN token）、
   `peer_ping_request_is_forwarded_without_extra_ack`、`foreign_token_ping_pair_is_forwarded`、
   `business_traffic_cannot_replace_ping_ack`。
5. A 在途半帧（3 项，新增）：`partial_probe_across_window_finishes_old_frame_first`（`cut = 1..=16`
   全量：前 17B 必须仍是旧帧原样，第 17B 之后才是新帧头，恰好 2 帧、`windows_opened=2`、
   `consecutive_failures=1`、`probes_sent=2`）；
   `partial_probe_across_windows_never_backlogs_dropped_probes`（跨 2 窗只出 2 帧，丢弃的探测不积压）；
   `in_flight_old_probe_ack_does_not_credit_new_window`（旧帧 ACK 原样交付、`intercepted_acks=0`、旧窗口计 1 次失败）。
6. C 终态与所有权（3 项，新增）：`fatal_timeout_releases_inner_before_first_error`（底层
   `poll_shutdown` 返回 `Pending` 的底层：首次错误即 `drops == 1`、`shutdown_calls == 0`、accessor 返回
   `None`、四类 poll 四次 sticky、`into_inner() == None`）；
   `underlying_write_failure_releases_inner_immediately`（写错误 / `Ok(0)` 同样当次释放，错误 `kind` 透传）；
   **`fatal_timeout_over_real_tcp_closes_socket`**（真实 `TcpStream` + 真实对端：`drops == 1`、
   `shutdown_calls == 0`、对端 `read()` 返回 0 即 FIN、终态不复活）。
7. EOF 截断（2 项，新增）：见 §3.5 第 2 条。
8. 死链与调度（2 项，保留）：`three_windows_without_matching_ack_close_underlying_io`、
   `windows_only_advance_when_polled`。
9. shutdown（2 项，保留）：`shutdown_stops_probing_and_backfill`、`shutdown_server_side_stops_probing`。
10. 定时器唤醒（1 项，保留）：`window_deadline_wakes_driver_task_without_background_task`（真实
    `tokio::spawn` 任务只挂在 `poll_read` 上，靠适配器注册的定时器醒来并发出下一窗探测）。
11. 诊断去敏（1 项，保留）：`debug_and_stats_never_expose_token`。
12. 真 h2 互操作（1 项，首轮断言全部保留）：`real_h2_stack_acks_injected_probe_and_survives_windows`
    （`h2::client::handshake` + `h2::server::handshake` 双向各垫一层适配器；注入的 PING 出现在底层出向、
    对端 h2 的 ACK 不出现在交给上层 h2 的字节里、跨 `MAX_CONSECUTIVE_WINDOW_FAILURES + 1` 个窗口持续
    POST/响应、服务端共服务 6 个请求、连接不判死）。
13. 单测（5 项，新增，覆盖 CSPRNG 与终态）：`token_history_holds_four_slots`、
    `token_resample_exhaustion_fails_closed`、
    `csprng_failure_closes_connection_without_deterministic_token`、
    `csprng_failure_mid_window_fails_closed`、`terminal_state_is_sticky_and_accessors_are_safe`
    （终态四类 poll sticky、底层已 drop、`into_inner()` 返回 `None` 不 panic、`Debug` 不含 token）。

## 6. 验证命令与输出

```bash
# 1) 归属测试（同一 target 目录，重复 5 次稳定）
CARGO_TARGET_DIR=target/tunnel-task-4a-fix cargo test -p rsetup-tunnel --test h2_keepalive --locked
#    test result: ok. 35 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
CARGO_TARGET_DIR=target/tunnel-task-4a-fix cargo test -p rsetup-tunnel --lib --locked
#    test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

编译零 warning；集成用例连跑 5 次结果一致（含真实 TCP 用例）。

```bash
# 2) Clippy（收窄到本模块与其测试目标）
CARGO_TARGET_DIR=target/tunnel-task-4a-fix cargo clippy -p rsetup-tunnel --lib --test h2_keepalive \
  --locked -- -D warnings
```

`crates/rsetup-tunnel/src/h2_keepalive.rs` 与 `crates/rsetup-tunnel/tests/h2_keepalive.rs`
**零命中**（含 `io_other_error` 已按建议改为 `io::Error::other`）。整包 clippy 目前仍红：
`src/secure_io.rs` 有 7 条 lint（`collapsible_if`、`redundant_pattern_matching`、`drop_non_drop`），
属并行写者的在写文件，不在本轮归属范围，未代为改动。

```bash
# 3) 格式门禁（本仓口径：逐包，不用 --all；见 AGENTS.md）
cargo fmt --package rsetup-tunnel -- --check
```

本任务两个文件用 `rustfmt --edition 2024` 就地格式化后 `--check` 零差异；整包 `--check` 的剩余差异
全部落在 `tests/secure_io.rs`（他人在写文件），本轮未格式化任何他人文件。

## 7. 本地环境事实（影响测试写法，非本组件缺陷）

1. **tokio 1.53.1 的 `AsyncRead` 用 `ReadBuf`**：子缓冲用 `ReadBuf::new(&mut slice)` +
   `initialize_unfilled_to(n)`/`advance(n)` 配对，交付用 `put_slice`。
2. **tokio 协作调度预算**：`tokio::io::duplex` 的 `poll_read`/`poll_write` 包在 `cfg_coop!` 内，单个任务
   片内约 128 次 poll 后即使缓冲空闲也返回 `Poll::Pending`（并登记 waker）。手工 `Context` 驱动的测试因此
   统一包在 `tokio::task::unconstrained(...)` 内；高层 `async` API 在每次 yield 时恢复预算，不受影响。
3. **暂停时钟下 `advance()` 不唤醒定时器，是观测方式造成的，不是 Tokio 语义**（纠正首轮报告 §6.3 的
   过强因果结论）：观测到的现象是“`pause()` 时钟 + 当前线程任务从不 park”时 time driver 没有机会处理
   到期定时器，`Sleep` 不 Ready，`park_driver()` 让线程真正 park 一轮后才 Ready。正常 Tokio 语义里
   `pause()` 会立即处理 `advance()` 后的到期定时器；`block_on` 本身不重置 coop 预算
   （`current_thread/mod.rs`、`park.rs` 仍在 `coop::budget` 内）。因此 `park_driver()` 只是本地观测的
   变通，不能作为组件缺陷或环境缺陷的依据。
4. **不拿 `fcntl(fd)` 当“socket 已关”的判据**：真实 close 可能由 runtime 的 IO driver 在后续轮次收尾，
   且 fd 号会被之后打开的 fd 复用，同一断言在连跑时会时红时绿。真实 TCP 用例改用两个确定证据：
   底层包装的 `Drop` 计数（所有权当场释放，只关半个方向时该计数为 0）+ 对端 `read()` 返回 0（FIN）。
5. **并行写者的中间态**：本轮期间 `cargo test -p rsetup-tunnel`（全包）会因 `src/secure_io.rs` 等文件的
   在写状态与慢用例失败或长时间不返回；本任务只跑归属目标（`--lib`、`--test h2_keepalive`），
   整包测试与整包 lint 的红点归属已在 §6 标明。

## 8. 未覆盖与边界（诚实声明）

1. **未与真实 `SecureIo` 组装**：全部验证在 `tokio::io::duplex`、手工 `Context` 与本地回环
   `TcpStream` 上完成；未接真实 TLS/`SecureIo`、真实 RTT、真实丢包与半关闭链路，也没做双进程/双
   主机联测。组件与后续会话层的装配（把 `H2KeepaliveIo` 垫进 `SecureIo` 与 h2 会话之间）**本轮未做、
   也不声称已做**。
2. **header block 约束只用手工 wire 帧验证**：证明的是“适配器不会在 HEADERS/CONTINUATION/PUSH_PROMISE
   序列内部插帧”，以及真 h2 用例中出向帧仍可被完整解析；没有让 h2 库真的发出未 END_HEADERS 的分片
   header block（h2 0.4.20 的公开 API 不会这样发帧），因此这条约束在真实 h2 下属于防御性保障。
   反向约束（收到被交错的 header block 时报错）属 h2 职责，未测。
3. **真实半写 socket 未构造**：A 的半帧用固定写出预算的 mock 底层覆盖 `cut = 1..=16` 全部切点；
   真实 TCP 只用于验证关闭语义（§3.3、§5 第 6 组）。
4. **CSPRNG 失败用测试专用注入**：`ring::rand::SystemRandom` 的真实失败无法从公开接口注入，用
   `#[cfg(test)]` 的 `TokenRng::TestFail`/`TestScript` 覆盖；生产路径没有对应开关，但也意味着该分支的
   真实触发频率未测（`/dev/urandom` 不可用属极端环境）。
5. **“无后台 task”**：依据模块内零 `spawn`/零线程 + “只 `advance` 不 poll 时窗口不推进”
   （`windows_only_advance_when_polled`）两条证据，未对运行时任务数做数值断言。
6. **h2 互操作范围**：只覆盖客户端/服务端各一条连接上的 POST 文本体与多窗口存活；未覆盖并发多流、
   FLOW_CONTROL 压力、GOAWAY 排空，以及 h2 自身 shutdown 阶段的 PING 时序（只验证了保留 token 的
   PING/ACK 不被改写或吞掉）。
7. **入站不囤积**：由代码结构（入站只维护帧头 9B + 定长 17B 检视缓冲，payload 流式转发）与分片交付、
   0 长帧、96 KiB 大帧用例支撑，属结构性保证，未做“声明长度极大但只喂部分 payload”的专门数值断言。

# Tunnel Task 1（修复第 1 轮）：方向化记录层与安全记录 I/O 实施与测试报告

本轮是 Task 1 的**审查后修复第 1 轮**：针对独立审查给出的 5 组反例（R1–R5）与 1 组文档口径问题逐条补齐可执行证据。上一轮实现（buffered 写、`Option<S>` 所有权释放、逐记录 deadline）已在工作树中，本轮**复核其语义、修复静态门禁与测试驱动缺陷，并以变异验证证明每条要求真的被测试咬住**。

独占涉及文件（本轮实际改动，未触碰其他写者模块）：

- `crates/rsetup-tunnel/src/secure_io.rs`
- `crates/rsetup-tunnel/tests/secure_io.rs`
- `crates/rsetup-tunnel/Cargo.toml`（仅注释口径）
- `crates/rsetup-protocol/src/crypto.rs`
- `crates/rsetup-protocol/tests/crypto.rs`
- `crates/rsetup-protocol/Cargo.toml`（feature 注释口径 + 去掉文件末尾空行）
- 本报告

未执行 `git commit` / `git push`，未执行 `cargo clean`，未起后台任务，实现内无 `unsafe`。构建目录按派发指令使用 `CARGO_TARGET_DIR=target/tunnel-task-1-fix`；R5 另用两个空目录（`target/tmp-r5-probe`、`target/tunnel-task-1-prodbuild`）复验，以排除上一轮遗留的陈旧构建产物干扰（见 §7.2）。

## 1. 本轮收到的 RED 与复现

### 1.1 派发方给出的编译失败（RED，已复现）

```
CARGO_TARGET_DIR=target/tunnel-task-1-fix timeout 90s cargo test -p rsetup-tunnel --test secure_io --locked -- --test-threads=1
```

`exit=101`：

```
error[E0277]: `h2::server::Connection<'_, ..., Bytes, &mut SecureIo<tokio::io::DuplexStream>>` is not a Future
    --> crates/rsetup-tunnel/tests/secure_io.rs:1742
```

根因：`h2::server::Connection` 实现的是 `futures_core::Stream`，不是 `Future`（`h2-0.4.20/src/server.rs`：`impl<S, B, T> Stream for Connection<S, B, T>`，另有 `pub fn poll_closed(&mut self, cx) -> Poll<Result<(), Error>>`）。把它直接交给 `tokio::time::timeout` 无法编译。修复方式见 §4。

### 1.2 审查方反例基线（同一目录内的只读反例二进制）

`target/tunnel-review-1/task1-readonly-counterexamples`（08:55 编译，静态链接**修复前**的 `rsetup-tunnel`）`--list` / 直接运行输出 6 条观察结果，本轮作为 RED 基线引用（**它链接的是修复前的源码副本，其输出不代表当前构建**）：

```
R1 real duplex: cancelled write(OLD) returned Pending after 8 wire bytes; write(NEW) reported Ok(3), decrypted OLD
R1 flush real duplex: Pending write accepted 0 reported bytes; flush sent complete OLD and advanced tx_seq=1
R2 real TCP: AuthFailed sent FIN (write-half shutdown), transport still retained; peer write after fatal accepted
R3 Pending shutdown: last-seq write Ready(3); next write/read never drive close; explicit flush Ready(Ok) despite shutdown still Pending
R4 paused real duplex: r0 starts t=0, completes t=29 with first byte r1; r1 fatals t=31 after only 2s (should allow until t=59)
R5 normal library build: public *_at_seq_for_test callable without cfg(test), initial seq=(123,456); consumes keys, no key export found
```

这 6 条在本轮全部变成仓库内的常驻回归用例（§2）+ 变异验证（§3），不再依赖该二进制。

### 1.3 修好编译后的第二个 RED：h2 用例挂死

编译修复后 `cargo test -p rsetup-tunnel --test secure_io`：30 个用例中 29 个秒过，`real_h2_client_and_server_interoperate_over_secure_io` 挂死 >60s。定位过程与结论见 §4——**根因在测试驱动，不在 `SecureIo`**。

## 2. R1–R5 逐条对账（要求 → 落点 → 用例）

| # | 审查要求 | 实现落点（`crates/rsetup-tunnel/src/secure_io.rs`） | 常驻用例 |
| --- | --- | --- | --- |
| R1 | `Pending` 之后取消 + 换 buffer / flush，都不得让"未接受的字节"上线 | `poll_write` 顺序固定：① 先 `drive_tx` 推在途记录，推不动 ⇒ 武装本条记录计时后 `Pending`（本次 0 接受）；② 空写入 `Ready(Ok(0))`；③ `accept_room()`（在途记录存在即为 0）；④ 只把 `data[..n]` 复制进内部 `buffered`，**不保留调用方 buffer 的任何引用**；⑤ 立即 `drive_tx`。`accept_room()` 是"最多再接受一条满记录"的闸门 ⇒ "记住被取消的那次入参"在结构上无处存放 | `cancelled_pending_write_cannot_be_answered_with_another_buffer`（真实 duplex + `abort()` 真实取消 + 线上顺序与逐条记录内容）、`flush_after_pending_write_sends_only_accepted_bytes`、`caller_may_reuse_or_resize_buffer_after_ready_accept`、`buffered_bytes_never_exceed_record_budget` |
| R2 | 致命必须同步 drop **整条底层 TCP**，不是只 shutdown 写半边 | `terminate()`：置 sticky 原因 → 作废在途记录/缓冲/重组器/序号计时 → `self.pair = None`（先作废密钥）→ `self.io = None`（**所有权释放，底层对象当场 drop**）→ 唤醒 park 中的对向 waker | `fatal_over_real_tcp_releases_socket_and_closes_connection`（`WatchedTcp` 的 `dropped` 必须为真；对端读到 EOF；对端继续写最终失败）、`bad_tag_delivers_no_plaintext_and_terminates_both_directions`、`underlying_write_failure_is_fatal_and_sticky`、`receive_side_sequence_exhausted_terminates_both_ways`、`fatal_terminates_parked_other_direction`、`dropping_secure_io_drops_underlying_transport` |
| R3 | 末有效序号 flush/shutdown 遇 `Pending` 必须真驱动，不得假报 `Ready` | `drive_close()` 状态机 `Flushing → ShuttingDown → Done`，任一 `poll_flush`/`poll_shutdown` 返回 `Pending` 就 `return Poll::Pending`；`poll_read`/`poll_write`/`poll_flush`/`poll_shutdown` **四个入口都会继续驱动它**（不假设"写完一定还会被 poll 写方向"）；`Done` 且处于 `closing` ⇒ 释放底层 | `last_sequence_close_propagates_pending_shutdown`（底层 `stall_shutdown` ⇒ flush/shutdown 持续 `Pending`，waker 到位后才 `Ready`）、`close_completes_from_read_side_without_another_write`、`last_valid_sequence_flushes_and_shuts_down_without_another_write`、`shutdown_flushes_buffered_record_then_closes` |
| R4 | 逐记录独立 30s：t0 record0 首字节、t29 record0 完整 + record1 首字节，record1 必须活到 ≈t59 | `poll_read` 用 `rx.reassembler.read_limit(...)` 把单次读上限夹在当前记录边界内；chunk 内凑齐记录（`crossed_record`）⇒ **立刻撤除旧 deadline**，随后 `arm_read_deadline` 为下一条记录的首字节重新锚定 30s；碎片不刷新（`if self.rx.deadline.is_none()`）；无首字节不启动；写方向 `arm_write_deadline` 同口径自首次受阻起算 | `read_deadline_reanchors_per_record_even_within_one_chunk`（t=0 起 record0，t=29 同一 chunk 完成 record0 并带来 record1 首字节，t=31 仍 `Pending`，实际到点在 [59s, 60s]）、`read_partial_deadline_is_30s_from_first_byte_and_not_reset`、`no_first_byte_starts_no_partial_read_deadline`、`blocked_write_deadline_is_30s`、`blocked_write_deadline_arms_at_first_pending`、`read_deadline_wakes_a_parked_reader_task`、`write_deadline_wakes_a_parked_writer_task` |
| R5 | `*_at_seq_for_test` 在普通生产构建不可见 | `crates/rsetup-protocol/src/crypto.rs`：两个函数为 `#[cfg(feature = "test-util")] pub fn`（feature 默认关闭，`crates/rsetup-protocol/Cargo.toml`）；仅单元测试用的私有 `tx_at_seq` / `rx_at_seq` 是 `#[cfg(test)]`；模块文档用 `#![cfg_attr(not(feature = "test-util"), doc = ...)]` 挂一段 `compile_fail` 文档测试——把它改回无条件 `pub`（哪怕加 `#[doc(hidden)]`）该用例立刻编译成功 ⇒ 变红 | 符号级 + doctest 级证据见 §5 |

R1/R2/R3/R4 的实现要点均在同文件模块注释中与条款对齐；本轮复核确认 `secure_io.rs` 内对 `seal()` 的调用数为 0（写路径只用 `prepare`/`commit`），"Pending 的那次写入被后续另一次写入静默替代"没有可存放的状态。

## 3. 变异验证（证明用例真的会失败）

方法：把被修复的缺陷**临时植入实现**，跑同一套用例，确认变红，再逐字节还原（`md5sum` 校验 `secure_io.rs=4bf299bb…`、`crypto.rs=2754a1fd…`，与变异前一致）。命令：`CARGO_TARGET_DIR=target/tmp-mut-verify cargo test -p rsetup-tunnel --test secure_io --locked -- --test-threads=1`。

| 变异 | 变红用例（同批 31 个用例） |
| --- | --- |
| M1 `accept_room()` 无视在途记录（被 `Pending` 的写入偷偷接受，之后与新 buf 混封） | `cancelled_pending_write_cannot_be_answered_with_another_buffer`、`flush_after_pending_write_sends_only_accepted_bytes`、`blocked_write_deadline_is_30s`、`write_deadline_wakes_a_parked_writer_task` → `27 passed; 4 failed` |
| M2 `terminate()` 不 `self.io = None`（只作废状态、不释放底层） | `fatal_over_real_tcp_releases_socket_and_closes_connection`、`bad_tag_delivers_no_plaintext_and_terminates_both_directions`、`receive_side_sequence_exhausted_terminates_both_ways`、`underlying_write_failure_is_fatal_and_sticky` → `27 passed; 4 failed` |
| M3 收尾 `poll_shutdown` 的 `Pending` 被吞（假报收尾完成） | `last_sequence_close_propagates_pending_shutdown`、`close_completes_from_read_side_without_another_write` → `29 passed; 2 failed` |
| M3b `poll_flush` 忽略"半条记录没推完" | `flush_after_pending_write_sends_only_accepted_bytes`、`pending_write_reentry_does_not_reseal_or_reuse_nonce` → `29 passed; 2 failed` |
| M4 跨记录不撤旧 deadline | `read_deadline_reanchors_per_record_even_within_one_chunk` → `30 passed; 1 failed` |
| M4b 每次 poll 都重锚读 deadline（碎片刷新期限） | `read_partial_deadline_is_30s_from_first_byte_and_not_reset` → `30 passed; 1 failed` |
| M7 写受阻重入时清零已写偏移（同一条记录重复上线 / nonce 复用） | `pending_write_reentry_does_not_reseal_or_reuse_nonce`、`cancelled_pending_write_cannot_be_answered_with_another_buffer`、`duplex_roundtrip_fragmented_mixed_and_oversized`、两个真实 h2 用例 → `26 passed; 5 failed` |
| M5 去掉 `*_at_seq_for_test` 的 feature 门禁 | `cargo test -p rsetup-protocol --doc --locked` → `crypto (line 40) - compile fail ... FAILED`，`Test compiled successfully, but it's marked 'compile_fail'` → `2 passed; 1 failed` |

两条经验记录：

1. **无效变异**：把 `poll_write` 里"缓冲无空间 ⇒ `Pending`"的分支单独短路掉不会让 R1 用例变红——因为正常路径下"在途记录未推完"先在第 ① 步返回 `Pending`，那个分支几乎不可达。真正咬住 R1 的是 `accept_room()`（M1）。
2. M5 需要在源文件改完后**留出一个可被 cargo 观察到的时间差**再跑：同一秒内"还原 ⇒ 改回"会让 cargo 判定 lib 为 `Fresh`、复用未含新符号的 rlib，从而出现"变异未变红"的假绿。手工复跑（日志中可见 `Compiling rsetup-protocol`）结果如上表。

## 4. 真实 h2 互操作用例的重写与根因

原用例把服务端写成"同一条任务里 `accept()` 后 inline `await body.data()`，最后 `timeout(.., h2)`"。两处都是测试自身的缺陷：

1. `timeout(.., h2)` 不成立（`Connection` 是 `Stream`）⇒ 编译失败（§1.1）。
2. 更本质的：**`h2::server::Connection` 只在被 poll 时才推进内部状态**（h2 文档明确要求调用方持续 `poll_accept`/`poll_close`）。在同一任务里 inline `await` 某条流的 body，那段等待期间没有任何人驱动连接；而底层 tokio `DuplexStream`（`SimplexStream`）的 `read_waker` 是**单槽**且在唤醒时 `take()`（`tokio-1.53.1/src/io/util/mem.rs`），于是"分片到达的下一条记录"永远读不上来 ⇒ 永久停摆。

对照实验（同一轮排查中用临时用例验证，之后删除）：

- 裸 `tokio::io::duplex(16 * 1024)`（**完全不套 `SecureIo`**）+ inline 服务端 ⇒ 同样停摆（`served=999`，`srv poll_closed = Err(elapsed)`）；
- 同样的裸 duplex ⇒ 改成"每条流 spawn 处理 + `accept()` 循环独占驱动" ⇒ 正常；
- `SecureIo<DuplexStream>` + spawn-per-stream ⇒ `duplex(16K)` 与 `duplex(64)` 均 `served=3 done=3`、`srv poll_closed = Ok(Ok(()))`。

结论：**这是测试驱动问题，不是 `SecureIo` 的写方向缺陷**。现在的用例（`crates/rsetup-tunnel/tests/secure_io.rs` §17）：

- 服务端 `accept()` 循环是这条连接唯一的驱动点，每条流交给 `tokio::spawn(echo_stream(..))`（含 `release_capacity`，否则 h2 自己的流控窗口会先卡死）；
- 收尾显式断言 `poll_closed` 在 500ms 内 `Ok(Ok(()))`（GOAWAY 与最后一条响应必须真的 flush 上线，不允许靠 drop 的副作用）；
- 客户端 `tokio::io::split` + `join` 包装后把 `Connection` future 交给独立任务驱动，`drop(send_req)` 触发优雅 GOAWAY；
- 整个往返包在 `timeout(30s)` 里：**一旦写方向出问题必须是用例失败，而不是挂死**；
- 两个变体：`real_h2_client_and_server_interoperate_over_secure_io`（`duplex(16 KiB)`，3 条流，请求体 1 B / 20 001 B / 40 001 B ⇒ 跨多条记录、每条记录都停在记录中间再重入）与 `real_h2_interoperates_under_extreme_backpressure`（`duplex(64 B)`，比一条记录小三个数量级）。

## 5. R5 门禁的可执行证据与 Cargo.toml 口径修正

实测（`CARGO_TARGET_DIR=target/tunnel-task-1-fix`，当前源码）：

| 命令 | 结果 |
| --- | --- |
| `cargo build -p rsetup-protocol --locked` + `nm --defined-only <debug rlib> \| grep -c at_seq_for_test` | `96216b67d57eccb9.rlib` → **0** |
| `cargo build -p rsetup-protocol --features test-util --locked` + 同上 | `d13c9c5f32d8a73d.rlib` → **2**（`client_…` / `server_…`） |
| `cargo build --release -p rsetup-protocol --locked` + 同上 | `00a5ec5038ca9b5e.rlib` → **0** |
| `cargo test -p rsetup-protocol --doc --locked`（生产 feature 组合） | 3 passed，其中 `crypto (line 40) - compile fail ... ok`（引用 `*_at_seq_for_test` 必须编译失败） |
| `cargo test -p rsetup-protocol --locked -- --list \| grep -c test_seq_constructor` | **0**（默认组合不编译该用例） |
| `cargo test -p rsetup-protocol --locked --features test-util -- --list \| grep -c test_seq_constructor` | **1** |
| `cargo test -p rsetup-protocol --tests --locked -v`（不带 `--features`） | 首轮 writer 曾报告启用，但独立空 target 复审仅见 `--check-cfg` 允许值、无实际 `--cfg feature="test-util"`，列出默认 21 项；首轮结论撤回 |

feature 统一取决于选中的构建根：tunnel/workspace 测试目标的 dev-dependency 或显式 `--features test-util` 可启用测试入口，不能笼统声称“所有默认 cargo test 都有”或“所有默认 cargo test 都无”。以本次 rustc 实际 `--cfg` 与测试列表为准。生产 build+nm=0、外部引用 E0425 和默认 doc compile_fail 的独立证据成立；该文案更正不推翻 R5 门禁。协议清单注释已按此收口。

补充：`crates/rsetup-protocol/src/crypto.rs` 模块注释里的 `compile_fail` 用例是**反向门禁**（若有人把入口改成无条件可见，它就变红），而不是"用注释代替验证"。

## 6. GREEN 证据（全部命令与结果）

```
CARGO_TARGET_DIR=target/tunnel-task-1-fix cargo test -p rsetup-tunnel --test secure_io --locked                 # ×3
CARGO_TARGET_DIR=target/tunnel-task-1-fix cargo test -p rsetup-tunnel --test secure_io --locked -- --test-threads=1
CARGO_TARGET_DIR=target/tunnel-task-1-fix cargo test -p rsetup-tunnel --lib --locked
CARGO_TARGET_DIR=target/tunnel-task-1-fix cargo test -p rsetup-protocol --locked                                # ×2
CARGO_TARGET_DIR=target/tunnel-task-1-fix cargo test -p rsetup-protocol --locked --features test-util
```

| 目标 | 结果 |
| --- | --- |
| `rsetup-tunnel --test secure_io`（默认并行，连跑 3 次） | `test result: ok. 31 passed; 0 failed`（每次 0.08s） |
| `rsetup-tunnel --test secure_io -- --test-threads=1` | `31 passed; 0 failed`（0.24s） |
| `rsetup-tunnel --lib`（含 `secure_io` 内单元测试） | `6 passed; 0 failed` |
| `rsetup-protocol`（默认 feature 组合，全套） | `191` 条 `ok`，`0 failed`（`lib` 14、`tests/crypto.rs` 21、`handshake_msg` 46、`codec` 34、`handshake_adapter` 14、`sig` 12、`frame_sequences` 9、`ed25519`/`frame` 各 8、`admission_state` 5、Doc-tests 3） |
| `rsetup-protocol --features test-util` | `191` 条 `ok`，`0 failed`，含 `test_seq_constructor_still_consumes_session_keys ... ok` |

31 个 `secure_io` 用例（本轮 30 → 31，新增极端背压 h2 变体）：`bad_tag_delivers_no_plaintext_and_terminates_both_directions`、`blocked_write_deadline_arms_at_first_pending`、`blocked_write_deadline_is_30s`、`buffered_bytes_never_exceed_record_budget`、`caller_may_reuse_or_resize_buffer_after_ready_accept`、`cancelled_pending_write_cannot_be_answered_with_another_buffer`、`clean_eof_after_last_record_is_ok`、`close_completes_from_read_side_without_another_write`、`constructed_over_fatal_pair_is_fatal_immediately`、`dropping_secure_io_drops_underlying_transport`、`duplex_roundtrip_fragmented_mixed_and_oversized`、`empty_record_is_not_eof_and_data_after_it_is_delivered`、`fatal_over_real_tcp_releases_socket_and_closes_connection`、`fatal_terminates_parked_other_direction`、`flush_after_pending_write_sends_only_accepted_bytes`、`flush_drains_buffered_record`、`last_sequence_close_propagates_pending_shutdown`、`last_valid_sequence_flushes_and_shuts_down_without_another_write`、`no_first_byte_starts_no_partial_read_deadline`、`oversized_and_undersized_prefix_rejected_immediately`、`pending_write_reentry_does_not_reseal_or_reuse_nonce`、`read_deadline_reanchors_per_record_even_within_one_chunk`、`read_deadline_wakes_a_parked_reader_task`、`read_partial_deadline_is_30s_from_first_byte_and_not_reset`、`real_h2_client_and_server_interoperate_over_secure_io`、`real_h2_interoperates_under_extreme_backpressure`、`receive_side_sequence_exhausted_terminates_both_ways`、`shutdown_flushes_buffered_record_then_closes`、`truncated_record_is_fatal_not_eof`、`underlying_write_failure_is_fatal_and_sticky`、`write_deadline_wakes_a_parked_writer_task`。

另有编译期用例 `secure_io_is_async_read_write_and_unpin`（`assert_io::<SecureIo<DuplexStream>>()` / `<MockIo>`，编译即门禁，不计入 31）。

## 7. 静态门禁

```
cargo clippy -p rsetup-protocol --all-targets --locked -- -D warnings     → Finished，无诊断
cargo clippy -p rsetup-tunnel --lib --locked -- -D warnings               → Finished，无诊断
cargo clippy -p rsetup-tunnel --test secure_io --locked -- -D warnings    → Finished，无诊断
rustfmt --edition 2024 <本任务 4 个 .rs 文件>（就地）                       → 无差异
```

本轮为通过 `-D warnings` 对实现做的**等价重构**（无语义变化，测试同批通过）：

- 删除 `drop(read_buf)`（`ReadBuf` 不实现 `Drop`，`clippy::drop_non_drop`），借用由 NLL 自然结束；
- `if let Poll::Pending = x? { … }` → `if x?.is_pending() { … }`（3 处，`clippy::redundant_pattern_matching`）；
- 两处 `if … { if let Poll::Ready(Err(e)) = … }` 折叠为 2024 edition 的 let-chain；`poll_write` 第 ① 步的嵌套 `if` 折叠为单条件（短路顺序不变：`drive_tx` 仍先于 `accept_room()` 求值）；
- 测试文件：`polls % n == 0` → `polls.is_multiple_of(n)`；空写入改为 `assert_eq!(a.write(&[]).await?, 0, "空写入不得接受任何字节")`（既消 lint 也强化断言）。

### 7.1 归属范围外的红（未修改，移交对应写者）

`cargo clippy -p rsetup-tunnel -p rsetup-protocol --all-targets -- -D warnings` 与 `cargo fmt --package rsetup-tunnel --package rsetup-protocol -- --check` 目前**仍红**，全部命中不在本轮独占范围的文件（本轮结束时的观测，并行写者仍在改动，名单是动态的）：

- clippy `-D warnings`：`crates/rsetup-tunnel/tests/session_streams.rs`（`collapsible_if`）。
- `cargo fmt`：`crates/rsetup-tunnel/src/session/mod.rs`（`pub mod streams;` 与 `pub mod types;` 顺序）、`crates/rsetup-tunnel/tests/h2_wire_control.rs`（11 处）、`crates/rsetup-tunnel/tests/session_state_review_tmp.rs`（35 处，看名字是他人遗留的临时评审用例）。

本轮未使用 `cargo fmt --package`（会连带格式化上述他人文件），改用逐文件 `rustfmt`。按 `AGENTS.md`，仓库格式门禁是 `make fmt-check`，不使用 `cargo fmt --all`。

### 7.2 陈旧构建产物会造成假失败（重要陷阱）

在本轮开始前，`target/tunnel-task-1-fix` 内仍留有上一位写者**修复前源码**编译出的 `librsetup_protocol-*.rlib`（含未加门禁的 `*_at_seq_for_test`）。`cargo test -p rsetup-protocol --doc` 曾因此报：

```
---- crates/rsetup-protocol/src/crypto.rs - crypto (line 40) stdout ----
Test compiled successfully, but it's marked `compile_fail`.
```

在空 target 目录（`target/tmp-r5-probe`）复跑同一命令得到 `compile fail ... ok / 3 passed`，并在 `target/tunnel-task-1-fix` 内以当前源码重新构建后同样通过。**这不是 R5 门禁失效，而是 cargo 复用了陈旧 rlib**；后续复现 R5 证据时请先确认 `nm` 计数与 `Compiling rsetup-protocol` 确实发生。

### 7.3 并行写者造成的临时不可编译

本轮期间两类临时红，均一律未触碰：

1. `cargo test -p rsetup-tunnel`（全目标）一度因他人模块报 `crates/rsetup-tunnel/src/handshake.rs` 内 `cannot find type FrameReader` / `ADMISSION_RECHECK_DELAY` / `send_frame`；另见 `tests/handshake.rs`、`tests/identity.rs` 被并行写者删除（`git status` 显示 ` D`）。
2. 12:02 前后 `crates/rsetup-controller/Cargo.toml` 新增 `rsetup-tunnel` 依赖（`+rsetup-tunnel = { path = "../rsetup-tunnel" }`），在其本人刷新锁文件之前的几分钟内，**整个 workspace 的 `--locked` 命令**都报 `error: cannot update the lock file Cargo.lock because --locked was passed to prevent this`。本轮未改该清单、也未改写 `Cargo.lock`（用 `cargo metadata` 试解析后与刷新前逐字节 `diff` 为空）。稍后该窗口令被其本人刷新，父代理的原始命令随即恢复通过（见 §6 末次复核）。

本报告的所有通过结论只依据 §6 列出的目标（`--test secure_io`、`--lib`、`-p rsetup-protocol`、以及 §7 的三个 clippy 目标）。

## 8. 上一轮遗留结论的更订

以下条目在旧报告中成立、本轮之后需要更新，已在正文按新事实重写：

1. 用例数 22 → **31**（新增 R1/R2/R3/R4 反例用例、2 个真实 h2 用例、`buffered_bytes_never_exceed_record_budget` 等）。
2. 旧报告"`cargo clippy -p rsetup-protocol -p rsetup-tunnel --all-targets` 本任务文件 0 命中"当时不成立：`secure_io.rs` 自身有 7 处 `-D warnings` 报错，`tests/secure_io.rs` 有 3 处，本轮已全部消除（见 §7）。
3. 旧报告称"`tests/h2_keepalive.rs` 是 `--all-targets` 唯一的红"；当前红点变成 `session_streams.rs` / `src/session/mod.rs`（§7.1），且 `h2_keepalive.rs` 的状态由该模块写者负责。
4. 旧 §3.3 表格中"尽力 `poll_shutdown` 底层"的描述与实现不符：实现是**先作废密钥再 `self.io = None` 释放底层**（R2），本轮按实现更正。
5. 旧报告"未改动任何 `Cargo.toml`"不再成立：本轮按审查要求改了两处 feature 注释口径与一个 EOF 空行（§5）。

## 9. 未覆盖与后续关注

1. **未接入生产接线**：`SecureIo` 目前只在测试里直接承载 h2（`SecureIo<OwnedTcpStream>` 的真实链路属 Task 2）。因此"生产会话跑在 SecureIo 上"尚未经过端到端验证。
2. **网络层真实性**：只用了 `tokio::io::duplex`（含 64 B 极端背压）与 127.0.0.1 loopback TCP；未覆盖真实链路 RTT、MTU/中段丢弃、`TCP_NODELAY` 组合与内核重传下的表现。
3. **h2 覆盖广度**：只覆盖 3 条流的 POST echo（含 40 001 B 大 body 跨记录）。未覆盖并发流控窗口整体耗尽、`RST_STREAM`、半路 `GOAWAY`、h2 侧超时与记录层 deadline 的相互作用。
4. **deadline 走虚拟时钟**：30s 相关用例用 `tokio::time::pause()` + 手工 `poll`，确定性好但未验证真实时钟（系统挂起、`Instant` 跳变）下的行为。
5. **R5 的产物级核对**：以 rlib 符号计数 + `compile_fail` 文档测试为证据，未对最终发布二进制（`rsetup-agent` / `rsetup-server`）做 `nm` 级核对（属打包任务）。
6. **序号末值路径依赖测试组合**：末有效序号 / 期望序号 `2^64-1` 的构造需要 `test-util`；生产构建里这条路径只能由真实累计到达，无法在默认 feature 组合下直接触发。
7. **`AdapterError::BufferLimit` 仍保留但不再产生**（需与 `crates/rsetup-tunnel/src/handshake.rs` 的穷尽 match 一并删除，该文件非本轮独占）。
8. **`TxRecordLayer::seal()` 仍在公共 API 上**（供库内测试与握手一次性发送，文档明确禁止 SecureIo 生产写使用；SecureIo 内 0 调用）。若要编译期禁止需牵动他人模块。
9. **本报告只含去敏结果**：全部密钥/随机量为合成常量 fixture（`0x5a` / `0x11` / `0x22`），不含真实凭据、地址或业务数据。

## 10. 公开 API 变更清单（相对 `HEAD`）

`rsetup-protocol::crypto`：

- `RecordLayer` 拆分为 **`TxRecordLayer`**（`seal` / `prepare` / `commit` / `abort_pending` / `mark_write_failed` / `is_fatal` / `tx_seq`）与 **`RxRecordLayer`**（`open` / `is_fatal` / `rx_seq`）；`RecordLayerPair { tx, rx }` 为唯一出口（方向能力无法重新长出，由 2 个 `compile_fail` doctest 把守）。
- 新增 **`PreparedRecord`**（`wire()` / `seq()`）+ `PendingOwner` 状态位：`prepare` 预留序号、`commit` 才推进；未 commit 即 drop ⇒ 整对熔断（§5.2"写出结果不确定即关闭整条 TCP"）。
- 新增错误码 `CryptoError::PendingOutstanding`（同一序号只允许一份 pending）与 `CommitMismatch`。
- **删除密钥导出** `KeyMaterial` / `SessionKeys::key_material_for_test`；删除 `record_layer_at_seq`；构造入口改为消耗 `SessionKeys` 的 `into_client_record_layers` / `into_server_record_layers`。
- `client_record_layers_at_seq_for_test` / `server_record_layers_at_seq_for_test` 收进默认关闭的 `test-util` feature（R5）。

`rsetup-tunnel::secure_io`（本 crate 尚未进入 git，以下为当前公开面）：`RECORD_IO_DEADLINE`（30s）、`SecureIo<S>`（`new` / `is_fatal` / `tx_seq` / `rx_seq` / `pair` / `tx_buffered_len` / `is_write_closed`，实现 `AsyncRead + AsyncWrite + Unpin`，无后台任务）、`SecureIoFailure`（`AuthFailed` / `Framing` / `Truncated` / `ReadDeadline` / `WriteDeadline` / `SequenceExhausted` / `PairFatal` / `WriteFailed` / `Underlying` / `Closed`）、`secure_io_failure(&io::Error) -> Option<SecureIoFailure>`。错误经 `io::Error::other(SecureIoFailure)` 承载，`Display` 只输出固定英文短语，不含明文、密钥、序号或对端数据。


## 10. Fix round2（主代理修复，独立复审待完成）

首轮31tests虽绿，备审独立发现F1末seqdrive后接受NEW并flush假成功（P1）、F2write早退/read吞close Pending（P2）、F3read驱动写不检查写deadline（P2）。不是把原R1取消缺陷全部归为未修，R2/R4/R5独立证据仍成立。

主代理先补RED：NEW被接受3B、read t31仍Pending分别真实失败；起初测试调用MockIo::new多传一个参数的E0061已修为with_stall，夹具编译错不算语义RED。实现以共同drive_tx入口poll原写Sleep，保证read也登记/核期限；drive_started_close统一close Pending传播；write在drive旧记录前后都判关闭状态，已接受本次n后必须Ready(n)而不把close Pending当未接受。read正容量时先等待已有close完成，不再用EOF掩盖Pending；零容量read不消费/不驱动close，fatal仍优先。

| 状态 | 正容量read | write（未接受本次入参） | flush/shutdown |
| --- | --- | --- | --- |
| Open/有在途记录 | drive_tx核写期限，可读其它方向 | 先drain；无容量则Pending不消费 | drain Pending如实传播 |
| 旧记录刚提交末seq | drive_started_close；未完成Pending | 再判状态，绝不接受NEW；驱动close | 同一close状态机 |
| Closing flush/shutdown Pending | Pending且注册底层waker，不先报EOF | Pending且推进close，不先报耗尽 | Pending |
| Done（末seq整连接释放） | 已认证剩余/EOF | SequenceExhausted | Ready成功 |
| Fatal | 原sticky错误 | 原sticky错误 | 原sticky错误 |

若末seq提交时不变量被破坏、另有已接受明文残留，返回WriteFailed熔断，不再clear后假flush成功。原末seq测试要求write立即Err掩盖不驱动行为，已改为准确Pending+buffer0并保留四次真实关闭受阻、未提前drop与最终shutdown/drop断言。

GREEN：38 secure_io tests passed/0failed（原31+7），含真实duplex8B背压末seq拒NEW、wire24B只解密LAST+EOF、parked reader必须等close唤醒再EOF、parked reader写超时30s唤醒，以及真实h2 64B背压。归属rustfmt执行。clippy -Dwarnings exit101仅h2_keepalive两处question_mark（其它writer在途），本模块无span，不声称全crate绿。新差异task-1-fix2-review.patch待独立复审；无真实controller/board双进程交付声明。

## 11. 修复第 3 轮（双 owner 计时唤醒 + 关闭前统一 deadline）

本轮只改 `crates/rsetup-tunnel/src/secure_io.rs`、`crates/rsetup-tunnel/tests/secure_io.rs` 与本报告；未改 `lib.rs`/`Cargo.toml`/算法，也未触碰其它 writer 的在途文件（`tests/h2_wire_control.rs`、`tests/handshake.rs`、`src/streams*` 等），它们同样不在本轮验证口径内。无 `git commit` / `git push` / `cargo clean`；实现内 0 处 `unsafe`（`grep -n unsafe` 只命中模块文档那一句）、0 处 `spawn`。构建目录 `CARGO_TARGET_DIR=target/tunnel-task-1-fix3`。

当前状态：**47 secure_io tests passed / 0 failed**（§10 的 38 条全部保留原强断言 + 本轮 9 条），7 条结构变异全部变红。

### 11.1 两个根因（不是"再加一个入口的补丁"）

1. **计时 waker 的所有权错了。** 一个 `tokio::time::Sleep` 只保留**最后一次 poll 它的那个 waker**。旧实现把两条期限 `Sleep` 登记成"当前调用任务的 `cx`"，而读写两向常常来自不同任务（`tokio::io::split` + 各自 `spawn`；单线程运行时靠 split 的锁也无法改变这一点）：读方向顺手 `drive_tx` 就把 parked writer 的 waker 顶掉，读任务随后 `Ready` 退出 ⇒ 到点只叫醒一个已经结束了的任务，写方向永久 park，直到有人再 poll 一次才"顺带"fatal。反过来固定算写侧也一样漏（读方向被取消或换任务时重演）。
2. **状态优先级遮蔽了期限。** `poll_read` 旧顺序是 `fatal → 零容量 → drive_tx → ready!(close) → loop{期限}`：末有效序号进入 `closing` 后，底层 `poll_flush`/`poll_shutdown` 持续 Pending 会让本函数**在核验 RX 期限之前**就返回 `Pending`。RX 首字节期限于是被"收尾推不动"永久遮掉（实测 30.001s 仍 `Pending`，放行 shutdown 立刻 `ReadDeadline` ⇒ 证明不是 timer 被刷新，而是优先级问题）。

### 11.2 结构修复：稳定 `Arc` 双 owner 唤醒中心 + 入口统一核验

- 新增 `WakeCenter`（`secure_io.rs:228`，私有）：`Mutex<OwnerSlots{read,write}>`，**只有两个有界槽**；`impl Wake for WakeCenter`（`:272`）的 `wake`/`wake_by_ref` 都走 `wake_both()`：临界区内**取出**两个 `Option` ⇒ **释放锁** ⇒ 再依次 `wake()`（不锁内 wake、不跨 poll 持锁、无后台任务、无 `unsafe`）。
- `SecureIo` 去掉 `read_waker`/`write_waker` 两个字段，换成 `wake: Arc<WakeCenter>`；四个入口先登记本向槽（`register_reader`/`register_writer`，同任务重复 poll 由 `Waker::will_wake` 免克隆）。
- 两条期限 `Sleep` 只向 `poll_deadline(wake, slot)`（`:287`）登记**中心生成的广播 waker**：`Waker::from(Arc::clone(&wake))` 始终是同一 `Arc` ⇒ `will_wake` 命中 ⇒ tokio 的 `Sleep` 不会因每次 poll 反复重登记。**任何** `arm_*` 与核验都不再接触调用方 `cx`，所以谁 poll 都不会顶掉谁。
- `arm_*` 与 `check_connection_deadline`（`:596`）严格分离：核验**只 poll 既有 `Sleep`**，绝不新建、绝不改锚；锚定只在 RX 首字节 / TX 首次受阻，撤除只在记录凑齐 / commit / terminate。close 自身**不新增任何期限**（`RECORD_IO_DEADLINE` 仍是唯一期限常量）。
- 四个入口统一顺序：`登记本向 waker → sticky fatal → check_connection_deadline() → 其余推进`；`poll_read` 循环顶部同样先核验（`:826` 起）。`drive_tx` 不再自己核验（避免双份核验，也保证期限到点后**不再往底层推任何字节**）。两条期限同时到点先报 `ReadDeadline`。
- `terminate()` 用同一次广播叫醒双向（`:501`）；收尾 `Done` **释放底层**时也广播一次（`:782` 的 `ClosePhase::Done` 分支，只在 `io.take()` 真拿到对象时发生）——否则对向会 park 在"已经消失的底层 waker"上，再没人能叫醒它。

#### waker 所有权 / 取消语义表

| 事件 | 登记在哪 | 谁被唤醒 | 取消语义 |
| --- | --- | --- | --- |
| `poll_read` 进入 | 中心 `read` 槽（该方向最新 waker） | — | 读任务被取消 ⇒ 该槽留下过期 waker，**写槽不受影响**；下次 poll 覆盖 |
| `poll_write`/`poll_flush`/`poll_shutdown` 进入 | 中心 `write` 槽 | — | 同上；换任务接手只是覆盖自己那一槽 |
| RX/TX `Sleep` 注册 | 中心广播 waker（同一 `Arc`，幂等） | — | 与"谁 arm 的"无关：登记对象不是任务 |
| 期限到点 | — | **读槽 + 写槽**（取出后释放锁再 wake） | 任一侧已消失 ⇒ 那次 `wake()` 是安全空操作，另一侧照旧醒 |
| `terminate()` 致命 | — | **读槽 + 写槽**（同上） | 双向得到同一 sticky 错误 |
| 收尾 `Done` 释放底层 | — | **读槽 + 写槽**（每次 Done 只一次） | 防止对向搁死在已 drop 的底层对象上 |
| 底层可读/可写/shutdown 完成 | 仍用调用方 `cx`（底层对象自己的 waker） | 该任务 | 与本中心无关，语义未改 |

槽位有界 ⇒ 旧任务/被取消任务的 waker 最多两份，不会累积；没有任何"后台刷新计时的任务"。

#### 状态 × 入口优先级表（本轮实现的实际顺序）

| 状态 | 正容量 `poll_read` | `poll_write` | `poll_flush` / `poll_shutdown` |
| --- | --- | --- | --- |
| sticky fatal | 同一 sticky 错误（**先于**零容量约定） | 同一 sticky 错误 | 同一 sticky 错误 |
| 期限已到期（RX 首字节 / TX 受阻） | **立即 fatal**，先于交付明文、EOF 与收尾 | **立即 fatal**，先于接受与收尾 | **立即 fatal**，先于 flush/shutdown 推进 |
| 零容量 read（未到期） | `Ready(Ok(0 字节))`：不消费底层、不推进 flush/shutdown（实测计数不变） | — | — |
| `closing` 收尾 Pending | `Pending`（不假报 EOF），**但先核验期限** | `Pending`（绝不接受 NEW），**但先核验期限** | `Pending`（绝不假报成功），**但先核验期限** |
| 收尾 `Done` 且 `closing` | 已认证剩余明文 → EOF | `SequenceExhausted` | `Ready(Ok(()))`，底层已释放并广播 |
| 显式 `poll_shutdown` 成功（非 closing） | 读半边继续交付明文，底层不释放 | `Closed` | `Ready(Ok(()))` |

"先核验期限"≠"重锚期限"：核验只 poll 既有 `Sleep`；原 deadline 依旧只在首字节/首次受阻锚定、只在凑齐/commit 撤除。close 也不引入新的超时。

### 11.3 先 RED：反例精确复现（真实 task park / 真实唤醒）

`CARGO_TARGET_DIR=target/tunnel-task-1-fix3 timeout 900 cargo test -p rsetup-tunnel --test secure_io --locked -- --test-threads=1`（修复前）：`39 passed; 5 failed`

| 反例 | 用例 | RED 实测（原文案） |
| --- | --- | --- |
| 真实 `duplex(8)` + split：`ACCEPTED` 被 Ready 接受但 28B 线上记录只上了 8B，写任务 `NEW-3` park，读任务读到合法 reply 后 **Ready 结束**，再 `advance` 过 30s | `parked_writer_wakes_after_the_read_task_has_left` | `写期限到点没有唤醒 park 中的写任务：Sleep 的 waker 被已结束的读任务覆盖了` |
| 同上，但读方向被**真实取消后彻底退场**（`timeout` 到点 drop 掉 Pending 中的读 future + `drop(r)`），此后无人再 poll 读方向 | `cancelling_the_read_task_does_not_lose_the_write_deadline` | `读方向被取消后，写期限到点没有唤醒仅剩的写任务` |
| RX 首字节 t=0 锚定期限 → 末有效序号进入 closing、底层 `poll_flush` 持续 Pending：t=29.999s `Pending`（正确）、t=30.001s 仍 `Pending` | `rx_first_byte_deadline_is_not_shadowed_by_pending_flush` | `RX 首字节期限被 closing（Flushing）的 Pending 遮蔽了：Pending` |
| 同上，收尾停在 `ShuttingDown`（`poll_shutdown` 持续 Pending） | `rx_first_byte_deadline_is_not_shadowed_by_pending_shutdown` | `RX 首字节期限被 closing（ShuttingDown）的 Pending 遮蔽了：Pending` |
| 同状态但改由**写入口**推进：`advance(30.001s)` 后 `poll_write` | `write_entries_observe_an_expired_rx_deadline` | `写入口没有核验已到期的 RX 期限：Pending` |

诚实记录两件事：

- 第一版把"取消 / 换任务"合成一条用例时，它在**修复前也通过**——因为接手的新读任务自己 poll 到期限、`terminate()` 顺带把写任务叫醒，属于"恰好路过"，不咬机制。已拆成两条：`cancelling_the_read_task...`（无人接手 ⇒ 真 RED）与 `replacing_the_read_task...`（换任务对照，修复前后都绿，作用是防止以后把 owner 改成"只认某个固定任务"时悄悄回退）。
- duplex 用例断言"对端 drain 掉那 8 字节后读到 EOF"，是因为 `DuplexStream` 会先把缓冲区里已有的 8 字节交给对端再 EOF；同时断言 `drained.len() == 8`，以证明致命后线上没有多出 `NEW`。

### 11.4 GREEN 与变异验证

GREEN（修复后，同一命令）：`test result: ok. 47 passed; 0 failed`；并行 3 次同样 47/47；`--lib` 6/6。

本轮新增 9 条用例（除两条保留项外都用真实任务 park/唤醒 + `tokio::time::advance` 界限，不靠手工反复 poll）：`parked_writer_wakes_after_the_read_task_has_left`、`cancelling_the_read_task_does_not_lose_the_write_deadline`、`replacing_the_read_task_keeps_the_write_deadline`、`rx_first_byte_deadline_is_not_shadowed_by_pending_flush`、`rx_first_byte_deadline_is_not_shadowed_by_pending_shutdown`、`write_entries_observe_an_expired_rx_deadline`、`close_completed_by_read_side_wakes_a_parked_writer`、`authenticated_residual_plaintext_is_delivered_in_order_across_polls`、`explicit_shutdown_keeps_the_read_half_and_refuses_new_writes`。测试基建新增：`MockIo::stall_flush`（构造收尾停在 Flushing 段）、`release_shutdown`（放行配额但**不**唤醒）、`wake_last`（只叫醒最近登记的那个 waker，用来精确构造"只有读方向被底层叫醒"）。

变异验证（逐条改实现 → 定向跑 → 全部还原并核对 md5）：

| # | 变异 | 变红用例 |
| --- | --- | --- |
| — | 修复前基线（期限登记"当前 `cx`" + 旧优先级） | §11.3 的 5 条 |
| M1 | `wake_both` 只叫醒写槽 | `read_deadline_wakes_a_parked_reader_task`、`write_deadline_wakes_reader_when_only_reader_remains_parked`、`replacing_the_read_task_keeps_the_write_deadline` |
| M2 | `wake_both` 只叫醒读槽 | `parked_writer_wakes_after_the_read_task_has_left`、`cancelling_the_read_task_does_not_lose_the_write_deadline`、`write_deadline_wakes_a_parked_writer_task`、`fatal_terminates_parked_other_direction`、`close_completed_by_read_side_wakes_a_parked_writer`、`replacing_the_read_task...` |
| M3 | 读入口期限核验退回"收尾之后"（保留广播） | `rx_first_byte_deadline_is_not_shadowed_by_pending_flush` / `..._pending_shutdown` |
| M4 | 删除写入口的期限核验 | `write_entries_observe_an_expired_rx_deadline` |
| M5 | 零容量 read 挪到期限核验之前 | `rx_first_byte_deadline_is_not_shadowed_by_pending_flush` / `..._pending_shutdown` |
| M6 | 收尾 `Done` 释放底层时不广播 | `close_completed_by_read_side_wakes_a_parked_writer` |
| M7 | `register_write` 变 no-op（写向 owner 丢失） | 与 M2 同一组（6 条） |

M5 第一次跑出的是**无效变异**（`45 passed`）：致命后的零容量读已被更早的 sticky fatal 拦住，掩盖了这个顺序。把用例改成"期限到点后的**第一次**观察就用零容量读"（它必须报 `ReadDeadline`，而不是回 `Ready(Ok(0 字节))` 让调用方误判成"只是还没数据"）后 M5 才变红。这类"改了实现没人红"的口子本轮撞到一次，已按上法补咬。

### 11.5 本轮实际执行的命令与结果

```bash
export CARGO_TARGET_DIR=target/tunnel-task-1-fix3   # 本轮专用目录

cargo test -p rsetup-tunnel --test secure_io --locked -- --test-threads=1
#   修复前：39 passed; 5 failed（§11.3）  →  修复后：47 passed; 0 failed
cargo test -p rsetup-tunnel --test secure_io --locked             # 并行 3 次：47 passed ×3
cargo test -p rsetup-tunnel --lib --locked                        # 6 passed
cargo clippy -p rsetup-tunnel --lib --locked -- -D warnings       # 0 诊断
cargo clippy -p rsetup-tunnel --test secure_io --locked -- -D warnings  # 0 诊断
rustfmt --edition 2024 --check crates/rsetup-tunnel/src/secure_io.rs \
                          crates/rsetup-tunnel/tests/secure_io.rs  # exit 0（先原地格式化再 --check）
cargo test -p rsetup-protocol --locked                             # 191 passed（R5 未回归）
cargo test -p rsetup-protocol --locked -- --list | grep -c test_seq_constructor                       # 0
cargo test -p rsetup-protocol --locked --features test-util -- --list | grep -c test_seq_constructor # 1
cargo test -p rsetup-protocol --doc --locked                       # 3 条 compile_fail 门禁通过
cargo doc -p rsetup-tunnel --no-deps --locked                      # 通过；只剩 2 条既有 rustdoc 告警
```

`cargo doc` 顺手跑了一次：本轮新写的模块文档原本用 intra-doc link 指向私有的 `WakeCenter`，会新增 2 条 `private_intra_doc_links` 告警，已改成普通 code span；剩下 2 条是**本轮之前就存在**的（`src/identity.rs:75` 链向私有 `Self::key_pair`，以及本文件 §1 那轮写下的 `TxRecordLayer::commit` 未解析链接），未越界去动。

§10 的 38 条用例名与断言全部未删改，包括：末有效序号 commit 当场不接受 NEW（Pending + buffer 0）、`LAST` 真实 wire 恰 24B 只解密 LAST、真实 duplex8 背压下的末 seq 拒 NEW、`ClosePending` 四条传播（write/flush/shutdown/read）、`cancel` 不同 NEW、record r0 在 t=29 完成 + r1 在 t=59 致命、真实 h2 64B 极端背压、真实 TCP 立即释放整条连接。brief 点名的"认证残余先 A 后 BC"与"显式 shutdown 保读半边"此前在本文件没有同名/等价断言，本轮补成常驻用例（`authenticated_residual_plaintext_is_delivered_in_order_across_polls`、`explicit_shutdown_keeps_the_read_half_and_refuses_new_writes`）。

本轮**没有**执行、因此也不声称：`make lint` / `cargo clippy --workspace --all-targets -D warnings` / `cargo fmt --all -- --check`（口径原因见本仓 `AGENTS.md`；且 `tests/h2_wire_control.rs`、`tests/handshake.rs`、`src/streams*` 等他人模块在途，把它们的告警算进本轮结论没有意义，也不去动它们）；`cargo clean`；真实 controller/board 双进程与真实网络交付（§7.4）；`#[ignore]` 的环回/双进程用例。

### 11.6 本轮仍未覆盖

1. **广播 waker 的"不重复重登记"没有专门断言**：只由 `Waker::from(同一 Arc)` 的性质（tokio `Sleep` 先 `will_wake` 比较再重登记）保证；用例断言的是唤醒结果，不是重登记次数。
2. **仍是单线程调度覆盖为主**：新用例跑在 `flavor = "current_thread"`（以及默认 `#[tokio::test]` 的手工 poll 路径）；两个槽位的跨线程真实竞争、"一个任务在别的 worker 线程被 wake 的同时另一任务正在 poll"这类交错没有压测（实现无 `unsafe`、锁内不 wake，风险面是虚假唤醒而不是数据竞争）。
3. **`MockIo` 的 waker 集合是"广播型"**（`wake_all` 一次叫醒全部），因此 `close_completed_by_read_side_wakes_a_parked_writer` 依赖刻意构造的 `wake_last`；真实 TCP 上 `poll_shutdown` 的 waker 分布只由 §14 的真实 TCP 用例覆盖。
4. **时钟边界只到毫秒**：29.999s 必须 Pending、30.001s 必须 fatal（`tokio::time::advance` 精确推进）；未覆盖时钟回退、系统休眠/恢复后的期限表现，也未测真实时钟。
5. **h2 之外的上层消费者**（真实 TLS 流、Unix socket、现网丢包/重传/半关闭）未测；两条 h2 用例仍只证明"SecureIo 不会成为瓶颈"。
6. 本轮不涉及认证材料，未重开 `crypto.rs` 的密钥/nonce 断言（§3/§5/§7 结论沿用，R5 门禁已重跑）。


## fix3独立复审与测试口径

备审85452cd1最终Spec/Quality PASS（47+6独立目标、10项stdin新鲜反例与真实duplex/split控制，限定两个旧阻断均ADDRESS）。单条ABC残余独立通过。其Low称旧A/BC两记录第一次不会认证BC，主代理补断言实跑却观察rx_seq=2；read_limit在buffer<4B时返回cap可批量成帧，所以此假设不成立。仍将常驻用例收敛为单次seal ABC，首次cap1=A后显式rx_seq=1且mock底层readable为空，再读BC与EOF，目标实际GREEN。只加强测试，未修改已复审生产代码。不要据此重开已通过记录deadline，也不把本段等同controller/board双进程证明。

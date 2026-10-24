# Task 4c 交付报告：H2 wire 级排空控制（GOAWAY 注入 / peer GOAWAY 观察 / P3 读 Err 同口径）

日期：2026-10-23 ｜ 范围：`crates/rsetup-tunnel/src/h2_keepalive.rs`、`tests/h2_keepalive.rs`、新增 `tests/h2_wire_control.rs`
不改动：`lib.rs`、manifest、其他模块（未 commit/push，未动 `cargo clean`，无任何密钥进入代码/测试/报告）。

## 1. 新增公开 API（均在 `rsetup_tunnel::h2_keepalive`）

| 项 | 语义 |
| --- | --- |
| `H2KeepaliveIo::control_handle()` | **必须在 io 交给 `h2::handshake` 之前**取句柄；之后即便泛型 `S` 随 io 被移走仍可驱动（测试用 `Box<dyn Any>` 黑盒验证）。 |
| `H2WireControl`（Clone + Send + Sync） | 不持有/不 clone 底层 `S`、不持 key、不启后台 task；只共享有界状态 + 唤醒 driver。 |
| `begin_drain()` | 幂等；丢弃从未上线的排队探测（在途半帧原样写完）；冻结窗口开/停与失败计数（排空 ≥5s 不被保活假阳性判死；排空可超 45s 也不误报死链）；业务字节与 h2 原生 PING/ACK 不受影响。 |
| `request_client_goaway()` | 单槽幂等，恰好一条：type=7、stream=0、payload 恰 8B 全零（NO_ERROR、debug 空）。**last_stream_id 恒 0**：RFC9113 §6.8 的 last ID 指 peer-initiated 流，本协议禁 server push ⇒ client 恒 0，不取自身最大奇数 ID。server 角色 `WrongRole`（其关闭走 h2 官方 graceful）；终态 `Closed`。入队即唤醒挂在 poll 上的 driver（空闲连接可排空）。 |
| `wait_goaway_sent()` | `Ok` 仅当整帧 17B 通过底层 `poll_write` **且**底层 `poll_flush` 返回 Ready(Ok)；只入队/flush Pending 不算。fatal、`poll_shutdown`、适配器 `Drop` 均当次唤醒 Pending waiter 为 `Closed`（不永挂）。driver 停止 poll 且未 fatal 时保持 Pending——契约是 driver 持续 poll，文档已写明。 |
| `subscribe_peer_goaway()` | `watch<Option<PeerGoaway{last_stream_id, error_code, sequence}>>`；latest + 单调序号；只观察结构 8B（保留位剥除），**peer 可控 debug 变长尾不进通知、不进日志、不整帧留存**（固定 `[u8;8]` 采集器）；结构性非法（len<8 / stream≠0）不通知但字节逐字交 h2。 |
| `WireControlError{WrongRole, Closed}` | 固定文案，可安全进日志。 |

## 2. 注入纪律（实现要点）

- **注入门控（新增，含判别性测试）**：自发 PING 与 GOAWAY 都必须在 24B 前导**与首条 SETTINGS**（§3.4）完整记账之后，且只落在完整帧边界与 HEADERS/CONTINUATION 块结束（§4.3）。`probe_never_precedes_first_settings_frame`：两侧角色在首 SETTINGS 前跨 2 个窗口零字节自发；SETTINGS 落地后注入立即恢复。
- **顺序硬约束**：在途半帧探测（inflight）未写完不得起 GOAWAY；GOAWAY 半帧占用字节流期间 h2 的 `poll_write` 返回 Pending（绝不把 GOAWAY 插进半帧中间，逐字节级用例覆盖 cap=40 截断场景）；`poll_flush` 在 probe/GOAWAY 未写完时保持 Pending。
- **无后台 task**：drive() 最先注册 driver waker（含 Pending 退场），句柄 `take()` 唤醒；`Drop for H2KeepaliveIo` 与 fail()/shutdown 均同步 `mark_closed`；`into_inner` 改 `mut self`+`take()` 兼容 Drop。锁全为短临界区、中毒容忍、绝不跨 poll 持锁；waker 在锁外唤醒。
- **GOAWAY 与既有流**：RFC9113 §6.8 允许 GOAWAY 后继续处理既有流；client 的 last=0 指未处理 server 发起的流，不能解释为拒绝自己的奇数流。早期测试把 server 的 body 等待放在驱动 Connection 的同一任务中，因无人持续 poll Connection 而挂起，错误地将它归为“协议矛盾”。这一归因撤回。主代理新增真实 h2 对照，server 的 accept 驱动与 handler 并行：确认 GOAWAY 写出且对端 watch 已观察后，才发送原流 DATA/EOS；响应头在 GOAWAY 前/后两种时机均能完成响应。session 应按协议先 GOAWAY 再排空，不能要求所有响应先完成来规避驱动错误。

## 3. P3 关闭：底层读 Err 与写 Err 同口径

三条读分支（9B 帧头、17B PING 定长暂存检视、payload 流式转发段）的 Err 现在统一走 `fail_read`：当次记录、**同步释放底层**（`S::drop` 立即发生）、四类 poll sticky、终态后不复活不二次释放；EOF（Ok(0)）仍走截断透传语义不变。`underlying_read_failure_releases_inner_immediately` 三分支各自断言 err kind 原样、`drops==1`（首错当次）、`shutdown_calls==0`、sticky 4 种 poll ×3 轮、`into_inner()==None`、终态后无探测调度。未引入 unsafe/fixed token。

## 4. fixture 口径修正（旧测试为何合法地改）

旧套件把“preface 后第一帧即保活 PING”当合法，违反 RFC9113 §3.4（连接首帧必须 SETTINGS）。门控落地后 24 个用例按真实 h2 时序补写首条 SETTINGS（client：preface 后；server：收到 preface 后先出站 SETTINGS），其余断言逐条保留原语义（含预算/偏移调整：MockIo 预算 9+cut、`attempts()[1]` 为首条探测等）。`blocked_write` 的抽干改保留式（`drain_peer_keep`，否则会吞掉刚写出的探测）并断言无补发。`server_role_rejects` 断言改为“PING 允许、GOAWAY 绝不”，窗口只推 2 个（3 个未 ACK 判死与本用例无关）。

## 5. 测试证据（全部本机实测）

```
CARGO_TARGET_DIR=target/tunnel-task-4c cargo test -p rsetup-tunnel --lib \
  --test h2_keepalive --test h2_wire_control
# test result: ok. 6 passed  (lib 单测)
# test result: ok. 37 passed (tests/h2_keepalive.rs：35 旧口径修正 + 2 新)
# test result: ok. 16 passed (tests/h2_wire_control.rs：全新)

CARGO_TARGET_DIR=target/tunnel-task-4c cargo clippy -p rsetup-tunnel --all-targets -- -D warnings
# 己方 3 个文件 0 诊断；剩余 3 个报错全部位于 tests/src secure_io.rs（他人进行中文件，越权不动）
CARGO_TARGET_DIR=target/tunnel-task-4c cargo fmt --package rsetup-tunnel -- --check
# 己方 3 个文件 0 diff（已定向 rustfmt --edition 2024）；剩余 diff 仅 tests/secure_io.rs（他人文件）
```

覆盖亮点：GOAWAY 帧字节逐字 `[0,0,8,7,0×9,0,0,0,0,0,0,0,0]`；跨线程 clone 单槽幂等；watch 先观察后订阅不丢通知（实现中曾因此踩 tokio `send` 无接收者丢值的坑，已改 `send_modify` 并有“先 feed 后订阅”用例钉住）；真 h2 对端 GOAWAY 结清在途流+拒新流；server 官方 graceful 的 SHUTDOWN PING（token `0b7ba2f08b9bfe54`）与 ACK 逐字透传不被吞/代答；句柄黑盒（`Box<dyn Any>`）闭环。

## 6. 范围诚实声明

本报告证明的是 **wire 适配层**行为（纯 tokio 协作调度、`tokio::time::pause`、无 sleep 伪成功）。**不**声明：会话层“排空 ≥5s 停止新探测周期”的端到端行为、双进程/真 TCP 两端的集成验证——它们属于 session 层后续任务。`fatal_timeout_over_real_tcp_closes_socket` 是既有真 TCP 用例（本轮仅按 §3.4 补写 SETTINGS 并保持全部断言）。

## 7. 遗留与移交

- `wait_goaway_sent` 的 Pending 依赖 driver 持续 poll（无后台 task 是计划裁决），会话层接入时须保证 connection 任务不因 drain 提前 abort。
- 排空期间窗口失败计数冻结（含在途窗口关闭不计失败）为设计决策，理由与替代方案已写入代码注释；若会话层希望排空期间仍计时判死，需要显式改接口而非默认。
- `secure_io.rs` 的 fmt/clippy 问题为他人进行中文件，未触碰。


## 8. 独立主审与归因复核

主审6ec7566c复跑6+37+16，确认wire实现与API成立，但把“中途GOAWAY后响应挂起”归因h2 conn_error。主代理再次读取h2源码：send_response不查conn_error，Connection只在error存在且无活跃流时关闭；以server持续accept驱动+JoinSet并行handler的真实h2常驻对照推翻这一归因。先等待GOAWAY发送/flush完成并serverwatch确认，之后才原流DATA/EOS，响应头早/晚两种均完成，非wire mock。不得把“请求GOAWAY前不能再等响应”作为session硬前提。此前测试inline等待body会停Connection驱动，不能据此证明h2不支持规范排空。

新增回归后h2_wire_control 17/17 actual exit0；定向clippy首次仅新增测试redundant_async_block报错，改tokio::spawn(conn)后exit0；归属rustfmt已执行。生产wire代码未改。新的测试只补既有流GOAWAY后存活证明，完整session>=5s排空/真实双进程仍由后续验收负责。


独立裁决f49d758c再次验证exact1+wire17/clippy绿，并stdin控制“暂停Connection→body超时→同连接恢复驱动即完成”推翻库不支持归因。两项非阻断建议已主代理收尾：常驻early分支await客户端ResponseFuture后才请求GOAWAY（严格早HEADERS屏障），完整响应body至EOS包入2s timeout，目标exact与clippy再次exit0。生产wire仍未改。

# Tunnel Task 2 修复（round2）：状态外帧收口 / pong 绝对期限复核 / Pending 统一退避 — 实施与测试报告

## 1. 概述与执行目标

依据 `.superpowers/sdd/2026-10-23-production-v3-tunnel/task-2-fix-round2-brief.md`，对 round1（F1–F7，见 §7 存档概述）后的 `crates/rsetup-tunnel/src/handshake.rs` 做 3 项 Low 修复。原 F1–F7 处置保持：pinned 准入 future 与 probe 调度并发、10s 写+读、15s 合法帧锚点、私钥文件（identity.rs）本轮零改动。

| 编号 | 级别 | 缺陷 | 定性 | 本轮处置 |
| --- | --- | --- | --- | --- |
| R1 | Low | `drive_pending` 读分支带 `if inflight.is_some()` 守卫：在途 probe 结清后读被关闭，**无在途时到达的旧 pong（t=1）留在连接里**；策略 t=2 Approved → 最终复查 → `Ok` 交付的 io 仍带完整明文 pong 帧，被加密/h2 层当记录层字节消费。`into_inner_checked` 只查 `FrameReader.buf`，救不了「从未被读走」的 socket 内帧 | **round1 前旧契约缺口**（「挂起期只接收匹配 PENDING_PONG」从未被真正执行） | 挂起期**全程读帧**；无在途时任何入站帧当场 `UnexpectedFrame` 违例（不再 `expect` panic）；交付边界先拒缓冲区残余整帧；`into_inner_checked` 继续兜底半截残包 |
| R2 | Low | pong 期限虽锚为 `sent_at + 10s` 绝对期限，但 **select 就绪顺序不是时间证明**：biased 读分支在 pong **整点 t=10.0**（与到期定时器同刻就绪、数据先可读）时抢先把帧结清并批准 | **round1 前旧契约缺口**（round1 只做了锚定，未做结清前复核） | 帧通过三元组核验后、结清（信用）之前，用 `Instant::now() >= probe.deadline` 显式核对**原绝对期限**；边界语义与 `timeout_at` 一致（整点即超时） |
| R3 | Low | round1 的 `final recheck Pending` 绕过 5s 延迟：`final_decision=Some(Pending)` 清掉后 `recheck_at` 为 None → 立即重新 `evaluate`；策略交替返回 Pending/Approved 时服务端在**同一个 0ns 虚拟瞬时**无限连轴转（旧反例二进制观察到 22 次调用止损） | **round1 修复新引入** | **统一所有回到 Pending 的状态转换**：`(1) Approved→复查 Pending` 与 `(1) final_decision==Pending` 均设 `recheck_at = now + ADMISSION_RECHECK_DELAY`（有界），恢复 probe 调度、清已消费决定；不用循环次数止损，未新增/修改任何 §4.1 常量 |

独占涉及文件：`crates/rsetup-tunnel/src/handshake.rs`、`crates/rsetup-tunnel/tests/handshake.rs`、本报告。identity 实现、lib/manifest、h2wirecontrol/NodeAST/SecureIo/streams 等其他 writer 文件零改动。构建目录 `CARGO_TARGET_DIR=target/tunnel-task-2-fix2`（本轮全部证据由当前代码在此目录重新编译产生）。未 commit/push、未 `cargo clean`。

时序常量保持 §4.1 原值：`INITIAL_STEP_TIMEOUT=10s`、`PROBE_PERIOD=5s`、`PROBE_PONG_TIMEOUT=10s`、`CLIENT_SILENCE_TIMEOUT=15s`、`ADMISSION_RECHECK_DELAY=PROBE_PERIOD`；**未增加任何新常量、未为故意慢的策略增加新 probe**。最终临界区复查维持已审认可的「有界直接 await」。

## 2. RED 证据（先失败，且能区分具体缺陷）

夹具全部 `#[tokio::test(start_paused = true)]` 虚拟时钟 + `tokio::io::duplex`，断言精确时刻与错误变体。以下输出采自**未修复的当前实现**。

```
---- stale_pending_pong_is_never_delivered_with_approved_io ----
无在途时到达的旧 pong 必须当场按协议违例关闭；实际批准被交付且 io 里带着 46 字节明文旧帧

---- pong_at_exact_deadline_is_not_credited ----
整点到达的 pong 不得结清批准（now >= deadline 即超时）:
HandshakeResult { io: DelayRead { .. }, session_keys: .., peer_public_key: "..", descriptor: Some(..) }   # 返回了 Ok

---- alternating_pending_approved_backoff_never_busy_loops_and_keeps_probing ----
同一虚拟瞬时 t=Instant { .. } evaluate 第 16 次：回到 Pending 的状态转换在 0ns 忙循环

---- stale_pending_pong_fails_at_arrival_between_probes ----
（等 t=5 才被下一轮 probe 的 token 失配顺带发现，非 t=1 当场 UnexpectedFrame；类型与时刻断言均失败）

test result: FAILED. 4 failed; partial_frame_survives_read_cancellation 通过（取消安全基线，回归保留）
```

判别力交叉验证（逐项回退修复点、其余保持，逐一确认对应测试红）：

| 回退点 | 变红的测试 |
| --- | --- |
| 读分支恢复 `if inflight.is_some()` | `stale_pending_pong_is_never_delivered_with_approved_io`（复现 46 字节泄漏）、`stale_pending_pong_fails_at_arrival_between_probes` |
| 删除结清前 `Instant >= deadline` 复核 | `pong_at_exact_deadline_is_not_credited`（返回 Ok 批准） |
| 复查 Pending 恢复「只清决定不设退避」 | `alternating_pending_approved_backoff_never_busy_loops_and_keeps_probing`（同瞬时第 16 次 evaluate panic） |

R2 的「同刻竞态」是确定性的：`t10.0-exact` 用 `DelayRead` 夹具（字节到达后延迟 100ms 才可读）让 pong 恰在 deadline 时刻可读，读侧被轮询时 `now >= deadline` 与数据就绪**同时成立**，biased 读分支必然抢先——旧实现因此在 t=10.2（该用例 probe 实际提交于 0.2s）批准而非超时。**期限一律锚「probe 提交时刻」**：DelayRead 同样延迟握手首帧，所以断言全部从 `read_pending` 观测到的提交时刻起算，而非测试进程 t=0。

旧独立反例二进制 `target/tunnel-review-2-fix/task2-pending-recheck-edge` 等绑定旧版本代码，**未作为本轮任何证据**；本轮全部反例已落为仓库内测试（§4）。

## 3. 实现要点（`src/handshake.rs`）

### 3.1 R1：挂起期全程读帧，错 phase 当场违例

* `select!` 读分支去掉 `inflight.is_some()` 守卫：挂起态从首个 `SERVER_PENDING` 到交付点**持续**消费并核验入站帧。批准交付点一到即停止读——不预读最终响应之后的记录层数据，也不做无界 drain 或任何静默等待窗口。
* 帧处理器 `inflight.expect(..)` 改为 `let Some(probe) = inflight else { return Err(unexpected_after_settlement(frame_type)) }`：**无在途时任何入站帧都是协议错误**（旧 pong 亦然），当场 `UnexpectedFrame` 关闭，不 panic、不吞进缓冲区留给交付时刻。
* 交付边界（`(1) Approved` 且无在途）：最终复查之前先 `frames.take_frame()` 拒掉缓冲区已坐着的整帧——「错 phase 立即违例，不带入交付」；`into_inner_checked()` 保留，继续对半截残包 fail closed。
* 语义口径如实收窄：纯泛型 socket 无法证明「内核里将来没有字节」，本保证只覆盖**交付前已可观察的帧**（到达于挂起期任何时刻、或已进缓冲区者）；这一点写进代码注释与本测试命名。

### 3.2 R2：pong 结清前复核原绝对期限

三元组（token/nonce/seq）全部核验通过后、`inflight = None`（信用）之前：

```rust
if Instant::now() >= probe.deadline {
    return Err(HandshakeError::Timeout(HandshakeStage::PendingPong));
}
```

`>=` 与 `tokio::time::timeout_at` 语义一致：整点即超时。字段失配仍优先报 `Pending(_)` 违例（核验顺序不变，`pending_pong_field_mismatches_close_the_connection` 未动）。`PongExpired` 定时器分支保留——本复核不取代它，只兜住「帧与到期定时器同刻就绪、读分支抢跑」。

### 3.3 R3：回到 Pending 的统一退避

三处「回到 Pending」的路径全部收敛到同一处置（清已消费决定、`recheck_at = now + ADMISSION_RECHECK_DELAY`、probe 调度随 `final_decision.is_none()` 自动恢复）：

1. `select` 的 `Admission(Pending)` 分支（round1 已有）；
2. `(1) Approved` 的**最终临界区复查回退 Pending**（漏洞点：原先只 `final_decision=Some(Pending)`，不带退避）；
3. `(1) final_decision == Pending` 清除分支（`recheck_at.get_or_insert(now + DELAY)` 兜底，保证任何路径都不会立即重新 `evaluate`）。

不限制循环次数、不新增常量；单在途约束不变（退避恢复期间 probe 只在无在途时发出，不叠加）。

## 4. GREEN 证据

```
$ CARGO_TARGET_DIR=target/tunnel-task-2-fix2 cargo test -p rsetup-tunnel --test handshake --test identity --lib
running 6 tests    -> test result: ok. 6 passed;   0 failed   (lib)
running 44 tests   -> test result: ok. 44 passed;  0 failed   (handshake：原 37 + 新增 7)
running 22 tests   -> test result: ok. 22 passed;  0 failed   (identity：未动)

# 稳定性：--test handshake 连续 5 次全绿（0.02–0.05s/次），R2 整点竞态无抖动。

$ cargo clippy -p rsetup-tunnel --lib --test handshake
（handshake.rs / tests/handshake.rs 零告警；剩余 2 条 match→? 告警位于 src/h2_keepalive.rs，属其他 writer，未改）

$ rustfmt --edition 2024 --check crates/rsetup-tunnel/src/handshake.rs crates/rsetup-tunnel/tests/handshake.rs
FMT OK
```

新增 7 个用例（覆盖 brief 要求的 late / exact / t9.5 / 批准同 timer / 错 phase 分开到达 / finalPending 交替 100 轮不 busy 且仍 probe / partial read 取消安全）：

```
stale_pending_pong_is_never_delivered_with_approved_io   # R1：旧 pong 到达后批准交付 → 必须违例，交付 io 不得带明文帧
stale_pending_pong_fails_at_arrival_between_probes      # R1：探测间隙单独到达的错 phase pong，t=1s 当场违例（不等下一轮 probe）
pong_before_deadline_is_still_credited_and_approved     # R2 控制：提交后 9.5s 合法 pong → 当场结清批准（9.5s 精确）
pong_at_exact_deadline_is_not_credited                  # R2：同刻竞态（DelayRead 夹具），t=10s 整点必须 Timeout(PendingPong)
pong_arriving_after_deadline_fails_as_timeout_not_approval # R2：t=11s 迟到；服务端须在「提交+10s」已关闭，迟到帧写不进已关连接
alternating_pending_approved_backoff_never_busy_loops_and_keeps_probing
                                                        # R3：P/A 交替 100 轮；策略内建「同一虚拟瞬时 evaluate>15 次即 panic」忙环检测；
                                                        # 断言 ≥495s 虚拟推进、calls 与轮数同阶（≤400）、probe 每轮准点且 nonce 不变、
                                                        # 翻 done 后仍收敛到批准（无总审批期限）
partial_frame_survives_read_cancellation                # 半截 pong 在途时被准入事件唤醒丢弃读 future；字节不丢、流边界不污（修复前即绿，回归钉住）
```

R3 的忙环检测放在策略内部（同瞬时调用计数），因此**任何将来 reintroduce 的 0ns 连轴转都会当场 panic**，不依赖外层超时观察挂死。三个 R2 边界用例必须各自独立 `#[tokio::test]`：同一 runtime 内跨用例共享虚拟时钟，单测试内循环复用会让后续用例的 t=0 基准被上一用例推进污染（首轮实现即因此出现 200ms 基准漂移，已拆开并加注释）。

## 5. 保留的原有断言（未放宽）

原 37 个 handshake、22 个 identity 用例一字未改断言与时刻期望，本轮全绿：真实 ring 签名/`descriptor_raw` 逐字节签名、无 TOFU、每连接新鲜 random/X25519、`SignedRejection` 仅来自验签响应、去敏、身份权限与 umask 回归；以及 round1 的 F1/F2/F3 精确时刻回归（`first_pending_is_immediate_then_probe_period_is_5s`、`pending_without_pong_times_out_at_10s`、`decision_after_long_hang_stops_new_probes_and_settles_inflight` t=18 整点、`pong_deadline_includes_the_blocked_probe_write` t=10 整点、`pong_inside_the_remaining_window_is_still_accepted` t=9.5、`blocked_pong_write_does_not_extend_client_silence` t=15、`new_pending_refreshes_silence_own_pong_does_not` t=25）。本轮未把任何 timeout 调长、未放宽任何 `==` 时刻断言。

## 6. 边界与明确未做的事

* **未做真实双进程/真机 TCP 验证**（不变）：全部时序用例走 `tokio::io::duplex` + 虚拟时钟，不据此声称生产环境（内核 socket 缓冲、中间盒、调度抢占）已闭环。
* **未做 fuzz**（不变）：`decode_handshake_frame`/`parse_frame_header` 模糊测试不在本轮范围，也未引入。
* **R1 保证的准确边界**：只覆盖交付前**已可观察**（到达于挂起期、或已进缓冲区）的帧；泛型 socket 上「最终复查直读期间到达的字节」无法在不违例地读进记录层的前提下证明其不存在——不夸称「交付 io 之后绝无字节」，也不为此增加静默窗口或无界 drain。最终复查的有界直接 await 维持已审口径。
* **旧反例二进制不作为证据**：`target/tunnel-review-2-fix/*` 绑定旧版实现；本轮 RED/GREEN 全部由 `target/tunnel-task-2-fix2` 下当前代码重编译产生。
* **并发 writer**：本轮期间 `src/secure_io.rs`、`src/h2_keepalive.rs` 等由其他 writer 修改（一次瞬时编译错误来自并行保存、随后自行消失，与本任务文件无关）；`h2_keepalive.rs` 现存 2 条 clippy 告警不属本任务，未改。未触碰 h2wirecontrol/NodeAST/SecureIo/streams/identity 实现。

## 7. 存档：round1（F1–F7）概述

round1 依据 `task-2-fix-brief.md` 完成：F1 PENDING 循环重写为「pinned 准入 future + probe 调度 + pong 期限」并发 `select!` 状态机（准入挂起期间 5s probe 不停发、无审批总期限）；F2 pong 期限改为锚在提交时刻的绝对期限（写阻塞耗时计入 10s）；F3 板端 15s 静默锚在最后一条合法 `SERVER_PENDING` 到达时刻（回 pong 不刷新）；F4 新建身份文件在**本次新建的 fd** 上 `fchmod(0600)`（仅子进程 umask 0200 用例）；F5 载入路径 `O_NOFOLLOW | O_NONBLOCK`；F6/F7 文档口径按实际实现改写。当轮 handshake 37 / identity 22 全绿；identity 与 F4/F5 部分本轮零改动，仅 R1–R3 触及握手文件。

## 8. 结论

R1（旧契约缺口：状态外帧随批准泄漏）与 R2（旧契约缺口：select 抢跑赢过绝对期限）已由确定性夹具从 RED 转 GREEN，且逐项回退可复红；R3（round1 新引入的 0ns 忙循环）以「所有回到 Pending 的转换统一有界退避」修复，100 轮交替推进 ≥495s 虚拟时间、probe 不停、策略内建忙环检测。原有 37+22 强断言全部保持。真实双进程与 fuzz 验证仍未做，故本报告不宣称 Task 2 已达生产交付标准，仅说明 round2 的 3 项缺陷已修复并具备可重复、可回退复现的判别测试。


## 接纳边界短读补丁（主代理，待定点复审）

备审6c265d29最终R2/R3 ADDRESS，唯一R1Low仍红：maxread1合法短读，同tick排队92B双pong，首帧后buf空直接批准，io残留完整46B。主代理先以max1/2/7/16常驻回归复现RED（max1确批准），后新增FrameReader::reject_observable_trailing_input：先拒用户buf，再一次poll_read最多1B；Pending立即Ok不等待，Ready额外byte违例，EOF/Err终止。批准最终SQL复查前后各一个观察点，直接批准路径同样调用；最终响应签发之后不再读，以免误伤合法加密前导。只承诺该观察线性化点接口可见输入，不能证明未来/并发socket永无字节。

GREEN：48handshake（原44+4）、22identity、lib6，当前目标全部exit0。新增短读双pong、正常shortread批准耗时0、不当输入在final await期间到达拒绝、final await期间EOF不当健康批准；R2/R3常驻精确期限与100轮退避不变。无静默窗口/无无界drain/无后台任务，常量与策略接口未改。未跑真实controller/board双进程/fuzz仍未覆盖；此补丁须独立复审，不以测试数宣布握手complete。

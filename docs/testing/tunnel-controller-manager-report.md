# Task 5 验收报告：controller 连接管理与生产装配

- 分支：`dev-aghost`（未提交、未推送，本次不执行任何 git 写操作）
- 目标：`.superpowers/sdd/2026-10-23-production-v3-tunnel/controller-manager-brief.md`
- 唯一编译/测试目标目录：`CARGO_TARGET_DIR=target/tunnel-controller-manager-resume`
- 结论：**已实现并在语义层验证**；**网络与会话语义已接线但未做双进程实测**；**真实库与真实板端行为未验**（无授权）。

## 1. 改动文件（严格限定在授权范围内）

| 文件 | 行数 | 性质 |
| --- | --- | --- |
| `crates/rsetup-controller/src/transport/manager.rs` | 1537 | 重写：已认证连接管理器 + `DeviceRpc` + 撤销/复查/轮询驱动 |
| `crates/rsetup-controller/src/transport/listener.rs` | 640 | 新增：生产监听装配（握手→记录层→会话 driver→登记） |
| `crates/rsetup-controller/src/transport/mod.rs` | 20 | 注册 `manager`/`listener` 并收敛公开面 |
| `crates/rsetup-controller/src/main.rs` | 1064 | 生产装配：替换两个 fail-closed 装配位、共享快照 store、后台任务与关停编排 |
| `crates/rsetup-controller/tests/transport_manager.rs` | 1248 | 新增：27 个语义用例 |

未触碰：`transport/admission.rs`、`transport/board_client.rs`、`crates/rsetup-tunnel/**`（driver 由另一 writer 独占，只读引用）、`Cargo.toml`、`lib.rs`、`config.rs`、`polling/**`、`tasks/**`。

> 上一位 writer 留下的 901 行 `manager.rs` **未注册也从未编译过**（`transport/mod.rs` 里没有 `pub mod manager;`），其文档中的"已接线"不是事实；本次为整体重写，不继承其自指式半开预留实现。

## 2. 关键不变量与实现位置

| brief 要求 | 实现 | 证据 |
| --- | --- | --- |
| 身份只取已验签公钥，禁止 payload 绑身份 | `run_connection` 只用 `HandshakeResult.peer_public_key`；`NewSession.public_key` 唯一来源；`TunnelSessionHandler` 的 `public_key` 由外层绑定 | 代码走查（`listener.rs:533-543`） |
| 本地产生代际（`Uuid::new_v4`） | `register_ready` 内 `generation`/`stream_epoch` | `same_key_replacement_...` |
| 同 key 顶替：先 `TYPE_SYSTEM kick`，**单调 500ms** 后只关被顶替那份句柄 | `close_connection`（`manager.rs`）+ `SYSTEM_SIGNAL_CLOSE_DELAY=500ms`，定时器只 clone 被顶替的 `Arc` | `same_key_replacement_kicks_old_then_closes_only_old_after_delay`（断言 450ms≤Δ<2s、新连接未被误关） |
| 撤销即时终止活跃 + 挂起，不缓存批准 | `apply_revoke` + `after_commit` 有界通道 + `spawn_revoke_drain` | `after_commit_revocation_terminates_active_session_and_pending_handshake` |
| 挂起握手可取消 | `CancelSignal`（先 `enable()` 注册再复查标志）+ `PendingTicket` | `cancel_signal_wakes_waiters_and_is_idempotent`、`pending_ticket_wakes_on_revoke` |
| 撤销与登记同锁，已撤销身份不得装成当前会话 | `register_ready` 在写锁内复查 `signal.is_cancelled()` → `RegisterError::Revoked` | `cancelled_pending_refuses_registration_of_revoked_identity` |
| 半开有界、无审批总期限 | `HalfOpenPermit`（RAII，`Arc<HalfOpenCounter>`）在**整段握手**期间持有 | `half_open_budget_is_bounded_and_returned_by_raii` |
| 每 peer 协议 probe / 每次握手复查 SQL | 复用已审 `SqlxAdmissionPolicy`（零缓存，`evaluate` 每次跑 SQL）；本层不写永久决定 | 既有 `tests/transport_admission.rs`（34 项，其中 2 项 `#[ignore]`，本次全绿） |
| 通知丢失 fallback 必须 fail-closed 于真实 SQL | `AdmissionTruth::load_current`（生产=`impl AdmissionStore for DbPool` 的只读 `load`）+ `recheck_all_sessions` + `sweep` 唤醒；读失败同样撤销 | `recheck_uses_sql_truth_and_revokes_on_change_or_read_failure`、`sweep_wakeup_triggers_immediate_recheck` |
| `after_commit` 投递失败不得假装成功 | 通道满 → `Err(Config)`（上层 `notification_queued=false`）+ `dropped_revocations` 计数 + 立即唤醒补扫 | `notification_channel_full_reports_error_and_counts_drop` |
| DeviceRpc 按当前 key/ready 路由、5s、**不重试** | `impl DeviceRpc for TransportManager` 恰好一次 `connection.request`；`RequestError` 逐项映射 typed `BoardClientError` | `request_is_issued_exactly_once_and_maps_errors_without_retry`、`request_waits_for_ready_and_rejects_non_ready_session` |
| `on_request` 只回协议允许的结果 | `TunnelSessionHandler::on_request` 一律 `STATUS_UNSUPPORTED_ACTION=1002` 并计数 | 代码走查（未做网络实测） |
| `device.task.result` 真实 codec 解码 + 绑定当前代际 | `on_event` 只接受该 action，`decode_business(TaskRecord)`，身份来自外层 | 解码路径已实现；**业务写入口不存在**（见未验 3） |
| 快照代际只由中控 `confirm_generation` | `ingest_status_sample` 是当前代际的唯一确认入口；旧代际 → `SampleOutcome::StaleGeneration` | `current_generation_sample_is_confirmed_once_and_written`、`old_generation_late_activity_call_and_sample_are_all_rejected` |
| 轮询复用 `PollScheduler`/`SnapshotStore`，不建第二真源 | `spawn_poll_driver`：成员变化才重建相位（重建间隔=周期会让 `next_due` 永追不上）、两次 `due()` 间隔严格 < `tick_cadence`、在途有界（超出丢弃 + `set_overloaded`） | `poll_driver_writes_current_generation_then_terminates`、`poll_driver_stops_polling_after_revoke` |
| `last_seen` 只由当前认证代际的成功活动更新 | `note_activity(public_key, generation)` 是唯一入口，`DeviceRpc` 成功时调用 | `old_generation_late_activity_call_and_sample_are_all_rejected` |
| 不偷换 HTTP 监听 | `TransportConfig::from_env` 只读 `RSETUP_TUNNEL_LISTEN`/`RSETUP_CONTROLLER_KEY_FILE`/`RSETUP_TUNNEL_MAX_*`/`RSETUP_TUNNEL_DB_TIMEOUT_MS`，与 `ControllerConfig::listen_address` 完全无关（`ControllerConfig` 无这些字段，故**未新增字段**） | 代码走查（`listener.rs` 顶部环境变量表） |
| 不打印私钥 | 只持 `Arc<IdentityKey>`；`Debug` 手写为 `IdentityKey(<redacted>)`；所有 `ListenerError` 是去敏固定文案 | `impl fmt::Debug for TransportConfig` |
| child tasks 不泄漏 | `TaskGuard` 计数 + 关停「GOAWAY 宽限 6s → 超期 abort → 逐个 await 确认退出」；三个后台任务由同一个 `watch<bool>` 取消 | `all_background_tasks_terminate_on_shutdown_and_close_outs_drain`、`recheck_task_runs_periodically_and_terminates_on_shutdown` |

### TYPE_SYSTEM 契约的实际使用（父任务追加指令）

- Kick/Revoke 一律经 driver 新增的 `SessionHandle::send_system(SystemSignal)`（`crates/rsetup-tunnel/src/session/handle.rs:499`，driver 已于 17:38 落地），走与业务解耦的 `system_tx` 优先队列，**没有任何 `request("tunnel.kick")` 冒充路径**。
- `Ok(())` 只表示"已交给本端发送队列"（driver 文档同此语义），因此 `close_connection` 的收尾任务：入队成功 → 单调 `tokio::time::sleep(500ms)` → `abort()`；入队失败（连接已坏）或**无 tokio 运行时**（`Handle::try_current()` 失败）→ 立即 `abort()`，不假装"稍后关闭"。
- 已发/未发边界可观测：`TransportManager::system_signals_sent()` / `system_signals_unsent()` / `pending_close_outs()`。
- 顶替路径的定时器只 clone **被顶替**那份 `Arc<dyn DeviceConnection>`，新代际不受影响（用例断言新连接 `aborts()==0`）。
- 撤销的 socket 切断同样落在 500ms flush 窗口之后，但**路由层在撤销瞬间**已摘除会话（`current_connection` 立即 `None`，用例已断言），旧会话不可能再被 RPC 使用。

## 3. 已验（可复现）

```
CARGO_TARGET_DIR=target/tunnel-controller-manager-resume cargo check -p rsetup-controller --all-targets --locked
  → 0 error / 0 warning（lib + bin + 全部 test 目标）
CARGO_TARGET_DIR=... cargo test -p rsetup-controller --locked --lib --test transport_manager --test transport_admission \
  --test transport_board_client --test production_composition --test observer_contract --test events_sse \
  --test sse_business_events --test task_scheduler --test time_consumption --test gate_udp \
  --test management_gateway_http --test web_assets
  → 全部 ok：lib 991 passed / 3 ignored；transport_manager 27 passed；transport_admission 32 passed(2 ignored)；
    transport_board_client 15；production_composition 14；observer_contract 68；events_sse 22；
    sse_business_events 11；task_scheduler 39；time_consumption 9；gate_udp 25；management_gateway_http 3；
    web_assets 88。exit code 0
CARGO_TARGET_DIR=... cargo clippy -p rsetup-controller --all-targets --locked
  → 本次 5 个文件零告警（`rsetup-tunnel` 自身的 3 条告警属另一 writer 的文件，未触碰）
cargo fmt --package rsetup-controller -- --check → 通过
```

未纳入上述命令的 `mysql_identity` / `tidb_identity` / `metrics_real_db` / `migration_v4` / `acceptance_runtime` 需要真实数据库，本次**未运行**（无授权）。

## 4. 未验（明确边界，不得当作已交付）

1. **双进程端到端未做**：`run_controller_listener` 的真实 `TcpListener` → `server_handshake` → `SecureIo` → `start_server_session` → `wait_ready` → 登记 这条链只做到**编译接线**，没有用真实板端进程或 loopback 双任务跑通一次握手，因此 1002 回包、`on_event` 解码计数、`accepted/ready/handshake_failed` 计数的真实数值全部未实测。
2. **真实库未跑**：`AdmissionTruth for DbPool`（`devices` 表只读 `load`）与 `SqlxAdmissionPolicy` 的真实 SQL 行为未被我的用例覆盖；我的用例注入的是 seam 替身，**替身通过不等于真实库撤回/拒绝已实测**。`CancellableAdmissionPolicy`（握手取消包装）也没有用例覆盖（它需要 `Arc<SqlxAdmissionPolicy>`，即需要池），其正确性目前只有代码走查 + 既有 `transport_admission` 的策略侧测试支撑。
3. **`device.task.result` 没有业务写入口**：`tasks::service` 的公开方法只有 preview/submit/cancel/time_review/release_lock，没有"按 `sub_task_id` 推进任务状态"的已审入口。因此生产装配注入 `RejectNoTaskResultSink`（一律 `false`），事件被**计数丢弃**（`BridgeStats::events_unwired`），不伪造任务推进。业务侧接线属后续任务。
4. **时钟轮询线未接**：`time/board_clock.rs` 只有纯函数 `estimate_board_offset`/`ClockEvidence`，没有可写入的 sink，故 `device.clock.get` 无消费者，未接线（不造第二套时钟口径）。
5. **恢复模式真源未交付**：`main.rs` 保留唯一 fail-closed 装配位 `UnwiredRecoveryProbe`（读取即 `Err(Config)`→503），`TransportManager::current_mode` 只委托它。这是"Runtime 恢复控制器尚未交付"的如实投影，不是本任务遗漏的第二真源。
6. **brief 提到的 `ConnectionSnapshots` 在仓库中不存在**（grep 无命中），因此未实现该投影；实时视图走既有 `DeviceLiveProbe`/`DeviceLiveView`。
7. **`DeviceStreamHealth` 没有 `Unknown` 变体**：因此"读取成功但该设备无会话"表达为 `Offline` + `capabilities: None`（契约要求读取本身不得对离线设备报错），"已注册未 ready"→`Starting`，排空→`Degraded`。若后续要区分"unknown"与"offline"，需要 02 视图先加变体。
8. **无运行时上下文的关闭路径未测**：`close_connection` 在 `Handle::try_current()` 失败时立即 `abort`（安全边界更靠前），该分支只在同步上下文可达，单测未覆盖。
9. **半开名额在连接任务内获取**：accept 之后才预留，意味着瞬时 backlog 落在内核 accept 队列；进程侧另有连接任务总量硬上界 `max_half_open + max_sessions`（超出即暂停 accept，不排队、不无限增任务）。这一取舍未在真实压力下验证。
10. **未做的配置项**：撤销通道容量（固定 256）、兜底复查节拍 `SWEEP_INTERVAL=5s`、关停宽限 `SHUTDOWN_GRACE=6s`（= `SessionConfig::drain_timeout` 5s + 余量）均为常量，未做 env 配置。
11. **私钥文件**：`IdentityKey::load_or_generate`（0600 审计、原子生成）由 `rsetup-tunnel` 负责且已审；本任务未新增任何 TOFU/信任列表路径（中控侧不需要 `trusted_servers`）。未做真实文件的权限拒绝实测。

## 5. 生产装配拓扑（`main.rs`）

- `build_transport_manager(&db, &time_gate) -> (Arc<TransportManager>, Arc<SnapshotStore>)` 是唯一构造点：准入真相 `Arc::new(db.clone())`（只读）、时间证据用**同一个** `TimeGateHandle`、快照 store 由它拥有。
- 同一个 `Arc<SnapshotStore>` 交给 `DeviceStatusState`（HTTP 读线）与轮询驱动（写入方）→ 新鲜度只有一份真源；未 `confirm_generation` 的 update 一律 `NoActiveGeneration`。
- `UnwiredTransportSources` 与 `UnwiredBoardClient` **已删除**：管理线三个源（`DeviceLiveProbe`/`RecoveryModeProbe`/`ManagementNotificationSink`）由 `TransportManager` 承担；启动恢复的 `BoardClient` 改为 `TransportBoardClient::new(manager.as_device_rpc())`（此刻登记表为空 → `task_record` 真实回 `DeviceOffline`，恢复路径永不发送，语义与旧 fail-closed 位一致）。
- 监听前 spawn：`spawn_revoke_drain`、`spawn_admission_recheck`、`spawn_poll_driver` + `run_controller_listener`（仅当 `RSETUP_CONTROLLER_KEY_FILE` 存在；缺失即打印"板端接入监听未启用"并以真实离线视图继续服务管理线）。
- 关停：`shutdown_signal`（Ctrl-C/SIGTERM）→ `watch<bool>` 单一广播 → 监听停接新连接、`shutdown_all()` 请求 GOAWAY 排空、后台任务退出、连接任务宽限后 abort 并确认退出；不重发任何已发出的变更。
- 监听任务自身失败 → `eprintln!` + `std::process::exit(1)`（半死不活比不监听更糟，交给进程管理器重启）。

## 6. 已知风险

- `register_ready` 通过 `pending.manager.take()` 抑制 `PendingTicket::Drop` 的自动摘除（std `RwLock` 不可重入，`Drop` 内再取同一把写锁会死锁）；挂起名额的归还因此分散在 `register_ready`/`forget_pending`/`apply_revoke` 三处，均带 `debug_assert` 下穿检查。改动这段时必须同时检查 `pending_slots` 记账（用例 `pending_ticket_drop_returns_slot` 只覆盖 `Drop` 路径）。
- 轮询节奏依赖 `PollScheduler::due()` 的"超过 `tick_cadence` 即视为暂停只推进相位"语义：`spawn_poll_driver` 的 nap 取 `cadence/3`。若以后调小到与调度抖动同量级，会退化为"永不派发"（用例 `poll_period=120ms/cadence=40ms` 即为此留出余量）。
- `Capabilities` 未派生 `Serialize`，`DeviceLiveView.capabilities` 是显式 typed 投影（7 个字段），字段增减需同步 `project_capabilities`。

---

## 7. 第一轮修复（Task5-fix）：审查 13 项 + 2 补项逐条闭环

本节是对上面 §1–§6 那一份"首轮交付报告"的**修复轮追加**。上面各节作为历史记录保留（含首轮评审的失败结论），其**过度声明**在 §7.2 逐条更正；未被更正的部分仍然有效。

- 基线：`HEAD = 7adbb0c189f91eda0124b6546ed8b7121e0a8ee2`（分支 `dev-aghost`，未提交、未推送，本次仍不做任何 git 写操作）。
- 唯一编译/测试目标目录：`CARGO_TARGET_DIR=target/tunnel-controller-manager-fix`，命令一律 `--locked --offline`。
- 写入范围（严格）：`src/transport/manager.rs`、`src/transport/listener.rs`、`src/transport/mod.rs`、`src/transport/limits.rs`（新增）、`src/main.rs`、`tests/transport_manager.rs`、新增 `tests/transport_manager_fixes.rs` 与 `tests/transport_listener.rs`，以及本报告。已审的 `transport/admission.rs`、`transport/board_client.rs`、`polling/snapshot.rs`（`SnapshotStore`）、protocol/tunnel/board/DB schema **只读复用，未改一行**；SQL 真相修复是在 `manager.rs` 内新写一条完整只读查询并复用已审的 `AdmissionRow`+`classify_row`，不新建第二套判定。
- 修改前的六个文件副本留档在 `target/tunnel-controller-manager-fix/snapshot-before/`（构建目录内的过程产物，不是交付物）。
- 方法论：逐条 finding 先 RED 复现 → 根因最小修复 → 永久回归。竞态一律用 `Barrier` / `tokio::join!` / 固定延迟制造**确定性**交错，不写"跑十万次碰概率"的测试；生产接口不引入任何测试专用开关（`Arc<dyn AdmissionPolicy>` 是策略本身的注入点，生产注入 `SqlxAdmissionPolicy`）。
- 替身纪律：`tests/transport_manager_fixes.rs` 里的 `FakeConnection` **会解码并校验真实 `Query` 请求体**（用已审 `BusinessCodec::decode_business` + `BusinessPayloadKind::Query` + `schema_version`），并有反向用例 `p1_1_empty_payload_is_caught_by_the_fake` 证明"空请求体一定被判违规"——否则该组测试就是自证清白。

### 7.1 逐条 ADDRESS / 证据 / 未验

> 记号：`fixes` = `tests/transport_manager_fixes.rs`，`listener` = `tests/transport_listener.rs`，`manager` = `tests/transport_manager.rs`（既有 27 项）。

#### P1-1 轮询发的是空 payload，不是真实编码的 `Query`

- **ADDRESS**：抽出唯一的 `request_on(&Arc<dyn DeviceConnection>, action, expected)`，请求体一律由 `encode_business(..., BusinessPayload::Query{schema_version: BUSINESS_SCHEMA_VERSION})` 现场编码（`encode_query_payload()` 辅助），响应再按 `expected` 的 `BusinessPayloadKind` 解码；`poll_device` 的 status/capabilities 与 `DeviceRpc::request` 共用这条编解码线，不存在"给 action 发空字节"的第二路径。
- **证据**：`fixes::p1_1_polling_sends_real_query_payload`（轮询驱动真实发出 ≥2 次可解码 Query，`query_violations()==0`）；`fixes::p1_1_empty_payload_is_caught_by_the_fake`（反向确认替身能抓到空 payload）；`manager` 侧 27 项仍全绿。
- **未验**：真实板端对同一请求体的接受行为（需要双进程）。

#### P1-2 快照"读当前会话 → confirm_generation → update"三步跨锁（首样本 TOCTOU）

- **ADDRESS**：`ingest_status_sample` 在**同一次管理器写锁**内完成"当代际校验 → `confirm_generation` → 标志位写入 → `store.update`"，锁序固定为 manager→store，临界区内不 await、不回调；旧代际一律 `SampleOutcome::StaleGeneration` 且对 store 零副作用。
- **证据**：`fixes::p1_2_first_sample_cannot_lease_generation_for_old_session`（8 路旧代际与 1 路新代际首样本同栅栏并发：旧代际全部 `StaleGeneration`，新代际必 `Accepted`，store 里只剩新代际样本）；`manager::current_generation_sample_is_confirmed_once_and_written`、`manager::old_generation_late_activity_call_and_sample_are_all_rejected` 仍绿。
- **未验**：`SnapshotStore` 与真实 HTTP 读线在高并发下的端到端新鲜度（属 04/06 线）。

#### P1-3 `main` 不持监听句柄、GOAWAY 排空窗口不真实

- **ADDRESS**：`main.rs` 用 `oneshot` 结果通道 spawn 监听，**自己保留 `JoinHandle`** 并完整 `await`；监听内部关停尾部为：`drop(listener)`（停接新）→ `manager.shutdown_all()`（请求 GOAWAY 排空）→ 有界 `JoinSet` 等到 `drain_grace`（= `SessionConfig.drain_timeout` + 1s 余量，`validate()` 保证 `>= drain_timeout`，而 `SessionConfig::default().drain_timeout >= 5s`，即 §6.6 的 ≥5s 窗口**不是靠主函数 sleep 假造**）→ `tasks.shutdown().await` → `manager.drain_close_outs(close_out_grace)` 逐个 `await`/abort 后确认。后台任务经 `JoinSet` 汇聚（`BACKGROUND_GRACE=2s`），板端监听另有 `BOARD_SHUTDOWN_GRACE=30s`；`std::process::exit` 已删除，监听失败也走正常关停并由顶层返回错误。
- **证据**：`listener::drain_grace_shorter_than_session_drain_is_rejected`、`listener::env_budget_is_the_single_source_for_manager_and_listener`（断言 `session.drain_timeout >= 5s` 且 `drain_grace >= drain_timeout`）、`listener::global_handshake_rate_limit_denies_new_connections` 与 `listener::source_quota_denies_excess_unauthenticated_connections`（关停后监听**自己返回 Ok**，`tasks_active()==0`）；`manager::all_background_tasks_terminate_on_shutdown_and_close_outs_drain`、`manager::same_key_replacement_kicks_old_then_closes_only_old_after_delay` 仍绿。
- **未验**：真实线路上确实发出 h2 GOAWAY 帧、以及双进程下"≥5s 排空窗口内新请求被拒而存量完成"的时间曲线。本轮 loopback 用例的对端不发合法 ClientHello，握手停在读帧阶段，**不构成**该项证据。

#### P1-4 半开名额活到会话结束（未 ready 的 backlog 挤占 ready 会话预算）

- **ADDRESS**：`HalfOpenPermit` 作为 `NewSession.half_open` 字段沿握手路径传递，**只在 `register_ready` 成功那一刻**随登记转移并 drop；所有拒绝路径（`Revoked`/`TicketMismatch`/`StaleHandshake`/预算不足）同样在返回前归还，`apply_revoke`/`shutdown_all` 亦按实际移除条目归还。半开在**昂贵握手之前**获取（`run_connection`），未认证 backlog 因此受真实预算约束。
- **证据**：`fixes::p1_4_half_open_permit_is_released_at_ready_not_at_session_end`（`max_half_open=1`：登记成功后 `half_open_count()==0` 且名额立即可复用）；`fixes::p1_4_unready_backlog_cannot_grow_past_half_open_budget`；`manager::cancelled_pending_refuses_registration_of_revoked_identity`（补 `half_open_count()==0` 断言）。
- **未验**：真实 accept 队列在 SYN flood 下的表现（内核侧行为，未测）。

#### P1-5 预算配置被解析后被丢弃（管理器仍拿 `Default`）

- **ADDRESS**：`TransportEnv::from_env() -> TransportEnv::{Enabled(TransportConfig), Disabled}` 是**唯一**入口（`main.rs` 在构造管理器之前调用）；`TransportConfig::manager_config()` 把同一份 `ManagerConfig` 同时交给监听侧连接任务上界与 `TransportManager::try_new`；监听循环用 `connection_task_budget()`（`checked_add`）拒绝超额新连接，而不是 `saturating_add` 出一个假上界。
- **证据**：`listener::env_budget_is_the_single_source_for_manager_and_listener`（`MAX_HALF_OPEN=7`/`MAX_SESSIONS=9` ⇒ `manager_config()` 与 `config.manager` 一致、`connection_task_budget()==Some(16)`、且**不等于** `ManagerConfig::default()`）；`listener::connection_task_budget_is_a_checked_sum`；`fixes::p2_13_illegal_manager_config_fails_closed`。
- **未验**：真实 systemd/env 文件装配（部署线，属打包 writer）。

#### P1-6 旧连接的异步回包会把新会话改回旧状态（缺回包后重核）

- **ADDRESS**：RPC 在 `await` 之后**重新核对** `session_is_current(key, generation)` 且连接仍 `Ready`，否则丢弃回包并回 `BoardClientError::DeviceOffline`；`note_activity` 只在重核通过后调用（旧代际的迟到活动不再刷新 `last_seen`）。登记侧 staleness 判据使用**本地单调 `ready_sequence`**，绝不比较随机 `Uuid` 顺序；更旧的挂起握手在 `register_ready` 里被 `StaleHandshake`/`Revoked` 拒绝。
- **证据**：`fixes::p1_6_reply_from_superseded_connection_is_dropped`（`join!` 确定性交错：回包到达时会话已被顶替 → 丢弃 + typed 离线 + 旧连接仍按 §4.7 收尾）；`fixes::p1_6_older_handshake_cannot_replace_newer_session`（当前会话不被逆向顶替）；`manager::old_generation_late_activity_call_and_sample_are_all_rejected`。
- **未验**：真实网络乱序/迟到帧下的表现（需要双进程与丢包环境）。

#### P1-7 撤销/关停在"已取消但仍占名额"的挂起握手下泄漏

- **ADDRESS**：名额归还**只按实际移除的条目数**（`entry.pending.len()` 在单次写锁内统计），`cancel()` 的首次翻转次数仅作为可观测量 `cancelled_pending`，**不参与**记账；不存在"取消标志位等于已归还"的假设。
- **证据**：`fixes::p2_10_revoke_releases_slots_of_already_cancelled_pendings`（先 `cancel()` 再 `apply_revoke`：`cancelled_pending==0` 而 `pending_count()` 必须归 0——这正是审查报告里被复现的缺陷）；`fixes::p2_10_shutdown_all_releases_every_pending_slot`。
- **未验**：真实 `after_commit` 通道在库撤回风暴下的背压（通道容量仍是常量 256）。

#### P1-8 只读真相查询不投影 `archived`（归档设备被判仍批准）

- **ADDRESS**：`CURRENT_ADMISSION_SQL` 完整投影 `admission_state, review_decision, revision, CAST(archived AS SIGNED)`，结果行喂给**已审**的 `AdmissionRow` + `classify_row`；判定入口统一为 `AdmissionTruth::load_current -> AdmissionDecision`，`decision_admits()` 只认 `Approved`。因此"归档/非法组合/读不到"全部 fail-closed，且没有第二套判定实现。
- **证据**：`fixes::p1_8_current_truth_query_projects_archived_and_stays_read_only`（字符串守卫：含 `archived`、无 `FOR UPDATE`、单条语句、只读）；`fixes::p1_8_decision_mapping_admits_only_approved`；`fixes::p1_8_revoke_on_truth_change_sweeps_existing_session`；`manager::recheck_uses_sql_truth_and_revokes_on_change_or_read_failure`；判定本体仍由已审 `tests/transport_admission.rs` 覆盖。
- **未验**：**未连真实 `devices` 表**。列名/类型来自既有 schema，本轮只做过字符串与映射一致性验证，真库列存在性与 `CAST` 行为未实测。

#### P1-9 复查无期限、无并发上界、不可被关停取消

- **ADDRESS**：每次真相读取包 `admission_read_timeout`；整轮包 `admission_sweep_deadline`，超期即 abort 剩余项并计数 `admission_sweeps_dropped()`（剩余项留给下一轮，不是"全部撤光"也不是"悄悄跳过"）；并发受 `admission_recheck_concurrency` 限制（按波次跑 `JoinSet`）；`spawn_admission_recheck` 用 `select!` 监听关停，一轮未结束也能退出。读不到真相 ⇒ 撤销既有会话（fail-closed）。
- **证据**：`fixes::p1_9_recheck_bounds_each_truth_read_and_fails_closed`（挂起的 SQL 被单条期限截断并撤销）；`fixes::p1_9_sweep_deadline_stops_round_and_is_observable`（整轮 200ms 内中止，`admission_sweeps_dropped()==1`，剩余会话留待下一轮）；`fixes::p1_9_recheck_task_is_cancellable_on_shutdown`。
- **未验**：真实库慢查询/连接池耗尽下的节拍漂移。

#### P2-10 挂起名额记账与取消标志耦合

- **ADDRESS**：同 P1-7（同一处修复）；`track_pending` 在同 key 上**取代更旧的挂起握手**并在同一把写锁内完成插入+取代+计数，`pending_count()` 恒等于 `pending_slots`。
- **证据**：`manager::pending_ticket_drop_returns_slot`（按 §4.6 第 3 条改写：同 key 的第二次挂起会取消并摘除第一次，计数保持 2）、`fixes::p2_10_*` 两项。
- **未验**：同上（背压场景）。

#### P2-11 轮询 child 是 spawn-and-forget（关停后仍在发请求）

- **ADDRESS**：`spawn_poll_driver` 是唯一 owner，child 进 `JoinSet`；退出路径先停止派发，再 `children.shutdown().await`（abort 并 join 每一个在途 child）后才返回；在途总量受 `max_concurrent_polls` 信号量约束，超出即丢弃本轮（不排队积压）。
- **证据**：`fixes::p2_11_poll_driver_joins_inflight_children_before_returning`（400ms 慢连接制造在途 child，owner 返回时 `inflight()==0`）；`manager::poll_driver_writes_current_generation_then_terminates`、`manager::poll_driver_stops_polling_after_revoke` 仍绿。
- **未验**：真实板端在 abort 时刻的 socket 收尾观测。

#### P2-12 板端事件流没有"当前 ready 的 Control 会话"闸门

- **ADDRESS**：事件投递用 `EventGate{closed,inflight}` 的 claim/commit 语义：`claim_task_result(&key, generation)` 要求连接 `Ready` 且代际为当代际，闸门**与摘除/撤销同一把写锁**关闭；投递方拿到的是携带 `manager/public_key/generation/gate` 的 `TaskResultContext`，落库前必须 `is_current()`。`on_event` 侧还要求流为 Control、action 为 `device.task.result`、payload 能被真实 codec 解码为 `TaskRecord`，四类不匹配分别计数（`events_dropped_stream/_action/_decode/_stale`）。
- **证据**：`fixes::p2_12_task_result_context_is_valid_only_for_current_ready_generation`：顶替后旧上下文立即失效、未 ready 不可投递、撤销后既领不到也已领出的失效。
- **未验**：**`TaskResultSink` 的生产写入口仍不存在**，装配注入的是 `RejectNoTaskResultSink`（一律 `false` + `events_unwired` 计数）。任务状态推进需要 `tasks::service` 的已审写入口，属后续任务；本任务不伪造推进。

#### P2-13 环境配置非法值被当成"未配置"静默吞掉

- **ADDRESS**：`TransportEnv::from_env()` 严格区分"缺失"与"非法"：只有 `RSETUP_CONTROLLER_KEY_FILE` **完全缺失**才是 `Disabled`；设了但为空、非数字、为 0、超硬上界、非 UTF-8（`NotUnicode`）一律 `Err(ListenerError::Config)`，且错误文案不回显路径/私钥。数值项全部经 `bounded_usize/bounded_u64/bounded_duration` 与 `ManagerConfig::validate()`/`TransportConfig::validate()` 检查（含 `poll_tick_cadence <= poll_period`、`admission_read_timeout <= admission_sweep_deadline`、`drain_grace >= session.drain_timeout`），硬上界常量集中（`MAX_HALF_OPEN_LIMIT=4_096`、`MAX_SESSIONS_LIMIT=16_384`、`MAX_CONCURRENT_POLLS_LIMIT=4_096`、`MAX_RECHECK_CONCURRENCY_LIMIT=256`、时长上界 `MAX_MANAGER_DURATION=1h`）。
- **证据**：`listener::env_missing_key_file_means_disabled_not_error`、`listener::env_empty_value_is_an_error_not_a_default`（并断言不回显路径）、`listener::env_zero_and_over_hard_bound_are_errors`（8 组非法输入逐项拒绝）、`listener::env_invalid_utf8_is_an_error_not_swallowed`（**子进程**用非 UTF-8 `OsString` 构造输入，父用例断言错误点名 `RSETUP_TUNNEL_MAX_HALF_OPEN` 与 `not valid UTF-8`；这是旧实现 `.ok()` 吞掉 `NotUnicode` 的确定性回归）、`listener::listener_reports_listen_address_failure_to_the_caller`。
- **未验**：`RSETUP_TUNNEL_*` 与部署 env 文件/systemd unit 的联调（打包 writer 的范围）。

#### 补项 A：采集失败不投影到快照（fresh 状态一路挂到过期）

- **ADDRESS**：`record_sample_error(&key, generation) -> bool` 走 `SnapshotStore::record_error`——**保留最后好样本与单调年龄**，只附加去敏固定错误码 `SNAPSHOT_ERROR_CODE="collection-failed"`；只对本代际**已确认过代际坐标**的会话生效，绝不为"有地方写错误"而提前伪造条目（返回 `false` 如实表达）；旧代际零副作用。轮询失败路径确实调用它。
- **证据**：`fixes::supplement_sampling_failure_is_recorded_on_current_generation_only`（错误码写入、最后好样本 `sample_seq` 不被清零、未确认代际返回 `false`、旧代际不改新代际）；`fixes::supplement_poll_failure_projects_error_instead_of_leaving_stale_fresh`（同一当代际连接先成功后失败，错误位由**驱动路径**写入）。
- **未验**：错误码到 HTTP 视图/告警线的呈现（04 线）。

#### 补项 B：无全局与单来源的握手速率/名额限制

- **ADDRESS**：新增 `src/transport/limits.rs`：`HandshakeRateLimiter`（整数令牌桶，`elapsed_nanos * rate / 1e9`，无浮点、时钟不前进则不补 tokens、burst 上限）与 `SourceQuota`（单来源未认证名额 + 来源表容量上限 + 空闲 TTL，`SourcePermit` 为 RAII，**ready 时释放**，因此 NAT 后同 IP 的已认证设备只受 `max_sessions` 约束）。accept 循环顺序固定为：连接任务硬上界 → accept → 全局速率 → 单来源名额 → spawn；三者都发生在**昂贵握手之前**，且都只是"拒新"，**不写任何准入决定**（资源不足≠审批拒绝）。
- **证据**：`src/transport/limits.rs` 7 项单测（速率补充、burst、时钟重复、来源隔离、表满、TTL 清理、RAII 归还）；`listener::global_handshake_rate_limit_denies_new_connections`（8 连接、桶 1 → ≥4 次速率拒，来源拒 0）；`listener::source_quota_denies_excess_unauthenticated_connections`（同来源上限 2、6 连接 → ≥3 次来源拒，速率拒 0，且已放行的仍被处理）。
- **未验**：真实来源分布（大量不同 /24、IPv6）与 NAT 规模下的调参；真实 SYN flood。

### 7.2 对上一版报告的更正（不得再按原文引用）

1. §2"半开有界、无审批总期限 —— `HalfOpenPermit` 在**整段握手**期间持有"：原文只说了"持有"，**未说何时释放**，而实现让名额活到会话结束（P1-4）。现为"登记成功那一刻释放"，见 §7.1 P1-4。
2. §2"通知丢失 fallback …… 生产=`impl AdmissionStore for DbPool` 的只读 `load`"：原文把它写成"真实 SQL 真源"，但该查询**没有投影 `archived`**（P1-8），归档设备会被判仍批准。现为 `CURRENT_ADMISSION_SQL` + 已审 `classify_row`；"未连真库"这一条仍然成立（见 §7.4）。
3. §2"挂起握手可取消"与 §6 第 1 条"名额归还分散三处 + `debug_assert`"：修复前 `pending_slots` 按 `cancel()` 翻转次数减账，已取消的凭证被摘除时会**双向出错**（P1-7/P2-10，审查报告附 2 已复现）。现归并为"按实际移除条目在单锁内归还"的单一内部路径，§6 那条风险描述作废。
4. §2"DeviceRpc 按当前 key/ready 路由"：原文的"按当前路由"只在**发出前**成立，回包后不重核（P1-6）。现补回包后重核 + 迟到 `note_activity` 拒绝。
5. §2"快照代际只由中控 `confirm_generation`"：不变量表述正确，但实现是三步跨锁，首样本存在 TOCTOU（P1-2）。现为单锁内完成。
6. §2"child tasks 不泄漏"：原文只对**关停编排的** child 成立；轮询 child 当时是 spawn-and-forget（P2-11），监听关停则是主函数 `sleep(6s)` 式宽限（P1-3），后者等价于"驱动已经掉了也照样等满"的假排空。现分别由 `JoinSet::shutdown` 与 `drain_grace`/`drain_close_outs` 承担。
7. §5"监听任务自身失败 → `eprintln!` + `std::process::exit(1)`"：**已改**为把 `ListenerError` 交回顶层，由顶层完成统一关停后返回错误；进程内不再有 `std::process::exit`。
8. §4.10"关停宽限为常量 6s"：现宽限由 `TransportConfig.drain_grace = session.drain_timeout + 1s` 派生并被 `validate()` 约束（不可短于会话排空窗口），仍**未做 env 配置**；撤销通道容量 256 与兜底节拍 5s 仍是常量。
9. §3 的命令与全部测试数字属**首轮**（`target/tunnel-controller-manager-resume`），本轮数字以 §7.3 为准；两处不可混引。
10. §4 的未验清单**全部继续有效**（尤其是"双进程端到端未做""真实库未跑""`device.task.result` 无业务写入口""时钟轮询线未接""恢复模式真源未交付"）。本轮新增的 loopback 用例只覆盖 accept 侧限额与关停返回，**不构成**双进程或 GOAWAY 证据。

### 7.3 本轮实测（全部为真实命令输出，摘录）

```
CARGO_TARGET_DIR=target/tunnel-controller-manager-fix cargo test -p rsetup-controller --locked --offline
  → exit code 0，无 failing 目标；逐目标（本轮相关部分）：
      lib（含 src/transport/limits.rs 的 7 项单测）      998 passed / 3 ignored
      tests/transport_manager.rs（既有 27 项，全部保留）    27 passed / 0 failed
      tests/transport_manager_fixes.rs（本轮新增）          20 passed / 0 failed
      tests/transport_listener.rs（本轮新增）               10 passed / 1 ignored（ignore 的是子进程入口）
      tests/transport_admission.rs（已审，未改）            32 passed / 2 ignored
      tests/transport_board_client.rs（已审，未改）          15 passed
      tests/production_composition.rs                       14 passed
      其余既有目标（observer_contract 68 / web_assets 88 / task_scheduler 39 / gate_udp 25 /
      events_sse 22 / sse_business_events 11 / time_consumption 9 / management_gateway_http 3 / …）全部 ok
CARGO_TARGET_DIR=... cargo test -p rsetup-controller --locked --offline --test transport_listener  ×3
  → 3 次均 10 passed / 1 ignored（无抖动；限额类用例的阈值断言留有 ≥2 的余量）
cargo fmt --package rsetup-controller -- --check → 通过（按包，不用 `--all`，不触碰 `externals/devicesinfo`）
CARGO_TARGET_DIR=... cargo clippy -p rsetup-controller --all-targets --locked --offline
  → 本仓范围零告警（含 lib/bin/全部 test 目标）
```

未运行：需要真实数据库的 `mysql_identity` / `tidb_identity` / `metrics_real_db` / `migration_v4` / `acceptance_runtime`（无授权，也不在本任务范围）。

`tests/transport_listener.rs` 里被 `#[ignore]` 的 `env_child_reports_verdict` 不是跳过：它由 `env_invalid_utf8_is_an_error_not_swallowed` 以 `--ignored --exact` 作为**子进程**调用（非 UTF-8 环境变量只能在子进程里构造），其运行结果由父用例断言。

### 7.4 本轮仍未验（不得当作已交付）

1. **双进程端到端仍未做**：`TcpListener` → `server_handshake` → `SecureIo` → `start_server_session` → `wait_ready` → 登记 这条链依旧只有编译接线 + accept 侧的 loopback 限额用例。`on_request` 的 1002 回包、真实 `on_event` 帧的解码计数、真实 GOAWAY 帧与"≥5s 排空窗口"的线上时间曲线**均未实测**。
2. **真实 SQL 未跑**：`CURRENT_ADMISSION_SQL` 只做过"字符串完整性 + 与被审 `classify_row` 的映射一致性"验证；列是否存在、`CAST(archived AS SIGNED)` 在真库上的行为、慢查询下的期限行为，全部未实测。`CancellableAdmissionPolicy`（握手期取消包装）仍无直接用例——它需要真实池，本轮只做了代码走查与其依赖侧（`AdmissionTruth` 替身 + `PendingTicket` 取消路径）的覆盖。
3. **`TaskResultSink` 生产写入口仍不存在**：装配注入 `RejectNoTaskResultSink`，事件被计数丢弃；任务状态推进需要 `tasks::service` 的已审写入口，属后续任务。
4. **时钟消费者、恢复模式真源、SchedulerStore 的 SQL 线**：与首轮相同，继续是后续任务；本轮未新增任何第二真源，`main.rs` 的 `UnwiredRecoveryProbe` 仍 fail-closed。
5. **来源限额的调参**：`handshakes_per_sec`/`handshake_burst`/`max_pending_per_source` 的默认值是保守工程值，未在真实来源分布与压力下标定；来源表容量上限 4096、空闲 TTL 60s 同样是常量。
6. **`register_ready` 的 `StaleHandshake` 分支在实践中难以单独到达**：`track_pending` 会取消并摘除同 key 更旧的挂起握手，因此旧握手通常先撞上 `Revoked`。回归用例 `p1_6_older_handshake_cannot_replace_newer_session` 因此断言的是"两者之一 + 当前会话不被逆向顶替"，而不是伪造一个只能命中 `StaleHandshake` 的内部状态。

### 7.5 与并行 writer 的边界

- 本节改动严格落在 §7 开头列出的文件内。工作区内其他 writer（会话 driver、板端生产装配、打包）仍在进行，其间 `crates/rsetup-tunnel/src/session/driver.rs` 出现过**瞬时编译失败**（`state` 未定义等 4 条）与 3 条 clippy 告警；本轮**未修改、未回退**其任何文件，最终一次整包运行已恢复正常。
- 结论口径：**连接管理第一轮修复的 13 项 + 2 补项已在语义层逐项闭环并留下永久回归**；这不等于 Task5 整体完成，也不等于生产可用——真实双进程、真实库、真实板端行为与任务事件写入口仍在 §7.4 的清单里。

## 8. 第二轮修复（接续 writer）：7 类根因逐条闭环

**本节的存在理由**：§7 的"全 ADDRESS"里，有 **N1/N2 的生命周期 helper 没有任何测试**、**P1-7 条目通篇讲的是 P2-10 而 ready 期取消零回归**、**N3 的注释口径比实现更强**这三处属于"符号已落地但未按其声称被证明"。本节逐条更正、补齐并给出可复现命令；§7 的历史文字**保留不删**，但其结论以本节为准。本轮不重做 §7 已通过的项，不改动 admission / boardclient / `SnapshotStore` / schema / 其它 crate。

### 8.0 接手状态与本轮实际改动

接手时**已存在**（前任 writer 落地，本轮逐项核行为而非只看符号）：`watch_turns_true` / `BoardTask` / `finish_board` / `supervise_shutdown` / `settle_background_tasks`、`recheck_cursor` 轮转续扫、`SourceRateLimiter` 与 accept 侧接线及计数、`refill` 的 fraction 保留、`TransportEnv::from_env` 的严格校验顺序。

本轮**实际改动**（5 处编译/告警修正 + 1 处注释更正 + 9 个新回归用例）：

1. `main.rs` `finish_board` 两条产出错误的 `"...".into()`：`BoxedError = Box<dyn Error + Send>` 没有 `From<&str>`（std 只对 `+ Sync` 提供该 impl），这是 `cargo check --bins` 的真实编译错误。改为 `boxed(std::io::Error::other(..))`，与全仓既有惯用法一致；不改 `BoxedError` 的定义，避免牵动 HTTP 侧错误装箱。
2. `main.rs` `supervise_shutdown` 末尾 `board_error.flatten()`：双层 `Option` 只存在于函数内部的 `board_done`（外层＝"board 是否已被观察过"），产出已经是 `Option<BoxedError>`。`flatten` 既是类型错误也是对语义的误读，直接去掉。
3. `finish_board` 形参的 `mut handle`：tokio `JoinHandle::abort(&self)`，`mut` 是 `unused_mut` 告警源（clippy `-D warnings` 会红），去掉。
4. N3 的文档注释按实现改写（见 §8.3）。
5. `finish_board` 的 `let Some(..) = board else { return None }` → `let BoardTask { handle, done } = board?;`（clippy `question_mark`，`-D warnings` 下是错误）。
6. `transport_listener` 用例里 `TransportEnv::from_env().err().expect(..)` → `unwrap_err()`（clippy `err_expect`）。语义不变，断言并入 `matches!` 的失败消息。

（第 5、6 条是上一轮 fix2 在途留下的 lint 存量，不在 §7 的 13 项之内。）

### 8.1 N1（P1）监听失败与 HTTP 缺并发监督 — ADDRESS

实现（**生产 main 的唯一路径**，不是测试专用装配）：

- OS 信号、board 意外结束、HTTP 自身结束**三条关停来源共用同一个 `watch`**；HTTP 的 `with_graceful_shutdown(watch_turns_true(..))`（[`main.rs:449`](crates/rsetup-controller/src/main.rs#L449)）因此可被 board 侧驱动，不再"只等 OS 信号"。
- [`supervise_shutdown`](crates/rsetup-controller/src/main.rs#L501)：`http` 与 `board` 都被 `Box::pin` **持有**，`select!` 只借用 ⇒ 不丢弃未收尾的 HTTP；board 先结束（`Err`/发送端被丢弃/提前 `Ok`）**立即** `send(true)` 广播；board 未赢时 HTTP 结束后再 await 它到自行收尾，绝不重复 poll 已消费的 oneshot；返回 `(http 结果, board 错误)`，优先级由顶层决定（HTTP 失败先报，board 随后）。
- [`finish_board`](crates/rsetup-controller/src/main.rs#L464)：宽限期内等 `done`；`Ok(Err(_))`（panic/取消）与超期两条路径都 **abort 原始 `JoinHandle` 再 `await` 确认退出**，错误交顶层返回。bind 失败仍由 `run_transport_listener` 上抛（既有 `listener_reports_listen_address_failure_to_the_caller`）。

测试（驱动的就是上面这几个生产 helper）：

| 用例（`main.rs` tests） | 断言 |
| --- | --- |
| `n1_board_error_broadcasts_shutdown_and_finishes_the_http_that_never_returns` | HTTP 侧**除关停广播外无任何返回路径**；board 早结束后：广播已发出、HTTP 自身的完成标志为真（不是"future 被丢弃"）、board 错误上交 |
| `n1_board_task_panic_is_observed_and_its_original_task_is_aborted` | 发送端被丢弃 ⇒ 产出错误 + `abort_handle.is_finished()` + DropWitness 置位（原 future 被展开） |
| `n1_board_task_overrunning_the_grace_is_aborted_not_detached` | 宽限期到点 ⇒ 原任务被 abort+await，不是 drop 句柄（detach 时 witness 永不置位） |
| `n1_supervise_joins_both_sides_when_board_task_hangs_after_panic` | `supervise_shutdown` + `finish_board` 组合端到端：两侧都结清、错误上交、watch 已广播 |

### 8.2 N2（P2）后台超时路径 detach — ADDRESS

[`settle_background_tasks`](crates/rsetup-controller/src/main.rs#L549) 收的是三个后台任务**真实的 `Vec<JoinHandle<()>>`**（撤销消费 / 准入复查 / 轮询驱动），宽限期内轮询原始句柄的 `is_finished()`，超期 `abort()` **原句柄**再 `await` 确认。没有"spawn 一个 wrapper 再 abort wrapper"的形态。

测试：`n2_settle_aborts_the_original_owner_tasks_not_a_wrapper`（①会随 watch 自行退出的任务正常收尾；②**不响应 watch 的真实 pending 任务**被 abort：`abort_handle.is_finished()` + DropWitness 置位 + 耗时确实用满宽限期）、`n2_settle_returns_early_when_every_owner_exits_by_itself`（全部自行退出时远早于 30s 宽限期返回）。

### 8.3 N3（P2）整轮复查 deadline 的尾部饥饿 — ADDRESS（口径同时更正）

实现：[`recheck_all_sessions`](crates/rsetup-controller/src/transport/manager.rs#L1380) 从 `recheck_cursor` 之后开始本轮（`active_keys()` 的 `sort_unstable` 只作稳定枚举基准，不当天旧），整轮自然结束 ⇒ 游标复位 0；被 `admission_sweep_deadline` 截断 ⇒ 游标按本轮**已 join 完成**的 key 数前进，在途子任务随 `JoinSet` drop 被 abort 且**不**计入"已复查"。

**对 §7/旧注释的更正**：原文"被 deadline 取消的在途 key 与尚未开始的 key 都留在游标之后"按实现**不成立**——游标按"完成数"前进，理论上可越过一个被取消的位置。注释已改写为真实规则与真实界：`ManagerConfig::validate` 强制 `admission_read_timeout <= admission_sweep_deadline` ⇒ 轮首一波读取必在期限内返回（读到或单条超时 fail-closed）⇒ **被截断的轮次游标至少前进 1** ⇒ 游标 ≤ `len` 轮必然绕回一次 ⇒ 任何当前 key（含本轮被取消者）最迟在一整轮遍历后重新排到轮首被**实际读取**，不通过即 fail-closed 摘除。最坏收敛界以成员集合停止增长为前提，约为 `ceil(len / 每轮前进数) × admission_recheck_interval`；持续新增会话时不能把当时的 `len` 当固定上界。默认生产环境装配的复查间隔是 **30s**（`listener::DEFAULT_ADMISSION_RECHECK_INTERVAL`），不是沿用旧说明中的 5s；1024 条会话下收敛可能远长于该节拍，**复查节拍不是撤销上界**（§7 若被引用为该意，作废）。

测试（都是 `start_paused` 虚拟时钟，无概率循环）：

- `n3_sweep_rotation_terminates_tail_starvation`（既有用例，保留）：6 会话、头 3 慢 80ms、尾 3 已撤销慢 90ms、并发 2、整轮 150ms ⇒ 断言尾部 `calls>0`（旧实现 `==0`）、尾部全部被摘除（收敛，而非"dropped>0"）、且场景确实以截断为前提。
- `n3_key_cancelled_mid_round_is_rechecked_next_round`（**本轮新增**，正面回答"已启动却被取消的 key"）：并发 1、单条 150ms、整轮 150ms、每条 140ms ⇒ 第 1 轮 K1 被读到并摘除、K2 **已启动却在 150ms 被 abort**（读取未完成 ⇒ 会话仍在，既未摘除也不算复查）；后续轮里 K2 重新排到轮首被**真正读到并摘除**（`calls_of(K2) >= 2`），4 条身份 ≤4 轮全部收敛。

### 8.4 补 B：缺来源握手速率 — ADDRESS

[`SourceRateLimiter`](crates/rsetup-controller/src/transport/limits.rs#L320)（按真实 `peer.ip`）与 [`SourceQuota`](crates/rsetup-controller/src/transport/limits.rs#L163)（并发名额）是**两张独立的有界表、两个正交维度、两个拒绝原因**，速率**不替代**名额：

- 桶按 IP 持久，**不随连接 ready/失败/取消归还**；条目只在"距最近一次握手尝试超过 60s"时淘汰，且 `try_acquire` 每次尝试都刷新 `last_seen` ⇒ 攻击者靠"快速建连再释放名额"**重置不了同来源桶**（专门用例 `source_rate_bucket_is_not_reset_by_churn_only_idle_evicts`）。
- 表硬容量 4096：满时只淘汰已过 TTL 的空闲来源，仍无空位就**拒新来源**，绝不驱逐活跃来源、绝不无界增长（`source_rate_table_is_hard_bounded_and_evicts_only_idle_sources`）。
- 默认 8/s/来源、burst 8，`RSETUP_TUNNEL_HANDSHAKES_PER_SOURCE` / `RSETUP_TUNNEL_HANDSHAKE_SOURCE_BURST` 可配（1..=100000、burst≥1，越界 `LimitsError` → `ListenerError::Config`，不静默夹值）。
- accept 顺序固定 ①全局速率 → ①.5 **来源速率**（[`listener.rs:961`](crates/rsetup-controller/src/transport/listener.rs#L961)）→ ②来源名额 → spawn，全部在昂贵握手之前、全部只拒新；NAT 后已 ready 的设备不占未认证名额，但**新握手仍受本来源速率约束**。

测试：limits 内 5 项单测（独立于全局桶、短间隔按 rate 放行、不被 churn 重置、表硬上界、配置越界）+ 真实 listener 用例 `source_handshake_rate_limit_denies_new_connections`（全局速率与来源名额都开到最大 ⇒ 观察到的拒绝只可能来自来源速率；并用两条 `assert_eq!(.., 0)` 锁死"不得混记为全局速率拒/名额拒"）。

### 8.5 N4（P3）令牌 fraction 被丢弃 — ADDRESS

[`refill`](crates/rsetup-controller/src/transport/limits.rs#L77) 不再 `floor(elapsed*rate/1e9)` 后把 `last_refill` 推到 `now`：改为把加权纳秒 `elapsed_nanos × rate` 累入 `accrued`，每满 `1e9` 兑换一枚整数令牌、**余数留到下次**（全程整数、无浮点）；攒到 `burst` 时把余量清零，空闲时间不得在下次"攒出"超出 burst 的透支。`available()` 与 `try_acquire` 复用同一条 refill，观测不额外改变令牌。全局桶与来源桶共用这一实现。

测试（全部注入 `Instant`，无真实 sleep）：`rate_bucket_keeps_fraction_across_short_intervals`（rate 10 / burst 1、60ms 短间隔：第 2 次即按累积时间获准——正是审查给的"22 次全拒"反例）、`rate_bucket_refills_exactly_at_rate_with_injected_clock`（10ms × 1/s 下 1s 恰 10 枚）、`rate_bucket_saturation_does_not_amplify_burst`；来源桶另有 `source_rate_refills_at_rate_across_short_intervals`。

### 8.6 N5（P3）Disabled 分支吞掉非法 env — ADDRESS

[`TransportEnv::from_env`](crates/rsetup-controller/src/transport/listener.rs#L248)：先只捕获 `RSETUP_CONTROLLER_KEY_FILE` 的**存在性**（存在但非 UTF-8 是"显式非法"，立即 `Err`，绝不降级成"当没配"），随后把**其余全部显式项**严格校验（空串、非数字、0、超硬上界、非 UTF-8、非法监听地址、`NotUnicode`），全部合法后才在 [`listener.rs:330`](crates/rsetup-controller/src/transport/listener.rs#L330) 依 key 有无决定 `Disabled` / `Enabled`。走 `Disabled` 时不加载身份、不生成密钥、不建监听；错误文案固定去敏（只含变量名与判据，不回显值，不读无关秘密）。

测试：`env_missing_key_file_does_not_swallow_illegal_numeric` / `env_missing_key_file_does_not_swallow_illegal_listen` / `env_missing_key_file_does_not_swallow_illegal_utf8`（非 UTF-8 只能由子进程构造，父用例断言其输出）+ `env_missing_key_file_with_valid_others_is_still_disabled`（其它项合法仍允许 Disabled）+ `env_missing_key_file_means_disabled_not_error`。

### 8.7 P1-7 ready 之前取消 — 三条要求分别归位

**报告更正**：§7.1 标题为 "P1-7" 的条目通篇讲的是**挂起名额记账**（那是 P2-10 的内容），`wait_ready` 期的取消当时**零回归**。三条要求现在分别是：

| 要求 | 证据 |
| --- | --- |
| (a) 旧 pending 逆向顶替新会话 | 既有 `p1_6_older_handshake_cannot_replace_newer_session`、`p1_6_reply_from_superseded_connection_is_dropped`（§7.1，本轮未改） |
| (b) 名额计数/归还 | 既有 `p2_10_revoke_releases_slots_of_already_cancelled_pendings`、`p2_10_shutdown_all_releases_every_pending_slot`（§7.1，本轮未改） |
| (c) **ready 期取消/关停** | **本轮新增** `p1_7_revoke_during_wait_ready_ends_the_connection_immediately`、`p1_7_shutdown_during_wait_ready_returns_well_before_the_probe_window` |

(c) 的真实装配：对端是真实 `rsetup-tunnel::client_handshake`，握手返回后**完全静默**（不发 h2 preface、不发首 probe），中控侧因此只能停在 `wait_ready`，其失败上界就是 `PROBE_WINDOW = 15s`（rsetup-tunnel 常量）。

- 撤销支：`apply_revoke` 后 **5s 内**（实测远小于 1s）连接任务归零、`pending_count()==0`、`half_open_count()==0`；
- 关停支：监听在 **12s（<15s probe 窗口）内**自行返回 `Ok`，`tasks_active/pending_count/half_open_count` 全部归零；
- 两支都断言 `stats.ready == 0`（测的确实是未 ready 窗口）且 `handshake_policy_rejected == 0 && handshake_failed == 0`（走的确实是 ready 之前的取消分支，而不是握手内策略复核失败的分支）。

入口用的是既有 seam `run_transport_listener_with_stats(.., Arc<dyn AdmissionPolicy>, ..)`；生产仍由 `TransportConfig::admission_policy` 注入 `SqlxAdmissionPolicy`（真实 SQL），本轮未新增任何生产批准开关。

### 8.8 本轮实测（真实命令输出）

全部命令在独立 `CARGO_TARGET_DIR=target/tunnel-controller-manager-fix2-resume` 下执行，参数一律 `--locked --offline`；与 §7.3 分属两轮，不可混引。

```
rustfmt --edition 2024 --files-with-diff <本轮 scope 的 6 个文件>     → 无差异
cargo clippy -p rsetup-controller --all-targets -- -D warnings        → Finished，零告警（lib/bin/全部 test 目标）
cargo check -p rsetup-controller --bins                              → Finished（首轮曾报 3 条，见 §8.0 第 1~3 条）
cargo test -p rsetup-controller --lib transport::                     → 18 passed / 0 failed（991 filtered）
cargo test -p rsetup-controller --test transport_manager              → 27 passed / 0 failed
cargo test -p rsetup-controller --test transport_manager_fixes        → 22 passed / 0 failed
cargo test -p rsetup-controller --test transport_listener             → 17 passed / 0 failed / 1 ignored
cargo test -p rsetup-controller --bin rsetup-controller               → 16 passed / 0 failed
cargo fmt --package rsetup-controller -- --check                      → exit 0（按包，不用 `--all`，不触碰 `externals/devicesinfo`）
```

本轮**新增的 9 个常驻回归**（全部在上述运行中通过）：

| 目标 | 用例 |
| --- | --- |
| `--bin`（main 生命周期 helper） | `n1_board_error_broadcasts_shutdown_and_finishes_the_http_that_never_returns`、`n1_board_task_panic_is_observed_and_its_original_task_is_aborted`、`n1_board_task_overrunning_the_grace_is_aborted_not_detached`、`n1_supervise_joins_both_sides_when_board_task_hangs_after_panic`、`n2_settle_aborts_the_original_owner_tasks_not_a_wrapper`、`n2_settle_returns_early_when_every_owner_exits_by_itself` |
| `transport_manager_fixes` | `n3_key_cancelled_mid_round_is_rechecked_next_round` |
| `transport_listener` | `p1_7_revoke_during_wait_ready_ends_the_connection_immediately`、`p1_7_shutdown_during_wait_ready_returns_well_before_the_probe_window` |

旧用例全部保留：`transport_manager` 27 项、`transport_manager_fixes` 原 21 项、`transport_listener` 原 15 项（含子进程入口那条 `#[ignore]`）均无删改。§7.3 的 10 项（`transport_listener` 10 passed）在合并本轮新增后为 17 passed + 1 ignored，非替换。

未运行：需要真实数据库的 `mysql_identity` / `tidb_identity` / `metrics_real_db` / `migration_v4` / `acceptance_runtime`（无授权，也不在本任务范围）。


### 8.9 本轮仍未验（不得当作已交付）

1. **真实 SQL / 双进程 / GOAWAY 线上时序**：与 §7.4 同等成立。本轮 P1-7 用例走的是 loopback + 内存 `AdmissionTruth`/注入策略，不构成真库或双进程证据。
2. **已 ready 会话被撤销后的排空计时**仍未测（本轮只覆盖"未 ready 之前"）。
3. **N3 最坏收敛界**以成员集合停止增长为前提，是“实现推理 + 小规模虚拟时钟”证据，未在 1024 条真实会话、真库慢查询下量过实际轮数。
4. **来源速率的聚合粒度与调参**：现按单 IP 计数，未做 /24 或 IPv6 前缀聚合，默认值未标定。
5. **main 的监督 helper** 只在测试装配下被驱动；真实 `axum::serve` + 真实监听在同一进程内的关停次序仍未做双进程验证。

### 8.10 第二轮修复的独立复审（bdf296d7）

**Spec PASS / Quality PASS，七项根因全部 ADDRESS，无行为回归。** reviewer 只读当前交付，
独立实跑 check --bins、clippy all-targets -D warnings、main bin 16、transport lib 18、manager 27、
fixes 22、listener 17+1 ignored 及包级格式门禁，全部通过。额外虚拟时钟探针覆盖乱序完成、
取消项重读、成员递减以及增长停止后的收敛，未把注释当作证明。

两项 Low 已由主代理收紧口径：行内注释不再声称所有取消 key 都位于游标之后；收敛界明确
以成员停止增长为前提；默认生产 env 装配间隔纠正为 30s，不把节拍当撤销完成上界。
只改注释/报告，无生产逻辑变更。真实 SQL、双进程、容量与 §8.9 边界仍未验；当前另行进行的
数据库 v4-only 清理不属于本复审，后续必须独立验收，不能据本 PASS 声称生产整体完成。

### 8.11 与并行 writer 的边界（本轮）

- 全程使用独立 `CARGO_TARGET_DIR=target/tunnel-controller-manager-fix2-resume`，未复用其它 writer 的 target；改动前 scope 内 5 个文件快照存于 `target/tunnel-controller-manager-fix2-resume-snapshot/`（构建目录，不入库）。
- 本轮期间 `crates/rsetup-tunnel/src/session/**`（会话 driver writer 在途）多次出现**瞬时编译失败**，实测到的诊断包括：`prune_finished` 未定义、`StreamEvent::StreamClosed` 模式缺 `protocol` 字段、`if let Some(wire) = &self.wire` 对 `Arc<H2WireControl>` 误用 `Option` 模式、`SessionOutcome` 初始化缺 `dropped_replies`。等待其恢复的轮询里，前 8 次 `cargo check -p rsetup-tunnel --lib` 失败、第 9 次通过；本任务全部验证（§8.8）都是在恢复之后同一次会话内跑完的。
- 本轮**未修改、未回退、未格式化** `crates/rsetup-tunnel` 及其它 crate 的任何文件；§8.0 的 3 处修正只落在 `main.rs` 自身（其中 2 处是 `--bins` 的真实编译错误，1 处是 `unused_mut` 告警）。
- 另两处 clippy 存量告警也落在本轮 scope 文件内并已按 lint 建议改写（`finish_board` 的 `let..else` → `?`；`transport_listener` 的 `.err().expect()` → `unwrap_err()`），不改语义。
- 结论口径：**N1 / N2 / N3 / 补 B（来源握手速率）/ N4 / N5 / P1-7 共 7 类根因逐项 ADDRESS，每项都有可复现的常驻回归**；这不等于 Task5 整体完成，也不等于生产可用——真实 SQL、双进程、GOAWAY 线上时间、任务事件写入口、时钟与恢复模式的 SQL 线仍在 §7.4 与本节 §8.9 的清单里。



## 9. RC2：单次生产生命周期的监听期被宽限期杀掉（本轮根因修复）

本轮只改 `crates/rsetup-controller/src/main.rs`（生产 `supervise_shutdown` / `finish_board` 接线与其
内部 `tests`）与本报告的追加章节。未触碰 transport/session/board/DB repo/schema/Cargo/secret，
未连真实数据库、未开网络、未重启服务；全部构建走独立 `CARGO_TARGET_DIR=target/tunnel-real-rc2`、
参数一律 `--locked --offline`；格式按本仓口径 `cargo fmt --package rsetup-controller -- --check`
（实测 fmt 只改动 `main.rs`，未用 `--all`，不碰 `externals/devicesinfo`）。

### 9.1 缺陷事实（真实两进程日志 b52 首次运行的时间线，只读取证）

| 时刻 | 观察到的事实 | 归因 |
| --- | --- | --- |
| 34.66s | 状态线 health 正常返回 | 进程已健康服务，监听器在跑 |
| 35–60s | 批准线持续 503 | **另一条根因 RC1（授权/设备侧，另修）**，与本节无关 |
| **60.05s / 60.09s** | HTTP 开始**拒连**，stderr 出现关停输出，进程随后非 0 退出 | **RC2**：宽限期在监听器启动时武装，到点把健康的板端监听 abort 并广播关停 |
| 94.77s | 脚本的 boardPending 超时与随后 SIGTERM | 发生在拒连**之后** 34s，不可能是关停原因 |

两条必须留档的排除项与口径纠正：

1. **不是「收到 SIGTERM 后排空慢」**：SIGTERM 落在 94.77s，而 HTTP 在 60.05s 就已经拒连；顺序上
   关停信号是结果不是原因。脚本自带的 20s term 是**脚本**的超时，与 `main.rs` 的
   `BOARD_SHUTDOWN_GRACE = 30s` 无关，报告不得把两者混为一谈（早期草稿写过"20s 宽限期"，作废）。
2. **不是 NTP `ready = false`**：该次启动走的是 `SystemFallback`，属于合法的启动路径（§7.2 口径不变），
   与本次退出无因果关系。

### 9.2 根因（实际 source 与 timing，不是推测）

修复前的接线（行号为修复前）：

- `main.rs:97` `const BOARD_SHUTDOWN_GRACE: Duration = Duration::from_secs(30);` — 常量本身没问题；
- `main.rs:737` `let board = board_task.map(|task| finish_board(Some(task), BOARD_SHUTDOWN_GRACE));`
  —— 在**绑好端口、监听器刚启动**时就把「结清 future」构造出来；
- `main.rs:510-529` `supervise_shutdown` 一进来就 `select!` 并发 poll 这个 board future；
- `main.rs:467` `tokio::time::timeout(grace, done).await` —— `timeout` 在**第一次被 poll** 时起表，
  也就是监听阶段起表。

于是宽限期的语义从「关停之后再给 board 的有界清理时间」被悄悄改成了「进程最多正常活 30s」：
运行满 30s ⇒ `Err(_elapsed)` 分支 ⇒ **abort 完全健康的板端监听** + 产出错误 ⇒ 监督据此广播关停
⇒ `axum` 停止接受连接 ⇒ 顶层拿到错误以非 0 退出。60.05s ≈ 启动到绑端口之后满 30s，与实测吻合。

### 9.3 修复结构（最小结构变更，不改宽限期数值）

1. **两阶段监督**（`supervise_shutdown`，现 `main.rs:603` 起）：
   *监听阶段*（`armed == None`）只在 `select!` 里观察三路事件——HTTP 自身结束、板端**原始回执**、
   OS 关停 watch 变 `true`（发送端被关闭＝fail-closed 也算关停）——**不构造任何关停超时**；
   *关停阶段*在关停意图确立的**那一刻**记 `armed = Instant::now()`，之后 `finish_board` 拿到的
   永远是**剩余**宽限期（`grace −(now − armed)`）。
2. **签名改为只收任务凭证**：`supervise_shutdown(http, Option<BoardTask>, grace, &tx, rx)`。
   编译期就再也无法把「已经起表的 `finish_board` future」交给监督——RC2 的错法不可表示。
3. **`BoardTask` 成为所有权凭证**：`handle` + `done: Option<Receiver>` + `observed: Option<BoardOutcome>`。
   监听阶段观察到回执时把 `done` 摘走、结果转入 `observed`，从结构上杜绝「重复 poll 已完成的
   future / 双消费」；`finish_board` 在 `observed` 已存在时绝不重新 poll，只在 `done` 尚在时装表等待。
4. **原始任务的归属**：`finish_board` 超期或回执通道关闭 ⇒ `abort` **原始 `JoinHandle`** 并 `await`
   确认；结果已产出（`Ok`）⇒ 仍然 `await` 原始句柄确认它真退出（旧代码在这里直接返回并把句柄
   drop＝detach）。全程没有 wrapper 任务。
5. **HTTP 原 future 持有到收尾**：`Box::pin` 持有，`select!` 只借用；若板端在宽限期到点先被结清，
   监督之后继续 `await` **同一个** HTTP future，不 detach。
6. **宽限期锚在关停意图**：板端有界清理与 HTTP 的活跃长请求**并发**倒计时，长请求不得延后板端 abort。
7. **接线唯一化**（`run_supervised_lifecycle`，现 `main.rs:695` 起）：`main` 与测试调用**同一个函数**，
   `BOARD_SHUTDOWN_GRACE` 只在这里绑定一次；测试不再自带 grace 参数，也不复制 select 逻辑。
8. 失败路径保持 fail-closed：板端在**任何关停意图之前**就自行返回 `Ok`（监听已死却无错误上抛）
   现在会产出错误交给顶层（不再静默 exit 0），同时仍立即广播关停。`shutdown_rx` 所在 watch 的
   发送端被丢弃按已关停处理（`watch_turns_true` 的 closed 分支）。
9. 未改动：`BOARD_SHUTDOWN_GRACE`（仍 30s，**没有靠调大数值掩盖缺陷**）、`BACKGROUND_GRACE`、
   监听内部 GOAWAY/`drain_grace` 的 5s 锚点与收尾 await、bootstrap 与 time gate 的先后顺序、
   关停路径不重试 mutation、`UnwiredRecoveryProbe` 的 no-new-probes 语义、`tokio::spawn` 限制。

### 9.4 常驻回归（`--bin rsetup-controller`，无网络 / 无 DB / 真实 `tokio::spawn` 任务）

`tests::` 内新增 9 项 + 原 16 项按新签名迁移后全部保留（原 n1/n2 六项语义不变，只改构造方式）：

| 用例 | 钉住的性质 |
| --- | --- |
| `rc2_listening_phase_does_not_arm_the_board_grace` | 监听期推进 **2× 生产 30s 宽限期**（虚拟时钟）且无关停信号：board 原任务仍在途、HTTP 未收尾、watch 仍 false、监督未返回 |
| `rc2_shutdown_intent_arms_the_grace_and_a_healthy_board_drains_by_itself` | `send(true)` 之后才起表；健康监听**自行**排空收尾（`finished` 见证），远小于宽限期返回，`(Ok, None)` |
| `rc2_grace_is_anchored_at_shutdown_intent_not_at_http_completion` | 关停后 HTTP 仍在活跃长请求：越过宽限期时 board **原 future 已被展开**（abort，非 detach），此时 HTTP 尚未收尾；放行后监督仍收尾返回并上交 board 错误 |
| `rc2_without_board_task_the_supervisor_still_shuts_down_cleanly` | `TransportEnv::Disabled` 路径：监听期不自愈结束、关停后干净返回、不凭空产出 board 错误 |
| `rc2_shutdown_already_established_before_supervise_starts_arms_immediately` | SIGTERM 落在绑端口与进入监督之间：立刻起表，不静默等满 30s，也不误记「未经请求的提前结束」 |
| `rc2_shutdown_and_board_failure_in_the_same_round_are_consumed_once` | 关停与 board 失败同轮 ready：只消费一次、不重复 poll、错误不被吞 |
| `rc2_board_ok_without_any_shutdown_request_is_reported_not_swallowed` | 未经请求的提前 `Ok` ⇒ fail-closed 上抛 + 立即广播关停 |
| `rc2_http_failure_broadcasts_shutdown_and_board_drains_by_itself` | HTTP 失败 ⇒ 广播关停；board 自行收尾不记为失败；顶层优先拿 HTTP 错误 |
| `rc2_finish_board_joins_the_original_task_after_its_result_arrives` | 结果写回后任务体仍在收尾 ⇒ 结清必须 join 到原任务真正结束（旧行为＝detach，本用例 RED） |

fixture 与生产同形状：`spawn_board_task` 用真实 `tokio::spawn` + `oneshot` 回执、监听体只等**同一个**
关停 watch、写回结果后任务体即结束（`main` 的 spawn 站点同序）；探针是**只读** `AbortHandle` +
「原任务自行跑到任务体结尾」见证 + `DropWitness`（drop 句柄 detach 不会让它置位）。

### 9.5 RED / GREEN 证据（真实命令输出）

**RED-1（旧接线，直接证明 RC2）**：在旧 API（`supervise_shutdown(http, Some(finish_board(Some(task), BOARD_SHUTDOWN_GRACE)), &tx)`）
上跑同一条监听期不变量断言：

```
$ CARGO_TARGET_DIR=target/tunnel-real-rc2 cargo test -p rsetup-controller --bin rsetup-controller --locked --offline rc2_listening_phase
test tests::rc2_listening_phase_does_not_arm_the_board_grace ... FAILED
监听期（无任何关停意图）绝不得 abort 仍然健康的板端监听原任务
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 16 filtered out
```

**RED-2（`finish_board` 只取结果不 join 原任务＝detach）**：临时把 `finish_board` 的 `Ok(result)` 分支
改回旧实现（`result.err().map(boxed)`，句柄 drop），其余不变：

```
$ ... cargo test -p rsetup-controller --bin rsetup-controller --locked --offline rc2_finish_board_joins
test tests::rc2_finish_board_joins_the_original_task_after_its_result_arrives ... FAILED
原任务必须被 **join** 到真正结束；只拿结果就返回＝drop 句柄 detach
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 24 filtered out
```

**RED-3（缺陷注入到共用接线所走的状态机，证明新用例覆盖真实 composition）**：把
`supervise_shutdown` 的 `armed` 初值改回「进入即起表」（＝RC2 缺陷本身），其余不变，跑整组：

```
$ ... cargo test -p rsetup-controller --bin rsetup-controller --locked --offline rc2_
test tests::rc2_listening_phase_does_not_arm_the_board_grace ... FAILED
监听期（无任何关停意图）绝不得 abort 仍然健康的板端监听原任务；实测 board 原任务在途=false HTTP 已收尾=false 关停已广播=false 监督已返回=false
test tests::rc2_shutdown_intent_arms_the_grace_and_a_healthy_board_drains_by_itself ... FAILED
test tests::rc2_board_ok_without_any_shutdown_request_is_reported_not_swallowed ... FAILED
test result: FAILED. 6 passed; 3 failed; 0 ignored; 0 measured; 16 filtered out
```

该变异体下健康 board 原任务在 30s（虚拟）即被展开、监督停在 HTTP 收尾前不出来——即「健康监听被
宽限期杀掉」这一 RC2 本质；与 §9.1 的两进程时间线的差异仅在于本变异体不再经旧 `select!` 广播，
因此**不**复述成"完整复现 60.05s 拒连＋exit 1 链路"（那条由 RED-1 的旧接线证明）。

**GREEN（最终代码，同一次会话内连续执行）**：

```
$ rustfmt --edition 2024 --check crates/rsetup-controller/src/main.rs            → exit 0
$ cargo check -p rsetup-controller --bins --locked --offline             → Finished
$ cargo test -p rsetup-controller --bin rsetup-controller --locked --offline
  test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.90s
$ cargo test -p rsetup-controller --bin rsetup-controller --locked --offline rc2_   × 5 次
  test result: ok. 9 passed; 0 failed（5/5，无抖动）
$ cargo clippy -p rsetup-controller --all-targets --locked --offline -- -D warnings → exit 0（零告警）
```

格式口径的具体选择：本轮曾按仓库口径执行过一次 `cargo fmt --package rsetup-controller -- --check`，
当时唯一差异就在 `main.rs`，随后 `cargo fmt --package rsetup-controller` 落盘后复跑 `--check` 为
exit 0，并用 mtime 核对确认**该次只改动 `main.rs` 一个文件**。之后并行 writer 的
`devices/repository.rs`、`integrity.rs` 进入在途状态，`--package` 口径开始把它们的差异一并报出，
因此本轮后续的格式验证一律收窄成 `rustfmt --edition 2024 --check <main.rs>`（与 §8.8 的按文件
口径一致），不使用 `cargo fmt --all`，也不代为格式化他人文件。

原 16 项（`listener_holds_until_the_time_gate_finishes`、`fallback_gate_still_listens_*`、
`bootstrap_or_recovery_failure_never_reaches_the_listener`、`pre_listen_*`、
`injected_time_source_tracks_the_same_gate_snapshot`、`default_cargo_run_selects_readonly_server`、
`startup_blocks_bootstrap_and_listener_when_schema_not_ready`、`secret_sink_*`、
`first_boot_secret_is_private_and_never_overwritten`、`n1_*` 四项、`n2_*` 两项）全部保留，
其中 `n1_*` 四项按新签名迁移（board 侧从「假 future」升级为**真实监听任务 + 真实回执**），未删减。

### 9.6 对上一轮独立复审口径的更正

§8.10 那轮复审的结论是「helper 全部单测通过」，但它**只测了 helper 自身**，没有测 `main` 的
startup composition：`finish_board` 的宽限期在 helper 语义下完全正确，被**监听阶段的第一个 poll**
起表才是缺陷，因此"helper 绿"当时无法暴露 RC2。本轮的弥补方式有两层：（a）用例直接跑 `main`
所用的同一个 `run_supervised_lifecycle`（宽限期由生产常量决定，测试不自选数值）；（b）签名不再
接受「已开始的结清 future」。RED-3 就是这条防线的实测：把缺陷塞回状态机，用例立刻变红。

### 9.7 本轮未验与边界（不得当作已交付）

1. **真实两进程 RC2 复跑未做**：本轮按指令只做无网络/无 DB 的进程内计时回归；§9.1 的时间线是
   对既有 b52 日志的只读引用，私有现场未改动、未重跑，因此**不得**声称"真实两进程已 PASS"。
2. **真实数据库未清空、未验证**：`mysql_identity` / `tidb_identity` / `metrics_real_db` 等需要授权
   真库的目标未运行；批准线 503 属 **RC1**，由另一条根因单独修复与验收。
3. **`axum::serve` 的真实排空与 GOAWAY 线上时序**仍未做双进程量测（与 §7.4、§8.9 同等成立）；
   本轮只保证「监督不再在监听期杀监听」以及「宽限期到期时原任务被 abort 并确认」。
4. 并行 writer 边界的实况记录：验证窗口内 `cargo clippy -p rsetup-controller --all-targets` 与
   `cargo check --bins` 曾各**一次**报 `could not compile rsetup-controller (lib)`（例如
   `validate_counts_json_text` not found，属另一 writer 正在改的 `integrity.rs` / `devices/repository.rs`
   的瞬时保存状态；同刻 `--bin rsetup-controller` 测试仍可正常构建运行）。本轮**只各记录一次、未轮询
   他人状态、未复用旧 target 快照冒充当前绿**；最终一次同参数 `cargo check --bins` /
   `cargo test --bin` / `cargo clippy --all-targets -D warnings` 全部通过（上面的 GREEN）。
   本轮改动仅 `crates/rsetup-controller/src/main.rs` 与本报告的追加段落，未修改、未回退、未格式化
   `crates/rsetup-controller/src/{authz,devices}/**`、`integrity.rs` 或其它 crate 的任何文件。

## 10. RC2 收尾（Low）：board 完成分支排在 watch 分支**之前**的同轮误判

本轮范围仍只有 `crates/rsetup-controller/src/main.rs`（`supervise_shutdown` 的一处判定 + 其内部
`tests`）与本报告的追加段落：未改其它源文件、Cargo、migrations/schema、secret，未连真实数据库、
未开网络、未重启服务、未触碰真实 TiDB 现场；未 `cargo fmt --all`（按文件口径
`rustfmt --edition 2024 <main.rs>`）；未 commit / push / clean。全部构建走独立
`CARGO_TARGET_DIR=target/tunnel-rc2-race`，参数一律 `--locked --offline`。§9 的结论与口径不重述、
不改写。

### 10.1 这条 Low 的实际情况与本轮的**首次复现**

上一轮复审给出口径时仍是**静态推断**（分支顺序 + `armed` 判定的读序），§9.4 的用例表里没有覆盖这条
时序，也没有运行过的复现用例；因此下面这段是**本轮新写用例先跑出的 RED**，不是对既有证据的改写。

时序窗口也**不是**「进监督时 watch 已为真」——那条 §9.4
`rc2_shutdown_already_established_before_supervise_starts_arms_immediately` 早已覆盖且一直是绿的。
真实窗口是：监督已经在监听阶段 poll 过，之后在**两次 poll 之间**依次发生

1. 外部关停证据到位（生产＝SIGTERM 把**同一个** shutdown watch 写成 `true`）；
2. 板端监听自行返回 `Ok`（回执 ready）；

并且 HTTP 因活跃长请求仍 pending（HTTP 分支不 ready，不会先替 `armed` 起表）。两者都先于下一次 poll
成立 ⇒ 同一次 `select!` 里 board 完成分支与 watch 分支**同时 ready**，而 `biased` 顺序下先执行的是
board 完成分支。修复前的真实输出：

```
$ CARGO_TARGET_DIR=target/tunnel-rc2-race cargo test -p rsetup-controller --bin rsetup-controller --locked --offline rc2_
test tests::rc2_external_shutdown_arriving_before_the_board_receipt_is_not_unprompted ... FAILED
关停意图在板端回执**之前**就已由外部确立（同一个 watch 已为真），健康监听随后的收尾不得记成未经请求的提前结束（那会让正常的 SIGTERM 关停非 0 退出）；实测 board 错误=板端接入监听在收到任何关停意图前提前结束（已广播关停） HTTP 结果=true 板端自行收尾=true 板端原 future 已展开=true HTTP 已收尾=true
test result: FAILED. 10 passed; 1 failed; 0 ignored; 0 measured; 16 filtered out
```

顶层后果如实：`main` 的错误优先级是「HTTP `Err` 优先，其次 `board_error`」，HTTP 这条路径是 `Ok`，
于是 `board_error` 这条**并不存在**的失败成为进程返回值 ⇒ 一次本该 exit 0 的正常 SIGTERM 优雅关停
以 1 退出。

### 10.2 根因（实际 source，行号为修复前）

| 位置 | 事实 | 归因 |
| --- | --- | --- |
| `main.rs:635-636`（修复前） | `tokio::select! { biased; ... }`，分支顺序 HTTP → **board 完成** → watch → 宽限期 | 同轮多分支 ready 时先执行的是 board 完成分支 |
| `main.rs:645`（修复前） | board 完成分支的**第一句动作**是 `let _ = shutdown_tx.send(true);` | 分支**自己**把 watch 写成 `true` |
| `main.rs:648`（修复前） | 隔两行后用 `armed.is_none()` 判断「是否未经请求」 | 读到的 `true` 有一半是自己刚写的，而 `armed` 因 watch 分支本轮没机会起表仍是 `None` ⇒ 外部意图存在却被判成「没有任何关停意图」 |

反向的诱惑也要留档：把这条判定挪到 `send(true)` **之后**去读 watch 同样错——那时读到的 `true`
无法区分外部意图与本分支自证，会把真正的「未经请求提前 `Ok`」放行（fail-open）。§10.5 的变异实测
就是这条边界的证据。

### 10.3 修复（有限改动：一处判定 + 注释；不动结构、不动数值）

board 完成分支内，在**广播关停之前**取一次**只来自本分支之外**的关停证据（现 `main.rs:661`）：

```rust
let external_shutdown = armed.is_some() || *shutdown_rx.borrow();
let _ = shutdown_tx.send(true);
if let Some(task) = board.as_mut() {
    task.done = None;
    if !external_shutdown && matches!(outcome, Ok(Ok(()))) { unprompted = Some(..); }
    task.observed = Some(outcome);
}
armed.get_or_insert_with(tokio::time::Instant::now);
```

* 两条证据的口径：① `armed` 已定（HTTP 先结束、或 watch 分支已处理过关停）——与修复前
  `armed.is_none()` 完全等价，不改变既有语义；② 进入本分支**此刻** watch 的当前值已为 `true`——
  本轮补的就是这条。用 `borrow()` 而非 `borrow_and_update()`：只读、不推进版本，watch 分支的
  `changed()` 观察不受影响。
* **没有**把 watch 分支挪到 board 分支之前：body 里的 `borrow()` 读的是该分支执行**时刻**的最新值，
  凡发生在本分支之前的外部意图都已能读到；重排只会把同轮回执的消费推迟一轮、并改动既有分支优先级
  （收益为零、回归面为正）。
* **没有**把「发送端被关闭」列成证据来源：本函数按**引用**持有 `&watch::Sender`，发送端在整个监督
  期间必然活着，`has_closed()` 在此不可达；`watch_turns_true` 自身的 closed fail-closed 分支不变，
  真出现 closed 也只落回 fail-closed 一侧（宁可多报错，不静默放行）。
* 未动：`armed` 的首次真实 intent 起表点、关停阶段 `remaining` 的剩余宽限期算法、`task.done` /
  `task.observed` 的成对摘走、HTTP future 的 `Box::pin` 持有与收尾 await、`finish_board` 的
  join/abort 归属、`BOARD_SHUTDOWN_GRACE`（仍 30s）、`BACKGROUND_GRACE`、监听内部 GOAWAY/5s 锚点。
  没有 `unsafe`，不依赖单线程 runtime 假设（修复本身与调度器无关），没有重写 supervision。

### 10.4 常驻回归（新增 2 项 ⇒ `--bin rsetup-controller` 由 25 项变 27 项）

| 用例 | 钉住的性质 |
| --- | --- |
| `rc2_external_shutdown_arriving_before_the_board_receipt_is_not_unprompted` | 监督已在监听阶段 poll 过 ⇒ 两次 poll 之间「外部关停先为真、板端 `Ok` 后到」⇒ 收尾必须 `(Ok, None)`，板端原任务**自行**收尾（`finished` 见证）、原 future 被展开、HTTP 被收尾完成、关停仍广播给后台三任务。**修复前 RED**（§10.1） |
| `rc2_board_ok_between_two_polls_is_still_fail_closed_without_external_evidence` | 同一驱动机制的反向用例：两次 poll 之间**只有**板端提前 `Ok`、没有任何外部证据（watch 仍 `false`、`armed` 仍 `None`）⇒ 仍必须 fail-closed 上抛。钉住「本分支自己 `send(true)` 不能自证关停意图」这条边界（修复前后都必须绿） |

fixture 与生产同形：两条用例都跑**生产** `run_supervised_lifecycle`（宽限期由生产
`BOARD_SHUTDOWN_GRACE` 决定，测试不自选数值），`#[tokio::test(start_paused = true)]` 虚拟时钟，
无网络、无端口、无 DB、无 sleep。竞态用例把外部事件精确安排在监督的**两次手动 poll 之间**
（`poll_once` / `poll_until_ready`，唤醒走 `Waker::noop()`）——这条 race 的先后顺序在真实调度器下
不可确定复现，手动 poll 只是把「两次 poll 之间」钉死；测试里**没有**复制 select，也没有用时间片赌
时序。板端 fixture `spawn_racy_board_task` 与 §9.4 的 `spawn_board_task` 同形状（真实 `tokio::spawn`
原任务 + 只读 `AbortHandle` 探针 + `DropWitness`），区别只是「写回 `done`」由测试执行、放行收尾门后
任务体立刻结束（＝生产契约「写回结果的下一步就是结束」），`finish_board` 照旧 join 这个真实任务。
原 25 项（含 §9.4 的 9 项）全部保留、语义不变，未删减、未放宽断言。

### 10.5 RED / 变异 / GREEN 证据（真实命令输出）

**变异实测（证明判定读序是 load-bearing，不是巧合通过）**：只把 `let external_shutdown = ...` 挪到
`send(true)` **之后**，其余一字未改：

```
$ CARGO_TARGET_DIR=target/tunnel-rc2-race cargo test -p rsetup-controller --bin rsetup-controller --locked --offline rc2_board_ok
test tests::rc2_board_ok_between_two_polls_is_still_fail_closed_without_external_evidence ... FAILED
没有任何外部关停证据的提前 `Ok` 必须 fail-closed 上抛
test tests::rc2_board_ok_without_any_shutdown_request_is_reported_not_swallowed ... FAILED
未经请求的提前结束必须 fail-closed 上抛，不得静默 exit 0
test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 25 filtered out
```

即「读自己刚写进去的 `true`」确实会把真正的未经请求提前结束放行成 fail-open（两条 fail-closed 用例
同时变红）。该变异体随后**逐字回退**为最终代码。

**GREEN（最终代码，同一会话内连续执行）**：

```
$ rustfmt --edition 2024 --check crates/rsetup-controller/src/main.rs                → exit 0
$ cargo check -p rsetup-controller --bins --locked --offline                        → Finished（exit 0）
$ cargo test -p rsetup-controller --bin rsetup-controller --locked --offline
  test result: ok. 27 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.90s
$ cargo test -p rsetup-controller --bin rsetup-controller --locked --offline rc2_   × 5 次
  test result: ok. 11 passed; 0 failed（5/5，无抖动）
$ cargo clippy -p rsetup-controller --all-targets --locked --offline -- -D warnings  → exit 0（零告警）
```

格式口径与 §9.5 一致：按文件 `rustfmt --edition 2024 <main.rs>`（本轮唯一差异就落在本轮新增的测试
段落里，落盘后 `--check` 为 exit 0），不使用 `cargo fmt --all`，不格式化他人文件。

### 10.6 本轮未验与边界（不得当作已交付）

1. **真实两进程未跑**：本轮只做无网络/无 DB 的进程内确定性回归；§10.1 的 RED 是**本轮新用例**的
   实测，不是真实两进程的复现，因此**不得**声称「真实 SIGTERM 现场已复现／已 PASS」。真实两进程的
   复跑仍属 §9.7 第 1 条的未验项。
2. **真实 TiDB 与生产 run 未触碰**：本轮不启动任何进程、不改现场（admin／device／audit 记录原样保留）；
   生产 run 由另一 executor 另行承担。
3. **`axum::serve` 真实排空与 GOAWAY 线上时序**仍未双进程量测（与 §7.4、§8.9、§9.7 同等成立）；
   本轮只保证「外部关停已确立时，板端收尾不再被误记成失败」以及「无外部证据的提前 `Ok` 仍然 fail-closed」。
4. **并行 writer 边界**：本轮改动仅 `crates/rsetup-controller/src/main.rs` 的 board 完成分支判定、
   其文档注释与内部 `tests` 段落，以及本报告的追加段落。验证窗口内
   `cargo check -p rsetup-controller --bins`、
   `cargo test -p rsetup-controller --bin rsetup-controller`、
   `cargo clippy -p rsetup-controller --all-targets -- -D warnings` 均一次通过，未遇到他人文件的瞬时
   编译状态；未修改、未回退、未格式化 `crates/rsetup-controller/src/{authz,devices}/**`、
   `integrity.rs`、`lib.rs` 或其它 crate 的任何文件。
5. **git 状态**：未 `add` / commit / push / clean，未改 index 与历史；本轮产物只有上述两个文件的工作区
   改动与 `target/tunnel-rc2-race` 构建缓存。

## 11. 首次真实运行修复的独立复核与整合门禁

RC2 主体（346987a8）与 Low 竞态收尾（6a620997）分别完成只读独立复核，均无新阻断。
后者独立确认广播前读取外部意图、Err 不被吞、宽限期不早起表与原任务所有权保持；
独立实跑当前 main **27 passed**、RC2 子集 **11 passed**、main 格式通过，前次 Low 已关闭。
历史 RED／mutation 来源是 writer 的记录，reviewer 未临时修改生产源码复现。

主代理随后以稳定当前工作树执行 `make lint`、workspace 全目标离线测试和当前生产
controller／board／schema 工具构建，**整条命令 exit 0（bash-2290）**；工件／源码哈希
记录在构建目录。JSON 投影相关 lib 929 项已另独审。上述证据不等价于修复后真实双进程
通过；现况 TiDB、管理员、设备和审计保留，真实复跑须新空 state、显式现有管理员凭据、
新现况备份与修复后的已核哈希工件，不能复用首次失败的旧二进制或伪首启。


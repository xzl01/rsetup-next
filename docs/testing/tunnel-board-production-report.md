# 板端生产连接（--connect）接线与本地验收

范围：`crates/rsetup-board-agent` 的 `connect.rs`、`session_handler.rs`、`host_source.rs`、
`src/bin/rsetup-board-agent.rs`、`tests/connect.rs`、`tests/session_handler.rs`、
`data/rsetup-board-agent.service`、`debian/rsetup-board-agent.README.Debian`。
把「本地 JSON 控制台」与「生产隧道」彻底分开：生产路径只经显式 `connect` 装配进入网络。

## 已实现

`connect.rs`：`TunnelConnector` 一次尝试的真实链路是 `SocketAddr` 解析 → 10s TCP 连接 →
`set_nodelay` → `client_handshake`（`ClientHandshakeConfig{identity, trusted_servers, descriptor}`，
信任集合来自 validated 配置的显式 pin，无 TOFU）→ `SecureIo::new(.., into_client_record_layers())`
→ `start_client_session` → 把 `SessionHandle` 交给 `SessionBinding`。身份密钥由生产入口
**启动期载入一次**（0600），每条 connection 的 random 与 X25519 临时密钥由握手库每次重新生成。
退避 `backoff_delay` 为 1/2/4…60s 封顶，加 `[0,1s)` CSPRNG 抖动（uuid v4 前 4 字节 + Lemhain，
不依赖墙钟），`ReconnectPolicy::drain` 小于 5s 被抬回 5s；**档位只在会话 Ready 后复位**，
Ready 之前的失败持续升档。`classify_handshake` 只把 `SignedRejection{Revoked|ApprovalDenied}`
判为永久停止，`NoTrustedServers` 判为本端配置故障（78），其余（Io/Timeout/Protocol/对端未认证
拒绝）一律可重试——未认证的拒绝永远不被冒充成「已吊销」。`SessionEndReason` 与
`SessionCloseReason` 的映射是显式表：`Kicked` 绝不映射成 `Revoked`。
会话侧 `Revoked` 走 abort+join 后落 `UNAUTHORIZED` 标记（`O_CREAT|O_EXCL`、0600、单行固定文案、
符号链接目标拒绝、已存在即拒绝覆盖），标记落不进可写状态目录时以 `StopStateUnwritten` 事件显式上报。
`run_board_connection` 覆盖：关停可取消 connect 阶段与握手待定阶段（握手**不设**总审批期限）、
`request_drain` → `join(5s)` 超时升级 `abort()`；收尾只认「join 确认成功」——一次不成先
`abort()` 再确认一次，仍不可确认即以 `Fault(DriverUnreclaimed)` 结束循环（绝不带着未回收的
驱动再 open），SIGTERM 路径保持退出 0 但同样记 `DriverUnreclaimed` 事件如实声明收尾结果；
退出码 Shutdown/永久停止 0，本端故障 78，因此 `Restart=on-failure` 无法绕过停止语义。
环境读取用三分支匹配区分 `NotPresent`/`NotUnicode`/`TooLong`（4 KiB 预算），
绝不把非 UTF-8 当成未设置回落默认值；配置仍走已评审的 `BoardConnectConfig::from_lookup`，
本层不放宽任何符号链接/属主校验，缺 pin 由 validated 构造报 `MissingPins`。

`session_handler.rs`：`BusinessHandler::decide` 只把 Control 流上的 Request 送进
`BoardAgent::handle_wire`（真实 `BusinessCodec` 解码 + 类型化编码），非 Request 不回包，
`tunnel.` 保留前缀不回包（保留命名空间归驱动），数据流业务请求 1003 + 空 payload，
身份恒取握手认证的 `[u8;32]` 公钥，payload/metadata 自报公钥与设备序列号一律无效；
`Indeterminate` 不回假装的成功也不发 1009，而是 `binding.abort()` 后 `pending()` 让当前会话彻底停止。
`SessionBinding` 记住 attach **之前**就已到达的中止意图（意图位与句柄构成一对 `SeqCst`
store/load 握手），attach 之后同步补发，两种到达顺序与并发交错都不会把「要求停止」丢掉。

`host_source.rs`：`boot_id` 只读 `/proc/sys/kernel/random/boot_id` 且必须规范化 UUIDv4，
读不到或格式不符返回 `BootIdUnreadable/BootIdMalformed`，**不存在伪造路径**（原 bin 内联实现里的
`Uuid::new_v4()` 兜底已删除）。`agent_epoch`/`clock_epoch` 每进程一个，`sample_seq` 跨重连单调，
取号是「比较 + 推进」不可分的 CAS 循环 + `checked_add`：到 `u64::MAX` 后计数器**饱和**
（不回绕、不重复发出同一个值）并置耗尽位，此后返回哨兵 0（DTO 自检必拒），序号轴永不回绕。
`reboot_execution=false` 时能力清单剔除 `device.reboot.*`，与「未注入 dispatcher → 1004」一致；
指标读不到时该指标缺席而非回填 0。journal 仍走 `JournalStore::open(.., RealFs)` 的真实恢复/持久化。

`bin/rsetup-board-agent.rs`：新增生产模式 `--connect`（配置/身份/journal/描述符任一失败即 78，
只打印去敏原因，不回显地址、公钥、路径或环境值），`SIGTERM`/`SIGINT` 共用优雅排空路径，
`--once/--watch/--loopback-test-server` 保持独立非生产模式且与 `--connect` 互斥。

## 证据（均 `CARGO_TARGET_DIR=target/tunnel-board-production --offline`）

`cargo test -p rsetup-board-agent` 全绿：lib 11、bin 单测 6、`board_agent_bin` 2、`connect` 28、
`connect_config` 8、`interop_dual_stream` 10、`read_only` 6、`session_handler` 18、`wire_04_*` 等，
exit 0。`cargo clippy -p rsetup-board-agent --all-targets` 对本 crate 零告警；
`cargo fmt --package rsetup-board-agent -- --check` 通过（未使用 `--all`）。
`connect` 集成测试用真实回环 TCP + 真实 v3 双向 pin 握手（测试内自签临时身份，不含任何真实秘密）：
真会话上跑通 `device.status.get/clock.get/capabilities/task.get` 的类型化回包、数据流业务 1003、
错误载荷 1003、合法 `RebootPrepare` 在未接线调度器下 1004 + 空 payload、pin 不匹配握手失败零业务副作用、
签名 reason 2 停止重连、reason 5 持续退避且绝不声称吊销、kick 重连 + revoke 永久停止并落标记。
真实进程冒烟（本机 `target/board-smoke/smoke.py`，非交付测试）：缺 pin → 退出码 78 且 stderr 仅
`connection configuration is invalid`；对端 accept 即断 → `AttemptStarted(0) → Backoff(1) →
AttemptStarted(1)`，随后 `SIGTERM` → 退出码 0、不落 `UNAUTHORIZED`、身份密钥 0600、journal 文件由
`RealFs` 真实生成。

## 未做（不得据此声称已闭环）

1. **真实双进程部署未验证**：以上网络证据全部在同一测试进程内的回环上完成（板端与中控替身同进程），
   未在两台主机/两个真实部署的 `rsetup-controller` 上跑过互通，也未验证生产 `EnvironmentFile` 实装、
   真实 `systemctl enable --now`、journald 下的停止行为与升级路径。
2. 真实 reboot dispatcher 未接线，重启边界仍是 1004；能力申报不声称可执行重启。
3. 中控侧（`rsetup-controller`）与本板端生产装配的握手互认、审批策略与吊销签发链路属其他 writer 范围，
   本报告不代其声称已通。
4. 本 crate 之外的 clippy `-D warnings` 门禁在本次复跑期间被 `crates/rsetup-tunnel/src/session/driver.rs`
   的在编告警阻断（非本 writer 文件，未改动），需在对方收敛后复跑工作区级 `make lint`。

## 独立审查与待修复项（07f1455f）

审查为 **Spec PASS（附 findings）/ Quality Good**；只读独立编译当前源码，54 个定向测试、
单包格式通过，本 crate clippy 零告警，6 个边界探针通过。永久拒绝三入口均落本地停止状态、
未认证拒绝不冒充吊销、身份来自验签公钥等已获验证。但不据此将以下边界视为闭合：

1. `SessionBinding` 在 attach 前 abort 不记录意图，隧道 `SessionHandle::abort` 又使用与业务
   共享的尽力 `try_send`，队列满可丢中止；Indeterminate 不回伪结果已成立，**主动关闭保证
   尚未完整成立，缺真实会话端到端中止测试**。隧道侧已交仍在途的 session writer 收口，
   板端 pending-abort 记忆与端到端回归纳入新修复任务。
2. required EnvironmentFile 不存在时是 systemd 在执行 binary 前拒启动，不是进程退出 78；
   只有环境文件存在但变量缺失/非法才可能进入 binary 的 78 路径。unit/README 需纠正。
3. 第二次 join 超时结果被忽略，极端情况下仍可重连；应停止而不是带未收口驱动再次 open。
4. `sample_seq` 的 fetch_add + 独立耗尽标志在 MAX 并发时理论上可回绕；正常 3200 并发取号
   探针通过不证明 MAX 原子边界。需改为不回绕原子取号并补饱和边界测试。

主代理要求上述四项修复并复审后再收口 Task6，不把 reviewer 的“无伪回包”结论扩大为
“Indeterminate 主动关闭端到端已验证”。既有 clock_quality 恒 observed、无跳钟检测，DNS
只试首地址、启动信号注册小窗口及 PrivateTmp journal 误配置风险作为如实边界保留。
真实双进程/真实 systemd/真 controller SQL/容量仍未验证；主机 reboot 不在授权执行范围。

## 首轮修复与复验（fresh writer，`CARGO_TARGET_DIR=target/tunnel-board-fix`，全部 `--locked --offline`）

范围只在授权的 `connect.rs`、`session_handler.rs`、`host_source.rs`、`tests/connect.rs`、
新增 `tests/session_abort.rs`、`data/rsetup-board-agent.service` 注释、
`debian/rsetup-board-agent.README.Debian` 与本报告；未改 `Cargo.toml`/lock、协议、隧道、
中控与既有 journal，无 commit/push/clean，无 secret、无真库、无真实 systemd/reboot 执行。
每项先 RED（旧实现下失败）再根因修复，RED 证据逐条记录。

### F1 — `Indeterminate` 的中止意图不再依赖 attach 时机（Medium）

根因：`SessionBinding` 只有一个 `OnceLock<SessionHandle>`，`abort()` 未 attach 时是静默
no-op。`start_client_session` 之后、外层 `attach` 之前，驱动已可能在别的 runtime 线程上
服务请求，此时 `Indeterminate` 裁决要求停止就会被凭空丢掉，会话继续在线。

修复：`SessionBinding(Arc<BindingSlot>)` = `OnceLock<SessionHandle>` + `attached`/
`abort_requested` 两个 `AtomicBool`，构成标准 Dekker 握手：`abort()` 先 `SeqCst` 置意图位
再 `SeqCst` 读就位位，`attach()` 先 `SeqCst` 置就位位再 `SeqCst` 读意图位。两侧都是全序
操作，「两边都看不见对方」被内存模型禁止，因此要么 abort 直接中止，要么 attach 事后补发
（**双方任一侧退化成 release/acquire 都不成立**，弱内存序板端这是真缺陷）；就位位与句柄
是同一原子的同步边，看见「已就位」必定读得到句柄；补发在**锁外**执行，不在任何临界区里
回调外部逻辑。`abort_requested()` 让调用方能区分「没人要求停」与「要求已记下等待补发」。
第二次 `attach` 仍返回 `false`（连接层据此 abort 新句柄并报内部故障），旧意图不会转嫁给
新 connection。

新增 `tests/session_abort.rs`：真实回环 TCP + v3 双向 pin 握手 + `SecureIo` + h2 双流会话，
`Indeterminate` 由注入的 `BusinessDispatch` 产生（不是伪网络 handler）。

* `abort_requested_before_attach_still_closes_the_real_session`（**RED 过**：旧实现下
  attach 后 3s 内会话毫无反应，中控侧业务超时才会拆连接）：中控发出真实业务请求、
  业务分发判定 `Indeterminate` 之后才 `attach`；断言驱动以 `SessionCloseReason::Abort`
  结束（3s 窗口 << 会话业务期限 5s，因此只能归因于本端主动中止）、对端在途请求**没有
  任何业务回包**、对端驱动任务收口、业务分发只看到握手认证的中控公钥。
* `abort_after_attach_closes_the_session_without_any_reply`：另一种顺序，同样关闭、
  同样无回包，且关闭后的句柄再请求只得到错误。
* `concurrent_attach_and_abort_always_ends_the_session`：12 轮反复 abort 与 attach 抢
  同一条全序，每轮都必须关闭（幂等，不产生第二个会话）。
* `a_binding_never_rebinds_to_a_new_generation_or_aborts_an_unbound_session`：同一绑定槽
  拒绝第二条 connection 的句柄；关闭第一条不会误关尚未绑定的第二条。
* `binding_state_is_observable_before_any_handle_exists` + lib 单测
  `binding_is_single_shot_and_keeps_an_early_abort`：意图在无句柄时可观察且置位后不丢。
* `indeterminate_on_the_production_path_closes_the_session_without_a_reply`：走**生产装配**
  （`TunnelConnector` + `run_board_connection` + `SharedAgentFactory`），断言在途请求以
  非超时的关闭错误收口（`!= RequestError::Timeout`，即不是「等不到结果」）、循环随后
  重开新代际并 Ready、事件序列含 `SessionEnded` 且不含 `DriverUnreclaimed`，最后
  SIGTERM 收口为 `RunOutcome::Shutdown`（驱动任务已 join）。

跨 crate 的另一半（`SessionHandle::abort` 原来走与业务共享的尽力 `try_send`）由 session
writer 收口为持久优先控制意图；本 writer 未改隧道一行，上述端到端断言跑在**其交付后的
当前工作树**上。命令队列饱和下的中止仍属该 driver writer 的测试范围，本报告不代为声称。

### F2 — required `EnvironmentFile` 的文字更正（Medium）

`data/rsetup-board-agent.service` 与 `debian/rsetup-board-agent.README.Debian` 原来都写成
「未创建该文件时服务以 78 退出」。实际两级失败关闭方向相同、机制不同：文件不存在时
systemd 在 **ExecStart 之前**加载环境文件失败，本次启动直接失败，二进制未被执行，
因此根本没有它的 78；文件存在但必填项缺失或取值非法时，才是进程自己的 78。
两处文字均已更正，并明确 `EnvironmentFile=` **保持 required**（不加 `-` 前缀，未放宽）；
默认 disabled/no-auto-enable、root、state 0700、sandbox 各项均未改动。unit 里还补了
SIGTERM 收尾的如实表述（join 确认不了记 driver-unreclaimed 事件，仍退出 0）。
**未在真实 systemd 下执行过该单元**（本机未运行被打包的 unit），文字依据 systemd 语义更正。

### F3 — join 不可确认时终止，不再 open（Low）

`run_board_connection` 第 4 步原来丢弃第二次 join 的结果，等于「只要放弃确认就能再 open」。
现在第二次 join 仍不可确认即 `AttemptFailure::Fault(FaultReason::DriverUnreclaimed)`
（新增变体）→ 退出 78、循环终止；新增 `ConnectEventKind::DriverUnreclaimed` 事件，
让「收尾不可确认」在任何路径上都可见。会话内 revoke 路径先 `abort()` 再 join 一次，
不可确认就记该事件，但**不改变**吊销的停止语义：仍落 `UNAUTHORIZED`、仍 `Stopped`、
退出码仍 0，`Restart=on-failure` 无从绕过。SIGTERM 路径同样记事件，但退出结果仍是
`Shutdown`（0），不让一次正常停止变成被 systemd 重新拉起的失败。

`tests/connect.rs`：`unrecoverable_join_still_terminates_the_loop` **补强**（**RED 过**：
旧实现下循环永不自行结束，只能靠观察者在第二轮发 shutdown 掩盖）——断言自行返回
`Fault(DriverUnreclaimed)`、`opens == 1`、无 `Backoff` 事件、`joins >= 2`、含
`DriverUnreclaimed` 与 `Fault` 事件；新增
`revoke_with_unconfirmed_join_still_stops_writes_state_and_exits_zero`（收尾不可确认时
仍 `Stopped{Revoked, state_written: true}`、退出 0、`opens == 1`、无 `Fault` 事件）与
`shutdown_with_unconfirmed_join_is_still_a_zero_exit_shutdown`（先排空、仍 `Shutdown`、
事件可见、不被改判 78）。真实驱动任务的 join 可确认性由
`indeterminate_on_the_production_path_…` 断言路径上无 `DriverUnreclaimed` 佐证。

### F4 — `sample_seq` 在上界饱和不回绕（Low）

根因：`fetch_add` 在 `u64::MAX` 处**先回绕再返回旧值**，耗尽标志是另一次写；已过
早检、尚未推进的并发线程会读到回绕后的 0 并返回 1，既复用旧序号又让计数器离开上界。
修复：`next_sample_seq` 改为 `load` + `checked_add` + `compare_exchange` 的 CAS 循环，
「比较」与「推进」同属一次原子操作，上界处不写回任何值。
（主代理用当前 stable `rustc 1.99.1` 的最小探针核实：`fetch_update` 仍可编译，
但已弃用并提示改名 `try_update`；不是 API 消失或已证实闭包签名变化。本仓只要求 stable，
不引入 1.85/MSRV 兼容承诺。选择显式 CAS 可避免该弃用告警，无需扩大工具链口径。）
未新增任何生产侧 test-only setter，测试直接操作模块内私有计数器。

`host_source.rs` 单测（**RED 过**：旧实现下预置 MAX 并发后计数器停在 0 而非 MAX）：
`concurrent_numbering_saturates_at_u64_max_without_wrapping`（预置 MAX + 8 线程 barrier，
全部只能拿哨兵 0，计数器保持 `u64::MAX`，耗尽不可逆）、
`the_last_legal_number_is_handed_out_at_most_once`（预置 `MAX-1`，`u64::MAX` 至多发出一次）、
`concurrent_numbering_stays_unique_below_the_boundary`（正常区间 8×500 并发取号唯一、
最大 4000、不误置耗尽）。未真的数 2^64 次，边界用预置 + barrier 确定性打开竞态窗口。

### 复验证据

`cargo --locked --offline test -p rsetup-board-agent`（`CARGO_TARGET_DIR=target/tunnel-board-fix`）
exit 0，共 179 项通过：lib 14、bin 单测 6、`board_agent_bin` 2、`connect` 30、
`connect_config` 8、`interop_dual_stream` 10、`read_only` 6、`session_abort` 6（新增）、
`session_handler` 18、`wire_04_execution` 36、`wire_04_journal` 23、`wire_codec` 20。
`cargo fmt --package rsetup-board-agent -- --check` 通过（未用 `--all`）；
`cargo clippy -p rsetup-board-agent --all-targets` 对本 crate 零告警（exit 0）。
上一轮记录的「隧道 `session/driver.rs` 在编告警阻断工作区 lint」在本轮工作树里已不再复现
（隧道 crate 可正常编译），但工作区级 `make lint` 仍留给 session 侧独立复核后复跑。
旧测试全部保留（`unrecoverable_join_still_terminates_the_loop` **就地补强**；lib 内原
`binding_is_single_shot_and_aborts_only_when_attached` 改名为
`binding_is_single_shot_and_keeps_an_early_abort`，因为它原来断言的「未 attach 时 abort 是
no-op」正是本次要消除的行为，断言同步加强；未删除任何既有用例）。

### 本轮仍未验证（不得据此声称已闭环）

1. 上述真实网络证据仍在单进程回环上（板端与中控替身同进程），双进程/双主机、真实
   `rsetup-controller` SQL 审批与吊销签发、容量与 GOAWAY 实测均未做，loopback 不等于生产互通。
2. 未在本机真实 systemd 里加载或启动被打包的 unit（F2 只更正文字，未执行）。
3. 真实 reboot dispatcher 仍未接线，1004 边界与能力申报不声称可执行重启；未触发主机重启。
4. 隧道命令队列**饱和**条件下的 abort 归 session driver writer 的范围，本报告的端到端
   用例覆盖的是业务队列不满的正常路径 + 板端 attach 竞态，两者不可互相替代。
5. 工作区级 `make lint`（`fmt-check` + `cargo clippy --workspace --all-targets -- -D warnings`）
   本轮未由本 writer 复跑：本 scope 只保证 `cargo fmt --package rsetup-board-agent` 与
   `cargo clippy -p rsetup-board-agent --all-targets` 干净，其它 crate（含隧道 session 侧）
   由其 writer 与独立复核负责，本报告不代为声称。
6. 如实保留的既有限制：`clock_quality` 恒 `observed` 且无跳钟检测（不伪称 NTP）；
   DNS 只取解析结果首地址；连接/解析/握手各阶段 10s 期限；进程启动到信号注册之间存在
   极短的早期信号窗口；unit 的 `PrivateTmp=yes` 与人工把 `RSETUP_BOARD_JOURNAL_DIR`
   指向 `/tmp`（或依赖临时路径）组合时会把 journal 落到私有可丢弃命名空间而失去耐久性
   ——运维必须把状态目录放在 `/var/lib/rsetup-board-agent`（`ReadWritePaths` 内），
   本单元不为此扩功能也不静默改路径。工作区内未发现真实秘密或越权文件。

## 首轮修复的独立复审（78d48e1a）

**Spec PASS / Quality Good，F1–F4 全部 ADDRESS。** 新 reviewer 只读当前交付，独立 target
实跑 lib 14、connect 30、session_abort 6 共 **50 passed / 0 failed**；主代理也独立复跑同组
通过。SeqCst 绑定握手按实际原子顺序验证；真实回环测试明确拒绝以 Timeout 代替主动 Abort；
二次 join 失败不再 open、revoke/Shutdown 零退出语义、CAS 上界不回绕与 required 环境文件
文字均通过。唯一 Minor 重复模块注释已由主代理删除（无生产逻辑改动）。

该复审仅收口板端本轮修复组件；上节真实双进程/真实 controller SQL/systemd/容量等未验
边界仍有效。主机真实 reboot 未授权执行，也未被测试触发。最终工作区门禁与集成验收另行执行。

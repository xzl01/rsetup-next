# Controller V1 剩余传输与板端 TDD 增量实施计划（2026-10-07）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实；使用 `- [ ]` 复选框跟踪步骤。这是**条件性增量实施计划**，仅规划非密码纯组件开发与剩余验收路径；未解决的安全阻断未解除前，严禁宣称加密握手完成、严禁上线生产、严禁运行未经授权的真库/硬件网络测试。

**Goal:** 基于已实现的 `crates/rsetup-protocol` 帧头与签名纯函数切片，按严格 TDD 流程交付非密码纯组件（Wire 信封校验、握手帧序列状态机、准入状态分类抽象、板端只读白名单、幂等任务持久日志、短期重启票据与固定无参 OS 重启边界、双流 Probe 调度器与中控准入变更异步通知 Sink）；明确将端到端加密握手、AES-256-GCM 记录层及真实网络双进程 Smoke 保持为 **BLOCKED** 交付，严格等待独立外部专家决议、规范修订与双端审查。

**Architecture:**
1. 纯协议库 `crates/rsetup-protocol` 负责 Wire 信封/载荷边界校验、帧序列状态机、双流路由契约与 Probe 调度器（纯内存/纯函数抽象，零网络/零 DB）；
2. 板端代理 `crates/rsetup-board-agent` 负责受限 Core 只读分发、原子持久日志引擎及受票据保护的无参 OS 重启边界；
3. 中控 `crates/rsetup-controller` 提供准入提交后异步通知 Adapter 并挂接独立设备 TCP 监听器骨架；
4. 密码学协商（X25519/HKDF/AES-256-GCM）与网络双进程联调受外部专家决议阻断，纯组件阶段使用测试隔离桩（Stub/Mock）实现契约自洽。

**Tech Stack:**
- 已在 workspace 声明的依赖：Rust 2024 / MSRV 1.85；`tokio = "1.47"`（`Cargo.lock` 当前解析为 1.53.1）；`ring = "0.17"`（锁定 0.17.14）；`thiserror = "2.0"`、`hex = "0.4"`、`uuid = "1.18"`（按根 manifest 与 lock 核对）。`tempfile` 是否已有及其精确版本须在新增 board-agent 依赖前核对，不当作已获准直接依赖。
- 待审定条件候选（当前未获批）：`prost 0.13`、`tonic`、`cargo-fuzz`。未经审批禁止引入任何额外密码学或网络依赖。
- **Manifest 与依赖审查纪律：** 新增 workspace 成员（`crates/rsetup-board-agent`）或新增/变更直接依赖时，必须同步核验并审查 `Cargo.lock`，严禁仅 `git add Cargo.toml` 而遗漏 `Cargo.lock`。

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` §3–6、`02-data-api.md` §2.2、`03-device-protocol.md` 全文、`04-task-lifecycle.md` §1–3、`05-runtime-operations.md` §6–7、`06-web-acceptance.md` §6–7；现行线协议规范 `docs/protocol_spec.md` (v2)；`docs/superpowers/plans/2026-10-02-controller-02-transport-board-tdd.md`；`docs/superpowers/plans/2026-10-05-controller-device-protocol-core.md`；`docs/superpowers/plans/2026-10-03-tunnel-security-protocol-revision-tdd.md`。

---

## Global Constraints

1. **安全生产硬阻断（绝不绕过安全门）：**
   - 现行线协议规范仍为 `docs/protocol_spec.md` (v2)。现行协议中存在服务端签名未覆盖 `reason_code`、验签失败描述冲突、AEAD tag 失效后的连接终止策略未定等安全缺陷。
   - 协议 v3 修订受 `2026-10-03-tunnel-security-protocol-revision-tdd.md` 中的**独立外部专家书面决议原件**硬关卡阻断。在专家书面决议到达 `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md` 且经用户再次复审之前，**绝不在本文或代码中提前决定密码布局（如是否签入 reason_code 或 FrameType）、不提前自定 v3 签名转录格式或额外私有协议、绝不创建虚构专家报告、绝不绕过安全门**。
   - 传输加密（X25519 临时密钥协商、HKDF 密钥派生、AES-256-GCM 记录层）在相应安全决议与修订完成前，保持为 **BLOCKED** 交付，不能声明安全完工或投入生产。
   - 已实现的 `ed25519.rs` 测试目前只钉死 ring 本地生成回归向量，尚无经独立来源核实的 RFC 8032/其他实现向量；须在 `tests/ed25519.rs` 从权威原文逐字核对并增补验签/拒签向量，在双端互操作验收前只能报告本地回归，不得把同一 ring 生成的期望值冒充独立证据；见[设备协议核心计划](2026-10-05-controller-device-protocol-core.md)的未决项。

2. **纯组件独立推进与加密/联网 Smoke 阻断分界：**
   - **非密码纯组件可独立推进：** Tasks 1–7 均为无加密、无外网依赖的纯数据结构、状态机、只读路由、本地文件日志与抽象契约，完全可在安全门解除前通过纯单元测试独立完成 TDD。
   - **加密握手与真实网络 Smoke 严格保持 BLOCKED：** Task 8 的端到端加密握手与 Task 9 的真实网络双进程只读业务 Smoke 必须等待外部专家决议到达、v3 规范修订完成、向量获批及 01/03 联合门具备条件后方可进入实施与验收；纯组件阶段仅在内存或测试替身（Mock/Stub）边界内验证消息编排与端口绑定。

3. **严格 TDD 纪律与可编译桩（有效 RED 准则）：**
   - **RED 准备（必须有可编译桩）：** 在编写失败测试前，必须先在源文件中定义好相关的结构体、枚举、方法或函数签名，并提供返回固定失败值或空放行的最小桩（Stub）。项目代码与测试代码必须能通过编译器完整类型检查并成功生成测试可执行文件。
   - **有效 RED 判定：** 必须使用精准定向命令运行指定测试，且退出码非零的原因必须是**目标业务断言（Assertion）不匹配失败**。缺少模块、类型未定义、语法错误等编译失败绝对不计为有效 RED！
   - **实施循环：** 遵循严格的“创建可编译桩与失败断言 → 定向运行确认断言失败（RED） → 编写最小实现（GREEN） → 定向运行确认通过（PASS） → 显式 pathspec 提交”五步循环。

4. **Wire 与信封契约（WIRE-01 / 现行规范 §6.4 / 03 规格 §1）：**
   - 业务消息协议 `schema_version` 必须等于 1，缺失或 0 不作默认兼容，未知版本返回 `UNSUPPORTED_VERSION (1001)`。
   - `TunnelPacket` 序列化完整信封字节数（包含 metadata、action、varint 长度与字段开销在内的完整 wire 编码或底层帧接收长度）必须 `≤512KiB`，而不是仅校验内部 `payload.len()`。审定 codec 前，纯结构校验只做确定越界的早期拒绝；**不能靠手写估算字节数决定是否接受**。未来编解码入口先做整包有界读取/分配，再用实际 encoded_len 或已接收的 raw wire 长度精确限制；`payload` 正好 512KiB 时有正信封开销，必然超限。无 codec 不能宣称 WIRE-01 线格式上限已通过。
   - `action` 字段长度 `≤64B`，且必须合法非空；除协议专用信令（`tunnel.kick` / `tunnel.revoke` 仅作 `TYPE_SYSTEM`）及双流探测（`tunnel.ping` 仅作 `TYPE_REQUEST` / `TYPE_RESPONSE`）外，其他业务 action 严禁使用 `tunnel.` 保留前缀；禁止空字符串通过。
   - `TunnelPacket.kind` 必须为已知合法类型（`Request` / `Response` / `Event` / `System`），禁止 `PacketType::Unknown` 错误放行；严禁在待审 codec 前自写私有 Proto 编解码器替代待审定依赖。
   - `metadata` `≤16` 项，且每个 key `≤64B`、value `≤256B`。
   - `tunnel.ping` 必须使用 `TYPE_REQUEST` / `TYPE_RESPONSE`，载荷为原始 16B CSPRNG nonce，**不得套用业务 Protobuf**。请求的 metadata 必须为空；响应的 `status_code` 必须为 0，且 `error_message` 与 metadata 必须为空。
   - 非法 ping 请求按流级协议错误终止该流；已知 probe 响应字段错误归不健康、不刷新期限（未知/过期/已完成响应静默丢弃），不得仅因全局 wire 预检拒包就漏计当前窗口/首 probe 失败。

5. **准入控制与连接代际（ADM-01 / 02 规格 §2.2 / 05 规格 §6–7）：**
   - 准入持久化使用完整快照 `AdmissionSnapshot { admission_state, review_decision, revision }`，按设备公钥（32 字节）索引并执行 CAS 严格校验。
   - 状态语义：新身份为 `PENDING + none`；人工批准为 `APPROVED + approved`；人工拒绝为 `PENDING + denied`；吊销为 `REVOKED + revoked`。
   - 握手判定：`PENDING + denied` 必须立即签名回绝 `APPROVAL_DENIED`，板端收到后停止自动重连；`REVOKED + revoked` 回绝 `REVOKED`，板端收到后停止自动重连。
   - **拟议资源配额，待 G0 审阅而非现行线协议常量：** 按 05 §6 分离未认证连接建议上限 128、已验签 PENDING 连接建议上限 2048、设备隧道 TCP 总连接建议上限 4096 与持久设备 identity 记录建议上限 **8192**；不得将 TCP 4096 误用作身份记录容量。`APPROVED` 已有持久记录仍须占用连接预算。超限使用可退避的资源拒绝，不得误记 REVOKED/APPROVAL_DENIED；审阅通过前不把这些拟议数值固化为最终产品承诺。
   - 健康 PENDING 连接**无人工审批总 TTL**，仅受名义 5s、pong 10s、客户端静默 15s 单调时钟约束（协议既定参数，非待审默认）。
   - **所有 DB 事务内均禁止任何网络调用**。中控 01 审批事务提交后，仅以 `{public_key, revision}` 异步通知 02 传输层；传输层重读权威 DB，校验连接代际并执行连接收敛，通知失败不回滚已提交的 DB 决定。

6. **板端安全边界与只读原则（03 规格 §1 / 04 规格 §1）：**
   - 板端远程白名单仅允许：`device.capabilities.get`、`device.status.get`、`device.clock.get`、`device.task.get`、`device.reboot.prepare`、`device.reboot.execute`，以及板端主动发出的 `device.task.result` 事件。
   - 绝不暴露现有单板 HTTP 危险动作、任意 shell/命令、配置写入、关机、升级或文件分发。
   - 白名单分发必须校验业务 payload schema（例如 `device.task.get` 必须解码并校验 `TaskQuery` 的 `schema_version = 1` 及非空合法 `sub_task_id`；非法 schema/未知版本返回 `UnsupportedVersion` 或 `InvalidArgument`，且 OS/持久写调用绝对为零）。
   - 必须通过 peer 认证身份上下文为任务去重隔离 namespace（绑定已认证 controller 公钥）；Task 4 之后必须接入真实 `TaskJournal`，严禁使用虚假硬编码消息冒充业务测试通过。
   - `RebootOs::reboot()` 必须为**无参数调用**，绝不接收 command/path/argv。
   - **自动化测试中宿主机真实 reboot 调用绝对为零**！必须使用测试隔离的 Mock/Fake OS 替身，绝不触发开发机重启。

7. **板端日志、票据与执行边界（WIRE-03..05 / 04 规格 §2–3）：**
   - 任务日志 `TaskJournal` 按 `(controller_key, sub_task_id)` 索引，维护递增的 `record_version` 与持久化初始化的 `journal_epoch`。日志建议容量 4096 条（03 §5 待审默认），满时拒绝新变更；首版不得自动淘汰去重记录。
   - `journal_epoch` 必须在日志初始化创建时持久化写入（持久 UUID），**绝不是 agent 进程级临时 epoch**；agent 进程正常重启绝不更换 `journal_epoch`，仅在日志丢失或不可逆损坏重建时才重新生成并更换 `journal_epoch`，且对外向中控报告不确定/`JOURNAL_LOST`。
   - `TaskRecord` 必须包含 `journal_epoch` 字段；`pre_boot_id` 必须在 `accepted` 阶段即已确定并落盘，绝不能推迟到 `attempted` 阶段才补入。
   - 可靠落盘协议：必须采用严格的“写入同目录临时文件 -> 文件 `sync_all` -> 原子 `rename` -> 父目录 `sync_all`”两阶段原子提交；普通 reopen 仅验证文件格式一致，不证明掉电持久性（电源故障崩溃恢复证据另设门禁验证）。在 `attempted` 达到可靠落盘前，OS reboot 调用绝对为零。日志写入必须具备明确的失败分类（区分确未提交与提交结果未知）。
   - 状态迁移：`accepted -> attempted -> finish (succeeded/failed/unknown)`。
   - 重启票据单调 TTL 建议 30s（03 §5 待审默认，非现行传输协议常量）；严格绑定连接代际、Boot ID 与 Agent 实例。
   - `accepted` 与 `attempted` 必须在进入 OS 调用边界前**可靠提交落盘**。若提交不明，断流待查，绝不冒充未接收或调用 OS。
   - OS 明确错误且 `finish(failed, "OS_ERROR")` 可靠落盘后，以 `status_code=0` 的成功信封携带持久 `TaskRecord` 回复；若落盘失败则断流待查，绝不返回非零 RPC 误导调用端。
   - 跨 Boot 恢复：只有存在可靠落盘的 `attempted` 记录、且当前 `boot_id` 与原记录 `pre_boot_id` 不同时，才原子写入 `succeeded` 并附带 `observed_boot_id` 与 `evidence_kind="boot_transition_observed"`；同 Boot、仅 accepted 或日志丢失时，保持 `unknown`，**绝不自动重发或自动重启**。

8. **双流 Probe 调度与分级故障（现行规范 §6.6 / 05 规格 §7）：**
   - 建立连接后必须**先开控制流（OpenControl）、再开数据流（OpenData）**。
   - 应用级 Probe：每流单在途，15s 窗口。明确区分：首 Probe 每次等待 15s，失败则计为 1 次重开失败；**连续 3 次单次重开失败**（经历 3 次独立的开流+首 probe 周期）才升级为整连接重建，**绝不是在一个 15s 窗口内重试 3 次**；运行期连续 3 窗口（45s）无有效 pong 仅重开坏流。
   - 连接级保活：HTTP/2 8B token PING/ACK，15s 窗口，连续 3 窗口无 ACK 判定连接死链；连接级 ACK 与应用级 Probe 独立计时，互不代答。
   - `TYPE_SYSTEM` 仅允许在 OpenControl 流发送（`tunnel.kick` / `tunnel.revoke`）。同公钥新连接抢占旧连接时，若旧 Control 流可用先发 kick，单调计时 500ms 后关闭旧 TCP。

---

## 文件清单与职责划分

| 文件路径 | 动作 | 唯一职责 | 可实施性状态 |
|---|---|---|---|
| `crates/rsetup-protocol/src/wire.rs` | 新增 | `TunnelPacket` 信封、03 业务载荷结构与 WIRE-01 边界校验纯函数 | 可独立实施 (Task 1) |
| `crates/rsetup-protocol/src/handshake.rs` | 新增 | 握手帧序列状态机（Client / Server）、严格单在途 Token/Nonce 校验 | 可独立实施 (Task 2) |
| `crates/rsetup-protocol/src/admission.rs` | 新增 | 传输准入决策状态机、健康 PENDING 保活管理与快照 CAS 抽象 | 可独立实施 (Task 2) |
| `crates/rsetup-protocol/src/tunnel.rs` | 新增 | gRPC 双流路由、信封分发、`TYPE_SYSTEM` 规则与流级代际隔离 | 可独立实施 (Task 6) |
| `crates/rsetup-protocol/src/probe.rs` | 新增 | 应用级 16B Probe 调度器、首 Probe 失败升级与 HTTP/2 8B PING/ACK 保活状态机 | 可独立实施 (Task 6) |
| `crates/rsetup-protocol/src/lib.rs` | 修改 | 导出新增的 `wire`, `handshake`, `admission`, `tunnel`, `probe` 模块 | 随任务增量导出 |
| `crates/rsetup-protocol/tests/wire_contract.rs` | 新增 | WIRE-01 业务版本、载荷超限、Ping 字段校验与空元数据行为测试 | 可独立实施 (Task 1) |
| `crates/rsetup-protocol/tests/frame_sequences.rs` | 新增 | 握手帧严格序列、乱序拒绝、Token 匹配与单在途状态机测试 | 可独立实施 (Task 2) |
| `crates/rsetup-protocol/tests/admission_state.rs` | 新增 | ADM-01 准入快照 CAS、PENDING+denied 拒绝、配额超限与代际隔离测试 | 可独立实施 (Task 2) |
| `crates/rsetup-protocol/tests/tunnel_probe.rs` | 新增 | 双流 Probe 调度、首 Probe 3 次升级、运行期单流重开与 HTTP/2 PING/ACK 死链测试 | 可独立实施 (Task 6) |
| `crates/rsetup-board-agent/Cargo.toml` | 新增 | 板端代理 Manifest，依赖 `rsetup-protocol`, `rsetup-core`, `tokio`, `thiserror`, `uuid` | 可独立实施 (Task 3) |
| `crates/rsetup-board-agent/src/lib.rs` | 新增 | 导出 board-agent 公共接口，窄化模块暴露 | 可独立实施 (Task 3) |
| `crates/rsetup-board-agent/src/read_only.rs` | 新增 | Core 硬件/监控快照只读包装器，禁止任何变更操作 | 可独立实施 (Task 3) |
| `crates/rsetup-board-agent/src/dispatch.rs` | 新增 | 业务 Action 白名单路由器，白名单外动作直接返回 1002 | 可独立实施 (Task 3) |
| `crates/rsetup-board-agent/src/journal.rs` | 新增 | 任务幂等持久日志引擎，原子事务写入、CAS 状态流转与跨 Boot 证据核验恢复 | 可独立实施 (Task 4) |
| `crates/rsetup-board-agent/src/ticket.rs` | 新增 | 短期重启票据生成与校验，绑定连接代际/Boot/Agent，30s 单调 TTL | 可独立实施 (Task 5) |
| `crates/rsetup-board-agent/src/reboot.rs` | 新增 | 固定无参 `RebootOs` 抽象与适配器，日志落盘后再进入 OS 重启边界 | 可独立实施 (Task 5) |
| `crates/rsetup-board-agent/src/tunnel_binding.rs` | 新增 | 绑定 BoardAgent Dispatch 与 Protocol 双流，提供客户端内存测试上下文 | 可独立实施 (Task 8) |
| `crates/rsetup-board-agent/tests/read_only.rs` | 新增 | 白名单只读验证、非白名单动作拒绝且变更调用为零测试 | 可独立实施 (Task 3) |
| `crates/rsetup-board-agent/tests/wire_04_journal.rs` | 新增 | 任务日志原子写入、提交前/后崩溃、跨 Boot 证据恢复与日志丢失测试 | 可独立实施 (Task 4) |
| `crates/rsetup-board-agent/tests/wire_03_tickets.rs` | 新增 | 票据过期、代际不匹配、OS 明确错误持久记录测试 | 可独立实施 (Task 5) |
| `crates/rsetup-board-agent/tests/reboot_boundary.rs` | 新增 | OS 重启边界无参数、宿主机 reboot 零调用、日志未落盘绝对不调用 OS 测试 | 可独立实施 (Task 5) |
| `crates/rsetup-board-agent/tests/interop_dual_stream.rs` | 新增 | 跨 Crate 双流开流时序、只读 RPC 与票据执行内存级集成测试 | 纯组件集成 (Task 8)；**完整加密握手 BLOCKED** |
| `crates/rsetup-controller/src/devices/admission_events.rs` | 新增 | 中控 `AdmissionChangeSink` 有界队列 Adapter，DB 事务提交后异步通知传输层重读权威 DB 并核对代际 | 可独立实施 (Task 7) |
| `crates/rsetup-controller/tests/admission_tunnel.rs` | 新增 | 真实 DeviceService 提交后有界通知、权威重读、乱序与代际保护、定向关闭补偿测试 | 可独立实施 (Task 7) |
| `crates/rsetup-board-agent/src/main.rs` | 新增 | 板端可执行入口骨架：外联目标、0600 私钥防覆盖落盘、先恢复 Journal | 骨架可实施 (Task 9) |
| `crates/rsetup-controller/src/devices/listener.rs` | 新增 | 中控独立设备安全 TCP 端口监听器骨架，与管理 HTTP 端口完全隔离 | 骨架可实施 (Task 9) |
| `crates/rsetup-controller/tests/process_loopback_smoke.rs` | 新增 | 独立 TCP 端口绑定单测（可执行）；真实网络双进程 Smoke 标为 `#[ignore]` | 骨架单测可做；**网络双进程业务 Smoke BLOCKED** |
| `crates/rsetup-protocol/src/crypto.rs` | 新增 | 【安全门关卡】临时 X25519 ECDH 与 HKDF 密钥派生 | **严格 BLOCKED (Task 10)** |
| `crates/rsetup-protocol/src/record.rs` | 新增 | 【安全门关卡】AES-256-GCM 记录层编解码与 Tag 失效终止策略 | **严格 BLOCKED (Task 10)** |
| `crates/rsetup-protocol/tests/crypto_vectors.rs` | 新增 | 【安全门关卡】已获独立专家批准的双端向量测试 | **严格 BLOCKED (Task 10)** |
| `crates/rsetup-protocol/tests/record_layer.rs` | 新增 | 【安全门关卡】已获独立专家批准的加密记录层测试 | **严格 BLOCKED (Task 10)** |

---

## 实施任务清单

### Task 1: 共享 Wire 协议与载荷契约（WIRE-01）

**Files:**
- Create: `crates/rsetup-protocol/src/wire.rs`
- Modify: `crates/rsetup-protocol/src/lib.rs`
- Test: `crates/rsetup-protocol/tests/wire_contract.rs`

**Interfaces:**
```rust
pub const MAX_ENVELOPE_BYTES: usize = 524_288; // 512 KiB 完整序列化信封上限
pub const MAX_ACTION_BYTES: usize = 64;
pub const MAX_METADATA_ENTRIES: usize = 16;
pub const MAX_METADATA_KEY_BYTES: usize = 64;
pub const MAX_METADATA_VAL_BYTES: usize = 256;
pub const PING_NONCE_LEN: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketType { Unknown = 0, Request = 1, Response = 2, Event = 3, System = 4 }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TunnelPacket {
    pub trace_id: u64,
    pub kind: PacketType,
    pub action: String,
    pub status_code: i32,
    pub error_message: String,
    pub payload: Vec<u8>,
    pub metadata: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WireError {
    #[error("unsupported business schema version: {0}")]
    UnsupportedVersion(u32),
    #[error("action cannot be empty")]
    ActionEmpty,
    #[error("action too long: {len} > {max}")]
    ActionTooLong { len: usize, max: usize },
    #[error("action contains reserved tunnel prefix or invalid characters")]
    InvalidAction,
    #[error("packet type unknown or invalid")]
    InvalidPacketType,
    #[error("metadata count too large: {len} > {max}")]
    MetadataTooMany { len: usize, max: usize },
    #[error("metadata key too long: {len} > {max}")]
    MetadataKeyTooLong { len: usize, max: usize },
    #[error("metadata value too long: {len} > {max}")]
    MetadataValTooLong { len: usize, max: usize },
    #[error("envelope wire size too large: {len} > {max}")]
    EnvelopeTooLarge { len: usize, max: usize },
    #[error("invalid ping nonce length: {0} (want 16)")]
    InvalidPingNonceLength(usize),
    #[error("ping request must have empty metadata")]
    PingRequestMetadataNotEmpty,
    #[error("ping response must have status_code=0: {0}")]
    InvalidPingStatusCode(i32),
    #[error("ping response must have empty error_message and metadata")]
    InvalidPingResponseFields,
}

pub fn validate_business_version(v: u32) -> Result<(), WireError>;
/// 结构合法性；最终 512KiB 整包校验必须在获审 codec 的实际 wire 边界进行
pub fn validate_packet(packet: &TunnelPacket) -> Result<(), WireError>;
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-protocol/src/lib.rs` 中追加 `pub mod wire;`。
在 `crates/rsetup-protocol/src/wire.rs` 中创建包含上述常量、枚举、结构体与桩函数的代码（桩函数暂放行所有输入，保证代码可编译；严禁在此阶段私自手写私有 Proto 编解码替代待审 codec）：
```rust
use std::collections::BTreeMap;
use thiserror::Error;

// 常量与类型定义如上 ...
pub fn validate_business_version(_v: u32) -> Result<(), WireError> {
    Ok(()) // 桩：错误放行无效版本
}

pub fn validate_packet(_packet: &TunnelPacket) -> Result<(), WireError> {
    Ok(()) // 桩：错误放行非法包
}
```

在 `crates/rsetup-protocol/tests/wire_contract.rs` 中编写测试：
```rust
use rsetup_protocol::wire::{validate_business_version, validate_packet, PacketType, TunnelPacket, WireError, MAX_ENVELOPE_BYTES};
use std::collections::BTreeMap;

#[test]
fn rejects_invalid_business_version() {
    assert_eq!(validate_business_version(0), Err(WireError::UnsupportedVersion(0)));
    assert_eq!(validate_business_version(2), Err(WireError::UnsupportedVersion(2)));
    assert!(validate_business_version(1).is_ok());
}

#[test]
fn rejects_unknown_packet_type_and_empty_action() {
    let empty_action = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: String::new(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&empty_action), Err(WireError::ActionEmpty));

    let unknown_type = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Unknown,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&unknown_type), Err(WireError::InvalidPacketType));
}

#[test]
fn rejects_unauthorized_tunnel_prefix_for_business_action() {
    let bad_prefix = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "tunnel.custom_action".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&bad_prefix), Err(WireError::InvalidAction));
}

#[test]
fn rejects_payload_exactly_512kib_when_envelope_overhead_exceeds_limit() {
    // 规范要求整包 ≤512KiB。若 payload 为正好 512KiB，加上 trace_id/action/metadata/varint 开销后必超 512KiB
    let full_payload = TunnelPacket {
        trace_id: 42,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; MAX_ENVELOPE_BYTES],
        metadata: BTreeMap::new(),
    };
    assert!(matches!(validate_packet(&full_payload), Err(WireError::EnvelopeTooLarge { .. })));
}

#[test]
fn rejects_invalid_ping_packets() {
    let bad_nonce = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "tunnel.ping".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![0u8; 15],
        metadata: BTreeMap::new(),
    };
    assert_eq!(validate_packet(&bad_nonce), Err(WireError::InvalidPingNonceLength(15)));
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-protocol --test wire_contract rejects_invalid_business_version -- --exact`
预期：编译顺利通过（类型完整），测试执行并报告断言失败（`assertion left == right failed; left: Ok(()), right: Err(UnsupportedVersion(0))`），退出码非零。

- [ ] **Step 3: 最小实现（GREEN，纯结构切片；真实线协议门另验）**

在 `crates/rsetup-protocol/src/wire.rs` 中补充字段合法性与**不会漏判的必要越界条件**。待审 codec 尚未确定字段号、可选字段编码、默认值省略规则或 metadata entry 的具体编码，因此不能用手工相加的“估计线长”宣称实现了精确 512KiB 限制：它可能把合法边界包误拒，或把真实超限包误放。纯结构切片只可检测确定越界（如 `payload.len() == MAX_ENVELOPE_BYTES` 且非空 action 保证完整 wire 有正开销）；最终必须在编解码入口**分配前按 raw 消息有界长度**、编码后按真实长度或获审 codec 的 `encoded_len()` 作精确复核，并用同一 codec 的512KiB-1/512KiB/512KiB+1 整包固定向量测试双端一致。缺 codec 时 WIRE-01 的整包上限证据仍为 BLOCKED，不得以纯 struct GREEN 代替。
```rust
pub fn validate_business_version(v: u32) -> Result<(), WireError> {
    if v == 1 { Ok(()) } else { Err(WireError::UnsupportedVersion(v)) }
}

pub fn validate_packet(packet: &TunnelPacket) -> Result<(), WireError> {
    if packet.kind == PacketType::Unknown {
        return Err(WireError::InvalidPacketType);
    }
    if packet.action.is_empty() {
        return Err(WireError::ActionEmpty);
    }
    if packet.action.len() > MAX_ACTION_BYTES {
        return Err(WireError::ActionTooLong { len: packet.action.len(), max: MAX_ACTION_BYTES });
    }
    // 校验 action 格式与保留前缀
    if packet.action.starts_with("tunnel.") {
        match packet.action.as_str() {
            "tunnel.kick" | "tunnel.revoke" => {
                if packet.kind != PacketType::System {
                    return Err(WireError::InvalidAction);
                }
            }
            "tunnel.ping" => {
                if packet.kind != PacketType::Request && packet.kind != PacketType::Response {
                    return Err(WireError::InvalidAction);
                }
            }
            _ => return Err(WireError::InvalidAction),
        }
    } else {
        // 普通业务 action 必须是由小写字母/数字/点分隔的合法动作，不得为 tunnel. 前缀
        if packet.action.chars().any(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '.') {
            return Err(WireError::InvalidAction);
        }
    }

    // 仅做不会误拒合法包的确定越界检测；非空 action 确保编码信封
    // 在 payload 之外至少占用 1 字节。接收端必须另在 codec 边界严格
    // 按真实整包 wire 长度 <= MAX_ENVELOPE_BYTES 审核。
    if packet.payload.len() >= MAX_ENVELOPE_BYTES {
        return Err(WireError::EnvelopeTooLarge {
            len: packet.payload.len(), max: MAX_ENVELOPE_BYTES,
        });
    }

    if packet.metadata.len() > MAX_METADATA_ENTRIES {
        return Err(WireError::MetadataTooMany { len: packet.metadata.len(), max: MAX_METADATA_ENTRIES });
    }
    for (k, v) in &packet.metadata {
        if k.len() > MAX_METADATA_KEY_BYTES {
            return Err(WireError::MetadataKeyTooLong { len: k.len(), max: MAX_METADATA_KEY_BYTES });
        }
        if v.len() > MAX_METADATA_VAL_BYTES {
            return Err(WireError::MetadataValTooLong { len: v.len(), max: MAX_METADATA_VAL_BYTES });
        }
    }
    if packet.action == "tunnel.ping" {
        if packet.payload.len() != PING_NONCE_LEN {
            return Err(WireError::InvalidPingNonceLength(packet.payload.len()));
        }
        match packet.kind {
            PacketType::Request => {
                if !packet.metadata.is_empty() { return Err(WireError::PingRequestMetadataNotEmpty); }
            }
            PacketType::Response => {
                if packet.status_code != 0 { return Err(WireError::InvalidPingStatusCode(packet.status_code)); }
                if !packet.error_message.is_empty() || !packet.metadata.is_empty() {
                    return Err(WireError::InvalidPingResponseFields);
                }
            }
            _ => return Err(WireError::InvalidAction),
        }
    }
    Ok(())
}
```

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-protocol --test wire_contract`
预期：PASS，所有测试通过。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-protocol/src/lib.rs crates/rsetup-protocol/src/wire.rs crates/rsetup-protocol/tests/wire_contract.rs
git commit -m "feat(protocol): implement wire contracts and ping envelope validation"
```

---

### Task 2: 握手帧序列状态机与准入控制抽象（ADM-01）

**Files:**
- Create: `crates/rsetup-protocol/src/handshake.rs`
- Create: `crates/rsetup-protocol/src/admission.rs`
- Modify: `crates/rsetup-protocol/src/lib.rs`
- Test: `crates/rsetup-protocol/tests/frame_sequences.rs`
- Test: `crates/rsetup-protocol/tests/admission_state.rs`

**Interfaces:**
```rust
pub const MAX_UNAUTH_CONNECTIONS: usize = 128;
pub const MAX_PENDING_CONNECTIONS: usize = 2048;
pub const MAX_GLOBAL_IDENTITIES: usize = 8192; // 05 §6 拟议上限；待 G0 审阅

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerHandshakePhase { Initial, AwaitingAuthRequest, InPending, Completed, Terminated }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasonCode {
    Unspecified = 0, SignatureInvalid = 1, Revoked = 2, ApprovalDenied = 3, HandshakeTimeout = 4, ServerError = 5
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceQuota {
    pub unauth_connections: usize,
    pub pending_connections: usize,
    pub global_identities: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentitySlot {
    Available,
    UnauthLimitReached,
    PendingPoolExhausted,
    IdentityStorageFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionState { Pending, Approved, Revoked }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision { None, Approved, Denied, Revoked }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    pub admission_state: AdmissionState,
    pub review_decision: ReviewDecision,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDecision { Pending, Approved, Reject(ReasonCode), RetryableServerError }

/// 准入与连接限额分类：无论是否已有 snapshot（即使为已批准设备 APPROVED），连接握手前必须检查未认证/活动连接限额，绝不因存在快照而绕过连接预算
pub fn classify_admission(slot: IdentitySlot, snapshot: Option<&AdmissionSnapshot>) -> AdmissionDecision;
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-protocol/src/lib.rs` 增加 `pub mod admission; pub mod handshake;`。
在 `crates/rsetup-protocol/src/admission.rs` 中创建桩（桩函数暂全部返回 `AdmissionDecision::Pending`）：
```rust
// 类型定义如上 ...
pub fn classify_admission(_slot: IdentitySlot, _snapshot: Option<&AdmissionSnapshot>) -> AdmissionDecision {
    AdmissionDecision::Pending // 桩：错误返回 Pending
}
```
在 `crates/rsetup-protocol/src/handshake.rs` 中创建 `ServerHandshakeSM` 桩（`on_frame` 暂无条件返回 `Ok(())`）。

在 `crates/rsetup-protocol/tests/frame_sequences.rs` 中编写测试：
```rust
use rsetup_protocol::frame::FrameType;
use rsetup_protocol::handshake::{HandshakeError, ServerHandshakeSM, ServerHandshakePhase};

#[test]
fn rejects_out_of_order_handshake_frames() {
    let mut sm = ServerHandshakeSM::new();
    assert_eq!(sm.phase(), ServerHandshakePhase::Initial);
    // 未收到 ClientHello 直接收到 ClientAuthRequest 必须被拒绝
    assert_eq!(sm.on_frame(FrameType::ClientAuthRequest), Err(HandshakeError::UnexpectedFrame(FrameType::ClientAuthRequest)));
    assert_eq!(sm.phase(), ServerHandshakePhase::Terminated);
}
```
在 `crates/rsetup-protocol/tests/admission_state.rs` 中编写测试：
```rust
use rsetup_protocol::admission::{classify_admission, AdmissionDecision, AdmissionSnapshot, AdmissionState, IdentitySlot, ReviewDecision};

#[test]
fn capacity_exhaustion_is_retryable_and_never_persists_as_denied() {
    // 资源耗尽必须分类为 RetryableServerError，绝不产生 ApprovalDenied 或 Revoked
    assert_eq!(classify_admission(IdentitySlot::IdentityStorageFull, None), AdmissionDecision::RetryableServerError);
    assert_eq!(classify_admission(IdentitySlot::UnauthLimitReached, None), AdmissionDecision::RetryableServerError);
}

#[test]
fn approved_identity_cannot_bypass_connection_quota() {
    // 已批准设备建立新连接时，若未认证连接超限，必须受限并返回 RetryableServerError，绝不能绕过连接预算
    let approved_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Approved,
        revision: 10,
    };
    assert_eq!(
        classify_admission(IdentitySlot::UnauthLimitReached, Some(&approved_snap)),
        AdmissionDecision::RetryableServerError
    );
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：
`cargo test -p rsetup-protocol --test frame_sequences rejects_out_of_order_handshake_frames -- --exact`
`cargo test -p rsetup-protocol --test admission_state capacity_exhaustion_is_retryable_and_never_persists_as_denied -- --exact`
预期：编译通过，两个测试分别由于返回 `Ok(())` 与 `Pending` 发生断言失败，退出码非零。

- [ ] **Step 3: 最小实现（GREEN）**

在 `crates/rsetup-protocol/src/admission.rs` 中实现正确状态分类逻辑：
```rust
pub fn classify_admission(slot: IdentitySlot, snapshot: Option<&AdmissionSnapshot>) -> AdmissionDecision {
    // 连接配额检查优先：无论是否存在快照，若连接层资源已耗尽，均拒绝并返回 RetryableServerError
    match slot {
        IdentitySlot::UnauthLimitReached | IdentitySlot::PendingPoolExhausted | IdentitySlot::IdentityStorageFull => {
            return AdmissionDecision::RetryableServerError;
        }
        IdentitySlot::Available => {}
    }

    if let Some(snap) = snapshot {
        match (snap.admission_state, snap.review_decision) {
            (AdmissionState::Revoked, _) | (_, ReviewDecision::Revoked) => AdmissionDecision::Reject(ReasonCode::Revoked),
            (AdmissionState::Pending, ReviewDecision::Denied) => AdmissionDecision::Reject(ReasonCode::ApprovalDenied),
            (AdmissionState::Approved, ReviewDecision::Approved) => AdmissionDecision::Approved,
            (AdmissionState::Pending, ReviewDecision::None) => AdmissionDecision::Pending,
            _ => AdmissionDecision::Reject(ReasonCode::Unspecified),
        }
    } else {
        AdmissionDecision::Pending
    }
}
```
在 `crates/rsetup-protocol/src/handshake.rs` 中实现 `ServerHandshakeSM` 帧序列迁移与严格递增探针序号检查。

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-protocol --test frame_sequences --test admission_state`
预期：PASS，所有用例通过。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-protocol/src/lib.rs crates/rsetup-protocol/src/{handshake,admission}.rs crates/rsetup-protocol/tests/{frame_sequences,admission_state}.rs
git commit -m "feat(protocol): implement handshake state machine and admission classifier"
```

---

### Task 3: 板端代理只读白名单分发（WIRE-01/02）

**Files:**
- Create: `crates/rsetup-board-agent/Cargo.toml`
- Create: `crates/rsetup-board-agent/src/lib.rs`
- Create: `crates/rsetup-board-agent/src/read_only.rs`
- Create: `crates/rsetup-board-agent/src/dispatch.rs`
- Modify: `Cargo.toml` (根 members 加入 `crates/rsetup-board-agent`)
- Modify: `Cargo.lock` (必须同步核查并包含依赖解析，严禁仅 add Cargo.toml)
- Test: `crates/rsetup-board-agent/tests/read_only.rs`

**Interfaces:**
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusinessStatus {
    Success = 0,
    UnsupportedVersion = 1001,
    UnsupportedAction = 1002,
    InvalidArgument = 1003,
    CapabilityUnavailable = 1004,
    DeviceBusy = 1005,
    TaskNotFound = 1006,
    TaskConflict = 1007,
    ResourceExhausted = 1008,
    InternalError = 1009,
    ExecutionTokenInvalid = 1010,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskQuery {
    pub schema_version: u32,
    pub sub_task_id: String,
}

pub trait ReadOnlySource: Send + Sync {
    fn capabilities(&self) -> Vec<u8>;
    fn status(&self) -> Vec<u8>;
    fn clock(&self) -> Vec<u8>;
    fn mutation_calls(&self) -> usize;
}

pub trait TaskJournalReader: Send + Sync {
    fn query(&self, controller_key: &[u8; 32], query: &TaskQuery) -> Result<Vec<u8>, BusinessStatus>;
}

pub struct PeerContext {
    pub authenticated_controller_key: [u8; 32],
}

pub struct BoardAgent<R: ReadOnlySource, J: TaskJournalReader> {
    read_only: R,
    journal: J,
}

impl<R: ReadOnlySource, J: TaskJournalReader> BoardAgent<R, J> {
    pub fn new(read_only: R, journal: J) -> Self;
    pub async fn handle(&self, peer: &PeerContext, action: &str, payload: &[u8]) -> Result<Vec<u8>, BusinessStatus>;
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在根 `Cargo.toml` 的 `members` 中追加 `"crates/rsetup-board-agent"`，同步检查 `Cargo.lock`。
创建 `crates/rsetup-board-agent/Cargo.toml`，仅引用已锁定的 workspace 依赖与 `tempfile 3`（开发依赖）。
创建 `crates/rsetup-board-agent/src/{lib.rs, read_only.rs, dispatch.rs}`。在 `dispatch.rs` 中提供桩实现：
```rust
// 桩：错误放行一切非白名单动作
pub async fn handle(&self, _peer: &PeerContext, _action: &str, _payload: &[u8]) -> Result<Vec<u8>, BusinessStatus> {
    Ok(vec![])
}
```

在 `crates/rsetup-board-agent/tests/read_only.rs` 中编写测试（覆盖未知 action、非法 schema、TaskQuery 解码与 task.get 命名空间隔离）：
```rust
use rsetup_board_agent::{BoardAgent, BusinessStatus, PeerContext, ReadOnlySource, TaskJournalReader, TaskQuery};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct FakeSource { mutations: AtomicUsize }
impl ReadOnlySource for FakeSource {
    fn capabilities(&self) -> Vec<u8> { b"caps_payload".to_vec() }
    fn status(&self) -> Vec<u8> { b"status_payload".to_vec() }
    fn clock(&self) -> Vec<u8> { b"clock_payload".to_vec() }
    fn mutation_calls(&self) -> usize { self.mutations.load(Ordering::SeqCst) }
}

#[derive(Default)]
struct FakeJournalReader { queries: AtomicUsize }
impl TaskJournalReader for FakeJournalReader {
    fn query(&self, controller_key: &[u8; 32], query: &TaskQuery) -> Result<Vec<u8>, BusinessStatus> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        if query.schema_version != 1 {
            return Err(BusinessStatus::UnsupportedVersion);
        }
        if query.sub_task_id.is_empty() {
            return Err(BusinessStatus::InvalidArgument);
        }
        // 验证 controller_key 命名空间隔离
        if controller_key == &[1u8; 32] && query.sub_task_id == "task-1" {
            Ok(b"task_record_bytes".to_vec())
        } else {
            Err(BusinessStatus::TaskNotFound)
        }
    }
}

#[tokio::test]
async fn remote_mutations_are_strictly_rejected() {
    let source = FakeSource::default();
    let journal = FakeJournalReader::default();
    let agent = BoardAgent::new(source, journal);
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };
    let res = agent.handle(&peer, "device.fan.apply", &[]).await;
    assert_eq!(res, Err(BusinessStatus::UnsupportedAction));
    assert_eq!(agent.source().mutation_calls(), 0);
}

#[tokio::test]
async fn task_get_validates_schema_and_version() {
    let source = FakeSource::default();
    let journal = FakeJournalReader::default();
    let agent = BoardAgent::new(source, journal);
    let peer = PeerContext { authenticated_controller_key: [1u8; 32] };

    // 非法 payload 无法解码为 TaskQuery
    let invalid_payload = b"not_valid_query";
    assert_eq!(agent.handle(&peer, "device.task.get", invalid_payload).await, Err(BusinessStatus::InvalidArgument));
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-board-agent --test read_only remote_mutations_are_strictly_rejected -- --exact`
预期：编译通过，断言失败（`left: Ok([]), right: Err(UnsupportedAction)`），退出码非零。

- [ ] **Step 3: 最小实现（GREEN）**

在 `crates/rsetup-board-agent/src/dispatch.rs` 中实现严格的白名单路由与 schema 解码（codec 获批前使用严格结构反序列化桩，不把假的 b"caps" 假消息当作真实业务通过，非法输入时 OS/写调用为零）：
```rust
impl<R: ReadOnlySource, J: TaskJournalReader> BoardAgent<R, J> {
    pub fn new(read_only: R, journal: J) -> Self { Self { read_only, journal } }
    pub fn source(&self) -> &R { &self.read_only }

    pub async fn handle(&self, peer: &PeerContext, action: &str, payload: &[u8]) -> Result<Vec<u8>, BusinessStatus> {
        match action {
            "device.capabilities.get" => Ok(self.read_only.capabilities()),
            "device.status.get" => Ok(self.read_only.status()),
            "device.clock.get" => Ok(self.read_only.clock()),
            "device.task.get" => {
                let query = decode_task_query(payload)?;
                if query.schema_version != 1 {
                    return Err(BusinessStatus::UnsupportedVersion);
                }
                if query.sub_task_id.trim().is_empty() {
                    return Err(BusinessStatus::InvalidArgument);
                }
                self.journal.query(&peer.authenticated_controller_key, &query)
            }
            _ => Err(BusinessStatus::UnsupportedAction),
        }
    }
}
```

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-board-agent --test read_only`
预期：PASS，所有测试通过。

- [ ] **Step 5: 提交**

```bash
git add Cargo.toml Cargo.lock crates/rsetup-board-agent
git commit -m "feat(board-agent): add read-only dispatch with strict action allowlist and schema validation"
```

---

### Task 4: 板端幂等任务持久日志引擎（WIRE-04/05）

**Files:**
- Create: `crates/rsetup-board-agent/src/journal.rs`
- Modify: `crates/rsetup-board-agent/src/lib.rs`
- Test: `crates/rsetup-board-agent/tests/wire_04_journal.rs`

**Interfaces:**
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState { Accepted, Attempted, Succeeded, Failed, Unknown }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    pub schema_version: u32,
    pub sub_task_id: String,
    pub journal_epoch: String, // 必须是持久 UUID，非进程临时 epoch
    pub record_version: u64,
    pub state: TaskState,
    pub pre_boot_id: String,   // 必须在 accepted 时即确定且落盘
    pub observed_boot_id: Option<String>,
    pub reason_code: Option<String>,
    pub evidence_kind: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JournalWriteError {
    #[error("write definitely failed before commit")]
    DefinitelyNotCommitted,
    #[error("write state unknown, may or may not be committed")]
    CommitUnknown,
    #[error("journal storage full")]
    StorageFull,
    #[error("io error: {0}")]
    Io(String),
}

pub struct TaskJournal { /* 私有字段：持久化的 journal_epoch, path 等 */ }

impl TaskJournal {
    /// 初始化或加载持久日志：若全新初始化则生成并持久化 UUID journal_epoch；进程重启复用已持久化的 epoch；仅损坏重建才换新 epoch
    pub fn open(dir: &std::path::Path) -> Result<Self, JournalWriteError>;
    pub fn recover(dir: &std::path::Path, current_boot_id: &str) -> Result<Self, JournalWriteError>;
    pub fn journal_epoch(&self) -> &str;
    pub fn get(&self, controller_key: &[u8; 32], sub_task_id: &str) -> Result<Option<TaskRecord>, JournalWriteError>;
    /// accepted 时 pre_boot_id 必须已知并持久化落盘
    pub fn insert_accepted(&self, controller_key: &[u8; 32], sub_task_id: &str, payload_hash: &[u8; 32], pre_boot_id: &str) -> Result<TaskRecord, JournalWriteError>;
    pub fn mark_attempted(&self, controller_key: &[u8; 32], sub_task_id: &str) -> Result<TaskRecord, JournalWriteError>;
    pub fn finish(&self, controller_key: &[u8; 32], sub_task_id: &str, state: TaskState, reason: Option<&str>) -> Result<TaskRecord, JournalWriteError>;
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-board-agent/src/lib.rs` 中增加 `pub mod journal;`。
在 `crates/rsetup-board-agent/src/journal.rs` 中定义结构体与可编译桩（`recover` 暂返回空，`get` 暂返回 `None`）。
在 `crates/rsetup-board-agent/tests/wire_04_journal.rs` 中编写跨 Boot 恢复与持久 Epoch 测试：
```rust
use rsetup_board_agent::journal::{TaskJournal, TaskState};

#[test]
fn journal_epoch_persists_across_restart_and_pre_boot_known_at_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let key = [9u8; 32];
    let j1 = TaskJournal::open(dir.path()).unwrap();
    let epoch1 = j1.journal_epoch().to_string();
    let acc = j1.insert_accepted(&key, "task-1", &[0u8; 32], "boot-initial").unwrap();
    assert_eq!(acc.state, TaskState::Accepted);
    assert_eq!(acc.pre_boot_id, "boot-initial");
    assert_eq!(acc.journal_epoch, epoch1);
    drop(j1);

    // 进程正常重启，journal_epoch 必须保持不变，绝不刷新
    let j2 = TaskJournal::open(dir.path()).unwrap();
    assert_eq!(j2.journal_epoch(), epoch1);
}

#[test]
fn attempted_reboot_persists_succeeded_upon_boot_transition() {
    let dir = tempfile::tempdir().unwrap();
    let key = [9u8; 32];
    let j = TaskJournal::open(dir.path()).unwrap();
    j.insert_accepted(&key, "task-1", &[0u8; 32], "boot-old").unwrap();
    let att = j.mark_attempted(&key, "task-1").unwrap();
    assert_eq!(att.state, TaskState::Attempted);
    drop(j);

    // 跨 Boot 恢复：当前 Boot ID 为 boot-new
    let recovered = TaskJournal::recover(dir.path(), "boot-new").unwrap();
    let record = recovered.get(&key, "task-1").unwrap().expect("record must exist");
    assert_eq!(record.state, TaskState::Succeeded);
    assert_eq!(record.observed_boot_id.as_deref(), Some("boot-new"));
    assert_eq!(record.evidence_kind.as_deref(), Some("boot_transition_observed"));
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-board-agent --test wire_04_journal attempted_reboot_persists_succeeded_upon_boot_transition -- --exact`
预期：编译顺利通过（`journal` 模块与所有方法完整存在），测试执行并因 `record must exist` 断言失败，退出码非零。严禁缺少模块的编译失败！

- [ ] **Step 3: 最小实现（GREEN）**

在 `crates/rsetup-board-agent/src/journal.rs` 中实现可靠两阶段落盘与状态流转：
- 严格文件操作顺序：同目录临时文件（`tempfile_in(dir)`）写入数据 -> 文件 `sync_all` -> 原子 `rename` 覆盖目标文件 -> 父目录 `sync_all`（确保目录项更新落盘）；
- 区分写入失败模式：在调用 rename 前发生 IO 错误归为 `DefinitelyNotCommitted`；在 rename 触发但结果不明时归为 `CommitUnknown`；
- 普通 reopen 仅校验格式一致；电源故障恢复需独立门禁；在可靠持久化 `attempted` 之前 OS 零调用。
- `open` 检查现有元数据；首次创建写入随机 UUID 作为持久 `journal_epoch` 并保存于持久元数据文件；后续重启读取已有 `journal_epoch`。

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-board-agent --test wire_04_journal`
预期：PASS，所有日志持久化测试通过。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-board-agent/src/lib.rs crates/rsetup-board-agent/src/journal.rs crates/rsetup-board-agent/tests/wire_04_journal.rs
git commit -m "feat(board-agent): implement persistent task journal with strict fsync and stable epoch"
```

---

### Task 5: 短期重启票据与固定无参 OS 重启边界（WIRE-03）

**Files:**
- Create: `crates/rsetup-board-agent/src/ticket.rs`
- Create: `crates/rsetup-board-agent/src/reboot.rs`
- Modify: `crates/rsetup-board-agent/src/lib.rs`
- Test: `crates/rsetup-board-agent/tests/wire_03_tickets.rs`
- Test: `crates/rsetup-board-agent/tests/reboot_boundary.rs`

**Interfaces:**
```rust
pub const TICKET_TTL_SECS: u64 = 30; // 03 §5 建议默认，G0 审阅后才可定稿

pub trait RebootOs: Send + Sync {
    fn reboot(&self) -> Result<(), std::io::Error>; // 必须固定无参，绝不接受 argv/cmd
}

pub struct TicketManager { /* 30s 单调 TTL，绑定 controller key/conn/agent epoch */ }

pub struct RebootReply {
    pub status_code: i32,
    pub record: TaskRecord,
}

pub struct RebootExecutor<O: RebootOs> {
    journal: TaskJournal,
    os: O,
    tickets: TicketManager,
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-board-agent/src/lib.rs` 增加 `pub mod ticket; pub mod reboot;`。
在 `ticket.rs` 中提供桩（`consume` 暂无条件返回 `Ok(())`，不校验 TTL 与代际）。
在 `reboot.rs` 中提供 `RebootExecutor` 桩。

在 `crates/rsetup-board-agent/tests/wire_03_tickets.rs` 中编写测试：
```rust
use rsetup_board_agent::dispatch::BusinessStatus;
use rsetup_board_agent::ticket::TicketManager;
use std::time::{Duration, Instant};

#[test]
fn expired_ticket_never_reboots() {
    let mut tm = TicketManager::new();
    let key = [1u8; 32];
    let token = tm.prepare(&key, "task-1", "boot-1", "conn-1", "agent-1", Instant::now());
    // 超过 30s TTL
    let check = tm.consume(&key, "task-1", token, "conn-1", "agent-1", Instant::now() + Duration::from_secs(31));
    assert_eq!(check, Err(BusinessStatus::ExecutionTokenInvalid));
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-board-agent --test wire_03_tickets expired_ticket_never_reboots -- --exact`
预期：编译顺利通过，断言失败（`left: Ok(()), right: Err(ExecutionTokenInvalid)`），退出码非零。

- [ ] **Step 3: 最小实现（GREEN）**

在 `ticket.rs` 中实现严格 30s 单调 TTL、代际匹配与一次性消费校验。
在 `reboot.rs` 中实现 `RebootExecutor`：要求 accepted/attempted 可靠落盘（同目录临时文件 -> sync_all -> rename -> 父目录 sync_all）后再调用无参 `os.reboot()`；若 OS 明确返回错误，且 `finish(failed, "OS_ERROR")` 落盘成功，以 `status_code = 0` 的成功信封返回持久 Failed 记录；若写入结果不明断流待查，绝不冒充未接收或调用 OS。

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-board-agent --test wire_03_tickets --test reboot_boundary`
预期：PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-board-agent/src/lib.rs crates/rsetup-board-agent/src/{ticket,reboot}.rs crates/rsetup-board-agent/tests/{wire_03_tickets,reboot_boundary}.rs
git commit -m "feat(board-agent): gate reboot execution with tickets and OS error boundary"
```

---

### Task 6: gRPC 双流路由、分层 Probe 与心跳容错

**Files:**
- Create: `crates/rsetup-protocol/src/tunnel.rs`
- Create: `crates/rsetup-protocol/src/probe.rs`
- Modify: `crates/rsetup-protocol/src/lib.rs`
- Test: `crates/rsetup-protocol/tests/tunnel_probe.rs`

**Interfaces:**
```rust
pub const PROBE_WINDOW_SECS: u64 = 15;
pub const KEEP_ALIVE_WINDOW_SECS: u64 = 15;
pub const MAX_STREAM_FAILURES_BEFORE_ESCALATION: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamType { Control, Data }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeAction {
    Continue,
    ReopenStream(StreamType),
    RebuildConnection,
}

pub struct StreamProbeScheduler {
    // 15s 窗口：明确区分开流/首 probe 周期。单次首 probe 15s 超时/无效算 1 次重开失败；
    // 连续 3 次独立的单次重开失败才升级为整连接重建（不是 15s 内 3 次）；运行期连续 3 窗口失败仅重开坏流
}

pub struct Http2PingScheduler {
    // 15s 窗口，唯一 8B token，连续 3 窗口无 ACK 判死链
}

pub fn check_stream_routing(stream: StreamType, packet: &crate::wire::TunnelPacket) -> bool;
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-protocol/src/lib.rs` 增加 `pub mod probe; pub mod tunnel;`。
在 `probe.rs` 与 `tunnel.rs` 提供可编译桩（`check_window` 暂返回 `None`；`check_deadline` 暂返回 `false`）。

在 `crates/rsetup-protocol/tests/tunnel_probe.rs` 编写测试：
```rust
use rsetup_protocol::probe::{ProbeAction, StreamProbeScheduler, StreamType};
use std::time::{Duration, Instant};

#[test]
fn three_consecutive_stream_reopen_failures_escalate_to_connection_rebuild() {
    let mut sched = StreamProbeScheduler::new();
    let mut now = Instant::now();

    // 经历 3 次独立的开流与首 probe 失败循环（每次等待 15s），第 3 次必须升级为连接级重建
    for attempt in 1..=3 {
        sched.on_stream_opened(StreamType::Data, now);
        // 首 probe 等待 15s 发生超时/失败
        now += Duration::from_secs(15);
        let action = sched.on_first_probe_timeout(StreamType::Data, now);
        if attempt == 3 {
            assert_eq!(action, ProbeAction::RebuildConnection);
        } else {
            assert_eq!(action, ProbeAction::ReopenStream(StreamType::Data));
        }
    }
}

#[test]
fn three_consecutive_runtime_failures_only_reopens_bad_stream() {
    let mut sched = StreamProbeScheduler::new();
    let now = Instant::now();
    sched.on_stream_opened(StreamType::Control, now);
    sched.on_stream_opened(StreamType::Data, now);
    sched.on_valid_first_pong(StreamType::Control);
    sched.on_valid_first_pong(StreamType::Data);

    // Data 流连续 3 次窗口失败
    for i in 1..=3 {
        assert_eq!(sched.check_window(StreamType::Data, now + Duration::from_secs(15 * i)), if i == 3 { Some(StreamType::Data) } else { None });
    }
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-protocol --test tunnel_probe three_consecutive_runtime_failures_only_reopens_bad_stream -- --exact`
预期：编译通过，断言失败（第 3 次调用返回 `None` 而非 `Some(Data)`），退出码非零。

- [ ] **Step 3: 最小实现（GREEN）**

在 `probe.rs` 实现首 Probe 失败升级与运行期窗口重开逻辑：
- 每次首 Probe 等待满 15s 未收到有效 pong 算作一次重开失败（`ReopenStream`）；
- 必须累计 3 次独立的重开失败才升级为整连接重建（`RebuildConnection`）；
- 实现 HTTP/2 8B PING/ACK 独立死链调度器；在 `tunnel.rs` 约束 `TYPE_SYSTEM` 仅限 Control 流。

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-protocol --test tunnel_probe`
预期：PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-protocol/src/lib.rs crates/rsetup-protocol/src/{probe,tunnel}.rs crates/rsetup-protocol/tests/tunnel_probe.rs
git commit -m "feat(protocol): implement dual-stream probe scheduler and HTTP/2 keepalive"
```

---

### Task 7: 中控准入变更提交后异步通知与连接代际收敛

**Files:**
- Create: `crates/rsetup-controller/src/devices/admission_events.rs`
- Modify: `crates/rsetup-controller/src/devices/mod.rs`
- Test: `crates/rsetup-controller/tests/admission_tunnel.rs`

**Interfaces:**
```rust
use crate::devices::service::{AdmissionChangeEvent, AdmissionChangeSink};
use crate::AdmissionStore;
use std::sync::Arc;

pub trait TunnelSessionManager: Send + Sync {
    fn kick_session(&self, public_key: &[u8; 32], expected_revision: u64, conn_epoch: u64);
    fn revoke_session(&self, public_key: &[u8; 32], expected_revision: u64, conn_epoch: u64);
    /// 非阻塞地标记该设备的连接暂停并安排定向关闭；不得在同步回调中等待网络。
    fn close_session_for_recheck(&self, public_key: &[u8; 32]);
}

/// 准入变更通知适配器：保持现行 DeviceService 同步 after_commit 边界，绝不杜撰 async trait！
/// 内部通过有界 channel 将事件通知移交异步后台 Worker 处理，防止阻塞 DB 事务完成；
/// Worker 仅将通知作为“唤醒信号”，绝不直接相信 event.snapshot！必须重读权威 DB，核验 revision 与 connection epoch
pub struct AdmissionTunnelNotifier<S: AdmissionStore + Send + Sync, T: TunnelSessionManager> {
    store: Arc<S>,
    tunnel: Arc<T>,
    sender: tokio::sync::mpsc::Sender<AdmissionNoticeCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionNoticeCommand {
    pub public_key: [u8; 32],
    pub revision: u64,
}

impl<S: AdmissionStore + Send + Sync, T: TunnelSessionManager> AdmissionChangeSink for AdmissionTunnelNotifier<S, T> {
    fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ()> {
        // 同步边界：通过 try_send 推入有界队列；队列失败时立即在内存中禁止放行目标连接，
        // 然后向调用方报告 Err(())；不能只返回错误而假设不存在的 Worker 会完成补偿。
        self.sender.try_send(AdmissionNoticeCommand {
            public_key: event.public_key,
            revision: event.snapshot.revision,
        }).map_err(|_| {
            // 必须是内存中的非阻塞禁止放行/定向关闭标记，不可在此同步回调等待网络；
            // 即便队列已满且没有 Worker 消费，已提交的吊销也不能继续放行。
            self.tunnel.close_session_for_recheck(&event.public_key);
        })
    }
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-controller/src/devices/mod.rs` 导出 `pub mod admission_events;`。
在 `admission_events.rs` 提供桩（`after_commit` 暂返回 `Ok(())` 但内部 Worker 为空操作）。

在 `crates/rsetup-controller/tests/admission_tunnel.rs` **先建立可编译的受控 AdmissionStore 与 SessionManager fixture**，从真实 `DeviceService::revoke/reauthorize` 提交调用 `after_commit`，不能用手工捏造 `AdmissionChangeEvent` 取代已提交状态。写下列独立的行为失败断言，逐一观察 RED；fixture/连接管理器接口尚缺时记 BLOCKED，编译失败或只含注释的测试不算 RED：

1. 队列容量耗尽：已提交 `revoke` 后，`try_send` 失败即触发**内存非阻塞**定向关闭/禁止放行并返回错误，原 DB 决定不回滚；测试在 Worker 不运行时仍需断言目标连接不可继续业务。 `after_commit` 不得等待网络 I/O。
2. 通知乱序：旧 `revision` 在重新授权且持久 `revision` 更高后到达，Worker **重新读取权威存储**，不得使用事件快照覆盖新决定；新连接代际不被旧通知 kick/revoke。
3. 权威读取失败：停止放行对应身份并进行定向关闭，不能因 Worker 查询失败保持 APPROVED 业务流畅通；恢复后重新读取，不能擅自重放任务。
4. 最新持久决定为 `REVOKED` 且连接代际仍匹配时确实关闭该会话；异步副作用要观察真实连接管理器状态变化，不只检查 mock 被调用次数。

这些测试在未接入真实双方连接之前仅属于内存组件层证据，不能宣称双端网络互操作或可靠通知已在 DB 故障中验证。

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-controller --test admission_tunnel out_of_order_or_reauthorized_new_session_is_not_killed_by_old_notice -- --exact`
预期：仅在 fixture 与目标用例可编译且实际运行 1 个测试时，因旧乱序通知错误踢掉新连接或队列满仍继续放行而**断言失败**；缺 fixture/网络/库不能算 RED。

- [ ] **Step 3: 最小实现（GREEN）**

在 `crates/rsetup-controller/src/devices/admission_events.rs` 中实现正确逻辑：
1. `after_commit` 严格遵循现有同步 `Result<(), ()>` 接口，采用有界 mpsc channel 转发唤醒信号；队列满时**同步完成内存非阻塞定向禁止放行标记**再返回 `Err(())`，不得只返回错误却假设不可运行的 Worker 会完成补偿，也不得在此等待网络；
2. 异步 Worker 收到通知后，通过 `store.load(public_key)` **重新读取权威 DB 快照**，绝不直接将 event 中的快照当权威；
3. 校验权威 DB 的 `revision`：若通知中的 `revision < authoritative.revision`（旧通知/乱序），丢弃并忽略；
4. 核验活动连接的 connection epoch：若是新代际连接，旧通知不得误踢；
5. 若权威 DB 判定为 Revoked，则调用 `revoke_session`；若为新准入踢旧连接，核验后调用 `kick_session`；若 DB 读取失败，调用 `close_session_for_recheck` 确保 fail-closed。

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-controller --test admission_tunnel`
预期：PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-controller/src/devices/ crates/rsetup-controller/tests/admission_tunnel.rs
git commit -m "feat(controller): add admission event sink adapter to sync transport sessions"
```

---

### Task 8: 跨 Crate 双端双流与业务互操作集成（纯组件内存级；完整加密握手 BLOCKED）

**状态说明：**
- **非密码纯组件内存集成（可独立推进）：** 在纯内存通道下测试流时序（先 Control 后 Data）、只读 RPC 分发与票据重启隔离边界。
- **集成 Fixture 与断言要求：** 严禁使用两个布尔变量的 `MockTunnelPeer` 伪造跨 crate 互操作！必须编写组合真实待建组件的集成 fixture（整合真实 `BoardAgent`、真实 `TaskJournal`、真实内存 gRPC/mpsc 双流传输适配器、无参 OS 计数替身），覆盖真实消息序列、负例拒绝与崩溃恢复断言。若组件尚缺失，必须显式标注为 **BLOCKED**，绝不能通过虚假布尔桩虚构互操作通过。
- **完整端到端加密握手（严格 BLOCKED）：** 在外部专家决议到达并完成双方安全审查前，严禁宣称握手互通或加密信道建立。

**Files:**
- Create: `crates/rsetup-board-agent/src/tunnel_binding.rs`
- Modify: `crates/rsetup-board-agent/src/lib.rs`
- Test: `crates/rsetup-board-agent/tests/interop_dual_stream.rs`

**Interfaces:**
```rust
use crate::dispatch::BoardAgent;
use crate::journal::TaskJournal;
use crate::read_only::ReadOnlySource;
use rsetup_protocol::wire::TunnelPacket;
use tokio::sync::mpsc;

pub struct MemoryDualStreamFixture<R: ReadOnlySource> {
    pub agent: BoardAgent<R, TaskJournal>,
    pub control_tx: mpsc::Sender<TunnelPacket>,
    pub data_tx: mpsc::Sender<TunnelPacket>,
}

impl<R: ReadOnlySource> MemoryDualStreamFixture<R> {
    pub fn new(source: R, journal: TaskJournal) -> Self;
    pub async fn send_control(&self, packet: TunnelPacket) -> Result<TunnelPacket, &'static str>;
    pub async fn send_data(&self, packet: TunnelPacket) -> Result<TunnelPacket, &'static str>;
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-board-agent/src/lib.rs` 增加 `pub mod tunnel_binding;`。
在 `tunnel_binding.rs` 提供桩（`send_data` 暂不检查 Control 流是否已建立）。

在 `crates/rsetup-board-agent/tests/interop_dual_stream.rs` 编写跨 crate 真实消息交互与负例测试：
```rust
use rsetup_board_agent::tunnel_binding::MemoryDualStreamFixture;
use rsetup_board_agent::journal::TaskJournal;
use rsetup_protocol::wire::{PacketType, TunnelPacket};

#[tokio::test]
async fn data_stream_strictly_blocked_until_control_stream_established() {
    let dir = tempfile::tempdir().unwrap();
    let journal = TaskJournal::open(dir.path()).unwrap();
    // 真实待建组件集成：未先在 Control 流建立会话前，向 Data 流发送消息必须被严格拒绝
    let packet = TunnelPacket {
        trace_id: 1,
        kind: PacketType::Request,
        action: "device.status.get".to_string(),
        status_code: 0,
        error_message: String::new(),
        payload: vec![],
        metadata: std::collections::BTreeMap::new(),
    };
    // 桩未实现时应返回错误并断言失败
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-board-agent --test interop_dual_stream data_stream_strictly_blocked_until_control_stream_established -- --exact`
预期：编译通过，断言失败，退出码非零。

- [ ] **Step 3: 最小实现（GREEN）**

在 `tunnel_binding.rs` 中使用真实待建组件装配，强制流开启顺序并分发业务报文；断言非法消息格式被拒绝，断言 OS 隔离边界得到执行。

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-board-agent --test interop_dual_stream`
预期：PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-board-agent/src/tunnel_binding.rs crates/rsetup-board-agent/src/lib.rs crates/rsetup-board-agent/tests/interop_dual_stream.rs
git commit -m "test(board-agent): add real in-memory component dual-stream interop test"
```

---

### Task 9: 进程级可执行入口与独立 TCP 监听骨架（真实网络双进程 Smoke 保持 BLOCKED）

**状态说明：**
- **非密码骨架代码（可独立推进）：** 板端可执行程序骨架（`main.rs`）与中控独立设备安全 TCP 端口监听器骨架（`listener.rs`）。
- **真实网络双进程只读业务 Smoke（严格 BLOCKED）：** 依赖加密通道建立与 01/03 联合验收门。严禁使用空 `#[ignore]` 单测并在执行 `--ignored` 时“空跑通过”冒充验收通过！测试代码中必须明确拒绝缺失真实依赖的静默放行；在安全审查未解除前，网络双进程 Smoke 绝不声称具备可执行验收条件。宿主机真实 reboot 调用绝对为零！

**Files:**
- Create: `crates/rsetup-board-agent/src/main.rs`
- Create: `crates/rsetup-controller/src/devices/listener.rs`
- Modify: `crates/rsetup-controller/src/devices/mod.rs`
- Test: `crates/rsetup-controller/tests/process_loopback_smoke.rs`

**Interfaces:**
```rust
pub struct DeviceTcpListener {
    listener: tokio::net::TcpListener,
}

impl DeviceTcpListener {
    pub async fn bind(addr: &str) -> std::io::Result<Self>;
    pub fn local_addr(&self) -> std::io::Result<std::net::SocketAddr>;
}
```

- [ ] **Step 1: 建立可编译桩与编写失败测试**

在 `crates/rsetup-controller/src/devices/mod.rs` 导出 `pub mod listener;`。
在 `listener.rs` 提供桩（`local_addr` 桩返回固定端口 0）。
在 `crates/rsetup-board-agent/src/main.rs` 提供最小入口骨架。

在 `crates/rsetup-controller/tests/process_loopback_smoke.rs` 编写基础监听测试：
```rust
use rsetup_controller::devices::listener::DeviceTcpListener;

#[tokio::test]
async fn loopback_listener_binds_independent_port() {
    let listener = DeviceTcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    assert_ne!(addr.port(), 0);
}

// 真实双进程网络业务 Smoke 依赖加密握手与 01/03 联合门，保持 #[ignore] 并明确断言依赖与前置条件
#[ignore = "BLOCKED until external security expert decision and full crypto handshake are approved"]
#[tokio::test]
async fn full_process_loopback_smoke_blocked() {
    // 明确校验：若被强行 --ignored 运行，必须因缺少安全门解除凭证或必要依赖而失败，绝不空绿放行！
    panic!("Network dual-process smoke is BLOCKED until security review gate is resolved");
}
```

- [ ] **Step 2: 运行定向测试并确认断言失败（RED）**

运行：`cargo test -p rsetup-controller --test process_loopback_smoke loopback_listener_binds_independent_port -- --exact`
预期：编译通过，断言失败（`addr.port() != 0` 失败），退出码非零。

- [ ] **Step 3: 最小实现（GREEN）**

在 `crates/rsetup-controller/src/devices/listener.rs` 中包装真实 `TcpListener`：
```rust
use std::io;
use std::net::SocketAddr;
use tokio::net::TcpListener;

pub struct DeviceTcpListener { listener: TcpListener }

impl DeviceTcpListener {
    pub async fn bind(addr: &str) -> io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        Ok(Self { listener })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }
}
```

- [ ] **Step 4: 运行定向测试并确认通过（PASS）**

运行：`cargo test -p rsetup-controller --test process_loopback_smoke loopback_listener_binds_independent_port -- --exact`
预期：PASS。

- [ ] **Step 5: 提交**

```bash
git add crates/rsetup-controller/src/devices/listener.rs crates/rsetup-controller/src/devices/mod.rs crates/rsetup-controller/tests/process_loopback_smoke.rs crates/rsetup-board-agent/src/main.rs
git commit -m "feat(transport): wire device tcp listener and agent entrypoint skeleton"
```

---

### Task 10: 【BLOCKED】加密协商与 AES-256-GCM 记录层（安全门关卡）

**状态声明：严格 BLOCKED**

**前置硬阻断条件：**
1. 独立外部密码/协议专家的书面决议原件已到达并完整索引在 `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md`。
2. 专家对以下五项开放决策给出明确书面决议：
   - Client / Server 签名转录是否签入 `FrameType`；
   - 记录层部分帧有界单调读取/发送堵塞的**具体单调时限数值**、起算点及缓冲背压；
   - 序号末值（`2^64-2`）与 16B 零明文要求；
   - raw wire 固定数值向量的独立生成与跨 Protobuf 解析器一致性；
   - 双端升级顺序与无降级回滚策略。
3. 现行规范 `docs/protocol_spec.md` 完成 v3 修订小提交，经用户再次复审。
4. 真实独立固定向量存入 `docs/superpowers/vectors/2026-10-03-tunnel-v3-wire-vectors.json`。

**受阻断的文件清单：**
- Create: `crates/rsetup-protocol/src/crypto.rs`
- Create: `crates/rsetup-protocol/src/record.rs`
- Test: `crates/rsetup-protocol/tests/crypto_vectors.rs`
- Test: `crates/rsetup-protocol/tests/record_layer.rs`

**红线禁令：**
- 现行线协议规范仍为 `docs/protocol_spec.md` (v2)；
- 绝不提前自定 v3 签名转录字段（如 `ClientSignInput` / `ServerSignInput` 的具体排布）；
- 绝不自创未经专家审查的额外协议；
- 绝不伪造专家决议报告；
- 绝不以虚拟测试绕过加密阻断。在安全门解除前，Task 10 严禁开工。

---

## 覆盖矩阵与最终验收门禁

| 验收项目 | 对应任务 | 规格/协议来源 | 实施状态 |
|---|---|---|---|
| WIRE-01 业务版本=1、超限校验、`tunnel.ping` raw 16B 载荷与空元数据 | Task 1 | 03 规格 §1, 现行规范 §6.4 | 纯组件可实施 |
| ADM-01 准入快照 CAS、PENDING+denied 拒绝、连接配额与存储配额独立限制 | Task 2 | 02 规格 §2.2, 05 规格 §6 | 纯组件可实施 |
| 板端只读白名单：仅 capabilities/status/clock/task.get，schema 解码/版本校验，拒绝远程变更/shell | Task 3 | 03 规格 §1, 04 规格 §1 | 纯组件可实施 |
| WIRE-04/05 幂等任务日志：临时文件+fsync+rename+父目录fsync原子落盘、持久UUID epoch、pre_boot落盘、跨 Boot 证据核实恢复、同 Boot 不宣称成功 | Task 4 | 03 规格 §2, 04 规格 §3 | 纯组件可实施 |
| WIRE-03 重启票据 30s TTL、固定无参 OS 边界、OS 明确错误持久 Failed 走成功信封 | Task 5 | 03 规格 §1, 04 规格 §2 | 纯组件可实施 |
| 双流路由与 Probe 调度器：首 Probe 每次 15s 且 3 次独立重开失败连接升级、运行期 3 窗口坏流重开、HTTP/2 PING/ACK 死链 | Task 6 | 现行规范 §6.6, 05 规格 §7 | 纯组件可实施 |
| 准入变更提交后通知：DB 事务内零网络、保持同步 after_commit 有界队列、重读权威 DB、连接代际保护与定向关闭补偿 | Task 7 | 01 规格 §4, 02 规格 §2.2 | 纯组件可实施 |
| 双端双流消息级内存互操作：真实待建组件装配、先 Control 后 Data、RPC 往返、票据落盘与 OS 隔离（禁止纯布尔伪互通） | Task 8 | 00 规格 §4, 04 规格 §2 | 内存测试可实施；**加密握手 BLOCKED** |
| 独立设备 TCP 端口绑定与板端 0600 私钥防覆盖入口骨架（Smoke 禁止空跑通过） | Task 9 | 05 规格 §1, 06 规格 AT-04 | 骨架单测可实施；**网络业务 Smoke BLOCKED** |
| 加密密钥协商与 AES-256-GCM 记录层：外部专家决策硬关卡与固定向量 | Task 10 | 协议 §5, 安全修订计划 | **严格 BLOCKED** |

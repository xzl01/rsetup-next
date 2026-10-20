# Controller V1 四专题 Task 4 · Runtime 任务执行收敛 实施计划

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实。使用 `- [ ]` 复选框跟踪步骤。
>
> 本计划交付的是**真实运行时执行路径**（NTP 启动门禁真请求关联、任务状态模型、schema v4、原子预览/提交与设备锁、共享 `BoardClient`、意图后发送与只读恢复、必需任务 HTTP 端点）。本计划**不授权**执行真实数据库迁移、不授权触碰真实凭据、不授权改动线协议 codec。凡涉及真实库的步骤均已明确标注为**条件门禁步骤**，未满足前置时必须保持未运行并在验收账本中记录 `BLOCKED`，**不得**以 `return`、空 fixture、常量结果或 `eprintln!("BLOCKED")` 冒充 PASS，也不得伪造 accepted/备份证据。

**Goal:** 在四专题设计（`docs/superpowers/specs/2026-10-07-controller-v1-four-task4-design.md`）与 Runtime remaining 计划 Task 2/3/4 及必要 Task 6 HTTP 的裁决下，把 Controller V1 Runtime 从“纯模型 + 独立解析切片”推进到可独立审查的**任务执行闭环**：完整 NTP 30s 启动门禁真请求关联、10 状态任务模型与原因码、schema v4 版本化只读 guard 与五表 SQL、原子预览/提交与设备唯一操作锁、共享类型化 `BoardClient` 与“先提交意图、后再发送”、恢复只读不重发，以及最小必需任务 HTTP 端点（含 `time-review`、`release-lock`、分页授权子集投影）。

**Architecture:**
1. **时间门禁（真请求关联与 RFC 偏移）**：既有 `controller-v1-runtime-ntp-parser` 分支交付的纯结构解析器**从该独立分支导入**，只做 48B/LI/VN/Mode/Stratum/发送时戳结构校验，绝不产生 `NtpValid`。在 RED 桩测试前先在导入的 `ntp.rs` 中扩充 `NtpError` 枚举变体确保可编译。请求 T1 的 64 位唯一发送时戳/nonce 仅由响应 Originate 全量回显关联；响应 T2/T3 是服务端时间，不得当作客户端回显。响应来源必须校验为请求实际解析/发送到的 `SocketAddr`（禁止直接与 DNS host 字符串比较）。T1 快照同时保存本地墙钟与单调起点，T4 为同一瞬间快照。往返上限（RTT）由本地 T1/T4 单调跨度（monotonic span）严格度量；偏移（offset）由 RFC 四时戳公式经 UTC/NTP epoch checked i128 精确换算，产出专门定义的 `NtpSample`（不重用 `board_clock::ClockEstimate` 的 board_offset 模型以免混淆）。正反测试由标准服务器响应构造，即使本地当前墙钟存在大偏移仍可接受合格样本。`src/time/gate.rs` 承载单一 UDP socket 真实往返，`GateDeadline` 承载 **≤30s 启动预算**（00 §3 C-TIME 已确认约束：**30s 处强制关闭**），超时走 `SystemFallback` 并在后台有界退避重试。启动路径收敛为单一 `run_startup_sample`：只创建一次绝对 deadline（硬 ≤30s），socket bind/DNS/多源/全部重试同处一个绝对 `timeout_at` 之下，成功返回 sample+source 供 `start_time_gate` 写 `TimeEvidence`，超时返回 `None` 走 `SystemFallback`。
2. **任务状态模型**：`SubTaskState` 10 态与 `ReasonCode` 12 个大写下划线原因码构成唯一机器语义，`assert_subtask_transition` 以纯函数固化 04 §3 合法转移表（含 `dispatching` 不得直达 `succeeded`、终态不可反向、`queued → dispatching` 前置、**不得以用户直接调用的 `advance_subtask` 跳过 `dispatching`**）。HTTP/前端只在传输层内嵌 `intent` 证明，展示层一律使用顶层 `state`，杜绝“把 accepted 展示成成功”。
3. **Schema v4 与版本化 guard**：`0004_tasks.sql` 新增 5 张表（`task_previews`、`main_tasks`、`sub_tasks`、`device_operation_locks`、`recovery_checks`），**无 FK、无 CHECK**；`src/integrity.rs` 的硬编码 `(1, 3)` 重构为共同版本化只读检查（`SUPPORTED_SCHEMA_VERSION = 4`、`MIN_SUPPORTED_SCHEMA_VERSION = 1`），识别但拒绝 `5+`；迁移只允许在只读核验、**真实备份**、隔离、独占与停写门禁全部满足后由操作员显式执行。
4. **原子预览/提交与锁**：预览固定设备清单（1..=1024）并持久化 token 摘要与**固定目标快照**（`targets_json`）；提交流程在**同一短事务**内消费 preview token（唯一键 + `DELETE ... WHERE expires_at > NOW(6) AND preview_generation = ?` 的原子单次使用；有效性同时受**进程启动代际**与**进程内单调 60s TTL**约束，`NOW(6)` 墙钟仅作上限兜底，口径见 Task 4 Step 4 第 5 步）、以快照读出的固定目标清单为**唯一目标真相源**（不采信请求体 `target`/`target_hash`）、插入 `main_tasks` 幂等键行、按 `schema_meta FOR UPDATE` → `users` → `roles`(`role_id` 字节序升序) → `device_groups`(`group_id` 字节序升序) → `devices`(`public_key` 字节序升序) → `device_operation_locks` 的锁序逐目标重验当前权限/可见性并插入子任务、对每个目标执行 `INSERT ... SELECT ... WHERE NOT EXISTS(...)` 的原子设备锁 CAS。锁竞争者记 `failed/DEVICE_BUSY`，**绝不排队**；主任务 `counts_json` 由子任务实际结果（含 `locked`/`device_busy` 分类）在同一事务内回写；事务内零网络 I/O。
5. **发送与恢复**：`BoardClient` 为唯一共享类型化板端接口，直接消费 `rsetup_protocol::wire` DTO 并**完整保留** `boot_id`/`valid_for_ms` 票据与 `TaskRecord` 证据，绝不降级为 `Result<()>`/`Result<String>`。调度顺序固定为 prepare → 复查权限 → 短事务提交 `dispatching` 意图 → **事务提交后**在事务外由当前进程至多一次发送 execute。恢复扫描把 `dispatching/accepted/verifying` 视为“可能已发送”，只做只读 `task.get`，**绝不自动补发**；`unknown` 释放并发槽但**始终持有设备锁**。
6. **HTTP 边界（严格 8 条，与最新规格逐字对齐）**：`src/http_tasks/` 只提供 `POST /api/v1/task-previews`、`GET /api/v1/tasks`、`POST /api/v1/tasks`、`GET /api/v1/tasks/{id}`、`GET /api/v1/tasks/{id}/children`、`POST /api/v1/tasks/{id}/cancel`、`POST /api/v1/tasks/{id}/time-review`、`POST /api/v1/subtasks/{id}/release-lock`。**不定义** `GET /task-previews`，**不实现** SSE `/events`、`/system/time` 或任何轮询/快照增强（未实施部分在验收账本中列为缺口，**不得**伪称完整 Runtime Task 6）。全部复用既有单值 Host、严格 Cookie、`AuthServiceApi` 权威会话、Origin/CSRF、限速与 JSON 边界；写端点缺失有效会话**不得触达业务仓储**。`TaskApi` 为对象安全边界，生产装配只从真实 SQLx 仓储接线，数据库错误返回 503，**绝不降级为空权限、常量空结果或 404**。

**Tech Stack:** Rust 2024 / MSRV 1.85，Tokio 1.47（新增 dev-dependency 特性 `test-util` 仅用于测试且本计划不使用 `start_paused`），Axum 0.8，SQLx 0.8（MySQL，无 FK/CHECK），Chrono 0.4，Uuid 1.18，Serde 1.0，Sha2 0.10，Hex 0.4，Tower 0.5。全部构建/测试命令使用 `--offline --locked`。

**Spec:** `docs/superpowers/specs/2026-10-07-controller-v1-four-task4-design.md`（本计划的正向裁决）；`docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`（状态机与恢复）；`docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md`（板端消息）；`docs/superpowers/plans/2026-10-07-controller-v1-remaining-runtime-tdd.md` Task 2/3/4 与必要 Task 6；`docs/superpowers/plans/2026-10-07-controller-v1-remaining-identity-tdd.md` Task 2/3 协作点。执行者必须同时阅读设计文档与 Runtime remaining 计划的对应 Task，本计划与它们冲突时以设计文档的书面裁决为准并记录偏差。

---

## Global Constraints

1. **纯代码 + 文档执行约束：** 只允许新增/修改下表中列出的文件。不读取 `secret/` 下任何内容、不连接任何真实数据库、不执行迁移脚本、不访问外部 NTP 或真实板端网络、不在未获授权时 `git add`/`git commit`。提交步骤仅在**前置条件全部满足且用户/审查者许可**后执行；未获许可时保留工作树变更并如实报告。
2. **可编译 RED 约束：** 每个任务必须先落地**可编译桩**（真实签名 + 故意错误的行为），再运行命令行中列出的**精确**测试名，观察因**业务断言失败**而产生的 RED。严禁以缺失模块、缺包、缺 fixture、缺数据库或 0 用例匹配充当 RED，也严禁以“实现已正确、测试首次即绿”冒充 RED。`ControllerError` 未派生 `PartialEq`，错误断言统一使用 `matches!(res, Err(ControllerError::...))`。
3. **测试命令约束：** 统一使用 `cargo test --offline --locked -p rsetup-controller <唯一测试函数名> --`，并在 `预期` 中写明“恰好执行 1 个用例”。仅当需要按模块路径精确定位时才追加 `--exact`；`-p rsetup-controller` 的集成测试定位统一使用 `--test <test 文件名>`（`src/bin/` 下无测试时不再叠加 `-p` 与 `--test`）。
4. **NTP 来源约束：** `src/time/ntp.rs` 与 `mod.rs` 中的 `parse_sntp_response` 导出**必须从既有分支 `controller-v1-runtime-ntp-parser` 导入**（`git show controller-v1-runtime-ntp-parser:crates/rsetup-controller/src/time/ntp.rs` 或等价 merge/cherry-pick），不得在本分支重写第二套解析器。该解析器**只做结构校验**，任何“结构合法”都不得升级为 `NtpValid`。在 Step 1 RED 桩测试前，**允许且必须在导入的 `src/time/ntp.rs` 中就地扩充 `NtpError` 枚举变体**（追加 `Io`、`SourceMismatch`、`OriginateMismatch`、`ZeroReceiveTimestamp`、`RttTooLarge`、`TimestampOverflow`），不修改已有 6 个变体与 `parse_sntp_response` 逻辑，以保证后续桩和测试可编译。
5. **数据库无外键/无 CHECK：** `0004_tasks.sql` 不得声明 `FOREIGN KEY` 或 `CHECK`；关联有效性、状态转移合法性与设备锁互斥一律由应用层锁序 + CAS 保证。不得依赖 TiDB 全局 CHECK 开关。
6. **迁移与备份门禁（严格条件步骤）：** 执行 `0004_tasks.sql` 的硬性前置是：①目标确认为已授权的两个专用隔离开发库之一；②只读核验 `SELECT DATABASE()`、`schema_version = 3`、身份表形状与数据完整、引擎版本与实际权限；③在该目标制作**真实备份**并校验 SHA256 与可读性、保留去敏匹配回执；④独占与停写条件确认；⑤现有显式迁移门禁存在。任一缺失则迁移步骤标 `BLOCKED` 并**不执行**。不得伪造 `backup_ref`/ACK，不得执行 `DROP DATABASE`，不得把凭据或备份内容写入任何被跟踪文件、聊天或普通日志。跳过需要数据库 root 权限的锁验证，但不跳过应用账号下的设备锁真实事务验证。
7. **真实库测试门禁（严格条件步骤）：** 所有涉及真实 MySQL/TiDB 的用例必须 `#[ignore]`，仅在操作员显式提供 `CONTROLLER_TEST_DATABASE_URL` 时以 `--ignored --test-threads=1` 串行执行；缺 URL 时**不运行**并记录 `BLOCKED`，连接失败或执行 0 用例**严禁**视为通过。**双引擎（MySQL 8.4 LTS 与 TiDB 8.5 LTS）必须各自独立执行并分别记录被测引擎版本**；现有 `mysql_identity`/`tidb_identity` 测试是 v3 专用测试，不可当作 v4 迁移验收，必须以独立的 `migration_v4` 测试明确验证 `schema_version = 4`、16 张表、无 FK、无 CHECK；跨进程故障测试（进程在“意图已提交、execute 未发送”窗口被杀）必须以**新进程**启动恢复入口，只把跨进程 DB 断言与 `execute` 调用计数为证据，绝不把同进程纯函数调用充当跨进程恢复证明。
8. **线协议依赖约束（未决项，不得伪造）：** 设计 §4 已确认 `rsetup-protocol::wire` **当前尚无** `Capabilities`/`DeviceStatus`/`ClockSample`/`RebootTicket`/`RebootExecute`/`TaskRecord`（本分支 `crates/rsetup-protocol/src/wire.rs` 仅 179 行，只有 `PacketType`/`TunnelPacket`/`WireError`/`validate_packet`）。Task 5 必须**先执行 Step 0 门禁判定**：若这些 DTO 尚未由获批的 transport 工作交付，则 Task 5 的 `BoardClient` 实现步骤整体标 `BLOCKED`，此时 `scheduler.rs` 仅实现纯函数/纯结构，泛型 `TaskScheduler`/`run_once` 保持未定义并标 `BLOCKED`；只允许提交**不含** `BoardClient` 的 `recovery.rs`/`model.rs` 纯逻辑切片（这些模块不得 `use rsetup_protocol::wire`），并保留 `src/tasks/board_client.rs` 不在本次提交中创建。绝不自定义编码器、固定业务 bytes 或影子 DTO 伪造互通。controller 新增本地 `rsetup-protocol` 直接依赖（workspace path 依赖，不装新包）与离线 `Cargo.lock` 更新为**明确待审步骤**，门禁通过前不自动修改 manifest（当前分支 `crates/rsetup-controller/Cargo.toml` 无 `rsetup-protocol` 依赖）。
9. **身份协作点与 Guard 期望版本约束：** Guard 的期望版本来自编译期配置 `SUPPORTED_SCHEMA_VERSION = 4`，而非从数据库行本身读取；`db.rs` 中的 `identity_schema_decision` 等旧 `3` 硬编码必须统一重构，`v3` 仅用于显式迁移 preflight，普通启动对旧版（v1..=3）、未知版本（v5+）或脏数据一律拒绝（fail-closed）；不得新增 `device.task.read` 之外的独立简写权限（`task.read` 简写一律映射 `device.task.read`）；不得定义第二套会话/身份模型，HTTP 层只消费既有 `AuthServiceApi`/`Session` 边界；`src/integrity.rs` 的版本化 guard 是本专题对外最敏感的协作点，修改后必须让登录、改密、准入与任务四条路径**共用同一**只读检查。
10. **共享 `BoardClient` 唯一性约束：** 全仓只允许存在一个 `BoardClient` trait 定义（位于 `src/tasks/board_client.rs`）。任何就地 `struct Board`、`trait BoardRpc` 或第二套 DTO 都属于违规，审查阶段直接拒绝。
11. **错误与状态单调性约束：** `unknown` 释放并发槽但**不释放设备锁**；已持久化的意图、终态与证据**不得倒退**；迟到有效证据只修正 outcome/计数并递增 revision，**不重新执行**，且**不得删除后续任务新 generation 的锁**。

---

## 文件结构与职责划分

| 文件路径 | 动作 | 模块归属 | 核心职责 |
| :--- | :--- | :--- | :--- |
| `crates/rsetup-controller/src/time/ntp.rs` | Import | 时间纯解析 | 从 `controller-v1-runtime-ntp-parser` 分支导入的 48B 结构解析器（只校验结构，绝不产生 `NtpValid`） |
| `crates/rsetup-controller/src/time/socket.rs` | Create | 时间 I/O 边界 | 唯一 SNTP UDP 往返封装：`SntpResolver` 抽象 + 单次请求/响应读写时戳 |
| `crates/rsetup-controller/src/time/gate.rs` | Create | 时间门禁 | 请求↔响应关联判定、`GateDeadline` ≤30s 预算、`TimeGateHandle` 后台退避 |
| `crates/rsetup-controller/src/time/mod.rs` | Modify | 时间导出 | 追加 `socket`/`gate` 导出并保留 `parse_sntp_response` |
| `crates/rsetup-controller/src/tasks/model.rs` | Create | 任务模型 | 10 态 `SubTaskState`、`ReasonCode`、主任务状态/outcome、转移 CAS 纯函数、`TaskIntent`/`TaskEvidence`/`TaskRecordRef` |
| `crates/rsetup-controller/src/tasks/mod.rs` | Create | 任务导出 | 导出 model/service/scheduler/recovery/board_client（board_client 受 Task 5 Step 0 门禁约束） |
| `crates/rsetup-controller/src/tasks/preview.rs` | Create | 任务预览 | 目标展开、去重排序、1..=1024 校验、preview token 生成与摘要 |
| `crates/rsetup-controller/src/tasks/service.rs` | Create | 任务服务 | `TaskService`：预览、提交、取消、time-review、release-lock 的事务编排 |
| `crates/rsetup-controller/src/tasks/repository.rs` | Create | 任务持久化 | `TaskRepository` trait、内存 fake、SQLx 实现（锁序、幂等、原子 consume） |
| `crates/rsetup-controller/src/tasks/board_client.rs` | Create（条件） | 板端契约 | 唯一共享类型化 `BoardClient` trait（6 方法 dyn-safe 契约）+ `BoardFuture` + `BoardClientError` + `DeviceIdentity`（内部调度上下文；Step 0 门禁未过则不创建/不导出） |
| `crates/rsetup-controller/src/tasks/scheduler.rs` | Create | 调度纯逻辑 | 槽位准入纯函数、`DispatchPlan`、按记账发送顺序的调度器 |
| `crates/rsetup-controller/src/tasks/recovery.rs` | Create | 恢复 | 恢复分类（只读/不发）、主任务汇总 `calculate_main_task_outcome` |
| `crates/rsetup-controller/src/tasks/evidence.rs` | Create | 证据核验 | TaskRecord 成功/失败/版本合并判定（依赖 Task 5 门禁的 DTO） |
| `crates/rsetup-controller/src/http_tasks/mod.rs` | Create | HTTP 装配 | `TaskApi` 对象安全边界、`HttpTasksState`、`build_tasks_router(HttpTasksState) -> Router<()>`（本任务不自改 `main.rs`） |
| `crates/rsetup-controller/src/http_tasks/decision.rs` | Create | HTTP 纯判定 | HTTP 请求/响应 DTO、取消/释放/time-review/分页的纯权限与投影判定 |
| `crates/rsetup-controller/src/http_tasks/routes.rs` | Create | HTTP 处理器 | 规格 8 条端点的 handler（复用 Identity 共享 gate：Host/Cookie/CSRF/限速/JSON 边界） |
| `crates/rsetup-controller/src/http_device_status.rs` | Create | 设备状态读 | 正式 Runtime 路由 `GET /api/v1/devices/{id}/status` handler + `DeviceStatusSnapshotSource` 状态源 trait（读真实缓存快照，不触发执行/采集） |
| `crates/rsetup-controller/src/integrity.rs` | Modify | 应用完整性 | `SUPPORTED_SCHEMA_VERSION = 4`、版本化 `validate_guard_rows_for_version`、运行时表形状/数据扫描 |
| `crates/rsetup-controller/src/db.rs` | Modify | 数据库访问 | v4 迁移语句常量、`validate_schema_v4_rows`、guard 消费接线 |
| `crates/rsetup-controller/src/lib.rs` | Modify | 根导出 | `pub mod http_tasks;`、`pub mod http_device_status;` 与 `pub mod tasks;` |
| `crates/rsetup-controller/src/main.rs` | 集成任务所有 | 生产装配 | 顶层装配（三子 router 合并、注册 `build_tasks_router`/`build_device_status_router`，生产装配仅真实 SQLx 仓储）归唯一独立集成任务所有者；Runtime 各任务**不自改** `main.rs` |
| `crates/rsetup-controller/Cargo.toml` | Modify | 依赖声明 | 声明直接依赖 `serde`（derive）与 dev-dependency `tokio` 的 `test-util` 特性（Task 4 恢复测试需要 `tokio::task::yield_now` 语义外的暂停时钟保留）。本计划测试**不使用** `start_paused`；Task 5 门禁通过时另将本地 `rsetup-protocol` path 直接依赖与离线 lock 更新列为**独立待审步骤**（不装新包、不自动改 manifest，不并入本行 serde/tokio 变更）。**依赖前置（RT-N1）**：本文件必须显式声明 `sqlx = { workspace = true, features = ["derive"] }`——workspace 根 `sqlx` 为 `default-features = false` 且特性列表**不含** `derive`，不开启该特性则本 crate 内 `#[derive(FromRow)]`（`MainTaskRawRow`/`SubTaskRawRow`/`PreviewSnapshotRow` 等）编译失败 |
| `crates/rsetup-controller/migrations/0004_tasks.sql` | Create | 数据库迁移 | 5 张 runtime 表（无 FK/CHECK，含 PK/UNIQUE/NOT NULL/UNSIGNED） |
| `crates/rsetup-controller/tests/gate_udp.rs` | Create | 集成测试 | 受控 fake UDP 应答的 30s 门禁、关联失败与启动总预算（提前返回 / DNS 取消 / 多源不扩容 / 空源有界等待 / 单次请求 3s 上限表驱动与多源真实路径）用例 |
| `crates/rsetup-controller/tests/migration_v4.rs` | Create（条件） | 集成测试 | 显式 v4 迁移验收：schema_version=4、16 表、无 FK、无 CHECK 独立断言（`#[ignore]`） |
| `crates/rsetup-controller/tests/task_db.rs` | Create（条件） | 集成测试 | 真实 v4 库的设备锁互斥/幂等/原子 preview 消费/过期后重放（`#[ignore]`） |
| `crates/rsetup-controller/tests/controller_recovery.rs` | Create（条件） | 集成测试 | 跨进程恢复不重发与迟到证据修正（`#[ignore]`） |
| `crates/rsetup-controller/tests/task_http.rs` | Create | 集成测试 | Axum oneshot 权限矩阵、主任务摘要投影与授权子集投影 |

**任务依赖：** Task 1（时间门禁）与 Task 2（状态模型）无前置、可并行；Task 3 依赖 Task 2；Task 4 依赖 Task 3；Task 5 依赖 Task 4（且受 Step 0 wire DTO 门禁；DTO 缺席时 scheduler.rs 仅纯函数/纯结构，泛型 TaskScheduler/run_once 标记 BLOCKED 等待 BoardClient 定义）；Task 6 依赖 Task 4（消费 TaskService 与完整 DTO 字段；`TaskApi` + fake 仓储即可独立推进，不等待 Task 5）。Task 6 的读桶变体（`TaskList`/`TaskDetail`/`TaskChildren` 与 `DeviceStatusRead`）由 Identity 一次性加入 `EndpointId`（接线依赖，见 Task 6 切片）；`GET /devices/{id}/status` 的**真实**快照源受 Task 5 Step 0 wire DTO 门禁约束（DTO 缺席时具体适配器标 `BLOCKED`，不阻塞 Task 6 其余路由）。

---


---

## 专题带编号拆分模块索引（单文件 700~2700 行）

为保障可维护性并消除单文件过大问题，完整执行细节按职责已拆分到对应子目录中（保留本主计划在 `docs/superpowers/plans/` 作为执行总索引）：

- [01-ntp-gate-and-driver.md](2026-10-16-runtime/01-ntp-gate-and-driver.md)：Task 1: 完整 NTP 30s 门禁、时钟单调驱动与平滑阶跃（~1102 行）
- [02-state-model-and-guard.md](2026-10-16-runtime/02-state-model-and-guard.md)：Task 2–3: 10 状态/原因码、Schema v4 Guard 与 SQL DDL（~1066 行）
- [03-preview-and-submit.md](2026-10-16-runtime/03-preview-and-submit.md)：Task 4: 原子预览/提交、设备唯一操作锁与混合聚合收敛（~1886 行）
- [04-board-client-and-http.md](2026-10-16-runtime/04-board-client-and-http.md)：Task 5–6: BoardClient、意图后发送、time-review 与任务 HTTP 端点（~1623 行）

---


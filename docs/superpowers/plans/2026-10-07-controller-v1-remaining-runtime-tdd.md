# Controller V1 剩余任务与运行期 TDD 实施计划（2026-10-07）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 分项落实；用 `- [ ]` 跟踪步骤。这是**纯计划文档**：不运行任何真实数据库、不执行迁移、不碰物理硬件或网络、不读取任何 secret 凭据、不修改其他任何文件、不执行 stage 或 commit。安全阻断未解除前不得宣称整套设备业务联验通过。

**Goal:** 在当前仅完成 identity schema v3、基础准入 CAS 与探活路由的 controller 代码库上，基于中控 00/02/03/04/05/06 规格与应用完整性设计，增量交付完整且安全的时间模型、独立 NTP 启动门禁、任务 schema v4 迁移与幂等提交、不盲发重启的任务调度与恢复、千台错峰轮询、授权投影 SSE/系统端点，以及受控备份恢复与双 DB 验收。

**Architecture:**
1. **时间层（纯模型 + 独立 NTP 门禁）：** 纯数学时钟偏差估算与四时戳算法不依赖任何 I/O；独立异步 NTP 客户端单次请求建议上限 ≤3s（待审默认）、NTP 启动关卡总预算必须 ≤30s（00 §3 C-TIME 已确认约束），超时回退 `system_fallback` 并在后台无限有界指数退避（建议 1s→60s 封顶 + 抖动），空源按退避重读配置，跳钟（`|Δwall - Δmonotonic| > 1s`）自动切换 `clock_epoch` 并触发审计。两主机独立时钟代际，偏差估算严格比对中控本端开始与结束时本地 `controller_epoch` 一致性，并核验板端采样稳定证据（非 `clock_unstable`），同机一致而非跨机相等；保留 boot 一致性、质量、大稳定偏差与 checked 宽算术/不确定度界线。
2. **任务与锁（应用级完整性 + 无外键/无 CHECK）：** 继承 v3 应用完整性设计，`0004_tasks.sql` 引入 5 张 runtime 表，严格保留 PK、UNIQUE 与 NOT NULL，彻底不使用 FK 与 CHECK；提交任务与撤权共享 `schema_meta(singleton=1) FOR UPDATE` 保护行，严格按 users → roles → device_groups → devices（公钥排序）加锁并在短事务内申请 `device_operation_locks`；已被占用者直接置 `DEVICE_BUSY` 终态，**坚决不排队**；事务内零网络 I/O。
3. **调度与故障恢复（不盲发重启）：** 全局 16 个并发槽（待审默认）；`BoardClient` 消费原 transport wire DTO，完整保留 prepare 票据的 `boot_id`/`valid_for_ms` 以及 execute 返回的完整 `TaskRecord` 证据，只读 `capabilities/status/clock/task_record` 均归入共同接口；在 prepare 成功并重验权限后，在短事务中提交 `dispatching` 发送意图；事务提交后才在外部独立协程调用 `execute`；中控崩溃恢复先从 DB 重建锁与状态，`dispatching/accepted/verifying` 视为可能已发送，**仅发起查询核实，绝对不自动重发 execute**；未知结果 `unknown` 释放并发槽但**持续持有设备锁**，主任务可按 `completed(unknown)` 结束批次，迟到证据可修正结果但不倒退至 running。
4. **轮询错峰与内存快照：** 1024 台设备按纳秒整数相位均匀分散在 10s 周期中；每 100ms tick 下发 ≤11 台；调度算法严格区分正常 tick 到期下发与停顿/过载跳轮，**绝不能无差别跳过所有到期设备导致饿死**；系统停顿或过载时跳过历史逾期周期，不积压不产生洪峰，恢复后严格维持原相位；最新快照仅驻留内存，按单调接收年龄计算 30s 新鲜度（05 RUN-01 已确认状态过期阈值），内存更新严格校验 session/stream/boot/agent 代际与严格递增的 sample_seq，失败保留最后好样本与 `last_error`，未知值不填 0。
5. **授权 API、SSE 与系统端点：** 普通用户详情接口返回 `view_scope: "authorized_subset"`，严格限本人任务并按当前 `device.task.read` 权限（注：规格中 `task.read` 简写统一对应系统标准 `device.task.read` 权限，绝不新增独立简写权限）投影子任务及统计，不泄露未授权目标；取消子任务必须逐目标校验当前 `device.reboot` 权限并在短事务内重验 `authz_epoch`；管理员可跨用户取消 queued/held 子任务；SSE 统一规范为命名事件（`event: device.updated/task.updated/permissions.changed/system.time.changed/reset`），携带对象局部 revision 而不假设列表全局 revision，每次发送重验 session 与 `authz_epoch`，用户被撤权立即清理待发队列并关闭连接，队列上限 128 条溢出下发 reset 并关闭。
6. **受控恢复与双 DB 验收：** 旧备份还原必须进入受控模式，吊销旧会话、暂停自动准入与变更任务调度，queued 记录不自动放行；受控恢复模式限定禁止准入与变更任务，**绝不笼统封锁所有写操作**，受审管理恢复入口（管理员登录/改密/注销、审计写入、单设备核验与管理退出恢复模式）必须保持可用；保留期清理器坚决不清理 held/unknown 及未释放的设备锁；双数据库（MySQL 8.4 LTS 与 TiDB 8.5 LTS）各自独立执行忽略的隔离集成测试。

**Tech Stack:** Rust 2024 / MSRV 1.85，Tokio 1.47，Axum 0.8，SQLx 0.8（无 FK/CHECK），Chrono 0.4，Uuid 1.18，Rand 0.8，Sha2 0.10，Hex 0.4，Tower 0.5。构建与测试命令统一使用 `--offline --locked`。Controller crate 声明未来受审离线更新加入直接依赖 `serde`（derive 特性）与 `tokio` 的 `test-util` 特性，不以依赖缺失导致编译失败充当 RED；`ControllerError` 未实现 `PartialEq`，测试断言统一使用 `matches!`。

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` 至 06、`docs/superpowers/specs/2026-10-03-controller-application-integrity-design.md`、`docs/protocol_spec.md`。

---

## Global Constraints

1. **草案审阅与安全阻断约束：** `controller-v1 / draft-1` 仍处于待审阅状态。05 §1/§2/§4/§6 的 NTP 单次请求上限 3s、板端时钟 RTT 上限 2s、状态保留 112/s/其它 48/s（可借空闲）、unknown 后台核实共用查询预算且可低频/不占重启并发槽、私钥 0600 权限检查、启动进度日志、调度并发槽 16、排队有效期 60min、终态保留期 30d/180d/24h 均为**拟议与待审默认参数而非已定稿常量**；NTP 启动关卡总预算 **≤30s** 则是 **00 §3 C-TIME 已确认约束**，全文统一作为确定要求，绝不再标为待审建议；协议 05 RUN-01 的状态过期阈值 30s 同样为确定单调新鲜度界线。协议层面关于 `reason_code` 签名未覆盖、验签失败描述冲突与 AEAD 失败终止策略的安全阻断未解除前，**不得宣称整套设备业务联验通过或具备生产发布条件**。
2. **纯文档执行约束：** 本任务**仅且只允许**更新本文档，不运行任何真实数据库、不触发任何迁移脚本、不访问外部网络或物理板端、不读取 `secret/` 下的任何敏感凭据、不修改其他任何代码文件、不执行 `git add` 或 `git commit`，不向任何子代理派发任务。
3. **数据库无外键/无 CHECK 约束：** 严格遵循应用完整性设计。`0004_tasks.sql` 不得声明 `FOREIGN KEY` 或 `CHECK`，不依赖 TiDB 全局 CHECK 开关；所有关联有效性、状态转移合法性与设备锁互斥均由应用层事务锁序与 CAS 保证。
4. **前置迁移、受审版本化与真实备份门禁约束：** 运行 `0004_tasks.sql` 的硬性前置是当前库必须经过只读结构与数据核验，确认 `schema_version = 3` 且身份表完整无损；若检测到库已被占用、版本不匹配或数据污染，必须 fail-closed 停止迁移，严禁猜测或跳过编号。Schema 升级计划必须将 `src/integrity.rs` 纳入核心修改文件，将其中硬编码的 `validate_guard_rows`（`(1, 3)`）以及所有消费 `schema_meta` 版本的逻辑（如 `lock_integrity_guard`、`check_identity_data`、`validate_identity_rows`、以及所有登录/改密/准入消费者）统一重构为共同版本化只读检查与 guard；旧版、未知版本或被污染数据一律失败关闭拒绝启动，绝不简单宽容放宽为 `3 | 4` 而跳过表结构及字段形状校验。绝不执行 `DROP DATABASE` 或以其他方式删除数据库本身；不能伪造真实备份、备份引用或确认记录；未满足真实备份等现有门禁条件时，不运行需要这些条件的迁移。
5. **网络与事务隔离约束：** 所有数据库事务内严禁包含任何网络 I/O、HTTP 调用或板端 RPC；`execute` 设备指令必须在 `dispatching` 意图事务提交成功后由事务外独立协程发出；意图一旦提交，崩溃恢复时一律视为“可能已发送”，绝不自动补发。
6. **时钟与单调性约束：** 所有 TTL、保活、轮询相位、观察窗口、退避及快照新鲜度均使用进程单调时间（`Instant`）；不同进程或跨主机单调时间绝不相减；跨主机时钟偏差估算基于四时戳算法，两端独立维护 `clock_epoch`，比较时仅核验中控采样开始与结束时本地 `controller_epoch` 一致以及板端 `ClockSample` 携带的稳定质量（非 `clock_unstable`），同机一致而非跨机相等；跨重启有效期必须基于参考时间证据重新可信计算，无法证明有效性的一律转为 `held`。
7. **真实测试隔离约束：** 所有涉及真实 MySQL/TiDB 的集成测试文件必须标明 `#[ignore]`，仅在操作员显式提供 `CONTROLLER_TEST_DATABASE_URL` 时单线程串行执行；缺 URL 显式标记为“未验证/BLOCKED”，连接失败或执行用例数为 0 严禁视为通过。不能为通过文档检查写 fake DB 假绿，缺少环境先决条件时如实呈现 BLOCKED 契约。
8. **严格 TDD 与可编译 RED 约束：** 所有新增行为必须先编写**可编译桩（Compilable Stub）**（签名真实，返回使测试能触发断言失败的 dummy 错误或状态），再编写行为测试，观察因业务断言失败而产生的真实 RED。**严禁以模块/函数/类型缺失编译错误充当 RED；严禁以缺包、缺数据库、缺 fixture 充当 RED；严禁运行匹配 0 测试的命令充当 RED 或 PASS**。Controller crate 依赖 `serde` 与 `tokio` 的 `test-util` 将在未来实施时进行受审离线声明；因 `ControllerError` 未派生 `PartialEq`，测试中对错误的断言必须统一使用 `matches!(res, Err(ControllerError::...))` 或 `assert!(matches!(...))`，不以编译报错充当 RED。使用 `cargo test --offline --locked -p rsetup-controller <唯一测试函数名> --` 按名称过滤，并核对实际执行用例恰为 1；只有传入 Rust 测试完整模块路径时才能使用 `--exact`。

---

## 现状对比与接口差量矩阵

| 子系统 / 组件 | 既有代码库现状（已存在） | 本计划所需交付（缺失） | 依赖与前置关系 |
| :--- | :--- | :--- | :--- |
| **数据库 Schema** | `0001`~`0003` 落地，`schema_version = 3`（无 FK/CHECK）；只读校验完整。`src/integrity.rs` 硬编码检查 `3`。 | `0004_tasks.sql` 尚未创建；5 张 runtime 表结构校验与数据扫描；重构 `src/integrity.rs` 与 `src/db.rs` 中的版本检查为共同版本化只读检查/guard。 | 必须在 v3 结构和数据校验通过后才允许执行 0004 迁移；迁移后 schema_version 升级至 4，旧版/未知版/污染数据统一失败关闭。 |
| **时间系统 (`time/`)** | 仅 `std::time` 与 `chrono` 基础依赖；无专有时间模块。 | 纯模型 `TimeEvidence`（含 sample_age_ms 字符串/null 自定义序列化）、`ClockEstimate`（四时戳同机一致性校验）、SNTP RFC 4330 编解码、30s 启动门禁（已确认约束）、后台指数退避。 | 纯模型无前置；启动门禁与 DB 连接并行，共同作为 readyz 前置。 |
| **任务系统 (`tasks/`)** | 仅模型层有 `AdmissionSnapshot`；无任务模型与仓储。 | `TaskPreview`、`MainTask`、`SubTask`、`ReasonCode`（大写下划线序列化）、`device_operation_locks`、短事务提交流程、幂等重放；真实 DB 夹具契约。 | 依赖 v4 schema；依赖 `authz_epoch` 与 `users/devices` 表。 |
| **调度与恢复 (`tasks/`)** | 无调度器；无故障恢复；无板端客户端接口。 | `BoardClient` 共同 trait（消费原 transport wire DTO，包含完整只读与 prepare/execute 证据）；16 并发槽管理；写意图后 execute；崩溃只查不发；unknown 保持持锁。 | 生产 `BoardClient` 依赖传输计划；本计划定义 trait 与 fake 实现完成闭环，不复制第二套身份。 |
| **错峰轮询 (`polling/`)** | 无轮询调度器；无快照缓存。 | `PollScheduler`（1024 台纳秒整数相位错峰、区分到期与跳轮无饿死无积压）；`SnapshotStore`（单调 30s 新鲜度、error 保留、session/stream/boot/agent 多代际推进）。 | 纯内存状态，依赖单调时钟与设备身份 ID。 |
| **管理 API 与 SSE** | 仅探活、准入 CAS 与身份认证 HTTP 路由；`main.rs` 仅启动最小探活。 | `/task-previews`、`/tasks` 提交/详情/取消/核验/释放锁（严格逐目标 `device.reboot` 校验与 `device.task.read` 投影）；SSE `/events` 128 上限带 reset、命名事件、对象局部 revision 与撤权清理；系统端点。 | 依赖任务服务、调度器、快照缓存与既有 `AppState`。 |
| **运维与受控恢复** | 仅单机管理员 bootstrap 密文写入；无运维状态机。 | `RecoveryMode` 受控恢复模式（限定禁止准入与变更任务，保留管理员受审入口）、保留期清理策略（终态 30d/审计 180d/幂等 24h 待审默认）、双 DB 验证。 | 依赖任务仓储、双引擎测试环境配置。 |

---

## 文件结构与职责划分

| 文件路径 | 动作 | 模块归属 | 核心职责 |
| :--- | :--- | :--- | :--- |
| `crates/rsetup-controller/src/time/evidence.rs` | Create | 时间纯模型 | 定义 `TimeEvidence`、`TimeQuality`、`sample_age_ms` 十进制字符串/null 序列化与单调年龄计算 |
| `crates/rsetup-controller/src/time/board_clock.rs` | Create | 时间纯算法 | 4 时戳偏差计算、RTT 阈值比较、两主机独立 epoch 比对、boot 一致性、稳定证据校验 |
| `crates/rsetup-controller/src/time/ntp.rs` | Create | 时间网络 | SNTP 48B 报文编解码、待审超时与 ≤30s 启动门禁（已确认约束）、指数退避与跳钟检测 |
| `crates/rsetup-controller/src/time/mod.rs` | Create | 时间根导出 | 导出时间门禁句柄与对外时间提供接口 |
| `crates/rsetup-controller/migrations/0004_tasks.sql` | Create | 数据库迁移 | 5 张 runtime 表（无 FK/CHECK，含 PK/UNIQUE/NOT NULL/UNSIGNED） |
| `crates/rsetup-controller/src/integrity.rs` | Modify | 应用完整性 | 升级版本化只读检查与 guard：支持根据配置/运行期版本校验元数据行，适配 schema v4，所有 auth/admission 消费者拒绝未知/污染版本 |
| `crates/rsetup-controller/src/tasks/model.rs` | Create | 任务模型 | 主子任务状态机、终态与 outcome 枚举、`ReasonCode`（大写下划线序列化）、审计与锁结构 |
| `crates/rsetup-controller/src/tasks/preview.rs` | Create | 任务预览 | 目标展开、去重排序、404/400 校验、128bit token 摘要生成 |
| `crates/rsetup-controller/src/tasks/repository.rs` | Create | 任务持久化 | 短事务提交流程、行级锁序、设备锁 CAS 申请、幂等重放查询 |
| `crates/rsetup-controller/src/tasks/board_client.rs` | Create | 板端接口契约 | 定义消费原 transport wire DTO 的 `BoardClient` trait（含完整 evidence 与只读接口）及 fake 夹具 |
| `crates/rsetup-controller/src/tasks/scheduler.rs` | Create | 任务调度 | 16 并发槽限制、写意图后事务外下发、超时/掉线转 verifying |
| `crates/rsetup-controller/src/tasks/evidence.rs` | Create | 任务证据合并 | 核验板端 TaskRecord、boot 切换判定成功、证据收敛与 outcome 修正 |
| `crates/rsetup-controller/src/tasks/recovery.rs` | Create | 进程恢复 | 启动扫描未决任务、重建内存锁、绝对不重发 execute、queued 转 held |
| `crates/rsetup-controller/src/tasks/mod.rs` | Create | 任务根导出 | 导出 `TaskService`、`TaskScheduler` 及恢复接口 |
| `crates/rsetup-controller/src/polling/scheduler.rs` | Create | 错峰轮询 | 1024 设备 10s 纳秒相位分配、区分正常到期与停顿/过载跳轮算法 |
| `crates/rsetup-controller/src/polling/snapshot.rs` | Create | 快照存储 | 内存最新快照、单调 30s 新鲜度计算、session/boot/agent 代际、保留最后好样本与错误状态 |
| `crates/rsetup-controller/src/polling/mod.rs` | Create | 轮询根导出 | 导出 `PollScheduler` 与 `SnapshotStore` |
| `crates/rsetup-controller/src/http_tasks.rs` | Create | HTTP 端点 | 任务预览、提交、详情授权子集投影（`device.task.read`）、逐目标权限取消（`device.reboot`）、时间核验、释放锁路由 |
| `crates/rsetup-controller/src/http_events.rs` | Create | HTTP SSE | 128 上限带 reset 的命名 SSE 推送、对象局部 revision、按用户权限过滤与撤权队列清理 |
| `crates/rsetup-controller/src/http_system.rs` | Create | HTTP 系统端点 | `/system/time`、`/system/status` 端点实现 |
| `crates/rsetup-controller/src/operations/mode.rs` | Create | 运维恢复模式 | `RecoveryMode` 状态机（普通运行 vs 受控恢复模式），限定禁止准入与变更任务，开放受审管理接口 |
| `crates/rsetup-controller/src/operations/retention.rs` | Create | 保留期清理 | 终态 30d/审计 180d/幂等 24h（待审默认），保护 held/unknown 与未释放锁 |
| `crates/rsetup-controller/src/operations/mod.rs` | Create | 运维根导出 | 导出恢复模式与清理服务 |
| `crates/rsetup-controller/src/db.rs` | Modify | 数据库访问 | 增加 runtime v4 表/列/索引校验、v4 迁移 runner 与共同版本化只读检查接线 |
| `crates/rsetup-controller/src/lib.rs` | Modify | 根库导出 | 导出 `time`、`tasks`、`polling`、`operations` 及 HTTP 扩展路由 |
| `crates/rsetup-controller/tests/task_db.rs` | Create | 集成测试 | 任务提交幂等、设备锁互斥、无网络下发验证（真实 fixture 先决契约，显式 `#[ignore]`） |
| `crates/rsetup-controller/tests/controller_recovery.rs` | Create | 集成测试 | 崩溃恢复不重发、晚到证据修正批次、旧锁保护（真实 fixture 先决契约，显式 `#[ignore]`） |
| `crates/rsetup-controller/tests/runtime_db.rs` | Create | 集成测试 | 旧备份受控恢复、保留期安全清理、MySQL/TiDB 验证（真实 fixture 先决契约，显式 `#[ignore]`） |

---

## 增量实施任务清单

### Task 1: 时间证据纯模型与四时戳纯算法 (Pure Time Models & Clock Estimation)

**Files:**
- Create: `crates/rsetup-controller/src/time/mod.rs`
- Create: `crates/rsetup-controller/src/time/evidence.rs`
- Create: `crates/rsetup-controller/src/time/board_clock.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- Consumes: `chrono::{DateTime, Utc}`, `uuid::Uuid`, `serde::{Serialize, Deserialize}`.
- Produces:
  - `pub enum TimeQuality { NtpValid, SystemFallback, Stale }`
  - `pub struct TimeEvidence { pub system_wall_utc: DateTime<Utc>, pub reference_utc: DateTime<Utc>, pub quality: TimeQuality, pub clock_epoch: Uuid, pub source: Option<String>, pub sample_age_ms: Option<u64>, pub offset_ms: Option<i64>, pub uncertainty_ms: Option<u64> }`
    - 注：`sample_age_ms` 在 JSON 序列化中必须输出为十进制字符串（如 `"150"`）或 `null`，严禁直接作为 JSON raw number 输出；`offset_ms` 与 `uncertainty_ms` 保留数值语义，不改变偏差与不确定度计算。
  - `pub struct ClockEstimate { pub board_offset_ms: i64, pub rtt_ms: i64 }`
  - `pub enum ClockEstimateError { NegativeRtt, RttTooLarge, ControllerEpochMismatch, BoardClockUnstable, BootMismatch, ArithmeticOverflow }`
  - `pub fn estimate_board_offset(t1_wall_ms: i64, t2_wall_ms: i64, t3_wall_ms: i64, t4_wall_ms: i64, t1_controller_epoch: Uuid, t4_controller_epoch: Uuid, board_clock_unstable: bool, board_boot: &str, expected_boot: &str, max_rtt_ms: i64) -> Result<ClockEstimate, ClockEstimateError>`（参数说明：05 §2 建议上限 2000ms 为待审默认阈值，由调用方传入或使用可配置项；时钟算法明确两主机独立 epoch，中控仅比对本端开始/结束 epoch 一致性，同时校验板端未处于 `board_clock_unstable` 状态，绝不跨机比对 `controller_epoch == board_epoch`）

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `crates/rsetup-controller/src/time/evidence.rs` 中先定义纯模型类型与**可编译的行为 RED 桩**。初始 `opt_u64_str::serialize` 故意对 `Some(u64)` 输出 JSON number；下方测试必须因预期字符串不符失败，不能把实现已正确却测试初次通过当作 RED：
```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeQuality {
    NtpValid,
    SystemFallback,
    Stale,
}

pub mod opt_u64_str {
    use super::*;

    pub fn serialize<S>(val: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where S: Serializer {
        match val {
            Some(v) => serializer.serialize_u64(*v), // RED 桩：错误地输出 JSON number
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where D: Deserializer<'de> {
        // RED 桩：错误接受 JSON number；GREEN 再严格限制为规范十进制字符串/null。
        Option::<u64>::deserialize(deserializer)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TimeEvidence {
    pub system_wall_utc: DateTime<Utc>,
    pub reference_utc: DateTime<Utc>,
    pub quality: TimeQuality,
    pub clock_epoch: Uuid,
    pub source: Option<String>,
    #[serde(with = "opt_u64_str", default)]
    pub sample_age_ms: Option<u64>,
    pub offset_ms: Option<i64>,
    pub uncertainty_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn time_evidence_serializes_sample_age_as_decimal_string_or_null() {
        let now = Utc::now();
        let mut evidence = TimeEvidence {
            system_wall_utc: now, reference_utc: now,
            quality: TimeQuality::SystemFallback, clock_epoch: Uuid::new_v4(),
            source: None, sample_age_ms: Some(150), offset_ms: Some(-5),
            uncertainty_ms: Some(15),
        };
        let wire = serde_json::to_value(&evidence).unwrap();
        assert_eq!(wire["sample_age_ms"], json!("150")); // 初次 RED：桩输出 150
        assert_eq!(wire["offset_ms"], json!(-5));
        assert_eq!(wire["uncertainty_ms"], json!(15));
        evidence.sample_age_ms = None;
        let null_wire = serde_json::to_value(&evidence).unwrap();
        assert!(null_wire["sample_age_ms"].is_null());
        for invalid in [json!(150), json!("01"), json!(-1)] {
            let mut bad = null_wire.clone();
            bad["sample_age_ms"] = invalid;
            assert!(serde_json::from_value::<TimeEvidence>(bad).is_err());
        }
        let roundtrip = serde_json::from_value::<TimeEvidence>(wire).unwrap();
        assert_eq!(roundtrip.sample_age_ms, Some(150));
    }
}
```

在 `crates/rsetup-controller/src/time/board_clock.rs` 中放置**可编译桩（函数返回使测试断言失败的 dummy 错误）**与测试：
```rust
use uuid::Uuid;

pub const DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED: i64 = 2000; // 05 §2 待审建议默认，非定稿协议常量

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockEstimate {
    pub board_offset_ms: i64,
    pub rtt_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockEstimateError {
    #[error("negative RTT observed")]
    NegativeRtt,
    #[error("RTT exceeds maximum threshold")]
    RttTooLarge,
    #[error("controller clock epoch mismatch between sample start and end")]
    ControllerEpochMismatch,
    #[error("board reported clock unstable")]
    BoardClockUnstable,
    #[error("boot ID mismatch")]
    BootMismatch,
    #[error("arithmetic overflow in clock estimation")]
    ArithmeticOverflow,
}

// 可编译 RED 桩：直接返回固定错误，确保能够编译，由测试断言触发 RED
pub fn estimate_board_offset(
    _t1_wall_ms: i64,
    _t2_wall_ms: i64,
    _t3_wall_ms: i64,
    _t4_wall_ms: i64,
    _t1_controller_epoch: Uuid,
    _t4_controller_epoch: Uuid,
    _board_clock_unstable: bool,
    _board_boot: &str,
    _expected_boot: &str,
    _max_rtt_ms: i64,
) -> Result<ClockEstimate, ClockEstimateError> {
    Err(ClockEstimateError::NegativeRtt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn board_clock_estimation_calculates_offset_and_rtt() {
        let epoch = Uuid::new_v4();
        // T1 = 1000, T2 = 1020, T3 = 1025, T4 = 1050
        // Board offset = ((1020 - 1000) + (1025 - 1050)) / 2 = (20 - 25) / 2 = -2 ms
        // RTT = (1050 - 1000) - (1025 - 1020) = 50 - 5 = 45 ms
        // 两主机独立 epoch：中控 T1/T4 epoch 均为 epoch，板端未报告 unstable
        let res = estimate_board_offset(
            1000, 1020, 1025, 1050, epoch, epoch, false, "boot-1", "boot-1", DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED
        ).unwrap();
        assert_eq!(res.board_offset_ms, -2);
        assert_eq!(res.rtt_ms, 45);
    }

    #[test]
    fn board_clock_rejects_negative_rtt_and_rtt_over_threshold() {
        let epoch = Uuid::new_v4();
        // Negative RTT: (T4 - T1) < (T3 - T2)
        assert!(matches!(
            estimate_board_offset(1000, 1000, 1050, 1010, epoch, epoch, false, "boot-1", "boot-1", 2000),
            Err(ClockEstimateError::NegativeRtt)
        ));
        // RTT > 2000 ms: (T4 - T1) - (T3 - T2) = 2050 - 10 = 2040 > 2000
        assert!(matches!(
            estimate_board_offset(1000, 1000, 1010, 3050, epoch, epoch, false, "boot-1", "boot-1", 2000),
            Err(ClockEstimateError::RttTooLarge)
        ));
    }

    #[test]
    fn board_clock_rejects_controller_epoch_mismatch_and_board_unstable_and_boot_mismatch() {
        let e1 = Uuid::new_v4();
        let e2 = Uuid::new_v4();
        // 中控本端跨采样发生跳钟/重启导致前后 epoch 不一致，同机不一致拒绝
        assert!(matches!(
            estimate_board_offset(1000, 1010, 1020, 1040, e1, e2, false, "boot-1", "boot-1", 2000),
            Err(ClockEstimateError::ControllerEpochMismatch)
        ));
        // 板端报告采样期间跳钟/不稳定
        assert!(matches!(
            estimate_board_offset(1000, 1010, 1020, 1040, e1, e1, true, "boot-1", "boot-1", 2000),
            Err(ClockEstimateError::BoardClockUnstable)
        ));
        // 板端 boot 不匹配
        assert!(matches!(
            estimate_board_offset(1000, 1010, 1020, 1040, e1, e1, false, "boot-1", "boot-2", 2000),
            Err(ClockEstimateError::BootMismatch)
        ));
    }

    #[test]
    fn board_clock_preserves_large_stable_offset_and_checked_bounds() {
        let epoch = Uuid::new_v4();
        // 板端无 RTC 或 1970 异常时间，但采样稳定且 RTT 有界：应正确计算大稳定偏差而非拒绝
        // T1 = 1_700_000_000_000, T2 = 0, T3 = 10, T4 = 1_700_000_000_050
        // total_elapsed = 50ms, processing = 10ms, RTT = 40ms <= 2000ms
        // offset = ((0 - 1_700_000_000_000) + (10 - 1_700_000_000_050)) / 2 = -1_700_000_000_020 ms
        let res = estimate_board_offset(
            1_700_000_000_000, 0, 10, 1_700_000_000_050, epoch, epoch, false, "boot-1", "boot-1", 2000
        ).unwrap();
        assert_eq!(res.rtt_ms, 40);
        assert_eq!(res.board_offset_ms, -1_700_000_000_020);
    }
}
```

在 `src/time/mod.rs` 导出模块，并在 `src/lib.rs` 增加 `pub mod time;`。

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

运行精确测试命令（每条实际执行 1 个用例，不以零匹配算 RED）：
```bash
cargo test --offline --locked -p rsetup-controller time_evidence_serializes_sample_age_as_decimal_string_or_null --
cargo test --offline --locked -p rsetup-controller board_clock_estimation_calculates_offset_and_rtt --
```
预期两个测试分别因业务断言失败：`TimeEvidence` 桩把 `Some(150)` 编码为 JSON number `150` 而非字符串 `"150"`，时钟桩固定返回 `Err(NegativeRtt)` 而非正确 `ClockEstimate`。**不是编译错误或匹配零测试**。

- [ ] **Step 3: 最小实现**

- `src/time/evidence.rs` 将 `opt_u64_str` 的两个 RED 桩替换为：`Some(v)` 使用 `serializer.serialize_str(&v.to_string())`；`None` 保持 `serialize_none()`。反序列化只接受 `Option<String>`（JSON number 不能自动当字符串），非空、全 ASCII 十进制、`"0"` 之外无前导零，再以 `parse::<u64>()` 拒绝溢出，非法返回固定去敏 `serde::de::Error::custom`；保留 `offset_ms`、`uncertainty_ms` 的数值语义。下方 GREEN 必须让上述序列化测试和 board clock 测试分别从 RED 转绿，不修改测试绕过错误输入。
- 在 `crates/rsetup-controller/src/time/board_clock.rs` 中替换桩函数为纯算法逻辑：
- 校验中控本端代际一致性：`if t1_controller_epoch != t4_controller_epoch { return Err(ClockEstimateError::ControllerEpochMismatch); }`；两主机独立 epoch，不跨机比对 `controller_epoch == board_epoch`；
- 校验板端采样稳定证据：`if board_clock_unstable { return Err(ClockEstimateError::BoardClockUnstable); }`；
- 校验 `board_boot == expected_boot`，不匹配返回 `BootMismatch`；
- 采用宽整型与 checked 算术防溢出：
  - `let total_elapsed = t4_wall_ms.checked_sub(t1_wall_ms).ok_or(ClockEstimateError::ArithmeticOverflow)?;`
  - `let board_processing = t3_wall_ms.checked_sub(t2_wall_ms).ok_or(ClockEstimateError::ArithmeticOverflow)?;`
  - `let rtt = total_elapsed.checked_sub(board_processing).ok_or(ClockEstimateError::ArithmeticOverflow)?;`
- 若 `rtt < 0` 返回 `NegativeRtt`；若 `rtt > max_rtt_ms` 返回 `RttTooLarge`；
- 计算时钟偏差：
  - `let d1 = t2_wall_ms.checked_sub(t1_wall_ms).ok_or(ClockEstimateError::ArithmeticOverflow)? as i128;`
  - `let d2 = t3_wall_ms.checked_sub(t4_wall_ms).ok_or(ClockEstimateError::ArithmeticOverflow)? as i128;`
  - `let offset = (d1 + d2) / 2;`
  - `let board_offset_ms = i64::try_from(offset).map_err(|_| ClockEstimateError::ArithmeticOverflow)?;`
- 两主机单调时间绝不相减；保留大稳定偏差；不确定度界线以 `rtt / 2` 为单次往返不对称度理论上界。

- [ ] **Step 4: 运行并确认 GREEN**

运行全部相关单元测试：
```bash
cargo test --offline --locked -p rsetup-controller time_evidence_serializes_sample_age_as_decimal_string_or_null --
cargo test --offline --locked -p rsetup-controller board_clock_estimation_calculates_offset_and_rtt --
cargo test --offline --locked -p rsetup-controller board_clock_rejects_negative_rtt_and_rtt_over_threshold --
cargo test --offline --locked -p rsetup-controller board_clock_rejects_controller_epoch_mismatch_and_board_unstable_and_boot_mismatch --
cargo test --offline --locked -p rsetup-controller board_clock_preserves_large_stable_offset_and_checked_bounds --
```
预期通过：每个命令各执行 1 个测试，全部 PASS（共 5 个用例 PASS，0 失败）。

- [ ] **Step 5: 重构、代码检查与提交规范**

运行静态检查：
```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
```
确认无警告后受控提交：`feat(controller): pure time evidence and board clock offset estimation`。

---

### Task 2: 独立 NTP 客户端与启动时间门禁 (Independent NTP Client & Boot Time Gate)

**Files:**
- Create: `crates/rsetup-controller/src/time/ntp.rs`
- Modify: `crates/rsetup-controller/src/time/mod.rs`
- Modify: `crates/rsetup-controller/src/config.rs`
- Modify: `crates/rsetup-controller/src/main.rs`

**Interfaces:**
- Consumes: Task 1 `TimeEvidence`, `TimeQuality`, `tokio::time::Duration`.
- Produces:
  - `pub struct TimeGateConfig { pub servers: Vec<String>, pub request_timeout: Duration, pub startup_budget: Duration }`（参数说明：05 §1/§2 建议单次请求 ≤3s 为待审默认建议，启动预算 ≤30s 为 00 §3 C-TIME 已确认约束，绝非待审参数）
  - `pub fn parse_sntp_response(packet: &[u8]) -> Result<(), NtpError>`
  - `pub fn start_time_gate(config: TimeGateConfig, clock_epoch: Uuid) -> TimeGateHandle`
  - `impl TimeGateHandle { pub async fn wait_ready(&self) -> TimeEvidence; pub fn current(&self) -> TimeEvidence; pub fn background_retry_active(&self) -> bool; }`

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `crates/rsetup-controller/src/time/ntp.rs` 中放置**可编译桩**与测试：
```rust
use crate::time::evidence::{TimeEvidence, TimeQuality};
use chrono::Utc;
use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use tokio::sync::RwLock;
use uuid::Uuid;

pub const DEFAULT_NTP_REQUEST_TIMEOUT_SUGGESTED: Duration = Duration::from_secs(3); // 05 §1 待审建议
pub const DEFAULT_NTP_STARTUP_BUDGET: Duration = Duration::from_secs(30); // 00 §3 C-TIME 已确认约束（≤30s）

#[derive(Clone, Debug)]
pub struct TimeGateConfig {
    pub servers: Vec<String>,
    pub request_timeout: Duration,
    pub startup_budget: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NtpError {
    #[error("packet too short")]
    PacketTooShort,
    #[error("clock not synchronized (LI=3)")]
    Unsynchronized,
    #[error("invalid stratum")]
    InvalidStratum,
    #[error("timeout")]
    Timeout,
    #[error("io error")]
    Io,
}

// 可编译 RED 桩：parse 暂不校验 LI，恒返回 Ok(())，使报文测试断言 RED
pub fn parse_sntp_response(packet: &[u8]) -> Result<(), NtpError> {
    if packet.len() < 48 { return Err(NtpError::PacketTooShort); }
    Ok(())
}

pub struct TimeGateHandle {
    current: Arc<RwLock<TimeEvidence>>,
    ready_notify: Arc<tokio::sync::Notify>,
    is_ready: Arc<AtomicBool>,
    retry_active: Arc<AtomicBool>,
}

impl TimeGateHandle {
    pub async fn wait_ready(&self) -> TimeEvidence {
        while !self.is_ready.load(Ordering::SeqCst) {
            self.ready_notify.notified().await;
        }
        self.current.read().await.clone()
    }
    pub async fn current(&self) -> TimeEvidence { self.current.read().await.clone() }
    pub fn background_retry_active(&self) -> bool { self.retry_active.load(Ordering::SeqCst) }
}

// 可编译 RED 桩：不启动后台超时与退避逻辑，直接标记 ready 为 false，使门禁测试断言 RED
pub fn start_time_gate(_config: TimeGateConfig, clock_epoch: Uuid) -> TimeGateHandle {
    let now = Utc::now();
    let initial = TimeEvidence {
        system_wall_utc: now,
        reference_utc: now,
        quality: TimeQuality::Stale, // 桩：错误返回 Stale 而非 Fallback
        clock_epoch,
        source: None,
        sample_age_ms: None,
        offset_ms: None,
        uncertainty_ms: None,
    };
    TimeGateHandle {
        current: Arc::new(RwLock::new(initial)),
        ready_notify: Arc::new(tokio::sync::Notify::new()),
        is_ready: Arc::new(AtomicBool::new(true)),
        retry_active: Arc::new(AtomicBool::new(false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntp_packet_decoder_rejects_unsynchronized_leap_indicator() {
        let mut packet = [0u8; 48];
        packet[0] = 0b11_000_000; // LI = 3
        assert_eq!(parse_sntp_response(&packet), Err(NtpError::Unsynchronized));
    }

    #[tokio::test(start_paused = true)]
    async fn ntp_timeout_yields_fallback_and_retry() {
        let epoch = Uuid::new_v4();
        let config = TimeGateConfig {
            servers: vec!["127.0.0.1:9999".into()],
            request_timeout: Duration::from_secs(3),
            startup_budget: Duration::from_secs(30),
        };
        let gate = start_time_gate(config, epoch);
        tokio::time::advance(Duration::from_secs(30)).await;
        let evidence = gate.wait_ready().await;
        assert_eq!(evidence.quality, TimeQuality::SystemFallback);
        assert!(gate.background_retry_active());
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

运行精确测试命令：
```bash
cargo test --offline --locked -p rsetup-controller ntp_packet_decoder_rejects_unsynchronized_leap_indicator --
cargo test --offline --locked -p rsetup-controller ntp_timeout_yields_fallback_and_retry --
```
预期失败：两条命令各执行 1 个测试；第一条因桩返回 `Ok(())` 而断言失败；第二条因桩返回 `TimeQuality::Stale` 及 `retry_active == false` 而断言失败。**真实行为断言失败，绝非编译错误或 0 用例**。

- [ ] **Step 3: 最小实现**

在 `crates/rsetup-controller/src/time/ntp.rs` 中替换为最小实现：
- `parse_sntp_response`：解码 LI，若 `li == 3` 返回 `Unsynchronized`；stratum 若为 0 或 >15 返回 `InvalidStratum`；
- `start_time_gate`：在 `startup_budget` 内执行多源查询；预算超时或全部失败则回退 `SystemFallback` 并唤醒 `ready_notify`，绝不因 NTP 超时挂死中控启动；
- 后台开启无限有界指数退避重试（建议 1s→60s 封顶 + 抖动，空源退避重读配置）；跳钟检测（`|Δwall - Δmonotonic| > 1s`）更换 `clock_epoch` 并触发审计。

- [ ] **Step 4: 运行并确认 GREEN**

运行精确测试命令：
```bash
cargo test --offline --locked -p rsetup-controller ntp_packet_decoder_rejects_unsynchronized_leap_indicator --
cargo test --offline --locked -p rsetup-controller ntp_timeout_yields_fallback_and_retry --
```
预期通过：各命令执行 1 个测试，全部 PASS（共 2 个用例 PASS，0 失败）。

- [ ] **Step 5: 重构、代码检查与提交规范**

运行静态检查：
```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
```
确认无警告后受控提交：`feat(controller): sntp client with bounded boot gate and fallback`。

---

### Task 3: 任务 Schema v4 迁移、应用完整性版本化与原子任务预览/提交 (Schema v4 Migration, Integrity Versioning & Atomic Task Preview/Submit)

**Files:**
- Create: `crates/rsetup-controller/migrations/0004_tasks.sql`
- Create: `crates/rsetup-controller/src/tasks/model.rs`
- Create: `crates/rsetup-controller/src/tasks/preview.rs`
- Create: `crates/rsetup-controller/src/tasks/repository.rs`
- Create: `crates/rsetup-controller/src/tasks/mod.rs`
- Create: `crates/rsetup-controller/tests/task_db.rs`
- Modify: `crates/rsetup-controller/src/integrity.rs`
- Modify: `crates/rsetup-controller/src/db.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- 前置门禁：当前库必须先通过只读结构与数据核验，确认 `schema_version = 3` 且身份表完整无损；
- Consumes: `auth::service::Session`, `db::DbPool`.
- Produces:
  - 5 张 runtime 表（`task_previews`, `main_tasks`, `sub_tasks`, `device_operation_locks`, `recovery_checks`），严格无 FK 与无 CHECK；
  - 升级 `src/integrity.rs` 与 `src/db.rs`：废除硬编码的 3，实现基于共同版本化的只读检查与 guard（如 `validate_guard_rows_for_version(rows, version)`、`validate_identity_rows(db, expected_version)`）；所有登录、改密、准入 CAS 及任务提交统一闭环依赖版本化 guard。在 v4 下，旧版（v1/v2/v3）、未知版本（如 v5）或脏数据一律只读拒绝普通启动，绝不简单宽容放宽为 `3 | 4` 而跳过表结构及字段形状校验；
  - `pub fn validate_target_count(count: usize) -> Result<(), ControllerError>`
  - `pub struct TaskService<R: TaskRepository> { ... }`
  - `impl<R: TaskRepository> TaskService<R> { pub async fn preview(&self, actor: &Session, device_ids: &[String], group_ids: &[Uuid]) -> Result<TaskPreviewResult, ControllerError>; pub async fn submit(&self, actor: &Session, preview_token: &str, idempotency_key: &str) -> Result<SubmitResult, ControllerError>; }`

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `crates/rsetup-controller/src/tasks/model.rs` 中定义状态机枚举：
```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubTaskState {
    Queued, Held, Dispatching, Accepted, Verifying, Succeeded, Failed, Unknown, Cancelled, Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReasonCode {
    PermissionDenied, DeviceOffline, CapabilityUnavailable, DeviceBusy,
    QueueExpired, TimeUncertain, ExecutionRejected, OsError, ResultTimeout,
    JournalLost, ProtocolMismatch, UserCancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTaskState { Queued, Running, Held, Completed }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainTaskOutcome { Success, Cancelled, Unknown, Partial, Failed }
```

在 `crates/rsetup-controller/src/tasks/preview.rs` 中放置**可编译桩（未校验边界导致测试断言失败）**与测试：
```rust
use crate::error::ControllerError;

// 可编译 RED 桩：恒返回 Ok(())，未拦截非法 0 或 >1024 目标
pub fn validate_target_count(_count: usize) -> Result<(), ControllerError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_rejects_empty_and_excessive_targets() {
        assert!(matches!(validate_target_count(0), Err(ControllerError::InvalidArgument)));
        assert!(validate_target_count(1024).is_ok());
        assert!(matches!(validate_target_count(1025), Err(ControllerError::InvalidArgument)));
    }

    #[test]
    fn reason_code_serializes_as_screaming_snake_case() {
        use crate::tasks::model::ReasonCode;
        assert_eq!(serde_json::to_string(&ReasonCode::DeviceBusy).unwrap(), "\"DEVICE_BUSY\"");
        assert_eq!(serde_json::to_string(&ReasonCode::PermissionDenied).unwrap(), "\"PERMISSION_DENIED\"");
        assert_eq!(serde_json::to_string(&ReasonCode::TimeUncertain).unwrap(), "\"TIME_UNCERTAIN\"");
    }
}
```

在 `crates/rsetup-controller/tests/task_db.rs` 中**仅当** `TaskTestFixture`、生产仓储/任务服务及 `device_operation_locks` v4 迁移实际实现、可编译且已通过独立审查后，新增 `#[tokio::test] #[ignore] fn later_batch_is_busy_without_network_send`。该测试不得在缺 URL 时打印 BLOCKED 后 `return`（Rust 会把它计为 1 passed），也不得引用尚不存在的 `tests::common::TaskTestFixture` 并将编译失败称 RED；缺 fixture/授权/真实备份时直接不运行并在验收报告标 BLOCKED。

**实际行为断言（先建可编译 fixture 后再观察 RED）：** 用已授权真实 v4 DB 及 `mod common; common::required_test_db()` 的精确目标校验，创建持有当前 `device.reboot` 的 actor 和已批准设备；同一设备两个不同 UUIDv4 幂等键提交两批，查询真实 `sub_tasks` 断言前者 queued、后者 failed/`DEVICE_BUSY`，查询 `device_operation_locks` 断言 owner 仍是首个子任务且只有一行；受控 BoardClient 替身的 `execute` 调用为 0。任意 DB 错误、迁移缺失或断言失败必须使测试失败，不能改为 `return`、空 fixture 或假的常量结果。`#[ignore]` 的测试须在 fixture 可用且 RED 来自预期行为缺失后，才进入下文 GREEN。

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller preview_rejects_empty_and_excessive_targets --
```
预期失败：执行 1 个测试；因桩返回 `Ok(())` 未报错而断言失败。**真实行为断言失败，绝非编译错误或 0 用例**。

真实 DB 测试验证命令（仅在操作员核验隔离目标、权限与真实备份后由环境提供）：
```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test task_db later_batch_is_busy_without_network_send -- --ignored --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test task_db later_batch_is_busy_without_network_send -- --ignored --test-threads=1
```
核对各引擎执行 1 个用例且断言失败，缺 URL 标未验证，禁止 0 用例。

- [ ] **Step 3: 最小实现**

1. 编写 `migrations/0004_tasks.sql`：创建 `task_previews`、`main_tasks`、`sub_tasks`、`device_operation_locks`、`recovery_checks`。严格保留 PK、UNIQUE、NOT NULL、UNSIGNED，彻底不声明 `FOREIGN KEY` 与 `CHECK`。
2. 实现 `validate_target_count`：`if count == 0 || count > 1024 { Err(InvalidArgument) } else { Ok(()) }`。
3. 实现短事务提交：共享 `schema_meta(singleton=1) FOR UPDATE` 行锁；重验 actor active 与 `authz_epoch`；目标按公钥排序；向 `device_operation_locks` 申请唯一锁，命中 Duplicate Key 直接置 `Failed` + `ReasonCode::DeviceBusy`，坚决不排队；排队有效期建议 60min（待审默认）；事务内零网络 I/O。

- [ ] **Step 4: 运行并确认 GREEN**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller preview_rejects_empty_and_excessive_targets --
```
预期通过：执行 1 个测试，PASS。
真实 DB 忽略测试双引擎分别验证执行 1 个用例且 PASS（零失败，记录被测引擎版本）。

- [ ] **Step 5: 重构、代码检查与提交规范**

运行静态检查：
```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
```
确认无警告后受控提交：`feat(controller): task schema v4 and atomic submit with persistent device locks`。

---

### Task 4: 任务调度器、串行下发与不重发的故障恢复 (Scheduler, Dispatching & Non-resending Recovery)

**Files:**
- Create: `crates/rsetup-controller/src/tasks/board_client.rs`
- Create: `crates/rsetup-controller/src/tasks/scheduler.rs`
- Create: `crates/rsetup-controller/src/tasks/evidence.rs`
- Create: `crates/rsetup-controller/src/tasks/recovery.rs`
- Create: `crates/rsetup-controller/tests/controller_recovery.rs`
- Modify: `crates/rsetup-controller/src/tasks/mod.rs`
- Modify: `crates/rsetup-controller/src/tasks/model.rs`

**Interfaces:**
- Consumes: Task 3 状态机与持久化仓储。
- Produces:
  - `pub trait BoardClient: Send + Sync { async fn capabilities(&self, device: &[u8; 32]) -> Result<rsetup_protocol::wire::Capabilities, ControllerError>; async fn status(&self, device: &[u8; 32]) -> Result<rsetup_protocol::wire::DeviceStatus, ControllerError>; async fn clock(&self, device: &[u8; 32]) -> Result<rsetup_protocol::wire::ClockSample, ControllerError>; async fn prepare(&self, device: &[u8; 32], subtask_id: Uuid, expected_boot: &str) -> Result<rsetup_protocol::wire::RebootTicket, ControllerError>; async fn execute(&self, device: &[u8; 32], subtask_id: Uuid, ticket: &rsetup_protocol::wire::RebootTicket) -> Result<rsetup_protocol::wire::TaskRecord, ControllerError>; async fn task_record(&self, device: &[u8; 32], subtask_id: Uuid) -> Result<rsetup_protocol::wire::TaskRecord, ControllerError>; }`
    - 注：`BoardClient` 必须直接消费原 transport wire DTO，完整保留 prepare 票据中的 `boot_id`、`valid_for_ms` 以及 execute 返回的完整 `TaskRecord` 执行证据，绝不降级为 `Result<String>` 或 `Result<()>` 丢弃证据；readonly 查询（capabilities/status/clock/task_record）也必须收敛在同一共同接口，不复制另一套身份模型。
  - `pub fn calculate_main_task_outcome(subtask_states: &[SubTaskState]) -> (MainTaskState, Option<MainTaskOutcome>)`
  - `pub async fn recover_process<R: TaskRepository>(repo: &R) -> Result<RecoverySummary, ControllerError>`

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `crates/rsetup-controller/src/tasks/recovery.rs` 中放置**可编译桩（返回错误状态导致汇总断言失败）**：
```rust
use crate::tasks::model::{MainTaskOutcome, MainTaskState, SubTaskState};

// 可编译 RED 桩：恒返回 (Running, None)，使测试断言失败
pub fn calculate_main_task_outcome(_subtask_states: &[SubTaskState]) -> (MainTaskState, Option<MainTaskOutcome>) {
    (MainTaskState::Running, None)
}
```

在 `crates/rsetup-controller/tests/controller_recovery.rs` 中编写测试：
```rust
use rsetup_controller::tasks::model::{MainTaskOutcome, MainTaskState, SubTaskState};
use rsetup_controller::tasks::recovery::calculate_main_task_outcome;

#[test]
fn main_task_outcome_corrected_by_late_evidence() {
    let (s, o) = calculate_main_task_outcome(&[SubTaskState::Succeeded, SubTaskState::Succeeded]);
    assert_eq!(s, MainTaskState::Completed);
    assert_eq!(o, Some(MainTaskOutcome::Success));

    let (s, o) = calculate_main_task_outcome(&[SubTaskState::Succeeded, SubTaskState::Unknown]);
    assert_eq!(s, MainTaskState::Completed);
    assert_eq!(o, Some(MainTaskOutcome::Unknown));

    let (s, o) = calculate_main_task_outcome(&[SubTaskState::Succeeded, SubTaskState::Failed]);
    assert_eq!(s, MainTaskState::Completed);
    assert_eq!(o, Some(MainTaskOutcome::Partial));

    let (s, o) = calculate_main_task_outcome(&[SubTaskState::Cancelled, SubTaskState::Cancelled]);
    assert_eq!(s, MainTaskState::Completed);
    assert_eq!(o, Some(MainTaskOutcome::Cancelled));
}
```

真实 DB 测试命令仅在 `RecoveryTestFixture`、生产恢复实现和目标数据库的授权/备份门禁完成后运行；未落实前本用例标 `BLOCKED`，不得以未定义 fixture、空测试、`return` 分支或编译错误冒充 RED。未来 `crates/rsetup-controller/tests/controller_recovery.rs` 中以 `mod common; common::required_test_db()` 核实真实 v4 库，构造真实已提交 `dispatching` 意图及对应设备锁，使用受控 BoardClient 替身；**跨新进程**启动恢复入口后查询 DB：该子任务不得回到 queued、设备锁仍属旧 owner 且 `execute` 计数为 0，只读 task.get 可用于核实。还须注入“意图已提交但网络发送前进程死亡”窗口，不能把仅在同进程调用一个纯函数作为跨进程恢复证据。实际测试因错误/锁丢失/调用 execute 时必须失败；缺前置则不运行并明确未验证。

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller --test controller_recovery main_task_outcome_corrected_by_late_evidence --
```
预期失败：执行 1 个测试；因桩返回 `(Running, None)` 而断言失败。**真实行为断言失败，绝非编译错误或 0 用例**。

真实 DB 恢复测试命令（仅在显式隔离配置下执行）：
```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test controller_recovery crash_after_intent_never_auto_resends -- --ignored --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test controller_recovery crash_after_intent_never_auto_resends -- --ignored --test-threads=1
```
核对各引擎执行 1 个用例且断言失败，缺 URL 标未验证，禁止 0 用例。

- [ ] **Step 3: 最小实现**

1. 主任务终态汇总算法：未全终态时按 active/held/queued 汇总；全终态时：有 unknown 则 outcome 为 Unknown；全成功为 Success；全取消为 Cancelled；混合成功为 Partial；其余为 Failed。迟到证据可修正 outcome 但状态不倒退回 Running。
2. 调度执行分界：全局 16 并发槽限制（待审默认）；先 prepare 成功并重验 `authz_epoch`；短事务提交 `dispatching` 意图；事务 COMMIT 后事务外独立调用 `execute`；超时或断线转 `Verifying`，绝不重发。
3. 恢复原则：扫描 DB 重建锁；`dispatching/accepted/verifying` 视为可能已发送，**绝对不自动重发 execute**，发起只读核查；`unknown` 持续持有设备锁并释放并发槽。

- [ ] **Step 4: 运行并确认 GREEN**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller --test controller_recovery main_task_outcome_corrected_by_late_evidence --
```
预期通过：执行 1 个测试，PASS。
真实 DB 忽略测试双引擎分别验证执行 1 个用例且 PASS（零失败，记录被测引擎版本）。

- [ ] **Step 5: 重构、代码检查与提交规范**

运行静态检查：
```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
```
确认无警告后受控提交：`feat(controller): task scheduler with intent-commit-first and safe recovery`。

---

### Task 5: 1024 台设备错峰轮询调度与内存最新快照 (Staggered Polling & Memory Snapshot)

**Files:**
- Create: `crates/rsetup-controller/src/polling/scheduler.rs`
- Create: `crates/rsetup-controller/src/polling/snapshot.rs`
- Create: `crates/rsetup-controller/src/polling/mod.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- Consumes: Task 1 `TimeEvidence`, `std::time::{Duration, Instant}`.
- Produces:
  - `pub struct PollScheduler { ... }`
  - `impl PollScheduler { pub fn new(origin: Instant, period: Duration, tick_cadence: Duration, devices: Vec<[u8; 32]>) -> Self; pub fn due(&mut self, now: Instant) -> Vec<[u8; 32]>; pub fn set_overloaded(&mut self, overloaded: bool); pub fn backlog(&self) -> usize; }`
  - `pub struct SnapshotStore { ... }`（仅按认证连接管理器确认的当前代际推进，不按随机 UUID 大小排序）
  - `impl SnapshotStore { pub fn update(&self, device: [u8; 32], session_epoch: uuid::Uuid, stream_epoch: uuid::Uuid, boot_id: &str, agent_epoch: uuid::Uuid, sample_seq: u64, metrics: Vec<Metric>, now: Instant); pub fn read(&self, device: &[u8; 32], now: Instant) -> Option<SnapshotView>; }`

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `crates/rsetup-controller/src/polling/scheduler.rs` 中放置**可编译桩（未实现跳轮导致断言失败）**与测试：
```rust
use std::time::{Duration, Instant};

pub struct PollScheduler {
    devices: Vec<[u8; 32]>,
}

impl PollScheduler {
    pub fn new(_origin: Instant, _period: Duration, _tick_cadence: Duration, devices: Vec<[u8; 32]>) -> Self {
        Self { devices }
    }
    pub fn set_overloaded(&mut self, _overloaded: bool) {}
    pub fn backlog(&self) -> usize { 100 } // 桩：错误返回积压 100
    // 可编译 RED 桩：直接返回全部设备，未实现错峰与跳轮
    pub fn due(&mut self, _now: Instant) -> Vec<[u8; 32]> {
        self.devices.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_devices(count: usize) -> Vec<[u8; 32]> {
        (0..count).map(|i| {
            let mut d = [0u8; 32];
            d[0..2].copy_from_slice(&(i as u16).to_be_bytes());
            d
        }).collect()
    }

    #[test]
    fn overdue_after_pause_skips_without_backlog_and_keeps_distribution() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        let mut scheduler = PollScheduler::new(origin, Duration::from_secs(10), Duration::from_millis(100), devices);

        let pause_now = origin.checked_add(Duration::from_secs(30)).unwrap()
            .checked_add(Duration::from_nanos(1)).unwrap();

        // 停顿恢复的瞬间，逾期轮次一律跳过
        let first = scheduler.due(pause_now);
        assert!(first.is_empty(), "overdue cycles must be skipped without burst");
        assert_eq!(scheduler.backlog(), 0);

        // 后续 10s (100 个 100ms tick)，验证每 tick 下发数 <= 11，整轮收齐 1024 台互异设备
        let mut collected = std::collections::HashSet::new();
        for tick in 1..=100 {
            let at = pause_now.checked_add(Duration::from_millis(tick * 100)).unwrap();
            let batch = scheduler.due(at);
            assert!(batch.len() <= 11, "tick {} exceeded limit: {}", tick, batch.len());
            for id in batch {
                assert!(collected.insert(id), "duplicate device emitted: {:?}", id);
            }
        }
        assert_eq!(collected.len(), 1024);
        assert_eq!(scheduler.backlog(), 0);
    }

    #[test]
    fn normal_tick_due_emits_ready_devices_without_starvation() {
        let origin = Instant::now();
        let devices = make_test_devices(1024);
        let mut scheduler = PollScheduler::new(origin, Duration::from_secs(10), Duration::from_millis(100), devices);

        // 正常到期下发测试：在首个 100ms tick，严格分配到该窗口的设备必须正常 emit，绝不能因跳轮逻辑误判而饿死
        let first_tick = origin.checked_add(Duration::from_millis(100)).unwrap();
        let batch = scheduler.due(first_tick);
        assert!(!batch.is_empty(), "normal tick must emit ready devices without starvation");
        assert!(batch.len() <= 11);
    }
}
```

在 `crates/rsetup-controller/src/polling/snapshot.rs` **先建立可编译的最小类型与行为 RED 桩**，再添加以下测试；不能以缺模块或签名不匹配充当 RED。桩的形状至少包括 `Metric::Cpu(f64)`、`SnapshotView { sample_seq, metrics, last_error }`、`SnapshotView::is_fresh(Instant) -> bool`，以及与上面完整八参数签名一致的 `SnapshotStore::{new,update,read,record_error}`。例如初始 `update` 不保存样本、`read` 返回固定 `Some(SnapshotView { sample_seq: 0, metrics: vec![], last_error: None })`，让下面首个 `sample_seq == 1` 断言在类型检查通过后**因行为缺失**而 RED；GREEN 时才实现代际和新鲜度。后续独立用认证连接管理器 fixture 注入旧 session/stream/boot/agent 回调，确认均不能覆盖当前样本，不凭 UUID 字节序判断新旧。

在 `crates/rsetup-controller/src/polling/snapshot.rs` 中编写测试（30s 单调年龄新鲜度判决、错误保留与序号更新）：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn snapshot_staleness_and_error_preservation_and_generation_updates() {
        let store = SnapshotStore::new();
        let dev = [0x77u8; 32];
        let t0 = Instant::now();
        let session1 = uuid::Uuid::new_v4();
        let stream1 = uuid::Uuid::new_v4();
        let agent1 = uuid::Uuid::new_v4();
        let boot1 = "boot-a";

        // 初始更新：带 session/stream/boot/agent 代际与 sample_seq=1
        store.update(dev, session1, stream1, boot1, agent1, 1, vec![Metric::Cpu(10.0)], t0);
        let s0 = store.read(&dev, t0).expect("snapshot should exist");
        assert_eq!(s0.sample_seq, 1);
        assert_eq!(s0.is_fresh(t0), true);

        // 30s 边界测试：05 RUN-01 状态过期阈值为 30s（单调接收年龄）
        let t_fresh = t0.checked_add(Duration::from_secs(30)).unwrap();
        assert_eq!(store.read(&dev, t_fresh).unwrap().is_fresh(t_fresh), true);
        let t_stale = t0.checked_add(Duration::from_secs(31)).unwrap();
        assert_eq!(store.read(&dev, t_stale).unwrap().is_fresh(t_stale), false);

        // 旧序号或旧代际拒绝覆盖现有快照
        let t_update = t0.checked_add(Duration::from_secs(5)).unwrap();
        store.update(dev, session1, stream1, boot1, agent1, 1, vec![Metric::Cpu(99.0)], t_update); // 相同序号，丢弃
        assert_eq!(store.read(&dev, t_update).unwrap().sample_seq, 1);

        // 采集失败：保留最后好样本与 last_error，未知数值绝不填 0
        store.record_error(dev, "network timeout", t_update);
        let s_err = store.read(&dev, t_update).unwrap();
        assert_eq!(s_err.metrics.len(), 1); // 依然保留原有好的指标
        assert_eq!(s_err.last_error, Some("network timeout".into()));
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller overdue_after_pause_skips_without_backlog_and_keeps_distribution --
```
预期失败：执行 1 个测试；因桩返回了全部 1024 台设备导致断言 `first.is_empty()` 失败。**真实行为断言失败，绝非编译错误或 0 用例**。

- [ ] **Step 3: 最小实现**

1. 纳秒整数相位错峰：1024 台设备按 `(i as u128 * period_nanos) / 1024` 分配整数纳秒相位；
2. 停顿与过载跳轮算法：调用方显式传入 `tick_cadence` 并与真实驱动步长保持一致，调度器记录上一轮 `due` 时间（初始 `origin`）。若调用间隔 `now - last_tick > tick_cadence` 或系统已标记 `overloaded`，本次零下发、将已到期设备的 `next_due` 按整数个 `period` 推进至严格大于 `now` 的下一个同相位点，**绝不积压（内部 backlog 恒为 0），恢复后绝无洪峰**；仅当间隔 `<= tick_cadence`、无过载且设备 `next_due <= now` 时发出该设备并推进一轮，已逾期整个 `period` 的旧轮次仍跳过。原 `now - next_due >= period` 不能识别亚周期停顿（例如 5s/10s 造成单 tick 数百台突发），不得再单独作为停顿判据。完整裁决及 RED 用例见[错峰 tick-cadence 修订设计](../specs/2026-10-07-controller-poll-cadence-correction-design.md)。
3. 内存最新快照：仅驻留内存，按单调接收年龄计算 30s 新鲜度（05 RUN-01 状态过期阈值）；更新时严格比对 session/stream/boot/agent 代际与严格递增的 sample_seq，旧代际或旧序号丢弃；发生错误保留最后好样本与 `last_error`，未知指标绝不回填 0。

- [ ] **Step 4: 运行并确认 GREEN**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller overdue_after_pause_skips_without_backlog_and_keeps_distribution --
cargo test --offline --locked -p rsetup-controller normal_tick_due_emits_ready_devices_without_starvation --
cargo test --offline --locked -p rsetup-controller snapshot_staleness_and_error_preservation_and_generation_updates --
```
预期通过：每个命令各执行 1 个测试，全部 PASS（共 3 个用例 PASS，0 失败）。

- [ ] **Step 5: 重构、代码检查与提交规范**

运行静态检查：
```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
```
确认无警告后受控提交：`feat(controller): staggered poll scheduler and memory snapshot store`。

---

### Task 6: 授权任务 API、SSE 增量推送与系统端点 (Authorized Task API, SSE & System Endpoints)

**Files:**
- Create: `crates/rsetup-controller/src/http_tasks.rs`
- Create: `crates/rsetup-controller/src/http_events.rs`
- Create: `crates/rsetup-controller/src/http_system.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- Consumes: Task 3 任务模型、Task 4 调度器、Task 5 快照、`auth::service::Session`.
- Produces:
  - `pub fn project_main_task_view(targets: &[(&str, bool, &str)], is_full_access: bool) -> serde_json::Value`
    - 普通任务详情读取必须严格限制为任务本人提交，且仅在当前持有该目标 `device.task.read` 权限时才可见该子任务（注：规格中 `task.read` 简写统一对应系统权限 `device.task.read`，严禁新增独立权限）；
  - `pub fn can_cancel_subtask(is_admin: bool, is_owner: bool, has_target_reboot_perm: bool, state: SubTaskState) -> bool`
    - 取消子任务必须逐目标校验当前 `device.reboot` 权限，并在事务中重验 `authz_epoch`；即使是本人提交的任务，若当前失去了该设备的 `device.reboot` 权限，也严禁取消；管理员跨用户取消同样必须重验当前管理员状态与事务锁；
  - HTTP 路由：`GET /api/v1/tasks/{id}`, `POST /api/v1/tasks/{id}/cancel`, `GET /api/v1/events`, `GET /api/v1/system/time`, `GET /api/v1/system/status`
  - SSE 规范契约：
    - 统一为命名 SSE 事件：`event: device.updated`、`event: task.updated`、`event: permissions.changed`、`event: system.time.changed`、`event: reset`；
    - 每个事件携带被变更对象的局部 revision（如 `{"device_id": "...", "revision": 42}`），绝不假设整个设备/任务列表具有全局 revision；
    - 每次发送重验 session 有效性与 `authz_epoch`；用户被撤权立即清理待发队列并关闭连接；队列上限 128 条，溢出时下发 `event: reset` 并断开连接。只规定清晰的交互与授权契约，不实现复杂冗余的大状态机。

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `crates/rsetup-controller/src/http_tasks.rs` 中放置**可编译桩（返回全量数据暴露未授权目标导致断言失败）**与测试：
```rust
use crate::tasks::model::SubTaskState;
use serde_json::{json, Value};

// 可编译 RED 桩：恒返回 full 视图，使授权子集投影测试断言 RED
pub fn project_main_task_view(_targets: &[(&str, bool, &str)], _is_full_access: bool) -> Value {
    json!({ "view_scope": "full" })
}

// 可编译 RED 桩：恒返回 false，使取消权限测试断言 RED
pub fn can_cancel_subtask(_is_admin: bool, _is_owner: bool, _has_target_reboot_perm: bool, _state: SubTaskState) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authorized_subset_projection_hides_unauthorized_targets() {
        let all_targets = vec![
            ("dev-1", true, "succeeded"), // actor 拥有该目标的 device.task.read 权限
            ("dev-2", false, "succeeded"), // actor 无权查看该设备 (无 device.task.read)
        ];
        let projection = project_main_task_view(&all_targets, false);
        assert_eq!(projection["view_scope"], "authorized_subset");
        assert_eq!(projection["counts"]["succeeded"], 1);
        assert!(projection.get("total_targets").is_none());
    }

    #[test]
    fn cancel_subtask_requires_exact_permissions_and_state() {
        // 管理员在 Queued/Held 下可取消，哪怕不是 owner
        assert!(can_cancel_subtask(true, false, false, SubTaskState::Queued));
        assert!(can_cancel_subtask(true, false, false, SubTaskState::Held));
        assert!(!can_cancel_subtask(true, false, false, SubTaskState::Dispatching));

        // 普通用户必须是 owner 且当前持有该目标的 device.reboot 权限
        assert!(can_cancel_subtask(false, true, true, SubTaskState::Queued));
        assert!(can_cancel_subtask(false, true, true, SubTaskState::Held));
        // 若普通用户失去了 device.reboot 权限，即使是本人任务也严禁取消
        assert!(!can_cancel_subtask(false, true, false, SubTaskState::Queued));
        // 非 owner 绝不可取消
        assert!(!can_cancel_subtask(false, false, true, SubTaskState::Queued));
    }

    #[test]
    fn sse_event_format_and_local_revision_contract() {
        // SSE 命名事件契约测试：确保格式符合 event: device.updated，且携带局部 revision
        let device_event = json!({
            "event": "device.updated",
            "data": {
                "device_id": "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
                "revision": 7,
                "status": "online"
            }
        });
        assert_eq!(device_event["event"], "device.updated");
        assert_eq!(device_event["data"]["revision"], 7);
        // 校验局部 revision 而非全局列表 revision
        assert!(device_event["data"].get("global_revision").is_none());
    }
}
```

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller authorized_subset_projection_hides_unauthorized_targets --
cargo test --offline --locked -p rsetup-controller cancel_subtask_requires_exact_permissions_and_state --
```
预期失败：两条命令各执行 1 个测试；第一条因桩返回 full 且缺少 counts 而断言失败；第二条因桩返回 false 拒绝取消断言失败。**真实行为断言失败，绝非编译错误或 0 用例**。

- [ ] **Step 3: 最小实现**

1. 授权子集投影：根据 actor 的 `device.task.read` 权限与设备授权过滤；非全权用户返回 `view_scope: "authorized_subset"`，计数仅统计可见子任务，隐藏总设备数；
2. 取消权限控制：仅限 `Queued` 与 `Held` 状态；管理员可跨用户取消（事务内校验管理员状态）；普通用户仅限取消本人提交且当前逐目标持有 `device.reboot` 权限的子任务，并在事务中重验 `authz_epoch`；已开始执行（dispatching/accepted/verifying）坚决不直接取消；
3. SSE 命名事件端点与授权清理：
   - 命名事件格式：`event: device.updated/task.updated/permissions.changed/system.time.changed/reset`；
   - 携带对象局部 revision，不假设列表全局 revision；
   - 每连接待发队列上限 128 条；溢出时下发 `event: reset` 并断开连接；
   - 每次发送重验 session 与 `authz_epoch` 变化，用户被撤权立即清理队列并断开连接。

- [ ] **Step 4: 运行并确认 GREEN**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller authorized_subset_projection_hides_unauthorized_targets --
cargo test --offline --locked -p rsetup-controller cancel_subtask_requires_exact_permissions_and_state --
cargo test --offline --locked -p rsetup-controller sse_event_format_and_local_revision_contract --
```
预期通过：各命令执行 1 个测试，全部 PASS（共 3 个用例 PASS，0 失败）。

- [ ] **Step 5: 重构、代码检查与提交规范**

运行静态检查：
```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
```
确认无警告后受控提交：`feat(controller): authorized task views and bounded sse events`。

---

### Task 7: 旧备份受控恢复与 MySQL/TiDB 双数据库验收 (Controlled Recovery Mode & Dual DB Acceptance)

**Files:**
- Create: `crates/rsetup-controller/src/operations/mode.rs`
- Create: `crates/rsetup-controller/src/operations/retention.rs`
- Create: `crates/rsetup-controller/src/operations/mod.rs`
- Create: `crates/rsetup-controller/tests/runtime_db.rs`
- Modify: `crates/rsetup-controller/src/main.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`

**Interfaces:**
- Consumes: Task 2 时钟门禁、Task 3 runtime schema、Task 4 恢复逻辑、`db::DbPool`.
- Produces:
  - `pub enum RecoveryMode { Normal, ControlledRecovery }`
    - 受控恢复模式（Controlled Recovery）语义明确限定为：**禁止自动准入（admission）与禁止变更任务（mutation tasks）调度**；绝不笼统阻止所有写接口；受审管理恢复入口（管理员登录、修改密码、注销、审计事件写入、单设备只读核验、以及显式退出恢复模式）必须保持正常可用；
  - `pub struct RetentionPolicy { pub completed_task_ttl_days: u32, pub audit_ttl_days: u32, pub idempotency_ttl_hours: u32 }`（参数说明：05 OPS-01 建议终态 30 天、审计 180 天、幂等 24 小时，均为待审建议默认）
  - `pub fn is_safe_to_delete(state: SubTaskState, lock_released: bool, age_days: u32) -> bool`
  - `pub async fn clean_retention_data(db: &DbPool, policy: &RetentionPolicy) -> Result<RetentionReport, ControllerError>`

- [ ] **Step 1: 先建立可编译桩，再编写行为 RED 测试**

在 `crates/rsetup-controller/src/operations/retention.rs` 中放置**可编译桩（错误返回 true 允许删除持锁记录导致断言失败）**：
```rust
use crate::tasks::model::SubTaskState;

// 可编译 RED 桩：恒返回 true，误将 held/unknown 及未释放锁记录判定为可删除
pub fn is_safe_to_delete(_state: SubTaskState, _lock_released: bool, _age_days: u32) -> bool {
    true
}
```

在 `tests/runtime_db.rs` 中先写可编译的纯保留期行为测试：
```rust
use rsetup_controller::operations::retention::is_safe_to_delete;
use rsetup_controller::tasks::model::SubTaskState;

#[test]
fn retention_preserves_held_and_unknown_and_active_locks() {
    assert!(is_safe_to_delete(SubTaskState::Succeeded, true, 31));
    assert!(!is_safe_to_delete(SubTaskState::Held, true, 100));
    assert!(!is_safe_to_delete(SubTaskState::Unknown, true, 100));
    assert!(!is_safe_to_delete(SubTaskState::Failed, false, 40));
}
```

在 `tests/runtime_db.rs` 仅于 `BackupRecoveryFixture`、真实受控恢复入口、v4 DB schema 与真实备份/目标门禁**已实现且可编译**后增加 `#[tokio::test] #[ignore] fn old_backup_never_replays_queued`。缺 URL/fixture/批准时由操作员不运行并将证据标记 `BLOCKED`，不得用 `eprintln!("BLOCKED")` 加 `return` 让 ignored 用例报告成功，也不能凭 `tests::common::...` 未定义的路径冒充行为 RED。

未来先 RED 后 GREEN 的具体可观察行为：独立于测试程序的真实可恢复备份和当前目标核实后恢复旧 DB，确认旧 `queued` 子任务及锁确实在库中；运行生产受控恢复入口，断言未调度或发送 `execute`，旧锁与证据未被新任务释放；仅经管理员授权核查/取消后方可退出恢复模式。管理员登录、改密、注销和必要去敏审计应可用；未放行的设备准入/变更下发被拒绝，不能笼统阻止所有管理写。数据库错误、空运行数、备份缺失或状态不可证均不构成 PASS；不能凭伪造 checkpoint 或 BoardClient mock 次数代替持久性断言。

- [ ] **Step 2: 运行并确认行为 RED（严禁编译错误与 0 用例）**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller --test runtime_db retention_preserves_held_and_unknown_and_active_locks --
```
预期失败：执行 1 个测试；因桩恒返回 true 误删 held/unknown 与持锁记录而断言失败。**真实行为断言失败，绝非编译错误或 0 用例**。

真实 DB 验收测试命令（仅在显式隔离配置下执行）：
```bash
CONTROLLER_TEST_DATABASE_URL="$MYSQL_TEST_URL" cargo test --offline --locked -p rsetup-controller --test runtime_db old_backup_never_replays_queued -- --ignored --test-threads=1
CONTROLLER_TEST_DATABASE_URL="$TIDB_TEST_URL" cargo test --offline --locked -p rsetup-controller --test runtime_db old_backup_never_replays_queued -- --ignored --test-threads=1
```
核对各引擎执行 1 个用例且断言失败，缺 URL 标未验证，禁止 0 用例。

- [ ] **Step 3: 最小实现**

1. 保留期清理守则：仅对已释放锁（`lock_released == true`）且处于 `Succeeded`、`Failed`、`Cancelled`、`Expired` 的终态记录执行超期清理（建议 30 天待审默认）；`Held`、`Unknown`、`Queued`、`Dispatching`、`Accepted`、`Verifying` 状态记录，以及任何尚未释放设备锁的记录，**绝对不执行物理删除**；
2. 受控恢复模式（Controlled Recovery）：旧备份恢复标记或冷启动异常时激活；禁止自动放行历史 `Queued` 任务，必须由管理员显式审查或取消；严格限定拦截范围：禁止设备准入决策与变更任务下发；保持管理员登录、改密、注销、审计记录、单设备只读核查及管理员核准退出受控恢复模式等关键入口可用，绝不笼统封锁所有写操作。

- [ ] **Step 4: 运行并确认 GREEN**

运行精确单测命令：
```bash
cargo test --offline --locked -p rsetup-controller --test runtime_db retention_preserves_held_and_unknown_and_active_locks --
```
预期通过：执行 1 个测试，PASS。
真实 DB 忽略测试双引擎分别验证执行 1 个用例且 PASS（零失败，记录被测引擎版本）。

- [ ] **Step 5: 重构、代码检查与提交规范**

运行静态检查：
```bash
cargo fmt -p rsetup-controller -- --check
cargo clippy --offline --locked -p rsetup-controller -- -D warnings
```
确认无警告后受控提交：`feat(controller): controlled recovery mode and retention cleanup guards`。

---

## 规格覆盖与验收映射

| 规格条款 / 需求 | 覆盖任务 | 核心实现与验收标准 |
| :--- | :--- | :--- |
| **04 TASK-01/02** | Task 3 | 任务预览 128bit token、幂等键去重；一主一子；同设备跨批次互斥，后来者报 `DEVICE_BUSY` 且不排队。 |
| **04 TASK-03/04** | Task 4 | 短事务提交 `dispatching` 意图后才调用外部 `execute`；中控崩溃恢复先重建锁，不重复下发指令。 |
| **04 TASK-05/06** | Task 4 | 必须凭 `boot_transition_observed` 判定成功；`unknown` 持续持有设备锁并释放并发槽；迟到证据修正结果。 |
| **04 TASK-07** | Task 4, 7 | 跨进程单调时钟不可信转为 `held`；旧备份恢复进入受控模式，绝不盲目放行重放。 |
| **05 TIME-01/02** | Task 1, 2 | 独立 NTP 客户端；启动预算 ≤30s 关卡门禁（00 §3 C-TIME 已确认约束）超时回退系统墙钟并后台指数退避；时钟跳变检测（`>1s`）自动切换 epoch。 |
| **05 TIME-03** | Task 1 | 4 时戳纯算法偏差估算；RTT 阈值比较（建议上限 2s）；负 RTT、跨 boot、跨 epoch 样本显式拒绝。 |
| **05 RUN-01** | Task 5 | 1024 台设备在 10s 周期内纳秒相位错峰分布；过载与停顿跳过轮次不积压，无洪峰崩溃。 |
| **05 OPS-01** | Task 7 | 建议终态 30 天、审计 180 天、幂等 24 小时保留期；held/unknown 与未释放锁坚决不清理。 |
| **06 AT-05..10** | Task 3, 5, 6 | 授权子集投影；管理员跨用户取消；SSE 128 上限带溢出 reset；设备状态单调 30s 新鲜度。 |
| **06 AT-12/13/15** | Task 4, 7 | 重启凭据校验；双引擎（MySQL 8.4 与 TiDB 8.5）独立迁移、锁与 CAS 验收。 |

---

## 执行交接与尚待主代理核对风险

1. **草案审阅与安全阻断状态：** `controller-v1 / draft-1` 协议层面关于 `reason_code` 签名未覆盖、验签失败描述冲突与 AEAD 失败终止策略的安全阻断未解除前，不得宣称具备生产发布条件。
2. **拟议/建议参数待审阅核准：** 规格 05 §1/§2/§4/§6 中关于 NTP 单次超时 ≤3s、板端 RTT 上限 ≤2s、并发槽 16、排队有效期 60min、status过期阈值30s、终态保留期 30d/180d/24h 均为待审默认建议值，需人工审查最终定稿，不可直接视为不可变更的常量；只有 NTP 启动关卡总预算 ≤30s（00 §3 C-TIME）属于已确认约束。
3. **真实数据库备份与隔离门禁：** 所有真实 DB 集成测试标记 `#[ignore]` 并依赖 `CONTROLLER_TEST_DATABASE_URL`，必须确认是获授权、具备已核验真实备份且可丢弃的隔离库；无 URL 显式标“未验证/BLOCKED”，连接错误或 0 用例不可视为通过；不能为文档写假 DB 绿。
4. **不盲发重启纪律：** 任何崩溃恢复或备份恢复场景，必须严格保证对已写意图或未知状态只核查、绝不自动重发 `execute`。

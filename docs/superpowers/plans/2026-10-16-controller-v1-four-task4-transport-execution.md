# Controller V1 四专题 Task 4 · 传输执行层实施计划

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实；使用 `- [ ]` 复选框跟踪步骤。
>
> **本计划的性质：** 这是**条件性增量实施计划**，只规划「共享 wire DTO（不含 codec）→ 板端 crate 与严格类型化白名单 → 幂等任务 journal（稳定 epoch 与两阶段 fsync）→ 票据与固定无参 OS 边界 → 内存双流绑定与审批门」这五个非密码纯组件。**未获外部专家书面决议与用户复审前，严禁编写任何密码学实现、严禁自造 Protobuf codec、严禁声称线协议互通或断电持久性已验收。**

**Goal:** 在 `dev-aghost` 已合并的 `rsetup-protocol` wire/帧切片之上，交付四专题 Task 4 在线协议侧所必需的传输执行层纯组件切片：唯一共享的类型化业务 DTO（保留全部证据字段、无 codec、Metric 使用 `Option<f64>` 与有限浮点校验、不派生 Eq）、`rsetup-board-agent` crate、严格 6 项入站动作白名单（`device.task.result` 仅为板端→中控事件、不入白名单与 `supported_actions`）与零变更隔离、带持久 `journal_epoch` 与「临时文件 → 文件 `sync_all` → 原子 rename → 父目录 `sync_all`」（rename 本身及之后为 `CommitUnknown`、rename 前确定失败为 `DefinitelyNotCommitted`）的幂等任务 journal 及真行为失败注入分类、30s 单调票据与固定无参 OS 重启边界（`accepted`/`mark_attempted` 持久化前失败不调用 OS：`insert_accepted` 确未提交可安全非零拒绝（fake OS 计数 0）、其 `CommitUnknown` 为 `Indeterminate` 断流无业务回包、不调用 OS（计数 0）；`accepted` 可靠而 `mark_attempted` 确未提交必须回复 `status=0` 的最后可靠 `accepted`、不能非零（计数 0）、其 `CommitUnknown` 亦 `Indeterminate` 断流无业务回包（计数 0）；`attempted` 可靠后才进入固定无参 `reboot()` OS 边界；OS 明确报错时仅 `finish(failed, "OS_ERROR")` 可靠落盘才以 `status=0` 回复真实 failed（FakeOS 计数 1、宿主物理 reboot 0），`finish` 为 `DefinitelyNotCommitted` 或 `CommitUnknown` 均 `Indeterminate` 断流无业务回包（计数 1）、不返回旧 `attempted`、不伪新 failed/unknown、不非零暗示未执行，且不得宽泛声称「所有 OS 明确错误无回包」（可靠 failed 仍可 status 0）；OS 返回 Ok 且 `attempted` 可靠只能回复 `attempted` 不能叫成功（真实重启证据需认证持久日志/boot transition）；`HostRebootOs` 恒 `Unsupported` 仅为组件安全切片，不等于完整真实任务互通验收，不得宣称 full Task4 端到端完成）、先 Control 后 Data 的内存双流绑定与 sealed/不可伪造 witness 守卫的 fail-closed 审批门。

**Architecture:**
1. 业务 DTO 与结构校验放回唯一共享 crate `crates/rsetup-protocol/src/wire.rs`（`Capabilities / DeviceStatus / ClockSample / RebootPrepare / RebootTicket / RebootExecute / TaskQuery / TaskRecord` + `BusinessMessage` trait + `validate_dto`），**只做结构校验，不定义任何字节格式**；`Metric` 的 `value` 为 `Option<f64>`，`Metric` 与 `DeviceStatus` 绝不派生 `Eq`，校验 `is_finite` 且 bytes 双精度整数 `<= 2^53 - 1`；`Codec` trait 使用 sealed trait 机制与私有不可伪造见证类型 `ApprovedCodecWitness`，杜绝任何公共虚方法绕过未批准 codec，暴露「无获批 codec 时原始字节入口固定不可接入」这一事实，保持编译与负例。
2. 新增板端 crate `crates/rsetup-board-agent`，把「允许哪些入站请求动作」做成**封闭的编译期常量表**（仅 6 个入站动作；`device.task.result` 属于仅板端→中控的 `TYPE_EVENT`，绝不纳入入站白名单或 `capabilities.supported_actions`），白名单之外的 action 在触碰任何 source/journal 之前返回 `UnsupportedAction`；原始字节入口 `handle_wire` 在解析前先查表，再对未获批 codec fail-closed，**绝不调用 OS 或任何持久写**。
3. `crates/rsetup-board-agent/src/journal.rs` 以 `Fs` trait 为最小文件操作缝（生产 `RealFs`，测试 `RecordingJournal` / `FaultyFs`），实现稳定 `journal_epoch`、`pre_boot_id` 在 `accepted` 即落盘、去重键 `(controller_key, sub_task_id)` + payload 哈希、容量上限 4096、以及失败分类：rename 之前的创建/写/sync 确定失败为 `DefinitelyNotCommitted`；rename 调用本身失败及之后的父目录 sync 失败为 `CommitUnknown`。使用 workspace 锁定的 `serde` 与 `serde_json` 依赖，绝不手写伪解析。删除无使用且越层的 `TicketConsumedNotExecuted`。`TempDir` 在 Drop 删除前严格验证绝对路径属于系统临时目录且携带专用前缀以防误删，错误可诊断。
4. `ticket.rs` 提供一次性消费的 30s 单调票据；`reboot.rs` 把执行顺序固定为 `journal dedup → fresh boot 校验 → consume → insert_accepted → mark_attempted → os.reboot()`（形状校验在最前；dedup 命中已存在持久记录时直接回该记录，不触达当前 boot 校验、票据与 OS），`RebootOs::reboot()` **无参数**。`accepted`/`mark_attempted` 持久化前失败不调用 OS：`insert_accepted` 确未提交可安全非零拒绝（fake OS 计数 0）、其 `CommitUnknown` 为 `Indeterminate` 断流、无业务回包、不调用 OS（计数 0）；`accepted` 可靠而 `mark_attempted` 为 `DefinitelyNotCommitted` 时不调用 OS，必须回复最后持久 `accepted` 的 `status=0`，绝不能非零（计数 0），其 `CommitUnknown` 亦 `Indeterminate` 断流、无业务回包（计数 0）；`attempted` 可靠后才进入固定无参 OS 边界；OS 明确报错时仅 `finish(failed, "OS_ERROR")` 可靠落盘才以 `status=0` 回复真实 failed（FakeOS 计数 1、宿主物理 reboot 0）；`finish` 为 `DefinitelyNotCommitted` 或 `CommitUnknown` 均 `Indeterminate` 断流、无业务回包（计数 1），不返回旧 `attempted`、不伪造 failed/unknown、不非零暗示未执行，不得宽泛声称「所有 OS 明确错误均无回包」（可靠 failed 仍可 status 0）；OS 返回 Ok 且 `attempted` 可靠只能回复 `attempted` 不能叫成功（真实重启证据需认证持久日志/boot transition）。`HostRebootOs` 永远返回 `Unsupported`，仅为纯组件安全切片，绝不等于完整真实任务互通验收，不得宣称 full Task4 端到端完成；保留不获批 crypto/codec 绝不实施、不真实重启。
5. `tunnel_binding.rs` 用真实 `BoardAgent` + 真实 `TaskJournal` + 内存双流（不伪造布尔 peer）装配，强制「先 Control 再 Data」，并把真实网络/真实加密通道统一挡在 `ApprovalGate::require_network_io()` 之后。

**Tech Stack:**
- workspace 既有且已锁定：Rust 2024 / MSRV 1.85、`tokio = "1.47"`（`Cargo.lock` 解析 1.53.1，含 `tokio-macros`）、`uuid = "1.18"`（解析 1.26.0，`Uuid::from_u128` 为 crate 内置 `const fn`）、`thiserror = "2.0"`、`serde = "1.0"`（features `["derive"]`）、`serde_json = "1.0"`、`hex = "0.4"`、`sha2 = "0.10"`、`ring = "0.17"`。
- **本计划不引入任何新依赖。** `tempfile` **不在 `Cargo.lock`**（已核对：`grep -n '^name = "tempfile"' Cargo.lock` 无输出），因此禁止把它作为 board-agent 的 dev-dependency；测试所需的临时目录一律用标准库 `std::{env, fs, sync::atomic, process}` 在 `tests/support/tempdir.rs` 内自行实现，并验证独立临时根防误删。`prost` / `tonic` / `cargo-fuzz` 同样未获批，禁止引入。
- `crates/rsetup-board-agent` 直接依赖限定为：`rsetup-protocol`、`rsetup-core`、`tokio.workspace`、`uuid.workspace`、`thiserror.workspace`、`serde.workspace`、`serde_json.workspace`；`[dev-dependencies]` 仅 `tokio.workspace` 与 `uuid.workspace`。board-agent 若持久化序列化，必须使用上述已锁定的 `serde` 与 `serde_json`，绝不手写伪解析。

**Spec:**
- `docs/superpowers/specs/2026-10-07-controller-v1-four-task4-design.md`（§2 阶段 A/B/C 依赖顺序、§4 任务持久执行与证据、§6 验收账本分层）
- `docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md`（§1 信封与 action 目录、§2 载荷定义、§4 票据、§5 日志与状态、§6 事件合并、§7 WIRE-01..05）
- `docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`（§1 不变量、§2 预览提交、§3 子任务状态、§5 串行流程、§6 成功/失败证据、§7 恢复）
- `docs/superpowers/plans/2026-10-07-controller-v1-remaining-transport-tdd.md`（Task 3/4 及其后续 Task 5/8/9/10 衔接）
- `docs/superpowers/plans/2026-10-07-controller-v1-remaining-runtime-tdd.md`（Task 4 `BoardClient` 契约，本计划为它提供 DTO 与形态建议，不实现中控调度器）

---

## Global Constraints

1. **密码学与 codec 硬闸门（不可由纯组件测试解除）**
   - 现行线协议仍是 `docs/protocol_spec.md` (v2)。协议 v3 修订受独立外部专家**书面决议原件**阻断；在决议落到 `docs/superpowers/reviews/2026-10-03-tunnel-security-expert-decisions.md` 并经用户复审前：**决不编写 X25519 / HKDF / AES-256-GCM 实现，决不自定签名转录格式，决不创建虚构专家报告，决不以自造 Protobuf 编解码器替代待审定依赖。**
   - 本计划所有「原始字节」入口（`BoardAgent::handle_wire`、`RebootOs` 之外的一切 wire 编解码、`decode_business` / `encode_business`）在无获批 codec 时**必须 fail-closed**：返回 `CodecError::CodecUnapproved`，且在被调用前不触碰 source/journal/OS。**严禁由公共 `Codec::is_approved()` 虚方法绕过未批准 codec**；必须采用 sealed trait 与私有不可伪造的见证类型 `ApprovedCodecWitness`，未获批原始路径固定不可接入，同时保持编译通过与负例测试。**无获批 codec 时永不宣称 WIRE-01 的 512 KiB 整包上限已通过。**
   - 未获独立专家批准前，不得声称加密握手、真实网络互通、双进程 smoke 或线协议兼容性已验收。Task 5 的真实网络绑定必须停在 `ApprovalGate`。

2. **板端安全边界与只读原则**
   - 远程入站请求白名单是**封闭集合**，仅允许 6 个入站动作：`device.capabilities.get`、`device.status.get`、`device.clock.get`、`device.task.get`、`device.reboot.prepare`、`device.reboot.execute`。`device.task.result` 属于仅板端→中控的 `TYPE_EVENT`，绝不纳入入站请求 action allowlist，也不纳入 `capabilities.supported_actions`。
   - 绝不暴露 shell、任意命令、配置写入、关机、升级、文件分发或任何现有单板变更动作；`tunnel.` 保留前缀的动作一律拒绝。
   - 动作必须在触碰 source/journal 之前完成白名单判定；`device.task.get` 的 `TaskQuery` 必须先通过 `schema_version == 1` 与非空合法 `sub_task_id` 校验，非法时对 OS/持久写的调用次数必须为 0；查询结果为 `None` 时必须显式返回 `TASK_NOT_FOUND`，不得误 map 导致类型不匹配。
   - 任务去重命名空间必须绑定**已认证 controller 公钥** `(controller_key, sub_task_id)`，不同控制器不得互相覆盖。
   - `RebootOs::reboot()` 必须固定无参，绝不接受 command/path/argv。
   - **HostRebootOs 永远 Unsupported，仅为隔离组件切片，不等于完整真实任务互通验收；不得宣称 full Task4 端到端完成。** 自动化测试中宿主机真实 reboot 调用恒为 0：所有测试使用记录型 `RebootOs` 替身；任何 `#[ignore]`、`--ignored` 或手工步骤都不得触发真实主机重启。fake OS 调用计数（FakeOS/`RecordingOs`/`FakeOs::call_count`，可以为 1，证明 OS 边界被进入）与宿主物理 reboot 计数（`HostRebootOs::call_count`，恒 0）严格区分、互为证据，不得互相冒充；本切片无真硬件验收。

3. **journal 与落盘纪律**
   - `journal_epoch` 是**持久 UUID**：首次初始化时写入并 `sync_all`；普通 agent 进程重启必须沿用它；只有日志丢失或不可逆损坏导致重建时才更换，并向中控报告 `JOURNAL_LOST`。
   - `pre_boot_id` 必须在 `accepted` 阶段即确定并落盘，绝不推迟到 `attempted`。
   - 可靠落盘顺序恒为：同目录临时文件写入 → 文件 `sync_all` → 原子 `rename` → 父目录 `sync_all`。
   - **严格失败分类：** 只有在 `rename` 调用之前（创建临时文件、写入数据、临时文件 `sync_all`）确定失败才分类为 `DefinitelyNotCommitted`；`rename` 调用本身失败（可能在底层已发生或半提交）以及 `rename` 之后父目录 `sync_all` 失败，均必须分类为 `CommitUnknown`。
   - 临时文件路径生成必须使用 `Path::file_name` 并严格校验有效性，拒绝非法路径，绝不 fallback 到固定文件名；`TempDir` 在 Drop 删除前必须验证绝对路径属于自己创建的独立临时根且具有特定前缀以防误删，错误须可诊断。
   - **普通 reopen 只证明文件格式一致，不证明断电持久性。** 断电/电源故障恢复证据需要独立门禁（真实电源故障注入），本计划不得以 reopen 测试推导。
   - 日志序列化采用已锁定的 `serde` 与 `serde_json`，绝不手写伪解析；删除无使用且越层的 `JournalError::TicketConsumedNotExecuted`。
   - 状态迁移恒为 `accepted → attempted → finish(succeeded|failed|unknown)`；日志容量 4096（03 §5 待审默认），满时拒绝新任务 `ResourceExhausted`，**首版绝不淘汰旧去重记录**。

4. **成功证据与执行结果收敛**
   - `accepted` 必须可靠落盘后才能返回接收响应；`attempted` 必须可靠落盘后才能调用 OS。
   - **execute 严格错误与响应边界（B-04 裁决，03 §1/§5）：**
     1. 若 `insert_accepted` 确未提交（`DefinitelyNotCommitted`），可安全返回非零错误（如 `InternalError` 或 `ResourceExhausted`），OS 不调用（fake OS 计数 0）；
     2. 若 `accepted` 已可靠持久，但随后的 `mark_attempted` 为 `DefinitelyNotCommitted`：OS 绝对不调用（计数 0），且**必须使用 status_code=0 回复最后持久 accepted 的 TaskRecord，绝不能返回非零错误**（不能暗示未接收）；
     3. `insert_accepted` 或 `mark_attempted`（OS 边界前）遇到 `CommitUnknown`，结果为 `Indeterminate`：**必须不发送业务结果、网络断流，OS 绝对不调用**（计数 0），交由后续 `device.task.get` 核实；
     4. `attempted` 可靠落盘且 OS 明确报错时：只有 `finish(failed, "OS_ERROR")` 可靠落盘才以 `status_code=0` 回复真实持久 failed（FakeOS 计数 1、宿主物理 reboot 0）；若 `finish` 写入为 `DefinitelyNotCommitted` 或 `CommitUnknown`（OS 边界后），均为 `Indeterminate` 断流、无业务回包（计数 1）：不返回旧 `attempted`、不伪造新 failed/unknown、不非零暗示未执行；**不得宽泛声称「所有 OS 明确错误均无回包」**——可靠 failed 仍可 status 0；
      5. OS 返回 Ok 且 `attempted` 可靠落盘时，只能以 `status_code=0` 回复持久 `attempted`（**不代表成功**：03 §5 `attempted ≠ 成功`，真实重启证据需认证持久日志/boot transition），绝不虚构 succeeded。
   - 板端恢复只有在「存在可靠落盘的 `attempted` 记录」且「当前 `boot_id` ≠ 记录 `pre_boot_id`」时才原子写入 `succeeded`，并附 `observed_boot_id = 当前 boot` 与 `evidence_kind = "boot_transition_observed"`。
   - 同 boot、仅 `accepted` 或日志丢失时，板端**绝不自动补调用、绝不自动重发**，也不据此声称成功或失败。
   - boot 变化本身、断线、Pong 恢复、`TASK_NOT_FOUND`、跨 `journal_epoch` 结果都不足以证明成功；这些判定属中控侧（04 §6），本计划不实现中控收敛逻辑。
   - 证据合并：较低 `record_version` 丢弃；同版本不同内容报一致性异常，绝不静默覆盖。

5. **严格 TDD，且 RED 必须是真实行为断言失败**
   - 每个 Task 的顺序恒为：先建**可编译桩**（类型/方法签名齐全）→ 写失败测试 → 运行定向命令确认**目标断言**失败（非编译错误、非 0 用例）→ 最小实现 → 运行确认 PASS → 显式 pathspec 提交。
   - Task 3.1 / Task 3.2 失败注入必须先有**真实行为 RED** 再实现分类：Step 3.1 仅实现基础文件写入与通用 IO 错误（未分类），Step 3.2 引入 `DefinitelyNotCommitted` 与 `CommitUnknown` 断言测试并运行确认因返回通用错误而断言失败（行为 RED），随后才在 `write_durable` 中实现精确分级；**绝对不能在已实现之后假称 RED**。
   - 缺少模块、类型未定义、语法错误的编译失败**不算 RED**。任何测试都不得用 `return`、空断言、`#[ignore]` 空跑或常量结果冒充通过。
   - **本计划的落地工作只编辑本文件，不修改其它文件、不提交、不派代理、不访问 secret、不实际联网或操作硬件。** 计划中的所有 `cargo test` 命令是给执行会话的交接内容，计划作者不得运行（运行会改写 `Cargo.lock`/`target/`）。

6. **依赖与锁文件纪律**
   - 新增 workspace 成员 `crates/rsetup-board-agent` 会改变 workspace 解析，Cargo 必须重算 `Cargo.lock`。因此：**第一次**在 board-agent 上执行 cargo 时，若只用已锁定的版本，需先运行不带 `--locked` 的 `cargo build -p rsetup-board-agent`（生成含 board-agent 的 lock 条目）并**提交 `Cargo.lock`**；此后所有命令统一带 `--offline --locked`。
   - 严禁只 `git add Cargo.toml` 而遗漏 `Cargo.lock`。若 `--locked` 报「lock file needs to be updated」，说明引入了未审依赖 —— 立即停止并报告，不得为了让它通过而放宽依赖。
   - 每个 worktree 使用私有 `CARGO_TARGET_DIR`（如 `export CARGO_TARGET_DIR=/tmp/cargo-target-<worktree>`），避免与其他 worktree 互相污染。

7. **凭据与真库门禁**
   - `secret/` 只读授权仅覆盖 `secret/mysql.json` 与 `secret/tidb.json` 指定的两个专用隔离开发库；本计划**不需要**任何真库、真实备份或迁移，全部测试在进程内/本地临时目录完成。
   - 绝不把凭据原文、派生值或 `secret/` 内容写入本计划、代码、测试 fixture、日志或提交。
   - 真实 reboot、真实电源故障、真实网络双进程、真库 v4 迁移与备份门禁**均不在本计划范围内**，且必须保持未运行。

---

## 文件结构与职责划分

| 文件路径 | 动作 | 唯一职责 | 任务 |
|---|---|---|---|
| `crates/rsetup-protocol/src/wire.rs` | Modify | 追加业务 DTO、`BusinessMessage`、`validate_dto`、`Codec`/`NoCodec` 与 fail-closed 入口 | Task 1 |
| `crates/rsetup-protocol/src/lib.rs` | Modify | 重导出新增 DTO 与校验入口 | Task 1 |
| `crates/rsetup-protocol/tests/wire_business_dto.rs` | Create | DTO 结构约束、UUID/schema/冲突矩阵、codec fail-closed | Task 1 |
| `crates/rsetup-protocol/src/wire.rs` | Modify | 追加 `canonical_reboot_payload_hash` 规范载荷哈希 producer（排除票据/nonce） | Task 1B |
| `crates/rsetup-protocol/src/lib.rs` | Modify | 重导出列表追加 `canonical_reboot_payload_hash` | Task 1B |
| `crates/rsetup-protocol/tests/wire_business_dto.rs` | Modify | 追加 1 个表驱动规范哈希纯测试（1 方法、多断言） | Task 1B |
| `Cargo.toml` | Modify | 根 `members` 加入 `crates/rsetup-board-agent` | Task 2 |
| `Cargo.lock` | Modify（自动） | 记录 board-agent 成员解析；必须与 `Cargo.toml` 同提交 | Task 2 |
| `crates/rsetup-board-agent/Cargo.toml` | Create | 板端 crate manifest，仅使用已锁定依赖 | Task 2 |
| `crates/rsetup-board-agent/src/lib.rs` | Create | 窄化导出：模块、常量、类型 | Task 2 |
| `crates/rsetup-board-agent/src/actions.rs` | Create | 封闭动作表：白名单、保留前缀、`ActionKind` | Task 2 |
| `crates/rsetup-board-agent/src/dispatch.rs` | Create | `BoardAgent` 类型化调度、`BusinessStatus`、原始字节 fail-closed | Task 2 |
| `crates/rsetup-board-agent/src/read_only.rs` | Create | `ReadOnlySource` / `ReadOnlyMutationGuard` 与 `PeerContext` | Task 2 |
| `crates/rsetup-board-agent/tests/support/tempdir.rs` | Create | 纯 std 临时目录守卫（替代未获批的 `tempfile`） | Task 3 |
| `crates/rsetup-board-agent/tests/support/mod.rs` | Create | 测试 support 模块入口（`pub mod` 按需；未用到的子模块用 `#[path]` 显式引入） | Task 3 |
| `crates/rsetup-board-agent/tests/read_only.rs` | Create | 白名单拒绝、零变更、命名空间隔离 | Task 2 |
| `crates/rsetup-board-agent/src/journal.rs` | Create | `TaskRecord`/`TaskState`、`Fs` 缝、稳定 epoch、两阶段 fsync、去重、恢复 | Task 3 |
| `crates/rsetup-board-agent/tests/wire_04_journal.rs` | Create | 契约测试：稳定 epoch、pre_boot、去重/冲突、容量、恢复 | Task 3 |
| `crates/rsetup-board-agent/src/test_seam.rs` | Create | `#[cfg(any(test, feature = "test-seam"))]` 的 `RecordingJournal` / `FaultyFs` | Task 3 |
| `crates/rsetup-board-agent/tests/journal_fault_injection.rs` | Create | 失败注入：rename 前后分类、持久性见证、清理顺序 | Task 3 |
| `crates/rsetup-board-agent/Cargo.toml` | Modify | 在 Task 2 既有 `[features]` 表内补 `test-seam = []` 行 | Task 3 |
| `crates/rsetup-board-agent/src/ticket.rs` | Create | 30s 单调票据、代际绑定、一次性消费 | Task 4 |
| `crates/rsetup-board-agent/src/reboot.rs` | Create | 固定无参 `RebootOs`、执行顺序、OS 错误证据、宿主 reboot 恒零 | Task 4 |
| `crates/rsetup-board-agent/tests/wire_03_tickets.rs` | Create | 票据过期/代际/sub_task_id/boot 绑定与一次性消费行为 | Task 4 |
| `crates/rsetup-board-agent/tests/reboot_boundary.rs` | Create | 落盘先后、零 OS 调用、OS 错误证据 | Task 4 |
| `crates/rsetup-board-agent/src/tunnel_binding.rs` | Create | 内存双流绑定、先 Control 后 Data、`ApprovalGate` | Task 5 |
| `crates/rsetup-board-agent/tests/interop_dual_stream.rs` | Create | Data 阻塞、真实组件装配、raw 端点 fail-closed | Task 5 |

---

## 增量实施任务清单


---

## 专题带编号拆分模块索引（单文件 700~2700 行）

为保障可维护性并消除单文件过大问题，完整执行细节按职责已拆分到对应子目录中（保留本主计划在 `docs/superpowers/plans/` 作为执行总索引）：

- [01-dto-and-board-agent.md](2026-10-16-transport/01-dto-and-board-agent.md)：Task 1, 1B, 2: 业务 DTO、规范载荷哈希、board-agent crate 与白名单（~1356 行）
- [02-journal-store.md](2026-10-16-transport/02-journal-store.md)：Task 3: 两阶段原子 fsync journal、全封闭终态守卫与 uncertain 隔离（~1536 行）
- [03-tickets-and-reboot.md](2026-10-16-transport/03-tickets-and-reboot.md)：Task 4: 30s 单调票据、设备占用排他保护与 OS 重启边界（~1611 行）
- [04-dual-stream-and-gates.md](2026-10-16-transport/04-dual-stream-and-gates.md)：Task 5: 内存双流绑定、Control-first 与审批门（~763 行）

---


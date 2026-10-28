# 子规格 12：`crates/rsetup-controller/src/tasks/service.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 12 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未读 `secret/`、未连接数据库、未提交。

撰写基线：分支 `dev-aghost`，头提交 `c7ff640`。撰写时 `git status` 实测：本文件 [tasks/service.rs](../../../../crates/rsetup-controller/src/tasks/service.rs) **无未提交改动**（行号与工作树一致），但同域 [tasks/scheduler.rs](../../../../crates/rsetup-controller/src/tasks/scheduler.rs)（+9/−7）与 [tasks/scheduler_store.rs](../../../../crates/rsetup-controller/src/tasks/scheduler_store.rs)（+83/−7 未提交）存在他人改动，漂移影响登记见 §9；全部行号为撰写会话逐段实测，实施时按当时工作树重新核对（总览 §3 并行改动注记）。任务域串行约束：本规格属总览 §5.2 批次 D 的第一项，**12 的 service/状态机边界定稿在前、06 后实施**；本文件不设计 06 的内部，与 [06-tasks-http.md](06-tasks-http.md) 的交叉一致登记见 §9。

## 1. 实测现状

### 1.1 体量、多测试块精确统计与消费面

- 全文件实测 **2,595 行** = 实现区 1–1,620（1,620 行，与总览 §3 快照「约 1,620」一致）+ 测试区 1,621–2,595（975 行，与「约 975」一致）。测试区含 **三个测试块**（快照「含多个测试块」成立，实测定型）：
  - **test-only 顶层函数**：第 358 行 `#[cfg(test)]` 标注的 `snapshot_allows_task_creation`（358–364），唯一消费者是 `policy_tests`（2533/2547/2567/2579/2580）；它不是测试本身，而是纯策略单测的布尔薄包装（源码 357 行文档自述「提交路径不消费本包装」）。
  - **`mod tests`**：`#[cfg(test)]`（1,622）+ 模块体 1,623–2,462（840 行），**22 个测试函数 = 17 个 `#[test]` + 5 个 `#[tokio::test]`**。
  - **`mod policy_tests`**：`#[cfg(test)]`（2,464）+ 模块体 2,465–2,595（131 行），**5 个 `#[test]`**。
  - 合计 **27 个测试函数**（`#[test]` 22 + `#[tokio::test]` 5），全部块已计入，无第三块、无被注释的测试（实测全文件仅 2 处 `mod tests`/`mod policy_tests` 声明与 1 处 `#[cfg(test)]` 函数属性）。
- 模块声明与再导出（C1 逐字不变）：[tasks/mod.rs](../../../../crates/rsetup-controller/src/tasks/mod.rs) 第 34 行 `pub mod service;`；`mod.rs` 的 `pub use`（36–38 行）不含 service 符号，**本拆分不动 `mod.rs` 任何一行**。`params!` 宏定义在 `tasks/mod.rs` 13–33 行、声明于 `pub mod service` 之前，故拆分后全部子文件仍可直接使用（无需 `#[macro_use]`）。
- 模块外消费面（实测，拆分后全部路径必须原样可用）：
  - [tasks/repository.rs](../../../../crates/rsetup-controller/src/tasks/repository.rs)（24–28 import 13 名：`AuthCtx`、`CancelOutcome`、`CancelRequest`、`ReleaseLockError`、`ReleaseLockOutcome`、`ReleaseRequest`、`SubmitError`、`SubmitOutcome`、`SubmitRequest`、`TaskService`、`TimeReviewError`、`TimeReviewOutcome`、`TimeReviewRequest`）——与 service 存在**既有双向 import**（service 38–41 行反向取 `TaskRepository` 等），拆分不新增此类耦合。
  - [tasks/adapter.rs](../../../../crates/rsetup-controller/src/tasks/adapter.rs)（67–71 import 14 名：上述之外的 `CancelError`、`TimeReviewDecision`、`parse_created_time_evidence`、`snapshot_effective_permissions`；并以字面量构造 `SubmitRequest`(327)、`AuthCtx`(436)、`CancelRequest`(751)、`TimeReviewRequest`(791)、`ReleaseRequest`(812)，故这些结构体的字段可见性 MUST 保持 `pub`）。
  - [tasks/http.rs](../../../../crates/rsetup-controller/src/tasks/http.rs)（50 import `SubmitOutcome`、`TimeReviewEvidenceKind`；用于 `TimeReviewTaskRequest.evidence`(201)、`TaskApi::submit` 返回(260) 与 handler 980）——与 06 的边界见 §9。
  - [tasks/scheduler_store.rs](../../../../crates/rsetup-controller/src/tasks/scheduler_store.rs)（工作树 30–32 import `counts_json_of`、`main_task_convergence`、`snapshot_effective_permissions`）与 [tasks/evidence.rs](../../../../crates/rsetup-controller/src/tasks/evidence.rs)（38 import `counts_json_of`；252 行文档钉 `counts_json_of` 为十键频数**唯一生产者**）。
  - [tasks/scheduler.rs](../../../../crates/rsetup-controller/src/tasks/scheduler.rs)（61/63 行另有调度域同名独立常量 `QUEUE_TTL_MS`/`OBSERVE_WINDOW_MS`，与本文件的 `QUEUE_TTL_MS` 是**两个不同常量**，勿混）。
  - [main.rs](../../../../crates/rsetup-controller/src/main.rs)（47 import `TaskService`）、[integrity.rs](../../../../crates/rsetup-controller/src/integrity.rs)（1,480 测试内取 `empty_counts_json`）。
  - 集成测试（总览 C6，只作验证依据、不改）：`tests/time_consumption.rs`（41–46：`QUEUE_TTL_MS`、`TaskService`、`TimeReviewDecision`、`TimeReviewEvidenceKind`、`TimeReviewVerdict`、`queue_deadline_of`、`time_review_verdict`）、`tests/task_scheduler.rs`（81：`counts_json_of`、`main_task_convergence`）、`tests/sse_business_events.rs`（80：`SubmitOutcome`）、`tests/production_composition.rs`（125：`TaskService`）。
  - 本文件被同域三处文档引用为口径真相源：`tasks/preview.rs`（4）、`tasks/evidence.rs`（252）、`operations/retention.rs`（219）；引用为注释文本，拆分不改路径。

### 1.2 实现区职责锚点（实测行号）

按实测职责切分，本文件真实职责是**任务编排服务**（预览签发、原子提交、取消、时限复核、解除占用）＋其纯策略内核，不含 HTTP 分页/游标（属 06）也不含调度准入（属 `scheduler`）：

- **模块文档与 import** 1–42；**进程级审计序号源** 44–46：`TASK_PROCESS_EPOCH`（`OnceLock<Uuid>`）、`TASK_EVENT_SEQ`（`AtomicU64`），private，与 `audit_events` 的 `(process_epoch, event_seq)` 唯一键同源（44 行文档自述），唯一消费者是 `insert_task_audit`(1,595–1,596)。
- **时间纯策略（RT-N5）** 48–181：`TimeReviewEvidenceKind`(51，serde snake_case，仅记审计)、`TimeReviewDecision`(59)、`time_review_allows_extend`(66)、`QUEUE_TTL_MS`(74，`pub const` 3_600_000)、`queue_deadline_of`(85)、`advanced_reference_utc`(105)、`TimeReviewVerdict`(119，四态)、`time_review_verdict`(137，零 IO)、`parse_created_time_evidence`(179，`pub(crate)`)。
- **取消/终态判定** 183–198：`cancel_precondition`(185，`pub`)、`is_terminal_state`(190，private)。
- **十态频数与主任务收敛** 200–292：`SUB_TASK_STATE_KEYS`(203，private 十元组)、`counts_json_of`(218)、`empty_counts_json`(234)、`submit_counts_json`(239)、`main_task_convergence`(257，注释钉「顺序不可调换」的 04 §4 优先级)。
- **恒定时间比较** 294–304：`constant_time_eq`(295，private)。
- **目标授权三态（RT-N6）** 306–364：`TargetAuthz`(308)、`snapshot_effective_permissions`(320)、`check_target_authz`(346，private 唯一判定实现)、`snapshot_allows_task_creation`(358，`#[cfg(test)]` 薄包装)。
- **契约 DTO 与错误映射** 366–590：`AuthCtx`(369)、`SubmitRequest`(377)、`SubmitOutcome`(390)、`SubmitError`(397)＋`From`×3(408/414/427)＋`into_controller_error`(422)、`CancelRequest`(446)/`CancelOutcome`(453)/`CancelError`(462)＋`From`×3(467/473/479)、`TimeReviewRequest`(490)/`TimeReviewOutcome`(499)/`TimeReviewError`(511)＋`From`×3(518/524/530)、`ReleaseRequest`(543)/`ReleaseLockOutcome`(552)/`ReleaseLockError`(558)＋`From`×3(567/573/579)。全部 `pub`。
- **事务内私有结构** 592–648：`ActorRow`(594)＋`active_admin`(604)/`writable`(609)、`PreviewSnapshotFixed`(616)、`NewSubTask`(628)、`TaskAudit`(640，11 字段审计入参)，均 private。
- **编排入口** 650–953：`TaskService<R: TaskRepository>`(652，`pub repo` 字段)＋`new`(657)＋`preview`(665)＋`submit`(698)＋`submit_in_tx`(721，160 行、步骤 ②–⑩ 编号注释)、`cancel`(883)、`time_review`(906)、`release_lock`(930)。
- **错误构造与频数折叠** 955–970：`preview_ttl_error`(955)、`time_evidence_error`(959)、`tally`(964)。
- **提交事务段** 972–1,218：`read_actor_in_tx`(975)、`lock_preview_in_tx`(996，三重有效性 + 单次 `DELETE` 消费)、`classify_missing_preview`(1,061)、`lock_authz_rows_in_order`(1,083，锁序唯一实现)、`lock_table_in_byte_order`(1,116)、`insert_sub_task`(1,141)、`acquire_device_lock`(1,169，CAS + `generation = 1`)、`mark_sub_task_failed`(1,202)。
- **取消/复核/解除事务段** 1,220–1,567：`cancel_in_tx`(1,220)、`time_review_in_tx`(1,315)、`release_lock_in_tx`(1,442)、`read_sub_task_states`(1,517)、`sync_main_task_summary`(1,540，子任务变更与主行汇总同事务原子回写)。
- **行解码与审计写入** 1,569–1,620：`sub_task_state_from_str`(1,572)、`strict_bool`(1,577)、`redact_reason`(1,586)、`insert_task_audit`(1,591，11 列 INSERT)。

### 1.3 语义不变量清单（逐源码核实，迁移时逐字不变）

C2 口径下迁移 MUST NOT 改动以下逐项事实（均为本会话读源码所得，非惯例推断）：

- **调度 SQL 文本**：全文件 26 处 `tx_execute`/`tx_row`/`tx_rows` 的 SQL 串、`params!` 位置绑定顺序、`revision = revision + 1` 自增位置、`FOR UPDATE` 出现位置逐字不变。锁序固定为 `schema_meta → users → roles → device_groups → devices → device_operation_locks`（模块文档 4–5 行钉死），唯一实现是 `read_actor_in_tx`(975，`users … FOR UPDATE`) → `lock_authz_rows_in_order`(1,083，三段 `BTreeSet` 字节序升序、空集合跳过) → `acquire_device_lock`(1,169，条件插入)。`lock_integrity_guard` 在每个事务体首行调用（728/1,225/1,321/1,447）。事务内零网络（模块文档 3 行）。
- **预览消费纪律**（1,029–1,056）：有效性三重判定顺序固定为 ① 进程代际 `preview_generation` ② 进程内单调 TTL `preview_mono_valid` ③ DB 时钟上限兜底；随后**单次 `DELETE`** 消费，禁 `UPDATE consumed_at` 两段式；`deleted != 1` 才调 `classify_missing_preview`（必须在回滚之前）。
- **时间计算函数**：`queue_deadline_of` 仅 `NtpValid` 累加 `sample_age_ms`（缺失按 0），`SystemFallback`/`Stale` 不得重复累加；溢出 `None` → 调用方 fail-closed（85–97、796）。`advanced_reference_utc` 溢出取 `DateTime::MAX_UTC`，**绝不回退采样时刻**（105–115）。`time_review_verdict` 的四道 fail-closed 顺序：当前时钟非 `NtpValid` → 创建证据缺失 → 创建时钟不可信或纪元不连续 → 按决策比对（137–174）；`queue_deadlines` 中的 `None` 按已过期处置（157）。
- **状态转换顺序**：`submit_in_tx` 步骤 ②`lock_integrity_guard` → ③actor 重验 → ④幂等先查（**先于**消费预览）→ ⑤锁预览 + 摘要恒定时间核验 → ⑥锁授权行 → ⑦主任务先落库（counts 十键全零占位）→ ⑧逐目标重验授权 + 抢设备锁（忙则就地 `failed`，不排队）→ ⑩收敛回写 → ⑨审计（编号即源码顺序，839–877 行的注释编号不得重排）；`cancel_in_tx` 顺序：guard → actor → 子任务 `FOR UPDATE` → 归属校验 → revision → 终态短路 → `cancel_precondition` → CAS 更新 → 删本代际锁 → `sync_main_task_summary` → 审计；`time_review_in_tx`：guard → `active_admin` → 主任务 `FOR UPDATE` → revision → 全部 `held` 子任务按 id 字节序 `FOR UPDATE`（比对依据是 `queue_deadline`，绝不混用 `observe_deadline`）→ 空集合 `InvalidArgument` → `time_review_verdict` 分派 → 逐条 CAS 更新 → **仅 `Expire` 分支**删设备锁 → 汇总回写 → 审计；`release_lock_in_tx`：guard → `active_admin` → 子任务 `FOR UPDATE` → revision → **先真实解析状态**（污染值 503，不当 `unknown` 放行）→ 删本代际锁 → revision CAS → 审计。
- **恢复与幂等语义**：`submit` 幂等命中即 `Replayed` 且零副作用（预览已过期/已消费仍直接重放，743–751）；`Inconclusive` 走**显式回滚**并返回 `Ok(Inconclusive)`（914–917），零状态变更、revision 不动；`Unverifiable`/`Contradiction` 分别落 `Inconclusive`/`InvalidArgument`；`TaskService` 不持连接池、`repo` 是唯一事务入口。
- **审计事件载荷**（11 列 `INSERT`，1,601–1,603）：`id`/`process_epoch`/`event_seq` 全由服务端生成（`Uuid::new_v4`、`TASK_PROCESS_EPOCH.get_or_init`、`crate::db::next_event_seq(&TASK_EVENT_SEQ)`），绝不取请求自报值；`outcome` 恒 `"success"` 且**只在成功分支**写审计（`Inconclusive`/错误分支不写）；`event_type` 四值 `task.submit`(869)/`task.cancel`(1,303)/`task.time_review`(1,428)/`task.release_lock`(1,502)；`time_evidence` 列按调用点分别为 `created_time_json`(874)、`crate::db::system_fallback_time_evidence()`(1,308/1,507)、`serde_json::to_string(clock)`(1,433)；`params_redacted` 中 release_lock 走 `redact_reason` 词数指纹（1,494，`redacted:{词数}`，绝不原样写自由文本）。
- **时钟纪律（RT-N5）**：时限复核只接受服务端 `TimeEvidence`，`evidence_kind` 仅记审计、绝不参与分派（117、132 行）；任何路径不得把 `created_at`/`observe_deadline` 改成更晚的值（`time_review_allows_extend` 只允许延长量 `0`）。

## 2. 目标私有模块树（拟新增文件用 ```` ```text ```` 标注；无文件更名）

采用「根入口 + 同名目录」：`service.rs` 保留为薄根入口（Rust 2018 允许 `tasks/service.rs` 与 `tasks/service/` 并存），**`tasks::service` 对外路径与 `tasks/mod.rs` 的 `pub mod service;` 逐字不变**，避免任何外部更名。

```text
crates/rsetup-controller/src/tasks/
├── service.rs                        （根入口，保留：模块文档（B8 后仅留总述段，三段纪律随所属文件剪切）、子模块声明、§3.1 公开清单逐项 `pub use`/`pub(crate) use`）
└── service/
    ├── contracts.rs                  （§1.2「契约 DTO 与错误映射」366–590 全量 + 955–961 两个错误构造 + 其 3 测试）
    ├── contracts/tests.rs            （`#[cfg(test)]` 宿主，3 例）
    ├── time_policy.rs                （48–181 时间纯策略全量 + 其 6 测试）
    ├── time_policy/tests.rs          （`#[cfg(test)]` 宿主，6 例 + `fixed_instant`/`evidence_at`/`same_epoch_pair`）
    ├── state_rules.rs                （183–198 取消/终态 + 200–292 十态频数与收敛 + `tally` 964 + `sub_task_state_from_str` 1,572 + 其 7 测试）
    ├── state_rules/tests.rs          （`#[cfg(test)]` 宿主，7 例）
    ├── authz_policy.rs               （306–364 授权三态，含 358 的 `#[cfg(test)]` 薄包装 + 4 测试 + policy fixture）
    ├── authz_policy/tests.rs         （`#[cfg(test)]` 宿主，4 例）
    ├── audit.rs                      （44–46 两个 static + 638–648 `TaskAudit` + 1,585–1,620 `redact_reason`/`insert_task_audit` + 其 1 测试）
    ├── audit/tests.rs                （`#[cfg(test)]` 宿主，1 例）
    ├── actor.rs                      （592–612 `ActorRow` 及其两个判定 + 972–993 `read_actor_in_tx` + 1,576–1,583 `strict_bool`）
    ├── tx_submit.rs                  （294–304 `constant_time_eq` + 614–636 两个事务结构 + 721–880 `submit_in_tx` + 995–1,218 七个提交段 helper + 其 1 测试）
    ├── tx_submit/tests.rs            （`#[cfg(test)]` 宿主，1 例）
    ├── tx_mutation.rs                （1,220–1,567 `cancel_in_tx`/`time_review_in_tx`/`release_lock_in_tx`/`read_sub_task_states`/`sync_main_task_summary`；0 测试，见 §4.2）
    ├── orchestration.rs              （650–719 `TaskService`/`new`/`preview`/`submit` + 882–953 `cancel`/`time_review`/`release_lock`）
    └── orchestration/tests.rs         （`#[cfg(test)]` 宿主，5 例 + `FakeRepo` 与其 `impl TaskRepository` + `target`/`submit_request`）
```

行数估算（含各自内联测试）：`tx_submit.rs` ≈ 410 为最大，其余 ≤ 300，根入口 ≈ 130——全部 ≤ 900，**无需 C4 例外说明**。

### 2.1 依赖方向（单向、无新增环）

- **零内部依赖（叶子）**：`time_policy`、`state_rules`、`authz_policy`、`audit`、`actor`。它们只依赖 crate 内既有模块（`error`、`tasks::model`、`tasks::repository` 的行解码件、`tasks::preview`、`time::evidence`、`authz`、`db`）与 crate 外既有依赖，互不回指。
- **契约层**：`contracts → time_policy`（唯一边：`TimeReviewRequest.decision`/`evidence_kind`(493–494) 两个字段引用时间策略的 `TimeReviewDecision`/`TimeReviewEvidenceKind`）；除此之外本层无环。
- **事务层**：`tx_submit → {contracts, time_policy, state_rules, authz_policy, audit, actor}`；`tx_mutation →` 同集合（`time_review_in_tx` 取 `time_policy` 的 `time_review_verdict`/`parse_created_time_evidence`、`state_rules` 的 `sub_task_state_from_str`）。二者互不引用（`submit_in_tx` 不触 `sync_main_task_summary`，`*_in_tx` 不触 `lock_preview_in_tx`，实测消费点闭合）。
- **装配层**：`orchestration → {contracts, tx_submit, tx_mutation}`（实测 `TaskService` 的 5 个方法只消费契约类型与三个 `*_in_tx`，不直接使用策略/审计子模块）。
- 根入口只声明子模块并逐项 re-export，不含逻辑。既有 `service ⇄ tasks::repository`、`service ⇄ tasks::preview` 的双向 import 属 crate 级既有事实，本拆分不新增、不试图解除；`tasks::service` 对 `tasks::http`/`tasks::adapter`/`tasks::scheduler*` **零依赖**（实测 import 清单 20–42 行），故 06/调度侧的拆分不会与本树成环。

## 3. 符号归属与最窄可见性精确表

### 3.1 既有对外可见面：全部原样保留

实测本文件对外可见项共 **29 个 `pub` 顶层项 + 1 个 `pub(crate)` 顶层项**（顶层 `pub` 声明 29 处、`pub(crate)` 声明 1 处；无 `pub use` 再导出、无 `pub type`），迁移后由根入口以**逐项 `pub use`（禁 glob）**挂回 `crate::tasks::service::<名>` 原路径：

`TimeReviewEvidenceKind`、`TimeReviewDecision`、`time_review_allows_extend`、`QUEUE_TTL_MS`、`queue_deadline_of`、`advanced_reference_utc`、`TimeReviewVerdict`、`time_review_verdict`、`cancel_precondition`、`counts_json_of`、`empty_counts_json`、`submit_counts_json`、`main_task_convergence`、`TargetAuthz`、`snapshot_effective_permissions`、`AuthCtx`、`SubmitRequest`、`SubmitOutcome`、`SubmitError`、`CancelRequest`、`CancelOutcome`、`CancelError`、`TimeReviewRequest`、`TimeReviewOutcome`、`TimeReviewError`、`ReleaseRequest`、`ReleaseLockOutcome`、`ReleaseLockError`、`TaskService`（及 `TaskService::new`/`preview`/`submit`/`cancel`/`time_review`/`release_lock`/`into_controller_error` 方法与 `pub repo` 字段）；`pub(crate)`：`parse_created_time_evidence`。DTO 的 `pub` 字段（`AuthCtx.user_id`/`is_admin`、`SubmitRequest` 六字段、`CancelRequest`/`TimeReviewRequest`/`ReleaseRequest` 各字段）可见性一律不变（adapter 以字面量构造）。

- 其中 5 个 `pub` 项实测**无 crate 内跨文件消费者**（`time_review_allows_extend`(66) 与 `submit_counts_json`(239) 仅本文件测试消费、`advanced_reference_utc`(105) 另有本文件内 156 行调用、`cancel_precondition`(185) 另有本文件内 1,268 行调用、`TargetAuthz`(308) 仅 `check_target_authz` 与内部路径使用）：按 C2 不降档、原样保持 `pub`，登记入 §6 已知观察。
- `#[allow(clippy::manual_async_fn)]`(1,640) 随 `FakeRepo` 的 `impl TaskRepository` 原样迁移。

### 3.2 逐项归属表（现可见性 → 目标文件 → 新可见性）

| 符号（现行号） | 归属文件 | 新可见性与最窄性论证 |
| --- | --- | --- |
| 29 个 `pub` 项 + `pub(crate)` 1 项（§3.1 清单） | contracts / time_policy / state_rules / authz_policy / orchestration | 可见性**原样不变**，根逐项 `pub use` 回挂；§1.1 消费面逐文件路径不变 |
| `TASK_PROCESS_EPOCH`(45)、`TASK_EVENT_SEQ`(46)、`SUB_TASK_STATE_KEYS`(203)、`PreviewSnapshotFixed`(616)、`NewSubTask`(628)、`constant_time_eq`(295)、`strict_bool`(1,577)、`snapshot_allows_task_creation`(358) | 见 §2 树（各自唯一消费点所在文件） | **保持 private**：消费点全部同文件（`TASK_PROCESS_EPOCH`/`TASK_EVENT_SEQ` 只被同文件的 `insert_task_audit`(1,595–1,596) 读；`constant_time_eq` 消费 746/759/1,014 全在 `tx_submit.rs`；`PreviewSnapshotFixed`/`NewSubTask` 的构造与读取同在 `tx_submit.rs`；`strict_bool` 仅 `read_actor_in_tx`(989–991) 消费）。`SUB_TASK_STATE_KEYS`(224) 与 `snapshot_allows_task_creation` 的额外消费者是各自宿主文件的**子**测试模块（`state_rules/tests.rs` 1,893、`authz_policy/tests.rs` 2,533 起），父模块 private 对子模块可达，无需升档 |
| `lock_preview_in_tx`(996)、`classify_missing_preview`(1,061)、`lock_authz_rows_in_order`(1,083)、`lock_table_in_byte_order`(1,116)、`insert_sub_task`(1,141)、`acquire_device_lock`(1,169)、`mark_sub_task_failed`(1,202) | tx_submit.rs | **保持 private**：全部消费点在 `submit_in_tx` 或本文件内（754/1,010/1,055/765/1,104–1,107/808/826/830） |
| `read_sub_task_states`(1,517)、`sync_main_task_summary`(1,540) | tx_mutation.rs | **保持 private**：消费点 1,298/1,417/1,545 同文件 |
| `tally`(964) | state_rules.rs | `private → pub(super)`：消费 841（tx_submit）、1,557（tx_mutation）与本文件测试；跨兄弟文件消费，`pub(super)` 即最窄 |
| `is_terminal_state`(190) | state_rules.rs | `private → pub(super)`：消费 1,254（tx_mutation） |
| `sub_task_state_from_str`(1,572) | state_rules.rs | `private → pub(super)`：消费 1,253/1,464/1,529（tx_mutation）与本文件测试 |
| `check_target_authz`(346) | authz_policy.rs | `private → pub(super)`：消费 799（tx_submit）、1,265（tx_mutation）；提交与取消两条线的**唯一**授权判定实现 |
| `preview_ttl_error`(955) | contracts.rs | `private → pub(super)`：消费 679/680（orchestration 的 `preview`） |
| `time_evidence_error`(959) | contracts.rs | `private → pub(super)`：消费 770/796（tx_submit）、1,433（tx_mutation） |
| `TaskAudit`(640) 及其 7 个字段 | audit.rs | `private → pub(super)`（含 7 字段逐个同档）：构造点 867（tx_submit）、1,301/1,426/1,500（tx_mutation）需跨文件构造，字段若不升档则结构体字面量不可见 |
| `insert_task_audit`(1,591) | audit.rs | `private → pub(super)`：消费 865/1,299/1,424/1,498 |
| `redact_reason`(1,586) | audit.rs | `private → pub(super)`：消费 1,494（tx_mutation） |
| `ActorRow`(594) 与 `is_admin` 字段(596)、`active_admin`(604)、`writable`(609) | actor.rs | `private → pub(super)`（上述 4 项逐个同档）：`active`/`must_change_password` 两字段**保持 private**（仅本文件与构造处可见，跨文件只读 `is_admin`，实测消费 1,232） |
| `read_actor_in_tx`(975) | actor.rs | `private → pub(super)`：消费 731/1,228/1,323/1,449 |
| `submit_in_tx`(721，方法) | tx_submit.rs | `private → pub(super)`：定义随固有 impl 块迁入 `tx_submit`，消费在 `orchestration` 的 `submit`(707) |
| `cancel_in_tx`(1,220)、`time_review_in_tx`(1,315)、`release_lock_in_tx`(1,442) | tx_mutation.rs | `private → pub(super)`：消费分别在 `orchestration` 的 `cancel`(889)/`time_review`(913)/`release_lock`(942) |

**可见性变更清单（C1 全量）恰为上表 25 项 `private → pub(super)`**：`tally`、`is_terminal_state`、`sub_task_state_from_str`、`check_target_authz`、`preview_ttl_error`、`time_evidence_error`、`TaskAudit`＋7 字段（8 项）、`insert_task_audit`、`redact_reason`、`ActorRow`＋`is_admin` 字段＋`active_admin`＋`writable`（4 项）、`read_actor_in_tx`、`submit_in_tx`、`cancel_in_tx`、`time_review_in_tx`、`release_lock_in_tx`。**等值论证**：这些符号现状是「文件内可见」，拆分后其唯一/全部消费点已按 §2 分布到兄弟文件，`pub(super)` 恰好覆盖「`tasks::service` 子树内可见」，与拆分前的实际可达集**完全相同**；crate 其他位置仍不可见，无一升级为 `pub(crate)`，更无新增 `pub`。该口径与已定稿的 [06-tasks-http.md](06-tasks-http.md) §3（6 项 `private → pub(super)`，同样的等值论证）一致。

### 3.3 impl 块归属（trait impl 不可拆，固有 impl 可分）

- **trait impl：生产代码恰 12 个，必须整块迁移**：`From<ControllerError>`/`From<sqlx::Error>`/`From<自身> for ControllerError` 四组各 3 个（`SubmitError`/`CancelError`/`TimeReviewError`/`ReleaseLockError`，408–442、467–486、518–539、567–590）全部随 `contracts.rs`；测试内另有 1 个 `impl TaskRepository for FakeRepo`(1,641) 整块随 `orchestration` 测试。同一 trait 对同一类型的 impl 不得拆到两个文件，本设计未做任何此类拆分。
- **固有 impl 恰 3 个（420、600、656），允许分块**：`impl SubmitError`(420，仅 `into_controller_error`) 随 `contracts.rs`；`impl ActorRow`(600) 随 `actor.rs`；`impl<R: TaskRepository> TaskService<R>`(656) 拆两块——公开入口与 `preview`/`submit`/`cancel`/`time_review`/`release_lock` 在 `orchestration.rs`，`submit_in_tx` 在 `tx_submit.rs`（同一 `impl` 头的两个块，方法可见性各自登记）。

## 4. 测试与 fixture 迁移（27 例逐文件映射，函数名一个不改）

### 4.1 精确计数与守恒式

`mod tests`(1,622–2,462) 的 22 例 + `mod policy_tests`(2,464–2,595) 的 5 例 = **27 例**；迁移后按目标宿主模块标称分布为 **contracts 3 + time_policy 6 + state_rules 7 + authz_policy 4 + audit 1 + tx_submit 1 + tx_mutation 0 + orchestration 5 = 27**，两侧清单 diff 必须为空（§7）。

### 4.2 逐宿主文件名单（标称 = 名单，逐文件相等）

| 新宿主（`#[cfg(test)]`） | 例数 | 测试函数名（现行号） |
| --- | --- | --- |
| `service/contracts/tests.rs` | 3 | `submit_errors_map_to_contract_codes`(1,822)、`sqlx_errors_are_wrapped_as_backend_database`(1,855)、`submit_error_backend_maps_through_to_same_controller_error`(2,584，原属 `policy_tests`) |
| `service/time_policy/tests.rs` | 6 | `time_review_never_extends_deadline`(1,775)、`queue_deadline_uses_trusted_time_base_and_sample_age`(2,078)、`time_review_resumes_only_when_every_queue_deadline_is_live`(2,103)、`time_review_fails_closed_on_untrusted_or_discontinuous_clock`(2,172)、`stale_sample_clock_cannot_keep_an_expired_task_alive`(2,228)、`created_time_evidence_parser_is_fail_closed`(2,268) |
| `service/state_rules/tests.rs` | 7 | `cancel_is_only_for_queued_or_held`(1,805)、`counts_json_is_ten_string_keys_and_sums_batches`(1,868)、`state_keys_match_serde_wire`(1,893)、`main_task_convergence_follows_sub_task_distribution`(1,900)、`main_task_convergence_priority_follows_spec_04`(1,971)、`every_created_sub_task_is_counted_including_visible_forbidden`(2,040)、`sub_task_state_parser_rejects_polluted_values`(2,299) |
| `service/authz_policy/tests.rs` | 4 | `snapshot_allows_task_creation_accepts_reboot_only_grant`(2,524)、`snapshot_allows_task_creation_rejects_task_read_only_grant`(2,537)、`snapshot_allows_task_creation_rejects_inactive_or_must_change_actor`(2,551)、`snapshot_allows_task_creation_rejects_missing_or_archived_device`(2,572) |
| `service/audit/tests.rs` | 1 | `audit_reason_is_never_echoed_verbatim`(2,291) |
| `service/tx_submit/tests.rs` | 1 | `constant_time_eq_is_length_and_content_sensitive`(2,283) |
| `service/tx_mutation.rs` | 0 | 无内联单测：三个 `*_in_tx` 的 SQL/锁序/幂等语义由真库轨道 `tests/task_scheduler_sqlx.rs` 与 `tests/` 集成测试覆盖（§7），本拆分**不新建**其断言 |
| `service/orchestration/tests.rs` | 5 | `preview_derives_hash_and_nonce_from_the_returned_token`(2,317)、`preview_rejects_empty_and_oversized_batches_without_writing`(2,357)、`submit_rejects_non_canonical_idempotency_key_before_any_transaction`(2,384)、`submit_propagates_storage_faults_as_backend_not_400`(2,407)、`release_lock_validates_acknowledgement_before_any_transaction`(2,434)（全部 5 个 `#[tokio::test]`） |

- 归属原则（C3）：断言与被测行为同时位。`sub_task_state_parser_rejects_polluted_values`(2,299) 测 `sub_task_state_from_str`、`audit_reason_is_never_echoed_verbatim`(2,291) 测 `redact_reason`、`constant_time_eq_is_length_and_content_sensitive`(2,283) 测 `constant_time_eq`——分别随被测函数落 `state_rules`/`audit`/`tx_submit`，**不留巨型 `tests.rs`**。
- 5 个 `#[tokio::test]` 全部只经 `TaskService` 公开入口（`FakeRepo.begin` 一律返回 `Database(RowNotFound)` 短路，1,642–1,739），故归 `orchestration`。

### 4.3 fixture 与 test-only 设施：一律与首消费者同批

- **test-only 函数 `snapshot_allows_task_creation`(358)**：MUST 保持 `#[cfg(test)]` + private，迁入 `authz_policy.rs`，与唯一消费者 `authz_policy/tests.rs`（原 `policy_tests`）**同一批次 B4** 落地；先迁消费者会让 `policy_tests` 引用不到包装（编译断），先迁包装则出现 `dead_code`——因此 B4 内「包装 + 4 例 + fixture」一次成组，不得拆两个提交。
- **时间夹具** `fixed_instant`(1,939)/`evidence_at`(1,943)/`same_epoch_pair`(1,961)：实测消费者全部在时间 6 例（1,963/1,964/2,079/2,082/2,086/2,092/2,106/2,177/2,233/2,269），随 B2 入 `time_policy/tests.rs` 并保持 private。
- **policy fixture** `POLICY_DEVICE`(2,475)/`ACTIVE_USER`(2,476)/`policy_device_fixture`(2,483)/`policy_snapshot`(2,495)/`policy_direct_grant`(2,513) 与 `policy_tests` 的 `use` 头（2,466–2,473）：随 B4 入 `authz_policy/tests.rs`，成员保持文件内 private（无跨宿主消费者）。
- **fake 设施** `RecordedPreview`(1,630)/`FakeRepo`(1,634)/`target`(1,741)/`submit_request`(1,753)：仅 5 个 `#[tokio::test]` 消费，随 B7 入 `orchestration/tests.rs`（与其唯一消费者同批，`FakeRepo` 的 `impl TaskRepository` 整块迁移），内部 private 即可满足同文件访问，**MUST NOT** 外迁为 `tests/` 集成测试（C3 反模式）。
- 本文件的 fixture 消费者与定义**天然同宿主**（跨宿主共享 0 项），故不存在 06 那类「fixture 首建早于消费者」的三段时序；每批建立即自足，回滚任意一批不留悬空引用。
- 测试模块路径由 `tasks::service::tests::<名>`/`tasks::service::policy_tests::<名>` 变为 `tasks::service::<宿主>::tests::<名>`，过滤器 `tasks::service` 仍全量命中（§7 以清单 diff 守恒核对）。

## 5. 分批顺序、暂留位置与回滚

每批 = 一个可独立回滚提交（提交标题中文，[AGENTS.md](../../../../AGENTS.md)）；**「每批可编译」是设计断言**，本规格会话未编译验证。B5 依赖 B1–B4，B6 依赖 B5，B7/B8 依赖全部；B1–B4 之间无依赖、可并行或任意先后。

| 批次 | 内容 | 暂留与回改 |
| --- | --- | --- |
| B1 | `contracts.rs`（366–590 + 955–961）+ 其 3 测试；根加 `mod contracts;` 与 13 名逐项 `pub use` | 其余实现暂留根文件，经根内 `use contracts::...`（过渡形态，B8 收敛）；`preview_ttl_error`/`time_evidence_error` 本批升 `pub(super)` |
| B2 | `time_policy.rs`（48–181）+ 6 测试 + 3 时间夹具 | 根 `pub use` 追加 9 名（含 `pub(crate) use parse_created_time_evidence`）；未迁实现继续走根作用域 |
| B3 | `state_rules.rs`（183–292 + `tally` + `sub_task_state_from_str`）+ 7 测试 | 根 `pub use` 追加 5 名；`tally`/`is_terminal_state`/`sub_task_state_from_str` 本批升 `pub(super)`，根内暂留实现改用 `state_rules::…` 显式路径 |
| B4 | `authz_policy.rs`（306–364，含 `#[cfg(test)]` 包装）+ `authz_policy/tests.rs`（4 例 + policy fixture） | 根 `pub use` 追加 2 名；包装与其 4 例同批（§4.3） |
| B5 | `audit.rs`（44–46、638–648、1,585–1,620）+ `actor.rs`（592–612、972–993、1,576–1,583）+ `audit/tests.rs` 1 例 | 两文件零内部依赖，可与 B1–B4 并行；`TaskAudit` 及字段、`ActorRow` 相关 4 项、`insert_task_audit`/`redact_reason`/`read_actor_in_tx` 本批升 `pub(super)` |
| B6 | `tx_submit.rs` + `tx_mutation.rs` + `tx_submit/tests.rs` 1 例 | 4 个 `*_in_tx` 本批升 `pub(super)`；根内暂留的 `submit`/`cancel`/`time_review`/`release_lock` 经 `tx_submit::…`/`tx_mutation::…` 调用（B7 收敛） |
| B7 | `orchestration.rs`（`TaskService` 两块 impl 的公开入口）+ `orchestration/tests.rs`（5 例 + `FakeRepo` + 2 helper）；删除根内 `mod tests`/`mod policy_tests` | 至此测试区清空；根 `pub use` 追加 `TaskService` 1 名；未迁的 `pub use` 清单不得提前删除 |
| B8 | 根入口清理：过渡 `use` 收敛为最终 import 表、§3.1 清单逐项核对、模块文档 1–18 段按主题剪切（事务纪律/锁序段 → `tx_submit.rs`，时钟纪律与两条期限段 → `time_policy.rs`，行读取口径段 → `tx_submit.rs`/`tx_mutation.rs`，文字不改）；核对 §2.1 四条 intra-doc 链接（`queue_deadline_of`、`tx_rows`、`tx_row`、`column`）随段落仍可解析 | 无代码移动；文档段落与代码同批，避免「文档描述已迁走、正文还在根」 |

回滚粒度：任一批 `revert` 即回到上一批可编译形态；B1–B4、B5 互不依赖可独立回滚；B6 依赖 B5 的 `actor`/`audit`（回滚 B5 必须先回滚 B6）；B7/B8 依赖全部。不存在跨批「半成品」测试：每个测试与其被测函数与所需 fixture 总在同批内成对出现。

## 6. 风险、例外与边界

- **C4 阈值**：估算最大目标文件 `tx_submit.rs` ≈ 410 行、根入口 ≈ 130 行、其余 ≤ 300 行，全部 ≤ 900，**无需例外说明**（实施时复核）。
- **状态机与并发/锁语义（本文件最大风险，总览 §4 登记项）**：§1.3 的锁序、`FOR UPDATE` 位置、CAS 条件（`revision = ?`、`generation = 1`、`deleted != 1`）与「先幂等后消费预览」「`Inconclusive` 显式回滚」「仅 `Expire` 分支删锁」是本文件的行为契约；迁移只随代码移动，逐行不改。回归哨兵：`every_created_sub_task_is_counted_including_visible_forbidden`(2,040)、`time_review_resumes_only_when_every_queue_deadline_is_live`(2,103)、`submit_rejects_non_canonical_idempotency_key_before_any_transaction`(2,384)、`release_lock_validates_acknowledgement_before_any_transaction`(2,434) 四例钉住「计数一一对应」「时钟只恢复不延长」「事务开启前的参数校验」三处最易被顺手改坏的语义。
- **审计事件仅成功分支**：4 处 `insert_task_audit` 调用点（865/1,299/1,424/1,498）均在成功路径、`outcome` 恒 `"success"`；`time_review` 的 `Inconclusive` 分支在审计之前 `return`（1,369），迁移不得把审计写入提到分支之前或改成失败也写。
- **SQL 文本零 diff 的核对方式**：`tx_submit.rs`/`tx_mutation.rs`/`actor.rs`/`audit.rs` 落地后，用 `git diff -U0` 逐 SQL 串核对（26 处 `tx_*` 调用），并要求 `SELECT`/`INSERT`/`UPDATE`/`DELETE` 文本与 `params!` 实参顺序成对未变。
- **`#[cfg(test)]` 薄包装**（358）：它是本文件唯一的 test-only 顶层函数，拆分期间若与消费者分批即编译断；`#[cfg(test)]` 属性与 private 可见性均不得改动（改成 `pub` 或 `pub(crate)` 会把测试专用面泄漏到生产面）。
- **既有可见性事实不得「顺手修」**：§3.1 登记的 5 个无跨文件消费者 `pub` 项、`pub` DTO 字段、`TaskService.repo` 公开字段一律原样（降档属 C2 违例）；`tests/time_consumption.rs`、`tests/task_scheduler.rs`、`tests/sse_business_events.rs`、`tests/production_composition.rs` 四个集成测试对 `tasks::service::*` 的 import 行必须零修改，作为「公开面守恒」的编译级证据。
- **不新增依赖**：`chrono`/`sqlx`/`uuid`/`serde_json` 均为既有；不动 `Cargo.toml`/`Cargo.lock`/CI/Makefile。本文件不被任何静态门禁按路径 grep（实测 [Makefile](../../../../Makefile) `metrics-static-gates` 五条 grep 目标为 `src/metrics*` 与 `src/main.rs`），故无总览 §6.1 的 F8 条目需求。
- **与兄弟模块文件的关系**：[tasks/adapter.rs](../../../../crates/rsetup-controller/src/tasks/adapter.rs)、[tasks/scheduler_store.rs](../../../../crates/rsetup-controller/src/tasks/scheduler_store.rs)、[production.rs](../../../../crates/rsetup-controller/src/production.rs) 是兄弟/上游模块，只**消费** service 符号（`production.rs` 实测不 import service，仅装配 adapter 与 repository）。本拆分不改这三个文件的任何一行；它们对 service 符号的全部引用路径（§1.1 清单）经根 re-export 保持可达。

## 7. 验证命令（登记项，本轮一律未执行、不声称通过）

前提 F2：先 `make web-dist-check`（或 `make web-build`）备妥 `apps/controller-web/dist`；随后：

```bash
make fmt-check
cargo clippy -p rsetup-controller --all-targets -- -D warnings
cargo test -p rsetup-controller --locked
cargo test -p rsetup-controller --locked -- tasks::service --list   # --locked 在 `--` 前，过滤在 `--` 后
make controller-test-offline                                        # 全量出口（F2/F3）
```

- 测试映射守恒核对：拆分前后各执行一次 `cargo test -p rsetup-controller --locked -- tasks::service --list`，两侧 27 个测试名清单 diff 必须为空（现路径为 `tasks::service::tests::` 下 22 例（17 同步 + 5 async）与 `tasks::service::policy_tests::` 下 5 例；拆分后为 §4.2 的 7 个宿主模块路径，函数名逐字不变）。
- 公开面守恒核对（编译即证据，不改测试）：`cargo test -p rsetup-controller --locked --test time_consumption --test task_scheduler --test sse_business_events --test production_composition` 的 import 行零修改即通过。
- 上述命令全部走既有入口（F8 不新增 Makefile 目标或 CI 接线）；clippy 与 test 均承担编译校验（F4）；格式判定用 `make fmt-check` 而非 `cargo fmt --all`（F1，[AGENTS.md](../../../../AGENTS.md)「Rust 格式检查口径」）。
- **真库轨道仅登记、未验证（F6）**：`crates/rsetup-controller/tests/task_scheduler_sqlx.rs`（`--ignored`，经 `scheduler_store` 覆盖本文件的 `counts_json_of`/`main_task_convergence`/`snapshot_effective_permissions` 真实 SQL 路径）及 `tests/v4_baseline.rs` 等需按引擎逐条授权专用开发库（[AGENTS.md](../../../../AGENTS.md)「本地开发授权与凭据保护」）；实施验收时另行执行，不进本规格默认门禁。本会话未触库、未读 `secret/`。

## 8. DoD（逐条自检，缺一即不成立）

1. **C1**：`tasks/mod.rs` 一行不改；§1.1 消费面（`repository`/`adapter`/`http`/`scheduler_store`/`evidence`/`main`/`integrity` + 4 个集成测试）路径与 `pub`/`pub(crate)` 面逐项比对零变更；§3.1 的 29+1 清单在根入口逐项可 `pub use`、无 glob、无新增 `pub`；可见性变更恰为 §3.2 的 25 项 `private → pub(super)`（含 `TaskAudit` 7 字段与 `ActorRow` 4 项逐个登记），且 `TaskAudit` 之外的 private 结构体字段未被动过。
2. **C2**：`git diff` 审查确认改动仅限移动 + `mod`/`use`/`pub use`；§1.3 逐项不变——26 处 SQL 文本与绑定顺序、锁序与 `FOR UPDATE`/CAS 条件、5 条状态转换顺序、幂等与 `Inconclusive` 回滚语义、4 处审计载荷与「仅成功分支」、时间计算函数（含溢出与 fail-closed 分支）全部零变更。
3. **C3**：27 个测试名守恒（§7 清单 diff 为空）、断言零删改、无测试外迁 `tests/`；test-only 件（358 包装、`FakeRepo`、时间/policy fixture）全部 `#[cfg(test)]` 且与首消费者同批。
4. **C4**：目标文件全部 ≤ 900 行（§6 估算，实施时复核），无例外。
5. **C5/C6**：不适用（非 ui、未触碰排除区）；**C7**：本文档无凭据、未读 `secret/`、未触库。
6. **C8/C9**：本文件八项内容齐备、路径为总览 §4 固定路径；对 06 的边界引用成立（§9），并明确 12 先于 06 实施（总览 §5.2 批次 D）。
7. §7 命令按前提执行并如实报告（含未执行项）；§5 逐批回滚可用（B6 回滚须连带 B7/B8，回滚 B5 前须先回滚 B6）。

## 9. 与总览、06 号的交叉一致与工作树漂移登记

- **实施顺序**：总览 §5.2 批次 D 规定 `12 → 06` 同域串行、先 12 后 06 实施。本规格边界即 §2/§3，06 实施时**不得**再改动 `crates/rsetup-controller/src/tasks/service*.rs`；若 06 对边界提出新需求，按总览 §5.3 先裁决再动，禁止并发触碰同一文件。
- **对 06 依赖面的核实（不要求 06 返工）**：06 §1.1 的消费面登记均针对 `tasks::http` 自身符号（`HttpTasksState` 四 `pub` 字段、`build_tasks_router`、`require_business_session`、adapter 的 22+1 名清单），**不含**本文件符号；两文件之间唯一真实符号耦合是 [tasks/http.rs](../../../../crates/rsetup-controller/src/tasks/http.rs) 第 50 行 `use crate::tasks::service::{SubmitOutcome, TimeReviewEvidenceKind};`（用于 201/260/980 行）。本设计把 `SubmitOutcome` 归 `contracts.rs`、`TimeReviewEvidenceKind` 归 `time_policy.rs` 并在根逐项 re-export，`crate::tasks::service::SubmitOutcome` 与 `crate::tasks::service::TimeReviewEvidenceKind` 两路径原样可用，且 `http.rs` 的这行 import 在 06 的设计里保留在根入口——故 06 的文件归属、批次与可见性结论均无需调整。
- **方向性事实更正登记**：06 §2 末段「`http` 子树对 `tasks::service` 零依赖」在**新增子文件**口径下成立，但 `http` 根文件保留 import（上条），两符号方向是 `http → service`。本拆分保持该方向不反转：本树对 `tasks::http`/`tasks::adapter` 零依赖（import 清单 20–42 行实测），不产生新环。
- **他人未提交改动的漂移**：撰写时工作树 [tasks/scheduler.rs](../../../../crates/rsetup-controller/src/tasks/scheduler.rs)（+9/−7）与 [tasks/scheduler_store.rs](../../../../crates/rsetup-controller/src/tasks/scheduler_store.rs)（+83/−7）在改。实测 `git diff` 显示 scheduler 侧改动新增了对 `crate::tasks::service::snapshot_effective_permissions` 的复用（HEAD 版 26 行 import 两名 → 工作树 30 行 import 三名，新增 `snapshot_effective_permissions`），即**调度侧新增了对本文件公开面的消费者**；同时 [tasks/scheduler.rs](../../../../crates/rsetup-controller/src/tasks/scheduler.rs) 的文档改动声明「`has_reboot` 与提交侧授权判定同源」。据此本规格把「提交与调度共用同一授权判定实现」列为 §3.2 的守恒项（`check_target_authz`/`snapshot_effective_permissions` 的唯一实现不得因拆分出现第二份），并使 §1.1 消费面登记以**工作树**为准。
- **锚点漂移处置**：本文件自身在头提交上无未提交改动，本规格全部行号即当前工作树实测值；实施开始前按总览 §3 重新核对当时工作树，若 `service.rs` 已被他人改动则先更新本规格锚点再动代码。

## 10. 修订记录（独立审查后定点修订，逐项已修订，待复核，不构成 PASS 结论）

本轮（2026-10-26；上轮静态审查 PASS（有条件），修 7 项 Minor 笔误，每处改前经当时工作树 read/grep 源码复核）：

- **M1**（§3.1）：「`SubmitRequest` 五字段」更正为**六字段**——实测 `pub` 字段 6 个（378/379/381/382/383/385：`idempotency_key`、`request_hash`、`preview_token_hash`、`preview_token_nonce`、`observe_deadline`、`time_evidence`）。
- **M2**（§2 树 `tx_submit.rs` 行）：「995–1,218 五个提交段 helper」更正为**七个**——区间内函数定义 996/1,061/1,083/1,116/1,141/1,169/1,202 共 7 个，与 §1.2 及 §3.2 表 7 项一致。
- **M3**（§1.3 状态转换顺序）：「946–877 行的注释编号不得重排」更正为 **839–877 行**——步骤 ⑩ 收敛回写在 839、⑨ 审计在 858、审计写入止于 877；946 不在 `submit_in_tx`(721–880) 范围内。
- **M4**（§1.3 审计事件载荷）：「`event_type` 三值」更正为**四值**——其后枚举的 `task.submit`(869)/`task.cancel`(1,303)/`task.time_review`(1,428)/`task.release_lock`(1,502) 恰为 4 项（实测全文件 `event_type: "task.` 共 4 处）。
- **M5**（§6）：「无总览 §6.4 门禁演进链路需求」更正为「无总览 **§6.1 的 F8 条目**需求」——总览 §6 仅有 6.1/6.2/6.3，无 §6.4；门禁演进四步链路（spec → Makefile → CI → 合同测试）登记于 §6.1 的 F8 条目。
- **M6**（§1.1 `scheduler.rs` 消费面行）：「61 行另有同名私有口径」更正为「**61/63 行另有调度域同名独立常量**」——两常量实均为 `pub const`（`QUEUE_TTL_MS` 61 行、`OBSERVE_WINDOW_MS` 63 行），原「私有口径」表述会误读为可见性事实。
- **M7**（§5 批次表 B7 行）：暂留与回改列补「**根 `pub use` 追加 `TaskService` 1 名**」——`TaskService` 是 §3.1 清单第 29 个 `pub` 项（B1 13 名 + B2 9 名 + B3 5 名 + B4 2 名后仅剩此项），与 B1–B4 明列口径一致。

本轮均为文档级笔误修订：主体结构、符号归属、可见性变更清单、批次骨架与 27 测试映射不变。

批次1实施登记（已修订，2026-10-27，提交eff0efa；审查PASS——锚点重采：规格基线c7ff640与实施基线eff0efa^间service.rs零漂移，规格行号锚点全部有效（旧2595行）；契约DTO段366–590与错误构造955–961逐字迁入新建service/contracts.rs（250行：模块文档5+use头7+正文225+cfg(test)声明2）与service/contracts/tests.rs（61行）；根入口pub use contracts恰13名逐项再导出，无glob、零新增pub，8处消费面（repository/adapter/http/main/4个集成测试）import零改动；可见性变更恰为preview_ttl_error/time_evidence_error两项private→pub(super)（§3.2清单6/7项），根以use contracts::{...}过渡引用（B8收敛）；27测试严格守恒：contracts::tests 3/根tests 20/policy_tests 4，函数名与断言逐字不变，policy_tests use头同步收窄2个孤儿import（必要适配）；12个trait impl（四组错误各3个From）与固有impl into_controller_error整块随contracts.rs、无跨文件拆分（§3.3）；contracts.rs新增5行模块文档（新建文件必要说明，与被移代码语义一致，登记）；contracts.rs经根路径回引TimeReviewDecision/TimeReviewEvidenceKind为B1过渡形态（B2迁time_policy后改直接路径）；doc既有2条private链接告警（[tx_rows]/[tx_row]）移交B8核对；验证五条全PASS（fmt-check/clippy -D warnings/test全量/tasks::service 27绿+--list清单diff为空/rustdoc -D broken_intra_doc_links）；tasks/mod.rs零改动）

批次2实施登记（2026-10-28，提交b9ef407；审查PASS——锚点位移：B1后根文件58–191（规格锚点48–181，+10），与实施自述一致；策略段+6测试+3夹具逐字迁入新建service/time_policy.rs（148行）与time_policy/tests.rs（272行，夹具保持private），根service.rs 2311→1916；根pub use追加恰9名（8 pub+pub(crate) parse_created_time_evidence），无glob、零新增pub、零可见性升降；消费面http.rs/time_consumption.rs/scheduler_store.rs/evidence.rs/repository.rs/adapter.rs/main.rs零改动，counts_json_of/main_task_convergence/snapshot_effective_permissions仍在根；TimeQuality导入处置＝符号迁走后根实现区零消费者的必要清理（clippy -D warnings强制适配）+根tests内1行按需导入（残留消费者），仅限use行/测试内；contracts.rs过渡回引改time_policy直接路径恰1行（批次1登记的预期B2适配），250行不变；27测试严格守恒：time_policy::tests 6/contracts::tests 3/根tests 14/根policy_tests 4，函数名与断言零改动，snapshot_allows_task_creation薄包装未动；2项minor登记：time_policy.rs derive行serde路径缩短（serde::Serialize→Serialize，经新文件use头解析语义等价、serde snake_case属性零改写）、queue_deadline测试1处rustfmt断行重排（缩进列宽变化、断言零变化）；RT-N5不变量逐字核对在位（queue_deadline_of仅NtpValid累加/advanced_reference_utc溢出MAX_UTC/time_review_verdict四道fail-closed顺序/evidence_kind仅记审计不参与分派）；验证五条全PASS（fmt-check/clippy -D warnings/test全量34 target 1545绿/tasks::service 27绿+--list分布吻合/rustdoc -D broken_intra_doc_links）；tasks/mod.rs零改动）


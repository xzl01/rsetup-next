# 任务执行与结果持久化闭环实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 完成中控任务调度生产接入、任务执行与结果证据合并持久化、板端结果事件触发与中控结果 Sink、设备重连后的只读核验闭环。

**Architecture:** 
1. 建立 `TaskEvidenceApplier` 作为全局单一事实源服务，负责 `TaskRecord` 契约校验、子任务状态机推进、设备锁释放/保留、主任务汇总收敛与 SSE 发布；
2. 建立 `SqlxSchedulerStore` 实现 `SchedulerStore` 生产适配，并在 `TaskScheduler` 中接入排队超时收敛、prepare 失败处理与 execute 结果持久化；在 `main.rs` 启动调度后台循环；
3. 建立 `SqlxTaskResultSink`，基于本地当前认证会话代际上下文核验后调用证据应用器；板端在 `execute` 持久写后异步触发 `device.task.result`；
4. 实现设备重连后未决任务的主动只读核验与观察窗口超时兜底核验。

**Tech Stack:** Rust, Tokio, SQLx, MySQL/TiDB (v4 schema), ed25519, SHA256, serde/serde_json, Axum.

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`

## Global Constraints

- 不实现、不启用、不调用真实主机 reboot；HostRebootOs 保持 Unsupported。
- 绝不自动 commit、push、reset、stash、clean。
- 网络等待永不持有数据库事务或行锁；短事务内完成 CAS 与状态更新。
- 格式检查严格执行 `make fmt-check`。
- 测试必须真实执行，不把过滤到 0 用例当通过。

---

### Task 1: 任务证据合并与状态/锁/汇总持久化服务 (TaskEvidenceApplier)

**Files:**
- Create: `crates/rsetup-controller/src/tasks/evidence.rs`
- Modify: `crates/rsetup-controller/src/tasks/mod.rs`
- Test: `crates/rsetup-controller/tests/task_evidence.rs`

**Interfaces:**
- Consumes:
  - `TaskRecord` from `rsetup_protocol::wire`
  - `TaskRepository`, `DbPool`
  - `EventBus`
- Produces:
  - `TaskEvidenceApplier` trait and `SqlxTaskEvidenceApplier`
  - `fn apply_task_record(&self, device_id: &[u8; 32], record: &TaskRecord) -> Result<ApplyOutcome, ControllerError>`

- [ ] **Step 1: 编写失败测试**
在 `crates/rsetup-controller/tests/task_evidence.rs` 中编写测试：
- 契约校验：sub_task_id 匹配、pre_boot_id 匹配、journal_epoch 非 nil、record_version 递增；
- 状态机转移：queued/dispatching -> accepted -> verifying -> succeeded/failed/unknown；
- 锁生命周期：终态释放设备锁，unknown 保留设备锁；
- 主任务汇总更新：重算主任务状态与 outcome。

- [ ] **Step 2: 运行测试验证失败**
`cargo test --test task_evidence`，预期编译失败（模块不存在）。

- [ ] **Step 3: 实现 TaskEvidenceApplier**
在 `crates/rsetup-controller/src/tasks/evidence.rs` 中实现：
- 纯逻辑校验与计算函数 `evaluate_record_transition`；
- `SqlxTaskEvidenceApplier` 结构体，持有 `DbPool` 与可选 `Arc<EventBus>`；
- 单个数据库短事务内：锁住子任务行（`FOR UPDATE`），校验 CAS 与代际，写入 `evidence_json`，更新 state/reason/revision，释放/保留 `device_operation_locks`，更新 `main_tasks` 汇总。
- 提交事务后向 `EventBus` 发布 `task_updated`。

- [ ] **Step 4: 运行测试验证通过**
`cargo test --test task_evidence`，预期测试全部通过。

- [ ] **Step 5: 格式检查**
运行 `make fmt-check` 确保格式合规。

---

### Task 2: 生产调度适配器 SqlxSchedulerStore 与调度器持久化收敛

**Files:**
- Create: `crates/rsetup-controller/src/tasks/scheduler_store.rs`
- Modify: `crates/rsetup-controller/src/tasks/scheduler.rs`
- Modify: `crates/rsetup-controller/src/tasks/mod.rs`
- Test: `crates/rsetup-controller/tests/task_scheduler_sqlx.rs`

**Interfaces:**
- Consumes:
  - `SchedulerStore`, `DispatchCandidate`, `DispatchIntentWrite`, `CommitVerdict` from `tasks::scheduler`
  - `DbPool`
- Produces:
  - `SqlxSchedulerStore` implementing `SchedulerStore`
  - 调度器对排队过期（`expired`）和 prepare 失败（`failed`）的持久化写入

- [ ] **Step 1: 编写失败测试**
在 `crates/rsetup-controller/tests/task_scheduler_sqlx.rs` 中编写测试：
- `list_candidates` 正确联查 queued 状态的候选子任务及其关联设备与锁信息；
- `commit_dispatch_intent` CAS 成功落库 dispatching 意图；
- CAS 条件不满足（已非 queued 或代际冲突）时返回 `DefinitelyNotCommitted`；
- 模拟数据库故障时返回 `CommitUnknown`。

- [ ] **Step 2: 运行测试验证失败**
`cargo test --test task_scheduler_sqlx`，预期编译失败。

- [ ] **Step 3: 实现 SqlxSchedulerStore 与调度收敛**
- 在 `crates/rsetup-controller/src/tasks/scheduler_store.rs` 中实现 `SqlxSchedulerStore`；
- 在 `tasks/scheduler.rs` 中增加调度收敛逻辑：
  - 对排队期限过期的候选执行标记 `expired` 并释放设备锁；
  - 对 prepare 明确失败的候选执行标记 `failed` 并释放设备锁；
  - 对 execute 返回结果调用 `TaskEvidenceApplier` 落地。

- [ ] **Step 4: 运行测试验证通过**
`cargo test --test task_scheduler_sqlx` 以及现有的 `cargo test --test task_scheduler`，验证通过。

- [ ] **Step 5: 格式检查**
运行 `make fmt-check` 验证格式。

---

### Task 3: 中控 TaskResultSink 与板端结果事件触发

**Files:**
- Modify: `crates/rsetup-controller/src/transport/listener.rs`
- Modify: `crates/rsetup-controller/src/main.rs`
- Modify: `crates/rsetup-board-agent/src/session_handler.rs`
- Modify: `crates/rsetup-board-agent/src/dispatch.rs`
- Test: `crates/rsetup-controller/tests/task_result_sink.rs`
- Test: `crates/rsetup-board-agent/tests/board_result_event.rs`

**Interfaces:**
- Consumes:
  - `TaskResultSink`, `TaskResultContext` from `transport::listener`
  - `TaskEvidenceApplier`
- Produces:
  - `SqlxTaskResultSink` 接入生产 listener
  - 板端 `execute` 后主动触发 `device.task.result`

- [ ] **Step 1: 编写失败测试**
- 中控端：测试 `SqlxTaskResultSink` 收到事件后校验代际并持久化推进任务；
- 板端：测试 `execute` 写入 journal 产生 `accepted` 记录后，通过会话句柄发送 `device.task.result` 包。

- [ ] **Step 2: 运行测试验证失败**
`cargo test --test task_result_sink` 与 `cargo test --test board_result_event`，预期失败。

- [ ] **Step 3: 实现 SqlxTaskResultSink 与板端主动事件**
- 在 `transport/listener.rs` 中实现 `SqlxTaskResultSink`，并在 `main.rs` 中装配替换 `RejectNoTaskResultSink`；
- 在板端 `session_handler.rs` 中注入事件发送机制，在 `execute` 返回 accepted 记录时异步发送 `device.task.result`。

- [ ] **Step 4: 运行测试验证通过**
`cargo test --test task_result_sink` 与 `cargo test --test board_result_event`，验证通过。

- [ ] **Step 5: 格式检查**
运行 `make fmt-check` 验证格式。

---

### Task 4: 调度器后台循环与设备上线持续核实

**Files:**
- Modify: `crates/rsetup-controller/src/tasks/scheduler.rs`
- Modify: `crates/rsetup-controller/src/transport/manager.rs`
- Modify: `crates/rsetup-controller/src/main.rs`
- Test: `crates/rsetup-controller/tests/task_continuous_verify.rs`

**Interfaces:**
- Consumes:
  - `TaskScheduler`
  - `TransportManager::active_devices`
  - `TaskEvidenceApplier`
- Produces:
  - `spawn_task_scheduler` 后台循环
  - 设备转 Ready 后的未决任务核验触发逻辑

- [ ] **Step 1: 编写失败测试**
测试设备断线重连转 Ready 后，自动触发未决任务的 `task_record` 查询并推进状态；测试收到关停信号后调度循环安全退出。

- [ ] **Step 2: 运行测试验证失败**
`cargo test --test task_continuous_verify`，预期失败。

- [ ] **Step 3: 实现调度循环与重连核验**
- 在 `main.rs` 中装配并启动 `spawn_task_scheduler`；
- 在 `transport/manager.rs` 或相关驱动中挂接新 Ready 设备的未决核验通知；
- 调度循环定期执行，支持优雅关停。

- [ ] **Step 4: 运行测试验证通过**
`cargo test --test task_continuous_verify`，验证通过。

- [ ] **Step 5: 格式检查**
运行 `make fmt-check` 验证格式。

---

### Task 5: 全链路回归、缺口文档更新与独立审查

**Files:**
- Modify: `docs/testing/controller-v1-known-gaps.md`

- [ ] **Step 1: 运行全量格式门禁**
`make fmt-check`

- [ ] **Step 2: 运行相关离线测试集合**
运行包含任务、调度、传输的测试，确认执行数非 0。

- [ ] **Step 3: 更新缺口文档**
如实说明任务调度生产接入、状态持久化收敛、主动结果接收和持续核验已闭环，保持真实硬件 reboot 的缺口边界不变。

- [ ] **Step 4: 启动 GLM 独立只读审查子代理**
模型：`glm-coding/glm-5.3-flash high`，只读审查代码与调用链。

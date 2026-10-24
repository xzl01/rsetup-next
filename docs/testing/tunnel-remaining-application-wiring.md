# 隧道验收之外：应用侧剩余未接线事实盘点（只读）

> 本文只做**事实盘点**，是给后续工作的输入，不是设计批准，也不是完成评审；不提议重构。
> 方法：符号 grep + 命中处 30–60 行上下文；未通读 task 源码全集或新架构文档。
> 行号为本次 grep 实际命中符号处，后续 fix 会移动；复核以符号名为准。
> 范围：`crates/rsetup-controller/`（src + tests）。范围内未命中即记 unknown，不推测。

## A. 事实表

### A-0. 四个问题的一句话结论

1. 生产 `TaskResultSink` **仍是** `RejectNoTaskResultSink` 默认，且监听入口无注入参数。
2. `board_clock` 纯算法**未被任何生产代码消费**（仅自身单测 + 一次再导出）。
3. `UnwiredRecoveryProbe` **是真生产引用**（main 装配），语义为按契约 503。
4. `SchedulerStore` **无生产 SQL adapter**，main **未启动调度 loop**。

| 项 | 定义 | 生产 caller | 状态 | 风险 |
|---|---|---|---|---|
| `TaskResultSink` 生产默认 | [listener.rs:657](crates/rsetup-controller/src/transport/listener.rs#L657) | [listener.rs:914-915](crates/rsetup-controller/src/transport/listener.rs#L914-L915) 函数体内硬编码 | **stub**（fail-closed） | 任务结果到达即丢弃，状态永不推进 |
| `board_clock` 纯算法 | [time/board_clock.rs](crates/rsetup-controller/src/time/board_clock.rs) | 无 | **未消费** | 改动/删除无编译信号 |
| `UnwiredRecoveryProbe` | [main.rs:80](crates/rsetup-controller/src/main.rs#L80) | [main.rs:118](crates/rsetup-controller/src/main.rs#L118) | **真生产引用**（有意 503） | 恢复模式视图恒 503 |
| `SchedulerStore` SQL adapter | [scheduler.rs:445](crates/rsetup-controller/src/tasks/scheduler.rs#L445) | 仅 tests | **stub**（无 SQL 实现） | exactly-once 保证未被证明 |
| main 启动调度 loop | `TaskScheduler::run_once` [scheduler.rs:741](crates/rsetup-controller/src/tasks/scheduler.rs#L741) | 无 | **未见入口（本次搜索范围）** | queued 子任务无驱动方 |
| `TaskResultContext::is_current` | [manager.rs:1847](crates/rsetup-controller/src/transport/manager.rs#L1847) | [listener.rs:795](crates/rsetup-controller/src/transport/listener.rs#L795) | **已接** | 后续实现须复用，不得重造 |

### A-1. TaskResultSink：仍为 `RejectNoTaskResultSink` 默认

- trait 与注释：[listener.rs:649-660](crates/rsetup-controller/src/transport/listener.rs#L649-L660)；`RejectNoTaskResultSink` [#L662-L672](crates/rsetup-controller/src/transport/listener.rs#L662-L672) 无条件返回 `false`，注释明写「任务结果的持久化写入口尚未交付」。
- 装配：`run_transport_listener_with_stats` 签名 [#L899-L905](crates/rsetup-controller/src/transport/listener.rs#L899-L905) **不含 sink 参数**，sink 在 [#L914-L915](crates/rsetup-controller/src/transport/listener.rs#L914-L915) 就地构造；`sink` 仅向 accept 分支透传（形参见 [#L1045](crates/rsetup-controller/src/transport/listener.rs#L1045)）。
- 入口链：`run_controller_listener` [#L871](crates/rsetup-controller/src/transport/listener.rs#L871) → `run_transport_listener` [#L886-L892](crates/rsetup-controller/src/transport/listener.rs#L886-L892) → `run_transport_listener_with_stats`。
- 丢弃计数点：[#L791-L798](crates/rsetup-controller/src/transport/listener.rs#L791-L798)（`!accepted` → `events_unwired`；随后 `is_current` 复核计 `events_stale_after_delivery`）。
- 结论：生产默认仍是拒绝；**本次搜索范围内未见注入真实 sink 的公开入口**（`crates/` 内 `run_transport_listener*` 的其它调用方只有 [tests/transport_listener.rs](crates/rsetup-controller/tests/transport_listener.rs)）。

### A-2. board_clock：纯算法在位，零生产消费

- 定义：[time/board_clock.rs](crates/rsetup-controller/src/time/board_clock.rs)；再导出 [time/mod.rs:7-10](crates/rsetup-controller/src/time/mod.rs#L7-L10)（`estimate_board_offset`、`ClockEstimate`、`ClockEstimateError`、`DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED`）。
- 复核 grep：`estimate_board_offset|ClockEstimate` 在 `crates/` 内除自身文件外仅命中再导出行 [time/mod.rs:8](crates/rsetup-controller/src/time/mod.rs#L8)。
- 其余命中均为该文件内 `#[cfg(test)]` 用例（[:90](crates/rsetup-controller/src/time/board_clock.rs#L90)、[:114](crates/rsetup-controller/src/time/board_clock.rs#L114)、[:133](crates/rsetup-controller/src/time/board_clock.rs#L133)、[:160](crates/rsetup-controller/src/time/board_clock.rs#L160)、[:184](crates/rsetup-controller/src/time/board_clock.rs#L184)）。
- 结论：**未被消费**——既未接入 TimeGate/时间证据链，也未接入任务判定路径。

### A-3. UnwiredRecoveryProbe：真生产引用（非 test-only）

- 定义 [main.rs:80](crates/rsetup-controller/src/main.rs#L80)，`impl RecoveryModeProbe for UnwiredRecoveryProbe` [main.rs:82](crates/rsetup-controller/src/main.rs#L82)，装配 `Arc::new(UnwiredRecoveryProbe)` [main.rs:118](crates/rsetup-controller/src/main.rs#L118)。
- 语义来源：模块注释 [main.rs:11](crates/rsetup-controller/src/main.rs#L11)（按契约上抛 503，不伪造 ready/mode）、[main.rs:76](crates/rsetup-controller/src/main.rs#L76)（`ready`/`mode` 必须来自 Runtime 当前真实快照）。
- 结论：**是真生产引用**，且是「如实报告未接线」的设计而非可删占位。

### A-4. SchedulerStore / TaskScheduler：无 SQL adapter、无调度 loop

- trait [scheduler.rs:445](crates/rsetup-controller/src/tasks/scheduler.rs#L445)：`list_candidates` [#L447](crates/rsetup-controller/src/tasks/scheduler.rs#L447)、`commit_dispatch_intent` [#L453](crates/rsetup-controller/src/tasks/scheduler.rs#L453)；CAS 契约与 `CommitUnknown` 语义见 [#L410-L456](crates/rsetup-controller/src/tasks/scheduler.rs#L410-L456)。
- 编排体：`TaskScheduler` [#L585](crates/rsetup-controller/src/tasks/scheduler.rs#L585)、`new` [#L600](crates/rsetup-controller/src/tasks/scheduler.rs#L600)、`run_once` [#L741](crates/rsetup-controller/src/tasks/scheduler.rs#L741)（单调时钟不可信即整体拒绝，[:746-750](crates/rsetup-controller/src/tasks/scheduler.rs#L746-L750)）；`MonoClock` [#L458](crates/rsetup-controller/src/tasks/scheduler.rs#L458)。
- `impl SchedulerStore` 全部命中：[tests/task_scheduler.rs:357](crates/rsetup-controller/tests/task_scheduler.rs#L357)（FakeStore，注释 [:41](crates/rsetup-controller/tests/task_scheduler.rs#L41) 自陈进程内内存 CAS 非强约束证明）、[:1141](crates/rsetup-controller/tests/task_scheduler.rs#L1141)（BrokenStore）。
- `src/` 内除自身文件外的 `scheduler` 命中均属轮询线 `polling::PollScheduler`（如 [manager.rs:1475](crates/rsetup-controller/src/transport/manager.rs#L1475)），与本调度器无关。
- main 实际 spawn 的常驻任务：`spawn_revoke_drain` [main.rs:856](crates/rsetup-controller/src/main.rs#L856)、`spawn_admission_recheck` :857、`spawn_poll_driver` :858、`shutdown_signal` :862、门禁 `wait_ready` :318 —— **无调度 loop 启动**。

### A-5. 已有真实公开入口（按现有名字，非建议）

- HTTP 顶层装配：[production.rs:123](crates/rsetup-controller/src/production.rs#L123) `build_tasks_router` 进入 `compose_component_routers`；路由集 [tasks/http.rs:742-753](crates/rsetup-controller/src/tasks/http.rs#L742-L753)。
- 业务编排：main.rs:216-219 `HttpTasksState { api: SqlxTaskApi::with_time_source(..) }`；`TaskService` `new` [service.rs:657](crates/rsetup-controller/src/tasks/service.rs#L657)、`preview` :665、`submit` :698、`cancel` :883、`time_review` :906、`release_lock` :930，落在 `SqlxTaskRepository`（[repository.rs:428](crates/rsetup-controller/src/tasks/repository.rs#L428)）。
- 启动期恢复：[main.rs:821-822](crates/rsetup-controller/src/main.rs#L821-L822) `TransportBoardClient::new(transport_manager.as_device_rpc())` + `recover_process(...)`（导出 [tasks/mod.rs:35](crates/rsetup-controller/src/tasks/mod.rs#L35)）——板端 RPC 已有真实消费方，与任务结果写入口是两件事。
- 未见入口（本次搜索范围）：按 `sub_task_id` 推进任务结果的仓储写方法；`SchedulerStore` 的 SQL 实现；`TaskScheduler` 的周期驱动；`board_clock` 估算的消费点。

## B. 最小后续依赖清单（≤6 步；只列候选文件与需要的测试）

1. 任务结果写入口：按 `sub_task_id` 推进的仓储方法 + 满足 `TaskResultSink` 的实现。候选 [tasks/repository.rs](crates/rsetup-controller/src/tasks/repository.rs)、[tasks/service.rs](crates/rsetup-controller/src/tasks/service.rs)。测试：真 sink 端到端 `accepted=true`、`events_unwired` 不增；在自己的写入边界复用 `is_current`，不重造代际判断。
2. 为监听入口补 sink 注入能力。候选 [transport/listener.rs](crates/rsetup-controller/src/transport/listener.rs)。测试：缺省仍 fail-closed；注入真 sink 才推进；拒绝路径计数可观测。
3. `SchedulerStore` 的 SQL adapter（只读候选短事务 + `commit_dispatch_intent` 持久 CAS）。候选 [tasks/repository.rs](crates/rsetup-controller/src/tasks/repository.rs) 或新 `tasks/scheduler_store.rs`。测试：MySQL 与 TiDB 双引擎的 CAS 冲突、`CommitUnknown` 不执行不重发不判成功。
4. `MonoClock` 生产时间源接入。候选 [tasks/scheduler.rs](crates/rsetup-controller/src/tasks/scheduler.rs)、[time/gate.rs](crates/rsetup-controller/src/time/gate.rs)。测试：时钟不可信 → `clock_untrusted` 且零 prepare、零 execute。
5. 调度 loop 启动点与优雅关停（含 `active_slots` 真实来源、shutdown watch）。候选 [main.rs](crates/rsetup-controller/src/main.rs)（与 :856-862 同段装配）。测试：关停收敛、单轮失败不影响下一轮。
6. 以真实 Runtime 快照替换 `UnwiredRecoveryProbe`。候选 [main.rs:80-120](crates/rsetup-controller/src/main.rs#L80-L120)。测试：ready/mode 随撤权/撤销如实翻转（禁止把 503 直接改成恒 200）。

## C. 边界（不可越线）

- 真实双进程 first-run 曾 FAIL（RC1 JSON 判定、RC2 grace），修复后**待复跑**；本文不声明其结果。
- DB v4 迁移与两引擎 SQL 通过 ≠ 任务业务行为已全部生产接线；fake store 只证明编排纯逻辑。
- host reboot 是 `Unsupported` 的合法能力限制，不允许以真实主机重启做验证。
- 不得新增测试专用 target 或伪 sink 让计数变绿；不得把同步返回的 `queued` 布尔当 DB commit；不得对无 generation 的任务结果注入（`claim_task_result` + `is_current` 闸门不可绕过，[manager.rs:1847](crates/rsetup-controller/src/transport/manager.rs#L1847)）。
- 本文不主张 API 或架构变更，只如实标注：**隧道验收独立之外，A 表各项仍未接线**。
- unknown：[tasks/repository.rs:896](crates/rsetup-controller/src/tasks/repository.rs#L896)、[:927](crates/rsetup-controller/src/tasks/repository.rs#L927) 出现的 `queued`/`dispatching` 字样属提交与投影路径，是否已含任何 dispatch 意图写入未逐行核实，按 unknown 记录。

## D. 本次搜索记录（可复现符号）

- `TaskResultSink` / `RejectNoTaskResultSink` / `run_transport_listener`：src 命中定义、mod 再导出与 listener 内部；tests 命中 [tests/transport_listener.rs](crates/rsetup-controller/tests/transport_listener.rs) 多处 spawn。
- `estimate_board_offset` / `ClockEstimate` / `board_clock::`：src 命中仅 [time/mod.rs:7-10](crates/rsetup-controller/src/time/mod.rs#L7-L10) 与自身 `#[cfg(test)]`；`time/` 下无 unit test 目录需排除。
- `UnwiredRecoveryProbe`：src 命中仅 [main.rs](crates/rsetup-controller/src/main.rs) 的 11/80/82/118/199 行。
- `SchedulerStore`：src 命中仅 [tasks/scheduler.rs](crates/rsetup-controller/src/tasks/scheduler.rs)；`impl SchedulerStore` 仅 tests。
- `run_controller_listener` / `on_task_result` / `is_current`：用于确定装配链与提交边界；`is_current` 的其余命中在 [tests/transport_manager.rs](crates/rsetup-controller/tests/transport_manager.rs)、[tests/transport_manager_fixes.rs](crates/rsetup-controller/tests/transport_manager_fixes.rs)。
- `build_tasks_router`：src 命中 [production.rs:38](crates/rsetup-controller/src/production.rs#L38)、[:123](crates/rsetup-controller/src/production.rs#L123)，证明 tasks 子路由已进入顶层生产 router。
- 未执行：编译、测试、进程、数据库与网络访问；未修改任何源码、测试、计划或其它报告。

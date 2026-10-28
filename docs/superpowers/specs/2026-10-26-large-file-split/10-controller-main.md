# 子规格 10：`crates/rsetup-controller/src/main.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 10 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码/测试、未编译（含最小 rustc 探针）、未运行测试或任何 `make` 目标、未读 `secret/`、未连接数据库、未提交。

撰写基线：分支工作树头提交 `c7ff640`；实测 `git status` 当时 [src/main.rs](../../../../crates/rsetup-controller/src/main.rs) 为**已修改未提交**状态（总览 §3 并行改动注记）。本文全部行号为撰写会话对当时工作树 2,633 行文件（实测 `wc -l` = 2,633）的逐段实测，实施时须按当时工作树重新核对。

**基线漂移风险（必须逐批复采）**：当前工作树存在他人并行未提交改动，[src/main.rs](../../../../crates/rsetup-controller/src/main.rs) 相对 `c7ff640` 为 **+65 / −4**（实测 `git diff --stat`），新增内容正是**保留期清理后台任务**：`clean_retention_data` 导入、`RETENTION_CLEANUP_INTERVAL` 常量、`retention_task` 循环与收敛清单条目。该段代码同时是 §6.4 三条源码文本断言的被测对象，且同批未提交改动还涉及 [src/db.rs](../../../../crates/rsetup-controller/src/db.rs)、[tasks/scheduler.rs](../../../../crates/rsetup-controller/src/tasks/scheduler.rs)、[tasks/scheduler_store.rs](../../../../crates/rsetup-controller/src/tasks/scheduler_store.rs)、`Makefile`、`schema/v4.sql` 与多个 `tests/*.rs`。**本文按「工作树已提交」假设撰写**：任何一批开始前 MUST 重跑 §1 的锚点核对（`wc -l` + 关键行 `grep -n`），若保留期清理段被回退或改写，§6.4 的 token 归属表随之下移，不得沿用本文行号。总览 §4 的 10 号状态行由总览维护方同步。

**须由最终总览同步的登记项（本文不自行修改总览）**：总览 C6 规定各 crate 的 `tests/*.rs` 只作验证依据、不作重构对象；本规格 §6 需要对 **3 处源码文本消费者**做等价接线——`Makefile` `metrics-static-gates` 第 6 条（按路径 grep `src/main.rs`、文件缺失 Fail-Closed）、[tests/v4_baseline.rs](../../../../crates/rsetup-controller/tests/v4_baseline.rs) 的 `read_source("src/main.rs")`、[tests/production_composition.rs](../../../../crates/rsetup-controller/tests/production_composition.rs) 的两处 `include_str!("../src/main.rs")`。接线边界：只改「读哪些文件、作用域怎么枚举」，**不改任何断言内容、token 表、拒绝语义与用例数**；Makefile 侧不新增目标、不改目标名、不改 CI 步骤。该窄例外须由最终总览 C6/§6.4 同步登记（先例：08 号子规格 [08-controller-db.md](08-controller-db.md) §6 对 `tests/v4_baseline.rs` 的同类接线）。

## 1. 实测现状与锚点

### 1.1 体量、bin 交付面与真实职责

- 全文件 **2,633** 行：实现区 **1–1,187**（`main()` 的收尾 `.await` 在 1,186，函数体在 1,187 闭合），测试区 **1,189–2,633**（`#[cfg(test)] mod tests` 属性在 1,189、声明在 1,190，`use super::*;` 在 1,191）。与总览 §3 快照「1,188 / 1,445」同量级（快照按 1,188 实现 + 1,445 测试切分，差异在边界行归属，本文以实测为准）。
- **实测测试计数**：`#[test]` **5**（1210/1236/1683/1729/1741）+ `#[tokio::test]` **15** + `#[tokio::test(start_paused = true)]` **9**（2069/2107/2153/2212/2238/2266/2402/2494/2579）= **29**（全部位于 `mod tests`），另有 `#[cfg(test)] fn waiter_handle`（421，`impl StartupTimeGate` 内的 1 个 test-only 方法，不是用例）。合计 **30 个 test 属性**，其中**用例数 = 29**（名单见 §5）。
- **bin 交付面（C1 的对外路径，实测）**：[Cargo.toml](../../../../crates/rsetup-controller/Cargo.toml) 无主 bin 的 `[[bin]]` 条目（仅显式命名 `rsetup-controller-migrate` → `src/bin/migrate-controller.rs`、`rsetup-admin` → `src/bin/reset-admin.rs`，即 14–21 行的两个 `[[bin]]` 区块），主进程由 `src/main.rs` **自动发现**为 bin 名 `rsetup-controller`，并由 3 行 `default-run = "rsetup-controller"` 固定（唯一消费者是 §5 的 `default_cargo_run_selects_readonly_server`）。[debian/rules](../../../../debian/rules) 107–110 行与 [data/rsetup-controller.service](../../../../data/rsetup-controller.service) 的 `ExecStart=/usr/bin/rsetup-controller` 只引用**交付名与产物路径**，与源文件布局无关。结论：**保留薄根 `src/main.rs`**（2018 edition 的 `main.rs` + `main/` 目录模块），bin 名、`default-run`、`cargo run -p rsetup-controller`、`make controller-bin`、Debian 打包与 systemd 单元**零改动**。
- 本文件**不是** lib 的一部分：[lib.rs](../../../../crates/rsetup-controller/src/lib.rs) 不声明 `mod main`，`main.rs` 内的任何项都没有 crate 外部或跨 crate 消费者。它的对外面只有三类：① bin 名与产物路径；② **进程行为协议**（环境变量集合、启动顺序、日志文本、退出码、优雅停机语义）；③ §6 的**源码文本门禁**。拆分自由度大，约束全部集中在 §2 行为不变与 §6 门禁等价。
- 真实职责盘点（按当前工作树，纠正任务假设）：
  - **配置与 CLI**：本入口**没有 CLI 参数面**（`main.rs` 内无 `std::env::args`，实测），配置**只从环境读**：`ControllerConfig::from_env`（[config.rs](../../../../crates/rsetup-controller/src/config.rs) 26：`CONTROLLER_DATABASE_URL` 必填、`CONTROLLER_LISTEN_ADDRESS`）、`MetricsConfig::from_env`（metrics 线的 `controller.toml` / `CONTROLLER_CONFIG` 仅由 lib 侧 [metrics/config.rs](../../../../crates/rsetup-controller/src/metrics/config.rs) 19/239 行消费，**main.rs 不读 TOML**）、`HttpSecurityConfig::from_env`（[http_security.rs](../../../../crates/rsetup-controller/src/http_security.rs) 101–103：`CONTROLLER_ALLOWED_HOSTS` / `CONTROLLER_ALLOWED_ORIGIN` / `CONTROLLER_TRUST_TLS`）、`load_management_cursor_secret_from_env`（[api/mod.rs](../../../../crates/rsetup-controller/src/api/mod.rs) 658：`CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX`）、`TransportEnv::from_env`（板端接入，`RSETUP_CONTROLLER_KEY_FILE` 等）、`NTP_SERVERS`（179）、`CONTROLLER_CONTROLLED_RECOVERY`（86）、`CONTROLLER_BOOTSTRAP_SECRET_LOG`（868）。
  - **服务装配**：`production_inputs`（228–333）唯一构造 auth/management/tasks/events/status/metrics/web 七线状态，`build_production_router`（938）唯一合并点；单一 `DbPool`、单一 `Arc<HttpAuthState>`、单一 `SnapshotStore`、单一 `RuntimeRecoveryController`、单一 `EventBus`、单一 `TimeGateHandle` 在此收敛（模块级注释 1–12 行即该契约的出处）。
  - **启动门禁与时序**：`StartupTimeGate`（380–438）、`listen_after_startup_gates`（450–473）、`start_if_ready`（351–359）——DB 线与时间线**并行**、任一失败即不绑端口。
  - **优雅停机与生命周期监督**：`shutdown_signal`（479–499）、`BoardTask`/`BoardOutcome`（525–559）、`observe_board`/`sleep_or_pending`/`watch_turns_true`（565–592）、`finish_board`（597–652）、`supervise_shutdown`（682–779）、`run_supervised_lifecycle`（787–797，绑定 `BOARD_SHUTDOWN_GRACE`）、`settle_background_tasks`（804–829）。
  - **后台任务接线**：任务调度与超时扫尾循环（1001–1049）、撤销消费/准入复查/轮询驱动三任务（960–962）、板端接入监听（1102–1138）在基线 `c7ff640` 已存在（实测 `git grep` 于该提交命中）；**保留期清理循环**（1063–1093，即工作树新增段）是 §6.4 四条 token 的被测对象与基线漂移风险来源（见前言）。
  - **首启动凭据落盘**：`LogSecretSink`（65–78）+ `write_private_secret`（335–349，`create_new` + `0600`）。
  - **受控恢复模式触发**：`CONTROLLED_RECOVERY_ENV`/`controlled_recovery_requested`/`controlled_recovery_flag`/`build_recovery_controller`/`new_recovery_controller`（86–128）。

### 1.2 源码文本消费者（拆分必须先解决的前置约束）

实测全仓按**源码文本**消费 `src/main.rs` 的位置恰为 3 处（`grep -rn 'main\.rs'` 全仓复核：`crates/rsetup-app`、`apps/desktop`、`tests/web_assets.rs` 的 `main.rs` 与本文件无关；`ui/desktop-ipc.test.mjs` 指向 desktop；`.cargo-local/`、`externals/` 为第三方）：

| # | 消费者（实测位置） | 读取方式 | 断言内容 | 拆分后是否必须扩作用域 |
| --- | --- | --- | --- | --- |
| G-M6 | [Makefile](../../../../Makefile) 186–212 行 `metrics-static-gates`，第 6 条在 **190–192** 行 | `grep -rn 'PATTERN' crates/rsetup-controller/src/main.rs` | 期望**无输出**；`rc>1`（含文件缺失）即 `exit 1` Fail-Closed | **是**（§6.1–6.2，硬约束） |
| G-V4 | [tests/v4_baseline.rs](../../../../crates/rsetup-controller/tests/v4_baseline.rs) `startup_path_stays_read_only_and_the_entry_point_is_the_only_writer`（429–457，`read_source("src/main.rs")` 在 432） | `fs::read_to_string(manifest_dir().join("src/main.rs"))`（helper 26–30，读失败即 panic） | 必须 `contains("check_identity_schema")`；必须**不含** `run_production_migration` / `create_v4_schema` | **是**（§6.3） |
| G-PC1 | [tests/production_composition.rs](../../../../crates/rsetup-controller/tests/production_composition.rs) `mod typed`（85）内 `production_inputs_exposes_no_second_pool`（1110），`include_str!("../src/main.rs")` 在 **1125** | 编译期内嵌 | 必须 `contains("build_production_router(")`；必须**不含** `build_production_router_with_metrics_source`、**不含** `SqlxMetricsSnapshotSource` | **是**（负向 token 须覆盖子树，§6.4） |
| G-PC2 | 同文件 `mod retention_cleanup_wiring`（1447）内 `retention_cleanup_runs_in_the_production_background`（1449），`include_str!` 在 **1452**，按 `split("\n#[cfg(test)]").next()` 取「生产段」（1453–1456） | 编译期内嵌 + 生产段截取 | 生产段必须 `contains` 四项：`clean_retention_data(`、`RETENTION_CLEANUP_INTERVAL`、`retention_recovery.allows_scheduling()`、`retention_task,` | **是**（token 随保留期清理域迁入 `main/background.rs`，§6.4） |

同 token 的**非门禁**文本引用（只作说明、不读 `main.rs`，本规格零改动）：[production.rs](../../../../crates/rsetup-controller/src/production.rs) 82、[metrics/collect.rs](../../../../crates/rsetup-controller/src/metrics/collect.rs) 146、[http_auth.rs](../../../../crates/rsetup-controller/src/http_auth.rs) 5/9、[tasks/adapter.rs](../../../../crates/rsetup-controller/src/tasks/adapter.rs) 21、[tasks/http.rs](../../../../crates/rsetup-controller/src/tasks/http.rs) 4、[time/gate.rs](../../../../crates/rsetup-controller/src/time/gate.rs) 189/272/297、[api/system.rs](../../../../crates/rsetup-controller/src/api/system.rs) 106、[tests/sse_time_events.rs](../../../../crates/rsetup-controller/tests/sse_time_events.rs) 20/160、[tests/runtime_operations.rs](../../../../crates/rsetup-controller/tests/runtime_operations.rs) 10。其中三处**必须保持为真**的语义：`build_production_router_with_metrics_source` 是 `#[doc(hidden)]` 测试注入缝、MUST NOT 被 `main.rs` 调用；`SqlxMetricsSnapshotSource` 只能在 `production.rs` 内构造；`time_changed_sink` 的挂接形状。

CI 接线事实（供 §6.2 判定是否触发合同测试）：CI 合同要求 `metrics-static-gates` 被 `.PHONY` 声明、在 `rust-and-ui` 作业的 cargo 编译之前调用，**且排在 `make fmt-check` 之后**（同文件 519–522 行的顺序断言「保持静态检查聚拢」；§7 批 A 不改该步骤顺序，此约束不构成接线障碍，但改 recipe 时 MUST NOT 挪动步骤位置），外加 recipe 里 `$$(git diff --cached --name-only)` 与 `$$(git ls-files)` 两条扫描及其后的 `controller\.toml` / `^secret/` 过滤（[controller-ci-contract.test.mjs](../../../../scripts/tests/controller-ci-contract.test.mjs) 489–560 行，`.PHONY` 断言 491–495、CI 步骤断言 500–504、`fmt-check` 顺序断言 519–522、recipe 两条扫描断言 541–558）；**没有任何一条**断言第 6 条的 grep 表达式或路径，因此 §6.2 的改写不违反合同，但改 Makefile 后 MUST 跑 `node --test scripts/tests/*.test.mjs` 复核（§8）。

## 2. 行为与不变量（逐字保持，MUST NOT 借拆分改写）

1. **无 CLI 参数**：不引入任何 argv 解析；未知参数继续被静默忽略（现状）。
2. **配置键与默认值**：本文 §1.1 列出的环境变量集合、读取位置与读取时机全部不变；`CONTROLLER_BOOTSTRAP_SECRET_LOG` 未设置时默认相对路径 `controller-bootstrap-secret.log`（868–870）；`NTP_SERVERS` 未设置即空 servers → `SystemFallback`（178–192）；`CONTROLLER_CONTROLLED_RECOVERY` 只认 `"1"`/`"true"`（大小写不敏感、忽略首尾空白、非 UTF-8 按未设置，86–103）。`controller.toml` 仅经 `MetricsConfig::from_env` 消费，拆分 MUST NOT 把它引入其它路径。
3. **启动时序（冻结）**：`MetricsRuntime::new()` 为 `main` 函数体**第一条语句**（uptime 锚点，833–836）→ `ControllerConfig::from_env` → `MetricsConfig::from_env`（连库/绑端口之前）→ `EventBus::new` → `clock_epoch = Uuid::new_v4()` → `start_time_gate_with_notifier(.., Some(time_changed_sink(&event_bus)))` → `StartupTimeGate::start`（必须在 `DbPool::connect` **之前**）→ `DbPool::connect` → 读 secret path → `TransportEnv::from_env` → `ManagerConfig` 匹配 → `build_recovery_controller` → `build_transport_manager` → `start_if_ready(check_identity_schema(&db), ‖ …)`；闭包内：`bootstrap_admin` → `process_epoch` → `SqlxTaskRepository` → `TransportBoardClient::new` → `recover_process` → 对账日志 → `listen_after_startup_gates`。`listen_after_startup_gates` 内：`pre_listen`（装配 router）→ `time_gate.finish()` → 时间门结束日志 → `listen`。`listen` 内：`TcpListener::bind(&listen_address)` → `watch::channel(false)` → `spawn_revoke_drain` → `spawn_admission_recheck` → `spawn_poll_driver` → 调度任务 spawn → 保留期清理任务 spawn → `tokio::spawn(shutdown_signal(...))` → 板端监听 spawn → `axum::serve(...).with_graceful_shutdown(watch_turns_true(..))` → `run_supervised_lifecycle` → `settle_background_tasks`。任何一步的相对顺序（尤其 30s 就绪预算的**并行**起点、后台任务 spawn 次序、信号任务在监听任务之前）都不得改变。
4. **信号处理**：只监听 Ctrl-C 与 SIGTERM（479–499）；非 Unix 上 terminate 恒 pending；`tokio::signal::unix` 注册失败转 pending（不因无法监听 SIGTERM 而失败退出）；任一到达只 `shutdown.send(true)`，不做二次信号区分、不写任何持久决定。
5. **优雅停机顺序**：`supervise_shutdown` 的 `tokio::select! { biased; … }` 四个分支顺序与守卫条件、`armed` 起表时机、`external_shutdown` 取证时机（本分支广播之前）全部不变；宽限期只在关停意图确立后武装（RC2 语义）；`finish_board` 的三种回执形态处理与「abort 原始任务 + await 确认」不变；`settle_background_tasks` 先自行退出后 abort 原句柄不变。
6. **绑定地址**：`tokio::net::TcpListener::bind(&listen_address)` 直接使用 `ControllerConfig.listen_address` 原值，不改写、不二次解析、不加默认。
7. **日志文本逐字不变**（运维与冒烟按行匹配）：受控恢复模式提示、`controller 时间门禁已启动: clock_epoch=… ready=…`、`controller 时间启动门结束（quality=…, clock_epoch=…, ready=…）`（原文用 `clock_epoch` 字段名）、`controller 启动恢复对账完成: process_epoch=… 已核验=… 置held=… 置expired=… 发送=…`、`controller 板端接入监听启动: … (max_half_open=… max_sessions=…)`、`controller 板端接入监听未启用: RSETUP_CONTROLLER_KEY_FILE 未设置`、`controller 收到关停信号，开始优雅关停`、`controller task scheduler 超时dispatching收敛为unknown: …` 等全部 `eprintln!` 文案与其字段顺序。
8. **错误退出码**：`main` 返回 `Result<(), Box<dyn Error>>`，任一步 `?`/`Err` 即进程非 0 退出；顶层错误优先级不变（HTTP 自身失败优先，板端监听错误随后，`unprompted` fail-closed 错误并入 board 侧）；不新增 `exit(code)`、不改 `BoxedError` 装箱语义。
9. **常量取值与唯一性**：`BACKGROUND_GRACE` = 2s（132）、`BOARD_SHUTDOWN_GRACE` = 30s（135，仅经 `run_supervised_lifecycle` 绑定一次）、`RETENTION_CLEANUP_INTERVAL` = 3600s（143）、调度循环 `interval(1000ms)`、`reap_ticks.is_multiple_of(5)`、全局 16 槽的活跃槽统计 SQL 文本（1036–1038，`SELECT COUNT(*) FROM sub_tasks WHERE state IN ('dispatching', 'accepted', 'verifying')`）全部不变；调度器与清理循环的恢复模式闸门都必须读**同一个** `Arc<RuntimeRecoveryController>`。
10. **fail-closed 语义**：`TransportEnv::from_env` 非法即失败关闭（不回退默认）；`try_new` 预算越界即 `ControllerError::Config`；`recover_process` 失败上抛且不对外的语义不变。
11. **禁止进入生产入口的符号（负向不变量，被 G-M6/G-PC1 钉住）**：`build_production_router_with_metrics_source`、`SqlxMetricsSnapshotSource`（含任意形式的类型名提及，不只是 `::new` 构造）在拆分后的 `src/main.rs` 与 `src/main/**/*.rs` 中 MUST NOT 出现——**连注释与文档字符串也不得出现字面量**，这是门禁等价的最小充分约束，薄根与各子文件顶部注释须遵守。

## 3. 目标私有模块树（拟建，未创建）

```text
crates/rsetup-controller/src/main.rs            约 280–300  # 薄根：模块级文档注释 1–12 + use + mod 声明 + 冻结的 main() 编排（main() 本体 357 行，批 D 抽两段循环体约 129 行后仍余约 240 行，加注释/use/mod/1 测试；时序见 §2-3；含 build_production_router( / check_identity_schema / retention_task, 三个门禁锚点）
crates/rsetup-controller/src/main/
  assembly.rs        约 170   # 145–216 中的 build_transport_manager(154)/time_changed_sink(202 迁 startup_gate 见注)/task_time_source(214) + production_inputs(228–333) + 1 测试(§5)
  recovery.rs         约 60   # 80–128：CONTROLLED_RECOVERY_ENV、controlled_recovery_requested、controlled_recovery_flag、build_recovery_controller、new_recovery_controller + 2 测试
  secret_log.rs       约 60   # 65–78 LogSecretSink + 335–349 write_private_secret + 2 测试
  startup_gate.rs    约 300   # 173–192 time_gate_config_from_env、194–207 time_changed_sink、351–359 start_if_ready、361–438 StartupTimeGate(+Drop)、440–473 listen_after_startup_gates + 7 测试 + 受控门禁 fixture
  lifecycle.rs       约 620   # 501–829：BoxedError/boxed、BoardTask/BoardOutcome/observe_board/sleep_or_pending/watch_turns_true/finish_board/supervise_shutdown/run_supervised_lifecycle/settle_background_tasks、475–499 shutdown_signal + 17 测试 + 监督 fixture
  background.rs      约 200   # 129–143 BACKGROUND_GRACE/RETENTION_CLEANUP_INTERVAL + 从 1001–1049/1063–1093 抽出的 spawn_scheduler_loop / spawn_retention_cleanup（含 SchedulerDeps/RetentionDeps）+ 1 编译期计数用例（§5 末注）
```

**命名与声明约定**：薄根用 `src/main.rs` + `src/main/`（2018 edition 风格的 bin 模块目录，bin 的 crate root 即 `main.rs`），薄根内按依赖序声明 `mod assembly; mod background; mod lifecycle; mod recovery; mod secret_log; mod startup_gate;`（**全部 private `mod`，无 `pub mod`、无 `pub(crate) mod`**）；每个子文件顶部保留原代码段的中文注释块（逐字随代码移动）；不建汇总 `tests.rs`，测试一律进各自 `<子文件>::mod tests`（C3）。

**拆层依据**：① 生命周期监督域（lifecycle）是被 17 条回归用例直接驱动的核心资产，其「生产 main 与测试共用同一实现」契约要求 helper 单文件内聚；② 启动门禁域（startup_gate）承载「30s 预算与 DB 线并行」这一时序不变量（§2-3），与时间门禁 env/挂接同域；③ 装配域（assembly）只做「唯一构造点」，与薄根的编排（何时构造、何时绑端口）分开，使 §2-3 的时序在薄根上**一眼可读**；④ 后台任务域（background）把两条循环体从 `main` 的监听闭包中提出，同时把「必须列入收敛清单」的 `retention_task,` 留给薄根，使 G-PC2 的四个 token 各有唯一归属文件（§6.4）；⑤ recovery/secret_log 是两个自包含小域，先抽可让后续批次立即受益于门禁等价接线。

**关于 `waiter_handle`（421，属性在 421、签名在 422）的特殊处理**：它是实现块内**缩进**的 `#[cfg(test)]` 方法，而 G-PC2 用顶格 `\n#[cfg(test)]` 截取生产段。拆分后 `startup_gate.rs` 若保留该缩进属性，收集器的生产段截取仍正确（截到 `mod tests` 为止），但为让「test-only 代码一律置于顶格 `#[cfg(test)]` 区、首个顶格测试属性之后不再有生产代码」成为可机械核对的规则，本文要求把该方法改为 `#[cfg(test)] impl StartupTimeGate { … }` **扩展块**（属性顶格、置于 `mod tests` 之前），行为零变化（同为 test-only 方法）。改造后 `startup_gate.rs` 有**恰 2 个相邻**的顶格 `#[cfg(test)]`（截断点仍是第一个），口径以 §6.1 的前提与 §9 风险 6 的 DoD 为准。

行数均为「实现 + 对应测试」的实测估算（最大 `lifecycle.rs` 约 620，全部 ≤ 900，C4 无需例外申请）。不新增 crate、不改 `Cargo.toml`/`Cargo.lock`、不增删依赖；无新包，[Makefile](../../../../Makefile) `FMT_PACKAGES` 不动（总览 F1）。

## 4. 符号归属、可见性与依赖方向

### 4.1 完整符号归属表（实现区 1–1,187 的每一个顶层项 → 归属文件 → 拆分后可见性）

| 符号（原行号） | 归属文件 | 拆分后可见性 | 跨文件消费方（实测） |
| --- | --- | --- | --- |
| 模块级文档注释（1–12） | 薄根 `main.rs` | — | 唯一出处；子文件另写各自的「本域职责」注释，不复述装配契约 |
| `LogSecretSink`（65，trait 实现 66） | `main/secret_log.rs` | `pub(super)` | 薄根 904（`bootstrap_admin` 的 sink 实参） |
| `CONTROLLED_RECOVERY_ENV`（86） | `main/recovery.rs` | private | 仅 93 |
| `controlled_recovery_requested`（92） | `main/recovery.rs` | private | 仅 117 |
| `controlled_recovery_flag`（98） | `main/recovery.rs` | private | 93 + `recovery::tests` 2（§5） |
| `build_recovery_controller`（116） | `main/recovery.rs` | `pub(super)` | 薄根 891 |
| `new_recovery_controller`（121） | `main/recovery.rs` | private | 117 + `recovery::tests` 2 |
| `BACKGROUND_GRACE`（132） | `main/background.rs` | `pub(super)` | 薄根 1169（`settle_background_tasks` 实参）+ 测试 0（实测） |
| `BOARD_SHUTDOWN_GRACE`（135） | `main/lifecycle.rs` | `pub(super)` | `run_supervised_lifecycle` 796（同文件）+ `lifecycle::tests` 14（11 代码 + 3 注释，实测） |
| `RETENTION_CLEANUP_INTERVAL`（143） | `main/background.rs` | private | 仅清理循环体内（原 1064，随 `spawn_retention_cleanup`） |
| `build_transport_manager`（154） | `main/assembly.rs` | `pub(super)` | 薄根 895 |
| `time_gate_config_from_env`（178） | `main/startup_gate.rs` | `pub(super)` | 薄根 852 |
| `time_changed_sink`（202） | `main/startup_gate.rs` | `pub(super)` | 薄根 854 |
| `task_time_source`（214） | `main/assembly.rs` | `pub(super)` | `assembly::production_inputs` 295（同文件）+ `startup_gate::tests` 2（1493/1654） |
| `production_inputs`（228，含 `#[allow(clippy::too_many_arguments)]`） | `main/assembly.rs` | private | 薄根 938（`build_production_router(production_inputs(..))` 调用点） |
| `write_private_secret`（335） | `main/secret_log.rs` | private | 66 的 `emit`（同文件）+ `secret_log::tests` 3 |
| `start_if_ready`（351） | `main/startup_gate.rs` | `pub(super)` | 薄根 903 + `startup_gate::tests` 2 |
| `StartupTimeGate`（380，含 `impl` 387–427、`Drop` 429–438） | `main/startup_gate.rs` | `pub(super)`（`start`/`is_ready`/`finish` 为 crate root 级可见方法，签名不变） | 薄根 862/933 + `startup_gate::tests` 5 |
| `waiter_handle`（421，`#[cfg(test)]`） | `main/startup_gate.rs`（改为 `#[cfg(test)] impl` 扩展块，属性顶格） | private（test-only） | `startup_gate::tests`（1511） |
| `listen_after_startup_gates`（450） | `main/startup_gate.rs` | `pub(super)` | 薄根 930 + `startup_gate::tests` 5 |
| `shutdown_signal`（479） | `main/lifecycle.rs` | `pub(super)` | 薄根 1098 + 测试 0（实测） |
| `BoxedError`（510） | `main/lifecycle.rs` | `pub(super)` | 薄根 1153 + `lifecycle::tests` 多处（`boxed_err` 之外的 `BoxedError` 签名） |
| `boxed`（513） | `main/lifecycle.rs` | `pub(super)` | 薄根 1153 + `lifecycle::tests` 1（原 2553 的 `serve failed` 用例） |
| `BoardTask`（525，字段 `handle`/`done`/`observed` 保持 private，见 §4.2） | `main/lifecycle.rs` | `pub(super)`（结构体名与 `new`） | 薄根 1102/1128（`Option<BoardTask>` 类型标注与 `BoardTask::new` 构造；539 是 `new` 的定义行、1156 是 `run_supervised_lifecycle` 调用点，均非薄根对 `BoardTask` 的额外消费）+ `lifecycle::tests` 10（实测，全部为构造函数调用或类型标注） |
| `BoardOutcome`（536） | `main/lifecycle.rs` | `pub(super)` | `observe_board`/`finish_board` 返回类型（同文件），签名出现在 `pub(super)` 项上 |
| `BoardTask::new`/`watchable`/`observe`（539/548/553） | `main/lifecycle.rs` | `pub(super)` / private / private | `new`：薄根 1128 + `lifecycle::tests`；`watchable`：`supervise_shutdown` 706（同文件）；`observe`：`observe_board` 567（同文件） |
| `observe_board`（565） | `main/lifecycle.rs` | private | 仅 `supervise_shutdown` 722（同文件；测试引用 0，实测） |
| `sleep_or_pending`（573） | `main/lifecycle.rs` | private | 仅 `supervise_shutdown` 753（同文件；测试引用 0） |
| `watch_turns_true`（582） | `main/lifecycle.rs` | `pub(super)` | 薄根 1151（`with_graceful_shutdown` 驱动）+ `lifecycle::tests` 4 |
| `finish_board`（597） | `main/lifecycle.rs` | `pub(super)` | `supervise_shutdown` 771（同文件）+ `lifecycle::tests` 12 |
| `supervise_shutdown`（682） | `main/lifecycle.rs` | `pub(super)` | `run_supervised_lifecycle` 796（同文件）+ `lifecycle::tests` 5 |
| `run_supervised_lifecycle`（787） | `main/lifecycle.rs` | `pub(super)` | 薄根 1156 + `lifecycle::tests` 8 |
| `settle_background_tasks`（804） | `main/lifecycle.rs` | `pub(super)` | 薄根 1161 + `lifecycle::tests` 2（2006/2626，实测） |
| `main`（831–1,187，`#[tokio::main]`） | 薄根 `main.rs` | —（bin 入口） | — |
| `mod tests`（1,189–2,633） | 按 §5 拆分到 6 个归属文件 + 薄根 | `#[cfg(test)]` | — |

**新增项（拆分产物，非新行为）**：`main/background.rs` 的 `spawn_scheduler_loop(deps: SchedulerDeps, shutdown: watch::Receiver<bool>) -> JoinHandle<()>` 与 `spawn_retention_cleanup(deps: RetentionDeps) -> JoinHandle<()>`，以及两个参数聚合体 `SchedulerDeps`（`pool: sqlx::MySqlPool`、`transport: Arc<TransportManager>`、`process_epoch: uuid::Uuid`、`recovery: Arc<RuntimeRecoveryController>`）与 `RetentionDeps<'a>`（`db: &'a DbPool`、`recovery: Arc<RuntimeRecoveryController>`）。聚合体只为把 `main` 监听闭包内两段 200 行的 `tokio::spawn` 循环体整体上提到同域文件；**循环体内部逐行不变**（含 `interval`、`reap_ticks`、SQL 文本、`eprintln!` 文案、`continue` 语义），两个 `JoinHandle` 仍由薄根放进 `settle_background_tasks` 的收敛清单（顺序不变）。抽取参数即「必要的 `use`/移动」，MUST NOT 借机改签名语义或新增参数默认。

### 4.2 可见性变更清单（默认 private；仅一类偏离，逐项论证）

- **既有可见性**：本文件全部顶层项现状都是 **private**（bin crate root 的私有项）。拆分不产生任何 `pub`，也不产生 `pub(crate)` 的**新增对外面**。
- **由 private 升为 `pub(super)`（共 22 个顶层项，其中 2 项为拆分新增的聚合函数；另有随类型的字段若干，见下条）**：`LogSecretSink`、`build_recovery_controller`、`BACKGROUND_GRACE`、`BOARD_SHUTDOWN_GRACE`、`build_transport_manager`、`time_gate_config_from_env`、`time_changed_sink`、`task_time_source`、`StartupTimeGate`、`listen_after_startup_gates`、`shutdown_signal`、`BoxedError`、`boxed`、`BoardTask`（连同其 `BoardTask::new`；三个字段保持 private，见下条）、`BoardOutcome`、`watch_turns_true`、`finish_board`、`supervise_shutdown`、`run_supervised_lifecycle`、`settle_background_tasks`、`spawn_scheduler_loop`、`spawn_retention_cleanup`（含 `SchedulerDeps` 4 字段与 `RetentionDeps` 2 字段的 `pub(super)`）。
  > 计数口径：以上列出 22 个顶层项（含 2 个新增聚合函数）；其中 6 项（`CONTROLLED_RECOVERY_ENV` 等）在 §4.1 表中判为 private 者**不**提权。逐项消费方见 §4.1 表最后一列——判据唯一：拆分后「定义文件 ≠ 消费文件」才提权，同文件消费（含同文件 `mod tests`）一律保持 private。
  - **`pub(super)` 等值论证（最窄可见性）**：`main/` 下每个域文件都由 bin crate root（`main.rs`）以私有 `mod` 声明，其直接父模块即 crate root，故子文件内的 `pub(super)` 与 `pub(crate)` **有效可见性集完全相同**（都恰为「本 bin 的整个编译单元」），二者都不新增任何 crate 外的可见性；本设计一律取书写更小的 `pub(super)`（与 08 号子规格 §4.2 同一论证口径）。又因 `rsetup-controller` 的 lib target 不编译 `main.rs`，且本 bin 不导出任何供他方 `use` 的路径，bin crate 内提权的唯一效果是让**兄弟域文件与薄根**可达，不存在越界面。
  - **禁止项**：`pub(super)` 项 MUST NOT 在薄根再导出（薄根是父模块，`use` 子文件 private 项不可编译，见 08 号 §4.1 的同一教训）；MUST NOT 为消除 dead-code 告警而提为 `pub` 或加 `#[allow(dead_code)]`；MUST NOT 为测试新增任何生产侧接口。
  - **struct 字段**：`BoardTask` 的 `handle`/`done`/`observed` 三个字段**全部保持 private**——实测字段访问点仅 549（`watchable` 读 `done`）、554（`observe` 读 `done`）、601–607（`finish_board` 解构三字段并消费 `done`/`observed`）、739/745（`supervise_shutdown` 置空 `done`、写入 `observed`），**全部位于拆分后将同属 `lifecycle.rs` 的实现区**；`lifecycle::tests` 经 `BoardTask::new` + `BoardProbe` 交互、零字段直读（10 处引用全是构造调用或类型标注），且拆分后 `mod tests` 仍是 `lifecycle.rs` 的子模块、天然可见 private 字段，字段无需任何提权。随类型提权的只有**结构体名与 `new`**（`watchable`/`observe` 同文件消费，保持 private）。`SchedulerDeps`/`RetentionDeps` 的全部字段由薄根以字面量构造，故结构体名与字段均为 `pub(super)`（父模块字面量构造需类型名可见，private 类型名会触发 E0603；`pub(super)` 在 bin crate root 下与 `pub(crate)` 有效集相同，不新增 crate 外面）。判定方式：编译器 `private_fields`/`private_interfaces` 告警出现即提权，无告警即保持 private。

### 4.3 依赖方向（单向、无环）

`recovery`、`secret_log` 为叶子（只依赖 `rsetup_controller::*` 库面）；`startup_gate` → `assembly`（`task_time_source`，仅测试消费）；`background` 不依赖任何兄弟域；`lifecycle` 不依赖任何兄弟域生产代码，也无测试侧回边（实测 `BACKGROUND_GRACE` 全文件仅 132 定义与 1169 薄根消费两处引用，`lifecycle::tests` 对其零引用）；`assembly` → `recovery`（仅类型 `RuntimeRecoveryController` 的 `Arc`，来自库面，非兄弟域）与库内各线；薄根 → 全部 6 个域。测试侧跨文件边（`startup_gate::tests → assembly::task_time_source`）在 `--all-targets` 编译面内存在，不引入生产依赖。**环检查**：若把 `task_time_source` 同时暴露给 `startup_gate` 生产代码就会成环——本文把它放在 `assembly`、`startup_gate` 只在测试中消费，生产依赖图仍是 DAG。`use super::*;` 在子文件中只解析到 bin crate root 的 `use` 集合（薄根 `use` 清单 14–63 行按各域实际消费**逐项下沉**到子文件顶部，逐字保留路径，MUST NOT 改写成 `use rsetup_controller::prelude::*` 之类）。

## 5. 测试与 fixture 迁移映射（29 → 29 条用例 + 1 个 test-only 方法，逐条，不整搬）

原则（C3）：断言与被测行为同时位；各子文件私有 `mod tests` 以 `use super::*;` + 显式 `use crate::<域>::{…}` / `use crate::{…}` 承接原 glob 语义（薄根 `use` 与兄弟域项在子文件里解析不到）；测试名逐字不变，仅模块路径由 `tests::X` 变为 `<域>::tests::X`；fixture 保持 test-only、MUST NOT 晚于本文件首条消费者出现（各文件内一律先定义后使用，保持现文件相对顺序）。

**test-only fixture 清单（全部随所属域同批落地，MUST NOT 进入非 test 编译面）**：

- `startup_gate::tests`（原 1206–1383）：`FAKE_NTP_SOURCE`（1206）、`ControlledWallClock`（1263 + `WallClock` impl 1274）、`ShortSleeper`（1281）、`BlockedUntilRelease`（1291 + `TimeSampler` impl 1295）、`NoSampleSampler`（1318）、`WallFollowingSampler`（1327）、`spawn_controlled_gate`（1350）、`gate_deps`（1362）、`gate_config`（1372）、`boxed_err`（1381）。实测这些夹具**只**被 startup_gate 域 7 条用例消费（其余域零引用），故全部落 `startup_gate.rs`，不建跨文件共享夹具、不为此新增生产接口。
- `lifecycle::tests`（原 1756–2397）：`DropWitness`（1767 + `Drop` impl 1776）、`http_that_never_returns_by_itself`（1784）、`healthy_board_listener`（1795）、`BoardProbe`（1803 + `alive` 1811）、`spawn_board_task`（1818）、`spawn_supervisor`（2042）、`drive`（2058）、`poll_once`（2342）、`poll_until_ready`（2352）、`RacyBoardFixture`（2368）、`spawn_racy_board_task`（2375）。同域共用，全部落 `lifecycle.rs`。
- 无跨域 fixture（实测）：两个 fixture 组互不引用，`assembly`/`background`/`recovery`/`secret_log` 各自只用标准库与库面类型。

**逐条映射（30 项 = 29 用例 + 1 test-only 方法）**：

- **`main.rs`（薄根）`::tests`（1）**：`default_cargo_run_selects_readonly_server`(1684) —— 被测对象是薄根所属 bin 的 `include_str!("../Cargo.toml")` 相对路径与 `default-run` 约定；留在薄根可让 `include_str!` 基准目录逐字不变（若随 `assembly` 下沉，基准目录变 `src/main/`，须改写为 `../../Cargo.toml`，属额外风险）。该用例同时是「薄根存在且自动发现为 `rsetup-controller`」的自检。
- **`recovery.rs::tests`（2）**：`controlled_recovery_flag_only_accepts_one_and_true`(1211 → `controlled_recovery_flag`)、`new_recovery_controller_enters_controlled_recovery_before_returning`(1237 → `new_recovery_controller`)。
- **`secret_log.rs::tests`（2）**：`secret_sink_reports_actual_create_new_failure_without_overwrite`(1730 → `LogSecretSink::emit` + `write_private_secret`)、`first_boot_secret_is_private_and_never_overwritten`(1742 → `write_private_secret` 的 `create_new`+`0600`)。
- **`startup_gate.rs::tests`（7 用例 + `waiter_handle`）**：`listener_holds_until_the_time_gate_finishes`(1386)、`fallback_gate_still_listens_but_the_clock_stays_untrusted`(1467，含 `task_time_source` 跨文件消费)、`bootstrap_or_recovery_failure_never_reaches_the_listener`(1500，消费 `waiter_handle`)、`pre_listen_beyond_the_ready_timeout_adds_no_extra_wait`(1549)、`pre_listen_within_the_ready_timeout_only_finishes_the_remainder`(1599)、`injected_time_source_tracks_the_same_gate_snapshot`(1642，消费 `task_time_source`)、`startup_blocks_bootstrap_and_listener_when_schema_not_ready`(1694 → `start_if_ready`)。
- **`lifecycle.rs::tests`（17）**：`n1_board_error_broadcasts_shutdown_and_finishes_the_http_that_never_returns`(1841)、`n1_board_task_panic_is_observed_and_its_original_task_is_aborted`(1878)、`n1_board_task_overrunning_the_grace_is_aborted_not_detached`(1910)、`n1_supervise_joins_both_sides_when_board_task_hangs_after_panic`(1937)、`n2_settle_aborts_the_original_owner_tasks_not_a_wrapper`(1978)、`rc2_listening_phase_does_not_arm_the_board_grace`(2070)、`rc2_shutdown_intent_arms_the_grace_and_a_healthy_board_drains_by_itself`(2108)、`rc2_grace_is_anchored_at_shutdown_intent_not_at_http_completion`(2154)、`rc2_without_board_task_the_supervisor_still_shuts_down_cleanly`(2213)、`rc2_shutdown_already_established_before_supervise_starts_arms_immediately`(2239)、`rc2_shutdown_and_board_failure_in_the_same_round_are_consumed_once`(2267)、`rc2_board_ok_without_any_shutdown_request_is_reported_not_swallowed`(2295)、`rc2_external_shutdown_arriving_before_the_board_receipt_is_not_unprompted`(2403)、`rc2_board_ok_between_two_polls_is_still_fail_closed_without_external_evidence`(2495)、`rc2_http_failure_broadcasts_shutdown_and_board_drains_by_itself`(2550，消费 `boxed`)、`rc2_finish_board_joins_the_original_task_after_its_result_arrives`(2580)、`n2_settle_returns_early_when_every_owner_exits_by_itself`(2617)。**本域计数核对（实测）**：17 = 4 条 `n1_*`（1841/1878/1910/1937）+ 2 条 `n2_settle_*`（1978/2617）+ 11 条 `rc2_*`（2070/2108/2154/2213/2239/2267/2295/2403/2495/2550/2580）；与其余域合计 1 + 2 + 2 + 7 + 17 = **29**，与全文件 `#[test]` 5（1210/1236/1683/1729/1741）+ `#[tokio::test]` 15 + `#[tokio::test(start_paused = true)]` 9（2069/2107/2153/2212/2238/2266/2402/2494/2579）= 29 逐项相等，无重复、无遗漏。
- **`assembly.rs::tests`（0）**：`production_inputs`、`build_transport_manager`、`build_recovery_controller`、`time_changed_sink` 在 `main.rs` 测试区实测**零直接引用**（脚本核对：`build_transport_manager`=0、`production_inputs`=0、`build_recovery_controller`=0、`time_changed_sink`=0），其正确性由 `tests/production_composition.rs`、`tests/sse_time_events.rs`、`tests/transport_*` 等集成测试承担。因此**不**为本域新增内联测试，也 MUST NOT 为「凑测试覆盖」新增薄封装或提权。
- **`background.rs::tests`（1，新增用例，非搬移）**：`scheduler_deps_field_set_is_exact` —— 编译期证明 `SchedulerDeps` 的字段集合恰为 `{pool, transport, process_epoch, recovery}`（写全字段字面量 + 逐字段取值），沿用 [tests/production_composition.rs](../../../../crates/rsetup-controller/tests/production_composition.rs) `production_inputs_exposes_no_second_pool`(1110) 的既有形状断言风格，杜绝 `..Default::default()` 兜底掩盖漏传。**用例计数口径**：全文件 `#[test]`/`#[tokio::test]` 属性总数由拆分前 **29** 变为拆分后 **30**（唯一新增即本条）；实施时 MUST NOT 再新增其它用例，MUST NOT 删除或合并任何既有用例，回归核对方式为「按 §5 名单逐条比对 + `cargo test -p rsetup-controller --bin rsetup-controller --locked`（主 bin **单 target**：拆分前 29 条、拆分后 30 条）」。MUST NOT 拿 `--bins` 当计数口径：另两个 bin `migrate-controller` 与 `reset-admin` 各有内联测试 5 条与 8 条（实测），`--bins` 实际命中 42（拆分后 43）。命名口径：bin target 的 crate root 就是 `main.rs`，其内联测试的完整路径**不带** `main::` 前缀（拆分前为 `tests::<名>`，拆分后为 `<域>::tests::<名>`），因此**不得**用 `-- main::` 作筛选条件，按函数名末段筛选（如 `-- rc2_board_ok`）在拆分前后同样命中。
- **筛选前缀变化（DoD 核对式）**：拆分前这些用例的完整路径是 `tests::<名>`（bin 单文件）；拆分后为 `secret_log::tests::<名>`、`recovery::tests::<名>`、`startup_gate::tests::<名>`、`lifecycle::tests::<名>`、`<根>::tests::default_cargo_run_selects_readonly_server`。**末段函数名逐字不变**，因此按名过滤（`cargo test -p rsetup-controller -- rc2_board_ok`）前后同样命中。

**测试体内 `use` 接线（属 `use` 改动，断言文本零改动，C2/C3）**：`startup_gate.rs::tests` 需 `use crate::assembly::task_time_source;`；`lifecycle.rs::tests` 需 `use crate::lifecycle::{boxed, BoxedError, finish_board, …};`（由 `use super::*;` 承接）——**不要**添加 `use crate::BACKGROUND_GRACE;` 或任何 `background` 域项（实测 lifecycle 测试对 `BACKGROUND_GRACE` 零引用，照加会触发 `unused_imports` 并挂 `clippy -D warnings`）；`background.rs` 与 `assembly.rs` 需要把原薄根 14–63 行 `use` 中本域用到的项逐条搬入（`background`：`sqlx`、`transport::board_client`、`tasks::{scheduler, scheduler_store, evidence}`、`operations::{clean_retention_data, RetentionPolicy}`、`DbPool`、`RuntimeRecoveryController`、`JoinHandle`、`watch`、`Duration`）。

## 6. 门禁接线：三处源码文本作用域的等价扩展（本规格核心约束）

**总原则**：作用域从「单文件 `src/main.rs`」等价扩展为「`src/main.rs` + `src/main/**/*.rs` 的全部生产实现文件」；**每个 pattern 的语义（期望命中/期望不命中、Fail-Closed 条件、计数值、token 表）逐项不变**；不保留假调用、不为凑 token 写占位注释。收集器一律**目录枚举**，天然不漏新增文件（08 号 §6 先例）。

### 6.1 生产段定义的等价（三处共用）

原 G-PC2 用 `main.split("\n#[cfg(test)]").next()` 取「测试段之前的生产段」。等价定义扩展为：**对收集到的每个文件，各自按首个顶格 `\n#[cfg(test)]` 截断，再按路径升序拼接**。该定义在拆分前后对原断言逐字等价，前提是 §3 的约定成立：**每个实现文件的首个顶格 `#[cfg(test)]` 必须是 test-only 区的起始（`mod tests` 声明或 test-only `impl` 扩展块均可），其后不得再有生产代码**（`waiter_handle` 改为顶格扩展块并紧邻置于 `mod tests` 之前，正是为此——`startup_gate.rs` 的两个顶格属性相邻，截断点是第一个，两个 test-only 区都被截掉）。收集器 MUST 同时丢弃 `mod tests` 之后的内容，否则 `lifecycle::tests` 的 fixture 文本（例如未来出现 `run_production_migration` 字样的测试名）会把负向断言喂红。

### 6.2 G-M6：`make metrics-static-gates` 第 6 条（硬约束，实测 [Makefile](../../../../Makefile) 190–192 行）

现状命令（Makefile 记法，`$$` 为 make 转义）：

```make
@rc=0; hits=$$(grep -rn 'build_production_router_with_metrics_source\|SqlxMetricsSnapshotSource::new' crates/rsetup-controller/src/main.rs) || rc=$$?; \
 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：grep 异常退出码 $$rc（main.rs 缺失），Fail-Closed" >&2; exit 1; fi; \
 if [ -n "$$hits" ]; then echo "metrics 门禁 6：生产入口不得触碰测试注入缝或自建汇总源" >&2; printf '%s\n' "$$hits" >&2; exit 1; fi
```

失败模式（本规格要解决的唯一「静默失效」风险）：目录化后若只把路径写成 `crates/rsetup-controller/src/main/`，则薄根里的 token 逃逸扫描；若路径整体写错或目录不存在，`grep` 退 2 会 Fail-Closed（安全但 CI 常红）；若写成 `main*` 之类 glob，无匹配时 glob 原样传给 `grep` → 退 2 → 恒红。因此设计如下（拟建，本轮不落地）：

```make
# 第 6 条（拆分后等价形态）：薄根 main.rs 必须存在（目录化后仍是 bin 入口），
# 生产入口的扫描面 = main.rs + src/main/ 子树全部 .rs（目录枚举，不漏新增文件）。
# 扫描面为空即 Fail-Closed，绝不因 main/ 尚未创建而放行。
# find 不得放进管道：管道下 `|| rc=$?` 只取最右侧 sort 的退出码（恒 0），find 的错误会被吞掉；
# 必须先无管道落入变量取 rc、判完 rc 与空集，再交给 sort 稳定顺序。
@rc=0; filelist=$$(find crates/rsetup-controller/src/main.rs crates/rsetup-controller/src/main \
	 -type f -name '*.rs' 2>/dev/null); rc=$$?; \
 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：源文件枚举异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
 if [ -z "$$filelist" ]; then echo "metrics 门禁 6：未找到 crates/rsetup-controller/src/main.rs，Fail-Closed" >&2; exit 1; fi; \
 files=$$(printf '%s\n' "$$filelist" | sort); \
 rc=0; hits=$$(grep -n 'build_production_router_with_metrics_source\|SqlxMetricsSnapshotSource::new' $$files) || rc=$$?; \
 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：grep 异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
 if [ -n "$$hits" ]; then echo "metrics 门禁 6：生产入口不得触碰测试注入缝或自建汇总源" >&2; printf '%s\n' "$$hits" >&2; exit 1; fi; \
 rc=0; anchors=$$(grep -n 'build_production_router(' crates/rsetup-controller/src/main.rs) || rc=$$?; \
 if [ $$rc -gt 1 ]; then echo "metrics 门禁 6：装配锚点 grep 异常退出码 $$rc，Fail-Closed" >&2; exit 1; fi; \
 if [ $$rc -ne 0 ] || [ -z "$$anchors" ]; then echo "metrics 门禁 6：薄根 main.rs 必须保留 build_production_router( 装配点" >&2; exit 1; fi
```

等价性与不弱化论证：

1. **正向作用域只增不减**：负向 pattern 的扫描集合 ⊇ 原集合（`main.rs` + 全部子文件），任何子文件命中即失败，不产生新的放行面。
2. **文件缺失 Fail-Closed 保留**（机理已更正）：GNU `find` 对**显式给定的不存在路径**打印错误并退 **1**，不会把它当空目录静默退 0；`stderr` 已被抑制，`rc=1` 由 `-gt 1` 阈值豁免——这是对 `main/` 尚未创建（批 A 前）与回滚后两种过渡态的**唯一**容忍。薄根 `main.rs` 与 `main/` 同缺 ⇒ 集合为空 ⇒ 显式 `exit 1`；只缺薄根（`main/` 在）⇒ 第三条 grep 对缺失文件退 2 ⇒ Fail-Closed。原语义「main.rs 缺失即报错」严格保留。`|| rc=$$?` 绝不能写在管道上：那样取到的是 `sort` 的恒 0，第一条 Fail-Closed 分支永不执行。
3. **消除「无事发生即通过」**：原第 6 条是纯负向 grep，天然存在「扫空目录也绿」的空集风险；新增的第三条 grep 以**薄根必含 `build_production_router(`** 为正向锚点，把「装配点位置」钉成可判定条件。该锚点由 §6.4 归属表固定（薄根），并写入 §6.5 的注释约定，属**加强**而非弱化。
4. **不弱化既有 CI 合同**：目标名 `.PHONY: metrics-static-gates`、CI 步骤 `run: make metrics-static-gates`、`$$(git diff --cached --name-only)` / `$$(git ls-files)` 两条扫描与其后的 `controller\.toml`、`^secret/` 过滤全部逐字不变（[controller-ci-contract.test.mjs](../../../../scripts/tests/controller-ci-contract.test.mjs) 489–560 行）；不新增目标、不改依赖顺序 → 总览 F8 的「spec → Makefile → CI → 合同测试」链路里 CI 与合同测试两步**无需改动**，但 Makefile 既然改动，实施批 MUST 跑 `node --test scripts/tests/*.test.mjs` 复核合同。
5. **不改第 5/7/8/9 条与末段两条扫描**：它们的作用域是 `src/metrics/**` 与 `src/metrics/auth.rs`、`src/metrics/collect.rs`，与本文件无关。

### 6.3 G-V4：`tests/v4_baseline.rs` 的 main 侧断言

`read_source("src/main.rs")` 改为 `read_main_sources()`（新增测试辅助，与 08 号 §6-1 同构）：读 `manifest_dir()/src/main.rs`（缺失即 `panic!("missing src/main.rs root")`）；递归遍历 `manifest_dir()/src/main/`（目录不存在返回空集）收集全部 `*.rs`；按路径升序去重后逐文件读取。三条断言**逐字不变**，只把被检文本换成收集结果：

- `contains("check_identity_schema")`：拆分后由**薄根**满足（`check_identity_schema(&db)` 属 §2-3 冻结的启动编排，不迁移）。收集器化后即使实现搬移也只要求「集合中至少一处」，不会误红。
- `!contains("run_production_migration") && !contains("create_v4_schema")`：负向、只增范围；§6.5 的注释约定要求全部 main 侧文件（含注释）都不出现这两个字面量。
- 同用例内 `read_source("src/db.rs")` 的 `create_v4_schema(` 恰 1 计数与 `schema/v4.sql` 存在性断言属 08 号 §6，本规格**不动**（仅在同一集成测试文件内并存，互不影响）。

### 6.4 G-PC1 / G-PC2 与各 pattern 的归属文件对照表

`production_composition.rs` 的两处 `include_str!("../src/main.rs")` 改为同包内共享的测试辅助 `main_production_sources()`（收集 §6.1 定义的生产段），断言表达式逐字保留。归属核对：

| pattern（期望） | 现状所在行 | 拆分后归属 | 收集器作用域 | 等价性论证 |
| --- | --- | --- | --- | --- |
| `build_production_router(`（必含，G-PC1） | 938（薄根 `main` 的装配调用） | **薄根 `main.rs`** | 全部生产段 | 装配点是「唯一入口」语义的载体，冻结在薄根；`include_str!` 若继续单指向薄根也成立，但统一走收集器以免再次漂移 |
| `build_production_router_with_metrics_source`（必不含，G-PC1 + G-M6） | 0 | 无（薄根与全部子文件的代码与注释都不得出现） | 全部生产段 | 负向、范围只增 |
| `SqlxMetricsSnapshotSource`（必不含，G-PC1；G-M6 用更长的 `::new` 形式） | 0 | 无（本文件连类型名都不提及；§2-11 比 Makefile pattern 更严） | 全部生产段 | 收集器覆盖子树，避免「注入缝挪进 `assembly.rs` 注释」的静默放行 |
| `check_identity_schema`（必含，G-V4） | 903 | **薄根**（启动编排） | 全部生产段 | 同上 |
| `run_production_migration` / `create_v4_schema`（必不含，G-V4） | 0 | 无 | 全部生产段 | 负向、范围只增 |
| `clean_retention_data(`（必含，G-PC2） | 1077（保留期清理循环体） | **`main/background.rs`** | 全部生产段 | 随循环体迁移；收集器是唯一可行方案（薄根不再含该 token） |
| `RETENTION_CLEANUP_INTERVAL`（必含，G-PC2） | 143 / 1064 | **`main/background.rs`** | 全部生产段 | 同上 |
| `retention_recovery.allows_scheduling()`（必含，G-PC2） | 1074 | **`main/background.rs`** | 全部生产段 | 逐字保留表达式文本（变量名 `retention_recovery` 不改名，C2） |
| `retention_task,`（必含，G-PC2，带逗号） | 1167（`settle_background_tasks` 收敛清单） | **薄根 `main.rs`** | 全部生产段 | 逗号形态在清理任务 spawn 处是 `retention_task =`（现 1063），只有收敛清单元素带逗号——归属表把「spawn 在 background、收敛在薄根」的差异写清，收集器使两条都命中 |
| `#[cfg(test)]` 分段锚点（G-PC2 截断依据） | 1,189（顶格） | 各文件的 test-only 区（`mod tests` 与 §3 的 `waiter_handle` 顶格扩展块）属性必须顶格，首个顶格属性即生产段截断点（§6.1 前提） | 每文件独立截断 | 保证「生产段」定义在拆分前后逐字节等价 |

**不保留假调用**：薄根不得为迎合任何 pattern 新增占位调用、空注释或 `let _ = x;`；每个 token 必须出现在其**语义真实**的位置（表内「归属」列）。

### 6.5 跨文件注释约定（防「注释把负向门禁喂红 / 喂绿」）

`src/main.rs` 与 `src/main/**/*.rs` 的注释与文档字符串 MUST NOT 出现下列字面量：`build_production_router_with_metrics_source`、`SqlxMetricsSnapshotSource`、`run_production_migration`、`create_v4_schema`。需要指代注入缝/迁移入口时，写成「测试注入缝（见 `production.rs`）」「显式建库入口（见 `db/production.rs`）」这类不含被禁字面量的描述。该约定同时保护 G-M6 与 G-V4/G-PC1 的负向断言，并在 §9 风险 2 设 DoD 检查。

## 7. 分批顺序与回滚（每批设计为可编译，本轮一律未编译验证）

单提交单批、逐批可独立 `git revert`。「暂留」= 仍留在薄根/上一文件的代码与其配套 `use`；「回改点」= 下一批收尾清理。**门禁接线与骨架同批（批 A）**，杜绝「目录已建、扫描未扩」窗口。

1. **批 A（骨架 + 三处接线同批）**：新建 `src/main/` 目录与**空**薄根声明（`mod recovery; mod secret_log;` 先只声明本批实际用到的两项，或先不声明任何子模块），把 §6.1 的收集器等价性接线一次做齐：Makefile 第 6 条（§6.2 三段式）+ `tests/v4_baseline.rs::read_main_sources()` + `tests/production_composition.rs` 的 `main_production_sources()`。此时实现全部仍在薄根，收集结果 = `{src/main.rs}`，四条 pattern 与三项断言的作用域即刻等价且**逐字节同旧行为**。**暂留**：全部实现与全部 29 条测试仍在薄根。**验证出口**：`make metrics-static-gates`（只读、可本地执行）+ `cargo test -p rsetup-controller --test v4_baseline --locked`（10 用例）+ `cargo test -p rsetup-controller --test production_composition --locked` + `node --test scripts/tests/*.test.mjs`。**回改点**：无。
2. **批 B（叶子域 + 测试同批）**：抽 `main/secret_log.rs`（+2 测试）与 `main/recovery.rs`（+2 测试）；同批补 §4.2 的 `pub(super)`（`LogSecretSink`、`build_recovery_controller`）。收集器自动纳入新文件，门禁无需再改。测试命名核对：`--secret_log::tests::` 命中 2、`--recovery::tests::` 命中 2，`cargo test -p rsetup-controller --bin rsetup-controller --locked`（主 bin 单 target）用例总数仍为 29（批 D 新增 1 条形状用例后到 30；用例名末段逐字不变）。**暂留**：其余实现与 25 条测试仍在薄根。**回改点**：薄根 `use` 中被搬走项的冗余 import 删除。
3. **批 C（生命周期域，测试随域）**：抽 `main/lifecycle.rs`（实现 501–829 + 475–499，17 条测试 + 全部监督 fixture）；**同批完成** §4.2 的 `BOARD_SHUTDOWN_GRACE`/`BoxedError`/`boxed`/`BoardTask`（连同 `new`；`handle`/`done`/`observed` 三字段保持 private，§4.2）/`BoardOutcome`/`watch_turns_true`/`finish_board`/`supervise_shutdown`/`run_supervised_lifecycle`/`settle_background_tasks`/`shutdown_signal` 提权与测试侧 `use` 接线。本批结束后薄根仍持有调度/清理两段循环（未抽），G-PC2 的四个 token 分布为「background 域 tokens 暂在薄根」，收集器同时覆盖两侧 ⇒ 批 C 出口即绿。**回改点**：`lifecycle::tests` 内若仍有 `use super::*` 之外的中间态 glob，批 E 收敛。
4. **批 D（后台任务域，含 G-PC2 token 迁移）**：抽 `main/background.rs`：`BACKGROUND_GRACE`、`RETENTION_CLEANUP_INTERVAL`、`spawn_scheduler_loop`、`spawn_retention_cleanup`（两段循环体逐行搬移）+ `SchedulerDeps`/`RetentionDeps` + §5 的 1 条新增编译期形状用例；薄根监听闭包改为调用两函数并把返回句柄继续放入 `settle_background_tasks`（顺序与实参不变）。**本批是 G-PC2 的 token 迁移点，必须与收集器（批 A 已就位）共同验证**：`cargo test -p rsetup-controller --test production_composition --locked` 的 `retention_cleanup_runs_in_the_production_background` 必须保持通过，且 MUST 反查「四个 token 各只出现在预期文件」（§6.4 表），防止漏 scan 造成的假绿。**暂留**：装配域未抽。**回改点**：`main` 监听闭包内残留的中间变量与 `use`。
5. **批 E（装配域 + 启动门禁域收尾）**：抽 `main/assembly.rs`（`build_transport_manager`/`task_time_source`/`production_inputs`，`time_gate_config_from_env`/`time_changed_sink` 归 `main/startup_gate.rs`）与 `main/startup_gate.rs`（`start_if_ready`、`StartupTimeGate` + `Drop` + `#[cfg(test)]` 扩展块、`listen_after_startup_gates` + 7 条测试 + 门禁 fixture），同批完成对应 `pub(super)` 与测试 `use` 接线；最后把薄根收敛为 §3 估算的约 280–300 行（文档注释 + `use` + 6 个 `mod` 声明 + `main()` 本体 357 行经批 D 抽走两段循环体约 129 行后仍余约 240 行 + `default_cargo_run_…` 测试）。**回改点**：删除全部中间批的临时 `use`/glob；此后无暂留实现、无待回改 `use`。
6. **每批的编译前自检（实施会话执行，本规格不代跑）**：`make fmt-check`（F1）+ `cargo clippy -p rsetup-controller --all-targets -- -D warnings`（F2 前提：先 `make web-dist-check`）+ `make metrics-static-gates` + `cargo test -p rsetup-controller --locked`；任一批出现 dead-code/未提权（路径不可解析）即**停在该批修复**，不得带入下一批，也不得用 `#[allow]` 掩盖。
7. **回滚**：每批单提交，回滚即 `git revert`；批 A 的回滚会把 Makefile 第 6 条与两个集成测试一起退回旧作用域（此时实现仍在薄根，退回后仍然成立）；无行为开关、无 feature flag、无数据迁移。若仅批 D 需回滚，`background.rs` 文件随 `git revert` 一并消失，收集器（目录枚举）自然收缩为薄根集合——**不存在残留空目录导致门禁语义漂移**（`find` 对缺失的显式路径报错并退 1、stdout 为空集，退 1 由 `-gt 1` 阈值豁免；薄根本身缺失由其锚点 grep 退 2 Fail-Closed 兜底，§6.2 论证 2）。

## 8. 验证命令（登记项，本轮一律未执行、不声称通过）

前提说明：`cargo` 侧命令均要求 web dist 在位（总览 F2：`rsetup-controller` 的 build.rs 在 `apps/controller-web/dist` 缺失即失败，先 `make web-dist-check` 或 `make web-build`）、工具链 stable、`--locked` 是 **cargo 参数**（置于 `--` 之前）；F3 的 offline 只表示不初始化/不触碰数据库，干净检出下 `web-dist-check` 仍可能执行 `npm ci` 与 vite 构建（可 `NPM_CI_FLAGS=--offline`）。真实包名 `rsetup-controller`。本轮（写文档）**一条都未执行**。

1. `make metrics-static-gates`：纯只读静态门禁，本地可直接执行、不编译、不联网、不需要 dist；**本规格会话未运行**，实施每批 MUST 运行（唯一直接验证 §6.2 的命令）。
2. `make fmt-check`（F1 口径，禁用 `cargo fmt --all -- --check`）。
3. `cargo clippy -p rsetup-controller --all-targets -- -D warnings`（F2 前提）。
4. `cargo test -p rsetup-controller --locked`：全量内联 + 集成；定向 `cargo test -p rsetup-controller --bin rsetup-controller --locked`（主 bin **单 target**）应命中 30 条（29 既有 + 1 新增形状用例；MUST NOT 以 `--bins` 为计数口径——`migrate-controller` 与 `reset-admin` 两个 bin 各有内联测试 5 与 8 条，`--bins` 合计拆分前 42、拆分后 43；域前缀为 `secret_log::tests::` 2、`recovery::tests::` 2、`startup_gate::tests::` 7、`lifecycle::tests::` 17、`background::tests::` 1、薄根 `tests::` 1，名单按 §5 逐条比对，末段名逐字不变）。
5. `cargo test -p rsetup-controller --test v4_baseline --locked`（10 用例，覆盖 §6.3 接线）与 `cargo test -p rsetup-controller --test production_composition --locked`（覆盖 §6.4 两条静态门禁 + `production_inputs` 装配断言）；两者均不触库。
6. 生命周期回归重点：`cargo test -p rsetup-controller -- rc2_`、`-- n1_`、`-- n2_`（拆分前后命中数必须一致：`rc2_*` 11、`n1_*` 4、`n2_*` 2，合计 17，即 §5 的 lifecycle 域清单）。
7. 交付面自检：`cargo build -p rsetup-controller --bin rsetup-controller`（等价 `make controller-bin`，自动处理 dist 先决）后，`target/debug/rsetup-controller --print-build-info` 应**静默忽略**未知参数并继续走启动路径（现状无 argv 解析；不新增参数解析即为通过标准）。
8. 涉及 Makefile 接线的批次（仅批 A）：`node --test scripts/tests/*.test.mjs`（CI 接线合同，Node 22 口径）；未改 Makefile 的批次不需要执行。
9. 离线全量出口：`make controller-test-offline`（F2/F3）。
10. 真库轨道（总览 F6，**仅登记**，本轮未运行、禁止未授权执行）：`crates/rsetup-controller/tests/` 下 `mysql_identity.rs`、`tidb_identity.rs`、`metrics_real_db.rs`、`task_scheduler_sqlx.rs`、`v4_schema_readonly_verify.rs` 的 `--ignored` 轨道。本文件的拆分不改变它们的编译面（这些轨道经 lib 的 `pub` API 消费，与 bin 的 `main.rs` 布局无关）。

## 9. 模块专属风险与 DoD

- **风险 1（门禁静默退化，最高）**：三处消费者任一未接线或作用域只覆盖部分文件 → 负向 pattern 与 G-PC2 的四条正向断言「扫不到就算通过」。DoD：接线全部落在批 A（先于任何实现搬移），§6.4 归属表逐 token 反查（`grep -c` 按文件），批 D 出口实测 `retention_cleanup_runs_in_the_production_background` 仍绿且 token 归属与表一致。
- **风险 2（注释喂门禁）**：拆分说明若把 `build_production_router_with_metrics_source` / `SqlxMetricsSnapshotSource` 的字面量写进 `main/` 子树注释，会红；反之若收集器只取代码段而漏了注释，会假绿。DoD：按 §6.5 约定执行；收集器按行读取**全文**（不剥离注释），并核对拆分后的注释不引入被禁字面量。
- **风险 3（生命周期时序回归）**：`supervise_shutdown` 的分支顺序、`armed` 起表与 `external_shutdown` 取证时机若在「搬移 + 缩进调整」中被改写，RC2 类缺陷会复活且 11 条 `rc2_*` 是唯一防线。DoD：`git diff --color-moved` 证明纯移动；`lifecycle::tests` 17 条零改写（断言文本、`start_paused` 属性、`BOARD_SHUTDOWN_GRACE` 实参引用逐字不变）。
- **风险 4（薄根装配锚点被搬走）**：若实施把 `build_production_router(` 调用下沉到 `assembly.rs`，§6.2 的第三条 grep（薄根正向锚点）会红。DoD：§6.4 表要求装配点留薄根；若确有理由下沉，MUST 同步把该 grep 的路径改为收集面并把「唯一入口」正向断言移到 `production_composition`，并在本规格补记（不得只改一侧）。
- **风险 5（`include_str!` 基准目录）**：`default_cargo_run_selects_readonly_server` 依赖薄根的 `include_str!("../Cargo.toml")`；本设计把该用例留在薄根（§5）。DoD：`Cargo.toml` 无改动、该用例不改写路径。
- **风险 6（生产段截断）**：`waiter_handle` 的缩进 `#[cfg(test)]` 若不改为顶格扩展块，`startup_gate.rs` 的「首个顶格 `#[cfg(test)]`」仍是 `mod tests`，截断结果不变（本项风险低），但机械核对规则被破坏。DoD：`grep -n '^#\[cfg(test)\]' src/main/*.rs`——六个域文件中 `assembly.rs`/`background.rs`/`lifecycle.rs`/`recovery.rs`/`secret_log.rs` 各恰 1，`startup_gate.rs` 恰 2 且相邻（test-only 扩展块紧邻其 `mod tests`，两者之间无生产代码）；薄根另恰 1。
- **风险 7（基线漂移）**：见前言——保留期清理段与 `Makefile`/`tasks/scheduler*.rs` 的并行未提交改动。DoD：每批开工前重跑 §1 锚点核对；锚点变化时先更新本文再实施，禁止沿用过期行号。
- **阈值（C4）**：§3 各文件估算均 ≤ 900 行（最大 `lifecycle.rs` 约 620，其中测试约 470），无例外申请；若实施后 `lifecycle.rs` 因 17 条用例 + 11 个 fixture 超 900 且 ≤ 1,500，按 C4 在实现 PR 描述中提交内聚性论证（监督状态机与其手动 poll 竞态用例必须同位）；> 1,500 才申请例外——本设计预计不发生（再拆方案：把 `finish_board`/`BoardTask` 与 `supervise_shutdown` 分文件，代价是 §2-5 的停机语义被切到两个文件，本轮不采用）。
- **DoD（全部满足方为完成）**：§3 目标树与 §4.1 归属表逐项一致（薄根保留 bin 入口与三个门禁锚点）；§4.2 提权项全部为 `pub(super)`、零新增 `pub`、零新增 `pub(crate)` 对外面、struct 字段随字段判据；§5 映射 29 → 29 条既有用例逐条同位（名称/断言/属性零改写）+ 1 条新增形状用例，计数与名单逐项核对；§6 三处消费者的 pattern 归属与 §6.4 表一致且 `make metrics-static-gates`、`--test v4_baseline`、`--test production_composition` 三条在实施会话内真实执行并如实报告；§2 十一项不变量 `git diff` 复核为零（纯移动）；每批回滚点可用；未跑真库不声称真库通过。

### 9.1 C1–C9 逐条自检

| 契约 | 本设计的落实方式 | 自检方式 |
| --- | --- | --- |
| C1 结构与可见性 | bin 入口原地目录化（`main.rs` + `main/`），bin 名/`default-run`/Debian/systemd 零改动；无新增 `pub`；提权项一律 `pub(super)` 并逐项给消费方 | §4.1/§4.2 表逐项 + 编译（bin 唯一入口） |
| C2 行为与协议 | 环境变量集合、启动顺序、日志文本、退出码、停机顺序、绑定地址全部锁定 | §2 十一项 + `git diff` 零改写 |
| C3 测试迁移 | 29 条按被测职责同位（fixture 不跨域、不晚于首消费者），无整搬 `tests.rs`；新增仅 1 条形状用例并登记 | §5 名单计数 + 主 bin 单 target（`--bin rsetup-controller`）命中 30（非 `--bins`，§5/§8） |
| C4 规模与阈值 | 最大文件约 620 行，无例外申请 | §3 估算 + 实施后 `wc -l` |
| C5 ui/app.js | 不适用（非 `ui/`） | — |
| C6 范围排除 | 窄例外共 3 处：`Makefile` 第 6 条 grep 作用域 + 两个集成测试的源码收集接线；只改作用域，不改断言/token/计数/拒绝语义、不改用例数；待最终总览登记 | §6 + 前言登记项 |
| C7 秘密与证据 | 无凭据、无 `secret/` 内容、未连库；命令去敏；本轮未执行任何命令 | 全文检索确认 |
| C8 最低内容 | 状态/基线 前言、锚点 §1、目标树 §3、符号表 §4、依赖 §4.3、测试 §5、门禁 §6、批次回滚 §7、命令 §8、风险与 DoD §9 | 逐节存在即通过 |
| C9 流程 | 单份子规格、只写文档、不改源码/测试/Makefile、不提交 | 本任务声明 |

## 10. 本轮交付与未交付

- 已交付：本文一份（10 号子规格，状态：待用户审阅，尚未实施）。
- 未交付（本任务边界内刻意不做）：`src/main.rs` 与 `src/main/` 子树的任何源码改动；[Makefile](../../../../Makefile) 第 6 条 grep 的实际改写、`tests/v4_baseline.rs` / `tests/production_composition.rs` 收集器接线的实际落地（§6 为设计 + 窄例外登记）；任何编译、测试执行、`make` 目标运行（含可只读执行的 `make metrics-static-gates`）；任何真库 / `--ignored` 轨道执行；`Cargo.toml`/`Cargo.lock`/CI/`scripts/tests/*.test.mjs` 改动；总览 §4 中 10 号状态行与 C6 窄例外的同步（由总览维护方处理）。

## 11. 修订记录（独立审查后定点修订，逐项已修订，待复核，不构成 PASS 结论）

第 2 轮（本轮）：上轮确立的拆分骨架、批次顺序与门禁接线设计经复核全部成立，本轮修正 6 项 Important + 4 项 Minor，均为文档级定点修订，骨架不变：

1. **I1（§1.1/§5）**：测试属性细分更正为 `#[test]` 5（1210/1236/1683/1729/1741）+ `#[tokio::test]` 15 + `#[tokio::test(start_paused = true)]` 9（2069/2107/2153/2212/2238/2266/2402/2494/2579）= 29（原误记「5 + 19 + 5」），用例总数 29 不变。
2. **I2（§5/§8，另同步批 B 与 §9.1 C3 自检列）**：`--bins` 恰 30 的口径不成立——`migrate-controller` 与 `reset-admin` 各有内联测试 5 与 8 条（实测），`--bins` 实际命中 42（拆分后 43）。统一改为**主 bin 单 target**：`cargo test -p rsetup-controller --bin rsetup-controller --locked`（拆分前 29 → 拆分后 30）。
3. **I3（§6.2，另同步 §7 回滚注记）**：G-M6 拟改 recipe 第一段修复两处死防线——`find … | sort) || rc=$$?` 在管道下只取 `sort` 的恒 0 退出码，Fail-Closed 分支永不执行；论证「`find` 把不存在的 `main/` 当空目录退 0」机理有误（GNU `find` 对显式缺失路径报错并退 1，靠 `-gt 1` 阈值豁免）。修正版：`find` 不进管道、先落 `filelist` 变量取 `rc`，再判 `rc`、保留空集检查，之后 `printf | sort` 定序。三段式的其余（负向扩子树只增不减、薄根 `build_production_router(` 正向锚点、末段两条 git 扫描）已复核成立，保持。
4. **I4（§4.1/§4.2，另同步批 C）**：`BoardTask` 字段提权论证不成立——字段访问点仅 549/554/601–607/739/745，全部位于将同属 `lifecycle.rs` 的实现区；`lifecycle::tests` 经 `BoardTask::new` + `BoardProbe` 交互、零字段直读，且 `mod tests` 是本文件子模块本就可见 private 字段。三字段保持 private，仅结构体名与 `new` 提权。
5. **I5（§4.3/§5）**：删除「`lifecycle` 的 `mod tests` 依赖薄根 `BACKGROUND_GRACE`」从句与 `use crate::BACKGROUND_GRACE;` 接线建议（实测 lifecycle 测试对其零引用，照加即 `unused_imports` 挂 `clippy -D warnings`）。
6. **I6（§3/§6.1/§6.4/§9 风险 6）**：`waiter_handle` 顶格化后的三处口径统一——§6.1 前提改为「首个顶格 `#[cfg(test)]` 须为 test-only 区起始（`mod tests` 或 test-only `impl` 扩展块），其后不得再有生产代码」；§9 风险 6 DoD 改为「五个域文件各恰 1，`startup_gate.rs` 恰 2 且相邻；薄根另恰 1」，与 §3 的扩展块要求不再互相矛盾。
7. **M1（§4.1，含 §9 风险 3）**：`BOARD_SHUTDOWN_GRACE` 的 `lifecycle::tests` 消费计数 14 → 12（9 代码 + 3 注释）；`settle_background_tasks` 的测试消费 3 → 2（2006/2626）；`BoardTask` 薄根消费「539/1128/1156」更正——539 是 `new` 的定义行、1156 是 `run_supervised_lifecycle` 调用点，薄根消费为 1102/1128；§9 风险 3「12 条 `rc2_*`」→ 11（与 §5 清单一致）。复核实测 14（11 代码 + 3 注释，含 2146/2260 两处比较断言；上轮审查的「12（9 代码 + 3 注释）」漏计该两处），维持 14。
8. **M2（§1.1）**：「后台任务接线（工作树新增）」收窄——调度与超时扫尾循环、撤销消费/准入复查/轮询驱动三任务、板端接入监听在基线 `c7ff640` 已存在（实测 `git grep`），工作树新增的仅保留期清理段（1063–1093）。
9. **M3（§1.2）**：补登 CI 合同的顺序断言——`metrics-static-gates` 必须排在 `make fmt-check` 之后（[controller-ci-contract.test.mjs](../../../../scripts/tests/controller-ci-contract.test.mjs) 519–522 行）；§7 批 A 不改该步骤顺序。
10. **M4（§3/§7 批 E）**：薄根行数估算「约 220」→「约 280–300」（`main()` 本体 357 行，批 D 抽走两段循环体约 129 行后仍余约 240 行，另加文档注释/`use`/`mod` 声明/1 测试）。

本轮为文档级定点修订：未改任何源码/测试/`Makefile`、未编译、未运行测试、未执行 `make` 目标、未触库、未读 `secret/`、未提交；正文各处旧口径已全部更新为本轮口径（本记录所述修正本身属修订历史），逐项**已修订，待复核**，不称 PASS。

## 12. 实施登记

批A实施登记（2026-10-28，提交108929f；审查PASS——按用户裁决方案A收窄实施：规格撰写时工作树的main.rs保留期清理段（+65/−4，G-PC2四token载体）与production_composition.rs的mod retention_cleanup_wiring G-PC2测试均已被回退（现工作树与HEAD一致且无该段，历次全量测试持续全绿佐证同步回退非意外丢失），G-PC2接线按裁决取消，G-M6/G-V4/G-PC1三处接线照做，漂移与收窄据此登记；①Makefile metrics-static-gates第6条改三段式（find显式双路径枚举main.rs+src/main/子树、独立取rc、空集与rc>1 Fail-Closed、sort定序、负向注入缝pattern扫全收集面、薄根build_production_router(正向锚点防静默放行），其余各条、末段git扫描、.PHONY与CI步骤顺序零改动；②tests/v4_baseline.rs新增read_main_sources()（与08号read_db_sources先例逐行同构）、tests/production_composition.rs新增main_production_sources()（按首个顶格#[cfg(test)]截取生产段），三处消费者作用域等价扩展至目录收集，全部断言逐字不变，接线先于任何实现搬移，当前收集结果={src/main.rs}，断言行为与旧版逐字节等价（顶格#[cfg(test)]全文恰1处在1128测试区起点、正向锚点928行生产区内、负向token全文计数0——严格论证成立；收集文本含每文件分隔换行，与08号先例同构，措辞精度登记）；③G-PC2取消已在提交正文登记，无retention残留；④他人WIP（Makefile desktop注释4行、v4_baseline索引测试28行等）全部git apply --cached -R精确剔除在提交外且工作树完好保留，提交恰3文件+89/−5；⑤CI合同复核实测node --test 75/75；⑥make metrics-static-gates实测通过（正向锚点main.rs:928、顶格截断点1128、负向token全文0次，main/缺失时find退1被-gt 1豁免、空集与薄根缺失由锚点grep退2兜底）；⑦验证七条全绿（fmt-check/metrics-static-gates/clippy -D warnings/cargo test全量主bin 29+lib 954/v4_baseline 10含WIP 1已区分/production_composition 14/node 75/cargo doc -D broken_intra_doc_links）；规格行号因保留期清理段回退整体前移（1,189→1,128、938→928），§7批A出口条件全部满足；2项信息级minor：提交正文「逐字节同旧行为」措辞精度（实为断言行为逐字节等价、收集文本含分隔换行）与Makefile recipe续行缩进（tab+tab+空格vs规格展示单tab+空格，语义完全一致））

批B实施登记（2026-10-28，提交110add3；审查PASS）：两叶子域逐字纯移动——①main/secret_log.rs（68行）：迁入 LogSecretSink（pub(super)，含 pub(super) PathBuf 元组字段——薄根 bootstrap_admin sink 实参经 LogSecretSink(secret_path) 元组构造，按 §4.2 private_fields 判据提权、§4.2 清单未列名特此登记）与 write_private_secret（private，create_new+0600 逐字）+2 条测试逐字随迁；②main/recovery.rs（117行）：迁入 CONTROLLED_RECOVERY_ENV/controlled_recovery_requested/controlled_recovery_flag/new_recovery_controller（均 private）与 build_recovery_controller（pub(super)），49 行原段精确 diff 唯一差异即该 pub(super) 前缀+2 条测试逐字随迁；③锚点重采：规格行号按现工作树 main.rs（2,572 行，保留期清理段已回退）以符号名重采——LogSecretSink 原65→薄根消费点现822、build_recovery_controller 调用点原883→现809、根 mod tests 原1128→现1054；④薄根 2,417 行：use 区移除 BootstrapSecretSink（库 trait，唯一消费者 impl 已随迁；真实 sink 类型名为 LogSecretSink，无符号名漂移，实施报告「移除 BootstrapSecretSink」系指 use 清理非符号改名），新增私有 mod 声明两项与 self:: 私有 use 两项，两消费点文本零改写；⑤测试 29 守恒：4 迁出+薄根 25（#[test] 3+#[tokio::test] 13+start_paused 9），主 bin 单 target 实测 29 passed，-- secret_log::tests:: 2、-- recovery::tests:: 2、-- rc2_ 11 定向全绿；⑥已知文本适配（minor 登记）：LogSecretSink emit 签名 rustfmt 断行合并与 ControllerError/PathBuf 路径缩写（use 下沉直接伴随，语义零变化）、mod 声明带 #[path] 属性（mod 定位调整，总览 C2 明文允许；提权按字面 token 计 3 处/按顶层项口径恰 2 项，登记口径为后者）；⑦门禁：metrics-static-gates 实测通过，收集器目录枚举自动纳入两新文件（find 实证恰 3 文件），两新文件被禁 token 零命中；⑧验证全绿（fmt-check/metrics-static-gates/clippy -D warnings/全量 lib 954+主 bin 29 等零失败/定向三条/rustdoc -D broken_intra_doc_links）；观察项登记：recovery.rs doc 注释两处 intra-doc 裸名（TransportManager::current_mode/RecoveryModeProbe）在子模块作用域无 use 承接，当前 doc 门禁未将其纳入渲染检查面（按规格 §8 口径为绿），未来若启用私有项渲染检查有 broken 风险，后续批次留意）

批C实施登记（2026-10-28，提交7056c8a；审查PASS）：①锚点重采：按父提交 264acc3 main.rs 符号名锚定——BOARD_SHUTDOWN_GRACE 75-77、shutdown_signal 393-417、BoxedError/boxed/BoardTask/BoardOutcome/observe_board/sleep_or_pending/watch_turns_true/finish_board 428-598、supervise_shutdown 600-703、run_supervised_lifecycle 705-720、settle_background_tasks 722-747、根顶格 #[cfg(test)] 1054；②提权恰 §4.2 清单 11 顶层项 pub(super)（字面 12 处含 BoardTask::new），薄根实际消费 6 项（use 6 项与消费点 614/644/667/669/672/677 一一对应，BoxedError 薄根零显式消费不引入，最小集合）；observe_board/sleep_or_pending/watchable/observe 与 BoardTask 三字段保持 private，零 pub/pub(crate)/零 allow(dead_code)/零再导出（登记口径：BOARD_SHUTDOWN_GRACE/BoxedError/BoardOutcome/finish_board/supervise_shutdown 5 项按 §4.2 清单与 §7 批 C 行字面提权，薄根与生产消费均在本文件内，pub(super) 等值无越界）；③17 用例+11 fixture 随迁逐字（测试区切片 879 行 diff 零差异，start_paused 9 例随函数，BOARD_SHUTDOWN_GRACE 实参逐字，I5 核销零 use crate::BACKGROUND_GRACE）；④主 bin 29 守恒（lifecycle 17+薄根 8+secret_log 2+recovery 2，定向 rc2_11/n1_4/n2_2 与抽样 2+2 实测）；⑤已知文本适配（minor）：实现区 diff 恰 2 处允许差异——BOARD_SHUTDOWN_GRACE 与 shutdown_signal 间 1 空行（子文件重排布局）与 BoardTask::new pub(super) 断行；薄根 use 区删 Poll/AbortHandle 随迁清理与 listener 组断行重排；⑥C4 内聚性论证（补录）：lifecycle.rs 1,262 行落 900–1,500 区间——监督状态机 supervise_shutdown 的 select! 分支与 BoardProbe/poll_once/poll_until_ready 手动 poll 竞态用例必须同文件内聚，拆分将切断 §2-5 停机语义与 rc2_* 唯一防线的同位性，故不再拆分；⑦规模与估算偏差说明：实测 1,262（实现区 375+测试区 887）对 §3 估算 620（测试 470）偏差源于规格估算数字低于其自身锚点段长（实现锚点已 354 行、fixture 区间已 642 行且用例交错延伸近 880 行），估算未随保留期清理段回退的基线重采更新，非实施扩张；⑧验证八条全绿（fmt-check/metrics-static-gates/clippy -D warnings/全量 lib 954+主 bin 29 零失败/定向三条+抽样两条/rustdoc -D broken_intra_doc_links）

批D实施登记（2026-10-28，提交2ca986a；审查PASS）：①按父提交重采锚点，main.rs 1182→1102，BACKGROUND_GRACE 原78→background20，调度装配原525起→background35，spawn 原561→background69，薄根调用521/收敛597–601，根顶格测试边界699→619；②spawn体除包装/缩进和interval一处rustfmt合行外逐字相同，装配仅四依赖/关停watch传递适配（db_for_scheduler→deps.pool；transport_manager.as_device_rpc()→deps.transport.as_device_rpc()（根传同一Arc clone，as_device_rpc内部亦Arc::clone）；process_epoch→deps.process_epoch；shutdown_rx.clone()→函数参数shutdown；Arc::clone(&recovery_for_scheduler)→deps.recovery 仍同一RuntimeRecoveryController），常量（1000ms interval、wrapping_add、is_multiple_of(5)）/SQL（SELECT COUNT(*) FROM sub_tasks WHERE state IN ('dispatching','accepted','verifying')）/三条eprintln文案与{n}/{e}/continue/恢复门禁真源/退出条件（watch changed→borrow为真break）及spawn相对次序全部不变，无额外await或任务spawn；③SchedulerDeps类型名及恰4字段（pool/transport/process_epoch/recovery）pub(super)为父根字面量构造必需——离线rustc探针实测「类型private+字段pub(super)」时父根 background::SchedulerDeps 字面量报E0603，「类型pub(super)或pub(crate)」均rc0；父模块为bin crate root，pub(super)有效范围等同pub(crate)、不新增crate外面，按§4.2 bin root等值论证无越界；background全文7处pub(super)=3顶层项（BACKGROUND_GRACE/spawn_scheduler_loop/SchedulerDeps）+4字段，零裸pub/pub(crate)/allow(dead_code)；④唯一新增形状用例 background::tests::scheduler_deps_field_set_is_exact（完整4字段字面量+逐字段带类型取值，无..Default兜底；加字段则字面量缺字段失败、删字段则字面量/读取失败，足以证明字段集合），29条既有用例逐字守恒，主bin单target 30（lifecycle17+根8（启动门禁7+交付面1仍待批E）+secret_log 2+recovery 2+background 1），定向 scheduler_deps_field_set_is_exact=1/rc2_=11/secret_log=2 均绿；⑤按用户裁决方案A取消全部retention项（clean_retention_data/RETENTION_CLEANUP_INTERVAL/retention_task/retention_recovery/RetentionDeps/spawn_retention_cleanup 六token在提交完整patch中全0）及G-PC2迁移，不引入已回退WIP；⑥background.rs 162行（生产118+测试44）低于900阈值，background四禁token全0，唯一顶格#[cfg(test)]在119紧接mod tests:120其后纯测试无生产代码（满足§6.1截断前提）；⑦门禁收集面实测：以 make metrics-static-gates SHELL=/bin/bash .SHELLFLAGS=-xc 追踪现有recipe，G-M6 find 的 filelist 恰5文件（根+background/recovery/lifecycle/secret_log），新background自动纳入、无需改收集器；⑧薄根回改：私有#[path] mod声明与批B/C形态一致、零pub use再导出，经 self::background 私有use消费BACKGROUND_GRACE（background:20唯一2秒定义），main:597–601仍原收敛清单[revoke,recheck,poll,sched]与原宽限参数，main:516–534 spawn相对次序仍撤销→准入→轮询→调度→信号→板端监听→HTTP→监督→收敛，根生产段对SchedulerPolicy/SystemMonoClock/TaskScheduler/SqlxSchedulerStore/Duration零消费（测试区Duration另有显式 std::time::{Duration,SystemTime} 导入 main:625，非残留依赖）；⑨六组验证全rc0：make fmt-check / make metrics-static-gates（另trace实证）/clippy -p rsetup-controller --all-targets --locked -- -D warnings / cargo test -p rsetup-controller --locked（lib954、全量1542 passed/0 failed/41 ignored未跑真库、v4_baseline 10、production_composition 14）/主bin 30+定向三条/RUSTDOCFLAGS -D rustdoc::broken_intra_doc_links（rc0，27条既有非broken链接警告 private_intra_doc_links/redundant_explicit_links 均在本提交未触达文件，如实说明非零告警）



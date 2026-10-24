# 中控 V1 当前已知缺口（Known Gaps）

日期：2026-10-24（按当前工作区与已有去敏报告清理过时描述；首版 2026-10-22）

## 0. 范围声明

- 本文按当前工作区（包括未提交改动）描述实现状态；历史执行结果只引用对应日期的报告，本文修订不代表重跑测试或新的验收通过。
- 各节可以说明已接线部分以限定缺口范围，但不得把局部实现或已有实测切片外推为完整 V1 发布批准。
- 本文**不改变** [`controller-v1-acceptance.md`](<controller-v1-acceptance.md>) 中任何账本行的 `status`/`executed_at`/`layer`/`command_or_steps`/`evidence`，也不新增或删除账本行；账本行仍是唯一的状态登记处。
- 缺口项一律不得被引用为 PASS 依据；补齐后须在验收账本按行合同另记执行时间、层级、命令与可持久复核的证据路径。
- 本文中标注「**正在落地**」的条目描述的是本轮**进行中**的工作，只说明「准备补什么」，**不构成**该缺口已闭合或任何验收行通过的证据。
- 本文不含凭据、真实主机地址、连接串或备份引用具体值；涉及真实库/真实设备的操作一律引用去敏报告。

本文链接均为仓库内位置；涉及代码的条目一律以**符号名**定位，不引用会随提交漂移的行号。

## a) 传输线与设备业务接线现状（状态轮询、任务调度、主动结果接收已接通，恢复模式仍缺）

- **代码位置**：[`crates/rsetup-controller/src/main.rs`](<../../crates/rsetup-controller/src/main.rs>)
  的 [`TransportManager`](<../../crates/rsetup-controller/src/transport/manager.rs>)（实现 `DeviceLiveProbe` 与 `ManagementNotificationSink`）、
  [`TransportBoardClient`](<../../crates/rsetup-controller/src/transport/board_client.rs>)（实现 `tasks::board_client::BoardClient`）、
  [`SqlxSchedulerStore`](<../../crates/rsetup-controller/src/tasks/scheduler_store.rs>)、
  [`SqlxTaskEvidenceApplier`](<../../crates/rsetup-controller/src/tasks/evidence.rs>) 与
  [`SqlxTaskResultSink`](<../../crates/rsetup-controller/src/transport/listener.rs>)。
- **已接通部分**：
  1. 生产入口已装配真实连接管理器与板端监听（`run_controller_listener_with_sink`），启动了轮询驱动任务（`spawn_poll_driver`），
     `GET /devices/{id}/status` 能够读取由轮询写入的真实设备状态快照；
  2. 准入吊销已可向活跃会话投递切断与停止重连信令；
  3. `TaskScheduler` 生产调度驱动已在 `main.rs` 启动后台循环，`SqlxSchedulerStore` 完成真实候选联查与短事务 CAS 意图落库，并对排队超时（`expired`）与 prepare 失败（`failed`）进行持久化收敛与设备锁释放；
  4. 中控结果接入 `SqlxTaskResultSink` 接入生产 listener 并注入 `TaskEvidenceApplier`，完成合法状态机推进与设备锁处理；板端在 `execute` 持久落盘后通过会话通道异步触发 `device.task.result` 事件；
  5. 关停广播已联合覆盖 HTTP、板端监听、准入复查、撤销消费、状态轮询与任务调度。
- **仍存在的缺口**：
  1. 恢复模式真源仍为 `UnwiredRecoveryProbe`（见 [`main.rs`](<../../crates/rsetup-controller/src/main.rs>)），管理员通过会话门禁后，`GET /api/v1/system/status` 在读取该探针时返回 503 `NOT_READY`；
  2. 真实数据库环境下的端到端长时间任务压测与容灾验证尚未完成。
- **影响**：任务派发、执行通知与状态持久化闭环已建立，但受控恢复模式（ControlledRecovery）仍未交付。
- **下一步**：交付 Runtime 恢复控制器替换占位探针，不以常量成功替代。

## b) 真实主机重启仍未实现；板端生产连接模式已接线

- **代码位置**：[`dispatch.rs`](<../../crates/rsetup-board-agent/src/dispatch.rs>) 的 `HostRebootOs::reboot`、
  [`host_source.rs`](<../../crates/rsetup-board-agent/src/host_source.rs>) 的能力申报，以及
  [`板端入口`](<../../crates/rsetup-board-agent/src/bin/rsetup-board-agent.rs>)、
  [`生产连接循环`](<../../crates/rsetup-board-agent/src/connect.rs>)。
- **现状**：生产 OS 边界仍恒返回 `ErrorKind::Unsupported`，生产装配不注入 reboot dispatcher，
  能力清单不申报可执行重启，prepare/execute 返回 `CapabilityUnavailable`。真实重启未实现、未验收。
- **已接线部分**：`--connect` 已走显式中控公钥 pin、TCP/v3 认证、加密记录层和 h2 双流，
  复用真实 journal；`--once`、`--watch` 与逐行 JSON 控制台仍是独立的本地/测试模式，不能替代生产隧道。
  [`systemd 单元`](<../../data/rsetup-board-agent.service>) 当前以 `User=root`/`Group=root` 运行 `--connect`，
  使用 0700 状态目录与必需的环境文件，不再默认执行只读 watch 循环；装包和升级仍不自动启停该服务。
  已有[真实双进程报告](<tunnel-two-process-real-verification-report.md>)记录只读状态与吊销停止重连链路，
  不应再称「认证隧道尚未实现」。
- **剩余边界**：真实 OS reboot、真实板卡上的 journal/掉电/跨 boot 行为，以及真实 systemd 部署取证仍缺；
  双进程只读回环结果不替代上述证据。
- **下一步**：在另获明确授权的受控板端环境交付真实 reboot 边界及负例/恢复验证；
  [生产隧道计划](<../superpowers/plans/2026-10-23-production-v3-tunnel.md>)明确不把真实重启扩为该任务的隐含授权，不能为清理文档打开执行能力。

## c) SSE 业务发布方已部分接线，端到端与时间线仍有边界

- **代码位置**：[`crates/rsetup-controller/src/events.rs`](<../../crates/rsetup-controller/src/events.rs>)
  的 `EventBus::publish`；生产装配处 `EventBus::new()` 见
  [`crates/rsetup-controller/src/main.rs`](<../../crates/rsetup-controller/src/main.rs>)，
  分发至 `EventsState`、`HttpTasksState`、`ManagementState`；测试见
  [`crates/rsetup-controller/tests/sse_business_events.rs`](<../../crates/rsetup-controller/tests/sse_business_events.rs>)。
- **现状**：
  1. 任务线（`tasks/http.rs`）在任务提交、有效取消、时限复核、释放锁成功时，已接入 `task_updated` 发布；
  2. 管理线在设备改名（`devices.rs` `patch_device`）与准入状态变更（`approvals.rs` `approve`/`revoke`/`reauthorize`）成功时，已接入 `device_updated`（`EventObjectAudience::admins()`）发布；
  3. 授权变更在目标用户明确（`users.rs` 触及 active/is_admin、密码重置，以及 `grants.rs` `create_grant`）成功时，已接入 `permissions_changed` 发布；
  4. 以上写操作与受众隔离在 [`tests/sse_business_events.rs`](<../../crates/rsetup-controller/tests/sse_business_events.rs>) 中通过 11 项进程内端到端测试。
- **仍存在的边界与未闭环项**：
  - **系统时间变更的 SSE 发布方没有生产调用方**：[`SystemApiState::publish_system_time_changed`](<../../crates/rsetup-controller/src/api/system.rs>) 已提供类型化入口，事件线句柄在生产装配中已接入（`events = Some(bus)`），但时间门禁的后台 driver（`start_time_gate` 启动采样、周期复验、单调退避、跳钟换代）在证据变化处不调用它，因此事件流上当前不会出现 `system_time_changed`；同一条时间证据的 HTTP 读线缺口见 j 节；
  - **`authz_epoch` 变更与 SSE 读线复核关流的时序特性**：写事务内 `bump_authz_epoch` 后，已建立的旧连接在发送下一帧前权威复核发现 `epoch` 不等，按 06 §4「发送前验证 session 与 authz_epoch，撤权关闭旧订阅，排队旧权限数据不得发送」立即主动断开连接；受影响用户是通过断线重连拉取最新授权，而不是持续在旧连接上消费事件；
  - **受影响集合不确定的写操作不虚构受众**：角色修改、组修改、删除授权等无法单点回显具体目标用户的场景，为防止向错人广播虚假提示，遵循安全失败原则不发送 `permissions_changed`，由全局 `authz_epoch` 关流重连机制保证收敛；
  - **真实浏览器与跨网络传输端到端仍属未闭环**（AT-18 保持未验证）。
- **下一步**：在时间门定时更新处接入 `system_time_changed` 发布；在真实网络与前端联合环境中按 AT-18 进行端到端复核。

## d) 设备状态快照生产写入已接线，完整容量与前端验收仍缺

- **代码位置**：[`main.rs`](<../../crates/rsetup-controller/src/main.rs>) 的 `build_transport_manager` 与 `production_inputs`
  共享同一个 `Arc<SnapshotStore>`；[`TransportManager`](<../../crates/rsetup-controller/src/transport/manager.rs>)
  的 `spawn_poll_driver`、`poll_device` 与 `ingest_status_sample` 承担采样和写入；
  [`状态端点`](<../../crates/rsetup-controller/src/http_device_status.rs>) 只读取授权后的缓存。
- **已接线部分**：`PollScheduler` 已在生产后台循环中驱动，当前会话首个可信样本由管理器确认代际后写入缓存；
  采集错误、断连与晚到样本继续使用 [`SnapshotStore`](<../../crates/rsetup-controller/src/polling/snapshot.rs>) 的代际/序号规则。
  不再存在「只有 HTTP 持有空缓存、没有生产写入方」的装配缺口。
- **可观察结果**：无样本时仍返回 `snapshot = null`、`freshness = unknown`、`age_ms = null`，
  但这不是所有设备的恒定结果；[2026-10-24 双进程报告](<tunnel-two-process-real-verification-report.md>)已记录真实 `fresh` 样本与指标。
  `device.status.read` 对象级权限与设备可见性门禁不因此放宽。
- **剩余缺口**：1024 会话/慢设备/预算饱和下的完整生产容量证据，以及真实浏览器消费状态变化的端到端验收仍未闭合（见 f 节）；
  板端 clock 周期查询、四时戳偏差估算的生产消费与监控 UI 展示仍不完整，不能从 status 采样通过推定已交付。
- **下一步**：保留已有接线与安全门禁，补齐时间采集和前端展示并按验收合同补证；不重复开发已接通的快照写入方，也不改写 T4-06 的历史账本行。

## e) v4-only 数据库已有实测切片，目标版本与完整事务验收未闭合

- **当前基线**：按 [v4-only 计划](<../superpowers/plans/2026-10-23-v4-only-database-baseline.md>)，
  空库由显式入口直接建立完整 v4，已有 v4 做只读闭合核验，旧版/未知版/部分结构拒绝；
  v1–v3 升级、续迁及旧 fixture 已有意移除，不再列为待修复功能或要求补回旧升级 CI。
- **已有证据**：[2026-10-24 真实引擎记录](<v4-real-engine-verification.md>)记录 MySQL 8.0 保留现存 v4、
  TiDB 8.5 备份核验后经生产 `--apply` 直接建 v4，以及两引擎结构/数据闭合和限定准入 CAS 用例复验。
  后续[真实双进程报告](<tunnel-two-process-real-verification-report.md>)记录 TiDB 上的准入 API 与状态链路，
  不能再笼统声称生产入口或真实双进程完全未执行。
- **剩余缺口**：MySQL 实测版本为 8.0，不替代 8.4 LTS；限定准入 CAS 不能替代完整任务设备锁、
  撤权/提交并发与故障矩阵。真实备份做过完整性/一致性核验但未做恢复演练。
  [`acceptance_runtime` 测试](<../../crates/rsetup-controller/tests/acceptance_runtime.rs>)中的旧双进程 smoke 仍是显式 ignored 占位，
  与后续独立脚本实测是两条证据，不把占位当成功，也不因它未完成否认已有报告。
- **历史口径**：[2026-10-22 报告](<real-db-2026-10-22/report.md>)中的 legacy 阻塞与计数保留为当时事实，
  不再作为当前 v4-only 待办；T4-12 历史账本行仍仅覆盖当时的结构迁移，不能外推全部事务门禁通过。
- **下一步**：在另获授权且满足备份/隔离/权限门禁的目标上补齐当前 v4 业务矩阵、目标版本与恢复演练；
  逐项登记新证据，不改写历史 PASS 或为凑通过恢复旧迁移路线。

## f) 浏览器、容量与 a11y 验收未闭合

- **代码位置 / 证据**：验收账本 [`controller-v1-acceptance.md`](<controller-v1-acceptance.md>)；
  无头浏览器脚本 [`scripts/headless_e2e_verify.py`](<../../scripts/headless_e2e_verify.py>)。
- **现状**：AT-01..AT-18 全部为「未验证」（含完整 SSE 全量、06 §7 容量/发布压测基准、真实宿主 reboot
  与真实电源故障、旧备份受控恢复演练 AT-15）。T4-10 的 PASS 实际只覆盖登录页外壳：2026-10-22
  以 1280×800 视口跑 44/44，以 Firefox 请求 320×800 时被浏览器夹到 500×800，43/44（唯一失败为
  “视口宽度未被夹取”断言），两视口截图经多模态查看布局/中文/无裁切均正常；**真实 320px 视口、
  完整任务流程、键盘/焦点/Esc、对比度/减少动效与中英切换仍未验证**。截图与证据 JSON 写在 `/tmp`，
  **不在仓库内，不构成可长期复核的证据**；复核必须重跑脚本。真实 320px 需要 Chromium 等可设置
  小于 500px 视口的浏览器，Firefox headless 当前环境无法做到。
- **影响**：Web 层的可访问性与容量结论一律缺失；页面在真实设备数据与真实事件流下的布局与行为未知。
- **下一步**：把证据（截图、trace、DOM 断言输出）写入仓库内受版本控制的目录并按行登记；按 AT 矩阵
  逐项执行并在账本按行更新，不批量推定。

## g) 交付与架构口径

- **代码位置**：[`debian/control`](<../../debian/control>) 的 `Package: rsetup-controller` 与
  `Package: rsetup-board-agent` 段及三个包的
  `Architecture`；[`debian/rules`](<../../debian/rules>)（`override_dh_auto_build`/`override_dh_auto_install`
  逐条静态构建 controller 三个 bin（`rsetup-controller`、`rsetup-controller-migrate`、
  `rsetup-admin`，后两者的 cargo bin 名与 `/usr/bin` 交付名一致）与板端 bin
  （`rsetup-board-agent`），
  `override_dh_auto_test` 已恢复完整 workspace，`override_dh_installsystemd` 以
  `--no-enable --no-start --no-stop-on-upgrade` 安装 controller 与 board-agent 两个单元，
  并**显式补一条** `dh_installsystemd -prsetup-next --no-start --no-stop-on-upgrade`（覆盖
  整个 dh 步骤不会为被省略的包回落默认序列；thermal/fan-curve/LED 因此保留默认 enable，
  但不再由包自动 start，也不会因升级被停掉后无人重启）；
  [`data/rsetup-controller.service`](<../../data/rsetup-controller.service>)；
  [`debian/rsetup-controller.default`](<../../debian/rsetup-controller.default>)；
  [`debian/rsetup-controller.postinst`](<../../debian/rsetup-controller.postinst>)（`configure`
  分支用 `adduser --system` 幂等建静态账户，不启动服务）；
  [`debian/rsetup-controller.README.Debian`](<../../debian/rsetup-controller.README.Debian>)；
  [`data/rsetup-board-agent.service`](<../../data/rsetup-board-agent.service>) 与
  [`debian/rsetup-board-agent.README.Debian`](<../../debian/rsetup-board-agent.README.Debian>)；
  迁移入口 [`crates/rsetup-controller/src/db.rs`](<../../crates/rsetup-controller/src/db.rs>) 的
  `migration_config_from_env`/`run_production_migration` 与
  [`crates/rsetup-controller/src/bin/migrate-controller.rs`](<../../crates/rsetup-controller/src/bin/migrate-controller.rs>)
  （cargo bin 名 `rsetup-controller-migrate`）；
  [`debian/rsetup-next.tmpfiles`](<../../debian/rsetup-next.tmpfiles>)（包内安装的 tmpfiles.d 配置）；
  CI 架构矩阵见 [`.github/workflows/ci.yml`](<../../.github/workflows/ci.yml>)。
- **现状**：controller **已进入包内**。前端产物 `apps/controller-web/dist` **不再入库**（整个
  目录被 `.gitignore` 忽略，仓库也不再保存固定哈希的构建产物）：`debian/control` 的
  Build-Depends 同时带 `nodejs` 与 `npm`，`debian/rules` 的 `override_dh_auto_build` 在第一条
  cargo 之前执行 `npm --prefix apps/controller-web ci` 与 `npm --prefix apps/controller-web run
  build` 现生成 `build.rs` 编译期读取的资产，产物文件名/hash 全由 vite 决定、无处写死；Rust
  侧仍走 `CARGO_NET_OFFLINE` + 包内 `debian/cargo-home` 离线。`rsetup-controller`
  包安装 `/usr/bin/rsetup-controller`、`/usr/bin/rsetup-controller-migrate`、
  `/usr/bin/rsetup-admin`、`rsetup-controller.service`、0644 无秘密的**配置示例**
  `/usr/share/doc/rsetup-controller/rsetup-controller.default` 与 `README.Debian`——
  **包不把任何文件写进 `/etc`**，`/etc/default/rsetup-controller` 必须由操作员自己从该示例
  创建（`README.Debian` 用 `install -m0600 -o root -g root`，不是先 `cp` 成 0644 再
  `chmod`）；单元改为静态系统账户
  `User=rsetup-controller`/`Group=rsetup-controller` + `StateDirectory=rsetup-controller`
  + `StateDirectoryMode=0700` + `UMask=0077`，
  账户与同名私有组由 postinst 以 `adduser --system --group --no-create-home --disabled-login`
  幂等创建（包 `Depends: adduser`），**不再使用 `DynamicUser=yes`**：动态用户的临时 uid 每次
  启动都不同、状态目录随之重新 chown，`/var/lib/rsetup-controller` 的属主随重启漂移；静态账户
  让属主跨重启与升级稳定（0700，`rsetup-controller:rsetup-controller`），备份/rsync/ACL 与事后
  取证才有锚点。单元以 `--no-enable --no-start` + `ConditionPathExists`
  三重保证装包不启动（`--no-enable` 单独用**不**阻止启动，postinst 仍会留
  `deb-systemd-invoke` 的启动/重启激活片段（升级形态是 `_dh_action=restart`）；
  `--no-stop-on-upgrade` 再保证升级不 stop/restart），
  postinst 除 debhelper 的单元状态登记外只建账户、不启动服务（早期构建记录中的产物里
  controller `postinst` 只有 `adduser --system` 与 `#DEBHELPER#` 合并结果，无任何 systemctl
  启动调用）。`rsetup-board-agent` 包安装
  `/usr/bin/rsetup-board-agent`、`rsetup-board-agent.service`（`User=root`/`Group=root`、
  `StateDirectory=rsetup-board-agent` 0700、`--connect`，参数来自运维创建的必需环境文件）与
  `README.Debian`，同样 `--no-enable --no-start --no-stop-on-upgrade`，依赖仅 `systemd`。`rsetup-next` 包内容不变（CLI/TUI 主二进制、helper、Polkit 策略、
  thermal/fan-curve/LED 单元与 tmpfiles.d 配置）。架构口径仍只维护 64 位 `arm64`（aarch64）与
  `amd64`（x86_64），不维护 32 位目标。
- **影响**：装完包**得不到可用的中控服务**，只得到未启动的交付物：用
  `install -m0600 -o root -g root` 从 `/usr/share/doc/rsetup-controller/rsetup-controller.default`
  创建 `/etc/default/rsetup-controller` 并填入数据库连接串（随包不含 `/etc` 下的模板）、
  对真实库做可恢复备份、设置四个 `CONTROLLER_MIGRATION_*` 门禁、
  `rsetup-controller-migrate --check` 后 `--apply`、`systemctl start`、读取并删除
  `bootstrap-secret.log` 后改密——这些一律是**操作员动作**，仓库不代跑，也没有任何一条被伪造为
  已完成。早期交付轮次曾用 `DEB_BUILD_OPTIONS=nocheck dpkg-buildpackage -d -b -uc -us`（`-d` 跳过构建依赖
  校验、`nocheck` 跳过 `override_dh_auto_test` 的完整 workspace 测试）实际成功构建
  `rsetup-next_0.5.0-1_amd64.deb`、`rsetup-controller_0.5.0-1_amd64.deb` 与新增的
  `rsetup-board-agent_0.5.0-1_amd64.deb`，并用 `dpkg-deb --contents`/`dpkg -e` 解包核对包内路径
  （`/usr/bin/rsetup-admin` 已取代旧交付名、`rsetup-board-agent.service` 在
  `/usr/lib/systemd/system/`、controller 包内 `postinst` 即 `adduser --system` 脚本且无
  `systemctl` 调用）、`Depends`（controller 含 `adduser`，board-agent 仅 `systemd`）与
  `readelf -d` 静态性（`rsetup-admin`、`rsetup-board-agent` 均零 `NEEDED`）。上述是早期构建记录，
  不替代当前构建工具依赖与实际包安装门禁；[后续打包审查报告](<tunnel-packaging-fix-report.md>)
  给出了当前模板审计口径及各轮未验边界，不能把跳过构建依赖或测试的命令当作完整打包验收。
  **系统级 `dpkg -i` 与真实 systemd 部署仍缺完整证据**，但不再声称生产初始化或首次登录完全未执行：
  [v4 真实引擎记录](<v4-real-engine-verification.md>)记载生产 `--apply` 及首次双进程的 bootstrap/强制改密，
  [后续双进程报告](<tunnel-two-process-real-verification-report.md>)记载使用已有管理员登录并完成只读接入链路；
  两者都不是包内 systemd 安装流程的证据。CI 合同已同步为「每条编译 controller 的作业都先构建前端；Debian
   作业经 `debian/rules` 里的 npm 步骤构建；dist 不入库，也不得再被提交比对」，并钉住
   「构建配置与文档不得写死生成资产文件名」；历史测试计数不代表本文清理时重新执行。`debian-package` 作业在产物检查后
   另加一步只读断言（`Assert Debian systemd maintainer scripts`）：用 `dpkg-deb --control`
   把 `DEBIAN/postinst` 解到临时目录，断言 controller 与 board-agent 的 postinst 既无
   `deb-systemd-invoke … start` 也无 `… restart`（即 `--no-start` 在构建机的 debhelper
   上确实生效），并断言 `data/rsetup-controller.service` 带 `StateDirectoryMode=0700`；
   断言只读，不改动包内容。上述 `.deb` 证据出自 dist 仍入库
   的那一轮；后续交付轮次改为打包期现构建前端后，曾用同一条
   `DEB_BUILD_OPTIONS=nocheck dpkg-buildpackage -d -b -uc -us` 重新构建成功——日志里
   `override_dh_auto_build` 先 `npm --prefix apps/controller-web ci` 与 `run build` 生成
   vite 产物，再进入 cargo 静态编译三个包，本次生成的 js/css 资产名嵌入
   `rsetup-controller`，全程不引用任何提交产物。
- **下一步**：在受控环境核对当前产物的真实安装、systemd 启停与首次启动流程，并登记可持久证据。
  当前[交付说明](<../../debian/rsetup-controller.README.Debian>)规定 remove/purge 后保留静态账户、私有组与状态目录，
  真实卸载残留仍需受控核验；不把早期动态 UID 的推测性升级方案当作现存部署问题，也不扩展旧账户/配置迁移功能。
  `--apply` 仅允许空库直接初始化 v4 或核验已有 v4，不能向操作员建议自动升级旧 schema。架构口径不变。

## h) CSRF 绑定容量清理的边界

- **代码位置**：[`crates/rsetup-controller/src/http_auth.rs`](<../../crates/rsetup-controller/src/http_auth.rs>)
  的 `HttpAuthState::csrf`（`session digest -> CsrfBinding` 绑定表）与 `MAX_CSRF_ENTRIES`、
  `CSRF_PRUNE_THRESHOLD`、`prune_csrf_table`、`prune_expired_csrf`、`prune_dead_csrf`；满表时的响应由
  [`crates/rsetup-controller/src/http_gate.rs`](<../../crates/rsetup-controller/src/http_gate.rs>)
  的 `exhausted` 产出（`RESOURCE_EXHAUSTED` + `Retry-After: 60`）。
- **现状**：登录窥探、权威 `login_success` 插入与 `me` 绑定三处，都会在已持有的 CSRF 锁内先按单调时钟
  清掉超过 12h 绝对寿命的绑定（`prune_expired_csrf`）；只有当表长达到 `CSRF_PRUNE_THRESHOLD`
  （硬上限的 3/4）时才追加一次死绑定扫描（`prune_dead_csrf`），该扫描的唯一真相源是本进程内存会话时钟
  `AuthServiceApi::is_live_session`，即**只回收进程时钟已经判定为死的绑定**（idle 过期、绝对过期、被本
  进程 logout 从时钟移除、或属于已换代的授权 epoch）。
- **边界**：在数据库侧已撤销（revoke-others、管理员重置口令、停用账号）但其内存时钟条目**尚未到期**的
  会话，其 CSRF 绑定**仍会占用 `MAX_CSRF_ENTRIES` 额度**，直到该会话到达 idle/绝对超时，或该会话下一次
  请求在预认证门或权威 `authenticate` 处拿到固定 401、由该分支删除自己的绑定。因此**不得**把
  `prune_dead_csrf` 表述为「已清理所有被撤销会话的绑定」：它不是数据库权威的清理。
- **影响**：撤销风暴只会在上述窗口内抬高绑定表占用率，窗口结束后自行回落；若 4096 个槽位全部由 live
  绑定占据，扫描之后仍按设计对新的绑定请求返回 503 `RESOURCE_EXHAUSTED`——既不会驱逐 live 绑定，也不会
  覆盖既有绑定，这是容量不足的 fail-closed，不是清理遗漏。
- **下一步**：若需要把清理收紧为数据库权威口径，先交付可低成本同步判定「该 digest 已撤销」的失效来源
  （可靠失效推送或内存撤销集合），再按行合同补负例测试与可复核证据；在此之前不得扩大对扫描语义的描述。

## i) 业务 Protobuf codec：已按所有者授权实现/启用，外部评审证据未闭环

- **代码位置**：[`crates/rsetup-protocol/src/wire.rs`](<../../crates/rsetup-protocol/src/wire.rs>) 的 codec 边界
  （sealed trait `Codec`、不可伪造的 `ApprovedCodecWitness` 与其授权入口 `approved_codec_witness()`、
  fail-closed 的 `NoCodec`、前置守卫 `encode_business`/`decode_business`）与
  [`crates/rsetup-protocol/src/codec.rs`](<../../crates/rsetup-protocol/src/codec.rs>) 的 `BusinessCodec`
  （按 03 §2 字段号**手写**的 proto3 canonical 编解码，不引入 Protobuf 运行时依赖，不使用 `unsafe`）；
  crate 级边界声明见 [`crates/rsetup-protocol/README.md`](<../../crates/rsetup-protocol/README.md>)。
  **符号与实际行为一律以这两个文件的代码为准。**
- **授权事实**：2026-10-22 项目所有者明确选择「直接实现并启用生产 codec」，接受跳过尚未完成的
  外部专家书面决议、获批 codec 库选型、固定跨实现向量与真实两进程负向互操作等门禁。授权主体、原话、
  范围与**仍缺证据**逐条登记在
  [`docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md`](<../superpowers/reviews/2026-10-22-codec-owner-authorization.md>)。
  该授权是**所有者授权，不是外部专家决议**，因此本文不因它把任何缺口写成已闭合。
- **已有传输证据与剩余缺口**：codec 已在生产监听与板端 `--connect` 接线，
  [2026-10-24 双进程报告](<tunnel-two-process-real-verification-report.md>)记录批准后的真实加密双流状态采样与吊销停连；
  不能再把它描述为「只有 codec、没有真实收发路径」。但该切片仍不替代：
  - **完整真实两进程负向互操作矩阵**（[`协议 §8`](<../protocol_spec.md>)）；
  - **固定跨实现向量与跨 Protobuf 库解析器一致性矩阵**；
  - **codec 路线选型审定与 coverage-guided fuzz**。已有[解析器压力报告](<tunnel-parser-stress-report.md>)明确是确定性变异测试，
    不等于握手、记录层、业务 codec 的覆盖引导 fuzz。
- **影响（不得据此推定的范围）**：已有生产只读 wire 证据与剩余安全门禁分开记录；
  本文**不改变验收账本任何行的 `status`**：`T4-09` 仍为 `BLOCKED`，`AT-09`/`AT-11`/`AT-12` 仍为「未验证」。
  不因局部双流实测推定完整互操作、安全审查、真实 reboot 或 V1 发布全部通过。
  板端本地 JSON 控制台仍不是生产 wire；所有者授权也不是外部专家审定，解析差异等风险仍见授权记录第三节。
- **下一步**：按授权记录第四节的补证清单**逐项单独**补齐并登记证据——固定向量（含边界、坏 varint、
  未知字段、未消费尾随字节）→ 真实两进程负向互操作 → 跨 Protobuf 库解析器一致性矩阵 → 解析器 fuzz →
  codec 路线选型审定 → 外部专家复审；每条各自按行合同另记 `executed_at`/`layer`/`command_or_steps`/`evidence`，
  一条完成不推定其它条完成。

## j) `GET /api/v1/system/time`：路由与证据返回已提交，时间门驱动调用与账本证据仍缺

- **规格依据**：[`docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md` §7「系统与事件入口」](<../superpowers/specs/2026-09-23-controller-v1-02-data-api.md>)
  明确规划 `GET /system/time`：请求「无」，响应为 **`TimeEvidence` + NTP 状态/下次重试**，权限「管理员」，
  且「响应不含凭据」。同节相邻的 `GET /system/status` 早已注册，`/system/time` 在此前所有生产路由表里
  都不存在。
- **代码位置**：[`crates/rsetup-controller/src/api/system.rs`](<../../crates/rsetup-controller/src/api/system.rs>)
  的 `system_router`（现为 `GET /api/v1/system/status` 与 `GET /api/v1/system/time` 两条路由的唯一注册
  锚点）与其 `get_system_time` handler、`fallback_time_evidence`、`SystemApiState::time_source` 字段；
  生产 `merge` 装配位见 [`crates/rsetup-controller/src/api/mod.rs`](<../../crates/rsetup-controller/src/api/mod.rs>)；
  证据类型见 [`crates/rsetup-controller/src/time/evidence.rs`](<../../crates/rsetup-controller/src/time/evidence.rs>)
  的 `TimeEvidence`/`TimeQuality`；门禁观测面见
  [`crates/rsetup-controller/src/time/gate.rs`](<../../crates/rsetup-controller/src/time/gate.rs>)
  的 `TimeGateHandle::current()`/`background_retry_active()`/`retry_attempt()` 与 `next_backoff`。
- **当前实现（工作区口径）**：
  1. `system_router` 注册 `GET /api/v1/system/time`，handler 调用顺序固定为
     `authenticate_read(EndpointId::SystemStatusRead)`（与 `/system/status` **共用同一读桶限流**）→
     `require_admin_session`（非管理员 403、未登录 401）→ 时间源分流；
  2. 生产装配 `api/mod.rs` 与 `main.rs` 中已提交 `time_source` 注入并跑通测试；
  3. 响应 `data` 恰为 `{ evidence, retry_active }`：`evidence` 是既有 `TimeEvidence` 的同一形状（不新增
     第二套时间字段），`retry_active` 取自门禁 `background_retry_active()`；
  4. `time_source` 为 `None` 时回退 `fallback_time_evidence()`：以当前系统墙钟给出
     `quality = system_fallback`、`clock_epoch` 为 nil、无 `source`/`sample_age_ms`/`offset_ms`/
     `uncertainty_ms` 的**诚实降级**证据，`retry_active: false`，不伪造 `NtpValid`；响应不含凭据；
  5. 模块内已有 5 项进程内 HTTP 测试覆盖并通过测试：门禁证据与重试态、后台重试激活、无时间源回退、非管理员 403、
     未登录 401。
- **剩余缺口**：
  1. **时间门 driver 未调 `publish_system_time_changed`**：`SystemApiState::publish_system_time_changed` 已就绪，但时间门禁 driver 在证据变化时未调用它（见 c 节）；
  2. **下次重试字段未审定**：响应只有 `retry_active`（后台退避重试是否在跑），未给出下次重试的具体时刻或退避时长（`retry_attempt()` 与 `next_backoff` 未进入响应），与 §7「NTP状态/下次重试」的字面要求仍有差距；是否在响应中补一个由 `next_backoff(attempt)` 派生的可机读字段尚未审定；
  3. **无账本证据**：上述行为目前只存在于模块内进程内测试，未记入验收账本，也不构成真实环境的端到端结论。
- **影响（不得据此推定的范围）**：本节只记录一条规划端点推进到「路由已提交并通过测试」，
  **不改变验收账本任何行的 `status`**：`T4-09` 仍为 `BLOCKED`；`AT-01`..`AT-18` 仍为「未验证」。在取得按行登记的账本证据前，「系统时间可读」「NTP 状态可观测」「Runtime Task 6 完整」一律不得引用为已
  实现或通过。
- **下一步**：在时间门 driver 中补齐 `publish_system_time_changed` 调用 → 审定「下次重试」的响应字段口径 → 按行合同另记 `executed_at`/`layer`/`command_or_steps`/`evidence`，
  至少覆盖：未登录 401、非管理员 403、响应形状与 `TimeEvidence` 一致且不含凭据、`SystemFallback`/`Stale`
  两种质量下的真实降级值、以及与门禁 `background_retry_active()`/`retry_attempt()` 的一致性。

## 相关文档

- [`docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md`](<../superpowers/reviews/2026-10-22-codec-owner-authorization.md>) — 业务 codec 的所有者授权事实、范围与缺失证据
- [`docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md`](<../superpowers/specs/2026-09-23-controller-v1-02-data-api.md>) — 02 §7 系统与事件入口（含 `GET /system/time` 的响应与权限口径）
- [`crates/rsetup-protocol/README.md`](<../../crates/rsetup-protocol/README.md>) — 协议 crate 的 codec 边界与使用方式
- [`apps/controller-web/README.md`](<../../apps/controller-web/README.md>) — 前端当前状态与安全边界
- [`apps/controller-web/DEV-PROXY.md`](<../../apps/controller-web/DEV-PROXY.md>) — 开发代理规则与验证边界
- [`docs/testing/real-db-2026-10-22/report.md`](<real-db-2026-10-22/report.md>) — 双引擎真库执行去敏报告

# 中控 V1 当前已知缺口（Known Gaps）

日期：2026-10-22

## 0. 范围声明

- 本文**只记录当前代码与验收证据中客观存在的缺口**：每一条都是「尚未做到什么」，不是「已通过什么」。
- 本文**不改变** [`controller-v1-acceptance.md`](<controller-v1-acceptance.md>) 中任何账本行的 `status`/`executed_at`/`layer`/`command_or_steps`/`evidence`，也不新增或删除账本行；账本行仍是唯一的状态登记处。
- 缺口项一律不得被引用为 PASS 依据；补齐后须在验收账本按行合同另记执行时间、层级、命令与可持久复核的证据路径。
- 本文不含凭据、真实主机地址、连接串或备份引用具体值；涉及真实库/真实设备的操作一律引用去敏报告。

本文链接均为仓库内位置；涉及代码的条目一律以**符号名**定位，不引用会随提交漂移的行号。

## a) 传输线与设备业务未接线（设备实时视图 / 恢复模式 / 通知 / 板端六方法）

- **代码位置**：[`crates/rsetup-controller/src/main.rs`](<../../crates/rsetup-controller/src/main.rs>)
  的 `UnwiredTransportSources`（实现 `DeviceLiveProbe`、`RecoveryModeProbe`、`ManagementNotificationSink`）
  与 `UnwiredBoardClient`（实现 `tasks::board_client::BoardClient`）。
- **现状**：生产入口的唯一 transport 装配位对三个探针与板端六个方法（`capabilities`/`status`/`clock`/
  `prepare`/`execute`/`task_record`）一律返回 fail-closed 错误（探针上抛 `Config` 映射 503，板端回
  `DeviceOffline`），绝不返回伪造的 `Offline`、空能力、`ready = true`、`Ok(None)` 或常量成功。
  启动期恢复编排 `recover_process` 因此把板端 `task_record` 记为「未核验」，`queued` 子任务按不可信
  期限保守置 `held`。
- **影响**：设备实时视图、恢复模式读取、准入变更后的 commit 通知、任务下发到板端（能力/时钟/票据/
  执行）在真实环境不可用；Web 上对应视图只能看到诚实的错误或空数据。任何「设备数据端到端已通」的
  说法均不成立。
- **下一步**：交付 transport 侧真实适配器（连接管理器 + typed 设备缓存 + 恢复控制器 + 通知通道）后，
  仅替换这两个装配位的实现，七线装配拓扑（auth/management/tasks/status/events/metrics/web）不变；
  替换后按 T4-01/T4-06/T4-07/T4-09 的行合同另记证据。

## b) 真实主机重启未实现；board-agent 已有 amd64 本地测试 bin

- **代码位置**：[`crates/rsetup-board-agent/src/dispatch.rs`](<../../crates/rsetup-board-agent/src/dispatch.rs>)
  的 `HostRebootOs::reboot`；本地测试入口
  [`crates/rsetup-board-agent/src/bin/rsetup-board-agent.rs`](<../../crates/rsetup-board-agent/src/bin/rsetup-board-agent.rs>)；
  说明见 [`crates/rsetup-board-agent/README.md`](<../../crates/rsetup-board-agent/README.md>)。
- **现状**：生产 OS 边界 `HostRebootOs::reboot()` 仍恒返回 `ErrorKind::Unsupported`，真实 host reboot
  未实现、未验收。本轮新增了可在 amd64 本机编译运行的板端测试进程：`--once`/`--watch` 走真实
  `BoardAgent::handle_typed_action` 的三条只读动作（capabilities/status/clock，读 `/proc` 与 boot_id），
  `--loopback-test-server` 提供**仅测试用**的逐行 JSON 控制台；reboot prepare/execute 恒
  `CapabilityUnavailable`，绝不在本地触发重启。业务 codec 已按所有者授权启用（见 i 节），但该
  JSON 控制台仍不是生产加密隧道，不能替代真实 wire；真实双流绑定的 codec 接入见 a/i 节现状。
- **影响**：真实重启路径（含 03 §5 B-04 的两阶段持久化在真实 OS 调用后的行为）与生产板端进程形态
  （认证隧道、守护、journal 真实环境）仍未实现；本地测试只覆盖只读 DTO、白名单、journal 查询与错误映射。
- **下一步**：在受控板端环境实现真实 reboot 边界与其负例/回退路径，再把本地测试 bin 接入生产隧道与
  部署单元；在真实设备上取得可持久证据前，不得宣称重启能力可用。

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
  - **系统时间变更发布方未在业务消费侧接线**：[`SystemApiState::publish_system_time_changed`](<../../crates/rsetup-controller/src/api/system.rs>) 已提供类型化入口，但当前时间采样与时间门未在生产循环中持久调用；
  - **`authz_epoch` 变更与 SSE 读线复核关流的时序特性**：写事务内 `bump_authz_epoch` 后，已建立的旧连接在发送下一帧前权威复核发现 `epoch` 不等，按 06 §4「发送前验证 session 与 authz_epoch，撤权关闭旧订阅，排队旧权限数据不得发送」立即主动断开连接；受影响用户是通过断线重连拉取最新授权，而不是持续在旧连接上消费事件；
  - **受影响集合不确定的写操作不虚构受众**：角色修改、组修改、删除授权等无法单点回显具体目标用户的场景，为防止向错人广播虚假提示，遵循安全失败原则不发送 `permissions_changed`，由全局 `authz_epoch` 关流重连机制保证收敛；
  - **真实浏览器与跨网络传输端到端仍属未闭环**（AT-18 保持未验证）。
- **下一步**：在时间门定时更新处接入 `system_time_changed` 发布；在真实网络与前端联合环境中按 AT-18 进行端到端复核。

## d) 设备状态快照没有写入方

- **本节范围**：`device.status.read` 对象级权限与设备可见性门禁**已由本轮修复补齐**，不属于本节缺口；
  本节**只**记录「快照数据源没有写入方」这一项，其余设备状态相关的缺口见 a) 节。
- **代码位置**：[`crates/rsetup-controller/src/main.rs`](<../../crates/rsetup-controller/src/main.rs>)
  装配 `status: DeviceStatusState { snapshot: Arc::new(SnapshotStore::new()) }`；缓存实现见
  [`crates/rsetup-controller/src/polling/snapshot.rs`](<../../crates/rsetup-controller/src/polling/snapshot.rs>)；
  端点实现见 [`crates/rsetup-controller/src/http_device_status.rs`](<../../crates/rsetup-controller/src/http_device_status.rs>)。
- **现状**：`SnapshotStore` 具备代际/序号校验与单调新鲜度判定，但当前生产装配里**没有任何写入方**
  （transport 轮询与认证连接管理器尚未接入）。因此 `GET /devices/{id}/status` 对任何设备都真实地返回
  「尚无样本」：`snapshot = null`、`freshness = unknown`、`age_ms = null`。端点的 `device.status.read`
  对象级权限与设备可见性门禁已由本轮修复补齐（不可见设备 404 且不触碰缓存，无权限 403 且不读缓存），
  该补齐只是使权限语义可验证，不改变上面「尚无样本」这一缺口。
- **影响**：设备状态读路径的权限与语义可验证，但「有真实样本、单调不回收、旧会话不覆盖」这一行为在
  真实环境不可观察；不得据此宣称 T4-06 的状态缓存链路整体可用。
- **下一步**：接入轮询驱动共享同一个 `Arc<SnapshotStore>`；每条设备必须先由认证连接管理器
  `confirm_generation`，未确认代际的 `update` 一律 `NoActiveGeneration` 拒绝，代际不由样本自行建立。

## e) 真实数据库验收未闭合

- **代码位置 / 证据**：[`docs/testing/real-db-2026-10-22/report.md`](<real-db-2026-10-22/report.md>)
  （去敏报告，不含主机、账号、口令、连接串或备份引用具体值）。
- **现状**：双引擎串行真库执行合计 72 项，66 项通过，**2 项 TiDB legacy 升级用例仍挂起未闭环**
  （`partial_v1_column_is_resumed_only_by_explicit_command`、`tidb_legacy_v2_to_v3_positive_single_case`；
  已加 3s/120 次/500ms 退避的有界 preflight 重试，把无限挂起变成有界失败，仍未通过），
  **4 项（两引擎各 2 项）因数据库账号权限前置未满足未执行**（缺 MySQL `SELECT ON performance_schema.*`
  与 TiDB `PROCESS` 只读观测权限）。`acceptance_runtime::two_process_real_db_smoke` 仍为
  `unimplemented!` 的显式 `#[ignore]`，未执行。
- **影响**：真实库迁移与并发/设备锁/撤权事务门禁未整体闭合；验收账本 T4-12 行的 PASS 只覆盖那一次
  `migration_v4` 结构迁移执行（且引擎版本口径为 MySQL 8.0 / TiDB v7.5，与本 V1 目标版本 8.4 LTS /
  8.5 LTS 不同），不推定本项整体通过。
- **下一步**：由 DBA 授予只读观测权限后重跑 4 项锁观测用例；定位并修复 TiDB 侧经 sqlx 连接池连续 DDL
  后的元数据查询阻塞；重跑时按行合同另记 `executed_at`/`layer`/`command_or_steps`/`evidence` 与实际
  `SELECT VERSION()`。

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

- **代码位置**：[`debian/control`](<../../debian/control>) 的 `Package: rsetup-controller` 段与两个包的
  `Architecture`；[`debian/rules`](<../../debian/rules>)（`override_dh_auto_build`/`override_dh_auto_install`
  逐条静态构建 controller 三个 bin，`override_dh_auto_test` 已恢复完整 workspace，
  `override_dh_installsystemd` 以 `--no-enable` 安装 controller 单元）；
  [`data/rsetup-controller.service`](<../../data/rsetup-controller.service>)；
  [`debian/rsetup-controller.default`](<../../debian/rsetup-controller.default>)；
  [`debian/rsetup-controller.README.Debian`](<../../debian/rsetup-controller.README.Debian>)；
  迁移入口 [`crates/rsetup-controller/src/db.rs`](<../../crates/rsetup-controller/src/db.rs>) 的
  `migration_config_from_env`/`run_production_migration` 与
  [`crates/rsetup-controller/src/bin/migrate-controller.rs`](<../../crates/rsetup-controller/src/bin/migrate-controller.rs>)；
  [`debian/rsetup-next.tmpfiles`](<../../debian/rsetup-next.tmpfiles>)（包内安装的 tmpfiles.d 配置）；
  CI 架构矩阵见 [`.github/workflows/ci.yml`](<../../.github/workflows/ci.yml>)。
- **现状**：controller **已进入包内**，且 Debian 源码包全程离线构建、不需要 Node/npm——
  `apps/controller-web/dist` 已入库，`build.rs` 编译期读取的资产由源码包自带。`rsetup-controller`
  包安装 `/usr/bin/rsetup-controller`、`/usr/bin/rsetup-controller-migrate`、
  `/usr/bin/rsetup-controller-reset-admin`、`rsetup-controller.service`、0644 无秘密的
  `/etc/default/rsetup-controller` 模板与 `README.Debian`；单元用 `DynamicUser=yes` +
  `StateDirectory=rsetup-controller`，不建静态用户，并以 `--no-enable` + `ConditionPathExists`
  双重保证装包不启动。`rsetup-next` 包内容不变（CLI/TUI 主二进制、helper、Polkit 策略、
  thermal/fan-curve/LED 单元与 tmpfiles.d 配置）。架构口径仍只维护 64 位 `arm64`（aarch64）与
  `amd64`（x86_64），不维护 32 位目标。
- **影响**：装完包**得不到可用的中控服务**，只得到未启动的交付物：配置 `/etc/default/rsetup-controller`
  （改成 0600 并填入数据库连接串）、对真实库做可恢复备份、设置四个 `CONTROLLER_MIGRATION_*` 门禁、
  `rsetup-controller-migrate --check` 后 `--apply`、`systemctl start`、读取并删除
  `bootstrap-secret.log` 后改密——这些一律是**操作员动作**，仓库不代跑，也没有任何一条被伪造为
  已完成。本轮已用 `dpkg-buildpackage -d --build=binary --no-sign` 实际成功构建
  `rsetup-next_0.5.0-1_amd64.deb` 与 `rsetup-controller_0.5.0-1_amd64.deb`，并用
  `dpkg-deb --contents`/解包冒烟核对包内路径与三个二进制；**系统级 `dpkg -i` 安装、systemd
  服务启动、真实库上的 `--apply` 与首次启动取凭据仍未执行**（真库迁移证据见
  [`docs/testing/real-db-2026-10-22/report.md`](<real-db-2026-10-22/report.md>)，其中不含本轮的
  生产入口执行记录）。CI 合同测试已同步为“Debian 构建 controller、无需前端构建”，当前 12/12 通过。
- **下一步**：在受控环境跑一次真实安装链路（`dpkg -i`、`--check`/`--apply` 与首次启动取凭据）并按行
  合同登记可持久证据；补齐升级路径与卸载残留（`/var/lib/rsetup-controller` 由动态 uid 持有）的复核。
  不改现有架构口径。

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
- **现状**：业务 codec 已从「只有 `NoCodec` 恒 `CodecUnapproved`」变为「按所有者授权启用的可用实现」，
  解码侧对未知字段号、重复 singular/optional 字段、错误 wire type、截断与长度越界、非最短 varint、
  非法 UTF-8、超 512 KiB 消息、`metrics` 超 128 项、缺 `schema_version` 一律拒绝；DTO 自校验与见证守卫
  不变，`NoCodec` 与 `witness: None` 两条 fail-closed 路径语义不变。但仍未闭环的是：
  - **传输线本身未接线**（见 a 节）：codec 可用不等于中控与板端之间已存在真实加密隧道，业务消息
    目前在真实环境上没有可观察的收发路径；
  - **真实两进程负向互操作未执行**（[`docs/protocol_spec.md` §8 第 4 条](<../protocol_spec.md>)）；
  - **固定跨实现向量与跨 Protobuf 库解析器一致性矩阵缺失**（同 §8 第 1 条）；
  - **codec 路线选型未审定**（继续手写 vs 改用经过 fuzz 的成熟 Protobuf 库）；本 crate 解析器与
    握手/记录层解析器的 fuzz 均未执行（同 §8 第 2 条）。
- **影响（不得据此推定的范围）**：本项只是把「无 codec、原始字节入口一律 fail-closed」替换为
  「所有者授权启用的 codec」，**不改变验收账本任何行的 `status`**：`T4-09` 仍为 `BLOCKED`；
  `AT-09`/`AT-11`/`AT-12` 仍为「未验证」；HTTP/2（h2）双流互通与真实 wire 端到端一律不得据此宣称通过。
  板端本地 JSON 测试控制台（b 节）也不是生产 wire，codec 启用不改变那一条。所有者授权启用但未经外部
  评审的 codec 带来的双端解析差异、坏包接受与不可复现字节风险见上述授权记录第三节。
- **下一步**：按授权记录第四节的补证清单**逐项单独**补齐并登记证据——固定向量（含边界、坏 varint、
  未知字段、未消费尾随字节）→ 真实两进程负向互操作 → 跨 Protobuf 库解析器一致性矩阵 → 解析器 fuzz →
  codec 路线选型审定 → 外部专家复审；每条各自按行合同另记 `executed_at`/`layer`/`command_or_steps`/`evidence`，
  一条完成不推定其它条完成。

## 相关文档

- [`docs/superpowers/reviews/2026-10-22-codec-owner-authorization.md`](<../superpowers/reviews/2026-10-22-codec-owner-authorization.md>) — 业务 codec 的所有者授权事实、范围与缺失证据
- [`crates/rsetup-protocol/README.md`](<../../crates/rsetup-protocol/README.md>) — 协议 crate 的 codec 边界与使用方式
- [`apps/controller-web/README.md`](<../../apps/controller-web/README.md>) — 前端当前状态与安全边界
- [`apps/controller-web/DEV-PROXY.md`](<../../apps/controller-web/DEV-PROXY.md>) — 开发代理规则与验证边界
- [`docs/testing/real-db-2026-10-22/report.md`](<real-db-2026-10-22/report.md>) — 双引擎真库执行去敏报告

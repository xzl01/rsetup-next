# 中控 V1 条件性执行计划总目录（TDD）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 分专题逐项落实；使用 `- [ ]` 跟踪步骤。四份专题计划列出文件、接口、RED → GREEN → REFACTOR、命令和独立提交。

**Goal:** 将既有 controller-design 转化为可审阅、待批准后按 TDD 执行的独立 Rust 中控 V1 实施路径。

**Architecture:** 中控 `crates/rsetup-controller`、共用协议 `crates/rsetup-protocol`、板端代理 `crates/rsetup-board-agent`、前端 `apps/controller-web`；独立于现有单机 `rsetup-app` 的无认证 HTTP 控制台。单实例模块化 Rust 中控：管理 HTTP、独立加密隧道、MySQL/TiDB 持久化身份/任务/审计，连接与最新快照驻内存。

**Tech Stack:** Rust 2024 / MSRV 1.85；Tokio、Axum、tracing 已存在；Vite + Vue 3；MySQL、TiDB 已确认，但版本/SQLx、tonic/prost、rust-embed 和前端配套仅为待审阅建议。

**Spec:** `docs/superpowers/specs/2026-09-22-controller-design.md`、`docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` 至同组 01–06、`docs/protocol_spec.md`、`PRODUCT.zh.md`。执行时必须同时阅读计划和相应规格。

## Global Constraints

- 用户同意**以待审阅 draft-1 为条件性计划依据**，不表示同意任何新增接口/参数或批准开工。执行前审阅并明确批准 00 §5、01–06，尤其身份会话、任务票据/unknown 锁、默认参数、API、SSE 和 DB 版本；若被否决，先改规格和计划，再开工。时序数据库、关机、升级、文件分发、任意 shell、多租户均不纳入首版。
- 协议发布阻断：服务端签名未覆盖 `reason_code`、验签失败描述冲突、AEAD 失败终止策略未定。安全审查、协议修订和**双端测试**未完成前，不宣称安全完工或生产可发布；不得自行修改密码字段布局。管理 HTTP 只能部署在受控可信网络，板端加密不保护浏览器。
- TDD 铁律：每个新增行为先写测试并看到**正确原因**的失败，再最少实现并看到绿，重构后重跑，最后小提交。依赖/数据库未安装、编译错误或坏 fixture 不算有效 RED。现有 `make test` / `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` 不得倒退；本次没有运行功能测试。
- 执行阶段先用 `using-git-worktrees` 创建隔离工作树；不把既有无认证板端 HTTP 暴露为远程管理。每任务独立评审，可拒绝一项而保留相邻项。危险操作不得以 mock 被调用次数代替持久副作用/故障恢复证据。
- 1024 在线与每台状态 10s 为设计验收目标，**不是已证明的容量**；独立 NTP 30s 启动预算、健康待审批连接无总期限、跨重启不盲发重启与仅保存最新监控快照必须维持。
- **文档静态检查与产品功能测试明确分层：** 本轮计划修订仅针对测试与验收追踪文档的审查缺陷进行规范校正，严禁将“文档修改完成”表述为“产品功能已修复”或“测试已通过”。当前所有功能实现、集成接线与测试用例仍处于未实现/待验证状态；全 G0-G5 检查点保持未完成，历史专题计划（01-04）不整体重写。
- **依赖管理与供应链审查：** 未来生产实现若有新增 workspace 成员或直接 crate/npm 依赖引用，必须附带完整的 manifest 清单并在离线环境下进行 lockfile 严格审查（锁定 MSRV 1.85，遵循许可证与离线构建安全，禁止未审网络拉取）。

## 专题顺序及依赖

| 阶段 | 计划 | 独立交付 | 上游依赖 |
| --- | --- | --- | --- |
| 01 | [`2026-10-02-controller-01-identity-data-tdd.md`](2026-10-02-controller-01-identity-data-tdd.md) | 中控 crate、双数据库存储、初始化/身份/动态授权、设备/准入/审计管理 API | 规格批准 |
| 02 | [`2026-10-02-controller-02-transport-board-tdd.md`](2026-10-02-controller-02-transport-board-tdd.md) | 双端协议、加密隧道、双流探测、板端查询/持久日志/重启边界、板端可执行入口与中控独立设备监听接线 | 安全协议审阅；纯类型和测试可与 01 并行，准入及进程联调依赖 01；完整只读RPC smoke与最终启动门禁依赖 03 联验 |
| 03 | [`2026-10-02-controller-03-tasks-runtime-tdd.md`](2026-10-02-controller-03-tasks-runtime-tdd.md) | NTP/时间、轮询背压、快照、主子任务、DB 锁、故障核实及旧备份恢复 | 01 数据/ACL 契约、02 板端业务客户端 |
| 04 | [`2026-10-02-controller-04-web-tdd.md`](2026-10-02-controller-04-web-tdd.md) | Vue 管理页、双语无障碍、SSE、Rust 嵌入 | 01/03 的 HTTP 契约；组件测试可先使用可控 HTTP |

**跨专题契约与生产实现边界：**
- **标识与编码：** `DeviceId=Ed25519 pubkey lowercase hex64`；UUIDv4 文本，64 位 JSON 计数十进制字符串。
- **状态与事务安全：** 01 的 `authz_epoch` 在 03 创建与下发前重验；仅 `device.reboot.execute` 必须在 03 的 dispatching 发送意图事务**提交后**发送。只读查询和 `device.reboot.prepare` 可先于该意图，prepare 取得票据后才提交意图；所有 DB 事务内均禁止网络 I/O。01 的准入存储向 02 提供完整 `admission_state/review_decision/revision`；准入变更提交后以 `{device_id,revision}` 通知 02 连接管理，02 重读最新持久决定并校验连接代际，完成拒绝、吊销、kick/revoke 与旧连接清理，通知失败不得回滚已提交决定或继续放行应关闭的连接。04 不复刻后端权限判定。专题接口矛盾必须先改文档和测试，再进入集成；不得通过重复实现绕开。
- **数据库启动与版本闭合：** 普通启动对数据库 schema 维持只读，禁止启动期隐式自动迁移；不仅比对版本数字，v4 启动必须进行结构与数据闭合检查，且公共写 guard 必须与身份数据扫描及 auth/admission 消费者同步升级回归，绝不把原来只认 v3 的 guard 留在 v4 写路径中。
- **真实 DB 双进程 smoke 隔离：** 门禁纯测试（离线装配与内存门禁阻断）与完整真实 DB smoke 严格分轨。完整真实 DB smoke 必须标 `#[ignore]` 并通过显式单 case `--ignored --exact` 串行执行；严禁使用原命令普通 cargo 跑网络 DB，严禁注入“只读 DB fake”冒充初始化、登录写入或准入写事务，生产入口不增加 fake 仓储开关。
- **Observer 锁观察证据边界：** 旧 writer 连接自观察绝不是独立的 Observer 证据，MySQL 8.0 绝不能替代 8.4 LTS（TiDB 必须对应 8.5 LTS）；无独立 Observer 账号与最小权限（MySQL performance_schema 锁表 SELECT / TiDB PROCESS）、无真实接线维持 BLOCKED，不恢复已暂停工具或额外扩权。
- **危险操作幂等性（Idempotency）：** 用户同一次确认操作生成的 UUID 必须保留，重试绝不更换 key；幂等 key 必须通过 HTTP 请求头 `Idempotency-Key: <UUID>` 发送，严禁放入 JSON body；普通远程 HTTP 非安全上下文禁用 `crypto.randomUUID` 时，前端必须实现并测试基于 `crypto.getRandomValues` 的安全随机方案；缺能力禁操作：若运行环境缺少密码学安全随机能力，严禁使用弱随机 fallback，必须直接禁用危险操作。
- **SSE 统一事件命名与客户端时序：** 统一五种命名事件：`event: device.updated`、`event: task.updated`、`event: permissions.changed`、`event: system.time.changed`、`event: reset`（JSON 数据在 `data:` 行，分别注册监听）；页面加载/重连时必须在 SSE 连接实际 open 成功建立后再发起全量 GET；维护连接与同步代际，丢弃旧代际响应与乱序事件；revision 仅在同一对象 kind+id 内部比对水位，Page 无全局 revision；断线或轮询期间页面严禁标“实时”；收到 reset、旧 GET 响应、事件乱序或撤权时立即清理旧 DOM 投影字段并触发重认证。
- **真实静态资源与脱离目录硬约束：** release 构建缺少前端 dist 产物时必须直接失败，严禁打包空目录；`web_assets.rs` 必须实际读取并解析构建生成的 `index.html`，提取真实哈希 JS/CSS 文件并验证内容、精确 MIME 与长期 immutable 缓存；运行时必须脱离源码与静态目录（在独立临时目录启动），严禁以空 HTML 或 assets 恒 404 伪装测试通过。
- **浏览器 E2E mock fixture 前置：** Playwright mock 若无受控事件源或会话列表接口，必须明确完整合法的前置 fixture（包括 `/api/v1/auth/me`、`/api/v1/auth/sessions` 及受控 SSE），严禁因认证畸形或 session 缺少导致页面初始化重定向或崩溃，避免伪 RED。

## 阶段检查点（依次验收）

- [ ] **G0 规格/风险关卡：** 按 00 §5 和设计 §14 确认所有草案新增参数与协议安全处理；未批准时只审计划。
- [ ] **G1 身份/数据：** 01 的 RED/GREEN/回归记录与代码评审；自助改密必须验证 current_password，错误旧密码不改变密码/账号状态或撤销有效会话。MySQL 8.4 LTS、TiDB 8.5 LTS 是 draft 建议版本，需审批后分别测迁移、初始化唯一性、并发管理员保护、授权撤销与 CAS。CI 对已配置 URL 的引擎显式执行 ignored DB 用例并检查实际执行数非零；缺 URL 标未验证，不能把普通 workspace 测试通过当双库验收。schema 普通启动只读，v4 启动必须进行结构与数据闭合检查，且公共写 guard 必须与身份数据扫描及 auth/admission 消费者同步升级，旧 guard 不得残留在 v4 写路径中。Observer 锁观察旧 writer 自观察不是 O 证据、MySQL 8.0 不替代 8.4，无接线/最小权限账号维持 BLOCKED，不恢复已暂停工具或额外扩权。
- [ ] **G2 隧道/板端：** 02 的字节向量、负例、互通、待审批资源界限、人工拒绝与 pending 区分、双流探测及重开失败升级（正确 nonce/trace 但非零 status_code 或非空 error_message/metadata 的 pong 也不得判健康）、吊销通知/强切连接、先落盘再重启、跨 boot 持久结果与 OS 错误信封。验收板端可执行入口、中控独立设备端口、长期身份/信任配置与两进程端口/握手/准入/probe分阶段证据，不以库内互通代替运行接线；完整只读RPC网络smoke须依赖03生产BoardClient/轮询/恢复接线，与DB/NTP/恢复门禁在G5联验，G2不提前记完整smoke通过。协议问题未消除不得进入生产发布。
- [ ] **G3 时间/任务：** 03 的 NTP/DNS 故障、1024 错峰/背压（调度暂停或过载跳轮后仍保留设备相位，验证下一周期分布）、网络/OS/DB 崩溃窗口、unknown 持锁、过期与旧备份受控恢复；任何可能已执行的重启不得自动重发。
- [ ] **G4 Web/交付：** 04 的授权投影、中英、键盘焦点/对比度/320px；危险操作同一次确认保留 UUID 且重试不换 key，通过 Idempotency-Key 头传递（非 body）；普通远程 HTTP 下测试 getRandomValues 安全随机方案（无弱随机 fallback，缺能力直接禁用操作）；SSE 统一命名为 `device.updated/task.updated/permissions.changed/system.time.changed/reset`，实际 open 后全量 GET，连接/同步代际，对象 kind+id 水位（Page 无全局 revision），断线不标实时，reset/旧 GET/乱序/撤权清旧 DOM 及重认证真实浏览器端到端验证；Playwright mock 明确 fixture 前置避免认证畸形伪 RED；资源嵌入后缺 dist release 直接失败，脱离目录运行，解析实际 index.html 中哈希 JS/CSS 验证内容/精确 MIME/immutable 缓存，严禁以空 HTML 或 assets 恒 404 伪装通过，API 不回退 HTML。
- [ ] **G5 端到端/发布：** `make test`、格式/Clippy、新前端测试/build；两数据库分别恢复演练；门禁纯测试（离线装配阻断）与完整真实 DB 双进程 smoke 严格分轨，真实 DB smoke 必须标 `#[ignore]` 且仅通过显式单 case `--ignored --exact` 串行执行，普通 cargo 测试严禁连接网络 DB，严禁注入只读 DB fake 冒充初始化、登录写入或准入写事务；门禁未完成不得接纳设备业务或下发变更，smoke 不调用开发宿主的真实 reboot。记录 1024 已批准/1024 首次待审、20 管理会话、RTT≤100ms、稳定 1h 的**待审阅基准**，CPU/RAM/报文大小/分位延迟/拒绝计数。逐项核对 06 AT-01..18；安全阻断未解除时只能报告测试，不得称可上线。

## 设计需求 → 验收追踪

| 设计需求（C-* 定义见规格 00 §3） | 计划/专题测试 | 06 验收 |
| --- | --- | --- |
| C-ARCH/C-DATA/C-AUTH/C-ACL | 01：DB-01、AUTH-01/02、ACL-01、API-01、ADMIN-01；01+03：ACL-02（授权 epoch/组变更与任务提交、执行前重验）；03 Task 2：API-02（幂等提交/预览）；ACL-03 见 01 授权侧 + 03 Task 5 任务/SSE 侧 | AT-01..03、AT-14 |
| C-ADMIT/C-LIVE/C-SCALE | 01+02：ADM-01、WIRE-01/02、协议§8；03：RUN-01 | AT-04、AT-10..12 |
| C-TASK/C-CONCUR/C-RECOVER | 02：WIRE-03..05；03：TASK-01..07、OPS-01 | AT-05..07、AT-12/13/15 |
| C-POLL/C-TIME | 03：TIME-01..03、RUN-01 | AT-08..10 |
| C-I18N/C-WEB/C-HTTP | 04：U-01..04 | AT-16..18 |

**交接：** 这是计划文档，不代表 draft-1 获批准、代码已实现、性能已验证或安全阻断已解决。规格批准后，选子代理驱动（逐任务新代理、双阶段审查）或 executing-plans（分批检查点）；每阶段只在正确原因 RED、GREEN、集成回归和相应审核完成后推进。

## 2026-10-07 当前实现基线与增量计划入口

以下只标记当前代码和现有证据的边界，不更改 01–04 条件性计划复选框、不替代其原始规格/审查记录，也不批准任何未决接口、依赖、真实 DB 迁移或生产发布。代码变化时重新核对基线。

| 专题 | 已有切片与尚缺证据 | 剩余计划 |
| --- | --- | --- |
| 身份与准入 | identity v3、准入 CAS 与认证/会话 handler 已有代码；生产 `main.rs` 仍仅提供探活，动态授权/管理 API/root 恢复 CLI 缺失，auth/session SQLx 事务缺真实双库验收。 | [身份/数据剩余步骤](2026-10-07-controller-v1-remaining-identity-tdd.md) |
| 设备协议与板端 | `rsetup-protocol` 已有 frame/sig/Ed25519 纯函数，不等于完整握手。板端 journal、双流及生产接线尚缺；独立向量和外部专家决议仍受门禁。 | [设备传输剩余步骤](2026-10-07-controller-v1-remaining-transport-tdd.md)；[既有安全修订计划](2026-10-03-tunnel-security-protocol-revision-tdd.md) |
| 任务与运行期 | identity v3 是未来任务迁移前置；`time/`、`tasks/`、`polling/`、`operations/` 与 `0004_tasks.sql` 未落地。 | [任务/运行剩余步骤](2026-10-07-controller-v1-remaining-runtime-tdd.md) |
| Web 管理与交付 | 独立 Vue 基础/认证/会话 UI 与 mock 测试已写；设备/任务页、SSE、Rust 静态资源嵌入及浏览器验收尚缺。 | [管理前端剩余步骤](2026-10-07-controller-v1-remaining-web-tdd.md) |
| G5 联合验收 | 现有 `make test` 仅覆盖 workspace Cargo 与既有单机 UI Node 测试；不能代表真库、双进程、真实浏览器或容量验收。 | [跨专题集成与 AT-01..18 验收](2026-10-07-controller-v1-integration-acceptance-tdd.md) |

**执行依赖与持续阻断：** G0 须逐项审阅 draft-1 新增值；identity v3 的只读完整性检查、真实备份/隔离目标确认先于任何 v4 迁移。02 的纯结构/状态机切片可以在安全决议前独立测试，但现行 `docs/protocol_spec.md` 仍为线协议权威；未经真实外部专家书面决议、必要用户复审、规范修订与双端负例/互操作，不可推进受阻的加密实现或真实两进程业务 smoke。03 的时间纯模型可先行，其执行依赖 01 ACL 与 02 已认证 BoardClient；04 的组件可先使用受控 mock，但不能代替真实浏览器及同源后端。所有 DB 事务禁止网络 I/O；仅 `reboot.execute` 必须在持久发送意图提交后发送，崩溃后不盲发。G5 按 06 §6 的 AT-01..18 分项取证；未经验证的容量目标和 HTTP 可信网络限制保持原状。
**文档静态检查与产品功能测试分层原则：** 本轮计划修订仅针对测试与验收追踪文档的审查缺陷进行规范校正，严禁将“文档修改完成”表述为“产品功能已修复”或“测试已通过”。全 G0-G5 检查点维持未完成（`- [ ]`），历史专题计划（01–04）不整体重写；未来生产实现必须遵守上述共享契约与边界限制。

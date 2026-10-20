# Controller V1 四专题 Task 4 · Web 交互可执行实施计划（2026-10-16）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实。使用 `- [ ]` 复选框跟踪步骤。
>
> **本计划的真实性边界（先读，违反即视为失败）：**
> 1. 本计划的每个 GREEN 都只是**组件层 / 纯函数层**证据：注入的 `on*` 回调是测试替身，`fetch` 由测试 stub。**任何 GREEN 都不得被表述为"后端任务 API 已可用""真实联调通过"或"Task 4 已完成"。**
> 2. 截至本计划编写时，`crates/rsetup-controller/src/http_auth.rs` 只注册了 `/healthz`、`/readyz` 与 `/api/v1/auth/*`；规格 02 §HTTP 表里的 8 个任务端点**在 Rust 侧一条都不存在**。对后端缺失端点**仅作静态注明**；Vitest stub 的 404 仅作为组件错误态单测，**禁止称“真实 404 证据”**，不得在组件任务强制网络访问或伪造真实验收。真实浏览器业务清单须后端已联通（或明确接入非生产 dev fixture，如受控 dev-proxy / fixture）才可执行。
> 3. 本计划只涉及 `apps/controller-web/**`。**禁止修改 `apps/controller-web` 之外任何文件**，特别是 `crates/rsetup-controller/src/{time,devices,polling,auth,authz}/**`、`crates/rsetup-runtime/**`、`crates/rsetup-identity/**`、`crates/rsetup-transport/**`、`crates/rsetup-protocol/**`（Runtime / Identity / Transport）。发现需要改这些文件才能推进时：停止该任务，在报告里写"阻断：需要 Runtime/Identity/Transport 侧接口"，不得就地修改。
> 4. 不新增任何 npm 依赖（含 Playwright / axe-core / vue-router / 组件库），不改 `package.json` / `package-lock.json`；不创建导入未安装依赖的候选死代码；不访问 `secret/`；不执行数据库或迁移命令；不做 `git push`。

**Goal:** 在既有纯函数 `task-presentation.ts` 与独立 `Idempotency-Key` 请求头切片之上，把 Task 4 的 Web 部分（任务预览固定清单、MainTask `id` 实体键、任务提交 `task_id` 202 响应、幂等键确认/重试/重置、任务列表首屏加载与错误态、`authorized_subset` 授权子集呈现、held 任务管理员 time-review、deep-link 下基于服务端数据的取消呈现、unknown 释放锁仅响应 `{released:true,state:'unknown'}` 并在成功后 GET 重取、a11y 对话框与原生候选焦点捕获/恢复、设备重启入口与真实请求白名单）拆成可独立审查、可编译行为 RED→GREEN 的 TDD 任务，并配一个真实浏览器 a11y 验收门禁规范；全程不把 fixture GREEN 冒充后端实现。

**Architecture:**
1. 纯逻辑先行：新建 `task-ops.ts`（DTO + 形状守卫，fail-closed，区分 MainTask `id` 与创建响应 `task_id`，定义 `ReleaseLockResult` 仅含 `{released:true,state:'unknown'}`）与扩展 `task-presentation.ts`（状态/原因/授权子集/幂等键生命周期纯函数），用 Vitest 无 DOM 测试锁定契约。
2. HTTP 编排集中到新建 `task-requests.ts`，导出冻结的真实请求白名单常量与 8 个操作函数；函数内部只允许通过 `api.ts` 的 `get`/`post` 访问。**写请求中只有 `POST /tasks` 使用 `RequestOptions.idempotencyKey` 请求头，其它写请求仅带 `X-CSRF-Token`**。
3. 视图职责划分：`components/TaskStateList.vue`（纯展示 10 状态/原因码/结果证据种类 + 释放锁入口，供 `TaskDetailView.vue` 消费）；`components/TaskModals.vue`（批量风险确认 + unknown 释放锁，基于原生 focusable 属性筛选焦点捕获与 Esc 恢复，`batchDisabled` 严格门控）；`views/TasksView.vue`（主任务列表 + 预览 + 设备勾选 + 批量提交编排，onMounted 首屏加载与错误态，精简掉无数据流的 TaskStateList）；`views/TaskDetailView.vue`（主任务详情 + 分页 children + handleCancel 绑定 + held time-review + unknown 释放锁二级确认并在成功后 GET 重取 children，deep-link 依据服务端数据计算可取消态）。
4. `router.ts` 只做最小扩展：允许 `#/tasks?device=<hex64>`，非法 query 丢弃而不改变既有 canonical 行为；`App.vue` 挂载两个视图、串联设备入口，任务详情通过路由参数传 `id`，由视图自行根据服务端加载数据和权限表判定可取消状态。

**Tech Stack:** Vue 3.5.43、TypeScript 5.9.3 (strict)、Vite 8.3.2、Vitest 5.0.3、@testing-library/vue 8.1.0、jsdom 26.1.0、vue-tsc 3.3.12（全部为已锁定依赖，零新增）。后端契约只作为**只读**参考：Axum 0.8 路由尚未实现。

**Spec:**
- `docs/superpowers/specs/2026-10-07-controller-v1-four-task4-design.md`（本专题收敛设计，§3 §5 为 Web 权威约束）
- **取消入口固定合同（用户批准，本次修订的唯一新增合同）：** 新增字段仅主任务 `actions.can_cancel: boolean`，由后端按当前身份、目标设备 `device.reboot` 与合格 `queued`/`held` 子任务计算并返回；前端只判“可请求”，不保成功（服务端事务 CAS 与逐端点重验身份/权限为最终准绳）；`ViewScope` 必须兼容合法 `full` 与 `authorized_subset`（不新增 `can_time_review` 之类字段）；前端不得借 children 分页、`task.read` 或主任务状态（`queued`/`held`/`running`/`completed`）猜 reboot；缺 `actions` / 非法 `can_cancel` 布尔 / 未知 `view_scope` 一律 fail-closed 关闭危险入口，只读任务兼容缺 `actions`；`running` 主任务可以有可取消目标，`authorized_subset` 下的 `completed` 主任务也不能按主任务状态关闭。权威出处：01 §4 `device.reboot` 与 §6:71、04 §4/§8、设计文档 §3（逐端点重验与取消 CAS 语义）与 §5（Web 交互契约）。
- `docs/superpowers/plans/2026-10-07-controller-v1-remaining-web-tdd.md` 的 `### Task 4`（上位增量计划，本计划是其可执行细化）
- `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md`（§HTTP 端点与字段表：`/task-previews`、`/tasks`、children、cancel、time-review、release-lock）
- `docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md`（`device.task.read` 权限语义、404 统一投影）
- `docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`（主任务 4 态、子任务 10 状态、12 原因码、held 核验语义）
- `docs/superpowers/specs/2026-09-23-controller-v1-06-web-acceptance.md`（U-01/U-02 与 AT-05/AT-16）

## Global Constraints

1. **零新增依赖与禁止候选死代码：** 禁止安装或升级任何 npm/cargo 包；禁止改 `package.json`、`package-lock.json`、`Makefile`、CI 配置。不创建引入未安装 Playwright / axe-core 的候选 `.spec.ts` 死代码文件；真实浏览器验收以可手工执行的清单（`README.md`）为准。
2. **文件范围：** 只允许新增/修改 `apps/controller-web/src/**`、`apps/controller-web/tests/browser/README.md`（手工清单）、`docs/superpowers/plans/2026-10-16-controller-v1-four-task4-web-execution.md`。禁止触碰 Runtime / Identity / Transport / Protocol / 数据库 / 迁移任何文件。
3. **伪 RED 禁令：** 每一步 RED 前必须先建立**可编译 stub**（导出同名符号、返回空值/空模板），确保 `vitest` 能加载测试文件且运行数非 0；因断言失败而红才算行为 RED。编译错误、模块不存在、`is not a function` 类型的报错不是有效 RED，必须回到 stub 步骤修好。
4. **幂等键约束：** 幂等键只能是规范小写 UUIDv4（`^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$`）；**只能且仅在 `POST /tasks` 请求中作为 HTTP `Idempotency-Key` 请求头传输**，其它写请求绝不发送此头；禁止放进 JSON body、URL、`localStorage`/`sessionStorage`、Cookie 或日志。无 `crypto.getRandomValues` 时必须抛出，绝不 fallback 到 `Math.random`。
5. **Cookie / CSRF：** 前端永不读写 Cookie（禁止 `document.cookie`）。POST 一律带 `X-CSRF-Token`（`/auth/login` 除外）；GET 绝不带。
6. **状态真实性：** 子任务 10 种状态分别显示，`unknown` 必须呈现"结果未确认"，禁止用失败文案诱导自动重发；`accepted` 只是已接收，不得显示为成功；主任务仅 `queued/running/held/completed` 四态（04 §4），`completed` 必须配对五词 outcome（`success/cancelled/unknown/partial/failed`）之一、非 completed 必须为 `null`，主任务列表/详情一律经 `mainTaskStateLabel` 呈现，不得借用子任务十态 `taskStateLabel` 或 `task.state.*` 键；不得由前端本地构造审计事实或延期结论。
7. **授权投影：** 任务列表/详情只显示服务端返回的当前授权子集；`view_scope` 标记缺失或非 `authorized_subset` 时按"未知范围"呈现，禁止展示隐藏目标数量、禁用过滤前总数、跨用户数据。
8. **a11y：** 对话框必须 `role="dialog"` + `aria-modal="true"` + 可见标题（`aria-labelledby`）+ Esc 关闭 + 基于原生可聚焦属性筛选的焦点捕获 + 关闭后焦点恢复到触发按钮；所有文案走 `i18n.t()`，精简无消费的文案键，禁止模板硬编码中文/英文。
9. **提交边界：** 每项任务的提交只暂存该任务 `Files:` 列出的路径；提交前执行 `git diff --cached --name-only` 核对，并确认 staged diff 不含凭据、不含 `secret/`、不含其它专题文件。仓库全局无 `git add -f`。
10. **测试命令固定形式：** 单文件 RED/GREEN 用 `npm --prefix apps/controller-web test -- --run <path>`；全量回归用 `npm --prefix apps/controller-web test -- --run`；类型检查 `npm --prefix apps/controller-web run typecheck`。禁止使用 watch 模式。

---

## 文件与职责总览

| 文件 | 动作 | 职责 |
| --- | --- | --- |
| `apps/controller-web/src/task-ops.ts` | Create | 任务 DTO（MainTask `id`、创建响应 `task_id`、4 态 state / 5 词 outcome、`created_time` 完整 `TaskTimeEvidence`、十态 `TaskCounts`、可选 `actions.can_cancel`、`ReleaseLockResult`）、fail-closed 形状守卫（`TaskTimeEvidence`/`TaskCounts` 本地定义，不改 `types.ts`）、`describeAuthorizedSubset`、`canRequestCancel` 取消入口纯函数 |
| `apps/controller-web/src/task-ops.test.ts` | Create | 上述纯逻辑的单测 |
| `apps/controller-web/src/task-presentation.ts` | Modify | 新增 `startTaskOperation` / `buildTaskPreviewRequest` / held 决策列表等纯函数；新增 `mainTaskStateLabel`（主任务四态文案，独立于子任务十态 `taskStateLabel`）；既有导出签名不变 |
| `apps/controller-web/src/task-presentation.test.ts` | Modify | 幂等键生命周期、预览请求构造、held 决策集合、主任务四态文案单测 |
| `apps/controller-web/src/task-requests.ts` | Create | 冻结真实请求白名单 + 8 个任务操作函数（仅 `POST /tasks` 带幂等头；release-lock 仅校验 `ReleaseLockResult`） |
| `apps/controller-web/src/task-requests.test.ts` | Create | 白名单与逐函数请求形状/取消/错误传播测试 |
| `apps/controller-web/src/components/TaskStateList.vue` | Create | 子任务状态/原因/结果证据种类 + 释放锁按钮（供 `TaskDetailView` 消费） |
| `apps/controller-web/src/components/TaskStateList.test.ts` | Create | 状态真实性、unknown 文案、admin 门控、空列表 |
| `apps/controller-web/src/components/TaskModals.vue` | Create | 批量风险确认弹框 + unknown 释放锁二级确认（属性筛选 focus trap、`batchDisabled` 修复、Esc 恢复） |
| `apps/controller-web/src/components/TaskModals.test.ts` | Create | dialog 语义、原生候选焦点捕获/恢复、Esc、取消零副作用、表单门控 |
| `apps/controller-web/src/views/TasksView.vue` | Create | MainTask 列表（`id` 键）+ onMounted 首屏加载与错误态 + 预览 + 设备勾选 + 提交编排（修复 confirmDisabled） |
| `apps/controller-web/src/views/TasksView.test.ts` | Create | 幂等键生成/重试/重置、onMounted 首屏加载与错误态、设备勾选与预览清单、stub 404 错误态单测 |
| `apps/controller-web/src/views/TaskDetailView.vue` | Create | 主任务详情（`id` 键）+ 分页 children + 取消入口（唯一依据后端 `actions.can_cancel`，只消费 `canRequestCancel` 与 busy）+ handleCancel 绑定 + held time-review + release-lock 二级确认与成功后 GET 重取 |
| `apps/controller-web/src/views/TaskDetailView.test.ts` | Create | time-review 决策、不延期呈现、children 分页、取消入口矩阵（running+true、completed+subset+true、full+true 开；queued false / 缺 / 非法 action / 未知 scope 关；task.read + 第一页 queued 不覆盖后端 false）、release-lock GET 重取 |
| `apps/controller-web/src/router.ts` | Modify | `#/tasks?device=<hex64>` 查询解析与格式化（其余行为不变） |
| `apps/controller-web/src/router.test.ts` | Modify | 新 query 行为的正反测试 + 既有 canonical 不回归 |
| `apps/controller-web/src/App.vue` | Modify | 挂载 TasksView / TaskDetailView、串联 `rebootDevice` 入口与任务导航，不向 TasksView 传入无用 subtasks 数据 |
| `apps/controller-web/src/App.test.ts` | Modify | 路由挂载、设备入口连通、无权限不加载任务数据、删除未实现 TODO stub |
| `apps/controller-web/src/locales/zh-CN.ts` / `en.ts` | Modify | Task 4 所需键的 1:1 补齐（精简无消费的死键与重复键） |
| `apps/controller-web/src/i18n.test.ts` | Modify | 新增键存在性与 1:1 等长断言 |
| `apps/controller-web/tests/browser/README.md` | Create | 手工真实浏览器验收清单与"未联通真实后端/dev fixture 则不执行业务操作"的边界说明 |

---


---

## 专题带编号拆分模块索引（单文件 700~2700 行）

为保障可维护性并消除单文件过大问题，完整执行细节按职责已拆分到对应子目录中（保留本主计划在 `docs/superpowers/plans/` 作为执行总索引）：

- [01-ops-and-requests.md](2026-10-16-web/01-ops-and-requests.md)：Task 1–5: 词典、DTO 形状守卫、幂等键与请求编排（~1868 行）
- [02-state-list-and-modals.md](2026-10-16-web/02-state-list-and-modals.md)：Task 6–7: TaskStateList、TaskModals 弹窗与确认（~742 行）
- [03-tasks-and-detail-views.md](2026-10-16-web/03-tasks-and-detail-views.md)：Task 8–9: TasksView 列表去重、TaskDetailView 详情写清错（~1563 行）
- [04-router-and-acceptance.md](2026-10-16-web/04-router-and-acceptance.md)：Task 10–12: 路由接线、浏览器 a11y 手工清单与回归复核（~688 行）

---


# Task 4 真实浏览器验收门禁（手工清单）

本目录只存放**人工执行**的浏览器验收清单，供 Task 4（任务预览 → 确认 → 提交、任务列表/详情状态呈现、unknown 释放锁、held 时间核验）在真实浏览器中逐项核对并留证。

## 1. 依赖与执行边界（零新增依赖声明）

- 本仓库 `apps/controller-web` **未安装** `@playwright/test`、`@axe-core/playwright` 或任何 axe 规则引擎；本目录**只有本 README**，不保留引入未安装依赖的候选 `.spec.ts` / `*.browser.spec.ts` 死代码文件。
- 本任务**不修改** `package.json`、`package-lock.json` 与 CI 配置，**不运行** `npx playwright`。若后续决定引入真实浏览器工具链，须另立任务，先补依赖与锁文件，再新增可执行用例。
- jsdom 组件测试（Vitest）**不能**替代真实浏览器验证：jsdom 无真实布局与滚动、无真实 Tab 顺序与焦点绘制、无辅助技术（AT）语义树、无 axe 规则引擎、无 DevTools 网络面板。jsdom 结果一律不作为浏览器验收证据。
- **写操作边界（硬门禁）**：当前中控真实任务 HTTP 路由（`/api/v1/task-previews`、`/api/v1/tasks*`、`/api/v1/subtasks/*/release-lock`）在后端**尚未连通**。若后端未联通真实路由，且未明确接入**非生产 dev fixture**，则**不执行任何业务写操作**（预览、提交、取消、时间核验、释放锁一律不点），只做静态注明；不得伪造真实验收通过证据。
- 只读项（路由渲染、文案、a11y 属性、键盘焦点、视口）可在只读或 fixture 环境执行；任何需要点击危险按钮的项，必须先确认目标为已获授权的隔离开发环境。

## 2. 前置条件

1. 后端按 [`crates/rsetup-controller`](<../../../../crates/rsetup-controller>) 启动。若任务路由未注册，`/api/v1/tasks*` 返回 404 时**仅静态注明**，不据此判定前端失败。
2. 前端开发服务器由执行人按 [`DEV-PROXY.md`](<../../DEV-PROXY.md>) **手工启动**（`CONTROLLER_DEV_BACKEND_TARGET` 必须为 loopback http 且带显式端口），同源代理 `/api/v1`；**不得**由测试脚本自动拉起新服务。
3. 准备**管理员账号**与**普通账号**各一个；测试 unknown 释放锁与 held 核验需具备对应子任务 fixture（`state: "unknown"`、`state: "held"`）。
4. 记录浏览器与版本（Chromium / Firefox / WebKit 任一，注明具体版本）；建议同时用一套屏幕阅读器（NVDA/VoiceOver）抽查对话框。
5. 打开 DevTools → Network，勾选 Preserve log，用于 B/G 组证据。

### 2.1 已知静态阻断（执行前必读，不得伪造通过）

- **任务页面入口**：截至本清单编写时，[`src/App.vue`](<../../src/App.vue>) 尚未挂载 `TasksView` / `TaskDetailView` 分支（Task 10 接线范围）。若入口按钮不存在、`#/tasks` 渲染为设备页，则本清单 A 组之后的所有交互项标记为**阻断（前端未接线）**，只作静态注明。
- **设备深链**：[`src/router.ts`](<../../src/router.ts>) 的 `parseHash` 不解析 hash 查询串，`#/tasks?device=<hex64>` 会落回 `#/devices`；`TasksView` 的 `pendingDeviceId` 目前无 URL 来源。A1 中该子项按**未实现**记录，不记为通过。
- 上述两项一旦由后续任务接线完成，须重新执行 A 组并更新记录。

## 3. 手工检查项

逐项记录"通过 / 失败 / 阻断 + 截图或录屏文件名 + 浏览器与版本"。任何"通过"必须有证据文件。

### A. 路由与刷新

- [ ] A1 地址栏直接输入 `#/tasks`、`#/tasks/<规范 UUIDv4>` 均正确渲染对应视图；刷新后 hash 不变、不跳回 `#/devices`。
- [ ] A2 畸形 `#/tasks/<非 UUIDv4>`（如 `#/tasks/1`、`#/tasks/NOT-A-UUID`）落回 `#/tasks`，且使用 `replaceState` 纠错：按一次"后退"不会被困在畸形 hash（无后退陷阱）。
- [ ] A3 `#/tasks?device=<hex64>` 深链：按 §2.1 记录为**未实现**（当前落回 `#/devices`）。
- [ ] A4 未登录访问 `#/tasks`、`#/tasks/<uuid>` 被守卫送回 `#/login`；`must_change_password` 用户被送回改密页。

### B. 键盘导航（Focus Trap 与 Esc 焦点恢复）

对象：[`src/components/TaskModals.vue`](<../../src/components/TaskModals.vue>)（批量风险确认 `mode="batch-risk"` 与释放设备占用 `mode="release-lock"`）。

- [ ] B1 纯键盘全流程：仅用 Tab / Shift+Tab / Enter / Space / Esc 可完成"勾选设备 → 生成预览 → 打开批量确认 → 勾选风险 → 确认提交"，全程不依赖鼠标。
- [ ] B2 **Tab 焦点捕获循环**：弹框打开后，焦点位于弹框内第一个可聚焦元素；在**最后一个**可聚焦元素上按 Tab，焦点回到**第一个**；焦点始终不逃出弹框（背景页面元素不可被 Tab 命中）。
- [ ] B3 **Shift+Tab 反向循环**：在**第一个**可聚焦元素上按 Shift+Tab，焦点跳到**最后一个**；连续反向循环不逃逸、不卡死在 `document.body`。
- [ ] B4 循环候选集合：Tab 序列**跳过** `disabled` 按钮（含确认按钮在 reason 为空/未勾选风险时的 disabled 态）、`type="hidden"` 输入与 `tabindex="-1"` 元素；实现仅按原生可聚焦属性筛选，无 `offsetParent` 可见性判定，故须实测确认无"看不见的焦点"。
- [ ] B5 **初始聚焦**：以 `open=true` 直接挂载（父组件挂载即打开）时，焦点也在下一帧移入弹框第一个可聚焦元素，不停留在触发按钮或 `body`。
- [ ] B6 **Esc 关闭与焦点安全恢复**：按 Esc 关闭弹框后，焦点自动回到**打开弹框前的源触发按钮**；关闭时不产生任何 POST（Network 中 POST 计数为 0）。
- [ ] B7 **源焦点恢复的降级路径**：触发按钮在弹框打开期间被移除/替换（例如提交成功后列表重取、触发按钮 `v-if` 消失）时，Esc 或父组件置 `open=false` 关闭弹框**不报错**（控制台无异常），焦点不困在已卸载节点或 dialog 内，页面仍可用 Tab 重新进入可聚焦内容。
- [ ] B8 父组件驱动关闭（提交成功后直接把 `open` 置 `false`，不走 Esc/取消按钮）同样恢复源焦点，且**不**触发第二次 `close` 回授（无重复请求、无死循环）。
- [ ] B9 提交在途（`busy`）时确认按钮为 disabled 且带 loading 文案，连点不产生第二个 `POST /api/v1/tasks`。

### C. 对话框无障碍属性

- [ ] C1 弹框根节点具备 `role="dialog"` 与 `aria-modal="true"`。
- [ ] C2 `aria-labelledby` 指向**可见**标题元素：批量风险弹框为 `task-modal-batch-title`，释放锁弹框为 `task-modal-release-title`；对应 `<h3>` 文案可见且非空（AT 朗读名称非空）。
- [ ] C3 释放锁弹框正文含"不撤回执行"类风险警示段落，且释放确认按钮文案与风险确认勾选说明一致。
- [ ] C4 弹框内 `input`（释放原因）与 `checkbox`（风险确认）均有可朗读名称（`aria-label` 或与 `<label>` 关联），AT 不朗读为空。
- [ ] C5 释放确认按钮在 reason 为空或未勾选风险时为 disabled，AT 朗读为"不可用/灰显"，而非"可点击"。
- [ ] C6 已知限制（须人工确认并如实记录，不记为通过）：背景内容未设置 `aria-hidden`/`inert`，AT 的浏览光标（虚拟光标，非 Tab 焦点）仍可能读到弹框外内容；对话框为普通 `<div>` 而非原生 `<dialog>`，无浏览器内置模态语义。
- [ ] C7 若后续安装 axe 类工具，可补跑自动规则；**当前未安装**，本项记为"未执行（无依赖）"，不得声称已跑无障碍扫描。

### D. 任务列表与详情状态呈现

- [ ] D1 **主任务四态**文案与 `state` 一一对应，无第五态、无空白：`queued`→"排队中"、`running`→"运行中"、`held`→"等待核验"、`completed`→"已完成"（英文界面须有对应译文，不得中英混排）。
- [ ] D2 **十态子任务**文案齐全：`queued`/`held`/`dispatching`/`accepted`/`verifying`/`succeeded`/`failed`/`unknown`/`cancelled`/`expired`；词表外状态安全落回"未知状态"，不显示原始枚举串。
- [ ] D3 **原因码**：12 个规范原因码（`PERMISSION_DENIED`、`DEVICE_OFFLINE`、`CAPABILITY_UNAVAILABLE`、`DEVICE_BUSY`、`QUEUE_EXPIRED`、`TIME_UNCERTAIN`、`EXECUTION_REJECTED`、`OS_ERROR`、`RESULT_TIMEOUT`、`JOURNAL_LOST`、`PROTOCOL_MISMATCH`、`USER_CANCELLED`）显示本地化文案；非法/超长/伪造字符串落回"未知原因"，**原始输入不进入界面文案或 i18n key**。
- [ ] D4 **结果证据种类**：`result_evidence` 按六值词表（`attempted`、`boot_transition_observed`、`execution_rejected`、`os_error`、`journal_lost`、`none`）以"结果证据: <snake_case>"呈现，只作当前授权的结果证据种类，界面不冒称完整 wire 证据对象或成功证明；`result_evidence` 为 `null`（证据未产生）时**不渲染**证据行；`execution_token` 及其摘要在页面上任何位置都不出现。
- [ ] D5 状态真实性：`unknown` 行显示"结果未确认"，页面无"失败""已自动重试"诱导文案；`accepted` 行不显示为成功；`held` 行不显示为失败。
- [ ] D6 授权范围：`view_scope: "authorized_subset"` 显示"仅显示当前已授权的目标"，且**不**显示任何隐藏总数或"共 N 台"；`view_scope` 缺失或为其它值显示"授权范围未知，已按最小投影显示"，绝不显示"全部"。
- [ ] D7 分页：列表与子任务列表"加载更多"按游标推进，重复点击不产生并发重复请求；末页无"加载更多"残留。

### E. unknown 释放锁（危险操作，需 fixture 与授权环境）

- [ ] E1 权限门：普通账号在 `unknown` 子任务行**看不到**"释放设备占用"按钮；管理员可见。
- [ ] E2 点击释放 → 出现 `role="dialog"` + `aria-modal="true"` 弹框，正文含"不撤回执行"警示；点"取消"或按 Esc 关闭，**不产生任何 POST**，焦点回到触发按钮。
- [ ] E3 确认按钮门控：reason 为空、仅空格或未勾选风险确认时 disabled；填写并勾选后可点。
- [ ] E4 确认请求体恰为 `{expected_revision, acknowledge_unknown: true, reason}`（`reason` 为 trim 后文本），无多余字段；请求头仅 CSRF，**不携带** `Idempotency-Key`。
- [ ] E5 成功后页面通过 GET 重新获取子任务（Network 可见 `GET /api/v1/subtasks/...` 或所属 children 请求），界面以服务端返回为准；`unknown` 行不因前端本地乐观更新被改写为成功。

### F. held 时间核验（需 held fixture 与管理员账号）

- [ ] F1 管理员且 `view_scope === "full"` 且有 held 子任务时，看到**恰好两个**决策按钮："核验后按原期限恢复" 与 "核验后终止为已过期"；页面上**不存在**任何"延长期限/自定义期限"控件或输入框。
- [ ] F2 非管理员（或 `view_scope` 非 `full`）看不到决策按钮，但仍能看到核验说明与"核验结果以服务端返回为准"字样。
- [ ] F3 无 held 子任务时显示"当前没有 held 子任务，无需核验"。
- [ ] F4 决策请求体含 `expected_revision`、`decision`、`evidence`；`evidence` 仅取 `reference_deadline_proven` / `reference_deadline_expired` / `inconclusive` 三值之一，界面不自行判定审计事实。

### G. 网络证据与幂等

- [ ] G1 写请求中**只有 `POST /api/v1/tasks` 携带 `Idempotency-Key`**，值为规范小写 UUIDv4；请求 body **不含**该字段。其它写请求（`task-previews`、`cancel`、`time-review`、`release-lock`）仅携带 CSRF，无幂等头。
- [ ] G2 断开网络后点重试：第二次请求的 `Idempotency-Key` 与第一次**相同**（一次确认只生成一次 key）。
- [ ] G3 重新生成预览使 preview 指纹变化后，确认勾选被清空，下一次提交的 `Idempotency-Key` 与上一次**不同**。
- [ ] G4 任务相关请求路径全部在白名单内：`task-previews`、`tasks`、`tasks/<id>`、`tasks/<id>/children`、`tasks/<id>/cancel`、`tasks/<id>/time-review`、`subtasks/<id>/release-lock`；出现任何其它任务路径即判失败。
- [ ] G5 语言切换（中 ↔ English）：设备勾选与在途幂等键不重置，且 Network 中 POST 计数增量为 **0**。
- [ ] G6 缺后端声明：若后端未连通且未接入 dev fixture，本组全部记为**阻断**，并在记录表备注"后端未连通，未执行任何业务写操作"。

### H. 视口与缩放

- [ ] H1 320×640 视口下任务列表、详情与两种弹框均无横向滚动条，弹框内按钮可完整触达。
- [ ] H2 200% 浏览器缩放下弹框操作区不被裁切，焦点环可见（未被 `outline: none` 抹掉）。

## 4. 记录格式

| 项目 | 结论（通过/失败/阻断/未执行） | 证据文件 | 浏览器/版本 | 时间 | 备注 |
| --- | --- | --- | --- | --- | --- |
| A1 | | | | | |
| B2 | | | | | |
| B6 | | | | | |
| C1 | | | | | |
| C2 | | | | | |
| D1 | | | | | |
| D4 | | | | | |
| E2 | | | | | |
| F1 | | | | | |
| G1 | | | | | |
| H1 | | | | | |

## 5. 门禁状态声明模板（不得声称已验收）

执行报告须按以下格式书写，未执行的项写"未执行"，不得留空或写"应该没问题"：

```
Task 11 状态：手工清单=<已创建，未执行 / 已执行（执行人、浏览器与版本）>；未安装依赖=<无死代码引入>；
真实浏览器验收结论=<未通过|部分通过|通过>，若后端未连通则静态注明阻断。jsdom 结果不作为浏览器验收证据。
```

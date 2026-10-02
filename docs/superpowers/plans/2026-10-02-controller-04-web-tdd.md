# 中控 V1 Web 交付实施计划（条件性 · 04）

> **致执行代理：** 使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实；用 `- [ ]` 跟踪每步。先运行失败测试，再做最小实现、全绿、重构、提交。

**Goal:** 用 Vue 3/Vite 交付双语、可访问、按权限展示的管理页，并把产物嵌入独立 Rust 中控。

**Architecture:** `apps/controller-web` 只访问新中控的 `/api/v1`，不复用无认证的板端 `ui/`。`crates/rsetup-controller::web` 提供资源路由，和管理 API 同源；身份/数据计划与任务/运行期计划提供服务端 API。

**Tech Stack:** Rust 2024 / MSRV 1.85，Axum 0.8，Vue 3/Vite；TypeScript、Vitest、@testing-library/vue、jsdom（组件测试 DOM）、Playwright、axe-core 是待批准工程候选；`rust-embed` 是 draft-1 候选（规格 00 §5），未获批前不固化嵌入机制。

**Spec:** `docs/superpowers/specs/2026-09-22-controller-design.md`、`docs/superpowers/specs/2026-09-23-controller-v1-00-index.md`、`docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md`、`docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md`、`docs/superpowers/specs/2026-09-23-controller-v1-03-device-protocol.md`、`docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`、`docs/superpowers/specs/2026-09-23-controller-v1-05-runtime-operations.md`、`docs/superpowers/specs/2026-09-23-controller-v1-06-web-acceptance.md`、`PRODUCT.zh.md`。执行时同时读规格与计划。

## Global Constraints

- `controller-v1 / draft-1` 待审阅；此条件性计划**不是开工许可**。先批准登录、API、SSE、组件和测试库选择；协议的 reason_code 签名、验签失败与 AEAD 失败策略及双端测试是生产发布阻断项。
- 现有 `crates/rsetup-app/src/server.rs` 仅供板端 loopback、本地无认证，绝不迁移其变更操作到多用户中控。中控管理端仅 HTTP，提示可信网络风险，不声称自带 TLS。
- `device.reboot` 不隐含读取详情、状态或任务；不泄露隐藏目标或数量；HTTP 202 仅表示持久接收，`unknown` 不等于失败/成功。API 的权限点/错误码/状态名语言中立。
- 每个 RED 必须运行并因**预期行为缺失**失败；工具未安装、无法编译、拼写错误不算有效 RED，要修好测试再跑。测试真实组件/HTTP 结果；仅边界使用 fake HTTP，避免只断言 mock 次数。
- 每个任务结束运行目标测试与受影响回归并小提交；本文未执行代码或测试。真实 MySQL/TiDB、容量及端到端验证依赖其他专题的集成。

## 文件结构、边界与接口

- Create `apps/controller-web/{package.json,vite.config.ts,playwright.config.ts,tsconfig.json,index.html}`：构建和测试。
- Create `apps/controller-web/src/{main.ts,App.vue,api.ts,types.ts,router.ts,i18n.ts,stream.ts,styles.css}`：应用入口、API、会话关卡、双语与事件。
- Create `apps/controller-web/src/views/{LoginView.vue,PasswordView.vue,DevicesView.vue,DeviceDetailView.vue,TasksView.vue,AdminView.vue,SystemView.vue}`：职责分离的页面；相关测试 colocate。
- Create `apps/controller-web/tests/flows.spec.ts`：浏览器双语/无障碍。
- Create `crates/rsetup-controller/src/web.rs`, `crates/rsetup-controller/tests/web_assets.rs`；Modify `crates/rsetup-controller/{Cargo.toml,src/lib.rs}`, `Makefile`, `.github/workflows/ci.yml`：资源嵌入、精确路由和构建/测试接线（01 创建 crate 后才可运行 Rust 测试）。
- Consumes 02 已定义（不是本专题新增）的包裹结构 `{data,request_id}` / `{error:{code,message_key,params},request_id}` 及 `/api/v1/auth/{me,login,password,logout}`、设备/管理 API、`POST /api/v1/task-previews`、`POST/GET /api/v1/tasks`、`GET /api/v1/tasks/{id}`、`GET /api/v1/tasks/{id}/children`、`POST /api/v1/tasks/{id}/cancel`、`POST /api/v1/tasks/{id}/time-review`、`POST /api/v1/subtasks/{id}/release-lock`、`GET /api/v1/audit`、`GET /api/v1/events`、`GET /api/v1/system/{time,status}`；身份/权限语义见 01，板端状态/时钟载荷见 03，时间/系统状态语义见 05。Produces `pub fn web::router() -> axum::Router`，由中控 merge，不单独监听。接口冲突先修改契约，不能复制服务端权限逻辑。

---

### Task 1: Vue 单入口与类型化 API

**Files:** Create 前端构建四文件 `apps/controller-web/{package.json,vite.config.ts,tsconfig.json,index.html}`、`src/{main.ts,App.vue,api.ts,types.ts,api.test.ts}`。

**Interfaces:** `request<T>(path: string, options?: RequestInit): Promise<T>`，`ApiError.code/messageKey`；`types.ts` 导出 `AuthUser {must_change_password:boolean; is_admin:boolean}`、`DeviceProjection {device_id:string; display_name:string; effective_permissions:string[]}`；Counter 类型为 JSON decimal string。

- [ ] **Step 1: RED。** 先配置经批准的测试运行器，然后写 `src/api.test.ts`：

```ts
import {afterEach,expect,test,vi} from 'vitest';
import {request} from './api';
afterEach(()=>vi.unstubAllGlobals());
test('202 is receipt, error keeps stable code',async()=>{
  vi.stubGlobal('fetch',vi.fn<typeof fetch>()
    .mockResolvedValueOnce(new Response(JSON.stringify({data:{task_id:'id',accepted:true},request_id:'r'}),{status:202}))
    .mockResolvedValueOnce(new Response(JSON.stringify({error:{code:'PERMISSION_DENIED',message_key:'errors.permission',params:{}},request_id:'r2'}),{status:403})));
  expect(await request<{task_id:string;accepted:boolean}>('/tasks',{method:'POST',headers:{'X-CSRF-Token':'test-session-token'}})).toEqual({task_id:'id',accepted:true});
  await expect(request('/users')).rejects.toMatchObject({code:'PERMISSION_DENIED'});
});
test('login POST succeeds without a prior CSRF token',async()=>{
  const loginFetch=vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify({data:{csrf_token:'fresh'},request_id:'r3'}),{status:200}));
  vi.stubGlobal('fetch',loginFetch);
  expect(await request<{csrf_token:string}>('/auth/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({username:'admin',password:'secret'})})).toEqual({csrf_token:'fresh'});
  const [url,options]=loginFetch.mock.calls[0]!;
  expect(url).toBe('/api/v1/auth/login');
  expect(options?.credentials).toBe('same-origin');
  expect(new Headers(options?.headers).has('X-CSRF-Token')).toBe(false);
});
```

- [ ] **Step 2: 确认 RED。** `npm --prefix apps/controller-web test -- --run src/api.test.ts`；先修复导入等测试错误，观察未解包数据/错误导致的断言失败。
- [ ] **Step 3: 最少 GREEN。** `api.ts` 用 `fetch('/api/v1'+path,{credentials:'same-origin',...options})`；成功仅返回 `data`，错误抛含中立 code/messageKey 的 `ApiError`。路径限制同源；非安全方法使用当前会话的 `X-CSRF-Token`，无 token 时阻止发送；**login 端点豁免**（无先验会话 token，仍要求同源 + JSON，成功后获得 CSRF token）。App 从 `auth/me` 读取身份与 CSRF token，不硬编码管理员。
- [ ] **Step 4: 确认 GREEN。** 目标 PASS；增加无 token 写请求、401/404、异常 JSON 的用例；`npm --prefix apps/controller-web test -- --run` PASS。
- [ ] **Step 5: 重构并提交。** 提取解包辅助函数，全绿后 `git add apps/controller-web && git commit -m 'feat(controller-web): add typed Vue API shell'`。

### Task 2: 登录及强制改密

**Files:** Create `src/{router.ts,router.test.ts}`, `src/views/{LoginView.vue,LoginView.test.ts,PasswordView.vue}`；Modify `src/App.vue`, `src/api.ts`。

**Interfaces:** `resolveInitialRoute(me: AuthUser|null): '/login'|'/password'|'/devices'`；临时用户不挂载业务页面或 SSE。

- [ ] **Step 1: RED。**

```ts
import {expect,test} from 'vitest';
import {resolveInitialRoute} from './router';
test('temporary admin sees only password gate',()=>{
  expect(resolveInitialRoute({must_change_password:true,is_admin:true})).toBe('/password');
  expect(resolveInitialRoute(null)).toBe('/login');
  expect(resolveInitialRoute({must_change_password:false,is_admin:false})).toBe('/devices');
});
```

- [ ] **Step 2: 确认 RED。** `npm --prefix apps/controller-web test -- --run src/router.test.ts`；补空导出后确认行为断言失败，而非导入失败。
- [ ] **Step 3: 最少 GREEN。** 返回 `!me?'/login':me.must_change_password?'/password':'/devices'`，导航守卫先取 `/auth/me`；改密成功清除会话状态并要求重新登录，不读取设备或打开 SSE。
- [ ] **Step 4: 确认 GREEN。** 目标 PASS；`LoginView.test.ts` 测登录 429 + `Retry-After` 限速反馈、账号不存在/密码错误同一失败文案；组件测改密前禁止设备/SSE、停用或401跳回登录，运行全量前端单测 PASS。
- [ ] **Step 5: 重构、复测、提交。** `git add apps/controller-web/src && git commit -m 'feat(controller-web): gate first login'`。

### Task 3: 设备授权投影与管理员页

**Files:** Create `src/views/{DevicesView.vue,DevicesView.test.ts,DeviceDetailView.vue,DeviceDetailView.test.ts,AdminView.vue,AdminView.test.ts,SystemView.vue,SystemView.test.ts}`；Modify `src/{router.ts,types.ts}`。

**Interfaces:** 消费 `/devices`, `/devices/{id}`, `/devices/{id}/status`, `/approvals/approve`、用户/组/角色/授权 API；输出 `canReadStatus(permissions: string[]): boolean` 与选定公钥/revision 快照。

- [ ] **Step 1: RED。**

```ts
import {render,screen} from '@testing-library/vue';
import {expect,test} from 'vitest';
import DevicesView from './DevicesView.vue';
test('reboot-only projection hides status',async()=>{
  render(DevicesView,{props:{devices:[{device_id:'a'.repeat(64),display_name:'A',effective_permissions:['device.reboot']}]}});
  expect(await screen.findByText('A')).toBeTruthy();
  expect(screen.queryByText(/CPU|温度|Temperature/)).toBeNull();
  expect(screen.getByRole('button',{name:/reboot|重启/i})).toBeTruthy();
});
```

  另在 `DeviceDetailView.test.ts` 先写 U-01 失败用例：准入、连接、control/data 流健康、监控新鲜度、时钟质量各有独立标签；流 `healthy` 且时钟 `stale` 时分别显示，断言不存在汇总的全绿“在线”结论；无采样时显示等待采集/unknown 而非 0。

- [ ] **Step 2: 确认 RED。** `npm --prefix apps/controller-web test -- --run src/views/DevicesView.test.ts src/views/DeviceDetailView.test.ts`；补齐最小页面壳，确保因未按权限/五维呈现而失败。
- [ ] **Step 3: 最少 GREEN。** 列表最小识别与 `DeviceDetailView.vue` 详情路由分离，仅按后端授权投影渲染；有 `device.read` 才取档案/能力/准入/连接/流健康，有 `device.status.read` 才取最新状态与板端时间质量。详情将准入、连接、control/data 业务流健康、监控新鲜度、时钟质量独立呈现，不用一个绿色“在线”汇总；无采样显示等待采集/unknown、null 不填零，stale/error 保留真实旧样本与错误。批量审批提交用户勾选时固定的 `{device_id,expected_revision}`，确认后新增行不入批；仅管理员访问审批、用户、组/角色/授权、审计、时间/系统页；提示吊销恢复须板端人工重置。

```ts
export const canReadStatus = (permissions: string[]) => permissions.includes('device.status.read');
export const approvalTargets = (rows: {device_id: string; revision: string}[]) =>
  rows.map(({device_id, revision}) => ({device_id, expected_revision: revision}));
```
- [ ] **Step 4: 确认 GREEN。** 目标 PASS；`DeviceDetailView.test.ts` 补五维互不聚合、权限隔离、无采样/null/旧样本组合断言；`DevicesView.test.ts` 和 `AdminView.test.ts` 测设备列表及准入审批 cursor 分页（筛选绑定、篡改 cursor 的 400、翻页只发 GET 不重发写请求/`Idempotency-Key`）；补隐藏数量、仅 read/reboot 不隐含 status、审批部分失败、revision 冲突和固定清单；列明 Task 4 的 `TasksView.test.ts` 必须覆盖批量重启未勾选风险确认不得提交主任务；`SystemView.test.ts` 测 NTP 质量/退避与 `/system/{time,status}`；本任务前端全部单测 PASS。
- [ ] **Step 5: 重构、复测、提交。** `git add apps/controller-web/src && git commit -m 'feat(controller-web): show authorized devices and admin'`。

### Task 4: 任务确认和 SSE 重同步

**Files:** Create `src/views/{TasksView.vue,TasksView.test.ts}`, `src/{task-presentation.ts,task-presentation.test.ts,i18n.ts,stream.ts,stream.test.ts}`；Modify `src/views/{DeviceDetailView.vue,DeviceDetailView.test.ts}`, `src/{router.ts,types.ts}`。

**Interfaces:** 消费已由 02 定义、由 03 Task 5 实现的 `/task-previews`, `/tasks`, `/tasks/{id}`, `/tasks/{id}/children`, `/tasks/{id}/cancel`, `/tasks/{id}/time-review`, `/subtasks/{id}/release-lock`, `/audit`, `/events`；从 `task-presentation.ts` 导出 `taskLabel(state,locale)`，从 `stream.ts` 导出 `subscribeAndResync(fetchSnapshot,source)`。

- [ ] **Step 1: RED。** 以下标签测试放在 `src/task-presentation.test.ts`，使用同目录相对导入；风险确认和任务交互另放 `src/views/TasksView.test.ts`。

```ts
import {expect,test} from 'vitest';
import {taskLabel} from './task-presentation';
test('unknown is not completion evidence',()=>{
  expect(taskLabel('unknown','zh-CN')).toBe('结果未确认');
  expect(taskLabel('accepted','zh-CN')).not.toBe('成功');
});
```

  另在 `stream.test.ts` 先建立订阅，令 revision 8 事件先于 revision 7 的 GET 返回，断言最终读取 revision 8 的**当前授权投影**；`task-presentation.test.ts` 对 10 个状态（queued/held/dispatching/accepted/verifying/succeeded/failed/unknown/cancelled/expired）逐一断言中英标签与未知状态稳定错误码/默认语言回退。`TasksView.test.ts` 先写并运行明确 RED 的组件用例：管理员在 unknown 子任务上点击“释放占用”时第一次只弹出“可能已经重启”二次风险确认，未确认不得 POST；确认后请求须带当前 `expected_revision`、`acknowledge_unknown:true`、非空 `reason`，仅释放锁且旧任务仍 unknown、旧结果保留、不产生新的 `/tasks` POST；取消或 revision 冲突不得释放/不得重发。另从 held 主任务详情打开管理员 time-review，分别选择 `resume_original_deadline`/`expire`，提交主任务当前 `expected_revision` 与 `evidence`，无 held 时禁用入口或展示 400，普通用户不见入口；测试其服务端响应所示的全部 held 子任务原子结果、截止不延长，未确认不 POST。测试壳/导入先可编译，失败须来自二次确认/字段/合法性/原子展示行为缺失。
- [ ] **Step 2: 确认 RED。** `npm --prefix apps/controller-web test -- --run src/views/TasksView.test.ts src/task-presentation.test.ts src/stream.test.ts`；先修好加载问题，逐项确认 unknown 二次风险确认/不重发、time-review 入口及合法性、状态误判、缺翻译或丢事件断言因行为缺失失败，记录 RED 用例名，不以空测试文件或导入错误冒充。
- [ ] **Step 3: 最少 GREEN。** 预览冻结清单、确认风险后提交（未勾选不得提交主任务）；同一提交网络重试复用 UUID `Idempotency-Key`，新确认才换 key；202 只显示已接收，unknown 不自动重试。任务列表→详情 GET `/tasks/{id}` 和分页 children GET 展示服务端当前授权投影；仅可取消的 queued/held 子任务显示取消入口，按当前 revision/`confirm:true` 请求 `/tasks/{id}/cancel`，dispatching 后提示不可保证撤回。管理员从 held 主任务详情进入 time-review，逐项展示原截止/证据，选择 `resume_original_deadline` 或 `expire` 后向 `/tasks/{id}/time-review` 发送主任务 `expected_revision` 和 `evidence`；没有 held 禁止提交（服务端仍以 400 为准），不能延长原截止，全部 held 原子核验结果须从服务端重取，权限或 revision 冲突按服务端错误显示。管理员 unknown 子任务的 `/subtasks/{id}/release-lock` 独立于批量重启首轮风险确认：第一步查看原 unknown 结果与持锁设备，第二步显式确认“可能已经重启，释放锁不等于失败/不会撤回执行”，填写非空 reason 后才 POST `{expected_revision,acknowledge_unknown:true,reason}`；取消确认不发送，响应后保留旧任务状态/结果并 GET 重取，不复用旧预览或 Idempotency-Key、不自动 POST 新 `/tasks`。`taskLabel` 从 `i18n.ts` 的词典取全部 10 个状态的中英标签，不复制字面量；未知状态回退稳定错误码/默认语言。详情任务入口仅按权限展示；管理员 audit 页读取 `/audit`，UI 不构造审计事实。SSE 先订阅并缓冲再 GET，较新 revision 触发 GET 重取，reset/断线标非实时并重取；撤权旧消息不得渲染，卸载关闭订阅；任务只显示授权子集与证据。

```ts
import {textFor} from './i18n';
export function taskLabel(state: string, locale: string) {
  return textFor(`task.${state}`, locale);
}
```
- [ ] **Step 4: 确认 GREEN。** 目标 PASS；`TasksView.test.ts` 测批量重启未确认风险不能提交，以及预览过期、响应丢失、隐藏子任务、取消不可撤回；额外逐项复跑 Step 1 的 unknown 锁释放二次确认（取消不 POST、确认须带 acknowledge_unknown/reason/revision、保持原 unknown 结果、不提交新操作）、time-review 管理员路径/两个合法 decision/无 held 400/权限与 revision 冲突/原截止不延长与全部 held 原子重取的组件 RED→GREEN。`DeviceDetailView.test.ts` 验证任务入口不把任务接收/流健康合成“在线成功”，并在准入与连接正常、流 `healthy`、监控新鲜度 `unknown`、时钟 `stale` 的混合状态下分别显示五维标签；`task-presentation.test.ts` 测 10 状态中英标签及未知回退；补 SSE 溢出/reset/撤权，管理员审计只读授权投影；前端全部单测 PASS。AT-18 需与 03 Task 5 的服务端授权过滤/背压-reset/断线后 GET 持久结果联合验收，单独组件 mock 不算服务端完成。
- [ ] **Step 5: 重构、复测、提交。** `git add apps/controller-web/src && git commit -m 'feat(controller-web): add safe tasks and events'`。

### Task 5: 双语、无障碍与静态产物内嵌

**Files:** Create `src/{i18n.test.ts,styles.css}`, `playwright.config.ts`, `tests/flows.spec.ts`, `crates/rsetup-controller/src/web.rs`, `crates/rsetup-controller/tests/web_assets.rs`；Modify `src/i18n.ts`, `apps/controller-web/{package.json,vite.config.ts}`, `crates/rsetup-controller/{Cargo.toml,src/lib.rs}`, `Makefile`, `.github/workflows/ci.yml`。

**Interfaces:** 资源模块导出 `web::router() -> axum::Router`；页面回退只能处理页面路由，API 和缺失资产禁止回退 HTML。

- [ ] **Step 1: RED。**

```ts
import {expect,test} from 'vitest';
import {textFor} from './i18n';
test('bilingual key and stable fallback',()=>{
  expect(textFor('task.unknown','zh-CN')).toBe('结果未确认');
  expect(textFor('task.unknown','en')).toBe('Result unconfirmed');
  expect(textFor('TASK_NEW_UNKNOWN','zh-CN')).toBe('TASK_NEW_UNKNOWN');
  expect(textFor('time.quality.stale','zh-CN')).not.toBe('time.quality.stale');
});
```

  另在 `src/i18n.test.ts` 和 `src/views/TasksView.test.ts`、`src/views/DevicesView.test.ts` 写 U-02 RED：在已勾选设备、已生成同一次提交的 UUID `Idempotency-Key` 且权限投影固定时切换 `zh-CN`↔`en`，刷新后偏好仍保存；选择集合、key、待提交 payload、按钮授权/禁用状态、unknown 含义不变，切换和刷新自身不 POST `/tasks`；导航、表格、危险确认和状态的 accessible name/aria-label 与 `document.documentElement.lang` 随语言改变，焦点仍可见并可键盘完成。分别用组件真实渲染/受控 fake HTTP 断言结果，先运行失败再实现，不能只断言词典函数。
  `web_assets.rs` 用 `tower::ServiceExt::oneshot` 向真实 `web::router()` 发请求，增加 U-04 RED：已构建的内容哈希 `/assets/*.js`/`*.css` 返回匹配 MIME `Content-Type` 与长期 immutable 缓存，入口 `/`/`/devices` 返回 `text/html` 并要求重新验证；缺失资产 `/assets/missing.js` 和 `/api/v1/missing` 不返回 HTML/200/入口缓存头。先确认这些断言因资源行为缺失而失败。
- [ ] **Step 2: 确认 RED。** `npm --prefix apps/controller-web test -- --run src/i18n.test.ts src/views/TasksView.test.ts src/views/DevicesView.test.ts`，逐项记录 U-02 语言偏好持久化、选择/key/权限/无障碍标签不变或随语言更新的行为断言失败；01 的 crate 就绪后，先建最小 `crates/rsetup-controller/src/web.rs`（空 `router()` 导出）并挂入 `src/lib.rs`，先构建前端资源再运行 `cargo test -p rsetup-controller --test web_assets`，逐项确认 U-04 MIME/缓存/边界失败来自路由行为而非缺产物/导入/编译错误；缺 crate 不是有效 RED，先完成依赖再跑行为断言。
- [ ] **Step 3: 最少 GREEN。** 扩展 Task 4 的词典，覆盖导航/错误/审批/风险/全部 10 个任务状态/时间，未知板端文案安全纯文本回退；保存 `zh-CN`/`en` 语言偏好并在切换/刷新时恢复，语言仅影响文案、`document.documentElement.lang` 及当前可见的 accessible name/aria-label，不重建设备选择/确认中的 payload 或 UUID `Idempotency-Key`，不重算服务端权限、不触发写请求，也不改变 unknown 的语义。样式含 `:focus-visible`、`prefers-reduced-motion`、`prefers-contrast`；弹窗进入/退出焦点恢复。Vite 产物哈希化；嵌入机制（`rust-embed` 或等价 `build.rs`/`include` 方案）仍是待审候选，选择会影响 debug Rust 构建是否要求 dist，审批后明确接线；无论选择哪种机制，使用 Vite preview 的 Playwright 测试都必须先完成 `npm --prefix apps/controller-web run build`。行为约束不变：生产 Rust 构建编译时嵌入全部 dist，缺产物使发布构建失败，发布二进制运行无需 Node/静态目录。对 `.js`/`.css` 返回正确 Content-Type，哈希资产 `Cache-Control: public, max-age=31536000, immutable`，入口 HTML `Cache-Control: no-cache`（或等价必须重新验证策略）；缺失资产/API 不返回 SPA HTML。Makefile 独立 `controller-build` 顺序先 npm build 后 cargo build；`make test` 串联 `apps/controller-web` 的 Vitest、生产资源 build 和 Playwright（所选嵌入机制若要求 dist，则相关 Rust 测试同样在 build 后执行），CI 在 `.github/workflows/ci.yml` 同步 Node/npm 依赖、Playwright 浏览器与上述顺序，并保留 01 Task 6 的双数据库 URL 门控/ignored 测试接线及 03 Task 6 获批后新增的三组真实 DB 测试；现有 `ui/*.test.mjs` 板端测试和目标不受影响。
- [ ] **Step 4: 确认 GREEN。** 单测/Rust 路由测试 PASS；复跑 U-02 真实组件测试核对偏好保存/刷新、切换中设备选择和 UUID `Idempotency-Key` 不变、权限按钮/任务语义不变、无写请求、`lang`/无障碍标签更新；复跑 U-04 `web_assets.rs` 核对 HTML/哈希 JS/CSS 的实际 Content-Type、入口重新验证与哈希资产长期 immutable 缓存、缺资源/API 不回退 HTML。`playwright.config.ts` 的 webServer 启动 Vite preview 加载已构建 dist，`tests/flows.spec.ts` 用 Playwright `page.route('**/api/v1/**')` 拦截同源请求并提供受控 mock API（固定身份、权限、任务状态及时间响应）；Playwright+axe 验证中英文、语言切换/刷新后偏好及标签、键盘/焦点、桌面/320px、非颜色状态、减少动效。E2E 允许 Node/dev 工具，不把 preview 当发布运行环境；U-04/AT-17 的发布二进制约束由 `web_assets.rs` 与下述运行检查验证。构建后临时移走 dist 再运行现成二进制检查无需 Node/静态目录及 API/缺文件不被 SPA 掩盖，完成后恢复。组合执行 `make test`、`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`npm --prefix apps/controller-web run build` 并确保全绿；`make test` 不替代 01/03 各引擎显式 URL + ignored DB 测试。
- [ ] **Step 5: 重构、复测、提交。** `git add apps/controller-web crates/rsetup-controller Makefile .github/workflows/ci.yml Cargo.toml Cargo.lock && git commit -m 'feat(controller-web): embed accessible bilingual console'`。

## 覆盖/退出关卡

06 U-01/U-02/U-03/U-04 → Task 3/4、Task 2/3/4/5、Task 4、Task 5；AT-16/17 → Task 5/5；AT-18 → 03 Task 5（服务端授权过滤、背压/reset、持久 GET）+ 本文 Task 4（订阅缓冲、重连 GET 当前授权投影）联验，不能以单端 mock 或 SSE 通知代替持久结果。AT-01..03 与 01 专题联合验收，AT-05..07 与 03 专题联合验收。已设计不等于已测试、获批准或生产安全可用。

### Task 10: 设备入口与路由接线（`router.ts` + `App.vue`）

**Files:**
- Modify: `apps/controller-web/src/router.ts`
- Modify: `apps/controller-web/src/router.test.ts`
- Modify: `apps/controller-web/src/App.vue`
- Modify: `apps/controller-web/src/App.test.ts`

**Interfaces:**
- Consumes: 既有 `AppRoute`、`parseHash`/`formatRouteToHash` 内部函数、`createRouter(auth)`、`guardRoute`、`syncHash`；`DevicesView` 的 `rebootDevice` emit（`apps/controller-web/src/views/DevicesView.vue:142`，**只读，禁止改该文件**）；`TasksView`/`TaskDetailView` 的 props/emits（Task 8/9 已定义）；`task-requests.ts` 的 8 个函数。
- Produces：
  ```ts
  // router.ts
  export type AppRoute =
    | ... 既有成员不变
    | { name: 'tasks'; query?: { device: string } }        // device 为 hex64，非法时整个 query 被丢弃
    | { name: 'task-detail'; params: { id: string } }
  ```
  - `parseHash('#/tasks?device=<hex64>')` → `{ name: 'tasks', query: { device } }`；query 中 `device` 非 hex64 或存在未知参数 → 返回 `{ name: 'tasks' }`（丢弃 query，不重定向）。
  - `formatRouteToHash({ name: 'tasks', query: { device } })` → `#/tasks?device=<device>`；无 query 时仍是 `#/tasks`。
  - **其它路由的解析/格式化/守卫行为必须逐字不变**（既有 `router.test.ts` 全部用例必须继续通过）。
  - **WEB-R5-3：query 解析严格限定于 `tasks` 列表分支**（`raw === 'tasks'` 或 `raw.startsWith('tasks?')`）；非 tasks 路由带 query（如 `parseHash('#/sessions?x=1')`、`#/devices/<hex64>?x=1`）不剥离 query，仍匹配不到任何已知分支，照旧 fallback 到 `{ name: 'devices' }`。
  ```ts
  // App.vue 新增（内部函数，不导出）
  function handleRebootDevice(deviceId: string): void   // 校验 hex64 后 navigateTo({ name: 'tasks', query: { device: deviceId } })
  const tasksPendingDevice = computed<string | null>(...)  // 从 router.currentRoute.value 取 query.device
  const taskPermissionByDevice = computed<Record<string, string[]>>(...)  // 来自 devicesList 的 effective_permissions
  ```
  App.vue 渲染契约：
  - `route.name === 'tasks'` → 挂载 `TasksView`，注入 `:devices`、`:pending-device-id="tasksPendingDevice"`、`:csrf-token`（按现有 `auth` 暴露的字段取，若不存在则传空串并由测试锁定）、`on-preview/on-submit/on-cancel/on-load-tasks` 走 `task-requests.ts`，`@open-task` → `navigateTo({ name: 'task-detail', params: { id } })`。
  - **`tasks` 路由冷启动/刷新必须加载最小设备列表**：既有 `watch([() => auth.status.value, () => router.currentRoute.value.name])` 的路由分支需增加对 `routeName === 'tasks'` 的处理——进入 tasks 路由且 `devicesList.value.length === 0 && !devicesLoading.value` 时调用 `void loadDevices()`（与 `routeName === 'devices'` 分支同型，复用既有 generation/auth/error 防御；不新增 endpoint、不扩 `AppRoute`）。由此 `#/tasks` 与 `#/tasks?device=<hex64>` 冷启动或刷新时 `:devices="devicesList"` 不再恒空，目标设备可勾选、`pendingDeviceId` 可命中（命中依赖下方 `tasksPendingDevice` 的 devicesList 成员门控）。
   - `route.name === 'task-detail'` → 挂载 `TaskDetailView`，注入 `:task-id="route.params.id"`、`:is-admin="auth.user.value?.is_admin === true"`、`:device-permissions="taskPermissionByDevice"`（App 提供权限表供接线与后续用途；TaskDetailView 的取消入口**仅**由服务端返回的 `actions.can_cancel` 经 `canRequestCancel` 判定，权限表不参与取消判定）、`onLoadTask/onLoadChildren/onCancel/onTimeReview/onReleaseLock` 走 `task-requests.ts`。
  - `route.name === 'devices'` → 既有 `DevicesView` 增加 `@reboot-device="handleRebootDevice"`；**不发任何任务写请求**（只导航）。
  - 导航区新增"任务"入口（`nav.tasks`），点击 `navigateTo({ name: 'tasks' })`。
  - 登出/`permissions.changed` 时清空任务相关内存状态（`tasks` 列表、待处理设备、幂等键所在视图随组件卸载销毁）。

- [ ] **Step 1: 编写失败测试（router）**

在 `apps/controller-web/src/router.test.ts` 末尾追加（沿用文件既有 `fakeAuthStore`/`window.location.hash` 操作方式）：

```ts
describe('tasks query routing for the device reboot entry', () => {
  it('parses a hex64 device query into the tasks route', async () => {
    const auth = fakeAuthStore(
      { id: 'u-1', username: 'u', is_admin: false, must_change_password: false, revision: '1' },
      'signed_in'
    )
    const router = createRouter(auth)
    const device = 'a'.repeat(64)
    window.location.hash = `#/tasks?device=${device}`
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    await vi.waitFor(() => expect(router.currentRoute.value).toEqual({ name: 'tasks', query: { device } }))
    window.location.hash = '#/tasks'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    await vi.waitFor(() => expect(router.currentRoute.value).toEqual({ name: 'tasks' }))
    router.cleanup()
  })

  it('drops a malformed or unknown query instead of redirecting elsewhere', async () => {
    const auth = fakeAuthStore(
      { id: 'u-1', username: 'u', is_admin: false, must_change_password: false, revision: '1' },
      'signed_in'
    )
    const router = createRouter(auth)
    window.location.hash = '#/tasks?device=NOT-A-DEVICE'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    await vi.waitFor(() => expect(router.currentRoute.value).toEqual({ name: 'tasks' }))
    window.location.hash = '#/tasks?device=' + 'a'.repeat(64) + '&extra=1'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    await vi.waitFor(() => expect(router.currentRoute.value).toEqual({ name: 'tasks' }))
    router.cleanup()
  })

  it('keeps the devices fallback for non-tasks routes carrying a query', async () => {
    const auth = fakeAuthStore(
      { id: 'u-1', username: 'u', is_admin: false, must_change_password: false, revision: '1' },
      'signed_in'
    )
    const router = createRouter(auth)
    window.location.hash = '#/sessions?x=1'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    await vi.waitFor(() => expect(router.currentRoute.value).toEqual({ name: 'devices' }))
    router.cleanup()
  })
})
```

（若既有文件用工厂函数辅助，实施时复用它；断言形状与命名不得改动。）

- [ ] **Step 2: 运行并确认失败（router RED）**

运行：`npm --prefix apps/controller-web test -- --run src/router.test.ts`

预期：新增 2 个用例失败，`AssertionError: expected { name: 'tasks' } to deeply equal { name: 'tasks', query: { device: 'aaa…' } }`，WEB-R5-3 回归用例实现前后均通过（仅锁定非 tasks 路由带 query 的既有 fallback）；既有用例仍全绿。若新增用例导致既有用例失败，说明基线被破坏，必须先修回。

- [ ] **Step 3: 实现 `router.ts` 最小改动**

仅在 `parseHash` 的 `tasks` 列表分支拆分 query（WEB-R5-3：非 tasks 路由不剥离 query，既有分支匹配基准逐字不变），并在 `formatRouteToHash` 的 `tasks` 分支拼接 query：

```ts
const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/   // 复用文件既有常量

function parseTasksQuery(search: string): { query?: { device: string } } {
  if (!search) return {}
  const params = new URLSearchParams(search)
  if (params.size !== 1) return {}                       // 未知或多余参数：整体丢弃
  const device = params.get('device')
  if (device === null || !DEVICE_ID_HEX64.test(device)) return {}
  return { query: { device } }
}

function parseHash(hash: string): AppRoute {
  const raw = hash.replace(/^#\/?/, '')
  // WEB-R5-3：仅 tasks 列表分支解析 query；raw 本身（含 query）继续作为所有既有分支的匹配基准，
  // 非 tasks 路由带 query（如 '#/sessions?x=1'、'#/devices/<hex64>?x=1'）仍匹配不到任何已知分支，
  // 照旧落入 devices fallback，逐字兼容。
  if (raw === 'tasks' || raw.startsWith('tasks?')) {
    const question = raw.indexOf('?')
    const search = question < 0 ? '' : raw.slice(question + 1)
    return { name: 'tasks', ...parseTasksQuery(search) }
  }
  // ... 既有分支保持不变：继续对 raw（含 query）匹配，tasks/task-detail 分支均不解析 query
}

function formatRouteToHash(route: AppRoute): string {
  switch (route.name) {
    // ...
    case 'tasks':
      return route.query?.device ? `#/tasks?device=${route.query.device}` : '#/tasks'
    // 其余分支不变
  }
}
```

`guardRoute`/`syncHash`/`navigate`/`onHashChange` 不动；`onHashChange` 里的相等性比较天然支持带 query 的形状（`JSON.stringify` 比较）。

- [ ] **Step 4: 运行并确认通过（router GREEN）**

运行：`npm --prefix apps/controller-web test -- --run src/router.test.ts`

预期：PASS（既有 + 新增全部）。

- [ ] **Step 5: 编写失败测试（App 接线）**

在 `apps/controller-web/src/App.test.ts` 末尾追加：

```ts
test('device reboot entry navigates to the tasks route carrying the device id and issues no write request', async () => {
  fetchForMe(jsonResponse({ data: ME_DATA, request_id: RID_ME }))
  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
    const url = String(input)
    if (url.includes('/api/v1/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME })
    if (url.includes('/api/v1/devices')) {
      return jsonResponse({ data: { items: [{
        device_id: 'a'.repeat(64), display_name: 'Alpha',
        effective_permissions: ['device.read', 'device.task.read', 'device.reboot'],
      }], next_cursor: null }, request_id: 'r-dev' })
    }
    if (url.includes('/api/v1/tasks')) return jsonResponse({ data: { items: [], next_cursor: null }, request_id: 'r-tasks' })
    return jsonResponse({ error: { code: 'NOT_FOUND', message_key: 'errors.notFound' }, request_id: 'r-nf' }, 404)
  })
  vi.stubGlobal('fetch', fetcher)

  // 隔离旧路由初態：App.test.ts 前序旧 test 结束后可能遗留 #/devices/<id>，此处显式回到设备列表初態
  window.location.hash = '#/devices'
  render(App)
  await vi.waitFor(() => expect(screen.getByText('Alpha')).toBeTruthy())
  await fireEvent.click(screen.getByTestId(`reboot-action-${'a'.repeat(64)}`))

  await vi.waitFor(() => expect(window.location.hash).toBe(`#/tasks?device=${'a'.repeat(64)}`))
  const writes = fetcher.mock.calls.filter(([, init]) => (init as RequestInit | undefined)?.method === 'POST')
  expect(writes).toHaveLength(0)
})

test('renders the tasks view for the tasks route without admin-only release controls', async () => {
  fetchForMe(jsonResponse({ data: ME_DATA, request_id: RID_ME }))
  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
    const url = String(input)
    if (url.includes('/api/v1/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME })
    if (url.includes('/api/v1/devices')) return jsonResponse({ data: { items: [], next_cursor: null }, request_id: 'r-dev' })
    if (url.includes('/api/v1/tasks')) return jsonResponse({ data: { items: [{
      id: 'f47ac10b-58cc-4372-a567-0e02b2c3d479', operation: 'reboot', state: 'running', outcome: null,
      revision: '7',
      created_time: {
        system_wall_utc: '2026-10-16T00:00:00Z', reference_utc: '2026-10-16T00:00:00Z',
        quality: 'system_fallback', clock_epoch: '0f8fad5b-d9cb-469f-a165-70867728950e',
        source: null, sample_age_ms: null, offset_ms: null, uncertainty_ms: null,
      },
      counts: { queued: '0', held: '0', dispatching: '0', accepted: '0', verifying: '0', succeeded: '0', failed: '0', unknown: '0', cancelled: '0', expired: '0' },
      view_scope: 'authorized_subset',
    }], next_cursor: null }, request_id: 'r-tasks' })
    return jsonResponse({ error: { code: 'NOT_FOUND', message_key: 'errors.notFound' }, request_id: 'r-nf' }, 404)
  })
  vi.stubGlobal('fetch', fetcher)
  window.location.hash = '#/tasks'
  render(App)
  await vi.waitFor(() => expect(screen.getByRole('heading', { name: '任务列表' })).toBeTruthy())
  expect(screen.queryByRole('button', { name: /释放设备占用/ })).toBeNull()
  expect(window.location.hash).toBe('#/tasks')
})

test('cold start on #/tasks?device=<id> loads the minimal device list and preselects the pending device', async () => {
  const deviceId = 'a'.repeat(64)
  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
    const url = String(input)
    if (url.includes('/api/v1/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME })
    if (url.includes('/api/v1/devices')) {
      return jsonResponse({ data: { items: [
        { device_id: deviceId, display_name: 'Alpha', effective_permissions: ['device.read', 'device.task.read', 'device.reboot'] },
        { device_id: 'b'.repeat(64), display_name: 'Beta', effective_permissions: ['device.read', 'device.task.read'] },
      ], next_cursor: null }, request_id: 'r-dev' })
    }
    if (url.includes('/api/v1/tasks')) return jsonResponse({ data: { items: [], next_cursor: null }, request_id: 'r-tasks' })
    return jsonResponse({ error: { code: 'NOT_FOUND', message_key: 'errors.notFound' }, request_id: 'r-nf' }, 404)
  })
  vi.stubGlobal('fetch', fetcher)

  // 冷启动：render 前 hash 已是目标 URL，devicesList 初始恒空，由 App.vue watch 的 tasks 分支触发 loadDevices
  window.location.hash = `#/tasks?device=${deviceId}`
  render(App)
  await vi.waitFor(() => expect(screen.getByRole('checkbox', { name: 'Alpha' })).toBeTruthy())
  expect(fetcher.mock.calls.some((call) => String(call[0]).includes('/api/v1/devices'))).toBe(true)
  expect((screen.getByRole('checkbox', { name: 'Alpha' }) as HTMLInputElement).checked).toBe(true)
  expect((screen.getByRole('checkbox', { name: 'Beta' }) as HTMLInputElement).checked).toBe(false)
})

test('direct navigation (refresh) to #/tasks triggers the minimal device list load', async () => {
  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit) => {
    const url = String(input)
    if (url.includes('/api/v1/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME })
    if (url.includes('/api/v1/devices')) {
      return jsonResponse({ data: { items: [
        { device_id: 'a'.repeat(64), display_name: 'Alpha', effective_permissions: ['device.read', 'device.reboot'] },
      ], next_cursor: null }, request_id: 'r-dev' })
    }
    if (url.includes('/api/v1/tasks')) return jsonResponse({ data: { items: [], next_cursor: null }, request_id: 'r-tasks' })
    return jsonResponse({ error: { code: 'NOT_FOUND', message_key: 'errors.notFound' }, request_id: 'r-nf' }, 404)
  })
  vi.stubGlobal('fetch', fetcher)

  // 刷新/直接导航：先隔离前序 test 遗留路由初态，render 前 hash 直接置为 #/tasks
  window.location.hash = '#/devices'
  window.location.hash = '#/tasks'
  render(App)
  await vi.waitFor(() => expect(screen.getByRole('checkbox', { name: 'Alpha' })).toBeTruthy())
  expect(fetcher.mock.calls.some((call) => String(call[0]).includes('/api/v1/devices'))).toBe(true)
  expect(window.location.hash).toBe('#/tasks')
})
```

- [ ] **Step 6: 运行并确认失败（App RED）**

运行：`npm --prefix apps/controller-web test -- --run src/App.test.ts`

预期：新增用例失败：`AssertionError: expected '#/devices' to be '#/tasks?device=aaa…'`、`Unable to find role "heading" and name "任务列表"`；既有用例全绿。
（有效行为 RED；`ME_DATA` 为既有非管理员用户 fixture。）

- [ ] **Step 7: 实现 `App.vue` 接线**

按 Interfaces 补齐：import 两个视图与 `task-requests.ts`；新增 `handleRebootDevice`、`tasksPendingDevice`、`taskPermissionByDevice`、任务列表加载；模板增加 `nav.tasks` 按钮、`tasks`/`task-detail` 两个分支、`DevicesView` 的 `@reboot-device`。要求：
- 任务列表读取走 `listTasks(null, { csrfToken: auth.csrfToken.value ?? '' })` 并对响应做 `assertTaskPageShape`；失败时保留错误态，不伪装空列表。
- `TasksView` 注入 `onPreview` 调 `createTaskPreview`、`onSubmit` 调 `submitTask`、`onCancel` 调 `cancelTask`、`onLoadTasks` 调 `listTasks`。
- `TaskDetailView` 注入 `onLoadTask` 调 `getTask`、`onLoadChildren` 调 `getTaskChildren`、`onCancel` 调 `cancelTask`、`onTimeReview` 调 `reviewHeldTime`、`onReleaseLock` 调 `releaseUnknownLock`。
- 幂等键由 `TasksView` 生成并传入 `submitTask` 的 `idempotencyKey`；`App.vue` **不得**自行生成或缓存幂等键，也不得把它写入任何持久存储。
- `handleRebootDevice` 必须先校验 hex64 再导航；非法 id 直接忽略（不发请求、不导航）。

`App.vue` 增量实现计划（片段为接线规范，供实现时参照；`computed` 若未引入需补入既有 `vue` import）：

`<script setup lang="ts">`（新增部分）：

```ts
import TasksView from './views/TasksView.vue'
import TaskDetailView from './views/TaskDetailView.vue'
import {
  createTaskPreview,
  submitTask,
  cancelTask,
  listTasks,
  getTask,
  getTaskChildren,
  reviewHeldTime,
  releaseUnknownLock,
} from './task-requests'
import type { SubTaskItem, TaskItem, TaskPage, TaskPreview } from './task-ops'
import type { TimeReviewEvidenceKind } from './task-requests'

const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/i

// 命名约定：App.vue 既有对象只有 `router`（createRouter(auth)）、`navigateTo(route: AppRoute)`、`auth`。
// 本文其它小节出现的 `navigate(...)` 是简写，实现时一律写成既有的 `navigateTo(...)`；不存在名为 `navigate` 的函数。
// `route` 定义为 `router.currentRoute` 的只读别名（AppRoute 判别联合），脚本与模板统一经它读取当前路由，
// 不得改写 router、不得用同步 hash setter 赋值。
const route = computed(() => router.currentRoute.value)

// 从 URL query 提取待处理设备（只读路由，不落本地缓存）；
// query 只挂在 tasks 分支且可选，必须先按 name 窄化再可选链读取，缺省回退 null
// devicesList 成员门控：冷启动/刷新时 devicesList 初始恒空（loadDevices 异步返回），
// 若直接透传 query，TasksView 挂载时的一次性判定与 pendingDeviceId watch 都会错过；
// 门控后 pendingDeviceId 在设备到达时发生 null 到 id 的跳变，触发 TasksView 既有 watch 完成默认勾选。
const tasksPendingDevice = computed<string | null>(() => {
  const current = route.value
  const device = current.name === 'tasks' ? current.query?.device ?? null : null
  return device !== null && devicesList.value.some((d) => d.device_id === device) ? device : null
})

// 从 devicesList 聚合每设备任务权限（形状与 TaskDetailView 的 `devicePermissions?: Record<string, string[]>` 一致）
const taskPermissionByDevice = computed<Record<string, string[]>>(() => {
  const permissions: Record<string, string[]> = {}
  for (const device of devicesList.value) {
    permissions[device.device_id] = device.effective_permissions ?? []
  }
  return permissions
})

// tasks 路由冷启动/刷新：devicesList 初始恒空，TasksView 的 :devices 与 pendingDeviceId 命中都依赖它。
// 在既有 watch([() => auth.status.value, () => router.currentRoute.value.name]) 的路由分支中新增以下分支
// （与既有 routeName === 'devices' 分支同型；登出分支的 devicesGeneration++/清空逻辑不变）：
//   if (routeName === 'tasks' && devicesList.value.length === 0 && !devicesLoading.value) {
//     void loadDevices()
//   }
// 复用既有 loadDevices()（自带 loading 防重入、generation 比对、signed_in 校验与错误兜底）；不新增 endpoint、不扩 AppRoute。

// DevicesView 重启入口：hex64 校验通过才导航；非法直接忽略（不发请求、不导航）
function handleRebootDevice(deviceId: string) {
  if (!DEVICE_ID_HEX64.test(deviceId)) return
  navigateTo({ name: 'tasks', query: { device: deviceId } })
}
```

Callback props 接线要点（Task 10 必要增量——`TasksView`/`TaskDetailView` 的回调均为 props 而非 emits，这是适配组件 callback props 的必要接线）：

- 两视图通过 `defineProps` 声明回调属性：
  - `TasksView`：`onPreview`、`onSubmit`、`onCancel`、`onLoadTasks`；
  - `TaskDetailView`：`onLoadTask`、`onLoadChildren`、`onCancel`、`onTimeReview`、`onReleaseLock`。
- 父模板**不得**用 `@preview`/`@submit`/`@load-tasks`/`@load-task`/`@time-review`/`@release-lock` 监听：这些名称未在各视图 `emits` 中声明，`@xxx` 只会成为属性透传，视图内 `props.on*` 恒为 `undefined`，运行时不报错、难以排查。
- 请求函数**禁止原样作为 callback 传入**，必须经「callback 适配器层（禁止原样传引用）」小节（见下方）的 8 个适配器中转（与对照表一致）；App.vue 模板因此以适配器函数作 prop 绑定（见下方模板片段）；`cancelTask` 同样必须经 `adaptCancel` 中转，不存在直传例外：
  - `TasksView`：`:on-preview="adaptPreview"`、`:on-submit="adaptSubmit"`、`:on-cancel="adaptCancel"`、`:on-load-tasks="adaptLoadTasks"`；
  - `TaskDetailView`：`:on-load-task="adaptLoadTask"`、`:on-load-children="adaptLoadChildren"`、`:on-cancel="adaptCancel"`、`:on-time-review="adaptTimeReview"`、`:on-release-lock="adaptReleaseLock"`。
- 禁止原样传引用的原因：`createTaskPreview`、`submitTask`、`listTasks`、`getTask`、`getTaskChildren`、`cancelTask`、`reviewHeldTime`、`releaseUnknownLock` 这 8 个请求函数需要 `TaskRequestContext` 且返回 `ApiResponse<T>` 信封，而视图 callback 消费裸 DTO，必须经下方小节定义的 8 个适配器注入 context、映射 payload、解包 `res.data` 后中转；所有绑定的 callback 函数（8 个适配器，无直传引用）均返回 `Promise`，**不得**再包一层同步包装函数，否则视图内 `await props.on*()` 的在途态（`busy`/防双击）门控语义被破坏。
- 若误用 `@event` 形式，首屏任务加载、预览生成、批量提交、详情/children 拉取、held time-review 与释放锁都会因 `props.on*` 为 `undefined` 而静默失败，Task 8/9 单测的 `waitFor` 断言将全部超时，且构建不报错（属性透传合法）。

callback 适配器层（禁止原样传引用）：

- 总则：请求函数需要 `TaskRequestContext`（含 `csrfToken`）且返回 `ApiResponse<T>` 信封，而视图 callback 消费裸 DTO；二者**禁止原样互传函数引用**，必须经以下 async 适配器中转：注入请求 context（`csrfToken: auth.csrfToken.value ?? ''`——`auth.csrfToken` 是 `Ref<string | null>`，必须取 `.value`，缺省注入空串）与**可选**的可取消 `AbortSignal`（适配器入参声明为 `signal?: AbortSignal`，**不得要求必传**，未传时以 `undefined` 注入 `context.signal`）、映射 payload（裸 DTO ↔ `SubmitTaskParams`，字段 snake_case ↔ camelCase，如 `preview_token` ↔ `previewToken`、`expected_revision` ↔ `expectedRevision`、`acknowledge_unknown` ↔ `acknowledgeUnknown`）、解包 `ApiResponse<T>` 的 `res.data` 后再交给视图 callback；void 语义 callback 必须 `await` 完成后才返回，每个适配器返回 `Promise`。

```ts
// adaptPreview / adaptSubmit：与 Task 8 视图 callback 签名逐字兼容的完整实现
async function adaptPreview(
  operation: 'reboot',
  deviceIds: string[],
  signal?: AbortSignal,
): Promise<TaskPreview> {
  const res = await createTaskPreview(operation, deviceIds, {
    csrfToken: auth.csrfToken.value ?? '',
    signal,
  });
  return res.data;
}

async function adaptSubmit(
  payload: { preview_token: string; confirm: true },
  options: { idempotencyKey: string; signal?: AbortSignal },
): Promise<{ task_id: string; accepted: boolean }> {
  const res = await submitTask({
    previewToken: payload.preview_token,
    idempotencyKey: options.idempotencyKey,
    confirm: true,
    context: { csrfToken: auth.csrfToken.value ?? '', signal: options.signal },
  });
  return res.data;
}
async function adaptLoadTasks(
  cursor: string | null,
  signal?: AbortSignal,
): Promise<TaskPage<TaskItem>> {
  const res = await listTasks(cursor, { csrfToken: auth.csrfToken.value ?? '', signal });
  return res.data;
}
async function adaptLoadTask(
  taskId: string,
  signal?: AbortSignal,
): Promise<TaskItem> {
  const res = await getTask(taskId, { csrfToken: auth.csrfToken.value ?? '', signal });
  return res.data;
}
async function adaptLoadChildren(
  taskId: string,
  cursor: string | null,
  signal?: AbortSignal,
): Promise<TaskPage<SubTaskItem>> {
  const res = await getTaskChildren(taskId, cursor, { csrfToken: auth.csrfToken.value ?? '', signal });
  return res.data;
}
// cancel / time-review / release-lock 三个适配器：注入 context、映射 payload，await 后返回 void 或 res.data；
// adaptTimeReview 签名引用 TimeReviewEvidenceKind，已由上方 import type 提供
async function adaptCancel(
  payload: { taskId: string; expectedRevision: string; confirm: true },
  signal?: AbortSignal,
): Promise<void> {
  await cancelTask({ ...payload, context: { csrfToken: auth.csrfToken.value ?? '', signal } });
}
async function adaptTimeReview(
  payload: { taskId: string; expectedRevision: string; decision: 'resume_original_deadline' | 'expire'; evidence: TimeReviewEvidenceKind },
  signal?: AbortSignal,
): Promise<TaskItem> {
  const res = await reviewHeldTime({ ...payload, context: { csrfToken: auth.csrfToken.value ?? '', signal } });
  return res.data;
}
async function adaptReleaseLock(
  payload: { subtaskId: string; expected_revision: string; acknowledge_unknown: true; reason: string },
  signal?: AbortSignal,
): Promise<void> {
  await releaseUnknownLock({
    subTaskId: payload.subtaskId,
    expectedRevision: payload.expected_revision,
    acknowledgeUnknown: payload.acknowledge_unknown,
    reason: payload.reason,
    context: { csrfToken: auth.csrfToken.value ?? '', signal },
  });
}
```

- 各适配器统一执行：①注入 `{ csrfToken: auth.csrfToken.value ?? '', signal }`（`signal` 允许为 `undefined`）；②映射裸 DTO ↔ 参数接口（`preview_token` → `previewToken`、`expected_revision` → `expectedRevision`、`acknowledge_unknown` → `acknowledgeUnknown`，提交方向组装 `SubmitTaskParams`，幂等键规则不变）；③`await` 请求函数后仅将 `res.data`（解包信封）**原样**交给视图 callback，不得重命名字段（提交方向保留 `task_id`）；④void 语义 callback 的 `await` 完成后适配器才返回。

静态核验（仅说明，不运行测试；可由后续单测补充断言）：① `adaptPreview('reboot', ['d-1'])` 不传 `signal` 调用合法，此时 `context.signal` 为 `undefined`；② `adaptSubmit` 的 `options.idempotencyKey` 进入 `SubmitTaskParams.idempotencyKey`、`options.signal` 进入 `context.signal`；③ `adaptSubmit` 返回值仍含 `task_id` 与 `accepted`（`res.data` 原样返回，无 `taskId` 重命名）；④ `adaptLoadTasks('cursor-1')` / `adaptLoadTask('t-1')` / `adaptLoadChildren('t-1', 'cursor-1')` 不传 `signal` 调用合法（`signal` 为额外可选参数，视图按自身签名调用不会缺省报错），context 已由适配器注入（`csrfToken: auth.csrfToken.value ?? ''`）；⑤ 三者返回值均为解包 `ApiResponse<T>` 后的 `res.data`，视图侧拿到裸 `{ items, next_cursor }` / `TaskItem`；⑥ 分页 `cursor` 原样进入 `listTasks` / `getTaskChildren` 请求参数，适配器不重写、不丢弃 `next_cursor`。

| callback prop | 适配器函数 | 调用的请求函数 |
|---|---|---|
| `onPreview` | `adaptPreview` | `createTaskPreview` |
| `onSubmit` | `adaptSubmit` | `submitTask` |
| `onLoadTasks` | `adaptLoadTasks` | `listTasks` |
| `onLoadTask` | `adaptLoadTask` | `getTask` |
| `onLoadChildren` | `adaptLoadChildren` | `getTaskChildren` |
| `onCancel` | `adaptCancel` | `cancelTask` |
| `onTimeReview` | `adaptTimeReview` | `reviewHeldTime` |
| `onReleaseLock` | `adaptReleaseLock` | `releaseUnknownLock` |

未来核验（仅说明，不运行测试；可由后续单测补充断言）：`adaptCancel` / `adaptTimeReview` / `adaptReleaseLock` 的 payload 映射与视图签名一致（`adaptReleaseLock` 将 snake_case 的 `expected_revision` / `acknowledge_unknown` 与 `subtaskId` 映射为请求侧 camelCase 的 `expectedRevision` / `acknowledgeUnknown` / `subTaskId`，其余字段不重命名）；三者均注入 `{ csrfToken: auth.csrfToken.value ?? '', signal }`（`signal` 为可选参数，绝不必传）；void 语义的 `adaptCancel` / `adaptReleaseLock` `await` 完成后才返回且返回 void，不把 `ApiResponse` 信封交给视图；`adaptTimeReview` 返回解包后的 `res.data`（`TaskItem`）。

`<template>`（新增部分）：

```vue
<!-- 导航栏：在既有导航按钮旁追加任务入口（导航统一走既有 navigateTo，不存在 navigate） -->
<button type="button" @click="navigateTo({ name: 'tasks' })">
  {{ t('nav.tasks') }}
</button>

<!-- 既有 DevicesView 分支：仅追加重启回调，其余绑定不变 -->
<DevicesView
  ...既有绑定
  @reboot-device="handleRebootDevice"
/>

<!-- 新分支：任务列表（6 个请求回调经适配器绑定，见「callback 适配器层」小节） -->
<TasksView
  v-else-if="route.name === 'tasks'"
  :devices="devicesList"
  :pending-device-id="tasksPendingDevice"
  :csrf-token="auth.csrfToken.value ?? ''"
  @open-task="(id) => navigateTo({ name: 'task-detail', params: { id } })"
  :on-preview="adaptPreview"
  :on-submit="adaptSubmit"
  :on-cancel="adaptCancel"
  :on-load-tasks="adaptLoadTasks"
/>

<!-- 新分支：任务详情（6 个请求回调经适配器绑定，见「callback 适配器层」小节） -->
<TaskDetailView
  v-else-if="route.name === 'task-detail'"
  :task-id="route.params.id"
  :is-admin="auth.user.value?.is_admin === true"
  :device-permissions="taskPermissionByDevice"
  :on-load-task="adaptLoadTask"
  :on-load-children="adaptLoadChildren"
  :on-cancel="adaptCancel"
  :on-time-review="adaptTimeReview"
  :on-release-lock="adaptReleaseLock"
/>
```

- [ ] **Step 8: 运行并确认通过（App GREEN）**

运行：`npm --prefix apps/controller-web test -- --run src/App.test.ts src/router.test.ts`

预期：PASS。

- [ ] **Step 9: 提交**

```bash
git add apps/controller-web/src/router.ts apps/controller-web/src/router.test.ts apps/controller-web/src/App.vue apps/controller-web/src/App.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 接线设备重启入口、任务路由与真实任务请求路径"
```

---

### Task 11: 真实浏览器 a11y 验收门禁（手工清单，精简未安装工具候选死代码）

**Files:**
- Create: `apps/controller-web/tests/browser/README.md`
- 禁止：新增 Playwright/axe 依赖、修改 `package.json`/`package-lock.json`、创建引入未安装 Playwright/axe-core 的候选死代码文件、修改 CI、在本任务中运行 `npx playwright`。

**Interfaces:**
- Consumes: 已运行的开发服务器（`apps/controller-web` 的 Vite dev server，由执行者按 `apps/controller-web/DEV-PROXY.md` 手工启动；**不得**由测试自动启动新服务）、可登录的控制器实例。若后端任务路由尚不存在，必须真实后端已联通或明确接入非生产 dev fixture 方可执行业务清单；未联通时只静态注明，不得伪造真实验收。
- Produces（手工门禁清单）：
  - 手工门禁清单（产出人工记录，不包含未安装依赖的死代码）。

- [ ] **Step 1: 编写手工门禁清单**

`apps/controller-web/tests/browser/README.md` 必须包含以下内容（逐条可勾选、可记录）：

```markdown
# Task 4 真实浏览器验收门禁

## 依赖与执行边界
- 仓库当前未安装 `@playwright/test` 与 `@axe-core/playwright`，不保留引入未安装依赖的候选死代码。
- jsdom 组件测试（Vitest）不能替代真实浏览器验证：jsdom 无真实布局、无真实 Tab 顺序、无 axe 规则引擎。
- **缺后端时只作静态注明**；真实浏览器预览/释放等业务清单必须在后端已联通（或明确接入非生产 dev fixture）后才可执行，不得在组件任务强制网络访问或伪造真实验收。

## 前置
1. 后端按 `crates/rsetup-controller` 启动；若任务路由未注册，`/api/v1/tasks*` 返回 404 时仅静态注明。若需验证交互，需配置好受控 dev fixture。
2. 前端 `apps/controller-web` 开发服务器按 `DEV-PROXY.md` 启动，同源代理 `/api/v1`。
3. 使用管理员账号与普通账号；若测试 unknown 释放锁需具备相应 fixture。

## 手工检查项（逐项记录"通过/失败 + 截图或录屏文件名 + 浏览器与版本"）
- [ ] A1 路由：地址栏直接输入 `#/tasks`、`#/tasks?device=<hex64>`、`#/tasks/<uuid>` 均正确渲染，刷新后 hash 不变；畸形 `#/tasks/<非uuid>` 落回 `#/tasks` 且不产生后退陷阱。
- [ ] B1 幂等：勾选设备 → 生成预览 → 打开批量确认 → 在 DevTools Network 中确认写请求中**只有 `POST /api/v1/tasks` 携带 `Idempotency-Key`（规范小写 UUIDv4）**、body 不含该字段；断开网络后点重试，第二次请求的 `Idempotency-Key` 与第一次**相同**。
- [ ] B2 重置：重新生成预览（preview token 变化）后，确认勾选被清空，下一次提交的 `Idempotency-Key` 与上一次**不同**。
- [ ] B3 防双击：提交在途时确认按钮为 disabled，连点不产生第二个 `POST /api/v1/tasks`。
- [ ] C1 授权子集：`GET /api/v1/tasks` 返回 `view_scope: "authorized_subset"` 时界面显示"仅显示当前已授权的目标"，无任何隐藏数量；返回其它/缺失值时显示"授权范围未知"，绝不显示"全部"。
- [ ] C2 状态真实性：`unknown` 行显示"结果未确认"，页面无"失败/自动重试"诱导；`accepted` 行不显示为成功。
- [ ] D1 unknown 释放：普通账号看不到"释放设备占用"；管理员点击后出现 `role="dialog"` + `aria-modal="true"` 弹框，正文含"不撤回执行"警示；Esc 关闭并回到触发按钮；取消不产生任何 POST。
- [ ] D2 释放确认：reason 为空或未勾选风险时确认按钮 disabled；填写后 POST body 恰为 `{expected_revision, acknowledge_unknown:true, reason}`；成功后页面通过 GET 重取子任务。
- [ ] E1 held 核验：管理员看到**恰好两个**决策按钮（按原期限恢复 / 终止为已过期），无任何"延长期限"控件；非管理员看不到决策按钮但能看到核验说明与"以服务端结果为准"字样；无 held 时显示"无需核验"。
- [ ] F1 键盘与视口（含 Focus Trap 与 Esc 源焦点恢复实测要点）：仅用 Tab/Shift+Tab/Enter/Esc/Space 可完成预览→确认→提交与释放锁全流程；弹框打开时 **Focus Trap 循环**——在最后一个可聚焦元素上按 Tab 焦点回到第一个、在第一个上按 Shift+Tab 回到最后一个，循环中跳过 `disabled`、`type="hidden"`、`tabindex="-1"` 元素（候选仅按原生可聚焦属性筛选，无 `offsetParent` 判定）；弹框打开且挂载即 `open=true` 的初始聚焦正常；**Esc 关闭后焦点自动恢复到打开前的源触发按钮**（触发元素已被移除时不报错、焦点不困在 dialog 内）；320×640 视口无横向滚动条。
- [ ] F2 语言切换：切到 English 再切回，设备勾选与在途幂等键不重置，且**零**写请求（Network 中 POST 计数为 0）。
- [ ] G1 网络证据：DevTools Network 记录的写请求中**只有 `POST /api/v1/tasks` 携带 `Idempotency-Key`**；其它写请求仅携带 CSRF；任何其它未列入白名单的任务路径出现即判失败。
- [ ] G2 缺后端声明：若后端未连通且未接入 dev fixture，仅静态注明后端未实现，禁止伪造真实验收通过证据。

## 记录格式
| 项目 | 结论 | 证据文件 | 浏览器/版本 | 时间 | 备注 |
| --- | --- | --- | --- | --- | --- |
| A1 | | | | | |
```

- [ ] **Step 2: 确认测试套件正常**

运行：
```bash
npm --prefix apps/controller-web test -- --run
```
预期：全绿，无外部依赖错误。

- [ ] **Step 3: 记录门禁状态（不声称已验收）**

执行报告必须写：
```
Task 11 状态：手工清单=<已创建，未执行/已执行（注明执行人与浏览器）>；未安装依赖=<无死代码引入>；
真实浏览器验收结论=<未通过|部分通过|通过>，若后端未连通则静态注明阻断。jsdom 结果不作为浏览器验收证据。
```

- [ ] **Step 4: 提交**

```bash
git add apps/controller-web/tests/browser/README.md
git diff --cached --name-only
git commit -m "docs(controller-web): 新增 Task 4 手工浏览器无障碍验收清单并移除未使用示例"
```

---

### Task 12: 全量回归、类型检查、真实请求白名单复核与后端不一致点报告

**Files:**
- 只读复核，不改文件（发现缺陷时回到对应任务修正并单独提交）。

**Interfaces:**
- Consumes: Task 1..11 的全部产物。
- Produces: 一份执行报告，含真实请求白名单证据、fixture GREEN 清单、未实现项清单、后端接口不一致点。

- [ ] **Step 1: 全量测试与类型检查**

运行：
```bash
npm --prefix apps/controller-web test -- --run
npm --prefix apps/controller-web run typecheck
```
预期：测试全绿，`vue-tsc --noEmit` 与 `tsc --noEmit -p tsconfig.node.json` 均无错误。任何失败必须修回后重跑，不得跳过。

- [ ] **Step 2: 真实请求白名单复核（静态 + 动态）**

静态：
```bash
grep -rn "'/api/v1\|\`/api/v1\|/api/v1/" apps/controller-web/src | grep -v "\.test\." || true
grep -rn "fetch(" apps/controller-web/src | grep -v "\.test\." 
```
预期：业务代码中除 `api.ts` 内的 `fetch` 外没有其它 `fetch` 调用；任务相关路径全部经 `task-requests.ts` 的 `TASK_API_PATHS` 生成。

动态：在 Vitest 中临时（或作为常驻用例）断言 `fetch` 收到的首个参数集合恰好是白名单允许的 8 条路径前缀：
```ts
const allowed = ['/api/v1/task-previews', '/api/v1/tasks', '/api/v1/subtasks/']
const urls = fetcher.mock.calls.map(([u]) => String(u))
for (const url of urls) expect(allowed.some((prefix) => url.startsWith(prefix))).toBe(true)
```

- [ ] **Step 3: 核对没有越界改动**

```bash
git status --short
git log --oneline -12
git diff --name-only dev-aghost...HEAD 2>/dev/null || git diff --name-only HEAD~11..HEAD
```
预期：改动只落在 `apps/controller-web/src/**`、`apps/controller-web/tests/**`、`docs/superpowers/plans/**`；**没有** `crates/rsetup-controller/src/{time,devices,polling,auth,authz}`、`crates/rsetup-runtime`、`crates/rsetup-identity`、`crates/rsetup-transport`、`crates/rsetup-protocol` 的改动。出现即阻断并报告。

- [ ] **Step 4: 后端接口不一致点清单（逐条核对，写进报告）**

执行者必须逐条复核并在报告中确认或修正（本计划已知的偏差如下，实施中若与代码不符，以代码为准并记录）：

| # | 不一致点 | 证据 | 本计划采取的立场 |
| --- | --- | --- | --- |
| 1 | 上位计划 Task 4 的 `onReleaseLock` payload 用 `{ subtaskId, expected_revision, acknowledge_unknown, reason }`（camel/snake 混用） | `docs/superpowers/plans/2026-10-07-controller-v1-remaining-web-tdd.md:1216-1221` | 保持该形状不变（它是前端回调契约），HTTP body 由 `task-requests.ts` 明确构造为全 snake_case（`expected_revision`、`acknowledge_unknown`、`reason`） |
| 2 | 上位计划 `TasksView` props 只声明 `previewTargets/previewToken/subtasks/onSubmit/onReleaseLock/isAdmin`，无预览请求入口，也无设备清单来源 | 同上 `:1086-1093` | 本计划为 `TasksView` 补齐 `devices`、`pendingDeviceId`、`onPreview`、`onLoadTasks`、`csrfToken`；子任务展示与 unknown 释放锁移入 `TaskDetailView`（`subtasks`/`onReleaseLock`/`isAdmin` 不再是 `TasksView` 契约），`onSubmit`/`onCancel` 语义保留 |
| 3 | 上位计划的 `ts` 提示块里 `taskStateLabel(_state: string, ...)` 与实现签名 `SubTaskState` 不一致 | 同上 `:1077` | 以上位计划 **Step 3 实现**（`SubTaskState`）为准；本计划不改该签名 |
| 4 | 上位计划 `previewTargets: Array<{ device_id: string; eligible: boolean }>` 缺 `reason`，但测试断言消费 `task.previewReason.*` | 同上 `:1128` 与 §Global Constraints 5 | 本计划用 `task-ops.ts` 的 `TaskPreviewTarget`（含可选 `reason`）作为唯一来源；组件不再接收 `previewTargets` 形状的裸数组 |
| 5 | 上位计划要求任务响应带 `view_scope:"authorized_subset"`；规格 02 的 §HTTP 表（`:150-157`）未逐个列出该字段，但 §模型表（`:24`）已定义 `MainTask含id、operation、state、outcome、revision、created_time、view_scope与按state分组的counts` | `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md:24` 与 `:150-157`；上位计划 Global Constraints 5 | 前端把 `view_scope` 视为**可选**标记：缺失即按 `task.scope.unknown` 呈现，绝不假设"完整授权"；字段名与语义以 02 §模型表 与 Task 4 设计文档 §3 为准 |
| 6 | 旧 Web 计划 `SubTaskItem` 必需 `main_task_id` 且使用 `evidence_source`（controller/board）与 `pre_boot_id`/`observed_boot_id`，与当前后端 6 字段合同（规格 02 §1:24 “SubTask含id、device_id、state、reason_code、revision及授权可见的result_evidence；不暴露execution_token或token摘要”；Runtime 计划 Task 6 `ChildItem`/`project_task_children` 恰好输出该 6 字段）直接不兼容 | `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md:24,74,154`；Runtime 计划 Task 6（`ChildItem`/`ChildEntry` 接口、`project_task_children` 说明、端点契约 JSON `result_evidence:"boot_transition_observed"`、集成测试断言 `items[0]["result_evidence"] == "boot_transition_observed"`）；父任务裁决要求以 Runtime 计划形状为准 | 本次修订对齐 6 字段合同：`id` 规范 UUIDv4（`INVALID_SUB_TASK_ID`，不放宽为非空 ≤128）、`device_id` hex64、`state` 十态、`reason_code` nullable 已知 ReasonCode、`revision` u64 十进制串、`result_evidence` nullable 六值 snake_case 串（JSON `null`=证据未产生，`'none'` 为枚举串勿与 null 混淆）；`main_task_id`/`pre_boot_id`/`observed_boot_id` 降为 DB 内部列、不升 HTTP 必需；移除旧 `EvidenceSource` 类型与 `INVALID_EVIDENCE_SOURCE`；`lock_released` 保留可选 fail-closed（后端投影不承诺输出，UI 不得据缺字段推危险授权）；`execution_token` 及其摘要永不暴露，守卫对扩展字段忽略不拒绝 |
| 7 | 后端当前**完全没有**这 8 条任务路由（规格已定契约，实现未落地） | `crates/rsetup-controller/src/http_auth.rs:259-268` 只注册 `/healthz`、`/readyz`、`/api/v1/auth/*`；`grep -rn "task-previews\|time-review\|release-lock" crates/` 无命中 | 前端按规格实现请求形状与错误态；**不得**声称后端已实现或联调通过 |
| 8 | 上位计划 Task 4 Step 5 要求"全量测试与类型检查"，但没有后端 404 的证据要求，也没有浏览器门禁归属 | 同上 `:1424-1429` | 本计划把"缺后端"改为静态注明 + 组件 stub 错误态单测（Task 8/9 Step 6）与手工浏览器门禁（Task 11）；**stub 404 不得称为真实 404 证据**，避免 fixture GREEN 被当成交付证据 |
| 9 | 前端 branch header 在独立已审提交 `c4fe870`（`controller-v1-web-idempotency`），定义了 `RequestOptions.idempotencyKey` | `git log -1 c4fe870`；`grep -n idempotencyKey apps/controller-web/src/api.ts` | 计划列出此依赖，执行前先将该已审切片导入目标集成分支，不在此空等阻断；且严格限制写请求中只有 `POST /tasks` 携带 `Idempotency-Key`，其它写请求仅带 CSRF |
| 10 | 规格 02 §模型表（`:24`）未列主任务 `actions` 字段，但取消入口固定合同（用户批准）要求后端按当前身份、目标设备 `device.reboot` 与合格 `queued`/`held` 子任务计算并返回 `actions.can_cancel` | `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md:24`；01 §4:42 与 §6:71、04 §8:150、设计文档 §3（逐端点重验 + 取消 CAS）与 §5 | 前端把 `actions?: { can_cancel: boolean }` 视为可选字段：缺失时只读任务合法且取消入口 fail-closed；存在时形状必须为布尔（否则 `INVALID_TASK_ACTIONS`）；入口唯一依据 `canRequestCancel(task)`（scope 须为合法 `full`/`authorized_subset`），前端不借 children 分页 / `task.read` / 主任务状态猜 reboot；可请求不保成功，服务端事务 CAS 与重验为最终准绳 |
| 11 | 规格 02 §模型表（`:24`）只说 MainTask 含 `created_time` 与按 state 分组的 counts，未给出 `created_time` 类型；真实 `apps/controller-web/src/types.ts` 无完整 `TimeEvidence` 类型导出；时间证据 JSON 形状以 `crates/rsetup-controller/src/time/evidence.rs` 为准（`sample_age_ms` 十进制字符串\|null、`offset_ms` 数\|null、`uncertainty_ms` 非负数\|null，仅 PartialEq 不派生 Eq），语义以 05 §3（"未知不是 0"）与 04 §4（计数按各 state 返回）为准 | `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md:15,24`；`docs/superpowers/specs/2026-09-23-controller-v1-05-runtime-operations.md:32-47`；`docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md:74-87`；`apps/controller-web/src/types.ts`（无 `TimeEvidence` 导出）；`crates/rsetup-controller/src/time/evidence.rs` | `task-ops.ts` 本地定义 `TaskTimeEvidence`/`TaskCounts`/`TASK_COUNT_KEYS` 与新错误码 `INVALID_TIME_EVIDENCE`/`INVALID_TASK_COUNTS`（不改 `types.ts`、不改 `evidence.rs`）；`created_time` 不再接受旧 string 形状；`counts` 必含十键全在且为 u64 十进制串；响应扩展字段忽略不拒绝；本次仅动响应 DTO，未来请求 schema 不新增字段、不新增权限点 |
| 12 | Runtime 计划 `EvidenceKind` 六值 wire 形状（对照 `SubTaskState`/`ReasonCode` 已显式钉 `snake_case`/`SCREAMING_SNAKE_CASE`）——原记录"serde rename 属性未显式钉"，现已由 producer 侧补齐纸面定义 | 主代理已实核（commit 1de 起）：Runtime 计划 Task 2 Step 1 单一定义 `pub enum EvidenceKind` + `#[serde(rename_all = "snake_case")]`，六值 wire 严格 `attempted`/`boot_transition_observed`/`execution_rejected`/`os_error`/`journal_lost`/`none`，且钉死 `Option::<EvidenceKind>::None` 为 JSON `null`（证据未产生）与 `Some(EvidenceKind::None)` 串 `"none"` 不同义（Task 2 Step 1 enum 段 + doc 注释 + 序列化用例 `result_evidence_kind_serializes_six_snake_case_values_and_distinguishes_null`）；"类型一致性核对"段（commit 81d 后）确认 `EvidenceKind` 唯一定义在 Task 2 Step 1、Web 侧不重定义，Task 6 `ChildEntry.result_evidence: Option<EvidenceKind>` 直接复用不重定义；但该切片 Runtime 已注明"四条命令均未运行 / 纯函数测试尚未运行，未实际执行前不得记 PASS" | **producer 纸面已定义，未编译/运行，跨线合同实测未验（不继承全稿批准）**：Web 守卫维持按 6 个 snake_case 字符串接受（`attempted`/`boot_transition_observed`/`execution_rejected`/`os_error`/`journal_lost`/`none`），其余形状（PascalCase/对象/数/布尔/旧来源词表）一律 `INVALID_RESULT_EVIDENCE`；本计划不臆造 `object`/`fullTaskEvidence` wire；Runtime 编译/运行实测前本行保持未验状态，实测不符时两侧同步 |
| 13 | Web time-review `evidence` 原为自由文本（Task 9 旧切片 textarea），而 Runtime 计划 Task 6 `TimeReviewTaskRequest.evidence` 为 `TimeReviewEvidenceKind` 三值枚举（Runtime 计划 Task 4 `#[serde(rename_all = "snake_case")]`，wire 严格 `reference_deadline_proven`/`reference_deadline_expired`/`inconclusive`）；02 §HTTP:156 只写 `evidence` 未钉词表 | Runtime 计划 Task 4 Step 1 `TimeReviewEvidenceKind` enum 段（含 `time_review_never_extends_deadline` 用例）与 Task 6 `TimeReviewTaskRequest`（doc：`evidence` 复用 Task 4 `TimeReviewEvidenceKind`，02 §6 evidence）；`docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md:156`；04 §8:152（held 核验语义：能可信证明剩余寿命才恢复原期限/能可信证明原期限已过期则 expired/无法证明时管理员裁决）；05 §3（TimeEvidence 证据语义） | **请求层纸面对齐、UI 显式三值单选已纸面对齐（本次修订，不继承全稿批准）**：Task 5 请求层定义 `TimeReviewEvidenceKind` 三串联合 + `TIME_REVIEW_EVIDENCE_KINDS` + `isTimeReviewEvidenceKind` 谓词（与 producer 词表 1:1，不重定义语义）；`reviewHeldTime` 在 `post` 前严格校验/narrow，非法（空/自由文本/未知/数字/null）抛 `INVALID_TIME_REVIEW_EVIDENCE` 且 0 网络调用，`body.evidence` 只发批准的三串、不降级映射/猜测；`TimeReviewParams.evidence` 保持 `string` 避免与未完成 Task 9 UI 类型断裂；旧 UI 自由文本如今不能通过请求 guard，UI 后续任务改显式三值选择（不默认 `inconclusive`、不强制任意文字为 `reference_deadline_proven`）；UI 切片前不能称全 Web 完成；本行不改行 12 EvidenceKind 六值 producer 纸面状态，不触行 11 大数/闰秒剩余 |

- [ ] **Step 5: 输出执行报告（不提交任何代码）**

报告模板：
```
Task 12 报告
- 全量测试：<PASS/FAIL，用例数>
- typecheck：<PASS/FAIL>
- 真实请求白名单证据：<用例名 + 观测到的 URL 列表>
- 缺后端静态注明：8 条端点中的 <n> 条在 Rust 树中不存在；仅静态注明，**不称真实 404 证据**
- fixture GREEN 清单：哪些 GREEN 只是注入回调/ fetch stub（逐条列出测试文件与用例名）
- 未覆盖/未验收：真实浏览器 a11y（Task 11 状态）、真实后端联调、真库与线协议、time-review evidence UI 三值选择（Task 9 旧自由文本尚未对齐，见表行 13）；UI 切片前不能称全 Web 完成
- 后端接口不一致点：<逐条确认或修正上表>
- 越界改动检查：<none/列表>
- 结论：Web Task 4 可执行部分=<完成/未完成>；后端依赖=<阻断>；不得表述为"Task 4 完成"
```

- [ ] **Step 6: 提交**

无提交（除非 Step 1-2 发现缺陷，此时回到对应任务按该任务的提交边界单独提交）。

---

## 自我审查（规格覆盖对照）

| 规格/设计要求 | 落在哪个任务 | 验证方式 |
| --- | --- | --- |
| 一次确认生成 CSPRNG UUIDv4，重试复用同 key | Task 4 `startTaskOperation` + Task 8 用例 3 | 纯函数单测 + 组件级 fetch/回调用例 |
| preview token 或清单改变重置确认与 key | Task 4 `previewFingerprint`/`resetTaskOperation` + Task 8 用例 4 | 指纹单测 + 组件重渲染用例 |
| busy 防双击 | Task 4 `TASK_OPERATION_BUSY` + Task 7 `confirmDisabled`/`busy` + Task 8 用例 3 | 抛错断言 + disabled 断言 |
| 幂等键只作为 HTTP 头、不入 body/存储 | Task 5 `submitTask` + Task 1 前置校验 | `post.test.ts`（c4fe870）+ Task 5 用例 |
| 管理员 unknown 释放 + 非空 reason + `acknowledge_unknown:true` | Task 5 守卫 + Task 7 弹框 + Task 7 用例 | 请求体断言 + disabled/emit 断言 |
| 弹框 `role=dialog`/`aria-modal`/Esc/焦点捕获与恢复/取消零 POST | Task 7 | 8 个 a11y 用例 + Task 11 手工门禁 |
| 10 状态与稳定原因码呈现、`unknown` 不冒充失败/成功 | Task 6 | 状态分栏用例（含 accepted≠succeeded） |
| 主任务四态与 outcome 配对（04 §4）；主列表/详情用独立 `mainTaskStateLabel`，不借用子任务 `taskStateLabel`/`task.state.*` | Task 3 守卫 + Task 4 `mainTaskStateLabel` + Task 2 键 + Task 8/9 渲染 | 守卫正反循环用例（含子态误作主态、outcome 错配负例）+ 键独立与四态文案单测 + 组件文案用例 |
| 主任务 `created_time` 为完整 TimeEvidence（02 §模型表:24 + 05 §3），`counts` 为当前授权全集十态 u64 十进制串（04 §4） | Task 3 守卫与本地 `TaskTimeEvidence`/`TaskCounts` 定义 + Task 5/8/9/10 fixture 同步 + Task 8/9 呈现消费 `reference_utc`/`quality` | 守卫正反用例（旧 string、非 UTC/空格、日历非法：非法月/日、非闰年 2 月 29、时分秒超范围、闰秒边界 vs 闰年 2 月 29+纳秒正例、<100 四位年、坏 UUID、非字符串 quality/source、前导零 sample_age、非有限/小数/字符串 offset、负/小数 uncertainty、i64 下界与 >2^53 大整数、缺键/数字/负/前导零/溢出 counts）+ 各 fixture 编译即含完整对象 |
| 子任务 6 字段投影（02 §1:24 + Runtime Task 6 `ChildItem`/`project_task_children`）：`result_evidence` 为授权可见证据**种类**（nullable 六值 snake_case 串），不暴露 execution_token 及其摘要，不把 DB 内部列升为 HTTP 必需 | Task 3 `SubTaskItem`/`ResultEvidenceKind`/`RESULT_EVIDENCE_KINDS`/`INVALID_RESULT_EVIDENCE` + Task 2 键 `task.detail.resultEvidence` + Task 5/6/7/9 fixture 与 `TaskStateList` 模板同步 + Task 12 表行 6/12 | 守卫正反用例（最小 6 字段合法、key 缺失/undefined 拒绝、`null` 合法、六 enum 全合法、非法证据 7 变体拒绝、token 扩展字段忽略不拒绝、id 非 UUIDv4 拒绝、未知 state 按既有守卫关闭）+ Task 6 渲染用例（种类字面值展示、null 不渲染证据行、旧 `evidenceSource` 标签负向断言） |
| held 管理员 time-review 原子两决策、不延期、以服务端结果为准（held 分页根因：入口唯一依据主任务 `counts.held` 当前授权全集——02 主任务 counts 与 children 独立分页，当前页 children 有无 held 不能证明授权全集是否 held；`hasHeld` 按 `isU64DecimalString` 严格验证且 ≠"0"、不转换 Number（u64 全域）；危险按钮另要求当前 `isAdmin === true` 且 `view_scope === 'full'`，缺/坏 counts 或未知 scope fail-closed；`runTimeReview` 与按钮同一 guard，不只禁按钮） | Task 4 决策集合 + Task 9 `hasHeld`/模板/`runTimeReview` | 决策集合单测 + 详情用例（counts.held 门禁 table：full+held=1+当前页无 held 允许 / held=0+当前页有 held 关闭 / 缺键与前导零、小数、负数、非十进制、JSON 数等非法值关闭 / 未知 scope 与 authorized_subset 关闭 / full 非管理员关闭；两决策独立 mount、evidence 必填、expectedRevision 不篡改、服务端返回 held=0 新 revision 后 noHeld/新状态为服务器结果并重取 children） |
| 列表/详情 `authorized_subset`、不显示隐藏数量 | Task 3 `describeAuthorizedSubset` + Task 8/9 | 守卫单测 + 组件文案用例（含 `\d` 负向断言） |
| 取消入口 = 后端 `actions.can_cancel` 唯一依据（身份/`device.reboot`/合格 queued-held 由后端计算；可请求不保成功；`running` 可有可取消目标、`authorized_subset` 下 `completed` 不按主状态关闭；前端不借 children 分页 / `task.read` / 主状态猜 reboot；缺 action / 非法 bool / 未知 scope fail-closed，只读任务兼容缺 action） | Task 3 `canRequestCancel` + Task 9 `cancellable`/`handleCancel` | helper 正反单测（running+true、completed+subset+true、full+true 开；queued false、缺 / 非法 action、未知 / 缺 scope 关）+ 组件矩阵用例（关闭 cases 回调 0、有权调用 spy>0 非恒拒绝假绿、`task.read`/第一页 queued 不覆盖后端 false、RED 正例先失败） |
| 设备入口与路由（`#/tasks`、`#/tasks?device=`、`#/tasks/:id`） | Task 10 | router 正反用例 + App 挂载/入口用例 |
| 真实 API 请求白名单，不发明端点 | Task 5 + Task 12 Step 2 | 白名单常量断言 + 负向正则 + 静态 grep + 动态 URL 断言 |
| 缺后端不得被伪装（stub 404 ≠ 真实 404 证据） | Task 1 Step 3（静态注明）、Task 8/9 Step 6（stub 错误态单测）、Task 12 Step 4 | 静态注明 + 报告模板 |
| 真实浏览器 a11y 验收 gate | Task 11 | 手工清单 + 后端未联通即静态注明、不执行的边界 |
| 不修改 Runtime/Identity/Transport | Global Constraints 3 + Task 12 Step 3 | 提交边界 + `git diff --name-only` 检查 |

**占位扫描结论：** 本计划每个任务都给出确切文件、接口签名、可直接运行的测试代码、RED/GREEN 命令与提交边界；无 `TBD`/`TODO`/"以后实现"字样；所有被引用的类型、函数、测试 id 都在本计划内定义（`task-ops.ts`、`task-presentation.ts` 新增导出、`task-requests.ts`、`TaskStateList.vue`、`TaskModals.vue`、i18n 键）。本次 6 字段对齐新增的 `ResultEvidenceKind`/`RESULT_EVIDENCE_KINDS`/`INVALID_RESULT_EVIDENCE`/`task.detail.resultEvidence` 均在 Task 3（类型/守卫/测试）与 Task 2（双语键）内定义并同步至 Task 5/6/7/9 fixture 与 Task 6 模板，无半截符号；子任务 6 字段合同（Task 3 `SubTaskItem`/`ResultEvidenceKind`）与 Runtime 计划 Task 6 `ChildItem`/`project_task_children` 一致，`EvidenceSource`/`INVALID_EVIDENCE_SOURCE`/`EVIDENCE_SOURCES` 及 `main_task_id`/`pre_boot_id`/`observed_boot_id`/`evidence_source` 的旧**类型/代码**引用已清除（契约正文仅保留为对照说明；`lock_released` 为保留的可选兼容字段，非本次新增）。

## 执行交接

计划已保存到 `docs/superpowers/plans/2026-10-16-controller-v1-four-task4-web-execution.md`。有两种执行方式：
1. 子代理驱动（推荐）：每项派新的子代理，任务间审查，快速迭代（使用 superpowers:subagent-driven-development）。
2. 当前会话执行：使用 superpowers:executing-plans，分批落实并设置检查点。

**执行前必须先满足 Task 1：** 前端 branch header 在独立已审提交 `c4fe870`（分支 `controller-v1-web-idempotency`）中；执行前先将该已审切片导入目标集成分支，导入后重跑 Task 1 确认 `MERGED` 再继续，不在此空等阻断。Task 1/2 与 Task 11 的手工清单不依赖该切片。后端 8 条任务路由联通之前，任何浏览器/联调结论都不得标记为通过；缺后端只作静态注明。

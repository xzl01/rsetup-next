import { fireEvent, render, screen } from '@testing-library/vue'
import { nextTick } from 'vue'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'

import App from './App.vue'
import { createAuth } from './auth'
import { createEventIntegration, type EventIntegration } from './event-integration'
import { baseTaskTimeEvidence, zeroTaskCounts, type TaskItem } from './task-ops'
import type { EventSourceLike } from './events'

/**
 * SSE App 第二轮整改回归（F2 / F4 / 失效 episode 预算链路）。
 *
 * 与 `App.events.races.test.ts` 的分工相同：本文件跑**真实 App + 真实 auth store + 真实
 * events.ts 客户端 + 真实接线层**，只替身 fetch 与 EventSource。deferred fetch 桩**故意忽略
 * AbortSignal**：代际与所有权必须自己成立，正确性不能外包给浏览器是否真的中止请求。
 *
 * - F2：任务首屏投影的 fallback/retry owner 必须唯一且在 SSE 接管当刻移交——接管后即使首快照
 *   从未成功，App 也不得把责任退回已作废的 TasksView 自有 GET（两边互等 = 列表凭空消失、
 *   重试点了不发请求）。
 * - F4：快照来源（SnapshotSource.fetch）的错误副作用只能由**当前在途 ticket** 提交：
 *   旧轮被 abort 后迟到落地的 500 不得把错误灌进已 realtime 的 DOM。
 * - 预算链路：`permissions.changed` → 真实 `auth.refresh()` 返回**新 token/epoch** 的自动重建
 *   仍共用失效 episode 的 ≤3 余额，身份三元组变化不得回满预算；
 *   合法的外部认证轮换本身不是失效 episode，不受此限（另一文件已回归）。
 *
 * 服务端事件发布方未实现，本文件只是接线层回归，不构成 AT-18 端到端或真实浏览器验收。
 */

const USER_ID = '01234567-89ab-4cde-8f01-234567890abc'
const RID = '123e4567-e89b-42d3-a456-426614174000'
const DEVICE_A = 'a'.repeat(64)

const ME_DATA = {
  user: { id: USER_ID, username: 'admin', active: true, is_admin: true, must_change_password: false, revision: '1' },
  csrf_token: 'mem-token',
  authz_epoch: '7',
}

type Listener = (event: unknown) => void

class FakeEventSource {
  static instances: FakeEventSource[] = []
  readonly listeners = new Map<string, Listener[]>()
  readonly url: string
  closed = false

  constructor(url: string) {
    this.url = url
    FakeEventSource.instances.push(this)
  }

  addEventListener(type: string, listener: Listener): void {
    const existing = this.listeners.get(type)
    if (existing === undefined) this.listeners.set(type, [listener])
    else existing.push(listener)
  }

  emit(type: string, event: unknown = {}): void {
    if (this.closed) throw new Error('closed EventSource must not deliver events')
    for (const listener of this.listeners.get(type) ?? []) listener(event)
  }

  emitData(type: string, data: unknown): void {
    this.emit(type, { data: JSON.stringify(data) })
  }

  close(): void {
    this.closed = true
  }

  static get newest(): FakeEventSource {
    const instance = FakeEventSource.instances[FakeEventSource.instances.length - 1]
    if (instance === undefined) throw new Error('no EventSource instance created')
    return instance
  }

  static get liveCount(): number {
    return FakeEventSource.instances.filter((instance) => !instance.closed).length
  }
}

// ── deferred fetch 替身：可任意顺序落地、可 reject、忽略 AbortSignal ─────────────────

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason: unknown) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej })
  return { promise, resolve, reject }
}

type RequestKind = 'devices' | 'tasks'

interface RecordedRequest {
  url: string
  kind: RequestKind
  settled: boolean
  response: Deferred<Response>
}

interface Harness {
  requests: RecordedRequest[]
  taskRequests: () => number
  devicesRequests: () => number
  lastTasks: () => RecordedRequest
  lastDevices: () => RecordedRequest
  rejectAllPendingTasks: () => void
  resolveWith: (request: RecordedRequest, items: unknown[], nextCursor?: string | null) => void
  rejectWith: (request: RecordedRequest, status?: number) => void
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

const DEVICE_PAGE_AUTO = { items: [], next_cursor: null }
const TASK_PAGE_AUTO = { items: [], next_cursor: null }

interface HarnessOptions {
  autoDevices?: unknown
  autoTasks?: unknown
  /** 按调用序号（1 起）决定 /auth/me 的响应：预算链路用它轮换 csrf_token。 */
  me?: (callIndex: number) => { csrfToken: string }
}

function makeHarness(options: HarnessOptions = {}): Harness {
  const requests: RecordedRequest[] = []
  let meIndex = 0

  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL): Promise<Response> => {
    const url = String(input)
    if (url.includes('/auth/me')) {
      meIndex += 1
      const rotation = options.me?.(meIndex)
      const body = rotation === undefined
        ? ME_DATA
        : { ...ME_DATA, csrf_token: rotation.csrfToken }
      return Promise.resolve(jsonResponse({ data: body, request_id: RID }))
    }
    if (url.includes('/auth/logout')) {
      return Promise.resolve(jsonResponse({ data: { logged_out: true }, request_id: RID }))
    }
    const kind: RequestKind | null = url.includes('/api/v1/devices') ? 'devices'
      : url.includes('/api/v1/tasks') ? 'tasks'
        : null
    if (kind === null) throw new TypeError(`unexpected fetch ${url}`)
    const auto = kind === 'devices' ? options.autoDevices : options.autoTasks
    if (auto !== undefined) return Promise.resolve(jsonResponse({ data: auto, request_id: RID }))
    const request: RecordedRequest = {
      url,
      kind,
      settled: false,
      response: deferred<Response>(),
    }
    requests.push(request)
    return request.response.promise
  }))

  const of = (kind: RequestKind): RecordedRequest[] =>
    requests.filter((request) => request.kind === kind)

  return {
    requests,
    taskRequests: () => of('tasks').length,
    devicesRequests: () => of('devices').length,
    lastTasks: () => {
      const list = of('tasks')
      const request = list[list.length - 1]
      if (request === undefined) throw new Error('no tasks request recorded')
      return request
    },
    lastDevices: () => {
      const list = of('devices')
      const request = list[list.length - 1]
      if (request === undefined) throw new Error('no devices request recorded')
      return request
    },
    resolveWith: (request, items, nextCursor = null) => {
      request.settled = true
      request.response.resolve(jsonResponse({ data: { items, next_cursor: nextCursor }, request_id: RID }))
    },
    rejectWith: (request, status = 500) => {
      request.settled = true
      request.response.resolve(jsonResponse(
        { error: { code: 'INTERNAL', message_key: 'errors.generic' }, request_id: RID },
        status,
      ))
    },
    rejectAllPendingTasks: () => {
      for (const request of requests.filter((item) => item.kind === 'tasks' && !item.settled)) {
        request.settled = true
        // AUTH_REQUIRED + 401：events.ts 识别为认证失效，当刻整体 shutdown（不等其他来源 settle）
        request.response.resolve(jsonResponse(
          { error: { code: 'AUTH_REQUIRED', message_key: 'auth.error.authRequired' }, request_id: RID },
          401,
        ))
      }
    },
  }
}

// ── 夹具 ──────────────────────────────────────────────────────────────────────

function deviceItem(id: string, displayName: string): Record<string, unknown> {
  return { device_id: id, display_name: displayName, effective_permissions: ['device.read', 'device.reboot'] }
}

function taskItem(id: string, revision: string): TaskItem {
  return {
    id, operation: 'reboot', state: 'queued', outcome: null, revision,
    created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(), view_scope: 'authorized_subset',
  }
}

const TASK_T1 = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const TASK_T3 = '1b3d5f7a-9c1e-4d2f-8a6b-0c4e6a8b0d2f'

async function settle(rounds = 5): Promise<void> {
  for (let index = 0; index < rounds; index += 1) {
    await new Promise<void>((resolve) => { setTimeout(resolve, 0) })
    await nextTick()
  }
}

async function until(condition: () => boolean, message: string): Promise<void> {
  await vi.waitFor(
    () => { if (!condition()) throw new Error(message) },
    { timeout: 3000, interval: 1 },
  )
}

let mountedUnmount: (() => void) | null = null
function renderApp(): void {
  const { unmount } = render(App)
  mountedUnmount = unmount
}

const indicator = (): string => screen.getByTestId('event-connection-state').textContent ?? ''
const taskRows = (): string => screen.queryByRole('table')?.textContent ?? ''
const hasProjectionError = (): boolean => screen.queryAllByText('无法加载内容').length > 0

beforeEach(() => {
  localStorage.clear()
  document.documentElement.removeAttribute('lang')
  window.location.hash = '#/devices'
  FakeEventSource.instances = []
  vi.stubGlobal('EventSource', FakeEventSource)
})

afterEach(() => {
  mountedUnmount?.()
  mountedUnmount = null
  vi.unstubAllGlobals()
  localStorage.clear()
  document.documentElement.removeAttribute('lang')
  window.location.hash = ''
})

// 真实 auth store + 真实 events.ts + 真实接线层；只有 fetch 与 EventSource 是替身。
// `auth.refresh()` 第一步同步置 checking：此时 eligibleIdentity 为 null，若接线层只 watch
// 有资格身份，则 checking→null 后的终态（同样 null）永不触发关闭——旧 ES 保持
// open/active/realtime、在途快照 GET 不 abort、授权 buffer 不清。
interface Stub {
  meResponses: Array<{ status: number; body: unknown }>
  /** true = 每次 /auth/me 都返回新的 csrf_token（模拟外部合法认证轮换）。 */
  rotateToken?: boolean
}

function makeAuthStub(stub: Stub): { meCalls: () => number } {
  let index = 0
  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL): Promise<Response> => {
    const url = String(input)
    if (!url.includes('/auth/me')) throw new TypeError(`unexpected fetch ${url}`)
    if (stub.rotateToken === true) {
      index += 1
      return Promise.resolve(jsonResponse({
        data: meBody({ csrf_token: `mem-token-${index}` }),
        request_id: RID,
      }))
    }
    const spec = stub.meResponses[Math.min(index, stub.meResponses.length - 1)]!
    index += 1
    if (spec.status >= 400) {
      return Promise.resolve(jsonResponse(
        { error: { code: 'AUTH_REQUIRED', message_key: 'auth.error.authRequired' }, request_id: RID },
        spec.status,
      ))
    }
    return Promise.resolve(jsonResponse({ data: spec.body, request_id: RID }))
  }))
  return { meCalls: () => index }
}

function makeRotatingAuthStub(): void {
  makeAuthStub({ meResponses: [], rotateToken: true })
}

function meBody(overrides: Partial<{ csrf_token: string; must_change_password: boolean }> = {}) {
  return {
    user: { ...ME_DATA.user, must_change_password: overrides.must_change_password ?? false },
    csrf_token: overrides.csrf_token ?? 'mem-token',
    authz_epoch: '7',
  }
}

function mountIntegration(): {
  auth: ReturnType<typeof createAuth>
  integration: EventIntegration
  snapshotSignals: AbortSignal[]
  snapshot: Deferred<Array<Record<string, unknown>>>
} {
  const auth = createAuth()
  const snapshot = deferred<Array<Record<string, unknown>>>()
  const snapshotSignals: AbortSignal[] = []
  const integration = createEventIntegration({
    auth,
    snapshots: [{
      kind: 'device',
      fetch: ({ signal }) => {
        snapshotSignals.push(signal)
        return snapshot.promise
      },
    }],
    eventSourceFactory: (url) => new FakeEventSource(url) as unknown as EventSourceLike,
    onSnapshot: () => { /* 快照提交与否由 events.ts 代际门禁决定 */ },
  })
  return { auth, integration, snapshotSignals, snapshot }
}

describe('F2：任务首屏 owner 在接管当刻移交，null 投影也有唯一 fallback/retry owner', () => {
  test('接管后首快照未成功即断线：App 必须立即兜底 GET（不再是 2→2 死区）', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    // connecting 期：TasksView 自有首屏 GET（请求 1）
    await until(() => harness.taskRequests() >= 1, '组件自有首屏 GET')
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    // 接管当刻：旧 GET 作废 + 快照 GET（请求 2）在途，tasksSnapshot 仍 null
    await settle()
    expect(harness.taskRequests()).toBe(2)
    const snapshot = harness.lastTasks()

    // 首快照从未成功就断线：owner 判据若是「快照为 null 就退回 TasksView」，两边互等，
    // 请求数永远停在 2（缺陷），列表凭空消失且无从恢复。
    FakeEventSource.newest.emit('error', {})
    await until(() => indicator().includes('非实时'), '断线指示非实时')
    expect(snapshot.settled).toBe(false)
    await until(() => harness.taskRequests() >= 3, '接管者断线后 App 兜底 GET')

    // 兜底成功即恢复列表，且诚实保持非实时
    harness.resolveWith(harness.lastTasks(), [taskItem(TASK_T1, '3')], null)
    await until(() => taskRows().includes(TASK_T1), '兜底 GET 渲染任务')
    expect(indicator()).toContain('非实时')
  })

  test('首快照 500：重试点必须真的发出新请求（App 是 null 投影的 retry owner）', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    const snapshot = harness.lastTasks()
    harness.rejectWith(snapshot, 500)
    await until(() => hasProjectionError(), '首快照失败可见')
    expect(harness.taskRequests()).toBe(2)

    await fireEvent.click(screen.getByRole('button', { name: '重试' }))
    await until(() => harness.taskRequests() >= 3, '重试发起新一轮获取（当前 2→2 即缺陷）')
    harness.resolveWith(harness.lastTasks(), [taskItem(TASK_T1, '3')], null)
    await until(() => taskRows().includes(TASK_T1), '重试后列表恢复')
    expect(hasProjectionError()).toBe(false)
  })

  test('接管期间不得出现两条竞争首屏：connecting 只 1 条，接管后由快照唯一', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    // 接管前：owner 是 TasksView 自有 GET，App 不得同时再发一条兜底
    await until(() => harness.taskRequests() === 1, '接管前唯一首屏 GET')
    await settle()
    expect(harness.taskRequests()).toBe(1)
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    // open 接管：旧 GET 作废，快照接管（第 2 条），不叠加第三条
    expect(harness.taskRequests()).toBe(2)
    harness.resolveWith(harness.lastTasks(), [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().includes(TASK_T1), '快照渲染')
    await settle()
    expect(harness.taskRequests()).toBe(2)
  })
})

describe('F4：快照来源错误只能由当前 ticket 提交，旧轮迟到 500 不得污染 realtime', () => {
  test('任务快照：旧轮 abort 后迟到 reject，不得在 realtime DOM 上显示错误', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    // 第一轮快照先成功落地（realtime），制造「当前投影健康」的背景
    const first = harness.lastTasks()
    harness.resolveWith(first, [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().includes(TASK_T1), '首快照渲染')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // 未知成员提示 → 当前授权重同步：A 轮被 abort（fetch 桩忽略 signal，永不落地）
    FakeEventSource.newest.emitData('task.updated', { id: TASK_T3, revision: '5' })
    await until(() => harness.taskRequests() >= 3, '重同步 GET')
    const roundA = harness.lastTasks()

    // 重同步期间服务端广播 reset：A 被 abort、B 开始并成功 realtime
    FakeEventSource.newest.emit('reset')
    await until(() => harness.taskRequests() >= 4, '第二代重同步 GET')
    const roundB = harness.lastTasks()
    expect(roundB).not.toBe(roundA)
    harness.resolveWith(roundB, [taskItem(TASK_T3, '9')], null)
    await until(() => taskRows().includes(TASK_T3), '新快照渲染')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // A 忽略 abort 迟到失败：当前投影已 realtime，错误不得落 DOM
    harness.rejectWith(roundA, 500)
    await settle()
    expect(hasProjectionError()).toBe(false)
    expect(taskRows()).toContain(TASK_T3)
    expect(indicator()).not.toContain('非实时')
  })

  test('设备快照：旧轮 abort 后迟到 reject，不得在 realtime DOM 上显示错误', async () => {
    const harness = makeHarness({ autoTasks: TASK_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()

    const first = harness.lastDevices()
    harness.resolveWith(first, [deviceItem(DEVICE_A, '实时设备A')])
    await until(() => screen.queryByText('实时设备A') !== null, '首快照渲染')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // 触发重同步：A 轮 abort（fetch 桩忽略 signal）；再触发一轮让 B 接管成功
    FakeEventSource.newest.emitData('device.updated', { device_id: 'e'.repeat(64), revision: '5' })
    await until(() => harness.devicesRequests() >= 3, '重同步 GET')
    const roundA = harness.lastDevices()
    FakeEventSource.newest.emit('reset')
    await until(() => harness.devicesRequests() >= 4, '第二代重同步 GET')
    const roundB = harness.lastDevices()
    expect(roundB).not.toBe(roundA)

    // B 成功（空列表也是合法快照）→ realtime
    harness.resolveWith(roundB, [])
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // A 忽略 abort 迟到失败：devicesList 为空时旧实现会把 devicesError 灌进 realtime DOM
    harness.rejectWith(roundA, 500)
    await settle()
    expect(hasProjectionError()).toBe(false)
    expect(indicator()).not.toContain('非实时')
  })
})

describe('预算链路：撤权→真实 auth.refresh 返回新 token 的自动重建共用失效 episode 余额', () => {
  test('每次 /auth/me 轮换 token：自动重建总数仍 ≤ 1+3，活订阅 ≤ 1', async () => {
    // token 轮换走「同主体新代际」路径：它是失效 episode 触发的自动重建，必须扣同一余额；
    // 三元组变化即回满预算 = permissions.changed 自旋无界（本用例即其回归）。
    const harness = makeHarness({
      autoTasks: TASK_PAGE_AUTO,
      me: (callIndex) => ({ csrfToken: `mem-token-${callIndex}` }),
    })
    renderApp()
    await until(() => harness.taskRequests() >= 0, 'harness')
    await until(() => FakeEventSource.instances.length === 1, '首个订阅')

    let emitted = 0
    for (let round = 0; round < 8; round += 1) {
      const source = FakeEventSource.newest
      if (source.closed) break
      source.emit('permissions.changed')
      emitted += 1
      await settle()
      const next = FakeEventSource.newest
      if (!next.closed) next.emit('open')
      await settle()
    }

    // 至少跑过预算之上的一轮，才证明收敛不是「一次就停」的假绿
    expect(emitted).toBeGreaterThanOrEqual(4)
    // 1 初建 + 最多 MAX_AUTO_RESTARTS(3)；refresh 返回新 token 不得让余额回满
    expect(FakeEventSource.instances.length).toBeLessThanOrEqual(4)
    expect(FakeEventSource.liveCount).toBeLessThanOrEqual(1)
    expect(indicator()).toContain('非实时')
  })

  test('快照 401 → 真实 auth.refresh 轮换 token 的自动链路同样受 episode 余额约束', async () => {
    // 完整自动链路：快照 GET 401 → events.ts shutdown('http-401') → 接线层 awaitingReauth →
    // 宿主 `auth.refresh()`（本用例每次都返回新的 csrf_token）→ 身份三元组变化触发自动重建。
    // 三元组变化不得让余额回满：只有主体真正切换或用户显式重试才重新发放。
    window.location.hash = '#/tasks'
    const harness = makeHarness({
      autoDevices: DEVICE_PAGE_AUTO,
      me: (callIndex) => ({ csrfToken: `mem-token-${callIndex}` }),
    })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '首个订阅')

    let rounds = 0
    for (let round = 0; round < 8; round += 1) {
      const source = FakeEventSource.newest
      if (source.closed) break
      source.emit('open')
      await settle()
      harness.rejectAllPendingTasks()
      rounds += 1
      await settle()
    }

    // 收敛前至少跑过一轮以上，证明「自动链路」真的转起来了而不是首轮就停
    expect(rounds).toBeGreaterThanOrEqual(3)
    expect(FakeEventSource.instances.length).toBeLessThanOrEqual(4)
    expect(FakeEventSource.liveCount).toBeLessThanOrEqual(1)
    expect(indicator()).toContain('非实时')
  })

  test('对照：合法外部轮换（无失效 episode）第 4/5 次换代仍须重建并保持 1 个活订阅', async () => {
    // 裁定的另一侧：外部合法认证轮换本身创建新 ES 且任一时刻只有 1 个活跃，是必要行为，
    // 不得因为「第 4 次换代」被永久杀订阅——只有失效 episode 触发的自动重建才受 ≤3 余额约束。
    makeRotatingAuthStub()
    const h = mountIntegration()
    try {
      await h.auth.refresh()
      await settle()
      for (let rotation = 0; rotation < 5; rotation += 1) {
        const before = FakeEventSource.instances.length
        await h.auth.refresh() // 每次都返回新的 csrf_token：合法外部换代
        await settle()
        expect(FakeEventSource.instances.length).toBe(before + 1)
        expect(FakeEventSource.liveCount).toBe(1)
        FakeEventSource.newest.emit('open')
        await settle()
        expect(h.integration.active.value).toBe(true)
      }
      // 五次合法换代后实时能力仍然可用（快照由 events.ts 代际门禁提交）
      h.snapshot.resolve([deviceItem(DEVICE_A, '轮换后设备')])
      await settle()
      expect(h.integration.isRealtime.value).toBe(true)
    } finally {
      h.integration.dispose()
    }
  })
})

describe('F3：auth 终态（signed_out/error/force_password）必须关闭旧 ES 并 abort 在途快照', () => {
  test('refresh 401 → signed_out：旧 ES close、active=false、在途快照 GET abort', async () => {
    makeAuthStub({ meResponses: [{ status: 200, body: meBody() }, { status: 401, body: {} }] })
    const h = mountIntegration()
    try {
      await h.auth.refresh()
      await settle()
      expect(FakeEventSource.instances.length).toBe(1)
      FakeEventSource.newest.emit('open')
      await settle()
      expect(h.integration.active.value).toBe(true)
      expect(h.snapshotSignals).toHaveLength(1)
      expect(h.snapshotSignals[0]!.aborted).toBe(false)

      // 真实 refresh → 401 → signed_out（经 checking 瞬时态）
      await h.auth.refresh()
      await settle()

      expect(h.auth.status.value).toBe('signed_out')
      expect(FakeEventSource.newest.closed).toBe(true)
      expect(h.integration.active.value).toBe(false)
      expect(h.integration.isRealtime.value).toBe(false)
      // 授权 buffer 水位必须随订阅失效清空：在途快照 GET 必须被 abort
      expect(h.snapshotSignals[0]!.aborted).toBe(true)
    } finally {
      h.integration.dispose()
    }
  })

  test('refresh 500 → error 终态：同样必须关闭旧 ES 并 abort 在途快照', async () => {
    makeAuthStub({ meResponses: [{ status: 200, body: meBody() }, { status: 500, body: {} }] })
    const h = mountIntegration()
    try {
      await h.auth.refresh()
      await settle()
      FakeEventSource.newest.emit('open')
      await settle()
      expect(h.integration.active.value).toBe(true)

      await h.auth.refresh()
      await settle()

      expect(h.auth.status.value).toBe('error')
      expect(FakeEventSource.newest.closed).toBe(true)
      expect(h.integration.active.value).toBe(false)
      expect(h.snapshotSignals[0]!.aborted).toBe(true)
    } finally {
      h.integration.dispose()
    }
  })

  test('refresh → force_password 终态：同样必须关闭旧 ES 并 abort 在途快照', async () => {
    makeAuthStub({
      meResponses: [
        { status: 200, body: meBody() },
        { status: 200, body: meBody({ must_change_password: true }) },
      ],
    })
    const h = mountIntegration()
    try {
      await h.auth.refresh()
      await settle()
      FakeEventSource.newest.emit('open')
      await settle()
      expect(h.integration.active.value).toBe(true)

      await h.auth.refresh()
      await settle()

      expect(h.auth.status.value).toBe('force_password')
      expect(FakeEventSource.newest.closed).toBe(true)
      expect(h.integration.active.value).toBe(false)
      expect(h.snapshotSignals[0]!.aborted).toBe(true)
    } finally {
      h.integration.dispose()
    }
  })

  test('终态关闭后重新登录：必须重新建立订阅并真实重同步，不靠 App 清 DOM 掩盖', async () => {
    makeAuthStub({
      meResponses: [
        { status: 200, body: meBody() },
        { status: 401, body: {} },
        { status: 200, body: meBody({ csrf_token: 'mem-token-new' }) },
      ],
    })
    const h = mountIntegration()
    try {
      await h.auth.refresh()
      await settle()
      FakeEventSource.newest.emit('open')
      await settle()
      h.snapshot.resolve([deviceItem(DEVICE_A, '旧授权设备')])
      await settle()
      expect(h.integration.isRealtime.value).toBe(true)
      const firstCount = FakeEventSource.instances.length

      await h.auth.refresh() // 401 → signed_out → 关订阅
      await settle()
      expect(FakeEventSource.newest.closed).toBe(true)
      expect(h.integration.active.value).toBe(false)

      // 重新鉴权成功：新订阅必须重建，且实时能力必须等新快照真实落位后才恢复
      await h.auth.refresh()
      await settle()
      expect(FakeEventSource.instances.length).toBe(firstCount + 1)
      expect(h.integration.active.value).toBe(true)
      expect(h.integration.isRealtime.value).toBe(false)
      FakeEventSource.newest.emit('open')
      await settle()
      expect(h.snapshotSignals.length).toBeGreaterThanOrEqual(2)
    } finally {
      h.integration.dispose()
    }
  })
})

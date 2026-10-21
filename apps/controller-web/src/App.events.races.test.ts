import { fireEvent, render, screen } from '@testing-library/vue'
import { nextTick } from 'vue'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'

import App from './App.vue'
import { createAuth } from './auth'
import { createEventIntegration } from './event-integration'
import { baseTaskTimeEvidence, zeroTaskCounts, type TaskItem } from './task-ops'
import type { EventSourceLike, SnapshotSource } from './events'

/**
 * AT-18 / 06 §4 独立审查六项缺陷的回归（06 §4；计划
 * `docs/superpowers/plans/2026-10-16-controller-v1-integration-acceptance-tdd.md:648-658`）。
 *
 * 与 `App.events.test.ts` 的分工：那份文件锁定「谁在什么时候创建/关闭订阅」的正常路径；
 * 本文件专门锁定**两条获取路径竞争时**的行为，因此用可控的 deferred fetch 替身
 * （可任意顺序 resolve、可 reject、**可忽略 AbortSignal 继续回灌**）+ 注入的 EventSource 替身，
 * 跑真实 App（`App.vue`）+ 真实 `auth` store（`auth.refresh()` 的同步 checking 照原样发生）
 * + 真实 `events.ts` 客户端 + 真实 `event-integration.ts` 接线层，不 mock 任何接线回调。
 *
 * 忽略 abort 的 fetch 替身是刻意选择：代际与所有权必须自己成立，不能把正确性外包给
 * 浏览器是否真的中止了请求（审查第 2、3 项：接管当刻 abort + 落地再核 ticket）。
 *
 * 服务端事件发布方未实现，本文件只是 A 层接线回归，不构成 AT-18 端到端或真实浏览器验收。
 */

const USER_ID = '01234567-89ab-4cde-8f01-234567890abc'
const RID = '123e4567-e89b-42d3-a456-426614174000'
const DEVICE_A = 'a'.repeat(64)
const DEVICE_OLD = 'o'.repeat(64)

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

  /** 只在未关闭的订阅上投递：关闭后的订阅不再收到事件（与浏览器语义一致）。 */
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

// ── deferred fetch 替身：每个业务 GET 由测试决定何时、以什么结果落地 ──────────────────

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
  /** 带 cursor 的请求 = 分页；否则首屏（SSE 快照或组件/父级兜底 GET）。 */
  paged: boolean
  signal: AbortSignal | null
  settled: boolean
  response: Deferred<Response>
}

interface MeSpec { body: unknown; status?: number }

interface HarnessOptions {
  /** 按调用序号（1 起）决定 /auth/me 的响应：用于 token 轮换与主体切换。 */
  me?: (callIndex: number) => MeSpec
  /** 立即落地的设备/任务分页：不需要参与竞争的另一种列表用它，避免测试被迫手工喂两侧。 */
  autoDevices?: unknown
  autoTasks?: unknown
}

interface Harness {
  requests: RecordedRequest[]
  pending: (kind: RequestKind, options?: { paged?: boolean }) => RecordedRequest[]
  last: (kind: RequestKind, options?: { paged?: boolean }) => RecordedRequest
  resolveWith: (request: RecordedRequest, items: unknown[], nextCursor?: string | null) => void
  rejectWith: (request: RecordedRequest, status?: number) => void
  rejectAllPending: (kind: RequestKind) => void
  devicesRequests: () => number
  taskRequests: () => number
  meRequests: () => number
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

const DEVICE_PAGE_AUTO = { items: [], next_cursor: null }
const TASK_PAGE_AUTO = { items: [], next_cursor: null }

function makeHarness(options: HarnessOptions = {}): Harness {
  const requests: RecordedRequest[] = []
  let meIndex = 0

  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input)
    if (url.includes('/auth/me')) {
      meIndex += 1
      const spec = options.me?.(meIndex) ?? { body: ME_DATA }
      return Promise.resolve(jsonResponse(
        spec.status !== undefined && spec.status >= 400
          ? { error: { code: 'AUTH_REQUIRED', message_key: 'auth.error.authRequired' }, request_id: RID }
          : { data: spec.body, request_id: RID },
        spec.status ?? 200,
      ))
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
      paged: url.includes('cursor='),
      signal: init?.signal ?? null,
      settled: false,
      response: deferred<Response>(),
    }
    requests.push(request)
    return request.response.promise
  }))

  const of = (kind: RequestKind, filter?: { paged?: boolean }): RecordedRequest[] =>
    requests.filter((request) => request.kind === kind &&
      (filter?.paged === undefined || request.paged === filter.paged))

  return {
    requests,
    pending: (kind, filter) => of(kind, filter).filter((request) => !request.settled),
    last: (kind, filter) => {
      const list = of(kind, filter)
      const request = list[list.length - 1]
      if (request === undefined) throw new Error(`no ${kind} request recorded`)
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
    rejectAllPending: (kind) => {
      for (const request of requests.filter((item) => item.kind === kind && !item.settled)) {
        request.settled = true
        request.response.resolve(jsonResponse(
          { error: { code: 'INTERNAL', message_key: 'errors.generic' }, request_id: RID }, 500,
        ))
      }
    },
    devicesRequests: () => of('devices').length,
    taskRequests: () => of('tasks').length,
    meRequests: () => meIndex,
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
const TASK_LATE = '9e2b6f4d-1c7a-4b3d-8e5f-6a0c2d4e8b11'
const TASK_FALLBACK = '3c5e7a9b-1d3f-5b7d-9e1c-3a5b7d9e1f3c'

/**
 * 静默推进：让真实 Promise 链（fetch → unwrap → auth.refresh → watcher → 订阅）跑完一轮，
 * 并把 Vue 的渲染队列刷到 DOM。生产接线不使用任何定时器，因此「不该发生的副作用」
 * 用这段静默期来证明（若有人改成 setTimeout 兜底重建，这里同样会暴露多出来的订阅）。
 */
async function settle(rounds = 5): Promise<void> {
  for (let index = 0; index < rounds; index += 1) {
    await new Promise<void>((resolve) => { setTimeout(resolve, 0) })
    await nextTick()
  }
}

/** 轮询等待条件成立（真实 Promise 链需要多个宏/微任务轮次，与 vi.waitFor 同级别）。 */
async function until(condition: () => boolean, message: string): Promise<void> {
  await vi.waitFor(
    () => { if (!condition()) throw new Error(message) },
    { timeout: 3000, interval: 1 },
  )
}

/** 每个用例都要卸载上一个 App：screen 查询覆盖整个 body，残留挂载会让后续用例互相串扰。 */
let mountedUnmount: (() => void) | null = null
function renderApp(): void {
  const { unmount } = render(App)
  mountedUnmount = unmount
}

const indicator = (): string => screen.getByTestId('event-connection-state').textContent ?? ''
const taskRows = (): string => screen.getByRole('table').textContent ?? ''
const hasLoadMore = (): boolean => screen.queryByRole('button', { name: '加载更多' }) !== null
const hasListError = (): boolean => screen.queryAllByText('无法加载内容').length > 0

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

describe('缺陷 1：撤权自动重建预算跨 checking/refresh 保留（真实 auth.refresh）', () => {
  test('连续撤权不再产生「每次一个 EventSource + 每次一个 /auth/me」的无界循环', async () => {
    const harness = makeHarness({ autoTasks: TASK_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '首个订阅')
    FakeEventSource.newest.emit('open')
    await settle()

    let emitted = 0
    for (let round = 0; round < 8; round += 1) {
      const source = FakeEventSource.newest
      if (source.closed) break // 预算耗尽后不再有活订阅：撤权循环必须自行终止
      source.emit('permissions.changed')
      emitted += 1
      await settle()
    }

    // 至少跑过预算之上的一轮，才证明这不是「一次就停」的假绿
    expect(emitted).toBeGreaterThanOrEqual(4)
    // 1 次初建 + 最多 MAX_AUTO_RESTARTS(3) 次自动重建；refresh 的 checking 瞬时态不得回满预算
    expect(FakeEventSource.instances.length).toBeLessThanOrEqual(4)
    // 每轮重建都走一次真实 auth.refresh()：同样必须收敛，不能与订阅数一起线性增长
    expect(harness.meRequests()).toBeLessThanOrEqual(5)
    expect(indicator()).toContain('非实时')
  })

  test('预算耗尽后同主体不再自动重建，但用户显式重试仍必须恢复订阅', async () => {
    const harness = makeHarness({ autoTasks: TASK_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '首个订阅')
    FakeEventSource.newest.emit('open')
    await settle()

    for (let round = 0; round < 8; round += 1) {
      const source = FakeEventSource.newest
      if (source.closed) break
      source.emit('permissions.changed')
      await settle()
    }
    const exhausted = FakeEventSource.instances.length
    expect(exhausted).toBeLessThanOrEqual(4)

    // 让设备投影进入错误态，暴露 App 既有的「重试」入口（用户显式重试 = 唯一的预算重发渠道）
    harness.rejectAllPending('devices')
    await until(() => screen.queryByRole('button', { name: '重试' }) !== null, '设备错误态与重试入口')
    const before = FakeEventSource.instances.length
    await fireEvent.click(screen.getByRole('button', { name: '重试' }))
    await settle()
    expect(FakeEventSource.instances.length).toBe(before + 1)
    expect(FakeEventSource.liveCount).toBe(1)
  })
})

describe('缺陷 2 + 6：SSE 首屏快照接管当刻作废旧游标分页，并原子清除分页错误', () => {
  test('旧 cursor 分页晚到不得把成员追加回列表、不得覆盖新 cursor', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    // SSE 还没 open（connecting）时诚实非实时：任务首屏由组件自己发一条普通 GET
    await until(() => harness.taskRequests() >= 1, '组件自有首屏 GET')
    const componentFirstScreen = harness.last('tasks', { paged: false })
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    // open 接管当刻（响应式一轮内）作废这条在途首屏 GET：同一时刻只能有一个获取者
    await settle()
    expect(componentFirstScreen.signal?.aborted).toBe(true)

    // 首屏快照 Trev1 + 游标 cursor-1
    const snapshot1 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot1, [taskItem(TASK_T1, '7')], 'cursor-1')
    await until(() => taskRows().includes(TASK_T1) && hasLoadMore(), '首屏快照渲染并出现分页入口')
    expect(indicator()).not.toContain('非实时')

    // 旧游标分页在途
    await fireEvent.click(screen.getByRole('button', { name: '加载更多' }))
    const paged = harness.last('tasks', { paged: true })
    expect(paged.settled).toBe(false)

    // 新快照 Trev3 / cursor null 先到（task.updated 只是失效提示 → 当前授权重取）
    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '重同步 GET')
    const snapshot2 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot2, [taskItem(TASK_T3, '3')], null)
    await until(() => taskRows().includes(TASK_T3) && !hasLoadMore(), '新首屏替换列表与游标')

    // 迟到的旧分页即使忽略 abort 也不得回灌
    harness.resolveWith(paged, [taskItem(TASK_LATE, '1')], 'cursor-2')
    await settle()
    expect(taskRows()).toContain(TASK_T3)
    expect(taskRows()).not.toContain(TASK_LATE)
    expect(hasLoadMore()).toBe(false)
    expect(indicator()).not.toContain('非实时')
  })

  test('分页失败后新快照到达：错误态与视图同步，不再停在「无法加载」', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    const snapshot1 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot1, [taskItem(TASK_T1, '7')], 'cursor-1')
    await until(() => hasLoadMore(), '首屏渲染')

    await fireEvent.click(screen.getByRole('button', { name: '加载更多' }))
    harness.rejectWith(harness.last('tasks', { paged: true }))
    await until(() => hasListError(), '分页错误可见')
    expect(indicator()).not.toContain('非实时')

    // 当前代际的新快照：原子清错误 + 清分页 busy，视图与同步状态一致
    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '重同步 GET')
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T3, '3')], null)
    await until(() => taskRows().includes(TASK_T3), '新快照渲染')
    expect(hasListError()).toBe(false)
    expect(indicator()).not.toContain('非实时')
  })
})

describe('缺陷 3：普通 GET 与 SSE 快照的所有权在切换当刻统一失效', () => {
  test('open 当刻 abort 在途普通 GET；忽略 abort 的旧响应不写 DOM；卸载 abort 在途快照', async () => {
    const harness = makeHarness({ autoTasks: TASK_PAGE_AUTO })
    renderApp()
    await until(() => harness.devicesRequests() === 1, '首屏普通 GET')
    const normalGet = harness.last('devices')
    expect(normalGet.signal).not.toBeNull()

    FakeEventSource.newest.emit('open')
    await settle()
    // 所有权切换当刻（open）即失效旧普通 GET，而不是等快照提交才 ++generation
    expect(normalGet.signal?.aborted).toBe(true)
    expect(harness.devicesRequests()).toBe(2)

    // 旧普通 GET 忽略 abort 先落地：绝不写进 DOM，也不能标实时
    harness.resolveWith(normalGet, [deviceItem(DEVICE_OLD, '旧列表设备')])
    await settle()
    expect(screen.queryByText('旧列表设备')).toBeNull()
    expect(indicator()).toContain('非实时')

    const snapshotGet = harness.last('devices')
    harness.resolveWith(snapshotGet, [deviceItem(DEVICE_A, '实时设备A')])
    await until(() => screen.queryByText('实时设备A') !== null, '快照回写')
    expect(indicator()).not.toContain('非实时')

    // 在途快照 GET 在卸载时必须被 abort
    FakeEventSource.newest.emitData('device.updated', { device_id: DEVICE_A, revision: '9' })
    await until(() => harness.pending('devices').length > 0, '重同步 GET')
    const inFlight = harness.last('devices')
    mountedUnmount?.()
    await settle()
    expect(inFlight.signal?.aborted).toBe(true)
  })
})

describe('缺陷 4：connecting/reconnecting/stale 诚实非实时且保留安全 GET 读取', () => {
  test('断线期间普通 GET 兜底可用；重新 open 接管当刻取消并失效该兜底请求', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    const snapshot1 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot1, [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().includes(TASK_T1), '首屏快照渲染')
    expect(indicator()).not.toContain('非实时')

    // 非认证类断线：保留最后合法快照（06 §4「不因此清DOM」），同时诚实非实时 + 普通 GET 兜底
    FakeEventSource.newest.emit('error', {})
    await until(() => indicator().includes('非实时'), '断线指示非实时')
    expect(taskRows()).toContain(TASK_T1)
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '兜底 GET')
    const fallback = harness.last('tasks', { paged: false })

    // 重新 open：SSE 接管当刻取消并失效这条兜底 GET，两条获取路径不得同时有效
    FakeEventSource.newest.emit('open')
    await settle()
    expect(fallback.signal?.aborted).toBe(true)
    harness.resolveWith(fallback, [taskItem(TASK_FALLBACK, '1')], null)
    await settle()
    expect(taskRows()).not.toContain(TASK_FALLBACK)

    const snapshot2 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot2, [taskItem(TASK_T3, '3')], null)
    await until(() => taskRows().includes(TASK_T3), '接管后快照重新成为唯一来源')
    expect(taskRows()).not.toContain(TASK_FALLBACK)
    expect(indicator()).not.toContain('非实时')
  })

  test('快照请求失败：诚实非实时、保留最后合法快照并提供重试', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ autoDevices: DEVICE_PAGE_AUTO })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    const snapshot1 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot1, [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().includes(TASK_T1), '首屏快照渲染')

    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '重同步 GET')
    harness.rejectWith(harness.last('tasks', { paged: false }))
    await until(() => hasListError(), '重同步失败可见并可重试')
    expect(taskRows()).toContain(TASK_T1)
    expect(indicator()).toContain('非实时')

    await fireEvent.click(screen.getByRole('button', { name: '重试' }))
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '重试再次发起 GET')
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T3, '3')], null)
    await until(() => taskRows().includes(TASK_T3), '重试成功')
    expect(hasListError()).toBe(false)
  })
})

describe('缺陷 5：csrf token 轮换属于认证代际变化（真实 events 客户端 + 接线层）', () => {
  test('同 id/同 epoch 仅 token 轮换：close 旧订阅、abort 在途快照、旧快照不回写、新订阅重建', async () => {
    const auth = createAuth()
    auth.status.value = 'signed_in'
    auth.user.value = ME_DATA.user
    auth.authzEpoch.value = '7'
    auth.csrfToken.value = 'token-1'

    const instances: FakeEventSource[] = []
    const snapshotRequest = deferred<Array<Record<string, unknown>>>()
    const snapshotSignals: AbortSignal[] = []
    const snapshotKinds: string[] = []
    const snapshots: SnapshotSource[] = [{
      kind: 'device',
      fetch: ({ signal }) => {
        snapshotSignals.push(signal)
        return snapshotRequest.promise
      },
    }]
    const integration = createEventIntegration({
      auth,
      snapshots,
      eventSourceFactory: (url) => {
        const instance = new FakeEventSource(url)
        instances.push(instance)
        return instance as unknown as EventSourceLike
      },
      onSnapshot: (kind) => { snapshotKinds.push(kind) },
    })

    try {
      expect(instances).toHaveLength(1)
      instances[0]!.emit('open')
      await settle()
      expect(snapshotSignals).toHaveLength(1)
      expect(integration.isRealtime.value).toBe(false)

      // token 轮换：user.id 与 authz_epoch 都没变，只有 csrf token 变了
      auth.csrfToken.value = 'token-2'
      await settle()

      expect(instances[0]!.closed).toBe(true)
      expect(instances).toHaveLength(2)
      // 旧授权下的在途快照 GET 必须被 abort，且它的响应绝不回写投影
      expect(snapshotSignals[0]!.aborted).toBe(true)
      snapshotRequest.resolve([deviceItem(DEVICE_A, '旧授权设备')])
      await settle()
      expect(snapshotKinds).toHaveLength(0)
      expect(integration.isRealtime.value).toBe(false)
      expect(integration.active.value).toBe(true)
    } finally {
      integration.dispose()
    }
  })
})

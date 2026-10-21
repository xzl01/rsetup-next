import { fireEvent, render, screen } from '@testing-library/vue'
import { nextTick } from 'vue'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'

import App from './App.vue'
import { baseTaskTimeEvidence, zeroTaskCounts, type SubTaskItem, type TaskItem } from './task-ops'

/**
 * SSE 接线第三轮整改回归（审查 P1/P2：快照 revision 单调、详情页失效链路、整轮失败呈现）。
 *
 * 与既有三份文件的分工：`App.events.test.ts` 锁定订阅存亡，`App.events.races.test.ts` 锁定
 * 两条获取路径竞争，`App.events.failures.test.ts` 锁定 owner 移交与错误 ticket。本文件锁定
 * 快照**落到投影之后**与**投影之外**的三段数据流：
 *
 * 1. 全量快照对成员集合与顺序权威，但对「同一对象的版本」不权威：服务端读库/缓存回退返回
 *    更旧 revision 时，投影绝不能被旧对象覆盖（events.ts 的水位只前进，唯独宿主这一跳漏了）；
 * 2. 任务详情页的 `/tasks/:id` 与 `/tasks/:id/children` 不属于任何全量快照来源，
 *    `task.updated` 失效提示必须单独送达详情页，否则用户停在详情页永远看的是旧 revision；
 * 3. 一轮同步里任一来源 reject 会让整轮原子不提交：成功那一路既没提交、也没错误线索，
 *    SSE 管理期组件又不自发 GET，结果必须是「如实错误 + 可用的重试」，不能是无痕空白。
 *
 * 与 races 相同的取数替身：deferred fetch 可任意顺序落地、可 reject、**忽略 AbortSignal**，
 * 真实 App + 真实 auth store + 真实 events.ts + 真实接线层，只有 fetch 与 EventSource 是替身。
 * 服务端事件发布方未实现，本文件只是接线层回归，不构成 AT-18 端到端或真实浏览器验收。
 */

const USER_ID = '01234567-89ab-4cde-8f01-234567890abc'
const RID = '123e4567-e89b-42d3-a456-426614174000'
const DEVICE_A = 'a'.repeat(64)
const TASK_T1 = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const TASK_T2 = '1b3d5f7a-9c1e-4d2f-8a6b-0c4e6a8b0d2f'
const SUBTASK_1 = '2b3d5f7a-9c1e-4d2f-8a6b-0c4e6a8b0d31'

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
}

// ── deferred fetch 替身：按端点分类，可任意顺序落地、可 reject、忽略 AbortSignal ─────────

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  const promise = new Promise<T>((res) => { resolve = res })
  return { promise, resolve }
}

type RequestKind = 'devices' | 'tasks' | 'detail' | 'children'

interface RecordedRequest {
  url: string
  kind: RequestKind
  /** 带 cursor 的请求 = 分页；否则首屏（SSE 快照或组件/父级兜底 GET）。 */
  paged: boolean
  settled: boolean
  /** events.ts / 组件下发的中止信号，用于断言所有权切换当刻的 abort。 */
  signal: AbortSignal | null
  response: Deferred<Response>
}

function classify(url: string): RequestKind | null {
  const path = url.split('?')[0] ?? url
  if (/^\/api\/v1\/devices$/.test(path)) return 'devices'
  if (/^\/api\/v1\/tasks$/.test(path)) return 'tasks'
  if (/^\/api\/v1\/tasks\/[^/]+\/children$/.test(path)) return 'children'
  if (/^\/api\/v1\/tasks\/[^/]+$/.test(path)) return 'detail'
  return null
}

interface Harness {
  requests: RecordedRequest[]
  pending: (kind: RequestKind, options?: { paged?: boolean }) => RecordedRequest[]
  last: (kind: RequestKind, options?: { paged?: boolean }) => RecordedRequest
  count: (kind: RequestKind, options?: { paged?: boolean }) => number
  /** 列表页响应：`{ items, next_cursor }`。 */
  resolveWith: (request: RecordedRequest, items: unknown[], nextCursor?: string | null) => void
  /** 单对象响应（任务详情 `data` 即 TaskItem 本身）。 */
  resolveData: (request: RecordedRequest, data: unknown) => void
  rejectWith: (request: RecordedRequest, status?: number) => void
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

interface HarnessOptions {
  /** 按 kind 立即落地的响应体；未列出的 kind 走 deferred，由测试决定何时、以什么结果落地。 */
  auto?: Partial<Record<RequestKind, unknown>>
}

function makeHarness(options: HarnessOptions = {}): Harness {
  const requests: RecordedRequest[] = []

  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input)
    if (url.includes('/auth/me')) {
      return Promise.resolve(jsonResponse({ data: ME_DATA, request_id: RID }))
    }
    if (url.includes('/auth/logout')) {
      return Promise.resolve(jsonResponse({ data: { logged_out: true }, request_id: RID }))
    }
    const kind = classify(url)
    if (kind === null) throw new TypeError(`unexpected fetch ${url}`)
    const auto = options.auto?.[kind]
    if (auto !== undefined) return Promise.resolve(jsonResponse({ data: auto, request_id: RID }))
    const request: RecordedRequest = {
      url,
      kind,
      paged: url.includes('cursor='),
      settled: false,
      signal: init?.signal ?? null,
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
    count: (kind, filter) => of(kind, filter).length,
    resolveWith: (request, items, nextCursor = null) => {
      request.settled = true
      request.response.resolve(jsonResponse({ data: { items, next_cursor: nextCursor }, request_id: RID }))
    },
    resolveData: (request, data) => {
      request.settled = true
      request.response.resolve(jsonResponse({ data, request_id: RID }))
    },
    rejectWith: (request, status = 500) => {
      request.settled = true
      request.response.resolve(jsonResponse(
        { error: { code: 'INTERNAL', message_key: 'errors.generic' }, request_id: RID },
        status,
      ))
    },
  }
}

// ── 夹具 ──────────────────────────────────────────────────────────────────────

function taskItem(id: string, revision: string): TaskItem {
  return {
    id, operation: 'reboot', state: 'queued', outcome: null, revision,
    created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(), view_scope: 'authorized_subset',
  }
}

/** held 主任务 + full 投影：让时间证据三选控件（组件局部状态）出现在详情页上。 */
function heldTask(id: string, revision: string): TaskItem {
  return {
    ...taskItem(id, revision),
    state: 'held',
    counts: { ...zeroTaskCounts(), held: '1' },
    view_scope: 'full',
  }
}

function deviceItem(id: string, displayName: string, revision?: string): Record<string, unknown> {
  return {
    device_id: id, display_name: displayName,
    effective_permissions: ['device.read', 'device.reboot'],
    ...(revision === undefined ? {} : { revision }),
  }
}

function subTaskItem(revision: string): SubTaskItem {
  return {
    id: SUBTASK_1, device_id: DEVICE_A, state: 'queued',
    reason_code: null, revision, result_evidence: null,
  }
}

const EMPTY_PAGE = { items: [], next_cursor: null }

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
/** 逐行返回 [任务 id, 版本]：版本是第 3 个 td，避免与 UUID、时间戳里的数字串味。 */
const taskRows = (): Array<{ id: string; revision: string }> =>
  Array.from(screen.queryByRole('table')?.querySelectorAll('tbody tr') ?? []).map((row) => ({
    id: row.querySelector('td:nth-child(1)')?.textContent?.trim() ?? '',
    revision: row.querySelector('td:nth-child(3)')?.textContent?.trim() ?? '',
  }))
const detailRevision = (): string => screen.getByTestId('task-revision').textContent ?? ''
const childRevision = (): string =>
  document.querySelector(`[data-testid="subtask-row-${SUBTASK_1}"] .task-state-row__revision`)?.textContent?.trim() ?? ''
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

describe('P1-a：旧 revision 的快照不得把已投影的较新对象打回旧状态', () => {
  test('任务快照回退到更旧 revision：同 ID 保留较新版本，成员与顺序仍按新快照', async () => {
    window.location.hash = '#/tasks'
    // 设备侧与本页无关，立即落地以免干扰计数
    const harness = makeHarness({ auto: { devices: EMPTY_PAGE } })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()

    const snapshot1 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot1, [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().some((row) => row.id === TASK_T1), '首快照渲染')
    expect(taskRows().find((row) => row.id === TASK_T1)?.revision).toBe('7')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // 已知成员提示 → 当前授权重取；这一轮服务端读库回退，返回更旧的 rev 3，
    // 同时带来新成员 T2：成员集合与顺序仍以本轮快照为准，只有同 ID 对象受 revision 保护
    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '重同步 GET')
    const snapshot2 = harness.last('tasks', { paged: false })
    harness.resolveWith(snapshot2, [taskItem(TASK_T2, '2'), taskItem(TASK_T1, '3')], null)
    await until(() => taskRows().some((row) => row.id === TASK_T2), '新成员进入投影')

    const rows = taskRows()
    expect(rows.map((row) => row.id)).toEqual([TASK_T2, TASK_T1])
    // 缺陷点：旧实现无条件整体替换，这里会退成 rev 3
    expect(rows.find((row) => row.id === TASK_T1)?.revision).toBe('7')
    expect(rows.find((row) => row.id === TASK_T2)?.revision).toBe('2')
    expect(indicator()).not.toContain('非实时')
  })

  test('设备快照回退到更旧 revision：同 ID 保留较新投影，不被旧列表项覆盖', async () => {
    const harness = makeHarness({ auto: { tasks: EMPTY_PAGE } })
    renderApp()
    await until(() => harness.count('devices') >= 1, '首屏普通 GET')
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '设备新版', '9')])
    await until(() => screen.queryByText('设备新版') !== null, '普通 GET 渲染')
    FakeEventSource.newest.emit('open')
    await settle()

    // 接管快照：同对象同 revision —— 正常回写，不应被保护逻辑误拦
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '设备新版', '9')])
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // 重同步返回 rev 4 的旧投影：不得把显示名打回
    FakeEventSource.newest.emitData('device.updated', { device_id: DEVICE_A, revision: '11' })
    await until(() => harness.pending('devices').length > 0, '重同步 GET')
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '设备旧版', '4')])
    await settle()
    expect(screen.queryByText('设备旧版')).toBeNull()
    expect(screen.queryByText('设备新版')).not.toBeNull()

    // fail-open 一侧：设备列表项本来就可以不带 revision（DeviceItem 无该字段），
    // 无法证明回退时按快照语义照常覆盖，保护逻辑不得把合法新快照整批丢掉
    FakeEventSource.newest.emitData('device.updated', { device_id: DEVICE_A, revision: '12' })
    await until(() => harness.pending('devices').length > 0, '第二次重同步 GET')
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '设备无版本')])
    await until(() => screen.queryByText('设备无版本') !== null, '无 revision 项按快照覆盖')
    expect(screen.queryByText('设备新版')).toBeNull()
  })

  test('更高 revision 的快照照常覆盖：保护逻辑只拦更旧，不拦更新', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ auto: { devices: EMPTY_PAGE } })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().some((row) => row.id === TASK_T1), '首快照渲染')

    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '重同步 GET')
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '11')], null)
    await until(() => taskRows().find((row) => row.id === TASK_T1)?.revision === '11', '更高 revision 覆盖')

    // 同 revision 也按快照覆盖（相等不是回退）
    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '12' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '第二次重同步 GET')
    harness.resolveWith(harness.last('tasks', { paged: false }), [{ ...taskItem(TASK_T1, '11'), state: 'running' }], null)
    await until(() => screen.queryAllByText('运行中').length > 0, '同 revision 项按快照覆盖')
  })
})

describe('P1-b：任务详情页接进 SSE 失效链路（主任务与 children 一起重取）', () => {
  test('停在详情页收到 task.updated：主任务与 children 都按当前授权重新拉取', async () => {
    window.location.hash = `#/tasks/${TASK_T1}`
    // 任务列表快照含 T1，使 task.updated 属于已知成员（未知成员走整轮重同步，不分发失效提示）
    const harness = makeHarness({
      auto: {
        devices: EMPTY_PAGE,
        tasks: { items: [taskItem(TASK_T1, '3')], next_cursor: null },
      },
    })
    renderApp()
    await until(() => harness.count('detail') === 1, '详情页首屏 GET')
    await until(() => harness.count('children') === 1, 'children 首屏 GET')
    harness.resolveData(harness.last('detail'), taskItem(TASK_T1, '7'))
    harness.resolveWith(harness.last('children'), [subTaskItem('3')])
    await until(() => detailRevision().includes('7'), '主任务渲染 rev 7')
    await until(() => childRevision() === '3', 'children 渲染 rev 3')

    FakeEventSource.newest.emit('open')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.count('detail') === 2 && harness.count('children') === 2,
      '失效提示触发详情与 children 重取')

    // 重取按当前授权重发同一条 GET，新数据真实回写到页面
    expect(harness.requests.filter((request) => request.kind === 'detail').map((request) => request.url))
      .toEqual([`/api/v1/tasks/${TASK_T1}`, `/api/v1/tasks/${TASK_T1}`])
    harness.resolveData(harness.last('detail'), taskItem(TASK_T1, '9'))
    harness.resolveWith(harness.last('children'), [subTaskItem('4')])
    await until(() => detailRevision().includes('9'), '主任务更新为 rev 9')
    await until(() => childRevision() === '4', 'children 更新为 rev 4')
  })

  test('一次失效只重取一次，且详情组件实例保留（危险操作的已选证据不被抹掉）', async () => {
    window.location.hash = `#/tasks/${TASK_T1}`
    const harness = makeHarness({
      auto: {
        devices: EMPTY_PAGE,
        tasks: { items: [taskItem(TASK_T1, '3')], next_cursor: null },
      },
    })
    renderApp()
    await until(() => harness.count('detail') === 1, '详情页首屏 GET')
    harness.resolveData(harness.last('detail'), heldTask(TASK_T1, '7'))
    harness.resolveWith(harness.last('children'), [subTaskItem('3')])
    await until(() => screen.queryByTestId('time-review-evidence') !== null, '时间证据三选控件渲染')

    const evidenceInput = screen.getByTestId('time-review-evidence-inconclusive') as HTMLInputElement
    await fireEvent.click(evidenceInput)
    expect(evidenceInput.checked).toBe(true)

    FakeEventSource.newest.emit('open')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.count('detail') === 2, '详情重取一次')
    harness.resolveData(harness.last('detail'), heldTask(TASK_T1, '9'))
    harness.resolveWith(harness.last('children'), [subTaskItem('4')])
    await until(() => detailRevision().includes('9'), '主任务更新为 rev 9')
    await settle()
    // 同一条失效提示 = 主任务一次 + children 一次，绝不为同一提示反复轰炸
    expect(harness.count('detail')).toBe(2)
    expect(harness.count('children')).toBe(2)
    // 组件未被 :key 重建：组件局部的三值证据选择仍然保留（重建会抹掉在途危险操作意图）
    expect((screen.getByTestId('time-review-evidence-inconclusive') as HTMLInputElement).checked).toBe(true)
  })
})

describe('P1-d：一轮同步部分失败 = 整轮不提交，未提交的投影必须可见且可重试', () => {
  test('设备快照 500 连坐整轮：任务页不得无痕空白，错误态 + 重试必须真的再发一轮', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness()
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()

    // 任务来源取到合法数据，设备来源 500：allSettled 整轮不提交
    const tasksGet = harness.last('tasks', { paged: false })
    const devicesGet = harness.last('devices')
    harness.rejectWith(devicesGet, 500)
    harness.resolveWith(tasksGet, [taskItem(TASK_T1, '1')], null)
    await settle()

    // 缺陷点：整轮未提交 + 组件不自发 GET → 旧行为是空白表格且没有任何错误入口
    await until(() => hasProjectionError(), '任务投影错误可见')
    expect(taskRows().some((row) => row.id === TASK_T1)).toBe(false)
    expect(indicator()).toContain('非实时')

    const tasksBefore = harness.count('tasks', { paged: false })
    const devicesBefore = harness.count('devices')
    await fireEvent.click(screen.getByRole('button', { name: '重试' }))
    await until(() => harness.count('tasks', { paged: false }) > tasksBefore &&
      harness.count('devices') > devicesBefore, '重试重新发起整轮快照')

    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '实时设备A', '1')])
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '1')], null)
    await until(() => taskRows().some((row) => row.id === TASK_T1), '重试成功后列表恢复')
    expect(hasProjectionError()).toBe(false)
    // 接管当刻被作废的组件自有首屏 GET 至今仍是 aborted：失败→重试这条路径上
    // 始终只有一条获取路径有效，迟到响应无从把旧页写回表格
    expect(harness.requests[0]?.signal?.aborted).toBe(true)
    await until(() => !indicator().includes('非实时'), '指示器 realtime')
  })

  test('任务快照 500 且已有合法快照：保留表格并提供重试，不清空已授权数据', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness()
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '实时设备A', '1')])
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().some((row) => row.id === TASK_T1), '首快照渲染')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '重同步 GET')
    harness.rejectWith(harness.last('tasks', { paged: false }), 500)
    await until(() => hasProjectionError(), '重同步失败可见')
    // 最后合法快照留在表格里（06 §4：非认证失效不清空已授权数据）
    expect(taskRows().some((row) => row.id === TASK_T1)).toBe(true)
    expect(screen.getByRole('button', { name: '重试' })).toBeTruthy()
  })
})

describe('P1-c：不在首屏快照成员中的任务，其失效同样必须送达详情页（复审 P1）', () => {
  test('深链进入的任务不在首屏快照里：未知成员的 task.updated 同样重取详情与 children', async () => {
    // 缺陷点：App 原先只由 `onInvalidation` 推进详情刷新，而那条链路只由 events.ts 的
    // `onEntityUpdate` 驱动 —— 它只对当前快照的**已知成员**分发。深链或翻分页进入的任务不在
    // 成员集合里，其事件走 `unknown-member` 整轮重同步：列表换了，详情仍永远停在旧 revision。
    window.location.hash = `#/tasks/${TASK_T2}`
    const harness = makeHarness({ auto: { devices: EMPTY_PAGE } })
    renderApp()
    await until(() => harness.count('detail') === 1, '详情页首屏 GET')
    harness.resolveData(harness.last('detail'), taskItem(TASK_T2, '7'))
    harness.resolveWith(harness.last('children'), [subTaskItem('3')])
    await until(() => detailRevision().includes('7'), '主任务渲染 rev 7')

    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    // 首屏快照成员只有 T1：正在浏览的 T2 对客户端而言是「未知对象」
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '3')], null)
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    FakeEventSource.newest.emitData('task.updated', { id: TASK_T2, revision: '9' })
    await until(() => harness.count('detail') === 2 && harness.count('children') === 2,
      '未知成员的失效同样触发详情重取')

    // 重取仍是按当前授权重发同一条 GET，最新数据真实回写到页面
    expect(harness.requests.filter((request) => request.kind === 'detail').map((request) => request.url))
      .toEqual([`/api/v1/tasks/${TASK_T2}`, `/api/v1/tasks/${TASK_T2}`])
    harness.resolveData(harness.last('detail'), taskItem(TASK_T2, '9'))
    harness.resolveWith(harness.last('children'), [subTaskItem('5')])
    await until(() => detailRevision().includes('9'), '主任务更新为 rev 9')
    // 同一事件在 events.ts 侧照旧按未知成员处理：整轮重同步发生，不凭提示打补丁
    expect(harness.count('tasks', { paged: false })).toBeGreaterThanOrEqual(2)
  })

  test('别的任务的更新不触发当前详情的重取：提示按 id 精确匹配', async () => {
    window.location.hash = `#/tasks/${TASK_T2}`
    const harness = makeHarness({ auto: { devices: EMPTY_PAGE } })
    renderApp()
    await until(() => harness.count('detail') === 1, '详情页首屏 GET')
    harness.resolveData(harness.last('detail'), taskItem(TASK_T2, '7'))
    harness.resolveWith(harness.last('children'), [subTaskItem('3')])
    await until(() => detailRevision().includes('7'), '主任务渲染 rev 7')

    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '3')], null)
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // T1 是首屏快照里的已知成员：它的更新照常重取列表，但当前浏览的是 T2，
    // 详情不该为无关对象的提示多打一对 GET（提示按 id 精确匹配）
    FakeEventSource.newest.emitData('task.updated', { id: TASK_T1, revision: '9' })
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '列表重同步 GET')
    await settle()
    expect(harness.count('detail')).toBe(1)
    expect(harness.count('children')).toBe(1)
  })
})

describe('P1-e：普通 GET 与断线降级兜底同走 revision 单调合并（复审 P1）', () => {
  test('断线降级后的兜底首屏带更旧 revision：不得把已投影的任务打回旧版本', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness({ auto: { devices: EMPTY_PAGE } })
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '7')], null)
    await until(() => taskRows().find((row) => row.id === TASK_T1)?.revision === '7', '快照渲染 rev 7')
    await until(() => !indicator().includes('非实时'), '指示器 realtime')

    // 断线降级：SSE 不再管理投影 → App 立刻用普通 GET 兜底续租（诚实非实时）
    FakeEventSource.newest.emit('error', {})
    await until(() => indicator().includes('非实时'), '降级后指示非实时')
    await until(() => harness.pending('tasks', { paged: false }).length > 0, '降级兜底 GET')
    // 缺陷点：这一路原先是 `tasksSnapshot.value = page.items` 整表替换，
    // 绕过了快照方向的单调保护，旧 revision 直接把表格打回 rev 3
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '3')], null)
    await settle()
    expect(taskRows().find((row) => row.id === TASK_T1)?.revision).toBe('7')
    expect(indicator()).toContain('非实时')
  })

  test('普通 GET 方向的设备兜底同样单调：旧页不得把已投影的设备打回旧版本', async () => {
    const harness = makeHarness({ auto: { tasks: EMPTY_PAGE } })
    renderApp()
    await until(() => harness.count('devices') >= 1, '设备首屏普通 GET')
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '设备新版', '9')])
    await until(() => screen.queryByText('设备新版') !== null, '渲染 rev 9')

    // 切到任务页再切回：设备投影按当前所有权刷新一次（SSE 未 open → 仍走普通 GET）
    await fireEvent.click(screen.getByRole('button', { name: '任务' }))
    await settle()
    await fireEvent.click(screen.getByRole('button', { name: '设备' }))
    await until(() => harness.count('devices') >= 2, '回到设备页再次普通 GET')
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '设备旧版', '4')])
    await settle()
    expect(screen.queryByText('设备旧版')).toBeNull()
    expect(screen.queryByText('设备新版')).not.toBeNull()
  })
})

describe('P2-b：合法空快照 [] 上的聚合失败必须可见且可重试（复审 P2）', () => {
  test('首屏是合法空列表后整轮失败：空表格不得停在无痕空白，重试必须真的再发一轮', async () => {
    window.location.hash = '#/tasks'
    const harness = makeHarness()
    renderApp()
    await until(() => FakeEventSource.instances.length === 1, '订阅创建')
    FakeEventSource.newest.emit('open')
    await settle()
    // 第一轮成功提交，且任务首屏是**合法空快照**：没有任何用户错误，指示器 realtime
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '实时设备A', '1')])
    harness.resolveWith(harness.last('tasks', { paged: false }), [], null)
    await until(() => !indicator().includes('非实时'), '空快照也是合法提交 → realtime')
    expect(hasProjectionError()).toBe(false)

    // 第二轮：设备来源 500 连坐整轮原子不提交（任务来源其实取到了数据）
    FakeEventSource.newest.emitData('device.updated', { device_id: DEVICE_A, revision: '5' })
    await until(() => harness.pending('devices').length > 0, '重同步 GET')
    harness.rejectWith(harness.last('devices'), 500)
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '2')], null)
    await settle()

    // 缺陷点：判据原先只看 `tasksSnapshot === null`，空快照 [] 这一路既不落错误也没有重试入口，
    // 用户面对空表格既不知道失败了、也点不出重试（SSE 管理期组件又不会自发 GET）
    await until(() => hasProjectionError(), '空快照上的整轮失败可见')
    expect(taskRows().some((row) => row.id === TASK_T1)).toBe(false)
    expect(indicator()).toContain('非实时')

    const before = harness.count('tasks', { paged: false })
    const devicesBefore = harness.count('devices')
    await fireEvent.click(screen.getByRole('button', { name: '重试' }))
    await until(() => harness.count('tasks', { paged: false }) > before &&
      harness.count('devices') > devicesBefore, '重试真的重新发起整轮快照')

    // 重试成功后空表格恢复数据，错误提示随新快照清除
    harness.resolveWith(harness.last('devices'), [deviceItem(DEVICE_A, '实时设备A', '1')])
    harness.resolveWith(harness.last('tasks', { paged: false }), [taskItem(TASK_T1, '2')], null)
    await until(() => taskRows().some((row) => row.id === TASK_T1), '重试后列表恢复')
    expect(hasProjectionError()).toBe(false)
  })
})

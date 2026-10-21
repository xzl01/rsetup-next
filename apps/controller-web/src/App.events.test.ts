import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest'

import App from './App.vue'

/**
 * AT-18 / 06 §4：App 与设备/任务数据流接入 SSE 客户端的行为回归。
 *
 * 这里刻意用「真实的 events.ts 客户端 + 注入的 EventSource 替身」跑通整条链路，
 * 断言的是接线契约：谁在什么时候创建/关闭订阅、事件只作为失效提示、非实时绝不伪装实时。
 * 服务端尚未实现事件发布方，因此本文件不构成端到端验收。
 */

const USER_ID = '01234567-89ab-4cde-8f01-234567890abc'
const RID = '123e4567-e89b-42d3-a456-426614174000'
const DEVICE_A = 'a'.repeat(64)
const DEVICE_B = 'b'.repeat(64)

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
  closeCount = 0

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
    for (const listener of this.listeners.get(type) ?? []) listener(event)
  }

  emitData(type: string, data: unknown): void {
    this.emit(type, { data: JSON.stringify(data) })
  }

  close(): void {
    this.closed = true
    this.closeCount += 1
  }

  static last(): FakeEventSource {
    const instance = FakeEventSource.instances[FakeEventSource.instances.length - 1]
    if (instance === undefined) throw new Error('no EventSource instance created')
    return instance
  }
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

interface HarnessOptions {
  meResponses?: Array<Response>
  devicePages?: Array<Array<{ device_id: string; display_name: string; effective_permissions: string[] }>>
}

interface Harness {
  urls: string[]
  deviceRequests: () => number
  meRequests: () => number
}

function stubFetch(options: HarnessOptions = {}): Harness {
  const urls: string[] = []
  let meIndex = 0
  let deviceIndex = 0
  const meResponses = options.meResponses ?? []
  const devicePages = options.devicePages ?? []

  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input)
    urls.push(url)
    if (url.includes('/auth/me')) {
      const custom = meResponses[meIndex] ?? meResponses[meResponses.length - 1]
      meIndex += 1
      if (custom !== undefined) return custom
      return jsonResponse({ data: ME_DATA, request_id: RID })
    }
    if (url.includes('/auth/logout')) return jsonResponse({ data: { logged_out: true }, request_id: RID })
    if (url.includes('/api/v1/devices')) {
      const items = devicePages[deviceIndex] ?? devicePages[devicePages.length - 1] ?? []
      deviceIndex += 1
      return jsonResponse({ data: { items, next_cursor: null }, request_id: RID })
    }
    if (url.includes('/api/v1/tasks')) {
      return jsonResponse({ data: { items: [], next_cursor: null }, request_id: RID })
    }
    throw new TypeError(`unexpected fetch ${url}`)
  }))

  return {
    urls,
    deviceRequests: () => urls.filter((u) => u.includes('/api/v1/devices')).length,
    meRequests: () => urls.filter((u) => u.includes('/auth/me')).length,
  }
}

const deviceA = [{ device_id: DEVICE_A, display_name: '实时设备A', effective_permissions: ['device.read'] }]

function indicatorText(): string {
  return screen.getByTestId('event-connection-state').textContent ?? ''
}

beforeEach(() => {
  localStorage.clear()
  document.documentElement.removeAttribute('lang')
  window.location.hash = '#/devices'
  FakeEventSource.instances = []
})

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
  document.documentElement.removeAttribute('lang')
  window.location.hash = ''
})

describe('SSE 接入 App 数据流', () => {
  test('未认证会话绝不创建 EventSource', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    stubFetch({ meResponses: [jsonResponse(
      { error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: RID },
      401,
    )] })
    render(App)
    await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy())
    expect(FakeEventSource.instances).toHaveLength(0)
  })

  test('强制改密会话绝不创建 EventSource', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    stubFetch({
      meResponses: [jsonResponse({
        data: { ...ME_DATA, user: { ...ME_DATA.user, must_change_password: true } },
        request_id: RID,
      })],
    })
    render(App)
    await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy())
    expect(FakeEventSource.instances).toHaveLength(0)
  })

  test('已认证会话创建订阅：URL 无 query 与 token；open 之前不发 SSE 快照 GET，open 之后才发', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    const harness = stubFetch({ devicePages: [deviceA] })
    render(App)
    await vi.waitFor(() => expect(screen.getByText('实时设备A')).toBeTruthy())

    expect(FakeEventSource.instances).toHaveLength(1)
    expect(FakeEventSource.last().url).not.toContain('?')
    expect(FakeEventSource.last().url).not.toContain('token')
    // 尚未 open：列表数据只来自普通 GET 兜底，指示器绝不显示实时
    expect(harness.deviceRequests()).toBe(1)
    expect(indicatorText()).toContain('非实时')

    FakeEventSource.last().emit('open')
    await vi.waitFor(() => expect(harness.deviceRequests()).toBe(2))
    // 快照回写完成后才标实时
    await vi.waitFor(() => expect(indicatorText()).toContain('数据已同步'))
    expect(indicatorText()).not.toContain('非实时')
  })

  test('未知成员的设备提示不创建可见成员，只按当前授权重新 GET', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    const harness = stubFetch({ devicePages: [deviceA] })
    render(App)
    await vi.waitFor(() => expect(screen.getByText('实时设备A')).toBeTruthy())
    FakeEventSource.last().emit('open')
    await vi.waitFor(() => expect(indicatorText()).toContain('数据已同步'))
    const before = harness.deviceRequests()

    FakeEventSource.last().emitData('device.updated', { device_id: DEVICE_B, display_name: '未授权设备B', revision: '9' })
    // 事件不是事实来源：不凭提示渲染列表成员，只触发当前授权 GET 重取
    await vi.waitFor(() => expect(harness.deviceRequests()).toBe(before + 1))
    expect(screen.queryByText('未授权设备B')).toBeNull()
  })

  test('撤权后不再展示旧授权设备，并走现有重新鉴权路径', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    const harness = stubFetch({ devicePages: [deviceA, []] })
    render(App)
    await vi.waitFor(() => expect(screen.getByText('实时设备A')).toBeTruthy())
    FakeEventSource.last().emit('open')
    await vi.waitFor(() => expect(indicatorText()).toContain('数据已同步'))
    const meBefore = harness.meRequests()

    FakeEventSource.last().emit('permissions.changed')
    await vi.waitFor(() => expect(screen.queryByText('实时设备A')).toBeNull())
    expect(harness.meRequests()).toBeGreaterThan(meBefore)
    // 旧订阅已关闭；重同步期间的连接状态绝不显示实时
    expect(FakeEventSource.instances[0]!.closed).toBe(true)
    expect(indicatorText()).toContain('非实时')
  })

  test('缺少 EventSource 能力：诚实显示不可用，普通 GET 读取仍然可用', async () => {
    const harness = stubFetch({ devicePages: [deviceA] })
    render(App)
    await vi.waitFor(() => expect(screen.getByText('实时设备A')).toBeTruthy())
    expect(FakeEventSource.instances).toHaveLength(0)
    expect(indicatorText()).toContain('不支持')
    expect(indicatorText()).not.toContain('数据已同步')
    expect(harness.deviceRequests()).toBe(1)
  })

  test('登出立即关闭订阅并清除实时标识', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    stubFetch({ devicePages: [deviceA] })
    render(App)
    await vi.waitFor(() => expect(screen.getByText('实时设备A')).toBeTruthy())
    FakeEventSource.last().emit('open')
    await vi.waitFor(() => expect(indicatorText()).toContain('数据已同步'))

    await fireEvent.click(screen.getByRole('button', { name: '退出登录' }))
    await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy())
    expect(FakeEventSource.instances[0]!.closed).toBe(true)
  })

  test('组件卸载关闭订阅', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    stubFetch({ devicePages: [deviceA] })
    const { unmount } = render(App)
    await vi.waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    unmount()
    expect(FakeEventSource.last().closed).toBe(true)
    expect(FakeEventSource.last().closeCount).toBe(1)
  })

  test('任务首屏在 open 前后各自只有一个获取者：接管前普通 GET 兜底、接管后快照唯一', async () => {
    vi.stubGlobal('EventSource', FakeEventSource)
    const harness = stubFetch({ devicePages: [deviceA] })
    window.location.hash = '#/tasks'
    const taskRequests = () => harness.urls.filter((u) => u.includes('/api/v1/tasks'))
    render(App)
    // 审查第 4 项修正：`active` 只代表订阅存在，不代表实时。connecting 期间诚实非实时，
    // 任务首屏必须由普通 GET 兜底取到（旧断言的「一条都不发」正是列表凭空消失的缺陷）。
    await vi.waitFor(() => expect(taskRequests().length).toBe(1))
    expect(indicatorText()).toContain('非实时')

    await vi.waitFor(() => expect(FakeEventSource.instances).toHaveLength(1))
    FakeEventSource.last().emit('open')
    await vi.waitFor(() => expect(indicatorText()).toContain('数据已同步'))
    // open 当刻 SSE 接管：兜底 GET 被作废，此后首屏只由快照来源提供，不再叠加第三条 GET
    expect(taskRequests().length).toBe(2)
    // 首屏语义：绝不自动翻页
    expect(taskRequests().filter((u) => u.includes('cursor='))).toHaveLength(0)
  })
})

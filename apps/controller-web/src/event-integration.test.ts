import { describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'

import {
  createEventIntegration,
  resolveConnectionTone,
  type EventConnectionTone,
  type EventIntegration,
} from './event-integration'
import type { AuthStatus, AuthStore, AuthUser } from './auth'
import type { EntityKind, EventSourceLike, ShutdownReason, SnapshotSource } from './events'

/**
 * AT-18 / 06 §4 集成层行为回归：
 * 本文件只验证「SSE 客户端与 App 会话/授权生命周期」的接线契约，
 * 五种命名事件、代际门禁与水位语义本身由 events.test.ts 负责。
 */

type Listener = (event: unknown) => void

class FakeEventSource {
  readonly listeners = new Map<string, Listener[]>()
  closed = false
  closeCount = 0
  readonly url: string
  readonly init: { withCredentials: boolean }

  constructor(url: string, init: { withCredentials: boolean }) {
    this.url = url
    this.init = init
    instances.push(this)
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
    this.emit(type, { data: typeof data === 'string' ? data : JSON.stringify(data) })
  }

  close(): void {
    this.closed = true
    this.closeCount += 1
  }
}

let instances: FakeEventSource[] = []

function latest(): FakeEventSource {
  const instance = instances[instances.length - 1]
  if (instance === undefined) throw new Error('no EventSource instance created')
  return instance
}

async function flush(times = 24): Promise<void> {
  for (let index = 0; index < times; index += 1) await Promise.resolve()
}

const DEVICE_A = 'a'.repeat(64)
const TASK_1 = 'task-1'

function deviceItems(...ids: string[]): Array<Record<string, unknown>> {
  return ids.map((id) => ({
    device_id: id,
    display_name: `设备 ${id.slice(0, 3)}`,
    effective_permissions: ['device.read'],
    revision: '1',
  }))
}

function taskItems(...ids: string[]): Array<Record<string, unknown>> {
  return ids.map((id) => ({ id, operation: 'reboot', revision: '1' }))
}

interface Harness {
  integration: EventIntegration
  auth: Pick<AuthStore, 'status' | 'user' | 'authzEpoch' | 'csrfToken'>
  devices: ReturnType<typeof vi.fn>
  tasks: ReturnType<typeof vi.fn>
  snapshotKinds: EntityKind[]
  invalidations: Array<ReadonlyArray<EntityKind>>
  /** 直发的对象级失效提示（kind + id）：覆盖面比 invalidations 更宽，含未知成员。 */
  updateHints: Array<{ kind: EntityKind; id: string }>
  permissionsChanged: ReturnType<typeof vi.fn>
  sessionReasons: ShutdownReason[]
  errorCodes: string[]
}

function setup(options: {
  status?: AuthStatus
  user?: AuthUser | null
  epoch?: string | null
  token?: string | null
  deviceSnapshot?: () => Promise<Array<Record<string, unknown>>>
  taskSnapshot?: () => Promise<Array<Record<string, unknown>>>
  withEventSource?: boolean
} = {}): Harness {
  const auth = {
    status: ref<AuthStatus>(options.status ?? 'signed_in'),
    user: ref<AuthUser | null>(options.user === undefined
      ? { id: '01234567-89ab-4cde-8f01-234567890abc', username: 'admin', must_change_password: false, revision: '1' }
      : options.user),
    authzEpoch: ref<string | null>(options.epoch === undefined ? '7' : options.epoch),
    csrfToken: ref<string | null>(options.token === undefined ? 'mem-token' : options.token),
  }

  const devices = vi.fn(options.deviceSnapshot ?? (async () => deviceItems(DEVICE_A)))
  const tasks = vi.fn(options.taskSnapshot ?? (async () => taskItems(TASK_1)))
  const sources: SnapshotSource[] = [
    { kind: 'device', fetch: async () => devices() },
    { kind: 'task', fetch: async () => tasks() },
  ]

  const snapshotKinds: EntityKind[] = []
  const invalidations: Array<ReadonlyArray<EntityKind>> = []
  const updateHints: Array<{ kind: EntityKind; id: string }> = []
  const sessionReasons: ShutdownReason[] = []
  const errorCodes: string[] = []
  const permissionsChanged = vi.fn()
  const factory = options.withEventSource === false
    ? undefined
    : (url: string, init: { withCredentials: boolean }) =>
        new FakeEventSource(url, init) as unknown as EventSourceLike

  const integration = createEventIntegration({
    auth,
    snapshots: sources,
    ...(factory === undefined ? {} : { eventSourceFactory: factory }),
    hasEventSource: () => options.withEventSource !== false,
    onSnapshot: (kind) => { snapshotKinds.push(kind) },
    onInvalidation: (kinds) => { invalidations.push(kinds) },
    onUpdateHint: (kind, id) => { updateHints.push({ kind, id }) },
    onPermissionsChanged: permissionsChanged,
    onSessionExpired: (reason) => { sessionReasons.push(reason) },
    onError: (error) => { errorCodes.push(error.code) },
  })

  return {
    integration,
    auth,
    devices,
    tasks,
    snapshotKinds,
    invalidations,
    updateHints,
    permissionsChanged,
    sessionReasons,
    errorCodes,
  }
}

describe('createEventIntegration：会话与授权代际门禁', () => {
  it('仅在已认证且非强制改密会话创建真实 EventSource', async () => {
    instances = []
    const harness = setup({ status: 'signed_out' })
    await flush()
    expect(instances).toHaveLength(0)
    expect(harness.integration.active.value).toBe(false)
    harness.integration.dispose()
  })

  it('强制改密会话（force_password 或 must_change_password）绝不创建 EventSource', async () => {
    instances = []
    const forced = setup({ status: 'force_password' })
    await flush()
    expect(instances).toHaveLength(0)
    forced.integration.dispose()

    instances = []
    const flagged = setup({
      user: { id: '01234567-89ab-4cde-8f01-234567890abc', username: 'admin', must_change_password: true, revision: '1' },
    })
    await flush()
    expect(instances).toHaveLength(0)
    expect(flagged.integration.active.value).toBe(false)
    flagged.integration.dispose()
  })

  it('已认证会话创建订阅：URL 无 query/token，Cookie 凭据 withCredentials', async () => {
    instances = []
    const harness = setup()
    await flush()
    expect(instances).toHaveLength(1)
    expect(latest().url).not.toContain('?')
    expect(latest().url).not.toContain('token')
    expect(latest().init.withCredentials).toBe(true)
    harness.integration.dispose()
  })

  it('登出即关闭订阅并清除实时标识', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    expect(harness.integration.isRealtime.value).toBe(true)

    harness.auth.status.value = 'signed_out'
    await flush()
    expect(latest().closed).toBe(true)
    expect(harness.integration.active.value).toBe(false)
    expect(harness.integration.isRealtime.value).toBe(false)
    harness.integration.dispose()
  })

  it('authz_epoch 前进：旧订阅 close，新订阅创建', async () => {
    instances = []
    const harness = setup()
    await flush()
    expect(instances).toHaveLength(1)
    harness.auth.authzEpoch.value = '8'
    await flush()
    expect(instances[0]!.closed).toBe(true)
    expect(instances).toHaveLength(2)
    harness.integration.dispose()
  })

  it('认证主体切换：旧订阅 close，新订阅创建', async () => {
    instances = []
    const harness = setup()
    await flush()
    harness.auth.user.value = {
      id: '11111111-89ab-4cde-8f01-234567890abc',
      username: 'other',
      must_change_password: false,
      revision: '1',
    }
    await flush()
    expect(instances[0]!.closed).toBe(true)
    expect(instances).toHaveLength(2)
    harness.integration.dispose()
  })

  it('dispose 关闭订阅并停止后续代际响应', async () => {
    instances = []
    const harness = setup()
    await flush()
    harness.integration.dispose()
    expect(latest().closed).toBe(true)
    harness.auth.authzEpoch.value = '9'
    await flush()
    expect(instances).toHaveLength(1)
  })
})

describe('createEventIntegration：自动重建预算跨 auth.refresh 的 checking 瞬时态保留（审查第 1 项）', () => {
  it('撤权 → 重新鉴权（含 checking）循环不得每次回满预算，重建总数有上限', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()

    // 真实 auth.refresh() 的第一步就是同步 status='checking'，接线层必然看到一次「身份为空」
    for (let round = 0; round < 5; round += 1) {
      const alive = [...instances].reverse().find((instance) => !instance.closed)
      if (alive === undefined) break
      alive.emit('permissions.changed')
      await flush()
      harness.auth.status.value = 'checking'
      await flush()
      harness.auth.status.value = 'signed_in'
      await flush()
    }

    // 1 次初建 + 最多 MAX_AUTO_RESTARTS(3) 次自动重建；checking 不得被当成新一代际而回满
    expect(instances.length).toBeLessThanOrEqual(4)
    expect(instances.filter((instance) => !instance.closed).length).toBeLessThanOrEqual(1)
    harness.integration.dispose()
  })

  it('预算耗尽后：认证主体真正切换重新发放，同主体保持停止', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()

    for (let round = 0; round < 5; round += 1) {
      const alive = [...instances].reverse().find((instance) => !instance.closed)
      if (alive === undefined) break
      alive.emit('permissions.changed')
      await flush()
    }
    const exhausted = instances.length
    expect(exhausted).toBeLessThanOrEqual(4)
    expect(instances[exhausted - 1]!.closed).toBe(true)

    // 同主体再撤权一次：仍不得重建（持续 401/permissions 不能无界重连）
    harness.auth.authzEpoch.value = '8'
    await flush()
    expect(instances).toHaveLength(exhausted)

    // 真正换了认证主体：预算重新发放，必须恢复订阅
    instances = []
    harness.auth.user.value = {
      id: '22222222-89ab-4cde-8f01-234567890abc',
      username: 'other',
      must_change_password: false,
      revision: '1',
    }
    await flush()
    expect(instances).toHaveLength(1)
    expect(instances[0]!.closed).toBe(false)
    harness.integration.dispose()
  })

  it('用户显式 reconnect() 是预算重新发放的另一个入口', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    for (let round = 0; round < 5; round += 1) {
      const alive = [...instances].reverse().find((instance) => !instance.closed)
      if (alive === undefined) break
      alive.emit('permissions.changed')
      await flush()
    }
    const exhausted = instances.length
    expect(instances[exhausted - 1]!.closed).toBe(true)

    expect(harness.integration.reconnect()).toBe(true)
    await flush()
    expect(instances).toHaveLength(exhausted + 1)
    expect(instances[exhausted]!.closed).toBe(false)
    harness.integration.dispose()
  })
})

describe('createEventIntegration：CSRF token 轮换属于认证代际（审查第 5 项）', () => {
  it('同主体同 epoch 仅换 token：旧订阅 close、新订阅重建', async () => {
    instances = []
    const harness = setup()
    await flush()
    expect(instances).toHaveLength(1)

    harness.auth.csrfToken.value = 'rotated-token'
    await flush()
    expect(instances[0]!.closed).toBe(true)
    expect(instances).toHaveLength(2)

    // token 再换一次仍是新一代际（不能因为主体/epoch 没变就跳过重建）
    harness.auth.csrfToken.value = 'rotated-token-2'
    await flush()
    expect(instances[1]!.closed).toBe(true)
    expect(instances).toHaveLength(3)
    harness.integration.dispose()
  })

  it('合法代际变化不消耗撤权预算；撤权重建仍受同一上限约束', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()

    // 合法轮换两次（重新鉴权后服务端轮换 CSRF token）：不是失效 episode，不得吃掉撤权预算
    harness.auth.csrfToken.value = 'token-a'
    await flush()
    harness.auth.csrfToken.value = 'token-b'
    await flush()
    const afterRotations = instances.length
    expect(afterRotations).toBe(3)

    // 之后的撤权重建仍共享同一预算：5 次撤权最多再来 3 个订阅，且最多只剩 1 个活订阅
    for (let round = 0; round < 5; round += 1) {
      const alive = [...instances].reverse().find((instance) => !instance.closed)
      if (alive === undefined) break
      alive.emit('permissions.changed')
      await flush()
    }
    expect(instances.length).toBeLessThanOrEqual(afterRotations + 3)
    expect(instances.filter((instance) => !instance.closed).length).toBeLessThanOrEqual(1)
    harness.integration.dispose()
  })
})

describe('createEventIntegration：快照 GET 只在实际 open 之后', () => {
  it('open 之前不发起任何快照 GET，open 之后各来源一次', async () => {
    instances = []
    const harness = setup()
    await flush()
    expect(harness.devices).not.toHaveBeenCalled()
    expect(harness.tasks).not.toHaveBeenCalled()
    expect(harness.integration.isRealtime.value).toBe(false)

    latest().emit('open')
    await flush()
    expect(harness.devices).toHaveBeenCalledTimes(1)
    expect(harness.tasks).toHaveBeenCalledTimes(1)
  })

  it('同步完成且连接已建立才标实时；断线立即回落非实时且期间不再 GET', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    expect(harness.integration.isRealtime.value).toBe(true)
    expect(harness.integration.tone.value).toBe('realtime')

    latest().emit('error', {})
    await flush()
    expect(harness.integration.isRealtime.value).toBe(false)
    expect(harness.integration.tone.value).toBe('reconnecting')
    const devicesAfterError = harness.devices.mock.calls.length
    await flush()
    expect(harness.devices).toHaveBeenCalledTimes(devicesAfterError)
    harness.integration.dispose()
  })

  it('重连 open 之后重新发起全量快照 GET', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    latest().emit('error', {})
    await flush()
    latest().emit('open')
    await flush()
    expect(harness.devices).toHaveBeenCalledTimes(2)
    expect(harness.integration.isRealtime.value).toBe(true)
    harness.integration.dispose()
  })
})

describe('createEventIntegration：事件只是失效提示', () => {
  it('同批多次对象提示合并为一次失效回调（不逐条打补丁）', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    harness.invalidations.length = 0

    latest().emitData('device.updated', { device_id: DEVICE_A, revision: '2' })
    latest().emitData('device.updated', { device_id: DEVICE_A, revision: '3' })
    latest().emitData('task.updated', { id: TASK_1, revision: '2' })
    await flush()

    expect(harness.invalidations).toHaveLength(1)
    expect([...harness.invalidations[0]!].sort()).toEqual(['device', 'task'])
    harness.integration.dispose()
  })

  it('未知成员的提示同样透传给宿主：不属于列表投影的读取才有失效来源（审查 P1）', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    harness.invalidations.length = 0
    harness.updateHints.length = 0

    // TASK_9 不在当前快照成员中：events.ts 拒绝按补丁分发（invalidations 仍为空），
    // 但提示必须原样透传，否则宿主上的任务详情对这类对象彻底失明
    latest().emitData('task.updated', { id: 'task-9', revision: '2' })
    await flush()

    expect(harness.updateHints).toEqual([{ kind: 'task', id: 'task-9' }])
    expect(harness.invalidations).toHaveLength(0)
    harness.integration.dispose()
  })

  it('requestResync：未被 SSE 管理返回 false，open 之后触发新一轮快照 GET', async () => {
    instances = []
    const harness = setup()
    await flush()
    expect(harness.integration.requestResync()).toBe(false)
    latest().emit('open')
    await flush()
    expect(harness.integration.requestResync()).toBe(true)
    await flush()
    expect(harness.devices).toHaveBeenCalledTimes(2)
    harness.integration.dispose()
  })
})

describe('createEventIntegration：撤权与会话失效', () => {
  it('permissions.changed 走宿主重新鉴权，且限次自动重连不永远卡住', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    expect(harness.integration.isRealtime.value).toBe(true)

    latest().emit('permissions.changed')
    await flush()
    expect(harness.permissionsChanged).toHaveBeenCalledTimes(1)
    expect(harness.integration.isRealtime.value).toBe(false)
    // 会话仍然有效时自动重建订阅（有上限），不永远停在失效态
    expect(instances.length).toBeGreaterThanOrEqual(2)
    expect(instances[0]!.closed).toBe(true)
    harness.integration.dispose()
  })

  it('持续撤权时自动重连有上限，绝不无界自旋重连', async () => {
    instances = []
    const harness = setup()
    await flush()
    latest().emit('open')
    await flush()
    // 反复触发失效：初始 1 次 + 最多 3 次自动重建
    for (let index = 0; index < 8; index += 1) {
      latest().emit('permissions.changed')
      await flush()
    }
    expect(instances.length).toBeLessThanOrEqual(4)
    expect(harness.integration.isRealtime.value).toBe(false)
    harness.integration.dispose()
  })

  it('快照 GET 返回 401：宿主收到会话失效回调并保持非实时', async () => {
    instances = []
    const unauthorized = Object.assign(new Error('AUTH_REQUIRED'), { code: 'AUTH_REQUIRED', status: 401 })
    const harness = setup({ deviceSnapshot: async () => { throw unauthorized } })
    await flush()
    latest().emit('open')
    await flush()
    expect(harness.integration.isRealtime.value).toBe(false)
    expect(harness.sessionReasons.length).toBeGreaterThan(0)
    expect(harness.sessionReasons[0]).toBe('http-401')
    harness.integration.dispose()
  })
})

describe('createEventIntegration：缺失 SSE 能力时诚实降级', () => {
  it('无 EventSource 能力：不构造订阅、tone=unsupported、resync 返回 false', async () => {
    instances = []
    const harness = setup({ withEventSource: false })
    await flush()
    expect(instances).toHaveLength(0)
    expect(harness.integration.supported.value).toBe(false)
    expect(harness.integration.tone.value).toBe('unsupported')
    expect(harness.integration.requestResync()).toBe(false)
    expect(harness.integration.isRealtime.value).toBe(false)
    harness.integration.dispose()
  })
})

describe('resolveConnectionTone', () => {
  const cases: Array<[EventConnectionTone, Parameters<typeof resolveConnectionTone>[0]]> = [
    ['unsupported', { supported: false, active: true, connectionState: 'open', syncState: 'synced', isRealtime: false }],
    ['unauthorized', { supported: true, active: false, connectionState: 'unauthorized', syncState: 'expired', isRealtime: false }],
    ['disconnected', { supported: true, active: false, connectionState: 'closed', syncState: 'idle', isRealtime: false }],
    ['disconnected', { supported: true, active: false, connectionState: null, syncState: null, isRealtime: false }],
    ['connecting', { supported: true, active: true, connectionState: 'connecting', syncState: 'idle', isRealtime: false }],
    ['reconnecting', { supported: true, active: true, connectionState: 'reconnecting', syncState: 'stale', isRealtime: false }],
    ['syncing', { supported: true, active: true, connectionState: 'open', syncState: 'syncing', isRealtime: false }],
    ['stale', { supported: true, active: true, connectionState: 'open', syncState: 'stale', isRealtime: false }],
    ['realtime', { supported: true, active: true, connectionState: 'open', syncState: 'synced', isRealtime: true }],
  ]

  it.each(cases)('%s', (expected, input) => {
    expect(resolveConnectionTone(input)).toBe(expected)
  })

  it('isRealtime 为真但连接未 open 或同步未完成时绝不判定实时', () => {
    expect(resolveConnectionTone({
      supported: true, active: true, connectionState: 'reconnecting', syncState: 'synced', isRealtime: true,
    })).toBe('reconnecting')
    expect(resolveConnectionTone({
      supported: true, active: true, connectionState: 'open', syncState: 'syncing', isRealtime: true,
    })).toBe('syncing')
  })
})

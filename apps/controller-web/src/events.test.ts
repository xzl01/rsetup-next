import { afterEach, describe, expect, it, vi, type Mock } from 'vitest'

import {
  createEventStreamClient,
  compareRevision,
  SSE_EVENT_NAMES,
  type EntityKind,
  type EventSourceFactory,
  type EventSourceLike,
  type EventStreamClient,
  type SnapshotSource,
} from './events'

type Listener = (event: unknown) => void

/** 纯 JS EventSource 替身：Node/Vitest 下注入，逐条驱动命名事件契约。 */
class FakeEventSource implements EventSourceLike {
  readonly listeners = new Map<string, Listener[]>()
  closed = false
  closeCount = 0

  addEventListener(type: string, listener: Listener): void {
    const existing = this.listeners.get(type)
    if (existing === undefined) {
      this.listeners.set(type, [listener])
      return
    }
    existing.push(listener)
  }

  emit(type: string, event: unknown = {}): void {
    for (const listener of this.listeners.get(type) ?? []) listener(event)
  }

  emitData(type: string, data: unknown): void {
    this.emit(type, { data: typeof data === 'string' ? data : JSON.stringify(data) })
  }

  listenerCount(type: string): number {
    return (this.listeners.get(type) ?? []).length
  }

  get types(): string[] {
    return [...this.listeners.keys()]
  }

  close(): void {
    this.closed = true
    this.closeCount += 1
  }
}

interface Deferred<T> {
  promise: Promise<T>
  resolve: (value: T) => void
  reject: (reason: unknown) => void
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

/** 多轮微任务刷新：Promise.allSettled + 连续 await 链需要多个 tick。 */
async function flush(times = 24): Promise<void> {
  for (let index = 0; index < times; index += 1) await Promise.resolve()
}

function deviceItem(id: string, revision: string): Record<string, unknown> {
  return { device_id: id, display_name: `设备 ${id}`, revision }
}

function taskItem(id: string, revision: string): Record<string, unknown> {
  return { id, operation: 'reboot', revision }
}

type SnapshotFetch = (context: { signal: AbortSignal }) => Promise<ReadonlyArray<Record<string, unknown>>>
type SnapshotSpy = Mock<SnapshotFetch>

interface Harness {
  client: EventStreamClient
  es: FakeEventSource
  devices: SnapshotSpy
  tasks: SnapshotSpy
  deviceSignals: AbortSignal[]
  taskSignals: AbortSignal[]
  snapshots: Array<{ kind: EntityKind; count: number }>
  updates: Array<{ kind: EntityKind; patch: Record<string, unknown> }>
  timeEvents: Array<Record<string, unknown> | null>
  expired: string[]
  errors: string[]
  created: FakeEventSource[]
}

interface HarnessOptions {
  deviceSnapshot?: () => Promise<ReadonlyArray<Record<string, unknown>>>
  taskSnapshot?: () => Promise<ReadonlyArray<Record<string, unknown>>>
  snapshotsConfig?: 'both' | 'device-only' | 'none'
  url?: string
  maxBufferSize?: number
  withCredentials?: boolean
  factory?: EventSourceFactory
}

function createHarness(options: HarnessOptions = {}): Harness {
  const es = new FakeEventSource()
  const created: FakeEventSource[] = [es]
  const deviceSignals: AbortSignal[] = []
  const taskSignals: AbortSignal[] = []
  const mode = options.snapshotsConfig ?? 'both'

  const defaultDevices = async () => [deviceItem('dev-1', '5'), deviceItem('dev-2', '10')]
  const defaultTasks = async () => [taskItem('task-1', '3')]

  const deviceFetch: SnapshotFetch = options.deviceSnapshot ?? defaultDevices
  const taskFetch: SnapshotFetch = options.taskSnapshot ?? defaultTasks
  const devices: SnapshotSpy = vi.fn((context) => {
    deviceSignals.push(context.signal)
    return deviceFetch(context)
  })
  const tasks: SnapshotSpy = vi.fn((context) => {
    taskSignals.push(context.signal)
    return taskFetch(context)
  })

  const snapshots: Array<{ kind: EntityKind; count: number }> = []
  const updates: Array<{ kind: EntityKind; patch: Record<string, unknown> }> = []
  const timeEvents: Array<Record<string, unknown> | null> = []
  const expired: string[] = []
  const errors: string[] = []

  const sources: SnapshotSource[] = []
  if (mode !== 'none') {
    sources.push({ kind: 'device', fetch: devices })
    if (mode === 'both') {
      sources.push({ kind: 'task', fetch: tasks })
    }
  }

  const client = createEventStreamClient({
    snapshots: sources,
    ...(options.url !== undefined ? { url: options.url } : {}),
    ...(options.maxBufferSize !== undefined ? { maxBufferSize: options.maxBufferSize } : {}),
    ...(options.withCredentials !== undefined ? { withCredentials: options.withCredentials } : {}),
    eventSourceFactory: options.factory ??
      ((url, init) => {
        const instance = es as EventSourceLike & { url?: string; init?: unknown }
        instance.url = url
        instance.init = init
        return instance
      }),
    onSnapshot: (kind, items) => snapshots.push({ kind, count: items.length }),
    onEntityUpdate: (kind, patch) => updates.push({ kind, patch }),
    onSystemTimeChanged: (payload) => timeEvents.push(payload),
    onSessionExpired: (reason) => expired.push(reason),
    onError: (error) => errors.push(error.code),
  })

  return { client, es, devices, tasks, deviceSignals, taskSignals, snapshots, updates, timeEvents, expired, errors, created }
}

let client: EventStreamClient | null = null

afterEach(() => {
  client?.close()
  client = null
  vi.restoreAllMocks()
})

describe('compareRevision：canonical u64 十进制字符串无损比较（纯函数）', () => {
  it('按十进制长度与字典序无损比较，包含超过 2^53 与 u64 上限的大数', () => {
    expect(compareRevision('0', '0')).toBe(0)
    expect(compareRevision('0', '1')).toBe(-1)
    expect(compareRevision('1', '0')).toBe(1)
    expect(compareRevision('99', '100')).toBe(-1)
    expect(compareRevision('100', '99')).toBe(1)
    const maxU64Minus1 = '18446744073709551614'
    const maxU64 = '18446744073709551615'
    expect(compareRevision(maxU64Minus1, maxU64)).toBe(-1)
    expect(compareRevision(maxU64, maxU64Minus1)).toBe(1)
    expect(compareRevision(maxU64, maxU64)).toBe(0)
    // 不经 Number：2^53+1 与 2^53 在 JS Number 下相等，这里必须仍可区分
    expect(compareRevision('9007199254740993', '9007199254740992')).toBe(1)
  })

  it('非规范输入一律抛错，绝不静默退化为 0', () => {
    const invalid: unknown[] = [
      '', '01', '00', '-1', '1.0', '1e5', ' 5', '5 ', '+5', 'abc', '1\n',
      '18446744073709551616', null, undefined, 123, {}, [], true,
    ]
    for (const value of invalid) {
      expect(() => compareRevision(value as string, '1')).toThrowError('INVALID_REVISION')
      expect(() => compareRevision('1', value as string)).toThrowError('INVALID_REVISION')
    }
  })
})

describe('EventSource 注入与五种命名事件的独立注册', () => {
  it('通过 addEventListener 分别注册五种命名事件，且从不使用默认 message/onmessage', () => {
    const h = createHarness()
    client = h.client
    for (const name of SSE_EVENT_NAMES) {
      expect(h.es.types).toContain(name)
      expect(h.es.listenerCount(name)).toBe(1)
    }
    expect(h.es.types).toContain('open')
    expect(h.es.types).toContain('error')
    expect(h.es.types).not.toContain('message')
    expect((h.es as unknown as { onmessage?: unknown }).onmessage).toBeUndefined()
  })

  it('默认端点为 /api/v1/events，凭据只走同源 Cookie（withCredentials，URL 无 token）', () => {
    const h = createHarness()
    client = h.client
    const instance = h.es as FakeEventSource & { url?: string; init?: { withCredentials?: boolean } }
    expect(instance.url).toBe('/api/v1/events')
    expect(instance.url).not.toContain('token')
    expect(instance.init).toEqual({ withCredentials: true })
  })

  it('支持自定义端点与 withCredentials 关闭', () => {
    const h = createHarness({ url: '/api/v1/events', withCredentials: false })
    client = h.client
    const instance = h.es as FakeEventSource & { url?: string; init?: { withCredentials?: boolean } }
    expect(instance.url).toBe('/api/v1/events')
    expect(instance.init).toEqual({ withCredentials: false })
  })

  it('构造时不建立订阅外的副作用：URL 带 query 或缓冲上限非法直接抛错', () => {
    const ok = new FakeEventSource()
    expect(() => createEventStreamClient({
      url: '/api/v1/events?token=abc',
      eventSourceFactory: () => ok,
    })).toThrowError('INVALID_SSE_URL')
    expect(() => createEventStreamClient({
      maxBufferSize: 0,
      eventSourceFactory: () => ok,
    })).toThrowError('INVALID_BUFFER_SIZE')
    expect(() => createEventStreamClient({
      snapshots: [
        { kind: 'device', fetch: async () => [] },
        { kind: 'device', fetch: async () => [] },
      ],
      eventSourceFactory: () => ok,
    })).toThrowError('DUPLICATE_SNAPSHOT_SOURCE')
    expect(() => createEventStreamClient({
      snapshots: [{ kind: 'unknown-kind' as EntityKind, fetch: async () => [] }],
      eventSourceFactory: () => ok,
    })).toThrowError('INVALID_SNAPSHOT_SOURCE')
  })
})

describe('open 之后才发起全量 GET', () => {
  it('open 前绝不 GET；open 后按 kind 全量 GET，完成后才标实时', async () => {
    const h = createHarness()
    client = h.client
    expect(h.devices).not.toHaveBeenCalled()
    expect(h.tasks).not.toHaveBeenCalled()
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.syncState.value).toBe('idle')

    h.es.emit('open')
    expect(h.devices).toHaveBeenCalledTimes(1)
    expect(h.tasks).toHaveBeenCalledTimes(1)
    expect(h.client.connectionState.value).toBe('open')
    expect(h.client.syncState.value).toBe('syncing')
    expect(h.client.isRealtime.value).toBe(false) // 同步未完成不得标实时

    await flush()
    expect(h.client.syncState.value).toBe('synced')
    expect(h.client.isRealtime.value).toBe(true)
    expect(h.snapshots).toEqual([
      { kind: 'device', count: 2 },
      { kind: 'task', count: 1 },
    ])
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('5')
    expect(h.client.watermarkOf('device', 'dev-2')).toBe('10')
    expect(h.client.watermarkOf('task', 'task-1')).toBe('3')
  })

  it('快照在途期间的连接错误使 isRealtime 保持 false（独立 GET 成功也不能标实时）', async () => {
    const gate = deferred<ReadonlyArray<Record<string, unknown>>>()
    const h = createHarness({ deviceSnapshot: () => gate.promise })
    client = h.client
    h.es.emit('open')
    h.es.emit('error')
    gate.resolve([deviceItem('dev-1', '9')])
    await flush()
    expect(h.snapshots).toHaveLength(0)
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.connectionState.value).toBe('reconnecting')
    expect(h.client.watermarkOf('device', 'dev-1')).toBeNull()
  })

  it('断线期间 reset 不发起 GET，重连 open 后重新全量同步并恢复实时', async () => {
    const h = createHarness()
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.client.isRealtime.value).toBe(true)

    h.es.emit('error')
    expect(h.client.isRealtime.value).toBe(false)
    h.es.emit('reset')
    await flush()
    expect(h.devices).toHaveBeenCalledTimes(1) // 断线期不 GET

    h.es.emit('open')
    expect(h.devices).toHaveBeenCalledTimes(2)
    await flush()
    expect(h.client.isRealtime.value).toBe(true)
  })
})

describe('五种命名事件的独立分发', () => {
  async function readyHarness(): Promise<Harness> {
    const h = createHarness()
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.client.isRealtime.value).toBe(true)
    return h
  }

  it('device.updated 独立分发到 device 投影', async () => {
    const h = await readyHarness()
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '6', display_name: '新名' })
    expect(h.updates).toHaveLength(1)
    expect(h.updates[0]!.kind).toBe('device')
    expect(h.updates[0]!.patch).toMatchObject({ device_id: 'dev-1', revision: '6' })
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('6')
    expect(h.tasks).toHaveBeenCalledTimes(1) // 对象补丁不触发全量重取
  })

  it('task.updated 独立分发到 task 投影，不影响 device', async () => {
    const h = await readyHarness()
    h.es.emitData('task.updated', { id: 'task-1', revision: '4', state: 'running' })
    expect(h.updates).toEqual([{ kind: 'task', patch: { id: 'task-1', revision: '4', state: 'running' } }])
    expect(h.client.watermarkOf('task', 'task-1')).toBe('4')
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('5')
  })

  it('system.time.changed 仅作为提示：回调 + 重新全量 GET，不当作对象补丁', async () => {
    const h = await readyHarness()
    h.es.emitData('system.time.changed', { quality: 'ntp_valid' })
    expect(h.timeEvents).toEqual([{ quality: 'ntp_valid' }])
    expect(h.updates).toHaveLength(0)
    expect(h.client.lastSyncReason.value).toBe('system-time-changed')
    expect(h.client.syncGeneration.value).toBe(2)
    await flush()
    expect(h.tasks).toHaveBeenCalledTimes(2)
    expect(h.client.isRealtime.value).toBe(true)
  })

  it('system.time.changed 载荷缺省（空 data）也按提示处理', async () => {
    const h = await readyHarness()
    h.es.emit('system.time.changed', { data: '' })
    expect(h.timeEvents).toEqual([null])
    expect(h.client.lastSyncReason.value).toBe('system-time-changed')
  })

  it('reset 使旧同步代际失效：水位清零、取消在途 GET、重新全量 GET', async () => {
    const first = deferred<ReadonlyArray<Record<string, unknown>>>()
    const second = [deviceItem('dev-1', '20')]
    let calls = 0
    const h = createHarness({
      snapshotsConfig: 'device-only',
      deviceSnapshot: () => {
        calls += 1
        return calls === 1 ? first.promise : Promise.resolve(second)
      },
    })
    client = h.client
    h.es.emit('open')
    expect(h.deviceSignals[0]).toBeDefined()
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '99' }) // 同步中先缓冲
    h.es.emit('reset')
    expect(h.deviceSignals[0]!.aborted).toBe(true) // 在途旧 GET 被取消
    expect(h.client.watermarkOf('device', 'dev-1')).toBeNull() // 水位已重置
    expect(h.devices).toHaveBeenCalledTimes(2)
    expect(h.client.syncGeneration.value).toBe(2)

    first.resolve([deviceItem('dev-1', '1')]) // 旧代际响应迟到
    await flush()
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('20') // 绝不是旧快照的 1
    expect(h.client.isRealtime.value).toBe(true)
    // 旧代际缓冲随 reset 一起作废，水位只由新快照重建
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('20')
  })

  it('permissions.changed 立即关闭订阅、清理授权投影并触发重新鉴权', async () => {
    const h = await readyHarness()
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('5')
    h.es.emit('permissions.changed')
    expect(h.expired).toEqual(['permissions.changed'])
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.connectionState.value).toBe('unauthorized')
    expect(h.client.syncState.value).toBe('expired')
    expect(h.es.closed).toBe(true)
    expect(h.client.watermarkOf('device', 'dev-1')).toBeNull()
    // 撤权后同连接上的后续事件一律忽略，也不再 GET
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '77' })
    h.es.emit('open')
    await flush()
    expect(h.devices).toHaveBeenCalledTimes(1)
    expect(h.updates).toHaveLength(0)
    expect(h.expired).toHaveLength(1)
  })
})

describe('代际守卫与 kind+id 水位丢弃', () => {
  it('同一 kind+id 低于或等于已应用水位的事件直接丢弃，更高才推进水位', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    await flush()

    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '4', display_name: '旧' })
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '5', display_name: '相等' })
    expect(h.updates).toHaveLength(0)

    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '6', display_name: '新' })
    expect(h.updates).toHaveLength(1)
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('6')

    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '6', display_name: '重复' })
    expect(h.updates).toHaveLength(1)
  })

  it('水位只在同一 kind+id 内比较：跨对象/跨 kind 绝不互相抑制', async () => {
    const h = createHarness({
      snapshotsConfig: 'both',
      deviceSnapshot: async () => [deviceItem('dev-1', '1000'), deviceItem('dev-2', '2')],
      taskSnapshot: async () => [taskItem('task-1', '3')],
    })
    client = h.client
    h.es.emit('open')
    await flush()

    // dev-2 的 revision 远小于 dev-1 的水位，但仍高于自身水位：绝不能被跨对象比较丢弃
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '1001' })
    h.es.emitData('device.updated', { device_id: 'dev-2', revision: '3' })
    expect(h.updates.map((u) => u.patch.device_id)).toEqual(['dev-1', 'dev-2'])

    // 相对自身水位偏低的 dev-2 rev 3（重复）与 dev-1 rev 5 一律丢弃
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '5' })
    h.es.emitData('device.updated', { device_id: 'dev-2', revision: '3' })
    expect(h.updates).toHaveLength(2)

    // task 与 device 即使 id 相同也互不相干（Page 无全局 revision）
    h.es.emitData('task.updated', { id: 'dev-2', revision: '1' })
    expect(h.updates).toHaveLength(3)
    expect(h.updates[2]!.kind).toBe('task')
    expect(h.client.watermarkOf('device', 'dev-2')).toBe('3')
    expect(h.client.watermarkOf('task', 'dev-2')).toBe('1')
  })

  it('列表项无 revision 时不建立虚假基线，首个事件提示即可建立水位', async () => {
    const h = createHarness({
      snapshotsConfig: 'device-only',
      deviceSnapshot: async () => [{ device_id: 'dev-9', display_name: '无版本' }],
    })
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.client.watermarkOf('device', 'dev-9')).toBeNull()
    h.es.emitData('device.updated', { device_id: 'dev-9', revision: '3' })
    expect(h.updates).toHaveLength(1)
    expect(h.client.watermarkOf('device', 'dev-9')).toBe('3')
    h.es.emitData('device.updated', { device_id: 'dev-9', revision: '2' })
    expect(h.updates).toHaveLength(1)
  })

  it('重同步期间的缓冲在快照落库后按水位回放（低水位丢弃、高水位生效）', async () => {
    const gate = deferred<ReadonlyArray<Record<string, unknown>>>()
    const h = createHarness({ snapshotsConfig: 'device-only', deviceSnapshot: () => gate.promise })
    client = h.client
    h.es.emit('open')

    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '4', display_name: '低于快照' })
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '6', display_name: '高于快照' })
    h.es.emitData('device.updated', { device_id: 'dev-3', revision: '1', display_name: '新对象' })
    expect(h.updates).toHaveLength(0) // 同步中绝不回写旧快照

    gate.resolve([deviceItem('dev-1', '5')])
    await flush()
    expect(h.snapshots).toEqual([{ kind: 'device', count: 1 }])
    expect(h.updates.map((u) => u.patch.device_id)).toEqual(['dev-1', 'dev-3'])
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('6')
    expect(h.client.isRealtime.value).toBe(true)
  })

  it('缓冲溢出：清空缓冲、标记非实时并重新发起全量同步', async () => {
    const gate = deferred<ReadonlyArray<Record<string, unknown>>>()
    const h = createHarness({ snapshotsConfig: 'device-only', maxBufferSize: 2, deviceSnapshot: () => gate.promise })
    client = h.client
    h.es.emit('open')
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '6' })
    h.es.emitData('device.updated', { device_id: 'dev-2', revision: '11' })
    h.es.emitData('device.updated', { device_id: 'dev-3', revision: '1' }) // 第 3 条溢出
    expect(h.errors).toContain('BUFFER_OVERFLOW')
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.lastSyncReason.value).toBe('buffer-overflow')
    expect(h.deviceSignals[0]!.aborted).toBe(true)
    expect(h.devices).toHaveBeenCalledTimes(2)

    gate.resolve([deviceItem('dev-1', '5')])
    await flush()
    expect(h.updates).toHaveLength(0) // 溢出丢弃的缓冲不回灌
  })

  it('重连产生新连接与新代际：旧代际在途 GET 即便不被 abort 也不能回写', async () => {
    const stale = deferred<ReadonlyArray<Record<string, unknown>>>()
    let calls = 0
    const h = createHarness({
      snapshotsConfig: 'device-only',
      deviceSnapshot: () => {
        calls += 1
        return calls === 1 ? stale.promise : Promise.resolve([deviceItem('dev-1', '30')])
      },
    })
    client = h.client
    h.es.emit('open')
    const firstGeneration = h.client.syncGeneration.value
    const firstEpoch = h.client.connectionEpoch.value

    h.es.emit('error')
    h.es.emit('open') // 同一替身再次 open：连接代际递增
    expect(h.client.connectionEpoch.value).toBe(firstEpoch + 1)
    expect(h.client.syncGeneration.value).toBeGreaterThan(firstGeneration)
    expect(h.deviceSignals[0]!.aborted).toBe(true)

    stale.resolve([deviceItem('dev-1', '1')]) // 旧代际响应迟到且未被 abort
    await flush()
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('30')
    expect(h.snapshots).toHaveLength(1)
    expect(h.client.isRealtime.value).toBe(true)
  })
})

describe('断线状态与实时指示', () => {
  it('error 使连接降级为非实时，重连后重新走订阅后快照流程', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.client.isRealtime.value).toBe(true)

    h.es.emit('error')
    expect(h.client.connectionState.value).toBe('reconnecting')
    expect(h.client.syncState.value).toBe('stale')
    expect(h.client.isRealtime.value).toBe(false)

    h.es.emit('open')
    expect(h.client.connectionState.value).toBe('open')
    expect(h.client.isRealtime.value).toBe(false) // 必须等新快照完成
    await flush()
    expect(h.client.isRealtime.value).toBe(true)
    expect(h.devices).toHaveBeenCalledTimes(2)
  })

  it('断线期间到达的提示只缓冲，重连快照落库后按对象水位合并', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    await flush()
    h.es.emit('error')
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '4', display_name: '低于新快照' })
    h.es.emitData('device.updated', { device_id: 'dev-3', revision: '2', display_name: '新对象' })
    expect(h.updates).toHaveLength(0) // 非实时期绝不回写投影

    h.es.emit('open')
    await flush()
    expect(h.updates.map((u) => u.patch.device_id)).toEqual(['dev-3']) // dev-1 rev 4 ≤ 快照 5 丢弃
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('5')
    expect(h.client.isRealtime.value).toBe(true)
  })

  it('旧连接遗留的高水位提示在重连快照后仍可合并（不丢状态更新）', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    await flush()
    h.es.emit('error')
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '99' })
    h.es.emit('open')
    await flush()
    expect(h.updates).toHaveLength(1)
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('99')
  })
describe('撤权与会话失效', () => {
  it('快照 GET 返回 401：关闭订阅、清理投影、只回调一次', async () => {
    const h = createHarness({
      snapshotsConfig: 'device-only',
      deviceSnapshot: async () => {
        throw Object.assign(new Error('unauthorized'), { status: 401, code: 'AUTH_REQUIRED' })
      },
    })
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.expired).toEqual(['http-401'])
    expect(h.es.closed).toBe(true)
    expect(h.client.connectionState.value).toBe('unauthorized')
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.syncState.value).toBe('expired')
    expect(h.snapshots).toHaveLength(0)
    h.es.emit('permissions.changed')
    await flush()
    expect(h.expired).toHaveLength(1)
  })

  it('SSE 层 error 事件携带 401 视为认证失效', () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    h.es.emit('error', { status: 401 })
    expect(h.expired).toEqual(['sse-401'])
    expect(h.es.closed).toBe(true)
  })

  it('外部 auth store 通知失效：notifyUnauthorized 关闭订阅并清理缓冲', async () => {
    const gate = deferred<ReadonlyArray<Record<string, unknown>>>()
    const h = createHarness({ snapshotsConfig: 'device-only', deviceSnapshot: () => gate.promise })
    client = h.client
    h.es.emit('open')
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '7' })
    h.client.notifyUnauthorized()
    expect(h.expired).toEqual(['notified'])
    expect(h.deviceSignals[0]!.aborted).toBe(true)
    expect(h.es.closed).toBe(true)
    gate.resolve([deviceItem('dev-1', '7')])
    await flush()
    expect(h.snapshots).toHaveLength(0)
    expect(h.updates).toHaveLength(0)
  })

  it('快照被拒绝（非 401）只标记过期并保留订阅，不自动重取（避免 GET 风暴）', async () => {
    const h = createHarness({
      snapshotsConfig: 'device-only',
      deviceSnapshot: async () => {
        throw Object.assign(new Error('boom'), { status: 503, code: 'SNAPSHOT_UNAVAILABLE' })
      },
    })
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.errors).toContain('SNAPSHOT_FAILED')
    expect(h.client.syncState.value).toBe('stale')
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.es.closed).toBe(false)
    expect(h.expired).toHaveLength(0)
    await flush()
    expect(h.devices).toHaveBeenCalledTimes(1)
  })
})

describe('关闭与清理', () => {
  it('close 关闭底层订阅、取消在途 GET 并清空状态；重复 close 幂等', async () => {
    const gate = deferred<ReadonlyArray<Record<string, unknown>>>()
    const h = createHarness({ snapshotsConfig: 'device-only', deviceSnapshot: () => gate.promise })
    client = h.client
    h.es.emit('open')
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '7' })
    h.client.close()
    expect(h.es.closed).toBe(true)
    expect(h.es.closeCount).toBe(1)
    expect(h.deviceSignals[0]!.aborted).toBe(true)
    expect(h.client.connectionState.value).toBe('closed')
    expect(h.client.syncState.value).toBe('idle')
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.watermarkOf('device', 'dev-1')).toBeNull()

    gate.resolve([deviceItem('dev-1', '7')])
    await flush()
    expect(h.snapshots).toHaveLength(0)

    h.es.emit('open')
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '9' })
    expect(h.devices).toHaveBeenCalledTimes(1)
    expect(h.updates).toHaveLength(0)

    h.client.close()
    expect(h.es.closeCount).toBe(1)
    client = null
  })

  it('组件卸载后不再产生任何分发（含 reset 与时间事件）', async () => {
    const h = createHarness()
    client = h.client
    h.es.emit('open')
    await flush()
    h.client.close()
    client = null
    h.es.emit('reset')
    h.es.emit('system.time.changed', { data: '{}' })
    h.es.emit('permissions.changed')
    await flush()
    expect(h.devices).toHaveBeenCalledTimes(1)
    expect(h.timeEvents).toHaveLength(0)
    expect(h.expired).toHaveLength(0)
  })
})

describe('畸形与不可映射事件', () => {
  it('data 非合法 JSON 对象：记录稳定错误码并触发重同步，不抛异常', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    await flush()
    const before = h.client.syncGeneration.value
    expect(() => h.es.emit('device.updated', { data: '{not-json' })).not.toThrow()
    expect(h.errors).toContain('MALFORMED_EVENT')
    expect(h.client.syncGeneration.value).toBe(before + 1)
    expect(h.client.isRealtime.value).toBe(false)
    await flush()
    expect(h.devices).toHaveBeenCalledTimes(2)
    expect(h.client.isRealtime.value).toBe(true)
  })

  it('事件缺对象 ID 或 revision：丢弃并触发重同步，不污染水位', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    await flush()
    h.es.emitData('device.updated', { revision: '99' })
    h.es.emitData('device.updated', { device_id: 'dev-1' })
    h.es.emitData('device.updated', { device_id: 'dev-1', revision: '007' })
    expect(h.errors.filter((code) => code === 'UNMAPPED_EVENT')).toHaveLength(2)
    expect(h.errors).toContain('INVALID_REVISION')
    expect(h.updates).toHaveLength(0)
    expect(h.client.watermarkOf('device', 'dev-1')).toBe('5')
  })

  it('快照项 revision 非法：整轮回写作废，不部分污染水位', async () => {
    const h = createHarness({
      snapshotsConfig: 'device-only',
      deviceSnapshot: async () => [deviceItem('dev-1', '5'), { device_id: 'dev-2', revision: '1e3' }],
    })
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.errors).toContain('INVALID_SNAPSHOT_ITEM')
    expect(h.snapshots).toHaveLength(0)
    expect(h.client.watermarkOf('device', 'dev-1')).toBeNull()
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.syncState.value).toBe('stale')
  })
})

describe('订阅生命周期与注入工厂', () => {
  it('EventSource 工厂只在构造时调用一次：断线重连由 EventSource 自身完成，客户端复用同一订阅', async () => {
    const factory = vi.fn(() => new FakeEventSource())
    const snapshots: EntityKind[] = []
    const c = createEventStreamClient({
      snapshots: [{ kind: 'device', fetch: async () => [deviceItem('dev-1', '5')] }],
      eventSourceFactory: factory,
      onSnapshot: (kind) => { snapshots.push(kind) },
    })
    client = c
    expect(factory).toHaveBeenCalledTimes(1)
    expect(factory).toHaveBeenCalledWith('/api/v1/events', { withCredentials: true })

    const instance = factory.mock.results[0]!.value as FakeEventSource
    instance.emit('open')
    await flush()
    expect(c.isRealtime.value).toBe(true)
    instance.emit('error')
    instance.emit('open') // 浏览器 EventSource 内部重连后再次 open
    await flush()
    expect(factory).toHaveBeenCalledTimes(1)
    expect(c.connectionEpoch.value).toBe(2)
    expect(c.isRealtime.value).toBe(true)
    expect(snapshots).toEqual(['device', 'device'])
  })
})
})

describe('主动重同步与异常路径加固', () => {
  it('resync() 在订阅未建立时只标记过期，绝不发起 GET', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.client.resync()
    await flush()
    expect(h.devices).not.toHaveBeenCalled()
    expect(h.client.syncState.value).toBe('stale')
    expect(h.client.isRealtime.value).toBe(false)
  })

  it('resync() 在实时态下重新全量 GET，期间非实时，完成后恢复实时', async () => {
    const h = createHarness({ snapshotsConfig: 'device-only' })
    client = h.client
    h.es.emit('open')
    await flush()
    const before = h.client.syncGeneration.value
    h.client.resync()
    expect(h.client.syncGeneration.value).toBe(before + 1)
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.client.lastSyncReason.value).toBe('manual')
    await flush()
    expect(h.devices).toHaveBeenCalledTimes(2)
    expect(h.client.isRealtime.value).toBe(true)
  })

  it('快照实现同步抛错：降级为非实时且不产生未处理拒绝', async () => {
    const unhandled: unknown[] = []
    const onUnhandled = (reason: unknown): void => { unhandled.push(reason) }
    process.on('unhandledRejection', onUnhandled)
    const h = createHarness({
      snapshotsConfig: 'device-only',
      deviceSnapshot: (): never => {
        throw new Error('snapshot threw synchronously')
      },
    })
    client = h.client
    h.es.emit('open')
    await flush()
    expect(h.errors).toContain('SNAPSHOT_FAILED')
    expect(h.client.syncState.value).toBe('stale')
    expect(h.client.isRealtime.value).toBe(false)
    expect(h.es.closed).toBe(false)
    process.off('unhandledRejection', onUnhandled)
    expect(unhandled).toHaveLength(0)
  })

  it('订阅方 onSnapshot 抛错：状态降级且保留订阅，不留下 syncing', async () => {
    const es = new FakeEventSource()
    const c = createEventStreamClient({
      snapshots: [{ kind: 'device', fetch: async () => [deviceItem('dev-1', '5')] }],
      eventSourceFactory: () => es,
      onSnapshot: () => {
        throw new Error('page projection bug')
      },
    })
    client = c
    es.emit('open')
    await flush()
    expect(c.syncState.value).toBe('stale')
    expect(c.isRealtime.value).toBe(false)
    expect(c.lastError.value?.code).toBe('SNAPSHOT_FAILED')
    expect(es.closed).toBe(false)
  })
})

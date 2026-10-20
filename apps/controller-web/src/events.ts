import { ref, type Ref } from 'vue'
import { isU64DecimalString } from './types'

/**
 * SSE 客户端连接与事件分发（AT-18 / 06 §4 U-03）。
 *
 * 契约要点（docs/superpowers/plans/2026-10-16-controller-v1-integration-acceptance-tdd.md:648-658）：
 * 1. 五种命名事件通过 `addEventListener` 分别独立注册，绝不只监听默认 `message`；
 * 2. 只有在 SSE 实际 `open` 建立成功后才发起全量 GET 快照；
 * 3. 维护连接代际与同步代际，旧代际的在途 GET 响应与乱序事件一律丢弃；
 * 4. revision 水位只在同一对象 kind + 对象 ID 内单调递增比较，Page 无全局 revision；
 * 5. 断线重连期间状态指示器绝不标记为「实时」；
 * 6. `reset` 使旧同步代际失效、取消在途 GET、重置水位并重新全量 GET；
 * 7. `permissions.changed` 或 401/认证失效时关闭订阅并清理授权投影。
 */

/** 统一命名事件；客户端对五个名称分别注册独立监听（不能只监听默认 `message`）。 */
export const SSE_EVENT_NAMES = [
  'device.updated',
  'task.updated',
  'permissions.changed',
  'system.time.changed',
  'reset',
] as const

export type SseEventName = (typeof SSE_EVENT_NAMES)[number]

/** 对象补丁 kind 词表：命名事件与 kind 的映射固定，水位只在同一 kind + id 内比较。 */
export type EntityKind = 'device' | 'task'

const UPDATE_EVENT_KIND: Readonly<Partial<Record<SseEventName, EntityKind>>> = {
  'device.updated': 'device',
  'task.updated': 'task',
}

export type ConnectionState = 'connecting' | 'open' | 'reconnecting' | 'closed' | 'unauthorized'
export type SyncState = 'idle' | 'syncing' | 'synced' | 'stale' | 'expired'

export type SyncReason =
  | 'open' | 'reset' | 'system-time-changed' | 'buffer-overflow'
  | 'invalid-event' | 'snapshot-failed' | 'manual'

/** 订阅终止原因：撤权事件、HTTP 401、SSE 层报告认证失效、外部主动通知。 */
export type ShutdownReason = 'permissions.changed' | 'http-401' | 'sse-401' | 'notified'

export type EventStreamErrorCode =
  | 'MALFORMED_EVENT'
  | 'UNMAPPED_EVENT'
  | 'INVALID_REVISION'
  | 'INVALID_SNAPSHOT_ITEM'
  | 'SNAPSHOT_FAILED'
  | 'BUFFER_OVERFLOW'

/** 错误线索只含稳定代码与固定标签，绝不回显服务端原始载荷。 */
export interface EventStreamError {
  code: EventStreamErrorCode
  kind?: EntityKind
  detail?: string
}

/** 注入用的最小 EventSource 形状：Node/Vitest 下可注入纯 JS mock 完成全套契约测试。 */
export interface EventSourceLike {
  addEventListener(type: string, listener: (event: unknown) => void): void
  close(): void
}

export interface EventSourceInitLike {
  withCredentials: boolean
}

export type EventSourceFactory = (url: string, init: EventSourceInitLike) => EventSourceLike

/** 生产默认工厂：惰性求值，测试环境不会触碰全局 `EventSource`。token 不进 URL，只用同源 Cookie。 */
export const defaultEventSourceFactory: EventSourceFactory = (url, init) =>
  new EventSource(url, { withCredentials: init.withCredentials })

/** 单一 kind 的全量快照来源；`fetch` 必须尊重 `signal`（reset/断线时会被 abort）。 */
export interface SnapshotSource {
  kind: EntityKind
  fetch: (context: { signal: AbortSignal }) => Promise<ReadonlyArray<Record<string, unknown>>>
}

export interface EventStreamClientOptions {
  /** 全量 GET 快照来源集合；同一 kind 只允许注册一次。 */
  snapshots?: ReadonlyArray<SnapshotSource>
  /** SSE 端点，默认 `/api/v1/events`，不允许携带 query（凭据只走 Cookie）。 */
  url?: string
  eventSourceFactory?: EventSourceFactory
  /** 同步期间事件缓冲上限，默认 128（与服务端每连接待发上限一致）。 */
  maxBufferSize?: number
  withCredentials?: boolean
  onSnapshot?: (kind: EntityKind, items: ReadonlyArray<Record<string, unknown>>) => void
  onEntityUpdate?: (kind: EntityKind, patch: Record<string, unknown>) => void
  onSystemTimeChanged?: (payload: Record<string, unknown> | null) => void
  onSessionExpired?: (reason: ShutdownReason) => void
  onError?: (error: EventStreamError) => void
}

export interface EventStreamClient {
  readonly connectionState: Readonly<Ref<ConnectionState>>
  readonly syncState: Readonly<Ref<SyncState>>
  /** 只有「连接已实际建立」且「当前同步代际的快照已完成回写」才为 true。 */
  readonly isRealtime: Readonly<Ref<boolean>>
  readonly connectionEpoch: Readonly<Ref<number>>
  readonly syncGeneration: Readonly<Ref<number>>
  readonly lastError: Readonly<Ref<EventStreamError | null>>
  /** 最近一次发起全量重同步的触发原因，供连接状态指示器与排障使用。 */
  readonly lastSyncReason: Readonly<Ref<SyncReason | null>>
  /** 读取某 kind+id 当前已应用的 revision 水位；无记录返回 null。 */
  watermarkOf(kind: EntityKind, id: string): string | null
  /** 主动发起一轮全量重同步（等价于 reset 的客户端侧触发）。 */
  resync(): void
  /** 外部（如 auth store）报告会话失效：关闭订阅并清理授权投影。 */
  notifyUnauthorized(): void
  close(): void
}

const DEFAULT_URL = '/api/v1/events'
const DEFAULT_MAX_BUFFER = 128
const MAX_BUFFER_LIMIT = 4096
const ENTITY_KINDS = new Set<string>(['device', 'task'])
const UNAUTHORIZED_CODES = new Set(['AUTH_REQUIRED', 'UNAUTHENTICATED', 'SESSION_EXPIRED', 'INVALID_SESSION', 'TOKEN_EXPIRED'])

/**
 * 严格 canonical u64 十进制字符串无损比较（不经 `Number`/`parseInt`，无精度损失）。
 * 非规范输入（前导零、负号、小数、指数、空白、非字符串、超 u64 上限）抛错，绝不静默按 0 处理。
 */
export function compareRevision(a: string, b: string): number {
  if (!isU64DecimalString(a) || !isU64DecimalString(b)) throw new Error('INVALID_REVISION')
  if (a === b) return 0
  if (a.length !== b.length) return a.length > b.length ? 1 : -1
  return a > b ? 1 : -1
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function parseEventData(raw: unknown): Record<string, unknown> | null {
  if (typeof raw !== 'string' || raw.trim() === '') return null
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return null
  }
  return isRecord(parsed) ? parsed : null
}

/** 对象 ID 取值顺序：`${kind}_id` → `id`（设备契约 `device_id`，任务契约 `id`）。 */
function pickEntityId(kind: EntityKind, payload: Record<string, unknown>): string | null {
  for (const key of [`${kind}_id`, 'id']) {
    const value = payload[key]
    if (typeof value === 'string' && value.length > 0 && value.length <= 512) return value
  }
  return null
}

function isUnauthorizedError(error: unknown): boolean {
  if (!isRecord(error)) return false
  if (error.status === 401) return true
  return typeof error.code === 'string' && UNAUTHORIZED_CODES.has(error.code)
}

/** SSE 层（或注入的 fetch 桥接实现）在 error 事件上暴露的 HTTP 状态。 */
function eventHttpStatus(event: unknown): number | null {
  return isRecord(event) && typeof event.status === 'number' ? event.status : null
}

export function createEventStreamClient(options: EventStreamClientOptions = {}): EventStreamClient {
  const url = options.url ?? DEFAULT_URL
  if (options.url !== undefined && (url.length === 0 || url.includes('?') || url.includes('#'))) {
    throw new Error('INVALID_SSE_URL')
  }
  const maxBufferSize = options.maxBufferSize ?? DEFAULT_MAX_BUFFER
  if (!Number.isInteger(maxBufferSize) || maxBufferSize < 1 || maxBufferSize > MAX_BUFFER_LIMIT) {
    throw new Error('INVALID_BUFFER_SIZE')
  }
  const sources = options.snapshots ?? []
  const seenKinds = new Set<string>()
  for (const source of sources) {
    if (!isRecord(source) || !ENTITY_KINDS.has(source.kind) || typeof source.fetch !== 'function') {
      throw new Error('INVALID_SNAPSHOT_SOURCE')
    }
    if (seenKinds.has(source.kind)) throw new Error('DUPLICATE_SNAPSHOT_SOURCE')
    seenKinds.add(source.kind)
  }
  const factory = options.eventSourceFactory ?? defaultEventSourceFactory
  const withCredentials = options.withCredentials ?? true

  const connectionState = ref<ConnectionState>('connecting')
  const syncState = ref<SyncState>('idle')
  const isRealtime = ref(false)
  const connectionEpoch = ref(0)
  const syncGeneration = ref(0)
  const lastError = ref<EventStreamError | null>(null)
  const lastSyncReason = ref<SyncReason | null>(null)

  let active = true
  let open = false
  /** 同步期间的事件缓冲：不回写旧快照，快照落库后按水位过滤回放。 */
  const buffer: Array<{ kind: EntityKind; payload: Record<string, unknown> }> = []
  /** 对象水位表：key = kind + 分隔符 + id，跨 kind/跨对象绝不互相比较。 */
  const watermarks = new Map<string, string>()
  let inFlight: AbortController | null = null

  function watermarkKey(kind: EntityKind, id: string): string {
    return `${kind}\u0000${id}`
  }

  function fail(code: EventStreamErrorCode, extra?: { kind?: EntityKind; detail?: string }): void {
    const error: EventStreamError = { code, ...extra }
    lastError.value = error
    options.onError?.(error)
  }

  function abortInFlight(): void {
    if (inFlight !== null) {
      inFlight.abort()
      inFlight = null
    }
  }

  function ticketOf(signal: AbortSignal, epoch: number, generation: number): { epoch: number; generation: number; signal: AbortSignal } {
    return { epoch, generation, signal }
  }

  function isCurrent(ticket: { epoch: number; generation: number; signal: AbortSignal }): boolean {
    return active && open && connectionEpoch.value === ticket.epoch &&
      syncGeneration.value === ticket.generation && !ticket.signal.aborted
  }

  interface PendingUpdate {
    kind: EntityKind
    id: string
    revision: string
    payload: Record<string, unknown>
  }

  /** 同一 kind+id 低于或等于已应用水位的事件直接丢弃；更高才推进水位并分发。 */
  function applyUpdate(update: PendingUpdate): void {
    const key = watermarkKey(update.kind, update.id)
    const applied = watermarks.get(key)
    if (applied !== undefined) {
      let comparison: number
      try {
        comparison = compareRevision(update.revision, applied)
      } catch {
        fail('INVALID_REVISION', { kind: update.kind, detail: 'revision_not_canonical_u64' })
        startSync('invalid-event')
        return
      }
      if (comparison <= 0) return
    }
    watermarks.set(key, update.revision)
    options.onEntityUpdate?.(update.kind, update.payload)
  }

  function drainBuffer(): void {
    const pending = buffer.splice(0, buffer.length)
    for (const entry of pending) {
      const id = pickEntityId(entry.kind, entry.payload)
      const revision = entry.payload.revision
      if (id === null || !isU64DecimalString(revision)) {
        fail('UNMAPPED_EVENT', { kind: entry.kind, detail: 'buffered_event_unmapped' })
        continue
      }
      applyUpdate({ kind: entry.kind, id, revision, payload: entry.payload })
    }
  }

  function startSync(reason: SyncReason): void {
    if (!active) return
    lastSyncReason.value = reason
    isRealtime.value = false
    abortInFlight()
    // 注意：这里绝不清空缓冲。断线/重同步期间缓存的变更提示必须在新快照落库后按水位合并，
    // 否则短暂断线期间的状态更新会永久丢失。缓冲上限由 maxBufferSize 兜底，reset/shutdown 显式清空。
    if (!open) {
      // 断线期间绝不发起 GET：重连后的独立 GET 成功同样不能标记实时，必须等下一次 open。
      syncState.value = 'stale'
      return
    }
    const generation = syncGeneration.value + 1
    const epochAtStart = connectionEpoch.value
    syncGeneration.value = generation
    const controller = new AbortController()
    inFlight = controller
    syncState.value = 'syncing'
    runSync(ticketOf(controller.signal, epochAtStart, generation), controller)
      .catch(() => {
        // 订阅方回调或快照实现抛出的意外异常：降级为非实时，绝不让状态卡在 syncing 或产生未处理拒绝。
        if (!isCurrent(ticketOf(controller.signal, epochAtStart, generation))) return
        syncState.value = 'stale'
        fail('SNAPSHOT_FAILED', { detail: 'sync_pipeline_threw' })
      })
  }

  async function runSync(
    ticket: { epoch: number; generation: number; signal: AbortSignal },
    controller: AbortController,
  ): Promise<void> {
    // 快照实现的同步抛错也必须变成 rejected，否则整条链路会抛出未处理拒绝并卡在 syncing。
    const settled = await Promise.allSettled(sources.map((source) =>
      new Promise<ReadonlyArray<Record<string, unknown>>>((resolve) => {
        resolve(source.fetch({ signal: controller.signal }))
      }),
    ))
    if (!isCurrent(ticket)) return
    if (inFlight === controller) inFlight = null

    for (const result of settled) {
      if (result.status === 'rejected' && isUnauthorizedError(result.reason)) {
        shutdown('http-401')
        return
      }
    }
    const rejected = settled.find((result) => result.status === 'rejected')
    if (rejected !== undefined) {
      // 快照失败保持非实时且不自动重试：重试由下一次 open、reset 或页面主动 resync 触发，避免 GET 风暴。
      syncState.value = 'stale'
      fail('SNAPSHOT_FAILED', { detail: 'snapshot_request_rejected' })
      return
    }

    // 先全量校验再原子提交：非法项绝不部分污染水位表或页面投影。
    const commits: Array<{ kind: EntityKind; items: ReadonlyArray<Record<string, unknown>>; entries: Array<{ key: string; revision: string }> }> = []
    for (let index = 0; index < sources.length; index += 1) {
      const source = sources[index]!
      const items = (settled[index] as PromiseFulfilledResult<ReadonlyArray<Record<string, unknown>>>).value
      if (!Array.isArray(items)) {
        syncState.value = 'stale'
        fail('INVALID_SNAPSHOT_ITEM', { kind: source.kind, detail: 'snapshot_not_array' })
        return
      }
      const entries: Array<{ key: string; revision: string }> = []
      for (const item of items) {
        if (!isRecord(item)) {
          syncState.value = 'stale'
          fail('INVALID_SNAPSHOT_ITEM', { kind: source.kind, detail: 'item_not_object' })
          return
        }
        const id = pickEntityId(source.kind, item)
        if (id === null) {
          syncState.value = 'stale'
          fail('INVALID_SNAPSHOT_ITEM', { kind: source.kind, detail: 'item_id_invalid' })
          return
        }
        const revision = item.revision
        if (revision === undefined) continue // 列表项可不带 revision：无基线，首个更高事件建立水位
        if (!isU64DecimalString(revision)) {
          syncState.value = 'stale'
          fail('INVALID_SNAPSHOT_ITEM', { kind: source.kind, detail: 'item_revision_invalid' })
          return
        }
        entries.push({ key: watermarkKey(source.kind, id), revision })
      }
      commits.push({ kind: source.kind, items, entries })
    }

    // 回写前再次核对代际：abort 无效或已返回的旧响应绝不能覆盖新状态。
    if (!isCurrent(ticket)) return
    for (const commit of commits) {
      for (const entry of commit.entries) watermarks.set(entry.key, entry.revision)
      options.onSnapshot?.(commit.kind, commit.items)
    }
    drainBuffer()
    if (!isCurrent(ticket)) return
    syncState.value = 'synced'
    isRealtime.value = true
  }

  function handleUpdateEvent(kind: EntityKind, raw: unknown): void {
    if (!active) return
    const payload = parseEventData(raw)
    if (payload === null) {
      fail('MALFORMED_EVENT', { kind, detail: 'data_not_json_object' })
      startSync('invalid-event')
      return
    }
    const id = pickEntityId(kind, payload)
    const revision = payload.revision
    if (revision !== undefined && !isU64DecimalString(revision)) {
      fail('INVALID_REVISION', { kind, detail: 'event_revision_not_canonical_u64' })
      startSync('invalid-event')
      return
    }
    if (id === null || !isU64DecimalString(revision)) {
      fail('UNMAPPED_EVENT', { kind, detail: 'event_without_object_revision' })
      startSync('invalid-event')
      return
    }
    // 只有「当前同步代际的快照已落库」才可直接回写；同步中或断线待重连都先缓冲，
    // 等下一次成功快照后按对象水位合并回放，绝不把事件写进旧快照。
    if (syncState.value !== 'synced') {
      // 重同步期间重新缓冲，不把事件写入旧快照。
      if (buffer.length >= maxBufferSize) {
        buffer.length = 0
        fail('BUFFER_OVERFLOW', { kind, detail: 'buffer_exceeded' })
        startSync('buffer-overflow')
        return
      }
      buffer.push({ kind, payload })
      return
    }
    applyUpdate({ kind, id, revision, payload })
  }

  function handleSystemTimeChanged(raw: unknown): void {
    if (!active) return
    const payload = parseEventData(raw)
    options.onSystemTimeChanged?.(payload)
    // 时间事件只是变化提示（无对象 ID）：重新 GET 当前授权快照，不凭事件宣称时钟已同步。
    startSync('system-time-changed')
  }

  function handleReset(): void {
    if (!active) return
    // 旧同步代际失效：水位表与缓冲一并作废，重连后的快照重新建立基线。
    watermarks.clear()
    buffer.length = 0
    startSync('reset')
  }

  function shutdown(reason: ShutdownReason): void {
    if (!active) return
    active = false
    open = false
    isRealtime.value = false
    syncState.value = 'expired'
    connectionState.value = 'unauthorized'
    syncGeneration.value += 1
    abortInFlight()
    buffer.length = 0
    watermarks.clear() // 清理授权投影，绝不继续展示已撤权数据
    try {
      eventSource.close()
    } catch {
      // 关闭失败不改变已失效状态
    }
    options.onSessionExpired?.(reason)
  }

  function handleConnectionError(event: unknown): void {
    if (!active) return
    if (eventHttpStatus(event) === 401) {
      shutdown('sse-401')
      return
    }
    open = false
    isRealtime.value = false
    syncState.value = 'stale'
    connectionState.value = 'reconnecting'
    syncGeneration.value += 1 // 在途旧 GET 立即失效
    abortInFlight()
    // 断线前缓存的变更提示保留：撤权由 permissions.changed 单独清理，普通重连同一会话授权不变，
    // 重连快照落库后按对象 kind+id 水位合并，旧提示因不高于水位自然丢弃。
  }

  function handleClose(): void {
    if (!active) {
      // 已因撤权/401 终止的订阅保留 unauthorized 语义，仅供 UI 区分「需重新登录」。
      if (connectionState.value !== 'unauthorized') connectionState.value = 'closed'
      return
    }
    active = false
    open = false
    isRealtime.value = false
    syncState.value = 'idle'
    connectionState.value = 'closed'
    syncGeneration.value += 1
    abortInFlight()
    buffer.length = 0
    watermarks.clear()
    try {
      eventSource.close()
    } catch {
      // 已关闭的订阅重复关闭视为幂等成功
    }
  }

  const eventSource = factory(url, { withCredentials })

  eventSource.addEventListener('open', () => {
    if (!active) return
    open = true
    connectionEpoch.value += 1
    connectionState.value = 'open'
    // 实际 open 之后才发起全量 GET，握手期间的状态更新不会漏收。
    startSync('open')
  })

  eventSource.addEventListener('error', (event: unknown) => {
    handleConnectionError(event)
  })

  for (const name of SSE_EVENT_NAMES) {
    const kind = UPDATE_EVENT_KIND[name]
    if (kind !== undefined) {
      eventSource.addEventListener(name, (event: unknown) => {
        handleUpdateEvent(kind, isRecord(event) ? event.data : undefined)
      })
    } else if (name === 'reset') {
      eventSource.addEventListener('reset', () => {
        handleReset()
      })
    } else if (name === 'system.time.changed') {
      eventSource.addEventListener('system.time.changed', (event: unknown) => {
        handleSystemTimeChanged(isRecord(event) ? event.data : undefined)
      })
    } else {
      eventSource.addEventListener('permissions.changed', () => {
        shutdown('permissions.changed')
      })
    }
  }

  return {
    connectionState,
    syncState,
    isRealtime,
    connectionEpoch,
    syncGeneration,
    lastError,
    lastSyncReason,
    watermarkOf(kind, id) {
      return watermarks.get(watermarkKey(kind, id)) ?? null
    },
    resync() {
      startSync('manual')
    },
    notifyUnauthorized() {
      shutdown('notified')
    },
    close: handleClose,
  }
}

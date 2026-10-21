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
 * 7. `permissions.changed` 或 401/认证失效时关闭订阅并清理授权投影，任一快照来源报 401 当刻立即
 *    终止整轮同步（abort 全部在途 GET），绝不等待其他来源 settle；
 * 8. 同步缓冲条目绑定产生时的连接/同步代际，跨代际一律安全丢弃，绝不回放到新连接；
 * 9. 快照落库只前进水位：对象 revision 低于已应用水位时保留更高水位，绝不逆序回退；
 * 10. 事件对象不在当前全量快照成员中时不凭提示直接打补丁，改为当前授权 GET 重取；
 * 11. 每次调用外部回调（`onSnapshot`/`onEntityUpdate`/`onUpdateHint`）之后与每条缓冲事件应用之前，
 *     都必须复核连接/同步代际，撤权或 reset 后绝不继续灌入数据。入口直发的 `onUpdateHint` 之后
 *     复核订阅活跃性：宿主若在回调内撤权/关闭即就地终止；若其内部另起一轮同步，后续条目按
 *     **复核后**的最新代际入库，绝不被旧票据吞掉；
 * 12. 缓冲提取按代际取子集：`drainBuffer` 入口先复核票据，且只移除并回放属于本轮票据代际的条目，
 *     其他代际（含订阅方回调内部 reset/resync 后新产生的条目）原序留在共享缓冲，交给各自那一轮回放，
 *     绝不被失效的旧同步流程抽空吞掉；
 * 13. 未注册快照来源的 kind 没有任何已授权成员事实，其提示 fail-closed 静默忽略：不分发补丁，
 *     也不建立或污染水位（06 §4）。
 * 14. `onUpdateHint` 是**成员判定之前**发出的对象级失效提示（只含 kind + id）：未知成员同样送达，
 *    使宿主上不属于列表快照投影的读取（如任务详情）也能接进失效链路；它仍是提示不是数据，
 *    不建立水位，也不放宽「未知对象只重取不打补丁」的既有契约。
 * 15. 该提示与补丁缓冲**完全解耦**：在事件分发入口通过校验的当刻直发，不进缓冲、不等快照落地、
 *    不受同步状态影响，回放时不重发。因此同步窗口里即使补丁被提前中断（未知成员触发重同步、
 *    缓冲溢出丢弃、断线丢弃旧代际）提示也绝不丢失——它不携带载荷，宿主的重取自带单调保护。
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
  | 'invalid-event' | 'unknown-member' | 'snapshot-failed' | 'manual'

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
  /**
   * 对象级失效提示：**只带 kind + id，绝不带载荷**，且覆盖范围比 `onEntityUpdate` 更宽。
   *
   * `onEntityUpdate` 只对「当前全量快照的已知成员」分发（06 §4：未知成员不得凭提示打补丁），
   * 因此宿主上那些**不属于列表快照投影**的读取——典型是任务详情的 `/tasks/:id` 与
   * `/tasks/:id/children`——若只挂 `onEntityUpdate`，在深链进入或分页进入（对象不在首屏快照成员里）
   * 时永远收不到失效提示：那条事件走 `unknown-member` 重同步，宿主只见列表快照重取。
   *
   * 时序（审查 P1：提示必须与补丁缓冲解耦）：本回调在**事件分发入口**触发——载荷可解析、
   * 对象 ID 与 revision 通过校验的当刻立即发出，位置早于同步状态判定、缓冲入队、成员判定与
   * 水位过滤。因此每条经校验的更新事件恰好通知一次（缓冲条目回放时不再重复发出），且**任何**
   * 使补丁落地失败或延后的情形都不会吞掉提示：同步窗口内未知成员中断本轮回放、缓冲溢出丢弃、
   * 断线丢弃旧代际条目、水位判定事件过时被丢弃。
   *
   * 代价是「事件 revision 不高于已应用水位」的过时提示也会通知一次——提示不携带数据，宿主据其
   * 按当前授权重取，配合宿主自身的 revision 单调保护无回滚风险。
   */
  onUpdateHint?: (kind: EntityKind, id: string) => void
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
  /**
   * 同步期间的事件缓冲条目：必须记录产生时的连接/同步代际。
   * 旧代际事件（断线前的连接、上一轮同步）绝不跨代际回放到新连接（06 §4）。
   */
  interface BufferedEvent {
    kind: EntityKind
    payload: Record<string, unknown>
    epoch: number
    generation: number
  }
  /** 同步期间的事件缓冲：不回写旧快照，仅回放与当前同步代际同源的条目并按水位过滤。 */
  const buffer: BufferedEvent[] = []
  /** 对象水位表：key = kind + 分隔符 + id，跨 kind/跨对象绝不互相比较。 */
  const watermarks = new Map<string, string>()
  /**
   * 当前全量快照成员集合：只有已成功落库的快照才拥有成员事实。
   * 不在集合内的对象提示属于「列表成员变化/无法对应快照」，只能重取，不能当补丁分发。
   */
  const members = new Map<EntityKind, Set<string>>()
  /** 已注册快照来源的 kind：未注册来源的 kind 没有成员事实，其提示一律 fail-closed 静默忽略。 */
  const sourceKinds = new Set<EntityKind>(sources.map((source) => source.kind))
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

  /** 一轮同步的身份票据：连接代际 + 同步代际 + abort 信号，用于回写与外部分发前的复核。 */
  interface SyncTicket {
    epoch: number
    generation: number
    signal: AbortSignal
  }

  function ticketOf(signal: AbortSignal, epoch: number, generation: number): SyncTicket {
    return { epoch, generation, signal }
  }

  /** 撤权/自毁复核：订阅仍活跃、票据未被 abort 且连接/同步代际都未被推进。 */
  function isValidTicket(ticket: SyncTicket): boolean {
    return active && !ticket.signal.aborted && connectionEpoch.value === ticket.epoch &&
      syncGeneration.value === ticket.generation
  }

  function isCurrent(ticket: SyncTicket): boolean {
    return open && isValidTicket(ticket)
  }

  /** 代际切换：丢弃一切不属于当前连接/同步代际的未确认缓冲事件，绝不跨代际回放。 */
  function pruneBuffer(): void {
    for (let index = buffer.length - 1; index >= 0; index -= 1) {
      const entry = buffer[index]!
      if (entry.epoch !== connectionEpoch.value || entry.generation !== syncGeneration.value) {
        buffer.splice(index, 1)
      }
    }
  }

  interface PendingUpdate {
    kind: EntityKind
    id: string
    revision: string
    payload: Record<string, unknown>
  }

  function isKnownMember(kind: EntityKind, id: string): boolean {
    const known = members.get(kind)
    return known !== undefined && known.has(id)
  }

  /**
   * 快照水位只前进不后退：与已应用水位逐对象比对，只有更高才推进。
   * 快照版本低于已有水位时保留更高水位（快照对列表成员是权威的，但绝不能把水位逆序拉回，
   * 否则迟到的旧事件会被误判为更新）。只有 reset/撤权/关闭才清空水位重建基线。
   */
  function advanceWatermark(key: string, revision: string): void {
    const applied = watermarks.get(key)
    if (applied === undefined) {
      watermarks.set(key, revision)
      return
    }
    if (compareRevision(revision, applied) > 0) watermarks.set(key, revision)
  }

  /**
   * 应用单个对象补丁。返回 false 表示外部回调期间连接/同步代际已失效（撤权、close 或 reset），
   * 调用方必须立即中断后续流程，绝不继续灌入数据。
   */
  function applyUpdate(update: PendingUpdate, alive: () => boolean): boolean {
    // 未注册快照来源的 kind：客户端对该 kind 没有任何已授权成员事实，06 §4 禁止把未知对象当作补丁
    // 直接分发（也不能借分发建立/污染水位）。此类提示 fail-closed 静默忽略，不分发、不建水位。
    if (!sourceKinds.has(update.kind)) return alive()
    // 失效提示（onUpdateHint）不在这里发出：它已在事件分发入口通过校验的当刻直发（审查 P1），
    // 与补丁缓冲彻底解耦。挂在此处会让同步窗口里的提示依附于回放能否走完——未知成员触发
    // 重同步即中断本轮回放，被 take 出来的后续条目连同其提示一起丢失。
    // 未知成员：06 §4 明确「无法对应当前快照的对象」不得凭提示直接打补丁，必须当前授权 GET 重取。
    if (!isKnownMember(update.kind, update.id)) {
      fail('UNMAPPED_EVENT', { kind: update.kind, detail: 'event_object_not_in_snapshot' })
      startSync('unknown-member')
      return false
    }
    const key = watermarkKey(update.kind, update.id)
    const applied = watermarks.get(key)
    if (applied !== undefined) {
      let comparison: number
      try {
        comparison = compareRevision(update.revision, applied)
      } catch {
        fail('INVALID_REVISION', { kind: update.kind, detail: 'revision_not_canonical_u64' })
        startSync('invalid-event')
        return alive()
      }
      if (comparison <= 0) return alive()
    }
    watermarks.set(key, update.revision)
    options.onEntityUpdate?.(update.kind, update.payload)
    // 订阅方回调可能内部触发撤权或 reset：其后绝不再分发任何数据
    return alive()
  }

  /** 快照落库后回放缓冲：只回放同代际条目，且每条应用前后都复核代际与撤权。 */
  function drainBuffer(ticket: SyncTicket): void {
    // 入口先复核票据：最后一次 onSnapshot 之后订阅方可能已 reset/撤权/重连，
    // 此时本轮票据已失效，绝不能对共享缓冲做任何提取动作（提取即移除）。
    if (!isCurrent(ticket)) return
    // 只提取并移除属于本轮票据代际的条目：其他代际（外部回调内 reset/resync 后新产生的
    // 条目）必须原序留在共享缓冲里，交给它自己那一轮同步回放，绝不因本轮抽空而被吞掉。
    const pending: BufferedEvent[] = []
    const retained: BufferedEvent[] = []
    for (const entry of buffer) {
      if (entry.epoch === ticket.epoch && entry.generation === ticket.generation) pending.push(entry)
      else retained.push(entry)
    }
    buffer.length = 0
    for (const entry of retained) buffer.push(entry)
    const alive = (): boolean => isCurrent(ticket)
    for (const entry of pending) {
      if (!isCurrent(ticket)) return
      const id = pickEntityId(entry.kind, entry.payload)
      const revision = entry.payload.revision
      if (id === null || !isU64DecimalString(revision)) {
        fail('UNMAPPED_EVENT', { kind: entry.kind, detail: 'buffered_event_unmapped' })
        continue
      }
      // 返回 false 时中断本轮回放：只影响**补丁**的落地（未知成员会另起一轮全量重取，权威快照
      // 覆盖这些对象；代际失效则本轮数据本就该作废）。缓冲条目的失效提示已在事件入口直发，
      // 故此处提前退出不会让任何对象对宿主失明（审查 P1）。
      if (!applyUpdate({ kind: entry.kind, id, revision, payload: entry.payload }, alive)) return
    }
  }

  function startSync(reason: SyncReason): void {
    if (!active) return
    lastSyncReason.value = reason
    isRealtime.value = false
    abortInFlight()
    if (!open) {
      // 断线期间绝不发起 GET：重连后的独立 GET 成功同样不能标记实时，必须等下一次 open。
      syncState.value = 'stale'
      return
    }
    const generation = syncGeneration.value + 1
    const epochAtStart = connectionEpoch.value
    syncGeneration.value = generation
    // 同步代际切换：本轮之前的缓冲条目属于旧代际，安全丢弃，绝不回放到新快照之后的新代际。
    // 新代际内（GET 在途期间）到达的提示继续缓冲，上限由 maxBufferSize 兜底；reset/shutdown 显式清空。
    pruneBuffer()
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

  /**
   * 单个来源的快照 GET：快照实现的同步抛错也必须变成 rejected，否则整条链路会抛出未处理拒绝并卡在 syncing。
   * 401/会话失效在被识别的当刻立即整体 shutdown，绝不等待其他来源 settle。
   */
  function fetchSnapshotItems(
    source: SnapshotSource,
    signal: AbortSignal,
  ): Promise<ReadonlyArray<Record<string, unknown>>> {
    return new Promise<ReadonlyArray<Record<string, unknown>>>((resolve, reject) => {
      const onFailure = (error: unknown): void => {
        if (isUnauthorizedError(error)) shutdown('http-401')
        reject(error)
      }
      try {
        Promise.resolve(source.fetch({ signal })).then(resolve, onFailure)
      } catch (error) {
        onFailure(error)
      }
    })
  }

  async function runSync(ticket: SyncTicket, controller: AbortController): Promise<void> {
    const settled = await Promise.allSettled(
      sources.map((source) => fetchSnapshotItems(source, controller.signal)),
    )
    if (!isCurrent(ticket)) return
    if (inFlight === controller) inFlight = null

    // 说明：401 已在各来源 rejection 的当刻完成 shutdown（不等其他来源 settle），
    // 此时 isCurrent 必为 false，上面的代际检查即已终止本轮后续处理。
    const rejected = settled.find((result) => result.status === 'rejected')
    if (rejected !== undefined) {
      // 快照失败保持非实时且不自动重试：重试由下一次 open、reset 或页面主动 resync 触发，避免 GET 风暴。
      syncState.value = 'stale'
      fail('SNAPSHOT_FAILED', { detail: 'snapshot_request_rejected' })
      return
    }

    // 先全量校验再原子提交：非法项绝不部分污染水位表、成员集合或页面投影。
    interface SnapshotCommit {
      kind: EntityKind
      items: ReadonlyArray<Record<string, unknown>>
      entries: Array<{ key: string; revision: string }>
      memberIds: string[]
    }
    const commits: SnapshotCommit[] = []
    for (let index = 0; index < sources.length; index += 1) {
      const source = sources[index]!
      const items = (settled[index] as PromiseFulfilledResult<ReadonlyArray<Record<string, unknown>>>).value
      if (!Array.isArray(items)) {
        syncState.value = 'stale'
        fail('INVALID_SNAPSHOT_ITEM', { kind: source.kind, detail: 'snapshot_not_array' })
        return
      }
      const entries: Array<{ key: string; revision: string }> = []
      const memberIds: string[] = []
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
        memberIds.push(id)
        const revision = item.revision
        if (revision === undefined) continue // 列表项可不带 revision：无基线，首个更高事件建立水位
        if (!isU64DecimalString(revision)) {
          syncState.value = 'stale'
          fail('INVALID_SNAPSHOT_ITEM', { kind: source.kind, detail: 'item_revision_invalid' })
          return
        }
        entries.push({ key: watermarkKey(source.kind, id), revision })
      }
      commits.push({ kind: source.kind, items, entries, memberIds })
    }

    // 回写前再次核对代际：abort 无效或已返回的旧响应绝不能覆盖新状态。
    if (!isCurrent(ticket)) return
    for (const commit of commits) {
      // 外部回调可能内部触发撤权或 reset：每轮提交前都必须复核，失效立即中断后续 kind 的灌入。
      if (!isCurrent(ticket)) return
      // 快照对列表成员是权威的：成员集合整体替换；水位只前进不后退，绝不因旧快照逆序回退。
      for (const entry of commit.entries) advanceWatermark(entry.key, entry.revision)
      members.set(commit.kind, new Set(commit.memberIds))
      options.onSnapshot?.(commit.kind, commit.items)
      // 回调返回当刻立即复核：订阅方可能在回调内部 reset/resync 并让新代际开始缓冲事件，
      // 本轮流程必须就地让位，绝不带着已失效的票据继续走缓冲回放。
      if (!isCurrent(ticket)) return
    }
    // 最后一次回调返回后到回放之间同样存在让位窗口：失效票据绝不触碰共享缓冲。
    if (!isCurrent(ticket)) return
    drainBuffer(ticket)
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
    // 对象级失效提示在**校验通过的当刻**直发（契约 14/15：与补丁缓冲解耦）。这是全客户端唯一的
    // onUpdateHint 发出点：提示只含 kind + id，不携带任何可能因同步窗口而过期的载荷，所以它既
    // 不需要缓冲、也不需要成员判定与水位过滤，更不该依附补丁回放能否走完。宿主（任务详情等
    // 不属于列表快照投影的读取）据此立刻发起带自身 revision 单调保护的授权 GET。
    // 未注册快照来源的 kind 仍 fail-closed（契约 13）：对它一无所知，连提示也不建立。
    if (sourceKinds.has(kind)) {
      options.onUpdateHint?.(kind, id)
      // 提示同样是外部回调：宿主可能在其内部撤权或关闭订阅，其后绝不再分发任何补丁。
      // 宿主若在回调内另起一轮同步也不危险——下面的分支与缓冲入库读取的都是**复核后**的
      // 最新 syncState/connectionEpoch/syncGeneration，条目归入它自己那一代际。
      if (!active) return
    }
    // 只有「当前同步代际的快照已落库」才可直接回写；同步中或断线待重连都先缓冲，
    // 等本轮快照落库后按对象水位合并回放，绝不把事件写进旧快照。
    if (syncState.value !== 'synced') {
      // 重同步期间重新缓冲，不把事件写入旧快照。缓冲条目绑定当前连接/同步代际，
      // 代际一旦切换（断线、重连、reset、重同步）即安全丢弃，绝不跨代际回放。
      if (buffer.length >= maxBufferSize) {
        buffer.length = 0
        fail('BUFFER_OVERFLOW', { kind, detail: 'buffer_exceeded' })
        startSync('buffer-overflow')
        return
      }
      buffer.push({ kind, payload, epoch: connectionEpoch.value, generation: syncGeneration.value })
      return
    }
    // 直接分发路径：唯一的外部副作用是 onEntityUpdate，回调后订阅可能已撤权/自毁。
    applyUpdate({ kind, id, revision, payload }, () => active)
  }

  function handleSystemTimeChanged(raw: unknown): void {
    if (!active) return
    const payload = parseEventData(raw)
    options.onSystemTimeChanged?.(payload)
    // 时间回调可能内部触发撤权：不再发起任何 GET
    if (!active) return
    // 时间事件只是变化提示（无对象 ID）：重新 GET 当前授权快照，不凭事件宣称时钟已同步。
    startSync('system-time-changed')
  }

  function handleReset(): void {
    if (!active) return
    // 旧同步代际失效：水位表、成员集合与缓冲一并作废，重连后的快照重新建立全量基线。
    watermarks.clear()
    members.clear()
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
    members.clear()
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
    // 断线即代际切换：旧连接未确认的缓冲事件一律安全丢弃（06 §4 不保证历史重放），
    // 重连 open 后的新快照是当前授权事实的唯一来源，绝不把旧连接事件回放进新连接。
    pruneBuffer()
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
    members.clear()
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

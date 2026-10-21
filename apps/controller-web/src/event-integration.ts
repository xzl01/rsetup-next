import { computed, shallowRef, watch } from 'vue'

import {
  createEventStreamClient,
  type ConnectionState,
  type EntityKind,
  type EventSourceFactory,
  type EventStreamClient,
  type EventStreamError,
  type ShutdownReason,
  type SnapshotSource,
  type SyncReason,
  type SyncState,
} from './events'
import type { AuthStore } from './auth'

/**
 * SSE 客户端与 App 会话/授权生命周期的接线层（AT-18 / 06 §4 U-03）。
 *
 * 本层只负责「什么时候允许存在订阅」和「订阅状态如何诚实呈现」，
 * 五种命名事件、连接/同步代际与 kind+id 水位全部由 events.ts 承担：
 *
 * 1. 只有 `signed_in` 且非强制改密的会话才创建真实 EventSource；未认证、改密中或运行时
 *    没有 EventSource 能力时绝不构造，页面保持诚实的非实时 + 普通 GET 读取；
 * 2. 认证主体、`authz_epoch` 或 `csrf_token` 任一变化 → 关闭旧订阅、abort 其授权下的在途 GET、
 *    丢弃其投影，再按新一代际重建；只换 token 也算新一代际（审查第 5 项）；
 * 3. 事件一律当作失效提示：本层把同批 `onEntityUpdate` 合并成一次 `onInvalidation`，
 *    由宿主按当前授权重新 GET，绝不把事件载荷当数据源（06 §4）；`onInvalidation` 只覆盖当前快照的
 *    已知成员，因此另开一条同样「只是提示」的 `onUpdateHint` 直发通道（含未知成员），
 *    供任务详情这类不属于列表快照来源的读取精确接失效；
 * 4. `permissions.changed` 与 401 都先交还宿主清投影 + 走现有重新鉴权路径，再在**共享预算**内
 *    有限次自动重建：预算跨 `auth.refresh()` 的 `checking` 瞬时态保留（任何自动重建路径共用同一
 *    上限，审查第 1 项），只有认证主体真正切换或用户显式重试才重新发放；
 * 5. 实时标识只认 events.ts 的 `isRealtime`（连接已建立 + 当前同步代际已回写），
 *    并由 `resolveConnectionTone` 再兜一层，缺任一条件都不判实时。
 *
 * 服务端事件发布方尚未实现，本层不构成端到端实时验收。
 */

/** 连接状态的呈现档位：视图据此显示「实时」或「非实时 + 原因」，绝不自行推断。 */
export type EventConnectionTone =
  | 'realtime'
  | 'syncing'
  | 'connecting'
  | 'reconnecting'
  | 'stale'
  | 'unsupported'
  | 'unauthorized'
  | 'disconnected'

/** 会话仍有效但订阅被撤权/失效关闭时，允许自动重建订阅的次数上限（防抖但也防永久卡死）。 */
const MAX_AUTO_RESTARTS = 3

/**
 * 认证代际的最小输入。`csrfToken` 与 `authzEpoch` 同属代际输入：服务端在重新鉴权时会轮换
 * CSRF token，token 变了就意味着**旧授权上下文已作废**（旧 Cookie 会话下的写凭据不再有效），
 * 即使主体 id 与 `authz_epoch` 都没变，也必须关闭旧订阅并 abort 其授权下的在途 GET（审查第 5 项）。
 */
export type AuthSession = Pick<AuthStore, 'status' | 'user' | 'authzEpoch' | 'csrfToken'>

export interface EventIntegrationOptions {
  /** 认证状态来源：状态、身份与授权代际变化直接决定订阅的存亡。 */
  auth: AuthSession
  /** 全量快照来源：宿主注入，必须尊重 `signal` 且不扩大读取范围（只取当前授权的首屏）。 */
  snapshots: ReadonlyArray<SnapshotSource>
  url?: string
  eventSourceFactory?: EventSourceFactory
  /** 运行时能力探测；默认检测 `globalThis.EventSource`，缺失即诚实降级为普通 GET 读取。 */
  hasEventSource?: () => boolean
  withCredentials?: boolean
  onSnapshot?: (kind: EntityKind, items: ReadonlyArray<Record<string, unknown>>) => void
  /** 同批对象提示合并后的一次失效通知：宿主据此按当前授权重新 GET。 */
  onInvalidation?: (kinds: ReadonlyArray<EntityKind>) => void
  /**
   * 单个对象的失效提示（审查 P1：`onInvalidation` 的覆盖面不足以送达「投影之外」的读取）：
   * `onInvalidation` 由 `onEntityUpdate` 驱动，而后者只对当前快照的**已知成员**分发，于是深链或
   * 分页进入的对象（不在首屏快照成员里）其 `task.updated` 走 `unknown-member` 重同步，
   * 宿主永远收不到失效通知。本通道由 events.ts 在成员判定之前直发（只含 kind + id，
   * 仍是提示不是数据），宿主可用它精确刷新详情页这类不属于快照来源的读取。
   */
  onUpdateHint?: (kind: EntityKind, id: string) => void
  onSystemTimeChanged?: (payload: Record<string, unknown> | null) => void
  /** `permissions.changed`：宿主清旧授权投影并走现有重新鉴权路径。 */
  onPermissionsChanged?: () => void
  /** 会话失效（401/SSE 认证失效/外部通知）。 */
  onSessionExpired?: (reason: ShutdownReason) => void
  onError?: (error: EventStreamError) => void
}

export interface EventIntegration {
  readonly supported: Readonly<import('vue').Ref<boolean>>
  /** 是否存在由本层持有、尚未失效的订阅。 */
  readonly active: Readonly<import('vue').Ref<boolean>>
  readonly connectionState: Readonly<import('vue').Ref<ConnectionState | null>>
  readonly syncState: Readonly<import('vue').Ref<SyncState | null>>
  readonly isRealtime: Readonly<import('vue').Ref<boolean>>
  readonly lastError: Readonly<import('vue').Ref<EventStreamError | null>>
  readonly lastSyncReason: Readonly<import('vue').Ref<SyncReason | null>>
  readonly tone: Readonly<import('vue').Ref<EventConnectionTone>>
  /** 请求一轮全量重同步；未被 SSE 管理（无订阅/已失效）时返回 false，调用方须走普通 GET。 */
  requestResync(): boolean
  /** 按当前会话重新建立订阅（用户显式重试的唯一重连入口）。返回是否已发起。 */
  reconnect(): boolean
  /** 关闭当前订阅并丢弃其投影；保留代际监听，后续授权变化仍会重建。 */
  stop(): void
  /** 卸载：关闭订阅并停止内部代际监听。 */
  dispose(): void
}

/** 纯函数：把连接/同步状态折叠成单一呈现档位。实时需要「连接已建立 + 已同步」同时成立。 */
export function resolveConnectionTone(input: {
  supported: boolean
  active: boolean
  connectionState: ConnectionState | null
  syncState: SyncState | null
  isRealtime: boolean
}): EventConnectionTone {
  if (!input.supported) return 'unsupported'
  if (input.connectionState === 'unauthorized') return 'unauthorized'
  if (!input.active || input.connectionState === null || input.connectionState === 'closed') return 'disconnected'
  if (input.isRealtime && input.connectionState === 'open' && input.syncState === 'synced') return 'realtime'
  if (input.connectionState === 'reconnecting') return 'reconnecting'
  if (input.connectionState === 'connecting') return 'connecting'
  if (input.connectionState === 'open') return input.syncState === 'syncing' ? 'syncing' : 'stale'
  return 'stale'
}

function defaultHasEventSource(): boolean {
  return typeof (globalThis as { EventSource?: unknown }).EventSource === 'function'
}

export function createEventIntegration(options: EventIntegrationOptions): EventIntegration {
  const hasEventSource = options.hasEventSource ?? defaultHasEventSource
  const supported = computed(() => hasEventSource())

  const client = shallowRef<EventStreamClient | null>(null)
  const active = shallowRef(false)
  let disposed = false
  /** 当前订阅绑定的认证代际（主体 id + 授权代际 + CSRF token）；无订阅时为 null。 */
  let sessionIdentity: string | null = null
  /**
   * 自动重建预算的归属主体与余额。
   *
   * 关键在于**预算跟着认证主体走，不跟着订阅走**（审查第 1 项）：
   * `auth.refresh()` 会同步把状态打到 `checking`，接线层因此必然经历「订阅关闭 → 重新鉴权 →
   * 重新建立」的循环。若按「新一代际」发放预算，这个循环每转一圈预算就回满，服务端持续撤权时
   * 就退化成 `撤权 → 重鉴权 → 重连` 的无界自旋（复现：7 次 `permissions.changed` → 8 个 EventSource
   * 与 8 次 `/auth/me`）。因此只有两种情况重新发放：认证主体真正切换、用户显式重试。
   */
  let budgetPrincipal: string | null = null
  let restartsLeft = MAX_AUTO_RESTARTS
  /**
   * 撤权/会话失效后的「等真实鉴权结果」标记：置位期间自动重建只能由真实落位的 signed_in
   * 鉴权结果或显式重试触发，且必须消耗共享预算；预算耗尽则保持停止（诚实非实时 + 普通 GET 读取）。
   */
  let awaitingReauth = false

  const connectionState = computed<ConnectionState | null>(() => client.value?.connectionState.value ?? null)
  const syncState = computed<SyncState | null>(() => client.value?.syncState.value ?? null)
  const isRealtime = computed(() => client.value?.isRealtime.value === true)
  const lastError = computed<EventStreamError | null>(() => client.value?.lastError.value ?? null)
  const lastSyncReason = computed<SyncReason | null>(() => client.value?.lastSyncReason.value ?? null)
  const tone = computed(() => resolveConnectionTone({
    supported: supported.value,
    active: active.value,
    connectionState: connectionState.value,
    syncState: syncState.value,
    isRealtime: isRealtime.value,
  }))

  /** 认证主体（用户 id）；不满足建立条件（未认证/改密中/缺身份）返回 null。 */
  function currentPrincipal(): string | null {
    if (options.auth.status.value !== 'signed_in') return null
    const user = options.auth.user.value
    if (user === null || user === undefined) return null
    if (user.must_change_password === true) return null
    return user.id
  }

  /**
   * 有资格建立订阅的认证代际标识；不满足条件返回 null。
   * 三元组里每一项变化都会关闭旧订阅：`authz_epoch` 前进 = 授权集变化；`csrf_token` 轮换 =
   * 旧授权上下文作废（06 §4「撤权/会话失效：关闭旧订阅并重新鉴权」，审查第 5 项）。
   */
  function eligibleIdentity(): string | null {
    const principal = currentPrincipal()
    if (principal === null) return null
    return `${principal}\u0000${options.auth.authzEpoch.value ?? ''}\u0000${options.auth.csrfToken.value ?? ''}`
  }

  /**
   * 取用一次自动重建预算。只有预算归属主体变化（真正换了认证主体）时才重新发放；
   * 其余任何自动重建路径（微任务重建、真实鉴权落位后的 watcher 重建）共用同一余额。
   */
  function takeAutoRestartBudget(principal: string): boolean {
    if (budgetPrincipal !== principal) {
      budgetPrincipal = principal
      restartsLeft = MAX_AUTO_RESTARTS
    }
    if (restartsLeft <= 0) return false
    restartsLeft -= 1
    return true
  }

  /** 认证主体真正切换：重新发放预算，并结束上一主体遗留的失效 episode。 */
  function resetBudgetForPrincipal(principal: string): void {
    budgetPrincipal = principal
    restartsLeft = MAX_AUTO_RESTARTS
    awaitingReauth = false
  }

  // 事件载荷不是数据源：同批提示折叠成一次失效通知，由宿主按当前授权重新 GET。
  const pendingKinds = new Set<EntityKind>()
  let invalidationScheduled = false
  function scheduleInvalidation(kind: EntityKind): void {
    if (disposed) return
    pendingKinds.add(kind)
    if (invalidationScheduled) return
    invalidationScheduled = true
    queueMicrotask(() => {
      invalidationScheduled = false
      if (disposed || pendingKinds.size === 0) return
      const kinds = [...pendingKinds] as ReadonlyArray<EntityKind>
      pendingKinds.clear()
      options.onInvalidation?.(kinds)
    })
  }

  function stop(): void {
    active.value = false
    const instance = client.value
    client.value = null
    if (instance !== null) {
      try {
        instance.close()
      } catch {
        // 关闭失败也视为已放弃：调用方不再持有该实例
      }
    }
  }

  /** 建立订阅。预算不在这里发放：调用方必须先通过 takeAutoRestartBudget 或显式重试入口。 */
  function start(identity: string): void {
    if (disposed || !supported.value) return
    sessionIdentity = identity
    client.value = createEventStreamClient({
      ...(options.url === undefined ? {} : { url: options.url }),
      snapshots: options.snapshots,
      ...(options.eventSourceFactory === undefined
        ? {}
        : { eventSourceFactory: options.eventSourceFactory }),
      ...(options.withCredentials === undefined
        ? {}
        : { withCredentials: options.withCredentials }),
      onSnapshot: (kind, items) => { options.onSnapshot?.(kind, items) },
      onEntityUpdate: (kind) => { scheduleInvalidation(kind) },
      onUpdateHint: (kind, id) => { options.onUpdateHint?.(kind, id) },
      onSystemTimeChanged: (payload) => { options.onSystemTimeChanged?.(payload) },
      onError: (error) => { options.onError?.(error) },
      onSessionExpired: (reason) => {
        // events.ts 已在 shutdown() 内关闭订阅并清空其授权投影，本层只需摘除引用并交还宿主。
        active.value = false
        sessionIdentity = null
        // 撤权/失效：先交宿主清投影 + 重新鉴权，再在共享预算内考虑自动重建（审查第 1 项）。
        awaitingReauth = true
        const principal = currentPrincipal() ?? ''
        if (reason === 'permissions.changed') options.onPermissionsChanged?.()
        else options.onSessionExpired?.(reason)
        scheduleAutoRestart(identity, principal)
      },
    })
    active.value = true
  }

  /**
   * 撤权/失效后的有限次自动重建：等宿主的重新鉴权先落位（微任务），且只有会话**真正**重新
   * 处于同一有效代际时才重建。真实 `auth.refresh()` 会同步进入 `checking`，此时本函数直接放弃
   * 重建，把重建权交还给 `syncWithSession`（即真实鉴权结果），既不用 setTimeout 掩盖循环，
   * 也不会让 refresh 的瞬时态骗取预算。
   */
  function scheduleAutoRestart(identity: string, principal: string): void {
    if (disposed || restartsLeft <= 0) return
    queueMicrotask(() => {
      if (disposed || active.value) return
      if (eligibleIdentity() !== identity) return
      if (!takeAutoRestartBudget(principal)) return
      awaitingReauth = false
      start(identity)
    })
  }

  function syncWithSession(): void {
    if (disposed) return
    const identity = eligibleIdentity()
    const principal = currentPrincipal()
    if (identity !== null && principal !== null && supported.value) {
      if (active.value && sessionIdentity === identity) return
      if (budgetPrincipal !== null && budgetPrincipal !== principal) resetBudgetForPrincipal(principal)
      else if (budgetPrincipal === null) budgetPrincipal = principal
      // 失效 episode 未结束（`awaitingReauth`）时，任何自动重建——包括真实鉴权带着**新 token/新
      // epoch** 落位这一条（审查第 1 项预算裁定：身份三元组变化不重置预算）——都必须取票。
      // 只有用户显式重试（`reconnect`）或认证主体真正切换才重新发放。
      if (awaitingReauth && !takeAutoRestartBudget(principal)) {
        // 预算耗尽：保持停止（诚实非实时 + 普通 GET 读取），只能等用户显式重试或换主体
        sessionIdentity = null
        stop()
        return
      }
      awaitingReauth = false
      stop()
      start(identity)
      return
    }
    // `checking` 是「还没知道结果」的瞬时态（`auth.refresh()` 的第一步就是同步置位）：
    // 保留现有订阅与预算，等真实鉴权结果按新一代际决定是否重建（审查第 1 项）。
    if (options.auth.status.value === 'checking' && supported.value) return
    // 会话真正结束（signed_out / force_password / error）或能力缺失：关闭订阅并结束 episode。
    // 授权 buffer/水位/在途快照 GET 由 events.ts 的 `close()` 负责清理（abortInFlight + 清空
    // 水位与成员集合），不能只靠宿主清 DOM 掩盖（审查第 3 项）。
    sessionIdentity = null
    awaitingReauth = false
    stop()
  }

  /**
   * 接线层的触发源必须是**完整认证生命周期**（状态 + 主体 + `authz_epoch` + `csrf_token`），
   * 而不是只 watch `eligibleIdentity()`：后者在 `checking`、`signed_out`、`error`、
   * `force_password` 下都是 null，于是 `signed_in → checking → 终态` 这一跳（真实
   * `auth.refresh()` 的 401/500/强改密三条失败路径）在 null→null 上永不触发，旧订阅会一直
   * 停在 open/active/realtime，其授权下的在途快照 GET 也不 abort（审查第 3 项）。
   */
  const stopWatcher = watch(
    () => [options.auth.status.value, eligibleIdentity()] as const,
    () => { syncWithSession() },
    { immediate: true },
  )
  // 能力位（测试内的运行时会话中翻转）变化时同样重算，绝不在缺失能力时保留假订阅。
  const stopSupportWatcher = watch(supported, () => { syncWithSession() }, { immediate: false })

  return {
    supported,
    active,
    connectionState,
    syncState,
    isRealtime,
    lastError,
    lastSyncReason,
    tone,
    requestResync() {
      const instance = client.value
      // 未被 SSE 管理（无订阅或连接尚未 open）时返回 false：调用方必须回落到普通 GET
      if (instance === null || !active.value || instance.connectionState.value !== 'open') return false
      instance.resync()
      return true
    },
    reconnect() {
      const identity = eligibleIdentity()
      const principal = currentPrincipal()
      if (identity === null || principal === null || !supported.value) return false
      // 用户显式重试是预算重新发放的两个入口之一（另一个是认证主体真正切换）
      resetBudgetForPrincipal(principal)
      stop()
      start(identity)
      return true
    },
    stop,
    dispose() {
      if (disposed) return
      disposed = true
      pendingKinds.clear()
      stopWatcher()
      stopSupportWatcher()
      sessionIdentity = null
      awaitingReauth = false
      budgetPrincipal = null
      restartsLeft = MAX_AUTO_RESTARTS
      stop()
    },
  }
}

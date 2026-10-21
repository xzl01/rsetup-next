<script setup lang="ts">
import { computed, onMounted, onUnmounted, provide, ref, watch } from 'vue'
import AppNotice from './components/AppNotice.vue'
import BaseButton from './components/BaseButton.vue'
import LoginView from './views/LoginView.vue'
import PasswordView from './views/PasswordView.vue'
import SessionsView from './views/SessionsView.vue'
import DevicesView from './views/DevicesView.vue'
import DeviceDetailView from './views/DeviceDetailView.vue'
import TasksView from './views/TasksView.vue'
import TaskDetailView from './views/TaskDetailView.vue'
import AdminApprovalsView from './views/AdminApprovalsView.vue'
import AdminGroupsView from './views/AdminGroupsView.vue'
import AdminUsersView from './views/AdminUsersView.vue'
import AdminRolesView from './views/AdminRolesView.vue'
import AdminGrantsView from './views/AdminGrantsView.vue'
import AdminAuditView from './views/AdminAuditView.vue'
import AdminSystemView from './views/AdminSystemView.vue'
import EventConnectionState from './components/EventConnectionState.vue'
import { createI18n, type Locale } from './i18n'
import { createAuth } from './auth'
import { createEventIntegration } from './event-integration'
import { compareRevision, type EntityKind, type EventStreamError, type SnapshotSource } from './events'
import { createRouter, type AppRoute } from './router'
import { get } from './api'
import {
  assertDeviceListPageShape,
  isU64DecimalString,
  type DeviceItem,
  type DeviceListPage,
} from './types'
import {
  createTaskPreview,
  submitTask,
  cancelTask,
  listTasks,
  getTask,
  getTaskChildren,
  reviewHeldTime,
  releaseUnknownLock,
  type TimeReviewEvidenceKind,
} from './task-requests'
import type { SubTaskItem, TaskItem, TaskPage, TaskPreview } from './task-ops'

const i18nInstance = createI18n()
const { locale, t, setLocale } = i18nInstance
provide('i18n', i18nInstance)

const auth = createAuth()
const router = createRouter(auth)

const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/i

const route = computed(() => router.currentRoute.value)

// 管理端入口只对 `is_admin` 会话可见；路由守卫已在 router.ts 拦截非管理员，
// 这里的显隐只是第二道防线（视图内部还会各自校验 isAdmin 后才发请求）。
const isAdminUser = computed(() => auth.user.value?.is_admin === true)

const ADMIN_ROUTE_LABELS: ReadonlyArray<{ route: AppRoute; labelKey: string }> = [
  { route: { name: 'admin-approvals' }, labelKey: 'admin.nav.approvals' },
  { route: { name: 'admin-groups' }, labelKey: 'admin.nav.groups' },
  { route: { name: 'admin-users' }, labelKey: 'admin.nav.users' },
  { route: { name: 'admin-roles' }, labelKey: 'admin.nav.roles' },
  { route: { name: 'admin-grants' }, labelKey: 'admin.nav.grants' },
  { route: { name: 'admin-audit' }, labelKey: 'admin.nav.audit' },
  { route: { name: 'admin-system' }, labelKey: 'admin.nav.system' },
]

const adminNav = computed(() => ADMIN_ROUTE_LABELS.map((entry) => ({
  route: entry.route,
  label: t(entry.labelKey),
})))

const adminCsrfToken = computed(() => auth.csrfToken.value ?? '')

const tasksPendingDevice = computed<string | null>(() => {
  const current = route.value
  const device = current.name === 'tasks' ? current.query?.device ?? null : null
  return device !== null && devicesList.value.some((d) => d.device_id === device) ? device : null
})

const taskPermissionByDevice = computed<Record<string, string[]>>(() => {
  const permissions: Record<string, string[]> = {}
  for (const device of devicesList.value) {
    permissions[device.device_id] = device.effective_permissions ?? []
  }
  return permissions
})

function handleRebootDevice(deviceId: string) {
  if (!DEVICE_ID_HEX64.test(deviceId)) return
  navigateTo({ name: 'tasks', query: { device: deviceId } })
}

async function adaptPreview(
  operation: 'reboot',
  deviceIds: string[],
  signal?: AbortSignal,
): Promise<TaskPreview> {
  const res = await createTaskPreview(operation, deviceIds, {
    csrfToken: auth.csrfToken.value ?? '',
    signal,
  })
  return res.data
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
  })
  return res.data
}

/**
 * 组件自发列表请求（TasksView 首屏/分页）的唯一出口。
 * `onLoadTasks` 的调用签名不带 signal（既有组件契约与单测不变），因此 AbortController
 * 由本适配器自己持有：SSE 接管或新一轮获取开始时由 `invalidateTasksFetch()` 统一 abort，
 * 并在落地前再核一次代际 + aborted（防忽略 AbortSignal 的实现把旧页写回）。
 */
async function adaptLoadTasks(cursor: string | null): Promise<TaskPage<TaskItem>> {
  invalidateTasksFetch()
  const currentGen = tasksFetchGeneration.value
  const controller = new AbortController()
  tasksFetchRequest = controller
  const res = await listTasks(cursor, { csrfToken: auth.csrfToken.value ?? '', signal: controller.signal })
  if (!isTasksFetchCurrent(currentGen, controller)) throw new Error('stale task page')
  return res.data
}

async function adaptLoadTask(
  taskId: string,
  signal?: AbortSignal,
): Promise<TaskItem> {
  const res = await getTask(taskId, { csrfToken: auth.csrfToken.value ?? '', signal })
  return res.data
}

async function adaptLoadChildren(
  taskId: string,
  cursor: string | null,
  signal?: AbortSignal,
): Promise<TaskPage<SubTaskItem>> {
  const res = await getTaskChildren(taskId, cursor, { csrfToken: auth.csrfToken.value ?? '', signal })
  return res.data
}

async function adaptCancel(
  payload: { taskId: string; expectedRevision: string; confirm: true },
  signal?: AbortSignal,
): Promise<void> {
  await cancelTask({ ...payload, context: { csrfToken: auth.csrfToken.value ?? '', signal } })
}

async function adaptTimeReview(
  payload: { taskId: string; expectedRevision: string; decision: 'resume_original_deadline' | 'expire'; evidence: TimeReviewEvidenceKind },
  signal?: AbortSignal,
): Promise<TaskItem> {
  const res = await reviewHeldTime({ ...payload, context: { csrfToken: auth.csrfToken.value ?? '', signal } })
  return res.data
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
  })
}

const busy = ref(false)
const sessionChecked = ref(false)

// SSE 快照提供的任务首屏；null 表示尚未由 SSE 管理（TasksView 按既有自有 GET 首屏）。
// 声明在设备投影之前：下方两个 immediate watcher 会在 setup 期清理它们（避免 TDZ）。
const tasksSnapshot = ref<TaskItem[] | null>(null)
const tasksSnapshotNextCursor = ref<string | null>(null)
/**
 * SSE/兜底 GET 这条获取路径的失败提示（审查第 4/6 项）：
 * 组件自有的 listError 只覆盖它自己发起的分页失败，SSE 管理期的失败必须由 owner 传入，
 * 否则表格停在旧内容而指示器另行其是；新快照到达时由 owner 与组件同轮清除。
 */
const tasksProjectionError = ref<string | null>(null)
/**
 * 任务首屏投影的 owner 归属（审查 F2）：`false` = SSE 从未接管，首屏 owner 是 TasksView 自己的
 * 初始 GET；`true` = SSE 已接管过（或快照已落地），此后 fallback 与重试的**唯一** owner 是 App，
 * 即使 `tasksSnapshot` 仍为 null 也不得把责任退回已被作废的组件请求。登出/撤权清投影时复位。
 */
const tasksProjectionOwned = ref(false)

const tasksFetchGeneration = ref(0)
let tasksFetchRequest: AbortController | null = null

const devicesList = ref<DeviceItem[]>([])
const devicesNextCursor = ref<string | null>(null)
const devicesLoading = ref(false)
const devicesError = ref<string | null>(null)
/**
 * 设备列表获取代际。用 ref 而不是普通计数：它是传给 DevicesView 的 `:parent-generation`，
 * 只有在**切换当刻**就变成响应式的新值，子组件的分页才会在同一轮里作废（审查第 3 项）。
 */
const devicesGeneration = ref(0)
/** 普通 GET 方向的在途请求；SSE 快照方向由 events.ts 自己的 controller 负责。 */
let deviceFetchRequest: AbortController | null = null

/** 作废设备列表方向的一切在途普通请求：所有权切换当刻立即调用，不等最终提交。 */
function invalidateDeviceFetch(): void {
  devicesGeneration.value += 1
  deviceFetchRequest?.abort()
  deviceFetchRequest = null
  devicesLoading.value = false
}

/** 落地前复核：代际未变、未被 abort（忽略 AbortSignal 的实现也拦得住）、会话仍有效。 */
function isDeviceFetchCurrent(gen: number, controller: AbortController): boolean {
  return devicesGeneration.value === gen && !controller.signal.aborted && auth.status.value === 'signed_in'
}

async function loadDevices() {
  if (auth.status.value !== 'signed_in') return
  // 单一获取者：新的普通 GET 作废旧的（含上一代在途请求），绝不让两条路径同时有效
  invalidateDeviceFetch()
  const currentGen = devicesGeneration.value
  const controller = new AbortController()
  deviceFetchRequest = controller
  devicesLoading.value = true
  devicesError.value = null
  try {
    const res = await get<DeviceListPage>('devices', { signal: controller.signal })
    assertDeviceListPageShape(res.data)
    if (!isDeviceFetchCurrent(currentGen, controller)) return
    // 与 SSE 快照方向同一条 revision 单调口径（审查 P1）：成员与顺序以本页为准，
    // 但同 ID 设备若本页更旧则保留已投影的较新项——SSE 降级到普通 GET 兜底时，
    // 一条读库回退的旧页不得把权限投影与显示名打回旧版本。
    devicesList.value = mergeSnapshotByRevision(
      devicesList.value,
      res.data.items,
      (item) => item.device_id,
    )
    devicesNextCursor.value = res.data.next_cursor
  } catch (err: unknown) {
    if (!isDeviceFetchCurrent(currentGen, controller)) return
    devicesError.value = t('state.error')
    devicesList.value = []
    devicesNextCursor.value = null
  } finally {
    if (devicesGeneration.value === currentGen) devicesLoading.value = false
  }
}

// 得到特定设备的权限投影
function getDevicePermissions(deviceId: string): string[] | undefined {
  const item = devicesList.value.find(d => d.device_id === deviceId)
  return item ? item.effective_permissions : undefined
}

function handlePageLoaded(page: DeviceListPage, parentGen?: number) {
  if (auth.status.value !== 'signed_in') return
  if (parentGen !== undefined && parentGen !== devicesGeneration.value) return
  if (!auth.user.value?.id) return

  // 针对相同 device_id，以后到项的最新投影替换；若后到项可能比先到项更旧，按安全 fail-closed
  // 选取权限交集与后到 display_name；若仅是正常后到分页，收紧已撤回权限。新项则直接追加。
  const map = new Map<string, DeviceItem>()
  for (const item of devicesList.value) {
    map.set(item.device_id, item)
  }
  for (const incoming of page.items) {
    const existing = map.get(incoming.device_id)
    if (existing) {
      // 安全 fail-closed 策略：对于重复项，权限取交集（任何一方未授权或已撤权均视为无权限，绝不盲目扩权）
      const incomingPermsSet = new Set(incoming.effective_permissions)
      const tightenedPerms = existing.effective_permissions.filter(p => incomingPermsSet.has(p))
      map.set(incoming.device_id, {
        ...incoming,
        effective_permissions: tightenedPerms,
      })
    } else {
      map.set(incoming.device_id, incoming)
    }
  }
  devicesList.value = Array.from(map.values())
  devicesNextCursor.value = page.next_cursor
}

watch(
  [() => auth.status.value, () => router.currentRoute.value.name],
  async ([newStatus, routeName]) => {
    if (newStatus === 'signed_in') {
      if (routeName === 'sessions') {
        void auth.listSessions()
      } else if (routeName === 'devices') {
        refreshDeviceProjection()
      } else if (routeName === 'tasks') {
        if (devicesList.value.length === 0 && !devicesLoading.value) refreshDeviceProjection()
        // 回到任务页时若投影已由 App 持有（SSE 或上次兜底 GET），按当前所有权刷新一次
        void refreshTasksProjection()
      } else if (routeName === 'device-detail') {
        const targetId = (router.currentRoute.value as { params?: { id?: string } }).params?.id
        // Deep link 进入详情路由：如果当前还没有该设备的授权缓存，且尚未加载列表，先获取受控最小列表
        if (!getDevicePermissions(targetId || '') && !devicesLoading.value) {
          await loadDevices()
        }
        // 如果确认没有该设备的授权投影，安全重定向回 /devices 列表，且在此之前不挂载 DeviceDetailView
        if (targetId && !getDevicePermissions(targetId)) {
          router.navigate({ name: 'devices' })
        }
      }
    } else {
      // 登出/非 signed_in：先作废在途普通 GET（含分页），再清理设备列表投影
      invalidateDeviceFetch()
      devicesList.value = []
      devicesNextCursor.value = null
      devicesError.value = null
      // SSE 侧的任务首屏投影同属该授权代际，一并作废（不跨会话留存旧数据）
      clearEventProjections()
    }
  },
  { immediate: true },
)

// 授权代际（04-router-and-acceptance.md:35）：登出或授权变更时递增，作为任务相关视图的 :key，
// 强制卸载并重建 TasksView / TaskDetailView —— 其内存中的 tasks 列表、待处理设备勾选与幂等键
// 全部是组件局部状态，随组件卸载一并销毁，绝不跨授权代际残留。
const tasksGeneration = ref(0)

function handleAuthorizationChanged() {
  // ① 任务视图整体重建：旧实例与其在途请求（onBeforeUnmount 的 abort）一起作废
  tasksGeneration.value += 1
  // ② 清除 URL 上的待处理设备 query：深链 `#/tasks?device=<id>` 携带的预选意图属于上一授权代际，
  // 若保留，设备重载后 tasksPendingDevice 会重新命中，重建的 TasksView 会把用户已取消勾选的
  // 设备重新勾回，使已取消的意图跨授权代际复活。导航到无 query 的 #/tasks 一并销毁该意图。
  const current = router.currentRoute.value
  if (current.name === 'tasks' && current.query?.device) {
    router.navigate({ name: 'tasks' })
  }
  // ③ 设备权限投影同样失效：先清空旧投影（fail-closed，绝不继续用旧授权放行危险操作），再按当前授权重载
  invalidateDeviceFetch()
  devicesList.value = []
  devicesNextCursor.value = null
  devicesError.value = null
  invalidateTasksFetch()
  clearEventProjections()
  if (auth.status.value === 'signed_in') void loadDevices()
}

// 授权变更的三个来源：① 登出/会话失效（auth.status 离开 signed_in）；② /auth/me 返回的 authz_epoch
// 前进或身份切换；③ SSE 客户端转发的 `permissions.changed` 命名事件（window 事件，见 onMounted）。
// 初值 null 只作基线记录：首次 me()/login 建立 epoch/身份时尚无旧授权可失效，不触发重建。
let lastAuthzUserId: string | null = null
let lastAuthzEpoch: string | null = null
let lastAuthzCsrfToken: string | null = null

watch(
  [
    () => auth.status.value,
    () => auth.user.value?.id ?? null,
    () => auth.authzEpoch.value ?? null,
    () => auth.csrfToken.value ?? null,
  ],
  ([status, userId, epoch, csrfToken]) => {
    if (status !== 'signed_in') {
      // 登出：任务视图随 signed_in 分区一起卸载；代际仍递增，确保任何旧实例/旧响应一律作废
      tasksGeneration.value += 1
      // `checking` 只是 refresh 的瞬时态：基线必须保留，否则刷新回来后这次 token/epoch 轮换会被
      // 当成「首次基线」而漏掉（审查第 5 项）。会话真正结束才清基线。
      if (status === 'checking') return
      lastAuthzUserId = null
      lastAuthzEpoch = null
      lastAuthzCsrfToken = null
      return
    }
    const identityChanged = lastAuthzUserId !== null && userId !== null && userId !== lastAuthzUserId
    const epochAdvanced = lastAuthzEpoch !== null && epoch !== null && epoch !== lastAuthzEpoch
    // CSRF token 轮换 = 旧授权上下文作废（同主体同 epoch 也要销毁旧任务态、作废旧授权下的 GET）
    const tokenRotated = lastAuthzCsrfToken !== null && csrfToken !== null && csrfToken !== lastAuthzCsrfToken
    if (identityChanged || epochAdvanced || tokenRotated) handleAuthorizationChanged()
    lastAuthzUserId = userId
    lastAuthzEpoch = epoch
    lastAuthzCsrfToken = csrfToken
  },
  { immediate: true },
)

// ── SSE 实时通道接入（AT-18 / 06 §4 U-03）────────────────────────────────────────
// 唯一获取源约定：设备/任务首屏列表在同一时刻只有一个获取者。
//   ① 订阅实际 open（`sseManagingLists`）→ 只由 events.ts 的全量快照来源发起 GET，
//      路由切换与重试按钮都转成 `requestResync()`，不再另发一条普通列表 GET；
//   ② 其余情况（未认证、无 EventSource 能力、connecting/reconnecting/失效）→ 由
//      `loadDevices()` 走普通 GET 兜底，页面显示非实时，绝不把旧快照当实时。
// 事件一律只是失效提示：`onEntityUpdate` 合并成一次 `requestResync()`（当前授权重取），
// App 从不把事件载荷写进列表，也不据事件创建可见成员（未知成员由 events.ts 触发重取）。
// 服务端事件发布方尚未实现，因此这里只覆盖客户端接线，不构成端到端实时验收。

/** 快照首屏的 next_cursor 只属于那次 GET：按 items 数组身份回取，避免旧响应串页。 */
const snapshotCursors = new WeakMap<object, string | null>()

/**
 * 任务首屏投影的所有权状态（审查第 4 项）。
 *
 * 唯一数据 owner 的判定只看一件事：`sseManagingLists`（订阅 active **且实际 open**）。
 * `active` 单独为真并不等于实时——connecting/reconnecting/stale 期间快照来源不会被调用，
 * 若此时仍把 `listManagedExternally` 报成真，TasksView 会跳过自己的首屏 GET，
 * 而 SSE 也不会取数，结果两条路径都不发请求、列表凭空消失。
 */

/** 作废任务首屏方向的一切在途普通请求（SSE 兜底 GET 与组件自发列表请求同属这条路径）。 */
function invalidateTasksFetch(): void {
  tasksFetchGeneration.value += 1
  tasksFetchRequest?.abort()
  tasksFetchRequest = null
}

function isTasksFetchCurrent(gen: number, controller: AbortController): boolean {
  return tasksFetchGeneration.value === gen && !controller.signal.aborted && auth.status.value === 'signed_in'
}

/** 首屏任务页：只取第一页，绝不自动翻 cursor（06 §4 首屏语义）。 */
async function fetchTaskFirstPage(signal: AbortSignal): Promise<TaskPage<TaskItem>> {
  const res = await listTasks(null, { csrfToken: auth.csrfToken.value ?? '', signal })
  return res.data
}

/**
 * 普通 GET 兜底（SSE 未接管/已降级）把一页首屏写进任务投影。
 *
 * 成员集合与顺序仍以这一页为准，但**同一对象的版本不能被更旧的响应打回**（审查 P1）：
 * 断线从 SSE 降级到普通 GET 时，兜底 GET 可能命中读库回退/主从延迟而带着更旧 revision 回来，
 * 早先 `handleEventSnapshot` 里的单调保护管不到这条路，直接赋值就会把 UI 回滚。
 * 与快照方向共用同一个 `mergeSnapshotByRevision`，两条获取路径的单调口径必须一致。
 */
function applyTaskPage(page: TaskPage<TaskItem>): void {
  tasksSnapshot.value = mergeSnapshotByRevision(
    tasksSnapshot.value ?? [],
    page.items,
    (item) => item.id,
  )
  tasksSnapshotNextCursor.value = page.next_cursor
  tasksProjectionError.value = null
}

/**
 * 任务首屏的唯一刷新入口（与 `refreshDeviceProjection` 对称）：
 * SSE 管理期转 `requestResync()`；否则由 App 侧普通 GET 兜底（保留最后合法快照，
 * 失败只如实标记 + 提供重试，不清空表格）。
 *
 * owner 移交判据（审查 F2）：
 *   ① SSE **从未接管**且无快照 → 首屏 owner 仍是 TasksView 自己的初始 GET，这里绝不抢发第二条
 *      竞争首屏（`tasksProjectionOwned === false`）；
 *   ② SSE 已接管过（`tasksProjectionOwned === true`）→ App 是这条投影**唯一**的
 *      fallback/retry owner，即使首快照一次都没成功（`tasksSnapshot` 仍为 null）也必须由 App
 *      兜底与重试。
 * 旧实现用 `tasksSnapshot === null` 当作「交回 TasksView 负责」的信号，但接管当刻组件那条自有
 * GET 已被 abort 且组件不会重发 → 两边互等：列表凭空消失、快照 500 后重试点了不发请求（2→2）。
 */
async function refreshTasksProjection(): Promise<void> {
  if (auth.status.value !== 'signed_in') return
  if (tasksSnapshot.value === null && !tasksProjectionOwned.value) return
  if (sseManagingLists.value && eventStream.requestResync()) {
    invalidateTasksFetch() // SSE 接管当刻：兜底 GET 立即作废，两条路径不同时有效
    tasksProjectionError.value = null
    return
  }
  invalidateTasksFetch()
  const currentGen = tasksFetchGeneration.value
  const controller = new AbortController()
  tasksFetchRequest = controller
  try {
    const page = await fetchTaskFirstPage(controller.signal)
    if (!isTasksFetchCurrent(currentGen, controller)) return
    applyTaskPage(page)
  } catch {
    if (!isTasksFetchCurrent(currentGen, controller)) return
    // 保留最后合法快照：只把「当前投影可能已过期」如实呈现，并提供重试
    tasksProjectionError.value = t('state.error')
  }
}

const eventSnapshots: ReadonlyArray<SnapshotSource> = [
  {
    kind: 'device',
    fetch: async ({ signal }) => {
      try {
        const res = await get<DeviceListPage>('devices', { signal })
        assertDeviceListPageShape(res.data)
        snapshotCursors.set(res.data.items as unknown as object, res.data.next_cursor)
        return res.data.items as unknown as ReadonlyArray<Record<string, unknown>>
      } catch (error) {
        // 快照失败：实时与否由 isRealtime 决定（不在此处推断）；已有合法快照时保留表格，
        // 只在「本来就没数据可展示」时才落到错误态，避免一次失败把已授权列表清空。
        //
        // 错误副作用只能由**当前在途 ticket** 提交（审查 F4）：`signal` 由 events.ts 按同步轮次
        // 下发，新一轮 `reset`/重同步、断线或关闭订阅都会在切换当刻 abort 它。被 abort 的旧轮
        // 即便忽略 AbortSignal 迟到返回 500，它也已经不代表当前投影——把错误写进 DOM 会与
        // 新一轮已成功回写的 realtime 状态自相矛盾。成功回写由 events.ts 的代际票据把关，
        // 失败回写在这里等价复核，二者同代际。
        if (!signal.aborted && devicesList.value.length === 0) devicesError.value = t('state.error')
        throw error
      }
    },
  },
  {
    kind: 'task',
    fetch: async ({ signal }) => {
      try {
        const res = await listTasks(null, { csrfToken: auth.csrfToken.value ?? '', signal })
        snapshotCursors.set(res.data.items as unknown as object, res.data.next_cursor)
        return res.data.items as unknown as ReadonlyArray<Record<string, unknown>>
      } catch (error) {
        // 同上：任务首屏失败如实可见 + 可重试，旧快照留在表格里但不再标实时。
        // 同样只允许当前 ticket 提交错误：被 abort 的旧轮迟到 500 不得覆盖新一轮的 realtime。
        if (!signal.aborted) tasksProjectionError.value = t('state.error')
        throw error
      }
    },
  },
]

/**
 * 同一对象 revision 回退判定（审查 P1）。
 *
 * 快照对「成员集合与顺序」是权威的，但对「同一对象的版本」不是：服务端读库/缓存回退、
 * 主从延迟都可能让下一轮快照带着**更旧的 revision**回来。events.ts 的 revision 水位只前进
 * 不后退（`advanceWatermark`），事件侧也不会把旧 revision 当更新分发，唯独宿主投影这一跳
 * 原先是无条件整体替换——旧 revision 直接把 UI 上的对象打回旧状态。
 *
 * 判定 fail-open 到「接受 incoming」：任一侧缺 revision 或不是 canonical u64 时无法证明回退，
 * 保持既有覆盖语义，绝不因字段缺失把合法新快照整批丢掉。比较走 `compareRevision`
 * （canonical u64 十进制字符串无损比较，不经 Number，u64 全域无精度损失）。
 */
function isRevisionRollback(current: unknown, incoming: unknown): boolean {
  if (!isU64DecimalString(current) || !isU64DecimalString(incoming)) return false
  return compareRevision(incoming, current) < 0
}

function revisionOf(item: unknown): unknown {
  return typeof item === 'object' && item !== null
    ? (item as { revision?: unknown }).revision
    : undefined
}

/**
 * 按 incoming 顺序重建列表（成员与顺序以本轮快照为准，保持结构与有序性），
 * 只对同 ID 对象做 revision 单调保护：incoming 更旧时保留已在投影里的较新对象。
 */
function mergeSnapshotByRevision<T extends object>(
  current: ReadonlyArray<T>,
  incoming: ReadonlyArray<T>,
  idOf: (item: T) => string,
): T[] {
  const previous = new Map<string, T>()
  for (const item of current) previous.set(idOf(item), item)
  return incoming.map((item) => {
    const existing = previous.get(idOf(item))
    if (existing === undefined) return item
    return isRevisionRollback(revisionOf(existing), revisionOf(item)) ? existing : item
  })
}

function handleEventSnapshot(kind: EntityKind, items: ReadonlyArray<Record<string, unknown>>) {
  // 授权投影已失效（登出/撤权回调先于本轮回写）时绝不落数据
  if (auth.status.value !== 'signed_in') return
  const list = items as unknown as ReadonlyArray<object>
  const nextCursor = snapshotCursors.get(list as unknown as object) ?? null
  if (kind === 'device') {
    // 接管当刻作废普通 GET 与分页在途响应（abort + 代际），避免两条获取路径互相覆盖
    invalidateDeviceFetch()
    devicesList.value = mergeSnapshotByRevision(
      devicesList.value,
      list as unknown as DeviceItem[],
      (item) => item.device_id,
    )
    devicesNextCursor.value = nextCursor
    devicesError.value = null
    return
  }
  invalidateTasksFetch()
  tasksProjectionOwned.value = true // 快照已落地：App 成为该投影唯一的 fallback/retry owner
  tasksSnapshot.value = mergeSnapshotByRevision(
    tasksSnapshot.value ?? [],
    list as unknown as TaskItem[],
    (item) => item.id,
  )
  tasksSnapshotNextCursor.value = nextCursor
  tasksProjectionError.value = null
}

/** 认证不安全时才彻底清投影：投影属于旧授权代际，绝不允许跨代回写。 */
function clearEventProjections() {
  invalidateTasksFetch()
  tasksProjectionOwned.value = false // 新授权代际：首屏 owner 交回重建后的 TasksView
  tasksSnapshot.value = null
  tasksSnapshotNextCursor.value = null
  tasksProjectionError.value = null
}

const sseManagingLists = computed(
  () => eventStream.active.value && eventStream.connectionState.value === 'open',
)

/** 列表投影的唯一刷新入口：按当前是否由 SSE 管理分派，二者互斥。 */
function refreshDeviceProjection() {
  if (sseManagingLists.value) {
    eventStream.requestResync()
    return
  }
  void loadDevices()
}

function handleDevicesRetry() {
  // SSE 不可用/未 open 时，重试既给出普通 GET 兜底，也显式尝试重建订阅（用户主动触发）
  if (!sseManagingLists.value) {
    eventStream.reconnect()
    void loadDevices()
    return
  }
  eventStream.requestResync()
}

/**
 * 一轮同步整体失败时的投影错误呈现（审查 P1：设备快照失败会连坐整轮，任务页不得无痕空白）。
 *
 * events.ts 用 `Promise.allSettled` 收齐所有来源后**只要有一个 reject 就整轮不提交**（原子提交，
 * 绝不做半轮污染），因此失败的那一路自己的 catch 只能标记它自己的投影——另一路明明取到了合法
 * 数据，却既没提交、也没任何错误线索。SSE 管理期 TasksView/DevicesView 都不再自发 GET
 * （`listManagedExternally`），结果就是「空白表格 + 无重试入口」且指示器另行其是。
 *
 * 这里只补齐缺失的那半边，且不推翻既有取舍：
 * - 只处理「整轮未提交」的失败码（`SNAPSHOT_FAILED` / `INVALID_SNAPSHOT_ITEM`）；
 *   被 abort 的旧轮在 events.ts 里当代际复核失败即 return，根本不会走到 `fail()`，
 *   所以旧轮迟到的失败不会经这里污染已 realtime 的投影（F4 契约）；
 * - 只在「本来就没数据可展示」时落错误：已有合法快照时保留表格，非实时由指示器如实呈现，
 *   与两个快照来源 catch 里的同一判据保持一致。「没数据可展示」包含**合法空快照 `[]`**
 *   （复审 P2）：首屏取到空列表是一次成功提交，之后每轮聚合失败都不会再有新数据落地，
 *   判据只看 `=== null` 的话，空表格既无错误也无重试按钮，用户永远出不去；
 * - 任务投影只在 App 已认领 owner 时才落错误：否则重试点下去 `refreshTasksProjection()`
 *   会按 owner 判据直接 return（按钮不干活），那种场景的首屏 owner 仍是 TasksView 自己的 GET。
 */
function handleEventStreamError(error: EventStreamError): void {
  if (error.code !== 'SNAPSHOT_FAILED' && error.code !== 'INVALID_SNAPSHOT_ITEM') return
  if (auth.status.value !== 'signed_in') return
  const tasksProjectionEmpty = tasksSnapshot.value === null || tasksSnapshot.value.length === 0
  if (tasksProjectionOwned.value && (tasksProjectionEmpty || error.kind === 'task')) {
    tasksProjectionError.value = t('state.error')
  }
  if (devicesList.value.length === 0) devicesError.value = t('state.error')
}

/**
 * 任务详情（主任务 + children）的刷新信号（审查 P1：详情不在列表快照的覆盖范围内）。
 *
 * SSE 的全量快照来源只有 devices/tasks **列表**，详情页的 `/tasks/:id` 与 `/tasks/:id/children`
 * 不属于任何快照来源——用户停在详情页时 `task.updated` 会被列表重同步「消化」掉，详情永远停在
 * 旧 revision（`tasksGeneration` 只在 auth changed 等少数情况变化，:key 也不会重建组件）。
 * 信号源必须是 `onUpdateHint` 而不是 `onInvalidation`（复审 P1）：后者只由已知成员的
 * `onEntityUpdate` 驱动，深链进入或翻分页进入的任务不在首屏成员集合里，其事件走
 * `unknown-member` 重同步，详情仍是盲区。
 * 信号推进 → TaskDetailView 按当前授权重新拉取主任务与 children（组件实例保留，
 * 已选时间证据与幂等键不丢；其内部 (gen,id)/(gen,cGen,id) 与读取代际令牌照旧拦住迟到响应）。
 */
const taskDetailRefresh = ref(0)

const eventStream = createEventIntegration({
  auth,
  snapshots: eventSnapshots,
  onSnapshot: handleEventSnapshot,
  onInvalidation: () => {
    // 断线期间绝不追发 GET：重同步由下一次实际 open 或用户重试负责。
    // 详情页的失效不在这里送达：本回调只由已知成员的 `onEntityUpdate` 驱动，覆盖面不足，
    // 见下方 `onUpdateHint`（审查 P1）。
    if (sseManagingLists.value) eventStream.requestResync()
  },
  onUpdateHint: (kind, id) => {
    // 详情页不属于列表快照投影，失效提示必须单独送达（审查 P1）。
    // 走 `onInvalidation` 这条路覆盖不到它：那条链路只由 events.ts 的 `onEntityUpdate` 驱动，
    // 而后者只对当前首屏快照的**已知成员**分发——深链进入或翻分页进入的任务不在成员集合里，
    // 其 `task.updated` 走 `unknown-member` 整轮重同步，列表换了、详情仍停在旧 revision。
    // 本通道在成员判定之前直发，因此无论该任务是否在首屏快照中都能送达；再按当前路由的
    // 任务 id 精确匹配，避免别的任务的更新触发无谓的详情 GET。
    if (kind !== 'task') return
    const current = router.currentRoute.value
    if (current.name !== 'task-detail' || current.params.id !== id) return
    taskDetailRefresh.value += 1
  },
  onPermissionsChanged: () => {
    // 清旧授权投影 + 现有重新鉴权路径：任务视图整体重建、设备投影按当前授权重载，再重取 /auth/me
    handleAuthorizationChanged()
    void auth.refresh()
  },
  onSessionExpired: () => {
    void auth.refresh()
  },
  onError: handleEventStreamError,
})

// 所有权切换当刻（不是最终提交时）统一作废另一条获取路径的在途请求（审查第 2/3 项）。
watch(sseManagingLists, (managed, wasManaged) => {
  if (managed === wasManaged) return
  invalidateDeviceFetch()
  invalidateTasksFetch()
  if (managed) {
    // 接管当刻即认领任务首屏（审查 F2）：此后无论首快照是否成功，fallback/retry 的唯一 owner
    // 都是 App——组件那条已被 abort 的自有 GET 不会被重发，两边互等就是「列表凭空消失」的缺陷。
    tasksProjectionOwned.value = true
    return
  }
  // 非认证原因失去管理（断线/重连耗尽/能力缺失）：授权仍有效，最后合法快照属于当前授权，
  // 保留它（06 §4「断线不因此清空已授权数据」）并立即改由普通 GET 兜底续租，诚实显示非实时。
  // 认证类失效（登出/改密/撤权）走 handleAuthorizationChanged → clearEventProjections，二者分开。
  void refreshTasksProjection()
})

function changeLocale(event: Event) {
  setLocale((event.target as HTMLSelectElement).value as Locale)
}

function handleSkipLink(event: Event) {
  event.preventDefault()
  const main = document.getElementById('main-content')
  if (main) {
    main.focus()
  }
}

function navigateTo(route: AppRoute) {
  router.navigate(route)
}

async function submitLogout() {
  if (busy.value) return
  busy.value = true
  try {
    await auth.logout()
  } finally {
    busy.value = false
  }
}

async function retryRefresh() {
  sessionChecked.value = false
  try {
    await auth.refresh()
  } finally {
    sessionChecked.value = true
  }
}

onMounted(async () => {
  // SSE 客户端（后续任务）收到 `permissions.changed` 命名事件后在 window 上转发，App 层据此销毁任务态
  window.addEventListener('permissions.changed', handleAuthorizationChanged)
  try {
    await auth.refresh()
  } finally {
    sessionChecked.value = true
  }
})

onUnmounted(() => {
  window.removeEventListener('permissions.changed', handleAuthorizationChanged)
  // 卸载即关闭订阅：events.ts 的 close() 会 abort 在途快照 GET 并清空水位/成员/缓冲投影
  eventStream.dispose()
  // 普通 GET 方向（设备列表、任务首屏/分页）不属于 events.ts，必须在此显式 abort（审查第 3 项）
  invalidateDeviceFetch()
  invalidateTasksFetch()
  clearEventProjections()
  router.cleanup()
})
</script>

<template>
  <a class="skip-link" href="#main-content" @click.prevent="handleSkipLink" @keydown.enter.prevent="handleSkipLink">{{ t('nav.skip') }}</a>
  <header class="app-header">
    <h1>{{ t('app.title') }}</h1>
    <label for="app-language">{{ t('language.label') }}</label>
    <select id="app-language" :value="locale" @change="changeLocale">
      <option value="zh-CN">简体中文</option>
      <option value="en">English</option>
    </select>
  </header>
  <main id="main-content" class="app-main" tabindex="-1">
    <template v-if="!sessionChecked && auth.status.value === 'checking'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="t('state.loading')">
        <span>{{ t('auth.checking') }}</span>
      </AppNotice>
    </template>
    <template v-else-if="auth.status.value === 'error'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="t('state.error')">
        <span>{{ t('auth.error.generic') }}</span>
        <BaseButton class="auth-retry" :loading-label="t('button.loading')" @click="retryRefresh">
          {{ t('state.retry') }}
        </BaseButton>
      </AppNotice>
    </template>
    <template v-else-if="router.currentRoute.value.name === 'login'">
      <LoginView :auth="auth" :t="t" />
    </template>
    <template v-else-if="router.currentRoute.value.name === 'password'">
      <PasswordView :auth="auth" :t="t" />
    </template>
    <section v-else class="auth-signed-in">
      <h2>{{ t('auth.signedIn.heading') }}</h2>
      <p role="status">{{ auth.user.value?.display_name || auth.user.value?.username }}</p>
      <nav :aria-label="t('auth.sessions.title')" class="auth-nav">
        <BaseButton
          variant="secondary"
          :disabled="busy"
          @click="() => navigateTo({ name: 'devices' })"
        >
          {{ t('nav.devices') }}
        </BaseButton>
        <BaseButton
          variant="secondary"
          :disabled="busy"
          @click="() => navigateTo({ name: 'tasks' })"
        >
          {{ t('nav.tasks') }}
        </BaseButton>
        <BaseButton
          variant="secondary"
          :disabled="busy"
          @click="() => navigateTo({ name: 'sessions' })"
        >
          {{ t('auth.sessions.title') }}
        </BaseButton>
        <template v-if="isAdminUser">
          <BaseButton
            v-for="entry in adminNav"
            :key="entry.label"
            variant="secondary"
            :disabled="busy"
            @click="() => navigateTo(entry.route)"
          >
            {{ entry.label }}
          </BaseButton>
        </template>
      </nav>
      <BaseButton :loading="busy" :loading-label="t('button.loading')" @click="submitLogout">{{ t('auth.logout.label') }}</BaseButton>

      <EventConnectionState :tone="eventStream.tone.value" :locale="locale" />

      <template v-if="router.currentRoute.value.name === 'sessions'">
        <SessionsView :auth="auth" :locale="locale" :t="t" />
      </template>
      <template v-else-if="router.currentRoute.value.name === 'devices'">
        <DevicesView
          :initial-devices="devicesList"
          :initial-next-cursor="devicesNextCursor"
          :loading="devicesLoading"
          :error="devicesError"
          :parent-generation="devicesGeneration"
          @retry="handleDevicesRetry"
          @page-loaded="handlePageLoaded"
          @select-device="(id) => navigateTo({ name: 'device-detail', params: { id } })"
          @reboot-device="handleRebootDevice"
        />
      </template>
      <template v-else-if="router.currentRoute.value.name === 'device-detail'">
        <DeviceDetailView
          v-if="getDevicePermissions(router.currentRoute.value.params.id)"
          :device-id="router.currentRoute.value.params.id"
          :permissions="getDevicePermissions(router.currentRoute.value.params.id)"
        />
      </template>
      <template v-else-if="route.name === 'tasks'">
        <TasksView
          :key="`tasks-${tasksGeneration}`"
          :devices="devicesList"
          :initial-tasks="tasksSnapshot ?? undefined"
          :initial-next-cursor="tasksSnapshot === null ? undefined : tasksSnapshotNextCursor"
          :list-managed-externally="sseManagingLists"
          :projection-error="tasksProjectionError"
          @retry-projection="refreshTasksProjection()"
          :pending-device-id="tasksPendingDevice"
          :csrf-token="auth.csrfToken.value ?? ''"
          @open-task="(id) => navigateTo({ name: 'task-detail', params: { id } })"
          :on-preview="adaptPreview"
          :on-submit="adaptSubmit"
          :on-cancel="adaptCancel"
          :on-load-tasks="adaptLoadTasks"
        />
      </template>
      <template v-else-if="route.name === 'task-detail'">
        <TaskDetailView
          :key="`task-detail-${tasksGeneration}`"
          :task-id="route.params.id"
          :refresh-token="taskDetailRefresh"
          :is-admin="auth.user.value?.is_admin === true"
          :device-permissions="taskPermissionByDevice"
          :on-load-task="adaptLoadTask"
          :on-load-children="adaptLoadChildren"
          :on-cancel="adaptCancel"
          :on-time-review="adaptTimeReview"
          :on-release-lock="adaptReleaseLock"
        />
      </template>
      <template v-else-if="route.name === 'admin-approvals'">
        <AdminApprovalsView :csrf-token="adminCsrfToken" :is-admin="isAdminUser" />
      </template>
      <template v-else-if="route.name === 'admin-groups'">
        <AdminGroupsView :csrf-token="adminCsrfToken" :is-admin="isAdminUser" />
      </template>
      <template v-else-if="route.name === 'admin-users'">
        <AdminUsersView :csrf-token="adminCsrfToken" :is-admin="isAdminUser" />
      </template>
      <template v-else-if="route.name === 'admin-roles'">
        <AdminRolesView :csrf-token="adminCsrfToken" :is-admin="isAdminUser" />
      </template>
      <template v-else-if="route.name === 'admin-grants'">
        <AdminGrantsView :csrf-token="adminCsrfToken" :is-admin="isAdminUser" />
      </template>
      <template v-else-if="route.name === 'admin-audit'">
        <AdminAuditView :csrf-token="adminCsrfToken" :is-admin="isAdminUser" />
      </template>
      <template v-else-if="route.name === 'admin-system'">
        <AdminSystemView :csrf-token="adminCsrfToken" :is-admin="isAdminUser" />
      </template>
      <template v-else>
        <div class="app-placeholder">
          <p>{{ t('app.notConnected') }}</p>
        </div>
      </template>
    </section>
  </main>
  <footer class="app-footer">{{ t('app.title') }}</footer>
</template>

<style scoped>
.skip-link {
  position: absolute;
  inset-block-start: 0;
  inset-inline-start: 0;
  padding: var(--space-2) var(--space-4);
  background: var(--color-surface);
  transform: translateY(-150%);
  z-index: 1;
}
.skip-link:focus { transform: none; }
.app-header, .app-main, .app-footer {
  width: min(100%, 68rem);
  margin-inline: auto;
  padding: var(--space-4);
  min-width: 0;
  overflow-wrap: anywhere;
}
.app-header { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--space-2) var(--space-4); }
/* 标题与语言选择同行，标题不再独占一行抬高整页首屏基线 */
.app-header h1 { margin: 0; margin-inline-end: auto; }
.app-header select { max-width: 100%; min-height: 2.75rem; font: inherit; }
.auth-signed-in {
  display: grid;
  grid-template-columns: auto auto minmax(0, 1fr) auto;
  align-items: center;
  gap: var(--space-3) var(--space-4);
  min-width: 0;
}
/* 已登录身份条：标题、显示名、导航与登出按钮收拢为一行，登出不再被栅格拉伸成整行，
   视图正文从第二行整行铺开，避免主内容被挤到折叠线下 */
.auth-signed-in > h2 { grid-column: 1; margin: 0; }
.auth-signed-in > p[role="status"] { grid-column: 2; margin: 0; }
.auth-signed-in > .auth-nav { grid-column: 3; justify-self: start; }
.auth-signed-in > button { grid-column: 4; justify-self: end; }
.auth-signed-in > :nth-child(n + 5) { grid-column: 1 / -1; }
.auth-nav { display: flex; flex-wrap: wrap; gap: var(--space-2); min-width: 0; }
.auth-retry { margin-inline-start: auto; }
.app-placeholder { display: grid; gap: var(--space-2); padding: var(--space-4); border: 1px dashed var(--color-border, #ccc); border-radius: 4px; }
.app-placeholder p { margin: 0; color: var(--color-text-muted, #666); }
</style>

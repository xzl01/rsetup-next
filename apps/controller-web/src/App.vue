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
import { createI18n, type Locale } from './i18n'
import { createAuth } from './auth'
import { createRouter, type AppRoute } from './router'
import { get } from './api'
import { assertDeviceListPageShape, type DeviceItem, type DeviceListPage } from './types'
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

async function adaptLoadTasks(
  cursor: string | null,
  signal?: AbortSignal,
): Promise<TaskPage<TaskItem>> {
  const res = await listTasks(cursor, { csrfToken: auth.csrfToken.value ?? '', signal })
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

const devicesList = ref<DeviceItem[]>([])
const devicesNextCursor = ref<string | null>(null)
const devicesLoading = ref(false)
const devicesError = ref<string | null>(null)
let devicesGeneration = 0

async function loadDevices() {
  if (devicesLoading.value) return
  devicesLoading.value = true
  devicesError.value = null
  const currentGen = ++devicesGeneration
  try {
    const res = await get<DeviceListPage>('devices')
    assertDeviceListPageShape(res.data)
    if (currentGen !== devicesGeneration || auth.status.value !== 'signed_in') return
    devicesList.value = res.data.items
    devicesNextCursor.value = res.data.next_cursor
  } catch (err: unknown) {
    if (currentGen !== devicesGeneration || auth.status.value !== 'signed_in') return
    devicesError.value = t('state.error')
    devicesList.value = []
    devicesNextCursor.value = null
  } finally {
    if (currentGen === devicesGeneration) {
      devicesLoading.value = false
    }
  }
}

// 得到特定设备的权限投影
function getDevicePermissions(deviceId: string): string[] | undefined {
  const item = devicesList.value.find(d => d.device_id === deviceId)
  return item ? item.effective_permissions : undefined
}

function handlePageLoaded(page: DeviceListPage, parentGen?: number) {
  if (auth.status.value !== 'signed_in') return
  if (parentGen !== undefined && parentGen !== devicesGeneration) return
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
        void loadDevices()
      } else if (routeName === 'tasks' && devicesList.value.length === 0 && !devicesLoading.value) {
        void loadDevices()
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
      // 登出/非 signed_in 状态立即清理设备列表与失效代际
      devicesGeneration++
      devicesList.value = []
      devicesNextCursor.value = null
      devicesError.value = null
      devicesLoading.value = false
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
  devicesGeneration++
  devicesList.value = []
  devicesNextCursor.value = null
  devicesError.value = null
  devicesLoading.value = false
  if (auth.status.value === 'signed_in') void loadDevices()
}

// 授权变更的三个来源：① 登出/会话失效（auth.status 离开 signed_in）；② /auth/me 返回的 authz_epoch
// 前进或身份切换；③ SSE 客户端转发的 `permissions.changed` 命名事件（window 事件，见 onMounted）。
// 初值 null 只作基线记录：首次 me()/login 建立 epoch/身份时尚无旧授权可失效，不触发重建。
let lastAuthzUserId: string | null = null
let lastAuthzEpoch: string | null = null

watch(
  [() => auth.status.value, () => auth.user.value?.id ?? null, () => auth.authzEpoch.value ?? null],
  ([status, userId, epoch]) => {
    if (status !== 'signed_in') {
      // 登出：任务视图随 signed_in 分区一起卸载；代际仍递增，确保任何旧实例/旧响应一律作废
      tasksGeneration.value += 1
      lastAuthzUserId = null
      lastAuthzEpoch = null
      return
    }
    const identityChanged = lastAuthzUserId !== null && userId !== null && userId !== lastAuthzUserId
    const epochAdvanced = lastAuthzEpoch !== null && epoch !== null && epoch !== lastAuthzEpoch
    if (identityChanged || epochAdvanced) handleAuthorizationChanged()
    lastAuthzUserId = userId
    lastAuthzEpoch = epoch
  },
  { immediate: true },
)

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
          @retry="loadDevices"
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

<script setup lang="ts">
// 用户管理页（spec 02 §4）：用户增改、停用/恢复、重置密码与有效权限来源。
//
// 安全约束：
// - 创建与重置密码返回的 `temporary_password` 只在一次性模态框中呈现：不写 localStorage、
//   不写日志、关闭即从内存清除；关闭后无法再次查看。
// - 最后管理员保护：后端 409 `LAST_ADMIN` 显式呈现，界面不假装可绕过。
// - 停用与降级都是 CAS 补丁，版本冲突由服务端裁决。
import { computed, inject, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'
import { createI18n } from '../i18n'
import {
  adminErrorCode,
  adminErrorKey,
  createUser,
  isAbortedError,
  listEffectivePermissions,
  listUsers,
  patchUser,
  resetUserPassword,
  type AdminEffectivePermissionItem,
  type AdminUserItem,
} from '../admin-api'
import { createAdminListResource } from '../admin-list'

const props = defineProps<{ csrfToken: string; isAdmin: boolean }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = i18n.t
const toneLabel = computed(() => (i18n.locale.value === 'en' ? 'Status' : '状态'))

const list = createAdminListResource<AdminUserItem>(
  (cursor, signal) => listUsers(cursor, { csrfToken: props.csrfToken, signal }),
)

const busy = ref(false)
const actionErrorKey = ref<string | null>(null)
const lastAdminBlocked = ref(false)
const username = ref('')
const displayName = ref('')
const isAdminFlag = ref(false)
const formInvalid = ref(false)

/** 一次性临时密码：只存在于组件内存，模态框关闭立即清空。 */
const temporaryPassword = ref<string | null>(null)
const dialog = ref<HTMLElement | null>(null)
const closeButton = ref<HTMLElement | null>(null)
const permissionsDraft = ref<{ userId: string; items: AdminEffectivePermissionItem[] } | null>(null)

function ctx() {
  return { csrfToken: props.csrfToken }
}

function handleError(error: unknown): void {
  if (isAbortedError(error)) return
  actionErrorKey.value = adminErrorKey(error)
  lastAdminBlocked.value = adminErrorCode(error) === 'LAST_ADMIN'
}

async function submitCreate(): Promise<void> {
  if (busy.value) return
  if (username.value.trim().length === 0 || displayName.value.trim().length === 0) {
    formInvalid.value = true
    return
  }
  formInvalid.value = false
  lastAdminBlocked.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    const res = await createUser(username.value.trim(), displayName.value.trim(), isAdminFlag.value, ctx())
    temporaryPassword.value = res.data.temporary_password
    username.value = ''
    displayName.value = ''
    isAdminFlag.value = false
    await list.reload()
  } catch (error: unknown) {
    handleError(error)
  } finally {
    busy.value = false
  }
}

async function setActive(user: AdminUserItem, active: boolean): Promise<void> {
  if (busy.value) return
  lastAdminBlocked.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    await patchUser(user.id, { expectedRevision: user.revision, active }, ctx())
    await list.reload()
  } catch (error: unknown) {
    handleError(error)
  } finally {
    busy.value = false
  }
}

async function resetPassword(user: AdminUserItem): Promise<void> {
  if (busy.value) return
  lastAdminBlocked.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    const res = await resetUserPassword(user.id, user.revision, ctx())
    // 重置密码会撤销该账号既有会话，列表需重新拉取以反映 must_change_password
    temporaryPassword.value = res.data.temporary_password
    await list.reload()
  } catch (error: unknown) {
    handleError(error)
  } finally {
    busy.value = false
  }
}

async function loadPermissions(user: AdminUserItem): Promise<void> {
  if (busy.value) return
  actionErrorKey.value = null
  busy.value = true
  try {
    const res = await listEffectivePermissions(user.id, {}, ctx())
    permissionsDraft.value = { userId: user.id, items: res.data.items }
  } catch (error: unknown) {
    handleError(error)
  } finally {
    busy.value = false
  }
}

function closeDialog(): void {
  // 关闭即销毁：临时密码绝不在界面上二次可见
  temporaryPassword.value = null
}

function onDialogKeydown(event: KeyboardEvent): void {
  if (event.key === 'Escape') {
    event.preventDefault()
    closeDialog()
    return
  }
  if (event.key !== 'Tab') return
  const focusables = dialog.value?.querySelectorAll<HTMLElement>('button:not([disabled])') ?? []
  if (focusables.length === 0) return
  const first = focusables[0] as HTMLElement
  const last = focusables[focusables.length - 1] as HTMLElement
  const active = document.activeElement
  if (event.shiftKey && (active === first || active === dialog.value)) {
    event.preventDefault()
    last.focus()
  } else if (!event.shiftKey && active === last) {
    event.preventDefault()
    first.focus()
  }
}

watch(temporaryPassword, async (value) => {
  if (value === null) return
  await nextTick()
  closeButton.value?.focus()
})

function reload(): void {
  actionErrorKey.value = null
  lastAdminBlocked.value = false
  void list.reload()
}

onMounted(() => {
  if (props.isAdmin) void list.reload()
})

watch(() => props.isAdmin, (allowed) => {
  if (!allowed) {
    list.reset()
    temporaryPassword.value = null
    permissionsDraft.value = null
  } else {
    void list.reload()
  }
})

watch(() => props.csrfToken, () => {
  list.reset()
  temporaryPassword.value = null
  permissionsDraft.value = null
  if (props.isAdmin) void list.reload()
})

onBeforeUnmount(() => {
  list.reset()
  temporaryPassword.value = null
})
</script>

<template>
  <section class="admin-view" :aria-label="t('admin.users.title')">
    <header class="admin-view__header">
      <h3>{{ t('admin.users.title') }}</h3>
      <BaseButton variant="secondary" :disabled="busy" :loading="busy && list.loading.value"
        :loading-label="t('button.loading')" @click="reload">
        {{ t('admin.common.reload') }}
      </BaseButton>
    </header>

    <AppNotice v-if="!isAdmin" tone="warning" :tone-label="toneLabel" :title="t('auth.error.denied')">
      <span>{{ t('admin.denied') }}</span>
    </AppNotice>

    <template v-else>
      <AppNotice v-if="list.errorKey.value || actionErrorKey" tone="error" :tone-label="toneLabel"
        :title="t('state.error')">
        <span>{{ t(list.errorKey.value ?? actionErrorKey ?? 'admin.error.generic') }}</span>
        <span v-if="lastAdminBlocked" data-testid="last-admin-notice">{{ t('admin.error.lastAdmin') }}</span>
        <BaseButton class="admin-view__retry" :disabled="busy" @click="reload">{{ t('state.retry') }}</BaseButton>
      </AppNotice>

      <form class="admin-view__panel" @submit.prevent="submitCreate">
        <BaseInput id="user-create-username" v-model="username" :label="t('admin.users.usernameLabel')"
          :error="formInvalid ? t('admin.common.fieldError') : undefined" :disabled="busy" />
        <BaseInput id="user-create-display-name" v-model="displayName" :label="t('admin.users.displayNameLabel')"
          :disabled="busy" />
        <label class="admin-view__confirm">
          <input id="user-create-is-admin" v-model="isAdminFlag" type="checkbox" :disabled="busy" />
          {{ t('admin.users.adminFlag') }}
        </label>
        <BaseButton type="submit" :disabled="busy" :loading="busy" :loading-label="t('button.loading')">
          {{ t('admin.users.create') }}
        </BaseButton>
      </form>

      <p v-if="list.loading.value && list.items.value.length === 0">{{ t('state.loading') }}</p>
      <p v-else-if="!list.loading.value && !list.errorKey.value && list.items.value.length === 0">
        {{ t('admin.common.empty') }}
      </p>

      <ul v-if="list.items.value.length > 0" class="admin-view__list">
        <li v-for="user in list.items.value" :key="user.id" class="admin-view__item" :data-testid="`user-${user.id}`">
          <div class="admin-view__item-main">
            <strong>{{ user.username }}</strong>
            <span>{{ user.display_name }}</span>
            <span v-if="user.is_admin" class="admin-view__badge">{{ t('admin.users.adminFlag') }}</span>
            <span class="admin-view__badge">
              {{ user.active ? t('admin.users.active') : t('admin.users.inactive') }}
            </span>
            <span v-if="user.must_change_password" data-testid="must-change-password" class="admin-view__badge">
              {{ t('admin.users.mustChangePassword') }}
            </span>
            <span>{{ t('admin.common.revision') }}: {{ user.revision }}</span>
          </div>
          <div class="admin-view__actions">
            <BaseButton variant="secondary" :disabled="busy" :loading-label="t('button.loading')"
              @click="() => setActive(user, !user.active)">
              {{ user.active ? t('admin.users.deactivate') : t('admin.users.activate') }}
            </BaseButton>
            <BaseButton variant="danger" :disabled="busy" :loading-label="t('button.loading')"
              @click="() => resetPassword(user)">
              {{ t('admin.users.resetPassword') }}
            </BaseButton>
            <BaseButton variant="secondary" :disabled="busy" :loading-label="t('button.loading')"
              @click="() => loadPermissions(user)">
              {{ t('admin.users.effectivePermissions') }}
            </BaseButton>
          </div>
        </li>
      </ul>

      <div v-if="permissionsDraft" class="admin-view__panel" data-testid="effective-permissions-panel">
        <h4>{{ t('admin.users.effectivePermissions') }}</h4>
        <p>{{ permissionsDraft.userId }}</p>
        <ul>
          <li v-for="row in permissionsDraft.items" :key="row.device_id" :data-testid="`effective-${row.device_id}`">
            <code>{{ row.device_id }}</code>: {{ row.effective_permissions.join(', ') }}
          </li>
        </ul>
      </div>

      <div v-if="list.nextCursor.value" class="admin-view__pagination">
        <BaseButton variant="secondary" :disabled="busy" :loading="busy && list.loading.value"
          :loading-label="t('button.loading')" @click="() => list.loadMore()">
          {{ t('admin.common.loadMore') }}
        </BaseButton>
      </div>
    </template>

    <div v-if="temporaryPassword !== null" ref="dialog" class="admin-view__overlay"
      role="dialog" aria-modal="true" aria-labelledby="temp-password-title" tabindex="-1"
      data-testid="temporary-password-dialog" @keydown="onDialogKeydown">
      <div class="admin-view__dialog">
        <h4 id="temp-password-title">{{ t('admin.users.temporaryPasswordTitle') }}</h4>
        <p>{{ t('admin.users.temporaryPasswordNotice') }}</p>
        <label for="temporary-password-value">{{ t('admin.users.temporaryPasswordLabel') }}</label>
        <output id="temporary-password-value" data-testid="temporary-password-value">{{ temporaryPassword }}</output>
        <button type="button" class="admin-view__close" ref="closeButton" data-testid="temporary-password-close"
          @click="closeDialog">
          {{ t('admin.users.temporaryPasswordClose') }}
        </button>
      </div>
    </div>
  </section>
</template>

<style scoped>
.admin-view { display: grid; gap: var(--space-3); margin-top: var(--space-4); border-top: 1px solid var(--color-border, #ccc); padding-top: var(--space-4); min-width: 0; }
.admin-view__header { display: flex; justify-content: space-between; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__header h3 { margin: 0; }
.admin-view__list { list-style: none; padding: 0; margin: 0; display: grid; gap: var(--space-2); }
.admin-view__item { display: flex; justify-content: space-between; gap: var(--space-2); flex-wrap: wrap; padding: var(--space-2); border: 1px solid var(--color-border, #eee); border-radius: 4px; }
.admin-view__item-main { display: flex; gap: var(--space-2); flex-wrap: wrap; align-items: baseline; overflow-wrap: anywhere; }
.admin-view__badge { background: var(--color-surface-subtle, #f1f5f9); border-radius: 9999px; padding: 2px 8px; font-size: 0.875rem; }
.admin-view__actions { display: flex; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__panel { display: grid; gap: var(--space-2); padding: var(--space-3); border: 1px dashed var(--color-border, #ccc); border-radius: 4px; }
.admin-view__confirm { display: flex; align-items: center; gap: var(--space-2); }
.admin-view__pagination { display: flex; justify-content: center; }
.admin-view__retry { margin-inline-start: auto; }
.admin-view__overlay { position: fixed; inset: 0; display: grid; place-items: center; background: rgba(15, 23, 42, 0.55); padding: var(--space-4); z-index: 10; }
.admin-view__dialog { display: grid; gap: var(--space-2); width: min(100%, 32rem); padding: var(--space-4); background: var(--color-surface, #fff); border-radius: 8px; overflow-wrap: anywhere; }
.admin-view__dialog h4 { margin: 0; }
.admin-view__close { min-height: 2.75rem; font: inherit; padding: var(--space-2) var(--space-4); justify-self: start; }
</style>

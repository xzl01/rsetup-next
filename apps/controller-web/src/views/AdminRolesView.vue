<script setup lang="ts">
// 角色与权限页（spec 02 §4）：角色增改归档与权限点清单。
//
// 安全约束：
// - 权限点必须来自后端权限目录：客户端只做形状校验（非空、去重、无空格），
//   目录合法性由服务端 400 裁决，界面不维护第二套权限常量表。
// - 内置角色不可归档：按钮禁用并在误触时给出说明，不发送必然失败的请求。
import { computed, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'
import { createI18n } from '../i18n'
import {
  adminErrorKey,
  createRole,
  isAbortedError,
  isValidAdminName,
  listRoles,
  patchRole,
  type AdminRoleItem,
} from '../admin-api'
import { createAdminListResource } from '../admin-list'

const props = defineProps<{ csrfToken: string; isAdmin: boolean }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = i18n.t
const toneLabel = computed(() => (i18n.locale.value === 'en' ? 'Status' : '状态'))

const list = createAdminListResource<AdminRoleItem>(
  (cursor, signal) => listRoles(cursor, { csrfToken: props.csrfToken, signal }),
)

const busy = ref(false)
const actionErrorKey = ref<string | null>(null)
const newName = ref('')
const newPermissions = ref('')
const formInvalid = ref(false)
const builtinBlocked = ref(false)
const editing = ref<{ id: string; name: string; permissions: string } | null>(null)

function ctx() {
  return { csrfToken: props.csrfToken }
}

/** 逗号/空白分隔的权限点集合：去空、去重、保持输入顺序。 */
function parsePermissions(raw: string): string[] {
  const seen = new Set<string>()
  const ordered: string[] = []
  for (const token of raw.split(/[,\s]+/)) {
    const value = token.trim()
    if (value.length === 0 || seen.has(value)) continue
    seen.add(value)
    ordered.push(value)
  }
  return ordered
}

async function submitCreate(): Promise<void> {
  if (busy.value) return
  const permissions = parsePermissions(newPermissions.value)
  if (!isValidAdminName(newName.value) || permissions.length === 0) {
    formInvalid.value = true
    return
  }
  formInvalid.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    await createRole(newName.value, permissions, ctx())
    newName.value = ''
    newPermissions.value = ''
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function submitPatch(): Promise<void> {
  const draft = editing.value
  if (!draft || busy.value) return
  const role = list.items.value.find((item) => item.id === draft.id)
  if (!role) return
  const permissions = parsePermissions(draft.permissions)
  if (!isValidAdminName(draft.name) || permissions.length === 0) {
    formInvalid.value = true
    return
  }
  formInvalid.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    await patchRole(draft.id, { expectedRevision: role.revision, name: draft.name, permissions }, ctx())
    editing.value = null
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function archiveRole(role: AdminRoleItem): Promise<void> {
  if (busy.value) return
  if (role.builtin) {
    // 内置角色归档必然被服务端拒绝：不发送请求，直接给出说明。
    builtinBlocked.value = true
    return
  }
  if (role.archived) return
  builtinBlocked.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    await patchRole(role.id, { expectedRevision: role.revision, archived: true }, ctx())
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

function reload(): void {
  actionErrorKey.value = null
  builtinBlocked.value = false
  void list.reload()
}

onMounted(() => {
  if (props.isAdmin) void list.reload()
})

watch(() => props.isAdmin, (allowed) => {
  if (!allowed) {
    list.reset()
    editing.value = null
  } else {
    void list.reload()
  }
})

watch(() => props.csrfToken, () => {
  list.reset()
  editing.value = null
  if (props.isAdmin) void list.reload()
})

onBeforeUnmount(() => list.reset())
</script>

<template>
  <section class="admin-view" :aria-label="t('admin.roles.title')">
    <header class="admin-view__header">
      <h3>{{ t('admin.roles.title') }}</h3>
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
        <BaseButton class="admin-view__retry" :disabled="busy" @click="reload">{{ t('state.retry') }}</BaseButton>
      </AppNotice>

      <AppNotice v-if="builtinBlocked" tone="warning" :tone-label="toneLabel" :title="t('common.error')">
        <span data-testid="builtin-archive-blocked">{{ t('admin.roles.builtinArchiveBlocked') }}</span>
      </AppNotice>

      <form class="admin-view__panel" @submit.prevent="submitCreate">
        <BaseInput id="role-create-name" v-model="newName" :label="t('admin.roles.nameLabel')"
          :error="formInvalid ? t('admin.common.fieldError') : undefined" :disabled="busy" />
        <BaseInput id="role-create-permissions" v-model="newPermissions" :label="t('admin.roles.permissionsLabel')"
          :hint="t('admin.roles.permissionsHint')" :disabled="busy" />
        <BaseButton type="submit" :disabled="busy" :loading="busy" :loading-label="t('button.loading')">
          {{ t('admin.roles.create') }}
        </BaseButton>
      </form>

      <p v-if="list.loading.value && list.items.value.length === 0">{{ t('state.loading') }}</p>
      <p v-else-if="!list.loading.value && !list.errorKey.value && list.items.value.length === 0">
        {{ t('admin.common.empty') }}
      </p>

      <ul v-if="list.items.value.length > 0" class="admin-view__list">
        <li v-for="role in list.items.value" :key="role.id" class="admin-view__item" :data-testid="`role-${role.id}`">
          <div class="admin-view__item-main">
            <strong>{{ role.name }}</strong>
            <code>{{ role.id }}</code>
            <span data-testid="role-permissions">{{ role.permissions.join(', ') }}</span>
            <span>{{ t('admin.common.revision') }}: {{ role.revision }}</span>
            <span v-if="role.builtin" class="admin-view__badge">{{ t('admin.roles.builtin') }}</span>
            <span v-if="role.archived" class="admin-view__badge">{{ t('admin.roles.archived') }}</span>
          </div>
          <div class="admin-view__actions">
            <BaseButton variant="secondary" :disabled="busy || role.archived" :loading-label="t('button.loading')"
              @click="editing = { id: role.id, name: role.name, permissions: role.permissions.join(', ') }">
              {{ t('admin.common.rename') }}
            </BaseButton>
            <BaseButton variant="danger" :disabled="busy || role.archived || role.builtin"
              :loading-label="t('button.loading')" @click="() => archiveRole(role)">
              {{ t('admin.roles.archive') }}
            </BaseButton>
          </div>
        </li>
      </ul>

      <div v-if="editing" class="admin-view__panel" data-testid="role-edit-panel">
        <BaseInput id="role-edit-name" v-model="editing.name" :label="t('admin.roles.nameLabel')"
          :error="formInvalid ? t('admin.common.fieldError') : undefined" :disabled="busy" />
        <BaseInput id="role-edit-permissions" v-model="editing.permissions" :label="t('admin.roles.permissionsLabel')"
          :hint="t('admin.roles.permissionsHint')" :disabled="busy" />
        <div class="admin-view__actions">
          <BaseButton :disabled="busy" @click="submitPatch">{{ t('admin.common.confirmAction') }}</BaseButton>
          <BaseButton variant="secondary" :disabled="busy" @click="editing = null">{{ t('common.cancel') }}</BaseButton>
        </div>
      </div>

      <div v-if="list.nextCursor.value" class="admin-view__pagination">
        <BaseButton variant="secondary" :disabled="busy" :loading="busy && list.loading.value"
          :loading-label="t('button.loading')" @click="() => list.loadMore()">
          {{ t('admin.common.loadMore') }}
        </BaseButton>
      </div>
    </template>
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
.admin-view__pagination { display: flex; justify-content: center; }
.admin-view__retry { margin-inline-start: auto; }
</style>

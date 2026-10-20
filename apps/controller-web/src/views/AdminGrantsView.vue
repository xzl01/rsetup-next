<script setup lang="ts">
// 授权关系页（spec 02 §4）：授权列表、创建（source/scope 严格互斥）与撤销。
//
// 安全约束：
// - source 与 scope 的互锁在前端先行校验（role↔role_id、direct↔permissions、
//   all/group/device 各自字段），非法组合直接拒发请求，避免污染审计；
//   最终合法性仍由服务端裁决。
// - 撤销授权是 CAS 删除（DELETE 仍需 JSON body 与 CSRF），版本冲突显式提示。
import { computed, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'
import { createI18n } from '../i18n'
import {
  adminErrorKey,
  createGrant,
  deleteGrant,
  isAbortedError,
  listGrants,
  validateGrantInput,
  type AdminGrantItem,
  type AdminGrantScope,
  type AdminGrantSource,
} from '../admin-api'
import { createAdminListResource } from '../admin-list'

const props = defineProps<{ csrfToken: string; isAdmin: boolean }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = i18n.t
const toneLabel = computed(() => (i18n.locale.value === 'en' ? 'Status' : '状态'))

const userFilter = ref('')
const appliedFilter = ref('')

const list = createAdminListResource<AdminGrantItem>(
  (cursor, signal) => listGrants({ userId: appliedFilter.value || null, cursor },
    { csrfToken: props.csrfToken, signal }),
)

const busy = ref(false)
const actionErrorKey = ref<string | null>(null)
const combinationInvalid = ref(false)

const userId = ref('')
const sourceKind = ref<'role' | 'direct'>('role')
const roleId = ref('')
const directPermissions = ref('')
const scopeKind = ref<'all' | 'group' | 'device'>('all')
const groupId = ref('')
const deviceId = ref('')

function ctx() {
  return { csrfToken: props.csrfToken }
}

function buildSource(): AdminGrantSource {
  if (sourceKind.value === 'role') return { kind: 'role', role_id: roleId.value.trim() }
  const permissions = directPermissions.value
    .split(/[,\s]+/)
    .map((value) => value.trim())
    .filter((value) => value.length > 0)
  return { kind: 'direct', permissions }
}

function buildScope(): AdminGrantScope {
  if (scopeKind.value === 'group') return { kind: 'group', group_id: groupId.value.trim() }
  if (scopeKind.value === 'device') return { kind: 'device', device_id: deviceId.value.trim() }
  return { kind: 'all' }
}

function sourceLabel(source: AdminGrantSource): string {
  if (source.kind === 'role') return `${t('admin.grants.source.role')} ${source.role_id ?? ''}`
  return `${t('admin.grants.source.direct')}: ${(source.permissions ?? []).join(', ')}`
}

function scopeLabel(scope: AdminGrantScope): string {
  if (scope.kind === 'all') return t('admin.grants.scope.all')
  if (scope.kind === 'group') return `${t('admin.grants.scope.group')} ${scope.group_id ?? ''}`
  return `${t('admin.grants.scope.device')} ${scope.device_id ?? ''}`
}

async function submitCreate(): Promise<void> {
  if (busy.value) return
  const input = { userId: userId.value.trim(), source: buildSource(), scope: buildScope() }
  if (!validateGrantInput(input)) {
    combinationInvalid.value = true
    return
  }
  combinationInvalid.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    await createGrant(input, ctx())
    userId.value = ''
    roleId.value = ''
    directPermissions.value = ''
    groupId.value = ''
    deviceId.value = ''
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function removeGrant(grant: AdminGrantItem): Promise<void> {
  if (busy.value) return
  actionErrorKey.value = null
  busy.value = true
  try {
    await deleteGrant(grant.id, grant.revision, ctx())
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

function reload(): void {
  actionErrorKey.value = null
  combinationInvalid.value = false
  void list.reload()
}

/** 筛选条件绑定签名游标：应用新筛选必须从第一页重新开始。 */
function applyFilter(): void {
  appliedFilter.value = userFilter.value.trim()
  reload()
}

onMounted(() => {
  if (props.isAdmin) void list.reload()
})

watch(() => props.isAdmin, (allowed) => {
  if (!allowed) list.reset()
  else void list.reload()
})

watch(() => props.csrfToken, () => {
  list.reset()
  if (props.isAdmin) void list.reload()
})

onBeforeUnmount(() => list.reset())
</script>

<template>
  <section class="admin-view" :aria-label="t('admin.grants.title')">
    <header class="admin-view__header">
      <h3>{{ t('admin.grants.title') }}</h3>
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

      <div class="admin-view__toolbar">
        <BaseInput id="grant-user-filter" v-model="userFilter" :label="t('admin.grants.userFilter')" :disabled="busy" />
        <BaseButton variant="secondary" :disabled="busy" @click="applyFilter">{{ t('admin.audit.applyFilters') }}</BaseButton>
      </div>

      <form class="admin-view__panel" @submit.prevent="submitCreate">
        <BaseInput id="grant-create-user" v-model="userId" :label="t('admin.grants.userLabel')" :disabled="busy" />

        <fieldset>
          <legend>{{ t('admin.grants.sourceLabel') }}</legend>
          <label>
            <input id="grant-source-role" v-model="sourceKind" type="radio" value="role" :disabled="busy" />
            {{ t('admin.grants.source.role') }}
          </label>
          <label>
            <input id="grant-source-direct" v-model="sourceKind" type="radio" value="direct" :disabled="busy" />
            {{ t('admin.grants.source.direct') }}
          </label>
        </fieldset>
        <BaseInput v-if="sourceKind === 'role'" id="grant-role-id" v-model="roleId"
          :label="t('admin.grants.roleIdLabel')" :disabled="busy" />
        <BaseInput v-else id="grant-direct-permissions" v-model="directPermissions"
          :label="t('admin.roles.permissionsLabel')" :hint="t('admin.roles.permissionsHint')" :disabled="busy" />

        <fieldset>
          <legend>{{ t('admin.grants.scopeLabel') }}</legend>
          <label>
            <input id="grant-scope-all" v-model="scopeKind" type="radio" value="all" :disabled="busy" />
            {{ t('admin.grants.scope.all') }}
          </label>
          <label>
            <input id="grant-scope-group" v-model="scopeKind" type="radio" value="group" :disabled="busy" />
            {{ t('admin.grants.scope.group') }}
          </label>
          <label>
            <input id="grant-scope-device" v-model="scopeKind" type="radio" value="device" :disabled="busy" />
            {{ t('admin.grants.scope.device') }}
          </label>
        </fieldset>
        <BaseInput v-if="scopeKind === 'group'" id="grant-group-id" v-model="groupId"
          :label="t('admin.grants.groupIdLabel')" :disabled="busy" />
        <BaseInput v-if="scopeKind === 'device'" id="grant-device-id" v-model="deviceId"
          :label="t('admin.grants.deviceIdLabel')" :disabled="busy" />

        <p v-if="combinationInvalid" data-testid="grant-invalid-combination">{{ t('admin.grants.invalidCombination') }}</p>
        <BaseButton type="submit" :disabled="busy" :loading="busy" :loading-label="t('button.loading')">
          {{ t('admin.grants.create') }}
        </BaseButton>
      </form>

      <p v-if="list.loading.value && list.items.value.length === 0">{{ t('state.loading') }}</p>
      <p v-else-if="!list.loading.value && !list.errorKey.value && list.items.value.length === 0">
        {{ t('admin.common.empty') }}
      </p>

      <ul v-if="list.items.value.length > 0" class="admin-view__list">
        <li v-for="grant in list.items.value" :key="grant.id" class="admin-view__item" :data-testid="`grant-${grant.id}`">
          <div class="admin-view__item-main">
            <code>{{ grant.id }}</code>
            <span>{{ t('admin.grants.userLabel') }}: {{ grant.user_id }}</span>
            <span>{{ t('admin.grants.sourceLabel') }}: {{ sourceLabel(grant.source) }}</span>
            <span>{{ t('admin.grants.scopeLabel') }}: {{ scopeLabel(grant.scope) }}</span>
            <span>{{ t('admin.common.revision') }}: {{ grant.revision }}</span>
          </div>
          <BaseButton variant="danger" :disabled="busy" :loading-label="t('button.loading')"
            @click="() => removeGrant(grant)">
            {{ t('admin.grants.delete') }}
          </BaseButton>
        </li>
      </ul>

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
.admin-view__toolbar { display: flex; align-items: flex-end; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__list { list-style: none; padding: 0; margin: 0; display: grid; gap: var(--space-2); }
.admin-view__item { display: flex; justify-content: space-between; gap: var(--space-2); flex-wrap: wrap; padding: var(--space-2); border: 1px solid var(--color-border, #eee); border-radius: 4px; }
.admin-view__item-main { display: flex; gap: var(--space-2); flex-wrap: wrap; align-items: baseline; overflow-wrap: anywhere; }
.admin-view__panel { display: grid; gap: var(--space-2); padding: var(--space-3); border: 1px dashed var(--color-border, #ccc); border-radius: 4px; }
.admin-view__panel fieldset { display: flex; gap: var(--space-3); flex-wrap: wrap; border: 1px solid var(--color-border, #eee); border-radius: 4px; }
.admin-view__panel label { display: flex; align-items: center; gap: var(--space-1); }
.admin-view__pagination { display: flex; justify-content: center; }
.admin-view__retry { margin-inline-start: auto; }
</style>

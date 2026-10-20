<script setup lang="ts">
// 审计日志页（spec 02 §7）：只读、脱敏、复合游标分页。
//
// 安全约束：
// - 审计数据只读呈现：不构造任何写请求，不回显 `params_redacted` 之外的参数；
//   事件 `event_seq` 与 `process_epoch` 原样以字符串显示，避免 JS number 精度丢失。
// - 分页游标是服务端签名的 `process_epoch,event_seq,id` 复合游标：客户端只回传
//   不透明 token，绝不自行拼造；筛选条件变化后旧游标立即作废，重新从第一页开始。
import { computed, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'
import { createI18n } from '../i18n'
import {
  isCanonicalUuidV4,
  isDeviceIdHex64,
  listAudit,
  type AdminAuditItem,
} from '../admin-api'
import { createAdminListResource } from '../admin-list'

const props = defineProps<{ csrfToken: string; isAdmin: boolean }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = i18n.t
const toneLabel = computed(() => (i18n.locale.value === 'en' ? 'Status' : '状态'))

const actorDraft = ref('')
const deviceDraft = ref('')
const applied = ref<{ actorId: string; deviceId: string }>({ actorId: '', deviceId: '' })
const filterInvalid = ref(false)

const list = createAdminListResource<AdminAuditItem>(
  (cursor, signal) => listAudit(
    { actorId: applied.value.actorId || null, deviceId: applied.value.deviceId || null, cursor },
    { csrfToken: props.csrfToken, signal },
  ),
)

const busy = ref(false)

function applyFilters(): void {
  // 二次门禁：撤权后筛选入口虽被隐藏，仍需在函数入口拒绝任何越权触发。
  if (!props.isAdmin) return
  const actorId = actorDraft.value.trim()
  const deviceId = deviceDraft.value.trim()
  if ((actorId.length > 0 && !isCanonicalUuidV4(actorId)) || (deviceId.length > 0 && !isDeviceIdHex64(deviceId))) {
    filterInvalid.value = true
    return
  }
  filterInvalid.value = false
  applied.value = { actorId, deviceId }
  void list.reload()
}

function reload(): void {
  // 二次门禁（P1）：头部刷新按钮渲染在 `v-if="!isAdmin"` 之外，因此未授权时既禁用按钮，
  // 也在入口直接返回，保证任何路径触发都零请求。
  if (!props.isAdmin) return
  filterInvalid.value = false
  void list.reload()
}

function loadMore(): void {
  if (!props.isAdmin) return
  void list.loadMore()
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
  <section class="admin-view" :aria-label="t('admin.audit.title')">
    <header class="admin-view__header">
      <h3>{{ t('admin.audit.title') }}</h3>
      <BaseButton variant="secondary" :disabled="!isAdmin || busy" :loading="busy && list.loading.value"
        :loading-label="t('button.loading')" data-testid="audit-reload" @click="reload">
        {{ t('admin.common.reload') }}
      </BaseButton>
    </header>

    <AppNotice v-if="!isAdmin" tone="warning" :tone-label="toneLabel" :title="t('auth.error.denied')">
      <span>{{ t('admin.denied') }}</span>
    </AppNotice>

    <template v-else>
      <form class="admin-view__toolbar" @submit.prevent="applyFilters">
        <BaseInput id="audit-filter-actor" v-model="actorDraft" :label="t('admin.audit.filterActor')" :disabled="busy" />
        <BaseInput id="audit-filter-device" v-model="deviceDraft" :label="t('admin.audit.filterDevice')" :disabled="busy" />
        <BaseButton type="submit" variant="secondary" :disabled="busy">
          {{ t('admin.audit.applyFilters') }}
        </BaseButton>
      </form>
      <p v-if="filterInvalid" data-testid="audit-filter-invalid">{{ t('admin.audit.filterInvalid') }}</p>

      <AppNotice v-if="list.errorKey.value" tone="error" :tone-label="toneLabel" :title="t('state.error')">
        <span>{{ t(list.errorKey.value) }}</span>
        <BaseButton class="admin-view__retry" :disabled="busy" @click="reload">{{ t('state.retry') }}</BaseButton>
      </AppNotice>

      <p>{{ t('admin.audit.cursorHint') }}</p>

      <p v-if="list.loading.value && list.items.value.length === 0">{{ t('state.loading') }}</p>
      <p v-else-if="!list.loading.value && !list.errorKey.value && list.items.value.length === 0">
        {{ t('admin.common.empty') }}
      </p>

      <table v-if="list.items.value.length > 0" class="admin-view__table">
        <thead>
          <tr>
            <th>{{ t('admin.audit.eventType') }}</th>
            <th>{{ t('admin.audit.actorKind') }}</th>
            <th>{{ t('admin.audit.actorUser') }}</th>
            <th>{{ t('admin.audit.targetKind') }}</th>
            <th>{{ t('admin.audit.targetId') }}</th>
            <th>{{ t('admin.audit.outcome') }}</th>
            <th>{{ t('admin.audit.processEpoch') }}</th>
            <th>{{ t('admin.audit.eventSeq') }}</th>
            <th>{{ t('admin.audit.paramsRedacted') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="event in list.items.value" :key="event.id" :data-testid="`audit-${event.id}`">
            <td>{{ event.event_type }}</td>
            <td>{{ event.actor_kind }}</td>
            <td>{{ event.actor_user_id ?? t('common.unknown') }}</td>
            <td>{{ event.target_kind ?? t('common.unknown') }}</td>
            <td><code>{{ event.target_id ?? t('common.unknown') }}</code></td>
            <td>{{ event.outcome }}</td>
            <td><code>{{ event.process_epoch }}</code></td>
            <td>{{ event.event_seq }}</td>
            <td><code>{{ event.params_redacted }}</code></td>
          </tr>
        </tbody>
      </table>

      <div v-if="list.nextCursor.value" class="admin-view__pagination">
        <BaseButton variant="secondary" :disabled="busy" :loading="busy && list.loading.value"
          :loading-label="t('button.loading')" @click="loadMore">
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
.admin-view__table { width: 100%; border-collapse: collapse; overflow-wrap: anywhere; }
.admin-view__table th, .admin-view__table td { text-align: start; padding: var(--space-2); border-bottom: 1px solid var(--color-border, #eee); vertical-align: top; }
.admin-view__pagination { display: flex; justify-content: center; }
.admin-view__retry { margin-inline-start: auto; }
</style>

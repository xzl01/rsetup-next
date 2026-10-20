<script setup lang="ts">
// 系统状态页（spec 02 §7）：就绪、版本、恢复模式探针与资源计数。
//
// 安全约束：
// - `ready` 与 `recovery_mode` 只来自运行时探针的真实读取；探针不可读时后端返回
//   503 `NOT_READY`，界面据此显示“未就绪”，绝不用默认 `normal` 兜底。
// - 五个资源计数是十进制字符串，原样显示，不转成 JS number。
import { computed, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import { createI18n } from '../i18n'
import {
  adminErrorCode,
  adminErrorKey,
  getSystemStatus,
  isAbortedError,
  type AdminSystemStatus,
} from '../admin-api'

const props = defineProps<{ csrfToken: string; isAdmin: boolean }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = i18n.t
const toneLabel = computed(() => (i18n.locale.value === 'en' ? 'Status' : '状态'))

const status = ref<AdminSystemStatus | null>(null)
const loading = ref(false)
const errorKey = ref<string | null>(null)
const notReady = ref(false)

let generation = 0
let controller: AbortController | null = null

function recoveryModeLabel(mode: string): string {
  // 只翻译已知模式；未知模式原样显示，不做猜测性映射。
  return mode === 'normal' ? t('admin.system.recoveryModeNormal') : mode
}

async function load(): Promise<void> {
  if (!props.isAdmin) return
  generation += 1
  const currentGeneration = generation
  controller?.abort()
  controller = typeof AbortController !== 'undefined' ? new AbortController() : null
  loading.value = true
  errorKey.value = null
  notReady.value = false
  try {
    const res = await getSystemStatus({ csrfToken: props.csrfToken, signal: controller?.signal })
    if (currentGeneration !== generation) return
    status.value = res.data
  } catch (error: unknown) {
    if (currentGeneration !== generation || isAbortedError(error)) return
    status.value = null
    notReady.value = adminErrorCode(error) === 'NOT_READY'
    errorKey.value = adminErrorKey(error)
  } finally {
    if (currentGeneration === generation) loading.value = false
  }
}

onMounted(() => {
  void load()
})

watch(() => props.isAdmin, (allowed) => {
  if (!allowed) {
    generation += 1
    controller?.abort()
    controller = null
    status.value = null
    errorKey.value = null
    loading.value = false
  } else {
    void load()
  }
})

onBeforeUnmount(() => {
  generation += 1
  controller?.abort()
  controller = null
})
</script>

<template>
  <section class="admin-view" :aria-label="t('admin.system.title')">
    <header class="admin-view__header">
      <h3>{{ t('admin.system.title') }}</h3>
      <BaseButton variant="secondary" :disabled="!isAdmin || loading" :loading="loading"
        :loading-label="t('button.loading')" @click="load">
        {{ t('admin.common.reload') }}
      </BaseButton>
    </header>

    <AppNotice v-if="!isAdmin" tone="warning" :tone-label="toneLabel" :title="t('auth.error.denied')">
      <span>{{ t('admin.denied') }}</span>
    </AppNotice>

    <template v-else>
      <AppNotice v-if="errorKey" tone="error" :tone-label="toneLabel" :title="t('state.error')">
        <span data-testid="system-error">{{ t(errorKey) }}</span>
        <span v-if="notReady" data-testid="system-not-ready">{{ t('admin.system.notReady') }}</span>
        <BaseButton class="admin-view__retry" :disabled="loading" @click="load">{{ t('state.retry') }}</BaseButton>
      </AppNotice>

      <p v-if="loading && status === null">{{ t('state.loading') }}</p>

      <dl v-if="status" class="admin-view__grid" data-testid="system-status">
        <div>
          <dt>{{ t('admin.system.ready') }}</dt>
          <dd :data-testid="status.ready ? 'system-ready' : 'system-not-ready-flag'">
            {{ status.ready ? t('admin.system.readyTrue') : t('admin.system.readyFalse') }}
          </dd>
        </div>
        <div>
          <dt>{{ t('admin.system.version') }}</dt>
          <dd>{{ status.version }}</dd>
        </div>
        <div>
          <dt>{{ t('admin.system.schemaVersion') }}</dt>
          <dd>{{ status.schema_version }}</dd>
        </div>
        <div>
          <dt>{{ t('admin.system.recoveryMode') }}</dt>
          <dd data-testid="system-recovery-mode">{{ recoveryModeLabel(status.recovery_mode) }}</dd>
        </div>
        <div>
          <dt>{{ t('admin.system.countUsers') }}</dt>
          <dd>{{ status.counts.users }}</dd>
        </div>
        <div>
          <dt>{{ t('admin.system.countDevices') }}</dt>
          <dd>{{ status.counts.devices }}</dd>
        </div>
        <div>
          <dt>{{ t('admin.system.countRoles') }}</dt>
          <dd>{{ status.counts.roles }}</dd>
        </div>
        <div>
          <dt>{{ t('admin.system.countGrants') }}</dt>
          <dd>{{ status.counts.grants }}</dd>
        </div>
        <div>
          <dt>{{ t('admin.system.countGroups') }}</dt>
          <dd>{{ status.counts.groups }}</dd>
        </div>
      </dl>

      <p v-if="status" class="admin-view__counts-label">{{ t('admin.system.counts') }}</p>
      <p>{{ t('admin.system.probeHint') }}</p>
    </template>
  </section>
</template>

<style scoped>
.admin-view { display: grid; gap: var(--space-3); margin-top: var(--space-4); border-top: 1px solid var(--color-border, #ccc); padding-top: var(--space-4); min-width: 0; }
.admin-view__header { display: flex; justify-content: space-between; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__header h3 { margin: 0; }
.admin-view__grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(12rem, 1fr)); gap: var(--space-2); margin: 0; }
.admin-view__grid > div { display: grid; gap: 2px; padding: var(--space-2); border: 1px solid var(--color-border, #eee); border-radius: 4px; min-width: 0; }
.admin-view__grid dt { color: var(--color-text-muted, #666); font-size: 0.875rem; }
.admin-view__grid dd { margin: 0; font-weight: 600; overflow-wrap: anywhere; }
.admin-view__counts-label { margin: 0; color: var(--color-text-muted, #666); font-size: 0.875rem; }
.admin-view__retry { margin-inline-start: auto; }
</style>

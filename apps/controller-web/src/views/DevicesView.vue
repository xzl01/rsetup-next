<script setup lang="ts">
import { computed, inject, ref, watch } from 'vue'
import { assertDeviceListPageShape, canReboot, type DeviceItem, type DeviceListPage } from '../types'
import { get } from '../api'
import type { createI18n } from '../i18n'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'
import AppNotice from '../components/AppNotice.vue'

const props = defineProps<{
  initialDevices?: DeviceItem[]
  initialNextCursor?: string | null
  permissions?: string[]
  loading?: boolean
  error?: string | null
}>()

const emit = defineEmits<{
  selectDevice: [deviceId: string]
  rebootDevice: [deviceId: string]
  retry: []
}>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const devices = ref<DeviceItem[]>(props.initialDevices ?? [])
const nextCursor = ref<string | null>(props.initialNextCursor ?? null)
const searchQuery = ref('')
const loadingMore = ref(false)
const paginationError = ref<string | null>(null)
let paginationGeneration = 0

watch(
  () => props.initialDevices,
  (newDevs) => {
    if (newDevs) {
      devices.value = newDevs
    }
  },
)

watch(
  () => props.initialNextCursor,
  (newCursor) => {
    nextCursor.value = newCursor ?? null
  },
)

const filteredDevices = computed(() => {
  const query = searchQuery.value.trim().toLowerCase()
  if (!query) return devices.value
  return devices.value.filter(
    d => d.display_name.toLowerCase().includes(query) || d.device_id.toLowerCase().includes(query)
  )
})

async function loadMore() {
  if (loadingMore.value || !nextCursor.value) return
  loadingMore.value = true
  paginationError.value = null
  const currentGen = ++paginationGeneration
  try {
    const res = await get<DeviceListPage>(
      `devices?cursor=${encodeURIComponent(nextCursor.value)}`
    )
    assertDeviceListPageShape(res.data)
    if (currentGen !== paginationGeneration) return
    devices.value = [...devices.value, ...res.data.items]
    nextCursor.value = res.data.next_cursor
  } catch (err: unknown) {
    if (currentGen !== paginationGeneration) return
    paginationError.value = i18n.t('state.error')
  } finally {
    if (currentGen === paginationGeneration) {
      loadingMore.value = false
    }
  }
}

function handleReboot(deviceId: string) {
  emit('rebootDevice', deviceId)
}

function handleSelect(deviceId: string) {
  emit('selectDevice', deviceId)
}
</script>

<template>
  <div class="devices-view">
    <header class="devices-view__header">
      <h2>{{ i18n.t('device.list.title') }}</h2>
      <div class="devices-view__search">
        <BaseInput
          id="device-search-input"
          data-testid="device-search-input"
          :model-value="searchQuery"
          :label="i18n.t('device.list.search')"
          name="deviceSearch"
          @update:model-value="searchQuery = $event"
        />
      </div>
    </header>

    <!-- 顶层错误态：绝不伪装空列表 -->
    <div v-if="error" class="devices-view__error">
      <AppNotice tone="error" :tone-label="i18n.locale.value === 'en' ? 'Error' : '错误'" :title="i18n.t('state.error')">
        <span>{{ error }}</span>
        <BaseButton
          class="devices-retry"
          :disabled="loading"
          :loading="loading"
          :loading-label="i18n.t('button.loading')"
          @click="emit('retry')"
        >
          {{ i18n.t('state.retry') }}
        </BaseButton>
      </AppNotice>
    </div>

    <div v-else-if="filteredDevices.length === 0 && !loading" class="devices-view__empty">
      <p>{{ i18n.t('device.list.empty') }}</p>
    </div>

    <!-- 最小投影列表：仅 device_id、display_name 与受该设备独立权限约束的操作 -->
    <ul v-else-if="filteredDevices.length > 0" class="devices-view__list">
      <li
        v-for="device in filteredDevices"
        :key="device.device_id"
        class="device-item"
        :data-testid="`device-item-${device.device_id}`"
      >
        <div class="device-item__identity">
          <button
            type="button"
            class="device-item__name-btn"
            @click="handleSelect(device.device_id)"
          >
            {{ device.display_name }}
          </button>
          <span class="device-item__id">{{ device.device_id }}</span>
        </div>

        <div class="device-item__actions">
          <!-- 只有该设备自身的 effective_permissions 包含 device.reboot 时才渲染重启入口 -->
          <BaseButton
            v-if="canReboot(device.effective_permissions)"
            :data-testid="`reboot-action-${device.device_id}`"
            variant="secondary"
            @click="handleReboot(device.device_id)"
          >
            {{ i18n.t('device.list.reboot') }}
          </BaseButton>
        </div>
      </li>
    </ul>

    <div v-if="paginationError" class="devices-view__pagination-error">
      <AppNotice tone="error" :tone-label="i18n.locale.value === 'en' ? 'Error' : '错误'" :title="i18n.t('state.error')">
        <span>{{ paginationError }}</span>
      </AppNotice>
    </div>

    <div v-if="nextCursor" class="devices-view__pagination">
      <BaseButton
        data-testid="load-more-btn"
        :loading="loadingMore"
        :loading-label="i18n.t('button.loading')"
        @click="loadMore"
      >
        {{ i18n.t('device.list.loadMore') }}
      </BaseButton>
    </div>
  </div>
</template>

<style scoped>
.devices-view {
  display: flex;
  flex-direction: column;
  gap: var(--space-4, 1rem);
}
.devices-view__header {
  display: flex;
  flex-wrap: wrap;
  justify-content: space-between;
  align-items: center;
  gap: var(--space-2, 0.5rem);
}
.devices-view__list {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-2, 0.5rem);
}
.device-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: var(--space-3, 0.75rem);
  border: 1px solid var(--color-border, #ccc);
  border-radius: 4px;
}
.device-item__identity {
  display: flex;
  flex-direction: column;
  gap: 0.25rem;
}
.device-item__name-btn {
  background: none;
  border: none;
  padding: 0;
  font: inherit;
  font-weight: bold;
  text-align: left;
  cursor: pointer;
  color: var(--color-primary, #0056b3);
  text-decoration: underline;
}
.device-item__name-btn:hover, .device-item__name-btn:focus {
  text-decoration: none;
}
.device-item__id {
  font-family: monospace;
  font-size: 0.85rem;
  color: var(--color-text-muted, #666);
}
</style>

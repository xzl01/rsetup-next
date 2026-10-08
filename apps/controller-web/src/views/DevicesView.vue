<script setup lang="ts">
import { computed, inject, ref, watch } from 'vue'
import { canReboot, type DeviceItem } from '../types'
import { get } from '../api'
import type { createI18n } from '../i18n'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'

const props = defineProps<{
  initialDevices?: DeviceItem[]
  initialNextCursor?: string | null
  permissions?: string[]
}>()

const emit = defineEmits<{
  selectDevice: [deviceId: string]
  rebootDevice: [deviceId: string]
}>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const devices = ref<DeviceItem[]>(props.initialDevices ?? [])
const nextCursor = ref<string | null>(props.initialNextCursor ?? null)
const searchQuery = ref('')
const loadingMore = ref(false)

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

const userPermissions = computed(() => props.permissions ?? [])
const canTriggerReboot = computed(() => canReboot(userPermissions.value))

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
  try {
    const res = await get<{ items: DeviceItem[]; next_cursor: string | null }>(
      `devices?cursor=${encodeURIComponent(nextCursor.value)}`
    )
    devices.value = [...devices.value, ...res.data.items]
    nextCursor.value = res.data.next_cursor
  } finally {
    loadingMore.value = false
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

    <div v-if="filteredDevices.length === 0" class="devices-view__empty">
      <p>{{ i18n.t('device.list.empty') }}</p>
    </div>

    <!-- 最小投影列表：仅 device_id、display_name 与受权限约束的操作 -->
    <ul v-else class="devices-view__list">
      <li
        v-for="device in filteredDevices"
        :key="device.device_id"
        class="device-item"
        :data-testid="`device-item-${device.device_id}`"
      >
        <div class="device-item__identity">
          <strong class="device-item__name" @click="handleSelect(device.device_id)">
            {{ device.display_name }}
          </strong>
          <span class="device-item__id">{{ device.device_id }}</span>
        </div>

        <div class="device-item__actions">
          <!-- 仅具 device.reboot 权限时渲染重启入口 -->
          <BaseButton
            v-if="canTriggerReboot || device.effective_permissions.includes('device.reboot')"
            :data-testid="`reboot-action-${device.device_id}`"
            variant="secondary"
            @click="handleReboot(device.device_id)"
          >
            {{ i18n.t('device.list.reboot') }}
          </BaseButton>
        </div>
      </li>
    </ul>

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
.device-item__name {
  cursor: pointer;
  color: var(--color-primary, #0056b3);
}
.device-item__id {
  font-family: monospace;
  font-size: 0.85rem;
  color: var(--color-text-muted, #666);
}
</style>

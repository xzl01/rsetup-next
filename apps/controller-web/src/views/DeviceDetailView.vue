<script setup lang="ts">
import { computed, inject, onMounted, ref, watch } from 'vue'
import { canReadDevice, canReadStatus, type DeviceDetail, type DeviceStatusReport } from '../types'
import { get } from '../api'
import type { createI18n } from '../i18n'
import BaseButton from '../components/BaseButton.vue'

const props = defineProps<{
  deviceId: string
  initialDetail?: DeviceDetail | null
  initialStatus?: DeviceStatusReport | null
  permissions?: string[]
}>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const detailData = ref<DeviceDetail | null>(props.initialDetail ?? null)
const statusData = ref<DeviceStatusReport | null>(props.initialStatus ?? null)
const loading = ref(false)
const error = ref<string | null>(null)

// 优先采用显式传入的 permissions，若未指定则退回到 initialDetail 中的 effective_permissions
const perms = computed(() => {
  if (props.permissions !== undefined) return props.permissions
  return detailData.value?.effective_permissions || []
})

const hasDetailPermission = computed(() => canReadDevice(perms.value))
const hasStatusPermission = computed(() => canReadStatus(perms.value))

// 请求代际与取消控制：换设备或撤权时废弃 stale 响应
let fetchGeneration = 0
let abortController: AbortController | null = null

async function loadData() {
  fetchGeneration += 1
  const currentGen = fetchGeneration

  if (abortController) {
    abortController.abort()
  }
  abortController = new AbortController()
  const signal = abortController.signal

  // 若无权限，必须确保清除旧投影，绝不保留已撤权数据
  if (props.permissions !== undefined && !hasDetailPermission.value) {
    detailData.value = null
  }
  if (props.permissions !== undefined && !hasStatusPermission.value) {
    statusData.value = null
  }

  // 当没有指定 permissions 且 detailData 尚为空时，初次尝试获取详情
  const needFetchDetail = props.permissions === undefined
    ? !detailData.value
    : hasDetailPermission.value && !props.initialDetail

  const needFetchStatus = hasStatusPermission.value && !props.initialStatus

  if (!needFetchDetail && !needFetchStatus) {
    return
  }

  loading.value = true
  error.value = null

  try {
    const promises: [Promise<unknown>?, Promise<unknown>?] = []
    if (needFetchDetail) {
      promises[0] = get<DeviceDetail>(`devices/${props.deviceId}`, { signal })
    }
    if (needFetchStatus) {
      promises[1] = get<DeviceStatusReport>(`devices/${props.deviceId}/status`, { signal })
    }

    const [detailRes, statusRes] = await Promise.all(promises)

    if (currentGen !== fetchGeneration) {
      return
    }

    if (detailRes && typeof detailRes === 'object' && 'data' in detailRes) {
      detailData.value = (detailRes as { data: DeviceDetail }).data
    }
    if (statusRes && typeof statusRes === 'object' && 'data' in statusRes) {
      statusData.value = (statusRes as { data: DeviceStatusReport }).data
    }
  } catch (err: unknown) {
    if (currentGen !== fetchGeneration) return
    if (err && typeof err === 'object' && 'name' in err && err.name === 'AbortError') return
    error.value = i18n.t('state.error')
  } finally {
    if (currentGen === fetchGeneration) {
      loading.value = false
    }
  }
}

watch(
  [() => props.deviceId, () => props.permissions],
  () => {
    // 换设备时必须立即清空旧数据，防止陈旧投影残留
    detailData.value = props.initialDetail ?? null
    statusData.value = props.initialStatus ?? null
    void loadData()
  },
)

onMounted(() => {
  void loadData()
})
</script>

<template>
  <div class="device-detail">
    <div v-if="error" class="device-detail__error" role="alert">
      <span>{{ error }}</span>
      <BaseButton @click="loadData">{{ i18n.t('state.retry') }}</BaseButton>
    </div>

    <!-- 档案与前三维 (需 device.read 权限) -->
    <div v-if="hasDetailPermission && detailData" data-testid="detail-profile-card" class="profile-card">
      <h2>{{ detailData.display_name }}</h2>
      <div class="dimension-grid">
        <!-- 1. 准入状态 -->
        <section data-testid="dim-admission">
          <h3>{{ i18n.t('device.dim.admission') }}</h3>
          <p>{{ detailData.admission_state }} ({{ detailData.review_decision }})</p>
        </section>

        <!-- 2. 连接状态 -->
        <section data-testid="dim-connection">
          <h3>{{ i18n.t('device.dim.connection') }}</h3>
          <p>{{ detailData.connection_state }}</p>
        </section>

        <!-- 3. 双业务流健康 (严禁合并全绿) -->
        <section data-testid="dim-stream-health">
          <h3>{{ i18n.t('device.dim.streamHealth') }}</h3>
          <p data-testid="dim-control-health">{{ i18n.t('device.health.control') }}: {{ detailData.control_health }}</p>
          <p data-testid="dim-data-health">{{ i18n.t('device.health.data') }}: {{ detailData.data_health }}</p>
        </section>
      </div>
    </div>

    <!-- 4 & 5. 遥测新鲜度、中控接收与板端时钟质量 (需 device.status.read 权限) -->
    <div v-if="hasStatusPermission && statusData" data-testid="status-card" class="status-card">
      <section data-testid="dim-freshness">
        <h3>{{ i18n.t('device.dim.freshness') }}</h3>
        <p>{{ statusData.freshness }} ({{ i18n.t('device.status.age') }}: {{ statusData.age_ms !== null && statusData.age_ms !== undefined ? `${statusData.age_ms}ms` : i18n.t('common.unknown') }})</p>
        <p v-if="statusData.last_error" data-testid="dim-last-error">{{ i18n.t('common.error') }}: {{ statusData.last_error }}</p>
      </section>

      <!-- 中控接收端时钟质量 -->
      <section data-testid="dim-controller-clock">
        <h3>{{ i18n.t('device.dim.controllerClock') }}</h3>
        <p>{{ statusData.received_time?.quality || 'null' }}</p>
      </section>

      <!-- 板端时钟测量：独立呈现，未知显示 null，严禁填 0 -->
      <section data-testid="dim-board-clock">
        <h3>{{ i18n.t('device.dim.boardClock') }}</h3>
        <p>{{ statusData.snapshot?.clock_quality !== undefined ? (statusData.snapshot.clock_quality ?? 'null') : 'null' }}</p>
      </section>
    </div>
  </div>
</template>

<style scoped>
.device-detail {
  display: flex;
  flex-direction: column;
  gap: var(--space-4, 1rem);
}
.profile-card, .status-card {
  border: 1px solid var(--color-border, #ccc);
  border-radius: 4px;
  padding: var(--space-4, 1rem);
}
.dimension-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  gap: var(--space-3, 0.75rem);
}
</style>

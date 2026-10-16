<script setup lang="ts">
import { computed, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { DeviceItem } from '../types'
import {
  assertTaskPageShape,
  assertTaskPreviewShape,
  describeAuthorizedSubset,
  eligibleDeviceIds,
  type TaskItem,
  type TaskPreview,
} from '../task-ops'
import {
  mainTaskStateLabel,
  previewFingerprint,
  resetTaskOperation,
  startTaskOperation,
} from '../task-presentation'
import type { createI18n } from '../i18n'
import BaseButton from '../components/BaseButton.vue'
import AppNotice from '../components/AppNotice.vue'
import TaskModals from '../components/TaskModals.vue'

const props = withDefaults(defineProps<{
  devices?: DeviceItem[]
  initialTasks?: TaskItem[]
  initialNextCursor?: string | null
  pendingDeviceId?: string | null
  csrfToken?: string
  onPreview?: (operation: 'reboot', deviceIds: string[], signal?: AbortSignal) => Promise<TaskPreview>
  onSubmit?: (payload: { preview_token: string; confirm: true }, options: { idempotencyKey: string; signal?: AbortSignal }) => Promise<{ task_id: string; accepted: boolean }>
  onCancel?: (payload: { taskId: string; expectedRevision: string; confirm: true }) => Promise<void>
  onLoadTasks?: (cursor: string | null) => Promise<{ items: TaskItem[]; next_cursor: string | null }>
}>(), {
  devices: () => [], initialNextCursor: null, pendingDeviceId: null,
  csrfToken: '',
})

const emit = defineEmits<{ openTask: [taskId: string]; rebootDevice: [deviceId: string] }>()
const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = (key: string) => i18n.t(key)

// 任务目标集合 = 持有 device.reboot 或 device.task.read 的设备（01 §4:42–45：reboot 不隐含 read，
// read 不隐含 reboot；两者皆无的设备不渲染、不进入 device_ids）。reboot-only 设备必须可预览与提交，
// 服务端逐端点重验为最终准绳；task.read-only 设备保留只读预览语义。
const taskDevices = computed(() => props.devices.filter(
  (device) => device.effective_permissions.includes('device.reboot') ||
    device.effective_permissions.includes('device.task.read'),
))
// pendingDeviceId 预选必须在 setup 期完成（而非 onMounted）：否则首帧渲染时勾选态尚未生效，
// 生成预览按钮在挂载后的第一次点击仍是 disabled，来自 #/tasks?device=<hex64> 的入口会静默失效
const pendingDevice = props.pendingDeviceId
const selected = ref<string[]>(
  pendingDevice && taskDevices.value.some((device) => device.device_id === pendingDevice)
    ? [pendingDevice]
    : [],
)
const preview = ref<TaskPreview | null>(null)
const previewError = ref<string | null>(null)
const previewBusy = ref(false)
const batchModalOpen = ref(false)
const operation = ref(resetTaskOperation())
const tasks = ref<TaskItem[]>(props.initialTasks ?? [])
const nextCursor = ref<string | null>(props.initialNextCursor ?? null)
const listError = ref<string | null>(null)
const loadMoreBusy = ref(false) // WEB-R6-1：loadMore 防重入门控
let abortController: AbortController | null = null
let fingerprint = ''
// 统一操作代际（参照 Task 9 isStale 风格）：preview 与 submit 共用同一代际计数，任一操作开始即递增；
// 旧操作的 await 返回、catch 分支与 finally 回写一律被 isStaleOp 拦截，丢弃结果且不触碰任何状态
const operationGeneration = ref(0)
const operationKind = ref<'preview' | 'submit' | null>(null)
const isStaleOp = (gen: number, kind: 'preview' | 'submit') =>
  gen !== operationGeneration.value || kind !== operationKind.value

function selectDevice(deviceId: string, checked: boolean) {
  selected.value = checked
    ? Array.from(new Set([...selected.value, deviceId]))
    : selected.value.filter((id) => id !== deviceId)
}

// CSPRNG 能力门禁：crypto.getRandomValues 不可用即 fail-closed，绝不 fallback 到 randomUUID/Math.random
const csprngAvailable = typeof globalThis.crypto?.getRandomValues === 'function'

// WEB-N3：props.devices 更新（权限撤销或移出列表）后 taskDevices 不再列出已失格设备，
// 但 selected 仍会残留其 ID；立即取交集清理，防止残留 ID 被后续操作提交
watch(() => props.devices, () => {
  selected.value = selected.value.filter((id) => taskDevices.value.some((device) => device.device_id === id))
})

async function loadPreview() {
  if (!props.onPreview) return
  // WEB-N3 双重纵深防御：发起前再与当前 taskDevices 取交集，已失格设备绝不进入 device_ids
  const targetIds = selected.value.filter((id) => taskDevices.value.some((device) => device.device_id === id))
  if (targetIds.length === 0) return
  abortController?.abort()
  const controller = new AbortController()
  abortController = controller
  const gen = ++operationGeneration.value
  operationKind.value = 'preview'
  // 新 preview 代际显式结束被替代 submit 的 inFlight（保留原 key）：同 fingerprint 时
  // resetTaskOperation() 不会执行，旧 submit 迟到的 resolve/reject/finally 被 isStaleOp
  // 拦截、绝不写新代状态；fingerprint 变化时才由下方 resetTaskOperation() 清除 key
  if (operation.value.inFlight) operation.value = { key: operation.value.key, inFlight: false }
  previewBusy.value = true
  previewError.value = null
  try {
    const result = await props.onPreview('reboot', targetIds, controller.signal)
    if (isStaleOp(gen, 'preview')) return // 迟到响应：丢弃，不覆盖新状态
    assertTaskPreviewShape(result)
    preview.value = result
    const nextFingerprint = previewFingerprint(result)
    if (nextFingerprint !== fingerprint) {
      // 固定清单或 token 变化：旧幂等键与新内容不再绑定，必须作废并清除确认
      fingerprint = nextFingerprint
      operation.value = resetTaskOperation()
      batchModalOpen.value = false
    }
  } catch {
    if (isStaleOp(gen, 'preview')) return
    preview.value = null
    previewError.value = t('state.error')
  } finally {
    if (!isStaleOp(gen, 'preview')) previewBusy.value = false // 旧代 finally 不得清除新代 busy
  }
}

async function submitBatch() {
  if (!preview.value || !props.onSubmit || !csprngAvailable) return
  let next: { key: string; inFlight: boolean }
  try {
    // 首次确认生成 UUIDv4 key；失败重试（inFlight 已落下）复用同 key——同 actor + 同 key + 同内容
    // 命中幂等重放返回原任务，不同内容才 409 IDEMPOTENCY_CONFLICT（02-data-api 幂等段约 L153–159）
    next = startTaskOperation(operation.value)
  } catch {
    return // TASK_OPERATION_BUSY
  }
  operation.value = next
  abortController?.abort()
  const controller = new AbortController()
  abortController = controller
  const gen = ++operationGeneration.value
  operationKind.value = 'submit'
  try {
    await props.onSubmit(
      { preview_token: preview.value.preview_token, confirm: true },
      { idempotencyKey: next.key, signal: controller.signal },
    )
    if (isStaleOp(gen, 'submit')) return // 迟到成功：不关闭新代模态、不清新代错误
    batchModalOpen.value = false
    previewError.value = null
  } catch {
    // 提交失败：置可见错误态（复用预览区 AppNotice alert，非静默 return）并保持同 key 可重试
    if (!isStaleOp(gen, 'submit')) previewError.value = t('state.error')
  } finally {
    // 旧代 finally 不得复活/清除新代 key 与 inFlight
    if (!isStaleOp(gen, 'submit')) operation.value = { key: next.key, inFlight: false }
  }
}

async function loadMore() {
  if (!props.onLoadTasks || !nextCursor.value || loadMoreBusy.value) return
  loadMoreBusy.value = true // WEB-R6-1：同一 nextCursor 连击不得并发发起两次请求
  try {
    const page = await props.onLoadTasks(nextCursor.value)
    assertTaskPageShape(page)
    const seenIds = new Set(tasks.value.map((item) => item.id)) // WEB-R7-1：兼防同一页内重复的新 ID
    const newItems = page.items.filter((item) => {
      if (seenIds.has(item.id)) return false
      seenIds.add(item.id)
      return true
    })
    tasks.value = [...tasks.value, ...newItems]
    nextCursor.value = page.next_cursor
    listError.value = null // WEB-R5-2：分页成功必须清除列表错误，否则表格永久隐藏
  } catch {
    listError.value = t('state.error')
  } finally {
    loadMoreBusy.value = false
  }
}

async function initialLoad() {
  if (props.initialTasks !== undefined || !props.onLoadTasks) return
  try {
    listError.value = null
    const page = await props.onLoadTasks(null)
    assertTaskPageShape(page)
    tasks.value = page.items
    nextCursor.value = page.next_cursor
  } catch {
    // 首屏加载失败呈现错误态，绝不伪装为空列表
    listError.value = t('state.error')
  }
}

onMounted(() => {
  initialLoad()
})

watch(() => props.pendingDeviceId, (id) => {
  if (id && taskDevices.value.some((device) => device.device_id === id)) {
    selected.value = Array.from(new Set([...selected.value, id]))
  }
})

watch(() => props.initialTasks, (val) => {
  if (val !== undefined) {
    tasks.value = val
    nextCursor.value = props.initialNextCursor ?? null
  }
})

watch(() => props.initialNextCursor, (val) => {
  nextCursor.value = val ?? null
})

onBeforeUnmount(() => {
  abortController?.abort()
})
</script>

<template>
  <div class="tasks-view">
    <section class="tasks-view__preview">
      <h2>{{ t('task.preview.title') }}</h2>
      <fieldset>
        <legend>{{ t('task.preview.selectDevices') }}</legend>
        <p v-if="taskDevices.length === 0">{{ t('task.preview.empty') }}</p>
        <label v-for="device in taskDevices" :key="device.device_id" class="tasks-view__device">
          <input
            type="checkbox"
            :data-testid="`task-device-${device.device_id}`"
            :checked="selected.includes(device.device_id)"
            :aria-label="device.display_name"
            @change="selectDevice(device.device_id, ($event.target as HTMLInputElement).checked)"
          />
          {{ device.display_name }}
        </label>
      </fieldset>
      <BaseButton
        :disabled="selected.length === 0 || previewBusy"
        :loading="previewBusy"
        :loading-label="t('button.loading')"
        @click="loadPreview"
      >
        {{ t('task.preview.submit') }}
      </BaseButton>
      <AppNotice
        v-if="previewError"
        tone="error"
        :tone-label="t('common.error')"
        :title="t('state.error')"
      >
        <span>{{ previewError }}</span>
      </AppNotice>
      <!-- 固定清单：逐目标 device_id + 可提交性 + 原因，无隐藏数量 -->
      <ul v-if="preview" class="tasks-view__targets">
        <li v-for="target in preview.targets" :key="target.device_id">
          <span class="tasks-view__target-id">{{ target.device_id }}</span>
          <span>{{ target.eligible ? t('task.preview.eligible') : t('task.preview.ineligible') }}</span>
          <span v-if="!target.eligible && target.reason">{{ t(`task.reason.${target.reason}`) }}</span>
        </li>
      </ul>
      <AppNotice
        v-if="!csprngAvailable"
        tone="error"
        :tone-label="t('common.error')"
        :title="t('state.error')"
      >
        <span>{{ t('task.security.csprngUnavailable') }}</span>
      </AppNotice>
      <BaseButton
        v-if="preview && eligibleDeviceIds(preview).length > 0"
        variant="danger"
        :disabled="!csprngAvailable"
        @click="batchModalOpen = true"
      >
        {{ t('task.submitReboot') }}
      </BaseButton>
    </section>

    <section class="tasks-view__list">
      <h2>{{ t('task.list.title') }}</h2>
      <AppNotice
        v-if="listError"
        tone="error"
        :tone-label="t('common.error')"
        :title="t('state.error')"
      >
        <span>{{ listError }}</span>
      </AppNotice>
      <p v-else-if="tasks.length === 0">{{ t('task.list.empty') }}</p>
      <table v-else>
        <thead>
          <tr>
            <th>{{ t('task.list.columns.device') }}</th>
            <th>{{ t('task.list.columns.state') }}</th>
            <th>{{ t('task.list.columns.revision') }}</th>
            <th>{{ t('common.actions') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="item in tasks" :key="item.id">
            <td>{{ item.id }}</td>
            <td>
              {{ mainTaskStateLabel(item.state, t) }}
              <!-- 时间证据只消费原始值，绝不把 created_time 对象整体渲染进模板 -->
              <span class="tasks-view__time">{{ item.created_time.reference_utc }} · {{ item.created_time.quality }}</span>
            </td>
            <td>{{ item.revision }}</td>
            <td>
              <span>{{ t(describeAuthorizedSubset(item.view_scope).key) }}</span>
              <BaseButton variant="secondary" @click="emit('openTask', item.id)">
                {{ t('task.detail.title') }}
              </BaseButton>
            </td>
          </tr>
        </tbody>
      </table>
      <BaseButton
        v-if="nextCursor"
        variant="secondary"
        :disabled="loadMoreBusy"
        @click="loadMore"
      >
        {{ t('task.list.loadMore') }}
      </BaseButton>
    </section>

    <TaskModals
      mode="batch-risk"
      :open="batchModalOpen"
      :confirm-disabled="!preview || previewBusy || !csprngAvailable"
      :busy="operation.inFlight"
      @close="batchModalOpen = false"
      @confirm-batch-risk="submitBatch"
    />
  </div>
</template>

<style scoped>
.tasks-view {
  display: flex;
  flex-direction: column;
  gap: var(--space-4, 1rem);
}
.tasks-view__device {
  display: flex;
  align-items: center;
  gap: var(--space-2, 0.5rem);
}
.tasks-view__targets {
  list-style: none;
  padding: 0;
  margin: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-1, 0.25rem);
}
.tasks-view__target-id {
  font-family: monospace;
}
.tasks-view__time {
  display: block;
  font-family: monospace;
  font-size: 0.85rem;
  color: var(--color-text-muted, #666);
}
</style>

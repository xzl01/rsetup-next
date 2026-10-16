<script setup lang="ts">
import { computed, inject } from 'vue'
import type { SubTaskItem } from '../task-ops'
import { reasonCodeLabel, taskStateLabel } from '../task-presentation'
import type { createI18n } from '../i18n'
import BaseButton from './BaseButton.vue'

const props = withDefaults(defineProps<{
  subtasks: SubTaskItem[]
  showReleaseAction?: boolean
  emptyKey?: string
}>(), {
  showReleaseAction: false,
  emptyKey: 'task.list.empty',
})

const emit = defineEmits<{ releaseLock: [subtask: SubTaskItem] }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = (key: string) => i18n.t(key)

const rows = computed(() => props.subtasks.map((subtask) => ({
  subtask,
  stateText: taskStateLabel(subtask.state, t),
  reasonText: subtask.reason_code ? reasonCodeLabel(subtask.reason_code, t) : null,
  // 释放判定以 state === 'unknown' 为准（04 §8）；lock_released 是可选兼容字段，
  // 仅在其显式为 true 时隐藏入口，绝不据其缺失推断危险授权
  canRelease: props.showReleaseAction && subtask.state === 'unknown' && subtask.lock_released !== true,
})))
</script>

<template>
  <div class="task-state-list">
    <p v-if="rows.length === 0" class="task-state-list__empty">{{ t(emptyKey) }}</p>
    <ul v-else class="task-state-list__items">
      <li
        v-for="row in rows"
        :key="row.subtask.id"
        class="task-state-row"
        :data-testid="`subtask-row-${row.subtask.id}`"
      >
        <span class="task-state-row__device">{{ row.subtask.device_id }}</span>
        <span class="task-state-row__state">{{ row.stateText }}</span>
        <span v-if="row.reasonText" class="task-state-row__reason">{{ row.reasonText }}</span>
        <span class="task-state-row__revision">{{ row.subtask.revision }}</span>
        <!-- 只展示当前授权的结果证据种类（snake_case 字面值），不冒称完整 wire 证据对象或成功证明（04 §6）；
             result_evidence 为 null（证据未产生）不渲染证据行；execution_token 及其摘要永不消费（02 §1） -->
        <span v-if="row.subtask.result_evidence" class="task-state-row__evidence">
          {{ t('task.detail.resultEvidence') }}: {{ row.subtask.result_evidence }}
        </span>
        <BaseButton
          v-if="row.canRelease"
          variant="danger"
          :data-testid="`release-lock-${row.subtask.id}`"
          @click="emit('releaseLock', row.subtask)"
        >
          {{ t('task.releaseLock') }}
        </BaseButton>
      </li>
    </ul>
  </div>
</template>

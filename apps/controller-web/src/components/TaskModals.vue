<script setup lang="ts">
import { computed, inject, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import type { SubTaskItem } from '../task-ops'
import type { createI18n } from '../i18n'
import BaseButton from './BaseButton.vue'

const props = withDefaults(defineProps<{
  mode: 'batch-risk' | 'release-lock'
  open: boolean
  subtask?: SubTaskItem | null
  busy?: boolean
  confirmDisabled?: boolean
}>(), {
  subtask: null,
  busy: false,
  confirmDisabled: false,
})

const emit = defineEmits<{
  close: []
  confirmBatchRisk: []
  confirmReleaseLock: [payload: {
    subtaskId: string
    expected_revision: string
    acknowledge_unknown: true
    reason: string
  }]
}>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = (key: string) => i18n.t(key)

const titleId = computed(() => (props.mode === 'release-lock' ? 'task-modal-release-title' : 'task-modal-batch-title'))
const dialogRef = ref<HTMLElement | null>(null)
const reason = ref('')
const acknowledged = ref(false)
const batchConfirmed = ref(false)
let restoreTarget: HTMLElement | null = null

const releaseConfirmed = computed(() => reason.value.trim().length > 0 && acknowledged.value && !props.busy)
const releaseButtonDisabled = computed(() => !releaseConfirmed.value)
const batchDisabled = computed(() => !batchConfirmed.value || props.confirmDisabled === true || props.busy)

function focusableElements(): HTMLElement[] {
  if (!dialogRef.value) return []
  // 严格过滤：排除 tabindex="-1"（仅可程序化聚焦）与 type="hidden"（状态存储输入），防止二者被当作首/末可聚焦元素纳入 Focus Trap 首末边界
  return Array.from(dialogRef.value.querySelectorAll<HTMLElement>(
    'input:not([disabled]):not([type="hidden"]):not([tabindex="-1"]), button:not([disabled]):not([tabindex="-1"]), select:not([disabled]):not([tabindex="-1"]), textarea:not([disabled]):not([tabindex="-1"]), [tabindex]:not([tabindex="-1"]):not([type="hidden"])',
  )).filter((el) => !el.hasAttribute('disabled') && el.getAttribute('aria-hidden') !== 'true')
}

let focusGeneration = 0

// 焦点恢复的唯一出口：先清空记录，仅当记录的目标仍在文档中且可聚焦时才聚焦；
// 记录为空（无触发元素或触发元素已断开）时静默跳过，不抛错。
function restoreFocus() {
  const target = restoreTarget
  restoreTarget = null
  if (target && typeof target.focus === 'function' && document.contains(target)) target.focus()
}

function close() {
  // 关闭即作废尚未落地的 openDialog 迟到聚焦
  focusGeneration += 1
  emit('close')
  restoreFocus()
}

function onKeydown(event: KeyboardEvent) {
  if (!props.open) return
  if (event.key === 'Escape') {
    event.preventDefault()
    close()
    return
  }
  if (event.key !== 'Tab') return
  const focusables = focusableElements()
  if (focusables.length === 0) return
  const first = focusables[0]!
  const last = focusables[focusables.length - 1]!
  const active = document.activeElement as HTMLElement | null
  if (event.shiftKey) {
    if (active === first || !active || !dialogRef.value?.contains(active)) {
      event.preventDefault()
      last.focus()
    }
    return
  }
  if (active === last || !active || !dialogRef.value?.contains(active)) {
    event.preventDefault()
    first.focus()
  }
}

async function openDialog() {
  const generation = ++focusGeneration
  restoreTarget = (document.activeElement as HTMLElement | null) ?? null
  reason.value = ''
  acknowledged.value = false
  batchConfirmed.value = false
  await nextTick()
  // 迟到守卫：await 期间父组件可能已把 open 置为 false，或组件已进入卸载流程；
  // 关闭与卸载都会推进代际，旧一代 openDialog 在此必须放弃聚焦，不得抢焦点
  if (!props.open || generation !== focusGeneration) return
  const focusables = focusableElements()
  if (focusables.length > 0) focusables[0]!.focus()
}

// 初次以 open=true 挂载时也须移入焦点（watch 不会触发），因此同时使用 onMounted 与 watch
onMounted(() => {
  if (props.open) openDialog()
})

// 父组件成功提交后直接把 open 置为 false 是合法关闭路径：组件常驻，close() 与
// onBeforeUnmount 都不会执行，必须由 watch 兜底恢复焦点；该路径只恢复焦点、不
// emit('close')，否则父组件的 open=false 会回授成第二次 close，形成回授循环
watch(() => props.open, (open) => {
  if (open) {
    openDialog()
    return
  }
  focusGeneration += 1
  restoreFocus()
})

function confirmRelease() {
  if (!releaseConfirmed.value || !props.subtask) return
  emit('confirmReleaseLock', {
    subtaskId: props.subtask.id,
    expected_revision: props.subtask.revision,
    acknowledge_unknown: true,
    reason: reason.value.trim(),
  })
}

function confirmBatch() {
  if (batchDisabled.value) return
  emit('confirmBatchRisk')
}

// 卸载与父驱动关闭共用同一 restoreFocus helper；推进代际让任何迟到的 openDialog 失效
onBeforeUnmount(() => {
  focusGeneration += 1
  restoreFocus()
})
</script>

<template>
  <div
    v-if="open"
    ref="dialogRef"
    class="task-modal-overlay"
    role="dialog"
    aria-modal="true"
    :aria-labelledby="titleId"
    @keydown="onKeydown"
  >
    <div class="task-modal">
      <template v-if="mode === 'release-lock'">
        <h3 :id="titleId">{{ t('task.risk.releaseLockTitle') }}</h3>
        <p class="task-modal__warning">{{ t('task.risk.releaseWarningDetail') }}</p>
        <label class="task-modal__field">
          {{ t('task.field.releaseReason') }}
          <input
            v-model="reason"
            type="text"
            :aria-label="t('task.field.releaseReason')"
            data-testid="release-reason"
          />
        </label>
        <label class="task-modal__ack">
          <input
            v-model="acknowledged"
            type="checkbox"
            :aria-label="t('task.risk.acknowledgeUnknown')"
          />
          {{ t('task.risk.acknowledgeUnknown') }}
        </label>
        <div class="task-modal__actions">
          <BaseButton
            variant="danger"
            :disabled="releaseButtonDisabled"
            :loading="busy"
            :loading-label="t('button.loading')"
            data-testid="confirm-release"
            @click="confirmRelease"
          >
            {{ t('task.confirmRelease') }}
          </BaseButton>
          <BaseButton variant="secondary" @click="close">{{ t('common.cancel') }}</BaseButton>
        </div>
      </template>
      <template v-else>
        <h3 :id="titleId">{{ t('task.preview.title') }}</h3>
        <label class="task-modal__ack">
          <input
            v-model="batchConfirmed"
            type="checkbox"
            :aria-label="t('task.risk.confirmBatchReboot')"
          />
          {{ t('task.risk.confirmBatchReboot') }}
        </label>
        <div class="task-modal__actions">
          <BaseButton
            :disabled="batchDisabled"
            :loading="busy"
            :loading-label="t('button.loading')"
            data-testid="confirm-batch-risk"
            @click="confirmBatch"
          >
            {{ t('task.submitReboot') }}
          </BaseButton>
          <BaseButton variant="secondary" @click="close">{{ t('common.cancel') }}</BaseButton>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
/* 模态浮层：全屏半透明遮罩 + 居中浮层。此前模板已有 task-modal-overlay/task-modal
   两个类名但无任何样式，对话框以普通文档流混排在页面中，既无遮罩隔离也无焦点聚焦 */
.task-modal-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
  padding: var(--space-4, 1rem);
}
.task-modal {
  display: grid;
  gap: var(--space-4, 1rem);
  background: var(--color-surface, #fff);
  color: var(--color-text, #182235);
  border-radius: var(--radius, 0.375rem);
  padding: var(--space-6, 1.5rem);
  max-width: 36rem;
  width: 100%;
  min-width: 0;
  box-shadow: 0 10px 25px rgba(0, 0, 0, 0.2);
  max-height: 90vh;
  overflow-y: auto;
  text-align: start;
}
.task-modal h3 {
  margin: 0;
  font-size: 1.25rem;
  line-height: 1.3;
}
.task-modal p {
  margin: 0;
}
.task-modal__warning {
  color: var(--color-danger, #a12333);
  font-weight: 600;
}
.task-modal__field {
  display: grid;
  gap: var(--space-1, 0.25rem);
  min-width: 0;
  font-weight: 600;
}
.task-modal__field input {
  width: 100%;
  min-width: 0;
  min-height: 2.75rem;
  padding: var(--space-2, 0.5rem) var(--space-3, 0.75rem);
  border: 2px solid var(--color-border, #69788b);
  border-radius: var(--radius, 0.375rem);
  background: var(--color-surface, #fff);
  color: var(--color-text, #182235);
  font: inherit;
  font-weight: 400;
}
.task-modal__ack {
  display: flex;
  align-items: flex-start;
  gap: var(--space-2, 0.5rem);
  min-width: 0;
  overflow-wrap: anywhere;
}
.task-modal__ack input {
  flex: none;
  width: 1.125rem;
  height: 1.125rem;
  margin-block-start: 0.2rem;
}
.task-modal__actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-2, 0.5rem);
}
@media (max-width: 320px) {
  .task-modal { padding: var(--space-4, 1rem); }
  .task-modal__actions > * { flex: 1 1 100%; }
}
</style>

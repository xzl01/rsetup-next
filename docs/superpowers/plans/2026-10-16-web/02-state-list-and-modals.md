### Task 6: `TaskStateList.vue` —— 10 状态/原因码/结果证据种类与 admin 释放入口

**Files:**
- Create: `apps/controller-web/src/components/TaskStateList.vue`
- Create: `apps/controller-web/src/components/TaskStateList.test.ts`

**Interfaces:**
- Consumes: `task-presentation.ts` 的 `taskStateLabel`、`reasonCodeLabel`；`task-ops.ts` 的 `SubTaskItem`；`i18n`（`inject('i18n')`）；既有 `BaseButton.vue`、`AppNotice.vue`。
- Produces:
  ```ts
  // props
  defineProps<{
    subtasks: SubTaskItem[]
    showReleaseAction?: boolean          // 仅管理员且视图允许时为 true
    emptyKey?: string                    // 默认 'task.list.empty'
  }>()
  // emits
  defineEmits<{ releaseLock: [subtask: SubTaskItem] }>()
  ```
  渲染契约：
  - 每个子任务渲染 `data-testid="subtask-row-<id>"`，其中包含设备 id、`taskStateLabel(state, t)` 文本、`task.revision` 值，以及（当 `reason_code` 非 null 时）`reasonCodeLabel(reason_code, t)` 文本（key 必含，值 nullable）。
  - `state === 'unknown'` 的行必须同时包含 `t('task.state.unknown')` 文本与"未确认"语义文案（`t('task.state.unknown')` 已表达），且**不得**渲染任何"失败/自动重试"文案或重试按钮。
  - `result_evidence` 非 null 时渲染 `t('task.detail.resultEvidence')` 与 snake_case 字面值（`attempted`/`boot_transition_observed`/`execution_rejected`/`os_error`/`journal_lost`/`none` 六种类之一），只展示当前授权的结果证据种类，不冒称完整 wire 证据对象或成功证明（04 §6：UI 展示证据类别，不写“确定由本指令导致”）；`result_evidence` 为 null（证据未产生）不渲染证据行。
  - UI 不得消费子任务 DTO 的任何 `execution_token` 或其摘要用于展示或安全判断（02 §1：投影不暴露 execution_token；守卫对扩展字段忽略不拒绝，见 Task 3）。
  - `showReleaseAction && state === 'unknown' && lock_released !== true` 才渲染释放按钮（`data-testid="release-lock-<id>"`），点击 emit `releaseLock`；`lock_released` 是可选兼容字段，后端投影不承诺输出，UI 不得据其缺失推断危险授权（释放判定以 `state === 'unknown'` 为准，04 §8）。
  - 空数组渲染 `t(emptyKey)` 提示；不渲染任何"总数/隐藏数量"。

- [ ] **Step 1: 建立可编译桩代码**

```vue
<script setup lang="ts">
import type { SubTaskItem } from '../task-ops'

defineProps<{
  subtasks: SubTaskItem[]
  showReleaseAction?: boolean
  emptyKey?: string
}>()

defineEmits<{ releaseLock: [subtask: SubTaskItem] }>()
</script>

<template>
  <div class="task-state-list-stub"></div>
</template>
```

- [ ] **Step 2: 编写失败测试**

`apps/controller-web/src/components/TaskStateList.test.ts`：

```ts
import { fireEvent, render, screen } from '@testing-library/vue'
import { describe, expect, it, vi } from 'vitest'
import TaskStateList from './TaskStateList.vue'
import { createI18n } from '../i18n'
import type { SubTaskItem } from '../task-ops'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const DEV_A = 'a'.repeat(64)
const SUB_ID = '123e4567-e89b-42d3-a456-426614174000'

function sub(overrides: Partial<SubTaskItem> = {}): SubTaskItem {
  return {
    id: SUB_ID, device_id: DEV_A, state: 'unknown',
    reason_code: 'RESULT_TIMEOUT', revision: '4', lock_released: false,
    result_evidence: 'boot_transition_observed', ...overrides,
  }
}

function renderList(props: Record<string, unknown>) {
  return render(TaskStateList, { props, global: { provide: { i18n } } })
}

describe('TaskStateList truthful state presentation', () => {
  it('renders the localized state label, reason code label, revision and result evidence kind', () => {
    renderList({ subtasks: [sub()] })
    const row = screen.getByTestId(`subtask-row-${SUB_ID}`)
    expect(row.textContent).toContain(i18n.t('task.state.unknown'))
    expect(row.textContent).toContain(i18n.t('task.reason.RESULT_TIMEOUT'))
    expect(row.textContent).toContain('4')
    // 只展示当前授权的结果证据种类（snake_case 字面值），不冒称完整 wire 证据对象/成功证明
    expect(row.textContent).toContain('boot_transition_observed')
    expect(row.textContent).toContain(i18n.t('task.detail.resultEvidence'))
  })

  it('hides the evidence line when result_evidence is null and does not leak a token digest', () => {
    // result_evidence 为 null（证据未产生）：不渲染证据行；扩展字段 execution_token/其摘要不被模板消费——
    // 用真实传入组件的受控 synthetic 扩展对象（synthetic 值不来自真实秘密；DTO 类型不新增这两个字段，守卫忽略不拒绝），
    // 证明整个模板不 JSON 序列化/debug dump 整个对象（RED 阶段：空桩 getByTestId 先行抛错，真实未来 RED）
    const incoming = {
      ...sub({ result_evidence: null }),
      execution_token: 'synthetic-never-render-token-7f3a91',
      execution_token_hash: 'synthetic-never-render-hash-c4d802',
    }
    renderList({ subtasks: [incoming] })
    const row = screen.getByTestId(`subtask-row-${SUB_ID}`)
    expect(row.textContent).not.toContain(i18n.t('task.detail.resultEvidence'))
    expect(row.textContent).not.toContain('boot_transition_observed')
    // 组件输出（含行外）不得出现两个唯一 synthetic 值：模板未把整个对象序列化/dump
    expect(document.body.textContent).not.toContain('synthetic-never-render-token-7f3a91')
    expect(document.body.textContent).not.toContain('synthetic-never-render-hash-c4d802')
  })

  it('separates the ten states without collapsing accepted into success or unknown into failure', () => {
    const states = ['queued', 'held', 'dispatching', 'accepted', 'verifying',
      'succeeded', 'failed', 'unknown', 'cancelled', 'expired'] as const
    renderList({ subtasks: states.map((state, index) => sub({
      id: `sub-${index}`, state, reason_code: null, lock_released: true,
    })) })
    for (const state of states) {
      expect(screen.getAllByText(i18n.t(`task.state.${state}`)).length).toBeGreaterThan(0)
    }
    const acceptedRow = screen.getByTestId('subtask-row-sub-3')
    expect(acceptedRow.textContent).not.toContain(i18n.t('task.state.succeeded'))
    const unknownRow = screen.getByTestId('subtask-row-sub-7')
    expect(unknownRow.textContent).not.toContain(i18n.t('task.state.failed'))
    expect(unknownRow.textContent).not.toContain(i18n.t('state.retry'))
  })

  it('shows the release action only when authorized, unknown and not already released', async () => {
    const releaseLock = vi.fn()
    const { rerender } = renderList({
      subtasks: [sub()], showReleaseAction: false, onReleaseLock: releaseLock,
    })
    expect(screen.queryByTestId(`release-lock-${SUB_ID}`)).toBeNull()

    await rerender({ subtasks: [sub()], showReleaseAction: true, onReleaseLock: releaseLock })
    const button = screen.getByTestId(`release-lock-${SUB_ID}`)
    await fireEvent.click(button)
    expect(releaseLock).toHaveBeenCalledWith(sub())

    await rerender({ subtasks: [sub({ lock_released: true })], showReleaseAction: true, onReleaseLock: releaseLock })
    expect(screen.queryByTestId(`release-lock-${SUB_ID}`)).toBeNull()

    await rerender({ subtasks: [sub({ state: 'succeeded' })], showReleaseAction: true, onReleaseLock: releaseLock })
    expect(screen.queryByTestId(`release-lock-${SUB_ID}`)).toBeNull()
  })

  it('renders the empty notice without any hidden-target count', () => {
    renderList({ subtasks: [] })
    expect(screen.getByText(i18n.t('task.list.empty'))).toBeTruthy()
    expect(document.body.textContent).not.toMatch(/\d+\s*(个|hidden|隐藏)/)
  })
})
```

- [ ] **Step 3: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/components/TaskStateList.test.ts`

预期：文件可编译加载，运行 5 个用例全部失败（原 4 + 新增 result_evidence null 不渲染/token 不展示 1，不虚报）：`TestingLibraryElementError: Unable to find an element by: [data-testid="subtask-row-..."]`。
（有效行为 RED。）

- [ ] **Step 4: 最小实现**

```vue
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
```

- [ ] **Step 5: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/components/TaskStateList.test.ts`

预期：PASS（5 个用例，含新增 result_evidence null 不渲染/token 不展示 1 个）。

- [ ] **Step 6: 提交**

```bash
git add apps/controller-web/src/components/TaskStateList.vue apps/controller-web/src/components/TaskStateList.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 新增真实子任务状态列表与受限释放入口"
```

---

### Task 7: `TaskModals.vue` —— 批量风险确认与 unknown 释放锁二级确认（focus trap / 焦点恢复 / Esc）

**Files:**
- Create: `apps/controller-web/src/components/TaskModals.vue`
- Create: `apps/controller-web/src/components/TaskModals.test.ts`

**Interfaces:**
- Consumes: `i18n`（`inject('i18n')`）、`BaseButton.vue`；`task-ops.ts` 的 `SubTaskItem`。
- Produces（一个组件承载两个互斥对话框，`open` 时挂载，关闭时 `emit('close')` 并自行恢复焦点）：
  ```ts
  type ReleaseLockPayload = {
    subtaskId: string
    expected_revision: string
    acknowledge_unknown: true
    reason: string
  }

  // props
  defineProps<{
    mode: 'batch-risk' | 'release-lock'
    open: boolean
    subtask?: SubTaskItem | null            // mode === 'release-lock' 时必填
    busy?: boolean
    confirmDisabled?: boolean               // 批量模式由父组件按 riskConfirmed/busy 计算
  }>()
  // emits
  defineEmits<{
    close: []
    confirmBatchRisk: []
    confirmReleaseLock: [payload: ReleaseLockPayload]
  }>()
  ```
- 行为契约（全部为后续测试与 Task 8/9 的依赖）：
  1. 仅当 `open === true` 时渲染根元素，根元素具有 `role="dialog"`、`aria-modal="true"`、`:aria-labelledby="titleId"`，且存在 id 等于 `titleId` 的标题元素。
  2. 打开时把焦点移到对话框内第一个可聚焦元素；**初次以 `open=true` 挂载时也必须生效**：此时 `watch` 不会触发，必须在 `onMounted` + `nextTick` 后执行初始聚焦（与 `watch(open)` 路径共用同一聚焦函数，保证行为一致）。Tab 从最后一个可聚焦元素前进时回到第一个，Shift+Tab 从第一个后退时到最后一个（**原生焦点捕获循环 / Focus Trap**）。可聚焦候选必须基于原生可聚焦属性筛选，选择器集合固定为：`input:not([disabled]):not([type="hidden"]):not([tabindex="-1"]), button:not([disabled]):not([tabindex="-1"]), select:not([disabled]):not([tabindex="-1"]), textarea:not([disabled]):not([tabindex="-1"]), [tabindex]:not([tabindex="-1"]):not([type="hidden"])`（按 DOM 顺序取第一个/最后一个）。jsdom 环境无真实布局，`offsetParent` 恒为 `null`，**禁止**用 `offsetParent !== null` 判定可见可聚焦。真实浏览器（Task 11）另验 Tab 顺序、可见性与 disabled/hidden/`tabindex="-1"` 元素被正确跳过的循环行为。
  3. Esc 关闭：`emit('close')`，并把焦点自动恢复到打开前记录的源触发元素（打开瞬间记录的 `document.activeElement`）；恢复前必须检查该元素仍处于 document 中且具有 `focus` 方法（如 `document.contains(trigger) && typeof trigger.focus === 'function'`），不满足时静默跳过，不得抛错或把焦点留在已卸载的 dialog 内。
  4. 关闭（Esc 或点"取消"）**绝不** emit `confirmReleaseLock` / `confirmBatchRisk`（取消零写请求）。
  5. `release-lock` 模式：包含 `task.risk.releaseLockTitle` 标题、`task.risk.releaseWarningDetail` 警示、带 `:aria-label="t('task.field.releaseReason')"` 的文本输入、`task.risk.acknowledgeUnknown` 复选框、`task.confirmRelease` 与 `common.cancel` 按钮。确认按钮在 `reason.trim() === ''`、未勾选或 `busy` 时 `disabled`。
  6. `release-lock` 确认时 emit 的 payload 必须是 `{ subtaskId: subtask.id, expected_revision: subtask.revision, acknowledge_unknown: true, reason: <trim 后非空字符串> }`。
  7. `batch-risk` 模式：包含 `task.risk.confirmBatchReboot` 复选框（`v-model` 内部状态）与 `task.submitReboot` 确认按钮；确认按钮必须满足 `batchDisabled = !batchConfirmed || props.confirmDisabled === true || props.busy`；确认时 emit `confirmBatchRisk`，**绝不**在组件内部生成幂等键或调用网络。
  8. 所有文案经 `t()`；不出现硬编码中文/英文字面量。

- [ ] **Step 1: 建立可编译桩代码**

```vue
<script setup lang="ts">
import type { SubTaskItem } from '../task-ops'

defineProps<{
  mode: 'batch-risk' | 'release-lock'
  open: boolean
  subtask?: SubTaskItem | null
  busy?: boolean
  confirmDisabled?: boolean
}>()

defineEmits<{
  close: []
  confirmBatchRisk: []
  confirmReleaseLock: [payload: {
    subtaskId: string
    expected_revision: string
    acknowledge_unknown: true
    reason: string
  }]
}>()
</script>

<template>
  <div class="task-modals-stub"></div>
</template>
```

- [ ] **Step 2: 编写失败测试**

`apps/controller-web/src/components/TaskModals.test.ts`：

```ts
import { fireEvent, render, screen, waitFor } from '@testing-library/vue'
import { describe, expect, it, vi } from 'vitest'
import TaskModals from './TaskModals.vue'
import { createI18n } from '../i18n'
import type { SubTaskItem } from '../task-ops'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const DEV_A = 'a'.repeat(64)
const SUB_ID = '123e4567-e89b-42d3-a456-426614174000'

// 最小 6 字段子任务（02 §1:24）；本测试只消费 id/revision 构造 release-lock payload
const subtask: SubTaskItem = {
  id: SUB_ID, device_id: DEV_A, state: 'unknown',
  reason_code: 'RESULT_TIMEOUT', revision: '4', lock_released: false, result_evidence: null,
}

function renderModals(props: Record<string, unknown>) {
  return render(TaskModals, { props, global: { provide: { i18n } } })
}

describe('TaskModals accessibility contract', () => {
  it('exposes dialog semantics with a labelled title and aria-modal', () => {
    renderModals({ mode: 'release-lock', open: true, subtask })
    const dialog = screen.getByRole('dialog')
    expect(dialog.getAttribute('aria-modal')).toBe('true')
    const labelledBy = dialog.getAttribute('aria-labelledby')!
    expect(labelledBy).toBeTruthy()
    expect(document.getElementById(labelledBy)!.textContent).toBe(i18n.t('task.risk.releaseLockTitle'))
    expect(screen.getByText(i18n.t('task.risk.releaseWarningDetail'))).toBeTruthy()
  })

  it('renders nothing while closed', () => {
    renderModals({ mode: 'release-lock', open: false, subtask })
    expect(screen.queryByRole('dialog')).toBeNull()
  })

  it('moves focus into the dialog on open and traps Tab and Shift+Tab inside it', async () => {
    renderModals({ mode: 'release-lock', open: true, subtask })
    const dialog = screen.getByRole('dialog')
    const focusables = dialog.querySelectorAll<HTMLElement>('input, button, select, textarea, [tabindex]:not([tabindex="-1"])')
    expect(focusables.length).toBeGreaterThan(1)
    // 焦点在 open 后的下一个 tick 落入对话框（onMounted/watch -> nextTick -> focus）
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true))

    const first = focusables[0]!
    const last = focusables[focusables.length - 1]!
    last.focus()
    await fireEvent.keyDown(last, { key: 'Tab', code: 'Tab' })
    expect(document.activeElement).toBe(first)

    first.focus()
    await fireEvent.keyDown(first, { key: 'Tab', code: 'Tab', shiftKey: true })
    expect(document.activeElement).toBe(last)
  })

  it('closes on Escape, restores focus to the trigger and emits no confirmation', async () => {
    const confirmReleaseLock = vi.fn()
    const close = vi.fn()
    const host = document.createElement('button')
    host.textContent = 'trigger'
    document.body.appendChild(host)
    host.focus()
    expect(document.activeElement).toBe(host)

    const { rerender } = render(TaskModals, {
      props: { mode: 'release-lock', open: true, subtask },
      global: { provide: { i18n } },
    })
    await rerender({ mode: 'release-lock', open: true, subtask, onClose: close, onConfirmReleaseLock: confirmReleaseLock })

    const dialog = screen.getByRole('dialog')
    // 打开后焦点应已落入对话框（还原目标在 onMounted 已记录）
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true))
    await fireEvent.keyDown(dialog, { key: 'Escape', code: 'Escape' })
    expect(close).toHaveBeenCalledOnce()
    expect(confirmReleaseLock).not.toHaveBeenCalled()
    // Esc 后焦点必须恢复到打开前的触发按钮
    await waitFor(() => expect(document.activeElement).toBe(host))
    host.remove()
  })

  it('restores focus to the trigger when the parent closes it by setting open=false, emitting no close', async () => {
    const confirmReleaseLock = vi.fn()
    const close = vi.fn()
    const host = document.createElement('button')
    host.textContent = 'trigger'
    document.body.appendChild(host)
    host.focus()
    expect(document.activeElement).toBe(host)

    const { rerender } = render(TaskModals, {
      props: { mode: 'release-lock', open: true, subtask },
      global: { provide: { i18n } },
    })
    await rerender({ mode: 'release-lock', open: true, subtask, onClose: close, onConfirmReleaseLock: confirmReleaseLock })

    const dialog = screen.getByRole('dialog')
    // 打开后焦点应已落入对话框（还原目标在 onMounted 已记录）
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true))

    // 父组件成功提交后直接把 open 置为 false：组件常驻，close() 与 onBeforeUnmount 都不会执行
    await rerender({ mode: 'release-lock', open: false, subtask, onClose: close, onConfirmReleaseLock: confirmReleaseLock })

    // 焦点必须回到打开前的触发按钮，且 dialog 已从 DOM 移除
    await waitFor(() => expect(document.activeElement).toBe(host))
    expect(screen.queryByRole('dialog')).toBeNull()
    // 父驱动关闭是父组件的决定：组件不得回授 emit('close')，也不得触发任何确认
    expect(close).not.toHaveBeenCalled()
    expect(confirmReleaseLock).not.toHaveBeenCalled()
    host.remove()
  })

  it('requires a non-blank reason and the risk acknowledgement before confirming, then emits the exact CAS payload', async () => {
    const confirmReleaseLock = vi.fn()
    render(TaskModals, {
      props: { mode: 'release-lock', open: true, subtask },
      global: { provide: { i18n } },
    })

    const confirmBtn = screen.getByRole('button', { name: new RegExp(i18n.t('task.confirmRelease'), 'i') })
    expect((confirmBtn as HTMLButtonElement).disabled).toBe(true)

    await fireEvent.update(screen.getByLabelText(new RegExp(i18n.t('task.field.releaseReason'), 'i')), '现场人工确认重启已完成')
    expect((confirmBtn as HTMLButtonElement).disabled).toBe(true)

    await fireEvent.click(screen.getByRole('checkbox', { name: new RegExp(i18n.t('task.risk.acknowledgeUnknown'), 'i') }))
    expect((confirmBtn as HTMLButtonElement).disabled).toBe(false)

    const { rerender } = render(TaskModals, {
      props: { mode: 'release-lock', open: true, subtask, onConfirmReleaseLock: confirmReleaseLock },
      global: { provide: { i18n } },
    })
    const buttons = screen.getAllByRole('button', { name: new RegExp(i18n.t('task.confirmRelease'), 'i') })
    const last = buttons[buttons.length - 1] as HTMLButtonElement
    expect(last.disabled).toBe(true)

    // 以父组件门控为准：confirmDisabled 必须能压住确认按钮
    await rerender({ mode: 'release-lock', open: true, subtask, busy: true, onConfirmReleaseLock: confirmReleaseLock })
    expect((screen.getAllByRole('button', { name: new RegExp(i18n.t('task.confirmRelease'), 'i') })
      .pop() as HTMLButtonElement).disabled).toBe(true)
  })

  it('emits the trimmed release payload with acknowledge_unknown true', async () => {
    const confirmReleaseLock = vi.fn()
    render(TaskModals, {
      props: { mode: 'release-lock', open: true, subtask, onConfirmReleaseLock: confirmReleaseLock },
      global: { provide: { i18n } },
    })
    await fireEvent.update(screen.getByLabelText(new RegExp(i18n.t('task.field.releaseReason'), 'i')), '  现场人工确认  ')
    await fireEvent.click(screen.getByRole('checkbox', { name: new RegExp(i18n.t('task.risk.acknowledgeUnknown'), 'i') }))
    await fireEvent.click(screen.getByRole('button', { name: new RegExp(i18n.t('task.confirmRelease'), 'i') }))
    expect(confirmReleaseLock).toHaveBeenCalledWith({
      subtaskId: SUB_ID,
      expected_revision: '4',
      acknowledge_unknown: true,
      reason: '现场人工确认',
    })
  })
})

describe('TaskModals batch risk confirmation', () => {
  it('never generates an idempotency key or calls the network itself', async () => {
    const fetcher = vi.fn()
    vi.stubGlobal('fetch', fetcher)
    const confirmBatchRisk = vi.fn()
    render(TaskModals, {
      props: { mode: 'batch-risk', open: true, confirmDisabled: true, onConfirmBatchRisk: confirmBatchRisk },
      global: { provide: { i18n } },
    })
    const checkbox = screen.getByRole('checkbox', { name: new RegExp(i18n.t('task.risk.confirmBatchReboot'), 'i') })
    await fireEvent.click(checkbox)
    const submit = screen.getByRole('button', { name: new RegExp(i18n.t('task.submitReboot'), 'i') }) as HTMLButtonElement
    expect(submit.disabled).toBe(true) // 父组件未放行时仍禁用
    await fireEvent.click(checkbox)
    expect(confirmBatchRisk).not.toHaveBeenCalled()
    expect(fetcher).not.toHaveBeenCalled()
    vi.unstubAllGlobals()
  })
})
```

说明：`busy`/`confirmDisabled` 门控必须由组件自身实现（第 5/7 条），否则最后一个用例按预期失败。

- [ ] **Step 3: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/components/TaskModals.test.ts`

预期：文件可编译加载，运行 8 个用例，全部或绝大多数失败：`TestingLibraryElementError: Unable to find role="dialog"`、`expected false to be true`。
（有效行为 RED。）

- [ ] **Step 4: 最小实现**

```vue
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
  if (target && typeof target.focus === 'function' && target.isConnected) target.focus()
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
            :data-testid="'confirm-release'"
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
            :data-testid="'confirm-batch-risk'"
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
```

说明：`batchDisabled` 表达式必须为 `!batchConfirmed.value || props.confirmDisabled === true || props.busy`，确保在用户未勾选 risk 确认、父组件显式传入 confirmDisabled 为 true 或在途 busy 状态下均禁用确认按钮，避免未确认即提交。

- [ ] **Step 5: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/components/TaskModals.test.ts`

预期：PASS（8 个用例，含父组件置 `open=false` 的程序化关闭用例：焦点回到触发按钮、dialog 已移除且不回授 `emit('close')`）。若 Tab 捕获用例失败，**不得**放宽断言：焦点捕获必须基于原生可聚焦属性筛选（`input:not([disabled])`/`button:not([disabled])` 等），并用 `preventDefault()` + `focus()` 手动回绕，确认真实 DOM 焦点确实变化；不得用 `offsetParent` 判定可见性（jsdom 恒为 `null`）。

- [ ] **Step 6: 提交**

```bash
git add apps/controller-web/src/components/TaskModals.vue apps/controller-web/src/components/TaskModals.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 新增无障碍批量风险与未知结果释放确认弹窗"
```

---

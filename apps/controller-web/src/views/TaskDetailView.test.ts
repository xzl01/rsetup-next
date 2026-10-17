import { fireEvent, render, screen, waitFor } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import TaskDetailView from './TaskDetailView.vue'
import { get } from '../api'
import { createI18n } from '../i18n'
import { baseTaskTimeEvidence, zeroTaskCounts, type SubTaskItem, type TaskItem, type TaskPage } from '../task-ops'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const DEV_A = 'a'.repeat(64)
const TASK_ID = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const SUB_A = '123e4567-e89b-42d3-a456-426614174000'
const SUB_B = '223e4567-e89b-42d3-a456-426614174001'

// 基线 fixture：后端返回 can_cancel true（取消入口默认"可请求"，负例逐个覆盖）
const heldTask: TaskItem = {
  id: TASK_ID, operation: 'reboot', state: 'held', outcome: null, revision: '7',
  created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(),
  view_scope: 'authorized_subset', actions: { can_cancel: true },
}

function child(overrides: Partial<SubTaskItem> = {}): SubTaskItem {
  return {
    id: SUB_A, device_id: DEV_A, state: 'held',
    reason_code: 'TIME_UNCERTAIN', revision: '3', lock_released: false, result_evidence: null, ...overrides,
  }
}

function name(key: string): RegExp { return new RegExp(i18n.t(key), 'i') }
function mount(props: Record<string, unknown>) {
  // 测试替身注入点：props 为组件契约的任意子集，render 的 ComponentProps 泛型在此按 never 放宽
  return render(TaskDetailView, { props: props as never, global: { provide: { i18n } } })
}

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals() })

describe('TaskDetailView authorization projection and cancellation', () => {
  // 组件本地 ref 只在挂载时由 initialTask 初始化一次，rerender 换 initialTask 不改变数据，改数据须独立 mount
  it('shows the authorized subset notice for an explicit scope and the unknown notice otherwise', async () => {
    const subset = mount({ taskId: TASK_ID, initialTask: { ...heldTask, view_scope: 'authorized_subset' }, initialChildren: [child()] })
    expect(screen.getByText(i18n.t('task.scope.authorizedSubset'))).toBeTruthy()
    subset.unmount()
    const unknown = mount({ taskId: TASK_ID, initialTask: { ...heldTask, view_scope: undefined }, initialChildren: [child()] })
    expect(screen.getByText(i18n.t('task.scope.unknown'))).toBeTruthy()
    unknown.unmount()
  })

  it('determines the cancel entry solely from the backend can_cancel in the deep-link scenario', async () => {
    const onCancel = vi.fn(async () => undefined)
    const onLoadTask = vi.fn(async () => ({ ...heldTask, actions: { can_cancel: true } }))
    const onLoadChildren = vi.fn(async () => ({ items: [child({ device_id: DEV_A })], next_cursor: null }))
    const devicePermissions = { [DEV_A]: ['device.task.read'] }
    mount({ taskId: TASK_ID, onLoadTask, onLoadChildren, devicePermissions, onCancel })
    await waitFor(() => expect(screen.getByRole('button', { name: name('task.cancel.action') })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: name('task.cancel.action') }))
    expect(onCancel).toHaveBeenCalledTimes(1)
    expect(onCancel).toHaveBeenCalledWith({ taskId: TASK_ID, expectedRevision: '7', confirm: true })
  })

  it('keeps the CAS payload only while the backend allows and closes once can_cancel flips false', async () => {
    let loadCalls = 0
    const onLoadTask = vi.fn(async () => {
      loadCalls += 1
      return loadCalls === 1 ? { ...heldTask, actions: { can_cancel: true } } : { ...heldTask, revision: '8', actions: { can_cancel: false } }
    })
    const onCancel = vi.fn(async () => undefined)
    mount({ taskId: TASK_ID, initialChildren: [child()], isAdmin: true, onCancel, onLoadTask })
    await waitFor(() => expect(screen.getByRole('button', { name: name('task.cancel.action') })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: name('task.cancel.action') }))
    await waitFor(() => expect(onCancel).toHaveBeenCalledTimes(1))
    expect(onCancel).toHaveBeenCalledWith({ taskId: TASK_ID, expectedRevision: '7', confirm: true })
    await waitFor(() => expect(screen.queryByRole('button', { name: name('task.cancel.action') })).toBeNull())
    expect(onCancel).toHaveBeenCalledTimes(1)
    expect(onLoadTask.mock.calls.length).toBeGreaterThanOrEqual(2)
    expect(screen.getByText(i18n.t('task.cancel.notCancellable'))).toBeTruthy()
  })

  it('keeps a running main task open when the backend reports can_cancel true and submits the CAS payload', async () => {
    const onCancel = vi.fn(async () => undefined)
    mount({ taskId: TASK_ID, initialTask: { ...heldTask, state: 'running' }, initialChildren: [child({ state: 'accepted' })], onCancel })
    const button = screen.getByRole('button', { name: name('task.cancel.action') })
    await fireEvent.click(button)
    await waitFor(() => expect(onCancel).toHaveBeenCalledTimes(1))
    expect(onCancel).toHaveBeenCalledWith({ taskId: TASK_ID, expectedRevision: '7', confirm: true })
  })

  it('does not close an authorized_subset completed main task by main state when the backend allows', async () => {
    const onCancel = vi.fn(async () => undefined)
    mount({ taskId: TASK_ID, initialTask: { ...heldTask, state: 'completed', outcome: 'cancelled' }, initialChildren: [child()], onCancel })
    const button = screen.getByRole('button', { name: name('task.cancel.action') })
    await fireEvent.click(button)
    expect(onCancel).toHaveBeenCalledTimes(1)
    expect(onCancel).toHaveBeenCalledWith({ taskId: TASK_ID, expectedRevision: '7', confirm: true })
  })

  it('closes the entry with zero callbacks when the backend reports can_cancel false for a queued main task', async () => {
    const onCancel = vi.fn(async () => undefined)
    mount({ taskId: TASK_ID, initialTask: { ...heldTask, state: 'queued', outcome: null, actions: { can_cancel: false } }, initialChildren: [child({ state: 'queued', reason_code: null })], onCancel })
    expect(screen.queryByRole('button', { name: name('task.cancel.action') })).toBeNull()
    expect(screen.getByText(i18n.t('task.cancel.notCancellable'))).toBeTruthy()
    expect(onCancel).not.toHaveBeenCalled()
  })

  it('closes the dangerous entry when actions are missing or malformed even for a readable queued main task', async () => {
    const cases: Array<{ can_cancel: boolean } | undefined> = [undefined, { can_cancel: 'yes' as never }]
    for (const actions of cases) {
      const onCancel = vi.fn(async () => undefined)
      const wrapper = mount({ taskId: TASK_ID, initialTask: { ...heldTask, state: 'queued', outcome: null, actions }, initialChildren: [child({ state: 'queued', reason_code: null })], onCancel })
      expect(screen.queryByRole('button', { name: name('task.cancel.action') })).toBeNull()
      expect(onCancel).not.toHaveBeenCalled()
      wrapper.unmount()
    }
  })

  it('opens the entry under a legitimate full scope when the backend allows', async () => {
    const onCancel = vi.fn(async () => undefined)
    mount({ taskId: TASK_ID, initialTask: { ...heldTask, state: 'queued', outcome: null, view_scope: 'full' }, initialChildren: [child({ state: 'queued', reason_code: null })], onCancel })
    const button = screen.getByRole('button', { name: name('task.cancel.action') })
    await fireEvent.click(button)
    expect(onCancel).toHaveBeenCalledTimes(1)
  })

  it('closes the dangerous entry for an unknown view_scope even when can_cancel is true', async () => {
    const onCancel = vi.fn(async () => undefined)
    mount({ taskId: TASK_ID, initialTask: { ...heldTask, view_scope: 'legacy' as never }, initialChildren: [child({ state: 'queued', reason_code: null })], onCancel })
    expect(screen.queryByRole('button', { name: name('task.cancel.action') })).toBeNull()
    expect(onCancel).not.toHaveBeenCalled()
  })

  it('never lets task.read permission or a queued first page override a backend can_cancel false', async () => {
    const onCancel = vi.fn(async () => undefined)
    const devicePermissions = { [DEV_A]: ['device.task.read'] }
    mount({ taskId: TASK_ID, initialTask: { ...heldTask, state: 'queued', outcome: null, actions: { can_cancel: false } }, initialChildren: [child({ state: 'queued', reason_code: null })], devicePermissions, onCancel })
    expect(screen.queryByRole('button', { name: name('task.cancel.action') })).toBeNull()
    expect(onCancel).not.toHaveBeenCalled()
  })

  it('paginates children with the opaque cursor and disables load more while a page is in flight', async () => {
    let resolvePage!: (v: { items: SubTaskItem[]; next_cursor: string | null }) => void
    const onLoadChildren = vi.fn(() => new Promise<{ items: SubTaskItem[]; next_cursor: string | null }>((r) => { resolvePage = r }))
    mount({ taskId: TASK_ID, initialTask: heldTask, initialChildren: [child()], initialNextCursor: 'cursor-1', onLoadChildren })
    await fireEvent.click(screen.getByRole('button', { name: name('task.list.loadMore') }))
    expect(onLoadChildren).toHaveBeenCalledWith(TASK_ID, 'cursor-1')
    // 分页在途：加载更多进入禁用态，重复点击不得再次发起请求或推进读取代际
    const loadMore = screen.getByRole('button', { name: name('task.list.loadMore') }) as HTMLButtonElement
    expect(loadMore.disabled).toBe(true)
    await fireEvent.click(loadMore)
    expect(onLoadChildren).toHaveBeenCalledTimes(1)
    resolvePage({ items: [child({ id: SUB_B })], next_cursor: null })
    await waitFor(() => expect(screen.getByTestId(`subtask-row-${SUB_B}`)).toBeTruthy())
  })

  it('triggers release lock modal and refetches children via GET on success', async () => {
    const onReleaseLock = vi.fn(async () => undefined)
    const reloaded = vi.fn(async () => ({ items: [child({ state: 'unknown', lock_released: true })], next_cursor: null }))
    mount({
      taskId: TASK_ID, initialTask: heldTask, initialChildren: [child({ state: 'unknown', lock_released: false })],
      isAdmin: true, onReleaseLock, onLoadChildren: reloaded,
    })
    await fireEvent.click(screen.getByTestId(`release-lock-${SUB_A}`))
    expect(screen.getByRole('dialog')).toBeTruthy()
    await fireEvent.update(screen.getByLabelText(name('task.field.releaseReason')), '现场确认已重启')
    await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.acknowledgeUnknown') }))
    await fireEvent.click(screen.getByTestId('confirm-release'))
    await waitFor(() => expect(onReleaseLock).toHaveBeenCalledWith({
      subtaskId: SUB_A,
      expected_revision: '3',
      acknowledge_unknown: true,
      reason: '现场确认已重启',
    }))
    await waitFor(() => expect(reloaded).toHaveBeenCalledWith(TASK_ID, null))
    // 主任务状态直接定位 task-state 元素：主任务 held 文案与子任务 held 文案同词（"等待核验"），
    // 通用文本查询会与子任务行混淆
    expect(screen.getByTestId('task-state').textContent).toContain(i18n.t('task.mainState.held'))
  })
})

describe('TaskDetailView parallel loading error isolation', () => {
  // WEB-N1 ①（WEB-R5-1 修订）：页面已有主任务快照（initialTask: heldTask）——取消提交成功后组件按既有契约 loadTask() GET 重取主任务，重取失败（500）在 catch 置 taskError 且保留旧 task，
  // children 后成功——children 的成功不得把已可见的 state.error 抹为 null；task 非空使 v-if="task" 满足，子任务行在 DOM 中可达
  it('keeps the visible task error when children resolve after the main task request already failed', async () => {
    let resolveChildren!: (v: { items: SubTaskItem[]; next_cursor: string | null }) => void
    const onLoadTask = vi.fn(async () => { throw new Error('HTTP_500') })
    const onLoadChildren = vi.fn(() => new Promise<{ items: SubTaskItem[]; next_cursor: string | null }>((r) => { resolveChildren = r }))
    const onCancel = vi.fn(async () => {})
    mount({ taskId: TASK_ID, initialTask: heldTask, onLoadTask, onLoadChildren, onCancel })
    await fireEvent.click(screen.getByRole('button', { name: name('task.cancel.action') }))
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    expect(onLoadTask).toHaveBeenCalledWith(TASK_ID)
    expect(onLoadChildren).toHaveBeenCalledWith(TASK_ID, null)
    resolveChildren({ items: [child()], next_cursor: null })
    await waitFor(() => expect(screen.getByTestId(`subtask-row-${SUB_A}`)).toBeTruthy())
    expect(screen.getByRole('alert')).toBeTruthy()
  })

  // WEB-N1 ②：children 先失败（500）、主任务后成功——主任务的成功不得抹除错误并伪装为空列表
  it('keeps the visible children error when the main task resolves after the children request already failed', async () => {
    let resolveTask!: (v: TaskItem) => void
    const onLoadTask = vi.fn(() => new Promise<TaskItem>((r) => { resolveTask = r }))
    const onLoadChildren = vi.fn(async () => { throw new Error('HTTP_500') })
    mount({ taskId: TASK_ID, onLoadTask, onLoadChildren })
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    expect(onLoadTask).toHaveBeenCalledWith(TASK_ID)
    resolveTask(heldTask)
    await waitFor(() => expect(screen.getByTestId('task-id').textContent).toBe(TASK_ID))
    expect(screen.getByRole('alert')).toBeTruthy()
  })

  it('recovers from an action error upon a successful retry', async () => {
    // WEB-R5-2：取消首次失败置 actionError，同任务重试成功后必须清除，alert 不得永久残留
    const onLoadTask = vi.fn(async () => ({ ...heldTask, actions: { can_cancel: true } }))
    const onCancel = vi.fn()
      .mockRejectedValueOnce(new Error('HTTP_500'))
      .mockResolvedValueOnce(undefined)
    mount({ taskId: TASK_ID, onLoadTask, onCancel })
    await waitFor(() => expect(screen.getByTestId('task-id').textContent).toBe(TASK_ID))
    await fireEvent.click(screen.getByRole('button', { name: name('task.cancel.action') }))
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: name('task.cancel.action') }))
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull())
    expect(onCancel).toHaveBeenCalledTimes(2)
  })
})

describe('TaskDetailView held time review', () => {
  it('hides decision controls from non-admins and shows the no-held notice when nothing is held', async () => {
    // 非管理员 + full scope + counts.held=1：当前页有 held 子任务仍不渲染任何决策按钮
    //（组件本地 ref 只初始化一次，rerender 换 initialTask/initialChildren 不改变数据，改数据须独立 mount）
    const first = mount({
      taskId: TASK_ID,
      initialTask: { ...heldTask, view_scope: 'full', counts: { ...zeroTaskCounts(), held: '1' } },
      initialChildren: [child()],
      isAdmin: false,
    })
    expect(screen.queryByRole('button', { name: name('task.held.reviewResumeOriginalDeadline') })).toBeNull()
    expect(screen.queryByRole('button', { name: name('task.held.reviewExpire') })).toBeNull()
    expect(screen.getByText(i18n.t('task.held.reviewExplain'))).toBeTruthy()
    expect(screen.queryByLabelText(name('task.detail.evidenceSource'))).toBeNull()
    first.unmount()

    // zero counts（held=0）即便旧 page 仍有 held 子任务也不能开：独立 mount，full 管理员
    mount({
      taskId: TASK_ID,
      initialTask: { ...heldTask, state: 'queued', view_scope: 'full' },
      initialChildren: [child()],
      isAdmin: true,
    })
    expect(screen.queryByRole('button', { name: name('task.held.reviewResumeOriginalDeadline') })).toBeNull()
    expect(screen.queryByRole('button', { name: name('task.held.reviewExpire') })).toBeNull()
    expect(screen.getByText(i18n.t('task.held.reviewNoHeld'))).toBeTruthy()
  })

  it('offers exactly the two non-extending decisions with an explicit three-value evidence choice, and never fabricates the outcome', async () => {
    const decisions: string[] = []
    const reloaded = vi.fn(async () => ({ items: [child({ state: 'queued', reason_code: null })], next_cursor: null }))
    // 服务端原子结果：同一授权视图 revision 前进、当前 counts.held 归零；主记录整体以服务端返回覆盖
    const onTimeReview = vi.fn(async (payload: { decision: 'resume_original_deadline' | 'expire'; evidence: 'reference_deadline_proven' | 'reference_deadline_expired' | 'inconclusive' }) => {
      decisions.push(payload.decision)
      return { ...heldTask, state: 'queued', revision: '8', counts: zeroTaskCounts() }
    })

    // 两次决策分别独立 mount：第 1 决策服务端返回 held=0 后按钮已卸载，不能在同一挂载里再点第二个
    for (const decision of ['resume_original_deadline', 'expire'] as const) {
      const wrapper = mount({
        taskId: TASK_ID,
        initialTask: { ...heldTask, view_scope: 'full', counts: { ...zeroTaskCounts(), held: '1' } },
        initialChildren: [child()],
        isAdmin: true, onTimeReview, onLoadChildren: reloaded,
      })

      // 显式三值证据选择：恰渲染 3 个合法单选项（Task 5 `TimeReviewEvidenceKind` 词表，Task 2 已有 i18n 键），初始无任何预选
      const radioValues = ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'] as const
      for (const kind of radioValues) {
        const radio = screen.getByLabelText(name(`task.timeReview.evidence.${kind}`)) as HTMLInputElement
        expect(radio.type).toBe('radio')
        expect(radio.checked).toBe(false)
      }
      // 页面不再有 textarea 自由输入框
      expect(document.querySelector('textarea')).toBeNull()

      const button = screen.getByRole('button', {
        name: name(decision === 'resume_original_deadline' ? 'task.held.reviewResumeOriginalDeadline' : 'task.held.reviewExpire'),
      }) as HTMLButtonElement
      expect(button.disabled).toBe(true)
      expect(screen.getByText(i18n.t('task.held.reviewSelfReported'))).toBeTruthy()
      expect(document.querySelectorAll('input[type="number"], input[type="datetime-local"]').length).toBe(0)

      // 未显式选择证据前按钮禁用；显式选择一个合法三值后按钮才可点（不默认预选、不接受自由文本）
      const kind = decision === 'resume_original_deadline' ? 'reference_deadline_proven' : 'reference_deadline_expired'
      await fireEvent.click(screen.getByLabelText(name(`task.timeReview.evidence.${kind}`)))
      expect(button.disabled).toBe(false)
      await fireEvent.click(button)
      await waitFor(() => expect(onTimeReview).toHaveBeenCalledWith({
        taskId: TASK_ID, expectedRevision: '7', decision,
        evidence: kind,
      }))
      // 结果以服务端返回为准并重取 children，不本地宣布"已恢复/已延期"
      await waitFor(() => expect(reloaded).toHaveBeenCalledWith(TASK_ID, null))
      expect(screen.getByTestId('task-state').textContent).toContain(i18n.t('task.mainState.queued'))
      expect(screen.getByText(i18n.t('task.held.reviewNoHeld'))).toBeTruthy()
      wrapper.unmount()
    }
    expect(onTimeReview).toHaveBeenCalledTimes(2)
    expect(reloaded).toHaveBeenCalledTimes(2)
    expect(decisions).toEqual(['resume_original_deadline', 'expire'])
  })

  it('gates the dangerous time-review entry on the main task counts.held, never on the paginated children page', async () => {
    type GateCase = { task: TaskItem; children: SubTaskItem[]; isAdmin: boolean; expectButtons: boolean }
    const fullAdminHeld: TaskItem = { ...heldTask, view_scope: 'full', counts: { ...zeroTaskCounts(), held: '1' } }
    const countsMissingHeldKey = { ...zeroTaskCounts() }
    delete (countsMissingHeldKey as Record<string, unknown>).held
    const cases: GateCase[] = [
      // 正例：当前授权全集 counts.held=1、当前页无任何 held 子任务 → 两按钮与 evidence 允许
      { task: fullAdminHeld, children: [child({ state: 'queued', reason_code: null })], isAdmin: true, expectButtons: true },
      // zero counts 即便旧 page 有 held 也关闭
      { task: { ...heldTask, view_scope: 'full' }, children: [child()], isAdmin: true, expectButtons: false },
      // 未知 scope / authorized_subset：危险 review 关闭（只读说明保留）
      { task: { ...fullAdminHeld, view_scope: 'legacy' as never }, children: [child()], isAdmin: true, expectButtons: false },
      { task: { ...fullAdminHeld, view_scope: 'authorized_subset' }, children: [child()], isAdmin: true, expectButtons: false },
      // counts.held 缺键：fail-closed
      { task: { ...fullAdminHeld, counts: countsMissingHeldKey }, children: [child()], isAdmin: true, expectButtons: false },
      // full 非管理员：counts.held=1 也不渲染危险入口
      { task: fullAdminHeld, children: [child()], isAdmin: false, expectButtons: false },
    ]
    for (const c of cases) {
      // 各例独立 mount/unmount；initialTask 为同步路径（本表不走 onLoadTask 异步路径），正例用 waitFor 明确就绪、负例直接断言，无任意 sleep
      const wrapper = mount({ taskId: TASK_ID, initialTask: c.task, initialChildren: c.children, isAdmin: c.isAdmin })
      if (c.expectButtons) {
        await waitFor(() => expect(screen.getByRole('button', { name: name('task.held.reviewResumeOriginalDeadline') })).toBeTruthy())
        expect(screen.getByRole('button', { name: name('task.held.reviewExpire') })).toBeTruthy()
        expect(screen.getByTestId('time-review-evidence')).toBeTruthy()
      } else {
        expect(screen.queryByRole('button', { name: name('task.held.reviewResumeOriginalDeadline') })).toBeNull()
        expect(screen.queryByRole('button', { name: name('task.held.reviewExpire') })).toBeNull()
        expect(screen.getByText(i18n.t('task.held.reviewExplain'))).toBeTruthy()
      }
      wrapper.unmount()
    }
    // 非法 counts.held 值（前导零/小数/负数/非十进制/JSON 数）逐个独立 mount 关闭
    for (const held of ['01', '1.5', '-1', 'x', 1] as unknown[]) {
      const wrapper = mount({
        taskId: TASK_ID,
        initialTask: { ...fullAdminHeld, counts: { ...zeroTaskCounts(), held } as TaskItem['counts'] },
        initialChildren: [child()],
        isAdmin: true,
      })
      expect(screen.queryByRole('button', { name: name('task.held.reviewResumeOriginalDeadline') })).toBeNull()
      expect(screen.queryByRole('button', { name: name('task.held.reviewExpire') })).toBeNull()
      wrapper.unmount()
    }
  })
})

describe('TaskDetailView generation defense', () => {
  it('discards delayed responses from the previous task after taskId switches mid-flight', async () => {
    // 手动 deferred 交错：A 的 children 请求与 cancel 写操作均挂起 → 切到 B（watch 清空并复位四 busy）→ A 迟到返回
    const TASK_B = 'b0e1f2d3-4c5a-4b6d-8e9f-0a1b2c3d4e5f'
    const taskB: TaskItem = { ...heldTask, id: TASK_B }
    const flush = () => new Promise<void>((r) => setTimeout(r, 0))
    let resolveChildrenA!: (v: { items: SubTaskItem[]; next_cursor: string | null }) => void
    let resolveCancelA!: () => void
    let resolveCancelB!: () => void
    const onLoadTask = vi.fn(async (id: string) => ({ ...taskB, id }))
    const onLoadChildren = vi.fn((id: string) => id === TASK_ID
      ? new Promise<{ items: SubTaskItem[]; next_cursor: string | null }>((r) => { resolveChildrenA = r })
      : Promise.resolve({ items: [child({ id: SUB_B })], next_cursor: null }))
    const onCancel = vi.fn((p: { taskId: string }) => p.taskId === TASK_ID
      ? new Promise<void>((r) => { resolveCancelA = r })
      : new Promise<void>((r) => { resolveCancelB = r }))
    const wrapper = mount({ taskId: TASK_ID, initialTask: heldTask, initialChildren: [], onLoadTask, onLoadChildren, onCancel })
    await fireEvent.click(screen.getByRole('button', { name: name('task.cancel.action') }))
    // @testing-library/vue 的 RenderResult.rerender 合并 partial props（内部走 VTU setProps 合并语义），挂载时提供的 onLoadTask/onLoadChildren/onCancel 回调保留
    await wrapper.rerender({ taskId: TASK_B })
    await waitFor(() => expect(screen.getByTestId('task-id').textContent).toBe(TASK_B))
    await fireEvent.click(screen.getByRole('button', { name: name('task.cancel.action') }))
    const cancelBtn = screen.getByRole('button', { name: name('task.cancel.action') })
    expect(cancelBtn.getAttribute('aria-busy')).toBe('true')
    resolveCancelA()
    await flush()
    // A 的过期 finally 不得清除 B 的 cancelBusy（releaseTarget/releaseBusy/reviewBusy/cancelBusy 四态由 watch 复位且保持）
    expect(cancelBtn.getAttribute('aria-busy')).toBe('true')
    resolveChildrenA({ items: [child()], next_cursor: 'cursor-1' })
    await flush()
    // A 的过期 DTO 不得写入 B 详情：无 SUB_A 行、无加载更多、B 数据与 releaseTarget 复位保持
    expect(screen.queryByTestId(`subtask-row-${SUB_A}`)).toBeNull()
    expect(screen.queryByRole('button', { name: name('task.list.loadMore') })).toBeNull()
    expect(screen.getByTestId(`subtask-row-${SUB_B}`)).toBeTruthy()
    expect(screen.getByTestId('task-id').textContent).toBe(TASK_B)
    expect(screen.queryByRole('dialog')).toBeNull()
    resolveCancelB()
    await waitFor(() => expect(screen.getByRole('button', { name: name('task.cancel.action') }).getAttribute('aria-busy')).toBe('false'))
    wrapper.unmount()
  })

  it('discards a delayed same-taskId children snapshot that lands after a write-triggered refetch', async () => {
    // 正审 N2：同 taskId 内 C0 挂起 → time-review 成功发起 C1 GET 重取 → C1 先返回最新 queued → 旧 C0 迟到返回 held。
    // 旧 C0 的 gen 与 id 均未变、会通过 isStale，必须被 isChildrenStale（childrenGeneration 已随 C1 启动推进）拦截丢弃
    const flush = () => new Promise<void>((r) => setTimeout(r, 0))
    let childrenCalls = 0
    let resolveChildrenC0!: (v: { items: SubTaskItem[]; next_cursor: string | null }) => void
    const onLoadChildren = vi.fn((_id: string, _cursor: string | null) => {
      childrenCalls += 1
      if (childrenCalls === 1) {
        // C0：初次挂载的首个 children 读取，异步挂起（网络迟缓）
        return new Promise<{ items: SubTaskItem[]; next_cursor: string | null }>((r) => { resolveChildrenC0 = r })
      }
      // C1：time-review 成功回调发起的 GET 重取，立即返回写后最新 queued 记录
      return Promise.resolve({ items: [child({ state: 'queued', reason_code: null })], next_cursor: null })
    })
    const onTimeReview = vi.fn(async (_payload: { decision: 'resume_original_deadline' | 'expire'; evidence: 'reference_deadline_proven' | 'reference_deadline_expired' | 'inconclusive' }) => {
      return { ...heldTask, state: 'queued', revision: '8', counts: zeroTaskCounts() }
    })
    mount({
      taskId: TASK_ID,
      initialTask: { ...heldTask, view_scope: 'full', counts: { ...zeroTaskCounts(), held: '1' } },
      initialChildren: [],
      isAdmin: true, onTimeReview, onLoadChildren,
    })
    await fireEvent.click(screen.getByLabelText(name('task.timeReview.evidence.reference_deadline_proven')))
    await fireEvent.click(screen.getByRole('button', { name: name('task.held.reviewResumeOriginalDeadline') }))
    // C1 先返回：子任务行渲染为最新 queued
    await waitFor(() => expect(screen.getByTestId(`subtask-row-${SUB_A}`).textContent).toContain(i18n.t('task.state.queued')))
    // 旧 C0 迟到返回（held 旧快照 + 过期 cursor）：整体被同 taskId 读取代际丢弃
    resolveChildrenC0({ items: [child()], next_cursor: 'cursor-stale' })
    await flush()
    expect(onLoadChildren).toHaveBeenCalledTimes(2)
    expect(screen.getByTestId(`subtask-row-${SUB_A}`).textContent).toContain(i18n.t('task.state.queued'))
    // 过期 next_cursor 不得复活加载更多按钮
    expect(screen.queryByRole('button', { name: name('task.list.loadMore') })).toBeNull()
  })

  it('never lets a stale-cursor page reverse-eliminate the in-flight post-write first page', async () => {
    // 审查 Important 回归：释放锁成功启动 loadChildren(null) 后，若旧 nextCursor 仍可点，
    // 旧游标分页会推进 childrenGeneration，把权威 GET(null) 写后快照判为 stale 丢弃，
    // 旧列表追加旧页并保留已释放锁前的旧状态与释放入口。修复后首屏重刷立即清空旧游标，
    // 且任意 children 读取在途时加载更多禁用，反向淘汰路径不复存在
    const flush = () => new Promise<void>((r) => setTimeout(r, 0))
    let resolveRefetch!: (v: { items: SubTaskItem[]; next_cursor: string | null }) => void
    const onLoadChildren = vi.fn((_id: string, cursor: string | null) => {
      if (cursor === null) {
        // 写后 GET(null) 权威重取：挂起在途（网络迟缓）
        return new Promise<{ items: SubTaskItem[]; next_cursor: string | null }>((r) => { resolveRefetch = r })
      }
      // 旧游标分页若被发起即视为竞态复活：返回释放锁前的旧快照页（未释放、带释放入口）
      return Promise.resolve({ items: [child({ id: SUB_B, state: 'unknown', lock_released: false })], next_cursor: null })
    })
    const onReleaseLock = vi.fn(async () => undefined)
    mount({
      taskId: TASK_ID,
      initialTask: heldTask,
      initialChildren: [child({ state: 'unknown', lock_released: false })],
      initialNextCursor: 'cursor-old',
      isAdmin: true, onReleaseLock, onLoadChildren,
    })
    // 挂载时已有旧页与旧游标：加载更多可点
    expect(screen.getByRole('button', { name: name('task.list.loadMore') })).toBeTruthy()
    await fireEvent.click(screen.getByTestId(`release-lock-${SUB_A}`))
    await fireEvent.update(screen.getByLabelText(name('task.field.releaseReason')), '现场确认已重启')
    await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.acknowledgeUnknown') }))
    await fireEvent.click(screen.getByTestId('confirm-release'))
    await waitFor(() => expect(onLoadChildren).toHaveBeenCalledWith(TASK_ID, null))
    await flush()
    // 首屏重刷在途：旧游标已清空，加载更多不可点，无法用旧游标推进读取代际
    expect(screen.queryByRole('button', { name: name('task.list.loadMore') })).toBeNull()
    expect(onLoadChildren).toHaveBeenCalledTimes(1)
    // 权威写后快照返回：只呈现释放后的新状态，无释放前旧状态、旧页或多余游标残留
    resolveRefetch({ items: [child({ state: 'unknown', lock_released: true })], next_cursor: null })
    await waitFor(() => expect(screen.queryByTestId(`release-lock-${SUB_A}`)).toBeNull())
    expect(onLoadChildren).toHaveBeenCalledTimes(1)
    expect(screen.queryByTestId(`subtask-row-${SUB_B}`)).toBeNull()
    expect(screen.queryByRole('button', { name: name('task.list.loadMore') })).toBeNull()
  })
})

// 静态注明：GET /api/v1/tasks/{id}、GET /api/v1/tasks/{id}/children 与 POST /api/v1/tasks/{id}/time-review
// 在当前 Rust 树中均未注册路由，本用例只是 Vitest stub fetch 的错误态单测，
// 不构成"真实 404 证据"，也不代表任何真实端到端验收结论。
it('renders error state truthfully when the detail requests fail with 404', async () => {
  const notFound = () => new Response(JSON.stringify({
    error: { code: 'NOT_FOUND', message_key: 'errors.notFound' }, request_id: 'r-404',
  }), { status: 404, headers: { 'Content-Type': 'application/json' } })
  const fetcher = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => notFound())
  vi.stubGlobal('fetch', fetcher)
  mount({
    taskId: TASK_ID,
    onLoadTask: async () => (await get<TaskItem>(`tasks/${TASK_ID}`)).data,
    onLoadChildren: async () => (await get<TaskPage<SubTaskItem>>(`tasks/${TASK_ID}/children`)).data,
  })
  await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
  const paths = fetcher.mock.calls.map((call) => String(call[0]))
  expect(paths).toContain(`/api/v1/tasks/${TASK_ID}`)
  expect(paths).toContain(`/api/v1/tasks/${TASK_ID}/children`)
  // 两路读取都失败：绝不伪装成功，也不伪装为空详情
  expect(screen.queryByTestId('task-id')).toBeNull()
})

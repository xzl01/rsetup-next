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

// 组件 props + 事件监听器的显式类型：不用 Record<string, unknown> 绕过 vue-tsc 的 props 校验
type ReleaseLockPayload = {
  subtaskId: string
  expected_revision: string
  acknowledge_unknown: true
  reason: string
}
type ModalsProps = {
  mode: 'batch-risk' | 'release-lock'
  open: boolean
  subtask?: SubTaskItem | null
  busy?: boolean
  confirmDisabled?: boolean
  onClose?: () => void
  onConfirmBatchRisk?: () => void
  onConfirmReleaseLock?: (payload: ReleaseLockPayload) => void
}

function renderModals(props: ModalsProps) {
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

import { fireEvent, render, screen, waitFor } from '@testing-library/vue'
import { reactive } from 'vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import TasksView from './TasksView.vue'
import { post } from '../api'
import { createI18n } from '../i18n'
import type { DeviceItem } from '../types'
import { baseTaskTimeEvidence, zeroTaskCounts, type TaskItem, type TaskPreview } from '../task-ops'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const DEV_A = 'a'.repeat(64)
const DEV_B = 'b'.repeat(64)
const TASK_ID = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

const DEV_C = 'c'.repeat(64)

const devices: DeviceItem[] = [
  // Alpha：reboot-only 最小投影（不含 device.task.read/device.read），即 DevicesView 重启入口的真实设备形态
  { device_id: DEV_A, display_name: 'Alpha', effective_permissions: ['device.reboot'] },
  { device_id: DEV_B, display_name: 'Beta', effective_permissions: ['device.read'] }, // 既无 device.reboot 也无 device.task.read
  { device_id: DEV_C, display_name: 'Gamma', effective_permissions: ['device.task.read'] }, // task.read-only 只读预览对照
]

const preview: TaskPreview = {
  preview_token: 'tok-123', expires_in_ms: 60000,
  targets: [{ device_id: DEV_A, eligible: true }, { device_id: DEV_B, eligible: false, reason: 'PERMISSION_DENIED' }],
}

const task: TaskItem = {
  id: TASK_ID, operation: 'reboot', state: 'queued', outcome: null, revision: '7',
  created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(),
  view_scope: 'authorized_subset',
}

// 测试替身注入点：props 为组件契约的任意子集，render 的 ComponentProps 泛型在此按 never 放宽
function mount(props: Record<string, unknown>) {
  return render(TasksView, { props: props as never, global: { provide: { i18n } } })
}

function name(key: string): RegExp { return new RegExp(i18n.t(key), 'i') }

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals() })

describe('TasksView preview, idempotency and retry', () => {
  it('offers devices holding device.reboot or device.task.read and preselects the reboot-only pending device entry', async () => {
    const onPreview = vi.fn(async () => preview)
    mount({ devices, pendingDeviceId: DEV_A, onPreview })
    expect(screen.getByText('Alpha')).toBeTruthy()
    expect(screen.getByText('Gamma')).toBeTruthy()
    expect(screen.queryByText('Beta')).toBeNull()

    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(onPreview).toHaveBeenCalledWith('reboot', [DEV_A], expect.any(AbortSignal)))
  })

  it('never enters the target set for a device without device.reboot or device.task.read even when the pending entry points to it', async () => {
    const onPreview = vi.fn(async (_operation: 'reboot', _deviceIds: string[], _signal?: AbortSignal) => preview)
    mount({ devices, pendingDeviceId: DEV_B, onPreview })
    // Beta 既无 device.reboot 也无 device.task.read：不渲染，pendingDeviceId 指向它也不默认勾选
    expect(screen.queryByText('Beta')).toBeNull()
    expect(screen.queryByTestId(`task-device-${DEV_B}`)).toBeNull()
    // Alpha（reboot-only）与 Gamma（task.read-only）都在目标集合内
    expect(screen.getByTestId(`task-device-${DEV_A}`)).toBeTruthy()
    expect(screen.getByTestId(`task-device-${DEV_C}`)).toBeTruthy()
    // reboot-only 设备手动勾选后仍可生成预览，device_ids 只含 reboot 目标
    await fireEvent.click(screen.getByTestId(`task-device-${DEV_A}`))
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(onPreview).toHaveBeenCalledWith('reboot', [DEV_A], expect.any(AbortSignal)))
  })

  it('drops a revoked device from selected and never sends it in the preview payload after a devices update', async () => {
    const onPreview = vi.fn(async (_operation: 'reboot', _deviceIds: string[], _signal?: AbortSignal) => preview)
    const revoked = devices.map((d) => (d.device_id === DEV_A ? { ...d, effective_permissions: ['device.read'] } : d))
    const { rerender } = mount({ devices, pendingDeviceId: DEV_A, onPreview })
    // 初始：Alpha（reboot-only）经 pendingDeviceId 预选，Gamma 手动勾选
    await fireEvent.click(screen.getByTestId(`task-device-${DEV_C}`))
    // 外部更新：Alpha 权限被撤销为仅 device.read，移出任务目标集合
    await rerender({ devices: revoked })
    expect(screen.queryByTestId(`task-device-${DEV_A}`)).toBeNull()
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    // 精确等于 [DEV_C]：既证明合法保留设备送达，也证明已失格的 DEV_A 绝不在发送目标中
    await waitFor(() => expect(onPreview).toHaveBeenCalledWith('reboot', [DEV_C], expect.any(AbortSignal)))
    expect(onPreview).toHaveBeenCalledTimes(1)
  })

  it('never calls onPreview when a devices update revokes every currently selected device', async () => {
    const onPreview = vi.fn(async (_operation: 'reboot', _deviceIds: string[], _signal?: AbortSignal) => preview)
    const revoked = devices.map((d) => (d.device_id === DEV_A ? { ...d, effective_permissions: ['device.read'] } : d))
    const { rerender } = mount({ devices, pendingDeviceId: DEV_A, onPreview })
    await rerender({ devices: revoked })
    // selected 被 watch 清理后按钮 disabled，loadPreview 交集亦为空：onPreview 绝不被调用
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    expect(onPreview).not.toHaveBeenCalled()
  })

  it('drops an in-place revoked or spliced device from selected without a new array reference', async () => {
    const onPreview = vi.fn(async (_operation: 'reboot', _deviceIds: string[], _signal?: AbortSignal) => preview)
    // 父级持有同一 reactive 数组：原地改 effective_permissions / splice 都不换引用，浅层 watch 不会触发
    const live = reactive([
      { device_id: DEV_A, display_name: 'Alpha', effective_permissions: ['device.reboot'] },
      { device_id: DEV_C, display_name: 'Gamma', effective_permissions: ['device.task.read'] },
    ])
    mount({ devices: live, pendingDeviceId: DEV_A, onPreview })
    await fireEvent.click(screen.getByTestId(`task-device-${DEV_C}`))
    // 原地撤销 Alpha 的 device.reboot：数组引用不变、无 rerender，selected 仍必须立即剔除 DEV_A
    live[0]!.effective_permissions = ['device.read']
    await waitFor(() => expect(screen.queryByTestId(`task-device-${DEV_A}`)).toBeNull())
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    // 精确等于 [DEV_C]：已失格的 DEV_A 绝不残留在发送目标中
    await waitFor(() => expect(onPreview).toHaveBeenCalledWith('reboot', [DEV_C], expect.any(AbortSignal)))
    expect(onPreview).toHaveBeenCalledTimes(1)
    // 原地 splice 移除 Gamma：selected 清空，按钮回到 disabled
    live.splice(1, 1)
    await waitFor(() => expect((screen.getByRole('button', {
      name: name('task.preview.submit'),
    }) as HTMLButtonElement).disabled).toBe(true))
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    expect(onPreview).toHaveBeenCalledTimes(1)
  })

  it('preselects the pending device when the device list arrives asynchronously after mount', async () => {
    const onPreview = vi.fn(async (_operation: 'reboot', _deviceIds: string[], _signal?: AbortSignal) => preview)
    const submitButton = () => screen.getByRole('button', { name: name('task.preview.submit') }) as HTMLButtonElement
    // 初次渲染 devices=[]：setup 期无法匹配 pendingDeviceId，按钮 disabled
    const { rerender } = mount({ devices: [], pendingDeviceId: DEV_A, onPreview })
    expect(submitButton().disabled).toBe(true)
    // 设备异步到达：pendingDeviceId 命中有效集合必须补做默认预选，无需用户再手动勾选
    await rerender({ devices })
    await waitFor(() => expect(screen.getByTestId(`task-device-${DEV_A}`)).toBeTruthy())
    expect((screen.getByTestId(`task-device-${DEV_A}`) as HTMLInputElement).checked).toBe(true)
    expect(submitButton().disabled).toBe(false)
    await fireEvent.click(submitButton())
    await waitFor(() => expect(onPreview).toHaveBeenCalledWith('reboot', [DEV_A], expect.any(AbortSignal)))
  })

  it('does not force the pending device back on after the user manually unchecks it', async () => {
    const onPreview = vi.fn(async (_operation: 'reboot', _deviceIds: string[], _signal?: AbortSignal) => preview)
    const { rerender } = mount({ devices, pendingDeviceId: DEV_A, onPreview })
    // 用户主动取消 pendingDeviceId 的勾选 = 默认预选已消费
    await fireEvent.click(screen.getByTestId(`task-device-${DEV_A}`))
    expect((screen.getByTestId(`task-device-${DEV_A}`) as HTMLInputElement).checked).toBe(false)
    // devices 更新（新引用、同内容）：watcher 不得把用户刚取消的设备强行勾回
    await rerender({ devices: [...devices] })
    expect((screen.getByTestId(`task-device-${DEV_A}`) as HTMLInputElement).checked).toBe(false)
  })

  it('shows the frozen preview list with per-target eligibility and reason, and no hidden counts', async () => {
    mount({ devices, pendingDeviceId: DEV_A, onPreview: vi.fn(async () => preview) })
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.getByText(DEV_A)).toBeTruthy())
    expect(screen.getByText(i18n.t('task.preview.eligible'))).toBeTruthy()
    expect(screen.getByText(i18n.t('task.preview.ineligible'))).toBeTruthy()
    expect(screen.getByText(i18n.t('task.reason.PERMISSION_DENIED'))).toBeTruthy()
    expect(document.body.textContent).not.toMatch(/隐藏|hidden/i)
  })

  it('enables the modal submit once risk is checked, surfaces rejected submit as error state and retries with the same key', async () => {
    const keys: string[] = []
    let previewToken = 'tok-123'
    let settle: (ok: boolean) => void = () => {}
    const onPreview = vi.fn(async () => ({ ...preview, preview_token: previewToken }))
    const onSubmit = vi.fn((_payload: { preview_token: string; confirm: true }, options: { idempotencyKey: string }) => {
      keys.push(options.idempotencyKey)
      return new Promise<{ task_id: string; accepted: boolean }>((resolve, reject) => {
        settle = (ok) => { if (ok) resolve({ task_id: TASK_ID, accepted: true }); else reject(new Error('HTTP_500')) }
      })
    })
    mount({ devices, pendingDeviceId: DEV_A, onPreview, onSubmit })
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.getByRole('button', { name: name('task.submitReboot') })).toBeTruthy())

    await fireEvent.click(screen.getByRole('button', { name: name('task.submitReboot') }))
    // 模态刚打开时 operation.key 为 null，确认按钮必须仅凭 risk checkbox 勾选即可启用（confirmDisabled 不能取决于未生成的 key）
    const confirm = screen.getByTestId('confirm-batch-risk') as HTMLButtonElement
    expect(confirm.disabled).toBe(true)
    const ack = screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') })
    await fireEvent.click(ack)
    expect((screen.getByTestId('confirm-batch-risk') as HTMLButtonElement).disabled).toBe(false)

    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1))
    expect(keys[0]).toMatch(UUID_V4)
    // 在途：按钮必须禁用（防双击）
    expect((screen.getByTestId('confirm-batch-risk') as HTMLButtonElement).disabled).toBe(true)

    // 第一次提交 reject（模拟网络 500）：必须呈现可见错误态（非静默 return），模态保持打开、同 key 保留
    settle(false)
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    expect((screen.getByTestId('confirm-batch-risk') as HTMLButtonElement).disabled).toBe(false)
    // 同 key 重试（账本契约：同 actor+key 同内容命中幂等重放返回原任务，而非 409 IDEMPOTENCY_CONFLICT）
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(2))
    expect(keys[1]).toBe(keys[0])
    settle(true)
    await waitFor(() => expect(screen.queryByTestId('confirm-batch-risk')).toBeNull())
    expect(screen.queryByRole('alert')).toBeNull()

    // 交错：提交在途时重新生成 token 变化的预览（代际递增、旧提交失效），再让旧提交迟到返回
    await fireEvent.click(screen.getByRole('button', { name: name('task.submitReboot') }))
    await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') }))
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(3))
    const staleKey = keys[2]!
    previewToken = 'tok-999'
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(onPreview).toHaveBeenCalledTimes(2))
    settle(true) // 旧提交迟到成功：不得关闭新代模态、不得复活旧 key
    // 旧提交 finally 不得复活旧 key：下一次提交必须生成新 key
    await fireEvent.click(screen.getByRole('button', { name: name('task.submitReboot') }))
    await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') }))
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(4))
    expect(keys[3]).toMatch(UUID_V4)
    expect(keys[3]).not.toBe(staleKey)
  })

  it('a same-fingerprint preview started while a submit is in flight takes over busy and keeps the idempotency key', async () => {
    const keys: string[] = []
    let settle: (ok: boolean) => void = () => {}
    const onPreview = vi.fn(async () => preview) // token 与清单均不变：fingerprint 相同
    const onSubmit = vi.fn((_payload: { preview_token: string; confirm: true }, options: { idempotencyKey: string }) => {
      keys.push(options.idempotencyKey)
      return new Promise<{ task_id: string; accepted: boolean }>((resolve, reject) => {
        settle = (ok) => { if (ok) resolve({ task_id: TASK_ID, accepted: true }); else reject(new Error('HTTP_500')) }
      })
    })
    mount({ devices, pendingDeviceId: DEV_A, onPreview, onSubmit })
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.getByRole('button', { name: name('task.submitReboot') })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: name('task.submitReboot') }))
    await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') }))
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1))
    const firstKey = keys[0]!
    expect((screen.getByTestId('confirm-batch-risk') as HTMLButtonElement).disabled).toBe(true)

    // 提交在途时发起同 fingerprint 预览：新代必须接管 busy，确认按钮重新可用
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(onPreview).toHaveBeenCalledTimes(2))
    await waitFor(() => expect((screen.getByTestId('confirm-batch-risk') as HTMLButtonElement).disabled).toBe(false))

    // 旧提交迟到成功：不得关闭新代模态、不得改变 key——下一次提交必须复用同 key
    settle(true)
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(2))
    expect(screen.getByTestId('confirm-batch-risk')).toBeTruthy()
    expect(keys[1]).toBe(firstKey)
    settle(true)
    await waitFor(() => expect(screen.queryByTestId('confirm-batch-risk')).toBeNull())
  })

  it('a failed same-fingerprint preview takes over the in-flight submit without locking retry, and the late rejection cannot pollute the new generation', async () => {
    const keys: string[] = []
    let previewFails = false
    let settle: (ok: boolean) => void = () => {}
    const onPreview = vi.fn(async () => {
      if (previewFails) throw new Error('HTTP_500')
      return preview
    })
    const onSubmit = vi.fn((_payload: { preview_token: string; confirm: true }, options: { idempotencyKey: string }) => {
      keys.push(options.idempotencyKey)
      return new Promise<{ task_id: string; accepted: boolean }>((resolve, reject) => {
        settle = (ok) => { if (ok) resolve({ task_id: TASK_ID, accepted: true }); else reject(new Error('HTTP_500')) }
      })
    })
    mount({ devices, pendingDeviceId: DEV_A, onPreview, onSubmit })
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.getByRole('button', { name: name('task.submitReboot') })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: name('task.submitReboot') }))
    await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') }))
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1))
    const firstKey = keys[0]!

    // 提交在途时同 fingerprint 预览失败：呈现可见错误态，但 busy 已由新 preview 代际接管
    previewFails = true
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())

    // 再次预览成功：预览恢复且确认按钮必须重新可用（旧提交 finally 被代际拦截，不得遗留 busy）
    previewFails = false
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(onPreview).toHaveBeenCalledTimes(3))
    await waitFor(() => expect((screen.getByTestId('confirm-batch-risk') as HTMLButtonElement).disabled).toBe(false))

    // 旧提交迟到 reject：被代际检查拦截，不得浮现错误态、不得锁死——提交仍复用同 key
    settle(false)
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(2))
    expect(keys[1]).toBe(firstKey)
    expect(screen.queryByRole('alert')).toBeNull()
    settle(true)
    await waitFor(() => expect(screen.queryByTestId('confirm-batch-risk')).toBeNull())
  })

  it('resets confirmation and idempotency key when the preview token changes', async () => {
    const onSubmit = vi.fn(async (
      _payload: { preview_token: string; confirm: true },
      _options: { idempotencyKey: string; signal?: AbortSignal },
    ) => ({ task_id: TASK_ID, accepted: true }))
    const second: TaskPreview = { preview_token: 'tok-456', expires_in_ms: 60000, targets: [{ device_id: DEV_A, eligible: true }] }
    const onPreview = vi.fn()
      .mockResolvedValueOnce(preview)
      .mockResolvedValueOnce(second)
    mount({ devices, pendingDeviceId: DEV_A, onPreview, onSubmit })

    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.getByRole('button', { name: name('task.submitReboot') })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: name('task.submitReboot') }))
    await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') }))
    const confirm = screen.getByTestId('confirm-batch-risk') as HTMLButtonElement
    expect(confirm.disabled).toBe(false)
    await fireEvent.click(confirm)
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1))
    const firstKey = onSubmit.mock.calls[0]![1].idempotencyKey as string

    // 重新生成预览（token 变化）：确认与幂等键必须重置
    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.queryByTestId('confirm-batch-risk')).toBeNull())
    await fireEvent.click(screen.getByRole('button', { name: name('task.submitReboot') }))
    const ack = screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') })
    expect((ack as HTMLInputElement).checked).toBe(false)
    await fireEvent.click(ack)
    await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(2))
    expect(onSubmit.mock.calls[1]![1].idempotencyKey as string).not.toBe(firstKey)
  })

  it('resets the idempotency key when only the target list changes and when only the token changes', async () => {
    const onSubmit = vi.fn(async (
      _payload: { preview_token: string; confirm: true },
      _options: { idempotencyKey: string; signal?: AbortSignal },
    ) => ({ task_id: TASK_ID, accepted: true }))
    // 三段预览：tok-123·[A✓,B✗] → tok-123·[A✓]（同 token、清单变化）→ tok-456·[A✓]（同清单、token 变化）
    const sameTokenNewList: TaskPreview = { preview_token: 'tok-123', expires_in_ms: 60000, targets: [{ device_id: DEV_A, eligible: true }] }
    const newTokenSameList: TaskPreview = { preview_token: 'tok-456', expires_in_ms: 60000, targets: [{ device_id: DEV_A, eligible: true }] }
    const onPreview = vi.fn()
      .mockResolvedValueOnce(preview)
      .mockResolvedValueOnce(sameTokenNewList)
      .mockResolvedValueOnce(newTokenSameList)
    mount({ devices, pendingDeviceId: DEV_A, onPreview, onSubmit })
    const submitButton = () => screen.getByRole('button', { name: name('task.preview.submit') })
    const rebootButton = () => screen.getByRole('button', { name: name('task.submitReboot') })
    async function confirmOnce(expectedCalls: number): Promise<string> {
      await fireEvent.click(rebootButton())
      await fireEvent.click(screen.getByRole('checkbox', { name: name('task.risk.confirmBatchReboot') }))
      await fireEvent.click(screen.getByTestId('confirm-batch-risk'))
      await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(expectedCalls))
      return onSubmit.mock.calls[expectedCalls - 1]![1].idempotencyKey as string
    }

    await fireEvent.click(submitButton())
    await waitFor(() => expect(rebootButton()).toBeTruthy())
    const key1 = await confirmOnce(1)

    // 同 token、清单变化：指纹变化，旧幂等键必须作废
    await fireEvent.click(submitButton())
    await waitFor(() => expect(onPreview).toHaveBeenCalledTimes(2))
    await waitFor(() => expect(screen.queryByTestId('confirm-batch-risk')).toBeNull())
    const key2 = await confirmOnce(2)
    expect(key2).not.toBe(key1)

    // 同清单、token 变化：同样重置，三次提交三个不同的 key
    await fireEvent.click(submitButton())
    await waitFor(() => expect(onPreview).toHaveBeenCalledTimes(3))
    await waitFor(() => expect(screen.queryByTestId('confirm-batch-risk')).toBeNull())
    const key3 = await confirmOnce(3)
    expect(key3).not.toBe(key2)
    expect(key3).not.toBe(key1)
  })

  it('fails closed without crypto.getRandomValues: preview stays available, risky submit disabled with visible notice', async () => {
    // 桩入缺 getRandomValues（即便带 randomUUID）的 crypto：证明门禁不经 randomUUID fallback 放行；afterEach 既有 vi.unstubAllGlobals 负责复原
    vi.stubGlobal('crypto', { randomUUID: () => '11111111-1111-4111-8111-111111111111' })
    const onPreview = vi.fn(async () => preview)
    const onSubmit = vi.fn(async () => ({ task_id: TASK_ID, accepted: true }))
    mount({ devices, pendingDeviceId: DEV_A, onPreview, onSubmit })

    await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
    await waitFor(() => expect(screen.getByText(i18n.t('task.preview.eligible'))).toBeTruthy())
    expect(onPreview).toHaveBeenCalledTimes(1)

    expect(screen.getByText(i18n.t('task.security.csprngUnavailable'))).toBeTruthy()
    const submit = screen.getByRole('button', { name: name('task.submitReboot') }) as HTMLButtonElement
    expect(submit.disabled).toBe(true)
    await fireEvent.click(submit)
    expect(screen.queryByRole('checkbox', { name: name('task.risk.confirmBatchReboot') })).toBeNull()
    expect(onSubmit).not.toHaveBeenCalled()
  })
})

describe('TasksView list, onMounted initial loading and error states', () => {
  it('triggers initial load onMounted and handles error state truthfully', async () => {
    const onLoadTasks = vi.fn(async () => { throw new Error('NETWORK_DOWN') })
    mount({ devices, onLoadTasks })
    await waitFor(() => expect(onLoadTasks).toHaveBeenCalledWith(null))
    // Vue 的渲染刷新排在 rejection 微任务之后，错误态必须等一次 flush 才落到 DOM
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    expect(screen.queryByText(i18n.t('task.list.empty'))).toBeNull()
  })

  it('renders the four main-state labels, authorized subset notice and cursor pagination with entity id', async () => {
    const onLoadTasks = vi.fn(async () => ({ items: [task], next_cursor: null }))
    mount({
      devices, initialTasks: [task], initialNextCursor: 'cursor-1', onLoadTasks,
    })
    expect(screen.getByText(TASK_ID)).toBeTruthy()
    expect(screen.getByText(i18n.t('task.mainState.queued'))).toBeTruthy()
    expect(screen.getByText(i18n.t('task.scope.authorizedSubset'))).toBeTruthy()
    await fireEvent.click(screen.getByRole('button', { name: name('task.list.loadMore') }))
    expect(onLoadTasks).toHaveBeenCalledWith('cursor-1')
  })

  it('marks an unknown view scope instead of claiming completeness', () => {
    mount({ devices, initialTasks: [{ ...task, view_scope: undefined }] })
    expect(screen.getByText(i18n.t('task.scope.unknown'))).toBeTruthy()
    expect(screen.queryByText(i18n.t('task.scope.authorizedSubset'))).toBeNull()
  })

  it('renders empty list only when loaded tasks are genuinely empty', () => {
    mount({ devices: [], initialTasks: [] })
    expect(screen.getByText(i18n.t('task.list.empty'))).toBeTruthy()
  })

  it('recovers from a pagination error and reveals the table upon a successful retry', async () => {
    const nextTask = { ...task, id: '9d2b6f4e-1c7a-4b3d-8e5f-6a0c2d4e8b11' }
    let failed = false
    const onLoadTasks = vi.fn(async () => {
      if (!failed) { failed = true; throw new Error('HTTP_500') }
      return { items: [nextTask], next_cursor: null }
    })
    mount({ devices, initialTasks: [task], initialNextCursor: 'c1', onLoadTasks })
    await fireEvent.click(screen.getByRole('button', { name: name('task.list.loadMore') }))
    await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
    expect(screen.queryByText(TASK_ID)).toBeNull()
    await fireEvent.click(screen.getByRole('button', { name: name('task.list.loadMore') }))
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull())
    expect(screen.getByText(TASK_ID)).toBeTruthy()
    expect(screen.getByText(nextTask.id)).toBeTruthy()
  })

  it('prevents concurrent duplicate pagination and duplicate keys on rapid clicks', async () => {
    const nextTask = { ...task, id: '7c9e6679-7425-40de-944b-e07fc1f90ae9' }
    let resolvePage!: (value: unknown) => void
    const onLoadTasks = vi.fn(() => new Promise((resolve) => { resolvePage = resolve }))
    mount({ devices, initialTasks: [task], initialNextCursor: 'c1', onLoadTasks })
    const button = screen.getByRole('button', { name: name('task.list.loadMore') })
    await fireEvent.click(button)
    await fireEvent.click(button)
    expect(onLoadTasks).toHaveBeenCalledTimes(1)
    expect(button.hasAttribute('disabled')).toBe(true)
    resolvePage({ items: [nextTask, nextTask], next_cursor: null })
    // WEB-R7-1：next_cursor 为 null 后按钮被移除，是响应已完整处理的明确完成信号
    await waitFor(() => expect(screen.queryByRole('button', { name: name('task.list.loadMore') })).toBeNull())
    expect(screen.getAllByText(TASK_ID)).toHaveLength(1)
    expect(screen.getAllByText(nextTask.id)).toHaveLength(1)
  })
})

// 静态注明：后端尚未注册任务路由，本用例只是 Vitest stub fetch 的错误态单测，
// 不构成"真实 404 证据"，也不代表任何真实端到端验收结论。
it('renders error state truthfully when request fails with 404', async () => {
  const notFound = () => new Response(JSON.stringify({
    error: { code: 'NOT_FOUND', message_key: 'errors.notFound' }, request_id: 'r-404',
  }), { status: 404, headers: { 'Content-Type': 'application/json' } })
  const fetcher = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit) => notFound())
  vi.stubGlobal('fetch', fetcher)
  const onPreview = async (operation: 'reboot', deviceIds: string[]) => {
    const res = await post<{ preview_token: string }>('task-previews', { operation, device_ids: deviceIds }, { csrfToken: 'csrf-1' })
    return res.data as never
  }
  mount({ devices, pendingDeviceId: DEV_A, onPreview })
  await fireEvent.click(screen.getByRole('button', { name: name('task.preview.submit') }))
  await waitFor(() => expect(fetcher).toHaveBeenCalledOnce())
  const url = String(fetcher.mock.calls[0]![0])
  expect(url).toBe('/api/v1/task-previews')
  await waitFor(() => expect(screen.getByRole('alert')).toBeTruthy())
  expect(screen.queryByText(i18n.t('task.preview.eligible'))).toBeNull()
})

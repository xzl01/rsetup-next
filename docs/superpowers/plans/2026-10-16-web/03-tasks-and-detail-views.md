### Task 8: `TasksView.vue` —— 列表、首屏加载错误态、预览固定清单、幂等确认/重试/重置与提交编排

**Files:**
- Create: `apps/controller-web/src/views/TasksView.vue`
- Create: `apps/controller-web/src/views/TasksView.test.ts`

**Interfaces:**
- Consumes: `task-presentation.ts`（`startTaskOperation`、`resetTaskOperation`、`previewFingerprint`、`mainTaskStateLabel`）、`task-ops.ts`（`TaskPreview`、`TaskItem`、`assertTaskPreviewShape`、`assertTaskPageShape`、`describeAuthorizedSubset`、`eligibleDeviceIds`）、`components/TaskModals.vue`、`api.ts`（`get`、`post`）。（精简掉无数据流的 `TaskStateList`，子任务展示与释放锁统一切入 `TaskDetailView`）。
- Produces（父组件/测试注入的契约；`on*` 全部为可选回调，测试注入替身，`App.vue` 注入真实实现）：
  ```ts
  // props
  defineProps<{
    devices?: DeviceItem[]                // 设备列表投影，含 effective_permissions
    initialTasks?: TaskItem[]
    initialNextCursor?: string | null
    pendingDeviceId?: string | null       // 来自 #/tasks?device=<hex64>
    csrfToken?: string
    onPreview?: (operation: 'reboot', deviceIds: string[], signal?: AbortSignal) => Promise<TaskPreview>
    onSubmit?: (payload: { preview_token: string; confirm: true }, options: { idempotencyKey: string; signal?: AbortSignal }) => Promise<{ task_id: string; accepted: boolean }>
    onCancel?: (payload: { taskId: string; expectedRevision: string; confirm: true }) => Promise<void>
    onLoadTasks?: (cursor: string | null) => Promise<{ items: TaskItem[]; next_cursor: string | null }>
  }>()
  // emits
  defineEmits<{
    openTask: [taskId: string]        // App.vue 据此 navigate({ name: 'task-detail', params: { id } })
    rebootDevice: [deviceId: string]  // 保留设备入口语义，由 App.vue 转为 tasks 路由
  }>()
  ```
- 行为契约：
  1. 初次 `onMounted` 挂载：若未提供 `initialTasks` 且存在 `props.onLoadTasks`，主动调用 `onLoadTasks(null)` 进行首屏加载；若加载失败置 `listError` 呈现错误态，**绝不伪装为空列表**。
  2. 设备勾选区只列出 `effective_permissions` 含 `device.reboot` 或 `device.task.read` 的 `devices`（统一谓词 `taskDevices`；01 §4:42–45：`device.reboot` 不隐含其他权限，任一有效设备权限即可获得最低识别列表；两者皆无的设备不渲染、不进入 `device_ids`）；`pendingDeviceId` 命中该集合时默认勾选。真实 `DevicesView` 重启入口只检查 `device.reboot`，因此 reboot-only 设备必须能进入勾选、预览与提交（服务端逐端点重验为最终准绳），本组件不得把 `device.task.read` 当作重启前置，也不得为 reboot-only 设备伪造任务详情、状态或执行证据的可读性；task.read-only 设备保留只读预览语义。`props.devices` 更新（权限撤销或设备移出列表）后必须同步清理 `selected` 中已不在当前 `taskDevices` 的残留 ID，`loadPreview` 发起前再与 `taskDevices` 取一次交集，已失格设备绝不进入 `device_ids`（WEB-N3）。
  3. 点击"生成预览"（`task.preview.submit`）调用 `onPreview('reboot', targetIds)`（`targetIds` 为 `selected` 与当前 `taskDevices` 的交集，已失格设备不发送；交集为空则不发起请求）；返回后 `assertTaskPreviewShape` 校验；预览区显示固定清单：每个 target 显示 device_id、`task.preview.eligible`/`task.preview.ineligible`，不可用目标且 `reason` 非 null 时显示 `task.reason.<SCREAMING_SNAKE_CASE>`（`reason` 为 null 时不渲染原因）。清单里**没有**隐藏数量。
  4. 批量提交走 `TaskModals mode="batch-risk"`：模态框内确认按钮状态由 `batchDisabled` 控制；父组件传给模态框的 `confirm-disabled` 必须是 `!preview || previewBusy || !csprngAvailable`（**不能取决于尚未首次生成的 operation.key，否则弹框打开时永远被禁用**；`csprngAvailable` 为 `<script setup>` 内 `typeof globalThis.crypto?.getRandomValues === 'function'` 的安全能力探测，**fail-closed：无 `crypto.getRandomValues` 时绝不 fallback 到 `randomUUID`/`Math.random`**）；`csprngAvailable` 为 false 时打开批量重启的按钮必须 `:disabled="!csprngAvailable"`，页面渲染可见 `task.security.csprngUnavailable` AppNotice 明确告知浏览器安全随机数不可用、危险提交已禁用，`submitBatch` 入口再次 guard 防直接事件绕过，只读浏览与 preview 不受影响；点击模态框内提交触发 `submitBatch` 时调用 `startTaskOperation(state)` 首次生成/重试复用 UUIDv4 key，并以 `onSubmit({ preview_token, confirm: true }, { idempotencyKey: key })` 提交；在途时 `busy === true` 禁用防双击。
  5. preview 与 submit 共用同一"操作代际"（参照 Task 9 `isStale` 风格：`operationGeneration` + `operationKind`，每次操作开始捕获 `(gen, kind)`，所有 await 返回后、catch 分支与 finally 回写前严格校验仍与捕获值一致；不一致即旧操作，结果一律丢弃且不触碰任何状态）。任一 preview/submit 新操作开始即递增代际、`AbortController.abort()` 在途请求，并把 `controller.signal` 经 callback 可选 `signal` 参数传入请求（Task 10 W1 适配器层注入 `TaskRequestContext.signal`）；即使 callback 忽略 signal，迟到响应也必须被代际检查显式丢弃，abort 后不得处理旧响应。`preview_token` 或固定清单变化时立即：`resetTaskOperation()`、清空确认勾选、清除预览错误（旧操作 `finally` 不得复活/清除新 preview 的 `operation.key` 与 `inFlight`）。任一 preview 开始（代际递增后、发起请求前）必须立即结束被替代 submit 的 inFlight：`operation.inFlight` 为 true 时置 `{ key: operation.value.key, inFlight: false }`（保留原 key），其后仅当 fingerprint 变化才 `resetTaskOperation()` 清除 key；同 fingerprint 预览（含预览失败）不得把提交锁死，下一次提交仍复用原 key。提交失败保持同 key 可重试（key 与内容绑定不变，重试复用同一 key——对应账本"相同 actor、相同 key、相同内容命中幂等重放返回原任务，不同内容 409 `IDEMPOTENCY_CONFLICT`"契约，02-data-api 幂等段约 L153–159）；提交 reject 必须在组件层 catch 设置可见错误态（复用预览区 `state.error` AppNotice alert，非静默 return、不吞错），错误态不影响后续重试。
  6. 任务列表显示 `item.id`（MainTask 实体键）、`state`（`mainTaskStateLabel`，主任务四态专用文案，不用子任务十态 `taskStateLabel`）、`revision`、`created_time`（呈现只消费 `item.created_time.reference_utc` 与 `item.created_time.quality` 原始值，绝不把对象整体渲染进模板；不新增 i18n 键与 testid；列表表格维持现有列，不补 `created_time` 列——既有列缺口，本任务不扩列），以及 `describeAuthorizedSubset(task.view_scope)` 的文案；`next_cursor` 存在时渲染 `task.list.loadMore` 按钮并调用 `onLoadTasks(cursor)` 追加；若追加失败呈现错误态，`task.list.loadMore` 按钮保持在表格外可继续点击，追加成功时必须清除 `listError` 恢复表格渲染（WEB-R5-2）；loadMore 采用 loadMoreBusy 防重入门控与按 id 去重，按钮在加载中处于 disabled 状态，防止并发连击产生重复项或旧错误覆盖新成功（WEB-R6-1）。
  7. 错误态显示 `state.error`，绝不伪装空列表；空列表显示 `task.list.empty`。
  8. 所有文案经 `i18n.t()`，无硬编码中文/英文。

- [ ] **Step 1: 建立可编译桩代码**

```vue
<script setup lang="ts">
import type { DeviceItem } from '../types'
import type { TaskItem, TaskPreview } from '../task-ops'

defineProps<{
  devices?: DeviceItem[]
  initialTasks?: TaskItem[]
  initialNextCursor?: string | null
  pendingDeviceId?: string | null
  csrfToken?: string
  onPreview?: (operation: 'reboot', deviceIds: string[], signal?: AbortSignal) => Promise<TaskPreview>
  onSubmit?: (payload: { preview_token: string; confirm: true }, options: { idempotencyKey: string; signal?: AbortSignal }) => Promise<{ task_id: string; accepted: boolean }>
  onCancel?: (payload: { taskId: string; expectedRevision: string; confirm: true }) => Promise<void>
  onLoadTasks?: (cursor: string | null) => Promise<{ items: TaskItem[]; next_cursor: string | null }>
}>()

defineEmits<{ openTask: [taskId: string]; rebootDevice: [deviceId: string] }>()
</script>

<template>
  <div class="tasks-view-stub"></div>
</template>
```

- [ ] **Step 2: 编写失败测试**

`apps/controller-web/src/views/TasksView.test.ts`：

```ts
import { fireEvent, render, screen, waitFor } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import TasksView from './TasksView.vue'
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

function mount(props: Record<string, unknown>) {
  return render(TasksView, { props, global: { provide: { i18n } } })
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
    const onSubmit = vi.fn((_payload, options: { idempotencyKey: string }) => {
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
    const onSubmit = vi.fn((_payload, options: { idempotencyKey: string }) => {
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
    const onSubmit = vi.fn((_payload, options: { idempotencyKey: string }) => {
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
    expect(screen.getByRole('alert')).toBeTruthy()
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
```

- [ ] **Step 3: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/views/TasksView.test.ts`

预期：文件可编译加载，运行 12 个用例（原 10 + WEB-R5-2 分页错误恢复回归 1 + WEB-R6-1 连击防重入回归 1，不虚报），绝大多数失败：`TestingLibraryElementError: Unable to find an accessible element with the role "button" and name /生成预览/i`、`Unable to find an element by: [data-testid="confirm-batch-risk"]`。
（有效行为 RED：文件可编译、运行数非 0、因断言失败而红。）

- [ ] **Step 4: 最小实现**

实现要点（按此结构写 `TasksView.vue`，命名必须与测试和 Interfaces 完全一致；模板使用已在 Task 2 定义的键）：

```vue
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

// 任务目标集合 = 持有 device.reboot 或 device.task.read 的设备（01 §4:42–45：reboot 不隐含 read，read 不隐含 reboot；两者皆无不进入）
const taskDevices = computed(() => props.devices.filter((d) => d.effective_permissions.includes('device.reboot') || d.effective_permissions.includes('device.task.read')))
const selected = ref<string[]>([])
const preview = ref<TaskPreview | null>(null)
const previewError = ref<string | null>(null)
const previewBusy = ref(false)
const batchModalOpen = ref(false)
const operation = ref(resetTaskOperation())
const tasks = ref<TaskItem[]>(props.initialTasks ?? [])
const nextCursor = ref<string | null>(props.initialNextCursor)
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

// WEB-N3：props.devices 更新（权限撤销或移出列表）后 taskDevices 不再列出已失格设备，
// 但 selected 仍会残留其 ID；立即取交集清理，防止残留 ID 被后续操作提交
watch(() => props.devices, () => {
  selected.value = selected.value.filter((id) => taskDevices.value.some((d) => d.device_id === id))
})

async function loadPreview() {
  if (!props.onPreview) return
  // WEB-N3 双重纵深防御：发起前再与当前 taskDevices 取交集，已失格设备绝不进入 device_ids
  const targetIds = selected.value.filter((id) => taskDevices.value.some((d) => d.device_id === id))
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
    if (isStaleOp(gen, 'preview')) return   // 迟到响应：丢弃，不覆盖新状态
    assertTaskPreviewShape(result)
    preview.value = result
    const nextFingerprint = previewFingerprint(result)
    if (nextFingerprint !== fingerprint) {
      fingerprint = nextFingerprint
      operation.value = resetTaskOperation()
      batchModalOpen.value = false
    }
  } catch {
    if (isStaleOp(gen, 'preview')) return
    preview.value = null
    previewError.value = t('state.error')
  } finally {
    if (!isStaleOp(gen, 'preview')) previewBusy.value = false   // 旧代 finally 不得清除新代 busy
  }
}

// CSPRNG 能力门禁：crypto.getRandomValues 不可用即 fail-closed，绝不 fallback 到 randomUUID/Math.random
const csprngAvailable = typeof globalThis.crypto?.getRandomValues === 'function'

async function submitBatch() {
  if (!preview.value || !props.onSubmit || !csprngAvailable) return
  let next: { key: string; inFlight: boolean }
  try {
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
    await props.onSubmit({ preview_token: preview.value.preview_token, confirm: true }, { idempotencyKey: next.key, signal: controller.signal })
    if (isStaleOp(gen, 'submit')) return   // 迟到成功：不关闭新代模态、不清新代错误
    batchModalOpen.value = false
    previewError.value = null
  } catch {
    // 提交失败：置可见错误态（复用预览区 AppNotice alert，非静默 return）并保持同 key 可重试——
    // key 与内容绑定不变，重试复用同 key，对应账本"同 actor+key 同内容命中幂等重放返回原任务、
    // 不同内容 409 IDEMPOTENCY_CONFLICT"契约（02-data-api 幂等段约 L153–159）
    if (!isStaleOp(gen, 'submit')) previewError.value = t('state.error')
  } finally {
    if (!isStaleOp(gen, 'submit')) operation.value = { key: next.key, inFlight: false }   // 旧代 finally 不得复活/清除新代 key 与 inFlight
  }
}

async function loadMore() {
  if (!props.onLoadTasks || !nextCursor.value || loadMoreBusy.value) return
  loadMoreBusy.value = true // WEB-R6-1：同一 nextCursor 连击不得并发发起两次请求
  try {
    const page = await props.onLoadTasks(nextCursor.value)
    assertTaskPageShape(page)
    const seenIds = new Set(tasks.value.map((t) => t.id)) // WEB-R7-1：增量动态集合，兼防同一页内重复的新 ID
    const newItems = page.items.filter((t) => {
      if (seenIds.has(t.id)) return false
      seenIds.add(t.id)
      return true
    })
    tasks.value = [...tasks.value, ...newItems]
    nextCursor.value = page.next_cursor
    listError.value = null // WEB-R5-2：分页成功必须清除列表错误，否则模板 v-if="listError" 永久隐藏表格
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
    listError.value = t('state.error')
  }
}

onMounted(() => {
  if (props.pendingDeviceId && taskDevices.value.some((d) => d.device_id === props.pendingDeviceId)) {
    selected.value = [props.pendingDeviceId]
  }
  initialLoad()
})

watch(() => props.pendingDeviceId, (id) => {
  if (id && taskDevices.value.some((d) => d.device_id === id)) {
    selected.value = Array.from(new Set([...selected.value, id]))
  }
})

watch(() => props.initialTasks, (val) => {
  if (val !== undefined) {
    tasks.value = val
    nextCursor.value = props.initialNextCursor
  }
})

onBeforeUnmount(() => {
  abortController?.abort()
})
</script>
```

模板骨架（键与 testid 必须一致）：

```vue
<template>
  <div class="tasks-view">
    <h2>{{ t('task.preview.title') }}</h2>
    <fieldset>
      <legend>{{ t('task.preview.selectDevices') }}</legend>
      <p v-if="taskDevices.length === 0">{{ t('task.preview.empty') }}</p>
      <label v-for="device in taskDevices" :key="device.device_id">
        <input
          type="checkbox"
          :data-testid="`task-device-${device.device_id}`"
          :checked="selected.includes(device.device_id)"
          @change="selectDevice(device.device_id, ($event.target as HTMLInputElement).checked)"
        />
        {{ device.display_name }}
      </label>
    </fieldset>
    <BaseButton :disabled="selected.length === 0 || previewBusy" :loading="previewBusy"
      :loading-label="t('button.loading')" @click="loadPreview">{{ t('task.preview.submit') }}</BaseButton>
    <AppNotice v-if="previewError" tone="error" :tone-label="t('common.error')" :title="t('state.error')">
      <span>{{ previewError }}</span>
    </AppNotice>
    <ul v-if="preview">
      <li v-for="target in preview.targets" :key="target.device_id">
        <span>{{ target.device_id }}</span>
        <span>{{ target.eligible ? t('task.preview.eligible') : t('task.preview.ineligible') }}</span>
        <span v-if="!target.eligible && target.reason">{{ t(`task.reason.${target.reason}`) }}</span>
      </li>
    </ul>
    <AppNotice v-if="!csprngAvailable" tone="error" :tone-label="t('common.error')" :title="t('state.error')"><span>{{ t('task.security.csprngUnavailable') }}</span></AppNotice>
    <BaseButton v-if="preview && eligibleDeviceIds(preview).length > 0" :disabled="!csprngAvailable" @click="batchModalOpen = true">
      {{ t('task.submitReboot') }}
    </BaseButton>

    <h2>{{ t('task.list.title') }}</h2>
    <AppNotice v-if="listError" tone="error" :tone-label="t('common.error')" :title="t('state.error')"><span>{{ listError }}</span></AppNotice>
    <p v-else-if="tasks.length === 0">{{ t('task.list.empty') }}</p>
    <table v-else>
      <thead><tr>
        <th>{{ t('task.list.columns.device') }}</th><th>{{ t('task.list.columns.state') }}</th>
        <th>{{ t('task.list.columns.revision') }}</th><th>{{ t('common.actions') }}</th>
      </tr></thead>
      <tbody>
        <tr v-for="item in tasks" :key="item.id">
          <td>{{ item.id }}</td>
          <td>{{ mainTaskStateLabel(item.state, t) }}</td>
          <td>{{ item.revision }}</td>
          <td>
            <span>{{ t(describeAuthorizedSubset(item.view_scope).key) }}</span>
            <BaseButton @click="emit('openTask', item.id)">{{ t('task.detail.title') }}</BaseButton>
          </td>
        </tr>
      </tbody>
    </table>
    <BaseButton v-if="nextCursor" :disabled="loadMoreBusy" @click="loadMore">{{ t('task.list.loadMore') }}</BaseButton>

    <TaskModals mode="batch-risk" :open="batchModalOpen"
      :confirm-disabled="!preview || previewBusy || !csprngAvailable" :busy="operation.inFlight"
      @close="batchModalOpen = false" @confirm-batch-risk="submitBatch" />
  </div>
</template>
```

- [ ] **Step 5: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/views/TasksView.test.ts`

预期：PASS（11 个用例，原 10 + WEB-R5-2 分页错误恢复回归 1）。

- [ ] **Step 6: 组件错误态测试与缺后端静态注明（不得称真实 404 证据）**

在测试文件追加单测（并把 `import { post } from '../api'` 加到文件顶部 import 区），使用 stub fetch 模拟 404 错误响应验证错误态呈现：

```ts
import { post } from '../api'

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
  expect(screen.getByRole('alert')).toBeTruthy()
})
```

运行：`npm --prefix apps/controller-web test -- --run src/views/TasksView.test.ts`

预期：PASS（含 `afterEach` 的 `vi.unstubAllGlobals()` 清理）。静态注明：后端尚未注册任务路由，本用例仅为 Vitest stub 错误态单测，**绝不称“真实 404 证据”**，不伪造真实验收。

- [ ] **Step 7: 提交**

```bash
git add apps/controller-web/src/views/TasksView.vue apps/controller-web/src/views/TasksView.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 新增任务列表视图（预览、幂等提交、首屏加载与错误态）"
```

---

### Task 9: `TaskDetailView.vue` —— 详情（`id` 键）、children 分页、unknown 释放锁（成功后 GET 重取）、held time-review 与 deep-link 授权取消

**Files:**
- Create: `apps/controller-web/src/views/TaskDetailView.vue`
- Create: `apps/controller-web/src/views/TaskDetailView.test.ts`

**Interfaces:**
- Consumes: `task-ops.ts`（`TaskItem`、`SubTaskItem`、`assertTaskItemShape`、`assertSubTaskPageShape`、`describeAuthorizedSubset`、`canRequestCancel`）、`task-presentation.ts`（`TIME_REVIEW_DECISIONS`、`timeReviewDecisionKeys`、`mainTaskStateLabel`）、`components/TaskStateList.vue`（子任务十态仍由其内部 `taskStateLabel` 呈现）、`components/TaskModals.vue`。
- Produces：
  ```ts
  // props
  defineProps<{
    taskId: string                      // 规范 UUIDv4（路由已保证）
    initialTask?: TaskItem | null
    initialChildren?: SubTaskItem[]
    initialNextCursor?: string | null
    isAdmin?: boolean
    devicePermissions?: Record<string, string[]> // 权限表：deviceId -> permissions（供 deep-link 下按目标判定）
    onLoadTask?: (taskId: string) => Promise<TaskItem>
    onLoadChildren?: (taskId: string, cursor: string | null) => Promise<{ items: SubTaskItem[]; next_cursor: string | null }>
    onCancel?: (payload: { taskId: string; expectedRevision: string; confirm: true }) => Promise<void>
    onTimeReview?: (payload: { taskId: string; expectedRevision: string; decision: 'resume_original_deadline' | 'expire'; evidence: TimeReviewEvidenceKind }) => Promise<TaskItem>
    onReleaseLock?: (payload: { subtaskId: string; expected_revision: string; acknowledge_unknown: true; reason: string }) => Promise<void>
  }>()
  ```
- 行为契约：
  1. 主任务区显示 `task.detail.title`、`item.id`（`task.id`）、`mainTaskStateLabel(state)`（主任务四态专用文案）、`revision`、`created_time`（模板只消费 `task.created_time.reference_utc` 与 `task.created_time.quality` 原始值，绝不把对象整体渲染；不新增 i18n 键，`data-testid="task-created"` 保持不变），以及 `t(describeAuthorizedSubset(task.view_scope).key)`；`view_scope` 缺失时显示 `task.scope.unknown`。
  2. children 区：`t('task.detail.children')` + `TaskStateList`，支持点击释放锁打开二级确认模态框；`next_cursor` 存在时渲染加载更多并调用 `onLoadChildren(taskId, cursor)` 追加。children 读取走独立的子任务读取代际 `childrenGeneration`（正审 N2）：每次 `loadChildren` 启动（含分页与写操作成功触发的 GET 重取）即递增并捕获本地 `cGen`，过期判定用 `isChildrenStale(gen, cGen, id)`（`detailGeneration`、`childrenGeneration`、`props.taskId` 三重校验），同 `taskId` 内迟到的旧快照（含分页页）一律丢弃，不得覆盖最新 `children` 或 `nextCursor`。
  3. 取消入口判定（支持 deep-link，唯一依据后端 `actions.can_cancel`）：可请求的唯一依据是服务端返回的主任务 `actions.can_cancel`（后端已按当前身份、目标设备 `device.reboot` 与合格 `queued`/`held` 子任务计算，01 §4/§6:71、04 §8、设计文档 §3）。deep-link 下 App 无法预先获知任务包含的具体设备 ID，`TaskDetailView` 在挂载后通过 `onLoadTask` / `initialTask` 取得服务端真实数据；`cancellable` computed 只消费 `canRequestCancel(task)`（合法 `full`/`authorized_subset` scope 且 `actions?.can_cancel === true`），`handleCancel` 前置只再消费该结果（经 `cancellable`）与 `cancelBusy`（防双击）。前端**不得**借 children 分页、`device.task.read` 权限表（`devicePermissions`）或主任务状态再推导——`running` 主任务可以有可取消目标，`authorized_subset` 下的 `completed` 主任务也不能按主任务状态关闭；缺 `actions`（只读任务仍合法）、非法布尔或未知 `view_scope` 一律 fail-closed 关闭危险入口（`devicePermissions` prop 保留供 `App.vue` 接线，但不参与取消判定）。"可请求"不保成功：最终服务端事务（CAS 及身份/权限重验）仍为权威准绳，提交成功后照旧 `loadTask()` GET 重取（children 读取另由 `childrenGeneration` 防御同 taskId 内旧快照覆盖，见要点 2）。不可请求时渲染 `task.cancel.notCancellable` 说明文本（文案不新增、不改写），且不显示取消按钮。
  4. 模板取消操作显式绑定 `handleCancel` 方法：定义并实现 `async function handleCancel()`，点击取消按钮触发，调用 `onCancel({ taskId: props.taskId, expectedRevision: task.value.revision, confirm: true })`，成功后重新拉取任务数据，绝不在模板中使用未定义的 `emitCancel`。
  5. unknown 释放锁与 GET 重取：在详情页处理 `onReleaseLock`，成功后调用 `loadChildren(null)` 重取子任务（因 release-lock 响应仅返回 `{released:true,state:'unknown'}`，真实 SubTask 状态需重新拉取）。该重取经 `loadChildren` 推进 `childrenGeneration`，在途的旧 children 读取（含初次挂载 C0 与进行中的分页页）立即失效，迟到旧快照不得覆盖写后最新列表。
  6. held 时间核验区（入口唯一依据主任务 `counts.held`，**不是**当前页 children）：
     - held 全集判定：`hasHeld` computed 只消费同一已加载主任务 `TaskItem` 的 `counts.held`（Task 3 全字段 guard 已保证其为合法 u64 十进制串；本处仍按 `isU64DecimalString` 严格验证且 `!== '0'`，**不转换 `Number`**——字符串比较支持 u64 全域），不由当前页 children 推导、不另发统计 GET、不泄露 hidden totals（02/04 §4：主任务 `counts` 为当前授权全集十态计数，与 children 独立分页；当前页有无 `state === 'held'` 子任务不能证明主任务当前授权全集是否 held）。
     - 只读说明（所有合法可见 scope 均可见）：显示 `task.held.reviewTitle`、`task.held.reviewExplain`、`task.held.reviewSelfReported`；`!hasHeld` 时显示 `task.held.reviewNoHeld`（zero counts 即便旧 page 有 held 也不开）。
     - 危险入口（两决策按钮 + evidence textarea）必须同时满足当前 `isAdmin === true`、`view_scope === 'full'`、`hasHeld === true` 才渲染：label 分别取 `timeReviewDecisionKeys()` 的两个键；**必须恰好两个**，且不存在任何"延长/自定义期限"控件（无 number/date 输入）；`authorized_subset` / 未知 `view_scope` / 缺或坏 `counts.held`（缺键、JSON 数、负数、前导零、非十进制）一律 fail-closed 不渲染；`runTimeReview` 前置消费**同一** guard（`hasHeld` 且 `props.isAdmin === true` 且 `view_scope === 'full'`，加非空 evidence 与 busy），不能只禁按钮而 method 不核 guard。
     - 非管理员：只显示只读说明，**不渲染**任何决策按钮。
     - 点击任一决策：要求先填写非空 `evidence`（textarea，`:aria-label` 绑定 `task.detail.evidenceSource`——该键专用于本 time-review 请求的 `evidence` 字段（02 §6），**不是**子任务 `result_evidence` 种类展示键，后者用 `task.detail.resultEvidence`，见 Task 2 词条值要求）；调用 `onTimeReview({ taskId, expectedRevision: task.revision, decision, evidence })`；用返回的 `TaskItem` **覆盖**主任务状态（`assertTaskItemShape` 校验），并**重新**调用 `onLoadChildren(taskId, null)` 重取 children；绝不本地推导"已恢复/已延期"结论，"无 held/新状态"均以服务端返回的 `counts`/`state` 为准。
     - 明确剩余（本切片不修、Task 12 报告单列，见 Task 12 表行 13）：请求层（Task 5）已纸面对齐：`TimeReviewEvidenceKind` 三串词表 + `TIME_REVIEW_EVIDENCE_KINDS` + `isTimeReviewEvidenceKind` 谓词，`reviewHeldTime` 在 `post` 前严格校验/narrow，非法（空/自由文本/未知/数字）抛 `INVALID_TIME_REVIEW_EVIDENCE` 且 0 网络调用、body 只发批准的三串；旧 UI 的 evidence textarea 仍为自由文本字符串，如今不能成功通过请求 guard（旧自由文本会被请求层拒绝），UI 需后续任务改为显式三值选择（不默认 `inconclusive`、不把任意文字强制为 `reference_deadline_proven`）；本切片不更改 Task 9 callback/textarea 实现、不做 `as TimeReviewEvidenceKind` cast 绕过、不新增 `can_time_review` 等 action/权限点、不改取消 `can_cancel` 合同、不改 wire/request payload 字段，不混入 taskId/watch/generation/a11y 改动。
  7. 所有核验文案旁必须出现 `task.held.reviewSelfReported`，明示以后端结果为准。
  8. 加载失败：显示 `state.error` 且保留旧数据不伪装成功。
  9. 行数与选择器预算（本合同新增，明确不虚报）：`task-ops.ts` 新增 1 个导出纯函数 `canRequestCancel`、1 个错误码 `INVALID_TASK_ACTIONS`、`TaskItem` 1 个可选字段、`ViewScope` 并集 1 个合法值；`TaskDetailView.vue` 新增局部方法 0 个、新增 computed 0 个（cancel 切片仅替换既有 `cancellable` 实现体；held 切片将 `heldChildren` 替换为 `hasHeld`，净 0）、新增 `data-testid` 选择器 0 个（复用既有 role 按钮 `task.cancel.action` 与 `task.cancel.notCancellable` 文案及既有 `time-review-*` 选择器）、导入符号共新增 2 个（`canRequestCancel` + `isU64DecimalString`（复用 `types.ts` 既有导出，不改 `types.ts`））；两切片合计新增/改动代码（含测试）≈180–200 行，预算 ≤240 行；测试计数 Task 3 恰为 9→17、Task 9 恰为 7→14→15→16→17→18（held 切片原地更新 2 个既有 held 用例、新增 1 个 counts 门禁 table 用例；代际防御切片新增 2 个过期响应交错用例（1 个跨 taskId、1 个同 taskId 写重取交错，正审 N2）；WEB-R5-2 写恢复回归新增 1 个；Task 6 `TaskStateList` 仍 5 不变），Task 12 全量计数按实跑报告、不预测。
  10. 写操作（取消、释放锁、时间审核）成功后必须清除自身所属的 `actionError`，使重试成功的操作恢复正常呈现；其后触发的 GET 重取若失败只置 `taskError`/`childrenError`，alert 依然可见、不破坏 WEB-R5-1 语义（WEB-R5-2）。

- [ ] **Step 1: 建立可编译桩代码**

```vue
<script setup lang="ts">
import type { SubTaskItem, TaskItem } from '../task-ops'
import type { TimeReviewEvidenceKind } from '../task-requests'

defineProps<{
  taskId: string
  initialTask?: TaskItem | null
  initialChildren?: SubTaskItem[]
  initialNextCursor?: string | null
  isAdmin?: boolean
  devicePermissions?: Record<string, string[]>
  onLoadTask?: (taskId: string) => Promise<TaskItem>
  onLoadChildren?: (taskId: string, cursor: string | null) => Promise<{ items: SubTaskItem[]; next_cursor: string | null }>
  onCancel?: (payload: { taskId: string; expectedRevision: string; confirm: true }) => Promise<void>
  onTimeReview?: (payload: {
    taskId: string
    expectedRevision: string
    decision: 'resume_original_deadline' | 'expire'
    evidence: TimeReviewEvidenceKind
  }) => Promise<TaskItem>
  onReleaseLock?: (payload: { subtaskId: string; expected_revision: string; acknowledge_unknown: true; reason: string }) => Promise<void>
}>()
</script>

<template>
  <div class="task-detail-view-stub"></div>
</template>
```

- [ ] **Step 2: 编写失败测试**

`apps/controller-web/src/views/TaskDetailView.test.ts`：

```ts
import { fireEvent, render, screen, waitFor } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import TaskDetailView from './TaskDetailView.vue'
import { createI18n } from '../i18n'
import { baseTaskTimeEvidence, zeroTaskCounts, type SubTaskItem, type TaskItem } from '../task-ops'

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
  return render(TaskDetailView, { props, global: { provide: { i18n } } })
}

afterEach(() => { vi.restoreAllMocks() })

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

  it('paginates children with the opaque cursor', async () => {
    const onLoadChildren = vi.fn(async () => ({ items: [child({ id: SUB_B })], next_cursor: null }))
    mount({ taskId: TASK_ID, initialTask: heldTask, initialChildren: [child()], initialNextCursor: 'cursor-1', onLoadChildren })
    await fireEvent.click(screen.getByRole('button', { name: name('task.list.loadMore') }))
    expect(onLoadChildren).toHaveBeenCalledWith(TASK_ID, 'cursor-1')
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
      expect(screen.getByText(i18n.t('task.mainState.queued'))).toBeTruthy()
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
})
```

- [ ] **Step 3: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/views/TaskDetailView.test.ts`

说明：本合同不新增 i18n 键（`task.cancel.action`/`task.cancel.notCancellable` 已在 Task 2 键表内），不改 `i18n.test.ts` 及其既有 `zh`/`en` 默认导入（Task 2 的旧 en 导入修正原样保留）；本测试文件沿用既有 `createI18n` 导入与 `name()`/`mount()`/fixture 结构，不重复追加 `en`/`zh` 导入。

预期：文件可编译加载，运行 18 个用例（前计 15 + 本代际防御切片新增 2 + WEB-R5-2 写恢复回归 1，另有 2 个既有 held 用例原地更新、不计新增，不虚报），绝大多数失败：`TestingLibraryElementError: Unable to find an accessible element with the role "button" and name /核验后按原期限恢复/i`。
（有效行为 RED：文件可编译、运行数非 0、因断言失败而红；非 todo / 缺符号 / 0 tests。）

- [ ] **Step 4: 最小实现**

实现要点（结构参照 Task 8，命名必须一致）：

```vue
<script setup lang="ts">
import { computed, inject, onMounted, ref, watch } from 'vue'
import {
  assertSubTaskPageShape,
  assertTaskItemShape,
  canRequestCancel,
  describeAuthorizedSubset,
  type SubTaskItem,
  type TaskItem,
} from '../task-ops'
import { isU64DecimalString } from '../types'
import { TIME_REVIEW_DECISIONS, mainTaskStateLabel, timeReviewDecisionKeys } from '../task-presentation'
import type { createI18n } from '../i18n'
import BaseButton from '../components/BaseButton.vue'
import AppNotice from '../components/AppNotice.vue'
import TaskStateList from '../components/TaskStateList.vue'
import TaskModals from '../components/TaskModals.vue'

const props = withDefaults(defineProps<{
  taskId: string
  initialTask?: TaskItem | null
  initialChildren?: SubTaskItem[]
  initialNextCursor?: string | null
  isAdmin?: boolean
  devicePermissions?: Record<string, string[]>
  onLoadTask?: (taskId: string) => Promise<TaskItem>
  onLoadChildren?: (taskId: string, cursor: string | null) => Promise<{ items: SubTaskItem[]; next_cursor: string | null }>
  onCancel?: (payload: { taskId: string; expectedRevision: string; confirm: true }) => Promise<void>
  onTimeReview?: (payload: {
    taskId: string
    expectedRevision: string
    decision: 'resume_original_deadline' | 'expire'
    evidence: 'reference_deadline_proven' | 'reference_deadline_expired' | 'inconclusive'
  }) => Promise<TaskItem>
  onReleaseLock?: (payload: { subtaskId: string; expected_revision: string; acknowledge_unknown: true; reason: string }) => Promise<void>
}>(), {
  initialTask: null,
  initialChildren: () => [],
  initialNextCursor: null,
  isAdmin: false,
  devicePermissions: () => ({}),
})

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = (key: string) => i18n.t(key)

const task = ref<TaskItem | null>(props.initialTask ?? null)
const children = ref<SubTaskItem[]>(props.initialChildren ?? [])
const nextCursor = ref<string | null>(props.initialNextCursor ?? null)
// 显式三值证据选择：初始 null（无预选），恰 3 个合法值（Task 5 `TimeReviewEvidenceKind` 词表 1:1，Task 2 i18n 键 `task.timeReview.evidence.*`）
const EVIDENCE_KINDS = ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'] as const
const evidence = ref<(typeof EVIDENCE_KINDS)[number] | null>(null)
const reviewBusy = ref(false)
const cancelBusy = ref(false)
const releaseBusy = ref(false)
const releaseTarget = ref<SubTaskItem | null>(null)
// WEB-N1：错误状态分离——主任务 GET、children 分页与写操作各持独立领域错误，
// 任一方成功只清空自身错误，绝不抹除另一路在途或已失败操作的错误（L3402 错误态保留契约）
const taskError = ref<string | null>(null)
const childrenError = ref<string | null>(null)
const actionError = ref<string | null>(null)
// 聚合消费：任领域失败必呈现可见错误，绝不被另一路成功抹为 null
const displayError = computed(() => taskError.value || childrenError.value || actionError.value)

// held 全集判定唯一依据：同一已加载主任务 counts.held（服务端按当前授权全集重算；02 主任务 counts 与 children 独立分页，
// 当前页 children 不能证明授权全集是否 held）；严格 isU64DecimalString 且 ≠"0"，不转换 Number（支持 u64 全域），不另发统计 GET
const hasHeld = computed(() => {
  const held = task.value?.counts?.held
  return typeof held === 'string' && isU64DecimalString(held) && held !== '0'
})

// 取消入口唯一依据：后端计算的 actions.can_cancel（当前身份/device.reboot/合格 queued-held，01 §4/§6:71、04 §8）；
// 不看主任务状态、不接 children 分页或 devicePermissions 权限表（prop 保留供 App.vue 接线，不参与判定）；
// 缺 actions/非法布尔/未知 view_scope 由 canRequestCancel fail-closed；"可请求"不保成功，服务端事务为准
const cancellable = computed(() => (task.value ? canRequestCancel(task.value) : false))

// 统一代际令牌：每次异步操作（初次加载/分页/写操作/GET 重取）开始时捕获 (gen, id)，所有 await 返回后、catch 分支与
// finally 回写前严格校验两者仍与捕获值一致；不一致即过期响应，直接丢弃结果且不触碰任何状态（含 busy 与 error）
const isStale = (gen: number, id: string) => gen !== detailGeneration.value || id !== props.taskId

// 子任务读取代际（同 taskId 内时序防御，正审 N2）：detailGeneration 只在切换 taskId 时递增，拦不住同一任务页面内
// "写操作成功 GET 重取（C1）先返回、挂载/分页旧读取（C0）迟到返回"——旧快照 gen 与 id 均未变、会通过 isStale 并把
// 旧列表写回 children 造成状态倒退。每次 loadChildren 启动（含分页与写操作成功触发的 GET 重取）即推进
// childrenGeneration 并捕获本地 cGen；分页只累加当前代际的页，迟到旧代际快照被 isChildrenStale 拦截整体丢弃，
// 不触碰 children/nextCursor/childrenError
const childrenGeneration = ref(0)
const isChildrenStale = (gen: number, cGen: number, id: string) =>
  gen !== detailGeneration.value || cGen !== childrenGeneration.value || id !== props.taskId

async function loadChildren(cursor: string | null = null) {
  if (!props.onLoadChildren) return
  const gen = detailGeneration.value
  const id = props.taskId
  childrenGeneration.value += 1
  const cGen = childrenGeneration.value
  try {
    const page = await props.onLoadChildren(id, cursor)
    if (isChildrenStale(gen, cGen, id)) return
    assertSubTaskPageShape(page)
    children.value = cursor === null ? page.items : [...children.value, ...page.items]
    nextCursor.value = page.next_cursor
    childrenError.value = null
  } catch {
    if (isChildrenStale(gen, cGen, id)) return
    childrenError.value = t('state.error')   // 保留旧数据，不伪装成功、不伪装空；不触碰 taskError/actionError
  }
}

async function loadTask() {
  if (!props.onLoadTask) return
  const gen = detailGeneration.value
  const id = props.taskId
  try {
    const data = await props.onLoadTask(id)
    if (isStale(gen, id)) return
    assertTaskItemShape(data)
    task.value = data
    taskError.value = null
  } catch {
    if (isStale(gen, id)) return
    taskError.value = t('state.error')   // 保留旧数据，不伪装成功；不触碰 childrenError/actionError
  }
}

async function handleCancel() {
  if (!props.onCancel || !task.value || !cancellable.value || cancelBusy.value) return
  const gen = detailGeneration.value
  const id = props.taskId
  cancelBusy.value = true
  try {
    await props.onCancel({ taskId: id, expectedRevision: task.value.revision, confirm: true })
    if (isStale(gen, id)) return   // 过期返回：不触发 GET 重取、不回写
    actionError.value = null   // WEB-R5-2：写操作成功必须清除动作错误，避免同任务重试成功后 alert 永久残留
    await loadTask()               // 同代重取：loadTask 捕获同一 (gen, id)，不换代
  } catch {
    if (!isStale(gen, id)) actionError.value = t('state.error')
  } finally {
    if (!isStale(gen, id)) cancelBusy.value = false   // 旧代 finally 不得清除新代 busy
  }
}

async function handleReleaseLock(payload: { subtaskId: string; expected_revision: string; acknowledge_unknown: true; reason: string }) {
  if (!props.onReleaseLock || releaseBusy.value) return
  const gen = detailGeneration.value
  const id = props.taskId
  releaseBusy.value = true
  try {
    await props.onReleaseLock(payload)
    if (isStale(gen, id)) return   // 过期返回：不关闭新代模态、不触发重取
    releaseTarget.value = null
    actionError.value = null   // WEB-R5-2：写操作成功必须清除动作错误，避免同任务重试成功后 alert 永久残留
    // 释放锁响应仅包含 {released:true,state:'unknown'}，成功后通过 GET 重新拉取子任务列表
    await loadChildren(null)
  } catch {
    if (!isStale(gen, id)) actionError.value = t('state.error')
  } finally {
    if (!isStale(gen, id)) releaseBusy.value = false
  }
}

async function runTimeReview(decision: 'resume_original_deadline' | 'expire') {
  // 与危险按钮同一 guard（fail-closed）：hasHeld + 当前 isAdmin===true + view_scope==='full'；不新增 can_time_review 权限点、不改 wire payload
  if (!props.onTimeReview || !task.value || !hasHeld.value || props.isAdmin !== true || task.value.view_scope !== 'full' || evidence.value === null || reviewBusy.value) return
  const gen = detailGeneration.value
  const id = props.taskId
  reviewBusy.value = true
  try {
    // 显式选择后 evidence.value 已 narrow 为强类型 `TimeReviewEvidenceKind`，原样透传、不 trim/不改写
    const selectedEvidence = evidence.value
    const updated = await props.onTimeReview({
      taskId: id,
      expectedRevision: task.value.revision,
      decision,
      evidence: selectedEvidence,
    })
    if (isStale(gen, id)) return   // 过期返回：旧 DTO 不得覆盖新任务的 task
    assertTaskItemShape(updated)
    task.value = updated          // 只采用服务端返回，绝不本地推断结论
    actionError.value = null   // WEB-R5-2：写操作成功必须清除动作错误，避免同任务重试成功后 alert 永久残留
    await loadChildren(null)      // 按服务端原子结果重取
  } catch {
    if (!isStale(gen, id)) actionError.value = t('state.error')
  } finally {
    if (!isStale(gen, id)) reviewBusy.value = false
  }
}

onMounted(() => {
  // 初次加载同样走统一代际令牌：loadTask/loadChildren 内部捕获当前 gen 与 taskId，不再有"不传 gen 即不校验"的例外
  if (!props.initialTask) loadTask()
  if (props.initialChildren.length === 0) loadChildren(null)
})

const detailGeneration = ref(0)
watch(
  () => props.taskId,
  (newId) => {
    // 路由参数切换到另一任务 ID（组件实例复用）：递增代际令牌并清空旧任务数据与四 busy，旧操作后续 catch/finally 一律被 isStale 拦截，再重取新任务
    detailGeneration.value += 1
    task.value = null
    children.value = []
    nextCursor.value = null
    evidence.value = null
    taskError.value = null
    childrenError.value = null
    actionError.value = null
    releaseTarget.value = null
    releaseBusy.value = false
    reviewBusy.value = false
    cancelBusy.value = false
    if (newId) {
      loadTask()
      loadChildren(null)
    }
  },
)
</script>

<template>
  <div class="task-detail-view">
    <h2>{{ t('task.detail.title') }}</h2>
    <AppNotice v-if="displayError" tone="error" :tone-label="t('common.error')" :title="t('state.error')"><span>{{ displayError }}</span></AppNotice>
    <template v-if="task">
      <p data-testid="task-id">{{ task.id }}</p>
      <p data-testid="task-state">{{ mainTaskStateLabel(task.state, t) }}</p>
      <p data-testid="task-revision">{{ task.revision }}</p>
      <p data-testid="task-created">{{ task.created_time.reference_utc }} {{ task.created_time.quality }}</p>
      <p data-testid="task-scope">{{ t(describeAuthorizedSubset(task.view_scope).key) }}</p>
      <p v-if="!cancellable">{{ t('task.cancel.notCancellable') }}</p>
      <BaseButton v-if="cancellable" variant="danger" :loading="cancelBusy" :loading-label="t('button.loading')" @click="handleCancel">{{ t('task.cancel.action') }}</BaseButton>

      <h3>{{ t('task.held.reviewTitle') }}</h3>
      <p>{{ t('task.held.reviewExplain') }}</p>
      <p>{{ t('task.held.reviewSelfReported') }}</p>
      <p v-if="!hasHeld">{{ t('task.held.reviewNoHeld') }}</p>
      <template v-else-if="isAdmin && task.view_scope === 'full'">
        <fieldset data-testid="time-review-evidence">
          <label>{{ t('task.detail.evidenceSource') }}</label>
          <label v-for="kind in EVIDENCE_KINDS" :key="kind">
            <input type="radio" name="time-review-evidence" :value="kind" v-model="evidence" :data-testid="'time-review-evidence-' + kind">
            {{ t('task.timeReview.evidence.' + kind) }}
          </label>
        </fieldset>
        <BaseButton
          v-for="decision in TIME_REVIEW_DECISIONS"
          :key="decision"
          :disabled="evidence === null || reviewBusy"
          :loading="reviewBusy"
          :loading-label="t('button.loading')"
          :data-testid="`time-review-${decision}`"
          @click="runTimeReview(decision)"
        >
          {{ t(timeReviewDecisionKeys()[decision]) }}
        </BaseButton>
      </template>

      <h3>{{ t('task.detail.children') }}</h3>
      <TaskStateList :subtasks="children" :show-release-action="isAdmin" @release-lock="(sub) => { releaseTarget = sub }" />
      <BaseButton v-if="nextCursor" @click="loadChildren(nextCursor)">{{ t('task.list.loadMore') }}</BaseButton>

      <TaskModals mode="release-lock" :open="releaseTarget !== null" :subtask="releaseTarget"
        :busy="releaseBusy" @close="releaseTarget = null"
        @confirm-release-lock="handleReleaseLock" />
    </template>
  </div>
</template>
```

- [ ] **Step 5: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/views/TaskDetailView.test.ts`

预期：PASS（18 个用例；前计 15 + 本代际防御切片新增 2 + WEB-R5-2 写恢复回归 1，另有 2 个既有 held 用例原地更新、不计新增，不虚报）。

- [ ] **Step 6: 详情页错误态测试与缺后端静态注明（不得称真实 404 证据）**

在测试文件中使用 stub fetch 模拟 404 响应，断言请求路径与详情页错误态呈现（写法同 Task 8 Step 6）。
报告静态注明：`GET /api/v1/tasks/{id}`、`GET /api/v1/tasks/{id}/children`、`POST /api/v1/tasks/{id}/time-review` 在当前 Rust 树中未注册；本用例仅验证组件捕获 404 后的错误呈现，**绝不称“真实 404 证据”**，不伪造真实验收。

- [ ] **Step 7: 提交**

```bash
git add apps/controller-web/src/views/TaskDetailView.vue apps/controller-web/src/views/TaskDetailView.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 新增任务详情视图（子任务分页、后端 can_cancel 取消入口与 held 时间核验）"
```

---


### Task 1: 前置验证（幂等头切片 + 纯函数基线）

**Files:**
- 只读检查，不产生文件改动；若缺前置则输出阻断报告并停止后续任务。

**Interfaces:**
- Consumes: 独立 Web 分支 `controller-v1-web-idempotency` 的 commit `c4fe870`（`feat(controller-web): support Idempotency-Key request header in api post`）；baseline 已合入的 `apps/controller-web/src/task-presentation.ts`。
- Produces: 后续任务的可用契约 ——
  ```ts
  // 必须来自 c4fe870（当前 baseline 还没有）
  export interface RequestOptions {
    signal?: AbortSignal
    csrfToken?: string
    idempotencyKey?: string
  }
  export async function post<T>(path: string, payload: unknown, options?: RequestOptions): Promise<ApiResponse<T>>
  ```
  以及 `task-presentation.ts` 既有导出（签名不得改动）：`SUBTASK_STATES`、`SubTaskState`、`SPEC_REASON_CODES`、`SpecReasonCode`、`taskStateLabel(state, t)`、`reasonCodeLabel(code, t)`、`generateUuidV4()`。

- [ ] **Step 1: 检查幂等头切片是否已进入当前树**

运行：
```bash
git log --oneline -1 c4fe870
git merge-base --is-ancestor c4fe870 HEAD && echo MERGED || echo NOT_MERGED
grep -n "idempotencyKey\|RequestOptions" apps/controller-web/src/api.ts
```

预期（已就绪）：提交存在、第二行输出 `MERGED`、第三行能看到 `RequestOptions` 与 `idempotencyKey`。若输出 `NOT_MERGED` 或 `grep` 无匹配 → 报告"依赖：`c4fe870`（controller-v1-web-idempotency，独立已审提交）尚未合入当前分支"，并**先把该切片导入目标集成分支**（由集成分支负责人 merge，本任务执行者不自行 merge、不 cherry-pick、不改 `api.ts`），导入后重跑本步骤确认 `MERGED` 再继续；不得在未导入的情况下空等 Task 3 起永久阻断。

- [ ] **Step 2: 检查纯函数基线与既有导出**

运行：
```bash
grep -n "export function\|export const\|export type" apps/controller-web/src/task-presentation.ts
npm --prefix apps/controller-web test -- --run src/task-presentation.test.ts
```

预期：能看到 `taskStateLabel`、`reasonCodeLabel`、`generateUuidV4`、`SUBTASK_STATES`、`SPEC_REASON_CODES`；测试全绿（baseline 已合入）。任何失败即阻断报告。

- [ ] **Step 3: 检查后端任务端点确实尚未存在（静态注明，不做修改）**

运行：
```bash
grep -rn "task-previews\|time-review\|release-lock\|/api/v1/tasks" crates/rsetup-controller/src/ || echo "NO_TASK_ROUTES"
```

预期：输出 `NO_TASK_ROUTES`（或仅命中注释）。把输出原样记入本任务的执行报告，作为**缺后端静态注明**。该静态注明不是真实请求证据；**不要**去实现这些路由，也不要在组件任务里强制真实网络访问。

- [ ] **Step 4: 记录前置结论（无提交）**

本任务不产生代码，不需要提交。报告格式：
```
Task 1 结论：幂等头切片=<MERGED|NOT_MERGED>（未合并则先导入 c4fe870 再继续）；纯函数基线=<PASS|FAIL>；后端任务路由=<NO_TASK_ROUTES|已存在>（仅静态注明，非真实 404 证据）。
若 FAIL：后续 Task 3..12 全部 BLOCKED。
```

- [ ] **Step 5: 提交**

无提交（只读验证任务）。

---

### Task 2: Task 4 词典键补齐与中英 1:1 等长

**Files:**
- Modify: `apps/controller-web/src/locales/zh-CN.ts`
- Modify: `apps/controller-web/src/locales/en.ts`
- Modify: `apps/controller-web/src/i18n.test.ts`

**Interfaces:**
- Consumes: `createI18n({ initialLocale, storage })`（`apps/controller-web/src/i18n.ts`），其 `t(key)` 在缺键时原样返回 key。
- Produces: 下列键在 `zh-CN` 与 `en` 中都存在且非空（后续视图与测试用 `i18n.t('...')` 查询，**不得新增本表以外的键**，其余词典扩项归上位计划 Task 6）：
  ```text
  common.cancel, common.actions
  nav.tasks
  task.list.title, task.list.empty, task.list.loadMore, task.list.columns.device,
  task.list.columns.state, task.list.columns.revision
  task.preview.title, task.preview.selectDevices, task.preview.submit, task.preview.empty,
  task.preview.eligible, task.preview.ineligible
  task.submitReboot, task.confirmRelease, task.releaseLock, task.security.csprngUnavailable
  task.field.releaseReason
  task.risk.confirmBatchReboot, task.risk.releaseLockTitle, task.risk.releaseWarningDetail,
  task.risk.acknowledgeUnknown
  task.held.reviewTitle, task.held.reviewResumeOriginalDeadline, task.held.reviewExpire,
  task.held.reviewExplain, task.held.reviewNoHeld, task.held.reviewSelfReported
  task.cancel.action, task.cancel.notCancellable
  task.detail.title, task.detail.children, task.detail.evidenceSource, task.detail.resultEvidence
  task.timeReview.evidence.reference_deadline_proven, task.timeReview.evidence.reference_deadline_expired,
  task.timeReview.evidence.inconclusive
  task.scope.authorizedSubset, task.scope.unknown
  task.mainState.queued, task.mainState.running, task.mainState.held, task.mainState.completed
  ```
- 词条值要求：`task.risk.releaseWarningDetail` 必须明确包含"释放锁不等于撤回执行 / the device may have already rebooted; releasing the lock does not undo the reboot"语义；`task.scope.authorizedSubset` 必须表述为"仅显示当前已授权目标 / only currently authorized targets are shown"，**不得**出现任何数字占位符；`task.detail.resultEvidence` 是子任务 `result_evidence`（结果证据**种类**，02 §1）展示专用键，`task.detail.evidenceSource` 仅保留给 held time-review 请求 `evidence` 字段（02 §6，管理员填写的证据来源/引用），子任务证据种类展示**不得**复用 EvidenceSource 标签误导种类。preview 目标 `reason` 文案复用基线 `task.reason.*` SCREAMING_SNAKE_CASE 键（经 `reasonCodeLabel` 映射；词表来源=Runtime Task 6 DTO 的 04 ReasonCode，账本决议③），**不新增** `task.previewReason.*` 小写键。

- [ ] **Step 1: 编写失败测试（可编译的行为 RED）**

把下面代码分两处放置（复用既有 `zh`、`en` 及 `describe`/`it`/`expect` 导入，不重复追加）：`const TASK4_KEYS = [...] as const` 定义放在既有 `const keys` **之前**（即 L7 导入之后、L9 `const keys` 声明之前），`describe('Task 4 lexicon completeness', ...)` 块仍追加在文件**末尾**；`TASK4_KEYS` 全文件只声明这一次，不得在两处重复声明（避免重复 const 与 TDZ）：

```ts
const TASK4_KEYS = [
  'common.cancel', 'common.actions',
  'nav.tasks',
  'task.list.title', 'task.list.empty', 'task.list.loadMore', 'task.list.columns.device',
  'task.list.columns.state', 'task.list.columns.revision',
  'task.preview.title', 'task.preview.selectDevices', 'task.preview.submit', 'task.preview.empty',
  'task.preview.eligible', 'task.preview.ineligible',
  'task.submitReboot', 'task.confirmRelease', 'task.releaseLock', 'task.security.csprngUnavailable', 'task.field.releaseReason',
  'task.risk.confirmBatchReboot', 'task.risk.releaseLockTitle', 'task.risk.releaseWarningDetail',
  'task.risk.acknowledgeUnknown',
  'task.held.reviewTitle', 'task.held.reviewResumeOriginalDeadline', 'task.held.reviewExpire',
  'task.held.reviewExplain', 'task.held.reviewNoHeld', 'task.held.reviewSelfReported',
  'task.cancel.action', 'task.cancel.notCancellable',
  'task.detail.title', 'task.detail.children', 'task.detail.evidenceSource', 'task.detail.resultEvidence',
  'task.timeReview.evidence.reference_deadline_proven', 'task.timeReview.evidence.reference_deadline_expired',
  'task.timeReview.evidence.inconclusive',
  'task.scope.authorizedSubset', 'task.scope.unknown',
  'task.mainState.queued', 'task.mainState.running', 'task.mainState.held', 'task.mainState.completed',
] as const

describe('Task 4 lexicon completeness', () => {
  it('defines every Task 4 key in both dictionaries with a non-empty value', () => {
    for (const key of TASK4_KEYS) {
      expect(zh[key], `zh-CN missing ${key}`).toBeTruthy()
      expect(en[key], `en missing ${key}`).toBeTruthy()
    }
  })

  it('keeps zh-CN and en key sets 1:1 equal in size', () => {
    expect(Object.keys(zh).sort()).toEqual(Object.keys(en).sort())
  })

  it('warns that releasing the unknown lock does not undo a possible reboot, in both locales', () => {
    expect(zh['task.risk.releaseWarningDetail']).toContain('不撤回')
    expect(en['task.risk.releaseWarningDetail']).toMatch(/does not undo/i)
  })

  it('never bakes a hidden-target count into the authorized subset notice', () => {
    expect(zh['task.scope.authorizedSubset']).not.toMatch(/\d/)
    expect(en['task.scope.authorizedSubset']).not.toMatch(/\d/)
  })
})
```

说明：复用既有导入 `zh`、`en`，locale 文件导出形状已核为 `Record<string, string>`（`export default messages`），因此上面的属性断言可编译。

**既有精确 keys 同步（必做，否则原测试必失败）：** 既有 `i18n.test.ts` L9-34 是硬编码 `const keys` 数组，L49 以 `Object.keys(zh).sort()` 与 `[...keys].sort()` 精确相等断言（L50 断言 zh/en 1:1）。Step 3 给 locale 新增键后若不同步 `keys`，原用例必因精确相等失败。同步方式（保留既有字面量键、不从 `Object.keys` 反推期望、不 disable 原断言）：

1. 把 `keys` 数组既有尾行 `'task.reason.unknown'];` 精确替换为下面两行（`TASK4_KEYS` 已按 Step 1 定义在 `const keys` 之前——`const` 无提升初始化，若定义在 `keys` 之后展开 `...TASK4_KEYS` 会触发 TDZ `ReferenceError`）：
   ```ts
   'task.reason.unknown',
   ...TASK4_KEYS];
   ```
2. `TASK4_KEYS` 与既有字面量键零重叠（既有数组只含 `app.*`/`button.*`/`auth.*`/`device.*`/`task.state.*`/`task.reason.*` 等，不含 `task.list.*`/`task.preview.*`/`task.mainState.*`/`task.security.csprngUnavailable`），精确断言不会因重复键多计。
3. `common.unknown` 与 `common.error` 是既有 `keys`（L23）与 zh/en 词典中已存在的键，`DeviceDetailView` 及本计划 Task 8/9 仍消费二者：**保留这两个键及其既有 locale 值**，不得从 `keys` 或词典删除，也不得改写其既有文案。

- [ ] **Step 2: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/i18n.test.ts`

预期：测试文件正常加载，运行数增加（含新增 4 个用例），新增用例失败，错误形如
`AssertionError: zh-CN missing common.cancel: expected undefined to be truthy`。
既有 `exports a Vue locale ref, complete matching dictionaries and exact foundation copy` 用例同时失败：`keys` 已含 `...TASK4_KEYS` 而 zh locale 尚未加键，L49 精确相等断言不成立（同为有效断言失败，非编译错误）。
（有效行为 RED：文件可编译、运行数非 0、因断言失败而红。）

- [ ] **Step 3: 最小实现**

在 `apps/controller-web/src/locales/zh-CN.ts` 的 `task.reason.unknown` 之后追加：

```ts
  'common.actions': '操作',
  'common.cancel': '取消',
  'nav.tasks': '任务',
  'task.list.title': '任务列表',
  'task.list.empty': '暂无可见任务',
  'task.list.loadMore': '加载更多',
  'task.list.columns.device': '设备',
  'task.list.columns.state': '状态',
  'task.list.columns.revision': '版本',
  'task.preview.title': '批量重启预览',
  'task.preview.selectDevices': '选择设备',
  'task.preview.submit': '生成预览',
  'task.preview.empty': '没有可提交的目标设备',
  'task.preview.eligible': '可提交',
  'task.preview.ineligible': '不可提交',
  'task.submitReboot': '确认提交批量重启',
  'task.security.csprngUnavailable': '当前浏览器缺少安全随机数来源（crypto.getRandomValues），批量重启等危险提交已禁用。',
  'task.confirmRelease': '确认释放设备锁',
  'task.releaseLock': '释放设备占用',
  'task.field.releaseReason': '释放原因',
  'task.risk.confirmBatchReboot': '我已确认固定清单并在风险确认后提交重启',
  'task.risk.releaseLockTitle': '释放 unknown 任务占用的设备锁',
  'task.risk.releaseWarningDetail': '设备可能已经重启；释放锁只是解除本次占用，不撤回已经发生的执行，也不会自动重发重启。',
  'task.risk.acknowledgeUnknown': '我已知悉结果仍未确认，释放锁不等于撤回执行',
  'task.held.reviewTitle': 'held 子任务时间核验',
  'task.held.reviewResumeOriginalDeadline': '核验后按原期限恢复',
  'task.held.reviewExpire': '核验后终止为已过期',
  'task.held.reviewExplain': '核验只依据服务端证据：可证明原期限仍有效则按原期限恢复，可证明已过期则终止；绝不延长期限。',
  'task.held.reviewNoHeld': '当前没有 held 子任务，无需核验',
  'task.held.reviewSelfReported': '核验结果以服务端返回为准，界面不自行判定审计事实',
  'task.cancel.action': '取消任务',
  'task.cancel.notCancellable': '该任务已进入下发及之后阶段，不能伪装为已取消',
  'task.detail.title': '任务详情',
  'task.detail.children': '子任务',
  'task.detail.evidenceSource': '证据来源',
  'task.timeReview.evidence.reference_deadline_proven': '基准截止时间已证明未过期',
  'task.timeReview.evidence.reference_deadline_expired': '基准截止时间已证明已过期',
  'task.timeReview.evidence.inconclusive': '证据不足或无法证明',
  'task.detail.resultEvidence': '结果证据种类',
  'task.scope.authorizedSubset': '仅显示当前已授权的目标',
  'task.scope.unknown': '授权范围未知，已按最小投影显示',
  'task.mainState.queued': '排队中',
  'task.mainState.running': '运行中',
  'task.mainState.held': '等待核验',
  'task.mainState.completed': '已完成',
```

在 `apps/controller-web/src/locales/en.ts` 的 `task.reason.unknown` 之后追加同键英文条目：

```ts
  'common.actions': 'Actions',
  'common.cancel': 'Cancel',
  'nav.tasks': 'Tasks',
  'task.list.title': 'Tasks',
  'task.list.empty': 'No visible tasks',
  'task.list.loadMore': 'Load more',
  'task.list.columns.device': 'Device',
  'task.list.columns.state': 'State',
  'task.list.columns.revision': 'Revision',
  'task.preview.title': 'Batch reboot preview',
  'task.preview.selectDevices': 'Select devices',
  'task.preview.submit': 'Create preview',
  'task.preview.empty': 'No submittable target devices',
  'task.preview.eligible': 'Submittable',
  'task.preview.ineligible': 'Not submittable',
  'task.submitReboot': 'Confirm batch reboot submission',
  'task.security.csprngUnavailable': 'This browser lacks a cryptographically secure random source (crypto.getRandomValues); risky submissions are disabled.',
  'task.confirmRelease': 'Confirm lock release',
  'task.releaseLock': 'Release device lock',
  'task.field.releaseReason': 'Release reason',
  'task.risk.confirmBatchReboot': 'I reviewed the frozen target list and confirm this risky batch reboot',
  'task.risk.releaseLockTitle': 'Release the device lock held by an unknown task',
  'task.risk.releaseWarningDetail': 'The device may have already rebooted; releasing the lock only frees this claim and does not undo the reboot, nor does it resend it.',
  'task.risk.acknowledgeUnknown': 'I understand the result is still unconfirmed; releasing the lock does not undo the reboot',
  'task.held.reviewTitle': 'Held subtask time review',
  'task.held.reviewResumeOriginalDeadline': 'Resume the original deadline after review',
  'task.held.reviewExpire': 'Terminate as expired after review',
  'task.held.reviewExplain': 'Review relies on server evidence only: resume only if the original deadline is still proven valid, otherwise terminate. The deadline is never extended.',
  'task.held.reviewNoHeld': 'No held subtasks to review',
  'task.held.reviewSelfReported': 'The server result is authoritative; this UI never fabricates the audit fact',
  'task.cancel.action': 'Cancel task',
  'task.cancel.notCancellable': 'This task already passed dispatching; it must not be shown as cancelled',
  'task.detail.title': 'Task detail',
  'task.detail.children': 'Subtasks',
  'task.detail.evidenceSource': 'Evidence source',
  'task.timeReview.evidence.reference_deadline_proven': 'Reference deadline proven valid',
  'task.timeReview.evidence.reference_deadline_expired': 'Reference deadline proven expired',
  'task.timeReview.evidence.inconclusive': 'Inconclusive',
  'task.detail.resultEvidence': 'Result evidence kind',
  'task.scope.authorizedSubset': 'Only currently authorized targets are shown',
  'task.scope.unknown': 'Authorization scope unknown; showing the minimal projection',
  'task.mainState.queued': 'Queued',
  'task.mainState.running': 'Running',
  'task.mainState.held': 'Awaiting verification',
  'task.mainState.completed': 'Completed',
```

- [ ] **Step 4: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/i18n.test.ts src/task-presentation.test.ts`

预期：PASS（含既有用例）。

- [ ] **Step 5: 提交**

```bash
git add apps/controller-web/src/locales/zh-CN.ts apps/controller-web/src/locales/en.ts apps/controller-web/src/i18n.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 补齐 Task 4 任务生命周期词典键（含主任务四态 task.mainState.*）与中英 1:1 等长断言"
```

---

### Task 3: `task-ops.ts` —— 任务 DTO、fail-closed 形状守卫与授权子集描述

**Files:**
- Create: `apps/controller-web/src/task-ops.ts`
- Create: `apps/controller-web/src/task-ops.test.ts`
- 参考（只读，不改）：`apps/controller-web/src/types.ts`（复用 `isU64DecimalString`、`isSafeOpaqueCursor`；该文件无完整 `TimeEvidence` 类型导出——`TaskTimeEvidence`/`TaskCounts` 在本任务 `task-ops.ts` 本地定义，不改任何真实文件）

**Interfaces:**
- Consumes: `types.ts` 的 `isU64DecimalString(value): value is string`、`isSafeOpaqueCursor(value): value is string`、`DeviceItem`。
- Produces（后续任务只允许用这些名字，不得改名）：
  ```ts
  export type TaskOperation = 'reboot'
  export type MainTaskState = 'queued' | 'running' | 'held' | 'completed'
  export type MainTaskOutcome = 'success' | 'cancelled' | 'unknown' | 'partial' | 'failed'
  export type ViewScope = 'full' | 'authorized_subset'
  export type PreviewReason = SpecReasonCode // 04 ReasonCode 12 值 SCREAMING_SNAKE_CASE（来源=Runtime Task 6 DTO，账本决议③）；null=eligible 无原因
  export type ResultEvidenceKind = 'attempted' | 'boot_transition_observed' | 'execution_rejected' | 'os_error' | 'journal_lost' | 'none'

  export interface TaskPreviewTarget { device_id: string; eligible: boolean; reason?: PreviewReason | null }
  export interface TaskPreview { preview_token: string; expires_in_ms: number; targets: TaskPreviewTarget[] }
  export interface TaskTimeEvidence {    // 05 §3 时间证据（JSON 形状与 src/time/evidence.rs 对齐；types.ts 无此导出，本地定义）
    system_wall_utc: string        // UTC RFC3339（Z 或 +00:00，1..9 位小数秒，含显式日历校验）
    reference_utc: string          // UTC RFC3339（Z 或 +00:00，1..9 位小数秒，含显式日历校验）
    quality: string                // ntp_valid/system_fallback/stale 或其它保守字符串；未知值不授予权限
    clock_epoch: string            // 规范 UUID（既有 CANONICAL_UUID，不新增 v4 约束）
    source: string | null
    sample_age_ms: string | null   // 规范 u64 十进制字符串；未知是 null，不是 0
    offset_ms: number | null       // i64 范围内整数（可负）；wire 保持 JSON number（evidence.rs Option<i64>）
    uncertainty_ms: number | null  // u64 范围内非负整数；wire 保持 JSON number（evidence.rs Option<u64>）
  }
  export const TASK_COUNT_KEYS: readonly ['queued', 'held', 'dispatching', 'accepted', 'verifying', 'succeeded', 'failed', 'unknown', 'cancelled', 'expired']
  export interface TaskCounts {    // 服务端当前授权全集的子任务十态计数（04 §3 十态、§4"计数按各 state 返回"）；值均为规范 u64 十进制字符串（零为 "0"）
    queued: string; held: string; dispatching: string; accepted: string; verifying: string
    succeeded: string; failed: string; unknown: string; cancelled: string; expired: string
  }
  export interface TaskItem {
    id: string                 // 实体键 id，规范小写 UUIDv4
    operation: TaskOperation
    state: MainTaskState
    outcome: MainTaskOutcome | null   // 非 completed 为 null；completed 为五词之一（04 §4）
    revision: string           // u64 十进制字符串
    created_time: TaskTimeEvidence    // 完整 TimeEvidence 对象（05 §3）；旧 string 形状不再接受
    counts: TaskCounts             // 十键全在；u64 十进制字符串
    view_scope?: ViewScope
    actions?: { can_cancel: boolean }   // 后端计算的取消可请求标记（当前身份/device.reboot/合格 queued-held）；缺失 = 只读任务兼容
  }
  export interface ReleaseLockResult {
    released: true
    state: 'unknown'
  }
  export interface SubTaskItem {
    id: string                 // 子任务 id，规范小写 UUIDv4（02 §1）；main_task_id/pre_boot_id/observed_boot_id 等 DB 内部列不升为 HTTP 字段
    device_id: string          // hex64
    state: SubTaskState        // 复用 task-presentation 的 10 状态
    reason_code: SpecReasonCode | null   // key 必含（值 nullable）：缺 key 或 undefined 抛 INVALID_REASON_CODE；JSON null = 无原因码（合法）
    revision: string
    result_evidence: ResultEvidenceKind | null   // 02 §1：授权可见的结果证据种类，key 必含（值 nullable）；JSON null = 证据未产生，'none' 是六值字符串（勿与 null 混淆）
    lock_released?: boolean             // 可选 fail-closed 兼容；后端投影不承诺输出，UI 不得据缺字段推危险授权
  }
  export interface TaskPage<T> { items: T[]; next_cursor: string | null }

  export interface AuthorizedSubsetNotice { kind: 'authorized_subset' | 'unknown'; key: string }

  export function assertTaskPreviewShape(data: unknown): asserts data is TaskPreview
  export function assertTaskItemShape(data: unknown): asserts data is TaskItem
  export function assertTaskTimeEvidence(data: unknown): asserts data is TaskTimeEvidence
  export function assertTaskCounts(data: unknown): asserts data is TaskCounts
  export function zeroTaskCounts(): TaskCounts
  export function baseTaskTimeEvidence(overrides?: Partial<TaskTimeEvidence>): TaskTimeEvidence
  export function assertTaskPageShape(data: unknown): asserts data is TaskPage<TaskItem>
  export function assertSubTaskItemShape(data: unknown): asserts data is SubTaskItem
  export function assertSubTaskPageShape(data: unknown): asserts data is TaskPage<SubTaskItem>
  export function assertReleaseLockResultShape(data: unknown): asserts data is ReleaseLockResult
  export function describeAuthorizedSubset(viewScope: unknown): AuthorizedSubsetNotice
  export function canRequestCancel(task: Pick<TaskItem, 'actions' | 'view_scope'>): boolean
  export function eligibleDeviceIds(preview: TaskPreview): string[]
  export function isCanonicalUuidV4(value: unknown): value is string   // 规范 UUIDv4 谓词（`TASK_ID_V4` 正则，无 `m` 标志：JS 中裸 `$` 即严格 end input）；由本任务内部守卫谓词升级为导出，供 Task 5 `submitTask` 响应 `task_id` 复用，不重复实现正则
  ```
- 守卫语义（fail-closed，全部抛 `Error('<CODE>')`，禁止静默规范化）：
  - `device_id` 必须匹配 `/^[0-9a-f]{64}$/`（否则 `INVALID_DEVICE_ID`）；MainTask `id` 必须匹配规范 UUIDv4（否则 `INVALID_TASK_ID`）；SubTask `id` 同样必须匹配规范 UUIDv4（否则 `INVALID_SUB_TASK_ID`，不放宽为非空 ≤128），`main_task_id`/`pre_boot_id`/`observed_boot_id` 等 DB 内部列不升为 HTTP 必需字段（出现即按扩展字段忽略）；`revision` 必须 `isU64DecimalString`（否则 `INVALID_REVISION`）。
  - `created_time` 必须是完整 `TaskTimeEvidence` 对象（05 §3；JSON 类型以 `src/time/evidence.rs` 为准）：`system_wall_utc`/`reference_utc` 为合法 UTC RFC3339（仅 `Z` 或 `+00:00`，1..9 位小数秒，且**显式日历校验**：月 1..12、日不超过该年该月实际天数（格里高利闰年规则，与 pinned chrono 的 `NaiveDate::from_ymd_opt` 一致）、时 0..23、分 0..59、秒 0..59，且仅 `23:59:60` 作为闰秒放行——依据 pinned chrono 0.4.45：`parse_rfc3339` 显式接受 `:60`（60→59+1s 纳秒，`src/format/parse.rs:230-233`），`write_rfc3339` 对已存闰秒会再输出 `:60`（`src/format/formatting.rs:528-531`），故后端 wire 可携带它；chrono 对 `00:00:60` 等其它位置的宽松接受被本守卫保守拒绝，标剩余待专家核，不扩为未批准设计）；`clock_epoch` 为规范 UUID（既有 CANONICAL_UUID 约束，不新增 v4 约束）；`quality` 为非空字符串（已知值 `ntp_valid`/`system_fallback`/`stale`，其它值按保守未知处理、不授予权限，不做降级映射）；`source` 为 `string|null`；`sample_age_ms` 为 `null` 或 `isU64DecimalString`；`offset_ms` 为 `null` 或 i64 范围内**整数**（可负）；`uncertainty_ms` 为 `null` 或 u64 范围内**非负整数**——两者 wire 均保持 JSON number（不擅改为 decimal 字符串），范围校验用已解码整数经 `BigInt` 与 64 位边界精确比较（整数双精度值转 `BigInt` 无损；不用 `Number.isSafeInteger` 拒绝 64 位 >2^53 合法大值并冒称完整兼容）；不声称原生 JSON number 保留 >2^53 精度，本展示证据不授予危险动作、任何精确安全决策不得基于已舍入值，完整大数往返精度标剩余（不自行新增依赖/codec）。**旧 string 形状**（如 `'2026-10-16T00:00:00Z'`）、缺字段、非 UTC 偏移、空格分隔、日历非法（非法月/日、非闰年 2 月 29、时分秒超范围、非 23:59 位置的闰秒）、小数/字符串 offset-uncertainty、非有限/负值一律抛 `INVALID_TIME_EVIDENCE`；未知是 null，不是 0。
  - `counts` 为必含对象，`queued`/`held`/`dispatching`/`accepted`/`verifying`/`succeeded`/`failed`/`unknown`/`cancelled`/`expired` 十键（子任务十态，04 §3）必须全在，且每个值必须 `isU64DecimalString`（零为 `"0"`；JSON 数字、负数、前导零、u64 溢出均非法）；响应扩展字段忽略（不拒绝）；缺键或值非法抛 `INVALID_TASK_COUNTS`。
  - `ReleaseLockResult` 严格校验 `released === true` 与 `state === 'unknown'`（否则分别抛 `INVALID_RELEASE_LOCK_RELEASED` 与 `INVALID_RELEASE_LOCK_STATE`）；不得要求 `revision`/`lock_released` 等 SubTask 字段。
  - `next_cursor` 非 null 时必须 `isSafeOpaqueCursor`（否则 `INVALID_NEXT_CURSOR`）。
  - 未知 `state` / `reason_code` / `reason` / `result_evidence` / `view_scope` / `operation` 一律抛 `INVALID_*`，不做降级映射。
  - `result_evidence` 的 key 必含（值 nullable）：缺 key 或 `undefined` 均抛 `INVALID_RESULT_EVIDENCE`；非 null 必须 ∈ `ResultEvidenceKind` 六值 snake_case 字符串（否则 `INVALID_RESULT_EVIDENCE`）；`'none'` 是枚举字符串（与 `Option None` 序列化的 JSON `null` 不同义）；响应扩展字段（含 `execution_token`、`main_task_id`）忽略不拒绝（02 §1：响应允许扩展字段，客户端忽略未知字段）。
  - 主任务 `state`/`outcome` 配对（04 §4）：`state` 只允许 `queued/running/held/completed` 四态（子任务十态词出现即抛 `INVALID_MAIN_TASK_STATE`）；`state === 'completed'` 时 `outcome` 必须为 `success/cancelled/unknown/partial/failed` 五词之一，非 `completed` 时 `outcome` 必须为 `null`，违例均抛 `INVALID_MAIN_TASK_OUTCOME`，不做降级映射。
  - `sub_tasks` 的 `reason_code` 的 key 必含（值 nullable）：缺 key 或 `undefined` 均抛 `INVALID_REASON_CODE`；非 null 必须 ∈ `SPEC_REASON_CODES`（来自 `task-presentation.ts`）。
  - `describeAuthorizedSubset`：`view_scope === 'authorized_subset'` → `{ kind: 'authorized_subset', key: 'task.scope.authorizedSubset' }`；其它任何值（含 `undefined`）→ `{ kind: 'unknown', key: 'task.scope.unknown' }`。绝不返回"完整授权"语义。**本合同不改变该函数的文案行为**：`full` 仍按保守的 `task.scope.unknown` 文案呈现，不新增 i18n 键；取消入口的 scope 合法性只由 `canRequestCancel` 判定。
  - `actions` 为可选字段（只读任务兼容其缺失，形状守卫不抛错）；一旦存在必须是纯对象且 `can_cancel` 必须为布尔，否则抛 `INVALID_TASK_ACTIONS`；不检查多余字段是否缺席，不新增 `can_time_review` 之类的 action 点。
  - `canRequestCancel`：`view_scope` ∈ `{ 'full', 'authorized_subset' }` 且 `actions?.can_cancel === true` → `true`，其余一律 `false`；是输入 task 的 `actions`/`view_scope` 上的纯函数（入参类型 `Pick<TaskItem, 'actions' | 'view_scope'>`，结构上排除其余字段），不看主任务状态、不接 children 或任何权限数组；"可请求"不保成功，服务端事务为准。
  - `eligibleDeviceIds` 只返回 `eligible === true` 的 `device_id`，保持服务端顺序，去重后返回。

- [ ] **Step 1: 建立可编译桩代码**

`apps/controller-web/src/task-ops.ts`：

```ts
import type { SpecReasonCode, SubTaskState } from './task-presentation'

export type TaskOperation = 'reboot'
export type MainTaskState = 'queued' | 'running' | 'held' | 'completed'
export type MainTaskOutcome = 'success' | 'cancelled' | 'unknown' | 'partial' | 'failed'
export type ViewScope = 'full' | 'authorized_subset'
export type PreviewReason = SpecReasonCode // 04 ReasonCode 12 值 SCREAMING_SNAKE_CASE（来源=Runtime Task 6 DTO，账本决议③）
export type ResultEvidenceKind = 'attempted' | 'boot_transition_observed' | 'execution_rejected' | 'os_error' | 'journal_lost' | 'none'

export interface TaskPreviewTarget { device_id: string; eligible: boolean; reason?: PreviewReason | null }
export interface TaskPreview { preview_token: string; expires_in_ms: number; targets: TaskPreviewTarget[] }
export interface TaskTimeEvidence {
  system_wall_utc: string
  reference_utc: string
  quality: string
  clock_epoch: string
  source: string | null
  sample_age_ms: string | null
  offset_ms: number | null
  uncertainty_ms: number | null
}
export const TASK_COUNT_KEYS = ['queued', 'held', 'dispatching', 'accepted', 'verifying', 'succeeded', 'failed', 'unknown', 'cancelled', 'expired'] as const
export interface TaskCounts {
  queued: string; held: string; dispatching: string; accepted: string; verifying: string
  succeeded: string; failed: string; unknown: string; cancelled: string; expired: string
}
export interface TaskItem {
  id: string
  operation: TaskOperation
  state: MainTaskState
  outcome: MainTaskOutcome | null
  revision: string
  created_time: TaskTimeEvidence
  counts: TaskCounts
  view_scope?: ViewScope
  actions?: { can_cancel: boolean }
}
export interface ReleaseLockResult {
  released: true
  state: 'unknown'
}
export interface SubTaskItem {
  id: string
  device_id: string
  state: SubTaskState
  reason_code: SpecReasonCode | null          // key 必含（值 nullable）；缺 key/undefined 按既有守卫拒绝
  revision: string
  result_evidence: ResultEvidenceKind | null  // key 必含（值 nullable）；缺 key/undefined 按既有守卫拒绝
  lock_released?: boolean
}
export interface TaskPage<T> { items: T[]; next_cursor: string | null }
export interface AuthorizedSubsetNotice { kind: 'authorized_subset' | 'unknown'; key: string }

// 数据构造纯函数：桩阶段即返回真实基线值（供各任务 fixture 复用，形状守卫才负责校验）
export function zeroTaskCounts(): TaskCounts {
  return {
    queued: '0', held: '0', dispatching: '0', accepted: '0', verifying: '0',
    succeeded: '0', failed: '0', unknown: '0', cancelled: '0', expired: '0',
  }
}
export function baseTaskTimeEvidence(overrides: Partial<TaskTimeEvidence> = {}): TaskTimeEvidence {
  return {
    system_wall_utc: '2026-10-16T00:00:00Z',
    reference_utc: '2026-10-16T00:00:00Z',
    quality: 'system_fallback',
    clock_epoch: '3f6a6f00-4a10-4e2a-b0a2-4b0f00000001',
    source: null,
    sample_age_ms: null,
    offset_ms: null,
    uncertainty_ms: null,
    ...overrides,
  }
}
export function assertTaskPreviewShape(_data: unknown): asserts _data is TaskPreview {}
export function assertTaskItemShape(_data: unknown): asserts _data is TaskItem {}
export function assertTaskTimeEvidence(_data: unknown): asserts _data is TaskTimeEvidence {}
export function assertTaskCounts(_data: unknown): asserts _data is TaskCounts {}
export function assertTaskPageShape(_data: unknown): asserts _data is TaskPage<TaskItem> {}
export function assertSubTaskItemShape(_data: unknown): asserts _data is SubTaskItem {}
export function assertSubTaskPageShape(_data: unknown): asserts _data is TaskPage<SubTaskItem> {}
export function assertReleaseLockResultShape(_data: unknown): asserts _data is ReleaseLockResult {}
export function describeAuthorizedSubset(_viewScope: unknown): AuthorizedSubsetNotice {
  return { kind: 'unknown', key: '' }
}
export function canRequestCancel(_task: Pick<TaskItem, 'actions' | 'view_scope'>): boolean { return false }
export function eligibleDeviceIds(_preview: TaskPreview): string[] { return [] }
```

- [ ] **Step 2: 编写失败测试**

`apps/controller-web/src/task-ops.test.ts`：

```ts
import { describe, expect, it } from 'vitest'
import {
  assertReleaseLockResultShape,
  assertSubTaskItemShape,
  assertSubTaskPageShape,
  assertTaskItemShape,
  assertTaskPageShape,
  assertTaskPreviewShape,
  baseTaskTimeEvidence,
  canRequestCancel,
  describeAuthorizedSubset,
  eligibleDeviceIds,
  zeroTaskCounts,
  type ReleaseLockResult,
  type SubTaskItem,
  type TaskItem,
  type TaskTimeEvidence,
  type TaskPreview,
} from './task-ops'

const DEV_A = 'a'.repeat(64)
const DEV_B = 'b'.repeat(64)
const TASK_ID = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const SUB_ID = '123e4567-e89b-42d3-a456-426614174000'

function mainTask(overrides: Partial<TaskItem> = {}): TaskItem {
  return { id: TASK_ID, operation: 'reboot', state: 'queued', outcome: null, revision: '7',
    created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(),
    view_scope: 'authorized_subset', ...overrides }
}

// 最小 6 字段合同（02 §1:24）：id/device_id/state/reason_code/revision/result_evidence；
// 不含 main_task_id/lock_released（DB 内部列/后端不承诺输出的可选兼容字段）
function subTask(overrides: Partial<SubTaskItem> = {}): SubTaskItem {
  return { id: SUB_ID, device_id: DEV_A, state: 'held',
    reason_code: 'TIME_UNCERTAIN', revision: '3', result_evidence: null, ...overrides }
}

const preview: TaskPreview = {
  preview_token: 'tok-1', expires_in_ms: 60000,
  targets: [
    { device_id: DEV_A, eligible: true, reason: null },
    { device_id: DEV_B, eligible: false, reason: 'DEVICE_BUSY' },
    { device_id: DEV_B, eligible: false, reason: 'PERMISSION_DENIED' },
  ],
}

describe('task-ops shape guards are fail-closed', () => {
  it('accepts a well-formed preview and rejects unknown reason vocabulary', () => {
    expect(() => assertTaskPreviewShape(preview)).not.toThrow()
    // 真实 producer 正例（来源=Runtime Task 6 DTO，账本决议③）：SCREAMING ReasonCode 与 eligible 的 reason:null 均放行
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: DEV_A, eligible: false, reason: 'DEVICE_OFFLINE' }] })).not.toThrow()
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: DEV_A, eligible: true, reason: null }] })).not.toThrow()
    // 负例：非法字符串仍抛 INVALID_PREVIEW_REASON
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: DEV_A, eligible: false, reason: 'maybe_later' }] }))
      .toThrowError('INVALID_PREVIEW_REASON')
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: 'A'.repeat(64), eligible: true }] }))
      .toThrowError('INVALID_DEVICE_ID')
    expect(() => assertTaskPreviewShape({ ...preview, expires_in_ms: 0 })).toThrowError('INVALID_PREVIEW_EXPIRY')
  })

  it('rejects non-u64 revision, non-v4 task id and unknown state', () => {
    expect(() => assertTaskItemShape(mainTask({ revision: '07' }))).toThrowError('INVALID_REVISION')
    expect(() => assertTaskItemShape(mainTask({ id: 'not-a-uuid' }))).toThrowError('INVALID_TASK_ID')
    expect(() => assertTaskItemShape(mainTask({ state: 'done' as never }))).toThrowError('INVALID_MAIN_TASK_STATE')
    expect(() => assertTaskItemShape(mainTask({ operation: 'shutdown' as never }))).toThrowError('INVALID_TASK_OPERATION')
    // view_scope 是可选字段，但一旦出现必须是合法值（full / authorized_subset）
    expect(() => assertTaskItemShape(mainTask({ view_scope: 'all' as never }))).toThrowError('INVALID_VIEW_SCOPE')
    expect(() => assertTaskItemShape(mainTask({ view_scope: 'full' }))).not.toThrow()
    // actions 可选（只读任务兼容缺失）；一旦存在 can_cancel 必须是布尔（fail-closed，不做降级映射）
    expect(() => assertTaskItemShape(mainTask({ actions: { can_cancel: true } }))).not.toThrow()
    expect(() => assertTaskItemShape(mainTask({ actions: { can_cancel: 'yes' as never } }))).toThrowError('INVALID_TASK_ACTIONS')
    expect(() => assertTaskItemShape(mainTask({ actions: {} as never }))).toThrowError('INVALID_TASK_ACTIONS')
    expect(() => assertTaskItemShape(mainTask({ actions: null as never }))).toThrowError('INVALID_TASK_ACTIONS')
  })

  it('accepts the four main states with the 04 §4 state/outcome pairing', () => {
    for (const state of ['queued', 'running', 'held'] as const) {
      expect(() => assertTaskItemShape(mainTask({ state, outcome: null }))).not.toThrow()
    }
    for (const outcome of ['success', 'cancelled', 'unknown', 'partial', 'failed'] as const) {
      expect(() => assertTaskItemShape(mainTask({ state: 'completed', outcome }))).not.toThrow()
    }
  })

  it('rejects child states as main states and any state/outcome mispairing', () => {
    for (const state of ['dispatching', 'accepted', 'verifying', 'succeeded', 'failed', 'unknown', 'cancelled', 'expired'] as const) {
      expect(() => assertTaskItemShape(mainTask({ state: state as never, outcome: null })))
        .toThrowError('INVALID_MAIN_TASK_STATE')
    }
    expect(() => assertTaskItemShape(mainTask({ state: 'running', outcome: 'success' })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
    expect(() => assertTaskItemShape(mainTask({ state: 'completed' })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
    expect(() => assertTaskItemShape(mainTask({ state: 'completed', outcome: null })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
    expect(() => assertTaskItemShape(mainTask({ state: 'completed', outcome: 'succeeded' as never })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
  })

  it('rejects a page whose cursor is not a safe opaque string, unknown subtask reason codes, and malformed result evidence', () => {
    expect(() => assertTaskPageShape({ items: [mainTask()], next_cursor: 'bad\u0000cursor' }))
      .toThrowError('INVALID_NEXT_CURSOR')
    expect(() => assertTaskPageShape({ items: [mainTask()], next_cursor: null })).not.toThrow()
    // 后端最小 6 字段合法（02 §1:24 SubTask 投影）：result_evidence 为 JSON null（证据未产生）亦合法；
    // main_task_id/pre_boot_id/observed_boot_id/evidence_source 不再是 HTTP 必需字段
    expect(() => assertSubTaskItemShape(subTask())).not.toThrow()
    expect(() => assertSubTaskItemShape(subTask({ result_evidence: null }))).not.toThrow()
    // 六 enum 全合法；'none' 是 EvidenceKind 枚举字符串，与 Option None → JSON null 不同义
    for (const evidence of ['attempted', 'boot_transition_observed', 'execution_rejected', 'os_error', 'journal_lost', 'none'] as const) {
      expect(() => assertSubTaskItemShape(subTask({ result_evidence: evidence }))).not.toThrow()
    }
    // 响应扩展字段（含 execution_token、main_task_id）忽略不拒绝；守卫不将其用于展示或安全判断
    //（UI 侧同样不得消费，见 Task 6/9 渲染契约）
    expect(() => assertSubTaskItemShape({ ...subTask(), execution_token: 'tok', main_task_id: TASK_ID })).not.toThrow()
    // 非法证据：旧来源词表 controller/board、PascalCase、unknown 枚举、对象（勿把完整 wire 证据对象当六值字符串）、数/布尔
    for (const badEvidence of ['controller', 'board', 'BootTransitionObserved', 'FULL_TASK_EVIDENCE', {}, 7, true] as Array<unknown>) {
      expect(() => assertSubTaskItemShape({ ...subTask(), result_evidence: badEvidence as never }))
        .toThrowError('INVALID_RESULT_EVIDENCE')
    }
    // id 为规范 UUIDv4（不放宽为非空 ≤128）
    expect(() => assertSubTaskItemShape(subTask({ id: 'not-a-uuid' }))).toThrowError('INVALID_SUB_TASK_ID')
    // 未知 reason_code / 未知 state 按既有守卫关闭
    expect(() => assertSubTaskItemShape(subTask({ reason_code: 'MADE_UP' as never })))
      .toThrowError('INVALID_REASON_CODE')
    expect(() => assertSubTaskPageShape({ items: [subTask({ state: 'restarting' as never })], next_cursor: null }))
      .toThrowError('INVALID_SUB_TASK_STATE')
    // 6 字段合同：reason_code/result_evidence 的 key 必含（值 nullable）：缺 key 或 undefined 按既有守卫拒绝，合法 null 仍合法
    const missingReason: Record<string, unknown> = { ...subTask() }; delete missingReason.reason_code
    const missingEvidence: Record<string, unknown> = { ...subTask() }; delete missingEvidence.result_evidence
    expect(() => assertSubTaskItemShape(missingReason)).toThrowError('INVALID_REASON_CODE')
    expect(() => assertSubTaskItemShape(subTask({ reason_code: undefined as never }))).toThrowError('INVALID_REASON_CODE')
    expect(() => assertSubTaskItemShape(missingEvidence)).toThrowError('INVALID_RESULT_EVIDENCE')
    expect(() => assertSubTaskItemShape(subTask({ result_evidence: undefined as never }))).toThrowError('INVALID_RESULT_EVIDENCE')
    expect(() => assertSubTaskItemShape(subTask({ reason_code: null, result_evidence: null }))).not.toThrow()
    // lock_released 保留可选 fail-closed 兼容：出现必须是布尔（后端投影不承诺输出该字段）
    expect(() => assertSubTaskItemShape(subTask({ lock_released: true }))).not.toThrow()
    expect(() => assertSubTaskItemShape(subTask({ lock_released: 'yes' as never })))
      .toThrowError('INVALID_LOCK_RELEASED')
  })

  it('validates release lock response strictly containing only released:true and state:unknown', () => {
    const valid: ReleaseLockResult = { released: true, state: 'unknown' }
    expect(() => assertReleaseLockResultShape(valid)).not.toThrow()
    expect(() => assertReleaseLockResultShape({ released: false, state: 'unknown' }))
      .toThrowError('INVALID_RELEASE_LOCK_RELEASED')
    expect(() => assertReleaseLockResultShape({ released: true, state: 'succeeded' }))
      .toThrowError('INVALID_RELEASE_LOCK_STATE')
    expect(() => assertReleaseLockResultShape({ state: 'unknown' }))
      .toThrowError('INVALID_RELEASE_LOCK_RELEASED')
  })

  it('accepts a legal full TimeEvidence and ten-key u64-string counts (05 §3 / 04 §4)', () => {
    // 正例：Z 与 +00:00、小数秒、已知与保守未知 quality、全 null、负 offset、零与 u64 上界计数
    expect(() => assertTaskItemShape(mainTask({
      created_time: {
        system_wall_utc: '2026-10-16T00:00:00+00:00',
        reference_utc: '2026-10-16T00:00:00.250Z',
        quality: 'ntp_valid',
        clock_epoch: '0f8fad5b-d9cb-469f-a165-70867728950e',
        source: 'ops-ref-1', sample_age_ms: '0', offset_ms: -5, uncertainty_ms: 0,
      },
      counts: { ...zeroTaskCounts(), held: '3', succeeded: '18446744073709551615' },
    }))).not.toThrow()
    // 正例（日历/闰秒/64 位边界）：闰年 2 月 29（含 <100 的四位年 0004）、9 位纳秒、闰秒 23:59:60、
    // i64 下界 offset（双精度精确可表示）、>2^53 大整数 uncertainty（2^53，不冒称 >2^53 无损往返）
    expect(() => assertTaskItemShape(mainTask({
      created_time: baseTaskTimeEvidence({
        system_wall_utc: '0004-02-29T23:59:60.123456789Z',
        reference_utc: '2024-02-29T23:59:59.999+00:00',
        offset_ms: -9223372036854775808,
        uncertainty_ms: 2 ** 53,
      }),
    }))).not.toThrow()
    expect(() => assertTaskItemShape(mainTask({
      created_time: baseTaskTimeEvidence({ quality: 'degraded_future' }),
    }))).not.toThrow()
  })

  it('rejects legacy string created_time and malformed TimeEvidence fields with INVALID_TIME_EVIDENCE', () => {
    // 旧 DTO 的 string 形状必须不再被接受（case 表 for 只算本 it 一个方法）
    expect(() => assertTaskItemShape(mainTask({ created_time: '2026-10-16T00:00:00Z' as never })))
      .toThrowError('INVALID_TIME_EVIDENCE')
    for (const bad of [
      { reference_utc: '2026-10-16T08:00:00+08:00' },   // 非 UTC 偏移
      { system_wall_utc: '2026-10-16 00:00:00Z' },      // 空格分隔而非 T
      { system_wall_utc: '2026-13-01T00:00:00Z' },      // 非法月 13
      { reference_utc: '2026-00-10T00:00:00Z' },        // 非法月 0
      { system_wall_utc: '2026-02-30T00:00:00Z' },      // 2 月 30（日历校验不得依赖 Date.parse 归一化）
      { reference_utc: '2026-02-29T00:00:00Z' },        // 非闰年 2 月 29
      { system_wall_utc: '2026-10-16T24:00:00Z' },      // 超范围时 24
      { reference_utc: '2026-10-16T00:60:00Z' },        // 超范围分 60
      { system_wall_utc: '2026-10-16T23:59:61Z' },      // 超范围秒 61
      { reference_utc: '2026-10-16T00:00:60Z' },        // 闰秒 :60 位于非 23:59（保守边界，见 Step 4）
      { clock_epoch: 'not-a-uuid' },
      { quality: 3 as never },
      { source: 7 as never },
      { sample_age_ms: '01' },                           // 前导零
      { offset_ms: Number.POSITIVE_INFINITY },           // 非有限
      { offset_ms: 1.5 },                                // 小数（i64 要求整数）
      { offset_ms: '5' as never },                       // JSON 字符串而非数字
      { uncertainty_ms: -1 },                            // 负数
      { uncertainty_ms: 0.5 },                           // 小数（u64 要求非负整数）
    ] as Array<Partial<TaskTimeEvidence>>) {
      expect(() => assertTaskItemShape(mainTask({ created_time: baseTaskTimeEvidence(bad) })))
        .toThrowError('INVALID_TIME_EVIDENCE')
    }
  })

  it('rejects missing, numeric, negative, leading-zero and overflowing counts with INVALID_TASK_COUNTS', () => {
    for (const badCounts of [
      { ...zeroTaskCounts(), held: undefined as never },                  // 缺键
      { ...zeroTaskCounts(), queued: 7 as never },                        // JSON 数字而非字符串
      { ...zeroTaskCounts(), failed: '-1' },                              // 负数
      { ...zeroTaskCounts(), cancelled: '007' },                          // 前导零
      { ...zeroTaskCounts(), expired: '18446744073709551616' },           // u64 溢出
    ]) {
      expect(() => assertTaskItemShape(mainTask({ counts: badCounts })))
        .toThrowError('INVALID_TASK_COUNTS')
    }
  })
})

describe('authorized subset projection', () => {
  it('marks an explicit authorized_subset scope and treats anything else as unknown scope', () => {
    expect(describeAuthorizedSubset('authorized_subset'))
      .toEqual({ kind: 'authorized_subset', key: 'task.scope.authorizedSubset' })
    for (const value of [undefined, null, '', 'all', 'full', 1]) {
      expect(describeAuthorizedSubset(value)).toEqual({ kind: 'unknown', key: 'task.scope.unknown' })
    }
  })

  it('never claims completeness: unknown scope must not reuse the authorized subset key', () => {
    expect(describeAuthorizedSubset(undefined).key).not.toBe('task.scope.authorizedSubset')
  })

  it('selects only eligible device ids, de-duplicated and order-preserving', () => {
    const p: TaskPreview = {
      preview_token: 'tok-2', expires_in_ms: 60000,
      targets: [
        { device_id: DEV_A, eligible: true },
        { device_id: DEV_B, eligible: false, reason: 'DEVICE_OFFLINE' },
        { device_id: DEV_A, eligible: true },
      ],
    }
    expect(eligibleDeviceIds(p)).toEqual([DEV_A])
    expect(eligibleDeviceIds({ ...p, targets: [] })).toEqual([])
  })
})

describe('canRequestCancel pure helper', () => {
  it('opens for a running main task when the backend reports can_cancel true (RED 正例，先失败)', () => {
    expect(canRequestCancel(mainTask({ state: 'running', actions: { can_cancel: true } }))).toBe(true)
  })

  it('keeps an authorized_subset completed main task open when the backend allows', () => {
    expect(canRequestCancel(mainTask({ state: 'completed', outcome: 'cancelled', actions: { can_cancel: true } }))).toBe(true)
  })

  it('opens under a legitimate full scope when the backend allows', () => {
    expect(canRequestCancel(mainTask({ view_scope: 'full', actions: { can_cancel: true } }))).toBe(true)
  })

  it('closes on backend false, missing actions, or malformed can_cancel regardless of main state', () => {
    expect(canRequestCancel(mainTask({ state: 'queued', actions: { can_cancel: false } }))).toBe(false)
    expect(canRequestCancel(mainTask({ state: 'running', actions: undefined }))).toBe(false)
    expect(canRequestCancel(mainTask({ state: 'running', actions: { can_cancel: 'yes' as never } }))).toBe(false)
  })

  it('closes the dangerous entry for unknown or missing view_scope even when can_cancel is true', () => {
    expect(canRequestCancel(mainTask({ view_scope: 'legacy' as never, actions: { can_cancel: true } }))).toBe(false)
    expect(canRequestCancel(mainTask({ view_scope: undefined, actions: { can_cancel: true } }))).toBe(false)
  })
})
```

- [ ] **Step 3: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/task-ops.test.ts`

预期：文件可编译加载，运行 17 个用例（原 9 + canRequestCancel 5 + 本次 TimeEvidence/counts 3，不虚报；子任务 6 字段合同断言并入既有 it，不新增 it 计数），至少 12 个失败，报错形如
`AssertionError: expected [Function] to throw error 'INVALID_PREVIEW_REASON' but it threw no error`、
`expected [Function] to throw error 'INVALID_TIME_EVIDENCE' but it threw no error`（旧 string `created_time` 与 19 种畸形证据变体，含非法月/日、非闰年 2 月 29、时分秒超范围、闰秒边界、小数/字符串 offset-uncertainty）、
`expected [Function] to throw error 'INVALID_TASK_COUNTS' but it threw no error`（缺键 / JSON 数字 / 负数 / 前导零 / u64 溢出）、
`expected [Function] to throw error 'INVALID_RESULT_EVIDENCE' but it threw no error`（旧来源词表 controller/board、PascalCase、unknown 枚举、对象/数/布尔共 7 种畸形证据变体）、
`expected [Function] to throw error 'INVALID_SUB_TASK_ID' but it threw no error`（id 非规范 UUIDv4）、
`expected [Function] to throw error 'INVALID_LOCK_RELEASED' but it threw no error`（`lock_released` 为字符串）、
`expected { kind: 'unknown', key: '' } to deeply equal { kind: 'authorized_subset', ... }` 与
`expected false to be true`（`canRequestCancel` 正例在恒 false 桩下先失败，首个正例即 running + true）。
（有效行为 RED：非 todo / 缺符号 / 0 tests；新 case 表 `for` 循环（TimeEvidence、counts、result_evidence 六正例与七负例）各按所在 `it` 计一次，不按案例数虚增。）

- [ ] **Step 4: 最小实现**

把 `apps/controller-web/src/task-ops.ts` 的桩替换为真实实现（保持上面 Interfaces 里的全部签名）：

```ts
import { SUBTASK_STATES, SPEC_REASON_CODES, type SpecReasonCode, type SubTaskState } from './task-presentation'
import { isSafeOpaqueCursor, isU64DecimalString } from './types'

const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/
const TASK_ID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
const RFC3339_UTC = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d{1,9})?(?:Z|\+00:00)$/
const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/

const MAIN_TASK_STATES = new Set<string>([
  'queued', 'running', 'held', 'completed',
])
const MAIN_TASK_OUTCOMES = new Set<string>(['success', 'cancelled', 'unknown', 'partial', 'failed'])
const SUB_TASK_STATES = new Set<string>(SUBTASK_STATES)
const REASON_CODES = new Set<string>(SPEC_REASON_CODES)
// preview reason 消费词表 = 04 ReasonCode 12 值 SCREAMING_SNAKE_CASE（来源=Runtime Task 6 DTO，账本决议③）；
// null/缺省放行（eligible 目标合法，见 assertTarget 的 !== null 分支），非法字符串仍抛 INVALID_PREVIEW_REASON
const PREVIEW_REASONS = new Set<string>(SPEC_REASON_CODES)
// EvidenceKind 六值（Runtime 计划 Task 2）的 snake_case 字符串词表；'none' 是枚举串，勿与 JSON null 混淆
const RESULT_EVIDENCE_KINDS = new Set<string>([
  'attempted', 'boot_transition_observed', 'execution_rejected', 'os_error', 'journal_lost', 'none',
])
const OPERATIONS = new Set<string>(['reboot'])
const VIEW_SCOPES = new Set<string>(['full', 'authorized_subset'])

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function fail(code: string): never {
  throw new Error(code)
}

export function isCanonicalUuidV4(value: unknown): value is string {
  return typeof value === 'string' && TASK_ID_V4.test(value)
}

function assertTarget(value: unknown): void {
  if (!isRecord(value)) fail('INVALID_PREVIEW_TARGET')
  if (typeof value.device_id !== 'string' || !DEVICE_ID_HEX64.test(value.device_id)) fail('INVALID_DEVICE_ID')
  if (typeof value.eligible !== 'boolean') fail('INVALID_PREVIEW_ELIGIBLE')
  if (value.reason !== undefined && value.reason !== null && (typeof value.reason !== 'string' || !PREVIEW_REASONS.has(value.reason))) {
    fail('INVALID_PREVIEW_REASON')
  }
}

export interface TaskTimeEvidence {
  system_wall_utc: string
  reference_utc: string
  quality: string
  clock_epoch: string
  source: string | null
  sample_age_ms: string | null
  offset_ms: number | null
  uncertainty_ms: number | null
}

export const TASK_COUNT_KEYS = [
  'queued', 'held', 'dispatching', 'accepted', 'verifying',
  'succeeded', 'failed', 'unknown', 'cancelled', 'expired',
] as const

export interface TaskCounts {
  queued: string; held: string; dispatching: string; accepted: string; verifying: string
  succeeded: string; failed: string; unknown: string; cancelled: string; expired: string
}

export interface TaskItem {
  id: string
  operation: TaskOperation
  state: MainTaskState
  outcome: MainTaskOutcome | null
  revision: string
  created_time: TaskTimeEvidence
  counts: TaskCounts
  view_scope?: ViewScope
  actions?: { can_cancel: boolean }   // 后端计算的取消可请求标记；可选，形状校验见 assertTaskItemShape
}

export interface ReleaseLockResult {
  released: true
  state: 'unknown'
}

export function assertTaskPreviewShape(data: unknown): asserts data is TaskPreview {
  if (!isRecord(data)) fail('INVALID_TASK_PREVIEW')
  if (typeof data.preview_token !== 'string' || data.preview_token.length === 0 || data.preview_token.length > 512) {
    fail('INVALID_PREVIEW_TOKEN')
  }
  if (typeof data.expires_in_ms !== 'number' || !Number.isInteger(data.expires_in_ms) ||
      data.expires_in_ms <= 0 || data.expires_in_ms > 600000) {
    fail('INVALID_PREVIEW_EXPIRY')
  }
  if (!Array.isArray(data.targets) || data.targets.length === 0) fail('INVALID_PREVIEW_TARGETS')
  data.targets.forEach(assertTarget)
}

export function assertTaskItemShape(data: unknown): asserts data is TaskItem {
  if (!isRecord(data)) fail('INVALID_MAIN_TASK')
  if (!isCanonicalUuidV4(data.id)) fail('INVALID_TASK_ID')
  if (typeof data.operation !== 'string' || !OPERATIONS.has(data.operation)) fail('INVALID_TASK_OPERATION')
  if (typeof data.state !== 'string' || !MAIN_TASK_STATES.has(data.state)) fail('INVALID_MAIN_TASK_STATE')
  if (data.state === 'completed') {
    if (typeof data.outcome !== 'string' || !MAIN_TASK_OUTCOMES.has(data.outcome)) fail('INVALID_MAIN_TASK_OUTCOME')
  } else if (data.outcome !== null) {
    fail('INVALID_MAIN_TASK_OUTCOME')
  }
  if (!isU64DecimalString(data.revision)) fail('INVALID_REVISION')
  assertTaskTimeEvidence(data.created_time)
  assertTaskCounts(data.counts)
  if (data.view_scope !== undefined && (typeof data.view_scope !== 'string' || !VIEW_SCOPES.has(data.view_scope))) {
    fail('INVALID_VIEW_SCOPE')
  }
  if (data.actions !== undefined && (!isRecord(data.actions) || typeof data.actions.can_cancel !== 'boolean')) {
    fail('INVALID_TASK_ACTIONS')
  }
}

// 格里高利闰年规则（含世纪/400 年规则），与 pinned chrono 0.4.45 NaiveDate::from_ymd_opt 一致
function isLeapYear(year: number): boolean {
  return year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0)
}

function daysInMonth(year: number, month: number): number {
  switch (month) {
    case 2: return isLeapYear(year) ? 29 : 28
    case 4: case 6: case 9: case 11: return 30
    default: return 31
  }
}

function isUtcRfc3339(value: unknown): value is string {
  if (typeof value !== 'string') return false
  const match = RFC3339_UTC.exec(value)
  if (!match) return false
  // 日历校验用显式数值比较，不用 Date.parse/Date.UTC：
  // 1) V8 会把越界日归一化（2026-02-30 → 3 月 2 日），其它引擎返回 NaN，跨引擎不一致；
  // 2) Date.UTC 会把 00..99 年映射到 1900..1999，而此处四位年为 0000..9999，直接按格里高利规则校验。
  const year = Number(match[1])
  const month = Number(match[2])
  const day = Number(match[3])
  const hour = Number(match[4])
  const minute = Number(match[5])
  const second = Number(match[6])
  if (month < 1 || month > 12) return false
  if (day < 1 || day > daysInMonth(year, month)) return false
  if (hour > 23 || minute > 59) return false
  // 闰秒政策（依据 pinned chrono 0.4.45，非臆造）：parse_rfc3339 显式允许 :60（60 → 59 + 1s 纳秒，
  // src/format/parse.rs:230-233），write_rfc3339 对已存闰秒会再输出 :60（src/format/formatting.rs:528-531），
  // 故后端 wire 可携带 23:59:60；边界：仅接受规范 23:59:60 位置，chrono 宽松接受的 00:00:60 等其它
  // 位置被保守拒绝（标剩余待专家核，不扩为未批准设计）。
  return second < 60 || (second === 60 && hour === 23 && minute === 59)
}

function isCanonicalUuid(value: unknown): value is string {
  return typeof value === 'string' && CANONICAL_UUID.test(value)
}

export function zeroTaskCounts(): TaskCounts {
  return {
    queued: '0', held: '0', dispatching: '0', accepted: '0', verifying: '0',
    succeeded: '0', failed: '0', unknown: '0', cancelled: '0', expired: '0',
  }
}

export function baseTaskTimeEvidence(overrides: Partial<TaskTimeEvidence> = {}): TaskTimeEvidence {
  return {
    system_wall_utc: '2026-10-16T00:00:00Z',
    reference_utc: '2026-10-16T00:00:00Z',
    quality: 'system_fallback',
    clock_epoch: '3f6a6f00-4a10-4e2a-b0a2-4b0f00000001',
    source: null,
    sample_age_ms: null,
    offset_ms: null,
    uncertainty_ms: null,
    ...overrides,
  }
}

// evidence.rs：offset_ms 为 Option<i64>、uncertainty_ms 为 Option<u64>，wire 保持 JSON number（不擅改 decimal 字符串）。
// 整数双精度值转 BigInt(value) 无损，与 64 位边界 BigInt 比较即精确；不用 Number.isSafeInteger
// （它会拒绝 64 位 >2^53 的合法大值并冒称"完整兼容"）。
// 剩余（本计划不扩为新设计）：|v| > 2^53 时 JSON.parse 已舍入到最近可表示值，wire 上合法 64 位值
// （如 2^63-1）会解码为 2^63 而在此被拒；本展示证据不授予危险动作，任何精确安全决策不得基于
// 已舍入值；解决需 wire decimal 字符串或专用 codec，待批准，不自行新增依赖。
const I64_MIN = -9223372036854775808n
const I64_MAX = 9223372036854775807n
const U64_MAX = 18446744073709551615n

function isI64Number(value: unknown): value is number {
  if (typeof value !== 'number' || !Number.isInteger(value)) return false
  const big = BigInt(value)
  return big >= I64_MIN && big <= I64_MAX
}

function isU64Number(value: unknown): value is number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) return false
  return BigInt(value) <= U64_MAX
}

export function assertTaskTimeEvidence(data: unknown): asserts data is TaskTimeEvidence {
  if (!isRecord(data)) fail('INVALID_TIME_EVIDENCE')
  if (!isUtcRfc3339(data.system_wall_utc)) fail('INVALID_TIME_EVIDENCE')
  if (!isUtcRfc3339(data.reference_utc)) fail('INVALID_TIME_EVIDENCE')
  if (!isCanonicalUuid(data.clock_epoch)) fail('INVALID_TIME_EVIDENCE')
  if (typeof data.quality !== 'string' || data.quality.length === 0) fail('INVALID_TIME_EVIDENCE')
  if (data.source !== null && typeof data.source !== 'string') fail('INVALID_TIME_EVIDENCE')
  if (data.sample_age_ms !== null && !isU64DecimalString(data.sample_age_ms)) fail('INVALID_TIME_EVIDENCE')
  if (data.offset_ms !== null && !isI64Number(data.offset_ms)) fail('INVALID_TIME_EVIDENCE')
  if (data.uncertainty_ms !== null && !isU64Number(data.uncertainty_ms)) fail('INVALID_TIME_EVIDENCE')
}

export function assertTaskCounts(data: unknown): asserts data is TaskCounts {
  if (!isRecord(data)) fail('INVALID_TASK_COUNTS')
  for (const key of TASK_COUNT_KEYS) {
    if (!isU64DecimalString(data[key])) fail('INVALID_TASK_COUNTS')
  }
}

export function assertReleaseLockResultShape(data: unknown): asserts data is ReleaseLockResult {
  if (!isRecord(data)) fail('INVALID_RELEASE_LOCK_RESULT')
  if (data.released !== true) fail('INVALID_RELEASE_LOCK_RELEASED')
  if (data.state !== 'unknown') fail('INVALID_RELEASE_LOCK_STATE')
}

export function assertSubTaskItemShape(data: unknown): asserts data is SubTaskItem {
  if (!isRecord(data)) fail('INVALID_SUB_TASK')
  // 02 §1:24 SubTask 投影 = 6 字段 id/device_id/state/reason_code/revision/result_evidence：
  // id 为规范 UUIDv4（不放宽为非空 ≤128）；main_task_id/pre_boot_id/observed_boot_id 等 DB 内部列
  // 不是 HTTP 必需字段；execution_token 及其摘要永不暴露（扩展字段忽略不拒绝）
  if (!isCanonicalUuidV4(data.id)) fail('INVALID_SUB_TASK_ID')
  if (typeof data.device_id !== 'string' || !DEVICE_ID_HEX64.test(data.device_id)) fail('INVALID_DEVICE_ID')
  if (typeof data.state !== 'string' || !SUB_TASK_STATES.has(data.state)) fail('INVALID_SUB_TASK_STATE')
  // reason_code：key 必含（值 nullable）：缺 key 或 undefined 按既有守卫拒绝，JSON null（无原因码）合法
  if (data.reason_code === undefined ||
      (data.reason_code !== null &&
       (typeof data.reason_code !== 'string' || !REASON_CODES.has(data.reason_code)))) {
    fail('INVALID_REASON_CODE')
  }
  if (!isU64DecimalString(data.revision)) fail('INVALID_REVISION')
  // result_evidence：key 必含（值 nullable）：缺 key 或 undefined 按既有守卫拒绝；JSON null = 证据未产生（Option None）
  // 'none' 是 EvidenceKind 六值枚举字符串之一（snake_case，勿与 null 混淆）；旧来源词表/PascalCase/对象/数/布尔一律拒绝
  //（nullable EvidenceKind 枚举串，不是完整 wire 证据对象）
  if (data.result_evidence === undefined ||
      (data.result_evidence !== null &&
       (typeof data.result_evidence !== 'string' || !RESULT_EVIDENCE_KINDS.has(data.result_evidence)))) {
    fail('INVALID_RESULT_EVIDENCE')
  }
  // lock_released：可选 fail-closed 兼容（出现必须是布尔）；后端投影不承诺输出该字段，
  // UI 不得据其缺失推断危险授权（unknown 释放锁按 state === 'unknown' 判定，04 §8）
  if (data.lock_released !== undefined && typeof data.lock_released !== 'boolean') fail('INVALID_LOCK_RELEASED')
}

function assertPage(data: unknown, assertItem: (item: unknown) => void): void {
  if (!isRecord(data) || !Array.isArray(data.items)) fail('INVALID_TASK_PAGE')
  if (data.next_cursor !== null && !isSafeOpaqueCursor(data.next_cursor)) fail('INVALID_NEXT_CURSOR')
  data.items.forEach(assertItem)
}

export function assertTaskPageShape(data: unknown): asserts data is TaskPage<TaskItem> {
  assertPage(data, assertTaskItemShape)
}

export function assertSubTaskPageShape(data: unknown): asserts data is TaskPage<SubTaskItem> {
  assertPage(data, assertSubTaskItemShape)
}

export function describeAuthorizedSubset(viewScope: unknown): AuthorizedSubsetNotice {
  if (viewScope === 'authorized_subset') {
    return { kind: 'authorized_subset', key: 'task.scope.authorizedSubset' }
  }
  return { kind: 'unknown', key: 'task.scope.unknown' }
}

export function canRequestCancel(task: Pick<TaskItem, 'actions' | 'view_scope'>): boolean {
  if (task.view_scope !== 'full' && task.view_scope !== 'authorized_subset') return false
  return task.actions?.can_cancel === true
}

export function eligibleDeviceIds(preview: TaskPreview): string[] {
  const seen = new Set<string>()
  const result: string[] = []
  for (const target of preview.targets) {
    if (target.eligible && !seen.has(target.device_id)) {
      seen.add(target.device_id)
      result.push(target.device_id)
    }
  }
  return result
}
```

- [ ] **Step 5: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/task-ops.test.ts`

预期：PASS（17 个用例全绿；计数与原 9 用例之差恰为新增 8 个——canRequestCancel 5 + TimeEvidence/counts 3，不虚报）。

- [ ] **Step 6: 提交**

```bash
git add apps/controller-web/src/task-ops.ts apps/controller-web/src/task-ops.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 新增 fail-closed 任务 DTO 形状守卫（主任务四态与 outcome 配对、ViewScope full|authorized_subset、可选 actions.can_cancel）、授权子集投影与 canRequestCancel 取消入口纯函数"
```

---

### Task 4: `task-presentation.ts` —— 幂等键生命周期、预览请求构造与 held 决策集合

**Files:**
- Modify: `apps/controller-web/src/task-presentation.ts`
- Modify: `apps/controller-web/src/task-presentation.test.ts`
- 禁止改动：`generateUuidV4`、`taskStateLabel`、`reasonCodeLabel`、`SUBTASK_STATES`、`SPEC_REASON_CODES` 的既有签名与行为（上位计划 Task 4 与已合入基线依赖它们）。

**Interfaces:**
- Consumes: 本文件既有 `generateUuidV4()`；`task-ops.ts` 的 `TaskPreview`、`eligibleDeviceIds`、类型 `MainTaskState`（type-only 导入：`task-ops.ts` 对本文件的 `SubTaskState`/`SpecReasonCode` 同为 type-only，双向 type-only 不构成运行时循环依赖，`vue-tsc` 通过即可，不做大重构）。
- Produces（后续任务只允许用这些名字）：
  ```ts
  export type TimeReviewDecision = 'resume_original_deadline' | 'expire'
  export const TIME_REVIEW_DECISIONS: readonly TimeReviewDecision[]   // 恰好两项，顺序固定
  export function timeReviewDecisionKeys(): Record<TimeReviewDecision, string>
  export interface TaskOperationState { key: string | null; inFlight: boolean }
  export function startTaskOperation(previous: TaskOperationState): { key: string; inFlight: boolean }
  export function resetTaskOperation(): TaskOperationState
  export function previewFingerprint(preview: TaskPreview): string
  export function buildTaskPreviewRequest(operation: 'reboot', deviceIds: string[]): { operation: 'reboot'; device_ids: string[] }
  export function mainTaskStateLabel(state: MainTaskState, t: (key: string) => string): string
  ```
- 语义要求：
  - `TIME_REVIEW_DECISIONS` 恰好是 `['resume_original_deadline', 'expire']`；**不得**存在任何"延长/自定义期限"的决策值。
  - `startTaskOperation({ key: null, inFlight: false })` → 用 `generateUuidV4()` 生成新 key；`startTaskOperation({ key: 'k', inFlight: false })` → **沿用** `'k'`（这是"重试复用同一 key"）；`startTaskOperation({ key: 'k', inFlight: true })` → 抛 `Error('TASK_OPERATION_BUSY')`（防双击）。
  - `resetTaskOperation()` → `{ key: null, inFlight: false }`（preview token 或清单变化时调用）。
  - `previewFingerprint(preview)` → 稳定字符串：包含 `preview_token` 与排序去重后的**固定清单** `device_id`（含 eligible 状态），清单变化即指纹变化；不得包含任何随机值。
  - `buildTaskPreviewRequest` → 设备 id 去重、仅接受 hex64、顺序保持入参顺序；空数组抛 `Error('EMPTY_TASK_TARGETS')`。
  - `mainTaskStateLabel(state, t)` → `t(\`task.mainState.${state}\`)`：主任务四态（04 §4）专用键，与子任务十态 `task.state.*` 键一一对应但互不复用（`queued`/`held` 两态虽同词，键必须不同）；子任务 `taskStateLabel` 与 `task.state.*` 既有键不变。

- [ ] **Step 1: 追加可编译桩代码**

在 `apps/controller-web/src/task-presentation.ts` 末尾追加：

```ts
import type { MainTaskState, TaskPreview } from './task-ops'

export type TimeReviewDecision = 'resume_original_deadline' | 'expire'
export const TIME_REVIEW_DECISIONS: readonly TimeReviewDecision[] = ['resume_original_deadline', 'expire']

export function timeReviewDecisionKeys(): Record<TimeReviewDecision, string> {
  return { resume_original_deadline: '', expire: '' }
}

export interface TaskOperationState { key: string | null; inFlight: boolean }

export function startTaskOperation(_previous: TaskOperationState): { key: string; inFlight: boolean } {
  return { key: '', inFlight: false }
}

export function resetTaskOperation(): TaskOperationState {
  return { key: null, inFlight: false }
}

export function previewFingerprint(_preview: TaskPreview): string { return '' }

export function buildTaskPreviewRequest(_operation: 'reboot', _deviceIds: string[]): { operation: 'reboot'; device_ids: string[] } {
  return { operation: 'reboot', device_ids: [] }
}

export function mainTaskStateLabel(_state: MainTaskState, _t: (key: string) => string): string {
  return _t(`task.state.${_state}`)
}
```

（`import type` 置于文件中部在 ESM/TS 下合法，但为通过 `vue-tsc` 与 lint 习惯，实施时把该 `import type` 合并到文件顶部；步骤 4 给出最终形态。桩里 `mainTaskStateLabel` 故意复用子任务 `task.state.*` 键，构成有效行为 RED：新用例断言 `task.mainState.*` 键与四态文案，对桩即失败，而非编译错误。）

- [ ] **Step 2: 编写失败测试**

在 `apps/controller-web/src/task-presentation.test.ts` 末尾追加：

```ts
import {
  TIME_REVIEW_DECISIONS,
  buildTaskPreviewRequest,
  mainTaskStateLabel,
  previewFingerprint,
  resetTaskOperation,
  startTaskOperation,
  timeReviewDecisionKeys,
} from './task-presentation'
import type { TaskPreview } from './task-ops'

const DEV_A = 'a'.repeat(64)
const DEV_B = 'b'.repeat(64)
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

function preview(overrides: Partial<TaskPreview> = {}): TaskPreview {
  return {
    preview_token: 'tok-123',
    expires_in_ms: 60000,
    targets: [{ device_id: DEV_A, eligible: true }, { device_id: DEV_B, eligible: false, reason: 'DEVICE_OFFLINE' }],
    ...overrides,
  }
}

describe('idempotency key lifecycle for a single confirmation', () => {
  it('generates a canonical UUIDv4 on first confirmation and reuses the same key on retry', () => {
    const first = startTaskOperation(resetTaskOperation())
    expect(first.key).toMatch(UUID_V4)
    expect(first.inFlight).toBe(true)

    // 请求结果不明后的重试：先落下 busy，再以同一 key 重试，key 必须保持不变
    const afterUnknownResult = { key: first.key, inFlight: false }
    const retry = startTaskOperation(afterUnknownResult)
    expect(retry.key).toBe(first.key)
    expect(retry.key).toMatch(UUID_V4)
  })

  it('refuses a second concurrent submission while the operation is in flight', () => {
    const busy = { key: '0f8fad5b-d9cb-469f-a165-70867728950e', inFlight: true }
    expect(() => startTaskOperation(busy)).toThrowError('TASK_OPERATION_BUSY')
  })

  it('resets key and busy state when the preview token or target list changes', () => {
    const started = startTaskOperation(resetTaskOperation())
    const tokenChanged = previewFingerprint(preview()) !== previewFingerprint(preview({ preview_token: 'tok-456' }))
    const listChanged = previewFingerprint(preview()) !==
      previewFingerprint(preview({ targets: [{ device_id: DEV_B, eligible: true }] }))
    expect(tokenChanged).toBe(true)
    expect(listChanged).toBe(true)
    expect(previewFingerprint(preview())).toBe(previewFingerprint(preview()))

    const reset = resetTaskOperation()
    expect(reset).toEqual({ key: null, inFlight: false })
    expect(reset.key).not.toBe(started.key)
  })
})

describe('held time review decisions and preview request body', () => {
  it('offers exactly the two non-extending held decisions with dedicated dictionary keys', () => {
    expect([...TIME_REVIEW_DECISIONS]).toEqual(['resume_original_deadline', 'expire'])
    expect(timeReviewDecisionKeys()).toEqual({
      resume_original_deadline: 'task.held.reviewResumeOriginalDeadline',
      expire: 'task.held.reviewExpire',
    })
    expect(JSON.stringify(TIME_REVIEW_DECISIONS)).not.toMatch(/extend|deadline\+|longer/i)
  })

  it('builds the frozen reboot preview body with de-duplicated hex64 device ids and rejects empty targets', () => {
    expect(buildTaskPreviewRequest('reboot', [DEV_A, DEV_B, DEV_A]))
      .toEqual({ operation: 'reboot', device_ids: [DEV_A, DEV_B] })
    expect(() => buildTaskPreviewRequest('reboot', [])).toThrowError('EMPTY_TASK_TARGETS')
    expect(() => buildTaskPreviewRequest('reboot', ['A'.repeat(64)])).toThrowError('INVALID_DEVICE_ID')
  })
})

describe('mainTaskStateLabel is the independent main four-state dictionary', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null, document: null })
  const identity = (key: string) => key

  it('maps each of the four main states to its dedicated task.mainState.* key and never to the child task.state.* key', () => {
    for (const state of ['queued', 'running', 'held', 'completed'] as const) {
      expect(mainTaskStateLabel(state, identity)).toBe(`task.mainState.${state}`)
      expect(mainTaskStateLabel(state, identity)).not.toBe(`task.state.${state}`)
    }
  })

  it('resolves the four main-state labels in zh-CN', () => {
    i18n.setLocale('zh-CN')
    expect(mainTaskStateLabel('queued', i18n.t)).toBe('排队中')
    expect(mainTaskStateLabel('running', i18n.t)).toBe('运行中')
    expect(mainTaskStateLabel('held', i18n.t)).toBe('等待核验')
    expect(mainTaskStateLabel('completed', i18n.t)).toBe('已完成')
  })

  it('resolves the four main-state labels in en', () => {
    i18n.setLocale('en')
    expect(mainTaskStateLabel('queued', i18n.t)).toBe('Queued')
    expect(mainTaskStateLabel('running', i18n.t)).toBe('Running')
    expect(mainTaskStateLabel('held', i18n.t)).toBe('Awaiting verification')
    expect(mainTaskStateLabel('completed', i18n.t)).toBe('Completed')
  })
})
```

- [ ] **Step 3: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/task-presentation.test.ts`

预期：文件可编译加载，运行数比原先多 8，新增用例全部失败，报错形如
`AssertionError: expected '' to match /^[0-9a-f]{8}-.../`、
`AssertionError: expected function to throw 'TASK_OPERATION_BUSY'`、
`expected { resume_original_deadline: '', expire: '' } to deeply equal {...}`、
`expected 'task.state.queued' to be 'task.mainState.queued'`。
（有效行为 RED；既有用例仍绿。）

- [ ] **Step 4: 最小实现**

把步骤 1 的桩替换为（并把 `import type { MainTaskState, TaskPreview } from './task-ops'` 放到文件顶部 import 区）：

```ts
export type TimeReviewDecision = 'resume_original_deadline' | 'expire'

export const TIME_REVIEW_DECISIONS: readonly TimeReviewDecision[] = ['resume_original_deadline', 'expire']

export function timeReviewDecisionKeys(): Record<TimeReviewDecision, string> {
  return {
    resume_original_deadline: 'task.held.reviewResumeOriginalDeadline',
    expire: 'task.held.reviewExpire',
  }
}

export interface TaskOperationState { key: string | null; inFlight: boolean }

export function resetTaskOperation(): TaskOperationState {
  return { key: null, inFlight: false }
}

export function startTaskOperation(previous: TaskOperationState): { key: string; inFlight: boolean } {
  if (previous.inFlight) throw new Error('TASK_OPERATION_BUSY')
  // 一次确认只生成一次 key：重试（inFlight 已落下）必须沿用原 key，绝不重新生成
  const key = previous.key ?? generateUuidV4()
  return { key, inFlight: true }
}

export function previewFingerprint(preview: TaskPreview): string {
  const targets = preview.targets
    .map((target) => `${target.device_id}:${target.eligible ? '1' : '0'}`)
    .sort()
    .join(',')
  return `${preview.preview_token}|${targets}`
}

const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/

export function buildTaskPreviewRequest(operation: 'reboot', deviceIds: string[]): { operation: 'reboot'; device_ids: string[] } {
  const seen = new Set<string>()
  const device_ids: string[] = []
  for (const id of deviceIds) {
    if (!DEVICE_ID_HEX64.test(id)) throw new Error('INVALID_DEVICE_ID')
    if (!seen.has(id)) {
      seen.add(id)
      device_ids.push(id)
    }
  }
  if (device_ids.length === 0) throw new Error('EMPTY_TASK_TARGETS')
  return { operation, device_ids }
}

export function mainTaskStateLabel(state: MainTaskState, t: (key: string) => string): string {
  return t(`task.mainState.${state}`)
}
```

- [ ] **Step 5: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/task-presentation.test.ts src/task-ops.test.ts`

预期：PASS（全部用例，含既有 3 个状态/原因码用例）。

- [ ] **Step 6: 提交**

```bash
git add apps/controller-web/src/task-presentation.ts apps/controller-web/src/task-presentation.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 新增幂等键生命周期、预览请求构造与主任务四态文案"
```

---

### Task 5: `task-requests.ts` —— 真实 API 请求白名单与任务操作编排

**Files:**
- Create: `apps/controller-web/src/task-requests.ts`
- Create: `apps/controller-web/src/task-requests.test.ts`
- 参考（只读，不改）：`apps/controller-web/src/api.ts`（`get`、`post`、`RequestOptions`，后者来自 `c4fe870`）

**Interfaces:**
- Consumes: `get<T>(path, { signal })`；`post<T>(path, payload, { signal, csrfToken, idempotencyKey })`；`task-ops.ts` 的守卫；`task-presentation.ts` 的 `TimeReviewDecision`、`TaskOperationState`。
- Produces（名称固定）：
  ```ts
  export const TASK_API_PATHS: {
    readonly preview: 'task-previews'
    readonly createTask: 'tasks'
    readonly listTasks: 'tasks'
    readonly taskDetail: (taskId: string) => string      // `tasks/${taskId}`
    readonly taskChildren: (taskId: string) => string    // `tasks/${taskId}/children`
    readonly cancelTask: (taskId: string) => string      // `tasks/${taskId}/cancel`
    readonly timeReview: (taskId: string) => string      // `tasks/${taskId}/time-review`
    readonly releaseLock: (subTaskId: string) => string  // `subtasks/${subTaskId}/release-lock`
  }

  export interface TaskRequestContext { csrfToken: string; signal?: AbortSignal }
  export interface SubmitTaskParams { previewToken: string; idempotencyKey: string; confirm: true; context: TaskRequestContext }
  export interface CancelTaskParams { taskId: string; expectedRevision: string; confirm: true; context: TaskRequestContext }
  export interface TimeReviewParams { taskId: string; expectedRevision: string; decision: TimeReviewDecision; evidence: string; context: TaskRequestContext }
  export interface ReleaseLockParams { subTaskId: string; expectedRevision: string; acknowledgeUnknown: true; reason: string; context: TaskRequestContext }

  // time-review evidence 词表：02 §HTTP:156 `evidence`；与 Runtime 计划 Task 4 `TimeReviewEvidenceKind`（`#[serde(rename_all = "snake_case")]`，wire 严格三串）1:1 对齐，Task 6 `TimeReviewTaskRequest.evidence` 直接复用该枚举；不是新 permission/action/请求字段
  export type TimeReviewEvidenceKind = 'reference_deadline_proven' | 'reference_deadline_expired' | 'inconclusive'
  export const TIME_REVIEW_EVIDENCE_KINDS: readonly TimeReviewEvidenceKind[]   // 恰好三值，顺序固定
  export function isTimeReviewEvidenceKind(value: unknown): value is TimeReviewEvidenceKind

  export function createTaskPreview(operation: 'reboot', deviceIds: string[], context: TaskRequestContext): Promise<ApiResponse<TaskPreview>>
  export function listTasks(cursor: string | null, context: TaskRequestContext): Promise<ApiResponse<TaskPage<TaskItem>>>
  export function getTask(taskId: string, context: TaskRequestContext): Promise<ApiResponse<TaskItem>>
  export function getTaskChildren(taskId: string, cursor: string | null, context: TaskRequestContext): Promise<ApiResponse<TaskPage<SubTaskItem>>>
  export function submitTask(params: SubmitTaskParams): Promise<ApiResponse<{ task_id: string; accepted: boolean }>>
  export function cancelTask(params: CancelTaskParams): Promise<ApiResponse<CancelTaskResponse>>
  export function reviewHeldTime(params: TimeReviewParams): Promise<ApiResponse<TaskItem>>
  export function releaseUnknownLock(params: ReleaseLockParams): Promise<ApiResponse<ReleaseLockResult>>
  ```
- 强约束：
  1. 每个函数只允许调用与规格 02 §HTTP 表一致的路径与载荷；**8 条路径就是全部真实请求白名单**，不得出现任何其它路径。与 `docs/superpowers/specs/2026-10-07-controller-v1-four-task4-design.md:30` 逐条一致（该行是任务 HTTP 集合的最新权威表述）：`POST /api/v1/task-previews`、`GET /api/v1/tasks`、`POST /api/v1/tasks`、`GET /api/v1/tasks/{id}`、`GET /api/v1/tasks/{id}/children`、`POST /api/v1/tasks/{id}/cancel`、`POST /api/v1/tasks/{id}/time-review`、`POST /api/v1/subtasks/{id}/release-lock`；**没有 `GET /task-previews`**，也没有 `/tasks/{id}/retry`、`/tasks/{id}/extend` 之类的自造端点。
  2. 写请求一律 `post(...)`；**只有 `POST /tasks`（提交任务）使用 `RequestOptions.idempotencyKey` 请求头**，其它写请求（预览、取消、time-review、释放锁）**绝不发送 `Idempotency-Key`**（取消 / time-review / 释放锁由 `expected_revision` CAS 保护，仅携带 CSRF）。
  3. `release-lock` 真实响应仅为 `{ released: true, state: 'unknown' }`（02 §6），**绝不能用完整 SubTaskItem 守卫，也不能期待响应中含有 `revision` 或 `lock_released`**；校验走 `assertReleaseLockResultShape`。未通过 `acknowledgeUnknown === true` 与非空 `reason` 时抛 `Error('RELEASE_CONFIRMATION_REQUIRED')`，不发请求。
  4. `time-review` 的 `decision` 必须 ∈ `TIME_REVIEW_DECISIONS`，否则抛 `Error('INVALID_TIME_REVIEW_DECISION')`，不发请求；`evidence` 必须通过 `isTimeReviewEvidenceKind`（严格三串：02 §HTTP:156 + Runtime 计划 Task 4 `TimeReviewEvidenceKind` snake_case、Task 6 `TimeReviewTaskRequest.evidence` 直接复用），否则抛 `Error('INVALID_TIME_REVIEW_EVIDENCE')`，不发请求（0 网络调用）；两项校验都在 `post` 之前，`body.evidence` 只发批准的三串，**不做自由文本→合法值的降级映射/猜测，也不 `as` cast**；`TimeReviewParams.evidence` 暂保持 `string`（避免与尚未完成的 Task 9 UI 类型断裂），请求层在 post 前用谓词 narrow。
  5. 所有响应先过 `task-ops.ts` 守卫再返回；守卫抛错原样向上传播（调用方按错误态呈现，不得吞掉）。
  6. 不读取/写入 Cookie、`localStorage`、`sessionStorage`。

- [ ] **Step 1: 建立可编译桩代码**

`apps/controller-web/src/task-requests.ts`：

```ts
import { get, post } from './api'
import type { ApiResponse } from './types'
import type { ReleaseLockResult, SubTaskItem, TaskItem, TaskPage, TaskPreview } from './task-ops'
import type { TimeReviewDecision } from './task-presentation'

export const TASK_API_PATHS = {
  preview: 'task-previews',
  createTask: 'tasks',
  listTasks: 'tasks',
  taskDetail: (taskId: string) => `tasks/${taskId}`,
  taskChildren: (taskId: string) => `tasks/${taskId}/children`,
  cancelTask: (taskId: string) => `tasks/${taskId}/cancel`,
  timeReview: (taskId: string) => `tasks/${taskId}/time-review`,
  releaseLock: (subTaskId: string) => `subtasks/${subTaskId}/release-lock`,
} as const

export interface TaskRequestContext { csrfToken: string; signal?: AbortSignal }
export interface SubmitTaskParams { previewToken: string; idempotencyKey: string; confirm: true; context: TaskRequestContext }
export interface CancelTaskParams { taskId: string; expectedRevision: string; confirm: true; context: TaskRequestContext }
export interface TimeReviewParams {
  taskId: string; expectedRevision: string; decision: TimeReviewDecision; evidence: string; context: TaskRequestContext
}
export interface ReleaseLockParams {
  subTaskId: string; expectedRevision: string; acknowledgeUnknown: true; reason: string; context: TaskRequestContext
}

export type CancelTaskResponse = { accepted: boolean; cancelled: string[] | null }

// time-review evidence 词表：与 Runtime 计划 Task 4 `TimeReviewEvidenceKind` snake_case wire 1:1 对齐（Task 6 `TimeReviewTaskRequest.evidence` 直接复用该枚举，02 §HTTP:156 `evidence`）；不是新 permission/action/请求字段
export type TimeReviewEvidenceKind = 'reference_deadline_proven' | 'reference_deadline_expired' | 'inconclusive'
export const TIME_REVIEW_EVIDENCE_KINDS: readonly TimeReviewEvidenceKind[] = ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'] as const
// 可编译 RED 桩：恒 true，不拒绝任何非法形态（Step 4 换真实谓词后，非法 evidence 断言由 RED 转 GREEN）
export function isTimeReviewEvidenceKind(_value: unknown): _value is TimeReviewEvidenceKind { return true }

function emptyResponse<T>(): Promise<ApiResponse<T>> { return Promise.resolve({ data: null as unknown as T, requestId: '' }) }

export function createTaskPreview(_operation: 'reboot', _deviceIds: string[], _context: TaskRequestContext): Promise<ApiResponse<TaskPreview>> { return emptyResponse<TaskPreview>() }
export function listTasks(_cursor: string | null, _context: TaskRequestContext): Promise<ApiResponse<TaskPage<TaskItem>>> { return emptyResponse<TaskPage<TaskItem>>() }
export function getTask(_taskId: string, _context: TaskRequestContext): Promise<ApiResponse<TaskItem>> { return emptyResponse<TaskItem>() }
export function getTaskChildren(_taskId: string, _cursor: string | null, _context: TaskRequestContext): Promise<ApiResponse<TaskPage<SubTaskItem>>> { return emptyResponse<TaskPage<SubTaskItem>>() }
export function submitTask(_params: SubmitTaskParams): Promise<ApiResponse<{ task_id: string; accepted: boolean }>> { return emptyResponse<{ task_id: string; accepted: boolean }>() }
export function cancelTask(_params: CancelTaskParams): Promise<ApiResponse<CancelTaskResponse>> { return emptyResponse<CancelTaskResponse>() }
export function reviewHeldTime(_params: TimeReviewParams): Promise<ApiResponse<TaskItem>> { return emptyResponse<TaskItem>() }
export function releaseUnknownLock(_params: ReleaseLockParams): Promise<ApiResponse<ReleaseLockResult>> { return emptyResponse<ReleaseLockResult>() }
```

- [ ] **Step 2: 编写失败测试**

`apps/controller-web/src/task-requests.test.ts`：

```ts
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  TASK_API_PATHS,
  TIME_REVIEW_EVIDENCE_KINDS,
  cancelTask,
  createTaskPreview,
  getTask,
  getTaskChildren,
  isTimeReviewEvidenceKind,
  listTasks,
  releaseUnknownLock,
  reviewHeldTime,
  submitTask,
} from './task-requests'
import { baseTaskTimeEvidence, zeroTaskCounts } from './task-ops'

const DEV_A = 'a'.repeat(64)
const TASK_ID = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const SUB_ID = '123e4567-e89b-42d3-a456-426614174000'
const KEY = '0f8fad5b-d9cb-469f-a165-70867728950e'
const ctx = { csrfToken: 'csrf-token-1' }

const MAIN_TASK = {
  id: TASK_ID, operation: 'reboot', state: 'queued', outcome: null, revision: '7',
  created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(),
  view_scope: 'authorized_subset',
}
// 最小 6 字段子任务（02 §1:24 投影）；result_evidence 为 null（unknown/RESULT_TIMEOUT 尚未产生证据）
const SUB_TASK = {
  id: SUB_ID, device_id: DEV_A, state: 'unknown',
  reason_code: 'RESULT_TIMEOUT', revision: '4', result_evidence: null,
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

function stubFetch(body: unknown): ReturnType<typeof vi.fn> {
  const fetcher = vi.fn(async (_url: string, _init?: RequestInit) => jsonResponse(body))
  vi.stubGlobal('fetch', fetcher)
  return fetcher
}

function calls(fetcher: ReturnType<typeof vi.fn>): Array<[string, RequestInit]> {
  return fetcher.mock.calls as Array<[string, RequestInit]>
}

afterEach(() => { vi.unstubAllGlobals() })

describe('frozen real request whitelist', () => {
  it('exposes exactly the eight documented task endpoints under /api/v1', () => {
    expect(TASK_API_PATHS.preview).toBe('task-previews')
    expect(TASK_API_PATHS.createTask).toBe('tasks')
    expect(TASK_API_PATHS.listTasks).toBe('tasks')
    expect(TASK_API_PATHS.taskDetail(TASK_ID)).toBe(`tasks/${TASK_ID}`)
    expect(TASK_API_PATHS.taskChildren(TASK_ID)).toBe(`tasks/${TASK_ID}/children`)
    expect(TASK_API_PATHS.cancelTask(TASK_ID)).toBe(`tasks/${TASK_ID}/cancel`)
    expect(TASK_API_PATHS.timeReview(TASK_ID)).toBe(`tasks/${TASK_ID}/time-review`)
    expect(TASK_API_PATHS.releaseLock(SUB_ID)).toBe(`subtasks/${SUB_ID}/release-lock`)
  })

  it('never builds an invented endpoint such as a task retry or deadline extension route', () => {
    const built = JSON.stringify(TASK_API_PATHS) + TASK_API_PATHS.taskDetail(TASK_ID)
      + TASK_API_PATHS.taskChildren(TASK_ID) + TASK_API_PATHS.cancelTask(TASK_ID)
      + TASK_API_PATHS.timeReview(TASK_ID) + TASK_API_PATHS.releaseLock(SUB_ID)
    expect(built).not.toMatch(/extend|retry|deadline|task-previews\//)
  })
})

describe('preview, list, detail and children requests', () => {
  it('posts the frozen preview body without an idempotency key and returns guarded targets', async () => {
    const fetcher = stubFetch({ data: { preview_token: 'tok-1', expires_in_ms: 60000,
      targets: [{ device_id: DEV_A, eligible: true }] }, request_id: 'r-1' })
    const res = await createTaskPreview('reboot', [DEV_A], ctx)
    expect(res.data.preview_token).toBe('tok-1')
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe('/api/v1/task-previews')
    expect(init.method).toBe('POST')
    expect(JSON.parse(init.body as string)).toEqual({ operation: 'reboot', device_ids: [DEV_A] })
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')
    expect((init.headers as Record<string, string>)['X-CSRF-Token']).toBe('csrf-token-1')
  })

  it('lists tasks with an optional cursor query, no CSRF header and guarded page shape', async () => {
    const fetcher = stubFetch({ data: { items: [MAIN_TASK], next_cursor: null }, request_id: 'r-2' })
    const res = await listTasks('opaque-cursor-1', ctx)
    expect(res.data.items).toHaveLength(1)
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe('/api/v1/tasks?cursor=opaque-cursor-1')
    expect(init.method).toBe('GET')
    expect(JSON.stringify(init.headers)).not.toContain('X-CSRF-Token')

    const fetcher2 = stubFetch({ data: { items: [], next_cursor: null }, request_id: 'r-3' })
    await listTasks(null, ctx)
    expect(calls(fetcher2)[0]![0]).toBe('/api/v1/tasks')
  })

  it('fetches detail and paginated children and rejects malformed payloads', async () => {
    const fetcher = stubFetch({ data: MAIN_TASK, request_id: 'r-4' })
    await expect(getTask(TASK_ID, ctx)).resolves.toMatchObject({ data: { id: TASK_ID } })
    expect(calls(fetcher)[0]![0]).toBe(`/api/v1/tasks/${TASK_ID}`)

    const badFetcher = stubFetch({ data: { ...MAIN_TASK, revision: '007' }, request_id: 'r-5' })
    await expect(getTask(TASK_ID, ctx)).rejects.toThrowError('INVALID_REVISION')
    expect(calls(badFetcher)).toHaveLength(1)

    const childrenFetcher = stubFetch({ data: { items: [SUB_TASK], next_cursor: null }, request_id: 'r-6' })
    const children = await getTaskChildren(TASK_ID, null, ctx)
    expect(children.data.items[0]!.state).toBe('unknown')
    expect(calls(childrenFetcher)[0]![0]).toBe(`/api/v1/tasks/${TASK_ID}/children`)
  })
})

describe('write operations carry the confirmation facts the backend requires', () => {
  it('submits the task with the Idempotency-Key header and never inside the JSON body', async () => {
    const fetcher = stubFetch({ data: { task_id: TASK_ID, accepted: true }, request_id: 'r-7' })
    const res = await submitTask({ previewToken: 'tok-1', idempotencyKey: KEY, confirm: true, context: ctx })
    expect(res.data).toEqual({ task_id: TASK_ID, accepted: true })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe('/api/v1/tasks')
    expect((init.headers as Record<string, string>)['Idempotency-Key']).toBe(KEY)
    const body = JSON.parse(init.body as string)
    expect(body).toEqual({ preview_token: 'tok-1', confirm: true })
    expect(JSON.stringify(body)).not.toContain('idempotency')
  })

  // 5 个 task_id 负向形态（`it.each` 展开为 5 个用例，count 明确）：响应已过 `post`，但 `task_id` 不是规范
  // UUIDv4 时 submitTask 必须 reject `INVALID_TASK_SUBMISSION`，不得把坏 id 交给调用方。谓词来自 Task 3
  // `task-ops.ts` 的 `isCanonicalUuidV4`（同一正则，无 `m` 标志）。末项"合法 UUID + 换行"是回归检查：
  // JS 裸 `$` 即严格 end input，该形态当前本就被拒绝，不声称这是修复正则的 RED。
  it.each([
    ['empty string', ''],
    ['not a uuid', 'not-a-uuid'],
    ['uppercase hex', 'F47AC10B-58CC-4372-A567-0E02B2C3D479'],
    ['v1 or wrong variant', 'f47ac10b-58cc-1372-a567-0e02b2c3d479'],
    ['trailing newline', `${TASK_ID}\n`],
  ])('rejects a submit response whose task_id is a %s with INVALID_TASK_SUBMISSION', async (_label, task_id) => {
    stubFetch({ data: { task_id, accepted: true }, request_id: 'synthetic' })
    await expect(submitTask({ previewToken: 'tok-1', idempotencyKey: KEY, confirm: true, context: ctx }))
      .rejects.toThrow('INVALID_TASK_SUBMISSION')
  })

  it('cancels only with an expected revision CAS payload and sends no Idempotency-Key', async () => {
    const fetcher = stubFetch({ data: { accepted: true, cancelled: [TASK_ID] }, request_id: 'r-8' })
    await cancelTask({ taskId: TASK_ID, expectedRevision: '7', confirm: true, context: ctx })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe(`/api/v1/tasks/${TASK_ID}/cancel`)
    expect(JSON.parse(init.body as string)).toEqual({ expected_revision: '7', confirm: true })
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')
  })

  // cancelTask 完整响应形状守卫：`post` 只校验信封，业务 `data` 由 cancelTask 自校验。
  // 正例（2 项）经真实 `post` 走合法信封，断言 resolve 且原样返回（null = 无可取消目标；空数组按合同 `string[] | null` 放行，
  // 由正例数组形态覆盖语义、不另立用例）；负例（9 项）覆盖 `cancelled` 缺失(undefined)/数字 42/字符串/对象/
  // 数组含坏 UUID/数组含 uppercase/数组含 wrong variant/数组含 numbers 及 `accepted: false`，
  // 一律 rejects `INVALID_CANCEL_RESPONSE`——不新建 DTO / state / HTTP、不混用 `INVALID_TASK_SUBMISSION`、额外字段忽略。
  // `isCanonicalUuidV4` 本文件已为 submitTask 导入，不重复 import；上方既有 header/body 用例原样保留；
  // 清理依赖既有 `afterEach` 的 `vi.unstubAllGlobals()`。
  it.each([
    ['null payload for an unreadable target', null],
    ['a canonical UUIDv4 array', [TASK_ID]],
  ])('resolves a cancel response whose cancelled is %s', async (_label, cancelled) => {
    stubFetch({ data: { accepted: true, cancelled }, request_id: 'synthetic' })
    const res = await cancelTask({ taskId: TASK_ID, expectedRevision: '7', confirm: true, context: ctx })
    expect(res.data).toEqual({ accepted: true, cancelled })
  })

  it.each([
    ['missing cancelled field', { accepted: true }],
    ['a numeric cancelled', { accepted: true, cancelled: 42 }],
    ['a string cancelled', { accepted: true, cancelled: TASK_ID }],
    ['an object cancelled', { accepted: true, cancelled: { task: TASK_ID } }],
    ['an array with a bad uuid', { accepted: true, cancelled: ['not-a-uuid'] }],
    ['an array with uppercase hex', { accepted: true, cancelled: ['F47AC10B-58CC-4372-A567-0E02B2C3D479'] }],
    ['an array with a wrong variant', { accepted: true, cancelled: ['f47ac10b-58cc-1372-a567-0e02b2c3d479'] }],
    ['an array of numbers', { accepted: true, cancelled: [1, 2] }],
    ['accepted false', { accepted: false, cancelled: null }],
  ])('rejects a cancel response with %s with INVALID_CANCEL_RESPONSE', async (_label, data) => {
    stubFetch({ data, request_id: 'synthetic' })
    await expect(cancelTask({ taskId: TASK_ID, expectedRevision: '7', confirm: true, context: ctx }))
      .rejects.toThrow('INVALID_CANCEL_RESPONSE')
  })

  it('reviews held time with both frozen decisions, sends no Idempotency-Key and refuses unknown decisions or non-enum evidence before fetch', async () => {
    const fetcher = stubFetch({ data: { ...MAIN_TASK, state: 'queued' }, request_id: 'r-9' })
    // 正例：两个 decision 都按后端当前词表发明确合法枚举（不发额外 reason 引用），evidence 严格三串
    await reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'resume_original_deadline', evidence: 'reference_deadline_proven', context: ctx })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe(`/api/v1/tasks/${TASK_ID}/time-review`)
    expect(JSON.parse(init.body as string)).toEqual({
      expected_revision: '7', decision: 'resume_original_deadline', evidence: 'reference_deadline_proven',
    })
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')

    await reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'expire', evidence: 'reference_deadline_expired', context: ctx })
    const [, second] = calls(fetcher)[1]!
    expect(JSON.parse(second.body as string)).toEqual({
      expected_revision: '7', decision: 'expire', evidence: 'reference_deadline_expired',
    })
    expect(JSON.stringify(second.headers)).not.toContain('Idempotency-Key')
    expect(calls(fetcher)).toHaveLength(2)

    // 谓词完整词表：恰接受三值，其余形态（自由文本/PascalCase/尾随空格/数字/null/布尔）全拒
    expect([...TIME_REVIEW_EVIDENCE_KINDS]).toEqual(['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'])
    for (const value of ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'] as const) {
      expect(isTimeReviewEvidenceKind(value)).toBe(true)
    }
    for (const value of ['', 'server evidence ref', 'proven', 'Inconclusive', 'reference_deadline_proven ', 42, null, true] as const) {
      expect(isTimeReviewEvidenceKind(value)).toBe(false)
    }

    // 负例 table：空/自由文本/未知/数字/null/尾随空格/PascalCase 全在 post 前被拒，请求 spy 调用次数不增
    const untouched = stubFetch({ data: MAIN_TASK, request_id: 'r-10' })
    const illegalEvidence = ['', 'server evidence ref', 'proven', 42, null, 'reference_deadline_proven ', 'Inconclusive'] as const
    for (const evidence of illegalEvidence) {
      await expect(reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'resume_original_deadline', evidence: evidence as unknown as string, context: ctx }))
        .rejects.toThrowError('INVALID_TIME_REVIEW_EVIDENCE')
    }
    // decision 校验先于 evidence：decision 非法时（此处 evidence 也是自由文本）仍先报 INVALID_TIME_REVIEW_DECISION
    await expect(reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'extend' as never, evidence: 'server evidence ref', context: ctx }))
      .rejects.toThrowError('INVALID_TIME_REVIEW_DECISION')
    expect(calls(untouched)).toHaveLength(0)
  })

  it('refuses unknown lock release without explicit acknowledgement and a non-blank reason, then validates only {released:true,state:unknown}', async () => {
    const untouched = stubFetch({ data: { released: true, state: 'unknown' }, request_id: 'r-11' })
    await expect(releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: false as never, reason: 'ok', context: ctx }))
      .rejects.toThrowError('RELEASE_CONFIRMATION_REQUIRED')
    await expect(releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: true, reason: '   ', context: ctx }))
      .rejects.toThrowError('RELEASE_CONFIRMATION_REQUIRED')
    expect(calls(untouched)).toHaveLength(0)

    // 真实响应只有 released/state 两个字段：不得用完整 SubTaskItem 守卫，也不得期待 lock_released/revision
    const fetcher = stubFetch({ data: { released: true, state: 'unknown' }, request_id: 'r-12' })
    const res = await releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: true, reason: '现场人工确认', context: ctx })
    expect(res.data).toEqual({ released: true, state: 'unknown' })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe(`/api/v1/subtasks/${SUB_ID}/release-lock`)
    expect(JSON.parse(init.body as string)).toEqual({
      expected_revision: '4', acknowledge_unknown: true, reason: '现场人工确认',
    })
    expect(init.method).toBe('POST')
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')

    const malformed = stubFetch({ data: { released: true, state: 'succeeded' }, request_id: 'r-13' })
    await expect(releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: true, reason: '现场人工确认', context: ctx }))
      .rejects.toThrowError('INVALID_RELEASE_LOCK_STATE')
    expect(calls(malformed)).toHaveLength(1)
  })
})
```

- [ ] **Step 3: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/task-requests.test.ts`

预期：文件可编译加载，运行本节全部具名用例及 `it.each` 展开项（未新增独立 `it`，time-review 负例并入既有用例；cancelTask 响应形状守卫新增 2 个 `it.each` 表，精确用例数以执行阶段实测非零为准，不预设纸面数字），多数失败：`AssertionError: expected null to deeply equal {...}`、`expected '' to be '/api/v1/tasks'`、`expected function to throw 'RELEASE_CONFIRMATION_REQUIRED'`；time-review 新增的 evidence 负例（谓词词表 true/false 断言、非法 evidence 的 `INVALID_TIME_REVIEW_EVIDENCE` rejects、decision 优先的 `INVALID_TIME_REVIEW_DECISION` rejects）此时全部 RED——桩谓词恒 true、桩 `reviewHeldTime` 不校验不抛错，正是真实谓词在 GREEN 阶段要修复的行为。白名单断言与 `TIME_REVIEW_EVIDENCE_KINDS` 词表常量断言此时**通过**（常量已在桩里定义），这是可接受的：它们锁定的正是后续实现不得漂移的契约。
（整体构成有效行为 RED：运行数非 0 且因行为缺失而失败。）

- [ ] **Step 4: 最小实现**

把 `task-requests.ts` 的桩函数替换为：

```ts
import { get, post } from './api'
import type { ApiResponse } from './types'
import {
  assertReleaseLockResultShape,
  assertSubTaskPageShape,
  assertTaskItemShape,
  assertTaskPageShape,
  assertTaskPreviewShape,
  isCanonicalUuidV4,
  type ReleaseLockResult,
  type SubTaskItem,
  type TaskItem,
  type TaskPage,
  type TaskPreview,
} from './task-ops'
import { TIME_REVIEW_DECISIONS, buildTaskPreviewRequest, type TimeReviewDecision } from './task-presentation'

// time-review evidence 词表：02 §HTTP:156 `evidence`；与 Runtime 计划 Task 4 `TimeReviewEvidenceKind` snake_case wire 1:1 对齐（Task 6 `TimeReviewTaskRequest.evidence` 直接复用该枚举）；不是新 permission/action/请求字段
export type TimeReviewEvidenceKind = 'reference_deadline_proven' | 'reference_deadline_expired' | 'inconclusive'
export const TIME_REVIEW_EVIDENCE_KINDS: readonly TimeReviewEvidenceKind[] = ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'] as const
// 完整词表校验：严格只接受上面三串，其余形态（自由文本/PascalCase/尾随空格/数字/布尔/null）全拒，不降级映射
export function isTimeReviewEvidenceKind(value: unknown): value is TimeReviewEvidenceKind {
  return typeof value === 'string' && (TIME_REVIEW_EVIDENCE_KINDS as readonly string[]).includes(value)
}

// 真实请求白名单：与规格 02 §HTTP 表逐条一致；新增端点必须先改规格与测试，禁止在此临时扩展。
export const TASK_API_PATHS = {
  preview: 'task-previews',
  createTask: 'tasks',
  listTasks: 'tasks',
  taskDetail: (taskId: string) => `tasks/${taskId}`,
  taskChildren: (taskId: string) => `tasks/${taskId}/children`,
  cancelTask: (taskId: string) => `tasks/${taskId}/cancel`,
  timeReview: (taskId: string) => `tasks/${taskId}/time-review`,
  releaseLock: (subTaskId: string) => `subtasks/${subTaskId}/release-lock`,
} as const

export interface TaskRequestContext { csrfToken: string; signal?: AbortSignal }
export interface SubmitTaskParams { previewToken: string; idempotencyKey: string; confirm: true; context: TaskRequestContext }
export interface CancelTaskParams { taskId: string; expectedRevision: string; confirm: true; context: TaskRequestContext }
export interface TimeReviewParams {
  taskId: string; expectedRevision: string; decision: TimeReviewDecision; evidence: string; context: TaskRequestContext
}
export interface ReleaseLockParams {
  subTaskId: string; expectedRevision: string; acknowledgeUnknown: true; reason: string; context: TaskRequestContext
}

function listPath(base: string, cursor: string | null): string {
  return cursor ? `${base}?cursor=${encodeURIComponent(cursor)}` : base
}

export async function createTaskPreview(operation: 'reboot', deviceIds: string[], context: TaskRequestContext): Promise<ApiResponse<TaskPreview>> {
  const body = buildTaskPreviewRequest(operation, deviceIds)
  const res = await post<TaskPreview>(TASK_API_PATHS.preview, body, { csrfToken: context.csrfToken, signal: context.signal })
  assertTaskPreviewShape(res.data)
  return res
}

export async function listTasks(cursor: string | null, context: TaskRequestContext): Promise<ApiResponse<TaskPage<TaskItem>>> {
  const res = await get<TaskPage<TaskItem>>(listPath(TASK_API_PATHS.listTasks, cursor), { signal: context.signal })
  assertTaskPageShape(res.data)
  return res
}

export async function getTask(taskId: string, context: TaskRequestContext): Promise<ApiResponse<TaskItem>> {
  const res = await get<TaskItem>(TASK_API_PATHS.taskDetail(taskId), { signal: context.signal })
  assertTaskItemShape(res.data)
  return res
}

export async function getTaskChildren(taskId: string, cursor: string | null, context: TaskRequestContext): Promise<ApiResponse<TaskPage<SubTaskItem>>> {
  const res = await get<TaskPage<SubTaskItem>>(listPath(TASK_API_PATHS.taskChildren(taskId), cursor), { signal: context.signal })
  assertSubTaskPageShape(res.data)
  return res
}

export async function submitTask(params: SubmitTaskParams): Promise<ApiResponse<{ task_id: string; accepted: boolean }>> {
  const res = await post<{ task_id: string; accepted: boolean }>(
    TASK_API_PATHS.createTask,
    { preview_token: params.previewToken, confirm: params.confirm },
    { csrfToken: params.context.csrfToken, signal: params.context.signal, idempotencyKey: params.idempotencyKey },
  )
  const data = res.data as unknown
  // 02 §模型表：创建响应的 `task_id` 是公共 Id（规范 UUIDv4）。此处复用 Task 3 `task-ops.ts` 导出的
  // `isCanonicalUuidV4` 谓词（同一 `TASK_ID_V4` 正则，不新建 validator、不改正则），
  // `data` 非 object / null、`task_id` 非规范 UUIDv4、`accepted` 非 true 一律同一错误码，不降级映射、不补默认值
  if (typeof data !== 'object' || data === null ||
      !isCanonicalUuidV4((data as { task_id?: unknown }).task_id) ||
      (data as { accepted?: unknown }).accepted !== true) {
    throw new Error('INVALID_TASK_SUBMISSION')
  }
  return res
}

export type CancelTaskResponse = { accepted: boolean; cancelled: string[] | null }

export async function cancelTask(params: CancelTaskParams): Promise<ApiResponse<CancelTaskResponse>> {
  const res = await post<CancelTaskResponse>(
    TASK_API_PATHS.cancelTask(params.taskId),
    { expected_revision: params.expectedRevision, confirm: params.confirm },
    { csrfToken: params.context.csrfToken, signal: params.context.signal },
  )
  // `post` 只校验信封，业务 `data` 在此校验：`data` 按 unknown 处理，非 object / null / array、`accepted !== true`、
  // `cancelled` 既非 null 也非全规范 UUIDv4 数组（`isCanonicalUuidV4`，Task 3 同一正则，本文件已为 submitTask 导入，不重复 import）
  // 一律抛同一 `INVALID_CANCEL_RESPONSE`：不新建 DTO / state / HTTP 映射、不混用 `INVALID_TASK_SUBMISSION`、
  // 不把取消结果提升为可见状态；额外字段忽略，空数组按声明合同 `string[] | null` 放行
  const data: unknown = res.data
  const cancelled = (data as { cancelled?: unknown } | null)?.cancelled
  if (typeof data !== 'object' || data === null || Array.isArray(data) ||
      (data as { accepted?: unknown }).accepted !== true ||
      !(cancelled === null || (Array.isArray(cancelled) && cancelled.every(isCanonicalUuidV4)))) {
    throw new Error('INVALID_CANCEL_RESPONSE')
  }
  return res
}

export async function reviewHeldTime(params: TimeReviewParams): Promise<ApiResponse<TaskItem>> {
  if (!TIME_REVIEW_DECISIONS.includes(params.decision)) throw new Error('INVALID_TIME_REVIEW_DECISION')
  const evidence = params.evidence
  if (!isTimeReviewEvidenceKind(evidence)) throw new Error('INVALID_TIME_REVIEW_EVIDENCE')
  const res = await post<TaskItem>(
    TASK_API_PATHS.timeReview(params.taskId),
    { expected_revision: params.expectedRevision, decision: params.decision, evidence },
    { csrfToken: params.context.csrfToken, signal: params.context.signal },
  )
  assertTaskItemShape(res.data)
  return res
}

export async function releaseUnknownLock(params: ReleaseLockParams): Promise<ApiResponse<ReleaseLockResult>> {
  if (params.acknowledgeUnknown !== true) throw new Error('RELEASE_CONFIRMATION_REQUIRED')
  const reason = params.reason.trim()
  if (reason.length === 0) throw new Error('RELEASE_CONFIRMATION_REQUIRED')
  const res = await post<ReleaseLockResult>(
    TASK_API_PATHS.releaseLock(params.subTaskId),
    { expected_revision: params.expectedRevision, acknowledge_unknown: true, reason },
    { csrfToken: params.context.csrfToken, signal: params.context.signal },
  )
  assertReleaseLockResultShape(res.data)
  return res
}
```

删除桩里未使用的 `eligibleDeviceIds` 导入（上面已不导入）；保留 `emptyResponse` 之外的导入只在本文件实际使用，避免 `noUnusedLocals` 类错误。

- [ ] **Step 5: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/task-requests.test.ts src/task-ops.test.ts src/task-presentation.test.ts`

预期：PASS。

- [ ] **Step 6: 提交**

```bash
git add apps/controller-web/src/task-requests.ts apps/controller-web/src/task-requests.test.ts
git diff --cached --name-only
git commit -m "feat(controller-web): 新增白名单化任务 API 请求编排"
```

---


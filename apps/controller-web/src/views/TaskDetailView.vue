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
import { compareRevision } from '../events'
import { TIME_REVIEW_DECISIONS, mainTaskStateLabel, timeReviewDecisionKeys } from '../task-presentation'
import type { TimeReviewEvidenceKind } from '../task-requests'
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
  /**
   * SSE 失效刷新信号（App 在 `task.updated` 失效提示且当前处于详情页时推进）。
   * 详情页的 `/tasks/:id` 与 `/tasks/:id/children` 不属于 SSE 全量快照来源，
   * 只能由这个显式信号接进失效链路；推进即按当前授权重新拉取主任务与 children。
   */
  refreshToken?: number
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
// 显式三值证据选择：初始 null（无预选），词表与 Task 5 `TimeReviewEvidenceKind` 1:1（不默认 inconclusive、
// 不接受自由文本，也不把任意文字强制为 reference_deadline_proven）
const EVIDENCE_KINDS: readonly TimeReviewEvidenceKind[] = ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive']
const evidence = ref<TimeReviewEvidenceKind | null>(null)
const reviewBusy = ref(false)
const cancelBusy = ref(false)
const releaseBusy = ref(false)
const releaseTarget = ref<SubTaskItem | null>(null)
// WEB-N1：错误状态分离——主任务 GET、children 分页与写操作各持独立领域错误，
// 任一方成功只清空自身错误，绝不抹除另一路在途或已失败操作的错误（错误态保留契约）
const taskError = ref<string | null>(null)
const childrenError = ref<string | null>(null)
const actionError = ref<string | null>(null)
// 聚合消费：任领域失败必呈现可见错误，绝不被另一路成功抹为 null
const displayError = computed(() => taskError.value || childrenError.value || actionError.value)

// held 全集判定唯一依据：同一已加载主任务 counts.held（服务端按当前授权全集重算；主任务 counts 与 children
// 独立分页，当前页 children 不能证明授权全集是否 held）；严格 isU64DecimalString 且 ≠"0"，不转换 Number
// （字符串比较支持 u64 全域），不另发统计 GET；缺键/JSON 数/负数/前导零/非十进制一律 fail-closed
const hasHeld = computed(() => {
  const held = task.value?.counts?.held
  return typeof held === 'string' && isU64DecimalString(held) && held !== '0'
})

// 取消入口唯一依据：后端计算的 actions.can_cancel（当前身份/device.reboot/合格 queued-held 由服务端判定）；
// 不看主任务状态、不接 children 分页或 devicePermissions 权限表（prop 保留供 App.vue 接线，不参与判定）；
// 缺 actions/非法布尔/未知 view_scope 由 canRequestCancel fail-closed；"可请求"不保成功，服务端事务为准
const cancellable = computed(() => (task.value ? canRequestCancel(task.value) : false))

// 统一代际令牌：每次异步操作（初次加载/分页/写操作/GET 重取）开始时捕获 (gen, id)，所有 await 返回后、
// catch 分支与 finally 回写前严格校验两者仍与捕获值一致；不一致即过期响应，直接丢弃且不触碰任何状态
const detailGeneration = ref(0)
const isStale = (gen: number, id: string) => gen !== detailGeneration.value || id !== props.taskId

// 子任务读取代际（同 taskId 内时序防御）：detailGeneration 只在切换 taskId 时递增，拦不住同一任务页面内
// "写操作成功 GET 重取（C1）先返回、挂载/分页旧读取（C0）迟到返回"——旧快照 gen 与 id 均未变、会通过
// isStale 并把旧列表写回 children 造成状态倒退。每次 loadChildren 启动（含分页与写操作成功触发的 GET 重取）
// 即推进 childrenGeneration 并捕获本地 cGen；分页只累加当前代际的页，迟到旧代际快照整体丢弃，
// 不触碰 children/nextCursor/childrenError
const childrenGeneration = ref(0)
const isChildrenStale = (gen: number, cGen: number, id: string) =>
  gen !== detailGeneration.value || cGen !== childrenGeneration.value || id !== props.taskId
// 子任务读取在途标记：任意 loadChildren（首屏或分页）在途时禁用"加载更多"，
// 防重复点击推进代际；旧代际的 finally 不得清除新代际的加载态
const childrenLoading = ref(false)

async function loadChildren(cursor: string | null = null) {
  if (!props.onLoadChildren) return
  const gen = detailGeneration.value
  const id = props.taskId
  childrenGeneration.value += 1
  const cGen = childrenGeneration.value
  // 首屏重刷（cursor === null，含写成功后 GET(null) 权威重取）在 await 前立即清空旧游标：
  // 在途期间"加载更多"随旧游标一起消失，无法发起旧游标分页推进 childrenGeneration，
  // 把随后返回的写后最新快照误判为 stale 反向淘汰、旧子任务状态残留；
  // 即便点击先于重渲染发生，模板读取的 nextCursor 也已是 null，最坏退化为又一次安全重置
  if (cursor === null) nextCursor.value = null
  childrenLoading.value = true
  try {
    const page = await props.onLoadChildren(id, cursor)
    if (isChildrenStale(gen, cGen, id)) return
    assertSubTaskPageShape(page)
    children.value = cursor === null ? page.items : [...children.value, ...page.items]
    nextCursor.value = page.next_cursor
    childrenError.value = null // 只清自身错误；不触碰 taskError/actionError
  } catch {
    if (isChildrenStale(gen, cGen, id)) return
    childrenError.value = t('state.error') // 保留旧数据，不伪装成功、不伪装空
  } finally {
    if (!isChildrenStale(gen, cGen, id)) childrenLoading.value = false
  }
}

/**
 * 主任务**读取代际**（复审 P2：同任务详情读取没有代际保护）。
 *
 * `detailGeneration` 只在切换 `props.taskId` 时递增，管不了同一个详情页里的并发读取：连续两条
 * `task.updated`（refreshToken 推进两次）、或写操作后的同代重取与一次外部刷新重叠时，
 * 两条 `/tasks/:id` 的 (gen, id) 完全相同，后发先至、先发迟到就会把详情**回滚**成旧快照。
 * 规则：最后启动的那次读取是唯一的合法落地者，任何更晚启动一次都让本次响应整体作废
 * （不写 task、不清 taskError——本次响应已不代表当前状态，也没资格断言失败）。
 */
let taskFetchGeneration = 0

/**
 * 同 ID 主任务的 revision 单调保护：incoming 严格更旧时不倒灌（复审 P2 第二道防线）。
 *
 * 读取代际拦住"同页并发 GET 的迟到者"，这一层再拦住**不经过 loadTask 的较新写入**：
 * 时间评审成功后直接采用服务端返回的 DTO（不经 loadTask，也就不推进读取代际），
 * 此前任何一条在途 GET 的旧 DTO 迟到返回时都不能把它盖回去。
 * 同一个函数也用于写响应自身（复审 P1）：评审响应在途期间 SSE 失效提示可能已由 loadTask 把
 * 主任务推进到更高 revision，迟到的评审响应（CAS 提交时的旧版本）同样不得倒灌。
 * 跨 ID 一律放行：revision 只在同一对象内单调，不同任务的 revision 之间没有可比性
 * （与 events.ts 的 kind + id 水位同一口径）。
 * 判定 fail-open：任一侧缺 revision 或不是 canonical u64 十进制时无法证明回退，照常采纳，
 * 绝不因字段异常把合法新数据丢掉（比较走 compareRevision，不经 Number，u64 全域无损）。
 */
function keepsRevisionMonotonic(current: TaskItem | null, incoming: TaskItem): boolean {
  const previous = current?.revision
  if (previous === undefined) return true
  if (current !== null && current.id !== incoming.id) return true
  if (!isU64DecimalString(previous) || !isU64DecimalString(incoming.revision)) return true
  return compareRevision(incoming.revision, previous) >= 0
}

async function loadTask() {
  if (!props.onLoadTask) return
  const gen = detailGeneration.value
  const id = props.taskId
  // 读取代际在**每次启动**时推进（含写操作成功后的同代重取）：detailGeneration 只跟 taskId 走，
  // 同任务页面内两次刷新信号并发时它不变，必须由 fetchGen 判定"最后一次启动才是唯一有效的落地者"
  const fetchGen = ++taskFetchGeneration
  // 过期 = 组件代际失效 ∪ taskId 变化 ∪ 已有更晚一次读取启动（本次响应无论多完整都整体丢弃）
  const expired = () => isStale(gen, id) || fetchGen !== taskFetchGeneration
  try {
    const data = await props.onLoadTask(id)
    if (expired()) return
    assertTaskItemShape(data)
    // 同 ID 主任务 revision 单调：更旧 DTO 不倒灌。fetchGen 挡住"同页并发 GET 的迟到者"，
    // 这一层再挡住"不经过 loadTask 的较新写入"（时间评审成功直接回写服务端 DTO 后，
    // 之前那条在途 GET 的旧响应不得覆盖它）
    if (!keepsRevisionMonotonic(task.value, data)) return
    task.value = data
    taskError.value = null
  } catch {
    if (expired()) return
    taskError.value = t('state.error') // 保留旧数据，不伪装成功；不触碰 childrenError/actionError
  }
}

async function handleCancel() {
  if (!props.onCancel || !task.value || !cancellable.value || cancelBusy.value) return
  const gen = detailGeneration.value
  const id = props.taskId
  cancelBusy.value = true
  try {
    await props.onCancel({ taskId: id, expectedRevision: task.value.revision, confirm: true })
    if (isStale(gen, id)) return // 过期返回：不触发 GET 重取、不回写
    actionError.value = null // 写操作成功必须清除动作错误，避免同任务重试成功后 alert 永久残留
    await loadTask() // 同代重取：loadTask 捕获同一 (gen, id)，不换代
  } catch {
    if (!isStale(gen, id)) actionError.value = t('state.error')
  } finally {
    if (!isStale(gen, id)) cancelBusy.value = false // 旧代 finally 不得清除新代 busy
  }
}

async function handleReleaseLock(payload: { subtaskId: string; expected_revision: string; acknowledge_unknown: true; reason: string }) {
  if (!props.onReleaseLock || releaseBusy.value) return
  const gen = detailGeneration.value
  const id = props.taskId
  releaseBusy.value = true
  try {
    await props.onReleaseLock(payload)
    if (isStale(gen, id)) return // 过期返回：不关闭新代模态、不触发重取
    releaseTarget.value = null
    actionError.value = null // 写操作成功必须清除动作错误
    // 释放锁响应仅包含 {released:true,state:'unknown'}，成功后通过 GET 重新拉取子任务列表
    await loadChildren(null)
  } catch {
    if (!isStale(gen, id)) actionError.value = t('state.error')
  } finally {
    if (!isStale(gen, id)) releaseBusy.value = false
  }
}

async function runTimeReview(decision: 'resume_original_deadline' | 'expire') {
  // 与危险按钮同一 guard（fail-closed）：hasHeld + 当前 isAdmin===true + view_scope==='full'；
  // 不新增 can_time_review 权限点、不改取消 can_cancel 合同与 wire payload
  if (!props.onTimeReview || !task.value || !hasHeld.value || props.isAdmin !== true
    || task.value.view_scope !== 'full' || evidence.value === null || reviewBusy.value) return
  const gen = detailGeneration.value
  const id = props.taskId
  reviewBusy.value = true
  try {
    // 显式选择后 evidence.value 已 narrow 为强类型 TimeReviewEvidenceKind，原样透传、不 trim/不改写
    const selectedEvidence = evidence.value
    const updated = await props.onTimeReview({
      taskId: id,
      expectedRevision: task.value.revision,
      decision,
      evidence: selectedEvidence,
    })
    if (isStale(gen, id)) return // 过期返回：旧 DTO 不得覆盖新任务的 task
    assertTaskItemShape(updated)
    // 复审 P1：写响应自身同样受同 ID revision 单调保护（与 loadTask 同一判定、同样 fail-open）。
    // 评审在途窗口内，SSE 失效提示可能已让 loadTask 把主任务推进到更高 revision（例如并发写者
    // 已完成该任务、或另一轮刷新拿到更新快照），CAS 提交时那个 revision 的响应迟到返回时不得把
    // 详情倒灌回旧版本。不倒灌不等于失败：本次 CAS 已被服务端接受，故动作错误照清、children 照
    // 按原子结果重取，仅跳过对更新 task 快照的回写（更高 revision 的快照本身已包含评审后的状态）。
    if (keepsRevisionMonotonic(task.value, updated)) task.value = updated
    actionError.value = null      // 写操作成功必须清除动作错误
    await loadChildren(null)      // 按服务端原子结果重取 children
  } catch {
    if (!isStale(gen, id)) actionError.value = t('state.error')
  } finally {
    if (!isStale(gen, id)) reviewBusy.value = false
  }
}

onMounted(() => {
  // 初次加载同样走统一代际令牌：loadTask/loadChildren 内部捕获当前 gen 与 taskId，无"不传 gen 即不校验"的例外
  if (!props.initialTask) loadTask()
  if (props.initialChildren.length === 0) loadChildren(null)
})

watch(
  () => props.taskId,
  (newId) => {
    // 路由参数切换到另一任务 ID（组件实例复用）：递增代际令牌并清空旧任务数据与四 busy，
    // 旧操作后续 catch/finally 一律被 isStale 拦截，再重取新任务
    detailGeneration.value += 1
    task.value = null
    children.value = []
    nextCursor.value = null
    childrenLoading.value = false
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

// SSE 失效提示（App 的 refreshToken 推进）：主任务与 children 都按当前授权重新拉取。
// 不重建组件实例（:key 不变）：已选时间证据与批处理幂等键属于本次操作意图，不该被一次
// 外部变化抹掉；两条读取各自走 (gen,id) 与 (gen,cGen,id) 代际令牌，写操作在途时同样安全，
// 失败只落各自领域错误并保留旧数据（不伪装成功、不伪装空）。
watch(
  () => props.refreshToken,
  (token, previous) => {
    if (token === previous || !props.taskId) return
    void loadTask()
    void loadChildren(null)
  },
)
</script>

<template>
  <div class="task-detail-view">
    <h2>{{ t('task.detail.title') }}</h2>
    <AppNotice v-if="displayError" tone="error" :tone-label="t('common.error')" :title="t('state.error')">
      <span>{{ displayError }}</span>
    </AppNotice>
    <template v-if="task">
      <!-- 主任务基本信息：紧凑栅格卡片，避免逐项纵向堆叠把下方子任务列表完全推出首屏 -->
      <div class="task-detail-view__meta">
        <p data-testid="task-id">{{ task.id }}</p>
        <!-- 字段标签与值渲染为同一文本节点：主任务四态文案与子任务十态文案存在同词（如"排队中"），
             带标签可避免两处文本产生歧义匹配；值本身仍是 mainTaskStateLabel 专用文案 -->
        <p data-testid="task-state">{{ t('task.list.columns.state') }}: {{ mainTaskStateLabel(task.state, t) }}</p>
        <p data-testid="task-revision">{{ t('task.list.columns.revision') }}: {{ task.revision }}</p>
        <!-- 时间证据只消费原始值，绝不把 created_time 对象整体渲染进模板 -->
        <p data-testid="task-created">{{ task.created_time.reference_utc }} {{ task.created_time.quality }}</p>
        <p data-testid="task-scope">{{ t(describeAuthorizedSubset(task.view_scope).key) }}</p>
      </div>

      <p v-if="!cancellable">{{ t('task.cancel.notCancellable') }}</p>
      <BaseButton
        v-if="cancellable"
        variant="danger"
        :loading="cancelBusy"
        :loading-label="t('button.loading')"
        @click="handleCancel"
      >
        {{ t('task.cancel.action') }}
      </BaseButton>

      <h3>{{ t('task.held.reviewTitle') }}</h3>
      <p>{{ t('task.held.reviewExplain') }}</p>
      <p>{{ t('task.held.reviewSelfReported') }}</p>
      <p v-if="!hasHeld">{{ t('task.held.reviewNoHeld') }}</p>
      <template v-else-if="isAdmin && task.view_scope === 'full'">
        <fieldset data-testid="time-review-evidence">
          <label>{{ t('task.detail.evidenceSource') }}</label>
          <label v-for="kind in EVIDENCE_KINDS" :key="kind" class="task-detail-view__evidence">
            <input
              type="radio"
              name="time-review-evidence"
              :value="kind"
              :data-testid="'time-review-evidence-' + kind"
              v-model="evidence"
            >
            {{ t('task.timeReview.evidence.' + kind) }}
          </label>
        </fieldset>
        <BaseButton
          v-for="decision in TIME_REVIEW_DECISIONS"
          :key="decision"
          variant="danger"
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
      <TaskStateList
        :subtasks="children"
        :show-release-action="isAdmin"
        @release-lock="(sub: SubTaskItem) => { releaseTarget = sub }"
      />
      <BaseButton v-if="nextCursor" variant="secondary" :disabled="childrenLoading" @click="loadChildren(nextCursor)">
        {{ t('task.list.loadMore') }}
      </BaseButton>

      <TaskModals
        mode="release-lock"
        :open="releaseTarget !== null"
        :subtask="releaseTarget"
        :busy="releaseBusy"
        @close="releaseTarget = null"
        @confirm-release-lock="handleReleaseLock"
      />
    </template>
  </div>
</template>

<style scoped>
.task-detail-view {
  display: flex;
  flex-direction: column;
  gap: var(--space-2, 0.5rem);
}
/* 视图内的裸文本节点段落不承担纵向间距职责：由 flex gap 统一控制，去掉浏览器默认
   上下 margin，避免主任务信息把下方子任务列表整段推出首屏 */
.task-detail-view > p,
.task-detail-view h2,
.task-detail-view h3 {
  margin-block: 0;
}
.task-detail-view h2,
.task-detail-view h3 {
  margin-block: var(--space-3, 0.75rem) 0;
}
/* 危险动作按钮按内容宽度呈现，不被 flex 列拉伸成整行红色色块 */
.task-detail-view > button {
  align-self: flex-start;
}
.task-detail-view__meta {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(14rem, 1fr));
  gap: var(--space-1, 0.25rem) var(--space-4, 1rem);
  min-width: 0;
  margin-block-end: var(--space-2, 0.5rem);
  padding: var(--space-3, 0.75rem) var(--space-4, 1rem);
  background: var(--color-surface, #fff);
  border: 1px solid var(--color-border, #69788b);
  border-radius: var(--radius, 0.375rem);
}
.task-detail-view__meta > p {
  margin: 0;
  min-width: 0;
  overflow-wrap: anywhere;
}
.task-detail-view__meta > [data-testid="task-id"] {
  grid-column: 1 / -1;
  font-family: monospace;
}
.task-detail-view__evidence {
  display: flex;
  align-items: center;
  gap: var(--space-1, 0.25rem);
}
</style>

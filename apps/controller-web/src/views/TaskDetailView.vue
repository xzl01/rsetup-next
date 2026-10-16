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
    childrenError.value = null // 只清自身错误；不触碰 taskError/actionError
  } catch {
    if (isChildrenStale(gen, cGen, id)) return
    childrenError.value = t('state.error') // 保留旧数据，不伪装成功、不伪装空
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
    task.value = updated          // 只采用服务端返回，绝不本地推断"已恢复/已延期"
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
    <AppNotice v-if="displayError" tone="error" :tone-label="t('common.error')" :title="t('state.error')">
      <span>{{ displayError }}</span>
    </AppNotice>
    <template v-if="task">
      <p data-testid="task-id">{{ task.id }}</p>
      <!-- 字段标签与值渲染为同一文本节点：主任务四态文案与子任务十态文案存在同词（如"排队中"），
           带标签可避免两处文本产生歧义匹配；值本身仍是 mainTaskStateLabel 专用文案 -->
      <p data-testid="task-state">{{ t('task.list.columns.state') }}: {{ mainTaskStateLabel(task.state, t) }}</p>
      <p data-testid="task-revision">{{ task.revision }}</p>
      <!-- 时间证据只消费原始值，绝不把 created_time 对象整体渲染进模板 -->
      <p data-testid="task-created">{{ task.created_time.reference_utc }} {{ task.created_time.quality }}</p>
      <p data-testid="task-scope">{{ t(describeAuthorizedSubset(task.view_scope).key) }}</p>

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
      <BaseButton v-if="nextCursor" variant="secondary" @click="loadChildren(nextCursor)">
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
.task-detail-view__evidence {
  display: flex;
  align-items: center;
  gap: var(--space-1, 0.25rem);
}
</style>

<script setup lang="ts">
// 准入审批页（spec 02 §5，AT-04）：批量批准/拒绝 + 单目标 reopen/revoke/reauthorize。
//
// 安全约束：
// - 勾选时刻冻结 `{device_id, expected_revision}`；后台刷新或翻页绝不静默改写已冻结
//   版本，也绝不把新出现的设备自动纳入目标集合（06 §1 ADM-01）。
// - 版本发生并发变更时显式标出冲突风险，由服务端 CAS 裁决，客户端绝不假装覆盖。
// - 吊销/重新授权后必须板端人工重置：界面常驻提示，批准不等于设备立即恢复连接。
import { computed, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'
import { createI18n } from '../i18n'
import {
  ADMIN_REVIEW_DECISIONS,
  MAX_APPROVAL_BATCH_TARGETS,
  adminErrorKey,
  approveApprovals,
  getDeviceProfile,
  isAbortedError,
  isValidAdminReason,
  listApprovals,
  patchDeviceProfile,
  reauthorizeDevice,
  rejectApprovals,
  reopenApproval,
  revokeDevice,
  type AdminReviewDecision,
  type ApprovalBatchResults,
  type ApprovalItem,
  type ApprovalTargetInput,
} from '../admin-api'
import { createAdminListResource } from '../admin-list'

const props = defineProps<{ csrfToken: string; isAdmin: boolean }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = i18n.t
const toneLabel = computed(() => (i18n.locale.value === 'en' ? 'Status' : '状态'))

const decisionFilter = ref<AdminReviewDecision | ''>('')
const list = createAdminListResource<ApprovalItem>(
  (cursor, signal) => listApprovals(
    { cursor, decision: decisionFilter.value === '' ? null : decisionFilter.value },
    { csrfToken: props.csrfToken, signal },
  ),
)

const frozen = ref(new Map<string, ApprovalTargetInput>())
const busy = ref(false)
const confirmRisk = ref(false)
const reason = ref('')
const actionErrorKey = ref<string | null>(null)
const reasonInvalid = ref(false)
const batchResult = ref<ApprovalBatchResults | null>(null)
const renaming = ref<{ deviceId: string; revision: string; displayName: string } | null>(null)

const targets = computed(() => Array.from(frozen.value.values()))
const tooMany = computed(() => targets.value.length > MAX_APPROVAL_BATCH_TARGETS)
const canSubmit = computed(() =>
  props.isAdmin && !busy.value && targets.value.length > 0 && !tooMany.value && confirmRisk.value)
const failedItems = computed(() => (batchResult.value?.results ?? []).filter((item) => !item.ok))
const succeededCount = computed(() => (batchResult.value?.results ?? []).filter((item) => item.ok).length)

/** 勾选时刻冻结 expected_revision；再次点击取消勾选。 */
function toggleSelect(item: ApprovalItem): void {
  const next = new Map(frozen.value)
  if (next.has(item.device_id)) {
    next.delete(item.device_id)
  } else {
    next.set(item.device_id, { device_id: item.device_id, expected_revision: item.revision })
  }
  frozen.value = next
  batchResult.value = null
}

/** 已冻结项的当前投影版本与冻结版本不一致：显式提示，不静默改写。 */
function isStale(item: ApprovalItem): boolean {
  const frozenTarget = frozen.value.get(item.device_id)
  return frozenTarget !== undefined && frozenTarget.expected_revision !== item.revision
}

function shortId(value: string): string {
  return value.length <= 12 ? value : `${value.slice(0, 8)}…`
}

function decisionLabel(decision: AdminReviewDecision): string {
  return t(`admin.approvals.decision.${decision}`)
}

/** 切换筛选后游标即失效：重新从第一页加载，而不是带着旧筛选的 cursor 继续翻页。 */
function onDecisionFilterChange(event: Event): void {
  const raw = (event.target as HTMLSelectElement).value
  decisionFilter.value = raw === '' ? '' : raw as AdminReviewDecision
  reload()
}

function ctx() {
  return { csrfToken: props.csrfToken }
}

function resetActionState(): void {
  actionErrorKey.value = null
  reasonInvalid.value = false
}

async function runBatch(kind: 'approve' | 'reject'): Promise<void> {
  if (!canSubmit.value) return
  if (kind === 'reject' && !isValidAdminReason(reason.value)) {
    reasonInvalid.value = true
    return
  }
  const submitted = targets.value
  resetActionState()
  busy.value = true
  batchResult.value = null
  try {
    const res = kind === 'approve'
      ? await approveApprovals(submitted, ctx())
      : await rejectApprovals(submitted, reason.value.trim(), ctx())
    batchResult.value = res.data
    // 已提交的目标从选择集合移除：绝不把同一批目标重复提交，也不吸纳刷新后新增的设备。
    const next = new Map(frozen.value)
    for (const target of submitted) next.delete(target.device_id)
    frozen.value = next
    confirmRisk.value = false
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function runSingle(action: 'reopen' | 'revoke' | 'reauthorize', item: ApprovalItem): Promise<void> {
  if (busy.value) return
  if ((action === 'revoke') && !isValidAdminReason(reason.value)) {
    resetActionState()
    reasonInvalid.value = true
    return
  }
  resetActionState()
  busy.value = true
  try {
    if (action === 'reopen') {
      await reopenApproval(item.device_id, item.revision, ctx())
    } else if (action === 'revoke') {
      await revokeDevice(item.device_id, item.revision, reason.value.trim(), ctx())
    } else {
      await reauthorizeDevice(item.device_id, item.revision, ctx())
    }
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

/** 改名前先读取中控档案真值，以档案返回的 revision 做 CAS，绝不用列表旧版本覆盖。 */
async function openRename(item: ApprovalItem): Promise<void> {
  if (busy.value) return
  resetActionState()
  busy.value = true
  try {
    const res = await getDeviceProfile(item.device_id, ctx())
    const profile = res.data as { display_name?: unknown; revision?: unknown }
    if (typeof profile.display_name !== 'string' || typeof profile.revision !== 'string') {
      actionErrorKey.value = 'admin.error.generic'
      return
    }
    renaming.value = { deviceId: item.device_id, revision: profile.revision, displayName: profile.display_name }
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function submitRename(): Promise<void> {
  const draft = renaming.value
  if (!draft || busy.value) return
  resetActionState()
  busy.value = true
  try {
    await patchDeviceProfile(draft.deviceId, draft.revision, draft.displayName, ctx())
    renaming.value = null
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

function reload(): void {
  resetActionState()
  void list.reload()
}

onMounted(() => {
  if (props.isAdmin) void list.reload()
})

watch(() => props.isAdmin, (allowed) => {
  if (!allowed) {
    list.reset()
    frozen.value = new Map()
    renaming.value = null
  } else {
    void list.reload()
  }
})

watch(() => props.csrfToken, () => {
  // 认证代际变更：旧投影与冻结选择一律作废，随后以新 token 重新拉取
  list.reset()
  frozen.value = new Map()
  renaming.value = null
  if (props.isAdmin) void list.reload()
})

onBeforeUnmount(() => list.reset())
</script>

<template>
  <section class="admin-view" :aria-label="t('admin.approvals.title')">
    <header class="admin-view__header">
      <h3>{{ t('admin.approvals.title') }}</h3>
      <BaseButton variant="secondary" :disabled="busy" :loading="busy && list.loading.value"
        :loading-label="t('button.loading')" @click="reload">
        {{ t('admin.common.reload') }}
      </BaseButton>
    </header>

    <AppNotice v-if="!isAdmin" tone="warning" :tone-label="toneLabel" :title="t('auth.error.denied')">
      <span>{{ t('admin.denied') }}</span>
    </AppNotice>

    <template v-else>
      <p data-testid="revoke-manual-reset-notice" class="admin-view__notice">
        {{ t('admin.approvals.manualResetNotice') }}
      </p>

      <div class="admin-view__toolbar">
        <label for="approvals-decision-filter">{{ t('admin.approvals.filterDecision') }}</label>
        <select id="approvals-decision-filter" :value="decisionFilter" :disabled="busy"
          @change="onDecisionFilterChange">
          <option value="">{{ t('common.unknown') }}</option>
          <option v-for="value in ADMIN_REVIEW_DECISIONS" :key="value" :value="value">{{ decisionLabel(value) }}</option>
        </select>
      </div>

      <AppNotice v-if="list.errorKey.value" tone="error" :tone-label="toneLabel" :title="t('state.error')">
        <span>{{ t(list.errorKey.value) }}</span>
        <BaseButton class="admin-view__retry" :disabled="busy" @click="reload">{{ t('state.retry') }}</BaseButton>
      </AppNotice>

      <AppNotice v-else-if="actionErrorKey" tone="error" :tone-label="toneLabel" :title="t('common.error')">
        <span>{{ t(actionErrorKey) }}</span>
      </AppNotice>

      <div v-if="batchResult && failedItems.length > 0" data-testid="batch-partial-fail-notice" class="admin-view__notice">
        <span>{{ t('admin.approvals.partialFail', { ok: succeededCount, failed: failedItems.length }) }}</span>
        <ul>
          <li v-for="item in failedItems" :key="item.device_id" :data-testid="`batch-fail-${item.device_id}`">
            {{ shortId(item.device_id) }}: {{ item.error?.code ?? t('admin.error.generic') }}
          </li>
        </ul>
      </div>
      <p v-else-if="batchResult" data-testid="batch-result-notice" class="admin-view__notice">
        {{ t('admin.approvals.partialFail', { ok: succeededCount, failed: failedItems.length }) }}
      </p>

      <p v-if="targets.length > 0" data-testid="frozen-selection-notice" class="admin-view__notice">
        {{ t('admin.approvals.frozenNotice') }}
      </p>

      <p v-if="list.loading.value && targets.length === 0">{{ t('state.loading') }}</p>
      <p v-else-if="!list.loading.value && !list.errorKey.value && list.items.value.length === 0">
        {{ t('admin.common.empty') }}
      </p>

      <table v-if="list.items.value.length > 0" class="admin-view__table">
        <thead>
          <tr>
            <th>{{ t('admin.common.select') }}</th>
            <th>{{ t('device.dim.connection') }}</th>
            <th>{{ t('admin.approvals.fingerprint') }}</th>
            <th>{{ t('admin.approvals.decision') }}</th>
            <th>{{ t('admin.common.revision') }}</th>
            <th>{{ t('common.actions') }}</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="item in list.items.value" :key="item.device_id">
            <td>
              <input type="checkbox" :data-testid="`select-${item.device_id}`"
                :aria-label="`${t('admin.common.select')} ${shortId(item.device_id)}`"
                :checked="frozen.has(item.device_id)" :disabled="busy"
                @change="toggleSelect(item)" />
            </td>
            <td>{{ item.connection_state }}</td>
            <td><code>{{ shortId(item.fingerprint) }}</code></td>
            <td>{{ decisionLabel(item.decision) }}</td>
            <td>
              {{ item.revision }}
              <span v-if="isStale(item)" :data-testid="`stale-${item.device_id}`" class="admin-view__warning">
                {{ t('admin.approvals.staleWarning') }}
              </span>
            </td>
            <td class="admin-view__actions">
              <BaseButton variant="secondary" :disabled="busy" :loading-label="t('button.loading')"
                @click="() => runSingle('reopen', item)">
                {{ t('admin.approvals.reopen') }}
              </BaseButton>
              <BaseButton variant="danger" :disabled="busy" :loading-label="t('button.loading')"
                @click="() => runSingle('revoke', item)">
                {{ t('admin.approvals.revoke') }}
              </BaseButton>
              <BaseButton variant="secondary" :disabled="busy" :loading-label="t('button.loading')"
                @click="() => runSingle('reauthorize', item)">
                {{ t('admin.approvals.reauthorize') }}
              </BaseButton>
              <BaseButton variant="secondary" :disabled="busy" :loading-label="t('button.loading')"
                @click="() => openRename(item)">
                {{ t('admin.approvals.renameProfile') }}
              </BaseButton>
            </td>
          </tr>
        </tbody>
      </table>

      <div v-if="renaming" class="admin-view__panel" data-testid="rename-panel">
        <BaseInput :id="`rename-${renaming.deviceId}`" v-model="renaming.displayName"
          :label="t('admin.approvals.profileDisplayName')" />
        <BaseButton :disabled="busy" @click="submitRename">{{ t('common.actions') }}</BaseButton>
        <BaseButton variant="secondary" :disabled="busy" @click="renaming = null">{{ t('common.cancel') }}</BaseButton>
      </div>

      <div class="admin-view__panel">
        <BaseInput id="approvals-reason" v-model="reason" :label="t('admin.common.reason')"
          :error="reasonInvalid ? t('admin.approvals.reasonRequired') : undefined" :disabled="busy" />
        <label class="admin-view__confirm">
          <input id="approvals-confirm-risk" v-model="confirmRisk" type="checkbox" :disabled="busy" />
          {{ t('admin.approvals.confirmRisk') }}
        </label>
        <p v-if="tooMany" data-testid="too-many-targets">{{ t('admin.approvals.tooManyTargets') }}</p>
        <div class="admin-view__actions">
          <BaseButton :disabled="!canSubmit" :loading="busy" :loading-label="t('button.loading')"
            @click="() => runBatch('approve')">
            {{ t('admin.approvals.approveSelected') }}
          </BaseButton>
          <BaseButton variant="danger" :disabled="!canSubmit" :loading="busy" :loading-label="t('button.loading')"
            @click="() => runBatch('reject')">
            {{ t('admin.approvals.rejectSelected') }}
          </BaseButton>
        </div>
      </div>

      <p data-testid="requires-device-reset-notice" class="admin-view__notice">
        {{ t('admin.approvals.requiresDeviceReset') }}
      </p>

      <div v-if="list.nextCursor.value" class="admin-view__pagination">
        <BaseButton variant="secondary" :disabled="busy" :loading="busy && list.loading.value"
          :loading-label="t('button.loading')" @click="() => list.loadMore()">
          {{ t('admin.common.loadMore') }}
        </BaseButton>
      </div>
    </template>
  </section>
</template>

<style scoped>
.admin-view { display: grid; gap: var(--space-3); margin-top: var(--space-4); border-top: 1px solid var(--color-border, #ccc); padding-top: var(--space-4); min-width: 0; }
.admin-view__header { display: flex; justify-content: space-between; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__header h3 { margin: 0; }
.admin-view__toolbar { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__notice { margin: 0; padding: var(--space-2); border: 1px solid var(--color-border, #ddd); border-radius: 4px; background: var(--color-surface-subtle, #f8fafc); }
.admin-view__warning { color: var(--color-danger, #b91c1c); font-weight: 600; }
.admin-view__table { width: 100%; border-collapse: collapse; overflow-wrap: anywhere; }
.admin-view__table th, .admin-view__table td { text-align: start; padding: var(--space-2); border-bottom: 1px solid var(--color-border, #eee); vertical-align: top; }
.admin-view__actions { display: flex; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__panel { display: grid; gap: var(--space-2); padding: var(--space-3); border: 1px dashed var(--color-border, #ccc); border-radius: 4px; }
.admin-view__confirm { display: flex; align-items: center; gap: var(--space-2); }
.admin-view__pagination { display: flex; justify-content: center; }
.admin-view__retry { margin-inline-start: auto; }
</style>

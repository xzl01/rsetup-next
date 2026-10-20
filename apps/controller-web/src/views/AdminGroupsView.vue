<script setup lang="ts">
// 设备组管理页（spec 02 §5）：组增改归档 + PUT 成员整体替换。
//
// 安全约束：
// - 后端只提供 `PUT /groups/{id}/members` 的整体替换语义，界面因此显式声明“替换”后果，
//   并展示当前成员数量；提交前做客户端去重与 hex64 校验，非法标识直接忽略而不是静默发送。
// - 所有写操作携带 CAS `expected_revision`，冲突由服务端 409 裁决并显式提示。
import { computed, inject, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import BaseInput from '../components/BaseInput.vue'
import { createI18n } from '../i18n'
import {
  adminErrorKey,
  createGroup,
  isAbortedError,
  isValidAdminName,
  listDevicesForAdmin,
  listGroups,
  normalizeDeviceIdSet,
  patchGroup,
  replaceGroupMembers,
  type AdminGroupItem,
} from '../admin-api'
import { createAdminListResource } from '../admin-list'

const props = defineProps<{ csrfToken: string; isAdmin: boolean }>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const t = i18n.t
const toneLabel = computed(() => (i18n.locale.value === 'en' ? 'Status' : '状态'))

const list = createAdminListResource<AdminGroupItem>(
  (cursor, signal) => listGroups(cursor, { csrfToken: props.csrfToken, signal }),
)

const busy = ref(false)
const actionErrorKey = ref<string | null>(null)
const newName = ref('')
const nameInvalid = ref(false)
const editing = ref<{ id: string; name: string } | null>(null)
const membersDraft = ref<{ id: string; text: string } | null>(null)
const referenceDevices = ref<string[]>([])
const referenceLoading = ref(false)

function ctx() {
  return { csrfToken: props.csrfToken }
}

async function submitCreate(): Promise<void> {
  if (busy.value) return
  if (!isValidAdminName(newName.value)) {
    nameInvalid.value = true
    return
  }
  nameInvalid.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    await createGroup(newName.value, ctx())
    newName.value = ''
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function submitRename(): Promise<void> {
  const draft = editing.value
  if (!draft || busy.value) return
  if (!isValidAdminName(draft.name)) {
    nameInvalid.value = true
    return
  }
  const group = list.items.value.find((item) => item.id === draft.id)
  if (!group) return
  nameInvalid.value = false
  actionErrorKey.value = null
  busy.value = true
  try {
    await patchGroup(draft.id, { expectedRevision: group.revision, name: draft.name }, ctx())
    editing.value = null
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function archiveGroup(group: AdminGroupItem): Promise<void> {
  if (busy.value || group.archived) return
  actionErrorKey.value = null
  busy.value = true
  try {
    await patchGroup(group.id, { expectedRevision: group.revision, archived: true }, ctx())
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

function openMembers(group: AdminGroupItem): void {
  membersDraft.value = { id: group.id, text: '' }
  actionErrorKey.value = null
}

const memberIds = computed(() => normalizeDeviceIdSet(
  (membersDraft.value?.text ?? '').split(/[\s,]+/).filter((value) => value.length > 0),
))
const memberInputRejected = computed(() => {
  const text = membersDraft.value?.text ?? ''
  const tokens = text.split(/[\s,]+/).map((value) => value.trim()).filter((value) => value.length > 0)
  return tokens.length > 0 && tokens.length !== memberIds.value.length
})

async function submitMembers(): Promise<void> {
  const draft = membersDraft.value
  if (!draft || busy.value) return
  const group = list.items.value.find((item) => item.id === draft.id)
  if (!group) return
  actionErrorKey.value = null
  busy.value = true
  try {
    await replaceGroupMembers(draft.id, group.revision, memberIds.value, ctx())
    membersDraft.value = null
    await list.reload()
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    busy.value = false
  }
}

async function loadReferenceDevices(): Promise<void> {
  if (referenceLoading.value) return
  referenceLoading.value = true
  actionErrorKey.value = null
  try {
    const res = await listDevicesForAdmin(null, ctx())
    referenceDevices.value = res.data.items.map((item) => item.device_id)
  } catch (error: unknown) {
    if (!isAbortedError(error)) actionErrorKey.value = adminErrorKey(error)
  } finally {
    referenceLoading.value = false
  }
}

function reload(): void {
  actionErrorKey.value = null
  void list.reload()
}

onMounted(() => {
  if (props.isAdmin) void list.reload()
})

watch(() => props.isAdmin, (allowed) => {
  if (!allowed) {
    list.reset()
    editing.value = null
    membersDraft.value = null
    referenceDevices.value = []
  } else {
    void list.reload()
  }
})

watch(() => props.csrfToken, () => {
  list.reset()
  editing.value = null
  membersDraft.value = null
  if (props.isAdmin) void list.reload()
})

onBeforeUnmount(() => list.reset())
</script>

<template>
  <section class="admin-view" :aria-label="t('admin.groups.title')">
    <header class="admin-view__header">
      <h3>{{ t('admin.groups.title') }}</h3>
      <BaseButton variant="secondary" :disabled="busy" :loading="busy && list.loading.value"
        :loading-label="t('button.loading')" @click="reload">
        {{ t('admin.common.reload') }}
      </BaseButton>
    </header>

    <AppNotice v-if="!isAdmin" tone="warning" :tone-label="toneLabel" :title="t('auth.error.denied')">
      <span>{{ t('admin.denied') }}</span>
    </AppNotice>

    <template v-else>
      <AppNotice v-if="list.errorKey.value || actionErrorKey" tone="error" :tone-label="toneLabel"
        :title="t('state.error')">
        <span>{{ t(list.errorKey.value ?? actionErrorKey ?? 'admin.error.generic') }}</span>
        <BaseButton class="admin-view__retry" :disabled="busy" @click="reload">{{ t('state.retry') }}</BaseButton>
      </AppNotice>

      <form class="admin-view__panel" @submit.prevent="submitCreate">
        <BaseInput id="group-create-name" v-model="newName" :label="t('admin.groups.nameLabel')"
          :error="nameInvalid ? t('admin.common.fieldError') : undefined" :disabled="busy" />
        <BaseButton type="submit" :disabled="busy" :loading="busy" :loading-label="t('button.loading')">
          {{ t('admin.groups.create') }}
        </BaseButton>
      </form>

      <p v-if="list.loading.value && list.items.value.length === 0">{{ t('state.loading') }}</p>
      <p v-else-if="!list.loading.value && !list.errorKey.value && list.items.value.length === 0">
        {{ t('admin.common.empty') }}
      </p>

      <ul v-if="list.items.value.length > 0" class="admin-view__list">
        <li v-for="group in list.items.value" :key="group.id" class="admin-view__item"
          :data-testid="`group-${group.id}`">
          <div class="admin-view__item-main">
            <strong>{{ group.name }}</strong>
            <code>{{ group.id }}</code>
            <span>{{ t('admin.groups.memberCount') }}: {{ group.member_count }}</span>
            <span>{{ t('admin.common.revision') }}: {{ group.revision }}</span>
            <span v-if="group.archived" class="admin-view__badge">{{ t('admin.groups.archived') }}</span>
          </div>
          <div class="admin-view__actions">
            <BaseButton variant="secondary" :disabled="busy || group.archived"
              :loading-label="t('button.loading')" @click="editing = { id: group.id, name: group.name }">
              {{ t('admin.common.rename') }}
            </BaseButton>
            <BaseButton variant="secondary" :disabled="busy || group.archived"
              :loading-label="t('button.loading')" @click="openMembers(group)">
              {{ t('admin.groups.members') }}
            </BaseButton>
            <BaseButton variant="danger" :disabled="busy || group.archived"
              :loading-label="t('button.loading')" @click="() => archiveGroup(group)">
              {{ t('admin.groups.archive') }}
            </BaseButton>
          </div>
        </li>
      </ul>

      <div v-if="editing" class="admin-view__panel" data-testid="group-rename-panel">
        <BaseInput id="group-rename-name" v-model="editing.name" :label="t('admin.groups.nameLabel')"
          :error="nameInvalid ? t('admin.common.fieldError') : undefined" :disabled="busy" />
        <div class="admin-view__actions">
          <BaseButton :disabled="busy" @click="submitRename">{{ t('admin.common.confirmAction') }}</BaseButton>
          <BaseButton variant="secondary" :disabled="busy" @click="editing = null">{{ t('common.cancel') }}</BaseButton>
        </div>
      </div>

      <div v-if="membersDraft" class="admin-view__panel" data-testid="group-members-panel">
        <label for="group-members-input">{{ t('admin.groups.members') }}</label>
        <textarea id="group-members-input" v-model="membersDraft.text" rows="4"
          :disabled="busy" :placeholder="t('admin.device.id')"></textarea>
        <p>{{ t('admin.groups.membersHint') }}</p>
        <p v-if="memberInputRejected" data-testid="members-invalid-notice">{{ t('admin.groups.membersInvalid') }}</p>
        <div class="admin-view__actions">
          <BaseButton :disabled="busy" @click="submitMembers">{{ t('admin.groups.saveMembers') }}</BaseButton>
          <BaseButton variant="secondary" :disabled="busy" @click="membersDraft = null">{{ t('common.cancel') }}</BaseButton>
        </div>
      </div>

      <div class="admin-view__panel">
        <BaseButton variant="secondary" :disabled="busy || referenceLoading"
          :loading="referenceLoading" :loading-label="t('button.loading')" @click="loadReferenceDevices">
          {{ t('admin.groups.visibleDevices') }}
        </BaseButton>
        <ul v-if="referenceDevices.length > 0" data-testid="reference-devices">
          <li v-for="deviceId in referenceDevices" :key="deviceId"><code>{{ deviceId }}</code></li>
        </ul>
      </div>

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
.admin-view__list { list-style: none; padding: 0; margin: 0; display: grid; gap: var(--space-2); }
.admin-view__item { display: flex; justify-content: space-between; gap: var(--space-2); flex-wrap: wrap; padding: var(--space-2); border: 1px solid var(--color-border, #eee); border-radius: 4px; }
.admin-view__item-main { display: flex; gap: var(--space-2); flex-wrap: wrap; align-items: baseline; overflow-wrap: anywhere; }
.admin-view__badge { background: var(--color-surface-subtle, #f1f5f9); border-radius: 9999px; padding: 2px 8px; font-size: 0.875rem; }
.admin-view__actions { display: flex; gap: var(--space-2); flex-wrap: wrap; }
.admin-view__panel { display: grid; gap: var(--space-2); padding: var(--space-3); border: 1px dashed var(--color-border, #ccc); border-radius: 4px; }
.admin-view__panel textarea { font: inherit; max-width: 100%; }
.admin-view__pagination { display: flex; justify-content: center; }
.admin-view__retry { margin-inline-start: auto; }
</style>

<script setup lang="ts">
import { ref } from 'vue'
import AppNotice from '../components/AppNotice.vue'
import BaseButton from '../components/BaseButton.vue'
import type { AuthStore } from '../auth'

const props = defineProps<{
  auth: AuthStore
  locale: string
  t: (key: string, params?: Record<string, string | number>) => string
}>()

const busy = ref(false)
const revokingSessionId = ref<string | null>(null)

async function revokeSingle(sessionId: string) {
  if (busy.value) return
  busy.value = true
  revokingSessionId.value = sessionId
  try {
    await props.auth.revokeSession(sessionId)
  } finally {
    revokingSessionId.value = null
    busy.value = false
  }
}

async function revokeOthers() {
  if (busy.value) return
  busy.value = true
  try {
    await props.auth.revokeOtherSessions()
  } finally {
    busy.value = false
  }
}

async function loadMoreSessions() {
  if (busy.value || !props.auth.sessionsNextCursor.value) return
  busy.value = true
  try {
    await props.auth.listSessions(props.auth.sessionsNextCursor.value)
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <section class="auth-sessions" :aria-label="t('auth.sessions.title')">
    <header class="auth-sessions__header">
      <h3>{{ t('auth.sessions.title') }}</h3>
      <BaseButton
        v-if="auth.sessions.value.length > 0"
        variant="secondary"
        :disabled="busy"
        :loading="busy && auth.sessionsLoading.value"
        :loading-label="t('button.loading')"
        @click="revokeOthers"
      >
        {{ t('auth.sessions.revokeOthers') }}
      </BaseButton>
    </header>

    <div v-if="auth.sessionsError.value" class="auth-sessions__error">
      <AppNotice tone="error" :tone-label="locale === 'en' ? 'Error' : '错误'" :title="t('state.error')">
        <span>{{ t('auth.error.generic') }}</span>
        <BaseButton
          class="auth-retry"
          :disabled="busy"
          :loading="busy && auth.sessionsLoading.value"
          :loading-label="t('button.loading')"
          @click="() => void auth.listSessions()"
        >
          {{ t('state.retry') }}
        </BaseButton>
      </AppNotice>
    </div>

    <p v-else-if="auth.sessions.value.length === 0 && !auth.sessionsLoading.value" class="auth-sessions__empty">
      {{ t('auth.sessions.empty') }}
    </p>

    <ul v-if="auth.sessions.value.length > 0" class="auth-sessions__list">
      <li v-for="sessionItem in auth.sessions.value" :key="sessionItem.id" class="auth-sessions__item">
        <div class="auth-sessions__info">
          <span v-if="sessionItem.current" class="auth-sessions__badge">{{ t('auth.sessions.current') }}</span>
          <span class="auth-sessions__label">{{ t('auth.sessions.created') }}:</span>
          <span class="auth-sessions__time">{{ sessionItem.created_time }}</span>
        </div>
        <BaseButton
          variant="secondary"
          :disabled="busy"
          :loading="busy && revokingSessionId === sessionItem.id"
          :loading-label="t('button.loading')"
          @click="() => revokeSingle(sessionItem.id)"
        >
          {{ t('auth.sessions.revoke') }}
        </BaseButton>
      </li>
    </ul>

    <div v-if="auth.sessionsNextCursor.value" class="auth-sessions__pagination">
      <BaseButton
        variant="secondary"
        :disabled="busy"
        :loading="busy && auth.sessionsLoading.value"
        :loading-label="t('button.loading')"
        @click="loadMoreSessions"
      >
        {{ t('auth.sessions.loadMore') }}
      </BaseButton>
    </div>
  </section>
</template>

<style scoped>
.auth-sessions { display: grid; gap: var(--space-3); margin-top: var(--space-4); border-top: 1px solid var(--color-border, #ccc); padding-top: var(--space-4); }
.auth-sessions__header { display: flex; justify-content: space-between; align-items: center; gap: var(--space-2); }
.auth-sessions__header h3 { margin: 0; }
.auth-sessions__list { list-style: none; padding: 0; margin: 0; display: grid; gap: var(--space-2); }
.auth-sessions__item { display: flex; justify-content: space-between; align-items: center; gap: var(--space-2); padding: var(--space-2); border: 1px solid var(--color-border, #eee); border-radius: 4px; }
.auth-sessions__info { display: flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
.auth-sessions__badge { background: var(--color-primary-subtle, #e0f2fe); color: var(--color-primary, #0369a1); padding: 2px 8px; border-radius: 9999px; font-size: 0.875rem; font-weight: 500; }
.auth-sessions__time { font-size: 0.875rem; color: var(--color-text-muted, #666); }
.auth-sessions__empty { margin: 0; color: var(--color-text-muted, #666); font-style: italic; }
.auth-sessions__pagination { display: flex; justify-content: center; }
.auth-retry { margin-inline-start: auto; }
</style>

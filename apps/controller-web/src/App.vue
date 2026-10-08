<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from 'vue'
import AppNotice from './components/AppNotice.vue'
import BaseButton from './components/BaseButton.vue'
import LoginView from './views/LoginView.vue'
import PasswordView from './views/PasswordView.vue'
import SessionsView from './views/SessionsView.vue'
import { createI18n, type Locale } from './i18n'
import { createAuth } from './auth'
import { createRouter } from './router'

const { locale, t, setLocale } = createI18n()
const auth = createAuth()
const router = createRouter(auth)

const busy = ref(false)

watch(
  () => auth.status.value,
  (newStatus) => {
    if (newStatus === 'signed_in') {
      void auth.listSessions()
    }
  },
  { immediate: true },
)

function changeLocale(event: Event) {
  setLocale((event.target as HTMLSelectElement).value as Locale)
}

async function submitLogout() {
  if (busy.value) return
  busy.value = true
  try {
    await auth.logout()
  } finally {
    busy.value = false
  }
}

onMounted(() => {
  void auth.refresh()
})

onUnmounted(() => {
  router.cleanup()
})
</script>

<template>
  <a class="skip-link" href="#main-content">{{ t('nav.skip') }}</a>
  <header class="app-header">
    <h1>{{ t('app.title') }}</h1>
    <label for="app-language">{{ t('language.label') }}</label>
    <select id="app-language" :value="locale" @change="changeLocale">
      <option value="zh-CN">简体中文</option>
      <option value="en">English</option>
    </select>
  </header>
  <main id="main-content" class="app-main" tabindex="-1">
    <template v-if="auth.status.value === 'checking'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="t('state.loading')">
        <span>{{ t('auth.checking') }}</span>
      </AppNotice>
    </template>
    <template v-else-if="auth.status.value === 'error'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="t('state.error')">
        <span>{{ t('auth.error.generic') }}</span>
        <BaseButton class="auth-retry" :loading-label="t('button.loading')" @click="() => void auth.refresh()">
          {{ t('state.retry') }}
        </BaseButton>
      </AppNotice>
    </template>
    <template v-else-if="router.currentRoute.value.name === 'login'">
      <LoginView :auth="auth" :t="t" />
    </template>
    <template v-else-if="router.currentRoute.value.name === 'password'">
      <PasswordView :auth="auth" :t="t" />
    </template>
    <section v-else class="auth-signed-in">
      <h2>{{ t('auth.signedIn.heading') }}</h2>
      <p role="status">{{ auth.user.value?.display_name || auth.user.value?.username }}</p>
      <BaseButton :loading="busy" :loading-label="t('button.loading')" @click="submitLogout">{{ t('auth.logout.label') }}</BaseButton>
      <SessionsView :auth="auth" :locale="locale" :t="t" />
    </section>
  </main>
  <footer class="app-footer">{{ t('app.title') }}</footer>
</template>

<style scoped>
.skip-link {
  position: absolute;
  inset-block-start: 0;
  inset-inline-start: 0;
  padding: var(--space-2) var(--space-4);
  background: var(--color-surface);
  transform: translateY(-150%);
  z-index: 1;
}
.skip-link:focus { transform: none; }
.app-header, .app-main, .app-footer {
  width: min(100%, 68rem);
  margin-inline: auto;
  padding: var(--space-4);
  min-width: 0;
  overflow-wrap: anywhere;
}
.app-header { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2) var(--space-4); }
.app-header h1 { margin: 0; flex-basis: 100%; }
.app-header select { max-width: 100%; min-height: 2.75rem; font: inherit; }
.auth-signed-in { display: grid; gap: var(--space-4); min-width: 0; }
.auth-retry { margin-inline-start: auto; }
</style>

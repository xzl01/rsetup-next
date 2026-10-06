<script setup lang="ts">
import { onMounted, ref, watch } from 'vue';
import AppNotice from './components/AppNotice.vue';
import BaseButton from './components/BaseButton.vue';
import BaseInput from './components/BaseInput.vue';
import { createI18n, type Locale } from './i18n';
import { authErrorKey, createAuth } from './auth';

const { locale, t, setLocale } = createI18n();
const auth = createAuth();

const username = ref('');
const password = ref('');
const currentPassword = ref('');
const newPassword = ref('');
const formError = ref('');
const busy = ref(false);
const revokingSessionId = ref<string | null>(null);

watch(
  () => auth.status.value,
  (newStatus) => {
    if (newStatus === 'signed_in') {
      void auth.listSessions();
    }
  },
  { immediate: true },
);

function changeLocale(event: Event) {
  setLocale((event.target as HTMLSelectElement).value as Locale);
}

function mappedFormError(): string {
  // 服务端文案永不展示：auth 状态机已把错误归约为 errorCode，唯一映射 authErrorKey 给出本地安全文案。
  return t(authErrorKey(auth.errorCode.value ?? 'OTHER'));
}

async function submitLogin(event: Event) {
  event.preventDefault();
  if (busy.value) return;
  busy.value = true;
  formError.value = '';
  const name = username.value;
  const secret = password.value;
  try {
    const ok = await auth.login(name, secret);
    if (!ok) formError.value = mappedFormError(); // 失败时视图仍停在登录表单；成功时表单随视图消失
  } catch {
    formError.value = t('errors.generic');
  } finally {
    password.value = ''; // 每次提交都清空口令输入，失败也不在界面残留
    busy.value = false;
  }
}

async function submitPassword(event: Event) {
  event.preventDefault();
  if (busy.value) return;
  busy.value = true;
  formError.value = '';
  const current = currentPassword.value;
  const next = newPassword.value;
  try {
    const ok = await auth.changePassword(current, next);
    if (!ok) formError.value = mappedFormError();
    // 结果未知（auth.status 落为 error）时视图整体切到错误提示，绝不虚报成功。
  } catch {
    formError.value = t('errors.generic');
  } finally {
    currentPassword.value = '';
    newPassword.value = '';
    busy.value = false;
  }
}

async function submitLogout() {
  if (busy.value) return;
  busy.value = true;
  formError.value = '';
  try {
    await auth.logout(); // 网络失败时 auth.status 为 error，视图显示重试而不是 signed_out
  } finally {
    username.value = '';
    password.value = '';
    currentPassword.value = '';
    newPassword.value = '';
    busy.value = false;
  }
}

async function revokeSingle(sessionId: string) {
  if (busy.value) return;
  busy.value = true;
  revokingSessionId.value = sessionId;
  try {
    await auth.revokeSession(sessionId);
  } finally {
    revokingSessionId.value = null;
    busy.value = false;
  }
}

async function revokeOthers() {
  if (busy.value) return;
  busy.value = true;
  try {
    await auth.revokeOtherSessions();
  } finally {
    busy.value = false;
  }
}

async function loadMoreSessions() {
  if (busy.value || !auth.sessionsNextCursor.value) return;
  busy.value = true;
  try {
    await auth.listSessions(auth.sessionsNextCursor.value);
  } finally {
    busy.value = false;
  }
}

onMounted(() => { void auth.refresh(); });
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
    <template v-if="auth.status.value === 'checking' || auth.status.value === 'error'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="auth.status.value === 'checking' ? t('state.loading') : t('state.error')">
        <span v-if="auth.status.value === 'checking'">{{ t('auth.checking') }}</span>
        <span v-else>{{ t('auth.error.generic') }}</span>
        <BaseButton v-if="auth.status.value === 'error'" class="auth-retry" :loading-label="t('button.loading')" @click="() => void auth.refresh()">
          {{ t('state.retry') }}
        </BaseButton>
      </AppNotice>
    </template>
    <form v-else-if="auth.status.value === 'signed_out'" class="auth-form" novalidate :aria-label="t('auth.login.title')" @submit.prevent="submitLogin">
      <h2>{{ t('auth.login.title') }}</h2>
      <p v-if="formError" role="alert" class="auth-form__error">{{ formError }}</p>
      <BaseInput id="auth-username" v-model="username" :label="t('auth.username.label')"
        :hint="t('auth.username.hint')" required :disabled="busy" />
      <BaseInput id="auth-password" v-model="password" :label="t('auth.password.label')"
        type="password" required :error="formError || undefined" :disabled="busy" />
      <BaseButton type="submit" :loading="busy" :loading-label="t('button.loading')">{{ t('auth.login.submit') }}</BaseButton>
    </form>
    <form v-else-if="auth.status.value === 'force_password'" class="auth-form" novalidate :aria-label="t('auth.passwordChange.title')" @submit.prevent="submitPassword">
      <h2>{{ t('auth.passwordChange.title') }}</h2>
      <p class="auth-form__forced">{{ t('auth.passwordChange.forced') }}</p>
      <p v-if="formError" role="alert" class="auth-form__error">{{ formError }}</p>
      <BaseInput id="auth-current-password" v-model="currentPassword" :label="t('auth.currentPassword.label')"
        type="password" required :error="formError || undefined" :disabled="busy" />
      <BaseInput id="auth-new-password" v-model="newPassword" :label="t('auth.newPassword.label')"
        :hint="t('auth.newPassword.hint')" type="password" required :error="formError || undefined" :disabled="busy" />
      <div class="auth-form__actions">
        <BaseButton type="submit" :loading="busy" :loading-label="t('button.loading')">{{ t('auth.passwordChange.submit') }}</BaseButton>
        <BaseButton variant="secondary" :disabled="busy" @click="submitLogout">{{ t('auth.logout.label') }}</BaseButton>
      </div>
    </form>
    <section v-else class="auth-signed-in">
      <h2>{{ t('auth.signedIn.heading') }}</h2>
      <p role="status">{{ auth.user.value?.display_name || auth.user.value?.username }}</p>
      <BaseButton :loading="busy" :loading-label="t('button.loading')" @click="submitLogout">{{ t('auth.logout.label') }}</BaseButton>

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
.auth-form { display: grid; gap: var(--space-4); min-width: 0; max-width: 34rem; }
.auth-form__error { color: var(--color-danger, var(--color-surface)); margin: 0; }
.auth-form__forced { margin: 0; }
.auth-form__actions { display: flex; gap: var(--space-2); flex-wrap: wrap; }
.auth-signed-in { display: grid; gap: var(--space-4); min-width: 0; }
.auth-retry { margin-inline-start: auto; }
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
</style>

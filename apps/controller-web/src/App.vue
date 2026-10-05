<script setup lang="ts">
import { ref } from 'vue';
import AppNotice from './components/AppNotice.vue';
import AsyncState from './components/AsyncState.vue';
import BaseButton from './components/BaseButton.vue';
import BaseInput from './components/BaseInput.vue';
import { createI18n, type Locale } from './i18n';

const { locale, t, setLocale } = createI18n();
const sampleValue = ref('');
const exampleState = ref<'error' | 'empty'>('error');

function changeLocale(event: Event) {
  setLocale((event.target as HTMLSelectElement).value as Locale);
}
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
    <p>{{ t('app.foundation') }}</p>
    <p>{{ t('app.notConnected') }}</p>
    <section class="app-showcase" :aria-label="t('notice.title')">
      <AppNotice :tone-label="locale === 'en' ? 'Information' : '提示'" :title="t('notice.title')">
        {{ t('app.foundation') }}
      </AppNotice>
      <BaseInput
        id="foundation-example"
        v-model="sampleValue"
        :label="t('field.label')"
        :hint="t('field.hint')"
      />
      <div class="app-showcase__actions">
        <BaseButton>{{ t('button.primary') }}</BaseButton>
        <BaseButton variant="secondary" loading :loading-label="t('button.loading')">
          {{ t('button.secondary') }}
        </BaseButton>
      </div>
      <p>{{ t('notice.body') }}</p>
      <AsyncState
        :state="exampleState"
        :title="t(exampleState === 'error' ? 'state.error' : 'state.empty')"
        :description="t('notice.body')"
        :retry-label="exampleState === 'error' ? t('state.retry') : undefined"
        @retry="exampleState = 'empty'"
      />
    </section>
  </main>
  <footer class="app-footer">{{ t('app.foundation') }}</footer>
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
.app-showcase { display: grid; gap: var(--space-4); min-width: 0; }
.app-showcase__actions { display: flex; flex-wrap: wrap; gap: var(--space-2); min-width: 0; }
.app-showcase > p { margin: 0; overflow-wrap: anywhere; }
</style>

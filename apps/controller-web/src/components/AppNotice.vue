<script setup lang="ts">
withDefaults(defineProps<{
  tone?: 'info' | 'success' | 'warning' | 'error';
  toneLabel: string;
  title?: string;
}>(), { tone: 'info' });
</script>

<template>
  <aside :role="tone === 'error' ? 'alert' : 'status'" :data-tone="tone" :class="['app-notice', `app-notice--${tone}`]">
    <strong class="app-notice__tone">{{ toneLabel }}</strong>
    <strong v-if="title" class="app-notice__title">{{ title }}</strong>
    <div class="app-notice__body"><slot /></div>
  </aside>
</template>

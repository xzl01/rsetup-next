<script setup lang="ts">
defineProps<{
  state: 'loading' | 'empty' | 'error';
  title: string;
  description?: string;
  retryLabel?: string;
}>();
const emit = defineEmits<{ retry: [] }>();
</script>

<template>
  <section :role="state === 'error' ? 'alert' : 'status'" :data-state="state" :class="['async-state', `async-state--${state}`]">
    <h2 class="async-state__title">{{ title }}</h2>
    <p v-if="description" class="async-state__description">{{ description }}</p>
    <button v-if="state === 'error' && retryLabel" type="button" class="async-state__retry" @click="emit('retry')">{{ retryLabel }}</button>
  </section>
</template>

<script setup lang="ts">
withDefaults(defineProps<{
  type?: 'button' | 'submit' | 'reset';
  variant?: 'primary' | 'secondary' | 'danger';
  disabled?: boolean;
  loading?: boolean;
  loadingLabel?: string;
}>(), {
  type: 'button',
  variant: 'primary',
  disabled: false,
  loading: false,
});

const emit = defineEmits<{ click: [event: MouseEvent] }>();
</script>

<template>
  <button
    :type="type"
    :class="['base-button', `base-button--${variant}`]"
    :disabled="disabled || loading"
    :aria-busy="loading"
    @click="!disabled && !loading && emit('click', $event)"
  >
    <slot />
    <span v-if="loading && loadingLabel" class="base-button__loading">{{ loadingLabel }}</span>
  </button>
</template>

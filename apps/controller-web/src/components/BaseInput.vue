<script setup lang="ts">
import { computed } from 'vue';

const props = withDefaults(defineProps<{
  id: string;
  label: string;
  modelValue: string;
  type?: 'text' | 'password';
  hint?: string;
  error?: string;
  required?: boolean;
  disabled?: boolean;
}>(), { type: 'text', required: false, disabled: false });
const emit = defineEmits<{ 'update:modelValue': [value: string] }>();
const describedBy = computed(() => [props.hint && `${props.id}-hint`, props.error && `${props.id}-error`].filter(Boolean).join(' ') || undefined);
</script>

<template>
  <div class="base-input">
    <label class="base-input__label" :for="id">{{ label }}</label>
    <input
      :id="id"
      class="base-input__control"
      :type="type"
      :value="modelValue"
      :required="required"
      :disabled="disabled"
      :aria-invalid="error ? 'true' : undefined"
      :aria-describedby="describedBy"
      @input="emit('update:modelValue', ($event.target as HTMLInputElement).value)"
    />
    <p v-if="hint" :id="`${id}-hint`" class="base-input__hint">{{ hint }}</p>
    <p v-if="error" :id="`${id}-error`" class="base-input__error">{{ error }}</p>
  </div>
</template>

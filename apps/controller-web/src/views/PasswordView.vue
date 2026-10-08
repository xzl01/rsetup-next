<script setup lang="ts">
import { ref } from 'vue'
import BaseInput from '../components/BaseInput.vue'
import BaseButton from '../components/BaseButton.vue'
import { authErrorKey, type AuthStore } from '../auth'

const props = defineProps<{
  auth: AuthStore
  t: (key: string, params?: Record<string, string | number>) => string
}>()

const currentPassword = ref('')
const newPassword = ref('')
const formError = ref('')
const busy = ref(false)

function mappedFormError(): string {
  return props.t(authErrorKey(props.auth.errorCode.value ?? 'OTHER'))
}

async function submitPassword(event?: Event) {
  if (event) event.preventDefault()
  if (busy.value) return
  busy.value = true
  formError.value = ''
  const current = currentPassword.value
  const next = newPassword.value
  try {
    const ok = await props.auth.changePassword(current, next)
    if (!ok) formError.value = mappedFormError()
  } catch {
    formError.value = props.t('errors.generic')
  } finally {
    currentPassword.value = ''
    newPassword.value = ''
    busy.value = false
  }
}

async function submitLogout() {
  if (busy.value) return
  busy.value = true
  formError.value = ''
  try {
    await props.auth.logout()
  } finally {
    currentPassword.value = ''
    newPassword.value = ''
    busy.value = false
  }
}
</script>

<template>
  <form class="auth-form" novalidate :aria-label="t('auth.passwordChange.title')" @submit.prevent="submitPassword">
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
</template>

<style scoped>
.auth-form { display: grid; gap: var(--space-4); min-width: 0; max-width: 34rem; }
.auth-form__error { color: var(--color-danger, var(--color-surface)); margin: 0; }
.auth-form__forced { margin: 0; }
.auth-form__actions { display: flex; gap: var(--space-2); flex-wrap: wrap; }
</style>

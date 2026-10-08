<script setup lang="ts">
import { computed, ref } from 'vue'
import BaseInput from '../components/BaseInput.vue'
import BaseButton from '../components/BaseButton.vue'
import { authErrorKey, type AuthStore } from '../auth'

const props = defineProps<{
  auth: AuthStore
  t: (key: string, params?: Record<string, string | number>) => string
}>()

const username = ref('')
const password = ref('')
const busy = ref(false)

const formError = computed(() => {
  if (props.auth.errorCode.value && props.auth.status.value === 'signed_out') {
    return props.t(authErrorKey(props.auth.errorCode.value))
  }
  return ''
})

async function submitLogin(event?: Event) {
  if (event) event.preventDefault()
  if (busy.value) return
  busy.value = true
  const name = username.value
  const secret = password.value
  try {
    await props.auth.login(name, secret)
  } catch {
    //
  } finally {
    password.value = ''
    busy.value = false
  }
}
</script>

<template>
  <form class="auth-form" novalidate :aria-label="t('auth.login.title')" @submit.prevent="submitLogin">
    <h2>{{ t('auth.login.title') }}</h2>
    <p v-if="formError" role="alert" class="auth-form__error">{{ formError }}</p>
    <BaseInput id="auth-username" v-model="username" :label="t('auth.username.label')"
      :hint="t('auth.username.hint')" required :disabled="busy" />
    <BaseInput id="auth-password" v-model="password" :label="t('auth.password.label')"
      type="password" required :error="formError || undefined" :disabled="busy" />
    <BaseButton type="submit" :loading="busy" :loading-label="t('button.loading')">{{ t('auth.login.submit') }}</BaseButton>
  </form>
</template>

<style scoped>
.auth-form { display: grid; gap: var(--space-4); min-width: 0; max-width: 34rem; }
.auth-form__error { color: var(--color-danger, var(--color-surface)); margin: 0; }
</style>

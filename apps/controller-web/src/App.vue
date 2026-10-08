<script setup lang="ts">
import { onMounted, onUnmounted, provide, ref, watch } from 'vue'
import AppNotice from './components/AppNotice.vue'
import BaseButton from './components/BaseButton.vue'
import LoginView from './views/LoginView.vue'
import PasswordView from './views/PasswordView.vue'
import SessionsView from './views/SessionsView.vue'
import DevicesView from './views/DevicesView.vue'
import DeviceDetailView from './views/DeviceDetailView.vue'
import { createI18n, type Locale } from './i18n'
import { createAuth } from './auth'
import { createRouter, type AppRoute } from './router'
import { get } from './api'
import { assertDeviceListPageShape, type DeviceItem, type DeviceListPage } from './types'

const i18nInstance = createI18n()
const { locale, t, setLocale } = i18nInstance
provide('i18n', i18nInstance)

const auth = createAuth()
const router = createRouter(auth)

const busy = ref(false)
const sessionChecked = ref(false)

const devicesList = ref<DeviceItem[]>([])
const devicesNextCursor = ref<string | null>(null)
const devicesLoading = ref(false)
const devicesError = ref<string | null>(null)
let devicesGeneration = 0

async function loadDevices() {
  if (devicesLoading.value) return
  devicesLoading.value = true
  devicesError.value = null
  const currentGen = ++devicesGeneration
  try {
    const res = await get<DeviceListPage>('devices')
    assertDeviceListPageShape(res.data)
    if (currentGen !== devicesGeneration || auth.status.value !== 'signed_in') return
    devicesList.value = res.data.items
    devicesNextCursor.value = res.data.next_cursor
  } catch (err: unknown) {
    if (currentGen !== devicesGeneration || auth.status.value !== 'signed_in') return
    devicesError.value = t('state.error')
    devicesList.value = []
    devicesNextCursor.value = null
  } finally {
    if (currentGen === devicesGeneration) {
      devicesLoading.value = false
    }
  }
}

// 得到特定设备的权限投影
function getDevicePermissions(deviceId: string): string[] | undefined {
  const item = devicesList.value.find(d => d.device_id === deviceId)
  return item ? item.effective_permissions : undefined
}

function handlePageLoaded(page: DeviceListPage) {
  if (auth.status.value !== 'signed_in') return
  const existingIds = new Set(devicesList.value.map(d => d.device_id))
  const newItems = page.items.filter(d => !existingIds.has(d.device_id))
  devicesList.value = [...devicesList.value, ...newItems]
  devicesNextCursor.value = page.next_cursor
}

watch(
  [() => auth.status.value, () => router.currentRoute.value.name],
  async ([newStatus, routeName]) => {
    if (newStatus === 'signed_in') {
      if (routeName === 'sessions') {
        void auth.listSessions()
      } else if (routeName === 'devices') {
        void loadDevices()
      } else if (routeName === 'device-detail') {
        const targetId = (router.currentRoute.value as { params?: { id?: string } }).params?.id
        // Deep link 进入详情路由：如果当前还没有该设备的授权缓存，且尚未加载列表，先获取受控最小列表
        if (!getDevicePermissions(targetId || '') && !devicesLoading.value) {
          await loadDevices()
        }
        // 如果确认没有该设备的授权投影，安全重定向回 /devices 列表，且在此之前不挂载 DeviceDetailView
        if (targetId && !getDevicePermissions(targetId)) {
          router.navigate({ name: 'devices' })
        }
      }
    } else {
      // 登出/非 signed_in 状态立即清理设备列表与失效代际
      devicesGeneration++
      devicesList.value = []
      devicesNextCursor.value = null
      devicesError.value = null
      devicesLoading.value = false
    }
  },
  { immediate: true },
)

function changeLocale(event: Event) {
  setLocale((event.target as HTMLSelectElement).value as Locale)
}

function handleSkipLink(event: Event) {
  event.preventDefault()
  const main = document.getElementById('main-content')
  if (main) {
    main.focus()
  }
}

function navigateTo(route: AppRoute) {
  router.navigate(route)
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

async function retryRefresh() {
  sessionChecked.value = false
  try {
    await auth.refresh()
  } finally {
    sessionChecked.value = true
  }
}

onMounted(async () => {
  try {
    await auth.refresh()
  } finally {
    sessionChecked.value = true
  }
})

onUnmounted(() => {
  router.cleanup()
})
</script>

<template>
  <a class="skip-link" href="#main-content" @click.prevent="handleSkipLink" @keydown.enter.prevent="handleSkipLink">{{ t('nav.skip') }}</a>
  <header class="app-header">
    <h1>{{ t('app.title') }}</h1>
    <label for="app-language">{{ t('language.label') }}</label>
    <select id="app-language" :value="locale" @change="changeLocale">
      <option value="zh-CN">简体中文</option>
      <option value="en">English</option>
    </select>
  </header>
  <main id="main-content" class="app-main" tabindex="-1">
    <template v-if="!sessionChecked && auth.status.value === 'checking'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="t('state.loading')">
        <span>{{ t('auth.checking') }}</span>
      </AppNotice>
    </template>
    <template v-else-if="auth.status.value === 'error'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="t('state.error')">
        <span>{{ t('auth.error.generic') }}</span>
        <BaseButton class="auth-retry" :loading-label="t('button.loading')" @click="retryRefresh">
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
      <nav :aria-label="t('auth.sessions.title')" class="auth-nav">
        <BaseButton
          variant="secondary"
          :disabled="busy"
          @click="() => navigateTo({ name: 'devices' })"
        >
          {{ t('nav.devices') }}
        </BaseButton>
        <BaseButton
          variant="secondary"
          :disabled="busy"
          @click="() => navigateTo({ name: 'sessions' })"
        >
          {{ t('auth.sessions.title') }}
        </BaseButton>
      </nav>
      <BaseButton :loading="busy" :loading-label="t('button.loading')" @click="submitLogout">{{ t('auth.logout.label') }}</BaseButton>

      <template v-if="router.currentRoute.value.name === 'sessions'">
        <SessionsView :auth="auth" :locale="locale" :t="t" />
      </template>
      <template v-else-if="router.currentRoute.value.name === 'devices'">
        <DevicesView
          :initial-devices="devicesList"
          :initial-next-cursor="devicesNextCursor"
          :loading="devicesLoading"
          :error="devicesError"
          @retry="loadDevices"
          @page-loaded="handlePageLoaded"
          @select-device="(id) => navigateTo({ name: 'device-detail', params: { id } })"
        />
      </template>
      <template v-else-if="router.currentRoute.value.name === 'device-detail'">
        <DeviceDetailView
          v-if="getDevicePermissions(router.currentRoute.value.params.id)"
          :device-id="router.currentRoute.value.params.id"
          :permissions="getDevicePermissions(router.currentRoute.value.params.id)"
        />
      </template>
      <template v-else>
        <div class="app-placeholder">
          <p>{{ t('app.notConnected') }}</p>
        </div>
      </template>
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
.auth-nav { display: flex; gap: var(--space-2); }
.auth-retry { margin-inline-start: auto; }
.app-placeholder { display: grid; gap: var(--space-2); padding: var(--space-4); border: 1px dashed var(--color-border, #ccc); border-radius: 4px; }
.app-placeholder p { margin: 0; color: var(--color-text-muted, #666); }
</style>

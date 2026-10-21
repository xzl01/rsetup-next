<script setup lang="ts">
import { computed } from 'vue'
import type { EventConnectionTone } from '../event-integration'
import type { Locale } from '../i18n'

/**
 * SSE 连接状态指示器（AT-18 / 06 §4「断线或未同步不显示实时」）。
 *
 * 文案刻意留在组件内部的局部中英映射：本任务不改动全局词典与 i18n 资源，
 * 只显示由 `resolveConnectionTone` 折叠出的档位，组件自身绝不推断「实时」。
 * 不使用 role="status"：已登录身份条已占用该播报位（同屏唯一），这里用 aria-live 增量播报。
 */
const props = defineProps<{
  tone: EventConnectionTone
  locale: Locale
}>()

const LABELS: Record<EventConnectionTone, { zh: string; en: string }> = {
  realtime: { zh: '实时：事件通道已连接，数据已同步', en: 'Live: event stream connected and synced' },
  syncing: { zh: '非实时：正在同步当前授权快照', en: 'Not live: syncing the authorized snapshot' },
  connecting: { zh: '非实时：正在建立事件通道', en: 'Not live: connecting to the event stream' },
  reconnecting: { zh: '非实时：事件通道重连中，数据可能已过期', en: 'Not live: event stream reconnecting, data may be stale' },
  stale: { zh: '非实时：快照未完成同步，请重试或刷新', en: 'Not live: snapshot not synced, retry or refresh' },
  unsupported: {
    zh: '非实时：当前环境不支持事件通道，仍可按普通请求读取',
    en: 'Not live: this environment has no event stream; regular requests still work',
  },
  unauthorized: { zh: '非实时：会话已失效，需要重新登录', en: 'Not live: session expired, please sign in again' },
  disconnected: { zh: '非实时：未连接事件通道', en: 'Not live: event stream not connected' },
}

const label = computed(() => {
  const entry = LABELS[props.tone] ?? LABELS.disconnected
  return props.locale === 'en' ? entry.en : entry.zh
})
</script>

<template>
  <p
    class="event-connection-state"
    :class="`event-connection-state--${tone}`"
    data-testid="event-connection-state"
    aria-live="polite"
    aria-atomic="true"
  >
    {{ label }}
  </p>
</template>

<style scoped>
.event-connection-state {
  margin: 0;
  font-size: 0.875rem;
  color: var(--color-text-muted, #666);
}
.event-connection-state--realtime { color: var(--color-success, #1a7f37); }
.event-connection-state--unauthorized,
.event-connection-state--stale { color: var(--color-danger, #b3261e); }
</style>

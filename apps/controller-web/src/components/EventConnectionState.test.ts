import { render, screen } from '@testing-library/vue'
import { describe, expect, it } from 'vitest'

import EventConnectionState from './EventConnectionState.vue'
import type { EventConnectionTone } from '../event-integration'

/**
 * AT-18 / 06 §4「断线或未同步绝不显示实时」的呈现层回归。
 * 文案走组件内局部中英映射，不进入全局词典（locales/i18n 不在本任务写范围）。
 */
const TONES: EventConnectionTone[] = [
  'realtime', 'syncing', 'connecting', 'reconnecting', 'stale', 'unsupported', 'unauthorized', 'disconnected',
]

function textOf(tone: EventConnectionTone, locale: 'zh-CN' | 'en'): string {
  const { unmount } = render(EventConnectionState, { props: { tone, locale } })
  const text = screen.getByTestId('event-connection-state').textContent ?? ''
  unmount()
  return text
}

describe('EventConnectionState', () => {
  it('非实时态文案一律明示「非实时」，绝不伪装实时', () => {
    for (const tone of TONES) {
      const zh = textOf(tone, 'zh-CN')
      const en = textOf(tone, 'en')
      if (tone === 'realtime') {
        expect(zh).toContain('实时：')
        expect(zh).not.toContain('非实时')
        expect(en).toContain('Live')
        expect(en).not.toContain('Not live')
        continue
      }
      expect(`${tone}:${zh}`).toContain('非实时')
      expect(`${tone}:${en}`).toContain('Not live')
    }
  })

  it('缺 SSE 能力时诚实说明不可用并保留普通读取语义', () => {
    const zh = textOf('unsupported', 'zh-CN')
    expect(zh).toContain('不支持')
    expect(zh).toContain('普通请求')
    expect(textOf('unsupported', 'en')).toContain('event stream')
  })

  it('会话失效态提示重新登录', () => {
    expect(textOf('unauthorized', 'zh-CN')).toContain('重新登录')
    expect(textOf('unauthorized', 'en')).toContain('sign in')
  })

  it('按当前 locale 切换文案', () => {
    expect(textOf('reconnecting', 'zh-CN')).toContain('重连')
    expect(textOf('reconnecting', 'en')).toContain('reconnecting')
  })

  it('tone 变化即更新文案', () => {
    const { rerender } = render(EventConnectionState, { props: { tone: 'stale', locale: 'zh-CN' } as never })
    expect(screen.getByTestId('event-connection-state').textContent).toContain('非实时')
    rerender({ tone: 'realtime', locale: 'zh-CN' })
    expect(screen.getByTestId('event-connection-state').textContent).toContain('实时：')
  })

  it('只读呈现：不新增按钮/表单，也不占用页面唯一的 role=status 播报位', () => {
    render(EventConnectionState, { props: { tone: 'stale', locale: 'zh-CN' } as never })
    const node = screen.getByTestId('event-connection-state')
    expect(node.querySelector('button')).toBeNull()
    expect(node.getAttribute('role')).toBeNull()
    expect(node.getAttribute('aria-live')).toBe('polite')
  })
})

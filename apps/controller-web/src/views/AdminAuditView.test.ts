import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminAuditView from './AdminAuditView.vue'
import { createI18n } from '../i18n'
import { DEVICE_A, USER_ID, USER_ID_B, fail, nextTick, ok, stubBackend } from './admin-test-helpers'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const EPOCH = '01234567-89ab-4def-8f01-234567890ac2'
const EVENT_ID = '01234567-89ab-4def-8f01-234567890ac3'
const EVENT_ID_B = '01234567-89ab-4def-8f01-234567890ac4'

function event(overrides: Record<string, unknown> = {}) {
  return {
    id: EVENT_ID,
    actor_kind: 'user',
    actor_user_id: USER_ID,
    event_type: 'identity.user.create',
    target_kind: 'user',
    target_id: USER_ID_B,
    params_redacted: '{"username":"ops"}',
    outcome: 'success',
    process_epoch: EPOCH,
    event_seq: '17',
    time_evidence: null,
    ...overrides,
  }
}

function renderView(isAdmin = true) {
  return render(AdminAuditView, {
    props: { csrfToken: 'csrf-token-1', isAdmin },
    global: { provide: { i18n } },
  })
}

function byId(id: string): HTMLInputElement {
  return document.getElementById(id) as HTMLInputElement
}

afterEach(() => vi.unstubAllGlobals())

describe('AdminAuditView', () => {
  it('按复合游标页呈现审计事件：序号与进程代际原样保留字符串', async () => {
    const calls = stubBackend({ 'GET /api/v1/audit': [ok({ items: [event()], next_cursor: 'cursor-2' })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`audit-${EVENT_ID}`)).toBeTruthy())
    expect(calls[0]?.url).toBe('/api/v1/audit')
    const row = screen.getByTestId(`audit-${EVENT_ID}`)
    expect(row.textContent).toContain('identity.user.create')
    expect(row.textContent).toContain('17')
    expect(row.textContent).toContain(EPOCH)
    expect(row.textContent).toContain('{"username":"ops"}')
    expect(screen.getByText(/复合游标/)).toBeTruthy()
  })

  it('非管理员不发请求，只提示无权限', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    expect(calls).toHaveLength(0)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('加载更多沿用服务端签名游标并累加行', async () => {
    const calls = stubBackend({
      'GET /api/v1/audit': [
        ok({ items: [event()], next_cursor: 'signed-cursor-2' }),
        ok({ items: [event({ id: EVENT_ID_B, event_seq: '18' })], next_cursor: null }),
      ],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '加载更多' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '加载更多' }))
    await vi.waitFor(() => expect(screen.getByTestId(`audit-${EVENT_ID_B}`)).toBeTruthy())
    expect(calls[1]?.url).toBe('/api/v1/audit?cursor=signed-cursor-2')
    expect(screen.getByTestId(`audit-${EVENT_ID}`)).toBeTruthy()
    // 末页游标为空后不再提供加载更多
    await vi.waitFor(() => expect(screen.queryByRole('button', { name: '加载更多' })).toBeNull())
  })

  it('应用筛选后带 actor_id/device_id 重新从第一页加载', async () => {
    const calls = stubBackend({ 'GET /api/v1/audit': [ok({ items: [], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '应用筛选' })).toBeTruthy())
    await fireEvent.update(byId('audit-filter-actor'), USER_ID)
    await fireEvent.update(byId('audit-filter-device'), DEVICE_A)
    await fireEvent.click(screen.getByRole('button', { name: '应用筛选' }))
    await vi.waitFor(() => expect(calls.some((call) =>
      call.url === `/api/v1/audit?actor_id=${USER_ID}&device_id=${DEVICE_A}`)).toBe(true))
  })

  it('筛选条件非法时前端拦截，不发新请求', async () => {
    const calls = stubBackend({ 'GET /api/v1/audit': [ok({ items: [], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '应用筛选' })).toBeTruthy())
    await fireEvent.update(byId('audit-filter-actor'), 'not-a-uuid')
    await fireEvent.click(screen.getByRole('button', { name: '应用筛选' }))
    await nextTick()
    expect(screen.getByTestId('audit-filter-invalid')).toBeTruthy()
    expect(calls.filter((call) => call.method === 'GET')).toHaveLength(1)

    await fireEvent.update(byId('audit-filter-actor'), '')
    await fireEvent.update(byId('audit-filter-device'), 'zzz')
    await fireEvent.click(screen.getByRole('button', { name: '应用筛选' }))
    await nextTick()
    expect(calls.filter((call) => call.method === 'GET')).toHaveLength(1)
  })

  it('审计存储不可用时按错误码提示并可重试', async () => {
    stubBackend({ 'GET /api/v1/audit': [fail(503, 'STORAGE_UNAVAILABLE')] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '重试' })).toBeTruthy())
    expect(screen.getByText('存储暂时不可用')).toBeTruthy()
  })

  it('空结果集显示空态而不是错误', async () => {
    stubBackend({ 'GET /api/v1/audit': [ok({ items: [], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByText('暂无记录')).toBeTruthy())
    expect(screen.queryByRole('button', { name: '重试' })).toBeNull()
  })

  it('未授权时头部刷新按钮禁用且点击零请求（二次权限门禁）', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    const reloadButton = screen.getByTestId('audit-reload') as HTMLButtonElement
    await fireEvent.click(reloadButton)
    await fireEvent.click(reloadButton)
    await nextTick()
    // 未授权时任何触发路径都必须零请求
    expect(calls).toHaveLength(0)
    expect(reloadButton.disabled).toBe(true)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('运行中被降权后点击刷新仍零请求', async () => {
    const calls = stubBackend({ 'GET /api/v1/audit': [ok({ items: [event()], next_cursor: null })] })
    const { rerender } = renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`audit-${EVENT_ID}`)).toBeTruthy())
    await rerender({ isAdmin: false })
    await nextTick()
    const before = calls.length
    const reloadButton = screen.getByTestId('audit-reload') as HTMLButtonElement
    await fireEvent.click(reloadButton)
    await nextTick()
    // 撤权后点击刷新必须零请求
    expect(calls).toHaveLength(before)
    expect(reloadButton.disabled).toBe(true)
  })
})

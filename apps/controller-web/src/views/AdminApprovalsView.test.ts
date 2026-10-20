import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminApprovalsView from './AdminApprovalsView.vue'
import { createI18n } from '../i18n'
import { fail, nextTick, ok, stubBackend } from './admin-test-helpers'

const DEVICE_A = 'a'.repeat(64)
const DEVICE_B = 'b'.repeat(64)
const DEVICE_C = 'c'.repeat(64)

function item(deviceId: string, revision: string, decision = 'none') {
  return {
    device_id: deviceId,
    fingerprint: deviceId,
    descriptor: null,
    decision,
    connection_state: 'online',
    revision,
  }
}

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

function renderView(isAdmin = true) {
  return render(AdminApprovalsView, {
    props: { csrfToken: 'csrf-token-1', isAdmin },
    global: { provide: { i18n } },
  })
}

afterEach(() => vi.unstubAllGlobals())

describe('AdminApprovalsView', () => {
  it('管理员进入时拉取审批列表并按五列呈现设备指纹与审核决定', async () => {
    const calls = stubBackend({ 'GET /api/v1/approvals': [ok({ items: [item(DEVICE_A, '3')], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())
    expect(calls[0]?.url).toBe('/api/v1/approvals')
    // 筛选下拉与列表行各出现一次本地化决定文案
    expect(screen.getAllByText('未决定').length).toBeGreaterThanOrEqual(2)
    expect(screen.getByTestId('revoke-manual-reset-notice').textContent)
      .toContain('吊销或重新授权需要板端人工重置')
  })

  it('非管理员：不发任何请求，只显示无权限提示', async () => {
    const calls = stubBackend({})
    renderView(false)
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(calls).toHaveLength(0)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('勾选时冻结 expected_revision：后台刷新改变版本或新增设备都不改变提交目标', async () => {
    const calls = stubBackend({
      'GET /api/v1/approvals': [
        ok({ items: [item(DEVICE_A, '1'), item(DEVICE_B, '2')], next_cursor: null }),
        ok({ items: [item(DEVICE_A, '9'), item(DEVICE_B, '2'), item(DEVICE_C, '1')], next_cursor: null }),
      ],
      'POST /api/v1/approvals/approve': [ok({ results: [{ device_id: DEVICE_A, ok: true, revision: '9' }] })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())

    await fireEvent.click(screen.getByTestId(`select-${DEVICE_A}`))
    expect(screen.getByTestId('frozen-selection-notice')).toBeTruthy()

    // 后台刷新：DEVICE_A 版本变为 '9'，并新增了 DEVICE_C
    await fireEvent.click(screen.getByRole('button', { name: '刷新' }))
    await vi.waitFor(() => expect(screen.getByTestId(`stale-${DEVICE_A}`)).toBeTruthy())
    expect(screen.getByTestId(`stale-${DEVICE_A}`).textContent).toContain('该设备版本已变更')
    // 新增设备不会被自动勾选
    expect((screen.getByTestId(`select-${DEVICE_C}`) as HTMLInputElement).checked).toBe(false)

    await fireEvent.click(screen.getByLabelText('我已确认所选设备与冻结版本'))
    await fireEvent.click(screen.getByRole('button', { name: '批准所选' }))

    await vi.waitFor(() => expect(calls.some((call) => call.method === 'POST'
      && call.url === '/api/v1/approvals/approve')).toBe(true))
    const approveCall = calls.find((call) => call.url === '/api/v1/approvals/approve')
    expect(approveCall?.body).toEqual({
      targets: [{ device_id: DEVICE_A, expected_revision: '1' }],
      confirm: true,
    })
  })

  it('未勾选风险确认时批准按钮保持禁用，勾选后可提交', async () => {
    stubBackend({
      'GET /api/v1/approvals': [ok({ items: [item(DEVICE_A, '1')], next_cursor: null })],
      'POST /api/v1/approvals/approve': [ok({ results: [] })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())
    const approveButton = screen.getByRole('button', { name: '批准所选' }) as HTMLButtonElement
    expect(approveButton.disabled).toBe(true)
    await fireEvent.click(screen.getByTestId(`select-${DEVICE_A}`))
    expect(approveButton.disabled).toBe(true)
    await fireEvent.click(screen.getByLabelText('我已确认所选设备与冻结版本'))
    expect(approveButton.disabled).toBe(false)
  })

  it('人工拒绝必须填写原因，否则不发请求并给出提示', async () => {
    const calls = stubBackend({
      'GET /api/v1/approvals': [ok({ items: [item(DEVICE_A, '1')], next_cursor: null })],
      'POST /api/v1/approvals/reject': [ok({ results: [] })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())
    await fireEvent.click(screen.getByTestId(`select-${DEVICE_A}`))
    await fireEvent.click(screen.getByLabelText('我已确认所选设备与冻结版本'))
    await fireEvent.click(screen.getByRole('button', { name: '拒绝所选' }))
    await new Promise((resolve) => setTimeout(resolve, 0))
    expect(calls.some((call) => call.url === '/api/v1/approvals/reject')).toBe(false)
    expect(screen.getByText('人工拒绝必须填写非空原因')).toBeTruthy()

    await fireEvent.update(screen.getByLabelText('原因'), '指纹与工单不符')
    await fireEvent.click(screen.getByRole('button', { name: '拒绝所选' }))
    await vi.waitFor(() => expect(calls.some((call) => call.url === '/api/v1/approvals/reject')).toBe(true))
    const rejectCall = calls.find((call) => call.url === '/api/v1/approvals/reject')
    expect(rejectCall?.body).toEqual({
      targets: [{ device_id: DEVICE_A, expected_revision: '1' }],
      reason: '指纹与工单不符',
      confirm: true,
    })
  })

  it('批量逐项结果呈现失败错误码（partial fail）', async () => {
    stubBackend({
      'GET /api/v1/approvals': [ok({ items: [item(DEVICE_A, '1')], next_cursor: null })],
      'POST /api/v1/approvals/approve': [ok({
        results: [{ device_id: DEVICE_A, ok: false, error: { code: 'REVISION_CONFLICT' } }],
      })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())
    await fireEvent.click(screen.getByTestId(`select-${DEVICE_A}`))
    await fireEvent.click(screen.getByLabelText('我已确认所选设备与冻结版本'))
    await fireEvent.click(screen.getByRole('button', { name: '批准所选' }))
    await vi.waitFor(() => expect(screen.getByTestId('batch-partial-fail-notice')).toBeTruthy())
    expect(screen.getByTestId('batch-partial-fail-notice').textContent).toContain('REVISION_CONFLICT')
    expect(screen.getByTestId('batch-partial-fail-notice').textContent).toContain('成功 0 项，失败 1 项')
  })

  it('吊销单台设备时提交 reason 与 confirm，并提示需板端人工重置', async () => {
    const calls = stubBackend({
      'GET /api/v1/approvals': [ok({ items: [item(DEVICE_A, '4')], next_cursor: null })],
      'POST /api/v1/devices/{id}/revoke': [ok({ state: 'REVOKED', requires_device_reset: true })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())
    await fireEvent.update(screen.getByLabelText('原因'), '密钥泄露')
    await fireEvent.click(screen.getByRole('button', { name: '吊销' }))
    await vi.waitFor(() => expect(calls.some((call) => call.url === `/api/v1/devices/${DEVICE_A}/revoke`)).toBe(true))
    const revokeCall = calls.find((call) => call.url === `/api/v1/devices/${DEVICE_A}/revoke`)
    expect(revokeCall?.body).toEqual({ expected_revision: '4', reason: '密钥泄露', confirm: true })
    expect(screen.getByTestId('requires-device-reset-notice').textContent).toContain('板端必须人工重置')
  })

  it('列表请求失败时按错误码呈现可重试提示', async () => {
    stubBackend({ 'GET /api/v1/approvals': [fail(403, 'PERMISSION_DENIED')] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '重试' })).toBeTruthy())
    expect(screen.getByText('没有权限执行此操作')).toBeTruthy()
  })

  it('CSRF token 轮换后清空冻结选择并重新加载', async () => {
    const calls = stubBackend({ 'GET /api/v1/approvals': [ok({ items: [item(DEVICE_A, '1')], next_cursor: null })] })
    const { rerender } = renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())
    await fireEvent.click(screen.getByTestId(`select-${DEVICE_A}`))
    await rerender({ csrfToken: 'csrf-token-2', isAdmin: true })
    await vi.waitFor(() => expect(calls.filter((call) => call.method === 'GET')).toHaveLength(2))
    // 新一轮加载中旧选择已作废
    expect(screen.queryByTestId('frozen-selection-notice')).toBeNull()
  })

  it('未授权时头部刷新按钮禁用且点击零请求（二次权限门禁）', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    const reloadButton = screen.getByTestId('approvals-reload') as HTMLButtonElement
    await fireEvent.click(reloadButton)
    await fireEvent.click(reloadButton)
    await nextTick()
    // 未授权时任何触发路径都必须零请求
    expect(calls).toHaveLength(0)
    expect(reloadButton.disabled).toBe(true)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('运行中被降权后点击刷新仍零请求', async () => {
    const calls = stubBackend({ 'GET /api/v1/approvals': [ok({ items: [item(DEVICE_A, '3')], next_cursor: null })] })
    const { rerender } = renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy())
    await rerender({ isAdmin: false })
    await nextTick()
    const before = calls.length
    const reloadButton = screen.getByTestId('approvals-reload') as HTMLButtonElement
    await fireEvent.click(reloadButton)
    await nextTick()
    // 撤权后点击刷新必须零请求
    expect(calls).toHaveLength(before)
    expect(reloadButton.disabled).toBe(true)
  })
})

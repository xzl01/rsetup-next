import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminSystemView from './AdminSystemView.vue'
import { createI18n } from '../i18n'
import { fail, nextTick, ok, stubBackend } from './admin-test-helpers'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

const STATUS = {
  ready: true,
  schema_version: 7,
  version: '1.4.2',
  recovery_mode: 'normal',
  counts: { users: '1', devices: '2', roles: '3', grants: '4', groups: '5' },
}

function renderView(isAdmin = true) {
  return render(AdminSystemView, {
    props: { csrfToken: 'csrf-token-1', isAdmin },
    global: { provide: { i18n } },
  })
}

afterEach(() => vi.unstubAllGlobals())

describe('AdminSystemView', () => {
  it('读取运行时状态：就绪标记、版本、恢复模式与资源计数', async () => {
    const calls = stubBackend({ 'GET /api/v1/system/status': [ok(STATUS)] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId('system-status')).toBeTruthy())
    expect(calls[0]?.url).toBe('/api/v1/system/status')
    expect(screen.getByTestId('system-ready').textContent).toBe('已就绪')
    expect(screen.getByTestId('system-recovery-mode').textContent).toBe('normal（正常运行）')
    const grid = screen.getByTestId('system-status').textContent ?? ''
    expect(grid).toContain('1.4.2')
    expect(grid).toContain('7')
    expect(grid).toContain('用户数')
    expect(grid).toContain('5')
  })

  it('非管理员不发请求，只提示无权限', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    expect(calls).toHaveLength(0)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
    expect(screen.queryByTestId('system-status')).toBeNull()
  })

  it('NOT_READY：探针不可读时显示未就绪而不是默认正常值', async () => {
    stubBackend({ 'GET /api/v1/system/status': [fail(503, 'NOT_READY')] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId('system-not-ready')).toBeTruthy())
    expect(screen.getByTestId('system-not-ready').textContent).toContain('运行时状态不可读，服务未就绪')
    expect(screen.queryByTestId('system-status')).toBeNull()
    expect(screen.queryByTestId('system-recovery-mode')).toBeNull()
  })

  it('ready=false 时明确显示未就绪而不是成功态', async () => {
    stubBackend({ 'GET /api/v1/system/status': [ok({ ...STATUS, ready: false })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId('system-not-ready-flag')).toBeTruthy())
    expect(screen.getByTestId('system-not-ready-flag').textContent).toBe('未就绪')
    expect(screen.queryByTestId('system-ready')).toBeNull()
  })

  it('未知恢复模式原样显示，不做猜测性翻译', async () => {
    stubBackend({ 'GET /api/v1/system/status': [ok({ ...STATUS, recovery_mode: 'single-user' })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId('system-recovery-mode')).toBeTruthy())
    expect(screen.getByTestId('system-recovery-mode').textContent).toBe('single-user')
  })

  it('刷新按钮重新读取探针', async () => {
    const calls = stubBackend({ 'GET /api/v1/system/status': [ok(STATUS)] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId('system-status')).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '刷新' }))
    await vi.waitFor(() => expect(calls).toHaveLength(2))
  })
})

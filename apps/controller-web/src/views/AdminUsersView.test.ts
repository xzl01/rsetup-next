import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminUsersView from './AdminUsersView.vue'
import { createI18n } from '../i18n'
import { DEVICE_A, USER_ID, USER_ID_B, fail, nextTick, ok, stubBackend } from './admin-test-helpers'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

function user(overrides: Record<string, unknown> = {}) {
  return {
    id: USER_ID,
    username: 'ops',
    display_name: '运维',
    active: true,
    is_admin: false,
    must_change_password: false,
    revision: '2',
    ...overrides,
  }
}

function renderView(isAdmin = true) {
  return render(AdminUsersView, {
    props: { csrfToken: 'csrf-token-1', isAdmin },
    global: { provide: { i18n } },
  })
}

function byId(id: string): HTMLInputElement {
  return document.getElementById(id) as HTMLInputElement
}

afterEach(() => vi.unstubAllGlobals())

describe('AdminUsersView', () => {
  it('管理员进入时拉取用户并显示账号状态标记', async () => {
    const calls = stubBackend({
      'GET /api/v1/users': [ok({
        items: [user(), user({ id: USER_ID_B, username: 'locked', active: false, must_change_password: true })],
        next_cursor: null,
      })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`user-${USER_ID}`)).toBeTruthy())
    expect(calls[0]?.url).toBe('/api/v1/users')
    expect(screen.getByTestId(`user-${USER_ID}`).textContent).toContain('已启用')
    expect(screen.getByTestId(`user-${USER_ID_B}`).textContent).toContain('已停用')
    expect(screen.getByTestId('must-change-password')).toBeTruthy()
  })

  it('非管理员不发请求，只提示无权限', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    expect(calls).toHaveLength(0)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('添加用户提交三字段并在一次性模态框中呈现临时密码', async () => {
    const calls = stubBackend({
      'GET /api/v1/users': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/users': [ok({ user: user(), temporary_password: 'Tmp-Secret-9f3k' })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '添加用户' })).toBeTruthy())
    await fireEvent.update(byId('user-create-username'), 'ops')
    await fireEvent.update(byId('user-create-display-name'), '运维')
    await fireEvent.click(byId('user-create-is-admin'))
    await fireEvent.click(screen.getByRole('button', { name: '添加用户' }))

    await vi.waitFor(() => expect(calls.some((call) => call.method === 'POST')).toBe(true))
    const createCall = calls.find((call) => call.method === 'POST')
    expect(createCall?.url).toBe('/api/v1/users')
    expect(createCall?.body).toEqual({ username: 'ops', display_name: '运维', is_admin: true })

    let dialog!: HTMLElement
    await vi.waitFor(() => {
      dialog = screen.getByTestId('temporary-password-dialog')
      expect(dialog).toBeTruthy()
    })
    expect(dialog.getAttribute('role')).toBe('dialog')
    expect(dialog.getAttribute('aria-modal')).toBe('true')
    expect(screen.getByTestId('temporary-password-value').textContent).toBe('Tmp-Secret-9f3k')
    expect(dialog.textContent).toContain('临时密码只显示这一次')
  })

  it('关闭一次性模态框后临时密码从界面彻底消失', async () => {
    stubBackend({
      'GET /api/v1/users': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/users': [ok({ user: user(), temporary_password: 'Tmp-Secret-9f3k' })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '添加用户' })).toBeTruthy())
    await fireEvent.update(byId('user-create-username'), 'ops')
    await fireEvent.update(byId('user-create-display-name'), '运维')
    await fireEvent.click(screen.getByRole('button', { name: '添加用户' }))
    await vi.waitFor(() => expect(screen.getByTestId('temporary-password-dialog')).toBeTruthy())

    await fireEvent.click(screen.getByTestId('temporary-password-close'))
    await nextTick()
    expect(screen.queryByTestId('temporary-password-dialog')).toBeNull()
    expect(document.body.textContent).not.toContain('Tmp-Secret-9f3k')
  })

  it('Esc 关闭一次性模态框', async () => {
    stubBackend({
      'GET /api/v1/users': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/users': [ok({ user: user(), temporary_password: 'Tmp-Secret-9f3k' })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '添加用户' })).toBeTruthy())
    await fireEvent.update(byId('user-create-username'), 'ops')
    await fireEvent.update(byId('user-create-display-name'), '运维')
    await fireEvent.click(screen.getByRole('button', { name: '添加用户' }))
    await vi.waitFor(() => expect(screen.getByTestId('temporary-password-dialog')).toBeTruthy())
    await fireEvent.keyDown(screen.getByTestId('temporary-password-dialog'), { key: 'Escape' })
    await nextTick()
    expect(screen.queryByTestId('temporary-password-dialog')).toBeNull()
  })

  it('停用账号提交 active:false 的 CAS 补丁', async () => {
    const calls = stubBackend({
      'GET /api/v1/users': [ok({ items: [user()], next_cursor: null })],
      [`PATCH /api/v1/users/${USER_ID}`]: [ok(user({ active: false, revision: '3' }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '停用账号' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '停用账号' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true))
    const patchCall = calls.find((call) => call.method === 'PATCH')
    expect(patchCall?.url).toBe(`/api/v1/users/${USER_ID}`)
    expect(patchCall?.body).toEqual({ expected_revision: '2', active: false })
  })

  it('重置密码后呈现新的临时密码并提示需改密', async () => {
    const calls = stubBackend({
      'GET /api/v1/users': [ok({ items: [user()], next_cursor: null })],
      [`POST /api/v1/users/${USER_ID}/reset-password`]: [ok({
        user: user({ must_change_password: true, revision: '4' }),
        temporary_password: 'Tmp-Restored-1',
      })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '重置密码' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '重置密码' }))
    await vi.waitFor(() => expect(calls.some((call) => call.url.endsWith('/reset-password'))).toBe(true))
    expect(calls.find((call) => call.url.endsWith('/reset-password'))?.body).toEqual({ expected_revision: '2' })
    await vi.waitFor(() => expect(screen.getByTestId('temporary-password-value').textContent).toBe('Tmp-Restored-1'))
  })

  it('最后管理员保护：409 LAST_ADMIN 显式提示，不假装成功', async () => {
    stubBackend({
      'GET /api/v1/users': [ok({ items: [user({ is_admin: true })], next_cursor: null })],
      [`PATCH /api/v1/users/${USER_ID}`]: [fail(409, 'LAST_ADMIN')],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '停用账号' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '停用账号' }))
    await vi.waitFor(() => expect(screen.getByTestId('last-admin-notice')).toBeTruthy())
    expect(screen.getByTestId('last-admin-notice').textContent)
      .toContain('最后一个管理员账号受保护，不能停用或降级')
  })

  it('有效权限来源按设备列出（走 users/{id}/effective-permissions）', async () => {
    const calls = stubBackend({
      'GET /api/v1/users': [ok({ items: [user()], next_cursor: null })],
      [`GET /api/v1/users/${USER_ID}/effective-permissions`]: [ok({
        items: [{ device_id: DEVICE_A, effective_permissions: ['device.read', 'device.reboot'], sources: [] }],
        next_cursor: null,
      })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '有效权限' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '有效权限' }))
    await vi.waitFor(() => expect(screen.getByTestId('effective-permissions-panel')).toBeTruthy())
    expect(calls.some((call) => call.url === `/api/v1/users/${USER_ID}/effective-permissions`)).toBe(true)
    expect(screen.getByTestId(`effective-${DEVICE_A}`).textContent).toContain('device.reboot')
  })
})

import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminRolesView from './AdminRolesView.vue'
import { createI18n } from '../i18n'
import { ROLE_ID, fail, nextTick, ok, stubBackend } from './admin-test-helpers'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const ROLE_ID_B = '01234567-89ab-4def-8f01-234567890ac1'

function role(overrides: Record<string, unknown> = {}) {
  return {
    id: ROLE_ID,
    name: 'operator',
    builtin: false,
    archived: false,
    revision: '1',
    permissions: ['device.read', 'device.reboot'],
    ...overrides,
  }
}

function renderView(isAdmin = true) {
  return render(AdminRolesView, {
    props: { csrfToken: 'csrf-token-1', isAdmin },
    global: { provide: { i18n } },
  })
}

function byId(id: string): HTMLInputElement {
  return document.getElementById(id) as HTMLInputElement
}

afterEach(() => vi.unstubAllGlobals())

describe('AdminRolesView', () => {
  it('渲染角色与权限清单及内置/归档标记', async () => {
    const calls = stubBackend({
      'GET /api/v1/roles': [ok({
        items: [role(), role({ id: ROLE_ID_B, name: 'builtin-admin', builtin: true, archived: true })],
        next_cursor: null,
      })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`role-${ROLE_ID}`)).toBeTruthy())
    expect(calls[0]?.url).toBe('/api/v1/roles')
    const permissionCells = screen.getAllByTestId('role-permissions')
    expect(permissionCells[0]?.textContent).toContain('device.reboot')
    expect(screen.getByTestId(`role-${ROLE_ID_B}`).textContent).toContain('内置')
    expect(screen.getByTestId(`role-${ROLE_ID_B}`).textContent).toContain('已归档')
  })

  it('非管理员不发请求，只提示无权限', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    expect(calls).toHaveLength(0)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('新建角色解析并去重权限点后提交', async () => {
    const calls = stubBackend({
      'GET /api/v1/roles': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/roles': [ok(role())],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建角色' })).toBeTruthy())
    await fireEvent.update(byId('role-create-name'), 'operator')
    await fireEvent.update(byId('role-create-permissions'), 'device.read, device.reboot device.read')
    await fireEvent.click(screen.getByRole('button', { name: '新建角色' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'POST')).toBe(true))
    expect(calls.find((call) => call.method === 'POST')?.url).toBe('/api/v1/roles')
    expect(calls.find((call) => call.method === 'POST')?.body).toEqual({
      name: 'operator',
      permissions: ['device.read', 'device.reboot'],
    })
  })

  it('权限点为空时前端拦截，不发请求', async () => {
    const calls = stubBackend({ 'GET /api/v1/roles': [ok({ items: [], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建角色' })).toBeTruthy())
    await fireEvent.update(byId('role-create-name'), 'operator')
    await fireEvent.click(screen.getByRole('button', { name: '新建角色' }))
    await nextTick()
    expect(calls.some((call) => call.method === 'POST')).toBe(false)
    expect(screen.getByText('请检查输入内容')).toBeTruthy()
  })

  it('非内置角色以 CAS 版本归档', async () => {
    const calls = stubBackend({
      'GET /api/v1/roles': [ok({ items: [role()], next_cursor: null })],
      [`PATCH /api/v1/roles/${ROLE_ID}`]: [ok(role({ archived: true, revision: '2' }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '归档角色' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '归档角色' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true))
    expect(calls.find((call) => call.method === 'PATCH')?.body).toEqual({ expected_revision: '1', archived: true })
  })

  it('内置角色的归档按钮保持禁用且不发请求', async () => {
    const calls = stubBackend({
      'GET /api/v1/roles': [ok({ items: [role({ builtin: true })], next_cursor: null })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '归档角色' })).toBeTruthy())
    const archiveButton = screen.getByRole('button', { name: '归档角色' }) as HTMLButtonElement
    expect(archiveButton.disabled).toBe(true)
    await fireEvent.click(archiveButton)
    await nextTick()
    expect(calls.some((call) => call.method === 'PATCH')).toBe(false)
  })

  it('非法权限目录条目由服务端 400 呈现为参数约束错误', async () => {
    stubBackend({
      'GET /api/v1/roles': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/roles': [fail(400, 'INVALID_ARGUMENT')],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建角色' })).toBeTruthy())
    await fireEvent.update(byId('role-create-name'), 'operator')
    await fireEvent.update(byId('role-create-permissions'), 'device.unknown-permission')
    await fireEvent.click(screen.getByRole('button', { name: '新建角色' }))
    await vi.waitFor(() => expect(screen.getByText('请求参数不符合约束')).toBeTruthy())
  })

  it('改名面板以 CAS 版本提交新名称', async () => {
    const calls = stubBackend({
      'GET /api/v1/roles': [ok({ items: [role()], next_cursor: null })],
      [`PATCH /api/v1/roles/${ROLE_ID}`]: [ok(role({ name: 'operator-2', revision: '2' }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '改名' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '改名' }))
    await vi.waitFor(() => expect(screen.getByTestId('role-edit-panel')).toBeTruthy())
    await fireEvent.update(byId('role-edit-name'), 'operator-2')
    await fireEvent.click(screen.getByRole('button', { name: '确认执行' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true))
    expect(calls.find((call) => call.method === 'PATCH')?.body).toEqual({
      expected_revision: '1',
      name: 'operator-2',
      permissions: ['device.read', 'device.reboot'],
    })
  })

  it('未授权时头部刷新按钮禁用且点击零请求（二次权限门禁）', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    const reloadButton = screen.getByTestId('roles-reload') as HTMLButtonElement
    await fireEvent.click(reloadButton)
    await fireEvent.click(reloadButton)
    await nextTick()
    // 未授权时任何触发路径都必须零请求
    expect(calls).toHaveLength(0)
    expect(reloadButton.disabled).toBe(true)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('编辑草稿冻结打开时的 revision：列表刷新后提交仍用旧版本并显式冲突', async () => {
    const calls = stubBackend({
      'GET /api/v1/roles': [
        ok({ items: [role()], next_cursor: null }),
        ok({ items: [role({ revision: '2', permissions: ['device.read'] })], next_cursor: null }),
      ],
      [`PATCH /api/v1/roles/${ROLE_ID}`]: [fail(409, 'REVISION_CONFLICT')],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '改名' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '改名' }))
    await vi.waitFor(() => expect(screen.getByTestId('role-edit-panel')).toBeTruthy())
    // 后台刷新把服务端版本推进到 '2'，且权限集合已变：草稿不得被隐式升级或改写
    await fireEvent.click(screen.getByRole('button', { name: '刷新' }))
    await vi.waitFor(() => expect(screen.getByTestId('role-draft-stale')).toBeTruthy())
    await fireEvent.update(byId('role-edit-name'), 'operator-2')
    await fireEvent.click(screen.getByTestId('role-draft-submit'))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true))
    const patchCall = calls.find((call) => call.method === 'PATCH')
    expect(patchCall?.body).toEqual({
      expected_revision: '1',
      name: 'operator-2',
      permissions: ['device.read', 'device.reboot'],
    })
    await vi.waitFor(() => expect(screen.getByText(/版本冲突/)).toBeTruthy())
    // 冲突后草稿保持打开，由管理员显式重新编辑，绝不隐式覆盖已撤回的权限
    expect(screen.getByTestId('role-edit-panel')).toBeTruthy()
  })
})

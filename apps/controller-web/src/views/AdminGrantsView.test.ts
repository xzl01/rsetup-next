import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminGrantsView from './AdminGrantsView.vue'
import { createI18n } from '../i18n'
import { DEVICE_A, GRANT_ID, GROUP_ID, ROLE_ID, USER_ID, nextTick, ok, stubBackend } from './admin-test-helpers'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

function grant(overrides: Record<string, unknown> = {}) {
  return {
    id: GRANT_ID,
    user_id: USER_ID,
    source: { kind: 'role', role_id: ROLE_ID },
    scope: { kind: 'all' },
    revision: '1',
    ...overrides,
  }
}

function renderView(isAdmin = true) {
  return render(AdminGrantsView, {
    props: { csrfToken: 'csrf-token-1', isAdmin },
    global: { provide: { i18n } },
  })
}

function byId(id: string): HTMLInputElement {
  return document.getElementById(id) as HTMLInputElement
}

afterEach(() => vi.unstubAllGlobals())

describe('AdminGrantsView', () => {
  it('渲染授权关系并标注来源与范围', async () => {
    const calls = stubBackend({ 'GET /api/v1/grants': [ok({ items: [grant()], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`grant-${GRANT_ID}`)).toBeTruthy())
    expect(calls[0]?.url).toBe('/api/v1/grants')
    const row = screen.getByTestId(`grant-${GRANT_ID}`)
    expect(row.textContent).toContain('角色授权')
    expect(row.textContent).toContain('全部设备')
  })

  it('非管理员不发请求，只提示无权限', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    expect(calls).toHaveLength(0)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('按用户筛选后带 user_id 重新从第一页加载', async () => {
    const calls = stubBackend({ 'GET /api/v1/grants': [ok({ items: [], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '应用筛选' })).toBeTruthy())
    await fireEvent.update(byId('grant-user-filter'), USER_ID)
    await fireEvent.click(screen.getByRole('button', { name: '应用筛选' }))
    await vi.waitFor(() => expect(calls.some((call) => call.url === `/api/v1/grants?user_id=${USER_ID}`)).toBe(true))
  })

  it('创建角色授权（scope=all）提交严格互斥的 source/scope', async () => {
    const calls = stubBackend({
      'GET /api/v1/grants': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/grants': [ok(grant())],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建授权' })).toBeTruthy())
    await fireEvent.update(byId('grant-create-user'), USER_ID)
    await fireEvent.update(byId('grant-role-id'), ROLE_ID)
    await fireEvent.click(screen.getByRole('button', { name: '新建授权' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'POST')).toBe(true))
    expect(calls.find((call) => call.method === 'POST')?.body).toEqual({
      user_id: USER_ID,
      source: { kind: 'role', role_id: ROLE_ID },
      scope: { kind: 'all' },
    })
  })

  it('直接授权 + 单台设备按所选组合提交', async () => {
    const calls = stubBackend({
      'GET /api/v1/grants': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/grants': [ok(grant({ source: { kind: 'direct', permissions: ['device.read'] }, scope: { kind: 'device', device_id: DEVICE_A } }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建授权' })).toBeTruthy())
    await fireEvent.update(byId('grant-create-user'), USER_ID)
    await fireEvent.click(byId('grant-source-direct'))
    await vi.waitFor(() => expect(byId('grant-direct-permissions')).toBeTruthy())
    await fireEvent.update(byId('grant-direct-permissions'), 'device.read')
    await fireEvent.click(byId('grant-scope-device'))
    await vi.waitFor(() => expect(byId('grant-device-id')).toBeTruthy())
    await fireEvent.update(byId('grant-device-id'), DEVICE_A)
    await fireEvent.click(screen.getByRole('button', { name: '新建授权' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'POST')).toBe(true))
    expect(calls.find((call) => call.method === 'POST')?.body).toEqual({
      user_id: USER_ID,
      source: { kind: 'direct', permissions: ['device.read'] },
      scope: { kind: 'device', device_id: DEVICE_A },
    })
  })

  it('来源与范围字段不合法时前端拦截并提示，不发请求', async () => {
    const calls = stubBackend({ 'GET /api/v1/grants': [ok({ items: [], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建授权' })).toBeTruthy())
    // 未填 role_id
    await fireEvent.update(byId('grant-create-user'), USER_ID)
    await fireEvent.click(screen.getByRole('button', { name: '新建授权' }))
    await nextTick()
    expect(screen.getByTestId('grant-invalid-combination')).toBeTruthy()
    expect(calls.some((call) => call.method === 'POST')).toBe(false)

    // direct 缺权限点
    await fireEvent.click(byId('grant-source-direct'))
    await fireEvent.click(screen.getByRole('button', { name: '新建授权' }))
    await nextTick()
    expect(calls.some((call) => call.method === 'POST')).toBe(false)

    // 分组 ID 非法 UUID
    await fireEvent.click(byId('grant-source-role'))
    await fireEvent.update(byId('grant-role-id'), ROLE_ID)
    await fireEvent.click(byId('grant-scope-group'))
    await vi.waitFor(() => expect(byId('grant-group-id')).toBeTruthy())
    await fireEvent.update(byId('grant-group-id'), 'not-a-uuid')
    await fireEvent.click(screen.getByRole('button', { name: '新建授权' }))
    await nextTick()
    expect(calls.some((call) => call.method === 'POST')).toBe(false)
  })

  it('撤销授权发送 DELETE 与 CAS 版本', async () => {
    const calls = stubBackend({
      'GET /api/v1/grants': [ok({ items: [grant()], next_cursor: null })],
      [`DELETE /api/v1/grants/${GRANT_ID}`]: [ok({ deleted: true })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '撤销授权' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '撤销授权' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'DELETE')).toBe(true))
    const deleteCall = calls.find((call) => call.method === 'DELETE')
    expect(deleteCall?.url).toBe(`/api/v1/grants/${GRANT_ID}`)
    expect(deleteCall?.body).toEqual({ expected_revision: '1' })
  })

  it('分组范围授权提交 group_id', async () => {
    const calls = stubBackend({
      'GET /api/v1/grants': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/grants': [ok(grant({ scope: { kind: 'group', group_id: GROUP_ID } }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建授权' })).toBeTruthy())
    await fireEvent.update(byId('grant-create-user'), USER_ID)
    await fireEvent.update(byId('grant-role-id'), ROLE_ID)
    await fireEvent.click(byId('grant-scope-group'))
    await vi.waitFor(() => expect(byId('grant-group-id')).toBeTruthy())
    await fireEvent.update(byId('grant-group-id'), GROUP_ID)
    await fireEvent.click(screen.getByRole('button', { name: '新建授权' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'POST')).toBe(true))
    expect(calls.find((call) => call.method === 'POST')?.body).toEqual({
      user_id: USER_ID,
      source: { kind: 'role', role_id: ROLE_ID },
      scope: { kind: 'group', group_id: GROUP_ID },
    })
  })
})

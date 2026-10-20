import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, describe, expect, it, vi } from 'vitest'
import AdminGroupsView from './AdminGroupsView.vue'
import { createI18n } from '../i18n'
import { DEVICE_A, DEVICE_B, GROUP_ID, fail, nextTick, ok, stubBackend } from './admin-test-helpers'

const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })
const GROUP_ID_B = '01234567-89ab-4def-8f01-234567890ac0'

function group(overrides: Record<string, unknown> = {}) {
  return { id: GROUP_ID, name: 'rack-a', archived: false, revision: '1', member_count: '2', ...overrides }
}

function renderView(isAdmin = true) {
  return render(AdminGroupsView, {
    props: { csrfToken: 'csrf-token-1', isAdmin },
    global: { provide: { i18n } },
  })
}

function byId(id: string): HTMLInputElement | HTMLTextAreaElement {
  return document.getElementById(id) as HTMLInputElement | HTMLTextAreaElement
}

afterEach(() => vi.unstubAllGlobals())

describe('AdminGroupsView', () => {
  it('管理员进入时拉取分组并显示名称与成员数量', async () => {
    const calls = stubBackend({ 'GET /api/v1/groups': [ok({ items: [group()], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByTestId(`group-${GROUP_ID}`)).toBeTruthy())
    expect(calls[0]?.url).toBe('/api/v1/groups')
    expect(screen.getByTestId(`group-${GROUP_ID}`).textContent).toContain('rack-a')
    expect(screen.getByTestId(`group-${GROUP_ID}`).textContent).toContain('成员数量')
    expect(screen.getByTestId(`group-${GROUP_ID}`).textContent).toContain('2')
  })

  it('非管理员不发请求，只提示无权限', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    expect(calls).toHaveLength(0)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('新建分组提交 name 并在成功后刷新列表', async () => {
    const calls = stubBackend({
      'GET /api/v1/groups': [ok({ items: [], next_cursor: null })],
      'POST /api/v1/groups': [ok(group({ name: 'rack-b' }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建分组' })).toBeTruthy())
    await fireEvent.update(byId('group-create-name'), 'rack-b')
    await fireEvent.click(screen.getByRole('button', { name: '新建分组' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'POST')).toBe(true))
    const createCall = calls.find((call) => call.method === 'POST')
    expect(createCall?.url).toBe('/api/v1/groups')
    expect(createCall?.body).toEqual({ name: 'rack-b' })
    // 成功后重新拉取列表（首次 + 创建后各一次）
    await vi.waitFor(() => expect(calls.filter((call) => call.method === 'GET')).toHaveLength(2))
  })

  it('空名称被前端拦截，不发请求并给出字段错误', async () => {
    const calls = stubBackend({ 'GET /api/v1/groups': [ok({ items: [], next_cursor: null })] })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '新建分组' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '新建分组' }))
    await nextTick()
    expect(calls.some((call) => call.method === 'POST')).toBe(false)
    expect(screen.getByText('请检查输入内容')).toBeTruthy()
  })

  it('归档分组以 CAS 版本提交 archived:true', async () => {
    const calls = stubBackend({
      'GET /api/v1/groups': [ok({ items: [group()], next_cursor: null })],
      [`PATCH /api/v1/groups/${GROUP_ID}`]: [ok(group({ archived: true, revision: '2' }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '归档分组' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '归档分组' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true))
    const patchCall = calls.find((call) => call.method === 'PATCH')
    expect(patchCall?.url).toBe(`/api/v1/groups/${GROUP_ID}`)
    expect(patchCall?.body).toEqual({ expected_revision: '1', archived: true })
  })

  it('成员保存以去重后的显式集合整体替换，非法标识被忽略并提示', async () => {
    const calls = stubBackend({
      'GET /api/v1/groups': [ok({ items: [group()], next_cursor: null })],
      [`PUT /api/v1/groups/${GROUP_ID}/members`]: [ok(group({ member_count: '2' }))],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '成员管理' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '成员管理' }))
    await vi.waitFor(() => expect(screen.getByTestId('group-members-panel')).toBeTruthy())
    // 提示替换语义，避免管理员误以为是追加
    expect(screen.getByTestId('group-members-panel').textContent).toContain('整体替换成员')

    await fireEvent.update(byId('group-members-input'), `${DEVICE_A}\n${DEVICE_A}\n${DEVICE_B}\n非法标识`)
    expect(screen.getByTestId('members-invalid-notice')).toBeTruthy()
    await fireEvent.click(screen.getByRole('button', { name: '保存成员' }))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PUT')).toBe(true))
    const putCall = calls.find((call) => call.method === 'PUT')
    expect(putCall?.url).toBe(`/api/v1/groups/${GROUP_ID}/members`)
    expect(putCall?.body).toEqual({ expected_revision: '1', device_ids: [DEVICE_A, DEVICE_B] })
  })

  it('可见设备参考走 GET /devices，展示可粘贴的设备标识', async () => {
    const calls = stubBackend({
      'GET /api/v1/groups': [ok({ items: [group()], next_cursor: null })],
      'GET /api/v1/devices': [ok({
        items: [{ device_id: DEVICE_A, hostname: 'node-a', platform: 'linux', connection_state: 'online' }],
        next_cursor: null,
      })],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '可见设备参考' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '可见设备参考' }))
    await vi.waitFor(() => expect(calls.some((call) => call.url === '/api/v1/devices')).toBe(true))
    await vi.waitFor(() => expect(screen.getByTestId('reference-devices').textContent).toContain(DEVICE_A))
  })

  it('版本冲突以服务端错误码呈现可重试提示', async () => {
    stubBackend({
      'GET /api/v1/groups': [ok({ items: [group()], next_cursor: null })],
      [`PATCH /api/v1/groups/${GROUP_ID}`]: [fail(409, 'REVISION_CONFLICT')],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '归档分组' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '归档分组' }))
    await vi.waitFor(() => expect(screen.getByText(/版本冲突/)).toBeTruthy())
    expect(screen.getByRole('button', { name: '重试' })).toBeTruthy()
  })

  it('游标分页仅在存在 next_cursor 时提供加载更多', async () => {
    const calls = stubBackend({
      'GET /api/v1/groups': [
        ok({ items: [group()], next_cursor: 'cursor-2' }),
        ok({ items: [group({ id: GROUP_ID_B, name: 'rack-b' })], next_cursor: null }),
      ],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '加载更多' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '加载更多' }))
    await vi.waitFor(() => expect(calls.filter((call) => call.method === 'GET')).toHaveLength(2))
    expect(calls[1]?.url).toBe('/api/v1/groups?cursor=cursor-2')
    await vi.waitFor(() => expect(screen.queryByRole('button', { name: '加载更多' })).toBeNull())
  })

  it('未授权时头部刷新按钮禁用且点击零请求（二次权限门禁）', async () => {
    const calls = stubBackend({})
    renderView(false)
    await nextTick()
    const reloadButton = screen.getByTestId('groups-reload') as HTMLButtonElement
    await fireEvent.click(reloadButton)
    await fireEvent.click(reloadButton)
    await nextTick()
    // 未授权时任何触发路径都必须零请求
    expect(calls).toHaveLength(0)
    expect(reloadButton.disabled).toBe(true)
    expect(screen.getByText('仅管理员可访问管理页面')).toBeTruthy()
  })

  it('改名草稿冻结打开时的 revision：列表刷新后提交仍用旧版本并显式冲突', async () => {
    const calls = stubBackend({
      'GET /api/v1/groups': [
        ok({ items: [group()], next_cursor: null }),
        ok({ items: [group({ revision: '2', member_count: '5' })], next_cursor: null }),
      ],
      [`PATCH /api/v1/groups/${GROUP_ID}`]: [fail(409, 'REVISION_CONFLICT')],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '改名' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '改名' }))
    await vi.waitFor(() => expect(screen.getByTestId('group-rename-panel')).toBeTruthy())
    // 后台刷新把服务端版本推进到 '2'：草稿必须保持打开时冻结的 '1'
    await fireEvent.click(screen.getByRole('button', { name: '刷新' }))
    await vi.waitFor(() => expect(screen.getByTestId('group-rename-draft-stale')).toBeTruthy())
    await fireEvent.update(byId('group-rename-name'), 'rack-a-2')
    await fireEvent.click(screen.getByTestId('group-rename-submit'))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PATCH')).toBe(true))
    const patchCall = calls.find((call) => call.method === 'PATCH')
    expect(patchCall?.body).toEqual({ expected_revision: '1', name: 'rack-a-2' })
    await vi.waitFor(() => expect(screen.getByText(/版本冲突/)).toBeTruthy())
    // 冲突后草稿保持打开，由管理员显式重新编辑，绝不隐式覆盖
    expect(screen.getByTestId('group-rename-panel')).toBeTruthy()
  })

  it('成员草稿同样冻结 revision：列表刷新后 PUT 仍携带打开时的旧版本', async () => {
    const calls = stubBackend({
      'GET /api/v1/groups': [
        ok({ items: [group()], next_cursor: null }),
        ok({ items: [group({ revision: '7' })], next_cursor: null }),
      ],
      [`PUT /api/v1/groups/${GROUP_ID}/members`]: [fail(409, 'REVISION_CONFLICT')],
    })
    renderView()
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '成员管理' })).toBeTruthy())
    await fireEvent.click(screen.getByRole('button', { name: '成员管理' }))
    await vi.waitFor(() => expect(screen.getByTestId('group-members-panel')).toBeTruthy())
    await fireEvent.update(byId('group-members-input'), DEVICE_A)
    await fireEvent.click(screen.getByRole('button', { name: '刷新' }))
    await vi.waitFor(() => expect(screen.getByTestId('group-members-draft-stale')).toBeTruthy())
    await fireEvent.click(screen.getByTestId('group-members-submit'))
    await vi.waitFor(() => expect(calls.some((call) => call.method === 'PUT')).toBe(true))
    const putCall = calls.find((call) => call.method === 'PUT')
    expect(putCall?.url).toBe(`/api/v1/groups/${GROUP_ID}/members`)
    expect(putCall?.body).toEqual({ expected_revision: '1', device_ids: [DEVICE_A] })
    await vi.waitFor(() => expect(screen.getByText(/版本冲突/)).toBeTruthy())
  })
})

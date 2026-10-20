import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError } from './api'
import {
  ADMIN_API_PATHS,
  ADMIN_ERROR_GENERIC_KEY,
  MAX_APPROVAL_BATCH_TARGETS,
  adminErrorCode,
  adminErrorKey,
  adminListPath,
  approveApprovals,
  assertAuditItemShape,
  assertSystemStatusShape,
  createGroup,
  createUser,
  createGrant,
  deleteGrant,
  getSystemStatus,
  isAbortedError,
  isValidAdminName,
  isValidAdminReason,
  listApprovals,
  listAudit,
  listGrants,
  listUsers,
  normalizeDeviceIdSet,
  patchUser,
  rejectApprovals,
  replaceGroupMembers,
  resetUserPassword,
  validateGrantInput,
} from './admin-api'

const USER_ID = '01234567-89ab-4def-8f01-234567890abc'
const ROLE_ID = '01234567-89ab-4def-8f01-234567890abd'
const GROUP_ID = '01234567-89ab-4def-8f01-234567890abe'
const GRANT_ID = '01234567-89ab-4def-8f01-234567890abf'
const DEVICE_A = 'a'.repeat(64)
const DEVICE_B = 'b'.repeat(64)

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  })
}

function envelope(data: unknown): Response {
  return jsonResponse({ data, request_id: 'req-1' })
}

function stubFetch(...responses: Response[]) {
  const calls: Array<{ url: string; init?: RequestInit }> = []
  let index = 0
  const fetcher = vi.fn(async (url: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    calls.push({ url: String(url), init })
    const response = responses[Math.min(index, responses.length - 1)]
    index += 1
    return response
  })
  vi.stubGlobal('fetch', fetcher)
  return { calls, fetcher }
}

const ctx = { csrfToken: 'csrf-token-1' }

const APPROVAL_ITEM = {
  device_id: DEVICE_A,
  fingerprint: DEVICE_A,
  descriptor: null,
  decision: 'none',
  connection_state: 'online',
  revision: '3',
}

const USER_ITEM = {
  id: USER_ID,
  username: 'ops',
  display_name: 'Ops',
  active: true,
  is_admin: false,
  must_change_password: false,
  revision: '2',
}

const GROUP_ITEM = { id: GROUP_ID, name: 'rack-a', archived: false, revision: '1', member_count: '2' }

const AUDIT_ITEM = {
  id: USER_ID,
  actor_kind: 'user',
  actor_user_id: USER_ID,
  event_type: 'identity.user.create',
  target_kind: 'user',
  target_id: USER_ID,
  params_redacted: '{}',
  outcome: 'success',
  process_epoch: ROLE_ID,
  event_seq: '17',
  time_evidence: null,
}

afterEach(() => vi.unstubAllGlobals())

describe('ADMIN_API_PATHS 覆盖中控 26 条管理路由', () => {
  it('逐条列出中控 26 条管理路由（方法 + 路径）', () => {
    const routes: ReadonlyArray<readonly [string, string]> = [
      ['GET', ADMIN_API_PATHS.approvals],
      ['POST', ADMIN_API_PATHS.approve],
      ['POST', ADMIN_API_PATHS.reject],
      ['POST', ADMIN_API_PATHS.reopen(DEVICE_A)],
      ['POST', ADMIN_API_PATHS.revokeDevice(DEVICE_A)],
      ['POST', ADMIN_API_PATHS.reauthorizeDevice(DEVICE_A)],
      ['GET', ADMIN_API_PATHS.devices],
      ['GET', ADMIN_API_PATHS.device(DEVICE_A)],
      ['PATCH', ADMIN_API_PATHS.device(DEVICE_A)],
      ['GET', ADMIN_API_PATHS.groups],
      ['POST', ADMIN_API_PATHS.groups],
      ['PATCH', ADMIN_API_PATHS.group(GROUP_ID)],
      ['PUT', ADMIN_API_PATHS.groupMembers(GROUP_ID)],
      ['GET', ADMIN_API_PATHS.users],
      ['POST', ADMIN_API_PATHS.users],
      ['PATCH', ADMIN_API_PATHS.user(USER_ID)],
      ['POST', ADMIN_API_PATHS.userResetPassword(USER_ID)],
      ['GET', ADMIN_API_PATHS.userEffectivePermissions(USER_ID)],
      ['GET', ADMIN_API_PATHS.roles],
      ['POST', ADMIN_API_PATHS.roles],
      ['PATCH', ADMIN_API_PATHS.role(ROLE_ID)],
      ['GET', ADMIN_API_PATHS.grants],
      ['POST', ADMIN_API_PATHS.grants],
      ['DELETE', ADMIN_API_PATHS.grant(GRANT_ID)],
      ['GET', ADMIN_API_PATHS.audit],
      ['GET', ADMIN_API_PATHS.systemStatus],
    ]
    expect(routes).toHaveLength(26)
    const serialized = routes.map(([method, path]) => `${method} ${path}`)
    expect(new Set(serialized).size).toBe(26)
    expect(serialized.every((entry) => !entry.includes(' /'))).toBe(true)
  })

  it('adminListPath 省略空值并按 query 编码', () => {
    expect(adminListPath('users')).toBe('users')
    expect(adminListPath('users', { cursor: null, limit: undefined, decision: '' })).toBe('users')
    expect(adminListPath('audit', { actor_id: USER_ID, cursor: 'abc' })).toBe(`audit?actor_id=${USER_ID}&cursor=abc`)
  })

  it('adminListPath 拒绝控制字符游标而不是编码送出', () => {
    expect(() => adminListPath('audit', { cursor: 'bad\nvalue' })).toThrowError()
  })
})

describe('准入审批请求封装', () => {
  it('GET /approvals 带 decision 筛选并通过形状校验', async () => {
    const { calls } = stubFetch(envelope({ items: [APPROVAL_ITEM], next_cursor: 'tok' }))
    const res = await listApprovals({ decision: 'none' }, ctx)
    expect(calls[0]?.url).toBe('/api/v1/approvals?decision=none')
    expect(res.data.items[0]?.revision).toBe('3')
  })

  it('拒绝 decision 非法或非十进制 revision 的审批页', async () => {
    stubFetch(
      envelope({ items: [{ ...APPROVAL_ITEM, decision: 'PENDING' }], next_cursor: null }),
      envelope({ items: [{ ...APPROVAL_ITEM, revision: '01' }], next_cursor: null }),
    )
    await expect(listApprovals({}, ctx)).rejects.toBeInstanceOf(ApiError)
    await expect(listApprovals({}, ctx)).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE' })
  })

  it('POST /approvals/approve 固定 confirm:true 并携带 CSRF 头', async () => {
    const { calls } = stubFetch(envelope({ results: [{ device_id: DEVICE_A, ok: true, revision: '4' }] }))
    await approveApprovals([{ device_id: DEVICE_A, expected_revision: '3' }], ctx)
    expect(calls[0]?.url).toBe('/api/v1/approvals/approve')
    expect(calls[0]?.init?.method).toBe('POST')
    expect(calls[0]?.init?.headers).toMatchObject({ 'X-CSRF-Token': 'csrf-token-1' })
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
      targets: [{ device_id: DEVICE_A, expected_revision: '3' }],
      confirm: true,
    })
  })

  it('批量目标数量越界或设备标识非法时不发请求', async () => {
    const { fetcher } = stubFetch(envelope({ results: [] }))
    await expect(approveApprovals([], ctx)).rejects.toBeInstanceOf(ApiError)
    const tooMany = Array.from({ length: MAX_APPROVAL_BATCH_TARGETS + 1 }, (_, index) => ({
      device_id: index.toString(16).padStart(64, '0'),
      expected_revision: '1',
    }))
    await expect(approveApprovals(tooMany, ctx)).rejects.toBeInstanceOf(ApiError)
    await expect(approveApprovals([{ device_id: 'Z'.repeat(64), expected_revision: '1' }], ctx)).rejects.toBeInstanceOf(ApiError)
    expect(fetcher).not.toHaveBeenCalled()
    expect(MAX_APPROVAL_BATCH_TARGETS).toBe(1024)
  })

  it('POST /approvals/reject 需要非空原因并随 body 一并提交', async () => {
    const { calls, fetcher } = stubFetch(envelope({ results: [] }))
    await expect(rejectApprovals([{ device_id: DEVICE_A, expected_revision: '1' }], '   ', ctx))
      .rejects.toBeInstanceOf(ApiError)
    expect(fetcher).not.toHaveBeenCalled()
    await rejectApprovals([{ device_id: DEVICE_A, expected_revision: '1' }], '设备指纹不符', ctx)
    expect(JSON.parse(String(calls[0]?.init?.body)).reason).toBe('设备指纹不符')
  })
})

describe('用户管理请求封装', () => {
  it('GET /users 解析分页并校验 UUIDv4 与十进制 revision', async () => {
    const { calls } = stubFetch(envelope({ items: [USER_ITEM], next_cursor: null }))
    const res = await listUsers(null, ctx)
    expect(calls[0]?.url).toBe('/api/v1/users')
    expect(res.data.items[0]?.id).toBe(USER_ID)
  })

  it('POST /users 三字段全必填并原样返回一次性临时密码', async () => {
    const { calls } = stubFetch(envelope({ user: USER_ITEM, temporary_password: 'tmp-secret' }))
    const res = await createUser('ops', 'Ops', false, ctx)
    expect(calls[0]?.url).toBe('/api/v1/users')
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({ username: 'ops', display_name: 'Ops', is_admin: false })
    expect(res.data.temporary_password).toBe('tmp-secret')
  })

  it('缺少 temporary_password 的创建响应判为形状不符', async () => {
    stubFetch(envelope({ user: USER_ITEM }))
    await expect(createUser('ops', 'Ops', false, ctx)).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE' })
  })

  it('PATCH /users/{id} 只发送给定补丁字段并携带 CAS 版本', async () => {
    const { calls } = stubFetch(envelope({ ...USER_ITEM, active: false }))
    await patchUser(USER_ID, { expectedRevision: '2', active: false }, ctx)
    expect(calls[0]?.url).toBe(`/api/v1/users/${USER_ID}`)
    expect(calls[0]?.init?.method).toBe('PATCH')
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({ expected_revision: '2', active: false })
  })

  it('POST /users/{id}/reset-password 仅提交 expected_revision', async () => {
    const { calls } = stubFetch(envelope({ user: USER_ITEM, temporary_password: 'tmp-2' }))
    await resetUserPassword(USER_ID, '2', ctx)
    expect(calls[0]?.url).toBe(`/api/v1/users/${USER_ID}/reset-password`)
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({ expected_revision: '2' })
  })

  it('非规范 UUID 或空补丁直接拒绝', async () => {
    const { fetcher } = stubFetch(envelope(USER_ITEM))
    await expect(patchUser('not-a-uuid', { expectedRevision: '1', active: true }, ctx)).rejects.toBeInstanceOf(ApiError)
    await expect(patchUser(USER_ID, { expectedRevision: '1' }, ctx)).rejects.toBeInstanceOf(ApiError)
    expect(fetcher).not.toHaveBeenCalled()
  })
})

describe('组、授权与审计请求封装', () => {
  it('PUT /groups/{id}/members 去重替换并按 revision CAS', async () => {
    const { calls } = stubFetch(envelope(GROUP_ITEM))
    await replaceGroupMembers(GROUP_ID, '1', [DEVICE_A.toUpperCase(), DEVICE_A, DEVICE_B], ctx)
    expect(calls[0]?.url).toBe(`/api/v1/groups/${GROUP_ID}/members`)
    expect(calls[0]?.init?.method).toBe('PUT')
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
      expected_revision: '1',
      device_ids: [DEVICE_A, DEVICE_B],
    })
  })

  it('normalizeDeviceIdSet 丢弃非法标识并保持首次出现顺序', () => {
    expect(normalizeDeviceIdSet([DEVICE_B, 'nope', DEVICE_B.toUpperCase(), DEVICE_A, '  '])).toEqual([DEVICE_B, DEVICE_A])
  })

  it('POST /groups 校验名称长度', async () => {
    const { calls } = stubFetch(envelope(GROUP_ITEM))
    await createGroup('rack-a', ctx)
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({ name: 'rack-a' })
    const { fetcher } = stubFetch(envelope(GROUP_ITEM))
    await expect(createGroup('', ctx)).rejects.toBeInstanceOf(ApiError)
    await expect(createGroup('x'.repeat(129), ctx)).rejects.toBeInstanceOf(ApiError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('DELETE /grants/{id} 仍发送 JSON body 与 CSRF 头', async () => {
    const { calls } = stubFetch(envelope({ deleted: true }))
    await deleteGrant(GRANT_ID, '5', ctx)
    expect(calls[0]?.url).toBe(`/api/v1/grants/${GRANT_ID}`)
    expect(calls[0]?.init?.method).toBe('DELETE')
    expect(calls[0]?.init?.headers).toMatchObject({ 'X-CSRF-Token': 'csrf-token-1' })
    expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({ expected_revision: '5' })
  })

  it('删除响应 deleted 非 true 判为形状不符', async () => {
    stubFetch(envelope({ deleted: false }))
    await expect(deleteGrant(GRANT_ID, '5', ctx)).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE' })
  })

  it('GET /grants 带 user_id 筛选', async () => {
    const { calls } = stubFetch(envelope({ items: [], next_cursor: null }))
    await listGrants({ userId: USER_ID }, ctx)
    expect(calls[0]?.url).toBe(`/api/v1/grants?user_id=${USER_ID}`)
  })

  it('source/scope 互斥校验', () => {
    expect(validateGrantInput({ userId: USER_ID, source: { kind: 'role', role_id: ROLE_ID }, scope: { kind: 'all' } })).toBe(true)
    expect(validateGrantInput({
      userId: USER_ID,
      source: { kind: 'direct', permissions: ['device.read'] },
      scope: { kind: 'device', device_id: DEVICE_A },
    })).toBe(true)
    // role 与 permissions 同时出现
    expect(validateGrantInput({
      userId: USER_ID,
      source: { kind: 'role', role_id: ROLE_ID, permissions: ['device.read'] },
      scope: { kind: 'all' },
    })).toBe(false)
    // direct 空权限
    expect(validateGrantInput({ userId: USER_ID, source: { kind: 'direct', permissions: [] }, scope: { kind: 'all' } })).toBe(false)
    // group 与 device 字段同时出现
    expect(validateGrantInput({
      userId: USER_ID,
      source: { kind: 'role', role_id: ROLE_ID },
      scope: { kind: 'group', group_id: GROUP_ID, device_id: DEVICE_A },
    })).toBe(false)
    // all 却带 group_id
    expect(validateGrantInput({
      userId: USER_ID,
      source: { kind: 'role', role_id: ROLE_ID },
      scope: { kind: 'all', group_id: GROUP_ID },
    })).toBe(false)
  })

  it('POST /grants 只在通过互斥校验后才发请求', async () => {
    const { fetcher } = stubFetch(envelope({ id: GRANT_ID, user_id: USER_ID, source: { kind: 'all' }, scope: { kind: 'all' }, revision: '1' }))
    await expect(createGrant({ userId: USER_ID, source: { kind: 'role' }, scope: { kind: 'all' } }, ctx))
      .rejects.toBeInstanceOf(ApiError)
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('GET /audit 组装筛选并保留复合游标字符串', async () => {
    const { calls } = stubFetch(envelope({ items: [AUDIT_ITEM], next_cursor: 'signed-cursor' }))
    const res = await listAudit({ actorId: USER_ID, deviceId: DEVICE_A, cursor: 'next-token' }, ctx)
    expect(calls[0]?.url).toBe(`/api/v1/audit?actor_id=${USER_ID}&device_id=${DEVICE_A}&cursor=next-token`)
    expect(res.data.next_cursor).toBe('signed-cursor')
    expect(res.data.items[0]?.event_seq).toBe('17')
  })

  it('审计 event_seq 为 JSON number 时判为形状不符（防精度丢失）', () => {
    expect(() => assertAuditItemShape({ ...AUDIT_ITEM, event_seq: 17 })).toThrowError()
    expect(() => assertAuditItemShape(AUDIT_ITEM)).not.toThrow()
  })

  it('非规范 actor_id / device_id 筛选不发请求', async () => {
    const { fetcher } = stubFetch(envelope({ items: [], next_cursor: null }))
    await expect(listAudit({ actorId: 'not-a-uuid' }, ctx)).rejects.toBeInstanceOf(ApiError)
    await expect(listAudit({ deviceId: 'ZZ' }, ctx)).rejects.toBeInstanceOf(ApiError)
    expect(fetcher).not.toHaveBeenCalled()
  })
})

describe('系统状态与输入门禁', () => {
  it('GET /system/status 校验嵌套 counts 全为十进制字符串', async () => {
    const { calls } = stubFetch(envelope({
      ready: true,
      schema_version: 7,
      version: '0.1.0',
      recovery_mode: 'normal',
      counts: { users: '1', devices: '0', roles: '2', grants: '3', groups: '4' },
    }))
    const res = await getSystemStatus(ctx)
    expect(calls[0]?.url).toBe('/api/v1/system/status')
    expect(res.data.counts.users).toBe('1')
  })

  it('counts 为 JSON number 时判为形状不符', async () => {
    stubFetch(envelope({
      ready: true, schema_version: 7, version: '0.1.0', recovery_mode: 'normal',
      counts: { users: 1, devices: '0', roles: '2', grants: '3', groups: '4' },
    }))
    await expect(getSystemStatus(ctx)).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE' })
    expect(() => assertSystemStatusShape({
      ready: true, schema_version: 1, version: 'v', recovery_mode: 'normal',
      counts: { users: '1', devices: '0', roles: '2', grants: '3', groups: '4' },
    })).not.toThrow()
  })

  it('isValidAdminReason 与后端原因门禁同口径', () => {
    expect(isValidAdminReason('设备指纹不符')).toBe(true)
    expect(isValidAdminReason('   ')).toBe(false)
    expect(isValidAdminReason(`line\nbreak`)).toBe(false)
    expect(isValidAdminReason('x'.repeat(1025))).toBe(false)
  })

  it('isValidAdminName 限制 1–128 字符且不超过 512 字节', () => {
    expect(isValidAdminName('rack-a')).toBe(true)
    expect(isValidAdminName('')).toBe(false)
    expect(isValidAdminName('x'.repeat(129))).toBe(false)
    expect(isValidAdminName('中'.repeat(128))).toBe(true)
    expect(isValidAdminName('中'.repeat(129))).toBe(false)
  })

  it('错误码映射只认后端去敏码，未知码回落通用文案', () => {
    const conflict = new ApiError({ code: 'REVISION_CONFLICT', messageKey: 'x' })
    expect(adminErrorKey(conflict)).toBe('admin.error.revisionConflict')
    expect(adminErrorCode(conflict)).toBe('REVISION_CONFLICT')
    expect(adminErrorKey(new ApiError({ code: 'LAST_ADMIN', messageKey: 'x' }))).toBe('admin.error.lastAdmin')
    expect(adminErrorKey(new ApiError({ code: 'WEIRD_CODE', messageKey: 'x' }))).toBe(ADMIN_ERROR_GENERIC_KEY)
    expect(adminErrorKey(new Error('boom'))).toBe(ADMIN_ERROR_GENERIC_KEY)
  })

  it('isAbortedError 覆盖 ApiError 取消与 DOM AbortError', () => {
    expect(isAbortedError(new ApiError({ code: 'REQUEST_ABORTED', messageKey: 'x' }))).toBe(true)
    expect(isAbortedError(Object.assign(new Error('c'), { name: 'AbortError' }))).toBe(true)
    expect(isAbortedError(new Error('other'))).toBe(false)
  })
})

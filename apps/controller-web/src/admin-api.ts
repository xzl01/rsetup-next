// 中控管理端 26 条管理路由的受限客户端封装（spec 02 §4/§5/§7）。
//
// 设计约束：
// - 只经由 `api.ts` 的 `get/post/put/patch/del` 出口，写请求强制携带 `X-CSRF-Token`，
//   请求头白名单不可被调用方扩展；绝不读取或写入 Cookie，绝不落 localStorage。
// - 所有响应在离开网络层前做运行时形状校验：id/revision 等身份与 CAS 字段是字符串
//   （revision 为规范十进制无符号 64 位字符串），形状不符一律 INVALID_API_RESPONSE，
//   不做默认值补齐，也不把 `null` 当 `0` 或空串呈现。
// - 错误只透出去敏业务码（`ApiError.code`）与稳定 i18n key，绝不回显服务端自由文本。
import { del, get, patch, post, put, ApiError } from './api'
import type { ApiResponse } from './types'
import { isSafeOpaqueCursor, isU64DecimalString } from './types'

/** 管理端请求上下文：CSRF Token 只存在于内存，由 App 层从认证store传入。 */
export interface AdminRequestContext {
  csrfToken: string
  signal?: AbortSignal
}

/** 02 §1 Page 投影：`{items, next_cursor}`，next_cursor 为不透明签名游标或 null。 */
export interface AdminPage<T> {
  items: T[]
  next_cursor: string | null
}

export const ADMIN_REVIEW_DECISIONS = ['none', 'approved', 'denied', 'revoked'] as const
export type AdminReviewDecision = (typeof ADMIN_REVIEW_DECISIONS)[number]

export interface ApprovalItem {
  device_id: string
  fingerprint: string
  descriptor: Record<string, unknown> | null
  decision: AdminReviewDecision
  connection_state: 'online' | 'offline'
  revision: string
}

export interface ApprovalBatchResultItem {
  device_id: string
  ok: boolean
  revision?: string
  error?: { code?: string }
}

export interface ApprovalBatchResults {
  results: ApprovalBatchResultItem[]
}

/** 单目标状态机响应：reopen 返回 decision，revoke/reauthorize 返回 state。 */
export interface AdmissionDecisionResult {
  decision?: AdminReviewDecision
  state?: 'PENDING' | 'APPROVED' | 'REVOKED'
  requires_device_reset: boolean
}

export interface ApprovalTargetInput {
  device_id: string
  expected_revision: string
}

export interface AdminGroupItem {
  id: string
  name: string
  archived: boolean
  revision: string
  member_count: string
}

export interface AdminUserItem {
  id: string
  username: string
  display_name: string
  active: boolean
  is_admin: boolean
  must_change_password: boolean
  revision: string
}

/** 创建用户与重置密码响应：临时密码只在此一次性响应中出现，绝不落盘。 */
export interface AdminUserWithTemporaryPassword {
  user: AdminUserItem
  temporary_password: string
}

export interface AdminRoleItem {
  id: string
  name: string
  builtin: boolean
  archived: boolean
  revision: string
  permissions: string[]
}

export interface AdminGrantSource {
  kind: 'role' | 'direct'
  role_id?: string | null
  permissions?: string[] | null
}

export interface AdminGrantScope {
  kind: 'all' | 'group' | 'device'
  group_id?: string | null
  device_id?: string | null
}

export interface AdminGrantItem {
  id: string
  user_id: string
  source: AdminGrantSource
  scope: AdminGrantScope
  revision: string
}

export interface AdminAuditItem {
  id: string
  actor_kind: string
  actor_user_id: string | null
  event_type: string
  target_kind: string | null
  target_id: string | null
  params_redacted: string
  outcome: string
  process_epoch: string
  event_seq: string
  time_evidence: unknown
}

export interface AdminSystemStatus {
  ready: boolean
  schema_version: number
  version: string
  recovery_mode: string
  counts: {
    users: string
    devices: string
    roles: string
    grants: string
    groups: string
  }
}

export interface AdminEffectivePermissionItem {
  device_id: string
  effective_permissions: string[]
  sources: Array<{ permission: string; source: { kind: string; role_id?: string; grant_id?: string } }>
}

/**
 * 管理端 26 条路由的固定清单（spec 02 §4/§5/§7 + Task 12 生产装配矩阵）：
 * approvals 6、devices 3、groups 4、users 5、roles 3、grants 3、audit 1、system 1。
 * 新增端点必须先改规格与本表，禁止在视图内临时拼路径。
 */
export const ADMIN_API_PATHS = {
  approvals: 'approvals',
  approve: 'approvals/approve',
  reject: 'approvals/reject',
  reopen: (deviceId: string) => `approvals/${deviceId}/reopen`,
  revokeDevice: (deviceId: string) => `devices/${deviceId}/revoke`,
  reauthorizeDevice: (deviceId: string) => `devices/${deviceId}/reauthorize`,
  devices: 'devices',
  device: (deviceId: string) => `devices/${deviceId}`,
  groups: 'groups',
  group: (groupId: string) => `groups/${groupId}`,
  groupMembers: (groupId: string) => `groups/${groupId}/members`,
  users: 'users',
  user: (userId: string) => `users/${userId}`,
  userResetPassword: (userId: string) => `users/${userId}/reset-password`,
  userEffectivePermissions: (userId: string) => `users/${userId}/effective-permissions`,
  roles: 'roles',
  role: (roleId: string) => `roles/${roleId}`,
  grants: 'grants',
  grant: (grantId: string) => `grants/${grantId}`,
  audit: 'audit',
  systemStatus: 'system/status',
} as const

// ---------------------------------------------------------------------------
// 运行时形状校验
// ---------------------------------------------------------------------------

const DEVICE_ID_HEX64 = /^[0-9a-f]{64}(?![\s\S])/
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}(?![\s\S])/

const invalidShape = () =>
  new ApiError({ code: 'INVALID_API_RESPONSE', messageKey: 'errors.invalidResponse' })

function asRecord(value: unknown): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw invalidShape()
  return value as Record<string, unknown>
}

function requireString(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0
}

function requireDecimal(value: unknown): value is string {
  return typeof value === 'string' && isU64DecimalString(value)
}

function requireStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === 'string')
}

/** Page 形状：items 必须是数组，next_cursor 只能是不透明游标字符串或 null。 */
export function assertAdminPageShape(data: unknown): asserts data is AdminPage<unknown> {
  const page = asRecord(data)
  if (!Array.isArray(page.items)) throw invalidShape()
  if (page.next_cursor !== null && !isSafeOpaqueCursor(page.next_cursor)) throw invalidShape()
}

export function assertApprovalItemShape(data: unknown): asserts data is ApprovalItem {
  const item = asRecord(data)
  if (!requireString(item.device_id) || !DEVICE_ID_HEX64.test(item.device_id)) throw invalidShape()
  if (!requireString(item.fingerprint)) throw invalidShape()
  if (item.descriptor !== null && typeof item.descriptor !== 'object') throw invalidShape()
  if (typeof item.decision !== 'string' ||
      !(ADMIN_REVIEW_DECISIONS as readonly string[]).includes(item.decision)) throw invalidShape()
  if (item.connection_state !== 'online' && item.connection_state !== 'offline') throw invalidShape()
  if (!requireDecimal(item.revision)) throw invalidShape()
}

export function assertGroupItemShape(data: unknown): asserts data is AdminGroupItem {
  const item = asRecord(data)
  if (!requireString(item.id) || !UUID_V4.test(item.id)) throw invalidShape()
  if (typeof item.name !== 'string') throw invalidShape()
  if (typeof item.archived !== 'boolean') throw invalidShape()
  if (!requireDecimal(item.revision)) throw invalidShape()
  if (!requireDecimal(item.member_count)) throw invalidShape()
}

export function assertUserItemShape(data: unknown): asserts data is AdminUserItem {
  const item = asRecord(data)
  if (!requireString(item.id) || !UUID_V4.test(item.id)) throw invalidShape()
  if (!requireString(item.username)) throw invalidShape()
  if (typeof item.display_name !== 'string') throw invalidShape()
  for (const key of ['active', 'is_admin', 'must_change_password'] as const) {
    if (typeof item[key] !== 'boolean') throw invalidShape()
  }
  if (!requireDecimal(item.revision)) throw invalidShape()
}

export function assertRoleItemShape(data: unknown): asserts data is AdminRoleItem {
  const item = asRecord(data)
  if (!requireString(item.id) || !UUID_V4.test(item.id)) throw invalidShape()
  if (!requireString(item.name)) throw invalidShape()
  if (typeof item.builtin !== 'boolean' || typeof item.archived !== 'boolean') throw invalidShape()
  if (!requireDecimal(item.revision)) throw invalidShape()
  if (!requireStringArray(item.permissions)) throw invalidShape()
}

export function assertGrantItemShape(data: unknown): asserts data is AdminGrantItem {
  const item = asRecord(data)
  if (!requireString(item.id) || !UUID_V4.test(item.id)) throw invalidShape()
  if (!requireString(item.user_id) || !UUID_V4.test(item.user_id)) throw invalidShape()
  if (!requireDecimal(item.revision)) throw invalidShape()
  const source = asRecord(item.source)
  if (source.kind !== 'role' && source.kind !== 'direct') throw invalidShape()
  const scope = asRecord(item.scope)
  if (scope.kind !== 'all' && scope.kind !== 'group' && scope.kind !== 'device') throw invalidShape()
}

export function assertAuditItemShape(data: unknown): asserts data is AdminAuditItem {
  const item = asRecord(data)
  if (!requireString(item.id) || !UUID_V4.test(item.id)) throw invalidShape()
  if (!requireString(item.actor_kind)) throw invalidShape()
  if (item.actor_user_id !== null && !requireString(item.actor_user_id)) throw invalidShape()
  if (!requireString(item.event_type)) throw invalidShape()
  if (item.target_kind !== null && !requireString(item.target_kind)) throw invalidShape()
  if (item.target_id !== null && !requireString(item.target_id)) throw invalidShape()
  if (typeof item.params_redacted !== 'string') throw invalidShape()
  if (!requireString(item.outcome)) throw invalidShape()
  if (!requireString(item.process_epoch) || !UUID_V4.test(item.process_epoch)) throw invalidShape()
  // event_seq 是 64 位计数器：只接受规范十进制字符串，绝不接受 JSON number。
  if (!requireDecimal(item.event_seq)) throw invalidShape()
}

export function assertSystemStatusShape(data: unknown): asserts data is AdminSystemStatus {
  const status = asRecord(data)
  if (typeof status.ready !== 'boolean') throw invalidShape()
  if (typeof status.schema_version !== 'number' || !Number.isInteger(status.schema_version)) throw invalidShape()
  if (!requireString(status.version)) throw invalidShape()
  if (!requireString(status.recovery_mode)) throw invalidShape()
  const counts = asRecord(status.counts)
  for (const key of ['users', 'devices', 'roles', 'grants', 'groups'] as const) {
    if (!requireDecimal(counts[key])) throw invalidShape()
  }
}

export function assertEffectivePermissionPageShape(data: unknown): asserts data is AdminPage<AdminEffectivePermissionItem> {
  assertAdminPageShape(data)
  const page = data as AdminPage<unknown>
  for (const item of page.items) {
    const record = asRecord(item)
    if (!requireString(record.device_id) || !DEVICE_ID_HEX64.test(record.device_id)) throw invalidShape()
    if (!requireStringArray(record.effective_permissions)) throw invalidShape()
    if (!Array.isArray(record.sources)) throw invalidShape()
  }
}

export function assertApprovalBatchResultsShape(data: unknown): asserts data is ApprovalBatchResults {
  const body = asRecord(data)
  if (!Array.isArray(body.results)) throw invalidShape()
  for (const raw of body.results) {
    const item = asRecord(raw)
    if (!requireString(item.device_id) || !DEVICE_ID_HEX64.test(item.device_id)) throw invalidShape()
    if (typeof item.ok !== 'boolean') throw invalidShape()
  }
}

export function assertTemporaryPasswordShape(data: unknown): asserts data is AdminUserWithTemporaryPassword {
  const body = asRecord(data)
  assertUserItemShape(body.user)
  // 临时密码必须是非空字符串；缺失说明响应形状不符，绝不显示空密码。
  if (!requireString(body.temporary_password)) throw invalidShape()
}

export function assertDecisionResultShape(data: unknown): asserts data is AdmissionDecisionResult {
  const body = asRecord(data)
  if (typeof body.requires_device_reset !== 'boolean') throw invalidShape()
  const hasDecision = requireString(body.decision) &&
    (ADMIN_REVIEW_DECISIONS as readonly string[]).includes(body.decision)
  const hasState = body.state === 'PENDING' || body.state === 'APPROVED' || body.state === 'REVOKED'
  if (!hasDecision && !hasState) throw invalidShape()
}

// ---------------------------------------------------------------------------
// query 组装
// ---------------------------------------------------------------------------

/**
 * 组装集合级 query：只接受白名单内的键值，值必须是安全不透明字符串（拒绝控制字符与
 * 超长值）。空值一律省略，避免把 `?cursor=` 这类畸形参数送到后端触发 400。
 */
export function adminListPath(base: string, params: Record<string, string | null | undefined> = {}): string {
  const parts: string[] = []
  for (const [key, value] of Object.entries(params)) {
    if (value === null || value === undefined || value === '') continue
    if (!isSafeOpaqueCursor(key) || !isSafeOpaqueCursor(value)) throw invalidShape()
    parts.push(`${encodeURIComponent(key)}=${encodeURIComponent(value)}`)
  }
  return parts.length === 0 ? base : `${base}?${parts.join('&')}`
}

/** 批量目标上限（spec 02 §3：单请求 1..=1024）。 */
export const MAX_APPROVAL_BATCH_TARGETS = 1024

/** 拒绝/吊销原因门禁：trim 非空、≤1024 字节、无控制字符（与后端 approvals 门禁同口径）。 */
export function isValidAdminReason(reason: string): boolean {
  const trimmed = reason.trim()
  if (trimmed.length === 0) return false
  if (/[\x00-\x1f\x7f-\x9f]/.test(reason)) return false
  return new TextEncoder().encode(reason).length <= 1024
}

/** 规范 hex64 设备公钥。 */
export function isDeviceIdHex64(value: string): boolean {
  return DEVICE_ID_HEX64.test(value)
}

/** 规范小写 UUIDv4。 */
export function isCanonicalUuidV4(value: string): boolean {
  return UUID_V4.test(value)
}

/** 名称门禁（02 §1 Name：UTF-8 1–128 字符，最多 512 字节）。 */
export function isValidAdminName(name: string): boolean {
  const chars = Array.from(name).length
  if (chars < 1 || chars > 128) return false
  return new TextEncoder().encode(name).length <= 512
}

// ---------------------------------------------------------------------------
// 26 条管理路由
// ---------------------------------------------------------------------------

function validateTargets(targets: readonly ApprovalTargetInput[]): void {
  if (targets.length < 1 || targets.length > MAX_APPROVAL_BATCH_TARGETS) throw invalidShape()
  for (const target of targets) {
    if (!isDeviceIdHex64(target.device_id) || !requireDecimal(target.expected_revision)) throw invalidShape()
  }
}

export async function listApprovals(params: { cursor?: string | null; decision?: AdminReviewDecision | null },
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<ApprovalItem>>> {
  const res = await get<AdminPage<ApprovalItem>>(adminListPath(ADMIN_API_PATHS.approvals, {
    cursor: params.cursor ?? null,
    decision: params.decision ?? null,
  }), { signal: context.signal })
  assertAdminPageShape(res.data)
  for (const item of res.data.items) assertApprovalItemShape(item)
  return res as ApiResponse<AdminPage<ApprovalItem>>
}

export async function approveApprovals(targets: readonly ApprovalTargetInput[],
  context: AdminRequestContext): Promise<ApiResponse<ApprovalBatchResults>> {
  validateTargets(targets)
  const res = await post<ApprovalBatchResults>(ADMIN_API_PATHS.approve,
    { targets: targets.map((t) => ({ device_id: t.device_id, expected_revision: t.expected_revision })), confirm: true },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertApprovalBatchResultsShape(res.data)
  return res as ApiResponse<ApprovalBatchResults>
}

export async function rejectApprovals(targets: readonly ApprovalTargetInput[], reason: string,
  context: AdminRequestContext): Promise<ApiResponse<ApprovalBatchResults>> {
  validateTargets(targets)
  if (!isValidAdminReason(reason)) throw invalidShape()
  const res = await post<ApprovalBatchResults>(ADMIN_API_PATHS.reject,
    {
      targets: targets.map((t) => ({ device_id: t.device_id, expected_revision: t.expected_revision })),
      reason,
      confirm: true,
    },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertApprovalBatchResultsShape(res.data)
  return res as ApiResponse<ApprovalBatchResults>
}

export async function reopenApproval(deviceId: string, expectedRevision: string,
  context: AdminRequestContext): Promise<ApiResponse<AdmissionDecisionResult>> {
  if (!isDeviceIdHex64(deviceId) || !requireDecimal(expectedRevision)) throw invalidShape()
  const res = await post<AdmissionDecisionResult>(ADMIN_API_PATHS.reopen(deviceId),
    { expected_revision: expectedRevision, confirm: true },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertDecisionResultShape(res.data)
  return res as ApiResponse<AdmissionDecisionResult>
}

export async function revokeDevice(deviceId: string, expectedRevision: string, reason: string,
  context: AdminRequestContext): Promise<ApiResponse<AdmissionDecisionResult>> {
  if (!isDeviceIdHex64(deviceId) || !requireDecimal(expectedRevision) || !isValidAdminReason(reason)) {
    throw invalidShape()
  }
  const res = await post<AdmissionDecisionResult>(ADMIN_API_PATHS.revokeDevice(deviceId),
    { expected_revision: expectedRevision, reason, confirm: true },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertDecisionResultShape(res.data)
  return res as ApiResponse<AdmissionDecisionResult>
}

export async function reauthorizeDevice(deviceId: string, expectedRevision: string,
  context: AdminRequestContext): Promise<ApiResponse<AdmissionDecisionResult>> {
  if (!isDeviceIdHex64(deviceId) || !requireDecimal(expectedRevision)) throw invalidShape()
  const res = await post<AdmissionDecisionResult>(ADMIN_API_PATHS.reauthorizeDevice(deviceId),
    { expected_revision: expectedRevision, confirm: true },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertDecisionResultShape(res.data)
  return res as ApiResponse<AdmissionDecisionResult>
}

export async function getDeviceProfile(deviceId: string,
  context: AdminRequestContext): Promise<ApiResponse<Record<string, unknown>>> {
  if (!isDeviceIdHex64(deviceId)) throw invalidShape()
  return await get<Record<string, unknown>>(ADMIN_API_PATHS.device(deviceId), { signal: context.signal })
}

export async function patchDeviceProfile(deviceId: string, expectedRevision: string, displayName: string,
  context: AdminRequestContext): Promise<ApiResponse<Record<string, unknown>>> {
  if (!isDeviceIdHex64(deviceId) || !requireDecimal(expectedRevision) || !isValidAdminName(displayName)) {
    throw invalidShape()
  }
  return await patch<Record<string, unknown>>(ADMIN_API_PATHS.device(deviceId),
    { expected_revision: expectedRevision, display_name: displayName },
    { csrfToken: context.csrfToken, signal: context.signal })
}

/** 管理员可见设备列表（02 §5：只返回授权投影，用于分组成员选择）。 */
export async function listDevicesForAdmin(cursor: string | null,
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<{ device_id: string; display_name: string }>>> {
  const res = await get<AdminPage<{ device_id: string; display_name: string }>>(
    adminListPath(ADMIN_API_PATHS.devices, { cursor: cursor ?? null }), { signal: context.signal })
  assertAdminPageShape(res.data)
  return res as ApiResponse<AdminPage<{ device_id: string; display_name: string }>>
}

export async function listGroups(cursor: string | null,
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<AdminGroupItem>>> {
  const res = await get<AdminPage<AdminGroupItem>>(
    adminListPath(ADMIN_API_PATHS.groups, { cursor: cursor ?? null }), { signal: context.signal })
  assertAdminPageShape(res.data)
  for (const item of res.data.items) assertGroupItemShape(item)
  return res as ApiResponse<AdminPage<AdminGroupItem>>
}

export async function createGroup(name: string,
  context: AdminRequestContext): Promise<ApiResponse<AdminGroupItem>> {
  if (!isValidAdminName(name)) throw invalidShape()
  const res = await post<AdminGroupItem>(ADMIN_API_PATHS.groups, { name },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertGroupItemShape(res.data)
  return res as ApiResponse<AdminGroupItem>
}

export interface AdminPatchGroupInput {
  expectedRevision: string
  name?: string
  archived?: boolean
}

export async function patchGroup(groupId: string, input: AdminPatchGroupInput,
  context: AdminRequestContext): Promise<ApiResponse<AdminGroupItem>> {
  if (!isCanonicalUuidV4(groupId) || !requireDecimal(input.expectedRevision)) throw invalidShape()
  if (input.name === undefined && input.archived === undefined) throw invalidShape()
  if (input.name !== undefined && !isValidAdminName(input.name)) throw invalidShape()
  const body: Record<string, unknown> = { expected_revision: input.expectedRevision }
  if (input.name !== undefined) body.name = input.name
  if (input.archived !== undefined) body.archived = input.archived
  const res = await patch<AdminGroupItem>(ADMIN_API_PATHS.group(groupId), body,
    { csrfToken: context.csrfToken, signal: context.signal })
  assertGroupItemShape(res.data)
  return res as ApiResponse<AdminGroupItem>
}

/** 显式去重并保持首次出现顺序的成员集合替换（02 §5：替换显式去重集合）。 */
export function normalizeDeviceIdSet(deviceIds: readonly string[]): string[] {
  const seen = new Set<string>()
  const ordered: string[] = []
  for (const raw of deviceIds) {
    const id = raw.trim().toLowerCase()
    if (!isDeviceIdHex64(id) || seen.has(id)) continue
    seen.add(id)
    ordered.push(id)
  }
  return ordered
}

export async function replaceGroupMembers(groupId: string, expectedRevision: string, deviceIds: readonly string[],
  context: AdminRequestContext): Promise<ApiResponse<AdminGroupItem>> {
  if (!isCanonicalUuidV4(groupId) || !requireDecimal(expectedRevision)) throw invalidShape()
  const members = normalizeDeviceIdSet(deviceIds)
  const res = await put<AdminGroupItem>(ADMIN_API_PATHS.groupMembers(groupId),
    { expected_revision: expectedRevision, device_ids: members },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertGroupItemShape(res.data)
  return res as ApiResponse<AdminGroupItem>
}

export async function listUsers(cursor: string | null,
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<AdminUserItem>>> {
  const res = await get<AdminPage<AdminUserItem>>(
    adminListPath(ADMIN_API_PATHS.users, { cursor: cursor ?? null }), { signal: context.signal })
  assertAdminPageShape(res.data)
  for (const item of res.data.items) assertUserItemShape(item)
  return res as ApiResponse<AdminPage<AdminUserItem>>
}

export async function createUser(username: string, displayName: string, isAdmin: boolean,
  context: AdminRequestContext): Promise<ApiResponse<AdminUserWithTemporaryPassword>> {
  if (!isValidAdminName(username) || !isValidAdminName(displayName)) throw invalidShape()
  const res = await post<AdminUserWithTemporaryPassword>(ADMIN_API_PATHS.users,
    { username, display_name: displayName, is_admin: isAdmin },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertTemporaryPasswordShape(res.data)
  return res as ApiResponse<AdminUserWithTemporaryPassword>
}

export interface AdminPatchUserInput {
  expectedRevision: string
  active?: boolean
  is_admin?: boolean
  display_name?: string
}

export async function patchUser(userId: string, input: AdminPatchUserInput,
  context: AdminRequestContext): Promise<ApiResponse<AdminUserItem>> {
  if (!isCanonicalUuidV4(userId) || !requireDecimal(input.expectedRevision)) throw invalidShape()
  if (input.active === undefined && input.is_admin === undefined && input.display_name === undefined) {
    throw invalidShape()
  }
  if (input.display_name !== undefined && !isValidAdminName(input.display_name)) throw invalidShape()
  const body: Record<string, unknown> = { expected_revision: input.expectedRevision }
  if (input.active !== undefined) body.active = input.active
  if (input.is_admin !== undefined) body.is_admin = input.is_admin
  if (input.display_name !== undefined) body.display_name = input.display_name
  const res = await patch<AdminUserItem>(ADMIN_API_PATHS.user(userId), body,
    { csrfToken: context.csrfToken, signal: context.signal })
  assertUserItemShape(res.data)
  return res as ApiResponse<AdminUserItem>
}

export async function resetUserPassword(userId: string, expectedRevision: string,
  context: AdminRequestContext): Promise<ApiResponse<AdminUserWithTemporaryPassword>> {
  if (!isCanonicalUuidV4(userId) || !requireDecimal(expectedRevision)) throw invalidShape()
  const res = await post<AdminUserWithTemporaryPassword>(ADMIN_API_PATHS.userResetPassword(userId),
    { expected_revision: expectedRevision },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertTemporaryPasswordShape(res.data)
  return res as ApiResponse<AdminUserWithTemporaryPassword>
}

export async function listEffectivePermissions(userId: string,
  params: { deviceId?: string | null; cursor?: string | null },
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<AdminEffectivePermissionItem>>> {
  if (!isCanonicalUuidV4(userId)) throw invalidShape()
  const res = await get<AdminPage<AdminEffectivePermissionItem>>(
    adminListPath(ADMIN_API_PATHS.userEffectivePermissions(userId), {
      device_id: params.deviceId ?? null,
      cursor: params.cursor ?? null,
    }), { signal: context.signal })
  assertEffectivePermissionPageShape(res.data)
  return res as ApiResponse<AdminPage<AdminEffectivePermissionItem>>
}

export async function listRoles(cursor: string | null,
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<AdminRoleItem>>> {
  const res = await get<AdminPage<AdminRoleItem>>(
    adminListPath(ADMIN_API_PATHS.roles, { cursor: cursor ?? null }), { signal: context.signal })
  assertAdminPageShape(res.data)
  for (const item of res.data.items) assertRoleItemShape(item)
  return res as ApiResponse<AdminPage<AdminRoleItem>>
}

export async function createRole(name: string, permissions: readonly string[],
  context: AdminRequestContext): Promise<ApiResponse<AdminRoleItem>> {
  if (!isValidAdminName(name) || permissions.length === 0) throw invalidShape()
  const res = await post<AdminRoleItem>(ADMIN_API_PATHS.roles, { name, permissions: [...permissions] },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertRoleItemShape(res.data)
  return res as ApiResponse<AdminRoleItem>
}

export interface AdminPatchRoleInput {
  expectedRevision: string
  name?: string
  permissions?: string[]
  archived?: boolean
}

export async function patchRole(roleId: string, input: AdminPatchRoleInput,
  context: AdminRequestContext): Promise<ApiResponse<AdminRoleItem>> {
  if (!isCanonicalUuidV4(roleId) || !requireDecimal(input.expectedRevision)) throw invalidShape()
  if (input.name === undefined && input.permissions === undefined && input.archived === undefined) {
    throw invalidShape()
  }
  if (input.name !== undefined && !isValidAdminName(input.name)) throw invalidShape()
  if (input.permissions !== undefined && input.permissions.length === 0) throw invalidShape()
  const body: Record<string, unknown> = { expected_revision: input.expectedRevision }
  if (input.name !== undefined) body.name = input.name
  if (input.permissions !== undefined) body.permissions = [...input.permissions]
  if (input.archived !== undefined) body.archived = input.archived
  const res = await patch<AdminRoleItem>(ADMIN_API_PATHS.role(roleId), body,
    { csrfToken: context.csrfToken, signal: context.signal })
  assertRoleItemShape(res.data)
  return res as ApiResponse<AdminRoleItem>
}

export const GRANT_SOURCE_KINDS = ['role', 'direct'] as const
export const GRANT_SCOPE_KINDS = ['all', 'group', 'device'] as const

export interface AdminCreateGrantInput {
  userId: string
  source: AdminGrantSource
  scope: AdminGrantScope
}

/**
 * source/scope 互斥校验（02 §4）：
 * role 必须带 role_id 且不带 permissions；direct 必须带非空 permissions 且不带 role_id；
 * all 不带任何字段；group 只带 group_id；device 只带 device_id。
 */
export function validateGrantInput(input: AdminCreateGrantInput): boolean {
  if (!isCanonicalUuidV4(input.userId)) return false
  const source = input.source
  if (source.kind === 'role') {
    if (!isCanonicalUuidV4(String(source.role_id ?? ''))) return false
    if (source.permissions !== undefined && source.permissions !== null) return false
  } else if (source.kind === 'direct') {
    if (source.role_id !== undefined && source.role_id !== null) return false
    const permissions = source.permissions
    if (!Array.isArray(permissions) || permissions.length === 0) return false
  } else {
    return false
  }
  const scope = input.scope
  if (scope.kind === 'all') {
    return (scope.group_id ?? null) === null && (scope.device_id ?? null) === null
  }
  if (scope.kind === 'group') {
    return isCanonicalUuidV4(String(scope.group_id ?? '')) && (scope.device_id ?? null) === null
  }
  if (scope.kind === 'device') {
    return isDeviceIdHex64(String(scope.device_id ?? '')) && (scope.group_id ?? null) === null
  }
  return false
}

export async function listGrants(params: { userId?: string | null; cursor?: string | null },
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<AdminGrantItem>>> {
  const res = await get<AdminPage<AdminGrantItem>>(
    adminListPath(ADMIN_API_PATHS.grants, { user_id: params.userId ?? null, cursor: params.cursor ?? null }),
    { signal: context.signal })
  assertAdminPageShape(res.data)
  for (const item of res.data.items) assertGrantItemShape(item)
  return res as ApiResponse<AdminPage<AdminGrantItem>>
}

export async function createGrant(input: AdminCreateGrantInput,
  context: AdminRequestContext): Promise<ApiResponse<AdminGrantItem>> {
  if (!validateGrantInput(input)) throw invalidShape()
  const res = await post<AdminGrantItem>(ADMIN_API_PATHS.grants,
    { user_id: input.userId, source: input.source, scope: input.scope },
    { csrfToken: context.csrfToken, signal: context.signal })
  assertGrantItemShape(res.data)
  return res as ApiResponse<AdminGrantItem>
}

export async function deleteGrant(grantId: string, expectedRevision: string,
  context: AdminRequestContext): Promise<ApiResponse<{ deleted: boolean }>> {
  if (!isCanonicalUuidV4(grantId) || !requireDecimal(expectedRevision)) throw invalidShape()
  const res = await del<{ deleted: boolean }>(ADMIN_API_PATHS.grant(grantId),
    { expected_revision: expectedRevision },
    { csrfToken: context.csrfToken, signal: context.signal })
  if (asRecord(res.data).deleted !== true) throw invalidShape()
  return res as ApiResponse<{ deleted: boolean }>
}

export interface AdminAuditQuery {
  actorId?: string | null
  deviceId?: string | null
  cursor?: string | null
}

export async function listAudit(query: AdminAuditQuery,
  context: AdminRequestContext): Promise<ApiResponse<AdminPage<AdminAuditItem>>> {
  if (query.actorId && !isCanonicalUuidV4(query.actorId)) throw invalidShape()
  if (query.deviceId && !isDeviceIdHex64(query.deviceId)) throw invalidShape()
  const res = await get<AdminPage<AdminAuditItem>>(adminListPath(ADMIN_API_PATHS.audit, {
    actor_id: query.actorId ?? null,
    device_id: query.deviceId ?? null,
    cursor: query.cursor ?? null,
  }), { signal: context.signal })
  assertAdminPageShape(res.data)
  for (const item of res.data.items) assertAuditItemShape(item)
  return res as ApiResponse<AdminPage<AdminAuditItem>>
}

export async function getSystemStatus(
  context: AdminRequestContext): Promise<ApiResponse<AdminSystemStatus>> {
  const res = await get<AdminSystemStatus>(ADMIN_API_PATHS.systemStatus, { signal: context.signal })
  assertSystemStatusShape(res.data)
  return res as ApiResponse<AdminSystemStatus>
}

// ---------------------------------------------------------------------------
// 错误码 → 稳定 i18n key
// ---------------------------------------------------------------------------

const ADMIN_ERROR_KEYS: Record<string, string> = {
  INVALID_ARGUMENT: 'admin.error.invalidArgument',
  PERMISSION_DENIED: 'auth.error.denied',
  NOT_FOUND: 'admin.error.notFound',
  REVISION_CONFLICT: 'admin.error.revisionConflict',
  LAST_ADMIN: 'admin.error.lastAdmin',
  INVALID_STATE: 'admin.error.invalidState',
  RESOURCE_EXHAUSTED: 'admin.error.resourceExhausted',
  RATE_LIMITED: 'auth.error.rateLimited',
  AUTH_REQUIRED: 'auth.error.authRequired',
  NOT_READY: 'admin.system.notReady',
  STORAGE_UNAVAILABLE: 'admin.error.storageUnavailable',
}

export const ADMIN_ERROR_GENERIC_KEY = 'admin.error.generic'

/** 只认后端去敏业务码；未知码回落到通用文案，绝不透出服务端 message。 */
export function adminErrorKey(error: unknown): string {
  if (error instanceof ApiError) {
    const mapped = Object.prototype.hasOwnProperty.call(ADMIN_ERROR_KEYS, error.code)
      ? ADMIN_ERROR_KEYS[error.code]
      : undefined
    if (mapped !== undefined) return mapped
  }
  return ADMIN_ERROR_GENERIC_KEY
}

/** 供视图针对特定码给出更明确的保护性提示（如 LAST_ADMIN）。 */
export function adminErrorCode(error: unknown): string | null {
  return error instanceof ApiError ? error.code : null
}

/** 请求是否因 AbortController 取消而失败：取消不是错误，不得污染界面状态。 */
export function isAbortedError(error: unknown): boolean {
  return (error instanceof ApiError && error.code === 'REQUEST_ABORTED') ||
    (typeof error === 'object' && error !== null && 'name' in error && error.name === 'AbortError')
}

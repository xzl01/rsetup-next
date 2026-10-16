import { SUBTASK_STATES, SPEC_REASON_CODES, type SpecReasonCode, type SubTaskState } from './task-presentation'
import { isSafeOpaqueCursor, isU64DecimalString } from './types'

export type TaskOperation = 'reboot'
export type MainTaskState = 'queued' | 'running' | 'held' | 'completed'
export type MainTaskOutcome = 'success' | 'cancelled' | 'unknown' | 'partial' | 'failed'
export type ViewScope = 'full' | 'authorized_subset'
export type PreviewReason = SpecReasonCode // 04 ReasonCode 12 值 SCREAMING_SNAKE_CASE（来源=Runtime Task 6 DTO，账本决议③）
export type ResultEvidenceKind = 'attempted' | 'boot_transition_observed' | 'execution_rejected' | 'os_error' | 'journal_lost' | 'none'

export interface TaskPreviewTarget { device_id: string; eligible: boolean; reason?: PreviewReason | null }
export interface TaskPreview { preview_token: string; expires_in_ms: number; targets: TaskPreviewTarget[] }
export interface SubTaskItem {
  id: string
  device_id: string
  state: SubTaskState
  reason_code: SpecReasonCode | null          // key 必含（值 nullable）；缺 key/undefined 按既有守卫拒绝
  revision: string
  result_evidence: ResultEvidenceKind | null  // key 必含（值 nullable）；缺 key/undefined 按既有守卫拒绝
  lock_released?: boolean
}
export interface TaskPage<T> { items: T[]; next_cursor: string | null }
export interface AuthorizedSubsetNotice { kind: 'authorized_subset' | 'unknown'; key: string }

const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/
const TASK_ID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
const RFC3339_UTC = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.\d{1,9})?(?:Z|\+00:00)$/
const CANONICAL_UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/

const MAIN_TASK_STATES = new Set<string>([
  'queued', 'running', 'held', 'completed',
])
const MAIN_TASK_OUTCOMES = new Set<string>(['success', 'cancelled', 'unknown', 'partial', 'failed'])
const SUB_TASK_STATES = new Set<string>(SUBTASK_STATES)
const REASON_CODES = new Set<string>(SPEC_REASON_CODES)
// preview reason 消费词表 = 04 ReasonCode 12 值 SCREAMING_SNAKE_CASE（来源=Runtime Task 6 DTO，账本决议③）；
// null/缺省放行（eligible 目标合法，见 assertTarget 的 !== null 分支），非法字符串仍抛 INVALID_PREVIEW_REASON
const PREVIEW_REASONS = new Set<string>(SPEC_REASON_CODES)
// EvidenceKind 六值（Runtime 计划 Task 2）的 snake_case 字符串词表；'none' 是枚举串，勿与 JSON null 混淆
const RESULT_EVIDENCE_KINDS = new Set<string>([
  'attempted', 'boot_transition_observed', 'execution_rejected', 'os_error', 'journal_lost', 'none',
])
const OPERATIONS = new Set<string>(['reboot'])
const VIEW_SCOPES = new Set<string>(['full', 'authorized_subset'])

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function fail(code: string): never {
  throw new Error(code)
}

export function isCanonicalUuidV4(value: unknown): value is string {
  return typeof value === 'string' && TASK_ID_V4.test(value)
}

function assertTarget(value: unknown): void {
  if (!isRecord(value)) fail('INVALID_PREVIEW_TARGET')
  if (typeof value.device_id !== 'string' || !DEVICE_ID_HEX64.test(value.device_id)) fail('INVALID_DEVICE_ID')
  if (typeof value.eligible !== 'boolean') fail('INVALID_PREVIEW_ELIGIBLE')
  if (value.reason !== undefined && value.reason !== null && (typeof value.reason !== 'string' || !PREVIEW_REASONS.has(value.reason))) {
    fail('INVALID_PREVIEW_REASON')
  }
}

export interface TaskTimeEvidence {
  system_wall_utc: string
  reference_utc: string
  quality: string
  clock_epoch: string
  source: string | null
  sample_age_ms: string | null
  offset_ms: number | null
  uncertainty_ms: number | null
}

export const TASK_COUNT_KEYS = [
  'queued', 'held', 'dispatching', 'accepted', 'verifying',
  'succeeded', 'failed', 'unknown', 'cancelled', 'expired',
] as const

export interface TaskCounts {
  queued: string; held: string; dispatching: string; accepted: string; verifying: string
  succeeded: string; failed: string; unknown: string; cancelled: string; expired: string
}

export interface TaskItem {
  id: string
  operation: TaskOperation
  state: MainTaskState
  outcome: MainTaskOutcome | null
  revision: string
  created_time: TaskTimeEvidence
  counts: TaskCounts
  view_scope?: ViewScope
  actions?: { can_cancel: boolean }   // 后端计算的取消可请求标记；可选，形状校验见 assertTaskItemShape
}

export interface ReleaseLockResult {
  released: true
  state: 'unknown'
}

export function assertTaskPreviewShape(data: unknown): asserts data is TaskPreview {
  if (!isRecord(data)) fail('INVALID_TASK_PREVIEW')
  if (typeof data.preview_token !== 'string' || data.preview_token.length === 0 || data.preview_token.length > 512) {
    fail('INVALID_PREVIEW_TOKEN')
  }
  if (typeof data.expires_in_ms !== 'number' || !Number.isInteger(data.expires_in_ms) ||
      data.expires_in_ms <= 0 || data.expires_in_ms > 600000) {
    fail('INVALID_PREVIEW_EXPIRY')
  }
  if (!Array.isArray(data.targets) || data.targets.length === 0) fail('INVALID_PREVIEW_TARGETS')
  data.targets.forEach(assertTarget)
}

export function assertTaskItemShape(data: unknown): asserts data is TaskItem {
  if (!isRecord(data)) fail('INVALID_MAIN_TASK')
  if (!isCanonicalUuidV4(data.id)) fail('INVALID_TASK_ID')
  if (typeof data.operation !== 'string' || !OPERATIONS.has(data.operation)) fail('INVALID_TASK_OPERATION')
  if (typeof data.state !== 'string' || !MAIN_TASK_STATES.has(data.state)) fail('INVALID_MAIN_TASK_STATE')
  if (data.state === 'completed') {
    if (typeof data.outcome !== 'string' || !MAIN_TASK_OUTCOMES.has(data.outcome)) fail('INVALID_MAIN_TASK_OUTCOME')
  } else if (data.outcome !== null) {
    fail('INVALID_MAIN_TASK_OUTCOME')
  }
  if (!isU64DecimalString(data.revision)) fail('INVALID_REVISION')
  assertTaskTimeEvidence(data.created_time)
  assertTaskCounts(data.counts)
  if (data.view_scope !== undefined && (typeof data.view_scope !== 'string' || !VIEW_SCOPES.has(data.view_scope))) {
    fail('INVALID_VIEW_SCOPE')
  }
  if (data.actions !== undefined && (!isRecord(data.actions) || typeof data.actions.can_cancel !== 'boolean')) {
    fail('INVALID_TASK_ACTIONS')
  }
}

// 格里高利闰年规则（含世纪/400 年规则），与 pinned chrono 0.4.45 NaiveDate::from_ymd_opt 一致
function isLeapYear(year: number): boolean {
  return year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0)
}

function daysInMonth(year: number, month: number): number {
  switch (month) {
    case 2: return isLeapYear(year) ? 29 : 28
    case 4: case 6: case 9: case 11: return 30
    default: return 31
  }
}

function isUtcRfc3339(value: unknown): value is string {
  if (typeof value !== 'string') return false
  const match = RFC3339_UTC.exec(value)
  if (!match) return false
  // 日历校验用显式数值比较，不用 Date.parse/Date.UTC：
  // 1) V8 会把越界日归一化（2026-02-30 → 3 月 2 日），其它引擎返回 NaN，跨引擎不一致；
  // 2) Date.UTC 会把 00..99 年映射到 1900..1999，而此处四位年为 0000..9999，直接按格里高利规则校验。
  const year = Number(match[1])
  const month = Number(match[2])
  const day = Number(match[3])
  const hour = Number(match[4])
  const minute = Number(match[5])
  const second = Number(match[6])
  if (month < 1 || month > 12) return false
  if (day < 1 || day > daysInMonth(year, month)) return false
  if (hour > 23 || minute > 59) return false
  // 闰秒政策（依据 pinned chrono 0.4.45，非臆造）：parse_rfc3339 显式允许 :60（60 → 59 + 1s 纳秒，
  // src/format/parse.rs:230-233），write_rfc3339 对已存闰秒会再输出 :60（src/format/formatting.rs:528-531），
  // 故后端 wire 可携带 23:59:60；边界：仅接受规范 23:59:60 位置，chrono 宽松接受的 00:00:60 等其它
  // 位置被保守拒绝（标剩余待专家核，不扩为未批准设计）。
  return second < 60 || (second === 60 && hour === 23 && minute === 59)
}

function isCanonicalUuid(value: unknown): value is string {
  return typeof value === 'string' && CANONICAL_UUID.test(value)
}

export function zeroTaskCounts(): TaskCounts {
  return {
    queued: '0', held: '0', dispatching: '0', accepted: '0', verifying: '0',
    succeeded: '0', failed: '0', unknown: '0', cancelled: '0', expired: '0',
  }
}

export function baseTaskTimeEvidence(overrides: Partial<TaskTimeEvidence> = {}): TaskTimeEvidence {
  return {
    system_wall_utc: '2026-10-16T00:00:00Z',
    reference_utc: '2026-10-16T00:00:00Z',
    quality: 'system_fallback',
    clock_epoch: '3f6a6f00-4a10-4e2a-b0a2-4b0f00000001',
    source: null,
    sample_age_ms: null,
    offset_ms: null,
    uncertainty_ms: null,
    ...overrides,
  }
}

// evidence.rs：offset_ms 为 Option<i64>、uncertainty_ms 为 Option<u64>，wire 保持 JSON number（不擅改 decimal 字符串）。
// 整数双精度值转 BigInt(value) 无损，与 64 位边界 BigInt 比较即精确；不用 Number.isSafeInteger
// （它会拒绝 64 位 >2^53 的合法大值并冒称"完整兼容"）。
// 剩余（本计划不扩为新设计）：|v| > 2^53 时 JSON.parse 已舍入到最近可表示值，wire 上合法 64 位值
// （如 2^63-1）会解码为 2^63 而在此被拒；本展示证据不授予危险动作，任何精确安全决策不得基于
// 已舍入值；解决需 wire decimal 字符串或专用 codec，待批准，不自行新增依赖。
const I64_MIN = -9223372036854775808n
const I64_MAX = 9223372036854775807n
const U64_MAX = 18446744073709551615n

function isI64Number(value: unknown): value is number {
  if (typeof value !== 'number' || !Number.isInteger(value)) return false
  const big = BigInt(value)
  return big >= I64_MIN && big <= I64_MAX
}

function isU64Number(value: unknown): value is number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0) return false
  return BigInt(value) <= U64_MAX
}

export function assertTaskTimeEvidence(data: unknown): asserts data is TaskTimeEvidence {
  if (!isRecord(data)) fail('INVALID_TIME_EVIDENCE')
  if (!isUtcRfc3339(data.system_wall_utc)) fail('INVALID_TIME_EVIDENCE')
  if (!isUtcRfc3339(data.reference_utc)) fail('INVALID_TIME_EVIDENCE')
  if (!isCanonicalUuid(data.clock_epoch)) fail('INVALID_TIME_EVIDENCE')
  if (typeof data.quality !== 'string' || data.quality.length === 0) fail('INVALID_TIME_EVIDENCE')
  if (data.source !== null && typeof data.source !== 'string') fail('INVALID_TIME_EVIDENCE')
  if (data.sample_age_ms !== null && !isU64DecimalString(data.sample_age_ms)) fail('INVALID_TIME_EVIDENCE')
  if (data.offset_ms !== null && !isI64Number(data.offset_ms)) fail('INVALID_TIME_EVIDENCE')
  if (data.uncertainty_ms !== null && !isU64Number(data.uncertainty_ms)) fail('INVALID_TIME_EVIDENCE')
}

export function assertTaskCounts(data: unknown): asserts data is TaskCounts {
  if (!isRecord(data)) fail('INVALID_TASK_COUNTS')
  for (const key of TASK_COUNT_KEYS) {
    if (!isU64DecimalString(data[key])) fail('INVALID_TASK_COUNTS')
  }
}

export function assertReleaseLockResultShape(data: unknown): asserts data is ReleaseLockResult {
  if (!isRecord(data)) fail('INVALID_RELEASE_LOCK_RESULT')
  if (data.released !== true) fail('INVALID_RELEASE_LOCK_RELEASED')
  if (data.state !== 'unknown') fail('INVALID_RELEASE_LOCK_STATE')
}

export function assertSubTaskItemShape(data: unknown): asserts data is SubTaskItem {
  if (!isRecord(data)) fail('INVALID_SUB_TASK')
  // 02 §1:24 SubTask 投影 = 6 字段 id/device_id/state/reason_code/revision/result_evidence：
  // id 为规范 UUIDv4（不放宽为非空 ≤128）；main_task_id/pre_boot_id/observed_boot_id 等 DB 内部列
  // 不是 HTTP 必需字段；execution_token 及其摘要永不暴露（扩展字段忽略不拒绝）
  if (!isCanonicalUuidV4(data.id)) fail('INVALID_SUB_TASK_ID')
  if (typeof data.device_id !== 'string' || !DEVICE_ID_HEX64.test(data.device_id)) fail('INVALID_DEVICE_ID')
  if (typeof data.state !== 'string' || !SUB_TASK_STATES.has(data.state)) fail('INVALID_SUB_TASK_STATE')
  // reason_code：key 必含（值 nullable）：缺 key 或 undefined 按既有守卫拒绝，JSON null（无原因码）合法
  if (data.reason_code === undefined ||
      (data.reason_code !== null &&
       (typeof data.reason_code !== 'string' || !REASON_CODES.has(data.reason_code)))) {
    fail('INVALID_REASON_CODE')
  }
  if (!isU64DecimalString(data.revision)) fail('INVALID_REVISION')
  // result_evidence：key 必含（值 nullable）：缺 key 或 undefined 按既有守卫拒绝；JSON null = 证据未产生（Option None）
  // 'none' 是 EvidenceKind 六值枚举字符串之一（snake_case，勿与 null 混淆）；旧来源词表/PascalCase/对象/数/布尔一律拒绝
  //（nullable EvidenceKind 枚举串，不是完整 wire 证据对象）
  if (data.result_evidence === undefined ||
      (data.result_evidence !== null &&
       (typeof data.result_evidence !== 'string' || !RESULT_EVIDENCE_KINDS.has(data.result_evidence)))) {
    fail('INVALID_RESULT_EVIDENCE')
  }
  // lock_released：可选 fail-closed 兼容（出现必须是布尔）；后端投影不承诺输出该字段，
  // UI 不得据其缺失推断危险授权（unknown 释放锁按 state === 'unknown' 判定，04 §8）
  if (data.lock_released !== undefined && typeof data.lock_released !== 'boolean') fail('INVALID_LOCK_RELEASED')
}

function assertPage(data: unknown, assertItem: (item: unknown) => void): void {
  if (!isRecord(data) || !Array.isArray(data.items)) fail('INVALID_TASK_PAGE')
  if (data.next_cursor !== null && !isSafeOpaqueCursor(data.next_cursor)) fail('INVALID_NEXT_CURSOR')
  data.items.forEach(assertItem)
}

export function assertTaskPageShape(data: unknown): asserts data is TaskPage<TaskItem> {
  assertPage(data, assertTaskItemShape)
}

export function assertSubTaskPageShape(data: unknown): asserts data is TaskPage<SubTaskItem> {
  assertPage(data, assertSubTaskItemShape)
}

export function describeAuthorizedSubset(viewScope: unknown): AuthorizedSubsetNotice {
  if (viewScope === 'authorized_subset') {
    return { kind: 'authorized_subset', key: 'task.scope.authorizedSubset' }
  }
  return { kind: 'unknown', key: 'task.scope.unknown' }
}

export function canRequestCancel(task: Pick<TaskItem, 'actions' | 'view_scope'>): boolean {
  if (task.view_scope !== 'full' && task.view_scope !== 'authorized_subset') return false
  return task.actions?.can_cancel === true
}

export function eligibleDeviceIds(preview: TaskPreview): string[] {
  const seen = new Set<string>()
  const result: string[] = []
  for (const target of preview.targets) {
    if (target.eligible && !seen.has(target.device_id)) {
      seen.add(target.device_id)
      result.push(target.device_id)
    }
  }
  return result
}

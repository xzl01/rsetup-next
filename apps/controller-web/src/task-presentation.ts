import type { MainTaskState, TaskPreview } from './task-ops'

export const SUBTASK_STATES = [
  'queued',
  'held',
  'dispatching',
  'accepted',
  'verifying',
  'succeeded',
  'failed',
  'unknown',
  'cancelled',
  'expired',
] as const

export type SubTaskState = typeof SUBTASK_STATES[number]

const SUBTASK_STATE_SET = new Set<string>(SUBTASK_STATES)

export const SPEC_REASON_CODES = [
  'PERMISSION_DENIED',
  'DEVICE_OFFLINE',
  'CAPABILITY_UNAVAILABLE',
  'DEVICE_BUSY',
  'QUEUE_EXPIRED',
  'TIME_UNCERTAIN',
  'EXECUTION_REJECTED',
  'OS_ERROR',
  'RESULT_TIMEOUT',
  'JOURNAL_LOST',
  'PROTOCOL_MISMATCH',
  'USER_CANCELLED',
] as const

export type SpecReasonCode = typeof SPEC_REASON_CODES[number]

const SPEC_REASON_CODE_SET = new Set<string>(SPEC_REASON_CODES)

export function taskStateLabel(state: SubTaskState, t: (key: string) => string): string {
  if (SUBTASK_STATE_SET.has(state)) {
    return t(`task.state.${state}`)
  }
  return t('task.state.unknown')
}

export function reasonCodeLabel(code: string, t: (key: string) => string): string {
  // Only valid spec reason codes are formatted into translation keys.
  // Unknown, malformed, or excessively long strings safely fall back without embedding raw input into translation keys.
  if (SPEC_REASON_CODE_SET.has(code)) {
    return t(`task.reason.${code}`)
  }
  return t('task.reason.unknown')
}

export function generateUuidV4(): string {
  const c = typeof globalThis !== 'undefined' ? globalThis.crypto : undefined
  if (!c || typeof c.getRandomValues !== 'function') {
    throw new Error('CSPRNG unavailable: crypto.getRandomValues is required')
  }
  const bytes = new Uint8Array(16)
  c.getRandomValues(bytes)
  bytes[6] = (bytes[6] & 0x0f) | 0x40 // RFC 4122 version 4
  bytes[8] = (bytes[8] & 0x3f) | 0x80 // RFC 4122 variant 1
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

export type TimeReviewDecision = 'resume_original_deadline' | 'expire'

export const TIME_REVIEW_DECISIONS: readonly TimeReviewDecision[] = ['resume_original_deadline', 'expire']

export function timeReviewDecisionKeys(): Record<TimeReviewDecision, string> {
  return {
    resume_original_deadline: 'task.held.reviewResumeOriginalDeadline',
    expire: 'task.held.reviewExpire',
  }
}

export interface TaskOperationState { key: string | null; inFlight: boolean }

export function resetTaskOperation(): TaskOperationState {
  return { key: null, inFlight: false }
}

export function startTaskOperation(previous: TaskOperationState): { key: string; inFlight: boolean } {
  if (previous.inFlight) throw new Error('TASK_OPERATION_BUSY')
  // 一次确认只生成一次 key：重试（inFlight 已落下）必须沿用原 key，绝不重新生成
  const key = previous.key ?? generateUuidV4()
  return { key, inFlight: true }
}

export function previewFingerprint(preview: TaskPreview): string {
  // 稳定字符串：preview_token + 排序去重后的固定清单 device_id（含 eligible 状态）；
  // 重复条目不得改变指纹，否则下游会误判目标清单变更并 reset 幂等 key
  const targetSet = new Set(
    preview.targets.map((target) => `${target.device_id}:${target.eligible ? '1' : '0'}`)
  )
  const targets = Array.from(targetSet).sort().join(',')
  return `${preview.preview_token}|${targets}`
}

const DEVICE_ID_HEX64 = /^[0-9a-f]{64}$/

export function buildTaskPreviewRequest(operation: 'reboot', deviceIds: string[]): { operation: 'reboot'; device_ids: string[] } {
  const seen = new Set<string>()
  const device_ids: string[] = []
  for (const id of deviceIds) {
    if (!DEVICE_ID_HEX64.test(id)) throw new Error('INVALID_DEVICE_ID')
    if (!seen.has(id)) {
      seen.add(id)
      device_ids.push(id)
    }
  }
  if (device_ids.length === 0) throw new Error('EMPTY_TASK_TARGETS')
  return { operation, device_ids }
}

export function mainTaskStateLabel(state: MainTaskState, t: (key: string) => string): string {
  return t(`task.mainState.${state}`)
}

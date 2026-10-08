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

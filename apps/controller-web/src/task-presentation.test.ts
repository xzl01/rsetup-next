import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  generateUuidV4,
  reasonCodeLabel,
  taskStateLabel,
  type SubTaskState,
  TIME_REVIEW_DECISIONS,
  buildTaskPreviewRequest,
  mainTaskStateLabel,
  previewFingerprint,
  resetTaskOperation,
  startTaskOperation,
  timeReviewDecisionKeys,
} from './task-presentation'
import type { TaskPreview } from './task-ops'
import { createI18n } from './i18n'

describe('taskStateLabel', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null, document: null })

  const allStates: SubTaskState[] = [
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
  ]

  it('translates all 10 states using actual zh-CN dictionary instead of returning empty or fallback key', () => {
    i18n.setLocale('zh-CN')
    for (const st of allStates) {
      const label = taskStateLabel(st, i18n.t)
      expect(label).toBeTruthy()
      expect(label).not.toBe('')
      expect(label).not.toBe(`task.state.${st}`)
    }
    // Verify unknown is not mapped to failed/succeeded
    expect(taskStateLabel('unknown', i18n.t)).not.toBe(taskStateLabel('failed', i18n.t))
    expect(taskStateLabel('unknown', i18n.t)).not.toBe(taskStateLabel('succeeded', i18n.t))
  })

  it('translates all 10 states using actual en dictionary instead of returning empty or fallback key', () => {
    i18n.setLocale('en')
    for (const st of allStates) {
      const label = taskStateLabel(st, i18n.t)
      expect(label).toBeTruthy()
      expect(label).not.toBe('')
      expect(label).not.toBe(`task.state.${st}`)
    }
    // Verify unknown is distinct in English too
    expect(taskStateLabel('unknown', i18n.t)).not.toBe(taskStateLabel('failed', i18n.t))
    expect(taskStateLabel('unknown', i18n.t)).not.toBe(taskStateLabel('succeeded', i18n.t))
  })
})

describe('reasonCodeLabel', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null, document: null })

  // Specs 04 §2 lists 12 machine reason codes:
  // PERMISSION_DENIED, DEVICE_OFFLINE, CAPABILITY_UNAVAILABLE, DEVICE_BUSY,
  // QUEUE_EXPIRED, TIME_UNCERTAIN, EXECUTION_REJECTED, OS_ERROR,
  // RESULT_TIMEOUT, JOURNAL_LOST, PROTOCOL_MISMATCH, USER_CANCELLED
  const specReasonCodes = [
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
  ]

  it('translates all 12 spec reason codes into Chinese dictionary entries', () => {
    i18n.setLocale('zh-CN')
    for (const code of specReasonCodes) {
      const label = reasonCodeLabel(code, i18n.t)
      expect(label).toBeTruthy()
      expect(label).not.toBe('')
      expect(label).not.toBe(`task.reason.${code}`)
    }
  })

  it('translates all 12 spec reason codes into English dictionary entries', () => {
    i18n.setLocale('en')
    for (const code of specReasonCodes) {
      const label = reasonCodeLabel(code, i18n.t)
      expect(label).toBeTruthy()
      expect(label).not.toBe('')
      expect(label).not.toBe(`task.reason.${code}`)
    }
  })

  it('safely falls back for unknown/malformed/overly-long reason codes without embedding raw untrusted strings into translation keys', () => {
    i18n.setLocale('zh-CN')
    const tSpy = vi.fn(i18n.t)

    // Test unknown code
    const unknownResult = reasonCodeLabel('SOME_STRANGE_NEW_CODE', tSpy)
    expect(unknownResult).toBeTruthy()
    // It should safely fallback to generic unknown reason or safe representation
    // Crucially: it should not format raw untrusted injection into i18n keys or bypass fallback

    // Test overly long code (e.g. 500 chars)
    const longCode = 'A'.repeat(500)
    const longResult = reasonCodeLabel(longCode, tSpy)
    expect(longResult).toBeTruthy()
    expect(longResult.length).toBeLessThan(100) // generic fallback label

    // Test malformed code with special chars / injection
    const injectionCode = '<script>alert(1)</script>; DROP TABLE;'
    const injectionResult = reasonCodeLabel(injectionCode, tSpy)
    expect(injectionResult).not.toContain('<script>')
    expect(tSpy).not.toHaveBeenCalledWith(expect.stringContaining('<script>'))
  })
})

describe('generateUuidV4', () => {
  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('generates canonical lowercase RFC 4122 v4 UUID with version 4 and variant 1 bits', () => {
    const uuidPattern = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
    const id = generateUuidV4()
    expect(id).toMatch(uuidPattern)
    expect(id).toBe(id.toLowerCase())
  })

  it('produces different UUIDs for different random byte sequences and uses crypto.getRandomValues', () => {
    const getRandomValuesSpy = vi.spyOn(globalThis.crypto, 'getRandomValues')

    const id1 = generateUuidV4()
    const id2 = generateUuidV4()
    expect(id1).not.toBe(id2)
    expect(getRandomValuesSpy).toHaveBeenCalled()

    // Test deterministic mock bytes
    const mockBytes = new Uint8Array([
      0x00, 0x11, 0x22, 0x33,
      0x44, 0x55,
      0x66, 0x77, // byte 6: 0x66 -> & 0x0f | 0x40 = 0x46
      0x88, 0x99, // byte 8: 0x88 -> & 0x3f | 0x80 = 0x88
      0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
    ])
    getRandomValuesSpy.mockImplementation((buf: any) => {
      buf.set(mockBytes)
      return buf
    })

    const fixedId = generateUuidV4()
    expect(fixedId).toBe('00112233-4455-4677-8899-aabbccddeeff')
  })

  it('throws an error when CSPRNG crypto.getRandomValues is unavailable with no Math.random/timestamp fallback', () => {
    const mathRandomSpy = vi.spyOn(Math, 'random')
    const dateNowSpy = vi.spyOn(Date, 'now')

    vi.stubGlobal('crypto', undefined)

    expect(() => generateUuidV4()).toThrow(/CSPRNG unavailable/i)
    expect(mathRandomSpy).not.toHaveBeenCalled()
    expect(dateNowSpy).not.toHaveBeenCalled()
  })

  it('throws when crypto exists but getRandomValues is not a function', () => {
    vi.stubGlobal('crypto', {} as any)
    expect(() => generateUuidV4()).toThrow(/CSPRNG unavailable/i)
  })
})

const DEV_A = 'a'.repeat(64)
const DEV_B = 'b'.repeat(64)
const UUID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

function preview(overrides: Partial<TaskPreview> = {}): TaskPreview {
  return {
    preview_token: 'tok-123',
    expires_in_ms: 60000,
    targets: [{ device_id: DEV_A, eligible: true }, { device_id: DEV_B, eligible: false, reason: 'DEVICE_OFFLINE' }],
    ...overrides,
  }
}

describe('idempotency key lifecycle for a single confirmation', () => {
  it('generates a canonical UUIDv4 on first confirmation and reuses the same key on retry', () => {
    const first = startTaskOperation(resetTaskOperation())
    expect(first.key).toMatch(UUID_V4)
    expect(first.inFlight).toBe(true)

    // 请求结果不明后的重试：先落下 busy，再以同一 key 重试，key 必须保持不变
    const afterUnknownResult = { key: first.key, inFlight: false }
    const retry = startTaskOperation(afterUnknownResult)
    expect(retry.key).toBe(first.key)
    expect(retry.key).toMatch(UUID_V4)
  })

  it('refuses a second concurrent submission while the operation is in flight', () => {
    const busy = { key: '0f8fad5b-d9cb-469f-a165-70867728950e', inFlight: true }
    expect(() => startTaskOperation(busy)).toThrowError('TASK_OPERATION_BUSY')
  })

  it('resets key and busy state when the preview token or target list changes', () => {
    const started = startTaskOperation(resetTaskOperation())
    const tokenChanged = previewFingerprint(preview()) !== previewFingerprint(preview({ preview_token: 'tok-456' }))
    const listChanged = previewFingerprint(preview()) !==
      previewFingerprint(preview({ targets: [{ device_id: DEV_B, eligible: true }] }))
    expect(tokenChanged).toBe(true)
    expect(listChanged).toBe(true)
    expect(previewFingerprint(preview())).toBe(previewFingerprint(preview()))

    const reset = resetTaskOperation()
    expect(reset).toEqual({ key: null, inFlight: false })
    expect(reset.key).not.toBe(started.key)
  })
})

describe('held time review decisions and preview request body', () => {
  it('offers exactly the two non-extending held decisions with dedicated dictionary keys', () => {
    expect([...TIME_REVIEW_DECISIONS]).toEqual(['resume_original_deadline', 'expire'])
    expect(timeReviewDecisionKeys()).toEqual({
      resume_original_deadline: 'task.held.reviewResumeOriginalDeadline',
      expire: 'task.held.reviewExpire',
    })
    expect(JSON.stringify(TIME_REVIEW_DECISIONS)).not.toMatch(/extend|deadline\+|longer/i)
  })

  it('builds the frozen reboot preview body with de-duplicated hex64 device ids and rejects empty targets', () => {
    expect(buildTaskPreviewRequest('reboot', [DEV_A, DEV_B, DEV_A]))
      .toEqual({ operation: 'reboot', device_ids: [DEV_A, DEV_B] })
    expect(() => buildTaskPreviewRequest('reboot', [])).toThrowError('EMPTY_TASK_TARGETS')
    expect(() => buildTaskPreviewRequest('reboot', ['A'.repeat(64)])).toThrowError('INVALID_DEVICE_ID')
  })
})

describe('mainTaskStateLabel is the independent main four-state dictionary', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null, document: null })
  const identity = (key: string) => key

  it('maps each of the four main states to its dedicated task.mainState.* key and never to the child task.state.* key', () => {
    for (const state of ['queued', 'running', 'held', 'completed'] as const) {
      expect(mainTaskStateLabel(state, identity)).toBe(`task.mainState.${state}`)
      expect(mainTaskStateLabel(state, identity)).not.toBe(`task.state.${state}`)
    }
  })

  it('resolves the four main-state labels in zh-CN', () => {
    i18n.setLocale('zh-CN')
    expect(mainTaskStateLabel('queued', i18n.t)).toBe('排队中')
    expect(mainTaskStateLabel('running', i18n.t)).toBe('运行中')
    expect(mainTaskStateLabel('held', i18n.t)).toBe('等待核验')
    expect(mainTaskStateLabel('completed', i18n.t)).toBe('已完成')
  })

  it('resolves the four main-state labels in en', () => {
    i18n.setLocale('en')
    expect(mainTaskStateLabel('queued', i18n.t)).toBe('Queued')
    expect(mainTaskStateLabel('running', i18n.t)).toBe('Running')
    expect(mainTaskStateLabel('held', i18n.t)).toBe('Awaiting verification')
    expect(mainTaskStateLabel('completed', i18n.t)).toBe('Completed')
  })
})

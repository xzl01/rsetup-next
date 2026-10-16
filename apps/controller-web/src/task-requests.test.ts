import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  TASK_API_PATHS,
  TIME_REVIEW_EVIDENCE_KINDS,
  cancelTask,
  createTaskPreview,
  getTask,
  getTaskChildren,
  isTimeReviewEvidenceKind,
  listTasks,
  releaseUnknownLock,
  reviewHeldTime,
  submitTask,
} from './task-requests'
import { baseTaskTimeEvidence, zeroTaskCounts } from './task-ops'

const DEV_A = 'a'.repeat(64)
const TASK_ID = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const SUB_ID = '123e4567-e89b-42d3-a456-426614174000'
const KEY = '0f8fad5b-d9cb-469f-a165-70867728950e'
const ctx = { csrfToken: 'csrf-token-1' }

const MAIN_TASK = {
  id: TASK_ID, operation: 'reboot', state: 'queued', outcome: null, revision: '7',
  created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(),
  view_scope: 'authorized_subset',
}
// 最小 6 字段子任务（02 §1:24 投影）；result_evidence 为 null（unknown/RESULT_TIMEOUT 尚未产生证据）
const SUB_TASK = {
  id: SUB_ID, device_id: DEV_A, state: 'unknown',
  reason_code: 'RESULT_TIMEOUT', revision: '4', result_evidence: null,
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } })
}

function stubFetch(body: unknown): ReturnType<typeof vi.fn> {
  const fetcher = vi.fn(async (_url: string, _init?: RequestInit) => jsonResponse(body))
  vi.stubGlobal('fetch', fetcher)
  return fetcher
}

function calls(fetcher: ReturnType<typeof vi.fn>): Array<[string, RequestInit]> {
  return fetcher.mock.calls as Array<[string, RequestInit]>
}

afterEach(() => { vi.unstubAllGlobals() })

describe('frozen real request whitelist', () => {
  it('exposes exactly the eight documented task endpoints under /api/v1', () => {
    expect(TASK_API_PATHS.preview).toBe('task-previews')
    expect(TASK_API_PATHS.createTask).toBe('tasks')
    expect(TASK_API_PATHS.listTasks).toBe('tasks')
    expect(TASK_API_PATHS.taskDetail(TASK_ID)).toBe(`tasks/${TASK_ID}`)
    expect(TASK_API_PATHS.taskChildren(TASK_ID)).toBe(`tasks/${TASK_ID}/children`)
    expect(TASK_API_PATHS.cancelTask(TASK_ID)).toBe(`tasks/${TASK_ID}/cancel`)
    expect(TASK_API_PATHS.timeReview(TASK_ID)).toBe(`tasks/${TASK_ID}/time-review`)
    expect(TASK_API_PATHS.releaseLock(SUB_ID)).toBe(`subtasks/${SUB_ID}/release-lock`)
  })

  it('never builds an invented endpoint such as a task retry or deadline extension route', () => {
    const built = JSON.stringify(TASK_API_PATHS) + TASK_API_PATHS.taskDetail(TASK_ID)
      + TASK_API_PATHS.taskChildren(TASK_ID) + TASK_API_PATHS.cancelTask(TASK_ID)
      + TASK_API_PATHS.timeReview(TASK_ID) + TASK_API_PATHS.releaseLock(SUB_ID)
    expect(built).not.toMatch(/extend|retry|deadline|task-previews\//)
  })
})

describe('preview, list, detail and children requests', () => {
  it('posts the frozen preview body without an idempotency key and returns guarded targets', async () => {
    const fetcher = stubFetch({ data: { preview_token: 'tok-1', expires_in_ms: 60000,
      targets: [{ device_id: DEV_A, eligible: true }] }, request_id: 'r-1' })
    const res = await createTaskPreview('reboot', [DEV_A], ctx)
    expect(res.data.preview_token).toBe('tok-1')
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe('/api/v1/task-previews')
    expect(init.method).toBe('POST')
    expect(JSON.parse(init.body as string)).toEqual({ operation: 'reboot', device_ids: [DEV_A] })
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')
    expect((init.headers as Record<string, string>)['X-CSRF-Token']).toBe('csrf-token-1')
  })

  it('lists tasks with an optional cursor query, no CSRF header and guarded page shape', async () => {
    const fetcher = stubFetch({ data: { items: [MAIN_TASK], next_cursor: null }, request_id: 'r-2' })
    const res = await listTasks('opaque-cursor-1', ctx)
    expect(res.data.items).toHaveLength(1)
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe('/api/v1/tasks?cursor=opaque-cursor-1')
    expect(init.method).toBe('GET')
    expect(JSON.stringify(init.headers)).not.toContain('X-CSRF-Token')

    const fetcher2 = stubFetch({ data: { items: [], next_cursor: null }, request_id: 'r-3' })
    await listTasks(null, ctx)
    expect(calls(fetcher2)[0]![0]).toBe('/api/v1/tasks')
  })

  it('fetches detail and paginated children and rejects malformed payloads', async () => {
    const fetcher = stubFetch({ data: MAIN_TASK, request_id: 'r-4' })
    await expect(getTask(TASK_ID, ctx)).resolves.toMatchObject({ data: { id: TASK_ID } })
    expect(calls(fetcher)[0]![0]).toBe(`/api/v1/tasks/${TASK_ID}`)

    const badFetcher = stubFetch({ data: { ...MAIN_TASK, revision: '007' }, request_id: 'r-5' })
    await expect(getTask(TASK_ID, ctx)).rejects.toThrowError('INVALID_REVISION')
    expect(calls(badFetcher)).toHaveLength(1)

    const childrenFetcher = stubFetch({ data: { items: [SUB_TASK], next_cursor: null }, request_id: 'r-6' })
    const children = await getTaskChildren(TASK_ID, null, ctx)
    expect(children.data.items[0]!.state).toBe('unknown')
    expect(calls(childrenFetcher)[0]![0]).toBe(`/api/v1/tasks/${TASK_ID}/children`)
  })
})

describe('write operations carry the confirmation facts the backend requires', () => {
  it('submits the task with the Idempotency-Key header and never inside the JSON body', async () => {
    const fetcher = stubFetch({ data: { task_id: TASK_ID, accepted: true }, request_id: 'r-7' })
    const res = await submitTask({ previewToken: 'tok-1', idempotencyKey: KEY, confirm: true, context: ctx })
    expect(res.data).toEqual({ task_id: TASK_ID, accepted: true })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe('/api/v1/tasks')
    expect((init.headers as Record<string, string>)['Idempotency-Key']).toBe(KEY)
    const body = JSON.parse(init.body as string)
    expect(body).toEqual({ preview_token: 'tok-1', confirm: true })
    expect(JSON.stringify(body)).not.toContain('idempotency')
  })

  // 5 个 task_id 负向形态（`it.each` 展开为 5 个用例，count 明确）：响应已过 `post`，但 `task_id` 不是规范
  // UUIDv4 时 submitTask 必须 reject `INVALID_TASK_SUBMISSION`，不得把坏 id 交给调用方。谓词来自 Task 3
  // `task-ops.ts` 的 `isCanonicalUuidV4`（同一正则，无 `m` 标志）。末项"合法 UUID + 换行"是回归检查：
  // JS 裸 `$` 即严格 end input，该形态当前本就被拒绝，不声称这是修复正则的 RED。
  it.each([
    ['empty string', ''],
    ['not a uuid', 'not-a-uuid'],
    ['uppercase hex', 'F47AC10B-58CC-4372-A567-0E02B2C3D479'],
    ['v1 or wrong variant', 'f47ac10b-58cc-1372-a567-0e02b2c3d479'],
    ['trailing newline', `${TASK_ID}\n`],
  ])('rejects a submit response whose task_id is a %s with INVALID_TASK_SUBMISSION', async (_label, task_id) => {
    stubFetch({ data: { task_id, accepted: true }, request_id: 'synthetic' })
    await expect(submitTask({ previewToken: 'tok-1', idempotencyKey: KEY, confirm: true, context: ctx }))
      .rejects.toThrow('INVALID_TASK_SUBMISSION')
  })

  it('cancels only with an expected revision CAS payload and sends no Idempotency-Key', async () => {
    const fetcher = stubFetch({ data: { accepted: true, cancelled: [TASK_ID] }, request_id: 'r-8' })
    await cancelTask({ taskId: TASK_ID, expectedRevision: '7', confirm: true, context: ctx })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe(`/api/v1/tasks/${TASK_ID}/cancel`)
    expect(JSON.parse(init.body as string)).toEqual({ expected_revision: '7', confirm: true })
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')
  })

  // cancelTask 完整响应形状守卫：`post` 只校验信封，业务 `data` 由 cancelTask 自校验。
  // 正例（2 项）经真实 `post` 走合法信封，断言 resolve 且原样返回（null = 无可取消目标；空数组按合同 `string[] | null` 放行，
  // 由正例数组形态覆盖语义、不另立用例）；负例（9 项）覆盖 `cancelled` 缺失(undefined)/数字 42/字符串/对象/
  // 数组含坏 UUID/数组含 uppercase/数组含 wrong variant/数组含 numbers 及 `accepted: false`，
  // 一律 rejects `INVALID_CANCEL_RESPONSE`——不新建 DTO / state / HTTP、不混用 `INVALID_TASK_SUBMISSION`、额外字段忽略。
  // `isCanonicalUuidV4` 本文件已为 submitTask 导入，不重复 import；上方既有 header/body 用例原样保留；
  // 清理依赖既有 `afterEach` 的 `vi.unstubAllGlobals()`。
  it.each([
    ['null payload for an unreadable target', null],
    ['a canonical UUIDv4 array', [TASK_ID]],
  ])('resolves a cancel response whose cancelled is %s', async (_label, cancelled) => {
    stubFetch({ data: { accepted: true, cancelled }, request_id: 'synthetic' })
    const res = await cancelTask({ taskId: TASK_ID, expectedRevision: '7', confirm: true, context: ctx })
    expect(res.data).toEqual({ accepted: true, cancelled })
  })

  it.each([
    ['missing cancelled field', { accepted: true }],
    ['a numeric cancelled', { accepted: true, cancelled: 42 }],
    ['a string cancelled', { accepted: true, cancelled: TASK_ID }],
    ['an object cancelled', { accepted: true, cancelled: { task: TASK_ID } }],
    ['an array with a bad uuid', { accepted: true, cancelled: ['not-a-uuid'] }],
    ['an array with uppercase hex', { accepted: true, cancelled: ['F47AC10B-58CC-4372-A567-0E02B2C3D479'] }],
    ['an array with a wrong variant', { accepted: true, cancelled: ['f47ac10b-58cc-1372-a567-0e02b2c3d479'] }],
    ['an array of numbers', { accepted: true, cancelled: [1, 2] }],
    ['accepted false', { accepted: false, cancelled: null }],
  ])('rejects a cancel response with %s with INVALID_CANCEL_RESPONSE', async (_label, data) => {
    stubFetch({ data, request_id: 'synthetic' })
    await expect(cancelTask({ taskId: TASK_ID, expectedRevision: '7', confirm: true, context: ctx }))
      .rejects.toThrow('INVALID_CANCEL_RESPONSE')
  })

  it('reviews held time with both frozen decisions, sends no Idempotency-Key and refuses unknown decisions or non-enum evidence before fetch', async () => {
    const fetcher = stubFetch({ data: { ...MAIN_TASK, state: 'queued' }, request_id: 'r-9' })
    // 正例：两个 decision 都按后端当前词表发明确合法枚举（不发额外 reason 引用），evidence 严格三串
    await reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'resume_original_deadline', evidence: 'reference_deadline_proven', context: ctx })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe(`/api/v1/tasks/${TASK_ID}/time-review`)
    expect(JSON.parse(init.body as string)).toEqual({
      expected_revision: '7', decision: 'resume_original_deadline', evidence: 'reference_deadline_proven',
    })
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')

    await reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'expire', evidence: 'reference_deadline_expired', context: ctx })
    const [, second] = calls(fetcher)[1]!
    expect(JSON.parse(second.body as string)).toEqual({
      expected_revision: '7', decision: 'expire', evidence: 'reference_deadline_expired',
    })
    expect(JSON.stringify(second.headers)).not.toContain('Idempotency-Key')
    expect(calls(fetcher)).toHaveLength(2)

    // 谓词完整词表：恰接受三值，其余形态（自由文本/PascalCase/尾随空格/数字/null/布尔）全拒
    expect([...TIME_REVIEW_EVIDENCE_KINDS]).toEqual(['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'])
    for (const value of ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'] as const) {
      expect(isTimeReviewEvidenceKind(value)).toBe(true)
    }
    for (const value of ['', 'server evidence ref', 'proven', 'Inconclusive', 'reference_deadline_proven ', 42, null, true] as const) {
      expect(isTimeReviewEvidenceKind(value)).toBe(false)
    }

    // 负例 table：空/自由文本/未知/数字/null/尾随空格/PascalCase 全在 post 前被拒，请求 spy 调用次数不增
    const untouched = stubFetch({ data: MAIN_TASK, request_id: 'r-10' })
    const illegalEvidence = ['', 'server evidence ref', 'proven', 42, null, 'reference_deadline_proven ', 'Inconclusive'] as const
    for (const evidence of illegalEvidence) {
      await expect(reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'resume_original_deadline', evidence: evidence as unknown as string, context: ctx }))
        .rejects.toThrowError('INVALID_TIME_REVIEW_EVIDENCE')
    }
    // decision 校验先于 evidence：decision 非法时（此处 evidence 也是自由文本）仍先报 INVALID_TIME_REVIEW_DECISION
    await expect(reviewHeldTime({ taskId: TASK_ID, expectedRevision: '7', decision: 'extend' as never, evidence: 'server evidence ref', context: ctx }))
      .rejects.toThrowError('INVALID_TIME_REVIEW_DECISION')
    expect(calls(untouched)).toHaveLength(0)
  })

  it('refuses unknown lock release without explicit acknowledgement and a non-blank reason, then validates only {released:true,state:unknown}', async () => {
    const untouched = stubFetch({ data: { released: true, state: 'unknown' }, request_id: 'r-11' })
    await expect(releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: false as never, reason: 'ok', context: ctx }))
      .rejects.toThrowError('RELEASE_CONFIRMATION_REQUIRED')
    await expect(releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: true, reason: '   ', context: ctx }))
      .rejects.toThrowError('RELEASE_CONFIRMATION_REQUIRED')
    expect(calls(untouched)).toHaveLength(0)

    // 真实响应只有 released/state 两个字段：不得用完整 SubTaskItem 守卫，也不得期待 lock_released/revision
    const fetcher = stubFetch({ data: { released: true, state: 'unknown' }, request_id: 'r-12' })
    const res = await releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: true, reason: '现场人工确认', context: ctx })
    expect(res.data).toEqual({ released: true, state: 'unknown' })
    const [url, init] = calls(fetcher)[0]!
    expect(url).toBe(`/api/v1/subtasks/${SUB_ID}/release-lock`)
    expect(JSON.parse(init.body as string)).toEqual({
      expected_revision: '4', acknowledge_unknown: true, reason: '现场人工确认',
    })
    expect(init.method).toBe('POST')
    expect(JSON.stringify(init.headers)).not.toContain('Idempotency-Key')

    const malformed = stubFetch({ data: { released: true, state: 'succeeded' }, request_id: 'r-13' })
    await expect(releaseUnknownLock({ subTaskId: SUB_ID, expectedRevision: '4', acknowledgeUnknown: true, reason: '现场人工确认', context: ctx }))
      .rejects.toThrowError('INVALID_RELEASE_LOCK_STATE')
    expect(calls(malformed)).toHaveLength(1)
  })
})

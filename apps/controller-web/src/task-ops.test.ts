import { describe, expect, it } from 'vitest'
import {
  assertReleaseLockResultShape,
  assertSubTaskItemShape,
  assertSubTaskPageShape,
  assertTaskItemShape,
  assertTaskPageShape,
  assertTaskPreviewShape,
  baseTaskTimeEvidence,
  canRequestCancel,
  describeAuthorizedSubset,
  eligibleDeviceIds,
  zeroTaskCounts,
  type ReleaseLockResult,
  type SubTaskItem,
  type TaskItem,
  type TaskTimeEvidence,
  type TaskPreview,
} from './task-ops'

const DEV_A = 'a'.repeat(64)
const DEV_B = 'b'.repeat(64)
const TASK_ID = 'f47ac10b-58cc-4372-a567-0e02b2c3d479'
const SUB_ID = '123e4567-e89b-42d3-a456-426614174000'

function mainTask(overrides: Partial<TaskItem> = {}): TaskItem {
  return { id: TASK_ID, operation: 'reboot', state: 'queued', outcome: null, revision: '7',
    created_time: baseTaskTimeEvidence(), counts: zeroTaskCounts(),
    view_scope: 'authorized_subset', ...overrides }
}

// 最小 6 字段合同（02 §1:24）：id/device_id/state/reason_code/revision/result_evidence；
// 不含 main_task_id/lock_released（DB 内部列/后端不承诺输出的可选兼容字段）
function subTask(overrides: Partial<SubTaskItem> = {}): SubTaskItem {
  return { id: SUB_ID, device_id: DEV_A, state: 'held',
    reason_code: 'TIME_UNCERTAIN', revision: '3', result_evidence: null, ...overrides }
}

const preview: TaskPreview = {
  preview_token: 'tok-1', expires_in_ms: 60000,
  targets: [
    { device_id: DEV_A, eligible: true, reason: null },
    { device_id: DEV_B, eligible: false, reason: 'DEVICE_BUSY' },
    { device_id: DEV_B, eligible: false, reason: 'PERMISSION_DENIED' },
  ],
}

describe('task-ops shape guards are fail-closed', () => {
  it('accepts a well-formed preview and rejects unknown reason vocabulary', () => {
    expect(() => assertTaskPreviewShape(preview)).not.toThrow()
    // 真实 producer 正例（来源=Runtime Task 6 DTO，账本决议③）：SCREAMING ReasonCode 与 eligible 的 reason:null 均放行
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: DEV_A, eligible: false, reason: 'DEVICE_OFFLINE' }] })).not.toThrow()
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: DEV_A, eligible: true, reason: null }] })).not.toThrow()
    // 负例：非法字符串仍抛 INVALID_PREVIEW_REASON
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: DEV_A, eligible: false, reason: 'maybe_later' }] }))
      .toThrowError('INVALID_PREVIEW_REASON')
    expect(() => assertTaskPreviewShape({ ...preview, targets: [{ device_id: 'A'.repeat(64), eligible: true }] }))
      .toThrowError('INVALID_DEVICE_ID')
    expect(() => assertTaskPreviewShape({ ...preview, expires_in_ms: 0 })).toThrowError('INVALID_PREVIEW_EXPIRY')
  })

  it('rejects non-u64 revision, non-v4 task id and unknown state', () => {
    expect(() => assertTaskItemShape(mainTask({ revision: '07' }))).toThrowError('INVALID_REVISION')
    expect(() => assertTaskItemShape(mainTask({ id: 'not-a-uuid' }))).toThrowError('INVALID_TASK_ID')
    expect(() => assertTaskItemShape(mainTask({ state: 'done' as never }))).toThrowError('INVALID_MAIN_TASK_STATE')
    expect(() => assertTaskItemShape(mainTask({ operation: 'shutdown' as never }))).toThrowError('INVALID_TASK_OPERATION')
    // view_scope 是可选字段，但一旦出现必须是合法值（full / authorized_subset）
    expect(() => assertTaskItemShape(mainTask({ view_scope: 'all' as never }))).toThrowError('INVALID_VIEW_SCOPE')
    expect(() => assertTaskItemShape(mainTask({ view_scope: 'full' }))).not.toThrow()
    // actions 可选（只读任务兼容缺失）；一旦存在 can_cancel 必须是布尔（fail-closed，不做降级映射）
    expect(() => assertTaskItemShape(mainTask({ actions: { can_cancel: true } }))).not.toThrow()
    expect(() => assertTaskItemShape(mainTask({ actions: { can_cancel: 'yes' as never } }))).toThrowError('INVALID_TASK_ACTIONS')
    expect(() => assertTaskItemShape(mainTask({ actions: {} as never }))).toThrowError('INVALID_TASK_ACTIONS')
    expect(() => assertTaskItemShape(mainTask({ actions: null as never }))).toThrowError('INVALID_TASK_ACTIONS')
  })

  it('accepts the four main states with the 04 §4 state/outcome pairing', () => {
    for (const state of ['queued', 'running', 'held'] as const) {
      expect(() => assertTaskItemShape(mainTask({ state, outcome: null }))).not.toThrow()
    }
    for (const outcome of ['success', 'cancelled', 'unknown', 'partial', 'failed'] as const) {
      expect(() => assertTaskItemShape(mainTask({ state: 'completed', outcome }))).not.toThrow()
    }
  })

  it('rejects child states as main states and any state/outcome mispairing', () => {
    for (const state of ['dispatching', 'accepted', 'verifying', 'succeeded', 'failed', 'unknown', 'cancelled', 'expired'] as const) {
      expect(() => assertTaskItemShape(mainTask({ state: state as never, outcome: null })))
        .toThrowError('INVALID_MAIN_TASK_STATE')
    }
    expect(() => assertTaskItemShape(mainTask({ state: 'running', outcome: 'success' })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
    expect(() => assertTaskItemShape(mainTask({ state: 'completed' })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
    expect(() => assertTaskItemShape(mainTask({ state: 'completed', outcome: null })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
    expect(() => assertTaskItemShape(mainTask({ state: 'completed', outcome: 'succeeded' as never })))
      .toThrowError('INVALID_MAIN_TASK_OUTCOME')
  })

  it('rejects a page whose cursor is not a safe opaque string, unknown subtask reason codes, and malformed result evidence', () => {
    expect(() => assertTaskPageShape({ items: [mainTask()], next_cursor: 'bad\u0000cursor' }))
      .toThrowError('INVALID_NEXT_CURSOR')
    expect(() => assertTaskPageShape({ items: [mainTask()], next_cursor: null })).not.toThrow()
    // 后端最小 6 字段合法（02 §1:24 SubTask 投影）：result_evidence 为 JSON null（证据未产生）亦合法；
    // main_task_id/pre_boot_id/observed_boot_id/evidence_source 不再是 HTTP 必需字段
    expect(() => assertSubTaskItemShape(subTask())).not.toThrow()
    expect(() => assertSubTaskItemShape(subTask({ result_evidence: null }))).not.toThrow()
    // 六 enum 全合法；'none' 是 EvidenceKind 枚举字符串，与 Option None → JSON null 不同义
    for (const evidence of ['attempted', 'boot_transition_observed', 'execution_rejected', 'os_error', 'journal_lost', 'none'] as const) {
      expect(() => assertSubTaskItemShape(subTask({ result_evidence: evidence }))).not.toThrow()
    }
    // 响应扩展字段（含 execution_token、main_task_id）忽略不拒绝；守卫不将其用于展示或安全判断
    //（UI 侧同样不得消费，见 Task 6/9 渲染契约）
    expect(() => assertSubTaskItemShape({ ...subTask(), execution_token: 'tok', main_task_id: TASK_ID })).not.toThrow()
    // 非法证据：旧来源词表 controller/board、PascalCase、unknown 枚举、对象（勿把完整 wire 证据对象当六值字符串）、数/布尔
    for (const badEvidence of ['controller', 'board', 'BootTransitionObserved', 'FULL_TASK_EVIDENCE', {}, 7, true] as Array<unknown>) {
      expect(() => assertSubTaskItemShape({ ...subTask(), result_evidence: badEvidence as never }))
        .toThrowError('INVALID_RESULT_EVIDENCE')
    }
    // id 为规范 UUIDv4（不放宽为非空 ≤128）
    expect(() => assertSubTaskItemShape(subTask({ id: 'not-a-uuid' }))).toThrowError('INVALID_SUB_TASK_ID')
    // 未知 reason_code / 未知 state 按既有守卫关闭
    expect(() => assertSubTaskItemShape(subTask({ reason_code: 'MADE_UP' as never })))
      .toThrowError('INVALID_REASON_CODE')
    expect(() => assertSubTaskPageShape({ items: [subTask({ state: 'restarting' as never })], next_cursor: null }))
      .toThrowError('INVALID_SUB_TASK_STATE')
    // 6 字段合同：reason_code/result_evidence 的 key 必含（值 nullable）：缺 key 或 undefined 按既有守卫拒绝，合法 null 仍合法
    const missingReason: Record<string, unknown> = { ...subTask() }; delete missingReason.reason_code
    const missingEvidence: Record<string, unknown> = { ...subTask() }; delete missingEvidence.result_evidence
    expect(() => assertSubTaskItemShape(missingReason)).toThrowError('INVALID_REASON_CODE')
    expect(() => assertSubTaskItemShape(subTask({ reason_code: undefined as never }))).toThrowError('INVALID_REASON_CODE')
    expect(() => assertSubTaskItemShape(missingEvidence)).toThrowError('INVALID_RESULT_EVIDENCE')
    expect(() => assertSubTaskItemShape(subTask({ result_evidence: undefined as never }))).toThrowError('INVALID_RESULT_EVIDENCE')
    expect(() => assertSubTaskItemShape(subTask({ reason_code: null, result_evidence: null }))).not.toThrow()
    // lock_released 保留可选 fail-closed 兼容：出现必须是布尔（后端投影不承诺输出该字段）
    expect(() => assertSubTaskItemShape(subTask({ lock_released: true }))).not.toThrow()
    expect(() => assertSubTaskItemShape(subTask({ lock_released: 'yes' as never })))
      .toThrowError('INVALID_LOCK_RELEASED')
  })

  it('validates release lock response strictly containing only released:true and state:unknown', () => {
    const valid: ReleaseLockResult = { released: true, state: 'unknown' }
    expect(() => assertReleaseLockResultShape(valid)).not.toThrow()
    expect(() => assertReleaseLockResultShape({ released: false, state: 'unknown' }))
      .toThrowError('INVALID_RELEASE_LOCK_RELEASED')
    expect(() => assertReleaseLockResultShape({ released: true, state: 'succeeded' }))
      .toThrowError('INVALID_RELEASE_LOCK_STATE')
    expect(() => assertReleaseLockResultShape({ state: 'unknown' }))
      .toThrowError('INVALID_RELEASE_LOCK_RELEASED')
  })

  it('accepts a legal full TimeEvidence and ten-key u64-string counts (05 §3 / 04 §4)', () => {
    // 正例：Z 与 +00:00、小数秒、已知与保守未知 quality、全 null、负 offset、零与 u64 上界计数
    expect(() => assertTaskItemShape(mainTask({
      created_time: {
        system_wall_utc: '2026-10-16T00:00:00+00:00',
        reference_utc: '2026-10-16T00:00:00.250Z',
        quality: 'ntp_valid',
        clock_epoch: '0f8fad5b-d9cb-469f-a165-70867728950e',
        source: 'ops-ref-1', sample_age_ms: '0', offset_ms: -5, uncertainty_ms: 0,
      },
      counts: { ...zeroTaskCounts(), held: '3', succeeded: '18446744073709551615' },
    }))).not.toThrow()
    // 正例（日历/闰秒/64 位边界）：闰年 2 月 29（含 <100 的四位年 0004）、9 位纳秒、闰秒 23:59:60、
    // i64 下界 offset（双精度精确可表示）、>2^53 大整数 uncertainty（2^53，不冒称 >2^53 无损往返）
    expect(() => assertTaskItemShape(mainTask({
      created_time: baseTaskTimeEvidence({
        system_wall_utc: '0004-02-29T23:59:60.123456789Z',
        reference_utc: '2024-02-29T23:59:59.999+00:00',
        offset_ms: -9223372036854775808,
        uncertainty_ms: 2 ** 53,
      }),
    }))).not.toThrow()
    expect(() => assertTaskItemShape(mainTask({
      created_time: baseTaskTimeEvidence({ quality: 'degraded_future' }),
    }))).not.toThrow()
  })

  it('rejects legacy string created_time and malformed TimeEvidence fields with INVALID_TIME_EVIDENCE', () => {
    // 旧 DTO 的 string 形状必须不再被接受（case 表 for 只算本 it 一个方法）
    expect(() => assertTaskItemShape(mainTask({ created_time: '2026-10-16T00:00:00Z' as never })))
      .toThrowError('INVALID_TIME_EVIDENCE')
    for (const bad of [
      { reference_utc: '2026-10-16T08:00:00+08:00' },   // 非 UTC 偏移
      { system_wall_utc: '2026-10-16 00:00:00Z' },      // 空格分隔而非 T
      { system_wall_utc: '2026-13-01T00:00:00Z' },      // 非法月 13
      { reference_utc: '2026-00-10T00:00:00Z' },        // 非法月 0
      { system_wall_utc: '2026-02-30T00:00:00Z' },      // 2 月 30（日历校验不得依赖 Date.parse 归一化）
      { reference_utc: '2026-02-29T00:00:00Z' },        // 非闰年 2 月 29
      { system_wall_utc: '2026-10-16T24:00:00Z' },      // 超范围时 24
      { reference_utc: '2026-10-16T00:60:00Z' },        // 超范围分 60
      { system_wall_utc: '2026-10-16T23:59:61Z' },      // 超范围秒 61
      { reference_utc: '2026-10-16T00:00:60Z' },        // 闰秒 :60 位于非 23:59（保守边界，见 Step 4）
      { clock_epoch: 'not-a-uuid' },
      { quality: 3 as never },
      { source: 7 as never },
      { sample_age_ms: '01' },                           // 前导零
      { offset_ms: Number.POSITIVE_INFINITY },           // 非有限
      { offset_ms: 1.5 },                                // 小数（i64 要求整数）
      { offset_ms: '5' as never },                       // JSON 字符串而非数字
      { uncertainty_ms: -1 },                            // 负数
      { uncertainty_ms: 0.5 },                           // 小数（u64 要求非负整数）
    ] as Array<Partial<TaskTimeEvidence>>) {
      expect(() => assertTaskItemShape(mainTask({ created_time: baseTaskTimeEvidence(bad) })))
        .toThrowError('INVALID_TIME_EVIDENCE')
    }
  })

  it('rejects missing, numeric, negative, leading-zero and overflowing counts with INVALID_TASK_COUNTS', () => {
    for (const badCounts of [
      { ...zeroTaskCounts(), held: undefined as never },                  // 缺键
      { ...zeroTaskCounts(), queued: 7 as never },                        // JSON 数字而非字符串
      { ...zeroTaskCounts(), failed: '-1' },                              // 负数
      { ...zeroTaskCounts(), cancelled: '007' },                          // 前导零
      { ...zeroTaskCounts(), expired: '18446744073709551616' },           // u64 溢出
    ]) {
      expect(() => assertTaskItemShape(mainTask({ counts: badCounts })))
        .toThrowError('INVALID_TASK_COUNTS')
    }
  })
})

describe('authorized subset projection', () => {
  it('marks an explicit authorized_subset scope and treats anything else as unknown scope', () => {
    expect(describeAuthorizedSubset('authorized_subset'))
      .toEqual({ kind: 'authorized_subset', key: 'task.scope.authorizedSubset' })
    for (const value of [undefined, null, '', 'all', 'full', 1]) {
      expect(describeAuthorizedSubset(value)).toEqual({ kind: 'unknown', key: 'task.scope.unknown' })
    }
  })

  it('never claims completeness: unknown scope must not reuse the authorized subset key', () => {
    expect(describeAuthorizedSubset(undefined).key).not.toBe('task.scope.authorizedSubset')
  })

  it('selects only eligible device ids, de-duplicated and order-preserving', () => {
    const p: TaskPreview = {
      preview_token: 'tok-2', expires_in_ms: 60000,
      targets: [
        { device_id: DEV_A, eligible: true },
        { device_id: DEV_B, eligible: false, reason: 'DEVICE_OFFLINE' },
        { device_id: DEV_A, eligible: true },
      ],
    }
    expect(eligibleDeviceIds(p)).toEqual([DEV_A])
    expect(eligibleDeviceIds({ ...p, targets: [] })).toEqual([])
  })
})

describe('canRequestCancel pure helper', () => {
  it('opens for a running main task when the backend reports can_cancel true (RED 正例，先失败)', () => {
    expect(canRequestCancel(mainTask({ state: 'running', actions: { can_cancel: true } }))).toBe(true)
  })

  it('keeps an authorized_subset completed main task open when the backend allows', () => {
    expect(canRequestCancel(mainTask({ state: 'completed', outcome: 'cancelled', actions: { can_cancel: true } }))).toBe(true)
  })

  it('opens under a legitimate full scope when the backend allows', () => {
    expect(canRequestCancel(mainTask({ view_scope: 'full', actions: { can_cancel: true } }))).toBe(true)
  })

  it('closes on backend false, missing actions, or malformed can_cancel regardless of main state', () => {
    expect(canRequestCancel(mainTask({ state: 'queued', actions: { can_cancel: false } }))).toBe(false)
    expect(canRequestCancel(mainTask({ state: 'running', actions: undefined }))).toBe(false)
    expect(canRequestCancel(mainTask({ state: 'running', actions: { can_cancel: 'yes' as never } }))).toBe(false)
  })

  it('closes the dangerous entry for unknown or missing view_scope even when can_cancel is true', () => {
    expect(canRequestCancel(mainTask({ view_scope: 'legacy' as never, actions: { can_cancel: true } }))).toBe(false)
    expect(canRequestCancel(mainTask({ view_scope: undefined, actions: { can_cancel: true } }))).toBe(false)
  })
})

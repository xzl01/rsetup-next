import { get, post } from './api'
import type { ApiResponse } from './types'
import {
  assertReleaseLockResultShape,
  assertSubTaskPageShape,
  assertTaskItemShape,
  assertTaskPageShape,
  assertTaskPreviewShape,
  isCanonicalUuidV4,
  type ReleaseLockResult,
  type SubTaskItem,
  type TaskItem,
  type TaskPage,
  type TaskPreview,
} from './task-ops'
import { TIME_REVIEW_DECISIONS, buildTaskPreviewRequest, type TimeReviewDecision } from './task-presentation'

// time-review evidence 词表：02 §HTTP:156 `evidence`；与 Runtime 计划 Task 4 `TimeReviewEvidenceKind` snake_case wire 1:1 对齐（Task 6 `TimeReviewTaskRequest.evidence` 直接复用该枚举）；不是新 permission/action/请求字段
export type TimeReviewEvidenceKind = 'reference_deadline_proven' | 'reference_deadline_expired' | 'inconclusive'
export const TIME_REVIEW_EVIDENCE_KINDS: readonly TimeReviewEvidenceKind[] = ['reference_deadline_proven', 'reference_deadline_expired', 'inconclusive'] as const
// 完整词表校验：严格只接受上面三串，其余形态（自由文本/PascalCase/尾随空格/数字/布尔/null）全拒，不降级映射
export function isTimeReviewEvidenceKind(value: unknown): value is TimeReviewEvidenceKind {
  return typeof value === 'string' && (TIME_REVIEW_EVIDENCE_KINDS as readonly string[]).includes(value)
}

// 真实请求白名单：与规格 02 §HTTP 表逐条一致；新增端点必须先改规格与测试，禁止在此临时扩展。
export const TASK_API_PATHS = {
  preview: 'task-previews',
  createTask: 'tasks',
  listTasks: 'tasks',
  taskDetail: (taskId: string) => `tasks/${taskId}`,
  taskChildren: (taskId: string) => `tasks/${taskId}/children`,
  cancelTask: (taskId: string) => `tasks/${taskId}/cancel`,
  timeReview: (taskId: string) => `tasks/${taskId}/time-review`,
  releaseLock: (subTaskId: string) => `subtasks/${subTaskId}/release-lock`,
} as const

export interface TaskRequestContext { csrfToken: string; signal?: AbortSignal }
export interface SubmitTaskParams { previewToken: string; idempotencyKey: string; confirm: true; context: TaskRequestContext }
export interface CancelTaskParams { taskId: string; expectedRevision: string; confirm: true; context: TaskRequestContext }
export interface TimeReviewParams {
  taskId: string; expectedRevision: string; decision: TimeReviewDecision; evidence: string; context: TaskRequestContext
}
export interface ReleaseLockParams {
  subTaskId: string; expectedRevision: string; acknowledgeUnknown: true; reason: string; context: TaskRequestContext
}

function listPath(base: string, cursor: string | null): string {
  return cursor ? `${base}?cursor=${encodeURIComponent(cursor)}` : base
}

export async function createTaskPreview(operation: 'reboot', deviceIds: string[], context: TaskRequestContext): Promise<ApiResponse<TaskPreview>> {
  const body = buildTaskPreviewRequest(operation, deviceIds)
  const res = await post<TaskPreview>(TASK_API_PATHS.preview, body, { csrfToken: context.csrfToken, signal: context.signal })
  assertTaskPreviewShape(res.data)
  return res
}

export async function listTasks(cursor: string | null, context: TaskRequestContext): Promise<ApiResponse<TaskPage<TaskItem>>> {
  const res = await get<TaskPage<TaskItem>>(listPath(TASK_API_PATHS.listTasks, cursor), { signal: context.signal })
  assertTaskPageShape(res.data)
  return res
}

export async function getTask(taskId: string, context: TaskRequestContext): Promise<ApiResponse<TaskItem>> {
  const res = await get<TaskItem>(TASK_API_PATHS.taskDetail(taskId), { signal: context.signal })
  assertTaskItemShape(res.data)
  return res
}

export async function getTaskChildren(taskId: string, cursor: string | null, context: TaskRequestContext): Promise<ApiResponse<TaskPage<SubTaskItem>>> {
  const res = await get<TaskPage<SubTaskItem>>(listPath(TASK_API_PATHS.taskChildren(taskId), cursor), { signal: context.signal })
  assertSubTaskPageShape(res.data)
  return res
}

export async function submitTask(params: SubmitTaskParams): Promise<ApiResponse<{ task_id: string; accepted: boolean }>> {
  const res = await post<{ task_id: string; accepted: boolean }>(
    TASK_API_PATHS.createTask,
    { preview_token: params.previewToken, confirm: params.confirm },
    { csrfToken: params.context.csrfToken, signal: params.context.signal, idempotencyKey: params.idempotencyKey },
  )
  const data = res.data as unknown
  // 02 §模型表：创建响应的 `task_id` 是公共 Id（规范 UUIDv4）。此处复用 Task 3 `task-ops.ts` 导出的
  // `isCanonicalUuidV4` 谓词（同一 `TASK_ID_V4` 正则，不新建 validator、不改正则），
  // `data` 非 object / null、`task_id` 非规范 UUIDv4、`accepted` 非 true 一律同一错误码，不降级映射、不补默认值
  if (typeof data !== 'object' || data === null ||
      !isCanonicalUuidV4((data as { task_id?: unknown }).task_id) ||
      (data as { accepted?: unknown }).accepted !== true) {
    throw new Error('INVALID_TASK_SUBMISSION')
  }
  return res
}

export type CancelTaskResponse = { accepted: boolean; cancelled: string[] | null }

export async function cancelTask(params: CancelTaskParams): Promise<ApiResponse<CancelTaskResponse>> {
  const res = await post<CancelTaskResponse>(
    TASK_API_PATHS.cancelTask(params.taskId),
    { expected_revision: params.expectedRevision, confirm: params.confirm },
    { csrfToken: params.context.csrfToken, signal: params.context.signal },
  )
  // `post` 只校验信封，业务 `data` 在此校验：`data` 按 unknown 处理，非 object / null / array、`accepted !== true`、
  // `cancelled` 既非 null 也非全规范 UUIDv4 数组（`isCanonicalUuidV4`，Task 3 同一正则，本文件已为 submitTask 导入，不重复 import）
  // 一律抛同一 `INVALID_CANCEL_RESPONSE`：不新建 DTO / state / HTTP 映射、不混用 `INVALID_TASK_SUBMISSION`、
  // 不把取消结果提升为可见状态；额外字段忽略，空数组按声明合同 `string[] | null` 放行
  const data: unknown = res.data
  const cancelled = (data as { cancelled?: unknown } | null)?.cancelled
  if (typeof data !== 'object' || data === null || Array.isArray(data) ||
      (data as { accepted?: unknown }).accepted !== true ||
      !(cancelled === null || (Array.isArray(cancelled) && cancelled.every(isCanonicalUuidV4)))) {
    throw new Error('INVALID_CANCEL_RESPONSE')
  }
  return res
}

export async function reviewHeldTime(params: TimeReviewParams): Promise<ApiResponse<TaskItem>> {
  if (!TIME_REVIEW_DECISIONS.includes(params.decision)) throw new Error('INVALID_TIME_REVIEW_DECISION')
  const evidence = params.evidence
  if (!isTimeReviewEvidenceKind(evidence)) throw new Error('INVALID_TIME_REVIEW_EVIDENCE')
  const res = await post<TaskItem>(
    TASK_API_PATHS.timeReview(params.taskId),
    { expected_revision: params.expectedRevision, decision: params.decision, evidence },
    { csrfToken: params.context.csrfToken, signal: params.context.signal },
  )
  assertTaskItemShape(res.data)
  return res
}

export async function releaseUnknownLock(params: ReleaseLockParams): Promise<ApiResponse<ReleaseLockResult>> {
  if (params.acknowledgeUnknown !== true) throw new Error('RELEASE_CONFIRMATION_REQUIRED')
  const reason = params.reason.trim()
  if (reason.length === 0) throw new Error('RELEASE_CONFIRMATION_REQUIRED')
  const res = await post<ReleaseLockResult>(
    TASK_API_PATHS.releaseLock(params.subTaskId),
    { expected_revision: params.expectedRevision, acknowledge_unknown: true, reason },
    { csrfToken: params.context.csrfToken, signal: params.context.signal },
  )
  assertReleaseLockResultShape(res.data)
  return res
}

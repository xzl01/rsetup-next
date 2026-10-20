// 管理端视图测试共用的后端桩件：按 `METHOD /api/v1/...` 精确匹配响应脚本，
// 并记录每一次请求的 URL / method / JSON body 供断言。
import { vi } from 'vitest'

export type StubbedResponse = { status?: number; body: unknown }

export function ok(data: unknown): StubbedResponse {
  return { body: { data, request_id: 'req-1' } }
}

export function fail(status: number, code: string): StubbedResponse {
  return { status, body: { error: { code, message_key: `errors.${code.toLowerCase()}` }, request_id: 'req-err' } }
}

export type RecordedCall = {
  url: string
  method: string
  body: Record<string, unknown> | null
  /** 请求绑定的 AbortSignal（若有），用于断言迟到请求确已被取消。 */
  signal: AbortSignal | null
}

function recordCall(input: RequestInfo | URL, init?: RequestInit): RecordedCall {
  return {
    url: String(input),
    method: String(init?.method ?? 'GET'),
    body: init?.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null,
    signal: init?.signal instanceof AbortSignal ? init.signal : null,
  }
}

function scriptFor(scripts: Record<string, StubbedResponse[]>, call: RecordedCall): StubbedResponse | undefined {
  // 依次尝试：精确 METHOD+URL → 精确 URL → 忽略 query 的 METHOD+路径 → 裸方法。
  // 这样同一端点的分页请求（仅游标不同）会共享同一段响应脚本。
  const base = call.url.split('?')[0] as string
  const queue = scripts[`${call.method} ${call.url}`] ?? scripts[call.url]
    ?? scripts[`${call.method} ${base}`] ?? scripts[call.method]
  return queue && queue.length > 1 ? queue.shift() : queue?.[0]
}

function responseFor(next: StubbedResponse | undefined): Response {
  return new Response(JSON.stringify(next?.body ?? { data: null, request_id: 'req-empty' }), {
    status: next?.status ?? 200,
    headers: { 'Content-Type': 'application/json' },
  })
}

export function stubBackend(scripts: Record<string, StubbedResponse[]>): RecordedCall[] {
  const calls: RecordedCall[] = []
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const call = recordCall(input, init)
    calls.push(call)
    return responseFor(scriptFor(scripts, call))
  }))
  return calls
}

export type PendingBackend = {
  calls: RecordedCall[]
  /** 当前挂起中的请求数。 */
  pending(): number
  /** 按到达顺序放行最早 n 个请求（默认全部），并等待微任务与渲染队列排空。 */
  release(n?: number): Promise<void>
}

/**
 * 所有请求都挂起、由测试显式放行的后端桩件：用于构造“迟到响应”竞态。
 *
 * 桩件不理会 AbortSignal，因此放行被 abort 的请求可以真实复现“撤权后响应才回来”的场景，
 * 用来验证组件是否靠代际门禁把响应丢弃，而不是侥幸依赖 fetch 被取消。
 */
export function stubPendingBackend(scripts: Record<string, StubbedResponse[]>): PendingBackend {
  const calls: RecordedCall[] = []
  const queue: Array<() => void> = []
  vi.stubGlobal('fetch', vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const call = recordCall(input, init)
    calls.push(call)
    return new Promise<Response>((resolve) => {
      queue.push(() => resolve(responseFor(scriptFor(scripts, call))))
    })
  }))
  return {
    calls,
    pending: () => queue.length,
    async release(n = queue.length): Promise<void> {
      const batch = queue.splice(0, Math.max(0, n))
      for (const settle of batch) settle()
      await nextTick()
      await nextTick()
    },
  }
}

export const USER_ID = '01234567-89ab-4def-8f01-234567890abc'
export const USER_ID_B = '01234567-89ab-4def-8f01-234567890abd'
export const ROLE_ID = '01234567-89ab-4def-8f01-234567890abe'
export const GROUP_ID = '01234567-89ab-4def-8f01-234567890abf'
export const GRANT_ID = '11234567-89ab-4def-8f01-234567890abc'
export const DEVICE_A = 'a'.repeat(64)
export const DEVICE_B = 'b'.repeat(64)

export function nextTick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0))
}

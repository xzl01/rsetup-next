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

export type RecordedCall = { url: string; method: string; body: Record<string, unknown> | null }

export function stubBackend(scripts: Record<string, StubbedResponse[]>): RecordedCall[] {
  const calls: RecordedCall[] = []
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input)
    const method = String(init?.method ?? 'GET')
    calls.push({ url, method, body: init?.body ? JSON.parse(String(init.body)) as Record<string, unknown> : null })
    // 依次尝试：精确 METHOD+URL → 精确 URL → 忽略 query 的 METHOD+路径 → 裸方法。
    // 这样同一端点的分页请求（仅游标不同）会共享同一段响应脚本。
    const base = url.split('?')[0] as string
    const queue = scripts[`${method} ${url}`] ?? scripts[url] ?? scripts[`${method} ${base}`] ?? scripts[method]
    const next = queue && queue.length > 1 ? queue.shift() : queue?.[0]
    return new Response(JSON.stringify(next?.body ?? { data: null, request_id: 'req-empty' }), {
      status: next?.status ?? 200,
      headers: { 'Content-Type': 'application/json' },
    })
  }))
  return calls
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

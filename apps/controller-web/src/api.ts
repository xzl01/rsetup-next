import type { ApiErrorFields, ApiErrorParams, ApiResponse } from './types'

const ERROR_KEYS = {
  INVALID_API_PATH: 'errors.invalidPath',
  NETWORK_ERROR: 'errors.network',
  REQUEST_ABORTED: 'errors.aborted',
  INVALID_API_RESPONSE: 'errors.invalidResponse',
} as const

type LocalErrorCode = keyof typeof ERROR_KEYS

/** Stable, non-server-provided message. */
export class ApiError extends Error {
  readonly code: string
  readonly messageKey: string
  readonly status?: number
  readonly requestId?: string
  readonly params?: ApiErrorFields['params']
  readonly retryAfter?: string

  constructor(fields: ApiErrorFields) {
    super('API request failed')
    this.name = 'ApiError'
    this.code = fields.code
    this.messageKey = fields.messageKey
    this.status = fields.status
    this.requestId = fields.requestId
    this.params = fields.params
    this.retryAfter = fields.retryAfter
  }
}

function localError(code: LocalErrorCode, status?: number): ApiError {
  return new ApiError({ code, messageKey: ERROR_KEYS[code], status })
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value) &&
    Object.getPrototypeOf(value) === Object.prototype
}

function hasOwn(value: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(value, key)
}

function isRequestId(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= 256
}

function validParams(value: unknown): value is ApiErrorParams {
  if (!isRecord(value)) return false
  const keys = Object.keys(value)
  if (keys.length > 32) return false
  return keys.every((key) => {
    if (key === '__proto__' || key === 'constructor' || key === 'prototype') return false
    const item = value[key]
    return item === null || typeof item === 'boolean' ||
      (typeof item === 'string' && item.length <= 1024) ||
      (typeof item === 'number' && Number.isFinite(item))
  })
}

function isAborted(error: unknown, signal?: AbortSignal): boolean {
  return signal?.aborted === true ||
    (typeof error === 'object' && error !== null && 'name' in error && error.name === 'AbortError')
}

function safePath(input: string): string {
  if (typeof input !== 'string' || !input || input.endsWith(' ') || /[\\#\u0000-\u001f\u007f-\u009f]/.test(input)) {
    throw localError('INVALID_API_PATH')
  }
  const question = input.indexOf('?')
  const pathname = question < 0 ? input : input.slice(0, question)
  if (!pathname || pathname.startsWith('//')) throw localError('INVALID_API_PATH')

  // Inspect decoded path segments before URL/fetch can normalize encoded separators or dot segments.
  // Repeated decoding also rejects double-encoded escapes such as %252f and %252e%252e.
  let decoded = pathname
  try {
    while (decoded.includes('%')) {
      const next = decodeURIComponent(decoded)
      if (next === decoded) break
      decoded = next
      if (/[\\?#\u0000-\u001f\u007f-\u009f]/.test(decoded)) throw localError('INVALID_API_PATH')
    }
  } catch {
    throw localError('INVALID_API_PATH')
  }
  if (decoded.startsWith('//') || decoded.includes('\\') ||
      decoded.split('/').some((part) => part === '.' || part === '..') ||
      decoded.split('/').length !== pathname.split('/').length ||
      /^[^/]*:/.test(decoded)) {
    throw localError('INVALID_API_PATH')
  }
  const url = `/api/v1/${input.replace(/^\//, '')}`
  // The base is only for parsing; never use it as a request origin.
  try {
    if (!new URL(url, 'https://synthetic.invalid').pathname.startsWith('/api/v1/')) {
      throw localError('INVALID_API_PATH')
    }
  } catch {
    throw localError('INVALID_API_PATH')
  }
  return url
}

export async function get<T>(path: string, options?: { signal?: AbortSignal }): Promise<ApiResponse<T>> {
  const url = safePath(path)
  const signal = options?.signal
  let response: Response
  try {
    response = await fetch(url, {
      method: 'GET', credentials: 'same-origin', redirect: 'error',
      headers: { Accept: 'application/json' }, signal,
    })
  } catch (error) {
    throw localError(isAborted(error, signal) ? 'REQUEST_ABORTED' : 'NETWORK_ERROR')
  }

  const contentType = response.headers.get('Content-Type') ?? ''
  if (!/^application\/(?:json|[a-z0-9.+-]+\+json)(?:\s*;|\s*$)/i.test(contentType)) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  let body: unknown
  try {
    body = await response.json()
  } catch (error) {
    throw localError(isAborted(error, signal) ? 'REQUEST_ABORTED' : 'INVALID_API_RESPONSE', response.status)
  }
  if (!isRecord(body) || !isRequestId(body.request_id)) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  if (response.ok) {
    if (!hasOwn(body, 'data') || hasOwn(body, 'error')) {
      throw localError('INVALID_API_RESPONSE', response.status)
    }
    return { data: body.data as T, requestId: body.request_id }
  }
  if (!isRecord(body.error) || hasOwn(body, 'data')) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  const { code, message_key: messageKey, params } = body.error
  if (typeof code !== 'string' || code.length > 128 || !/^[A-Za-z0-9_.-]+$/.test(code) ||
      typeof messageKey !== 'string' || !messageKey || messageKey.length > 256 ||
      !/^[A-Za-z0-9_.-]+$/.test(messageKey) ||
      (hasOwn(body.error, 'params') && !validParams(params))) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  const retryAfter = response.headers.get('Retry-After')
  throw new ApiError({ code, messageKey, requestId: body.request_id,
    status: response.status, ...(hasOwn(body.error, 'params') ? { params: params as ApiErrorParams } : {}),
    ...(retryAfter !== null && retryAfter.length <= 256 && !/[\u0000-\u001f\u007f-\u009f]/.test(retryAfter)
      ? { retryAfter } : {}),
  })
}

import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, post } from './api'

function jsonResponse(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  })
}

function fakeFetch(response: Response) {
  const fetcher = vi.fn(async (_url: string, _init?: RequestInit): Promise<Response> => response)
  vi.stubGlobal('fetch', fetcher)
  return fetcher
}

afterEach(() => vi.unstubAllGlobals())

describe('authenticated post', () => {
  it('locks transport: relative /api/v1 URL, same-origin credentials, redirect error, JSON body', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: { user: { username: 'admin' }, csrf_token: 'tok-1' }, request_id: 'r1' }))
    const signal = new AbortController().signal
    const result = await post('/auth/login', { username: 'admin', password: 'synthetic-only' },
      { signal, method: 'GET', credentials: 'include', headers: { Authorization: 'bad' }, baseURL: 'https://example.invalid' } as { signal: AbortSignal })
    expect(result).toEqual({ data: { user: { username: 'admin' }, csrf_token: 'tok-1' }, requestId: 'r1' })
    expect(fetcher).toHaveBeenCalledOnce()
    expect(fetcher.mock.calls[0]?.[0]).toBe('/api/v1/auth/login')
    // toMatchObject：实现按现有 get 模式以 `signal` 展开，避免 exact-match 对 undefined own 属性的分歧
    expect(fetcher.mock.calls[0]?.[1]).toMatchObject({
      method: 'POST', credentials: 'same-origin', redirect: 'error',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ username: 'admin', password: 'synthetic-only' }),
    })
    expect((fetcher.mock.calls[0]?.[1] as { signal?: AbortSignal }).signal).toBe(signal)
  })

  it('sends X-CSRF-Token only when a valid token is provided', async () => {
    const fetcher = vi.fn(async (_url: string, _init?: RequestInit): Promise<Response> => jsonResponse({ data: { changed: true }, request_id: 'r2' }))
    vi.stubGlobal('fetch', fetcher)
    await post('/auth/password', { current_password: 'a', new_password: 'b' }, { csrfToken: 'tok-9_-Z' })
    expect(fetcher.mock.calls[0]?.[1]).toMatchObject({
      headers: { 'Content-Type': 'application/json', Accept: 'application/json', 'X-CSRF-Token': 'tok-9_-Z' },
    })
    await post('/auth/logout', {})
    const second = fetcher.mock.calls[1]?.[1]
    expect(second).toMatchObject({
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    })
    expect(JSON.stringify(second)).not.toContain('X-CSRF-Token')
  })

  it.each([null, {}, { message: '中文与 unicode', nested: { ok: 1 } }, [1, 'two']])(
    'serializes any JSON-serializable payload %j', async (payload) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'r' }))
    await post('/auth/logout', payload)
    expect(JSON.parse((fetcher.mock.calls[0]?.[1] as RequestInit).body as string)).toEqual(payload)
  })

  it('rejects an invalid csrfToken before calling fetch', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'r' }))
    await expect(post('/auth/logout', {}, { csrfToken: 'bad\x00token' }))
      .rejects.toMatchObject({ code: 'INVALID_API_PATH', messageKey: 'errors.invalidPath' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it.each([
    'https://example.invalid/a', '//example.invalid', '/../x', '/%2e%2e/x', '/a\\b',
    '/a#frag', '/a\nx', '/%252f/x',
  ])('rejects unsafe path %s before calling fetch', async (path) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'r' }))
    await expect(post(path, {})).rejects.toMatchObject({ code: 'INVALID_API_PATH' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('accepts ordinary api paths', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'ok' }))
    await expect(post('auth/login', {})).resolves.toEqual({ data: true, requestId: 'ok' })
    expect(fetcher.mock.calls[0]?.[0]).toBe('/api/v1/auth/login')
  })

  it('preserves validated structured 401 errors without exposing server extras', async () => {
    const fetcher = fakeFetch(jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: 'r1', message: '<img src=x onerror=alert(1)>' }, 401))
    let caught: unknown
    try { await post('/auth/login', { username: 'a', password: 'b' }) } catch (error) { caught = error }
    expect(caught).toBeInstanceOf(ApiError)
    expect(caught).toMatchObject({ code: 'INVALID_CREDENTIALS', messageKey: 'errors.invalidCredentials', status: 401, requestId: 'r1', message: 'API request failed' })
    expect(fetcher).toHaveBeenCalledOnce()
  })

  it('preserves 429 with Retry-After and 503 NOT_READY for upper-layer retry decisions', async () => {
    fakeFetch(jsonResponse({ error: { code: 'RATE_LIMITED', message_key: 'errors.rateLimited', params: { attempts: 5 } }, request_id: 'r1' }, 429, { 'Retry-After': '30' }))
    await expect(post('/auth/login', { username: 'a', password: 'b' }))
      .rejects.toMatchObject({ code: 'RATE_LIMITED', status: 429, retryAfter: '30', params: { attempts: 5 } })
    fakeFetch(jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: 'r2' }, 503))
    await expect(post('/auth/me', null))
      .rejects.toMatchObject({ code: 'NOT_READY', status: 503, messageKey: 'errors.notReady' })
  })

  it.each([
    { request_id: 'r1' }, { data: false, request_id: '' }, { data: false, request_id: 42 },
    [1, 2], null, 0,
    { data: false, request_id: 'r1', error: { code: 'DENIED', message_key: 'errors.denied' } },
  ])('rejects malformed success envelopes %j', async (body) => {
    fakeFetch(jsonResponse(body))
    await expect(post('/auth/me', null)).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE', messageKey: 'errors.invalidResponse' })
  })

  it('rejects malformed error envelopes without echoing server message or raw body', async () => {
    fakeFetch(new Response(JSON.stringify({ error: { code: 'BAD CODE', message_key: 'errors.x' }, request_id: 'r' }), { status: 400, headers: { 'Content-Type': 'application/json' } }))
    const err = await post('/auth/login', { username: 'a', password: 'b' }).catch((e: unknown) => e) as ApiError
    expect(err).toMatchObject({ code: 'INVALID_API_RESPONSE', message: 'API request failed' })
    expect(JSON.stringify(err)).not.toContain('BAD CODE')
  })

  it('does not trust non-JSON content types', async () => {
    fakeFetch(new Response('<script>bad</script>', { status: 401, headers: { 'Content-Type': 'text/html' } }))
    await expect(post('/auth/login', { username: 'a', password: 'b' })).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE' })
  })

  it('classifies network errors and aborts without exposing their messages', async () => {
    const fetcher = vi.fn().mockRejectedValue(new TypeError('secret network data'))
    vi.stubGlobal('fetch', fetcher)
    await expect(post('/auth/login', { username: 'a', password: 'b' }))
      .rejects.toMatchObject({ code: 'NETWORK_ERROR', messageKey: 'errors.network', message: 'API request failed' })
    expect(fetcher).toHaveBeenCalledOnce()
    const controller = new AbortController()
    controller.abort()
    vi.stubGlobal('fetch', vi.fn(async () => { throw new DOMException('aborted', 'AbortError') }))
    await expect(post('/auth/login', { username: 'a', password: 'b' }, { signal: controller.signal }))
      .rejects.toMatchObject({ code: 'REQUEST_ABORTED', messageKey: 'errors.aborted' })
  })
})

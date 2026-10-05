import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, get } from './api'
import type { DecimalString } from './types'

function jsonResponse(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  })
}

function fakeFetch(response: Response) {
  const fetcher = vi.fn(async (_input: RequestInfo | URL, _init?: RequestInit): Promise<Response> => response)
  vi.stubGlobal('fetch', fetcher)
  return fetcher
}

afterEach(() => vi.unstubAllGlobals())

describe('read-only get', () => {
  it('unpacks data without converting a maximum-u64 decimal string', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: { revision: '18446744073709551615' }, request_id: 'r1' }))
    const result = await get<{ revision: DecimalString }>('/devices?limit=20&name=A%20B')
    expect(result).toEqual({ data: { revision: '18446744073709551615' }, requestId: 'r1' })
    expect(fetcher).toHaveBeenCalledOnce()
    expect(fetcher.mock.calls[0]?.[0]).toBe('/api/v1/devices?limit=20&name=A%20B')
  })

  it('fixes transport options and forwards AbortSignal unchanged', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: null, request_id: 'r1' }))
    const signal = new AbortController().signal
    await get('/devices', { signal, method: 'POST', credentials: 'include', headers: { Authorization: 'bad' }, baseURL: 'https://example.invalid' } as { signal: AbortSignal })
    expect(fetcher).toHaveBeenCalledExactlyOnceWith('/api/v1/devices', {
      method: 'GET', credentials: 'same-origin', redirect: 'error',
      headers: { Accept: 'application/json' }, signal,
    })
  })

  it.each([
    'https://example.invalid/a', 'http://example.invalid/a', '//example.invalid',
    '/../other', '/./other', '/%2e%2e/other', '/.%2e/other',
    '/%2F%2E%2E%2Fother', '/%2fother', '/a%5cb', '/a\\b',
    '/a#fragment', '/a\nextra', '/a\u007fextra', '/a\u0085extra', '/a%00b', '/bad%ZZ',
    '/a/%252f/b', '/a/%252e%252e/b', '/%3fescape', '/%23escape',
    'https%3a%2f%2fexample.invalid/',
  ])('rejects unsafe path %s before calling fetch', async (path) => {
    const fetcher = fakeFetch(jsonResponse({ data: 1, request_id: 'r1' }))
    await expect(get(path)).rejects.toMatchObject({ code: 'INVALID_API_PATH', messageKey: 'errors.invalidPath' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it.each(['/.. ', '/%2e%2e '])('rejects a trailing-space dot segment %s before URL normalization can escape the prefix', async (path) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'r1' }))
    expect(new URL(`/api/v1/${path.slice(1)}`, 'https://synthetic.invalid').pathname).toBe('/api/')
    await expect(get(path)).rejects.toMatchObject({ code: 'INVALID_API_PATH', messageKey: 'errors.invalidPath' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('rejects a trailing query space instead of silently stripping or forwarding it', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'r1' }))
    await expect(get('/devices?term=hello ')).rejects.toMatchObject({ code: 'INVALID_API_PATH' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it.each(['/../.. ', '/%2e%2e/other', '/a%2fb'])('keeps existing path escape rejection %s', async (path) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'r1' }))
    await expect(get(path)).rejects.toMatchObject({ code: 'INVALID_API_PATH' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it.each(['/devices?term=A%20B&sort=asc', '/devices?term=hello world&sort=asc'])('keeps safe query text unchanged %s', async (path) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'r1' }))
    expect(await get(path)).toEqual({ data: true, requestId: 'r1' })
    expect(fetcher.mock.calls[0]?.[0]).toBe(`/api/v1${path}`)
    expect(new URL(fetcher.mock.calls[0]![0] as string, 'https://synthetic.invalid').pathname).toBe('/api/v1/devices')
  })

  it.each(['/devices', 'devices', '/devices/123', '/devices?term=a%2Fb&sort=asc'])('accepts ordinary paths and queries %s', async (path) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'ok' }))
    expect(await get(path)).toEqual({ data: true, requestId: 'ok' })
    expect(fetcher).toHaveBeenCalledOnce()
  })

  it.each([
    { request_id: 'r1' }, { data: false, request_id: '' }, { data: false, request_id: 42 },
    { data: false, request_id: 'x'.repeat(257) }, [1, 2], null, 0,
    { data: false, request_id: 'r1', error: { code: 'DENIED', message_key: 'errors.denied' } },
  ])('rejects malformed success envelopes', async (body) => {
    fakeFetch(jsonResponse(body))
    await expect(get('/devices')).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE', messageKey: 'errors.invalidResponse' })
  })

  it.each([
    new Response('<script>bad</script>', { status: 200, headers: { 'Content-Type': 'text/html' } }),
    new Response('{broken', { status: 200, headers: { 'Content-Type': 'application/json' } }),
    new Response(JSON.stringify({ data: 1, request_id: 'r1' }), { status: 200, headers: { 'Content-Type': 'text/plain' } }),
  ])('does not trust non-JSON content or malformed JSON', async (response) => {
    fakeFetch(response)
    await expect(get('/devices')).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE' })
  })

  it.each([401, 403, 404, 409, 429])('preserves validated structured errors on HTTP %s without retry', async (status) => {
    const fetcher = fakeFetch(jsonResponse({ error: { code: 'RATE_LIMITED', message_key: 'errors.rateLimited', params: { attempts: 3, hint: 'later', optional: null, enabled: true } }, request_id: 'r1', message: '<img src=x onerror=alert(1)>' }, status, { 'Retry-After': '120' }))
    let caught: unknown
    try { await get('/devices') } catch (error) { caught = error }
    expect(caught).toBeInstanceOf(ApiError)
    expect(caught).toMatchObject({
      code: 'RATE_LIMITED', messageKey: 'errors.rateLimited', requestId: 'r1',
      status, retryAfter: '120', params: { attempts: 3, hint: 'later', optional: null, enabled: true },
      message: 'API request failed',
    })
    expect(fetcher).toHaveBeenCalledOnce()
  })

  it.each([
    { error: { code: 'BAD CODE', message_key: 'errors.bad' }, request_id: 'r1' },
    { error: { code: 'A'.repeat(129), message_key: 'errors.bad' }, request_id: 'r1' },
    { error: { code: 'OK', message_key: 'x'.repeat(257) }, request_id: 'r1' },
    { error: { code: 'OK', message_key: 'errors.bad' }, request_id: '' },
    { error: { code: 'OK', message_key: 'errors.bad', params: { x: [] } }, request_id: 'r1' },
    { error: { code: 'OK', message_key: 'errors.bad', params: { x: 'x'.repeat(1025) } }, request_id: 'r1' },
    { error: { code: 'OK', message_key: 'errors.bad', params: Object.fromEntries(Array.from({ length: 33 }, (_, index) => [`key${index}`, index])) }, request_id: 'r1' },
    { error: { code: 'OK', message_key: 'errors.bad', params: JSON.parse('{"__proto__":"bad"}') }, request_id: 'r1' },
    { error: { code: 'OK', message_key: 'errors.bad', params: { constructor: 'bad' } }, request_id: 'r1' },
    { error: { code: 'OK', message_key: '<img src=x onerror=alert(1)>' }, request_id: 'r1' },
    { data: { trusted: false }, request_id: 'r1' },
  ])('rejects malformed error envelopes without exposing server message', async (body) => {
    fakeFetch(jsonResponse(body, 400))
    await expect(get('/devices')).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE', message: 'API request failed' })
  })

  it('classifies network errors without retrying or exposing their messages', async () => {
    const fetcher = vi.fn().mockRejectedValue(new TypeError('secret network data'))
    vi.stubGlobal('fetch', fetcher)
    await expect(get('/devices')).rejects.toMatchObject({ code: 'NETWORK_ERROR', messageKey: 'errors.network', message: 'API request failed' })
    expect(fetcher).toHaveBeenCalledOnce()
  })

  it('classifies signal cancellation independently', async () => {
    const controller = new AbortController()
    const fetcher = vi.fn().mockImplementation(async (_url: string, options: RequestInit) => {
      expect(options.signal).toBe(controller.signal)
      controller.abort()
      throw new DOMException('The request was aborted', 'AbortError')
    })
    vi.stubGlobal('fetch', fetcher)
    await expect(get('/devices', { signal: controller.signal })).rejects.toMatchObject({ code: 'REQUEST_ABORTED', messageKey: 'errors.aborted' })
    expect(fetcher).toHaveBeenCalledOnce()
  })
})

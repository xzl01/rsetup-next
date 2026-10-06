import { afterEach, describe, expect, it, vi } from 'vitest'
import { createAuth, authErrorKey } from './auth'

const ME_DATA = {
  user: { username: 'admin', display_name: 'Sam', must_change_password: false, is_admin: true, active: true, revision: '1' },
  csrf_token: 'mem-only-token',
  authz_epoch: '7',
}

function jsonResponse(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...headers } })
}

function fakeFetch(handler: (url: string, init?: RequestInit) => Response | Promise<Response>) {
  const fetcher = vi.fn(handler as unknown as typeof fetch)
  vi.stubGlobal('fetch', fetcher)
  return fetcher
}

function localStorageJoined(): string {
  return Object.keys(localStorage).map((k) => k + localStorage.getItem(k)).join(';')
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

describe('auth store state machine', () => {
  it('starts checking with nulls and issues no request until refresh', () => {
    const fetcher = vi.fn()
    vi.stubGlobal('fetch', fetcher)
    const store = createAuth()
    expect(store.status.value).toBe('checking')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    expect(store.errorCode.value).toBeNull()
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('refresh: 200 me → signed_in with in-memory csrf and epoch, nothing written to localStorage', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data: ME_DATA, request_id: 'r1' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('signed_in')
    expect(store.user.value?.username).toBe('admin')
    expect(store.csrfToken.value).toBe('mem-only-token')
    expect(store.authzEpoch.value).toBe('7')
    expect(localStorageJoined()).not.toContain('mem-only-token')
  })

  it('refresh: me with must_change_password=true → force_password', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data: { ...ME_DATA, user: { ...ME_DATA.user, must_change_password: true } }, request_id: 'r1' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('force_password')
  })

  it('refresh: only 401 maps to signed_out; errorCode cleared', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r1' }, 401)
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.errorCode.value).toBeNull()
  })

  it('refresh: network failure → error, not signed_out', async () => {
    fakeFetch(() => { throw new TypeError('offline') })
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
  })

  it('refresh: 503 NOT_READY and 429 → error with mapped code (upper layer may retry)', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: 'r' }, 503)
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('OTHER') // NOT_READY 未列入专属映射 → 安全通用文案
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ error: { code: 'RATE_LIMITED', message_key: 'errors.rateLimited' }, request_id: 'r' }, 429, { 'Retry-After': '30' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('RATE_LIMITED')
  })

  it('login: success stores in-memory csrf, no X-CSRF-Token sent, no localStorage write', async () => {
    const fetcher = fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ data: { user: { username: 'admin', must_change_password: false }, csrf_token: 'tok-2' }, request_id: 'r' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await expect(store.login('admin', 'synthetic-only')).resolves.toBe(true)
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('tok-2')
    const init = fetcher.mock.calls[0]?.[1] as { headers: Record<string, string>; body: string; method: string }
    expect(init.method).toBe('POST')
    expect(JSON.stringify(init.headers)).not.toContain('X-CSRF-Token')
    expect(JSON.parse(init.body)).toEqual({ username: 'admin', password: 'synthetic-only' })
    expect(localStorageJoined()).not.toContain('tok-2')
  })

  it('login: success with must_change_password=true → force_password (no device data surface)', async () => {
    fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ data: { user: { username: 'admin', must_change_password: true }, csrf_token: 'tok-3' }, request_id: 'r' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.login('admin', 'synthetic-only')
    expect(store.status.value).toBe('force_password')
  })

  it.each([
    ['INVALID_CREDENTIALS', 401, 'signed_out'],
    ['AUTH_REQUIRED', 401, 'signed_out'],
    ['RATE_LIMITED', 429, 'error'],
  ])('login: %s → status %s with mapped errorCode', async (code, status, expected) => {
    fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ error: { code, message_key: 'errors.x' }, request_id: 'r' }, status)
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await expect(store.login('admin', 'wrong')).resolves.toBe(false)
    expect(store.status.value).toBe(expected)
    expect(store.errorCode.value).toBe(code)
  })

  it('login: 503/network → error, never signed_out', async () => {
    fakeFetch(() => { throw new TypeError('offline') })
    const store = createAuth()
    await expect(store.login('admin', 'x')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
  })

  // I-A: a login 401 must clear the stale authzEpoch left by a prior successful me, together with
  // user/csrf — every other signed_out path already clears the epoch.
  it('login: 401 after a successful me clears stale authzEpoch (epoch 7 → null) and ends signed_out', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
      if (url.includes('/auth/login')) {
        return jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: 'r' }, 401)
      }
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('signed_in')
    expect(store.authzEpoch.value).toBe('7')
    await expect(store.login('admin', 'wrong')).resolves.toBe(false)
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    expect(store.errorCode.value).toBe('INVALID_CREDENTIALS')
  })

  it('changePassword: success clears csrf, refreshes via me with the new session, returns true', async () => {
    let phase = 0
    fakeFetch((url, init) => {
      if (url.includes('/auth/me')) {
        phase += 1
        return phase === 1
          ? jsonResponse({ data: ME_DATA, request_id: 'r1' })
          : jsonResponse({ data: { ...ME_DATA, csrf_token: 'new-token' }, request_id: 'r2' })
      }
      if (url.includes('/auth/password')) {
        const initHeaders = (init?.headers ?? {}) as Record<string, string>
        expect(initHeaders['X-CSRF-Token']).toBe('mem-only-token')
        return jsonResponse({ data: { changed: true }, request_id: 'r3' })
      }
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(true)
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('new-token')
    expect(store.user.value?.must_change_password).toBe(false)
  })

  // M-A: per the backend contract a wrong old password is 400 INVALID_ARGUMENT (service verifies the
  // current password before any commit) — NOT 401. Definitive 4xx keeps prior state without calling me.
  it('changePassword: 400 INVALID_ARGUMENT (wrong old password) keeps prior signed_in/force_password state and sets errorCode, no me call', async () => {
    let meCalls = 0
    fakeFetch((url) => {
      if (url.includes('/auth/me')) { meCalls += 1; return jsonResponse({ data: ME_DATA, request_id: 'r1' }) }
      if (url.includes('/auth/password')) {
        return jsonResponse({ error: { code: 'INVALID_ARGUMENT', message_key: 'errors.invalidArgument' }, request_id: 'r' }, 400)
      }
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('signed_in')
    expect(store.errorCode.value).toBe('OTHER') // INVALID_ARGUMENT 未列入专属映射 → 安全通用文案
    expect(store.csrfToken.value).toBe('mem-only-token')
    expect(store.authzEpoch.value).toBe('7') // prior state fully preserved
    expect(meCalls).toBe(1)
  })

  it('changePassword: 401 AUTH_REQUIRED (session itself revoked) → signed_out, secrets cleared, errorCode null', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
      if (url.includes('/auth/password')) {
        return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r' }, 401)
      }
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    expect(store.errorCode.value).toBeNull()
  })

  // I-B: /auth/password contract — every 4xx short-circuits before commit, and the only 401 this endpoint
  // can emit is a dead session (wrong old password is 400 INVALID_ARGUMENT). Defense in depth: ANY
  // validated 401, whatever its code, means session-invalidated → signed_out; it must never fall into the
  // "definitive 4xx keep-state" branch (that would leave a phantom signed_in).
  it('changePassword: any legit 401 (e.g. INVALID_CREDENTIALS) on /auth/password → signed_out, secrets cleared, no me call', async () => {
    let meCalls = 0
    fakeFetch((url) => {
      if (url.includes('/auth/me')) { meCalls += 1; return jsonResponse({ data: ME_DATA, request_id: 'r1' }) }
      if (url.includes('/auth/password')) {
        return jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: 'r' }, 401)
      }
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    expect(store.errorCode.value).toBeNull()
    expect(meCalls).toBe(1)
  })

  it('logout: posts empty JSON with CSRF when present, clears all in-memory state, ends signed_out', async () => {
    const fetcher = fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
      if (url.includes('/auth/logout')) return jsonResponse({ data: { logged_out: true }, request_id: 'r' })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await store.logout()
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    const init = fetcher.mock.calls[1]?.[1] as { headers: Record<string, string>; body: string }
    expect(init.headers['X-CSRF-Token']).toBe('mem-only-token')
    expect(JSON.parse(init.body)).toEqual({})
  })

  // ---- Required risk tests from the independent rereview (M1–M7) ----

  // M1: login success user missing / string must_change_password → error INVALID_API_RESPONSE, never signed_in.
  it.each([
    ['missing', { user: { username: 'admin' }, csrf_token: 't' }],
    ['string', { user: { username: 'admin', must_change_password: 'true' }, csrf_token: 't' }],
  ])('login: must_change_password %s → error INVALID_API_RESPONSE, never signed_in', async (_label, data) => {
    fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ data, request_id: 'r' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await expect(store.login('admin', 'synthetic-only')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
  })

  // M2: me authz_epoch missing or non-decimal → error INVALID_API_RESPONSE, never signed_in.
  it.each([
    ['non-decimal', { ...ME_DATA, authz_epoch: '1a' }],
    ['missing', { user: ME_DATA.user, csrf_token: ME_DATA.csrf_token }],
  ])('refresh: me authz_epoch %s → error INVALID_API_RESPONSE, never signed_in', async (_label, data) => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data, request_id: 'r1' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
  })

  // M-C: me user missing username (or empty) → error INVALID_API_RESPONSE, never signed_in.
  it.each([
    ['missing', { user: { display_name: 'Sam', must_change_password: false }, csrf_token: 't', authz_epoch: '1' }],
    ['empty', { user: { username: '', must_change_password: false }, csrf_token: 't', authz_epoch: '1' }],
  ])('refresh: me username %s → error INVALID_API_RESPONSE, never signed_in', async (_label, data) => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data, request_id: 'r1' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
  })

  // M4: logout 200 non-JSON / bad envelope → error INVALID_API_RESPONSE, not signed_out.
  it('logout: POST 200 non-JSON → error INVALID_API_RESPONSE, not signed_out', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
      if (url.includes('/auth/logout')) return new Response('<html>not json</html>', { status: 200, headers: { 'Content-Type': 'text/html' } })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.logout()).resolves.toBeUndefined()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.csrfToken.value).toBeNull()
  })

  // M5: changePassword 200 but changed missing → error INVALID_API_RESPONSE, clear user+csrf, no phantom signed_in.
  it('changePassword: 200 but changed missing → error INVALID_API_RESPONSE, clears user+csrf, never phantom signed_in', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
      if (url.includes('/auth/password')) return jsonResponse({ data: {}, request_id: 'r3' })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
  })

  // M-B: change confirmed (200 changed:true) but the follow-up me gets a 401 → signed_out, and
  // changePassword still returns true — the change itself succeeded; only the session is gone.
  it('changePassword: 200 changed:true then internal me 401 → signed_out, returns true', async () => {
    let meCalls = 0
    fakeFetch((url) => {
      if (url.includes('/auth/me')) {
        meCalls += 1
        return meCalls === 1
          ? jsonResponse({ data: ME_DATA, request_id: 'r1' })
          : jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r2' }, 401)
      }
      if (url.includes('/auth/password')) return jsonResponse({ data: { changed: true }, request_id: 'r3' })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(true)
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    expect(meCalls).toBe(2)
  })

  // I1 baseline (plan): refresh me missing must_change_password.
  it('refresh: me missing must_change_password (not a boolean) → error INVALID_API_RESPONSE, never signed_in/force_password', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data: { user: { username: 'admin' }, csrf_token: 't', authz_epoch: '1' }, request_id: 'r1' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
  })

  // I1 baseline (plan): login success response missing csrf_token.
  it('login: success response missing csrf_token → error INVALID_API_RESPONSE, no usable signed_in', async () => {
    fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ data: { user: { username: 'admin', must_change_password: false } }, request_id: 'r' })
      : jsonResponse({ data: null, request_id: 'r0' }))
    const store = createAuth()
    await expect(store.login('admin', 'synthetic-only')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.csrfToken.value).toBeNull()
    expect(store.user.value).toBeNull()
  })

  // I3 baseline (plan): changePassword network failure clears secrets, never restores prior.
  it('changePassword: network failure (server may have executed) → clears secrets + error, never restores priorStatus or old csrf', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
      if (url.includes('/auth/password')) throw new TypeError('offline')
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
  })

  // I2 baseline (plan): logout network failure → error NETWORK_ERROR, not signed_out.
  it('logout: POST network failure → error NETWORK_ERROR, not signed_out (server revocation unconfirmed)', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
      if (url.includes('/auth/logout')) throw new TypeError('offline')
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.logout()).resolves.toBeUndefined()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
    expect(store.csrfToken.value).toBeNull()
  })

  // I4: stale me responses (401 or 200 with old csrf) after a completed login must not overwrite the new state.
  it('race: stale me responses (401 or 200 with old csrf) after a completed login must not overwrite the new state', async () => {
    let resolveA!: (r: Response) => void
    const gateA = new Promise<Response>((r) => { resolveA = r })
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return gateA
      if (url.includes('/auth/login')) return jsonResponse({ data: { user: { username: 'admin', must_change_password: false }, csrf_token: 'tok-new' }, request_id: 'r' })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    const refreshing = store.refresh()
    await store.login('admin', 'synthetic-only')
    resolveA(jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r' }, 401))
    await refreshing
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('tok-new')

    let resolveB!: (r: Response) => void
    const gateB = new Promise<Response>((r) => { resolveB = r })
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return gateB
      if (url.includes('/auth/login')) return jsonResponse({ data: { user: { username: 'admin', must_change_password: false }, csrf_token: 'tok-2' }, request_id: 'r' })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store2 = createAuth()
    const refreshing2 = store2.refresh()
    await store2.login('admin', 'synthetic-only')
    resolveB(jsonResponse({ data: { user: { username: 'admin', must_change_password: false }, csrf_token: 'stale-csrf', authz_epoch: '3' }, request_id: 'r' }))
    await refreshing2
    expect(store2.status.value).toBe('signed_in')
    expect(store2.csrfToken.value).toBe('tok-2')
  })

  // M7: a held logout that resolves (confirmed revocation) after a newer login must not overwrite the new login.
  it('race: a held logout that resolves after a newer login must not overwrite the new login', async () => {
    let resolveLogout!: (r: Response) => void
    const gateLogout = new Promise<Response>((r) => { resolveLogout = r })
    fakeFetch((url) => {
      if (url.includes('/auth/logout')) return gateLogout
      if (url.includes('/auth/login')) return jsonResponse({ data: { user: { username: 'admin', must_change_password: false }, csrf_token: 'tok-after' }, request_id: 'r' })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    const loggingOut = store.logout()
    await expect(store.login('admin', 'synthetic-only')).resolves.toBe(true)
    resolveLogout(jsonResponse({ data: { logged_out: true }, request_id: 'r' }))
    await expect(loggingOut).resolves.toBeUndefined()
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('tok-after')
  })

  // M7 variant: a held logout that fails after a newer login must not downgrade to signed_out/error.
  it('race: a held logout that fails after a newer login must not downgrade to signed_out/error', async () => {
    let settleLogout!: (e: unknown) => void
    const gateLogout = new Promise<Response>((_resolve, reject) => { settleLogout = reject })
    fakeFetch((url) => {
      if (url.includes('/auth/logout')) return gateLogout
      if (url.includes('/auth/login')) return jsonResponse({ data: { user: { username: 'admin', must_change_password: false }, csrf_token: 'tok-after' }, request_id: 'r' })
      return jsonResponse({ data: null, request_id: 'r0' })
    })
    const store = createAuth()
    const loggingOut = store.logout()
    await expect(store.login('admin', 'synthetic-only')).resolves.toBe(true)
    settleLogout(new TypeError('offline'))
    await expect(loggingOut).resolves.toBeUndefined()
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('tok-after')
  })

  it('authErrorKey maps known codes and falls back to the generic safe key', () => {
    expect(authErrorKey('INVALID_CREDENTIALS')).toBe('auth.error.invalidCredentials')
    expect(authErrorKey('AUTH_REQUIRED')).toBe('auth.error.authRequired')
    expect(authErrorKey('RATE_LIMITED')).toBe('auth.error.rateLimited')
    expect(authErrorKey('PASSWORD_CHANGE_REQUIRED')).toBe('auth.error.passwordChangeRequired')
    expect(authErrorKey('PERMISSION_DENIED')).toBe('auth.error.denied')
    expect(authErrorKey('SOMETHING_NEW')).toBe('errors.generic')
  })

  describe('session management (Task 5)', () => {
    const VALID_ID_1 = 'a'.repeat(64)
    const VALID_ID_2 = 'b'.repeat(64)
    const SESSIONS_PAGE_1 = {
      items: [
        { id: VALID_ID_1, current: true, created_time: '2026-10-05T00:00:00Z' },
        { id: VALID_ID_2, current: false, created_time: '2026-10-05T01:00:00Z' },
      ],
      next_cursor: 'c'.repeat(132),
    }

    it('listSessions: sends GET /auth/sessions?limit=50 with credentials same-origin, updates sessions and next_cursor without reading localStorage/cookies', async () => {
      const fetcher = fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r2' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      expect(store.status.value).toBe('signed_in')

      const ok = await store.listSessions()
      expect(ok).toBe(true)
      expect(store.sessions.value).toEqual(SESSIONS_PAGE_1.items)
      expect(store.sessionsNextCursor.value).toBe(SESSIONS_PAGE_1.next_cursor)
      expect(store.sessionsError.value).toBeNull()

      expect(fetcher).toHaveBeenCalledWith(
        '/api/v1/auth/sessions?limit=50',
        expect.objectContaining({ method: 'GET', credentials: 'same-origin' }),
      )
      expect(localStorageJoined()).not.toContain(VALID_ID_1)
    })

    it('listSessions with cursor: safely encodes cursor into query and appends next page items', async () => {
      const PAGE_2 = {
        items: [
          { id: 'd'.repeat(64), current: false, created_time: '2026-10-05T02:00:00Z' },
        ],
        next_cursor: null,
      }
      const fetcher = fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions?limit=50&cursor=')) return jsonResponse({ data: PAGE_2, request_id: 'r3' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r2' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.sessions.value.length).toBe(2)

      const ok = await store.listSessions(store.sessionsNextCursor.value)
      expect(ok).toBe(true)
      expect(store.sessions.value.length).toBe(3)
      expect(store.sessions.value[2].id).toBe('d'.repeat(64))
      expect(store.sessionsNextCursor.value).toBeNull()
      expect(fetcher).toHaveBeenCalledWith(
        expect.stringContaining(`/api/v1/auth/sessions?limit=50&cursor=${SESSIONS_PAGE_1.next_cursor}`),
        expect.anything(),
      )
    })

    it('listSessions: rejects invalid session items shape (missing id / non-boolean current / non-string created_time)', async () => {
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions')) return jsonResponse({ data: { items: [{ id: 123, current: 'true' }], next_cursor: null }, request_id: 'r' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      const ok = await store.listSessions()
      expect(ok).toBe(false)
      expect(store.sessionsError.value).toBe('INVALID_API_RESPONSE')
      expect(store.sessions.value).toEqual([])
    })

    it('revokeSession: revoking another session sends POST /auth/sessions/{id}/revoke with memory CSRF and empty JSON {}, stays signed_in and refreshes session list', async () => {
      let listCalled = 0
      const fetcher = fakeFetch((url, init) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions?limit=50')) {
          listCalled++
          return jsonResponse({ data: { items: [{ id: VALID_ID_1, current: true, created_time: '2026-10-05T00:00:00Z' }], next_cursor: null }, request_id: `r_list_${listCalled}` })
        }
        if (url.includes(`/auth/sessions/${VALID_ID_2}/revoke`)) {
          const headers = (init?.headers ?? {}) as Record<string, string>
          expect(headers['X-CSRF-Token']).toBe('mem-only-token')
          expect(JSON.parse(init?.body as string)).toEqual({})
          return jsonResponse({ data: { revoked: true }, request_id: 'r_rev' })
        }
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.status.value).toBe('signed_in')

      const ok = await store.revokeSession(VALID_ID_2)
      expect(ok).toBe(true)
      expect(store.status.value).toBe('signed_in')
      expect(store.csrfToken.value).toBe('mem-only-token')
      expect(listCalled).toBe(2)
      expect(fetcher).toHaveBeenCalledWith(
        `/api/v1/auth/sessions/${VALID_ID_2}/revoke`,
        expect.objectContaining({ method: 'POST', credentials: 'same-origin' }),
      )
    })

    it('revokeSession: revoking current session (id matching current) succeeds, clears in-memory state and transitions to signed_out', async () => {
      fakeFetch((url, init) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r2' })
        if (url.includes(`/auth/sessions/${VALID_ID_1}/revoke`)) {
          const headers = (init?.headers ?? {}) as Record<string, string>
          expect(headers['X-CSRF-Token']).toBe('mem-only-token')
          return jsonResponse({ data: { revoked: true }, request_id: 'r_rev_self' })
        }
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      await store.listSessions()

      const ok = await store.revokeSession(VALID_ID_1)
      expect(ok).toBe(true)
      expect(store.status.value).toBe('signed_out')
      expect(store.user.value).toBeNull()
      expect(store.csrfToken.value).toBeNull()
      expect(store.authzEpoch.value).toBeNull()
      expect(store.sessions.value).toEqual([])
      expect(store.sessionsLoading.value).toBe(false)
    })

    it('revokeOthers: sends POST /auth/sessions/revoke-others with memory CSRF and empty JSON {}, stays signed_in and refreshes session list', async () => {
      let listCalled = 0
      const fetcher = fakeFetch((url, init) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions?limit=50')) {
          listCalled++
          return jsonResponse({ data: { items: [{ id: VALID_ID_1, current: true, created_time: '2026-10-05T00:00:00Z' }], next_cursor: null }, request_id: `r_list_${listCalled}` })
        }
        if (url.includes('/auth/sessions/revoke-others')) {
          const headers = (init?.headers ?? {}) as Record<string, string>
          expect(headers['X-CSRF-Token']).toBe('mem-only-token')
          expect(JSON.parse(init?.body as string)).toEqual({})
          return jsonResponse({ data: { revoked_count: 3 }, request_id: 'r_rev_others' })
        }
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      await store.listSessions()

      const ok = await store.revokeOthers()
      expect(ok).toBe(true)
      expect(store.status.value).toBe('signed_in')
      expect(store.csrfToken.value).toBe('mem-only-token')
      expect(listCalled).toBe(2)
      expect(fetcher).toHaveBeenCalledWith(
        '/api/v1/auth/sessions/revoke-others',
        expect.objectContaining({ method: 'POST', credentials: 'same-origin' }),
      )
    })

    it('revokeSession: 401 response clears local identity and transitions to signed_out', async () => {
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r2' })
        if (url.includes('/auth/sessions/')) return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r_err' }, 401)
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      await store.listSessions()

      const ok = await store.revokeSession(VALID_ID_2)
      expect(ok).toBe(false)
      expect(store.status.value).toBe('signed_out')
      expect(store.user.value).toBeNull()
      expect(store.csrfToken.value).toBeNull()
      expect(store.sessions.value).toEqual([])
    })

    it('revokeSession: 503 / network error does NOT claim success, keeps signed_in, sets sessionsError, does not clear current session', async () => {
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r1' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r2' })
        if (url.includes('/auth/sessions/')) throw new TypeError('offline')
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store = createAuth()
      await store.refresh()
      await store.listSessions()

      const ok = await store.revokeSession(VALID_ID_2)
      expect(ok).toBe(false)
      expect(store.status.value).toBe('signed_in')
      expect(store.csrfToken.value).toBe('mem-only-token')
      expect(store.sessionsError.value).toBe('NETWORK_ERROR')
      expect(store.sessions.value.length).toBe(2)
    })

    // C-1: revokeSession 当前会话成功必须 ++gen，旧 in-flight refresh 不能复活 signed_in
    it('C-1 race: revoke current session increments gen so a slow in-flight refresh cannot resurrect signed_in', async () => {
      let resolveSlowMe!: (r: Response) => void
      const slowMeGate = new Promise<Response>((r) => { resolveSlowMe = r })
      let meCalls = 0
      fakeFetch((url) => {
        if (url.includes('/auth/me')) {
          meCalls++
          if (meCalls === 1) return jsonResponse({ data: ME_DATA, request_id: 'r_me_1' })
          return slowMeGate
        }
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes(`/auth/sessions/${VALID_ID_1}/revoke`)) return jsonResponse({ data: { revoked: true }, request_id: 'r_rev' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh() // meCalls = 1
      await store.listSessions()
      expect(store.status.value).toBe('signed_in')

      // Trigger background refresh (slow in-flight)
      const pendingRefresh = store.refresh() // meCalls = 2, held by slowMeGate

      // Now revoke current session
      const ok = await store.revokeSession(VALID_ID_1)
      expect(ok).toBe(true)
      expect(store.status.value).toBe('signed_out')
      expect(store.csrfToken.value).toBeNull()

      // The slow refresh resolves with 200 OK ME_DATA
      resolveSlowMe(jsonResponse({ data: ME_DATA, request_id: 'r_me_2' }))
      await pendingRefresh

      // Must STAY signed_out, not resurrected to signed_in!
      expect(store.status.value).toBe('signed_out')
      expect(store.csrfToken.value).toBeNull()
      expect(store.user.value).toBeNull()
    })

    // C-2: targetIsCurrent 未知自注销 (session not in loaded sessions list, e.g. pagination or direct call)
    // 成功后必须以 me() reconcile 身份；若 me() 失败/网络错误，status 绝不得保持 signed_in (fail-closed to error)
    it('C-2: revoking an unknown session (not in sessions list) that succeeds must reconcile via me(); if me() fails with 503/network it must become error, never keep signed_in', async () => {
      const UNKNOWN_SESSION_ID = 'f'.repeat(64)
      let meCalls = 0
      fakeFetch((url) => {
        if (url.includes('/auth/me')) {
          meCalls++
          if (meCalls === 1) return jsonResponse({ data: ME_DATA, request_id: 'r_me_1' })
          // Second me() call after unknown session revocation: fails with 503
          return jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: 'r_me_err' }, 503)
        }
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes(`/auth/sessions/${UNKNOWN_SESSION_ID}/revoke`)) return jsonResponse({ data: { revoked: true }, request_id: 'r_rev' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.status.value).toBe('signed_in')
      expect(store.sessions.value.find(s => s.id === UNKNOWN_SESSION_ID)).toBeUndefined()

      // Revoke this unknown session
      const ok = await store.revokeSession(UNKNOWN_SESSION_ID)
      expect(ok).toBe(false)
      // Because second me() failed with 503, status must NOT be signed_in! It must be error (fail-closed)
      expect(store.status.value).toBe('error')
      expect(store.sessionsLoading.value).toBe(false)
      expect(meCalls).toBe(2)
    })

    // C-2 variant: unknown session was current session on the backend; me() returns 401 AUTH_REQUIRED -> becomes signed_out
    it('C-2 variant: revoking an unknown session that happens to be self -> backend revokes self cookie -> me() returns 401 -> transitions to signed_out', async () => {
      const UNKNOWN_SESSION_ID = 'e'.repeat(64)
      let meCalls = 0
      fakeFetch((url) => {
        if (url.includes('/auth/me')) {
          meCalls++
          if (meCalls === 1) return jsonResponse({ data: ME_DATA, request_id: 'r_me_1' })
          return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r_me_401' }, 401)
        }
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes(`/auth/sessions/${UNKNOWN_SESSION_ID}/revoke`)) return jsonResponse({ data: { revoked: true }, request_id: 'r_rev' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.status.value).toBe('signed_in')

      const ok = await store.revokeSession(UNKNOWN_SESSION_ID)
      expect(ok).toBe(true)
      expect(store.status.value).toBe('signed_out')
      expect(store.user.value).toBeNull()
      expect(store.csrfToken.value).toBeNull()
      expect(store.sessions.value).toEqual([])
      expect(store.sessionsLoading.value).toBe(false)
    })

    // I-1: listSessions 竞态：慢的旧列表响应不能覆盖新的列表响应
    it('I-1 race: slow listSessions response cannot overwrite newer listSessions results', async () => {
      let resolveSlowList!: (r: Response) => void
      const slowListGate = new Promise<Response>((r) => { resolveSlowList = r })
      let listCalls = 0
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50')) {
          listCalls++
          if (listCalls === 1) return slowListGate
          return jsonResponse({
            data: {
              items: [{ id: '9'.repeat(64), current: true, created_time: '2026-10-05T09:00:00Z' }],
              next_cursor: null,
            },
            request_id: 'r_list_new',
          })
        }
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()

      // Start slow list call 1
      const p1 = store.listSessions()
      // Start fast list call 2
      const p2 = store.listSessions()
      await p2
      expect(store.sessions.value.length).toBe(1)
      expect(store.sessions.value[0].id).toBe('9'.repeat(64))

      // Now slow list call 1 resolves with SESSIONS_PAGE_1 (2 items)
      resolveSlowList(jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list_slow' }))
      await p1

      // Must retain call 2's session list, NOT overwritten by call 1
      expect(store.sessions.value.length).toBe(1)
      expect(store.sessions.value[0].id).toBe('9'.repeat(64))
    })

    // I-3: logout/401 必须清除 sessions 和 sessionsNextCursor
    it('I-3: logout clears sessions and sessionsNextCursor', async () => {
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes('/auth/logout')) return jsonResponse({ data: { logged_out: true }, request_id: 'r_logout' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.sessions.value.length).toBe(2)
      expect(store.sessionsNextCursor.value).toBe(SESSIONS_PAGE_1.next_cursor)

      await store.logout()
      expect(store.sessions.value).toEqual([])
      expect(store.sessionsNextCursor.value).toBeNull()
    })

    it('I-3: refresh 401 clears sessions and sessionsNextCursor', async () => {
      let meCalls = 0
      fakeFetch((url) => {
        if (url.includes('/auth/me')) {
          meCalls++
          if (meCalls === 1) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
          return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r_401' }, 401)
        }
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.sessions.value.length).toBe(2)
      expect(store.sessionsNextCursor.value).toBe(SESSIONS_PAGE_1.next_cursor)

      await store.refresh()
      expect(store.status.value).toBe('signed_out')
      expect(store.sessions.value).toEqual([])
      expect(store.sessionsNextCursor.value).toBeNull()
    })

    // Minor: cursor 必须恰好 132 位小写 hex
    it('Minor: assertSessionsListShape rejects next_cursor that is not 132 lowercase hex', async () => {
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50')) {
          return jsonResponse({
            data: {
              items: [{ id: VALID_ID_1, current: true, created_time: '2026-10-05T00:00:00Z' }],
              next_cursor: 'not-132-hex',
            },
            request_id: 'r_bad_cur',
          })
        }
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      const ok = await store.listSessions()
      expect(ok).toBe(false)
      expect(store.sessionsError.value).toBe('INVALID_API_RESPONSE')
      expect(store.sessions.value).toEqual([])
    })

    // Minor: revoked_count 必须是非负整数（不能是小数或负数或非数字）
    it('Minor: revokeOthers rejects revoked_count that is not a non-negative integer (e.g. float 1.5)', async () => {
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes('/auth/sessions/revoke-others')) return jsonResponse({ data: { revoked_count: 1.5 }, request_id: 'r_float' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      await store.listSessions()

      const ok = await store.revokeOthers()
      expect(ok).toBe(false)
      expect(store.sessionsError.value).toBe('INVALID_API_RESPONSE')
    })

    // Minor: 分页重复 id 去重
    it('Minor: listSessions with cursor deduplicates existing session ids', async () => {
      const DUPLICATE_ID = VALID_ID_2
      const PAGE_2_DUP = {
        items: [
          { id: DUPLICATE_ID, current: false, created_time: '2026-10-05T01:00:00Z' },
          { id: '7'.repeat(64), current: false, created_time: '2026-10-05T03:00:00Z' },
        ],
        next_cursor: null,
      }
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50&cursor=')) return jsonResponse({ data: PAGE_2_DUP, request_id: 'r_p2' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_p1' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.sessions.value.length).toBe(2)

      const ok = await store.listSessions(store.sessionsNextCursor.value)
      expect(ok).toBe(true)
      // Original 2 items + 1 unique new item = 3 items (DUPLICATE_ID deduplicated)
      expect(store.sessions.value.length).toBe(3)
      const ids = store.sessions.value.map(s => s.id)
      expect(ids).toEqual([VALID_ID_1, VALID_ID_2, '7'.repeat(64)])
    })

    it('sessionsLoading: revoking current session, unknown session 401 reconcile, and unknown session 503 reconcile all reset sessionsLoading to false', async () => {
      // 1. Current session revocation
      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes(`/auth/sessions/${VALID_ID_1}/revoke`)) return jsonResponse({ data: { revoked: true }, request_id: 'r_rev' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store1 = createAuth()
      await store1.refresh()
      await store1.listSessions()
      expect(store1.sessionsLoading.value).toBe(false)
      const res1 = await store1.revokeSession(VALID_ID_1)
      expect(res1).toBe(true)
      expect(store1.sessionsLoading.value).toBe(false)

      // 2. Unknown target session reconcile 401
      const UNKNOWN_ID = 'e'.repeat(64)
      let meCalls = 0
      fakeFetch((url) => {
        if (url.includes('/auth/me')) {
          meCalls++
          if (meCalls === 1) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
          return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: 'r_me_401' }, 401)
        }
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes(`/auth/sessions/${UNKNOWN_ID}/revoke`)) return jsonResponse({ data: { revoked: true }, request_id: 'r_rev' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store2 = createAuth()
      await store2.refresh()
      await store2.listSessions()
      expect(store2.sessionsLoading.value).toBe(false)
      const res2 = await store2.revokeSession(UNKNOWN_ID)
      expect(res2).toBe(true)
      expect(store2.status.value).toBe('signed_out')
      expect(store2.sessionsLoading.value).toBe(false)

      // 3. Unknown target session reconcile 503
      meCalls = 0
      fakeFetch((url) => {
        if (url.includes('/auth/me')) {
          meCalls++
          if (meCalls === 1) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
          return jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: 'r_me_503' }, 503)
        }
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes(`/auth/sessions/${UNKNOWN_ID}/revoke`)) return jsonResponse({ data: { revoked: true }, request_id: 'r_rev' })
        return jsonResponse({ data: null, request_id: 'r0' })
      })
      const store3 = createAuth()
      await store3.refresh()
      await store3.listSessions()
      expect(store3.sessionsLoading.value).toBe(false)
      const res3 = await store3.revokeSession(UNKNOWN_ID)
      expect(res3).toBe(false)
      expect(store3.status.value).toBe('error')
      expect(store3.sessionsLoading.value).toBe(false)
    })

    // sessionsLoading guard: an older in-flight session operation completing must not prematurely clear sessionsLoading if a newer operation is still in-flight
    it('race: older in-flight session operation cannot prematurely clear sessionsLoading of a newer in-flight operation', async () => {
      let resolveSlowOp!: (r: Response) => void
      const slowOpGate = new Promise<Response>((r) => { resolveSlowOp = r })
      let resolveFastOp!: (r: Response) => void
      const fastOpGate = new Promise<Response>((r) => { resolveFastOp = r })
      let listCalls = 0

      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50')) {
          listCalls++
          if (listCalls === 1) return slowOpGate // first listSessions is slow
          return fastOpGate // second listSessions or fast op
        }
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      expect(store.sessionsLoading.value).toBe(false)

      // Request 1 starts (old request)
      const p1 = store.listSessions()
      expect(store.sessionsLoading.value).toBe(true)

      // Request 2 starts (newer request)
      const p2 = store.listSessions()
      expect(store.sessionsLoading.value).toBe(true)

      // Request 1 finishes while Request 2 is still pending
      resolveSlowOp(jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_p1' }))
      await p1

      // Request 2 is still in-flight: sessionsLoading MUST still be true!
      expect(store.sessionsLoading.value).toBe(true)

      // Now Request 2 finishes
      resolveFastOp(jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_p2' }))
      await p2
      expect(store.sessionsLoading.value).toBe(false)
    })

    // sessionsLoading guard: an older in-flight revokeSession completing must not prematurely clear sessionsLoading if a newer revoke is still in-flight
    it('race: older in-flight revokeSession cannot prematurely clear sessionsLoading of a newer in-flight revoke', async () => {
      let resolveSlowRevoke!: (r: Response) => void
      const slowRevokeGate = new Promise<Response>((r) => { resolveSlowRevoke = r })
      let resolveFastRevoke!: (r: Response) => void
      const fastRevokeGate = new Promise<Response>((r) => { resolveFastRevoke = r })
      let revokeCalls = 0

      fakeFetch((url) => {
        if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: 'r_me' })
        if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_PAGE_1, request_id: 'r_list' })
        if (url.includes('/auth/sessions/') && url.includes('/revoke')) {
          revokeCalls++
          if (revokeCalls === 1) return slowRevokeGate
          return fastRevokeGate
        }
        return jsonResponse({ data: null, request_id: 'r0' })
      })

      const store = createAuth()
      await store.refresh()
      await store.listSessions()
      expect(store.sessionsLoading.value).toBe(false)

      const p1 = store.revokeSession(VALID_ID_2)
      expect(store.sessionsLoading.value).toBe(true)

      const p2 = store.revokeOthers()
      expect(store.sessionsLoading.value).toBe(true)

      // p1 finishes
      resolveSlowRevoke(jsonResponse({ data: { revoked: true }, request_id: 'r_rev_slow' }))
      await p1

      // p2 is still in-flight: sessionsLoading must remain true
      expect(store.sessionsLoading.value).toBe(true)

      // p2 finishes
      resolveFastRevoke(jsonResponse({ data: { revoked_count: 1 }, request_id: 'r_rev_others_fast' }))
      await p2
      expect(store.sessionsLoading.value).toBe(false)
    })
  })
})

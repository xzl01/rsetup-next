import { ref, type Ref } from 'vue'
import { ApiError, get, post } from './api'

/** Identity fields as returned by /auth/me and /auth/login (user_public projection). */
export interface AuthUser {
  /** Canonical lowercase standard UUIDv4 string, as emitted by the /api/v1 projection. */
  id: string
  username: string
  display_name?: string
  must_change_password: boolean
  is_admin?: boolean
  active?: boolean
  /** Canonical decimal string (e.g. "1"); never a JSON number. */
  revision: string
}

export type AuthStatus = 'checking' | 'signed_out' | 'force_password' | 'signed_in' | 'error'

export type AuthErrorCode =
  | 'INVALID_CREDENTIALS' | 'AUTH_REQUIRED' | 'RATE_LIMITED'
  | 'PASSWORD_CHANGE_REQUIRED' | 'PERMISSION_DENIED'
  | 'INVALID_API_RESPONSE' | 'NETWORK_ERROR' | 'REQUEST_ABORTED' | 'OTHER'

export interface UserSession {
  id: string
  current: boolean
  created_time: string
}

export interface SessionListPage {
  items: UserSession[]
  next_cursor: string | null
}

export interface AuthStore {
  status: Ref<AuthStatus>
  user: Ref<AuthUser | null>
  csrfToken: Ref<string | null>
  authzEpoch: Ref<string | null>
  errorCode: Ref<AuthErrorCode | null>
  sessions: Ref<UserSession[]>
  sessionsNextCursor: Ref<string | null>
  sessionsLoading: Ref<boolean>
  sessionsError: Ref<AuthErrorCode | null>
  refresh(): Promise<void>   // GET /auth/me；仅 401 → signed_out，其余失败 → error
  login(username: string, password: string): Promise<boolean>   // 无 CSRF header；成功存内存 token
  changePassword(currentPassword: string, newPassword: string): Promise<boolean>
  logout(): Promise<void>
  listSessions(cursor?: string | null): Promise<boolean>
  revokeSession(sessionId: string): Promise<boolean>
  revokeOtherSessions(): Promise<boolean>
  revokeOthers(): Promise<boolean>
}

const KNOWN_CODES: readonly AuthErrorCode[] = [
  'INVALID_CREDENTIALS', 'AUTH_REQUIRED', 'RATE_LIMITED',
  'PASSWORD_CHANGE_REQUIRED', 'PERMISSION_DENIED',
  'INVALID_API_RESPONSE', 'NETWORK_ERROR', 'REQUEST_ABORTED',
]

function errorCodeFor(error: unknown): AuthErrorCode {
  if (error instanceof ApiError && (KNOWN_CODES as readonly string[]).includes(error.code)) {
    return error.code as AuthErrorCode
  }
  return 'OTHER'
}

interface MeData { user: AuthUser; csrf_token: string; authz_epoch: string }

const invalidShape = () => new ApiError({ code: 'INVALID_API_RESPONSE', messageKey: 'errors.invalidResponse' })

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

// Full lowercase standard UUIDv4 with RFC 4122 variant bits (8/9/a/b): version nibble fixed to 4.
// A JSON number or any other casing/format fails the typeof + regex pair.
const USER_ID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/

// Canonical decimal string for revision: no empty, no sign, no fraction, no exponent, no leading
// zero (so "0" is valid but "01" is not); a JSON number fails the typeof check. No Number/parseInt.
const DECIMAL_STRING = /^(0|[1-9][0-9]*)$/

// get/post validate only the envelope (data passes through as T), so identity fields are re-checked here
// at runtime — including the business id (UUIDv4) and revision (decimal string) contract.
// Missing/wrong shape → INVALID_API_RESPONSE; the caller clears identity/CSRF and never enters
// signed_in/force_password.
function assertIdentityShape(data: unknown): void {
  if (!isPlainObject(data) || !isPlainObject(data.user)) throw invalidShape()
  if (typeof data.user.id !== 'string' || !USER_ID_V4.test(data.user.id)) throw invalidShape()
  if (typeof data.user.username !== 'string' || data.user.username.length === 0) throw invalidShape()
  if (typeof data.user.revision !== 'string' || !DECIMAL_STRING.test(data.user.revision)) throw invalidShape()
  if (typeof data.user.must_change_password !== 'boolean') throw invalidShape()
  if (typeof data.csrf_token !== 'string' || data.csrf_token.length === 0) throw invalidShape()
}

// /auth/me additionally requires a canonical decimal authz_epoch.
function assertEpochShape(epoch: unknown): void {
  if (typeof epoch !== 'string' || !/^\d+$/.test(epoch)) throw invalidShape()
}

function assertSessionsListShape(data: unknown): asserts data is SessionListPage {
  if (!isPlainObject(data) || !Array.isArray(data.items)) throw invalidShape()
  if (data.next_cursor !== null && (typeof data.next_cursor !== 'string' || !/^[0-9a-f]{132}$/.test(data.next_cursor))) {
    throw invalidShape()
  }
  for (const item of data.items) {
    if (!isPlainObject(item)) throw invalidShape()
    if (typeof item.id !== 'string' || !/^[0-9a-f]{64}$/.test(item.id)) throw invalidShape()
    if (typeof item.current !== 'boolean') throw invalidShape()
    if (typeof item.created_time !== 'string' || item.created_time.length === 0) throw invalidShape()
  }
}

export function createAuth(): AuthStore {
  const status = ref<AuthStatus>('checking')
  const user = ref<AuthUser | null>(null)
  const csrfToken = ref<string | null>(null)
  const authzEpoch = ref<string | null>(null)
  const errorCode = ref<AuthErrorCode | null>(null)
  const sessions = ref<UserSession[]>([])
  const sessionsNextCursor = ref<string | null>(null)
  const sessionsLoading = ref<boolean>(false)
  const sessionsError = ref<AuthErrorCode | null>(null)
  // Generation guard: a stale in-flight response must never overwrite a newer operation's state.
  let gen = 0
  let sessionsGen = 0
  let sessionsOpGen = 0

  function applyMe(data: MeData): void {
    user.value = data.user
    csrfToken.value = data.csrf_token
    authzEpoch.value = data.authz_epoch
    errorCode.value = null
    status.value = data.user.must_change_password === true ? 'force_password' : 'signed_in'
  }

  // Only a validated 401 from me means "not signed in"; every other failure is a retryable error state.
  function fail(error: unknown, mine: number): void {
    if (mine !== gen) return
    if (error instanceof ApiError && error.status === 401) {
      user.value = null
      csrfToken.value = null
      authzEpoch.value = null
      errorCode.value = null
      sessions.value = []
      sessionsNextCursor.value = null
      sessionsLoading.value = false
      status.value = 'signed_out'
      return
    }
    errorCode.value = errorCodeFor(error)
    status.value = 'error'
  }

  // Shape-guard failure / malformed envelope: drop local identity/CSRF/epoch, do not guess login state.
  function rejectIdentity(mine: number): void {
    if (mine !== gen) return
    user.value = null
    csrfToken.value = null
    authzEpoch.value = null
    errorCode.value = 'INVALID_API_RESPONSE'
    status.value = 'error'
  }

  async function refresh(): Promise<void> {
    const mine = ++gen
    status.value = 'checking'
    errorCode.value = null
    try {
      const { data } = await get<MeData>('/auth/me')
      if (mine !== gen) return
      assertIdentityShape(data)
      assertEpochShape(data.authz_epoch)
      applyMe(data)
    } catch (error) {
      if (mine !== gen) return
      if (error instanceof ApiError && error.code === 'INVALID_API_RESPONSE') rejectIdentity(mine)
      else fail(error, mine)
    }
  }

  async function login(username: string, password: string): Promise<boolean> {
    const mine = ++gen
    status.value = 'checking'
    errorCode.value = null
    try {
      const { data } = await post<{ user: AuthUser; csrf_token: string }>('/auth/login',
        { username, password }) // first-time login: no CSRF token exists yet
      if (mine !== gen) return false
      assertIdentityShape(data) // login response carries no authz_epoch
      user.value = data.user
      csrfToken.value = data.csrf_token
      authzEpoch.value = null
      status.value = data.user.must_change_password === true ? 'force_password' : 'signed_in'
      return true
    } catch (error) {
      if (mine !== gen) return false
      if (error instanceof ApiError && error.code === 'INVALID_API_RESPONSE') {
        rejectIdentity(mine)
      } else if (error instanceof ApiError && error.status === 401) {
        user.value = null
        csrfToken.value = null
        authzEpoch.value = null // I-A: a 401 clears the full sensitive set, incl. a stale epoch from a prior me
        errorCode.value = errorCodeFor(error)
        sessions.value = []
        sessionsNextCursor.value = null
        sessionsLoading.value = false
        status.value = 'signed_out'
      } else {
        fail(error, mine)
      }
      return false
    }
  }

  async function changePassword(currentPassword: string, newPassword: string): Promise<boolean> {
    const mine = ++gen
    const priorStatus = status.value
    const priorUser = user.value
    try {
      const { data } = await post<{ changed: boolean }>('/auth/password',
        { current_password: currentPassword, new_password: newPassword },
        { csrfToken: csrfToken.value ?? undefined })
      if (mine !== gen) return false
      if (data.changed !== true) throw invalidShape()
      // Changing the password revokes all sessions: drop local secrets and re-establish real state via me()
      // (a 401 there resolves to signed_out). No shared-state write happens after the internal await, so a
      // stale outer generation cannot clobber a newer operation.
      csrfToken.value = null
      user.value = null
      authzEpoch.value = null
      await refresh()
      return true
    } catch (error) {
      if (mine !== gen) return false
      if (error instanceof ApiError && error.status === 401) {
        // I-B: per the /auth/password contract (auth: 401 → CSRF 403 → body 400 → verify 400) every 4xx
        // short-circuits before commit, and the only 401 this endpoint can emit is a dead session —
        // a wrong old password is 400 INVALID_ARGUMENT. Defense in depth: any validated 401, whatever its
        // code, means session-invalidated → signed_out; never the keep-state 4xx branch (phantom signed_in).
        user.value = null
        csrfToken.value = null
        authzEpoch.value = null
        errorCode.value = null
        sessions.value = []
        sessionsNextCursor.value = null
        sessionsLoading.value = false
        status.value = 'signed_out'
        return false
      }
      if (error instanceof ApiError && error.status !== undefined && error.status >= 400 && error.status < 500) {
        // Explicit 4xx (e.g. 400 INVALID_ARGUMENT — wrong old password): the server definitively did NOT
        // change it, so the prior signed_in/force_password state is preserved with a safe mapped error.
        user.value = priorUser
        status.value = priorStatus === 'checking' ? 'error' : priorStatus
        errorCode.value = errorCodeFor(error)
        return false
      }
      // Unknown outcome — the change MAY have executed (network/abort/invalid response/5xx): never restore the
      // prior status or the stale csrf; clear local secrets and surface a "please log in again" error.
      user.value = null
      csrfToken.value = null
      authzEpoch.value = null
      errorCode.value = errorCodeFor(error)
      status.value = 'error'
      return false
    }
  }

  async function logout(): Promise<void> {
    const mine = ++gen
    try {
      const { data } = await post<{ logged_out: boolean }>('/auth/logout', {},
        { csrfToken: csrfToken.value ?? undefined })
      if (mine !== gen) return // a stale logout must not wipe a newer login
      if (data.logged_out !== true) throw invalidShape()
      user.value = null
      csrfToken.value = null
      authzEpoch.value = null
      errorCode.value = null
      sessions.value = []
      sessionsNextCursor.value = null
      sessionsLoading.value = false
      status.value = 'signed_out'
    } catch (error) {
      if (mine !== gen) return
      // Network / non-JSON / status error: server revocation unknown (HttpOnly cookie is unreadable from JS) —
      // never claim signed_out; drop the usable local csrf, keep user/epoch for a later me()-driven reconcile.
      csrfToken.value = null
      errorCode.value = errorCodeFor(error)
      status.value = 'error'
    }
  }

  function handleAuthError(error: unknown, mine: number): void {
    if (mine !== gen) return
    if (error instanceof ApiError && error.status === 401) {
      user.value = null
      csrfToken.value = null
      authzEpoch.value = null
      errorCode.value = null
      sessions.value = []
      sessionsNextCursor.value = null
      sessionsLoading.value = false
      status.value = 'signed_out'
      return
    }
  }

  async function listSessions(cursor?: string | null, internalOpMine?: number): Promise<boolean> {
    const mine = gen
    const listMine = ++sessionsGen
    const opMine = internalOpMine ?? ++sessionsOpGen
    sessionsLoading.value = true
    sessionsError.value = null
    try {
      const query = cursor
        ? `/auth/sessions?limit=50&cursor=${encodeURIComponent(cursor)}`
        : '/auth/sessions?limit=50'
      const { data } = await get<SessionListPage>(query)
      if (mine !== gen || listMine !== sessionsGen) return false
      assertSessionsListShape(data)
      if (cursor) {
        const existingIds = new Set(sessions.value.map((s) => s.id))
        const newItems = data.items.filter((s) => !existingIds.has(s.id))
        sessions.value = [...sessions.value, ...newItems]
      } else {
        sessions.value = data.items
      }
      sessionsNextCursor.value = data.next_cursor
      return true
    } catch (error) {
      if (mine !== gen || listMine !== sessionsGen) return false
      if (error instanceof ApiError && error.status === 401) {
        handleAuthError(error, mine)
        return false
      }
      sessionsError.value = errorCodeFor(error)
      if (!cursor) {
        sessions.value = []
        sessionsNextCursor.value = null
      }
      return false
    } finally {
      if (opMine === sessionsOpGen) {
        sessionsLoading.value = false
      }
    }
  }

  async function revokeSession(sessionId: string): Promise<boolean> {
    const mine = gen
    const opMine = ++sessionsOpGen
    sessionsLoading.value = true
    sessionsError.value = null
    const matchedSession = sessions.value.find((s) => s.id === sessionId)
    const isKnownCurrent = matchedSession !== undefined && matchedSession.current === true
    const isKnownOther = matchedSession !== undefined && matchedSession.current === false
    try {
      const { data } = await post<{ revoked: boolean }>(
        `/auth/sessions/${encodeURIComponent(sessionId)}/revoke`,
        {},
        { csrfToken: csrfToken.value ?? undefined },
      )
      if (mine !== gen) return false
      if (!isPlainObject(data) || data.revoked !== true) throw invalidShape()

      if (isKnownCurrent) {
        // C-1: Known current session successfully revoked -> advance gen to invalidate prior in-flight me/refreshes
        ++gen
        user.value = null
        csrfToken.value = null
        authzEpoch.value = null
        errorCode.value = null
        sessions.value = []
        sessionsNextCursor.value = null
        sessionsLoading.value = false
        status.value = 'signed_out'
        return true
      }

      if (isKnownOther) {
        await listSessions(undefined, opMine)
        return true
      }

      // C-2: Target session was unknown (e.g. not loaded yet or direct call).
      // If it revoked self on backend, HttpOnly cookie is gone or invalid; if it revoked other, session remains.
      // Must reconcile identity via me(). If me() network/error, must fail-closed to error, never keep signed_in.
      try {
        await refresh()
      } catch {
        // refresh handles fail/error internally, but if uncaught:
        status.value = 'error'
      }
      if (status.value === 'signed_in') {
        await listSessions(undefined, opMine)
        return true
      }
      if (status.value === 'signed_out') {
        return true
      }
      // Status fell to error (network or 5xx or invalid) -> do not claim success
      return false
    } catch (error) {
      if (mine !== gen) return false
      if (error instanceof ApiError && error.status === 401) {
        handleAuthError(error, mine)
        return false
      }
      sessionsError.value = errorCodeFor(error)
      return false
    } finally {
      if (opMine === sessionsOpGen) {
        sessionsLoading.value = false
      }
    }
  }

  async function revokeOtherSessions(): Promise<boolean> {
    const mine = gen
    const opMine = ++sessionsOpGen
    sessionsLoading.value = true
    sessionsError.value = null
    try {
      const { data } = await post<{ revoked_count: number }>(
        '/auth/sessions/revoke-others',
        {},
        { csrfToken: csrfToken.value ?? undefined },
      )
      if (mine !== gen) return false
      if (!isPlainObject(data) || typeof data.revoked_count !== 'number' || !Number.isInteger(data.revoked_count) || data.revoked_count < 0) {
        throw invalidShape()
      }

      await listSessions(undefined, opMine)
      return true
    } catch (error) {
      if (mine !== gen) return false
      if (error instanceof ApiError && error.status === 401) {
        handleAuthError(error, mine)
        return false
      }
      sessionsError.value = errorCodeFor(error)
      return false
    } finally {
      if (opMine === sessionsOpGen) {
        sessionsLoading.value = false
      }
    }
  }

  async function revokeOthers(): Promise<boolean> {
    return revokeOtherSessions()
  }

  return {
    status,
    user,
    csrfToken,
    authzEpoch,
    errorCode,
    sessions,
    sessionsNextCursor,
    sessionsLoading,
    sessionsError,
    refresh,
    login,
    changePassword,
    logout,
    listSessions,
    revokeSession,
    revokeOtherSessions,
    revokeOthers,
  }
}

export function authErrorKey(code: string): string {
  switch (code) {
    case 'INVALID_CREDENTIALS': return 'auth.error.invalidCredentials'
    case 'AUTH_REQUIRED': return 'auth.error.authRequired'
    case 'RATE_LIMITED': return 'auth.error.rateLimited'
    case 'PASSWORD_CHANGE_REQUIRED': return 'auth.error.passwordChangeRequired'
    case 'PERMISSION_DENIED': return 'auth.error.denied'
    default: return 'errors.generic'
  }
}

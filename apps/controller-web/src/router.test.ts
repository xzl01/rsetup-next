import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import { createRouter } from './router'
import type { AuthStore, AuthUser, AuthStatus, AuthErrorCode, UserSession } from './auth'

function fakeAuthStore(
  user: AuthUser | null,
  status: AuthStatus = 'signed_in'
): AuthStore {
  return {
    status: ref(status),
    user: ref(user),
    csrfToken: ref('token'),
    authzEpoch: ref('1'),
    errorCode: ref<AuthErrorCode | null>(null),
    sessions: ref<UserSession[]>([]),
    sessionsNextCursor: ref<string | null>(null),
    sessionsLoading: ref(false),
    sessionsError: ref<AuthErrorCode | null>(null),
    refresh: vi.fn(),
    login: vi.fn(),
    changePassword: vi.fn(),
    logout: vi.fn(),
    listSessions: vi.fn(),
    revokeSession: vi.fn(),
    revokeOtherSessions: vi.fn(),
    revokeOthers: vi.fn(),
  }
}

describe('client router hash synchronization & guards', () => {
  beforeEach(() => {
    window.location.hash = ''
  })
  afterEach(() => {
    window.location.hash = ''
  })

  it('redirects unauthenticated users to login and updates hash', () => {
    const auth = fakeAuthStore(null, 'signed_out')
    const router = createRouter(auth)
    expect(router.resolveInitialRoute(null)).toEqual({ name: 'login' })
    router.navigate({ name: 'devices' })
    expect(router.currentRoute.value).toEqual({ name: 'login' })
    expect(window.location.hash).toBe('#/login')
    router.cleanup()
  })

  it('preserves bookmarked route on initial mount when status is checking, without rewriting hash to login', () => {
    window.location.hash = '#/tasks'
    const auth = fakeAuthStore(null, 'checking')
    const router = createRouter(auth)
    expect(window.location.hash).toBe('#/tasks')
    expect(router.currentRoute.value).toEqual({ name: 'tasks' })
    router.cleanup()
  })

  it('preserves bookmarked canonical hex64 device route on initial mount when status is checking', () => {
    const validHex64 = '0123456789abcdef'.repeat(4)
    window.location.hash = `#/devices/${validHex64}`
    const auth = fakeAuthStore(null, 'checking')
    const router = createRouter(auth)
    expect(window.location.hash).toBe(`#/devices/${validHex64}`)
    expect(router.currentRoute.value).toEqual({
      name: 'device-detail',
      params: { id: validHex64 },
    })
    router.cleanup()
  })

  it('restores bookmarked route upon refresh success and redirects on 401 or force_password', async () => {
    window.location.hash = '#/tasks'
    const auth = fakeAuthStore(null, 'checking')
    const router = createRouter(auth)
    expect(router.currentRoute.value).toEqual({ name: 'tasks' })

    // Case 1: refresh succeeds with signed_in
    auth.user.value = {
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    }
    auth.status.value = 'signed_in'
    await (new Promise((resolve) => setTimeout(resolve, 0)))
    expect(router.currentRoute.value).toEqual({ name: 'tasks' })
    expect(window.location.hash).toBe('#/tasks')

    // Case 2: status changes to force_password -> redirects to password
    auth.user.value = {
      ...auth.user.value,
      must_change_password: true,
    }
    auth.status.value = 'force_password'
    await (new Promise((resolve) => setTimeout(resolve, 0)))
    expect(router.currentRoute.value).toEqual({ name: 'password' })
    expect(window.location.hash).toBe('#/password')

    // Case 3: status changes to signed_out -> redirects to login
    auth.user.value = null
    auth.status.value = 'signed_out'
    await (new Promise((resolve) => setTimeout(resolve, 0)))
    expect(router.currentRoute.value).toEqual({ name: 'login' })
    expect(window.location.hash).toBe('#/login')

    router.cleanup()
  })

  it('redirects signed_in users visiting login or password routes back to devices', () => {
    const auth = fakeAuthStore({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: false,
      revision: '1',
    }, 'signed_in')
    const router = createRouter(auth)

    router.navigate({ name: 'login' })
    expect(router.currentRoute.value).toEqual({ name: 'devices' })
    expect(window.location.hash).toBe('#/devices')

    router.navigate({ name: 'password' })
    expect(router.currentRoute.value).toEqual({ name: 'devices' })
    expect(window.location.hash).toBe('#/devices')

    // Hash change to #/login also redirected
    window.location.hash = '#/login'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    expect(router.currentRoute.value).toEqual({ name: 'devices' })
    expect(window.location.hash).toBe('#/devices')

    router.cleanup()
  })

  it('strictly forces users with must_change_password to password view', () => {
    const auth = fakeAuthStore({
      id: '123e4567-e89b-42d3-a456-426614174000',
      username: 'temp_admin',
      must_change_password: true,
      is_admin: true,
      revision: '1',
    }, 'force_password')
    const router = createRouter(auth)
    expect(router.resolveInitialRoute(auth.user.value)).toEqual({ name: 'password' })
    router.navigate({ name: 'devices' })
    expect(router.currentRoute.value).toEqual({ name: 'password' })
    router.navigate({ name: 'admin-users' })
    expect(router.currentRoute.value).toEqual({ name: 'password' })
    router.cleanup()
  })

  it('blocks non-admin users from admin routes and redirects to devices', () => {
    const auth = fakeAuthStore({
      id: '223e4567-e89b-42d3-a456-426614174001',
      username: 'operator',
      must_change_password: false,
      is_admin: false,
      revision: '2',
    })
    const router = createRouter(auth)
    router.navigate({ name: 'admin-approvals' })
    expect(router.currentRoute.value).toEqual({ name: 'devices' })
    router.cleanup()
  })

  it('allows admin users to navigate to admin routes', () => {
    const auth = fakeAuthStore({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    })
    const router = createRouter(auth)
    router.navigate({ name: 'admin-approvals' })
    expect(router.currentRoute.value).toEqual({ name: 'admin-approvals' })
    expect(window.location.hash).toBe('#/admin/approvals')
    router.cleanup()
  })

  it('handles window hashchange event and cleans up listener on cleanup', () => {
    const auth = fakeAuthStore({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    })
    const router = createRouter(auth)
    window.location.hash = '#/tasks'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    expect(router.currentRoute.value).toEqual({ name: 'tasks' })

    router.cleanup()
    window.location.hash = '#/devices'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    // After cleanup, currentRoute does not update from window hashchange
    expect(router.currentRoute.value).toEqual({ name: 'tasks' })
  })

  it('reacts to auth status changes: redirects to login if user becomes signed_out', () => {
    const auth = fakeAuthStore({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    })
    const router = createRouter(auth)
    router.navigate({ name: 'devices' })
    expect(router.currentRoute.value).toEqual({ name: 'devices' })

    // Simulate session expired / permissions revoked to signed_out
    auth.user.value = null
    auth.status.value = 'signed_out'
    // Router reacts to reactive store change
    router.navigate(router.currentRoute.value)
    expect(router.currentRoute.value).toEqual({ name: 'login' })
    router.cleanup()
  })

  describe('canonical identity validation & fail closed (02 §1 & progress.md ruling)', () => {
    const adminUser: AuthUser = {
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    }

    it('accepts canonical lowercase hex64 device id and rejects malformed ones by falling back to devices and normalizing hash', () => {
      const auth = fakeAuthStore(adminUser)
      const router = createRouter(auth)
      const validHex64 = '0123456789abcdef'.repeat(4) // 64 chars
      window.location.hash = `#/devices/${validHex64}`
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({
        name: 'device-detail',
        params: { id: validHex64 },
      })

      // Programmatic navigate with invalid id also fails closed
      router.navigate({ name: 'device-detail', params: { id: 'invalid-id' } })
      expect(router.currentRoute.value).toEqual({ name: 'devices' })
      expect(window.location.hash).toBe('#/devices')

      // Uppercase hex64 fails closed (spec requires lowercase hex64)
      const upperHex64 = '0123456789ABCDEF'.repeat(4)
      window.location.hash = `#/devices/${upperHex64}`
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({ name: 'devices' })
      expect(window.location.hash).toBe('#/devices')

      // Short or malformed id fails closed
      window.location.hash = '#/devices/dev-123'
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({ name: 'devices' })
      expect(window.location.hash).toBe('#/devices')

      router.cleanup()
    })

    it('accepts canonical UUIDv4 task id and rejects malformed ones by falling back to tasks and normalizing hash', () => {
      const auth = fakeAuthStore(adminUser)
      const router = createRouter(auth)
      const validTaskUuid = 'a1234567-e89b-42d3-a456-426614174099'
      window.location.hash = `#/tasks/${validTaskUuid}`
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({
        name: 'task-detail',
        params: { id: validTaskUuid },
      })

      // Programmatic navigate with invalid task id also fails closed
      router.navigate({ name: 'task-detail', params: { id: 'invalid-task' } })
      expect(router.currentRoute.value).toEqual({ name: 'tasks' })
      expect(window.location.hash).toBe('#/tasks')

      // Non-v4 UUID (version 1) or uppercase fails closed
      const nonV4Uuid = 'a1234567-e89b-12d3-a456-426614174099'
      window.location.hash = `#/tasks/${nonV4Uuid}`
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({ name: 'tasks' })
      expect(window.location.hash).toBe('#/tasks')

      // Short id fails closed
      window.location.hash = '#/tasks/task-456'
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({ name: 'tasks' })
      expect(window.location.hash).toBe('#/tasks')

      router.cleanup()
    })

    it('fails closed to devices when unknown path is requested for authenticated user and normalizes URL hash', () => {
      const auth = fakeAuthStore(adminUser)
      const router = createRouter(auth)
      expect(router.currentRoute.value).toEqual({ name: 'devices' })
      expect(window.location.hash).toBe('#/devices')

      // Malformed or unknown hash when currentRoute is already devices
      window.location.hash = '#/devices/INVALID'
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({ name: 'devices' })
      expect(window.location.hash).toBe('#/devices')

      window.location.hash = '#/unknown/something'
      window.dispatchEvent(new HashChangeEvent('hashchange'))
      expect(router.currentRoute.value).toEqual({ name: 'devices' })
      expect(window.location.hash).toBe('#/devices')

      router.cleanup()
    })
  })
})

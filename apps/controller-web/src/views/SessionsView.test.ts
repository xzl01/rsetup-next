import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import SessionsView from './SessionsView.vue'
import { createI18n } from '../i18n'
import type { AuthStore, AuthStatus, AuthErrorCode, UserSession } from '../auth'

function fakeAuthStore(
  sessions: UserSession[] = [],
  nextCursor: string | null = null,
  loading: boolean = false,
  error: AuthErrorCode | null = null,
): AuthStore {
  return {
    status: ref<AuthStatus>('signed_in'),
    user: ref({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    }),
    csrfToken: ref('token'),
    authzEpoch: ref('1'),
    errorCode: ref<AuthErrorCode | null>(null),
    sessions: ref<UserSession[]>(sessions),
    sessionsNextCursor: ref<string | null>(nextCursor),
    sessionsLoading: ref(loading),
    sessionsError: ref<AuthErrorCode | null>(error),
    refresh: vi.fn(),
    login: vi.fn(),
    changePassword: vi.fn(),
    logout: vi.fn(),
    listSessions: vi.fn().mockResolvedValue(true),
    revokeSession: vi.fn().mockResolvedValue(true),
    revokeOtherSessions: vi.fn().mockResolvedValue(true),
    revokeOthers: vi.fn().mockResolvedValue(true),
  }
}

describe('SessionsView component', () => {
  beforeEach(() => {
    localStorage.clear()
    document.documentElement.removeAttribute('lang')
  })
  afterEach(() => {
    vi.unstubAllGlobals()
    localStorage.clear()
    document.documentElement.removeAttribute('lang')
  })

  it('renders empty sessions message when sessions list is empty and not loading', () => {
    const auth = fakeAuthStore([], null, false, null)
    const i18n = createI18n()
    render(SessionsView, {
      props: {
        auth,
        locale: i18n.locale.value,
        t: i18n.t,
      },
    })

    expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy()
    expect(screen.getByText('无活跃会话')).toBeTruthy()
  })

  it('renders sessions list with current badge and creation time', () => {
    const sessions: UserSession[] = [
      { id: '1'.repeat(64), current: true, created_time: '2026-10-05T10:00:00Z' },
      { id: '2'.repeat(64), current: false, created_time: '2026-10-05T11:00:00Z' },
    ]
    const auth = fakeAuthStore(sessions, 'next-cursor-123')
    const i18n = createI18n()
    render(SessionsView, {
      props: {
        auth,
        locale: i18n.locale.value,
        t: i18n.t,
      },
    })

    expect(screen.getByText('当前会话')).toBeTruthy()
    expect(screen.getByText('2026-10-05T10:00:00Z')).toBeTruthy()
    expect(screen.getByText('2026-10-05T11:00:00Z')).toBeTruthy()
    expect(screen.getByRole('button', { name: '注销其他会话' })).toBeTruthy()
    expect(screen.getAllByRole('button', { name: '注销此会话' }).length).toBe(2)
    expect(screen.getByRole('button', { name: '加载更多' })).toBeTruthy()
  })

  it('calls revokeSession and revokeOtherSessions when clicking respective buttons', async () => {
    const sessions: UserSession[] = [
      { id: '1'.repeat(64), current: true, created_time: '2026-10-05T10:00:00Z' },
      { id: '2'.repeat(64), current: false, created_time: '2026-10-05T11:00:00Z' },
    ]
    const auth = fakeAuthStore(sessions)
    const i18n = createI18n()
    render(SessionsView, {
      props: {
        auth,
        locale: i18n.locale.value,
        t: i18n.t,
      },
    })

    const revokeButtons = screen.getAllByRole('button', { name: '注销此会话' })
    await fireEvent.click(revokeButtons[1]!)
    expect(auth.revokeSession).toHaveBeenCalledWith('2'.repeat(64))

    await fireEvent.click(screen.getByRole('button', { name: '注销其他会话' }))
    expect(auth.revokeOtherSessions).toHaveBeenCalledOnce()
  })

  it('calls listSessions with cursor when clicking load more', async () => {
    const sessions: UserSession[] = [
      { id: '1'.repeat(64), current: true, created_time: '2026-10-05T10:00:00Z' },
    ]
    const auth = fakeAuthStore(sessions, 'next-cursor-xyz')
    const i18n = createI18n()
    render(SessionsView, {
      props: {
        auth,
        locale: i18n.locale.value,
        t: i18n.t,
      },
    })

    await fireEvent.click(screen.getByRole('button', { name: '加载更多' }))
    expect(auth.listSessions).toHaveBeenCalledWith('next-cursor-xyz')
  })
})

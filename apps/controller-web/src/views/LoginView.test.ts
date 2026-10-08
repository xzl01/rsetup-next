import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import LoginView from './LoginView.vue'
import { createI18n } from '../i18n'
import type { AuthStore, AuthStatus, AuthErrorCode } from '../auth'

function fakeAuthStore(status: AuthStatus = 'signed_out'): AuthStore {
  return {
    status: ref(status),
    user: ref(null),
    csrfToken: ref(null),
    authzEpoch: ref(null),
    errorCode: ref<AuthErrorCode | null>(null),
    sessions: ref([]),
    sessionsNextCursor: ref(null),
    sessionsLoading: ref(false),
    sessionsError: ref(null),
    refresh: vi.fn(),
    login: vi.fn().mockResolvedValue(true),
    changePassword: vi.fn(),
    logout: vi.fn(),
    listSessions: vi.fn(),
    revokeSession: vi.fn(),
    revokeOtherSessions: vi.fn(),
    revokeOthers: vi.fn(),
  }
}

describe('LoginView component', () => {
  beforeEach(() => {
    localStorage.clear()
    document.documentElement.removeAttribute('lang')
  })
  afterEach(() => {
    vi.unstubAllGlobals()
    localStorage.clear()
    document.documentElement.removeAttribute('lang')
  })

  it('renders login form with username and password inputs and submits credentials', async () => {
    const auth = fakeAuthStore('signed_out')
    const i18n = createI18n()
    render(LoginView, {
      props: {
        auth,
        t: i18n.t,
      },
    })

    expect(screen.getByRole('form')).toBeTruthy()
    expect(screen.getByRole('heading', { name: '登录' })).toBeTruthy()
    await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'admin')
    await fireEvent.update(screen.getByLabelText('密码') as HTMLInputElement, 'secret123')
    await fireEvent.submit(screen.getByRole('form'))

    expect(auth.login).toHaveBeenCalledWith('admin', 'secret123')
    // Clears password input after submission
    expect((screen.getByLabelText('密码') as HTMLInputElement).value).toBe('')
  })

  it('shows mapped error alert when login fails and preserves username', async () => {
    const auth = fakeAuthStore('signed_out')
    auth.login = vi.fn().mockImplementation(async () => {
      auth.errorCode.value = 'INVALID_CREDENTIALS'
      return false
    })
    const i18n = createI18n()
    render(LoginView, {
      props: {
        auth,
        t: i18n.t,
      },
    })

    await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'admin')
    await fireEvent.update(screen.getByLabelText('密码') as HTMLInputElement, 'wrong')
    await fireEvent.submit(screen.getByRole('form'))

    await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toContain('用户名或密码不正确'))
    expect((screen.getByRole('textbox', { name: '用户名' }) as HTMLInputElement).value).toBe('admin')
    expect((screen.getByLabelText('密码') as HTMLInputElement).value).toBe('')
  })
})

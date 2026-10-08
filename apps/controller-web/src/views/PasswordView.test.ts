import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ref } from 'vue'
import PasswordView from './PasswordView.vue'
import { createI18n } from '../i18n'
import type { AuthStore, AuthStatus, AuthErrorCode } from '../auth'

function fakeAuthStore(status: AuthStatus = 'force_password'): AuthStore {
  return {
    status: ref(status),
    user: ref({
      id: '123e4567-e89b-42d3-a456-426614174000',
      username: 'temp_admin',
      must_change_password: true,
      is_admin: true,
      revision: '1',
    }),
    csrfToken: ref('token'),
    authzEpoch: ref('1'),
    errorCode: ref<AuthErrorCode | null>(null),
    sessions: ref([]),
    sessionsNextCursor: ref(null),
    sessionsLoading: ref(false),
    sessionsError: ref(null),
    refresh: vi.fn(),
    login: vi.fn(),
    changePassword: vi.fn().mockResolvedValue(true),
    logout: vi.fn().mockResolvedValue(undefined),
    listSessions: vi.fn(),
    revokeSession: vi.fn(),
    revokeOtherSessions: vi.fn(),
    revokeOthers: vi.fn(),
  }
}

describe('PasswordView component', () => {
  beforeEach(() => {
    localStorage.clear()
    document.documentElement.removeAttribute('lang')
  })
  afterEach(() => {
    vi.unstubAllGlobals()
    localStorage.clear()
    document.documentElement.removeAttribute('lang')
  })

  it('renders forced password change form and submits current and new passwords', async () => {
    const auth = fakeAuthStore('force_password')
    const i18n = createI18n()
    render(PasswordView, {
      props: {
        auth,
        t: i18n.t,
      },
    })

    expect(screen.getByRole('form')).toBeTruthy()
    expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy()
    expect(screen.getByText('当前为临时密码，必须先修改密码才能继续')).toBeTruthy()

    await fireEvent.update(screen.getByLabelText('当前密码') as HTMLInputElement, 'old-pass-123')
    await fireEvent.update(screen.getByLabelText('新密码') as HTMLInputElement, 'new-brand-pass-456')
    await fireEvent.submit(screen.getByRole('form'))

    expect(auth.changePassword).toHaveBeenCalledWith('old-pass-123', 'new-brand-pass-456')
    expect((screen.getByLabelText('当前密码') as HTMLInputElement).value).toBe('')
    expect((screen.getByLabelText('新密码') as HTMLInputElement).value).toBe('')
  })

  it('triggers auth.logout when clicking logout button', async () => {
    const auth = fakeAuthStore('force_password')
    const i18n = createI18n()
    render(PasswordView, {
      props: {
        auth,
        t: i18n.t,
      },
    })

    await fireEvent.click(screen.getByRole('button', { name: '退出登录' }))
    expect(auth.logout).toHaveBeenCalledOnce()
  })

  it('shows mapped error alert when changePassword fails and clears both password inputs', async () => {
    const auth = fakeAuthStore('force_password')
    auth.changePassword = vi.fn().mockImplementation(async () => {
      auth.errorCode.value = 'OTHER'
      return false
    })
    const i18n = createI18n()
    render(PasswordView, {
      props: {
        auth,
        t: i18n.t,
      },
    })

    await fireEvent.update(screen.getByLabelText('当前密码') as HTMLInputElement, 'bad-old')
    await fireEvent.update(screen.getByLabelText('新密码') as HTMLInputElement, 'bad-new')
    await fireEvent.submit(screen.getByRole('form'))

    await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toBe('发生错误，请重试'))
    expect((screen.getByLabelText('当前密码') as HTMLInputElement).value).toBe('')
    expect((screen.getByLabelText('新密码') as HTMLInputElement).value).toBe('')
  })
})

import { fireEvent, render, screen } from '@testing-library/vue';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

import App from './App.vue';

// Contract identity (2026-10-07 HTTP ID design): the /api/v1 user_public projection carries a
// canonical lowercase UUIDv4 id and a decimal-string revision; display_name is a future optional
// field, not part of the current projection — so the UI must fall back to username ('admin').
const USER_ID = '01234567-89ab-4cde-8f01-234567890abc';
const CONTRACT_USER = {
  id: USER_ID,
  username: 'admin',
  active: true,
  is_admin: true,
  must_change_password: false,
  revision: '1',
};

// Normal simulated HTTP packets carry contract request_ids (UUIDv4); api.ts still only validates
// a non-empty bounded string for request_id — that runtime policy is intentionally unchanged.
const RID_ME = '123e4567-e89b-42d3-a456-426614174000';
const RID_ME_2 = '345a6789-fa2c-44d6-b678-648836396222';
const RID_LOGIN = '234f5678-e91b-43c5-a567-537725285111';
const RID_PW = '456b789a-0b3d-45e7-8789-759947407333';
const RID_LOGOUT = '567c89ab-1c4e-46f8-a89a-86aa58518444';
const RID_LIST = '89afbcde-4f7b-49cb-8bcd-b9dd8b84b777';
const RID_REVOKE = '9abcfdef-5a8c-4adc-9cde-c0ee9c95c888';
const RID_ERR = '678d9abc-2d5f-47a9-99ab-97bb69629555';
const RID_NULL = '789eabcd-3e6a-48ba-babc-a8cc7a73a666';
const RID_DEV_DETAIL = 'abcdef01-2345-4678-9abc-def012345678';
const RID_DEV_STATUS = '456789ab-cdef-4123-8456-789abcdef012';

const ME_DATA = {
  user: CONTRACT_USER,
  csrf_token: 'mem-token',
  authz_epoch: '7',
};

const FORCE_ME_DATA = {
  user: { ...CONTRACT_USER, must_change_password: true },
  csrf_token: 'force-token',
  authz_epoch: '7',
};

function jsonResponse(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...headers } });
}

function fetchForMe(meResponse: Response, loginResponse?: (init?: RequestInit) => Response,
  passwordResponse?: (init?: RequestInit) => Response) {
  const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) return meResponse;
    if (url.includes('/auth/login')) return loginResponse ? loginResponse(init) : jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 'login-token' }, request_id: RID_LOGIN });
    if (url.includes('/auth/password')) return passwordResponse ? passwordResponse(init) : jsonResponse({ data: { changed: true }, request_id: RID_PW });
    if (url.includes('/auth/logout')) return jsonResponse({ data: { logged_out: true }, request_id: RID_LOGOUT });
    if (url.includes('/auth/sessions')) return jsonResponse({ data: { items: [], next_cursor: null }, request_id: RID_LIST });
    if (url.includes('/devices')) return jsonResponse({ data: { items: [], next_cursor: null }, request_id: RID_LIST });
    throw new TypeError(`unexpected fetch ${url}`);
  });
  vi.stubGlobal('fetch', fetcher);
  return fetcher;
}

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute('lang');
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
  document.documentElement.removeAttribute('lang');
});

test('checking state: mount triggers exactly one GET /auth/me and shows a safe waiting status without rendering protected content', () => {
  window.location.hash = '#/tasks';
  // 永不 resolve 的 fetch：状态在断言期间确定停留在 checking，无异步竞态。
  const fetcher = vi.fn((_input: RequestInfo | URL) => new Promise<Response>(() => {}));
  vi.stubGlobal('fetch', fetcher);
  render(App);
  expect(screen.getByRole('status').textContent).toContain('正在检查登录状态');
  expect(screen.queryByRole('form')).toBeNull();
  // 保护内容绝不被渲染
  expect(screen.queryByRole('heading', { name: '已登录' })).toBeNull();
  expect(screen.queryByText('尚未连接业务服务')).toBeNull();
  expect(screen.queryByRole('heading', { name: '登录会话' })).toBeNull();
  // 密码框按可访问 label 查询：钉住的 dom-accessibility-api 0.5.x 不把 type=password 映射为 textbox 角色。
  expect(screen.queryByLabelText('密码')).toBeNull();
  expect(fetcher).toHaveBeenCalledOnce();
  expect(String(fetcher.mock.calls[0]?.[0])).toContain('/api/v1/auth/me');
  // hash 路由未被强行修改为 login
  expect(window.location.hash).toBe('#/tasks');
});

test('signed_in state: no forms, shows user and logout; logout returns to the login form, no device data surface', async () => {
  fetchForMe(jsonResponse({ data: ME_DATA, request_id: RID_ME }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  expect(screen.queryByRole('form')).toBeNull();
  expect(screen.getByRole('button', { name: '退出登录' })).toBeTruthy();
  await fireEvent.click(screen.getByRole('button', { name: '退出登录' }));
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
  expect(screen.getByRole('heading', { name: '登录' })).toBeTruthy();
  // 全程无设备/任务/统计占位：容器文本中不出现设备字样
  expect(screen.getByRole('main').textContent).not.toContain('设备');
});

test('force_password after login: only password change and logout are offered', async () => {
  fetchForMe(
    new Response(JSON.stringify({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: RID_ERR }),
      { status: 401, headers: { 'Content-Type': 'application/json' } }),
    () => jsonResponse({ data: { user: { ...CONTRACT_USER, must_change_password: true }, csrf_token: 't' }, request_id: RID_LOGIN }),
  );
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
  await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'admin');
  await fireEvent.update(screen.getByLabelText('密码') as HTMLInputElement, 'synthetic-only');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy());
  expect(screen.getByText('当前为临时密码，必须先修改密码才能继续')).toBeTruthy();
  expect(screen.getByRole('button', { name: '退出登录' })).toBeTruthy();
  expect(screen.queryByRole('textbox', { name: '用户名' })).toBeNull();
});

test('login form: submit posts credentials, error uses the safe mapped message, retry re-submits', async () => {
  let call = 0;
  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: RID_ERR }, 401);
    call += 1;
    if (call === 1) return jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: RID_ERR }, 401);
    return jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 't' }, request_id: RID_LOGIN });
  });
  vi.stubGlobal('fetch', fetcher);
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
  await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'admin');
  await fireEvent.update(screen.getByLabelText('密码') as HTMLInputElement, 'wrong');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toContain('用户名或密码不正确'));
  const body = JSON.parse((fetcher.mock.calls[1]?.[1] as { body: string }).body);
  expect(body).toEqual({ username: 'admin', password: 'wrong' });
  await fireEvent.update(screen.getByLabelText('密码') as HTMLInputElement, 'ok');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  expect(screen.queryByRole('form')).toBeNull();
});

test('login in-flight checking state does NOT unmount LoginView; deferred 401 preserves username, clears password, and shows button loading without leak', async () => {
  let deferredLoginResolve!: (r: Response) => void;
  const deferredLoginPromise = new Promise<Response>((r) => {
    deferredLoginResolve = r;
  });

  const fetcher = vi.fn(async (input: RequestInfo | URL, _init?: RequestInit): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) {
      return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: RID_ERR }, 401);
    }
    if (url.includes('/auth/login')) {
      return deferredLoginPromise;
    }
    throw new TypeError(`unexpected fetch ${url}`);
  });
  vi.stubGlobal('fetch', fetcher);

  render(App);
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());

  await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'testuser');
  await fireEvent.update(screen.getByLabelText('密码') as HTMLInputElement, 'sensitive_pass_123');

  const submitButton = screen.getByRole('button', { name: '登录' });
  await fireEvent.click(submitButton);

  // RED 断言：在 login in-flight 阶段（auth.status === 'checking'），LoginView 不得被卸载成全页 AppNotice！
  // 表单依然存在在文档中
  expect(screen.queryByRole('form')).not.toBeNull();
  expect(screen.getByRole('heading', { name: '登录' })).toBeTruthy();
  // 全局 checking notice 不应该取代登录表单
  expect(screen.queryByText('正在检查登录状态')).toBeNull();

  // 按钮处于 loading 状态（aria-busy="true" 或包含处理中）
  expect(submitButton.getAttribute('aria-busy')).toBe('true');

  // 口令内容绝不泄露到 DOM 其他文本区域
  expect(document.body.textContent).not.toContain('sensitive_pass_123');

  // 现在 resolve 401 INVALID_CREDENTIALS
  deferredLoginResolve(
    jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: RID_ERR }, 401)
  );

  // 等待 401 处理完成
  await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toContain('用户名或密码不正确'));

  // 401 后：用户名保留、密码清空、按钮 loading 恢复
  expect((screen.getByRole('textbox', { name: '用户名' }) as HTMLInputElement).value).toBe('testuser');
  expect((screen.getByLabelText('密码') as HTMLInputElement).value).toBe('');
  expect(submitButton.getAttribute('aria-busy')).toBe('false');
  expect(document.body.textContent).not.toContain('sensitive_pass_123');
});

test('signed_in state: csrf/session stay in memory only, never in localStorage (changePassword 成功路径已由 auth.test.ts 锁定)', async () => {
  let me = 0;
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) {
      me += 1;
      return jsonResponse({ data: { ...ME_DATA, csrf_token: me === 1 ? 'old' : 'new' }, request_id: me === 1 ? RID_ME : RID_ME_2 });
    }
    return jsonResponse({ data: null, request_id: RID_NULL });
  }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  // 已登录视图无表单；改密成功→清 token→me() 重建会话的完整路径由 auth.test.ts 的
  // changePassword 用例锁定；这里只锁定 App 视图映射与敏感数据不落存储。
  expect(screen.queryByRole('form')).toBeNull();
  const joined = Object.keys(localStorage).map((k) => k + localStorage.getItem(k)).join(';');
  expect(joined).not.toContain('old');
  expect(joined).not.toContain('new');
});

test('error state: network failure shows a safe message and a working retry button (no auto retry)', async () => {
  const fetcher = vi.fn(async (): Promise<Response> => { throw new TypeError('offline'); });
  vi.stubGlobal('fetch', fetcher);
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('无法完成操作，请稍后重试'));
  expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
  expect(fetcher).toHaveBeenCalledTimes(1);
  await fireEvent.click(screen.getByRole('button', { name: '重试' }));
  expect(fetcher).toHaveBeenCalledTimes(2);
});

test('language switch translates the auth page and accessible names without re-fetching', async () => {
  const fetcher = vi.fn(() => new Promise<Response>(() => {}));
  vi.stubGlobal('fetch', fetcher);
  render(App);
  expect(screen.getByRole('link', { name: '跳至主要内容' })).toBeTruthy();
  expect(screen.getByRole('status').textContent).toContain('正在检查登录状态');
  await fireEvent.update(screen.getByRole('combobox', { name: '语言' }), 'en');
  expect(screen.getByRole('link', { name: 'Skip to main content' })).toBeTruthy();
  expect(screen.getByRole('combobox', { name: 'Language' })).toBeTruthy();
  expect(screen.getByRole('status').textContent).toContain('Checking your session');
  expect(document.documentElement.lang).toBe('en');
  expect(localStorage.getItem('rsetup.controller.locale')).toBe('en');
  expect(fetcher).toHaveBeenCalledOnce();
});

test('signed_in language switch translates navigation accessible name according to dictionary without hardcoded English', async () => {
  fetchForMe(jsonResponse({ data: ME_DATA, request_id: RID_ME }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));

  // In zh-CN, navigation accessible name must match localized dictionary, not hardcoded English "Account Navigation"
  expect(screen.queryByRole('navigation', { name: 'Account Navigation' })).toBeNull();
  const navZh = screen.getByRole('navigation', { name: '登录会话' });
  expect(navZh).toBeTruthy();

  // Switch language to en
  await fireEvent.update(screen.getByRole('combobox', { name: '语言' }), 'en');
  expect(screen.queryByRole('navigation', { name: 'Account Navigation' })).toBeNull();
  const navEn = screen.getByRole('navigation', { name: 'Active sessions' });
  expect(navEn).toBeTruthy();
});

test('skip-link keyboard activation focuses main element without clobbering existing hash route', async () => {
  window.location.hash = '#/sessions';
  fetchForMe(jsonResponse({ data: ME_DATA, request_id: RID_ME }));
  render(App);

  await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy());
  expect(window.location.hash).toBe('#/sessions');

  const skipLink = screen.getByRole('link', { name: '跳至主要内容' });
  const mainElement = screen.getByRole('main');

  // Trigger keyboard activation (Enter key on skip link)
  await fireEvent.keyDown(skipLink, { key: 'Enter', code: 'Enter' });
  await fireEvent.click(skipLink);

  // Assert main element received focus
  expect(document.activeElement).toBe(mainElement);

  // Assert current route hash is NOT wiped or replaced by #main-content
  expect(window.location.hash).toBe('#/sessions');
  expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy();
});

test('login submit clears the password field on both failed and successful attempts', async () => {
  let call = 0;
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: RID_ERR }, 401);
    call += 1;
    if (call === 1) return jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: RID_ERR }, 401);
    return jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 't' }, request_id: RID_LOGIN });
  }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
  await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'admin');
  const password = screen.getByLabelText('密码') as HTMLInputElement;
  await fireEvent.update(password, 'wrong');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toContain('用户名或密码不正确'));
  // 重新查询：重新渲染后输入框元素可能被替换，锁定的是可见输入框的当前值
  expect((screen.getByLabelText('密码') as HTMLInputElement).value).toBe('');
  await fireEvent.update(screen.getByLabelText('密码') as HTMLInputElement, 'ok');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  expect(screen.queryByLabelText('密码')).toBeNull();
});

test('logout network failure shows the error state with retry, never signed_out', async () => {
  let me = 0;
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) {
      me += 1;
      return jsonResponse({ data: { ...ME_DATA, csrf_token: me === 1 ? 'first' : 'second' }, request_id: me === 1 ? RID_ME : RID_ME_2 });
    }
    if (url.includes('/auth/logout')) throw new TypeError('offline');
    throw new TypeError(`unexpected fetch ${url}`);
  }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  await fireEvent.click(screen.getByRole('button', { name: '退出登录' }));
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('无法完成操作，请稍后重试'));
  expect(screen.queryByRole('form')).toBeNull();
  expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
  await fireEvent.click(screen.getByRole('button', { name: '重试' }));
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  expect(me).toBe(2);
});

// 对已有行为补覆盖（review I-1）：changePassword 的 App 层视图分支。
// 使本测试失败的生产改动：submitPassword 的 !ok 分支不再保留 formError（如误清状态/切视图）；
// 或视图回显服务端 raw 文案（params/message）；或 4xx 分支误恢复 signed_in/signed_out。
test('force_password: wrong old password (400 INVALID_ARGUMENT) keeps the form, shows the fixed local alert, never the raw server message, never success', async () => {
  const RAW = 'SYNTHETIC-RAW-WRONG-OLD-PASSWORD-XYZ';
  const fetcher = fetchForMe(
    jsonResponse({ data: FORCE_ME_DATA, request_id: RID_ME }),
    undefined,
    () => jsonResponse({ error: { code: 'INVALID_ARGUMENT', message_key: 'errors.invalidArgument', params: { detail: RAW } }, request_id: RID_ERR }, 400),
  );
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy());
  await fireEvent.update(screen.getByLabelText('当前密码') as HTMLInputElement, 'wrong-old');
  await fireEvent.update(screen.getByLabelText('新密码') as HTMLInputElement, 'brand-new-password-1');
  await fireEvent.click(screen.getByRole('button', { name: '保存新密码' }));
  // 等待终态（映射文案的 alert 出现）：在途时按钮 loading，可访问名被追加 loading-label，不能以按钮名等待
  await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toBe('发生错误，请重试'));
  // 仍保留强制改密表单（4xx 明确未执行：前态 force_password 不被误清/误恢复）
  expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy();
  expect(screen.getByRole('button', { name: '保存新密码' })).toBeTruthy();
  // role=alert 显示固定本地安全文案：INVALID_ARGUMENT 不在已知码表，映射回退 errors.generic
  expect(screen.getByRole('alert').textContent).toBe('发生错误，请重试');
  // 服务器 raw 文案（经 envelope params 携带）绝不出现在页面上
  expect(document.body.textContent).not.toContain(RAW);
  // 前端不声称成功：无 signed_in 视图
  expect(screen.queryByRole('heading', { name: '已登录' })).toBeNull();
  // 失败后两个口令输入均被清空（submitPassword 的 finally）
  expect((screen.getByLabelText('当前密码') as HTMLInputElement).value).toBe('');
  expect((screen.getByLabelText('新密码') as HTMLInputElement).value).toBe('');
  // 真实到达 /auth/password 且提交内容正确（经 api.post 的 body，而非 mock 计数）
  const body = JSON.parse((fetcher.mock.calls[1]?.[1] as { body: string }).body);
  expect(body).toEqual({ current_password: 'wrong-old', new_password: 'brand-new-password-1' });
  expect(String(fetcher.mock.calls[1]?.[0])).toContain('/api/v1/auth/password');
});

// 对已有行为补覆盖（review I-1）：changePassword 结果未知（网络错误）的 App 层视图分支。
// 使本测试失败的生产改动：changePassword 的未知结果分支恢复前态（幻影 force_password/signed_in）
// 或误置 signed_out（出现登录表单）；或视图声称保存成功；或密码输入残留/口令值落入 localStorage。
test('force_password: network failure on submit is an unknown outcome — error view with retry, never signed_in/login/success, password inputs gone, secrets not in localStorage', async () => {
  let me = 0;
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) {
      me += 1;
      return jsonResponse({ data: FORCE_ME_DATA, request_id: me === 1 ? RID_ME : RID_ME_2 });
    }
    if (url.includes('/auth/password')) throw new TypeError('offline');
    throw new TypeError(`unexpected fetch ${url}`);
  }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy());
  await fireEvent.update(screen.getByLabelText('当前密码') as HTMLInputElement, 'leakcheck-old-9');
  await fireEvent.update(screen.getByLabelText('新密码') as HTMLInputElement, 'leakcheck-new-987654321');
  await fireEvent.click(screen.getByRole('button', { name: '保存新密码' }));
  // 切 error 视图：固定本地安全文案 + 重试
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('无法完成操作，请稍后重试'));
  // 绝不 signed_in（无已登录视图）、绝不 signed_out（无登录表单）、不声称保存成功（无改密表单残留）
  expect(screen.queryByRole('heading', { name: '已登录' })).toBeNull();
  expect(screen.queryByRole('heading', { name: '登录' })).toBeNull();
  expect(screen.queryByRole('form')).toBeNull();
  // 密码输入消失
  expect(screen.queryByLabelText('当前密码')).toBeNull();
  expect(screen.queryByLabelText('新密码')).toBeNull();
  // 提交的口令值不落 localStorage
  const joined = Object.keys(localStorage).map((k) => k + (localStorage.getItem(k) ?? '')).join(';');
  expect(joined).not.toContain('leakcheck-old-9');
  expect(joined).not.toContain('leakcheck-new-987654321');
  // 可重试：重试重新发 me 并回到改密表单
  await fireEvent.click(screen.getByRole('button', { name: '重试' }));
  await vi.waitFor(() => expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy());
  expect(me).toBe(2);
});

describe('signed_in session management UI (Task 5)', () => {
  beforeEach(() => {
    window.location.hash = '#/sessions';
  });
  afterEach(() => {
    window.location.hash = '';
  });
  const SESSIONS_MOCK = {
    items: [
      { id: '1'.repeat(64), current: true, created_time: '2026-10-05T10:00:00Z' },
      { id: '2'.repeat(64), current: false, created_time: '2026-10-05T11:00:00Z' },
    ],
    next_cursor: 'c'.repeat(132),
  };

  test('signed_in state loads and displays active session list with current indicator and created time, but no device names', async () => {
    let sessionsFetchCount = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/sessions')) {
        sessionsFetchCount++;
        return jsonResponse({ data: SESSIONS_MOCK, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    render(App);
    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy());

    expect(sessionsFetchCount).toBe(1);
    expect(screen.getByText('当前会话')).toBeTruthy();
    expect(screen.getByText('2026-10-05T10:00:00Z')).toBeTruthy();
    expect(screen.getByText('2026-10-05T11:00:00Z')).toBeTruthy();

    // Spec constraint: 不凭空推断“设备”，不显示不存在的设备名
    const mainText = screen.getByRole('main').textContent ?? '';
    expect(mainText).not.toContain('设备名');
    expect(mainText).not.toContain('Device');
    expect(mainText).not.toContain('device');

    // Action buttons visible
    expect(screen.getByRole('button', { name: '注销其他会话' })).toBeTruthy();
    const revokeButtons = screen.getAllByRole('button', { name: '注销此会话' });
    expect(revokeButtons.length).toBe(2);

    // Next cursor button visible
    expect(screen.getByRole('button', { name: '加载更多' })).toBeTruthy();
  });

  test('force_password state NEVER renders session list or revoke buttons', async () => {
    let sessionsFetched = false;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: FORCE_ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/sessions')) {
        sessionsFetched = true;
        return jsonResponse({ data: SESSIONS_MOCK, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    render(App);
    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy());

    expect(sessionsFetched).toBe(false);
    expect(screen.queryByRole('heading', { name: '登录会话' })).toBeNull();
    expect(screen.queryByRole('button', { name: '注销其他会话' })).toBeNull();
    expect(screen.queryByRole('button', { name: '注销此会话' })).toBeNull();
    expect(screen.queryByRole('button', { name: '加载更多' })).toBeNull();
  });

  test('revoking another session triggers POST with memory CSRF, remains signed_in, and refreshes session list', async () => {
    let listCount = 0;
    const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/sessions?limit=50')) {
        listCount++;
        return jsonResponse({ data: SESSIONS_MOCK, request_id: RID_LIST });
      }
      if (url.includes(`/auth/sessions/${'2'.repeat(64)}/revoke`)) {
        const headers = (init?.headers ?? {}) as Record<string, string>;
        expect(headers['X-CSRF-Token']).toBe('mem-token');
        expect(JSON.parse(init?.body as string)).toEqual({});
        return jsonResponse({ data: { revoked: true }, request_id: RID_REVOKE });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    });
    vi.stubGlobal('fetch', fetcher);

    render(App);
    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy());
    expect(listCount).toBe(1);

    const revokeButtons = screen.getAllByRole('button', { name: '注销此会话' });
    // Click revoke on the other session (second button)
    await fireEvent.click(revokeButtons[1]!);

    await vi.waitFor(() => expect(listCount).toBe(2));
    expect(screen.getByRole('status').textContent).toContain('admin');
    expect(screen.queryByRole('form')).toBeNull();
  });

  test('revoking current session transitions to signed_out (login form)', async () => {
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_MOCK, request_id: RID_LIST });
      if (url.includes(`/auth/sessions/${'1'.repeat(64)}/revoke`)) {
        const headers = (init?.headers ?? {}) as Record<string, string>;
        expect(headers['X-CSRF-Token']).toBe('mem-token');
        return jsonResponse({ data: { revoked: true }, request_id: RID_REVOKE });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    render(App);
    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy());

    const revokeButtons = screen.getAllByRole('button', { name: '注销此会话' });
    // First button is current session
    await fireEvent.click(revokeButtons[0]!);

    await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
    expect(screen.getByRole('heading', { name: '登录' })).toBeTruthy();
    expect(screen.queryByRole('heading', { name: '登录会话' })).toBeNull();
  });

  test('clicking load more requests next page with cursor and displays appended sessions safely', async () => {
    let call = 0;
    const PAGE_2 = {
      items: [
        { id: '3'.repeat(64), current: false, created_time: '2026-10-05T12:00:00Z' },
      ],
      next_cursor: null,
    };
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/sessions')) {
        call++;
        if (call === 1) return jsonResponse({ data: SESSIONS_MOCK, request_id: RID_LIST });
        expect(url).toContain(`cursor=${'c'.repeat(132)}`);
        return jsonResponse({ data: PAGE_2, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    render(App);
    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy());
    expect(screen.queryByText('2026-10-05T12:00:00Z')).toBeNull();

    const loadMoreBtn = screen.getByRole('button', { name: '加载更多' });
    await fireEvent.click(loadMoreBtn);

    await vi.waitFor(() => expect(screen.getByText('2026-10-05T12:00:00Z')).toBeTruthy());
    // next_cursor became null -> load more button disappears
    expect(screen.queryByRole('button', { name: '加载更多' })).toBeNull();
  });

  // I-2: sessionsError 完全未渲染且失败清空显示“无活跃会话”，撤销失败无反馈
  // 必须渲染 sessionsError 并且提供重试按钮，绝不伪装“无活跃会话”空结果
  test('I-2: listSessions 503 error renders error notice with working retry button and NEVER disguises as empty sessions', async () => {
    let sessionsFetchCount = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/sessions')) {
        sessionsFetchCount++;
        if (sessionsFetchCount === 1) {
          return jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: RID_ERR }, 503);
        }
        return jsonResponse({ data: SESSIONS_MOCK, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    render(App);
    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy());

    // 绝不伪装显示“无活跃会话”！
    expect(screen.queryByText('无活跃会话')).toBeNull();

    // 必须显示错误反馈信息与重试按钮
    await vi.waitFor(() => expect(screen.getByText('无法完成操作，请稍后重试')).toBeTruthy());
    const retryButtons = screen.getAllByRole('button', { name: '重试' });
    expect(retryButtons.length).toBeGreaterThan(0);

    // 点击会话重试按钮可以重新发起请求并成功加载列表
    await fireEvent.click(retryButtons[retryButtons.length - 1]!);
    await vi.waitFor(() => expect(screen.getByText('2026-10-05T10:00:00Z')).toBeTruthy());
    expect(screen.queryByText('无活跃会话')).toBeNull();
    expect(sessionsFetchCount).toBe(2);
  });

  test('I-2 & Minor: revoke failure displays error message, does not claim success, and supports per-item loading state', async () => {
    let revokeCalls = 0;
    let deferredRevokeResolve!: (r: Response) => void;
    const deferredRevokeGate = new Promise<Response>((r) => { deferredRevokeResolve = r });

    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/sessions?limit=50')) return jsonResponse({ data: SESSIONS_MOCK, request_id: RID_LIST });
      if (url.includes(`/auth/sessions/${'2'.repeat(64)}/revoke`)) {
        revokeCalls++;
        return deferredRevokeGate;
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    render(App);
    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录会话' })).toBeTruthy());

    const revokeButtons = screen.getAllByRole('button', { name: '注销此会话' });
    const targetRevokeBtn = revokeButtons[1]!;

    // 点击注销
    await fireEvent.click(targetRevokeBtn);

    // Minor: 单项撤销 loading：该按钮应当处于 loading 状态（aria-busy 或带有 loading-label）
    expect(targetRevokeBtn.getAttribute('aria-busy')).toBe('true');

    // 模拟服务端返回 500 / 503 失败
    deferredRevokeResolve(jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: RID_ERR }, 503));

    // 出现错误提示，不伪称成功，保持 signed_in
    await vi.waitFor(() => expect(screen.getByText('无法完成操作，请稍后重试')).toBeTruthy());
    expect(screen.getByRole('status').textContent).toContain('admin');
    expect(screen.getByText('2026-10-05T11:00:00Z')).toBeTruthy();
  });

  test('signed_in devices view: integrates DevicesView and allows navigating to device-detail with canonical hex64 id', async () => {
    const DEV_ID = 'e'.repeat(64);
    const mockDevices = [
      {
        device_id: DEV_ID,
        display_name: 'Device Echo',
        effective_permissions: ['device.read'],
      },
    ];
    const mockDetail = {
      device_id: DEV_ID,
      display_name: 'Device Echo',
      effective_permissions: ['device.read'],
      admission_state: 'APPROVED',
      review_decision: 'approved',
      connection_state: 'online',
      control_health: 'healthy',
      data_health: 'healthy',
      capabilities: ['reboot'],
      revision: '1',
    };

    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/devices/') && !url.includes('/status')) {
        return jsonResponse({ data: mockDetail, request_id: RID_DEV_DETAIL });
      }
      if (url.includes('/devices')) {
        return jsonResponse({ data: { items: mockDevices, next_cursor: null }, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
    await vi.waitFor(() => expect(screen.getByText('Device Echo')).toBeTruthy());
    expect(screen.getByText(DEV_ID)).toBeTruthy();

    // 点击设备名称进入设备详情路由
    await fireEvent.click(screen.getByText('Device Echo'));

    await vi.waitFor(() => expect(window.location.hash).toBe(`#/devices/${DEV_ID}`));
    await vi.waitFor(() => expect(screen.getByTestId('detail-profile-card')).toBeTruthy());
    expect(screen.getByTestId('dim-admission').textContent).toContain('APPROVED');
  });

  test('Critical 1: user with READ + STATUS permissions fetches BOTH detail and status on device-detail route', async () => {
    const DEV_ID = 'f'.repeat(64);
    const mockDevices = [
      {
        device_id: DEV_ID,
        display_name: 'Device Foxtrot',
        effective_permissions: ['device.read', 'device.status.read'],
      },
    ];
    const mockDetail = {
      device_id: DEV_ID,
      display_name: 'Device Foxtrot',
      effective_permissions: ['device.read', 'device.status.read'],
      admission_state: 'APPROVED',
      review_decision: 'approved',
      connection_state: 'online',
      control_health: 'healthy',
      data_health: 'healthy',
      capabilities: ['reboot'],
      revision: '1',
    };
    const mockStatus = {
      snapshot: { clock_quality: 'observed' },
      received_time: {
        quality: 'ntp_valid',
        system_wall_utc: '2026-10-07T00:00:00Z',
        reference_utc: '2026-10-07T00:00:00Z',
      },
      freshness: 'fresh',
      age_ms: '50',
    };

    const fetchedUrls: string[] = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      fetchedUrls.push(url);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes(`/devices/${DEV_ID}/status`)) {
        return jsonResponse({ data: mockStatus, request_id: RID_DEV_STATUS });
      }
      if (url.includes(`/devices/${DEV_ID}`)) {
        return jsonResponse({ data: mockDetail, request_id: RID_DEV_DETAIL });
      }
      if (url.includes('/devices')) {
        return jsonResponse({ data: { items: mockDevices, next_cursor: null }, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByText('Device Foxtrot')).toBeTruthy());
    // Click button to navigate to detail
    await fireEvent.click(screen.getByText('Device Foxtrot'));

    await vi.waitFor(() => expect(screen.getByTestId('detail-profile-card')).toBeTruthy());
    await vi.waitFor(() => expect(screen.getByTestId('status-card')).toBeTruthy());
    expect(fetchedUrls.some(u => u.includes(`/devices/${DEV_ID}/status`))).toBe(true);
    expect(fetchedUrls.some(u => u.endsWith(`/devices/${DEV_ID}`))).toBe(true);
  });

  test('Critical 2: status-only user never fetches profile detail GET; reboot-only never fetches detail or status GET', async () => {
    const DEV_STATUS_ONLY = '1'.repeat(64);
    const DEV_REBOOT_ONLY = '2'.repeat(64);
    const mockDevices = [
      {
        device_id: DEV_STATUS_ONLY,
        display_name: 'Status Only Dev',
        effective_permissions: ['device.status.read'],
      },
      {
        device_id: DEV_REBOOT_ONLY,
        display_name: 'Reboot Only Dev',
        effective_permissions: ['device.reboot'],
      },
    ];
    const mockStatus = {
      snapshot: { clock_quality: 'observed' },
      received_time: {
        quality: 'ntp_valid',
        system_wall_utc: '2026-10-07T00:00:00Z',
        reference_utc: '2026-10-07T00:00:00Z',
      },
      freshness: 'fresh',
      age_ms: '100',
    };

    const fetchedUrls: string[] = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      fetchedUrls.push(url);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes(`/devices/${DEV_STATUS_ONLY}/status`)) {
        return jsonResponse({ data: mockStatus, request_id: RID_DEV_STATUS });
      }
      if (url.includes(`/devices/${DEV_STATUS_ONLY}`)) {
        throw new Error('STATUS_ONLY MUST NEVER ISSUE DETAIL GET');
      }
      if (url.includes(`/devices/${DEV_REBOOT_ONLY}`)) {
        throw new Error('REBOOT_ONLY MUST NEVER ISSUE DETAIL OR STATUS GET');
      }
      if (url.includes('/devices')) {
        return jsonResponse({ data: { items: mockDevices, next_cursor: null }, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByText('Status Only Dev')).toBeTruthy());
    await fireEvent.click(screen.getByText('Status Only Dev'));

    await vi.waitFor(() => expect(screen.getByTestId('status-card')).toBeTruthy());
    expect(screen.queryByTestId('detail-profile-card')).toBeNull();
    expect(fetchedUrls.some(u => u.endsWith(`/devices/${DEV_STATUS_ONLY}`))).toBe(false);
  });

  test('Critical 3: deep link without known permission projection safely redirects or fails closed without speculative detail GET', async () => {
    const DEV_DEEP = '3'.repeat(64);
    const fetchedUrls: string[] = [];
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      fetchedUrls.push(url);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes(`/devices/${DEV_DEEP}`)) {
        throw new Error('DEEP LINK MUST NEVER ISSUE SPECULATIVE DETAIL GET');
      }
      if (url.includes('/devices')) {
        return jsonResponse({ data: { items: [], next_cursor: null }, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = `#/devices/${DEV_DEEP}`;
    render(App);

    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
    // Should safely fallback to devices list, not probing /devices/:id
    await vi.waitFor(() => expect(window.location.hash).toBe('#/devices'));
    expect(fetchedUrls.some(u => u.includes(`/devices/${DEV_DEEP}`))).toBe(false);
  });

  test('Critical 4: device list 503 error renders error notice with retry button and does not disguise as empty list', async () => {
    let devFetchCount = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/devices')) {
        devFetchCount++;
        if (devFetchCount === 1) {
          return jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: RID_ERR }, 503);
        }
        return jsonResponse({
          data: {
            items: [{ device_id: '4'.repeat(64), display_name: 'Device Four', effective_permissions: ['device.read'] }],
            next_cursor: null,
          },
          request_id: RID_LIST,
        });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
    // Never show empty list notice!
    expect(screen.queryByText('暂无可见设备')).toBeNull();

    // Must show error and retry button
    await vi.waitFor(() => expect(screen.getAllByText('无法加载内容').length).toBeGreaterThan(0));
    const retryBtn = screen.getByRole('button', { name: '重试' });
    await fireEvent.click(retryBtn);

    await vi.waitFor(() => expect(screen.getByText('Device Four')).toBeTruthy());
    expect(screen.queryAllByText('无法加载内容').length).toBe(0);
    expect(devFetchCount).toBe(2);
  });

  test('Critical 5: logout / relogin clears devices list and in-flight requests cannot resurface for new user', async () => {
    let slowListResolve!: (r: Response) => void;
    const slowListPromise = new Promise<Response>((r) => { slowListResolve = r; });

    let fetchCount = 0;
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/auth/logout')) return jsonResponse({ data: { logged_out: true }, request_id: RID_LOGOUT });
      if (url.includes('/devices')) {
        fetchCount++;
        if (fetchCount === 1) {
          return slowListPromise;
        }
        return jsonResponse({
          data: {
            items: [{ device_id: '5'.repeat(64), display_name: 'User2 Device', effective_permissions: ['device.read'] }],
            next_cursor: null,
          },
          request_id: RID_LIST,
        });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));

    // User logs out before slow list returns
    const logoutBtn = screen.getByRole('button', { name: '退出登录' });
    await fireEvent.click(logoutBtn);

    // Old list resolves after logout
    slowListResolve(jsonResponse({
      data: {
        items: [{ device_id: '6'.repeat(64), display_name: 'Stale User1 Device', effective_permissions: ['device.read'] }],
        next_cursor: null,
      },
      request_id: RID_LIST,
    }));

    await vi.waitFor(() => expect(screen.getByRole('heading', { name: '登录' })).toBeTruthy());
    // Stale device must not be rendered anywhere
    expect(screen.queryByText('Stale User1 Device')).toBeNull();
  });

  test('Important: GET /devices returning malformed 200 (e.g. non-hex64 device_id) triggers safe error state, not crash or corrupted list', async () => {
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes('/devices')) {
        return jsonResponse({
          data: {
            items: [
              {
                device_id: 'not-hex64',
                display_name: 'Corrupted Device',
                effective_permissions: ['device.read'],
              },
            ],
            next_cursor: null,
          },
          request_id: RID_LIST,
        });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
    // Should show error notice, never render Corrupted Device
    await vi.waitFor(() => expect(screen.getAllByText('无法加载内容').length).toBeGreaterThan(0));
    expect(screen.queryByText('Corrupted Device')).toBeNull();
  });

  test('Important: GET /devices/:id returning malformed revision triggers safe error state in detail view', async () => {
    const DEV_ID = '7'.repeat(64);
    const mockDevices = [
      {
        device_id: DEV_ID,
        display_name: 'Device Seven',
        effective_permissions: ['device.read'],
      },
    ];
    const malformedDetail = {
      device_id: DEV_ID,
      display_name: 'Device Seven',
      effective_permissions: ['device.read'],
      admission_state: 'APPROVED',
      review_decision: 'approved',
      connection_state: 'online',
      control_health: 'healthy',
      data_health: 'healthy',
      capabilities: ['reboot'],
      revision: 12345, // malformed: number instead of canonical decimal string
    };

    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes(`/devices/${DEV_ID}`)) {
        return jsonResponse({ data: malformedDetail, request_id: RID_DEV_DETAIL });
      }
      if (url.includes('/devices')) {
        return jsonResponse({ data: { items: mockDevices, next_cursor: null }, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByText('Device Seven')).toBeTruthy());
    await fireEvent.click(screen.getByText('Device Seven'));

    // Should show safe error in detail view, not corrupted profile card
    await vi.waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
    expect(screen.getByText('无法加载内容')).toBeTruthy();
    expect(screen.queryByTestId('detail-profile-card')).toBeNull();
  });

  test('Important: GET /devices/:id/status returning numeric age_ms triggers safe error state', async () => {
    const DEV_ID = '8'.repeat(64);
    const mockDevices = [
      {
        device_id: DEV_ID,
        display_name: 'Device Eight',
        effective_permissions: ['device.status.read'],
      },
    ];
    const malformedStatus = {
      snapshot: { clock_quality: 'observed' },
      received_time: {
        quality: 'ntp_valid',
        system_wall_utc: '2026-10-07T00:00:00Z',
        reference_utc: '2026-10-07T00:00:00Z',
      },
      freshness: 'fresh',
      age_ms: 12345, // malformed: number instead of decimal string
    };

    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
      const url = String(input);
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: RID_ME });
      if (url.includes(`/devices/${DEV_ID}/status`)) {
        return jsonResponse({ data: malformedStatus, request_id: RID_DEV_STATUS });
      }
      if (url.includes('/devices')) {
        return jsonResponse({ data: { items: mockDevices, next_cursor: null }, request_id: RID_LIST });
      }
      throw new TypeError(`unexpected fetch ${url}`);
    }));

    window.location.hash = '#/devices';
    render(App);

    await vi.waitFor(() => expect(screen.getByText('Device Eight')).toBeTruthy());
    await fireEvent.click(screen.getByText('Device Eight'));

    // Should show safe error in detail view, not corrupted status card
    await vi.waitFor(() => expect(screen.getByRole('alert')).toBeTruthy());
    expect(screen.getByText('无法加载内容')).toBeTruthy();
    expect(screen.queryByTestId('status-card')).toBeNull();
  });
});


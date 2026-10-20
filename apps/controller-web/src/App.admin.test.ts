import { fireEvent, render, screen } from '@testing-library/vue';
import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

import App from './App.vue';

const USER_ID = '01234567-89ab-4cde-8f01-234567890abc';
const RID = '123e4567-e89b-42d3-a456-426614174000';
const DEVICE_A = 'a'.repeat(64);
const GROUP_ID = '01234567-89ab-4def-8f01-234567890abe';
const EPOCH = '01234567-89ab-4def-8f01-234567890ac2';
const EVENT_ID = '01234567-89ab-4def-8f01-234567890ac3';

function user(isAdmin: boolean) {
  return { id: USER_ID, username: isAdmin ? 'admin' : 'ops', active: true, is_admin: isAdmin, must_change_password: false, revision: '1' };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json' } });
}

type Handler = (callCount: number) => Response;

// [hash, 导航入口文案, 页面标题]：导航用短标签，页面标题用全称。
const ADMIN_ROUTES: Array<[string, string, string]> = [
  ['#/admin/approvals', '准入审批', '准入审批'],
  ['#/admin/groups', '设备组', '设备组'],
  ['#/admin/users', '用户', '用户管理'],
  ['#/admin/roles', '角色', '角色与权限'],
  ['#/admin/grants', '授权', '授权关系'],
  ['#/admin/audit', '审计日志', '审计日志'],
  ['#/admin/system', '系统状态', '系统状态'],
];

function stubBackend(admin: boolean, overrides: Record<string, Handler> = {}) {
  const counts = new Map<string, number>();
  const defaults: Record<string, Response> = {
    '/api/v1/auth/me': jsonResponse({ data: { user: user(admin), csrf_token: 'admin-csrf', authz_epoch: '7' }, request_id: RID }),
    '/api/v1/devices': jsonResponse({ data: { items: [], next_cursor: null }, request_id: RID }),
    '/api/v1/auth/logout': jsonResponse({ data: { logged_out: true }, request_id: RID }),
    '/api/v1/auth/sessions': jsonResponse({ data: { items: [], next_cursor: null }, request_id: RID }),
    '/api/v1/approvals': jsonResponse({
      data: {
        items: [{
          device_id: DEVICE_A, fingerprint: DEVICE_A, descriptor: null, decision: 'none', connection_state: 'online', revision: '3',
        }],
        next_cursor: null,
      },
      request_id: RID,
    }),
    '/api/v1/groups': jsonResponse({ data: { items: [{ id: GROUP_ID, name: 'rack-a', archived: false, revision: '1', member_count: '2' }], next_cursor: null }, request_id: RID }),
    '/api/v1/users': jsonResponse({ data: { items: [{ id: USER_ID, username: 'ops', display_name: '运维', active: true, is_admin: false, must_change_password: false, revision: '2' }], next_cursor: null }, request_id: RID }),
    '/api/v1/roles': jsonResponse({ data: { items: [{ id: GROUP_ID, name: 'operator', builtin: false, archived: false, revision: '1', permissions: ['device.read'] }], next_cursor: null }, request_id: RID }),
    '/api/v1/grants': jsonResponse({ data: { items: [], next_cursor: null }, request_id: RID }),
    '/api/v1/audit': jsonResponse({
      data: {
        items: [{
          id: EVENT_ID, actor_kind: 'user', actor_user_id: USER_ID, event_type: 'identity.user.create', target_kind: 'user',
          target_id: USER_ID, params_redacted: '{}', outcome: 'success', process_epoch: EPOCH, event_seq: '17', time_evidence: null,
        }],
        next_cursor: null,
      },
      request_id: RID,
    }),
    '/api/v1/system/status': jsonResponse({
      data: { ready: true, schema_version: 7, version: '1.4.2', recovery_mode: 'normal', counts: { users: '1', devices: '2', roles: '3', grants: '4', groups: '5' } },
      request_id: RID,
    }),
  };
  const fetcher = vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const path = new URL(String(input), 'http://localhost').pathname;
    const next = (counts.get(path) ?? 0) + 1;
    counts.set(path, next);
    const override = overrides[path];
    if (override) return override(next);
    const response = defaults[path];
    if (response) return response;
    throw new TypeError(`unexpected fetch ${String(input)}`);
  });
  vi.stubGlobal('fetch', fetcher);
  return {
    fetcher,
    paths: () => fetcher.mock.calls.map((call) => new URL(String(call[0]), 'http://localhost').pathname),
  };
}

beforeEach(() => {
  localStorage.clear();
  window.location.hash = '';
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
  window.location.hash = '';
  document.documentElement.removeAttribute('lang');
});

describe('App 管理端挂载', () => {
  test('管理员登录后导航出现七个管理入口，非管理员不出现', async () => {
    stubBackend(true);
    const adminView = render(App);
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '准入审批' })).toBeTruthy());
    for (const [, navLabel] of ADMIN_ROUTES) {
      expect(screen.getByRole('button', { name: navLabel })).toBeTruthy();
    }

    // 换会话重新挂载：先撤下管理员视图，避免两个实例的按钮混在同一 DOM 中
    adminView.unmount();
    vi.unstubAllGlobals();
    stubBackend(false);
    render(App);
    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('ops'));
    for (const [, navLabel] of ADMIN_ROUTES) {
      expect(screen.queryByRole('button', { name: navLabel })).toBeNull();
    }
  });

  test('管理员逐个进入七个管理页，各页只请求自己的端点并渲染标题', async () => {
    for (const [hash, , title] of ADMIN_ROUTES) {
      window.location.hash = hash;
      const { paths } = stubBackend(true);
      const { unmount } = render(App);
      await vi.waitFor(() => expect(screen.getByRole('heading', { name: title })).toBeTruthy());
      const adminPaths = paths().filter((path) => path !== '/api/v1/auth/me' && path !== '/api/v1/devices');
      expect(adminPaths.length).toBeGreaterThanOrEqual(1);
      expect(new Set(adminPaths).size).toBe(1);
      expect(adminPaths[0]).not.toBe('/api/v1/devices');
      unmount();
      vi.unstubAllGlobals();
    }
  });

  test('审批页在 App 内拉取待审批集合并提供逐台选择', async () => {
    window.location.hash = '#/admin/approvals';
    const { paths } = stubBackend(true);
    render(App);
    await vi.waitFor(() => expect(screen.getByTestId(`select-${DEVICE_A}`)).toBeTruthy());
    expect(paths()).toContain('/api/v1/approvals');
    expect(screen.getByTestId('revoke-manual-reset-notice')).toBeTruthy();
  });

  test('审计页在 App 内翻页：下一页沿用签名游标', async () => {
    window.location.hash = '#/admin/audit';
    const { fetcher } = stubBackend(true, {
      '/api/v1/audit': (callCount) => jsonResponse({
        data: {
          items: callCount === 1
            ? [{ id: EVENT_ID, actor_kind: 'user', actor_user_id: USER_ID, event_type: 'identity.user.create', target_kind: null, target_id: null, params_redacted: '{}', outcome: 'success', process_epoch: EPOCH, event_seq: '17', time_evidence: null }]
            : [{ id: '01234567-89ab-4def-8f01-234567890ac4', actor_kind: 'system', actor_user_id: null, event_type: 'system.recovery.enter', target_kind: null, target_id: null, params_redacted: '{}', outcome: 'success', process_epoch: EPOCH, event_seq: '18', time_evidence: null }],
          next_cursor: callCount === 1 ? 'signed-cursor-2' : null,
        },
        request_id: RID,
      }),
    });
    render(App);
    await vi.waitFor(() => expect(screen.getByTestId(`audit-${EVENT_ID}`)).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: '加载更多' }));
    await vi.waitFor(() => expect(screen.getByTestId('audit-01234567-89ab-4def-8f01-234567890ac4')).toBeTruthy());
    const auditCalls = fetcher.mock.calls
      .map((call) => String(call[0]))
      .filter((url) => url.includes('/api/v1/audit'));
    expect(auditCalls[1]).toContain('cursor=signed-cursor-2');
  });

  test('非管理员直接访问管理 hash 被守卫送回设备列表，且不发管理请求', async () => {
    window.location.hash = '#/admin/users';
    const { paths } = stubBackend(false);
    render(App);
    await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('ops'));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(window.location.hash).toBe('#/devices');
    expect(paths()).not.toContain('/api/v1/users');
    expect(screen.queryByRole('heading', { name: '用户管理' })).toBeNull();
  });

  test('管理端点失败时在 App 内呈现错误与重试，不静默留白', async () => {
    window.location.hash = '#/admin/system';
    stubBackend(true, {
      '/api/v1/system/status': () => jsonResponse(
        { error: { code: 'STORAGE_UNAVAILABLE', message_key: 'errors.storageUnavailable' }, request_id: RID }, 503,
      ),
    });
    render(App);
    await vi.waitFor(() => expect(screen.getByRole('button', { name: '重试' })).toBeTruthy());
    expect(screen.getByTestId('system-error').textContent).toContain('存储暂时不可用');
    expect(screen.queryByTestId('system-status')).toBeNull();
  });

  test('退出登录后管理入口与保护内容一并撤下', async () => {
    window.location.hash = '#/admin/system';
    stubBackend(true);
    render(App);
    await vi.waitFor(() => expect(screen.getByTestId('system-status')).toBeTruthy());
    await fireEvent.click(screen.getByRole('button', { name: '退出登录' }));
    await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
    expect(screen.queryByRole('button', { name: '系统状态' })).toBeNull();
    expect(screen.queryByTestId('system-status')).toBeNull();
  });
});

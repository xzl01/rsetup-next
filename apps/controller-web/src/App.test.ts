import { fireEvent, render, screen, within } from '@testing-library/vue';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';

import App from './App.vue';

const fetchSpy = vi.fn();

beforeEach(() => {
  localStorage.clear();
  vi.stubGlobal('fetch', fetchSpy);
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
  fetchSpy.mockClear();
  document.documentElement.removeAttribute('lang');
});

test('mounts the bilingual semantic shell and explicitly identifies a disconnected component demo without requesting data', () => {
  render(App);
  expect(screen.getByRole('banner')).toBeTruthy();
  expect(screen.getByRole('main')).toHaveProperty('id', 'main-content');
  expect(screen.getByRole('contentinfo')).toBeTruthy();
  expect(screen.getByRole('heading', { level: 1, name: 'Rsetup Controller' })).toBeTruthy();
  expect(screen.getByRole('link', { name: '跳至主要内容' }).getAttribute('href')).toBe('#main-content');
  expect(screen.getByText('尚未连接业务服务')).toBeTruthy();
  const demo = screen.getByRole('region', { name: '基础组件展示' });
  expect(demo.querySelector(':scope > p')?.textContent).toBe('此工作台仅展示基础组件，尚未连接业务服务。');
  expect(within(demo).getByRole('status').getAttribute('data-tone')).toBe('info');
  expect(within(demo).getByRole('status').textContent).toContain('提示');
  expect(within(demo).getByRole('textbox', { name: '示例输入' })).toBeTruthy();
  expect(within(demo).getByRole('button', { name: /主要操作/ })).toBeTruthy();
  expect(within(demo).getByRole('alert').textContent).toContain('无法加载内容');
  expect(document.documentElement.lang).toBe('zh-CN');
  expect(fetchSpy).not.toHaveBeenCalled();
});

test('switches translated accessible names and document language without clearing the input or fetching', async () => {
  render(App);
  const input = screen.getByRole('textbox', { name: '示例输入' }) as HTMLInputElement;
  await fireEvent.update(input, 'preserved input');
  const selector = screen.getByRole('combobox', { name: '语言' });
  await fireEvent.update(selector, 'en');
  expect(screen.getByRole('link', { name: 'Skip to main content' })).toBeTruthy();
  expect(screen.getByRole('combobox', { name: 'Language' })).toBe(selector);
  expect(screen.getByRole('textbox', { name: 'Sample input' })).toBe(input);
  expect(input.value).toBe('preserved input');
  expect(screen.getByText('Business services are not connected')).toBeTruthy();
  expect(screen.getByRole('region', { name: 'Foundation component showcase' })).toBeTruthy();
  expect(screen.getByRole('status').textContent).toContain('Information');
  expect(screen.getByRole('button', { name: /Primary action/ })).toBeTruthy();
  expect(document.documentElement.lang).toBe('en');
  expect(localStorage.getItem('rsetup.controller.locale')).toBe('en');
  await fireEvent.update(selector, 'zh-CN');
  expect(screen.getByRole('textbox', { name: '示例输入' })).toBe(input);
  expect(input.value).toBe('preserved input');
  expect(document.documentElement.lang).toBe('zh-CN');
  expect(fetchSpy).not.toHaveBeenCalled();
});

test('labels the isolated error alert as a disconnected example in both languages without fetching', async () => {
  render(App);
  const alert = screen.getByRole('alert');
  expect(within(alert).getByText('此工作台仅展示基础组件，尚未连接业务服务。').textContent)
    .toBe('此工作台仅展示基础组件，尚未连接业务服务。');
  expect(fetchSpy).not.toHaveBeenCalled();

  await fireEvent.update(screen.getByRole('combobox', { name: '语言' }), 'en');
  expect(screen.getByRole('alert')).toBe(alert);
  expect(within(alert).getByText('This workspace only demonstrates foundation components. Business services are not connected.').textContent)
    .toBe('This workspace only demonstrates foundation components. Business services are not connected.');
  expect(fetchSpy).not.toHaveBeenCalled();

  await fireEvent.click(within(alert).getByRole('button', { name: 'Retry' }));
  expect(screen.queryByRole('alert')).toBeNull();
  expect(screen.getByText('No content yet').closest('[role="status"]')?.getAttribute('data-state')).toBe('empty');
  expect(fetchSpy).not.toHaveBeenCalled();
});

test('keeps showcase loading disabled and retries a clearly labelled error example only in the UI', async () => {
  const view = render(App);
  const loading = screen.getByRole('button', { name: /处理中/ });
  expect(loading.hasAttribute('disabled')).toBe(true);
  expect(loading.getAttribute('aria-busy')).toBe('true');
  await fireEvent.click(loading);
  const demo = screen.getByRole('region', { name: '基础组件展示' });
  expect(demo.querySelector(':scope > p')?.textContent).toBe('此工作台仅展示基础组件，尚未连接业务服务。');
  expect(within(demo).getByRole('alert').textContent).toContain('无法加载内容');
  await fireEvent.click(screen.getByRole('button', { name: '重试' }));
  expect(within(demo).queryByRole('alert')).toBeNull();
  expect(within(demo).getByText('暂无内容')).toBeTruthy();
  expect(within(demo).queryByRole('button', { name: '重试' })).toBeNull();
  expect(view.container.querySelector('[data-state="empty"]')?.getAttribute('role')).toBe('status');
  expect(fetchSpy).not.toHaveBeenCalled();
});

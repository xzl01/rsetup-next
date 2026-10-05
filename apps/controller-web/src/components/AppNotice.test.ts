import { defineComponent } from 'vue';
import { render, screen } from '@testing-library/vue';
import { expect, test } from 'vitest';

import AppNotice from './AppNotice.vue';

test('error notice exposes alert role and escapes its plain text title', () => {
  const view = render(AppNotice, { props: { tone: 'error', toneLabel: '错误', title: '<img src=x> Failure' }, slots: { default: 'problem' } });
  const notice = screen.getByRole('alert');
  expect(notice.querySelector('.app-notice__tone')?.textContent).toBe('错误');
  expect(notice.textContent).toContain('<img src=x> Failure');
  expect(notice.textContent).toContain('problem');
  expect(view.container.querySelector('img')).toBeNull();
  expect(notice.getAttribute('data-tone')).toBe('error');
});

test.each([
  { tone: 'info', toneLabel: '提示' },
  { tone: 'success', toneLabel: '成功' },
  { tone: 'warning', toneLabel: '警告' },
  { tone: 'info', toneLabel: 'Information' },
  { tone: 'success', toneLabel: 'Success' },
  { tone: 'warning', toneLabel: 'Warning' },
] as const)('$tone notice uses status and caller $toneLabel, not color alone', ({ tone, toneLabel }) => {
  render(AppNotice, { props: { tone, toneLabel }, slots: { default: 'Message' } });
  const notice = screen.getByRole('status');
  expect(notice.textContent).toContain('Message');
  expect(notice.querySelector('.app-notice__tone')?.textContent).toBe(toneLabel);
  expect(notice.getAttribute('data-tone')).toBe(tone);
  expect(notice.classList.contains(`app-notice--${tone}`)).toBe(true);
});

test('default notice uses info status while its visible label comes from the caller', () => {
  render(AppNotice, { props: { toneLabel: 'Information' }, slots: { default: 'Note' } });
  const notice = screen.getByRole('status');
  expect(notice.textContent).toContain('Note');
  expect(notice.querySelector('.app-notice__tone')?.textContent).toBe('Information');
  expect(notice.getAttribute('data-tone')).toBe('info');
});

test('caller-interpolated untrusted slot text stays text; trusted slot markup stays available', () => {
  const untrusted = '<img src=x onerror=alert(1)>';
  const Caller = defineComponent({
    components: { AppNotice },
    setup: () => ({ untrusted }),
    template: '<AppNotice tone="warning" tone-label="警告"><span>{{ untrusted }}</span><em>Trusted markup</em></AppNotice>',
  });
  const view = render(Caller);
  const notice = screen.getByRole('status');
  expect(notice.querySelector('.app-notice__body')?.textContent).toContain(untrusted);
  expect(view.container.querySelector('img')).toBeNull();
  expect(notice.querySelector('em')?.textContent).toBe('Trusted markup');
});

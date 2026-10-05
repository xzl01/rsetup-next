import { fireEvent, render, screen } from '@testing-library/vue';
import { expect, test } from 'vitest';

import AsyncState from './AsyncState.vue';

test('error shows description and an explicitly labeled retry that emits only on click', async () => {
  const view = render(AsyncState, { props: { state: 'error', title: 'Problem', description: 'Try again later', retryLabel: 'Retry' } });
  expect(screen.getByRole('heading', { name: 'Problem' })).toBeTruthy();
  expect(screen.getByText('Try again later')).toBeTruthy();
  expect(view.emitted('retry')).toBeUndefined();
  const retry = screen.getByRole('button', { name: 'Retry' });
  expect(retry.getAttribute('type')).toBe('button');
  await fireEvent.click(retry);
  expect(view.emitted('retry')).toHaveLength(1);
});

test('retryLabel with markup-shaped input stays text in the retry button', () => {
  const payload = '<img src=x onerror=alert(1)>';
  const view = render(AsyncState, { props: { state: 'error', title: 'Failed', retryLabel: payload } });
  const retry = screen.getByRole('button', { name: payload });
  expect(retry.textContent).toBe(payload);
  expect(view.container.querySelector('img')).toBeNull();
});

test.each(['loading', 'empty'] as const)('%s state never exposes retry, even when given a label', (state) => {
  const view = render(AsyncState, { props: { state, title: 'Waiting', retryLabel: 'Retry' } });
  expect(screen.getByRole('status')).toBeTruthy();
  expect(screen.queryByRole('button')).toBeNull();
  expect(view.emitted('retry')).toBeUndefined();
});

test('error without retryLabel has no button and never retries automatically', () => {
  const view = render(AsyncState, { props: { state: 'error', title: 'Failed' } });
  expect(screen.getByRole('alert')).toBeTruthy();
  expect(screen.queryByRole('button')).toBeNull();
  expect(view.emitted('retry')).toBeUndefined();
});

test('changing state removes retry button and preserves text escaping', async () => {
  const view = render(AsyncState, { props: { state: 'error', title: '<img src=x>', retryLabel: 'Retry' } });
  expect(screen.getByRole('heading', { name: '<img src=x>' })).toBeTruthy();
  expect(view.container.querySelector('img')).toBeNull();
  await view.rerender({ state: 'empty' });
  expect(screen.queryByRole('button')).toBeNull();
});

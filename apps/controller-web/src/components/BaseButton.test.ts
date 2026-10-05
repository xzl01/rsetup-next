import { fireEvent, render, screen } from '@testing-library/vue';
import { expect, test } from 'vitest';

import BaseButton from './BaseButton.vue';

test('loading button keeps a safe type, announces progress and cannot emit click', async () => {
  const view = render(BaseButton, { props: { loading: true, loadingLabel: 'Working' }, slots: { default: 'Save' } });
  const button = screen.getByRole('button', { name: /Save/ });
  expect(button.getAttribute('type')).toBe('button');
  expect(button.hasAttribute('disabled')).toBe(true);
  expect(button.getAttribute('aria-busy')).toBe('true');
  expect(button.textContent).toContain('Working');
  await fireEvent.click(button);
  expect(view.emitted().click).toBeUndefined();
});

test('loadingLabel with markup-shaped input remains literal text', () => {
  const payload = '<img src=x onerror=alert(1)>';
  const view = render(BaseButton, { props: { loading: true, loadingLabel: payload }, slots: { default: 'Save' } });
  const button = screen.getByRole('button');
  expect(button.querySelector('.base-button__loading')?.textContent).toBe(payload);
  expect(view.container.querySelector('img')).toBeNull();
});

test('disabled button blocks activation, while enabled button emits click with its native event', async () => {
  const view = render(BaseButton, { props: { disabled: true }, slots: { default: 'Save' } });
  const button = screen.getByRole('button', { name: 'Save' });
  await fireEvent.click(button);
  expect(view.emitted().click).toBeUndefined();
  await view.rerender({ disabled: false });
  expect(button.hasAttribute('disabled')).toBe(false);
  expect(button.getAttribute('aria-busy')).toBe('false');
  await fireEvent.click(button);
  expect(view.emitted('click')).toEqual([[expect.any(MouseEvent)]]);
});

test('explicit submit type and danger variant stay available without changing native semantics', () => {
  render(BaseButton, { props: { type: 'submit', variant: 'danger' }, slots: { default: 'Delete' } });
  const button = screen.getByRole('button', { name: 'Delete' });
  expect(button.getAttribute('type')).toBe('submit');
  expect(button.classList.contains('base-button--danger')).toBe(true);
});

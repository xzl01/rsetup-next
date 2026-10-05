import { fireEvent, render, screen } from '@testing-library/vue';
import { expect, test } from 'vitest';

import BaseInput from './BaseInput.vue';

test('labels the required field, associates error and emits v-model string updates', async () => {
  const view = render(BaseInput, { props: { id: 'name', label: 'Name', modelValue: '', error: 'Required', required: true } });
  const input = screen.getByLabelText('Name') as HTMLInputElement;
  expect(input.required).toBe(true);
  expect(input.getAttribute('aria-invalid')).toBe('true');
  expect(input.getAttribute('aria-describedby')).toContain('name-error');
  expect(screen.getByText('Required').id).toBe('name-error');
  await fireEvent.update(input, 'new');
  expect(view.emitted()['update:modelValue']).toEqual([['new']]);
});

test('hint and error are described together; clearing error also clears invalid state and description', async () => {
  const view = render(BaseInput, { props: { id: 'email', label: 'Email', modelValue: 'old', hint: 'Use work email', error: 'Invalid' } });
  const input = screen.getByLabelText('Email') as HTMLInputElement;
  expect(input.getAttribute('aria-describedby')?.split(' ')).toEqual(['email-hint', 'email-error']);
  expect(screen.getByText('Use work email').id).toBe('email-hint');
  await view.rerender({ error: undefined });
  expect(input.getAttribute('aria-invalid')).not.toBe('true');
  expect(input.getAttribute('aria-describedby')).toBe('email-hint');
  expect(screen.queryByText('Invalid')).toBeNull();
});

test('password and disabled use native input behavior, and a changed model value updates display', async () => {
  const view = render(BaseInput, { props: { id: 'pwd', label: 'Password', modelValue: '', type: 'password', disabled: true } });
  const input = screen.getByLabelText('Password') as HTMLInputElement;
  expect(input.type).toBe('password');
  expect(input.disabled).toBe(true);
  await view.rerender({ modelValue: 'secret', disabled: false });
  expect(input.disabled).toBe(false);
  expect(input.value).toBe('secret');
});

test('untrusted text is rendered literally rather than as markup', () => {
  const view = render(BaseInput, { props: { id: 'x', label: '<svg>Label</svg>', modelValue: '', error: '<img src=x>' } });
  expect(screen.getByLabelText('<svg>Label</svg>')).toBeTruthy();
  expect(screen.getByText('<img src=x>')).toBeTruthy();
  expect(view.container.querySelector('svg, img')).toBeNull();
});

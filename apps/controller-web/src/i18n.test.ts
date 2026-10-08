import { defineComponent, h, isRef, ref, type Ref } from 'vue';
import { fireEvent, render, screen } from '@testing-library/vue';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { createI18n, type Locale } from './i18n';
import zh from './locales/zh-CN';
import en from './locales/en';

const preferenceKey = 'rsetup.controller.locale';
const keys = ['app.title', 'app.foundation', 'app.notConnected', 'nav.skip', 'language.label',
  'button.primary', 'button.secondary', 'button.danger', 'button.loading',
  'field.label', 'field.hint', 'field.error', 'notice.title', 'notice.body',
  'state.loading', 'state.empty', 'state.error', 'state.retry',
  'auth.checking', 'auth.error.generic', 'errors.generic',
  'auth.error.invalidCredentials', 'auth.error.authRequired', 'auth.error.rateLimited',
  'auth.error.passwordChangeRequired', 'auth.error.denied',
  'auth.login.title', 'auth.username.label', 'auth.username.hint', 'auth.password.label',
  'auth.login.submit', 'auth.logout.label', 'auth.passwordChange.title', 'auth.passwordChange.forced',
  'auth.currentPassword.label', 'auth.newPassword.label', 'auth.newPassword.hint',
  'auth.passwordChange.submit', 'auth.signedIn.heading',
  'auth.sessions.title', 'auth.sessions.current', 'auth.sessions.created',
  'auth.sessions.revoke', 'auth.sessions.revokeOthers', 'auth.sessions.loadMore',
  'auth.sessions.empty',
  'common.unknown', 'common.error',
  'nav.devices',
  'device.list.title', 'device.list.search', 'device.list.empty', 'device.list.reboot', 'device.list.loadMore',
  'device.dim.admission', 'device.dim.connection', 'device.dim.streamHealth',
  'device.health.control', 'device.health.data',
  'device.dim.freshness', 'device.status.age', 'device.dim.controllerClock', 'device.dim.boardClock'];

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  localStorage.clear();
  document.documentElement.removeAttribute('lang');
});

describe('createI18n', () => {
  it('exports a Vue locale ref, complete matching dictionaries and exact foundation copy', () => {
    const i18n = createI18n({ storage: null, document });
    const locale: Ref<Locale> = i18n.locale;
    expect(isRef(locale)).toBe(true);
    expect(locale.value).toBe('zh-CN');
    expect(Object.keys(zh).sort()).toEqual([...keys].sort());
    expect(Object.keys(en).sort()).toEqual(Object.keys(zh).sort());
    for (const key of keys) {
      expect(typeof zh[key]).toBe('string');
      expect(zh[key]?.trim()).toBeTruthy();
      expect(en[key]?.trim()).toBeTruthy();
    }
    expect(i18n.t('app.notConnected')).toBe('尚未连接业务服务');
    expect(document.documentElement.lang).toBe('zh-CN');
    i18n.setLocale('en');
    expect(i18n.t('app.notConnected')).toBe('Business services are not connected');
    expect(locale.value).toBe('en');
    expect(document.documentElement.lang).toBe('en');
  });

  it('prefers valid persisted locale and writes only the locale on switching', () => {
    localStorage.setItem(preferenceKey, 'en');
    localStorage.setItem('unrelated', 'keep');
    const i18n = createI18n({ initialLocale: 'zh-CN', storage: localStorage, document });
    expect(i18n.locale.value).toBe('en');
    expect(document.documentElement.lang).toBe('en');
    i18n.setLocale('zh-CN');
    expect(localStorage.getItem(preferenceKey)).toBe('zh-CN');
    expect(localStorage.getItem('unrelated')).toBe('keep');
    expect(localStorage.length).toBe(2);
    expect(createI18n({ storage: localStorage, document: null }).locale.value).toBe('zh-CN');
  });

  it('ignores unknown stored preferences and normalizes runtime invalid locales', () => {
    localStorage.setItem(preferenceKey, 'fr');
    const i18n = createI18n({ initialLocale: 'en', storage: localStorage, document });
    expect(i18n.locale.value).toBe('en');
    i18n.setLocale('invalid' as Locale);
    expect(i18n.locale.value).toBe('zh-CN');
    expect(document.documentElement.lang).toBe('zh-CN');
    expect(localStorage.getItem(preferenceKey)).toBe('zh-CN');
    expect(createI18n({ initialLocale: 'invalid' as Locale, storage: null, document: null }).locale.value).toBe('zh-CN');
  });

  it('uses safe browser defaults when options are omitted', () => {
    localStorage.setItem(preferenceKey, 'en');
    const i18n = createI18n();
    expect(i18n.locale.value).toBe('en');
    expect(document.documentElement.lang).toBe('en');
    i18n.setLocale('zh-CN');
    expect(localStorage.getItem(preferenceKey)).toBe('zh-CN');
  });

  it('null explicitly disables both default globals', () => {
    localStorage.setItem(preferenceKey, 'en');
    document.documentElement.lang = 'untouched';
    const i18n = createI18n({ storage: null, document: null });
    expect(i18n.locale.value).toBe('zh-CN');
    i18n.setLocale('en');
    i18n.setLocale('zh-CN');
    expect(document.documentElement.lang).toBe('untouched');
    expect(localStorage.getItem(preferenceKey)).toBe('en');
  });

  it('continues through storage read and write exceptions', () => {
    const storage = {
      getItem() { throw new Error('denied'); },
      setItem() { throw new Error('quota'); },
    } as unknown as Storage;
    const i18n = createI18n({ initialLocale: 'en', storage, document });
    expect(i18n.locale.value).toBe('en');
    expect(() => i18n.setLocale('zh-CN')).not.toThrow();
    expect(i18n.t('app.notConnected')).toBe('尚未连接业务服务');
    expect(document.documentElement.lang).toBe('zh-CN');
  });

  it('handles unavailable globals without DOM', () => {
    vi.stubGlobal('document', undefined);
    vi.stubGlobal('localStorage', undefined);
    const i18n = createI18n({ initialLocale: 'en' });
    expect(i18n.locale.value).toBe('en');
    i18n.setLocale('zh-CN');
    expect(i18n.t('app.notConnected')).toBe('尚未连接业务服务');
  });

  it('catches exceptions while accessing default globals', () => {
    vi.spyOn(globalThis, 'localStorage', 'get').mockImplementation(() => { throw new Error('blocked'); });
    vi.spyOn(globalThis, 'document', 'get').mockImplementation(() => { throw new Error('blocked'); });
    const i18n = createI18n({ initialLocale: 'en' });
    expect(i18n.locale.value).toBe('en');
    expect(() => i18n.setLocale('zh-CN')).not.toThrow();
    expect(i18n.locale.value).toBe('zh-CN');
  });

  it('falls back from absent English to Chinese to the original key, not prototype properties', () => {
    const original = en['app.notConnected'];
    delete en['app.notConnected'];
    try {
      const i18n = createI18n({ initialLocale: 'en', storage: null, document: null });
      expect(i18n.t('app.notConnected')).toBe('尚未连接业务服务');
      expect(i18n.t('missing.key')).toBe('missing.key');
      expect(i18n.t('toString')).toBe('toString');
      expect(i18n.t('__proto__')).toBe('__proto__');
    } finally {
      en['app.notConnected'] = original!;
    }
  });

  it.each(['zh-CN', 'en'] as const)('returns unknown keys unchanged in %s even with placeholder parameters', (initialLocale) => {
    const i18n = createI18n({ initialLocale, storage: null, document: null });
    const key = 'missing.{name}.{count}.{missing}';
    expect(i18n.t(key, { name: 'x', count: 0 })).toBe(key);
    expect(i18n.t(key)).toBe(key);
    expect(i18n.t(key, {})).toBe(key);
  });

  it('only substitutes own parameters as literal text, leaving missing placeholders unchanged', () => {
    const key = 'test.interpolation';
    zh[key] = '{name}: {count} {missing} {inherited} {replacement}';
    try {
      const i18n = createI18n({ storage: null, document: null });
      const params = Object.assign(Object.create({ inherited: 'unsafe' }) as Record<string, string | number>,
        { name: '<img src=x onerror=alert(1)>', count: 0, replacement: '$&{count}' });
      expect(i18n.t(key, params))
        .toBe('<img src=x onerror=alert(1)>: 0 {missing} {inherited} $&{count}');
      expect(i18n.t(key)).toBe(zh[key]);
      i18n.setLocale('en');
      expect(i18n.t(key, params))
        .toBe('<img src=x onerror=alert(1)>: 0 {missing} {inherited} $&{count}');
    } finally {
      delete zh[key];
    }
  });

  it('reactively updates real Vue text and lang without losing input or making requests', async () => {
    const key = 'test.payload';
    zh[key] = 'Payload: {name}';
    en[key] = 'Payload: {name}';
    try {
      const fetch = vi.fn();
      vi.stubGlobal('fetch', fetch);
      const i18n = createI18n({ storage: null, document });
      const attack = '<img src=x onerror=alert(1)>';
      const view = render(defineComponent({
        setup() {
          const value = ref('');
          return () => h('section', [
            h('p', i18n.t('app.notConnected')),
            h('label', { for: 'sample' }, i18n.t('field.label')),
            h('input', { id: 'sample', value: value.value,
              onInput: (event: Event) => { value.value = (event.target as HTMLInputElement).value; } }),
            h('button', { onClick: () => i18n.setLocale(i18n.locale.value === 'en' ? 'zh-CN' : 'en') }, 'Switch'),
            h('span', i18n.t(key, { name: attack })),
            h('span', i18n.t(attack)),
          ]);
        },
      }));
      const input = screen.getByRole('textbox') as HTMLInputElement;
      await fireEvent.update(input, 'keep my value');
      expect(screen.getByText('尚未连接业务服务')).toBeTruthy();
      await fireEvent.click(screen.getByRole('button', { name: 'Switch' }));
      expect(screen.getByText('Business services are not connected')).toBeTruthy();
      expect(screen.getByLabelText(en['field.label']!)).toBe(input);
      expect(document.documentElement.lang).toBe('en');
      expect(input.value).toBe('keep my value');
      await fireEvent.click(screen.getByRole('button', { name: 'Switch' }));
      expect(screen.getByText('尚未连接业务服务')).toBeTruthy();
      expect(document.documentElement.lang).toBe('zh-CN');
      expect(input.value).toBe('keep my value');
      expect(screen.getByText(`Payload: ${attack}`)).toBeTruthy();
      expect(screen.getByText(attack)).toBeTruthy();
      expect(view.container.querySelector('img')).toBeNull();
      expect(fetch).not.toHaveBeenCalled();
    } finally {
      delete zh[key];
      delete en[key];
    }
  });
});

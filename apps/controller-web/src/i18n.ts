import { ref, type Ref } from 'vue';
import zh from './locales/zh-CN';
import en from './locales/en';

export type Locale = 'zh-CN' | 'en';

type Options = {
  initialLocale?: Locale;
  storage?: Storage | null;
  document?: Document | null;
};

const preferenceKey = 'rsetup.controller.locale';

function isLocale(value: unknown): value is Locale {
  return value === 'zh-CN' || value === 'en';
}

function safely<T>(read: () => T): T | undefined {
  try {
    return read();
  } catch {
    // Browser privacy settings and unavailable DOM/storage must not block text.
    return undefined;
  }
}

function message(dictionary: Record<string, string>, key: string): string | undefined {
  return Object.prototype.hasOwnProperty.call(dictionary, key) ? dictionary[key] : undefined;
}

export function createI18n(options: Options = {}): {
  locale: Ref<Locale>;
  t(key: string, params?: Record<string, string | number>): string;
  setLocale(locale: Locale): void;
} {
  const storage = options.storage === undefined
    ? safely(() => globalThis.localStorage)
    : options.storage;
  const document = options.document === undefined
    ? safely(() => globalThis.document)
    : options.document;
  const preferred = safely(() => storage?.getItem(preferenceKey));
  const initial = isLocale(options.initialLocale) ? options.initialLocale : 'zh-CN';
  const locale = ref<Locale>(isLocale(preferred) ? preferred : initial);

  function syncDocument() {
    safely(() => {
      if (document?.documentElement) document.documentElement.lang = locale.value;
    });
  }
  syncDocument();

  function setLocale(value: Locale): void {
    locale.value = isLocale(value) ? value : 'zh-CN';
    syncDocument();
    safely(() => storage?.setItem(preferenceKey, locale.value));
  }

  function t(key: string, params?: Record<string, string | number>): string {
    const text = (locale.value === 'en' ? message(en, key) : undefined) ?? message(zh, key);
    if (text === undefined) return key;
    return text.replace(/\{([^{}]+)\}/g, (placeholder: string, name: string) =>
      params && Object.prototype.hasOwnProperty.call(params, name)
        ? String(params[name])
        : placeholder);
  }

  return { locale, t, setLocale };
}

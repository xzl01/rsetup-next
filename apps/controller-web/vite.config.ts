/// <reference types="vitest/config" />
import vue from '@vitejs/plugin-vue';
import { defineConfig, type UserConfig } from 'vite';

const LOOPBACK_HOSTS = new Set(['127.0.0.1', 'localhost', '[::1]', '::1']);

/**
 * Validates and normalizes the backend dev proxy target URL.
 * Fails closed if the target is external, contains credentials/query/hash/subpaths,
 * or lacks an explicit port.
 */
export function resolveBackendProxyTarget(rawTarget?: string): string | undefined {
  if (rawTarget === undefined) {
    return undefined;
  }
  const trimmed = rawTarget.trim();
  if (trimmed === '') {
    return undefined;
  }

  let parsed: URL;
  try {
    parsed = new URL(trimmed);
  } catch {
    throw new Error('[vite-proxy] Invalid target URL format');
  }

  if (parsed.protocol !== 'http:') {
    throw new Error('[vite-proxy] Invalid target protocol; only "http:" is allowed');
  }

  if (parsed.username !== '' || parsed.password !== '') {
    throw new Error('[vite-proxy] Target URL must not contain user credentials');
  }

  if (parsed.search !== '') {
    throw new Error('[vite-proxy] Target URL must not contain query parameters');
  }

  if (parsed.hash !== '') {
    throw new Error('[vite-proxy] Target URL must not contain hash fragments');
  }

  if (parsed.pathname !== '/' && parsed.pathname !== '') {
    throw new Error('[vite-proxy] Target URL must not specify path prefix');
  }

  if (parsed.port === '') {
    throw new Error('[vite-proxy] Target URL must specify an explicit non-zero port');
  }

  const portNum = Number(parsed.port);
  if (!Number.isInteger(portNum) || portNum <= 0 || portNum > 65535) {
    throw new Error('[vite-proxy] Target URL port is out of valid range (1-65535)');
  }

  const hostname = parsed.hostname.toLowerCase();
  if (!LOOPBACK_HOSTS.has(hostname)) {
    throw new Error('[vite-proxy] Target hostname is not an allowed loopback address');
  }

  return `${parsed.protocol}//${parsed.host}`;
}

/**
 * Constructs Vite server configuration for local dev proxying.
 * When CONTROLLER_DEV_BACKEND_TARGET is unset, proxy is disabled.
 * When set, safe loopback proxy is configured for `/api/v1` without altering
 * Origin or Host, without wildcard CORS, and with strict fail-closed validation.
 */
export function createServerConfig(env: Record<string, string | undefined> = process.env): UserConfig['server'] {
  const rawTarget = env.CONTROLLER_DEV_BACKEND_TARGET;
  const target = resolveBackendProxyTarget(rawTarget);

  if (!target) {
    return undefined;
  }

  return {
    proxy: {
      '/api/v1': {
        target,
        // MUST be false: preserve client's actual Host and Origin for backend whitelist validation
        changeOrigin: false,
        secure: false,
        ws: false,
      },
    },
  };
}

export default defineConfig({
  plugins: [vue()],
  server: createServerConfig(),
  test: {
    environment: 'jsdom',
    setupFiles: ['./src/test-setup.ts'],
  },
});

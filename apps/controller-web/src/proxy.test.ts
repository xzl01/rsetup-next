import { describe, expect, it } from 'vitest'
import { createServerConfig, resolveBackendProxyTarget } from '../vite.config'

describe('resolveBackendProxyTarget', () => {
  it('returns undefined when target is not set or empty', () => {
    expect(resolveBackendProxyTarget(undefined)).toBeUndefined()
    expect(resolveBackendProxyTarget('')).toBeUndefined()
    expect(resolveBackendProxyTarget('   ')).toBeUndefined()
  })

  it('accepts valid loopback HTTP targets with explicit port and normalizes URL', () => {
    expect(resolveBackendProxyTarget('http://127.0.0.1:8080')).toBe('http://127.0.0.1:8080')
    expect(resolveBackendProxyTarget('http://127.0.0.1:3000/')).toBe('http://127.0.0.1:3000')
    expect(resolveBackendProxyTarget('http://localhost:8080')).toBe('http://localhost:8080')
    expect(resolveBackendProxyTarget('http://[::1]:8080')).toBe('http://[::1]:8080')
  })

  it('rejects targets with credentials, query, or hash (fail closed)', () => {
    expect(() => resolveBackendProxyTarget('http://user:pass@127.0.0.1:8080')).toThrow(/credentials/)
    expect(() => resolveBackendProxyTarget('http://user@127.0.0.1:8080')).toThrow(/credentials/)
    expect(() => resolveBackendProxyTarget('http://127.0.0.1:8080?token=secret')).toThrow(/query/)
    expect(() => resolveBackendProxyTarget('http://127.0.0.1:8080#section')).toThrow(/hash/)
  })

  it('never leaks raw target or sensitive markers in error messages across all rejection paths', () => {
    const sensitiveMarker = 'MARKER_SYNTHETIC_SECRET_98765'
    const inputsWithMarker = [
      // Malformed URLs that fail new URL parsing
      `http://user:${sensitiveMarker}@127.0.0.1:not-a-port`,
      `http://${sensitiveMarker}:8080\\invalid-path`,
      `http://[invalid-ipv6-${sensitiveMarker}]:8080`,
      // Protocol violations
      `ftp://${sensitiveMarker}.com:8080`,
      `https://${sensitiveMarker}.com:8080`,
      // Hostname violations (external / non-loopback)
      `http://${sensitiveMarker}.example.com:8080`,
      `http://192.168.1.10:${sensitiveMarker}`,
      // Subpath violations
      `http://127.0.0.1:8080/${sensitiveMarker}`,
      // Query / hash violations
      `http://127.0.0.1:8080/?leak=${sensitiveMarker}`,
      `http://127.0.0.1:8080/#${sensitiveMarker}`,
      // User credentials violations
      `http://${sensitiveMarker}@127.0.0.1:8080`,
      `http://admin:${sensitiveMarker}@127.0.0.1:8080`,
    ]

    for (const input of inputsWithMarker) {
      try {
        resolveBackendProxyTarget(input)
        expect.unreachable(`Expected input to throw: ${input}`)
      } catch (err: unknown) {
        expect(err).toBeInstanceOf(Error)
        const msg = (err as Error).message
        expect(msg).not.toContain(sensitiveMarker)
        expect(msg).not.toContain(input)
      }
    }
  })

  it('rejects non-loopback hosts, wildcards, 0.0.0.0, and external IPs or domains', () => {
    expect(() => resolveBackendProxyTarget('http://0.0.0.0:8080')).toThrow(/loopback/)
    expect(() => resolveBackendProxyTarget('http://*')).toThrow()
    expect(() => resolveBackendProxyTarget('http://*:8080')).toThrow()
    expect(() => resolveBackendProxyTarget('http://192.168.1.10:8080')).toThrow(/loopback/)
    expect(() => resolveBackendProxyTarget('http://10.0.0.1:8080')).toThrow(/loopback/)
    expect(() => resolveBackendProxyTarget('http://example.com:8080')).toThrow(/loopback/)
    expect(() => resolveBackendProxyTarget('http://evil.127.0.0.1.nip.io:8080')).toThrow(/loopback/)
  })

  it('rejects non-http protocols or missing/invalid port', () => {
    expect(() => resolveBackendProxyTarget('https://127.0.0.1:8080')).toThrow(/protocol/)
    expect(() => resolveBackendProxyTarget('ftp://127.0.0.1:8080')).toThrow(/protocol/)
    expect(() => resolveBackendProxyTarget('ws://127.0.0.1:8080')).toThrow(/protocol/)
    expect(() => resolveBackendProxyTarget('http://127.0.0.1')).toThrow(/port/)
    expect(() => resolveBackendProxyTarget('http://localhost')).toThrow(/port/)
    expect(() => resolveBackendProxyTarget('http://127.0.0.1:notaport')).toThrow()
    expect(() => resolveBackendProxyTarget('http://127.0.0.1:0')).toThrow(/port/)
    expect(() => resolveBackendProxyTarget('http://127.0.0.1:65536')).toThrow()
  })

  it('rejects paths deeper than root', () => {
    expect(() => resolveBackendProxyTarget('http://127.0.0.1:8080/api')).toThrow(/path/)
    expect(() => resolveBackendProxyTarget('http://127.0.0.1:8080/prefix/')).toThrow(/path/)
  })
})

describe('createServerConfig', () => {
  it('returns undefined when CONTROLLER_DEV_BACKEND_TARGET is not defined', () => {
    const config = createServerConfig({})
    expect(config).toBeUndefined()
  })

  it('configures safe /api/v1 proxy without changing Origin or Host, without CORS wildcard, and preserves same-origin semantics', () => {
    const config = createServerConfig({
      CONTROLLER_DEV_BACKEND_TARGET: 'http://127.0.0.1:8080',
    })
    expect(config).toBeDefined()
    expect(config?.proxy).toBeDefined()
    const proxyConfig = config?.proxy?.['/api/v1']
    expect(proxyConfig).toBeDefined()
    expect(typeof proxyConfig === 'object').toBe(true)
    if (typeof proxyConfig === 'object' && proxyConfig !== null) {
      expect(proxyConfig.target).toBe('http://127.0.0.1:8080')
      // changeOrigin MUST NOT be true: backend enforces exact CONTROLLER_ALLOWED_HOSTS and CONTROLLER_ALLOWED_ORIGIN
      expect(proxyConfig.changeOrigin).toBe(false)
      // Must not enable cors wildcard or rewrite origin
      expect(proxyConfig.secure).toBe(false)
      expect(proxyConfig.ws).toBe(false)
      // proxy path is `/api/v1`, not rewriting away prefix
      expect(proxyConfig.rewrite).toBeUndefined()
    }
  })

  it('fails closed when CONTROLLER_DEV_BACKEND_TARGET is an invalid or external target', () => {
    expect(() => createServerConfig({ CONTROLLER_DEV_BACKEND_TARGET: 'http://malicious.com:8080' })).toThrow(/loopback/)
    expect(() => createServerConfig({ CONTROLLER_DEV_BACKEND_TARGET: 'http://user:pass@127.0.0.1:8080' })).toThrow(/credentials/)
  })
})

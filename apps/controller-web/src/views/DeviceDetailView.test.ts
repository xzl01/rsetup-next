import { render, screen } from '@testing-library/vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import DeviceDetailView from './DeviceDetailView.vue'
import { createI18n } from '../i18n'
import type { DeviceDetail, DeviceStatusReport } from '../types'

describe('DeviceDetailView U-01 five-dimensional health', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

  beforeEach(() => {
    vi.restoreAllMocks()
  })

  afterEach(() => {
    vi.restoreAllMocks()
    vi.unstubAllGlobals()
  })

  it('renders admission, connection, stream health, freshness, controller reception and board clock measurement independently without a combined online badge', async () => {
    // 规格遵从修正：板端 clock_quality 为 observed / clock_unstable
    // 中控接收端 received_time.quality 为 ntp_valid / system_fallback / stale
    const detail: DeviceDetail = {
      device_id: 'a'.repeat(64),
      display_name: 'Device A',
      effective_permissions: ['device.read', 'device.status.read'],
      admission_state: 'APPROVED',
      review_decision: 'approved',
      connection_state: 'online',
      control_health: 'healthy',
      data_health: 'degraded',
      capabilities: ['reboot'],
      revision: '10',
    }
    const status: DeviceStatusReport = {
      snapshot: { cpu_usage: 12, clock_quality: 'observed' },
      received_time: {
        quality: 'system_fallback',
        system_wall_utc: '2026-10-07T00:00:00Z',
        reference_utc: '2026-10-07T00:00:00Z',
      },
      freshness: 'stale',
      age_ms: '45000',
      last_error: 'heartbeat probe delayed',
    }

    render(DeviceDetailView, {
      props: {
        deviceId: 'a'.repeat(64),
        initialDetail: detail,
        initialStatus: status,
        permissions: ['device.read', 'device.status.read'],
      },
      global: {
        provide: { i18n },
      },
    })

    // 1. 准入独立分栏
    expect(screen.getByTestId('dim-admission').textContent).toContain('APPROVED')
    // 2. 连接独立分栏
    expect(screen.getByTestId('dim-connection').textContent).toContain('online')
    // 3. 业务流独立分栏 (分别呈现 Control 与 Data)
    expect(screen.getByTestId('dim-control-health').textContent).toContain('healthy')
    expect(screen.getByTestId('dim-data-health').textContent).toContain('degraded')
    // 4. 新鲜度与错误信息 (保留历史样本，十进制字符串 age_ms)
    expect(screen.getByTestId('dim-freshness').textContent).toContain('stale')
    expect(screen.getByTestId('dim-freshness').textContent).toContain('45000ms')
    expect(screen.getByTestId('dim-last-error').textContent).toContain('heartbeat probe delayed')
    // 5. 中控接收端与板端测量独立分栏
    expect(screen.getByTestId('dim-controller-clock').textContent).toContain('system_fallback')
    expect(screen.getByTestId('dim-board-clock').textContent).toContain('observed')

    // 严禁合并为单一全绿“在线”标签
    expect(screen.queryByTestId('unified-online-badge')).toBeNull()
  })

  it('renders board clock as null when snapshot measurement is absent, never fills 0', () => {
    const status: DeviceStatusReport = {
      snapshot: { cpu_usage: 12 }, // 无 clock_quality
      received_time: {
        quality: 'ntp_valid',
        system_wall_utc: '2026-10-07T00:00:00Z',
        reference_utc: '2026-10-07T00:00:00Z',
      },
      freshness: 'fresh',
      age_ms: '120',
    }
    render(DeviceDetailView, {
      props: {
        deviceId: 'a'.repeat(64),
        initialDetail: null,
        initialStatus: status,
        permissions: ['device.status.read'],
      },
      global: { provide: { i18n } },
    })
    const boardClock = screen.getByTestId('dim-board-clock')
    expect(boardClock.textContent).toContain('null')
    expect(boardClock.textContent).not.toContain('0')
  })

  it('hides profile detail card when user is status-only (lacks device.read)', () => {
    const status: DeviceStatusReport = {
      snapshot: { cpu_usage: 50 },
      received_time: {
        quality: 'ntp_valid',
        system_wall_utc: '2026-10-07T00:00:00Z',
        reference_utc: '2026-10-07T00:00:00Z',
      },
      freshness: 'fresh',
      age_ms: '300',
    }
    render(DeviceDetailView, {
      props: {
        deviceId: 'b'.repeat(64),
        initialDetail: null,
        initialStatus: status,
        permissions: ['device.status.read'],
      },
      global: { provide: { i18n } },
    })
    expect(screen.queryByTestId('detail-profile-card')).toBeNull()
    expect(screen.getByTestId('status-card')).toBeTruthy()
  })

  it('hides status card and prevents status fetch when user lacks device.status.read', () => {
    const detail: DeviceDetail = {
      device_id: 'b'.repeat(64),
      display_name: 'Device B',
      effective_permissions: ['device.read'],
      admission_state: 'APPROVED',
      review_decision: 'approved',
      connection_state: 'online',
      control_health: 'healthy',
      data_health: 'healthy',
      capabilities: ['reboot'],
      revision: '5',
    }
    render(DeviceDetailView, {
      props: {
        deviceId: 'b'.repeat(64),
        initialDetail: detail,
        initialStatus: null,
        permissions: ['device.read'],
      },
      global: { provide: { i18n } },
    })
    expect(screen.queryByTestId('status-card')).toBeNull()
    expect(screen.getByTestId('detail-profile-card')).toBeTruthy()
  })

  it('drops stale responses and clears old projection on device switch or permission revoke', async () => {
    let detailResolve!: (val: any) => void
    const slowDetailPromise = new Promise(resolve => { detailResolve = resolve })

    const fetchSpy = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      if (url.includes(`/api/v1/devices/${'1'.repeat(64)}`)) {
        return slowDetailPromise
      }
      if (url.includes(`/api/v1/devices/${'2'.repeat(64)}`)) {
        return new Response(JSON.stringify({
          data: {
            device_id: '2'.repeat(64),
            display_name: 'Device Fast',
            effective_permissions: ['device.read'],
            admission_state: 'APPROVED',
            review_decision: 'approved',
            connection_state: 'online',
            control_health: 'healthy',
            data_health: 'healthy',
            capabilities: [],
            revision: '1',
          },
          request_id: '3456789a-3456-4456-8456-3456789abcde',
        }), { status: 200, headers: { 'Content-Type': 'application/json' } })
      }
      throw new Error(`Unexpected url ${url}`)
    })
    vi.stubGlobal('fetch', fetchSpy)

    const { rerender } = render(DeviceDetailView, {
      props: {
        deviceId: '1'.repeat(64),
        permissions: ['device.read'],
      },
      global: { provide: { i18n } },
    })

    // 立即换设备到 2
    await rerender({
      deviceId: '2'.repeat(64),
      permissions: ['device.read'],
    })

    await vi.waitFor(() => expect(screen.getByText('Device Fast')).toBeTruthy())

    // 此时慢响应 1 返回，断言不会覆盖快速返回的 2
    detailResolve(new Response(JSON.stringify({
      data: {
        device_id: '1'.repeat(64),
        display_name: 'Device Slow Stale',
        effective_permissions: ['device.read'],
        admission_state: 'APPROVED',
        review_decision: 'approved',
        connection_state: 'online',
        control_health: 'healthy',
        data_health: 'healthy',
        capabilities: [],
        revision: '1',
      },
      request_id: '456789ab-4567-4567-8567-456789abcdef',
    }), { status: 200, headers: { 'Content-Type': 'application/json' } }))

    await new Promise(r => setTimeout(r, 10))
    expect(screen.queryByText('Device Slow Stale')).toBeNull()
    expect(screen.getByText('Device Fast')).toBeTruthy()

    // 撤销 device.read 权限：旧投影必须立即被清除
    await rerender({
      deviceId: '2'.repeat(64),
      permissions: [],
    })

    expect(screen.queryByTestId('detail-profile-card')).toBeNull()
    expect(screen.queryByText('Device Fast')).toBeNull()
  })

  it('aborts in-flight requests and cleans up on unmount', () => {
    let capturedSignal: AbortSignal | undefined
    const fetchSpy = vi.fn(async (_input: RequestInfo | URL, init?: RequestInit) => {
      capturedSignal = init?.signal as AbortSignal
      return new Promise(() => {}) // never resolves
    })
    vi.stubGlobal('fetch', fetchSpy)

    const { unmount } = render(DeviceDetailView, {
      props: {
        deviceId: '9'.repeat(64),
        permissions: ['device.read'],
      },
      global: { provide: { i18n } },
    })

    expect(fetchSpy).toHaveBeenCalled()
    expect(capturedSignal?.aborted).toBe(false)

    unmount()

    expect(capturedSignal?.aborted).toBe(true)
  })
})


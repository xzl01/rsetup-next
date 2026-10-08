import { fireEvent, render, screen } from '@testing-library/vue'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import DevicesView from './DevicesView.vue'
import { createI18n } from '../i18n'
import type { DeviceItem } from '../types'

describe('DevicesView pagination, projection and permissions', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

  beforeEach(() => {
    vi.restoreAllMocks()
  })

  afterEach(() => {
    vi.restoreAllMocks()
  })

  it('renders minimal projection: device_id and display_name, without detail or status', () => {
    const devices: DeviceItem[] = [
      {
        device_id: 'a'.repeat(64),
        display_name: 'Device Alpha',
        effective_permissions: ['device.read'],
      },
      {
        device_id: 'b'.repeat(64),
        display_name: 'Device Beta',
        effective_permissions: ['device.reboot'],
      },
    ]

    render(DevicesView, {
      props: {
        initialDevices: devices,
        permissions: ['device.read', 'device.reboot'],
      },
      global: { provide: { i18n } },
    })

    expect(screen.getByText('Device Alpha')).toBeTruthy()
    expect(screen.getByText('Device Beta')).toBeTruthy()
    expect(screen.getByText('a'.repeat(64))).toBeTruthy()
    expect(screen.getByText('b'.repeat(64))).toBeTruthy()
    // 列表仅最小投影，严禁直接展示详情卡片/状态卡片
    expect(screen.queryByTestId('detail-profile-card')).toBeNull()
    expect(screen.queryByTestId('status-card')).toBeNull()
  })

  it('when user has only device.reboot: displays reboot trigger, never requests detail or status GET', async () => {
    const fetchSpy = vi.fn()
    vi.stubGlobal('fetch', fetchSpy)

    const devices: DeviceItem[] = [
      {
        device_id: 'c'.repeat(64),
        display_name: 'Reboot Only Device',
        effective_permissions: ['device.reboot'],
      },
    ]

    render(DevicesView, {
      props: {
        initialDevices: devices,
        permissions: ['device.reboot'],
      },
      global: { provide: { i18n } },
    })

    // reboot-only 用户可以看到重启入口
    const rebootBtn = screen.getByTestId(`reboot-action-${'c'.repeat(64)}`)
    expect(rebootBtn).toBeTruthy()

    // 绝不自动发起 GET /devices/{id} 或 GET /devices/{id}/status 请求
    const calledUrls = fetchSpy.mock.calls.map(c => String(c[0]))
    expect(calledUrls.some(u => u.includes(`/devices/${'c'.repeat(64)}/status`))).toBe(false)
    expect(calledUrls.some(u => u.endsWith(`/devices/${'c'.repeat(64)}`))).toBe(false)
  })

  it('hides reboot trigger when user lacks device.reboot', () => {
    const devices: DeviceItem[] = [
      {
        device_id: 'd'.repeat(64),
        display_name: 'Read Only Device',
        effective_permissions: ['device.read'],
      },
    ]

    render(DevicesView, {
      props: {
        initialDevices: devices,
        permissions: ['device.read'],
      },
      global: { provide: { i18n } },
    })

    expect(screen.queryByTestId(`reboot-action-${'d'.repeat(64)}`)).toBeNull()
  })

  it('supports pagination via next_cursor and loads additional devices without losing existing list', async () => {
    const page1: DeviceItem[] = [
      { device_id: '1'.repeat(64), display_name: 'Dev 1', effective_permissions: ['device.read'] },
    ]
    const page2: DeviceItem[] = [
      { device_id: '2'.repeat(64), display_name: 'Dev 2', effective_permissions: ['device.read'] },
    ]

    const fetchSpy = vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input)
      if (url.includes('/api/v1/devices') && url.includes('cursor=cursor_page_2')) {
        return new Response(JSON.stringify({
          data: {
            items: page2,
            next_cursor: null,
          },
          request_id: '12345678-1234-4234-8234-123456789abc',
        }), { status: 200, headers: { 'Content-Type': 'application/json' } })
      }
      return new Response(JSON.stringify({
        data: { items: page1, next_cursor: 'cursor_page_2' },
        request_id: '23456789-2345-4345-8345-23456789abcd',
      }), { status: 200, headers: { 'Content-Type': 'application/json' } })
    })
    vi.stubGlobal('fetch', fetchSpy)

    render(DevicesView, {
      props: {
        initialDevices: page1,
        initialNextCursor: 'cursor_page_2',
        permissions: ['device.read'],
      },
      global: { provide: { i18n } },
    })

    expect(screen.getByText('Dev 1')).toBeTruthy()
    expect(screen.queryByText('Dev 2')).toBeNull()

    const loadMoreBtn = screen.getByTestId('load-more-btn')
    await fireEvent.click(loadMoreBtn)

    await vi.waitFor(() => expect(screen.getByText('Dev 2')).toBeTruthy())
    expect(screen.getByText('Dev 1')).toBeTruthy()
    expect(screen.queryByTestId('load-more-btn')).toBeNull()
  })

  it('filters devices by search input', async () => {
    const devices: DeviceItem[] = [
      { device_id: 'a'.repeat(64), display_name: 'Sensor Kitchen', effective_permissions: ['device.read'] },
      { device_id: 'b'.repeat(64), display_name: 'Actuator Garden', effective_permissions: ['device.read'] },
    ]

    render(DevicesView, {
      props: {
        initialDevices: devices,
        permissions: ['device.read'],
      },
      global: { provide: { i18n } },
    })

    expect(screen.getByText('Sensor Kitchen')).toBeTruthy()
    expect(screen.getByText('Actuator Garden')).toBeTruthy()

    const searchInput = screen.getByRole('textbox', { name: '搜索设备' })
    await fireEvent.update(searchInput, 'Kitchen')

    expect(screen.getByText('Sensor Kitchen')).toBeTruthy()
    expect(screen.queryByText('Actuator Garden')).toBeNull()
  })
})

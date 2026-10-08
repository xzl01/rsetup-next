import { describe, expect, it } from 'vitest'
import { assertDeviceListPageShape } from './types'

describe('types runtime guard assertDeviceListPageShape', () => {
  const validItem = {
    device_id: 'a'.repeat(64),
    display_name: 'Device Alpha',
    effective_permissions: ['device.read'],
  }

  it('accepts opaque string cursors (e.g. opaque:c2, cursor_page_2)', () => {
    const pageWithOpaqueCursor = {
      items: [validItem],
      next_cursor: 'opaque:c2',
    }

    expect(() => assertDeviceListPageShape(pageWithOpaqueCursor)).not.toThrow()
  })

  it('accepts null next_cursor', () => {
    const pageWithNullCursor = {
      items: [validItem],
      next_cursor: null,
    }

    expect(() => assertDeviceListPageShape(pageWithNullCursor)).not.toThrow()
  })

  it('rejects next_cursor that is empty string, contains control characters, or is overly long', () => {
    expect(() => assertDeviceListPageShape({ items: [validItem], next_cursor: '' })).toThrow('INVALID_NEXT_CURSOR')
    expect(() => assertDeviceListPageShape({ items: [validItem], next_cursor: 'cursor\x00bad' })).toThrow('INVALID_NEXT_CURSOR')
    expect(() => assertDeviceListPageShape({ items: [validItem], next_cursor: 'cursor\nbad' })).toThrow('INVALID_NEXT_CURSOR')
    expect(() => assertDeviceListPageShape({ items: [validItem], next_cursor: 'x'.repeat(1025) })).toThrow('INVALID_NEXT_CURSOR')
    expect(() => assertDeviceListPageShape({ items: [validItem], next_cursor: 12345 })).toThrow('INVALID_NEXT_CURSOR')
  })
})

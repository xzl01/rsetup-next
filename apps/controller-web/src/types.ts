/** Wire representation only; this alias does not validate decimal syntax. */
export type DecimalString = string

export interface ApiResponse<T> {
  data: T
  requestId: string
}

export type ApiErrorParams = Record<string, string | number | boolean | null>
export interface ApiErrorFields {
  code: string
  messageKey: string
  status?: number
  requestId?: string
  params?: ApiErrorParams
  retryAfter?: string
}

export interface DeviceItem {
  device_id: string
  display_name: string
  effective_permissions: string[]
}

export interface DeviceDetail extends DeviceItem {
  admission_state: 'PENDING' | 'APPROVED' | 'REVOKED'
  review_decision: 'none' | 'approved' | 'denied' | 'revoked'
  connection_state: 'online' | 'offline'
  control_health: 'starting' | 'healthy' | 'degraded' | 'offline'
  data_health: 'starting' | 'healthy' | 'degraded' | 'offline'
  capabilities: string[]
  revision: string
}

export interface DeviceStatusReport {
  snapshot: {
    clock_quality?: 'observed' | 'clock_unstable' | string | null
    [key: string]: unknown
  } | null
  received_time: {
    quality: 'ntp_valid' | 'system_fallback' | 'stale' | string
    system_wall_utc: string
    reference_utc: string
  }
  freshness: 'unknown' | 'fresh' | 'stale' | 'error'
  age_ms: string | null // 64位无符号十进制字符串，禁止JS Number精度丢失
  last_error?: string | null
}

export interface DeviceListPage {
  items: DeviceItem[]
  next_cursor: string | null
}

const DEVICE_ID_HEX64_REGEX = /^[0-9a-f]{64}(?![\s\S])/
const CURSOR_HEX132_REGEX = /^[0-9a-f]{132}(?![\s\S])/
const DECIMAL_STRING_REGEX = /^(0|[1-9][0-9]*)(?![\s\S])/
const U64_MAX_DEC = '18446744073709551615'

export function isU64DecimalString(value: unknown): value is string {
  if (typeof value !== 'string') return false
  if (!DECIMAL_STRING_REGEX.test(value)) return false
  return value.length < U64_MAX_DEC.length ||
    (value.length === U64_MAX_DEC.length && value <= U64_MAX_DEC)
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function assertDeviceItemShape(data: unknown): asserts data is DeviceItem {
  if (!isPlainObject(data)) throw new Error('INVALID_DEVICE_ITEM')
  if (typeof data.device_id !== 'string' || !DEVICE_ID_HEX64_REGEX.test(data.device_id)) {
    throw new Error('INVALID_DEVICE_ID')
  }
  if (typeof data.display_name !== 'string' || data.display_name.length === 0 || data.display_name.length > 512) {
    throw new Error('INVALID_DISPLAY_NAME')
  }
  if (!Array.isArray(data.effective_permissions) || !data.effective_permissions.every((p: unknown) => typeof p === 'string')) {
    throw new Error('INVALID_EFFECTIVE_PERMISSIONS')
  }
}

export function assertDeviceListPageShape(data: unknown): asserts data is DeviceListPage {
  if (!isPlainObject(data) || !Array.isArray(data.items)) {
    throw new Error('INVALID_DEVICE_LIST_PAGE')
  }
  if (data.next_cursor !== null && (typeof data.next_cursor !== 'string' || !CURSOR_HEX132_REGEX.test(data.next_cursor))) {
    throw new Error('INVALID_NEXT_CURSOR')
  }
  for (const item of data.items) {
    assertDeviceItemShape(item)
  }
}

const ADMISSION_STATES = new Set(['PENDING', 'APPROVED', 'REVOKED'])
const REVIEW_DECISIONS = new Set(['none', 'approved', 'denied', 'revoked'])
const CONNECTION_STATES = new Set(['online', 'offline'])
const HEALTH_STATES = new Set(['starting', 'healthy', 'degraded', 'offline'])

export function assertDeviceDetailShape(data: unknown): asserts data is DeviceDetail {
  assertDeviceItemShape(data)
  const d = data as unknown as Record<string, unknown>
  if (typeof d.admission_state !== 'string' || !ADMISSION_STATES.has(d.admission_state)) {
    throw new Error('INVALID_ADMISSION_STATE')
  }
  if (typeof d.review_decision !== 'string' || !REVIEW_DECISIONS.has(d.review_decision)) {
    throw new Error('INVALID_REVIEW_DECISION')
  }
  if (typeof d.connection_state !== 'string' || !CONNECTION_STATES.has(d.connection_state)) {
    throw new Error('INVALID_CONNECTION_STATE')
  }
  if (typeof d.control_health !== 'string' || !HEALTH_STATES.has(d.control_health)) {
    throw new Error('INVALID_CONTROL_HEALTH')
  }
  if (typeof d.data_health !== 'string' || !HEALTH_STATES.has(d.data_health)) {
    throw new Error('INVALID_DATA_HEALTH')
  }
  if (!Array.isArray(d.capabilities) || !d.capabilities.every((c: unknown) => typeof c === 'string')) {
    throw new Error('INVALID_CAPABILITIES')
  }
  if (!isU64DecimalString(d.revision)) {
    throw new Error('INVALID_REVISION')
  }
}

const FRESHNESS_STATES = new Set(['unknown', 'fresh', 'stale', 'error'])

export function assertDeviceStatusReportShape(data: unknown): asserts data is DeviceStatusReport {
  if (!isPlainObject(data)) throw new Error('INVALID_STATUS_REPORT')
  if (data.snapshot !== null && !isPlainObject(data.snapshot)) {
    throw new Error('INVALID_SNAPSHOT')
  }
  if (!isPlainObject(data.received_time)) {
    throw new Error('INVALID_RECEIVED_TIME')
  }
  const rt = data.received_time
  if (typeof rt.quality !== 'string' || typeof rt.system_wall_utc !== 'string' || typeof rt.reference_utc !== 'string') {
    throw new Error('INVALID_RECEIVED_TIME_FIELDS')
  }
  if (typeof data.freshness !== 'string' || !FRESHNESS_STATES.has(data.freshness)) {
    throw new Error('INVALID_FRESHNESS')
  }
  if (data.age_ms !== null && !isU64DecimalString(data.age_ms)) {
    throw new Error('INVALID_AGE_MS')
  }
  if (data.last_error !== undefined && data.last_error !== null && typeof data.last_error !== 'string') {
    throw new Error('INVALID_LAST_ERROR')
  }
}

export function canReadDevice(permissions: string[]): boolean {
  return permissions.includes('device.read')
}

export function canReadStatus(permissions: string[]): boolean {
  return permissions.includes('device.status.read')
}

export function canReboot(permissions: string[]): boolean {
  return permissions.includes('device.reboot')
}


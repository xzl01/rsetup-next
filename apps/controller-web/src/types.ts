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

export function canReadDevice(permissions: string[]): boolean {
  return permissions.includes('device.read')
}

export function canReadStatus(permissions: string[]): boolean {
  return permissions.includes('device.status.read')
}

export function canReboot(permissions: string[]): boolean {
  return permissions.includes('device.reboot')
}


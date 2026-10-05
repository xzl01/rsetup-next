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

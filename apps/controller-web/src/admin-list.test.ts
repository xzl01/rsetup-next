import { afterEach, describe, expect, it, vi } from 'vitest'
import { createAdminListResource } from './admin-list'
import { ApiError } from './api'
import type { AdminPage } from './admin-api'
import type { ApiResponse } from './types'

function page(items: string[], next_cursor: string | null): ApiResponse<AdminPage<string>> {
  return { data: { items, next_cursor }, requestId: 'req' }
}

type Loader = (cursor: string | null, signal: AbortSignal | undefined) => Promise<ApiResponse<AdminPage<string>>>

function deferred<T>() {
  let resolve!: (value: T) => void
  let reject!: (reason?: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

afterEach(() => vi.restoreAllMocks())

describe('createAdminListResource', () => {
  it('reload 填充集合与游标，loadMore 追加下一页', async () => {
    const loadPage = vi.fn(async (cursor: string | null) =>
      (cursor === null ? page(['a'], 'c1') : page(['b'], null)))
    const resource = createAdminListResource<string>(loadPage)

    await resource.reload()
    expect(resource.items.value).toEqual(['a'])
    expect(resource.nextCursor.value).toBe('c1')
    expect(loadPage).toHaveBeenLastCalledWith(null, expect.any(AbortSignal))

    await resource.loadMore()
    expect(resource.items.value).toEqual(['a', 'b'])
    expect(resource.nextCursor.value).toBe(null)
    expect(loadPage).toHaveBeenLastCalledWith('c1', expect.any(AbortSignal))
  })

  it('没有 next_cursor 时 loadMore 不发请求', async () => {
    const loadPage = vi.fn(async () => page(['a'], null))
    const resource = createAdminListResource<string>(loadPage)
    await resource.reload()
    await resource.loadMore()
    expect(loadPage).toHaveBeenCalledOnce()
  })

  it('旧代际响应不得覆盖新数据（刷新与翻页竞争）', async () => {
    const first = deferred<ApiResponse<AdminPage<string>>>()
    const second = deferred<ApiResponse<AdminPage<string>>>()
    const loadPage = vi.fn<Loader>()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
    const resource = createAdminListResource<string>(loadPage)

    const firstCall = resource.reload()
    const secondCall = resource.reload()
    second.resolve(page(['fresh'], null))
    await secondCall
    expect(resource.items.value).toEqual(['fresh'])

    // 迟到的第一轮响应到达：必须被丢弃，界面继续显示最新集合
    first.resolve(page(['stale'], null))
    await firstCall
    expect(resource.items.value).toEqual(['fresh'])
    expect(resource.errorKey.value).toBe(null)
  })

  it('reload 失败清空旧投影并映射错误 key（fail-closed）', async () => {
    const loadPage = vi.fn<Loader>()
      .mockResolvedValueOnce(page(['a'], null))
      .mockRejectedValueOnce(new ApiError({ code: 'REVISION_CONFLICT', messageKey: 'x' }))
    const resource = createAdminListResource<string>(loadPage)

    await resource.reload()
    expect(resource.items.value).toEqual(['a'])

    await resource.reload()
    expect(resource.items.value).toEqual([])
    expect(resource.errorKey.value).toBe('admin.error.revisionConflict')
    expect(resource.loading.value).toBe(false)
  })

  it('主动取消不是错误：既不写入数据也不设置 errorKey', async () => {
    const loadPage = vi.fn(async (_cursor: string | null, signal?: AbortSignal) => {
      await new Promise<void>((_resolve, reject) => {
        signal?.addEventListener('abort', () => reject(new ApiError({ code: 'REQUEST_ABORTED', messageKey: 'x' })))
      })
      return page(['never'], null)
    })
    const resource = createAdminListResource<string>(loadPage)
    const pending = resource.reload()
    resource.reset()
    await pending
    expect(resource.items.value).toEqual([])
    expect(resource.errorKey.value).toBe(null)
    expect(resource.loading.value).toBe(false)
  })

  it('reset 清空状态并作废旧请求的写入', async () => {
    const first = deferred<ApiResponse<AdminPage<string>>>()
    const loadPage = vi.fn<Loader>().mockReturnValueOnce(first.promise)
    const resource = createAdminListResource<string>(loadPage)

    const pending = resource.reload()
    resource.reset()
    first.resolve(page(['late'], 'c9'))
    await pending
    expect(resource.items.value).toEqual([])
    expect(resource.nextCursor.value).toBe(null)
  })
})

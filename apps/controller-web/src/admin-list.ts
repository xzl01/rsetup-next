// 管理端列表分页加载器：为 7 个管理视图提供一致的代际失效、取消与错误处理。
//
// 语义约束：
// - 每次 reload 递增代际并新建 AbortController；旧代际的在途响应一律丢弃，
//   绝不覆盖新数据（防止刷新与翻页竞争产生陈旧投影）。
// - reload 失败清空列表（fail-closed），不把上一次的数据继续显示为“最新”。
// - 主动取消（登出/换页/组件卸载）不是错误：不设置 errorKey，也不保留半成品数据。
import { ref, type Ref } from 'vue'
import { adminErrorKey, isAbortedError, type AdminPage } from './admin-api'
import type { ApiResponse } from './types'

export interface AdminListResource<T> {
  items: Ref<T[]>
  nextCursor: Ref<string | null>
  loading: Ref<boolean>
  errorKey: Ref<string | null>
  /** 加载第一页，替换当前集合。 */
  reload(): Promise<void>
  /** 追加下一页；没有 next_cursor 时为无操作。 */
  loadMore(): Promise<void>
  /** 作废在途请求并清空状态（登出、撤权、离开视图）。 */
  reset(): void
}

export function createAdminListResource<T>(
  loadPage: (cursor: string | null, signal: AbortSignal | undefined) => Promise<ApiResponse<AdminPage<T>>>,
  resolveErrorKey: (error: unknown) => string = adminErrorKey,
): AdminListResource<T> {
  const items = ref<T[]>([]) as Ref<T[]>
  const nextCursor = ref<string | null>(null)
  const loading = ref(false)
  const errorKey = ref<string | null>(null)

  let generation = 0
  let controller: AbortController | null = null

  function abortInFlight(): void {
    generation += 1
    if (controller) {
      controller.abort()
      controller = null
    }
  }

  async function run(cursor: string | null, append: boolean): Promise<void> {
    abortInFlight()
    const currentGeneration = generation
    const signal = typeof AbortController !== 'undefined' ? new AbortController() : null
    controller = signal
    loading.value = true
    errorKey.value = null
    try {
      const res = await loadPage(cursor, signal?.signal)
      if (currentGeneration !== generation) return
      const page = res.data
      items.value = append ? [...items.value, ...page.items] : page.items
      nextCursor.value = page.next_cursor
    } catch (error: unknown) {
      if (currentGeneration !== generation) return
      if (isAbortedError(error)) return
      items.value = []
      nextCursor.value = null
      errorKey.value = resolveErrorKey(error)
    } finally {
      if (currentGeneration === generation) {
        loading.value = false
        if (controller === signal) controller = null
      }
    }
  }

  return {
    items,
    nextCursor,
    loading,
    errorKey,
    reload() {
      return run(null, false)
    },
    loadMore() {
      if (!nextCursor.value || loading.value) return Promise.resolve()
      return run(nextCursor.value, true)
    },
    reset() {
      abortInFlight()
      items.value = []
      nextCursor.value = null
      errorKey.value = null
      loading.value = false
    },
  }
}

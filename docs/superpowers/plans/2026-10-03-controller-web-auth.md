# Controller Web 认证前端实施计划

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐项落实。使用 `- [ ]` 复选框跟踪步骤。前端 auth 设计已获用户明确批准（前后端并行）；后端 auth 计划与本计划**完全不共享文件**，最后一步只做合同对齐核验，不启动服务。

**Goal:** 在现有 `apps/controller-web` 上交付 cookie-session 认证的严格同源 POST 客户端、内存态 auth 状态机与双语可键盘操作的登录/强制改密 UI，全程只 mock `fetch`/jsdom。
**Architecture:** 三层递进：(1) 在 `src/api.ts` 扩展严格 `post`（与 `get` 共用校验与信封解包）；(2) `src/auth.ts` 提供内存态状态机（`checking / signed_out / force_password / signed_in / error`）与 login/me/logout/password 动作，CSRF token 仅存内存；(3) `src/App.vue` 按状态渲染表单，`i18n` 与词典补齐中英文案。登录成功后 `must_change_password=true` 时只允许改密/登出，绝不展示伪造设备数据。
**Tech Stack:** Vue 3.5.43 / Vite 8.3.2 / TypeScript 5.9.3（strict）/ Vitest 5.0.3 / @testing-library/vue 8.1.0 / jsdom 26.1.0 / vue-tsc 3.3.12，全部为 lockfile 已有版本，不新增依赖。
**Spec:** 身份规格 §3：[01 身份与授权](../specs/2026-09-23-controller-v1-01-identity-access.md)；HTTP 契约与账号 API：[02 数据模型与管理 API](../specs/2026-09-23-controller-v1-02-data-api.md) §3–4；前端基础约束沿用 [基础设计](../specs/2026-10-03-controller-web-foundation-design.md)。执行者同时阅读规格与计划。

## Global Constraints

- 只改 `apps/controller-web/**` 与本计划文件；与并行后端 auth 计划零文件交集；不改 `apps/controller-ui`、Rust/Cargo/DB、Makefile/CI。
- 不新增依赖、不改 `package.json`/`package-lock.json`；不假定存在 vue-router 或任何路由依赖。
- 不运行开发/预览服务，不做浏览器/a11y 工具/320px 视口/E2E 宣称；jsdom 通过只证明渲染与行为断言成立。
- 实现者禁止 stage/commit、派代理、读 secret、联网安装；所有 npm 命令必须带应用内缓存前缀：
  `NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" ...`（在 `apps/controller-web` 目录下执行；依赖已装好，不重装）。
- 测试只 mock `fetch`（`vi.stubGlobal`）边界并在 `afterEach` 恢复；`afterEach` 由 `src/test-setup.ts` 自动 cleanup。
- session 是 `HttpOnly` cookie：前端**永远读不到也绝不写** cookie，计划中不得出现 `document.cookie`。
- CSRF token 与 raw session token 只存内存（模块级/组件 ref），绝不进 `localStorage`/`sessionStorage`/URL/日志；测试须显式断言 `localStorage` 不含 token/session 内容。
- `X-CSRF-Token` 只在**已登录写请求**发送；`POST /auth/login` 不发送。
- 状态机判定：**仅 401 视为未登录**（`signed_out`）；网络失败、429、503 等一律 `error` 状态，UI 提供重试（重新 `me()`），不自动静默重试、不降级为 `signed_out`。
- 用户可见文案来自 i18n 词典（中英双词典 key 集合必须一致，由 `i18n.test.ts` 等长断言锁定）；`AppNotice` 的 `tone-label` 沿用既有 `App.vue` 的 `locale === 'en' ? ... : ...` 模式（既有代码模式，不新增 key）。服务端 `message_key` 只用作内部选择逻辑，未知 JSON 形状/未知错误 code 一律落到固定安全本地文案，**不回显** raw 服务器错误、params 或 HTML。
- 缺依赖/导入/编译失败不算 RED：先让测试可编译，再观察具体行为失败（断言失败或错误码不符才是 RED）。每个实现者报告 RED/GREEN 证据与边界。
- 每任务结束必须通过：`npm test -- --run <本任务测试文件>` + `npm run typecheck`（vue-tsc）；Task 4 追加 `npm run build` 与全量测试。

## 文件与任务边界

- Task 1 独占 `src/post.test.ts`（新建）、`src/api.ts`（修改）、`src/api.test.ts`（只读回归，不改）。
- Task 2 独占 `src/auth.test.ts`（新建）、`src/auth.ts`（新建）；不碰 `src/i18n.test.ts` 与词典。
- Task 3 独占 `src/App.vue`、`src/App.test.ts`（重写）、`src/locales/en.ts`、`src/locales/zh-CN.ts`（仅追加 key）、`src/i18n.test.ts`（keys 列表与词典同一步落地，保持等长断言通过）。
- 顺序依赖：Task 2 消费 Task 1 的 `post`；Task 3 消费 Task 2 的 `createAuth`。Task 1/2 可在 Task 3 之前独立审查。

---

### Task 1: 严格 JSON POST 客户端（api.post）

**Files:**
- Create: `apps/controller-web/src/post.test.ts`
- Modify: `apps/controller-web/src/api.ts`（新增 `post` 导出；把信封解包抽成共享内部函数，`get` 行为必须保持不变）
- Test: `apps/controller-web/src/api.test.ts`（既有回归，只运行不修改）

**Interfaces:**
- Consumes: 既有 `safePath`、`localError`、`ApiError`、`ApiResponse<T>`（`src/types.ts`）；本地错误码 `INVALID_API_PATH / NETWORK_ERROR / REQUEST_ABORTED / INVALID_API_RESPONSE`。
- Produces: `post<T>(path: string, payload: unknown, options?: { signal?: AbortSignal; csrfToken?: string }): Promise<ApiResponse<T>>`。行为契约：URL 恒为 `/api/v1/...`；`method:'POST'`、`credentials:'same-origin'`、`redirect:'error'`；headers 恒含 `Content-Type:'application/json'` 与 `Accept:'application/json'`，仅当 `csrfToken` 提供且匹配 `/^[A-Za-z0-9_-]{1,256}$/` 时附加 `X-CSRF-Token`（非法 token 在调用 fetch 前抛 `INVALID_API_PATH`）；`body` 恒为 `JSON.stringify(payload)`（`payload` 可为任意可序列化值）；成功信封 `{data,request_id}` → `{data, requestId}`；非 2xx 合法 `{error:{code,message_key,params?},request_id}` → 抛带 `code/status/requestId/retryAfter?/params?` 的 `ApiError`；其余任何形状 → 固定 `INVALID_API_RESPONSE`（`messageKey:'errors.invalidResponse'`），不回显服务器内容；fetch 抛错 → `NETWORK_ERROR`（signal aborted → `REQUEST_ABORTED`）。

- [ ] **Step 1: 编写失败测试（可编译的行为 RED）**

先让测试引用一个**已存在的可编译桩**（签名与最终契约一致，`noUnusedParameters` 要求未用参数加下划线前缀）：在 `src/api.ts` 末尾临时加

```ts
export function post<T>(_path: string, _payload: unknown, _options?: { signal?: AbortSignal; csrfToken?: string }): Promise<ApiResponse<T>> {
  throw new Error('post not implemented')
}
```

保证 `npm run typecheck` 通过；随后写 `src/post.test.ts`：

```ts
import { afterEach, describe, expect, it, vi } from 'vitest'
import { ApiError, post } from './api'

function jsonResponse(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json', ...headers },
  })
}

function fakeFetch(response: Response) {
  const fetcher = vi.fn(async (_url: string, _init?: RequestInit): Promise<Response> => response)
  vi.stubGlobal('fetch', fetcher)
  return fetcher
}

afterEach(() => vi.unstubAllGlobals())

describe('authenticated post', () => {
  it('locks transport: relative /api/v1 URL, same-origin credentials, redirect error, JSON body', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: { user: { username: 'admin' }, csrf_token: 'tok-1' }, request_id: '234f5678-e91b-43c5-a567-537725285111' }))
    const signal = new AbortController().signal
    const result = await post('/auth/login', { username: 'admin', password: 'synthetic-only' },
      { signal, method: 'GET', credentials: 'include', headers: { Authorization: 'bad' }, baseURL: 'https://example.invalid' } as { signal: AbortSignal })
    expect(result).toEqual({ data: { user: { username: 'admin' }, csrf_token: 'tok-1' }, requestId: '234f5678-e91b-43c5-a567-537725285111' })
    expect(fetcher).toHaveBeenCalledOnce()
    expect(fetcher.mock.calls[0]?.[0]).toBe('/api/v1/auth/login')
    // toMatchObject：实现按现有 get 模式以 `signal` 展开，避免 exact-match 对 undefined own 属性的分歧
    expect(fetcher.mock.calls[0]?.[1]).toMatchObject({
      method: 'POST', credentials: 'same-origin', redirect: 'error',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
      body: JSON.stringify({ username: 'admin', password: 'synthetic-only' }),
    })
    expect((fetcher.mock.calls[0]?.[1] as { signal?: AbortSignal }).signal).toBe(signal)
  })

  it('sends X-CSRF-Token only when a valid token is provided', async () => {
    const fetcher = vi.fn(async (): Promise<Response> => jsonResponse({ data: { changed: true }, request_id: '456b789a-0b3d-45e7-c789-759947407333' }))
    vi.stubGlobal('fetch', fetcher)
    await post('/auth/password', { current_password: 'a', new_password: 'b' }, { csrfToken: 'tok-9_-Z' })
    expect(fetcher.mock.calls[0]?.[1]).toMatchObject({
      headers: { 'Content-Type': 'application/json', Accept: 'application/json', 'X-CSRF-Token': 'tok-9_-Z' },
    })
    await post('/auth/logout', {})
    const second = fetcher.mock.calls[1]?.[1]
    expect(second).toMatchObject({
      headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    })
    expect(JSON.stringify(second)).not.toContain('X-CSRF-Token')
  })

  it.each([null, {}, { message: '中文与 unicode', nested: { ok: 1 } }, [1, 'two']])(
    'serializes any JSON-serializable payload %j', async (payload) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091' }))
    await post('/auth/logout', payload)
    expect(JSON.parse((fetcher.mock.calls[0]?.[1] as RequestInit).body as string)).toEqual(payload)
  })

  it('rejects an invalid csrfToken before calling fetch', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091' }))
    await expect(post('/auth/logout', {}, { csrfToken: 'bad\x00token' }))
      .rejects.toMatchObject({ code: 'INVALID_API_PATH', messageKey: 'errors.invalidPath' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it.each([
    'https://example.invalid/a', '//example.invalid', '/../x', '/%2e%2e/x', '/a\\b',
    '/a#frag', '/a\nx', '/%252f/x',
  ])('rejects unsafe path %s before calling fetch', async (path) => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091' }))
    await expect(post(path, {})).rejects.toMatchObject({ code: 'INVALID_API_PATH' })
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('accepts ordinary api paths', async () => {
    const fetcher = fakeFetch(jsonResponse({ data: true, request_id: 'e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091' }))
    await expect(post('auth/login', {})).resolves.toEqual({ data: true, requestId: 'e5f6a7b8-c9d0-4e1f-8a2b-4c5d6e7f8091' })
    expect(fetcher.mock.calls[0]?.[0]).toBe('/api/v1/auth/login')
  })

  it('preserves validated structured 401 errors without exposing server extras', async () => {
    const fetcher = fakeFetch(jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555', message: '<img src=x onerror=alert(1)>' }, 401))
    let caught: unknown
    try { await post('/auth/login', { username: 'a', password: 'b' }) } catch (error) { caught = error }
    expect(caught).toBeInstanceOf(ApiError)
    expect(caught).toMatchObject({ code: 'INVALID_CREDENTIALS', messageKey: 'errors.invalidCredentials', status: 401, requestId: '678d9abc-2d5f-47a9-e9ab-97bb69629555', message: 'API request failed' })
    expect(fetcher).toHaveBeenCalledOnce()
  })

  it('preserves 429 with Retry-After and 503 NOT_READY for upper-layer retry decisions', async () => {
    const fetcher = fakeFetch(jsonResponse({ error: { code: 'RATE_LIMITED', message_key: 'errors.rateLimited', params: { attempts: 5 } }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 429, { 'Retry-After': '30' }))
    await expect(post('/auth/login', { username: 'a', password: 'b' }))
      .rejects.toMatchObject({ code: 'RATE_LIMITED', status: 429, retryAfter: '30', params: { attempts: 5 } })
    fakeFetch(jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 503))
    await expect(post('/auth/me', null))
      .rejects.toMatchObject({ code: 'NOT_READY', status: 503, messageKey: 'errors.notReady' })
  })

  it.each([
    { request_id: 'r1' }, { data: false, request_id: '' }, { data: false, request_id: 42 },
    [1, 2], null, 0,
    { data: false, request_id: 'r1', error: { code: 'DENIED', message_key: 'errors.denied' } },
  ])('rejects malformed success envelopes %j', async (body) => {
    fakeFetch(jsonResponse(body))
    await expect(post('/auth/me', null)).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE', messageKey: 'errors.invalidResponse' })
  })

  it('rejects malformed error envelopes without echoing server message or raw body', async () => {
    fakeFetch(new Response(JSON.stringify({ error: { code: 'BAD CODE', message_key: 'errors.x' }, request_id: 'r' }), { status: 400, headers: { 'Content-Type': 'application/json' } }))
    const err = await post('/auth/login', { username: 'a', password: 'b' }).catch((e: unknown) => e) as ApiError
    expect(err).toMatchObject({ code: 'INVALID_API_RESPONSE', message: 'API request failed' })
    expect(JSON.stringify(err)).not.toContain('BAD CODE')
  })

  it('does not trust non-JSON content types', async () => {
    fakeFetch(new Response('<script>bad</script>', { status: 401, headers: { 'Content-Type': 'text/html' } }))
    await expect(post('/auth/login', { username: 'a', password: 'b' })).rejects.toMatchObject({ code: 'INVALID_API_RESPONSE' })
  })

  it('classifies network errors and aborts without exposing their messages', async () => {
    const fetcher = vi.fn().mockRejectedValue(new TypeError('secret network data'))
    vi.stubGlobal('fetch', fetcher)
    await expect(post('/auth/login', { username: 'a', password: 'b' }))
      .rejects.toMatchObject({ code: 'NETWORK_ERROR', messageKey: 'errors.network', message: 'API request failed' })
    expect(fetcher).toHaveBeenCalledOnce()
    const controller = new AbortController()
    controller.abort()
    vi.stubGlobal('fetch', vi.fn(async () => { throw new DOMException('aborted', 'AbortError') }))
    await expect(post('/auth/login', { username: 'a', password: 'b' }, { signal: controller.signal }))
      .rejects.toMatchObject({ code: 'REQUEST_ABORTED', messageKey: 'errors.aborted' })
  })
})
```

- [ ] **Step 2: 运行并确认 RED**

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run src/post.test.ts`
预期：全部 FAIL（桩抛 `post not implemented`，断言不成立）——这是行为 RED，不是导入失败。

- [ ] **Step 3: 最小实现**

在 `src/api.ts` 中把 `get` 的 Content-Type/JSON/信封/错误构造逻辑抽为共享函数（保持既有 `api.test.ts` 全绿），再加 `post`：

```ts
/** Unpack a same-origin /api/v1 JSON envelope; never echoes raw server content. */
async function unwrap<T>(response: Response, signal?: AbortSignal): Promise<ApiResponse<T>> {
  const contentType = response.headers.get('Content-Type') ?? ''
  if (!/^application\/(?:json|[a-z0-9.+-]+\+json)(?:\s*;|\s*$)/i.test(contentType)) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  let body: unknown
  try {
    body = await response.json()
  } catch (error) {
    throw localError(isAborted(error, signal) ? 'REQUEST_ABORTED' : 'INVALID_API_RESPONSE', response.status)
  }
  if (!isRecord(body) || !isRequestId(body.request_id)) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  if (response.ok) {
    if (!hasOwn(body, 'data') || hasOwn(body, 'error')) {
      throw localError('INVALID_API_RESPONSE', response.status)
    }
    return { data: body.data as T, requestId: body.request_id }
  }
  if (!isRecord(body.error) || hasOwn(body, 'data')) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  const { code, message_key: messageKey, params } = body.error
  if (typeof code !== 'string' || code.length > 128 || !/^[A-Za-z0-9_.-]+$/.test(code) ||
      typeof messageKey !== 'string' || !messageKey || messageKey.length > 256 ||
      !/^[A-Za-z0-9_.-]+$/.test(messageKey) ||
      (hasOwn(body.error, 'params') && !validParams(params))) {
    throw localError('INVALID_API_RESPONSE', response.status)
  }
  const retryAfter = response.headers.get('Retry-After')
  throw new ApiError({ code, messageKey, requestId: body.request_id,
    status: response.status, ...(hasOwn(body.error, 'params') ? { params: params as ApiErrorParams } : {}),
    ...(retryAfter !== null && retryAfter.length <= 256 && !/[\u0000-\u001f\u007f-\u009f]/.test(retryAfter)
      ? { retryAfter } : {}),
  })
}

function validCsrfToken(token: string): boolean {
  return /^[A-Za-z0-9_-]{1,256}$/.test(token)
}

export async function post<T>(path: string, payload: unknown,
  options?: { signal?: AbortSignal; csrfToken?: string }): Promise<ApiResponse<T>> {
  const url = safePath(path)
  const signal = options?.signal
  if (options?.csrfToken !== undefined && !validCsrfToken(options.csrfToken)) {
    throw localError('INVALID_API_PATH')
  }
  const headers: Record<string, string> = { 'Content-Type': 'application/json', Accept: 'application/json' }
  if (options?.csrfToken !== undefined) headers['X-CSRF-Token'] = options.csrfToken
  let response: Response
  try {
    response = await fetch(url, {
      method: 'POST', credentials: 'same-origin', redirect: 'error',
      headers, body: JSON.stringify(payload), signal,
    })
  } catch (error) {
    throw localError(isAborted(error, signal) ? 'REQUEST_ABORTED' : 'NETWORK_ERROR')
  }
  return unwrap<T>(response, signal)
}
```

同时把 `get` 中自 `const contentType = ...` 起至 `throw new ApiError(...)` 的整段替换为 `return unwrap<T>(response, signal)`。

- [ ] **Step 4: 运行并确认 GREEN + 回归**

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run src/post.test.ts src/api.test.ts`
预期：两个文件全部 PASS（`api.test.ts` 既有 40 余断言零改动通过，证明 `get` 行为未变）。

- [ ] **Step 5: 类型检查与提交报告**

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run typecheck`
预期：PASS。向协调者报告 RED/GREEN 输出摘录、`post` 最终签名、未浏览器实测边界；不 stage/commit。

---

### Task 2: 内存态 auth 状态机与动作（auth.ts）

**Files:**
- Create: `apps/controller-web/src/auth.test.ts`
- Create: `apps/controller-web/src/auth.ts`
（不修改 `src/i18n.test.ts`、词典与 `src/api.ts`；auth 状态机的文案映射只依赖 Task 3 将加入词典的 key 名，字符串一致性由 Task 3 的 `i18n.test.ts` keys 断言锁定。）

**Interfaces:**
- Consumes: Task 1 的 `post<T>`；`src/api.ts` 的 `get<T>`；`ApiError`。
- Produces（`src/auth.ts` 导出）：

```ts
export interface AuthUser {
  id: string           // 完整小写标准 UUIDv4 字符串（user_public 投影）
  username: string
  display_name?: string
  must_change_password: boolean
  is_admin?: boolean
  active?: boolean
  revision: string     // 规范十进制字符串，不经 Number/parseInt
}
export type AuthStatus = 'checking' | 'signed_out' | 'force_password' | 'signed_in' | 'error'
export type AuthErrorCode =
  | 'INVALID_CREDENTIALS' | 'AUTH_REQUIRED' | 'RATE_LIMITED'
  | 'PASSWORD_CHANGE_REQUIRED' | 'PERMISSION_DENIED'
  | 'INVALID_API_RESPONSE' | 'NETWORK_ERROR' | 'REQUEST_ABORTED' | 'OTHER'
export interface AuthStore {
  status: Ref<AuthStatus>
  user: Ref<AuthUser | null>
  csrfToken: Ref<string | null>
  authzEpoch: Ref<string | null>
  errorCode: Ref<AuthErrorCode | null>
  refresh(): Promise<void>   // GET /auth/me；仅 401 → signed_out，其余失败 → error
  login(username: string, password: string): Promise<boolean>   // 无 CSRF header；成功存内存 token
  changePassword(currentPassword: string, newPassword: string): Promise<boolean>
  logout(): Promise<void>
}
export function createAuth(): AuthStore
export function authErrorKey(code: string): string   // 错误码 → i18n key 的唯一映射，App 表单错误也走这里
```

状态机规则（全部由测试锁定）：
- 初始 `status='checking'`，`user/csrfToken/authzEpoch/errorCode` 均为 `null`；`createAuth()` 不发请求。
- **代际守卫（四操作通用）**：`let gen=0`，每个公开操作开头 `const mine=++gen`；任何对共享状态的写入前若 `mine!==gen` 则整段丢弃（迟到的旧 `me` 200/401、旧 `logout` 等不可覆盖更新的 login；短暂状态判定有歧义时宁可保守 `error`，不猜服务端权限）。
- **运行时 shape 守卫（me/login 的 data 通用，TS 接口不替代运行时校验）**：`get/post` 仅校验 envelope、`data` 一律 `as T` 透传，故响应返回后必须校验：`data.user` 是普通对象、`id` 为完整小写标准 UUIDv4 字符串（版本位 4、RFC 4122 变体位 8/9/a/b；缺失/number/短串/非 v4/非小写/错误变体均不合格）、`username` 非空 string、`revision` 为规范十进制字符串（`/^(0|[1-9]\d*)$/`，排除空串/符号/小数/指数/前导零/JSON number，不经 `Number`/`parseInt`）、`must_change_password` **严格 boolean**（`typeof==='boolean'`，缺失/字符串 `'true'` 均不合格）、`csrf_token` 非空 string；`me` 另要求 `authz_epoch` 为规范十进制字符串（`/^\d+$/`）。缺/错一律抛固定 `ApiError('INVALID_API_RESPONSE')`，并清本地 `user/csrfToken/authzEpoch`、进入 `error`——**不进入** `signed_in`/`force_password`（后端合同须恒发布尔，见末尾接口风险 1）。
- `refresh()`：先置 `checking`；`GET /auth/me` 成功且**过 shape 守卫** → `must_change_password===false` 时 `signed_in`，否则 `force_password`，并把 `user/csrfToken/authzEpoch` 存入内存（`authz_epoch` 保持十进制字符串）。shape 守卫不过 → 清秘密 + `error`+`INVALID_API_RESPONSE`。仅 `status===401` 的 `ApiError` → `signed_out` 且 `user=null, csrfToken=null, errorCode=null`。其他任何 `ApiError`（含 429/503/403/`INVALID_API_RESPONSE`）与网络/中断 → `error`，`errorCode` 按映射填入（未知 code → `'OTHER'`），**不自动重试**。
- `login()`：置 `checking`；`post('/auth/login', {username,password})`（**不传 csrfToken**）。成功（200，`data={user,csrf_token}`）且**过 shape 守卫**（免 `authz_epoch` 检查）→ 存 user 与 csrfToken（内存）；`user.must_change_password===true` → `force_password`，否则 `signed_in`；返回 `true`。shape 守卫不过 → 清秘密 + `error`+`INVALID_API_RESPONSE`，返回 `false`。`INVALID_CREDENTIALS`/`AUTH_REQUIRED` → `signed_out` + `errorCode`；`RATE_LIMITED` → `error` + `errorCode='RATE_LIMITED'`（UI 展示安全限速文案）；其余 → `error`；返回 `false`。
- `changePassword()`：需 `csrfToken`；`post('/auth/password', {current_password,new_password}, {csrfToken})`。成功（`data.changed===true`）→ 清 `csrfToken/user`，随后**立即调用 `refresh()` 重建真实状态**（规格 A-02：改密撤销全部会话；refresh 收到 401 → `signed_out`）；返回 `true`。失败且为 401 `AUTH_REQUIRED`（会话本身已失效）→ 清内存秘密、`signed_out`、`errorCode=null`。**其余 4xx 显式错误**（如 401 `INVALID_CREDENTIALS` 旧密码错、403 `CSRF_INVALID`、429 `RATE_LIMITED`——服务端明确未执行改密）→ 保持先前状态（`signed_in`/`force_password`），`errorCode` 按映射，返回 `false`。**结果未知/可能已执行**（网络 `NETWORK_ERROR`、中断 `REQUEST_ABORTED`、无效响应 `INVALID_API_RESPONSE`、5xx）→ **不得恢复 priorStatus 或旧 csrf**：清 `csrfToken/user/authzEpoch`，`status='error'`（提示"改密结果未知，请重新登录"），`errorCode` 按映射，返回 `false`。
- `logout()`：`post('/auth/logout', {}, {csrfToken?})`；**仅当 200 且 `data.logged_out===true`** 才可显示 `signed_out`（清 `user/csrfToken/authzEpoch`，`errorCode=null`）。网络失败/非 JSON/其他状态错误/`logged_out!==true` → `status='error'` + `errorCode` 映射，**不得声称服务端已撤会话**（HttpOnly cookie 客户端读不到，属真歧义）；敏感 `csrfToken` 仍清掉（本地），但未知状态下**不得再发起携带旧 token 的写请求**；用户可手动重试 `refresh()` 以 `me` 的真实 200/401 调和状态。
- 任何路径不得读写 `localStorage`（测试断言：操作后 `localStorage` 中不存在 csrf_token / session 字样）。
- `authErrorKey` 映射：`INVALID_CREDENTIALS→'auth.error.invalidCredentials'`、`AUTH_REQUIRED→'auth.error.authRequired'`、`RATE_LIMITED→'auth.error.rateLimited'`、`PASSWORD_CHANGE_REQUIRED→'auth.error.passwordChangeRequired'`、`PERMISSION_DENIED→'auth.error.denied'`、`INVALID_API_RESPONSE/NETWORK_ERROR/REQUEST_ABORTED/OTHER→'errors.generic'`。

- [ ] **Step 1: 编写失败测试（可编译的行为 RED）**

先在 `src/auth.ts` 放可编译桩（导出类型按上方 Interfaces 段原样定义；`verbatimModuleSyntax` 要求类型导入用 `import type`；`noUnusedParameters` 要求未用参数加下划线前缀）：

```ts
import type { Ref } from 'vue'
// 在此处按 Interfaces 段原样定义 AuthUser / AuthStatus / AuthErrorCode / AuthStore（AuthStore 字段用 Ref<...>）
export function createAuth(): AuthStore {
  throw new Error('auth not implemented')
}
export function authErrorKey(_code: string): string {
  return 'errors.generic'
}
```

此时 `npm run typecheck` 必须通过（只有类型引用，无未用值导入）。写 `src/auth.test.ts`：

```ts
import { afterEach, describe, expect, it, vi } from 'vitest'
import { createAuth, authErrorKey } from './auth'

// 契约身份（2026-10-07 HTTP ID 设计）：user.id 为固定合成小写 UUIDv4、revision 为十进制字符串；
// display_name 是未来可选字段，不属于当前 user_public 正常投影。
const USER_ID = '01234567-89ab-4cde-8f01-234567890abc'
const CONTRACT_USER = {
  id: USER_ID,
  username: 'admin',
  active: true,
  is_admin: true,
  must_change_password: false,
  revision: '1',
}

const ME_DATA = {
  user: CONTRACT_USER,
  csrf_token: 'mem-only-token',
  authz_epoch: '7',
}

function jsonResponse(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...headers } })
}

function fakeFetch(handler: (url: string, init?: RequestInit) => Response | Promise<Response>) {
  const fetcher = vi.fn(handler as unknown as typeof fetch)
  vi.stubGlobal('fetch', fetcher)
  return fetcher
}

afterEach(() => {
  vi.unstubAllGlobals()
  localStorage.clear()
})

describe('auth store state machine', () => {
  it('starts checking with nulls and issues no request until refresh', () => {
    const fetcher = vi.fn()
    vi.stubGlobal('fetch', fetcher)
    const store = createAuth()
    expect(store.status.value).toBe('checking')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    expect(store.errorCode.value).toBeNull()
    expect(fetcher).not.toHaveBeenCalled()
  })

  it('refresh: 200 me → signed_in with in-memory csrf and epoch, nothing written to localStorage', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' })
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('signed_in')
    expect(store.user.value?.username).toBe('admin')
    expect(store.csrfToken.value).toBe('mem-only-token')
    expect(store.authzEpoch.value).toBe('7')
    const joined = Object.keys(localStorage).map((k) => k + localStorage.getItem(k)).join(';')
    expect(joined).not.toContain('mem-only-token')
  })

  it('refresh: me with must_change_password=true → force_password', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data: { ...ME_DATA, user: { ...ME_DATA.user, must_change_password: true } }, request_id: '123e4567-e89b-42d3-a456-426614174000' })
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('force_password')
  })

  it('refresh: only 401 maps to signed_out; errorCode cleared', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 401)
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.errorCode.value).toBeNull()
  })

  it('refresh: network failure → error, not signed_out', async () => {
    fakeFetch(() => { throw new TypeError('offline') })
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
  })

  it('refresh: 503 NOT_READY and 429 → error with mapped code (upper layer may retry)', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ error: { code: 'NOT_READY', message_key: 'errors.notReady' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 503)
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('OTHER') // NOT_READY 未列入专属映射 → 安全通用文案
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ error: { code: 'RATE_LIMITED', message_key: 'errors.rateLimited' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 429, { 'Retry-After': '30' })
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('RATE_LIMITED')
  })

  it('login: success stores in-memory csrf, no X-CSRF-Token sent, no localStorage write', async () => {
    const fetcher = fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 'tok-2' }, request_id: '234f5678-e91b-43c5-a567-537725285111' })
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await expect(store.login('admin', 'synthetic-only')).resolves.toBe(true)
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('tok-2')
    const init = fetcher.mock.calls[0]?.[1] as { headers: Record<string, string>; body: string; method: string }
    expect(init.method).toBe('POST')
    expect(JSON.stringify(init.headers)).not.toContain('X-CSRF-Token')
    expect(JSON.parse(init.body)).toEqual({ username: 'admin', password: 'synthetic-only' })
    const joined = Object.keys(localStorage).map((k) => k + localStorage.getItem(k)).join(';')
    expect(joined).not.toContain('tok-2')
  })

  it('login: success with must_change_password=true → force_password (no device data surface)', async () => {
    fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ data: { user: { ...CONTRACT_USER, must_change_password: true }, csrf_token: 'tok-3' }, request_id: '234f5678-e91b-43c5-a567-537725285111' })
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await store.login('admin', 'synthetic-only')
    expect(store.status.value).toBe('force_password')
  })

  it.each([
    ['INVALID_CREDENTIALS', 401, 'signed_out'],
    ['AUTH_REQUIRED', 401, 'signed_out'],
    ['RATE_LIMITED', 429, 'error'],
  ])('login: %s → status %s with mapped errorCode', async (code, status, expected) => {
    fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ error: { code, message_key: 'errors.x' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, status)
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await expect(store.login('admin', 'wrong')).resolves.toBe(false)
    expect(store.status.value).toBe(expected)
    expect(store.errorCode.value).toBe(code)
  })

  it('login: 503/network → error, never signed_out', async () => {
    fakeFetch(() => { throw new TypeError('offline') })
    const store = createAuth()
    await expect(store.login('admin', 'x')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
  })

  it('changePassword: success clears csrf, refreshes via me with the new session, returns true', async () => {
    let phase = 0
    fakeFetch((url, init) => {
      if (url.includes('/auth/me')) {
        phase += 1
        return phase === 1
          ? jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' })
          : jsonResponse({ data: { ...ME_DATA, csrf_token: 'new-token' }, request_id: '345a6789-fa2c-44d6-b678-648836396222' })
      }
      if (url.includes('/auth/password')) {
        const initHeaders = (init?.headers ?? {}) as Record<string, string>
        expect(initHeaders['X-CSRF-Token']).toBe('mem-only-token')
        return jsonResponse({ data: { changed: true }, request_id: '456b789a-0b3d-45e7-c789-759947407333' })
      }
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(true)
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('new-token')
    expect(store.user.value?.must_change_password).toBe(false)
  })

  it('changePassword: 401 keeps prior signed_in/force_password state and sets errorCode, no me call', async () => {
    let meCalls = 0
    fakeFetch((url) => {
      if (url.includes('/auth/me')) { meCalls += 1; return jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' }) }
      if (url.includes('/auth/password')) {
        return jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 401)
      }
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('signed_in')
    expect(store.errorCode.value).toBe('INVALID_CREDENTIALS')
    expect(store.csrfToken.value).toBe('mem-only-token')
    expect(meCalls).toBe(1)
  })

  it('changePassword: 401 AUTH_REQUIRED (session itself revoked) → signed_out, secrets cleared, errorCode null', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' })
      if (url.includes('/auth/password')) {
        return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 401)
      }
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    expect(store.errorCode.value).toBeNull()
  })

  it('logout: posts empty JSON with CSRF when present, clears all in-memory state, ends signed_out', async () => {
    const fetcher = fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' })
      if (url.includes('/auth/logout')) return jsonResponse({ data: { logged_out: true }, request_id: '567c89ab-1c4e-46f8-d89a-86aa58518444' })
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store = createAuth()
    await store.refresh()
    await store.logout()
    expect(store.status.value).toBe('signed_out')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
    expect(store.authzEpoch.value).toBeNull()
    const init = fetcher.mock.calls[1]?.[1] as { headers: Record<string, string>; body: string }
    expect(init.headers['X-CSRF-Token']).toBe('mem-only-token')
    expect(JSON.parse(init.body)).toEqual({})
  })

  it('refresh: me missing must_change_password (not a boolean) → error INVALID_API_RESPONSE, never signed_in/force_password', async () => {
    fakeFetch((url) => url.includes('/auth/me')
      ? jsonResponse({ data: { user: { username: 'admin' }, csrf_token: 't', authz_epoch: '1' }, request_id: '123e4567-e89b-42d3-a456-426614174000' })
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await store.refresh()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
  })

  it('login: success response missing csrf_token → error INVALID_API_RESPONSE, no usable signed_in', async () => {
    fakeFetch((url) => url.includes('/auth/login')
      ? jsonResponse({ data: { user: { username: 'admin', must_change_password: false } }, request_id: '234f5678-e91b-43c5-a567-537725285111' })
      : jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' }))
    const store = createAuth()
    await expect(store.login('admin', 'synthetic-only')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('INVALID_API_RESPONSE')
    expect(store.csrfToken.value).toBeNull()
    expect(store.user.value).toBeNull()
  })

  it('changePassword: network failure (server may have executed) → clears secrets + error, never restores priorStatus or old csrf', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' })
      if (url.includes('/auth/password')) throw new TypeError('offline')
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.changePassword('old-pw', 'new-pw')).resolves.toBe(false)
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
    expect(store.user.value).toBeNull()
    expect(store.csrfToken.value).toBeNull()
  })

  it('logout: POST network failure → error NETWORK_ERROR, not signed_out (server revocation unconfirmed)', async () => {
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' })
      if (url.includes('/auth/logout')) throw new TypeError('offline')
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store = createAuth()
    await store.refresh()
    await expect(store.logout()).resolves.toBeUndefined()
    expect(store.status.value).toBe('error')
    expect(store.errorCode.value).toBe('NETWORK_ERROR')
    expect(store.csrfToken.value).toBeNull()
  })

  it('race: stale me responses (401 or 200 with old csrf) after a completed login must not overwrite the new state', async () => {
    let resolveA!: (r: Response) => void
    const gateA = new Promise<Response>((r) => { resolveA = r })
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return gateA
      if (url.includes('/auth/login')) return jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 'tok-new' }, request_id: '234f5678-e91b-43c5-a567-537725285111' })
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store = createAuth()
    const refreshing = store.refresh()
    await store.login('admin', 'synthetic-only')
    resolveA(jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 401))
    await refreshing
    expect(store.status.value).toBe('signed_in')
    expect(store.csrfToken.value).toBe('tok-new')

    let resolveB!: (r: Response) => void
    const gateB = new Promise<Response>((r) => { resolveB = r })
    fakeFetch((url) => {
      if (url.includes('/auth/me')) return gateB
      if (url.includes('/auth/login')) return jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 'tok-2' }, request_id: '234f5678-e91b-43c5-a567-537725285111' })
      return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' })
    })
    const store2 = createAuth()
    const refreshing2 = store2.refresh()
    await store2.login('admin', 'synthetic-only')
    resolveB(jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 'stale-csrf', authz_epoch: '3' }, request_id: '345a6789-fa2c-44d6-b678-648836396222' }))
    await refreshing2
    expect(store2.status.value).toBe('signed_in')
    expect(store2.csrfToken.value).toBe('tok-2')
  })

  it('authErrorKey maps known codes and falls back to the generic safe key', () => {
    expect(authErrorKey('INVALID_CREDENTIALS')).toBe('auth.error.invalidCredentials')
    expect(authErrorKey('AUTH_REQUIRED')).toBe('auth.error.authRequired')
    expect(authErrorKey('RATE_LIMITED')).toBe('auth.error.rateLimited')
    expect(authErrorKey('PASSWORD_CHANGE_REQUIRED')).toBe('auth.error.passwordChangeRequired')
    expect(authErrorKey('PERMISSION_DENIED')).toBe('auth.error.denied')
    expect(authErrorKey('SOMETHING_NEW')).toBe('errors.generic')
  })
})
```

- [ ] **Step 2: 运行并确认 RED**

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run src/auth.test.ts`
预期：FAIL（桩抛 `auth not implemented`）；所有失败必须是断言层面的行为 RED（新增的 shape 守卫、logout 失败、改密歧义、迟到响应竞争用例均为状态断言，竞争用例用手动 resolve 的延迟 fetch promise 观察），不得出现导入/类型错误。

- [ ] **Step 3: 最小实现**

```ts
import { ref, type Ref } from 'vue'
import { ApiError, get, post } from './api'

export interface AuthUser {
  id: string           // 完整小写标准 UUIDv4 字符串（user_public 投影）
  username: string
  display_name?: string
  must_change_password: boolean
  is_admin?: boolean
  active?: boolean
  revision: string     // 规范十进制字符串，不经 Number/parseInt
}
export type AuthStatus = 'checking' | 'signed_out' | 'force_password' | 'signed_in' | 'error'
export type AuthErrorCode =
  | 'INVALID_CREDENTIALS' | 'AUTH_REQUIRED' | 'RATE_LIMITED'
  | 'PASSWORD_CHANGE_REQUIRED' | 'PERMISSION_DENIED'
  | 'INVALID_API_RESPONSE' | 'NETWORK_ERROR' | 'REQUEST_ABORTED' | 'OTHER'
export interface AuthStore {
  status: Ref<AuthStatus>
  user: Ref<AuthUser | null>
  csrfToken: Ref<string | null>
  authzEpoch: Ref<string | null>
  errorCode: Ref<AuthErrorCode | null>
  refresh(): Promise<void>
  login(username: string, password: string): Promise<boolean>
  changePassword(currentPassword: string, newPassword: string): Promise<boolean>
  logout(): Promise<void>
}

const KNOWN_CODES: readonly AuthErrorCode[] = [
  'INVALID_CREDENTIALS', 'AUTH_REQUIRED', 'RATE_LIMITED',
  'PASSWORD_CHANGE_REQUIRED', 'PERMISSION_DENIED',
  'INVALID_API_RESPONSE', 'NETWORK_ERROR', 'REQUEST_ABORTED',
]

function errorCodeFor(error: unknown): AuthErrorCode {
  if (error instanceof ApiError && (KNOWN_CODES as readonly string[]).includes(error.code)) {
    return error.code as AuthErrorCode
  }
  return 'OTHER'
}

interface MeData { user: AuthUser; csrf_token: string; authz_epoch: string }

const invalidShape = () => new ApiError({ code: 'INVALID_API_RESPONSE', messageKey: 'errors.invalidResponse' })

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

// Full lowercase standard UUIDv4 (version 4, RFC 4122 variant 8/9/a/b); JSON number or other casing fails.
const USER_ID_V4 = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/
// Canonical decimal string: no empty/sign/fraction/exponent/leading-zero; JSON number fails typeof. No Number/parseInt.
const DECIMAL_STRING = /^(0|[1-9][0-9]*)$/

// Runtime shape guard: get/post validate only the envelope (data passes through as T), so identity fields
// (incl. business id UUIDv4 and decimal-string revision) are re-checked here; missing/wrong shape →
// INVALID_API_RESPONSE, caller clears identity/CSRF, never signed_in.
function assertIdentityShape(data: unknown): void {
  if (!isPlainObject(data) || !isPlainObject(data.user)) throw invalidShape()
  if (typeof data.user.id !== 'string' || !USER_ID_V4.test(data.user.id)) throw invalidShape()
  if (typeof data.user.username !== 'string' || data.user.username.length === 0) throw invalidShape()
  if (typeof data.user.revision !== 'string' || !DECIMAL_STRING.test(data.user.revision)) throw invalidShape()
  if (typeof data.user.must_change_password !== 'boolean') throw invalidShape()
  if (typeof data.csrf_token !== 'string' || data.csrf_token.length === 0) throw invalidShape()
}

function assertEpochShape(epoch: unknown): void {
  if (typeof epoch !== 'string' || !/^\d+$/.test(epoch)) throw invalidShape()
}

export function createAuth(): AuthStore {
  const status = ref<AuthStatus>('checking')
  const user = ref<AuthUser | null>(null)
  const csrfToken = ref<string | null>(null)
  const authzEpoch = ref<string | null>(null)
  const errorCode = ref<AuthErrorCode | null>(null)
  let gen = 0 // generation guard: a stale response must never overwrite a newer operation's state

  function applyMe(data: MeData): void {
    user.value = data.user
    csrfToken.value = data.csrf_token
    authzEpoch.value = data.authz_epoch
    errorCode.value = null
    status.value = data.user.must_change_password === true ? 'force_password' : 'signed_in'
  }

  function fail(error: unknown, mine: number): void {
    if (mine !== gen) return
    // Only a validated 401 means "not signed in"; everything else is a retryable error state.
    if (error instanceof ApiError && error.status === 401) {
      user.value = null
      csrfToken.value = null
      authzEpoch.value = null
      errorCode.value = null
      status.value = 'signed_out'
      return
    }
    errorCode.value = errorCodeFor(error)
    status.value = 'error'
  }

  // Shape-guard failure (or a malformed-envelope INVALID_API_RESPONSE): drop local identity/CSRF, do not guess login state.
  function rejectIdentity(mine: number): void {
    if (mine !== gen) return
    user.value = null
    csrfToken.value = null
    authzEpoch.value = null
    errorCode.value = 'INVALID_API_RESPONSE'
    status.value = 'error'
  }

  async function refresh(): Promise<void> {
    const mine = ++gen
    status.value = 'checking'
    errorCode.value = null
    try {
      const { data } = await get<MeData>('/auth/me')
      if (mine !== gen) return
      assertIdentityShape(data)
      assertEpochShape(data.authz_epoch)
      applyMe(data)
    } catch (error) {
      if (mine !== gen) return
      if (error instanceof ApiError && error.code === 'INVALID_API_RESPONSE') { rejectIdentity(mine) } else { fail(error, mine) }
    }
  }

  async function login(username: string, password: string): Promise<boolean> {
    const mine = ++gen
    status.value = 'checking'
    errorCode.value = null
    try {
      const { data } = await post<{ user: AuthUser; csrf_token: string }>('/auth/login',
        { username, password }) // first-time login: JSON + Host/Origin checks happen server-side; no CSRF token exists yet
      if (mine !== gen) return false
      assertIdentityShape(data) // login response carries no authz_epoch
      user.value = data.user
      csrfToken.value = data.csrf_token
      authzEpoch.value = null
      status.value = data.user.must_change_password === true ? 'force_password' : 'signed_in'
      return true
    } catch (error) {
      if (mine !== gen) return false
      if (error instanceof ApiError && error.code === 'INVALID_API_RESPONSE') {
        rejectIdentity(mine)
      } else if (error instanceof ApiError && error.status === 401) {
        user.value = null
        csrfToken.value = null
        errorCode.value = errorCodeFor(error)
        status.value = 'signed_out'
      } else {
        fail(error, mine)
      }
      return false
    }
  }

  async function changePassword(currentPassword: string, newPassword: string): Promise<boolean> {
    const mine = ++gen
    const priorStatus = status.value
    const priorUser = user.value
    try {
      const { data } = await post<{ changed: boolean }>('/auth/password',
        { current_password: currentPassword, new_password: newPassword },
        { csrfToken: csrfToken.value ?? undefined })
      if (mine !== gen) return false
      if (data.changed !== true) throw invalidShape()
      // Spec A-02: changing the password revokes all sessions — drop local secrets and re-establish via me() (401 → signed_out).
      csrfToken.value = null
      user.value = null
      await refresh()
      return true
    } catch (error) {
      if (mine !== gen) return false
      if (error instanceof ApiError && error.status === 401 && error.code === 'AUTH_REQUIRED') {
        // The session itself is gone (revoked/inactive): this is the only 401 that means signed out.
        user.value = null
        csrfToken.value = null
        authzEpoch.value = null
        errorCode.value = null
        status.value = 'signed_out'
        return false
      }
      if (error instanceof ApiError && error.status !== undefined && error.status >= 400 && error.status < 500) {
        // Explicit 4xx (e.g. 401 INVALID_CREDENTIALS — wrong old password): the server definitively did NOT change it.
        user.value = priorUser
        status.value = priorStatus === 'checking' ? 'error' : priorStatus
        errorCode.value = errorCodeFor(error)
        return false
      }
      // Unknown outcome — the change MAY have executed (network/abort/invalid response/5xx): never restore priorStatus or the stale csrf.
      user.value = null
      csrfToken.value = null
      authzEpoch.value = null
      errorCode.value = errorCodeFor(error)
      status.value = 'error'
      return false
    }
  }

  async function logout(): Promise<void> {
    const mine = ++gen
    try {
      const { data } = await post<{ logged_out: boolean }>('/auth/logout', {},
        { csrfToken: csrfToken.value ?? undefined })
      if (mine !== gen) return // a stale logout must not wipe a newer login
      if (data.logged_out !== true) throw invalidShape()
      user.value = null
      csrfToken.value = null
      authzEpoch.value = null
      errorCode.value = null
      status.value = 'signed_out'
    } catch (error) {
      if (mine !== gen) return
      // Network / non-JSON / status error: server revocation unknown (HttpOnly cookie unreadable from JS) — never claim signed_out; drop local csrf.
      csrfToken.value = null
      errorCode.value = errorCodeFor(error)
      status.value = 'error'
    }
  }

  return { status, user, csrfToken, authzEpoch, errorCode, refresh, login, changePassword, logout }
}

export function authErrorKey(code: string): string {
  switch (code) {
    case 'INVALID_CREDENTIALS': return 'auth.error.invalidCredentials'
    case 'AUTH_REQUIRED': return 'auth.error.authRequired'
    case 'RATE_LIMITED': return 'auth.error.rateLimited'
    case 'PASSWORD_CHANGE_REQUIRED': return 'auth.error.passwordChangeRequired'
    case 'PERMISSION_DENIED': return 'auth.error.denied'
    default: return 'errors.generic'
  }
}
```

- [ ] **Step 4: 运行并确认 GREEN + 回归**

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run src/auth.test.ts src/post.test.ts src/api.test.ts`
预期：全 PASS。

- [ ] **Step 5: 类型检查与提交报告**

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run typecheck`
预期：PASS（本任务不触碰词典与 `i18n.test.ts`；文案 key 字符串在 Task 3 与词典同步骤落地并由 keys 等长断言锁定）。报告 RED/GREEN 摘录与状态机边界（401 vs 其他失败的分流证据）；不 stage/commit。

---

### Task 3: 登录 / 强制改密 / 已登录 UI 接线（App.vue）

**Files:**
- Modify: `apps/controller-web/src/App.vue`（重写模板与脚本，保留 skip-link/语言切换/语义壳）
- Modify: `apps/controller-web/src/App.test.ts`（重写为 auth 行为测试；原“未连接展示”演示被认证状态取代）
- Modify: `apps/controller-web/src/locales/en.ts`、`apps/controller-web/src/locales/zh-CN.ts`（仅追加下表 21 个 key：`auth.*` 20 个 + `errors.generic` 1 个，不改既有值）
- Modify: `apps/controller-web/src/i18n.test.ts`（keys 列表补全，与词典同步骤落地，见 Step 2）

**Interfaces:**
- Consumes: `createAuth(): AuthStore` 与 `authErrorKey(code: string): string`（Task 2）；`createI18n`；`BaseInput`（`id/label/modelValue/type/hint/error/required`）、`BaseButton`（`type/variant/loading/loadingLabel`）。
- Produces: 认证态 App 页面——无路由、无业务设备数据面；状态到 UI 的固定映射（测试锁定）：`checking`/`error` 显示 `role="status"` 提示（error 时附“重试”按钮触发 `refresh()`）；`signed_out` 登录表单（`<form>` + `role="alert"` 错误区）；`force_password` 改密表单 + 登出按钮，**无任何设备/任务/统计占位**；`signed_in` 显示用户显示名（回退 username）与登出按钮。

新增 i18n key（双词典同 key 同义；既有 `app.title/nav.skip/language.label/state.loading/state.retry/button.loading/errors.*` 中前 4 个已存在，`errors.generic` 为新增）：

| key | zh-CN | en |
| --- | --- | --- |
| `auth.checking` | 正在检查登录状态 | Checking your session |
| `auth.error.generic` | 无法完成操作，请稍后重试 | The request could not be completed, please try again |
| `errors.generic` | 发生错误，请重试 | An error occurred, please retry |
| `auth.error.invalidCredentials` | 用户名或密码不正确 | The username or password is incorrect |
| `auth.error.authRequired` | 需要登录，请使用有效凭据重试 | Sign-in required, retry with valid credentials |
| `auth.error.rateLimited` | 尝试过于频繁，请等待后重试 | Too many attempts, please wait before retrying |
| `auth.error.passwordChangeRequired` | 当前密码需要先修改 | The password must be changed first |
| `auth.error.denied` | 没有权限执行此操作 | You do not have permission for this action |
| `auth.login.title` | 登录 | Sign in |
| `auth.username.label` | 用户名 | Username |
| `auth.username.hint` | 使用初始化日志或管理员提供的用户名 | Use the username from the setup log or your administrator |
| `auth.password.label` | 密码 | Password |
| `auth.login.submit` | 登录 | Sign in |
| `auth.logout.label` | 退出登录 | Sign out |
| `auth.passwordChange.title` | 修改密码 | Change password |
| `auth.passwordChange.forced` | 当前为临时密码，必须先修改密码才能继续 | This is a temporary password; change it to continue |
| `auth.currentPassword.label` | 当前密码 | Current password |
| `auth.newPassword.label` | 新密码 | New password |
| `auth.newPassword.hint` | 12–128 个字符，不能与当前密码相同 | 12–128 characters, and different from the current password |
| `auth.passwordChange.submit` | 保存新密码 | Save new password |
| `auth.signedIn.heading` | 已登录 | Signed in |

- [ ] **Step 1: 重写 App.test.ts（可编译的行为 RED）**

保留 `render(App)` 与 fetch stub 模式；新测试文件全文替换为：

```ts
import { fireEvent, render, screen } from '@testing-library/vue';
import { afterEach, beforeEach, expect, test, vi } from 'vitest';

import App from './App.vue';

// 契约身份（2026-10-07 HTTP ID 设计）：user.id 为固定合成小写 UUIDv4、revision 为十进制字符串；
// display_name 是未来可选字段，不属于当前 user_public 正常投影，UI 回退显示 username。
const USER_ID = '01234567-89ab-4cde-8f01-234567890abc';
const CONTRACT_USER = {
  id: USER_ID,
  username: 'admin',
  active: true,
  is_admin: true,
  must_change_password: false,
  revision: '1',
};

const ME_DATA = {
  user: CONTRACT_USER,
  csrf_token: 'mem-token',
  authz_epoch: '7',
};

function jsonResponse(body: unknown, status = 200, headers?: HeadersInit): Response {
  return new Response(JSON.stringify(body), { status, headers: { 'Content-Type': 'application/json', ...headers } });
}

function fetchForMe(meResponse: Response, loginResponse?: (init?: RequestInit) => Response,
  passwordResponse?: (init?: RequestInit) => Response) {
  const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) return meResponse;
    if (url.includes('/auth/login')) return loginResponse ? loginResponse(init) : jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 'login-token' }, request_id: '234f5678-e91b-43c5-a567-537725285111' });
    if (url.includes('/auth/password')) return passwordResponse ? passwordResponse(init) : jsonResponse({ data: { changed: true }, request_id: '456b789a-0b3d-45e7-c789-759947407333' });
    if (url.includes('/auth/logout')) return jsonResponse({ data: { logged_out: true }, request_id: '567c89ab-1c4e-46f8-d89a-86aa58518444' });
    throw new TypeError(`unexpected fetch ${url}`);
  });
  vi.stubGlobal('fetch', fetcher);
  return fetcher;
}

beforeEach(() => {
  localStorage.clear();
  document.documentElement.removeAttribute('lang');
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
  document.documentElement.removeAttribute('lang');
});

test('checking state: mount triggers exactly one GET /auth/me and shows a safe waiting status', () => {
  // 永不 resolve 的 fetch：状态在断言期间确定停留在 checking，无异步竞态。
  const fetcher = vi.fn(() => new Promise<Response>(() => {}));
  vi.stubGlobal('fetch', fetcher);
  render(App);
  expect(screen.getByRole('status').textContent).toContain('正在检查登录状态');
  expect(screen.queryByRole('form')).toBeNull();
  expect(screen.queryByRole('textbox', { name: '密码' })).toBeNull();
  expect(fetcher).toHaveBeenCalledOnce();
  expect(String(fetcher.mock.calls[0]?.[0])).toContain('/api/v1/auth/me');
});

test('signed_in state: no forms, shows user and logout; logout returns to the login form, no device data surface', async () => {
  fetchForMe(jsonResponse({ data: ME_DATA, request_id: '123e4567-e89b-42d3-a456-426614174000' }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  expect(screen.queryByRole('form')).toBeNull();
  expect(screen.getByRole('button', { name: '退出登录' })).toBeTruthy();
  await fireEvent.click(screen.getByRole('button', { name: '退出登录' }));
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
  expect(screen.getByRole('heading', { name: '登录' })).toBeTruthy();
  // 全程无设备/任务/统计占位：容器文本中不出现设备字样
  expect(screen.getByRole('main').textContent).not.toContain('设备');
});

test('force_password after login: only password change and logout are offered', async () => {
  fetchForMe(
    new Response(JSON.stringify({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }),
      { status: 401, headers: { 'Content-Type': 'application/json' } }),
    () => jsonResponse({ data: { user: { ...CONTRACT_USER, must_change_password: true }, csrf_token: 't' }, request_id: '234f5678-e91b-43c5-a567-537725285111' }),
  );
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
  await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'admin');
  await fireEvent.update(screen.getByRole('textbox', { name: '密码' }), 'synthetic-only');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('heading', { name: '修改密码' })).toBeTruthy());
  expect(screen.getByText('当前为临时密码，必须先修改密码才能继续')).toBeTruthy();
  expect(screen.getByRole('button', { name: '退出登录' })).toBeTruthy();
  expect(screen.queryByRole('textbox', { name: '用户名' })).toBeNull();
});

test('login form: submit posts credentials, error uses the safe mapped message, retry re-submits', async () => {
  let call = 0;
  const fetcher = vi.fn(async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) return jsonResponse({ error: { code: 'AUTH_REQUIRED', message_key: 'errors.authRequired' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 401);
    call += 1;
    if (call === 1) return jsonResponse({ error: { code: 'INVALID_CREDENTIALS', message_key: 'errors.invalidCredentials' }, request_id: '678d9abc-2d5f-47a9-e9ab-97bb69629555' }, 401);
    return jsonResponse({ data: { user: CONTRACT_USER, csrf_token: 't' }, request_id: '234f5678-e91b-43c5-a567-537725285111' });
  });
  vi.stubGlobal('fetch', fetcher);
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('form')).toBeTruthy());
  await fireEvent.update(screen.getByRole('textbox', { name: '用户名' }), 'admin');
  await fireEvent.update(screen.getByRole('textbox', { name: '密码' }), 'wrong');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('alert').textContent).toContain('用户名或密码不正确'));
  const body = JSON.parse((fetcher.mock.calls[1]?.[1] as { body: string }).body);
  expect(body).toEqual({ username: 'admin', password: 'wrong' });
  await fireEvent.update(screen.getByRole('textbox', { name: '密码' }), 'ok');
  await fireEvent.click(screen.getByRole('button', { name: '登录' }));
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  expect(screen.queryByRole('form')).toBeNull();
});

test('signed_in state: csrf/session stay in memory only, never in localStorage (changePassword 成功路径已由 auth.test.ts 锁定)', async () => {
  let me = 0;
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL): Promise<Response> => {
    const url = String(input);
    if (url.includes('/auth/me')) {
      me += 1;
      return jsonResponse({ data: { ...ME_DATA, csrf_token: me === 1 ? 'old' : 'new' }, request_id: me === 1 ? '123e4567-e89b-42d3-a456-426614174000' : '345a6789-fa2c-44d6-b678-648836396222' });
    }
    return jsonResponse({ data: null, request_id: '789eabcd-3e6a-48ba-fabc-a8cc7a73a666' });
  }));
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('admin'));
  // 已登录视图无表单；改密成功→清 token→me() 重建会话的完整路径由 auth.test.ts 的
  // changePassword 用例锁定；这里只锁定 App 视图映射与敏感数据不落存储。
  expect(screen.queryByRole('form')).toBeNull();
  const joined = Object.keys(localStorage).map((k) => k + localStorage.getItem(k)).join(';');
  expect(joined).not.toContain('old');
  expect(joined).not.toContain('new');
});

test('error state: network failure shows a safe message and a working retry button (no auto retry)', async () => {
  const fetcher = vi.fn(async (): Promise<Response> => { throw new TypeError('offline'); });
  vi.stubGlobal('fetch', fetcher);
  render(App);
  await vi.waitFor(() => expect(screen.getByRole('status').textContent).toContain('无法完成操作，请稍后重试'));
  expect(screen.getByRole('button', { name: '重试' })).toBeTruthy();
  expect(fetcher).toHaveBeenCalledTimes(1);
  await fireEvent.click(screen.getByRole('button', { name: '重试' }));
  expect(fetcher).toHaveBeenCalledTimes(2);
});
```

（`vi.waitFor` 是 Vitest 5 内置 API，直接用于等待异步状态落定；`fireEvent.click` 提交按钮在 jsdom 中会走表单 `submit` 事件（表单已加 `novalidate`，跳过约束校验）。测试只覆盖渲染与行为，不宣称真实浏览器表单提交行为。）

此时新测试引用尚未存在的 `auth` 模块行为（`createAuth` 已存在，但 App.vue 仍是基础展示壳），typecheck 必须仍通过（`App.test.ts` 只导入 `./App.vue` 与 vitest/testing-library，无新模块导入）。

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run src/App.test.ts`
预期：FAIL（当前 App 不发起 `/auth/me` 请求、不渲染登录/改密表单——断言层行为 RED，如 `getByRole('form')` 找不到元素、fetch 未被调用等具体断言失败）。

- [ ] **Step 2: 追加 i18n key 并补全 i18n.test.ts keys 列表**

在 `src/locales/zh-CN.ts` 与 `src/locales/en.ts` 的 `messages` 中按上表**逐字**追加全部 21 个 key（`auth.*` 20 个 + `errors.generic` 1 个，逐行对照表格，不得遗漏 `auth.signedIn.heading` 与 `auth.error.denied`）。在 `src/i18n.test.ts` 的 `keys` 数组中追加同样的 21 个 key（既有 key 不动，等长断言自动覆盖双词典一致性）。

- [ ] **Step 3: 重写 App.vue（最小实现）**

```vue
<script setup lang="ts">
import { onMounted, ref } from 'vue';
import AppNotice from './components/AppNotice.vue';
import BaseButton from './components/BaseButton.vue';
import BaseInput from './components/BaseInput.vue';
import { createI18n, type Locale } from './i18n';
import { authErrorKey, createAuth } from './auth';

const { locale, t, setLocale } = createI18n();
const auth = createAuth();

const username = ref('');
const password = ref('');
const currentPassword = ref('');
const newPassword = ref('');
const formError = ref('');
const busy = ref(false);

function changeLocale(event: Event) {
  setLocale((event.target as HTMLSelectElement).value as Locale);
}

function mappedFormError(): string {
  // 服务端文案永不展示：状态机已把错误归约为 errorCode，唯一映射 authErrorKey 给出本地安全文案。
  return t(authErrorKey(auth.errorCode.value ?? 'OTHER'));
}

async function submitLogin(event: Event) {
  event.preventDefault();
  if (busy.value) return;
  busy.value = true;
  formError.value = '';
  try {
    const ok = await auth.login(username.value, password.value);
    if (ok) { password.value = ''; return; } // 成功后表单随视图消失；清空口令输入
    formError.value = mappedFormError();
  } catch {
    formError.value = t('errors.generic');
  } finally {
    busy.value = false;
  }
}

async function submitPassword(event: Event) {
  event.preventDefault();
  if (busy.value) return;
  busy.value = true;
  formError.value = '';
  try {
    const ok = await auth.changePassword(currentPassword.value, newPassword.value);
    if (!ok) formError.value = mappedFormError();
  } catch {
    formError.value = t('errors.generic');
  } finally {
    currentPassword.value = '';
    busy.value = false;
  }
}

async function submitLogout() {
  if (busy.value) return;
  busy.value = true;
  formError.value = '';
  try { await auth.logout(); }
  finally {
    username.value = '';
    password.value = '';
    busy.value = false;
  }
}

onMounted(() => { void auth.refresh(); });
</script>

<template>
  <a class="skip-link" href="#main-content">{{ t('nav.skip') }}</a>
  <header class="app-header">
    <h1>{{ t('app.title') }}</h1>
    <label for="app-language">{{ t('language.label') }}</label>
    <select id="app-language" :value="locale" @change="changeLocale">
      <option value="zh-CN">简体中文</option>
      <option value="en">English</option>
    </select>
  </header>
  <main id="main-content" class="app-main" tabindex="-1">
    <template v-if="auth.status.value === 'checking' || auth.status.value === 'error'">
      <AppNotice :tone-label="locale === 'en' ? 'Status' : '状态'"
        :title="auth.status.value === 'checking' ? t('state.loading') : t('state.error')">
        <span v-if="auth.status.value === 'checking'">{{ t('auth.checking') }}</span>
        <span v-else>{{ t('auth.error.generic') }}</span>
        <BaseButton v-if="auth.status.value === 'error'" class="auth-retry" :loading-label="t('button.loading')" @click="() => void auth.refresh()">
          {{ t('state.retry') }}
        </BaseButton>
      </AppNotice>
    </template>
    <form v-else-if="auth.status.value === 'signed_out'" class="auth-form" novalidate @submit.prevent="submitLogin">
      <h2>{{ t('auth.login.title') }}</h2>
      <p v-if="formError" role="alert" class="auth-form__error">{{ formError }}</p>
      <BaseInput id="auth-username" v-model="username" :label="t('auth.username.label')"
        :hint="t('auth.username.hint')" required autocomplete="username" :disabled="busy" />
      <BaseInput id="auth-password" v-model="password" :label="t('auth.password.label')"
        type="password" required autocomplete="current-password" :error="formError || undefined" :disabled="busy" />
      <BaseButton type="submit" :loading="busy" :loading-label="t('button.loading')">{{ t('auth.login.submit') }}</BaseButton>
    </form>
    <form v-else-if="auth.status.value === 'force_password'" class="auth-form" novalidate @submit.prevent="submitPassword">
      <h2>{{ t('auth.passwordChange.title') }}</h2>
      <p class="auth-form__forced">{{ t('auth.passwordChange.forced') }}</p>
      <p v-if="formError" role="alert" class="auth-form__error">{{ formError }}</p>
      <BaseInput id="auth-current-password" v-model="currentPassword" :label="t('auth.currentPassword.label')"
        type="password" required autocomplete="current-password" :error="formError || undefined" :disabled="busy" />
      <BaseInput id="auth-new-password" v-model="newPassword" :label="t('auth.newPassword.label')"
        :hint="t('auth.newPassword.hint')" type="password" required autocomplete="new-password"
        :error="formError || undefined" :disabled="busy" />
      <div class="auth-form__actions">
        <BaseButton type="submit" :loading="busy" :loading-label="t('button.loading')">{{ t('auth.passwordChange.submit') }}</BaseButton>
        <BaseButton variant="secondary" :disabled="busy" @click="submitLogout">{{ t('auth.logout.label') }}</BaseButton>
      </div>
    </form>
    <section v-else class="auth-signed-in">
      <h2>{{ t('auth.signedIn.heading') }}</h2>
      <p role="status">{{ auth.user.value?.display_name || auth.user.value?.username }}</p>
      <BaseButton :loading="busy" :loading-label="t('button.loading')" @click="submitLogout">{{ t('auth.logout.label') }}</BaseButton>
    </section>
  </main>
  <footer class="app-footer">{{ t('app.title') }}</footer>
</template>
```

样式：在 `App.vue` 的 scoped style 中新增 `.auth-form{display:grid;gap:var(--space-4);min-width:0;max-width:34rem;}`、`.auth-form__error{color:var(--color-danger,var(--color-surface));margin:0;}`、`.auth-form__forced{margin:0;}`、`.auth-form__actions{display:flex;gap:var(--space-2);flex-wrap:wrap;}`、`.auth-signed-in{display:grid;gap:var(--space-4);}`、`.auth-retry{margin-inline-start:auto;}`。表单原生 `<input type="password">` 键盘可操作（BaseInput 已提供 label/aria-invalid/aria-describedby），不引入新的焦点陷阱。

- [ ] **Step 4: 运行并确认 GREEN**

运行：`NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run src/App.test.ts src/i18n.test.ts src/auth.test.ts`
预期：全 PASS；`i18n.test.ts` 的 keys 等长断言通过（双词典 21 个新 key 一致，key 集合完全相同）。

- [ ] **Step 5: 全量测试 + 类型检查 + 构建**

运行（均在 `apps/controller-web`）：
1. `NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run`
2. `NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run typecheck`
3. `NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run build`

预期：全部 PASS/PASS/built。向协调者报告：RED/GREEN 证据、双语错误文案 key 清单、**明确未验证项**（无浏览器实测：键盘导航、屏幕阅读器、320px 视口、真实 HTTPS 下的 cookie/Origin 行为均未验证）；不 stage/commit。

---

### Task 4: 合同对齐核验（待后端就绪）

**Files:**
- Modify: 无（只读核验；若发现合同偏差，只允许在本计划允许的 `apps/controller-web/**` 内修正并补测试，重新执行 Task 1–3 的相关步骤）
- Test: 全量 `apps/controller-web` 测试套件

**Interfaces:**
- Consumes: Task 1–3 的全部产物；规格 [02 §3–4](../specs/2026-09-23-controller-v1-02-data-api.md) 的 wire 表。
- Produces: 一份合同对齐报告（写入协调者结果，不落盘）：逐项列出前端假设 vs 规格的字段/状态/错误码对照，标注“已确认 / 待后端联调”；不声称已联调。

- [ ] **Step 1: 全量回归**

运行：
1. `NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run`
2. `NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run typecheck`
3. `NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run build`

预期：全 PASS。

- [ ] **Step 2: 逐项合同核对（只读，不改码）**

对照 [02 §3](../specs/2026-09-23-controller-v1-02-data-api.md) 响应/错误表与 [§4](../specs/2026-09-23-controller-v1-02-data-api.md) 账号 API 表，逐条核对并在报告中标注：
- 信封：成功 `{data,request_id}`、错误 `{error:{code,message_key,params?},request_id}` —— 前端 `unwrap` 已拒绝其余形状（`INVALID_API_RESPONSE` 兜底）。
- 四个 auth 端点的方法、请求字段名（`username/password`、`current_password/new_password`、logout 空对象）、响应字段（`user/csrf_token/authz_epoch/changed/logged_out`）与 §4 表逐字一致。
- 错误码表：前端只依赖 `INVALID_CREDENTIALS/AUTH_REQUIRED(401)`、`RATE_LIMITED(429, Retry-After)`、`PASSWORD_CHANGE_REQUIRED/CSRF_INVALID/PERMISSION_DENIED(403)` 的**存在性**，其余一律安全通用文案；确认规格未新增登录相关 4xx 码。
- [01 §3](../specs/2026-09-23-controller-v1-01-identity-access.md)：临时密码仅可访问 `auth/me|password|logout` —— 前端 `force_password` 视图无其他入口；CSRF 仅已登录写请求携带；无任意来源 CORS；前端不读 cookie。
- 报告“接口风险”小节（见下）。

- [ ] **Step 3: 交付报告**

向协调者报告：全量测试/typecheck/build 输出摘录；合同对照表；**未验证清单**（真实浏览器、a11y 工具、320px、真实后端联调、SSE、设备数据面均不属于本计划）；不 stage/commit。

---

## 接口风险（回讯要点）

1. **`GET /auth/me` 的 `user` 字段集**：规格 §4 只写 `user,csrf_token,authz_epoch`，未列 `user` 字段。前端按最小集 `id` + `username` + `revision` + `must_change_password` 读取，其余（`display_name/is_admin/active`）按可选处理（未知字段忽略、缺失可选字段不炸）。**门禁已内置（Task 2 运行时 shape 守卫）**：`id` 缺/非完整小写 UUIDv4（版本位 4、RFC 4122 变体位）、`revision` 缺/非规范十进制字符串、`must_change_password` 缺/非严格 boolean、`username` 空、`csrf_token` 空、`authz_epoch` 非规范十进制字符串 → 一律 `INVALID_API_RESPONSE` + 清本地身份/CSRF + `error`，**不存在**"缺 `must_change_password` 时误入 `signed_in`"的路径。**后端合同仍待联调确认**：后端须恒发布尔 `must_change_password` 且恒给 `csrf_token`/`authz_epoch` 字段（`user_public` 投影恒给 UUIDv4 `id` 与十进制字符串 `revision`）；若后端可能省略，前端会保守落 `error`（用户需重试/刷新），不会静默放行。
2. **CSRF token 传输位置**：前端假设 login/me 的 `csrf_token` 在 **data body**（非 header/非 cookie）。若后端改为 header，Task 2 的 `applyMe/login` 需一处小改（消费 `post/get` 返回的 headers），测试需加 header 断言。
3. **401 语义边界**：规格允许 401 携带 `AUTH_REQUIRED` 或 `INVALID_CREDENTIALS`；前端把**任何 401**（含 `/auth/me` 上的）统一视为未登录。若后端在“session 有效但账号被停用”时也回 401，前端会静默回到登录页——语义可接受但需联调确认不会把 `PERMISSION_DENIED` 场景误标 401。
4. **`POST /auth/me` 之外的错误码**：前端未为 `NOT_READY/STORAGE_UNAVAILABLE/RESOURCE_EXHAUSTED(503)` 建立专属文案，一律 `errors.generic` + 重试按钮；若产品要求 503 专属文案，只需加 i18n key 与映射，不动状态机。
5. **Origin 检查不可前端自检**：初次登录的 Origin/Host 精确允许是服务端职责；前端无法在 jsdom 中验证，联调前不得宣称登录 Origin 行为已验。
6. **改密后 `refresh()` 的时序**：规格要求改密撤销全部会话；前端在 `changePassword` 成功后立即 `me()` 重建登录态。若后端在同响应内已清除 cookie 且新 cookie 延迟生效，`me()` 可能短暂 401 落到 `signed_out`——联调时确认该路径的用户体验（当前实现如实显示登录表单，不伪造登录态）。

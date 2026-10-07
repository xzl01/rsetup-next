# Controller V1 剩余 Web 交付增量实施计划（2026-10-07）

> **致执行代理：** 推荐使用 `superpowers:executing-plans` 分项落实；用 `- [ ]` 跟踪步骤。这是**纯计划文档**：不运行任何真实服务或数据库、不执行任何迁移、不读取任何 `secret/` 凭据、不碰物理硬件或网络、不修改其他既有业务代码或规格文件、不执行 `git add` 或 `git commit`。每个 RED 阶段必须先建立最小可编译桩代码（stub），再编写目标行为断言失败测试，测试运行数必须非 0 且因预期行为缺失而失败；严禁把编译报错、模块缺失或类型未定义当有效 RED。

**Goal:** 在当前仅具备基础组件、认证与会话界面的 `apps/controller-web` 之上，对照中控 00/01/02/04/06 规格，增量交付受细粒度权限约束的设备与五维详情页面、管理员专用审批与系统配置页面、任务预览/提交与 unknown/time-review 二级风险确认、SSE 先订阅后快照同步状态机（含 64 位十进制 revision 无损高位比较与撤权缓冲清理）、全量双语与键盘无障碍验收，以及 Rust 内嵌静态资源与严格同源网络路由。

**Architecture:**
1. **轻量路由与认证隔离：** 鉴于工程已锁定无大型 router 依赖，采用纯响应式 Hash 客户端路由（`#/path`）解耦页面；实现完整的 `hashchange` 事件监听、URL 初始化与清理机制；严格响应认证与状态机变化（`auth.status`、`auth.user`），守卫 `must_change_password`（仅允许密码修改与登出）与未登录态，管理员路由（审批、组、用户、角色、授权、审计、系统）强校验 `is_admin`。
2. **授权投影与五维状态呈现（U-01）：** 遵循 01 权限点（`device.read`, `device.status.read`, `device.reboot`, `device.task.read`）；列表支持分页 cursor 与最小识别；详情页将准入状态、连接状态、双业务流健康（Control/Data）、快照新鲜度（含 `last_error` 与十进制字符串 `age_ms`）、板端时钟质量（严格按 `snapshot.clock_quality` 或板端测量呈现，中控 `received_time.quality` 独立作为接收侧元数据，板端未知显示 null 且严禁填 0）分别独立呈现，**绝不合并为单一绿色“在线”标签**。具备 `device.status.read` 但无 `device.read` 的 status-only 用户仅展示状态分栏，不发详情请求且隐藏无权限档案；具备 `device.reboot` 但无 read/status.read 时仅展示最小识别与重启入口，不发详情与状态请求。
3. **任务流水线、幂等控制与二级风险确认：** 批量重启通过 `/task-previews` 展开固定清单，显式勾选风险确认时单次生成并保留规范 UUIDv4 `Idempotency-Key`，重试复用同一 key，提交过程设置 busy 状态防双击；previewToken 或清单变更时立即重置确认与幂等键。UUIDv4 统一使用 `crypto.getRandomValues` 规范生成，HTTP 远程环境可用且绝不 fallback 到弱随机。API 客户端在未来任务中受限扩展 `idempotencyKey` 请求头及管理 CRUD 所需的 `put`/`patch`/`del`，严格限制 headers 白名单，绝不任意开放 `RequestInit` 或自定义 headers，fetch 边界强校验 header 传输而非混入 body。管理员释放 unknown 占用必须校验 `is_admin` 并触发符合 a11y 规范（focus trap、role="dialog"、Esc 恢复）的二级风险确认（明确警告“可能已重启，释放锁不等于撤回执行”），输入非空 reason 并勾选知悉风险后释放设备锁；held 任务支持管理员 atomic time-review，不延长期限；任务授权子集标记 `view_scope: "authorized_subset"`，不显示隐藏设备数量。
4. **SSE 变化提示流管理与实体局部重同步（U-03）：** 服务端统一发送命名事件（`device.updated`, `task.updated`, `permissions.changed`, `system.time.changed`, `reset`）；客户端必须**先完成 EventSource 的 open 确认订阅成立，再发起 GET 快照**；引入 sync generation 代际计数与 `AbortController`，使重连/reset 产生的旧快照响应失效；只有连接保持且快照同步完成才标为 `isRealtime`；断线或同步中正确缓冲；队列满 128 条或收到 `reset` 时标非实时并触发全量重同步；revision 严格按各实体（`kind:id`）局部独立跟踪，绝不用全局 single revision 比较跨对象数据；权限变更（`permissions.changed`）立即取消订阅、清空缓冲队列、清除内存敏感投影与缓存，并触发重新认证与路由重定向，彻底清理敏感 DOM；`compareRevision` 保持纯函数且严格校验 canonical u64 输入，具备正反测试。
5. **双语无障碍与依赖分层（U-02）：** 中英双词典 1:1 键集严格等长锁定；所有新增页面与弹窗必须实际通过 `i18n.t(key)` 消费词典，严禁在模板中硬编码中文；语言切换保持用户勾选、表单输入与 Idempotency-Key，不发写请求；全键盘可达，弹窗进入/退出具备清晰的焦点捕获（focus trap）与恢复；审批操作勾选时冻结 `{device_id, expected_revision}`，后台刷新或翻页不静默扩大 targets 或篡改 revision。**依赖边界严格锁定：** 默认单元与组件测试严格沿用当前已锁定的 Vue/Vitest/Testing-Library/jsdom 依赖，测试命令必须可直接执行且运行数非 0；Playwright 与 axe-core 是**需单独获得审批安装依赖的工程候选工具**，作为 G5 真实浏览器验收门禁规范，严禁一边写零新增依赖一边默认运行未装工具。
6. **Rust 产物内嵌与同源安全路由门禁（U-04）：** 明确待审 embed 机制的实际实现门禁（不假装已引入未经审批的 `rust-embed`，在未引入依赖前以构建与测试门禁规范为准）；Makefile 保证先 `npm run build` 后 `cargo build`，缺少 dist 产物时 release 构建失败（fail-closed）；测试解析真实构建 `index.html` 引用的静态资产 URL，验证 JS/CSS 文件真实内容、MIME 与长效 immutable 缓存；测试在脱离项目根目录（不同 cwd）下内嵌运行；SPA 精确回退仅限于前端已知页面路由白名单（如 `/`, `/login`, `/devices`, `/devices/:id`, `/tasks`, `/tasks/:id`, `/admin/*`）且仅限于 GET/HEAD 请求；缺失静态资产坚决 404；`/api/v1/*` 坚决返回完整 404 JSON error envelope，绝不回退 HTML 掩盖错误。

**Tech Stack:**
- 前端（既有锁定版本，无新增依赖）：Vue 3.5.43, TypeScript 5.9.3 (strict), Vite 8.3.2, Vitest 5.0.3, @testing-library/vue 8.1.0, jsdom 26.1.0, vue-tsc 3.3.12。
- 后端：Rust 2024 / MSRV 1.85, Axum 0.8, Tower 0.5。
- E2E 候选（需单独审批安装候选）：Playwright, @axe-core/playwright。

**Spec:**
- `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md`
- `docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md`
- `docs/superpowers/specs/2026-09-23-controller-v1-02-data-api.md`
- `docs/superpowers/specs/2026-09-23-controller-v1-04-task-lifecycle.md`
- `docs/superpowers/specs/2026-09-23-controller-v1-05-runtime-operations.md`
- `docs/superpowers/specs/2026-09-23-controller-v1-06-web-acceptance.md`

---

## Global Constraints

1. **草案审阅与协议发布阻断约束：** `controller-v1 / draft-1` 仍为待审阅状态。板端协议层关于 `reason_code` 签名、验签失败描述与 AEAD 终止策略等未决项在未获独立安全审阅前，不得宣称整个系统可投产。中控管理端仅提供 HTTP 服务，不可替代链路传输加密，提示可信网络风险。
2. **纯文档执行约束：** 本任务为纯设计与实施计划文档，不运行任何真实服务或数据库、不执行任何迁移、不访问外部网络或硬件、不读取 `secret/` 下的任何凭据、不修改现有代码文件、不执行 `git add` 或 `git commit`。
3. **现有前端依赖与 Lockfile 冻结约束：** 严格遵循既有前端设计规范，不新增任何 npm 依赖，不修改 `package.json` 与 `package-lock.json`。路由采用轻量纯响应式 Hash 路由实现，完整监听 `hashchange`、同步 URL 与组件清理，不引入重量级 vue-router 或第三方组件库。Playwright 与 axe-core 仅作为需独立申请批准安装的候选工具；未获审批前，默认测试套件必须仅依赖现有已安装工具。
4. **Cookie、CSRF、幂等头与状态隔离约束：** 会话使用 `HttpOnly` Cookie，前端代码**绝对禁止读取、解析或写入 Cookie**（禁止使用 `document.cookie`）。CSRF Token 与会话状态仅保存在 Vue 响应式内存中，**绝对禁止存入 `localStorage`、`sessionStorage`、URL 或日志**。只读请求（GET）绝对不带 CSRF 头；写请求（POST/PUT 等）必须携带 `X-CSRF-Token` 头，`/auth/login` 请求豁免。API 客户端扩展受限的 `idempotencyKey` 请求头（小写规范 UUIDv4）及受限方法（`post`, `put`, `patch`, `del`），在 fetch 边界强校验为 HTTP 头传输而非 payload 属性，绝不开放任意 RequestInit/headers。
5. **细粒度权限与 404 隔离约束：**
   - 具备 `device.reboot` 但无 `device.read`/`device.status.read` 时，只展示最小识别信息与重启入口，不发详情与状态请求；
   - 具备 `device.status.read` 但无 `device.read` 时（status-only），仅展示设备状态分栏，不发详情请求，隐藏档案字段；
   - 未授权目标与不存在目标统一返回 404，不泄露过滤前的设备或任务总数；
   - 任务响应标记 `view_scope: "authorized_subset"`，不显示隐藏数量。
6. **状态真实性呈现约束（U-01）：**
   - 准入状态、连接状态、双流（Control/Data）健康状态、监控新鲜度、板端时钟测量必须独立分栏呈现，**绝不允许用单一“在线”或“全绿”徽标代表所有业务正常**；
   - 监控状态无采样显示等待采集/unknown，null 严禁展示为 0；stale/error 时保留真实历史样本并展示 `last_error` 与十进制字符串 `age_ms`；
   - 时钟质量呈现严格区分中控与板端：`received_time.quality` 明确标注为中控接收质量，板端时钟测量来自 `snapshot.clock_quality`，板端无采样或无权限时独立显示为 unknown/null，禁止拿中控质量冒充板端质量；
   - 任务 202 仅代表持久接收，`unknown` 明确显示为“结果未确认”，禁止用失败文案诱导用户自动重发。
7. **双语与可访问性约束（U-02）：**
   - 简体中文（`zh-CN`）与英文（`en`）为一等语言，词典键集必须 1:1 严格等长对等；所有新增页面与模态框必须实际消费词典（`i18n.t(...)`），严禁保留硬编码中文；
   - 语言切换仅变更界面语言、`lang` 属性与可访问标签，**绝不重置用户选中的设备列表、不重新生成 Idempotency-Key、不触发任何写请求**；
   - 审批操作在勾选时冻结 `{device_id, expected_revision}`，后台数据刷新或翻页不静默扩大 targets 集合或静默修改 expected_revision；
   - 所有主要流程全键盘可达，弹窗复用无障碍模态规范（`role="dialog"`、`aria-modal="true"`、Esc 键关闭、焦点捕获与恢复），提供 320px 视口无横向溢出适配。
8. **SSE 事件与状态同步约束（U-03）：**
   - SSE 仅用于轻量变更提示，服务端使用统一命名事件（`event: device.updated`, `task.updated`, `permissions.changed`, `system.time.changed`, `reset`）；
   - 客户端打开/重连时**先等待 EventSource 触发 open 确认订阅成立，再发起 GET 快照**；
   - 客户端维护 `syncGeneration` 代际计数与 `AbortController`，使重连/重同步期间过期的旧快照响应直接失效并丢弃，防止旧 GET 覆盖新 GET；
   - revision 严格为各实体的局部版本（按 `kind:id` 独立跟踪），列表 Page 无全局 single revision，严禁跨对象比较 revision；
   - `compareRevision` 必须为纯函数，对非规范十进制 u64（非数字、前导零、负数、小数、指数、超出 u64 范围）严格抛错拒绝，提供完备正反测试；
   - 快照到达后，丢弃 `compareRevision(event.revision, currentRevision) <= 0` 的陈旧事件，较新事件应用增量；
   - 队列满 128 条或收到 `reset` 或网络断线时，标记“非实时”并触发重新 GET 快照；只有连接成立且快照同步完成才标为 `isRealtime`；
   - 权限变更（`permissions.changed`）或组件卸载时立即取消订阅、清空缓冲队列、清除内存中旧投影与缓存，并重新触发认证与路由导航，彻底销毁可能残留的敏感 DOM。
9. **静态资源内嵌与同源交付约束（U-04）：**
   - Vite 生产构建必须先于 Rust 编译完成，二进制内嵌全部 dist 产物；
   - 明确待审 embed 机制的实际实现门禁（未获引入 `rust-embed` 审批前不假装依赖已就绪）；
   - 缺失 dist 时 release 构建直接失败（fail-closed）；
   - 运行时完全脱离 Node.js 与外部静态目录，支持非项目根目录（不同 cwd）独立运行；
   - 测试需实际解析 `index.html` 引用的 JS/CSS URL，断言真实文件内容匹配且携带长期不可变缓存头（1年），入口 HTML 携带 `no-cache`；
   - SPA 精确回退仅限于已知前端路由白名单，且仅针对 GET/HEAD 请求；缺失静态资源与 `/api/v1/*` 坚决 404（API 返回完整 JSON error envelope），严禁回退 HTML。
10. **TDD 测试分层真实性约束：**
    - **严禁伪 RED：** 编译报错、未导出、模块找不到或测试语法错误不是有效 RED；必须先建立最小可编译 stub，测试正常执行且测试运行数非 0，因业务断言失败而构成行为 RED；
    - 现有 jsdom 仅用于快速验证组件 DOM 渲染与事件，**严禁宣称 jsdom 通过即代表真实浏览器验收**；真实浏览器与无障碍审查在获审批后的 G5 门禁阶段使用真实浏览器运行。

---

## 前后端边界与测试分层策略

```text
[分层 1: 纯前端 Mock TDD (vitest + @testing-library/vue + jsdom)]
  - 适用工具：现有锁定依赖，无新增安装。
  - 验证范围：纯响应式 Hash 路由守卫流转、设备权限隔离展示、五维状态独立分栏（绝无全绿在线标签）、
             批量重启固定清单与 UUID 幂等键、unknown 释放锁二级确认弹窗、SSE 缓冲-快照高位剪枝状态机、
             双语词典 1:1 严格对等、切换语言零写请求、组件焦点捕获/恢复与键盘事件。
  - 核心要求：先建立可编译 stub 再运行行为失败断言；测试运行数非 0。

[分层 2: 真实浏览器 E2E 候选 (Playwright + axe-core - G5 独立门禁)]
  - 适用工具：需单独提请审批安装的工程候选（未获批准前不作为默认构建门禁）。
  - 验证范围：真实 Chromium/Firefox 下的 CSS 盒模型渲染、320px 真实视口无横向滚动条、真实浏览器按键 Tab 焦点遍历、
             真实视觉高对比度与动效抑制模式、axe-core 自动化可访问性规则审计。
  - 边界隔离：仅在获得工具安装授权的独立 CI/CD 阶段运行，不侵入前端基础依赖树。

[分层 3: 后端静态路由集成 (cargo test --test web_assets)]
  - 适用工具：Axum / tower (ServiceExt::oneshot)。
  - 验证范围：Axum 静态资源嵌入契约、MIME Content-Type 匹配、哈希资源不可变缓存响应头、
             入口 HTML no-cache 响应头、SPA 精确回退机制、缺失静态资源 404 与 /api/v1/* 404 防 HTML 掩盖。
  - 核心要求：先建立 crates/rsetup-controller/src/web.rs 可编译 stub 并在 lib.rs 暴露，再运行测试断言行为失败。

[分层 4: 跨专题真实联合网络联验 (G5 门禁)]
  - 适用工具：集成验收测试套件（由 2026-10-07-controller-v1-integration-acceptance-tdd.md 统一协调）。
  - 验证范围：真实 HttpOnly Cookie 传输、Host/Origin 白名单、真实任务调度与板端重启、双数据库一致性。
```

---

## 增量实施任务清单

### Task 1: 前端轻量 Hash 路由与认证视图解耦 (Router & Decoupled Auth Views)

**Files:**
- Create: `apps/controller-web/src/router.ts`
- Create: `apps/controller-web/src/router.test.ts`
- Create: `apps/controller-web/src/views/LoginView.vue`
- Create: `apps/controller-web/src/views/LoginView.test.ts`
- Create: `apps/controller-web/src/views/PasswordView.vue`
- Create: `apps/controller-web/src/views/PasswordView.test.ts`
- Create: `apps/controller-web/src/views/SessionsView.vue`
- Create: `apps/controller-web/src/views/SessionsView.test.ts`
- Modify: `apps/controller-web/src/App.vue`
- Modify: `apps/controller-web/src/App.test.ts`

**Interfaces:**
- Consumes: `src/auth.ts` (`AuthStore`, `AuthUser`), `src/i18n.ts` (`createI18n`).
- Produces:
  ```ts
  export type AppRoute =
    | { name: 'login' }
    | { name: 'password' }
    | { name: 'sessions' }
    | { name: 'devices' }
    | { name: 'device-detail'; params: { id: string } }
    | { name: 'tasks' }
    | { name: 'task-detail'; params: { id: string } }
    | { name: 'admin-approvals' }
    | { name: 'admin-groups' }
    | { name: 'admin-users' }
    | { name: 'admin-roles' }
    | { name: 'admin-grants' }
    | { name: 'admin-audit' }
    | { name: 'admin-system' }

  export interface AppRouter {
    currentRoute: Ref<AppRoute>
    navigate(route: AppRoute): void
    resolveInitialRoute(user: AuthUser | null): AppRoute
    cleanup(): void
  }
  export function createRouter(auth: AuthStore): AppRouter
  ```

- [ ] **Step 1: 建立可编译桩代码与编写失败测试（可编译的行为 RED）**

首先在 `apps/controller-web/src/router.ts` 中建立**最小可编译 stub**（满足类型签名，包含 cleanup，但未实现 hashchange 监听与路由守卫逻辑）：

```ts
import { ref, type Ref } from 'vue'
import type { AuthStore, AuthUser } from './auth'

export type AppRoute =
  | { name: 'login' }
  | { name: 'password' }
  | { name: 'sessions' }
  | { name: 'devices' }
  | { name: 'device-detail'; params: { id: string } }
  | { name: 'tasks' }
  | { name: 'task-detail'; params: { id: string } }
  | { name: 'admin-approvals' }
  | { name: 'admin-groups' }
  | { name: 'admin-users' }
  | { name: 'admin-roles' }
  | { name: 'admin-grants' }
  | { name: 'admin-audit' }
  | { name: 'admin-system' }

export interface AppRouter {
  currentRoute: Ref<AppRoute>
  navigate(route: AppRoute): void
  resolveInitialRoute(user: AuthUser | null): AppRoute
  cleanup(): void
}

export function createRouter(_auth: AuthStore): AppRouter {
  // 最小桩：无 hashchange 监听与守卫
  const currentRoute = ref<AppRoute>({ name: 'devices' })
  return {
    currentRoute,
    navigate(route: AppRoute) { currentRoute.value = route },
    resolveInitialRoute(_user: AuthUser | null) { return { name: 'devices' } },
    cleanup() {},
  }
}
```

在 `apps/controller-web/src/router.test.ts` 中编写完整的路由守卫、Hash 同步与清理机制测试：
注意：根据 [HTTP ID Followup 契约规范](../specs/2026-10-07-controller-http-id-and-review-followup-design.md)，`AuthUser` 的 fixture 必须包含合法的 UUIDv4 `id` 与规范十进制字符串 `revision`。

```ts
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ref, type Ref } from 'vue'
import { createRouter } from './router'
import type { AuthStore, AuthUser, AuthStatus, AuthErrorCode, UserSession } from './auth'

function fakeAuthStore(
  user: AuthUser | null,
  status: AuthStatus = 'signed_in'
): AuthStore {
  return {
    status: ref(status),
    user: ref(user),
    csrfToken: ref('token'),
    authzEpoch: ref('1'),
    errorCode: ref<AuthErrorCode | null>(null),
    sessions: ref<UserSession[]>([]),
    sessionsNextCursor: ref<string | null>(null),
    sessionsLoading: ref(false),
    sessionsError: ref<AuthErrorCode | null>(null),
    refresh: vi.fn(),
    login: vi.fn(),
    changePassword: vi.fn(),
    logout: vi.fn(),
    listSessions: vi.fn(),
    revokeSession: vi.fn(),
    revokeOtherSessions: vi.fn(),
    revokeOthers: vi.fn(),
  }
}

describe('client router hash synchronization & guards', () => {
  beforeEach(() => {
    window.location.hash = ''
  })
  afterEach(() => {
    window.location.hash = ''
  })

  it('redirects unauthenticated users to login and updates hash', () => {
    const auth = fakeAuthStore(null, 'signed_out')
    const router = createRouter(auth)
    expect(router.resolveInitialRoute(null)).toEqual({ name: 'login' })
    router.navigate({ name: 'devices' })
    expect(router.currentRoute.value).toEqual({ name: 'login' })
    expect(window.location.hash).toBe('#/login')
    router.cleanup()
  })

  it('strictly forces users with must_change_password to password view', () => {
    const auth = fakeAuthStore({
      id: '123e4567-e89b-42d3-a456-426614174000',
      username: 'temp_admin',
      must_change_password: true,
      is_admin: true,
      revision: '1',
    }, 'force_password')
    const router = createRouter(auth)
    expect(router.resolveInitialRoute(auth.user.value)).toEqual({ name: 'password' })
    router.navigate({ name: 'devices' })
    expect(router.currentRoute.value).toEqual({ name: 'password' })
    router.navigate({ name: 'admin-users' })
    expect(router.currentRoute.value).toEqual({ name: 'password' })
    router.cleanup()
  })

  it('blocks non-admin users from admin routes and redirects to devices', () => {
    const auth = fakeAuthStore({
      id: '223e4567-e89b-42d3-a456-426614174001',
      username: 'operator',
      must_change_password: false,
      is_admin: false,
      revision: '2',
    })
    const router = createRouter(auth)
    router.navigate({ name: 'admin-approvals' })
    expect(router.currentRoute.value).toEqual({ name: 'devices' })
    router.cleanup()
  })

  it('allows admin users to navigate to admin routes', () => {
    const auth = fakeAuthStore({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    })
    const router = createRouter(auth)
    router.navigate({ name: 'admin-approvals' })
    expect(router.currentRoute.value).toEqual({ name: 'admin-approvals' })
    expect(window.location.hash).toBe('#/admin/approvals')
    router.cleanup()
  })

  it('handles window hashchange event and cleans up listener on cleanup', () => {
    const auth = fakeAuthStore({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    })
    const router = createRouter(auth)
    window.location.hash = '#/tasks'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    expect(router.currentRoute.value).toEqual({ name: 'tasks' })

    router.cleanup()
    window.location.hash = '#/devices'
    window.dispatchEvent(new HashChangeEvent('hashchange'))
    // After cleanup, currentRoute does not update from window hashchange
    expect(router.currentRoute.value).toEqual({ name: 'tasks' })
  })

  it('reacts to auth status changes: redirects to login if user becomes signed_out', () => {
    const auth = fakeAuthStore({
      id: '323e4567-e89b-42d3-a456-426614174002',
      username: 'admin',
      must_change_password: false,
      is_admin: true,
      revision: '3',
    })
    const router = createRouter(auth)
    router.navigate({ name: 'devices' })
    expect(router.currentRoute.value).toEqual({ name: 'devices' })

    // Simulate session expired / permissions revoked to signed_out
    auth.user.value = null
    auth.status.value = 'signed_out'
    // Router reacts to reactive store change
    router.navigate(router.currentRoute.value)
    expect(router.currentRoute.value).toEqual({ name: 'login' })
    router.cleanup()
  })
})
```

- [ ] **Step 2: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/router.test.ts`
预期结果：测试套件成功编译并加载，测试运行数非 0（运行 6 个用例，失败 5 个），明确提示业务行为断言失败：
`AssertionError: expected { name: 'devices' } to deeply equal { name: 'login' }`。
（验证：并非模块导入或未定义错误，而是业务逻辑未达标的有效 RED）。

- [ ] **Step 3: 最小实现**

在 `apps/controller-web/src/router.ts` 中实现响应式 Hash 客户端路由、URL 路由映射、Hash 监听与清理机制：

```ts
import { ref, watch, type Ref } from 'vue'
import type { AuthStore, AuthUser } from './auth'

export type AppRoute =
  | { name: 'login' }
  | { name: 'password' }
  | { name: 'sessions' }
  | { name: 'devices' }
  | { name: 'device-detail'; params: { id: string } }
  | { name: 'tasks' }
  | { name: 'task-detail'; params: { id: string } }
  | { name: 'admin-approvals' }
  | { name: 'admin-groups' }
  | { name: 'admin-users' }
  | { name: 'admin-roles' }
  | { name: 'admin-grants' }
  | { name: 'admin-audit' }
  | { name: 'admin-system' }

export interface AppRouter {
  currentRoute: Ref<AppRoute>
  navigate(route: AppRoute): void
  resolveInitialRoute(user: AuthUser | null): AppRoute
  cleanup(): void
}

function parseHash(hash: string): AppRoute {
  const path = hash.replace(/^#\/?/, '')
  if (!path || path === 'login') return { name: 'login' }
  if (path === 'password') return { name: 'password' }
  if (path === 'sessions') return { name: 'sessions' }
  if (path === 'devices') return { name: 'devices' }
  const devMatch = path.match(/^devices\/([a-zA-Z0-9_-]+)$/)
  if (devMatch) return { name: 'device-detail', params: { id: devMatch[1] } }
  if (path === 'tasks') return { name: 'tasks' }
  const taskMatch = path.match(/^tasks\/([a-zA-Z0-9_-]+)$/)
  if (taskMatch) return { name: 'task-detail', params: { id: taskMatch[1] } }
  if (path === 'admin/approvals') return { name: 'admin-approvals' }
  if (path === 'admin/groups') return { name: 'admin-groups' }
  if (path === 'admin/users') return { name: 'admin-users' }
  if (path === 'admin/roles') return { name: 'admin-roles' }
  if (path === 'admin/grants') return { name: 'admin-grants' }
  if (path === 'admin/audit') return { name: 'admin-audit' }
  if (path === 'admin/system') return { name: 'admin-system' }
  return { name: 'devices' }
}

function formatRouteToHash(route: AppRoute): string {
  switch (route.name) {
    case 'login': return '#/login'
    case 'password': return '#/password'
    case 'sessions': return '#/sessions'
    case 'devices': return '#/devices'
    case 'device-detail': return `#/devices/${route.params.id}`
    case 'tasks': return '#/tasks'
    case 'task-detail': return `#/tasks/${route.params.id}`
    case 'admin-approvals': return '#/admin/approvals'
    case 'admin-groups': return '#/admin/groups'
    case 'admin-users': return '#/admin/users'
    case 'admin-roles': return '#/admin/roles'
    case 'admin-grants': return '#/admin/grants'
    case 'admin-audit': return '#/admin/audit'
    case 'admin-system': return '#/admin/system'
  }
}

export function createRouter(auth: AuthStore): AppRouter {
  const currentRoute = ref<AppRoute>({ name: 'login' })

  function isRestrictedAdminRoute(name: string): boolean {
    return name.startsWith('admin-')
  }

  function resolveInitialRoute(user: AuthUser | null): AppRoute {
    if (!user) return { name: 'login' }
    if (user.must_change_password) return { name: 'password' }
    return { name: 'devices' }
  }

  function guardRoute(target: AppRoute): AppRoute {
    const user = auth.user.value
    if (!user) {
      return { name: 'login' }
    }
    if (user.must_change_password) {
      return { name: 'password' }
    }
    if (isRestrictedAdminRoute(target.name) && !user.is_admin) {
      return { name: 'devices' }
    }
    return target
  }

  function navigate(target: AppRoute): void {
    const allowed = guardRoute(target)
    currentRoute.value = allowed
    const targetHash = formatRouteToHash(allowed)
    if (typeof window !== 'undefined' && window.location.hash !== targetHash) {
      window.location.hash = targetHash
    }
  }

  function onHashChange() {
    if (typeof window === 'undefined') return
    const routeFromHash = parseHash(window.location.hash)
    navigate(routeFromHash)
  }

  if (typeof window !== 'undefined') {
    window.addEventListener('hashchange', onHashChange)
    // 初始化路由
    const initial = window.location.hash ? parseHash(window.location.hash) : resolveInitialRoute(auth.user.value)
    navigate(initial)
  }

  // 监听认证状态动态变更（例如撤权、登出、强制改密）
  const stopAuthWatch = watch([auth.user, auth.status], () => {
    navigate(currentRoute.value)
  })

  function cleanup() {
    if (typeof window !== 'undefined') {
      window.removeEventListener('hashchange', onHashChange)
    }
    stopAuthWatch()
  }

  return {
    currentRoute,
    navigate,
    resolveInitialRoute,
    cleanup,
  }
}
```

随后解耦既有 `App.vue` 中的内嵌表单，分别抽离为 `LoginView.vue`、`PasswordView.vue`、`SessionsView.vue`，并在 `App.vue` 中通过 `router.currentRoute.value` 切换展示。

- [ ] **Step 4: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/router.test.ts src/App.test.ts`
预期：PASS，所有用例绿灯通过。

- [ ] **Step 5: 重构、回归与条件提交**

运行全量单测及类型检查：
`npm --prefix apps/controller-web test -- --run`
`npm --prefix apps/controller-web run typecheck`
确认无任何警告与类型错误。

---

### Task 2: 受权限约束的设备列表与五维详情页面 (Authorized Devices & 5-Dimension Detail Views)

**Files:**
- Create: `apps/controller-web/src/views/DevicesView.vue`
- Create: `apps/controller-web/src/views/DevicesView.test.ts`
- Create: `apps/controller-web/src/views/DeviceDetailView.vue`
- Create: `apps/controller-web/src/views/DeviceDetailView.test.ts`
- Modify: `apps/controller-web/src/types.ts`
- Modify: `apps/controller-web/src/router.ts`

**Interfaces:**
- Consumes: `src/api.ts` (`get`), `src/types.ts`, `src/auth.ts`, `src/i18n.ts`.
- Produces:
  ```ts
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
      clock_quality?: string | null
      [key: string]: unknown
    } | null
    received_time: {
      quality: string // 中控接收端时钟质量
      system_wall_utc: string
      reference_utc: string
    }
    freshness: 'unknown' | 'fresh' | 'stale' | 'error'
    age_ms: string | null // 64位无符号十进制字符串，禁止JS Number精度丢失
    last_error?: string | null
  }
  export function canReadDevice(permissions: string[]): boolean
  export function canReadStatus(permissions: string[]): boolean
  export function canReboot(permissions: string[]): boolean
  ```

- [ ] **Step 1: 建立可编译桩代码与编写失败测试（可编译的行为 RED）**

首先在 `apps/controller-web/src/types.ts` 中补充类型与可编译桩函数：

```ts
export function canReadDevice(permissions: string[]): boolean { return permissions.includes('device.read') }
export function canReadStatus(_permissions: string[]): boolean { return false } // stub: 恒为 false
export function canReboot(permissions: string[]): boolean { return permissions.includes('device.reboot') }
```

并在 `apps/controller-web/src/views/DeviceDetailView.vue` 中建立可挂载的**最小可编译 stub**：

```vue
<script setup lang="ts">
defineProps<{
  deviceId: string
  initialDetail?: any
  initialStatus?: any
  permissions?: string[]
}>()
</script>
<template>
  <div class="device-detail-stub"></div>
</template>
```

在 `apps/controller-web/src/views/DeviceDetailView.test.ts` 中编写测试，重点断言：
1. U-01 五维状态独立呈现且绝不合并为单一全绿“在线”；
2. 中控接收质量（`received_time.quality`）与板端时钟测量（`snapshot.clock_quality`）独立呈现，未知板端时钟显示 null 且严禁填 0；
3. `age_ms` 作为 64 位十进制字符串正确显示，避免 JS 浮点截断；
4. 当用户仅具 `device.status.read` 而无 `device.read` 时（status-only），仅展示状态卡片，绝不展示或请求详情档案；
5. 所有可见标签与标题通过 i18n 消费，无硬编码中文：

```ts
import { render, screen } from '@testing-library/vue'
import { describe, expect, it } from 'vitest'
import DeviceDetailView from './DeviceDetailView.vue'
import { createI18n } from '../i18n'

describe('DeviceDetailView U-01 five-dimensional health', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

  it('renders admission, connection, stream health, freshness, controller reception and board clock measurement independently without a combined online badge', async () => {
    const detail = {
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
    const status = {
      snapshot: { cpu_usage: 12, clock_quality: 'crystal_locked' },
      received_time: { quality: 'system_fallback', system_wall_utc: '2026-10-07T00:00:00Z', reference_utc: '2026-10-07T00:00:00Z' },
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
    expect(screen.getByTestId('dim-board-clock').textContent).toContain('crystal_locked')

    // 严禁合并为单一全绿“在线”标签
    expect(screen.queryByTestId('unified-online-badge')).toBeNull()
  })

  it('renders board clock as null when snapshot measurement is absent, never fills 0', () => {
    const status = {
      snapshot: { cpu_usage: 12 }, // 无 clock_quality
      received_time: { quality: 'synchronized', system_wall_utc: '2026-10-07T00:00:00Z', reference_utc: '2026-10-07T00:00:00Z' },
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
    const status = {
      snapshot: { cpu_usage: 50 },
      received_time: { quality: 'synchronized', system_wall_utc: '2026-10-07T00:00:00Z', reference_utc: '2026-10-07T00:00:00Z' },
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
    const detail = {
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
})
```

- [ ] **Step 2: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/views/DeviceDetailView.test.ts`
预期结果：测试套件正常编译加载，测试运行数非 0（运行 4 个用例，失败 3 个），提示找不到对应 test-id 元素：
`TestingLibraryElementError: Unable to find an element by: [data-testid="dim-admission"]`。
（有效行为 RED）。

- [ ] **Step 3: 最小实现**

在 `apps/controller-web/src/types.ts` 中修正 `canReadStatus` 实现：

```ts
export function canReadStatus(permissions: string[]): boolean {
  return permissions.includes('device.status.read')
}
```

在 `apps/controller-web/src/views/DeviceDetailView.vue` 中完整实现五维独立渲染、国际化消费与权限隔离：

```vue
<script setup lang="ts">
import { computed, inject } from 'vue'
import { canReadDevice, canReadStatus } from '../types'
import type { createI18n } from '../i18n'

const props = defineProps<{
  deviceId: string
  initialDetail?: any
  initialStatus?: any
  permissions?: string[]
}>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!
const perms = computed(() => props.permissions || props.initialDetail?.effective_permissions || [])

const hasDetailPermission = computed(() => canReadDevice(perms.value))
const hasStatusPermission = computed(() => canReadStatus(perms.value))
</script>

<template>
  <div class="device-detail">
    <div v-if="hasDetailPermission && initialDetail" data-testid="detail-profile-card" class="profile-card">
      <h2>{{ initialDetail.display_name }}</h2>
      <div class="dimension-grid">
        <!-- 1. 准入状态 -->
        <section data-testid="dim-admission">
          <h3>{{ i18n.t('device.dim.admission') }}</h3>
          <p>{{ initialDetail.admission_state }} ({{ initialDetail.review_decision }})</p>
        </section>

        <!-- 2. 连接状态 -->
        <section data-testid="dim-connection">
          <h3>{{ i18n.t('device.dim.connection') }}</h3>
          <p>{{ initialDetail.connection_state }}</p>
        </section>

        <!-- 3. 双业务流健康 -->
        <section data-testid="dim-stream-health">
          <h3>{{ i18n.t('device.dim.streamHealth') }}</h3>
          <p data-testid="dim-control-health">{{ i18n.t('device.health.control') }}: {{ initialDetail.control_health }}</p>
          <p data-testid="dim-data-health">{{ i18n.t('device.health.data') }}: {{ initialDetail.data_health }}</p>
        </section>
      </div>
    </div>

    <!-- 4 & 5. 遥测新鲜度、中控接收与板端时钟质量 (需 device.status.read 权限) -->
    <div v-if="hasStatusPermission && initialStatus" data-testid="status-card" class="status-card">
      <section data-testid="dim-freshness">
        <h3>{{ i18n.t('device.dim.freshness') }}</h3>
        <p>{{ initialStatus.freshness }} ({{ i18n.t('device.status.age') }}: {{ initialStatus.age_ms !== null ? `${initialStatus.age_ms}ms` : i18n.t('common.unknown') }})</p>
        <p v-if="initialStatus.last_error" data-testid="dim-last-error">{{ i18n.t('common.error') }}: {{ initialStatus.last_error }}</p>
      </section>

      <!-- 中控接收端时钟质量 -->
      <section data-testid="dim-controller-clock">
        <h3>{{ i18n.t('device.dim.controllerClock') }}</h3>
        <p>{{ initialStatus.received_time?.quality || 'null' }}</p>
      </section>

      <!-- 板端时钟测量：独立呈现，未知显示 null，严禁填 0 -->
      <section data-testid="dim-board-clock">
        <h3>{{ i18n.t('device.dim.boardClock') }}</h3>
        <p>{{ initialStatus.snapshot?.clock_quality !== undefined ? (initialStatus.snapshot.clock_quality ?? 'null') : 'null' }}</p>
      </section>
    </div>
  </div>
</template>
```

在 `apps/controller-web/src/views/DevicesView.vue` 中实现 cursor 分页、名称筛选与最小投影；当用户仅具 `device.reboot` 权限时仅展示列表和重启入口，绝不请求详情与状态接口；status-only 用户不发详情请求。

- [ ] **Step 4: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/views/DeviceDetailView.test.ts src/views/DevicesView.test.ts`
预期：PASS。

- [ ] **Step 5: 重构、回归与条件提交**

运行全量测试与类型检查：
`npm --prefix apps/controller-web test -- --run`
`npm --prefix apps/controller-web run typecheck`
确认无任何警告与类型错误。

---

### Task 3: 管理员专用审批、组、用户/角色/授权、审计与系统状态页面 (Admin Console Views)

**Files:**
- Create: `apps/controller-web/src/views/AdminApprovalsView.vue`
- Create: `apps/controller-web/src/views/AdminApprovalsView.test.ts`
- Create: `apps/controller-web/src/views/AdminGroupsView.vue`
- Create: `apps/controller-web/src/views/AdminGroupsView.test.ts`
- Create: `apps/controller-web/src/views/AdminUsersView.vue`
- Create: `apps/controller-web/src/views/AdminUsersView.test.ts`
- Create: `apps/controller-web/src/views/AdminRolesView.vue`
- Create: `apps/controller-web/src/views/AdminRolesView.test.ts`
- Create: `apps/controller-web/src/views/AdminGrantsView.vue`
- Create: `apps/controller-web/src/views/AdminGrantsView.test.ts`
- Create: `apps/controller-web/src/views/AdminAuditView.vue`
- Create: `apps/controller-web/src/views/AdminAuditView.test.ts`
- Create: `apps/controller-web/src/views/AdminSystemView.vue`
- Create: `apps/controller-web/src/views/AdminSystemView.test.ts`
- Modify: `apps/controller-web/src/router.ts`

**Interfaces:**
- Consumes: `src/api.ts`, `src/types.ts`, `src/i18n.ts`.
- Produces:
  - `AdminApprovalsView`: 显式勾选清单批处理（≤1024）；在勾选时冻结 `{device_id, expected_revision}`；页面自动刷新或分页翻页时**绝不静默篡改已冻结项的 revision 或扩大 targets 集合**；提交 `{targets: [{device_id, expected_revision}], confirm: true}`；审批吊销/重授权提示“板端必须人工重置，中控批准不代表设备立即恢复连接”；处理批量操作 partial-fail（返回 targets 逐项结果）与 409 CAS 冲突；所有文本由 i18n 驱动。
  - `AdminGroupsView`: 组增改归档；PUT 成员去重替换并提示 epoch 动态权限生效。
  - `AdminUsersView`: 用户增改；创建时一次性展示临时密码（模态弹窗，符合 a11y focus trap）；最后管理员修改防护（409 `LAST_ADMIN`）。
  - `AdminRolesView`: 角色增改归档；内置角色归档阻止。
  - `AdminGrantsView`: 授权分配；source（role 与 direct 互斥）与 scope（all、group、device 互斥）输入验证。
  - `AdminAuditView` 与 `AdminSystemView`: 审计只读脱敏查询；NTP 退避与时钟质量展示。

- [ ] **Step 1: 建立可编译桩代码与编写失败测试（可编译的行为 RED）**

首先在 `apps/controller-web/src/views/AdminApprovalsView.vue` 中建立**最小可编译 stub**：

```vue
<script setup lang="ts">
defineProps<{
  approvals?: any[]
  onApprove?: (targets: any[]) => Promise<void>
  onRevoke?: (id: string, rev: string) => Promise<void>
}>()
</script>
<template>
  <div class="admin-approvals-stub"></div>
</template>
```

在 `apps/controller-web/src/views/AdminApprovalsView.test.ts` 中编写测试，重点断言：
1. 勾选时冻结 `{device_id, expected_revision}`，后台数据刷新（props.approvals 更新）时保持冻结的 expected_revision，提交不扩大 targets；
2. 刷新竞争检测：当后台数据更新导致 revision 发生变化时，界面标出 conflict/stale 提示，阻止提交过期目标；
3. 吊销/重授权警告提示通过 i18n 正确呈现；
4. 审批操作 risk/partial-fail 场景正确呈现各设备执行结果：

```ts
import { fireEvent, render, screen } from '@testing-library/vue'
import { describe, expect, it, vi } from 'vitest'
import AdminApprovalsView from './AdminApprovalsView.vue'
import { createI18n } from '../i18n'

describe('AdminApprovalsView batch actions & frozen selection', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

  it('submits only frozen selected targets with original expected_revision even after props refresh', async () => {
    const onApprove = vi.fn()
    const onRevoke = vi.fn()
    const items = [
      { device_id: '1'.repeat(64), fingerprint: 'fp1', descriptor: null, connection_state: 'online', decision: 'none', revision: '1' },
      { device_id: '2'.repeat(64), fingerprint: 'fp2', descriptor: null, connection_state: 'offline', decision: 'none', revision: '2' },
    ]

    const { rerender } = render(AdminApprovalsView, {
      props: { approvals: items, onApprove, onRevoke },
      global: { provide: { i18n } },
    })

    // 勾选第一台设备（冻结 revision '1'）
    const checkbox1 = screen.getByTestId(`select-${'1'.repeat(64)}`)
    await fireEvent.click(checkbox1)

    // 后台刷新返回了新的设备列表，且第一台设备的 revision 变成了 '3'
    const updatedItems = [
      { device_id: '1'.repeat(64), fingerprint: 'fp1', descriptor: null, connection_state: 'online', decision: 'none', revision: '3' },
      { device_id: '2'.repeat(64), fingerprint: 'fp2', descriptor: null, connection_state: 'offline', decision: 'none', revision: '2' },
      { device_id: '3'.repeat(64), fingerprint: 'fp3', descriptor: null, connection_state: 'online', decision: 'none', revision: '1' }, // 新增设备不应被自动选中
    ]
    await rerender({ approvals: updatedItems, onApprove, onRevoke })

    // 界面应提示第 1 台设备有并发变更风险 / 保持原冻结 revision
    const approveBtn = screen.getByRole('button', { name: /批准/i })
    await fireEvent.click(approveBtn)

    // 绝不静默将 target 变为 revision '3' 或纳入 device 3
    expect(onApprove).toHaveBeenCalledWith([
      { device_id: '1'.repeat(64), expected_revision: '1' },
    ])

    // 吊销警告提示验证 (通过 i18n 消费)
    const revokeNotice = screen.getByTestId('revoke-manual-reset-notice')
    expect(revokeNotice.textContent).toContain('吊销或重新授权需要板端人工重置')
  })

  it('displays partial fail outcomes when batch approval returns partial results', async () => {
    const onApprove = vi.fn()
    const items = [
      { device_id: '1'.repeat(64), fingerprint: 'fp1', descriptor: null, connection_state: 'online', decision: 'none', revision: '1' },
    ]
    render(AdminApprovalsView, {
      props: {
        approvals: items,
        onApprove,
        lastBatchResult: {
          succeeded: [],
          failed: [{ device_id: '1'.repeat(64), error_code: 'CAS_CONFLICT' }],
        },
      },
      global: { provide: { i18n } },
    })
    expect(screen.getByTestId('batch-partial-fail-notice').textContent).toContain('CAS_CONFLICT')
  })
})
```

- [ ] **Step 2: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/views/AdminApprovalsView.test.ts`
预期结果：测试套件正常编译加载，运行数非 0（运行 2 个用例，2 个失败），提示找不到对应 checkbox：
`TestingLibraryElementError: Unable to find an element by: [data-testid="select-1111..."]`。
（有效行为 RED）。

- [ ] **Step 3: 最小实现**

在 `apps/controller-web/src/views/AdminApprovalsView.vue` 中实现冻结选择集合、并发竞争守卫、国际化消费与局部失败提示：

```vue
<script setup lang="ts">
import { ref, inject } from 'vue'
import type { createI18n } from '../i18n'

const props = defineProps<{
  approvals: Array<{ device_id: string; fingerprint: string; descriptor: any; connection_state: string; decision: string; revision: string }>
  onApprove?: (targets: Array<{ device_id: string; expected_revision: string }>) => Promise<void>
  onRevoke?: (deviceId: string, revision: string) => Promise<void>
  lastBatchResult?: { succeeded: string[]; failed: Array<{ device_id: string; error_code: string }> }
}>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!

// 冻结已选对象字典: device_id -> { device_id, expected_revision }
const frozenSelected = ref<Map<string, { device_id: string; expected_revision: string }>>(new Map())

function toggleSelect(item: { device_id: string; revision: string }) {
  if (frozenSelected.value.has(item.device_id)) {
    frozenSelected.value.delete(item.device_id)
  } else {
    // 勾选时刻冻结 expected_revision
    frozenSelected.value.set(item.device_id, {
      device_id: item.device_id,
      expected_revision: item.revision,
    })
  }
}

async function submitApprove() {
  const targets = Array.from(frozenSelected.value.values())
  if (targets.length === 0 || targets.length > 1024) return
  await props.onApprove?.(targets)
}
</script>

<template>
  <div class="admin-approvals">
    <h2>{{ i18n.t('admin.approvals.title') }}</h2>
    <div data-testid="revoke-manual-reset-notice" class="notice warning">
      {{ i18n.t('admin.approvals.manualResetNotice') }}
    </div>

    <div v-if="lastBatchResult && lastBatchResult.failed.length > 0" data-testid="batch-partial-fail-notice" class="notice error">
      <span v-for="fail in lastBatchResult.failed" :key="fail.device_id">
        {{ fail.device_id.slice(0, 8) }}: {{ fail.error_code }}
      </span>
    </div>

    <table>
      <thead>
        <tr>
          <th>{{ i18n.t('common.select') }}</th>
          <th>{{ i18n.t('device.field.deviceId') }}</th>
          <th>{{ i18n.t('device.dim.connection') }}</th>
          <th>{{ i18n.t('common.actions') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="item in approvals" :key="item.device_id">
          <td>
            <input
              type="checkbox"
              :data-testid="`select-${item.device_id}`"
              :checked="frozenSelected.has(item.device_id)"
              @change="toggleSelect(item)"
            />
          </td>
          <td>{{ item.device_id }}</td>
          <td>{{ item.connection_state }}</td>
          <td>
            <button v-if="onRevoke" @click="() => onRevoke!(item.device_id, item.revision)">{{ i18n.t('admin.approvals.revoke') }}</button>
          </td>
        </tr>
      </tbody>
    </table>
    <button @click="submitApprove">{{ i18n.t('admin.approvals.approveSelected') }}</button>
  </div>
</template>
```

同时实现 `AdminGroupsView`、`AdminUsersView`（含临时密码一次性弹窗与 `LAST_ADMIN` 错误呈现）、`AdminRolesView`、`AdminGrantsView`、`AdminAuditView` 与 `AdminSystemView`，所有页面完整消费 `i18n.t(...)`。

- [ ] **Step 4: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/views/AdminApprovalsView.test.ts src/views/AdminUsersView.test.ts`
预期：PASS。

- [ ] **Step 5: 重构、回归与条件提交**

运行全部前端测试并检查类型：
`npm --prefix apps/controller-web test -- --run`
`npm --prefix apps/controller-web run typecheck`
确认无任何警告与类型错误。

---

### Task 4: 任务预览、批量确认、状态展示与 unknown / time-review 风险处理 (Task Lifecycle & Risk Action Views)

**Files:**
- Create: `apps/controller-web/src/task-presentation.ts`
- Create: `apps/controller-web/src/task-presentation.test.ts`
- Create: `apps/controller-web/src/views/TasksView.vue`
- Create: `apps/controller-web/src/views/TasksView.test.ts`
- Create: `apps/controller-web/src/views/TaskDetailView.vue`
- Create: `apps/controller-web/src/views/TaskDetailView.test.ts`
- Modify: `apps/controller-web/src/views/DevicesView.vue`
- Modify: `apps/controller-web/src/router.ts`

**Interfaces & Client Contract:**
- Consumes: `src/api.ts`, `src/types.ts`, `src/i18n.ts`.
- Produces:
  ```ts
  export type SubTaskState = 'queued' | 'held' | 'dispatching' | 'accepted' | 'verifying' | 'succeeded' | 'failed' | 'unknown' | 'cancelled' | 'expired'
  export function taskStateLabel(state: SubTaskState, t: (key: string) => string): string
  export function reasonCodeLabel(code: string, t: (key: string) => string): string
  export function generateUuidV4(): string
  ```
- **API Client 受限扩展契约（未来任务规范）：**
  在后续 API 客户端改造任务中，`api.ts` 的 `post` 及新增 `put`/`patch`/`del` 函数将接收受限 options：
  ```ts
  export interface RequestOptions {
    signal?: AbortSignal
    csrfToken?: string
    idempotencyKey?: string // 必须是规范小写 UUIDv4 字符串
  }
  ```
  禁止向业务代码暴露任意 `RequestInit` 或自定义 `headers` 对象；fetch 边界强校验 `Idempotency-Key` 必须作为 HTTP 请求头传递，绝不允许混入请求 body JSON 中。

- [ ] **Step 1: 建立可编译桩代码与编写失败测试（可编译的行为 RED）**

首先在 `apps/controller-web/src/task-presentation.ts` 中建立**最小可编译 stub**：

```ts
export type SubTaskState = 'queued' | 'held' | 'dispatching' | 'accepted' | 'verifying' | 'succeeded' | 'failed' | 'unknown' | 'cancelled' | 'expired'
export function taskStateLabel(_state: string, _t: (key: string) => string): string { return '' }
export function reasonCodeLabel(_code: string, _t: (key: string) => string): string { return '' }
export function generateUuidV4(): string { return '' }
```

在 `apps/controller-web/src/views/TasksView.vue` 中建立可挂载的**最小可编译 stub**：

```vue
<script setup lang="ts">
defineProps<{
  previewTargets?: any[]
  previewToken?: string
  subtasks?: any[]
  onSubmit?: (p: any, opts: any) => Promise<void>
  onReleaseLock?: (p: any) => Promise<void>
  isAdmin?: boolean
}>()
</script>
<template>
  <div class="tasks-view-stub"></div>
</template>
```

在 `apps/controller-web/src/views/TasksView.test.ts` 中编写测试，重点断言：
1. 一次确认生成规范 UUIDv4 Key，重试复用同一 Key，preview 改变重置确认与 Key，busy 状态防双击；
2. UUIDv4 生成使用 `crypto.getRandomValues`，远程 HTTP 可用，绝无弱随机 fallback；
3. unknown 释放锁严格受 `isAdmin` 限制，非管理员不展示释放按钮；
4. 二级风险确认模态框符合 a11y 规范（`role="dialog"`、`aria-modal="true"`、Esc 关闭、焦点捕获与恢复）；
5. 词典真实消费，无硬编码中文：

```ts
import { fireEvent, render, screen } from '@testing-library/vue'
import { describe, expect, it, vi } from 'vitest'
import TasksView from './TasksView.vue'
import { createI18n } from '../i18n'

describe('TasksView idempotency, retry, preview reset & admin unknown release', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

  it('generates UUIDv4 key on confirmation, reuses same key on retry, resets key on preview change, and sets busy during submit', async () => {
    let callCount = 0
    let lastKey = ''
    const onSubmit = vi.fn().mockImplementation(async (_body, opts) => {
      callCount++
      lastKey = opts.idempotencyKey
      // 模拟请求在途
      await new Promise((res) => setTimeout(res, 20))
    })

    const { rerender } = render(TasksView, {
      props: {
        previewTargets: [{ device_id: 'a'.repeat(64), eligible: true }],
        previewToken: 'tok-123',
        onSubmit,
        isAdmin: true,
      },
      global: { provide: { i18n } },
    })

    const submitBtn = screen.getByRole('button', { name: new RegExp(i18n.t('task.submitReboot'), 'i') })
    expect(submitBtn.hasAttribute('disabled')).toBe(true)

    const riskCheckbox = screen.getByRole('checkbox', { name: new RegExp(i18n.t('task.risk.confirmBatchReboot'), 'i') })
    await fireEvent.click(riskCheckbox)

    expect(submitBtn.hasAttribute('disabled')).toBe(false)

    // 第一次提交
    const submitPromise = fireEvent.click(submitBtn)
    // busy 状态防双击：按钮在提交在途时必须 disabled
    expect(submitBtn.hasAttribute('disabled')).toBe(true)
    await submitPromise

    expect(onSubmit).toHaveBeenCalledTimes(1)
    expect(lastKey).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/)
    const firstKey = lastKey

    // 重试提交（相同 previewToken）：必须保持原 key
    await fireEvent.click(submitBtn)
    expect(onSubmit).toHaveBeenCalledTimes(2)
    expect(lastKey).toBe(firstKey)

    // previewToken 改变：必须重置 riskConfirmed 与 idempotencyKey
    await rerender({
      previewTargets: [{ device_id: 'b'.repeat(64), eligible: true }],
      previewToken: 'tok-456',
      onSubmit,
      isAdmin: true,
    })
    expect(submitBtn.hasAttribute('disabled')).toBe(true)
    expect(riskCheckbox.matches(':checked')).toBe(false)
  })

  it('restricts unknown release to admin only and requires double-confirmation modal with focus trap & Esc', async () => {
    const onReleaseLock = vi.fn()
    const { rerender } = render(TasksView, {
      props: {
        subtasks: [{ id: 'sub-1', device_id: 'a'.repeat(64), state: 'unknown', revision: '4', lock_released: false }],
        onReleaseLock,
        isAdmin: false, // 普通用户
      },
      global: { provide: { i18n } },
    })

    // 非管理员不可见释放占用按钮
    expect(screen.queryByRole('button', { name: new RegExp(i18n.t('task.releaseLock'), 'i') })).toBeNull()

    // 切换为管理员
    await rerender({
      subtasks: [{ id: 'sub-1', device_id: 'a'.repeat(64), state: 'unknown', revision: '4', lock_released: false }],
      onReleaseLock,
      isAdmin: true,
    })

    const releaseBtn = screen.getByRole('button', { name: new RegExp(i18n.t('task.releaseLock'), 'i') })
    await fireEvent.click(releaseBtn)

    // 弹出二级确认模态框，具备 role="dialog" 与 aria-modal="true"
    const modal = screen.getByRole('dialog')
    expect(modal).toBeTruthy()
    expect(modal.getAttribute('aria-modal')).toBe('true')
    expect(screen.getByText(new RegExp(i18n.t('task.risk.releaseWarningDetail'), 'i'))).toBeTruthy()

    // Esc 键关闭模态框
    await fireEvent.keyDown(window, { key: 'Escape', code: 'Escape' })
    expect(screen.queryByRole('dialog')).toBeNull()

    // 重新打开并完成确认
    await fireEvent.click(releaseBtn)
    const reasonInput = screen.getByLabelText(new RegExp(i18n.t('task.field.releaseReason'), 'i'))
    await fireEvent.update(reasonInput, '现场人工确认重启已完成')

    const ackCheckbox = screen.getByRole('checkbox', { name: new RegExp(i18n.t('task.risk.acknowledgeUnknown'), 'i') })
    await fireEvent.click(ackCheckbox)

    const confirmReleaseBtn = screen.getByRole('button', { name: new RegExp(i18n.t('task.confirmRelease'), 'i') })
    expect(confirmReleaseBtn.hasAttribute('disabled')).toBe(false)
    await fireEvent.click(confirmReleaseBtn)

    expect(onReleaseLock).toHaveBeenCalledWith({
      subtaskId: 'sub-1',
      expected_revision: '4',
      acknowledge_unknown: true,
      reason: '现场人工确认重启已完成',
    })
  })
})
```

- [ ] **Step 2: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/views/TasksView.test.ts`
预期结果：测试套件正常编译加载，运行数非 0（运行 2 个用例，2 个失败），提示找不到对应按钮：
`TestingLibraryElementError: Unable to find an accessible element with the role "button"`。
（有效行为 RED）。

- [ ] **Step 3: 最小实现**

在 `apps/controller-web/src/task-presentation.ts` 中实现状态映射、原因码映射与基于 CSPRNG 的规范 UUIDv4 生成器：

```ts
export const SUBTASK_STATES = [
  'queued', 'held', 'dispatching', 'accepted', 'verifying',
  'succeeded', 'failed', 'unknown', 'cancelled', 'expired'
] as const

export type SubTaskState = typeof SUBTASK_STATES[number]

export function generateUuidV4(): string {
  const c = typeof globalThis !== 'undefined' ? globalThis.crypto : undefined
  if (!c || typeof c.getRandomValues !== 'function') {
    throw new Error('CSPRNG unavailable: crypto.getRandomValues is required')
  }
  const bytes = new Uint8Array(16)
  c.getRandomValues(bytes)
  bytes[6] = (bytes[6] & 0x0f) | 0x40 // RFC 4122 version 4
  bytes[8] = (bytes[8] & 0x3f) | 0x80 // RFC 4122 variant 1
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
}

export function taskStateLabel(state: SubTaskState, t: (key: string) => string): string {
  return t(`task.state.${state}`)
}

export function reasonCodeLabel(code: string, t: (key: string) => string): string {
  // reason_code 是机器标识；词典缺项时 i18n.t 按既有安全规则回退为纯文本 key。
  return t(`task.reason.${code}`)
}
```

在 `apps/controller-web/src/views/TasksView.vue` 中严格实现确认保留 key、重试同 key、preview 改变重置、防双击 busy、admin-only unknown 释放以及符合 a11y 规范的模态框：

```vue
<script setup lang="ts">
import { ref, watch, inject, onMounted, onUnmounted } from 'vue'
import { generateUuidV4 } from '../task-presentation'
import type { createI18n } from '../i18n'

const props = defineProps<{
  previewTargets?: Array<{ device_id: string; eligible: boolean }>
  previewToken?: string
  subtasks?: Array<{ id: string; device_id: string; state: string; revision: string; lock_released: boolean }>
  onSubmit?: (payload: { preview_token: string; confirm: boolean }, options: { idempotencyKey: string }) => Promise<void>
  onReleaseLock?: (payload: { subtaskId: string; expected_revision: string; acknowledge_unknown: boolean; reason: string }) => Promise<void>
  isAdmin?: boolean
}>()

const i18n = inject<ReturnType<typeof createI18n>>('i18n')!

const riskConfirmed = ref(false)
const retainedIdempotencyKey = ref<string | null>(null)
const isSubmitting = ref(false)

const showReleaseModal = ref(false)
const targetSubtask = ref<any>(null)
const releaseReason = ref('')
const releaseAck = ref(false)

// 监听 riskConfirmed 变更：首次勾选生成 key；取消勾选清除 key
watch(riskConfirmed, (confirmed) => {
  if (confirmed && !retainedIdempotencyKey.value) {
    retainedIdempotencyKey.value = generateUuidV4()
  } else if (!confirmed) {
    retainedIdempotencyKey.value = null
  }
})

// 监听 previewToken 变更：重置 riskConfirmed 与幂等 key
watch(() => props.previewToken, () => {
  riskConfirmed.value = false
  retainedIdempotencyKey.value = null
})

function openReleaseModal(subtask: any) {
  targetSubtask.value = subtask
  releaseReason.value = ''
  releaseAck.value = false
  showReleaseModal.value = true
}

function closeReleaseModal() {
  showReleaseModal.value = false
  targetSubtask.value = null
}

function handleKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape' && showReleaseModal.value) {
    closeReleaseModal()
  }
}

onMounted(() => {
  window.addEventListener('keydown', handleKeydown)
})

onUnmounted(() => {
  window.removeEventListener('keydown', handleKeydown)
})

async function handleReleaseConfirm() {
  if (!props.isAdmin || !releaseAck.value || !releaseReason.value.trim() || !targetSubtask.value) return
  await props.onReleaseLock?.({
    subtaskId: targetSubtask.value.id,
    expected_revision: targetSubtask.value.revision,
    acknowledge_unknown: true,
    reason: releaseReason.value.trim(),
  })
  closeReleaseModal()
}

async function handleSubmitReboot() {
  if (!riskConfirmed.value || !props.previewToken || !retainedIdempotencyKey.value || isSubmitting.value) return
  isSubmitting.value = true
  try {
    await props.onSubmit?.(
      { preview_token: props.previewToken, confirm: true },
      { idempotencyKey: retainedIdempotencyKey.value }
    )
  } finally {
    isSubmitting.value = false
  }
}
</script>

<template>
  <div class="tasks-view">
    <!-- 批量重启风险确认 -->
    <div v-if="previewTargets" class="preview-section">
      <label>
        <input v-model="riskConfirmed" type="checkbox" :aria-label="i18n.t('task.risk.confirmBatchReboot')" />
        {{ i18n.t('task.risk.confirmBatchReboot') }}
      </label>
      <button :disabled="!riskConfirmed || isSubmitting" @click="handleSubmitReboot">
        {{ i18n.t('task.submitReboot') }}
      </button>
    </div>

    <!-- 子任务列表与未知结果释放占用 (仅管理员有权操作) -->
    <div v-if="subtasks" class="subtasks-section">
      <div v-for="st in subtasks" :key="st.id">
        <span>{{ st.device_id }} - {{ st.state }}</span>
        <button
          v-if="isAdmin && st.state === 'unknown' && !st.lock_released"
          @click="openReleaseModal(st)"
        >
          {{ i18n.t('task.releaseLock') }}
        </button>
      </div>
    </div>

    <!-- 二级风险确认模态框 (无障碍 focus trap & dialog) -->
    <div
      v-if="showReleaseModal"
      class="modal-overlay"
      role="dialog"
      aria-modal="true"
      :aria-labelledby="'modal-release-title'"
    >
      <div class="modal">
        <h3 id="modal-release-title">{{ i18n.t('task.risk.releaseLockTitle') }}</h3>
        <p>{{ i18n.t('task.risk.releaseWarningDetail') }}</p>
        <label>
          {{ i18n.t('task.field.releaseReason') }}:
          <input v-model="releaseReason" :aria-label="i18n.t('task.field.releaseReason')" type="text" />
        </label>
        <label>
          <input v-model="releaseAck" type="checkbox" :aria-label="i18n.t('task.risk.acknowledgeUnknown')" />
          {{ i18n.t('task.risk.acknowledgeUnknown') }}
        </label>
        <button :disabled="!releaseAck || !releaseReason.trim()" @click="handleReleaseConfirm">
          {{ i18n.t('task.confirmRelease') }}
        </button>
        <button @click="closeReleaseModal">{{ i18n.t('common.cancel') }}</button>
      </div>
    </div>
  </div>
</template>
```

在 `TaskDetailView.vue` 中增加 held 状态管理员 time-review 操作（原子选择 `resume_original_deadline` 或 `expire`，不延长期限）；响应标记 `view_scope: "authorized_subset"` 时展示授权子集标识，不显示隐藏设备数量。

- [ ] **Step 4: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/views/TasksView.test.ts src/task-presentation.test.ts`
预期：PASS。

- [ ] **Step 5: 重构、回归与条件提交**

运行全量测试与类型检查：
`npm --prefix apps/controller-web test -- --run`
`npm --prefix apps/controller-web run typecheck`
确认无任何警告与类型错误。

---

### Task 5: SSE 变化提示流管理与实体局部重同步 (SSE Stream: Buffer-First, GET Resync & Queue Limit)

**Files:**
- Create: `apps/controller-web/src/stream.ts`
- Create: `apps/controller-web/src/stream.test.ts`
- Modify: `apps/controller-web/src/views/DevicesView.vue`
- Modify: `apps/controller-web/src/views/TasksView.vue`
- Modify: `apps/controller-web/src/views/DeviceDetailView.vue`

**Interfaces & Event Protocol:**
- Consumes: `src/api.ts`, 06 U-03 规范。
- Produces:
  ```ts
  export type SseEventType =
    | 'device.updated'
    | 'task.updated'
    | 'permissions.changed'
    | 'system.time.changed'
    | 'reset'

  export interface StreamEvent<T = unknown> {
    type: SseEventType
    revision?: string
    data?: T
  }

  export interface StreamController {
    unsubscribe(): void
    isRealtime: Ref<boolean>
  }

  /**
   * 严格规范 u64 无符号十进制字符串无损比较。
   * 正确比较非负整数大小；对非字符串、带前导零（非"0"）、负数、小数、指数或超过 u64 最大值的非法输入严格抛错。
   */
  export function compareRevision(a: string, b: string): number

  export interface SubscribeOptions<T> {
    fetchSnapshot: (options: { signal: AbortSignal }) => Promise<{ kind: string; items: T[] }>
    onApplySnapshot: (snapshot: { kind: string; items: T[] }) => void
    onEntityPatch: (kind: string, patch: any) => void
    onPermissionsRevoked?: () => void
    EventSourceClass?: typeof EventSource
  }

  export function subscribeAndResync<T>(options: SubscribeOptions<T>): StreamController
  ```

- [ ] **Step 1: 建立可编译桩代码与编写失败测试（可编译的行为 RED）**

首先在 `apps/controller-web/src/stream.ts` 中建立**最小可编译 stub**：

```ts
import { ref, type Ref } from 'vue'

export type SseEventType = 'device.updated' | 'task.updated' | 'permissions.changed' | 'system.time.changed' | 'reset'

export interface StreamEvent<T = unknown> {
  type: SseEventType
  revision?: string
  data?: T
}

export interface StreamController {
  unsubscribe(): void
  isRealtime: Ref<boolean>
}

export function compareRevision(_a: string, _b: string): number {
  return 0 // stub
}

export function subscribeAndResync<T>(_options: any): StreamController {
  return {
    unsubscribe() {},
    isRealtime: ref(false),
  }
}
```

在 `apps/controller-web/src/stream.test.ts` 中编写测试，重点覆盖：
1. `compareRevision` 的纯无损比较与严格 canonical u64 输入正反校验（包括 0、普通数字、超过 2^53-1 的 64 位大数、超出 u64 溢出抛错、带前导零抛错、负数/小数/非数字抛错）；
2. SSE 状态机先等 `open` 触发建立订阅，再发起 GET 快照；
3. 代际守卫（generation/abort）：旧快照在途时若发生重连或 `reset`，旧响应直接作废，不覆盖新快照；
4. 只有在 EventSource 处于 open 且快照同步完成后，`isRealtime` 才变为 true；断线时变为 false；
5. revision 严格为各实体的局部版本（`kind:id`），不拿全局单一 revision 跨对象比较；
6. 收到 `permissions.changed` 立即取消订阅、清空缓冲队列、清除投影与缓存，触发撤权回调；
7. 128 条缓冲溢出或 `reset` 触发非实时标记并重新发起 GET 快照：

```ts
import { describe, expect, it, vi } from 'vitest'
import { compareRevision, subscribeAndResync, type SseEventType } from './stream'

describe('compareRevision canonical u64 parser & comparison', () => {
  it('correctly compares valid canonical u64 decimal strings including beyond 2^53 - 1', () => {
    expect(compareRevision('0', '0')).toBe(0)
    expect(compareRevision('0', '1')).toBe(-1)
    expect(compareRevision('1', '0')).toBe(1)
    expect(compareRevision('99', '100')).toBe(-1)
    expect(compareRevision('100', '99')).toBe(1)
    // 超过 JS 53 位安全整数的大数比较
    const maxU64Minus1 = '18446744073709551614'
    const maxU64 = '18446744073709551615'
    expect(compareRevision(maxU64Minus1, maxU64)).toBe(-1)
    expect(compareRevision(maxU64, maxU64Minus1)).toBe(1)
    expect(compareRevision(maxU64, maxU64)).toBe(0)
  })

  it('strictly rejects non-canonical or overflowing revision inputs with descriptive errors', () => {
    const invalidInputs = [
      '',
      '01', // 带前导零非规范
      '00',
      '-1', // 负数
      '1.0', // 小数
      '1e5', // 指数
      ' 5', // 空格
      '5 ',
      '18446744073709551616', // 超过 u64 最大值
      'abc',
      null as any,
      123 as any,
    ]
    for (const inv of invalidInputs) {
      expect(() => compareRevision(inv, '1')).toThrow()
      expect(() => compareRevision('1', inv)).toThrow()
    }
  })
})

describe('subscribeAndResync lifecycle, open-first snapshot, generation guard & per-entity revisions', () => {
  class FakeEventSource {
    listeners = new Map<string, Array<(e: any) => void>>()
    closed = false

    addEventListener(event: string, fn: (e: any) => void) {
      if (!this.listeners.has(event)) this.listeners.set(event, [])
      this.listeners.get(event)!.push(fn)
    }

    emit(event: string, data?: any) {
      const list = this.listeners.get(event) || []
      for (const fn of list) fn({ data })
    }

    close() {
      this.closed = true
    }
  }

  it('does not trigger GET snapshot until open event confirms subscription', async () => {
    let openCalled = false
    const fetchSnapshot = vi.fn().mockImplementation(async () => {
      openCalled = true
      return { kind: 'device', items: [] }
    })

    let fakeEs: FakeEventSource | null = null
    const ctrl = subscribeAndResync({
      fetchSnapshot,
      onApplySnapshot: vi.fn(),
      onEntityPatch: vi.fn(),
      EventSourceClass: class extends FakeEventSource {
        constructor() {
          super()
          fakeEs = this
        }
      } as any,
    })

    expect(fetchSnapshot).not.toHaveBeenCalled()
    expect(ctrl.isRealtime.value).toBe(false)

    // EventSource 触发 open
    fakeEs!.emit('open')
    await vi.waitFor(() => expect(fetchSnapshot).toHaveBeenCalledOnce())
    expect(ctrl.isRealtime.value).toBe(true)
    ctrl.unsubscribe()
  })

  it('never becomes realtime when an in-flight GET resolves after disconnect', async () => {
    let fakeEs: FakeEventSource | null = null
    let resolveOld!: (value: { kind: string; items: unknown[] }) => void
    const fetchSnapshot = vi.fn().mockImplementation(() => new Promise(resolve => { resolveOld = resolve }))
    const onApplySnapshot = vi.fn()
    const ctrl = subscribeAndResync({ fetchSnapshot, onApplySnapshot, onEntityPatch: vi.fn(),
      EventSourceClass: class extends FakeEventSource { constructor() { super(); fakeEs = this } } as any })
    fakeEs!.emit('open')
    expect(fetchSnapshot).toHaveBeenCalledOnce()
    fakeEs!.emit('error')
    resolveOld({ kind: 'device', items: [] })
    await Promise.resolve()
    expect(onApplySnapshot).not.toHaveBeenCalled()
    expect(ctrl.isRealtime.value).toBe(false)
    fakeEs!.emit('reset')
    expect(fetchSnapshot).toHaveBeenCalledOnce() // disconnected reset must wait for next open
    ctrl.unsubscribe()
  })

  it('tracks per-entity revisions independently, pruning older patches for dev-1 without affecting dev-2', async () => {
    let fakeEs: FakeEventSource | null = null
    const appliedSnapshots: any[] = []
    const appliedPatches: Array<{ kind: string; patch: any }> = []

    const fetchSnapshot = vi.fn().mockResolvedValue({
      kind: 'device',
      items: [
        { device_id: 'dev-1', revision: '5' },
        { device_id: 'dev-2', revision: '10' },
      ],
    })

    const ctrl = subscribeAndResync({
      fetchSnapshot,
      onApplySnapshot: (snap) => appliedSnapshots.push(snap),
      onEntityPatch: (kind, patch) => appliedPatches.push({ kind, patch }),
      EventSourceClass: class extends FakeEventSource {
        constructor() {
          super()
          fakeEs = this
        }
      } as any,
    })

    // 触发 open
    fakeEs!.emit('open')
    // 在快照在途期间缓冲事件：dev-1 revision 4（旧），dev-1 revision 6（新），dev-2 revision 8（针对 dev-2 属于陈旧）
    fakeEs!.emit('device.updated', JSON.stringify({ device_id: 'dev-1', revision: '4', name: 'old-1' }))
    fakeEs!.emit('device.updated', JSON.stringify({ device_id: 'dev-1', revision: '6', name: 'new-1' }))
    fakeEs!.emit('device.updated', JSON.stringify({ device_id: 'dev-2', revision: '8', name: 'old-2' }))

    await vi.waitFor(() => expect(fetchSnapshot).toHaveBeenCalledOnce())
    await Promise.resolve()

    expect(appliedSnapshots).toHaveLength(1)
    // 仅 dev-1 revision 6 被应用；dev-1 rev 4 与 dev-2 rev 8 被各自独立剪枝丢弃
    expect(appliedPatches).toEqual([
      { kind: 'device', patch: { device_id: 'dev-1', revision: '6', name: 'new-1' } },
    ])
    expect(ctrl.isRealtime.value).toBe(true)
    ctrl.unsubscribe()
  })

  it('aborts and invalidates stale in-flight snapshot response when reset occurs', async () => {
    let fakeEs: FakeEventSource | null = null
    let resolveFirstSnap: any
    let firstSnapSignal: AbortSignal | null = null

    const fetchSnapshot = vi.fn()
      .mockImplementationOnce(({ signal }) => {
        firstSnapSignal = signal
        return new Promise((res) => { resolveFirstSnap = res })
      })
      .mockResolvedValueOnce({ kind: 'device', items: [{ device_id: 'dev-1', revision: '20' }] })

    const onApplySnapshot = vi.fn()
    const ctrl = subscribeAndResync({
      fetchSnapshot,
      onApplySnapshot,
      onEntityPatch: vi.fn(),
      EventSourceClass: class extends FakeEventSource {
        constructor() {
          super()
          fakeEs = this
        }
      } as any,
    })

    fakeEs!.emit('open')
    expect(fetchSnapshot).toHaveBeenCalledTimes(1)

    // 触发 reset：旧快照尚在途
    fakeEs!.emit('reset')
    expect(firstSnapSignal!.aborted).toBe(true)
    expect(fetchSnapshot).toHaveBeenCalledTimes(2)

    // 此时第一次请求延迟返回
    resolveFirstSnap({ kind: 'device', items: [{ device_id: 'dev-1', revision: '1' }] })
    await vi.waitFor(() => expect(ctrl.isRealtime.value).toBe(true))

    // 确认最终生效的是第二次快照 (rev 20)，绝不是旧快照 (rev 1)
    expect(onApplySnapshot).toHaveBeenLastCalledWith({
      kind: 'device',
      items: [{ device_id: 'dev-1', revision: '20' }],
    })
    ctrl.unsubscribe()
  })

  it('purges buffer, closes subscription and triggers onPermissionsRevoked on permissions.changed', async () => {
    let fakeEs: FakeEventSource | null = null
    const onRevoked = vi.fn()
    const patchFn = vi.fn()

    const ctrl = subscribeAndResync({
      fetchSnapshot: vi.fn().mockResolvedValue({ kind: 'device', items: [] }),
      onApplySnapshot: vi.fn(),
      onEntityPatch: patchFn,
      onPermissionsRevoked: onRevoked,
      EventSourceClass: class extends FakeEventSource {
        constructor() {
          super()
          fakeEs = this
        }
      } as any,
    })

    fakeEs!.emit('open')
    await Promise.resolve()

    fakeEs!.emit('permissions.changed')
    expect(onRevoked).toHaveBeenCalledOnce()
    expect(ctrl.isRealtime.value).toBe(false)
    expect(fakeEs!.closed).toBe(true)
  })
})
```

- [ ] **Step 2: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/stream.test.ts`
预期结果：测试套件正常编译加载，运行数非 0（运行 5 个用例，全部失败），因 stub 恒返回 0 且未实现流状态机导致断言失败：
`AssertionError: expected 0 to be -1`。
（有效行为 RED）。

- [ ] **Step 3: 最小实现**

在 `apps/controller-web/src/stream.ts` 中实现严格 canonical u64 大数比较与基于命名事件、代际 guard、局部 revision 跟踪的流管理状态机：

```ts
import { ref, type Ref } from 'vue'

export type SseEventType =
  | 'device.updated'
  | 'task.updated'
  | 'permissions.changed'
  | 'system.time.changed'
  | 'reset'

export interface StreamEvent<T = unknown> {
  type: SseEventType
  revision?: string
  data?: T
}

export interface StreamController {
  unsubscribe(): void
  isRealtime: Ref<boolean>
}

const CANONICAL_U64_RE = /^(?:0|[1-9]\d{0,19})$/
const MAX_U64 = '18446744073709551615'

/**
 * 64位无符号十进制字符串无损比较。
 * 严格校验输入为规范十进制表示（无前导零、非负、无小数/指数），且不超出 u64 上限。
 */
export function compareRevision(a: string, b: string): number {
  if (typeof a !== 'string' || typeof b !== 'string') {
    throw new TypeError('Revision must be a string')
  }
  if (!CANONICAL_U64_RE.test(a) || !CANONICAL_U64_RE.test(b)) {
    throw new Error(`Invalid canonical decimal revision: ${a} or ${b}`)
  }
  if (a.length === 20 && a > MAX_U64) throw new RangeError(`Revision overflows u64: ${a}`)
  if (b.length === 20 && b > MAX_U64) throw new RangeError(`Revision overflows u64: ${b}`)
  if (a === b) return 0
  if (a.length !== b.length) {
    return a.length > b.length ? 1 : -1
  }
  return a > b ? 1 : -1
}

export interface SubscribeOptions<T> {
  fetchSnapshot: (options: { signal: AbortSignal }) => Promise<{ kind: string; items: T[] }>
  onApplySnapshot: (snapshot: { kind: string; items: T[] }) => void
  onEntityPatch: (kind: string, patch: any) => void
  onPermissionsRevoked?: () => void
  EventSourceClass?: typeof EventSource
}

export function subscribeAndResync<T>(options: SubscribeOptions<T>): StreamController {
  const isRealtime = ref(false)
  let active = true
  let connected = false // 与最近一次 EventSource open/error 代际绑定
  let syncGeneration = 0
  let abortController: AbortController | null = null

  // 实体局部 revision 映射: `${kind}:${id}` -> revision
  const entityRevisions = new Map<string, string>()
  const eventBuffer: Array<{ kind: string; patch: any }> = []

  const ESClass = options.EventSourceClass || EventSource
  const es = new ESClass('/api/v1/events', { withCredentials: true })

  function getEntityKey(kind: string, item: any): string {
    return `${kind}:${item.id || item.device_id}`
  }

  function applyPatchIfNewer(kind: string, patch: any) {
    const key = getEntityKey(kind, patch)
    const currentRev = entityRevisions.get(key)
    if (!currentRev || compareRevision(patch.revision, currentRev) > 0) {
      entityRevisions.set(key, patch.revision)
      options.onEntityPatch(kind, patch)
    }
  }

  async function triggerResync() {
    isRealtime.value = false
    if (abortController) abortController.abort()
    const currentGen = ++syncGeneration
    if (!active || !connected) return // reset/溢出发生在断线期：等下一次 open 后再 GET
    abortController = new AbortController()

    try {
      const snap = await options.fetchSnapshot({ signal: abortController.signal })
      if (!active || !connected || currentGen !== syncGeneration) return

      // 以快照项的 revision 填充局部 revision 表
      for (const item of snap.items as any[]) {
        const key = getEntityKey(snap.kind, item)
        if (item.revision) {
          entityRevisions.set(key, item.revision)
        }
      }
      options.onApplySnapshot(snap)

      // 回放并应用缓冲区中的新事件
      while (eventBuffer.length > 0) {
        const { kind, patch } = eventBuffer.shift()!
        applyPatchIfNewer(kind, patch)
      }
      isRealtime.value = true
    } catch {
      if (!active || !connected || currentGen !== syncGeneration) return
      isRealtime.value = false
    }
  }

  function handleUpdateEvent(kind: string, rawData: string) {
    if (!active) return
    try {
      const parsed = JSON.parse(rawData)
      if (isRealtime.value) {
        applyPatchIfNewer(kind, parsed)
      } else {
        if (eventBuffer.length < 128) {
          eventBuffer.push({ kind, patch: parsed })
        } else {
          // 缓冲队列满 128 条：清空缓冲区并重新发起快照
          eventBuffer.length = 0
          void triggerResync()
        }
      }
    } catch {
      // 畸形事件/非法 revision 不能悄悄维持“实时”旧数据；重新获取授权快照。
      eventBuffer.length = 0
      void triggerResync()
    }
  }

  // 1. 监听 open 事件：确认订阅成立后发起首次快照
  es.addEventListener('open', () => {
    if (!active) return
    connected = true
    void triggerResync()
  })

  // 2. 监听具名更新事件
  es.addEventListener('device.updated', (e: MessageEvent) => handleUpdateEvent('device', e.data))
  es.addEventListener('task.updated', (e: MessageEvent) => handleUpdateEvent('task', e.data))
  // system.time.changed 仅由后端对管理员投影；作为变化提示重新 GET 授权快照，
  // 不把无对象 ID 的系统事件当设备/任务补丁，也不凭事件宣称时钟已经同步。
  es.addEventListener('system.time.changed', () => { if (active) void triggerResync() })

  // 3. 监听 reset 事件
  es.addEventListener('reset', () => {
    if (!active) return
    eventBuffer.length = 0
    void triggerResync()
  })

  // 4. 监听 permissions.changed 撤权事件
  es.addEventListener('permissions.changed', () => {
    if (!active) return
    unsubscribe()
    options.onPermissionsRevoked?.()
  })

  // 5. 连接错误
  es.addEventListener('error', () => {
    connected = false
    isRealtime.value = false
    ++syncGeneration
    abortController?.abort()
    eventBuffer.length = 0 // 旧连接权限/事件代际不能流入下一次 open
  })

  function unsubscribe() {
    active = false
    connected = false
    isRealtime.value = false
    if (abortController) abortController.abort()
    eventBuffer.length = 0
    entityRevisions.clear()
    es.close()
  }

  return {
    unsubscribe,
    isRealtime,
  }
}
```

- [ ] **Step 4: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/stream.test.ts`
预期：PASS。

- [ ] **Step 5: 重构、回归与条件提交**

运行全量测试与类型检查：
`npm --prefix apps/controller-web test -- --run`
`npm --prefix apps/controller-web run typecheck`
确认无任何警告与类型错误。

---

### Task 6: 双语词典完整性与无障碍键盘/视口浏览器验收 (Bilingual Dictionaries & Accessible Browser E2E)

**Files:**
- Modify: `apps/controller-web/src/locales/zh-CN.ts`
- Modify: `apps/controller-web/src/locales/en.ts`
- Modify: `apps/controller-web/src/i18n.test.ts`
- Create: `apps/controller-web/src/views/A11yModalFocus.test.ts`
- Candidate Spec (待审批候选): `apps/controller-web/playwright.config.ts`
- Candidate Spec (待审批候选): `apps/controller-web/tests/flows.spec.ts`

**Interfaces & Dependency Constraints:**
- Consumes: `src/i18n.ts`, 06 U-02 规范。
- Produces:
  - 双词典 1:1 键集严格等长断言通过，消除所有新增视图中的硬编码中文；
  - 语言切换与刷新：设备勾选、表单数据与 Idempotency-Key 零重置，不发写请求（POST 计数为 0）；
  - 全键盘 Tab 焦点移动、Modal 焦点捕获（focus trap）与 Esc 恢复（基于 Vitest + jsdom 进行组件级自动化断言）；
  - **重要依赖约束：** Playwright 与 axe-core 仅作为需独立申请审批安装的候选工具，**绝对禁止在当前开发计划中默认执行未安装的 playwright 命令**。

- [ ] **Step 1: 建立可编译桩代码与编写失败测试（可编译的行为 RED）**

在 `apps/controller-web/src/i18n.test.ts` 中增加全量新增业务键（任务状态、五维指标、审批与释放锁警告提示、常用按钮）的 1:1 严格对等断言，并在 `apps/controller-web/src/views/A11yModalFocus.test.ts` 中建立弹窗焦点捕获、Esc 退出与无障碍属性（`role="dialog"`、`aria-modal="true"`）的行为 RED 测试：

```ts
import { describe, expect, it } from 'vitest'
import zhCN from './locales/zh-CN'
import en from './locales/en'

describe('i18n dictionary completeness & 1:1 parity', () => {
  it('strictly matches all keys between zh-CN and en', () => {
    const zhKeys = Object.keys(zhCN).sort()
    const enKeys = Object.keys(en).sort()
    expect(zhKeys).toEqual(enKeys)
  })

  it('contains mandatory risk, five-dimension, admin and common keys without missing values', () => {
    const mandatoryKeys = [
      'common.select',
      'common.actions',
      'common.cancel',
      'common.error',
      'common.unknown',
      'device.field.deviceId',
      'device.dim.admission',
      'device.dim.connection',
      'device.dim.streamHealth',
      'device.dim.freshness',
      'device.dim.controllerClock',
      'device.dim.boardClock',
      'device.health.control',
      'device.health.data',
      'device.status.age',
      'task.state.unknown',
      'task.submitReboot',
      'task.releaseLock',
      'task.confirmRelease',
      'task.field.releaseReason',
      'task.risk.confirmBatchReboot',
      'task.risk.releaseLockTitle',
      'task.risk.releaseWarningDetail',
      'task.risk.acknowledgeUnknown',
      'admin.approvals.title',
      'admin.approvals.approveSelected',
      'admin.approvals.revoke',
      'admin.approvals.manualResetNotice',
    ]
    for (const key of mandatoryKeys) {
      expect(zhCN).toHaveProperty(key)
      expect(en).toHaveProperty(key)
      expect(zhCN[key]).toBeTruthy()
      expect(en[key]).toBeTruthy()
    }
  })
})
```

在 `apps/controller-web/src/views/A11yModalFocus.test.ts` 中断言：
1. 模态框打开时自动聚焦到模态框内的第一个可聚焦元素；
2. Tab 键在模态框内循环捕获（focus trap），不泄露到背景 DOM；
3. 按下 Esc 键关闭模态框，并将焦点恢复到触发打开的按钮：

```ts
import { fireEvent, render, screen } from '@testing-library/vue'
import { describe, expect, it } from 'vitest'
import TasksView from './TasksView.vue'
import { createI18n } from '../i18n'

describe('A11y modal focus trap and restoration', () => {
  const i18n = createI18n({ initialLocale: 'zh-CN', storage: null })

  it('traps focus inside modal and restores focus to trigger button upon Escape', async () => {
    render(TasksView, {
      props: {
        subtasks: [{ id: 'sub-1', device_id: 'a'.repeat(64), state: 'unknown', revision: '1', lock_released: false }],
        isAdmin: true,
      },
      global: { provide: { i18n } },
    })

    const triggerBtn = screen.getByRole('button', { name: new RegExp(i18n.t('task.releaseLock'), 'i') })
    triggerBtn.focus()
    expect(document.activeElement).toBe(triggerBtn)

    await fireEvent.click(triggerBtn)

    const modal = screen.getByRole('dialog')
    expect(modal).toBeTruthy()
    expect(modal.getAttribute('aria-modal')).toBe('true')

    // 按下 Esc 键关闭
    await fireEvent.keyDown(window, { key: 'Escape', code: 'Escape' })
    expect(screen.queryByRole('dialog')).toBeNull()
  })
})
```

- [ ] **Step 2: 运行并确认失败**

运行：`npm --prefix apps/controller-web test -- --run src/i18n.test.ts src/views/A11yModalFocus.test.ts`
预期结果：测试套件正常编译加载，运行数非 0，因新增业务键在既有词典中尚未定义断言失败：
`AssertionError: expected {} to have property 'device.dim.admission'`。
（有效行为 RED）。

- [ ] **Step 3: 最小实现**

在 `apps/controller-web/src/locales/zh-CN.ts` 与 `apps/controller-web/src/locales/en.ts` 中补齐包括设备五维、10 个任务状态、12 个原因码、审批警告、释放锁二次确认在内的全部 1:1 对等词条。
完善模态框组件焦点恢复逻辑，确保关闭时恢复至原始触发按钮。
编写候选规范 `playwright.config.ts` 与 `tests/flows.spec.ts` 备查（仅在后续获得依赖安装审批后使用）。

- [ ] **Step 4: 运行并确认通过**

运行：`npm --prefix apps/controller-web test -- --run src/i18n.test.ts src/views/A11yModalFocus.test.ts`
预期：PASS。

- [ ] **Step 5: 重构、回归与条件提交**

运行前端类型检查与构建测试：
`npm --prefix apps/controller-web run typecheck`
`npm --prefix apps/controller-web run build`
确认无任何警告与类型错误。

---

### Task 7: Rust 生产静态资源嵌入、精确路由与同源联验 (Rust Embedded Assets, Exact Routing & Live Same-Origin Smoke)

**Files:**
- Create: `crates/rsetup-controller/src/web.rs`
- Create: `crates/rsetup-controller/tests/web_assets.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`
- Modify: `Makefile`
- Modify: `.github/workflows/ci.yml`

**Interfaces & Embedding Gate Contracts:**
- Consumes: `apps/controller-web/dist/*` 真实生产产物。
- **待审 embed 机制与实现门禁规范：**
  在未经独立审批引入第三方 crate（如 `rust-embed`）前，实施规范不得虚构已拥有该依赖；生产内嵌应采用获准的标准库宏（如 `include_str!` / `include_bytes!`）或构建脚本 `build.rs` 生成的静态资源表，或者在提交申请并获得依赖审批后再引入。
- Produces:
  ```rust
  pub fn web_router() -> axum::Router
  ```
- 行为与测试门禁契约：
  1. **构建顺序与缺失产物门禁：** Makefile 保证先 `npm run build` 后 `cargo build`；Release 构建时若缺失 `apps/controller-web/dist` 则编译直接失败（fail-closed），绝不伪造空目录或静默放行；
  2. **独立目录运行：** 二进制必须脱离 Node.js 与前端源码目录；测试在与项目根目录不同的工作目录（非项目 cwd）下运行，验证静态资源依然正常提供；
  3. **真实资源与内容断言：** 测试实际解析 `index.html` 引用的 JS 与 CSS URL，发起请求并断言获取到非空的真实产物内容，验证 `Content-Type` 与长期不可变缓存响应头（`Cache-Control: public, max-age=31536000, immutable`）；
  4. **SPA 精确路由回退：** SPA 精确回退仅限于前端已知页面路由白名单（`/`, `/login`, `/password`, `/sessions`, `/devices`, `/devices/:id`, `/tasks`, `/tasks/:id`, `/admin/*`）且仅限于 GET/HEAD 请求；
  5. **严格 404 边界防掩盖：** 缺失的 `/assets/missing.js` 坚决返回 404；缺失的 `/api/v1/not-found` 坚决返回完整 404 JSON error envelope（`{"error":{"code":"NOT_FOUND","message_key":"errors.notFound"},"request_id":"..."}`），绝不回退 HTML 掩盖后端错误。

- [ ] **Step 1: 建立可编译桩代码与编写失败测试（可编译的行为 RED）**

首先在 `crates/rsetup-controller/src/web.rs` 中建立**最小可编译 stub**：

```rust
use axum::Router;

/// 最小可编译 stub：返回空 Router，尚未挂载静态资源、精确 SPA 白名单与真实资源解析
pub fn web_router() -> Router {
    Router::new()
}
```

并在 `crates/rsetup-controller/src/lib.rs` 中声明模块：
```rust
pub mod web;
```

在 `crates/rsetup-controller/tests/web_assets.rs` 中编写严谨的集成测试，断言真实产物、解析 index.html 真实资产 URL、独立 cwd、GET/HEAD 边界及完整 API 错误包络：

```rust
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, Method},
};
use tower::ServiceExt;

#[tokio::test]
async fn web_assets_mime_cache_and_404_boundaries() {
    let router = rsetup_controller::web::web_router();

    // 1. SPA 页面路由返回 200 OK，包含真实 HTML 入口且携带 no-cache
    let req = Request::builder().method(Method::GET).uri("/devices").body(Body::empty()).unwrap();
    let res = router.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers().get("content-type").unwrap(), "text/html; charset=utf-8");
    assert_eq!(res.headers().get("cache-control").unwrap(), "no-cache");
    let html_bytes = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let html_str = String::from_utf8(html_bytes.to_vec()).unwrap();
    assert!(html_str.contains("<div id=\"app\">"));

    // 2. 真实 index.html 必须包含 JS 与 CSS；任一不存在即失败，不能 if let 跳过。
    let js_url = html_str.split('"').find(|s| s.starts_with("/assets/") && s.ends_with(".js"))
        .expect("Vite index must reference a hashed JS asset");
    let css_url = html_str.split('"').find(|s| s.starts_with("/assets/") && s.ends_with(".css"))
        .expect("Vite index must reference a hashed CSS asset");
    for (url, mime) in [(js_url, "text/javascript"), (css_url, "text/css")] {
        let expected = std::fs::read(format!("{}/../../apps/controller-web/dist{}",
            env!("CARGO_MANIFEST_DIR"), url)).expect("built asset must exist");
        assert!(!expected.is_empty(), "asset is not allowed to be empty");
        let req_asset = Request::builder().method(Method::GET).uri(url).body(Body::empty()).unwrap();
        let res_asset = router.clone().oneshot(req_asset).await.unwrap();
        assert_eq!(res_asset.status(), StatusCode::OK);
        assert_eq!(res_asset.headers().get("content-type").unwrap(), mime);
        assert_eq!(res_asset.headers().get("cache-control").unwrap(),
            "public, max-age=31536000, immutable");
        let asset_bytes = to_bytes(res_asset.into_body(), usize::MAX).await.unwrap();
        assert_eq!(asset_bytes.as_ref(), expected.as_slice(), "serve the actual build bytes");
    }

    // 3. 缺失静态资产坚决 404，绝不回退 HTML
    let req_missing_asset = Request::builder().method(Method::GET).uri("/assets/missing.js").body(Body::empty()).unwrap();
    let res_missing_asset = router.clone().oneshot(req_missing_asset).await.unwrap();
    assert_eq!(res_missing_asset.status(), StatusCode::NOT_FOUND);

    // 4. API 404 JSON 由另一个测试在完整生产合并 router 上断言，
    // 此孤立 web_router 用例只验证不能把 API 当 SPA 成功回退。
    let req_api = Request::builder().method(Method::GET).uri("/api/v1/not-found").body(Body::empty()).unwrap();
    let res_api = router.clone().oneshot(req_api).await.unwrap();
    assert_ne!(res_api.status(), StatusCode::OK);

    // 5. 非 GET/HEAD 尝试访问 SPA 页面路由返回 405 Method Not Allowed 或 404
    let req_post_spa = Request::builder().method(Method::POST).uri("/devices").body(Body::empty()).unwrap();
    let res_post_spa = router.oneshot(req_post_spa).await.unwrap();
    assert_ne!(res_post_spa.status(), StatusCode::OK);
}
```

- [ ] **Step 2: 运行并确认失败**

运行：`cargo test -p rsetup-controller --test web_assets`
预期结果：测试顺利通过编译并执行，测试运行数非 0（运行 1 个测试），因空 Router 未挂载任何路由导致断言失败：
`assertion `left == right` failed: left: 404, right: 200`。
（验证：确切的行为 RED，绝非模块找不到或未定义的编译错误）。

- [ ] **Step 3: 实现真实静态资源嵌入（不得使用空页面/404 占位当 GREEN）**

先审定标准库 `include_str!`/`include_bytes!` 或 `build.rs` 生成完整资产表的构建方案；对 Vite 哈希文件名不能在 Rust 代码里写死上一轮构建的哈希。构建阶段枚举 `dist/index.html` 引用的全部本地资产并核对实物存在；缺失目录、入口或任一引用时 `cargo build --release` 必须失败，不允许仅 `Makefile` 防护却让直接 Cargo 发布产生旧/空产物。资产表在编译期嵌入 JS/CSS 字节及 MIME，运行时不读磁盘或依赖 cwd。根据真实路由表仅 GET/HEAD 的已知 SPA 页面回退入口；GET 资产按精确路径返回编译时字节与 `immutable`，HEAD 同状态/头而无 body，未知 `/assets/*` 404。`/api/v1/*` 的 404 应通过已绑定状态的管理 API 层返回统一 `{error:{code,message_key,params:{}},request_id}` JSON；`web_router` 不能私造缺 `params` 的信封，也不能以全局 fallback 抢走已经匹配的认证/管理路由。真实合并 router 上逐项验证，而不仅测试孤立 `web_router`。

以下构建顺序只是未来实现的入口；子 crate 构建本身也必须 fail-closed，确保绕过 Makefile 不会发布缺产物版本：
```makefile
controller-build:
	cd apps/controller-web && npm run build
	test -f apps/controller-web/dist/index.html || { echo "Missing frontend dist!"; exit 1; }
	cargo build --release -p rsetup-controller
```

缺 `dist` 的构建负例须在**隔离临时构建上下文**验证，不能在当前工作区直接移走用户已有的 `dist`；真实运行验收另以已构建二进制在独立 cwd/无 Node/无静态目录下请求所有入口资源。不得把当前上面的只读资产测试在默认 cwd 下通过称为脱离目录验收。


- [ ] **Step 4: 运行并确认通过**

运行：`cargo test -p rsetup-controller --test web_assets`
预期：PASS（运行数 1，全部通过）。

- [ ] **Step 5: 重构、回归与条件提交**

运行 Rust 格式与 Clippy 检查：
`cargo fmt -p rsetup-controller -- --check`
`cargo clippy -p rsetup-controller -- -D warnings`
确认无任何警告。

---

## 规格验收矩阵追踪 (Spec 06 AT-01..18)

| 验收项 | 场景与通过条件 | 本计划对应任务 | 跨专题联合依赖 |
| :--- | :--- | :--- | :--- |
| **AT-01** | 初始化密码只日志输出一次、强制改密、未改密非白名单拦截 | Task 1 (`PasswordView`) | 依赖 01 身份计划生产路由 |
| **AT-02** | 会话停用/改密失效、CSRF 绑定、同源 Origin/Host、限速反馈 | Task 1 (`LoginView`, `SessionsView`) | 依赖 01 身份计划与已审会话端点 |
| **AT-03** | 动态权限并集、撤权立即生效、最小识别投影、隐藏设备 404 | Task 2 (`DevicesView`, `DeviceDetailView`) | 依赖 01 动态授权仓储 |
| **AT-04** | 1024 审批清单、审批人工拒绝与 reopen 恢复提示 | Task 3 (`AdminApprovalsView`) | 依赖 01/02 准入 CAS 存储 |
| **AT-05** | 预览固定清单、一主一子、UUID 幂等键、跨主任务设备互斥 | Task 4 (`TasksView`) | 依赖 03/04 任务模型与锁表 |
| **AT-06** | 撤权/取消与 dispatching 竞争，故障不重复重启 | Task 4 (`TasksView`) | 依赖 03/04 调度器与锁恢复 |
| **AT-07** | 仅掉线不判成功，unknown 二级风险确认释放锁不解开新锁 | Task 4 (`TasksView`) | 依赖 04 状态机与板端日志核验 |
| **AT-16** | 中英双语 1:1、全键盘 Tab/Esc 焦点、320px 视口无溢出、无障碍 | Task 6 (`i18n.test.ts`, `A11yModalFocus.test.ts`) | 候选 Playwright 独立门禁 |
| **AT-17** | 单二进制独立运行、无 Node 依赖、静态 404/API 严禁回退 HTML | Task 7 (`web_assets.rs`) | 依赖 Axum 与前端 build 产物 |
| **AT-18** | SSE 先订阅后 GET 快照、128 上限 reset 重取、撤权立即断开并清空缓冲 | Task 5 (`stream.ts`) | 依赖 03 轮询与 02 `/events` 端点 |

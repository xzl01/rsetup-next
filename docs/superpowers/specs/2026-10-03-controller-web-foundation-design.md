# Controller Web 基础公用层设计

## 批准与范围

用户 `controller_frontend_foundation_approval` 已批准完整基础范围及工具：Vue 3/Vite、TypeScript、Vitest、Vue Testing Library、jsdom；允许安装依赖并生成独立锁文件。此为原[条件性 Web 计划](../plans/2026-10-02-controller-04-web-tdd.md)的有限开工批准，不批准其全部业务或发布任务。

只新增独立 `apps/controller-web`。不修改板端 `ui`、DSH GUI、Rust/Cargo、数据库、Makefile/CI。无登录/权限/设备等业务页，无认证会话/CSRF/写请求/SSE，无生产资源嵌入。不得启动开发服务器或部署。当前 controller 只有健康/就绪路由；测试采用合成 fetch 响应，应用挂载不发 API 请求，不假装已有后端业务能力。

## 工程和界面

Node 22，npm 独立 package-lock；采用兼容当前 Node 的固定依赖版本，以 lockfile 保证重现。仅必要 Vue/Vite/TypeScript/Vitest/Vue Testing Library/jsdom/vue-tsc 和 Vite Vue 插件，不引入大型组件库或 router。测试、typecheck、build 分开；无真实 API/数据库依赖。工程局部忽略 node_modules/dist/coverage，安装缓存留当前 workspace，不改其他包。

应用是基础工作台壳：语义 header/main/footer，跳过导航链接、清晰标题、语言切换和静态基础组件展示，注明尚未连接业务服务。无虚构设备数量、用户权限或服务健康。CSS tokens、字体、间距、色彩及高对比焦点统一，320px 窄屏不产生页面水平滚动；支持 prefers-reduced-motion、prefers-contrast。没有浏览器实测时不得宣称视觉/端到端验收完成。

## 公共组件边界

所有组件只渲染 props/slots，不读取认证或发网络请求，不绑定翻译模块。文案由调用者传入；不使用 v-html。

- `BaseButton.vue`：type 默认 button；variant primary/secondary/danger；disabled/loading；loading 状态阻止 click 并 aria-busy=true，`loadingLabel` 提供纯文本提示；slot 提供标题。
- `BaseInput.vue`：必需 id/label，modelValue string，hint/error/required/disabled 可选，type=text/password 默认 text；emit update:modelValue；label-for、hint/error IDs、aria-describedby/aria-invalid 正确关联。密码不记录、不持久化。
- `AppNotice.vue`：tone info/success/warning/error（可选，默认 info）、必需 `toneLabel: string`（由调用者传入的纯文本可见语义标签，无内置英文默认或组件翻译；调用者负责随语言切换提供相应文案）、title 可选，slot 正文；error 用 alert，其余 status；保留 `data-tone`/tone class，不用颜色作为唯一语义。
- `AsyncState.vue`：state loading/empty/error，title/description，可选 retryLabel；只有 error 且有 retryLabel 才显示重试按钮并 emit retry，不自动重试。

组件样式独占 `src/styles.css`，不依赖 API/i18n。App 在所有模块冻结后串行接线。

## 双语接口

`src/i18n.ts` 导出 `Locale = 'zh-CN' | 'en'`、`createI18n(options?: {initialLocale?: Locale; storage?: Storage | null; document?: Document | null})`；返回 `{locale: Ref<Locale>, t(key: string, params?: Record<string,string|number>): string, setLocale(locale: Locale): void}`。

默认语言 zh-CN，偏好 key 为 `rsetup.controller.locale`，只存语言，不存秘密/身份。若 options 未显式提供 storage/document，可尝试浏览器 localStorage/document；访问异常和存储异常安全回退。有效持久偏好优先于 initialLocale；未知偏好忽略。setLocale 即时同步 ref、documentElement.lang 和偏好；存储失败不阻止切换。未知语言运行时回退 zh-CN；未知翻译 key 原样纯文本返回；缺英文条目回退中文再回退 key。只做 `{name}` 字符串插值，不生成 HTML。

`src/locales/zh-CN.ts`、`en.ts` 具有一致 key 集合，包括 app.title/app.foundation/app.notConnected/nav.skip/language.label、button.primary/button.secondary/button.danger/button.loading、field.label/field.hint/field.error、notice.title/notice.body、state.loading/state.empty/state.error/state.retry。语言切换只影响文本/lang，不丢失表单值、不触发 fetch。

## 只读 API 接口

`src/api.ts` 导出 `get<T>(path: string, options?: {signal?: AbortSignal}): Promise<ApiResponse<T>>`；`src/types.ts` 导出 `ApiResponse<T> = {data:T;requestId:string}`、`DecimalString = string`（只表明 wire 形态，不声称任意 string 已验证）、`ApiError` 类或其明确公共类型。

调用仅使用固定 GET、credentials=same-origin、redirect=error、Accept:application/json，前缀 `/api/v1`。不接受调用者覆写 method/credentials/headers/baseURL，绝不自动重试或跳转登录。不接受绝对 URL、协议相对路径、反斜杠、控制字符、fragment、路径中的解码后点段或分隔符逃逸；允许普通同源路径和 query。验证发生在 fetch 之前，不能只用字符串 startsWith('/')。AbortSignal 原样传入，取消独立标记。

成功包必须为非数组对象，拥有 data 及非空字符串 request_id；返回 data/requestId。不运行 Number/parseInt 转换业务计数或 revision；大整数十进制字符串字节保持。错误包 `{error:{code,message_key,params},request_id}` 保留中立 code/messageKey、requestId/status、可选 Retry-After；只接受明确类型/有界字符串和纯数据 params，不把任意服务端 message/HTML当 UI 文案。错误用 `ApiError` 固定 message；网络、取消、非法路径、非 JSON、畸形 envelope 各稳定 code。401/403/404/409/429不伪装成功；未知错误用固定回退，不 log 原始响应、请求体或秘密。

## 并行与验收

先由工程骨架任务独占 package/config/index/main/App/tests setup/README；随后三路互斥：组件+styles、i18n+locales、api+types，均含各自 colocated tests；最后 App 串行接线。除骨架/集成任务外禁止更改包配置/lock/main/App。

TDD 的 RED 必须来自可编译行为断言失败，缺依赖/导入/编译错误不计。纯函数之外还需真实 Vue DOM 组件测试：键盘可用结构、label与错误关联、loading阻止提交/点击、语言偏好/回退、恶意文案纯文本、只读请求及错误分类。请求在 fetch 边界合成，不 mock 被测函数。最终 test/typecheck/build 全通过；生产后端、浏览器布局、真实认证和端到端仍未验收。

凭据、账号配置、DB诊断与 Task4 observer 只在后端测试专题，绝不暴露到浏览器。本基础层可与 Task4 并行，不改变其授权。

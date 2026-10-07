# Controller Web — 认证与会话前端

独立 Vue 3 / Vite / TypeScript 应用，不接入板端 UI。目前已有双语应用壳、基础组件、同源 GET/POST 客户端、登录/强制改密/登出状态机，以及本人会话列表、指定会话撤销和“撤销其他会话”界面。尚无设备/任务/完整管理员业务页面、SSE 或 Rust 静态资源嵌入；不展示伪造的设备统计和服务健康。

## 当前请求与安全边界

- [App.vue](<src/App.vue>) 挂载会发起 `GET /api/v1/auth/me`；有效 `signed_in` 状态下会加载本人会话。手动重试会重新请求，登录/改密/登出和会话撤销会发送对应的同源 POST；语言切换本身不提交业务操作。
- 浏览器负责 HttpOnly Cookie，前端不读取或写入 cookie；CSRF 与身份状态仅保存在内存，只有语言偏好持久化。网络失败或未知响应不等同于已注销、改密成功或后端就绪。
- 当前[中控生产入口](<../../crates/rsetup-controller/src/main.rs>) 仍只挂载探活路由；已有认证/会话 handler 不等于已接入生产监听。前端离线测试的合成 fetch 成功不能当作真实后端联调结果；直接打开页面时仍需要未来获审的后端接线。
- 管理端 HTTP 直连只适用于受控可信网络。HttpOnly、CSRF、Host/Origin 检查都不提供链路加密；外部 HTTPS 反代可选，板端隧道加密不保护浏览器链路。

## 本地验证

运行环境：Node 22.22.1、npm 9.2.0。进入本应用目录后，所有 npm 操作使用应用内缓存及日志；只有确需重装依赖时才从本地缓存离线安装：

```sh
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" ci --offline --engine-strict --ignore-scripts --no-fund --no-audit
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run typecheck
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run build
```

`package-lock.json` 固定依赖树。保留接管时的 Vue 3.5.43、Vite 8.3.2、Vitest 5.0.3 等直接版本；`@types/node` 固定为 22.12.0 以满足 Vite peer 范围。`overrides` 将 `@vue/test-utils` 固定为 2.4.6（满足 Vue Testing Library 的 `^2.4.1`），避免 2.5.1 引入的 `js-beautify@2` → `nopt@10` 要求 Node 22.22.2 而不兼容当前 22.22.1；这是已有安装错误证据的兼容性固定，不是漏洞修复或升级策略。

`node_modules/`、`dist/`、`coverage/`、`.npm-cache/` 仅为本地忽略产物。测试使用 jsdom 渲染真实 Vue SFC，并在每个测试后自动 cleanup。测试、类型检查与构建是独立步骤。`--no-audit` 并不表示依赖无漏洞。

本任务未启动开发/预览服务；构建或 jsdom 测试通过不代表浏览器布局、键盘/辅助技术、320px 视口或端到端验收通过。生产后端联调与发布仍未完成。

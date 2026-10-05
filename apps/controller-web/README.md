# Controller Web — 前端基础工作台

独立 Vue 3 / Vite / TypeScript 应用：双语语义工作台壳、跳过导航、基础组件展示、仅供未来使用的同源只读 API 模块。页面明确提示尚未连接业务服务；示例的错误和重试只是本地 UI 演示，不表示发生过请求或真实服务状态。挂载、语言切换和示例重试均不发 API 请求。没有业务页面、认证、设备统计或真实服务健康展示；不接入板端 UI。

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

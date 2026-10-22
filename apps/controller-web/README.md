# Controller Web — 中控 Web 前端

独立 Vue 3 / Vite / TypeScript 应用，不接入板端 UI。当前状态：双语应用壳与路由/国际化，登录、强制改密、登出与本人会话管理（列表、单个撤销、撤销其他会话），设备列表与设备详情，任务列表、任务详情与「预览 → 确认 → 提交」流程（`preview_token` + `Idempotency-Key`），以及审批、设备组、用户、角色、授权、审计、系统 7 个管理页面。SSE 客户端与事件集成已实现（五种命名事件、连接/同步代际、revision 水位、`reset` 全量重取、断线期间不标记「实时」）。生产构建产物 `dist/` 由 [crates/rsetup-controller/build.rs](<../../crates/rsetup-controller/build.rs>) 在编译期校验并嵌入中控单一二进制，随生产 router 一起装配。页面不展示伪造的设备统计或服务健康。

## 当前请求与安全边界

- [App.vue](<src/App.vue>) 挂载会发起 `GET /api/v1/auth/me`；有效 `signed_in` 状态下会加载本人会话。手动重试会重新请求，登录/改密/登出和会话撤销会发送对应的同源 POST；语言切换本身不提交业务操作。
- 浏览器负责 HttpOnly Cookie，前端不读取或写入 cookie；CSRF token 与身份/授权状态仅保存在内存，只有语言偏好持久化。网络失败或未知响应不等同于已注销、改密成功或后端就绪。
- 所有业务请求走相对路径的同源 `/api/v1`（`credentials: 'same-origin'`），前端不发送任何跨域请求，也不依赖 CORS 通配或宽松 header；本地开发代理的规则与限制见 [DEV-PROXY.md](<DEV-PROXY.md>)。
- 管理端 HTTP 直连只适用于受控可信网络。HttpOnly、CSRF、Host/Origin 检查都不提供链路加密；外部 HTTPS 反代可选，板端隧道加密不保护浏览器链路。

## 与后端的真实接线状态

- 前端已接入生产装配：auth、management、tasks、status、events（SSE）、metrics、web 各线共享同一认证上下文，由 [中控生产入口](<../../crates/rsetup-controller/src/main.rs>) 经 `build_production_router` 合并进同一个 router，静态资产与 API 同源同进程；静态资产缺失时 release 构建直接失败（`build.rs` 校验），不产出「无前端」的二进制。
- **后端传输线与真实设备数据仍是 fail-closed 占位**：设备实时视图、恢复模式探针、commit 后通知与板端六方法分别由 `UnwiredTransportSources`、`UnwiredBoardClient` 承接，一律返回未接线错误；设备状态快照当前没有写入方；SSE 流上目前只有心跳与总线自身产生的 `reset`。因此真实环境里前端看到的是诚实的错误/空数据，这不构成设备数据或端到端链路已验证的证据。完整清单见 [中控 V1 当前已知缺口](<../../docs/testing/controller-v1-known-gaps.md>)。
- 前端离线测试（jsdom + 合成 fetch）通过不等于真实后端联调结果，也不等于真实浏览器端到端通过。

## 本地验证

运行环境：Node 22.22.1、npm 9.2.0。进入本应用目录后，所有 npm 操作使用应用内缓存及日志；只有确需重装依赖时才从本地缓存离线安装：

```sh
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" ci --offline --engine-strict --ignore-scripts --no-fund --no-audit
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" test -- --run
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run typecheck
NPM_CONFIG_CACHE="$PWD/.npm-cache" NPM_CONFIG_LOGS_DIR="$PWD/.npm-cache/_logs" npm --cache "$PWD/.npm-cache" run build
```

本目录以 `package-lock.json` 为唯一锁文件（其他包管理器的锁文件不入库），它固定依赖树。保留接管时的 Vue 3.5.43、Vite 8.3.2、Vitest 5.0.3 等直接版本；`@types/node` 固定为 22.12.0 以满足 Vite peer 范围。`overrides` 将 `@vue/test-utils` 固定为 2.4.6（满足 Vue Testing Library 的 `^2.4.1`），避免 2.5.1 引入的 `js-beautify@2` → `nopt@10` 要求 Node 22.22.2 而不兼容当前 22.22.1；这是已有安装错误证据的兼容性固定，不是漏洞修复或升级策略。本目录的 `pnpm-lock.yaml` 已被 `.gitignore` 忽略；请勿再生成或提交其他锁文件，统一使用 `package-lock.json`。

`node_modules/`、`coverage/`、`.npm-cache/` 仅为本地忽略产物。`dist/` 则**已随仓库提交**：
它是 [crates/rsetup-controller/build.rs](<../../crates/rsetup-controller/build.rs>) 编译期读取并嵌入中控二进制的交付物，入库后干净检出与离线构建不再需要 Node/npm。
改过本目录源码后必须回到仓库根运行 `make web-build` 重新生成 `dist/`，并把产物与源码改动
一起提交；CI 会从源码重新构建并用 `git diff --exit-code -- apps/controller-web/dist` 比对，
不一致即判红。`dist/.rsetup-build-stamp` 是 make 的时间戳文件，仍然忽略、不入库。
测试使用 jsdom 渲染真实 Vue SFC，并在每个测试后自动 cleanup。测试、类型检查与构建是独立步骤。`--no-audit` 并不表示依赖无漏洞。

本目录本身不要求常驻开发/预览服务（开发代理的用法见 [DEV-PROXY.md](<DEV-PROXY.md>)）；构建或 jsdom 测试通过不代表浏览器布局、键盘/辅助技术、320px 视口或端到端验收通过。已有的操作员无头浏览器执行记录与证据局限性见 [DEV-PROXY.md](<DEV-PROXY.md>) 与验收账本 T4-10；真实业务全链路与发布验收仍未完成，见 [中控 V1 当前已知缺口](<../../docs/testing/controller-v1-known-gaps.md>)。

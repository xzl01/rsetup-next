# Controller Web 开发代理使用说明

本文档说明 `apps/controller-web` 在本地前端开发阶段反向代理 `/api/v1` 请求到后端服务的目标规则与安全约束。

> **重要安全提示**：
> 1. 本代理配置**仅供本地开发环境联调**，严禁用于生产环境。
> 2. 生产环境中，前端静态文件与 `/api/v1` API 必须通过网关或同一反向代理服务器（如 Nginx、Caddy）保持真实同源部署。
> 3. 截至目前，**未执行**任何真实浏览器/端到端 (E2E) 或真实后端数据库联调；自动化测试仅验证了配置函数、类型推导、jsdom 及生产构建。

---

## 1. 环境变量配置

Vite 开发服务支持通过显式环境变量指定本地后端 loopback 地址：

```sh
# 启用代理：指定 loopback target（必须为 http 且带有明确端口）
export CONTROLLER_DEV_BACKEND_TARGET="http://127.0.0.1:8080"
```

如果未定义该环境变量（或值为空白），Vite 将不启用代理（`server.proxy` 为 `undefined`），保持纯静态开发模式。

---

## 2. 目标地址校验规则（Fail-Closed 闭环验证）

为了防止 SSRF、凭据泄露或错误路由，`resolveBackendProxyTarget` 对目标 URL 施加严格校验；任何不合规目标均直接抛出异常阻止启动：

- **仅限 HTTP 协议**：必须以 `http://` 开头（不支持 `https:` 或其他协议，本地 loopback 联调无需额外 TLS 隧道）。
- **仅限 Loopback 地址**：主机名仅允许 `127.0.0.1`、`localhost`、`[::1]`、`::1`。严禁 `0.0.0.0`、通配符（`*`）、局域网 IP（`192.168.x.x` / `10.x.x.x`）或外网域名。
- **显式合法端口**：必须包含非零合法端口号（`1`–`65535`）。
- **禁止凭据**：严禁携带用户名或密码（如 `http://user:pass@127.0.0.1:8080`）。
- **禁止 Query / Hash**：URL 不得包含查询参数（`?`）或片段标识符（`#`）。
- **禁止子路径**：目标 URL 路径必须为根路径（`/` 或空），不得指定路径前缀。

---

## 3. 请求头与安全对齐说明

后端 Controller 认证层施加了极其严格的防护策略：
- `CONTROLLER_ALLOWED_HOSTS`：严格精确匹配 `Host` 请求头（区分大小写与端口，无通配）。
- `CONTROLLER_ALLOWED_ORIGIN`：严格精确匹配 `Origin` 请求头（无通配）。
- Session Cookie：标记 `HttpOnly; SameSite=Strict; Path=/`。

因此，Vite 开发代理遵循以下安全准则：
1. **`changeOrigin: false`**：
   - Vite 代理**绝不改写**客户端发出的 `Host` 或 `Origin` 请求头为后端监听地址。
   - 联调时，Vite 请求的实际 Host/Origin 必须在后端的 `CONTROLLER_ALLOWED_HOSTS` 与 `CONTROLLER_ALLOWED_ORIGIN` 允许列表中，否则后端将返回 403 拒绝写入。
2. **零 CORS 通配**：
   - 不开启任何 CORS 通配符或宽松 header。
3. **保持同一同源 Cookie 语义**：
   - 前端通过相对路径 `/api/v1` 发起请求，`credentials: 'same-origin'` 依靠浏览器同源策略在开发服务器域名下管理 cookie。
4. **不影响 Production Build**：
   - 生产构建直接打包纯静态资产（HTML/CSS/JS），不嵌入任何 proxy 配置。

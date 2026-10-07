# Controller HTTP ID 契约定点修订与审查后续设计（2026-10-07）

## 背景与决策

[数据/API 规格 §1、§3](2026-09-23-controller-v1-02-data-api.md)将业务 `Id` 定义为小写标准 UUIDv4 字符串、`Revision/Counter` 定义为 JSON 十进制字符串；[HTTP 投影](../../../crates/rsetup-controller/src/http_api.rs)现已将 `user_public.id` 输出为 UUID 字符串、`revision` 输出为十进制字符串，并为响应生成 UUIDv4 `request_id`。但[前端身份状态](../../../apps/controller-web/src/auth.ts)的 `AuthUser` 没有 `id`，`revision` 可选，`assertIdentityShape` 未校验两者；[auth 测试](../../../apps/controller-web/src/auth.test.ts)和[App 测试](../../../apps/controller-web/src/App.test.ts)的正常身份 fixture 缺 `id`，且 `display_name: 'Sam'` 不属于当前 `user_public` 投影。这会让伪造的不完整成功响应进入已登录状态，使测试与实际 HTTP 不一致。

**采用**只在前端对接中控 `/api/v1` 的 HTTP JSON 边界修正类型、运行时校验、正常 fixture 和示例。`AuthUser.id` 为必需的小写标准 UUIDv4 字符串，`revision` 为必需的规范十进制字符串；`/auth/login` 与 `/auth/me` 均在接受身份和 CSRF 前验证，保持现有 `authz_epoch` 仅 `/auth/me` 必需。十进制字符串不经 `Number`/`parseInt`；验证至少排除空串、符号、小数、指数、前导零和 JSON number，若约束 u64 则仅用无损字符串/`BigInt` 比较，不能改变线上 wire 类型。UUID 必须完整小写连字符形式且版本位为 4、变体位为 RFC 4122（例如 `123e4567-e89b-42d3-a456-426614174000`），不接受大写、短串、其他版本或 number。`display_name` 仍是可选的未来显示字段，不在当前正常投影中捏造；App 正常断言用户名称改用 `username: 'admin'`。

**拒绝**全局重写 `api.ts` 的 `request_id` 解包校验、给所有字符串套 UUID 正则、修改 Rust 内部/数据库主键、板端 Protobuf `trace_id: uint64`，或将 session 公开别名改为 UUID。[会话设计](2026-10-05-controller-session-management-design.md)的 `sessions.items[].id` 仍是 HMAC 派生的小写 hex64、cursor 仍是 hex132；`request_id` 当前前端只校验非空有界字符串，保留其已有行为，但正常模拟 HTTP 包采用合法 UUIDv4。`trace_id` 只关联板端请求响应，不是 HTTP 业务 ID（见[设备协议](2026-09-23-controller-v1-03-device-protocol.md)、[协议原文](../../protocol_spec.md)）。未来 Device DTO 的 `device_id` 是小写公钥 hex64，而 MainTask/SubTask 等公共业务 `id` 为 UUIDv4 字符串、`revision` 为十进制字符串；仅写类型契约，不新增未实现页面或 API。

## 文件边界与数据流

实施仅定点涉及 `apps/controller-web/src/auth.ts`、`apps/controller-web/src/auth.test.ts`、`apps/controller-web/src/App.test.ts`，以及实际用作**正常 HTTP** 教学/计划示例且仍写 `request_id: 'r1'` 或短业务 ID 的既有文档示例（先逐例确认语境；[Web 基础计划](../plans/2026-10-03-controller-web-foundation.md)、[Web 认证计划](../plans/2026-10-03-controller-web-auth.md)可作为检查起点）。不替换刻意检验畸形响应的负例，不更改其他协议/DB 示例，不为文档修正扩展到生产代码。具体实施必须另经用户批准；本次仅落本文档。

`fetch → api.ts` 解包 `{data,request_id}` → `auth.ts` 对登录/会话身份作 runtime shape guard → 原有状态机。类型声明不能替代对不可信 JSON 的运行时验证；正常成功包才设置 `user`、`csrfToken`、`status`。缺失、number、格式错误的 `user.id`/`user.revision` 统一 `INVALID_API_RESPONSE`，清除本地身份、CSRF、epoch，绝不进入 `signed_in`/`force_password`；`/auth/me` 的 epoch 原规则不变。保留刷新仅有效 401 → `signed_out`、登录 401 的既有清状态分支；网络/未知 5xx 不冒充 401。改密结果未知时清身份/CSRF 的既有保守语义不放宽，不把 JSON 形状异常当成功。客户端不记录原响应、密码、cookie、CSRF 或私有密钥；界面只用安全本地文案。

## TDD、验证矩阵与证据边界

先在合成 `fetch` 边界增加行为 RED，再最小修改类型/guard 和正常 fixture 至 GREEN；RED 应是能运行的具体断言失败，不以缺依赖或编译失败冒充。[Web 基础设计](2026-10-03-controller-web-foundation-design.md)的 envelope、安全文本与无精度损失约束仍成立。

| 离线场景 | 必须断言 |
| --- | --- |
| `/auth/login`、`/auth/me` 正常完整投影（合法 UUIDv4、`revision: '1'`、正常 UUIDv4 `request_id`） | 分别进入原有 `signed_in`/`force_password`，可读 `user.id`/原样 revision；`/auth/me` 保留 epoch；UI 显示 `admin` 而非伪造 `Sam`。 |
| 两入口的 `id` 缺失、数字、短串、非 v4、非小写或错误变体；`revision` 缺失、数字、空串、带符号/小数/指数/前导零 | 均拒绝 `INVALID_API_RESPONSE`，清身份/CSRF/epoch；状态不伪称已登录。正常 UUID 字符串测试应使用固定合成值，不需真实服务。 |
| 有效 401、429、503、网络异常、改密未知结果、并发旧响应 | 原错误/代际守卫状态不退化；尤其 401 与未知结果不能混同，秘密仅留内存。 |
| session id hex64、cursor hex132、`request_id` 解包与故意畸形负例 | 既有正反测试语义不变；不得误套身份 UUID 校验。 |
| 文档/类型示例 | 只换正常 HTTP 伪短 ID 为合约值，业务 revision 保持字符串；设备公钥和 `trace_id` 保持原类型。 |

执行时可在 `apps/controller-web` 运行现有 Vitest、TypeScript typecheck、build，并视需用 Rust `http_api` 单测核对投影（以上均不需真实数据库或浏览器）；核对改动仅在已批准文件内、静态文档链接与差异，无凭据输出。这里不宣称测试已经运行、浏览器联调或真实 MySQL/TiDB 已验收。

## 其他审查项：仅列后续评估，不并入本次 ID 修订

- **并发登录限流**：审视同账户/来源并发失败计数的原子性和失败关闭边界；**改密 409 重验**：确认冲突后服务端会话/修订状态及前端可见语义，不能仅由通用 4xx 保态假定。参见[身份服务](../../../crates/rsetup-controller/src/auth/service.rs)、[HTTP 鉴权](../../../crates/rsetup-controller/src/http_auth.rs)。均需单独设计、竞争测试与批准。
- **A1 `revoked_count` 决策门**：[会话专项设计](2026-10-05-controller-session-management-design.md)明确非负 JSON integer，现有[HTTP handler](../../../crates/rsetup-controller/src/http_auth.rs)亦输出 number；[通用规格](2026-09-23-controller-v1-02-data-api.md)写 `Revision/Counter` 十进制字符串。先请用户裁决计数属于哪项规范、兼容与范围，再设计迁移/测试；不得借本次 ID 修订擅改 `revoked_count`。
- **A2 准入解码错误分类**：污染数据与不存在/未授权应区分，防止解码失败被掩为普通业务状态；**A3 用户名双规则**：对照输入与持久化解码 canonical 校验，确定统一规则及既有数据处理。分别审[设备服务](../../../crates/rsetup-controller/src/devices/service.rs)、[仓储解码](../../../crates/rsetup-controller/src/auth/sqlx_repo.rs)、[HTTP 登录](../../../crates/rsetup-controller/src/http_auth.rs)与[身份规格](2026-09-23-controller-v1-01-identity-access.md)后另批。
- **B1 Host 配置空格**：明确逗号分隔配置是否允许修剪空格，同时保持请求 `Host` 精确比对，不静默扩大允许来源；**B3 logout 业务错误映射**：校对已知失败与未知撤销结果，不把未知返回当注销成功；**B4 SessionClock 清理**：评估到期条目回收与生命周期，不引入隐式续期；**B5 bootstrap secret 持久恢复**：评估提交成功但秘密输出失败时安全恢复，禁止重复初始化或泄密。参见[HTTP 安全](../../../crates/rsetup-controller/src/http_security.rs)、[前端鉴权](../../../apps/controller-web/src/auth.ts)、[SessionClock](../../../crates/rsetup-controller/src/auth/session.rs)、[bootstrap](../../../crates/rsetup-controller/src/bootstrap.rs)。各项另拟设计、门禁和测试。
- **B2 pool 容量**：仅评估[HTTP live](../../../crates/rsetup-controller/src/http_live.rs)与服务并发/等待预算是否匹配，不预判已有缺陷；**C1 ignore 规则**、**C2/C3 低优先维护**：另列维护范围与验证，不趁机改忽略文件、依赖、CI 或清理目录。

涉及真实 DB、迁移、备份、生产回归、浏览器联调、密钥/持久秘密恢复方案、HTTP wire 计数变更、并发安全改造、协议或页面新增均需用户另行批准及相应安全审查；不得把离线 mock 结果替代这些证据。本设计不读取 `secret/`，不连接 DB，不提交代码。

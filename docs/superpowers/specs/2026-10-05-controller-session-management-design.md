# Controller 会话管理设计（2026-10-05）

## 已确认需求与边界

用户批准三个入口：`GET /api/v1/auth/sessions` 查看本人的会话；`POST /api/v1/auth/sessions/{id}/revoke` 按公开 ID 撤销指定会话；`POST /api/v1/auth/sessions/revoke-others` 撤销本人的所有其他会话，保留当前会话。改密保持 A-02 既有语义：事务内撤销本账号所有会话，包括当前会话，必须重新登录。本设计不开放管理员越权撤销他人会话、原始 token 查询、无会话可用的公共登记入口，也不改变设备接入/审批语义。

基线是 v3 `sessions(token_hash BINARY(32) PRIMARY KEY,user_id BINARY(16),process_epoch BINARY(16),created_time DATETIME(6),revoked BOOLEAN)`；cookie 原值只在客户端 HttpOnly cookie 中，数据库仅存 SHA-256 摘要。`SessionClock` 的 idle 30min / absolute 12h 和进程代际使旧进程所有会话失效。普通启动只读校验 schema；不能为此功能暗中迁移或松动 v3 形状检查。

## 方案选择

- **采用：进程内随机密钥派生公开别名**。进程启动用 OS CSPRNG 生成独立于 `process_epoch` 的 256-bit secret；`id = hex(HMAC-SHA256(secret, "rsetup-session-id-v1" || token_hash))`（64 个小写 hex）。HMAC 的密钥绝不持久化、记录或返回；别名只用于定位当前进程下本人既有会话，绝不充当认证凭据。重启后密钥改变，但旧进程会话本就无效。比较公开 ID 需固定格式并使用固定时长比较（或从按当前用户过滤的有界集合中匹配）；不得将 ID 解码成或直接回显 `token_hash`。只为已认证本人产生别名；退出、改密、停用后旧别名不能继续操作。TDD 固定密钥测试可复现，不使用生产静态密钥。
- **未采用：新持久化 UUID 列**。这会改变 v3 固定结构，要求新迁移、隔离 devDB 的真实备份/审核与兼容方案，不适合本阶段。**禁止**把 `token_hash`、cookie token 或未加密的拼接哈希作为公开 ID。

## HTTP、授权及前端

所有三个入口先检查单值精确允许 `Host`，写请求再检查单值 `Origin`（恰好缺失仅在有效 session + 正确绑定 CSRF 时豁免；重复/非 ASCII 必拒），从严格 `rsc_session` cookie 调用 `authenticate`，仅以鉴权成功的会话为 actor。未认证统一 401，后端/解码不确定错误 503，不泄漏 SQLx/密码/密钥/摘要。`must_change_password` 仅允许原有 `/auth/me`、`/auth/password`、`/auth/logout`，本组三个新接口统一 403 `PASSWORD_CHANGE_REQUIRED`。列表属于只读 50/s 预算；两个 POST 属已认证会话写 20/s 预算且要求当前 session digest 绑定的单值 `X-CSRF-Token`、`application/json` 及恰好 `{}` 的有界 JSON body（≤1MiB）。429 仅来自真实限速器并有整数 `Retry-After`；资源容量失败为 503 `RESOURCE_EXHAUSTED` + `Retry-After:60`。全程复用标准 `{data,request_id}` 或 `{error:{code,message_key,params:{}},request_id}`。

列表响应 `data:{items:[{id,current,created_time}],next_cursor}`，默认 limit 50、最大 200，时间 UTC 规范字符串、`current` 严格布尔；只列当前进程、当前账号、数据库未撤销且 `SessionClock` 此刻仍有效的会话。列表不得为**其他**会话续 idle；自身正常鉴权只续当前会话。从 DB 仅扫描该用户当前 `process_epoch` 的候选，严格解码 `revoked` 后再过滤已撤销行（不能在 SQL 中用 `revoked=FALSE` 掩盖污染）；内存及数据库候选都以最多 8192 条作硬上限，第 8193 条固定 503 `RESOURCE_EXHAUSTED`（附 `Retry-After:60`），不返回截断伪装的完整页。按公开别名字节序排序作为稳定决胜键；cursor 固定为 132 位小写 hex：32B 最后公开 alias + 2B big-endian limit + 32B HMAC；MAC 输入使用单独的 `rsetup-session-cursor-v1` 域标签、当前 actor ID、进程代际、该 32B alias 和 limit。篡改、其他用户复用或与当前 limit 不匹配均固定 400，不包含 cookie、`token_hash` 或内部 DB 标识。可变化的会话集合允许分页间遗漏刚撤销项，不承诺事务快照；前端撤销后刷新列表。输出绝不含 IP、设备名或 UA（当前 schema 没有这些可信字段），不凭空推断“设备”。

按 ID 指定：只能命中本人且同一进程的会话；格式错误 400，别人的 ID／不存在／已失效均同形 404 `NOT_FOUND`。指定当前会话成功时同普通登出，清 cookie、清 CSRF 绑定、本机 `SessionClock` 条目，并使前端进入 signed_out；指定他会话成功时保持当前 cookie/CSRF，响应 `data:{revoked:true}`，前端刷新列表。撤销他会话 DB 事务成功后应移除目标本进程内存 deadline（仅当 DB 已提交确认）；未持有该条目但 DB 未撤销时不伪称成功。`revoke-others` 的 `data:{revoked_count:<nonnegative integer>}` 仅统计本次实际从未撤销改为撤销的会话，重放可返回 0；保持当前 cookie/CSRF 与 deadline，其他已撤销的本进程 deadline 在确定提交后移除。服务或提交返回不确定 503 时不清当前 cookie/CSRF、不返回成功计数；前端提示刷新校准状态。

前端只在正常 signed_in 状态显示会话列表与两个操作及安全本地化文案；force_password 仍只呈现改密/普通登出。请求使用现有同源 `get`/`post`，CSRF 仅存内存，永不读取/写入 cookie、`localStorage` 或 URL 中的鉴权秘密（公开会话 ID 不是鉴权秘密）。指定当前注销成功清本地状态；注销其他成功保持登录并重新获取列表；401 清认证态；未知 5xx/网络结果不得宣称注销成功，重新 `me()` 和列表确认。不记录服务端原始错误内容。

## 事务/锁及错误边界

新仓储方法只接收服务层已鉴权的 `Session`、process epoch 和经服务层解析出的目标 digest；不接收未认证的客户端自称 user_id、raw token 或任意账号 ID。写事务统一先锁 `schema_meta` guard，再锁当前 `users` 行，再严格读取并锁 `sessions`（同类按 token_hash 字节序排序）；事务内重验 active、当前会话 owner/epoch/revoked、目标同 user/epoch、布尔污染，且更改 `revoked` 和去敏 `audit_events` 原子提交。失败回滚；`rows_affected` 与实际计数严格核对；并发改密/停用不能让旧会话撤销操作反向维持权限。对目标无权/不存在不得在错误体泄漏目标是否属于别人。会话撤销本身不改变角色/授权，不虚增 `schema_meta.authz_epoch`；保持既有审计事件命名规则，目标 ID 只使用非敏内部身份标识，绝不写 token/hash/public alias 到日志与 audit。

只读列表在不续其他会话的前提下检测其 DB `revoked`、用户归属、进程 epoch 和内存 deadline；所有持久化解码（包括 boolean、BINARY 长度、时间）严格，污染或查询失败为固定 503，不能当空列表。锁内不等待网络或执行 Argon2。若列表/撤销面对超过界限的历史行且无法保证完整安全结果，503 fail-closed；只有明确非本人的目标返回 404。成功返回后，其他会话的下一次 `/auth/me` 必须 401；已在途请求需在敏感事务内重验，不声称撤回已经提交的操作。

## TDD 与验收边界

依次做公开 ID/clock 纯单测（不同 digest/密钥、非 token/hash、重启失效、peek 不续期），FakeRepo 服务测试（跨用户 404、并发停用/改密、自己/其他/全部其他、失败回滚和 audit）、固定 SQL 目录/解码与仓储读写测试、Axum oneshot HTTP 测试（单值头、CSRF、限速、分页、403 强制改密、404 隐藏目标、清 cookie/失败保态）、前端 Vitest 状态/UI/i18n 测试；每轮先行为 RED 再最小 GREEN，独立审查规格与代码质量。不要把 mock 测试或 compile-only SQLx 当 MySQL/TiDB 的原子性、锁边或物理解码验收。真实 DB 仅在实际隔离目标、真实备份及迁移门禁都核准时测试；绝不删除数据库、不扩 writer grant、不提交/回显凭据。浏览器同源联调须单独记录真实页面/网络结果，否则只报告逻辑测试。

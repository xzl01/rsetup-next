# Controller 会话管理实施计划（2026-10-05）

> **执行者：** 必须按 `subagent-driven-development` 或 `executing-plans` 实施；以下任务先行为 RED 后最少 GREEN，每任务独立规格/质量审查。与正在进行的四基础 auth 路由工作共享 `http_auth.rs`，Task 4 必须等待其完成并审查，不可并发编辑。

**Goal:** 已认证用户可查看自身会话、按公开 ID 注销一个会话、注销除当前会话以外的全部会话；改密依旧撤销所有会话。

**Architecture:** v3 schema 不变。进程秘密 HMAC 生成不含 digest/token 的会话 ID；`SessionClock` 只读 peek，仓储严格列出本人当前进程的有界行并按统一锁序处理撤销和审计；HTTP 与前端复用已审 auth 安全层。数据库/浏览器验收单独门禁。

**Tech Stack:** Rust 1.85、SQLx 0.8.6、MySQL/TiDB、Axum 0.8、sha2 现有依赖、Vue 3、TypeScript、Vitest；零新增依赖。

**Spec:** `docs/superpowers/specs/2026-10-05-controller-session-management-design.md`（用户已审阅确认）；同时只读参考 01 A-02/A-03、02 §3–4、05 §6。

## Global Constraints

- 保留 v3 schema 与普通启动只读检查；严禁直接暴露/记录/审计 raw cookie、token_hash、HMAC key；公开 alias 仅能在本人已鉴权的会话列表与指定撤销路径使用，不得写日志或 audit。测试中固定密钥仅限单测 fixture。无新依赖，无真实 secret 或外网。无实际备份与隔离目标证明不运行 ignored 真 DB/迁移；不删数据库、不扩 grant。
- 所有 actor 由现有 `authenticate` 的有效 Session 得出；must_change_password 只准原有 `/auth/me|password|logout`，新路由 403 `PASSWORD_CHANGE_REQUIRED`。Host/Origin/CSRF 顺序和复查过的重复/非 ASCII 失败关闭规则不可改。
- 身份写入持久化时严格 guard→users→sessions（同类按 digest 顺序），同事务严格校验 + 仅在变更时去敏审计；对非本人/不存在同形 404。不因会话 revoke 虚增授权 epoch。
- 三新路由单独守 50/s 每 `(endpoint,session.digest)` 读与 20/s 每 session 写；未鉴权或 CSRF 失败不消耗他人额度。429 整数 Retry-After，资源容量503 Retry-After:60；≤1MiB JSON `{}` 两写；其余 503 不伪称成功。
- 严格 RED 必须可编译、断言行为失败；不可将导入/缺类型/ignore 当 RED。运行 `CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target cargo +1.85.0 test --offline --locked -p rsetup-controller`、fmt/clippy（Cargo lock 共用串行），前端在 app 目录运行带 `.npm-cache` 前缀的 npm test/typecheck/build。每任务报告真实 RED/GREEN 和限制。既有 readyz 全套测试尝试 synthetic loopback，不可称零 socket。

## 文件与依赖预检

| 任务 | 文件所有权及交接 | 冲突决策 |
| --- | --- | --- |
| 1 → 2 | `auth/session.rs` 产出 alias/peek，`auth/service.rs` 消费；类型可置 `auth/session.rs`。 | 2 仅在 1 的 API 审查后开始。 |
| 2 → 3 | `auth/service.rs` 新 trait 与 FakeRepo 方法，`auth/sqlx_repo.rs` 消费。 | 数据方法先有可编译桩并通过行为 RED，不把 SQL stub 错误当 RED。 |
| 3 → 4 | 仓储返回有界内部 row，`http_auth.rs`/`http_live.rs` 延伸对象安全 trait。 | 等四路由与限速任务完成；不得并发写 `http_auth.rs`。 |
| 4 → 5 | HTTP 固定公开合同，`apps/controller-web/src/auth.ts` 与 `App.vue` 消费。 | 代理配置已由另一任务独占，不修改。 |
| 各项自洽 | 每任务同时修改生产文件及对应 tests；`main.rs` 接线不得早于基础四路与限速审查。 | 不以 mock/compile-only 代替真 DB/浏览器。 |

### Task 1: 进程级公开 ID 和 clock peek

**Files:** Modify `crates/rsetup-controller/src/auth/session.rs` + tests。
**Consumes:** `SessionClock` 已有 `insert/check/remove`, `sha2`、OsRng。
**Produces:** `SessionAliasKey::new()` 及测试专用固定密钥构造、`alias(digest: &[u8;32])->String`、`matches(digest,id)->bool`、内部 `sign_cursor(payload)->[u8;32]`（使用与别名不同的 HMAC 域分隔标签）；`SessionClock::is_live(&self,digest,now)->bool`（不续 idle）。对目标公开 ID 可先 validate 64 lowercase hex，非格式不做 key 计算。

- [ ] RED: 先加入可编译 stub（`alias` 固定全零、`matches` 恒 false、`is_live` 恒 false），写行为测试：同 digest/同 key 稳定；不同 key/digest 的 64-hex 不同且不同于 hex(digest)；别名错误值不可匹配；`sign_cursor` 与 `alias` 使用不同域隔离；`is_live` 不续 idle 且在等号边界过期、未建立/已 remove 返回 false。
  ```rust
  let key = SessionAliasKey::from_test_bytes([7; 32]); // 仅 #[cfg(test)]
  let digest = token_digest(b"fixture-not-a-cookie");
  let alias = key.alias(&digest);
  assert_eq!(alias.len(), 64);
  assert_ne!(alias, hex::encode(digest));
  assert!(key.matches(&digest, &alias));
  assert!(!key.matches(&token_digest(b"other"), &alias));
  let clock = SessionClock::new();
  let t0 = Instant::now();
  clock.insert(digest, t0);
  assert!(clock.is_live(&digest, t0 + IDLE - Duration::from_nanos(1)));
  assert!(!clock.is_live(&digest, t0 + IDLE)); // peek 不得续期
  ```
- [ ] 运行 `cargo +1.85.0 test --offline --locked -p rsetup-controller --lib auth::session`（带 Global Constraints 的 CARGO_HOME/TARGET_DIR），确认至少这些断言按预期 RED 而非编译失败。
- [ ] GREEN: 使用标准 HMAC-SHA256（64-byte key pad、`0x36/0x5c` 内外两次 SHA-256、固定域分隔串和 32B digest），OsRng 初始化进程 key；32 字节固定时长 XOR 比较；`is_live` 只读取 deadline 不调用 `check`，保留原续期行为不变。测试 Key 零值只在 `#[cfg(test)]`。
- [ ] 重跑定向测试和全 Rust 离线套件；审查密码学输入/密钥保密、边界与副作用。

### Task 2: 服务层会话业务与 FakeRepo

**Files:** Modify `crates/rsetup-controller/src/auth/service.rs` + tests and `crates/rsetup-controller/src/error.rs`（仅新增固定 `ControllerError::ResourceExhausted` 变体）；必要的内部会话行类型与 `IdentityRepository` trait 声明在 service.rs。
**Consumes:** Task 1 alias/peek；`IdentityUser` 与 `Session`；现有六仓储方法。
**Produces:** 内部 `StoredSession {digest, created_time, revoked, process_epoch, owner_id}` 及 `list_user_sessions(session,epoch)->Result<Vec<StoredSession>,ControllerError>`（必须扫描 ≤8193 行并严格解码）；`revoke_selected_session(session,epoch,target_digest)->Result<bool,ControllerError>`（false=该账号目标不存在/非活）；`revoke_other_sessions(session,epoch)->Result<(u64,Vec<digest>),ControllerError>`（已提交的实际数量/摘要供成功后清 clock）。**为保持现有生产 `SqlxIdentityRepository` 在 Task 2 后可编译，三个 trait 新方法临时提供固定 `Config("identity session management not wired")` 默认返回，不把它接入 `main`；FakeRepo 在本任务覆盖，Task 3 必须全部 override 并经源码审查后才算仓储交付。** Service 方法 `list_sessions`, `revoke_by_alias`, `revoke_others` 不能接收任意用户 ID，并在返回时把 digest 只在内部使用。

- [ ] 可编译 trait/FakeRepo 桩 + 真实断言 RED：两个活会话返回当前标识与创建时间，不延长另一个 idle；不属于本用户的 alias 和过期 alias 同形 NotFound；单个他会话撤销后当前仍可 authenticate，被撤销的不能；按 ID 撤当前与普通 logout 一致；批量撤其他保当前且 2→count2、重放→count0；FakeRepo 注入失败时全状态/审计不变；强制改密身份在服务入口被拒。最小签名示例（后续实现时须与 Rust trait 的 `impl Future` 形状一致）：
  ```rust
  fn list_user_sessions(&self, actor: &Session, epoch: [u8; 16])
      -> impl Future<Output = Result<Vec<StoredSession>, ControllerError>> + Send;
  fn revoke_selected_session(&self, actor: &Session, epoch: [u8; 16], target: [u8; 32])
      -> impl Future<Output = Result<bool, ControllerError>> + Send;
  fn revoke_other_sessions(&self, actor: &Session, epoch: [u8; 16])
      -> impl Future<Output = Result<(u64, Vec<[u8; 32]>), ControllerError>> + Send;
  // fake test: revoke_others(&actor) 得到 2 后再次调用得到 0，
  // actor 原 token 仍能 authenticate，其他两个原 token 均 InvalidArgument。
  ```
- [ ] 在可编译 trait/FakeRepo 桩阶段先增加固定 `ControllerError::ResourceExhausted`，更新现有错误映射的所有穷尽匹配（若为 wildcard 则在 Task 4 新路由中显式映射 503 + Retry-After:60）；首个测试必须证明超过候选容量不是 200/429/普通 503。
- [ ] 运行定向 `auth::service` 测试，记录断言 RED。
- [ ] 最少实现：service 只处理 alias 过滤、授权、clock；FakeRepo 模拟 guard+用户+session 串行、严格 owner/epoch/revoked 与 audit，不虚构 DB 原子性；避免持有 Mutex 跨 await。按 spec 只列同用户当前 epoch 未撤销且内存仍有效。返回结果不能含 raw token。
- [ ] 定向和全 Rust 离线套件 GREEN；独立审查 trait 的返回类型不会让 HTTP 构造 actor。

### Task 3: SQLx 持久化读/写边界

**Files:** Modify `crates/rsetup-controller/src/auth/sqlx_repo.rs` + 同文件 tests（除必要编译适配外不改 service.rs）；不改 migration。
**Consumes:** Task 2 三个新 trait 方法/内部 StoredSession；既有 guard/strict decode/audit helper。
**Produces:** 有界列举与两个撤销事务：固定 SQL、严格 `LIMIT 8193`/计数拒绝、owner/epoch/boolean/bytes/time 解码；每次写前 guard→users→sorted sessions 锁内重验当前生存、目标所有权及 epoch；只实际变更的 target 审计，rows_affected 精确；失败整体 rollback；commit 不确定不返回成功。

- [ ] 可编译生产桩后先写 SQL catalog/纯行解码/事务决策 RED：SELECT 不用 `revoked=FALSE` 掩盖污染；8193 行固定资源失败；跨用户和已撤销相同 404；单个 target 当前/其他取锁字节序一致；count 与 changed rows 不符失败；审计参数不包含 alias/digest；RevisionConflict/Config/Database 不误报成功。
  ```rust
  const LIST_USER_SESSIONS_SQL: &str = "SELECT token_hash, user_id, process_epoch, created_time,
      CAST(revoked AS SIGNED) AS revoked FROM sessions
      WHERE user_id = ? AND process_epoch = ? ORDER BY token_hash LIMIT 8193";
  // RED: polluted revoked=2 must be rejected, never suppressed by WHERE revoked=FALSE.
  // RED: rows.len()==8193 -> fixed RESOURCE_EXHAUSTED class before public projection.
  // RED: changed_rows != expected_live_count -> rollback, never Ok(count).
  ```
- [ ] 运行 `--lib auth::sqlx_repo` 确认行为 RED；最少 SQLx 事务实现；同任务执行 `--lib` / 全包、fmt/clippy。不要把纯测试说成真实事务或物理解码验收；若需要真 DB 等独立门禁。

### Task 4: HTTP 和生产适配器

**Files:** Modify `crates/rsetup-controller/src/http_auth.rs`, `src/http_live.rs` + tests；如需增加 auth trait 方法需同时改既有 FakeAuth；不改 `main.rs` 直到基础四路与限速双门禁都获审查。
**Consumes:** Tasks 1–3 Service 方法与前期 20/s、50/s 限流器、固定封装。
**Produces:** `GET /api/v1/auth/sessions?limit=...&cursor=...`、`POST /api/v1/auth/sessions/{id}/revoke`、`POST /api/v1/auth/sessions/revoke-others`；object-safe `AuthServiceApi` 与生产 `LiveAuth` 同步实现。

- [ ] FakeAuth 可编译桩 → oneshot 行为 RED：仅本人有权，must_change_password 403，恶意 ID 400/不存在与他人一致 404；按当前 ID 成功清 cookie/CSRF、按其他 ID 和批量成功不清当前；重复/畸形 Host/Origin、缺/重复 CSRF 403 且零业务调用；JSON 非 `{}`/无 CL >1MiB 400；50/s/20/s 真实 limiter 429+Retry-After、容量503/60；cursor 绑定用户与 limit、篡改400；DB 故障503不宣称撤销。
  ```rust
  // Router registrations (not an authentication shortcut):
  .route("/api/v1/auth/sessions", get(list_sessions))
  .route("/api/v1/auth/sessions/revoke-others", post(revoke_others))
  .route("/api/v1/auth/sessions/{id}/revoke", post(revoke_by_id))
  // oneshot assertions: forged other's id -> 404; malformed id -> 400;
  // self revoke -> 200 + Set-Cookie Max-Age=0; other revoke -> 200 and NO Set-Cookie;
  // verified CSRF duplicate Origin -> 403 before any service call.
  ```
- [ ] 定向 `--lib http_auth` RED；最少实现并复用既有 `check_session_write`、body 限制、envelopes，不复制另一套 Origin 豁免。动态路由放在静态 `/revoke-others` 后并明确避免冲突。`LiveAuth` 五旧方法行为不变，新方法原样委托。
- [ ] 全 Rust 离线测试、fmt/clippy GREEN；审查 production main 实际已接上述新路由且启动 fail closed，未满足门禁绝不接。

### Task 5: 前端状态与界面

**Files:** Modify `apps/controller-web/src/auth.ts`, `src/auth.test.ts`, `src/App.vue`, `src/App.test.ts`, `src/locales/en.ts`, `src/locales/zh-CN.ts`, `src/i18n.test.ts`；不改 proxy/Vite 配置。
**Consumes:** Task 4 固定 JSON 合同，现有 `api.get/post` 和内存 CSRF。
**Produces:** signed_in 态会话列表与“注销该登录/注销其他登录”操作；强制改密态无列表/按钮；本地安全双语提示。

- [ ] 可编译 UI/store stub 后写 mock fetch 行为 RED：正确 URL/JSON/CSRF、只显示属于登录用户的公开 ID/当前标识；当前自注销进入 signed_out，其他/全部其他保持当前 token 并刷新列表；401 清态，503/网络错误不虚报结果并转重试；异常数据不显示；禁 cookie/localStorage 原始访问；两个语言的 key 集合相等。
  ```ts
  // auth.test.ts: mock GET /auth/sessions -> {items:[{id: 'a'.repeat(64), current:false,
  // created_time:'2026-10-05T00:00:00Z'}],next_cursor:null}; assert no cookie reads.
  await store.revokeSession('a'.repeat(64))
  expect(fetcher).toHaveBeenCalledWith('/api/v1/auth/sessions/' + 'a'.repeat(64) + '/revoke',
    expect.objectContaining({method:'POST',credentials:'same-origin'}))
  expect(store.status.value).toBe('signed_in')
  // same user's current id success -> signed_out; 503 -> retryable error, NOT signed_out.
  ```
- [ ] 带本地 npm cache 定向测试 RED，再最少实现；`npm test -- --run`、`npm run typecheck`、`npm run build` GREEN。jsdom/mock 不声称真实浏览器。

### Task 6: 验收与门禁（不替代前五项）

**Files:** 仅在获准的已有隔离测试报告位置增补去敏记录，不修改授权数据或凭据。
- [ ] 对两个明确授权的可丢弃开发数据库分别在实际连接之前核验目标、写权限、真实备份引用及迁移门禁；未满足则记录阻断，绝不试跑 ignored 测试、任意迁移或 DROP DATABASE。
- [ ] 只有门禁满足后运行按用户隔离的真实 SQLx 原子性/回滚/物理解码测试；在另一层使用真实浏览器与可信网络做同源登录/列表/撤销验收，不把 fake 测试或编译推断成真实结果。
- [ ] 最终独立跨分支审查、安全 secret/index 审计、全包离线 Rust 与前端测试/typecheck/build，并明确列出仍缺设备真实连接/审批业务验证。

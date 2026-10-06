# 计划：Controller auth HTTP 层 — 四 auth 端点 / router / config / 安全 / 限速（2026-10-03）

> **致执行代理：** 必需子技能：superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans。用 `- [ ]` 复选框跟踪步骤；每任务完成后做独立审查（审查方未参与实现，仅读 diff 与本计划）。

**Goal:** 为 `POST /api/v1/auth/login`、`GET /api/v1/auth/me`、`POST /api/v1/auth/password`、`POST /api/v1/auth/logout` 提供 HTTP 层：router、JSON 契约、Host/Origin/CSRF、cookie 策略、登录限速；全部以 tower oneshot + 内存 fake 验证。

**Architecture:** 三个新文件分层：`src/http_api.rs`（契约/静态响应合同）→ `src/http_security.rs`（纯安全函数与限速器）→ `src/http_auth.rs`（`AuthServiceApi` trait + router/handler，fake 注入）。业务逻辑仍在 `AuthService<R: IdentityRepository>`（`service.rs` 只读消费）；本层不重复 dummy Argon2、不改仓储。

**Tech Stack:** Rust 1.85 / axum 0.8、serde_json、uuid(v4)、rand(OsRng)、tower 0.5(dev, `ServiceExt::oneshot`)——全现依赖，**零新增**（`Cargo.lock` 的 rsetup-controller 条目无 serde，`--locked` 下加依赖即失败；契约用 `serde_json::Value` 手工构造/校验）。

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md` L18–34（A-02/A-03）+ `2026-09-23-controller-v1-02-data-api.md` L83–110；仓储/服务接口见 `docs/superpowers/plans/2026-10-03-controller-auth-repository.md` 接口段。

## Global Constraints

1. 文件面：仅新增 `src/http_api.rs`、`src/http_security.rs`、`src/http_auth.rs`；仅改 `src/lib.rs`（mod/re-export）、`src/main.rs`（门禁见 Task 3 Step 6）。**不改** `auth/service.rs`、`auth/sqlx_repo.rs`、`db.rs`、`ui/`、其他 crate。
2. 别假设旧 router 已有 auth 端点：现 `build_router` 只有 `/healthz` `/readyz`（`src/lib.rs:45-48`），保留不动；新 router 是独立 `build_http_router`，既有 lib.rs 测试（health 200 / readyz 503）原样不回归。
3. RED→GREEN 统一命令（worktree 根执行，离线锁定，与仓储计划一致）：
   ```bash
   CARGO_HOME=$PWD/.cargo-home CARGO_TARGET_DIR=/home/aghost/workspace/rsetup-next/target \
     cargo +1.85.0 test --offline --locked -p rsetup-controller
   ```
4. 无真实 secret/DB/网络：测试全为内存 fake + oneshot；限速器进程内不写库；dummy Argon2id 与固定审计由仓储/服务计划承担，本层不实现。
5. 契约：成功 `{data,request_id}`、失败 `{error:{code,message_key,params},request_id}`（request_id=UUID v4）。错误信息不得含密码、raw token、digest、username 原文、URL。code 只用 02 表（L89–97）：400 INVALID_ARGUMENT / 401 AUTH_REQUIRED、INVALID_CREDENTIALS / 403 CSRF_INVALID / 404 NOT_FOUND / 409 REVISION_CONFLICT / 429 RATE_LIMITED（附 Retry-After）/ 503 NOT_READY、STORAGE_UNAVAILABLE、RESOURCE_EXHAUSTED（资源耗尽固定 `Retry-After: 60` 秒，仅退避提示、不承诺届时可用）。
6. 安全：登录前只要求精确允许列表 Host + 精确允许 Origin + JSON（无 Origin 的初次登录拒绝）；已有 session 的非登录**写**请求还须绑定该 session 的 `X-CSRF-Token`（无 Origin 的非浏览器客户端仅在该绑定+允许 Host+JSON 成立时接受）；无效/丢失/格式非法 cookie 一律 401、**不得当作 actor**（所有 HTTP business actor 必须来自有效 session）。`must_change_password` 白名单仅 me/password/logout：本计划 4 端点不因其拒绝；非 auth 端点的 `PASSWORD_CHANGE_REQUIRED` 门禁属 02 计划。CSRF 绑定以 session digest 作键，成功 logout/password 必须删除，失效 session 在鉴权失败时清除；最多持有 4096 条，容量不足固定 503 RESOURCE_EXHAUSTED + `Retry-After: 60` 并拒绝新登录或补发 token，绝不无界增长或覆盖仍有效会话。
7. JSON 写请求体最多 1_048_576 字节（超限 400 INVALID_ARGUMENT，不得把 Axum 默认 413 当合约）。每个 handler 先检查原始 Content-Length（若有），再用 `axum::body::to_bytes(body, 1_048_576)` 有界读取；包括**无 Content-Length 的分块体**越界也固定映射 400，拒绝未知字段/非 JSON Content-Type，不等待无限字节后才检查。
8. Cookie 名 `rsc_session`，属性 `HttpOnly; SameSite=Strict; Path=/`；HTTP 直连**不**设 Secure，仅 `CONTROLLER_TRUST_TLS=true` 才追加 `Secure`（不盲信转发头）；值固定 64 位小写 hex。
9. 限速：登录按 (账号) 与 (来源=连接地址) 两维各自统计，15min 内 5 次失败→该窗口 429，不永久锁死。
10. Stub 必须可编译：RED 一律**行为 RED**（stub 返回 501/固定错误），禁止用缺类型/缺导入/编译失败当 RED，禁止 `#[ignore]`。
11. 范围外：用户管理/审批/设备接入/observer/真实 DB 执行。禁止运行第 3 条之外的 cargo 命令、联网、读 secret、stage/commit、派代理。

---

### Task 1: `src/http_api.rs` — 契约类型 + 静态响应合同（前端 mock）

**Files:**
- Create: `crates/rsetup-controller/src/http_api.rs`（含 `#[cfg(test)] mod tests`）
- Modify: `crates/rsetup-controller/src/lib.rs`（`pub mod config;` 后加 `pub mod http_api;`）

**Consumes:** `serde_json`、`uuid`、`hex`、axum `Response/StatusCode/Json`（现依赖）；`crate::auth::service::IdentityUser`（`service.rs:10-19`，只读）。
**Produces:**
```rust
pub fn request_id() -> String;                                    // UUID v4
pub fn ok_response(data: serde_json::Value) -> axum::response::Response;   // 200 {data,request_id}
pub fn err_response(status: axum::http::StatusCode, code: &str, message_key: &str, params: serde_json::Value) -> axum::response::Response;
pub fn user_public(u: &crate::auth::service::IdentityUser) -> serde_json::Value;  // 非敏字段
```

- [ ] **Step 1: RED 测试 + 可编译 stub**（stub：`request_id` 零值串；`ok/err_response` 返 501 空体；`user_public` 返 `Value::Null`）：

```rust
#[test] fn ok_envelope_shape() { /* ok_response(json!({"changed":true}))：200；body（to_bytes 读出后 from_str）恰含 data(==输入) 与 request_id(uuid v4 可解析) */ }
#[test] fn err_envelope_shape() { /* 状态码透传；body 恰含 error{code,message_key,params:{}} 与 request_id */ }
#[test] fn user_public_redacts_and_strict_bool() {
    // IdentityUser{id:*uuid::Uuid::new_v4().as_bytes(),username:"a",password_hash:"$argon2..",active:true,is_admin:true,must_change_password:true,revision:1}
    // → 无 "password_hash" 键；must_change_password 是 JSON bool；id 是小写标准 UUIDv4 字符串（不是 hex32）；含username 非空/active/is_admin/revision("1") }
```

- [ ] **Step 2: 确认 RED** — 第 3 条命令：3 项行为测试 FAIL（可编译 stub 返 501/Null，与目标响应不符；不能将注释占位计为测试断言）。
- [ ] **Step 3: GREEN 最小实现**：

```rust
pub fn request_id() -> String { uuid::Uuid::new_v4().to_string() }
pub fn ok_response(data: Value) -> Response {
    (StatusCode::OK, Json(json!({"data": data, "request_id": request_id()}))).into() }
pub fn err_response(status: StatusCode, code: &str, message_key: &str, params: Value) -> Response {
    (status, Json(json!({"error":{"code":code,"message_key":message_key,"params":params},
        "request_id": request_id()}))).into() }
pub fn user_public(u: &IdentityUser) -> Value {
    json!({"id": uuid::Uuid::from_bytes(u.id).to_string(), "username": u.username, "active": u.active,
           "is_admin": u.is_admin, "must_change_password": u.must_change_password,
           "revision": u.revision.to_string()})
}
```

- [ ] **Step 4: 确认 GREEN** — 第 3 条命令全绿。
- [ ] **Step 5: 独立审查** — ① 两 envelope 键名与 02 L85 逐字一致；② `user_public` 无 `password_hash`/token 且 bool 严格；③ 前端 mock 合同遵循 01/02 规格与真实HTTP集成测试，不另造生产静态JSON常量；④ 除 lib.rs mod 行外无其他改动。

---

### Task 2: `src/http_security.rs` — Host/Origin、cookie、CSRF、登录限速（纯函数）

**Files:**
- Create: `crates/rsetup-controller/src/http_security.rs`（含 `#[cfg(test)] mod tests`）
- Modify: `crates/rsetup-controller/src/lib.rs`（加 `pub mod http_security;`）

**Consumes:** `ControllerError::Config`；`std::time::Instant`（不依赖 http_api）。
**Produces:**
```rust
pub const COOKIE_NAME: &str = "rsc_session";
pub const RATE_WINDOW: std::time::Duration; pub const RATE_MAX_FAILURES: usize; // 15min / 5
#[derive(Clone, Debug)] pub struct HttpSecurityConfig { pub allowed_hosts: Vec<String>, pub allowed_origin: Option<String>, pub trust_tls: bool }
impl HttpSecurityConfig {
    pub fn from_values(hosts: Option<&str>, origin: Option<&str>, trust_tls: Option<&str>) -> Result<Self, ControllerError>; // 纯函数单测
    pub fn from_env() -> Result<Self, ControllerError>; // 生产中只读env后委派from_values，不在Tokio运行时改env
}
// CONTROLLER_ALLOWED_HOSTS 必填（逗号分隔精确 Host 串，含端口，如 127.0.0.1:8080）；CONTROLLER_ALLOWED_ORIGIN 必填（精确 Origin 串，如
// http://127.0.0.1:5173，CLI 登录也须显式发送）；CONTROLLER_TRUST_TLS 可选 "1"/"true"，默认 false。缺失/空 → ControllerError::Config（信息不含值）。
#[derive(Debug, PartialEq)] pub enum WriteDeny { Host, Origin, Csrf }
pub fn check_pre_session(host: Option<&str>, origin: Option<&str>, cfg: &HttpSecurityConfig) -> Result<(), WriteDeny>;
pub fn check_session_write(host: Option<&str>, origin: Option<&str>, csrf: Option<&str>, bound: &str, cfg: &HttpSecurityConfig) -> Result<(), WriteDeny>;
pub fn session_cookie_header(token: &str, cfg: &HttpSecurityConfig) -> Result<String, ControllerError>; // 仅接受64位小写hex，防HeaderValue注入；内部错误固定Config
pub fn clear_session_cookie_header() -> String;
pub const LOGIN_RATE_MAX_KEYS: usize = 4096;
pub enum RateLimitError { Exhausted }
pub struct LoginRateLimiter; // new()；record_failure(&mut self, now: Instant, account: &str, source: IpAddr) -> Result<(),RateLimitError>；
                             // retry_after(&mut self, now: Instant, account: &str, source: IpAddr) -> Result<Option<Duration>,RateLimitError>
                             // 满容量且新账号/来源无法记录时Err(Exhausted)→固定503 RESOURCE_EXHAUSTED+Retry-After:60，不静默丢条目
```

- [ ] **Step 1: RED 测试 + 可编译 stub**（stub：check 恒 `Ok(())`、cookie 空串、limiter 恒 `None`）：

```rust
fn cfg() -> HttpSecurityConfig { HttpSecurityConfig { allowed_hosts: vec!["127.0.0.1:8080".into()], allowed_origin: Some("http://127.0.0.1:5173".into()), trust_tls: false } }
#[test] fn pre_session_requires_exact_host_and_origin() { /* 全对→Ok；错 host→Host；缺 origin→Origin；错 origin→Origin（大小写敏感精确比较） */ }
#[test] fn session_write_requires_bound_csrf() { /* 无 Origin+CSRF==bound→Ok；无 Origin+CSRF 缺/错→Csrf；Origin 不匹配→Origin；host 不在列表→Host */ }
#[test] fn cookie_headers() { /* 仅合法token=`hex::encode([0x5a_u8;32])`可生成：trust_tls=false 含rsc_session=、HttpOnly/SameSite=Strict/Path=/、Max-Age=43200，不含Secure；trust_tls=true含Secure；含CR/LF或长度非64/大写的token固定Config拒绝、不得回显原文；clear含Max-Age=0 */ }
#[test] fn rate_limiter_five_failures_in_window() {
    let (mut rl, t0, ip) = (LoginRateLimiter::new(), Instant::now(), "10.0.0.1".parse::<IpAddr>().unwrap());
    for i in 0..5 { rl.record_failure(t0 + Duration::from_secs(i), "admin", ip).unwrap(); }
    assert!(rl.retry_after(t0 + Duration::from_secs(30), "admin", ip).unwrap().is_some());    // 同账号
    assert!(rl.retry_after(t0 + Duration::from_secs(30), "other", ip).unwrap().is_some());    // 同源不同账号也阻断
    assert!(rl.retry_after(t0 + RATE_WINDOW + Duration::from_secs(1), "admin", ip).unwrap().is_none()); // 窗口过后放行
}
#[test] fn config_from_values() { /* 缺hosts/缺origin→Err；齐全且trust_tls缺省→Ok(false)；"1"→Ok(true)；不调用std::env::set_var/remove_var */ }
#[test] fn limiter_fails_closed_at_key_capacity_and_recovers_after_expiry() { /* 4096个不同账号且来源键有限；第4097个新账号 record_failure/retry_after→Exhausted，窗口过后旧键可清理再用；双键同时阻断取较长剩余期限 */ }
```

- [ ] **Step 2: 确认 RED** — 第 3 条命令：六组测试中至少 Host/Origin、CSRF、cookie、失败窗口及容量行为有断言 RED（可编译），config 测试桩若本来满足可GREEN但不能计作RED；测试函数必须含真实断言。
- [ ] **Step 3: GREEN 最小实现**：

```rust
fn host_ok(host: Option<&str>, cfg: &HttpSecurityConfig) -> bool {
    host.map_or(false, |h| cfg.allowed_hosts.iter().any(|a| a == h))
}
pub fn check_pre_session(host: Option<&str>, origin: Option<&str>, cfg: &HttpSecurityConfig) -> Result<(), WriteDeny> {
    if !host_ok(host, cfg) { return Err(WriteDeny::Host); }
    if origin.is_none() || cfg.allowed_origin.as_deref() != origin { return Err(WriteDeny::Origin); } Ok(())
}
pub fn check_session_write(host: Option<&str>, origin: Option<&str>, csrf: Option<&str>, bound: &str, cfg: &HttpSecurityConfig) -> Result<(), WriteDeny> {
    if !host_ok(host, cfg) { return Err(WriteDeny::Host); }
    if origin.is_some_and(|o| cfg.allowed_origin.as_deref() != Some(o)) { return Err(WriteDeny::Origin); }
    if csrf != Some(bound) { return Err(WriteDeny::Csrf); } Ok(()) // 无 Origin 仅豁免 Origin 检查，不豁免 CSRF 绑定
}
pub fn session_cookie_header(token: &str, cfg: &HttpSecurityConfig) -> Result<String, ControllerError> {
    if token.len()!=64 || !token.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(ControllerError::Config("invalid session token".into()));
    }
    Ok(format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age=43200{}",
        if cfg.trust_tls { "; Secure" } else { "" }))
}
pub fn clear_session_cookie_header() -> String { format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0") }
```
`LoginRateLimiter`：由 `HttpAuthState` 外层 `Mutex<LoginRateLimiter>` 序列化访问；内部账号→失败时间与来源 IP→失败时间两个普通 `HashMap`。`retry_after(&mut self,...)` 在查两键之前先删除过期条目并保持各窗口内最多5个时间戳，任一键达5则返回尚需等待期限、两键同时达限取**较长**的 Retry-After（`0<d≤RATE_WINDOW`）。`record_failure`/`retry_after` 遇新键而账号或来源映射分别已有4096个活动键时返回 `RateLimitError::Exhausted`，handler固定503 RESOURCE_EXHAUSTED+Retry-After:60；不得静默删除活跃键放行攻击者。缺可信`ConnectInfo<SocketAddr>`固定503 NOT_READY、不猜来源。纯fake测试覆盖容量+过期回收。

- [ ] **Step 4: 确认 GREEN** — 第 3 条命令全绿。
- [ ] **Step 5: 独立审查** — ① 全部精确相等比较（无大小写折叠/通配）；② 无 Origin 时仍强制 CSRF 绑定；③ cookie 属性与约束 8 一致、`trust_tls=false` 无 Secure；④ 限速窗口/次数与约束 9 一致，无持久化、无第 3 个统计维度。

---

### Task 3: `src/http_auth.rs` — router + 四端点 handler + main 接线（门禁）

**Files:**
- Create: `crates/rsetup-controller/src/http_auth.rs`（含 `#[cfg(test)] mod tests`：`FakeAuth` + oneshot 路由测试）
- Modify: `crates/rsetup-controller/src/lib.rs`（`pub mod http_auth;` + `pub use http_auth::{AppState, HttpAuthState, build_http_router};`）
- Modify: `crates/rsetup-controller/src/main.rs`（仅 Step 6 门禁通过后）

**Consumes:**
- Task 1 `http_api::*`；Task 2 `http_security::*`。
- `crate::auth::service::{AuthService, IdentityRepository, Login, Session}`（`service.rs:73/95/115/133` 四个公开方法，签名只读）；仓储计划改的是 trait 内部方法（`insert_session(&IdentityUser)`、新增 `record_login_failure`），**不改变本层消费的四个方法签名**。
- `me.authz_epoch` 是 `schema_meta.authz_epoch BIGINT UNSIGNED` 的**每请求当前值**，JSON 为 `u64` 十进制字符串；绝不是 `SessionClock.epoch`（进程 UUID）。Task 3 的生产 `LiveAuth` 适配器持 `DbPool`，在已验证 session 后以固定 SELECT 读取完整 schema_meta 单例 `(singleton=1, authz_epoch)`，缺行/多行/解码错固定 503；FakeAuth 使用固定 u64 测试。无需、更禁止向 `AuthService` 增加错误的进程 epoch accessor。

**Produces:**
```rust
pub type AuthFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;
pub trait AuthServiceApi: Send + Sync {
    fn login<'a>(&'a self, username: &'a str, password: &'a str) -> AuthFuture<'a, Login>;
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session>;
    fn change_password<'a>(&'a self, session: &'a Session, current: &'a str, new_password: &'a str) -> AuthFuture<'a, ()>;
    fn logout<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, ()>;
    fn authz_epoch(&self) -> AuthFuture<'_, u64>; // schema_meta 当前授权计数，绝非 process epoch
}
// Task 3 还产出生产 LiveAuth<R: IdentityRepository> { service: Arc<AuthService<R>>, db: DbPool }；
// 五个方法分别 Box::pin 同名服务调用或固定 SELECT schema_meta(singleton,authz_epoch)。
// FakeAuth 实现同一 object-safe trait（只用合成 u64，无 DB）。
pub struct HttpAuthState { /* auth: Arc<dyn AuthServiceApi>, security: HttpSecurityConfig,
    csrf: Mutex<bounded session-digest -> csrf>, rate: Mutex<LoginRateLimiter> */ }
impl HttpAuthState { pub fn new(auth: Arc<dyn AuthServiceApi>, security: HttpSecurityConfig) -> Self; }
pub struct AppState { pub db: crate::DbPool, pub auth: Arc<HttpAuthState> }
pub fn build_http_router(state: AppState) -> axum::Router; // 保留 /healthz、/readyz，再加4端点
// 请求体由显式有界读取转换超限为400 INVALID_ARGUMENT；仅检查Content-Length不够。
```

- [ ] **Step 1: RED 测试 + 可编译 stub**（stub：trait/`FakeAuth`/state/`build_http_router` 全就位可编译，四 handler 一律 `err_response(501, "INVALID_STATE", "stub", json!({}))`）：

```rust
struct FakeAuth { st: Mutex<FakeState> } // authz_epoch() -> AuthFuture<'_,u64> yields 7; no process UUID conversion
fn app(fails: u32) -> Router { /* security: hosts ["127.0.0.1:8080"], origin Some("http://127.0.0.1:5173"), trust_tls:false；db=lazy 无效 pool（照抄 lib.rs:69-74）；
    AppState{ db, auth: Arc::new(HttpAuthState::new(Arc::new(FakeAuth::new(fails)), security)) } */ }
// 请求辅助：builder + header("host","127.0.0.1:8080") + header("origin","http://127.0.0.1:5173") + extension(axum::extract::ConnectInfo("10.1.1.1:5000".parse::<std::net::SocketAddr>().unwrap())) + Json body；router.oneshot
#[tokio::test] async fn login_success_contract() // 200；data.user.username=="admin"、must_change_password 是 bool、csrf_token 64hex；Set-Cookie 以 "rsc_session=" 开头含 HttpOnly/SameSite=Strict/Path=/，不含 "Secure"
#[tokio::test] async fn login_rejects_bad_host_or_origin() // 错 host→403；缺 origin→403；错 origin→403（CSRF_INVALID）
#[tokio::test] async fn login_bad_body_and_bad_credentials() // 多余/缺失键→400；非法username(含65位/超长)在触及limiter/Argon2前400且不记录限速键；错误密码→401 INVALID_CREDENTIALS 且 body 不含 username 原文
#[tokio::test] async fn me_requires_valid_session_and_shape() // 无cookie→401；非法cookie→401；有效→200且data.authz_epoch=="7"（fake读取数据库u64）
#[tokio::test] async fn password_requires_csrf_and_clears() // 缺/错 X-CSRF-Token→403；正确→200 data.changed==true 且 Set-Cookie 含 "Max-Age=0"
#[tokio::test] async fn logout_success_and_invalid_session() // 有效+绑定 CSRF→200 data.logged_out==true+清 cookie；无效 cookie→401
#[tokio::test] async fn login_rate_limited_429()         // 同账号同来源 5 次失败后第 6 次→429 RATE_LIMITED + Retry-After 1..=900 秒
#[tokio::test] async fn oversized_body_400_and_probes()  // Content-Length>1MiB及无Content-Length实际body>1MiB均400；healthz→200；readyz→503
#[tokio::test] async fn backend_failure_never_counts_as_bad_password() // FakeAuth Database/Crypto→503，5次后仍非429
#[tokio::test] async fn invalid_csrf_and_revocation_clear_binding() // logout/password成功清绑定、失效鉴权不能补发旧csrf
#[tokio::test] async fn capacity_exhaustion_returns_503_with_retry_after() // csrf4096/limiter键上限，RESOURCE_EXHAUSTED+Retry-After:60；无身份泄漏
```

- [ ] **Step 2: 确认 RED** — 第 3 条命令：上述 11 组用例在可编译 stub 下因响应行为不符 FAIL；先编译通过再统计，不能把示意注释当测试体。
- [ ] **Step 3: GREEN 最小实现**。各 handler 顺序（先头后体，错误即返回，不泄漏内部细节）：

```rust
// login  ① check_pre_session(host,origin)→403 "security.origin_host"  ② body 恰 {username,password} 且非空
//        字符串→400 "auth.login.bad_body"；username 3–64 位 ASCII 小写 [a-z0-9._-]、首字符字母/数字→400 同键
//        ③ `rate.retry_after(now,username,peer.ip())` 为 Result：Exhausted→503 RESOURCE_EXHAUSTED+Retry-After:60，Some(d)→429 RATE_LIMITED+Retry-After（整数秒向上取整1..=900），None→继续；用户名格式及≤64字节必须在此之前检查
//        ④ auth.login Ok 先以OsRng生成csrf=32B hex，容量检查再绑定session.digest；`session_cookie_header(&raw_token,&cfg)` 返回Result，固定错误→503而非使用未验header；仅构造成功后设置Set-Cookie并返回200
//           data={user:user_public,csrf_token}；仅 InvalidArgument（未知账号/错误密码/停用）→`rate.record_failure(now,username,peer.ip())`的Result必须处理：Exhausted→503 RESOURCE_EXHAUSTED+Retry-After:60（不把审计过的失败改报401），Ok→401 INVALID_CREDENTIALS；
//           Database/SchemaNotReady/Config/Crypto→固定503（不计凭据失败、不增加限速次数、不泄错误详情）
// me     cookie须64位小写hex→auth.authenticate→401；csrf缺失才补发并绑定；
//        `auth.authz_epoch().await?` 每请求固定SELECT schema_meta单例计数（u64），data.authz_epoch=epoch.to_string()
// password 会话缺失/失效/被停用仅返回 401 AUTH_REQUIRED（不能 INVALID_CREDENTIALS）；先鉴权→CSRF（403）→恰好两个非空JSON字段（400）→auth.change_password：
//        原密码错误明确未执行返回 400 INVALID_ARGUMENT；Ok→删除CSRF绑定、清cookie、data={changed:true}；RevisionConflict→409；
//        Database/Crypto/Config/SchemaNotReady→固定503（不能伪报密码未变）
// logout   先鉴权（401）→CSRF（403）→body恰{}（400）→auth.logout：
//        Ok→删除CSRF绑定、清cookie、data={logged_out:true}；Database等不确定错误→503，不清cookie也不宣称撤销完成
```
错误映射辅助穷尽 `ControllerError` 的所有变体（`InvalidArgument`400；`RevisionConflict`409；`PermissionDenied`403；`NotFound`404；`Config/SchemaNotReady/Crypto`503 NOT_READY；`Database`503 STORAGE_UNAVAILABLE），绝不 stringify 原始 SQLx/Config 文本。登录/鉴权仅 `InvalidArgument` 映射固定401；数据库/密码学故障保持503，且不计登录失败限速。

- [ ] **Step 4: 确认 GREEN** — 第 3 条命令全绿（含 Task 1/2 与既有 lib.rs 测试）。
- [ ] **Step 5: 独立审查** — ① 无效/丢失 cookie 路径全部 401，无 handler 用 cookie 之外的头推断 actor；② 登录前检查仅 Host/Origin/JSON，无 session 假设；③ 429 只来自限速器且均带 Retry-After；④ 错误体无 username/密码/token/URL；⑤ cookie 与 Secure 策略与约束 8 一致；⑥ 旧 `build_router` 及其测试零改动。
- [ ] **Step 6: main.rs 生产接线（双门禁）** — 仅当 **(a)** Task 3 的 object-safe `LiveAuth` 能从已就绪 `DbPool` 每请求读取 `schema_meta.authz_epoch:u64`、**(b)** `SqlxIdentityRepository` 的六个 trait 方法（尤其 insert_session/change_password/revoke_session/record_login_failure 四个写方法）全部已有生产实现、写成功/失败审计原子性和锁序通过独立审查，**没有任何 `writes_not_ready` 桩**时执行；缺一则 main.rs 保持 health-only，不得借用 `SessionClock.epoch` 自造数据库计数或将固定拒绝仓储接入路由：

```rust
// main.rs start_if_ready 闭包内（bootstrap_admin 之后）替换 build_router 一行：
let security = rsetup_controller::http_security::HttpSecurityConfig::from_env()?;
let service = rsetup_controller::auth::service::AuthService::new(
    std::sync::Arc::new(rsetup_controller::auth::sqlx_repo::SqlxIdentityRepository::new(db.clone())))?;
let live = rsetup_controller::http_auth::LiveAuth::new(std::sync::Arc::new(service), db.clone());
let state = rsetup_controller::AppState { db: db.clone(),
    auth: std::sync::Arc::new(rsetup_controller::http_auth::HttpAuthState::new(std::sync::Arc::new(live), security)) };
let listener = tokio::net::TcpListener::bind(&config.listen_address).await?;
axum::serve(listener, rsetup_controller::build_http_router(state)
    .into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;
```

**独立审查（接线）**：① 接线前后第 3 条命令均全绿；② 未改 `auth/service.rs`、`sqlx_repo.rs`、`db.rs`；③ 零新依赖、`--locked` 不失败。

---

## 验收与后续门禁

完成 = 三任务独立审查通过 + 第 3 条命令全绿（含既有 health/readyz 不回归）；产物**零新依赖、零真实 DB/secret/网络、未写 auth 端点以外的业务路由**。
后续（不在本计划）：(a) 仓储计划落地且 `LiveAuth` 真实读取 DB 授权 epoch 后才能完成 Step 6 接线；(b) 非 auth 端点、`PASSWORD_CHANGE_REQUIRED` 白名单中间件、批量/幂等属 02 数据 API 计划；(c) 前端以已审 01/02 规格与后端真实响应测试为合同联调（`apps/controller-web/**` 由 web 计划负责）。

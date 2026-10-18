# Controller Prometheus 指标端点设计规范（2026-10-19）

日期：2026-10-19。分支：`dev-aghost`。作用域：`crates/rsetup-controller`。

> **状态与效力**：本文是 `rsetup-controller` Prometheus 指标端点的**架构与契约规范**，架构方向已经对话逐节对齐确认，本文是实现与验收的唯一依据。本文只新增规范，**不修改**任何既有规范（`docs/superpowers/specs/2026-09-23-controller-v1-0{1..6}*`、`docs/protocol_spec.md`）、不改动源码、不授权数据库迁移。本文及其后续提交**不得包含任何真实凭据**；文中出现的 `prom_admin` / `secure_password` 一律是形状示例，不是可用凭据（本地开发授权与凭据保护规则见仓库根 `AGENTS.md`）。

约定用词：**必须（MUST）/ 不得（MUST NOT）** 是硬门禁，违反即验收不通过；**应当（SHOULD）** 是默认做法，偏离须在代码注释与本规范变更记录中说明；**可以（MAY）** 是允许项。

---

## 1. 背景与目标

### 1.1 现状

- 中控进程当前只有两条探活路由 `GET /healthz`、`GET /readyz`（`crates/rsetup-controller/src/lib.rs` 的 `build_router`，由 `http_auth::build_http_router` 并入认证线），没有任何可被监控系统拉取的状态/性能指标端点。
- 五线（auth / management / tasks / status / web）在 `production::build_production_router` 处唯一装配，对外只有一个监听端口（`ControllerConfig::listen_address`，默认 `127.0.0.1:8080`）。
- 配置只有环境变量一条通道：`ControllerConfig::from_env()` 读 `CONTROLLER_DATABASE_URL`、`CONTROLLER_LISTEN_ADDRESS`；`HttpSecurityConfig::from_env()` 读 `CONTROLLER_ALLOWED_HOSTS`、`CONTROLLER_ALLOWED_ORIGIN`、`CONTROLLER_TRUST_TLS`。没有配置文件机制，也没有 `Cargo.lock` 之外的任何 TOML 依赖。
- 认证边界只有面向浏览器的会话 Cookie（`rsc_session`）+ `X-CSRF-Token` + Host/Origin 精确白名单 + 会话限速（`http_gate::authenticate_read`/`authenticate_write`，按 `EndpointId` 变体逐端点建限速桶）。机器对机器的抓取方（Prometheus）不发送 `Origin`、不持有会话，**无法**复用这条边界。

### 1.2 目标

| 编号 | 目标 | 完成判据 |
| --- | --- | --- |
| G1 | 为 `rsetup-controller` 引入 Prometheus 文本格式的拉取式指标端点，与业务路由共用同一端口、同一进程 | `GET <endpoint>` 返回 `200` 与 `text/plain; version=0.0.4; charset=utf-8`，含 §5.2 全部指标族 |
| G2 | 端点访问由 HTTP Basic Auth 保护，凭据来自 TOML 配置文件（`CONTROLLER_CONFIG` 指定路径，缺省 `controller.toml`），并可用环境变量成对覆盖 | §3 加载顺序与 §4 认证门禁逐条有测试覆盖 |
| G3 | 安全启动门禁（fail-closed）：`enabled = true` 而凭据不完整/非法时，进程在触达监听套接字之前拒绝启动 | §3.6 的每一条错误分支都有单元测试断言错误消息为固定去敏文案 |
| G4 | 指标只反映进程与数据库的**真实**取值：内存原子量 + 抓取时点的轻量 DB 汇总，任何取不到的数据一律使本次抓取失败，绝不输出伪造 0 或降级 200 | §5.5 失败语义 + §7.4/§7.5 的 fail-closed 测试 |
| G5 | 指标端点的认证边界与会话边界完全隔离：不读 Cookie、不建会话、不占 CSRF/限速桶、不写审计 | §4.1 不变量 + §7.6 的隔离测试 |

### 1.3 非目标（本期明确不做）

1. **不做远程推送**：不引入 Pushgateway、不实现远程写（remote write）、不做 Agent API、不做 exemplar/OTLP 导出。只提供标准 **Pull** 拉取接口，由 Prometheus `scrape_configs` 主动抓取。
2. 不引入指标 SDK/注册中心（`prometheus`、`metrics`、`once_cell` 等 crate），文本渲染由本模块自行实现（§5.1）。
3. 不做多用户/用户组/RBAC：Basic Auth 只有**一组**只读凭据，不复用 Identity 用户表，不新增权限点。
4. 不做 Bearer Token、mTLS、IP 白名单、签名 URL，也不提供"免认证公开指标"选项（`type` 只接受 `"basic"`，见 §3.5）。
5. 不新增监听端口、不做 TLS 终止、不改 `listen_address` 语义；`/metrics` 与业务路由同端口。
6. 不做指标缓存、采样聚合、直方图/摘要、增量抓取（`Accept-Encoding: zstd`/`Accept: application/openmetrics-text`）、推送限流、抓取历史。
7. 不做配置热加载：TOML 只在进程启动时读取一次，改配置必须重启进程（避免运行期凭据轮换与路由路径漂移的不确定状态）。
8. 不把 TOML 变成全量配置载体：本期该文件**只承载 `[metrics]` 段**，既有五个环境变量继续按原样生效（§3.3 与 §10.3）。
9. 不改协议层（`rsetup-protocol`）、板端 agent、Web 前端与数据库 DDL；不新增表、列、索引或迁移。
10. 不在本规范内交付告警规则、Grafana 面板与部署清单。

---

## 2. 规范用语与全局不变量

以下不变量贯穿各章，任何一条不成立即视为实现缺陷：

- **I1 单一端口**：指标路由必须经 `production::build_production_router` 合并进唯一顶层 router，不得在 `main.rs` 或任何测试之外另行 `axum::serve`。
- **I2 单一连接池**：全进程只有 `DbPool::connect` 一处构造连接池；指标线持有的必须是同一 `DbPool` 的 `Clone`（内部 `Arc` 共享），不得自建第二个池。
- **I3 边界隔离**：指标线的任何类型都不得包含 `Arc<HttpAuthState>`、`Session` 或 `EndpointId`；指标 handler 不得调用 `http_gate` 的认证函数，也不得向 `HttpAuthState` 的 session/CSRF/限速结构写入任何内容。
- **I4 失败关闭**：配置非法、凭据缺失、采集失败一律 fail-closed（启动失败或 5xx），不得降级为"看起来正常"的空指标或常量 0。
- **I5 去敏**：所有错误消息、日志、指标文本与测试报告都不得回显密码、base64 凭据原文或配置文件的行原文。
- **I6 不污染业务契约**：不新增 `EndpointId` 变体、不新增 permission、不写 `audit_events`（抓取是高频只读行为，写审计会造成写入放大并污染安全审计的可读性）。

---

## 3. 配置层架构（Configuration Architecture）

### 3.1 依赖引入

在**工作区根** `Cargo.toml` 的 `[workspace.dependencies]` 增加两行，并在 `crates/rsetup-controller/Cargo.toml` 的 `[dependencies]` 以 `workspace = true` 引用：

```toml
# Cargo.toml（工作区根，[workspace.dependencies]）
toml = "0.8"
base64 = "0.22"
```

```toml
# crates/rsetup-controller/Cargo.toml（[dependencies]）
toml.workspace = true
base64.workspace = true
```

约束与说明：

1. `toml` 只用于解析配置文件文本；`base64` 只用于 §4.2 的 `Authorization: Basic` 解码。二者**不得**用于其他用途。
2. `base64 = "0.22"` 与 `Cargo.lock` 中已锁定的 `base64 0.22.1`（当前为传递依赖）同版本，升级为直接依赖不引入新树；`toml 0.8` 是**首次**进入本工作区，首次构建需在有网络/有 vendor 的环境更新 `Cargo.lock`，并把新增 crate 记入构建说明。
3. 只使用 `toml` 的 `from_str` 反序列化能力，不得启用 `preserve_order` 等 feature；`base64` 只使用 `general_purpose::STANDARD`（要求 padding、拒绝尾部比特残留、不容忍空白），不得使用 `STANDARD_NO_PAD` 或自定义宽松 `GeneralPurposeConfig`。
4. 不引入 `subtle`、`ring`、`sha2` 之外的新密码学依赖：`sha2` 已是本 crate 直接依赖（`tasks/http.rs`、`auth/session.rs` 已在用）。

### 3.2 TOML 结构与 Rust 类型

配置文件结构（键顺序即文档示例顺序；键缺失按下表默认值处理）：

```toml
[metrics]
enabled = true
endpoint = "/metrics"

[metrics.auth]
type = "basic"
username = "prom_admin"
password = "secure_password"
```

Rust 结构分三层，**严格区分"文件里的原始形状"与"校验后的终态"**：

```rust
// metrics/config.rs

/// 文件原始形状：整份 TOML 文档。除 `[metrics]` 外的任何顶层表都不被接受。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsFile {
    pub metrics: Option<MetricsSection>,
}

/// `[metrics]` 段。`enabled` 用 `Option` 承载"是否显式声明"，缺省策略在 `from_parts` 裁决。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    pub enabled: Option<bool>,
    pub endpoint: Option<String>,
    pub auth: Option<MetricsAuthSection>,
}

/// `[metrics.auth]` 段：`type` 必须显式给出，不给默认协议。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsAuthSection {
    #[serde(rename = "type")]
    pub kind: String,
    pub username: String,
    pub password: String,
}

/// 校验后的终态：只有这一种形态能离开 config.rs。
/// 不派生 `Debug`（防凭据经 `{:?}` 泄出）也不派生 `PartialEq`（防用短路 `==` 比较凭据）；
/// 断言由测试逐字段进行（§7.2）。
#[derive(Clone)]
pub struct MetricsConfig {
    pub enabled: bool,
    /// 已校验的绝对路径形式端点，恒以 `/` 开头且无尾斜杠。
    pub endpoint: String,
    /// `Some` 当且仅当 `enabled == true`；禁用态不保留凭据副本。
    pub credentials: Option<BasicCredentials>,
}

/// 同样不派生 `Debug`/`PartialEq`。
#[derive(Clone)]
pub struct BasicCredentials {
    pub username: String,
    pub password: String,
}
```

公开常量（供 `auth.rs`、测试与文档共用，避免字面量漂移）：

```rust
pub const DEFAULT_ENDPOINT: &str = "/metrics";
pub const CONFIG_PATH_ENV: &str = "CONTROLLER_CONFIG";
pub const DEFAULT_CONFIG_PATH: &str = "controller.toml";
pub const AUTH_USER_ENV: &str = "PROMETHEUS_AUTH_USER";
pub const AUTH_PASSWORD_ENV: &str = "PROMETHEUS_AUTH_PASSWORD";
/// Basic `realm`，参与 `WWW-Authenticate`，必须与本字符串逐字节一致。
pub const REALM: &str = "rsetup-controller-metrics";
```

### 3.3 加载顺序与优先级

固定三段式：**文件路径解析 → 文件解析 → 环境变量成对覆盖 → 统一校验与门禁**。

```rust
impl MetricsConfig {
    /// 纯函数核心：文件形状 + 环境变量覆盖对 → 终态配置。
    /// 全部校验与 fail-closed 门禁集中在这里，保证可用 `from_parts` 表驱动测试。
    pub fn from_parts(
        file: Option<MetricsFile>,
        override_pair: Option<(&str, &str)>,
    ) -> Result<Self, ControllerError>;

    /// 生产入口：解析路径 → 读文件 → 读两个环境变量 → 委托 `from_parts`。
    /// 不修改进程环境，不回显任何取值。
    pub fn from_env() -> Result<Self, ControllerError>;

    /// 注入式内核：环境变量读取闭包 + 文件读取闭包 → 终态配置。`from_env()` 只是
    /// 把真实 `std::env::var` 与 `std::fs::read_to_string` 接进去的薄封装。
    /// 路径解析与覆盖语义的全部测试通过该闭包注入，MUST NOT 使用
    /// `std::env::set_var`（edition 2024 下为 `unsafe`，且会跨测试污染进程环境）。
    pub fn from_parts_with(
        env: &dyn Fn(&str) -> Option<String>,
        read_file: &dyn Fn(&std::path::Path) -> Result<String, std::io::Error>,
    ) -> Result<Self, ControllerError>;

    /// 纯函数：TOML 文本 → 文件形状。
    pub fn parse_file(text: &str) -> Result<MetricsFile, ControllerError>;
}
```

`from_env()` 的确定步骤：

1. **路径解析**：取 `CONTROLLER_CONFIG`（空串按未设置处理，与 `HttpSecurityConfig::from_values` 的 `filter(|v| !v.is_empty())` 惯例一致）。
   - 已设置 → 该路径为**权威路径**，文件必须存在且可读；不存在/不可读即 `Err`（显式指定即表达"必须从这里加载"的意图，静默回落会造成"以为改了其实没改"）。
   - 未设置 → 使用相对路径 `controller.toml`（相对进程工作目录）。该文件**不存在** → 视为"无配置文件"，`file = None`，继续第 3 步（部署可以完全不用 TOML，仅用环境变量）。文件存在但不可读/不可解析 → `Err`。
2. **文件解析**：读取整个文件为 UTF-8 文本（非 UTF-8 → `Err`），交给 `parse_file`。
3. **环境变量覆盖（成对、原子）**：读取 `PROMETHEUS_AUTH_USER`、`PROMETHEUS_AUTH_PASSWORD`，各自空串按未设置处理。
   - 两者皆未设置 → 不覆盖。
   - 两者皆已设置 → 覆盖文件中的 `username`/`password`（`override_pair = Some((user, password))`）。
   - **只设置其一 → `Err`（固定消息 `"metrics auth override must set both user and password"`）**。禁止把文件里的 username 与环境里的 password 拼成一半新一半旧的凭据：这类混搭会让轮换出现不可预期的组合，且排障时无法从任一来源还原实际生效值。
   - 覆盖发生在 `enabled` 门禁**之前**，因此"文件里禁用 + 环境变量给凭据"合法（凭据被丢弃），"文件里启用 + 环境变量补凭据"合法。
4. **校验与门禁**：`from_parts` 按 §3.4、§3.5、§3.6 顺序校验，任一失败返回 `ControllerError::Config(固定去敏消息)`。

优先级结论（后者覆盖前者）：`内置默认值` → `controller.toml`（或 `CONTROLLER_CONFIG` 指定文件） → `PROMETHEUS_AUTH_USER`/`PROMETHEUS_AUTH_PASSWORD`。`enabled` 与 `endpoint` **没有**环境变量覆盖通道，只能来自文件或内置默认值（避免用环境变量悄悄打开一个未配置凭据的端点）。

`MetricsConfig::from_env()` 的调用点必须在 `ControllerConfig::from_env()` 之后、`DbPool::connect()` 之前（§6.4），保证配置门禁失败时进程没有产生任何数据库连接或监听套接字副作用。

### 3.4 缺省值与取值校验

| 键 | 缺省 | 合法取值 | 违规后果 |
| --- | --- | --- | --- |
| 顶层未知表 | — | 只允许 `[metrics]` | `Err`（`deny_unknown_fields`） |
| `[metrics]` 整段缺失 | `enabled=false`、`endpoint="/metrics"`、无 `auth` | — | 合法（端点注册但恒 404，见 §4.5） |
| `[metrics]` 存在但缺 `enabled` | 无缺省 | `true`/`false` 必须显式 | `Err`：`"metrics.enabled is required"`（表已存在即必须显式声明意图） |
| `endpoint` | `"/metrics"` | `len ≤ 128`；首字符 `/`；仅由 `/` 分隔的非空 ASCII 小写段组成（`[a-z0-9_-]+`）；不得以 `/` 结尾；不得含 `?`/`#`/`%`/空白/`..` | `Err`：`"metrics.endpoint is invalid"` |
| `endpoint` 与既有路由冲突 | — | 不得等于/落在 §6.5 的保留路径集合内 | `Err`：`"metrics.endpoint collides with an existing route"` |
| `[metrics.auth].type` | 无缺省 | 恰好 `"basic"`（区分大小写，不做 trim） | `Err`：`"metrics.auth.type must be \"basic\""` |
| `[metrics.auth].username` | 无缺省 | 非空；不含 `:`；不含 `< 0x20` 或 `0x7F` 控制字符 | `Err`：`"metrics.auth.username is invalid"` |
| `[metrics.auth].password` | 无缺省 | 非空；不含 `< 0x20` 或 `0x7F` 控制字符 | `Err`：`"metrics.auth.password is invalid"` |

统一规则：

1. **不做任何归一化**：不 trim、不 case folding、不做 Unicode 规范化。配置文件里的字节就是比对用的字节（凭据语义由运维负责，实现不"猜测"意图）。
2. `username` 禁止 `:` 是 RFC 7617 §2「userid 不得包含冒号」的前移校验：配置期就把不可用凭据挡在启动门禁，而不是让抓取方在线上收到无解的 401。
3. **未知键 fail-closed**（`deny_unknown_fields`）：拼错的键（如 `passwaerd`、`endpont`）会静默变成缺省值，是监控可用性最典型的隐性故障，因此一律拒绝启动而不是忽略。
4. **密码最小长度不强制**：强制长度会破坏既有秘密轮换，本期只在 §8 风险表登记并给出部署建议（建议 ≥ 20 字符、由 CSPRNG 生成）。
5. TOML 解析错误必须转换为其消息**只含行列号**的固定形状：`ControllerError::Config(format!("metrics config: invalid TOML syntax at line {line}"))`。不得复用 `toml::de::Error` 的 `Display`，因为其消息可能包含该行原文（潜在秘密），违反 I5。

### 3.5 认证类型门禁

`type` 只接受 `"basic"`。`"none"`、`"bearer"`、`"token"`、空串及任何未知值一律 `Err` 并拒绝启动，不回落到"免认证"或"默认 basic"。免认证公开指标在本期是不可用能力，而不是可用但需自行加固的能力。

### 3.6 安全启动门禁（Fail-Closed）

终态裁决按顺序执行（三条分支的触发条件互斥，不重叠）：

1. `enabled == false` → 丢弃 `[metrics.auth]`（若存在），终态 `credentials = None`，不校验凭据完整性（合法配置：`enabled = false` 且不写 auth）。
2. `enabled == true` 且文件与环境变量**都没有提供** `[metrics.auth]` → `Err`：`"metrics.auth is required when metrics.enabled is true"`。
3. `enabled == true` 且 `[metrics.auth]` 存在，但 `username`/`password` 为空或含非法字符 → 由 §3.4 的字段校验先拦截，返回更具体的 `"metrics.auth.username is invalid"` 或 `"metrics.auth.password is invalid"`（同一条件只有一个消息，不重复报告）。
4. 全部通过 → `credentials = Some(BasicCredentials)`。

空凭据的可达路径唯一：`[metrics.auth]` 里写了空串，命中分支 3。环境变量侧的空串按"未设置"处理（§3.3 第 3 步），因此要么整个凭据缺失（分支 2），要么在 §3.3 第 3 步就被 partial 拒绝；不会出现第三种解释，也不存在同一条件对应两个消息的情形。

第 2、3 条即需求的核心安全门禁：**启用指标但没有完整凭据，服务在启动期直接拒绝启动**，错误经 `main()` 的 `?` 上抛，进程不绑定端口。所有上述固定消息 MUST NOT 含用户名、密码、路径或文件内容；测试以断言"`err.message()` 不含配置里写入的密码字面量"（§7.2）反向保证。

### 3.7 配置文件的安全处理

1. 文件权限建议 `0600`、属主为服务账户；本模块不 `stat` 既有文件的权限位，也不因权限位不符合预期而拒绝启动（配置文件由部署方创建，与 `bootstrap` 首次落盘初始密码时用 `create_new` + `mode(0o600)` 主动设定权限的场合不同，此处没有可创建动作可加固）。
2. 凭据只存在进程内存（`MetricsConfig` → `Arc<MetricsConfig>`）；MUST NOT 写入数据库、`audit_events`、任何日志 target、`Data`/`Debug` 输出。`BasicCredentials` **不得** `derive(Debug)`（`MetricsConfig` 可以 `Debug`，因为其 `credentials` 字段类型不实现 `Debug`）。
3. 仓库跟踪范围与 `secret/` 规则不变：真实 TOML 配置文件不得提交，示例文件（若实现阶段需要交付）必须使用 `prom_admin` / `secure_password` 这类明显的占位形状并在文件头注明不可直接使用。

---

## 4. 安全与认证门禁（Security & Authentication Gate）

### 4.1 边界归属与隔离不变量

指标端点是**独立认证边界**，与业务会话边界互不干涉：

| 维度 | 业务线（auth/management/tasks/status） | 指标线 |
| --- | --- | --- |
| 凭据 | `rsc_session` 会话 Cookie（64 位小写 hex） | `Authorization: Basic <base64>` |
| 权威校验 | `http_gate::authenticate_read`/`authenticate_write` | `metrics::auth`（§4.2/§4.3） |
| Host/Origin 精确白名单 | 是 | **否**（见下） |
| CSRF（`X-CSRF-Token`） | 写请求必须 | 不适用（只读 GET） |
| 限速 | 读 50/s、写 20/s 每会话 | 不参与会话限速；抓取并发由 §5.5 信号量约束 |
| 会话/CSRF 表写入 | 是 | 零写入 |
| 审计 | 管理写事务内审计 | 不写 |
| `Set-Cookie` | 登录/登出 | 任何响应都 MUST NOT 携带 |

**不套用 `HttpSecurityConfig` 的 Host/Origin 白名单是刻意决策**，不是遗漏：`http_gate::authenticate_read` 的第一步就是把 `Host` 与 `HttpSecurityConfig::allowed_hosts` 精确比对，不在白名单一律 403（`origin_host_invalid`），而 Prometheus 抓取请求的 `Host` 是抓取目标地址（可能与 `CONTROLLER_ALLOWED_HOSTS` 中登记的浏览器可达地址不同），且监控客户端不发送 `Origin`。若强行套用会话边界，跨主机抓取会恒 403，而为了让抓取通过而放宽 `ALLOWED_HOSTS` 又会削弱浏览器侧防护——两个安全域耦合是净损失。指标线的保护由 Basic Auth + §8 部署约束（默认监听 `127.0.0.1:8080`，对外暴露交由反向代理/防火墙并在其上另配 TLS 与来源限制）承担。

类型层面的结构性隔离（I3 的可编译表达）：`MetricsState`、`MetricsConfig`、handler 签名中都不存在 `Arc<HttpAuthState>`/`Session`/`EndpointId`，因此指标线**无法**触碰会话上下文；§7.6 用行为测试（携带完全合法会话 Cookie 仍得 401）反向验证。

### 4.2 `Authorization` 头解析

固定步骤，任何一步失败 → 401（同一个响应形状，不做区分）。步骤 1–6 由公开纯函数 `pub fn parse_authorization(headers: &HeaderMap) -> Option<(&str, &str)>` 承载（L1 单测直接调用它，不依赖 HTTP 栈）：

1. `single_header(headers, header::AUTHORIZATION)`：缺失、出现多次（任意多于一个值）、非 ASCII 值一律 `None`。复用 `http_gate` 中同名 helper 的**语义**（"重复头绝不简化为取第一个"）；因该 helper 当前是 `pub(crate)`，指标线可直接调用（同 crate 内），MUST NOT 复制一份实现。
2. 取 `&str`，按第一个空格切分：`scheme` 与 `rest`。要求两段都存在且 `rest` 非空。
3. `scheme` 必须 ASCII 大小写不敏感等于 `basic`（RFC 7617 §2.2 scheme 不区分大小写）。`rest` 内部 MUST NOT 含空白、换行或前导 `=`。
4. `base64::engine::general_purpose::STANDARD.decode(rest)`：解码失败（含 padding 缺失、尾部比特残留、非法字符）→ `None`。
5. 解码结果按 **UTF-8** 严格解码，失败 → `None`。
6. 按**第一个** `:` 切分 `userid:password`；无 `:`、`userid` 为空、`password` 段中仍含 `:` → `None`（配置期已禁止 username 含冒号，这里同步拒绝这类请求，避免歧义解析）。
7. 得到 `(user, password)` 后进入 §4.3 校验；**校验值之外的一切解析失败都不区分错误码**，统一 401 且响应体为空。

### 4.3 常量时间凭据比较

```rust
// metrics/auth.rs（签名固定，便于单测直接调用）
pub fn verify(user: &str, password: &str, expected: &BasicCredentials) -> bool;
fn sha256(bytes: &[u8]) -> [u8; 32];
fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool;
```

实现与语义约束：

1. `verify` 先比较**用户名**、再比较**密码**，两次比较**都必须执行**，结果以按位与合并：

   ```rust
   let ok_user = ct_eq_32(&sha256(user.as_bytes()), &sha256(&expected.username));
   let ok_password = ct_eq_32(&sha256(password.as_bytes()), &sha256(&expected.password));
   ok_user & ok_password
   ```

   MUST NOT 写成 `if !user_ok { return false; } check_password()` 形式：短路会在用户名上建立"用户名是否正确"的时序判定器。
2. `ct_eq_32` 为固定长度 32 字节的无短路折叠比较，形态与本仓库既有实现一致（`auth/session.rs` 的 `constant_time_eq_32`、`tasks/http.rs` 的 `constant_time_eq`）：

   ```rust
   let mut diff = 0u8;
   for i in 0..32 {
       diff |= a[i] ^ b[i];
   }
   diff == 0
   ```

   MUST NOT 使用 `==`、`starts_with`、`slice::eq`、`str::contains` 等可短路/可提前的比较。
3. **先哈希后比较**的目的：把输入长度归一为固定 32 字节，从而消除"长度不同提前返回"与逐字节短路两类长度/内容侧信道。这与 `constant_time_eq` 现有注释「长度已由调用方固定」的前提一致。
4. 诚实登记残余泄漏面（不假装成完美常量时间）：哈希本身的耗时随输入长度线性增长；§4.2 的 base64/UTF-8 解析失败会在凭据比较之前提前返回 401（提前量只与"提交的是否是合法 base64 形状"有关，与凭据内容无关）。二者的可利用性远低于网络抖动，属可接受残余，记录在 §8。
5. `verify` MUST NOT 记录日志、MUST NOT 返回值区分"用户名错"与"密码错"。

### 4.4 401 响应构造

- 状态码 `401 Unauthorized`。
- `WWW-Authenticate: Basic realm="rsetup-controller-metrics"`（逐字节固定；不携带 `charset` 参数——Prometheus 官方 `basic_auth` 按 UTF-8 发凭据，不带参数是最大兼容选择）。
- **空响应体**、无 `Content-Type`。MUST NOT 复用 `http_api::err_response` 的 JSON 信封：指标线是独立边界，向监控客户端吐业务错误信封既无必要也会诱导前端误用该端点。
- MUST 携带 `Cache-Control: no-store` 与 `X-Content-Type-Options: nosniff`（与 200/503 一致，便于测试固定响应头集合）。

### 4.5 状态码与拦截矩阵

| 条件 | 状态码 | 响应体/头 | 是否读凭据 | 是否触达数据源 |
| --- | --- | --- | --- | --- |
| `enabled = false` | `404 Not Found` | 空体、无 `Content-Type`、无 `WWW-Authenticate`（与路由未注册不可区分，不宣告端点存在） | 否 | 否 |
| `enabled = true`、缺 `Authorization` | `401` | §4.4 | 是（解析失败） | 否 |
| `enabled = true`、scheme/encoding/UTF-8/冒号非法 | `401` | §4.4 | 是 | 否 |
| `enabled = true`、凭据不匹配 | `401` | §4.4 | 是（两次比较都执行） | 否 |
| 合法凭据、采集成功 | `200` | §5.1 | 是 | 是 |
| 合法凭据、采集失败/超时/并发闸满/渲染超上限 | `503` | 空体 + `Cache-Control: no-store` + `Retry-After: 5` | 是 | 是（已失败） |
| 非 GET 方法 | `405` | 由 Axum 方法匹配给出（禁用态亦如此，见 §8 残余风险） | 否 | 否 |
| 其他路径 | `404` | 顶层 router 默认未匹配响应 | 否 | 否 |

handler 内部顺序 MUST 固定为：①`enabled` 门禁 → ②Basic 认证 → ③并发闸 `try_acquire` → ④快照采集（含超时）→ ⑤渲染与上限校验 → ⑥响应。（HTTP 方法匹配由路由层在此之前完成。）先判 `enabled` 再判认证，保证禁用态无论是否携带凭据都恒 404。

### 4.6 认证失败不引入进程内限速（本期决策）

指标凭据是长期静态凭据，猜测成本由密码长度承担；本期**不**在进程内为 Basic Auth 失败实现类似登录的滑动窗口限速，避免与 `LoginRateLimiter` 共享状态造成边界混淆，也避免引入新的内存上限与 fail-closed 分支。防护依赖：监听地址默认回环、部署层的连接来源限制与失败封禁、以及强随机密码。此项登记为 §8 的已知残余风险；若后续实现，必须另立规范明确键维度（来源 IP）、内存上限与耗尽语义。

---

## 5. 指标模型与 Prometheus 文本格式（Metrics Data Model）

### 5.1 输出格式契约

1. `Content-Type: text/plain; version=0.0.4; charset=utf-8`，常量 `metrics::render::CONTENT_TYPE`，逐字节固定（含 `version=0.0.4` 前后的分号与单空格）。
2. 文本结构：每个指标族依次输出 `# HELP <name> <text>`、`# TYPE <name> <type>`、若干样本行；族之间不插入空行；整个响应体以单个 `\n` 结尾，末尾不得有多余空行。
3. **族顺序与样本顺序固定**（渲染为纯函数，输出逐字节确定，供 golden 测试钉住）：
   1. `rsetup_controller_build_info`
   2. `rsetup_controller_uptime_seconds`
   3. `rsetup_controller_devices_total`（标签顺序 `APPROVED`、`PENDING`、`REJECTED`）
   4. `rsetup_controller_tasks_total`（标签顺序 `pending`、`running`、`succeeded`、`failed`）
   5. `rsetup_controller_db_pool_connections`（标签顺序 `active`、`idle`）
4. **序列集合恒定为 11 条**（§5.2 各族标签全展开，计数为 0 也 MUST 输出该样本）。缺序列会让 PromQL 出现空洞、`absent()` 误报，也让"求和 = 表行数"这一不变量无法在零值时验证。
5. 数值一律无符号十进制整数字面量（`u64`）；不使用科学计数法、`NaN`、`+Inf`。`build_info` 恒为 `1`。
6. 标签值转义（本模块唯一一处）：`\` → `\\`、`"` → `\"`、`\n` → `\n`、`\r` → `\r`、`\t` → `\t`。当前所有标签值都来自 §5.2 的固定词汇与 §5.2.1 的构建常量，实现仍 MUST 经过转义函数，不得依赖"输入总是安全的"。
7. `HELP` 文本为 ASCII 英文、单行、不含换行；`TYPE` 一律 `gauge`（§5.4 的命名偏差说明）。
8. 响应头：`Content-Type`（上述常量）、`Cache-Control: no-store`、`X-Content-Type-Options: nosniff`。MUST NOT 携带 `Set-Cookie`、`ETag`、`Vary`（本端点无内容协商）；本期不做压缩与分块协商。
9. **大小上限**：`pub fn render(values: &ScrapeValues) -> Result<String, ControllerError>` 在返回前校验长度，超过 `pub const MAX_BODY_BYTES: usize = 65_536` 字节即返回 `ControllerError::Config("metrics exposition exceeds size limit")`（渲染层内部 fail-closed，L1 单测可直接断言 `Err`；handler 只需把它映射成 503）。序列上限 11 条时实际 < 1 KiB，该上限只是防御性护栏，防止将来扩展族时无界输出直出监控端。
10. 本模块自行渲染，不引入 SDK：MUST NOT 依赖任何外部注册中心的隐式指标（进程 CPU、GC、请求计数等），只输出 §5.2 清单。

### 5.2 核心指标清单

| 指标名 | 类型 | 标签 | 取值来源 | 语义 |
| --- | --- | --- | --- | --- |
| `rsetup_controller_build_info` | Gauge | `version`、`edition` | 编译期常量 | 构建标识，值恒 `1`。用于按版本分组与"该实例是否已被目标版本替换"的判定。 |
| `rsetup_controller_uptime_seconds` | Gauge | — | 进程内单调时钟 | 进程启动至今的**整秒**数。单调锚点计算，宿主墙钟前跳/回拨都不得使其倒退或清零。归零即进程重启。 |
| `rsetup_controller_devices_total` | Gauge | `admission="APPROVED\|PENDING\|REJECTED"` | 抓取时点 DB 聚合 | 设备准入分布；三条序列之和恒等于 `devices` 表行数。 |
| `rsetup_controller_tasks_total` | Gauge | `status="pending\|running\|succeeded\|failed"` | 抓取时点 DB 聚合 | 主任务状态分布（归一化四桶）；四条序列之和恒等于 `main_tasks` 表行数。 |
| `rsetup_controller_db_pool_connections` | Gauge | `state="active\|idle"` | 连接池内存状态（零网络） | 抓取时点连接池占用；`active` 为借出使用中，`idle` 为池中空闲。 |

#### 5.2.1 类型选择与构建常量

`rsetup_controller_uptime_seconds` 取 **gauge** 而不是 counter：它的值由进程启动锚点与当前单调时钟之差算出，每次抓取输出的是瞬时量；counter 的语义要求从 0 单调累加并由抓取端做 `increase()`/`rate()`，与本指标（以及"归零即重启"的告警用法）不符。五个指标族一律 `# TYPE ... gauge`，本端点不声明任何 counter/histogram/summary。

`version` 取 `env!("CARGO_PKG_VERSION")`（由 `[workspace.package] version = "0.5.0"` 继承；撰写本文时取值为 `"0.5.0"`，示例与 golden 测试均用该值，但**测试断言不硬编码版本号**，改断言"等于 `CARGO_PKG_VERSION` 且为非空 ASCII"，避免版本 bump 造成假失败）。

`edition` 无对应 `CARGO_PKG_*` 环境变量，因此在 `metrics/mod.rs` 显式声明常量并与清单做一致性测试：

```rust
/// Rust edition 标签值。Cargo 不暴露 `CARGO_PKG_EDITION`，故显式声明，
/// 由 metrics_edition_matches_workspace_manifest 与根清单 [workspace.package] 对齐。
pub const EDITION: &str = "2024";
```

单元测试 `metrics_edition_matches_workspace_manifest` 用 `include_str!("../../../../Cargo.toml")`（相对 `src/metrics/mod.rs`，即仓库根清单）定位 `[workspace.package]` 段内 `edition = "..."` 行，断言其值与 `EDITION` 相同；清单缺该行或值不同都必须失败。

#### 5.2.2 标签取值域与持久层映射

**设备准入**（`devices.admission_state`，`VARCHAR(16)`，DDL `chk_devices_state` 约束三值）：

| DB `admission_state` | 指标标签 `admission` | 说明 |
| --- | --- | --- |
| `APPROVED` | `APPROVED` | 已准入 |
| `PENDING` | `PENDING` | 待审批 |
| `REVOKED` | `REJECTED` | 未获准入（含先批准后吊销） |

命名映射是刻意选择并在本文留档的偏差：持久层用 `REVOKED` 描述"当前不处于准入态"（`AdmissionState::Revoked`），监控侧统一用 `REJECTED` 表达"未获准入"这一告警语义，使告警规则可以写 `rsetup_controller_devices_total{admission="REJECTED"}` 而不必了解内部 `REVOKED`/`Denied` 的状态机细节。映射 MUST 在 Rust 侧纯函数完成（`fn admission_label(raw: &str) -> Option<AdmissionBucket>`），MUST NOT 依赖 SQL 端的隐式转换。

**主任务状态**（`main_tasks.state` / `main_tasks.outcome`，v4 表按约定不建 CHECK 约束，故 MUST 用显式穷举映射；`state` 取值域按 `2026-09-23-controller-v1-04-task-lifecycle.md` 为 `queued`/`running`/`held`/`completed`，`outcome` 为 `success`/`cancelled`/`unknown`/`partial`/`failed` 或 `NULL`）：

| `state` | `outcome` | 指标标签 `status` |
| --- | --- | --- |
| `queued` | 任意 | `pending` |
| `held` | 任意 | `pending` |
| `running` | 任意 | `running` |
| `completed` | `success` | `succeeded` |
| `completed` | `failed`/`partial`/`cancelled`/`unknown` | `failed` |
| `completed` | `NULL` | `failed`（终态却缺 outcome 属契约外数据；确定为未成功而非整次抓取失败，避免历史脏数据使监控端点不可用） |
| 其他任意字符串 | — | **fail-closed 503**，绝不静默归入任一桶 |

纯函数 `fn task_bucket(state: &str, outcome: Option<&str>) -> Option<TaskBucket>` 承载该表；返回 `None` 时采集器返回 `ControllerError::Config("metrics task aggregate: unknown task state")`（去敏，不回显取值）。

`held`（等待人工 time-review 的暂停态）并入 `pending`：两者都是"尚未在执行中"的非终态，四桶标签由对齐设计固定为 `pending|running|succeeded|failed`，并入可保证四条序列求和恰等于表行数（若另立第五桶则违反 §1.3 的固定标签域）。

### 5.3 采集与查询

两条聚合查询 + 一次内存读取，全部在 `metrics/collect.rs`：

```sql
-- 设备准入分布
SELECT CAST(admission_state AS CHAR) AS bucket, COUNT(*) AS n
FROM devices
GROUP BY admission_state;

-- 主任务状态分布
SELECT CAST(state AS CHAR) AS state, CAST(outcome AS CHAR) AS outcome, COUNT(*) AS n
FROM main_tasks
GROUP BY state, outcome;
```

约束：

1. 两条查询 MUST 在**同一条池连接**上顺序执行（同一连接不保证跨语句快照，因此下面第 3 条明确语义边界），MUST NOT 用 `query!` 宏（本仓库不使用编译期 SQL 宏，保持与 `db.rs`、`tasks/repository.rs` 一致的 `query_as` 运行期写法）。
2. `COUNT(*)` 以 `i64` 接收，再 `u64::try_from`；负值（不可能出现）→ fail-closed 503。`CAST(... AS CHAR)` 与本仓库既有查询写法一致，固定字符串类型不受会话字符集影响。
3. **语义边界**：两条聚合之间不做事务，因此两个族之间允许存在毫秒级偏差。该指标语义是"抓取时点的近似瞬时 gauge"，MUST NOT 为此持有长事务或 `SERIALIZABLE` 快照；如需强一致对账，走 SQL 而非本端点。
4. 连接池 gauge 直接读内存：`active = pool.size().saturating_sub(pool.num_idle() as u32)`、`idle = pool.num_idle()`（`sqlx::Pool::size()` 返回当前打开连接总数、含空闲；`num_idle()` 返回空闲数；两者都以 `u64` 渲染），MUST NOT 为此发起任何连接或查询，两条序列永远可得、永不变错。`state="max"` 本期不输出（`Pool::options().get_max_connections()` 可用，留待后续需要 `饱和度` 告警时另立规范）。
5. `Pool::size()` 为 0（例如尚未建立任何连接的 lazy 池）时输出 `active=0, idle=0`，这是"当前没有任何打开连接"的真实表达，不是采集失败。

### 5.4 命名规范偏差说明

Prometheus 官方命名约定把 `_total` 后缀留给 counter。本设计的 `rsetup_controller_devices_total`、`rsetup_controller_tasks_total` 与对齐设计指定的固定名保留 `_total`，但**类型恒为 gauge**（当前计数快照，不是单调累计量，重启会下降）：

1. 每个族的 `# TYPE ... gauge` 行 MUST 输出，Prometheus 解析器据此按 gauge 建模，不会生成 `rate()` 可用的 counter 语义。
2. 该偏差 MUST 在指标 `HELP` 文本与 §5.2 清单同时留档：`HELP` 明确"sum over all labels equals the table row count"，把"这是一组可求和的分布快照"写进抓取方可见的元数据。
3. 未来若需要真正的累计量（例如抓取次数、认证失败次数），MUST 使用不带 `_total` 冲突语义的新指标名并另立规范。

### 5.5 采集模型、并发与失败语义

- **Pull 模式**：只在请求到达时采集，MUST NOT 启动后台定时采集任务、MUST NOT 缓存上一次响应（同一路径上的两次抓取允许得到不同取值，这是 gauge 的正常语义）。
- **取值装配**：`collect.rs` 把 `MetricsSnapshot`（两条 DB 聚合）+ `MetricsRuntime`（构建常量与单调 uptime）+ 连接池内存读数（`PoolCounts`）合成 `model::ScrapeValues`，再交 `render::render`；`ScrapeValues` 是渲染层唯一的输入形态（§7.4 的 golden 测试直接构造它，不需要 DB）。
- **并发闸**：`Arc<tokio::sync::Semaphore>`，容量 `pub const MAX_CONCURRENT_SCRAPE: usize = 4`（定义在 `metrics/collect.rs`，`http.rs` 与 `production.rs` 统一按 `metrics::collect::MAX_CONCURRENT_SCRAPE` 引用，不重复声明字面量），handler 用 `try_acquire_owned`；已满 → 立即 503（不排队、不占连接），防止 Prometheus 抓取风暴把业务连接池挤占。`tokio` 的 `sync`/`time` feature 已由工作区声明，无需新增 feature。
- **超时**：两条聚合查询整体包在 `tokio::time::timeout(SCRAPE_TIMEOUT, ...)` 内，`pub const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2)`（同处 `collect.rs` 定义）；超时 → 503。该上限小于常见 Prometheus `scrape_timeout`（10s），使失败由本进程先判定，而不是让抓取端观察到悬挂连接。
- **失败语义（G4）**：任一聚合查询报错或超时、映射遇到未知取值、`u64` 转换失败、渲染超上限 → **整次抓取 503、空响应体**。MUST NOT 输出"部分成功 + 缺失族记 0"或 `unknown` 占位：把取不到写成 0 会让 `devices_total` 假装设备消失、让告警静默，是伪造 PASS。
- **数据源可注入**：DB 汇总经 dyn-safe trait 注入，与 `http_device_status::DeviceStatusSnapshotSource` 同一形态，便于进程内 HTTP 测试不连真实库：

  ```rust
  pub type MetricsFuture<'a, T> =
      std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

  pub trait MetricsSnapshotSource: Send + Sync {
      fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot>;
  }

  /// 两条聚合的行级映射结果（终态：全部为 u64 的四/三桶计数）。
  pub struct MetricsSnapshot {
      pub devices: DeviceCounts, // approved, pending, rejected
      pub tasks: TaskCounts,     // pending, running, succeeded, failed
  }

  pub struct SqlxMetricsSnapshotSource { /* db: DbPool */ }
  impl SqlxMetricsSnapshotSource {
      pub fn new(db: DbPool) -> Self;
  }

  pub struct MetricsRuntime {
      pub started_at: std::time::Instant,
      pub version: &'static str,
      pub edition: &'static str,
  }
  impl MetricsRuntime {
      pub fn new() -> Self;                       // started_at = Instant::now()
      pub fn uptime_seconds(&self) -> u64;         // elapsed().as_secs()，饱和于 u64::MAX
  }
  ```

  测试替身（fake source）只能证明 handler、映射与渲染逻辑，MUST NOT 被描述为"真实数据库聚合已验收"（§7.7）。

### 5.6 完整响应示例（golden）

给定 fixture：`version="0.5.0"`、`edition="2024"`、uptime 4213s、设备 approved 7 / pending 2 / revoked 1、主任务 pending 1 / running 2 / succeeded 40 / failed 3、连接池 active 2 / idle 3：

```text
# HELP rsetup_controller_build_info Build information for the rsetup-controller process, always 1.
# TYPE rsetup_controller_build_info gauge
rsetup_controller_build_info{version="0.5.0",edition="2024"} 1
# HELP rsetup_controller_uptime_seconds Seconds elapsed since the controller process started, from the monotonic clock.
# TYPE rsetup_controller_uptime_seconds gauge
rsetup_controller_uptime_seconds 4213
# HELP rsetup_controller_devices_total Devices grouped by admission state; sum over all labels equals the devices table row count.
# TYPE rsetup_controller_devices_total gauge
rsetup_controller_devices_total{admission="APPROVED"} 7
rsetup_controller_devices_total{admission="PENDING"} 2
rsetup_controller_devices_total{admission="REJECTED"} 1
# HELP rsetup_controller_tasks_total Main tasks grouped by normalized status; sum over all labels equals the main_tasks table row count.
# TYPE rsetup_controller_tasks_total gauge
rsetup_controller_tasks_total{status="pending"} 1
rsetup_controller_tasks_total{status="running"} 2
rsetup_controller_tasks_total{status="succeeded"} 40
rsetup_controller_tasks_total{status="failed"} 3
# HELP rsetup_controller_db_pool_connections Database pool connections by state at scrape time, active or idle.
# TYPE rsetup_controller_db_pool_connections gauge
rsetup_controller_db_pool_connections{state="active"} 2
rsetup_controller_db_pool_connections{state="idle"} 3
```

（共 11 条样本，最后一行后恰一个 `\n`。）

---

## 6. 系统装配与路由架构（Routing & System Integration）

### 6.1 模块布局

新增目录 `crates/rsetup-controller/src/metrics/`（与既有 `api/`、`auth/`、`tasks/`、`time/` 的 `mod.rs` 风格一致），并在 `lib.rs` 追加 `pub mod metrics;`：

| 文件 | 职责 | 关键公开项 |
| --- | --- | --- |
| `metrics/mod.rs` | 模块头契约注释、跨文件常量、公开再导出 | `EDITION`、`pub use` config/auth/http 公开类型 |
| `metrics/config.rs` | TOML 形状、路径与覆盖解析、校验与启动门禁（§3） | `MetricsConfig::{from_env, from_parts_with, from_parts, parse_file}`、`BasicCredentials`、`RESERVED_PATHS`、§3.2 常量 |
| `metrics/auth.rs` | Basic 头解析 + 常量时间比较（§4.2/§4.3） | `verify`、`parse_authorization` |
| `metrics/model.rs` | 指标数据模型、标签词汇、纯映射函数（§5.2） | `ScrapeValues`、`DeviceCounts`、`TaskCounts`、`PoolCounts`、`admission_label`、`task_bucket` |
| `metrics/render.rs` | 0.0.4 文本渲染纯函数与格式常量（§5.1） | `CONTENT_TYPE`、`MAX_BODY_BYTES`、`render` |
| `metrics/collect.rs` | 采集：DB 汇总源、连接池内存读取、运行时量、并发闸与超时（§5.3/§5.5） | `MetricsSnapshotSource`、`SqlxMetricsSnapshotSource`、`MetricsRuntime`、`MetricsSnapshot` |
| `metrics/http.rs` | 状态、路由与 handler（§4.5 顺序） | `MetricsState`、`build_metrics_router` |

依赖方向 MUST 单向：`http → {auth, collect, render, config}`、`collect → model`、`render → model`；`config` 与 `model` 不依赖 axum/sqlx 之外的模块，`render`/`model` 为纯函数层（无 I/O、无 async），保证 §7 的表驱动单测不需要 DB 或 HTTP。

### 6.2 HTTP 层

```rust
// metrics/http.rs
#[derive(Clone)]
pub struct MetricsState {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
    pub snapshot: Arc<dyn MetricsSnapshotSource>,
    /// 只用于 §5.3 第 4 条的内存读取（`size()`/`num_idle()`），永不发起查询。
    pub pool: DbPool,
    pub guard: Arc<tokio::sync::Semaphore>,
}

/// 指标线子 router：按配置路径注册恰好 1 条 GET 路由。
pub fn build_metrics_router(state: MetricsState) -> Router<()>;
```

1. 路由注册 `state.config.endpoint` 上的 `get(scrape)`，MUST 始终注册（含 `enabled = false` 时），由 handler 依 §4.5 顺序返回 404；不采用"禁用即不注册"的方式，这样禁用态响应与路径配置项完全无关，也不受后续合并顺序或前端 SPA 白名单演变影响。
2. handler 不接收任何业务 extractor：入参固定 `(State<MetricsState>, HeaderMap)`，MUST NOT 使用 `Path`/`Query`/`Json`，从而不存在"提取器先于门禁短路"的历史问题（`http_device_status` 已为此改用 `Result<Path<_>, PathRejection>`，本端点直接不引入路径参数）。
3. 成功响应由 `(StatusCode::OK, [(CONTENT_TYPE, ...), ("cache-control", ...), ("x-content-type-options", ...)], body)` 构造，除 §5.1 列出的三个头之外不附加任何业务响应头。
4. 本端点 MUST NOT 写 `tracing` 日志（含 401）：抓取的正常节奏会淹没日志，失败已可由 Prometheus 的 `up == 0` 与 `scrape_duration_seconds` 观测。

### 6.3 扩展 `ProductionInputs` 与装配

`ProductionInputs` 增加**一个**字段；`compose_component_routers` 相应扩为六参数（保持"唯一拓扑合并点 + 路径重叠即 panic"的既有不变量，不因新线引入静默覆盖）：

```rust
// production.rs
pub struct ProductionInputs {
    pub auth: AppState,
    pub management: ManagementState,
    pub tasks: HttpTasksState,
    pub status: DeviceStatusState,
    pub web: Router<()>,
    /// 第六线：Prometheus 指标线。刻意不持有 `DbPool` 与 `Arc<HttpAuthState>`
    /// ——池由 `auth.db` 派生，会话边界类型上不可达（I2/I3 的结构性保证）。
    pub metrics: MetricsLineInputs,
}

/// 指标线的装配输入：配置 + 运行时量 + 汇总源（三者都不是新构造点）。
pub struct MetricsLineInputs {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
    pub snapshot: Arc<dyn MetricsSnapshotSource>,
}

pub fn compose_component_routers(
    auth: Router<()>,
    management: Router<()>,
    tasks: Router<()>,
    status: Router<()>,
    metrics: Router<()>,
    web: Router<()>,
) -> Router<()>;
```

`build_production_router` 的改动（指针一致性校验保持原样，只覆盖原四线；指标线无 `HttpAuthState`，不参与也无法违反该校验）：

```rust
pub fn build_production_router(inputs: ProductionInputs) -> Result<Router<()>, ControllerError> {
    if !Arc::ptr_eq(&inputs.auth.auth, &inputs.management.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.tasks.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.status.auth)
    {
        return Err(ControllerError::Config("production auth context mismatch".into()));
    }
    // 连接池唯一派生点：指标线只能拿到 auth 线那个池的 Clone（I2）。
    let metrics_state = MetricsState {
        config: inputs.metrics.config,
        runtime: inputs.metrics.runtime,
        snapshot: inputs.metrics.snapshot,
        pool: inputs.auth.db.clone(),
        guard: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_SCRAPE)),
    };
    let metrics = build_metrics_router(metrics_state);
    let auth = inputs.auth; // 在 metrics 借用之后再移交给认证线
    Ok(compose_component_routers(
        http_auth::build_http_router(auth),
        build_management_router(inputs.management),
        build_tasks_router(inputs.tasks),
        build_device_status_router(inputs.status),
        metrics,
        inputs.web,
    ))
}
```

设计取舍：**不新增第四条指针校验**，而是让指标线的类型里根本没有第二个 `DbPool` 入口——池只能从 `inputs.auth.db` 派生，违规在编译期不可表达，比运行期 `ptr_eq` 检查更强（这是既有"指针一致性硬门禁"在新线上的结构性替代）。

### 6.4 `main.rs` 装配顺序

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ControllerConfig::from_env()?;
    // 指标配置门禁：在连接数据库与绑定端口之前完成（§3.6 fail-closed 无副作用）。
    let metrics_config = MetricsConfig::from_env()?;
    let db = DbPool::connect(&config).await?;
    // …既有 secret 路径与 start_if_ready(check_identity_schema, …) 流程不变…
    let router = build_production_router(production_inputs(&db, metrics_config)?)?;
    // …TcpListener::bind 与 axum::serve 不变…
    Ok(())
}

/// 唯一生产构造点：签名扩展，新增指标线输入。
fn production_inputs(db: &DbPool, metrics: MetricsConfig) -> Result<ProductionInputs, ControllerError> {
    // …cursor secret / 共享 Arc<HttpAuthState> / management / tasks 装配不变…
    Ok(ProductionInputs {
        auth: AppState { db: db.clone(), auth: auth.clone() },
        management,
        tasks,
        status: DeviceStatusState { auth, snapshot: Arc::new(EmptyDeviceStatusSnapshotSource) },
        web: web_router(),
        metrics: MetricsLineInputs {
            config: Arc::new(metrics),
            runtime: Arc::new(MetricsRuntime::new()),
            snapshot: Arc::new(SqlxMetricsSnapshotSource::new(db.clone())),
        },
    })
}
```

`MetricsRuntime::new()` 在 `production_inputs` 内构造，因此 `uptime_seconds` 的零点是**生产装配完成时点**（与进程真正开始服务只差绑端口毫秒）；测试可注入自建 `MetricsRuntime` 以得到确定的 uptime。

上述 `main()` 片段是顺序示意，不逐字复制闭包捕获细节：`start_if_ready` 的启动闭包按既有形态写作 `|| async move { … }` 并把 `metrics_config` 移入，`production_inputs` 仍返回 `Result<ProductionInputs, ControllerError>`，`?` 与错误类型链（`Box<dyn std::error::Error>`）保持现状。

### 6.5 路径保留集合与不冲突约束

`metrics.endpoint` MUST NOT 命中以下保留集合（`metrics/config.rs` 内定义 `pub const RESERVED_PATHS: &[&str]` 与 `fn is_reserved(path: &str) -> bool`，供 §3.4 校验与测试共用）：

- 探活：`/healthz`、`/readyz`；
- 认证与管理/任务/状态 API 前缀：`/api`（含任意子路径）；
- 前端精确页面：`/`、`/login`、`/password`、`/sessions`、`/devices`、`/tasks`、`/admin`、`/assets` 及其任意子路径，以及 SPA 参数路由 `/devices/{id}`、`/tasks/{id}`、`/admin/{*rest}` 对应的字面路径前缀。

保留集合与 `web.rs`（`SPA_EXACT_ROUTES`/`SPA_PARAM_ROUTES`/`ASSET_DIR`）、`http_auth::build_http_router`、`api`/`tasks`/`http_device_status` 的路由表由 §7.6 的一致性测试钉住：漂移时新增路由必须同步登记，否则测试红。配置期拦截冲突而非依赖 Axum 合并期 panic（`Overlapping method route`），因为运维能在启动前看到明确配置错误而不是进程崩溃栈。

---

## 7. 测试验证策略（Testing Strategy）

分层门禁与仓库既有约定一致：先在无 I/O 层拿到行为 RED→GREEN，再做进程内 HTTP 集成，最后做装配与真实库验收；fake 只证明它所代表的那一层。

### 7.1 层次划分

| 层 | 位置 | 证明内容 | 不证明 |
| --- | --- | --- | --- |
| L1 纯逻辑单测 | `metrics/config.rs`、`auth.rs`、`model.rs`、`render.rs` 的 `#[cfg(test)]` | 解析、校验、映射、渲染确定性 | 路由、HTTP 语义 |
| L2 进程内 HTTP | `metrics/http.rs` 的 `#[cfg(test)]`（`tower::ServiceExt::oneshot` + fake source） | 状态码、响应头、认证与禁用门禁、失败语义 | 真实 SQL、真实库数据 |
| L3 装配测试 | `tests/production_composition.rs` 扩展 | 六线合并可达、单一端口、隔离与池派生、路径不冲突 | 认证语义、真实指标取值 |
| L4 真库验收 | `tests/mysql_identity.rs` 风格（`#[ignore]`，需授权开发库） | 两条聚合 SQL 在 MySQL/TiDB 上的真实分布与求和不变量 | — |

### 7.2 配置解析单元测试（L1）

以 `MetricsConfig::from_parts` / `from_parts_with` / `parse_file` 表驱动，逐条断言错误消息的**固定文案**：

| 测试名 | 场景 | 期望 |
| --- | --- | --- |
| `parses_aligned_example` | §3.2 示例 TOML | `enabled=true`、`endpoint="/metrics"`、凭据为示例值 |
| `omitting_metrics_section_is_disabled` | 空文档 / 无 `[metrics]` | `enabled=false`、`endpoint="/metrics"`、`credentials=None` |
| `metrics_section_requires_enabled` | `[metrics]` 存在但无 `enabled` | `Err("metrics.enabled is required")` |
| `rejects_unknown_top_level_table` | `[database]` 顶层表 | `Err`（未知字段） |
| `rejects_unknown_key_in_metrics` | `[metrics]` 内 `endpont = "/x"` | `Err`（未知字段） |
| `rejects_unknown_key_in_auth` | `[metrics.auth]` 内 `passwaerd = "x"` | `Err`（未知字段） |
| `rejects_invalid_toml_without_leaking_values` | 语法错误且错误行含 `password = "secret-value"` | `Err`，且 `err.message()` 不含 `secret-value` |
| `rejects_non_basic_auth_type` | `type = "bearer"` / `"none"` / `""` | `Err("metrics.auth.type must be \"basic\"")` |
| `rejects_endpoint_without_leading_slash` | `endpoint = "metrics"` | `Err("metrics.endpoint is invalid")` |
| `rejects_endpoint_with_trailing_slash_query_or_space` | `"/metrics/"`、`"/metrics?x=1"`、`"/met rics"`、`"/a//b"`、`"/METRICS"` | 同上 |
| `rejects_reserved_endpoint` | `"/healthz"`、`"/api/v1/tasks"`、`"/devices"`、`"/"` | `Err("metrics.endpoint collides with an existing route")` |
| `rejects_username_with_colon_or_control_char` | `"a:b"`、`"a\u{7}"` | `Err("metrics.auth.username is invalid")` |
| `rejects_empty_password` | `password = ""` | `Err("metrics.auth.password is invalid")` |
| `fail_closed_when_enabled_without_auth` | `enabled=true` 且无 `[metrics.auth]`、无环境变量 | `Err("metrics.auth is required when metrics.enabled is true")` |
| `rejects_blank_username_when_enabled` | `enabled=true`、`[metrics.auth]` 内 `username = ""` | `Err("metrics.auth.username is invalid")` |
| `rejects_blank_password_when_enabled` | `enabled=true`、`[metrics.auth]` 内 `password = ""` | `Err("metrics.auth.password is invalid")` |
| `disabled_drops_credentials_without_error` | `enabled=false` + 完整 `[metrics.auth]` | `Ok`，`credentials=None`，终态不保留密码副本 |
| `env_pair_overrides_file_credentials` | 文件示例值 + 环境变量对 | 终态为环境变量值 |
| `env_partial_override_is_rejected` | 只设 `PROMETHEUS_AUTH_USER` | `Err("metrics auth override must set both user and password")` |
| `env_empty_string_counts_as_unset` | `PROMETHEUS_AUTH_USER=""`、password 已设 | `Err`（同 partial） |
| `error_messages_never_echo_credentials` | 覆盖上述所有 `Err` 分支 | 消息不含用户名/密码字面量 |

另需路径与覆盖语义测试，全部通过 `from_parts_with` 注入闭包（假 `env` 映射 + 假文件读取），不使用 `std::env::set_var`：`explicit_path_missing_is_error`、`explicit_path_present_is_used`、`default_path_absent_is_ok`、`default_path_present_is_used`、`empty_config_env_falls_back_to_default_path`、`unreadable_file_is_error`、`non_utf8_file_is_error`。

### 7.3 Basic Auth 与常量时间单元测试（L1）

| 测试名 | 场景 | 期望 |
| --- | --- | --- |
| `parses_valid_basic_header` | `Basic` + `base64("prom_admin:secure_password")` | 得到正确的两段 |
| `scheme_is_case_insensitive` | `Basic` / `basic` / `BASIC` | 均解析成功；`Bas ic`（内部空格）失败 |
| `rejects_other_schemes` | `Bearer <b64>`、`Digest …`、无 scheme、多空格、前导空白 | `None` |
| `rejects_multiple_authorization_headers` | 两个 `Authorization` 头（值相同或不同） | `None`（绝不取第一个） |
| `rejects_invalid_base64_and_unpadded` | 含 `!`、长度非 4 倍数、去掉 `=` padding | `None` |
| `rejects_non_utf8_and_missing_colon` | 解码后非法 UTF-8；无 `:`；`":pw"`；`"u:s:p"` | `None` |
| `verify_accepts_exact_match` | 完全匹配 | `true` |
| `verify_rejects_each_field_independently` | 用户名错 / 密码错 / 都错 / 都为空 | 均 `false`（响应不可区分） |
| `verify_is_length_independent` | 长度差 1 与差 64 的错误密码均 `false` | 与 `ct_eq_32` 固定长度契约一致（长度不进比较） |
| `ct_eq_32_is_content_sensitive` | 逐位差异（首字节、末字节、单比特） | 均 `false`；相等为 `true` |
| `verify_runs_both_comparisons` | 用可观测封装（`#[cfg(test)]` 计数的 `ct_eq_32` 包装）断言用户名不匹配时密码比较仍发生 | 调用计数 == 2 |

`verify_runs_both_comparisons` 是 §4.3 第 1 条的回归网：将来有人把两次比较改回短路写法，该测试必须变红。

### 7.4 数据模型与渲染单元测试（L1）

| 测试名 | 断言 |
| --- | --- |
| `admission_label_maps_persisted_vocabulary` | `APPROVED→APPROVED`、`PENDING→PENDING`、`REVOKED→REJECTED`；`"approved"`（小写）、`""`、`"UNKNOWN"` → `None` |
| `task_bucket_covers_documented_matrix` | §5.2.2 全表逐条，含 `("completed", None)→failed` |
| `task_bucket_fails_closed_on_unknown_state` | `("queued ", _)`、`"DONE"`、`""` → `None` |
| `render_matches_golden_exposition` | §5.6 fixture 逐字节相等（含末尾单个 `\n`） |
| `render_emits_zero_valued_series` | 全 0 fixture 仍输出全部 11 条序列 |
| `render_escapes_label_values` | 含 `"`、`\`、`\n` 的标签值被正确转义（用直接构造的 `ScrapeValues`） |
| `render_values_are_unsigned_decimal` | 正则逐行匹配 `^[a-z_]+(\{[^}]*\})? [0-9]+$`，不出现 `e`/`.`/`-`/`NaN` |
| `render_is_deterministic` | 同一输入渲染两次逐字节相等 |
| `render_series_count_and_sum_invariants` | 样本条数 == 11；`devices_total` 三值之和 == `devices` 总数；`tasks_total` 四值之和 == 主任务总数 |
| `render_rejects_oversized_body` | 构造超 `MAX_BODY_BYTES` 的输入 → `render` 返回 `Err(Config)`，`Err` 消息不含任何指标文本片段 |
| `uptime_never_regresses_across_wall_clock_jumps`（`collect.rs` 的 `#[cfg(test)]`） | 以 `MetricsRuntime { started_at: Instant::now() - 60s, .. }` 断言 `uptime_seconds() >= 60`；再次调用取值不减（与 `http_device_status::age_ms` 的单调锚点规则一致，宿主墙钟跳变不影响） |

### 7.5 路由集成测试（L2）

固定 `MetricsState` + `FakeMetricsSnapshotSource`（可设定 `Ok(fixture)` / `Err(Config)` / 挂起超时的三种行为），`oneshot` 发请求：

| 测试名 | 请求 | 期望 |
| --- | --- | --- |
| `scrape_with_valid_credentials_returns_200` | `GET /metrics` + 正确 Basic | `200`；`Content-Type` 逐字节等于常量；体与 golden 一致；`Cache-Control: no-store`；无 `Set-Cookie` |
| `scrape_without_credentials_returns_401` | 无 `Authorization` | `401`；`WWW-Authenticate` 逐字节等于 `Basic realm="rsetup-controller-metrics"`；体为空；**source 调用次数 0** |
| `scrape_with_wrong_password_returns_401` | 正确用户名 + 错密码 | `401`，与无凭据响应同形（source 0 次） |
| `scrape_with_wrong_username_returns_401` | 错用户名 + 正确密码 | `401`（source 0 次） |
| `scrape_with_malformed_header_returns_401` | `Bearer …`、非法 base64、无冒号、两个 `Authorization` 头 | 均 `401`（source 0 次） |
| `disabled_endpoint_returns_404_regardless_of_credentials` | `enabled=false`，分别带/不带凭据 | 均 `404`；无 `WWW-Authenticate`；无 `Content-Type`；source 0 次 |
| `other_paths_are_not_registered_by_metrics_line` | `GET /`、`/healthz`、`/metrics.txt`、`POST /metrics` | 404 / 404 / 405（本线不注册任何其他路径） |
| `metrics_ignores_session_cookie` | 携带格式合法的 `rsc_session=…` 而无 Basic | `401`（§4.1 隔离的行为证明） |
| `metrics_success_does_not_create_session` | 成功抓取 200 | 响应无 `Set-Cookie`；`HttpAuthState` 相关结构（若断言）零变化 |
| `source_error_maps_to_503_not_partial_zero` | source 返回 `Err(Config)` | `503`；体为空；**不含任何指标行** |
| `source_timeout_maps_to_503` | source 挂起 > `SCRAPE_TIMEOUT` | `503`（测试用 `tokio::time::pause`/缩短常量注入） |
| `scrape_concurrency_cap_is_enforced` | 占满 `Semaphore(4)` 后请求 | `503`，且 source 未被再次进入 |
| `credentials_are_not_echoed_in_error_paths` | 各 401 响应 | 头与体都不含提交的凭据原文或其 base64 |

### 7.6 架构指针一致性与路由合并测试（L3）

扩展 `crates/rsetup-controller/tests/production_composition.rs`：

1. `inputs()` 构造增加 `metrics: MetricsLineInputs { config: Arc::new(disabled/enabled fixture), runtime, snapshot: Arc::new(SyntheticMetricsSnapshotSource) }`；既有五线断言保持不变。
2. `every_component_is_reachable_without_duplicate_probes`：合成拓扑扩为六组件，断言六条路径（含 `/metrics`）在同一顶层 router 上各自可达、marker 不串线，无重复探活。
3. `duplicate_business_path_panics_instead_of_being_hidden_by_override`：既有 panic 用例扩到六参数版本；新增用例 `metrics_path_colliding_with_reserved_route_panics_on_merge`（`compose_component_routers` 传两个 `/metrics`）确认合并期 panic 仍存在，作为配置期拦截的第二道网。
4. `mismatched_auth_context_is_rejected_on_every_line`：保持三条负例（management/tasks/status）不变，证明指标线新增未削弱原门禁。
5. `shared_auth_context_composes_all_five_lines` 改名为 `shared_auth_context_composes_all_six_lines` 并扩展断言：`enabled=true` 的 fixture 下，`/healthz` 200、`/readyz` 503（lazy 池不可达）、`/api/v1/auth/me` 401、`GET /metrics` 无凭据 401 且带 `WWW-Authenticate`——四者同时成立，证明单一端口、六线合并且原五线断言全部保留。
6. `metrics_gauges_read_the_shared_pool_only`：用 lazy 池断言 `db_pool_connections{state="active"} 0` 与 `{state="idle"} 0`（lazy 池未建立任何连接），且抓取前后 `lazy_db()` 未产生连接——证明指标线读的是同一池且不发查询。
7. `metrics_line_never_touches_session_boundary`：在全量顶层 router 上，用完整合法会话 Cookie（L2 的 `LiveSessionAuth` 桩形态）请求 `/metrics` 得 401；随后请求 `/api/v1/auth/me` 仍按会话语义工作，证明指标抓取未污染会话/CSRF/限速状态。
8. `reserved_paths_stay_registered`：对 `/healthz`、`/readyz`、`/`、`/login`、`/assets` 前缀逐一探测，断言它们仍由所属条线应答（而不是被指标线抢占或退化为 404），并且 `RESERVED_PATHS` 覆盖 `web.rs` 的 `SPA_EXACT_ROUTES`/`SPA_PARAM_ROUTES`/`ASSET_DIR` 全部条目，使保留集合不与代码漂移。

### 7.7 真实数据库验收（L4）与边界

两条聚合 SQL 的真实行为（`GROUP BY` 分布、`CAST(... AS CHAR)` 类型、TiDB 兼容性、求和不变量、空表 → 全 0 序列）按 `tests/mysql_identity.rs`/`tests/tidb_identity.rs` 的做法在专用 `#[ignore]` 测试中验收，连接**已获授权的隔离开发库**、凭据仅进程内载入、测试报告只出去敏结果（`AGENTS.md` 凭据保护规则）。该层只读，不建表、不迁移、不写入。L2 的 fake source 通过 MUST NOT 被表述为真库聚合已验收；反之 L4 未运行时，交付报告必须写明"两条聚合 SQL 的真实引擎验收仍待执行"。

### 7.8 命令与静态门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p rsetup-controller
cargo test -p rsetup-controller --test production_composition
grep -rnE 'TO''DO|T-B-D|FIXME' crates/rsetup-controller/src/metrics
```

最后一条 grep MUST 无输出；模式串刻意写成 `'TO''DO|T-B-D|FIXME'` 的 shell 拆分形式，这样把本命令原文写进文档后，同一条 grep 也不会命中自己。评审时另需对本文件执行同一检查（预期零命中）。提交前另需检查 `git diff --cached` 不含真实凭据、不含 `controller.toml` 实体（§3.7）。

---

## 8. 安全评审要点与残余风险

| 编号 | 风险 | 影响 | 本期处置 |
| --- | --- | --- | --- |
| R1 | 长期静态凭据、进程内无失败限速（§4.6） | 在线暴力猜测 | 默认监听 `127.0.0.1:8080`；对外暴露必须由部署层加来源限制/TLS/失败封禁；密码建议 CSPRNG ≥ 20 字符；比较不短路且长度归一 |
| R2 | 凭据明文存于 TOML | 文件读取即泄密 | 建议 `0600` + 私有目录；环境变量成对覆盖允许不落盘；凭据不入库、不入日志、不入 `Debug`；错误消息不回显（§3.4/§3.6 有测试） |
| R3 | 哈希前耗时与 base64 解析失败提前返回（§4.3 第 4 条） | 弱时序旁证（不含凭据内容） | 明确留档为可接受残余；不宣称全 handler 常量时间 |
| R4 | `401` 携带 `WWW-Authenticate` 会宣告端点存在 | 信息暴露有限 | 对齐设计的显式要求（抓取方排障需要）；禁用态 404 刻意不带该头，保持与"路径不存在"不可区分 |
| R5 | 禁用态下非 GET 返回 405（§4.5） | 可探测配置路径存在 | 与业务路由既有语义一致，不特殊化；如需完全隐藏须改为"不注册路由"，同时放弃 §6.2 的确定性 404，本期不做 |
| R6 | 每次抓取两条聚合查询 | 高频抓取压库 | 两条 `GROUP BY` 上限为表行数级；抓取并发闸 4 + 2s 超时；`scrape_interval` 由部署控制（建议 ≥ 30s）；`devices` 可用既有 `ix_devices_admission`，`main_tasks` 按 `(state, outcome)` 分组无法完全依赖既有 `ix_main_tasks_state`（不覆盖 `outcome`），属可接受的观测成本，出现实测压力时另立规范引入覆盖索引或缓存 |
| R7 | 跨两条聚合的非事务读 | 两族之间毫秒级偏差 | §5.3 第 3 条显式声明为近似瞬时 gauge；对账走 SQL |
| R8 | `_total` 后缀用作 gauge 违反命名习惯 | 误用 `rate()` | §5.4：`# TYPE gauge` 必发 + `HELP` 说明求和语义 |
| R9 | `REVOKED → REJECTED` 词汇映射（§5.2.2） | 阅读者误认为存在第四种准入态 | 映射表在指标 `HELP`、本文与单元测试三处留档；未知值 fail-closed 503，绝不静默归类 |
| R10 | `toml` 为工作区新增依赖 | 供应链与离线构建 | 仅解析用途、不启用额外 feature；首次构建需更新 `Cargo.lock` 并在交付说明中登记新增 crate |

---

## 9. 验收标准（Definition of Done）

1. §7.2–§7.6 的全部测试存在且通过，并各自先有对应 RED 记录（实现提交按 RED→GREEN 分步）。
2. 端点在默认（禁用）配置下不改变现有任何响应；启用后 `/metrics` 在同一端口 200，`Content-Type` 与 golden 文本逐字节符合 §5.1/§5.6。
3. `enabled = true` 且凭据不完整/非法时进程拒绝启动，错误文案与 §3.6 完全一致，且没有产生监听套接字或数据库连接。
4. §4.5 矩阵每一行都有对应自动化断言；401 头逐字节符合 §4.4。
5. §4.1 隔离不变量与 I1–I6 均有代码结构或测试证据；未新增 `EndpointId` 变体、permission、审计写入、数据库对象或第二个连接池构造点。
6. §7.8 的 fmt/clippy/test 全绿，`grep` 无占位符，提交内不含凭据。
7. 若 L4 真库聚合验收尚未执行，交付报告必须显式标注该项未完成，不以 L2 fake 通过替代（G4/§7.7）。

### 9.1 交付边界

本规范文档本身不改动源码。实现应拆为可独立审查的增量：①`config` + 单测；②`auth` + 单测；③`model`/`render` + 单测；④`collect`（含 SQL）+ L1/L4；⑤`http` + L2；⑥`production`/`main` 装配 + L3；⑦文档与部署说明。提交信息遵循仓库规则（简体中文，Conventional Commits 描述亦为中文），例如 `feat(controller): 增加普罗米修斯指标端点与配置文件认证`。

---

## 10. 附录

### 10.1 `controller.toml` 完整示例（不可直接使用）

```toml
# 示例形状：真实部署请替换为强随机凭据，并将文件权限设为 0600。
# 本期该文件只允许 [metrics] 段；其他顶层表会被拒绝。
[metrics]
enabled = true
endpoint = "/metrics"

[metrics.auth]
type = "basic"
username = "prom_admin"
password = "secure_password"
```

与"完全省略该文件"等价的显式禁用形态：

```toml
[metrics]
enabled = false
```

对照：只写 `enabled = true` 而不写 `[metrics.auth]` 是**非法配置**，启动失败并报告 `metrics.auth is required when metrics.enabled is true`（§3.6 分支 2），不会退化成免认证端点。

### 10.2 环境变量清单（本设计新增）

| 变量 | 作用 | 缺省 | 空串语义 |
| --- | --- | --- | --- |
| `CONTROLLER_CONFIG` | TOML 配置文件路径（可为相对进程工作目录） | `controller.toml` | 视为未设置，回落默认路径 |
| `PROMETHEUS_AUTH_USER` | 覆盖 `[metrics.auth].username` | 无 | 视为未设置 |
| `PROMETHEUS_AUTH_PASSWORD` | 覆盖 `[metrics.auth].password` | 无 | 视为未设置 |

`PROMETHEUS_AUTH_USER` 与 `PROMETHEUS_AUTH_PASSWORD` MUST 同时设置或同时不设置（§3.3 第 3 步）。

### 10.3 与既有配置的关系（本期不迁移）

| 配置 | 现有来源 | 本期 |
| --- | --- | --- |
| `CONTROLLER_DATABASE_URL`、`CONTROLLER_LISTEN_ADDRESS` | 环境变量（`config.rs`） | 不变，TOML 不接受这些键（未知顶层表被拒） |
| `CONTROLLER_ALLOWED_HOSTS`、`CONTROLLER_ALLOWED_ORIGIN`、`CONTROLLER_TRUST_TLS` | 环境变量（`http_security.rs`） | 不变，且 MUST NOT 影响指标端点（§4.1） |
| 指标 `[metrics]` | 新增 TOML（+ 两个凭据环境变量覆盖） | 本设计新增 |

### 10.4 Prometheus 抓取配置示例

```yaml
scrape_configs:
  - job_name: rsetup-controller
    metrics_path: /metrics
    scheme: http
    scrape_interval: 30s
    basic_auth:
      username: prom_admin
      # 凭据不写进 prometheus.yml；文件由部署管理，仓库不跟踪。
      password_file: /etc/prometheus/rsetup-metrics.password
    static_configs:
      - targets: ["127.0.0.1:8080"]
```

### 10.5 手工验收命令

```bash
# 期望 200 + 指标文本
curl -fsS -u 'prom_admin:secure_password' http://127.0.0.1:8080/metrics -D -

# 期望 401 且带 WWW-Authenticate: Basic realm="rsetup-controller-metrics"
curl -sS -o /dev/null -D - http://127.0.0.1:8080/metrics

# 期望 401
curl -sS -o /dev/null -D - -u 'prom_admin:wrong-password' http://127.0.0.1:8080/metrics

# 期望 404（enabled = false 配置）
curl -sS -o /dev/null -D - http://127.0.0.1:8080/metrics
```

### 10.6 关键决策摘要

| 决策 | 取舍与被否方案 |
| --- | --- |
| 只支持 Pull，不做 Pushgateway/远程写 | 与 Prometheus 生态默认一致、失败可见（`up`）；推送需要额外的重试/缓冲语义，本期无需求 |
| 同端口同进程 | 少一个监听面与一套 TLS；代价是指标线故障与业务共用进程（可接受，采集只读且带并发闸与超时） |
| 独立 Basic Auth 边界，不复用会话 | 机器客户端无会话与 Origin；复用会把 Host/Origin/CSRF 语义强推给监控（§4.1） |
| 凭据来自 TOML，环境变量成对覆盖 | 需要结构化的多键配置；成对覆盖避免混搭出不可追溯的半新凭据 |
| 启用即要求凭据（启动期 fail-closed） | 拒绝"先开起来再补认证"的默认开放风险；与仓库既有配置门禁风格一致 |
| 不引入指标 SDK | 11 条固定序列无需注册中心；避免隐式指标与新的全局状态 |
| 采集失败整次 503 | 拒绝部分降级为 0：伪 0 会让告警静默，比"抓取失败"更危险 |
| 池 gauge 结构性派生自 `auth.db` | 比第四条 `Arc::ptr_eq` 检查更强：违规不可编译 |
| `held` 并入 `pending`、`REVOKED` 映射 `REJECTED` | 保持对齐设计固定的标签域与求和不变量；映射在 `HELP`、本文与测试三处留档 |

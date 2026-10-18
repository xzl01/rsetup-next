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

在**工作区根** `Cargo.toml` 的 `[workspace.dependencies]` 增加三行，并在 `crates/rsetup-controller/Cargo.toml` 的 `[dependencies]` 以 `workspace = true` 引用：

```toml
# Cargo.toml（工作区根，[workspace.dependencies]）
toml = "0.8"
base64 = "0.22"
subtle = "2.6"
```

```toml
# crates/rsetup-controller/Cargo.toml（[dependencies]）
toml.workspace = true
base64.workspace = true
subtle.workspace = true
```

约束与说明：

1. `toml` 只用于解析配置文件文本；`base64` 只用于 §4.2 的 `Authorization: Basic` 解码；`subtle` 只用于 §4.3 的常量时间比较。三者**不得**用于其他用途。
2. `base64 = "0.22"` 与 `Cargo.lock` 中已锁定的 `base64 0.22.1`（当前为传递依赖）同版本，升级为直接依赖不引入新树；`subtle = "2.6"` 同理对应 `Cargo.lock` 已锁定的 `subtle 2.6.1`（当前是 `digest`、`password-hash`、`rustls` 的传递依赖）。**传递依赖不等于可用**：不经由 `[dependencies]` 显式声明，本 crate 无法 `use subtle::ConstantTimeEq`，因此必须把它提升为直接依赖（同版本、不新增依赖树）。`toml 0.8` 是**首次**进入本工作区，首次构建需在有网络/有 vendor 的环境更新 `Cargo.lock`，并把新增 crate 记入构建说明。
3. 只使用 `toml` 的 `from_str` 反序列化能力，不得启用 `preserve_order` 等 feature；`base64` 只使用 `general_purpose::STANDARD`（要求 padding、拒绝尾部比特残留、不容忍空白），不得使用 `STANDARD_NO_PAD` 或自定义宽松 `GeneralPurposeConfig`；`subtle` 只使用 `ConstantTimeEq`（可加 `Choice`→`bool` 的 `Into`），不得启用 `std`/`core_hint_black_box` 之外的额外 feature。
4. 密码学相关只允许 `sha2` 与 `subtle` 两个直接依赖：`sha2` 已是本 crate 直接依赖（`tasks/http.rs`、`auth/session.rs` 已在用），`subtle` 按上文提升为直接依赖；不得引入 `ring`、`constant_time_eq` 等第三家，也**不得**手写 `diff |= a[i] ^ b[i]` 循环替代 `subtle::ConstantTimeEq`（§4.3）。

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

Rust 结构**严格区分"文件里的原始形状（Raw DTO）"与"校验后的终态"**，配置处理分三阶段（§3.3）：Raw DTO 的每个叶子字段都是 `Option`，任何"必填/非空/取值域"判断都不发生在反序列化期，而发生在阶段三的统一门禁里。理由：一旦 `[metrics.auth]` 的字段声明成必填 `String`，那么 `enabled = false` 的配置只要写了一半 auth（运维轮换凭据中途、注释掉两行准备替换），TOML 反序列化就直接失败并**阻断整个服务启动**——禁用态的指标线根本没有能力伤害任何人，却把主业务一起拖死。

```rust
// metrics/config.rs

/// 阶段一（反序列化）形状：整份 TOML 文档。除 `[metrics]` 外的任何顶层表都不被接受。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsFile {
    pub metrics: Option<MetricsSection>,
}

/// 阶段一形状：`[metrics]` 段。`enabled` 用 `Option` 承载"是否显式声明"，缺省策略在 `from_parts` 裁决。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    pub enabled: Option<bool>,
    pub endpoint: Option<String>,
    pub auth: Option<MetricsAuthRaw>,
}

/// 阶段一形状：`[metrics.auth]` 段。**全部字段可选**，类型只有 `String`，
/// 缺失/空串/取值非法都不在这里判失败（`deny_unknown_fields` 仍然生效：
/// 拼错的键必须在阶段一就被拒绝）。是否必填由 §3.6 门禁按 `enabled` 裁决。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsAuthRaw {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

/// 校验后的终态：只有这一种形态能离开 config.rs。
/// **手写脱敏 `Debug`**（不 `derive(Debug)`，也不依赖字段类型是否实现 `Debug`）：
/// 凭据只以"有无"出现（固定 `[redacted]`），绝不出现原文、用户名或任何长度；
/// 未来给本结构新增凭据类字段时，编译器会强制在同一个 `impl Debug` 里显式处置它。
#[derive(Clone)]
pub struct MetricsConfig {
    pub enabled: bool,
    /// 已校验的绝对路径形式端点，恒以 `/` 开头且无尾斜杠。
    pub endpoint: String,
    /// `Some` 当且仅当 `enabled == true`；禁用态不保留凭据副本。
    pub credentials: Option<BasicCredentials>,
}

impl std::fmt::Debug for MetricsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsConfig")
            .field("enabled", &self.enabled)
            .field("endpoint", &self.endpoint)
            // 脱敏：只暴露"有无凭据"，不暴露用户名、长度与密码。
            .field("credentials", &self.credentials.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// 不派生 `Debug`（防凭据经 `{:?}` 泄出）也不派生 `PartialEq`（防用短路 `==`
/// 比较凭据，§4.3）；终态断言由测试逐字段进行（§7.2）。
#[derive(Clone)]
pub struct BasicCredentials {
    pub username: String,
    pub password: String,
}
```

`MetricsConfig` 的 `Debug` 是**手写**而非"恰好不可派生"：脱敏文案固定为 `"[redacted]"`，MUST NOT 输出用户名、密码或二者长度（用户名同样属 §3.7 的去敏范围）。回归网是 §7.2 的 `metrics_config_debug_is_redacted`（对启用态实例求 `{:?}`）与 `error_messages_never_echo_credentials`（错误消息形态）。

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

### 3.3 加载顺序与优先级（三阶段生命周期）

固定四步、三阶段语义：**路径解析 →（阶段一）反序列化 Raw DTO →（阶段二）环境变量原子覆盖 →（阶段三）统一门禁校验与终态装配**。阶段二只写数据不做门禁，阶段三只读数据不做覆盖，两者责任不重叠，因此"某条规则在什么时候生效"始终是单一可判定问题（这是禁用态能被不完整 auth 配置安全放行的前提）。

```rust
impl MetricsConfig {
    /// 纯函数核心：Raw DTO + 环境变量覆盖对 → 终态配置（阶段二 + 阶段三）。
    /// 全部校验与 fail-closed 门禁集中在这里，保证可用 `from_parts` 表驱动测试。
    /// `override_pair` 是**已经成对**的两个环境变量取值；`None` 表示环境变量未设置
    /// （空串与 `NotUnicode` 在 `from_parts_with` 侧已分别处置，见第 3 步）。
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
        env: &dyn Fn(&str) -> EnvRead,
        read_file: &dyn Fn(&std::path::Path) -> Result<String, std::io::Error>,
    ) -> Result<Self, ControllerError>;

    /// 纯函数：TOML 文本 → 阶段一 Raw 形状（只检查键集合与类型形状，不判必填）。
    pub fn parse_file(text: &str) -> Result<MetricsFile, ControllerError>;
}

/// 环境变量读取的三态结果。MUST 区分"未设置"与"设置了但不是合法 Unicode"：
/// `std::env::var` 对后者返回 `VarError::NotUnicode`，若把它折叠成 `Option::None`，
/// 一次误注入的二进制凭据就静默退化为"环境变量未覆盖"，实际生效值回落到文件里的
/// 旧凭据（凭据轮换静默失效），违反 I4。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvRead {
    /// `VarError::NotPresent`。
    Unset,
    /// `VarError::NotUnicode`：变量存在但不是合法 UTF-8。MUST NOT 回显原始字节。
    NotUnicode,
    /// 取值存在（可能是空串；空串语义由阶段二裁决）。
    Value(String),
}

impl EnvRead {
    /// 把真实 `std::env::var` 映射到三态：`Ok(v)` → `Value(v)`、
    /// `Err(VarError::NotPresent)` → `Unset`、`Err(VarError::NotUnicode(_))` → `NotUnicode`。
    pub fn from_std(name: &str) -> Self;
}
```

`from_env()` 的确定步骤：

1. **路径解析**：取 `CONTROLLER_CONFIG`（空串按未设置处理，与 `HttpSecurityConfig::from_values` 的 `filter(|v| !v.is_empty())` 惯例一致）。
   - `EnvRead::NotUnicode` → `Err(ControllerError::Config("metrics config path is not valid unicode"))`：固定文案，MUST NOT 含变量值或路径字节。此时"显式指定了哪个文件"已不可知，回落默认路径属静默降级，因此一律 fail-closed。
   - 已设置 → 该路径为**权威路径**，文件必须存在且可读；不存在/不可读即 `Err`（显式指定即表达"必须从这里加载"的意图，静默回落会造成"以为改了其实没改"）。
   - 未设置 → 使用相对路径 `controller.toml`（相对进程工作目录）。该文件**不存在** → 视为"无配置文件"，`file = None`，继续第 3 步（部署可以完全不用 TOML，仅用环境变量）。文件存在但不可读/不可解析 → `Err`。
2. **阶段一反序列化**：读取整个文件为 UTF-8 文本（非 UTF-8 → `Err`），交给 `parse_file` 得到 `MetricsFile`。本阶段只接受形状：`[metrics.auth]` 三个字段都是 `Option<String>`（§3.2），因此写了半截的 auth 段在这里必然成功，"缺什么"由阶段三按 `enabled` 裁决。
3. **阶段二：环境变量原子覆盖（成对、原子）**：读取 `PROMETHEUS_AUTH_USER`、`PROMETHEUS_AUTH_PASSWORD`，各自空串按未设置处理。
   - 任一变量为 `EnvRead::NotUnicode` → `Err(ControllerError::Config("metrics auth override env var is not valid unicode"))`：两个变量共用这一条固定消息，不区分是哪一个、不回显取值。"设置了但不可用"绝不当作"未设置"继续沿用文件里的凭据。
   - 两者皆未设置 → 不覆盖（`override_pair = None`）。
   - 两者皆已设置（非空）→ 覆盖/注入凭据字段（`override_pair = Some((user, password))`）：`username`、`password` 一律替换为环境变量值；Raw 里**没有** `[metrics.auth]` 段时按覆盖语义**合成**该段；Raw 里 `kind` 缺失（`None`）时一并注入 `kind = Some("basic")`——用环境变量交付凭据本身即表达"basic 认证"的意图，不要求再补一行 `type`。Raw 里已显式写了 `kind` 时**保留原值**（含非法值）交给阶段三拒绝：覆盖不猜意图、不偷偷改协议。
   - **只设置其一 → `Err`（固定消息 `"metrics auth override must set both user and password"`）**。禁止把文件里的 username 与环境里的 password 拼成一半新一半旧的凭据：这类混搭会让轮换出现不可预期的组合，且排障时无法从任一来源还原实际生效值。
   - 覆盖发生在阶段三门禁**之前**，因此"文件里禁用 + 环境变量给凭据"合法（凭据在阶段三被丢弃），"文件里启用 + 环境变量补凭据"合法。
4. **阶段三：统一门禁校验**：`from_parts` 按 §3.4（endpoint 与保留路径）、§3.5（`kind`）、§3.6（凭据完整性与终态裁决）顺序校验，任一失败返回 `ControllerError::Config(固定去敏消息)`。凭据字段的必填、非空与字符集校验**只在这里**发生，且只在 `enabled == true` 分支执行。

优先级结论（后者覆盖前者）：`内置默认值` → `controller.toml`（或 `CONTROLLER_CONFIG` 指定文件） → `PROMETHEUS_AUTH_USER`/`PROMETHEUS_AUTH_PASSWORD`。`enabled` 与 `endpoint` **没有**环境变量覆盖通道，只能来自文件或内置默认值（避免用环境变量悄悄打开一个未配置凭据的端点）。

`MetricsConfig::from_env()` 的调用点必须在 `ControllerConfig::from_env()` 之后、`DbPool::connect()` 之前（§6.4），保证配置门禁失败时进程没有产生任何数据库连接或监听套接字副作用。

### 3.4 缺省值与取值校验

| 键 | 缺省 | 合法取值 | 校验阶段 | 违规后果 |
| --- | --- | --- | --- | --- |
| 顶层未知表 | — | 只允许 `[metrics]` | 1（反序列化） | `Err`（`deny_unknown_fields`） |
| `[metrics]` 整段缺失 | `enabled=false`、`endpoint="/metrics"`、无 `auth` | — | — | 合法（端点注册但恒 404，见 §4.5） |
| `[metrics]` 存在但缺 `enabled` | 无缺省 | `true`/`false` 必须显式 | 3（门禁） | `Err`：`"metrics.enabled is required"`（表已存在即必须显式声明意图） |
| `endpoint` | `"/metrics"` | `len ≤ 128`；首字符 `/`；仅由 `/` 分隔的非空 ASCII 小写段组成（`[a-z0-9_-]+`）；不得以 `/` 结尾；不得含 `?`/`#`/`%`/空白/`..` | 3（门禁，两种 `enabled` 都校验） | `Err`：`"metrics.endpoint is invalid"` |
| `endpoint` 与既有路由冲突 | — | 不得等于/落在 §6.5 的保留路径集合内 | 3（门禁，两种 `enabled` 都校验） | `Err`：`"metrics.endpoint collides with an existing route"` |
| `[metrics.auth].type` | 无缺省 | 恰好 `"basic"`（区分大小写，不做 trim）；`enabled = true` 时必填（缺失只可由环境变量凭据对推导，§3.3 第 3 步） | 3（门禁，仅 `enabled = true`） | `Err`：`"metrics.auth.type must be \"basic\""` |
| `[metrics.auth].username` | 无缺省 | 非空；不含 `:`；不含 `< 0x20` 或 `0x7F` 控制字符 | 3（门禁，仅 `enabled = true`） | `Err`：`"metrics.auth.username is invalid"` |
| `[metrics.auth].password` | 无缺省 | 非空；**可以包含 `:`**；不含 `< 0x20` 或 `0x7F` 控制字符 | 3（门禁，仅 `enabled = true`） | `Err`：`"metrics.auth.password is invalid"` |

统一规则：

1. **不做任何归一化**：不 trim、不 case folding、不做 Unicode 规范化。配置文件里的字节就是比对用的字节（凭据语义由运维负责，实现不"猜测"意图）。
2. **只有 `username` 禁止 `:`**：RFC 7617 §2 规定 `userid` 不得包含冒号，这是前移校验——配置期就把不可用凭据挡在启动门禁，而不是让抓取方在线上收到无解的 401。**`password` 明确允许包含冒号**（RFC 7617 的 `password` 无此限制）：解析按**首个**冒号切分（§4.2 第 6 步 `split_once(':')`），首个冒号之后的全部字符都归属密码，因此 `a:b:c` 是合法密码。MUST NOT 在配置期或解析期对密码里的冒号做任何拒绝或转义处理。
3. **未知键 fail-closed**（`deny_unknown_fields`）：拼错的键（如 `passwaerd`、`endpont`）会静默变成缺省值，是监控可用性最典型的隐性故障，因此一律拒绝启动而不是忽略。该规则属于阶段一，与 `enabled` 取值无关。
4. **凭据类规则只在 `enabled = true` 时生效**（阶段三）：`enabled = false` 时 `[metrics.auth]` 的任何组合（缺失、只写一个字段、空串、`type = "basc"`）都是合法配置并被直接丢弃，MUST NOT 阻断服务启动（§3.6 分支 1）。`endpoint` 与其冲突检查则两种状态都要校验——禁用态该路径仍会注册路由并返回 404，路径写错或撞上业务路由同样是缺陷。
5. **密码最小长度不强制**：强制长度会破坏既有秘密轮换，本期只在 §8 风险表登记并给出部署建议（建议 ≥ 20 字符、由 CSPRNG 生成）。密码含冒号、`=`、UTF-8 多字节字符都是受支持的组合。
6. TOML 解析错误必须转换为其消息**只含行列号**的固定形状：`ControllerError::Config(format!("metrics config: invalid TOML syntax at line {line}"))`。不得复用 `toml::de::Error` 的 `Display`，因为其消息可能包含该行原文（潜在秘密），违反 I5。
7. 环境变量凭据值**非 Unicode 是配置错误**，不是"未设置"（§3.3 `EnvRead::NotUnicode`）：静默当未设置会让轮换后的新凭据不生效而继续跑旧凭据，属于最危险的一类静默降级。

### 3.5 认证类型门禁

`kind`（TOML 键 `type`）在 Raw DTO 里是 `Option<String>`（§3.2），门禁规则：

1. `Some(v)`：`v` 必须逐字节等于 `"basic"`（区分大小写、不做 trim）。`"none"`、`"bearer"`、`"token"`、空串及任何未知值一律 `Err("metrics.auth.type must be \"basic\"")` 并拒绝启动，不回落到"免认证"或"默认 basic"。免认证公开指标在本期是不可用能力，而不是可用但需自行加固的能力。
2. `None`：**只有一种**合法出路——凭据来自环境变量覆盖对，由阶段二推导注入 `Some("basic")`（§3.3 第 3 步）。除此之外 `enabled = true` 且 `kind == None` 即 `Err("metrics.auth.type must be \"basic\"")`：文件里写了 auth 段却不声明 `type` 是配置缺陷，猜成 basic 会让将来的第二种协议静默改变既有部署的语义。
3. `enabled = false` 时本节整体跳过（不校验、不推导，`kind` 随整段一起丢弃）。

### 3.6 安全启动门禁（Fail-Closed）

终态裁决是阶段三的最后一步，按顺序执行（各分支触发条件互斥，不重叠）：

1. `enabled == false` → 丢弃整个 `[metrics.auth]`（无论其内容是否完整、`type` 是否合法），终态 `credentials = None`，**不校验任何凭据字段**。合法配置示例：`enabled = false` 且不写 auth；以及"禁用 + 写了半截 auth（例如只写 `username`）"——后者被安全忽略，不阻断主业务启动。
2. `enabled == true` 且凭据既不在文件也不在环境变量里（阶段二后 `username`/`password` 至少有一个仍是 `None`） → `Err`：`"metrics.auth is required when metrics.enabled is true"`。
3. `enabled == true` 且凭据齐备但 `kind` 非法或缺失且未由环境变量推导 → 由 §3.5 拦截，返回 `"metrics.auth.type must be \"basic\""`。
4. `enabled == true` 且 `kind == "basic"`，但 `username`/`password` 为空串或含非法字符 → 由 §3.4 的字段校验拦截，返回更具体的 `"metrics.auth.username is invalid"` 或 `"metrics.auth.password is invalid"`（同一条件只有一个消息，不重复报告）。
5. 全部通过 → `credentials = Some(BasicCredentials { username, password })`。

空凭据的可达路径唯一：`[metrics.auth]` 里写了空串，命中分支 4。环境变量侧的空串按"未设置"处理（§3.3 第 3 步），因此要么整个凭据缺失（分支 2），要么在 §3.3 第 3 步就被 partial 拒绝；`NotUnicode` 则在第 3 步就报错（§3.4 规则 7）。三条路径互斥，不存在同一条件对应两个消息的情形。

分支 2、3、4 即需求的核心安全门禁：**启用指标但没有完整、合法的凭据，服务在启动期直接拒绝启动**，错误经 `main()` 的 `?` 上抛，进程不绑定端口。所有上述固定消息 MUST NOT 含用户名、密码、路径或文件内容；测试以断言"`err.message()` 不含配置里写入的密码字面量"（§7.2）反向保证。

### 3.7 配置文件的安全处理

1. 文件权限建议 `0600`、属主为服务账户；本模块不 `stat` 既有文件的权限位，也不因权限位不符合预期而拒绝启动（配置文件由部署方创建，与 `bootstrap` 首次落盘初始密码时用 `create_new` + `mode(0o600)` 主动设定权限的场合不同，此处没有可创建动作可加固）。
2. 凭据只存在进程内存（`MetricsConfig` → `Arc<MetricsConfig>`）；MUST NOT 写入数据库、`audit_events`、任何日志 target、`Data`/`Debug` 输出。脱敏由**手写 `impl Debug for MetricsConfig`** 保证（§3.2：凭据字段渲染为固定 `"[redacted]"`，不含用户名、密码与长度），MUST NOT 依赖"字段类型恰好没实现 `Debug` 所以派生不出来"这种脆弱推导；`BasicCredentials` 自身 MUST NOT 实现 `Debug`，也 MUST NOT 实现 `PartialEq`。
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
| CSRF（`X-CSRF-Token`） | 写请求必须 | 不适用（端点只接受 GET/HEAD，无写语义） |
| 限速 | 读 50/s、写 20/s 每会话 | 不参与会话限速；抓取并发由 §5.5 信号量约束 |
| 会话/CSRF 表写入 | 是 | 零写入 |
| 审计 | 管理写事务内审计 | 不写 |
| `Set-Cookie` | 登录/登出 | 任何响应都 MUST NOT 携带 |

**不套用 `HttpSecurityConfig` 的 Host/Origin 白名单是刻意决策**，不是遗漏：`http_gate::authenticate_read` 的第一步就是把 `Host` 与 `HttpSecurityConfig::allowed_hosts` 精确比对，不在白名单一律 403（`origin_host_invalid`），而 Prometheus 抓取请求的 `Host` 是抓取目标地址（可能与 `CONTROLLER_ALLOWED_HOSTS` 中登记的浏览器可达地址不同），且监控客户端不发送 `Origin`。若强行套用会话边界，跨主机抓取会恒 403，而为了让抓取通过而放宽 `ALLOWED_HOSTS` 又会削弱浏览器侧防护——两个安全域耦合是净损失。指标线的保护由 Basic Auth + §8 部署约束（默认监听 `127.0.0.1:8080`，对外暴露交由反向代理/防火墙并在其上另配 TLS 与来源限制）承担。

类型层面的结构性隔离（I3 的可编译表达）：`MetricsState`、`MetricsConfig`、handler 签名中都不存在 `Arc<HttpAuthState>`/`Session`/`EndpointId`，因此指标线**无法**触碰会话上下文；§7.6 用行为测试（携带完全合法会话 Cookie 仍得 401）反向验证。

### 4.2 `Authorization` 头解析

固定步骤，任何一步失败 → 401（同一个响应形状，不做区分）。步骤 1–6 由公开纯函数承载（L1 单测直接调用它，不依赖 HTTP 栈）：

```rust
// metrics/auth.rs

/// `Authorization: Basic` 的解析结果。**拥有所有权**：base64 解码与 UTF-8 校验
/// 产生的字符串只在解析函数内部存在，任何 `Option<(&str, &str)>` 形态都会借用
/// 一个随函数返回而销毁的临时缓冲（`Vec<u8>`/`String`），要么编译不过，要么靠
/// 把整张 `HeaderMap` 一路带调用链来续命。切分点又落在**解码后**的字节上而非
/// 头部原值上，因此终态只能是 owned。
pub struct ParsedBasic {
    pub username: String,
    pub password: String,
}

pub fn parse_authorization(headers: &HeaderMap) -> Option<ParsedBasic>;
```

`ParsedBasic` 不派生 `Debug`（凭据不外泄，§3.7），不派生 `PartialEq`（防用短路 `==` 比较凭据，§4.3）；只在单次请求内存活，MUST NOT 被缓存、写入日志或放进错误消息。

1. `single_header(headers, header::AUTHORIZATION)`：缺失、出现多次（任意多于一个值）、非 ASCII 值一律 `None`。复用 `http_gate` 中同名 helper 的**语义**（"重复头绝不简化为取第一个"）；因该 helper 当前是 `pub(crate)`，指标线可直接调用（同 crate 内），MUST NOT 复制一份实现。
2. 取 `&str`，按第一个空格切分：`scheme` 与 `rest`。要求两段都存在且 `rest` 非空。
3. `scheme` 必须 ASCII 大小写不敏感等于 `basic`（RFC 7617 §2.2 scheme 不区分大小写）。`rest` 内部 MUST NOT 含空白、换行或前导 `=`。
4. `base64::engine::general_purpose::STANDARD.decode(rest)`：解码失败（含 padding 缺失、尾部比特残留、非法字符）→ `None`。
5. 解码结果按 **UTF-8** 严格解码，失败 → `None`。
6. 对解码后的字符串调用 **`split_once(':')`**：仅**第一个**冒号是分隔符，其之前是 `username`、其后**全部**字符（可再含冒号）是 `password`。
   - 没有冒号 → `None`；`username` 为空串 → `None`（配置期已禁止空用户名与含冒号用户名，§3.4）。
   - `password` **允许为空串**（`"user:"` 解析成功，交由 §4.3 判否），也**允许包含任意个冒号**：`"u:a:b:c"` → `username = "u"`、`password = "a:b:c"`。MUST NOT 使用 `split(':')`、`rsplit_once(':')`、`splitn(2..)` 之后再校验"剩余部分不含冒号"之类的写法——那会拒绝掉 RFC 7617 明确允许的密码，并把合法抓取方永久锁在 401 之外。
7. 得到 `ParsedBasic` 后进入 §4.3 校验；**校验值之外的一切解析失败都不区分错误码**，统一 401 且响应体为空。

### 4.3 常量时间凭据比较

```rust
// metrics/auth.rs（签名固定，便于单测直接调用）
pub fn verify(user: &str, password: &str, expected: &BasicCredentials) -> bool;
fn sha256(bytes: &[u8]) -> [u8; 32];
fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool;   // subtle::ConstantTimeEq 的薄封装
```

实现与语义约束：

1. `verify` 先比较**用户名**、再比较**密码**，两次比较**都必须执行**，结果以按位与合并：

   ```rust
   let ok_user = ct_eq_32(&sha256(user.as_bytes()), &sha256(&expected.username));
   let ok_password = ct_eq_32(&sha256(password.as_bytes()), &sha256(&expected.password));
   ok_user & ok_password
   ```

   MUST NOT 写成 `if !user_ok { return false; } check_password()` 形式：短路会在用户名上建立"用户名是否正确"的时序判定器。handler 侧的调用形态是 `verify(&parsed.username, &parsed.password, credentials)`——`ParsedBasic` 是 owned 值（§4.2），借用只活在这一次请求里。
2. `ct_eq_32` MUST 直接落在 `subtle` 的标准原语上（`subtle 2.6.1` 已在 `Cargo.lock`，§3.1 把它提升为直接依赖），MUST NOT 手写 `diff |= a[i] ^ b[i]` 循环：

   ```rust
   use subtle::ConstantTimeEq;

   fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
       a.ct_eq(b).into()
   }
   ```

   理由：`subtle` 的 `Choice` 与 `black_box` 语义是对**编译器优化后机器码**的抗侧信道保证（抑制提前返回、短路折叠与向量化早退），且经社区审计；手写的 XOR/OR 折叠在 `opt-level` 提升、LLVM 把循环识别为 `memcmp` 或做提前逃逸分析后不保证仍等价，属于"看起来常量时间"的实现。本仓库 `auth/session.rs` 的 `constant_time_eq_32` 与 `tasks/http.rs` 的 `constant_time_eq` 属既有实现，本期不改，但指标线 MUST NOT 新增第四份手写折叠。
   仍 MUST NOT 使用 `==`、`starts_with`、`slice::eq`、`str::contains` 等可短路/可提前的比较。
3. **先哈希后比较**的目的：把输入长度归一为固定 32 字节，从而消除"长度不同提前返回"与逐字节短路两类长度/内容侧信道；副产品是 `ct_eq` 的输入长度成为编译期常量，落在 `subtle` 为 `[u8; N]` 提供的实现上。
4. 诚实登记残余泄漏面（不假装成完美常量时间）：哈希本身的耗时随输入长度线性增长；§4.2 的 base64/UTF-8 解析失败会在凭据比较之前提前返回 401（提前量只与"提交的是否是合法 base64 形状"有关，与凭据内容无关）。二者的可利用性远低于网络抖动，属可接受残余，记录在 §8。
5. `verify` MUST NOT 记录日志、MUST NOT 返回值区分"用户名错"与"密码错"。

### 4.4 401 响应构造

- 状态码 `401 Unauthorized`。
- `WWW-Authenticate: Basic realm="rsetup-controller-metrics"`（逐字节固定；不携带 `charset` 参数——Prometheus 官方 `basic_auth` 按 UTF-8 发凭据，不带参数是最大兼容选择）。
- **空响应体**、无 `Content-Type`。MUST NOT 复用 `http_api::err_response` 的 JSON 信封：指标线是独立边界，向监控客户端吐业务错误信封既无必要也会诱导前端误用该端点。
- MUST 携带 `Cache-Control: no-store` 与 `X-Content-Type-Options: nosniff`（与 200/503 一致，便于测试固定响应头集合）。

### 4.5 状态码与拦截矩阵

GET 与 HEAD 是**同一认证域**的两种方法（§6.2 说明 Axum 的 HEAD 派发事实）；矩阵里"合法凭据"的行对二者除响应体外完全一致。

| 条件 | 状态码 | 响应体/头 | 是否读凭据 | 是否触达数据源 |
| --- | --- | --- | --- | --- |
| `enabled = false`（GET 或 HEAD） | `404 Not Found` | 空体、无 `Content-Type`、无 `WWW-Authenticate`（与路由未注册不可区分，不宣告端点存在） | 否 | 否 |
| `enabled = true`、缺 `Authorization` | `401` | §4.4 | 是（解析失败） | 否 |
| `enabled = true`、scheme/base64/UTF-8 非法、无冒号或用户名为空 | `401` | §4.4 | 是 | 否 |
| `enabled = true`、凭据不匹配 | `401` | §4.4 | 是（两次比较都执行） | 否 |
| GET、合法凭据、采集成功 | `200` | §5.1 | 是 | 是 |
| HEAD、合法凭据 | `200` | **空体** + §5.1 的三个响应头（`Content-Type` 常量、`Cache-Control: no-store`、`X-Content-Type-Options: nosniff`）；不占并发闸 | 是 | **否**（§6.2 HEAD 短路） |
| GET、合法凭据、采集失败/超时/并发闸满/渲染超上限 | `503` | 空体 + `Cache-Control: no-store` + `Retry-After: 5` | 是 | 是（已失败） |
| HEAD、采集类失败不适用 | — | HEAD 不采集，因此不会因采集失败产生 503；仅可能 401/404 | — | — |
| 其他方法（`POST`/`PUT`/`PATCH`/`DELETE`/`OPTIONS` 等） | `405` | 由 Axum 方法匹配给出，并带 `Allow: GET, HEAD`（禁用态亦如此，见 §8 残余风险 R5） | 否 | 否 |
| 其他路径 | `404` | 顶层 router 默认未匹配响应 | 否 | 否 |

handler 内部顺序 MUST 固定为：①`enabled` 门禁 → ②Basic 认证 → ③`HEAD` 短路（直接 200 空体，不取并发闸、不采集）→ ④并发闸 `try_acquire` → ⑤快照采集（含超时与连接防护，§5.5）→ ⑥渲染与上限校验 → ⑦响应。（HTTP 方法匹配由路由层在此之前完成；`HEAD` 之外没有第二种方法能进入 handler。）先判 `enabled` 再判认证，保证禁用态无论是否携带凭据都恒 404；认证先于 HEAD 短路，保证 HEAD 绝不在未鉴权下触达任何数据源。

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
6. 标签值转义（本模块唯一一处）**严格按 Prometheus exposition format（text 0.0.4）legacy 文本规范只处理三个字符**：`\` → `\\`、`"` → `\"`、换行 `0x0A` → `\n`。MUST NOT 转义 `\r`、`\t` 或其他控制字符——该格式的转义表里没有这两项，额外转义会产出 `Foo\rBar` 之类的非规范样本行，让严格的抓取端解析失败或让 golden 测试与参考实现不一致（当前所有标签值都来自 §5.2 的固定词汇与 §5.2.1 的构建常量，正常路径碰不到这些字符，正因如此更不该在转义表里写不存在规则）。实现仍 MUST 经过该转义函数，不得依赖"输入总是安全的"。
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

本端点只聚合两张表：设备表 `devices` 与主任务表 `main_tasks`。同库另有子任务表 `sub_tasks`（`migrations/0004_tasks.sql` 的真实表名，`tests`、`integrity.rs` 与 `db.rs` 均用该名）：本文、实现与测试 MUST 一律写作 `sub_tasks`，且本端点**不统计**子任务（`rsetup_controller_tasks_total` 恒等于 `main_tasks` 的行数分布，不含子任务行）。

**设备准入**（`devices.admission_state`，`VARCHAR(16) NOT NULL`，字符集 `utf8mb4`）：

| DB `admission_state` | 指标标签 `admission` | 说明 |
| --- | --- | --- |
| `APPROVED` | `APPROVED` | 已准入 |
| `PENDING` | `PENDING` | 待审批 |
| `REVOKED` | `REJECTED` | 未获准入（含先批准后吊销） |
| 其他任意字符串 | **fail-closed 503** | 绝不静默归入任一桶（§5.3 的二进制分组保证这类值真的成为独立分组，而不是被大小写不敏感校对吞并） |

命名映射是刻意选择并在本文留档的偏差：持久层用 `REVOKED` 描述"当前不处于准入态"（`AdmissionState::Revoked`），监控侧统一用 `REJECTED` 表达"未获准入"这一告警语义，使告警规则可以写 `rsetup_controller_devices_total{admission="REJECTED"}` 而不必了解内部 `REVOKED`/`Denied` 的状态机细节。映射 MUST 在 Rust 侧纯函数完成（`fn admission_label(raw: &str) -> Option<AdmissionBucket>`），MUST NOT 依赖 SQL 端的隐式转换。

**事实修正（必读）**：`chk_devices_state CHECK (admission_state IN ('PENDING','APPROVED','REVOKED'))` 只存在于最初的 `0001_identity_devices.sql`。**v3（`0003_identity_application_integrity.sql`）重建 `devices` 表时不再携带任何 CHECK 约束**，且 v3 迁移路径显式执行 `ALTER TABLE devices DROP CHECK chk_devices_state`（`db.rs` 的 `LEGACY_CHECK_DROPS` 白名单之一）。因此现网 schema 里**没有**数据库层的三值约束，异常枚举值（历史脏数据、旁路写入、大小写变体）在数据库侧完全可能落库：枚举防御只有"SQL 二进制分组 + Rust 显式穷举映射 + 未知值 503"这一条路，MUST NOT 以"DDL 已经约束了"作为省略防御的理由。同理，v4 任务表按 `0004_tasks.sql` 文件头约定"五张表、无外键、无 CHECK 约束"。

**主任务状态**（`main_tasks.state` / `main_tasks.outcome`，v4 表不建 CHECK 约束，故 MUST 用显式穷举映射；`state` 取值域按 `2026-09-23-controller-v1-04-task-lifecycle.md` 为 `queued`/`running`/`held`/`completed`，`outcome` 为 `success`/`cancelled`/`unknown`/`partial`/`failed` 或 `NULL`）：

| `state` | `outcome` | 指标标签 `status` |
| --- | --- | --- |
| `queued` | 任意（非终态忽略该列，`integrity.rs` 另有"非 completed 必须 NULL"的独立不变量） | `pending` |
| `held` | 任意（同上） | `pending` |
| `running` | 任意（同上） | `running` |
| `completed` | `success` | `succeeded` |
| `completed` | `failed`/`partial`/`cancelled`/`unknown` | `failed` |
| `completed` | `NULL` | `failed`（终态却缺 outcome 属契约外数据；确定为未成功而非整次抓取失败，避免历史脏数据使监控端点不可用） |
| `completed` | **不属于上述 5 个已知值且非 `NULL`** 的任意字符串（含大小写变体，如 `Success`） | **fail-closed 503**（`task_bucket` 返回 `None`） |
| 其他任意字符串（`state` 不在四值域内，含大小写/pad 变体） | — | **fail-closed 503**，绝不静默归入任一桶 |

未知 `outcome` 与未知 `state` 一样是 fail-closed：把没见过的终态原因猜成 `failed` 或 `succeeded` 都是伪造数据。区分两条去敏消息：`task_bucket` 返回 `None` 时，若 `state` 本身不在四值域内 → `ControllerError::Config("metrics task aggregate: unknown task state")`；若 `state` 合法但 `outcome` 不在枚举内 → `ControllerError::Config("metrics task aggregate: unknown task outcome")`。两条消息都 MUST NOT 回显数据库里的原始取值（I5）。

纯函数 `fn task_bucket(state: &str, outcome: Option<&str>) -> Option<TaskBucket>` 承载该表；`("completed", None) → Some(Failed)` 是唯一的 `NULL` 分支，其余 `Option::None`（非终态传 `None`）按"任意"处理。

`held`（等待人工 time-review 的暂停态）并入 `pending`：两者都是"尚未在执行中"的非终态，四桶标签由对齐设计固定为 `pending|running|succeeded|failed`，并入可保证四条序列求和恰等于表行数（若另立第五桶则违反 §1.3 的固定标签域）。

### 5.3 采集与查询

两条聚合查询 + 一次内存读取，全部在 `metrics/collect.rs`。分组 MUST 使用**二进制校对语义**（逐字节区分大小写、不做 `NO PAD`/`PAD SPACE` 尾部空格折叠）：

```sql
-- 设备准入分布（二进制分组：'APPROVED' 与 'approved'/'APPROVED ' 是两个独立分组）
SELECT CAST(admission_state AS CHAR) COLLATE utf8mb4_bin AS bucket, COUNT(*) AS n
FROM devices
GROUP BY CAST(admission_state AS CHAR) COLLATE utf8mb4_bin;

-- 主任务状态分布（state 与 outcome 两列都按二进制分组）
SELECT CAST(state AS CHAR) COLLATE utf8mb4_bin AS state,
       CAST(outcome AS CHAR) COLLATE utf8mb4_bin AS outcome,
       COUNT(*) AS n
FROM main_tasks
GROUP BY CAST(state AS CHAR) COLLATE utf8mb4_bin, CAST(outcome AS CHAR) COLLATE utf8mb4_bin;
```

约束：

1. 两条查询 MUST 在**同一条池连接**上顺序执行（同一连接不保证跨语句快照，因此下面第 3 条明确语义边界），MUST NOT 用 `query!` 宏（本仓库不使用编译期 SQL 宏，保持与 `db.rs`、`tasks/repository.rs` 一致的 `query_as` 运行期写法）。
2. **为什么必须二进制分组**：`utf8mb4` 的默认排序规则大小写不敏感且 pad-space，`GROUP BY admission_state` 会把 `APPROVED` 与 `approved` 折叠成一组、把尾部空格视为相等，于是 §5.2.2 依赖的"未知枚举值 → 独立分组 → Rust 映射失败 → 503"这条 fail-closed 链路在 SQL 层就被吞掉，脏数据被静默计入合法桶（伪造 PASS）。写法上 MUST 让**选择列表里的表达式与 `GROUP BY` 表达式逐字符相同**（即两侧都写 `CAST(... AS CHAR) COLLATE utf8mb4_bin`），否则在默认开启 `ONLY_FULL_GROUP_BY` 的 MySQL 8/TiDB 上会因表达式不同而报 1055。`COLLATE` 只影响比较/分组，不改变返回的字节值，`bucket` 拿到的仍是原始字符串。
   - 不采用 `GROUP BY BINARY admission_state`/`GROUP BY CAST(admission_state AS BINARY)`：`BINARY` 运算符在 MySQL 8.0 已弃用（产生 deprecation 告警），且同样触发"选择列与分组表达式不一致"的 1055 风险；语义等价、可移植性更好的拼法是显式 `COLLATE utf8mb4_bin`。
   - 该 SQL 的确切拼法 MUST 由 §7.7 的 L4 用例在 MySQL 与 TiDB 双引擎上实测通过（含大小写变体不折叠的断言）；任一引擎不接受该写法时，MUST 修改本规范并重新评审，MUST NOT 静默退化为无 `COLLATE` 的写法。
3. `COUNT(*)` 以 `i64` 接收，再 `u64::try_from`；负值（不可能出现）→ fail-closed 503。`CAST(... AS CHAR)` 与本仓库既有查询写法一致，固定字符串类型不受会话字符集影响。
4. **语义边界**：两条聚合之间不做事务，因此两个族之间允许存在毫秒级偏差。该指标语义是"抓取时点的近似瞬时 gauge"，MUST NOT 为此持有长事务或 `SERIALIZABLE` 快照；如需强一致对账，走 SQL 而非本端点。
5. 连接池 gauge 直接读内存：`active = pool.size().saturating_sub(pool.num_idle() as u32)`、`idle = pool.num_idle()`（`sqlx::Pool::size()` 返回当前打开连接总数、含空闲；`num_idle()` 返回空闲数；两者都以 `u64` 渲染），MUST NOT 为此发起任何连接或查询，两条序列永远可得、永不变错。`state="max"` 本期不输出（`Pool::options().get_max_connections()` 可用，留待后续需要 `饱和度` 告警时另立规范）。
6. `Pool::size()` 为 0（例如尚未建立任何连接的 lazy 池）时输出 `active=0, idle=0`，这是"当前没有任何打开连接"的真实表达，不是采集失败。

### 5.4 命名规范偏差说明

Prometheus 官方命名约定把 `_total` 后缀留给 counter。本设计的 `rsetup_controller_devices_total`、`rsetup_controller_tasks_total` 与对齐设计指定的固定名保留 `_total`，但**类型恒为 gauge**（当前计数快照，不是单调累计量，重启会下降）：

1. 每个族的 `# TYPE ... gauge` 行 MUST 输出，Prometheus 解析器据此按 gauge 建模，不会生成 `rate()` 可用的 counter 语义。
2. 该偏差 MUST 在指标 `HELP` 文本与 §5.2 清单同时留档：`HELP` 明确"sum over all labels equals the table row count"，把"这是一组可求和的分布快照"写进抓取方可见的元数据。
3. 未来若需要真正的累计量（例如抓取次数、认证失败次数），MUST 使用不带 `_total` 冲突语义的新指标名并另立规范。

### 5.5 采集模型、并发与失败语义

- **Pull 模式**：只在请求到达时采集，MUST NOT 启动后台定时采集任务、MUST NOT 缓存上一次响应（同一路径上的两次抓取允许得到不同取值，这是 gauge 的正常语义）。
- **取值装配**：`collect.rs` 把 `MetricsSnapshot`（两条 DB 聚合）+ `MetricsRuntime`（构建常量与单调 uptime）+ 连接池内存读数（`PoolCounts`）合成 `model::ScrapeValues`，再交 `render::render`；`ScrapeValues` 是渲染层唯一的输入形态（§7.4 的 golden 测试直接构造它，不需要 DB）。
- **并发闸**：`Arc<tokio::sync::Semaphore>`，容量 `pub const MAX_CONCURRENT_SCRAPE: usize = 4`（定义在 `metrics/collect.rs`，`http.rs` 与 `production.rs` 统一按 `metrics::collect::MAX_CONCURRENT_SCRAPE` 引用，不重复声明字面量），handler 用 `try_acquire_owned`；已满 → 立即 503（不排队、不占连接），防止 Prometheus 抓取风暴把业务连接池挤占。`tokio` 的 `sync`/`time` feature 已由工作区声明，无需新增 feature。
- **超时**：两条聚合查询整体包在 `tokio::time::timeout(SCRAPE_TIMEOUT, ...)` 内，`pub const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2)`（同处 `collect.rs` 定义）；超时 → 503。该上限小于常见 Prometheus `scrape_timeout`（10s），使失败由本进程先判定，而不是让抓取端观察到悬挂连接。单次抓取的最坏墙钟耗时是 `POOL_ACQUIRE_TIMEOUT + SCRAPE_TIMEOUT = 2.5s`（§5.5.1），仍在 10s 之内，因此 handler 外层 MUST NOT 再叠加第三层超时（叠了只会产生第二种失败形态而无法定性）。**该超时只是必要条件，不是充分的池保护**，见 §5.5.1。
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
      /// 只接受**既有**池的 Clone；实现内部按 §5.5.1 执行 acquire 超时 + close_on_drop。
      /// 生产装配只能由 `production.rs` 以 `inputs.auth.db` 调用（§6.3），
      /// `main.rs` 与任何业务线 MUST NOT 自行构造它再作为入参传入。
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

#### 5.5.1 数据库连接防护规则（池共享下的强制约束）

指标线与 auth/tasks 共用同一个 `DbPool`（I2），因此指标慢查询**首先**是业务可用性问题，其次才是观测成本问题。`sqlx` 与 `sqlx-core 0.8.6` 的以下事实是本节规则的根据，实现与评审 MUST 逐条对照：

1. **`tokio::time::timeout` 只取消外层 Future**。被取消的只是"等待查询完成"的那段 await；池连接由 `PoolConnection` 持有，其 `Drop`（`sqlx-core-0.8.6/src/pool/connection.rs`）会 `rt::spawn(...)` 一个后台任务去做归还或关闭，该任务**不受**已返回的 `timeout` 影响。默认路径 `return_to_pool()` 会先 `raw.ping()` 以"测试连接并 flush 掉因取消而残留的结果"（源码注释明确写了这是取消后状态不一致的 band-aid），**成功后**才 `PoolInner::release()` 释放池槽位的 semaphore permit。也就是说：外层已经 503 了，池槽位仍被这条慢/脏连接占着，直到数据库把余下结果吐完或 ping 失败。慢查询连续发生时，指标线就会持续吃掉共享池的连接。
2. **`acquire()` 默认超时是 30 秒**（`sqlx-core-0.8.6/src/pool/options.rs` 的 `PoolOptions::default().acquire_timeout`，而 `DbPool::connect` 使用 `MySqlPool::connect` 的默认参数，本期不改其池参数以免牵动业务线）。指标线若直接 `acquire()`，在池被打满时会排队 30 秒，与 2 秒抓取上限完全脱节。

据此，`SqlxMetricsSnapshotSource::snapshot()` MUST 按以下顺序实现（三条都是硬门禁）：

```rust
// metrics/collect.rs
/// 借连接的最长等待：池紧张时快速失败，绝不排队 30s。
pub const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(500);
/// 两条聚合的整体执行上限（§5.5）。
pub const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn snapshot_via(pool: &DbPool) -> Result<MetricsSnapshot, ControllerError> {
    // 1) 显式 acquire 超时：500ms 借不到连接 → 立即失败（映射 503）。
    let mut conn = tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, pool.0.acquire())
        .await
        .map_err(|_| ControllerError::Config("metrics snapshot: pool acquire timeout"))?
        .map_err(|_| ControllerError::Config("metrics snapshot: pool acquire failed"))?;

    // 2) 借到即标记为"用后即弃"：任何取消/出错路径都走 take_and_close()
    //    （sqlx-core 0.8.6 的 CLOSE_ON_DROP_TIMEOUT = 5s 有界关闭），
    //    而不是 return_to_pool() 的 ping-drain-再归还。
    conn.close_on_drop();

    // 3) 两条聚合在上限内顺序执行；超时/错误都让 conn 在此处离开作用域。
    tokio::time::timeout(SCRAPE_TIMEOUT, run_aggregates(&mut conn)).await
        .map_err(|_| ControllerError::Config("metrics snapshot: query timeout"))?
}
```

1. **显式 `acquire` 超时**：`POOL_ACQUIRE_TIMEOUT = 500ms`（MUST NOT 大于 1s，且 MUST 明显小于 `SCRAPE_TIMEOUT`），超时映射 503。这让"池紧张"在 0.5 秒内就是可见的 503，而不是 2 秒后仍无法定性的超时。
2. **借到连接立即 `close_on_drop()`**：取消发生在查询中途时，这条连接的会话状态已不可信，`close_on_drop` 让 `Drop` 走 `take_and_close()`（`Floating::close()` 外包 `rt::timeout(CLOSE_ON_DROP_TIMEOUT = 5s, ...)`），**销毁**该连接而不是 ping-drain 后放回空闲队列。效果上：超时/取消绝不把"还在为上一条慢查询吐结果"的连接重新交给业务线使用，池槽位也随关闭尽快释放回补。MUST NOT 依赖 `Pool::disconnect_idle()`（只处理空闲连接，对借出中的连接无效），也 MUST NOT 用 `PoolConnection::detach()`（detach 会让进程实际超出 `max_connections`）。
3. **诚实登记残余占用**：`Drop` 派生出的后台关闭任务在 `timeout` 之后仍会运行至多 `CLOSE_ON_DROP_TIMEOUT`（5 秒）才释放 permit，`tokio` 层无法把它提前。占用面由并发闸封顶：至多 `MAX_CONCURRENT_SCRAPE = 4` 条抓取各自持有一条待销毁连接，而池的默认 `max_connections = 10`，因此最坏情况下业务线仍有 ≥ 6 个可用槽位；该项登记为 §8 的 R11。若压测出现实测排队，处置方向是"降低抓取频率/另立规范引入指标缓存或独立只读副本"，MUST NOT 改为"指标线自建第二个池"（违反 I2）。
4. **服务端兜底（SHOULD）**：两条聚合 SHALL 附带 `/*+ MAX_EXECUTION_TIME(...) */` 优化器提示（取值 `SCRAPE_TIMEOUT` 的毫秒数）作为服务端侧的兜底，让数据库主动终止慢聚合、缩短待关闭连接的 drain 时间；该提示 MUST 在 §7.7 双引擎实测确认"被接受且生效"后方可保留，任一引擎拒绝或无效时删除提示而不改变其余语义（客户端 `close_on_drop` 才是硬保证）。
5. 超时与取消路径 MUST NOT 记录原始 SQL 参数以外的任何数据（本查询无参数），MUST NOT 重试：一次抓取失败就是 503，重试只会把同一个慢数据库压得更慢。

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
| `metrics/config.rs` | TOML 形状（Raw DTO）、三阶段加载、路径与环境变量覆盖、统一门禁与启动 fail-closed（§3） | `MetricsConfig::{from_env, from_parts_with, from_parts, parse_file}`、`MetricsAuthRaw`、`EnvRead`、`BasicCredentials`、`RESERVED_PATHS`/`is_reserved`、§3.2 常量 |
| `metrics/auth.rs` | Basic 头解析（owned 结果）+ `subtle` 常量时间比较（§4.2/§4.3） | `ParsedBasic`、`verify`、`parse_authorization` |
| `metrics/model.rs` | 指标数据模型、标签词汇、纯映射函数（§5.2） | `ScrapeValues`、`DeviceCounts`、`TaskCounts`、`PoolCounts`、`admission_label`、`task_bucket` |
| `metrics/render.rs` | 0.0.4 文本渲染纯函数与格式常量（§5.1） | `CONTENT_TYPE`、`MAX_BODY_BYTES`、`render` |
| `metrics/collect.rs` | 采集：DB 汇总源（含 §5.5.1 池防护）、连接池内存读取、运行时量、并发闸与超时（§5.3/§5.5） | `MetricsSnapshotSource`、`SqlxMetricsSnapshotSource`、`MetricsRuntime`、`MetricsSnapshot`、`MAX_CONCURRENT_SCRAPE`、`SCRAPE_TIMEOUT`、`POOL_ACQUIRE_TIMEOUT` |
| `metrics/http.rs` | 状态、路由与 handler（§4.5 顺序，含 HEAD 短路） | `MetricsState`、`build_metrics_router` |

依赖方向 MUST 单向：`http → {auth, collect, render, config}`、`collect → model`、`render → model`；`config` 与 `model` 不依赖 axum/sqlx 之外的模块，`render`/`model` 为纯函数层（无 I/O、无 async），保证 §7 的表驱动单测不需要 DB 或 HTTP。

### 6.2 HTTP 层

```rust
// metrics/http.rs
#[derive(Clone)]
pub struct MetricsState {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
    pub snapshot: Arc<dyn MetricsSnapshotSource>,
    /// 只用于 §5.3 第 5 条的内存读取（`size()`/`num_idle()`），永不发起查询。
    pub pool: DbPool,
    pub guard: Arc<tokio::sync::Semaphore>,
}

/// 指标线子 router：按配置路径注册恰好 1 条 GET 路由（Axum 0.8 隐含同时接受 HEAD，见下）。
pub fn build_metrics_router(state: MetricsState) -> Router<()>;

/// 入参固定为三件；`method` 用于认证之后的 HEAD 短路（§4.5 顺序）。
async fn scrape(
    State(state): State<MetricsState>,
    method: Method,
    headers: HeaderMap,
) -> Response { /* … */ }
```

1. 路由注册 `state.config.endpoint` 上的 `get(scrape)`，MUST 始终注册（含 `enabled = false` 时），由 handler 依 §4.5 顺序返回 404；不采用"禁用即不注册"的方式，这样禁用态响应与路径配置项完全无关，也不受后续合并顺序或前端 SPA 白名单演变影响。
2. **Axum 0.8 的 HEAD 事实（先厘清再定策略）**：`MethodRouter::get(handler)` 注册的 `MethodFilter::GET` 同时把 `HEAD` 也指向该 handler（`axum-0.8.9/src/routing/method_routing.rs` 中 GET 端点登记的允许方法是 `["GET", "HEAD"]`，其自带测试 `get_accepts_head` 断言 HEAD 得 200 且 body 为空；显式 `.head()` 优先于 GET）。命中后 handler **完整执行**，响应离开顶层路由时由 axum 先按 `size_hint` 写入 `Content-Length`、再把 body 替换为空（`axum-0.8.9/src/routing/route.rs`）。响应还会带 `Allow: GET, HEAD`。结论：**只写 `get()` 绝不等于"HEAD 返回 405"**——HEAD 会走到 handler，若不特殊处理就会为一次探测执行两条聚合查询（body 被丢弃，但数据库开销全额发生）。
3. **本端点对 HEAD 的策略（显式选择，不靠框架默认）**：允许 HEAD，且
   - HEAD 与 GET 共用**同一条认证与门禁路径**（先 `enabled`、后 Basic 认证），凭据缺失/不匹配一律 401，形状与 GET 完全一致；禁用态一律 404。
   - 认证通过后 HEAD **短路返回 200 空体** + §5.1 的三个响应头，MUST NOT 获取并发闸、MUST NOT 调用 `snapshot()`/连接池/数据库：HEAD 的用途是"探活 + 检查认证配置"，为一个被丢弃的 body 支付两次 `GROUP BY` 是纯粹的可用性风险。
   - 短路只可能发生在认证之后，因此不存在"未鉴权即触达数据源"的窗口；`Content-Length: 0` 是可观察行为（axum 先设长度后置空 body）。
   - MUST NOT 用独立的 `.head(handler)` 注册第二条 handler 实现：单 handler + `Method` 分支才能保证两个方法的认证与门禁不会各自漂移。
4. handler 不接收任何业务 extractor：入参固定 `(State<MetricsState>, Method, HeaderMap)`，MUST NOT 使用 `Path`/`Query`/`Json`，从而不存在"提取器先于门禁短路"的历史问题（`http_device_status` 已为此改用 `Result<Path<_>, PathRejection>`，本端点直接不引入路径参数）。
5. 成功响应由 `(StatusCode::OK, [(CONTENT_TYPE, ...), ("cache-control", ...), ("x-content-type-options", ...)], body)` 构造（HEAD 走同一构造，只是 `body` 为空），除 §5.1 列出的三个头之外不附加任何业务响应头。
6. 本端点 MUST NOT 写 `tracing` 日志（含 401）：抓取的正常节奏会淹没日志，失败已可由 Prometheus 的 `up == 0` 与 `scrape_duration_seconds` 观测。

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

/// 指标线的装配输入：**只有配置与运行时量两项**。
/// 刻意不包含 `snapshot`、`DbPool` 与 `Arc<HttpAuthState>`：生产汇总源只能在
/// `build_production_router` 内部由 `inputs.auth.db` 构造（§6.3 末尾），顶层入参
/// 里没有"把一个外部自建的池塞进指标线"的入口，I2 由类型结构而非运行期检查保证。
pub struct MetricsLineInputs {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
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
    // 生产唯一入口：汇总源只能在函数内部由 auth 线的池构造。
    build_router(inputs, None)
}

/// 唯一实现：`snapshot_override` 只允许测试注入（见下），`None` 即真实源。
fn build_router(
    inputs: ProductionInputs,
    snapshot_override: Option<Arc<dyn MetricsSnapshotSource>>,
) -> Result<Router<()>, ControllerError> {
    if !Arc::ptr_eq(&inputs.auth.auth, &inputs.management.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.tasks.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.status.auth)
    {
        return Err(ControllerError::Config("production auth context mismatch".into()));
    }
    // 连接池唯一派生点：指标线的池与汇总源都只能来自 inputs.auth.db（I2）。
    let snapshot = snapshot_override
        .unwrap_or_else(|| Arc::new(SqlxMetricsSnapshotSource::new(inputs.auth.db.clone())));
    let metrics_state = MetricsState {
        config: inputs.metrics.config,
        runtime: inputs.metrics.runtime,
        snapshot,
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

设计取舍：**不新增第四条指针校验**，而是让指标线的类型里根本没有第二个 `DbPool` 入口——配置之外的指标线装配项（汇总源、池读数）都在 `build_production_router` 内部由 `inputs.auth.db` 派生，违规在编译期不可表达，比运行期 `ptr_eq` 检查更强（这是既有"指针一致性硬门禁"在新线上的结构性替代）。

**测试注入缝（唯一的第二个入口，且无法用它引入第二个池）**：装配测试需要假汇总源（不连真库也要断言六线合并与响应形状），因此暴露

```rust
/// 仅供 `crates/rsetup-controller/tests/` 的装配测试注入合成汇总源。
/// `#[doc(hidden)]` 且 MUST NOT 被 `main.rs` 或任何生产路径调用：它接收的是
/// `Arc<dyn MetricsSnapshotSource>`（类型里没有 `DbPool`），因此即使被误用也
/// 只是替换取值来源，不会新增连接池构造点；`MetricsState.pool` 仍恒为
/// `inputs.auth.db.clone()`。
#[doc(hidden)]
pub fn build_production_router_with_metrics_source(
    inputs: ProductionInputs,
    snapshot: Arc<dyn MetricsSnapshotSource>,
) -> Result<Router<()>, ControllerError>;
```

该边界由 §7.8 的静态门禁钉住：`main.rs` 里 MUST 只出现 `build_production_router`，`SqlxMetricsSnapshotSource` 与 `build_production_router_with_metrics_source` 在 `src/` 下 MUST 只出现在 `production.rs`（`tests/` 使用注入版本不受限）。

### 6.4 `main.rs` 装配顺序

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 运行时间零点：main 函数体第一条语句即记录，包含配置解析、建池、schema 检查与
    // 绑端口在内的整个启动过程都计入 uptime（§5.2 语义"进程启动至今"）。
    let metrics_runtime = Arc::new(MetricsRuntime::new());
    let config = ControllerConfig::from_env()?;
    // 指标配置门禁：在连接数据库与绑定端口之前完成（§3.6 fail-closed 无副作用）。
    let metrics_config = MetricsConfig::from_env()?;
    let db = DbPool::connect(&config).await?;
    // …既有 secret 路径与 start_if_ready(check_identity_schema, …) 流程不变…
    let router = build_production_router(production_inputs(&db, metrics_config, metrics_runtime)?)?;
    // …TcpListener::bind 与 axum::serve 不变…
    Ok(())
}

/// 唯一生产构造点：签名扩展，新增指标线输入。**没有 snapshot 参数**（§6.3）。
fn production_inputs(
    db: &DbPool,
    metrics: MetricsConfig,
    runtime: Arc<MetricsRuntime>,
) -> Result<ProductionInputs, ControllerError> {
    // …cursor secret / 共享 Arc<HttpAuthState> / management / tasks 装配不变…
    Ok(ProductionInputs {
        auth: AppState { db: db.clone(), auth: auth.clone() },
        management,
        tasks,
        status: DeviceStatusState { auth, snapshot: Arc::new(EmptyDeviceStatusSnapshotSource) },
        web: web_router(),
        metrics: MetricsLineInputs {
            config: Arc::new(metrics),
            runtime,
        },
    })
}
```

`MetricsRuntime::new()`（`started_at = Instant::now()`）必须在 `main()` 的**第一条语句**构造，MUST NOT 留在 `production_inputs` 里：装配发生在 `start_if_ready(check_identity_schema, …)` 成功之后，把零点放在那里会让 uptime 系统性低估"进程已经活多久"（schema 检查、bootstrap、绑端口全部丢失），而 uptime 归零告警恰恰要覆盖这些阶段。运行时对象经 `production_inputs` 参数传入装配，测试仍可注入自建 `MetricsRuntime`（自定义 `started_at`）以得到确定的 uptime。

上述 `main()` 片段是顺序示意，不逐字复制闭包捕获细节：`start_if_ready` 的启动闭包按既有形态写作 `|| async move { … }` 并把 `metrics_config` 与 `metrics_runtime` 一起移入，`production_inputs` 仍返回 `Result<ProductionInputs, ControllerError>`，`?` 与错误类型链（`Box<dyn std::error::Error>`）保持现状。

### 6.5 路径保留集合与不冲突约束

`metrics.endpoint` MUST NOT 命中以下保留集合（`metrics/config.rs` 内定义 `pub const RESERVED_PATHS: &[&str]` 与 `fn is_reserved(path: &str) -> bool`，供 §3.4 校验与测试共用）：

- **完全精确匹配**（只等于该字符串本身才算冲突）：探活 `/healthz`、`/readyz`；前端精确页面 `/`、`/login`、`/password`、`/sessions`、`/devices`、`/tasks`、`/admin`（对齐 `web.rs` 的 `SPA_EXACT_ROUTES`）。
  **`/` 只按完全相等判定，绝不做前缀匹配**——任何以 `/` 开头的路径都以 `/` 开头，把 `/` 当前缀会让 §3.4 的冲突检查对任意端点恒失败（`/metrics` 也被判冲突），检查本身变成不可用能力。
- **前缀匹配**（等于该段，或以"该段 + `/`"开头）：`/api`（认证与管理/任务/状态 API 的整棵子树）、`/assets`（`web.rs` 的 `ASSET_DIR = "/assets/"`），以及 SPA 参数路由 `/devices/{id}`、`/tasks/{id}`、`/admin/{*rest}` 对应的字面段 `/devices/`、`/tasks/`、`/admin/`。

即 `is_reserved("/")` 为真、`is_reserved("/metrics")` 为假、`is_reserved("/api")` 与 `is_reserved("/api/v1/tasks")` 为真、`is_reserved("/apiv1")` 为假（前缀匹配必须以 `/` 边界成立，不做裸 `starts_with`）。

保留集合与 `web.rs`（`SPA_EXACT_ROUTES`/`SPA_PARAM_ROUTES`/`ASSET_DIR`）、`http_auth::build_http_router`、`api`/`tasks`/`http_device_status` 的路由表由 §7.6 的一致性测试钉住：漂移时新增路由必须同步登记，否则测试红。配置期拦截冲突而非依赖 Axum 合并期 panic（`Overlapping method route`），因为运维能在启动前看到明确配置错误而不是进程崩溃栈。

---

## 7. 测试验证策略（Testing Strategy）

分层门禁与仓库既有约定一致：先在无 I/O 层拿到行为 RED→GREEN，再做进程内 HTTP 集成，最后做装配与真实库验收；fake 只证明它所代表的那一层。

### 7.1 层次划分

| 层 | 位置 | 证明内容 | 不证明 |
| --- | --- | --- | --- |
| L1 纯逻辑单测 | `metrics/config.rs`、`auth.rs`、`model.rs`、`render.rs` 的 `#[cfg(test)]` | 三阶段配置解析与门禁、Basic 头解析（owned `ParsedBasic`）、常量时间映射、渲染确定性 | 路由、HTTP 语义 |
| L2 进程内 HTTP | `metrics/http.rs` 的 `#[cfg(test)]`（`tower::ServiceExt::oneshot` + fake source） | 状态码与响应头（含 GET/HEAD 分派）、认证与禁用门禁、失败语义 | 真实 SQL、真实库数据、真实连接池回收 |
| L3 装配测试 | `tests/production_composition.rs` 扩展 | 六线合并可达、单一端口、隔离与**单一池的结构性装配**、路径不冲突 | 认证语义、真实指标取值、真实慢查询下的池行为 |
| L4 真库验收 | `tests/mysql_identity.rs` 风格（`#[ignore]`，需授权开发库） | 两条聚合 SQL 在 MySQL/TiDB 上的真实分布与求和不变量、二进制分组语义、**慢查询/超时后业务仍可借到连接** | — |

L1 的关键签名以本规范为准（改动签名即改契约，需同步改本表）：`parse_authorization(&HeaderMap) -> Option<ParsedBasic>`（owned，§4.2）、`verify(&str, &str, &BasicCredentials) -> bool`、`MetricsConfig::from_parts_with(&dyn Fn(&str) -> EnvRead, &dyn Fn(&Path) -> Result<String, io::Error>) -> Result<MetricsConfig, ControllerError>`（§3.3）。

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
| `accepts_incomplete_auth_section_shape` | `[metrics.auth]` 只写 `username = "u"`（阶段一 Raw DTO 全 `Option`） | `parse_file` **Ok**（形状合法，必填性由门禁裁决） |
| `rejects_invalid_toml_without_leaking_values` | 语法错误且错误行含 `password = "secret-value"` | `Err`，且 `err.message()` 不含 `secret-value` |
| `rejects_non_basic_auth_type` | `enabled=true` + `type = "bearer"` / `"none"` / `""` | `Err("metrics.auth.type must be \"basic\"")` |
| `rejects_missing_auth_type_when_enabled_without_env` | `enabled=true`、`[metrics.auth]` 写了完整 username/password 但缺 `type` | `Err("metrics.auth.type must be \"basic\"")`（§3.5 第 2 条） |
| `accepts_password_containing_colons` | `enabled=true`、`password = "a:b:c"` | `Ok`，终态密码逐字节 `"a:b:c"`（§3.4 规则 2） |
| `rejects_endpoint_without_leading_slash` | `endpoint = "metrics"` | `Err("metrics.endpoint is invalid")` |
| `rejects_endpoint_with_trailing_slash_query_or_space` | `"/metrics/"`、`"/metrics?x=1"`、`"/met rics"`、`"/a//b"`、`"/METRICS"` | 同上 |
| `rejects_reserved_endpoint` | `"/healthz"`、`"/api/v1/tasks"`、`"/devices"`、`"/assets/x"`、`"/"` | `Err("metrics.endpoint collides with an existing route")` |
| `reserved_root_path_is_exact_match_only` | `"/metrics"`、`"/metrics/deep"` 与保留集合 | 均**不**冲突（`/` 只精确匹配、前缀匹配需 `/` 边界，§6.5） |
| `rejects_username_with_colon_or_control_char` | `"a:b"`、`"a\u{7}"` | `Err("metrics.auth.username is invalid")` |
| `rejects_password_with_control_char` | `password = "a\u{7}b"` | `Err("metrics.auth.password is invalid")` |
| `fail_closed_when_enabled_without_auth` | `enabled=true` 且无 `[metrics.auth]`、无环境变量 | `Err("metrics.auth is required when metrics.enabled is true")` |
| `rejects_blank_username_when_enabled` | `enabled=true`、`[metrics.auth]` 内 `username = ""` | `Err("metrics.auth.username is invalid")` |
| `rejects_blank_password_when_enabled` | `enabled=true`、`[metrics.auth]` 内 `password = ""` | `Err("metrics.auth.password is invalid")` |
| `disabled_drops_credentials_without_error` | `enabled=false` + 完整 `[metrics.auth]` | `Ok`，`credentials=None`，终态不保留密码副本 |
| `disabled_tolerates_incomplete_auth` | `enabled=false` + `[metrics.auth]` 只写 `username`（或 `type = "basc"`） | `Ok`，`credentials=None`，**不阻断启动**（§3.6 分支 1；P1 的核心回归网） |
| `env_pair_overrides_file_credentials` | 文件示例值 + 环境变量对 | 终态为环境变量值 |
| `env_pair_synthesizes_auth_section_when_absent` | 文件只有 `enabled = true`（无 `[metrics.auth]`）+ 环境变量对 | `Ok`，凭据为环境变量值且 `kind` 推导为 `basic` |
| `env_pair_derives_basic_when_type_absent` | 文件 `[metrics.auth]` 有 username/password 无 `type` + 环境变量对 | `Ok`（§3.3 第 3 步推导） |
| `env_does_not_override_illegal_type` | 文件 `type = "bearer"` + 环境变量对 | `Err("metrics.auth.type must be \"basic\"")`（覆盖不猜意图） |
| `env_partial_override_is_rejected` | 只设 `PROMETHEUS_AUTH_USER` | `Err("metrics auth override must set both user and password")` |
| `env_empty_string_counts_as_unset` | `PROMETHEUS_AUTH_USER=""`、password 已设 | `Err`（同 partial） |
| `env_non_unicode_password_is_error` | `PROMETHEUS_AUTH_PASSWORD` 为 `EnvRead::NotUnicode` | `Err("metrics auth override env var is not valid unicode")`，且**不是**"未设置→沿用文件凭据"（§3.4 规则 7） |
| `env_non_unicode_user_is_error` | `PROMETHEUS_AUTH_USER` 为 `EnvRead::NotUnicode` | 同上（两个变量同一条消息） |
| `config_path_non_unicode_is_error` | `CONTROLLER_CONFIG` 为 `EnvRead::NotUnicode` | `Err("metrics config path is not valid unicode")`，不回落默认路径 |
| `error_messages_never_echo_credentials` | 覆盖上述所有 `Err` 分支 | 消息不含用户名/密码字面量 |
| `metrics_config_debug_is_redacted` | 对启用态 `MetricsConfig` 求 `format!("{:?}", …)` | 不含用户名与密码字面量，含 `"[redacted]"`（§3.2 手写 `Debug`） |

另需路径与覆盖语义测试，全部通过 `from_parts_with` 注入闭包（假 `env` 映射返回 `EnvRead` + 假文件读取），不使用 `std::env::set_var`：`explicit_path_missing_is_error`、`explicit_path_present_is_used`、`default_path_absent_is_ok`、`default_path_present_is_used`、`empty_config_env_falls_back_to_default_path`、`unreadable_file_is_error`、`non_utf8_file_is_error`。

### 7.3 Basic Auth 与常量时间单元测试（L1）

| 测试名 | 场景 | 期望 |
| --- | --- | --- |
| `parses_valid_basic_header` | `Basic` + `base64("prom_admin:secure_password")` | `Some(ParsedBasic)`：`username == "prom_admin"`、`password == "secure_password"`（owned 值可脱离 `HeaderMap` 存活：先 `drop(headers)` 再断言两字段） |
| `parses_password_with_colons` | `base64("prom_admin:a:b:c")` | `Some`，`username == "prom_admin"`、`password == "a:b:c"`（§4.2 第 6 步 `split_once`；RFC 7617 §2） |
| `parses_password_with_many_and_edge_colons` | `base64("u::::")`、`base64("u:日本語:パ:Sword=")` | `Some`，冒号、UTF-8、`=` 全部归属密码；`"u:"` → 密码为空串 `Some` |
| `scheme_is_case_insensitive` | `Basic` / `basic` / `BASIC` | 均解析成功；`Bas ic`（内部空格）失败 |
| `rejects_other_schemes` | `Bearer <b64>`、`Digest …`、无 scheme、多空格、前导空白 | `None` |
| `rejects_multiple_authorization_headers` | 两个 `Authorization` 头（值相同或不同） | `None`（绝不取第一个） |
| `rejects_invalid_base64_and_unpadded` | 含 `!`、长度非 4 倍数、去掉 `=` padding | `None` |
| `rejects_non_utf8` | 解码后非法 UTF-8 字节序列 | `None` |
| `rejects_missing_colon_and_empty_user` | `"prom_admin"`（无冒号）、`":pw"`、`":"` | `None` |
| `verify_accepts_exact_match` | 完全匹配 | `true` |
| `verify_accepts_password_with_colons` | 配置密码 `"a:b:c"` + 提交 `"a:b:c"` | `true`（端到端凭据形状与 §7.5 L2 用例配对） |
| `verify_rejects_each_field_independently` | 用户名错 / 密码错 / 都错 / 都为空 | 均 `false`（响应不可区分） |
| `verify_is_length_independent` | 长度差 1 与差 64 的错误密码均 `false` | 与 `ct_eq_32` 固定长度契约一致（长度不进比较） |
| `ct_eq_32_is_content_sensitive` | 逐位差异（首字节、末字节、单比特） | 均 `false`；相等为 `true` |
| `ct_eq_32_uses_subtle_constant_time_eq` | 源码级断言（`include_str!("../auth.rs")` 含 `use subtle::ConstantTimeEq`；`Cargo.toml` 断言 `subtle.workspace = true`） | `ct_eq_32` 由 `subtle::ConstantTimeEq` 实现；本模块源码不含 §4.3 第 2 条所禁的手写逐字节折叠形态（与 §7.8 第 7 条 grep 互为冗余） |
| `verify_runs_both_comparisons` | 用可观测封装（`#[cfg(test)]` 计数的 `ct_eq_32` 包装）断言用户名不匹配时密码比较仍发生 | 调用计数 == 2 |

`verify_runs_both_comparisons` 是 §4.3 第 1 条的回归网：将来有人把两次比较改回短路写法，该测试必须变红。

### 7.4 数据模型与渲染单元测试（L1）

| 测试名 | 断言 |
| --- | --- |
| `admission_label_maps_persisted_vocabulary` | `APPROVED→APPROVED`、`PENDING→PENDING`、`REVOKED→REJECTED`；`"approved"`（小写）、`""`、`"UNKNOWN"`、`"APPROVED "` → `None`（§5.2.2 未知值 fail-closed；数据库侧已无 CHECK 约束，见同节事实修正） |
| `task_bucket_covers_documented_matrix` | §5.2.2 全表逐条，含 `("completed", None)→failed` 与非终态 `outcome` 取任意已知值仍归该桶 |
| `task_bucket_fails_closed_on_unknown_state` | `("queued ", _)`、`"DONE"`、`""` → `None` |
| `task_bucket_fails_closed_on_unknown_outcome` | `("completed", Some("Success"))`、`("completed", Some(""))`、`("completed", Some("succeded"))` → `None`（未知 outcome 与未知 state 同等 fail-closed） |
| `unknown_state_and_outcome_messages_are_distinct` | 两条采集错误分别为 `"metrics task aggregate: unknown task state"` 与 `"…unknown task outcome"`，且都不含原始取值 |
| `render_matches_golden_exposition` | §5.6 fixture 逐字节相等（含末尾单个 `\n`） |
| `render_emits_zero_valued_series` | 全 0 fixture 仍输出全部 11 条序列 |
| `render_escapes_label_values` | 含 `"`、`\`、`\n` 的标签值被转义为 `\"`、`\\`、`\n`（用直接构造的 `ScrapeValues`） |
| `render_does_not_escape_carriage_return_or_tab` | 含 `\r`、`\t` 的标签值**逐字节原样**输出（Prometheus text 0.0.4 的转义表只含三项，§5.1 第 6 条） |
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
| `scrape_with_password_containing_colons_returns_200` | 配置密码为 `a:b:c`，提交 `base64("prom_admin:a:b:c")` | `200` 且体与 golden 一致（P1 的端到端回归网：RFC 7617 合法密码不得被解析或配置层拒绝） |
| `head_with_valid_credentials_returns_200_without_collection` | `HEAD /metrics` + 正确 Basic | `200`；体为空；`Content-Type`/`Cache-Control`/`X-Content-Type-Options` 三头齐全；**source 调用次数 0**、并发闸 permit 未减少（§6.2 HEAD 短路） |
| `head_without_credentials_returns_401_like_get` | `HEAD /metrics`，无 `Authorization` | `401`；`WWW-Authenticate` 与 GET 逐字节相同；source 0 次（HEAD 不绕过认证） |
| `head_when_disabled_returns_404` | `enabled=false` + `HEAD /metrics`（带/不带凭据） | 均 `404`；无 `WWW-Authenticate`；source 0 次 |
| `scrape_without_credentials_returns_401` | 无 `Authorization` | `401`；`WWW-Authenticate` 逐字节等于 `Basic realm="rsetup-controller-metrics"`；体为空；**source 调用次数 0** |
| `scrape_with_wrong_password_returns_401` | 正确用户名 + 错密码 | `401`，与无凭据响应同形（source 0 次） |
| `scrape_with_wrong_username_returns_401` | 错用户名 + 正确密码 | `401`（source 0 次） |
| `scrape_with_malformed_header_returns_401` | `Bearer …`、非法 base64、无冒号、`":pw"`、两个 `Authorization` 头 | 均 `401`（source 0 次） |
| `disabled_endpoint_returns_404_regardless_of_credentials` | `enabled=false`，分别带/不带凭据 | 均 `404`；无 `WWW-Authenticate`；无 `Content-Type`；source 0 次 |
| `other_methods_are_rejected_with_allow_header` | `POST`/`PUT`/`PATCH`/`DELETE`/`OPTIONS` `/metrics` | 均 `405` 且 `Allow` 头为 `GET, HEAD`（Axum 依注册方法生成，§4.5）；source 0 次 |
| `other_paths_are_not_registered_by_metrics_line` | `GET /`、`/healthz`、`/metrics.txt` | 404 / 404 / 404（本线只注册配置路径一条） |
| `metrics_ignores_session_cookie` | 携带格式合法的 `rsc_session=…` 而无 Basic | `401`（§4.1 隔离的行为证明） |
| `metrics_success_does_not_create_session` | 成功抓取 200 | 响应无 `Set-Cookie`；`HttpAuthState` 相关结构（若断言）零变化 |
| `source_error_maps_to_503_not_partial_zero` | source 返回 `Err(Config)` | `503`；体为空；**不含任何指标行** |
| `source_timeout_maps_to_503` | source 挂起 > `SCRAPE_TIMEOUT` | `503`（测试用 `tokio::time::pause`/缩短常量注入） |
| `scrape_concurrency_cap_is_enforced` | 占满 `Semaphore(4)` 后 `GET` 请求 | `503`，且 source 未被再次进入；同刻 `HEAD` 仍 `200`（HEAD 不占闸，§6.2） |
| `credentials_are_not_echoed_in_error_paths` | 各 401 响应 | 头与体都不含提交的凭据原文或其 base64 |

### 7.6 架构指针一致性与路由合并测试（L3）

扩展 `crates/rsetup-controller/tests/production_composition.rs`：

1. `inputs()` 构造增加 `metrics: MetricsLineInputs { config: Arc::new(disabled/enabled fixture), runtime }`——**没有 `snapshot` 字段可传**（§6.3）；需要合成取值的用例改走 `build_production_router_with_metrics_source(inputs, Arc::new(SyntheticMetricsSnapshotSource))`。既有五线断言保持不变。
2. `every_component_is_reachable_without_duplicate_probes`：合成拓扑扩为六组件，断言六条路径（含 `/metrics`）在同一顶层 router 上各自可达、marker 不串线，无重复探活。
3. `duplicate_business_path_panics_instead_of_being_hidden_by_override`：既有 panic 用例扩到六参数版本；新增用例 `metrics_path_colliding_with_reserved_route_panics_on_merge`（`compose_component_routers` 传两个 `/metrics`）确认合并期 panic 仍存在，作为配置期拦截的第二道网。
4. `mismatched_auth_context_is_rejected_on_every_line`：保持三条负例（management/tasks/status）不变，证明指标线新增未削弱原门禁。
5. `shared_auth_context_composes_all_five_lines` 改名为 `shared_auth_context_composes_all_six_lines` 并扩展断言：`enabled=true` 的 fixture 下，`/healthz` 200、`/readyz` 503（lazy 池不可达）、`/api/v1/auth/me` 401、`GET /metrics` 无凭据 401 且带 `WWW-Authenticate`——四者同时成立，证明单一端口、六线合并且原五线断言全部保留。
6. `metrics_gauges_read_the_shared_pool_only`：用 lazy 池 + `build_production_router_with_metrics_source(… SyntheticMetricsSnapshotSource)`（两条聚合走合成源，池 gauge 仍走真实内存读数）断言 `db_pool_connections{state="active"} 0` 与 `{state="idle"} 0`（lazy 池未建立任何连接），且抓取前后 `lazy_db()` 未产生连接——证明指标线读的是 `auth.db` 那个池且不发查询。
7. `metrics_line_never_touches_session_boundary`：在全量顶层 router 上，用完整合法会话 Cookie（L2 的 `LiveSessionAuth` 桩形态）请求 `/metrics` 得 401；随后请求 `/api/v1/auth/me` 仍按会话语义工作，证明指标抓取未污染会话/CSRF/限速状态。
8. `reserved_paths_stay_registered`：对 `/healthz`、`/readyz`、`/`、`/login`、`/assets` 前缀逐一探测，断言它们仍由所属条线应答（而不是被指标线抢占或退化为 404），并且 `RESERVED_PATHS` 覆盖 `web.rs` 的 `SPA_EXACT_ROUTES`/`SPA_PARAM_ROUTES`/`ASSET_DIR` 全部条目，使保留集合不与代码漂移。
9. `production_inputs_exposes_no_second_pool`（结构性、编译期即成立）：断言 `MetricsLineInputs` 的字段集合只有 `config` 与 `runtime`（构造点写全字段、不允许 `..Default::default()` 兜底），并用源码级断言（`include_str!("../src/main.rs")`）钉住 `main.rs` 只调用 `build_production_router`、`src/` 下 `SqlxMetricsSnapshotSource::new` 与 `build_production_router_with_metrics_source` 只出现在 `production.rs`（§6.3、§7.8）。这条测试是"生产装配内部基于 `auth.db` 构造汇总源"的回归网：一旦有人把汇总源或 `DbPool` 重新加回顶层入参，编译或该测试立刻失败。
10. `metrics_timeout_does_not_starve_business_routes`（L3 侧的**结构**部分）：注入一个"永久挂起"的合成汇总源，占满并发闸后，断言 `/healthz` 仍 200、`/api/v1/auth/me` 仍按会话语义 401（不经指标线）、且业务线借用连接的调用点数未被指标线改变（无新增池构造、无 `detach`）。**真实慢查询下的池可用性不在此证明范围**，见 §7.7 的 L4 用例。

### 7.7 真实数据库验收（L4）与边界

两条聚合 SQL 的真实行为（`GROUP BY` 分布、`CAST(... AS CHAR)` 类型、TiDB 兼容性、求和不变量、空表 → 全 0 序列）按 `tests/mysql_identity.rs`/`tests/tidb_identity.rs` 的做法在专用 `#[ignore]` 测试中验收，连接**已获授权的隔离开发库**、凭据仅进程内载入、测试报告只出去敏结果（`AGENTS.md` 凭据保护规则）。该层只读，不建表、不迁移、不写入（表数据准备亦限定在已获授权的隔离开发库内，并按同规则只出去敏结果）。L2 的 fake source 通过 MUST NOT 被表述为真库聚合已验收；反之 L4 未运行时，交付报告必须写明"两条聚合 SQL 的真实引擎验收仍待执行"。

L4 必须覆盖以下三组、缺一即该层未完成：

1. `aggregates_match_table_distribution_and_sum_invariant`：既有数据下三桶/四桶逐桶与独立 `COUNT(*)` 对账，且求和恒等于表行数（§5.1 第 4 条）。
2. `grouping_is_binary_collated_on_both_engines`：**构造**只差大小写（`APPROVED` 与 `approved`）或只差尾部空格（`PENDING` 与 `'PENDING '`）的分组键，断言两条聚合把它们返回为**各自独立的分组**（证明 §5.3 的 `COLLATE utf8mb4_bin` 生效、默认排序规则没有折叠差异），并断言此时经 `metrics` 采集路径得到 **503 fail-closed**（映射函数拒绝未知值），而不是把异常值算进合法桶。同时断言该 SQL 在 MySQL 与 TiDB 上都不因 `ONLY_FULL_GROUP_BY` 报错。
3. `metrics_slow_query_does_not_exhaust_shared_business_pool`：在**同一个 `DbPool`** 上执行本用例（不新建池），步骤固定为
   1. 通过指标采集路径（`SqlxMetricsSnapshotSource` 的实现骨架 + 把两条聚合替换为 `SELECT SLEEP(...)` 慢查询的测试变体，保留 `POOL_ACQUIRE_TIMEOUT` 与 `close_on_drop()` 语义）并发发起 `MAX_CONCURRENT_SCRAPE + 1` 次抓取；
   2. 断言全部抓取在 `POOL_ACQUIRE_TIMEOUT + SCRAPE_TIMEOUT` 之内以 503 结束（不是悬挂到 30s `acquire_timeout`）；
   3. 在抓取进行中与结束后各测一次业务侧 `pool.acquire()`：均须在 **500 ms** 内成功（业务未被指标慢查询饿死）；
   4. 慢查询超时后再连续借还 10 次连接，断言每次都在 500 ms 内成功、`pool.size()` 回落到 `max_connections` 以内、且**没有**超时期间归还的空闲连接被业务再次取到（`close_on_drop()` 生效：脏连接被销毁而非回到空闲队列）；
   5. 报告只出去敏数值（耗时、permit/size/idle 计数），不回显 DSN。

   该用例是 §5.5.1 三条防护（acquire 超时、`close_on_drop`、并发闸封顶）在真实驱动行为上的唯一验收点：L2/L3 的合成源证明不了 `PoolConnection::Drop` 的后台 drain 语义。

### 7.8 命令与静态门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p rsetup-controller
cargo test -p rsetup-controller --test production_composition
grep -rnE 'TO''DO|T-B-D|FIXME' crates/rsetup-controller/src/metrics
grep -rn 'build_production_router_with_metrics_source\|SqlxMetricsSnapshotSource::new' crates/rsetup-controller/src/main.rs
grep -rn 'diff |= a\[i\]' crates/rsetup-controller/src/metrics
grep -rn 'subtle::ConstantTimeEq\|use subtle::ConstantTimeEq' crates/rsetup-controller/src/metrics/auth.rs
```

第 5 条 grep MUST 无输出；模式串刻意写成 `'TO''DO|T-B-D|FIXME'` 的 shell 拆分形式，这样 `TODO` 不会因把命令原文写进文档而自指命中（`FIXME` 无法用同样手法拆开——拆开就不再是那个模式串，因此对本文件执行同一条检查时，MUST 只命中本节这条命令自身所在的代码块，除此之外零命中）。第 6、7 条 MUST 无输出（生产入口不得触碰测试注入缝；指标线不得手写常量时间折叠）。第 8 条 MUST **有**输出（常量时间比较确实落在 `subtle` 上）。提交前另需检查 `git diff --cached` 不含真实凭据、不含 `controller.toml` 实体（§3.7）。

---

## 8. 安全评审要点与残余风险

| 编号 | 风险 | 影响 | 本期处置 |
| --- | --- | --- | --- |
| R1 | 长期静态凭据、进程内无失败限速（§4.6） | 在线暴力猜测 | 默认监听 `127.0.0.1:8080`；对外暴露必须由部署层加来源限制/TLS/失败封禁；密码建议 CSPRNG ≥ 20 字符（含冒号等符号均合法）；比较不短路且长度归一 |
| R2 | 凭据明文存于 TOML | 文件读取即泄密 | 建议 `0600` + 私有目录；环境变量成对覆盖允许不落盘；凭据不入库、不入日志；`MetricsConfig` 的 `Debug` 是**手写脱敏**（§3.2）；错误消息不回显（§3.4/§3.6 有测试） |
| R3 | 哈希前耗时与 base64 解析失败提前返回（§4.3 第 4 条） | 弱时序旁证（不含凭据内容） | 明确留档为可接受残余；比较本身用 `subtle::ConstantTimeEq`（§4.3 第 2 条），不宣称全 handler 常量时间 |
| R4 | `401` 携带 `WWW-Authenticate` 会宣告端点存在 | 信息暴露有限 | 对齐设计的显式要求（抓取方排障需要）；禁用态 404 刻意不带该头，保持与"路径不存在"不可区分 |
| R5 | 禁用态下 `POST` 等未注册方法返回 405（§4.5） | 可探测配置路径存在 | 与业务路由既有语义一致，不特殊化；如需完全隐藏须改为"不注册路由"，同时放弃 §6.2 的确定性 404，本期不做。HEAD 在禁用态与本行同属 404，不额外暴露信息 |
| R6 | 每次抓取两条聚合查询 | 高频抓取压库 | 两条聚合都按表达式分组（`CAST(...) COLLATE utf8mb4_bin`），**既有 `ix_devices_admission` 与 `ix_main_tasks_state` 都无法用于该分组**（表达式使索引不可用），代价是表行数级的扫描 + 临时分组；唯一护栏是并发闸 4 + 2s 超时 + `scrape_interval` 由部署控制（建议 ≥ 30s）+ §5.5.1 的 `MAX_EXECUTION_TIME` 兜底提示。出现实测压力时另立规范引入覆盖索引、表达式索引或指标缓存 |
| R7 | 跨两条聚合的非事务读 | 两族之间毫秒级偏差 | §5.3 第 4 条显式声明为近似瞬时 gauge；对账走 SQL |
| R8 | `_total` 后缀用作 gauge 违反命名习惯 | 误用 `rate()` | §5.4：`# TYPE gauge` 必发 + `HELP` 说明求和语义 |
| R9 | `REVOKED → REJECTED` 词汇映射（§5.2.2） | 阅读者误认为存在第四种准入态 | 映射表在指标 `HELP`、本文与单元测试三处留档；未知值 fail-closed 503，绝不静默归类 |
| R10 | `toml` 为工作区新增依赖、`subtle` 提升为直接依赖 | 供应链与离线构建 | 均按最小用途使用、不启用额外 feature（§3.1）；`subtle 2.6.1` 已在 `Cargo.lock` 不构成新树，`toml 0.8` 首次进入需更新 `Cargo.lock` 并在交付说明中登记新增 crate |
| R11 | 抓取超时/取消后，池槽位的实际释放由 `PoolConnection::Drop` 派生的后台任务完成，最坏受 `CLOSE_ON_DROP_TIMEOUT`（5s）约束（§5.5.1 第 3 条） | 数据库持续极慢时，最多多占 4 条连接、最长约 5s | 并发闸 4 ≪ 池默认 `max_connections = 10`，业务恒有 ≥ 6 个槽位；`acquire` 侧 500ms 快速失败使紧张状态立刻可见为 503；§7.7 第 3 组 L4 用例实测业务借连接仍 < 500ms |
| R12 | 单个脏枚举值（未知 `admission_state`/`state`/`outcome`）使整次抓取 503 | 一条异常历史数据即让该实例指标全黑（`up` 抖动） | 刻意的 fail-closed 取舍（I4/G4）：宁可指标缺失，不输出可求和但错误的数据；数据库侧已无 CHECK 约束（§5.2.2 事实修正），因此同时用 503 的去敏消息与 §7.7 的二进制分组用例把"哪个桶对不上"变成可排障事件；后续如需容错须另立规范定义显式的 `unknown` 桶语义 |

---

## 9. 验收标准（Definition of Done）

1. §7.2–§7.6 的全部测试存在且通过，并各自先有对应 RED 记录（实现提交按 RED→GREEN 分步）。
2. 端点在默认（禁用）配置下不改变现有任何响应；启用后 `/metrics` 在同一端口 200，`Content-Type` 与 golden 文本逐字节符合 §5.1/§5.6；`HEAD /metrics` 认证后 200 空体且不触达数据源（§6.2）。
3. 配置按 §3.3 三阶段生效：`enabled = false` 且 auth 不完整时启动成功并丢弃该段；`enabled = true` 且凭据不完整/非法时进程拒绝启动，错误文案与 §3.6 完全一致，且没有产生监听套接字或数据库连接；环境变量非 Unicode 值是启动错误而非"未设置"。
4. §4.5 矩阵每一行都有对应自动化断言；401 头逐字节符合 §4.4；未注册方法的 405 带 `Allow: GET, HEAD`。
5. §4.1 隔离不变量与 I1–I6 均有代码结构或测试证据；未新增 `EndpointId` 变体、permission、审计写入、数据库对象或第二个连接池构造点；`MetricsLineInputs` 无汇总源/池字段，生产汇总源只在 `build_production_router` 内部由 `inputs.auth.db` 构造（§6.3、§7.6 第 9 条）。
6. §5.5.1 三条连接防护（`POOL_ACQUIRE_TIMEOUT`、`close_on_drop()`、并发闸封顶）在代码中可指认，且 §7.7 第 3 组的池可用性用例已在授权开发库执行；若未执行，交付报告显式标注。
7. 两条聚合 SQL 使用 §5.3 的二进制分组写法，并在 MySQL 与 TiDB 上均通过 §7.7 第 2 组（大小写/尾部空格不折叠 + 未知枚举 503）。
8. §7.8 的 fmt/clippy/test 全绿，四条 grep 门禁符合预期（第 5/6/7 条无输出、第 8 条有输出），提交内不含凭据。
9. 若 L4 真库聚合验收尚未执行，交付报告必须显式标注该项未完成，不以 L2 fake 通过替代（G4/§7.7）。

### 9.1 交付边界

本规范文档本身不改动源码。实现应拆为可独立审查的增量：①`config`（三阶段 + Raw DTO + `EnvRead`）+ 单测；②`auth`（`ParsedBasic` + `subtle`）+ 单测；③`model`/`render` + 单测；④`collect`（含 §5.3 二进制分组 SQL 与 §5.5.1 三条连接防护）+ L1/L4；⑤`http`（含 HEAD 短路）+ L2；⑥`production`/`main` 装配（含 uptime 零点前移）+ L3；⑦文档与部署说明。提交信息遵循仓库规则（简体中文，Conventional Commits 描述亦为中文），例如 `feat(controller): 增加普罗米修斯指标端点与配置文件认证`。

---

## 10. 附录

### 10.1 `controller.toml` 完整示例（不可直接使用）

```toml
# 示例形状：真实部署请替换为强随机凭据，并将文件权限设为 0600。
# 本期该文件只允许 [metrics] 段；其他顶层表会被拒绝。
# 密码可以包含冒号、等号与 UTF-8 字符（RFC 7617 §2；解析按首个冒号切分）。
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

禁用态对 `[metrics.auth]` 完全宽容（§3.6 分支 1）：下面这份**能正常启动**，auth 段被直接丢弃，用于凭据轮换过程中"先关端点、后补凭据"的中间状态——把 Raw DTO 全字段设为可选正是为了让这种状态不阻断主业务：

```toml
[metrics]
enabled = false

[metrics.auth]
username = "prom_admin"   # 合法：禁用态不校验，也不会被用于任何认证
```

对照：只写 `enabled = true` 而不写 `[metrics.auth]`（也没有环境变量凭据对）是**非法配置**，启动失败并报告 `metrics.auth is required when metrics.enabled is true`（§3.6 分支 2），不会退化成免认证端点。

### 10.2 环境变量清单（本设计新增）

| 变量 | 作用 | 缺省 | 空串语义 | 非 Unicode 语义（`VarError::NotUnicode`） |
| --- | --- | --- | --- | --- |
| `CONTROLLER_CONFIG` | TOML 配置文件路径（可为相对进程工作目录） | `controller.toml` | 视为未设置，回落默认路径 | 启动失败：`"metrics config path is not valid unicode"`（不回落） |
| `PROMETHEUS_AUTH_USER` | 覆盖 `[metrics.auth].username` | 无 | 视为未设置 | 启动失败：`"metrics auth override env var is not valid unicode"`（不静默沿用文件凭据） |
| `PROMETHEUS_AUTH_PASSWORD` | 覆盖 `[metrics.auth].password` | 无 | 视为未设置 | 同上 |

`PROMETHEUS_AUTH_USER` 与 `PROMETHEUS_AUTH_PASSWORD` MUST 同时设置或同时不设置（§3.3 第 3 步）。两者都非空设置时，若文件未写 `type`，`kind` 推导为 `"basic"`（§3.3 第 3 步）。

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

# 密码含冒号同样 200（curl 亦按首个冒号切分 -u 参数；服务端 split_once 一致）
curl -fsS -o /dev/null -w '%{http_code}\n' -u 'prom_admin:a:b:c' http://127.0.0.1:8080/metrics

# 期望 200 且无响应体：HEAD 走同一认证，但不采集（不产生数据库开销）
curl -sS -o /dev/null -D - -I -u 'prom_admin:secure_password' http://127.0.0.1:8080/metrics

# 期望 401 且带 WWW-Authenticate: Basic realm="rsetup-controller-metrics"
curl -sS -o /dev/null -D - http://127.0.0.1:8080/metrics

# 期望 401（HEAD 也不得绕过认证）
curl -sS -o /dev/null -D - -I http://127.0.0.1:8080/metrics

# 期望 401
curl -sS -o /dev/null -D - -u 'prom_admin:wrong-password' http://127.0.0.1:8080/metrics

# 期望 405 且 Allow: GET, HEAD
curl -sS -o /dev/null -D - -X POST -u 'prom_admin:secure_password' http://127.0.0.1:8080/metrics

# 期望 404（enabled = false 配置）
curl -sS -o /dev/null -D - http://127.0.0.1:8080/metrics
```

### 10.6 关键决策摘要

| 决策 | 取舍与被否方案 |
| --- | --- |
| 只支持 Pull，不做 Pushgateway/远程写 | 与 Prometheus 生态默认一致、失败可见（`up`）；推送需要额外的重试/缓冲语义，本期无需求 |
| 同端口同进程 | 少一个监听面与一套 TLS；代价是指标线故障与业务共用进程（可接受，采集只读且带并发闸、超时与 §5.5.1 池防护） |
| 独立 Basic Auth 边界，不复用会话 | 机器客户端无会话与 Origin；复用会把 Host/Origin/CSRF 语义强推给监控（§4.1） |
| 凭据来自 TOML，环境变量成对覆盖 | 需要结构化的多键配置；成对覆盖避免混搭出不可追溯的半新凭据 |
| Raw DTO 全字段可选 + 阶段三门禁 | 必填字段会让"禁用 + 半截 auth"这种无害中间态直接炸掉服务启动；形状与语义分阶段后，禁用态宽容、启用态严格（§3.2/§3.6） |
| 启用即要求凭据（启动期 fail-closed） | 拒绝"先开起来再补认证"的默认开放风险；与仓库既有配置门禁风格一致 |
| 环境变量非 Unicode 视为配置错误 | 静默当未设置会让凭据轮换悄悄失效并继续跑旧值，比拒绝启动危险（§3.3 `EnvRead`） |
| 解析结果用 owned `ParsedBasic` | base64 解码与 UTF-8 校验的产物是函数内临时缓冲，借用式返回不可能安全；owned 换取"值不依赖 `HeaderMap` 存活"的确定性（§4.2） |
| 密码允许冒号（`split_once`） | RFC 7617 §2 只约束 userid；拒绝含冒号密码会把合法抓取方永久锁在 401（§3.4/§4.2） |
| 常量时间比较用 `subtle::ConstantTimeEq` | 手写 XOR/OR 折叠不保证在优化后仍是常量时间；`subtle` 已在依赖锁里，成本仅为提升直接依赖（§4.3） |
| HEAD 认证后短路 200 空体 | 沿用框架默认会让 HEAD 付出一条被丢弃 body 的全部数据库开销；短路既不绕认证也不产生采集（§6.2） |
| 不引入指标 SDK | 11 条固定序列无需注册中心；避免隐式指标与新的全局状态 |
| 采集失败整次 503 | 拒绝部分降级为 0：伪 0 会让告警静默，比"抓取失败"更危险；未知枚举同样 503（§5.2.2、R12） |
| 分组用二进制校对（`COLLATE utf8mb4_bin`） | 默认排序规则会折叠大小写与尾部空格，把未知枚举吞进合法桶，使 fail-closed 链路失效；`BINARY` 运算符已弃用且有 1055 风险（§5.3） |
| 单一池由装配内部保证 | 顶层入参不接受汇总源/池：第二个池无法表达；测试注入缝的类型里没有 `DbPool`，无法借此引入第二池（§6.3） |
| 超时连接 `close_on_drop()` 销毁 | 外层 `timeout` 管不到 `PoolConnection::Drop` 的后台 drain；销毁脏连接而非归还空闲队列，才真正保护业务线（§5.5.1） |
| `held` 并入 `pending`、`REVOKED` 映射 `REJECTED` | 保持对齐设计固定的标签域与求和不变量；映射在 `HELP`、本文与测试三处留档 |

---

## 11. 变更记录

| 版本 | 日期 | 变更 |
| --- | --- | --- |
| v1 | 2026-10-19 | 初版：架构与契约方向逐节对齐后落档。 |
| v2 | 2026-10-19 | 按架构与代码复审意见修订（4 项 P1 阻断 + 7 项 P2 缺陷），全部为文档变更，未改动任何源码：<br>**P1** ①`parse_authorization` 改为返回 owned `ParsedBasic`，同步 §4.2/§7.1/§7.3；②密码冒号兼容 RFC 7617 §2（删除"password 不得含冒号"，改 `split_once(':')`，§3.4/§4.2/§7.2/§7.3/§7.5）；③Raw serde DTO 字段全 `Option<String>` + 三阶段生命周期（反序列化 → 环境变量原子覆盖与 `type` 推导 → 统一门禁；禁用态容忍不完整 auth，§3.2/§3.3/§3.5/§3.6）；④SQLx 连接池回收与超时强化（`POOL_ACQUIRE_TIMEOUT`、`close_on_drop()`、后台 `return_to_pool`/`ping` drain 事实与残余占用登记、L4 池可用性用例，新增 §5.5.1）。<br>**P2** ⑤Axum 0.8 HEAD 派发事实与 HEAD 策略（认证后 200 空体、不采集；未注册方法 405 + `Allow: GET, HEAD`，§4.5/§6.2/§7.5）；⑥SQL 二进制分组 `COLLATE utf8mb4_bin` 与未知 `outcome` fail-closed（§5.2.2/§5.3/§7.4/§7.7）；⑦生产连接池单一性由装配内部保证，`MetricsLineInputs` 去掉 `snapshot` 字段 + 测试注入缝与 grep 门禁（§6.3/§6.4/§7.6/§7.8）；⑧改用 `subtle::ConstantTimeEq`（§3.1/§4.3/§7.3/§7.8）；⑨标签转义只保留 `\\`、`\"`、`\n`（§5.1/§7.4）；⑩环境变量 `NotUnicode` 显式失败（§3.3/§3.4/§7.2/§10.2）；⑪事实与描述自洽：`chk_devices_state` 已被 v3 移除、`sub_tasks` 表名、`MetricsConfig` 手写脱敏 `Debug`、uptime 零点提前到 `main()` 首行、`/` 保留路径完全精确匹配（§3.2/§5.2.2/§6.4/§6.5）。风险表新增 R11（池 permit 有界延迟释放）与 R12（脏枚举值致整次 503），§9 验收标准同步扩写。 |

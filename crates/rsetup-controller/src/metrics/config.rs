//! 指标线配置层（规范 §3）：三阶段生命周期 = 路径解析 →（阶段一）反序列化 Raw DTO
//! →（阶段二）环境变量原子覆盖 →（阶段三）统一门禁与终态装配。
//!
//! 阶段二只写数据不做门禁、阶段三只读数据不做覆盖，两者责任不重叠；这正是
//! "禁用态能被不完整 auth 配置安全放行"的前提（§3.6 分支 1）。Raw DTO 的每个叶子
//! 字段都是 `Option`，任何必填/非空/取值域判断都不发生在反序列化期。
//!
//! 本文件 MUST NOT `use axum`/`sqlx`（§6.1 依赖方向），也 MUST NOT 记录日志或回显
//! 凭据、路径、配置行原文（I5）。

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::ControllerError;

pub const DEFAULT_ENDPOINT: &str = "/metrics";
pub const CONFIG_PATH_ENV: &str = "CONTROLLER_CONFIG";
pub const DEFAULT_CONFIG_PATH: &str = "controller.toml";
pub const AUTH_USER_ENV: &str = "PROMETHEUS_AUTH_USER";
pub const AUTH_PASSWORD_ENV: &str = "PROMETHEUS_AUTH_PASSWORD";
/// Basic `realm`，参与 `WWW-Authenticate`，必须与本字符串逐字节一致。
pub const REALM: &str = "rsetup-controller-metrics";

/// §3.4/§3.5/§3.6 的固定去敏文案：唯一权威顺序的每条分支恰好一个消息，不叠加。
const MSG_ENDPOINT_RESERVED: &str = "metrics.endpoint collides with an existing route";
const MSG_ENDPOINT_INVALID: &str = "metrics.endpoint is invalid";
const MSG_ENABLED_REQUIRED: &str = "metrics.enabled is required";
const MSG_AUTH_REQUIRED: &str = "metrics.auth is required when metrics.enabled is true";
const MSG_AUTH_TYPE: &str = "metrics.auth.type must be \"basic\"";
const MSG_USERNAME_INVALID: &str = "metrics.auth.username is invalid";
const MSG_PASSWORD_INVALID: &str = "metrics.auth.password is invalid";
const MSG_OVERRIDE_PARTIAL: &str = "metrics auth override must set both user and password";
const MSG_PATH_NOT_UNICODE: &str = "metrics config path is not valid unicode";
const MSG_AUTH_ENV_NOT_UNICODE: &str = "metrics auth override env var is not valid unicode";
const MSG_FILE_UNREADABLE: &str = "metrics config file is not readable";

/// 唯一支持的认证类型（§3.5：`"none"`/免认证在本期是不可用能力）。
const AUTH_TYPE_BASIC: &str = "basic";
/// §3.4：`endpoint` 长度上限（字符形状护栏，非性能考量）。
const MAX_ENDPOINT_LEN: usize = 128;

/// §6.5 精确匹配集合：只有等于该字符串本身才算冲突。**`/` 只按完全相等判定，
/// 绝不做前缀匹配**——任何以 `/` 开头的路径都以 `/` 开头，把 `/` 当前缀会让
/// §3.4 的冲突检查对任意端点恒失败（检查本身变成不可用能力）。
const RESERVED_EXACT: &[&str] = &[
    "/",
    "/healthz",
    "/readyz",
    "/login",
    "/password",
    "/sessions",
    "/devices",
    "/tasks",
    "/admin",
];

/// §6.5 前缀匹配集合：等于该段，或以"该段 + `/`"开头。以 `/` 结尾的条目本身就是
/// 边界（对应 `web.rs` 的 `SPA_PARAM_ROUTES` 字面段与 `ASSET_DIR`）。
const RESERVED_PREFIX: &[&str] = &["/api", "/assets", "/devices/", "/tasks/", "/admin/"];

/// 两条集合的并集：供 §3.4 校验、§7.6 第 8 条与 `web.rs` 路由表的一致性测试共用，
/// 漂移时（新增业务路由未登记）测试红。
pub const RESERVED_PATHS: &[&str] = &[
    "/",
    "/healthz",
    "/readyz",
    "/login",
    "/password",
    "/sessions",
    "/devices",
    "/tasks",
    "/admin",
    "/api",
    "/assets",
    "/devices/",
    "/tasks/",
    "/admin/",
];

/// 端点是否与既有路由冲突（§6.5）。前缀匹配必须以 `/` 边界成立，绝不做裸 `starts_with`：
/// `is_reserved("/apiv1")` 为假、`is_reserved("/api/v1/tasks")` 为真。
pub fn is_reserved(path: &str) -> bool {
    if RESERVED_EXACT.contains(&path) {
        return true;
    }
    RESERVED_PREFIX.iter().any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| prefix.ends_with('/') || rest.starts_with('/'))
    })
}

/// 字符形状校验（§3.4）：`len ≤ 128`；首字符 `/`；仅由 `/` 分隔的非空 ASCII 小写段
/// （`[a-z0-9_-]+`）组成；不得以 `/` 结尾；不得含 `?`/`#`/`%`/空白/`..`。
/// 不做任何归一化：不 trim、不 case folding、不做 Unicode 规范化。
fn is_valid_endpoint_shape(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_ENDPOINT_LEN {
        return false;
    }
    if path == "/" {
        // 根路径的字符形状本身合法；它由保留集合（is_reserved）拦截。
        // 判定顺序（先保留冲突后字符形状）由 §7.2 `rejects_reserved_endpoint` 钉住。
        return true;
    }
    if !path.starts_with('/') || path.ends_with('/') || path.contains("..") {
        return false;
    }
    path[1..].split('/').all(|segment| {
        !segment.is_empty()
            && segment.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
            })
    })
}

/// 用户名合法：非空、不含 `:`（RFC 7617 §2 的 userid 约束，前移到配置期）、不含
/// `< 0x20` 或 `0x7F` 控制字符。
fn is_valid_username(value: &str) -> bool {
    !value.is_empty() && !value.contains(':') && !has_control_char(value)
}

/// 密码合法：非空、不含控制字符；**明确允许 `:`**（§3.4 规则 2：RFC 7617 的 password
/// 无此限制，配置期与解析期都 MUST NOT 对密码里的冒号做任何拒绝或转义）。
fn is_valid_password(value: &str) -> bool {
    !value.is_empty() && !has_control_char(value)
}

/// 只按规范的字面定义判定控制字符：`< 0x20` 或 `0x7F`。
fn has_control_char(value: &str) -> bool {
    value
        .chars()
        .any(|character| (character as u32) < 0x20 || (character as u32) == 0x7F)
}

/// 阶段一（反序列化）形状：整份 TOML 文档。除 `[metrics]` 外的任何顶层表都不被接受。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsFile {
    pub metrics: Option<MetricsSection>,
}

/// 阶段一形状：`[metrics]` 段。`enabled` 用 `Option` 承载"是否显式声明"，
/// 缺省策略在 `from_parts` 裁决（缺段=禁用、有段无 `enabled`= 错误）。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    pub enabled: Option<bool>,
    pub endpoint: Option<String>,
    pub auth: Option<MetricsAuthRaw>,
}

/// 阶段一形状：`[metrics.auth]` 段。**全部字段可选**、类型只有 `String`：缺失/空串/
/// 取值非法都不在这里判失败（`deny_unknown_fields` 仍然生效：拼错的键必须在阶段一
/// 就被拒绝）。是否必填由 §3.6 门禁按 `enabled` 裁决。
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsAuthRaw {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl MetricsAuthRaw {
    fn empty() -> Self {
        Self {
            kind: None,
            username: None,
            password: None,
        }
    }
}

/// 校验后的终态：只有这一种形态能离开 config.rs。
#[derive(Clone)]
pub struct MetricsConfig {
    pub enabled: bool,
    /// 已校验的端点，恒以 `/` 开头（保留冲突已在阶段三拦截）。
    pub endpoint: String,
    /// `Some` 当且仅当 `enabled == true`；禁用态不保留凭据副本。
    pub credentials: Option<BasicCredentials>,
}

/// **手写脱敏 `Debug`**（不 `derive(Debug)`，也不依赖字段类型是否实现 `Debug`）：
/// 凭据只以"有无"出现（固定 `[redacted]`），绝不出现原文、用户名或任何长度。
/// 未来给本结构新增凭据类字段时，编译器会强制在同一个 `impl Debug` 里显式处置它。
impl std::fmt::Debug for MetricsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsConfig")
            .field("enabled", &self.enabled)
            .field("endpoint", &self.endpoint)
            // 脱敏：只暴露"有无凭据"，不暴露用户名、长度与密码。
            .field(
                "credentials",
                &self.credentials.as_ref().map(|_| "[redacted]"),
            )
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
    /// 把真实 `std::env::var` 映射到三态。
    pub fn from_std(name: &str) -> Self {
        match std::env::var(name) {
            Ok(value) => Self::Value(value),
            Err(std::env::VarError::NotPresent) => Self::Unset,
            Err(std::env::VarError::NotUnicode(_)) => Self::NotUnicode,
        }
    }
}

impl MetricsConfig {
    /// 纯函数核心：Raw DTO + 环境变量覆盖对 → 终态配置（阶段二 + 阶段三）。
    /// 全部校验与 fail-closed 门禁集中在这里。`override_pair` 是**已经成对**的两个
    /// 环境变量取值；`None` 表示环境变量未设置（空串与 `NotUnicode` 已在
    /// `from_parts_with` 侧分别处置）。
    pub fn from_parts(
        file: Option<MetricsFile>,
        override_pair: Option<(&str, &str)>,
    ) -> Result<Self, ControllerError> {
        let MetricsFile { metrics } = file.unwrap_or(MetricsFile { metrics: None });
        let section_present = metrics.is_some();
        let MetricsSection {
            enabled,
            endpoint,
            auth,
        } = metrics.unwrap_or(MetricsSection {
            enabled: None,
            endpoint: None,
            auth: None,
        });

        // 端点：两种 `enabled` 状态通用（§3.4）。判定顺序是"先保留冲突、后字符形状"，
        // 由 §7.2 `rejects_reserved_endpoint`（把 `/` 归入冲突）钉住。
        let endpoint = endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());
        if is_reserved(&endpoint) {
            return Err(ControllerError::Config(MSG_ENDPOINT_RESERVED.to_string()));
        }
        if !is_valid_endpoint_shape(&endpoint) {
            return Err(ControllerError::Config(MSG_ENDPOINT_INVALID.to_string()));
        }

        // 整段缺失 = 缺省禁用；有段但缺 `enabled` = 必须显式声明意图。
        let enabled = match enabled {
            Some(value) => value,
            None if section_present => {
                return Err(ControllerError::Config(MSG_ENABLED_REQUIRED.to_string()));
            }
            None => false,
        };

        // 阶段二：成对原子覆盖。Raw 里没有 auth 段时按覆盖语义**合成**；Raw 里 `kind`
        // 缺失时一并注入 `basic`（用环境变量交付凭据即表达 basic 意图）；已显式写了
        // `kind`（含非法值）时**保留原值**交给阶段三拒绝——覆盖不猜意图。
        let auth = match override_pair {
            None => auth,
            Some((user, password)) => {
                let mut raw = auth.unwrap_or_else(MetricsAuthRaw::empty);
                raw.username = Some(user.to_string());
                raw.password = Some(password.to_string());
                if raw.kind.is_none() {
                    raw.kind = Some(AUTH_TYPE_BASIC.to_string());
                }
                Some(raw)
            }
        };

        // §3.6 分支 1（禁用放行）：丢弃整个 auth 段，不校验任何凭据字段。
        if !enabled {
            return Ok(Self {
                enabled: false,
                endpoint,
                credentials: None,
            });
        }

        let MetricsAuthRaw {
            kind,
            username,
            password,
        } = auth.unwrap_or_else(MetricsAuthRaw::empty);

        // §3.6 分支 2（凭据存在性，优先于 kind 与字段合法性）。存在性看 `Option`，
        // 不看取值：`username = ""` 是 Some("")，会走到分支 4。
        let (Some(username), Some(password)) = (username, password) else {
            return Err(ControllerError::Config(MSG_AUTH_REQUIRED.to_string()));
        };

        // §3.6 分支 3（协议类型）：逐字节等于 "basic"，区分大小写、不做 trim。
        if kind.as_deref() != Some(AUTH_TYPE_BASIC) {
            return Err(ControllerError::Config(MSG_AUTH_TYPE.to_string()));
        }

        // §3.6 分支 4（字段合法性）：先 username 再 password，同一条件只有一个消息。
        if !is_valid_username(&username) {
            return Err(ControllerError::Config(MSG_USERNAME_INVALID.to_string()));
        }
        if !is_valid_password(&password) {
            return Err(ControllerError::Config(MSG_PASSWORD_INVALID.to_string()));
        }

        // §3.6 分支 5（装配）。
        Ok(Self {
            enabled: true,
            endpoint,
            credentials: Some(BasicCredentials { username, password }),
        })
    }

    /// 生产入口：解析路径 → 读文件 → 读两个环境变量 → 委托 `from_parts`。
    /// 不修改进程环境，不回显任何取值。调用点必须在 `ControllerConfig::from_env()`
    /// 之后、`DbPool::connect()` 之前（§6.4），保证门禁失败时没有任何副作用。
    pub fn from_env() -> Result<Self, ControllerError> {
        Self::from_parts_with(&EnvRead::from_std, &|path| std::fs::read_to_string(path))
    }

    /// 注入式内核：环境变量读取闭包 + 文件读取闭包 → 终态配置。路径解析与覆盖语义的
    /// 全部测试都通过该闭包注入，MUST NOT 使用 `std::env::set_var`（edition 2024 下
    /// 为 `unsafe`，且会跨测试污染进程环境）。
    pub fn from_parts_with(
        env: &dyn Fn(&str) -> EnvRead,
        read_file: &dyn Fn(&Path) -> Result<String, std::io::Error>,
    ) -> Result<Self, ControllerError> {
        // 第 1 步：路径解析。空串按未设置处理（与 HttpSecurityConfig::from_values 的
        // `filter(|v| !v.is_empty())` 惯例一致）。
        let configured = match env(CONFIG_PATH_ENV) {
            EnvRead::NotUnicode => {
                // "显式指定了哪个文件"已不可知，回落默认路径属静默降级 → fail-closed。
                return Err(ControllerError::Config(MSG_PATH_NOT_UNICODE.to_string()));
            }
            EnvRead::Unset => None,
            EnvRead::Value(value) if value.is_empty() => None,
            EnvRead::Value(value) => Some(PathBuf::from(value)),
        };
        let explicit = configured.is_some();
        let path = configured.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));

        // 第 2 步（阶段一）：读取整份 UTF-8 文本并反序列化成 Raw DTO。显式指定的路径
        // 必须存在（静默回落会造成"以为改了其实没改"）；默认路径不存在视为"无配置文件"。
        let file = match read_file(&path) {
            Ok(text) => Some(Self::parse_file(&text)?),
            Err(error) if !explicit && error.kind() == std::io::ErrorKind::NotFound => None,
            // 去敏：不回显路径、行原文或 io::Error 的 Display（其中可能含路径）。
            Err(_) => return Err(ControllerError::Config(MSG_FILE_UNREADABLE.to_string())),
        };

        // 第 3 步（阶段二）：读取两个凭据变量，各自空串按未设置处理。
        let user = match env(AUTH_USER_ENV) {
            EnvRead::NotUnicode => {
                return Err(ControllerError::Config(
                    MSG_AUTH_ENV_NOT_UNICODE.to_string(),
                ));
            }
            EnvRead::Unset => None,
            EnvRead::Value(value) if value.is_empty() => None,
            EnvRead::Value(value) => Some(value),
        };
        let password = match env(AUTH_PASSWORD_ENV) {
            EnvRead::NotUnicode => {
                return Err(ControllerError::Config(
                    MSG_AUTH_ENV_NOT_UNICODE.to_string(),
                ));
            }
            EnvRead::Unset => None,
            EnvRead::Value(value) if value.is_empty() => None,
            EnvRead::Value(value) => Some(value),
        };
        let override_pair = match (&user, &password) {
            (None, None) => None,
            (Some(user), Some(password)) => Some((user.as_str(), password.as_str())),
            // 禁止把文件里的 username 与环境里的 password 拼成半新一半旧的凭据。
            _ => return Err(ControllerError::Config(MSG_OVERRIDE_PARTIAL.to_string())),
        };

        // 第 4 步（阶段三）：唯一凭据判定顺序在 from_parts 内。
        Self::from_parts(file, override_pair)
    }

    /// 纯函数：TOML 文本 → 阶段一 Raw 形状（只检查键集合与类型形状，不判必填）。
    ///
    /// TOML/serde 的错误消息 MUST NOT 复用（其 `Display` 可能含该行原文 → 违反 I5）；
    /// 这里只保留由 `span()` 推出的**行号**（§3.4 规则 6）。
    pub fn parse_file(text: &str) -> Result<MetricsFile, ControllerError> {
        toml::from_str::<MetricsFile>(text).map_err(|error| {
            ControllerError::Config(format!(
                "metrics config: invalid TOML syntax at line {}",
                error
                    .span()
                    .map(|span| line_of(text, span.start))
                    .unwrap_or(0)
            ))
        })
    }
}

/// 字节偏移 → 1 起始行号；非字符边界或越界一律 0（表示"行号未知"）。
fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map(|prefix| prefix.matches('\n').count() + 1)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    /// 规范 §3.2/§10.1 的示例形状（`prom_admin`/`secure_password` 是形状示例，不是可用凭据）。
    const ALIGNED_EXAMPLE: &str = r#"[metrics]
enabled = true
endpoint = "/metrics"

[metrics.auth]
type = "basic"
username = "prom_admin"
password = "secure_password"
"#;
    const USER: &str = "prom_admin";
    const PASSWORD: &str = "secure_password";
    /// 只可能来自被拒绝的 TOML 行原文的秘密，任何错误消息都不得含它（I5）。
    const SECRET_MARKER: &str = "secret-value";

    /// 注入式环境/文件读取（规范 §7.2：MUST NOT 使用 `std::env::set_var`）。
    #[derive(Default)]
    struct Harness {
        /// 路径 → 读取结果；未列出的路径按 `NotFound` 处置。
        files: BTreeMap<PathBuf, Result<String, std::io::ErrorKind>>,
        env: BTreeMap<&'static str, EnvRead>,
    }

    impl Harness {
        fn new() -> Self {
            Self::default()
        }
        /// 默认路径存在且可读。
        fn text(self, text: &str) -> Self {
            self.file(DEFAULT_CONFIG_PATH, text)
        }
        fn file(self, path: &str, text: &str) -> Self {
            self.files(path, Ok(text.to_string()))
        }
        fn files(mut self, path: &str, outcome: Result<String, std::io::ErrorKind>) -> Self {
            self.files.insert(PathBuf::from(path), outcome);
            self
        }
        /// 默认路径存在但不可读（权限位/IO 错误）。
        fn unreadable_default(self) -> Self {
            self.files(
                DEFAULT_CONFIG_PATH,
                Err(std::io::ErrorKind::PermissionDenied),
            )
        }
        /// 默认路径存在但不是合法 UTF-8（`read_to_string` 的 `InvalidData`）。
        fn non_utf8_default(self) -> Self {
            self.files(DEFAULT_CONFIG_PATH, Err(std::io::ErrorKind::InvalidData))
        }
        fn env(mut self, key: &'static str, value: EnvRead) -> Self {
            self.env.insert(key, value);
            self
        }
        fn pair(self, user: EnvRead, password: EnvRead) -> Self {
            let first = self.env(AUTH_USER_ENV, user);
            first.env(AUTH_PASSWORD_ENV, password)
        }
        fn load(&self) -> Result<MetricsConfig, ControllerError> {
            MetricsConfig::from_parts_with(
                &|name| self.env.get(name).cloned().unwrap_or(EnvRead::Unset),
                &|path| match self.files.get(path) {
                    Some(Ok(text)) => Ok(text.clone()),
                    Some(Err(kind)) => Err(std::io::Error::new(*kind, "injected")),
                    None => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "absent")),
                },
            )
        }
    }

    /// 直接驱动阶段二 + 阶段三纯函数（文件已由 `parse_file` 变成 Raw DTO）。
    fn parts(text: &str, pair: Option<(&str, &str)>) -> Result<MetricsConfig, ControllerError> {
        MetricsConfig::from_parts(Some(MetricsConfig::parse_file(text)?), pair)
    }

    /// `Err` 分支的固定文案（`ControllerError::Config` 的载荷）。
    fn message(error: &ControllerError) -> String {
        match error {
            ControllerError::Config(text) => text.clone(),
            other => panic!("必须是 ControllerError::Config，实得 {other:?}"),
        }
    }

    /// §3.2 示例 TOML 全量解析。
    #[test]
    fn parses_aligned_example() {
        let config = Harness::new().text(ALIGNED_EXAMPLE).load().unwrap();
        assert!(config.enabled);
        assert_eq!(config.endpoint, "/metrics");
        let credentials = config.credentials.expect("启用态必须携带凭据");
        assert_eq!(credentials.username, USER);
        assert_eq!(credentials.password, PASSWORD);
    }

    /// 空文档 / 无 `[metrics]`：合法禁用态，端点缺省、无凭据。
    #[test]
    fn omitting_metrics_section_is_disabled() {
        for text in ["", "# 只有注释\n", "[other]\n"] {
            let outcome = Harness::new().text(text).load();
            if text == "[other]\n" {
                // 非 `[metrics]` 的顶层表不在接受范围内（deny_unknown_fields）。
                assert!(outcome.is_err(), "未知顶层表必须失败：{text:?}");
                continue;
            }
            let config =
                outcome.unwrap_or_else(|error| panic!("{text:?} 必须合法：{}", message(&error)));
            assert!(!config.enabled);
            assert_eq!(config.endpoint, DEFAULT_ENDPOINT);
            assert!(config.credentials.is_none());
        }
    }

    /// `[metrics]` 存在但缺 `enabled`：表已存在即必须显式声明意图（整段缺失才允许缺省）。
    #[test]
    fn metrics_section_requires_enabled() {
        let error = Harness::new()
            .text("[metrics]\nendpoint = \"/metrics\"\n")
            .load()
            .expect_err("缺 enabled 必须失败");
        assert_eq!(message(&error), "metrics.enabled is required");
        // 对照：整段缺失才是合法的缺省禁用。
        assert!(!Harness::new().text("").load().unwrap().enabled);
    }

    /// 未知顶层表（`deny_unknown_fields`）。
    #[test]
    fn rejects_unknown_top_level_table() {
        let error = Harness::new()
            .text("[database]\nurl = \"mysql://x\"\n")
            .load()
            .expect_err("未知顶层表必须失败");
        assert!(
            message(&error).starts_with("metrics config: invalid TOML syntax at line "),
            "阶段一错误必须是只含行列号的固定形状，实得 {}",
            message(&error)
        );
    }

    /// `[metrics]` 内拼错的键。
    #[test]
    fn rejects_unknown_key_in_metrics() {
        let error = Harness::new()
            .text("[metrics]\nenabled = false\nendpont = \"/x\"\n")
            .load()
            .expect_err("拼错的键必须失败");
        assert!(message(&error).starts_with("metrics config: invalid TOML syntax at line "));
    }

    /// `[metrics.auth]` 内拼错的键（禁用态同样拒绝：该规则属阶段一）。
    #[test]
    fn rejects_unknown_key_in_auth() {
        for text in [
            "[metrics]\nenabled = true\n\n[metrics.auth]\npasswaerd = \"x\"\n",
            "[metrics]\nenabled = false\n\n[metrics.auth]\npasswaerd = \"x\"\n",
        ] {
            let error = Harness::new()
                .text(text)
                .load()
                .expect_err("未知键必须失败：{text}");
            assert!(message(&error).starts_with("metrics config: invalid TOML syntax at line "));
        }
    }

    /// 阶段一只检查形状：写了半截的 auth 段必须反序列化成功（必填性由门禁裁决）。
    #[test]
    fn accepts_incomplete_auth_section_shape() {
        MetricsConfig::parse_file(
            "[metrics]\nenabled = true\n\n[metrics.auth]\nusername = \"u\"\n",
        )
        .expect("半截 auth 段在阶段一必须合法（Raw DTO 全字段 Option）");
    }

    /// TOML 语法错误：消息只含行列号，绝不含该行原文（I5 / §3.4 规则 6）。
    #[test]
    fn rejects_invalid_toml_without_leaking_values() {
        let error = Harness::new()
            .text(&format!(
                "[metrics\nenabled = false\n[metrics.auth]\npassword = \"{SECRET_MARKER}\"\n"
            ))
            .load()
            .expect_err("语法错误必须失败");
        let text = message(&error);
        assert!(
            text.starts_with("metrics config: invalid TOML syntax at line "),
            "{text}"
        );
        assert!(
            !text.contains(SECRET_MARKER),
            "错误消息回显了配置行原文：{text}"
        );
        assert!(!text.contains("password"), "错误消息回显了键名原文：{text}");
    }

    /// 启用态 + 凭据齐备 + 非 `basic` 的 `type`（区分大小写、不 trim）。
    #[test]
    fn rejects_non_basic_auth_type() {
        for kind in ["bearer", "none", "token", "", "Basic", " basic"] {
            let text = format!(
                "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"{kind}\"\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
            );
            let error = Harness::new()
                .text(&text)
                .load()
                .expect_err("非法 kind 必须失败");
            assert_eq!(message(&error), "metrics.auth.type must be \"basic\"");
        }
    }

    /// 凭据齐备但缺 `type`：文件写了 auth 段却不声明 type 是配置缺陷。
    #[test]
    fn rejects_missing_auth_type_when_enabled_without_env() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
        );
        let error = Harness::new()
            .text(&text)
            .load()
            .expect_err("缺 type 必须失败");
        assert_eq!(message(&error), "metrics.auth.type must be \"basic\"");
    }

    /// 存在性检查先于 kind（§3.6 分支 2 优先于分支 3）。
    #[test]
    fn missing_credentials_precede_type_error() {
        let error = Harness::new()
            .text("[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"bearer\"\n")
            .load()
            .expect_err("缺凭据必须失败");
        assert_eq!(
            message(&error),
            "metrics.auth is required when metrics.enabled is true"
        );
    }

    /// 存在性检查先于字段合法性（已写字段含非法字符也不得抢报）。
    #[test]
    fn missing_credentials_precede_field_error() {
        let error = Harness::new()
            .text("[metrics]\nenabled = true\n\n[metrics.auth]\nusername = \"a:b\"\n")
            .load()
            .expect_err("缺 password 必须失败");
        assert_eq!(
            message(&error),
            "metrics.auth is required when metrics.enabled is true"
        );
    }

    /// 密码允许冒号（RFC 7617 §2 只约束 userid）。
    #[test]
    fn accepts_password_containing_colons() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"a:b:c\"\n"
        );
        let config = Harness::new().text(&text).load().unwrap();
        assert_eq!(config.credentials.expect("凭据必须齐备").password, "a:b:c");
    }

    /// `endpoint` 不以 `/` 开头。
    #[test]
    fn rejects_endpoint_without_leading_slash() {
        let error = Harness::new()
            .text("[metrics]\nenabled = false\nendpoint = \"metrics\"\n")
            .load()
            .expect_err("端点必须绝对路径");
        assert_eq!(message(&error), "metrics.endpoint is invalid");
    }

    /// 尾斜杠、查询串、空白、空段、大写都属于字符形状非法。
    #[test]
    fn rejects_endpoint_with_trailing_slash_query_or_space() {
        for endpoint in [
            "/metrics/",
            "/metrics?x=1",
            "/met rics",
            "/a//b",
            "/METRICS",
        ] {
            let text = format!("[metrics]\nenabled = false\nendpoint = \"{endpoint}\"\n");
            let error = Harness::new()
                .text(&text)
                .load()
                .expect_err("非法端点必须失败：{endpoint}");
            assert_eq!(message(&error), "metrics.endpoint is invalid", "{endpoint}");
        }
    }

    /// 命中保留路径集合（含被显式排除的 `/`）。
    #[test]
    fn rejects_reserved_endpoint() {
        for endpoint in ["/healthz", "/api/v1/tasks", "/devices", "/assets/x", "/"] {
            let text = format!("[metrics]\nenabled = false\nendpoint = \"{endpoint}\"\n");
            let error = Harness::new()
                .text(&text)
                .load()
                .expect_err("冲突端点必须失败：{endpoint}");
            assert_eq!(
                message(&error),
                "metrics.endpoint collides with an existing route",
                "{endpoint}"
            );
        }
    }

    /// `/` 只按完全相等判定；前缀匹配必须有 `/` 边界（§6.5）。
    #[test]
    fn reserved_root_path_is_exact_match_only() {
        assert!(is_reserved("/"));
        for path in [
            "/metrics",
            "/metrics/deep",
            "/apiv1",
            "/assetsx",
            "/healthz2",
        ] {
            assert!(!is_reserved(path), "{path} 不得被判为保留路径");
        }
        for path in [
            "/healthz",
            "/readyz",
            "/login",
            "/password",
            "/sessions",
            "/devices",
            "/tasks",
            "/admin",
            "/api",
            "/api/v1/tasks",
            "/assets",
            "/assets/app.js",
            "/devices/00000000000000000000000000000001",
            "/tasks/1",
            "/admin/users",
        ] {
            assert!(is_reserved(path), "{path} 必须属保留集合");
        }
    }

    /// 保留集合的两个维度都必须在 `enabled = false` 时生效（§3.4 规则 4）。
    #[test]
    fn endpoint_rules_apply_when_disabled() {
        let error = Harness::new()
            .text("[metrics]\nenabled = false\nendpoint = \"/healthz\"\n")
            .load()
            .expect_err("禁用态仍要拦截冲突路径");
        assert_eq!(
            message(&error),
            "metrics.endpoint collides with an existing route"
        );
        let error = Harness::new()
            .text("[metrics]\nenabled = false\nendpoint = \"/METRICS\"\n")
            .load()
            .expect_err("禁用态仍要校验字符形状");
        assert_eq!(message(&error), "metrics.endpoint is invalid");
    }

    /// `len ≤ 128`（§3.4）。
    #[test]
    fn rejects_overlong_endpoint() {
        let long = format!("/{}", "a".repeat(128));
        assert_eq!(long.len(), 129);
        let error = Harness::new()
            .text(&format!(
                "[metrics]\nenabled = false\nendpoint = \"{long}\"\n"
            ))
            .load()
            .expect_err("超长端点必须失败");
        assert_eq!(message(&error), "metrics.endpoint is invalid");
        let edge = format!("/{}", "a".repeat(127));
        assert_eq!(edge.len(), 128);
        let text = format!("[metrics]\nenabled = false\nendpoint = \"{edge}\"\n");
        assert!(!Harness::new().text(&text).load().unwrap().enabled);
    }

    /// username 禁冒号（RFC 7617 §2）与控制字符；password 只禁控制字符。
    #[test]
    fn rejects_username_with_colon_or_control_char() {
        // TOML basic string 禁止原始控制字符，用 TOML 转义序列注入 BEL：反序列化后即真实控制字符。
        for username in ["a:b", r"a\u0007"] {
            let text = format!(
                "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{username}\"\npassword = \"{PASSWORD}\"\n"
            );
            let error = Harness::new()
                .text(&text)
                .load()
                .expect_err("非法 username 必须失败");
            assert_eq!(message(&error), "metrics.auth.username is invalid");
        }
    }

    #[test]
    fn rejects_password_with_control_char() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"a\\u0007b\"\n"
        );
        let error = Harness::new()
            .text(&text)
            .load()
            .expect_err("非法 password 必须失败");
        assert_eq!(message(&error), "metrics.auth.password is invalid");
    }

    /// 启用但完全没有 `[metrics.auth]`，也没有环境变量 → 拒绝启动。
    #[test]
    fn fail_closed_when_enabled_without_auth() {
        let error = Harness::new()
            .text("[metrics]\nenabled = true\n")
            .load()
            .expect_err("启用无凭据必须失败");
        assert_eq!(
            message(&error),
            "metrics.auth is required when metrics.enabled is true"
        );
    }

    /// 空串是"已提供但非法"（`Some("")`）→ 命中分支 4，不命中分支 2。
    #[test]
    fn rejects_blank_username_when_enabled() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"\"\npassword = \"{PASSWORD}\"\n"
        );
        let error = Harness::new()
            .text(&text)
            .load()
            .expect_err("空 username 必须失败");
        assert_eq!(message(&error), "metrics.auth.username is invalid");
    }

    #[test]
    fn rejects_blank_password_when_enabled() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"\"\n"
        );
        let error = Harness::new()
            .text(&text)
            .load()
            .expect_err("空 password 必须失败");
        assert_eq!(message(&error), "metrics.auth.password is invalid");
    }

    /// 分支 1：禁用态丢弃整个 auth 段（含合法凭据），终态不保留密码副本。
    #[test]
    fn disabled_drops_credentials_without_error() {
        let text = format!(
            "[metrics]\nenabled = false\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
        );
        let config = Harness::new().text(&text).load().unwrap();
        assert!(!config.enabled);
        assert!(config.credentials.is_none());
    }

    /// 分支 1 的核心回归网：禁用 + 半截/非法 auth 都不得阻断启动。
    #[test]
    fn disabled_tolerates_incomplete_auth() {
        for tail in [
            "[metrics.auth]\n",
            "[metrics.auth]\nusername = \"prom_admin\"\n",
            "[metrics.auth]\ntype = \"basc\"\n",
            "[metrics.auth]\ntype = \"basic\"\npassword = \"\"\n",
        ] {
            let text = format!("[metrics]\nenabled = false\n\n{tail}");
            let config = Harness::new()
                .text(&text)
                .load()
                .unwrap_or_else(|error| panic!("禁用态必须放行 {tail:?}：{}", message(&error)));
            assert!(!config.enabled);
            assert!(config.credentials.is_none());
        }
    }

    /// 环境变量成对覆盖文件凭据。
    #[test]
    fn env_pair_overrides_file_credentials() {
        let config = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .pair(
                EnvRead::Value("env_user".into()),
                EnvRead::Value("env_password".into()),
            )
            .load()
            .unwrap();
        let credentials = config.credentials.expect("凭据必须齐备");
        assert_eq!(credentials.username, "env_user");
        assert_eq!(credentials.password, "env_password");
    }

    /// 文件只有 `enabled = true`：环境变量对按覆盖语义**合成** auth 段并推导 kind。
    #[test]
    fn env_pair_synthesizes_auth_section_when_absent() {
        let config = Harness::new()
            .text("[metrics]\nenabled = true\n")
            .pair(
                EnvRead::Value("env_user".into()),
                EnvRead::Value("env_password".into()),
            )
            .load()
            .unwrap();
        assert!(config.enabled);
        assert_eq!(config.credentials.expect("必须有凭据").username, "env_user");
    }

    /// 凭据齐备且文件缺 `type` 时，由环境变量推导注入 `basic`（§3.3 第 3 步）。
    #[test]
    fn env_pair_derives_basic_when_type_absent() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
        );
        let config = Harness::new()
            .text(&text)
            .pair(
                EnvRead::Value("env_user".into()),
                EnvRead::Value("env_password".into()),
            )
            .load()
            .unwrap();
        assert_eq!(
            config.credentials.expect("必须有凭据").password,
            "env_password"
        );
    }

    /// 覆盖不猜意图：文件里显式写了非法 `kind` 时保留原值交给阶段三拒绝。
    #[test]
    fn env_does_not_override_illegal_type() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"bearer\"\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
        );
        let error = Harness::new()
            .text(&text)
            .pair(
                EnvRead::Value("env_user".into()),
                EnvRead::Value("env_password".into()),
            )
            .load()
            .expect_err("非法 kind 不得被覆盖救回");
        assert_eq!(message(&error), "metrics.auth.type must be \"basic\"");
    }

    /// 只设置其一即拒绝：禁止半新一半旧的混搭凭据。
    #[test]
    fn env_partial_override_is_rejected() {
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .env(AUTH_USER_ENV, EnvRead::Value("env_user".into()))
            .load()
            .expect_err("partial 覆盖必须失败");
        assert_eq!(
            message(&error),
            "metrics auth override must set both user and password"
        );
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .env(AUTH_PASSWORD_ENV, EnvRead::Value("env_password".into()))
            .load()
            .expect_err("partial 覆盖必须失败");
        assert_eq!(
            message(&error),
            "metrics auth override must set both user and password"
        );
    }

    /// 空串按未设置处理，因此与「只设另一个」等价。
    #[test]
    fn env_empty_string_counts_as_unset() {
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .pair(
                EnvRead::Value(String::new()),
                EnvRead::Value("env_password".into()),
            )
            .load()
            .expect_err("空串 user + 非空 password 属 partial");
        assert_eq!(
            message(&error),
            "metrics auth override must set both user and password"
        );
        // 两者皆空串 = 都未设置：文件凭据原样生效。
        let config = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .pair(EnvRead::Value(String::new()), EnvRead::Value(String::new()))
            .load()
            .unwrap();
        assert_eq!(config.credentials.expect("必须有凭据").username, USER);
    }

    /// 设置了但不可用（非 Unicode）绝不当作"未设置"继续沿用文件凭据。
    #[test]
    fn env_non_unicode_password_is_error() {
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .pair(EnvRead::Value("env_user".into()), EnvRead::NotUnicode)
            .load()
            .expect_err("非 Unicode 密码必须是配置错误");
        assert_eq!(
            message(&error),
            "metrics auth override env var is not valid unicode"
        );
    }

    #[test]
    fn env_non_unicode_user_is_error() {
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .pair(EnvRead::NotUnicode, EnvRead::Value("env_password".into()))
            .load()
            .expect_err("非 Unicode 用户名必须是配置错误");
        assert_eq!(
            message(&error),
            "metrics auth override env var is not valid unicode"
        );
    }

    /// `CONTROLLER_CONFIG` 非 Unicode：不回落默认路径，一律 fail-closed。
    #[test]
    fn config_path_non_unicode_is_error() {
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .env(CONFIG_PATH_ENV, EnvRead::NotUnicode)
            .load()
            .expect_err("非 Unicode 路径必须失败");
        assert_eq!(message(&error), "metrics config path is not valid unicode");
    }

    /// 所有 `Err` 分支的消息都不含用户名/密码/秘密字面量（I5 反向保证）。
    #[test]
    fn error_messages_never_echo_credentials() {
        let scenarios: Vec<(&str, Harness)> = vec![
            ("未知顶层表", Harness::new().text("[database]\nurl = \"x\"\n")),
            ("未知键", Harness::new().text("[metrics]\nenabled = false\nendpont = \"/x\"\n")),
            (
                "缺 enabled",
                Harness::new().text("[metrics]\nendpoint = \"/metrics\"\n"),
            ),
            ("语法错误", Harness::new().text(&format!("[metrics\npassword = \"{SECRET_MARKER}\"\n"))),
            ("冲突路径", Harness::new().text("[metrics]\nenabled = true\nendpoint = \"/healthz\"\n")),
            ("非法路径", Harness::new().text("[metrics]\nenabled = true\nendpoint = \"metrics\"\n")),
            (
                "缺 type",
                Harness::new().text(&format!("[metrics]\nenabled = true\n\n[metrics.auth]\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n")),
            ),
            (
                "非法 kind",
                Harness::new().text(&format!("[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"bearer\"\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n")),
            ),
            ("缺凭据", Harness::new().text("[metrics]\nenabled = true\n")),
            (
                "非法 username",
                Harness::new().text(&format!("[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"a:b\"\npassword = \"{PASSWORD}\"\n")),
            ),
            (
                "非法 password",
                Harness::new().text(&format!("[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"a\\u0007b\"\n")),
            ),
            ("路径非 Unicode", Harness::new().text(ALIGNED_EXAMPLE).env(CONFIG_PATH_ENV, EnvRead::NotUnicode)),
            ("覆盖 partial", Harness::new().text(ALIGNED_EXAMPLE).env(AUTH_USER_ENV, EnvRead::Value(USER.into()))),
            ("覆盖非 Unicode", Harness::new().text(ALIGNED_EXAMPLE).env(AUTH_PASSWORD_ENV, EnvRead::NotUnicode)),
            ("不可读文件", Harness::new().unreadable_default()),
            ("非 UTF-8 文件", Harness::new().non_utf8_default()),
            ("显式路径缺失", Harness::new().env(CONFIG_PATH_ENV, EnvRead::Value("/etc/rsetup/absent.toml".into()))),
        ];
        for (label, harness) in scenarios {
            let error = match harness.load() {
                Ok(config) => panic!("{label} 必须失败，实得 {config:?}"),
                Err(error) => error,
            };
            let text = message(&error);
            for marker in [
                USER,
                PASSWORD,
                SECRET_MARKER,
                "controller.toml",
                "/etc/rsetup/absent.toml",
            ] {
                assert!(
                    !text.contains(marker),
                    "{label} 的错误消息回显了 {marker:?}：{text}"
                );
            }
        }
    }

    /// 手写 `Debug`：凭据只以"有无"出现（§3.2）。
    #[test]
    fn metrics_config_debug_is_redacted() {
        let config = Harness::new().text(ALIGNED_EXAMPLE).load().unwrap();
        let text = format!("{config:?}");
        assert!(
            text.contains("[redacted]"),
            "Debug 必须含固定脱敏文案：{text}"
        );
        assert!(!text.contains(USER), "Debug 回显了用户名：{text}");
        assert!(!text.contains(PASSWORD), "Debug 回显了密码：{text}");
        assert!(
            text.contains("enabled: true"),
            "Debug 必须保留 enabled/endpoint：{text}"
        );
        assert!(text.contains("/metrics"), "Debug 必须保留 endpoint：{text}");
        let disabled = Harness::new()
            .text("[metrics]\nenabled = false\n")
            .load()
            .unwrap();
        assert!(format!("{disabled:?}").contains("credentials: None"));
    }

    // -----------------------------------------------------------------
    // 路径解析与覆盖语义（全部经注入闭包，不使用 std::env::set_var）
    // -----------------------------------------------------------------

    #[test]
    fn explicit_path_missing_is_error() {
        let error = Harness::new()
            .env(
                CONFIG_PATH_ENV,
                EnvRead::Value("/etc/rsetup/absent.toml".into()),
            )
            .load()
            .expect_err("显式指定的路径不存在必须失败");
        assert!(
            !message(&error).contains("absent.toml"),
            "错误消息不得回显路径"
        );
    }

    #[test]
    fn explicit_path_present_is_used() {
        let config = Harness::new()
            .env(
                CONFIG_PATH_ENV,
                EnvRead::Value("/etc/rsetup/custom.toml".into()),
            )
            .file("/etc/rsetup/custom.toml", ALIGNED_EXAMPLE)
            .load()
            .unwrap();
        assert!(config.enabled);
    }

    #[test]
    fn default_path_absent_is_ok() {
        let config = Harness::new().load().unwrap();
        assert!(!config.enabled);
        assert_eq!(config.endpoint, DEFAULT_ENDPOINT);
    }

    #[test]
    fn default_path_present_is_used() {
        assert!(Harness::new().text(ALIGNED_EXAMPLE).load().unwrap().enabled);
    }

    #[test]
    fn empty_config_env_falls_back_to_default_path() {
        let config = Harness::new()
            .env(CONFIG_PATH_ENV, EnvRead::Value(String::new()))
            .text(ALIGNED_EXAMPLE)
            .load()
            .unwrap();
        assert!(config.enabled, "空串路径必须回落到默认路径并读到配置");
    }

    #[test]
    fn unreadable_file_is_error() {
        let error = Harness::new()
            .unreadable_default()
            .load()
            .expect_err("不可读文件必须失败");
        assert!(!message(&error).contains("controller.toml"), "不得回显路径");
    }

    #[test]
    fn non_utf8_file_is_error() {
        let error = Harness::new()
            .non_utf8_default()
            .load()
            .expect_err("非 UTF-8 文件必须失败");
        assert!(!message(&error).is_empty());
    }

    /// 三阶段责任不重叠：阶段二只写数据、阶段三只读数据（用纯函数钉住）。
    #[test]
    fn override_happens_before_gate() {
        // 文件里禁用 + 环境变量给凭据：合法，凭据在阶段三被丢弃。
        let config = Harness::new()
            .text("[metrics]\nenabled = false\n")
            .pair(
                EnvRead::Value("env_user".into()),
                EnvRead::Value("env_password".into()),
            )
            .load()
            .unwrap();
        assert!(!config.enabled);
        assert!(config.credentials.is_none());
        // 文件里启用 + 环境变量补凭据：合法。
        let config = parts(
            "[metrics]\nenabled = true\n",
            Some(("env_user", "env_password")),
        )
        .unwrap();
        assert_eq!(config.credentials.expect("必须有凭据").username, "env_user");
        // 文件里启用 + 无凭据：非法。
        assert!(parts("[metrics]\nenabled = true\n", None).is_err());
    }
}

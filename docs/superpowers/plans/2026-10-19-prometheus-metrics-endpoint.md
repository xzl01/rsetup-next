# Controller Prometheus 指标端点 TDD 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 `rsetup-controller` 增加 Prometheus text 0.0.4 拉取式指标端点（同端口、Basic Auth、TOML 配置文件 + 环境变量成对覆盖、启动期 fail-closed），并把「并发闸许可与数据库连接全生命周期绑定」实现为可验证的硬门禁。

**Architecture:** 新增 `crates/rsetup-controller/src/metrics/` 模块（`mod`/`config`/`auth`/`model`/`render`/`collect`/`http` 七个文件，依赖方向单向：`http → {auth, collect, render, config}`、`collect → model`、`render → model`）。配置层是三阶段纯函数（Raw DTO → 环境变量成对覆盖 → 统一门禁），渲染层是纯函数，采集层用 dyn-safe trait 注入。指标线作为第六线由 `production::build_production_router` 唯一装配点合并进顶层 router，其连接池与汇总源都在函数内部由 `inputs.auth.db` 派生（I1/I2）。采集内核 `snapshot_via` 在**同一任务内**完成 `acquire → 聚合 → close`，`close` 之前彻底禁止 `?` 早退；许可由 `tokio::spawn` 的独立任务持有，随该任务返回（= 连接已完全关闭）才释放（I7）。

**Tech Stack:** Rust 2024（toolchain 1.99 本地 / MSRV 1.85）、`axum 0.8.9`、`tokio 1.47`（`sync`/`time`）、`sqlx 0.8.6`（mysql / runtime-tokio）、`serde`、新增直接依赖 `toml = "0.8"`、`base64 = "0.22"`、`subtle = "2.6"`（后两者与 `Cargo.lock` 已锁版本同版，仅提升为直接依赖）、`sha2`（既有）。测试：`tower::ServiceExt::oneshot`（L2 进程内 HTTP）、`cargo test`（L1/L2/L3）、`#[ignore]` 真库用例（L4，需操作员逐引擎授权）。

**Spec:** `docs/superpowers/specs/2026-10-19-prometheus-metrics-endpoint-design.md`（commit `717d9c7`）。本计划逐节实现该规范；规范是唯一的契约来源，本计划只把它拆成 RED→GREEN 的增量。

## 前置说明（执行者必读）

1. **文件命名对照**：任务书里的 `src/metrics/handler.rs` 对应规范 §6.1 模块布局表中的 `metrics/http.rs`（同一职责文件：状态、路由与 handler），本计划按规范名 `http.rs` 执行；规范 §6.1 的模块表与 §7.1 的测试层次表是文件归属的权威来源。`MockMetricsSnapshotSource`（任务书名）= 规范 §5.5/§7.5 的 fake source，本计划统一命名为 `FakeMetricsSnapshotSource`（`#[cfg(test)]`，仅存在于库内测试构建），L3 的装配测试另有定义在测试文件内的 `SyntheticMetricsSnapshotSource` 与 `HangingMetricsSnapshotSource`。
2. **依赖获取**：`toml 0.8` 首次进入本工作区（`Cargo.lock` 尚无该 crate，本机 `~/.cargo/registry/cache` 也无 `.crate` 包），Task 1 Step 2 起需要网络以更新 `Cargo.lock`；`base64 0.22.1` 与 `subtle 2.6.1` 已在 `Cargo.lock` 与本地 cache 中，仅提升为直接依赖，不新增依赖树（规范 §3.1 约束 2、§8 R10）。离线环境先在有 vendor 的机器上 `cargo fetch` 后再继续。
3. **凭据保护**：本计划所有示例凭据一律是规范指定的形状示例 `prom_admin` / `secure_password`，不是可用凭据；任何任务都不得把真实 TOML 配置、真实 DSN 或真实凭据写入源码、fixture、日志或提交（仓库根 `AGENTS.md`、规范 §3.7、I5）。提交前检查 `git diff --cached` 不含 `controller.toml` 实体与任何真实凭据（规范 §7.8）。
4. **提交信息**：按仓库 `AGENTS.md`，所有 commit message 标题与正文用简体中文（Conventional Commits 的类型/作用域标记保留英文）。
5. **门禁命令**：`cargo fmt` 用 `cargo fmt --package rsetup-controller -- --check`（仓库 `Makefile` 的 `FMT_PACKAGES` 显式列包，不用 `--all`）；全量门禁见 Task 7。
6. **RED 的含义**：Rust 的 RED 通常表现为编译失败（未定义的类型/函数、参数个数不符）。本计划每个 Step 2 都写出**预期失败的关键错误码与判据**，不要求编译器文案逐字节一致；但「必须 FAIL」与「FAIL 的原因类别」是硬要求。禁止用 `todo!()`/`unimplemented!()` 制造假 GREEN。

## 文件结构（本计划交付的全部文件与职责）

| 文件 | 动作 | 职责（规范出处） |
| --- | --- | --- |
| `Cargo.toml` | Modify | `[workspace.dependencies]` 增加 `toml`/`base64`/`subtle`（§3.1） |
| `crates/rsetup-controller/Cargo.toml` | Modify | `[dependencies]` 以 `workspace = true` 引用三者（§3.1） |
| `crates/rsetup-controller/src/lib.rs` | Modify | 追加 `pub mod metrics;`（§6.1） |
| `crates/rsetup-controller/src/metrics/mod.rs` | Create | 模块头契约注释、`EDITION` 常量与清单一致性测试、公开再导出（§5.2.1、§6.1） |
| `crates/rsetup-controller/src/metrics/config.rs` | Create | Raw DTO、三阶段加载、路径与环境变量覆盖、`RESERVED_PATHS`/`is_reserved`、统一 fail-closed 门禁、手写脱敏 `Debug`（§3） |
| `crates/rsetup-controller/src/metrics/auth.rs` | Create | `ParsedBasic`（owned）、`parse_authorization`、`subtle` 常量时间 `verify`、`WWW-Authenticate` 值（§4.2/§4.3/§4.4） |
| `crates/rsetup-controller/src/metrics/model.rs` | Create | `ScrapeValues`/`DeviceCounts`/`TaskCounts`/`PoolCounts`、`admission_label`/`task_bucket` 字节精确映射与两条去敏错误（§5.2） |
| `crates/rsetup-controller/src/metrics/render.rs` | Create | `CONTENT_TYPE`、`MAX_BODY_BYTES`、`render`（11 条固定序列、三项转义）（§5.1） |
| `crates/rsetup-controller/src/metrics/collect.rs` | Create | `MetricsSnapshotSource`、`FakeMetricsSnapshotSource`（`#[cfg(test)]`）、`SqlxMetricsSnapshotSource`、`MetricsRuntime`、`snapshot_via`（§5.5.1 六条硬规则）、`pool_counts`、三个常量（§5.3/§5.5/§5.5.1） |
| `crates/rsetup-controller/src/metrics/http.rs` | Create | `MetricsState`、`build_metrics_router`、`scrape`（§4.5 固定顺序 + HEAD 短路 + 许可移交）（§6.2） |
| `crates/rsetup-controller/src/production.rs` | Modify | `MetricsLineInputs`、`ProductionInputs.metrics`、六参数 `compose_component_routers`、内部派生汇总源、`build_production_router_with_metrics_source` 注入缝（§6.3） |
| `crates/rsetup-controller/src/web.rs` | Modify | 三张路由表白名单常量改 `pub`（值不变），供 §7.6 第 8 条做保留集合的漂移一致性断言（§6.5） |
| `crates/rsetup-controller/src/main.rs` | Modify | uptime 零点为 `main` 首条语句、`MetricsConfig::from_env()` 前置于建池与绑端口、`production_inputs` 扩展（§6.4） |
| `crates/rsetup-controller/tests/production_composition.rs` | Modify | 六线合并、注入缝、结构性与源码级门禁（§7.6） |
| `crates/rsetup-controller/tests/metrics_real_db.rs` | Create | L4 真库三组验收（含 `CAST(... AS BINARY)` 双引擎与 PAD SPACE 对照、多波超时、受控关闭屏障），全部 `#[ignore]`（§7.7） |
| `Makefile` | Modify | `metrics-gates` 目标：9 条静态门禁（§7.8） |

**依赖方向硬约束（§6.1）**：`metrics/config.rs` 与 `metrics/model.rs` 不得 `use axum`/`sqlx`；`render`、`model` 是纯函数层（无 I/O、无 async）；`metrics/*` 任何文件都不得 `use crate::http_auth::{HttpAuthState, EndpointId}`、`crate::auth::service::Session` 或调用 `http_gate::authenticate_read`/`authenticate_write`（I3、I6）。

## Global Constraints

以下 7 条全局不变量逐字复制自规范 §2，**每一个 Task 的验收都隐含包含本节全部 7 条**；任一条不成立即实现缺陷：

- **I1 单一端口**：指标路由必须经 `production::build_production_router` 合并进唯一顶层 router，不得在 `main.rs` 或任何测试之外另行 `axum::serve`。
- **I2 单一连接池**：全进程只有 `DbPool::connect` 一处构造连接池；指标线持有的必须是同一 `DbPool` 的 `Clone`（内部 `Arc` 共享），不得自建第二个池。
- **I3 边界隔离**：指标线的任何类型都不得包含 `Arc<HttpAuthState>`、`Session` 或 `EndpointId`；指标 handler 不得调用 `http_gate` 的认证函数，也不得向 `HttpAuthState` 的 session/CSRF/限速结构写入任何内容。
- **I4 失败关闭**：配置非法、凭据缺失、采集失败一律 fail-closed（启动失败或 5xx），不得降级为"看起来正常"的空指标或常量 0。
- **I5 去敏**：所有错误消息、日志、指标文本与测试报告都不得回显密码、base64 凭据原文或配置文件的行原文。
- **I6 不污染业务契约**：不新增 `EndpointId` 变体、不新增 permission、不写 `audit_events`（抓取是高频只读行为，写审计会造成写入放大并污染安全审计的可读性）。
- **I7 生命周期对齐**：指标线每一条连接从借出、查询到关闭的整个生命周期都被**恰好一个**并发闸许可（permit）持有，许可只有在底层连接**完全关闭完毕**后才释放（指标连接一律销毁、不回空闲池，§5.5.1）。由此无条件成立：任何时刻指标线"在途 + 关闭中"的连接总数 ≤ `MAX_CONCURRENT_SCRAPE = 4`，共享池默认 `max_connections = 10`——**指标线任何时刻在途与关闭中的连接数至多占 4 槽，连接池中至少 6 槽容量属于主业务、绝对不被指标线占用或挤占**（该结论只封顶指标线侧占用，不对业务自身用量作任何假设）——连续多波慢抓取**不得**发生连接累积饥饿。

---

## Task 1: 依赖声明与配置层实现（`toml`/`base64`/`subtle` + `metrics/config.rs`）

**Files:**
- Create: `crates/rsetup-controller/src/metrics/mod.rs`（本 Task 只写模块声明、`EDITION` 常量与清单一致性测试）
- Create: `crates/rsetup-controller/src/metrics/config.rs`
- Modify: `crates/rsetup-controller/src/lib.rs`（追加 `pub mod metrics;`，加在 `pub mod integrity;` 之后保持字母序）
- Modify: `Cargo.toml`（`[workspace.dependencies]` 追加 `toml = "0.8"`、`base64 = "0.22"`、`subtle = "2.6"`）
- Modify: `crates/rsetup-controller/Cargo.toml`（`[dependencies]` 追加 `toml.workspace = true`、`base64.workspace = true`、`subtle.workspace = true`）
- Test: 同文件 `#[cfg(test)]`（L1，规范 §7.2）

**Interfaces:**
- Consumes: `crate::ControllerError::Config(String)`（`error.rs:13`，唯一错误形态；`Display` 为 `"configuration: {0}"`）。
- Produces（后续 Task 依赖的精确签名，规范 §3.2/§3.3/§6.5/§7.1）：
  - `metrics::EDITION: &str = "2024"`
  - `metrics::config::DEFAULT_ENDPOINT: &str = "/metrics"`
  - `metrics::config::CONFIG_PATH_ENV: &str = "CONTROLLER_CONFIG"`
  - `metrics::config::DEFAULT_CONFIG_PATH: &str = "controller.toml"`
  - `metrics::config::AUTH_USER_ENV: &str = "PROMETHEUS_AUTH_USER"`
  - `metrics::config::AUTH_PASSWORD_ENV: &str = "PROMETHEUS_AUTH_PASSWORD"`
  - `metrics::config::REALM: &str = "rsetup-controller-metrics"`
  - `metrics::config::RESERVED_PATHS: &[&str]`
  - `metrics::config::is_reserved(path: &str) -> bool`
  - `metrics::config::MetricsFile { metrics: Option<MetricsSection> }`（`Deserialize` + `deny_unknown_fields`）
  - `metrics::config::MetricsSection { enabled: Option<bool>, endpoint: Option<String>, auth: Option<MetricsAuthRaw> }`（同上）
  - `metrics::config::MetricsAuthRaw { kind: Option<String>, username: Option<String>, password: Option<String> }`（`kind` 对应 TOML 键 `type`；同上）
  - `metrics::config::BasicCredentials { username: String, password: String }`（`Clone`，**不实现** `Debug`/`PartialEq`）
  - `metrics::config::MetricsConfig { enabled: bool, endpoint: String, credentials: Option<BasicCredentials> }`（`Clone` + **手写** `Debug`）
  - `MetricsConfig::parse_file(text: &str) -> Result<MetricsFile, ControllerError>`
  - `MetricsConfig::from_parts(file: Option<MetricsFile>, override_pair: Option<(&str, &str)>) -> Result<Self, ControllerError>`
  - `MetricsConfig::from_parts_with(env: &dyn Fn(&str) -> EnvRead, read_file: &dyn Fn(&Path) -> Result<String, std::io::Error>) -> Result<Self, ControllerError>`
  - `MetricsConfig::from_env() -> Result<Self, ControllerError>`
  - `metrics::config::EnvRead::{Unset, NotUnicode, Value(String)}`（`Debug + Clone + PartialEq + Eq`）+ `EnvRead::from_std(name: &str) -> Self`

- [ ] **Step 1: Write the failing test**

先建模块骨架与测试，让「测试先存在」。`crates/rsetup-controller/src/lib.rs` 追加一行（第 16 行 `pub mod integrity;` 之后）：

```rust
pub mod metrics;
```

`crates/rsetup-controller/src/metrics/mod.rs`（此 Step 刻意不含 `EDITION` 定义，实现属于 Step 3）：

```rust
//! Controller Prometheus 指标线（规范 `docs/superpowers/specs/2026-10-19-prometheus-metrics-endpoint-design.md`）。
//!
//! 依赖方向 MUST 单向（规范 §6.1）：`http → {auth, collect, render, config}`、
//! `collect → model`、`render → model`；`config`/`model` 是纯函数层，MUST NOT `use axum`
//! 或 `sqlx`。本模块整体是**独立认证边界**：不读会话 Cookie、不建会话、不占 CSRF/限速桶、
//! 不写审计（I3/I6），因此这里 MUST NOT 出现 `Arc<HttpAuthState>`、`Session` 或 `EndpointId`。

pub mod config;

#[cfg(test)]
mod tests {
    use super::EDITION;

    /// 从根清单 `Cargo.toml` 的 `[workspace.package]` 段里取 `edition = "…"` 的值。
    fn workspace_package_edition(manifest: &str) -> Option<&str> {
        let header = "[workspace.package]";
        let start = manifest.find(header)? + header.len();
        let body = &manifest[start..];
        let body = match body.find("\n[") {
            Some(offset) => &body[..offset],
            None => body,
        };
        body.lines().find_map(|line| quoted_value(line, "edition"))
    }

    /// 取 `key = "value"` 形态一行的引号内取值。
    fn quoted_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
        let rest = line
            .trim()
            .strip_prefix(key)?
            .trim_start()
            .strip_prefix('=')?
            .trim();
        rest.strip_prefix('"')?.strip_suffix('"')
    }

    /// 规范 §5.2.1：`EDITION` 必须与根清单 `[workspace.package] edition` 逐字节一致；
    /// 清单缺该行或值不同都必须失败（防标签值与真实构建定义漂移）。
    #[test]
    fn metrics_edition_matches_workspace_manifest() {
        let manifest = include_str!("../../../../Cargo.toml");
        let declared =
            workspace_package_edition(manifest).expect("根清单 [workspace.package] 必须有 edition 行");
        assert_eq!(declared, EDITION);
    }
}
```

`crates/rsetup-controller/src/metrics/config.rs`（此 Step 只有测试；实现属于 Step 3）：

```rust
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
            self.files(
                path,
                Ok(text.to_string()),
            )
        }
        fn files(
            mut self,
            path: &str,
            outcome: Result<String, std::io::ErrorKind>,
        ) -> Self {
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
                &|name| self
                    .env
                    .get(name)
                    .cloned()
                    .unwrap_or(EnvRead::Unset),
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
    fn message(error: ControllerError) -> String {
        match error {
            ControllerError::Config(text) => text,
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
            let config = outcome.unwrap_or_else(|error| panic!("{text:?} 必须合法：{}", message(error)));
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
        assert_eq!(message(error), "metrics.enabled is required");
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
            message(error)
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
        MetricsConfig::parse_file("[metrics]\nenabled = true\n\n[metrics.auth]\nusername = \"u\"\n")
            .expect("半截 auth 段在阶段一必须合法（Raw DTO 全字段 Option）");
    }

    /// TOML 语法错误：消息只含行列号，绝不含该行原文（I5 / §3.4 规则 6）。
    #[test]
    fn rejects_invalid_toml_without_leaking_values() {
        let error = Harness::new()
            .text(&format!("[metrics\nenabled = false\n[metrics.auth]\npassword = \"{SECRET_MARKER}\"\n"))
            .load()
            .expect_err("语法错误必须失败");
        let text = message(error);
        assert!(text.starts_with("metrics config: invalid TOML syntax at line "), "{text}");
        assert!(!text.contains(SECRET_MARKER), "错误消息回显了配置行原文：{text}");
        assert!(!text.contains("password"), "错误消息回显了键名原文：{text}");
    }

    /// 启用态 + 凭据齐备 + 非 `basic` 的 `type`（区分大小写、不 trim）。
    #[test]
    fn rejects_non_basic_auth_type() {
        for kind in ["bearer", "none", "token", "", "Basic", " basic"] {
            let text = format!(
                "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"{kind}\"\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
            );
            let error = Harness::new().text(&text).load().expect_err("非法 kind 必须失败");
            assert_eq!(message(error), "metrics.auth.type must be \"basic\"");
        }
    }

    /// 凭据齐备但缺 `type`：文件写了 auth 段却不声明 type 是配置缺陷。
    #[test]
    fn rejects_missing_auth_type_when_enabled_without_env() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
        );
        let error = Harness::new().text(&text).load().expect_err("缺 type 必须失败");
        assert_eq!(message(error), "metrics.auth.type must be \"basic\"");
    }

    /// 存在性检查先于 kind（§3.6 分支 2 优先于分支 3）。
    #[test]
    fn missing_credentials_precede_type_error() {
        let error = Harness::new()
            .text("[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"bearer\"\n")
            .load()
            .expect_err("缺凭据必须失败");
        assert_eq!(
            message(error),
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
            message(error),
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
        assert_eq!(message(error), "metrics.endpoint is invalid");
    }

    /// 尾斜杠、查询串、空白、空段、大写都属于字符形状非法。
    #[test]
    fn rejects_endpoint_with_trailing_slash_query_or_space() {
        for endpoint in ["/metrics/", "/metrics?x=1", "/met rics", "/a//b", "/METRICS"] {
            let text = format!("[metrics]\nenabled = false\nendpoint = \"{endpoint}\"\n");
            let error = Harness::new()
                .text(&text)
                .load()
                .expect_err("非法端点必须失败：{endpoint}");
            assert_eq!(message(error), "metrics.endpoint is invalid", "{endpoint}");
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
                message(error),
                "metrics.endpoint collides with an existing route",
                "{endpoint}"
            );
        }
    }

    /// `/` 只按完全相等判定；前缀匹配必须有 `/` 边界（§6.5）。
    #[test]
    fn reserved_root_path_is_exact_match_only() {
        assert!(is_reserved("/"));
        for path in ["/metrics", "/metrics/deep", "/apiv1", "/assetsx", "/healthz2"] {
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
            message(error),
            "metrics.endpoint collides with an existing route"
        );
        let error = Harness::new()
            .text("[metrics]\nenabled = false\nendpoint = \"/METRICS\"\n")
            .load()
            .expect_err("禁用态仍要校验字符形状");
        assert_eq!(message(error), "metrics.endpoint is invalid");
    }

    /// `len ≤ 128`（§3.4）。
    #[test]
    fn rejects_overlong_endpoint() {
        let long = format!("/{}", "a".repeat(128));
        assert_eq!(long.len(), 129);
        let error = Harness::new()
            .text(&format!("[metrics]\nenabled = false\nendpoint = \"{long}\"\n"))
            .load()
            .expect_err("超长端点必须失败");
        assert_eq!(message(error), "metrics.endpoint is invalid");
        let edge = format!("/{}", "a".repeat(127));
        assert_eq!(edge.len(), 128);
        let text = format!("[metrics]\nenabled = false\nendpoint = \"{edge}\"\n");
        assert!(!Harness::new().text(&text).load().unwrap().enabled);
    }

    /// username 禁冒号（RFC 7617 §2）与控制字符；password 只禁控制字符。
    #[test]
    fn rejects_username_with_colon_or_control_char() {
        for username in ["a:b", "a\u{7}"] {
            let text = format!(
                "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{username}\"\npassword = \"{PASSWORD}\"\n"
            );
            let error = Harness::new().text(&text).load().expect_err("非法 username 必须失败");
            assert_eq!(message(error), "metrics.auth.username is invalid");
        }
    }

    #[test]
    fn rejects_password_with_control_char() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"a\u{7}b\"\n"
        );
        let error = Harness::new().text(&text).load().expect_err("非法 password 必须失败");
        assert_eq!(message(error), "metrics.auth.password is invalid");
    }

    /// 启用但完全没有 `[metrics.auth]`，也没有环境变量 → 拒绝启动。
    #[test]
    fn fail_closed_when_enabled_without_auth() {
        let error = Harness::new()
            .text("[metrics]\nenabled = true\n")
            .load()
            .expect_err("启用无凭据必须失败");
        assert_eq!(
            message(error),
            "metrics.auth is required when metrics.enabled is true"
        );
    }

    /// 空串是"已提供但非法"（`Some("")`）→ 命中分支 4，不命中分支 2。
    #[test]
    fn rejects_blank_username_when_enabled() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"\"\npassword = \"{PASSWORD}\"\n"
        );
        let error = Harness::new().text(&text).load().expect_err("空 username 必须失败");
        assert_eq!(message(error), "metrics.auth.username is invalid");
    }

    #[test]
    fn rejects_blank_password_when_enabled() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"\"\n"
        );
        let error = Harness::new().text(&text).load().expect_err("空 password 必须失败");
        assert_eq!(message(error), "metrics.auth.password is invalid");
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
                .unwrap_or_else(|error| panic!("禁用态必须放行 {tail:?}：{}", message(error)));
            assert!(!config.enabled);
            assert!(config.credentials.is_none());
        }
    }

    /// 环境变量成对覆盖文件凭据。
    #[test]
    fn env_pair_overrides_file_credentials() {
        let config = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .pair(EnvRead::Value("env_user".into()), EnvRead::Value("env_password".into()))
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
            .pair(EnvRead::Value("env_user".into()), EnvRead::Value("env_password".into()))
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
            .pair(EnvRead::Value("env_user".into()), EnvRead::Value("env_password".into()))
            .load()
            .unwrap();
        assert_eq!(config.credentials.expect("必须有凭据").password, "env_password");
    }

    /// 覆盖不猜意图：文件里显式写了非法 `kind` 时保留原值交给阶段三拒绝。
    #[test]
    fn env_does_not_override_illegal_type() {
        let text = format!(
            "[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"bearer\"\nusername = \"{USER}\"\npassword = \"{PASSWORD}\"\n"
        );
        let error = Harness::new()
            .text(&text)
            .pair(EnvRead::Value("env_user".into()), EnvRead::Value("env_password".into()))
            .load()
            .expect_err("非法 kind 不得被覆盖救回");
        assert_eq!(message(error), "metrics.auth.type must be \"basic\"");
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
            message(error),
            "metrics auth override must set both user and password"
        );
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .env(AUTH_PASSWORD_ENV, EnvRead::Value("env_password".into()))
            .load()
            .expect_err("partial 覆盖必须失败");
        assert_eq!(
            message(error),
            "metrics auth override must set both user and password"
        );
    }

    /// 空串按未设置处理，因此与「只设另一个」等价。
    #[test]
    fn env_empty_string_counts_as_unset() {
        let error = Harness::new()
            .text(ALIGNED_EXAMPLE)
            .pair(EnvRead::Value(String::new()), EnvRead::Value("env_password".into()))
            .load()
            .expect_err("空串 user + 非空 password 属 partial");
        assert_eq!(
            message(error),
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
            message(error),
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
            message(error),
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
        assert_eq!(message(error), "metrics config path is not valid unicode");
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
                Harness::new().text(&format!("[metrics]\nenabled = true\n\n[metrics.auth]\ntype = \"basic\"\nusername = \"{USER}\"\npassword = \"a\u{7}b\"\n")),
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
            let text = message(error);
            for marker in [USER, PASSWORD, SECRET_MARKER, "controller.toml", "/etc/rsetup/absent.toml"] {
                assert!(!text.contains(marker), "{label} 的错误消息回显了 {marker:?}：{text}");
            }
        }
    }

    /// 手写 `Debug`：凭据只以"有无"出现（§3.2）。
    #[test]
    fn metrics_config_debug_is_redacted() {
        let config = Harness::new().text(ALIGNED_EXAMPLE).load().unwrap();
        let text = format!("{config:?}");
        assert!(text.contains("[redacted]"), "Debug 必须含固定脱敏文案：{text}");
        assert!(!text.contains(USER), "Debug 回显了用户名：{text}");
        assert!(!text.contains(PASSWORD), "Debug 回显了密码：{text}");
        assert!(text.contains("enabled: true"), "Debug 必须保留 enabled/endpoint：{text}");
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
            .env(CONFIG_PATH_ENV, EnvRead::Value("/etc/rsetup/absent.toml".into()))
            .load()
            .expect_err("显式指定的路径不存在必须失败");
        assert!(
            !message(error).contains("absent.toml"),
            "错误消息不得回显路径"
        );
    }

    #[test]
    fn explicit_path_present_is_used() {
        let config = Harness::new()
            .env(CONFIG_PATH_ENV, EnvRead::Value("/etc/rsetup/custom.toml".into()))
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
        assert!(!message(error).contains("controller.toml"), "不得回显路径");
    }

    #[test]
    fn non_utf8_file_is_error() {
        let error = Harness::new()
            .non_utf8_default()
            .load()
            .expect_err("非 UTF-8 文件必须失败");
        assert!(!message(error).is_empty());
    }

    /// 三阶段责任不重叠：阶段二只写数据、阶段三只读数据（用纯函数钉住）。
    #[test]
    fn override_happens_before_gate() {
        // 文件里禁用 + 环境变量给凭据：合法，凭据在阶段三被丢弃。
        let config = Harness::new()
            .text("[metrics]\nenabled = false\n")
            .pair(EnvRead::Value("env_user".into()), EnvRead::Value("env_password".into()))
            .load()
            .unwrap();
        assert!(!config.enabled);
        assert!(config.credentials.is_none());
        // 文件里启用 + 环境变量补凭据：合法。
        let config = parts("[metrics]\nenabled = true\n", Some(("env_user", "env_password"))).unwrap();
        assert_eq!(config.credentials.expect("必须有凭据").username, "env_user");
        // 文件里启用 + 无凭据：非法。
        assert!(parts("[metrics]\nenabled = true\n", None).is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p rsetup-controller --lib metrics
```

预期：**编译失败（RED）**。判据（错误码与符号名必须出现，措辞可变）：

```text
error[E0432]: unresolved import `super::EDITION`
error[E0432]: unresolved imports `super::DEFAULT_CONFIG_PATH`, `super::EnvRead`, `super::MetricsConfig`, ...
error[E0433]: failed to resolve: use of undeclared type `MetricsConfig`
error: could not compile `rsetup-controller` (lib test) due to N previous errors
```

同时确认 `cargo test -p rsetup-controller --lib metrics` **没有**任何 `test result: ok` 行——测试从未跑起来才是本 Step 的通过判据。若本机 `Cargo.lock` 还没有 `toml`，此时仍不应报依赖错误（本 Step 的测试代码不 `use toml`）；出现 `no matching package named 'toml'` 说明 Step 3 的清单改动被提前做了，回退到本 Step 的骨架。

- [ ] **Step 3: Write minimal implementation**

3.1 工作区根 `Cargo.toml` 的 `[workspace.dependencies]` 内、`thiserror = "2.0"` 之前追加三行（键名字母序无强制要求，保持与相邻块风格一致）：

```toml
toml = "0.8"
base64 = "0.22"
subtle = "2.6"
```

3.2 `crates/rsetup-controller/Cargo.toml` 的 `[dependencies]` 追加（与既有 `*.workspace = true` 风格一致，按字母序插在 `serde` 之前/之后均可，但必须紧邻同类）：

```toml
base64.workspace = true
subtle.workspace = true
toml.workspace = true
```

> 用途锁定（规范 §3.1 约束 1/3）：`toml` 只做 `from_str` 反序列化（不启用 `preserve_order` 等 feature）；`base64` 只用于 Task 2 的 `general_purpose::STANDARD`；`subtle` 只用于 Task 2 的 `ConstantTimeEq`。三者 MUST NOT 用于其他用途。`toml 0.8` 首次进入工作区，本 Step 起首次构建需网络以更新 `Cargo.lock`，并在交付说明中登记新增 crate（§8 R10）。

3.3 `crates/rsetup-controller/src/metrics/config.rs` 的**实现部分**（写在 Step 1 那个 `#[cfg(test)] mod tests` 之前，测试原样保留）：

```rust
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
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-')
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
                return Err(ControllerError::Config(MSG_AUTH_ENV_NOT_UNICODE.to_string()));
            }
            EnvRead::Unset => None,
            EnvRead::Value(value) if value.is_empty() => None,
            EnvRead::Value(value) => Some(value),
        };
        let password = match env(AUTH_PASSWORD_ENV) {
            EnvRead::NotUnicode => {
                return Err(ControllerError::Config(MSG_AUTH_ENV_NOT_UNICODE.to_string()));
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
        toml::from_str::<MetricsFile>(text).map_err(|error| ControllerError::Config(format!(
            "metrics config: invalid TOML syntax at line {}",
            error
                .span()
                .map(|span| line_of(text, span.start))
                .unwrap_or(0)
        )))
    }
}

/// 字节偏移 → 1 起始行号；非字符边界或越界一律 0（表示"行号未知"）。
fn line_of(text: &str, offset: usize) -> usize {
    text.get(..offset)
        .map(|prefix| prefix.matches('\n').count() + 1)
        .unwrap_or(0)
}
```

3.4 `crates/rsetup-controller/src/metrics/mod.rs` 追加跨文件常量与再导出（放在 `pub mod config;` 之后、`#[cfg(test)]` 之前）：

```rust
/// Rust edition 标签值。Cargo 不暴露 `CARGO_PKG_EDITION`，故显式声明，
/// 由 `metrics_edition_matches_workspace_manifest` 与根清单 `[workspace.package]` 对齐。
pub const EDITION: &str = "2024";

pub use config::{
    AUTH_PASSWORD_ENV, AUTH_USER_ENV, BasicCredentials, CONFIG_PATH_ENV, DEFAULT_CONFIG_PATH,
    DEFAULT_ENDPOINT, EnvRead, MetricsAuthRaw, MetricsConfig, MetricsFile, MetricsSection, REALM,
    RESERVED_PATHS, is_reserved,
};
```

3.5 更新 `Cargo.lock`（首次引入 `toml`）并确认只新增了预期 crate：

```bash
cargo update -p rsetup-controller --workspace
grep -n -A1 '^name = "toml"' Cargo.lock
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo fmt --package rsetup-controller
cargo test -p rsetup-controller --lib metrics::config
cargo test -p rsetup-controller --lib metrics::tests
```

预期（GREEN）：

```text
test result: ok. 38 passed; 0 failed; 0 ignored; 0 measured; N filtered out
...
test metrics::tests::metrics_edition_matches_workspace_manifest ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

再跑一次全量 lib 测试确认没有打破既有用例：

```bash
cargo test -p rsetup-controller --lib
cargo clippy --package rsetup-controller --all-targets -- -D warnings
```

预期：`test result: ok.`（0 failed），clippy 无 warning（特别是 `derivable_impls`、`manual_let_else`、`redundant_closure_for_method_calls`）。

> 计数容差说明：`38 passed` 是本 Task Step 1 写下的测试函数条数（执行时以实际数量为准）；**判据是 `0 failed` 且 `metrics::config::tests` 下每个测试都出现在输出里**，不得用 `--skip` 掩盖任何一条。

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock crates/rsetup-controller/Cargo.toml \
  crates/rsetup-controller/src/lib.rs \
  crates/rsetup-controller/src/metrics/mod.rs \
  crates/rsetup-controller/src/metrics/config.rs
git diff --cached --stat
git commit -m "feat(controller): 增加指标配置三阶段解析与启动安全门禁"
```

> 提交前检查：`git diff --cached` 不含真实凭据与 `controller.toml` 实体（§3.7/§7.8）；`git status --porcelain` 中 `secret/` 不出现（已被 `.gitignore` 覆盖，且 MUST NOT 用 `git add -f` 绕过）。
---

## Task 2: Basic Auth 提取器与常量时间认证（`metrics/auth.rs`）

**Files:**
- Create: `crates/rsetup-controller/src/metrics/auth.rs`
- Modify: `crates/rsetup-controller/src/metrics/mod.rs`（追加 `pub mod auth;` 与再导出）
- Test: 同文件 `#[cfg(test)]`（L1，规范 §7.3）

**Interfaces:**
- Consumes：
  - `crate::http_gate::single_header(headers: &HeaderMap, key: HeaderName) -> Option<&str>`（既有 `src/http_gate.rs:30`，`pub(crate)`；语义：**任意多于一个值一律 `None`**、非 ASCII 值 `None`。规范 §4.2 第 1 步要求复用该 helper，MUST NOT 复制一份实现）
  - `metrics::config::BasicCredentials`（Task 1，字段 `username`/`password`，无 `Debug`/`PartialEq`）、`metrics::config::REALM`
  - 直接依赖 `base64 0.22`（`general_purpose::STANDARD`）、`subtle 2.6`（`ConstantTimeEq`）、`sha2`（既有）
- Produces（`metrics/http.rs` 与 L2 测试依赖的精确签名，规范 §4.2/§4.3/§4.4）：
  - `metrics::auth::ParsedBasic { pub username: String, pub password: String }`（**不派生** `Debug`/`PartialEq`）
  - `metrics::auth::parse_authorization(&HeaderMap) -> Option<ParsedBasic>`
  - `metrics::auth::verify(user: &str, password: &str, expected: &BasicCredentials) -> bool`
  - `metrics::auth::www_authenticate() -> String`（恒 `Basic realm="rsetup-controller-metrics"`）
  - 私有 `fn sha256(bytes: &[u8]) -> [u8; 32]`、`fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool`

- [ ] **Step 1: Write the failing test**

`crates/rsetup-controller/src/metrics/auth.rs` 先只写测试模块（实现属 Step 3）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::config::BasicCredentials;
    use axum::http::{HeaderMap, HeaderValue, header};
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    const USER: &str = "prom_admin";
    const PASSWORD: &str = "secure_password";

    fn credentials() -> BasicCredentials {
        BasicCredentials {
            username: USER.to_string(),
            password: PASSWORD.to_string(),
        }
    }

    /// 以**原始字节**构造 Authorization 头（非 ASCII 场景必须绕开 `&str` 检查）。
    fn headers_with(raw: &[u8]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_bytes(raw).expect("合法 header 字节"),
        );
        headers
    }

    fn header_for(value: &str) -> HeaderMap {
        headers_with(value.as_bytes())
    }

    /// 按 `user:password` 组装标准 base64（带 padding）。
    fn basic(user: &[u8], password: &[u8]) -> String {
        let mut bytes = Vec::with_capacity(user.len() + password.len() + 1);
        bytes.extend_from_slice(user);
        bytes.push(b':');
        bytes.extend_from_slice(password);
        STANDARD.encode(bytes)
    }

    // ---------------------------------------------------------------------------
    // §4.2 解析
    // ---------------------------------------------------------------------------

    /// owned 结果：`ParsedBasic` 必须能脱离 `HeaderMap` 存活（先 drop 再断言）。
    #[test]
    fn parses_valid_basic_header() {
        let headers = header_for(&format!("Basic {}", basic(USER.as_bytes(), PASSWORD.as_bytes())));
        let parsed = parse_authorization(&headers).expect("合法 Basic 必须解析成功");
        drop(headers);
        assert_eq!(parsed.username, USER);
        assert_eq!(parsed.password, PASSWORD);
    }

    /// 首个冒号之后的全部字符归密码（§4.2 第 6 步 `split_once`、§3.4 规则 2）。
    #[test]
    fn parses_password_with_colons() {
        let headers = header_for(&format!("Basic {}", basic(b"prom_admin", b"a:b:c")));
        let parsed = parse_authorization(&headers).expect("密码含冒号必须解析成功");
        assert_eq!(parsed.username, "prom_admin");
        assert_eq!(parsed.password, "a:b:c");
    }

    /// 冒号串、UTF-8、`=` 全部归属密码；`"u:"` 的密码是空串。
    #[test]
    fn parses_password_with_many_and_edge_colons() {
        let cases: &[(&[u8], &[u8], &str)] = &[
            (b"u", b":::", "u::::"),
            (b"u", "日本語:パ:Sword=".as_bytes(), "u:日本語:パ:Sword="),
            (b"u", b"", "u:"),
        ];
        for (user, password, raw) in cases {
            let headers = header_for(&format!("Basic {}", basic(user, password)));
            let parsed = parse_authorization(&headers)
                .unwrap_or_else(|| panic!("{raw:?} 必须解析成功"));
            assert_eq!(parsed.username.as_bytes(), *user);
            assert_eq!(parsed.password.as_bytes(), *password);
        }
    }

    /// scheme 不区分大小写；`Bas ic`（内部空格）失败。
    #[test]
    fn scheme_is_case_insensitive() {
        let encoded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        for scheme in ["Basic", "basic", "BASIC", "bAsIc"] {
            let headers = header_for(&format!("{scheme} {encoded}"));
            assert!(parse_authorization(&headers).is_some(), "{scheme} 必须可用");
        }
        assert!(parse_authorization(&header_for(&format!("Bas ic {encoded}"))).is_none());
    }

    /// 其他 scheme、无 scheme、多空格、前导空白一律 None。
    #[test]
    fn rejects_other_schemes() {
        let encoded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        for value in [
            format!("Bearer {encoded}"),
            format!("Digest username=\"{USER}\""),
            encoded.clone(),
            format!("Basic  {encoded}"),
            format!(" {encoded}"),
            format!(" {USER}:{PASSWORD}"),
        ] {
            assert!(
                parse_authorization(&header_for(&value)).is_none(),
                "{value:?} 必须解析失败"
            );
        }
    }

    /// 多个 Authorization 头（相同或不同值）绝不简化为取第一个。
    #[test]
    fn rejects_multiple_authorization_headers() {
        let encoded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        for second in [
            format!("Basic {encoded}"),
            format!("Basic {}", basic(b"other", b"secret")),
        ] {
            let mut headers = header_for(&format!("Basic {encoded}"));
            headers.append(header::AUTHORIZATION, HeaderValue::from_str(&second).unwrap());
            assert!(parse_authorization(&headers).is_none(), "多值头必须 None");
        }
    }

    /// 缺失头、空 rest、非 ASCII 头值。
    #[test]
    fn rejects_missing_header_empty_rest_and_non_ascii() {
        assert!(parse_authorization(&HeaderMap::new()).is_none());
        assert!(parse_authorization(&header_for("Basic ")).is_none());
        assert!(parse_authorization(&header_for("Basic")).is_none());
        // 非 ASCII（UTF-8 的 é）：to_str() 失败 → None（§4.2 第 1 步）。
        assert!(parse_authorization(&headers_with("Basic é".as_bytes())).is_none());
    }

    /// 非法 base64：含 `!`、长度非 4 倍数、去掉 padding、前导 `=`、内部空白。
    #[test]
    fn rejects_invalid_base64_and_unpadded() {
        let padded = basic(USER.as_bytes(), PASSWORD.as_bytes());
        assert!(padded.ends_with('='));
        for value in [
            "Basic nothex!!".to_string(),
            format!("Basic {}", padded.trim_end_matches('=')),
            format!("Basic ={padded}"),
            format!("Basic {} {}", padded, padded),
            "Basic YQ".to_string(),
        ] {
            assert!(
                parse_authorization(&header_for(&value)).is_none(),
                "{value:?} 必须解析失败"
            );
        }
    }

    /// 解码后非法 UTF-8 字节序列 → None（§4.2 第 5 步）。
    #[test]
    fn rejects_non_utf8() {
        let headers = header_for(&format!("Basic {}", STANDARD.encode([0xFF_u8, 0xFE, 0x00, 0x41])));
        assert!(parse_authorization(&headers).is_none());
    }

    /// 无冒号、空用户名（含只有冒号）一律 None。
    #[test]
    fn rejects_missing_colon_and_empty_user() {
        for raw in ["prom_admin", ":pw", ":"] {
            let headers = header_for(&format!(
                "Basic {}",
                STANDARD.encode(raw.as_bytes().to_vec())
            ));
            assert!(parse_authorization(&headers).is_none(), "{raw:?} 必须 None");
        }
    }

    // ---------------------------------------------------------------------------
    // §4.3 常量时间比较
    // ---------------------------------------------------------------------------

    #[test]
    fn verify_accepts_exact_match() {
        assert!(verify(USER, PASSWORD, &credentials()));
    }

    /// 端到端凭据形状：配置里的冒号密码不被任何一层拒绝（与 §7.5 L2 用例配对）。
    #[test]
    fn verify_accepts_password_with_colons() {
        let expected = BasicCredentials {
            username: USER.to_string(),
            password: "a:b:c".to_string(),
        };
        assert!(verify(USER, "a:b:c", &expected));
        assert!(!verify(USER, "a:b", &expected));
    }

    /// 用户名错 / 密码错 / 都错 / 都空：一律 false，响应不可区分。
    #[test]
    fn verify_rejects_each_field_independently() {
        let expected = credentials();
        assert!(!verify("wrong_user", PASSWORD, &expected));
        assert!(!verify(USER, "wrong_password", &expected));
        assert!(!verify("wrong_user", "wrong_password", &expected));
        assert!(!verify("", "", &expected));
    }

    /// 长度差 1 与差 64 的错误密码都 false：长度不进比较。
    #[test]
    fn verify_is_length_independent() {
        let expected = credentials();
        assert!(!verify(USER, &format!("{PASSWORD}x"), &expected));
        assert!(!verify(USER, &format!("{PASSWORD}{}", "x".repeat(64)), &expected));
        assert!(!verify(&format!("{USER}x"), PASSWORD, &expected));
    }

    /// 逐位差异（首字节、末字节、单比特）都必须被检出。
    #[test]
    fn ct_eq_32_is_content_sensitive() {
        let base = sha256(b"baseline");
        assert!(ct_eq_32(&base, &base));
        let mut first = base;
        first[0] ^= 1;
        assert!(!ct_eq_32(&base, &first));
        let mut last = base;
        last[31] ^= 1;
        assert!(!ct_eq_32(&base, &last));
        assert!(!ct_eq_32(&base, &sha256(b"baselinF")));
        // 输入长度不进入比较：哈希把长度归一为 32 字节。
        assert_eq!(sha256(b"a").len(), sha256(b"aaaaaaaaaaa").len());
    }

    /// 去掉行注释后的纯代码文本（本文件的注释会解释"不许写什么"，直接扫原文会误判）。
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 源码级门禁：常量时间比较必须落在 `subtle` 上，且 `subtle` 是直接依赖。
    /// 模式串取 `"diff |= a"`（比 §7.8 第 7 条的 `diff |= a\[i\]` 更短），这样本测试
    /// 自身不会成为该 grep 的命中项。
    #[test]
    fn ct_eq_32_uses_subtle_constant_time_eq() {
        let source = include_str!("auth.rs");
        let code = code_only(&source);
        assert!(
            source.contains("use subtle::ConstantTimeEq"),
            "ct_eq_32 必须由 subtle::ConstantTimeEq 实现（§7.8 第 8 条 grep 必须命中）"
        );
        assert!(
            !code.contains("diff |= a"),
            "MUST NOT 在指标线代码里出现手写逐字节折叠（§4.3 第 2 条）"
        );
        assert!(
            !code.contains("starts_with(expected") && !code.contains("== expected"),
            "MUST NOT 用可短路的比较凭据（§4.3 第 2 条）"
        );
        let manifest = include_str!("../../Cargo.toml");
        assert!(
            manifest.contains("subtle.workspace = true"),
            "subtle 必须是提升而来的直接依赖（§3.1 约束 2）"
        );
    }

    /// `verify` 无条件执行两次比较：短路写法会在用户名上建时序判定器（§4.3 第 1 条）。
    #[test]
    fn verify_runs_both_comparisons() {
        reset_ct_eq_calls();
        assert!(!verify("wrong_user", PASSWORD, &credentials()));
        assert_eq!(
            ct_eq_calls(),
            2,
            "用户名不匹配时密码比较仍必须执行（计数应为 2）"
        );
        reset_ct_eq_calls();
        assert!(verify(USER, PASSWORD, &credentials()));
        assert_eq!(ct_eq_calls(), 2);
    }

    // ---------------------------------------------------------------------------
    // §4.4 401 挑战值
    // ---------------------------------------------------------------------------

    #[test]
    fn www_authenticate_value_is_byte_fixed() {
        assert_eq!(
            www_authenticate(),
            "Basic realm=\"rsetup-controller-metrics\""
        );
        assert!(www_authenticate().contains(REALM));
        assert!(!www_authenticate().contains("charset"));
    }
}
```

在 `crates/rsetup-controller/src/metrics/mod.rs` 追加（实现部分属 Step 3）：

```rust
pub mod auth;
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p rsetup-controller --lib metrics::auth
```

预期：**编译失败（RED）**，判据是解析/校验函数与 `ParsedBasic` 未定义：

```text
error[E0432]: unresolved imports `super::parse_authorization`, `super::verify`, `super::www_authenticate`
error: cannot find function `sha256` in this scope
error: cannot find function `ct_eq_32` in this scope
error: could not compile `rsetup-controller` (lib test) due to N previous errors
```

不得出现 `test result: ok`。若出现 `metrics::auth` 相关以外的编译错误（例如 `single_header` 不可见），说明越过了 §4.2 的复用约束，先修依赖面再继续。

- [ ] **Step 3: Write minimal implementation**

`crates/rsetup-controller/src/metrics/auth.rs` 的实现部分（写在 Step 1 的测试模块之前）：

```rust
//! Basic Auth 解析与常量时间凭据比较（规范 §4.2/§4.3/§4.4）。
//!
//! 本模块只做两件事：把 `Authorization: Basic` 解析成**拥有所有权**的凭据对，以及
//! 用 `subtle::ConstantTimeEq` 做**不短路**的比较。它不构造 HTTP 响应、不读配置、
//! 不碰会话边界（I3）：`use crate::http_gate::single_header` 是本模块对 `http_gate`
//! 的唯一依赖，且该 helper 只做"单值 + ASCII"提取。
//!
//! 比较前先做 SHA-256：把输入长度归一为固定 32 字节，消除"长度不同提前返回"与
//! 逐字节短路两类侧信道（§4.3 第 3 条）。诚实登记的残余面（§4.3 第 4 条、§8 R3）：
//! 哈希本身耗时随输入长度线性增长；解析失败会在比较之前返回 401（提前量只与
//! "提交的是否是合法 base64 形状"有关，与凭据内容无关）。

use axum::http::{HeaderMap, header};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use subtle::ConstantTimeEq;

use crate::http_gate::single_header;
use crate::metrics::config::{BasicCredentials, REALM};

/// `Authorization: Basic` 的解析结果。**拥有所有权**：base64 解码与 UTF-8 校验
/// 产生的字符串只在解析函数内部存在，任何 `Option<(&str, &str)>` 形态都会借用
/// 一个随函数返回而销毁的临时缓冲，要么编译不过，要么靠把整张 `HeaderMap` 一路
/// 带调用链来续命。切分点又落在**解码后**的字节上，因此终态只能是 owned。
///
/// 不派生 `Debug`（凭据不外泄，§3.7）也不派生 `PartialEq`（防用短路 `==` 比较凭据）。
pub struct ParsedBasic {
    pub username: String,
    pub password: String,
}

/// §4.4 的固定挑战值；`realm` 取 §3.2 的 `REALM` 常量，二者 MUST 逐字节一致
/// （`www_authenticate_value_is_byte_fixed` 钉住）。不携带 `charset` 参数。
pub fn www_authenticate() -> String {
    format!("Basic realm=\"{REALM}\"")
}

/// §4.2 的固定七步解析。任何一步失败都返回 `None`，**不区分失败原因**（§4.2 第 7 步）。
pub fn parse_authorization(headers: &HeaderMap) -> Option<ParsedBasic> {
    // 1) 单值 + ASCII：缺失、出现多次（任意多于一个值）、非 ASCII 值一律 None。
    //    复用 http_gate 的同名 helper，绝不复制实现（"重复头绝不简化为取第一个"）。
    let value = single_header(headers, header::AUTHORIZATION)?;
    // 2) 按**第一个**空格切分 scheme 与 rest；两段都存在且 rest 非空。
    //    前导空白的行首段是空串 → 不是 basic → None；多空格使 rest 以空格开头 → 第 3 步拒绝。
    let (scheme, rest) = value.split_once(' ')?;
    // 3) scheme ASCII 大小写不敏感等于 basic（RFC 7617 §2.2）。
    if !scheme.eq_ignore_ascii_case("basic")
        || rest.is_empty()
        || rest.starts_with('=')
        || rest.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    // 4) 唯一合法编码：general_purpose::STANDARD（要求 padding、拒绝尾部比特残留、
    //    不容忍空白）；STANDARD_NO_PAD / URL_SAFE / base32 / hex 一律不接受。
    let decoded = STANDARD.decode(rest).ok()?;
    // 5) 解码结果按 UTF-8 严格解码（Prometheus 官方 basic_auth 按 UTF-8 发凭据）。
    let decoded = String::from_utf8(decoded).ok()?;
    // 6) 仅**第一个**冒号是分隔符，其右侧**全部**字符（可再含冒号）都是密码。
    //    MUST NOT 用 split(':')/rsplit_once/splitn 后再校验"剩余不含冒号"——那会拒绝
    //    RFC 7617 明确允许的密码，把合法抓取方永久锁在 401 之外。
    let (username, password) = decoded.split_once(':')?;
    if username.is_empty() {
        return None;
    }
    // 7) 交 §4.3 校验。
    Some(ParsedBasic {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

/// 凭据校验：先用户名、后密码，**两次比较都必须执行**，结果按位与合并（§4.3 第 1 条）。
///
/// MUST NOT 写成 `if !user_ok { return false; } …`：短路会在用户名上建立"用户名是否
/// 正确"的时序判定器。返回值也不区分"用户名错"与"密码错"（§4.3 第 5 条），不记日志。
pub fn verify(user: &str, password: &str, expected: &BasicCredentials) -> bool {
    let ok_user = ct_eq_32(&sha256(user.as_bytes()), &sha256(expected.username.as_bytes()));
    let ok_password = ct_eq_32(&sha256(password.as_bytes()), &sha256(expected.password.as_bytes()));
    // `&`（非 `&&`）：两侧都已求值，此处再显式保证不因折叠而短路。
    ok_user & ok_password
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes).into()
}

/// `subtle::ConstantTimeEq` 的薄封装（§4.3 第 2 条）。MUST NOT 改写为手写的逐字节
/// XOR/OR 折叠循环：这类写法在 `opt-level` 提升或 LLVM 把循环识别为 `memcmp` 后不保证
/// 仍是常量时间；`subtle` 的 `Choice` 与 `black_box` 是对**编译器优化后机器码**的保证
/// （§7.8 第 7 条 grep 对 `src/metrics` 下该模式串的要求是"零命中"，故此处不复述拼法）。
fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    #[cfg(test)]
    COUNT_CT_EQ.with(|calls| calls.set(calls.get() + 1));
    let subtle::Choice(result) = a.ct_eq(b);
    // `Choice -> bool` 的 Into 转换；上面解构只为让"结果来自 subtle"在源码上可指认
    // （§7.8 第 8 条 grep 命中 `subtle::ConstantTimeEq`）。
    result == 1
}

// ---------------------------------------------------------------------------
// 仅测试构建存在的调用计数（§7.3 `verify_runs_both_comparisons` 的注入点）
// ---------------------------------------------------------------------------

#[cfg(test)]
thread_local! {
    /// 线程本地计数而非全局原子量：libtest 为每个测试新建线程，因此该计数不受
    /// 同批并行测试干扰，断言 `== 2` 是确定的。计数代码只在 `cfg(test)` 下存在。
    static COUNT_CT_EQ: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn ct_eq_calls() -> u8 {
    COUNT_CT_EQ.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_ct_eq_calls() {
    COUNT_CT_EQ.with(|calls| calls.set(0));
}
```

`crates/rsetup-controller/src/metrics/mod.rs` 的再导出追加 `auth` 公开项（与 Task 1 的 `pub use config::{…}` 并列）：

```rust
pub use auth::{ParsedBasic, parse_authorization, verify, www_authenticate};
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo fmt --package rsetup-controller
cargo test -p rsetup-controller --lib metrics::auth
```

预期（GREEN）：

```text
test metrics::auth::tests::parses_valid_basic_header ... ok
test metrics::auth::tests::verify_runs_both_comparisons ... ok
...
test result: ok. 18 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

判据：`0 failed`，且 `verify_runs_both_comparisons` 与 `ct_eq_32_uses_subtle_constant_time_eq` 都出现在输出中（前者是 §4.3 短路写法的回归网，后者是 §7.8 第 8 条 grep 的冗余网）。随后：

```bash
cargo clippy --package rsetup-controller --all-targets -- -D warnings
grep -rn 'subtle::ConstantTimeEq' crates/rsetup-controller/src/metrics/auth.rs
grep -rn 'diff |= a\[i\]' crates/rsetup-controller/src/metrics
```

预期：clippy 无 warning；第二条 grep **有**输出（§7.8 第 8 条）；第三条 grep **无**输出（§7.8 第 7 条）。

- [ ] **Step 5: Commit**

```bash
git add crates/rsetup-controller/src/metrics/mod.rs crates/rsetup-controller/src/metrics/auth.rs
git commit -m "feat(controller): 增加 Basic 凭据解析与常量时间认证"
```

---

## Task 3: 数据模型与 Prometheus 0.0.4 文本渲染器（`metrics/model.rs`、`metrics/render.rs`）

**Files:**
- Create: `crates/rsetup-controller/src/metrics/model.rs`
- Create: `crates/rsetup-controller/src/metrics/render.rs`
- Modify: `crates/rsetup-controller/src/metrics/mod.rs`（`pub mod model; pub mod render;` + 再导出）
- Test: 两个新文件的 `#[cfg(test)]`（L1，规范 §7.4）

**Interfaces:**
- Consumes：`metrics::EDITION`（Task 1）；`crate::ControllerError::Config`。`model.rs` 与 `render.rs` 是纯函数层：MUST NOT `use axum`/`sqlx`、无 I/O、无 async（§6.1）。
- Produces（Task 4/5 与 §7.4 依赖的精确签名）：
  - `metrics::model::AdmissionBucket::{Approved, Pending, Rejected}` + `as_str() -> &'static str`（`"APPROVED"`/`"PENDING"`/`"REJECTED"`）
  - `metrics::model::TaskBucket::{Pending, Running, Succeeded, Failed}` + `as_str() -> &'static str`（`"pending"`/`"running"`/`"succeeded"`/`"failed"`）
  - `metrics::model::admission_label(raw: &[u8]) -> Option<AdmissionBucket>`
  - `metrics::model::task_bucket(state: &[u8], outcome: Option<&[u8]>) -> Option<TaskBucket>`
  - `metrics::model::task_state_is_known(state: &[u8]) -> bool`
  - `metrics::model::unknown_admission_state_error() -> ControllerError`（`"metrics device aggregate: unknown admission state"`）
  - `metrics::model::task_aggregate_error(state: &[u8]) -> ControllerError`（state 不在四值域 → `"metrics task aggregate: unknown task state"`；否则 `"…unknown task outcome"`）
  - `metrics::model::{DeviceCounts, TaskCounts, PoolCounts, ScrapeValues}`（字段见下）
  - `metrics::render::CONTENT_TYPE: &str`、`metrics::render::MAX_BODY_BYTES: usize = 65_536`
  - `metrics::render::render(values: &ScrapeValues) -> Result<String, ControllerError>`
  - `metrics::render::escape_label_value(value: &str) -> String`（三项转义，本模块唯一一处）

- [ ] **Step 1: Write the failing test**

`crates/rsetup-controller/src/metrics/model.rs`（仅测试模块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn message(error: ControllerError) -> String {
        match error {
            ControllerError::Config(text) => text,
            other => panic!("必须是 ControllerError::Config，实得 {other:?}"),
        }
    }

    /// §5.2.2 的持久层词汇：只有三个字节值有映射，其余（含大小写与尾部空格变体）都是未知桶。
    #[test]
    fn admission_label_maps_persisted_vocabulary() {
        assert_eq!(admission_label(b"APPROVED"), Some(AdmissionBucket::Approved));
        assert_eq!(admission_label(b"PENDING"), Some(AdmissionBucket::Pending));
        assert_eq!(admission_label(b"REVOKED"), Some(AdmissionBucket::Rejected));
        assert_eq!(AdmissionBucket::Rejected.as_str(), "REJECTED");
        for raw in [
            &b"approved"[..],
            &b""[..],
            &b"UNKNOWN"[..],
            &b"APPROVED "[..],
            &b"REVOKED "[..],
            &b"revoked"[..],
            &[0xFF, 0xFE][..],
        ] {
            assert_eq!(admission_label(raw, ), None, "{raw:?} 必须是未知桶");
        }
    }

    /// §5.2.2 全表逐条：非终态忽略 outcome；completed 严格五值 + NULL→failed。
    #[test]
    fn task_bucket_covers_documented_matrix() {
        assert_eq!(task_bucket(b"queued", None), Some(TaskBucket::Pending));
        assert_eq!(task_bucket(b"held", None), Some(TaskBucket::Pending));
        assert_eq!(task_bucket(b"running", None), Some(TaskBucket::Running));
        for outcome in [Some(b"success"), Some(b"failed"), None, Some(b"weird")] {
            assert_eq!(task_bucket(b"queued", outcome), Some(TaskBucket::Pending));
            assert_eq!(task_bucket(b"held", outcome), Some(TaskBucket::Pending));
            assert_eq!(task_bucket(b"running", outcome), Some(TaskBucket::Running));
        }
        assert_eq!(task_bucket(b"completed", Some(b"success")), Some(TaskBucket::Succeeded));
        for outcome in [b"failed", b"partial", b"cancelled", b"unknown"] {
            assert_eq!(task_bucket(b"completed", Some(&outcome[..])), Some(TaskBucket::Failed));
        }
        // 唯一的 NULL 分支：终态却缺 outcome → 确定为未成功（不是整次抓取失败）。
        assert_eq!(task_bucket(b"completed", None), Some(TaskBucket::Failed));
        assert_eq!(TaskBucket::Pending.as_str(), "pending");
        assert_eq!(TaskBucket::Succeeded.as_str(), "succeeded");
    }

    #[test]
    fn task_bucket_fails_closed_on_unknown_state() {
        for state in [&b"queued "[..], &b"DONE"[..], &b""[..], &b"Queued"[..], &b"HELD"[..]] {
            for outcome in [None, Some(&b"success"[..])] {
                assert_eq!(task_bucket(state, outcome), None, "{state:?} 必须 fail-closed");
            }
        }
    }

    /// 未知 outcome 与未知 state 同等 fail-closed；大小写与尾部空格变体不得被任何折叠语义吞并。
    #[test]
    fn task_bucket_fails_closed_on_unknown_outcome() {
        for outcome in [
            &b"Success"[..],
            &b""[..],
            &b"succeded"[..],
            &b"success "[..],
            &b"SUCCESS"[..],
        ] {
            assert_eq!(
                task_bucket(b"completed", Some(outcome)),
                None,
                "未知 outcome {outcome:?} 必须 fail-closed"
            );
        }
    }

    /// 字节级映射不要求 UTF-8 合法性：state 恒须落在四值域；非终态忽略 outcome。
    #[test]
    fn unknown_bucket_values_need_not_be_valid_utf8() {
        let broken: &[u8] = &[0xFF, 0xFE];
        assert_eq!(task_bucket(broken, Some(b"success")), None);
        assert_eq!(task_bucket(b"completed", Some(broken)), None);
        assert_eq!(task_bucket(b"queued", Some(broken)), Some(TaskBucket::Pending));
        assert_eq!(task_bucket(b"held", Some(broken)), Some(TaskBucket::Pending));
        assert_eq!(task_bucket(b"running", Some(broken)), Some(TaskBucket::Running));
        assert_eq!(admission_label(broken), None);
    }

    /// 两条采集错误消息互不相同，且都不含原始取值（§5.2.2）。
    #[test]
    fn unknown_state_and_outcome_messages_are_distinct() {
        assert_eq!(
            message(task_aggregate_error(b"DONE")),
            "metrics task aggregate: unknown task state"
        );
        assert_eq!(
            message(task_aggregate_error(b"completed")),
            "metrics task aggregate: unknown task outcome"
        );
        assert!(!task_state_is_known(b"DONE"));
        assert!(task_state_is_known(b"held"));
        for text in [
            message(task_aggregate_error(b"DONE")),
            message(task_aggregate_error(b"completed")),
            message(unknown_admission_state_error()),
        ] {
            assert!(!text.contains("DONE"), "错误消息不得回显原始取值：{text}");
            assert!(!text.contains("APPROVED"), "错误消息不得回显原始取值：{text}");
        }
        assert_eq!(
            message(unknown_admission_state_error()),
            "metrics device aggregate: unknown admission state"
        );
    }
}
```

`crates/rsetup-controller/src/metrics/render.rs`（仅测试模块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::EDITION;
    use crate::metrics::model::{DeviceCounts, PoolCounts, ScrapeValues, TaskCounts};
    use crate::ControllerError;

    /// §5.6 的 golden 文本。`@VERSION@` 是版本占位符：规范 §5.2.1 要求测试**不硬编码**
    /// 版本号（改断言"等于 CARGO_PKG_VERSION"），渲染逐字节比对仍以替换后的定本文执行。
    const GOLDEN: &str = r#"# HELP rsetup_controller_build_info Build information for the rsetup-controller process, always 1.
# TYPE rsetup_controller_build_info gauge
rsetup_controller_build_info{version="@VERSION@",edition="2024"} 1
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
"#;

    /// §5.6 fixture：approved 7 / pending 2 / revoked 1；pending 1 / running 2 / succeeded 40 / failed 3。
    fn fixture() -> ScrapeValues {
        ScrapeValues {
            version: env!("CARGO_PKG_VERSION"),
            edition: EDITION,
            uptime_seconds: 4213,
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
            pool: PoolCounts { active: 2, idle: 3 },
        }
    }

    fn golden() -> String {
        GOLDEN.replace("@VERSION@", env!("CARGO_PKG_VERSION"))
    }

    #[test]
    fn render_matches_golden_exposition() {
        let rendered = render(&fixture()).expect("fixture 必须可渲染");
        assert_eq!(rendered, golden(), "golden 逐字节不一致");
        assert!(rendered.ends_with('\n'), "必须以换行结尾");
        assert!(!rendered.ends_with("\n\n"), "末尾不得有多余空行");
        assert_eq!(
            env!("CARGO_PKG_VERSION").len(),
            env!("CARGO_PKG_VERSION").chars().count(),
            "版本标签必须是 ASCII"
        );
    }

    /// 全 0 fixture 仍输出全部 11 条序列（缺序列会让 PromQL 出现空洞、absent() 误报）。
    #[test]
    fn render_emits_zero_valued_series() {
        let values = ScrapeValues {
            version: env!("CARGO_PKG_VERSION"),
            edition: EDITION,
            uptime_seconds: 0,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        };
        let rendered = render(&values).unwrap();
        assert_eq!(sample_lines(&rendered).len(), 11);
        for line in sample_lines(&rendered) {
            assert!(line.ends_with(" 0") || line.ends_with("} 0") || line.ends_with(" 1"), "{line}");
        }
        assert!(golden_lines_present(&rendered));
    }

    #[test]
    fn render_series_count_and_sum_invariants() {
        let values = fixture();
        let rendered = render(&values).unwrap();
        let lines = sample_lines(&rendered);
        assert_eq!(lines.len(), 11, "§5.1 第 4 条：序列集合恒定为 11 条");
        let devices: u64 = lines
            .iter()
            .filter(|line| line.starts_with("rsetup_controller_devices_total"))
            .map(|line| value_of(line))
            .sum();
        let tasks: u64 = lines
            .iter()
            .filter(|line| line.starts_with("rsetup_controller_tasks_total"))
            .map(|line| value_of(line))
            .sum();
        assert_eq!(devices, 10, "三桶之和必须等于设备表行数");
        assert_eq!(tasks, 46, "四桶之和必须等于主任务表行数");
        assert_eq!(value_of(&lines[0]), 1, "build_info 恒为 1");
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("rsetup_controller_devices_total"))
                .map(|line| line.split('{').nth(1).unwrap().split('=').nth(1).unwrap())
                .collect::<Vec<_>>()
                .join(","),
            "\"APPROVED\"},\"PENDING\"},\"REJECTED\"}",
            "§5.1 第 3 条固定标签顺序"
        );
    }

    #[test]
    fn render_declares_every_family_as_gauge() {
        let rendered = render(&fixture()).unwrap();
        let types: Vec<&str> = rendered
            .lines()
            .filter(|line| line.starts_with("# TYPE"))
            .collect();
        assert_eq!(types.len(), 5, "五个族各一条 TYPE");
        assert!(types.iter().all(|line| line.ends_with(" gauge")));
        assert_eq!(
            types.join("\n"),
            "# TYPE rsetup_controller_build_info gauge\n\
             # TYPE rsetup_controller_uptime_seconds gauge\n\
             # TYPE rsetup_controller_devices_total gauge\n\
             # TYPE rsetup_controller_tasks_total gauge\n\
             # TYPE rsetup_controller_db_pool_connections gauge"
        );
    }

    /// 三项转义（§5.1 第 6 条）：`\\`、`\"`、`\n`。
    #[test]
    fn render_escapes_label_values() {
        let values = ScrapeValues {
            version: "a\"b\\c\nd",
            edition: EDITION,
            uptime_seconds: 1,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        };
        let rendered = render(&values).unwrap();
        assert!(
            rendered.contains("version=\"a\\\"b\\\\c\\nd\""),
            "转义不符合 0.0.4 转义表：{rendered}"
        );
        assert!(!rendered.contains("a\"b\\c\nd"));
    }

    /// `\r`、`\t` 与其他控制字符 MUST 逐字节原样输出（0.0.4 转义表只有三项）。
    #[test]
    fn render_does_not_escape_carriage_return_or_tab() {
        let values = ScrapeValues {
            version: "a\r\tb\u{1}",
            edition: EDITION,
            uptime_seconds: 1,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        };
        let rendered = render(&values).unwrap();
        assert!(rendered.contains("version=\"a\r\tb\u{1}\""), "CR/TAB 被额外转义");
        assert!(!rendered.contains("\\r"));
        assert!(!rendered.contains("\\t"));
    }

    /// 数值一律无符号十进制（等价 `^[a-z_]+(\{[^}]*\})? [0-9]+$`，不引入 regex）。
    #[test]
    fn render_values_are_unsigned_decimal() {
        let rendered = render(&fixture()).unwrap();
        for line in sample_lines(&rendered) {
            assert!(is_unsigned_sample_line(line), "非法样本行：{line:?}");
        }
        assert!(!rendered.contains('e'));
        assert!(!rendered.contains("NaN"));
        assert!(!rendered.contains("+Inf"));
        assert!(!rendered.contains("-1"));
    }

    #[test]
    fn render_is_deterministic() {
        let values = fixture();
        assert_eq!(render(&values).unwrap(), render(&values).unwrap());
    }

    #[test]
    fn content_type_is_the_byte_constant() {
        assert_eq!(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8");
        assert_eq!(MAX_BODY_BYTES, 65_536);
    }

    /// 渲染层内部 fail-closed：超限返回 Err，消息不含任何指标文本片段（I5）。
    #[test]
    fn render_rejects_oversized_body() {
        // 标签值是唯一可被外部拉长的输入（正常路径由 §3.4/§5.2.1 限定）。
        let huge: &'static str = Box::leak("x".repeat(MAX_BODY_BYTES + 1).into_boxed_str());
        let values = ScrapeValues {
            version: huge,
            edition: EDITION,
            uptime_seconds: 1,
            devices: DeviceCounts::default(),
            tasks: TaskCounts::default(),
            pool: PoolCounts::default(),
        };
        match render(&values) {
            Err(ControllerError::Config(text)) => {
                assert_eq!(text, "metrics exposition exceeds size limit");
                assert!(!text.contains("rsetup_controller"), "错误消息含指标文本：{text}");
            }
            other => panic!("超限必须 Err(Config)，实得 {other:?}"),
        }
        // 恰好等于上限的输入不得被拒绝（边界是"超过"）。
        let edge = "x".repeat(MAX_BODY_BYTES - 120);
        let edge: &'static str = Box::leak(edge.into_boxed_str());
        assert!(
            render(&ScrapeValues {
                version: edge,
                edition: EDITION,
                uptime_seconds: 1,
                devices: DeviceCounts::default(),
                tasks: TaskCounts::default(),
                pool: PoolCounts::default(),
            })
            .is_ok()
        );
    }

    /// 非 `#` 开头的行即样本行。
    fn sample_lines(rendered: &str) -> Vec<String> {
        rendered
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    fn golden_lines_present(rendered: &str) -> bool {
        ["rsetup_controller_build_info",
         "rsetup_controller_uptime_seconds",
         "rsetup_controller_devices_total",
         "rsetup_controller_tasks_total",
         "rsetup_controller_db_pool_connections"]
            .iter()
            .all(|name| rendered.contains(&format!("# TYPE {name} gauge")))
    }

    fn value_of(line: &str) -> u64 {
        line.rsplit_once(' ')
            .unwrap()
            .1
            .parse()
            .unwrap_or_else(|_| panic!("取值非法：{line:?}"))
    }

    /// 手写最小判据（等价 `^[a-z_]+(\{[^}]*\})? [0-9]+$`）：本仓库不引入 regex 依赖。
    fn is_unsigned_sample_line(line: &str) -> bool {
        let Some((head, value)) = line.rsplit_once(' ') else {
            return false;
        };
        if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return false;
        }
        let (name, labels) = match head.split_once('{') {
            Some((name, tail)) => match tail.strip_suffix('}') {
                Some(labels) => (name, Some(labels)),
                None => return false,
            },
            None => (head, None),
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            return false;
        }
        if let Some(labels) = labels {
            if labels.is_empty() || labels.contains('}') || !labels.contains('=') {
                return false;
            }
        }
        true
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p rsetup-controller --lib metrics::model metrics::render
```

（分两条跑：`cargo test -p rsetup-controller --lib metrics::model`、`cargo test -p rsetup-controller --lib metrics::render`。）

预期：**编译失败（RED）**：

```text
error[E0432]: unresolved import `super::admission_label`
error[E0433]: failed to resolve: use of undeclared type `AdmissionBucket`
error[E0432]: unresolved imports `super::CONTENT_TYPE`, `super::MAX_BODY_BYTES`, `super::render`
error: could not compile `rsetup-controller` (lib test) due to N previous errors
```

- [ ] **Step 3: Write minimal implementation**

`crates/rsetup-controller/src/metrics/model.rs` 的实现部分：

```rust
//! 指标数据模型与字节精确的标签映射（规范 §5.2/§5.2.2）。
//!
//! 纯函数层：无 I/O、无 async、不 `use axum`/`sqlx`。映射一律 `&[u8]` 逐字节比较，
//! **不做任何字符级折叠**（大小写、尾部空格、`PAD SPACE` 语义都不存在）：数据库侧
//! v3 起已无 CHECK 约束（§5.2.2 事实修正），因此"SQL 字节级分组 + 本文件字节精确
//! 映射 + 未知值 503"是枚举防御的唯一通路。

use crate::ControllerError;

/// 持久层 `devices.admission_state` 的合法字节词汇（`Vec<u8>` 与之逐字节比较）。
pub const ADMISSION_APPROVED: &[u8] = b"APPROVED";
pub const ADMISSION_PENDING: &[u8] = b"PENDING";
/// 持久层 `REVOKED` → 指标标签 `REJECTED`（§5.2.2 刻意留档的命名映射偏差）。
pub const ADMISSION_REVOKED: &[u8] = b"REVOKED";

/// 主任务 `state` 四值域（`2026-09-23-controller-v1-04-task-lifecycle.md`）。
pub const TASK_STATE_QUEUED: &[u8] = b"queued";
pub const TASK_STATE_RUNNING: &[u8] = b"running";
pub const TASK_STATE_HELD: &[u8] = b"held";
pub const TASK_STATE_COMPLETED: &[u8] = b"completed";

/// 主任务 `outcome` 五值域（`integrity.rs` 的真实取值域）。
pub const TASK_OUTCOME_SUCCESS: &[u8] = b"success";
pub const TASK_OUTCOME_FAILED: &[u8] = b"failed";
pub const TASK_OUTCOME_PARTIAL: &[u8] = b"partial";
pub const TASK_OUTCOME_CANCELLED: &[u8] = b"cancelled";
pub const TASK_OUTCOME_UNKNOWN: &[u8] = b"unknown";

/// 设备准入三桶（标签顺序即 §5.1 第 3 条固定顺序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionBucket {
    Approved,
    Pending,
    Rejected,
}

impl AdmissionBucket {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "APPROVED",
            Self::Pending => "PENDING",
            Self::Rejected => "REJECTED",
        }
    }
}

/// 主任务四桶（固定标签域 `pending|running|succeeded|failed`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskBucket {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl TaskBucket {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

/// 字节精确映射。`'PENDING '`、`'approved'` 与任何非 UTF-8 字节值都无法匹配合法值，
/// 一律 `None`（→ 采集层 503），绝不静默归入任一桶。
pub fn admission_label(raw: &[u8]) -> Option<AdmissionBucket> {
    if raw == ADMISSION_APPROVED {
        return Some(AdmissionBucket::Approved);
    }
    if raw == ADMISSION_PENDING {
        return Some(AdmissionBucket::Pending);
    }
    if raw == ADMISSION_REVOKED {
        return Some(AdmissionBucket::Rejected);
    }
    None
}

/// `state` 是否落在四值域内（决定未知取值的两条去敏消息走哪一条）。
pub fn task_state_is_known(state: &[u8]) -> bool {
    state == TASK_STATE_QUEUED
        || state == TASK_STATE_RUNNING
        || state == TASK_STATE_HELD
        || state == TASK_STATE_COMPLETED
}

/// §5.2.2 统计口径（唯一判据）：非终态只按 `state` 聚合，`outcome` 既不校验也不参与；
/// 终态 `completed` 严格校验五值与 `NULL`（NULL 归 `failed`），字节级其他取值 → `None`。
pub fn task_bucket(state: &[u8], outcome: Option<&[u8]>) -> Option<TaskBucket> {
    if state == TASK_STATE_QUEUED || state == TASK_STATE_HELD {
        return Some(TaskBucket::Pending);
    }
    if state == TASK_STATE_RUNNING {
        return Some(TaskBucket::Running);
    }
    if state != TASK_STATE_COMPLETED {
        return None;
    }
    let outcome = outcome?;
    if outcome == TASK_OUTCOME_SUCCESS {
        return Some(TaskBucket::Succeeded);
    }
    if outcome == TASK_OUTCOME_FAILED
        || outcome == TASK_OUTCOME_PARTIAL
        || outcome == TASK_OUTCOME_CANCELLED
        || outcome == TASK_OUTCOME_UNKNOWN
    {
        return Some(TaskBucket::Failed);
    }
    None
}

/// 设备侧未知取值的固定去敏消息（不含原始取值，I5）。
pub fn unknown_admission_state_error() -> ControllerError {
    ControllerError::Config("metrics device aggregate: unknown admission state".into())
}

/// 任务侧未知取值的两条去敏消息（§5.2.2）：`state` 本身不在四值域 → unknown task
/// state；`state` 合法而 `outcome` 不在枚举 → unknown task outcome。都不回显取值。
pub fn task_aggregate_error(state: &[u8]) -> ControllerError {
    if task_state_is_known(state) {
        ControllerError::Config("metrics task aggregate: unknown task outcome".into())
    } else {
        ControllerError::Config("metrics task aggregate: unknown task state".into())
    }
}

/// 设备准入分布（三桶恒定输出，计数为 0 也输出该序列）。
#[derive(Clone, Copy, Debug, Default)]
pub struct DeviceCounts {
    pub approved: u64,
    pub pending: u64,
    pub rejected: u64,
}

/// 主任务状态分布（四桶）。
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskCounts {
    pub pending: u64,
    pub running: u64,
    pub succeeded: u64,
    pub failed: u64,
}

/// 连接池内存读数（§5.3 第 5 条，永不发起连接或查询）。
#[derive(Clone, Copy, Debug, Default)]
pub struct PoolCounts {
    pub active: u64,
    pub idle: u64,
}

/// 渲染层唯一的输入形态：golden 测试直接构造它，不需要 DB 或 HTTP（§5.5）。
#[derive(Clone, Copy, Debug)]
pub struct ScrapeValues {
    /// 构建常量：`env!("CARGO_PKG_VERSION")`。
    pub version: &'static str,
    /// 构建常量：`metrics::EDITION`。
    pub edition: &'static str,
    pub uptime_seconds: u64,
    pub devices: DeviceCounts,
    pub tasks: TaskCounts,
    pub pool: PoolCounts,
}
```

`crates/rsetup-controller/src/metrics/render.rs` 的实现部分：

```rust
//! Prometheus exposition format（text 0.0.4）渲染（规范 §5.1/§5.6）。
//!
//! 纯函数：输出逐字节确定（族顺序、标签顺序、末尾单个 `\n`），供 golden 测试钉住。
//! 不引入任何指标 SDK/注册中心，也不依赖其隐式指标（§1.3、§5.1 第 10 条）。

use crate::ControllerError;
use crate::metrics::model::ScrapeValues;

/// 逐字节固定的响应类型（含 `version=0.0.4` 前后的分号与单空格）。
pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

/// 响应体上限：序列上限 11 条时实际 < 1 KiB，该上限只是防御性护栏（§5.1 第 9 条）。
pub const MAX_BODY_BYTES: usize = 65_536;

const NAME_BUILD_INFO: &str = "rsetup_controller_build_info";
const NAME_UPTIME: &str = "rsetup_controller_uptime_seconds";
const NAME_DEVICES: &str = "rsetup_controller_devices_total";
const NAME_TASKS: &str = "rsetup_controller_tasks_total";
const NAME_POOL: &str = "rsetup_controller_db_pool_connections";

// HELP 文本为 ASCII 英文、单行、不含换行；devices/tasks 两条同时留档"求和 = 表行数"
// 与 `_total` 后缀的 gauge 语义（§5.4 第 2 条）。
const HELP_BUILD_INFO: &str =
    "Build information for the rsetup-controller process, always 1.";
const HELP_UPTIME: &str =
    "Seconds elapsed since the controller process started, from the monotonic clock.";
const HELP_DEVICES: &str = "Devices grouped by admission state; sum over all labels equals the devices table row count.";
const HELP_TASKS: &str =
    "Main tasks grouped by normalized status; sum over all labels equals the main_tasks table row count.";
const HELP_POOL: &str =
    "Database pool connections by state at scrape time, active or idle.";

/// 标签值转义（本模块唯一一处）：严格按 0.0.4 legacy 文本规范**只**处理三个字符。
/// MUST NOT 转义 `\r`、`\t` 或其他控制字符——该格式的转义表里没有这两项。
/// 实现仍 MUST 经过本函数，不得依赖"输入总是安全的"。
pub fn escape_label_value(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// 渲染整份 exposition。族顺序与样本顺序固定（§5.1 第 3 条），序列集合恒定 11 条。
/// 超过 `MAX_BODY_BYTES` → 渲染层内部 fail-closed（handler 映射 503）。
pub fn render(values: &ScrapeValues) -> Result<String, ControllerError> {
    let mut out = String::with_capacity(1024);
    family(&mut out, NAME_BUILD_INFO, HELP_BUILD_INFO);
    out.push_str(&format!(
        "{NAME_BUILD_INFO}{{version=\"{}\",edition=\"{}\"}} 1\n",
        escape_label_value(values.version),
        escape_label_value(values.edition),
    ));
    family(&mut out, NAME_UPTIME, HELP_UPTIME);
    out.push_str(&format!("{NAME_UPTIME} {}\n", values.uptime_seconds));
    family(&mut out, NAME_DEVICES, HELP_DEVICES);
    out.push_str(&format!(
        "{NAME_DEVICES}{{admission=\"APPROVED\"}} {}\n",
        values.devices.approved
    ));
    out.push_str(&format!(
        "{NAME_DEVICES}{{admission=\"PENDING\"}} {}\n",
        values.devices.pending
    ));
    out.push_str(&format!(
        "{NAME_DEVICES}{{admission=\"REJECTED\"}} {}\n",
        values.devices.rejected
    ));
    family(&mut out, NAME_TASKS, HELP_TASKS);
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"pending\"}} {}\n",
        values.tasks.pending
    ));
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"running\"}} {}\n",
        values.tasks.running
    ));
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"succeeded\"}} {}\n",
        values.tasks.succeeded
    ));
    out.push_str(&format!(
        "{NAME_TASKS}{{status=\"failed\"}} {}\n",
        values.tasks.failed
    ));
    family(&mut out, NAME_POOL, HELP_POOL);
    out.push_str(&format!("{NAME_POOL}{{state=\"active\"}} {}\n", values.pool.active));
    out.push_str(&format!("{NAME_POOL}{{state=\"idle\"}} {}\n", values.pool.idle));

    if out.len() > MAX_BODY_BYTES {
        // 去敏：固定文案，不含任何指标文本片段与长度。
        return Err(ControllerError::Config(
            "metrics exposition exceeds size limit".into(),
        ));
    }
    Ok(out)
}

/// 族头：`# HELP <name> <text>` + `# TYPE <name> gauge`（族之间不插入空行）。
fn family(out: &mut String, name: &str, help: &str) {
    out.push_str(&format!("# HELP {name} {help}\n"));
    out.push_str(&format!("# TYPE {name} gauge\n"));
}
```

`crates/rsetup-controller/src/metrics/mod.rs` 追加：

```rust
pub mod model;
pub mod render;
```

以及再导出：

```rust
pub use model::{
    AdmissionBucket, DeviceCounts, PoolCounts, ScrapeValues, TaskBucket, TaskCounts,
    admission_label, task_bucket,
};
pub use render::{CONTENT_TYPE, MAX_BODY_BYTES, render};
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo fmt --package rsetup-controller
cargo test -p rsetup-controller --lib metrics::model
cargo test -p rsetup-controller --lib metrics::render
```

预期（GREEN）：

```text
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; N filtered out
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

判据：`0 failed`，且 `render_matches_golden_exposition` 与 `render_does_not_escape_carriage_return_or_tab` 都在输出中。若 golden 比对失败，输出会给出逐字节 diff——**改实现，不改 golden 常量**（golden 是规范 §5.6 的定本）。

- [ ] **Step 5: Commit**

```bash
git add crates/rsetup-controller/src/metrics/mod.rs crates/rsetup-controller/src/metrics/model.rs \
  crates/rsetup-controller/src/metrics/render.rs
git commit -m "feat(controller): 增加指标数据模型与普罗米修斯文本渲染器"
```
---

## Task 4: 数据采集与连接生命周期强绑定（`metrics/collect.rs`，I7 核心）

**Files:**
- Create: `crates/rsetup-controller/src/metrics/collect.rs`
- Modify: `crates/rsetup-controller/src/metrics/mod.rs`（`pub mod collect;` + 再导出）
- Test: 同文件 `#[cfg(test)]`（L1，规范 §7.4 末行 uptime 用例 + §5.5.1 六条硬规则的静态门禁）
- 关联（本 Task 只登记，执行在 Task 7）：`tests/metrics_real_db.rs` 的 L4 三组用例（§7.7）

**Interfaces:**
- Consumes：`crate::DbPool`（`pub struct DbPool(pub sqlx::MySqlPool)`）；`metrics::model::{admission_label, task_bucket, task_aggregate_error, unknown_admission_state_error, DeviceCounts, TaskCounts, PoolCounts, ScrapeValues}`（Task 3）；`metrics::{EDITION}`（Task 1）；`crate::ControllerError::{Config, Database}`。
- Produces（Task 5/6 依赖的精确签名，规范 §5.5/§5.5.1）：
  - `metrics::collect::MAX_CONCURRENT_SCRAPE: usize = 4`
  - `metrics::collect::SCRAPE_TIMEOUT: Duration = Duration::from_secs(2)`
  - `metrics::collect::POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(500)`
  - `metrics::collect::DEVICE_AGGREGATE_SQL: &str`、`metrics::collect::TASK_AGGREGATE_SQL: &str`（`pub` 供 L4 与源码级断言引用）
  - `metrics::collect::MetricsFuture<'a, T>`
  - `metrics::collect::MetricsSnapshotSource`（object-safe：`fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot>`）
  - `metrics::collect::MetricsSnapshot { devices: DeviceCounts, tasks: TaskCounts }`
  - `metrics::collect::SqlxMetricsSnapshotSource` + `new(db: DbPool) -> Self`（**只接受既有池的 Clone**）
  - `metrics::collect::MetricsRuntime { started_at: Instant, version: &'static str, edition: &'static str }` + `new()`、`uptime_seconds() -> u64`、`scrape_values(&self, snapshot: &MetricsSnapshot, pool: PoolCounts) -> ScrapeValues`、`Default`
  - `metrics::collect::pool_counts(&DbPool) -> PoolCounts`
  - `metrics::collect::snapshot_via(pool: &DbPool) -> Result<MetricsSnapshot, ControllerError>`（§5.5.1 骨架的公开形态）
  - 私有 `async fn run_aggregates(conn: &mut PoolConnection<MySqlConnection>) -> Result<MetricsSnapshot, ControllerError>`
  - `#[cfg(test)] metrics::collect::FakeMetricsSnapshotSource`（§7.5 的三种行为 + 调用计数）

- [ ] **Step 1: Write the failing test**

`crates/rsetup-controller/src/metrics/collect.rs`（仅测试模块；`FakeMetricsSnapshotSource` 的类型本体属 Step 3，因为它被 Task 5 的 L2 测试复用）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::model::{DeviceCounts, PoolCounts, TaskCounts};
    use std::time::{Duration, Instant};

    fn message(error: ControllerError) -> String {
        match error {
            ControllerError::Config(text) => text,
            other => panic!("必须是 ControllerError::Config，实得 {other:?}"),
        }
    }

    /// §7.4 末行：uptime 由单调锚点求差，宿主墙钟跳变不得使其倒退或清零。
    #[test]
    fn uptime_never_regresses_across_wall_clock_jumps() {
        let started = Instant::now() - Duration::from_secs(60);
        let runtime = MetricsRuntime {
            started_at: started,
            version: env!("CARGO_PKG_VERSION"),
            edition: crate::metrics::EDITION,
        };
        assert!(
            runtime.uptime_seconds() >= 60,
            "锚点在过去 60 秒时 uptime 必须 >= 60"
        );
        let first = runtime.uptime_seconds();
        for _ in 0..8 {
            assert!(runtime.uptime_seconds() >= first, "uptime 不得倒退");
        }
        // 锚点即当下：uptime 为 0（不是负数回卷）。
        let fresh = MetricsRuntime {
            started_at: Instant::now(),
            ..MetricsRuntime::new()
        };
        assert_eq!(fresh.uptime_seconds(), 0);
    }

    /// `new()` 取构建常量，`scrape_values` 合成渲染输入（uptime 来自锚点，计数来自参数）。
    #[test]
    fn scrape_values_uses_build_constants_and_pool_reading() {
        let runtime = MetricsRuntime::new();
        let snapshot = MetricsSnapshot {
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
        };
        let values = runtime.scrape_values(&snapshot, PoolCounts { active: 2, idle: 3 });
        assert_eq!(values.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(values.edition, crate::metrics::EDITION);
        assert_eq!(values.devices, snapshot.devices);
        assert_eq!(values.tasks, snapshot.tasks);
        assert_eq!((values.pool.active, values.pool.idle), (2, 3));
        assert!(values.uptime_seconds() < 5);
    }

    /// §5.3 第 5 条：池 gauge 只读内存字段，绝不发起连接或查询。
    /// lazy 池（永不实际拨号）上 `size()` 恒 0 → `active` 饱和为 0，且调用不产生连接。
    #[test]
    fn pool_counts_reads_memory_only_on_lazy_pool() {
        let pool = lazy_pool();
        let counts = pool_counts(&pool);
        assert_eq!(counts.active, 0);
        assert_eq!(counts.idle, 0);
        assert_eq!(pool.0.size(), 0, "读取池读数不得建立连接");
        assert_eq!(pool.0.num_idle(), 0);
    }

    /// §7.4 的 golden 取值装配路径复用同一 fixture（求和不变量在采集侧同样成立）。
    #[test]
    fn snapshot_buckets_sum_to_fixture_totals() {
        let snapshot = MetricsSnapshot {
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
        };
        assert_eq!(
            snapshot.devices.approved + snapshot.devices.pending + snapshot.devices.rejected,
            10
        );
        assert_eq!(
            snapshot.tasks.pending
                + snapshot.tasks.running
                + snapshot.tasks.succeeded
                + snapshot.tasks.failed,
            46
        );
    }

    /// §5.3 约束 2/4：两条聚合的 SQL 必须是字节级 `CAST(... AS BINARY)` 分组写法，
    /// 选择列表与 `GROUP BY` 的表达式逐字符相同（`ONLY_FULL_GROUP_BY`），且不含
    /// `COLLATE`/`JOIN`/`WHERE`/`ORDER BY`/`MAX_EXECUTION_TIME`。
    #[test]
    fn aggregate_sql_is_byte_level_grouping() {
        let normalized = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            normalized(DEVICE_AGGREGATE_SQL),
            "SELECT CAST(admission_state AS BINARY) AS bucket, COUNT(*) AS n \
             FROM devices GROUP BY CAST(admission_state AS BINARY)"
        );
        assert_eq!(
            normalized(TASK_AGGREGATE_SQL),
            "SELECT CAST(state AS BINARY) AS state, CAST(outcome AS BINARY) AS outcome, \
             COUNT(*) AS n FROM main_tasks \
             GROUP BY CAST(state AS BINARY), CAST(outcome AS BINARY)"
        );
        for sql in [DEVICE_AGGREGATE_SQL, TASK_AGGREGATE_SQL] {
            let flat = normalized(sql);
            assert!(!flat.contains("COLLATE"), "MUST NOT 使用 COLLATE（PAD SPACE 陷阱）：{flat}");
            assert!(!flat.contains("JOIN"), "两条聚合都是单表查询：{flat}");
            assert!(!flat.contains("WHERE"), "全表分布不得加过滤条件：{flat}");
            assert!(!flat.contains("ORDER BY"), "行数恒定，不需要稳定顺序：{flat}");
            assert!(
                !flat.contains("MAX_EXECUTION_TIME"),
                "该提示须先经 §7.7 双引擎确认才可加入（§5.5.1 规则 5）：{flat}"
            );
        }
    }

    /// §5.5.1 规则 2 + 三轮复审 F1：连接借出后到 `close().await` 之间禁止 `?` 早退。
    /// 源码级门禁（与 §7.8 第 9 条 grep 互为冗余）：本文件的错误一律折入 `scrape_outcome`，
    /// 并在**无条件**的 `close()` 之后统一返回。
    /// 去掉行注释后的纯代码文本（判定"有没有 `?` 早退"必须看代码而不是注释；
    /// 本文件的注释里大量出现 `?`、`return`、`detach` 等词，直接扫原文会误判）。
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn snapshot_kernel_has_no_early_exit_between_acquire_and_close() {
        let source = include_str!("collect.rs");
        let start = source
            .find("pub async fn snapshot_via")
            .expect("必须存在 snapshot_via 内核");
        let body = &source[start..source.find("\n}\n", start).expect("snapshot_via 函数体结束") + 3];
        let code = code_only(body);
        assert!(
            code.contains("conn.close_on_drop();"),
            "第 2 步兜底缺失：借到连接必须立即 close_on_drop()"
        );
        assert!(
            code.contains("let _ = conn.close().await;"),
            "第 4 步缺失：必须无条件 await conn.close()"
        );
        let borrowed_at = code
            .find("conn.close_on_drop();")
            .expect("close_on_drop 调用必须存在");
        let close_at = code.find("conn.close().await").expect("close 调用必须存在");
        let acquire_at = code
            .find("POOL_ACQUIRE_TIMEOUT")
            .expect("显式 acquire 超时必须存在");
        assert!(acquire_at < borrowed_at && borrowed_at < close_at, "顺序必须是 acquire → 兜底 → close");
        // F1：自"连接已借出"到"close 完成"之间不得出现 `?` 或 `return`（错误一律折入变量）。
        let between = &code[borrowed_at..close_at];
        assert!(!between.contains('?'), "连接借出后到 close 之前出现 `?` 早退（F1 违规）：{between}");
        assert!(!between.contains("return"), "连接借出后到 close 之前出现 return 早退（F1 违规）：{between}");
        // 只有两层超时（acquire 与聚合），MUST NOT 为 close 再设会提前放行许可的第三层超时。
        assert_eq!(
            code.matches("tokio::time::timeout(").count(),
            2,
            "恰好两层超时：POOL_ACQUIRE_TIMEOUT 与 SCRAPE_TIMEOUT"
        );
        assert!(code.contains("scrape_outcome"), "结果必须折入变量后统一返回");
        assert!(!code.contains("detach"), "detach 会让进程实际超出 max_connections");
        assert!(!code.contains("disconnect_idle"), "对借出中的连接无效，MUST NOT 依赖");
    }

    /// §5.5.1 规则 1/3 与 §5.5 的常量取值：`POOL_ACQUIRE_TIMEOUT < SCRAPE_TIMEOUT`、
    /// 并发闸上限 4（与 §1.3 "至多 4 槽、至少 6 槽属于业务" 的可指认数值）。
    #[test]
    fn guard_constants_match_spec() {
        assert_eq!(MAX_CONCURRENT_SCRAPE, 4);
        assert_eq!(SCRAPE_TIMEOUT, Duration::from_secs(2));
        assert_eq!(POOL_ACQUIRE_TIMEOUT, Duration::from_millis(500));
        assert!(POOL_ACQUIRE_TIMEOUT < Duration::from_secs(1));
        assert!(POOL_ACQUIRE_TIMEOUT < SCRAPE_TIMEOUT);
    }

    /// 采集侧的两条去敏错误在 `run_aggregates` 映射路径上可指认（L1 侧的等价断言：
    /// 未知取值经映射函数必然产生去敏错误，DB 行数据绝不进消息）。
    #[test]
    fn mapping_errors_are_redacted_and_distinguishable() {
        assert_eq!(
            message(crate::metrics::model::task_aggregate_error(b"DONE")),
            "metrics task aggregate: unknown task state"
        );
        assert_eq!(
            message(crate::metrics::model::task_aggregate_error(b"completed")),
            "metrics task aggregate: unknown task outcome"
        );
        assert_eq!(
            message(crate::metrics::model::unknown_admission_state_error()),
            "metrics device aggregate: unknown admission state"
        );
        // 固定文案里绝不出现 SQL 片段或行数据。
        for text in [
            message(crate::metrics::model::unknown_admission_state_error()),
            message(crate::metrics::model::task_aggregate_error(b"x")),
        ] {
            assert!(!text.contains("SELECT"), "{text}");
            assert!(!text.contains("devices"), "{text}");
        }
    }

    /// 挂起中的 fake 可被外部取消（证明 handler 侧取消只影响 handler；
    /// 采集任务的许可持有语义由 Task 5 的 in-proc 屏障用例与 Task 7 的 L4 用例覆盖）。
    #[tokio::test]
    async fn fake_source_can_be_cancelled_by_caller() {
        let source = FakeMetricsSnapshotSource::hanging();
        let outcome = tokio::time::timeout(
            Duration::from_millis(20),
            source.snapshot(),
        )
        .await;
        assert!(outcome.is_err(), "挂起源必须可被调用方取消");
        assert_eq!(source.calls(), 0, "未产出结果时不应计入完成次数");
    }

    #[tokio::test]
    async fn fake_source_fixed_and_fail_behaviours() {
        let snapshot = MetricsSnapshot::default();
        let source = FakeMetricsSnapshotSource::fixed(snapshot);
        assert!(matches!(source.snapshot().await, Ok(other) if other == snapshot));
        assert_eq!(source.calls(), 1);
        let source = FakeMetricsSnapshotSource::failing("metrics fixture failure");
        assert!(source.snapshot().await.is_err());
        assert_eq!(source.calls(), 1);
    }

    fn lazy_pool() -> DbPool {
        DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(Duration::from_millis(50))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .expect("lazy 池构造不连接"),
        )
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p rsetup-controller --lib metrics::collect
```

预期：**编译失败（RED）**：

```text
error[E0432]: unresolved imports `super::MAX_CONCURRENT_SCRAPE`, `super::MetricsRuntime`, `super::MetricsSnapshot`, `super::pool_counts`, `super::snapshot_via`, ...
error[E0433]: failed to resolve: use of undeclared type `FakeMetricsSnapshotSource`
error: could not compile `rsetup-controller` (lib test) due to N previous errors
```

注意：`snapshot_kernel_has_no_early_exit_between_acquire_and_close` 也必须在 Step 2 失败（`include_str!` 指向的文件此刻没有 `pub async fn snapshot_via`）——它就是 F1 回归网的红灯侧。

- [ ] **Step 3: Write minimal implementation**

`crates/rsetup-controller/src/metrics/collect.rs` 的实现部分（写在 Step 1 的测试模块之前）：

```rust
//! 采集层（规范 §5.3/§5.5/§5.5.1）：两条聚合 + 一次内存读取，以及**连接生命周期与
//! 并发闸许可的强绑定（I7）**。
//!
//! 三条不可让渡的事实决定了本文件的形状：
//! 1. 池槽位的释放与 handler 的返回天然解耦：`return_to_pool()` 会先 `ping()` 再释放
//!    permit，`close_on_drop()` 的 `Drop` 走的 `take_and_close()` 也是 `rt::spawn` 的
//!    后台关闭（外包 ≤5s）。只要许可随 handler 返回立即释放，"新一波借出新连接"就会与
//!    "上一波仍在后台关闭的连接"逐波叠加，总占用不受并发闸封顶（二轮复审 R1 的根因）。
//! 2. `acquire()` 的默认超时是 30 秒，与 2 秒抓取上限完全脱节 → 必须显式 500ms。
//! 3. `PoolConnection::close()` 全程持有池 permit（sqlx 文档："the connection permit
//!    is retained for the duration so that the pool may not exceed `max_connections`"），
//!    因此"在任务内 await close 完毕"既是池槽位的释放点，也是许可的释放点。
//!
//! 本文件 MUST NOT 依赖 `Pool::disconnect_idle()`（对借出中的连接无效）或
//! `PoolConnection::detach()`（会让进程实际超出 `max_connections`）。

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use sqlx::mysql::MySqlConnection;
use sqlx::pool::PoolConnection;
use sqlx::Row as _;

use crate::metrics::model::{
    admission_label, task_aggregate_error, task_bucket, unknown_admission_state_error,
    AdmissionBucket, DeviceCounts, PoolCounts, ScrapeValues, TaskBucket, TaskCounts,
};
use crate::metrics::EDITION;
use crate::{ControllerError, DbPool};

/// 并发闸上限：指标线任何时刻在途 + 关闭中的连接数至多 4 槽，共享池默认
/// `max_connections = 10` 中**至少 6 槽恒属主业务**（I7 的容量推论）。
pub const MAX_CONCURRENT_SCRAPE: usize = 4;

/// 两条聚合的整体执行上限（§5.5）。
pub const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);

/// 借连接的最长等待：池紧张时快速失败（映射 503），绝不排队 30s。
/// MUST NOT 大于 1s，且 MUST 明显小于 `SCRAPE_TIMEOUT`。
pub const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(500);

/// 设备准入分布（字节级分组：`'APPROVED'` 与 `'approved'`/`'APPROVED '` 是三个独立分组）。
/// 选择列表与 `GROUP BY` 的表达式逐字符相同（`ONLY_FULL_GROUP_BY`）。
///
/// MUST NOT 改用 `COLLATE utf8mb4_bin`：`utf8mb4_bin` 是 `PAD SPACE` 校对规则，
/// 比较时忽略尾部空格填充，`'PENDING' = 'PENDING '` 恒真，脏值会被折叠进合法分组，
/// 整条 fail-closed 链路在 SQL 层就被吞掉（§5.3 约束 2）。也不使用已弃用的 `BINARY`
/// 运算符（产生 deprecation 告警）——`CAST(... AS BINARY)` 不在弃用范围。
///
/// `/*+ MAX_EXECUTION_TIME(2000) */` 提示按 §5.5.1 规则 5 属于 SHOULD：MUST 先在 §7.7
/// 双引擎实测确认"被接受且生效"方可加入（Task 7），未确认前保持本拼法。
pub const DEVICE_AGGREGATE_SQL: &str = r"
SELECT CAST(admission_state AS BINARY) AS bucket, COUNT(*) AS n
FROM devices
GROUP BY CAST(admission_state AS BINARY)
";

/// 主任务状态分布（`state` 与 `outcome` 两列都按字节分组；`outcome` 可为 `NULL`）。
pub const TASK_AGGREGATE_SQL: &str = r"
SELECT CAST(state AS BINARY) AS state,
       CAST(outcome AS BINARY) AS outcome,
       COUNT(*) AS n
FROM main_tasks
GROUP BY CAST(state AS BINARY), CAST(outcome AS BINARY)
";

/// 与 `http_device_status::DeviceStatusSnapshotSource` 同形态的 dyn-safe 注入面（§5.5）。
pub type MetricsFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 数据源可注入：进程内 HTTP 测试不连真实库（fake 只能证明 handler/映射/渲染，
/// MUST NOT 被表述为"真实数据库聚合已验收"，§7.7）。
pub trait MetricsSnapshotSource: Send + Sync {
    fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot>;
}

/// 两条聚合的行级映射结果（终态：全部为 `u64` 的三桶/四桶计数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub devices: DeviceCounts,
    pub tasks: TaskCounts,
}

/// 运行时量：单调 uptime 锚点 + 两个构建常量（§5.2/§5.2.1）。
/// `started_at` 在 `main()` 第一条语句构造（§6.4），使 schema 检查、bootstrap 与
/// 绑端口全部计入 uptime。
pub struct MetricsRuntime {
    pub started_at: Instant,
    pub version: &'static str,
    pub edition: &'static str,
}

impl MetricsRuntime {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
            edition: EDITION,
        }
    }

    /// 单调时钟求差后取整秒（`u64::as_secs()` 天然饱和于 `u64::MAX`，不会回卷）。
    /// 宿主墙钟前跳/回拨都不影响：锚点与读数同为 `Instant`。
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// 渲染层唯一输入形态的合成点：DB 聚合 + 运行时量 + 池内存读数（§5.5）。
    pub fn scrape_values(
        &self,
        snapshot: &MetricsSnapshot,
        pool: PoolCounts,
    ) -> ScrapeValues {
        ScrapeValues {
            version: self.version,
            edition: self.edition,
            uptime_seconds: self.uptime_seconds(),
            devices: snapshot.devices,
            tasks: snapshot.tasks,
            pool,
        }
    }
}

impl Default for MetricsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// 生产汇总源。**只接受既有池的 Clone**：内部按 §5.5.1 执行 acquire 超时、
/// `close_on_drop()` 兜底，并在本调用内 await 连接关闭完毕后才返回——池槽位与
/// 并发闸许可随返回一并释放。生产装配只能由 `production.rs` 以 `inputs.auth.db`
/// 调用（§6.3），`main.rs` 与任何业务线 MUST NOT 自行构造它再作为入参传入。
pub struct SqlxMetricsSnapshotSource {
    db: DbPool,
}

impl SqlxMetricsSnapshotSource {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl MetricsSnapshotSource for SqlxMetricsSnapshotSource {
    fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
        Box::pin(snapshot_via(&self.db))
    }
}

/// 连接池内存读数（§5.3 第 5 条）：零网络操作，MUST NOT 因此发起连接或查询。
/// `size()` 是已创建连接数、`num_idle()` 是空闲数，二者之差即在途数；饱和减法规避
/// 两个读数非原子采集时的瞬时负差。
pub fn pool_counts(pool: &DbPool) -> PoolCounts {
    let size = u64::from(pool.0.size());
    let idle = pool.0.num_idle() as u64;
    PoolCounts {
        active: size.saturating_sub(idle),
        idle,
    }
}

/// 采集内核：`acquire` → 聚合 → **关闭** 全部在调用方任务内顺序完成。
/// 调用方 MUST 把并发闸许可移交本任务持有、直到本函数返回才释放——
/// 本函数的返回点就是"底层连接已完全关闭"的时点（I7）。
pub async fn snapshot_via(pool: &DbPool) -> Result<MetricsSnapshot, ControllerError> {
    // 1) 显式 acquire 超时：500ms 借不到连接 → 立即失败（映射 503）。
    //    此时没有任何连接产生，许可可立即释放。
    let mut conn = tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, pool.0.acquire())
        .await
        .map_err(|_| ControllerError::Config("metrics snapshot: pool acquire timeout".into()))?
        .map_err(|_| ControllerError::Config("metrics snapshot: pool acquire failed".into()))?;

    // 2) 借到即标记"用后即弃"（兜底）：若本 future 整体被取消（正常流程不可达，
    //    仅 runtime 关停/任务 abort），Drop 走 take_and_close() 的 ≤5s 有界关闭、
    //    绝不 return_to_pool() 的 ping-drain-再回池，脏连接永不交还业务线。
    conn.close_on_drop();

    // 3) 两条聚合在上限内顺序执行。自本步起直到第 4 步 close() 完成之前，
    //    **彻底禁止 `?`（或任何等价的提前 return / 提前 drop 分岔）**：查询报错
    //    与超时一律只把错误折进 scrape_outcome，继续走到第 4 步的关闭收尾。
    //    反例（三轮复审 F1，MUST NOT 复活）：`timeout(..).await.map_err(..)?`
    //    ——`?` 会在超时/报错时提前返回、跳过第 4 步 close()，conn 的 Drop 因
    //    close_on_drop() 改由后台任务异步关闭，而持有 OwnedPermit 的独立采集
    //    任务却在此刻退出并归还许可，重新引入"许可提前归还与后台关闭重叠"
    //    导致连接池累积耗尽的漏洞。
    let scrape_outcome = match tokio::time::timeout(SCRAPE_TIMEOUT, run_aggregates(&mut conn)).await
    {
        Ok(Ok(metrics)) => Ok(metrics),
        // 查询报错：沿用 run_aggregates 已映射的去敏错误，绝不提前返回。
        Ok(Err(db_err)) => Err(db_err),
        // 超时：固定去敏文案，绝不提前返回。
        Err(_) => Err(ControllerError::Config("metrics snapshot: query timeout".into())),
    };

    // 4) 池槽位释放点 = 本函数返回点：无论查询成功、失败还是超时，都必须在当前
    //    任务内**无条件** await PoolConnection::close()，彻底释放连接池槽位
    //    （其文档明确：关闭期间持续持有池 permit，池不会超出 max_connections——
    //    不是 detach），并**不为其设第三层超时**：提前放行许可等于重开多波累积
    //    后门。返回 Err 仅表示关闭握手未正常完成，套接字同样被丢弃、池槽位同样释放。
    let _ = conn.close().await;

    // close().await 执行完成后才返回采集结果；持有 OwnedPermit 的独立采集任务
    // 随本函数返回而退出，随后 OwnedPermit 随任务退出释放（I7）。
    scrape_outcome
}

/// 两条聚合在**同一条池连接**上顺序执行（§5.3 约束 1），行级映射即 §5.2.2 的字节精确
/// 映射；任一未知取值立即返回去敏错误（整次抓取 503，不输出"部分成功 + 缺失族记 0"）。
///
/// 语义边界（§5.3 约束 4）：同一连接不保证跨语句快照，两族读发生在两个时间点，毫秒级
/// 偏差使两条聚合之和非强一致——显式建模为"近似瞬时 gauge"，对账走 SQL，MUST NOT 为此
/// 引入事务（会长期占用共享池槽位）。
async fn run_aggregates(
    conn: &mut PoolConnection<MySqlConnection>,
) -> Result<MetricsSnapshot, ControllerError> {
    let mut devices = DeviceCounts::default();
    for row in sqlx::query(DEVICE_AGGREGATE_SQL)
        .fetch_all(&mut *conn)
        .await?
    {
        let bucket: Vec<u8> = row.try_get("bucket")?;
        let count: i64 = row.try_get("n")?;
        let count = checked_count(count)?;
        match admission_label(&bucket) {
            None => return Err(unknown_admission_state_error()),
            Some(bucket) => match bucket {
                AdmissionBucket::Approved => devices.approved += count,
                AdmissionBucket::Pending => devices.pending += count,
                AdmissionBucket::Rejected => devices.rejected += count,
            },
        }
    }

    let mut tasks = TaskCounts::default();
    for row in sqlx::query(TASK_AGGREGATE_SQL)
        .fetch_all(&mut *conn)
        .await?
    {
        let state: Vec<u8> = row.try_get("state")?;
        let outcome: Option<Vec<u8>> = row.try_get("outcome")?;
        let count: i64 = row.try_get("n")?;
        let count = checked_count(count)?;
        match task_bucket(&state, outcome.as_deref()) {
            None => return Err(task_aggregate_error(&state)),
            Some(bucket) => match bucket {
                TaskBucket::Pending => tasks.pending += count,
                TaskBucket::Running => tasks.running += count,
                TaskBucket::Succeeded => tasks.succeeded += count,
                TaskBucket::Failed => tasks.failed += count,
            },
        }
    }

    Ok(MetricsSnapshot { devices, tasks })
}

/// `COUNT(*)` 恒非负（无 `GROUP BY ... WITH ROLLUP`），负值只能是契约外数据 → fail-closed。
fn checked_count(count: i64) -> Result<u64, ControllerError> {
    u64::try_from(count).map_err(|_| ControllerError::Config("metrics aggregate count overflow".into()))
}

// ---------------------------------------------------------------------------
// 测试替身（§7.5 的三种行为；仅测试构建存在，不进生产路径）
// ---------------------------------------------------------------------------

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

/// 仅测试构建可见：L2 测试直接 `Arc::new(FakeMetricsSnapshotSource::…())`。
/// 计数是**实例级**原子量（每个测试各建一个 fake，因此并行测试互不干扰），
/// 语义为"已完成的采集次数"——挂起变体永不产出结果，也就绝不计数。
#[cfg(test)]
#[derive(Clone)]
pub struct FakeMetricsSnapshotSource {
    behavior: Arc<FakeBehavior>,
    calls: Arc<AtomicUsize>,
}

#[cfg(test)]
enum FakeBehavior {
    Fixed(MetricsSnapshot),
    Fails(&'static str),
    Hanging,
}

#[cfg(test)]
impl FakeMetricsSnapshotSource {
    pub fn fixed(snapshot: MetricsSnapshot) -> Self {
        Self::with(FakeBehavior::Fixed(snapshot))
    }
    pub fn failing(message: &'static str) -> Self {
        Self::with(FakeBehavior::Fails(message))
    }
    /// 永久挂起（`std::future::pending`）：用于并发闸占用与 handler 取消语义。
    pub fn hanging() -> Self {
        Self::with(FakeBehavior::Hanging)
    }
    /// 已完成的采集次数。
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn with(behavior: FakeBehavior) -> Self {
        Self {
            behavior: Arc::new(behavior),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[cfg(test)]
impl MetricsSnapshotSource for FakeMetricsSnapshotSource {
    fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
        Box::pin(async move {
            let outcome = match self.behavior.as_ref() {
                FakeBehavior::Fixed(snapshot) => Ok(*snapshot),
                FakeBehavior::Fails(message) => {
                    Err(ControllerError::Config((*message).to_string()))
                }
                FakeBehavior::Hanging => std::future::pending().await,
            };
            self.calls.fetch_add(1, Ordering::SeqCst);
            outcome
        })
    }
}

#[cfg(test)]
impl std::fmt::Debug for FakeMetricsSnapshotSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeMetricsSnapshotSource")
            .field("calls", &self.calls())
            .finish()
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo fmt --package rsetup-controller
cargo test -p rsetup-controller --lib metrics::collect
```

预期（GREEN）：

```text
test metrics::collect::tests::uptime_never_regresses_across_wall_clock_jumps ... ok
test metrics::collect::tests::aggregate_sql_is_byte_level_grouping ... ok
test metrics::collect::tests::snapshot_kernel_has_no_early_exit_between_acquire_and_close ... ok
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

判据：`0 failed`。三条 F1/R1 回归网（`snapshot_kernel_has_no_early_exit_between_acquire_and_close`、`aggregate_sql_is_byte_level_grouping`、`guard_constants_match_spec`）必须都在输出中。

```bash
cargo clippy --package rsetup-controller --all-targets -- -D warnings
grep -nE 'SCRAPE_TIMEOUT.*\?' crates/rsetup-controller/src/metrics/collect.rs
```

第二条 grep（§7.8 第 9 条）**无输出**（本文件注释里出现的 `?` 都在注释里，grep 只看同一行是否有 `SCRAPE_TIMEOUT.*?` 的**代码**形态；若命中，说明把 `SCRAPE_TIMEOUT` 与 `?` 写到了同一行）。

> 若 `snapshot_kernel_has_no_early_exit_between_acquire_and_close` 因第 1 步的两个 `map_err(...)?` 报错：那是断言范围写错，不是实现错——早退禁令的区间是"连接借出成功之后到 `close().await`"，第 1 步的两个 `?` 发生在**没有连接产生**时（许可可立即释放），必须保留。

- [ ] **Step 5: Commit**

```bash
git add crates/rsetup-controller/src/metrics/mod.rs crates/rsetup-controller/src/metrics/collect.rs
git commit -m "feat(controller): 增加指标采集内核与连接生命周期许可绑定"
```
---

## Task 5: HTTP handler 与 Axum 路由（`metrics/http.rs`，含 HEAD 短路）

**Files:**
- Create: `crates/rsetup-controller/src/metrics/http.rs`
- Modify: `crates/rsetup-controller/src/metrics/mod.rs`（`pub mod http;` + 再导出）
- Test: 同文件 `#[cfg(test)]`（L2，进程内 `oneshot`，规范 §7.5 全 19 行）

**Interfaces:**
- Consumes：`metrics::config::{BasicCredentials, MetricsConfig, DEFAULT_ENDPOINT}`（Task 1）、`metrics::auth::{parse_authorization, verify, www_authenticate}`（Task 2）、`metrics::collect::{FakeMetricsSnapshotSource, MetricsRuntime, MetricsSnapshotSource, MAX_CONCURRENT_SCRAPE, SqlxMetricsSnapshotSource, pool_counts}`（Task 4）、`metrics::render::{render, CONTENT_TYPE}`（Task 3）、`crate::DbPool`、`crate::ControllerError`。
- Produces（Task 6 依赖的精确签名，规范 §6.2）：
  - `metrics::http::MetricsState { config: Arc<MetricsConfig>, runtime: Arc<MetricsRuntime>, snapshot: Arc<dyn MetricsSnapshotSource>, pool: DbPool, guard: Arc<tokio::sync::Semaphore> }`（`Clone`，字段全 `pub`）
  - `metrics::http::build_metrics_router(state: MetricsState) -> Router<()>`
  - 私有 `async fn scrape(State<MetricsState>, Method, HeaderMap) -> Response`

- [ ] **Step 1: Write the failing test**

`crates/rsetup-controller/src/metrics/http.rs`（仅测试模块）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::collect::{
        FakeMetricsSnapshotSource, MetricsSnapshot, SqlxMetricsSnapshotSource,
        MAX_CONCURRENT_SCRAPE,
    };
    use crate::metrics::config::DEFAULT_ENDPOINT;
    use crate::metrics::model::{DeviceCounts, TaskCounts};
    use crate::metrics::render::CONTENT_TYPE;
    use crate::DbPool;
    use axum::body::{to_bytes, Body};
    use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
    use std::time::{Duration, Instant};
    use tower::ServiceExt;

    const USER: &str = "prom_admin";
    const PASSWORD: &str = "secure_password";

    fn config_with(endpoint: &str, enabled: bool, password: Option<&str>) -> Arc<MetricsConfig> {
        Arc::new(MetricsConfig {
            enabled,
            endpoint: endpoint.to_string(),
            credentials: password.map(|password| BasicCredentials {
                username: USER.to_string(),
                password: password.to_string(),
            }),
        })
    }

    /// §5.6 的 fixture 计数（uptime 由 runtime 锚点决定，因此响应体用**同锚点**重渲染
    /// 后逐字节比对；固定 uptime 的逐字节 golden 由 Task 3 的 L1 用例钉住）。
    fn fixture_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
        }
    }

    struct Harness {
        router: Router<()>,
        runtime: Arc<MetricsRuntime>,
        source: FakeMetricsSnapshotSource,
        pool: DbPool,
        guard: Arc<Semaphore>,
    }

    impl Harness {
        fn enabled() -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn with_password(password: &str) -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(password)),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn disabled() -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, false, None),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn failing(message: &'static str) -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                FakeMetricsSnapshotSource::failing(message),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        fn with_capacity(capacity: usize, source: FakeMetricsSnapshotSource) -> Self {
            Self::with(
                config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                source,
                capacity,
            )
        }
        fn with(
            config: Arc<MetricsConfig>,
            source: FakeMetricsSnapshotSource,
            capacity: usize,
        ) -> Self {
            let runtime = Arc::new(MetricsRuntime::new());
            let pool = lazy_pool();
            let guard = Arc::new(Semaphore::new(capacity));
            let router = build_metrics_router(MetricsState {
                config,
                runtime: Arc::clone(&runtime),
                snapshot: Arc::new(source.clone()),
                pool: pool.clone(),
                guard: Arc::clone(&guard),
            });
            Self {
                router,
                runtime,
                source,
                pool,
                guard,
            }
        }
        /// 自定义端点（证明 §4.5 的 404/405 形状只跟配置路径有关）。
        fn at_endpoint(endpoint: &str) -> Self {
            Self::with(
                config_with(endpoint, true, Some(PASSWORD)),
                FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                MAX_CONCURRENT_SCRAPE,
            )
        }
        /// 用外部汇总源替换（`source_timeout_maps_to_503` 走真实源）。
        fn with_source(source: Arc<dyn MetricsSnapshotSource>) -> Self {
            let runtime = Arc::new(MetricsRuntime::new());
            let pool = lazy_pool();
            let guard = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPE));
            let router = build_metrics_router(MetricsState {
                config: config_with(DEFAULT_ENDPOINT, true, Some(PASSWORD)),
                runtime: Arc::clone(&runtime),
                snapshot: source,
                pool: pool.clone(),
                guard: Arc::clone(&guard),
            });
            Self {
                router,
                runtime,
                source: FakeMetricsSnapshotSource::fixed(fixture_snapshot()),
                pool,
                guard,
            }
        }
        async fn send(&self, method: Method, path: &str, headers: HeaderMap) -> Response {
            let mut builder = Request::builder().method(method).uri(path);
            for (name, value) in &headers {
                builder = builder.header(name, value);
            }
            self.router
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .expect("router 不得 panic 或拒绝请求")
        }
        async fn get(&self, path: &str, headers: HeaderMap) -> Response {
            self.send(Method::GET, path, headers).await
        }
        async fn get_ok(&self) -> Response {
            self.get(DEFAULT_ENDPOINT, authorized(USER, PASSWORD)).await
        }
    }

    fn authorized(user: &str, password: &str) -> HeaderMap {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let mut headers = HeaderMap::new();
        let token = STANDARD.encode(format!("{user}:{password}"));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Basic {token}")).unwrap(),
        );
        headers
    }

    fn raw_authorization(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, HeaderValue::from_str(value).unwrap());
        headers
    }

    async fn body_of(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 262_144).await.unwrap()
    }

    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers()
            .get(HeaderName::from_static(name))
            .map(|value| value.to_str().expect("ASCII 响应头"))
    }

    fn lazy_pool() -> DbPool {
        DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(Duration::from_millis(100))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .expect("lazy 池构造不连接"),
        )
    }

    /// 断言响应只带 §5.1 第 8 条允许的三个响应头（200 与 HEAD 形状）。
    fn assert_success_headers(response: &Response) {
        assert_eq!(header(response, "content-type"), Some(CONTENT_TYPE));
        assert_eq!(header(response, "cache-control"), Some("no-store"));
        assert_eq!(
            header(response, "x-content-type-options"),
            Some("nosniff"),
            "§5.1 第 8 条"
        );
        assert!(header(response, "allow").is_none(), "200 绝不携带 Allow（§6.2）");
        assert!(header(response, "set-cookie").is_none(), "I3：不写会话 Cookie");
        assert!(header(response, "etag").is_none() && header(response, "vary").is_none());
    }

    // ---------------------------------------------------------------------
    // 200
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn scrape_with_valid_credentials_returns_200() {
        let harness = Harness::enabled();
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_success_headers(&response);
        let text = String::from_utf8(body_of(response).await).expect("UTF-8 文本");
        let expected = render(
            &harness
                .runtime
                .scrape_values(&fixture_snapshot(), pool_counts(&harness.pool)),
        )
        .expect("fixture 必须可渲染");
        assert_eq!(text, expected, "响应体必须与渲染器输出逐字节一致");
        assert!(text.starts_with("# HELP rsetup_controller_build_info "));
        assert!(text.contains("rsetup_controller_devices_total{admission=\"APPROVED\"} 7"));
        assert!(text.contains("rsetup_controller_tasks_total{status=\"succeeded\"} 40"));
        assert!(text.ends_with('\n'));
        assert_eq!(harness.source.calls(), 1);
    }

    /// P1 的端到端回归网：RFC 7617 合法密码不得被解析层或配置层拒绝。
    #[tokio::test]
    async fn scrape_with_password_containing_colons_returns_200() {
        let harness = Harness::with_password("a:b:c");
        let response = harness
            .get(DEFAULT_ENDPOINT, authorized(USER, "a:b:c"))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(header(&response, "content-type"), Some(CONTENT_TYPE));
        assert_eq!(harness.source.calls(), 1);
    }

    // ---------------------------------------------------------------------
    // HEAD 短路
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn head_with_valid_credentials_returns_200_without_collection() {
        let harness = Harness::enabled();
        let response = harness
            .send(Method::HEAD, DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_success_headers(&response);
        assert!(body_of(response).await.is_empty(), "HEAD 响应体必须为空");
        assert_eq!(harness.source.calls(), 0, "HEAD 绝不调用汇总源（§6.2）");
        assert_eq!(
            harness.guard.available_permits(),
            MAX_CONCURRENT_SCRAPE,
            "HEAD 不占并发闸"
        );
    }

    #[tokio::test]
    async fn head_without_credentials_returns_401_like_get() {
        let harness = Harness::enabled();
        let head = harness
            .send(Method::HEAD, DEFAULT_ENDPOINT, HeaderMap::new())
            .await;
        let get = harness.get(DEFAULT_ENDPOINT, HeaderMap::new()).await;
        assert_eq!(head.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(head.status(), get.status());
        assert_eq!(
            header(&head, "www-authenticate"),
            header(&get, "www-authenticate"),
            "HEAD 与 GET 的挑战值逐字节相同"
        );
        assert!(body_of(head).await.is_empty());
        assert_eq!(harness.source.calls(), 0, "HEAD 绝不绕过认证");
    }

    #[tokio::test]
    async fn head_when_disabled_returns_404() {
        let harness = Harness::disabled();
        for headers in [HeaderMap::new(), authorized(USER, PASSWORD)] {
            let response = harness.send(Method::HEAD, DEFAULT_ENDPOINT, headers).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(header(&response, "www-authenticate").is_none());
            assert!(header(&response, "content-type").is_none());
            assert!(body_of(response).await.is_empty());
        }
        assert_eq!(harness.source.calls(), 0);
    }

    // ---------------------------------------------------------------------
    // 401
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn scrape_without_credentials_returns_401() {
        let harness = Harness::enabled();
        let response = harness.get(DEFAULT_ENDPOINT, HeaderMap::new()).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            header(&response, "www-authenticate"),
            Some("Basic realm=\"rsetup-controller-metrics\"")
        );
        assert_eq!(header(&response, "cache-control"), Some("no-store"));
        assert_eq!(
            header(&response, "x-content-type-options"),
            Some("nosniff")
        );
        assert!(
            header(&response, "content-type").is_none(),
            "401 不得带 Content-Type（§4.4）"
        );
        assert!(body_of(response).await.is_empty());
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn scrape_with_wrong_password_returns_401() {
        let harness = Harness::enabled();
        let response = harness
            .get(DEFAULT_ENDPOINT, authorized(USER, "wrong_password"))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn scrape_with_wrong_username_returns_401() {
        let harness = Harness::enabled();
        let response = harness
            .get(DEFAULT_ENDPOINT, authorized("wrong_user", PASSWORD))
            .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn scrape_with_malformed_header_returns_401() {
        let harness = Harness::enabled();
        let mut two_headers = authorized(USER, PASSWORD);
        two_headers.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Basic dXNlcjpwd3dvZA=="),
        );
        let cases = [
            raw_authorization("Bearer abc"),
            raw_authorization("Basic nothex!!"),
            raw_authorization("Basic dXNlcg"),
            raw_authorization("Basic cHJvbV9hZG1pbjo="),
            raw_authorization("OnBzc2R3ZA=="),
            two_headers,
        ];
        for (index, headers) in cases.into_iter().enumerate() {
            let response = harness.get(DEFAULT_ENDPOINT, headers).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "第 {index} 个畸形头必须 401"
            );
            assert_eq!(
                header(&response, "www-authenticate"),
                Some("Basic realm=\"rsetup-controller-metrics\"")
            );
        }
        assert_eq!(harness.source.calls(), 0);
    }

    // ---------------------------------------------------------------------
    // 404 / 405
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn disabled_endpoint_returns_404_regardless_of_credentials() {
        let harness = Harness::disabled();
        for headers in [HeaderMap::new(), authorized(USER, PASSWORD)] {
            let response = harness.get(DEFAULT_ENDPOINT, headers).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
            assert!(
                header(&response, "www-authenticate").is_none(),
                "禁用态不宣告端点存在（§4.5）"
            );
            assert!(header(&response, "content-type").is_none());
            assert!(header(&response, "cache-control").is_none());
            assert!(body_of(response).await.is_empty());
        }
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn other_methods_are_rejected_with_allow_header() {
        let harness = Harness::enabled();
        for method in [
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ] {
            let response = harness
                .send(method.clone(), DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
                .await;
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{method} 必须 405"
            );
            assert_eq!(
                header(&response, "allow"),
                Some("GET,HEAD"),
                "Axum 0.8.9 逐字节事实：append_allow_header 以 `,` 连接且无空格"
            );
            assert!(header(&response, "content-type").is_none());
        }
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn other_paths_are_not_registered_by_metrics_line() {
        let harness = Harness::enabled();
        for path in ["/", "/healthz", "/metrics.txt", "/metrics/deep"] {
            let response = harness.get(path, authorized(USER, PASSWORD)).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path} 不得由本线应答");
        }
        let custom = Harness::at_endpoint("/internal/metrics");
        assert_eq!(
            custom
                .get("/internal/metrics", authorized(USER, PASSWORD))
                .await
                .status(),
            StatusCode::OK,
            "配置路径必须生效"
        );
        assert_eq!(
            custom
                .send(Method::POST, "/internal/metrics", HeaderMap::new())
                .await
                .status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            custom
                .get(DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
                .await
                .status(),
            StatusCode::NOT_FOUND,
            "配置路径之外的 /metrics 必须由其他条线或默认响应处置"
        );
    }

    // ---------------------------------------------------------------------
    // 会话边界隔离（§4.1）
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn metrics_ignores_session_cookie() {
        let harness = Harness::enabled();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("rsc_session=valid-looking-session-value"),
        );
        let response = harness.get(DEFAULT_ENDPOINT, headers).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn metrics_success_does_not_create_session() {
        let harness = Harness::enabled();
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            header(&response, "set-cookie").is_none(),
            "MUST NOT 写会话 Cookie（I3）"
        );
        // 结构证明：实现部分（`#[cfg(test)]` 之前）根本不出现会话/CSRF/限速/审计
        // 边界类型，也不写日志——本线是独立认证边界（§4.1）。
        let source = include_str!("http.rs");
        let implementation = source
            .split("#[cfg(test)]")
            .next()
            .expect("本文件必须有实现部分");
        let code = code_only(implementation);
        for forbidden in [
            "HttpAuthState",
            "Session",
            "EndpointId",
            "Csrf",
            "RateLimit",
            "audit",
            "axum_extra",
            "tracing",
        ] {
            assert!(
                !code.contains(forbidden),
                "指标线实现不得引用会话/CSRF/限速/审计边界或写日志：{forbidden}"
            );
        }
    }

    // ---------------------------------------------------------------------
    // 503
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn source_error_maps_to_503_not_partial_zero() {
        let harness = Harness::failing("metrics fixture failure");
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header(&response, "retry-after"), Some("5"));
        assert_eq!(header(&response, "cache-control"), Some("no-store"));
        assert!(header(&response, "content-type").is_none(), "503 不得带 Content-Type");
        let body = body_of(response).await;
        assert!(body.is_empty(), "503 必须空体，绝不输出部分指标（G4）");
        assert_eq!(harness.source.calls(), 1);
    }

    /// 真实源路径的失败/超时映射：lazy 池指向不可达端口，采集必须在
    /// `POOL_ACQUIRE_TIMEOUT`（500ms）量级内失败并映射 503，MUST NOT 排队 30 秒。
    #[tokio::test]
    async fn source_timeout_maps_to_503() {
        let source = Arc::new(SqlxMetricsSnapshotSource::new(lazy_pool()));
        let harness = Harness::with_source(source);
        let started = Instant::now();
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            harness.get(DEFAULT_ENDPOINT, authorized(USER, PASSWORD)),
        )
        .await
        .expect("必须在 3 秒内定性（不得退化为 30 秒 acquire 排队）");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header(&response, "retry-after"), Some("5"));
        assert!(
            started.elapsed() < Duration::from_millis(1_500),
            "真实源失败必须在 POOL_ACQUIRE_TIMEOUT 量级内返回，实测 {:?}",
            started.elapsed()
        );
    }

    // ---------------------------------------------------------------------
    // 并发闸与许可生命周期（I7 的进程内可证部分）
    // ---------------------------------------------------------------------

    #[tokio::test]
    async fn scrape_concurrency_cap_is_enforced() {
        let harness =
            Harness::with_capacity(1, FakeMetricsSnapshotSource::fixed(fixture_snapshot()));
        let permits: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| Arc::clone(&harness.guard).try_acquire_owned().expect("预占许可"))
            .collect();
        assert_eq!(harness.guard.available_permits(), 0);
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(header(&response, "retry-after"), Some("5"));
        assert_eq!(harness.source.calls(), 0, "闸满时 MUST NOT 进入采集");
        // HEAD 不占闸：闸满仍 200（§6.2）。
        let head = harness
            .send(Method::HEAD, DEFAULT_ENDPOINT, authorized(USER, PASSWORD))
            .await;
        assert_eq!(head.status(), StatusCode::OK);
        // 归还许可后 GET 立即可用。
        drop(permits);
        assert_eq!(harness.guard.available_permits(), MAX_CONCURRENT_SCRAPE);
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(harness.source.calls(), 1);
    }

    /// handler 侧许可绑定：许可由 `tokio::spawn` 的采集任务持有，**客户端断线
    /// （handler 被取消）既不取消采集任务，也不提前释放许可**（§5.5.1 规则 3）。
    /// 这是 I7 在进程内可指认的那一半；"close 完成前 permit 绝不回落"的确定性
    /// 证明由 Task 7 §7.7 第 4 步的受控关闭屏障完成。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn permit_stays_held_after_client_disconnects() {
        let harness = Harness::with_capacity(1, FakeMetricsSnapshotSource::hanging());
        let handler = {
            let router = harness.router.clone();
            tokio::spawn(async move {
                let _ = router
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(Method::GET)
                            .uri(DEFAULT_ENDPOINT)
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await;
            })
        };
        // 等采集任务真正进入挂起态（许可已从闸里取走）。
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            harness.guard.available_permits(),
            0,
            "采集任务在途期间许可不得回落"
        );
        handler.abort();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            harness.guard.available_permits(),
            0,
            "客户端断线不得提前释放许可（§5.5.1 规则 3）"
        );
        // 采集任务仍在持有许可：第二条请求立即 503（闸满），且未进入采集。
        let response = harness.get_ok().await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(harness.source.calls(), 0);
    }

    #[tokio::test]
    async fn credentials_are_not_echoed_in_error_paths() {
        let harness = Harness::enabled();
        let responses = [
            harness
                .get(DEFAULT_ENDPOINT, authorized("prom_admin", "supersensitive-secret"))
                .await,
            harness
                .get(DEFAULT_ENDPOINT, raw_authorization("Basic bm90LWEtY29sb24"))
                .await,
            harness.get(DEFAULT_ENDPOINT, HeaderMap::new()).await,
        ];
        for response in responses {
            let mut text = format!("{:?}", response.headers());
            text.push_str(&String::from_utf8_lossy(&body_of(response).await));
            for marker in ["supersensitive-secret", "prom_admin", "c3VwZXJzZW5zaXRpdmU"] {
                assert!(!text.contains(marker), "错误响应回显了 {marker:?}：{text}");
            }
        }
    }

    /// 去掉行注释与文档注释后的纯代码文本（结构门禁看代码，不看注释）。
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p rsetup-controller --lib metrics::http
```

预期：**编译失败（RED）**：

```text
error[E0432]: unresolved imports `super::MetricsState`, `super::build_metrics_router`
error[E0425]: cannot find function `scrape` in this scope
error: could not compile `rsetup-controller` (lib test) due to N previous errors
```

不得出现 `test result: ok`。

- [ ] **Step 3: Write minimal implementation**

`crates/rsetup-controller/src/metrics/http.rs` 的实现部分（写在 Step 1 的测试模块之前）：

```rust
//! 指标线路由与 handler（规范 §6.2/§4.5）。
//!
//! handler 内部顺序 MUST 固定为：①`enabled` 门禁 → ②Basic 认证 → ③`HEAD` 短路
//! （200 空体，不取闸、不采集）→ ④并发闸 `try_acquire_owned`（`OwnedPermit` 移交
//! 采集任务持有，直到连接完全关闭才释放，§5.5.1）→ ⑤快照采集 → ⑥渲染与上限校验
//! → ⑦响应。HTTP 方法匹配由路由层在此之前完成；`HEAD` 之外没有第二种方法能进入本
//! handler（`MethodRouter::get` 把 `["GET", "HEAD"]` 指向同一 handler，且成功的 200
//! 响应不携带 `Allow`——`Allow` 只由 405 fallback 挂载）。
//!
//! 本模块 MUST NOT 写日志（含 401）：抓取的正常节奏会淹没日志，失败已可由 Prometheus
//! 的 `up == 0` 与 `scrape_duration_seconds` 观测（§6.2 第 6 条）。

use std::sync::Arc;

use axum::extract::State;
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::sync::{oneshot, Semaphore};

use crate::metrics::auth::{parse_authorization, verify, www_authenticate};
use crate::metrics::collect::{pool_counts, MetricsRuntime, MetricsSnapshotSource};
use crate::metrics::config::{BasicCredentials, MetricsConfig};
use crate::metrics::render::{render, CONTENT_TYPE};
use crate::DbPool;

/// 指标线状态。**没有**认证上下文、会话边界类型与审计/限速状态（I3），因此这里在
/// 类型上就不可能触达会话边界；`pool` 只用于 §5.3 第 5 条的内存读取（`size()` /
/// `num_idle()`），MUST NOT 用它发起查询。
#[derive(Clone)]
pub struct MetricsState {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
    pub snapshot: Arc<dyn MetricsSnapshotSource>,
    pub pool: DbPool,
    pub guard: Arc<Semaphore>,
}

/// 指标线子 router：按配置路径注册恰好 1 条 GET 路由（Axum 0.8 隐含同时接受 HEAD）。
///
/// MUST 始终注册（含 `enabled = false` 时），由 handler 依 §4.5 顺序返回 404：这样
/// 禁用态响应与路径配置项完全无关，也不受后续合并顺序或 SPA 白名单演变影响。
/// MUST NOT 用独立的 `.head(handler)` 注册第二条 handler——单 handler + `Method`
/// 分支才能保证两个方法的认证与门禁不会各自漂移。
pub fn build_metrics_router(state: MetricsState) -> Router<()> {
    let endpoint = state.config.endpoint.clone();
    Router::new()
        .route(&endpoint, get(scrape))
        .with_state(state)
}

/// 401：固定挑战值 + 空体 + 无 `Content-Type`（MUST NOT 复用业务 JSON 信封，§4.4）。
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            (WWW_AUTHENTICATE, www_authenticate()),
            ("cache-control", "no-store".to_string()),
            ("x-content-type-options", "nosniff".to_string()),
        ],
    )
        .into_response()
}

/// 采集失败（超时、映射未知取值、渲染超上限、闸满）→ 503 空体 + `Retry-After: 5`；
/// 不记日志、不回显错误内容（§4.5）。
fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("retry-after", "5"),
        ],
    )
        .into_response()
}

async fn scrape(
    State(state): State<MetricsState>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    // ① enabled 门禁：禁用态恒 404 且与"路径不存在"不可区分（无 WWW-Authenticate、
    //    无 Content-Type、无 Cache-Control），无论是否携带凭据。
    if !state.config.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    // ② Basic 认证：解析失败或不匹配一律同一形状 401（不区分原因，不写日志）。
    //    配置门禁保证启用态必有凭据（§3.6 分支 5）；缺失只能是装配错误 → fail-closed。
    let Some(credentials) = state.config.credentials.as_ref() else {
        return unavailable();
    };
    let Some(parsed) = parse_authorization(&headers) else {
        return unauthorized();
    };
    if !verify(&parsed.username, &parsed.password, credentials) {
        return unauthorized();
    }
    // ③ HEAD 短路：认证通过后 200 空体，不取并发闸、不采集（HEAD 的用途是"探活 +
    //    检查认证配置"，为一个被丢弃的 body 支付两次 GROUP BY 是纯粹的可用性风险）。
    if method == Method::HEAD {
        return (
            StatusCode::OK,
            [
                ("content-type", CONTENT_TYPE),
                ("cache-control", "no-store"),
                ("x-content-type-options", "nosniff"),
            ],
        )
            .into_response();
    }
    // ④ 并发闸：闸满立即 503（不排队、不重试），MUST NOT 进入采集。
    let Ok(permit) = state.guard.clone().try_acquire_owned() else {
        return unavailable();
    };
    // ⑤ 采集：许可与采集调用一起移交独立任务，用 oneshot 等结果。许可只在任务返回
    //    （= 连接已完全关闭）时随 Drop 释放；客户端断线只取消本 handler，MUST NOT
    //    取消采集任务、MUST NOT 提前释放许可（§5.5.1 规则 3）。
    let (tx, rx) = oneshot::channel();
    let source = Arc::clone(&state.snapshot);
    tokio::spawn(async move {
        let _permit = permit; // 任务结束（连接已完全关闭）才随 Drop 释放（I7）。
        let _ = tx.send(source.snapshot().await); // handler 已断线时发送失败，取值直接丢弃。
    });
    let Ok(Ok(snapshot)) = rx.await else {
        return unavailable();
    };
    // ⑥ 渲染与上限校验（渲染层内部 fail-closed，这里只映射 503）。
    let values = state
        .runtime
        .scrape_values(&snapshot, pool_counts(&state.pool));
    match render(&values) {
        Ok(body) => (
            StatusCode::OK,
            [
                ("content-type", CONTENT_TYPE),
                ("cache-control", "no-store"),
                ("x-content-type-options", "nosniff"),
            ],
            body,
        )
            .into_response(),
        Err(_) => unavailable(),
    }
}
```

`crates/rsetup-controller/src/metrics/mod.rs` 追加 `pub mod http;`（置于 `pub mod render;` 之前保持字母序）与再导出：

```rust
pub use http::{build_metrics_router, MetricsState};
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo fmt --package rsetup-controller
cargo test -p rsetup-controller --lib metrics::http
```

预期（GREEN）：

```text
test metrics::http::tests::scrape_with_valid_credentials_returns_200 ... ok
test metrics::http::tests::head_with_valid_credentials_returns_200_without_collection ... ok
test metrics::http::tests::other_methods_are_rejected_with_allow_header ... ok
test metrics::http::tests::permit_stays_held_after_client_disconnects ... ok
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; N filtered out
```

判据：`0 failed`，且 §7.5 表中 19 个测试名逐个出现在输出中（`other_methods_are_rejected_with_allow_header` 断言的是**无空格**的 `GET,HEAD`；写成 `GET, HEAD` 必红）。

```bash
cargo clippy --package rsetup-controller --all-targets -- -D warnings
cargo test -p rsetup-controller --lib
```

> 与规范 §7.5 的三条已登记偏差（执行时不得改回，交付报告须复述）：
> 1. **`source_timeout_maps_to_503` 改为"真实源 + 不可达 lazy 池"**。规范原文写"source 挂起 > `SCRAPE_TIMEOUT`（测试用 `tokio::time::pause`/缩短常量注入）"，而 §5.5.1 规则 2 明确 **MUST NOT** 在 handler 侧为采集设第三层会提前放行许可的超时——`SCRAPE_TIMEOUT` 只存在于 `snapshot_via` 内部包裹两条聚合的位置。因此"挂起的 fake source → 503"在 L2 不可测（handler 会一直等），本用例改为证明真实源路径的**超时上界与 503 映射**（`POOL_ACQUIRE_TIMEOUT` 生效、不退化为 30 秒排队）；查询级超时（`metrics snapshot: query timeout`）由 Task 7 的 L4 用例在真实库上验收。
> 2. **`metrics_success_does_not_create_session` 的"零变化"用结构门禁 + 无 `Set-Cookie` 证明**：`MetricsState` 与 handler 源码（`#[cfg(test)]` 之前的实现部分）不含 `HttpAuthState`/`Session`/`EndpointId`/`Csrf`/`RateLimit`/`audit`/`axum_extra`/日志调用；跨线行为（抓取不污染会话）由 Task 6 的 `metrics_line_never_touches_session_boundary` 在顶层 router 上证明。
> 3. **新增 `permit_stays_held_after_client_disconnects`**（规范表未列，属 I7 的进程内可指认部分）：`multi_thread` runtime + 两处 80ms 观察窗口，只断言"许可不回落"，不声称证明 `close()` 语义（那是 Task 7 §7.7 第 4 步受控关闭屏障的职责）。

- [ ] **Step 5: Commit**

```bash
git add crates/rsetup-controller/src/metrics/mod.rs crates/rsetup-controller/src/metrics/http.rs
git commit -m "feat(controller): 增加指标端点处理链与 HEAD 短路门禁"
```
---

## Task 6: 生产六线装配与 `main.rs`（`production.rs` + `main.rs` + L3）

**Files:**
- Modify: `crates/rsetup-controller/src/production.rs`（`MetricsLineInputs`、6 参数 `compose_component_routers`、`build_router`、`build_production_router_with_metrics_source`）
- Modify: `crates/rsetup-controller/src/main.rs`（uptime 零点为 `main` 第一条语句、`MetricsConfig::from_env` 前置、`production_inputs` 签名扩展）
- Modify: `crates/rsetup-controller/src/web.rs`（`ASSET_DIR`/`SPA_EXACT_ROUTES`/`SPA_PARAM_ROUTES` 三个常量改为 `pub`，供 §7.6 第 8 条做漂移一致性断言）
- Modify: `crates/rsetup-controller/tests/production_composition.rs`（既有五线用例扩为六线 + §7.6 第 1–10 条）
- Test: `tests/production_composition.rs`（L3，无真库）

**Interfaces:**
- Consumes：`metrics::http::{MetricsState, build_metrics_router}`（Task 5）、`metrics::collect::{MAX_CONCURRENT_SCRAPE, MetricsRuntime, MetricsSnapshotSource, SqlxMetricsSnapshotSource}`（Task 4）、`metrics::config::MetricsConfig`（Task 1）、`AppState { db, auth }`、`crate::web_router`。
- Produces：
  - `production::MetricsLineInputs { config: Arc<MetricsConfig>, runtime: Arc<MetricsRuntime> }`（**只有这两个字段**，§7.6 第 9 条）
  - `production::ProductionInputs { auth, management, tasks, status, metrics, web }`（新增 `metrics`，`web` 仍居末）
  - `production::build_production_router(ProductionInputs) -> Result<Router<()>, ControllerError>`（签名不变）
  - `#[doc(hidden)] production::build_production_router_with_metrics_source(ProductionInputs, Arc<dyn MetricsSnapshotSource>) -> Result<Router<()>, ControllerError>`
  - `production::compose_component_routers(auth, management, tasks, status, metrics, web) -> Router<()>`

- [ ] **Step 1: Write the failing test**

`crates/rsetup-controller/tests/production_composition.rs`。**先改既有断言的 arity**（不改断言内容），再追加 §7.6 的 10 条新用例。

3 处既有调用点补第 5 个组件参数（`metrics` 恒在 `status` 之后、`web` 之前）：

```rust
fn composed() -> Router<()> {
    let auth = component("/healthz", "auth-health").merge(component("/readyz", "auth-ready"));
    compose_component_routers(
        auth,
        component("/api/v1/users", "management"),
        component("/api/v1/tasks", "tasks"),
        component("/api/v1/devices/test/status", "status"),
        component("/metrics", "metrics"),
        component("/", "web"),
    )
}
```

`every_component_is_reachable_without_duplicate_probes` 的断言表新增一行 `("/metrics", "metrics")`（六组件、六路径各自可达、marker 不串线、探活仍只有 `/healthz` 与 `/readyz` 各一次）；`duplicate_business_path_panics_instead_of_being_hidden_by_override` 同样补 `component("/metrics", "metrics")` 第 5 参数，其余不动。

`mod typed` 的 `inputs()` 改为**接收外部池**并新增 `metrics` 字段（原四参数版本保留为薄壳，供既有 3 条负例复用）：

```rust
    /// 指标线装配项的 fixture：`enabled` 与端点可按用例指定。
    fn metrics_inputs(enabled: bool) -> MetricsLineInputs {
        MetricsLineInputs {
            config: Arc::new(MetricsConfig {
                enabled,
                endpoint: "/metrics".to_string(),
                credentials: enabled.then(|| BasicCredentials {
                    username: "prom_admin".to_string(),
                    password: "metrics-composition-fixture".to_string(),
                }),
            }),
            runtime: Arc::new(MetricsRuntime::new()),
        }
    }

    /// 用**指定**池装配六线输入（§7.6 第 6/10 条要观察同一个池的读数）。
    fn inputs_with_db(
        db: &DbPool,
        auth: Arc<HttpAuthState>,
        management_auth: Arc<HttpAuthState>,
        tasks_auth: Arc<HttpAuthState>,
        status_auth: Arc<HttpAuthState>,
        metrics: MetricsLineInputs,
    ) -> ProductionInputs {
        let transport = Arc::new(SyntheticSources);
        ProductionInputs {
            auth: AppState {
                db: db.clone(),
                auth,
            },
            management: build_sqlx_management_state(
                db.clone(),
                management_auth,
                transport.clone(),
                transport.clone(),
                transport,
            ),
            tasks: HttpTasksState {
                api: Arc::new(SqlxTaskApi::new(
                    Arc::new(TaskService::new(SqlxTaskRepository::new(db.0.clone()))),
                    Arc::new(SqlxTaskRepository::new(db.0.clone())),
                    Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone())),
                    SYNTHETIC_CURSOR_SECRET,
                )),
                auth: tasks_auth,
            },
            status: DeviceStatusState {
                auth: status_auth,
                snapshot: Arc::new(EmptyDeviceStatusSnapshotSource),
            },
            metrics,
            web: web_router(),
        }
    }

    fn inputs(
        auth: Arc<HttpAuthState>,
        management_auth: Arc<HttpAuthState>,
        tasks_auth: Arc<HttpAuthState>,
        status_auth: Arc<HttpAuthState>,
    ) -> ProductionInputs {
        let db = lazy_db();
        inputs_with_db(
            &db,
            auth,
            management_auth,
            tasks_auth,
            status_auth,
            metrics_inputs(false),
        )
    }
```

`mod typed` 顶部补 `use`：

```rust
    use axum::http::{HeaderMap, HeaderValue, header};
    use rsetup_controller::metrics::collect::{
        MAX_CONCURRENT_SCRAPE, MetricsFuture, MetricsRuntime, MetricsSnapshot,
        MetricsSnapshotSource,
    };
    use rsetup_controller::metrics::config::{BasicCredentials, MetricsConfig, RESERVED_PATHS};
    use rsetup_controller::metrics::model::{DeviceCounts, TaskCounts};
    use rsetup_controller::production::{
        MetricsLineInputs, build_production_router, build_production_router_with_metrics_source,
    };
    use rsetup_controller::web::{ASSET_DIR, SPA_EXACT_ROUTES, SPA_PARAM_ROUTES};
```

（`MetricsLineInputs` 一律从 `production` 模块取，`metrics` 模块不再导出它；`header` 与两个 `Header*` 类型是本文件此前没有的导入。）

追加 10 条用例（`mod typed` 内，接在 `composed_router_registers_status_route_behind_shared_gate` 之后）：

```rust
    /// 合成汇总源：两条聚合的取值由测试给定，池 gauge 仍走真实内存读数。
    struct SyntheticMetricsSnapshotSource;

    impl MetricsSnapshotSource for SyntheticMetricsSnapshotSource {
        fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
            Box::pin(async {
                Ok(MetricsSnapshot {
                    devices: DeviceCounts {
                        approved: 3,
                        pending: 2,
                        rejected: 1,
                    },
                    tasks: TaskCounts {
                        pending: 1,
                        running: 2,
                        succeeded: 4,
                        failed: 5,
                    },
                })
            })
        }
    }

    /// 永久挂起的合成源（§7.6 第 10 条）。
    struct HangingMetricsSnapshotSource;

    impl MetricsSnapshotSource for HangingMetricsSnapshotSource {
        fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
            Box::pin(std::future::pending())
        }
    }

    fn authorized_metrics_headers() -> HeaderMap {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!(
                "Basic {}",
                STANDARD.encode("prom_admin:metrics-composition-fixture")
            ))
            .unwrap(),
        );
        headers
    }

    async fn request_with_headers(
        router: &Router<()>,
        method: &str,
        uri: &str,
        headers: &HeaderMap,
    ) -> (StatusCode, Vec<u8>) {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let response = router
            .clone()
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 262_144).await.unwrap();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn shared_auth_context_composes_all_six_lines() {
        // 原五线断言全部保留，另加指标线：四件事同时成立才是单端口六线合并。
        let shared = auth_state();
        let db = lazy_db();
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &db,
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("同一 Arc<HttpAuthState> 必须装配成功");
        let (status, _) = request_get(&router, "/healthz").await;
        assert_eq!(status, StatusCode::OK, "/healthz");
        let (status, _) = request_get(&router, "/readyz").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "/readyz");
        let (status, body) = request_get(&router, "/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
        let (status, _) = request_with_headers(&router, "GET", "/metrics", &HeaderMap::new()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "/metrics 无凭据必须 401");
        let (status, body) = request_with_headers(
            &router,
            "GET",
            "/metrics",
            &authorized_metrics_headers(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "/metrics 带凭据必须 200");
        let text = String::from_utf8(body).expect("UTF-8 文本");
        assert!(text.contains("rsetup_controller_devices_total{admission=\"APPROVED\"} 3"));
        // 原五线的其余断言逐字保留（与改名前完全相同），此处只登记新增部分：
        for path in [
            "/api/v1/users",
            "/api/v1/roles",
            "/api/v1/audit",
            "/api/v1/system/status",
            "/api/v1/tasks",
            DEVICE_STATUS_PATH,
        ] {
            let (status, body) = request_get(&router, path).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "业务路径 {path} 必须 401");
            assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"), "{path}");
        }
        let (status, _) = request_get(&router, "/").await;
        assert_eq!(status, StatusCode::OK, "web 线 / 必须 200");
        let (status, _) = request_get(&router, "/api/v1/not-registered").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    #[should_panic(expected = "Overlapping method route")]
    fn metrics_path_colliding_with_reserved_route_panics_on_merge() {
        // 配置期拦截（§3.4）之外的第二道网：真把两条 /metrics 递进合并，仍必须 panic。
        let _router = compose_component_routers(
            component("/healthz", "auth-health"),
            component("/api/v1/users", "management"),
            component("/api/v1/tasks", "tasks"),
            component("/api/v1/devices/test/status", "status"),
            component("/metrics", "metrics"),
            component("/metrics", "duplicate-metrics"),
        );
    }

    #[tokio::test]
    async fn metrics_gauges_read_the_shared_pool_only() {
        // 两条聚合走合成源，池 gauge 仍是真实内存读数：lazy 池未建任何连接 → 全 0，
        // 且抓取前后 lazy 池都不得产生连接（证明读的是 auth.db 那个池且不发查询）。
        let shared = auth_state();
        let db = lazy_db();
        assert_eq!(db.0.size(), 0, "lazy 池构造不连接");
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &db,
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        let (status, body) = request_with_headers(
            &router,
            "GET",
            "/metrics",
            &authorized_metrics_headers(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let text = String::from_utf8(body).expect("UTF-8 文本");
        assert!(
            text.contains("rsetup_controller_db_pool_connections{state=\"active\"} 0"),
            "lazy 池无活动连接：{text}"
        );
        assert!(
            text.contains("rsetup_controller_db_pool_connections{state=\"idle\"} 0"),
            "lazy 池无空闲连接：{text}"
        );
        assert_eq!(db.0.size(), 0, "抓取前后 lazy 池都不得建立连接（指标线不发查询）");
    }

    #[tokio::test]
    async fn metrics_line_never_touches_session_boundary() {
        // 完整合法会话 Cookie 打 /metrics → 仍 401（会话不是指标线的凭据）；
        // 随后打 /api/v1/auth/me 仍按会话语义工作 → 抓取未污染会话/CSRF/限速。
        let shared = auth_state();
        let router = build_production_router_with_metrics_source(
            inputs(
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("rsc_session=composition-fixture-session"),
        );
        let (status, _) = request_with_headers(&router, "GET", "/metrics", &headers).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "会话 Cookie 不是指标线凭据");
        let (status, body) = request_get(&router, "/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "会话线路语义不变");
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
    }

    #[tokio::test]
    async fn reserved_paths_stay_registered() {
        // 保留集合内的路径必须仍由所属条线应答，不被指标线抢占或退化为 404。
        let shared = auth_state();
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &lazy_db(),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(SyntheticMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        for path in ["/healthz", "/readyz", "/", "/login", "/password", "/sessions"] {
            let (status, _) = request_get(&router, path).await;
            assert_ne!(status, StatusCode::NOT_FOUND, "保留路径 {path} 不得退化为 404");
        }
        let (status, _) = request_with_headers(
            &router,
            "GET",
            "/assets/index.js",
            &HeaderMap::new(),
        )
        .await;
        assert_ne!(status, StatusCode::NOT_FOUND, "资源前缀仍由 web 线应答");
        // 保留集合不与代码漂移：web.rs 的三张表逐条被 RESERVED_PATHS 覆盖。
        for path in SPA_EXACT_ROUTES {
            assert!(
                RESERVED_PATHS.contains(path),
                "SPA 精确路由 {path} 未登记进 RESERVED_PATHS"
            );
        }
        // ASSET_DIR 是 "/assets/"，登记形态是前缀段 "/assets"（§6.5）。
        let asset_prefix = ASSET_DIR.trim_end_matches('/');
        assert!(
            RESERVED_PATHS.contains(&asset_prefix),
            "资源目录前缀 {asset_prefix} 未登记（ASSET_DIR = {ASSET_DIR}）"
        );
        for template in SPA_PARAM_ROUTES {
            // "/devices/{id}" → 字面段 "/devices"（§6.5 的前缀段）。
            let literal = format!(
                "/{}",
                template.trim_start_matches('/').split('/').next().unwrap_or_default()
            );
            assert!(
                RESERVED_PATHS.contains(&literal.as_str()),
                "SPA 参数路由字面段 {literal} 未登记（模板 {template}）"
            );
        }
    }

    #[test]
    fn production_inputs_exposes_no_second_pool() {
        // 结构性断言：装配项只有 config 与 runtime，写全字段，杜绝 ..Default 兜底。
        let inputs = MetricsLineInputs {
            config: Arc::new(MetricsConfig {
                enabled: false,
                endpoint: "/metrics".to_string(),
                credentials: None,
            }),
            runtime: Arc::new(MetricsRuntime::new()),
        };
        // 下面两行是"字段集合恰为 {config, runtime}"的编译期证明：多一个字段就编译不过。
        let _config: Arc<MetricsConfig> = Arc::clone(&inputs.config);
        let _runtime: Arc<MetricsRuntime> = Arc::clone(&inputs.runtime);
        // 源码级静态门禁（§6.3、§7.8）：main.rs 只调用 build_production_router，
        // 真实源与测试注入缝只出现在 production.rs。
        let main = include_str!("../src/main.rs");
        assert!(main.contains("build_production_router("), "main.rs 必须经唯一入口装配");
        assert!(
            !main.contains("build_production_router_with_metrics_source"),
            "测试注入缝绝不被生产路径调用"
        );
        assert!(
            !main.contains("SqlxMetricsSnapshotSource"),
            "真实汇总源只能在 production.rs 内构造"
        );
        let production = include_str!("../src/production.rs");
        assert!(production.contains("SqlxMetricsSnapshotSource::new(inputs.auth.db.clone())"),
            "汇总源必须由 auth.db 派生（I2 架构契约）");
        let api = include_str!("../src/lib.rs");
        assert!(!api.contains("SqlxMetricsSnapshotSource::new"));
        let http = include_str!("../src/metrics/http.rs");
        assert!(!http.contains("SqlxMetricsSnapshotSource::new"));
    }

    #[tokio::test]
    async fn metrics_timeout_does_not_starve_business_routes() {
        // L3 侧的**结构**部分：合成源永久挂起 + 并发闸占满，业务线路由形状不受影响。
        // 真实慢查询下的池可用性不在此证明范围（§7.7 L4，Task 7）。
        let shared = auth_state();
        let router = build_production_router_with_metrics_source(
            inputs_with_db(
                &lazy_db(),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                Arc::clone(&shared),
                metrics_inputs(true),
            ),
            Arc::new(HangingMetricsSnapshotSource),
        )
        .expect("装配必须成功");
        let pending: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let router = router.clone();
                let headers = authorized_metrics_headers();
                tokio::spawn(async move {
                    let _ = request_with_headers(&router, "GET", "/metrics", &headers).await;
                })
            })
            .collect();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let (status, _) = request_get(&router, "/healthz").await;
        assert_eq!(status, StatusCode::OK, "指标线挂起不得拖住探活");
        let (status, body) = request_get(&router, "/api/v1/auth/me").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "指标线挂起不得改变会话语义");
        assert_eq!(body["error"]["code"], json!("AUTH_REQUIRED"));
        let (status, _) = request_with_headers(&router, "GET", "/metrics", &HeaderMap::new()).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "认证门禁先于并发闸，不受挂起影响");
        for task in pending {
            task.abort();
        }
        // 无新增池构造、无 detach：源码级复核。
        let production = include_str!("../src/production.rs");
        assert!(!production.contains("detach"), "指标线不得把关闭甩到后台");
        assert_eq!(
            production.matches("MySqlPoolOptions::new").count(),
            0,
            "production.rs 不得新建连接池"
        );
    }
```

既有 `mismatched_auth_context_is_rejected_on_every_line` 与 `composed_router_registers_status_route_behind_shared_gate` **保持原样**（§7.6 第 4 条：三条负例不变，证明新增线未削弱原门禁）。

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p rsetup-controller --test production_composition
```

预期：**编译失败（RED）**：

```text
error[E0061]: this function takes 5 arguments but 6 arguments were supplied
error[E0432]: unresolved import `rsetup_controller::production::MetricsLineInputs`
error[E0432]: unresolved import `rsetup_controller::production::build_production_router_with_metrics_source`
error: could not compile `rsetup-controller` (test "production_composition") due to N previous errors
```

- [ ] **Step 3: Write minimal implementation**

`crates/rsetup-controller/src/production.rs` 全文替换为：

```rust
//! 生产路由装配（Task 0a 纯拓扑合并 + Task 0b typed 生产装配 + 指标线六线合并）。
//!
//! 本模块是中控六线（auth / management / tasks / status / metrics / web）**唯一**的
//! 顶层装配点：
//! - [`compose_component_routers`] 只做纯拓扑合并，不额外注册探活路径，路径集合
//!   归各条线所有，组件合并顺序无业务语义；
//! - [`build_production_router`] 接收各条线已构造好的 typed 状态，先强制校验原四线
//!   共享**同一个** `Arc<HttpAuthState>`，再合并。指标线**没有** `HttpAuthState`，
//!   不参与也无法违反该校验（I3：独立认证边界）。
//!
//! 指针一致性是硬门禁（保持原样，只覆盖原四线）。指标线的连接池唯一性由**结构性
//! 收缩 + 契约 + 静态门禁**保证：`MetricsLineInputs` 里既没有 `DbPool` 也没有汇总源，
//! 两者都只能在装配函数内部由 `inputs.auth.db` 派生（I2）。诚实界定：这是装配约定
//! 加上 §7.6 第 9 条的源码级回归与 §7.8 的 grep 静态门禁共同执法的**架构契约**，
//! 不是"编译期不可能违反"的绝对结论。

use std::sync::Arc;

use axum::Router;

use crate::ControllerError;
use crate::api::{ManagementState, build_management_router};
use crate::http_auth::{self, AppState};
use crate::http_device_status::{DeviceStatusState, build_device_status_router};
use crate::metrics::collect::{
    MAX_CONCURRENT_SCRAPE, MetricsRuntime, MetricsSnapshotSource, SqlxMetricsSnapshotSource,
};
use crate::metrics::config::MetricsConfig;
use crate::metrics::http::{MetricsState, build_metrics_router};
use crate::tasks::http::{HttpTasksState, build_tasks_router};

/// 指标线的**全部**外部装配输入：配置与运行时锚点。
///
/// 字段集合**只有这两项**（§7.6 第 9 条用写全字段的构造点把它钉死）：汇总源与池
/// 读数都在 `build_production_router` 内部由 `inputs.auth.db` 派生，外部既递不进
/// `DbPool`，也递不进自建的汇总源。
pub struct MetricsLineInputs {
    pub config: Arc<MetricsConfig>,
    pub runtime: Arc<MetricsRuntime>,
}

/// 生产顶层 router 的全部 typed 输入：六线状态各由所属条线的生产适配器构造，
/// 本模块不构造任何默认源、测试网关或占位池。
pub struct ProductionInputs {
    /// Identity 认证线（含 `/healthz`、`/readyz` 探活）。
    pub auth: AppState,
    /// Identity 管理线（26 条管理路由）。
    pub management: ManagementState,
    /// Runtime 任务线（8 条任务路由）。
    pub tasks: HttpTasksState,
    /// 设备状态读线（1 条状态路由）。
    pub status: DeviceStatusState,
    /// Prometheus 指标线（1 条路由，独立 Basic 认证边界）。
    pub metrics: MetricsLineInputs,
    /// Web 嵌入静态资源线。
    pub web: Router<()>,
}

/// 唯一生产装配入口：校验共享认证上下文指针一致后合并六线子 router。
///
/// 真实汇总源只能在本函数内部由 `auth.db` 构造（I2）；不一致时返回去敏固定
/// `Config`（不回显任何指针/地址值），进程应在触达监听套接字之前失败关闭。
pub fn build_production_router(inputs: ProductionInputs) -> Result<Router<()>, ControllerError> {
    build_router(inputs, None)
}

/// 仅供 `crates/rsetup-controller/tests/` 的装配测试注入合成汇总源。
/// `#[doc(hidden)]` 且 MUST NOT 被 `main.rs` 或任何生产路径调用（§7.8 第 6 条 grep
/// 门禁）。它接收的是 `Arc<dyn MetricsSnapshotSource>`：类型里没有 `DbPool` 只是
/// **收缩违规面**，MUST NOT 据此宣称"该缝在类型上绝不可能引入第二池"。无论走哪个
/// 入口，`MetricsState.pool` 恒为 `inputs.auth.db.clone()`。
#[doc(hidden)]
pub fn build_production_router_with_metrics_source(
    inputs: ProductionInputs,
    snapshot: Arc<dyn MetricsSnapshotSource>,
) -> Result<Router<()>, ControllerError> {
    build_router(inputs, Some(snapshot))
}

/// 唯一实现：`snapshot_override` 只允许测试注入，`None` 即真实源。
fn build_router(
    inputs: ProductionInputs,
    snapshot_override: Option<Arc<dyn MetricsSnapshotSource>>,
) -> Result<Router<()>, ControllerError> {
    if !Arc::ptr_eq(&inputs.auth.auth, &inputs.management.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.tasks.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.status.auth)
    {
        return Err(ControllerError::Config(
            "production auth context mismatch".into(),
        ));
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
    let auth = inputs.auth; // 在指标线借用 db 之后再移交给认证线。
    Ok(compose_component_routers(
        http_auth::build_http_router(auth),
        build_management_router(inputs.management),
        build_tasks_router(inputs.tasks),
        build_device_status_router(inputs.status),
        metrics,
        inputs.web,
    ))
}

/// 把 auth / management / tasks / status / metrics / web 六个组件子 router 合并为
/// 顶层路由。`metrics` 固定排在 `status` 之后、`web` 之前（web 含 SPA 兜底，必须
/// 最后合并）。
pub fn compose_component_routers(
    auth: Router<()>,
    management: Router<()>,
    tasks: Router<()>,
    status: Router<()>,
    metrics: Router<()>,
    web: Router<()>,
) -> Router<()> {
    auth.merge(management)
        .merge(tasks)
        .merge(status)
        .merge(metrics)
        .merge(web)
}
```

`crates/rsetup-controller/src/web.rs`：三个路由表白名单常量改为 `pub`（值与顺序逐字不变），使 §7.6 第 8 条能在不复制清单的前提下钉住"保留集合不与代码漂移"：

```rust
/// 静态资源目录前缀（改为 `pub` 只为 §7.6 第 8 条的一致性断言；值逐字不变）。
pub const ASSET_DIR: &str = "/assets/";

/// 已知 SPA 页面路由白名单（精确路径）。
pub const SPA_EXACT_ROUTES: &[&str] = &[
    "/",
    "/login",
    "/password",
    "/sessions",
    "/devices",
    "/tasks",
    "/admin",
];

/// 已知 SPA 页面路由白名单（带路径参数，axum 0.8 语法）。
pub const SPA_PARAM_ROUTES: &[&str] = &["/devices/{id}", "/tasks/{id}", "/admin/{*rest}"];
```

`crates/rsetup-controller/src/main.rs` 三处改动（既有 secret 路径、`start_if_ready(check_identity_schema, …)` 流程、`TcpListener::bind`/`axum::serve` 全部不动）：

1. `use` 追加：

```rust
use rsetup_controller::metrics::collect::MetricsRuntime;
use rsetup_controller::metrics::config::MetricsConfig;
use rsetup_controller::production::MetricsLineInputs;
```

（`ProductionInputs` 与 `build_production_router` 已由既有 `use rsetup_controller::production::…` 导入，只在既有那行里补 `MetricsLineInputs`，不新增重复 `use`。MUST NOT 在 `main.rs` 里出现 `SqlxMetricsSnapshotSource` 或 `build_production_router_with_metrics_source`——§7.8 第 6 条 grep 门禁要求两者在 `src/` 下只出现在 `production.rs`。）

2. `main()` 第一条语句即 uptime 零点，并在建池/绑端口之前完成指标配置门禁：

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
    // …既有 start_if_ready(check_identity_schema, …) 启动闭包按原形态写作
    //    `|| async move { … }`，把 metrics_config 与 metrics_runtime 一起移入…
    let router = build_production_router(production_inputs(
        &db,
        metrics_config,
        metrics_runtime,
    )?)?;
    // …TcpListener::bind 与 axum::serve 不变…
    Ok(())
}
```

3. `production_inputs` 签名扩展（函数体其余部分逐字保留），末尾返回 `metrics` 字段：

```rust
/// 生产装配唯一构造点。
///
/// 顺序即门禁：运行时间零点由 `main` 第一条语句提供（**不在**本函数里构造，否则
/// uptime 会系统性低估"进程已经活多久"）→ 管理 cursor 密钥 → 共享认证上下文 →
/// 六线状态。
fn production_inputs(
    db: &DbPool,
    metrics: MetricsConfig,
    runtime: Arc<MetricsRuntime>,
) -> Result<ProductionInputs, ControllerError> {
    // …既有 cursor secret / HttpAuthState / management / tasks / status 装配逐字不变…
    Ok(ProductionInputs {
        // …既有字段逐字不变…
        metrics: MetricsLineInputs {
            config: Arc::new(metrics),
            runtime,
        },
    })
}
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo fmt --package rsetup-controller
cargo test -p rsetup-controller --test production_composition
```

预期（GREEN）：

```text
test every_component_is_reachable_without_duplicate_probes ... ok
test duplicate_business_path_panics_instead_of_being_hidden_by_override - should panic ... ok
test metrics_path_colliding_with_reserved_route_panics_on_merge - should panic ... ok
test typed::shared_auth_context_composes_all_six_lines ... ok
test typed::metrics_gauges_read_the_shared_pool_only ... ok
test typed::metrics_line_never_touches_session_boundary ... ok
test typed::reserved_paths_stay_registered ... ok
test typed::production_inputs_exposes_no_second_pool ... ok
test typed::metrics_timeout_does_not_starve_business_routes ... ok
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

判据：`0 failed`；`shared_auth_context_composes_all_five_lines` 已不存在（改名为 `…_all_six_lines`）；`mismatched_auth_context_is_rejected_on_every_line` 仍以三条负例通过。

```bash
cargo build --package rsetup-controller
cargo clippy --package rsetup-controller --all-targets -- -D warnings
cargo test -p rsetup-controller
```

> §7.6 第 9 条的实现取舍：该条要求"断言字段集合只有 `config` 与 `runtime`"。Rust 没有"恰好两个字段"的运行时反射，因此用**写全字段的构造点**（多字段即编译失败）+ 两次显式取字段 + 源码级断言（`include_str!`）实现；对 `RESERVED_PATHS` 与 `web.rs` 三张表的一致性同理用集合包含断言，不引入序列化反射。

- [ ] **Step 5: Commit**

```bash
git add crates/rsetup-controller/src/production.rs crates/rsetup-controller/src/web.rs \
  crates/rsetup-controller/src/main.rs crates/rsetup-controller/tests/production_composition.rs
git commit -m "feat(controller): 生产装配接入普罗米修斯指标第六线"
```
---

## Task 7: 关闭屏障、九条静态门禁、双引擎 L4 与全量门禁

**Files:**
- Create: `crates/rsetup-controller/tests/metrics_real_db.rs`（L4，全 `#[ignore]`，§7.7 三组 + 受控关闭屏障）
- Modify: `Makefile`（新增 `metrics-gates` 目标，承载 §7.8 的九条命令）
- Test: `tests/metrics_real_db.rs`（真库，只读 + 仅本用例自建的 fixture 行）+ 既有全量门禁

**Interfaces:**
- Consumes：`metrics::collect::{MAX_CONCURRENT_SCRAPE, SCRAPE_TIMEOUT, POOL_ACQUIRE_TIMEOUT, DEVICE_AGGREGATE_SQL, MetricsSnapshotSource, SqlxMetricsSnapshotSource}`（Task 4，常量与两条 SQL 均为 `pub`）、`common::required_test_db()`（`tests/common/mod.rs`）。
- Produces：无生产 API（本 Task 只加测试与门禁）。

- [ ] **Step 1: Write the failing test**

`crates/rsetup-controller/tests/metrics_real_db.rs`（整文件）：

```rust
//! L4 真实数据库验收（规范 §7.7）。全部 `#[ignore]`：只在操作员逐引擎显式授权的
//! 隔离开发库上运行，凭据仅进程内载入（`tests/common::required_test_db`），报告只出
//! 去敏数值（耗时、permit/size/idle 计数），绝不回显 DSN（`AGENTS.md` 凭据保护）。
//!
//! 诚实界定（三条，交付报告必须复述）：
//! 1. L2/L3 的合成源通过 MUST NOT 被表述为"真库聚合已验收"；L4 未运行时，交付报告
//!    必须显式写明"两条聚合 SQL 的真实引擎验收仍待执行"。
//! 2. 慢查询与受控关闭屏障使用**测试变体内核**（本文件的 `guarded_snapshot`），它是
//!    §5.5.1 生产骨架的逐字复制，只有两处不同：两条聚合换成 `SELECT SLEEP(...)`，
//!    close await 之前可选插入屏障。生产实现与骨架的一致性由 Task 4 的 L1 源码级门禁
//!    `snapshot_kernel_has_no_early_exit_between_acquire_and_close` 与 §7.8 第 9 条 grep
//!    钉住；本文件证明的是这套骨架在真实驱动下的 `close()`/permit 语义成立。
//! 3. 普通 `SELECT SLEEP` 只拖慢**服务端查询**，而 `MySqlConnection::close()` 只发一个
//!    Quit 报文，"close 中"与"下一波借出"的真实重叠窗口极短且依赖时序，因此第 3 组
//!    第 1 步只能证明宏观不饥饿；"close 完成前 permit 绝不回落"的确定性证明由第 4 步
//!    的受控关闭屏障完成（对旧缺陷实现稳定必红，对正确实现稳定绿）。
#![allow(dead_code)]

mod common;

use rsetup_controller::metrics::collect::{
    DEVICE_AGGREGATE_SQL, MAX_CONCURRENT_SCRAPE, MetricsSnapshotSource, POOL_ACQUIRE_TIMEOUT,
    SCRAPE_TIMEOUT, SqlxMetricsSnapshotSource,
};
use rsetup_controller::DbPool;
use sqlx::Row as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore, oneshot};

/// 大于 `SCRAPE_TIMEOUT`，保证每波都以查询超时收场。
const SLEEP_SECONDS: u64 = 3;
const WAVES: usize = 3;

// ---------------------------------------------------------------------------
// §5.5.1 骨架的测试变体内核（受控关闭屏障的唯一注入点，MUST NOT 进生产代码）
// ---------------------------------------------------------------------------

/// 受控关闭屏障：`wait_at_close` 在 close await **之前**抵达并挂起，直到测试放行。
/// 只控制 close 的完成时刻，不改变 acquire 超时、`close_on_drop()` 兜底、许可持有
/// 任务等任何其余语义。
#[derive(Default)]
struct CloseGate {
    waiting: AtomicUsize,
    arrived: Notify,
    release: Semaphore,
}

impl CloseGate {
    /// 0 permits：抵达即挂起，由测试 `release_all` 放行。
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
    async fn wait_at_close(&self) {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        self.arrived.notify_waiters();
        let permit = self
            .release
            .acquire()
            .await
            .expect("放行侧只 add_permits，永不 close");
        drop(permit);
    }
    fn release_all(&self, count: usize) {
        self.release.add_permits(count);
    }
}

/// 轮询到"恰好 N 路 close 已挂起"，让随后的断言完全不依赖时序侥幸。
async fn await_waiting(gate: &CloseGate, want: usize) {
    for _ in 0..200 {
        if gate.waiting() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "受控关闭屏障未在 5 秒内挂住 {want} 路 close（实测 {}）",
        gate.waiting()
    );
}

/// §5.5.1 代码骨架的复制：显式 acquire 超时 → 立即 `close_on_drop()` → 超时包裹慢
/// 查询 → 结果折入变量 → **无条件** `await close()`。全程禁止 `?` 早退（同 Task 4）。
/// 返回 `true` 表示查询超时（生产侧映射 503）。
async fn guarded_snapshot(db: &DbPool, gate: Option<Arc<CloseGate>>) -> bool {
    let mut conn = match tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, db.0.acquire()).await {
        // 借不到连接：没有连接产生，许可可立即释放。
        Ok(Ok(conn)) => conn,
        _ => return false,
    };
    conn.close_on_drop();
    let outcome = tokio::time::timeout(SCRAPE_TIMEOUT, async {
        sqlx::query(&format!("SELECT SLEEP({SLEEP_SECONDS})"))
            .fetch_optional(&mut *conn)
            .await
    })
    .await;
    let timed_out = outcome.is_err();
    if let Some(gate) = gate {
        gate.wait_at_close().await;
    }
    let _ = conn.close().await;
    timed_out
}

/// 生产 handler 许可接线的测试侧等价物：许可由 `tokio::spawn` 的任务持有，任务返回
/// （= 连接已完全关闭）才随 Drop 释放；返回 `false` 对应生产侧 503（闸满或采集失败）。
async fn scrape_holding_permit(
    db: &DbPool,
    guard: Arc<Semaphore>,
    gate: Option<Arc<CloseGate>>,
) -> bool {
    let Ok(permit) = guard.clone().try_acquire_owned() else {
        return false;
    };
    let (tx, rx) = oneshot::channel();
    let db = db.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _ = tx.send(guarded_snapshot(&db, gate).await);
    });
    rx.await.unwrap_or(false)
}

/// 业务哨兵：每 100 ms 一次 `acquire()` + `SELECT 1` + 归还，记录耗时与成败；
/// 同时采样 `pool.size()` 峰值，用于"指标线占用至多 4 槽"的封顶断言。
#[derive(Default)]
struct Sentinel {
    samples: Mutex<Vec<(u128, bool)>>,
    peak_size: AtomicUsize,
}

impl Sentinel {
    fn spawn(self: &Arc<Self>, db: DbPool) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let started = Instant::now();
                let alive = match tokio::time::timeout(Duration::from_millis(2_000), db.0.acquire())
                    .await
                {
                    Ok(Ok(mut conn)) => {
                        let ok = sqlx::query_scalar::<_, i64>("SELECT 1")
                            .fetch_optional(&mut *conn)
                            .await
                            .is_ok();
                        let _ = conn.close().await;
                        ok
                    }
                    _ => false,
                };
                this.samples
                    .lock()
                    .expect("哨兵样本锁")
                    .push((started.elapsed().as_millis(), alive));
                let _ = this
                    .peak_size
                    .fetch_max(db.0.size() as usize, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    }
    /// `(样本数, 失败数, p50 毫秒, p99 毫秒)`。
    fn report(&self) -> (usize, usize, u128, u128) {
        let mut samples = self.samples.lock().expect("哨兵样本锁");
        let total = samples.len();
        let failed = samples.iter().filter(|(_, alive)| !*alive).count();
        samples.sort_unstable_by_key(|(ms, _)| *ms);
        let percentile = |q: f64| -> u128 {
            if samples.is_empty() {
                return 0;
            }
            let index = ((samples.len() - 1) as f64 * q).round() as usize;
            samples[index].0
        };
        (total, failed, percentile(0.50), percentile(0.99))
    }
}

async fn join_all(handles: Vec<tokio::task::JoinHandle<bool>>) -> Vec<bool> {
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.unwrap_or(false));
    }
    out
}

// ---------------------------------------------------------------------------
// §7.7 第 1 组：分布对账与求和不变量
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires an operator-authorized isolated disposable dev DB (MySQL or TiDB); read-only"]
async fn aggregates_match_table_distribution_and_sum_invariant() {
    let db = common::required_test_db().await;
    let engine: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert!(!engine.trim().is_empty(), "SELECT VERSION() 必须返回版本");

    let source = SqlxMetricsSnapshotSource::new(db.clone());
    let snapshot = source
        .snapshot()
        .await
        .expect("既有数据上的两条聚合必须成功");
    for (label, expected) in [
        ("APPROVED", snapshot.devices.approved),
        ("PENDING", snapshot.devices.pending),
        ("REJECTED", snapshot.devices.rejected),
    ] {
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devices WHERE CAST(admission_state AS BINARY) = CAST(? AS BINARY)",
        )
        .bind(label)
        .fetch_one(&db.0)
        .await
        .unwrap();
        assert_eq!(
            rows as u64, expected,
            "admission={label} 的聚合取值必须与独立 COUNT(*) 对账"
        );
    }
    let device_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(
        device_rows as u64,
        snapshot.devices.approved + snapshot.devices.pending + snapshot.devices.rejected,
        "三桶求和必须恒等于 devices 行数（§5.1 第 4 条）"
    );
    let task_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM main_tasks")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(
        task_rows as u64,
        snapshot.tasks.pending
            + snapshot.tasks.running
            + snapshot.tasks.succeeded
            + snapshot.tasks.failed,
        "四桶求和必须恒等于 main_tasks 行数"
    );
    // 去敏报告：只出计数，不出 DSN、不出取值原文。
    eprintln!(
        "L4 对账：devices_rows={device_rows} tasks_rows={task_rows} 指标侧 devices={}/{}/{} tasks={}/{}/{}/{}",
        snapshot.devices.approved,
        snapshot.devices.pending,
        snapshot.devices.rejected,
        snapshot.tasks.pending,
        snapshot.tasks.running,
        snapshot.tasks.succeeded,
        snapshot.tasks.failed,
    );
}

// ---------------------------------------------------------------------------
// §7.7 第 2 组：字节级分组 + PAD SPACE 对照
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires an operator-authorized isolated disposable dev DB with writable fixture tables; inserts only rows it deletes afterwards"]
async fn grouping_is_byte_exact_on_both_engines() {
    let db = common::required_test_db().await;
    // 两对分组键：只差大小写、只差尾部空格。列名以 `DESCRIBE devices` 为准，
    // 本用例只依赖 admission_state 可写入任意字节串（§5.2.2：库侧已无 CHECK 约束）。
    let ids = fixture_rows(&db, &["CASE", "case", "PADX", "PADX "]).await;

    let byte_rows = grouped_by(&db, DEVICE_AGGREGATE_SQL).await;
    assert!(
        byte_rows.iter().any(|(key, _)| key == "CASE")
            && byte_rows.iter().any(|(key, _)| key == "case"),
        "大小写必须各自成组（字节级分组生效）：{byte_rows:?}"
    );
    assert!(
        byte_rows.iter().any(|(key, _)| key == "PADX")
            && byte_rows.iter().any(|(key, _)| key == "PADX "),
        "尾部空格必须各自成组：{byte_rows:?}"
    );
    assert!(
        count_of(&byte_rows, "CASE") >= 1 && count_of(&byte_rows, "case") >= 1,
        "两行必须落在两个不同分组：{byte_rows:?}"
    );

    // PAD SPACE 陷阱对照：同一份数据用 utf8mb4_bin 分组会**折叠**尾部空格。
    let collation_rows = grouped_by(
        &db,
        "SELECT CAST(admission_state AS CHAR) COLLATE utf8mb4_bin AS bucket, COUNT(*) AS n \
         FROM devices WHERE CAST(admission_state AS BINARY) IN (CAST('PADX' AS BINARY), CAST('PADX ' AS BINARY)) \
         GROUP BY CAST(admission_state AS CHAR) COLLATE utf8mb4_bin",
    )
    .await;
    assert!(
        collation_rows
            .iter()
            .filter(|(key, _)| key.trim_end() == "PADX")
            .count()
            <= 1,
        "utf8mb4_bin 必须把 'PADX' 与 'PADX ' 折叠为同一组，否则本规范对引擎事实的评审不成立：{collation_rows:?}"
    );

    // 两类异常值都必须让映射返回 None → 经采集路径得到 503 fail-closed。
    let source = SqlxMetricsSnapshotSource::new(db.clone());
    let error = source
        .snapshot()
        .await
        .expect_err("未知 admission_state 取值必须让整次抓取失败（I4/G4）");
    let text = format!("{error:?}");
    for marker in ["CASE", "case", "PADX"] {
        assert!(!text.contains(marker), "去敏：错误不得回显取值原文：{text}");
    }

    // ONLY_FULL_GROUP_BY 下可执行；SHOW WARNINGS 不得含 CAST(... AS BINARY) 弃用告警。
    sqlx::query(DEVICE_AGGREGATE_SQL)
        .fetch_all(&db.0)
        .await
        .expect("ONLY_FULL_GROUP_BY 下必须可执行");
    let warnings = sqlx::query("SHOW WARNINGS").fetch_all(&db.0).await.unwrap();
    for row in &warnings {
        let mut text = String::new();
        for column in ["Level", "Message"] {
            if let Ok(bytes) = row.try_get::<&[u8], _>(column) {
                text.push_str(&String::from_utf8_lossy(bytes));
                text.push(' ');
            }
        }
        assert!(
            !text.to_lowercase().contains("deprecated"),
            "SHOW WARNINGS 含 CAST(... AS BINARY) 弃用告警：{text}"
        );
    }
    cleanup_rows(&db, &ids).await;
}

/// 插入给定 `admission_state` 取值的 fixture 行，返回主键以便收尾删除。
async fn fixture_rows(db: &DbPool, values: &[&'static str]) -> Vec<Vec<u8>> {
    let mut ids = Vec::new();
    for (index, value) in values.iter().enumerate() {
        // 32 字节主键（devices.id 为定长/变长二进制主键，取 32 字节两侧兼容）。
        let mut id = format!("l4m{index}").into_bytes();
        id.resize(32, b'0');
        sqlx::query(
            "INSERT INTO devices (id, device_name, admission_state) VALUES (?, ?, ?) \
             ON DUPLICATE KEY UPDATE admission_state = VALUES(admission_state)",
        )
        .bind(&id)
        .bind(format!("l4-metrics-{index}"))
        .bind(*value)
        .execute(&db.0)
        .await
        .expect("fixture 行插入必须在已授权开发库内成功");
        ids.push(id);
    }
    ids
}

async fn cleanup_rows(db: &DbPool, ids: &[Vec<u8>]) {
    for id in ids {
        sqlx::query("DELETE FROM devices WHERE id = ?")
            .bind(id)
            .execute(&db.0)
            .await
            .expect("fixture 必须清理干净，不留残留行");
    }
}

async fn grouped_by(db: &DbPool, sql: &str) -> Vec<(String, i64)> {
    let rows = sqlx::query(sql).fetch_all(db).await.unwrap();
    rows.iter()
        .map(|row| {
            let bucket: &[u8] = row.try_get("bucket").unwrap();
            let n: i64 = row.try_get("n").unwrap();
            (String::from_utf8_lossy(bucket).into_owned(), n)
        })
        .collect()
}

fn count_of(rows: &[(String, i64)], key: &str) -> i64 {
    rows.iter()
        .find(|(k, _)| k == key)
        .map(|(_, n)| *n)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// §7.7 第 3 组：多波超时 + 业务哨兵 + 受控关闭屏障 + 取消变体
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an operator-authorized isolated disposable dev DB; runs 3 slow waves plus a controlled close barrier"]
async fn metrics_slow_query_does_not_exhaust_shared_business_pool() {
    let db = common::required_test_db().await; // **同一个池**，绝不新建（§7.7 第 3 组）
    let baseline = db.0.size() as usize;
    let guard = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPE));
    let sentinel = Arc::new(Sentinel::default());
    let sentinel_task = Arc::clone(&sentinel).spawn(db.clone());

    // 第 1 步：3 波背靠背，每波 MAX+1 路（其中 1 路必然闸满）。
    let mut wave_elapsed = Vec::new();
    for wave in 0..WAVES {
        let started = Instant::now();
        let handles: Vec<_> = (0..=MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let db = db.clone();
                let guard = Arc::clone(&guard);
                tokio::spawn(async move { scrape_holding_permit(&db, guard, None).await })
            })
            .collect();
        let results = join_all(handles).await;
        assert!(
            results.iter().all(|ok| !ok),
            "第 {wave} 波必须全部定性为采集失败或闸满：{results:?}"
        );
        wave_elapsed.push(started.elapsed());
        // 辅助判据：若许可随超时被提前放行，下一波会立刻进闸并再付一次 2 秒超时；
        // 正确实现下第 2、3 波应在毫秒级结束（"闸满即拒"的比例不低于第 1 波）。
        if wave > 0 {
            assert!(
                wave_elapsed[wave] < Duration::from_millis(4_000),
                "第 {wave} 波耗时 {:?} 说明许可未按连接生命周期持有",
                wave_elapsed[wave]
            );
        }
    }

    // 第 2/3 步：哨兵零失败、每次 acquire < 500 ms、p99 不随波次上升。
    tokio::time::sleep(Duration::from_secs(5)).await;
    sentinel_task.abort();
    let (total, failed, p50, p99) = sentinel.report();
    assert!(
        total >= 30,
        "哨兵样本过少（{total}），不足以证明无累积型饥饿"
    );
    assert_eq!(failed, 0, "业务哨兵必须零失败");
    assert!(p50 < 500, "业务 acquire p50 必须 < 500 ms，实测 {p50} ms");
    assert!(p99 < 500, "业务 acquire 每次都必须 < 500 ms，实测 {p99} ms");
    let peak = sentinel.peak_size.load(Ordering::SeqCst);
    assert!(
        peak <= baseline + MAX_CONCURRENT_SCRAPE,
        "指标线任何时刻（在途 + 关闭中）至多占 {MAX_CONCURRENT_SCRAPE} 槽，实测峰值 {peak}（基线 {baseline}）"
    );
    assert!(
        (db.0.size() as usize) <= baseline + 1,
        "末波结束 5 秒内池容量必须回落基线附近，实测 {}",
        db.0.size()
    );
    eprintln!(
        "L4 池回落：size={}/{} idle={} 哨兵 n={total} 失败={failed} p50={p50}ms p99={p99}ms 波耗时={wave_elapsed:?}",
        db.0.size(),
        baseline + MAX_CONCURRENT_SCRAPE,
        db.0.num_idle(),
    );

    // 第 4 步：**受控关闭屏障**——close 完成前 permit 绝不回落的确定性断言。
    let gate = CloseGate::new();
    let handles: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
        .map(|_| {
            let db = db.clone();
            let guard = Arc::clone(&guard);
            let gate = Arc::clone(&gate);
            tokio::spawn(async move { scrape_holding_permit(&db, guard, Some(gate)).await })
        })
        .collect();
    await_waiting(&gate, MAX_CONCURRENT_SCRAPE).await;
    assert_eq!(
        guard.available_permits(),
        0,
        "屏障挂起期间（close 未完成）permit 绝不回落"
    );
    let fifth_started = Instant::now();
    let fifth = scrape_holding_permit(&db, Arc::clone(&guard), None).await;
    assert!(!fifth, "第 5 路必须闸满拒绝（等价断言：始终拿不到许可）");
    assert!(
        fifth_started.elapsed() < Duration::from_millis(200),
        "闸满必须**立即**返回，实测 {:?}",
        fifth_started.elapsed()
    );
    // 放行屏障：4 路 close 完成 → 任务退出 → permit 恢复 → 新抓取可进闸。
    gate.release_all(MAX_CONCURRENT_SCRAPE);
    join_all(handles).await;
    for _ in 0..40 {
        if guard.available_permits() == MAX_CONCURRENT_SCRAPE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        guard.available_permits(),
        MAX_CONCURRENT_SCRAPE,
        "close 完成后 permit 必须随任务退出恢复"
    );
    drop(
        db.0.acquire()
            .await
            .expect("业务必须始终能借到连接"),
    );

    // 第 6 步（SHOULD）：取消变体——4 路抓取刚发起就掐断调用方，再连发 2 波。
    for wave in 0..2 {
        let cancel_guard = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPE));
        let handles: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let db = db.clone();
                let guard = Arc::clone(&cancel_guard);
                tokio::spawn(async move { scrape_holding_permit(&db, guard, None).await })
            })
            .collect();
        for handle in handles {
            handle.abort(); // 模拟客户端断线：调用方被取消，采集任务不受影响
        }
        let started = Instant::now();
        let conn = tokio::time::timeout(Duration::from_millis(500), db.0.acquire())
            .await
            .unwrap_or_else(|_| panic!("第 {wave} 波掐断后业务 acquire 仍须 < 500 ms"))
            .expect("掐断调用方后业务必须仍能借到连接");
        drop(conn);
        eprintln!("L4 取消变体：业务 acquire 耗时 {:?}", started.elapsed());
    }
}
```

`Makefile` 追加目标（并在既有 `.PHONY` 行登记 `metrics-gates`）：

```make
# Prometheus 指标线的静态门禁（规范 §7.8 的九条命令）。第 5/6/7/9 条必须无输出、
# 第 8 条必须**有**输出——两条 grep 的期望相反是刻意的：前者拦手写常量时间折叠与
# 测试注入缝泄漏进生产入口，后者钉住比较确实落在 subtle 上。第 5 条的模式串刻意写成
# shell 拆分形式 'TO''DO|T-B-D|FIXME'，这样把命令写进文档不会自指命中。
# 第 1 条用仓库既有的 fmt-check（等价于 cargo fmt --all -- --check，但额外校验
# FMT_PACKAGES 未漏登记 workspace member）。
.PHONY: metrics-gates
metrics-gates: fmt-check
	cargo clippy --workspace --all-targets -- -D warnings
	cargo test -p rsetup-controller
	cargo test -p rsetup-controller --test production_composition
	@test -z "$$(grep -rnE 'TO''DO|T-B-D|FIXME' crates/rsetup-controller/src/metrics)" || { \
	  echo "metrics 门禁 5：src/metrics 下存在 TODO/T-B-D/FIXME 占位符" >&2; exit 1; }
	@test -z "$$(grep -rn 'build_production_router_with_metrics_source\|SqlxMetricsSnapshotSource::new' crates/rsetup-controller/src/main.rs)" || { \
	  echo "metrics 门禁 6：生产入口不得触碰测试注入缝或自建汇总源" >&2; exit 1; }
	@test -z "$$(grep -rn 'diff |= a\[i\]' crates/rsetup-controller/src/metrics)" || { \
	  echo "metrics 门禁 7：指标线不得手写常量时间折叠" >&2; exit 1; }
	@test -n "$$(grep -rn 'subtle::ConstantTimeEq\|use subtle::ConstantTimeEq' crates/rsetup-controller/src/metrics/auth.rs)" || { \
	  echo "metrics 门禁 8：常量时间比较必须落在 subtle 上" >&2; exit 1; }
	@test -z "$$(grep -nE 'SCRAPE_TIMEOUT.*\?' crates/rsetup-controller/src/metrics/collect.rs)" || { \
	  echo "metrics 门禁 9：连接借出后禁止 ? 早退（F1/I7）" >&2; exit 1; }
	@if git diff --cached --name-only | grep -Eq 'controller\.toml|^secret/'; then \
	  echo "metrics 门禁（§7.8 末段附加检查）：暂存区含配置文件实体或凭据目录" >&2; exit 1; fi
	@echo "metrics-gates：九条门禁全部符合预期（第 8 条已确认有输出）"
```

- [ ] **Step 2: Run test to verify it fails**

```bash
make metrics-gates
cargo test -p rsetup-controller --test metrics_real_db --no-run
```

预期（RED）：

```text
metrics 门禁 8：常量时间比较必须落在 subtle 上
make: *** [Makefile:NN: metrics-gates] Error 1
```

以及 L4 文件在 `collect.rs` 尚未导出常量时的编译失败形态：

```text
error[E0432]: unresolved imports `rsetup_controller::metrics::collect::SCRAPE_TIMEOUT`, `rsetup_controller::metrics::collect::DEVICE_AGGREGATE_SQL`
error: could not compile `rsetup-controller` (test "metrics_real_db") due to 1 previous error
```

门禁自身的 RED 记录（一次性验证第 9 条真的会红，验证后立即还原，**不得提交**）：

```bash
printf '\n// SCRAPE_TIMEOUT ?\n' >> crates/rsetup-controller/src/metrics/collect.rs
grep -nE 'SCRAPE_TIMEOUT.*\?' crates/rsetup-controller/src/metrics/collect.rs   # 必须命中
git checkout -- crates/rsetup-controller/src/metrics/collect.rs                  # 立即还原
git status --short                                                               # 确认已还原
```

- [ ] **Step 3: Write minimal implementation**

本 Task 不改生产代码，"实现"即让门禁与 L4 文件成立：

1. `metrics/auth.rs` 含 `use subtle::ConstantTimeEq;` 且 `ct_eq_32` 内以 `a.ct_eq(b).into()` 收敛为 `bool`（Task 2 已给出），`Cargo.toml` 含 `subtle.workspace = true` → 第 8 条转绿。
2. `metrics/collect.rs` 的 `snapshot_via` 保持 §5.5.1 四步骨架、`conn.close().await` 无条件执行 → 第 9 条保持无输出。
3. `src/main.rs` 只出现 `build_production_router(` → 第 6 条保持无输出（Task 6 已给出）。
4. L4 的 `guarded_snapshot` 是 §5.5.1 骨架的复制：任何对骨架的改动都必须同步到这里（两侧注释都写明对端位置），否则 Task 4 的 L1 源码级门禁与评审会指出漂移。
5. 屏障只包 close 的完成信号：`wait_at_close()` 位于 `close_on_drop()` 之后、`conn.close().await` 之前；不触碰 `POOL_ACQUIRE_TIMEOUT`、不取消 `close_on_drop()`、不改许可持有任务。
6. 受控关闭屏障的确定性来自"抵达数可观测"（`await_waiting` 轮询到 4 才断言），不依赖网络往返或 MySQL 关闭时序；对"提前释放许可"的缺陷实现，第 4 步的第 2 小步稳定必红。
7. `tests/metrics_real_db.rs` 顶部用 `#![allow(dead_code)]`：本文件只用 `common::required_test_db`，其余 `pub` 辅助函数在该测试目标内未使用属预期，与既有 `tests/mysql_identity.rs` 的形态一致但更严格（不产生 clippy `-D warnings` 噪声）。

- [ ] **Step 4: Run test to verify it passes**

离线全量门禁（不含真库）：

```bash
make metrics-gates
cargo test -p rsetup-controller
cargo test -p rsetup-controller --test production_composition
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

预期：

```text
test result: ok. N passed; 0 failed; M ignored; 0 measured; 0 filtered out
metrics-gates：九条门禁全部符合预期（第 8 条已确认有输出）
```

判据：`0 failed`；`M ignored` 至少比基线多 3（本 Task 新增 3 个 L4 用例）；`make metrics-gates` 末行打印成功；第 8 条 grep 至少命中 1 行（在 Makefile 内被 `-n` 判定，故终端不回显）。

L4 真库轨道（**逐引擎由操作员显式授权**；凭据仅进程内载入，报告不出 DSN）：

```bash
# 由操作员把已授权的开发库 URL 注入进程环境（tests/common::required_test_db 读取），
# MySQL 与 TiDB 各一轮，两轮都必须跑完第 2 组。
cargo test -p rsetup-controller --test metrics_real_db -- --ignored --test-threads=1 --nocapture
```

预期：

```text
test aggregates_match_table_distribution_and_sum_invariant ... ok
test grouping_is_byte_exact_on_both_engines ... ok
test metrics_slow_query_does_not_exhaust_shared_business_pool ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

判据：三组全绿，报告只含去敏数值（`L4 对账：…`、`L4 池回落：…`、`L4 取消变体：…`）。**若本轮未执行 L4，交付报告必须显式标注"两条聚合 SQL 的真实引擎验收仍待执行"，不得以 L2/L3 fake 通过替代（§9 第 9 条、G4）。**

- [ ] **Step 5: Commit**

```bash
git add Makefile crates/rsetup-controller/tests/metrics_real_db.rs
git diff --cached --name-only          # 逐行确认：不得含 controller.toml 实体、secret/ 任何内容
git diff --cached | grep -nE 'password[[:space:]]*=[[:space:]]*"[^"]{8,}"' || echo "暂存区无凭据形态"
git status --short                     # 确认 secret/ 未被改动
git commit -m "test(controller): 增加指标线受控关闭屏障与静态门禁"
```

严禁 `git push`；严禁 `git add -f`。

- [ ] **Step 6: 交付报告要素（§9 Definition of Done 自检表）**

报告须逐条给出证据（测试名 + 命令 + 关键输出行）：

| # | DoD 条目 | 证据来源 |
| --- | --- | --- |
| 1 | §7.2–§7.6 全部测试存在且通过，各有 RED 记录 | Task 1–6 的 Step 2 / Step 4 输出 |
| 2 | 默认禁用态零改变；启用后同端口 200 且 golden 逐字节；HEAD 200 空体不触达源 | Task 3 golden、Task 5 `head_with_valid_credentials_returns_200_without_collection`、Task 6 `shared_auth_context_composes_all_six_lines` |
| 3 | 三阶段配置与 fail-closed 无副作用 | Task 1 `parses_aligned_example` / `rejects_invalid_toml_without_leaking_values` / `missing_credentials_precede_type_error` |
| 4 | §4.5 矩阵每行有断言；401 逐字节；405 带无空格 `Allow: GET,HEAD`；200 无 `Allow` | Task 5 全 19 例 |
| 5 | §4.1 隔离与 I1–I7 有结构或测试证据；无第二池 | Task 5 结构门禁、Task 6 `production_inputs_exposes_no_second_pool` |
| 6 | §5.5.1 四条防护可指认，§7.7 第 3 组含受控关闭屏障已执行 | Task 4 `snapshot_kernel_has_no_early_exit_between_acquire_and_close`、Task 7 L4 |
| 7 | 两条聚合为字节级分组且双引擎过 §7.7 第 2 组 | Task 4 `aggregate_sql_is_byte_level_grouping`、Task 7 `grouping_is_byte_exact_on_both_engines` |
| 8 | §7.8 九条门禁符合预期，提交不含凭据 | `make metrics-gates` + Step 5 三条检查 |
| 9 | L4 未执行则显式标注 | 交付报告首段 |

必须写进交付报告的五条偏差与前置：

1. **文件命名以规范为准**：handler 位于 `src/metrics/http.rs`（任务描述里的 `handler.rs` 是同一模块的旧称；规范 §6.1/§7.1 固定为 `http.rs`）。
2. **`source_timeout_maps_to_503` 的实现形态与 §7.5 原文不同**：§5.5.1 规则 2 禁止 handler 侧第三层超时，故改为"真实源 + 不可达 lazy 池"证明超时上界与 503 映射，查询级超时由 L4 验收（Task 5 Step 4 偏差说明 1）。
3. **`toml 0.8` 是工作区新增 crate**（首次需联网更新 `Cargo.lock`）；`base64 0.22` 与 `subtle 2.6` 已在锁文件内，不构成新依赖树（§8 R10）。
4. **`MAX_EXECUTION_TIME(2000)` 提示未加入 SQL**：§5.5.1 规则 5 把它列为 SHOULD，须先由 §7.7 双引擎确认在 MySQL 与 TiDB 上都安全；确认前加入会让 `aggregate_sql_is_byte_level_grouping` 失败。
5. **L4 未运行时本计划的验收状态是"部分完成"**：离线门禁全绿 + 真库聚合与受控关闭屏障验收待执行（§9 第 6/9 条）。

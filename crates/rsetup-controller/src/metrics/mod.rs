//! Controller Prometheus 指标线（规范 `docs/superpowers/specs/2026-10-19-prometheus-metrics-endpoint-design.md`）。
//!
//! 依赖方向 MUST 单向（规范 §6.1）：`http → {auth, collect, render, config}`、
//! `collect → model`、`render → model`；`config`/`model` 是纯函数层，MUST NOT `use axum`
//! 或 `sqlx`。本模块整体是**独立认证边界**：不读会话 Cookie、不建会话、不占 CSRF/限速桶、
//! 不写审计（I3/I6），因此这里 MUST NOT 出现 `Arc<HttpAuthState>`、`Session` 或 `EndpointId`。

pub mod config;

/// Rust edition 标签值。Cargo 不暴露 `CARGO_PKG_EDITION`，故显式声明，
/// 由 `metrics_edition_matches_workspace_manifest` 与根清单 `[workspace.package]` 对齐。
pub const EDITION: &str = "2024";

pub use config::{
    AUTH_PASSWORD_ENV, AUTH_USER_ENV, BasicCredentials, CONFIG_PATH_ENV, DEFAULT_CONFIG_PATH,
    DEFAULT_ENDPOINT, EnvRead, MetricsAuthRaw, MetricsConfig, MetricsFile, MetricsSection, REALM,
    RESERVED_PATHS, is_reserved,
};

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
        let declared = workspace_package_edition(manifest)
            .expect("根清单 [workspace.package] 必须有 edition 行");
        assert_eq!(declared, EDITION);
    }
}

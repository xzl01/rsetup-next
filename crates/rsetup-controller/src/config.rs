use crate::ControllerError;

/// 控制器启动配置。
///
/// `database_url` 是含账号与口令的完整 DSN，属于敏感值，因此本类型**不派生**
/// `Debug`，改用手写实现固定输出脱敏占位（见下方 `impl Debug`）。`Clone` 保留，
/// 复制的是真实 DSN（调用点需要真实值去连库）。
#[derive(Clone)]
pub struct ControllerConfig {
    pub database_url: String,
    pub listen_address: String,
}

/// 脱敏 `Debug`：`database_url` 永远只打印 `"[redacted]"`，绝不把 DSN（含口令、
/// 主机、库名）带进日志、`unwrap`/`expect` 附带输出或 panic 报告。
impl std::fmt::Debug for ControllerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControllerConfig")
            .field("database_url", &"[redacted]")
            .field("listen_address", &self.listen_address)
            .finish()
    }
}

impl ControllerConfig {
    pub fn from_env() -> Result<Self, ControllerError> {
        let database_url = std::env::var("CONTROLLER_DATABASE_URL")
            .map_err(|_| ControllerError::Config("CONTROLLER_DATABASE_URL is required".into()))?;
        if database_url.is_empty() {
            return Err(ControllerError::Config(
                "CONTROLLER_DATABASE_URL is empty".into(),
            ));
        }
        Ok(Self {
            database_url,
            listen_address: std::env::var("CONTROLLER_LISTEN_ADDRESS")
                .unwrap_or_else(|_| "127.0.0.1:8080".into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ControllerConfig;

    /// 构造期就配好的样本 DSN：含 scheme、账号、口令、主机与库名五个片段。
    fn sample() -> ControllerConfig {
        ControllerConfig {
            database_url: "mysql://rsc_svc:Sup3r-S3cret-Pw@db.internal:3306/rsc_ctl".into(),
            listen_address: "127.0.0.1:18080".into(),
        }
    }

    #[test]
    fn debug_redacts_database_url_dsn_and_password() {
        let rendered = format!("{:?}", sample());
        // DSN 的任何片段都不得出现在 Debug 里。
        for fragment in [
            "Sup3r-S3cret-Pw",
            "mysql://",
            "rsc_svc",
            "db.internal",
            "3306",
            "rsc_ctl",
        ] {
            assert!(
                !rendered.contains(fragment),
                "Debug 输出不得含 DSN 片段 {fragment:?}，实际：{rendered}"
            );
        }
        // 占位符存在且可指认，非敏感字段照常可读。
        assert!(
            rendered.contains("[redacted]"),
            "Debug 输出必须以 [redacted] 代替 database_url，实际：{rendered}"
        );
        assert!(
            rendered.contains("127.0.0.1:18080"),
            "listen_address 应正常输出，实际：{rendered}"
        );
    }

    #[test]
    fn clone_still_carries_the_real_database_url() {
        // 脱敏只针对 Debug，Clone 必须仍是完整值（否则连库会拿到占位符）。
        let cloned = sample().clone();
        assert!(
            cloned
                .database_url
                .starts_with("mysql://rsc_svc:Sup3r-S3cret-Pw@"),
            "Clone 必须保留真实 DSN"
        );
        assert!(format!("{cloned:?}").contains("[redacted]"));
    }
}

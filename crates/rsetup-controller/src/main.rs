//! 中控进程唯一入口（集成验收 Task 0b：全量 typed 生产装配）。
//!
//! 唯一构造点：单一 `DbPool` → 单一 `Arc<HttpAuthState>` → auth/management/tasks/
//! status/web 五线共享同一认证上下文（session 预过滤、CSRF 绑定表与限速桶跨端点
//! 互相可见），再经 [`build_production_router`] 合并成唯一顶层 router。
//! 本入口不构造测试网关、`connect_lazy` 占位池、固定 offline/空能力或 authz
//! epoch 0，也不构造任何返回伪造取值的常量源；尚未交付的 transport 线三个源以
//! **读取即失败**的 fail-closed 装配位接入（见 [`UnwiredTransportSources`]）。

use std::sync::Arc;

use rsetup_controller::api::adapters::build_sqlx_management_state;
use rsetup_controller::api::approvals::ManagementNotificationSink;
use rsetup_controller::api::devices::{DeviceLiveProbe, DeviceLiveView};
use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
use rsetup_controller::api::{
    ManagementFuture, install_management_cursor_secret, load_management_cursor_secret_from_env,
};
use rsetup_controller::auth::service::AuthService;
use rsetup_controller::auth::sqlx_repo::SqlxIdentityRepository;
use rsetup_controller::authz::sqlx_repo::SqlxAuthorizationSnapshotRepository;
use rsetup_controller::devices::service::AdmissionChangeEvent;
use rsetup_controller::http_auth::{AppState, HttpAuthState};
use rsetup_controller::http_device_status::{DeviceStatusState, EmptyDeviceStatusSnapshotSource};
use rsetup_controller::http_live::LiveAuth;
use rsetup_controller::http_security::HttpSecurityConfig;
use rsetup_controller::production::{ProductionInputs, build_production_router};
use rsetup_controller::tasks::SqlxTaskApi;
use rsetup_controller::tasks::http::HttpTasksState;
use rsetup_controller::tasks::repository::SqlxTaskRepository;
use rsetup_controller::tasks::service::TaskService;
use rsetup_controller::web_router;
use rsetup_controller::{
    BootstrapSecretSink, ControllerConfig, ControllerError, DbPool, bootstrap_admin,
    check_identity_schema,
};

struct LogSecretSink(std::path::PathBuf);
impl BootstrapSecretSink for LogSecretSink {
    fn emit(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), rsetup_controller::ControllerError> {
        write_private_secret(&self.0, username, password).map_err(|error| {
            rsetup_controller::ControllerError::Config(format!(
                "unable to record first-boot secret in private file: {error}; administrator recovery requires separate reviewed procedure"
            ))
        })
    }
}

/// Transport 线三个真实源的 **fail-closed** 装配位：设备实时视图探针、恢复模式
/// 探针与 commit 后 typed 通知通道。
///
/// Transport 的 typed 设备缓存/恢复控制器/通知适配器尚未交付（见 Task 0b 报告
/// concerns）。本装配位的唯一行为是**读取即失败上抛**：`DeviceLiveProbe` 与
/// `RecoveryModeProbe` 的契约本身就规定「读取失败必须失败上抛（503），不得降级为
/// offline/空能力 200」「不可读时上抛错误映射 503 `NOT_READY`」，因此这里返回固定
/// 去敏 `Config` 是契约内的正确失败形态，绝不返回伪造的 `Offline`、空能力或
/// `ready = true`，也绝不返回 `Ok(())` 假装通知已入队。
///
/// 真实适配器交付后，只需在此处替换为 transport 的实现，五线装配拓扑不变。
struct UnwiredTransportSources;

impl DeviceLiveProbe for UnwiredTransportSources {
    fn snapshot<'a>(&'a self, _id: &'a [u8; 32]) -> ManagementFuture<'a, DeviceLiveView> {
        Box::pin(async {
            Err(ControllerError::Config(
                "device live cache not wired".into(),
            ))
        })
    }
}

impl RecoveryModeProbe for UnwiredTransportSources {
    fn current_mode<'a>(&'a self) -> ManagementFuture<'a, RecoveryView> {
        Box::pin(async {
            Err(ControllerError::Config(
                "recovery controller not wired".into(),
            ))
        })
    }
}

impl ManagementNotificationSink for UnwiredTransportSources {
    fn after_commit(&self, _event: &AdmissionChangeEvent) -> Result<(), ControllerError> {
        Err(ControllerError::Config(
            "admission notification channel not wired".into(),
        ))
    }
}

/// 生产装配唯一构造点。
///
/// 顺序即门禁：管理 cursor 密钥（唯一来源是环境变量，缺失/非法即 `Err`，绝不以
/// 「无 cursor 密钥」降级启动）→ 共享认证上下文 → 五线状态各自 `auth.clone()`。
fn production_inputs(db: &DbPool) -> Result<ProductionInputs, ControllerError> {
    let cursor_secret = load_management_cursor_secret_from_env()?;
    install_management_cursor_secret(&cursor_secret)?;

    let auth = Arc::new(HttpAuthState::new(
        Arc::new(LiveAuth::new(
            Arc::new(AuthService::new(Arc::new(SqlxIdentityRepository::new(
                db.clone(),
            )))?),
            db.clone(),
        )),
        HttpSecurityConfig::from_env()?,
    ));

    let transport = Arc::new(UnwiredTransportSources);
    let management = build_sqlx_management_state(
        db.clone(),
        auth.clone(),
        transport.clone(),
        transport.clone(),
        transport,
    );

    let tasks = HttpTasksState {
        api: Arc::new(SqlxTaskApi::new(
            Arc::new(TaskService::new(SqlxTaskRepository::new(db.0.clone()))),
            Arc::new(SqlxTaskRepository::new(db.0.clone())),
            Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone())),
            cursor_secret,
        )),
        auth: auth.clone(),
    };

    Ok(ProductionInputs {
        auth: AppState {
            db: db.clone(),
            auth: auth.clone(),
        },
        management,
        tasks,
        status: DeviceStatusState {
            auth,
            snapshot: Arc::new(EmptyDeviceStatusSnapshotSource),
        },
        web: web_router(),
    })
}

fn write_private_secret(
    path: &std::path::Path,
    username: &str,
    password: &str,
) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    // create_new + 0600 protect the file, not its parent: deploy under a private directory.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{username}: {password}")?;
    file.sync_all()
}

async fn start_if_ready<C, F, Fut>(check: C, start: F) -> Result<(), Box<dyn std::error::Error>>
where
    C: std::future::Future<Output = Result<(), rsetup_controller::ControllerError>>,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    check.await?;
    start().await
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ControllerConfig::from_env()?;
    let db = DbPool::connect(&config).await?;
    let secret_path = std::env::var_os("CONTROLLER_BOOTSTRAP_SECRET_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "controller-bootstrap-secret.log".into());
    start_if_ready(check_identity_schema(&db), || async {
        bootstrap_admin(&db, &LogSecretSink(secret_path)).await?;
        // 唯一生产装配点：五线共享同一 Arc<HttpAuthState>，指针不一致即失败关闭。
        let router = build_production_router(production_inputs(&db)?)?;
        let listener = tokio::net::TcpListener::bind(&config.listen_address).await?;
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await?;
        Ok::<(), Box<dyn std::error::Error>>(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cargo_run_selects_readonly_server() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            manifest
                .lines()
                .any(|line| line.trim() == "default-run = \"rsetup-controller\"")
        );
    }

    #[tokio::test]
    async fn startup_blocks_bootstrap_and_listener_when_schema_not_ready() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let result = start_if_ready(
            async {
                Err(rsetup_controller::ControllerError::SchemaNotReady {
                    found: Some(1),
                    required: 2,
                })
            },
            || async {
                calls.set(calls.get() + 1);
                Ok(())
            },
        )
        .await;
        assert!(matches!(
            result
                .unwrap_err()
                .downcast_ref::<rsetup_controller::ControllerError>(),
            Some(rsetup_controller::ControllerError::SchemaNotReady {
                found: Some(1),
                required: 2
            })
        ));
        assert_eq!(calls.get(), 0);
        start_if_ready(async { Ok(()) }, || async {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn secret_sink_reports_actual_create_new_failure_without_overwrite() {
        let path =
            std::env::temp_dir().join(format!("rsetup-secret-failure-{}", uuid::Uuid::new_v4()));
        let sink = LogSecretSink(path.clone());
        write_private_secret(&path, "admin", "first").unwrap();
        let result = sink.emit("admin", "second");
        assert!(result.is_err(), "sink must report write failure");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "admin: first\n");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn first_boot_secret_is_private_and_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("rsetup-controller-secret-{}", uuid::Uuid::new_v4()));
        write_private_secret(&path, "admin", "first").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(write_private_secret(&path, "admin", "second").is_err());
        assert!(std::fs::read_to_string(&path).unwrap().contains("first"));
        std::fs::remove_file(path).unwrap();
    }
}

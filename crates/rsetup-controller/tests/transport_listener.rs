//! 生产监听装配（`transport::listener`）的回归测试（审查 P2-13 / P1-5 / 补项）。
//!
//! 覆盖三件事：
//!
//! 1. **环境装配**：变量「缺失」与「非法」必须分开（`NotUnicode`、空串、0、超硬上界一律
//!    `Err`，只有 `RSETUP_CONTROLLER_KEY_FILE` 完全缺失才是「不启用板端接入」）；
//! 2. **单一预算真源**：同一份 `ManagerConfig` 同时是监听侧连接任务上界与管理器预算；
//!    连接任务上界是**受检和**；排空宽限期不得短于会话 `drain_timeout`（§6.6 的 ≥5s 下界）；
//! 3. **accept 侧有界资源**：全局握手速率与单一来源未认证名额在**昂贵握手之前**判定，
//!    并且监听在关停时**自己返回**（连接任务全部结清）。
//!
//! # 诚实边界
//!
//! 第 3 组用真实 `TcpListener` + 真实 loopback socket，但对端**不发**合法 ClientHello，
//! 因此握手停在读帧阶段：这足以证明「限额在握手前判定」与「关停会 await 连接任务」，
//! **不证明**真实记录层/h2 GOAWAY 帧与真实板端进程的排空行为（那在报告的「未验」清单里）。
//! 注入的 [`AdmissionPolicy`] 是策略的**注入点**（生产注入 `SqlxAdmissionPolicy`），不是测试开关。

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;

use std::time::Duration;

use tokio::net::TcpStream;
use tokio::sync::watch;

use chrono::Utc;
use rsetup_controller::ControllerError;
use rsetup_controller::api::system::{RecoveryModeProbe, RecoveryView};
use rsetup_controller::polling::SnapshotStore;
use rsetup_controller::tasks::adapter::{TaskTimeSource, TimeEvidenceFuture};
use rsetup_controller::time::evidence::{TimeEvidence, TimeQuality};
use rsetup_controller::transport::listener::{
    ListenerError, ListenerStats, TransportConfig, TransportEnv, run_transport_listener_with_stats,
};
use rsetup_controller::transport::manager::AdmissionDecisionFuture;
use rsetup_controller::transport::manager::{AdmissionTruth, ManagerConfig, TransportManager};
use rsetup_tunnel::handshake::{AdmissionDecision, AdmissionFuture, AdmissionPolicy, VerifiedPeer};
use uuid::Uuid;

/// 环境变量是进程级全局状态：本文件的用例必须串行跑。
static ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// 替身：准入策略注入点 + 只读真相 + 固定时间源 + 失败关闭的恢复探针
// ---------------------------------------------------------------------------

/// 一律拒绝的策略：本文件的用例都让握手停在读帧阶段（对端不发包），因此 `evaluate`
/// 实际不会被调用；它的存在只证明监听**只经注入点**取准入决定。
struct RejectPolicy;

impl AdmissionPolicy for RejectPolicy {
    fn evaluate<'a>(&'a self, _peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        Box::pin(async move { Ok(AdmissionDecision::Rejected(2)) })
    }
}

struct NeverTruth;

impl AdmissionTruth for NeverTruth {
    fn load_current(&self, _public_key: [u8; 32]) -> AdmissionDecisionFuture<'_> {
        Box::pin(async { Err(ControllerError::NotFound) })
    }
}

struct FixedTime;

impl TaskTimeSource for FixedTime {
    fn current_evidence(&self) -> TimeEvidenceFuture<'_> {
        Box::pin(async {
            let now = Utc::now();
            TimeEvidence {
                system_wall_utc: now,
                reference_utc: now,
                quality: TimeQuality::SystemFallback,
                clock_epoch: Uuid::new_v4(),
                source: None,
                sample_age_ms: None,
                offset_ms: None,
                uncertainty_ms: None,
            }
        })
    }
}

struct FailingRecovery;

impl RecoveryModeProbe for FailingRecovery {
    fn current_mode<'a>(&'a self) -> rsetup_controller::api::ManagementFuture<'a, RecoveryView> {
        Box::pin(async {
            Err(ControllerError::Config(
                "recovery controller not wired".to_string(),
            ))
        })
    }
}

// ---------------------------------------------------------------------------
// 环境装配
// ---------------------------------------------------------------------------

/// 子进程入口的名字：非 UTF-8 环境变量只能由**子进程**构造（父进程只断言其输出）。
const CHILD_TEST: &str = "env_child_reports_verdict";

/// 子进程入口：仅被 [`env_invalid_utf8_is_an_error_not_swallowed`] 以
/// `--ignored --exact` 方式单独运行，把 `TransportEnv::from_env()` 的判定如实打到 stderr。
#[test]
#[ignore = "由 env 用例以子进程方式调用，不作为常规用例"]
fn env_child_reports_verdict() {
    match TransportEnv::from_env() {
        Ok(TransportEnv::Enabled(_)) => eprintln!("child: enabled"),
        Ok(TransportEnv::Disabled) => eprintln!("child: disabled"),
        Err(error) => eprintln!("child: error {error:?}"),
    }
}

fn spawn_transport_env_child(
    vars: &[(&std::ffi::OsStr, std::ffi::OsString)],
) -> Result<String, std::io::Error> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command.args(["--ignored", "--exact", CHILD_TEST, "--nocapture"]);
    // 先清掉本文件其它用例可能留下的变量，保证子进程只看到本用例的输入。
    for key in [
        "RSETUP_TUNNEL_LISTEN",
        "RSETUP_CONTROLLER_KEY_FILE",
        "RSETUP_TUNNEL_MAX_HALF_OPEN",
        "RSETUP_TUNNEL_MAX_SESSIONS",
        "RSETUP_TUNNEL_DB_TIMEOUT_MS",
        "RSETUP_TUNNEL_HANDSHAKES_PER_SEC",
        "RSETUP_TUNNEL_MAX_PENDING_PER_SOURCE",
    ] {
        command.env_remove(key);
    }
    for (key, value) in vars {
        command.env(key, value);
    }
    let output = command.output()?;
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));
    Ok(text)
}

#[test]
fn env_missing_key_file_means_disabled_not_error() {
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _guard = EnvGuard::clear(&[
        "RSETUP_TUNNEL_LISTEN",
        "RSETUP_CONTROLLER_KEY_FILE",
        "RSETUP_TUNNEL_MAX_HALF_OPEN",
    ]);
    assert!(matches!(
        TransportEnv::from_env(),
        Ok(TransportEnv::Disabled)
    ));
    // 「不启用」不是配置错误：`TransportConfig::from_env` 才对缺失报错。
    let error = TransportConfig::from_env().expect_err("缺失即不可用");
    assert!(
        matches!(error, ListenerError::Config(_)),
        "缺失走 Config 语义，不是静默默认"
    );
}

#[test]
fn env_missing_key_file_with_valid_others_is_still_disabled() {
    // 审查 N5（正向）：key 缺失且**其它项全部合法**仍是合法的 Disabled——且其它项都**显式存在**。
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _guard = EnvGuard::clear(&["RSETUP_CONTROLLER_KEY_FILE"])
        .and(EnvGuard::set("RSETUP_TUNNEL_LISTEN", "127.0.0.1:0"))
        .and(EnvGuard::set("RSETUP_TUNNEL_MAX_HALF_OPEN", "8"))
        .and(EnvGuard::set("RSETUP_TUNNEL_MAX_SESSIONS", "16"));
    assert!(
        matches!(TransportEnv::from_env(), Ok(TransportEnv::Disabled)),
        "其它项合法 + key 缺失仍应 Disabled（不生成身份、不开监听）"
    );
}

#[test]
fn env_missing_key_file_does_not_swallow_illegal_numeric() {
    // 审查 N5：key 缺失**不得**把其它非法数值静默吞成 Disabled。旧实现 key 早返回 → Disabled。
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for (name, value) in [
        ("RSETUP_TUNNEL_MAX_HALF_OPEN", "0"),
        ("RSETUP_TUNNEL_MAX_HALF_OPEN", "not-a-number"),
        ("RSETUP_TUNNEL_MAX_SESSIONS", "100000000"),
        ("RSETUP_TUNNEL_DB_TIMEOUT_MS", "0"),
        ("RSETUP_TUNNEL_HANDSHAKES_PER_SEC", "0"),
    ] {
        let _guard =
            EnvGuard::clear(&["RSETUP_CONTROLLER_KEY_FILE", name]).and(EnvGuard::set(name, value));
        let error = TransportEnv::from_env().err().unwrap_or_else(|| {
            panic!("key 缺失但 {name}={value} 非法：必须报错，不得吞成 Disabled")
        });
        assert!(
            matches!(error, ListenerError::Config(_)),
            "{name}={value} 应走 Config 错误，实得 {error:?}"
        );
    }
}

#[test]
fn env_missing_key_file_does_not_swallow_illegal_listen() {
    // 审查 N5：key 缺失时非法监听地址同样必须报错，而不是先 `Disabled` 早返回把它吞掉。
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _guard = EnvGuard::clear(&["RSETUP_CONTROLLER_KEY_FILE"]).and(EnvGuard::set(
        "RSETUP_TUNNEL_LISTEN",
        "not-an-socket-address",
    ));
    let error = TransportEnv::from_env().unwrap_err();
    assert!(
        matches!(error, ListenerError::Config(_)),
        "key 缺失 + 非法 LISTEN 必须报错，不得吞成 Disabled；实得 {error:?}"
    );
    assert!(
        error.to_string().contains("socket address"),
        "应点名监听地址非法，实得 {error}"
    );
}

#[test]
fn env_missing_key_file_does_not_swallow_illegal_utf8() {
    // 审查 N5：key 缺失时其它变量的非 UTF-8 值同样不得被吞成 Disabled（子进程构造非 UTF-8）。
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let raw = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![b'7', 0xff, 0xfe])
    };
    // 子进程会先清掉 KEY_FILE（即 key 缺失），只留这一个非 UTF-8 数值项。
    let text =
        spawn_transport_env_child(&[(std::ffi::OsStr::new("RSETUP_TUNNEL_MAX_HALF_OPEN"), raw)])
            .expect("子进程可运行");
    assert!(
        !text.contains("disabled"),
        "key 缺失不得把非法 UTF-8 吞成 Disabled，实得输出：{text}"
    );
    assert!(
        text.contains("not valid UTF-8"),
        "非 UTF-8 必须报错，实得 {text}"
    );
    assert!(
        text.contains("RSETUP_TUNNEL_MAX_HALF_OPEN"),
        "必须点名是哪个变量：{text}"
    );
}

#[test]
fn env_empty_value_is_an_error_not_a_default() {
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("empty");
    let key = dir.join("identity.bin");
    let _guard = EnvGuard::set("RSETUP_CONTROLLER_KEY_FILE", &key.display().to_string())
        .and(EnvGuard::set("RSETUP_TUNNEL_MAX_HALF_OPEN", ""));
    let error = TransportEnv::from_env().expect_err("空串必须报错");
    let text = error.to_string();
    assert!(
        text.contains("empty"),
        "必须明确区分「设了但为空」与「没设」：{text}"
    );
    assert!(
        !text.contains(&key.display().to_string()),
        "错误文本不得回显路径：{text}"
    );
}

#[test]
fn env_zero_and_over_hard_bound_are_errors() {
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("bounds");
    let key = dir.join("identity.bin");
    let _guard = EnvGuard::set("RSETUP_CONTROLLER_KEY_FILE", &key.display().to_string());
    for (name, value) in [
        ("RSETUP_TUNNEL_MAX_HALF_OPEN", "0"),
        ("RSETUP_TUNNEL_MAX_HALF_OPEN", "not-a-number"),
        ("RSETUP_TUNNEL_MAX_HALF_OPEN", "1000000"),
        ("RSETUP_TUNNEL_MAX_SESSIONS", "0"),
        ("RSETUP_TUNNEL_DB_TIMEOUT_MS", "0"),
        ("RSETUP_TUNNEL_MAX_PENDING_PER_SOURCE", "0"),
        ("RSETUP_TUNNEL_HANDSHAKES_PER_SEC", "0"),
        ("RSETUP_TUNNEL_ADMISSION_READ_TIMEOUT_MS", "-1"),
    ] {
        let _value = EnvGuard::set(name, value);
        let error = TransportEnv::from_env()
            .err()
            .unwrap_or_else(|| panic!("{name}={value} 必须失败关闭"));
        let text = error.to_string();
        assert!(
            text.contains("bound")
                || text.contains("positive")
                || text.contains("non-zero")
                || text.contains("not a"),
            "{name}={value} 应给出可诊断的拒绝理由，实得 {text}"
        );
    }
}

#[test]
fn env_invalid_utf8_is_an_error_not_swallowed() {
    // 子进程路径：`OsString` 可以携带非 UTF-8 字节，父进程只断言输出（跨平台安全）。
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("notunicode");
    let key = dir.join("identity.bin");
    let raw = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![b'7', 0xff, 0xfe])
    };
    let text = spawn_transport_env_child(&[
        (
            std::ffi::OsStr::new("RSETUP_CONTROLLER_KEY_FILE"),
            std::ffi::OsString::from(&key.display().to_string()),
        ),
        (std::ffi::OsStr::new("RSETUP_TUNNEL_MAX_HALF_OPEN"), raw),
    ])
    .expect("子进程可运行");
    assert!(
        text.contains("not valid UTF-8"),
        "非 UTF-8 环境变量必须报错（旧实现 `.ok()` 吞掉 NotUnicode 当成未配置），实得 {text}"
    );
    assert!(
        text.contains("RSETUP_TUNNEL_MAX_HALF_OPEN"),
        "必须点名是哪个变量：{text}"
    );
}

#[test]
fn env_budget_is_the_single_source_for_manager_and_listener() {
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("single");
    let key = dir.join("identity.bin");
    let _guard = EnvGuard::set("RSETUP_CONTROLLER_KEY_FILE", &key.display().to_string())
        .and(EnvGuard::set("RSETUP_TUNNEL_MAX_HALF_OPEN", "7"))
        .and(EnvGuard::set("RSETUP_TUNNEL_MAX_SESSIONS", "9"))
        .and(EnvGuard::set("RSETUP_TUNNEL_LISTEN", "127.0.0.1:0"));
    let TransportEnv::Enabled(config) = TransportEnv::from_env().expect("合法配置") else {
        panic!("配了身份文件即启用");
    };
    // 审查 P1-5：监听侧与管理器共用**同一份**预算（旧实现监听解析完仍把
    // `ManagerConfig::default()` 交给管理器）。
    let manager = config.manager_config();
    assert_eq!(manager.max_half_open, 7);
    assert_eq!(manager.max_sessions, 9);
    assert_eq!(config.manager.max_half_open, manager.max_half_open);
    assert_eq!(config.connection_task_budget(), Some(16));
    assert_eq!(manager.connection_task_budget(), Some(16));
    manager.validate().expect("解析出的预算必须自洽");
    config.validate().expect("监听配置必须自洽");
    assert_ne!(
        (manager.max_half_open, manager.max_sessions),
        (
            ManagerConfig::default().max_half_open,
            ManagerConfig::default().max_sessions
        ),
        "配置必须真的被消费，不是解析完丢掉"
    );
    // §6.6 的下界：会话排空窗口本身 ≥5s，且监听宽限期不短于它。
    assert!(config.session.drain_timeout >= Duration::from_secs(5));
    assert!(config.drain_grace >= config.session.drain_timeout);
}

#[test]
fn drain_grace_shorter_than_session_drain_is_rejected() {
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("drain");
    let key = dir.join("identity.bin");
    let _guard = EnvGuard::set("RSETUP_CONTROLLER_KEY_FILE", &key.display().to_string());
    let TransportEnv::Enabled(mut config) = TransportEnv::from_env().expect("合法配置") else {
        panic!("配了身份文件即启用");
    };
    // 审查 P1-3：关停必须给足 GOAWAY 排空窗口；把宽限期压到排空窗口之下要直接报错。
    config.drain_grace = config.session.drain_timeout - Duration::from_millis(1);
    let error = config.validate().expect_err("宽限期不得短于会话排空窗口");
    assert!(error.to_string().contains("drain"));
}

#[test]
fn connection_task_budget_is_a_checked_sum() {
    let overflow = ManagerConfig {
        max_half_open: usize::MAX,
        max_sessions: usize::MAX,
        ..ManagerConfig::default()
    };
    assert_eq!(
        overflow.connection_task_budget(),
        None,
        "溢出必须如实 None（绝不 saturating_add 出一个看起来合法的假上界）"
    );
    let ok = ManagerConfig {
        max_half_open: 3,
        max_sessions: 4,
        ..ManagerConfig::default()
    };
    assert_eq!(ok.connection_task_budget(), Some(7));
}

// ---------------------------------------------------------------------------
// accept 侧有界资源 + 关停收敛（真实 loopback，握手停在读帧阶段）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn global_handshake_rate_limit_denies_new_connections() {
    let stats = Arc::new(ListenerStats::default());
    let (manager, config) = harness("rate", |config| {
        config.handshakes_per_sec = 1;
        config.handshake_burst = 1;
        config.max_pending_per_source = 64;
    });
    let (tx, rx) = watch::channel(false);
    let listener = tokio::spawn(run_transport_listener_with_stats(
        config,
        Arc::new(RejectPolicy) as Arc<dyn AdmissionPolicy>,
        Arc::clone(&manager),
        rx,
        Arc::clone(&stats),
    ));
    let address = bound(&stats).await;

    let mut sockets = Vec::new();
    for _ in 0..8 {
        sockets.push(TcpStream::connect(address).await.expect("连接监听"));
        // 不发送任何字节：握手停在读 ClientHello，连接任务持有名额直到被关停。
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let limited = stats
        .handshake_rate_limited
        .load(std::sync::atomic::Ordering::Acquire);
    assert!(
        limited >= 4,
        "桶容量 1、速率 1/s：8 次连接里至少 4 次必须因**全局握手速率**被拒，实得 {limited}"
    );
    assert_eq!(
        stats
            .source_quota_rejected
            .load(std::sync::atomic::Ordering::Acquire),
        0,
        "来源名额充足时不得混记为速率拒绝"
    );

    let _ = tx.send(true);
    // 关停：监听必须**自己返回**（内部已 await 全部连接任务），不是被外部 abort。
    tokio::time::timeout(Duration::from_secs(20), listener)
        .await
        .expect("监听必须在宽限期内自行收尾返回")
        .expect("任务不 panic")
        .expect("正常关停返回 Ok");
    assert_eq!(stats.tasks_active(), 0, "关停后不得残留连接任务");
    drop(sockets);
}

#[tokio::test]
async fn source_quota_denies_excess_unauthenticated_connections() {
    let stats = Arc::new(ListenerStats::default());
    let (manager, config) = harness("quota", |config| {
        // 速率放到最大，确保观察到的拒绝只可能来自来源名额。
        config.handshakes_per_sec = 100_000;
        config.handshake_burst = 100_000;
        config.max_pending_per_source = 2;
    });
    let (tx, rx) = watch::channel(false);
    let listener = tokio::spawn(run_transport_listener_with_stats(
        config,
        Arc::new(RejectPolicy) as Arc<dyn AdmissionPolicy>,
        Arc::clone(&manager),
        rx,
        Arc::clone(&stats),
    ));
    let address = bound(&stats).await;

    let mut sockets = Vec::new();
    for _ in 0..6 {
        sockets.push(TcpStream::connect(address).await.expect("连接监听"));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let rejected = stats
        .source_quota_rejected
        .load(std::sync::atomic::Ordering::Acquire);
    assert!(
        rejected >= 3,
        "同一来源（127.0.0.1）上限 2：6 次连接至少 3 次必须被来源名额拒绝，实得 {rejected}"
    );
    assert_eq!(
        stats
            .handshake_rate_limited
            .load(std::sync::atomic::Ordering::Acquire),
        0,
        "速率充足时不得混记为速率拒绝"
    );
    assert!(
        stats.accepted.load(std::sync::atomic::Ordering::Acquire) > 0,
        "限额只能拒新，已放行的连接照常处理"
    );

    let _ = tx.send(true);
    tokio::time::timeout(Duration::from_secs(20), listener)
        .await
        .expect("监听必须自行收尾")
        .expect("任务不 panic")
        .expect("正常关停返回 Ok");
    assert_eq!(stats.tasks_active(), 0);
    drop(sockets);
}

#[tokio::test]
async fn source_handshake_rate_limit_denies_new_connections() {
    // 审查补项：**真实 listener 接线**——按 `peer.ip` 的来源握手速率必须与全局速率、来源并发名额
    // **分开计数、分开原因**。这里全局速率与来源名额都放到很大，因此观察到的拒绝只可能来自**来源速率**。
    let stats = Arc::new(ListenerStats::default());
    let (manager, config) = harness("sourcerate", |config| {
        // 全局速率与来源名额都开大，把拒绝来源隔离到「来源速率」这一维。
        config.handshakes_per_sec = 100_000;
        config.handshake_burst = 100_000;
        config.max_pending_per_source = 64;
        // 来源速率：桶 1、速率 1/s。8 次同来源连接只够放行极少数。
        config.max_handshakes_per_source = 1;
        config.handshake_source_burst = 1;
    });
    let (tx, rx) = watch::channel(false);
    let listener = tokio::spawn(run_transport_listener_with_stats(
        config,
        Arc::new(RejectPolicy) as Arc<dyn AdmissionPolicy>,
        Arc::clone(&manager),
        rx,
        Arc::clone(&stats),
    ));
    let address = bound(&stats).await;

    let mut sockets = Vec::new();
    for _ in 0..8 {
        sockets.push(TcpStream::connect(address).await.expect("连接监听"));
        // 全部来自 127.0.0.1：握手停在读 ClientHello，连接任务持有到关停。
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let rate_limited = stats
        .source_rate_limited
        .load(std::sync::atomic::Ordering::Acquire);
    assert!(
        rate_limited >= 4,
        "来源速率桶 1、速率 1/s：8 次同来源连接至少 4 次必须被**来源速率**拒，实得 {rate_limited}"
    );
    assert_eq!(
        stats
            .handshake_rate_limited
            .load(std::sync::atomic::Ordering::Acquire),
        0,
        "全局速率充足时不得混记为全局速率拒"
    );
    assert_eq!(
        stats
            .source_quota_rejected
            .load(std::sync::atomic::Ordering::Acquire),
        0,
        "来源名额充足时不得混记为名额拒（来源 rate 与并发名额分开计数/原因）"
    );
    assert!(
        stats.accepted.load(std::sync::atomic::Ordering::Acquire) > 0,
        "来源速率只拒新，早期放行的连接照常处理"
    );

    let _ = tx.send(true);
    tokio::time::timeout(Duration::from_secs(20), listener)
        .await
        .expect("监听必须自行收尾")
        .expect("任务不 panic")
        .expect("正常关停返回 Ok");
    assert_eq!(stats.tasks_active(), 0);
    drop(sockets);
}

#[tokio::test]
async fn listener_reports_listen_address_failure_to_the_caller() {
    // 端口占用/无权限：必须把 `Err` 交回调用方（由顶层传播），而不是在 detach 任务里退出进程。
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("占位端口");
    let address: SocketAddr = taken.local_addr().expect("占位地址");
    // 同步装配（含环境变量），返回后再 `await`：不跨 `await` 持有环境变量锁。
    let (config, manager) = build_config("bindfail", |config| {
        config.listen = address;
    });
    let (_tx, rx) = watch::channel(false);
    let error = run_transport_listener_with_stats(
        config,
        Arc::new(RejectPolicy) as Arc<dyn AdmissionPolicy>,
        manager,
        rx,
        Arc::new(ListenerStats::default()),
    )
    .await
    .expect_err("端口被占用必须上抛");
    assert!(
        matches!(error, ListenerError::ListenAddress),
        "实得 {error:?}"
    );
}

// ---------------------------------------------------------------------------
// 审查 P1-7：ready 之前的取消/关停（真实握手 + 真实会话 driver，不等 15s probe 窗口）
//
// 对端是**真实板端握手**（`rsetup-tunnel::client_handshake`），握手成功后**完全静默**：不发
// h2 preface、不发首 probe，因此中控侧 `SessionHandle::wait_ready` 只能等满 `PROBE_WINDOW`
// （15s，rsetup-tunnel 常量）才会失败。监听跑生产 `run_transport_listener_with_stats`，
// 注入的 [`AdmissionPolicy`] 是策略注入点（生产注入 `SqlxAdmissionPolicy`），不是测试开关。
// ---------------------------------------------------------------------------

/// 放行**指定板端公钥**的准入策略（注入点；其余公钥拒绝）。
struct ApprovePolicy {
    expected: [u8; 32],
}

impl AdmissionPolicy for ApprovePolicy {
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        let expected = self.expected;
        Box::pin(async move {
            if peer.public_key != expected {
                return Ok(AdmissionDecision::Rejected(2));
            }
            Ok(AdmissionDecision::Approved)
        })
    }
}

/// 真实板端：完成握手后交回 socket，随后**一个字节都不发**。
///
/// 返回的 socket 必须被调用方持有（drop 即断开，就测不到「挂起在 ready 之前」了）。
async fn connect_silent_board(
    board: Arc<rsetup_tunnel::identity::IdentityKey>,
    controller_public_key: [u8; 32],
    address: SocketAddr,
) -> TcpStream {
    let config = rsetup_tunnel::handshake::ClientHandshakeConfig {
        identity: Arc::clone(&board),
        trusted_servers: vec![controller_public_key],
        descriptor: rsetup_protocol::handshake_msg::DeviceDescriptor::new(
            "integration",
            "listener-p1-7",
            "test",
            "test",
        ),
    };
    let stream = TcpStream::connect(address).await.expect("连接监听");
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        rsetup_tunnel::handshake::client_handshake(stream, &config),
    )
    .await
    .expect("板端握手必须在 10s 内完成")
    .expect("板端握手必须成功（准入已放行该公钥）");
    // 握手成功即进入「静默」：不写 h2 preface、不发首 probe ⇒ 中控侧永远等不到 ready。
    outcome.io
}

/// 轮询等待条件成立（同 `transport_manager_fixes` 的写法，本文件独立可用）。
async fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 审查 P1-7（撤销支）：首 probe 未 ready 时**撤销该身份**，连接任务必须立刻 `abort` +
/// **await** 会话任务并归还半开/挂起名额，而不是挂到 15s probe 窗口结束。
#[tokio::test]
async fn p1_7_revoke_during_wait_ready_ends_the_connection_immediately() {
    let stats = Arc::new(ListenerStats::default());
    let (config, manager, identity_path) = build_config_inner("p1_7_revoke", |_| {});
    let controller_public_key =
        rsetup_tunnel::identity::IdentityKey::load_or_generate(&identity_path)
            .expect("读回中控身份")
            .public_key();
    let board = Arc::new(
        rsetup_tunnel::identity::IdentityKey::load_or_generate(
            temp_dir("p1_7_board").join("identity.bin"),
        )
        .expect("板端身份"),
    );
    let board_public_key = board.public_key();

    let (tx, rx) = watch::channel(false);
    let listener = tokio::spawn(run_transport_listener_with_stats(
        config,
        Arc::new(ApprovePolicy {
            expected: board_public_key,
        }) as Arc<dyn AdmissionPolicy>,
        Arc::clone(&manager),
        rx,
        Arc::clone(&stats),
    ));
    let address = bound(&stats).await;
    let _board_socket = connect_silent_board(board, controller_public_key, address).await;
    // 客户端握手**已返回**（`ServerFinished` 已收到）⇒ 中控侧只可能停在 `wait_ready` 这一段；
    // 再让出一段远小于 15s 的时间，确保连接任务确实已经 park 在 ready 之前的 select 上。
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 前置状态：握手已过、挂起凭证已登记、半开名额仍被持有、且**从未 ready**。
    assert!(
        wait_until(Duration::from_secs(10), || manager.pending_count() == 1
            && manager.half_open_count() == 1)
        .await,
        "握手后应恰好挂起 1 条未 ready 的连接（pending=1 且半开=1）"
    );
    assert_eq!(stats.ready.load(std::sync::atomic::Ordering::Acquire), 0);
    assert_eq!(stats.tasks_active(), 1, "应恰有一条连接任务在途");

    // 撤销：`apply_revoke` 取消挂起凭证（生产里由 SQL 提交后的撤销通道驱动同一入口）。
    let started = std::time::Instant::now();
    let outcome = manager.apply_revoke(board_public_key);
    assert_eq!(outcome.cancelled_pending, 1, "必须恰好取消这条挂起握手");
    let settled = wait_until(Duration::from_secs(5), || {
        stats.tasks_active() == 0 && manager.pending_count() == 0 && manager.half_open_count() == 0
    })
    .await;
    let elapsed = started.elapsed();
    assert!(
        settled,
        "撤销后必须在 5s 内归还全部名额并结清连接任务（实测 {elapsed:?}，仍挂着＝只发信号未 await）"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "退出必须远早于 15s probe 窗口（实测 {elapsed:?}）"
    );
    assert_eq!(
        stats.ready.load(std::sync::atomic::Ordering::Acquire),
        0,
        "本用例全程不得出现 ready（否则测的就不是 ready 之前的取消）"
    );
    // 分支鉴别：若取消是打进**握手中的策略评估**，`recheck` 会以 `NeverTruth` 失败并计入
    // 策略拒绝。两者都为 0 ⇒ 本用例走的确实是 ready 之前的取消分支。
    assert_eq!(
        stats
            .handshake_policy_rejected
            .load(std::sync::atomic::Ordering::Acquire),
        0,
        "不得走握手内策略复核失败分支"
    );
    assert_eq!(
        stats
            .handshake_failed
            .load(std::sync::atomic::Ordering::Acquire),
        0,
        "不得走握手失败分支"
    );

    let _ = tx.send(true);
    tokio::time::timeout(Duration::from_secs(20), listener)
        .await
        .expect("监听必须自行收尾")
        .expect("任务不 panic")
        .expect("正常关停返回 Ok");
    assert_eq!(stats.tasks_active(), 0);
}

/// 审查 P1-7（关停支）：同一条挂起在 ready 之前的连接，**进程关停**同样必须在 probe 窗口
/// 之前把监听与连接任务全部结清（旧实现里 `wait_ready` 不与关停竞争，只能等 15s）。
#[tokio::test]
async fn p1_7_shutdown_during_wait_ready_returns_well_before_the_probe_window() {
    let stats = Arc::new(ListenerStats::default());
    let (config, manager, identity_path) = build_config_inner("p1_7_shutdown", |_| {});
    let controller_public_key =
        rsetup_tunnel::identity::IdentityKey::load_or_generate(&identity_path)
            .expect("读回中控身份")
            .public_key();
    let board = Arc::new(
        rsetup_tunnel::identity::IdentityKey::load_or_generate(
            temp_dir("p1_7_board2").join("identity.bin"),
        )
        .expect("板端身份"),
    );
    let board_public_key = board.public_key();

    let (tx, rx) = watch::channel(false);
    let listener = tokio::spawn(run_transport_listener_with_stats(
        config,
        Arc::new(ApprovePolicy {
            expected: board_public_key,
        }) as Arc<dyn AdmissionPolicy>,
        Arc::clone(&manager),
        rx,
        Arc::clone(&stats),
    ));
    let address = bound(&stats).await;
    let board_socket = connect_silent_board(board, controller_public_key, address).await;

    assert!(
        wait_until(Duration::from_secs(10), || manager.pending_count() == 1
            && stats.tasks_active() == 1)
        .await,
        "握手后应恰有一条未 ready 的挂起连接"
    );
    // 同撤销支：握手已返回，再让出 200ms（≪15s）让连接任务 park 在 ready 之前的 select 上。
    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = std::time::Instant::now();
    let _ = tx.send(true);
    // 15s 是 `PROBE_WINDOW` 硬窗口：关停若不与 `wait_ready` 竞争，这里就会等满它。
    let result = tokio::time::timeout(Duration::from_secs(12), listener)
        .await
        .expect("监听必须在 12s（< 15s probe 窗口）内自行返回");
    let elapsed = started.elapsed();
    result.expect("任务不 panic").expect("正常关停返回 Ok");
    assert!(
        elapsed < Duration::from_secs(12),
        "关停必须在 probe 窗口之前完成（实测 {elapsed:?}）"
    );
    assert_eq!(stats.tasks_active(), 0, "关停后不得残留连接任务");
    assert_eq!(manager.pending_count(), 0, "挂起名额必须归还");
    assert_eq!(manager.half_open_count(), 0, "半开名额必须归还");
    assert_eq!(
        stats.ready.load(std::sync::atomic::Ordering::Acquire),
        0,
        "本用例全程不得出现 ready"
    );
    // 分支鉴别：关停走的必须是 ready 之前的 `ShuttingDown` 分支，不是握手失败/策略复核分支。
    assert_eq!(
        stats
            .handshake_policy_rejected
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    assert_eq!(
        stats
            .handshake_failed
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    drop(board_socket);
}

///
/// 环境变量只在本函数内被读（`ENV_LOCK` 保证与其它 env 用例互斥），因此返回的元组里
/// 不携带任何守卫，调用方可以安全地跨 `await` 持有它。
fn build_config(
    tag: &str,
    tune: impl FnOnce(&mut TransportConfig),
) -> (TransportConfig, Arc<TransportManager>) {
    let (config, manager, _identity_path) = build_config_inner(tag, tune);
    (config, manager)
}

/// 同 [`build_config`]，额外交出**身份文件路径**：需要中控公钥的用例（真实板端握手）事后用
/// `IdentityKey::load_or_generate` 原样读回公钥，测试全程不自己落盘私钥。
fn build_config_inner(
    tag: &str,
    tune: impl FnOnce(&mut TransportConfig),
) -> (TransportConfig, Arc<TransportManager>, std::path::PathBuf) {
    let _serial = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir(tag);
    let key = dir.join("identity.bin");
    let _guard = EnvGuard::set("RSETUP_CONTROLLER_KEY_FILE", &key.display().to_string())
        .and(EnvGuard::set("RSETUP_TUNNEL_LISTEN", "127.0.0.1:0"));
    let TransportEnv::Enabled(mut config) = TransportEnv::from_env().expect("合法配置") else {
        panic!("配了身份文件即启用");
    };
    tune(&mut config);
    config.validate().expect("用例配置必须自洽");
    let manager = build_manager(&config.manager_config());
    (config, manager, key)
}

// ---------------------------------------------------------------------------
// 辅助
// ---------------------------------------------------------------------------

fn harness(
    tag: &str,
    tune: impl FnOnce(&mut TransportConfig),
) -> (Arc<TransportManager>, TransportConfig) {
    let (config, manager) = build_config(tag, tune);
    (manager, config)
}

fn build_manager(config: &ManagerConfig) -> Arc<TransportManager> {
    TransportManager::try_new(
        Arc::new(NeverTruth) as Arc<dyn AdmissionTruth>,
        Arc::new(SnapshotStore::new()),
        Arc::new(FailingRecovery),
        Arc::new(FixedTime),
        *config,
    )
    .expect("管理器预算合法")
}

async fn bound(stats: &ListenerStats) -> SocketAddr {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Some(address) = stats.bound() {
            return address;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("监听未在期限内公布绑定地址");
}

/// 身份密钥目录必须是 `0700`（`rsetup-tunnel::identity` 会拒绝更宽的 mode），
/// 因此这里显式建目录并设权限，而不是依赖 umask。
fn temp_dir(tag: &str) -> std::path::PathBuf {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let unique = Uuid::new_v4();
    let dir = std::env::temp_dir()
        .join(format!("rsetup-listener-{tag}-{unique}"))
        .join("keys");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .expect("临时目录");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("0700");
    dir
}

/// 环境变量守卫（RAII 还原）：环境变量是进程级状态，用例必须串行（`ENV_LOCK`）。
struct EnvGuard {
    entries: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    /// 记录原值后写入新值。
    fn apply(&mut self, key: &'static str, value: Option<std::ffi::OsString>) {
        let previous = std::env::var_os(key);
        if !self.entries.iter().any(|(name, _)| *name == key) {
            self.entries.push((key, previous));
        }
        // 环境变量写入是进程级 unsafe（Rust 2024）：本文件所有用例都由 `ENV_LOCK` 串行化。
        unsafe {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    fn set(key: &'static str, value: &str) -> Self {
        let mut guard = EnvGuard {
            entries: Vec::new(),
        };
        guard.apply(key, Some(std::ffi::OsString::from(value)));
        guard
    }

    fn clear(keys: &[&'static str]) -> Self {
        let mut guard = EnvGuard {
            entries: Vec::new(),
        };
        for key in keys {
            guard.apply(key, None);
        }
        guard
    }

    fn and(mut self, mut other: EnvGuard) -> Self {
        // 逐项搬走（不是 move）：搬空后 `other` 的 `Drop` 不会提前还原变量。
        while let Some(entry) = other.entries.pop() {
            if !self.entries.iter().any(|(name, _)| *name == entry.0) {
                self.entries.push(entry);
            }
        }
        self
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, previous) in self.entries.iter() {
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

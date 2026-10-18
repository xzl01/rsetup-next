//! 集成验收 Task 2：运行期门禁装配测试与双进程真实 DB smoke 框架。
//!
//! 两轨严格分轨（计划 §Task 2 Step 2）：
//! - [`gate_blocks_business_before_readiness`]：普通离线环境运行的门禁纯测试。
//!   以 `(time_ready, db_ready, recovery_ready)` 五组状态矩阵，逐个证明「任一单项门禁
//!   关闭都独立阻断 `/readyz` 与两条业务路由、业务网关零调用」，以及三门禁全放行后
//!   真正放行且业务调用计数如实递增的装配契约；不连接真实数据库、不访问网络、
//!   不做真实 NTP 采样。
//! - [`two_process_real_db_smoke`]：完整真实 DB 双进程 smoke 骨架，恒 `#[ignore]`，
//!   仅在操作员核验物理隔离、真实备份与授权隔离库后，经显式
//!   `--ignored --exact --test-threads=1` 单 case 串行命令执行；生产接线未就绪时
//!   显式失败，绝不伪造通过。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use tower::ServiceExt;

use rsetup_controller::time::gate::TokioSleeper;
use rsetup_controller::time::socket::NtpSample;
use rsetup_controller::time::{
    NullTimeAuditSink, StaticSourceList, SystemWallClock, TimeGateConfig, TimeGateDeps,
    TimeGateHandle, TimeSampleFuture, TimeSampler, start_time_gate_with,
};

// ---------------------------------------------------------------------------
// 门禁纯测试（离线装配）
// ---------------------------------------------------------------------------

/// 可编程时间采样替身（仅内存门禁装配使用）：首轮采样阻塞直到测试显式放行，
/// 随后返回合格样本，由生产 `TimeGateDriver` 转 `NtpValid` 并置 ready。
/// 全程不访问网络；时间门禁的 ready 语义（初始未 ready、合格样本后 ready 且
/// 不回退）完全由生产 `TimeGateHandle`/`TimeGateDriver` 承接，非测试自造。
struct ControlledTimeSampler {
    release: Arc<tokio::sync::watch::Receiver<bool>>,
}

impl TimeSampler for ControlledTimeSampler {
    fn sample(&self, _servers: &[String]) -> TimeSampleFuture<'_> {
        let mut release = (*self.release).clone();
        Box::pin(async move {
            while !*release.borrow_and_update() {
                if release.changed().await.is_err() {
                    // 放行侧已 drop：本轮失败，driver 按契约退避重试，门禁保持不放行。
                    return None;
                }
            }
            Some((
                NtpSample {
                    offset_ms: 0,
                    rtt_ms: 1,
                    reference_utc: chrono::Utc::now(),
                    uncertainty_ms: 1,
                },
                "fake.acceptance.local".to_string(),
            ))
        })
    }
}

/// 内存就绪门禁开关：生产装配中由 DB 只读版本检查与恢复流程完成后打开；
/// 离线装配测试由测试显式打开。本测试不伪造 DB 检查本身通过，只证明
/// 「门禁状态决定路由可用性」的装配契约。
#[derive(Clone)]
struct GateSwitch(Arc<AtomicBool>);

impl GateSwitch {
    fn closed() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    fn is_open(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn open(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// 运行期装配上下文：生产时间门禁句柄 + DB/恢复就绪门禁 + 业务调用计数。
#[derive(Clone)]
struct AssemblyState {
    time_gate: TimeGateHandle,
    db_ready: GateSwitch,
    recovery_ready: GateSwitch,
    business_calls: Arc<AtomicUsize>,
}

impl AssemblyState {
    fn all_gates_open(&self) -> bool {
        self.time_gate.is_ready() && self.db_ready.is_open() && self.recovery_ready.is_open()
    }
}

/// 固定 503 `NOT_READY` 响应，形状对齐生产 `http_gate::not_ready` 的对外契约
/// （该函数为 crate 私有，集成测试按同形契约断言）。
fn not_ready_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": { "code": "NOT_READY", "message_key": "system.not_ready" }
        })),
    )
        .into_response()
}

/// 存活探活：不受就绪门禁阻断（与生产 `/healthz` 语义一致）。
async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// 就绪探活：三门禁（时间 / DB / 恢复）全部放行前不得返回 200。
async fn readyz(State(state): State<AssemblyState>) -> Response {
    if state.all_gates_open() {
        StatusCode::OK.into_response()
    } else {
        not_ready_response()
    }
}

/// 业务网关替身：只统计真实到达的业务调用，供「门禁关闭期间零业务调用」断言。
async fn tasks_list(State(state): State<AssemblyState>) -> Response {
    state.business_calls.fetch_add(1, Ordering::SeqCst);
    (StatusCode::OK, Json(serde_json::json!({ "items": [] }))).into_response()
}

async fn device_status(
    State(state): State<AssemblyState>,
    Path(_device_id): Path<String>,
) -> Response {
    state.business_calls.fetch_add(1, Ordering::SeqCst);
    (StatusCode::OK, Json(serde_json::json!({ "state": "idle" }))).into_response()
}

/// 业务路由就绪门禁中间件：任一门禁未放行即在触达业务网关前返回固定 503。
async fn readiness_gate(
    State(state): State<AssemblyState>,
    request: Request,
    next: Next,
) -> Response {
    if state.all_gates_open() {
        next.run(request).await
    } else {
        not_ready_response()
    }
}

/// 装配上下文：生产 `start_time_gate_with` 时间门禁（fake 采样、零网络）+
/// 内存 DB/恢复门禁 + 「探活（未包裹）/业务（门禁中间件包裹）」顶层 router。
struct RuntimeAssembly {
    state: AssemblyState,
    app: Router,
    release: Arc<tokio::sync::watch::Sender<bool>>,
}

fn assemble_runtime() -> RuntimeAssembly {
    let (release_tx, release_rx) = tokio::sync::watch::channel(false);
    let config = TimeGateConfig {
        servers: vec!["ntp.acceptance.invalid".to_string()],
        ..TimeGateConfig::default()
    };
    let deps = TimeGateDeps {
        sampler: Arc::new(ControlledTimeSampler {
            release: Arc::new(release_rx),
        }),
        sources: Arc::new(StaticSourceList::new(config.servers.clone())),
        wall_clock: Arc::new(SystemWallClock),
        sleeper: Arc::new(TokioSleeper),
        audit: Arc::new(NullTimeAuditSink),
    };
    let time_gate = start_time_gate_with(config, uuid::Uuid::new_v4(), deps);
    let state = AssemblyState {
        time_gate,
        db_ready: GateSwitch::closed(),
        recovery_ready: GateSwitch::closed(),
        business_calls: Arc::new(AtomicUsize::new(0)),
    };
    let business = Router::new()
        .route("/api/v1/tasks", get(tasks_list))
        .route("/api/v1/devices/{device_id}/status", get(device_status))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            readiness_gate,
        ))
        .with_state(state.clone());
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .merge(business)
        .with_state(state.clone());
    RuntimeAssembly {
        state,
        app,
        release: Arc::new(release_tx),
    }
}

impl RuntimeAssembly {
    /// 放行受控时间采样，并有界等待生产 driver 真正置 ready（超时即失败）。
    async fn release_time_gate(&self) {
        self.release.send(true).expect("时间门禁放行信号必须可送达");
        wait_until_time_gate_ready(&self.state.time_gate).await;
    }
}

/// 两条受就绪门禁保护的业务路由（任务列表 / 设备状态）。
const BUSINESS_PATHS: [&str; 2] = [
    "/api/v1/tasks",
    "/api/v1/devices/0123456789abcdef0123456789abcdef/status",
];

/// 按指定 DB / 恢复门禁开关装配一份全新实例；时间门禁默认未 ready，
/// 需要放行时由调用方显式 [`RuntimeAssembly::release_time_gate`]。
fn assemble_with(db_ready: bool, recovery_ready: bool) -> RuntimeAssembly {
    let assembly = assemble_runtime();
    if db_ready {
        assembly.state.db_ready.open();
    }
    if recovery_ready {
        assembly.state.recovery_ready.open();
    }
    assembly
}

async fn get_response(app: &Router, uri: &str) -> (StatusCode, String) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn assert_not_ready(app: &Router, uri: &str, context: &str) {
    let (status, body) = get_response(app, uri).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{context}：{uri}");
    let value: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["error"]["code"], "NOT_READY", "{context}：{uri}");
    assert_eq!(
        value["error"]["message_key"], "system.not_ready",
        "{context}：{uri}"
    );
}

/// 断言该门禁状态下 `/readyz` 与两条业务路由全部受控不可用（固定 503 `NOT_READY`），
/// 且业务网关调用恒零：每个负例都用独立装配实例断言，互不借用彼此的放行进度。
async fn assert_all_business_blocked(assembly: &RuntimeAssembly, context: &str) {
    assert_not_ready(&assembly.app, "/readyz", context).await;
    for uri in BUSINESS_PATHS {
        assert_not_ready(&assembly.app, uri, context).await;
    }
    assert_eq!(
        assembly.state.business_calls.load(Ordering::SeqCst),
        0,
        "{context}：门禁关闭期间业务网关调用必须恒零"
    );
}

/// 有界等待时间门禁 ready；超时即失败，绝不把超时兜底误当放行。
async fn wait_until_time_gate_ready(handle: &TimeGateHandle) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !handle.is_ready() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "时间门禁在放行后 5s 内未转 ready"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 三门禁状态矩阵：状态记为 `(time_ready, db_ready, recovery_ready)`。
/// 4 个负例各用独立装配实例，逐个证明「仅该门禁关闭」也足以阻断 `/readyz` 与
/// 两条业务路由且业务网关零调用；正例证明全放行后真正可达且计数如实递增。
#[tokio::test]
async fn gate_blocks_business_before_readiness() {
    // 负例 1（000 全关）：/healthz 存活探活不受就绪门禁阻断，其余全部 503。
    let closed_all = assemble_with(false, false);
    assert!(
        !closed_all.state.time_gate.is_ready(),
        "刚装配的时间门禁必须未 ready"
    );
    let (status, _) = get_response(&closed_all.app, "/healthz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "存活探活不得被就绪门禁阻断（与生产 /healthz 语义一致）"
    );
    assert_all_business_blocked(&closed_all, "负例 1（000 三门禁全关）").await;

    // 负例 2（011 仅时间门禁关闭）：DB 与恢复均已放行，时间门禁仍关 → 必须仍全阻断。
    // 若装配漏接 time_gate.is_ready()，本负例立即失败。
    let time_only_closed = assemble_with(true, true);
    assert!(
        !time_only_closed.state.time_gate.is_ready(),
        "未放行的时间门禁必须未 ready"
    );
    assert!(
        time_only_closed.state.db_ready.is_open()
            && time_only_closed.state.recovery_ready.is_open(),
        "负例 2 前置：DB 与恢复门禁必须已放行"
    );
    assert_all_business_blocked(&time_only_closed, "负例 2（011 仅时间门禁关闭）").await;

    // 负例 3（101 仅 DB 门禁关闭）：时间已 ready、恢复已放行，DB 仍关 → 必须仍全阻断。
    // 若装配漏接 db_ready.is_open()，本负例立即失败。
    let db_only_closed = assemble_with(false, true);
    db_only_closed.release_time_gate().await;
    assert!(
        !db_only_closed.state.db_ready.is_open() && db_only_closed.state.recovery_ready.is_open(),
        "负例 3 前置：时间已 ready、恢复已放行、DB 仍关"
    );
    assert_all_business_blocked(&db_only_closed, "负例 3（101 仅 DB 门禁关闭）").await;

    // 负例 4（110 仅恢复门禁关闭）：时间已 ready、DB 已放行，恢复仍关 → 必须仍全阻断。
    // 保留原有「仅恢复未放行」的独立阻断证明。
    let recovery_only_closed = assemble_with(true, false);
    recovery_only_closed.release_time_gate().await;
    assert!(
        recovery_only_closed.state.db_ready.is_open()
            && !recovery_only_closed.state.recovery_ready.is_open(),
        "负例 4 前置：时间已 ready、DB 已放行、恢复仍关"
    );
    assert_all_business_blocked(&recovery_only_closed, "负例 4（110 仅恢复门禁关闭）").await;

    // 正例（111 全放行）：/readyz 200，两条业务真正可达且计数精确递增到 2。
    let all_open = assemble_with(true, true);
    all_open.release_time_gate().await;
    assert!(
        all_open.state.time_gate.is_ready()
            && all_open.state.db_ready.is_open()
            && all_open.state.recovery_ready.is_open(),
        "正例前置：三门禁必须全部放行"
    );
    let (status, _) = get_response(&all_open.app, "/readyz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "全部门禁放行后 /readyz 必须返回 200"
    );
    for uri in BUSINESS_PATHS {
        let (status, _) = get_response(&all_open.app, uri).await;
        assert_eq!(status, StatusCode::OK, "全部门禁放行后 {uri} 必须可达");
    }
    assert_eq!(
        all_open.state.business_calls.load(Ordering::SeqCst),
        BUSINESS_PATHS.len(),
        "放行后业务调用必须恰好两条到达网关"
    );
}

// ---------------------------------------------------------------------------
// 完整真实 DB 双进程 smoke 框架（恒 #[ignore]，显式授权串行执行）
// ---------------------------------------------------------------------------

/// 执行命令（操作员核验物理隔离、真实备份与授权隔离库后，单 case 显式串行）：
///
/// ```text
/// cargo test -p rsetup-controller --test acceptance_runtime two_process_real_db_smoke \
///   -- --ignored --exact --test-threads=1
/// ```
///
/// 计划步骤（生产接线或授权未就绪时显式失败，绝不伪造通过）：
/// 1. 从环境读取已授权隔离库目标与真实备份引用（`CONTROLLER_TEST_DATABASE_URL` /
///    `CONTROLLER_TEST_BACKUP_REF` / `CONTROLLER_TEST_MIGRATION_ACK`），只验存在、不回显值；
/// 2. spawn 中控（`rsetup-controller` 二进制）与独立板端测试进程两个真实进程，
///    断言两个独立 PID 与真实 TCP 连接，不以库内 loopback 或 tower oneshot 冒充独立进程；
/// 3. 受控登录 → 待审准入/批准写事务 → Control/Data 双流 probe → 只读 RPC
///    （`capabilities`/`status`/`clock`/`task.get`）→ 恢复锁检查，各自有效 pong；
/// 4. 测试板端进程注入 `RebootOs` 安全替身，断言宿主机 reboot 调用为零；
/// 5. 掉线/中控重启后存在 dispatching 意图且结果未知时不重发 execute；
/// 6. 失败必须是缺生产接线/真实门禁的错误，严禁注入「只读 DB fake」或 fake 仓储
///    冒充初始化、登录写入、准入提交与持久任务恢复的完整事务链路。
#[tokio::test]
#[ignore = "requires operator confirmed physical isolation and backup"]
async fn two_process_real_db_smoke() {
    let gates_present = std::env::var_os("CONTROLLER_TEST_DATABASE_URL").is_some()
        && std::env::var_os("CONTROLLER_TEST_BACKUP_REF").is_some()
        && std::env::var_os("CONTROLLER_TEST_MIGRATION_ACK").is_some();
    assert!(
        gates_present,
        "缺少已授权隔离库、真实备份引用或迁移确认门禁；smoke 不得隐式运行（不回显任何目标值）"
    );
    unimplemented!(
        "双进程真实 DB smoke 的生产接线（可注入进程启动 fixture、板端二进制、准入/恢复写事务）尚未就绪；依赖未齐备时本 case 必须显式失败，不得以 fake 替代"
    );
}

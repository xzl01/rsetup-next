//! 集成验收 Task 2：运行期门禁装配测试与双进程真实 DB smoke 框架。
//!
//! 两轨严格分轨（计划 §Task 2 Step 2）：
//! - [`gate_blocks_business_before_readiness`]：普通离线环境运行的门禁纯测试。
//!   只证明「时间 / DB / 恢复门禁未放行前 `/readyz` 与业务路由不可用、全部放行后可用」
//!   的装配契约；不连接真实数据库、不访问网络、不做真实 NTP 采样。
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

#[tokio::test]
async fn gate_blocks_business_before_readiness() {
    let assembly = assemble_runtime();
    let app = assembly.app.clone();
    let device_path = "/api/v1/devices/0123456789abcdef0123456789abcdef/status";

    // 1. 三门禁全关：/readyz 必须非 200；/healthz 存活探活不受就绪门禁阻断。
    assert!(
        !assembly.state.time_gate.is_ready(),
        "刚装配的时间门禁必须未 ready"
    );
    let (status, _) = get_response(&app, "/healthz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "存活探活不得被就绪门禁阻断（与生产 /healthz 语义一致）"
    );
    assert_not_ready(&app, "/readyz", "三门禁全关").await;

    // 2. 业务路由受控不可用：固定 503 且业务网关零调用。
    assert_not_ready(&app, "/api/v1/tasks", "三门禁全关").await;
    assert_not_ready(&app, device_path, "三门禁全关").await;
    assert_eq!(
        assembly.state.business_calls.load(Ordering::SeqCst),
        0,
        "门禁关闭期间业务网关调用必须恒零"
    );

    // 3. 仅放行 DB 门禁：时间与恢复仍关，/readyz 与业务仍必须非 200。
    assembly.state.db_ready.open();
    assert_not_ready(&app, "/readyz", "仅 DB 门禁放行").await;
    assert_not_ready(&app, "/api/v1/tasks", "仅 DB 门禁放行").await;

    // 4. 再放行时间门禁（生产 driver 转 ready）：恢复门禁仍关，仍不得放行。
    assembly
        .release
        .send(true)
        .expect("时间门禁放行信号必须可送达");
    wait_until_time_gate_ready(&assembly.state.time_gate).await;
    assert_not_ready(&app, "/readyz", "恢复门禁仍关").await;
    assert_not_ready(&app, "/api/v1/tasks", "恢复门禁仍关").await;
    assert_eq!(
        assembly.state.business_calls.load(Ordering::SeqCst),
        0,
        "恢复门禁关闭期间业务网关调用必须恒零"
    );

    // 5. 放行恢复门禁：/readyz 返回 200，业务真正可达且计数如实递增。
    assembly.state.recovery_ready.open();
    let (status, _) = get_response(&app, "/readyz").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "全部门禁放行后 /readyz 必须返回 200"
    );
    let (status, _) = get_response(&app, "/api/v1/tasks").await;
    assert_eq!(status, StatusCode::OK, "全部门禁放行后任务业务必须可达");
    let (status, _) = get_response(&app, device_path).await;
    assert_eq!(status, StatusCode::OK, "全部门禁放行后设备业务必须可达");
    assert_eq!(
        assembly.state.business_calls.load(Ordering::SeqCst),
        2,
        "放行后业务调用必须恰好两次到达网关"
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

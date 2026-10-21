//! 可信时间消费接线（RT-N5 缺口闭环）离线行为测试。
//!
//! 三组契约，全部零公网 NTP、零真实数据库、零真实 30s/60s 等待：
//!
//! 1. **消费入口**：`SqlxTaskApi::submit` 与 `SqlxTaskApi::time_review` 每次调用都从
//!    **同一个注入的 [`TaskTimeSource`]** 读一次当前证据（spy 计数 + 两次调用之间改值
//!    证明适配器不缓存）；四入参兼容构造 `SqlxTaskApi::new` 仍等价于注入
//!    [`SystemFallbackTimeSource`]，既有 fallback 行为不变。
//! 2. **保真与不伪造**：`NtpValid` 证据逐字段原样进入 `SubmitRequest.time_evidence`
//!    与 `time_review` 的比对时钟；fallback / 从未采样 / 墙钟 1970 / 纪元不连续一律
//!    不把质量抬成 `NtpValid`，生产纯策略 `time_review_verdict` 保持 `Unverifiable`
//!    （对应服务层 `Inconclusive` + 零状态变更），可信同纪元证据则确实进入 `Resume`。
//! 3. **动态降级**：同一个 `TimeGateHandle` 在样本超龄、墙钟回跳换代后，下一次读取
//!    立即反映降级/新纪元，绝不把「一次 `NtpValid`」当成永久有效。
//!
//! 数据库边界（诚实界定）：`submit` / `time_review` 的事务写入属生产 SQL 路径，本文件
//! 沿用仓库既有离线装配惯例（`connect_lazy` 指向 `127.0.0.1:1` 的关闭端口，永不触达
//! 任何数据库服务），因此只证明**时钟读取发生在写库之前且错误确实来自存储层**，
//! 不声称已实测 `main_tasks.created_time_json` 落库与事务内复核行为。

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use rsetup_controller::auth::service::{IdentityUser, Session};
use rsetup_controller::authz::sqlx_repo::SqlxAuthorizationSnapshotRepository;
use rsetup_controller::tasks::SqlxTaskApi;
use rsetup_controller::tasks::adapter::{
    SystemFallbackTimeSource, TaskTimeSource, TimeEvidenceFuture, prepare_submit_request,
    prepare_time_review_clock,
};
use rsetup_controller::tasks::http::{TaskApi, TimeReviewDecision, TimeReviewTaskRequest};
use rsetup_controller::tasks::preview::new_preview_token;
use rsetup_controller::tasks::repository::SqlxTaskRepository;
use rsetup_controller::tasks::scheduler::OBSERVE_WINDOW_MS;
use rsetup_controller::tasks::service::{
    QUEUE_TTL_MS, TaskService, TimeReviewDecision as ServiceDecision, TimeReviewEvidenceKind,
    TimeReviewVerdict, queue_deadline_of, time_review_verdict,
};
use rsetup_controller::time::socket::NtpSample;
use rsetup_controller::time::{
    NullTimeAuditSink, SleepFuture, Sleeper, StaticSourceList, TimeEvidence, TimeGateConfig,
    TimeGateDeps, TimeGateDriver, TimeGateHandle, TimeQuality, TimeSampleFuture, TimeSampler,
    WallClock,
};
use rsetup_controller::{ControllerError, DbPool};

const CURSOR_SECRET: [u8; 32] = [0x5Au8; 32];
const TRUSTED_SOURCE: &str = "fake.trusted-consumption.local";

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

fn fixed_now() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("fixed instant in range")
}

/// 可信证据夹具：字段全部非零且互不相同，任何「丢字段/抬质量」的改动都会被抓到。
fn valid_evidence(clock_epoch: Uuid, at: DateTime<Utc>, age_ms: u64) -> TimeEvidence {
    TimeEvidence {
        system_wall_utc: at,
        reference_utc: at,
        quality: TimeQuality::NtpValid,
        clock_epoch,
        source: Some(TRUSTED_SOURCE.to_owned()),
        sample_age_ms: Some(age_ms),
        offset_ms: Some(-42),
        uncertainty_ms: Some(7),
    }
}

/// 不可信证据夹具（`SystemFallback`）：无来源、无偏差、无年龄。
fn fallback_evidence(clock_epoch: Uuid, at: DateTime<Utc>) -> TimeEvidence {
    TimeEvidence {
        system_wall_utc: at,
        reference_utc: at,
        quality: TimeQuality::SystemFallback,
        clock_epoch,
        source: None,
        sample_age_ms: None,
        offset_ms: None,
        uncertainty_ms: None,
    }
}

fn hour_later() -> Option<DateTime<Utc>> {
    Utc::now().checked_add_signed(chrono::Duration::hours(1))
}

/// 可编程时间源替身：记录读取次数，返回测试显式设定的证据（对象安全注入面）。
struct SpyTimeSource {
    reads: AtomicUsize,
    evidence: Mutex<TimeEvidence>,
}

impl SpyTimeSource {
    fn new(evidence: TimeEvidence) -> Arc<Self> {
        Arc::new(Self {
            reads: AtomicUsize::new(0),
            evidence: Mutex::new(evidence),
        })
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn set(&self, evidence: TimeEvidence) {
        *self.evidence.lock().expect("spy lock") = evidence;
    }
}

impl TaskTimeSource for SpyTimeSource {
    fn current_evidence(&self) -> TimeEvidenceFuture<'_> {
        Box::pin(async move {
            let evidence = self.evidence.lock().expect("spy lock").clone();
            self.reads.fetch_add(1, Ordering::SeqCst);
            evidence
        })
    }
}

/// 脚本化采样器：按序弹出样本（`None` = 本轮失败），队列耗尽后恒 `None`。
struct ScriptedSampler {
    queue: Mutex<VecDeque<Option<NtpSample>>>,
    source: &'static str,
}

impl ScriptedSampler {
    fn new(samples: Vec<Option<NtpSample>>, source: &'static str) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(samples.into_iter().collect()),
            source,
        })
    }
}

impl TimeSampler for ScriptedSampler {
    fn sample(&self, _servers: &[String]) -> TimeSampleFuture<'_> {
        let next = self.queue.lock().expect("sampler lock").pop_front();
        let source = self.source;
        Box::pin(async move { next.flatten().map(|sample| (sample, source.to_owned())) })
    }
}

/// 跟随可编程墙钟的合格采样器：用于「只有墙钟跳动能改变参考系」的跳钟用例。
struct WallFollowSampler {
    wall: Arc<ControlledWallClock>,
    source: &'static str,
}

impl TimeSampler for WallFollowSampler {
    fn sample(&self, _servers: &[String]) -> TimeSampleFuture<'_> {
        let reference = DateTime::<Utc>::from(self.wall.now());
        let source = self.source;
        Box::pin(async move {
            Some((
                NtpSample {
                    offset_ms: 0,
                    rtt_ms: 1,
                    reference_utc: reference,
                    uncertainty_ms: 1,
                },
                source.to_owned(),
            ))
        })
    }
}

/// 恒失败采样器：本进程从未取得合格样本（fallback 语义，绝不伪造样本）。
struct AlwaysNoneSampler;

impl TimeSampler for AlwaysNoneSampler {
    fn sample(&self, _servers: &[String]) -> TimeSampleFuture<'_> {
        Box::pin(async move { None })
    }
}

/// 可编程墙钟（driver 契约 4 的可注入点）。
struct ControlledWallClock(Mutex<SystemTime>);

impl ControlledWallClock {
    fn at(now: SystemTime) -> Arc<Self> {
        Arc::new(Self(Mutex::new(now)))
    }

    fn set(&self, now: SystemTime) {
        *self.0.lock().expect("wall lock") = now;
    }
}

impl WallClock for ControlledWallClock {
    fn now(&self) -> SystemTime {
        *self.0.lock().expect("wall lock")
    }
}

/// 短等待替身：真实等待被封顶到 5ms（本仓库 tokio 未启用 `test-util`），
/// 断言只看「是否发生/顺序」，不依赖真实退避与复验节奏。
struct ShortSleeper;

impl Sleeper for ShortSleeper {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        let capped = duration.min(Duration::from_millis(5));
        Box::pin(async move { tokio::time::sleep(capped).await })
    }
}

/// 受控门禁装配：与生产 `start_time_gate_with` 同一条 `TimeGateDriver::run()` 路径，
/// 只用公开的 `with_ready_timeout` 把就绪等待上限压到测试尺度（生产默认 30s 不变）。
fn spawn_gate(
    config: TimeGateConfig,
    deps: TimeGateDeps,
    clock_epoch: Uuid,
    ready_timeout: Duration,
) -> TimeGateHandle {
    let handle = TimeGateHandle::new(&config, clock_epoch).with_ready_timeout(ready_timeout);
    let driver = TimeGateDriver::new(&handle, config, deps);
    tokio::spawn(driver.run());
    handle
}

fn deps_with(
    sampler: Arc<dyn TimeSampler>,
    wall_clock: Arc<ControlledWallClock>,
    servers: Vec<String>,
) -> TimeGateDeps {
    TimeGateDeps {
        sampler,
        sources: Arc::new(StaticSourceList::new(servers)),
        wall_clock,
        sleeper: Arc::new(ShortSleeper),
        audit: Arc::new(NullTimeAuditSink),
    }
}

fn gate_config(max_evidence_age_ms: u64) -> TimeGateConfig {
    TimeGateConfig {
        servers: vec![TRUSTED_SOURCE.to_owned()],
        revalidate_interval: Duration::from_millis(5),
        max_evidence_age_ms,
        ..TimeGateConfig::default()
    }
}

/// 轮询直到门禁快照换到新纪元（跳钟换代），超时即失败。
async fn wait_until_clock_epoch_changes(handle: &TimeGateHandle, previous: Uuid) -> TimeEvidence {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let evidence = prepare_time_review_clock(handle).await;
        if evidence.clock_epoch != previous {
            return evidence;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "墙钟回跳后 driver 未换代：clock_epoch 仍为 {previous}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 离线池：`connect_lazy` 到已关闭端口，永不触达任何数据库服务（仓库既有惯例）。
/// 只用于证明「时钟读取发生在存储访问之前」，不用于任何 SQL 行为断言。
fn offline_pool() -> DbPool {
    DbPool(
        sqlx::mysql::MySqlPoolOptions::new()
            // 短获取超时：关闭端口的连接尝试本身在本机即时失败，这里只是兜底，
            // 避免任何环境下把测试拖成长等待（断言只依赖「错误来自存储层」）。
            .acquire_timeout(Duration::from_millis(1_500))
            .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/rsetup_time_consumption")
            .expect("lazy pool 不需要运行时连接"),
    )
}

fn api_with_source(time: Arc<dyn TaskTimeSource>) -> SqlxTaskApi {
    let db = offline_pool();
    SqlxTaskApi::with_time_source(
        Arc::new(TaskService::new(SqlxTaskRepository::new(db.0.clone()))),
        Arc::new(SqlxTaskRepository::new(db.0.clone())),
        Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone())),
        CURSOR_SECRET,
        time,
    )
}

fn api_with_fallback_clock() -> SqlxTaskApi {
    let db = offline_pool();
    SqlxTaskApi::new(
        Arc::new(TaskService::new(SqlxTaskRepository::new(db.0.clone()))),
        Arc::new(SqlxTaskRepository::new(db.0.clone())),
        Arc::new(SqlxAuthorizationSnapshotRepository::new(db.clone())),
        CURSOR_SECRET,
    )
}

fn actor() -> Session {
    Session {
        user: IdentityUser {
            id: [0x22u8; 16],
            username: "time-consumer".to_owned(),
            password_hash: "not-a-real-hash".to_owned(),
            active: true,
            is_admin: true,
            must_change_password: false,
            revision: 1,
        },
        digest: [0x33u8; 32],
    }
}

fn review_request() -> TimeReviewTaskRequest {
    TimeReviewTaskRequest {
        expected_revision: "1".to_owned(),
        decision: TimeReviewDecision::ResumeOriginalDeadline,
        evidence: TimeReviewEvidenceKind::Inconclusive,
    }
}

// ---------------------------------------------------------------------------
// 1. 消费入口：submit / time_review 读同一个注入源，各一次、不缓存
// ---------------------------------------------------------------------------

#[tokio::test]
async fn submit_and_time_review_consume_the_same_injected_source() {
    let clock_epoch = Uuid::new_v4();
    let source = SpyTimeSource::new(valid_evidence(clock_epoch, Utc::now(), 0));
    let dyn_source: Arc<dyn TaskTimeSource> = source.clone();
    let api = api_with_source(dyn_source);
    let actor = actor();

    // 预览令牌合法 → 适配器已读时钟 → 之后的唯一失败来源是存储层（关闭端口）。
    let before = source.reads();
    let submit = api
        .submit(&actor, &new_preview_token(), &Uuid::new_v4().to_string())
        .await;
    assert!(
        matches!(submit, Err(ControllerError::Database(_))),
        "submit 必须在写库处失败（不得被误判为参数错误）: {submit:?}"
    );
    assert_eq!(
        source.reads(),
        before + 1,
        "submit 必须且只能读一次注入时间源"
    );

    // 两次入口之间改值：下一次读取必须看到新值 → 适配器没有把证据缓存在构造期。
    source.set(fallback_evidence(clock_epoch, Utc::now()));
    let before = source.reads();
    let review = api
        .time_review(&actor, Uuid::new_v4(), review_request())
        .await;
    assert!(
        matches!(review, Err(ControllerError::Database(_))),
        "time_review 必须在写库处失败: {review:?}"
    );
    assert_eq!(
        source.reads(),
        before + 1,
        "time_review 必须且只能读一次注入时间源"
    );
}

#[tokio::test]
async fn legacy_constructor_keeps_the_untrusted_fallback_clock() {
    // 四入参兼容构造：既有行为不变（仍在存储层失败），且其时钟等价于
    // SystemFallbackTimeSource：永远不可信、纪元进程内稳定、无来源无偏差。
    let api = api_with_fallback_clock();
    let submit = api
        .submit(&actor(), &new_preview_token(), &Uuid::new_v4().to_string())
        .await;
    assert!(
        matches!(submit, Err(ControllerError::Database(_))),
        "兼容构造不得改变 submit 的失败口径: {submit:?}"
    );

    let token = [0x11u8; 32];
    let first = prepare_submit_request(&SystemFallbackTimeSource, &token, "idem-a")
        .await
        .expect("fallback 时钟组装 SubmitRequest 必须成功");
    let second = prepare_submit_request(&SystemFallbackTimeSource, &token, "idem-b")
        .await
        .expect("fallback 时钟组装 SubmitRequest 必须成功");
    assert_eq!(first.time_evidence.quality, TimeQuality::SystemFallback);
    assert_eq!(
        first.time_evidence.clock_epoch, second.time_evidence.clock_epoch,
        "fallback 纪元必须进程内稳定（time_review 的纪元连续性判定依赖它）"
    );
    assert!(first.time_evidence.source.is_none());
    assert!(first.time_evidence.offset_ms.is_none());
    assert!(first.time_evidence.uncertainty_ms.is_none());
    assert!(first.time_evidence.sample_age_ms.is_none());
    assert!(first.time_evidence.reference_utc <= second.time_evidence.reference_utc);
    // 该证据喂给生产复核策略：恒不可证明（原实现恒 Inconclusive 的既有语义保持）。
    assert_eq!(
        time_review_verdict(
            Some(&first.time_evidence),
            &second.time_evidence,
            ServiceDecision::ResumeOriginalDeadline,
            &[hour_later()],
        ),
        TimeReviewVerdict::Unverifiable
    );
}

// ---------------------------------------------------------------------------
// 2. 保真与不伪造
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ntp_valid_evidence_flows_into_submit_request_field_by_field() {
    let clock_epoch = Uuid::new_v4();
    let reference = fixed_now();
    let evidence = valid_evidence(clock_epoch, reference, 120);
    let source = SpyTimeSource::new(evidence.clone());

    let request = prepare_submit_request(source.as_ref(), &[0x22u8; 32], "idem-fidelity")
        .await
        .expect("可信证据必须可组装 SubmitRequest");
    // 逐字段原样：既不抬质量，也不抹掉 source/offset/uncertainty/age。
    assert_eq!(request.time_evidence, evidence);
    assert_eq!(request.idempotency_key, "idem-fidelity");
    assert_eq!(source.reads(), 1);

    // 观察窗口与排队截止都取自同一证据（同一可信时基），不由宿主墙钟另行取值。
    let window = chrono::Duration::try_milliseconds(OBSERVE_WINDOW_MS).expect("window in range");
    assert_eq!(request.observe_deadline, reference + window);
    let ttl = chrono::Duration::try_milliseconds(120 + QUEUE_TTL_MS).expect("ttl in range");
    assert_eq!(
        queue_deadline_of(&request.time_evidence),
        reference.checked_add_signed(ttl)
    );
}

#[test]
fn untrusted_clock_or_created_evidence_never_resumes_and_never_forges_ntp() {
    let clock_epoch = Uuid::new_v4();
    let now = Utc::now();
    let created_valid = valid_evidence(clock_epoch, now, 0);
    let deadlines = [hour_later()];

    // 当前时钟 fallback：原期限有效性无法证明。
    assert_eq!(
        time_review_verdict(
            Some(&created_valid),
            &fallback_evidence(clock_epoch, now),
            ServiceDecision::ResumeOriginalDeadline,
            &deadlines,
        ),
        TimeReviewVerdict::Unverifiable
    );
    // 创建证据 fallback（即使纪元相同）：来源时基不可信。
    assert_eq!(
        time_review_verdict(
            Some(&fallback_evidence(clock_epoch, now)),
            &valid_evidence(clock_epoch, now, 0),
            ServiceDecision::ResumeOriginalDeadline,
            &deadlines,
        ),
        TimeReviewVerdict::Unverifiable
    );
    // 纪元不连续（跳钟换代 / 跨进程）：绝不跨参考系恢复。
    assert_eq!(
        time_review_verdict(
            Some(&created_valid),
            &valid_evidence(Uuid::new_v4(), now, 0),
            ServiceDecision::ResumeOriginalDeadline,
            &deadlines,
        ),
        TimeReviewVerdict::Unverifiable
    );
    // 从未采样（Stale）：同样不可证明。
    let mut stale = fallback_evidence(clock_epoch, now);
    stale.quality = TimeQuality::Stale;
    assert_eq!(
        time_review_verdict(
            Some(&created_valid),
            &stale,
            ServiceDecision::ResumeOriginalDeadline,
            &deadlines,
        ),
        TimeReviewVerdict::Unverifiable
    );
    // 墙钟停在 1970（RTC 未初始化）的 fallback 时钟：不因「有个时间」就恢复。
    let rtc_1970 = fallback_evidence(clock_epoch, DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    assert_eq!(
        time_review_verdict(
            Some(&created_valid),
            &rtc_1970,
            ServiceDecision::ResumeOriginalDeadline,
            &deadlines,
        ),
        TimeReviewVerdict::Unverifiable
    );
    // 同一可信纪元 + 未过期截止 → 确实放行 Resume（NTP 成功真的被业务消费）。
    assert_eq!(
        time_review_verdict(
            Some(&created_valid),
            &valid_evidence(clock_epoch, now, 0),
            ServiceDecision::ResumeOriginalDeadline,
            &deadlines,
        ),
        TimeReviewVerdict::Resume
    );
}

#[test]
fn unix_epoch_reference_is_expired_never_extended() {
    let clock_epoch = Uuid::new_v4();
    let y1970 = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
    let created = valid_evidence(clock_epoch, y1970, 0);
    // 可信但参考在 1970：排队截止仍是 1970 + TTL，绝不因「可信」就往后延长。
    let original_deadline = queue_deadline_of(&created);
    assert_eq!(
        original_deadline,
        y1970.checked_add_signed(
            chrono::Duration::try_milliseconds(QUEUE_TTL_MS).expect("ttl in range")
        )
    );
    let now_valid = valid_evidence(clock_epoch, Utc::now(), 0);
    let deadlines = [original_deadline];
    // 原期限早已越过：恢复决策不放行（不盲目恢复、不重建 TTL 寿命）。
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &now_valid,
            ServiceDecision::ResumeOriginalDeadline,
            &deadlines,
        ),
        TimeReviewVerdict::Unverifiable
    );
    // 只有「全部已过期」的判过期决策成立。
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &now_valid,
            ServiceDecision::Expire,
            &deadlines
        ),
        TimeReviewVerdict::Expire
    );
}

// ---------------------------------------------------------------------------
// 3. 真实 TimeGateHandle：保真 / 超龄降级 / 跳钟换代 / 无样本 fallback
// ---------------------------------------------------------------------------

#[tokio::test]
async fn gate_handle_evidence_is_consumed_field_by_field_by_both_entries() {
    let clock_epoch = Uuid::new_v4();
    let reference = Utc::now();
    let sampler = ScriptedSampler::new(
        vec![Some(NtpSample {
            offset_ms: -42,
            rtt_ms: 3,
            reference_utc: reference,
            uncertainty_ms: 7,
        })],
        TRUSTED_SOURCE,
    );
    let gate = spawn_gate(
        gate_config(300_000),
        deps_with(
            sampler,
            ControlledWallClock::at(SystemTime::now()),
            vec![TRUSTED_SOURCE.to_owned()],
        ),
        clock_epoch,
        Duration::from_secs(5),
    );

    let ready = gate.wait_ready().await;
    assert_eq!(ready.quality, TimeQuality::NtpValid);
    assert_eq!(ready.clock_epoch, clock_epoch, "首次合格样本不得换代");

    let clock = prepare_time_review_clock(&gate).await;
    assert_eq!(clock.quality, TimeQuality::NtpValid);
    assert_eq!(clock.source.as_deref(), Some(TRUSTED_SOURCE));
    assert_eq!(clock.offset_ms, Some(-42));
    assert_eq!(clock.uncertainty_ms, Some(7));
    assert_eq!(clock.reference_utc, reference, "NTP 参考时刻必须原样消费");
    assert_eq!(clock.clock_epoch, clock_epoch);
    assert!(clock.sample_age_ms.is_some(), "可信样本必须带单调样本年龄");

    // submit 消费入口读同一个句柄 → 创建证据与复核时钟同源同纪元。
    let request = prepare_submit_request(&gate, &[0x33u8; 32], "idem-gate")
        .await
        .expect("门禁可信证据必须可组装 SubmitRequest");
    assert_eq!(request.time_evidence.quality, TimeQuality::NtpValid);
    assert_eq!(request.time_evidence.clock_epoch, clock.clock_epoch);
    assert_eq!(request.time_evidence.source, clock.source);

    // 同纪元 + 未过期截止 → 生产策略判定 Resume（不再恒 Inconclusive）。
    let queue_deadline = request.time_evidence.reference_utc.checked_add_signed(
        chrono::Duration::try_milliseconds(QUEUE_TTL_MS).expect("ttl in range"),
    );
    assert_eq!(
        time_review_verdict(
            Some(&request.time_evidence),
            &clock,
            ServiceDecision::ResumeOriginalDeadline,
            &[queue_deadline.map(|value| value + chrono::Duration::hours(1))],
        ),
        TimeReviewVerdict::Resume
    );
}

#[tokio::test]
async fn aged_sample_degrades_on_next_read_instead_of_staying_valid() {
    let clock_epoch = Uuid::new_v4();
    let reference = Utc::now();
    // 只给一次合格样本：超龄后 driver 只能失效，测试不被周期复验「续命」。
    let sampler = ScriptedSampler::new(
        vec![Some(NtpSample {
            offset_ms: -42,
            rtt_ms: 3,
            reference_utc: reference,
            uncertainty_ms: 7,
        })],
        TRUSTED_SOURCE,
    );
    let gate = spawn_gate(
        gate_config(20),
        deps_with(
            sampler,
            ControlledWallClock::at(SystemTime::now()),
            vec![TRUSTED_SOURCE.to_owned()],
        ),
        clock_epoch,
        Duration::from_secs(5),
    );

    let created = gate.wait_ready().await;
    assert_eq!(created.quality, TimeQuality::NtpValid);

    // 超过 max_evidence_age_ms 后，同一句柄的下一次读取必须降级（不缓存一次成功）。
    let mut degraded = prepare_time_review_clock(&gate).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while degraded.quality == TimeQuality::NtpValid {
        assert!(
            tokio::time::Instant::now() < deadline,
            "样本超龄后读取侧未降级，等于把一次 NtpValid 当永久有效"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
        degraded = prepare_time_review_clock(&gate).await;
    }
    assert_eq!(
        degraded.quality,
        TimeQuality::SystemFallback,
        "超龄必须降级为不可信的当前墙钟，不得伪造其他质量"
    );
    assert_eq!(
        degraded.clock_epoch, clock_epoch,
        "仅超龄不构成参考系断裂，不得换代"
    );
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &degraded,
            ServiceDecision::ResumeOriginalDeadline,
            &[hour_later()],
        ),
        TimeReviewVerdict::Unverifiable
    );
}

#[tokio::test]
async fn wall_clock_jump_renews_epoch_so_original_deadline_cannot_resume() {
    let clock_epoch = Uuid::new_v4();
    let start_wall = SystemTime::now();
    let wall = ControlledWallClock::at(start_wall);
    let sampler = Arc::new(WallFollowSampler {
        wall: Arc::clone(&wall),
        source: TRUSTED_SOURCE,
    });
    let gate = spawn_gate(
        gate_config(300_000),
        deps_with(sampler, Arc::clone(&wall), vec![TRUSTED_SOURCE.to_owned()]),
        clock_epoch,
        Duration::from_secs(5),
    );

    let created = gate.wait_ready().await;
    assert_eq!(created.quality, TimeQuality::NtpValid);
    assert_eq!(created.clock_epoch, clock_epoch);

    // 回跳 10s（> JUMP_BACK_EPS 2s）：参考系断裂 → 换代（driver 失效 + 换代双保险）。
    wall.set(start_wall - Duration::from_secs(10));
    let renewed = wait_until_clock_epoch_changes(&gate, created.clock_epoch).await;
    assert_ne!(renewed.clock_epoch, created.clock_epoch);
    assert_ne!(
        renewed.quality,
        TimeQuality::Stale,
        "跳钟后必须落到明确质量，不得停留在未初始化的 Stale"
    );
    // 旧纪元证据 + 新纪元时钟：原排队期限永不恢复（也不得被延长）。
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &renewed,
            ServiceDecision::ResumeOriginalDeadline,
            &[hour_later()],
        ),
        TimeReviewVerdict::Unverifiable
    );
}

#[tokio::test]
async fn gate_without_any_sample_completes_as_untrusted_fallback() {
    let clock_epoch = Uuid::new_v4();
    let gate = spawn_gate(
        gate_config(300_000),
        deps_with(
            Arc::new(AlwaysNoneSampler),
            ControlledWallClock::at(SystemTime::now()),
            vec![TRUSTED_SOURCE.to_owned()],
        ),
        clock_epoch,
        Duration::from_millis(120),
    );

    // 受控短超时兜底（生产仍为 30s）：无样本也必须有结果可返回，且质量如实不可信。
    let evidence = gate.wait_ready().await;
    assert!(!gate.is_ready(), "无合格样本不得转 ready");
    assert_eq!(evidence.quality, TimeQuality::SystemFallback);
    assert!(evidence.source.is_none(), "无样本时不得携带来源");
    assert!(evidence.offset_ms.is_none() && evidence.uncertainty_ms.is_none());
    assert_eq!(
        time_review_verdict(
            Some(&valid_evidence(clock_epoch, Utc::now(), 0)),
            &evidence,
            ServiceDecision::ResumeOriginalDeadline,
            &[hour_later()],
        ),
        TimeReviewVerdict::Unverifiable,
        "fallback 启动是规范允许的，但业务复核必须保持 Inconclusive"
    );
}

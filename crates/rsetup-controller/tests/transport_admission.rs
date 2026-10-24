//! 已验签板端身份的 SQL 准入验收（`protocol_spec.md` §4.6、Task 简报
//! `.superpowers/sdd/2026-10-23-production-v3-tunnel/controller-admission-brief.md`）。
//!
//! 本文件证明的是**判定与失败关闭语义**：
//!
//! 1. 现有 SQL 状态严格映射 `Approved`/`Pending`/`Rejected(3)`/`Rejected(2)`；归档行、
//!    非法组合、数据库异常、超时一律失败关闭（`HandshakeError::Policy`），绝不伪批准。
//! 2. 容量不足只回**可重试** `REJECTED(SERVER_ERROR=5)`，绝不写 `APPROVAL_DENIED(3)`/
//!    `REVOKED(2)` 这类让板端停止自动重连的永久决定；既有已批准/已拒绝身份不受新身份
//!    容量预算影响。
//! 3. 每次 `evaluate` 都重新查最新 SQL 状态（握手 final recheck 依赖它），不缓存
//!    `Approved`；草稿（display_name/descriptor JSON）不受对端输入撑爆。
//! 4. 生产登记 SQL 的结构门禁：登记语句在 SQL 文本层面**不可能**写出 APPROVED/REVOKED，
//!    只能登记 `PENDING`/`none`/revision 1/`archived = FALSE`。
//!
//! 诚实界定：本文件未连接任何真实数据库。真库用例全部 `#[ignore]`，只在操作员已预迁移到
//! 当前受支持 v4 的隔离可丢弃开发库上、经 `CONTROLLER_TEST_*` 显式授权后运行；夹具凭据
//! 仅进程内载入（`tests/common`），本文件不读取 `secret/`、不伪造备份引用、不执行
//! `DROP DATABASE`，只做本用例公钥的表级 `DELETE` 清理。fake repository 只用于证明
//! 判定语义，**不能**被表述为真实引擎验收。

use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// 真库夹具（凭据只从环境在进程内载入；本文件不读取 secret/）。
mod common;

use rsetup_controller::transport::admission::{
    AdmissionRecord, AdmissionRepository, AdmissionRow, AdmissionStoreError, COUNT_IDENTITIES_SQL,
    LOCK_ADMISSION_ROW_SQL, MAX_IDENTITY_BUDGET, NewIdentityDraft, REGISTER_NEW_IDENTITY_SQL,
    SqlxAdmissionPolicy, classify_row, decide_record, derived_display_name, descriptor_json,
    new_identity_allowed, time_evidence_json,
};

use rsetup_controller::{AdmissionState, AdmissionStore, DbPool, ReviewDecision};
use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_protocol::sig::{
    MAX_DESCRIPTOR_WIRE_BYTES, REASON_APPROVAL_DENIED, REASON_REVOKED, REASON_SERVER_ERROR,
};
use rsetup_tunnel::handshake::{AdmissionDecision, AdmissionPolicy, HandshakeError, VerifiedPeer};

/// 本文件所有源码级结构门禁读取的是**当前**模块源码；改坏顺序即变红。
const ADMISSION_SOURCE: &str = include_str!("../src/transport/admission.rs");

// ---------------------------------------------------------------------------
// 测试夹具
// ---------------------------------------------------------------------------

fn descriptor() -> DeviceDescriptor {
    DeviceDescriptor::new("SN-0001", "R2-D2 板型", "1.4.2", "batch=α/β")
}

fn peer(public_key: [u8; 32]) -> VerifiedPeer {
    VerifiedPeer {
        public_key,
        descriptor: descriptor(),
    }
}

fn key(byte: u8) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0] = byte;
    key
}

fn row(state: &str, decision: &str, revision: u64, archived: i64) -> AdmissionRow {
    AdmissionRow {
        admission_state: state.to_string(),
        review_decision: decision.to_string(),
        revision,
        archived,
    }
}

/// 脚本化 fake repository：按序弹出预置结果，并记录每次调用的草稿与预算。
/// 只证明判定语义，不替代真实 SQL。
struct FakeRepo {
    script: Mutex<VecDeque<Result<AdmissionRecord, AdmissionStoreError>>>,
    drafts: Mutex<Vec<(NewIdentityDraft, usize)>>,
    loads: AtomicUsize,
    delay: Duration,
}

impl FakeRepo {
    fn new(script: Vec<Result<AdmissionRecord, AdmissionStoreError>>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            drafts: Mutex::new(Vec::new()),
            loads: AtomicUsize::new(0),
            delay: Duration::ZERO,
        }
    }

    fn with_delay(
        script: Vec<Result<AdmissionRecord, AdmissionStoreError>>,
        delay: Duration,
    ) -> Self {
        Self {
            delay,
            ..Self::new(script)
        }
    }

    fn loads(&self) -> usize {
        self.loads.load(Ordering::SeqCst)
    }

    fn drafts(&self) -> Vec<(NewIdentityDraft, usize)> {
        self.drafts.lock().expect("fake drafts lock").clone()
    }
}

impl AdmissionRepository for FakeRepo {
    fn load_or_register(
        &self,
        draft: NewIdentityDraft,
        max_identities: usize,
    ) -> impl Future<Output = Result<AdmissionRecord, AdmissionStoreError>> + Send {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.drafts
            .lock()
            .expect("fake drafts lock")
            .push((draft, max_identities));
        let next = self
            .script
            .lock()
            .expect("fake script lock")
            .pop_front()
            .unwrap_or(Err(AdmissionStoreError::Unavailable {
                stage: "fake-script-exhausted",
            }));
        let delay = self.delay;
        async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            next
        }
    }
}

/// 不触网的 lazy 池：任何真实取连接都失败，用于证明「DB 不可达 → 失败关闭」。
fn unreachable_pool() -> DbPool {
    DbPool(
        sqlx::mysql::MySqlPoolOptions::new()
            .acquire_timeout(Duration::from_millis(50))
            .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/rsetup-offline")
            .expect("lazy pool 构造不触网"),
    )
}

// ---------------------------------------------------------------------------
// 1. 现有状态的严格映射（纯判定矩阵）
// ---------------------------------------------------------------------------

#[test]
fn classify_row_maps_current_states_exactly() {
    let cases = [
        (
            row("APPROVED", "approved", 7, 0),
            AdmissionDecision::Approved,
        ),
        (row("PENDING", "none", 1, 0), AdmissionDecision::Pending),
        (
            row("PENDING", "denied", 4, 0),
            AdmissionDecision::Rejected(REASON_APPROVAL_DENIED),
        ),
        (
            row("REVOKED", "revoked", 9, 0),
            AdmissionDecision::Rejected(REASON_REVOKED),
        ),
    ];
    for (stored, expected) in cases {
        let decision = classify_row(&stored).expect("合法组合必须给出决定");
        assert_eq!(decision, expected, "{stored:?} 映射错误");
        assert_eq!(
            decide_record(&AdmissionRecord::Existing(stored)).expect("同一映射"),
            expected
        );
    }
    // §4.6 第 4 条：只有 REVOKED(2)/APPROVAL_DENIED(3) 让板端停止自动重连。
    assert_eq!(
        classify_row(&row("REVOKED", "revoked", 1, 0)).expect("吊销"),
        AdmissionDecision::Rejected(2)
    );
    assert_eq!(
        classify_row(&row("PENDING", "denied", 1, 0)).expect("拒绝"),
        AdmissionDecision::Rejected(3)
    );
}

#[test]
fn classify_row_fails_closed_on_archived_and_illegal_combinations() {
    // 归档行不得被握手路径自动复活，也不得当作「新身份」重登记。
    for state in ["PENDING", "APPROVED", "REVOKED"] {
        for decision in ["none", "approved", "denied", "revoked"] {
            let archived = row(state, decision, 1, 1);
            assert!(
                matches!(
                    classify_row(&archived),
                    Err(AdmissionStoreError::Unavailable { .. })
                ),
                "archived=1 必须失败关闭：{archived:?}"
            );
        }
    }
    // archived 只允许 0/1；其他值说明行不可信。
    for raw in [-1i64, 2, i64::MAX] {
        let bad = row("APPROVED", "approved", 1, raw);
        assert!(
            matches!(
                classify_row(&bad),
                Err(AdmissionStoreError::Unavailable { .. })
            ),
            "archived={raw} 必须失败关闭"
        );
    }
    // 与 `validate_device_fields` 一致的合法域之外的全部组合。
    let illegal = [
        ("PENDING", "approved"),
        ("PENDING", "revoked"),
        ("APPROVED", "none"),
        ("APPROVED", "denied"),
        ("APPROVED", "revoked"),
        ("REVOKED", "none"),
        ("REVOKED", "approved"),
        ("REVOKED", "denied"),
        ("approved", "approved"),
        ("PENDING", "NONE"),
        ("", ""),
        ("WEIRD", "none"),
    ];
    for (state, decision) in illegal {
        let stored = row(state, decision, 1, 0);
        assert!(
            matches!(
                classify_row(&stored),
                Err(AdmissionStoreError::Unavailable { .. })
            ),
            "非法组合 {state}/{decision} 必须失败关闭"
        );
    }
    // 决定映射绝不产出 §4.3 之外的 reason（尤其 1=SIGNATURE_INVALID）。
    for state in ["PENDING", "APPROVED", "REVOKED"] {
        for decision in ["none", "approved", "denied", "revoked"] {
            if let Ok(AdmissionDecision::Rejected(code)) = classify_row(&row(state, decision, 1, 0))
            {
                assert!(
                    code == REASON_REVOKED || code == REASON_APPROVAL_DENIED,
                    "既有状态只能映射到 2/3，实际 {code}"
                );
            }
        }
    }
}

#[test]
fn registered_new_identity_is_pending() {
    assert_eq!(
        decide_record(&AdmissionRecord::Registered).expect("新身份恒为挂起"),
        AdmissionDecision::Pending
    );
}

// ---------------------------------------------------------------------------
// 2. 新身份容量预算（可重试，不是永久拒绝）
// ---------------------------------------------------------------------------

#[test]
fn new_identity_capacity_budget_boundary() {
    assert!(new_identity_allowed(0, 1));
    assert!(new_identity_allowed(8_191, MAX_IDENTITY_BUDGET));
    // 已达预算即拒绝新增（含预算 0 = 关闭新身份登记）。
    assert!(!new_identity_allowed(8_192, 8_192));
    assert!(!new_identity_allowed(1, 0));
    assert!(!new_identity_allowed(0, 0));
    // 预算收缩到小于存量时，既有身份仍然可以复查（由 policy 保证），新身份收敛。
    assert!(!new_identity_allowed(u64::from(u32::MAX), 8_192));
}

#[tokio::test]
async fn policy_clamps_max_identities_to_budget() {
    let clamped = SqlxAdmissionPolicy::new(unreachable_pool(), MAX_IDENTITY_BUDGET + 1);
    assert_eq!(clamped.max_identities(), MAX_IDENTITY_BUDGET);
    let smaller = SqlxAdmissionPolicy::new(unreachable_pool(), 512);
    assert_eq!(smaller.max_identities(), 512);
    let zero = SqlxAdmissionPolicy::new(unreachable_pool(), 0);
    assert_eq!(zero.max_identities(), 0, "0 表示关闭新身份登记，不得被抬升");
}

#[tokio::test]
async fn capacity_exhaustion_is_retryable_and_never_a_permanent_decision() {
    let repo = FakeRepo::new(vec![Err(AdmissionStoreError::CapacityExhausted)]);
    let policy = SqlxAdmissionPolicy::with_repository(repo, 8);
    let decision = policy
        .evaluate(&peer(key(0x11)))
        .await
        .expect("容量不足是已认证的服务端拒绝，不是策略失败");
    assert_eq!(decision, AdmissionDecision::Rejected(REASON_SERVER_ERROR));
    // 明确不是会让板端停止自动重连的 2/3，也不是 Pending。
    assert_ne!(decision, AdmissionDecision::Rejected(REASON_REVOKED));
    assert_ne!(
        decision,
        AdmissionDecision::Rejected(REASON_APPROVAL_DENIED)
    );
    assert_ne!(decision, AdmissionDecision::Pending);
    assert_eq!(policy.capacity_rejections(), 1);
    // 容量事件计数可观测（供装配层指标/日志消费），且拒绝本身可重试。
    assert!(!matches!(
        decision,
        AdmissionDecision::Rejected(REASON_REVOKED) | AdmissionDecision::Rejected(3)
    ));
}

#[tokio::test]
async fn existing_decisions_are_not_limited_by_new_identity_capacity() {
    // 预算为 0（新身份全关），但既有已批准/已拒绝/挂起身份仍按最新状态复查放行或回绝。
    let repo = FakeRepo::new(vec![
        Ok(AdmissionRecord::Existing(row("APPROVED", "approved", 5, 0))),
        Ok(AdmissionRecord::Existing(row("PENDING", "denied", 5, 0))),
        Ok(AdmissionRecord::Existing(row("REVOKED", "revoked", 5, 0))),
        Ok(AdmissionRecord::Existing(row("PENDING", "none", 5, 0))),
    ]);
    let policy = SqlxAdmissionPolicy::with_repository(repo, 0);
    for expected in [
        AdmissionDecision::Approved,
        AdmissionDecision::Rejected(REASON_APPROVAL_DENIED),
        AdmissionDecision::Rejected(REASON_REVOKED),
        AdmissionDecision::Pending,
    ] {
        let decision = policy
            .evaluate(&peer(key(0x22)))
            .await
            .expect("既有身份可复查");
        assert_eq!(decision, expected);
    }
    assert_eq!(policy.capacity_rejections(), 0);
}

// ---------------------------------------------------------------------------
// 3. 每次 evaluate 读最新 SQL，不缓存
// ---------------------------------------------------------------------------

#[tokio::test]
async fn evaluate_reloads_latest_state_every_time() {
    let repo = FakeRepo::new(vec![
        Ok(AdmissionRecord::Existing(row("PENDING", "none", 1, 0))),
        Ok(AdmissionRecord::Existing(row("APPROVED", "approved", 2, 0))),
        Ok(AdmissionRecord::Existing(row("REVOKED", "revoked", 3, 0))),
    ]);
    let policy = SqlxAdmissionPolicy::with_repository(repo, 8);
    let peer = peer(key(0x33));
    assert_eq!(
        policy.evaluate(&peer).await.expect("pending"),
        AdmissionDecision::Pending
    );
    // 批准后不得被任何「永远 APPROVED」缓存吞掉。
    assert_eq!(
        policy.evaluate(&peer).await.expect("approved"),
        AdmissionDecision::Approved
    );
    // 批准之后的吊销立即生效（撤销优先）。
    assert_eq!(
        policy.evaluate(&peer).await.expect("revoked"),
        AdmissionDecision::Rejected(REASON_REVOKED)
    );
    assert_eq!(
        policy.repo().loads(),
        3,
        "每次 evaluate 必须各做一次 SQL 读取"
    );
}

#[tokio::test]
async fn new_identity_draft_is_derived_not_peer_controlled() {
    let repo = FakeRepo::new(vec![Ok(AdmissionRecord::Registered)]);
    let policy = SqlxAdmissionPolicy::with_repository(repo, 8);
    let public_key = key(0x44);
    assert_eq!(
        policy
            .evaluate(&peer(public_key))
            .await
            .expect("登记为挂起"),
        AdmissionDecision::Pending
    );
    let drafts = policy.repo().drafts();
    assert_eq!(drafts.len(), 1);
    let (draft, budget) = &drafts[0];
    assert_eq!(draft.public_key, public_key);
    assert_eq!(*budget, 8);
    // display_name 由公钥派生，绝不携带 peer descriptor 文本。
    assert_eq!(draft.display_name, derived_display_name(&public_key));
    assert!(!draft.display_name.contains("SN-0001"));
    assert!(!draft.display_name.contains("R2-D2"));
    assert_eq!(
        draft.descriptor_json,
        descriptor_json(&descriptor()).expect("同源自 descriptor JSON")
    );
    // 首次登记的 first/last_seen 同源且为 fallback 口径。
    assert_eq!(draft.first_seen, draft.last_seen);
    let evidence: rsetup_controller::time::TimeEvidence =
        serde_json::from_str(&draft.first_seen).expect("first_seen 必须是 TimeEvidence JSON");
    assert_eq!(
        evidence.quality,
        rsetup_controller::time::TimeQuality::SystemFallback
    );
}

// ---------------------------------------------------------------------------
// 4. 去敏草稿与时间证据口径
// ---------------------------------------------------------------------------

#[test]
fn descriptor_json_keeps_typed_fields_verbatim() {
    let json = descriptor_json(&descriptor()).expect("预算内 descriptor");
    let value: serde_json::Value = serde_json::from_str(&json).expect("紧凑 JSON");
    assert_eq!(value["device_sn"], serde_json::json!("SN-0001"));
    assert_eq!(value["device_model"], serde_json::json!("R2-D2 板型"));
    assert_eq!(value["fw_version"], serde_json::json!("1.4.2"));
    assert_eq!(value["extra_info"], serde_json::json!("batch=α/β"));
    // 空串必须保留为空串（不用重编码、不做有损合并）。
    let empty =
        descriptor_json(&DeviceDescriptor::new("", "", "", "")).expect("空 descriptor 合法");
    let empty: serde_json::Value = serde_json::from_str(&empty).expect("JSON");
    assert_eq!(empty["device_sn"], serde_json::json!(""));
    assert_eq!(empty["extra_info"], serde_json::json!(""));
}

#[test]
fn descriptor_json_rejects_oversize_budget() {
    // 防御性预算门禁：单字段撑满即失败关闭（握手层已限 16247B，此处不依赖调用方）。
    let oversize = DeviceDescriptor::new("x".repeat(MAX_DESCRIPTOR_WIRE_BYTES), "y", "z", "w");
    assert!(matches!(
        descriptor_json(&oversize),
        Err(AdmissionStoreError::Unavailable { .. })
    ));
    // 恰好等于预算的内容（四段之和）仍应通过。
    let exact = DeviceDescriptor::new("x".repeat(MAX_DESCRIPTOR_WIRE_BYTES), "", "", "");
    assert!(descriptor_json(&exact).is_ok());
}

#[test]
fn derived_display_name_stays_inside_varchar_and_ignores_peer_text() {
    for byte in [0u8, 1, 127, 255] {
        let name = derived_display_name(&key(byte));
        assert!(name.len() <= 255, "VARCHAR(255) 预算：{}", name.len());
        assert!(!name.trim().is_empty());
        assert!(name.chars().all(|c| !c.is_control()));
        rsetup_controller::authz::sqlx_repo::validate_display_name(&name)
            .expect("派生 display_name 必须过既有门禁");
    }
    // 同一公钥恒定；不同公钥可区分。
    assert_eq!(derived_display_name(&key(1)), derived_display_name(&key(1)));
    assert_ne!(derived_display_name(&key(1)), derived_display_name(&key(2)));
}

#[test]
fn time_evidence_uses_real_fallback_not_claimed_ntp() {
    let json = time_evidence_json().expect("fallback 时间证据可读");
    let evidence: rsetup_controller::time::TimeEvidence =
        serde_json::from_str(&json).expect("TimeEvidence JSON");
    assert_eq!(
        evidence.quality,
        rsetup_controller::time::TimeQuality::SystemFallback,
        "默认系统时间不得标成 ntp_valid"
    );
    assert!(evidence.source.is_none());
    assert!(evidence.sample_age_ms.is_none());
    assert!(evidence.offset_ms.is_none());
    assert!(!evidence.clock_epoch.is_nil());
}

// ---------------------------------------------------------------------------
// 5. 失败关闭（含 DB 不可达与超时）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn repository_failure_fails_closed_without_pseudo_approval() {
    let repo = FakeRepo::new(vec![Err(AdmissionStoreError::Unavailable {
        stage: "select",
    })]);
    let policy = SqlxAdmissionPolicy::with_repository(repo, 8);
    let error = policy
        .evaluate(&peer(key(0x55)))
        .await
        .expect_err("数据库异常必须失败关闭");
    assert!(matches!(error, HandshakeError::Policy));
    assert!(
        !error.stops_auto_reconnect(),
        "策略失败不得停止板端自动重连"
    );
}

#[tokio::test]
async fn archived_or_illegal_row_fails_closed() {
    let repo = FakeRepo::new(vec![
        Ok(AdmissionRecord::Existing(row("APPROVED", "approved", 2, 1))),
        Ok(AdmissionRecord::Existing(row("APPROVED", "none", 2, 0))),
    ]);
    let policy = SqlxAdmissionPolicy::with_repository(repo, 8);
    for _ in 0..2 {
        let error = policy
            .evaluate(&peer(key(0x66)))
            .await
            .expect_err("归档行与非法组合必须失败关闭");
        assert!(matches!(error, HandshakeError::Policy));
    }
}

#[tokio::test]
async fn invalid_descriptor_fails_closed_before_touching_sql() {
    let repo = FakeRepo::new(vec![Ok(AdmissionRecord::Registered)]);
    let policy = SqlxAdmissionPolicy::with_repository(repo, 8);
    let peer = VerifiedPeer {
        public_key: key(0x77),
        descriptor: DeviceDescriptor::new("x".repeat(MAX_DESCRIPTOR_WIRE_BYTES + 1), "", "", ""),
    };
    let error = policy
        .evaluate(&peer)
        .await
        .expect_err("超预算 descriptor 不得入库");
    assert!(matches!(error, HandshakeError::Policy));
    assert_eq!(policy.repo().loads(), 0, "校验失败必须在取连接之前");
}

#[tokio::test]
async fn unreachable_database_fails_closed_within_budget() {
    let policy = SqlxAdmissionPolicy::new(unreachable_pool(), MAX_IDENTITY_BUDGET);
    let started = Instant::now();
    let error = policy
        .evaluate(&peer(key(0x88)))
        .await
        .expect_err("DB 不可达必须失败关闭，不伪批准");
    assert!(matches!(error, HandshakeError::Policy));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "握手初始步骤期限 10s 内必须返回，实际 {:?}",
        started.elapsed()
    );
    assert_eq!(policy.capacity_rejections(), 0, "不可达不是容量事件");
}

#[tokio::test]
async fn hanging_repository_is_cut_by_db_budget() {
    let repo = FakeRepo::with_delay(
        vec![Ok(AdmissionRecord::Registered)],
        Duration::from_secs(30),
    );
    let policy =
        SqlxAdmissionPolicy::with_repository(repo, 8).with_db_timeout(Duration::from_millis(25));
    let started = Instant::now();
    let error = policy
        .evaluate(&peer(key(0x99)))
        .await
        .expect_err("事务不得无界挂起");
    assert!(matches!(error, HandshakeError::Policy));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "预算必须切断慢仓储，实际 {:?}",
        started.elapsed()
    );
    assert!(!error.stops_auto_reconnect());
}

// ---------------------------------------------------------------------------
// 6. 生产 SQL 结构门禁
// ---------------------------------------------------------------------------

#[test]
fn register_sql_cannot_write_a_decision() {
    // 只能登记新身份：状态/决定/revision/archived 必须是字面量，5 个绑定恰好是
    // 公钥、display_name、descriptor_json、first_seen、last_seen。
    let sql = REGISTER_NEW_IDENTITY_SQL.to_ascii_uppercase();
    assert_eq!(
        REGISTER_NEW_IDENTITY_SQL.matches('?').count(),
        5,
        "{REGISTER_NEW_IDENTITY_SQL}"
    );
    assert!(sql.contains("INSERT INTO DEVICES"));
    assert!(sql.contains("'PENDING'"));
    assert!(sql.contains("'NONE'"));
    assert!(sql.contains("ARCHIVED"));
    assert!(sql.contains("FALSE"));
    // 结构性不可能：登记语句里没有 APPROVED/REVOKED/approved/revoked/TRUE，也没有 UPDATE。
    for forbidden in ["APPROVED", "REVOKED", "TRUE", "UPDATE", "ON DUPLICATE"] {
        assert!(
            !sql.contains(forbidden),
            "登记语句不得出现 {forbidden}：{REGISTER_NEW_IDENTITY_SQL}"
        );
    }
    for column in [
        "PUBLIC_KEY",
        "DISPLAY_NAME",
        "DESCRIPTOR_JSON",
        "FIRST_SEEN",
        "LAST_SEEN",
    ] {
        assert!(sql.contains(column), "登记语句缺少 {column}");
    }
}

#[test]
fn row_lock_sql_locks_the_single_public_key() {
    let sql = LOCK_ADMISSION_ROW_SQL.to_ascii_uppercase();
    assert!(sql.contains("FROM DEVICES"));
    assert!(sql.contains("PUBLIC_KEY = ?"));
    assert!(
        sql.ends_with("FOR UPDATE"),
        "必须按公钥加行锁：{LOCK_ADMISSION_ROW_SQL}"
    );
    assert_eq!(LOCK_ADMISSION_ROW_SQL.matches('?').count(), 1);
    for column in ["ADMISSION_STATE", "REVIEW_DECISION", "REVISION", "ARCHIVED"] {
        assert!(sql.contains(column), "锁定读缺少 {column}");
    }
    // 容量计数必须是有界的整表行数（不做无界扫描之外的推断），且不带锁。
    let count = COUNT_IDENTITIES_SQL.to_ascii_uppercase();
    assert!(count.starts_with("SELECT COUNT(*)"));
    assert!(count.ends_with("FROM DEVICES"));
    assert!(
        !count.contains("FOR UPDATE"),
        "计数不持行锁：{COUNT_IDENTITIES_SQL}"
    );
}

#[test]
fn production_path_takes_guard_before_row_lock_and_wraps_write() {
    // 顺序门禁只针对生产 impl 的函数体（trait 声明里没有 async fn）。
    let body_start = ADMISSION_SOURCE
        .find("async fn load_or_register")
        .expect("生产 impl 必须存在");
    let source = &ADMISSION_SOURCE[body_start..];
    let guard = source
        .find("integrity::lock_integrity_guard(&mut tx)")
        .expect("必须在同一事务内先取 v4 singleton guard");
    let select = source
        .find("LOCK_ADMISSION_ROW_SQL")
        .expect("必须按公钥锁定读最新决定");
    let count = source
        .find("COUNT_IDENTITIES_SQL")
        .expect("新身份必须先做容量计数");
    let capacity = source
        .find("AdmissionStoreError::CapacityExhausted")
        .expect("容量分支必须显式存在");
    let insert = source
        .find("REGISTER_NEW_IDENTITY_SQL")
        .expect("新身份必须走登记的真实 SQL");
    assert!(guard < select, "锁序必须 guard → 公钥行锁");
    assert!(select < count, "必须先读最新状态再计数");
    assert!(count < capacity, "容量必须在计数之后判定");
    assert!(capacity < insert, "容量不足必须在任何写入之前返回");
    let source = ADMISSION_SOURCE;
    // 事务只有两条显式提交路径：既有状态只读提交、新身份登记提交。
    assert_eq!(
        source.matches("tx.commit()").count(),
        2,
        "不得存在第三条提交/隐藏写入路径"
    );
    // 批准在整个模块内只能由 classify_row 的一处状态映射产生（禁止常量批准桩）。
    assert_eq!(
        source.matches("AdmissionDecision::Approved").count(),
        1,
        "唯一批准来源必须是既有状态映射"
    );
    assert!(
        source.contains("HandshakeError::Policy"),
        "必须有失败关闭映射"
    );
    assert!(
        !source.contains("todo!") && !source.contains("unimplemented!"),
        "不得留下骨架"
    );
}

// ---------------------------------------------------------------------------
// 7. 真库用例（显式 ignore：仅操作员预置的隔离可丢弃 v4 开发库）
// ---------------------------------------------------------------------------
//
// 前置条件（不满足即不得运行，AGENTS.md 凭据保护）：
// * 操作员已确认目标是由 `CONTROLLER_TEST_DATABASE_URL` 指向的隔离、独占、可丢弃开发库，
//   并已通过 `CONTROLLER_TEST_EXPECTED_DATABASE` / `CONTROLLER_TEST_BACKUP_REF` /
//   `CONTROLLER_TEST_MIGRATION_ACK=isolated-exclusive-backed-up-disposable` 显式授权；
//   凭据由 `tests/common::required_test_db` 从环境在进程内载入，本文件不读取 `secret/`；
// * 该库已由操作员预先迁移到当前受支持 v4 基线（本用例不做 v1/v2/v3 中间迁移、
//   不触发任何破坏性迁移入口、不伪造备份引用）；
// * 用例只按公钥表级 `DELETE` 清理自己插入的行，绝不 `DROP DATABASE` 或删表。
//
// 未运行本组用例时，交付报告必须写明「真实引擎上的登记/并发/复查/容量收敛仍待验收」。
mod real_db {
    use super::*;
    use crate::common::required_test_db;
    use sqlx::Row as _;

    async fn cleanup(db: &DbPool, public_key: &[u8; 32]) {
        sqlx::query("DELETE FROM admission_decisions WHERE device_id = ?")
            .bind(public_key.as_slice())
            .execute(&db.0)
            .await
            .expect("清理历史决定");
        sqlx::query("DELETE FROM audit_events WHERE target_id = ?")
            .bind(hex::encode(public_key))
            .execute(&db.0)
            .await
            .expect("清理审计");
        sqlx::query("DELETE FROM devices WHERE public_key = ?")
            .bind(public_key.as_slice())
            .execute(&db.0)
            .await
            .expect("清理设备行");
    }

    async fn device_row(db: &DbPool, public_key: &[u8; 32]) -> Option<(String, String, u64, i64)> {
        let row = sqlx::query(
            "SELECT admission_state, review_decision, revision, CAST(archived AS SIGNED) AS archived FROM devices WHERE public_key = ?",
        )
        .bind(public_key.as_slice())
        .fetch_optional(&db.0)
        .await
        .expect("读取设备行");
        row.map(|row| {
            (
                row.try_get::<String, _>("admission_state")
                    .expect("admission_state"),
                row.try_get::<String, _>("review_decision")
                    .expect("review_decision"),
                row.try_get::<u64, _>("revision").expect("revision"),
                row.try_get::<i64, _>("archived").expect("archived"),
            )
        })
    }

    async fn identity_rows(db: &DbPool, public_key: &[u8; 32]) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM devices WHERE public_key = ?")
            .bind(public_key.as_slice())
            .fetch_one(&db.0)
            .await
            .expect("统计身份行")
    }

    #[tokio::test]
    #[ignore = "requires an operator-provisioned, exclusively authorized disposable v4 database"]
    async fn real_db_registers_dedupes_and_rechecks_latest_decision() {
        let db = required_test_db().await;
        rsetup_controller::check_identity_schema(&db)
            .await
            .expect("夹具必须是当前受支持 v4");
        let policy = SqlxAdmissionPolicy::new(DbPool(db.0.clone()), MAX_IDENTITY_BUDGET);
        let public_key = key(0xa1);
        cleanup(&db, &public_key).await;

        // 1. 未知身份 → PENDING 真实入库：revision 1、archived 0、descriptor/时间证据齐备。
        assert_eq!(
            policy.evaluate(&peer(public_key)).await.expect("首次登记"),
            AdmissionDecision::Pending
        );
        assert_eq!(identity_rows(&db, &public_key).await, 1);
        let (state, decision, revision, archived) =
            device_row(&db, &public_key).await.expect("设备行存在");
        assert_eq!((state.as_str(), decision.as_str()), ("PENDING", "none"));
        assert_eq!(revision, 1);
        assert_eq!(archived, 0);
        // 真实列类型是 JSON（v4.sql：`descriptor_json JSON NULL`、`first_seen JSON NULL`、
        // `last_seen JSON NULL`）。两引擎在二进制协议下按 `ColumnType::Json` 回报该列，
        // 而 sqlx 0.8 的 `String` 只声明兼容 VarChar/Blob 系，直读必然 mismatched types
        // （前次真库失败即此处）。workspace 的 sqlx 未启用 `json` feature，也就没有
        // `sqlx::types::Json` 可用，因此沿用本仓既有 JSON 读取口径（`tasks/repository.rs`、
        // `authz/sqlx_repo.rs` 的 `CAST(... AS CHAR)`）显式取 JSON 文本后再做原有
        // serde 强断言——只改解码方式，不放宽 descriptor/时间证据的字段与质量位校验。
        let stored: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT CAST(descriptor_json AS CHAR) AS descriptor_json, \
             CAST(first_seen AS CHAR) AS first_seen, \
             CAST(last_seen AS CHAR) AS last_seen \
             FROM devices WHERE public_key = ?",
        )
        .bind(public_key.as_slice())
        .fetch_one(&db.0)
        .await
        .expect("读取 descriptor/时间证据");
        let descriptor_value: serde_json::Value =
            serde_json::from_str(stored.0.as_deref().expect("descriptor_json")).expect("JSON");
        assert_eq!(descriptor_value["device_sn"], serde_json::json!("SN-0001"));
        for evidence in [stored.1.as_deref(), stored.2.as_deref()] {
            let evidence: rsetup_controller::time::TimeEvidence =
                serde_json::from_str(evidence.expect("时间证据")).expect("TimeEvidence JSON");
            assert_eq!(
                evidence.quality,
                rsetup_controller::time::TimeQuality::SystemFallback
            );
        }

        // 2. 重复与并发登记收敛为一个身份、一条行、revision 不变。
        let verified = peer(public_key);
        let (first, second, third) = tokio::join!(
            policy.evaluate(&verified),
            policy.evaluate(&verified),
            policy.evaluate(&verified)
        );
        for decision in [first, second, third] {
            assert_eq!(
                decision.expect("重复登记仍为挂起"),
                AdmissionDecision::Pending
            );
        }
        assert_eq!(identity_rows(&db, &public_key).await, 1);
        let (_, _, revision, _) = device_row(&db, &public_key).await.expect("设备行存在");
        assert_eq!(revision, 1, "重复握手不得推进 revision");

        // 3. 管理 CAS 批准 → 下一次 evaluate 立即放行（无 APPROVED 缓存）。
        <DbPool as AdmissionStore>::compare_and_set(
            &db,
            public_key,
            1,
            AdmissionState::Pending,
            ReviewDecision::Approved,
            None,
            Some("real-db-acceptance"),
        )
        .await
        .expect("CAS 批准");
        assert_eq!(
            policy.evaluate(&peer(public_key)).await.expect("复查批准"),
            AdmissionDecision::Approved
        );

        // 4. 吊销优先于批准；重新握手不得复活。
        let revision: u64 = sqlx::query_scalar("SELECT revision FROM devices WHERE public_key = ?")
            .bind(public_key.as_slice())
            .fetch_one(&db.0)
            .await
            .expect("读取当前 revision");
        <DbPool as AdmissionStore>::compare_and_set(
            &db,
            public_key,
            revision,
            AdmissionState::Approved,
            ReviewDecision::Revoked,
            None,
            Some("real-db-revoke"),
        )
        .await
        .expect("CAS 吊销");
        assert_eq!(
            policy.evaluate(&peer(public_key)).await.expect("复查吊销"),
            AdmissionDecision::Rejected(REASON_REVOKED)
        );

        // 5. PENDING + denied → APPROVAL_DENIED(3)，与资源拒绝不混用。
        let denied_key = key(0xa2);
        cleanup(&db, &denied_key).await;
        assert_eq!(
            policy.evaluate(&peer(denied_key)).await.expect("登记"),
            AdmissionDecision::Pending
        );
        <DbPool as AdmissionStore>::compare_and_set(
            &db,
            denied_key,
            1,
            AdmissionState::Pending,
            ReviewDecision::Denied,
            None,
            Some("real-db-deny"),
        )
        .await
        .expect("CAS 拒绝");
        assert_eq!(
            policy.evaluate(&peer(denied_key)).await.expect("复查拒绝"),
            AdmissionDecision::Rejected(REASON_APPROVAL_DENIED)
        );

        cleanup(&db, &public_key).await;
        cleanup(&db, &denied_key).await;
    }

    #[tokio::test]
    #[ignore = "requires an operator-provisioned, exclusively authorized disposable v4 database"]
    async fn real_db_capacity_convergence_leaves_existing_identities_alone() {
        let db = required_test_db().await;
        rsetup_controller::check_identity_schema(&db)
            .await
            .expect("夹具必须是当前受支持 v4");
        let known = key(0xa3);
        let unknown = key(0xa4);
        cleanup(&db, &known).await;
        cleanup(&db, &unknown).await;

        // 预算恰好等于当前存量 → 未知身份只能拿到可重试 SERVER_ERROR，且不写任何行。
        // `SELECT COUNT(*)` 在两个真实引擎上均以**有符号** BIGINT 回报（前次真库失败即
        // 此处用 `u64` 解码：sqlx 0.8 的无符号整数 compatible 判定要求列带 UNSIGNED 标志）。
        // 与生产 `COUNT_IDENTITIES_SQL` 同口径：先按 `i64` 读，再 checked 转为预算，
        // 负数直接失败，绝不用 `as` 截断成假绿预算。
        let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
            .fetch_one(&db.0)
            .await
            .expect("统计现有身份");
        let full = SqlxAdmissionPolicy::new(
            DbPool(db.0.clone()),
            usize::try_from(existing).expect("现有身份计数必须非负且可转为预算"),
        );
        assert_eq!(
            full.evaluate(&peer(unknown)).await.expect("容量拒绝"),
            AdmissionDecision::Rejected(REASON_SERVER_ERROR)
        );
        assert_eq!(
            identity_rows(&db, &unknown).await,
            0,
            "容量不足不得写永久决定"
        );
        assert_eq!(full.capacity_rejections(), 1);

        // 同一容量预算下，既有身份（含已批准）不受影响。
        let registered = SqlxAdmissionPolicy::new(DbPool(db.0.clone()), MAX_IDENTITY_BUDGET);
        assert_eq!(
            registered.evaluate(&peer(known)).await.expect("登记"),
            AdmissionDecision::Pending
        );
        <DbPool as AdmissionStore>::compare_and_set(
            &db,
            known,
            1,
            AdmissionState::Pending,
            ReviewDecision::Approved,
            None,
            Some("real-db-capacity-approved"),
        )
        .await
        .expect("CAS 批准");
        assert_eq!(
            full.evaluate(&peer(known))
                .await
                .expect("既有批准不受容量限制"),
            AdmissionDecision::Approved
        );

        cleanup(&db, &known).await;
        cleanup(&db, &unknown).await;
    }
}

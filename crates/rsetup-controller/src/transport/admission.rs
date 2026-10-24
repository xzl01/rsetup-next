//! 已验签板端身份的 SQL 准入（`protocol_spec.md` §4.6、中控设计 §5）。
//!
//! 本模块把 `rsetup-tunnel` 握手层的 [`AdmissionPolicy`] 注入点接到**真实** `devices`
//! 表。调用前提：装配层只能把server_handshake已完成wire审计与Ed25519验签后的
//! [`VerifiedPeer`] 传入；本模块不重复验签。其public字段可以构造，已验签是调用
//! 契约而非由此结构体可见性提供的不可伪造类型证明。
//!
//! 状态映射（§4.6，权威真相只有 SQL 一列，本模块零缓存）：
//!
//! | `devices` 现有行 | 握手决定 |
//! | --- | --- |
//! | `APPROVED` + `approved` | 放行 |
//! | `PENDING` + `none` | 挂起（进入 probe 循环节奏，不设审批总期限） |
//! | `PENDING` + `denied` | `REJECTED(APPROVAL_DENIED=3)` |
//! | `REVOKED` + `revoked` | `REJECTED(REVOKED=2)` |
//! | 归档行、非法组合、读取失败 | 失败关闭（[`HandshakeError::Policy`]） |
//!
//! 关键约束：
//!
//! * **锁序与既有管理写一致**：同一事务内先 `integrity::lock_integrity_guard`（v4
//!   singleton guard），再按 `public_key` 取 `FOR UPDATE` 行锁；事务只做「读最新决定 +
//!   至多一条 INSERT」，不等待人工审批、不做网络 IO，慢速对端不能长时间持锁
//!   （§4.6 第 3 条）。
//! * **未知首次并发收敛一条**：guard 串行 + `public_key` 主键 + 锁定读，保证同一公钥的
//!   并发首次握手最多登记一行，其余走既有行分支。
//! * **容量不是拒绝审批**（§4.6 第 2 条）：预算已满时只回**可重试**的
//!   `REJECTED(SERVER_ERROR=5)`，板端按 §7 退避重试；绝不写 `APPROVAL_DENIED`/`REVOKED`，
//!   也不写任何行。既有已批准/已拒绝/挂起身份的复查不受新身份预算影响。预算上限
//!   [`MAX_IDENTITY_BUDGET`]，构造时只能收缩、不能放宽。
//! * **不缓存批准**：每次 `evaluate` 都执行一次 SQL 读取，握手在最终批准前的临界区复查
//!   （撤销优先）依赖这一点。
//! * **登记语句结构上不可能批准**：INSERT 的状态/决定/revision/archived 全是字面量
//!   `'PENDING'`/`'none'`/`1`/`FALSE`，绑定参数只有公钥、派生 `display_name`、
//!   descriptor JSON 与 first/last_seen 时间证据；本模块不存在 UPDATE 路径。
//! * **去敏**：`display_name` 由公钥派生（不把对端可控文本写进 `VARCHAR(255)`），
//!   descriptor 以 typed 字段原值记录为独立 JSON（不作验签材料、不重编码），first/
//!   last_seen 为既有 `TimeEvidence` 口径的中控系统后备证据（不把默认时间标成 NTP 有效）。
//!   错误只带编译期阶段码，不带 SQL 文本或密钥。
//!
//! 观测口径：本 crate 尚无结构化日志汇，容量拒绝以进程内计数
//! （[`SqlxAdmissionPolicy::capacity_rejections`]）加去敏 stderr 提示记录，装配层
//! （Task 5/6 网络装配）负责接入指标与告警；资源原因绝不与审批决定混用。

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rsetup_protocol::handshake_msg::DeviceDescriptor;
use rsetup_protocol::sig::{
    MAX_DESCRIPTOR_WIRE_BYTES, REASON_APPROVAL_DENIED, REASON_REVOKED, REASON_SERVER_ERROR,
};
use rsetup_tunnel::handshake::{
    AdmissionDecision, AdmissionFuture, AdmissionPolicy, HandshakeError, VerifiedPeer,
};
use sqlx::Row as _;
use thiserror::Error;

use crate::db::{DbPool, system_fallback_time_evidence};
use crate::integrity;
use crate::time::evidence::{TimeEvidence, TimeQuality};

/// 新身份入库的硬预算上限：按公钥去重不防御攻击者生成大量不同公钥，因此即使验签通过，
/// 新身份也必须受容量约束（§4.6 第 1 条）。构造参数只能收缩、不能突破本上限。
pub const MAX_IDENTITY_BUDGET: usize = 8_192;

/// 未显式指定时建议采用的新身份容量预算（装配层未接入前的唯一推荐值）。
pub const DEFAULT_MAX_IDENTITIES: usize = MAX_IDENTITY_BUDGET;

/// 单次 `evaluate` 允许占用的 SQL 临界区墙钟预算。
///
/// 这不是审批期限（§4.6 明确不设审批总期限），只约束「取连接 + guard + 行锁 + 至多一条
/// INSERT」这段临界区：超时即失败关闭并释放锁，握手层按 §7 退避重试。
pub const DEFAULT_DB_TIMEOUT: Duration = Duration::from_secs(5);

/// 派生 `display_name` 采用的公钥 hex 前缀长度（ASCII，恒 12 字符）。
const DERIVED_NAME_HEX_PREFIX: usize = 12;

/// 容量拒绝的 stderr 提示节流：首次与之后每 256 次一条，避免被攻击放大日志。
const CAPACITY_NOTICE_EVERY: u64 = 256;

// ---------------------------------------------------------------------------
// 生产 SQL（形状由 tests/transport_admission.rs 的结构门禁钉住）
// ---------------------------------------------------------------------------

/// 按公钥锁定读最新准入决定（与既有管理写同形的 `FOR UPDATE` 行锁）。
pub const LOCK_ADMISSION_ROW_SQL: &str = "SELECT admission_state,review_decision,revision,CAST(archived AS SIGNED) AS archived FROM devices WHERE public_key = ? FOR UPDATE";

/// 新身份容量计数：整表行数即身份行数（`public_key` 是主键，记录不因断连删除）。
pub const COUNT_IDENTITIES_SQL: &str = "SELECT COUNT(*) FROM devices";

/// 新身份登记：状态/决定/revision/archived 恒为字面量，参数只有五个去敏字段。
pub const REGISTER_NEW_IDENTITY_SQL: &str = "INSERT INTO devices (public_key,display_name,descriptor_json,admission_state,review_decision,revision,first_seen,last_seen,archived) VALUES (?,?,?,'PENDING','none',1,?,?,FALSE)";

// ---------------------------------------------------------------------------
// 错误
// ---------------------------------------------------------------------------

/// 准入存储/判定错误。变体只携带编译期阶段码，绝不携带 SQL 文本、DSN 或密钥材料。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Error)]
pub enum AdmissionStoreError {
    /// 新身份容量预算已满：**可重试**的服务端资源拒绝，不含任何审批语义。
    #[error("new identity capacity budget exhausted")]
    CapacityExhausted,
    /// 其余一律失败关闭：数据库不可达/异常、非法存量行、归档行、草稿口径不合法、
    /// 临界区超时。握手层据此断开本连接，不写永久决定。
    #[error("admission store unavailable at {stage}")]
    Unavailable {
        /// 去敏阶段码（编译期常量）。
        stage: &'static str,
    },
}

fn unavailable(stage: &'static str) -> AdmissionStoreError {
    AdmissionStoreError::Unavailable { stage }
}

// ---------------------------------------------------------------------------
// 纯判定：状态映射、容量预算与去敏草稿
// ---------------------------------------------------------------------------

/// `devices` 单行的准入相关列原值（严格映射由 [`classify_row`] 完成，不做隐式默认）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionRow {
    /// `admission_state` 列原值。
    pub admission_state: String,
    /// `review_decision` 列原值。
    pub review_decision: String,
    /// 状态代际（供审计与后续代际复查使用）。
    pub revision: u64,
    /// `archived` 列的 `CAST(... AS SIGNED)` 值（只允许 0/1）。
    pub archived: i64,
}

/// 一次临界区读取的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdmissionRecord {
    /// 已有准入行（可能是归档行或非法组合，由 [`classify_row`] 失败关闭）。
    Existing(AdmissionRow),
    /// 本次调用在同一事务内登记了新的 `PENDING` 身份。
    Registered,
}

/// 把现有状态严格映射为握手决定。
///
/// 合法域复用既有 `integrity::validate_device_fields`；归档行、`archived` 非 0/1、合法域
/// 之外的任何组合都失败关闭，绝不降级成「当作新身份重登记」或伪批准。
pub fn classify_row(row: &AdmissionRow) -> Result<AdmissionDecision, AdmissionStoreError> {
    let archived = integrity::raw_bool(row.archived, "devices.archived")
        .map_err(|_| unavailable("devices.archived"))?;
    if archived {
        // 归档身份不得由握手路径自动复活（恢复必须走管理面）。
        return Err(unavailable("devices.archived"));
    }
    integrity::validate_device_fields(&row.admission_state, &row.review_decision)
        .map_err(|_| unavailable("devices.state"))?;
    Ok(
        match (row.admission_state.as_str(), row.review_decision.as_str()) {
            ("APPROVED", "approved") => AdmissionDecision::Approved,
            ("PENDING", "none") => AdmissionDecision::Pending,
            ("PENDING", "denied") => AdmissionDecision::Rejected(REASON_APPROVAL_DENIED),
            ("REVOKED", "revoked") => AdmissionDecision::Rejected(REASON_REVOKED),
            // validate_device_fields 合法域之外的组合不可达；显式失败关闭而非 panic。
            _ => return Err(unavailable("devices.state")),
        },
    )
}

/// 把一次临界区结果映射为决定（`Registered` 恒为挂起，等待人工审批）。
pub fn decide_record(record: &AdmissionRecord) -> Result<AdmissionDecision, AdmissionStoreError> {
    match record {
        AdmissionRecord::Registered => Ok(AdmissionDecision::Pending),
        AdmissionRecord::Existing(row) => classify_row(row),
    }
}

/// 容量预算判定（纯函数）：`max_identities == 0` 表示关闭新身份登记。
#[must_use]
pub fn new_identity_allowed(active_identities: u64, max_identities: usize) -> bool {
    match u64::try_from(max_identities) {
        Ok(max) => active_identities < max,
        // `usize` 超出 `u64` 只可能是构造错误；失败关闭。
        Err(_) => false,
    }
}

/// 按原 typed 字段把 descriptor 记录为独立 JSON（不作验签材料，也不重编码 wire 字节）。
///
/// 四段内容之和必须仍在握手 descriptor 预算内；`display_name` 不取其中任何文本，因此
/// 对端无法用超长输入撑 `VARCHAR(255)`（§4.2 提取规则 + 中控档案去敏）。
pub fn descriptor_json(descriptor: &DeviceDescriptor) -> Result<String, AdmissionStoreError> {
    let lengths = [
        descriptor.device_sn.len(),
        descriptor.device_model.len(),
        descriptor.fw_version.len(),
        descriptor.extra_info.len(),
    ];
    let Some(total) = lengths.into_iter().try_fold(0usize, usize::checked_add) else {
        return Err(unavailable("descriptor.size"));
    };
    if total > MAX_DESCRIPTOR_WIRE_BYTES {
        return Err(unavailable("descriptor.size"));
    }
    let value = serde_json::json!({
        "device_sn": descriptor.device_sn,
        "device_model": descriptor.device_model,
        "fw_version": descriptor.fw_version,
        "extra_info": descriptor.extra_info,
    });
    serde_json::to_string(&value).map_err(|_| unavailable("descriptor.json"))
}

/// 由公钥派生的档案名：固定前缀 + 公钥 hex 短串，恒 ASCII 后缀、恒 19 字节。
#[must_use]
pub fn derived_display_name(public_key: &[u8; 32]) -> String {
    let encoded = hex::encode(public_key);
    format!("设备 {}", &encoded[..DERIVED_NAME_HEX_PREFIX])
}

/// 中控 fallback 时间证据 JSON（既有 `TimeEvidence` 口径）。
///
/// 读取真实 fallback 生成器并反核其质量位：只有 `system_fallback` 可写入 first/last_seen，
/// 任何「默认时间被标成 NTP 有效」的输出都失败关闭，不降级记录。
pub fn time_evidence_json() -> Result<String, AdmissionStoreError> {
    let raw = system_fallback_time_evidence();
    let evidence: TimeEvidence =
        serde_json::from_str(&raw).map_err(|_| unavailable("time.evidence"))?;
    if evidence.quality != TimeQuality::SystemFallback {
        return Err(unavailable("time.quality"));
    }
    Ok(raw)
}

/// 新身份登记草稿：全部字段由本模块派生，对端只能影响 `descriptor_json` 的内容且已受预算。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewIdentityDraft {
    /// 已验签公钥。
    pub public_key: [u8; 32],
    /// 去敏派生档案名。
    pub display_name: String,
    /// typed descriptor 的独立 JSON。
    pub descriptor_json: String,
    /// 首次出现的时间证据。
    pub first_seen: String,
    /// 本次登记的时间证据（首次登记与 `first_seen` 同源）。
    pub last_seen: String,
}

impl NewIdentityDraft {
    /// 在取任何连接之前完成全部校验与派生：口径不合法即失败关闭。
    pub fn from_verified_peer(peer: &VerifiedPeer) -> Result<Self, AdmissionStoreError> {
        let descriptor_json = descriptor_json(&peer.descriptor)?;
        let first_seen = time_evidence_json()?;
        Ok(Self {
            public_key: peer.public_key,
            display_name: derived_display_name(&peer.public_key),
            descriptor_json,
            last_seen: first_seen.clone(),
            first_seen,
        })
    }
}

// ---------------------------------------------------------------------------
// 仓储 seam
// ---------------------------------------------------------------------------

/// 准入仓储 seam：生产实现是真实 SQL（[`SqlxAdmissionRepository`]），测试可注入脚本化
/// 实现覆盖判定矩阵。**生产 impl 不含任何桩路径。**
pub trait AdmissionRepository: Send + Sync {
    /// 在同一个短事务内读取该公钥的最新准入行；行缺失时在同一事务内完成容量检查，并在
    /// 预算内登记新的 `PENDING` 身份。
    ///
    /// 返回 `Err(CapacityExhausted)` 时必须没有写入任何行。
    fn load_or_register(
        &self,
        draft: NewIdentityDraft,
        max_identities: usize,
    ) -> impl Future<Output = Result<AdmissionRecord, AdmissionStoreError>> + Send;
}

/// 真实 SQL 仓储：guard → 公钥行锁 → （计数 → 登记）单事务。
pub struct SqlxAdmissionRepository {
    db: DbPool,
}

impl SqlxAdmissionRepository {
    #[must_use]
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl AdmissionRepository for SqlxAdmissionRepository {
    async fn load_or_register(
        &self,
        draft: NewIdentityDraft,
        max_identities: usize,
    ) -> Result<AdmissionRecord, AdmissionStoreError> {
        let mut tx = self.db.0.begin().await.map_err(|_| unavailable("pool"))?;
        // 1. 与既有管理写同序：先取 v4 singleton guard。
        integrity::lock_integrity_guard(&mut tx)
            .await
            .map_err(|_| unavailable("guard"))?;
        // 2. 再按公钥取行锁读最新决定（未知身份在此收敛为「无行」）。
        let stored = sqlx::query(LOCK_ADMISSION_ROW_SQL)
            .bind(draft.public_key.as_slice())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| unavailable("select"))?;
        if let Some(stored) = stored {
            let row = AdmissionRow {
                admission_state: stored
                    .try_get("admission_state")
                    .map_err(|_| unavailable("decode.state"))?,
                review_decision: stored
                    .try_get("review_decision")
                    .map_err(|_| unavailable("decode.decision"))?,
                revision: stored
                    .try_get::<u64, _>("revision")
                    .map_err(|_| unavailable("decode.revision"))?,
                archived: stored
                    .try_get::<i64, _>("archived")
                    .map_err(|_| unavailable("decode.archived"))?,
            };
            // 既有决定绝不覆写：本路径只读，立即提交以尽快释放 guard。
            tx.commit().await.map_err(|_| unavailable("commit"))?;
            return Ok(AdmissionRecord::Existing(row));
        }
        // 3. 新身份：先受容量预算，再登记。
        let identities: i64 = sqlx::query_scalar(COUNT_IDENTITIES_SQL)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| unavailable("count"))?;
        let identities = u64::try_from(identities).map_err(|_| unavailable("count.range"))?;
        if !new_identity_allowed(identities, max_identities) {
            // §4.6 第 2 条：资源不足不是拒绝审批。此处不写行、不落决定，drop tx 回滚。
            return Err(AdmissionStoreError::CapacityExhausted);
        }
        // 4. 登记 PENDING/none/revision 1/archived FALSE（本模块唯一写路径）。
        let changed = sqlx::query(REGISTER_NEW_IDENTITY_SQL)
            .bind(draft.public_key.as_slice())
            .bind(&draft.display_name)
            .bind(&draft.descriptor_json)
            .bind(&draft.first_seen)
            .bind(&draft.last_seen)
            .execute(&mut *tx)
            .await
            .map_err(|_| unavailable("insert"))?
            .rows_affected();
        if changed != 1 {
            return Err(unavailable("insert.cardinality"));
        }
        tx.commit().await.map_err(|_| unavailable("commit"))?;
        Ok(AdmissionRecord::Registered)
    }
}

// ---------------------------------------------------------------------------
// AdmissionPolicy 适配
// ---------------------------------------------------------------------------

/// 真实 SQL 准入策略：消费握手层已验签的 [`VerifiedPeer`]，每次调用都读最新 SQL 状态。
pub struct SqlxAdmissionPolicy<R = SqlxAdmissionRepository> {
    repo: R,
    max_identities: usize,
    db_timeout: Duration,
    capacity_rejections: AtomicU64,
}

impl SqlxAdmissionPolicy {
    /// 以真实 SQL 仓储装配策略；`max_identities` 只能收缩到 [`MAX_IDENTITY_BUDGET`] 以内
    /// （`0` 表示关闭新身份登记，既有身份仍可复查）。
    #[must_use]
    pub fn new(db: DbPool, max_identities: usize) -> Self {
        Self::with_repository(SqlxAdmissionRepository::new(db), max_identities)
    }
}

impl<R: AdmissionRepository> SqlxAdmissionPolicy<R> {
    /// 注入仓储实现（测试 seam；生产装配使用 [`Self::new`]）。
    #[must_use]
    pub fn with_repository(repo: R, max_identities: usize) -> Self {
        Self {
            repo,
            max_identities: max_identities.min(MAX_IDENTITY_BUDGET),
            db_timeout: DEFAULT_DB_TIMEOUT,
            capacity_rejections: AtomicU64::new(0),
        }
    }

    /// 收缩 SQL 临界区墙钟预算（只允许缩短到 [`DEFAULT_DB_TIMEOUT`] 以内）。
    #[must_use]
    pub fn with_db_timeout(self, db_timeout: Duration) -> Self {
        Self {
            db_timeout: db_timeout.min(DEFAULT_DB_TIMEOUT),
            ..self
        }
    }

    #[must_use]
    pub fn max_identities(&self) -> usize {
        self.max_identities
    }

    /// 因容量预算被可重试拒绝的次数（供装配层指标/告警消费）。
    #[must_use]
    pub fn capacity_rejections(&self) -> u64 {
        self.capacity_rejections.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn repo(&self) -> &R {
        &self.repo
    }

    /// 一次判定：草稿派生 → 有界临界区 → 严格映射。零缓存。
    async fn evaluate_inner(
        &self,
        peer: &VerifiedPeer,
    ) -> Result<AdmissionDecision, AdmissionStoreError> {
        // 先派生并校验草稿：口径不合法时在取连接之前就失败关闭。
        let draft = NewIdentityDraft::from_verified_peer(peer)?;
        let record = tokio::time::timeout(
            self.db_timeout,
            self.repo.load_or_register(draft, self.max_identities),
        )
        .await
        .map_err(|_| unavailable("budget"))??;
        decide_record(&record)
    }

    /// 容量拒绝计数与去敏提示（本 crate 尚无结构化日志汇，见模块文档）。
    fn note_capacity_rejection(&self) {
        let total = self.capacity_rejections.fetch_add(1, Ordering::Relaxed) + 1;
        if total == 1 || total.is_multiple_of(CAPACITY_NOTICE_EVERY) {
            eprintln!(
                "controller admission: new identity capacity budget exhausted \
                 (max_identities={}, rejections={total}); peer gets signed \
                 REJECTED(SERVER_ERROR) and must back off",
                self.max_identities,
            );
        }
    }
}

impl<R: AdmissionRepository> AdmissionPolicy for SqlxAdmissionPolicy<R> {
    fn evaluate<'a>(&'a self, peer: &'a VerifiedPeer) -> AdmissionFuture<'a> {
        Box::pin(async move {
            match self.evaluate_inner(peer).await {
                Ok(decision) => Ok(decision),
                // §4.6 第 2 条：已具备完整认证上下文时，过载回签名 SERVER_ERROR（可重试），
                // 不写吊销、不占用 APPROVAL_DENIED。
                Err(AdmissionStoreError::CapacityExhausted) => {
                    self.note_capacity_rejection();
                    Ok(AdmissionDecision::Rejected(REASON_SERVER_ERROR))
                }
                // 其余全部失败关闭：本连接终止，不留任何永久决定。
                Err(AdmissionStoreError::Unavailable { .. }) => Err(HandshakeError::Policy),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(state: &str, decision: &str, archived: i64) -> AdmissionRow {
        AdmissionRow {
            admission_state: state.to_string(),
            review_decision: decision.to_string(),
            revision: 1,
            archived,
        }
    }

    /// 纯映射的合法域必须与 `validate_device_fields` 逐字一致。
    #[test]
    fn mapping_domain_equals_validate_device_fields_domain() {
        let states = ["PENDING", "APPROVED", "REVOKED", "UNKNOWN"];
        let decisions = ["none", "approved", "denied", "revoked", "NONE"];
        for state in states {
            for decision in decisions {
                let legal = integrity::validate_device_fields(state, decision).is_ok();
                let classified = classify_row(&stored(state, decision, 0));
                if legal {
                    assert!(
                        classified.is_ok(),
                        "{state}/{decision} 是合法组合，必须给出决定"
                    );
                } else {
                    assert!(
                        matches!(classified, Err(AdmissionStoreError::Unavailable { .. })),
                        "{state}/{decision} 非法，必须失败关闭"
                    );
                }
            }
        }
    }

    /// 派生档案名不得越出 `VARCHAR(255)`，也不得携带 control 字符。
    #[test]
    fn derived_name_is_stable_ascii_and_bounded() {
        let name = derived_display_name(&[0xab; 32]);
        assert!(name.len() <= 255);
        assert!(name.starts_with("设备 "));
        assert_eq!(name.chars().count(), 3 + DERIVED_NAME_HEX_PREFIX);
        assert!(name.chars().all(|c| !c.is_control()));
    }

    /// 预算判定边界：等号即拒绝。
    #[test]
    fn capacity_boundary_rejects_at_budget() {
        assert!(new_identity_allowed(
            (MAX_IDENTITY_BUDGET - 1) as u64,
            MAX_IDENTITY_BUDGET
        ));
        assert!(!new_identity_allowed(
            MAX_IDENTITY_BUDGET as u64,
            MAX_IDENTITY_BUDGET
        ));
        assert!(!new_identity_allowed(0, 0));
    }
}

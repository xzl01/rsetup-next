//! 设备准入与连接限额分类纯模型（protocol_spec.md §4.6，05 §6）。
//!
//! 注意：
//! - `MAX_GLOBAL_IDENTITIES = 8192` 属于 05 §6 拟议上限，非已验证生产容量。
//! - 限额分类为纯函数模型，绝不持久化准入决定，也不修改快照。
//! - 即使存在已批准快照（APPROVED），连接握手前必须检查未认证/活动连接限额，绝不因存在快照而绕过连接预算。
//! - 资源耗尽必须分类为 `RetryableServerError`，绝不能产生 `Reject(ApprovalDenied)` 或 `Reject(Revoked)`。

pub const MAX_UNAUTH_CONNECTIONS: usize = 128;
pub const MAX_PENDING_CONNECTIONS: usize = 2048;
pub const MAX_GLOBAL_IDENTITIES: usize = 8192; // 05 §6 拟议上限；待 G0 审阅

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerHandshakePhase {
    Initial,
    AwaitingChallengeSend,
    AwaitingAuthRequest,
    EvaluatingAuth,
    InPending,
    Completed,
    Terminated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ReasonCode {
    Unspecified = 0,
    SignatureInvalid = 1,
    Revoked = 2,
    ApprovalDenied = 3,
    HandshakeTimeout = 4,
    ServerError = 5,
}

impl ReasonCode {
    pub fn from_u32(code: u32) -> Option<Self> {
        match code {
            0 => Some(ReasonCode::Unspecified),
            1 => Some(ReasonCode::SignatureInvalid),
            2 => Some(ReasonCode::Revoked),
            3 => Some(ReasonCode::ApprovalDenied),
            4 => Some(ReasonCode::HandshakeTimeout),
            5 => Some(ReasonCode::ServerError),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceQuota {
    pub unauth_connections: usize,
    pub pending_connections: usize,
    pub global_identities: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentitySlot {
    Available,
    UnauthLimitReached,
    PendingPoolExhausted,
    IdentityStorageFull,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionState {
    Pending,
    Approved,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewDecision {
    None,
    Approved,
    Denied,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionSnapshot {
    pub admission_state: AdmissionState,
    pub review_decision: ReviewDecision,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionDecision {
    Pending,
    Approved,
    Reject(ReasonCode),
    RetryableServerError,
}

/// 准入与连接限额分类纯模型：
/// 1. UnauthLimitReached 属于所有连接的前置检查（preflight check）：
///    无论是否已有快照（即使快照为 Approved 或 Revoked），均返回 RetryableServerError，
///    绝不在此阶段读取具体身份，未具完整认证上下文不可错误泄露永久状态，且连接配额绝不能绕过。
/// 2. PendingPoolExhausted 与 IdentityStorageFull 仅限制新身份或待决（Pending）身份：
///    若快照已处于 Approved 或已明确决议（Denied / Revoked），不占用新待决槽位或新身份配额，不应被此类资源池阻断。
/// 3. 资源耗尽分类始终为 RetryableServerError，绝不产生持久化的 ApprovalDenied 或 Revoked。
pub fn classify_admission(
    slot: IdentitySlot,
    snapshot: Option<&AdmissionSnapshot>,
) -> AdmissionDecision {
    // 1. 未认证连接配额超限是所有握手的硬性 preflight，无论身份快照为何，均返回 RetryableServerError
    if slot == IdentitySlot::UnauthLimitReached {
        return AdmissionDecision::RetryableServerError;
    }

    if let Some(snap) = snapshot {
        match (snap.admission_state, snap.review_decision) {
            (AdmissionState::Revoked, _) | (_, ReviewDecision::Revoked) => {
                AdmissionDecision::Reject(ReasonCode::Revoked)
            }
            (AdmissionState::Pending, ReviewDecision::Denied) => {
                AdmissionDecision::Reject(ReasonCode::ApprovalDenied)
            }
            (AdmissionState::Approved, ReviewDecision::Approved) => {
                // 已批准设备无需占用 PendingPool 或新 IdentityStorage，直接 Approved
                AdmissionDecision::Approved
            }
            (AdmissionState::Pending, ReviewDecision::None) => {
                // 处于待决状态，受 PendingPoolExhausted 和 IdentityStorageFull 限制
                if slot == IdentitySlot::PendingPoolExhausted
                    || slot == IdentitySlot::IdentityStorageFull
                {
                    AdmissionDecision::RetryableServerError
                } else {
                    AdmissionDecision::Pending
                }
            }
            _ => AdmissionDecision::Reject(ReasonCode::Unspecified),
        }
    } else {
        // 新身份（无快照），若挂起池耗尽或身份存储满，返回 RetryableServerError；否则进入 Pending
        if slot == IdentitySlot::PendingPoolExhausted || slot == IdentitySlot::IdentityStorageFull {
            AdmissionDecision::RetryableServerError
        } else {
            AdmissionDecision::Pending
        }
    }
}

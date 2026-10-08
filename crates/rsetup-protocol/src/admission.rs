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
    AwaitingAuthRequest,
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
/// 无论是否已有 snapshot（即使为已批准设备 APPROVED），连接握手前必须检查未认证/活动连接限额，绝不因存在快照而绕过连接预算。
pub fn classify_admission(
    slot: IdentitySlot,
    snapshot: Option<&AdmissionSnapshot>,
) -> AdmissionDecision {
    // 资源配额检查优先：若连接层资源已耗尽，均拒绝并返回 RetryableServerError，绝不产生 ApprovalDenied 或 Revoked。
    match slot {
        IdentitySlot::UnauthLimitReached
        | IdentitySlot::PendingPoolExhausted
        | IdentitySlot::IdentityStorageFull => {
            return AdmissionDecision::RetryableServerError;
        }
        IdentitySlot::Available => {}
    }

    if let Some(snap) = snapshot {
        match (snap.admission_state, snap.review_decision) {
            (AdmissionState::Revoked, _) | (_, ReviewDecision::Revoked) => {
                AdmissionDecision::Reject(ReasonCode::Revoked)
            }
            (AdmissionState::Pending, ReviewDecision::Denied) => {
                AdmissionDecision::Reject(ReasonCode::ApprovalDenied)
            }
            (AdmissionState::Approved, ReviewDecision::Approved) => AdmissionDecision::Approved,
            (AdmissionState::Pending, ReviewDecision::None) => AdmissionDecision::Pending,
            _ => AdmissionDecision::Reject(ReasonCode::Unspecified),
        }
    } else {
        AdmissionDecision::Pending
    }
}

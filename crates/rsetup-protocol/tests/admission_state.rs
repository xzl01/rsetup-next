use rsetup_protocol::admission::{
    AdmissionDecision, AdmissionSnapshot, AdmissionState, IdentitySlot, ReasonCode, ReviewDecision,
    classify_admission,
};

#[test]
fn capacity_exhaustion_is_retryable_and_never_persists_as_denied() {
    // 资源耗尽必须分类为 RetryableServerError，绝不产生 ApprovalDenied 或 Revoked
    assert_eq!(
        classify_admission(IdentitySlot::IdentityStorageFull, None),
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::UnauthLimitReached, None),
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::PendingPoolExhausted, None),
        AdmissionDecision::RetryableServerError
    );
}

#[test]
fn approved_identity_cannot_bypass_unauth_connection_quota_but_pools_do_not_block_it() {
    // 已批准设备建立新连接时，若未认证连接超限（UnauthLimitReached），必须受限并返回 RetryableServerError。
    // 但是 PendingPoolExhausted 和 IdentityStorageFull 仅限制新/待决身份，绝不阻断已存在 Approved 身份。
    let approved_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Approved,
        revision: 10,
    };
    assert_eq!(
        classify_admission(IdentitySlot::UnauthLimitReached, Some(&approved_snap)),
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::PendingPoolExhausted, Some(&approved_snap)),
        AdmissionDecision::Approved
    );
    assert_eq!(
        classify_admission(IdentitySlot::IdentityStorageFull, Some(&approved_snap)),
        AdmissionDecision::Approved
    );
}

#[test]
fn revoked_and_denied_identities_cannot_bypass_unauth_preflight_but_ignore_pool_exhaustion() {
    let revoked_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Revoked,
        review_decision: ReviewDecision::Revoked,
        revision: 11,
    };
    // UnauthLimitReached 属于所有连接的 preflight 检查，即使传入 Revoked 也不读身份，始终返回 RetryableServerError
    assert_eq!(
        classify_admission(IdentitySlot::UnauthLimitReached, Some(&revoked_snap)),
        AdmissionDecision::RetryableServerError
    );
    // 资源池耗尽不阻断已决定的 Revoked 身份
    assert_eq!(
        classify_admission(IdentitySlot::PendingPoolExhausted, Some(&revoked_snap)),
        AdmissionDecision::Reject(ReasonCode::Revoked)
    );
    assert_eq!(
        classify_admission(IdentitySlot::IdentityStorageFull, Some(&revoked_snap)),
        AdmissionDecision::Reject(ReasonCode::Revoked)
    );

    let denied_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::Denied,
        revision: 12,
    };
    assert_eq!(
        classify_admission(IdentitySlot::UnauthLimitReached, Some(&denied_snap)),
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::PendingPoolExhausted, Some(&denied_snap)),
        AdmissionDecision::Reject(ReasonCode::ApprovalDenied)
    );
    assert_eq!(
        classify_admission(IdentitySlot::IdentityStorageFull, Some(&denied_snap)),
        AdmissionDecision::Reject(ReasonCode::ApprovalDenied)
    );
}

#[test]
fn pending_and_new_identities_are_blocked_by_pool_exhaustion() {
    let pending_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::None,
        revision: 13,
    };
    assert_eq!(
        classify_admission(IdentitySlot::PendingPoolExhausted, Some(&pending_snap)),
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::IdentityStorageFull, Some(&pending_snap)),
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::PendingPoolExhausted, None),
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::IdentityStorageFull, None),
        AdmissionDecision::RetryableServerError
    );
}

#[test]
fn snapshot_decision_mappings_when_resources_available() {
    // 1. 无快照时新设备默认为 Pending
    assert_eq!(
        classify_admission(IdentitySlot::Available, None),
        AdmissionDecision::Pending
    );

    // 2. Pending 且 None -> Pending
    let pending_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::None,
        revision: 1,
    };
    assert_eq!(
        classify_admission(IdentitySlot::Available, Some(&pending_snap)),
        AdmissionDecision::Pending
    );

    // 3. Approved 且 Approved -> Approved
    let approved_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Approved,
        revision: 2,
    };
    assert_eq!(
        classify_admission(IdentitySlot::Available, Some(&approved_snap)),
        AdmissionDecision::Approved
    );

    // 4. Pending 且 Denied -> Reject(ApprovalDenied)
    let denied_snap = AdmissionSnapshot {
        admission_state: AdmissionState::Pending,
        review_decision: ReviewDecision::Denied,
        revision: 3,
    };
    assert_eq!(
        classify_admission(IdentitySlot::Available, Some(&denied_snap)),
        AdmissionDecision::Reject(ReasonCode::ApprovalDenied)
    );

    // 5. 任何 Revoked -> Reject(Revoked)
    let revoked_snap1 = AdmissionSnapshot {
        admission_state: AdmissionState::Revoked,
        review_decision: ReviewDecision::None,
        revision: 4,
    };
    assert_eq!(
        classify_admission(IdentitySlot::Available, Some(&revoked_snap1)),
        AdmissionDecision::Reject(ReasonCode::Revoked)
    );

    let revoked_snap2 = AdmissionSnapshot {
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Revoked,
        revision: 5,
    };
    assert_eq!(
        classify_admission(IdentitySlot::Available, Some(&revoked_snap2)),
        AdmissionDecision::Reject(ReasonCode::Revoked)
    );
}

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
fn approved_identity_cannot_bypass_connection_quota() {
    // 已批准设备建立新连接时，若未认证连接超限，必须受限并返回 RetryableServerError，绝不能绕过连接预算
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
        AdmissionDecision::RetryableServerError
    );
    assert_eq!(
        classify_admission(IdentitySlot::IdentityStorageFull, Some(&approved_snap)),
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

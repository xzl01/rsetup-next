use crate::{
    AdmissionSnapshot, AdmissionState, AdmissionStore, ControllerError, ReviewDecision,
    auth::service::Session,
};

pub const MAX_ADMISSION_REASON_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecisionResult {
    pub snapshot: AdmissionSnapshot,
    pub requires_device_reset: bool,
    pub notification_queued: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionChangeEvent {
    pub public_key: [u8; 32],
    pub previous_state: AdmissionState,
    pub snapshot: AdmissionSnapshot,
}

pub trait AdmissionChangeSink: Send + Sync {
    #[allow(clippy::result_unit_err)]
    fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ()>;
}

struct TransitionParams<'a> {
    public_key: [u8; 32],
    expected_revision: u64,
    expected_state: AdmissionState,
    decision: ReviewDecision,
    reason: Option<&'a str>,
    requires_device_reset: bool,
}

pub struct DeviceService<S: AdmissionStore, K: AdmissionChangeSink> {
    store: S,
    sink: K,
}

fn validate_actor(actor: &Session) -> Result<[u8; 16], ControllerError> {
    if actor.user.active && actor.user.is_admin && !actor.user.must_change_password {
        Ok(actor.user.id)
    } else {
        Err(ControllerError::PermissionDenied)
    }
}

fn validate_reason(reason: &str) -> Result<(), ControllerError> {
    if reason.trim().is_empty()
        || reason.len() > MAX_ADMISSION_REASON_BYTES
        || reason.chars().any(|c| c.is_control())
    {
        return Err(ControllerError::InvalidArgument);
    }
    Ok(())
}

impl<S: AdmissionStore, K: AdmissionChangeSink> DeviceService<S, K> {
    pub fn new(store: S, sink: K) -> Self {
        Self { store, sink }
    }

    async fn execute_transition(
        &self,
        actor: &Session,
        params: TransitionParams<'_>,
    ) -> Result<DecisionResult, ControllerError> {
        let actor_id = validate_actor(actor)?;
        let snapshot = self
            .store
            .compare_and_set(
                params.public_key,
                params.expected_revision,
                params.expected_state,
                params.decision,
                Some(actor_id),
                params.reason,
            )
            .await?;

        let event = AdmissionChangeEvent {
            public_key: params.public_key,
            previous_state: params.expected_state,
            snapshot,
        };
        let notification_queued = self.sink.after_commit(&event).is_ok();

        Ok(DecisionResult {
            snapshot,
            requires_device_reset: params.requires_device_reset,
            notification_queued,
        })
    }

    pub async fn approve(
        &self,
        actor: &Session,
        public_key: [u8; 32],
        expected_revision: u64,
    ) -> Result<DecisionResult, ControllerError> {
        self.execute_transition(
            actor,
            TransitionParams {
                public_key,
                expected_revision,
                expected_state: AdmissionState::Pending,
                decision: ReviewDecision::Approved,
                reason: None,
                requires_device_reset: false,
            },
        )
        .await
    }

    pub async fn reject(
        &self,
        actor: &Session,
        public_key: [u8; 32],
        expected_revision: u64,
        reason: &str,
    ) -> Result<DecisionResult, ControllerError> {
        validate_reason(reason)?;
        self.execute_transition(
            actor,
            TransitionParams {
                public_key,
                expected_revision,
                expected_state: AdmissionState::Pending,
                decision: ReviewDecision::Denied,
                reason: Some(reason),
                requires_device_reset: false,
            },
        )
        .await
    }

    pub async fn reopen(
        &self,
        actor: &Session,
        public_key: [u8; 32],
        expected_revision: u64,
    ) -> Result<DecisionResult, ControllerError> {
        self.execute_transition(
            actor,
            TransitionParams {
                public_key,
                expected_revision,
                expected_state: AdmissionState::Pending,
                decision: ReviewDecision::None,
                reason: None,
                requires_device_reset: true,
            },
        )
        .await
    }

    pub async fn revoke(
        &self,
        actor: &Session,
        public_key: [u8; 32],
        expected_revision: u64,
        reason: &str,
    ) -> Result<DecisionResult, ControllerError> {
        validate_reason(reason)?;
        self.execute_transition(
            actor,
            TransitionParams {
                public_key,
                expected_revision,
                expected_state: AdmissionState::Approved,
                decision: ReviewDecision::Revoked,
                reason: Some(reason),
                requires_device_reset: true,
            },
        )
        .await
    }

    pub async fn reauthorize(
        &self,
        actor: &Session,
        public_key: [u8; 32],
        expected_revision: u64,
    ) -> Result<DecisionResult, ControllerError> {
        self.execute_transition(
            actor,
            TransitionParams {
                public_key,
                expected_revision,
                expected_state: AdmissionState::Revoked,
                decision: ReviewDecision::Approved,
                reason: None,
                requires_device_reset: true,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::service::IdentityUser;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct CasCall {
        public_key: [u8; 32],
        expected_revision: u64,
        expected_state: AdmissionState,
        decision: ReviewDecision,
        actor_id: Option<[u8; 16]>,
        reason: Option<String>,
    }

    struct FakeStore {
        calls: Mutex<Vec<CasCall>>,
        device_state: Mutex<Option<AdmissionSnapshot>>,
        cas_override: Mutex<Option<Result<AdmissionSnapshot, ControllerError>>>,
    }

    impl FakeStore {
        fn new(initial: Option<AdmissionSnapshot>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                device_state: Mutex::new(initial),
                cas_override: Mutex::new(None),
            }
        }

        fn calls(&self) -> Vec<CasCall> {
            self.calls.lock().unwrap().clone()
        }

        fn set_cas_override(&self, res: Result<AdmissionSnapshot, ControllerError>) {
            *self.cas_override.lock().unwrap() = Some(res);
        }
    }

    impl AdmissionStore for Arc<FakeStore> {
        async fn load(&self, _public_key: [u8; 32]) -> Result<AdmissionSnapshot, ControllerError> {
            self.device_state
                .lock()
                .unwrap()
                .ok_or(ControllerError::NotFound)
        }

        async fn compare_and_set(
            &self,
            public_key: [u8; 32],
            expected_revision: u64,
            expected_state: AdmissionState,
            decision: ReviewDecision,
            actor_id: Option<[u8; 16]>,
            reason: Option<&str>,
        ) -> Result<AdmissionSnapshot, ControllerError> {
            self.calls.lock().unwrap().push(CasCall {
                public_key,
                expected_revision,
                expected_state,
                decision,
                actor_id,
                reason: reason.map(ToString::to_string),
            });
            if let Some(res) = self.cas_override.lock().unwrap().take() {
                return res;
            }
            let mut state_guard = self.device_state.lock().unwrap();
            let current = state_guard.ok_or(ControllerError::NotFound)?;
            if current.revision != expected_revision || current.admission_state != expected_state {
                return Err(ControllerError::RevisionConflict);
            }
            // Use canonical transition simulation
            let next_state = match (current.admission_state, current.review_decision, decision) {
                (AdmissionState::Pending, ReviewDecision::None, ReviewDecision::Approved)
                | (AdmissionState::Revoked, ReviewDecision::Revoked, ReviewDecision::Approved) => {
                    AdmissionState::Approved
                }
                (AdmissionState::Pending, ReviewDecision::None, ReviewDecision::Denied) => {
                    AdmissionState::Pending
                }
                (AdmissionState::Pending, ReviewDecision::Denied, ReviewDecision::None) => {
                    AdmissionState::Pending
                }
                (AdmissionState::Approved, ReviewDecision::Approved, ReviewDecision::Revoked) => {
                    AdmissionState::Revoked
                }
                _ => return Err(ControllerError::RevisionConflict),
            };
            let next = AdmissionSnapshot {
                admission_state: next_state,
                review_decision: decision,
                revision: current
                    .revision
                    .checked_add(1)
                    .ok_or(ControllerError::RevisionConflict)?,
            };
            *state_guard = Some(next);
            Ok(next)
        }
    }

    struct RecordingSink {
        events: Mutex<Vec<AdmissionChangeEvent>>,
        fail_enqueue: Mutex<bool>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
                fail_enqueue: Mutex::new(false),
            }
        }

        fn events(&self) -> Vec<AdmissionChangeEvent> {
            self.events.lock().unwrap().clone()
        }

        fn set_fail_enqueue(&self, fail: bool) {
            *self.fail_enqueue.lock().unwrap() = fail;
        }
    }

    impl AdmissionChangeSink for Arc<RecordingSink> {
        fn after_commit(&self, event: &AdmissionChangeEvent) -> Result<(), ()> {
            if *self.fail_enqueue.lock().unwrap() {
                return Err(());
            }
            self.events.lock().unwrap().push(*event);
            Ok(())
        }
    }

    fn sample_admin() -> Session {
        Session {
            user: IdentityUser {
                id: [0x11; 16],
                username: "admin1".into(),
                password_hash: "$argon2id$...fake".into(),
                active: true,
                is_admin: true,
                must_change_password: false,
                revision: 1,
            },
            digest: [0xaa; 32],
        }
    }

    #[tokio::test]
    async fn approve_pending_success_invokes_cas_and_sink() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: 4,
        })));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0x42; 32];

        let res = service
            .approve(&admin, pubkey, 4)
            .await
            .expect("approve should succeed");

        assert_eq!(
            res,
            DecisionResult {
                snapshot: AdmissionSnapshot {
                    admission_state: AdmissionState::Approved,
                    review_decision: ReviewDecision::Approved,
                    revision: 5,
                },
                requires_device_reset: false,
                notification_queued: true,
            }
        );
        let calls = store.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].public_key, pubkey);
        assert_eq!(calls[0].expected_revision, 4);
        assert_eq!(calls[0].expected_state, AdmissionState::Pending);
        assert_eq!(calls[0].decision, ReviewDecision::Approved);
        assert_eq!(calls[0].actor_id, Some(admin.user.id));
        assert_eq!(calls[0].reason, None);

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].public_key, pubkey);
        assert_eq!(events[0].previous_state, AdmissionState::Pending);
        assert_eq!(events[0].snapshot, res.snapshot);
    }

    #[tokio::test]
    async fn reauthorize_revoked_success_sets_requires_device_reset() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Revoked,
            review_decision: ReviewDecision::Revoked,
            revision: 12,
        })));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0x55; 32];

        let res = service
            .reauthorize(&admin, pubkey, 12)
            .await
            .expect("reauthorize should succeed");

        assert_eq!(
            res,
            DecisionResult {
                snapshot: AdmissionSnapshot {
                    admission_state: AdmissionState::Approved,
                    review_decision: ReviewDecision::Approved,
                    revision: 13,
                },
                requires_device_reset: true,
                notification_queued: true,
            }
        );
        let calls = store.calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].expected_state, AdmissionState::Revoked);
        assert_eq!(calls[0].decision, ReviewDecision::Approved);
        assert_eq!(calls[0].reason, None);

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].previous_state, AdmissionState::Revoked);
    }

    #[tokio::test]
    async fn approve_on_revoked_device_atomically_fails_revision_conflict() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Revoked,
            review_decision: ReviewDecision::Revoked,
            revision: 7,
        })));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0x77; 32];

        let err = service
            .approve(&admin, pubkey, 7)
            .await
            .expect_err("approve on revoked device must fail with RevisionConflict");
        assert!(matches!(err, ControllerError::RevisionConflict));
        assert_eq!(store.calls().len(), 1);
        assert_eq!(store.calls()[0].expected_state, AdmissionState::Pending);
        assert_eq!(sink.events().len(), 0);
    }

    #[tokio::test]
    async fn reject_requires_valid_reason_and_sets_pending_denied() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: 3,
        })));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0x88; 32];

        // Valid reject
        let res = service
            .reject(&admin, pubkey, 3, "unauthorized hardware revision")
            .await
            .expect("reject should succeed");

        assert_eq!(
            res,
            DecisionResult {
                snapshot: AdmissionSnapshot {
                    admission_state: AdmissionState::Pending,
                    review_decision: ReviewDecision::Denied,
                    revision: 4,
                },
                requires_device_reset: false,
                notification_queued: true,
            }
        );
        let calls = store.calls();
        assert_eq!(calls[0].expected_state, AdmissionState::Pending);
        assert_eq!(calls[0].decision, ReviewDecision::Denied);
        assert_eq!(
            calls[0].reason.as_deref(),
            Some("unauthorized hardware revision")
        );

        // Validation failures: empty, whitespace-only, too long, control chars
        assert!(matches!(
            service.reject(&admin, pubkey, 4, "").await,
            Err(ControllerError::InvalidArgument)
        ));
        assert!(matches!(
            service.reject(&admin, pubkey, 4, "   \n\t  ").await,
            Err(ControllerError::InvalidArgument)
        ));
        let oversized = "a".repeat(1025);
        assert!(matches!(
            service.reject(&admin, pubkey, 4, &oversized).await,
            Err(ControllerError::InvalidArgument)
        ));
        let with_nul = "bad\0reason";
        assert!(matches!(
            service.reject(&admin, pubkey, 4, with_nul).await,
            Err(ControllerError::InvalidArgument)
        ));
        let with_ctrl = "bad\x07reason";
        assert!(matches!(
            service.reject(&admin, pubkey, 4, with_ctrl).await,
            Err(ControllerError::InvalidArgument)
        ));
    }

    #[tokio::test]
    async fn reopen_transitions_denied_to_none_and_requires_reset() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::Denied,
            revision: 8,
        })));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0x99; 32];

        let res = service
            .reopen(&admin, pubkey, 8)
            .await
            .expect("reopen should succeed");

        assert_eq!(
            res,
            DecisionResult {
                snapshot: AdmissionSnapshot {
                    admission_state: AdmissionState::Pending,
                    review_decision: ReviewDecision::None,
                    revision: 9,
                },
                requires_device_reset: true,
                notification_queued: true,
            }
        );
        let calls = store.calls();
        assert_eq!(calls[0].expected_state, AdmissionState::Pending);
        assert_eq!(calls[0].decision, ReviewDecision::None);
        assert_eq!(calls[0].reason, None);
    }

    #[tokio::test]
    async fn revoke_transitions_approved_to_revoked_and_requires_reset() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Approved,
            review_decision: ReviewDecision::Approved,
            revision: 10,
        })));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0xaa; 32];

        let res = service
            .revoke(&admin, pubkey, 10, "compromised key")
            .await
            .expect("revoke should succeed");

        assert_eq!(
            res,
            DecisionResult {
                snapshot: AdmissionSnapshot {
                    admission_state: AdmissionState::Revoked,
                    review_decision: ReviewDecision::Revoked,
                    revision: 11,
                },
                requires_device_reset: true,
                notification_queued: true,
            }
        );
        let calls = store.calls();
        assert_eq!(calls[0].expected_state, AdmissionState::Approved);
        assert_eq!(calls[0].decision, ReviewDecision::Revoked);
        assert_eq!(calls[0].reason.as_deref(), Some("compromised key"));
    }

    #[tokio::test]
    async fn actor_permission_checks_fail_closed_before_store_call() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: 1,
        })));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let pubkey = [0x11; 32];

        // 1. Not an admin
        let mut non_admin = sample_admin();
        non_admin.user.is_admin = false;
        assert!(matches!(
            service.approve(&non_admin, pubkey, 1).await,
            Err(ControllerError::PermissionDenied)
        ));

        // 2. Inactive user
        let mut inactive = sample_admin();
        inactive.user.active = false;
        assert!(matches!(
            service.approve(&inactive, pubkey, 1).await,
            Err(ControllerError::PermissionDenied)
        ));

        // 3. Must change password
        let mut forced_pwd = sample_admin();
        forced_pwd.user.must_change_password = true;
        assert!(matches!(
            service.approve(&forced_pwd, pubkey, 1).await,
            Err(ControllerError::PermissionDenied)
        ));

        // Ensure store and sink were never called
        assert_eq!(store.calls().len(), 0);
        assert_eq!(sink.events().len(), 0);
    }

    #[tokio::test]
    async fn sink_failure_returns_committed_snapshot_with_notification_queued_false() {
        let store = Arc::new(FakeStore::new(Some(AdmissionSnapshot {
            admission_state: AdmissionState::Pending,
            review_decision: ReviewDecision::None,
            revision: 2,
        })));
        let sink = Arc::new(RecordingSink::new());
        sink.set_fail_enqueue(true);
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0x33; 32];

        let res = service
            .approve(&admin, pubkey, 2)
            .await
            .expect("commit succeeded so result must be returned Ok");

        assert!(!res.notification_queued);
        assert_eq!(
            res.snapshot,
            AdmissionSnapshot {
                admission_state: AdmissionState::Approved,
                review_decision: ReviewDecision::Approved,
                revision: 3,
            }
        );
        assert_eq!(store.calls().len(), 1);
    }

    #[tokio::test]
    async fn cas_failure_does_not_call_sink() {
        let store = Arc::new(FakeStore::new(None));
        store.set_cas_override(Err(ControllerError::NotFound));
        let sink = Arc::new(RecordingSink::new());
        let service = DeviceService::new(store.clone(), sink.clone());
        let admin = sample_admin();
        let pubkey = [0x44; 32];

        let err = service
            .approve(&admin, pubkey, 1)
            .await
            .expect_err("store error");
        assert!(matches!(err, ControllerError::NotFound));
        assert_eq!(sink.events().len(), 0);
    }
}

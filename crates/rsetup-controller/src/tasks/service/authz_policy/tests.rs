use std::collections::HashMap;

use uuid::Uuid;

use super::snapshot_allows_task_creation;
use crate::authz::policy::{GrantRecord, GrantScope, GrantSource, Permission, SnapshotActor};
use crate::authz::sqlx_repo::AuthorizationSnapshot;
use crate::devices::repository::DeviceAdminView;
use crate::model::{AdmissionState, ReviewDecision};

const POLICY_DEVICE: [u8; 32] = [3u8; 32];
const ACTIVE_USER: SnapshotActor = SnapshotActor {
    id: [2u8; 16],
    active: true,
    is_admin: false,
    must_change_password: false,
};

fn policy_device_fixture(id: [u8; 32], archived: bool) -> DeviceAdminView {
    DeviceAdminView {
        public_key: id,
        display_name: "policy-fixture".into(),
        admission_state: AdmissionState::Approved,
        review_decision: ReviewDecision::Approved,
        revision: 1,
        descriptor: None,
        archived,
    }
}

fn policy_snapshot(
    actor: SnapshotActor,
    grants: Vec<GrantRecord>,
    devices: Vec<([u8; 32], bool)>,
) -> AuthorizationSnapshot {
    AuthorizationSnapshot {
        actor,
        authz_epoch: 1,
        grants,
        roles: HashMap::new(),
        device_groups: HashMap::new(),
        devices: devices
            .into_iter()
            .map(|(id, archived)| (id, policy_device_fixture(id, archived)))
            .collect(),
    }
}

fn policy_direct_grant(user_id: [u8; 16], permissions: Vec<Permission>) -> GrantRecord {
    GrantRecord {
        id: Uuid::new_v4(),
        user_id,
        source: GrantSource::Direct(permissions),
        scope: GrantScope::All,
        revision: 1,
    }
}

#[test]
fn snapshot_allows_task_creation_accepts_reboot_only_grant() {
    let snapshot = policy_snapshot(
        ACTIVE_USER,
        vec![policy_direct_grant(
            ACTIVE_USER.id,
            vec![Permission::DeviceReboot],
        )],
        vec![(POLICY_DEVICE, false)],
    );
    assert!(snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
}

#[test]
fn snapshot_allows_task_creation_rejects_task_read_only_grant() {
    let snapshot = policy_snapshot(
        ACTIVE_USER,
        vec![policy_direct_grant(
            ACTIVE_USER.id,
            vec![Permission::DeviceTaskRead],
        )],
        vec![(POLICY_DEVICE, false)],
    );
    // task_read-only 是 VisibleForbidden，不是 Permitted
    assert!(!snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
}

#[test]
fn snapshot_allows_task_creation_rejects_inactive_or_must_change_actor() {
    let grants = vec![policy_direct_grant(
        ACTIVE_USER.id,
        vec![Permission::DeviceReboot],
    )];
    for actor in [
        SnapshotActor {
            active: false,
            ..ACTIVE_USER
        },
        SnapshotActor {
            must_change_password: true,
            ..ACTIVE_USER
        },
    ] {
        let snapshot = policy_snapshot(actor, grants.clone(), vec![(POLICY_DEVICE, false)]);
        assert!(!snapshot_allows_task_creation(&snapshot, POLICY_DEVICE));
    }
}

#[test]
fn snapshot_allows_task_creation_rejects_missing_or_archived_device() {
    let grants = vec![policy_direct_grant(
        ACTIVE_USER.id,
        vec![Permission::DeviceReboot],
    )];
    let missing = policy_snapshot(ACTIVE_USER, grants.clone(), vec![]);
    let archived = policy_snapshot(ACTIVE_USER, grants, vec![(POLICY_DEVICE, true)]);
    assert!(!snapshot_allows_task_creation(&missing, POLICY_DEVICE));
    assert!(!snapshot_allows_task_creation(&archived, POLICY_DEVICE));
}

//! 任务 HTTP 域测试共享 Fixture。
//!
//! 跨 projection / gate / routes / events 共享的会话与实体构造函数及常量。
//! 批次 3 首建：仅包含 `RAW_TOKEN` 与 `fixture_session` 两项（三段时序第一段）。
//! 批次 4 补齐：迁入 `IDEMPOTENCY_KEY`、`fixture_time_evidence` 与 `fixture_detail`（三段时序第二段）。

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::auth::service::{IdentityUser, Session};
use crate::auth::session::token_digest;
use crate::tasks::http::contract::{TaskActions, TaskCounts, TaskDetail, TaskOperation, ViewScope};
use crate::tasks::model::{MainTaskState, SubTaskState};
use crate::time::evidence::{TimeEvidence, TimeQuality};

pub(super) const RAW_TOKEN: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub(super) const IDEMPOTENCY_KEY: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";

pub(super) fn fixture_session(is_admin: bool, must_change_password: bool) -> Session {
    Session {
        user: IdentityUser {
            id: [7u8; 16],
            username: if is_admin {
                "admin".into()
            } else {
                "alice".into()
            },
            password_hash: "$argon2id$=synthetic-test-only=".to_string(),
            active: true,
            is_admin,
            must_change_password,
            revision: 1,
        },
        digest: token_digest(RAW_TOKEN.as_bytes()),
    }
}

pub(super) fn fixture_time_evidence() -> TimeEvidence {
    let ts = DateTime::parse_from_rfc3339("2026-10-16T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    TimeEvidence {
        system_wall_utc: ts,
        reference_utc: ts,
        quality: TimeQuality::SystemFallback,
        clock_epoch: Uuid::new_v4(),
        source: None,
        sample_age_ms: None,
        offset_ms: None,
        uncertainty_ms: None,
    }
}

pub(super) fn fixture_detail() -> TaskDetail {
    TaskDetail {
        id: Uuid::from_u128(0x1234567812344abc80de123456789a01),
        operation: TaskOperation::Reboot,
        state: MainTaskState::Running,
        outcome: None,
        revision: 4,
        created_time: fixture_time_evidence(),
        view_scope: ViewScope::AuthorizedSubset,
        counts: TaskCounts {
            queued: 0,
            held: 0,
            dispatching: 0,
            accepted: 0,
            verifying: 0,
            succeeded: 1,
            failed: 0,
            unknown: 1,
            cancelled: 0,
            expired: 0,
        },
        actions: TaskActions { can_cancel: false },
        visible_states: vec![SubTaskState::Succeeded, SubTaskState::Unknown],
    }
}

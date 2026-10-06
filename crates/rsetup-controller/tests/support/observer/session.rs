//! Pure, test-only identity and edge contracts; no network or SQL here.
use super::{Engine, ObserverError};
use std::{future::Future, time::Duration};
use tokio::time::Instant;

// Identity values must not be printed by a derived Debug implementation.
pub(crate) struct SessionIdentity {
    pub(crate) connection_id: u64,
    pub(crate) version: String,
    pub(crate) database: Option<String>,
    pub(crate) current_user: String,
    pub(crate) server_uuid: Option<String>,
    pub(crate) global_ids: Option<bool>,
    pub(crate) pessimistic: Option<bool>,
}

pub(crate) fn validate_sessions(
    engine: Engine,
    schema: &str,
    a: &SessionIdentity,
    b: &SessionIdentity,
    o: &SessionIdentity,
    topology_current: bool,
) -> Result<(), ObserverError> {
    let expected_version = match engine {
        Engine::Mysql => "8.0.46",
        Engine::Tidb => "8.0.11-TiDB-v8.5.8",
    };
    let ids = [a.connection_id, b.connection_id, o.connection_id];
    if schema.is_empty()
        || ids.contains(&0)
        || ids[0] == ids[1]
        || ids[0] == ids[2]
        || ids[1] == ids[2]
        || [a, b, o].iter().any(|s| s.version != expected_version)
        || a.database.as_deref() != Some(schema)
        || b.database.as_deref() != Some(schema)
        || o.database.is_some()
        || a.current_user.is_empty()
        || a.current_user != b.current_user
        || o.current_user.is_empty()
        || a.current_user == o.current_user
    {
        return Err("identity_mismatch");
    }
    match engine {
        Engine::Mysql => {
            let uuid = a.server_uuid.as_deref().filter(|uuid| !uuid.is_empty());
            if uuid.is_none()
                || b.server_uuid.as_deref() != uuid
                || o.server_uuid.as_deref() != uuid
            {
                return Err("identity_mismatch");
            }
        }
        Engine::Tidb => {
            if !topology_current {
                return Err("prerequisite_missing");
            }
            if [a, b, o].iter().any(|s| s.global_ids != Some(true))
                || a.pessimistic != Some(true)
                || b.pessimistic != Some(true)
            {
                return Err("identity_mismatch");
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
pub(crate) struct ActorIds {
    pub(crate) waiter: u64,
    pub(crate) holder: u64,
}

pub(crate) struct TrxMapping {
    pub(crate) session: u64,
    pub(crate) start_ts: u64,
}

pub(crate) async fn bounded<F, T>(
    future: F,
    total: Instant,
    stage: Duration,
) -> Result<T, ObserverError>
where
    F: Future<Output = Result<T, ObserverError>>,
{
    let deadline = std::cmp::min(total, Instant::now().checked_add(stage).unwrap_or(total));
    if Instant::now() >= deadline {
        return Err("timeout");
    }
    tokio::time::timeout_at(deadline, future)
        .await
        .map_err(|_| "timeout")?
}

pub(crate) fn validate_edge(
    ids: ActorIds,
    mappings: &[TrxMapping],
    edge: Option<(u64, u64)>,
) -> Result<bool, ObserverError> {
    if ids.waiter == 0 || ids.holder == 0 || ids.waiter == ids.holder {
        return Err("identity_mismatch");
    }
    let mut waiter = None;
    let mut holder = None;
    let mut seen_sessions = std::collections::HashSet::new();
    let mut seen_start_ts = std::collections::HashSet::new();
    for mapping in mappings {
        if mapping.session == 0
            || mapping.start_ts == 0
            || !seen_sessions.insert(mapping.session)
            || !seen_start_ts.insert(mapping.start_ts)
        {
            return Err("identity_mismatch");
        }
        if mapping.session == ids.waiter {
            waiter = Some(mapping.start_ts);
        } else if mapping.session == ids.holder {
            holder = Some(mapping.start_ts);
        }
    }
    let holder = holder.ok_or("identity_mismatch")?;
    if matches!(edge, Some((0, _)) | Some((_, 0))) {
        return Err("identity_mismatch");
    }
    Ok(matches!((waiter, edge), (Some(w), Some((ew, eh))) if w == ew && holder == eh))
}

//! Shared typed step sequencing for a synthetic or future SQLx reader.
//! Only query_plan selects SQL and binds; this layer cannot accept arbitrary SQL.
use super::{
    Account, ActorIds, Engine, ObserverError, QueryPlan, ReadStep, SessionIdentity, TrxMapping,
    bounded, query_plan, validate_edge, validate_grants, validate_identity,
};
use std::time::Duration;
use tokio::time::Instant;

// Nullable decoded columns remain explicit; none of these types implement Debug/Display.
pub(crate) enum ReadResult {
    Identity {
        connection_id: Option<u64>,
        version: Option<String>,
        database: Option<String>,
        current_user: Option<String>,
        server_uuid: Option<String>,
        global_ids: Option<String>,
        txn_mode: Option<String>,
    },
    Grants(Vec<Option<String>>),
    Roles {
        current_role: Option<String>,
        mandatory_roles: Option<String>,
    },
    Scan(Vec<Option<i64>>),
    Count(Option<i64>),
    TrxMapping(Vec<(Option<u64>, Option<u64>)>),
    Edges(Vec<(Option<u64>, Option<u64>)>),
}

pub(crate) enum ReadFailure {
    Query,
    Decode,
}

/// Closed, nonsensitive stage identifier; never infer this from an error class.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlowStage {
    Prerequisite,
    Connect,
    Step(ReadStep),
}

pub(crate) struct FlowFailure {
    pub(crate) stage: FlowStage,
    pub(crate) error_class: ObserverError,
}

fn failed(stage: FlowStage, error_class: ObserverError) -> FlowFailure {
    FlowFailure { stage, error_class }
}

/// Implementations execute the supplied catalog plan with typed binds; an O3B2B
/// SQLx adapter must use the new result aliases and fetch_all for LIMIT 3/2.
/// Driver details must collapse to Query/Decode; no raw text is retained here.
pub(crate) trait ReadExecutor {
    async fn read(&mut self, step: ReadStep, plan: QueryPlan) -> Result<ReadResult, ReadFailure>;
}

const QUERY_BUDGET: Duration = Duration::from_secs(3);
const NO_ACTORS: ActorIds = ActorIds {
    waiter: 0,
    holder: 0,
};

async fn read_step<E: ReadExecutor>(
    executor: &mut E,
    engine: Engine,
    schema: &str,
    ids: ActorIds,
    step: ReadStep,
    total: Instant,
) -> Result<ReadResult, FlowFailure> {
    let stage = FlowStage::Step(step);
    let plan = query_plan(engine, step, schema, ids).map_err(|error| failed(stage, error))?;
    bounded(
        async {
            executor
                .read(step, plan)
                .await
                .map_err(|failure| match failure {
                    ReadFailure::Query if step == ReadStep::Grants => "grants_query_error",
                    ReadFailure::Query => "query_error",
                    ReadFailure::Decode => "decode_error",
                })
        },
        total,
        QUERY_BUDGET,
    )
    .await
    .map_err(|error| failed(stage, error))
}

/// `actual_writer` is only supplied data. O6 must obtain A/B non-NULL actual
/// CURRENT_USER() on the participating physical connections and compare them;
/// a configuration value is never an accepted substitute for that evidence.
pub(crate) async fn preflight<E: ReadExecutor>(
    executor: &mut E,
    engine: Engine,
    schema: &str,
    expected: &Account,
    actual_writer: &str,
    topology_current: bool,
    total: Instant,
) -> Result<SessionIdentity, FlowFailure> {
    if engine == Engine::Tidb && !topology_current {
        return Err(failed(FlowStage::Prerequisite, "prerequisite_missing"));
    }
    if actual_writer.is_empty() {
        return Err(failed(FlowStage::Prerequisite, "identity_mismatch"));
    }
    let identity = read_step(
        executor,
        engine,
        schema,
        NO_ACTORS,
        ReadStep::Identity,
        total,
    )
    .await?;
    let stage = FlowStage::Step(ReadStep::Identity);
    let ReadResult::Identity {
        connection_id,
        version,
        database,
        current_user,
        server_uuid,
        global_ids,
        txn_mode,
    } = identity
    else {
        return Err(failed(stage, "decode_error"));
    };
    let connection_id = connection_id
        .filter(|id| *id != 0)
        .ok_or_else(|| failed(stage, "decode_error"))?;
    let version = version.ok_or_else(|| failed(stage, "decode_error"))?;
    let current_user = current_user.ok_or_else(|| failed(stage, "decode_error"))?;
    let expected_version = match engine {
        Engine::Mysql => "8.0.46",
        Engine::Tidb => "8.0.11-TiDB-v8.5.8",
    };
    if version != expected_version
        || database.is_some()
        || current_user.is_empty()
        || current_user != format!("{}@{}", expected.user, expected.host)
        || current_user == actual_writer
    {
        return Err(failed(stage, "identity_mismatch"));
    }
    let (server_uuid, global_ids) = match engine {
        Engine::Mysql => {
            if global_ids.is_some() || txn_mode.is_some() {
                return Err(failed(stage, "decode_error"));
            }
            let uuid = server_uuid
                .filter(|uuid| !uuid.is_empty())
                .ok_or_else(|| failed(stage, "decode_error"))?;
            (Some(uuid), None)
        }
        Engine::Tidb => {
            if server_uuid.is_some() || txn_mode.is_none() {
                return Err(failed(stage, "decode_error"));
            }
            if global_ids.as_deref() != Some("true") {
                return Err(failed(
                    stage,
                    if global_ids.is_none() {
                        "decode_error"
                    } else {
                        "identity_mismatch"
                    },
                ));
            }
            (None, Some(true))
        }
    };
    let grants = read_step(executor, engine, schema, NO_ACTORS, ReadStep::Grants, total).await?;
    let stage = FlowStage::Step(ReadStep::Grants);
    let ReadResult::Grants(rows) = grants else {
        return Err(failed(stage, "decode_error"));
    };
    if rows.iter().any(Option::is_none) {
        return Err(failed(stage, "decode_error"));
    }
    validate_grants(engine, &rows, expected).map_err(|error| failed(stage, error))?;
    let roles = read_step(executor, engine, schema, NO_ACTORS, ReadStep::Roles, total).await?;
    let stage = FlowStage::Step(ReadStep::Roles);
    let ReadResult::Roles {
        current_role,
        mandatory_roles,
    } = roles
    else {
        return Err(failed(stage, "decode_error"));
    };
    if current_role.is_none()
        || (engine == Engine::Mysql && mandatory_roles.is_none())
        || (engine == Engine::Tidb && mandatory_roles.is_some())
    {
        return Err(failed(stage, "decode_error"));
    }
    validate_identity(
        engine,
        &current_user,
        actual_writer,
        expected,
        current_role.as_deref(),
        mandatory_roles.as_deref(),
    )
    .map_err(|error| failed(stage, error))?;
    let scans: &[ReadStep] = match engine {
        Engine::Mysql => &[
            ReadStep::MysqlWaits,
            ReadStep::MysqlLocks,
            ReadStep::MysqlThreads,
        ],
        Engine::Tidb => &[ReadStep::TidbWaits, ReadStep::TidbTrx],
    };
    for &step in scans {
        let result = read_step(executor, engine, schema, NO_ACTORS, step, total).await?;
        match result {
            ReadResult::Scan(rows) if rows.is_empty() || rows.len() == 1 && rows[0] == Some(1) => {}
            _ => return Err(failed(FlowStage::Step(step), "decode_error")),
        }
    }
    Ok(SessionIdentity {
        connection_id,
        version,
        database,
        current_user,
        server_uuid,
        global_ids,
        pessimistic: None,
    }) // O never begins a TiDB transaction.
}

fn count(result: ReadResult, step: ReadStep) -> Result<i64, FlowFailure> {
    match result {
        ReadResult::Count(Some(n)) if n >= 0 => Ok(n),
        _ => Err(failed(FlowStage::Step(step), "decode_error")),
    }
}

/// A false result is not positive observation. Caller reuses absolute deadline.
pub(crate) async fn sample_edge<E: ReadExecutor>(
    executor: &mut E,
    engine: Engine,
    schema: &str,
    ids: ActorIds,
    total: Instant,
) -> Result<bool, FlowFailure> {
    if ids.waiter == 0 || ids.holder == 0 || ids.waiter == ids.holder {
        return Err(failed(FlowStage::Prerequisite, "identity_mismatch"));
    }
    match engine {
        Engine::Mysql => {
            for step in [ReadStep::MysqlWaiterThread, ReadStep::MysqlHolderThread] {
                if count(
                    read_step(executor, engine, schema, ids, step, total).await?,
                    step,
                )? != 1
                {
                    return Err(failed(FlowStage::Step(step), "identity_mismatch"));
                }
            }
            Ok(count(
                read_step(executor, engine, schema, ids, ReadStep::Edge, total).await?,
                ReadStep::Edge,
            )? > 0)
        }
        Engine::Tidb => {
            let step = ReadStep::TidbMapping;
            let result = read_step(executor, engine, schema, ids, step, total).await?;
            let stage = FlowStage::Step(step);
            let ReadResult::TrxMapping(rows) = result else {
                return Err(failed(stage, "decode_error"));
            };
            if rows.len() > 3 {
                return Err(failed(stage, "decode_error"));
            }
            let mut mappings = Vec::with_capacity(rows.len());
            for (session, start_ts) in rows {
                let session = session
                    .filter(|n| *n != 0)
                    .ok_or_else(|| failed(stage, "decode_error"))?;
                if session != ids.waiter && session != ids.holder {
                    return Err(failed(stage, "decode_error"));
                }
                let start_ts = start_ts
                    .filter(|n| *n != 0)
                    .ok_or_else(|| failed(stage, "decode_error"))?;
                mappings.push(TrxMapping { session, start_ts });
            }
            validate_edge(ids, &mappings, None).map_err(|error| failed(stage, error))?;
            let step = ReadStep::Edge;
            let result = read_step(executor, engine, schema, ids, step, total).await?;
            let stage = FlowStage::Step(step);
            let ReadResult::Edges(rows) = result else {
                return Err(failed(stage, "decode_error"));
            };
            let edges = rows
                .into_iter()
                .map(|(waiter, holder)| {
                    let waiter = waiter
                        .filter(|n| *n != 0)
                        .ok_or_else(|| failed(stage, "decode_error"))?;
                    let holder = holder
                        .filter(|n| *n != 0)
                        .ok_or_else(|| failed(stage, "decode_error"))?;
                    Ok::<_, FlowFailure>((waiter, holder))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if edges.len() > 1 {
                // LIMIT 2 suffices to reject 2+, not to infer uniqueness from a sample.
                return Err(failed(stage, "identity_mismatch"));
            }
            validate_edge(ids, &mappings, edges.into_iter().next())
                .map_err(|error| failed(stage, error))
        }
    }
}

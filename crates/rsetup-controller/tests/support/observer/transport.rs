//! Dedicated, test-only SQLx reader; constructing it does not open a connection.
use super::{
    Account, ActorIds, AuthorizedRun, Bind, Engine, FlowFailure, FlowStage, PreparedObserver,
    QueryPlan, ReadExecutor, ReadFailure, ReadResult, ReadStep, SessionIdentity, authorize_open,
    bounded, preflight, sample_edge,
};
use sqlx::{
    Connection, MySqlConnection, Row,
    mysql::{MySqlConnectOptions, MySqlRow},
};
use std::{future::Future, time::Duration};
use tokio::time::Instant;

// Pure cardinality checks used by the actual SQLx row decoders, not a parallel fake reader.
pub(crate) fn exactly_one<T>(rows: Vec<T>) -> Result<T, ReadFailure> {
    let mut rows = rows.into_iter();
    match (rows.next(), rows.next()) {
        (Some(row), None) => Ok(row),
        _ => Err(ReadFailure::Decode),
    }
}

pub(crate) fn zero_or_one<T>(rows: Vec<T>) -> Result<Option<T>, ReadFailure> {
    let mut rows = rows.into_iter();
    let first = rows.next();
    if rows.next().is_some() {
        Err(ReadFailure::Decode)
    } else {
        Ok(first)
    }
}

fn decode_count(row: &MySqlRow) -> Result<ReadResult, ReadFailure> {
    Ok(ReadResult::Count(
        row.try_get::<Option<i64>, _>("matches")
            .map_err(|_| ReadFailure::Decode)?,
    ))
}

pub(crate) struct SqlxReadExecutor<'a> {
    connection: &'a mut MySqlConnection,
    engine: Engine,
}

impl<'a> SqlxReadExecutor<'a> {
    pub(crate) fn new(connection: &'a mut MySqlConnection, engine: Engine) -> Self {
        Self { connection, engine }
    }
}

impl ReadExecutor for SqlxReadExecutor<'_> {
    async fn read(&mut self, step: ReadStep, plan: QueryPlan) -> Result<ReadResult, ReadFailure> {
        // Reject a mismatched or noncanonical plan before even constructing a query.
        if !plan.validate(self.engine, step) {
            return Err(ReadFailure::Decode);
        }
        let mut query = sqlx::query(plan.sql());
        for bind in plan.consume_binds() {
            query = match bind {
                Bind::Unsigned(value) => query.bind(value),
                Bind::Text(value) => query.bind(value),
            };
        }
        let rows = query
            .fetch_all(&mut *self.connection)
            .await
            .map_err(|_| ReadFailure::Query)?;
        decode_rows(self.engine, step, rows)
    }
}

fn decode_rows(
    engine: Engine,
    step: ReadStep,
    rows: Vec<MySqlRow>,
) -> Result<ReadResult, ReadFailure> {
    use ReadStep as S;
    let decode = |_| ReadFailure::Decode;
    match step {
        S::Identity => {
            let row = exactly_one(rows)?;
            let (server_uuid, global_ids, txn_mode) = match engine {
                Engine::Mysql => (
                    row.try_get::<Option<String>, _>("server_uuid")
                        .map_err(decode)?,
                    None,
                    None,
                ),
                Engine::Tidb => (
                    None,
                    row.try_get::<Option<String>, _>("global_ids")
                        .map_err(decode)?,
                    row.try_get::<Option<String>, _>("txn_mode")
                        .map_err(decode)?,
                ),
            };
            Ok(ReadResult::Identity {
                connection_id: row
                    .try_get::<Option<u64>, _>("connection_id")
                    .map_err(decode)?,
                version: row
                    .try_get::<Option<String>, _>("version")
                    .map_err(decode)?,
                database: row
                    .try_get::<Option<String>, _>("observer_database")
                    .map_err(decode)?,
                current_user: row
                    .try_get::<Option<String>, _>("observer_current_user")
                    .map_err(decode)?,
                server_uuid,
                global_ids,
                txn_mode,
            })
        }
        S::Grants => rows
            .iter()
            .map(|row| row.try_get::<Option<String>, _>(0).map_err(decode))
            .collect::<Result<Vec<_>, _>>()
            .map(ReadResult::Grants),
        S::Roles => {
            let row = exactly_one(rows)?;
            Ok(ReadResult::Roles {
                current_role: row
                    .try_get::<Option<String>, _>("current_role")
                    .map_err(decode)?,
                mandatory_roles: if engine == Engine::Mysql {
                    row.try_get::<Option<String>, _>("mandatory_roles")
                        .map_err(decode)?
                } else {
                    None
                },
            })
        }
        S::MysqlWaits | S::MysqlLocks | S::MysqlThreads | S::TidbWaits | S::TidbTrx => {
            let row = zero_or_one(rows)?;
            let values = row
                .into_iter()
                .map(|r| r.try_get::<Option<i64>, _>("readable").map_err(decode))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ReadResult::Scan(values))
        }
        S::MysqlWaiterThread | S::MysqlHolderThread | S::Edge if engine == Engine::Mysql => {
            decode_count(&exactly_one(rows)?)
        }
        S::TidbMapping if engine == Engine::Tidb => {
            if rows.len() > 3 {
                return Err(ReadFailure::Decode);
            }
            rows.iter()
                .map(|r| {
                    Ok((
                        r.try_get::<Option<u64>, _>("observer_session")
                            .map_err(decode)?,
                        r.try_get::<Option<u64>, _>("start_ts").map_err(decode)?,
                    ))
                })
                .collect::<Result<Vec<_>, ReadFailure>>()
                .map(ReadResult::TrxMapping)
        }
        S::Edge if engine == Engine::Tidb => {
            if rows.len() > 2 {
                return Err(ReadFailure::Decode);
            }
            rows.iter()
                .map(|r| {
                    Ok((
                        r.try_get::<Option<u64>, _>("waiter_start_ts")
                            .map_err(decode)?,
                        r.try_get::<Option<u64>, _>("holder_start_ts")
                            .map_err(decode)?,
                    ))
                })
                .collect::<Result<Vec<_>, ReadFailure>>()
                .map(ReadResult::Edges)
        }
        _ => Err(ReadFailure::Decode),
    }
}

// No Debug/Display: this value owns precisely one physical connection and private identity.
pub(crate) struct ObserverSession {
    connection: MySqlConnection,
    identity: SessionIdentity,
    schema: String,
    engine: Engine,
}

impl ObserverSession {
    /// Caller prerequisite (O4/O6): actual_writer must come from a successful non-NULL
    /// CURRENT_USER() read on the actual authorized writer physical connection and be
    /// bound to this target/window; A and B must each be re-read and verified before
    /// concurrency. Neither config nor a fake string is acceptable live evidence.
    /// topology_current is likewise supplied only by current operator verification.
    pub(crate) async fn open(
        prepared: PreparedObserver,
        auth: Option<&AuthorizedRun>,
        requested_scope: &str,
        expected: &Account,
        actual_writer: &str,
        topology_current: bool,
        total: Instant,
    ) -> Result<Self, FlowFailure> {
        open_with(
            prepared,
            auth,
            requested_scope,
            expected,
            actual_writer,
            topology_current,
            total,
            |options| async move {
                MySqlConnection::connect_with(&options)
                    .await
                    .map_err(|_| "query_error")
            },
        )
        .await
    }

    pub(crate) fn identity(&self) -> &SessionIdentity {
        &self.identity
    }

    /// Uses this same physical connection, with caller-provided 20s absolute deadline.
    /// It never commits, performs CAS, retries, or starts another connection.
    pub(crate) async fn sample_edge(
        &mut self,
        ids: ActorIds,
        total: Instant,
    ) -> Result<bool, FlowFailure> {
        let mut executor = SqlxReadExecutor::new(&mut self.connection, self.engine);
        sample_edge(&mut executor, self.engine, &self.schema, ids, total).await
    }
}

// The only open control flow; production supplies SQLx, offline tests supply a
// no-network connector. Neither branch can reach connector before the gate.
// The eighth argument is precisely the injected connector; the other seven
// deliberately mirror ObserverSession::open to prevent gate/control-flow drift.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn open_with<F, Fut>(
    prepared: PreparedObserver,
    auth: Option<&AuthorizedRun>,
    requested_scope: &str,
    expected: &Account,
    actual_writer: &str,
    topology_current: bool,
    total: Instant,
    connect: F,
) -> Result<ObserverSession, FlowFailure>
where
    F: FnOnce(MySqlConnectOptions) -> Fut,
    Fut: Future<Output = Result<MySqlConnection, super::ObserverError>>,
{
    authorize_open(&prepared, auth, expected, requested_scope, total)?;
    if actual_writer.is_empty() {
        return Err(FlowFailure {
            stage: FlowStage::Prerequisite,
            error_class: "identity_mismatch",
        });
    }
    let engine = prepared.engine();
    if engine == Engine::Tidb && !topology_current {
        return Err(FlowFailure {
            stage: FlowStage::Prerequisite,
            error_class: "prerequisite_missing",
        });
    }
    let deadline = std::cmp::min(
        total,
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .unwrap_or(total),
    );
    let mut connection = bounded(
        async {
            connect(prepared.options().clone())
                .await
                .map_err(|_| "query_error")
        },
        deadline,
        Duration::from_secs(10),
    )
    .await
    .map_err(|error_class| FlowFailure {
        stage: FlowStage::Connect,
        error_class,
    })?;
    let schema = prepared.schema().to_owned();
    let mut executor = SqlxReadExecutor::new(&mut connection, engine);
    let identity = preflight(
        &mut executor,
        engine,
        &schema,
        expected,
        actual_writer,
        topology_current,
        deadline,
    )
    .await?;
    Ok(ObserverSession {
        connection,
        identity,
        schema,
        engine,
    })
}

//! Fixed, test-only observer query catalog; no connection or SQL execution.
use super::{ActorIds, Engine, ObserverError};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadStep {
    Identity,
    Grants,
    Roles,
    MysqlWaits,
    MysqlLocks,
    MysqlThreads,
    TidbWaits,
    TidbTrx,
    MysqlWaiterThread,
    MysqlHolderThread,
    TidbMapping,
    Edge,
}

// Neither bind values nor complete plans implement Debug/Display.
#[derive(PartialEq, Eq)]
pub(crate) enum Bind {
    Unsigned(u64),
    Text(String),
}

// Fields and the sole constructor remain private to this module: callers cannot
// supply SQL, replace typed binds, or forge catalog provenance.
pub(crate) struct QueryPlan {
    sql: &'static str,
    binds: Vec<Bind>,
    engine: Engine,
    step: ReadStep,
    schema: String,
    ids: ActorIds,
}

impl QueryPlan {
    pub(crate) fn sql(&self) -> &'static str {
        self.sql
    }

    pub(crate) fn binds(&self) -> &[Bind] {
        &self.binds
    }

    pub(crate) fn consume_binds(self) -> Vec<Bind> {
        self.binds
    }

    // Pure, closed validation: called before constructing any SQLx query. Rebuild
    // the catalog entry with the original inputs, not caller-provided SQL.
    pub(crate) fn validate(&self, engine: Engine, step: ReadStep) -> bool {
        self.engine == engine
            && self.step == step
            && query_plan(engine, step, &self.schema, self.ids)
                .is_ok_and(|canonical| self.sql == canonical.sql && self.binds == canonical.binds)
    }
}

pub(crate) fn query_plan(
    engine: Engine,
    step: ReadStep,
    schema: &str,
    ids: ActorIds,
) -> Result<QueryPlan, ObserverError> {
    use ReadStep as S;
    if schema.is_empty() {
        return Err("shape_rejected");
    }
    let actor = |id| {
        (id != 0)
            .then_some(vec![Bind::Unsigned(id)])
            .ok_or("identity_mismatch")
    };
    let pair = || {
        if ids.waiter == 0 || ids.holder == 0 || ids.waiter == ids.holder {
            return Err("identity_mismatch");
        }
        Ok(vec![Bind::Unsigned(ids.waiter), Bind::Unsigned(ids.holder)])
    };
    let (sql, binds) = match (engine, step) {
        (Engine::Mysql, S::Identity) => (
            "SELECT CAST(CONNECTION_ID() AS UNSIGNED) AS connection_id,CAST(VERSION() AS CHAR) AS version,CAST(DATABASE() AS CHAR) AS `observer_database`,CAST(CURRENT_USER() AS CHAR) AS `observer_current_user`,CAST(@@GLOBAL.server_uuid AS CHAR) AS server_uuid",
            vec![],
        ),
        (Engine::Tidb, S::Identity) => (
            "SELECT CAST(CONNECTION_ID() AS UNSIGNED) AS connection_id,CAST(VERSION() AS CHAR) AS version,CAST(DATABASE() AS CHAR) AS `observer_database`,CAST(CURRENT_USER() AS CHAR) AS `observer_current_user`,JSON_UNQUOTE(JSON_EXTRACT(@@GLOBAL.tidb_config,'$.\"enable-global-kill\"')) AS global_ids,CAST(@@SESSION.tidb_txn_mode AS CHAR) AS txn_mode",
            vec![],
        ),
        (_, S::Grants) => ("SHOW GRANTS", vec![]),
        (Engine::Mysql, S::Roles) => (
            "SELECT CAST(CURRENT_ROLE() AS CHAR) AS current_role,CAST(@@GLOBAL.mandatory_roles AS CHAR) AS mandatory_roles",
            vec![],
        ),
        (Engine::Tidb, S::Roles) => (
            "SELECT CAST(CURRENT_ROLE() AS CHAR) AS current_role",
            vec![],
        ),
        (Engine::Mysql, S::MysqlWaits) => (
            "SELECT CAST(1 AS SIGNED) AS readable FROM performance_schema.data_lock_waits LIMIT 1",
            vec![],
        ),
        (Engine::Mysql, S::MysqlLocks) => (
            "SELECT CAST(1 AS SIGNED) AS readable FROM performance_schema.data_locks LIMIT 1",
            vec![],
        ),
        (Engine::Mysql, S::MysqlThreads) => (
            "SELECT CAST(1 AS SIGNED) AS readable FROM performance_schema.threads LIMIT 1",
            vec![],
        ),
        (Engine::Tidb, S::TidbWaits) => (
            "SELECT CAST(1 AS SIGNED) AS readable FROM information_schema.DATA_LOCK_WAITS LIMIT 1",
            vec![],
        ),
        (Engine::Tidb, S::TidbTrx) => (
            "SELECT CAST(1 AS SIGNED) AS readable FROM information_schema.CLUSTER_TIDB_TRX LIMIT 1",
            vec![],
        ),
        (Engine::Mysql, S::MysqlWaiterThread) => (
            "SELECT CAST(COUNT(*) AS SIGNED) AS matches FROM performance_schema.threads WHERE PROCESSLIST_ID=CAST(? AS UNSIGNED)",
            actor(ids.waiter)?,
        ),
        (Engine::Mysql, S::MysqlHolderThread) => (
            "SELECT CAST(COUNT(*) AS SIGNED) AS matches FROM performance_schema.threads WHERE PROCESSLIST_ID=CAST(? AS UNSIGNED)",
            actor(ids.holder)?,
        ),
        (Engine::Tidb, S::TidbMapping) => (
            // At most three rows: enough to expose ambiguity for the two actors.
            "SELECT CAST(SESSION_ID AS UNSIGNED) AS `observer_session`,CAST(ID AS UNSIGNED) AS start_ts FROM information_schema.CLUSTER_TIDB_TRX WHERE SESSION_ID IN (CAST(? AS UNSIGNED),CAST(? AS UNSIGNED)) LIMIT 3",
            pair()?,
        ),
        (Engine::Mysql, S::Edge) => (
            "SELECT CAST(COUNT(*) AS SIGNED) AS matches FROM performance_schema.data_lock_waits AS w JOIN performance_schema.threads AS wt ON wt.THREAD_ID=w.REQUESTING_THREAD_ID JOIN performance_schema.threads AS ht ON ht.THREAD_ID=w.BLOCKING_THREAD_ID JOIN performance_schema.data_locks AS dl ON dl.ENGINE=w.ENGINE AND dl.ENGINE_LOCK_ID=w.REQUESTING_ENGINE_LOCK_ID WHERE w.ENGINE='INNODB' AND wt.PROCESSLIST_ID=CAST(? AS UNSIGNED) AND ht.PROCESSLIST_ID=CAST(? AS UNSIGNED) AND dl.LOCK_STATUS='WAITING' AND CAST(dl.OBJECT_SCHEMA AS BINARY)=CAST(? AS BINARY) AND CAST(dl.OBJECT_NAME AS BINARY)=CAST('schema_meta' AS BINARY)",
            with_schema(pair()?, schema),
        ),
        (Engine::Tidb, S::Edge) => (
            // Only current transaction IDs leave the server, not KEY_INFO itself.
            "SELECT CAST(w.TRX_ID AS UNSIGNED) AS waiter_start_ts,CAST(w.CURRENT_HOLDING_TRX_ID AS UNSIGNED) AS holder_start_ts FROM information_schema.DATA_LOCK_WAITS AS w JOIN information_schema.CLUSTER_TIDB_TRX AS wt ON w.TRX_ID=wt.ID JOIN information_schema.CLUSTER_TIDB_TRX AS ht ON w.CURRENT_HOLDING_TRX_ID=ht.ID WHERE wt.SESSION_ID=CAST(? AS UNSIGNED) AND ht.SESSION_ID=CAST(? AS UNSIGNED) AND CAST(JSON_UNQUOTE(JSON_EXTRACT(CASE WHEN JSON_VALID(w.KEY_INFO) THEN w.KEY_INFO ELSE NULL END,'$.db_name')) AS BINARY)=CAST(? AS BINARY) AND CAST(JSON_UNQUOTE(JSON_EXTRACT(CASE WHEN JSON_VALID(w.KEY_INFO) THEN w.KEY_INFO ELSE NULL END,'$.table_name')) AS BINARY)=CAST('schema_meta' AS BINARY) LIMIT 2",
            with_schema(pair()?, schema),
        ),
        _ => return Err("shape_rejected"),
    };
    Ok(QueryPlan {
        sql,
        binds,
        engine,
        step,
        schema: schema.to_owned(),
        ids,
    })
}

fn with_schema(mut binds: Vec<Bind>, schema: &str) -> Vec<Bind> {
    binds.push(Bind::Text(schema.to_owned()));
    binds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_tampering_cannot_pass_canonical_validation() {
        let engine = Engine::Mysql;
        let step = ReadStep::Edge;
        let ids = ActorIds {
            waiter: 22,
            holder: 11,
        };
        let mut plan = query_plan(engine, step, "Exact_DB", ids).unwrap();
        assert!(plan.validate(engine, step));
        plan.sql = "SET @x=1";
        assert!(!plan.validate(engine, step));
        let mut plan = query_plan(engine, step, "Exact_DB", ids).unwrap();
        plan.binds.pop();
        assert!(!plan.validate(engine, step));
    }
}

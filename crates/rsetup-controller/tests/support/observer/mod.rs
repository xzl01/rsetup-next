//! Test-only observer contract; never exported by the controller library.
// The loader only accepts already-owned fds; raw adoption belongs to O4/O6.
#[allow(dead_code)]
mod authorization;
#[allow(dead_code)]
mod config;
#[allow(dead_code)]
mod flow;
mod grants;
#[cfg(target_os = "linux")]
#[allow(dead_code)]
mod handoff;
#[allow(dead_code)]
mod queries;
#[allow(dead_code)]
mod session;
#[allow(unused_imports)]
pub(crate) use authorization::{AuthorizedRun, authorize_open, read_authorization};
#[allow(unused_imports)]
pub(crate) use config::{PinnedInput, PreparedObserver, load_inherited, prepare};
#[allow(unused_imports)]
pub(crate) use flow::{
    FlowFailure, FlowStage, ReadExecutor, ReadFailure, ReadResult, preflight, sample_edge,
};
pub(crate) use grants::{validate_grants, validate_identity};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub(crate) use handoff::{InheritedChildFds, adopt_child_or_exit};
#[allow(unused_imports)]
pub(crate) use queries::{Bind, QueryPlan, ReadStep, query_plan};
#[allow(unused_imports)]
pub(crate) use session::{
    ActorIds, SessionIdentity, TrxMapping, bounded, validate_edge, validate_sessions,
};

#[allow(dead_code)]
mod transport;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use transport::open_with;
#[allow(unused_imports)]
pub(crate) use transport::{ObserverSession, SqlxReadExecutor, exactly_one, zero_or_one};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Engine {
    Mysql,
    Tidb,
}

pub(crate) type ObserverError = &'static str;

// Deliberately no Debug: authorized account values are private inputs.
pub(crate) struct Account {
    pub(crate) user: String,
    pub(crate) host: String,
}

//! 板端代理：只读白名单分发、任务 journal、票据与无参 OS 重启边界。
//! 本 crate 不包含任何密码学实现、不建立真实网络连接。

pub mod actions;
pub mod dispatch;
pub mod journal;
pub mod read_only;

pub use actions::{
    ACTION_ALLOWLIST, ActionKind, ActionSpec, allowed_action_names, is_reserved, lookup_action,
};
pub use dispatch::{
    BoardAgent, BusinessResponse, BusinessStatus, JournalEpochSource, TaskJournalReader,
};
pub use journal::{
    DedupOutcome, EvidenceMerge, Fs, JOURNAL_MAX_RECORDS, JOURNAL_SCHEMA_VERSION, JournalError,
    JournalRecovery, JournalStore, RealFs, TaskJournal, TaskRecord, TaskState,
};
pub use read_only::{PeerContext, ReadOnlyMutationGuard, ReadOnlySource};

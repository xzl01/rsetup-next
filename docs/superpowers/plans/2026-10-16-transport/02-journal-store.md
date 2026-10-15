### Task 3: 幂等任务 journal —— 稳定 epoch、accepted+pre_boot、两阶段 fsync 与失败注入

**Files:**
- Create: `crates/rsetup-board-agent/src/journal.rs`
- Modify: `crates/rsetup-board-agent/src/lib.rs`
- Create: `crates/rsetup-board-agent/src/test_seam.rs`（`#[cfg(any(test, feature = "test-seam"))]`）
- Modify: `crates/rsetup-board-agent/Cargo.toml`（在 Task 2 既有 `[features]` 表内补 `test-seam = []` 行，不重复表头）
- Create: `crates/rsetup-board-agent/tests/support/mod.rs`
- Create: `crates/rsetup-board-agent/tests/support/tempdir.rs`
- Test: `crates/rsetup-board-agent/tests/wire_04_journal.rs`
- Test: `crates/rsetup-board-agent/tests/journal_fault_injection.rs`

> 拆分为三次提交（3.1 稳定 epoch 与 accepted/pre_boot；3.2 两阶段 fsync 与失败注入；3.3 去重/容量/恢复）。

**Interfaces:**
```rust
// ---- journal.rs ----
pub const JOURNAL_SCHEMA_VERSION: u32 = 1;
pub const JOURNAL_MAX_RECORDS: usize = 4096; // 03 §5 待审默认，G0 审阅后才可定稿

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState { Accepted, Attempted, Succeeded, Failed, Unknown }

impl TaskState {
    pub fn as_str(self) -> &'static str;
    pub fn from_str_exact(value: &str) -> Option<TaskState>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRecord {
    pub schema_version: u32,
    pub sub_task_id: String,
    pub journal_epoch: String,
    pub record_version: u64,
    pub state: TaskState,
    pub pre_boot_id: String,
    pub observed_boot_id: Option<String>,
    pub reason_code: Option<String>,
    pub updated_unix_ms: Option<i64>,
    pub evidence_kind: Option<String>,
}

impl TaskRecord {
    pub fn to_wire(&self) -> rsetup_protocol::wire::TaskRecord;
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum JournalError {
    /// rename 调用之前确定失败（创建临时文件、写入、临时文件 sync_file 失败）：本次状态变更确未提交
    #[error("write definitely failed before commit")]
    DefinitelyNotCommitted,
    /// rename 调用本身失败或 rename 之后父目录 sync 失败：提交结果不明
    #[error("write state unknown, may or may not be committed")]
    CommitUnknown,
    #[error("journal storage full")]
    StorageFull,
    #[error("io error: {0}")]
    Io(String),
    #[error("journal decode error: {0}")]
    Decode(String),
    #[error("record not found")]
    NotFound,
    #[error("same sub_task_id with different payload hash")]
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DedupOutcome { Inserted, ReusedExisting }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalRecovery {
    /// 普通启动：epoch 保留，不做任何自动状态改写
    ReusedEpoch,
    /// 日志丢失/不可逆损坏后的重建：epoch 已更换，必须向中控报告 JOURNAL_LOST
    RebuiltAfterLoss { journal_lost: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvidenceMerge {
    /// 证据版本更高，已采用
    Applied(TaskRecord),
    /// 迟到/重复的较低版本，按协议丢弃
    StaleVersion(TaskRecord),
    /// 同版本不同内容：一致性异常，绝不覆盖
    Conflict(TaskRecord),
}

/// 最小文件操作缝：生产 `RealFs`，测试 `RecordingJournal` / `FaultyFs`。
/// 每次提交的固定调用序为
/// `create_write -> write_all -> sync_file -> rename -> sync_dir -> remove_file -> sync_dir`。
pub trait Fs: Send + Sync {
    fn create_dir_all(&self, dir: &std::path::Path) -> std::io::Result<()>;
    fn create_write(&self, path: &std::path::Path) -> std::io::Result<Box<dyn std::io::Write + Send>>;
    fn read(&self, path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>>;
    fn sync_file(&self, path: &std::path::Path) -> std::io::Result<()>;
    fn rename(&self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()>;
    fn remove_file(&self, path: &std::path::Path) -> std::io::Result<()>;
    fn sync_dir(&self, dir: &std::path::Path) -> std::io::Result<()>;
}

pub struct RealFs;
impl Fs for RealFs { /* std::fs::* + File::sync_all / File::open(dir).sync_all() */ }

pub struct JournalStore<F: Fs> { /* 私有：dir, epoch, fs, records: Mutex<Vec<StoredRecord>>, uncertain: std::sync::atomic::AtomicBool */ }

/// 类型别名：提供给不需要替换文件后端的生产调用方，兼容上游 transport 计划的命名习惯。
pub type TaskJournal = JournalStore<RealFs>;

/// **uncertain 隔离契约（TR-N2 / 正审 R-02，fail-closed）**：`write_durable` 返回 `CommitUnknown`
/// （rename 调用本身失败，或 rename 后父目录 `sync_all` 失败）时，磁盘可能已落地新记录而内存未换，
/// 二者不再可比。该 store 实例必须在向调用方返回该错误之前置 `uncertain = true`，进入不可逆隔离态：
/// 此后 `get`、`payload_hash`、`insert_accepted`、`mark_attempted`、`finish`、`finish_succeeded_with_boot`、
/// `merge_evidence` 一律在加锁与任何 I/O 之前 fail-closed，返回固定去敏
/// `JournalError::Io("journal store in uncertain state: CommitUnknown requires reopen or controlled rebuild".to_string())`，
/// 绝不把旧内存快照当确定事实返回，也绝不据其发起新的整文件提交（否则可能覆盖磁盘上已提交的新记录，
/// 破坏同 epoch 内 `record_version` 与证据单调性）。`DefinitelyNotCommitted` 不置位（磁盘确定未变）。
/// 受控恢复仅限：丢弃该实例后重新 `open`/`recover`（走持久校验与一致性加载），或显式 `rebuild_after_loss`；
/// 核实持久真相前不得继续服务，`uncertain` 不自动清除。
impl<F: Fs> JournalStore<F> {
    pub const META_FILE: &'static str = "journal-meta.json";
    pub const LOG_FILE: &'static str = "journal-logs.json";

    /// 首次初始化：生成并持久化 `journal_epoch`；已存在则**沿用**已持久化 epoch。
    pub fn open(dir: &std::path::Path, fs: F) -> Result<Self, JournalError>;
    /// 普通启动：同 `open`，并对已持久化记录做只读兼容检查；**绝不做自动状态改写**。
    pub fn recover(dir: &std::path::Path, fs: F, current_boot_id: &str) -> Result<Self, JournalError>;
    /// 显式重建路径（仅在已知日志不可逆损坏时调用）：更换 `journal_epoch`，旧记录不可再比较。
    pub fn rebuild_after_loss(dir: &std::path::Path, fs: F) -> Result<(Self, JournalRecovery), JournalError>;

    pub fn journal_epoch(&self) -> &str;
    pub fn get(&self, controller_key: &[u8; 32], sub_task_id: &str) -> Result<Option<TaskRecord>, JournalError>;
    pub fn record_count(&self) -> usize;

    /// 去重查询（只读，无 I/O）：返回记录 `(controller_key, sub_task_id)` 保存的规范载荷哈希。
    /// 候选 = `controller_hex == encode_journal_hash32(controller_key)`（lowerhex64）且 `sub_task_id` 精确匹配；
    /// 0 命中 → `Ok(None)`；恰 1 命中 → `decode_journal_hash32` 解码返回 `Ok(Some(hash))`；
    /// >1 命中（同键重复记录，属损坏）→ 固定去敏 `Decode("journal.duplicate_record")`（绝不返回第一条、不伪 None）。
    /// 不改 records/epoch/version、不把哈希写入日志或 HTTP；复用本文件 3.1g 的 hex helper（不重定义）；
    /// 毒锁照抄本文件 Result 方法既定 fail-closed 策略（同 `TicketManager::consume`）：调用直接返回、不 panic、不吞毒继续读；
    /// std `Mutex` 毒锁持久，本子步不实现 `clear_poison`/重建恢复、不新增错误变体（journal poison 完整契约与
    /// FaultyFs/FakeJournal 具体故障用例仍未补，本子步不扩）。
    pub fn payload_hash(&self, controller_key: &[u8; 32], sub_task_id: &str) -> Result<Option<[u8; 32]>, JournalError> {
        // uncertain 隔离态先于一切（TR-N2）：绝不回旧内存哈希
        if self.uncertain.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JournalError::Io("journal store in uncertain state: CommitUnknown requires reopen or controlled rebuild".to_string()));
        }
        let expected_controller_hex = encode_journal_hash32(controller_key);
        let records = match self.records.lock() {
            Ok(guard) => guard,
            // fail-closed：毒锁 → 直接返回（消息去敏，不含记录内容）
            Err(_) => return Err(JournalError::Io("journal.records poisoned".to_string())),
        };
        if self.uncertain.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JournalError::Io("journal store in uncertain state: CommitUnknown requires reopen or controlled rebuild".to_string()));
        }
        let hits: Vec<&StoredRecord> = records
            .iter()
            .filter(|r| r.controller_hex == expected_controller_hex && r.sub_task_id == sub_task_id)
            .collect();
        match hits.len() {
            0 => Ok(None),
            1 => decode_journal_hash32(&hits[0].payload_hash_hex).map(Some),
            // 同键重复记录 = 损坏：固定去敏错误（绝不返回第一条、不伪 None）
            _ => Err(JournalError::Decode("journal.duplicate_record".to_owned())),
        }
    }

    /// 供 reopen 测试：返回本实例已打开的 journal 目录路径（不改 dir 路径值，也不改实际 Fs 后端）。
    pub fn dir(&self) -> &std::path::Path { &self.dir }

    /// accepted：`pre_boot_id` 必须在本次调用内即确定并落盘。
    pub fn insert_accepted(
        &self,
        controller_key: &[u8; 32],
        sub_task_id: &str,
        payload_hash: &[u8; 32],
        pre_boot_id: &str,
    ) -> Result<(TaskRecord, DedupOutcome), JournalError>;

    pub fn mark_attempted(&self, controller_key: &[u8; 32], sub_task_id: &str) -> Result<TaskRecord, JournalError>;

    pub fn finish(
        &self,
        controller_key: &[u8; 32],
        sub_task_id: &str,
        state: TaskState,
        reason: Option<&str>,
    ) -> Result<TaskRecord, JournalError>;

    /// 完成路径：`succeeded` 必须同时写入 `observed_boot_id` 与 `evidence_kind = "boot_transition_observed"`。
    pub fn finish_succeeded_with_boot(
        &self,
        controller_key: &[u8; 32],
        sub_task_id: &str,
        observed_boot_id: &str,
    ) -> Result<TaskRecord, JournalError>;

    /// 只读恢复：对「有可靠 attempted、且 pre_boot_id != current_boot_id」的记录原子写入 succeeded 证据；
    /// 其余情况返回原状态，绝不自动补调用、绝不自动重发。
    pub fn apply_boot_transition_evidence(
        &self,
        current_boot_id: &str,
    ) -> Result<Vec<TaskRecord>, JournalError>;

    /// 证据合并：仅在同一 `journal_epoch` 内比较版本；跨 epoch、`sub_task_id` 或
    /// `schema_version` 不一致，一律返回 `Conflict(stored)`；
    /// TR-R10-1 已存终态全封闭守卫（04 规格 L69–70、L119–124）：已存记录处于终态
    /// （Failed/Succeeded）时，任何后续状态跃迁——无论 incoming 是 Accepted、Attempted、
    /// Unknown 还是矛盾的另一终态，无论版本多高——一律直接返回 `Conflict(stored)`，
    /// 零写盘、不改版本，原终态记录及其证据绝对不可被覆盖或降级
    /// （封死 Failed→Unknown→Succeeded 两步中继绕过）；
    /// 仅当已存记录处于中间态（Accepted/Attempted/Unknown）时才允许终态收敛；incoming 为
    /// `Succeeded` 时必须携带 `evidence_kind == Some("boot_transition_observed")` 且
    /// `observed_boot_id` 存在并与 `pre_boot_id` 不同，否则同样 `Conflict(stored)`；
    /// 全部守卫通过后才比较版本：低版本丢弃；同版本不同内容报一致性异常；高版本采用。
    pub fn merge_evidence(
        &self,
        controller_key: &[u8; 32],
        incoming: &TaskRecord,
    ) -> Result<EvidenceMerge, JournalError>;
}
```

- [ ] **Step 3.1: 建立可编译桩、编写失败测试、RED、最小实现（稳定 epoch 与 accepted/pre_boot）**

   a. 创建 `tests/support/tempdir.rs`（替代未获批的 `tempfile`）：
```rust
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 纯标准库临时目录守卫；Drop 时在验证独立临时根绝对路径后递归清理，防误删。
pub struct TempDir { path: PathBuf }

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let seq = COUNTER.fetch_add(1, Ordering::SeqCst);
        let prefix = format!("rsetup-board-agent-{}-{}-{seq}", tag, std::process::id());
        let root = std::env::temp_dir();
        let path = root.join(&prefix);
        let abs_path = if path.is_absolute() {
            path
        } else {
            root.canonicalize().unwrap_or(root).join(path)
        };
        std::fs::create_dir_all(&abs_path).expect("create temp dir");
        Self { path: abs_path }
    }
    pub fn path(&self) -> &Path { &self.path }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path { &self.path }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // 安全防护门禁：严格验证绝对路径位于系统临时目录内且持有专用前缀，防误删任意主机目录
        let temp_root = std::env::temp_dir();
        let is_safe = self.path.is_absolute()
            && self.path.starts_with(&temp_root)
            && self
                .path
                .file_name()
                .and_then(|f| f.to_str())
                .map(|name| name.starts_with("rsetup-board-agent-"))
                .unwrap_or(false);

        if is_safe {
            if let Err(err) = std::fs::remove_dir_all(&self.path) {
                // 错误可诊断：在非 NotFound 情况下打印诊断信息，不静默吞掉真实 IO 错误
                if err.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("diagnostics: failed to clean up TempDir at {:?}: {err}", self.path);
                }
            }
        } else {
            eprintln!(
                "safety violation: refusing to remove path {:?} not matching safe temp root",
                self.path
            );
        }
    }
}
```
   创建 `tests/support/mod.rs`：
```rust
#[path = "tempdir.rs"]
pub mod tempdir;
```
   并在需要它的测试文件顶部声明：
```rust
mod support;
use support::tempdir::TempDir;
```
   > 说明：`tests/` 下每个 `.rs` 是独立 crate，`tests/support/` 目录本身不会被当作测试目标；`mod support;` 通过 `support/mod.rs` 引入。**不得**因此引入 `tempfile`。

   b. 在 `crates/rsetup-board-agent/Cargo.toml` **Task 2 已定义的既有 `[features]` 表内**追加所需 feature 行；**不得再次写 `[features]` 表头**——TOML 同名表重复会使 manifest 解析失败。Task 2 的 manifest 已声明 `default = []` 与 `test-seam = []`，本步只确认其存在，缺失才补行：
```toml
# 既有 [features] 表（Task 2 创建）内的增量行，不重复表头
test-seam = []
```

   c. `src/journal.rs` 先写可编译桩：结构体、`TaskState`、`TaskRecord`、`JournalError`、`DedupOutcome`、`JournalRecovery`、`EvidenceMerge`、`Fs`、`RealFs`（真实实现）全部就位；`JournalStore` 的 `open`/`recover`/`get`/`insert_accepted` 为桩：
```rust
pub fn open(_dir: &Path, _fs: F) -> Result<Self, JournalError> {
    Err(JournalError::NotFound) // 桩：让「epoch 稳定」断言失败
}
pub fn get(&self, _key: &[u8; 32], _id: &str) -> Result<Option<TaskRecord>, JournalError> { Ok(None) }
pub fn insert_accepted(
    &self, _key: &[u8; 32], _id: &str, _hash: &[u8; 32], _pre: &str,
) -> Result<(TaskRecord, DedupOutcome), JournalError> {
    Err(JournalError::NotFound) // 桩
}
pub fn journal_epoch(&self) -> &str { "stub-epoch" }
pub fn record_count(&self) -> usize { 0 }
pub fn dir(&self) -> &Path { &self.dir } // 供 reopen 测试：返回已打开的 journal 目录
```

   d. `tests/wire_04_journal.rs` 契约测试（本步只要求 RED 的是第 1 个）：
```rust
mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::journal::{
    DedupOutcome, EvidenceMerge, JOURNAL_MAX_RECORDS, JournalError, JournalRecovery, JournalStore, RealFs, TaskState,
};
use rsetup_protocol::wire::BUSINESS_SCHEMA_VERSION;

const BOOT_OLD: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const BOOT_NEW: &str = "6f8b1c2d-0000-4000-8000-000000000002";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

fn open_store(dir: &TempDir) -> JournalStore<RealFs> {
    JournalStore::open(dir.path(), RealFs).expect("open journal")
}

#[test]
fn journal_epoch_is_stable_across_normal_reopen() {
    let dir = TempDir::new("epoch");
    let key = [9u8; 32];

    let first = open_store(&dir);
    let epoch = first.journal_epoch().to_string();
    assert!(rsetup_protocol::wire::is_canonical_uuid_v4(&epoch), "epoch must be a uuid: {epoch}");

    let (record, outcome) = first
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("insert accepted");
    assert_eq!(outcome, DedupOutcome::Inserted);
    assert_eq!(record.state, TaskState::Accepted);
    assert_eq!(record.pre_boot_id, BOOT_OLD, "pre_boot_id must be known at accepted");
    assert_eq!(record.journal_epoch, epoch);
    assert_eq!(record.record_version, 1);
    assert_eq!(record.schema_version, BUSINESS_SCHEMA_VERSION);
    drop(first);

    // 普通进程重启：epoch 绝不刷新，记录仍在
    let second = open_store(&dir);
    assert_eq!(second.journal_epoch(), epoch, "normal reopen must reuse the persisted epoch");
    let reread = second.get(&key, SUB_TASK).expect("get").expect("record must survive reopen");
    assert_eq!(reread.state, TaskState::Accepted);
    assert_eq!(reread.pre_boot_id, BOOT_OLD);
}

#[test]
fn journal_epoch_changes_only_on_explicit_rebuild_after_loss() {
    let dir = TempDir::new("rebuild");
    let first = open_store(&dir);
    let epoch = first.journal_epoch().to_string();
    drop(first);

    let (rebuilt, outcome) = JournalStore::rebuild_after_loss(dir.path(), RealFs).expect("rebuild");
    assert_ne!(rebuilt.journal_epoch(), epoch, "loss rebuild must rotate the epoch");
    assert_eq!(outcome, JournalRecovery::RebuiltAfterLoss { journal_lost: true });
}

#[test]
fn accepted_reopen_never_downgrades_state() {
    let dir = TempDir::new("no-auto");
    let key = [9u8; 32];
    {
        let store = open_store(&dir);
        store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    }
    // 恢复时当前 boot 未变：既不得自动补调用，也不得擅自改写状态
    let recovered = JournalStore::recover(dir.path(), RealFs, BOOT_OLD).expect("recover");
    let record = recovered.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(record.state, TaskState::Accepted, "accepted without attempted must not be auto-advanced");
    assert_eq!(record.observed_boot_id, None);
}

#[test]
fn attempted_record_with_changed_boot_becomes_succeeded_with_evidence() {
    let dir = TempDir::new("boot-change");
    let key = [9u8; 32];
    {
        let store = open_store(&dir);
        store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
        let attempted = store.mark_attempted(&key, SUB_TASK).expect("attempted");
        assert_eq!(attempted.state, TaskState::Attempted);
        assert_eq!(attempted.pre_boot_id, BOOT_OLD);
    }

    let recovered = JournalStore::recover(dir.path(), RealFs, BOOT_NEW).expect("recover");
    assert_eq!(
        recovered.journal_epoch(),
        open_store(&dir).journal_epoch(),
        "boot transition must not rotate the epoch"
    );
    let record = recovered.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(record.state, TaskState::Succeeded);
    assert_eq!(record.observed_boot_id.as_deref(), Some(BOOT_NEW));
    assert_eq!(record.evidence_kind.as_deref(), Some("boot_transition_observed"));
    assert!(record.record_version > 2, "evidence application must bump the version");
}

#[test]
fn attempted_record_with_same_boot_stays_attempted() {
    let dir = TempDir::new("boot-same");
    let key = [9u8; 32];
    {
        let store = open_store(&dir);
        store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
        store.mark_attempted(&key, SUB_TASK).expect("attempted");
    }
    let recovered = JournalStore::recover(dir.path(), RealFs, BOOT_OLD).expect("recover");
    let record = recovered.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(record.state, TaskState::Attempted, "same boot is not success evidence");
    assert_eq!(record.observed_boot_id, None);
}

#[test]
fn duplicate_sub_task_id_returns_record_and_conflicting_payload_is_rejected() {
    let dir = TempDir::new("dedup");
    let key = [9u8; 32];
    let store = open_store(&dir);

    let (first, outcome) = store.insert_accepted(&key, SUB_TASK, &[1u8; 32], BOOT_OLD).expect("first");
    assert_eq!(outcome, DedupOutcome::Inserted);

    let (again, outcome) = store.insert_accepted(&key, SUB_TASK, &[1u8; 32], BOOT_OLD).expect("replay");
    assert_eq!(outcome, DedupOutcome::ReusedExisting);
    assert_eq!(again.record_version, first.record_version, "replay must not bump the version");
    assert_eq!(again.pre_boot_id, BOOT_OLD);

    assert_eq!(
        store.insert_accepted(&key, SUB_TASK, &[2u8; 32], BOOT_OLD),
        Err(JournalError::Conflict)
    );

    // 不同 controller 命名空间互不影响
    let (other, outcome) = store.insert_accepted(&[7u8; 32], SUB_TASK, &[2u8; 32], BOOT_OLD).expect("other key");
    assert_eq!(outcome, DedupOutcome::Inserted);
    assert_eq!(other.sub_task_id, SUB_TASK);
}

#[test]
fn full_journal_rejects_new_records_without_evicting_dedup_entries() {
    let dir = TempDir::new("capacity");
    let store = open_store(&dir);
    let key = [3u8; 32];
    for i in 0..JOURNAL_MAX_RECORDS {
        let id = format!("00000000-0000-4000-8000-{i:012}");
        store.insert_accepted(&key, &id, &[0u8; 32], BOOT_OLD).expect("fill");
    }
    assert_eq!(store.record_count(), JOURNAL_MAX_RECORDS);
    assert_eq!(
        store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD),
        Err(JournalError::StorageFull)
    );
    // 首版绝不淘汰旧去重记录
    assert_eq!(store.record_count(), JOURNAL_MAX_RECORDS);
    let first_id = "00000000-0000-4000-8000-000000000000";
    assert!(store.get(&key, first_id).expect("get").is_some());
}

#[test]
fn finish_records_reason_and_merge_rules() {
    let dir = TempDir::new("finish");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store.finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR")).expect("finish");
    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(failed.reason_code.as_deref(), Some("OS_ERROR"));

    // 同版本不同内容 -> Conflict；更低版本 -> StaleVersion
    let mut conflicting = failed.clone();
    conflicting.reason_code = Some("JOURNAL_LOST".to_string());
    assert_eq!(store.merge_evidence(&key, &conflicting), Ok(EvidenceMerge::Conflict(failed.clone())));

    let mut stale = failed.clone();
    stale.record_version = failed.record_version.saturating_sub(1);
    assert_eq!(store.merge_evidence(&key, &stale), Ok(EvidenceMerge::StaleVersion(failed.clone())));

    let mut newer = failed.clone();
    newer.record_version = failed.record_version + 1;
    newer.reason_code = Some("JOURNAL_LOST".to_string());
    assert_eq!(store.merge_evidence(&key, &newer), Ok(EvidenceMerge::Applied(newer)));
    assert_eq!(
        store.get(&key, SUB_TASK).expect("get").unwrap().reason_code.as_deref(),
        Some("JOURNAL_LOST")
    );
}

#[test]
fn merge_rejects_higher_version_from_foreign_journal_epoch() {
    let dir = TempDir::new("merge_epoch");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    let stored = store.get(&key, SUB_TASK).expect("get").expect("record");
    // 跨 epoch 的高版本记录不可直接比较，更不得采用（03 规格）
    let mut foreign = stored.clone();
    foreign.journal_epoch = "00000000-0000-4000-8000-000000000000".to_string();
    foreign.record_version = stored.record_version + 1;
    assert_eq!(store.merge_evidence(&key, &foreign), Ok(EvidenceMerge::Conflict(stored.clone())));
    assert_eq!(store.get(&key, SUB_TASK).expect("get").unwrap(), stored, "原记录不得被修改");
}

#[test]
fn merge_never_reverts_confirmed_terminal_state() {
    let dir = TempDir::new("merge_terminal");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store.finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR")).expect("finish");
    // 更高版本的 Attempted 绝不允许逆转已确认的 Failed/OS_ERROR 终态（04 规格）
    let mut higher_attempted = failed.clone();
    higher_attempted.state = TaskState::Attempted;
    higher_attempted.reason_code = None;
    higher_attempted.record_version = failed.record_version + 1;
    assert_eq!(store.merge_evidence(&key, &higher_attempted), Ok(EvidenceMerge::Conflict(failed.clone())));
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(after.state, TaskState::Failed, "终态不得被回退为中间态");
    assert_eq!(after.reason_code.as_deref(), Some("OS_ERROR"));
}

#[test]
fn merge_rejects_contradictory_terminal_state_with_higher_version() {
    let dir = TempDir::new("merge_contradiction");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store.finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR")).expect("finish");
    // TR-R9-1：更高版本 Succeeded（即便携带完整 boot 证据）不得静默覆盖已确认 Failed/OS_ERROR（04 规格 L119–124）
    let mut succeeded = failed.clone();
    succeeded.state = TaskState::Succeeded;
    succeeded.reason_code = None;
    succeeded.observed_boot_id = Some(BOOT_NEW.to_string());
    succeeded.evidence_kind = Some("boot_transition_observed".to_string());
    succeeded.record_version = failed.record_version + 1;
    assert_eq!(store.merge_evidence(&key, &succeeded), Ok(EvidenceMerge::Conflict(failed.clone())));
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(after.state, TaskState::Failed, "矛盾终态不得覆盖已确认失败记录");
    assert_eq!(after.reason_code.as_deref(), Some("OS_ERROR"));
    assert_eq!(after.record_version, failed.record_version, "零写盘，原记录保持不变");
}

#[test]
fn merge_rejects_terminal_state_transition_to_unknown_with_higher_version() {
    let dir = TempDir::new("merge_terminal_to_unknown");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store.finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR")).expect("finish");
    // TR-R10-1：更高版本 Unknown（RESULT_TIMEOUT）不得中继抹除已确认 Failed/OS_ERROR（04 规格 L69–70，
    // 封死 Failed(v3)→Unknown(v4)→Succeeded(v5) 两步绕过：Unknown 不得使 stored 脱离终态全封闭守卫）
    let mut unknown = failed.clone();
    unknown.state = TaskState::Unknown;
    unknown.reason_code = Some("RESULT_TIMEOUT".to_string());
    unknown.record_version = failed.record_version + 1;
    assert_eq!(store.merge_evidence(&key, &unknown), Ok(EvidenceMerge::Conflict(failed.clone())));
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(after.state, TaskState::Failed, "已确认终态不得退化为 Unknown");
    assert_eq!(after.reason_code.as_deref(), Some("OS_ERROR"));
    assert_eq!(after.record_version, failed.record_version, "零写盘，原记录保持不变");
}

#[test]
fn merge_rejects_succeeded_without_boot_evidence() {
    let dir = TempDir::new("merge_no_evidence");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    let attempted = store.mark_attempted(&key, SUB_TASK).expect("attempted");
    // TR-R9-1：incoming Succeeded 缺少 boot_transition_observed 证据绝不允许应用
    let mut no_evidence = attempted.clone();
    no_evidence.state = TaskState::Succeeded;
    no_evidence.record_version = attempted.record_version + 1;
    assert_eq!(store.merge_evidence(&key, &no_evidence), Ok(EvidenceMerge::Conflict(attempted.clone())));
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(after.state, TaskState::Attempted, "缺证据的 Succeeded 不得被应用");
    assert_eq!(after.record_version, attempted.record_version, "零写盘，原记录保持不变");
}

#[test]
fn journal_errors_are_not_silently_swallowed() {
    // 目录不存在时不得假装成功
    let dir = TempDir::new("missing");
    let missing = dir.path().join("nope");
    assert!(matches!(JournalStore::open(&missing, RealFs), Err(_)));
    // JournalError 必须区分「确未提交」与「提交结果不明」
    assert_ne!(JournalError::DefinitelyNotCommitted, JournalError::CommitUnknown);
}
```
   > 该测试文件需要 `use rsetup_board_agent::journal::{...}`，因此 Step 3.1 的 `lib.rs` 必须同时补上 `pub mod journal;` 并导出 `JOURNAL_MAX_RECORDS`。若 Step 3.1 先只跑前两个用例（见下），其余用例的编译也需要全部类型存在——因此**桩阶段要把全部类型写全**。

   e. 运行 RED：
```bash
cargo test --offline --locked -p rsetup-board-agent --test wire_04_journal journal_epoch_is_stable_across_normal_reopen -- --exact
```
预期：编译通过、执行 1 个用例，因桩 `open` 返回 `Err(NotFound)` 使 `.expect("open journal")` 失败，退出码非零。

   f. 最小实现（3.1 只实现 `open`/`get`/`insert_accepted`/`journal_epoch`/`record_count` 与 `RealFs`，`recover` 先委托 `open` 并返回原状态，`mark_attempted`/`finish`/`rebuild_after_loss`/`merge_evidence` 仍为桩）：
```rust
// RealFs 关键实现
impl Fs for RealFs {
    fn create_dir_all(&self, dir: &Path) -> std::io::Result<()> { std::fs::create_dir_all(dir) }

    fn create_write(&self, path: &Path) -> std::io::Result<Box<dyn std::io::Write + Send>> {
        Ok(Box::new(std::fs::File::create(path)?))
    }

    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        match std::fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::File::open(path)?.sync_all()
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> { std::fs::rename(from, to) }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> { std::fs::remove_file(path) }

    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> { std::fs::File::open(dir)?.sync_all() }
}
```
```rust
// 持久文件格式（采用已锁定的 serde/serde_json 结构化编解码，绝不手写伪解析）
#[derive(serde::Serialize, serde::Deserialize)]
struct JournalMeta {
    v: u32,
    epoch: String,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct JournalLogs {
    v: u32,
    epoch: String,
    records: Vec<StoredRecord>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StoredRecord {
    controller_hex: String,
    sub_task_id: String,
    payload_hash_hex: String,
    record: StoredTaskRecord,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct StoredTaskRecord {
    schema_version: u32,
    sub_task_id: String,
    journal_epoch: String,
    record_version: u64,
    state: String,
    pre_boot_id: String,
    observed_boot_id: Option<String>,
    reason_code: Option<String>,
    updated_unix_ms: Option<i64>,
    evidence_kind: Option<String>,
}

/// 加载恢复时的严格持久格式与语义校验（TR-N3 / 正审 R-03 / 正审 R2 TR-R6-2）：未知 schema 版本、epoch 格式与一致性、
/// 记录内层一致性（外层与内层 sub_task_id 必须相等）、合法状态、规范 64 小写 hex controller 与 payload hash、
/// UUIDv4 sub_task_id 与 pre_boot_id/observed_boot_id、reason_code 十二词表、boot 证据完整性、去重键唯一性等。
fn validate_loaded_journal(meta: &JournalMeta, logs: &JournalLogs) -> Result<(), JournalError> {
    if meta.v != JOURNAL_SCHEMA_VERSION || logs.v != JOURNAL_SCHEMA_VERSION {
        return Err(JournalError::Decode("unsupported journal schema version".to_string()));
    }
    if logs.epoch != meta.epoch {
        return Err(JournalError::Decode("epoch mismatch in logs".to_string()));
    }
    if !rsetup_protocol::wire::is_canonical_uuid_v4(&meta.epoch) {
        return Err(JournalError::Decode("invalid journal epoch format".to_string()));
    }
    let mut seen_keys = std::collections::HashSet::new();
    for stored in &logs.records {
        if stored.record.schema_version != rsetup_protocol::wire::BUSINESS_SCHEMA_VERSION {
            return Err(JournalError::Decode("unsupported record schema version".to_string()));
        }
        if stored.record.journal_epoch != meta.epoch {
            return Err(JournalError::Decode("record epoch mismatch".to_string()));
        }
        if stored.record.record_version == 0 {
            return Err(JournalError::Decode("invalid record version".to_string()));
        }
        if TaskState::from_str_exact(&stored.record.state).is_none() {
            return Err(JournalError::Decode("journal.state_format".to_string()));
        }
        if !rsetup_protocol::wire::is_canonical_uuid_v4(&stored.sub_task_id) {
            return Err(JournalError::Decode("invalid sub_task_id format".to_string()));
        }
        // TR-R6-2：外层去重键与内层记录必须指向同一 sub_task_id，损坏/伪造记录不得通过 open 后在 get 返回。
        if stored.sub_task_id != stored.record.sub_task_id {
            return Err(JournalError::Decode("sub_task_id mismatch".to_string()));
        }
        if decode_journal_hash32(&stored.controller_hex).is_err() {
            return Err(JournalError::Decode("journal.hash_format".to_string()));
        }
        // TR-R6-2：payload_hash_hex 与 controller_hex 同标准——恰 64 小写 ASCII hex（复用 decode_journal_hash32）。
        if decode_journal_hash32(&stored.payload_hash_hex).is_err() {
            return Err(JournalError::Decode("journal.hash_format".to_string()));
        }
        // TR-R6-2：pre_boot_id 与已提供的 observed_boot_id 必须是 canonical UUIDv4。
        if !rsetup_protocol::wire::is_canonical_uuid_v4(&stored.record.pre_boot_id) {
            return Err(JournalError::Decode("invalid pre_boot_id format".to_string()));
        }
        if let Some(observed) = stored.record.observed_boot_id.as_deref() {
            if !rsetup_protocol::wire::is_canonical_uuid_v4(observed) {
                return Err(JournalError::Decode("invalid observed_boot_id format".to_string()));
            }
        }
        // TR-R6-2：reason_code（若存在）必须严格属于十二词表（空串不在词表内，同样拒绝）。
        if let Some(code) = stored.record.reason_code.as_deref() {
            if !rsetup_protocol::wire::TASK_REASON_CODES.contains(&code) {
                return Err(JournalError::Decode("invalid reason code".to_string()));
            }
        }
        // TR-R6-2：自称 boot_transition_observed 必须携带与 pre_boot_id 不同的 observed_boot_id 证据。
        match stored.record.evidence_kind.as_deref() {
            None => {}
            Some("boot_transition_observed") => {
                let observed = stored.record.observed_boot_id.as_deref();
                if observed.is_none() || observed == Some(stored.record.pre_boot_id.as_str()) {
                    return Err(JournalError::Decode("boot evidence incomplete".to_string()));
                }
            }
            _ => return Err(JournalError::Decode("invalid evidence kind".to_string())),
        }
        // TR-R8-1：Succeeded 必须持有完整 boot transition 证据（spec 03 §165-170；本计划 L63/L1649）。
        // 上方 match 的 None 臂只拦「自称证据不完整」，拦不住反方向：state = Succeeded 且
        // 完全无证据的记录放行。联合守卫强制三重条件：evidence_kind 为
        // boot_transition_observed、observed_boot_id 存在、且与 pre_boot_id 不同。
        // （持久层 state 为字符串表示，与 L2102 `TaskState::from_str_exact` 同一词表。）
        if stored.record.state == "Succeeded"
            && (stored.record.evidence_kind.as_deref() != Some("boot_transition_observed")
                || stored.record.observed_boot_id.is_none()
                || stored.record.observed_boot_id.as_deref()
                    == Some(stored.record.pre_boot_id.as_str()))
        {
            return Err(JournalError::Decode(
                "succeeded record must have valid boot transition evidence".to_string(),
            ));
        }
        if !seen_keys.insert((&stored.controller_hex, &stored.sub_task_id)) {
            return Err(JournalError::Decode("journal.duplicate_record".to_string()));
        }
    }
    Ok(())
}

fn open(dir: &Path, fs: F) -> Result<Self, JournalError> {
    // open 绝不创建目录：目录缺失必须 Err（write_durable 的首个 create_write 以 ENOENT 失败）。
    // 如确需「首次创建目录」能力，须另立显式配置开关 create_missing_dir（默认 false，本切片不实现）。
    //
    // J-01 修订：先读齐 META_FILE 与 LOG_FILE 两个文件，再按 (meta, logs) 组合分支。
    // 旧实现「meta 存在而 LOG_FILE 缺失 → 保留旧 epoch 直接 Vec::new()」会让已持久化的
    // accepted/attempted 记录在同 epoch 下静默失去去重能力，违反 03 spec §175-177 与
    // 联合 design §36，必须 fail-closed 为固定去敏错误，绝不静默清空。
    let meta_path = dir.join(Self::META_FILE);
    let logs_path = dir.join(Self::LOG_FILE);
    let meta_bytes = fs.read(&meta_path).map_err(|e| JournalError::Io(e.to_string()))?;
    let logs_bytes = fs.read(&logs_path).map_err(|e| JournalError::Io(e.to_string()))?;
    match (meta_bytes, logs_bytes) {
        // (None, None) 首次初始化：生成新 UUIDv4 epoch，同一函数体内先落空 LOG_FILE、
        // 再落 META_FILE，两条 write_durable 都成功才返回 Ok；任何一条失败（含 Step 3.2
        // 后的 CommitUnknown）直接以 `?` 返回 Err，不构造、不返回 store。
        // 两次写不是跨文件原子提交：若中途被打断（只剩 LOG_FILE 无 META_FILE），
        // 下次 open 落入下方 exactly-one-Some 分支继续 fail-closed，绝不冒称原子。
        (None, None) => {
            let epoch = uuid::Uuid::new_v4().to_string();
            let empty_logs = JournalLogs {
                v: JOURNAL_SCHEMA_VERSION,
                epoch: epoch.clone(),
                records: Vec::new(),
            };
            let logs_body = serde_json::to_vec(&empty_logs)
                .map_err(|e| JournalError::Decode(e.to_string()))?;
            // 与日志提交同一套两阶段落盘：首次初始化 open 产生 LOG_FILE 与 META_FILE 各自的
            // create_write/write/sync_file/rename/sync_dir 等操作（不再含 CreateDir），全部计入
            // 调用计数基准；注入偏移仍以 open 成功后的激活时刻快照为基准，不受 open 操作数影响。
            write_durable(&fs, dir, &logs_path, &logs_body)?;
            let meta = JournalMeta { v: JOURNAL_SCHEMA_VERSION, epoch: epoch.clone() };
            let meta_body = serde_json::to_vec(&meta)
                .map_err(|e| JournalError::Decode(e.to_string()))?;
            write_durable(&fs, dir, &meta_path, &meta_body)?;
            Ok(Self { dir: dir.to_path_buf(), epoch, fs, records: Mutex::new(Vec::new()), uncertain: std::sync::atomic::AtomicBool::new(false) })
        }
        // (Some, Some) 普通恢复：decode 两个文件，保留 meta.epoch 并执行完整语义校验（TR-N3 / 正审 R-03）。
        (Some(meta_bytes), Some(logs_bytes)) => {
            let meta: JournalMeta =
                serde_json::from_slice(&meta_bytes).map_err(|e| JournalError::Decode(e.to_string()))?;
            let logs: JournalLogs =
                serde_json::from_slice(&logs_bytes).map_err(|e| JournalError::Decode(e.to_string()))?;
            validate_loaded_journal(&meta, &logs)?;
            Ok(Self { dir: dir.to_path_buf(), epoch: meta.epoch, fs, records: Mutex::new(logs.records), uncertain: std::sync::atomic::AtomicBool::new(false) })
        }
        // exactly one Some：文件集不完整（部分丢失或初始化被打断）。固定去敏 Decode 错误，
        // 绝不返回同 epoch 空 store、绝不自动 rebuild、不改旧 epoch、不删除任何现存文件；
        // 恢复只能由调用方显式走既有受控路径 `rebuild_after_loss`（说明见本代码块之后），
        // 不新增 dispatcher/helper，不改 JournalError 枚举与 wire 契约。
        _ => Err(JournalError::Decode("journal.incomplete_files".to_owned())),
    }
}

/// Step 3.1 初始阶段：两阶段落盘骨架就位，临时文件路径严密拒绝非法路径且不 fallback；
/// 此时各类 IO 错误统一返回 `JournalError::Io`，等待 Step 3.2 失败注入测试建立真实行为 RED 后，
/// 再替换为精细化状态分类。
fn write_durable(fs: &F, dir: &Path, final_path: &Path, body: &[u8]) -> Result<(), JournalError> {
    let file_name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| JournalError::Io("invalid target path for durable write".to_string()))?;
    let tmp = dir.join(format!(".{}.tmp-{}", file_name, uuid::Uuid::new_v4()));
    let mut file = fs.create_write(&tmp).map_err(|e| JournalError::Io(e.to_string()))?;
    file.write_all(body).map_err(|e| JournalError::Io(e.to_string()))?;
    drop(file);
    fs.sync_file(&tmp).map_err(|e| JournalError::Io(e.to_string()))?;
    fs.rename(&tmp, final_path).map_err(|e| JournalError::Io(e.to_string()))?;
    fs.sync_dir(dir).map_err(|e| JournalError::Io(e.to_string()))?;
    let _ = fs.remove_file(&tmp);
    let _ = fs.sync_dir(dir);
    Ok(())
}
```
   运行 PASS：
```bash
cargo test --offline --locked -p rsetup-board-agent --test wire_04_journal journal_epoch_is_stable_across_normal_reopen -- --exact
```
   预期：PASS（执行 1 个用例）。

**J-01 修订说明（`open` 组合矩阵与受控恢复路径；仅静态修订，未运行任何测试）：**

- 组合矩阵（以分支前两次 `fs.read` 为准，正/负对照均来自真实 `open`；不新增 dispatcher/helper，`JournalError` 枚举与 wire 契约不变）：

  | (META_FILE, LOG_FILE) | 行为 |
  | --- | --- |
  | (None, None) | 首次初始化：新 UUIDv4 epoch；先 `write_durable` 空 `JournalLogs`，再 `write_durable` `JournalMeta`，同函数体内两条都成功才 `Ok`，任何一条失败（含 `CommitUnknown`）返回 `Err` 且不返回 store |
  | (Some, Some) | 普通恢复：decode 两个文件，保留 `meta.epoch` 并执行完整持久格式与语义校验（TR-N3 / 正审 R-03：版本、规范 UUIDv4 epoch 与 logs.epoch 一致性、记录内层 epoch 一致、`BUSINESS_SCHEMA_VERSION`、`record_version > 0`、合法 state、规范 64 hex controller 与 UUIDv4 sub_task_id、evidence_kind 与 `(controller_hex, sub_task_id)` 去重唯一性，任一失败固定去敏 `Decode`；不改文件、不自动重建） |
  | 恰一个 Some | 固定去敏 `Err(JournalError::Decode("journal.incomplete_files"))`：绝不返回同 epoch 空 store、绝不自动 rebuild、不改旧 epoch、不删除现存日志 |

- interrupted initialization 同样 fail-closed：两次写不是跨文件原子提交，中途被打断只剩 LOG_FILE 时，下次 `open` 落入 exactly-one-Some 分支继续 `Err`，绝不冒称首次初始化或普通恢复；目录缺失仍 `Err`（`write_durable` 首个 `create_write` 以 ENOENT 失败），不 mkdir。
- 受控恢复路径：调用方拿到 `journal.incomplete_files` 后，只能显式调用既有 `rebuild_after_loss`，由其更换 epoch 并返回 `JournalRecovery::RebuiltAfterLoss { journal_lost: true }`（对外语义 JOURNAL_LOST）；没有原件/授权不重建、不 fake unknown record、不重复 OS 调用；`open` 自身永不触发重建。
- fs 故障注入基准不受影响：open 的全部操作（含首次初始化期间 LOG_FILE 与 META_FILE 的两阶段落盘）计入调用计数，注入偏移仍以 open 成功后的激活时刻快照为基准，无关测试的 offset 不变。
- `META_FILE`/`LOG_FILE` 为 `pub const`（见上文 Interfaces 声明），集成测试可直接引用 `JournalStore::<RealFs>::META_FILE` / `JournalStore::<RealFs>::LOG_FILE`（与故障注入测试中 `JournalStore::<RealFs>::LOG_FILE` 同一用法），无需新增 API。

在 `tests/wire_04_journal.rs` 增加 1 个完整 synthetic 测试（复用既有 `mod support`/`TempDir`/`open_store`/`BOOT_OLD`/`SUB_TASK` 导入与 helper，不新增 helper 占位；无真实断电；`std::fs::remove_file` 只出现在下方 Markdown 测试代码中，计划作者不执行任何删除或测试）：

```rust
#[test]
fn open_fails_closed_when_only_log_file_is_missing() {
    let dir = TempDir::new("partial-loss");
    let meta_path = dir.path().join(JournalStore::<RealFs>::META_FILE);
    let logs_path = dir.path().join(JournalStore::<RealFs>::LOG_FILE);

    // 正对照：正常初次 open 创建两个文件，并落 1 条 Accepted 记录
    let store = open_store(&dir);
    let epoch = store.journal_epoch().to_string();
    store
        .insert_accepted(&[3u8; 32], SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    drop(store);
    assert!(meta_path.exists(), "first open must create META_FILE");
    assert!(logs_path.exists(), "first open must create LOG_FILE");

    // 正对照：正常重开 epoch 保持，两文件仍在
    let reopened = open_store(&dir);
    assert_eq!(reopened.journal_epoch(), epoch, "normal reopen keeps the epoch");
    drop(reopened);
    assert!(meta_path.exists() && logs_path.exists(), "reopen must not remove files");

    // 部分丢失：仅移除 LOG_FILE，保留 META_FILE
    std::fs::remove_file(&logs_path).expect("remove log file");
    assert!(meta_path.exists(), "META_FILE must be preserved");

    // 负对照：open 必须 fail-closed 为固定去敏 Decode 错误——不同 epoch 空 store、
    // 不自动 rebuild、不改 epoch、不删除 META_FILE，也不触达 OS 边界（OS 仅在 Task 4 reboot 路径）
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("open must fail closed on incomplete journal files");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "journal.incomplete_files"));
    assert!(meta_path.exists(), "failed open must not delete META_FILE");
}

/// TR-N3 / 正审 R-03 负例测试：open 加载恢复时持久文件损坏或语义非法必须 fail-closed 为固定去敏 Decode 错误
#[test]
fn open_fails_closed_on_corrupted_or_malformed_journal_entries() {
    let dir = TempDir::new("malformed-journal");
    let meta_path = dir.path().join(JournalStore::<RealFs>::META_FILE);
    let logs_path = dir.path().join(JournalStore::<RealFs>::LOG_FILE);

    // 1. 未知 schema 版本返回 Decode 拒绝
    std::fs::write(&meta_path, r#"{"v":99,"epoch":"6f8b1c2d-0000-4000-8000-000000000001"}"#).expect("write meta");
    std::fs::write(&logs_path, r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[]}"#).expect("write logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on bad schema version");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "unsupported journal schema version"));

    // 2. 记录内层 epoch 与外层 meta.epoch 不一致返回 Decode 拒绝
    std::fs::write(&meta_path, r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001"}"#).expect("write meta");
    let bad_epoch_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000002","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_epoch_record).expect("write bad epoch logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on record epoch mismatch");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "record epoch mismatch"));

    // 3. 记录含未知 state 返回 Decode 拒绝
    let bad_state_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"InvalidState","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_state_record).expect("write bad state logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on bad state");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "journal.state_format"));

    // 4. 重复 (controller, sub_task_id) 键返回 Decode 拒绝
    let dup_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}},{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":2,"state":"Attempted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, dup_record).expect("write dup logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on duplicate key");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "journal.duplicate_record"));

    // 5. 内层 record.sub_task_id 与外层 sub_task_id 不一致 → Decode 拒绝（TR-R6-2）
    let inner_mismatch_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"11111111-2222-4333-8444-555555555555","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, inner_mismatch_record).expect("write inner mismatch logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on inner sub_task_id mismatch");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "sub_task_id mismatch"));

    // 6. payload_hash_hex 为 uppercase（非 64 小写 hex）→ Decode 拒绝（TR-R6-2）
    let bad_hash_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_hash_record).expect("write bad hash logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on uppercase payload hash");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "journal.hash_format"));

    // 7. pre_boot_id 非 canonical UUIDv4 → Decode 拒绝（TR-R6-2）
    let bad_boot_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"not-a-uuid","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_boot_record).expect("write bad boot logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on non-UUID pre_boot_id");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "invalid pre_boot_id format"));

    // 8. evidence_kind 自称 boot_transition_observed 但缺 observed_boot_id → Decode 拒绝（TR-R6-2）
    let bad_evidence_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Attempted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":"boot_transition_observed"}}]}"#;
    std::fs::write(&logs_path, bad_evidence_record).expect("write bad evidence logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on missing observed boot evidence");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "boot evidence incomplete"));

    // 9. TR-R8-1：state = Succeeded 且完全无 boot transition 证据 → 联合守卫 Decode 拒绝
    let succeeded_no_evidence = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Succeeded","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, succeeded_no_evidence).expect("write succeeded-without-evidence logs");
    let err = JournalStore::open(dir.path(), RealFs).err().expect("must fail on Succeeded without boot evidence");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "succeeded record must have valid boot transition evidence"));
}
```

未来选择器（真实函数名，供执行会话交接，计划作者不执行）：
`cargo test --offline --locked -p rsetup-board-agent --test wire_04_journal open_fails_closed_when_only_log_file_is_missing -- --exact`
`cargo test --offline --locked -p rsetup-board-agent --test wire_04_journal open_fails_closed_on_corrupted_or_malformed_journal_entries -- --exact`

   g. `payload_hash_hex` 固定 hex 字段纯检查：2 个私有纯函数 + 1 个纯 lib 测试（这是持久 JSON 中固定 hex 字段的纯检查：JSON 仍 serde，不手写 JSON/不造 wire codec；不新增 hex/ring 直接依赖，std 两个 nibble 编码/解码足够——board allowed 直接依赖没有 `hex`；动手前先 grep 确认同义 helper 未已定义，不复制两份）。前置声明：本子步不补齐 `payload_hash` 只读 getter（签名见 Task 4 `payload_matches` 一节）与 `payload_matches`/`execute` 接线，不在此标 getter 或完整 journal 完成。
      (1) 先在 `journal.rs` 末尾写完整测试模块（独立 `#[cfg(test)]`，完整 module/imports；全部 synthetic 数据，无 Fs/open/records/磁盘/假凭据）：
```rust
#[cfg(test)]
mod hash_hex_tests {
    use super::{decode_journal_hash32, encode_journal_hash32, JournalError};

    /// 唯一纯 lib 测试：64 小写 ASCII hex 规范往返 + 8 类坏格式固定 `Decode` 去敏（消息不含实际 input）。
    #[test]
    fn journal_hash32_hex_helpers_are_canonical_and_roundtrip() {
        let zero = [0u8; 32];
        let ff = [0xffu8; 32];
        let custom = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
        ];
        assert_eq!(encode_journal_hash32(&zero), "00".repeat(32));
        assert_eq!(encode_journal_hash32(&ff), "ff".repeat(32));
        assert_eq!(encode_journal_hash32(&custom), "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        assert_eq!(decode_journal_hash32(&encode_journal_hash32(&zero)).unwrap(), zero);
        assert_eq!(decode_journal_hash32(&encode_journal_hash32(&ff)).unwrap(), ff);
        assert_eq!(decode_journal_hash32("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f").unwrap(), custom);
        let expected: Result<[u8; 32], JournalError> = Err(JournalError::Decode("journal.hash_format".to_owned()));
        let bad: Vec<String> = vec![
            String::new(),         // 1 空
            "0".repeat(63),        // 2 长度 63
            "0".repeat(65),        // 3 长度 65
            "FF".repeat(32),       // 4 uppercase
            "zz".repeat(32),       // 5 non-hex
            "0".repeat(63) + " ",  // 6 空格（仍 64 字符）
            "0x".repeat(32),       // 7 `0x` prefix
            "\u{e9}".repeat(32),   // 8 Unicode（UTF-8 每字符 2 字节，字节长度恰 64）
        ];
        for (idx, value) in bad.iter().enumerate() {
            assert_eq!(decode_journal_hash32(value), expected, "bad case {idx}");
        }
    }
}
```

      (2) RED：先在 `journal.rs` 放两个可编译桩（不用 (3) 的真实实现），首条 zero 编码 assert_eq 行为 RED：
```rust
fn encode_journal_hash32(_value: &[u8; 32]) -> String {
    String::new()
}

fn decode_journal_hash32(_value: &str) -> Result<[u8; 32], JournalError> {
    Ok([0u8; 32])
}
```
      运行（未来 3.1 命令，本轮不执行；`--offline --locked` 与 `-p` 私有 target 统一本任务其他命令）：
```bash
cargo test --offline --locked -p rsetup-board-agent --lib journal::hash_hex_tests::journal_hash32_hex_helpers_are_canonical_and_roundtrip -- --exact
```
      预期：编译通过、执行 1 个用例，因首条零字节编码断言（`"" ≠ "00"×32`）失败，退出码非零——RED。
      (3) GREEN：以真实实现替换桩并重跑同命令，预期 PASS（GREEN）：
```rust
/// 32B → 64 小写 ASCII hex：持久 JSON `StoredRecord.payload_hash_hex` 字段的规范形态（非 wire codec）。
fn encode_journal_hash32(value: &[u8; 32]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for byte in value.iter().copied() {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// `encode_journal_hash32` 的精确逆：只收恰 64 个 lower ASCII hex（无 prefix/空格/uppercase/非 ASCII）；
/// 坏格式统一去敏 `Decode("journal.hash_format")`，消息不含实际 input。
fn decode_journal_hash32(value: &str) -> Result<[u8; 32], JournalError> {
    let bad = || JournalError::Decode("journal.hash_format".to_owned());
    let bytes = value.as_bytes();
    // 先长度（恰 64），再逐字节 ASCII 白名单，再 nibble 值：不 Unicode 切片 panic，不错数据变 0 hash
    if bytes.len() != 64 {
        return Err(bad());
    }
    let nibble = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None, // 拒 uppercase/空格/`0x` prefix/Unicode 等
        }
    };
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.chunks_exact(2)) {
        let hi = nibble(pair[0]).ok_or_else(bad)?;
        let lo = nibble(pair[1]).ok_or_else(bad)?;
        *slot = (hi << 4) | lo;
    }
    Ok(out)
}
```
      两个纯函数与测试模块在本子步写入 `journal.rs`，随下方提交步骤与 3.1 的 `journal.rs` 一并提交（不新增提交步）。
   提交：
```bash
cargo fmt -p rsetup-board-agent
git add crates/rsetup-board-agent/Cargo.toml crates/rsetup-board-agent/src/lib.rs crates/rsetup-board-agent/src/journal.rs crates/rsetup-board-agent/tests/support crates/rsetup-board-agent/tests/wire_04_journal.rs
git commit -m "feat(board-agent): 新增任务 journal，含持久稳定 epoch 与已接受 pre_boot_id"
```

- [ ] **Step 3.2: 两阶段 fsync 顺序见证与失败注入（先真实行为 RED 再实现分类）**

   a. 创建 `src/test_seam.rs`（同一文件同时服务 `#[cfg(test)]` 与 feature 导出）：
```rust
//! 仅测试/见证用途的文件操作替身。生产构建中通过 `test-seam` feature 显式开启。

use crate::journal::Fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FsOp {
    CreateDir(PathBuf),
    CreateWrite(PathBuf),
    Write(PathBuf),
    SyncFile(PathBuf),
    Rename(PathBuf, PathBuf),
    RemoveFile(PathBuf),
    SyncDir(PathBuf),
}

/// 仪表共享内部状态：写入顺序记录 + 故障注入计数/标记，由 `RecordingJournal` 与其派发的 writer 经 `Arc` 共享。
#[derive(Default)]
struct RecordingState {
    ops: Mutex<Vec<FsOp>>,
    failures: Mutex<Vec<(usize, std::io::ErrorKind)>>, // (第几次调用产生的失败, ErrorKind)
    calls: Mutex<usize>,
}

/// 记录调用序，并注入一次性失败。`RecordingJournal::new()` 无失败注入。
pub struct RecordingJournal {
    fs: crate::journal::RealFs,
    state: Arc<RecordingState>,
}

impl RecordingJournal {
    pub fn new() -> Self;
    /// 让「从当前计数起的第 offset 次」文件操作返回 `kind` 类型的 IO 错误。
    /// offset 0..=6 对应 `write_durable` 的单次落盘调用序。
    pub fn fail_at_offset(self, offset: usize, kind: std::io::ErrorKind) -> Self;
    pub fn ops(&self) -> Vec<FsOp>;
}

/// 落地于 `crates/rsetup-board-agent/src/test_seam.rs`，紧邻 `impl RecordingJournal`；后文 GREEN 片段不得重复实现本 `Default`。
impl Default for RecordingJournal {
    fn default() -> Self {
        Self::new()
    }
}

/// 生产签名别名，便于测试代码语义化书写。
pub type FaultyFs = RecordingJournal;

impl Fs for RecordingJournal {
    fn create_dir_all(&self, dir: &Path) -> std::io::Result<()> {
        self.record(FsOp::CreateDir(dir.to_path_buf()))?;
        self.fs.create_dir_all(dir)
    }
    fn create_write(&self, path: &Path) -> std::io::Result<Box<dyn Write + Send>> {
        self.record(FsOp::CreateWrite(path.to_path_buf()))?;
        let inner = self.fs.create_write(path)?;
        Ok(Box::new(RecordingWriter {
            inner,
            state: Arc::clone(&self.state),
            path: path.to_path_buf(),
        }))
    }
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> { self.fs.read(path) }
    fn sync_file(&self, path: &Path) -> std::io::Result<()> {
        self.record(FsOp::SyncFile(path.to_path_buf()))?;
        self.fs.sync_file(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.record(FsOp::Rename(from.to_path_buf(), to.to_path_buf()))?;
        self.fs.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        self.record(FsOp::RemoveFile(path.to_path_buf()))?;
        self.fs.remove_file(path)
    }
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        self.record(FsOp::SyncDir(dir.to_path_buf()))?;
        self.fs.sync_dir(dir)
    }
}

impl RecordingJournal {
    fn record(&self, op: FsOp) -> std::io::Result<()> {
        record_raw(&self.state, op)
    }
}

/// journal 与 writer 共用的记账与故障注入：先追加顺序记录，再消耗一次调用计数，命中注入索引则返回错误。
fn record_raw(state: &RecordingState, op: FsOp) -> std::io::Result<()> {
    state.ops.lock().unwrap().push(op);
    let mut calls = state.calls.lock().unwrap();
    let index = *calls;
    *calls += 1;
    if let Some((_, kind)) = state
        .failures
        .lock()
        .unwrap()
        .iter()
        .find(|(i, _)| *i == index)
        .cloned()
    {
        return Err(std::io::Error::new(kind, format!("injected failure at op {index}")));
    }
    Ok(())
}
```
   > `Write` 记录需要拿到写入动作。若 `record` 只在 `create_write` 时记账，无法见证「写入发生在 sync_file 之前」；因此 `create_write` 返回一个包装 writer。仪表状态收敛到共享 `Arc<RecordingState>`，writer 持有 `Arc`（owned，满足 `Box<dyn Write + Send>` 的默认 `'static` 约束）：
```rust
struct RecordingWriter {
    inner: Box<dyn Write + Send>,
    state: Arc<RecordingState>,
    path: PathBuf,
}
impl Write for RecordingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // 先记账并可注入失败，再真正写盘
        record_raw(&self.state, FsOp::Write(self.path.clone()))?;
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> { self.inner.flush() }
}
```
   把 `record` 抽成自由函数 `fn record_raw(state: &RecordingState, op: FsOp) -> std::io::Result<()>`，`RecordingJournal::record` 与 `RecordingWriter::write` 共用。`create_write` 返回 `Box::new(RecordingWriter { .. })`（owned writer 包裹 Arc 状态，与 `Box<dyn Write + Send>` 签名兼容）。

   失败注入点以「相对激活时刻调用计数的偏移」表达。计数口径：`record_raw` 记录的每一个操作（含 open 期间的 META_FILE 两阶段落盘；T5 后 open 不再 create_dir_all，不再含 CreateDir）都递增 `calls`；「记录提交」计数基准 = 激活时刻的 `calls` 快照。因此针对目标记录提交的故障必须在 open 成功后经 `fs_handle()` 调用 `arm_at_offset` 激活，offset 0 才首个命中 insert 提交的 create_write，而不是 open 期间的 META_FILE 落盘：
```rust
impl RecordingJournal {
    /// 在共享状态上激活故障：让「从激活时刻调用计数起的第 offset 次」文件操作失败。
    /// 必须在 open 成功后调用，offset 才落在目标记录提交窗口。
    /// offset 的含义与 `write_durable` 的固定调用序一一对应：
    /// 0=create_write, 1=write, 2=sync_file, 3=rename, 4=sync_dir(提交点), 5=remove_file, 6=sync_dir(收尾)
    pub fn arm_at_offset(&self, offset: usize, kind: std::io::ErrorKind) {
        let calls = *self.state.calls.lock().unwrap();
        self.state.failures.lock().unwrap().push((calls + offset, kind));
    }
    /// 链式便捷：在激活时刻（open 之前）取基准，仅供针对 open 自身落盘阶段的用例使用。
    pub fn fail_at_offset(self, offset: usize, kind: std::io::ErrorKind) -> Self {
        self.arm_at_offset(offset, kind);
        self
    }
}
```

   b. 在 `src/lib.rs` 与 `Cargo.toml` 暴露见证模块：
```rust
#[cfg(any(test, feature = "test-seam"))]
pub mod test_seam;
```
   测试文件顶部使用：
```rust
#[cfg(feature = "test-seam")]
mod support;
#[cfg(feature = "test-seam")]
use rsetup_board_agent::test_seam::{FaultyFs, FsOp, RecordingJournal};
```
   运行命令必须带 feature：
```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test journal_fault_injection
```
   > 顺序要求：先完成下列测试文件（步骤 c）及紧随其后的 Cargo.toml target 声明，再执行本段任何测试命令（含后续 Step 2 的首个实际运行命令）。`required-features` 不豁免显式 target 解析，target 文件缺失时所有 cargo 命令都会失败。

   在 `crates/rsetup-board-agent/Cargo.toml` 既有 `[features]` 表之后追加本测试目标的显式声明（与步骤 c 的文件创建构成同一准备步骤，二者齐备前不得运行任何测试命令；不得重复 `[features]` 表头）：
```toml
[[test]]
name = "journal_fault_injection"
required-features = ["test-seam"]
```

   c. 创建 `tests/journal_fault_injection.rs`：
```rust
mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::journal::{JournalError, JournalStore, RealFs, TaskState};
use rsetup_board_agent::test_seam::{FsOp, RecordingJournal};
use std::io::ErrorKind;

const BOOT_OLD: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

fn op_index(ops: &[FsOp], wanted: &str) -> usize {
    ops.iter()
        .position(|op| match (op, wanted) {
            (FsOp::CreateWrite(_), "create") => true,
            (FsOp::Write(_), "write") => true,
            (FsOp::SyncFile(_), "sync_file") => true,
            (FsOp::Rename(..), "rename") => true,
            (FsOp::RemoveFile(_), "remove") => true,
            (FsOp::SyncDir(_), "sync_dir") => true,
            _ => false,
        })
        .unwrap_or_else(|| panic!("missing op {wanted} in {ops:?}"))
}

#[test]
fn commit_order_is_write_sync_rename_parent_sync() {
    let dir = TempDir::new("order");
    let key = [9u8; 32];
    let fs = RecordingJournal::new();
    let store = JournalStore::open(dir.path(), fs).expect("open");
    let before = store.record_count();
    assert_eq!(before, 0);
    // 初始化本身已产生一次两阶段落盘；此处只核对「提交记录」产生的调用序
    let ops_after_open = store.journal_epoch().len(); // 仅确保 open 成功
    assert!(ops_after_open > 0);

    let fs_handle = store.fs_handle();
    let mark = fs_handle.ops().len();
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    let ops = fs_handle.ops()[mark..].to_vec();

    let create = op_index(&ops, "create");
    let write = op_index(&ops, "write");
    let sync_file = op_index(&ops, "sync_file");
    let rename = op_index(&ops, "rename");
    let sync_dir = op_index(&ops, "sync_dir");
    assert!(create < write, "temp file must be created before writing: {ops:?}");
    assert!(write < sync_file, "file sync_all must follow the write: {ops:?}");
    assert!(sync_file < rename, "rename must follow the file sync_all: {ops:?}");
    assert!(rename < sync_dir, "parent dir sync_all must follow the rename: {ops:?}");
    // 临时文件清理发生在提交点之后，绝不夹在 rename 之前
    if let Some(remove) = ops.iter().position(|op| matches!(op, FsOp::RemoveFile(_))) {
        assert!(sync_dir < remove, "temp cleanup must not precede the commit sync: {ops:?}");
    }
}
```
   > 为让 `RecordingJournal` 的接受者类型不被 `JournalStore<F>` 消耗后依然可查询，`JournalStore` 增加一个只读访问器（不是新的行为接口，只是让见证对象可观察）：
```rust
impl<F: Fs> JournalStore<F> {
    /// 仅用于测试见证：暴露底层 `Fs` 的共享引用。
    pub fn fs_handle(&self) -> &F { &self.fs }
}
```

```rust
#[test]
fn failure_before_rename_is_definitely_not_committed() {
    let key = [9u8; 32];
    // 偏移 0 (create_write), 1 (write), 2 (sync_file) 均在 rename 调用之前发生 => 确未提交
    for (offset, name) in [(0usize, "create"), (1, "write"), (2, "sync_file")] {
        let dir = TempDir::new(&format!("pre-{name}"));
        let store = JournalStore::open(dir.path(), RecordingJournal::new()).expect("open");
        // open 成功后激活：基准快照在 open 之后，offset 0 首个命中 insert 提交的 create_write
        store.fs_handle().arm_at_offset(offset, ErrorKind::Other);
        let before = store.fs_handle().ops().len();
        let error = store
            .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
            .expect_err("definite pre-rename failure in the commit stage must be reported");
        assert_eq!(error, JournalError::DefinitelyNotCommitted, "offset={offset} ({name})");
        // 确未提交：记录不存在，且没有把日志文件替换成半成品
        let reopened = JournalStore::open(dir.path(), RealFs).expect("reopen");
        assert_eq!(reopened.get(&key, SUB_TASK).expect("get"), None);
        assert_eq!(reopened.record_count(), 0);
        assert!(store.fs_handle().ops().len() > before);
    }
}

#[test]
fn failure_at_rename_or_parent_sync_is_commit_unknown() {
    let key = [9u8; 32];
    // 偏移 3 (rename 调用本身失败) 与 偏移 4 (rename 之后的父目录 sync_dir 失败)
    // 均处于提交边界上或提交之后：底层状态可能已落地或半提交，因此必须分类为 CommitUnknown
    for (offset, name) in [(3usize, "rename"), (4, "sync_dir")] {
        let dir = TempDir::new(&format!("commit-unknown-{name}"));
        let store = JournalStore::open(dir.path(), RecordingJournal::new()).expect("open");
        // open 成功后激活：故障只可能命中目标记录提交的 rename/sync_dir
        store.fs_handle().arm_at_offset(offset, ErrorKind::Other);
        assert_eq!(
            store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD),
            Err(JournalError::CommitUnknown),
            "offset={offset} ({name}) must be classified as commit-unknown"
        );
    }
}

#[test]
fn temp_cleanup_failure_does_not_invalidate_the_commit() {
    let key = [9u8; 32];
    let dir = TempDir::new("cleanup");
    let store = JournalStore::open(dir.path(), RecordingJournal::new()).expect("open");
    // open 成功后激活偏移 5 = 提交点之后的 remove_file 失败（清理失败不得否定提交）
    store.fs_handle().arm_at_offset(5, ErrorKind::PermissionDenied);
    let (record, _) = store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("cleanup failure must not invalidate an already committed record");
    assert_eq!(record.state, TaskState::Accepted);
}

#[test]
fn temporary_files_are_never_named_like_the_committed_log() {
    let dir = TempDir::new("tmpname");
    let key = [9u8; 32];
    let store = JournalStore::open(dir.path(), RecordingJournal::new()).expect("open");
    // 只对「目标记录提交窗口」的 rename 断言目的文件；open 期间的 META_FILE rename 不在此窗口
    let mark = store.fs_handle().ops().len();
    store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD).expect("accepted");
    for op in store.fs_handle().ops()[mark..].to_vec() {
        if let FsOp::Rename(from, to) = op {
            let from_name = from.file_name().unwrap().to_string_lossy().to_string();
            let to_name = to.file_name().unwrap().to_string_lossy().to_string();
            assert_ne!(from_name, to_name);
            assert!(from_name.starts_with('.'), "temp file must be hidden: {from_name}");
            assert!(from_name.contains(".tmp-"), "temp file must be uniquely tagged: {from_name}");
            assert_eq!(to_name, JournalStore::<RealFs>::LOG_FILE);
        }
    }
}
```

   d. **真实行为 RED 验证**：
```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test journal_fault_injection failure_before_rename_is_definitely_not_committed -- --exact
```
预期：**编译通过**，执行 1 个用例并发生**断言失败**（Step 3.1 的 `write_durable` 尚未对 IO 错误分类，返回 `Err(JournalError::Io(...))`，与断言期望的 `JournalError::DefinitelyNotCommitted` 不相等导致断言失败）。**此为真正的行为 RED，严禁在已实现分类后假称 RED，也严禁将类型缺失作为 RED。**

   e. **最小实现（精细化错误分类）**：
更新 `write_durable` 实现，严格落实失败边界：
```rust
/// 固定两阶段落盘与原子重命名：
/// 0=create_write, 1=write, 2=sync_file, 3=rename, 4=sync_dir, 5=remove_file, 6=sync_dir
fn write_durable(fs: &F, dir: &Path, final_path: &Path, body: &[u8]) -> Result<(), JournalError> {
    let file_name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| JournalError::Io("invalid target path for durable write".to_string()))?;
    let tmp = dir.join(format!(".{}.tmp-{}", file_name, uuid::Uuid::new_v4()));
    // (0)(1)(2) rename 之前确定失败 => DefinitelyNotCommitted
    let mut file = fs.create_write(&tmp).map_err(|_| JournalError::DefinitelyNotCommitted)?;
    file.write_all(body).map_err(|_| JournalError::DefinitelyNotCommitted)?;
    drop(file); // 必须先释放文件句柄，再做 sync
    fs.sync_file(&tmp).map_err(|_| JournalError::DefinitelyNotCommitted)?;
    // (3) rename 调用本身失败：文件系统层状态未知（可能部分提交或已修改目录项）=> CommitUnknown
    fs.rename(&tmp, final_path).map_err(|_| JournalError::CommitUnknown)?;
    // (4) rename 之后的父目录 sync 失败：掉电可能导致元数据回滚 => CommitUnknown
    fs.sync_dir(dir).map_err(|_| JournalError::CommitUnknown)?;
    // (5) 清理临时文件属于提交点之后的收尾：删除失败不否定提交
    let _ = fs.remove_file(&tmp);
    // (6) 第二次父目录 sync 尽力而为
    let _ = fs.sync_dir(dir);
    Ok(())
}
```

   f. PASS：
```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam --test journal_fault_injection
```
预期：PASS，全部失败注入用例通过。
   g. 提交：
```bash
cargo fmt -p rsetup-board-agent
git add crates/rsetup-board-agent/Cargo.toml crates/rsetup-board-agent/src/lib.rs crates/rsetup-board-agent/src/journal.rs crates/rsetup-board-agent/src/test_seam.rs crates/rsetup-board-agent/tests/journal_fault_injection.rs
git commit -m "test(board-agent): 见证两阶段 fsync 顺序并注入 rename 边界故障"
```

- [ ] **Step 3.3: 去重、容量与恢复证据**

   a. RED（契约测试中剩余用例）：
```bash
cargo test --offline --locked -p rsetup-board-agent --test wire_04_journal duplicate_sub_task_id_returns_record_and_conflicting_payload_is_rejected -- --exact
```
预期：断言失败（桩 `insert_accepted` 返回 `Err`）。再确认恢复证据用例：
```bash
cargo test --offline --locked -p rsetup-board-agent --test wire_04_journal attempted_record_with_changed_boot_becomes_succeeded_with_evidence -- --exact
```
预期：断言失败（`recover` 桩不做证据应用）。
   b. 最小实现：
   1. `insert_accepted`：`(controller_key, sub_task_id)` 命中且 `payload_hash` 相同 → `(existing, ReusedExisting)`，**不递增 `record_version`**；命中但哈希不同 → `Err(Conflict)`；未命中且 `record_count() == JOURNAL_MAX_RECORDS` → `Err(StorageFull)`；否则写入 `Accepted` 记录（`pre_boot_id` 来自参数，`record_version = 1`，`journal_epoch` 取自身）。
   2. `mark_attempted`：仅 `Accepted → Attempted`，其他状态返回 `Err(Conflict)`。
   3. `finish`：仅 `Attempted → {Failed, Unknown}`；`Succeeded` 必须走 `finish_succeeded_with_boot`。
   4. `finish_succeeded_with_boot`：同时写 `observed_boot_id`、`evidence_kind = Some("boot_transition_observed")`、`state = Succeeded`；缺少 boot 参数则编译期不可能（签名强制）。
   5. `recover`：调用 `open`（沿用 epoch），随后 `apply_boot_transition_evidence(current_boot_id)`：遍历 `state == Attempted && pre_boot_id != current_boot_id` 的记录，逐条原子落盘为 `Succeeded` 并附证据（每次一条提交，避免一次提交失败牵连多条）；**`Accepted` 记录绝不被改写**。返回的 `JournalRecovery` 为 `ReusedEpoch`。
   6. `rebuild_after_loss`：更换 epoch 并整文件重写空记录集，返回 `RebuiltAfterLoss { journal_lost: true }`。
   7. `merge_evidence`：先做合法性守卫（任一不满足 → `Conflict(stored)`，绝不修改原记录，之后才比较版本）：`incoming.journal_epoch == stored.journal_epoch`（03 规格：`record_version` 只在当前 journal_epoch 内递增比较，跨 epoch 的原历史不可直接比较，高版本也不得直接采用）；`incoming.sub_task_id == stored.sub_task_id` 且 `incoming.schema_version == stored.schema_version`（任务身份与 schema 不一致属核实异常）；已存终态全封闭不可篡改守卫（TR-R10-1 / 04 规格 L69–70、L119–124）：`stored.state` 已是终态（`Failed`/`Succeeded`）时，任何后续状态跃迁——无论 `incoming.state` 是 `Accepted`、`Attempted`、`Unknown` 还是矛盾的另一终态（Failed vs Succeeded 或 Succeeded vs Failed），无论 `incoming.record_version` 多高——一律直接返回 `Conflict(stored)`——零写盘、不改版本，原终态记录及其证据绝对不可被覆盖或降级。该守卫一次性封死三类绕过：更高版本中间态回退终态（防止已确认 `Failed/OS_ERROR` 被 `Attempted` 覆盖后经 boot 转换误写 `Succeeded`）；更高版本矛盾终态静默覆盖（04 规格 L119–124：已确认矛盾失败绝不允许被更高版本 `Succeeded` 静默覆盖）；更高版本 `Unknown` 中继抹除已确认失败（04 规格 L69–70：仅允许 `Unknown` 收敛为 `Succeeded`/`Failed`，严禁已确认终态退化为 `Unknown`，封死 `Failed(v3)`→`Unknown(v4)`→`Succeeded(v5)` 两步绕过）。仅当 `stored.state` 处于中间态（`Accepted`/`Attempted`/`Unknown`）时，才允许通过更高版本 incoming 以 `Succeeded`（须持完整 boot 证据，见下）或 `Failed` 收敛为终态。Succeeded 完整证据守卫（TR-R9-1，与 `validate_loaded_journal` 联合校验一致）：`incoming.state == Succeeded` 时必须满足 `incoming.evidence_kind.as_deref() == Some("boot_transition_observed")` 且 `incoming.observed_boot_id.is_some()` 且 `incoming.observed_boot_id != incoming.pre_boot_id`，任一不满足一律返回 `Conflict(stored)`。全部守卫通过后才比较版本：`incoming.record_version < stored.record_version` → `StaleVersion(stored)`；相等且内容不同 → `Conflict(stored)`；相等且内容相同 → `StaleVersion(stored)`；更高 → 原子落盘并返回 `Applied(incoming)`。
   8. 每次落盘成功后 `record_version += 1`（`Applied` 采用 incoming 版本号），且绝不能出现「先改内存再落盘失败」的窗口：**先在内存构造候选记录，落盘成功后才替换内存状态**。**CommitUnknown → uncertain 隔离（TR-N2 / 正审 R-02）**：上述任何提交路径（`insert_accepted`/`mark_attempted`/`finish`/`finish_succeeded_with_boot`/`merge_evidence`/`recover` 证据回写）调用 `write_durable` 得到 `CommitUnknown` 时，必须在向调用方返回该错误之前先 `self.uncertain.store(true, Ordering::SeqCst)`（`DefinitelyNotCommitted` 不置位，磁盘确定未变）；置位后本实例全部读写入口（含 `get`/`payload_hash`）在入口检查 `self.uncertain.load(Ordering::SeqCst)`，为真即返回固定去敏 `JournalError::Io("journal store in uncertain state: CommitUnknown requires reopen or controlled rebuild".to_string())`，绝不返回旧内存记录、绝不发起新写入；恢复仅限丢弃实例重新 `open` 或 `rebuild_after_loss`（见 Interfaces 块 impl 级 uncertain 隔离契约）。
   9. `get`（GREEN：同名替换 Step 1c 恒 `Ok(None)` 桩；签名与 Interfaces 声明一致，同一 impl、紧随 `record_count`）与测试专用毒锁 seam——`get` 为纯内存只读查找、无 I/O、不改 records/epoch/version；毒锁 fail-closed 照抄 `payload_hash`（调用直接返回、不 panic、不吞毒继续读）；0 命中 → `Ok(None)`；>1 命中（同键重复，属损坏）→ 固定去敏 `Decode("journal.duplicate_record")`；恰 1 命中 → `TaskState::from_str_exact` 解析 `state`（未知值 → 去敏 `Decode("journal.state_format")`），其余字段克隆构造 `TaskRecord`（不假设有未定义的转换 helper）：
```rust
impl<F: Fs> JournalStore<F> {
    /// 去重只读查询（GREEN，替换 Step 1c 桩）：纯内存查找、无 I/O、不改 records/epoch/version。
    /// 候选 = `controller_hex == encode_journal_hash32(controller_key)`（lowerhex64）且 `sub_task_id` 精确匹配；
    /// 毒锁 → 固定去敏 `Io`（fail-closed 照抄 `payload_hash`：直接返回、不 panic、不吞毒）；
    /// 0 命中 → `Ok(None)`；>1 命中 → `Decode("journal.duplicate_record")`；
    /// 恰 1 命中 → `TaskState::from_str_exact` 解析 `state`（未知值 → `Decode("journal.state_format")`），其余字段克隆。
    /// uncertain 隔离态（TR-N2）先于一切：入口检查 `self.uncertain`，为真直接返回固定去敏 uncertain `Io`，绝不回旧内存记录。
    pub fn get(&self, controller_key: &[u8; 32], sub_task_id: &str) -> Result<Option<TaskRecord>, JournalError> {
        if self.uncertain.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JournalError::Io("journal store in uncertain state: CommitUnknown requires reopen or controlled rebuild".to_string()));
        }
        let expected_controller_hex = encode_journal_hash32(controller_key);
        let records = match self.records.lock() {
            Ok(guard) => guard,
            Err(_) => return Err(JournalError::Io("journal.records poisoned".to_string())),
        };
        if self.uncertain.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(JournalError::Io("journal store in uncertain state: CommitUnknown requires reopen or controlled rebuild".to_string()));
        }
        let hits: Vec<&StoredRecord> = records
            .iter()
            .filter(|r| r.controller_hex == expected_controller_hex && r.sub_task_id == sub_task_id)
            .collect();
        match hits.len() {
            0 => Ok(None),
            1 => {
                let stored = &hits[0].record;
                let state = TaskState::from_str_exact(&stored.state)
                    .ok_or_else(|| JournalError::Decode("journal.state_format".to_owned()))?;
                Ok(Some(TaskRecord {
                    schema_version: stored.schema_version,
                    sub_task_id: stored.sub_task_id.clone(),
                    journal_epoch: stored.journal_epoch.clone(),
                    record_version: stored.record_version,
                    state,
                    pre_boot_id: stored.pre_boot_id.clone(),
                    observed_boot_id: stored.observed_boot_id.clone(),
                    reason_code: stored.reason_code.clone(),
                    updated_unix_ms: stored.updated_unix_ms,
                    evidence_kind: stored.evidence_kind.clone(),
                }))
            }
            _ => Err(JournalError::Decode("journal.duplicate_record".to_owned())),
        }
    }

    /// 测试专用 seam（`#[cfg]` 门控，不出现在默认生产接口）：对 `records` mutex 注入真实毒锁，
    /// 使 `get`/`payload_hash` 的毒锁 `Err(Io)` 路径确定性可达。实现：持锁闭包内 synthetic panic
    /// 真实毒化 mutex，`catch_unwind` 在闭包外捕获、panic 不传播；不清毒、不修改记录、无 Fs 读写。
    /// 全限定路径，不新增 import。
    #[cfg(any(test, feature = "test-seam"))]
    pub fn poison_records_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = self.records.lock();
            panic!("synthetic poison seam: records mutex is now poisoned (test-only)");
        }));
    }
}
```
   > `get` 不经 `Fs::read`（`open` 后的内存 `records` 是唯一数据源），因此 Task 4 的 dedup get 失败用例不使用 `FakeJournal` 的旧文件读故障 seam（已删除），而使用 `poison_records_for_test` 毒锁注入（见 Task 4 `journal_dedup_get_failure_results_in_indeterminate_with_zero_os_calls`）。
   c. PASS：
```bash
cargo test --offline --locked -p rsetup-board-agent --features test-seam
```
预期：PASS（`wire_04_journal` 全部契约用例 + `journal_fault_injection` 全部注入用例）。
   d. 提交：
```bash
cargo fmt -p rsetup-board-agent
cargo clippy --offline --locked -p rsetup-board-agent --features test-seam -- -D warnings
git add crates/rsetup-board-agent/src/journal.rs crates/rsetup-board-agent/tests/wire_04_journal.rs
git commit -m "feat(board-agent): journal 去重、容量上限与 boot 切换证据恢复"
```

---


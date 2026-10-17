//! 幂等任务 journal：持久稳定 epoch、accepted/pre_boot 落盘与两阶段 fsync 提交。
//!
//! 本模块不引入任何密码学实现与 OS 副作用；全部文件操作经 [`Fs`] 缝注入，
//! 生产用 [`RealFs`]，测试可注入替身。持久格式为 serde/serde_json 结构化
//! 编解码（绝不手写伪解析），提交序固定为
//! `create_write -> write_all -> sync_file -> rename -> sync_dir -> remove_file -> sync_dir`。

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub const JOURNAL_SCHEMA_VERSION: u32 = 1;
/// 03 §5 待审默认，G0 审阅后才可定稿。
pub const JOURNAL_MAX_RECORDS: usize = 4096;

// ---------------------------------------------------------------------------
// 状态与记录
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Accepted,
    Attempted,
    Succeeded,
    Failed,
    Unknown,
}

impl TaskState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Attempted => "Attempted",
            Self::Succeeded => "Succeeded",
            Self::Failed => "Failed",
            Self::Unknown => "Unknown",
        }
    }

    pub fn from_str_exact(value: &str) -> Option<TaskState> {
        match value {
            "Accepted" => Some(TaskState::Accepted),
            "Attempted" => Some(TaskState::Attempted),
            "Succeeded" => Some(TaskState::Succeeded),
            "Failed" => Some(TaskState::Failed),
            "Unknown" => Some(TaskState::Unknown),
            _ => None,
        }
    }
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
    pub fn to_wire(&self) -> rsetup_protocol::wire::TaskRecord {
        rsetup_protocol::wire::TaskRecord {
            schema_version: self.schema_version,
            sub_task_id: self.sub_task_id.clone(),
            journal_epoch: self.journal_epoch.clone(),
            record_version: self.record_version,
            state: self.state.as_str().to_ascii_lowercase(),
            pre_boot_id: self.pre_boot_id.clone(),
            observed_boot_id: self.observed_boot_id.clone(),
            reason_code: self.reason_code.clone(),
            updated_unix_ms: self.updated_unix_ms,
            evidence_kind: self.evidence_kind.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// 错误与结果枚举
// ---------------------------------------------------------------------------

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
pub enum DedupOutcome {
    Inserted,
    ReusedExisting,
}

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

// ---------------------------------------------------------------------------
// 文件操作缝
// ---------------------------------------------------------------------------

/// 最小文件操作缝：生产 `RealFs`，测试 `RecordingJournal` / `FaultyFs`。
pub trait Fs: Send + Sync {
    fn create_dir_all(&self, dir: &Path) -> std::io::Result<()>;
    fn create_write(&self, path: &Path) -> std::io::Result<Box<dyn std::io::Write + Send>>;
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>>;
    fn sync_file(&self, path: &Path) -> std::io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn remove_file(&self, path: &Path) -> std::io::Result<()>;
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()>;
}

#[derive(Clone, Copy, Debug)]
pub struct RealFs;

impl Fs for RealFs {
    fn create_dir_all(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)
    }

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

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        std::fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        std::fs::remove_file(path)
    }

    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::File::open(dir)?.sync_all()
    }
}

// ---------------------------------------------------------------------------
// 持久格式（serde 结构化编解码）
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// JournalStore
// ---------------------------------------------------------------------------

/// **uncertain 隔离契约（TR-N2 / 正审 R-02，fail-closed）**：`write_durable` 返回
/// `CommitUnknown` 时磁盘可能已落地新记录而内存未换，本实例进入不可逆隔离态：
/// 此后 `get`、`insert_accepted` 等读写入口在加锁与任何 I/O 之前 fail-closed，
/// 返回固定去敏 `JournalError::Io`，绝不把旧内存快照当确定事实返回。
/// 受控恢复仅限丢弃该实例后重新 `open`/`recover`，或显式 `rebuild_after_loss`。
pub struct JournalStore<F: Fs> {
    dir: PathBuf,
    epoch: String,
    fs: F,
    records: Mutex<Vec<StoredRecord>>,
    uncertain: AtomicBool,
}

/// 类型别名：提供给不需要替换文件后端的生产调用方，兼容上游 transport 计划的命名习惯。
pub type TaskJournal = JournalStore<RealFs>;

/// uncertain 隔离态固定去敏错误（TR-N2）：不含记录内容，绝不回旧内存快照。
fn uncertain_error() -> JournalError {
    JournalError::Io(
        "journal store in uncertain state: CommitUnknown requires reopen or controlled rebuild"
            .to_string(),
    )
}

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

/// 加载恢复时的严格持久格式与语义校验（TR-N3 / 正审 R-03 / 正审 R2 TR-R6-2）：未知 schema 版本、
/// epoch 格式与一致性、记录内层一致性（外层与内层 sub_task_id 必须相等）、合法状态、规范 64 小写
/// hex controller 与 payload hash、UUIDv4 sub_task_id 与 pre_boot_id/observed_boot_id、
/// reason_code 十二词表、boot 证据完整性、去重键唯一性等。
fn validate_loaded_journal(meta: &JournalMeta, logs: &JournalLogs) -> Result<(), JournalError> {
    if meta.v != JOURNAL_SCHEMA_VERSION || logs.v != JOURNAL_SCHEMA_VERSION {
        return Err(JournalError::Decode(
            "unsupported journal schema version".to_string(),
        ));
    }
    if logs.epoch != meta.epoch {
        return Err(JournalError::Decode("epoch mismatch in logs".to_string()));
    }
    if !rsetup_protocol::wire::is_canonical_uuid_v4(&meta.epoch) {
        return Err(JournalError::Decode(
            "invalid journal epoch format".to_string(),
        ));
    }
    let mut seen_keys = std::collections::HashSet::new();
    for stored in &logs.records {
        if stored.record.schema_version != rsetup_protocol::wire::BUSINESS_SCHEMA_VERSION {
            return Err(JournalError::Decode(
                "unsupported record schema version".to_string(),
            ));
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
            return Err(JournalError::Decode(
                "invalid sub_task_id format".to_string(),
            ));
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
            return Err(JournalError::Decode(
                "invalid pre_boot_id format".to_string(),
            ));
        }
        if let Some(observed) = stored.record.observed_boot_id.as_deref() {
            if !rsetup_protocol::wire::is_canonical_uuid_v4(observed) {
                return Err(JournalError::Decode(
                    "invalid observed_boot_id format".to_string(),
                ));
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
        // TR-R8-1：Succeeded 必须持有完整 boot transition 证据（spec 03 §165-170）。
        // 上方 match 的 None 臂只拦「自称证据不完整」，拦不住反方向：state = Succeeded 且
        // 完全无证据的记录放行。联合守卫强制三重条件：evidence_kind 为
        // boot_transition_observed、observed_boot_id 存在、且与 pre_boot_id 不同。
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

/// 固定两阶段落盘与原子重命名，按失败边界精确分类（Task 3.2）：
/// 0=create_write, 1=write, 2=sync_file, 3=rename, 4=sync_dir, 5=remove_file, 6=sync_dir。
///
/// - (0)(1)(2) rename 调用之前确定失败：磁盘确定未变 → `DefinitelyNotCommitted`；
/// - (3) rename 调用本身失败：文件系统层状态未知（可能部分提交或已修改目录项）→ `CommitUnknown`；
/// - (4) rename 之后的父目录 sync 失败：掉电可能导致元数据回滚 → `CommitUnknown`；
/// - (5)(6) 提交点之后的收尾尽力而为，失败不否定提交。
///
/// 临时文件路径严密拒绝非法路径且不 fallback。
fn write_durable<F: Fs>(
    fs: &F,
    dir: &Path,
    final_path: &Path,
    body: &[u8],
) -> Result<(), JournalError> {
    let file_name = final_path
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| JournalError::Io("invalid target path for durable write".to_string()))?;
    let tmp = dir.join(format!(".{}.tmp-{}", file_name, uuid::Uuid::new_v4()));
    // (0)(1)(2) rename 之前确定失败 => DefinitelyNotCommitted
    let mut file = fs
        .create_write(&tmp)
        .map_err(|_| JournalError::DefinitelyNotCommitted)?;
    file.write_all(body)
        .map_err(|_| JournalError::DefinitelyNotCommitted)?;
    drop(file); // 必须先释放文件句柄，再做 sync
    fs.sync_file(&tmp)
        .map_err(|_| JournalError::DefinitelyNotCommitted)?;
    // (3) rename 调用本身失败：文件系统层状态未知 => CommitUnknown
    fs.rename(&tmp, final_path)
        .map_err(|_| JournalError::CommitUnknown)?;
    // (4) rename 之后的父目录 sync 失败：掉电可能回滚目录项 => CommitUnknown
    fs.sync_dir(dir).map_err(|_| JournalError::CommitUnknown)?;
    // (5) 清理临时文件属于提交点之后的收尾：删除失败不否定提交
    let _ = fs.remove_file(&tmp);
    // (6) 第二次父目录 sync 尽力而为
    let _ = fs.sync_dir(dir);
    Ok(())
}

/// 由持久内层记录构造内存 `TaskRecord`；未知 state 字符串固定去敏 `Decode`。
fn task_record_from(stored: &StoredTaskRecord) -> Result<TaskRecord, JournalError> {
    let state = TaskState::from_str_exact(&stored.state)
        .ok_or_else(|| JournalError::Decode("journal.state_format".to_owned()))?;
    Ok(TaskRecord {
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
    })
}

impl<F: Fs> JournalStore<F> {
    pub const META_FILE: &'static str = "journal-meta.json";
    pub const LOG_FILE: &'static str = "journal-logs.json";

    /// 首次初始化：生成并持久化 `journal_epoch`；已存在则**沿用**已持久化 epoch。
    ///
    /// open 绝不创建目录：目录缺失必须 Err（`write_durable` 的首个 `create_write` 以 ENOENT 失败）。
    /// J-01 修订：先读齐 META_FILE 与 LOG_FILE 两个文件，再按 (meta, logs) 组合分支；
    /// 恰一个 Some（文件集不完整）固定去敏 `Decode("journal.incomplete_files")` fail-closed，
    /// 绝不返回同 epoch 空 store、绝不自动 rebuild、不改旧 epoch、不删除任何现存文件。
    pub fn open(dir: &Path, fs: F) -> Result<Self, JournalError> {
        let meta_path = dir.join(Self::META_FILE);
        let logs_path = dir.join(Self::LOG_FILE);
        let meta_bytes = fs
            .read(&meta_path)
            .map_err(|e| JournalError::Io(e.to_string()))?;
        let logs_bytes = fs
            .read(&logs_path)
            .map_err(|e| JournalError::Io(e.to_string()))?;
        match (meta_bytes, logs_bytes) {
            // (None, None) 首次初始化：生成新 UUIDv4 epoch，同一函数体内先落空 LOG_FILE、
            // 再落 META_FILE，两条 write_durable 都成功才返回 Ok；任何一条失败直接以 `?` 返回 Err。
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
                write_durable(&fs, dir, &logs_path, &logs_body)?;
                let meta = JournalMeta {
                    v: JOURNAL_SCHEMA_VERSION,
                    epoch: epoch.clone(),
                };
                let meta_body =
                    serde_json::to_vec(&meta).map_err(|e| JournalError::Decode(e.to_string()))?;
                write_durable(&fs, dir, &meta_path, &meta_body)?;
                Ok(Self {
                    dir: dir.to_path_buf(),
                    epoch,
                    fs,
                    records: Mutex::new(Vec::new()),
                    uncertain: AtomicBool::new(false),
                })
            }
            // (Some, Some) 普通恢复：decode 两个文件，保留 meta.epoch 并执行完整语义校验（TR-N3 / 正审 R-03）。
            (Some(meta_bytes), Some(logs_bytes)) => {
                let meta: JournalMeta = serde_json::from_slice(&meta_bytes)
                    .map_err(|e| JournalError::Decode(e.to_string()))?;
                let logs: JournalLogs = serde_json::from_slice(&logs_bytes)
                    .map_err(|e| JournalError::Decode(e.to_string()))?;
                validate_loaded_journal(&meta, &logs)?;
                Ok(Self {
                    dir: dir.to_path_buf(),
                    epoch: meta.epoch,
                    fs,
                    records: Mutex::new(logs.records),
                    uncertain: AtomicBool::new(false),
                })
            }
            // exactly one Some：文件集不完整（部分丢失或初始化被打断）。固定去敏 Decode 错误；
            // 恢复只能由调用方显式走既有受控路径 `rebuild_after_loss`，open 自身永不触发重建。
            _ => Err(JournalError::Decode("journal.incomplete_files".to_owned())),
        }
    }

    /// 普通启动：3.1 委托 `open`，绝不做自动状态改写。
    pub fn recover(dir: &Path, fs: F, _current_boot_id: &str) -> Result<Self, JournalError> {
        Self::open(dir, fs)
    }

    /// 显式重建路径（仅在已知日志不可逆损坏时调用）：3.3 实现。
    pub fn rebuild_after_loss(
        _dir: &Path,
        _fs: F,
    ) -> Result<(Self, JournalRecovery), JournalError> {
        Err(JournalError::NotFound) // 桩：3.3 实现
    }

    pub fn journal_epoch(&self) -> &str {
        &self.epoch
    }

    /// 去重只读查询：纯内存查找、无 I/O、不改 records/epoch/version。
    /// 候选 = `controller_hex == encode_journal_hash32(controller_key)`（lowerhex64）且 `sub_task_id` 精确匹配；
    /// uncertain 隔离态（TR-N2）先于一切：入口检查 `self.uncertain`，为真直接返回固定去敏 `Io`；
    /// 毒锁 → 固定去敏 `Io`（fail-closed：直接返回、不 panic、不吞毒）；
    /// 0 命中 → `Ok(None)`；>1 命中 → `Decode("journal.duplicate_record")`；恰 1 命中 → 构造 `TaskRecord`。
    pub fn get(
        &self,
        controller_key: &[u8; 32],
        sub_task_id: &str,
    ) -> Result<Option<TaskRecord>, JournalError> {
        self.ensure_not_uncertain()?;
        let expected_controller_hex = encode_journal_hash32(controller_key);
        let records = match self.records.lock() {
            Ok(guard) => guard,
            // fail-closed：毒锁 → 直接返回（消息去敏，不含记录内容）
            Err(_) => return Err(JournalError::Io("journal.records poisoned".to_string())),
        };
        self.ensure_not_uncertain()?;
        let hits: Vec<&StoredRecord> = records
            .iter()
            .filter(|r| r.controller_hex == expected_controller_hex && r.sub_task_id == sub_task_id)
            .collect();
        match hits.len() {
            0 => Ok(None),
            1 => task_record_from(&hits[0].record).map(Some),
            // 同键重复记录 = 损坏：固定去敏错误（绝不返回第一条、不伪 None）
            _ => Err(JournalError::Decode("journal.duplicate_record".to_owned())),
        }
    }

    pub fn record_count(&self) -> usize {
        self.records.lock().map(|guard| guard.len()).unwrap_or(0)
    }

    /// 供 reopen 测试：返回本实例已打开的 journal 目录路径。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 仅用于测试见证：暴露底层 `Fs` 的共享引用（不是新的行为接口，只是让见证对象可观察）。
    pub fn fs_handle(&self) -> &F {
        &self.fs
    }

    /// 实例提交路径的统一入口（`open` 之外的全部落盘提交都经此函数）。
    ///
    /// `write_durable` 返回 `CommitUnknown` 时，必须在把错误返回给调用方**之前**先置
    /// `uncertain = true`（TR-N2 / 正审 R-02）：磁盘可能已落地新记录而内存未换，本实例进入
    /// 不可逆隔离态。`DefinitelyNotCommitted` 表示 rename 之前确定失败、磁盘确定未变，不置位。
    fn commit_durable(&self, final_path: &Path, body: &[u8]) -> Result<(), JournalError> {
        match write_durable(&self.fs, &self.dir, final_path, body) {
            Ok(()) => Ok(()),
            Err(err @ JournalError::CommitUnknown) => {
                self.uncertain.store(true, Ordering::SeqCst);
                Err(err)
            }
            Err(err) => Err(err),
        }
    }

    /// uncertain 隔离态入口守卫（TR-N2）：为真即返回固定去敏 `Io`，先于任何加锁与 I/O，
    /// 绝不返回旧内存快照、绝不发起新写入。
    fn ensure_not_uncertain(&self) -> Result<(), JournalError> {
        if self.uncertain.load(Ordering::SeqCst) {
            return Err(uncertain_error());
        }
        Ok(())
    }

    /// accepted：`pre_boot_id` 必须在本次调用内即确定并落盘。
    ///
    /// 去重命名空间 = `controller_key` + `sub_task_id`：命中且载荷哈希相同 → 复用原记录
    /// （`ReusedExisting`，绝不递增 `record_version`）；命中但哈希不同 → `Err(Conflict)`；
    /// 未命中 → 先在内存构造候选记录，`write_durable` 落盘成功后才替换内存状态，
    /// 杜绝「先改内存再落盘失败」的窗口。
    pub fn insert_accepted(
        &self,
        controller_key: &[u8; 32],
        sub_task_id: &str,
        payload_hash: &[u8; 32],
        pre_boot_id: &str,
    ) -> Result<(TaskRecord, DedupOutcome), JournalError> {
        self.ensure_not_uncertain()?;
        let controller_hex = encode_journal_hash32(controller_key);
        let payload_hash_hex = encode_journal_hash32(payload_hash);
        let mut records = match self.records.lock() {
            Ok(guard) => guard,
            Err(_) => return Err(JournalError::Io("journal.records poisoned".to_string())),
        };
        self.ensure_not_uncertain()?;
        let hits: Vec<&StoredRecord> = records
            .iter()
            .filter(|r| r.controller_hex == controller_hex && r.sub_task_id == sub_task_id)
            .collect();
        match hits.len() {
            0 => {}
            1 => {
                let existing = hits[0];
                if existing.payload_hash_hex != payload_hash_hex {
                    return Err(JournalError::Conflict);
                }
                return task_record_from(&existing.record)
                    .map(|record| (record, DedupOutcome::ReusedExisting));
            }
            _ => return Err(JournalError::Decode("journal.duplicate_record".to_owned())),
        }
        // 新插入分支：触碰文件系统之前校验 UUIDv4 格式（与 validate_loaded_journal 同标准）。
        // 若不拦截，非法 sub_task_id/pre_boot_id 会持久化到磁盘，导致下次 open 时
        // validate_loaded_journal 判定整个 journal 损坏、破坏既有有效记录的恢复。
        if !rsetup_protocol::wire::is_canonical_uuid_v4(sub_task_id) {
            return Err(JournalError::Decode(
                "invalid sub_task_id format".to_string(),
            ));
        }
        if !rsetup_protocol::wire::is_canonical_uuid_v4(pre_boot_id) {
            return Err(JournalError::Decode(
                "invalid pre_boot_id format".to_string(),
            ));
        }
        let stored = StoredRecord {
            controller_hex,
            sub_task_id: sub_task_id.to_owned(),
            payload_hash_hex,
            record: StoredTaskRecord {
                schema_version: rsetup_protocol::wire::BUSINESS_SCHEMA_VERSION,
                sub_task_id: sub_task_id.to_owned(),
                journal_epoch: self.epoch.clone(),
                record_version: 1,
                state: TaskState::Accepted.as_str().to_owned(),
                pre_boot_id: pre_boot_id.to_owned(),
                observed_boot_id: None,
                reason_code: None,
                updated_unix_ms: None,
                evidence_kind: None,
            },
        };
        let mut candidate = records.clone();
        candidate.push(stored.clone());
        let logs = JournalLogs {
            v: JOURNAL_SCHEMA_VERSION,
            epoch: self.epoch.clone(),
            records: candidate.clone(),
        };
        let body = serde_json::to_vec(&logs).map_err(|e| JournalError::Decode(e.to_string()))?;
        let logs_path = self.dir.join(Self::LOG_FILE);
        self.commit_durable(&logs_path, &body)?;
        // 落盘成功后才替换内存状态
        *records = candidate;
        Ok((task_record_from(&stored.record)?, DedupOutcome::Inserted))
    }

    /// 3.3 实现。
    pub fn mark_attempted(
        &self,
        _controller_key: &[u8; 32],
        _sub_task_id: &str,
    ) -> Result<TaskRecord, JournalError> {
        self.ensure_not_uncertain()?; // uncertain 隔离态先于一切（TR-N2）
        Err(JournalError::NotFound) // 桩
    }

    /// 3.3 实现。
    pub fn finish(
        &self,
        _controller_key: &[u8; 32],
        _sub_task_id: &str,
        _state: TaskState,
        _reason: Option<&str>,
    ) -> Result<TaskRecord, JournalError> {
        self.ensure_not_uncertain()?; // uncertain 隔离态先于一切（TR-N2）
        Err(JournalError::NotFound) // 桩
    }

    /// 3.3 实现。
    pub fn finish_succeeded_with_boot(
        &self,
        _controller_key: &[u8; 32],
        _sub_task_id: &str,
        _observed_boot_id: &str,
    ) -> Result<TaskRecord, JournalError> {
        self.ensure_not_uncertain()?; // uncertain 隔离态先于一切（TR-N2）
        Err(JournalError::NotFound) // 桩
    }

    /// 3.3 实现。
    pub fn apply_boot_transition_evidence(
        &self,
        _current_boot_id: &str,
    ) -> Result<Vec<TaskRecord>, JournalError> {
        self.ensure_not_uncertain()?; // uncertain 隔离态先于一切（TR-N2）
        Err(JournalError::NotFound) // 桩
    }

    /// 3.3 实现。
    pub fn merge_evidence(
        &self,
        _controller_key: &[u8; 32],
        _incoming: &TaskRecord,
    ) -> Result<EvidenceMerge, JournalError> {
        self.ensure_not_uncertain()?; // uncertain 隔离态先于一切（TR-N2）
        Err(JournalError::NotFound) // 桩
    }
}

// ---------------------------------------------------------------------------
// 固定 hex 字段纯检查 helper 的 lib 测试（真实实现见上方 encode/decode_journal_hash32）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod hash_hex_tests {
    use super::{JournalError, decode_journal_hash32, encode_journal_hash32};

    /// 唯一纯 lib 测试：64 小写 ASCII hex 规范往返 + 8 类坏格式固定 `Decode` 去敏（消息不含实际 input）。
    #[test]
    fn journal_hash32_hex_helpers_are_canonical_and_roundtrip() {
        let zero = [0u8; 32];
        let ff = [0xffu8; 32];
        let custom = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b,
            0x1c, 0x1d, 0x1e, 0x1f,
        ];
        assert_eq!(encode_journal_hash32(&zero), "00".repeat(32));
        assert_eq!(encode_journal_hash32(&ff), "ff".repeat(32));
        assert_eq!(
            encode_journal_hash32(&custom),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
        );
        assert_eq!(
            decode_journal_hash32(&encode_journal_hash32(&zero)).unwrap(),
            zero
        );
        assert_eq!(
            decode_journal_hash32(&encode_journal_hash32(&ff)).unwrap(),
            ff
        );
        assert_eq!(
            decode_journal_hash32(
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
            )
            .unwrap(),
            custom
        );
        let expected: Result<[u8; 32], JournalError> =
            Err(JournalError::Decode("journal.hash_format".to_owned()));
        let bad: Vec<String> = vec![
            String::new(),        // 1 空
            "0".repeat(63),       // 2 长度 63
            "0".repeat(65),       // 3 长度 65
            "FF".repeat(32),      // 4 uppercase
            "zz".repeat(32),      // 5 non-hex
            "0".repeat(63) + " ", // 6 空格（仍 64 字符）
            "0x".repeat(32),      // 7 `0x` prefix
            "\u{e9}".repeat(32),  // 8 Unicode（UTF-8 每字符 2 字节，字节长度恰 64）
        ];
        for (idx, value) in bad.iter().enumerate() {
            assert_eq!(decode_journal_hash32(value), expected, "bad case {idx}");
        }
    }
}

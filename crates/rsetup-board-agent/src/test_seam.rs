//! 仅测试/见证用途的文件操作替身。生产构建中通过 `test-seam` feature 显式开启。
//!
//! `RecordingJournal` 包裹真实 `RealFs`，按调用顺序记录每一个 `write_durable` 触达的
//! 文件操作，并支持「相对激活时刻调用计数偏移」的一次性故障注入，用于见证两阶段落盘
//! 的固定顺序与 rename 边界失败分类。

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
    pub fn new() -> Self {
        Self {
            fs: crate::journal::RealFs,
            state: Arc::new(RecordingState::default()),
        }
    }
}

/// 落地于 `crates/rsetup-board-agent/src/test_seam.rs`，紧邻 `impl RecordingJournal`。
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
    fn read(&self, path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        self.fs.read(path)
    }
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

    /// 已记录的文件操作顺序快照。
    pub fn ops(&self) -> Vec<FsOp> {
        self.state.ops.lock().unwrap().clone()
    }

    /// 在共享状态上激活故障：让「从激活时刻调用计数起的第 offset 次」文件操作失败。
    /// 必须在 open 成功后调用，offset 才落在目标记录提交窗口。
    /// offset 的含义与 `write_durable` 的固定调用序一一对应：
    /// 0=create_write, 1=write, 2=sync_file, 3=rename, 4=sync_dir(提交点), 5=remove_file, 6=sync_dir(收尾)
    pub fn arm_at_offset(&self, offset: usize, kind: std::io::ErrorKind) {
        let calls = *self.state.calls.lock().unwrap();
        self.state
            .failures
            .lock()
            .unwrap()
            .push((calls + offset, kind));
    }

    /// 链式便捷：在激活时刻（open 之前）取基准，仅供针对 open 自身落盘阶段的用例使用。
    pub fn fail_at_offset(self, offset: usize, kind: std::io::ErrorKind) -> Self {
        self.arm_at_offset(offset, kind);
        self
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
        return Err(std::io::Error::new(
            kind,
            format!("injected failure at op {index}"),
        ));
    }
    Ok(())
}

/// 包装真实 writer，使「写入」本身成为可见证、可注入失败的一次调用，
/// 从而能断言写入确实发生在 `sync_file` 之前。
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
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

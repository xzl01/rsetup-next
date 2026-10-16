//! Task 3.2：两阶段 fsync 顺序见证、rename 边界失败注入分类与 uncertain 隔离态。
//!
//! 全部用例走 `RecordingJournal` 替身（真实落盘 + 调用序记录 + 一次性故障注入），
//! 故障偏移以「open 成功后的激活时刻调用计数」为基准，与 `write_durable` 的固定
//! 调用序一一对应：0=create_write, 1=write, 2=sync_file, 3=rename, 4=sync_dir(提交点),
//! 5=remove_file, 6=sync_dir(收尾)。

mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::journal::{DedupOutcome, JournalError, JournalStore, RealFs, TaskState};
use rsetup_board_agent::test_seam::{FsOp, RecordingJournal};
use std::io::ErrorKind;

const BOOT_OLD: &str = "6f8b1c2d-0000-4000-8000-000000000001";
const SUB_TASK: &str = "9c858901-8a57-4791-81fe-4c455b099bc9";

fn op_index(ops: &[FsOp], wanted: &str) -> usize {
    ops.iter()
        .position(|op| {
            matches!(
                (op, wanted),
                (FsOp::CreateWrite(_), "create")
                    | (FsOp::Write(_), "write")
                    | (FsOp::SyncFile(_), "sync_file")
                    | (FsOp::Rename(..), "rename")
                    | (FsOp::RemoveFile(_), "remove")
                    | (FsOp::SyncDir(_), "sync_dir")
            )
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
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    let ops = fs_handle.ops()[mark..].to_vec();

    let create = op_index(&ops, "create");
    let write = op_index(&ops, "write");
    let sync_file = op_index(&ops, "sync_file");
    let rename = op_index(&ops, "rename");
    let sync_dir = op_index(&ops, "sync_dir");
    assert!(
        create < write,
        "temp file must be created before writing: {ops:?}"
    );
    assert!(
        write < sync_file,
        "file sync_all must follow the write: {ops:?}"
    );
    assert!(
        sync_file < rename,
        "rename must follow the file sync_all: {ops:?}"
    );
    assert!(
        rename < sync_dir,
        "parent dir sync_all must follow the rename: {ops:?}"
    );
    // 临时文件清理发生在提交点之后，绝不夹在 rename 之前
    if let Some(remove) = ops.iter().position(|op| matches!(op, FsOp::RemoveFile(_))) {
        assert!(
            sync_dir < remove,
            "temp cleanup must not precede the commit sync: {ops:?}"
        );
    }
}

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
        assert_eq!(
            error,
            JournalError::DefinitelyNotCommitted,
            "offset={offset} ({name})"
        );
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
    store
        .fs_handle()
        .arm_at_offset(5, ErrorKind::PermissionDenied);
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
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    for op in store.fs_handle().ops()[mark..].to_vec() {
        if let FsOp::Rename(from, to) = op {
            let from_name = from.file_name().unwrap().to_string_lossy().to_string();
            let to_name = to.file_name().unwrap().to_string_lossy().to_string();
            assert_ne!(from_name, to_name);
            assert!(
                from_name.starts_with('.'),
                "temp file must be hidden: {from_name}"
            );
            assert!(
                from_name.contains(".tmp-"),
                "temp file must be uniquely tagged: {from_name}"
            );
            assert_eq!(to_name, JournalStore::<RealFs>::LOG_FILE);
        }
    }
}

#[test]
fn commit_unknown_puts_store_into_irreversible_uncertain_state() {
    let key = [9u8; 32];
    let dir = TempDir::new("uncertain");
    let store = JournalStore::open(dir.path(), RecordingJournal::new()).expect("open");
    store.fs_handle().arm_at_offset(3, ErrorKind::Other);
    assert_eq!(
        store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD),
        Err(JournalError::CommitUnknown)
    );
    // 隔离态：读写入口一律 fail-closed 为固定去敏 Io，绝不返回旧内存记录
    let ops_before = store.fs_handle().ops().len();
    let get_err = store
        .get(&key, SUB_TASK)
        .expect_err("get must fail closed in uncertain state");
    assert!(
        matches!(&get_err, JournalError::Io(msg) if msg.contains("uncertain state")),
        "{get_err:?}"
    );
    let insert_err = store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect_err("insert must fail closed in uncertain state");
    assert!(
        matches!(&insert_err, JournalError::Io(msg) if msg.contains("uncertain state")),
        "{insert_err:?}"
    );
    // 隔离态绝不发起新的文件操作，且不可逆（同一实例后续调用持续 fail-closed）
    assert_eq!(
        store.fs_handle().ops().len(),
        ops_before,
        "uncertain store must not touch the filesystem again"
    );
    assert!(matches!(
        store.get(&key, SUB_TASK),
        Err(JournalError::Io(_))
    ));
    // 受控恢复：丢弃实例重新 open 才可用
    drop(store);
    let reopened = JournalStore::open(dir.path(), RealFs).expect("reopen after uncertain");
    assert_eq!(reopened.record_count(), 0);
}

#[test]
fn definite_failure_does_not_enter_uncertain_state() {
    let key = [9u8; 32];
    let dir = TempDir::new("not-uncertain");
    let store = JournalStore::open(dir.path(), RecordingJournal::new()).expect("open");
    // 偏移 2 = rename 之前的 sync_file 失败：磁盘确定未变
    store.fs_handle().arm_at_offset(2, ErrorKind::Other);
    assert_eq!(
        store.insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD),
        Err(JournalError::DefinitelyNotCommitted)
    );
    // 未进入隔离态：同一实例可继续提交并读回
    let (record, outcome) = store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("retry after definite failure must succeed");
    assert_eq!(outcome, DedupOutcome::Inserted);
    assert_eq!(record.state, TaskState::Accepted);
    assert_eq!(store.record_count(), 1);
    assert_eq!(
        store
            .get(&key, SUB_TASK)
            .expect("get after definite failure")
            .expect("record")
            .state,
        TaskState::Accepted
    );
}

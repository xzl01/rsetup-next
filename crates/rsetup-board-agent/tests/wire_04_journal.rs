mod support;
use support::tempdir::TempDir;

use rsetup_board_agent::journal::{
    DedupOutcome, EvidenceMerge, JOURNAL_MAX_RECORDS, JournalError, JournalRecovery, JournalStore,
    RealFs, TaskState,
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
    assert!(
        rsetup_protocol::wire::is_canonical_uuid_v4(&epoch),
        "epoch must be a uuid: {epoch}"
    );

    let (record, outcome) = first
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("insert accepted");
    assert_eq!(outcome, DedupOutcome::Inserted);
    assert_eq!(record.state, TaskState::Accepted);
    assert_eq!(
        record.pre_boot_id, BOOT_OLD,
        "pre_boot_id must be known at accepted"
    );
    assert_eq!(record.journal_epoch, epoch);
    assert_eq!(record.record_version, 1);
    assert_eq!(record.schema_version, BUSINESS_SCHEMA_VERSION);
    drop(first);

    // 普通进程重启：epoch 绝不刷新，记录仍在
    let second = open_store(&dir);
    assert_eq!(
        second.journal_epoch(),
        epoch,
        "normal reopen must reuse the persisted epoch"
    );
    let reread = second
        .get(&key, SUB_TASK)
        .expect("get")
        .expect("record must survive reopen");
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
    assert_ne!(
        rebuilt.journal_epoch(),
        epoch,
        "loss rebuild must rotate the epoch"
    );
    assert_eq!(
        outcome,
        JournalRecovery::RebuiltAfterLoss { journal_lost: true }
    );
}

#[test]
fn accepted_reopen_never_downgrades_state() {
    let dir = TempDir::new("no-auto");
    let key = [9u8; 32];
    {
        let store = open_store(&dir);
        store
            .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
            .expect("accepted");
    }
    // 恢复时当前 boot 未变：既不得自动补调用，也不得擅自改写状态
    let recovered = JournalStore::recover(dir.path(), RealFs, BOOT_OLD).expect("recover");
    let record = recovered.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(
        record.state,
        TaskState::Accepted,
        "accepted without attempted must not be auto-advanced"
    );
    assert_eq!(record.observed_boot_id, None);
}

#[test]
fn attempted_record_with_changed_boot_becomes_succeeded_with_evidence() {
    let dir = TempDir::new("boot-change");
    let key = [9u8; 32];
    {
        let store = open_store(&dir);
        store
            .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
            .expect("accepted");
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
    assert_eq!(
        record.evidence_kind.as_deref(),
        Some("boot_transition_observed")
    );
    assert!(
        record.record_version > 2,
        "evidence application must bump the version"
    );
}

#[test]
fn attempted_record_with_same_boot_stays_attempted() {
    let dir = TempDir::new("boot-same");
    let key = [9u8; 32];
    {
        let store = open_store(&dir);
        store
            .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
            .expect("accepted");
        store.mark_attempted(&key, SUB_TASK).expect("attempted");
    }
    let recovered = JournalStore::recover(dir.path(), RealFs, BOOT_OLD).expect("recover");
    let record = recovered.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(
        record.state,
        TaskState::Attempted,
        "same boot is not success evidence"
    );
    assert_eq!(record.observed_boot_id, None);
}

#[test]
fn duplicate_sub_task_id_returns_record_and_conflicting_payload_is_rejected() {
    let dir = TempDir::new("dedup");
    let key = [9u8; 32];
    let store = open_store(&dir);

    let (first, outcome) = store
        .insert_accepted(&key, SUB_TASK, &[1u8; 32], BOOT_OLD)
        .expect("first");
    assert_eq!(outcome, DedupOutcome::Inserted);

    let (again, outcome) = store
        .insert_accepted(&key, SUB_TASK, &[1u8; 32], BOOT_OLD)
        .expect("replay");
    assert_eq!(outcome, DedupOutcome::ReusedExisting);
    assert_eq!(
        again.record_version, first.record_version,
        "replay must not bump the version"
    );
    assert_eq!(again.pre_boot_id, BOOT_OLD);

    assert_eq!(
        store.insert_accepted(&key, SUB_TASK, &[2u8; 32], BOOT_OLD),
        Err(JournalError::Conflict)
    );
    // 冲突拒绝不得改写已落盘的去重哈希
    assert_eq!(
        store.payload_hash(&key, SUB_TASK).expect("payload hash"),
        Some([1u8; 32])
    );

    // 不同 controller 命名空间互不影响
    let (other, outcome) = store
        .insert_accepted(&[7u8; 32], SUB_TASK, &[2u8; 32], BOOT_OLD)
        .expect("other key");
    assert_eq!(outcome, DedupOutcome::Inserted);
    assert_eq!(other.sub_task_id, SUB_TASK);
}

#[test]
fn payload_hash_returns_stored_hash_only_for_the_same_dedup_key() {
    let dir = TempDir::new("payload-hash");
    let key = [9u8; 32];
    let store = open_store(&dir);
    let unused_sub_task = "11111111-2222-4333-8444-555555555555";

    // 未命中：尚无任何记录
    assert_eq!(store.payload_hash(&key, SUB_TASK).expect("miss"), None);

    store
        .insert_accepted(&key, SUB_TASK, &[7u8; 32], BOOT_OLD)
        .expect("accepted");

    // 命中：返回 accepted 时落盘的规范载荷哈希
    assert_eq!(
        store.payload_hash(&key, SUB_TASK).expect("hit"),
        Some([7u8; 32])
    );
    // 同 controller、不同 sub_task_id → 未命中
    assert_eq!(
        store.payload_hash(&key, unused_sub_task).expect("miss"),
        None
    );
    // 同 sub_task_id、不同 controller 命名空间 → 未命中
    assert_eq!(
        store.payload_hash(&[7u8; 32], SUB_TASK).expect("miss"),
        None
    );

    // 只读查询：不新增记录、不递增 record_version
    assert_eq!(store.record_count(), 1);
    assert_eq!(
        store
            .get(&key, SUB_TASK)
            .expect("get")
            .expect("record")
            .record_version,
        1
    );

    // 哈希是持久字段：重开（新实例）后仍返回同一值
    drop(store);
    let reopened = open_store(&dir);
    assert_eq!(
        reopened
            .payload_hash(&key, SUB_TASK)
            .expect("hit after reopen"),
        Some([7u8; 32])
    );
}

#[test]
fn full_journal_rejects_new_records_without_evicting_dedup_entries() {
    let dir = TempDir::new("capacity");
    let store = open_store(&dir);
    let key = [3u8; 32];
    for i in 0..JOURNAL_MAX_RECORDS {
        let id = format!("00000000-0000-4000-8000-{i:012}");
        store
            .insert_accepted(&key, &id, &[0u8; 32], BOOT_OLD)
            .expect("fill");
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
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store
        .finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR"))
        .expect("finish");
    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(failed.reason_code.as_deref(), Some("OS_ERROR"));

    // 同版本不同内容 -> Conflict；更低版本 -> StaleVersion
    let mut conflicting = failed.clone();
    conflicting.reason_code = Some("JOURNAL_LOST".to_string());
    assert_eq!(
        store.merge_evidence(&key, &conflicting),
        Ok(EvidenceMerge::Conflict(failed.clone()))
    );

    let mut stale = failed.clone();
    stale.record_version = failed.record_version.saturating_sub(1);
    assert_eq!(
        store.merge_evidence(&key, &stale),
        Ok(EvidenceMerge::StaleVersion(failed.clone()))
    );

    let mut newer = failed.clone();
    newer.record_version = failed.record_version + 1;
    newer.reason_code = Some("JOURNAL_LOST".to_string());
    assert_eq!(
        store.merge_evidence(&key, &newer),
        Ok(EvidenceMerge::Applied(newer))
    );
    assert_eq!(
        store
            .get(&key, SUB_TASK)
            .expect("get")
            .unwrap()
            .reason_code
            .as_deref(),
        Some("JOURNAL_LOST")
    );
}

#[test]
fn merge_rejects_higher_version_from_foreign_journal_epoch() {
    let dir = TempDir::new("merge_epoch");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    let stored = store.get(&key, SUB_TASK).expect("get").expect("record");
    // 跨 epoch 的高版本记录不可直接比较，更不得采用（03 规格）
    let mut foreign = stored.clone();
    foreign.journal_epoch = "00000000-0000-4000-8000-000000000000".to_string();
    foreign.record_version = stored.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &foreign),
        Ok(EvidenceMerge::Conflict(stored.clone()))
    );
    assert_eq!(
        store.get(&key, SUB_TASK).expect("get").unwrap(),
        stored,
        "原记录不得被修改"
    );
}

#[test]
fn merge_never_reverts_confirmed_terminal_state() {
    let dir = TempDir::new("merge_terminal");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store
        .finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR"))
        .expect("finish");
    // 更高版本的 Attempted 绝不允许逆转已确认的 Failed/OS_ERROR 终态（04 规格）
    let mut higher_attempted = failed.clone();
    higher_attempted.state = TaskState::Attempted;
    higher_attempted.reason_code = None;
    higher_attempted.record_version = failed.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &higher_attempted),
        Ok(EvidenceMerge::Conflict(failed.clone()))
    );
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(after.state, TaskState::Failed, "终态不得被回退为中间态");
    assert_eq!(after.reason_code.as_deref(), Some("OS_ERROR"));
}

#[test]
fn merge_rejects_contradictory_terminal_state_with_higher_version() {
    let dir = TempDir::new("merge_contradiction");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store
        .finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR"))
        .expect("finish");
    // TR-R9-1：更高版本 Succeeded（即便携带完整 boot 证据）不得静默覆盖已确认 Failed/OS_ERROR（04 规格 L119–124）
    let mut succeeded = failed.clone();
    succeeded.state = TaskState::Succeeded;
    succeeded.reason_code = None;
    succeeded.observed_boot_id = Some(BOOT_NEW.to_string());
    succeeded.evidence_kind = Some("boot_transition_observed".to_string());
    succeeded.record_version = failed.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &succeeded),
        Ok(EvidenceMerge::Conflict(failed.clone()))
    );
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(
        after.state,
        TaskState::Failed,
        "矛盾终态不得覆盖已确认失败记录"
    );
    assert_eq!(after.reason_code.as_deref(), Some("OS_ERROR"));
    assert_eq!(
        after.record_version, failed.record_version,
        "零写盘，原记录保持不变"
    );
}

#[test]
fn merge_rejects_terminal_state_transition_to_unknown_with_higher_version() {
    let dir = TempDir::new("merge_terminal_to_unknown");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let failed = store
        .finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR"))
        .expect("finish");
    // TR-R10-1：更高版本 Unknown（RESULT_TIMEOUT）不得中继抹除已确认 Failed/OS_ERROR（04 规格 L69–70，
    // 封死 Failed(v3)→Unknown(v4)→Succeeded(v5) 两步绕过：Unknown 不得使 stored 脱离终态全封闭守卫）
    let mut unknown = failed.clone();
    unknown.state = TaskState::Unknown;
    unknown.reason_code = Some("RESULT_TIMEOUT".to_string());
    unknown.record_version = failed.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &unknown),
        Ok(EvidenceMerge::Conflict(failed.clone()))
    );
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(
        after.state,
        TaskState::Failed,
        "已确认终态不得退化为 Unknown"
    );
    assert_eq!(after.reason_code.as_deref(), Some("OS_ERROR"));
    assert_eq!(
        after.record_version, failed.record_version,
        "零写盘，原记录保持不变"
    );
}

#[test]
fn merge_rejects_succeeded_without_boot_evidence() {
    let dir = TempDir::new("merge_no_evidence");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    let attempted = store.mark_attempted(&key, SUB_TASK).expect("attempted");
    // TR-R9-1：incoming Succeeded 缺少 boot_transition_observed 证据绝不允许应用
    let mut no_evidence = attempted.clone();
    no_evidence.state = TaskState::Succeeded;
    no_evidence.record_version = attempted.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &no_evidence),
        Ok(EvidenceMerge::Conflict(attempted.clone()))
    );
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(
        after.state,
        TaskState::Attempted,
        "缺证据的 Succeeded 不得被应用"
    );
    assert_eq!(
        after.record_version, attempted.record_version,
        "零写盘，原记录保持不变"
    );
}

/// `finish_succeeded_with_boot` 是本任务唯一允许写入 `Succeeded` 的入口：boot 证据由签名强制，
/// 且必须已有可靠 `Attempted`（04 §6 成功条件 2）、观察到的 boot 必须与 `pre_boot_id` 不同。
#[test]
fn finish_succeeded_with_boot_writes_evidence_and_closes_terminal_state() {
    let dir = TempDir::new("finish-boot");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    // 尚无可靠 attempted 记录：不得直接成功
    assert_eq!(
        store.finish_succeeded_with_boot(&key, SUB_TASK, BOOT_NEW),
        Err(JournalError::Conflict)
    );
    let attempted = store.mark_attempted(&key, SUB_TASK).expect("attempted");
    // 同一 boot 不是成功证据
    assert_eq!(
        store.finish_succeeded_with_boot(&key, SUB_TASK, BOOT_OLD),
        Err(JournalError::Conflict)
    );
    let succeeded = store
        .finish_succeeded_with_boot(&key, SUB_TASK, BOOT_NEW)
        .expect("succeeded with boot");
    assert_eq!(succeeded.state, TaskState::Succeeded);
    assert_eq!(succeeded.observed_boot_id.as_deref(), Some(BOOT_NEW));
    assert_eq!(
        succeeded.evidence_kind.as_deref(),
        Some("boot_transition_observed")
    );
    assert_eq!(succeeded.record_version, attempted.record_version + 1);
    // 终态全封闭：任何后续跃迁一律 Conflict，版本不动
    assert_eq!(
        store.mark_attempted(&key, SUB_TASK),
        Err(JournalError::Conflict)
    );
    assert_eq!(
        store.finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR")),
        Err(JournalError::Conflict)
    );
    drop(store);
    // 携带完整证据的成功记录可通过持久校验并原样恢复
    let reopened = open_store(&dir);
    let record = reopened.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(record, succeeded);
}

/// 非法跃迁与非法 reason 必须在写盘之前拒绝：磁盘字节与版本绝对不动。
#[test]
fn finish_rejects_illegal_transitions_and_reasons_without_touching_disk() {
    let dir = TempDir::new("finish-guards");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    // Accepted 不是 Attempted：必须先可靠 attempted 才允许落终态
    assert_eq!(
        store.finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR")),
        Err(JournalError::Conflict)
    );
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let logs_path = dir.path().join(JournalStore::<RealFs>::LOG_FILE);
    let logs_before = std::fs::read(&logs_path).expect("read logs");
    // Succeeded 只能经 finish_succeeded_with_boot
    assert_eq!(
        store.finish(&key, SUB_TASK, TaskState::Succeeded, None),
        Err(JournalError::Conflict)
    );
    // 词表外 reason 在触碰文件系统之前拒绝
    assert_eq!(
        store.finish(&key, SUB_TASK, TaskState::Failed, Some("NOT_A_REASON")),
        Err(JournalError::Decode("invalid reason code".to_string()))
    );
    // 未知去重键 → NotFound，绝不新建记录
    assert_eq!(
        store.mark_attempted(&key, "11111111-2222-4333-8444-555555555555"),
        Err(JournalError::NotFound)
    );
    assert_eq!(
        std::fs::read(&logs_path).expect("re-read logs"),
        logs_before,
        "被拒绝的跃迁必须零写盘"
    );
    assert_eq!(store.record_count(), 1);
    // 内存记录同样原样不动：仍是 Attempted(v2)
    let unchanged = store.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(unchanged.state, TaskState::Attempted);
    assert_eq!(unchanged.record_version, 2);
    assert_eq!(unchanged.reason_code, None);
}

/// 崩溃恢复不得改写已确认终态：boot 变了也不得把 `Failed/OS_ERROR` 误写成 `Succeeded`。
#[test]
fn confirmed_failed_terminal_state_survives_recover_with_changed_boot() {
    let dir = TempDir::new("terminal-recover");
    let key = [9u8; 32];
    {
        let store = open_store(&dir);
        store
            .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
            .expect("accepted");
        store.mark_attempted(&key, SUB_TASK).expect("attempted");
        let failed = store
            .finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR"))
            .expect("finish");
        assert_eq!(failed.state, TaskState::Failed);
        // 终态记录不得再被 boot 证据改写
        assert_eq!(
            store.finish_succeeded_with_boot(&key, SUB_TASK, BOOT_NEW),
            Err(JournalError::Conflict)
        );
    }
    let recovered = JournalStore::recover(dir.path(), RealFs, BOOT_NEW).expect("recover");
    let record = recovered.get(&key, SUB_TASK).expect("get").expect("record");
    assert_eq!(record.state, TaskState::Failed);
    assert_eq!(record.reason_code.as_deref(), Some("OS_ERROR"));
    assert_eq!(record.observed_boot_id, None);
    assert_eq!(record.evidence_kind, None);
}

#[test]
fn journal_errors_are_not_silently_swallowed() {
    // 目录不存在时不得假装成功
    let dir = TempDir::new("missing");
    let missing = dir.path().join("nope");
    assert!(JournalStore::open(&missing, RealFs).is_err());
    // JournalError 必须区分「确未提交」与「提交结果不明」
    assert_ne!(
        JournalError::DefinitelyNotCommitted,
        JournalError::CommitUnknown
    );
}

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
    assert_eq!(
        reopened.journal_epoch(),
        epoch,
        "normal reopen keeps the epoch"
    );
    drop(reopened);
    assert!(
        meta_path.exists() && logs_path.exists(),
        "reopen must not remove files"
    );

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
    std::fs::write(
        &meta_path,
        r#"{"v":99,"epoch":"6f8b1c2d-0000-4000-8000-000000000001"}"#,
    )
    .expect("write meta");
    std::fs::write(
        &logs_path,
        r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[]}"#,
    )
    .expect("write logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on bad schema version");
    assert!(
        matches!(err, JournalError::Decode(ref msg) if msg == "unsupported journal schema version")
    );

    // 2. 记录内层 epoch 与外层 meta.epoch 不一致返回 Decode 拒绝
    std::fs::write(
        &meta_path,
        r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001"}"#,
    )
    .expect("write meta");
    let bad_epoch_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000002","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_epoch_record).expect("write bad epoch logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on record epoch mismatch");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "record epoch mismatch"));

    // 3. 记录含未知 state 返回 Decode 拒绝
    let bad_state_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"InvalidState","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_state_record).expect("write bad state logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on bad state");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "journal.state_format"));

    // 4. 重复 (controller, sub_task_id) 键返回 Decode 拒绝
    let dup_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}},{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":2,"state":"Attempted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, dup_record).expect("write dup logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on duplicate key");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "journal.duplicate_record"));

    // 5. 内层 record.sub_task_id 与外层 sub_task_id 不一致 → Decode 拒绝（TR-R6-2）
    let inner_mismatch_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"11111111-2222-4333-8444-555555555555","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, inner_mismatch_record).expect("write inner mismatch logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on inner sub_task_id mismatch");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "sub_task_id mismatch"));

    // 6. payload_hash_hex 为 uppercase（非 64 小写 hex）→ Decode 拒绝（TR-R6-2）
    let bad_hash_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_hash_record).expect("write bad hash logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on uppercase payload hash");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "journal.hash_format"));

    // 7. pre_boot_id 非 canonical UUIDv4 → Decode 拒绝（TR-R6-2）
    let bad_boot_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Accepted","pre_boot_id":"not-a-uuid","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, bad_boot_record).expect("write bad boot logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on non-UUID pre_boot_id");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "invalid pre_boot_id format"));

    // 8. evidence_kind 自称 boot_transition_observed 但缺 observed_boot_id → Decode 拒绝（TR-R6-2）
    let bad_evidence_record = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Attempted","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":"boot_transition_observed"}}]}"#;
    std::fs::write(&logs_path, bad_evidence_record).expect("write bad evidence logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on missing observed boot evidence");
    assert!(matches!(err, JournalError::Decode(ref msg) if msg == "boot evidence incomplete"));

    // 9. TR-R8-1：state = Succeeded 且完全无 boot transition 证据 → 联合守卫 Decode 拒绝
    let succeeded_no_evidence = r#"{"v":1,"epoch":"6f8b1c2d-0000-4000-8000-000000000001","records":[{"controller_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","payload_hash_hex":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","record":{"schema_version":1,"sub_task_id":"9c858901-8a57-4791-81fe-4c455b099bc9","journal_epoch":"6f8b1c2d-0000-4000-8000-000000000001","record_version":1,"state":"Succeeded","pre_boot_id":"6f8b1c2d-0000-4000-8000-000000000001","observed_boot_id":null,"reason_code":null,"updated_unix_ms":null,"evidence_kind":null}}]}"#;
    std::fs::write(&logs_path, succeeded_no_evidence)
        .expect("write succeeded-without-evidence logs");
    let err = JournalStore::open(dir.path(), RealFs)
        .err()
        .expect("must fail on Succeeded without boot evidence");
    assert!(
        matches!(err, JournalError::Decode(ref msg) if msg == "succeeded record must have valid boot transition evidence")
    );
}

/// Task 3.1 修复负例：`insert_accepted` 新插入分支必须在触碰文件系统之前校验
/// `sub_task_id` 与 `pre_boot_id` 的规范 UUIDv4（与 `validate_loaded_journal` 同标准），
/// 否则非法记录落盘后会让下次 open 判定整个 journal 损坏、破坏既有有效记录的恢复。
#[test]
fn insert_accepted_rejects_non_canonical_uuid_v4_before_touching_disk() {
    let dir = TempDir::new("uuid-validate");
    let key = [9u8; 32];
    let store = open_store(&dir);
    // 先落 1 条有效记录，作为「既有有效记录不受影响」的见证
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    let logs_path = dir.path().join(JournalStore::<RealFs>::LOG_FILE);
    let logs_before = std::fs::read(&logs_path).expect("read logs");

    // 非法 sub_task_id：非 UUID、大写、版本位非 4、variant 位非 8/9/a/b、空串
    let bad_sub_task_ids = [
        "not-a-uuid",
        "9C858901-8A57-4791-81FE-4C455B099BC9",
        "9c858901-8a57-5791-81fe-4c455b099bc9",
        "9c858901-8a57-4791-01fe-4c455b099bc9",
        "",
    ];
    for bad in bad_sub_task_ids {
        assert_eq!(
            store.insert_accepted(&key, bad, &[1u8; 32], BOOT_OLD),
            Err(JournalError::Decode(
                "invalid sub_task_id format".to_string()
            )),
            "must reject invalid sub_task_id: {bad}"
        );
    }

    // 非法 pre_boot_id：sub_task_id 用有效且未占用的 UUID，避免先命中去重分支
    let unused_sub_task = "11111111-2222-4333-8444-555555555555";
    let bad_pre_boot_ids = [
        "not-a-uuid",
        "6F8B1C2D-0000-4000-8000-000000000001",
        "6f8b1c2d-0000-4000-8000-00000000000",
    ];
    for bad in bad_pre_boot_ids {
        assert_eq!(
            store.insert_accepted(&key, unused_sub_task, &[0u8; 32], bad),
            Err(JournalError::Decode(
                "invalid pre_boot_id format".to_string()
            )),
            "must reject invalid pre_boot_id: {bad}"
        );
    }

    // 全部拒绝路径零写盘：LOG_FILE 字节不变、内存记录数不变
    assert_eq!(
        std::fs::read(&logs_path).expect("re-read logs"),
        logs_before,
        "rejected inserts must not touch disk"
    );
    assert_eq!(store.record_count(), 1);
    drop(store);

    // 既有有效记录不受影响：重开正常、记录仍在
    let reopened = open_store(&dir);
    assert_eq!(reopened.record_count(), 1);
    let reread = reopened
        .get(&key, SUB_TASK)
        .expect("get")
        .expect("valid record must survive reopen");
    assert_eq!(reread.state, TaskState::Accepted);
    assert_eq!(reread.pre_boot_id, BOOT_OLD);
}

/// TR-R11-1（Critical）：`pre_boot_id` 是 `accepted` 时刻即固化的**意图基线**，
/// 任何入站记录都不得改写。否则入站 `Attempted(v3, pre_boot_id=B)` 覆盖原 `pre_boot_id=A` 后，
/// `apply_boot_transition_evidence(A)` 会因 `B != A` 误判发生重启并伪造 `Succeeded`。
#[test]
fn merge_rejects_higher_version_with_different_pre_boot_id() {
    let dir = TempDir::new("merge_pre_boot");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    let attempted = store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let logs_path = dir.path().join(JournalStore::<RealFs>::LOG_FILE);
    let logs_before = std::fs::read(&logs_path).expect("read logs");

    // 入站：同任务、同 epoch、同状态、形状合法，仅 pre_boot_id 换成另一 boot 代际且版本更高
    let mut forged = attempted.clone();
    forged.pre_boot_id = BOOT_NEW.to_string();
    forged.record_version = attempted.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &forged),
        Ok(EvidenceMerge::Conflict(attempted.clone())),
        "不同 pre_boot_id 的高版本记录绝不允许替换"
    );
    assert_eq!(
        store.get(&key, SUB_TASK).expect("get").unwrap(),
        attempted,
        "内存原记录不得被改写"
    );
    assert_eq!(
        std::fs::read(&logs_path).expect("re-read logs"),
        logs_before,
        "被拒绝的合并必须零写盘"
    );

    // 攻击链闭合：意图基线仍是 BOOT_OLD，同 boot 恢复不构成重启证据，绝不伪造 Succeeded
    assert!(
        store
            .apply_boot_transition_evidence(BOOT_OLD)
            .expect("apply")
            .is_empty(),
        "同 boot 不得写入成功证据"
    );
    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(after.state, TaskState::Attempted);
    assert_eq!(after.pre_boot_id, BOOT_OLD);
    assert_eq!(after.observed_boot_id, None);
    assert_eq!(after.evidence_kind, None);
    assert_eq!(after.record_version, attempted.record_version);
    assert_eq!(
        std::fs::read(&logs_path).expect("re-read logs"),
        logs_before,
        "整条攻击链零写盘"
    );
}

/// TR-R11-1：已固化 `Succeeded` 的 `observed_boot_id` 与 `evidence_kind` 同样不得被冲突数据替换。
/// 状态本身相同（`Succeeded`→`Succeeded`）时终态全封闭守卫不触发，必须由证据字段守卫兜住。
#[test]
fn merge_never_replaces_frozen_boot_evidence_of_succeeded_record() {
    const BOOT_THIRD: &str = "6f8b1c2d-0000-4000-8000-000000000003";
    let dir = TempDir::new("merge_frozen_evidence");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    store.mark_attempted(&key, SUB_TASK).expect("attempted");
    let succeeded = store
        .finish_succeeded_with_boot(&key, SUB_TASK, BOOT_NEW)
        .expect("succeeded");
    let logs_path = dir.path().join(JournalStore::<RealFs>::LOG_FILE);
    let logs_before = std::fs::read(&logs_path).expect("read logs");

    // 同状态、更高版本、证据本身合法，但观察代际不同：不得替换已落盘的 observed_boot_id
    let mut rival = succeeded.clone();
    rival.observed_boot_id = Some(BOOT_THIRD.to_string());
    rival.record_version = succeeded.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &rival),
        Ok(EvidenceMerge::Conflict(succeeded.clone())),
        "已固化的 observed_boot_id 不得被替换"
    );
    // 同状态、更高版本、抹掉 evidence_kind：同样不得替换已固化的证据种类
    let mut stripped = succeeded.clone();
    stripped.evidence_kind = None;
    stripped.record_version = succeeded.record_version + 1;
    assert_eq!(
        store.merge_evidence(&key, &stripped),
        Ok(EvidenceMerge::Conflict(succeeded.clone())),
        "已固化的 evidence_kind 不得被抹除"
    );

    let after = store.get(&key, SUB_TASK).expect("get").unwrap();
    assert_eq!(after, succeeded, "终态记录整体原样不动");
    assert_eq!(after.observed_boot_id.as_deref(), Some(BOOT_NEW));
    assert_eq!(
        after.evidence_kind.as_deref(),
        Some("boot_transition_observed")
    );
    assert_eq!(
        std::fs::read(&logs_path).expect("re-read logs"),
        logs_before,
        "被拒绝的合并必须零写盘"
    );
}

/// 毒锁测试缝（`test-seam`）：`records` 互斥锁被毒化后，全部读写入口必须 fail-closed
/// 返回 `JournalError::Io`，绝不 panic、不吞毒继续返回旧内存快照；毒化仅限本实例，
/// 磁盘完好，重开（新实例、新锁）后记录仍可正常读取。
#[test]
#[cfg(feature = "test-seam")]
fn poisoned_records_mutex_fails_closed_with_io() {
    let dir = TempDir::new("poison-records");
    let key = [9u8; 32];
    let store = open_store(&dir);
    store
        .insert_accepted(&key, SUB_TASK, &[0u8; 32], BOOT_OLD)
        .expect("accepted");
    let stored = store.get(&key, SUB_TASK).expect("get").expect("record");
    let logs_path = dir.path().join(JournalStore::<RealFs>::LOG_FILE);
    let logs_before = std::fs::read(&logs_path).expect("read logs");

    store.poison_records_for_test();

    let err = store
        .get(&key, SUB_TASK)
        .expect_err("poisoned records mutex must fail closed");
    assert!(
        matches!(err, JournalError::Io(ref msg) if msg == "journal.records poisoned"),
        "毒锁必须返回固定去敏 Io，实际 {err:?}"
    );
    // 其余入口同样在加锁处 fail-closed：不 panic、不落盘
    let unused_sub_task = "11111111-2222-4333-8444-555555555555";
    let hash_err = store
        .payload_hash(&key, SUB_TASK)
        .expect_err("payload_hash must fail closed on poisoned records mutex");
    assert!(
        matches!(hash_err, JournalError::Io(ref msg) if msg == "journal.records poisoned"),
        "payload_hash 毒锁必须返回固定去敏 Io，实际 {hash_err:?}"
    );
    assert!(matches!(
        store.payload_hash(&key, unused_sub_task),
        Err(JournalError::Io(_))
    ));
    assert!(matches!(
        store.insert_accepted(&key, unused_sub_task, &[0u8; 32], BOOT_OLD),
        Err(JournalError::Io(_))
    ));
    assert!(matches!(
        store.mark_attempted(&key, SUB_TASK),
        Err(JournalError::Io(_))
    ));
    assert!(matches!(
        store.finish(&key, SUB_TASK, TaskState::Failed, Some("OS_ERROR")),
        Err(JournalError::Io(_))
    ));
    assert!(matches!(
        store.merge_evidence(&key, &stored),
        Err(JournalError::Io(_))
    ));
    assert!(matches!(
        store.apply_boot_transition_evidence(BOOT_NEW),
        Err(JournalError::Io(_))
    ));
    assert_eq!(
        std::fs::read(&logs_path).expect("re-read logs"),
        logs_before,
        "毒锁路径必须零写盘"
    );

    // 毒化只影响本实例的锁：磁盘完好，重开后记录仍可读取
    drop(store);
    let reopened = open_store(&dir);
    let reread = reopened
        .get(&key, SUB_TASK)
        .expect("get")
        .expect("record survives poison of a different instance");
    assert_eq!(reread, stored);
    assert_eq!(
        reopened.payload_hash(&key, SUB_TASK).expect("payload hash"),
        Some([0u8; 32])
    );
}

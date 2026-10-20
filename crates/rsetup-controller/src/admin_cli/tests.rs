//! `admin_cli` 行为测试：门禁、事务编排、CAS 失败关闭、审计脱敏与写出时机。
//!
//! 说明：内存桩只承担**编排语义**验证（真实 DB 写路径仍需本机 PTY + 隔离真实 DB 验收，
//! 不以桩冒充）。桩刻意模拟 InnoDB 原子性：事务内的写先进 draft，只有 `commit` 才落到
//! committed，`rollback` 丢弃 draft，因此“失败路径不留半成品”是可断言的行为而非源码字符串检查。

use std::sync::{Arc, Mutex};

use argon2::{Argon2, PasswordHash, PasswordVerifier as _};
use serde_json::Value;

use super::*;

// ---------------------------------------------------------------- 测试桩

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FailPoint {
    Guard,
    LockAdmin,
    ReplacePassword,
    RevokeSessions,
    BumpEpoch,
    RecordAudit,
    Commit,
}

/// 已提交行的可见状态（模拟表数据）。
#[derive(Clone, Debug, Default)]
struct RowState {
    admin: Option<AdminAccountRow>,
    password_hash: String,
    must_change_password: bool,
    live_sessions: u64,
    authz_epoch: u64,
    audit: Vec<ResetAuditEvent>,
}

#[derive(Clone, Debug, Default)]
struct MemoryState {
    committed: RowState,
    /// 事务内暂存：`None` 表示未开启事务；`commit` 才覆盖 committed。
    draft: Option<RowState>,
    calls: Vec<String>,
    tx_opened: bool,
    guard_locked: bool,
    committed_tx: bool,
    rolled_back: bool,
    fail: Option<FailPoint>,
    /// 模拟“锁定行之后另一写者先提交”：让 CAS 的 expected_revision 失配。
    racing_commit: bool,
}

impl MemoryState {
    fn row(&self) -> &RowState {
        self.draft.as_ref().unwrap_or(&self.committed)
    }
}

#[derive(Clone, Debug, Default)]
struct SharedMemory(Arc<Mutex<MemoryState>>);

impl SharedMemory {
    fn snapshot(&self) -> MemoryState {
        self.0
            .lock()
            .expect("memory store lock not poisoned")
            .clone()
    }
}

#[derive(Clone, Debug, Default)]
struct MemoryStore {
    shared: SharedMemory,
}

impl MemoryStore {
    fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> MemoryState {
        self.shared.snapshot()
    }

    fn fail_at(&self, point: FailPoint) {
        self.shared.0.lock().unwrap().fail = Some(point);
    }

    fn with_racing_commit(&self) {
        self.shared.0.lock().unwrap().racing_commit = true;
    }

    fn seed(&self, admin: AdminAccountRow, password_hash: &str, live_sessions: u64, epoch: u64) {
        let mut state = self.shared.0.lock().unwrap();
        state.committed.admin = Some(admin);
        state.committed.password_hash = password_hash.into();
        state.committed.live_sessions = live_sessions;
        state.committed.authz_epoch = epoch;
    }
}

macro_rules! record_call {
    ($state:expr, $name:literal) => {
        $state.calls.push($name.to_owned())
    };
}

impl EmergencyResetStore for MemoryStore {
    fn acquire_guard(&mut self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "acquire_guard");
            if state.fail == Some(FailPoint::Guard) {
                return Err(ControllerError::Database(sqlx::Error::RowNotFound));
            }
            if state.tx_opened {
                return Err(ControllerError::InvalidState);
            }
            state.tx_opened = true;
            state.guard_locked = true;
            state.draft = Some(state.committed.clone());
            Ok(())
        })
    }

    fn lock_admin(&mut self) -> StoreFuture<'_, Option<AdminAccountRow>> {
        Box::pin(async move {
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "lock_admin");
            if state.fail == Some(FailPoint::LockAdmin) {
                return Err(ControllerError::Database(sqlx::Error::RowNotFound));
            }
            Ok(state.row().admin)
        })
    }

    fn replace_password<'a>(
        &'a mut self,
        admin_id: [u8; 16],
        password_hash: &'a str,
        expected_revision: u64,
        next_revision: u64,
    ) -> StoreFuture<'a, bool> {
        Box::pin(async move {
            // 与真实实现同一道空哈希门禁。
            verify_password_hash_present(password_hash)?;
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "replace_password");
            if state.fail == Some(FailPoint::ReplacePassword) {
                return Err(ControllerError::Database(sqlx::Error::RowNotFound));
            }
            // 模拟“锁定后另一写者先提交”：CAS 的 expected_revision 因此失配。
            if state.racing_commit {
                if let Some(admin) = state.draft.as_mut().and_then(|row| row.admin.as_mut()) {
                    admin.revision += 1;
                }
            }
            let draft = state.draft.as_mut().expect("guard acquired first");
            // 真实 CAS 语义：WHERE id = ? AND revision = ? 未命中即 0 行。
            let matched = draft
                .admin
                .as_ref()
                .is_some_and(|admin| admin.id == admin_id && admin.revision == expected_revision);
            if !matched {
                return Ok(false);
            }
            let admin = draft.admin.as_mut().expect("matched admin row");
            admin.revision = next_revision;
            draft.password_hash = password_hash.to_owned();
            draft.must_change_password = true;
            Ok(true)
        })
    }

    fn revoke_sessions(&mut self, admin_id: [u8; 16]) -> StoreFuture<'_, u64> {
        Box::pin(async move {
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "revoke_sessions");
            if state.fail == Some(FailPoint::RevokeSessions) {
                return Err(ControllerError::Database(sqlx::Error::RowNotFound));
            }
            let draft = state.draft.as_mut().expect("guard acquired first");
            let revoked = if draft.admin.is_some() {
                draft.live_sessions
            } else {
                let _ = admin_id;
                0
            };
            draft.live_sessions = 0;
            Ok(revoked)
        })
    }

    fn bump_authz_epoch(&mut self) -> StoreFuture<'_, u64> {
        Box::pin(async move {
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "bump_authz_epoch");
            if state.fail == Some(FailPoint::BumpEpoch) {
                return Err(ControllerError::RevisionConflict);
            }
            let draft = state.draft.as_mut().expect("guard acquired first");
            let next = draft
                .authz_epoch
                .checked_add(1)
                .ok_or(ControllerError::RevisionConflict)?;
            draft.authz_epoch = next;
            Ok(next)
        })
    }

    fn record_audit<'a>(&'a mut self, event: &'a ResetAuditEvent) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "record_audit");
            if state.fail == Some(FailPoint::RecordAudit) {
                return Err(ControllerError::Database(sqlx::Error::RowNotFound));
            }
            state
                .draft
                .as_mut()
                .expect("guard acquired first")
                .audit
                .push(event.clone());
            Ok(())
        })
    }

    fn commit(&mut self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "commit");
            if state.fail == Some(FailPoint::Commit) {
                // 结果未知：桩刻意既不落 draft 也不回滚，调用方不得声称已回滚。
                return Err(ControllerError::Database(sqlx::Error::RowNotFound));
            }
            let draft = state.draft.take().expect("guard acquired first");
            state.committed = draft;
            state.committed_tx = true;
            Ok(())
        })
    }

    fn rollback(&mut self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let mut state = self.shared.0.lock().unwrap();
            record_call!(state, "rollback");
            state.draft = None;
            state.rolled_back = true;
            Ok(())
        })
    }
}

#[derive(Clone, Debug, Default)]
struct SinkLog {
    emitted: usize,
    secrets: Vec<String>,
    written: String,
}

#[derive(Clone, Debug, Default)]
struct RecordingSink {
    log: Arc<Mutex<SinkLog>>,
    fail_write: bool,
}

impl RecordingSink {
    fn new() -> Self {
        Self::default()
    }

    fn log(&self) -> SinkLog {
        self.log.lock().unwrap().clone()
    }
}

impl CredentialSink for RecordingSink {
    fn emit(&mut self, credential: &TemporaryCredential) -> Result<(), ControllerError> {
        let mut log = self.log.lock().unwrap();
        log.emitted += 1;
        if self.fail_write {
            return Err(ControllerError::Config(ERR_SINK_WRITE.into()));
        }
        log.secrets.push(credential.expose().to_owned());
        log.written.push_str(&render_credential_text(credential));
        Ok(())
    }
}

fn admin_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    id[0] = 0xde;
    id[15] = 0x2f;
    id
}

fn admin_row(active: bool, is_admin: bool, revision: u64) -> AdminAccountRow {
    AdminAccountRow {
        id: admin_id(),
        revision,
        active,
        is_admin,
    }
}

fn root_preconditions() -> ResetPreconditions {
    ResetPreconditions {
        real_uid: 0,
        effective_uid: 0,
        stdin_interactive: true,
        stdout_interactive: true,
        stderr_interactive: true,
        controlling_tty_held: true,
    }
}

fn seeded_store() -> MemoryStore {
    let store = MemoryStore::new();
    store.seed(admin_row(true, true, 7), "old-argon2id-digest", 2, 41);
    store
}

/// 成功路径：返回捕获到的明文凭据（只用于断言）。
async fn run_success(store: &mut MemoryStore) -> (SinkLog, EmergencyResetOutcome) {
    let sink = RecordingSink::new();
    let outcome = run_emergency_reset(store, &root_preconditions(), sink.clone())
        .await
        .expect("root + TTY + healthy store must succeed");
    (sink.log(), outcome)
}

// ---------------------------------------------------------------- 门禁

#[test]
fn root_gate_requires_both_os_uids_to_be_zero() {
    assert!(verify_root_credentials(0, 0).is_ok());
    for (real, effective) in [(1000u32, 1000), (1000, 0), (0, 1000), (1, 0), (0, 1)] {
        let error =
            verify_root_credentials(real, effective).expect_err("non-root UID must be rejected");
        assert!(matches!(error, ControllerError::Config(_)));
        // 错误文案固定，不回显 uid，也不接受任何环境变量声称。
        assert_eq!(
            error.to_string(),
            format!("configuration: {ERR_ROOT_REQUIRED}")
        );
        assert!(!error.to_string().contains(&real.to_string()));
        assert!(!error.to_string().contains(&effective.to_string()));
    }
}

#[test]
fn interactive_gate_requires_every_standard_stream_to_be_a_tty() {
    assert!(verify_interactive_tty(true, true, true).is_ok());
    for (stdin, stdout, stderr) in [
        (false, true, true),
        (true, false, true),
        (true, true, false),
        (false, false, false),
    ] {
        let error = verify_interactive_tty(stdin, stdout, stderr)
            .expect_err("redirected or missing stream must be rejected");
        assert_eq!(
            error.to_string(),
            format!("configuration: {ERR_INTERACTIVE_REQUIRED}")
        );
    }
}

#[test]
fn controlling_tty_handle_must_be_held_before_any_write() {
    assert!(verify_controlling_tty_held(true).is_ok());
    let error =
        verify_controlling_tty_held(false).expect_err("missing /dev/tty handle must be rejected");
    assert_eq!(
        error.to_string(),
        format!("configuration: {ERR_TTY_REQUIRED}")
    );
}

#[test]
fn gate_order_is_root_then_streams_then_tty_handle() {
    let mut preconditions = root_preconditions();
    preconditions.real_uid = 0;
    preconditions.effective_uid = 1000;
    preconditions.stdin_interactive = false;
    preconditions.controlling_tty_held = false;
    // 先报 root，再报流，最后报 tty 句柄：顺序固定便于运维定位。
    assert_eq!(
        preconditions.verify().unwrap_err().to_string(),
        format!("configuration: {ERR_ROOT_REQUIRED}")
    );
    preconditions.effective_uid = 0;
    assert_eq!(
        preconditions.verify().unwrap_err().to_string(),
        format!("configuration: {ERR_INTERACTIVE_REQUIRED}")
    );
    preconditions.stdin_interactive = true;
    preconditions.stderr_interactive = false;
    assert_eq!(
        preconditions.verify().unwrap_err().to_string(),
        format!("configuration: {ERR_INTERACTIVE_REQUIRED}")
    );
    preconditions.stderr_interactive = true;
    preconditions.controlling_tty_held = false;
    assert_eq!(
        preconditions.verify().unwrap_err().to_string(),
        format!("configuration: {ERR_TTY_REQUIRED}")
    );
}

#[test]
fn os_snapshot_reports_missing_tty_handle_and_never_passes_gate() {
    // 无论测试进程以何 UID、是否有控制终端运行，缺失 `/dev/tty` 句柄都必须拒绝。
    let without_handle = collect_preconditions(false);
    assert!(!without_handle.controlling_tty_held);
    assert!(without_handle.verify().is_err());
    // UID 只来自 OS 调用，与环境变量无关。
    let (real, effective) = unsafe { (libc::getuid(), libc::geteuid()) };
    let snapshot = collect_preconditions(true);
    assert_eq!(snapshot.real_uid, real);
    assert_eq!(snapshot.effective_uid, effective);
    assert_eq!(
        snapshot.verify().is_ok(),
        real == 0
            && effective == 0
            && snapshot.stdin_interactive
            && snapshot.stdout_interactive
            && snapshot.stderr_interactive
    );
}

#[tokio::test]
async fn all_gates_are_checked_before_any_database_access() {
    let cases = [
        ResetPreconditions {
            real_uid: 0,
            effective_uid: 1000,
            stdin_interactive: true,
            stdout_interactive: true,
            stderr_interactive: true,
            controlling_tty_held: true,
        },
        ResetPreconditions {
            real_uid: 0,
            effective_uid: 0,
            stdin_interactive: true,
            stdout_interactive: false,
            stderr_interactive: true,
            controlling_tty_held: true,
        },
        ResetPreconditions {
            real_uid: 0,
            effective_uid: 0,
            stdin_interactive: true,
            stdout_interactive: true,
            stderr_interactive: true,
            controlling_tty_held: false,
        },
    ];
    for preconditions in cases {
        let mut store = seeded_store();
        let sink = RecordingSink::new();
        let error = run_emergency_reset(&mut store, &preconditions, sink.clone())
            .await
            .expect_err("failing gate must reject");
        assert!(!error.to_string().is_empty());
        // 零副作用：不开事务、不写库、不输出凭据。
        let state = store.snapshot();
        assert!(
            state.calls.is_empty(),
            "store must not be touched: {:?}",
            state.calls
        );
        assert!(!state.tx_opened);
        assert!(!state.committed_tx);
        assert_eq!(state.committed.password_hash, "old-argon2id-digest");
        assert_eq!(state.committed.authz_epoch, 41);
        assert_eq!(sink.log().emitted, 0);
    }
}

// ---------------------------------------------------------------- 凭据与哈希

#[test]
fn temporary_credential_is_hex_secret_with_argon2id_hash() {
    let (credential, hash) = generate_temporary_credential().expect("CSPRNG + argon2");
    let secret = credential.expose();
    assert_eq!(secret.len(), TEMPORARY_CREDENTIAL_BYTES * 2);
    assert!(
        secret
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "128-bit CSPRNG entropy rendered as lowercase hex"
    );
    // 复用既有 Argon2id 策略（m=65536 KiB, t=3, p=1）。
    let parsed = PasswordHash::new(&hash).expect("argon2 PHC string");
    assert_eq!(parsed.algorithm.as_str(), "argon2id");
    assert_eq!(parsed.params.get("m").unwrap().decimal().unwrap(), 65536);
    assert_eq!(parsed.params.get("t").unwrap().decimal().unwrap(), 3);
    assert_eq!(parsed.params.get("p").unwrap().decimal().unwrap(), 1);
    Argon2::default()
        .verify_password(secret.as_bytes(), &parsed)
        .expect("hash must verify the emitted credential");
    assert_ne!(secret, hash);
}

#[test]
fn credential_debug_and_display_never_leak_the_secret() {
    let (credential, _hash) = generate_temporary_credential().expect("credential");
    let secret = credential.expose().to_owned();
    let debug = format!("{credential:?}");
    let display = format!("{credential}");
    assert!(debug.contains("REDACTED") && !debug.contains(&secret));
    assert!(display.contains("REDACTED") && !display.contains(&secret));
}

#[test]
fn rendered_credential_text_shows_the_secret_exactly_once() {
    let credential = TemporaryCredential::from_test_secret("a1b2c3d4e5f60718293a4b5c6d7e8f90");
    let text = render_credential_text(&credential);
    assert_eq!(text.matches(credential.expose()).count(), 1);
    assert!(text.contains(BOOTSTRAP_ADMIN_USERNAME));
    assert!(text.contains("首次登录必须立即修改"));
    // 不含哈希、连接串或任何其它秘密形态。
    assert!(!text.contains("argon2"));
    assert!(!text.contains('$'));
    assert_eq!(render_credential_text(&credential), text);
}

#[test]
fn empty_password_hash_is_rejected_before_any_write() {
    assert!(verify_password_hash_present("$argon2id$v=19$m=65536").is_ok());
    assert!(matches!(
        verify_password_hash_present(""),
        Err(ControllerError::Crypto)
    ));
}

// ---------------------------------------------------------------- 审计载荷

#[test]
fn audit_event_is_redacted_and_has_no_actor_or_parameters() {
    let event = reset_audit_event(admin_id());
    assert_eq!(event.event_type, "auth.reset-admin");
    assert_eq!(event.actor_kind, "system");
    assert_eq!(event.actor_user_id, None, "本机 reset 无 HTTP actor");
    assert_eq!(event.target_kind, "user");
    assert_eq!(event.target_id, hex::encode(admin_id()));
    assert_eq!(event.outcome, "success");
    assert_eq!(
        serde_json::from_str::<Value>(event.params_redacted).expect("valid json"),
        Value::Object(serde_json::Map::new())
    );
    // 载荷可安全入库：只有 id hex，无密码/哈希形态。
    let rendered = format!("{event:?}");
    assert!(!rendered.contains("argon2"));
    assert!(!rendered.contains('$'));
}

// ---------------------------------------------------------------- 成功路径

#[tokio::test]
async fn successful_reset_updates_password_hash_must_change_and_authz_epoch() {
    let mut store = seeded_store();
    let (log, outcome) = run_success(&mut store).await;
    let state = store.snapshot();

    // 锁序与写序固定：guard → admin 行 → 账号 → 会话 → epoch → 审计 → commit。
    assert_eq!(
        state.calls,
        vec![
            "acquire_guard",
            "lock_admin",
            "replace_password",
            "revoke_sessions",
            "bump_authz_epoch",
            "record_audit",
            "commit",
        ]
    );
    assert!(state.guard_locked && state.committed_tx && !state.rolled_back);

    // 落库哈希必须能被**写出的那次**临时凭据验证（等价于密码真的被更新且可用）。
    assert_eq!(log.secrets.len(), 1, "一次性凭据只写出一次");
    let secret = log.secrets.first().expect("one emitted secret");
    assert!(
        PasswordHasher::new()
            .verify(secret, &state.committed.password_hash)
            .expect("argon2 verify")
    );
    assert_ne!(state.committed.password_hash, "old-argon2id-digest");
    assert!(state.committed.must_change_password, "强制首次登录改密");
    assert_eq!(state.committed.admin.expect("admin row").revision, 8);
    assert_eq!(state.committed.live_sessions, 0, "全部既有会话撤销");
    assert_eq!(state.committed.authz_epoch, 42, "同事务递增 authz_epoch");

    assert_eq!(outcome.admin_revision, 8);
    assert_eq!(outcome.authz_epoch, 42);
    assert_eq!(outcome.revoked_sessions, 2);

    // 凭据只进 sink 渲染文本，且只出现一次。
    assert_eq!(log.written.matches(secret.as_str()).count(), 1);
}

#[tokio::test]
async fn successful_reset_records_one_redacted_audit_event_in_the_same_transaction() {
    let mut store = seeded_store();
    let (log, _outcome) = run_success(&mut store).await;
    let state = store.snapshot();

    assert_eq!(state.committed.audit.len(), 1);
    let audit = state.committed.audit.first().expect("one audit row");
    assert_eq!(audit, &reset_audit_event(admin_id()));

    // 审计与业务写同一事务：审计先于 commit 记录，回滚即同时消失（见失败用例）。
    let audit_index = state
        .calls
        .iter()
        .position(|call| call == "record_audit")
        .expect("audit recorded");
    let commit_index = state
        .calls
        .iter()
        .position(|call| call == "commit")
        .expect("committed");
    assert!(audit_index < commit_index);
    assert_eq!(
        state.calls.iter().filter(|call| *call == "commit").count(),
        1
    );

    // 审计与调用日志都不含明文凭据。
    let secret = log.secrets.first().expect("emitted secret");
    assert!(!format!("{:?}", state.committed.audit).contains(secret));
    assert!(!state.calls.join(",").contains(secret));
}

// ---------------------------------------------------------------- 失败关闭

#[tokio::test]
async fn lost_cas_race_rolls_back_and_never_emits_a_credential() {
    let mut store = seeded_store();
    store.with_racing_commit();
    let sink = RecordingSink::new();
    let error = run_emergency_reset(&mut store, &root_preconditions(), sink.clone())
        .await
        .expect_err("CAS miss must fail closed");
    assert!(matches!(error, ControllerError::RevisionConflict));
    let state = store.snapshot();
    assert!(state.rolled_back && !state.committed_tx);
    assert_eq!(state.committed.password_hash, "old-argon2id-digest");
    assert_eq!(state.committed.admin.expect("admin row").revision, 7);
    assert_eq!(state.committed.live_sessions, 2, "不得撤销他人会话");
    assert_eq!(state.committed.authz_epoch, 41);
    assert!(state.committed.audit.is_empty());
    assert_eq!(sink.log().emitted, 0, "未提交绝不输出凭据");
    // 绝不 commit（也不重试图写）。
    assert!(!state.calls.iter().any(|call| call == "commit"));
}

#[tokio::test]
async fn epoch_and_audit_failures_roll_back_the_whole_transaction() {
    for point in [FailPoint::BumpEpoch, FailPoint::RecordAudit] {
        let mut store = seeded_store();
        store.fail_at(point);
        let sink = RecordingSink::new();
        let error = run_emergency_reset(&mut store, &root_preconditions(), sink.clone())
            .await
            .expect_err("mid-transaction failure must fail closed");
        let state = store.snapshot();
        assert!(!error.to_string().is_empty());
        assert!(state.rolled_back && !state.committed_tx);
        assert_eq!(state.committed.password_hash, "old-argon2id-digest");
        assert_eq!(state.committed.admin.expect("admin row").revision, 7);
        assert_eq!(state.committed.live_sessions, 2);
        assert_eq!(state.committed.authz_epoch, 41);
        assert!(
            state.committed.audit.is_empty(),
            "审计失败必须连业务一起回滚"
        );
        assert_eq!(sink.log().emitted, 0);
        assert!(!state.calls.iter().any(|call| call == "commit"));
    }
}

#[tokio::test]
async fn guard_and_read_failures_never_open_a_write() {
    for point in [FailPoint::Guard, FailPoint::LockAdmin] {
        let mut store = seeded_store();
        store.fail_at(point);
        let sink = RecordingSink::new();
        assert!(
            run_emergency_reset(&mut store, &root_preconditions(), sink.clone())
                .await
                .is_err()
        );
        let state = store.snapshot();
        assert_eq!(sink.log().emitted, 0);
        assert!(!state.committed_tx);
        assert_eq!(state.committed.password_hash, "old-argon2id-digest");
        assert_eq!(state.committed.authz_epoch, 41);
    }
}

#[tokio::test]
async fn commit_error_keeps_result_unknown_and_never_emits_or_claims_rollback() {
    let mut store = seeded_store();
    store.fail_at(FailPoint::Commit);
    let sink = RecordingSink::new();
    let error = run_emergency_reset(&mut store, &root_preconditions(), sink.clone())
        .await
        .expect_err("commit error means unknown outcome");
    let state = store.snapshot();
    // 规格：commit 返回错误即提交结果未知——不输出秘密，也不主动声称已回滚。
    assert_eq!(sink.log().emitted, 0);
    assert!(!state.rolled_back, "不得在结果未知时自行声称回滚");
    assert!(!state.committed_tx);
    assert_eq!(
        state.calls.iter().filter(|call| *call == "commit").count(),
        1,
        "commit 只发生一次，不重试"
    );
    assert_eq!(state.committed.password_hash, "old-argon2id-digest");
    assert!(!error.to_string().is_empty());
}

#[tokio::test]
async fn tty_write_failure_keeps_committed_reset_without_repeating_output() {
    let mut store = seeded_store();
    let sink = RecordingSink {
        fail_write: true,
        ..RecordingSink::new()
    };
    let error = run_emergency_reset(&mut store, &root_preconditions(), sink.clone())
        .await
        .expect_err("tty write failure must be reported");
    let state = store.snapshot();
    assert!(state.committed_tx, "已提交的重置保持有效");
    assert_ne!(state.committed.password_hash, "old-argon2id-digest");
    assert_eq!(sink.log().emitted, 1, "写失败不得重试图写或重显旧凭据");
    assert!(!state.calls.iter().any(|call| call == "rollback"));
    // 指引固定且不含任何秘密。
    assert_eq!(
        error.to_string(),
        format!("configuration: {ERR_SINK_WRITE}")
    );
    assert_eq!(sink.log().written, "");
}

#[tokio::test]
async fn inactive_or_non_admin_bootstrap_account_is_refused_without_reinitializing() {
    for row in [
        admin_row(false, true, 7),
        admin_row(true, false, 7),
        admin_row(false, false, 7),
    ] {
        let store = MemoryStore::new();
        store.seed(row, "existing-argon2id-digest", 3, 41);
        let mut store = store;
        let sink = RecordingSink::new();
        let error = run_emergency_reset(&mut store, &root_preconditions(), sink.clone())
            .await
            .expect_err("inactive or non-admin account must be refused");
        assert!(matches!(error, ControllerError::InvalidState));
        let state = store.snapshot();
        assert!(state.rolled_back && !state.committed_tx);
        // 不自动启用、不重新初始化：账号布尔位与哈希均保持原样。
        let committed = state.committed.admin.expect("account untouched");
        assert_eq!(committed, row);
        assert_eq!(state.committed.password_hash, "existing-argon2id-digest");
        assert!(!state.committed.must_change_password);
        assert_eq!(state.committed.live_sessions, 3);
        assert_eq!(state.committed.authz_epoch, 41);
        assert!(state.committed.audit.is_empty());
        assert_eq!(sink.log().emitted, 0);
    }
}

#[tokio::test]
async fn missing_bootstrap_account_is_not_found_and_never_reinitialized() {
    let store = MemoryStore::new();
    let mut store = store;
    let sink = RecordingSink::new();
    let error = run_emergency_reset(&mut store, &root_preconditions(), sink.clone())
        .await
        .expect_err("absent admin account is not reinitialized here");
    assert!(matches!(error, ControllerError::NotFound));
    let state = store.snapshot();
    assert_eq!(
        state.calls,
        vec![
            "acquire_guard".to_owned(),
            "lock_admin".to_owned(),
            "rollback".to_owned(),
        ],
        "缺账号必须回滚且不追加任何写"
    );
    assert!(state.rolled_back && !state.committed_tx);
    assert!(state.committed.admin.is_none(), "绝不创建 admin 账号");
    assert!(state.committed.password_hash.is_empty());
    assert_eq!(sink.log().emitted, 0);
}

// ---------------------------------------------------------------- 真实仓储结构门禁

/// 结构性核查（防“顺手写第二条 SQL”）；不替代真实 PTY + 隔离 DB 验收。
#[test]
fn store_statements_are_single_parameterized_cas_statements() {
    for sql in [
        SELECT_ADMIN_SQL,
        UPDATE_ADMIN_PASSWORD_SQL,
        REVOKE_ADMIN_SESSIONS_SQL,
    ] {
        assert!(!sql.contains(';'), "no statement stacking: {sql}");
    }
    assert!(SELECT_ADMIN_SQL.contains("FOR UPDATE"));
    assert!(SELECT_ADMIN_SQL.contains("username = 'admin'"));
    assert!(SELECT_ADMIN_SQL.contains("CAST(active AS SIGNED)"));
    assert!(SELECT_ADMIN_SQL.contains("CAST(is_admin AS SIGNED)"));
    assert_eq!(UPDATE_ADMIN_PASSWORD_SQL.matches('?').count(), 4);
    assert!(UPDATE_ADMIN_PASSWORD_SQL.contains("must_change_password = TRUE"));
    assert!(UPDATE_ADMIN_PASSWORD_SQL.contains("WHERE id = ? AND revision = ?"));
    assert_eq!(REVOKE_ADMIN_SESSIONS_SQL.matches('?').count(), 1);
    assert!(REVOKE_ADMIN_SESSIONS_SQL.contains("revoked = FALSE"));
}

#[tokio::test]
async fn sqlx_store_refuses_writes_before_the_guard_transaction_exists() {
    // 不连库的 lazy 池：本用例只断言“未取 guard 即写”被结构性拒绝，不触碰 DB。
    let db = DbPool(
        sqlx::mysql::MySqlPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(50))
            .connect_lazy("mysql://invalid.invalid:1/invalid")
            .expect("lazy pool needs no database"),
    );
    let mut store = SqlxResetStore::new(&db);
    let id = admin_id();

    let error = store.lock_admin().await.expect_err("no transaction yet");
    assert_eq!(
        error.to_string(),
        "configuration: identity reset-admin tx missing"
    );
    let error = store
        .replace_password(id, "$argon2id$digest", 1, 2)
        .await
        .expect_err("no transaction yet");
    assert_eq!(
        error.to_string(),
        "configuration: identity reset-admin tx missing"
    );
    assert!(
        matches!(
            store.revoke_sessions(id).await,
            Err(ControllerError::Config(_))
        ),
        "revoke requires the guard transaction"
    );
    assert!(matches!(
        store.bump_authz_epoch().await,
        Err(ControllerError::Config(_))
    ));
    assert!(matches!(
        store.commit().await,
        Err(ControllerError::Config(_))
    ));
    // 空哈希在任何 SQL 之前就被拒绝（错误不是“缺事务”）。
    let error = store
        .replace_password(id, "", 1, 2)
        .await
        .expect_err("empty hash must be rejected");
    assert!(matches!(error, ControllerError::Crypto));
    // rollback 幂等且吞错；Debug 不含连接信息。
    // rollback 幂等：无事务时也安全，且吞掉回滚错误。
    let _ = store.rollback().await;
    let _ = store.rollback().await;
    let debug = format!("{store:?}");
    assert!(!debug.contains("invalid.invalid"));
    assert!(!debug.contains("mysql://"));
}

#[test]
fn controlling_tty_debug_and_open_failure_are_redacted() {
    // `/dev/tty` 不可用（无控制终端）时必须是去敏固定文案；可用时 Debug 也不含秘密。
    match ControllingTty::open() {
        Ok(tty) => {
            let debug = format!("{tty:?}");
            assert_eq!(debug, "ControllingTty { .. }");
        }
        Err(error) => {
            assert_eq!(
                error.to_string(),
                format!("configuration: {ERR_TTY_REQUIRED}")
            );
        }
    }
}

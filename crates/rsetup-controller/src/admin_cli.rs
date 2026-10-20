//! 本机 root 应急重置内置管理员凭据（`reset-admin`）的核心逻辑。
//!
//! 约束来源：`docs/superpowers/specs/2026-09-23-controller-v1-01-identity-access.md` §1
//! 与 `docs/superpowers/plans/2026-10-02-controller-01-identity-data-tdd.md` Task 2、
//! `docs/superpowers/plans/2026-10-07-controller-v1-remaining-identity-tdd.md` Task 5。
//!
//! 不变式（本模块自身即门禁，缺失即 fail-closed）：
//! 1. 只信 OS 实际 `real UID`/`effective UID`（`libc::getuid`/`geteuid`），不信任何环境变量、
//!    `USER`/`SUDO_USER` 或命令行声称的权限；
//! 2. `stdin`/`stdout`/`stderr` 必须各自都是交互 TTY，且 `/dev/tty` 只写句柄必须在**任何写库
//!    动作之前**成功打开并核验——防止事务已提交却无处输出凭据；
//! 3. 仅操作引导账号 `username = 'admin'`，且该账号必须仍是 `active && is_admin`；不自动启用、
//!    不重新初始化数据库、不接受其它用户名；
//! 4. 新哈希、`must_change_password = TRUE`、`revision` 自增、全部 sessions 撤销与脱敏审计在同一
//!    事务内提交，`authz_epoch` 走 checked CAS；任一步失败即整体回滚，绝不部分生效；
//! 5. 一次性临时凭据只在 commit 明确成功后经 [`CredentialSink`] 写出一次，绝不进入
//!    stdout/stderr、普通日志或审计表；commit 返回错误时提交结果未知，不输出秘密、不声称已回滚；
//! 6. 真实一次性凭据是 capability，其 `Debug`/`Display` 恒去敏，只有显式 `expose()` 才取值。
//!
//! 本模块的可测部分（门禁判定、事务编排、CAS 失败关闭、审计脱敏、写出时机）以 [`EmergencyResetStore`]
//! 抽象 + 内存桩做行为测试；真实 MySQL 写路径仍需另行以本机 PTY + 隔离真实 DB 验收，不以内存桩冒充。

use std::fmt;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::pin::Pin;

use rand::{RngCore, rngs::OsRng};
use sqlx::Row;

use crate::audit::AuditInsert;
use crate::auth::password::PasswordHasher;
use crate::db::DbPool;
use crate::error::ControllerError;

/// 引导账号唯一合法用户名：不接受参数替换，inactive/非 admin 一律拒绝。
pub const BOOTSTRAP_ADMIN_USERNAME: &str = "admin";

/// 必审事件类型（spec 02 §审计最小集要求 reset-admin 必须留痕）。
pub const RESET_ADMIN_EVENT_TYPE: &str = "auth.reset-admin";

/// 一次性临时凭据的熵：16 字节 CSPRNG = 128 bit，hex 表示后 32 个 ASCII 字符，
/// 同时满足密码策略的 12–128 字符与 ≤512 字节边界。
const TEMPORARY_CREDENTIAL_BYTES: usize = 16;

/// 门禁失败文案固定为稳定标识符：绝不回显 uid、路径细节或任何驱动原文。
const ERR_ROOT_REQUIRED: &str = "real and effective UID must both be root";
const ERR_INTERACTIVE_REQUIRED: &str =
    "interactive controlling TTY required on stdin, stdout and stderr";
const ERR_TTY_REQUIRED: &str = "writable /dev/tty controlling terminal required";

/// 预取校验结果（OS 实际权限与交互性快照）。纯数据，便于注入与单元测试。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetPreconditions {
    /// OS 实际 `real UID`（`getuid()`），不是环境变量。
    pub real_uid: u32,
    /// OS 实际 `effective UID`（`geteuid()`），不是环境变量。
    pub effective_uid: u32,
    pub stdin_interactive: bool,
    pub stdout_interactive: bool,
    pub stderr_interactive: bool,
    /// `/dev/tty` 只写句柄是否已在写库之前成功打开并核验为 TTY。
    pub controlling_tty_held: bool,
}

impl ResetPreconditions {
    /// 任一门禁不满足即 `Err`；判定顺序即拒绝顺序（先权限、再流交互性、再 TTY 句柄）。
    pub fn verify(&self) -> Result<(), ControllerError> {
        verify_root_credentials(self.real_uid, self.effective_uid)?;
        verify_interactive_tty(
            self.stdin_interactive,
            self.stdout_interactive,
            self.stderr_interactive,
        )?;
        verify_controlling_tty_held(self.controlling_tty_held)
    }
}

/// 只接受 OS 实际 real/effective UID 双 0；任一非 0 直接 `PermissionDenied`。
pub fn verify_root_credentials(real_uid: u32, effective_uid: u32) -> Result<(), ControllerError> {
    if real_uid == 0 && effective_uid == 0 {
        Ok(())
    } else {
        Err(ControllerError::Config(ERR_ROOT_REQUIRED.into()))
    }
}

/// 三个标准流必须**各自**是交互终端：任一重定向或非 TTY 都拒绝。
pub fn verify_interactive_tty(
    stdin_interactive: bool,
    stdout_interactive: bool,
    stderr_interactive: bool,
) -> Result<(), ControllerError> {
    if stdin_interactive && stdout_interactive && stderr_interactive {
        Ok(())
    } else {
        Err(ControllerError::Config(ERR_INTERACTIVE_REQUIRED.into()))
    }
}

/// 写库前必须已持有核验过的 `/dev/tty` 只写句柄（见 [`ControllingTty::open`]）。
pub fn verify_controlling_tty_held(held: bool) -> Result<(), ControllerError> {
    if held {
        Ok(())
    } else {
        Err(ControllerError::Config(ERR_TTY_REQUIRED.into()))
    }
}

/// 已打开并核验为字符终端的 `/dev/tty` **只写**句柄。
///
/// 它是“写库前就已持有输出通道”这一时序不变式的载体：真实入口 `run_reset_admin`
/// 必须先成功构造它（失败即在任何 DB 连接与写入之前退出），此后凭据只能经由它写出。
/// 不复制 fd、不 `dup`，句柄在 `File` `Drop` 时关闭。
pub struct ControllingTty(std::fs::File);

impl ControllingTty {
    /// 打开 `/dev/tty` 只写句柄并核验确为终端；失败返回去敏固定文案。
    pub fn open() -> Result<Self, ControllerError> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/tty")
            .map_err(|_| ControllerError::Config(ERR_TTY_REQUIRED.into()))?;
        // SAFETY: 传入本函数刚成功打开、在本作用内存活的合法 fd；isatty 只读属性、不接管所有权。
        let is_terminal = unsafe { libc::isatty(file.as_raw_fd()) } == 1;
        if !is_terminal {
            return Err(ControllerError::Config(ERR_TTY_REQUIRED.into()));
        }
        Ok(Self(file))
    }
}

/// `ControllingTty` 只作为“已持有输出通道”的证据；凭据写出走下面的固定文本渲染，
/// 不经 `Debug`/`Display`，因此这里只实现 `Debug` 且绝不打印 fd 之外的信息。
impl fmt::Debug for ControllingTty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ControllingTty").finish_non_exhaustive()
    }
}

/// 一次性临时凭据的写出通道。
///
/// 实现方必须保证：只写受控终端，不写 stdout/stderr/普通日志/审计；同一次运行内至多成功写出一次。
pub trait CredentialSink {
    fn emit(&mut self, credential: &TemporaryCredential) -> Result<(), ControllerError>;
}

/// 让已持有的句柄（`&mut ControllingTty` 或测试桩）可以直接作为 sink 传入，
/// 调用方仍保有句柄所有权，便于在写出后继续断言或关闭。
impl<S: CredentialSink + ?Sized> CredentialSink for &mut S {
    fn emit(&mut self, credential: &TemporaryCredential) -> Result<(), ControllerError> {
        (**self).emit(credential)
    }
}

/// 把凭据渲染为固定的去敏外壳 + 一次性密码文本。
///
/// 唯一渲染出口，便于行为测试断言文本形状；绝不包含哈希、连接串或任何其它秘密。
pub fn render_credential_text(credential: &TemporaryCredential) -> String {
    format!(
        "\n[EMERGENCY RESET] 内置管理员账号 {BOOTSTRAP_ADMIN_USERNAME} 密码已重置。\n\
         Username: {BOOTSTRAP_ADMIN_USERNAME}\n\
         Temporary password: {}\n\
         该密码只展示这一次；首次登录必须立即修改，且原会话已全部撤销。\n\
         若本行未能读到，请勿重复执行初始化：由 root 受控再次运行 reset-admin 以签发新的临时凭据。\n\n",
        credential.expose()
    )
}

/// 凭据写出失败后的固定指引：已提交的这次重置仍然有效，但其临时凭据视为丢失。
const ERR_SINK_WRITE: &str = "committed reset succeeded but the temporary credential could not \
                              be written to the controlling terminal; treat this credential as \
                              lost and run reset-admin again as root to issue a new one";

/// 经已持有的 `/dev/tty` 句柄写出凭据；只 write+flush，不 fsync（tty 不支持）。
impl CredentialSink for ControllingTty {
    fn emit(&mut self, credential: &TemporaryCredential) -> Result<(), ControllerError> {
        let text = render_credential_text(credential);
        // `&File: Write`，写整块文本后 flush：tty 是行缓冲的，不 fsync（不支持）。
        let mut writer = &self.0;
        writer
            .write_all(text.as_bytes())
            .and_then(|_| writer.flush())
            .map_err(|_| ControllerError::Config(ERR_SINK_WRITE.into()))
    }
}

/// 一次性临时凭据：capability 型新类型。
///
/// `Debug`/`Display` **恒去敏**，避免被顺手 `{:?}`/`println!` 带入日志或测试输出；
/// 只有显式 `expose()` 才取到明文。
pub struct TemporaryCredential(String);

impl TemporaryCredential {
    /// 显式取值：命名即审计点，调用方必须保证目标只写受控终端。
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// 仅测试用：由已知秘密构造，用于断言渲染与去敏行为。
    #[cfg(test)]
    pub(crate) fn from_test_secret(secret: &str) -> Self {
        Self(secret.to_owned())
    }
}

impl fmt::Debug for TemporaryCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TemporaryCredential([REDACTED])")
    }
}

impl fmt::Display for TemporaryCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// 生成一次性临时凭据：CSPRNG 秘密 + Argon2id 哈希（策略复用 `bootstrap_admin`）。
pub fn generate_temporary_credential() -> Result<(TemporaryCredential, String), ControllerError> {
    let mut entropy = [0u8; TEMPORARY_CREDENTIAL_BYTES];
    OsRng.fill_bytes(&mut entropy);
    let secret = hex::encode(entropy);
    let hash = PasswordHasher::new().hash(&secret)?;
    Ok((TemporaryCredential(secret), hash))
}

/// 事务内读到的内置 admin 账号快照（严格解码后的可信布尔位）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdminAccountRow {
    pub id: [u8; 16],
    pub revision: u64,
    pub active: bool,
    pub is_admin: bool,
}

/// 重置成功后的去敏结果（不含任何秘密；`revision`/`authz_epoch` 用于运维核对）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmergencyResetOutcome {
    pub admin_revision: u64,
    pub authz_epoch: u64,
    pub revoked_sessions: u64,
}

/// `audit_events` 的脱敏载荷（11 字段中由本结构固定 7 个；id、time_evidence、
/// process_epoch、event_seq 由 `crate::audit::insert_audit` 生成）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetAuditEvent {
    pub event_type: &'static str,
    pub actor_kind: &'static str,
    pub actor_user_id: Option<[u8; 16]>,
    pub target_kind: &'static str,
    pub target_id: String,
    pub params_redacted: &'static str,
    pub outcome: &'static str,
}

/// 应急重置审计事件：本机无 HTTP actor，故 `actor_kind = "system"` 且 `actor_user_id` 为空；
/// 参数恒为 `{}`，`target_id` 只含 admin 用户 id 的 hex，零明文秘密。
pub fn reset_audit_event(admin_id: [u8; 16]) -> ResetAuditEvent {
    ResetAuditEvent {
        event_type: RESET_ADMIN_EVENT_TYPE,
        actor_kind: "system",
        actor_user_id: None,
        target_kind: "user",
        target_id: hex::encode(admin_id),
        params_redacted: "{}",
        outcome: "success",
    }
}

/// 被 `EmergencyResetStore` 复用的 future 别名（与 `authz::sqlx_repo::RepoFuture` 同形）。
pub type StoreFuture<'a, T> =
    Pin<Box<dyn std::future::Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 应急重置的事务操作抽象。
///
/// 唯一真实实现是 [`SqlxResetStore`]（MySQL 事务）；测试用内存桩只验证编排语义
/// （门禁前置、CAS 失败关闭、审计同事务、commit 前不泄密），不构成真实 DB 验收。
///
/// 实现方不变式：
/// - 除 `rollback` 外的任何方法出错后调用方立即 `rollback`，不得重试或继续写；
/// - `commit` 只在全部业务写与审计成功后调用一次；`commit` 返回错误即结果未知，
///   调用方不得声称已回滚，也绝不输出临时凭据；
/// - `rollback` 只在**确认尚未 commit** 的错误路径调用，幂等且必须吞掉回滚错误。
pub trait EmergencyResetStore {
    /// 开启事务并锁定 `schema_meta(singleton = 1)` guard 行（复用 `lock_integrity_guard`）。
    fn acquire_guard(&mut self) -> StoreFuture<'_, ()>;

    /// 以 `FOR UPDATE` 读取内置 admin 行；账号不存在返回 `None`。
    fn lock_admin(&mut self) -> StoreFuture<'_, Option<AdminAccountRow>>;

    /// CAS 替换密码哈希：`WHERE id = ? AND revision = ?`，同时置 `must_change_password = TRUE`
    /// 与 `revision = next_revision`；返回是否恰好命中 1 行。
    fn replace_password<'a>(
        &'a mut self,
        admin_id: [u8; 16],
        password_hash: &'a str,
        expected_revision: u64,
        next_revision: u64,
    ) -> StoreFuture<'a, bool>;

    /// 撤销该用户全部活动会话，返回撤销行数。
    fn revoke_sessions(&mut self, admin_id: [u8; 16]) -> StoreFuture<'_, u64>;

    /// checked CAS 递增 `schema_meta.authz_epoch`（复用 `authz::sqlx_repo::bump_authz_epoch`）。
    fn bump_authz_epoch(&mut self) -> StoreFuture<'_, u64>;

    /// 同事务写入脱敏审计事件。
    fn record_audit<'a>(&'a mut self, event: &'a ResetAuditEvent) -> StoreFuture<'a, ()>;

    /// 提交事务；错误即结果未知。
    fn commit(&mut self) -> StoreFuture<'_, ()>;

    /// 明确未 commit 的错误路径回滚（吞掉回滚错误，保留原始错误）。
    fn rollback(&mut self) -> StoreFuture<'_, ()>;
}

/// 账号资格门禁：inactive 或非 admin 一律拒绝，不自动启用、不重新初始化。
pub fn verify_admin_eligible(admin: &AdminAccountRow) -> Result<(), ControllerError> {
    if admin.active && admin.is_admin {
        Ok(())
    } else {
        Err(ControllerError::InvalidState)
    }
}

/// 空哈希防御：绝不把空 `password_hash` 写库（否则该账号密码永不可验证）。
pub fn verify_password_hash_present(password_hash: &str) -> Result<(), ControllerError> {
    if password_hash.is_empty() {
        Err(ControllerError::Crypto)
    } else {
        Ok(())
    }
}

/// 应急重置编排：门禁 → 生成秘密 → 单事务原子写 → 仅 commit 成功后写出一次凭据。
pub async fn run_emergency_reset<S, W>(
    store: &mut S,
    preconditions: &ResetPreconditions,
    mut sink: W,
) -> Result<EmergencyResetOutcome, ControllerError>
where
    S: EmergencyResetStore + ?Sized,
    W: CredentialSink,
{
    // 明确未 commit 的失败路径：先回滚（吞掉回滚自身的错误），再原样上抛原始错误。
    macro_rules! abandon {
        ($error:expr) => {{
            let _ = store.rollback().await;
            return Err($error);
        }};
    }
    // store 上任意一步出错即按 abandon 处理；Ok 分支直接取值。
    macro_rules! step {
        ($body:expr) => {
            match $body {
                Ok(value) => value,
                Err(error) => abandon!(error),
            }
        };
    }

    // 1. 一切 DB 访问之前完成全部本机门禁；失败即零副作用退出。
    preconditions.verify()?;

    // 2. 秘密在事务外生成：任何失败路径都不会把它写库，也不会被输出。
    let (credential, password_hash) = generate_temporary_credential()?;
    verify_password_hash_present(&password_hash)?;

    // 3. 统一锁序：guard 行 → admin 用户行。
    step!(store.acquire_guard().await);
    let Some(admin) = step!(store.lock_admin().await) else {
        // 引导账号不存在：交人工核实安装状态，本 CLI 绝不重新初始化。
        abandon!(ControllerError::NotFound);
    };
    if let Err(error) = verify_admin_eligible(&admin) {
        abandon!(error);
    }
    let Some(next_revision) = admin.revision.checked_add(1) else {
        abandon!(ControllerError::RevisionConflict);
    };

    // 4. CAS 更新账号：affected != 1 即冲突，绝不覆盖较新写入。
    let replaced = step!(
        store
            .replace_password(admin.id, &password_hash, admin.revision, next_revision)
            .await
    );
    if !replaced {
        abandon!(ControllerError::RevisionConflict);
    }

    // 5. 会话撤销、authz_epoch 递增与脱敏审计必须与账号写同事务。
    let revoked_sessions = step!(store.revoke_sessions(admin.id).await);
    let authz_epoch = step!(store.bump_authz_epoch().await);
    let audit = reset_audit_event(admin.id);
    step!(store.record_audit(&audit).await);

    // 6. commit 错误即结果未知：不输出秘密、不声称已回滚（故也不主动 rollback）。
    match store.commit().await {
        Ok(()) => {}
        Err(error) => return Err(error),
    }

    // 7. 仅确认 commit 成功后写出一次；写失败不回滚、不重试、不重显旧凭据。
    sink.emit(&credential)?;

    Ok(EmergencyResetOutcome {
        admin_revision: next_revision,
        authz_epoch,
        revoked_sessions,
    })
}

/// 真实 MySQL 事务实现。
///
/// 复用既有安全更新逻辑：guard 行锁 `integrity::lock_integrity_guard`、
/// `authz_epoch` 的 checked CAS `authz::sqlx_repo::bump_authz_epoch`、
/// ADMIN 审计命名空间 `audit::insert_audit`，不自建第二套写法。
pub struct SqlxResetStore<'a> {
    db: &'a DbPool,
    tx: Option<sqlx::Transaction<'a, sqlx::MySql>>,
}

/// `SqlxResetStore` 的 Debug 只呈现状态机阶段，不含连接、SQL 参数或秘密。
impl fmt::Debug for SqlxResetStore<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqlxResetStore")
            .field("tx", &self.tx.is_some())
            .finish()
    }
}

/// 固定语句常量：全部参数化、单条语句、无拼接。
const SELECT_ADMIN_SQL: &str = "SELECT id, revision, CAST(active AS SIGNED) AS active, CAST(is_admin AS SIGNED) AS is_admin FROM users WHERE username = 'admin' FOR UPDATE";
const UPDATE_ADMIN_PASSWORD_SQL: &str = "UPDATE users SET password_hash = ?, must_change_password = TRUE, revision = ? WHERE id = ? AND revision = ?";
const REVOKE_ADMIN_SESSIONS_SQL: &str =
    "UPDATE sessions SET revoked = TRUE WHERE user_id = ? AND revoked = FALSE";

fn read_error(code: &'static str) -> ControllerError {
    ControllerError::Config(format!("identity reset-admin read {code}"))
}

impl<'a> SqlxResetStore<'a> {
    /// 只存句柄、不开事务：`begin` 推迟到 `acquire_guard`，保持“门禁先于 DB 访问”。
    pub fn new(db: &'a DbPool) -> Self {
        Self { db, tx: None }
    }

    fn tx_mut(&mut self) -> Result<&mut sqlx::Transaction<'a, sqlx::MySql>, ControllerError> {
        self.tx
            .as_mut()
            .ok_or_else(|| ControllerError::Config("identity reset-admin tx missing".into()))
    }
}

impl EmergencyResetStore for SqlxResetStore<'_> {
    fn acquire_guard(&mut self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            if self.tx.is_some() {
                return Err(ControllerError::InvalidState);
            }
            let mut tx = self.db.0.begin().await?;
            // 复用写事务 guard：期望版本取编译期常量，版本不符即 fail-closed。
            crate::integrity::lock_integrity_guard(&mut tx).await?;
            self.tx = Some(tx);
            Ok(())
        })
    }

    fn lock_admin(&mut self) -> StoreFuture<'_, Option<AdminAccountRow>> {
        Box::pin(async move {
            let tx = self.tx_mut()?;
            let row = sqlx::query(SELECT_ADMIN_SQL)
                .fetch_optional(&mut **tx)
                .await?;
            let Some(row) = row else { return Ok(None) };
            let id_raw: Vec<u8> = row.try_get("id").map_err(|_| read_error("users.id"))?;
            let id: [u8; 16] = id_raw
                .as_slice()
                .try_into()
                .map_err(|_| read_error("users.id"))?;
            let revision: u64 = row
                .try_get("revision")
                .map_err(|_| read_error("users.revision"))?;
            // 严格 0/1 解码：污染值当 corruption 拒绝，绝不当成 true。
            let active = crate::integrity::raw_bool(
                row.try_get::<i64, _>("active")
                    .map_err(|_| read_error("users.active"))?,
                "users.active",
            )?;
            let is_admin = crate::integrity::raw_bool(
                row.try_get::<i64, _>("is_admin")
                    .map_err(|_| read_error("users.is_admin"))?,
                "users.is_admin",
            )?;
            Ok(Some(AdminAccountRow {
                id,
                revision,
                active,
                is_admin,
            }))
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
            verify_password_hash_present(password_hash)?;
            let tx = self.tx_mut()?;
            let affected = sqlx::query(UPDATE_ADMIN_PASSWORD_SQL)
                .bind(password_hash)
                .bind(next_revision)
                .bind(admin_id.as_slice())
                .bind(expected_revision)
                .execute(&mut **tx)
                .await?
                .rows_affected();
            Ok(affected == 1)
        })
    }

    fn revoke_sessions(&mut self, admin_id: [u8; 16]) -> StoreFuture<'_, u64> {
        Box::pin(async move {
            let tx = self.tx_mut()?;
            let affected = sqlx::query(REVOKE_ADMIN_SESSIONS_SQL)
                .bind(admin_id.as_slice())
                .execute(&mut **tx)
                .await?
                .rows_affected();
            Ok(affected)
        })
    }

    fn bump_authz_epoch(&mut self) -> StoreFuture<'_, u64> {
        Box::pin(async move {
            let tx = self.tx_mut()?;
            // 复用既有 checked CAS：affected != 1 即 RevisionConflict，整笔事务回滚。
            crate::authz::sqlx_repo::bump_authz_epoch(&mut *tx).await
        })
    }

    fn record_audit<'a>(&'a mut self, event: &'a ResetAuditEvent) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let tx = self.tx_mut()?;
            crate::audit::insert_audit(
                tx,
                &AuditInsert {
                    actor_kind: event.actor_kind,
                    actor_user_id: event.actor_user_id,
                    event_type: event.event_type,
                    target_kind: event.target_kind,
                    target_id: Some(event.target_id.clone()),
                    params_redacted: event.params_redacted,
                    outcome: event.outcome,
                },
            )
            .await
        })
    }

    fn commit(&mut self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            // take()：commit 之后事务句柄不存在，杜绝“commit 后再写”。
            let tx = self
                .tx
                .take()
                .ok_or_else(|| ControllerError::Config("identity reset-admin tx missing".into()))?;
            tx.commit().await?;
            Ok(())
        })
    }

    fn rollback(&mut self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            // 仅错误路径调用；回滚失败不覆盖原始错误。句柄被 take 后 Drop 兜底回滚。
            if let Some(tx) = self.tx.take() {
                let _ = tx.rollback().await;
            }
            Ok(())
        })
    }
}

/// 从 OS 实际权限与标准流读取门禁快照（不接受任何环境变量输入）。
pub fn collect_preconditions(controlling_tty_held: bool) -> ResetPreconditions {
    use std::io::IsTerminal;

    // SAFETY: getuid/geteuid 无参数、无指针、不产生副作用，是 POSIX 保证的纯系统调用。
    let (real_uid, effective_uid) = unsafe { (libc::getuid(), libc::geteuid()) };
    ResetPreconditions {
        real_uid,
        effective_uid,
        stdin_interactive: std::io::stdin().is_terminal(),
        stdout_interactive: std::io::stdout().is_terminal(),
        stderr_interactive: std::io::stderr().is_terminal(),
        controlling_tty_held,
    }
}

/// CLI 使用的完整入口。
///
/// 调用方必须在建立 DB 连接与任何写库动作之前成功打开 `/dev/tty`
/// （[`ControllingTty`] 即“输出通道已持有”的类型级证据），并把它作为唯一写出通道传入；
/// 本函数此后才建立 guard 事务。UID 与三标准流交互性在此处从 OS 实际取值复核。
pub async fn run_reset_admin(
    db: &DbPool,
    tty: &mut ControllingTty,
) -> Result<EmergencyResetOutcome, ControllerError> {
    let preconditions = collect_preconditions(true);
    let mut store = SqlxResetStore::new(db);
    run_emergency_reset(&mut store, &preconditions, tty).await
}

#[cfg(test)]
mod tests;

//! `migrate-controller`：中控**生产**库的显式迁移 CLI（操作员专用，非服务启动路径）。
//!
//! 只做两件事：`--check` 只读地报告当前 schema 版本；`--apply` 按固定决策表把目标库
//! 迁移到受支持版本（见 `rsetup_controller::db::run_production_migration`）。它**不**是
//! Web 端点，**不**接受任何 DSN/库名/备份路径参数——这些一律只从进程环境读取。
//!
//! 顺序即门禁：解析参数 → 读取配置与门禁 → 连接数据库 → 由库内共享授权器复核
//! （声明库名必须等于服务端真实返回的库名、账号必须真的有 schema 元数据可见性）→ 迁移。
//! 任一步失败即固定去敏退出，绝不自动修复、重试或伪造成功。
//!
//! 权限：不要求 root。写入权限由 `CONTROLLER_DATABASE_URL` 指向的账号自身权限决定，
//! 隔离/独占/已备份三项由 `CONTROLLER_MIGRATION_*` 门禁由操作员显式确认。本 CLI 从不
//! 读写 `/dev/tty`，也不向任何文件写秘密。
//!
//! 参数解析必须使用 `std::env::args_os()`：Unix 下 argv 是任意非 NUL 字节串，
//! `std::env::args()` 会对非法 UTF-8 参数在标准库内部 `unwrap()` panic（退出码 101），并把
//! 携带原始字节的 panic 原文写入 stderr——等于回显操作员误传的输入。这里只做 `OsStr`
//! 字节级精确比较，绝不做编码转换、绝不回显任何收到的 token。

use std::ffi::OsStr;

use rsetup_controller::db::{
    self, migration_config_from_env, read_schema_version, run_production_migration,
};
use rsetup_controller::{ControllerConfig, ControllerError, DbPool};

/// 使用方式：纯固定文本，不含主机、路径或任何凭据。命令名用交付给操作员的二进制名
/// `rsetup-controller-migrate`（cargo bin 名与 Debian `/usr/bin` 名一致），不是源文件名。
const USAGE: &str = "\
rsetup-controller-migrate - explicit schema migration for the controller database

Usage:
  rsetup-controller-migrate --check    read-only: print the current schema version
  rsetup-controller-migrate --apply    migrate the database to the supported version

Environment (--check):
  CONTROLLER_DATABASE_URL                     required, the controller database URL

Environment (--apply, all of them required):
  CONTROLLER_DATABASE_URL                     the same variable the service uses
  CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE       must be exactly 1
  CONTROLLER_MIGRATION_EXPECTED_DATABASE      must equal the connected database name
  CONTROLLER_MIGRATION_BACKUP_REF             non-blank reference to a real backup
  CONTROLLER_MIGRATION_ACK                    exact acknowledgement literal

No credential, host, database name or backup reference is ever printed. Failures are
reported with fixed, de-identified messages; the database driver's own text is suppressed.
";

/// 解析后的命令行请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// 只读版本探测。
    Check,
    /// 执行迁移。
    Apply,
    /// 打印使用方式（`-h`/`--help`），不读配置、不连接数据库。
    Usage,
}

/// CLI 错误：携带已去敏、可直接打印的单行消息。
#[derive(Debug)]
struct CliError {
    message: String,
    /// 为真时在消息后附使用方式。
    show_usage: bool,
}

impl CliError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            show_usage: false,
        }
    }
}

impl From<ControllerError> for CliError {
    fn from(error: ControllerError) -> Self {
        Self::new(describe_error(&error))
    }
}

/// 解析命令行：**恰好**一个模式 token，`--check` 或 `--apply`（或 `-h`/`--help`），
/// 其余（含非法 UTF-8、两个模式同时给、尾随参数、`--dsn=…`）一律 fail-closed 拒绝。
///
/// 入参是 `OsStr`：只做字节级精确比较，绝不 `Debug`/`Display`/lossy 收到的 token，
/// 因此非法 UTF-8 既不 panic 也不会被回显。
fn parse_request<I, T>(args: I) -> Result<Request, CliError>
where
    I: IntoIterator<Item = T>,
    T: AsRef<OsStr>,
{
    let tokens: Vec<T> = args.into_iter().collect();
    let request = match tokens.as_slice() {
        [flag] if flag.as_ref() == OsStr::new("--check") => Request::Check,
        [flag] if flag.as_ref() == OsStr::new("--apply") => Request::Apply,
        [flag] if flag.as_ref() == OsStr::new("-h") || flag.as_ref() == OsStr::new("--help") => {
            Request::Usage
        }
        // 绝不回显收到的 token：其中可能正是口令，也可能是含非法字节的输入。
        _ => {
            return Err(CliError {
                message: "unexpected arguments: choose exactly one of --check or --apply"
                    .to_owned(),
                show_usage: true,
            });
        }
    };
    Ok(request)
}

/// 把错误映射为固定、去敏的可打印消息。
///
/// `Database` 变体的驱动原文可能含主机、端口或用户信息，一律以固定文案替换；`Config`
/// 变体来自本 crate 的门禁与配置检查（其文案由编译期字面量或表/列名组成，不含秘密），
/// 原样透出便于运维定位。
fn describe_error(error: &ControllerError) -> String {
    match error {
        ControllerError::Config(_)
        | ControllerError::InvalidArgument
        | ControllerError::RevisionConflict
        | ControllerError::PermissionDenied
        | ControllerError::NotFound
        | ControllerError::ResourceExhausted
        | ControllerError::LastAdmin
        | ControllerError::InvalidState => error.to_string(),
        ControllerError::Database(_) => {
            "database operation failed (details suppressed; check CONTROLLER_DATABASE_URL \
             and database availability)"
                .to_owned()
        }
        ControllerError::Crypto => "cryptographic operation failed".to_owned(),
        ControllerError::SchemaNotReady { found, required } => {
            format!("identity schema not ready: found {found:?}, required {required}")
        }
        ControllerError::TaskIdempotencyConflict | ControllerError::TaskPreviewInvalidState => {
            "invalid state for this operation".to_owned()
        }
    }
}

/// 向 stdout 写入固定文本；写失败即错误（不使用会 panic 的 `print!`）。
fn write_stdout(text: &str) -> Result<(), CliError> {
    use std::io::Write;

    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle
        .write_all(text.as_bytes())
        .and_then(|()| handle.flush())
        .map_err(|_| CliError::new("standard output is unavailable"))
}

/// 向 stderr 写一行去敏错误；stderr 不可用时静默（退出码仍为失败）。
fn note_stderr(text: &str) {
    use std::io::Write;

    let stderr = std::io::stderr();
    let mut handle = stderr.lock();
    let _ = handle.write_all(text.as_bytes());
    let _ = handle.flush();
}

/// `--check`：只读版本探测。输出只有版本号或 `missing`，绝不含 URL、库名或主机。
async fn run_check() -> Result<(), CliError> {
    let config = ControllerConfig::from_env()?;
    let db = DbPool::connect(&config).await?;
    match read_schema_version(&db).await? {
        Some(version) => write_stdout(&format!("schema_version={version}\n")),
        None => write_stdout("schema_version=missing\n"),
    }
}

/// `--apply`：门禁 → 授权 → 迁移。失败信息全部去敏。
async fn run_apply() -> Result<(), CliError> {
    let config = migration_config_from_env()?;
    run_production_migration(&config).await?;
    write_stdout("migration complete: schema_version=4\n")
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // 必须使用 `args_os()`：`args()` 在遇到非法 UTF-8 字节时于标准库内部 `unwrap()` panic，
    // 并把携带原始字节的 panic 原文写入 stderr（退出码 101），构成信息泄露。
    let request = match parse_request(std::env::args_os().skip(1)) {
        Ok(request) => request,
        Err(error) => {
            note_stderr(&format!("migrate-controller: {}\n", error.message));
            if error.show_usage {
                note_stderr(USAGE);
            }
            return std::process::ExitCode::FAILURE;
        }
    };
    let result = match request {
        // ack 字面量是固定契约值而非秘密，直接取自 db 模块，避免文档与实现漂移。
        Request::Usage => write_stdout(&format!(
            "{USAGE}The exact acknowledgement literal for CONTROLLER_MIGRATION_ACK is:\n  {}\n",
            db::MIGRATION_ACK_CONFIRMATION
        )),
        Request::Check => run_check().await,
        Request::Apply => run_apply().await,
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            note_stderr(&format!("migrate-controller: {}\n", error.message));
            if error.show_usage {
                note_stderr(USAGE);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

/// 让 `db` 模块的公开迁移面在本 bin 内可见：这里只引用，不重新实现任何门禁。
const _: fn() = || {
    let _ = db::MIGRATION_ACK_CONFIRMATION;
};

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(|item| OsString::from(*item)).collect()
    }

    #[test]
    fn accepts_exactly_one_of_the_two_modes() {
        assert_eq!(parse_request(argv(&["--check"])).unwrap(), Request::Check);
        assert_eq!(parse_request(argv(&["--apply"])).unwrap(), Request::Apply);
        assert_eq!(parse_request(argv(&["-h"])).unwrap(), Request::Usage);
        assert_eq!(parse_request(argv(&["--help"])).unwrap(), Request::Usage);
    }

    #[test]
    fn every_other_shape_is_rejected_without_echo() {
        for arguments in [
            &[][..],
            &["--check", "--apply"][..],
            &["--apply", "extra"][..],
            &["--mode", "upgrade"][..],
            &["-c"][..],
            &["--CHECK"][..],
            &["--apply=true"][..],
            &["--dsn", "mysql://user:secret@host/db"][..],
            &[""][..],
        ] {
            let error = parse_request(argv(arguments)).expect_err("必须拒绝");
            assert_eq!(
                error.message,
                "unexpected arguments: choose exactly one of --check or --apply"
            );
            assert!(error.message.is_ascii(), "固定错误必须纯 ASCII");
            assert!(error.show_usage);
        }
        // 被拒绝的 token 本身不得出现在消息里（上例中的 DSN 即测试样本）。
        let error =
            parse_request(argv(&["--dsn", "mysql://user:secret@host/db"])).expect_err("必须拒绝");
        for fragment in ["mysql://", "secret", "host", "db"] {
            assert!(!error.message.contains(fragment), "参数不得回显 {fragment}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_reject_without_panic_or_echo() {
        use std::os::unix::ffi::OsStringExt as _;

        let cases: Vec<Vec<u8>> = vec![
            b"FAKE_CLI_SECRET_ONLY\xff".to_vec(),
            b"--apply\xfe".to_vec(),
            b"--check\xff".to_vec(),
            vec![0xff, 0xfe, 0x80],
        ];
        for bytes in cases {
            let token = OsString::from_vec(bytes.clone());
            let error = parse_request([token]).expect_err("非 UTF-8 参数必须被拒绝");
            assert_eq!(
                error.message,
                "unexpected arguments: choose exactly one of --check or --apply"
            );
            assert!(error.message.is_ascii(), "固定错误必须纯 ASCII");
            assert!(
                !error.message.contains("FAKE_CLI_SECRET_ONLY"),
                "参数不得回显"
            );
            assert!(!error.message.contains('\u{FFFD}'), "不得含 lossy 替换符");
        }
    }

    #[test]
    fn driver_errors_are_never_forwarded_verbatim() {
        let error = ControllerError::Database(sqlx::Error::Io(std::io::Error::other(
            "connect to db.internal:3306 as rsc_svc with Sup3r-S3cret-Pw failed",
        )));
        let message = describe_error(&error);
        for fragment in [
            "db.internal",
            "3306",
            "rsc_svc",
            "Sup3r-S3cret-Pw",
            "mysql://",
        ] {
            assert!(!message.contains(fragment), "驱动原文不得透出 {fragment}");
        }
        assert!(message.contains("CONTROLLER_DATABASE_URL"));
        // 配置类门禁文案原样保留（其本身由编译期字面量构成）。
        assert_eq!(
            describe_error(&ControllerError::Config(
                "production migration gate failed".into()
            )),
            "configuration: production migration gate failed"
        );
    }

    #[test]
    fn usage_is_fixed_ascii_without_targets() {
        assert!(USAGE.is_ascii());
        for fragment in ["mysql://", "@", "/etc/", "secret"] {
            assert!(!USAGE.contains(fragment), "USAGE 不得含 {fragment}");
        }
    }
}

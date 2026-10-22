//! `reset-admin`：中控主机**本机**管理员应急重置恢复 CLI。
//!
//! 用途：operator 遗失内置管理员凭据或账号死锁时，在中控主机本地以 root 身份脱机恢复访问权。
//! 本 CLI **不**是 Web 端点、**不**接受用户名/密码参数、**不**读取任何声称权限的环境变量。
//!
//! fail-closed 顺序（任一步失败即以非 0 退出，且不触碰数据库）：
//! 1. 参数：只接受 `reset-admin` 与 `-h`/`--help`，其余（含任意用户名/密码参数、含非法 UTF-8
//!    字节的参数）一律以固定脱敏文案拒绝；argv 经 `std::env::args_os()` 以 `OsStr` 精确比较，
//!    绝不做 UTF-8 转换，因而绝不在标准库内部 `unwrap()` panic 并把原始字节打到 stderr；
//! 2. 本机门禁：OS 实际 `real UID` 与 `effective UID` 双 0，且 `stdin`/`stdout`/`stderr`
//!    各自均为交互 TTY（判定在 `admin_cli` 内于任何 DB 访问之前执行）；
//! 3. 先打开并核验 `/dev/tty` 只写句柄（写库前的输出通道证明）；
//! 4. 最后才从 `CONTROLLER_DATABASE_URL` 读取既有配置、连接数据库并执行原子重置。
//!
//! 秘密处理：一次性临时凭据只经 `/dev/tty` 写出一次；本 CLI 从不向 stdout/stderr/日志写入
//! 凭据；错误信息经 [`describe_error`] 去敏，数据库驱动原文不外泄。

use std::ffi::OsStr;

use rsetup_controller::ControllerConfig;
use rsetup_controller::admin_cli::{self, ControllingTty};
use rsetup_controller::error::ControllerError;

/// 使用方式；只含固定文本，不含主机、路径或任何凭据。
const USAGE: &str = "\
reset-admin - local root emergency recovery for the built-in administrator account

Run this CLI directly on the controller host, as root, from an interactive terminal:

  sudo rsetup-controller-reset-admin

(Packaged binary name; when running from the build tree the binary is
`target/debug/reset-admin`.)

Constraints enforced fail-closed before any database access:
  * real and effective UID must both be root (as reported by the OS, not by env vars);
  * stdin, stdout and stderr must each be an interactive terminal;
  * a writable /dev/tty must be openable (the one-time credential is printed only there);
  * only the bootstrap account 'admin' is eligible, and it must still be active and admin.

Environment:
  CONTROLLER_DATABASE_URL  required, the existing controller database URL
  CONTROLLER_LISTEN_ADDRESS ignored by this CLI

No credential is ever printed to stdout, stderr or logs. A temporary password is shown
on /dev/tty exactly once, only after the reset transaction has committed.
";

/// 解析后的命令行请求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    /// 执行应急重置。
    Reset,
    /// 打印使用方式（`-h`/`--help`），不读写数据库。
    Usage,
}

/// CLI 错误：携带已去敏、可直接打印的单行消息。
#[derive(Debug)]
struct CliError {
    message: String,
    /// 为真时在消息后附使用方式（参数非法或用例请求）。
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

/// 解析命令行：无参数 → 执行重置；仅 `-h`/`--help`（至多一次）→ 打印使用方式。
///
/// 入参是 `OsStr` 而不是 `String`：Unix 下 argv 可以是任意非 NUL 字节，`std::env::args()`
/// 会在标准库内部对非法 UTF-8 做 `unwrap()` 并 panic，panic 原文带着未脱敏的原始字节打到
/// stderr（退出码 101），等于把操作员误传的密码回显出来。因此本 CLI 只使用永不 panic 的
/// `std::env::args_os()`，并且只用 `OsStr` 做**字节级精确比较**，不做任何编码转换。
///
/// 任何其他 token（用户名、密码、`--username`、非法 UTF-8 字节串、重复参数、尾随参数）一律
/// fail-closed 拒绝：内置管理员用户名固定为 `admin`，本 CLI 不提供覆盖入口；错误消息是固定
/// 文案，绝不 `Debug`/`Display` 收到的 token，也绝不回显其任何部分。
fn parse_request<I, T>(args: I) -> Result<Request, CliError>
where
    I: IntoIterator<Item = T>,
    T: AsRef<OsStr>,
{
    let tokens: Vec<T> = args.into_iter().collect();
    match tokens.as_slice() {
        [] => Ok(Request::Reset),
        [flag] if flag.as_ref() == OsStr::new("-h") || flag.as_ref() == OsStr::new("--help") => {
            Ok(Request::Usage)
        }
        _ => {
            // 绝不回显收到的 token：其中可能正是操作员误传的密码，也可能是含非法字节的输入。
            Err(CliError {
                message: "unexpected argument: reset-admin takes no arguments \
                          (the bootstrap administrator account is always 'admin')"
                    .to_owned(),
                show_usage: true,
            })
        }
    }
}

/// 把错误映射为固定、去敏的可打印消息。
///
/// `Database` 变体的驱动原文可能含主机、端口或用户信息，一律以固定文案替换；
/// `Config` 变体来自本 crate 的门禁与配置检查（本身不含秘密），原样透出便于运维定位。
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

/// 按既有规范从进程环境读取配置（唯一来源 `CONTROLLER_DATABASE_URL`，缺失即 fail-closed）。
fn load_config() -> Result<ControllerConfig, ControllerError> {
    ControllerConfig::from_env()
}

/// 读取配置与连接数据库**之前**完成的 OS 级门禁：root 双 UID → 三标准流交互性 → `/dev/tty`
/// 只写句柄。`admin_cli::run_reset_admin` 会在任何事务之前再复核一次（纵深防御）。
fn open_output_channel() -> Result<ControllingTty, ControllerError> {
    let snapshot = admin_cli::collect_preconditions(false);
    admin_cli::verify_root_credentials(snapshot.real_uid, snapshot.effective_uid)?;
    admin_cli::verify_interactive_tty(
        snapshot.stdin_interactive,
        snapshot.stdout_interactive,
        snapshot.stderr_interactive,
    )?;
    ControllingTty::open()
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

/// CLI 主体。
///
/// 顺序即门禁：先解析参数，再打开并核验 `/dev/tty`（写库前的输出通道证明），
/// 然后读配置与连接数据库；`admin_cli` 内部在任何事务之前复核 OS UID 与三标准流交互性。
async fn run(request: Request) -> Result<(), CliError> {
    match request {
        Request::Usage => return write_stdout(USAGE),
        Request::Reset => {}
    }

    // 门禁与输出通道全部先于配置读取与数据库连接：失败即零 DB 副作用退出，
    // 也绝不出现「事务已提交却无处输出凭据」。
    let mut tty = open_output_channel()?;
    let config = load_config()?;
    let db = rsetup_controller::DbPool::connect(&config).await?;
    let outcome = admin_cli::run_reset_admin(&db, &mut tty).await?;
    // 只输出脱敏核对信息（无凭据、无主机），便于运维留档。
    write_stdout(&format!(
        "Emergency reset committed for account '{}': revision {}, authz epoch {}, {} session(s) revoked.\n",
        admin_cli::BOOTSTRAP_ADMIN_USERNAME,
        outcome.admin_revision,
        outcome.authz_epoch,
        outcome.revoked_sessions
    ))
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // 必须使用 `args_os()`：`args()` 在遇到非法 UTF-8 字节时于标准库内部 `unwrap()` panic，
    // 并把携带原始字节的 panic 原文写入 stderr（退出码 101），构成凭据泄露。
    let request = match parse_request(std::env::args_os().skip(1)) {
        Ok(request) => request,
        Err(error) => {
            note_stderr(&format!("reset-admin: {}\n", error.message));
            if error.show_usage {
                note_stderr(USAGE);
            }
            return std::process::ExitCode::FAILURE;
        }
    };
    match run(request).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            note_stderr(&format!("reset-admin: {}\n", error.message));
            if error.show_usage {
                note_stderr(USAGE);
            }
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(|item| (*item).into()).collect()
    }

    #[test]
    fn only_bare_invocation_and_help_are_accepted() {
        assert_eq!(parse_request(argv(&[])).unwrap(), Request::Reset);
        assert_eq!(parse_request(argv(&["-h"])).unwrap(), Request::Usage);
        assert_eq!(parse_request(argv(&["--help"])).unwrap(), Request::Usage);
    }

    #[test]
    fn any_other_argument_fails_closed_without_echoing_it() {
        // 含用户名/密码形态的参数也一律拒绝，且消息里绝不回显收到的 token。
        for arguments in [
            &["admin"][..],
            &["reset-admin"][..],
            &["--username"][..],
            &["--username", "admin"][..],
            &["--password", "hunter2hunter2"][..],
            &["-h", "--help"][..],
            &["--help", "extra"][..],
            &[""][..],
            &["-H"][..],
            &["--HELP"][..],
            &["管理员口令"][..],
        ] {
            let error = parse_request(argv(arguments)).expect_err("must reject");
            assert!(error.show_usage);
            assert!(
                !error.message.contains("hunter2hunter2"),
                "argument must not be echoed"
            );
            assert!(error.message.contains("takes no arguments"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_fail_closed_without_panic_or_echo() {
        use std::os::unix::ffi::OsStringExt as _;

        // Unix argv 是字节串：非法 UTF-8 正是 `std::env::args()` 会在 std 内部 `unwrap()`
        // panic 并把原始字节打进 stderr 的输入。这里必须走同一条 fail-closed 路径。
        let cases: Vec<Vec<u8>> = vec![
            b"FAKE_PASSWORD\xff".to_vec(),
            b"--password\xff\xfe".to_vec(),
            // 形状与合法 flag 相近，仅多出非法字节：绝不允许命中 Usage 分支。
            b"-h\xff".to_vec(),
            b"--help\xfe".to_vec(),
            vec![0xff, 0xfe, 0x80, 0x41],
        ];
        for bytes in cases {
            let rejected = OsString::from_vec(bytes.clone());
            let error = parse_request([rejected]).expect_err("非 UTF-8 参数必须被拒绝");
            assert!(error.show_usage);
            assert!(error.message.contains("takes no arguments"));
            // 固定脱敏文案：纯 ASCII，既不含凭据明文，也不含任何非法字节。
            assert!(error.message.is_ascii(), "错误消息必须只含固定 ASCII 文案");
            assert!(!error.message.contains("FAKE_PASSWORD"), "参数不得回显");
            // `Debug`/`Display` 同样不得带出原始字节（token 未被保留）。
            assert!(!format!("{error:?}").contains("FAKE_PASSWORD"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn help_flags_still_parse_when_arguments_are_os_bytes() {
        use std::os::unix::ffi::OsStringExt as _;

        // args_os 路径下合法 flag 仍精确匹配；非法字节仅存在于其他参数时同样拒绝。
        assert_eq!(
            parse_request([OsString::from_vec(b"-h".to_vec())]).unwrap(),
            Request::Usage
        );
        assert_eq!(
            parse_request([OsString::from_vec(b"--help".to_vec())]).unwrap(),
            Request::Usage
        );
        assert!(
            parse_request([
                OsString::from_vec(b"-h".to_vec()),
                OsString::from_vec(b"\xff".to_vec()),
            ])
            .is_err()
        );
    }

    #[tokio::test]
    async fn usage_request_returns_success_without_touching_the_database() {
        // 无 CONTROLLER_DATABASE_URL 也必须成功：Usage 分支不读配置、不连库。
        assert!(
            run(Request::Usage).await.is_ok(),
            "usage must not require configuration"
        );
    }

    #[test]
    fn configuration_comes_from_the_existing_env_contract_and_never_leaks_a_url() {
        // 不在单测里改动进程环境（会污染同进程其它测试）：两种结果都必须安全。
        // 缺失时的 fail-closed 退出由 tests/reset_admin_cli.rs 以真实子进程验证。
        let message = match load_config() {
            Ok(config) => {
                assert!(!config.database_url.is_empty());
                describe_error(&ControllerError::Config(
                    "CONTROLLER_DATABASE_URL is required".to_owned(),
                ))
            }
            Err(error) => describe_error(&error),
        };
        assert!(message.contains("CONTROLLER_DATABASE_URL"));
        assert!(!message.contains("://"), "连接串不得进入错误输出");
    }

    #[test]
    fn os_gates_are_checked_before_configuration_and_database_access() {
        let snapshot = admin_cli::collect_preconditions(false);
        let root_gate_message =
            describe_error(&admin_cli::verify_root_credentials(1, 0).unwrap_err());
        match open_output_channel() {
            // 只有 root 且三个标准流与 /dev/tty 全部可用才可能成功。
            Ok(_) => assert_eq!(
                (snapshot.real_uid, snapshot.effective_uid),
                (0, 0),
                "非 root 绝不允许通过门禁"
            ),
            Err(error) => {
                let message = describe_error(&error);
                if snapshot.real_uid != 0 || snapshot.effective_uid != 0 {
                    // 非 root 时最先且唯一的拒绝理由就是 root 门禁。
                    assert_eq!(message, root_gate_message);
                }
                // 任何情况下都不得推进到配置或数据库阶段。
                assert!(!message.contains("CONTROLLER_DATABASE_URL"), "{message}");
                assert!(!message.contains("database operation failed"), "{message}");
            }
        }
    }

    #[test]
    fn driver_and_crypto_errors_are_reduced_to_fixed_text() {
        // 驱动原文可能含主机/端口/用户：一律替换为固定文案。
        let database = describe_error(&ControllerError::Database(sqlx::Error::Io(
            std::io::Error::other("host db.internal:3306 user root"),
        )));
        assert!(!database.contains("db.internal"));
        assert!(!database.contains("root"));
        assert!(database.contains("CONTROLLER_DATABASE_URL"));
        assert_eq!(
            describe_error(&ControllerError::Crypto),
            "cryptographic operation failed"
        );
        // 其余门禁错误保持原样，便于运维定位（这些都是本 crate 的固定去敏文案）。
        assert_eq!(
            describe_error(&ControllerError::InvalidState),
            "invalid state for this operation"
        );
        assert_eq!(
            describe_error(&ControllerError::Config(
                "real and effective UID must both be root".to_owned()
            )),
            "configuration: real and effective UID must both be root"
        );
    }
}

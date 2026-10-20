//! `reset-admin` CLI 的真实子进程验收：只验证 fail-closed 行为，不接触任何数据库。
//!
//! 子进程的 stdin/stdout/stderr 均为管道（非交互 TTY），因此在任何 DB 访问之前就会被拒绝；
//! 这正是要断言的生产语义：非交互、无配置、非法参数一律非 0 退出，且输出零凭据。
//! 真实成功路径（本机 PTY + 隔离真实 DB）由运维门禁另行验收，不在本文件伪装。

use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};

fn reset_admin_binary() -> &'static str {
    env!("CARGO_BIN_EXE_reset-admin")
}

fn run<I, S>(args: I, clear_database_url: bool) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(reset_admin_binary());
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if clear_database_url {
        command.env_remove("CONTROLLER_DATABASE_URL");
    }
    command
        .output()
        .expect("reset-admin binary must be executable")
}

fn text(output: &Output) -> String {
    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    combined
}

#[test]
fn help_is_served_without_configuration_or_database_access() {
    for flag in ["-h", "--help"] {
        // 即使配置成一个不可能的地址，Usage 分支也必须成功且不连库。
        let output = Command::new(reset_admin_binary())
            .arg(flag)
            .env(
                "CONTROLLER_DATABASE_URL",
                "mysql://invalid.invalid:1/invalid",
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("reset-admin binary must be executable");
        assert!(
            output.status.success(),
            "usage must succeed: {}",
            text(&output)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("reset-admin"));
        assert!(stdout.contains("/dev/tty"));
        assert!(!stdout.contains("Temporary password"));
    }
}

#[test]
fn unexpected_arguments_fail_closed_without_echoing_them() {
    for args in [
        vec!["admin"],
        vec!["--username", "admin"],
        vec!["--password", "hunter2hunter2hunter2"],
        vec!["reset-admin"],
        vec!["--help", "extra"],
    ] {
        let output = run(&args, true);
        assert!(!output.status.success(), "must reject {args:?}");
        let combined = text(&output);
        assert!(
            combined.contains("takes no arguments"),
            "expected usage rejection, got: {combined}"
        );
        // 可能正是操作员误传密码的参数不得被回显。
        assert!(
            !combined.contains("hunter2hunter2hunter2"),
            "arguments must never be echoed"
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("Temporary password"),
            "nothing may be emitted on stdout"
        );
    }
}

#[test]
fn non_interactive_invocation_without_configuration_fails_closed() {
    // 无 TTY、无 CONTROLLER_DATABASE_URL：必须在任何写库动作之前非 0 退出。
    let no_arguments: &[&str] = &[];
    let output = run(no_arguments, true);
    assert!(
        !output.status.success(),
        "must fail closed: {}",
        text(&output)
    );
    let combined = text(&output);
    assert!(!combined.is_empty(), "an explanation must be printed");
    assert!(!combined.contains("Temporary password"));
    assert!(!combined.contains("argon2"));
    // 连接串与驱动原文不外泄。
    assert!(!combined.contains("mysql://"));
    assert!(String::from_utf8_lossy(&output.stdout).is_empty());
}

/// 以原始字节构造参数，绕开 Rust 源码的 UTF-8 约束，还原操作员在 shell 里
/// 误传非法字节口令的真实场景（例如 `reset-admin $'FAKE_PASSWORD\xff'`）。
#[cfg(unix)]
fn raw_os_bytes(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt as _;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

/// 断言子进程对非法输入的处理：退出码 1、stdout 静默、stderr 是纯 ASCII 固定文案。
#[cfg(unix)]
#[track_caller]
fn assert_fail_closed_no_leak(output: &Output, secret: &[u8], label: &str) {
    // 1. 退出码必须是固定失败码 1；101 意味着标准库在参数解码处 panic。
    assert_eq!(
        output.status.code(),
        Some(1),
        "{label}: 退出码必须为 1，绝不能是 panic 的 101"
    );
    // 2. stdout 完全静默。
    assert!(output.stdout.is_empty(), "{label}: stdout 必须为空");
    // 3. stderr 必须是可无损解码的 ASCII 固定 Usage/脱敏错误。
    let stderr = String::from_utf8(output.stderr.clone())
        .unwrap_or_else(|error| panic!("{label}: stderr 混入非 UTF-8 原始字节: {error}"));
    assert!(
        stderr.is_ascii(),
        "{label}: stderr 含非 ASCII 字节，等于回显外部输入"
    );
    assert!(
        stderr.contains("reset-admin: unexpected argument"),
        "{label}: 缺少固定脱敏错误行: {stderr}"
    );
    assert!(
        stderr.contains("takes no arguments"),
        "{label}: 缺少固定 Usage 拒绝文案: {stderr}"
    );
    assert!(
        stderr.contains("Constraints enforced fail-closed"),
        "{label}: 缺少 Usage 正文: {stderr}"
    );
    // 4. 绝不回显输入：明文字节、U+FFFD 替换符、panic 痕迹一律不允许。
    assert!(
        !output
            .stderr
            .windows(secret.len())
            .any(|window| window == secret),
        "{label}: stderr 回显了输入凭据字节"
    );
    assert!(!stderr.contains('\u{FFFD}'), "{label}: stderr 含替换符");
    for forbidden in ["panicked", "called `Result::unwrap()`", "RUST_BACKTRACE"] {
        assert!(
            !stderr.contains(forbidden),
            "{label}: stderr 含 panic 痕迹 {forbidden:?}: {stderr}"
        );
    }
    assert!(
        !stderr.contains("Temporary password"),
        "{label}: 不得输出任何凭据行"
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_byte_argument_fails_closed_without_panic_or_leak() {
    // 断言的漏洞本体：`std::env::args()` 会在标准库内部对非法 UTF-8 `unwrap()` panic，
    // 把携带原始字节的 panic 原文（含操作员误传的口令）打到 stderr，退出码 101。
    const SECRET: &[u8] = b"FAKE_PASSWORD";
    let cases: Vec<Vec<u8>> = vec![
        // 凭据明文 + 尾随非法字节。
        [SECRET, b"\xff".as_slice()].concat(),
        [SECRET, b"\xff\xfe\x80".as_slice()].concat(),
        // 形状贴近合法 flag，仅多出非法字节：绝不允许命中 Usage 分支。
        b"-h\xff".to_vec(),
        b"--help\xfe".to_vec(),
        // 纯非法字节，且不含任何 ASCII 明文。
        vec![0xff, 0xfe, 0x80],
    ];

    for bytes in cases {
        let label = format!("{bytes:02x?}");
        let output = run([raw_os_bytes(&bytes)], true);
        assert_fail_closed_no_leak(&output, SECRET, &label);
    }
}

#[cfg(unix)]
#[test]
fn multiple_non_utf8_arguments_fail_closed_without_panic_or_leak() {
    const SECRET: &[u8] = b"FAKE_PASSWORD";
    // 多个非法参数、以及合法 flag 与非法字节混排：一律同一条 fail-closed 路径。
    let cases: Vec<Vec<Vec<u8>>> = vec![
        vec![SECRET.to_vec(), vec![0xff, 0xfe]],
        vec![
            b"--password".to_vec(),
            [SECRET, b"\xff".as_slice()].concat(),
        ],
        vec![b"-h".to_vec(), vec![0xc3, 0x28, 0xa0]],
        vec![b"--help".to_vec(), b"extra\xff".to_vec()],
    ];

    for case in cases {
        let label = format!("{case:02x?}");
        let argv: Vec<std::ffi::OsString> = case.iter().map(|bytes| raw_os_bytes(bytes)).collect();
        let output = run(argv, true);
        assert_fail_closed_no_leak(&output, SECRET, &label);
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_configuration_value_is_not_echoed() {
    // 同一泄露类的相邻面：非 UTF-8 的 `CONTROLLER_DATABASE_URL` 也必须折叠成固定文案。
    const SECRET: &[u8] = b"FAKE_SECRET";
    let url = raw_os_bytes(b"mysql://root:FAKE_SECRET\xff@db.internal:3306/x");
    let output = Command::new(reset_admin_binary())
        .env("CONTROLLER_DATABASE_URL", url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("reset-admin binary must be executable");

    assert_eq!(output.status.code(), Some(1), "必须固定失败而非 panic");
    assert!(output.stdout.is_empty());
    assert!(
        !output
            .stderr
            .windows(SECRET.len())
            .any(|window| window == SECRET),
        "连接串中的凭据不得回显"
    );
    assert!(
        !output
            .stderr
            .windows(b"db.internal".len())
            .any(|w| w == b"db.internal"),
        "主机地址不得回显"
    );
    let stderr = String::from_utf8(output.stderr.clone()).expect("stderr 必须是纯 ASCII 固定文案");
    assert!(stderr.is_ascii());
    assert!(!stderr.contains("panicked"), "{stderr}");
}

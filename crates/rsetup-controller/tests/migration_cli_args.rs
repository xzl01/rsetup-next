//! `migrate-identity-test` CLI 参数解析的真实子进程回归测试。
//!
//! 要修复的漏洞：本 CLI 原先用 `std::env::args()` 读取命令行。Unix 下 argv 是任意非 NUL
//! 字节串，`args()` 会在标准库内部对非法 UTF-8 参数 `unwrap()` panic（退出码 101），并把
//! 携带原始字节的 panic 原文写入 stderr —— 等于把操作员误传的参数（可能正是口令）回显出来。
//!
//! 本文件只验证 fail-closed 语义：非法 UTF-8 一律走固定错误、退出码 1、绝不 panic、绝不回显
//! 输入字节；且在参数被拒绝时绝不读取 `CONTROLLER_*` 配置、绝不连接数据库。真实迁移路径不在
//! 本文件伪装，也不会被触发（本文件只喂非法/未知参数，并清空全部数据库环境变量）。

use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};

fn migrate_binary() -> &'static OsStr {
    OsStr::new(env!("CARGO_BIN_EXE_migrate-identity-test"))
}

/// 合成凭据标记：纯合成、非任何真实秘密，仅用于断言「绝不被回显」。
const SECRET: &[u8] = b"FAKE_CLI_SECRET_ONLY";

/// 以原始字节构造参数，还原操作员在 shell 里误传非法字节的真实场景。
#[cfg(unix)]
fn raw(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt as _;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

/// 运行子进程：清空全部 `CONTROLLER_*` 数据库环境变量，以证明参数被拒绝时不读配置、不连库。
fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(migrate_binary());
    command
        .args(args)
        .env_remove("CONTROLLER_DATABASE_URL")
        .env_remove("CONTROLLER_TEST_DATABASE_URL")
        .env_remove("CONTROLLER_TEST_ALLOW_DESTRUCTIVE")
        .env_remove("CONTROLLER_TEST_EXPECTED_DATABASE")
        .env_remove("CONTROLLER_TEST_BACKUP_REF")
        .env_remove("CONTROLLER_TEST_MIGRATION_ACK")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .output()
        .expect("migrate-identity-test binary must be executable")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// U+FFFD 的 UTF-8 编码：lossy 转换或 panic 原文都可能带出它，一律视为泄露痕迹。
const REPLACEMENT_CHAR: &[u8] = b"\xef\xbf\xbd";

/// 断言子进程对非法输入的处理：退出码固定为 1、stdout 静默、stderr 是纯 ASCII 固定错误，
/// 且 stdout/stderr 均不含合成凭据明文字节、原始非法字节、U+FFFD 替换符或任何 panic 痕迹。
#[track_caller]
fn assert_fail_closed_no_leak(output: &Output, label: &str) {
    // 1. 退出码必须是固定失败码 1；101 意味着标准库在 argv 解码处 panic。
    assert_eq!(
        output.status.code(),
        Some(1),
        "{label}: 退出码必须为 1，绝不能是 panic 的 101（stderr 见下）:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 2. 参数被拒绝：stdout 完全静默。
    assert!(output.stdout.is_empty(), "{label}: stdout 必须为空");
    // 3. stderr 必须是可无损解码的纯 ASCII 固定错误。
    let stderr = String::from_utf8(output.stderr.clone())
        .unwrap_or_else(|error| panic!("{label}: stderr 混入非 UTF-8 原始字节: {error}"));
    assert!(
        stderr.is_ascii(),
        "{label}: stderr 含非 ASCII 字节，等于回显外部输入"
    );
    // 4. 走的是固定错误分支（参数解析短路），而非读取配置或连接数据库。
    assert!(
        stderr.contains("explicit --mode") && stderr.contains("required"),
        "{label}: 缺少固定 --mode 拒绝文案: {stderr}"
    );
    assert!(
        !stderr.contains("CONTROLLER_TEST_DATABASE_URL"),
        "{label}: 不得进入配置/数据库阶段: {stderr}"
    );
    // 5. 绝不回显输入：合成明文字节、原始非法字节、U+FFFD 替换符一律不允许出现在两个输出流。
    for stream in [output.stdout.as_slice(), output.stderr.as_slice()] {
        assert!(
            !contains_bytes(stream, SECRET),
            "{label}: 输出回显了合成凭据明文字节"
        );
        assert!(
            stream.iter().all(|byte| *byte != 0xff),
            "{label}: 输出含原始非法字节 0xff"
        );
        assert!(
            !contains_bytes(stream, REPLACEMENT_CHAR),
            "{label}: 输出含 U+FFFD 替换符（lossy/panic 痕迹）"
        );
    }
    // 6. 不得出现任何 panic 痕迹。
    for forbidden in [
        "panicked",
        "called `Result::unwrap()`",
        "invalid utf-8",
        "RUST_BACKTRACE",
    ] {
        assert!(
            !stderr.contains(forbidden),
            "{label}: stderr 含 panic 痕迹 {forbidden:?}: {stderr}"
        );
    }
}

#[cfg(unix)]
#[test]
fn single_non_utf8_argument_fails_closed_without_panic_or_leak() {
    // 断言的漏洞本体：`std::env::args()` 会在标准库内部对非法 UTF-8 `unwrap()` panic，
    // 把携带原始字节的 panic 原文（含操作员误传的口令）打到 stderr，退出码 101。
    let cases: Vec<Vec<u8>> = vec![
        [SECRET, b"\xff".as_slice()].concat(),
        [SECRET, b"\xff\xfe\x80".as_slice()].concat(),
        // 形状贴近合法参数，仅多出非法字节：绝不允许命中合法 mode 分支。
        b"--mode\xfe".to_vec(),
        // 纯非法字节，不含任何 ASCII 明文。
        vec![0xff, 0xfe, 0x80],
    ];
    for bytes in cases {
        let label = format!("{bytes:02x?}");
        let output = run([raw(&bytes)]);
        assert_fail_closed_no_leak(&output, &label);
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_mode_value_and_multiple_arguments_fail_closed() {
    let cases: Vec<Vec<Vec<u8>>> = vec![
        // 非法字节作为独立参数。
        vec![SECRET.to_vec(), vec![0xff, 0xfe]],
        // `--mode <含非法字节的值>`：形似合法 mode 仍必须拒绝。
        vec![b"--mode".to_vec(), [SECRET, b"\xff".as_slice()].concat()],
        vec![b"--mode".to_vec(), b"upgrade\xfe".to_vec()],
        // 合法 mode 之后混入非法尾随参数：绝不允许命中 Upgrade 分支。
        vec![
            b"--mode".to_vec(),
            b"upgrade".to_vec(),
            vec![0xc3, 0x28, 0xa0],
        ],
    ];
    for case in cases {
        let label = format!("{case:02x?}");
        let argv: Vec<std::ffi::OsString> = case.iter().map(|bytes| raw(bytes)).collect();
        let output = run(argv);
        assert_fail_closed_no_leak(&output, &label);
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_argv0_does_not_panic() {
    // `std::env::args()` 的 `.skip(1)` 会先解码 argv[0]：若 argv[0] 含非法字节即 panic。
    // `args_os()` 永不 panic，且 argv[0] 被跳过、绝不参与比较或回显。
    use std::os::unix::process::CommandExt as _;
    let output = {
        let mut command = Command::new(migrate_binary());
        command
            .arg0(raw(b"\xff\xfe migrate-identity-test"))
            .args(["--mode", "bogus"])
            .env_remove("CONTROLLER_DATABASE_URL")
            .env_remove("CONTROLLER_TEST_DATABASE_URL")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
            .output()
            .expect("migrate-identity-test binary must be executable")
    };
    assert_fail_closed_no_leak(&output, "arg0-non-utf8");
}

#[cfg(unix)]
#[test]
fn help_and_ascii_invalid_arguments_fail_closed_without_database_access() {
    // 普通 ASCII 非法参数与帮助参数：一律走同一条固定错误、退出码 1，且不读配置、不连库。
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["--mode", "bogus"],
        vec!["--mode"],
        vec!["--mode", "upgrade", "extra"],
        vec![],
    ] {
        let output = run(&args);
        assert!(!output.status.success(), "must reject {args:?}");
        assert_eq!(output.status.code(), Some(1), "must exit 1 for {args:?}");
        assert!(
            output.stdout.is_empty(),
            "stdout must stay empty for {args:?}"
        );
        let stderr = String::from_utf8(output.stderr.clone()).expect("stderr must be ASCII");
        assert!(
            stderr.is_ascii(),
            "stderr must be fixed ASCII text for {args:?}"
        );
        assert!(
            stderr.contains("explicit --mode") && stderr.contains("required"),
            "expected fixed --mode rejection for {args:?}: {stderr}"
        );
    }
}

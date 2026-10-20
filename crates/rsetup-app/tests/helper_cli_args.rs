//! `rsetup-next-helper` 参数解析的真实子进程回归测试。
//!
//! 要修复的漏洞：helper 原先用 `std::env::args()` 读取命令行。Unix 下 argv 是任意非 NUL 字节
//! 串，`args()` 会在标准库内部对非法 UTF-8 参数 `unwrap()` panic（退出码 101），并把携带原始
//! 字节的 panic 原文写入 stderr —— 等于把操作员/上游误传的参数回显出来，且绕过了 JSON 错误信封。
//!
//! 本文件只验证 fail-closed 语义：非法 UTF-8 与未知参数一律走 JSON 错误信封、退出码 1、绝不
//! panic、绝不回显输入字节。测试只喂非法/未知参数，绝不构造任何合法的写操作请求（overlay /
//! install / flash / 迁移等），因此不会触发任何真实特权动作；且 helper 的非 root 门禁先于参数
//! 解析，子进程即便以 root 运行也会停在固定错误处。

use std::ffi::OsStr;
use std::process::{Command, Output, Stdio};

fn helper_binary() -> &'static OsStr {
    OsStr::new(env!("CARGO_BIN_EXE_rsetup-next-helper"))
}

/// 合成凭据标记：纯合成、非任何真实秘密，仅用于断言「绝不被回显」。
const SECRET: &[u8] = b"FAKE_CLI_SECRET_ONLY";

#[cfg(unix)]
fn raw(bytes: &[u8]) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt as _;
    std::ffi::OsString::from_vec(bytes.to_vec())
}

fn run<I, S>(args: I) -> Output
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(helper_binary());
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .output()
        .expect("rsetup-next-helper binary must be executable")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

const REPLACEMENT_CHAR: &[u8] = b"\xef\xbf\xbd";

/// 断言 helper 对非法输入的处理：退出码固定为 1、stdout 是 `helper_failed` 的 JSON 错误信封、
/// 两个输出流均不含合成明文字节、原始非法字节、U+FFFD 替换符或任何 panic 痕迹。
#[cfg(unix)]
#[track_caller]
fn assert_fail_closed_no_leak(output: &Output, label: &str) {
    // 1. 退出码必须是固定失败码 1；101 意味着标准库在 argv 解码处 panic。
    assert_eq!(
        output.status.code(),
        Some(1),
        "{label}: 退出码必须为 1，绝不能是 panic 的 101（stderr 见下）:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 2. JSON 错误信封契约保持：stdout 是可解析的 {error:{code:helper_failed,...}}。
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{label}: stdout 必须是 JSON 错误信封: {error}"));
    assert_eq!(
        envelope["error"]["code"].as_str(),
        Some("helper_failed"),
        "{label}: 错误信封 code 必须保持 helper_failed"
    );
    // 3. 绝不回显输入：合成明文、原始非法字节、U+FFFD 替换符一律不允许出现在两个输出流。
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
    // 4. stderr 必须是可无损解码的纯 ASCII，且不得含 panic 痕迹。
    let stderr = String::from_utf8(output.stderr.clone())
        .unwrap_or_else(|error| panic!("{label}: stderr 混入非 UTF-8 原始字节: {error}"));
    assert!(stderr.is_ascii(), "{label}: stderr 含非 ASCII 字节");
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
    // 5. 信封 message 与 stderr 均不得含 lossy 替换符。
    let message = envelope["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains('\u{FFFD}'),
        "{label}: 信封 message 含替换符"
    );
    assert!(!stderr.contains('\u{FFFD}'), "{label}: stderr 含替换符");
}

#[cfg(unix)]
#[test]
fn single_non_utf8_argument_fails_closed_without_panic_or_leak() {
    let cases: Vec<Vec<u8>> = vec![
        [SECRET, b"\xff".as_slice()].concat(),
        [SECRET, b"\xff\xfe\x80".as_slice()].concat(),
        b"overlays-inspect\xfe".to_vec(),
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
fn multiple_non_utf8_arguments_fail_closed_without_panic_or_leak() {
    // 多个非法参数、合法 flag 与非法字节混排：一律同一条 fail-closed 路径。
    // 注意：所有含合法动词的用例，其业务参数位一律塞入非法字节，使解码在任何真实动作之前失败。
    let cases: Vec<Vec<Vec<u8>>> = vec![
        vec![SECRET.to_vec(), vec![0xff, 0xfe]],
        vec![
            b"action".to_vec(),
            [SECRET, b"\xff".as_slice()].concat(),
            b"--confirmed".to_vec(),
        ],
        vec![
            b"overlays-apply".to_vec(),
            b"uart.dtbo".to_vec(),
            vec![0xff, 0xfe],
            b"--confirmed".to_vec(),
        ],
        vec![b"led-rgb".to_vec(), vec![0xc3, 0x28, 0xa0]],
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
    // `.skip(1)` 之前 argv[0] 也需解码：非法 argv[0] 曾让 `args()` 直接 panic。
    use std::os::unix::process::CommandExt as _;
    let output = {
        let mut command = Command::new(helper_binary());
        command
            .arg0(raw(b"\xff\xfe rsetup-next-helper"))
            .args(["--help"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
            .output()
            .expect("rsetup-next-helper binary must be executable")
    };
    assert_fail_closed_no_leak(&output, "arg0-non-utf8");
}

#[test]
fn ascii_invalid_arguments_keep_json_envelope_and_exit_code() {
    // 普通 ASCII 非法/帮助参数：退出码 1、stdout 仍是合法 JSON 错误信封，行为契约不变。
    for args in [
        vec!["--help"],
        vec!["-h"],
        vec!["shell", "id"],
        vec!["overlays-inspect", "/arbitrary/path"],
        vec![],
    ] {
        let output = run(&args);
        assert!(!output.status.success(), "must reject {args:?}");
        assert_eq!(output.status.code(), Some(1), "must exit 1 for {args:?}");
        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("{args:?}: stdout 必须是 JSON 错误信封: {error}"));
        assert_eq!(
            envelope["error"]["code"].as_str(),
            Some("helper_failed"),
            "错误信封 code 必须保持 helper_failed: {args:?}"
        );
    }
}

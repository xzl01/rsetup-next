//! `migrate-identity-test`：仅在**隔离、可丢弃的开发库**上演练身份迁移的测试专用 CLI。
//!
//! 参数解析必须使用 `std::env::args_os()` 而不是 `std::env::args()`：Unix 下 argv 是任意非 NUL
//! 字节串，`args()` 会在标准库内部对非法 UTF-8 参数 `unwrap()` panic（退出码 101），并把携带
//! 原始字节的 panic 原文写入 stderr —— 等于把操作员误传的输入（可能正是口令）回显出来。这里只用
//! `args_os()` 取得 `OsString`，对固定的 `--mode <mode>` 组合做**字节级精确比较**，绝不做 UTF-8
//! 转换、绝不回显任何收到的 token；参数被拒绝时以固定错误退出，绝不读取配置或连接数据库。

use std::ffi::OsStr;

use rsetup_controller::{TestMigrationConfig, TestMigrationMode, run_identity_test_migration};

/// 参数被拒绝时的固定错误文案：纯 ASCII，绝不含任何收到的 token。保持与原实现完全一致。
const INVALID_ARGUMENTS: &str =
    "explicit --mode upgrade|fixture-v1|fixture-partial-v1|fixture-v2 required";

/// 解析命令行：只接受恰好 `--mode <upgrade|fixture-v1|fixture-partial-v1|fixture-v2>` 两个参数，
/// 其余（含非法 UTF-8、缺参、尾随参数、`--help`）一律返回固定错误。
///
/// 入参是 `OsStr` 而非 `String`：仅做字节级精确比较，绝不 `Debug`/`Display`/lossy 收到的 token，
/// 因此非法 UTF-8 既不 panic，也不会被回显。
fn parse_mode<I, T>(args: I) -> Result<TestMigrationMode, &'static str>
where
    I: IntoIterator<Item = T>,
    T: AsRef<OsStr>,
{
    let tokens: Vec<T> = args.into_iter().collect();
    match tokens.as_slice() {
        [flag, value] if flag.as_ref() == OsStr::new("--mode") => {
            let value: &OsStr = value.as_ref();
            if value == OsStr::new("upgrade") {
                Ok(TestMigrationMode::Upgrade)
            } else if value == OsStr::new("fixture-v1") {
                Ok(TestMigrationMode::FixtureV1)
            } else if value == OsStr::new("fixture-partial-v1") {
                Ok(TestMigrationMode::FixturePartialV1)
            } else if value == OsStr::new("fixture-v2") {
                Ok(TestMigrationMode::FixtureV2)
            } else {
                Err(INVALID_ARGUMENTS)
            }
        }
        _ => Err(INVALID_ARGUMENTS),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 必须使用 args_os()：args() 在非法 UTF-8 时于标准库内部 unwrap() panic（退出码 101），把原始
    // 字节打进 stderr。解析失败即以固定错误退出，绝不读取配置或连接数据库。
    let mode = parse_mode(std::env::args_os().skip(1))?;
    // Never log the config, environment, database URL, credentials, or administrator secret.
    let config = TestMigrationConfig::from_test_env()?;
    run_identity_test_migration(&config, mode).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(|item| OsString::from(*item)).collect()
    }

    #[test]
    fn accepts_only_the_four_fixed_modes() {
        assert!(matches!(
            parse_mode(argv(&["--mode", "upgrade"])).unwrap(),
            TestMigrationMode::Upgrade
        ));
        assert!(matches!(
            parse_mode(argv(&["--mode", "fixture-v1"])).unwrap(),
            TestMigrationMode::FixtureV1
        ));
        assert!(matches!(
            parse_mode(argv(&["--mode", "fixture-partial-v1"])).unwrap(),
            TestMigrationMode::FixturePartialV1
        ));
        assert!(matches!(
            parse_mode(argv(&["--mode", "fixture-v2"])).unwrap(),
            TestMigrationMode::FixtureV2
        ));
    }

    #[test]
    fn any_other_shape_returns_the_fixed_message_without_echo() {
        for arguments in [
            &[][..],
            &["--mode"][..],
            &["--mode", "bogus"][..],
            &["--mode", "upgrade", "extra"][..],
            &["-h"][..],
            &["--help"][..],
            &["mode", "upgrade"][..],
            &["--mode", "UPGRADE"][..],
        ] {
            let error = parse_mode(argv(arguments)).expect_err("must reject");
            assert_eq!(error, INVALID_ARGUMENTS);
            assert!(error.is_ascii(), "固定错误必须纯 ASCII");
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_arguments_reject_without_panic_or_echo() {
        use std::os::unix::ffi::OsStringExt as _;

        // 这些正是 `std::env::args()` 会在 std 内部 unwrap() panic 并把原始字节打进 stderr 的输入。
        let cases: Vec<Vec<u8>> = vec![
            b"FAKE_CLI_SECRET_ONLY\xff".to_vec(),
            b"--mode\xfe".to_vec(),
            b"upgrade\xff".to_vec(),
            vec![0xff, 0xfe, 0x80],
        ];
        for bytes in cases {
            let token = OsString::from_vec(bytes.clone());
            // 单个非法参数。
            let error = parse_mode([token.clone()]).expect_err("非 UTF-8 参数必须被拒绝");
            assert_eq!(error, INVALID_ARGUMENTS);
            // `--mode <非法字节>`：形似合法 mode 仍必须拒绝。
            let error = parse_mode([OsString::from("--mode"), token])
                .expect_err("非 UTF-8 mode 值必须被拒绝");
            assert_eq!(error, INVALID_ARGUMENTS);
            // 固定脱敏文案：纯 ASCII，既不含合成凭据明文，也不含任何非法字节。
            assert!(error.is_ascii(), "固定错误必须纯 ASCII");
            assert!(!error.contains("FAKE_CLI_SECRET_ONLY"), "参数不得回显");
            assert!(!error.contains('\u{FFFD}'), "不得含 lossy 替换符");
        }
    }
}

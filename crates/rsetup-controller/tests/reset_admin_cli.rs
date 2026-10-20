//! `reset-admin` CLI 的真实子进程验收：只验证 fail-closed 行为，不接触任何数据库。
//!
//! 子进程的 stdin/stdout/stderr 均为管道（非交互 TTY），因此在任何 DB 访问之前就会被拒绝；
//! 这正是要断言的生产语义：非交互、无配置、非法参数一律非 0 退出，且输出零凭据。
//! 真实成功路径（本机 PTY + 隔离真实 DB）由运维门禁另行验收，不在本文件伪装。

use std::process::{Command, Output, Stdio};

fn reset_admin_binary() -> &'static str {
    env!("CARGO_BIN_EXE_reset-admin")
}

fn run(args: &[&str], clear_database_url: bool) -> Output {
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
    let output = run(&[], true);
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

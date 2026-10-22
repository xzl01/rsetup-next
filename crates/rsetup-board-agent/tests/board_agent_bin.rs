//! amd64 本地板端代理二进制冒烟：只启动 `--once`，验证三条只读动作返回合法 JSON。
//! 不建立网络、不触发重启、不连接真实控制器。

use std::process::Command;

#[test]
fn once_mode_returns_valid_capabilities_status_and_clock() {
    let journal =
        std::env::temp_dir().join(format!("rsetup-board-agent-bin-{}", uuid::Uuid::new_v4()));
    let output = Command::new(env!("CARGO_BIN_EXE_rsetup-board-agent"))
        .arg("--once")
        .arg("--journal-dir")
        .arg(&journal)
        .output()
        .expect("spawn board agent");
    assert!(
        output.status.success(),
        "board agent --once failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("--once must print one JSON object");
    for action in [
        "device.capabilities.get",
        "device.status.get",
        "device.clock.get",
    ] {
        assert!(value.get(action).is_some(), "missing {action} in {value}");
    }
    assert_eq!(value["device.capabilities.get"]["schema_version"], 1);
    assert!(value["device.status.get"]["sample_seq"].as_u64().unwrap() >= 1);
    assert_eq!(
        value["device.capabilities.get"]["durable_task_journal"],
        true
    );
    let _ = std::fs::remove_dir_all(&journal);
}

#[test]
fn unknown_mode_is_rejected_without_echoing_arguments() {
    let output = Command::new(env!("CARGO_BIN_EXE_rsetup-board-agent"))
        .arg("--definitely-not-a-mode")
        .output()
        .expect("spawn board agent");
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("--definitely-not-a-mode"));
}

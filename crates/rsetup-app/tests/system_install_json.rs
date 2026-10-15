//! The JSON surfaces must leave exactly one document on stdout.
//!
//! The install preview and the typed-confirmation prompt are a conversation with
//! a person, so in JSON mode they belong on stderr; automation parses stdout.
//! Before this, the preview was written to stdout first and the result appended
//! after it, so `JSON.parse` failed on a command that exited 0.

use std::process::Command;

/// A minimal MBR image: the planner refuses a payload that is not a disk image,
/// so the fixture has to be one.
fn fixture_image() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "rsetup-install-json-{}-{:?}.img",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut bytes = vec![0u8; 512];
    bytes[446 + 4] = 0x83;
    bytes[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
    bytes[446 + 12..446 + 16].copy_from_slice(&1024u32.to_le_bytes());
    bytes[510] = 0x55;
    bytes[511] = 0xAA;
    std::fs::write(&path, &bytes).expect("write the fixture image");
    path
}

fn install(json: bool, image: &std::path::Path) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rsetup-next"));
    command.args(["--demo", "system", "install", "--target", "sda"]);
    command.arg("--image").arg(image);
    command.args(["--acknowledge", "sda"]);
    if json {
        command.arg("--json");
    }
    command.output().expect("run the CLI")
}

#[test]
fn json_mode_prints_one_document_on_stdout() {
    let image = fixture_image();
    let output = install(true, &image);
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8");
    let stderr = String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8");
    assert!(output.status.success(), "stderr: {stderr}");

    let trimmed = stdout.trim();
    assert!(
        trimmed.starts_with('{') && trimmed.ends_with('}'),
        "stdout must be one JSON document:\n{stdout}"
    );
    // The exact defect: a human preview ahead of the document.
    assert!(
        !stdout.contains("System image write preview"),
        "the preview must not be on stdout:\n{stdout}"
    );
    // Exactly one document: a second opening brace outside of a string would mean
    // two values were concatenated.
    assert_eq!(
        trimmed.matches("\"planToken\"").count(),
        1,
        "stdout carries one plan, not a preview plus a result:\n{stdout}"
    );
    // The operator still sees the preview; it moved rather than disappeared.
    assert!(
        stderr.contains("System image write preview"),
        "the preview belongs on stderr in JSON mode:\n{stderr}"
    );
    let _ = std::fs::remove_file(&image);
}

#[test]
fn human_mode_keeps_the_preview_on_stdout() {
    let image = fixture_image();
    let output = install(false, &image);
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8");
    assert!(output.status.success());
    assert!(
        stdout.contains("System image write preview"),
        "without --json the preview stays on stdout:\n{stdout}"
    );
    let _ = std::fs::remove_file(&image);
}

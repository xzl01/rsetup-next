//! Task 2: 身份文件与 Ed25519 签名密钥测试（protocol_spec.md §3.3、计划 Task 2）。
//!
//! 口径：每个测试只断言一件事；所有密钥由 CSPRNG 现场生成，仓库与测试目录
//! 都不留任何固定/生产私钥。测试目录取 `temp_dir()` 下进程内唯一的 0700 目录。

use std::ffi::CString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use ring::rand::SystemRandom;
use ring::signature::Ed25519KeyPair;
use rsetup_tunnel::identity::{IdentityError, IdentityKey};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// 测试独占的 0700 目录；`Drop` 时删除。
struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!(
            "rsetup-tunnel-identity-{}-{name}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("create test dir");
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        Self { path: p }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn random_pkcs8() -> Vec<u8> {
    Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .expect("generate pkcs8")
        .as_ref()
        .to_vec()
}

fn put_key_file(dir: &Path, name: &str, bytes: &[u8], mode: u32) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write key file");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod key file");
    path
}

fn dir_mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("stat dir")
        .permissions()
        .mode()
        & 0o777
}

fn entries_of(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------- 基本行为

#[test]
fn generates_new_identity_with_0600_and_reuses_it() {
    let dir = TestDir::new("generate");
    let path = dir.path.join("identity.key");

    let id = IdentityKey::load_or_generate(&path).expect("generate");
    assert_eq!(id.public_key().len(), 32);
    assert_eq!(
        fs::symlink_metadata(&path)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "私钥文件必须恰为 0600"
    );

    let reloaded = IdentityKey::load_or_generate(&path).expect("reload");
    assert_eq!(id.public_key(), reloaded.public_key(), "已有身份必须复用");
}

#[test]
fn sign_produces_real_ed25519_verifiable_by_public_key() {
    let dir = TestDir::new("sign");
    let path = dir.path.join("identity.key");
    let id = IdentityKey::load_or_generate(&path).expect("generate");

    let msg = b"rsetup-next/client-auth/v3";
    let sig = id.sign(msg);
    assert_eq!(sig.len(), 64);
    rsetup_protocol::ed25519::verify_ed25519(msg, &sig, &id.public_key())
        .expect("真实 Ed25519 验签必须通过");
    assert!(
        rsetup_protocol::ed25519::verify_ed25519(b"other", &sig, &id.public_key()).is_err(),
        "签名必须与消息绑定"
    );
}

#[test]
fn from_pkcs8_matches_file_loaded_identity() {
    let bytes = random_pkcs8();
    let direct = IdentityKey::from_pkcs8(&bytes).expect("from_pkcs8");
    let dir = TestDir::new("from_pkcs8");
    let path = put_key_file(&dir.path, "identity.key", &bytes, 0o600);
    let loaded = IdentityKey::load_or_generate(&path).expect("load");
    assert_eq!(direct.public_key(), loaded.public_key());
}

#[test]
fn from_pkcs8_rejects_garbage_and_truncated_der() {
    assert!(matches!(
        IdentityKey::from_pkcs8(b"not a pkcs8 document"),
        Err(IdentityError::InvalidFormat(_))
    ));
    assert!(matches!(
        IdentityKey::from_pkcs8(&[]),
        Err(IdentityError::InvalidFormat(_))
    ));
    let full = random_pkcs8();
    assert!(matches!(
        IdentityKey::from_pkcs8(&full[..full.len() - 8]),
        Err(IdentityError::InvalidFormat(_))
    ));
}

// ---------------------------------------------------------------- 安全审计

#[test]
fn rejects_world_readable_key_file() {
    let dir = TestDir::new("reject_0644");
    let path = dir.path.join("identity.key");
    IdentityKey::load_or_generate(&path).expect("generate");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod 0644");
    let err = IdentityKey::load_or_generate(&path).expect_err("0644 必须拒绝");
    assert!(
        matches!(err, IdentityError::InsecurePermissions(_)),
        "实际错误: {err:?}"
    );
}

#[test]
fn rejects_group_readable_key_file() {
    let dir = TestDir::new("reject_0640");
    let path = dir.path.join("identity.key");
    IdentityKey::load_or_generate(&path).expect("generate");

    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).expect("chmod 0640");
    let err = IdentityKey::load_or_generate(&path).expect_err("0640 必须拒绝");
    assert!(
        matches!(err, IdentityError::InsecurePermissions(_)),
        "实际错误: {err:?}"
    );
}

#[test]
fn rejects_insecure_parent_dir_mode() {
    let dir = TestDir::new("reject_dir_0755");
    let path = dir.path.join("identity.key");
    IdentityKey::load_or_generate(&path).expect("generate");

    fs::set_permissions(&dir.path, fs::Permissions::from_mode(0o755)).expect("chmod 0755");
    let err = IdentityKey::load_or_generate(&path).expect_err("0755 目录必须拒绝");
    assert!(
        matches!(err, IdentityError::InsecurePermissions(_)),
        "实际错误: {err:?}"
    );
}

#[test]
fn rejects_symlinked_key_file() {
    let dir = TestDir::new("reject_symlink_file");
    let real = dir.path.join("real.key");
    let link = dir.path.join("link.key");
    IdentityKey::load_or_generate(&real).expect("generate");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");

    let err = IdentityKey::load_or_generate(&link).expect_err("符号链接必须拒绝");
    assert!(
        matches!(err, IdentityError::SymlinkNotAllowed),
        "实际错误: {err:?}"
    );
    assert!(real.is_file(), "拒绝符号链接时不得动原身份文件");
}

#[test]
fn rejects_symlinked_parent_dir() {
    let dir = TestDir::new("reject_symlink_dir");
    let real_sub = dir.path.join("real");
    fs::create_dir(&real_sub).expect("mkdir");
    fs::set_permissions(&real_sub, fs::Permissions::from_mode(0o700)).expect("chmod");
    let link_sub = dir.path.join("link");
    std::os::unix::fs::symlink(&real_sub, &link_sub).expect("symlink");

    let err = IdentityKey::load_or_generate(link_sub.join("identity.key"))
        .expect_err("父目录符号链接必须拒绝");
    assert!(
        matches!(err, IdentityError::SymlinkNotAllowed),
        "实际错误: {err:?}"
    );
}

#[test]
fn rejects_corrupt_identity_format() {
    let dir = TestDir::new("corrupt");
    let path = put_key_file(
        &dir.path,
        "identity.key",
        b"corrupt bytes, not pkcs8",
        0o600,
    );

    let err = IdentityKey::load_or_generate(&path).expect_err("坏格式必须拒绝");
    assert!(
        matches!(err, IdentityError::InvalidFormat(_)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        fs::read(&path).expect("read back"),
        b"corrupt bytes, not pkcs8"
    );
}

#[test]
fn rejects_empty_identity_file() {
    let dir = TestDir::new("empty");
    let path = put_key_file(&dir.path, "identity.key", b"", 0o600);
    let err = IdentityKey::load_or_generate(&path).expect_err("空文件必须拒绝");
    assert!(
        matches!(err, IdentityError::InvalidFormat(_)),
        "实际错误: {err:?}"
    );
}

#[test]
fn rejects_oversized_identity_file_as_invalid() {
    let dir = TestDir::new("oversized");
    let path = put_key_file(&dir.path, "identity.key", &vec![0u8; 64 * 1024], 0o600);
    let err = IdentityKey::load_or_generate(&path).expect_err("超大文件必须拒绝");
    assert!(
        matches!(err, IdentityError::InvalidFormat(_)),
        "实际错误: {err:?}"
    );
}

#[test]
fn rejects_directory_target_without_reading_it() {
    let dir = TestDir::new("dir_target");
    let target = dir.path.join("identity.key");
    fs::create_dir(&target).expect("mkdir as target");

    let err = IdentityKey::load_or_generate(&target).expect_err("目录目标必须拒绝");
    assert!(
        matches!(err, IdentityError::InvalidFormat(_)),
        "实际错误: {err:?}"
    );
}

#[test]
fn never_blocks_when_target_is_a_fifo() {
    let dir = TestDir::new("fifo_target");
    let target = dir.path.join("identity.key");
    let c_path = CString::new(target.to_str().expect("utf8 path")).expect("cstr");
    assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

    let (tx, rx) = mpsc::channel();
    let spawned = target.clone();
    std::thread::spawn(move || {
        let _ = tx.send(IdentityKey::load_or_generate(&spawned).map(|k| k.public_key()));
    });

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(res) => assert!(
            matches!(res, Err(IdentityError::InvalidFormat(_))),
            "命名管道目标必须直接拒绝，实际: {res:?}"
        ),
        Err(_) => panic!("load_or_generate 打开了 FIFO 目标并阻塞在 read 上"),
    }
}

// ---------------------------------------------------------------- 原子落盘

#[test]
fn creates_missing_parent_dirs_as_0700() {
    let dir = TestDir::new("create_parents");
    let nested = dir.path.join("a").join("b").join("c");
    let path = nested.join("identity.key");

    IdentityKey::load_or_generate(&path).expect("generate into missing dirs");

    assert_eq!(dir_mode(&nested), 0o700, "叶子目录必须 0700");
    assert_eq!(
        dir_mode(&dir.path.join("a").join("b")),
        0o700,
        "中间目录必须 0700"
    );
    assert_eq!(dir_mode(&dir.path.join("a")), 0o700, "中间目录必须 0700");
}

#[test]
fn concurrent_first_start_converges_to_one_identity() {
    let dir = TestDir::new("concurrent");
    let path = dir.path.join("identity.key");

    let mut handles = Vec::new();
    for _ in 0..8 {
        let p = path.clone();
        handles.push(std::thread::spawn(move || {
            IdentityKey::load_or_generate(&p)
        }));
    }

    let mut keys = Vec::new();
    for h in handles {
        keys.push(
            h.join()
                .expect("join")
                .expect("并发首启不得失败")
                .public_key(),
        );
    }
    for k in &keys[1..] {
        assert_eq!(k, &keys[0], "首启并发必须收敛到同一身份");
    }
    assert_eq!(entries_of(&dir.path), vec!["identity.key".to_string()]);
}

#[test]
fn generation_never_overwrites_an_existing_file() {
    let dir = TestDir::new("no_overwrite");
    let path = dir.path.join("identity.key");
    let existing = random_pkcs8();
    fs::write(&path, &existing).expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");

    let id = IdentityKey::load_or_generate(&path).expect("load existing");
    assert_eq!(
        id.public_key(),
        IdentityKey::from_pkcs8(&existing)
            .expect("expect")
            .public_key(),
        "已有身份绝不能被覆盖"
    );
    assert_eq!(fs::read(&path).expect("read back"), existing);
}

#[test]
fn leaves_no_temporary_files_behind() {
    let dir = TestDir::new("no_tmp_leftover");
    let path = dir.path.join("identity.key");
    IdentityKey::load_or_generate(&path).expect("generate");
    assert_eq!(
        entries_of(&dir.path),
        vec!["identity.key".to_string()],
        "不得遗留临时文件"
    );
}

// ---------------------------------------------------------------- 去敏

#[test]
fn debug_and_errors_never_echo_private_key_or_path() {
    let dir = TestDir::new("sanitized");
    let path = dir.path.join("very-secret-identity.key");
    let id = IdentityKey::load_or_generate(&path).expect("generate");

    let secret_hex = hex::encode(fs::read(&path).expect("read key"));
    let dir_str = dir.path.to_str().expect("utf8").to_owned();
    let file_str = "very-secret-identity.key".to_string();

    let mut samples = vec![format!("{id:?}")];

    // 不安全权限
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("chmod");
    let err = IdentityKey::load_or_generate(&path).expect_err("must fail");
    samples.extend([format!("{err:?}"), err.to_string()]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod back");

    // 坏格式
    fs::write(&path, b"garbage").expect("write garbage");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("chmod");
    let err = IdentityKey::load_or_generate(&path).expect_err("must fail");
    samples.extend([format!("{err:?}"), err.to_string()]);

    // I/O 失败（目标是一个目录）
    fs::remove_file(&path).expect("rm");
    fs::create_dir(&path).expect("mkdir");
    let err = IdentityKey::load_or_generate(&path).expect_err("must fail");
    samples.extend([format!("{err:?}"), err.to_string()]);

    // 符号链接
    let link = dir.path.join("link.key");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    let err = IdentityKey::load_or_generate(&link).expect_err("must fail");
    samples.extend([format!("{err:?}"), err.to_string()]);

    // 父目录不安全
    fs::set_permissions(&dir.path, fs::Permissions::from_mode(0o777)).expect("chmod dir");
    let err = IdentityKey::load_or_generate(&path).expect_err("must fail");
    samples.extend([format!("{err:?}"), err.to_string()]);
    fs::set_permissions(&dir.path, fs::Permissions::from_mode(0o700)).expect("chmod dir back");

    for sample in &samples {
        assert!(
            !sample.contains(&secret_hex),
            "输出泄漏了私钥文件内容: {sample}"
        );
        assert!(!sample.contains(&dir_str), "输出泄漏了绝对路径: {sample}");
        assert!(
            !sample.contains(&file_str),
            "输出泄漏了敏感文件名: {sample}"
        );
    }
}

#[test]
fn identity_error_variants_carry_no_paths() {
    let cases: Vec<IdentityError> = vec![
        IdentityError::SymlinkNotAllowed,
        IdentityError::WrongOwner {
            expected: 0,
            actual: 1000,
        },
        IdentityError::InsecurePermissions("mode 0644".to_string()),
        IdentityError::InvalidFormat("bad pkcs8".to_string()),
        IdentityError::Crypto("primitive".to_string()),
        IdentityError::Io(std::io::ErrorKind::PermissionDenied),
    ];
    for case in cases {
        let text = format!("{case:?} {case} ");
        assert!(!text.contains('/'), "错误文本不得含路径: {text}");
    }
}

// ---------------------------------------------------------------- 特殊 umask

/// 子进程协议：父进程只在**子进程**里把 umask 改成 0200，
/// 绝不改本测试进程的 umask（Rust 测试并行执行，全局 umask 会污染其它用例）。
const UMASK_CHILD_ENV: &str = "RSETUP_IDENTITY_UMASK_CHILD";

#[test]
fn new_identity_stays_0600_under_hostile_umask() {
    if std::env::var_os(UMASK_CHILD_ENV).is_some() {
        // 子进程：先改本进程 umask，再走「新建 → 重载」全流程。
        // SAFETY: umask(2) 只改本进程的文件模式创建掩码，无参数、不失败。
        unsafe { libc::umask(0o200) };
        let dir = TestDir::new("umask_child");
        let path = dir.path.join("nested").join("identity.key");
        let code = match IdentityKey::load_or_generate(&path) {
            Ok(id) => {
                let mode = fs::symlink_metadata(&path)
                    .map(|m| m.permissions().mode() & 0o777)
                    .unwrap_or(0o7777);
                let reloaded = IdentityKey::load_or_generate(&path)
                    .map(|k| k.public_key())
                    .ok();
                let dir_mode = fs::symlink_metadata(path.parent().expect("parent"))
                    .map(|m| m.permissions().mode() & 0o777)
                    .unwrap_or(0o7777);
                eprintln!(
                    "child: key mode={mode:#o} dir mode={dir_mode:#o} reload_ok={reloaded:?}"
                );
                if mode == 0o600 && dir_mode == 0o700 && reloaded == Some(id.public_key()) {
                    0
                } else {
                    2
                }
            }
            Err(err) => {
                eprintln!("child: generate failed: {err}");
                3
            }
        };
        std::process::exit(code);
    }

    let exe = std::env::current_exe().expect("current_exe");
    let output = std::process::Command::new(&exe)
        .arg("new_identity_stays_0600_under_hostile_umask")
        .arg("--exact")
        .arg("--nocapture")
        .env(UMASK_CHILD_ENV, "1")
        .output()
        .expect("spawn umask 子进程");
    assert!(
        output.status.success(),
        "umask 0200 下新建身份必须仍是可加载的 0600；退出码 {:?}，子进程输出: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn existing_insecure_file_is_still_refused_under_hostile_umask() {
    // F4 边界：兜底 chmod 只允许作用在**本次新建**的文件上；
    // 已存在的不安全文件必须继续 fail closed，绝不「顺手修好」。
    let dir = TestDir::new("umask_no_repair");
    let path = put_key_file(&dir.path, "identity.key", &random_pkcs8(), 0o600);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).expect("chmod 0400");

    let err = IdentityKey::load_or_generate(&path).expect_err("0400 既有文件必须拒绝");
    assert!(
        matches!(err, IdentityError::InsecurePermissions(_)),
        "实际错误: {err:?}"
    );
    assert_eq!(
        fs::symlink_metadata(&path)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777,
        0o400,
        "拒绝时不得改动既有文件的权限"
    );
}

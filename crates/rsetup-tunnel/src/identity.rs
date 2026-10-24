//! v3 身份凭据与 Ed25519 长期签名密钥（protocol_spec.md §3、计划 Task 2）。
//!
//! 安全约束（全部由 `tests/identity.rs` 实测）：
//!
//! * **权限**：私钥文件必须恰为 `0600`，父目录必须恰为 `0700`，二者属主必须是
//!   当前 euid；任何一项不满足直接拒绝加载，不「顺手修好」别人的权限。
//!   本次新建的目录项与临时文件都会在 **新建后的 fd/路径**上显式 `set_permissions`，
//!   因此特殊 umask（例如 `0200`）削不掉 owner 权限；**已存在**的不安全文件继续
//!   fail closed，绝不代其修复。
//! * **拒绝非普通文件**：符号链接（含直接父目录是指向别处的链接）→
//!   [`IdentityError::SymlinkNotAllowed`]；目录、FIFO、设备节点等非 regular file →
//!   [`IdentityError::InvalidFormat`]。类型判定先在 `open` 之前用 `symlink_metadata`
//!   完成，因此命名管道目标通常根本不会被打开，更不会阻塞在 `read` 上；真正读取走
//!   `O_NOFOLLOW | O_NONBLOCK`，打开后一律以 **fd 的 fstat** 为准。
//! * **原子且不覆盖**：新生成的身份写入同目录 `.tmp_identity_*`（`create_new`），
//!   立刻在该 fd 上 `fchmod(0600)`，再写内容并 `fsync`，然后用 `hard_link` 落向目标
//!   路径（`hard_link` 遇已存在直接失败，因此并发首启绝不会互相覆盖，只会退化为加载
//!   已胜出的身份），最后 `fsync` 父目录保证目录项落盘。临时文件与目录项的失败一律
//!   向上报错，不静默忽略。
//! * **去敏**：[`IdentityError`] 只携带 `ErrorKind`、权限位、uid 等结构信息，绝不
//!   携带路径、文件名、私钥字节或 DER 内容；[`IdentityKey`] 的 `Debug` 只显示公钥 hex。
//!
//! 本模块只负责「一把长期身份密钥的载入/生成与签名」，不含任何握手或网络语义。
//! 关于 TOCTOU、崩溃残留、审计范围与权限位比较的**准确边界**（不夸大之处），
//! 见文件末尾「安全边界」注释。

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};
use thiserror::Error;

/// 私钥文件权限：owner read/write only。
const KEY_FILE_MODE: u32 = 0o600;
/// 身份目录权限：owner only。
const IDENTITY_DIR_MODE: u32 = 0o700;
/// 身份文件读取上限（PKCS#8 Ed25519 实际 48B；留出编码余量）。
/// 先校验大小再读取，绝不按 peer 提供的长度无界分配。
const MAX_IDENTITY_FILE_BYTES: u64 = 8 * 1024;

/// 身份密钥错误。所有变体的 `Display`/`Debug` 均已去敏：只含 `ErrorKind`、
/// 权限位与 uid 等结构信息，不含路径、文件名、私钥或 DER 字节。
#[derive(Debug, Error)]
pub enum IdentityError {
    /// 底层 I/O 失败，只保留 [`ErrorKind`]（`std::io::Error` 的自定义消息可能带路径）。
    #[error("identity file access failed: {0:?}")]
    Io(ErrorKind),

    /// 权限不满足 0600/0700 要求。
    #[error("insecure permissions: {0}")]
    InsecurePermissions(String),

    /// 属主不是当前 euid。
    #[error("wrong owner: expected euid {expected}, found uid {actual}")]
    WrongOwner { expected: u32, actual: u32 },

    /// 身份文件或父目录是符号链接。
    #[error("symlinks are not permitted for the identity key or its parent directory")]
    SymlinkNotAllowed,

    /// 目标不是普通文件，或大小/格式不符合 PKCS#8 Ed25519。
    #[error("invalid identity key material: {0}")]
    InvalidFormat(String),

    /// 密码学原语失败（ring 错误已去敏为固定文本）。
    #[error("cryptographic primitive failed: {0}")]
    Crypto(String),
}

/// Ed25519 长期身份密钥。私钥材料只存在于 [`Self::key_pair`] 内，
/// `Debug` 只输出公钥 hex。
pub struct IdentityKey {
    key_pair: Ed25519KeyPair,
    public_key: [u8; 32],
}

impl fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityKey")
            .field("public_key", &hex::encode(self.public_key))
            .finish()
    }
}

impl IdentityKey {
    /// 从包含并校验公钥的 PKCS#8 v2 DER 装载 Ed25519 私钥。
    /// OpenSSL 默认生成的不含公钥的 v1 编码不会被此严格入口接受。
    pub fn from_pkcs8(bytes: &[u8]) -> Result<Self, IdentityError> {
        let key_pair = Ed25519KeyPair::from_pkcs8(bytes)
            .map_err(|_| IdentityError::InvalidFormat("not a PKCS#8 Ed25519 key".to_string()))?;
        let mut public_key = [0u8; 32];
        public_key.copy_from_slice(key_pair.public_key().as_ref());
        Ok(Self {
            key_pair,
            public_key,
        })
    }

    /// 32 字节 Ed25519 公钥（握手与可信列表使用的值）。
    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// 以长期私钥对消息签名，返回恰 64 字节 Ed25519 签名。
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        let sig = self.key_pair.sign(message);
        let mut out = [0u8; 64];
        out.copy_from_slice(sig.as_ref());
        out
    }

    /// 加载或（仅当目标不存在时）原子生成身份密钥。
    ///
    /// 顺序固定：父目录安全审计 → 目标类型审计 → fd 上复核权限/属主 → 读取 →
    /// 严格解析。任何一步失败都不写入、不覆盖、不修复权限。
    pub fn load_or_generate(path: impl AsRef<Path>) -> Result<Self, IdentityError> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        ensure_secure_parent_dir(parent)?;

        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => Err(IdentityError::SymlinkNotAllowed),
            // 目录/FIFO/设备节点在读之前就拒绝：open 一个 FIFO 会永久阻塞。
            Ok(meta) if !meta.file_type().is_file() => Err(IdentityError::InvalidFormat(
                "identity path is not a regular file".to_string(),
            )),
            Ok(_) => Self::load_existing(path),
            Err(err) if err.kind() == ErrorKind::NotFound => generate_atomically(parent, path),
            Err(err) => Err(IdentityError::Io(err.kind())),
        }
    }

    /// 严格加载已存在的身份文件：`O_NOFOLLOW | O_NONBLOCK` 打开后一律以 **fd 的
    /// fstat** 为准，使「先 `symlink_metadata` 再 `open`」之间的换链接竞态无法绕过
    /// 审计；`O_NONBLOCK` 只让同一竞态里「目标被换成 FIFO」不再变成永久阻塞
    /// （普通文件上是 no-op），并不消除该窗口本身。
    fn load_existing(path: &Path) -> Result<Self, IdentityError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|err| {
                // O_NOFOLLOW 撞上链接（ELOOP）：按本模块的语义报「符号链接不允许」。
                // `ErrorKind::FilesystemLoop` 在本工具链仍不稳定，故比对原始 errno。
                if err.raw_os_error() == Some(libc::ELOOP) {
                    IdentityError::SymlinkNotAllowed
                } else {
                    IdentityError::Io(err.kind())
                }
            })?;

        let meta = file.metadata().map_err(io_err)?;
        if !meta.is_file() {
            return Err(IdentityError::InvalidFormat(
                "identity path is not a regular file".to_string(),
            ));
        }
        audit_mode(
            meta.permissions().mode(),
            KEY_FILE_MODE,
            "identity key file",
        )?;
        audit_owner(meta.uid())?;
        if meta.len() > MAX_IDENTITY_FILE_BYTES {
            return Err(IdentityError::InvalidFormat(format!(
                "identity file larger than {MAX_IDENTITY_FILE_BYTES} bytes"
            )));
        }

        // 上限已校验，按上限读取，绝不信任何自报长度。
        let mut buf = Vec::with_capacity(MAX_IDENTITY_FILE_BYTES as usize);
        file.take(MAX_IDENTITY_FILE_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(io_err)?;
        Self::from_pkcs8(&buf)
    }
}

// ---------------------------------------------------------------- 内部实现

fn io_err(err: std::io::Error) -> IdentityError {
    IdentityError::Io(err.kind())
}

fn effective_uid() -> u32 {
    // SAFETY: `geteuid` 无参数、无指针、不失败，POSIX 保证可在任何上下文调用。
    unsafe { libc::geteuid() }
}

fn audit_mode(mode: u32, expected: u32, what: &str) -> Result<(), IdentityError> {
    let actual = mode & 0o777;
    if actual != expected {
        return Err(IdentityError::InsecurePermissions(format!(
            "{what} mode is {actual:#o}, expected {expected:#o}"
        )));
    }
    Ok(())
}

fn audit_owner(uid: u32) -> Result<(), IdentityError> {
    let euid = effective_uid();
    if uid != euid {
        return Err(IdentityError::WrongOwner {
            expected: euid,
            actual: uid,
        });
    }
    Ok(())
}

fn audit_dir_meta(meta: &fs::Metadata) -> Result<(), IdentityError> {
    if meta.file_type().is_symlink() {
        return Err(IdentityError::SymlinkNotAllowed);
    }
    if !meta.is_dir() {
        return Err(IdentityError::InvalidFormat(
            "identity parent is not a directory".to_string(),
        ));
    }
    audit_mode(
        meta.permissions().mode(),
        IDENTITY_DIR_MODE,
        "identity directory",
    )?;
    audit_owner(meta.uid())
}

/// 父目录必须存在、是目录（且不是链接）、属主为 euid、权限恰 0700。
/// 缺失时按 0700 逐级创建（含所有中间目录）后再审计。
fn ensure_secure_parent_dir(parent: &Path) -> Result<(), IdentityError> {
    if fs::symlink_metadata(parent).is_err() {
        create_private_dir_tree(parent)?;
    }
    let meta = fs::symlink_metadata(parent).map_err(io_err)?;
    audit_dir_meta(&meta)
}

/// 逐级创建缺失的目录组件，并把**本次新建**的每一个组件收紧到 0700。
/// 已存在的祖先目录一律不动（可能是 `/var` 这类共享目录）。
///
/// `DirBuilder::mode(0700)` 仍会被 umask 削位，因此创建后必须显式
/// `set_permissions(0700)`，否则随后加载会因「mode 不是 0700」失败。
fn create_private_dir_tree(parent: &Path) -> Result<(), IdentityError> {
    // 先记下哪些组件当前不存在（且路径上没有非目录组件）。
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut cursor = parent.to_path_buf();
    loop {
        match fs::symlink_metadata(&cursor) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(IdentityError::SymlinkNotAllowed);
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(IdentityError::InvalidFormat(
                    "identity parent is not a directory".to_string(),
                ));
            }
            Ok(_) => break,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                missing.push(cursor.clone());
                match cursor.parent() {
                    Some(next) if !next.as_os_str().is_empty() => cursor = next.to_path_buf(),
                    _ => break,
                }
            }
            Err(err) => return Err(IdentityError::Io(err.kind())),
        }
    }

    fs::DirBuilder::new()
        .mode(IDENTITY_DIR_MODE)
        .recursive(true)
        .create(parent)
        .map_err(io_err)?;

    for path in &missing {
        fs::set_permissions(path, fs::Permissions::from_mode(IDENTITY_DIR_MODE)).map_err(io_err)?;
    }
    Ok(())
}

/// 生成新身份并原子落盘：临时文件（`create_new`+0600）→ 写入 → `fsync` →
/// `hard_link`（已存在即失败，故绝不覆盖）→ `fsync` 父目录。
fn generate_atomically(parent: &Path, target: &Path) -> Result<IdentityKey, IdentityError> {
    let rng = SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| IdentityError::Crypto("Ed25519 key generation failed".to_string()))?;

    let (tmp_path, tmp_file) = loop {
        let tmp_path = parent.join(unique_tmp_name(&rng)?);
        let file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(KEY_FILE_MODE)
            .open(&tmp_path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(IdentityError::Io(err.kind())),
        };
        // `mode(0o600)` 仍会被 umask 削位（umask 0200 会把它变成 0400，随后
        // `hard_link` 出来的身份文件就加载不了）。必须在**本次新建的 fd** 上
        // 显式 fchmod，且在写入与 fsync 之前完成；这里只可能改到自己刚创建的
        // 那个 fd，绝不触碰任何已存在的文件。
        if let Err(err) = file.set_permissions(fs::Permissions::from_mode(KEY_FILE_MODE)) {
            drop(file);
            let _ = fs::remove_file(&tmp_path);
            return Err(io_err(err));
        }
        break (tmp_path, file);
    };

    if let Err(err) = write_and_sync(tmp_file, pkcs8.as_ref()) {
        let _ = fs::remove_file(&tmp_path);
        return Err(IdentityError::Io(err.kind()));
    }

    // hard_link 而非 rename：目标已存在时直接失败，语义上不可能覆盖既有身份。
    match fs::hard_link(&tmp_path, target) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp_path);
            sync_dir(parent)?;
            IdentityKey::from_pkcs8(pkcs8.as_ref())
        }
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            // 并发首启：另一方已落盘。丢弃自己生成的密钥，加载胜出者，
            // 使所有进程/线程收敛到同一身份。
            let _ = fs::remove_file(&tmp_path);
            IdentityKey::load_existing(target)
        }
        Err(err) => {
            let _ = fs::remove_file(&tmp_path);
            Err(IdentityError::Io(err.kind()))
        }
    }
}

fn write_and_sync(mut file: File, bytes: &[u8]) -> std::io::Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn sync_dir(dir: &Path) -> Result<(), IdentityError> {
    let file = File::open(dir).map_err(io_err)?;
    file.sync_all().map_err(io_err)
}

/// 同一进程内的多线程竞争也要产生不同临时名，故叠加纳秒时间、pid、计数与随机值。
fn unique_tmp_name(rng: &SystemRandom) -> Result<String, IdentityError> {
    let mut random = [0u8; 8];
    rng.fill(&mut random)
        .map_err(|_| IdentityError::Crypto("CSPRNG unavailable".to_string()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_nanos();
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(format!(
        ".tmp_identity_{}_{nanos}_{seq}_{}",
        std::process::id(),
        hex::encode(random)
    ))
}

// ============================================================ 安全边界
//
// 以下是本模块**故意不承诺**的部分，写在代码旁边以免被读成更强的保证：
//
// 1. TOCTOU（F5 口径）：`symlink_metadata` → `open` 之间存在窗口，`O_NOFOLLOW`
//    只挡住「目标被换成符号链接」，不挡住「目标被换成 FIFO/其它 inode」。
//    `O_NONBLOCK` 消除的是**后果**（对 FIFO 的 open/read 永久阻塞），窗口本身
//    仍然存在，因此不声称静态 FIFO 测试覆盖了完整竞态；能在这个 0700 私有目录内
//    创建/替换文件的对端，已经在威胁模型之外（等价于已拥有该 uid）。
//    打开后一律以 fd 的 `fstat` 复核「是 regular file」，普通文件语义不变。
//
// 2. 崩溃残留（F6 口径）：临时文件的删除只发生在成功路径与本模块可见的失败路径
//    上。进程在 `create_new` 之后被杀，会留下 0700 目录与 0600 的
//    `.tmp_identity_*` 残留；「成功路径不留临时文件」不等于「崩溃后不留」。
//    本模块不做启动期批量清理——按名字/模式匹配删除同目录临时文件会误删其它进程
//    合法创建的临时文件，而残留文件本身已是 0600，收益远小于风险。
//
// 3. 审计范围（F7 口径）：只审计**直接父目录**本身（存在、是目录、不是符号链接、
//    恰 0700、属主为当前 euid）。更上层祖先若本身是指向别处的符号链接，不在审计
//    范围内；本模块不声称「全部祖先目录均无 symlink」，路径由部署者提供并在
//    0700 私有目录内使用。
//
// 4. 权限位比较（F7 口径）：`audit_mode` 比较 `mode & 0o777`，即
//    setuid/setgid/sticky 等特殊位**不在该约束的比较范围**内——本模块既不会因为
//    特殊位拒绝，也不会顺手清掉它们；属主（euid）判定始终生效，不因任何原因放宽。

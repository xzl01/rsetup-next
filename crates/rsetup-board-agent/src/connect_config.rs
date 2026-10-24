//! 生产连接配置；不建立网络、不发现公钥、不读写密钥或journal文件。
//!
//! pin必须由管理员显式提供，无TOFU/随机信任/地址推断。路径校验仅是词法校验，
//! 实际密钥文件owner/mode/symlink校验由隧道IdentityKey负责。
use std::fmt;
use std::net::Ipv6Addr;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

pub const MAX_CONTROLLER_PINS: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct BoardConnectConfig {
    pub controller_addr: String,
    pub identity_file: PathBuf,
    pub journal_dir: PathBuf,
    pub trusted_servers: Vec<[u8; 32]>,
}
impl fmt::Debug for BoardConnectConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoardConnectConfig")
            .field("controller_addr", &"<configured>")
            .field("identity_file", &"<configured>")
            .field("journal_dir", &"<configured>")
            .field("trusted_server_count", &self.trusted_servers.len())
            .finish()
    }
}
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ConnectConfigError {
    #[error("controller host and nonzero TCP port are required")]
    Address,
    #[error("explicit trusted controller public keys are required")]
    MissingPins,
    #[error("trusted controller key list is invalid or exceeds its budget")]
    Pins,
    #[error("identity and journal paths must be absolute without parent traversal")]
    StatePath,
}
impl BoardConnectConfig {
    /// 生产入口可传入env::var(key).ok()；错误只带固定类别，不回显环境值。
    pub fn from_lookup(
        mut lookup: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, ConnectConfigError> {
        let addr = lookup("RSETUP_CONTROLLER_ADDR").ok_or(ConnectConfigError::Address)?;
        let pins =
            lookup("RSETUP_CONTROLLER_PUBLIC_KEYS").ok_or(ConnectConfigError::MissingPins)?;
        let identity = lookup("RSETUP_BOARD_KEY_FILE")
            .unwrap_or_else(|| "/var/lib/rsetup-board-agent/identity.pk8".into());
        let journal = lookup("RSETUP_BOARD_JOURNAL_DIR")
            .unwrap_or_else(|| "/var/lib/rsetup-board-agent/journal".into());
        Self::from_values(&addr, Path::new(&identity), Path::new(&journal), &pins)
    }

    pub fn from_values(
        controller_addr: &str,
        identity_file: &Path,
        journal_dir: &Path,
        pins: &str,
    ) -> Result<Self, ConnectConfigError> {
        validate_address(controller_addr)?;
        if !valid_state_path(identity_file)
            || !valid_state_path(journal_dir)
            || identity_file == journal_dir
        {
            return Err(ConnectConfigError::StatePath);
        }
        let trusted_servers = parse_pins(pins)?;
        Ok(Self {
            controller_addr: controller_addr.into(),
            identity_file: identity_file.into(),
            journal_dir: journal_dir.into(),
            trusted_servers,
        })
    }
}
fn valid_state_path(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().is_some()
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
}
fn validate_address(addr: &str) -> Result<(), ConnectConfigError> {
    if addr.is_empty()
        || addr.len() > 260
        || !addr.is_ascii()
        || addr.bytes().any(|b| b.is_ascii_whitespace())
    {
        return Err(ConnectConfigError::Address);
    }
    let (host, port) = addr.rsplit_once(':').ok_or(ConnectConfigError::Address)?;
    if port.is_empty()
        || !port.bytes().all(|b| b.is_ascii_digit())
        || port.parse::<u16>().ok().filter(|p| *p != 0).is_none()
    {
        return Err(ConnectConfigError::Address);
    }
    if host.starts_with('[') && host.ends_with(']') {
        host[1..host.len() - 1]
            .parse::<Ipv6Addr>()
            .map_err(|_| ConnectConfigError::Address)?;
    } else {
        if host.is_empty() || host.len() > 253 {
            return Err(ConnectConfigError::Address);
        }
        for label in host.split('.') {
            if label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            {
                return Err(ConnectConfigError::Address);
            }
        }
    }
    Ok(())
}
fn parse_pins(pins: &str) -> Result<Vec<[u8; 32]>, ConnectConfigError> {
    if pins.trim().is_empty() {
        return Err(ConnectConfigError::MissingPins);
    }
    if pins.len() > 2048 {
        return Err(ConnectConfigError::Pins);
    }
    let mut keys = Vec::new();
    for text in pins.split(',') {
        let text = text.trim();
        if keys.len() >= MAX_CONTROLLER_PINS
            || text.len() != 64
            || !text.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(ConnectConfigError::Pins);
        }
        let mut key = [0u8; 32];
        hex::decode_to_slice(text, &mut key).map_err(|_| ConnectConfigError::Pins)?;
        if key == [0u8; 32] || keys.contains(&key) {
            return Err(ConnectConfigError::Pins);
        }
        keys.push(key);
    }
    Ok(keys)
}

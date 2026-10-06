//! ring 0.17.14 Ed25519 薄封装（本 crate 唯一真实密码学入口）。
//!
//! 本切片生产路径只用 `verify_ed25519`；`sign_ed25519` 供测试向量生成与
//! 未来中控响应签名。密钥生成/落盘/加载（0600 等）不在本切片。

use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};
use thiserror::Error;

pub const PUBLIC_KEY_LEN: usize = 32;
pub const SIGNATURE_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VerifyError {
    #[error("bad public key length: {len} (want {PUBLIC_KEY_LEN})")]
    PublicKeyLen { len: usize },
    #[error("bad signature length: {len} (want {SIGNATURE_LEN})")]
    SignatureLen { len: usize },
    #[error("invalid signature")]
    InvalidSignature,
}

/// 验签：先长度守卫（公钥 32B，再签名 64B，固定顺序），后真实 ring 验签。
/// 错误固定、去敏：不回显 key/消息/签名内容。
pub fn verify_ed25519(
    message: &[u8],
    signature: &[u8],
    public_key: &[u8],
) -> Result<(), VerifyError> {
    if public_key.len() != PUBLIC_KEY_LEN {
        return Err(VerifyError::PublicKeyLen {
            len: public_key.len(),
        });
    }
    if signature.len() != SIGNATURE_LEN {
        return Err(VerifyError::SignatureLen {
            len: signature.len(),
        });
    }
    let pk = UnparsedPublicKey::new(&ED25519, public_key);
    pk.verify(message, signature)
        .map(|_| ())
        .map_err(|_| VerifyError::InvalidSignature)
}

/// 签名：ring 薄封装，只接收真实 `Ed25519KeyPair`。
pub fn sign_ed25519(key: &Ed25519KeyPair, message: &[u8]) -> [u8; SIGNATURE_LEN] {
    let sig = key.sign(message);
    let mut out = [0u8; SIGNATURE_LEN];
    out.copy_from_slice(sig.as_ref());
    out
}

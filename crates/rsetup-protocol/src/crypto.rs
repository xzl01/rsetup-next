//! protocol v3 传输加密核心（第 1 层）：X25519 / HKDF-SHA256 / AES-256-GCM 记录层。
//!
//! 严格对应 `docs/protocol_spec.md` §4.4（X25519 约束）与 §5（传输加密规范）：
//! - §5.1：HKDF-SHA256，Salt = `client_random || server_random`（64B），
//!   Extract(SharedSecret) 后按四条逐字 `info` Expand 出 c2s/s2c 的 key(32B)/iv(12B)；
//! - §5.2：记录帧 = `4B BE Record Length + AES-256-GCM 密文 + 16B Tag`，
//!   记录体（密文+Tag）严格 `16 ≤ len ≤ 65536`；
//!   `Nonce[0:4] = IV_Base[0:4]`，`Nonce[4:12] = IV_Base[4:12] XOR BE64(Seq)`；
//!   `AAD = BE32(Record Length) || BE64(Seq)`；`TxSeq`/`RcvSeq` 从 0 起单调递增，
//!   有效序号 `0..=2^64-2`，禁止回绕，序号耗尽即连接级致命。
//!
//! 本模块是纯函数/纯状态机：不含网络 socket、不含握手消息编解码、不含重放窗口
//! 之外的任何 IO 假设。同一方向的 `seal` 串行化（以及 `open` 串行化）由调用方
//! 保证；本模块用 fatal 状态兜底：任何认证、长度或序号失败后整个记录层进入
//! 致命状态，后续所有 `seal`/`open` 一律拒绝（对应规范"错误 tag 或 AAD 即关闭
//! 整条 TCP，双向 key/IV/seq 全部失效"）。
//!
//! 所有错误统一映射为 [`CryptoError`]，内容去敏：不回显密钥、明文、密文或
//! 对端数据；不使用 panic 路径（越界均先做长度守卫）。

use ring::aead;
use ring::agreement;
use ring::hkdf;
use ring::rand::SystemRandom;
use thiserror::Error;

// ---------- 固定长度常量（§4.4 / §5.1 / §5.2） ----------

/// X25519 公钥字节数（§4.4）。
pub const X25519_KEY_LEN: usize = 32;
/// X25519 共享密钥字节数（§5.1）。
pub const SHARED_SECRET_LEN: usize = 32;
/// 派生的记录层 AES-256 密钥字节数（§5.1）。
pub const RECORD_KEY_LEN: usize = 32;
/// 派生的记录层 IV_Base 字节数（§5.1，恰为 GCM Nonce 长度）。
pub const RECORD_IV_LEN: usize = 12;
/// AES-256-GCM Tag 字节数（§5.2）。
pub const GCM_TAG_LEN: usize = 16;
/// 记录帧长度前缀字节数（§5.2，大端）。
pub const RECORD_LEN_PREFIX_LEN: usize = 4;
/// 记录体（密文 + Tag）最小长度：恰为 16B Tag，对应零明文记录（§5.2）。
pub const MIN_RECORD_BODY_LEN: usize = GCM_TAG_LEN;
/// 记录体（密文 + Tag）最大长度（§5.2）。
pub const MAX_RECORD_BODY_LEN: usize = 65_536;
/// 单条记录线上完整字节数上限 = 4B 前缀 + 65536B 记录体 = 65540B，
/// 即接收方未完成缓冲区预算上限（§5.2）。
pub const MAX_RECORD_WIRE_LEN: usize = RECORD_LEN_PREFIX_LEN + MAX_RECORD_BODY_LEN;
/// 单条记录明文上限 = 65536 - 16（§5.2）。
pub const MAX_PLAINTEXT_LEN: usize = MAX_RECORD_BODY_LEN - GCM_TAG_LEN;
/// HKDF Salt 字节数 = `client_random || server_random` = 32 + 32（§5.1）。
pub const HKDF_SALT_LEN: usize = 64;

/// §5.1 HKDF-Expand `info` 字符串，逐字使用，不得更改。
const INFO_C2S_KEY: &[u8] = b"rsetup-next/tunnel/c2s-key";
const INFO_S2C_KEY: &[u8] = b"rsetup-next/tunnel/s2c-key";
const INFO_C2S_IV: &[u8] = b"rsetup-next/tunnel/c2s-iv";
const INFO_S2C_IV: &[u8] = b"rsetup-next/tunnel/s2c-iv";

/// 传输加密核心统一错误。Display 输出去敏固定文案：不包含任何密钥、明文、
/// 密文、对端字节或长度之外的会话数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CryptoError {
    /// X25519 密钥协商失败：对端公钥全零/低阶/非法编码，或共享密钥全零
    /// （§4.4：均为终止会话级错误）。
    #[error("x25519 key agreement failed (peer key rejected or all-zero shared secret)")]
    KeyAgreement,
    /// HKDF-SHA256 派生失败（§5.1，正常输入下不可达，防御性映射）。
    #[error("hkdf-sha256 session key derivation failed")]
    Derive,
    /// 记录体超过 65536B 上限（§5.2）：`seal` 明文 > 65520B，或 `open`
    /// 长度字段声明 > 65536。
    #[error("record too large (body exceeds 65536 bytes)")]
    RecordTooLarge,
    /// 记录体小于 16B 下限（§5.2）：长度字段声明 < 16。
    #[error("record too small (body below 16 bytes)")]
    RecordTooSmall,
    /// 输入不是恰好一条完整记录：总字节数与 4B 大端长度字段不一致（截断或
    /// 多余尾随字节），或不足 4B 长度前缀。
    #[error("record framing invalid (input is not exactly one complete record)")]
    BadFrame,
    /// GCM Tag/AAD 认证失败（§5.2：tag 或 AAD 被篡改即致命）。失败不推进
    /// RcvSeq、不交付任何明文。
    #[error("record authentication failed (tag or aad mismatch)")]
    AuthFailed,
    /// 序号空间耗尽（§5.2）：发送方在发出序号 `2^64-2` 的记录后再次 `seal`，
    /// 或接收方期望序号到达 `2^64-1`。均为连接级致命，须关闭整条 TCP，
    /// 不得在重连后复用密钥。
    #[error("record sequence space exhausted (close the connection; smooth shutdown required)")]
    SequenceExhausted,
    /// 记录层已进入致命状态：此前发生过认证/长度/序号/耗尽类失败，按 §5.2
    /// 双向 key、IV、序号全部失效，拒绝一切后续操作。
    #[error("record layer is fatal; all further seal/open operations are rejected")]
    Fatal,
    /// ring 原语装配失败（密钥长度固定，正常输入下不可达，防御性映射）。
    #[error("cryptographic primitive setup failed")]
    Internal,
}

// ---------- §4.4 X25519 ----------

/// X25519 临时密钥对（§4.4，RFC 7748）。一次会话一对，`agree` 消耗自身，
/// 结构上保证同一私钥至多协商一次。
pub struct EphemeralKeyPair {
    private: agreement::EphemeralPrivateKey,
    public: [u8; X25519_KEY_LEN],
}

impl core::fmt::Debug for EphemeralKeyPair {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 公钥非机密，可显示；私钥材料绝不显示。
        f.debug_struct("EphemeralKeyPair")
            .field("public", &hex::encode(self.public))
            .finish()
    }
}

impl EphemeralKeyPair {
    /// 生成一次性 X25519 临时密钥对（ring CSPRNG）。
    pub fn generate() -> Result<Self, CryptoError> {
        let rng = SystemRandom::new();
        let private = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng)
            .map_err(|_| CryptoError::KeyAgreement)?;
        let public_key = private
            .compute_public_key()
            .map_err(|_| CryptoError::KeyAgreement)?;
        let mut public = [0u8; X25519_KEY_LEN];
        public.copy_from_slice(public_key.as_ref());
        Ok(Self { private, public })
    }

    /// 本端 32B 公钥（发送给对端的字节）。
    pub fn public_key(&self) -> [u8; X25519_KEY_LEN] {
        self.public
    }

    /// 与对端 32B 公钥执行 X25519 共享密钥协商（§4.4）。
    ///
    /// 拒绝：全零公钥、低阶（小阶）公钥、ring 判定的非法编码，以及协商出的
    /// 全零共享密钥；ring 内部错误统一去敏映射为 [`CryptoError::KeyAgreement`]。
    pub fn agree(
        self,
        peer_public: &[u8; X25519_KEY_LEN],
    ) -> Result<[u8; SHARED_SECRET_LEN], CryptoError> {
        self.agree_from_slice(peer_public.as_slice())
    }

    /// [`Self::agree`] 的切片入口：额外承担公钥长度校验（≠32B 直接拒绝），
    /// 供帧/消息层在拿到未定型字节切片时复用同一套约束。
    pub fn agree_from_slice(
        self,
        peer_public: &[u8],
    ) -> Result<[u8; SHARED_SECRET_LEN], CryptoError> {
        if peer_public.len() != X25519_KEY_LEN {
            return Err(CryptoError::KeyAgreement);
        }
        // 显式拒绝全零公钥（低阶点之一）；其余低阶/小序点由 ring 的
        // RFC 7748 全零输出检查拒绝。
        if peer_public.iter().all(|&b| b == 0) {
            return Err(CryptoError::KeyAgreement);
        }
        let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, peer_public);
        agreement::agree_ephemeral(self.private, &peer, |shared| {
            // 双保险守卫：ring 已对 X25519 全零输出返回错误（§4.4）。
            if shared.len() != SHARED_SECRET_LEN || shared.iter().all(|&b| b == 0) {
                return Err(CryptoError::KeyAgreement);
            }
            let mut out = [0u8; SHARED_SECRET_LEN];
            out.copy_from_slice(shared);
            Ok(out)
        })
        .map_err(|_| CryptoError::KeyAgreement)?
    }
}

// ---------- §5.1 HKDF-SHA256 会话密钥派生 ----------

/// `Prk::expand` 所需的任意输出长度包装（ring 0.17.14 未导出 `L`，
/// `KeyType` 是公开 trait，此处按规范长度实现）。
#[derive(Clone, Copy)]
struct OkmLen(usize);

impl hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        self.0
    }
}

/// §5.1 派生出的会话密钥四元组。字段私有、Debug 去敏；唯一出口是
/// [`Self::client_record_layer`] / [`Self::server_record_layer`]。
pub struct SessionKeys {
    c2s_key: [u8; RECORD_KEY_LEN],
    s2c_key: [u8; RECORD_KEY_LEN],
    c2s_iv: [u8; RECORD_IV_LEN],
    s2c_iv: [u8; RECORD_IV_LEN],
}

impl core::fmt::Debug for SessionKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SessionKeys").finish_non_exhaustive()
    }
}

impl SessionKeys {
    /// §5.1 密钥派生：
    /// `PRK = HKDF-Extract(Salt = client_random || server_random, ikm = SharedSecret)`，
    /// 再按四条逐字 `info` 各做一次 HKDF-Expand：
    /// `c2s-key` 32B、`s2c-key` 32B、`c2s-iv` 12B、`s2c-iv` 12B。
    ///
    /// 双方以各自的 `(SharedSecret, client_random, server_random)` 同序输入，
    /// 得到逐字节一致的四元组；c2s/s2c 方向由记录层按角色绑定。
    pub fn derive(
        shared_secret: &[u8; SHARED_SECRET_LEN],
        client_random: &[u8; 32],
        server_random: &[u8; 32],
    ) -> Result<Self, CryptoError> {
        let mut salt = [0u8; HKDF_SALT_LEN];
        salt[..32].copy_from_slice(client_random);
        salt[32..].copy_from_slice(server_random);
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &salt).extract(shared_secret);

        let mut keys = Self {
            c2s_key: [0u8; RECORD_KEY_LEN],
            s2c_key: [0u8; RECORD_KEY_LEN],
            c2s_iv: [0u8; RECORD_IV_LEN],
            s2c_iv: [0u8; RECORD_IV_LEN],
        };
        expand_into(&prk, INFO_C2S_KEY, &mut keys.c2s_key)?;
        expand_into(&prk, INFO_S2C_KEY, &mut keys.s2c_key)?;
        expand_into(&prk, INFO_C2S_IV, &mut keys.c2s_iv)?;
        expand_into(&prk, INFO_S2C_IV, &mut keys.s2c_iv)?;
        Ok(keys)
    }

    /// 板端（client）视角记录层：tx = c2s 方向（seal 用 c2s key/iv），
    /// rx = s2c 方向（open 用 s2c key/iv）。
    pub fn client_record_layer(&self) -> RecordLayerPair {
        RecordLayerPair {
            tx: record_layer(self.c2s_key, self.c2s_iv),
            rx: record_layer(self.s2c_key, self.s2c_iv),
        }
    }

    /// 中控端（server）视角记录层：tx = s2c 方向，rx = c2s 方向。
    pub fn server_record_layer(&self) -> RecordLayerPair {
        RecordLayerPair {
            tx: record_layer(self.s2c_key, self.s2c_iv),
            rx: record_layer(self.c2s_key, self.c2s_iv),
        }
    }
}

fn expand_into(prk: &hkdf::Prk, info: &[u8], out: &mut [u8]) -> Result<(), CryptoError> {
    prk.expand(&[info], OkmLen(out.len()))
        .map_err(|_| CryptoError::Derive)?
        .fill(out)
        .map_err(|_| CryptoError::Derive)
}

// ---------- §5.2 AES-256-GCM 记录层 ----------

/// 一条握手会话记录层方向对。字段即两个独立的单向 [`RecordLayer`]：
/// - `tx`：调用 [`RecordLayer::seal`] 发出记录；
/// - `rx`：调用 [`RecordLayer::open`] 解入对端记录。
///
/// client/server 两端对同一方向的 key/iv 相同，但各端只在自己的 tx 上
/// `seal`、自己的 rx 上 `open`。
#[derive(Debug)]
pub struct RecordLayerPair {
    /// 发送方向（seal）。
    pub tx: RecordLayer,
    /// 接收方向（open）。
    pub rx: RecordLayer,
}

/// 单向记录保护层（§5.2）。持有该方向的 key/iv 与两个序号计数器
/// （`tx_seq` 供 `seal`、`rx_seq` 供 `open`，均从 0 起）。
///
/// 一个实例服务一个方向：对端用同 key/iv 构造自己的对偶实例。同方向调用
/// 串行化由调用方保证（§5.2"同一方向不得并发交错"）。任何失败（tag/AAD
/// 认证、长度非法、序号耗尽）都会把实例置为 fatal，此后一切操作拒绝，
/// 对应规范"该连接全部密钥/IV/序号立即失效，不得重同步"。
pub struct RecordLayer {
    key: [u8; RECORD_KEY_LEN],
    iv: [u8; RECORD_IV_LEN],
    tx_seq: u64,
    rx_seq: u64,
    fatal: bool,
}

impl core::fmt::Debug for RecordLayer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 序号与 fatal 位非机密，便于日志定位；key/iv 绝不显示。
        f.debug_struct("RecordLayer")
            .field("tx_seq", &self.tx_seq)
            .field("rx_seq", &self.rx_seq)
            .field("fatal", &self.fatal)
            .finish()
    }
}

fn record_layer(key: [u8; RECORD_KEY_LEN], iv: [u8; RECORD_IV_LEN]) -> RecordLayer {
    RecordLayer {
        key,
        iv,
        tx_seq: 0,
        rx_seq: 0,
        fatal: false,
    }
}

/// ⚠️ 仅测试使用（`#[doc(hidden)]`，非生产 API）。
///
/// 生产记录层只能经 [`SessionKeys::client_record_layer`] /
/// [`SessionKeys::server_record_layer`] 创建，序号恒从 0 起。本入口供集成
/// 测试构造"序号接近耗尽"等内部状态，以验证 §5.2 序号末值行为。
#[doc(hidden)]
pub fn record_layer_at_seq(
    key: &[u8; RECORD_KEY_LEN],
    iv: &[u8; RECORD_IV_LEN],
    seq: u64,
) -> RecordLayer {
    RecordLayer {
        key: *key,
        iv: *iv,
        tx_seq: seq,
        rx_seq: seq,
        fatal: false,
    }
}

/// ⚠️ 仅测试使用（`#[doc(hidden)]`，非生产 API）：派生密钥四元组原始字节，
/// 供集成测试做 HKDF 一致性与序号末值构造。
#[doc(hidden)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyMaterial {
    /// 32B C2S AES-256 密钥。
    pub c2s_key: [u8; RECORD_KEY_LEN],
    /// 32B S2C AES-256 密钥。
    pub s2c_key: [u8; RECORD_KEY_LEN],
    /// 12B C2S IV-Base。
    pub c2s_iv: [u8; RECORD_IV_LEN],
    /// 12B S2C IV-Base。
    pub s2c_iv: [u8; RECORD_IV_LEN],
}

#[doc(hidden)]
impl core::fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("KeyMaterial").finish_non_exhaustive()
    }
}

#[doc(hidden)]
impl SessionKeys {
    /// ⚠️ 仅测试使用（`#[doc(hidden)]`，非生产 API）：导出派生四元组。
    pub fn key_material_for_test(&self) -> KeyMaterial {
        KeyMaterial {
            c2s_key: self.c2s_key,
            s2c_key: self.s2c_key,
            c2s_iv: self.c2s_iv,
            s2c_iv: self.s2c_iv,
        }
    }
}

impl RecordLayer {
    /// 是否已处于致命状态（致命后实例只能被丢弃重建）。
    pub fn is_fatal(&self) -> bool {
        self.fatal
    }

    /// 封箱一条记录（§5.2）：输出恰为
    /// `4B BE 长度前缀 + AES-256-GCM 密文 + 16B Tag`；
    /// 长度字段只计密文+Tag（16..=65536），明文上限 65520B，允许零明文。
    ///
    /// Nonce/AAD 按 §5.2 由 `tx_seq` 构造；仅当整条记录成功封箱才推进
    /// `tx_seq`。序号 `2^64-1` 拒绝：发出序号 `2^64-2` 的记录后再次调用
    /// 返回 [`CryptoError::SequenceExhausted`]（调用方须平滑关闭连接）。
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.fatal {
            return Err(CryptoError::Fatal);
        }
        if plaintext.len() > MAX_PLAINTEXT_LEN {
            // 记录体将超过 65536 上限：协议级致命（§5.2 长度上限）。
            self.fatal = true;
            return Err(CryptoError::RecordTooLarge);
        }
        if self.tx_seq == u64::MAX {
            // 有效序号仅 0..=2^64-2；上一记录（2^64-2）已发出，必须收尾。
            self.fatal = true;
            return Err(CryptoError::SequenceExhausted);
        }
        let seq = self.tx_seq;
        let body_len = plaintext.len() + GCM_TAG_LEN; // ≤ 65536，u32 安全
        let nonce = aead::Nonce::assume_unique_for_key(build_nonce(&self.iv, seq));
        let aad = build_aad(body_len as u32, seq);
        let key = gcm_key(&self.key).inspect_err(|_e| {
            self.fatal = true;
        })?;

        let mut out = Vec::with_capacity(RECORD_LEN_PREFIX_LEN + body_len);
        out.extend_from_slice(&(body_len as u32).to_be_bytes());
        out.extend_from_slice(plaintext);
        let tag = key
            .seal_in_place_separate_tag(
                nonce,
                aead::Aad::from(aad),
                &mut out[RECORD_LEN_PREFIX_LEN..],
            )
            .map_err(|_| {
                self.fatal = true;
                CryptoError::Internal
            })?;
        out.extend_from_slice(tag.as_ref());
        // 整条记录封箱成功才推进；seq < u64::MAX 已在入口守卫，不会回绕。
        self.tx_seq = seq + 1;
        Ok(out)
    }

    /// 解封一条记录（§5.2）。输入必须是**恰好一条完整记录**（含 4B 长度
    /// 前缀，总长 = 4 + 声明长度）。
    ///
    /// 校验顺序固定：fatal → 前缀完整性 → 长度字段（先校验预算再分配）→
    /// 帧完整性 → 序号 → tag/AAD 认证。任何失败都不交付明文、不推进
    /// `rx_seq`，并把实例置为 fatal（连接级致命，不得在原连接重同步）。
    pub fn open(&mut self, record: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.fatal {
            return Err(CryptoError::Fatal);
        }
        if record.len() < RECORD_LEN_PREFIX_LEN {
            self.fatal = true;
            return Err(CryptoError::BadFrame);
        }
        let mut len_be = [0u8; RECORD_LEN_PREFIX_LEN];
        len_be.copy_from_slice(&record[..RECORD_LEN_PREFIX_LEN]);
        let body_len_be = u32::from_be_bytes(len_be);
        // §5.2：先校验长度与缓冲预算，再分配。
        if body_len_be < MIN_RECORD_BODY_LEN as u32 {
            self.fatal = true;
            return Err(CryptoError::RecordTooSmall);
        }
        if body_len_be > MAX_RECORD_BODY_LEN as u32 {
            self.fatal = true;
            return Err(CryptoError::RecordTooLarge);
        }
        let body_len = body_len_be as usize;
        if record.len() != RECORD_LEN_PREFIX_LEN + body_len {
            // 截断或多余尾随字节：不是恰好一条完整记录。
            self.fatal = true;
            return Err(CryptoError::BadFrame);
        }
        if self.rx_seq == u64::MAX {
            // 期望序号到达 2^64-1：立即终止（§5.2 接收端规则）。
            self.fatal = true;
            return Err(CryptoError::SequenceExhausted);
        }
        let seq = self.rx_seq;
        let nonce = aead::Nonce::assume_unique_for_key(build_nonce(&self.iv, seq));
        let aad = build_aad(body_len_be, seq);
        let key = gcm_key(&self.key).inspect_err(|_e| {
            self.fatal = true;
        })?;

        let mut buf = record[RECORD_LEN_PREFIX_LEN..].to_vec();
        let opened = key.open_in_place(nonce, aead::Aad::from(aad), &mut buf);
        match opened {
            Ok(plaintext) => {
                let out = plaintext.to_vec();
                // 认证成功才推进；seq < u64::MAX 已在入口守卫，不会回绕。
                self.rx_seq = seq + 1;
                Ok(out)
            }
            Err(_) => {
                // tag/AAD 失败：不交付明文、不推进 rx_seq、原连接不可重同步。
                self.fatal = true;
                Err(CryptoError::AuthFailed)
            }
        }
    }
}

/// AES-256-GCM key 装配。密钥长度为固定 32B，正常输入下不会失败；
/// 失败路径由调用方置 fatal。
fn gcm_key(key: &[u8; RECORD_KEY_LEN]) -> Result<aead::LessSafeKey, CryptoError> {
    aead::UnboundKey::new(&aead::AES_256_GCM, key)
        .map(aead::LessSafeKey::new)
        .map_err(|_| CryptoError::Internal)
}

/// §5.2 Nonce 生成：`Nonce[0:4] = IV_Base[0:4]`；
/// `Nonce[4:12] = IV_Base[4:12] XOR BE64(Seq)`。
fn build_nonce(iv: &[u8; RECORD_IV_LEN], seq: u64) -> [u8; RECORD_IV_LEN] {
    let mut nonce = [0u8; RECORD_IV_LEN];
    nonce[..4].copy_from_slice(&iv[..4]);
    let mut tail = [0u8; 8];
    tail.copy_from_slice(&iv[4..]);
    let tail = u64::from_be_bytes(tail) ^ seq;
    nonce[4..].copy_from_slice(&tail.to_be_bytes());
    nonce
}

/// §5.2 AAD：`Record Length(4B, BE) || BE64(SeqID)`，共 12B。
/// `Record Length` 与长度字段一致（密文+Tag 字节数）。
fn build_aad(body_len_be: u32, seq: u64) -> [u8; 12] {
    let mut aad = [0u8; 12];
    aad[..4].copy_from_slice(&body_len_be.to_be_bytes());
    aad[4..].copy_from_slice(&seq.to_be_bytes());
    aad
}

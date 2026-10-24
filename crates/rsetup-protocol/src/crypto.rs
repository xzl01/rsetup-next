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
//! 保证；本模块用 **pair 级共享 fatal 熔断位**兜底：任一方向的 `seal`/`open`
//! 发生任何认证、长度或序号失败，都会原子地置位 [`RecordLayerPair`] 两方向共享
//! 的熔断位，此后两个方向的一切 `seal`/`open` 一律拒绝（对应规范"错误 tag 或
//! AAD 即关闭整条 TCP，双向 key/IV/seq 全部失效"）。该传播由类型实现**强制**，
//! 不依赖调用方自觉检查 [`RecordLayerPair::is_fatal`]：即使经由公开字段直接调用
//! 对向的 `seal`/`open`，一侧失败后另一侧也立即返回 [`CryptoError::Fatal`]。
//!
//! 会话密钥 [`SessionKeys`] 为**一次性**消耗品：唯一出口
//! [`SessionKeys::into_client_record_layers`] /
//! [`SessionKeys::into_server_record_layers`] 均消耗 `self`，且本类型不实现
//! `Clone`/`Copy`/`Default`。生产 API 结构上无法复制一组 key/IV、也无法把
//! `seq` 重置回 0 再启第二个发送器，从根上排除同 (key/IV, seq) 的 nonce 重用。
//!
//! 本模块看不到 socket：`seal` 成功返回后写出失败或只写出部分字节，本模块
//! 无从感知（`seal` 已推进 TxSeq，禁止重试），调用方必须调用
//! [`RecordLayerPair::mark_write_failed`] 把两个方向一并作废。
//!
//! 所有错误统一映射为 [`CryptoError`]，内容去敏：不回显密钥、明文、密文或
//! 对端数据；不使用 panic 路径（越界均先做长度守卫）。

// 生产构建门禁（R5）：**默认 feature 组合**（未启用 `test-util`）下，序号可
// 指定的测试构造器必须完全不存在。下面这条 `compile_fail` 文档测试只在生产
// feature 组合下被收集（启用 `test-util` 后随 cfg 一起消失），因此
// `cargo test -p rsetup-protocol --doc` 会真的编译它并要求"编译失败"——这条
// 门禁是可执行的证据，不是 `#[doc(hidden)]` 那种装饰性的"文档里说不要用"。
#![cfg_attr(
    not(feature = "test-util"),
    doc = r#"
## 生产构建门禁（本节本身就是一条会自动执行的测试）

本 crate 处于生产 feature 组合（未启用 `test-util`）时，"可指定起始序号"的
记录层构造器必须**完全不存在**：下面这段代码必须编译失败。若有人把
`*_at_seq_for_test` 改回无条件 `pub`（哪怕加 `#[doc(hidden)]`），本节立刻编译
成功、文档测试立刻变红。

```compile_fail
let _ = rsetup_protocol::crypto::client_record_layers_at_seq_for_test;
let _ = rsetup_protocol::crypto::server_record_layers_at_seq_for_test;
```
"#
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// 同一发送方向上上一条预封箱记录尚未 `commit`/`abort` 就再次预封箱：
    /// 同一序号会产出两份相同 Nonce/AAD 的记录，属 §5.2 禁止的 nonce 重用
    /// 入口。返回本错误时**不**推进序号、不消耗第二份 nonce；调用方必须
    /// 先处置（commit 或 abort）在途的那一条。
    #[error("a prepared record is still outstanding (commit or abort it first)")]
    PendingOutstanding,
    /// `commit` 收到的预封箱记录不属于本 pair、或不是本方向当前在途的那一条
    /// （跨 pair 混用、重复提交、越序提交）：调用方的写出状态机已错乱，整对
    /// 拉闸（§5.2 禁止把一条记录的 nonce 记到另一条连接的序号空间）。
    #[error("prepared record does not belong to this direction's outstanding sequence")]
    CommitMismatch,
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

/// §5.1 派生出的会话密钥四元组。字段私有、Debug 去敏；且是**一次性**消耗品：
/// 唯一出口是消耗 `self` 的 [`Self::into_client_record_layers`] /
/// [`Self::into_server_record_layers`]，一份 `SessionKeys` 只能产出**一个**
/// [`RecordLayerPair`]。本类型不实现 `Clone`/`Copy`/`Default`（新增即破坏
/// nonce 唯一性，禁止）。
///
/// 这是 H1（nonce 重用）的构造性防线：`&self` 版构造入口一旦存在，重复调用
/// 就会产生同 key/IV、seq 同为 0 的多个发送器，同 (key/IV, seq) 的 Nonce/AAD
/// 被重用直接违反 §5.2；消耗 `self` 后生产 API 结构上无法复制 key/IV 或把
/// seq 重置回 0。需要独立材料只能重新调用 [`Self::derive`]（测试同样如此）。
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

    /// 消耗会话密钥，产出板端（client）视角记录层对：tx = c2s 方向
    /// （seal 用 c2s key/iv），rx = s2c 方向（open 用 s2c key/iv）。
    ///
    /// 一次性：`self` 被移走消耗，本会话该方向的发送器**唯一存在**，双序号
    /// 从 0 起，pair 两方向此刻装配一个全新的共享 fatal 熔断位（见
    /// [`RecordLayerPair`]）。不存在任何 API 能在产出后再造一个 seq=0 的
    /// 同密钥发送器。
    pub fn into_client_record_layers(self) -> RecordLayerPair {
        self.into_pair_at(0, 0, DirectionRole::Client)
    }

    /// 消耗会话密钥，产出中控端（server）视角记录层对：tx = s2c 方向，
    /// rx = c2s 方向。一次性语义同 [`Self::into_client_record_layers`]。
    pub fn into_server_record_layers(self) -> RecordLayerPair {
        self.into_pair_at(0, 0, DirectionRole::Server)
    }

    /// `into_*_record_layers` 的共同实现：`tx_seq`/`rx_seq` 是唯一的差异点。
    fn into_pair_at(self, tx_seq: u64, rx_seq: u64, role: DirectionRole) -> RecordLayerPair {
        let (tx_key, tx_iv, rx_key, rx_iv) = match role {
            DirectionRole::Client => (self.c2s_key, self.c2s_iv, self.s2c_key, self.s2c_iv),
            DirectionRole::Server => (self.s2c_key, self.s2c_iv, self.c2s_key, self.c2s_iv),
        };
        let pair_fatal = Arc::new(AtomicBool::new(false));
        let pair_owner = Arc::new(PendingOwner);
        RecordLayerPair {
            tx: TxRecordLayer {
                state: record_state(tx_key, tx_iv, tx_seq, Arc::clone(&pair_fatal)),
                pending: None,
                pair_owner,
            },
            rx: RxRecordLayer {
                state: record_state(rx_key, rx_iv, rx_seq, pair_fatal),
            },
        }
    }
}

#[derive(Clone, Copy)]
enum DirectionRole {
    Client,
    Server,
}

/// ⚠️ **测试专用（`test-util` feature，默认关闭）**：构造"指定起始序号"的
/// 记录层对，用来在回归测试里复现"序号末值"等内部状态。
///
/// 与 [`SessionKeys::into_client_record_layers`] 一样**消耗** `SessionKeys`：
/// 一份会话密钥仍然只能产出**一个** pair，所以这里放开的只有"起始序号"，
/// 不存在"同 key/IV 重复产出 seq=0 发送器"的 nonce 重用入口，也不导出任何
/// 密钥材料。
///
/// **可见性由 feature 把守，不是 `#[doc(hidden)]`**：默认 feature 组合（生产
/// `cargo build` / `cargo build --release`）根本不编译本函数，crate 外无法
/// 引用；本模块文档的"生产构建门禁"一节就是这条断言的可执行测试。生产代码
/// 只能使用 `into_client_record_layers`（序号恒从 0 起）。
#[cfg(feature = "test-util")]
pub fn client_record_layers_at_seq_for_test(
    keys: SessionKeys,
    tx_seq: u64,
    rx_seq: u64,
) -> RecordLayerPair {
    keys.into_pair_at(tx_seq, rx_seq, DirectionRole::Client)
}

/// [`client_record_layers_at_seq_for_test`] 的 server 视角版本（同样消耗
/// `SessionKeys`、同样只在 `test-util` 下存在）。
#[cfg(feature = "test-util")]
pub fn server_record_layers_at_seq_for_test(
    keys: SessionKeys,
    tx_seq: u64,
    rx_seq: u64,
) -> RecordLayerPair {
    keys.into_pair_at(tx_seq, rx_seq, DirectionRole::Server)
}

fn expand_into(prk: &hkdf::Prk, info: &[u8], out: &mut [u8]) -> Result<(), CryptoError> {
    prk.expand(&[info], OkmLen(out.len()))
        .map_err(|_| CryptoError::Derive)?
        .fill(out)
        .map_err(|_| CryptoError::Derive)
}

// ---------- §5.2 AES-256-GCM 记录层（方向分离 + 两阶段发送） ----------

/// 单向记录层的**内部**状态，私有类型：crate 外只能拿到 [`TxRecordLayer`]
/// （只能封箱）或 [`RxRecordLayer`]（只能解封），各自只暴露本方向允许的那一半
/// 能力。这是"接收方向不得 seal、发送方向不得 open"的编译期落点：不再有
/// "两用"的 `RecordLayer` 可以被误喂对端字节或被反向消耗发送序号。
struct RecordState {
    /// 构造时装配并缓存的 AES-256-GCM key：`seal`/`open` 复用同一实例，
    /// 不再每次重建 `UnboundKey`/`LessSafeKey`（API 与线格式均不变）。
    /// `None` 表示 ring 拒绝了该密钥（密钥固定 32B、算法固定 AES_256_GCM，
    /// 正常路径不可达）：构造时不走 panic，而是连同共享熔断位一并置位，
    /// 整个 pair 后续一切操作一律拒绝（[`CryptoError::Fatal`]）。
    key: Option<aead::LessSafeKey>,
    iv: [u8; RECORD_IV_LEN],
    /// Tx 方向：下一个待发序号；Rx 方向：期望序号。生产入口恒从 0 起。
    seq: u64,
    /// pair 级共享 fatal 熔断位：同一 pair 的 tx/rx 持有同一个 `Arc`。
    /// 只会从 false 单向翻到 true（置位后永不复位）；任何失败路径在返回
    /// `Err` 前原子置位，入口原子读取。用 Relaxed 即可：这是一次性"拉闸"位，
    /// 不要求与其他内存顺序建立先后关系；跨线程的最终兜底是调用方关闭整条
    /// TCP 的动作。
    fatal: Arc<AtomicBool>,
}

impl RecordState {
    fn is_fatal(&self) -> bool {
        self.fatal.load(Ordering::Relaxed)
    }

    /// 拉闸：置位所在 pair 的共享熔断位（tx 失败 ⇒ rx 同样立即拒绝，反之
    /// 亦然）。
    fn mark_fatal(&self) {
        self.fatal.store(true, Ordering::Relaxed);
    }

    /// 取构造时缓存的 key（热路径不再重建 `LessSafeKey`），并在 ring 拒绝固定
    /// 32B 密钥时**先拉闸整对**再返回错误：不走 panic，防御性映射为
    /// [`CryptoError::Internal`]。
    fn cached_key(&self) -> Result<&aead::LessSafeKey, CryptoError> {
        match self.key.as_ref() {
            Some(key) => Ok(key),
            None => {
                self.mark_fatal();
                Err(CryptoError::Internal)
            }
        }
    }
}

/// 生产构造入口：单向记录层内部状态，key 在构造时装配一次并缓存，fatal 熔断位
/// 由同一 pair 的两个实例共享（仅 [`SessionKeys`] 的 `into_*` 方法调用本函数）。
fn record_state(
    key: [u8; RECORD_KEY_LEN],
    iv: [u8; RECORD_IV_LEN],
    seq: u64,
    pair_fatal: Arc<AtomicBool>,
) -> RecordState {
    let key = aead::UnboundKey::new(&aead::AES_256_GCM, &key)
        .map(aead::LessSafeKey::new)
        .ok();
    if key.is_none() {
        // ring 拒绝固定 32B 密钥（正常路径不可达）：立即拉闸整个 pair。
        pair_fatal.store(true, Ordering::Relaxed);
    }
    RecordState {
        key,
        iv,
        seq,
        fatal: pair_fatal,
    }
}

/// ⚠️ 仅单元测试使用（模块私有，不经生产 API、不对 crate 外暴露）。
///
/// 生产记录层只能经 [`SessionKeys`] 的 `into_*` 方法创建，序号恒从 0 起。本
/// 入口只在本模块 `#[cfg(test)] mod tests` 内可见，供测试构造"序号接近耗尽"
/// 等内部状态，以验证 §5.2 序号末值行为。返回**独立** fatal 熔断位的孤立实例
/// （不与任何 pair 共享），并带一个**孤立**的 pending 属主标记。
#[cfg(test)]
fn tx_at_seq(key: &[u8; RECORD_KEY_LEN], iv: &[u8; RECORD_IV_LEN], seq: u64) -> TxRecordLayer {
    TxRecordLayer {
        state: record_state(*key, *iv, seq, Arc::new(AtomicBool::new(false))),
        pending: None,
        pair_owner: Arc::new(PendingOwner),
    }
}

/// [`tx_at_seq`] 的接收方向版本（同样仅单元测试可见）。
#[cfg(test)]
fn rx_at_seq(key: &[u8; RECORD_KEY_LEN], iv: &[u8; RECORD_IV_LEN], seq: u64) -> RxRecordLayer {
    RxRecordLayer {
        state: record_state(*key, *iv, seq, Arc::new(AtomicBool::new(false))),
    }
}

/// pair 级"预封箱属主"标记：每个 [`RecordLayerPair`] 装配一个，
/// [`PreparedRecord`] 持有它的 `Arc`，[`TxRecordLayer::commit`] 用
/// `Arc::ptr_eq` 判定"这条 pending 究竟属于哪个 pair"。
///
/// 没有这个标记，风险面是：把 A pair 预封箱出来的记录 commit 进 B pair
/// （测试里两个 pair 甚至可以是同一份确定性 HKDF 密钥），序号恰好也对得上，
/// 于是一条记录的 nonce 被记到另一条连接的序号空间上。指针等价判定把这条路
/// 彻底堵死。
pub struct PendingOwner;

/// 一条**已封箱但未确认写出**的记录（§5.2 两阶段发送的第一阶段产物）。
///
/// 持有 wire 字节的只读视图与它占用的序号；`TxRecordLayer` 侧记下该序号，
/// 在 [`TxRecordLayer::commit`] 之前**不推进** `TxSeq`，也不允许再封第二条
/// （[`CryptoError::PendingOutstanding`]）。这样"序号被消耗"与"整条记录已经
/// 完整写出并被传输层确认"严格同步（§5.2：必须在整条记录完整成功写出之后
/// 才推进 TxSeq）。
///
/// **未 commit 就被 drop ⇒ 整个 pair 拉闸**：本类型持有 pair 的共享熔断位，
/// Drop 时发现没有 commit 就置位。也就是把"忘记 commit"与"写出结果不确定"
/// 同价处理（§5.2：任何无法确认整条记录已完整写出的情形都关闭整条 TCP）；
/// API 上不存在"这条算了、拿同一份明文再 seal 一条重发"的可能。
pub struct PreparedRecord {
    wire: Vec<u8>,
    seq: u64,
    pair_owner: Arc<PendingOwner>,
    pair_fatal: Arc<AtomicBool>,
    committed: bool,
}

impl PreparedRecord {
    /// 必须**原样、完整**写进 socket 的字节：
    /// `4B BE 长度前缀 + 密文 + 16B Tag`。写出结果不确定时禁止重发。
    pub fn wire(&self) -> &[u8] {
        &self.wire
    }

    /// 该记录占用的序号（Nonce/AAD 里的 SeqID），也是 commit 后 `TxSeq` 的落点。
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// 标记为"已完整写出"，从而 drop 时不再拉闸。仅 [`TxRecordLayer::commit`]
    /// 可调用（模块私有可见性），外部无法伪造"已提交"。
    fn mark_committed(&mut self) {
        self.committed = true;
    }
}

impl core::fmt::Debug for PreparedRecord {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 序号与长度非机密；密文/密钥绝不显示。
        f.debug_struct("PreparedRecord")
            .field("seq", &self.seq)
            .field("wire_len", &self.wire.len())
            .field("committed", &self.committed)
            .finish()
    }
}

impl Drop for PreparedRecord {
    fn drop(&mut self) {
        if !self.committed {
            // 未 commit ⇒ 无法确认这条已封箱的记录到底有没有上线：整对作废。
            self.pair_fatal.store(true, Ordering::Relaxed);
        }
    }
}

/// 一条握手会话记录层对。字段即两个**方向受限**的记录层：
/// - [`RecordLayerPair::tx`]：[`TxRecordLayer`]，**只能封箱**
///   （[`TxRecordLayer::prepare`] + [`TxRecordLayer::commit`]，或原子的
///   [`TxRecordLayer::seal`]），结构上没有 `open`；
/// - [`RecordLayerPair::rx`]：[`RxRecordLayer`]，**只能解封**
///   （[`RxRecordLayer::open`]），结构上没有 `seal`/`prepare`/`commit`。
///
/// 于是"拿接收方向去 seal"（把对端字节当明文二次封箱、反向消耗发送序号）与
/// "拿发送方向去 open"（把本地写出缓冲当作对端记录解封）都不再是运行时风险，
/// 而是编译错误（下面两个 `compile_fail` 示例把这两条钉死）。
///
/// **fatal 语义按 pair 整体判定且由代码强制（§5.2）**：两个方向共享同一个
/// fatal 熔断位（`Arc<AtomicBool>`）。任一侧 `seal`/`prepare`/`commit`/`open`
/// 的任何失败（认证、长度、帧、序号耗尽、pending 归属不符、原语错误）或
/// [`Self::mark_write_failed`] 都会置位该共享位，**此后两个方向（含未直接
/// 失败的那一侧）的一切操作立即返回 [`CryptoError::Fatal`]**——不是"调用方
/// 检查 `is_fatal()` 后才停用"的口头契约：即使经由公开字段直接调用对向，也
/// 不可能出现"一侧失败、另一侧仍可用"。
///
/// 因此任一侧失败（[`Self::is_fatal`] 为 true）后整对即不可用：调用方必须
/// 关闭整条 TCP 连接并丢弃本 pair、重新握手，不得只停用失败方向、不得在原
/// 连接重同步、不得跨重连复用密钥。
///
/// ```compile_fail
/// use rsetup_protocol::crypto::RxRecordLayer;
/// // 接收方向结构上没有 seal/prepare：对端字节不可能被二次封箱。
/// fn must_not_compile(rx: &mut RxRecordLayer) {
///     let _ = rx.seal(b"peer bytes must never be sealed");
/// }
/// ```
///
/// ```compile_fail
/// use rsetup_protocol::crypto::TxRecordLayer;
/// // 发送方向结构上没有 open：本地写出缓冲不可能被当作对端记录解封。
/// fn must_not_compile(tx: &mut TxRecordLayer) {
///     let _ = tx.open(&[0, 0, 0, 16]);
/// }
/// ```
#[derive(Debug)]
pub struct RecordLayerPair {
    /// 发送方向（只能 seal/prepare/commit）。与本 pair 的 `rx` 共享 fatal 熔断位。
    pub tx: TxRecordLayer,
    /// 接收方向（只能 open）。与本 pair 的 `tx` 共享 fatal 熔断位。
    pub rx: RxRecordLayer,
}

impl RecordLayerPair {
    /// 本 pair 是否已整体致命：两个方向共享同一个熔断位，任一侧失败（或
    /// [`Self::mark_write_failed`]）即 true。
    ///
    /// 返回 true 后本 pair 的两个方向在代码层面已全部拒绝一切 seal/prepare/
    /// commit/open（无需调用方再做检查才停用）；调用方必须关闭整条 TCP 连接
    /// （§5.2"该连接全部密钥/IV/序号立即失效"）、丢弃本 pair 并重新握手，
    /// 禁止继续读取对端数据、禁止只替换单侧方向、禁止重试。
    pub fn is_fatal(&self) -> bool {
        self.tx.is_fatal() || self.rx.is_fatal()
    }

    /// 把整对置为 fatal（置共享熔断位），供**网络写出侧**在记录写出结果不
    /// 确定时调用。
    ///
    /// [`TxRecordLayer::seal`] / [`TxRecordLayer::commit`] 成功即意味着该序号
    /// 对应的 Nonce/AAD 组合已被消耗；若随后 socket 写出失败或只写出部分
    /// 字节，对端可能收到半条记录或收不到记录，本端无法判断对端的接收序号
    /// 状态，继续发送只会让两侧序号永久错位（后续记录一律认证失败）。因此
    /// 这种状态下**整对记录层必须作废、禁止重发或重试**：调用本函数置位共享
    /// 熔断位（[`Self::is_fatal`] 随即为 true，两方向的一切操作一律返回
    /// [`CryptoError::Fatal`]），然后关闭整条 TCP 连接。
    pub fn mark_write_failed(&mut self) {
        self.tx.mark_pair_fatal();
        self.rx.mark_pair_fatal();
    }
}

/// 发送方向记录层（§5.2）：持有该方向的 AES-256-GCM key（构造时装配一次并
/// 缓存）、IV-Base 与发送序号 `TxSeq`。
///
/// **只有封箱能力**：本类型不提供 `open`，对端字节在类型层面就进不来。同方向
/// 调用串行化由调用方保证（§5.2"同一方向不得并发交错"）；本类型再用
/// `&mut self` + "至多一条在途 pending"（[`CryptoError::PendingOutstanding`]）
/// 从结构上堵死"同序号封两次"。
///
/// 任何失败都会置位所在 [`RecordLayerPair`] 的共享熔断位，此后两个方向的一切
/// 操作拒绝，对应规范"该连接全部密钥/IV/序号立即失效，不得重同步"。
pub struct TxRecordLayer {
    state: RecordState,
    /// 在途未 commit 的预封箱序号（`prepare` 与 `commit` 之间）。
    pending: Option<u64>,
    /// 本 pair 的 pending 属主标记（见 [`PendingOwner`]）。
    pair_owner: Arc<PendingOwner>,
}

impl core::fmt::Debug for TxRecordLayer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 序号、pending 与 fatal 位非机密，便于日志定位；key/iv 绝不显示。
        f.debug_struct("TxRecordLayer")
            .field("tx_seq", &self.state.seq)
            .field("pending", &self.pending)
            .field("fatal", &self.state.is_fatal())
            .finish()
    }
}

/// 接收方向记录层（§5.2）：持有该方向的 key、IV-Base 与期望序号 `RcvSeq`。
///
/// **只有解封能力**：本类型不提供 `seal`/`prepare`/`commit`，"把接收方向当
/// 发送方向用"编译不过。校验顺序固定：fatal → **序号耗尽** → 前缀完整性 →
/// 长度字段（先校验预算再分配）→ 帧完整性 → tag/AAD 认证；任何失败都不交付
/// 明文、不推进 `RcvSeq`，并拉闸整对（含对向发送方向）。
pub struct RxRecordLayer {
    state: RecordState,
}

impl core::fmt::Debug for RxRecordLayer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RxRecordLayer")
            .field("rx_seq", &self.state.seq)
            .field("fatal", &self.state.is_fatal())
            .finish()
    }
}

impl TxRecordLayer {
    /// 本方向（即所在 pair）是否已致命：读的是 [`RecordLayerPair`] 的**共享**
    /// 熔断位，因此对向 `rx` 失败后这里同样是 true。
    pub fn is_fatal(&self) -> bool {
        self.state.is_fatal()
    }

    /// 下一个待发送序号（`commit` 之后才会前进）。非机密，供传输层判断"末
    /// 有效序号是否已发出"（发出 `2^64-2` 后 `TxSeq == u64::MAX`）。
    pub fn tx_seq(&self) -> u64 {
        self.state.seq
    }

    /// 供 [`RecordLayerPair::mark_write_failed`] 使用的内部拉闸入口。
    fn mark_pair_fatal(&self) {
        self.state.mark_fatal();
    }

    /// 第一阶段：封箱一条记录但**不**推进 `TxSeq`（§5.2 两阶段发送）。
    ///
    /// 输出与 [`Self::seal`] 完全一致的线格式字节（`4B BE 长度 + 密文 + Tag`），
    /// 由调用方负责**完整**写出；完整写出并得到传输层确认后必须调用
    /// [`Self::commit`] 推进序号。写出结果不确定（失败、部分写出、任务被取消、
    /// 直接 drop 这条 pending）⇒ 整对作废（`Drop` 兜底拉闸），禁止重发。
    ///
    /// 检查顺序与 [`Self::seal`] 相同：fatal → 在途 pending → 长度上限 →
    /// 序号耗尽 → 原语。在途已有一条未 commit 的记录时返回
    /// [`CryptoError::PendingOutstanding`]，且**不**消耗第二份 nonce、不拉闸
    /// （这是 nonce 重用的结构性防线，也是"不得重复 seal/重发"的运行时约束）。
    pub fn prepare(&mut self, plaintext: &[u8]) -> Result<PreparedRecord, CryptoError> {
        if self.state.is_fatal() {
            return Err(CryptoError::Fatal);
        }
        if self.pending.is_some() {
            // 未 commit 就再封一条 = 同一 (key/iv, seq) 的第二份 nonce。
            return Err(CryptoError::PendingOutstanding);
        }
        if plaintext.len() > MAX_PLAINTEXT_LEN {
            // 记录体将超过 65536 上限：协议级致命（§5.2 长度上限），整对拉闸。
            self.state.mark_fatal();
            return Err(CryptoError::RecordTooLarge);
        }
        if self.state.seq == u64::MAX {
            // 有效序号仅 0..=2^64-2；上一记录（2^64-2）已发出，必须收尾。
            self.state.mark_fatal();
            return Err(CryptoError::SequenceExhausted);
        }
        let seq = self.state.seq;
        let body_len = plaintext.len() + GCM_TAG_LEN; // ≤ 65536，u32 安全
        let nonce = aead::Nonce::assume_unique_for_key(build_nonce(&self.state.iv, seq));
        let aad = build_aad(body_len as u32, seq);
        let key = self.state.cached_key()?;

        let mut out = Vec::with_capacity(RECORD_LEN_PREFIX_LEN + body_len);
        out.extend_from_slice(&(body_len as u32).to_be_bytes());
        out.extend_from_slice(plaintext);
        // Tag 为独立拥有所有权的值，match 分支内已无对 self 的借用。
        let sealed = key.seal_in_place_separate_tag(
            nonce,
            aead::Aad::from(aad),
            &mut out[RECORD_LEN_PREFIX_LEN..],
        );
        let tag = match sealed {
            Ok(tag) => tag,
            Err(_) => {
                self.state.mark_fatal();
                return Err(CryptoError::Internal);
            }
        };
        out.extend_from_slice(tag.as_ref());
        // 序号留到 commit 才推进；此处只登记"在途 pending"，同序号不得再封。
        self.pending = Some(seq);
        Ok(PreparedRecord {
            wire: out,
            seq,
            pair_owner: Arc::clone(&self.pair_owner),
            pair_fatal: Arc::clone(&self.state.fatal),
            committed: false,
        })
    }

    /// 第二阶段：确认这条预封箱记录已**整条完整写出**并得到传输层确认，
    /// 推进 `TxSeq`（§5.2）。
    ///
    /// 返回被提交的序号；调用方据此判断是否已发出末有效序号（`2^64-2`），
    /// 发出后必须主动平滑关闭整条 TCP（不再发送任何记录）。
    ///
    /// 只有同时满足"属于本 pair（`Arc::ptr_eq` 属主判定）"且"正是本方向当前
    /// 在途的那一条（序号与 `TxSeq` 一致）"才允许提交；否则说明调用方的写出
    /// 状态机已经错乱（跨 pair 混用、重复提交、越序提交），一律拉闸整对。
    ///
    /// 本函数**不会**再次封箱，因此不存在"commit 时重新 seal 导致 nonce 重用"。
    pub fn commit(&mut self, mut pending: PreparedRecord) -> Result<u64, CryptoError> {
        let same_pair = Arc::ptr_eq(&self.pair_owner, &pending.pair_owner);
        let is_current = self.pending == Some(pending.seq) && self.state.seq == pending.seq;
        if self.state.is_fatal() || !same_pair || !is_current {
            // 归属/序号对不上：这条记录的写出结果已无法归属到本方向的任何一次
            // 发送 ⇒ 整对作废（显式置位，语义不依赖 drop 时机）。
            self.state.mark_fatal();
            pending.pair_fatal.store(true, Ordering::Relaxed);
            return Err(CryptoError::CommitMismatch);
        }
        let seq = pending.seq;
        // seq < u64::MAX 已在 prepare 入口守卫，不会回绕。
        self.state.seq = seq + 1;
        self.pending = None;
        pending.mark_committed();
        Ok(seq)
    }

    /// 放弃在途的预封箱记录（写出结果不确定时的显式出口）：返回 `true` 表示
    /// 确实作废了一条，并拉闸整对（§5.2：部分写出/写出结果不确定 = 关闭整条
    /// TCP）。没有在途记录时返回 `false` 且不改变任何状态。
    pub fn abort_pending(&mut self) -> bool {
        let had = self.pending.take().is_some();
        if had {
            self.state.mark_fatal();
        }
        had
    }

    /// 原子便捷入口（预封箱 + 立即 commit），输出恰为
    /// `4B BE 长度前缀 + AES-256-GCM 密文 + 16B Tag`；长度字段只计密文+Tag
    /// （16..=65536），明文上限 65520B，允许零明文。
    ///
    /// ⚠️ **网络写出侧（`SecureIo` 等生产路径）不得使用本方法**：它一返回就已
    /// 推进 `TxSeq`，调用方若随后写出失败/只写出部分字节，只能整对作废，且
    /// 无法表达"还没写完"这个中间态。生产写出必须走 [`Self::prepare`] → 完整
    /// 写出 → [`Self::commit`]，不确定时 [`Self::abort_pending`]（或直接 drop
    /// 那条 pending）。本方法保留给纯内存的库内测试与"封箱结果不落网络"的
    /// 场景（`tests/crypto.rs`、`handshake_adapter::EstablishedTunnel` 等）。
    ///
    /// # 成功返回即已推进 TxSeq，禁止重试
    ///
    /// 返回 `Ok(record)` 的那一刻本方向 `tx_seq` 已加 1，该序号对应的
    /// Nonce/AAD 组合即被**永久消耗**：
    /// - 必须**原样、一次性**写入 socket 并交给对端；
    /// - **禁止**用同一入参明文重新 `seal` 后重发（会得到不同序号，对端立刻
    ///   因序号错位而认证失败）；
    /// - **禁止**在写失败后重发同一 `record`（对端可能已收下并推进了 RcvSeq），
    ///   唯一正确处置是 [`RecordLayerPair::mark_write_failed`] 后关闭整条 TCP。
    ///
    /// # 与 `open` 的错误优先序不同（有意选择，不是遗漏）
    ///
    /// 本方向：fatal → 在途 pending → **长度上限** → 序号耗尽 → 原语/tag；
    /// [`RxRecordLayer::open`] 则是 fatal → **序号耗尽** → 长度/帧 → tag。
    /// 差异只在"同时违规时先报哪一个"：`seal`/`prepare` 的明文长度是调用方
    /// 自己的入参错误，先报 `RecordTooLarge` 才能把"用错 API"和"会话到头了
    /// 该收尾"区分开；`open` 面对的是不可信对端字节，期望序号已到 `2^64-1`
    /// 时无需再解析这些字节。两条路径**一律 fatal**（`RecordTooLarge` 与
    /// `SequenceExhausted` 都先拉闸再返回），调用方处置完全相同。
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let pending = self.prepare(plaintext)?;
        let wire = pending.wire().to_vec();
        self.commit(pending)?;
        Ok(wire)
    }
}

impl RxRecordLayer {
    /// 本方向（即所在 pair）是否已致命：读的是共享熔断位，对向 `tx` 失败后
    /// 这里同样是 true。
    pub fn is_fatal(&self) -> bool {
        self.state.is_fatal()
    }

    /// 当前期望序号（认证通过才前进）。非机密，供传输层与测试观测。
    pub fn rx_seq(&self) -> u64 {
        self.state.seq
    }

    /// 供 [`RecordLayerPair::mark_write_failed`] 使用的内部拉闸入口。
    fn mark_pair_fatal(&self) {
        self.state.mark_fatal();
    }

    /// 解封一条完整记录（§5.2）。输入必须是**恰好一条完整记录**（含 4B 长度
    /// 前缀，总长 = 4 + 声明长度）；分帧/组包由调用方（`RecordReassembler`、
    /// `SecureIo`）负责。
    ///
    /// 校验顺序固定：fatal → **序号耗尽** → 前缀完整性 → 长度字段（先校验
    /// 预算再分配）→ 帧完整性 → tag/AAD 认证。序号耗尽检查**提前**到长度/帧
    /// 校验之前：期望序号已到 `2^64-1` 时无论来帧是否合法都返回
    /// [`CryptoError::SequenceExhausted`]，长度违规不会先命中。两条路径都只是
    /// 错误码不同：**一律拉闸整对共享熔断位**（对向 `seal` 同样必拒绝），
    /// 调用方一律关闭整条 TCP。任何失败都不交付明文、不推进 `rx_seq`、不在
    /// 原连接重同步。
    ///
    /// 与 [`TxRecordLayer::seal`] 的"先长度后耗尽"相反，本方向**故意**先判
    /// 耗尽：差异只在错误码，二者均为 fatal、状态影响完全相同（`seal` 一侧的
    /// 说明见 [`TxRecordLayer::seal`]）。
    ///
    /// 防御性原语路径也不例外：`cached_key()` 失败时先置共享熔断位再返回
    /// [`CryptoError::Internal`]，tag/AAD 认证失败同样先拉闸再返回
    /// [`CryptoError::AuthFailed`]——本模块不存在"返回错误但整对仍可用"的路径。
    pub fn open(&mut self, record: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if self.state.is_fatal() {
            return Err(CryptoError::Fatal);
        }
        if self.state.seq == u64::MAX {
            // 期望序号到达 2^64-1：先于长度/帧校验终止（§5.2 接收端规则）。
            self.state.mark_fatal();
            return Err(CryptoError::SequenceExhausted);
        }
        if record.len() < RECORD_LEN_PREFIX_LEN {
            self.state.mark_fatal();
            return Err(CryptoError::BadFrame);
        }
        let mut len_be = [0u8; RECORD_LEN_PREFIX_LEN];
        len_be.copy_from_slice(&record[..RECORD_LEN_PREFIX_LEN]);
        let body_len_be = u32::from_be_bytes(len_be);
        // §5.2：先校验长度与缓冲预算，再分配。
        if body_len_be < MIN_RECORD_BODY_LEN as u32 {
            self.state.mark_fatal();
            return Err(CryptoError::RecordTooSmall);
        }
        if body_len_be > MAX_RECORD_BODY_LEN as u32 {
            self.state.mark_fatal();
            return Err(CryptoError::RecordTooLarge);
        }
        let body_len = body_len_be as usize;
        if record.len() != RECORD_LEN_PREFIX_LEN + body_len {
            // 截断或多余尾随字节：不是恰好一条完整记录。
            self.state.mark_fatal();
            return Err(CryptoError::BadFrame);
        }
        let seq = self.state.seq;

        // 在仅借用 state（缓存 key、iv）的作用域内完成认证，并把结果转成
        // 不借用 self 的 owned 值，之后才更新 rx_seq / 拉闸。
        let opened = {
            let key = self.state.cached_key()?;
            let nonce = aead::Nonce::assume_unique_for_key(build_nonce(&self.state.iv, seq));
            let aad = build_aad(body_len_be, seq);
            let mut buf = record[RECORD_LEN_PREFIX_LEN..].to_vec();
            match key.open_in_place(nonce, aead::Aad::from(aad), &mut buf) {
                Ok(plaintext) => Ok(plaintext.to_vec()),
                Err(_) => Err(CryptoError::AuthFailed),
            }
        };
        match opened {
            Ok(out) => {
                // 认证成功才推进；seq < u64::MAX 已在入口守卫，不会回绕。
                self.state.seq = seq + 1;
                Ok(out)
            }
            Err(err) => {
                // tag/AAD 失败：不交付明文、不推进 rx_seq、整对拉闸，
                // 原连接不可重同步。
                self.state.mark_fatal();
                Err(err)
            }
        }
    }
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

// ---------- 单元测试 ----------

#[cfg(test)]
mod tests {
    //! 单元测试：只走生产 API 与模块私有的测试构造入口
    //! [`tx_at_seq`] / [`rx_at_seq`]；密钥四元组通过私有字段直接读取，crate 外
    //! （`tests/crypto.rs` 集成测试）拿不到任何导出密钥的入口。需要多份
    //! 独立记录层材料时，每份都重新调用 [`SessionKeys::derive`]（`into_*`
    //! 消耗 `self`，一份密钥只出一个 pair）。
    //!
    //! 覆盖：§5.1 HKDF 逐字节钉死（测试侧用 `ring::hmac` 独立实现
    //! HKDF-Extract/Expand，不经过被测 [`SessionKeys::derive`]）、Salt 的
    //! client/server random 顺序绑定、c2s/s2c 方向不互换、§5.2 序号末值、
    //! 线格式与规范参考实现互通、pair 共享 fatal 熔断位（单侧失败 ⇒ 对向
    //! 必拒绝）与写出失败作废语义、`SessionKeys` 一次性（消耗 self）的编译期
    //! 钉死。

    use super::*;
    use ring::hmac;

    const CLIENT_RANDOM: [u8; 32] = [0x11; 32];
    const SERVER_RANDOM: [u8; 32] = [0x22; 32];
    const SHARED: [u8; SHARED_SECRET_LEN] = [0x5A; SHARED_SECRET_LEN];

    // ---------- 期望常量 ----------
    //
    // 由**独立于本 crate 与 ring** 的 HKDF-SHA256 实现（Python 标准库
    // `hmac`/`hashlib` 手写 RFC 5869，先用 RFC 5869 A.1 测试向量自校验，再与
    // `cryptography` 库的 HKDF 交叉验证）离线算出后逐字钉死。输入固定为
    // `IKM = 0x5A×32`、`client_random = 0x11×32`、`server_random = 0x22×32`。
    const EXPECT_PRK: &str = "1884299bf16f68246f6212724ea76fa9406355cc305c7495bcd26c181d594933";
    const EXPECT_C2S_KEY: &str = "2ff93d4762ecdeb633138898ad2ba047a1943395c07c8bbd9ee7b9bdb919eadb";
    const EXPECT_S2C_KEY: &str = "cc243c870e367b998ecd6ff70fba95f0352bb1c6114efbc2db3d7fc84aa31392";
    const EXPECT_C2S_IV: &str = "5d9606d2c06236f20d76c7ac";
    const EXPECT_S2C_IV: &str = "210e549d74c5980e2f419a3f";
    /// Salt 字节序交换（`server_random || client_random`）后的 c2s-key，
    /// 用于钉死"random 顺序参与派生"。
    const EXPECT_C2S_KEY_SWAPPED_SALT: &str =
        "aa06be06f8a7a74dbd738c60b659e60dbe04e1b4deedf972076fc3a89785ead6";

    fn unhex(s: &str) -> Vec<u8> {
        hex::decode(s).expect("fixture 必须是合法 hex")
    }

    // ---------- 测试侧独立 HKDF-SHA256（只用 ring::hmac 原语，RFC 5869） ----------

    fn t_hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
        hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data)
            .as_ref()
            .to_vec()
    }

    /// `PRK = HMAC-Hash(salt, IKM)`（HKDF 第一步，零填充到 HashLen 的规则由
    /// HMAC 自身对任意 key 长度的处理保证，salt 恰为 64B = 一个 HMAC block）。
    fn t_extract(salt: &[u8], ikm: &[u8]) -> Vec<u8> {
        t_hmac(salt, ikm)
    }

    /// `T(n) = HMAC(PRK, T(n-1) || info || i)`，`OKM = T(1)||T(2)||…` 截断到 len。
    fn t_expand(prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut prev: Vec<u8> = Vec::new();
        for i in 1u8.. {
            let mut data = prev;
            data.extend_from_slice(info);
            data.push(i);
            prev = t_hmac(prk, &data);
            out.extend_from_slice(&prev);
            if out.len() >= len {
                break;
            }
        }
        out.truncate(len);
        out
    }

    /// 自校验：测试侧 HKDF 实现必须精确复现 RFC 5869 A.1（HMAC-SHA-256），
    /// 否则下面的逐字节钉死毫无意义（含跨 3 个 HMAC block 的多块展开）。
    #[test]
    fn test_side_hkdf_matches_rfc5869_appendix_a1() {
        let ikm = [0x0bu8; 22];
        let salt = unhex("000102030405060708090a0b0c");
        let info = unhex("f0f1f2f3f4f5f6f7f8f9");
        let prk = t_extract(&salt, &ikm);
        assert_eq!(
            hex::encode(&prk),
            "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"
        );
        assert_eq!(
            hex::encode(t_expand(&prk, &info, 42)),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
    }

    /// §5.1 逐字节钉死：独立 HKDF 的四组输出 == 固定期望常量 ==
    /// 被测 `SessionKeys::derive` 的私有字段；同时钉死 Salt 拼装顺序与
    /// 四条 `info` 字面量。
    #[test]
    fn hkdf_session_material_pinned_byte_for_byte() {
        // info 字面量逐字钉死（改一个字符即派生材料全变，必须显式改期望）。
        assert_eq!(INFO_C2S_KEY, b"rsetup-next/tunnel/c2s-key");
        assert_eq!(INFO_S2C_KEY, b"rsetup-next/tunnel/s2c-key");
        assert_eq!(INFO_C2S_IV, b"rsetup-next/tunnel/c2s-iv");
        assert_eq!(INFO_S2C_IV, b"rsetup-next/tunnel/s2c-iv");

        // Salt = client_random || server_random，顺序在此钉死。
        let mut salt = Vec::new();
        salt.extend_from_slice(&CLIENT_RANDOM);
        salt.extend_from_slice(&SERVER_RANDOM);
        assert_eq!(salt.len(), HKDF_SALT_LEN);

        let prk = t_extract(&salt, &SHARED);
        assert_eq!(hex::encode(&prk), EXPECT_PRK, "HKDF-Extract PRK");

        let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
        let cases: [(&[u8], &str, usize); 4] = [
            (INFO_C2S_KEY, EXPECT_C2S_KEY, RECORD_KEY_LEN),
            (INFO_S2C_KEY, EXPECT_S2C_KEY, RECORD_KEY_LEN),
            (INFO_C2S_IV, EXPECT_C2S_IV, RECORD_IV_LEN),
            (INFO_S2C_IV, EXPECT_S2C_IV, RECORD_IV_LEN),
        ];
        for (info, expect, len) in cases {
            let okm = t_expand(&prk, info, len);
            assert_eq!(hex::encode(&okm), expect, "独立 HKDF 输出 info={info:?}");
            assert_eq!(okm.len(), len);
        }

        // 被测实现必须与独立实现逐字节一致（四组全部）。
        assert_eq!(keys.c2s_key.as_slice(), unhex(EXPECT_C2S_KEY).as_slice());
        assert_eq!(keys.s2c_key.as_slice(), unhex(EXPECT_S2C_KEY).as_slice());
        assert_eq!(keys.c2s_iv.as_slice(), unhex(EXPECT_C2S_IV).as_slice());
        assert_eq!(keys.s2c_iv.as_slice(), unhex(EXPECT_S2C_IV).as_slice());
    }

    /// Salt 顺序与方向绑定：交换 client/server random 四组输出全部变化；
    /// c2s 与 s2c 的 key/iv 互不相同、不可互换。
    #[test]
    fn hkdf_salt_order_and_direction_not_interchangeable() {
        let base = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
        let swapped =
            SessionKeys::derive(&SHARED, &SERVER_RANDOM, &CLIENT_RANDOM).expect("derive swap");

        // 交换 client/server random（= Salt 字节序交换）后四组输出必须全变。
        assert_ne!(
            base.c2s_key, swapped.c2s_key,
            "c2s key 必须绑定 random 顺序"
        );
        assert_ne!(
            base.s2c_key, swapped.s2c_key,
            "s2c key 必须绑定 random 顺序"
        );
        assert_ne!(base.c2s_iv, swapped.c2s_iv, "c2s iv 必须绑定 random 顺序");
        assert_ne!(base.s2c_iv, swapped.s2c_iv, "s2c iv 必须绑定 random 顺序");
        // 交换后的 c2s-key 与独立实现的期望常量一致。
        let mut swapped_salt = Vec::new();
        swapped_salt.extend_from_slice(&SERVER_RANDOM);
        swapped_salt.extend_from_slice(&CLIENT_RANDOM);
        let swapped_prk = t_extract(&swapped_salt, &SHARED);
        assert_eq!(
            hex::encode(t_expand(&swapped_prk, INFO_C2S_KEY, RECORD_KEY_LEN)),
            EXPECT_C2S_KEY_SWAPPED_SALT
        );
        assert_eq!(
            swapped.c2s_key.as_slice(),
            unhex(EXPECT_C2S_KEY_SWAPPED_SALT).as_slice()
        );

        // 方向不互换：同一 PRK 下 c2s 与 s2c 的 key/iv 两两不同。
        assert_ne!(base.c2s_key, base.s2c_key);
        assert_ne!(base.c2s_iv, base.s2c_iv);
        // 派生材料也不是"同 key 换 iv"这类退化关系。
        assert_ne!(base.c2s_key.as_slice(), base.c2s_iv.as_slice());

        // 记录层方向绑定：c2s 方向的记录拿到 s2c 方向解封必然认证失败。
        let mut client = base.into_client_record_layers();
        let rec = client.tx.seal(b"direction probe").expect("seal c2s");
        assert_eq!(client.rx.open(&rec), Err(CryptoError::AuthFailed));
        // 单侧 rx 失败 ⇒ 共享熔断位拉闸：对向 tx.seal 必拒绝，整对 fatal。
        assert!(client.is_fatal());
        assert!(client.tx.is_fatal(), "rx 失败 ⇒ tx 也必须 fatal");
        assert_eq!(client.tx.seal(b"dead pair"), Err(CryptoError::Fatal));
    }

    /// 记录层类型必须可跨任务搬移（缓存 `LessSafeKey` 后仍需 Send + Sync）。
    #[test]
    fn record_types_remain_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<EphemeralKeyPair>();
        assert_send_sync::<SessionKeys>();
        assert_send_sync::<TxRecordLayer>();
        assert_send_sync::<RxRecordLayer>();
        assert_send_sync::<PreparedRecord>();
        assert_send_sync::<RecordLayerPair>();
    }

    // ---------- §5.2 规范参考实现 ----------

    /// Nonce[0:4] = IV[0:4]；Nonce[4:12] = IV[4:12] XOR BE64(seq)。
    fn ref_nonce(iv: &[u8; 12], seq: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&iv[..4]);
        let mut tail = [0u8; 8];
        tail.copy_from_slice(&iv[4..]);
        let xored = u64::from_be_bytes(tail) ^ seq;
        n[4..].copy_from_slice(&xored.to_be_bytes());
        n
    }

    /// AAD = BE32(Record Length) || BE64(SeqID)。
    fn ref_aad(body_len: u32, seq: u64) -> [u8; 12] {
        let mut a = [0u8; 12];
        a[..4].copy_from_slice(&body_len.to_be_bytes());
        a[4..].copy_from_slice(&seq.to_be_bytes());
        a
    }

    /// 按 §5.2 拼装：4B BE(密文+Tag 长度) || 密文 || Tag。
    fn ref_seal(key: &[u8; 32], iv: &[u8; 12], seq: u64, plaintext: &[u8]) -> Vec<u8> {
        let body_len = (plaintext.len() + GCM_TAG_LEN) as u32;
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, key).expect("ref key"),
        );
        let mut out = plaintext.to_vec();
        let tag = key
            .seal_in_place_separate_tag(
                aead::Nonce::assume_unique_for_key(ref_nonce(iv, seq)),
                aead::Aad::from(ref_aad(body_len, seq)),
                &mut out,
            )
            .expect("ref seal");
        let mut record = body_len.to_be_bytes().to_vec();
        record.extend_from_slice(&out);
        record.extend_from_slice(tag.as_ref());
        record
    }

    fn ref_open(key: &[u8; 32], iv: &[u8; 12], seq: u64, record: &[u8]) -> Option<Vec<u8>> {
        let mut len_be = [0u8; 4];
        len_be.copy_from_slice(&record[..4]);
        let body_len = u32::from_be_bytes(len_be);
        let key = aead::LessSafeKey::new(
            aead::UnboundKey::new(&aead::AES_256_GCM, key).expect("ref key"),
        );
        let mut buf = record[RECORD_LEN_PREFIX_LEN..].to_vec();
        key.open_in_place(
            aead::Nonce::assume_unique_for_key(ref_nonce(iv, seq)),
            aead::Aad::from(ref_aad(body_len, seq)),
            &mut buf,
        )
        .ok()
        .map(|pt| pt.to_vec())
    }

    /// 线格式与规范参考实现逐字节互通（含序号末值附近），锁定 Nonce、AAD、
    /// 大端长度前缀。
    #[test]
    fn wire_format_matches_spec_reference_implementation() {
        let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
        for seq in [0u64, 1, 2, 7, 1000, u64::MAX - 1] {
            let msg = format!("spec-fidelity-{seq}");

            let mut tx = tx_at_seq(&keys.c2s_key, &keys.c2s_iv, seq);
            let mine = tx.seal(msg.as_bytes()).expect("seal");
            let theirs = ref_seal(&keys.c2s_key, &keys.c2s_iv, seq, msg.as_bytes());
            assert_eq!(mine, theirs, "seq={seq} c2s 线格式");

            // 反向：参考实现封箱 → 被测层解封。
            let mut rx = rx_at_seq(&keys.s2c_key, &keys.s2c_iv, seq);
            let rec = ref_seal(&keys.s2c_key, &keys.s2c_iv, seq, msg.as_bytes());
            assert_eq!(rx.open(&rec).expect("open ref"), msg.as_bytes());
            assert_eq!(
                ref_open(&keys.s2c_key, &keys.s2c_iv, seq, rec.as_slice()).as_deref(),
                Some(msg.as_bytes())
            );
        }
    }

    /// 发送端序号末值：`2^64-2` 可发，再发即 SequenceExhausted→Fatal。
    #[test]
    fn send_side_sequence_last_valid_then_exhausted() {
        let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
        let mut tx = tx_at_seq(&keys.c2s_key, &keys.c2s_iv, u64::MAX - 1);
        let record = tx
            .seal(b"tail record")
            .expect("seq 2^64-2 must be sendable");
        assert_eq!(&record[..4], &27u32.to_be_bytes(), "11B 明文 + 16B Tag");

        let mut rx = rx_at_seq(&keys.c2s_key, &keys.c2s_iv, u64::MAX - 1);
        assert_eq!(rx.open(&record).expect("open last"), b"tail record");

        assert_eq!(
            tx.seal(b"one too many"),
            Err(CryptoError::SequenceExhausted)
        );
        assert_eq!(tx.seal(b"and again"), Err(CryptoError::Fatal));
        assert!(tx.is_fatal());
    }

    /// 接收端序号耗尽：期望序号 `2^64-1` 时一律拒绝，且**先于**长度校验命中。
    #[test]
    fn receive_side_sequence_exhausted_at_max() {
        let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");

        let mut rx = rx_at_seq(&keys.c2s_key, &keys.c2s_iv, u64::MAX);
        let bogus = ref_seal(&keys.c2s_key, &keys.c2s_iv, u64::MAX, b"never acceptable");
        assert_eq!(rx.open(&bogus), Err(CryptoError::SequenceExhausted));
        assert_eq!(rx.open(&bogus), Err(CryptoError::Fatal));

        // 顺序检查：序号耗尽与"长度字段非法"同时存在时报 SequenceExhausted
        // （耗尽检查已提前到长度校验之前），两者都是 fatal。
        let mut rx = rx_at_seq(&keys.c2s_key, &keys.c2s_iv, u64::MAX);
        let too_small = 1u32.to_be_bytes().to_vec();
        assert_eq!(rx.open(&too_small), Err(CryptoError::SequenceExhausted));
        assert!(rx.is_fatal());

        // 成功解封 2^64-2 后 RcvSeq 推到 2^64-1，下一条即便合法也拒绝。
        let mut tx = tx_at_seq(&keys.c2s_key, &keys.c2s_iv, u64::MAX - 1);
        let last = tx.seal(b"last").expect("seal last");
        let beyond = ref_seal(&keys.c2s_key, &keys.c2s_iv, u64::MAX, b"beyond");
        let mut rx2 = rx_at_seq(&keys.c2s_key, &keys.c2s_iv, u64::MAX - 1);
        assert_eq!(rx2.open(&last).expect("open seq 2^64-2"), b"last");
        assert_eq!(rx2.open(&beyond), Err(CryptoError::SequenceExhausted));
    }

    /// `SessionKeys` 的唯一出口按值消耗 `self`（一次性）：函数指针签名编译期
    /// 钉死。若有人把 `into_*` 改回 `&self`（可重复调用 ⇒ 同 key/IV、seq=0 的
    /// 多个发送器，nonce 重用），下面两行立刻编译失败。
    #[test]
    fn session_keys_into_layers_take_self_by_value() {
        let _client: fn(SessionKeys) -> RecordLayerPair = SessionKeys::into_client_record_layers;
        let _server: fn(SessionKeys) -> RecordLayerPair = SessionKeys::into_server_record_layers;
    }

    /// pair 级共享 fatal 熔断位：
    /// ① 单侧 `rx.open` 失败 ⇒ 对向 `tx.seal` 必拒绝；
    /// ② 单侧 `tx.seal` 失败 ⇒ 对向 `rx.open` 必拒绝（哪怕喂给它的记录本身
    ///    完全合法）；
    /// ③ `mark_write_failed`（写出失败/部分写入）整对作废、禁止重试。
    #[test]
    fn pair_shares_single_fatal_bit() {
        let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
        // 预先离线生成一条对 pair.rx（s2c 方向、seq 0）完全合法的记录：
        // 若共享熔断位失效，rx 对它本可成功解封，拒绝断言才有区分度。
        let s2c_key = keys.s2c_key;
        let s2c_iv = keys.s2c_iv;
        let valid_s2c = ref_seal(&s2c_key, &s2c_iv, 0, b"would open if not killed");

        // ① rx 失败 ⇒ tx 同样 fatal 且 seal 必拒绝。
        let mut pair = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM)
            .expect("derive")
            .into_client_record_layers();
        assert!(!pair.is_fatal());
        assert_eq!(pair.rx.open(b"abc"), Err(CryptoError::BadFrame));
        assert!(pair.rx.is_fatal());
        assert!(pair.tx.is_fatal(), "共享熔断位：rx 失败 ⇒ tx 必 fatal");
        assert!(pair.is_fatal(), "任一方向失败 ⇒ pair fatal");
        assert_eq!(pair.tx.seal(b"must reject"), Err(CryptoError::Fatal));

        // ② tx 失败 ⇒ rx 同样 fatal，且对合法记录也必拒绝（Fatal 先于一切校验）。
        let mut pair = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM)
            .expect("derive")
            .into_client_record_layers();
        let too_big = vec![0u8; MAX_PLAINTEXT_LEN + 1];
        assert_eq!(pair.tx.seal(&too_big), Err(CryptoError::RecordTooLarge));
        assert!(pair.rx.is_fatal(), "共享熔断位：tx 失败 ⇒ rx 必 fatal");
        assert!(pair.is_fatal(), "发送方向失败 ⇒ pair fatal");
        assert_eq!(pair.rx.open(&valid_s2c), Err(CryptoError::Fatal));

        // ③ 写出失败/部分写入（`seal` 已成功、TxSeq 已推进、禁止重试）：
        // `mark_write_failed()` 拉闸共享熔断位，此后 tx/rx 一律 Fatal。
        let mut pair = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM)
            .expect("derive")
            .into_client_record_layers();
        pair.tx
            .seal(b"sealed but lost on the wire")
            .expect("seal ok");
        pair.mark_write_failed();
        assert!(pair.tx.is_fatal() && pair.rx.is_fatal());
        assert!(pair.is_fatal());
        assert_eq!(pair.tx.seal(b"no retry"), Err(CryptoError::Fatal));
        assert_eq!(pair.rx.open(&valid_s2c), Err(CryptoError::Fatal));
    }

    /// 生产构造路径装配的 pair **不得**带 fatal 位：`record_state()` 只在 ring
    /// 拒绝固定 32B 密钥（`key == None`，正常输入不可达）时才在构造期拉闸，
    /// [`RecordState::cached_key`] 随后也总在同一位上兜底。若有人把密钥装配
    /// 改坏（算法或长度写错 ⇒ 恒为 `None`），本测试立刻失败，而不是让所有
    /// 连接静默返回 `Internal`/`Fatal` 被误当成对端问题。
    #[test]
    fn constructed_pair_is_not_fatal_and_cached_key_path_works() {
        let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
        let c2s_key = keys.c2s_key;
        let c2s_iv = keys.c2s_iv;
        let mut pair = keys.into_client_record_layers();
        assert!(!pair.is_fatal(), "正常密钥不得在构造期拉闸");

        let record = pair.tx.seal(b"cached key path").expect("seal ok");
        let mut peer = rx_at_seq(&c2s_key, &c2s_iv, 0);
        assert_eq!(
            peer.open(&record).expect("peer open ok"),
            b"cached key path"
        );
        assert!(!pair.is_fatal(), "成功路径不得置位熔断位");
        assert!(!peer.is_fatal(), "对端成功解封不得置位熔断位");
    }

    /// 发送端错误优先序（有意的不对称，见 [`RecordLayer::seal`] 文档）：序号
    /// 已耗尽**且**明文超长时，`seal` 先报 [`CryptoError::RecordTooLarge`]，
    /// 而 [`RecordLayer::open`] 同条件下先报 `SequenceExhausted`。两者都 fatal。
    #[test]
    fn send_side_checks_length_before_exhaustion() {
        let keys = SessionKeys::derive(&SHARED, &CLIENT_RANDOM, &SERVER_RANDOM).expect("derive");
        let mut tx = tx_at_seq(&keys.c2s_key, &keys.c2s_iv, u64::MAX);
        let too_big = vec![0u8; MAX_PLAINTEXT_LEN + 1];
        assert_eq!(tx.seal(&too_big), Err(CryptoError::RecordTooLarge));
        assert!(tx.is_fatal(), "长度违规同样拉闸");
        // 拉闸之后一律 Fatal，长度/耗尽的先后已不再有意义。
        assert_eq!(tx.seal(b"short"), Err(CryptoError::Fatal));
    }
}

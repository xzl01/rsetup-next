//! rsetup-protocol：板端/中控设备协议字节契约（纯函数，无 IO，无全局状态）。
//!
//! 本切片覆盖帧边界（protocol_spec.md §4.1）、签名输入 raw wire bytes（§4.3）、
//! 握手帧序列状态机与准入分类纯模型（§4.1, §4.5, §4.6）、
//! 以及 ring Ed25519 签/验薄封装；业务 Protobuf codec 见 `codec` 模块（所有者
//! 2026-10-22 授权启用，见 `wire::approved_codec_witness` 的性质说明）；不含网络、设备 DB。
//! 其交付不构成任何握手步骤，更不构成握手完成。

pub mod admission;
pub mod codec;
pub mod crypto;
pub mod ed25519;
pub mod frame;
pub mod handshake;
pub mod sig;
pub mod wire;

pub use codec::BusinessCodec;
pub use crypto::{CryptoError, EphemeralKeyPair, RecordLayer, RecordLayerPair, SessionKeys};
pub use wire::{
    ApprovedCodecWitness, BUSINESS_SCHEMA_VERSION, BusinessMessage, BusinessPayload,
    BusinessPayloadKind, Capabilities, ClockSample, Codec, CodecError, DeviceStatus, DtoError,
    MAX_METRIC_KEY_BYTES, MAX_METRIC_UNIT_BYTES, MAX_METRICS, MAX_TICKET_TTL_MS, Metric, NoCodec,
    Query, RebootExecute, RebootPrepare, RebootTicket, TaskQuery, TaskRecord,
    approved_codec_witness, canonical_reboot_payload_hash, decode_business, encode_business,
    is_canonical_uuid_v4, validate_dto,
};

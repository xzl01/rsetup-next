//! gRPC 编帧与有界消息解码器。
//!
//! 内存边界：
//! - 单条未完成消息的内部缓冲严格 `≤ 5 + 512 KiB`：先按 5 字节头判定长度合法，再按该长度
//!   逐步补数据，绝不整体缓存对端 chunk。
//! - 批量输出真实有界：任何一次调用最多产出 [`MAX_PACKETS_PER_PUSH`] 个包；超出预算的输入
//!   保留在游标 / [`PushOutcome::rest`] 中由调用方继续喂入，**不会**因此污染连接。
//!
//! 生产路径（session）应使用增量接口 [`GrpcDecoder::next_packet`] /
//! [`GrpcDecoder::next_packets`]；[`GrpcDecoder::push`] 是带批量预算的便捷包装，
//! 其返回的 `rest` 必须被调用方重新喂回。

use crate::envelope::{EnvelopeError, MAX_ENVELOPE_SIZE, TunnelPacket};
use bytes::BytesMut;
use thiserror::Error;

/// gRPC 消息上限：1 MiB = 1,048,576 字节。
pub const MAX_GRPC_MESSAGE_SIZE: usize = 1024 * 1024;
/// gRPC 长度前缀头：1 字节压缩标志 + 4 字节 BE32 长度。
pub const GRPC_HEADER_SIZE: usize = 5;
/// 单次调用可产出的最大包数（批量输出预算）。
///
/// 该预算只限制**输出条数**，不限制输入：超出预算的字节保留在游标中，属正常粘包处理，
/// 不构成协议错误。
pub const MAX_PACKETS_PER_PUSH: usize = 64;

/// gRPC 编解码错误。
///
/// 长度前缀两道上限的判定顺序固定：先 `> 1MiB`（[`GrpcError::MessageTooLarge`]，对应 gRPC
/// 收发最大消息尺寸），再 `> 512KiB`（[`GrpcError::EnvelopeTooLarge`]，对应 §6.4 整包上限）；
/// 信封上限更严格，因此 512KiB ~ 1MiB 之间的前缀会得到 `EnvelopeTooLarge`。
#[derive(Debug, Error)]
pub enum GrpcError {
    #[error("envelope encoding failed: {0}")]
    EnvelopeEncode(#[from] EnvelopeError),

    #[error("envelope decoding failed: {0}")]
    EnvelopeDecode(EnvelopeError),

    #[error("gRPC compression is not supported (flag must be 0)")]
    CompressionNotSupported,

    #[error("gRPC message length prefix exceeds 1MiB: {0} bytes")]
    MessageTooLarge(usize),

    #[error("gRPC envelope length exceeds 512KiB limit: {0} bytes")]
    EnvelopeTooLarge(usize),

    #[error("gRPC stream finished with truncated message")]
    TruncatedMessage,

    #[error("decoder is poisoned due to a previous fatal error")]
    DecoderPoisoned,
}

/// 输入游标：包裹一次到达的对端字节，记录已消费位置。
///
/// 解码器只按需从游标取用字节（当前消息所需），因此超大 chunk 不会被整体复制。
#[derive(Debug, Clone)]
pub struct InputCursor<'a> {
    bytes: &'a [u8],
    consumed: usize,
}

impl<'a> InputCursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        InputCursor { bytes, consumed: 0 }
    }

    /// 已从游标消费的字节数。
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    /// 游标中尚未消费的字节数。
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.consumed
    }

    /// 尚未消费的输入（供调用方在预算耗尽后重新喂入）。
    pub fn unconsumed(&self) -> &'a [u8] {
        &self.bytes[self.consumed..]
    }

    /// 至多取用 `max` 字节并推进游标；返回的切片长度即实际消费量。
    fn take_at_most(&mut self, max: usize) -> &'a [u8] {
        let start = self.consumed;
        let end = start.saturating_add(max).min(self.bytes.len());
        self.consumed = end;
        &self.bytes[start..end]
    }
}

/// [`GrpcDecoder::push`] 的结果：已解码的包 + 未消费的输入。
#[derive(Debug)]
pub struct PushOutcome<'a> {
    packets: Vec<TunnelPacket>,
    consumed: usize,
    rest: &'a [u8],
}

impl<'a> PushOutcome<'a> {
    pub fn packets(&self) -> &[TunnelPacket] {
        &self.packets
    }

    pub fn into_packets(self) -> Vec<TunnelPacket> {
        self.packets
    }

    /// 本次调用从 chunk 中消费的字节数。
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    /// 预算耗尽后剩余的输入；非空时调用方**必须**把它作为后续输入重新喂入
    /// （拼在下一个 chunk 之前即可），否则会丢消息。
    pub fn rest(&self) -> &'a [u8] {
        self.rest
    }

    /// 是否因批量预算而提前停止（`rest` 非空）。
    pub fn budget_exhausted(&self) -> bool {
        !self.rest.is_empty()
    }
}

/// 编码单个 `TunnelPacket` 为带 5 字节头的 gRPC 消息：
/// - 1 字节压缩标志（固定为 0）
/// - 4 字节大端长度（BE32）
/// - 信封 payload
///
/// 信封编码自身带语义校验与 checked 大小计算，超大包不会产生等大的中间分配。
pub fn encode_message(packet: &TunnelPacket) -> Result<Vec<u8>, GrpcError> {
    let payload = packet.encode()?;
    if payload.len() > MAX_ENVELOPE_SIZE {
        return Err(GrpcError::EnvelopeTooLarge(payload.len()));
    }
    if payload.len() > MAX_GRPC_MESSAGE_SIZE {
        return Err(GrpcError::MessageTooLarge(payload.len()));
    }

    let mut out = Vec::with_capacity(GRPC_HEADER_SIZE + payload.len());
    out.push(0u8); // 压缩标志 = 0
    let len =
        u32::try_from(payload.len()).map_err(|_| GrpcError::MessageTooLarge(payload.len()))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload);

    Ok(out)
}

/// gRPC 有界消息解码器。
///
/// - 单包信封上限 ≤ 512 KiB；gRPC 长度前缀上限 ≤ 1 MiB
/// - 单条未完成消息缓冲 ≤ `5 + 512 KiB`，且按需从输入取用，绝不整体缓存 chunk
/// - 每次调用输出 ≤ [`MAX_PACKETS_PER_PUSH`] 个包
/// - 协议级致命错误后立即 poisoned 粘滞；合法的批量超限**不**致 fatal
#[derive(Debug)]
pub struct GrpcDecoder {
    buf: BytesMut,
    poisoned: bool,
}

impl Default for GrpcDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl GrpcDecoder {
    /// 创建新的 gRPC 解码器。
    pub fn new() -> Self {
        GrpcDecoder {
            buf: BytesMut::new(),
            poisoned: false,
        }
    }

    /// 当前内部未完成缓冲大小（含 5 字节头），上限 `5 + 512 KiB`。
    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    /// 是否已进入 fatal 粘滞状态。
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// 增量解码（生产推荐）：从游标中取出**一条**完整消息。
    ///
    /// 返回 `Ok(None)` 表示数据不足，需要更多输入（不是错误，也不污染解码器）。
    pub fn next_packet(
        &mut self,
        cursor: &mut InputCursor<'_>,
    ) -> Result<Option<TunnelPacket>, GrpcError> {
        if self.poisoned {
            return Err(GrpcError::DecoderPoisoned);
        }
        let res = self.fill_and_take(cursor);
        self.settle(res)
    }

    /// 增量批量解码：最多产出 `max_packets` 条（再受 [`MAX_PACKETS_PER_PUSH`] 上限夹紧）。
    ///
    /// 未消费的输入保留在 `cursor` 中，由调用方决定何时继续取用。
    pub fn next_packets(
        &mut self,
        cursor: &mut InputCursor<'_>,
        max_packets: usize,
    ) -> Result<Vec<TunnelPacket>, GrpcError> {
        if self.poisoned {
            return Err(GrpcError::DecoderPoisoned);
        }
        let budget = max_packets.min(MAX_PACKETS_PER_PUSH);
        let res = self.fill_batch(cursor, budget);
        self.settle(res)
    }

    /// 便捷包装（旧 API，带明确批量预算）：一次喂入一个 chunk。
    ///
    /// - 最多输出 [`MAX_PACKETS_PER_PUSH`] 个包；
    /// - 剩余输入通过 [`PushOutcome::rest`] 返回，调用方必须重新喂回（否则会丢消息）；
    /// - 预算超限属正常粘包，不污染解码器。
    pub fn push<'a>(&mut self, chunk: &'a [u8]) -> Result<PushOutcome<'a>, GrpcError> {
        let mut cursor = InputCursor::new(chunk);
        let packets = self.next_packets(&mut cursor, MAX_PACKETS_PER_PUSH)?;
        Ok(PushOutcome {
            consumed: cursor.consumed(),
            rest: cursor.unconsumed(),
            packets,
        })
    }

    /// 完成解码流。若存在未完成的消息分片，返回截断错误并进入 poisoned 状态。
    pub fn finish(&mut self) -> Result<(), GrpcError> {
        if self.poisoned {
            return Err(GrpcError::DecoderPoisoned);
        }

        if !self.buf.is_empty() {
            self.poison();
            self.buf.clear();
            return Err(GrpcError::TruncatedMessage);
        }

        Ok(())
    }

    fn settle<T>(&mut self, res: Result<T, GrpcError>) -> Result<T, GrpcError> {
        if res.is_err() {
            self.poisoned = true;
            self.buf.clear();
        }
        res
    }

    fn poison(&mut self) {
        self.poisoned = true;
    }

    fn fill_batch(
        &mut self,
        cursor: &mut InputCursor<'_>,
        budget: usize,
    ) -> Result<Vec<TunnelPacket>, GrpcError> {
        let mut packets = Vec::new();
        while packets.len() < budget {
            match self.fill_and_take(cursor)? {
                Some(packet) => packets.push(packet),
                None => break,
            }
        }
        Ok(packets)
    }

    /// 先补 5 字节头 → 校验 flag/长度 → 再按该长度增量补数据；一次至多产出一帧。
    fn fill_and_take(
        &mut self,
        cursor: &mut InputCursor<'_>,
    ) -> Result<Option<TunnelPacket>, GrpcError> {
        // 1) 头部：最多只需再缓存 5 字节即可判定本帧尺寸，避免先缓存整个 chunk
        if self.buf.len() < GRPC_HEADER_SIZE {
            let need = GRPC_HEADER_SIZE - self.buf.len();
            let take = cursor.take_at_most(need);
            self.buf.extend_from_slice(take);
            if self.buf.len() < GRPC_HEADER_SIZE {
                return Ok(None);
            }
        }

        // 2) 校验压缩标志与长度前缀（分配之前）
        if self.buf[0] != 0 {
            return Err(GrpcError::CompressionNotSupported);
        }
        let length_bytes: [u8; 4] = [self.buf[1], self.buf[2], self.buf[3], self.buf[4]];
        let msg_len = u32::from_be_bytes(length_bytes) as usize;
        if msg_len > MAX_GRPC_MESSAGE_SIZE {
            return Err(GrpcError::MessageTooLarge(msg_len));
        }
        if msg_len > MAX_ENVELOPE_SIZE {
            return Err(GrpcError::EnvelopeTooLarge(msg_len));
        }

        // 3) 按已确认长度增量填充：缓冲上限 = 5 + 512 KiB
        let total_frame_len = GRPC_HEADER_SIZE + msg_len;
        while self.buf.len() < total_frame_len {
            let need = total_frame_len - self.buf.len();
            let take = cursor.take_at_most(need);
            if take.is_empty() {
                return Ok(None);
            }
            self.buf.extend_from_slice(take);
        }

        // 4) 整帧就绪，切出并解码（解码自身含语义校验）
        let frame = self.buf.split_to(total_frame_len);
        let packet =
            TunnelPacket::decode(&frame[GRPC_HEADER_SIZE..]).map_err(GrpcError::EnvelopeDecode)?;

        Ok(Some(packet))
    }
}

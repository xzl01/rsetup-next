//! SNTP socket 层（Runtime Task 1b）：请求构造、异步可取消 DNS、真请求关联
//! （correlate）与单次查询。
//!
//! 关联（correlate）与解析（parse）分层：结构校验复用 `ntp::parse_sntp_response`
//! （固定校验顺序，尾部扩展字节忽略），本模块在其上追加源地址、Originate 回显、
//! Receive 时戳与本地单调 RTT 上限的 fail-closed 校验，再按 RFC 4330 四时戳公式
//! 产出 `NtpSample`。RTT 一律由本地 T1/T4 单调跨度度量，不信任服务端时戳差。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use tokio::net::UdpSocket;

use crate::time::ntp::NtpError;

/// SNTP 客户端请求/服务器响应的固定头部长度（RFC 4330 §4）。
pub const NTP_PACKET_LEN: usize = 48;
/// NTP 纪元（1900-01-01）到 UNIX 纪元（1970-01-01）的固定毫秒偏差。
pub const NTP_EPOCH_OFFSET_MS: i128 = 2_208_988_800 * 1000;

/// 一次通过关联校验的 SNTP 样本。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtpSample {
    /// RFC 四时戳偏移 `((T2 - T1) + (T3 - T4)) / 2`（毫秒）。
    pub offset_ms: i64,
    /// 本地单调跨度度量的往返耗时（毫秒）。
    pub rtt_ms: i64,
    /// 服务端发送时刻 T3 换算出的 UTC 参考时间。
    pub reference_utc: DateTime<Utc>,
    /// 不确定度：`max(rtt_ms / 2, 1)` 毫秒。
    pub uncertainty_ms: u64,
}

/// 异步可取消 DNS 解析 future（纳入整体预算，不使用同步 `ToSocketAddrs`）。
pub type SntpResolveFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<SocketAddr, NtpError>> + Send + 'a>>;

/// 异步 DNS 解析抽象：生产为 `tokio::net::lookup_host`，测试注入受控 fake。
pub trait SntpResolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str) -> SntpResolveFuture<'a>;
}

/// Owned “绑定 UDP socket” future（生产：`UdpSocket::bind` 异步；测试注入受控 bind），
/// 与 `SntpResolveFuture` 同形。
pub type SntpBindFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<UdpSocket, NtpError>> + Send>>;

/// 同一瞬间捕获的本地墙钟与单调快照。
#[derive(Clone, Copy, Debug)]
pub struct LocalStamp {
    pub wall_utc: DateTime<Utc>,
    pub mono: Instant,
}

/// 一次 SNTP 请求的关联凭据：实际解析/发送到的地址、64 位唯一发送时戳/nonce、
/// 请求发出的本地墙钟 T1 与单调起点。
#[derive(Clone, Debug)]
pub struct NtpCorrelationRequest {
    pub server_addr: SocketAddr,
    pub request_tx: u64,
    pub t1_wall_utc: DateTime<Utc>,
    pub t1_mono: Instant,
}

/// 生产 DNS 解析器：`tokio::net::lookup_host` 取第一个地址。
pub struct TokioSntpResolver;

impl SntpResolver for TokioSntpResolver {
    fn resolve<'a>(&'a self, host: &'a str) -> SntpResolveFuture<'a> {
        Box::pin(async move {
            let mut addresses = tokio::net::lookup_host(host)
                .await
                .map_err(|_| NtpError::Io)?;
            addresses.next().ok_or(NtpError::Io)
        })
    }
}

/// 构造 48 字节 SNTP 客户端请求：LI=0、VN=4、Mode=3（client），
/// Transmit Timestamp（bytes 40..48）写入 64 位唯一 nonce，供服务端 Originate 回显关联。
pub fn write_sntp_request(buffer: &mut [u8; NTP_PACKET_LEN], nonce: u64) {
    buffer.fill(0);
    buffer[0] = (4 << 3) | 3;
    buffer[40..48].copy_from_slice(&nonce.to_be_bytes());
}

/// 单次 SNTP 查询：`timeout` 一次性包住 DNS、send、recv 整个 future，绝不每个阶段重置预算。
/// DNS/socket/超时错误一律映射 `NtpError::Io`。
pub async fn query_sntp(
    socket: &UdpSocket,
    resolver: &dyn SntpResolver,
    server: &str,
    request_nonce: u64,
    timeout: Duration,
    max_rtt_ms: i64,
) -> Result<NtpSample, NtpError> {
    tokio::time::timeout(timeout, async {
        let target_addr = resolver.resolve(server).await?;
        let mut packet = [0u8; NTP_PACKET_LEN];
        write_sntp_request(&mut packet, request_nonce);
        let request = NtpCorrelationRequest {
            server_addr: target_addr,
            request_tx: request_nonce,
            t1_wall_utc: Utc::now(),
            t1_mono: Instant::now(),
        };
        let sent = socket
            .send_to(&packet, target_addr)
            .await
            .map_err(|_| NtpError::Io)?;
        if sent != packet.len() {
            return Err(NtpError::Io);
        }
        // 读取缓冲大于 48B 头部（容纳 Key Identifier/Digest 尾部扩展），但关联只按
        // parse 已校验的前 48 字节取值，尾部未经校验的字节不参与任何换算。
        let mut response = [0u8; 512];
        let (length, source) = socket
            .recv_from(&mut response)
            .await
            .map_err(|_| NtpError::Io)?;
        let t4 = LocalStamp {
            wall_utc: Utc::now(),
            mono: Instant::now(),
        };
        correlate_sntp_response(&response[..length], &request, source, t4, max_rtt_ms)
    })
    .await
    .map_err(|_| NtpError::Io)?
}

/// 真请求关联：按严格优先级 fail-closed 校验后产出 `NtpSample`。
///
/// 校验顺序：48B 结构 → 响应源地址严格相等 → Originate 全量回显 nonce → T2 非零 →
/// 本地单调 RTT 上限 → RFC 四时戳公式（checked i128，溢出 `TimestampOverflow`）。
pub fn correlate_sntp_response(
    packet: &[u8],
    request: &NtpCorrelationRequest,
    response_source: SocketAddr,
    t4: LocalStamp,
    max_rtt_ms: i64,
) -> Result<NtpSample, NtpError> {
    // 1) 48B 纯结构（导入自 parser 分支，绝不升级为 NtpValid）
    crate::time::ntp::parse_sntp_response(packet)?;
    // 2) 响应源必须是请求实际解析/发送到的 SocketAddr（禁止与 host 字符串盲目比对）
    if response_source != request.server_addr {
        return Err(NtpError::SourceMismatch);
    }
    // 3) 请求 T1 的 64 位唯一发送时戳/nonce 仅由响应 Originate 全量回显关联
    let originate = u64::from_be_bytes(packet[24..32].try_into().unwrap());
    if originate != request.request_tx {
        return Err(NtpError::OriginateMismatch);
    }
    // 4) 响应 T2/T3 是服务端时间，不得当作回显；T2 必须非零
    let t2_raw = u64::from_be_bytes(packet[32..40].try_into().unwrap());
    let t3_raw = u64::from_be_bytes(packet[40..48].try_into().unwrap());
    if t2_raw == 0 {
        return Err(NtpError::ZeroReceiveTimestamp);
    }
    // 5) RTT 上限由本地 T1/T4 单调跨度（monotonic span）严格度量
    let rtt_duration = t4
        .mono
        .checked_duration_since(request.t1_mono)
        .ok_or(NtpError::TimestampOverflow)?;
    let rtt_ms =
        i64::try_from(rtt_duration.as_millis()).map_err(|_| NtpError::TimestampOverflow)?;
    if max_rtt_ms < 0 || rtt_ms > max_rtt_ms {
        return Err(NtpError::RttTooLarge);
    }
    // 6) RFC 四时戳公式：((T2 - T1) + (T3 - T4)) / 2，UTC/NTP epoch checked i128 精确换算
    let t1_ms = to_ntp_epoch_ms(request.t1_wall_utc);
    let t2_ms = ntp_u64_to_ms(t2_raw);
    let t3_ms = ntp_u64_to_ms(t3_raw);
    let t4_ms = to_ntp_epoch_ms(t4.wall_utc);

    let offset_ms_i128 = ((t2_ms
        .checked_sub(t1_ms)
        .ok_or(NtpError::TimestampOverflow)?)
    .checked_add(
        t3_ms
            .checked_sub(t4_ms)
            .ok_or(NtpError::TimestampOverflow)?,
    )
    .ok_or(NtpError::TimestampOverflow)?)
        / 2;
    let offset_ms = i64::try_from(offset_ms_i128).map_err(|_| NtpError::TimestampOverflow)?;

    let ref_unix_ms = t3_ms
        .checked_sub(NTP_EPOCH_OFFSET_MS)
        .ok_or(NtpError::TimestampOverflow)?;
    let ref_unix_ms_i64 = i64::try_from(ref_unix_ms).map_err(|_| NtpError::TimestampOverflow)?;
    let reference_utc = Utc
        .timestamp_millis_opt(ref_unix_ms_i64)
        .single()
        .ok_or(NtpError::TimestampOverflow)?;

    Ok(NtpSample {
        offset_ms,
        rtt_ms,
        reference_utc,
        uncertainty_ms: u64::try_from((rtt_ms / 2).max(1))
            .map_err(|_| NtpError::TimestampOverflow)?,
    })
}

/// 本地墙钟（UNIX 毫秒）换算到 NTP 纪元毫秒。
fn to_ntp_epoch_ms(wall: DateTime<Utc>) -> i128 {
    (wall.timestamp_millis() as i128) + NTP_EPOCH_OFFSET_MS
}

/// NTP 64 位定点时戳（高 32 位秒 + 低 32 位小数）换算到 NTP 纪元毫秒（i128 精确）。
fn ntp_u64_to_ms(raw: u64) -> i128 {
    let sec = (raw >> 32) as i128;
    let frac = (raw & 0xffff_ffff) as i128;
    (sec * 1000) + ((frac * 1000) / (1i128 << 32))
}

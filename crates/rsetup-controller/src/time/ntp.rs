//! SNTP（RFC 4330）客户端侧**纯解析器**。
//!
//! 本模块只做「一段字节 → 结构化响应」的同步纯函数解析，不含 async、socket、
//! 时钟换算与关联（correlate）逻辑：源地址比对、Originate 回显校验、RTT 度量与
//! RFC 四时戳偏移公式属于后续 `gate`/`socket` 层（Runtime Task 1b）。
//!
//! 自定决定（重建时记录，供 T1b 与评审核对）：
//! 1. **尾部字节忽略**：SNTP 包最小 48 字节，`Key Identifier`/`Digest` 等尾部扩展
//!    （自动密钥，RFC 4330 §4）不在 V1 校验范围内。因此长度 ≥ 48 即通过长度校验，
//!    且**只解析前 48 字节**，多余字节一律忽略，不做「必须恰好 48 字节」的拒绝。
//! 2. **固定校验顺序**（测试断言优先级，短路返回首个错误）：
//!    长度 → Leap Indicator → Version Number → Mode → Stratum → Transmit Timestamp。
//!    顺序依据 RFC 4330 §4 的字段布局由外而内：先保证可读，再校验头部标志字节
//!    （byte 0 的 LI/VN/Mode），再校验 byte 1 的 stratum，最后校验时戳。
//! 3. **Stratum 合法范围 1..=15**：0 表示 kiss-o'-death/未同步（交由 LI 或上层策略
//!    处理，本解析器直接判为 `InvalidStratum`），16..=255 保留/未用。
//! 4. **LI 仅拒绝 3**（alarm condition，服务器明确宣告时钟不可信）；LI=1/2
//!    （延迟/提前闰秒）是正常服务状态，不拒绝。
//! 5. **不校验 Originate/Receive 时戳**：零值 Originate 只说明服务端未回显，
//!    关联判定属于 T1b `correlate_sntp_response`，解析器不得提前拒绝。

/// SNTP 解析与（T1b）关联阶段可能出现的错误。
///
/// 前 6 个变体由本模块的 `parse_sntp_response` 产生；后 6 个变体供 T1b 的
/// socket/关联层使用，本任务只定义其判别式与 `Display` 文本。
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NtpError {
    #[error("packet too short")]
    PacketTooShort,
    #[error("server clock unsynchronized")]
    Unsynchronized,
    #[error("invalid stratum")]
    InvalidStratum,
    #[error("invalid version")]
    InvalidVersion,
    #[error("invalid mode")]
    InvalidMode,
    #[error("zero transmit timestamp")]
    ZeroTransmitTimestamp,
    #[error("io error")]
    Io,
    #[error("response source mismatch")]
    SourceMismatch,
    #[error("originate timestamp mismatch")]
    OriginateMismatch,
    #[error("zero receive timestamp")]
    ZeroReceiveTimestamp,
    #[error("round trip time exceeds maximum allowable threshold")]
    RttTooLarge,
    #[error("timestamp arithmetic overflow")]
    TimestampOverflow,
}

/// 一次 SNTP 服务器响应的结构化视图（RFC 4330 §4 头部字段）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SntpResponse {
    /// Leap Indicator：byte 0 的 bit 7-6，合法值 0..=3（3 已被拒绝）。
    pub leap_indicator: u8,
    /// Version Number：byte 0 的 bit 5-3，接受 3 或 4。
    pub version: u8,
    /// Mode：byte 0 的 bit 2-0，本解析器只接受 4（server）。
    pub mode: u8,
    /// Stratum：byte 1，合法值 1..=15。
    pub stratum: u8,
    /// Originate Timestamp：bytes 24..32，大端 64 位（服务端回显的客户端 Transmit）。
    pub originate_timestamp: u64,
    /// Receive Timestamp：bytes 32..40，大端 64 位（服务端收到请求的时刻 T2）。
    pub receive_timestamp: u64,
    /// Transmit Timestamp：bytes 40..48，大端 64 位（服务端发出响应的时刻 T3）。
    pub transmit_timestamp: u64,
}

/// 解析一段 SNTP 服务器响应字节。
///
/// 长度 ≥ 48 时只读取前 48 字节，尾部扩展字节忽略；校验顺序与拒绝条件见模块
/// 文档注释。不校验源地址、Originate 回显、Receive 时戳与 RTT。
pub fn parse_sntp_response(packet: &[u8]) -> Result<SntpResponse, NtpError> {
    // 1) 长度：只保证前 48 字节可读，尾部扩展字节忽略。
    if packet.len() < 48 {
        return Err(NtpError::PacketTooShort);
    }
    let flags = packet[0];
    // 2) Leap Indicator：仅 3（时钟不可信）拒绝。
    let leap_indicator = flags >> 6;
    if leap_indicator == 3 {
        return Err(NtpError::Unsynchronized);
    }
    // 3) Version Number：接受 SNTP v3/v4。
    let version = (flags >> 3) & 0x07;
    if version != 3 && version != 4 {
        return Err(NtpError::InvalidVersion);
    }
    // 4) Mode：只接受服务器响应。
    let mode = flags & 0x07;
    if mode != 4 {
        return Err(NtpError::InvalidMode);
    }
    // 5) Stratum：1..=15。
    let stratum = packet[1];
    if stratum == 0 || stratum > 15 {
        return Err(NtpError::InvalidStratum);
    }
    // 6) Transmit Timestamp：非零是服务端已产生响应时戳的最低证据。
    let transmit_timestamp = read_u64(packet, 40);
    if transmit_timestamp == 0 {
        return Err(NtpError::ZeroTransmitTimestamp);
    }
    // 7) 组装；Originate/Receive 不校验（关联属 T1b）。
    Ok(SntpResponse {
        leap_indicator,
        version,
        mode,
        stratum,
        originate_timestamp: read_u64(packet, 24),
        receive_timestamp: read_u64(packet, 32),
        transmit_timestamp,
    })
}

/// 读取包内偏移 `offset` 处的大端 64 位时戳；调用方已保证 `offset + 8 <= len`。
fn read_u64(packet: &[u8], offset: usize) -> u64 {
    let bytes: [u8; 8] = packet[offset..offset + 8].try_into().expect(
        "parse_sntp_response 已前置 packet.len() >= 48 校验，offset 只取 24/32/40，切片必为 8 字节",
    );
    u64::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGINATE: u64 = 0x0102_0304_0506_0708;
    const RECEIVE: u64 = 0xE0F1_E2F3_E4F5_E6F7;
    const TRANSMIT: u64 = 0xE8B4_A690_0000_0FA1;

    /// 按 RFC 4330 字段布局组装 48 字节响应，避免测试散落魔法数。
    fn packet(
        li: u8,
        vn: u8,
        mode: u8,
        stratum: u8,
        originate: u64,
        receive: u64,
        transmit: u64,
    ) -> [u8; 48] {
        let mut buf = [0u8; 48];
        buf[0] = (li << 6) | (vn << 3) | mode;
        buf[1] = stratum;
        buf[24..32].copy_from_slice(&originate.to_be_bytes());
        buf[32..40].copy_from_slice(&receive.to_be_bytes());
        buf[40..48].copy_from_slice(&transmit.to_be_bytes());
        buf
    }

    /// 以固定三时戳组装基准包；LI/VN/Mode/stratum 由参数决定，便于优先级用例复用。
    fn valid_packet(li: u8, vn: u8, mode: u8, stratum: u8) -> [u8; 48] {
        packet(li, vn, mode, stratum, ORIGINATE, RECEIVE, TRANSMIT)
    }

    #[test]
    fn parses_valid_v4_response_fields() {
        let buf = valid_packet(0, 4, 4, 2);
        let resp = parse_sntp_response(&buf).expect("合法响应应解析成功");
        assert_eq!(resp.leap_indicator, 0);
        assert_eq!(resp.version, 4);
        assert_eq!(resp.mode, 4);
        assert_eq!(resp.stratum, 2);
        assert_eq!(resp.originate_timestamp, ORIGINATE);
        assert_eq!(resp.receive_timestamp, RECEIVE);
        assert_eq!(resp.transmit_timestamp, TRANSMIT);
    }

    #[test]
    fn parses_timestamps_as_big_endian() {
        // 逐字节递增，确保不是小端或错位读取。
        let mut buf = valid_packet(0, 4, 4, 1);
        for (i, slot) in [24usize..32, 32..40, 40..48].iter().enumerate() {
            for (k, idx) in slot.clone().enumerate() {
                buf[idx] = (i * 16 + k) as u8 + 1;
            }
        }
        let resp = parse_sntp_response(&buf).expect("应解析成功");
        assert_eq!(
            resp.originate_timestamp,
            u64::from_be_bytes(buf[24..32].try_into().expect("48B 包必有 8 字节切片"))
        );
        assert_eq!(
            resp.receive_timestamp,
            u64::from_be_bytes(buf[32..40].try_into().expect("48B 包必有 8 字节切片"))
        );
        assert_eq!(
            resp.transmit_timestamp,
            u64::from_be_bytes(buf[40..48].try_into().expect("48B 包必有 8 字节切片"))
        );
        assert_ne!(resp.transmit_timestamp, 0);
    }

    #[test]
    fn accepts_version_3_and_4() {
        for vn in [3u8, 4] {
            let buf = valid_packet(0, vn, 4, 2);
            let resp =
                parse_sntp_response(&buf).unwrap_or_else(|e| panic!("VN={vn} 应接受，实际 {e:?}"));
            assert_eq!(resp.version, vn);
        }
    }

    #[test]
    fn rejects_version_2_and_5() {
        for vn in [2u8, 5] {
            let buf = valid_packet(0, vn, 4, 2);
            assert_eq!(
                parse_sntp_response(&buf),
                Err(NtpError::InvalidVersion),
                "VN={vn} 应被拒绝"
            );
        }
    }

    #[test]
    fn rejects_non_server_modes() {
        for mode in [1u8, 2, 3, 5, 6, 7] {
            let buf = valid_packet(0, 4, mode, 2);
            assert_eq!(
                parse_sntp_response(&buf),
                Err(NtpError::InvalidMode),
                "Mode={mode} 应被拒绝"
            );
        }
    }

    #[test]
    fn accepts_leap_indicator_0_to_2() {
        for li in [0u8, 1, 2] {
            let buf = valid_packet(li, 4, 4, 2);
            let resp =
                parse_sntp_response(&buf).unwrap_or_else(|e| panic!("LI={li} 应接受，实际 {e:?}"));
            assert_eq!(resp.leap_indicator, li);
        }
    }

    #[test]
    fn rejects_leap_indicator_3() {
        let buf = valid_packet(3, 4, 4, 2);
        assert_eq!(parse_sntp_response(&buf), Err(NtpError::Unsynchronized));
    }

    #[test]
    fn accepts_stratum_1_and_15() {
        for stratum in [1u8, 15] {
            let buf = valid_packet(0, 4, 4, stratum);
            let resp = parse_sntp_response(&buf).unwrap_or_else(|e| {
                panic!("stratum={stratum} 应接受，实际 {e:?}");
            });
            assert_eq!(resp.stratum, stratum);
        }
    }

    #[test]
    fn rejects_stratum_0_and_above_15() {
        for stratum in [0u8, 16, 255] {
            let buf = valid_packet(0, 4, 4, stratum);
            assert_eq!(
                parse_sntp_response(&buf),
                Err(NtpError::InvalidStratum),
                "stratum={stratum} 应被拒绝"
            );
        }
    }

    #[test]
    fn rejects_zero_transmit_timestamp() {
        let buf = packet(0, 4, 4, 2, ORIGINATE, RECEIVE, 0);
        assert_eq!(
            parse_sntp_response(&buf),
            Err(NtpError::ZeroTransmitTimestamp)
        );
    }

    #[test]
    fn accepts_zero_originate_and_receive_when_transmit_nonzero() {
        // 解析器不校验 Originate/Receive，零值仍应合法（关联属 T1b）。
        let buf = packet(0, 4, 4, 2, 0, 0, TRANSMIT);
        let resp = parse_sntp_response(&buf).expect("Originate/Receive 为零不应影响解析");
        assert_eq!(resp.originate_timestamp, 0);
        assert_eq!(resp.receive_timestamp, 0);
        assert_eq!(resp.transmit_timestamp, TRANSMIT);
    }

    #[test]
    fn rejects_packets_shorter_than_48_bytes() {
        let full = valid_packet(0, 4, 4, 2);
        for len in [0usize, 1, 47] {
            assert_eq!(
                parse_sntp_response(&full[..len]),
                Err(NtpError::PacketTooShort),
                "长度 {len} 应被拒绝"
            );
        }
    }

    #[test]
    fn accepts_48_and_longer_packets_ignoring_trailing_bytes() {
        // 自定决定：长度 ≥48 只解析前 48 字节，尾部扩展字节忽略。
        let full = valid_packet(0, 4, 4, 2);
        let mut with_trailer = full.to_vec();
        with_trailer.push(0xAB);
        let expected = SntpResponse {
            leap_indicator: 0,
            version: 4,
            mode: 4,
            stratum: 2,
            originate_timestamp: ORIGINATE,
            receive_timestamp: RECEIVE,
            transmit_timestamp: TRANSMIT,
        };
        assert_eq!(parse_sntp_response(&full).ok(), Some(expected));
        assert_eq!(parse_sntp_response(&with_trailer).ok(), Some(expected));
    }

    #[test]
    fn order_length_before_leap_indicator() {
        let mut short = valid_packet(3, 4, 4, 2).to_vec();
        short.truncate(47);
        assert_eq!(parse_sntp_response(&short), Err(NtpError::PacketTooShort));
    }

    #[test]
    fn order_leap_indicator_before_version() {
        let buf = valid_packet(3, 2, 4, 2);
        assert_eq!(parse_sntp_response(&buf), Err(NtpError::Unsynchronized));
    }

    #[test]
    fn order_version_before_mode() {
        let buf = valid_packet(0, 2, 1, 2);
        assert_eq!(parse_sntp_response(&buf), Err(NtpError::InvalidVersion));
    }

    #[test]
    fn order_mode_before_stratum() {
        let buf = valid_packet(0, 4, 1, 0);
        assert_eq!(parse_sntp_response(&buf), Err(NtpError::InvalidMode));
    }

    #[test]
    fn order_stratum_before_transmit_timestamp() {
        let buf = packet(0, 4, 4, 0, ORIGINATE, RECEIVE, 0);
        assert_eq!(parse_sntp_response(&buf), Err(NtpError::InvalidStratum));
    }

    #[test]
    fn error_display_text_matches_variants() {
        let cases: [(NtpError, &str); 12] = [
            (NtpError::PacketTooShort, "packet too short"),
            (NtpError::Unsynchronized, "server clock unsynchronized"),
            (NtpError::InvalidStratum, "invalid stratum"),
            (NtpError::InvalidVersion, "invalid version"),
            (NtpError::InvalidMode, "invalid mode"),
            (NtpError::ZeroTransmitTimestamp, "zero transmit timestamp"),
            (NtpError::Io, "io error"),
            (NtpError::SourceMismatch, "response source mismatch"),
            (NtpError::OriginateMismatch, "originate timestamp mismatch"),
            (NtpError::ZeroReceiveTimestamp, "zero receive timestamp"),
            (
                NtpError::RttTooLarge,
                "round trip time exceeds maximum allowable threshold",
            ),
            (NtpError::TimestampOverflow, "timestamp arithmetic overflow"),
        ];
        for (err, text) in cases {
            assert_eq!(err.to_string(), text, "{err:?} 的 Display 文本不符");
        }
    }
}

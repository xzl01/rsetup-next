//! 板端时钟采样（ClockSample）的四时戳偏差估算与质量判定（05 §3 TIME-03）。
//!
//! 本模块只放**纯函数**：不读 socket、不读墙钟、不写数据库，因此所有分支都能在离线
//! 单测里逐条枚举。计算式与拒绝守则来自 05 §3「时间证据」与 §4「板端时间校验」：
//!
//! - `rtt = (T4 - T1) - (T3 - T2)`
//! - `board_minus_controller = ((T2 - T1) + (T3 - T4)) / 2`
//!
//! 两端时钟代际相互独立：中控只比对本端 T1 与 T4 的 `clock_epoch` 是否一致，板端只在
//! 采样期间自报 `observed` / `clock_unstable`；**绝不跨机比较** `controller_epoch ==
//! board_epoch`，也不为凑比较而让板端复制中控代际。
//!
//! 估算结果只宣称「板端相对中控」的偏差，不宣称板端相对标准 UTC 的误差（05 §4）。

use uuid::Uuid;

/// 05 §3/§4 建议 RTT 上限默认值（2s，待审建议值，非定稿协议常量）。
pub const DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED: i64 = 2000; // 05 §2 待审建议默认，非定稿协议常量

/// 无 RTC / 1970 异常时间戳校验的默认门限（2000-01-01T00:00:00Z 的 Unix 毫秒）。
///
/// 无 RTC 的板卡在 NTP 生效前墙钟常停在 1970 附近，此时算出的偏差虽然算术上合法，
/// 但没有参考意义。按 05 §4「大但稳定偏差应报告，不直接当无效样本」，本门限只**降级**
/// 质量（附 `NoRtcSuspected` 原因），不做拒绝。
pub const DEFAULT_NO_RTC_WALL_MS_CEILING_MS: i64 = 946_684_800_000;

/// 同代际平滑时前序偏移的默认权重（百分比，待审建议值）。
pub const DEFAULT_SMOOTHING_PREVIOUS_WEIGHT_PERCENT: u8 = 70;

/// 板端在 `ClockSample.quality` 中申报的时钟质量（03 载荷，05 §4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardReportedQuality {
    /// 板端只是观察到本机墙钟，采样期间未察觉跳钟。
    Observed,
    /// 板端在采样期间察觉跳钟/代际变化，本次四个时戳不可比较。
    ClockUnstable,
    /// 未能在 03 已审阅清单中识别的质量字符串：失败关闭，不按 `observed` 接受。
    Unknown,
}

impl BoardReportedQuality {
    /// 解析线格式质量字符串。除已审阅的 `observed` / `clock_unstable` 外一律 `Unknown`，
    /// 大小写与空白都由上游归一化负责，此处不做宽松匹配。
    #[must_use]
    pub fn from_wire(value: &str) -> Self {
        match value {
            "observed" => Self::Observed,
            "clock_unstable" => Self::ClockUnstable,
            _ => Self::Unknown,
        }
    }
}

/// 一次四时戳采样的板端身份锚点：`boot_id` 是启动标识，`clock_epoch` 是板端时钟代际。
///
/// 二者都不是时钟值，只用于「是否同一代际」的等值比较（05 §3 R-02 / §4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardClockIdentity<'a> {
    pub boot_id: &'a str,
    pub clock_epoch: &'a str,
}

/// 一次完整采样的四时戳加两端代际证据（全部为原始输入，不含任何推算值）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardClockSample<'a> {
    /// T1：中控发出 clock.get 时的本地墙钟（毫秒）。
    pub t1_controller_wall_ms: i64,
    /// T2：板端收到请求时的墙钟（毫秒）。
    pub t2_board_receive_wall_ms: i64,
    /// T3：板端发出响应时的墙钟（毫秒）。
    pub t3_board_send_wall_ms: i64,
    /// T4：中控收到响应时的本地墙钟（毫秒）。
    pub t4_controller_wall_ms: i64,
    /// T1 时刻的中控 `clock_epoch`。
    pub t1_controller_clock_epoch: Uuid,
    /// T4 时刻的中控 `clock_epoch`：与 T1 不同即本端采样期间跳钟。
    pub t4_controller_clock_epoch: Uuid,
    /// 板端 `ClockSample.boot_id`。
    pub board_boot_id: &'a str,
    /// 板端 `ClockSample.clock_epoch`（进程级，只有板端自己能判定）。
    pub board_clock_epoch: &'a str,
    /// 板端 `ClockSample.quality`。
    pub board_quality: BoardReportedQuality,
}

impl<'a> BoardClockSample<'a> {
    /// 板端身份锚点（boot_id + clock_epoch）。
    #[must_use]
    pub fn identity(&self) -> BoardClockIdentity<'a> {
        BoardClockIdentity {
            boot_id: self.board_boot_id,
            clock_epoch: self.board_clock_epoch,
        }
    }

    /// 板端自报质量是否为 `observed`。
    #[must_use]
    pub fn board_reports_stable(&self) -> bool {
        self.board_quality == BoardReportedQuality::Observed
    }
}

/// 估算与质量判定的可调参数。默认值全部是 05 中标注「建议」的待审值，由调用方覆盖。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardClockPolicy {
    /// RTT 超过该毫秒数即标记为降级/不可用（不判非法）。
    pub max_rtt_ms: i64,
    /// 板端墙钟低于该 Unix 毫秒即视为无 RTC / 1970 异常，参与降级但不拒绝。
    pub no_rtc_wall_ms_ceiling: i64,
}

impl Default for BoardClockPolicy {
    fn default() -> Self {
        Self {
            max_rtt_ms: DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED,
            no_rtc_wall_ms_ceiling: DEFAULT_NO_RTC_WALL_MS_CEILING_MS,
        }
    }
}

impl BoardClockPolicy {
    /// 以建议默认值构造，只覆盖 RTT 上限。
    #[must_use]
    pub fn with_max_rtt_ms(max_rtt_ms: i64) -> Self {
        Self {
            max_rtt_ms,
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockEstimate {
    pub board_offset_ms: i64,
    pub rtt_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ClockEstimateError {
    #[error("negative RTT observed")]
    NegativeRtt,
    #[error("RTT exceeds maximum threshold")]
    RttTooLarge,
    #[error("controller clock epoch mismatch between sample start and end")]
    ControllerEpochMismatch,
    #[error("board reported clock unstable")]
    BoardClockUnstable,
    #[error("boot ID mismatch")]
    BootMismatch,
    #[error("board clock epoch mismatch between samples")]
    BoardEpochMismatch,
    #[error("controller wall clock rewound during sampling (T4 < T1)")]
    ControllerClockRewound,
    #[error("board wall clock rewound during sampling (T3 < T2)")]
    BoardClockRewound,
    #[error("board reported an unrecognised clock quality")]
    UnverifiedQuality,
    #[error("arithmetic overflow in clock estimation")]
    ArithmeticOverflow,
}

/// 质量等级：三态语义不可合并，「未知」不等于 0，也「拒绝」不等于「降级」（05 §3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardClockQuality {
    /// 可直接用于偏差估计与平滑。
    Ok,
    /// 算术上合法但不可用于偏差估计，或存在需运维知悉的可疑特征：仍要上报。
    Degraded,
    /// 非法或跨代际样本：显式拒绝，不进入偏差估计与平滑。
    Rejected,
}

/// 质量判定的具体原因：拒绝原因与降级原因分开列举，便于审计定位。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoardClockReason {
    ControllerEpochMismatch,
    BoardClockUnstable,
    UnverifiedQuality,
    BootMismatch,
    BoardEpochMismatch,
    ControllerClockRewound,
    BoardClockRewound,
    NegativeRtt,
    ArithmeticOverflow,
    /// RTT 超过策略上限：降级/不可靠，本样本偏移不用于估计。
    RttTooLarge,
    /// 板端墙钟早于策略门限：疑似无 RTC 或 1970 异常时间戳。
    NoRtcSuspected,
}

impl BoardClockReason {
    /// 该原因是否属于「显式拒绝」而非「降级」。
    #[must_use]
    pub fn is_rejection(self) -> bool {
        !matches!(self, Self::RttTooLarge | Self::NoRtcSuspected)
    }
}

impl From<ClockEstimateError> for BoardClockReason {
    fn from(value: ClockEstimateError) -> Self {
        match value {
            ClockEstimateError::NegativeRtt => Self::NegativeRtt,
            ClockEstimateError::RttTooLarge => Self::RttTooLarge,
            ClockEstimateError::ControllerEpochMismatch => Self::ControllerEpochMismatch,
            ClockEstimateError::BoardClockUnstable => Self::BoardClockUnstable,
            ClockEstimateError::BootMismatch => Self::BootMismatch,
            ClockEstimateError::BoardEpochMismatch => Self::BoardEpochMismatch,
            ClockEstimateError::ControllerClockRewound => Self::ControllerClockRewound,
            ClockEstimateError::BoardClockRewound => Self::BoardClockRewound,
            ClockEstimateError::UnverifiedQuality => Self::UnverifiedQuality,
            ClockEstimateError::ArithmeticOverflow => Self::ArithmeticOverflow,
        }
    }
}

/// 质量投影结果：等级 + 可用的偏差估算 + 全部原因（去重、按检查顺序）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardClockVerdict {
    pub quality: BoardClockQuality,
    /// 仅当本样本可用于偏差估计时为 `Some`；`RttTooLarge` 降级时同样为 `None`，
    /// 因为 05 §4 规定过大延迟不可用于偏差估计。
    pub estimate: Option<ClockEstimate>,
    pub reasons: Vec<BoardClockReason>,
}

impl BoardClockVerdict {
    /// 是否被显式拒绝。
    #[must_use]
    pub fn is_rejected(&self) -> bool {
        self.quality == BoardClockQuality::Rejected
    }

    /// 是否可参与与前序样本的平滑（仅 `Ok` 且确实算出偏移时）。
    #[must_use]
    pub fn is_usable_for_smoothing(&self) -> bool {
        self.quality == BoardClockQuality::Ok && self.estimate.is_some()
    }

    /// 是否含有某一原因。
    #[must_use]
    pub fn contains(&self, reason: BoardClockReason) -> bool {
        self.reasons.contains(&reason)
    }
}

/// 纯算法逻辑：两主机独立 epoch，中控仅比对本端 T1 与 T4 epoch 一致性
#[allow(clippy::too_many_arguments)]
pub fn estimate_board_offset(
    t1_wall_ms: i64,
    t2_wall_ms: i64,
    t3_wall_ms: i64,
    t4_wall_ms: i64,
    t1_controller_epoch: Uuid,
    t4_controller_epoch: Uuid,
    board_clock_unstable: bool,
    board_boot: &str,
    expected_boot: &str,
    max_rtt_ms: i64,
) -> Result<ClockEstimate, ClockEstimateError> {
    if t1_controller_epoch != t4_controller_epoch {
        return Err(ClockEstimateError::ControllerEpochMismatch);
    }
    if board_clock_unstable {
        return Err(ClockEstimateError::BoardClockUnstable);
    }
    if board_boot != expected_boot {
        return Err(ClockEstimateError::BootMismatch);
    }

    // 单调性先于减法：任一端墙钟在本次采样期间倒流，四个时戳就不满足算法前提，
    // 必须在算术之前判为非法样本（05 §4 负时延非法样本显式拒绝）。
    if t4_wall_ms < t1_wall_ms {
        return Err(ClockEstimateError::ControllerClockRewound);
    }
    if t3_wall_ms < t2_wall_ms {
        return Err(ClockEstimateError::BoardClockRewound);
    }

    let total_elapsed = t4_wall_ms
        .checked_sub(t1_wall_ms)
        .ok_or(ClockEstimateError::ArithmeticOverflow)?;
    let board_processing = t3_wall_ms
        .checked_sub(t2_wall_ms)
        .ok_or(ClockEstimateError::ArithmeticOverflow)?;
    let rtt = total_elapsed
        .checked_sub(board_processing)
        .ok_or(ClockEstimateError::ArithmeticOverflow)?;

    if rtt < 0 {
        return Err(ClockEstimateError::NegativeRtt);
    }
    if rtt > max_rtt_ms {
        return Err(ClockEstimateError::RttTooLarge);
    }

    let d1 = t2_wall_ms
        .checked_sub(t1_wall_ms)
        .ok_or(ClockEstimateError::ArithmeticOverflow)? as i128;
    let d2 = t3_wall_ms
        .checked_sub(t4_wall_ms)
        .ok_or(ClockEstimateError::ArithmeticOverflow)? as i128;
    let offset = (d1 + d2) / 2;
    let board_offset_ms =
        i64::try_from(offset).map_err(|_| ClockEstimateError::ArithmeticOverflow)?;

    Ok(ClockEstimate {
        board_offset_ms,
        rtt_ms: rtt,
    })
}

/// 从一次完整采样做纯估算：只做**采样自证**的校验（本端代际一致、板端申报稳定、
/// 质量字符串可识别、负时延非法、RTT 阈值），不涉及跨样本比较。
///
/// `RttTooLarge` 在此以 `Err` 返回：05 §4 规定过大延迟不可用于偏差估计；质量投影层
/// 再把它归到「降级」而非「非法拒绝」。
#[must_use = "estimation result must be handled"]
pub fn estimate_from_sample(
    sample: &BoardClockSample<'_>,
    policy: &BoardClockPolicy,
) -> Result<ClockEstimate, ClockEstimateError> {
    if sample.t1_controller_clock_epoch != sample.t4_controller_clock_epoch {
        return Err(ClockEstimateError::ControllerEpochMismatch);
    }
    match sample.board_quality {
        BoardReportedQuality::Observed => {}
        BoardReportedQuality::ClockUnstable => {
            return Err(ClockEstimateError::BoardClockUnstable);
        }
        BoardReportedQuality::Unknown => return Err(ClockEstimateError::UnverifiedQuality),
    }
    estimate_board_offset(
        sample.t1_controller_wall_ms,
        sample.t2_board_receive_wall_ms,
        sample.t3_board_send_wall_ms,
        sample.t4_controller_wall_ms,
        sample.t1_controller_clock_epoch,
        sample.t4_controller_clock_epoch,
        false,
        sample.board_boot_id,
        sample.board_boot_id,
        policy.max_rtt_ms,
    )
}

/// 两个板端身份是否属于同一可平滑代际：`boot_id` 与 `clock_epoch` 都相同才允许。
#[must_use]
pub fn is_smoothing_continuous(
    current: &BoardClockIdentity<'_>,
    previous: &BoardClockIdentity<'_>,
) -> bool {
    current.boot_id == previous.boot_id && current.clock_epoch == previous.clock_epoch
}

/// 板端墙钟是否可疑地早于门限（无 RTC / 1970 异常时间戳）。
#[must_use]
pub fn board_wall_clock_is_suspect(board_wall_ms: i64, policy: &BoardClockPolicy) -> bool {
    board_wall_ms < policy.no_rtc_wall_ms_ceiling
}

/// 质量投影：把一次采样连同其身份上下文投影为可审计的三态质量结论（05 §3 TIME-03）。
///
/// - `expected`：会话/缓存申报的板端身份（如已观察到的 boot_id）。跨 boot_id 或跨
///   `clock_epoch` 的样本**显式拒绝**，绝不与前序样本平滑；
/// - `previous`：前序已接受样本的身份。跨代际同样显式拒绝，调用方须据此重置平滑；
/// - 检查顺序固定，全部原因都收集（去重）后一并返回，不因先命中一项而掩盖其余。
#[must_use]
pub fn assess_board_clock_sample(
    sample: &BoardClockSample<'_>,
    expected: Option<&BoardClockIdentity<'_>>,
    previous: Option<&BoardClockIdentity<'_>>,
    policy: &BoardClockPolicy,
) -> BoardClockVerdict {
    let mut reasons: Vec<BoardClockReason> = Vec::new();

    if sample.t1_controller_clock_epoch != sample.t4_controller_clock_epoch {
        push_unique(&mut reasons, BoardClockReason::ControllerEpochMismatch);
    }
    match sample.board_quality {
        BoardReportedQuality::Observed => {}
        BoardReportedQuality::ClockUnstable => {
            push_unique(&mut reasons, BoardClockReason::BoardClockUnstable)
        }
        BoardReportedQuality::Unknown => {
            push_unique(&mut reasons, BoardClockReason::UnverifiedQuality)
        }
    }
    let identity = sample.identity();
    if let Some(expected) = expected {
        if identity.boot_id != expected.boot_id {
            push_unique(&mut reasons, BoardClockReason::BootMismatch);
        }
        if identity.clock_epoch != expected.clock_epoch {
            push_unique(&mut reasons, BoardClockReason::BoardEpochMismatch);
        }
    }
    if let Some(previous) = previous
        && !is_smoothing_continuous(&identity, previous)
    {
        if identity.boot_id != previous.boot_id {
            push_unique(&mut reasons, BoardClockReason::BootMismatch);
        } else {
            push_unique(&mut reasons, BoardClockReason::BoardEpochMismatch);
        }
    }

    if reasons.iter().any(|reason| reason.is_rejection()) {
        return BoardClockVerdict {
            quality: BoardClockQuality::Rejected,
            estimate: None,
            reasons,
        };
    }

    match estimate_from_sample(sample, policy) {
        Ok(estimate) => {
            if board_wall_clock_is_suspect(sample.t2_board_receive_wall_ms, policy)
                || board_wall_clock_is_suspect(sample.t3_board_send_wall_ms, policy)
            {
                push_unique(&mut reasons, BoardClockReason::NoRtcSuspected);
            }
            let quality = if reasons.is_empty() {
                BoardClockQuality::Ok
            } else {
                BoardClockQuality::Degraded
            };
            BoardClockVerdict {
                quality,
                estimate: Some(estimate),
                reasons,
            }
        }
        Err(ClockEstimateError::RttTooLarge) => {
            push_unique(&mut reasons, BoardClockReason::RttTooLarge);
            BoardClockVerdict {
                quality: BoardClockQuality::Degraded,
                estimate: None,
                reasons,
            }
        }
        Err(error) => {
            push_unique(&mut reasons, BoardClockReason::from(error));
            BoardClockVerdict {
                quality: BoardClockQuality::Rejected,
                estimate: None,
                reasons,
            }
        }
    }
}

/// 同代际内的有界平滑（简单加权平均，整数四舍五入，远离零）。
///
/// 调用方**必须**先用 [`is_smoothing_continuous`] 或 [`assess_board_clock_sample`] 确认
/// 两个样本同处一个 boot_id/clock_epoch 代际；跨代际在此没有额外信号可依据，只能由
/// 调用方以前序状态为 `None` 表达「重置基线」。RTT 取较新观测，不做历史平均。
pub fn smooth_board_offset(
    previous: Option<ClockEstimate>,
    current: ClockEstimate,
    previous_weight_percent: u8,
) -> Result<ClockEstimate, ClockEstimateError> {
    let Some(previous) = previous else {
        return Ok(current);
    };
    let weight = i128::from(previous_weight_percent.min(100));
    let numerator = i128::from(previous.board_offset_ms) * weight
        + i128::from(current.board_offset_ms) * (100 - weight);
    // 四舍五入且远离零：权重端点 0/100 必须是恒等变换，不能被舍入项推移 1ms
    let blended = if numerator < 0 {
        -((-numerator + 50) / 100)
    } else {
        (numerator + 50) / 100
    };
    let board_offset_ms =
        i64::try_from(blended).map_err(|_| ClockEstimateError::ArithmeticOverflow)?;
    Ok(ClockEstimate {
        board_offset_ms,
        rtt_ms: current.rtt_ms,
    })
}

/// 追加原因，保持顺序且去重。
fn push_unique(reasons: &mut Vec<BoardClockReason>, reason: BoardClockReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn board_clock_estimation_calculates_offset_and_rtt() {
        let epoch = Uuid::new_v4();
        // T1 = 1000, T2 = 1020, T3 = 1025, T4 = 1050
        // Board offset = ((1020 - 1000) + (1025 - 1050)) / 2 = (20 - 25) / 2 = -2 ms
        // RTT = (1050 - 1000) - (1025 - 1020) = 50 - 5 = 45 ms
        // 两主机独立 epoch：中控 T1/T4 epoch 均为 epoch，板端未报告 unstable
        let res = estimate_board_offset(
            1000,
            1020,
            1025,
            1050,
            epoch,
            epoch,
            false,
            "boot-1",
            "boot-1",
            DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED,
        )
        .unwrap();
        assert_eq!(res.board_offset_ms, -2);
        assert_eq!(res.rtt_ms, 45);
    }

    #[test]
    fn board_clock_rejects_negative_rtt_and_rtt_over_threshold() {
        let epoch = Uuid::new_v4();
        // Negative RTT: (T4 - T1) < (T3 - T2)
        assert!(matches!(
            estimate_board_offset(
                1000, 1000, 1050, 1010, epoch, epoch, false, "boot-1", "boot-1", 2000
            ),
            Err(ClockEstimateError::NegativeRtt)
        ));
        // RTT > 2000 ms: (T4 - T1) - (T3 - T2) = 2050 - 10 = 2040 > 2000
        assert!(matches!(
            estimate_board_offset(
                1000, 1000, 1010, 3050, epoch, epoch, false, "boot-1", "boot-1", 2000
            ),
            Err(ClockEstimateError::RttTooLarge)
        ));
    }

    #[test]
    fn board_clock_rejects_controller_epoch_mismatch_and_board_unstable_and_boot_mismatch() {
        let e1 = Uuid::new_v4();
        let e2 = Uuid::new_v4();
        // 中控本端跨采样发生跳钟/重启导致前后 epoch 不一致，同机不一致拒绝
        assert!(matches!(
            estimate_board_offset(
                1000, 1010, 1020, 1040, e1, e2, false, "boot-1", "boot-1", 2000
            ),
            Err(ClockEstimateError::ControllerEpochMismatch)
        ));
        // 板端报告采样期间跳钟/不稳定
        assert!(matches!(
            estimate_board_offset(
                1000, 1010, 1020, 1040, e1, e1, true, "boot-1", "boot-1", 2000
            ),
            Err(ClockEstimateError::BoardClockUnstable)
        ));
        // 板端 boot 不匹配
        assert!(matches!(
            estimate_board_offset(
                1000, 1010, 1020, 1040, e1, e1, false, "boot-1", "boot-2", 2000
            ),
            Err(ClockEstimateError::BootMismatch)
        ));
    }

    #[test]
    fn board_clock_preserves_large_stable_offset_and_checked_bounds() {
        let epoch = Uuid::new_v4();
        // 板端无 RTC 或 1970 异常时间，但采样稳定且 RTT 有界：应正确计算大稳定偏差而非拒绝
        // T1 = 1_700_000_000_000, T2 = 0, T3 = 10, T4 = 1_700_000_000_050
        // total_elapsed = 50ms, processing = 10ms, RTT = 40ms <= 2000ms
        // offset = ((0 - 1_700_000_000_000) + (10 - 1_700_000_000_050)) / 2 = -1_700_000_000_020 ms
        let res = estimate_board_offset(
            1_700_000_000_000,
            0,
            10,
            1_700_000_000_050,
            epoch,
            epoch,
            false,
            "boot-1",
            "boot-1",
            2000,
        )
        .unwrap();
        assert_eq!(res.rtt_ms, 40);
        assert_eq!(res.board_offset_ms, -1_700_000_000_020);
    }

    #[test]
    fn board_clock_rejects_arithmetic_overflow() {
        let epoch = Uuid::new_v4();
        // T4 - T1 溢出：t4 = i64::MAX, t1 = -1 -> i64::MAX - (-1) 溢出
        assert!(matches!(
            estimate_board_offset(
                -1,
                0,
                0,
                i64::MAX,
                epoch,
                epoch,
                false,
                "boot-1",
                "boot-1",
                2000
            ),
            Err(ClockEstimateError::ArithmeticOverflow)
        ));
        // T3 - T2 溢出
        assert!(matches!(
            estimate_board_offset(
                0,
                -1,
                i64::MAX,
                100,
                epoch,
                epoch,
                false,
                "boot-1",
                "boot-1",
                2000
            ),
            Err(ClockEstimateError::ArithmeticOverflow)
        ));
    }

    #[test]
    fn board_clock_rejects_rewound_wall_clock_before_arithmetic() {
        let epoch = Uuid::new_v4();
        // T4 < T1：中控本端采样期间墙钟倒流
        assert!(matches!(
            estimate_board_offset(
                1000, 1010, 1020, 999, epoch, epoch, false, "boot-1", "boot-1", 2000
            ),
            Err(ClockEstimateError::ControllerClockRewound)
        ));
        // T3 < T2：板端采样期间墙钟倒流（RTT 会虚高，必须在减法前拒绝）
        assert!(matches!(
            estimate_board_offset(
                1000, 1050, 1040, 1060, epoch, epoch, false, "boot-1", "boot-1", 2000
            ),
            Err(ClockEstimateError::BoardClockRewound)
        ));
    }
}

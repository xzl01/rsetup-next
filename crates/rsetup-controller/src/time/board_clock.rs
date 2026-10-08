use uuid::Uuid;

pub const DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED: i64 = 2000; // 05 §2 待审建议默认，非定稿协议常量

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
    #[error("arithmetic overflow in clock estimation")]
    ArithmeticOverflow,
}

// 纯算法逻辑：两主机独立 epoch，中控仅比对本端 T1 与 T4 epoch 一致性
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
}

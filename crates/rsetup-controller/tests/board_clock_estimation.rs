//! 板端时钟采样四时戳估算与质量判定离线单测（05 §3 TIME-03）。
//!
//! 全部为纯函数断言：零网络、零数据库、零真实墙钟读取，因此每一条守则分支都可枚举。
//! 计算式与拒绝/降级守则（05 §3「时间证据」+ §4「板端时间校验」）：
//!
//! - `rtt = (T4 - T1) - (T3 - T2)`，`board_minus_controller = ((T2 - T1) + (T3 - T4)) / 2`；
//! - `T4 < T1` 或 `T3 < T2`（时钟倒流/单调性破坏）或 `RTT < 0`：负时延非法样本，显式拒绝；
//! - `RTT > 上限`（默认 2000ms，可配置）：标记为降级/不可靠，不判非法、也不用于偏差估计；
//! - 跨 `boot_id`、跨 `clock_epoch` 样本：显式拒绝，且不与前序样本平滑；
//! - 无 RTC / 1970 异常墙钟：只降级并附原因，大但稳定的偏差仍要报告。
//!
//! 两主机 epoch 独立：断言中从不要求中控 `clock_epoch` 等于板端 `clock_epoch`。

use uuid::Uuid;

use rsetup_controller::time::board_clock::{
    BoardClockIdentity, BoardClockPolicy, BoardClockQuality, BoardClockReason, BoardClockSample,
    BoardReportedQuality, ClockEstimate, ClockEstimateError, DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED,
    DEFAULT_NO_RTC_WALL_MS_CEILING_MS, DEFAULT_SMOOTHING_PREVIOUS_WEIGHT_PERCENT,
    assess_board_clock_sample, board_wall_clock_is_suspect, estimate_board_offset,
    estimate_from_sample, is_smoothing_continuous, smooth_board_offset,
};

const BOOT_A: &str = "0f6f6a52-1c1e-4a10-9a3f-6b0f0a1c2d31";
const BOOT_B: &str = "7b2c9d0e-5a41-4f3e-8f27-1c0d9e8a7b65";
const BOARD_EPOCH_1: &str = "78e1f2a3-4b5c-4d6e-8f70-9a1b2c3d4e5f";
const BOARD_EPOCH_2: &str = "21d4c3b2-1a0f-4e5d-9c8b-7a6f5e4d3c2b";

/// 建议默认上限下构造策略，便于单点覆盖 RTT 阈值。
fn policy() -> BoardClockPolicy {
    BoardClockPolicy::default()
}

/// 构造一份「健康」采样：RTT 45ms、板端偏移 -2ms、板端墙钟已是 2024 年代。
fn healthy_sample<'a>(
    controller_epoch: Uuid,
    board_boot: &'a str,
    board_epoch: &'a str,
    quality: BoardReportedQuality,
) -> BoardClockSample<'a> {
    BoardClockSample {
        t1_controller_wall_ms: 1_700_000_000_000,
        t2_board_receive_wall_ms: 1_700_000_000_020,
        t3_board_send_wall_ms: 1_700_000_000_025,
        t4_controller_wall_ms: 1_700_000_000_050,
        t1_controller_clock_epoch: controller_epoch,
        t4_controller_clock_epoch: controller_epoch,
        board_boot_id: board_boot,
        board_clock_epoch: board_epoch,
        board_quality: quality,
    }
}

// ---------------------------------------------------------------------------
// 1. 正常计算：四时戳公式
// ---------------------------------------------------------------------------

#[test]
fn four_timestamp_formula_matches_specification() {
    let controller_epoch = Uuid::new_v4();
    let sample = healthy_sample(
        controller_epoch,
        BOOT_A,
        BOARD_EPOCH_1,
        BoardReportedQuality::Observed,
    );

    let estimate = estimate_from_sample(&sample, &policy()).expect("健康样本必须可估算");
    // RTT = (T4 - T1) - (T3 - T2) = 50 - 5 = 45
    assert_eq!(estimate.rtt_ms, 45);
    // offset = ((T2 - T1) + (T3 - T4)) / 2 = (20 + (-25)) / 2 = -2（向零取整）
    assert_eq!(estimate.board_offset_ms, -2);

    // 正偏移方向：板端墙钟整体领先中控
    let leading = BoardClockSample {
        t1_controller_wall_ms: 1_700_000_000_000,
        t2_board_receive_wall_ms: 1_700_000_100_020,
        t3_board_send_wall_ms: 1_700_000_100_030,
        t4_controller_wall_ms: 1_700_000_000_050,
        ..sample.clone()
    };
    let leading = estimate_from_sample(&leading, &policy()).unwrap();
    // offset = ((T2-T1) + (T3-T4)) / 2 = (100_020 + 99_980) / 2 = 100_000
    assert_eq!(leading.board_offset_ms, 100_000);
    assert_eq!(leading.rtt_ms, 40);
}

#[test]
fn asymmetric_network_delays_produce_expected_offset_sign() {
    let controller_epoch = Uuid::new_v4();
    // 下行 40ms、上行 4ms：板端处理 6ms，RTT = 44ms
    // offset = ((40) + (-4)) / 2 = 18：不对称只污染一半，不改变公式
    let sample = BoardClockSample {
        t1_controller_wall_ms: 1_700_000_000_000,
        t2_board_receive_wall_ms: 1_700_000_000_040,
        t3_board_send_wall_ms: 1_700_000_000_046,
        t4_controller_wall_ms: 1_700_000_000_050,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    let estimate = estimate_from_sample(&sample, &policy()).unwrap();
    assert_eq!(estimate.rtt_ms, 44);
    assert_eq!(estimate.board_offset_ms, 18);
}

#[test]
fn fractional_offsets_truncate_towards_zero() {
    let controller_epoch = Uuid::new_v4();
    // (T2 - T1) + (T3 - T4) = 3 + 0 = 3 -> 3 / 2 = 1：毫秒级向零取整，不外溢
    let sample = BoardClockSample {
        t1_controller_wall_ms: 1_700_000_000_000,
        t2_board_receive_wall_ms: 1_700_000_000_003,
        t3_board_send_wall_ms: 1_700_000_000_006,
        t4_controller_wall_ms: 1_700_000_000_006,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    assert_eq!(
        estimate_from_sample(&sample, &policy())
            .unwrap()
            .board_offset_ms,
        1
    );

    let negative = BoardClockSample {
        t1_controller_wall_ms: 1_700_000_000_000,
        t2_board_receive_wall_ms: 1_700_000_000_003,
        t3_board_send_wall_ms: 1_700_000_000_004,
        t4_controller_wall_ms: 1_700_000_000_006,
        ..sample.clone()
    };
    // (3 + (-2)) / 2 = 1 / 2 = 0：向零取整，不引入舍入偏差放大
    assert_eq!(
        estimate_from_sample(&negative, &policy())
            .unwrap()
            .board_offset_ms,
        0
    );
}

#[test]
fn boundary_rtt_exactly_at_limit_is_accepted() {
    let controller_epoch = Uuid::new_v4();
    // RTT 恰为上限 2000ms：阈值是「大于」才降级，边界必须可用
    let sample = BoardClockSample {
        t1_controller_wall_ms: 1_700_000_000_000,
        t2_board_receive_wall_ms: 1_700_000_000_000,
        t3_board_send_wall_ms: 1_700_000_000_010,
        t4_controller_wall_ms: 1_700_000_000_000 + DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED + 10,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    let estimate = estimate_from_sample(&sample, &policy()).expect("边界 RTT 必须可估算");
    assert_eq!(estimate.rtt_ms, DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED);
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Ok);
    assert!(verdict.is_usable_for_smoothing());
}

// ---------------------------------------------------------------------------
// 2. 负时延非法样本：显式拒绝
// ---------------------------------------------------------------------------

#[test]
fn controller_clock_rewound_during_sample_is_rejected() {
    let controller_epoch = Uuid::new_v4();
    // T4 < T1：中控本端单调性破坏（时钟倒流），四个时戳不再满足算法前提
    let sample = BoardClockSample {
        t4_controller_wall_ms: 1_699_999_999_999,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    assert!(matches!(
        estimate_from_sample(&sample, &policy()),
        Err(ClockEstimateError::ControllerClockRewound)
    ));

    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.is_rejected());
    assert!(!verdict.is_usable_for_smoothing());
    assert!(verdict.estimate.is_none());
    assert!(verdict.contains(BoardClockReason::ControllerClockRewound));
    assert!(verdict.reasons.iter().all(|reason| reason.is_rejection()));
}

#[test]
fn board_clock_rewound_during_sample_is_rejected_even_when_rtt_would_be_positive() {
    let controller_epoch = Uuid::new_v4();
    // T3 < T2：板端墙钟倒流。若只按 RTT 判负，此处 RTT = 50 - (-10) = 60 > 0 会被误接受，
    // 因此单调性检查必须在减法之前显式拒绝。
    let sample = BoardClockSample {
        t2_board_receive_wall_ms: 1_700_000_000_060,
        t3_board_send_wall_ms: 1_700_000_000_050,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    assert!(matches!(
        estimate_from_sample(&sample, &policy()),
        Err(ClockEstimateError::BoardClockRewound)
    ));
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::BoardClockRewound));
}

#[test]
fn negative_rtt_is_rejected_as_illegal_sample() {
    let controller_epoch = Uuid::new_v4();
    // 板端「处理耗时」大于中控观测的总跨度：RTT = 10 - 50 = -40 < 0
    let sample = BoardClockSample {
        t2_board_receive_wall_ms: 1_700_000_000_000,
        t3_board_send_wall_ms: 1_700_000_000_050,
        t4_controller_wall_ms: 1_700_000_000_010,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    assert!(matches!(
        estimate_from_sample(&sample, &policy()),
        Err(ClockEstimateError::NegativeRtt)
    ));
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::NegativeRtt));
}

#[test]
fn negative_delay_rejects_before_any_epoch_or_stability_shortcut() {
    let controller_epoch = Uuid::new_v4();
    // 直接走低层纯函数：负时延不得因阈值调大而变可用
    assert!(matches!(
        estimate_board_offset(
            1_000,
            1_050,
            1_040,
            999,
            controller_epoch,
            controller_epoch,
            false,
            BOOT_A,
            BOOT_A,
            i64::MAX,
        ),
        Err(ClockEstimateError::ControllerClockRewound)
    ));
}

// ---------------------------------------------------------------------------
// 3. 跨代际与两端独立 epoch
// ---------------------------------------------------------------------------

#[test]
fn controller_epoch_mismatch_within_sample_is_rejected() {
    // 中控 T1 与 T4 的 clock_epoch 不同：本端采样期间跳钟/重启换代际
    let sample = BoardClockSample {
        t4_controller_clock_epoch: Uuid::new_v4(),
        ..healthy_sample(
            Uuid::new_v4(),
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    assert!(matches!(
        estimate_from_sample(&sample, &policy()),
        Err(ClockEstimateError::ControllerEpochMismatch)
    ));
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::ControllerEpochMismatch));
}

#[test]
fn board_and_controller_epochs_are_never_compared_across_hosts() {
    // 两端 UUID 独立生成：板端代际与中控代际毫无关系，采样仍必须可用
    let controller_epoch = Uuid::new_v4();
    let sample = healthy_sample(
        controller_epoch,
        BOOT_A,
        BOARD_EPOCH_1,
        BoardReportedQuality::Observed,
    );
    assert_ne!(
        sample.board_clock_epoch,
        sample.t1_controller_clock_epoch.to_string()
    );
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Ok);
    assert!(verdict.estimate.is_some());
}

#[test]
fn cross_boot_sample_is_rejected_and_not_smoothed_with_previous_sample() {
    let controller_epoch = Uuid::new_v4();
    let previous = BoardClockIdentity {
        boot_id: BOOT_A,
        clock_epoch: BOARD_EPOCH_1,
    };
    // 板端重启：boot_id 变了，即使算法值完全合法也必须拒绝并与前序样本断开
    let after_reboot = healthy_sample(
        controller_epoch,
        BOOT_B,
        BOARD_EPOCH_1,
        BoardReportedQuality::Observed,
    );
    let verdict =
        assess_board_clock_sample(&after_reboot, Some(&previous), Some(&previous), &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::BootMismatch));
    assert!(!verdict.is_usable_for_smoothing());
    // 连续性判定自身也必须判否，调用方据此重置平滑基线
    assert!(!is_smoothing_continuous(
        &after_reboot.identity(),
        &previous
    ));
}

#[test]
fn cross_clock_epoch_sample_is_rejected_without_boot_change() {
    let controller_epoch = Uuid::new_v4();
    let previous = BoardClockIdentity {
        boot_id: BOOT_A,
        clock_epoch: BOARD_EPOCH_1,
    };
    // 同一 boot 内板端时钟代际变化（跳钟/来源切换）：不得与前序样本平滑
    let next_epoch = healthy_sample(
        controller_epoch,
        BOOT_A,
        BOARD_EPOCH_2,
        BoardReportedQuality::Observed,
    );
    let verdict = assess_board_clock_sample(&next_epoch, None, Some(&previous), &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::BoardEpochMismatch));
    assert!(!is_smoothing_continuous(&next_epoch.identity(), &previous));
    // 同 boot、同代际才连续
    assert!(is_smoothing_continuous(
        &previous,
        &BoardClockIdentity {
            boot_id: BOOT_A,
            clock_epoch: BOARD_EPOCH_1,
        }
    ));
}

#[test]
fn expected_boot_mismatch_is_rejected() {
    let controller_epoch = Uuid::new_v4();
    let expected = BoardClockIdentity {
        boot_id: BOOT_A,
        clock_epoch: BOARD_EPOCH_1,
    };
    let sample = healthy_sample(
        controller_epoch,
        BOOT_B,
        BOARD_EPOCH_1,
        BoardReportedQuality::Observed,
    );
    let verdict = assess_board_clock_sample(&sample, Some(&expected), None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::BootMismatch));
}

// ---------------------------------------------------------------------------
// 4. 板端自报质量
// ---------------------------------------------------------------------------

#[test]
fn board_reported_clock_unstable_is_rejected() {
    let controller_epoch = Uuid::new_v4();
    let sample = healthy_sample(
        controller_epoch,
        BOOT_A,
        BOARD_EPOCH_1,
        BoardReportedQuality::ClockUnstable,
    );
    assert!(matches!(
        estimate_from_sample(&sample, &policy()),
        Err(ClockEstimateError::BoardClockUnstable)
    ));
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::BoardClockUnstable));
    assert!(!sample.board_reports_stable());
}

#[test]
fn unrecognised_board_quality_fails_closed() {
    // 03 未审阅过的质量字符串不能被当成 observed 接受
    assert_eq!(
        BoardReportedQuality::from_wire("ntp_valid"),
        BoardReportedQuality::Unknown
    );
    assert_eq!(
        BoardReportedQuality::from_wire(""),
        BoardReportedQuality::Unknown
    );
    assert_eq!(
        BoardReportedQuality::from_wire("observed"),
        BoardReportedQuality::Observed
    );
    assert_eq!(
        BoardReportedQuality::from_wire("clock_unstable"),
        BoardReportedQuality::ClockUnstable
    );

    let sample = healthy_sample(
        Uuid::new_v4(),
        BOOT_A,
        BOARD_EPOCH_1,
        BoardReportedQuality::Unknown,
    );
    assert!(matches!(
        estimate_from_sample(&sample, &policy()),
        Err(ClockEstimateError::UnverifiedQuality)
    ));
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert!(verdict.contains(BoardClockReason::UnverifiedQuality));
}

// ---------------------------------------------------------------------------
// 5. 过大延迟：降级/不可靠，而非非法
// ---------------------------------------------------------------------------

#[test]
fn oversized_rtt_is_degraded_not_rejected() {
    let controller_epoch = Uuid::new_v4();
    // RTT = 2040ms > 2000ms
    let sample = BoardClockSample {
        t2_board_receive_wall_ms: 1_700_000_000_000,
        t3_board_send_wall_ms: 1_700_000_000_010,
        t4_controller_wall_ms: 1_700_000_002_050,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    assert!(matches!(
        estimate_from_sample(&sample, &policy()),
        Err(ClockEstimateError::RttTooLarge)
    ));

    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Degraded);
    assert!(!verdict.is_rejected());
    assert!(verdict.contains(BoardClockReason::RttTooLarge));
    // 05 §4：过大延迟不可用于偏差估计
    assert!(verdict.estimate.is_none());
    assert!(!verdict.is_usable_for_smoothing());
    // 降级原因不属于「显式拒绝」
    assert!(!BoardClockReason::RttTooLarge.is_rejection());
}

#[test]
fn oversized_rtt_threshold_is_configurable() {
    let controller_epoch = Uuid::new_v4();
    // RTT = 4000ms：默认 2s 上限下降级，放宽到 60s 后必须可用（证明阈值可配置）
    let sample = BoardClockSample {
        t2_board_receive_wall_ms: 1_700_000_000_000,
        t3_board_send_wall_ms: 1_700_000_000_010,
        t4_controller_wall_ms: 1_700_000_004_010,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    let relaxed = BoardClockPolicy::with_max_rtt_ms(60_000);
    let estimate = estimate_from_sample(&sample, &relaxed).expect("放宽阈值后必须可用");
    assert_eq!(estimate.rtt_ms, 4_000);
    assert!(
        assess_board_clock_sample(&sample, None, None, &relaxed).quality == BoardClockQuality::Ok
    );
    assert!(
        assess_board_clock_sample(&sample, None, None, &policy()).quality
            == BoardClockQuality::Degraded
    );
    // 收紧阈值同样生效：500ms 上限下 4000ms 降级
    assert!(
        assess_board_clock_sample(&sample, None, None, &BoardClockPolicy::with_max_rtt_ms(500))
            .quality
            == BoardClockQuality::Degraded
    );
    assert_eq!(
        BoardClockPolicy::default().max_rtt_ms,
        DEFAULT_MAX_BOARD_RTT_MS_SUGGESTED
    );
}

// ---------------------------------------------------------------------------
// 6. 无 RTC / 1970 异常时间戳：只降级并报告大偏移
// ---------------------------------------------------------------------------

#[test]
fn board_wall_clock_at_1970_is_degraded_but_still_reported() {
    let controller_epoch = Uuid::new_v4();
    // 板端无 RTC，墙钟停在 1970：RTT 40ms 有界，偏差 -1_700_000_000_020ms 巨大但稳定
    let sample = BoardClockSample {
        t2_board_receive_wall_ms: 0,
        t3_board_send_wall_ms: 10,
        t4_controller_wall_ms: 1_700_000_000_050,
        ..healthy_sample(
            controller_epoch,
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    let verdict = assess_board_clock_sample(&sample, None, None, &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Degraded);
    assert!(!verdict.is_rejected());
    assert!(verdict.contains(BoardClockReason::NoRtcSuspected));
    let estimate = verdict
        .estimate
        .expect("大但稳定的偏差必须报告，不直接当无效样本");
    assert_eq!(estimate.rtt_ms, 40);
    assert_eq!(estimate.board_offset_ms, -1_700_000_000_020);
}

#[test]
fn no_rtc_ceiling_is_configurable_and_normal_wall_clock_is_clean() {
    let controller_epoch = Uuid::new_v4();
    let sample = healthy_sample(
        controller_epoch,
        BOOT_A,
        BOARD_EPOCH_1,
        BoardReportedQuality::Observed,
    );
    assert_eq!(
        BoardClockPolicy::default().no_rtc_wall_ms_ceiling,
        DEFAULT_NO_RTC_WALL_MS_CEILING_MS
    );
    assert!(!board_wall_clock_is_suspect(
        sample.t2_board_receive_wall_ms,
        &policy()
    ));
    assert!(board_wall_clock_is_suspect(0, &policy()));
    // 门限之下逐点判定：门限本身不算可疑
    assert!(!board_wall_clock_is_suspect(
        DEFAULT_NO_RTC_WALL_MS_CEILING_MS,
        &policy()
    ));
    assert!(board_wall_clock_is_suspect(
        DEFAULT_NO_RTC_WALL_MS_CEILING_MS - 1,
        &policy()
    ));
    // 收紧门限可把健康采样也标为可疑（可配置性）
    let strict = BoardClockPolicy {
        no_rtc_wall_ms_ceiling: i64::MAX,
        ..BoardClockPolicy::default()
    };
    assert_eq!(
        assess_board_clock_sample(&sample, None, None, &strict).quality,
        BoardClockQuality::Degraded
    );
}

// ---------------------------------------------------------------------------
// 7. 平滑门与算术边界
// ---------------------------------------------------------------------------

#[test]
fn smoothing_blends_only_within_same_generation() {
    let previous = ClockEstimate {
        board_offset_ms: -100,
        rtt_ms: 30,
    };
    let current = ClockEstimate {
        board_offset_ms: 100,
        rtt_ms: 25,
    };
    // 默认前序权重 70%：(-100*70 + 100*30)/100 = -40（四舍五入远离零）
    let blended = smooth_board_offset(
        Some(previous),
        current,
        DEFAULT_SMOOTHING_PREVIOUS_WEIGHT_PERCENT,
    )
    .unwrap();
    assert_eq!(blended.board_offset_ms, -40);
    // RTT 取较新观测，不做历史平均
    assert_eq!(blended.rtt_ms, 25);
    // 权重 0% 全取新值；100% 保留旧值；跨代际由调用方以 None 表达重置
    assert_eq!(
        smooth_board_offset(Some(previous), current, 0)
            .unwrap()
            .board_offset_ms,
        100
    );
    assert_eq!(
        smooth_board_offset(Some(previous), current, 100)
            .unwrap()
            .board_offset_ms,
        -100
    );
    assert_eq!(
        smooth_board_offset(None, current, DEFAULT_SMOOTHING_PREVIOUS_WEIGHT_PERCENT).unwrap(),
        current
    );
}

#[test]
fn smoothing_saturates_weights_and_survives_extreme_offsets() {
    let current = ClockEstimate {
        board_offset_ms: 10,
        rtt_ms: 5,
    };
    // 超过 100 的权重被夹到 100，不可能因权重错误放大偏移
    assert_eq!(
        smooth_board_offset(
            Some(ClockEstimate {
                board_offset_ms: 90,
                rtt_ms: 5
            }),
            current,
            255
        )
        .unwrap()
        .board_offset_ms,
        90
    );
    // 中间量以 i128 计算：两端极值不溢出
    let blended = smooth_board_offset(
        Some(ClockEstimate {
            board_offset_ms: i64::MIN,
            rtt_ms: 5,
        }),
        ClockEstimate {
            board_offset_ms: i64::MAX,
            rtt_ms: 5,
        },
        50,
    )
    .unwrap();
    assert!(blended.board_offset_ms.abs() < i64::MAX);
    // 权重 100% 保留 i64::MIN 本身，仍然可用
    assert_eq!(
        smooth_board_offset(
            Some(ClockEstimate {
                board_offset_ms: i64::MIN,
                rtt_ms: 5
            }),
            current,
            100
        )
        .unwrap()
        .board_offset_ms,
        i64::MIN
    );
}

#[test]
fn arithmetic_overflow_is_rejected_without_wrapping() {
    let controller_epoch = Uuid::new_v4();
    assert!(matches!(
        estimate_board_offset(
            -1,
            0,
            0,
            i64::MAX,
            controller_epoch,
            controller_epoch,
            false,
            BOOT_A,
            BOOT_A,
            2000,
        ),
        Err(ClockEstimateError::ArithmeticOverflow)
    ));
    assert!(matches!(
        estimate_board_offset(
            0,
            -1,
            i64::MAX,
            100,
            controller_epoch,
            controller_epoch,
            false,
            BOOT_A,
            BOOT_A,
            2000,
        ),
        Err(ClockEstimateError::ArithmeticOverflow)
    ));
}

// ---------------------------------------------------------------------------
// 8. 质量投影的整体行为
// ---------------------------------------------------------------------------

#[test]
fn verdict_reports_every_reason_in_check_order() {
    let sample = BoardClockSample {
        t4_controller_clock_epoch: Uuid::new_v4(),
        board_boot_id: BOOT_B,
        board_quality: BoardReportedQuality::ClockUnstable,
        ..healthy_sample(
            Uuid::new_v4(),
            BOOT_A,
            BOARD_EPOCH_1,
            BoardReportedQuality::Observed,
        )
    };
    let previous = BoardClockIdentity {
        boot_id: BOOT_A,
        clock_epoch: BOARD_EPOCH_1,
    };
    let verdict = assess_board_clock_sample(&sample, Some(&previous), Some(&previous), &policy());
    assert_eq!(verdict.quality, BoardClockQuality::Rejected);
    assert_eq!(
        verdict.reasons,
        vec![
            BoardClockReason::ControllerEpochMismatch,
            BoardClockReason::BoardClockUnstable,
            BoardClockReason::BootMismatch,
        ]
    );
    // 去重：同一原因不因 expected 与 previous 同时命中而重复出现
    assert_eq!(
        verdict
            .reasons
            .iter()
            .filter(|reason| **reason == BoardClockReason::BootMismatch)
            .count(),
        1
    );
}

#[test]
fn healthy_sample_projects_clean_ok_verdict() {
    let sample = healthy_sample(
        Uuid::new_v4(),
        BOOT_A,
        BOARD_EPOCH_1,
        BoardReportedQuality::Observed,
    );
    let identity = sample.identity();
    let verdict = assess_board_clock_sample(
        &sample,
        Some(&identity),
        Some(&identity),
        &BoardClockPolicy::default(),
    );
    assert_eq!(verdict.quality, BoardClockQuality::Ok);
    assert!(verdict.reasons.is_empty());
    assert!(verdict.is_usable_for_smoothing());
    assert!(is_smoothing_continuous(&identity, &identity));
    assert!(sample.board_reports_stable());
}

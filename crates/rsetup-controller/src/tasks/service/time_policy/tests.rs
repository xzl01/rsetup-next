use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::*;
use crate::time::evidence::{TimeEvidence, TimeQuality};

/// 固定可复现时刻（不依赖测试运行时刻）。
fn fixed_instant() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("fixed instant in range")
}

fn evidence_at(
    quality: TimeQuality,
    reference: DateTime<Utc>,
    age_ms: Option<u64>,
) -> TimeEvidence {
    TimeEvidence {
        system_wall_utc: reference,
        reference_utc: reference,
        quality,
        clock_epoch: Uuid::new_v4(),
        source: None,
        sample_age_ms: age_ms,
        offset_ms: None,
        uncertainty_ms: None,
    }
}

/// 同一纪元下的「创建时证据 + 当前时钟」对（时间安全核验用例的共用夹具）。
fn same_epoch_pair(age_ms: Option<u64>) -> (TimeEvidence, TimeEvidence) {
    let epoch = Uuid::new_v4();
    let mut created = evidence_at(TimeQuality::NtpValid, fixed_instant(), None);
    let mut clock = evidence_at(TimeQuality::NtpValid, fixed_instant(), age_ms);
    created.clock_epoch = epoch;
    clock.clock_epoch = epoch;
    (created, clock)
}

#[test]
fn time_review_never_extends_deadline() {
    // 核验只能恢复原期限或判过期，绝不允许延长
    assert!(!time_review_allows_extend(
        60_000,
        TimeReviewEvidenceKind::ReferenceDeadlineProven
    ));
    assert!(!time_review_allows_extend(
        1,
        TimeReviewEvidenceKind::Inconclusive
    ));
    assert!(!time_review_allows_extend(
        -1,
        TimeReviewEvidenceKind::ReferenceDeadlineExpired
    ));
    // 任何证据种类下的正数延长都必须拒绝
    for kind in [
        TimeReviewEvidenceKind::ReferenceDeadlineProven,
        TimeReviewEvidenceKind::ReferenceDeadlineExpired,
        TimeReviewEvidenceKind::Inconclusive,
    ] {
        assert!(!time_review_allows_extend(1, kind));
        // 只有「证据已证明 + 延长量为 0」才允许恢复原期限
        assert_eq!(
            time_review_allows_extend(0, kind),
            matches!(kind, TimeReviewEvidenceKind::ReferenceDeadlineProven)
        );
    }
}

#[test]
fn queue_deadline_uses_trusted_time_base_and_sample_age() {
    let reference = fixed_instant();
    let ttl = chrono::Duration::milliseconds(QUEUE_TTL_MS);
    // NtpValid：reference + 样本年龄 + TTL（样本越旧，排队寿命不得被隐性拉长）
    let ntp = queue_deadline_of(&evidence_at(TimeQuality::NtpValid, reference, Some(150)))
        .expect("deadline");
    assert_eq!(ntp, reference + ttl + chrono::Duration::milliseconds(150));
    // NtpValid 且样本年龄缺失：按 0 处理，绝不让创建失败
    let no_age =
        queue_deadline_of(&evidence_at(TimeQuality::NtpValid, reference, None)).expect("deadline");
    assert_eq!(no_age, reference + ttl);
    // SystemFallback/Stale：reference_utc 已是当前墙钟，不重复累加历史 age
    for quality in [TimeQuality::SystemFallback, TimeQuality::Stale] {
        let fallback =
            queue_deadline_of(&evidence_at(quality, reference, Some(9_999))).expect("deadline");
        assert_eq!(
            fallback,
            reference + ttl,
            "{quality:?} 不得重复累加样本年龄"
        );
    }
    assert!(ntp > reference, "排队截止必须晚于创建参考时刻");
}

#[test]
fn time_review_resumes_only_when_every_queue_deadline_is_live() {
    use TimeReviewDecision::ResumeOriginalDeadline;
    use TimeReviewVerdict::*;
    let (created, clock) = same_epoch_pair(None);
    let live = fixed_instant() + chrono::Duration::seconds(60);
    let past = fixed_instant() - chrono::Duration::seconds(1);
    // 全部未过期 + Resume → 整批恢复
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            ResumeOriginalDeadline,
            &[Some(live), Some(live)]
        ),
        Resume
    );
    // 部分过期 + Resume → 绝不允许整批恢复（保持 held，不是错误恢复）
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            ResumeOriginalDeadline,
            &[Some(live), Some(past)]
        ),
        Unverifiable
    );
    // 全部过期 + Resume → 同样不得恢复
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            ResumeOriginalDeadline,
            &[Some(past)]
        ),
        Unverifiable
    );
    // 缺失 queue_deadline 无法证明未过期，按已过期处置
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            ResumeOriginalDeadline,
            &[Some(live), None]
        ),
        Unverifiable
    );
    // 全部已过期 + Expire → 整批 expired
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            TimeReviewDecision::Expire,
            &[Some(past), None]
        ),
        Expire
    );
    // 部分过期 + Expire → 矛盾请求（整批判过期会误伤未过期者）
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            TimeReviewDecision::Expire,
            &[Some(past), Some(live)]
        ),
        Contradiction
    );
}

#[test]
fn time_review_fails_closed_on_untrusted_or_discontinuous_clock() {
    use TimeReviewDecision::ResumeOriginalDeadline;
    use TimeReviewVerdict::{Resume, Unverifiable};
    let live = fixed_instant() + chrono::Duration::seconds(60);
    let deadlines = [Some(live)];
    let (created, clock) = same_epoch_pair(None);
    // 基线：同纪元、双方 NtpValid、未过期 → 允许恢复
    assert_eq!(
        time_review_verdict(Some(&created), &clock, ResumeOriginalDeadline, &deadlines),
        Resume
    );
    // ① 当前时钟不可信
    for quality in [TimeQuality::SystemFallback, TimeQuality::Stale] {
        let mut untrusted = clock.clone();
        untrusted.quality = quality;
        assert_eq!(
            time_review_verdict(
                Some(&created),
                &untrusted,
                ResumeOriginalDeadline,
                &deadlines
            ),
            Unverifiable,
            "{quality:?} 的当前时钟不得参与恢复判定"
        );
    }
    // ② 创建当时时钟不可信（纪元虽连续也一样）
    for quality in [TimeQuality::SystemFallback, TimeQuality::Stale] {
        let mut created_at = created.clone();
        created_at.quality = quality;
        assert_eq!(
            time_review_verdict(
                Some(&created_at),
                &clock,
                ResumeOriginalDeadline,
                &deadlines
            ),
            Unverifiable,
            "创建时 {quality:?} 的原排队期限无法证明有效"
        );
    }
    // ③ 时钟纪元不连续（跳钟/进程换代）：参考系连续性无法证明
    let mut shifted = clock.clone();
    shifted.clock_epoch = Uuid::new_v4();
    assert_eq!(
        time_review_verdict(Some(&created), &shifted, ResumeOriginalDeadline, &deadlines),
        Unverifiable
    );
    // ④ 创建证据缺失（created_time_json 缺失或不可解析）
    assert_eq!(
        time_review_verdict(None, &clock, ResumeOriginalDeadline, &deadlines),
        Unverifiable
    );
}

#[test]
fn stale_sample_clock_cannot_keep_an_expired_task_alive() {
    use TimeReviewDecision::ResumeOriginalDeadline;
    use TimeReviewVerdict::*;
    // 采样时刻 < queue_deadline < 采样时刻 + 样本年龄：必须判过期，
    // 否则陈旧时钟会把已越期的任务错误恢复为 queued。
    let (created, mut clock) = same_epoch_pair(Some(50));
    let deadline = fixed_instant() + chrono::Duration::milliseconds(10);
    assert!(advanced_reference_utc(&clock) > deadline);
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            ResumeOriginalDeadline,
            &[Some(deadline)]
        ),
        Unverifiable
    );
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            TimeReviewDecision::Expire,
            &[Some(deadline)]
        ),
        Expire
    );
    // 样本年龄缺失按 0 推进：同一期限此刻仍未过期，允许恢复
    clock.sample_age_ms = None;
    assert_eq!(
        time_review_verdict(
            Some(&created),
            &clock,
            ResumeOriginalDeadline,
            &[Some(deadline)]
        ),
        Resume
    );
}

#[test]
fn created_time_evidence_parser_is_fail_closed() {
    let evidence = evidence_at(TimeQuality::NtpValid, fixed_instant(), Some(7));
    let raw = serde_json::to_string(&evidence).expect("serialize");
    assert_eq!(parse_created_time_evidence(&raw), Some(evidence));
    // 空串、非对象、缺字段一律 None（无法证明原期限有效，fail-closed）
    for raw in ["", "null", "\"x\"", "{}", "{\"clock_epoch\":\"nope\"}"] {
        assert_eq!(
            parse_created_time_evidence(raw),
            None,
            "{raw:?} 必须解析失败"
        );
    }
}

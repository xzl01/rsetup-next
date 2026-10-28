//! 时间纯策略（RT-N5）：排队时限计算、时限复核纯策略判定与时间证据解析。
//!
//! 时钟纪律：时限复核只接受服务端可信时钟快照（[`TimeEvidence`]），
//! 决不接受请求侧自报时钟或自报证据种类参与分派；任何路径都不会把
//! `created_at` / `observe_deadline` 改成更晚的值。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::time::evidence::{TimeEvidence, TimeQuality};

#[cfg(test)]
mod tests;

/// 时限复核的证据种类（仅记审计，绝不作为分派依据）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeReviewEvidenceKind {
    ReferenceDeadlineProven,
    ReferenceDeadlineExpired,
    Inconclusive,
}

/// 复核结论：只能恢复**原**期限或判过期，绝不写入更晚的期限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeReviewDecision {
    ResumeOriginalDeadline,
    Expire,
}

/// 是否允许「延长」期限：仅当证据为 `ReferenceDeadlineProven` 且延长量为 `0`
/// （即恢复原期限）时放行；任何正数延长一律拒绝，绝不延长。
pub fn time_review_allows_extend(
    requested_extension_ms: i64,
    kind: TimeReviewEvidenceKind,
) -> bool {
    matches!(kind, TimeReviewEvidenceKind::ReferenceDeadlineProven) && requested_extension_ms == 0
}

/// 排队有效期（04 §5.1 建议 60min）：`sub_tasks.queue_deadline` 的唯一 TTL 来源。
pub const QUEUE_TTL_MS: i64 = 3_600_000;

/// 排队截止：与时限复核的比对处于**同一可信时基**，绝不由 DB 宿主墙钟
/// （`DATE_ADD(NOW(6), …)`）生成——数据库漂移会变成排队寿命错配。
///
/// - `NtpValid`：`reference_utc` 是静态采样时刻，须累加样本年龄 `sample_age_ms`，
///   否则样本越旧、排队寿命被隐性拉长（缺失按 0 处理）；
/// - `SystemFallback` / `Stale`：`reference_utc` 已由时钟源刷新为当前墙钟，
///   直接加 TTL，不重复累加历史 age，否则排队截止被双重放大。
///
/// 溢出（TTL 或 age 大到不可表示）返回 `None`，由调用方 fail-closed 拒绝创建。
pub fn queue_deadline_of(created: &TimeEvidence) -> Option<DateTime<Utc>> {
    let age_ms = match created.quality {
        // 样本年龄缺失按 0 处理：缺列不等于「无限陈旧」，更不得让创建失败。
        TimeQuality::NtpValid => created.sample_age_ms.unwrap_or(0),
        TimeQuality::SystemFallback | TimeQuality::Stale => 0,
    };
    let age = chrono::Duration::try_milliseconds(i64::try_from(age_ms).ok()?)?;
    let ttl = chrono::Duration::try_milliseconds(QUEUE_TTL_MS)?;
    created
        .reference_utc
        .checked_add_signed(age)?
        .checked_add_signed(ttl)
}

/// 推进后的当前参考时刻：`reference_utc` 是静态 NTP 采样时刻（driver 周期性复验
/// 才刷新），直接当当前时刻比对，会把「采样时刻 < 期限 < 真实当前时刻」的已过期
/// 任务误判为未过期，因此必须先累加样本年龄再比对。
///
/// 溢出（age 大到不可表示）取 `DateTime::MAX_UTC`：视为早已越过任何合理期限，
/// 绝不回退到采样时刻。
pub fn advanced_reference_utc(clock: &TimeEvidence) -> DateTime<Utc> {
    let age = match clock.sample_age_ms {
        None => chrono::Duration::zero(),
        Some(ms) => chrono::Duration::try_milliseconds(i64::try_from(ms).unwrap_or(i64::MAX))
            .unwrap_or_else(|| chrono::Duration::seconds(i64::MAX / 4)),
    };
    clock
        .reference_utc
        .checked_add_signed(age)
        .unwrap_or(DateTime::<Utc>::MAX_UTC)
}

/// 时间安全核验的纯策略结论：时限复核分派的唯一依据（`evidence_kind` 只记审计）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeReviewVerdict {
    /// 原排队期限的有效性**无法证明**：当前或创建时时钟不可信、时钟纪元不连续、
    /// 创建证据缺失/不可解析，或恢复决策下已存在过期子任务。
    /// 一律保持 `held`、零状态变更（回滚），绝不盲目恢复。
    Unverifiable,
    /// 全部未过期且请求恢复原期限：整批 `held → queued`，不写任何更晚期限。
    Resume,
    /// 全部已过期且请求判过期：整批 `expired` + `QUEUE_EXPIRED` 并释放本代际设备锁。
    Expire,
    /// 请求与服务端真实比对矛盾（仍有未过期子任务却请求整批判过期）：拒绝。
    Contradiction,
}

/// 纯策略 seam：由创建时证据、当前时钟快照与全部 `held` 子任务的**排队截止**
/// 推出唯一允许的处置，零 IO、零副作用。
///
/// `queue_deadlines` 是被锁定的全部 `held` 子任务的 `queue_deadline`；`None`
/// 表示列值缺失，无法证明未过期，按已过期处置。空集合由调用方先行拒绝。
pub fn time_review_verdict(
    created: Option<&TimeEvidence>,
    clock: &TimeEvidence,
    decision: TimeReviewDecision,
    queue_deadlines: &[Option<DateTime<Utc>>],
) -> TimeReviewVerdict {
    // 当前时钟不可信（Stale/SystemFallback）：无法证明任何期限的有效性。
    if clock.quality != TimeQuality::NtpValid {
        return TimeReviewVerdict::Unverifiable;
    }
    // 创建证据缺失或不可解析：`queue_deadline` 的来源时基无法证明可信。
    let Some(created) = created else {
        return TimeReviewVerdict::Unverifiable;
    };
    // 创建当时时钟不可信（纪元虽连续也一样），或跳钟/进程换代导致纪元不连续：
    // 原排队期限的参考系连续性无法证明，绝不盲目恢复。
    if created.quality != TimeQuality::NtpValid || created.clock_epoch != clock.clock_epoch {
        return TimeReviewVerdict::Unverifiable;
    }
    let now = advanced_reference_utc(clock);
    let expired = |deadline: &Option<DateTime<Utc>>| deadline.is_none_or(|deadline| now > deadline);
    let expired_count = queue_deadlines
        .iter()
        .filter(|deadline| expired(deadline))
        .count();
    match decision {
        // 要求**全部**未过期才允许整批恢复；存在任一过期即绝不整批恢复。
        TimeReviewDecision::ResumeOriginalDeadline if expired_count == 0 => {
            TimeReviewVerdict::Resume
        }
        TimeReviewDecision::ResumeOriginalDeadline => TimeReviewVerdict::Unverifiable,
        // 要求**全部**已过期才允许整批判过期；仍有未过期者即误伤，拒绝矛盾请求。
        TimeReviewDecision::Expire if expired_count == queue_deadlines.len() => {
            TimeReviewVerdict::Expire
        }
        TimeReviewDecision::Expire => TimeReviewVerdict::Contradiction,
    }
}

/// 解析主任务 `created_time_json` 为创建时时钟证据；缺失或不可解析一律 `None`
/// （无法证明原期限有效，fail-closed 保持 `held`），绝不猜测近似值。
/// 生产 `TaskApi` 适配器组装 `TaskDetail.created_time` 时复用同一解析口径。
pub(crate) fn parse_created_time_evidence(raw: &str) -> Option<TimeEvidence> {
    serde_json::from_str(raw).ok()
}

//! 指标数据模型与字节精确的标签映射（规范 §5.2/§5.2.2）。
//!
//! 纯函数层：无 I/O、无 async、不 `use axum`/`sqlx`。映射一律 `&[u8]` 逐字节比较，
//! **不做任何字符级折叠**（大小写、尾部空格、`PAD SPACE` 语义都不存在）：数据库侧
//! v3 起已无 CHECK 约束（§5.2.2 事实修正），因此"SQL 字节级分组 + 本文件字节精确
//! 映射 + 未知值 503"是枚举防御的唯一通路。

use crate::ControllerError;

/// 持久层 `devices.admission_state` 的合法字节词汇（`Vec<u8>` 与之逐字节比较）。
pub const ADMISSION_APPROVED: &[u8] = b"APPROVED";
pub const ADMISSION_PENDING: &[u8] = b"PENDING";
/// 持久层 `REVOKED` → 指标标签 `REJECTED`（§5.2.2 刻意留档的命名映射偏差）。
pub const ADMISSION_REVOKED: &[u8] = b"REVOKED";

/// 主任务 `state` 四值域（`2026-09-23-controller-v1-04-task-lifecycle.md`）。
pub const TASK_STATE_QUEUED: &[u8] = b"queued";
pub const TASK_STATE_RUNNING: &[u8] = b"running";
pub const TASK_STATE_HELD: &[u8] = b"held";
pub const TASK_STATE_COMPLETED: &[u8] = b"completed";

/// 主任务 `outcome` 五值域（`integrity.rs` 的真实取值域）。
pub const TASK_OUTCOME_SUCCESS: &[u8] = b"success";
pub const TASK_OUTCOME_FAILED: &[u8] = b"failed";
pub const TASK_OUTCOME_PARTIAL: &[u8] = b"partial";
pub const TASK_OUTCOME_CANCELLED: &[u8] = b"cancelled";
pub const TASK_OUTCOME_UNKNOWN: &[u8] = b"unknown";

/// 设备准入三桶（标签顺序即 §5.1 第 3 条固定顺序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmissionBucket {
    Approved,
    Pending,
    Rejected,
}

impl AdmissionBucket {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "APPROVED",
            Self::Pending => "PENDING",
            Self::Rejected => "REJECTED",
        }
    }
}

/// 主任务四桶（固定标签域 `pending|running|succeeded|failed`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskBucket {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl TaskBucket {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

/// 字节精确映射。`'PENDING '`、`'approved'` 与任何非 UTF-8 字节值都无法匹配合法值，
/// 一律 `None`（→ 采集层 503），绝不静默归入任一桶。
pub fn admission_label(raw: &[u8]) -> Option<AdmissionBucket> {
    if raw == ADMISSION_APPROVED {
        return Some(AdmissionBucket::Approved);
    }
    if raw == ADMISSION_PENDING {
        return Some(AdmissionBucket::Pending);
    }
    if raw == ADMISSION_REVOKED {
        return Some(AdmissionBucket::Rejected);
    }
    None
}

/// `state` 是否落在四值域内（决定未知取值的两条去敏消息走哪一条）。
pub fn task_state_is_known(state: &[u8]) -> bool {
    state == TASK_STATE_QUEUED
        || state == TASK_STATE_RUNNING
        || state == TASK_STATE_HELD
        || state == TASK_STATE_COMPLETED
}

/// §5.2.2 统计口径（唯一判据）：非终态只按 `state` 聚合，`outcome` 既不校验也不参与；
/// 终态 `completed` 严格校验五值与 `NULL`（NULL 归 `failed`），字节级其他取值 → `None`。
pub fn task_bucket(state: &[u8], outcome: Option<&[u8]>) -> Option<TaskBucket> {
    if state == TASK_STATE_QUEUED || state == TASK_STATE_HELD {
        return Some(TaskBucket::Pending);
    }
    if state == TASK_STATE_RUNNING {
        return Some(TaskBucket::Running);
    }
    if state != TASK_STATE_COMPLETED {
        return None;
    }
    // 唯一的 NULL 分支（§5.2.2 末行）：终态却缺 outcome 属契约外数据，确定为未成功，
    // 不使整次抓取失败。`?` 早退会把该分支误判成未知取值，因此显式接住 `None`。
    let Some(outcome) = outcome else {
        return Some(TaskBucket::Failed);
    };
    if outcome == TASK_OUTCOME_SUCCESS {
        return Some(TaskBucket::Succeeded);
    }
    if outcome == TASK_OUTCOME_FAILED
        || outcome == TASK_OUTCOME_PARTIAL
        || outcome == TASK_OUTCOME_CANCELLED
        || outcome == TASK_OUTCOME_UNKNOWN
    {
        return Some(TaskBucket::Failed);
    }
    None
}

/// 设备侧未知取值的固定去敏消息（不含原始取值，I5）。
pub fn unknown_admission_state_error() -> ControllerError {
    ControllerError::Config("metrics device aggregate: unknown admission state".into())
}

/// 任务侧未知取值的两条去敏消息（§5.2.2）：`state` 本身不在四值域 → unknown task
/// state；`state` 合法而 `outcome` 不在枚举 → unknown task outcome。都不回显取值。
pub fn task_aggregate_error(state: &[u8]) -> ControllerError {
    if task_state_is_known(state) {
        ControllerError::Config("metrics task aggregate: unknown task outcome".into())
    } else {
        ControllerError::Config("metrics task aggregate: unknown task state".into())
    }
}

/// 设备准入分布（三桶恒定输出，计数为 0 也输出该序列）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceCounts {
    pub approved: u64,
    pub pending: u64,
    pub rejected: u64,
}

/// 主任务状态分布（四桶）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskCounts {
    pub pending: u64,
    pub running: u64,
    pub succeeded: u64,
    pub failed: u64,
}

/// 连接池内存读数（§5.3 第 5 条，永不发起连接或查询）。
#[derive(Clone, Copy, Debug, Default)]
pub struct PoolCounts {
    pub active: u64,
    pub idle: u64,
}

/// 渲染层唯一的输入形态：golden 测试直接构造它，不需要 DB 或 HTTP（§5.5）。
#[derive(Clone, Copy, Debug)]
pub struct ScrapeValues {
    /// 构建常量：`env!("CARGO_PKG_VERSION")`。
    pub version: &'static str,
    /// 构建常量：`metrics::EDITION`。
    pub edition: &'static str,
    pub uptime_seconds: u64,
    pub devices: DeviceCounts,
    pub tasks: TaskCounts,
    pub pool: PoolCounts,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ControllerError;

    fn message(error: ControllerError) -> String {
        match error {
            ControllerError::Config(text) => text,
            other => panic!("必须是 ControllerError::Config，实得 {other:?}"),
        }
    }

    /// §5.2.2 的持久层词汇：只有三个字节值有映射，其余（含大小写与尾部空格变体）都是未知桶。
    #[test]
    fn admission_label_maps_persisted_vocabulary() {
        assert_eq!(
            admission_label(b"APPROVED"),
            Some(AdmissionBucket::Approved)
        );
        assert_eq!(admission_label(b"PENDING"), Some(AdmissionBucket::Pending));
        assert_eq!(admission_label(b"REVOKED"), Some(AdmissionBucket::Rejected));
        assert_eq!(AdmissionBucket::Rejected.as_str(), "REJECTED");
        for raw in [
            &b"approved"[..],
            &b""[..],
            &b"UNKNOWN"[..],
            &b"APPROVED "[..],
            &b"REVOKED "[..],
            &b"revoked"[..],
            &[0xFF, 0xFE][..],
        ] {
            assert_eq!(admission_label(raw), None, "{raw:?} 必须是未知桶");
        }
    }

    /// §5.2.2 全表逐条：非终态忽略 outcome；completed 严格五值 + NULL→failed。
    #[test]
    fn task_bucket_covers_documented_matrix() {
        assert_eq!(task_bucket(b"queued", None), Some(TaskBucket::Pending));
        assert_eq!(task_bucket(b"held", None), Some(TaskBucket::Pending));
        assert_eq!(task_bucket(b"running", None), Some(TaskBucket::Running));
        for outcome in [
            Some(&b"success"[..]),
            Some(&b"failed"[..]),
            None,
            Some(&b"weird"[..]),
        ] {
            assert_eq!(task_bucket(b"queued", outcome), Some(TaskBucket::Pending));
            assert_eq!(task_bucket(b"held", outcome), Some(TaskBucket::Pending));
            assert_eq!(task_bucket(b"running", outcome), Some(TaskBucket::Running));
        }
        assert_eq!(
            task_bucket(b"completed", Some(b"success")),
            Some(TaskBucket::Succeeded)
        );
        for outcome in [
            &b"failed"[..],
            &b"partial"[..],
            &b"cancelled"[..],
            &b"unknown"[..],
        ] {
            assert_eq!(
                task_bucket(b"completed", Some(outcome)),
                Some(TaskBucket::Failed)
            );
        }
        // 唯一的 NULL 分支：终态却缺 outcome → 确定为未成功（不是整次抓取失败）。
        assert_eq!(task_bucket(b"completed", None), Some(TaskBucket::Failed));
        assert_eq!(TaskBucket::Pending.as_str(), "pending");
        assert_eq!(TaskBucket::Succeeded.as_str(), "succeeded");
    }

    #[test]
    fn task_bucket_fails_closed_on_unknown_state() {
        for state in [
            &b"queued "[..],
            &b"DONE"[..],
            &b""[..],
            &b"Queued"[..],
            &b"HELD"[..],
        ] {
            for outcome in [None, Some(&b"success"[..])] {
                assert_eq!(
                    task_bucket(state, outcome),
                    None,
                    "{state:?} 必须 fail-closed"
                );
            }
        }
    }

    /// 未知 outcome 与未知 state 同等 fail-closed；大小写与尾部空格变体不得被任何折叠语义吞并。
    #[test]
    fn task_bucket_fails_closed_on_unknown_outcome() {
        for outcome in [
            &b"Success"[..],
            &b""[..],
            &b"succeded"[..],
            &b"success "[..],
            &b"SUCCESS"[..],
        ] {
            assert_eq!(
                task_bucket(b"completed", Some(outcome)),
                None,
                "未知 outcome {outcome:?} 必须 fail-closed"
            );
        }
    }

    /// 字节级映射不要求 UTF-8 合法性：state 恒须落在四值域；非终态忽略 outcome。
    #[test]
    fn unknown_bucket_values_need_not_be_valid_utf8() {
        let broken: &[u8] = &[0xFF, 0xFE];
        assert_eq!(task_bucket(broken, Some(b"success")), None);
        assert_eq!(task_bucket(b"completed", Some(broken)), None);
        assert_eq!(
            task_bucket(b"queued", Some(broken)),
            Some(TaskBucket::Pending)
        );
        assert_eq!(
            task_bucket(b"held", Some(broken)),
            Some(TaskBucket::Pending)
        );
        assert_eq!(
            task_bucket(b"running", Some(broken)),
            Some(TaskBucket::Running)
        );
        assert_eq!(admission_label(broken), None);
    }

    /// 两条采集错误消息互不相同，且都不含原始取值（§5.2.2）。
    #[test]
    fn unknown_state_and_outcome_messages_are_distinct() {
        assert_eq!(
            message(task_aggregate_error(b"DONE")),
            "metrics task aggregate: unknown task state"
        );
        assert_eq!(
            message(task_aggregate_error(b"completed")),
            "metrics task aggregate: unknown task outcome"
        );
        assert!(!task_state_is_known(b"DONE"));
        assert!(task_state_is_known(b"held"));
        for text in [
            message(task_aggregate_error(b"DONE")),
            message(task_aggregate_error(b"completed")),
            message(unknown_admission_state_error()),
        ] {
            assert!(!text.contains("DONE"), "错误消息不得回显原始取值：{text}");
            assert!(
                !text.contains("APPROVED"),
                "错误消息不得回显原始取值：{text}"
            );
        }
        assert_eq!(
            message(unknown_admission_state_error()),
            "metrics device aggregate: unknown admission state"
        );
    }
}

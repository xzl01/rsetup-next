//! 保留与生命周期守则：终态/审计/幂等的保留期清理（05 §8、OPS-01）与
//! 受控恢复模式的保留观察窗（05 §9 / 04 §5.2）。
//!
//! ## 05 §8 保留期清理守则
//!
//! 规格原文：「建议终态任务 30 天、审计 180 天；幂等索引至少 24h 且不早于关联任务
//! 清理。held、unknown 持锁、待核实记录不自动清理。」本文件把这段话固化成**唯一**
//! 的删除准入判定 [`is_safe_to_delete`]，并让 [`clean_retention_data`] 的每条 SQL
//! 只能表达与该判定**同口径**的条件——不允许存在「SQL 比纯函数更宽松」的缝隙，
//! 否则一次误删不可逆。
//!
//! 两条硬守则：
//! 1. **未释放设备锁一律不删**（`lock_released == false` 直接 `false`），与状态、
//!    留存时长无关：`Failed` 持锁同样不删。设备锁的释放口径是物理
//!    `DELETE FROM device_operation_locks`（见 `tasks::evidence` 的释放注释），
//!    所以「已释放」在库内的唯一真相是**不存在** `released_at IS NULL` 的归属行，
//!    不能靠 `sub_tasks.state` 猜。
//! 2. **只有可清理终态才可能删**：`succeeded`/`failed`/`cancelled`/`expired`。
//!    `unknown` 虽是 04 §3 的终态，但它**保留设备锁直到核实或管理员释放**
//!    （`tasks::model::holds_device_lock`），05 §8 又明确「unknown 持锁不自动
//!    清理」，因此本模块的可清理终态集合**故意窄于** `tasks::model::is_terminal`，
//!    绝不复用那个判定。
//!
//! ## 05 §9 受控恢复观察窗
//!
//! 受控恢复模式解除后，备份窗口内的 queued 任务**不因备份状态自动重放**（05 §9），
//! 而解除受控模式本身也必须先进入一个最短保留观察窗：进入受控模式的时刻起，
//! 未满观察窗不得解除（`0` 表示不设窗口，仅由核对清单门禁）。
//! 观察窗用单调钟（[`Instant`]）计量，不受墙钟回拨或 NTP 校准影响。

use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::Transaction;
use sqlx::mysql::{MySql, MySqlArguments};

use crate::ControllerError;
use crate::db::DbPool;
use crate::tasks::model::SubTaskState;

/// 05 §8 建议默认值：已完成终态任务保留 30 天。
pub const DEFAULT_COMPLETED_TASK_TTL_DAYS: u32 = 30;
/// 05 §8 建议默认值：审计记录保留 180 天。
pub const DEFAULT_AUDIT_TTL_DAYS: u32 = 180;
/// 05 §8 下限：幂等索引至少 24h，且不早于关联任务清理。
pub const DEFAULT_IDEMPOTENCY_TTL_HOURS: u32 = 24;
/// 单条清理语句的批删上限：限死一次清理的影响行数，避免长事务锁住整表；
/// 需要清完历史由运维按批多次调用，而不是放开上限。
pub const CLEANUP_BATCH_LIMIT: u32 = 500;

/// 保留策略（05 §8 建议值，运维可调）。三个字段都是**最短保留期**语义：配置成 `0`
/// 只表示「到界即可清」，**不会**因此绕过设备锁与终态两条硬守则。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// 已完成终态任务保留天数（建议 30）。
    pub completed_task_ttl_days: u32,
    /// 审计记录保留天数（建议 180）。
    pub audit_ttl_days: u32,
    /// 幂等索引保留小时数（建议下限 24）。
    pub idempotency_ttl_hours: u32,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            completed_task_ttl_days: DEFAULT_COMPLETED_TASK_TTL_DAYS,
            audit_ttl_days: DEFAULT_AUDIT_TTL_DAYS,
            idempotency_ttl_hours: DEFAULT_IDEMPOTENCY_TTL_HOURS,
        }
    }
}

impl RetentionPolicy {
    /// 终态任务清理的库内时间上界：只有 `updated_at` **严格早于**该时刻的行才可能
    /// 进入清理批次。
    pub fn task_cutoff(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        cutoff_by_days(now, self.completed_task_ttl_days)
    }

    /// 审计清理的时间上界：`now - audit_ttl_days`。
    pub fn audit_cutoff(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        cutoff_by_days(now, self.audit_ttl_days)
    }

    /// 幂等索引清理的时间上界。
    ///
    /// 幂等索引住在 `main_tasks` 的 `uq_main_tasks_actor_idem` 唯一键上，主任务行一删
    /// 键位即消失，所以「幂等索引不早于关联任务清理」在实现上取幂等下限与终态任务
    /// 保留期的**较长者**：任一方向都不允许比关联任务先消失。
    pub fn idempotency_cutoff(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        let idempotency_days = self.idempotency_ttl_hours.div_ceil(24).max(1);
        cutoff_by_days(now, self.completed_task_ttl_days.max(idempotency_days))
    }

    /// 以本策略的终态任务保留期为 `policy_ttl_days` 调用唯一守则判定
    /// [`is_safe_to_delete`]，避免调用方各自传错天数。
    pub fn is_safe_to_delete_subtask(
        &self,
        state: SubTaskState,
        lock_released: bool,
        age_days: u32,
    ) -> bool {
        is_safe_to_delete(state, lock_released, age_days, self.completed_task_ttl_days)
    }
}

/// 天粒度时间上界：`now - days`。极端配置溢出时失败关闭为 `now`（本批什么都不删）。
fn cutoff_by_days(now: DateTime<Utc>, days: u32) -> DateTime<Utc> {
    let span = TimeDelta::try_days(i64::from(days)).unwrap_or(TimeDelta::MAX);
    now.checked_sub_signed(span).unwrap_or(now)
}

/// 留存天数：`now - at` 的**整**天数；`at` 来自未来或相等（时钟异常）一律按 `0` 天
/// 处理，只会更保守地不删，绝不会把未决记录说成已超期。
pub fn age_days(at: DateTime<Utc>, now: DateTime<Utc>) -> u32 {
    let elapsed_days = (now - at).num_days();
    if elapsed_days > 0 {
        u32::try_from(elapsed_days).unwrap_or(u32::MAX)
    } else {
        0
    }
}

/// 可清理终态（05 §8）：`succeeded`/`failed`/`cancelled`/`expired`。
///
/// 与 `tasks::model::is_terminal` 的差异是**有意为之**：`unknown` 是终态但持锁待
/// 核实，不在可清理集合内。
pub fn is_deletable_terminal(state: SubTaskState) -> bool {
    matches!(
        state,
        SubTaskState::Succeeded
            | SubTaskState::Failed
            | SubTaskState::Cancelled
            | SubTaskState::Expired
    )
}

/// 物理删除准入判定——本模块唯一守则入口（纯函数，不碰 DB）。
///
/// 三个条件**同时**成立才返回 `true`：
/// 1. `lock_released`：设备锁已释放。为 `false` 时**绝对禁止**删除，与状态和留存
///    时长无关（`Failed` 持锁也不删）；
/// 2. [`is_deletable_terminal`]：处于可清理终态。`Held`、`Unknown`（持锁未决）以及
///    `Queued`/`Dispatching`/`Accepted`/`Verifying` 等执行中状态**无论多久**都不删；
/// 3. `age_days >= policy_ttl_days`：留存已达策略下限（恰好达界即允许）。
///
/// 「保留期未到」与「根本不该删」都返回 `false`，不作区分：调用方无从借返回值把
/// 未决记录说成已超期。
pub fn is_safe_to_delete(
    state: SubTaskState,
    lock_released: bool,
    age_days: u32,
    policy_ttl_days: u32,
) -> bool {
    lock_released && is_deletable_terminal(state) && age_days >= policy_ttl_days
}

/// 一轮清理的去敏计数，不含任何行内容。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RetentionSummary {
    /// 删除的 `sub_tasks` 行数。
    pub subtasks_deleted: u64,
    /// 删除的 `main_tasks` 行数（其幂等索引键位随主任务一并释放）。
    pub main_tasks_deleted: u64,
    /// 删除的 `audit_events` 行数。
    pub audit_events_deleted: u64,
}

/// 「设备锁已释放」的库内真相：不存在本子任务名下未释放的锁行。
///
/// 释放口径是物理 `DELETE`（见模块注释），历史遗留的 `released_at IS NOT NULL` 行
/// 与「无行」同等视为已释放，两种形态都不再阻塞清理。
const LOCK_RELEASED_CONDITION: &str = "NOT EXISTS (SELECT 1 FROM device_operation_locks l \
                                        WHERE l.owner_sub_task_id = sub_tasks.id \
                                        AND l.released_at IS NULL)";

/// 子任务清理：终态白名单 + 已释放锁 + `updated_at` 达界，三条件与
/// [`is_safe_to_delete`] 同口径。`updated_at` 是终态落库时刻（各收敛路径都带
/// `updated_at = NOW(6)`），比 `created_at` 保守——只会更晚删，不会更早。
/// 白名单与 [`LOCK_RELEASED_CONDITION`] 由 [`subtask_cleanup_sql`] 装配，
/// 避免有人只改一处。
const DELETE_SUBTASKS_SQL: &str = "DELETE FROM sub_tasks WHERE {STATES} \
                                   AND {LOCK} AND updated_at < ? LIMIT ?";

/// 子任务终态白名单字面量（与 [`is_deletable_terminal`] 逐一对应；`unknown` 不在内）。
const DELETABLE_TERMINAL_STATES: &str = "state IN ('succeeded','failed','cancelled','expired')";

/// 主任务清理（`main_tasks`，连带释放 `uq_main_tasks_actor_idem` 幂等键位）。
/// 除自身达界（界由 [`RetentionPolicy::idempotency_cutoff`] 取较长者）外，还要求
/// 同一主任务下**已无子任务残留**，落实「幂等索引不早于关联任务清理」。
/// `state = 'completed'` 由 04 §4 收敛写入，`queued`/`running`/`held` 一律不碰。
const DELETE_MAIN_TASKS_SQL: &str = "DELETE FROM main_tasks WHERE state = 'completed' \
                                     AND updated_at < ? \
                                     AND NOT EXISTS (SELECT 1 FROM sub_tasks s \
                                     WHERE s.main_task_id = main_tasks.id) LIMIT ?";

/// `time_evidence` 中事件时刻的文本投影。`audit_events` 没有独立时间列，事件时刻
/// 只在 `time_evidence` JSON 的 `reference_utc`（`db::system_fallback_time_evidence`
/// 写入的 RFC3339 串）。
const AUDIT_REFERENCE: &str =
    "CAST(JSON_UNQUOTE(JSON_EXTRACT(time_evidence, '$.reference_utc')) AS CHAR)";

/// 审计清理：先要求事件时刻**可证为 UTC**（串尾 `Z` 或 `+00:00`），再取其秒精度
/// 墙钟前缀转 `DATETIME(6)` 与上界比较。偏移不是 UTC、或时刻缺失/解析不出的行，
/// 无法证明已度满保留期，一律不删（失败关闭）。
const DELETE_AUDIT_EVENTS_SQL: &str = "DELETE FROM audit_events \
                                       WHERE {REFERENCE} IS NOT NULL \
                                       AND (RIGHT({REFERENCE}, 1) = 'Z' \
                                       OR RIGHT({REFERENCE}, 6) = '+00:00') \
                                       AND CAST(LEFT({REFERENCE}, 19) AS DATETIME(6)) < ? \
                                       LIMIT ?";

/// 子任务清理语句（占位符已展开）；与 [`clean_retention_data`] 所用完全同源。
pub fn subtask_cleanup_sql() -> String {
    DELETE_SUBTASKS_SQL
        .replace("{STATES}", DELETABLE_TERMINAL_STATES)
        .replace("{LOCK}", LOCK_RELEASED_CONDITION)
}

/// 主任务（幂等索引）清理语句。
pub fn main_task_cleanup_sql() -> String {
    String::from(DELETE_MAIN_TASKS_SQL)
}

/// 审计清理语句（时刻投影占位符已展开）。
pub fn audit_cleanup_sql() -> String {
    DELETE_AUDIT_EVENTS_SQL.replace("{REFERENCE}", AUDIT_REFERENCE)
}

/// 位置绑定：与本 crate `tasks::params!` 同口径（`?` 顺序 = 实参顺序），编码失败
/// 立即以 `Database(Encode)` 上抛，绝不静默少绑一个条件而写出「少一个条件的 DELETE」。
/// `operations` 在 `tasks::params!` 的宏作用域之外，故本模块自带一份同口径实现。
macro_rules! bind {
    ($($e:expr),* $(,)?) => {{
        let mut args = sqlx::mysql::MySqlArguments::default();
        $(
            sqlx::Arguments::add(&mut args, &$e)
                .map_err(|err| ControllerError::Database(sqlx::Error::Encode(err)))?;
        )*
        args
    }};
}

/// 保留与清理编排（05 §8、OPS-01）。
///
/// 三条删除依次提交在**同一个短事务**里（子任务 → 主任务 → 审计），任一条失败整体
/// 回滚：绝不出现「子任务已删、主任务留下无子任务孤行」的半程状态。每条语句的行数
/// 受 [`CLEANUP_BATCH_LIMIT`] 限制，历史清完需要按批多次调用。
///
/// 未被本守则覆盖的数据一概不碰：`held`、`unknown` 持锁、待核实的 `recovery_checks`
/// 全部留在库内。空间不足的报警与写入失败关闭属 05 §8 后半段，不在本函数职责内。
pub async fn clean_retention_data(
    pool: &DbPool,
    policy: &RetentionPolicy,
) -> Result<RetentionSummary, ControllerError> {
    let now = Utc::now();
    let task_cutoff = policy.task_cutoff(now);
    let idempotency_cutoff = policy.idempotency_cutoff(now);
    let audit_cutoff = policy.audit_cutoff(now);

    let mut tx = pool.0.begin().await?;

    let subtasks_deleted = delete_in_tx(
        &mut tx,
        &subtask_cleanup_sql(),
        bind!(&task_cutoff, &i64::from(CLEANUP_BATCH_LIMIT)),
    )
    .await?;

    let main_tasks_deleted = delete_in_tx(
        &mut tx,
        &main_task_cleanup_sql(),
        bind!(&idempotency_cutoff, &i64::from(CLEANUP_BATCH_LIMIT)),
    )
    .await?;

    let audit_events_deleted = delete_in_tx(
        &mut tx,
        &audit_cleanup_sql(),
        bind!(&audit_cutoff, &i64::from(CLEANUP_BATCH_LIMIT)),
    )
    .await?;

    tx.commit().await?;

    Ok(RetentionSummary {
        subtasks_deleted,
        main_tasks_deleted,
        audit_events_deleted,
    })
}

/// 短事务内执行一条清理语句，返回受影响行数；SQL 错误原样映射 `Database`（503）。
async fn delete_in_tx(
    tx: &mut Transaction<'_, MySql>,
    sql: &str,
    args: MySqlArguments,
) -> Result<u64, ControllerError> {
    let result = sqlx::query_with::<MySql, _>(sql, args)
        .execute(&mut **tx)
        .await
        .map_err(ControllerError::Database)?;
    Ok(result.rows_affected())
}

// ───────────────────────────── 05 §9 受控恢复观察窗 ─────────────────────────────

/// 受控恢复模式的保留观察窗策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryRetentionPolicy {
    /// 进入受控恢复模式后、允许解除之前必须度满的最短观察窗。
    /// `Duration::ZERO` 表示不设窗口（缺省：解除仅受核对清单门禁）。
    pub observation_window: Duration,
}

impl Default for RecoveryRetentionPolicy {
    fn default() -> Self {
        Self {
            observation_window: Duration::ZERO,
        }
    }
}

impl RecoveryRetentionPolicy {
    /// 指定最短观察窗的解除策略。
    pub const fn after_entry(observation_window: Duration) -> Self {
        Self { observation_window }
    }

    /// 自 `entered_at`（进入受控恢复模式的单调时刻）起，观察窗是否已度满。
    /// `entered_at` 来自未来（时钟异常）按未度满处理，fail-closed。
    pub fn observation_window_open(&self, entered_at: Instant) -> bool {
        if self.observation_window.is_zero() {
            return true;
        }
        Instant::now()
            .checked_duration_since(entered_at)
            .is_some_and(|elapsed| elapsed >= self.observation_window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_window_is_always_open() {
        let policy = RecoveryRetentionPolicy::default();
        assert!(policy.observation_window_open(Instant::now()));
    }

    #[test]
    fn window_opens_only_after_entry_elapses() {
        let policy = RecoveryRetentionPolicy::after_entry(Duration::from_millis(20));
        let just_entered = Instant::now();
        assert!(
            !policy.observation_window_open(just_entered),
            "刚进入受控模式不得立即解除"
        );
        let long_ago = just_entered - Duration::from_secs(3600);
        assert!(policy.observation_window_open(long_ago));
    }

    #[test]
    fn default_policy_matches_spec_suggestions() {
        let policy = RetentionPolicy::default();
        assert_eq!(
            policy.completed_task_ttl_days,
            DEFAULT_COMPLETED_TASK_TTL_DAYS
        );
        assert_eq!(policy.audit_ttl_days, DEFAULT_AUDIT_TTL_DAYS);
        assert_eq!(policy.idempotency_ttl_hours, DEFAULT_IDEMPOTENCY_TTL_HOURS);
        assert_eq!(
            (
                DEFAULT_COMPLETED_TASK_TTL_DAYS,
                DEFAULT_AUDIT_TTL_DAYS,
                DEFAULT_IDEMPOTENCY_TTL_HOURS
            ),
            (30, 180, 24)
        );
    }

    #[test]
    fn subtask_statement_keeps_both_hard_guards() {
        let sql = subtask_cleanup_sql();
        assert!(sql.contains("'succeeded'"));
        assert!(sql.contains("'expired'"));
        assert!(!sql.contains("'unknown'"), "unknown 持锁不得进入清理白名单");
        assert!(!sql.contains("'held'"), "held 不得进入清理白名单");
        assert!(
            sql.contains("l.released_at IS NULL"),
            "缺「已释放设备锁」守卫的清理语句不可接受"
        );
        assert!(sql.ends_with("LIMIT ?"), "清理必须限批");
    }

    #[test]
    fn main_task_statement_releases_idempotency_only_after_children_gone() {
        let sql = main_task_cleanup_sql();
        assert!(sql.contains("state = 'completed'"));
        assert!(sql.contains("NOT EXISTS (SELECT 1 FROM sub_tasks"));
        assert!(sql.ends_with("LIMIT ?"), "清理必须限批");
    }

    #[test]
    fn audit_statement_only_trusts_utc_reference() {
        let sql = audit_cleanup_sql();
        assert!(sql.contains("$.reference_utc"));
        assert!(sql.contains("'Z'"));
        assert!(sql.contains("'+00:00'"));
        assert!(sql.ends_with("LIMIT ?"), "清理必须限批");
    }

    #[test]
    fn idempotency_cutoff_never_earlier_than_task_cleanup() {
        let now = Utc::now();
        let policy = RetentionPolicy {
            completed_task_ttl_days: 90,
            audit_ttl_days: 180,
            idempotency_ttl_hours: 24,
        };
        // 关联任务保留 90 天长于幂等 24h：幂等索引必须跟着 90 天走，不得先消失。
        assert_eq!(policy.idempotency_cutoff(now), policy.task_cutoff(now));

        let short = RetentionPolicy {
            completed_task_ttl_days: 0,
            audit_ttl_days: 180,
            idempotency_ttl_hours: 48,
        };
        // 反向：幂等下限 48h 比任务保留期（0 天）更长时取较长者，仍然不早于关联任务清理。
        assert!(short.idempotency_cutoff(now) <= now - TimeDelta::days(2));
        assert!(short.idempotency_cutoff(now) <= short.task_cutoff(now));
    }

    #[test]
    fn age_days_fails_closed_on_future_timestamps() {
        let now = Utc::now();
        let last_week = now - TimeDelta::days(7);
        assert_eq!(age_days(last_week, now), 7);
        assert_eq!(age_days(now, now), 0);
        assert_eq!(age_days(now + TimeDelta::days(3), now), 0);
    }
}

//! 保留期清理守则测试（05 §8、OPS-01）。
//!
//! 证明的核心行为：终态任务、审计与幂等数据的**物理删除**必须同时满足「已释放设备
//! 锁 + 处于可清理终态 + 留存已达策略下限」三条，缺一条就绝不放行；`Held` 与
//! `Unknown`（持锁未决）无论留存多久都不可删除；未释放锁的记录即使是 `Failed` 也不可
//! 删除。全部为纯内存测试：不触数据库、网络或板端。

use rsetup_controller::operations::retention::{
    CLEANUP_BATCH_LIMIT, DEFAULT_AUDIT_TTL_DAYS, DEFAULT_COMPLETED_TASK_TTL_DAYS,
    DEFAULT_IDEMPOTENCY_TTL_HOURS, RetentionPolicy, age_days, audit_cleanup_sql,
    is_deletable_terminal, is_safe_to_delete, main_task_cleanup_sql, subtask_cleanup_sql,
};
use rsetup_controller::tasks::model::SubTaskState;
use rsetup_controller::tasks::model::is_terminal;

/// 覆盖「无论多久」的用例：从刚落库到远超任何保留期的跨度都试一遍。
const ANY_AGE_DAYS: [u32; 6] = [0, 1, 30, 180, 3_650, u32::MAX];

/// 执行中状态（04 §3：queued/dispatching/accepted/verifying 持锁执行中）。
const IN_FLIGHT_STATES: [SubTaskState; 4] = [
    SubTaskState::Queued,
    SubTaskState::Dispatching,
    SubTaskState::Accepted,
    SubTaskState::Verifying,
];

/// 可清理终态（05 §8：succeeded/failed/cancelled/expired）。
const DELETABLE_TERMINAL: [SubTaskState; 4] = [
    SubTaskState::Succeeded,
    SubTaskState::Failed,
    SubTaskState::Cancelled,
    SubTaskState::Expired,
];

#[test]
fn default_policy_matches_spec_suggestions() {
    // 05 §8 建议值：终态任务 30 天、审计 180 天、幂等索引至少 24h。
    let policy = RetentionPolicy::default();
    assert_eq!(
        policy.completed_task_ttl_days,
        DEFAULT_COMPLETED_TASK_TTL_DAYS
    );
    assert_eq!(policy.audit_ttl_days, DEFAULT_AUDIT_TTL_DAYS);
    assert_eq!(policy.idempotency_ttl_hours, DEFAULT_IDEMPOTENCY_TTL_HOURS);
    assert_eq!(
        (
            policy.completed_task_ttl_days,
            policy.audit_ttl_days,
            policy.idempotency_ttl_hours
        ),
        (30, 180, 24)
    );
}

#[test]
fn released_terminal_record_past_ttl_is_deletable() {
    // 终态 + 已释放锁 + 已超期：三个可清理终态逐一放行。
    for state in DELETABLE_TERMINAL {
        assert!(
            is_safe_to_delete(state, true, 31, DEFAULT_COMPLETED_TASK_TTL_DAYS),
            "{state:?} 已释放锁且超期，必须允许清理"
        );
    }
    // 恰好达界（age == ttl）即视为度满保留期。
    for state in DELETABLE_TERMINAL {
        assert!(
            is_safe_to_delete(state, true, 30, DEFAULT_COMPLETED_TASK_TTL_DAYS),
            "{state:?} 恰好达界应按度满处理"
        );
    }
}

#[test]
fn record_within_retention_window_is_not_deletable() {
    // 未超期：哪怕只差一天也不删。
    for state in DELETABLE_TERMINAL {
        assert!(
            !is_safe_to_delete(state, true, 29, DEFAULT_COMPLETED_TASK_TTL_DAYS),
            "{state:?} 未达保留期不得删除"
        );
        assert!(
            !is_safe_to_delete(state, true, 0, 1),
            "{state:?} 一天未满不得删除"
        );
    }
    // 保留期配成 0 是「到界即可清」，与两条硬守则正交。
    assert!(is_safe_to_delete(SubTaskState::Succeeded, true, 0, 0));
}

#[test]
fn held_is_never_deletable_however_old() {
    // held 是「已下发未执行/待核验」，05 §8 明确不自动清理。
    for age in ANY_AGE_DAYS {
        for lock_released in [true, false] {
            assert!(
                !is_safe_to_delete(SubTaskState::Held, lock_released, age, 30),
                "Held 在 age={age} lock_released={lock_released} 时被误判为可删"
            );
        }
    }
    assert!(!is_deletable_terminal(SubTaskState::Held));
}

#[test]
fn unknown_is_never_deletable_however_old() {
    // unknown 是 04 §3 终态，但持锁直到核实/管理员释放：无论多久都不可删。
    assert!(
        is_terminal(SubTaskState::Unknown),
        "前提：unknown 在状态机里是终态"
    );
    assert!(
        !is_deletable_terminal(SubTaskState::Unknown),
        "可清理终态集合必须窄于 is_terminal：unknown 持锁不清理"
    );
    for age in ANY_AGE_DAYS {
        for lock_released in [true, false] {
            assert!(
                !is_safe_to_delete(SubTaskState::Unknown, lock_released, age, 30),
                "Unknown 在 age={age} lock_released={lock_released} 时被误判为可删"
            );
        }
    }
}

#[test]
fn in_flight_states_are_never_deletable() {
    for state in IN_FLIGHT_STATES {
        for age in ANY_AGE_DAYS {
            for lock_released in [true, false] {
                assert!(
                    !is_safe_to_delete(state, lock_released, age, 30),
                    "{state:?} 在执行中，age={age} lock_released={lock_released} 也不得删除"
                );
            }
        }
    }
}

#[test]
fn record_without_released_lock_is_never_deletable() {
    // 未释放设备锁：绝对禁止删除——即使是 Failed 这种可清理终态、即使留存百年。
    for state in DELETABLE_TERMINAL {
        for age in ANY_AGE_DAYS {
            assert!(
                !is_safe_to_delete(state, false, age, 30),
                "{state:?} 仍持锁，age={age} 也不得物理删除"
            );
        }
    }
    // 反面对照：同一条记录一旦锁已释放，就回到常规保留期判定。
    assert!(is_safe_to_delete(
        SubTaskState::Failed,
        true,
        3_650,
        DEFAULT_COMPLETED_TASK_TTL_DAYS
    ));
}

#[test]
fn policy_helper_never_widens_the_guard() {
    // 便捷入口必须等价于「以本策略的终态保留期调用唯一判定」，不得放松任何条件。
    let policy = RetentionPolicy::default();
    assert!(policy.is_safe_to_delete_subtask(SubTaskState::Expired, true, 30));
    assert!(!policy.is_safe_to_delete_subtask(SubTaskState::Expired, true, 29));
    assert!(!policy.is_safe_to_delete_subtask(SubTaskState::Expired, false, 3_650));
    assert!(!policy.is_safe_to_delete_subtask(SubTaskState::Unknown, true, 3_650));
}

#[test]
fn idempotency_index_never_expires_earlier_than_its_tasks() {
    let now = chrono::Utc::now();
    let policy = RetentionPolicy::default();
    // 幂等索引至少 24h：截止点必须早于当下。
    assert!(policy.idempotency_cutoff(now) < now);
    // 且不早于关联任务清理：默认策略下两者一致（30 天 > 24h）。
    assert_eq!(policy.idempotency_cutoff(now), policy.task_cutoff(now));
    // 幂等下限长于任务保留期时取较长者，审计保留期独立。
    let short_tasks = RetentionPolicy {
        completed_task_ttl_days: 1,
        audit_ttl_days: 180,
        idempotency_ttl_hours: 96,
    };
    assert!(short_tasks.idempotency_cutoff(now) < short_tasks.task_cutoff(now));
    assert!(policy.audit_cutoff(now) < policy.task_cutoff(now));
}

#[test]
fn age_days_fails_closed_on_clock_skew() {
    let now = chrono::Utc::now();
    let last_week = now - chrono::Duration::days(7);
    assert_eq!(age_days(last_week, now), 7);
    assert_eq!(age_days(now, now), 0);
    assert_eq!(age_days(now + chrono::Duration::days(3), now), 0);
}

#[test]
fn cleanup_statements_keep_both_hard_guards() {
    // 清理语句与纯函数同口径：终态白名单不含 held/unknown，且必须带「已释放锁」守卫。
    let subtasks = subtask_cleanup_sql();
    assert!(subtasks.starts_with("DELETE FROM sub_tasks"));
    assert!(subtasks.contains("'succeeded'"));
    assert!(subtasks.contains("'cancelled'"));
    for forbidden in ["'held'", "'unknown'", "'queued'", "'verifying'"] {
        assert!(
            !subtasks.contains(forbidden),
            "清理白名单混入执行中/持锁状态：{forbidden}"
        );
    }
    assert!(
        subtasks.contains("device_operation_locks") && subtasks.contains("l.released_at IS NULL"),
        "清理语句必须显式要求设备锁已释放"
    );

    // 主任务：只清 completed，且必须等子任务全部清理完（幂等索引不早于关联任务）。
    let mains = main_task_cleanup_sql();
    assert!(mains.contains("state = 'completed'"));
    assert!(mains.contains("NOT EXISTS (SELECT 1 FROM sub_tasks"));

    // 审计：只信任可证为 UTC 的事件时刻，其余一律不删。
    let audit = audit_cleanup_sql();
    assert!(audit.contains("$.reference_utc"));
    assert!(audit.contains("'Z'"));
    assert!(audit.contains("'+00:00'"));

    // 三条语句都必须限批，避免一次清理锁住整表。
    for statement in [&subtasks, &mains, &audit] {
        assert!(
            statement.ends_with("LIMIT ?"),
            "清理语句必须限批（CLEANUP_BATCH_LIMIT = {CLEANUP_BATCH_LIMIT}）：{statement}"
        );
    }
}

#[test]
fn guard_returns_false_indistinguishably_for_two_rejections() {
    // 「未超期」与「不该删」都只返回 false：调用方无从把未决记录说成已超期。
    let not_aged = is_safe_to_delete(SubTaskState::Succeeded, true, 1, 30);
    let not_terminal = is_safe_to_delete(SubTaskState::Held, true, 3_650, 30);
    assert!(!not_aged);
    assert!(!not_terminal);
    assert_eq!(not_aged, not_terminal);
}

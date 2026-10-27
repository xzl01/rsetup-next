//! 新管理/任务写者的独立审计命名空间：与 `auth/sqlx_repo.rs` 的
//! `AUTH_PROCESS_EPOCH`/`AUTH_EVENT_SEQ`、`db.rs` 函数内局部
//! `PROCESS_EPOCH`/`EVENT_SEQ` 完全独立——`uq_audit_epoch_seq
//! (process_epoch, event_seq)` 下三个 epoch 各自由 CSPRNG 独立采样，随机碰撞
//! 概率极低但非数学零碰撞；DB 唯一约束 `uq_audit_epoch_seq` 失败关闭，
//! 冲突必须使业务+审计同事务失败，不静默覆盖。
use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use uuid::Uuid;

use crate::ControllerError;

static ADMIN_PROCESS_EPOCH: OnceLock<Uuid> = OnceLock::new();
static ADMIN_EVENT_SEQ: AtomicU64 = AtomicU64::new(0);

/// ADMIN 命名空间进程代际：同一新写者内稳定，UUIDv4 合法。
pub fn admin_process_epoch() -> Uuid {
    *ADMIN_PROCESS_EPOCH.get_or_init(Uuid::new_v4)
}

/// ADMIN 命名空间事件序列：严格递增，checked 溢出返回 `RevisionConflict` 不 wrap。
pub fn next_admin_event_seq() -> Result<u64, ControllerError> {
    // 复用既有辅助（主体不改）；checked 溢出返回 RevisionConflict。
    crate::db::next_event_seq(&ADMIN_EVENT_SEQ)
}

/// 管理写事务的审计插入载荷（11 字段中 7 个来自本结构，其余 4 个由
/// 分配器与 `insert_audit` 生成）。
pub struct AuditInsert<'a> {
    pub actor_kind: &'a str,
    pub actor_user_id: Option<[u8; 16]>,
    pub event_type: &'a str,
    pub target_kind: &'a str,
    pub target_id: Option<String>,
    pub params_redacted: &'a str,
    pub outcome: &'a str,
}

const AUDIT_INSERT_SQL: &str = "INSERT INTO audit_events \
 (id, actor_kind, actor_user_id, event_type, target_kind, target_id, \
  params_redacted, outcome, time_evidence, process_epoch, event_seq) \
 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

/// 业务同事务审计插入：失败整体回滚；只透传 `Database`/`RevisionConflict`，
/// 由调用方决定是否窄化映射。
pub async fn insert_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::MySql>,
    insert: &AuditInsert<'_>,
) -> Result<(), ControllerError> {
    // `time_evidence` 必须非空：复用既有 system_fallback_time_evidence（含 clock_epoch，主体不改）。
    let evidence = crate::db::system_fallback_time_evidence();
    // ADMIN namespace 序列；溢出返回 RevisionConflict，业务同事务失败整体回滚。
    let seq = next_admin_event_seq()?;
    let epoch = admin_process_epoch();
    sqlx::query(AUDIT_INSERT_SQL)
        .bind(Uuid::new_v4().as_bytes().as_slice())
        .bind(insert.actor_kind)
        .bind(insert.actor_user_id.as_ref().map(|id| id.as_slice()))
        .bind(insert.event_type)
        .bind(insert.target_kind)
        .bind(insert.target_id.as_deref())
        .bind(insert.params_redacted)
        .bind(insert.outcome)
        .bind(&evidence)
        .bind(epoch.as_bytes().as_slice())
        .bind(seq)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // 真实 ADMIN 分配器行为失败测试（非源码字符串检查）。注意 `ADMIN_EVENT_SEQ`
    // 为进程级单例、测试间不重置：自增部分直接测 `next_admin_event_seq()`，只断言
    // 相对严格递增（不断言绝对初值）；overflow/nonwrap 不经 ADMIN 单例，改用可控
    // `AtomicU64::new(u64::MAX)` 直接复用 `crate::db::next_event_seq` 辅助验证。
    #[test]
    fn admin_epoch_is_stable_within_writer_and_uuid_v4() {
        let first = admin_process_epoch();
        let second = admin_process_epoch();
        assert_eq!(first, second, "same admin writer must reuse one epoch");
        assert_eq!(first.get_version_num(), 4, "epoch must be a legal UUIDv4");
    }

    #[test]
    fn admin_event_seq_strictly_increases_and_checked_overflow_does_not_wrap() {
        let first = next_admin_event_seq().expect("no overflow");
        let second = next_admin_event_seq().expect("no overflow");
        assert!(second > first, "event_seq must strictly increase");
        // checked 溢出：必须返回 RevisionConflict，不得 wrap 回 0。
        let counter = std::sync::atomic::AtomicU64::new(u64::MAX);
        assert!(matches!(
            crate::db::next_event_seq(&counter),
            Err(ControllerError::RevisionConflict)
        ));
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            u64::MAX,
            "counter must not wrap past u64::MAX"
        );
    }

    // 结构核查（非行为 RED）：防实现顺手改动旧写者；不得凭源码字符串把数据库审计当通过。
    #[test]
    fn legacy_audit_writer_sources_remain_unchanged() {
        let auth = include_str!("auth/sqlx_repo.rs");
        assert!(auth.contains("static AUTH_PROCESS_EPOCH"));
        assert!(auth.contains("static AUTH_EVENT_SEQ"));
        let db = include_str!("db/admission.rs");
        assert!(db.contains("static PROCESS_EPOCH"));
        assert!(db.contains("static EVENT_SEQ"));
        assert!(db.contains("INSERT INTO audit_events"));
    }
}

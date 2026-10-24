-- rsetup-controller 数据库 schema v4 唯一基线 DDL（fresh only，16 张表）。
--
-- 本文件是**唯一**的建表 SQL：空库经显式受控入口（`rsetup-controller-migrate --apply`）
-- 直接创建完整 v4，绝不再走「先建 v1/v2/v3 再 ALTER」的历史执行链；仓库中已不存在
-- migrations/0001..0004。契约：
--   * 11 张 identity 表（schema_meta/users/sessions/roles/role_permissions/device_groups/
--     devices/group_members/grants/admission_decisions/audit_events）
--     + 5 张 runtime 表（task_previews/main_tasks/sub_tasks/device_operation_locks/
--     recovery_checks）= 恰好 16 张表；
--   * 无 FOREIGN KEY、无 CHECK 约束（跨表一致性由应用层 guard 与只读扫描保证）；
--   * 保留 PK / UNIQUE / NOT NULL / UNSIGNED、ASCII 二进制排序规则的用户名列、
--     全部既有索引与 DATETIME(6) 时间列；
--   * 建表后由显式入口写入 schema_meta(schema_version = 4) 并做只读闭合校验。
-- 解析约定：整行 `--` 注释可自由出现，列的说明性注释必须写在对应列的**上一整行**；行内
--   注释、井号注释与块注释都会被 db.rs 的注释形态门禁 fail-closed 拒绝（它们会被顶层逗号
--   切分并入下一个声明片段，伪装成"库里缺列"）。语句一律形如
--   `CREATE TABLE IF NOT EXISTS <表名> (`。

CREATE TABLE IF NOT EXISTS schema_meta (
 singleton TINYINT NOT NULL PRIMARY KEY, schema_version INT NOT NULL, instance_id BINARY(16) NOT NULL,
 initialized BOOLEAN NOT NULL DEFAULT FALSE, authz_epoch BIGINT UNSIGNED NOT NULL DEFAULT 0,
 admin_guard_revision BIGINT UNSIGNED NOT NULL DEFAULT 0
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS users (
 id BINARY(16) NOT NULL PRIMARY KEY, username VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
 display_name VARCHAR(255) NOT NULL, password_hash VARCHAR(255) NOT NULL,
 active BOOLEAN NOT NULL, is_admin BOOLEAN NOT NULL, must_change_password BOOLEAN NOT NULL,
 revision BIGINT UNSIGNED NOT NULL, created_time DATETIME(6) NOT NULL,
 UNIQUE KEY uq_users_username (username)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS sessions (
 token_hash BINARY(32) NOT NULL PRIMARY KEY, user_id BINARY(16) NOT NULL,
 process_epoch BINARY(16) NOT NULL, created_time DATETIME(6) NOT NULL,
 revoked BOOLEAN NOT NULL DEFAULT FALSE, KEY ix_sessions_user_id (user_id)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS roles (
 id BINARY(16) NOT NULL PRIMARY KEY, name VARCHAR(128) NOT NULL,
 builtin BOOLEAN NOT NULL, archived BOOLEAN NOT NULL, revision BIGINT UNSIGNED NOT NULL,
 UNIQUE KEY uq_roles_name (name)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS role_permissions (
 role_id BINARY(16) NOT NULL, permission VARCHAR(128) NOT NULL,
 PRIMARY KEY (role_id, permission)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS device_groups (
 id BINARY(16) NOT NULL PRIMARY KEY, name VARCHAR(128) NOT NULL,
 archived BOOLEAN NOT NULL, revision BIGINT UNSIGNED NOT NULL, UNIQUE KEY uq_device_groups_name (name)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS devices (
 public_key BINARY(32) NOT NULL PRIMARY KEY, display_name VARCHAR(255) NOT NULL,
 descriptor_json JSON NULL, admission_state VARCHAR(16) NOT NULL,
 review_decision VARCHAR(16) NOT NULL, revision BIGINT UNSIGNED NOT NULL,
 first_seen JSON NULL, last_seen JSON NULL, archived BOOLEAN NOT NULL DEFAULT FALSE,
 KEY ix_devices_admission (admission_state, review_decision)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS group_members (
 group_id BINARY(16) NOT NULL, device_id BINARY(32) NOT NULL,
 PRIMARY KEY (group_id, device_id), KEY ix_group_members_device (device_id)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS grants (
 id BINARY(16) NOT NULL PRIMARY KEY, user_id BINARY(16) NOT NULL,
 source_kind VARCHAR(16) NOT NULL, role_id BINARY(16) NULL, permissions JSON NULL,
 scope_kind VARCHAR(16) NOT NULL, scope_group_id BINARY(16) NULL,
 scope_device_id BINARY(32) NULL, revision BIGINT UNSIGNED NOT NULL,
 KEY ix_grants_user_id (user_id)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS admission_decisions (
 id BINARY(16) NOT NULL PRIMARY KEY, device_id BINARY(32) NOT NULL,
 actor_id BINARY(16) NULL, decision VARCHAR(16) NOT NULL,
 previous_revision BIGINT UNSIGNED NOT NULL, new_revision BIGINT UNSIGNED NOT NULL,
 reason TEXT NULL, time_evidence JSON NOT NULL, KEY ix_admission_decisions_device_id (device_id)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS audit_events (
 id BINARY(16) NOT NULL PRIMARY KEY, actor_kind VARCHAR(32) NOT NULL,
 actor_user_id BINARY(16) NULL, event_type VARCHAR(128) NOT NULL,
 target_kind VARCHAR(32) NULL, target_id VARCHAR(128) NULL,
 params_redacted JSON NOT NULL, outcome VARCHAR(32) NOT NULL, time_evidence JSON NOT NULL,
 process_epoch BINARY(16) NOT NULL, event_seq BIGINT UNSIGNED NOT NULL,
 UNIQUE KEY uq_audit_epoch_seq (process_epoch, event_seq),
 KEY ix_audit_actor (actor_user_id), KEY ix_audit_target (target_kind, target_id)
) CHARACTER SET utf8mb4;

-- --- runtime 五表（v4 运行时任务表，无外键、无 CHECK） ---
CREATE TABLE IF NOT EXISTS task_previews (
  token_nonce BINARY(32) NOT NULL PRIMARY KEY,
  actor_user_id BINARY(16) NOT NULL,
  -- target_hash：目标清单规范化升序后的 32 字节 SHA256 摘要（device_target_hash 计算值）；提交时直读该列写入 main_tasks.target_hash
  target_hash BINARY(32) NOT NULL,
  -- targets_json：固定目标快照：canonical 升序字节序 device_id 清单（提交时的唯一目标真相源）
  targets_json JSON NOT NULL,
  preview_token_hash BINARY(32) NOT NULL,
  -- preview_generation：创建该 preview 的进程启动 epoch；跨代际一律失效，并参与 token 摘要输入
  preview_generation BINARY(16) NOT NULL,
  created_at DATETIME(6) NOT NULL,
  expires_at DATETIME(6) NOT NULL,
  consumed_at DATETIME(6) NULL,
  KEY ix_task_previews_actor (actor_user_id),
  UNIQUE KEY uq_task_previews_token_hash (preview_token_hash),
  KEY ix_task_previews_expires (expires_at)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS main_tasks (
  id BINARY(16) NOT NULL PRIMARY KEY,
  actor_user_id BINARY(16) NOT NULL,
  idempotency_key VARCHAR(64) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  request_hash BINARY(32) NOT NULL,
  target_hash BINARY(32) NOT NULL,
  preview_token_hash BINARY(32) NOT NULL,
  state VARCHAR(16) NOT NULL,
  outcome VARCHAR(16) NULL,
  revision BIGINT UNSIGNED NOT NULL,
  counts_json JSON NOT NULL,
  created_time_json JSON NOT NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  UNIQUE KEY uq_main_tasks_actor_idem (actor_user_id, idempotency_key),
  KEY ix_main_tasks_actor_created (actor_user_id, created_at),
  KEY ix_main_tasks_state (state)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS sub_tasks (
  id BINARY(16) NOT NULL PRIMARY KEY,
  main_task_id BINARY(16) NOT NULL,
  device_id BINARY(32) NOT NULL,
  state VARCHAR(16) NOT NULL,
  reason_code VARCHAR(32) NULL,
  revision BIGINT UNSIGNED NOT NULL,
  lock_generation BIGINT UNSIGNED NOT NULL DEFAULT 0,
  pre_boot_id VARCHAR(128) NULL,
  controller_epoch BINARY(16) NULL,
  session_epoch BINARY(16) NULL,
  stream_epoch BINARY(16) NULL,
  ticket_digest BINARY(32) NULL,
  request_digest BINARY(32) NULL,
  queue_deadline DATETIME(6) NOT NULL,
  observe_deadline DATETIME(6) NULL,
  evidence_json JSON NULL,
  created_at DATETIME(6) NOT NULL,
  updated_at DATETIME(6) NOT NULL,
  UNIQUE KEY uq_sub_tasks_main_device (main_task_id, device_id),
  KEY ix_sub_tasks_main (main_task_id),
  KEY ix_sub_tasks_device_state (device_id, state)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS device_operation_locks (
  device_id BINARY(32) NOT NULL PRIMARY KEY,
  main_task_id BINARY(16) NOT NULL,
  owner_sub_task_id BINARY(16) NOT NULL,
  generation BIGINT UNSIGNED NOT NULL,
  acquired_at DATETIME(6) NOT NULL,
  released_at DATETIME(6) NULL,
  UNIQUE KEY uq_device_locks_owner (owner_sub_task_id),
  KEY ix_device_locks_main (main_task_id)
) CHARACTER SET utf8mb4;
CREATE TABLE IF NOT EXISTS recovery_checks (
  id BINARY(16) NOT NULL PRIMARY KEY,
  process_epoch BINARY(16) NOT NULL,
  subtask_id BINARY(16) NOT NULL,
  outcome VARCHAR(32) NOT NULL,
  detail_redacted JSON NOT NULL,
  checked_at DATETIME(6) NOT NULL,
  KEY ix_recovery_checks_subtask (subtask_id),
  UNIQUE KEY uq_recovery_checks_epoch_subtask (process_epoch, subtask_id)
) CHARACTER SET utf8mb4;

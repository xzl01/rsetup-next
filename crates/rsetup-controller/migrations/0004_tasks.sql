-- Schema v4 运行时任务表（0004）：五张表、无外键、无 CHECK 约束。
-- 仅由显式迁移入口（操作员/测试）执行，普通启动路径绝不隐式建表。
-- 约定：时间列一律 DATETIME(6)；revision/epoch 计数一律 BIGINT UNSIGNED；
-- 摘要与 epoch 用 BINARY；跨表引用由应用层与只读扫描校验，不建 FK。
CREATE TABLE IF NOT EXISTS task_previews (
  token_nonce BINARY(32) NOT NULL PRIMARY KEY,
  actor_user_id BINARY(16) NOT NULL,
  target_hash BINARY(32) NOT NULL,        -- 目标清单规范化升序后的 32 字节 SHA256 摘要（device_target_hash 计算值）；提交时直读该列写入 main_tasks.target_hash
  targets_json JSON NOT NULL,             -- 固定目标快照：canonical 升序字节序 device_id 清单（提交时的唯一目标真相源）
  preview_token_hash BINARY(32) NOT NULL,
  preview_generation BINARY(16) NOT NULL, -- 创建该 preview 的进程启动 epoch；跨代际一律失效，并参与 token 摘要输入
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

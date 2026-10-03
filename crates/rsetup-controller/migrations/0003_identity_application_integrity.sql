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

//! 任务 HTTP 域测试共享 Fixture。
//!
//! 跨 projection / gate / routes / events 共享的会话与实体构造函数及常量。
//! 批次 3 首建：仅包含 `RAW_TOKEN` 与 `fixture_session` 两项（三段时序第一段）。

use crate::auth::service::{IdentityUser, Session};
use crate::auth::session::token_digest;

pub(super) const RAW_TOKEN: &str =
    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

pub(super) fn fixture_session(is_admin: bool, must_change_password: bool) -> Session {
    Session {
        user: IdentityUser {
            id: [7u8; 16],
            username: if is_admin {
                "admin".into()
            } else {
                "alice".into()
            },
            password_hash: "$argon2id$=synthetic-test-only=".to_string(),
            active: true,
            is_admin,
            must_change_password,
            revision: 1,
        },
        digest: token_digest(RAW_TOKEN.as_bytes()),
    }
}

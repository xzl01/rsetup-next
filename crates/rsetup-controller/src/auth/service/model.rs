//! 认证与会话数据模型。

#[derive(Clone, Debug)]
pub struct IdentityUser {
    pub id: [u8; 16],
    pub username: String,
    pub password_hash: String,
    pub active: bool,
    pub is_admin: bool,
    pub must_change_password: bool,
    pub revision: u64,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub user: IdentityUser,
    pub digest: [u8; 32],
}

pub struct Login {
    pub raw_token: String,
    pub session: Session,
}

#[derive(Clone, Debug)]
pub struct StoredSession {
    pub digest: [u8; 32],
    pub owner_id: [u8; 16],
    pub process_epoch: [u8; 16],
    pub created_time: chrono::DateTime<chrono::Utc>,
    pub revoked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionItem {
    pub id: String,
    pub current: bool,
    pub created_time: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPage {
    pub items: Vec<SessionItem>,
    pub next_cursor: Option<String>,
}

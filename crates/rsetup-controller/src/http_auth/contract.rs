use std::future::Future;
use std::pin::Pin;

use crate::auth::service::{Login, Session, SessionPage};
use crate::error::ControllerError;

/// Bounded boxed future so `AuthServiceApi` stays object-safe.
pub type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 长连接（SSE）逐帧复核所需的**权威**会话快照，见
/// [`AuthServiceApi::session_authority`]。
///
/// 三个字段都是「此刻权威侧怎么说」，不是握手期的缓存值：
/// - `session_live`：`false` 表示会话在权威身份库里已不存在/已撤销/所属用户
///   已停用（或本进程 epoch 已换代），连接必须立即关闭；
/// - `is_admin`：权威库里该用户**当前**的管理员标志。握手后被降权的管理员在
///   这里暴露，长连接不得继续享用握手期的旧权限（06 §4「撤权关闭旧订阅」）；
/// - `authz_epoch`：当前授权 epoch（`schema_meta` 计数，与 `me` 同源）。与
///   握手期基准值不等即表示授权投影已变更，旧连接的可见范围不再可信。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionAuthority {
    pub session_live: bool,
    pub is_admin: bool,
    pub authz_epoch: u64,
}

/// Object-safe auth boundary consumed by the HTTP layer. The production
/// adapter (Task 3b) wraps `AuthService<R: IdentityRepository>`; `authz_epoch`
/// is the per-request `schema_meta` count, never the process epoch.
pub trait AuthServiceApi: Send + Sync {
    fn login<'a>(&'a self, username: &'a str, password: &'a str) -> AuthFuture<'a, Login>;
    fn authenticate<'a>(&'a self, raw: &'a str) -> AuthFuture<'a, Session>;
    fn change_password<'a>(
        &'a self,
        session: &'a Session,
        current: &'a str,
        new_password: &'a str,
    ) -> AuthFuture<'a, ()>;
    fn logout<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, ()>;
    fn authz_epoch(&self) -> AuthFuture<'_, u64>;
    /// Synchronous, pure in-memory precheck (Task 3f): does `digest` still
    /// exist and is live in the current process session clock?
    ///
    /// No async I/O, no database access, and no idle renewal. A `true`
    /// result is only a necessary condition — the caller must still run the
    /// DB-authoritative `authenticate` and every downstream gate. This
    /// method is REQUIRED (no default): an always-true default would
    /// silently mask a missing gate behind the object-safe boundary.
    fn is_live_session(&self, digest: &[u8; 32]) -> bool;
    /// Authoritative session/authority snapshot for LONG-LIVED streams,
    /// keyed by the session digest (a stream never retains the raw token).
    /// Unlike [`AuthServiceApi::is_live_session`], this MUST reach the
    /// authoritative store: session revoked, owner deactivated, process epoch
    /// replaced, admin flag changed and the current `authz_epoch` all have to
    /// be observable here.
    ///
    /// Contract (06 §4「发送前验证 session 与 authz_epoch，撤权关闭旧订阅，
    /// 排队旧权限数据不得发送」):
    /// - an `Err` (backend/driver/decode failure) is NOT "still valid" — the
    ///   caller must fail closed and close the stream;
    /// - `session_live == false` must close the stream;
    /// - the caller keeps a handshake baseline of `authz_epoch`/`is_admin`
    ///   and closes the stream on any divergence: a stream never keeps
    ///   serving with a permission it was granted earlier;
    /// - this method is REQUIRED (no default): a permissive default would
    ///   silently freeze authorization for the whole lifetime of a connection.
    fn session_authority<'a>(&'a self, digest: &'a [u8; 32]) -> AuthFuture<'a, SessionAuthority>;
    fn list_sessions<'a>(
        &'a self,
        session: &'a Session,
        cursor: Option<&'a str>,
        limit: usize,
    ) -> AuthFuture<'a, SessionPage>;
    fn revoke_by_alias<'a>(&'a self, session: &'a Session, id: &'a str) -> AuthFuture<'a, bool>;
    fn revoke_others<'a>(&'a self, session: &'a Session) -> AuthFuture<'a, u64>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::db::DbPool;

    fn lazy_db() -> DbPool {
        DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(std::time::Duration::from_millis(100))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .unwrap(),
        )
    }

    // 11. 对象安全编译证明
    #[tokio::test]
    async fn test_group_11_trait_object_safety_compilation_proof() {
        // Must compile and coerce to Arc<dyn AuthServiceApi>
        let db = lazy_db();
        let repo = Arc::new(crate::auth::sqlx_repo::SqlxIdentityRepository::new(
            db.clone(),
        ));
        let service = crate::auth::service::AuthService::new(repo).unwrap();
        let live = crate::http_live::LiveAuth::new(Arc::new(service), db);
        let api: Arc<dyn AuthServiceApi> = Arc::new(live);
        assert!(!api.is_live_session(&[0x7c; 32]));
    }
}

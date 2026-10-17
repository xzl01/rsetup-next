//! 任务域：模型、预览纯逻辑、调度准入与只读恢复纯逻辑、仓储边界、编排服务与 HTTP 端点。

pub mod adapter;
pub mod http;
pub mod model;
pub mod preview;
pub mod recovery;
pub mod scheduler;

/// 位置绑定宏：宏实参顺序 = SQL `?` 顺序，逐位调用 `sqlx::Arguments::add`。
///
/// 不是任何 sqlx 宏的薄包装（sqlx 0.8.6 未导出 `args!`）。每个 `add` 返回的
/// `BoxDynError` 一律映射成 `ControllerError::Database(sqlx::Error::Encode(..))`
/// 后用 `?` 上抛：不 `unwrap`、不吞错。宏内错误类型固定为 `ControllerError`，
/// 因此在返回 `Result<_, SubmitError>` 等编排函数体内展开时，`?` 由各自的
/// `From<ControllerError>` 闭合。取 `&$e` 而非移动实参，是为了让
/// `req.idempotency_key: String` 这类非 `Copy` 字段仍可作位置参数。
macro_rules! params {
    ($($e:expr),* $(,)?) => {{
        let mut args = sqlx::mysql::MySqlArguments::default();
        $(
            // 位置绑定顺序 = 宏实参顺序 = SQL `?` 顺序；编码失败立即上抛
            sqlx::Arguments::add(&mut args, &$e)
                .map_err(|err| crate::ControllerError::Database(sqlx::Error::Encode(err)))?;
        )*
        args
    }};
}

pub mod repository;
pub mod service;

pub use adapter::SqlxTaskApi;
pub use http::build_tasks_router;

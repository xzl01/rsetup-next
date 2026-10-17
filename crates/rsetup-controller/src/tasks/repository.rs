//! 任务仓储边界：领域行、checked 解码与唯一生产实现 [`SqlxTaskRepository`]。
//!
//! 分层口径（B1 实体行解耦）：领域行（[`MainTaskRow`] / [`SubTaskRow`]）绝不进入
//! sqlx 解码路径，MySQL 原始行也不出仓储边界——原始列一律先以 `Vec<u8>` / `String`
//! 承接，再经 checked 转换；长度或枚举值不符视为 DB 损坏，统一映射
//! [`ControllerError::Database`]（HTTP 503），不 truncate、不 unwrap、不返回 400。
//!
//! 本 crate 的 `sqlx` 依赖未启用 `derive` 特性（workspace 根为
//! `default-features = false`，且本任务禁止改动 `Cargo.toml`），因此不使用
//! `#[derive(FromRow)]`：原始行读取统一走 [`tx_rows`] / [`tx_row`] + 显式 `try_get`。

use chrono::{DateTime, Utc};
use sqlx::Row as _;
use sqlx::Transaction;
use sqlx::mysql::{MySql, MySqlArguments, MySqlPool, MySqlRow};
use uuid::Uuid;

use crate::ControllerError;
use crate::tasks::model::{MainTaskOutcome, MainTaskState, ReasonCode, SubTaskState};
use crate::tasks::preview::{
    PreviewTarget, canonical_device_order, device_target_hash, encode_preview_targets,
    preview_generation_bytes, register_preview_mono_deadline,
};
use crate::tasks::service::{
    AuthCtx, CancelOutcome, CancelRequest, ReleaseLockError, ReleaseLockOutcome, ReleaseRequest,
    SubmitError, SubmitOutcome, SubmitRequest, TaskService, TimeReviewError, TimeReviewOutcome,
    TimeReviewRequest,
};
use crate::time::evidence::TimeEvidence;

/// 枚举列的字符串真相源即 Task 2 的 serde 定义（snake_case / SCREAMING_SNAKE_CASE）。
/// 非法值返回 `None`，由调用方统一映射 DB 损坏（503），绝不猜测近似值。
impl MainTaskState {
    pub fn from_str_exact(raw: &str) -> Option<Self> {
        serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok()
    }
}

impl MainTaskOutcome {
    pub fn from_str_exact(raw: &str) -> Option<Self> {
        serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok()
    }
}

impl SubTaskState {
    pub fn from_str_exact(raw: &str) -> Option<Self> {
        serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok()
    }
}

impl ReasonCode {
    pub fn from_str_exact(raw: &str) -> Option<Self> {
        serde_json::from_value::<Self>(serde_json::Value::String(raw.to_owned())).ok()
    }
}

/// 枚举 → 持久化串（同一 serde 真相源；非字符串表示即配置错误，绝不静默写空串）。
pub(crate) fn enum_wire<T: serde::Serialize>(value: &T) -> Result<String, ControllerError> {
    match serde_json::to_value(value)
        .map_err(|_| ControllerError::Config("task enum is not a json string".into()))?
    {
        serde_json::Value::String(text) => Ok(text),
        _ => Err(ControllerError::Config(
            "task enum is not a json string".into(),
        )),
    }
}

/// 去敏的 DB 解码错误（503）：消息不含任何实际列值，与 `fixed_mysql_bytes` 同口径。
pub(crate) fn task_row_decode_error() -> ControllerError {
    ControllerError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "task row column has unexpected value",
    ))))
}

/// MySQL 固定宽列（`BINARY(16)` / `BINARY(32)`）读出后的长度校验：长度不符即 DB 损坏，
/// 返回 `ControllerError::Database`（503），不 truncate、不 panic、不返回 400。
///
/// sqlx-mysql 0.8.6 未对 `[u8; N]` 实现 `Type`/`Decode`，固定宽列一律以 `Vec<u8>` 承接再校验。
pub(crate) fn fixed_mysql_bytes<const N: usize>(raw: &[u8]) -> Result<[u8; N], ControllerError> {
    raw.try_into().map_err(|_| {
        ControllerError::Database(sqlx::Error::Decode(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "fixed-width binary column has unexpected length",
        ))))
    })
}

/// `BINARY(16)` 列 → `Uuid`：长度不符 → DB 损坏（503）。
pub(crate) fn mysql_uuid(raw: &[u8]) -> Result<Uuid, ControllerError> {
    Ok(Uuid::from_bytes(fixed_mysql_bytes::<16>(raw)?))
}

/// 读出列的 checked `try_get`：解码失败一律映射去敏 DB 损坏（503）。
pub(crate) fn column<T>(row: &MySqlRow, name: &str) -> Result<T, ControllerError>
where
    T: for<'r> sqlx::Decode<'r, MySql> + sqlx::Type<MySql>,
{
    row.try_get(name).map_err(|_| task_row_decode_error())
}

/// 主任务领域行（不含期限列；期限由恢复/调度路径单独读取）。
///
/// `created_time_json` 是 `TaskDetail.created_time`（创建时时钟证据）的唯一来源；
/// `created_at` 是 `(created_at, id)` keyset 列表游标的排序键，生产读路径必需。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainTaskRow {
    pub id: Uuid,
    pub actor_user_id: Uuid,
    pub state: MainTaskState,
    pub outcome: Option<MainTaskOutcome>,
    pub revision: u64,
    pub counts_json: String,
    pub created_time_json: String,
    pub created_at: DateTime<Utc>,
}

/// 子任务领域行（列表投影；两条期限列都不进列表行：`queue_deadline` 是排队截止、
/// 只由时限复核/调度路径读取，`observe_deadline` 是派发后的观察窗截止，
/// 两者语义独立，都不属于列表展示字段）。
///
/// `evidence_json` 是 children 页 `result_evidence` 的原始 JSON 文本（当前无生产
/// 者写入、恒 NULL；读取侧防御性解码）；`created_at` 供 children 签名游标的
/// `created_at_ms` payload 使用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubTaskRow {
    pub id: Uuid,
    pub device_id: [u8; 32],
    pub state: SubTaskState,
    pub reason_code: Option<ReasonCode>,
    pub revision: u64,
    pub evidence_json: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// `main_tasks` 原始行：`BINARY` 列以 `Vec<u8>` 承接、枚举列以 `String` 承接。
#[derive(Clone, Debug)]
pub(crate) struct MainTaskRawRow {
    pub id: Vec<u8>,
    pub actor_user_id: Vec<u8>,
    pub state: String,
    pub outcome: Option<String>,
    pub revision: u64,
    pub counts_json: String,
    pub created_time_json: String,
    pub created_at: DateTime<Utc>,
}

/// `sub_tasks` 原始行。
#[derive(Clone, Debug)]
pub(crate) struct SubTaskRawRow {
    pub id: Vec<u8>,
    pub device_id: Vec<u8>,
    pub state: String,
    pub reason_code: Option<String>,
    pub revision: u64,
    pub evidence_json: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl MainTaskRawRow {
    /// 从 `main_tasks` 投影读出行做 checked 转换；任一分量损坏即整行失败。
    pub(crate) fn from_row(row: &MySqlRow) -> Result<Self, ControllerError> {
        Ok(Self {
            id: column::<Vec<u8>>(row, "id")?,
            actor_user_id: column::<Vec<u8>>(row, "actor_user_id")?,
            state: column::<String>(row, "state")?,
            outcome: column::<Option<String>>(row, "outcome")?,
            revision: column::<u64>(row, "revision")?,
            counts_json: column::<String>(row, "counts_json")?,
            created_time_json: column::<String>(row, "created_time_json")?,
            created_at: column::<DateTime<Utc>>(row, "created_at")?,
        })
    }
}

impl SubTaskRawRow {
    pub(crate) fn from_row(row: &MySqlRow) -> Result<Self, ControllerError> {
        Ok(Self {
            id: column::<Vec<u8>>(row, "id")?,
            device_id: column::<Vec<u8>>(row, "device_id")?,
            state: column::<String>(row, "state")?,
            reason_code: column::<Option<String>>(row, "reason_code")?,
            revision: column::<u64>(row, "revision")?,
            evidence_json: column::<Option<String>>(row, "evidence_json")?,
            created_at: column::<DateTime<Utc>>(row, "created_at")?,
        })
    }
}

impl TryFrom<MainTaskRawRow> for MainTaskRow {
    type Error = ControllerError;

    fn try_from(raw: MainTaskRawRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: mysql_uuid(&raw.id)?,
            actor_user_id: mysql_uuid(&raw.actor_user_id)?,
            state: MainTaskState::from_str_exact(&raw.state).ok_or_else(task_row_decode_error)?,
            outcome: match &raw.outcome {
                None => None,
                Some(raw) => {
                    Some(MainTaskOutcome::from_str_exact(raw).ok_or_else(task_row_decode_error)?)
                }
            },
            revision: raw.revision,
            counts_json: raw.counts_json,
            created_time_json: raw.created_time_json,
            created_at: raw.created_at,
        })
    }
}

impl TryFrom<SubTaskRawRow> for SubTaskRow {
    type Error = ControllerError;

    fn try_from(raw: SubTaskRawRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: mysql_uuid(&raw.id)?,
            device_id: fixed_mysql_bytes::<32>(&raw.device_id)?,
            state: SubTaskState::from_str_exact(&raw.state).ok_or_else(task_row_decode_error)?,
            reason_code: match &raw.reason_code {
                None => None,
                Some(raw) => {
                    Some(ReasonCode::from_str_exact(raw).ok_or_else(task_row_decode_error)?)
                }
            },
            revision: raw.revision,
            evidence_json: raw.evidence_json,
            created_at: raw.created_at,
        })
    }
}

/// 短事务内执行语句，返回受影响行数；任何 SQL 错误原样映射 `Database`（503）。
pub(crate) async fn tx_execute(
    tx: &mut Transaction<'_, MySql>,
    sql: &str,
    args: MySqlArguments,
) -> Result<u64, ControllerError> {
    let result = sqlx::query_with::<MySql, _>(sql, args)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}

/// 短事务内批量读原始行（调用方负责 checked 转换；不在此处解码业务语义）。
pub(crate) async fn tx_rows(
    tx: &mut Transaction<'_, MySql>,
    sql: &str,
    args: MySqlArguments,
) -> Result<Vec<MySqlRow>, ControllerError> {
    Ok(sqlx::query_with::<MySql, _>(sql, args)
        .fetch_all(&mut **tx)
        .await?)
}

/// 短事务内读单行；零行返回 `Ok(None)`，多行由调用方判定为损坏。
pub(crate) async fn tx_row(
    tx: &mut Transaction<'_, MySql>,
    sql: &str,
    args: MySqlArguments,
) -> Result<Option<MySqlRow>, ControllerError> {
    Ok(sqlx::query_with::<MySql, _>(sql, args)
        .fetch_optional(&mut **tx)
        .await?)
}

/// `main_tasks` 列表/详情投影的统一列集合（JSON 列经 `CAST(... AS CHAR)` 取文本）。
/// `created_time_json` 供详情 `created_time`；`created_at` 是列表 keyset 排序键。
const MAIN_TASK_COLUMNS: &str = "id, actor_user_id, state, outcome, revision, \
                                 CAST(counts_json AS CHAR) AS counts_json, \
                                 CAST(created_time_json AS CHAR) AS created_time_json, \
                                 created_at";

/// `sub_tasks` 列表投影的统一列集合（不含期限列）。`evidence_json` 供 children 页
/// `result_evidence`（当前恒 NULL，读取侧防御性解码）；`created_at` 供 children
/// 签名游标 payload。
const SUB_TASK_COLUMNS: &str = "id, device_id, state, reason_code, revision, \
                                CAST(evidence_json AS CHAR) AS evidence_json, created_at";

/// 主任务 keyset 的毫秒时间轴唯一表达式（游标谓词与 `ORDER BY` 共用，二者必须
/// 同轴，否则跨页顺序漂移）。
///
/// 根因（H1）：`main_tasks.created_at` 是 `DATETIME(6)`（`NOW(6)` 写入，带微秒），
/// 而游标 payload 只携带毫秒（`timestamp_millis()` 向下截断）。若谓词与排序按微秒
/// 时间轴进行，上一页最后一毫秒内的行在下一页会被重新命中（`limit=1` 时游标完全
/// 重复、永不前进）。统一到本表达式后，SQL 与游标落在同一毫秒时间轴上：同毫秒行
/// 的先后完全由 `id` 裁决，不重复、不丢行、不死循环。
///
/// 必须是 `FLOOR`（向下截断）而非 `ROUND`：与游标编码的截断语义逐位一致——四舍五入
/// 会把微秒 ≥0.5ms 的行归入下一毫秒桶，游标行仍会被下一页重新命中，死循环复现。
/// 口径前提：会话时区为 UTC（与本 crate `NOW(6)` 写入、DATETIME 按 UTC 解码的既有
/// 全库口径同一前提）。
const MAIN_TASK_KEYSET_MILLIS_EXPR: &str = "FLOOR(UNIX_TIMESTAMP(created_at) * 1000)";

/// 单行的毫秒 keyset 键：与游标编码 `timestamp_millis()` 同口径向下截断（chrono
/// 的 `timestamp()` 向下取整 + 非负小数毫秒分量，合成即毫秒向下取整）；越界
/// （不可表示为 i64 毫秒）返回 `None`，由调用方映射 `InvalidArgument`，绝不钳位。
fn keyset_millis(created_at: DateTime<Utc>) -> Option<i64> {
    created_at
        .timestamp()
        .checked_mul(1000)?
        .checked_add(i64::from(created_at.timestamp_subsec_millis()))
}

/// 游标谓词的 Rust 侧镜像（与 [`build_list_main_tasks_sql`] 的 SQL 条件逐字同
/// 口径，供无 DB 的纯分页回归）：行 `(row_ms, row_id)` 是否严格位于游标之后。
/// 生产谓词只存在于 SQL 中，本镜像仅测试期存在。
#[cfg(test)]
fn keyset_after(row_ms: i64, row_id: &Uuid, cursor_ms: i64, cursor_id: &Uuid) -> bool {
    row_ms > cursor_ms || (row_ms == cursor_ms && row_id > cursor_id)
}

/// 组装主任务 keyset 列表 SQL（纯函数，供单测锁定形状）：WHERE 条件按参数存在性
/// 动态拼装；`ORDER BY` 恒与游标谓词同轴（毫秒时间轴 + `id` 决胜），首页与后续页
/// 共用同一全序，跨页不漂移。
fn build_list_main_tasks_sql(has_actor: bool, has_state: bool, has_after: bool) -> String {
    let mut conditions: Vec<String> = Vec::new();
    if has_actor {
        conditions.push("actor_user_id = ?".to_owned());
    }
    if has_state {
        conditions.push("state = ?".to_owned());
    }
    if has_after {
        conditions.push(format!(
            "({MAIN_TASK_KEYSET_MILLIS_EXPR} > ? OR ({MAIN_TASK_KEYSET_MILLIS_EXPR} = ? AND id > ?))"
        ));
    }
    let where_clause = if conditions.is_empty() {
        "1 = 1".to_owned()
    } else {
        conditions.join(" AND ")
    };
    format!(
        "SELECT {MAIN_TASK_COLUMNS} FROM main_tasks WHERE {where_clause} \
         ORDER BY {MAIN_TASK_KEYSET_MILLIS_EXPR} ASC, id ASC LIMIT ?"
    )
}

/// 解析 keyset 游标：恰 32 位小写 hex（对应 `BINARY(16)` 主键）。大写、长度不符、
/// 非 hex 一律 `None`，由调用方映射 `InvalidArgument`，绝不退化为「返回首页」。
fn parse_id_cursor(raw: &str) -> Option<Vec<u8>> {
    if raw.len() != 32
        || !raw
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    hex::decode(raw).ok().filter(|bytes| bytes.len() == 16)
}

/// 任务仓储：`MySqlPool` 只存在于实现内部，`begin()` 是外部唯一的事务入口。
///
/// 编排型方法（提交/取消/时限复核/解除占用）与 [`TaskService`] 共用同一实现，
/// 单一真相源；[`SqlxTaskRepository`] 是唯一生产实现。
pub trait TaskRepository: Send + Sync {
    /// 唯一事务入口：短事务，事务内零网络。
    fn begin(
        &self,
    ) -> impl std::future::Future<Output = Result<Transaction<'static, MySql>, ControllerError>> + Send;

    /// 落盘一枚预览快照；**INSERT 提交成功后**才登记进程内单调期限。
    fn create_preview(
        &self,
        token_nonce: [u8; 32],
        actor_user_id: Uuid,
        targets: &[PreviewTarget],
        token_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send;

    /// 原子消费预览并创建主/子任务（幂等重放优先，见 [`TaskService::submit`]）。
    fn consume_preview_and_create_tasks(
        &self,
        req: &SubmitRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<SubmitOutcome, ControllerError>> + Send;

    fn find_main_task(
        &self,
        main_task_id: Uuid,
    ) -> impl std::future::Future<Output = Result<Option<MainTaskRow>, ControllerError>> + Send;

    /// 按 `id` 字节序 keyset 分页；游标是上一页末行 `id` 的小写 hex，`None` 表示首页。
    fn list_subtasks_page(
        &self,
        main_task_id: Uuid,
        cursor: Option<String>,
        limit: usize,
    ) -> impl std::future::Future<
        Output = Result<(Vec<SubTaskRow>, Option<String>), ControllerError>,
    > + Send;

    fn cancel_subtask(
        &self,
        req: &CancelRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<CancelOutcome, ControllerError>> + Send;

    /// `clock` 必须是服务端时钟源（TimeGate）产出的快照，绝不来自请求体。
    /// 分派依据只有服务端真实比对：`sub_tasks.queue_deadline`（排队截止，不是
    /// `observe_deadline` 观察窗）对「采样时刻 + 样本年龄」，并要求主任务
    /// `created_time_json` 的时钟纪元与当前纪元连续，否则一律 `Inconclusive`。
    fn time_review(
        &self,
        req: &TimeReviewRequest,
        clock: &TimeEvidence,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<TimeReviewOutcome, TimeReviewError>> + Send;

    fn release_lock(
        &self,
        req: &ReleaseRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<ReleaseLockOutcome, ReleaseLockError>> + Send;
}

/// 唯一生产实现：内部持有连接池，事务只经 `begin()` 下发。
#[derive(Clone, Debug)]
pub struct SqlxTaskRepository {
    pool: MySqlPool,
}

impl SqlxTaskRepository {
    pub fn new(pool: MySqlPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &MySqlPool {
        &self.pool
    }

    /// 主任务 keyset 扫描一页（生产列表读路径）。
    ///
    /// - `actor_filter`：`Some(user)` 只扫该用户发起的主任务（普通用户），`None` 全域
    ///   扫描（会话侧 admin；权威 admin 判定仍由调用方的授权快照事务内重验兜底）；
    /// - `after`：上一页末候选的 `(created_at, id)`；比较与排序统一在**毫秒时间轴**
    ///   上进行（[`MAIN_TASK_KEYSET_MILLIS_EXPR`]，与游标 payload 的毫秒截断同轴），
    ///   谓词为 `毫秒(created_at) > ? OR (毫秒(created_at) = ? AND id > ?)`，
    ///   同毫秒行完全由 `id` 决胜——不重复、不丢行、`limit=1` 也正常推进（H1）；
    /// - `next`：**最后一条已扫描候选**的 keyset 键（扫描满 `limit` 行才给出，
    ///   不足即 `None`）。可见性过滤发生在扫描之后，故下一页游标以「已扫描候选」
    ///   为准：不重扫、不死循环，也不泄漏过滤前总数。
    pub async fn list_main_tasks_page(
        &self,
        actor_filter: Option<Uuid>,
        state_filter: Option<MainTaskState>,
        after: Option<(DateTime<Utc>, Uuid)>,
        limit: usize,
    ) -> Result<MainTaskScanPage, ControllerError> {
        let sql = build_list_main_tasks_sql(
            actor_filter.is_some(),
            state_filter.is_some(),
            after.is_some(),
        );
        let mut args = MySqlArguments::default();
        if let Some(actor) = &actor_filter {
            push_arg(&mut args, &actor.as_bytes().as_slice())?;
        }
        if let Some(state) = &state_filter {
            push_arg(&mut args, &enum_wire(state)?)?;
        }
        if let Some((created_at, id)) = after {
            // 游标按毫秒时间轴绑定（与 SQL 表达式同轴）；越界即请求错误，不钳位。
            let cursor_ms = keyset_millis(created_at).ok_or(ControllerError::InvalidArgument)?;
            push_arg(&mut args, &cursor_ms)?;
            push_arg(&mut args, &cursor_ms)?;
            push_arg(&mut args, &id.as_bytes().as_slice())?;
        }
        let limit = i64::try_from(limit).map_err(|_| ControllerError::InvalidArgument)?;
        push_arg(&mut args, &limit)?;
        let mut tx = self.pool.begin().await?;
        let rows = tx_rows(&mut tx, &sql, args).await?;
        // fail-closed：任一行损坏即整页失败，不跳行、不截断
        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            items.push(MainTaskRow::try_from(MainTaskRawRow::from_row(row)?)?);
        }
        tx.commit().await?;
        let next = if limit > 0 && items.len() as i64 == limit {
            items.last().map(|row| (row.created_at, row.id))
        } else {
            None
        };
        Ok(MainTaskScanPage { rows: items, next })
    }

    /// 一次读事务批量读出多张主任务的全部子任务（列表可见性评估的唯一批量入口）。
    /// 返回按 `(main_task_id, id)` 升序的扁平列表；空输入零查询。
    pub async fn list_subtasks_for_main_tasks(
        &self,
        main_task_ids: &[Uuid],
    ) -> Result<Vec<(Uuid, SubTaskRow)>, ControllerError> {
        if main_task_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; main_task_ids.len()].join(", ");
        let sql = format!(
            "SELECT main_task_id, {SUB_TASK_COLUMNS} FROM sub_tasks \
             WHERE main_task_id IN ({placeholders}) ORDER BY main_task_id ASC, id ASC"
        );
        let mut args = MySqlArguments::default();
        for id in main_task_ids {
            push_arg(&mut args, &id.as_bytes().as_slice())?;
        }
        let mut tx = self.pool.begin().await?;
        let rows = tx_rows(&mut tx, &sql, args).await?;
        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            let main_task_id = mysql_uuid(&column::<Vec<u8>>(row, "main_task_id")?)?;
            items.push((
                main_task_id,
                SubTaskRow::try_from(SubTaskRawRow::from_row(row)?)?,
            ));
        }
        tx.commit().await?;
        Ok(items)
    }

    /// 同一读事务内读出主任务行与其全部子任务（详情/取消编排的唯一读取入口，
    /// 主/子行因此来自同一一致性快照）。主任务不存在返回 `Ok(None)`。
    pub async fn load_main_task_with_subtasks(
        &self,
        main_task_id: Uuid,
    ) -> Result<Option<(MainTaskRow, Vec<SubTaskRow>)>, ControllerError> {
        let mut tx = self.pool.begin().await?;
        let main = tx_row(
            &mut tx,
            &format!("SELECT {MAIN_TASK_COLUMNS} FROM main_tasks WHERE id = ?"),
            params![main_task_id.as_bytes().as_slice()],
        )
        .await?;
        let Some(main) = main else {
            // 主任务不存在：显式回滚退出，不占读事务
            tx.rollback().await?;
            return Ok(None);
        };
        let main = MainTaskRow::try_from(MainTaskRawRow::from_row(&main)?)?;
        let rows = tx_rows(
            &mut tx,
            &format!(
                "SELECT {SUB_TASK_COLUMNS} FROM sub_tasks WHERE main_task_id = ? \
                 ORDER BY id ASC"
            ),
            params![main_task_id.as_bytes().as_slice()],
        )
        .await?;
        let mut subtasks = Vec::with_capacity(rows.len());
        for row in &rows {
            subtasks.push(SubTaskRow::try_from(SubTaskRawRow::from_row(row)?)?);
        }
        tx.commit().await?;
        Ok(Some((main, subtasks)))
    }
}

/// 主任务 keyset 扫描一页的结果：候选行 + 下一页 keyset 键（见
/// [`SqlxTaskRepository::list_main_tasks_page`]）。
#[derive(Clone, Debug)]
pub struct MainTaskScanPage {
    pub rows: Vec<MainTaskRow>,
    pub next: Option<(DateTime<Utc>, Uuid)>,
}

/// 条件参数的逐个绑定：与 `params!` 宏完全同口径（编码失败 → `Database(Encode)`
/// 503），用于 WHERE 条件按参数存在性动态拼装的多形态 SQL。
fn push_arg<'q, T>(args: &mut MySqlArguments, value: &'q T) -> Result<(), ControllerError>
where
    T: sqlx::Type<MySql> + sqlx::Encode<'q, MySql>,
{
    sqlx::Arguments::add(args, value)
        .map_err(|err| ControllerError::Database(sqlx::Error::Encode(err)))
}

/// 方法签名刻意写成 `-> impl Future + Send`（而非 `async fn`）：与
/// `auth::service::IdentityRepository` 的仓储 trait 同风格，把「future 必须 Send」
/// 写进类型边界——`async fn` in trait 不保证这一点，调用方就无法把编排放进
/// `tokio::spawn`。因此这里豁免 `manual_async_fn` 的简化建议。
#[allow(clippy::manual_async_fn)]
impl TaskRepository for SqlxTaskRepository {
    fn begin(
        &self,
    ) -> impl std::future::Future<Output = Result<Transaction<'static, MySql>, ControllerError>> + Send
    {
        async move {
            // sqlx::Error 经 ? 转 ControllerError::Database，保留 503 存储语义
            let tx: Transaction<'static, MySql> = self.pool.begin().await?;
            Ok(tx)
        }
    }

    fn create_preview(
        &self,
        token_nonce: [u8; 32],
        actor_user_id: Uuid,
        targets: &[PreviewTarget],
        token_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send {
        async move {
            // ① 目标规范化：字节序升序 + 去重；eligible/reason_code 属预览期判定，不持久化
            let mut device_ids: Vec<[u8; 32]> =
                targets.iter().map(|target| target.device_id).collect();
            canonical_device_order(&mut device_ids);
            // ② target_hash：提交时直读该列写入 main_tasks.target_hash。
            //    PreviewTarget 不携带 group 维度，group_ids 恒空数组；提交侧若含 group，
            //    快照比较天然不一致而拒绝（fail-closed）。
            let target_hash = device_target_hash(&device_ids, &[]);
            // ③ targets_json：与 decode_preview_targets 逐字节同口径的 64 小写 hex 扁平数组
            let targets_json = encode_preview_targets(&device_ids);
            // ④ 进程启动 epoch；token_nonce 是调用方的服务端派生值，本方法不生成随机数
            let preview_generation = preview_generation_bytes();
            // ⑤ 单语句 INSERT（存储层自动提交）；consumed_at 显式 NULL，created_at 取 DB 时钟
            let args = params![
                &token_nonce[..],
                actor_user_id.as_bytes().as_slice(),
                &target_hash[..],
                &targets_json,
                &token_hash[..],
                &preview_generation[..],
                expires_at,
            ];
            let rows = sqlx::query_with::<MySql, _>(
                "INSERT INTO task_previews (token_nonce, actor_user_id, target_hash, targets_json, \
                 preview_token_hash, preview_generation, created_at, expires_at, consumed_at) \
                 VALUES (?, ?, ?, ?, ?, ?, NOW(6), ?, NULL)",
                args,
            )
            .execute(&self.pool)
            .await?;
            // MySqlQueryResult 未实现与整数的 PartialEq：受影响行数只能经 rows_affected() 判断
            if rows.rows_affected() != 1 {
                return Err(ControllerError::Database(sqlx::Error::RowNotFound));
            }
            // ⑥ 单调期限唯一注册点：提交成功之后、返回 preview token 之前。
            //    登记失败即放弃该预览（fail-closed，不做补偿删除：单调表无该 nonce，
            //    preview_mono_valid 恒 false，消费必被拒）。
            register_preview_mono_deadline(token_nonce)?;
            Ok(())
        }
    }

    fn consume_preview_and_create_tasks(
        &self,
        req: &SubmitRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<SubmitOutcome, ControllerError>> + Send {
        async move {
            let outcome: SubmitOutcome = TaskService::new(self)
                .submit(req, ctx)
                .await
                .map_err(SubmitError::into_controller_error)?;
            Ok(outcome)
        }
    }

    fn find_main_task(
        &self,
        main_task_id: Uuid,
    ) -> impl std::future::Future<Output = Result<Option<MainTaskRow>, ControllerError>> + Send
    {
        async move {
            let mut tx = self.pool.begin().await?;
            let row = tx_row(
                &mut tx,
                &format!("SELECT {MAIN_TASK_COLUMNS} FROM main_tasks WHERE id = ?"),
                params![main_task_id.as_bytes().as_slice()],
            )
            .await?;
            let Some(row) = row else {
                return Ok(None);
            };
            let task = MainTaskRow::try_from(MainTaskRawRow::from_row(&row)?)?;
            tx.commit().await?;
            Ok(Some(task))
        }
    }

    fn list_subtasks_page(
        &self,
        main_task_id: Uuid,
        cursor: Option<String>,
        limit: usize,
    ) -> impl std::future::Future<
        Output = Result<(Vec<SubTaskRow>, Option<String>), ControllerError>,
    > + Send {
        async move {
            // 游标必须是 32 位小写 hex（上一页末行 16 字节 id）；非法游标是请求错误，不静默当首页
            let after = match cursor.as_deref() {
                None => None,
                Some(raw) => Some(parse_id_cursor(raw).ok_or(ControllerError::InvalidArgument)?),
            };
            let mut tx = self.pool.begin().await?;
            let rows = match after.as_deref() {
                None => {
                    let sql = format!(
                        "SELECT {SUB_TASK_COLUMNS} FROM sub_tasks WHERE main_task_id = ? \
                         ORDER BY id LIMIT ?"
                    );
                    tx_rows(
                        &mut tx,
                        &sql,
                        params![main_task_id.as_bytes().as_slice(), limit as i64],
                    )
                    .await?
                }
                Some(after) => {
                    let sql = format!(
                        "SELECT {SUB_TASK_COLUMNS} FROM sub_tasks \
                         WHERE main_task_id = ? AND id > ? ORDER BY id LIMIT ?"
                    );
                    tx_rows(
                        &mut tx,
                        &sql,
                        params![main_task_id.as_bytes().as_slice(), after, limit as i64],
                    )
                    .await?
                }
            };
            // fail-closed：任一行损坏即整页失败，不跳行、不截断、不 panic
            let mut items = Vec::with_capacity(rows.len());
            for row in &rows {
                items.push(SubTaskRow::try_from(SubTaskRawRow::from_row(row)?)?);
            }
            let next_cursor = if items.len() == limit && limit > 0 {
                Some(hex::encode(items[items.len() - 1].id.as_bytes().as_slice()))
            } else {
                None
            };
            tx.commit().await?;
            Ok((items, next_cursor))
        }
    }

    fn cancel_subtask(
        &self,
        req: &CancelRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<CancelOutcome, ControllerError>> + Send {
        async move {
            let outcome = TaskService::new(self)
                .cancel(req, ctx)
                .await
                .map_err(ControllerError::from)?;
            Ok(outcome)
        }
    }

    fn time_review(
        &self,
        req: &TimeReviewRequest,
        clock: &TimeEvidence,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<TimeReviewOutcome, TimeReviewError>> + Send {
        async move { TaskService::new(self).time_review(req, clock, ctx).await }
    }

    fn release_lock(
        &self,
        req: &ReleaseRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<ReleaseLockOutcome, ReleaseLockError>> + Send
    {
        async move { TaskService::new(self).release_lock(req, ctx).await }
    }
}

/// 引用即仓储：让 `TaskService<&SqlxTaskRepository>` 也能经 `begin()` 取事务，
/// 编排实现因此只有一份（服务方法与仓储同名方法共用同一事务 helper 与 SQL）。
impl<R: TaskRepository + Sync> TaskRepository for &R {
    fn begin(
        &self,
    ) -> impl std::future::Future<Output = Result<Transaction<'static, MySql>, ControllerError>> + Send
    {
        (**self).begin()
    }

    fn create_preview(
        &self,
        token_nonce: [u8; 32],
        actor_user_id: Uuid,
        targets: &[PreviewTarget],
        token_hash: &[u8; 32],
        expires_at: DateTime<Utc>,
    ) -> impl std::future::Future<Output = Result<(), ControllerError>> + Send {
        (**self).create_preview(token_nonce, actor_user_id, targets, token_hash, expires_at)
    }

    fn consume_preview_and_create_tasks(
        &self,
        req: &SubmitRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<SubmitOutcome, ControllerError>> + Send {
        (**self).consume_preview_and_create_tasks(req, ctx)
    }

    fn find_main_task(
        &self,
        main_task_id: Uuid,
    ) -> impl std::future::Future<Output = Result<Option<MainTaskRow>, ControllerError>> + Send
    {
        (**self).find_main_task(main_task_id)
    }

    fn list_subtasks_page(
        &self,
        main_task_id: Uuid,
        cursor: Option<String>,
        limit: usize,
    ) -> impl std::future::Future<
        Output = Result<(Vec<SubTaskRow>, Option<String>), ControllerError>,
    > + Send {
        (**self).list_subtasks_page(main_task_id, cursor, limit)
    }

    fn cancel_subtask(
        &self,
        req: &CancelRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<CancelOutcome, ControllerError>> + Send {
        (**self).cancel_subtask(req, ctx)
    }

    fn time_review(
        &self,
        req: &TimeReviewRequest,
        clock: &TimeEvidence,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<TimeReviewOutcome, TimeReviewError>> + Send {
        (**self).time_review(req, clock, ctx)
    }

    fn release_lock(
        &self,
        req: &ReleaseRequest,
        ctx: &AuthCtx,
    ) -> impl std::future::Future<Output = Result<ReleaseLockOutcome, ReleaseLockError>> + Send
    {
        (**self).release_lock(req, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_created_at() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("fixed instant in range")
    }

    fn raw_main_task(outcome: Option<&str>, state: &str) -> MainTaskRawRow {
        MainTaskRawRow {
            id: [7u8; 16].to_vec(),
            actor_user_id: [8u8; 16].to_vec(),
            state: state.to_owned(),
            outcome: outcome.map(str::to_owned),
            revision: 3,
            counts_json: "{}".to_owned(),
            created_time_json: "{}".to_owned(),
            created_at: fixed_created_at(),
        }
    }

    #[test]
    fn main_task_row_maps_domain_values() {
        let row =
            MainTaskRow::try_from(raw_main_task(Some("partial"), "running")).expect("domain row");
        assert_eq!(row.id, Uuid::from_bytes([7u8; 16]));
        assert_eq!(row.state, MainTaskState::Running);
        assert_eq!(row.outcome, Some(MainTaskOutcome::Partial));
        assert_eq!(row.revision, 3);
        assert_eq!(row.created_time_json, "{}");
        assert_eq!(row.created_at, fixed_created_at());
    }

    #[test]
    fn main_task_row_rejects_unknown_enum_and_short_uuid() {
        let err = MainTaskRow::try_from(raw_main_task(None, "recycling")).unwrap_err();
        assert!(matches!(err, ControllerError::Database(_)));
        let mut broken = raw_main_task(None, "running");
        broken.id = vec![1u8, 2, 3];
        assert!(matches!(
            MainTaskRow::try_from(broken).unwrap_err(),
            ControllerError::Database(_)
        ));
    }

    #[test]
    fn sub_task_row_rejects_corrupt_reason_code_and_device_width() {
        let base = SubTaskRawRow {
            id: [1u8; 16].to_vec(),
            device_id: [9u8; 32].to_vec(),
            state: "queued".to_owned(),
            reason_code: Some("DEVICE_BUSY".to_owned()),
            revision: 1,
            evidence_json: None,
            created_at: fixed_created_at(),
        };
        let row = SubTaskRow::try_from(base.clone()).expect("domain row");
        assert_eq!(row.state, SubTaskState::Queued);
        assert_eq!(row.reason_code, Some(ReasonCode::DeviceBusy));
        assert_eq!(row.evidence_json, None);
        assert_eq!(row.created_at, fixed_created_at());

        let mut broken = base.clone();
        broken.reason_code = Some("not_a_reason".to_owned());
        assert!(matches!(
            SubTaskRow::try_from(broken).unwrap_err(),
            ControllerError::Database(_)
        ));

        let mut broken = base.clone();
        broken.device_id = vec![0u8; 31];
        assert!(matches!(
            SubTaskRow::try_from(broken).unwrap_err(),
            ControllerError::Database(_)
        ));
    }

    #[test]
    fn enum_wire_round_trips_through_from_str_exact() {
        assert_eq!(
            enum_wire(&SubTaskState::Dispatching).unwrap(),
            "dispatching"
        );
        assert_eq!(
            enum_wire(&ReasonCode::QueueExpired).unwrap(),
            "QUEUE_EXPIRED"
        );
        assert_eq!(
            SubTaskState::from_str_exact(&enum_wire(&SubTaskState::Verifying).unwrap()),
            Some(SubTaskState::Verifying)
        );
        // 大写与近似值一律拒绝，绝不模糊匹配
        assert_eq!(ReasonCode::from_str_exact("device_busy"), None);
        assert_eq!(MainTaskState::from_str_exact("RUNNING"), None);
    }

    #[test]
    fn id_cursor_must_be_lowercase_hex_of_a_binary16_id() {
        assert_eq!(parse_id_cursor(&"0a".repeat(16)).unwrap().len(), 16);
        assert_eq!(parse_id_cursor(&"0A".repeat(16)), None);
        assert_eq!(parse_id_cursor(&"0a".repeat(15)), None);
        assert_eq!(parse_id_cursor(&"0a".repeat(17)), None);
        assert_eq!(parse_id_cursor("zz".repeat(16).as_str()), None);
    }

    #[test]
    fn fixed_mysql_bytes_rejects_every_other_length() {
        assert_eq!(fixed_mysql_bytes::<16>(&[5u8; 16]).unwrap(), [5u8; 16]);
        assert!(matches!(
            fixed_mysql_bytes::<16>(&[5u8; 15]),
            Err(ControllerError::Database(_))
        ));
        assert!(matches!(
            fixed_mysql_bytes::<32>(&[5u8; 33]),
            Err(ControllerError::Database(_))
        ));
    }

    #[test]
    fn list_main_tasks_sql_aligns_keyset_to_millisecond_timeline() {
        let paged = build_list_main_tasks_sql(true, true, true);
        assert!(paged.contains("actor_user_id = ?"));
        assert!(paged.contains("state = ?"));
        // 游标谓词与 ORDER BY 共用同一毫秒时间轴表达式（共出现 3 次）：同毫秒行
        // 的先后完全由 id 裁决，游标（毫秒截断）与 SQL 比较落在同一时间轴，
        // 不重复、不丢行、limit=1 也正常推进。
        assert_eq!(paged.matches(MAIN_TASK_KEYSET_MILLIS_EXPR).count(), 3);
        assert!(paged.contains(&format!(
            "({MAIN_TASK_KEYSET_MILLIS_EXPR} > ? OR ({MAIN_TASK_KEYSET_MILLIS_EXPR} = ? AND id > ?))"
        )));
        assert!(paged.ends_with(&format!(
            "ORDER BY {MAIN_TASK_KEYSET_MILLIS_EXPR} ASC, id ASC LIMIT ?"
        )));
        // 首页（无游标）与后续页共用同一排序键，否则跨页顺序漂移。
        let first = build_list_main_tasks_sql(false, false, false);
        assert!(first.contains("WHERE 1 = 1"));
        assert!(first.ends_with(&format!(
            "ORDER BY {MAIN_TASK_KEYSET_MILLIS_EXPR} ASC, id ASC LIMIT ?"
        )));
    }

    #[test]
    fn keyset_millis_truncates_microseconds_to_millisecond_floor() {
        // 与游标编码 `timestamp_millis()` 同口径：向下截断（非四舍五入），
        // 微秒 .123999 必须归入 …123 毫秒桶，否则游标行会被下一页重新命中。
        let micro = DateTime::<Utc>::from_timestamp(1_800_000_000, 123_999_000).expect("in range");
        assert_eq!(keyset_millis(micro), Some(1_800_000_000_123));
        let exact = DateTime::<Utc>::from_timestamp(1_800_000_000, 0).expect("in range");
        assert_eq!(keyset_millis(exact), Some(1_800_000_000_000));
    }

    /// 模拟 `list_main_tasks_page` 的 keyset 分页：排序键 `(keyset_millis, id)`、
    /// 游标谓词 [`keyset_after`]、每页取 `limit` 行；页数超过行数+1 即游标不推进
    /// （死循环），直接 panic。
    fn simulate_keyset_pages(rows: &[(i64, Uuid)], limit: usize) -> Vec<(i64, Uuid)> {
        let mut sorted = rows.to_vec();
        sorted.sort();
        let mut cursor: Option<(i64, Uuid)> = None;
        let mut seen = Vec::new();
        for _ in 0..=sorted.len() {
            let page: Vec<(i64, Uuid)> = sorted
                .iter()
                .copied()
                .filter(|(ms, id)| match cursor {
                    None => true,
                    Some((cursor_ms, cursor_id)) => keyset_after(*ms, id, cursor_ms, &cursor_id),
                })
                .take(limit)
                .collect();
            if page.is_empty() {
                return seen;
            }
            cursor = page.last().copied();
            seen.extend(page);
        }
        panic!("keyset 分页在行数+1 页内未终止（游标不推进，死循环）");
    }

    #[test]
    fn keyset_pagination_same_millisecond_rows_never_repeat_or_stall() {
        // 同毫秒三行（微秒 .001/.456/.999，NOW(6) 的真实形状）+ 前后毫秒行。
        // 旧「微秒时间轴谓词 + 毫秒截断游标」下，最后一毫秒的行会被下一页反复
        // 命中，limit=1 时游标完全重复、永不前进；毫秒时间轴对齐后由 id 决胜。
        let base_ms = 1_800_000_000_123i64;
        let rows = vec![
            (base_ms - 1, Uuid::from_u128(3)),
            (base_ms, Uuid::from_u128(5)),
            (base_ms, Uuid::from_u128(9)),
            (base_ms, Uuid::from_u128(1)),
            (base_ms + 1, Uuid::from_u128(2)),
        ];
        let mut expected = rows.clone();
        expected.sort();
        assert_eq!(simulate_keyset_pages(&rows, 1), expected);
        assert_eq!(simulate_keyset_pages(&rows, 2), expected);
        assert_eq!(simulate_keyset_pages(&rows, 10), expected);
    }
}

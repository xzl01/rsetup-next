//! 采集层（规范 §5.3/§5.5/§5.5.1）：两条聚合 + 一次内存读取，以及**连接生命周期与
//! 并发闸许可的强绑定（I7）**。
//!
//! 三条不可让渡的事实决定了本文件的形状：
//! 1. 池槽位的释放与 handler 的返回天然解耦：`return_to_pool()` 会先 `ping()` 再释放
//!    permit，`close_on_drop()` 的 `Drop` 走的 `take_and_close()` 也是 `rt::spawn` 的
//!    后台关闭（外包 ≤5s）。只要许可随 handler 返回立即释放，"新一波借出新连接"就会与
//!    "上一波仍在后台关闭的连接"逐波叠加，总占用不受并发闸封顶（二轮复审 R1 的根因）。
//! 2. `acquire()` 的默认超时是 30 秒，与 2 秒抓取上限完全脱节 → 必须显式 500ms。
//! 3. `PoolConnection::close()` 全程持有池 permit（sqlx 文档："the connection permit
//!    is retained for the duration so that the pool may not exceed `max_connections`"），
//!    因此"在任务内 await close 完毕"既是池槽位的释放点，也是许可的释放点。
//!
//! 本文件 MUST NOT 依赖 `Pool::disconnect_idle()`（对借出中的连接无效）或
//! `PoolConnection::detach()`（会让进程实际超出 `max_connections`）。

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};

use sqlx::Row as _;
use sqlx::mysql::MySql;
use sqlx::pool::PoolConnection;

use crate::metrics::EDITION;
use crate::metrics::model::{
    AdmissionBucket, DeviceCounts, PoolCounts, ScrapeValues, TaskBucket, TaskCounts,
    admission_label, task_aggregate_error, task_bucket, unknown_admission_state_error,
};
use crate::{ControllerError, DbPool};

/// 并发闸上限：指标线任何时刻在途 + 关闭中的连接数至多 4 槽，共享池默认
/// `max_connections = 10` 中**至少 6 槽恒属主业务**（I7 的容量推论）。
pub const MAX_CONCURRENT_SCRAPE: usize = 4;

/// 两条聚合的整体执行上限（§5.5）。
pub const SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);

/// 借连接的最长等待：池紧张时快速失败（映射 503），绝不排队 30s。
/// MUST NOT 大于 1s，且 MUST 明显小于 `SCRAPE_TIMEOUT`。
pub const POOL_ACQUIRE_TIMEOUT: Duration = Duration::from_millis(500);

/// 设备准入分布（字节级分组：`'APPROVED'` 与 `'approved'`/`'APPROVED '` 是三个独立分组）。
/// 选择列表与 `GROUP BY` 的表达式逐字符相同（`ONLY_FULL_GROUP_BY`）。
///
/// MUST NOT 改用 `COLLATE utf8mb4_bin`：`utf8mb4_bin` 是 `PAD SPACE` 校对规则，
/// 比较时忽略尾部空格填充，`'PENDING' = 'PENDING '` 恒真，脏值会被折叠进合法分组，
/// 整条 fail-closed 链路在 SQL 层就被吞掉（§5.3 约束 2）。也不使用已弃用的 `BINARY`
/// 运算符（产生 deprecation 告警）——`CAST(... AS BINARY)` 不在弃用范围。
///
/// `/*+ MAX_EXECUTION_TIME(2000) */` 提示按 §5.5.1 规则 5 属于 SHOULD：MUST 先在 §7.7
/// 双引擎实测确认"被接受且生效"方可加入（Task 7），未确认前保持本拼法。
pub const DEVICE_AGGREGATE_SQL: &str = r"
SELECT CAST(admission_state AS BINARY) AS bucket, COUNT(*) AS n
FROM devices
GROUP BY CAST(admission_state AS BINARY)
";

/// 主任务状态分布（`state` 与 `outcome` 两列都按字节分组；`outcome` 可为 `NULL`）。
pub const TASK_AGGREGATE_SQL: &str = r"
SELECT CAST(state AS BINARY) AS state,
       CAST(outcome AS BINARY) AS outcome,
       COUNT(*) AS n
FROM main_tasks
GROUP BY CAST(state AS BINARY), CAST(outcome AS BINARY)
";

/// 与 `http_device_status::DeviceStatusSnapshotSource` 同形态的 dyn-safe 注入面（§5.5）。
pub type MetricsFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ControllerError>> + Send + 'a>>;

/// 数据源可注入：进程内 HTTP 测试不连真实库（fake 只能证明 handler/映射/渲染，
/// MUST NOT 被表述为"真实数据库聚合已验收"，§7.7）。
pub trait MetricsSnapshotSource: Send + Sync {
    fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot>;
}

/// 两条聚合的行级映射结果（终态：全部为 `u64` 的三桶/四桶计数）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub devices: DeviceCounts,
    pub tasks: TaskCounts,
}

/// 运行时量：单调 uptime 锚点 + 两个构建常量（§5.2/§5.2.1）。
/// `started_at` 在 `main()` 第一条语句构造（§6.4），使 schema 检查、bootstrap 与
/// 绑端口全部计入 uptime。
pub struct MetricsRuntime {
    pub started_at: Instant,
    pub version: &'static str,
    pub edition: &'static str,
}

impl MetricsRuntime {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            version: env!("CARGO_PKG_VERSION"),
            edition: EDITION,
        }
    }

    /// 单调时钟求差后取整秒（`u64::as_secs()` 天然饱和于 `u64::MAX`，不会回卷）。
    /// 宿主墙钟前跳/回拨都不影响：锚点与读数同为 `Instant`。
    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// 渲染层唯一输入形态的合成点：DB 聚合 + 运行时量 + 池内存读数（§5.5）。
    pub fn scrape_values(&self, snapshot: &MetricsSnapshot, pool: PoolCounts) -> ScrapeValues {
        ScrapeValues {
            version: self.version,
            edition: self.edition,
            uptime_seconds: self.uptime_seconds(),
            devices: snapshot.devices,
            tasks: snapshot.tasks,
            pool,
        }
    }
}

impl Default for MetricsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// 生产汇总源。**只接受既有池的 Clone**：内部按 §5.5.1 执行 acquire 超时、
/// `close_on_drop()` 兜底，并在本调用内 await 连接关闭完毕后才返回——池槽位与
/// 并发闸许可随返回一并释放。生产装配只能由 `production.rs` 以 `inputs.auth.db`
/// 调用（§6.3），`main.rs` 与任何业务线 MUST NOT 自行构造它再作为入参传入。
pub struct SqlxMetricsSnapshotSource {
    db: DbPool,
}

impl SqlxMetricsSnapshotSource {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl MetricsSnapshotSource for SqlxMetricsSnapshotSource {
    fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
        Box::pin(snapshot_via(&self.db))
    }
}

/// 连接池内存读数（§5.3 第 5 条）：零网络操作，MUST NOT 因此发起连接或查询。
/// `size()` 是已创建连接数、`num_idle()` 是空闲数，二者之差即在途数；饱和减法规避
/// 两个读数非原子采集时的瞬时负差。
pub fn pool_counts(pool: &DbPool) -> PoolCounts {
    let size = u64::from(pool.0.size());
    let idle = pool.0.num_idle() as u64;
    PoolCounts {
        active: size.saturating_sub(idle),
        idle,
    }
}

/// 采集内核：`acquire` → 聚合 → **关闭** 全部在调用方任务内顺序完成。
/// 调用方 MUST 把并发闸许可移交本任务持有、直到本函数返回才释放——
/// 本函数的返回点就是"底层连接已完全关闭"的时点（I7）。
pub async fn snapshot_via(pool: &DbPool) -> Result<MetricsSnapshot, ControllerError> {
    // 1) 显式 acquire 超时：500ms 借不到连接 → 立即失败（映射 503）。
    //    此时没有任何连接产生，许可可立即释放。
    let mut conn = tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, pool.0.acquire())
        .await
        .map_err(|_| ControllerError::Config("metrics snapshot: pool acquire timeout".into()))?
        .map_err(|_| ControllerError::Config("metrics snapshot: pool acquire failed".into()))?;

    // 2) 借到即标记"用后即弃"（兜底）：若本 future 整体被取消（正常流程不可达，
    //    仅 runtime 关停/任务 abort），Drop 走 take_and_close() 的 ≤5s 有界关闭、
    //    绝不 return_to_pool() 的 ping-drain-再回池，脏连接永不交还业务线。
    conn.close_on_drop();

    // 3) 两条聚合在上限内顺序执行。自本步起直到第 4 步 close() 完成之前，
    //    **彻底禁止 `?`（或任何等价的提前 return / 提前 drop 分岔）**：查询报错
    //    与超时一律只把错误折进 scrape_outcome，继续走到第 4 步的关闭收尾。
    //    反例（三轮复审 F1，MUST NOT 复活）：`timeout(..).await.map_err(..)?`
    //    ——`?` 会在超时/报错时提前返回、跳过第 4 步 close()，conn 的 Drop 因
    //    close_on_drop() 改由后台任务异步关闭，而持有 OwnedPermit 的独立采集
    //    任务却在此刻退出并归还许可，重新引入"许可提前归还与后台关闭重叠"
    //    导致连接池累积耗尽的漏洞。
    let scrape_outcome = match tokio::time::timeout(SCRAPE_TIMEOUT, run_aggregates(&mut conn)).await
    {
        Ok(Ok(metrics)) => Ok(metrics),
        // 查询报错：沿用 run_aggregates 已映射的去敏错误，绝不提前返回。
        Ok(Err(db_err)) => Err(db_err),
        // 超时：固定去敏文案，绝不提前返回。
        Err(_) => Err(ControllerError::Config(
            "metrics snapshot: query timeout".into(),
        )),
    };

    // 4) 池槽位释放点 = 本函数返回点：无论查询成功、失败还是超时，都必须在当前
    //    任务内**无条件** await PoolConnection::close()，彻底释放连接池槽位
    //    （其文档明确：关闭期间持续持有池 permit，池不会超出 max_connections——
    //    不是 detach），并**不为其设第三层超时**：提前放行许可等于重开多波累积
    //    后门。返回 Err 仅表示关闭握手未正常完成，套接字同样被丢弃、池槽位同样释放。
    let _ = conn.close().await;

    // close().await 执行完成后才返回采集结果；持有 OwnedPermit 的独立采集任务
    // 随本函数返回而退出，随后 OwnedPermit 随任务退出释放（I7）。
    scrape_outcome
}

/// 两条聚合在**同一条池连接**上顺序执行（§5.3 约束 1），行级映射即 §5.2.2 的字节精确
/// 映射；任一未知取值立即返回去敏错误（整次抓取 503，不输出"部分成功 + 缺失族记 0"）。
///
/// 语义边界（§5.3 约束 4）：同一连接不保证跨语句快照，两族读发生在两个时间点，毫秒级
/// 偏差使两条聚合之和非强一致——显式建模为"近似瞬时 gauge"，对账走 SQL，MUST NOT 为此
/// 引入事务（会长期占用共享池槽位）。
async fn run_aggregates(
    conn: &mut PoolConnection<MySql>,
) -> Result<MetricsSnapshot, ControllerError> {
    let mut devices = DeviceCounts::default();
    for row in sqlx::query(DEVICE_AGGREGATE_SQL)
        .fetch_all(&mut **conn)
        .await?
    {
        let bucket: Vec<u8> = row.try_get("bucket")?;
        let count: i64 = row.try_get("n")?;
        let count = checked_count(count)?;
        match admission_label(&bucket) {
            None => return Err(unknown_admission_state_error()),
            Some(bucket) => match bucket {
                AdmissionBucket::Approved => devices.approved += count,
                AdmissionBucket::Pending => devices.pending += count,
                AdmissionBucket::Rejected => devices.rejected += count,
            },
        }
    }

    let mut tasks = TaskCounts::default();
    for row in sqlx::query(TASK_AGGREGATE_SQL)
        .fetch_all(&mut **conn)
        .await?
    {
        let state: Vec<u8> = row.try_get("state")?;
        let outcome: Option<Vec<u8>> = row.try_get("outcome")?;
        let count: i64 = row.try_get("n")?;
        let count = checked_count(count)?;
        match task_bucket(&state, outcome.as_deref()) {
            None => return Err(task_aggregate_error(&state)),
            Some(bucket) => match bucket {
                TaskBucket::Pending => tasks.pending += count,
                TaskBucket::Running => tasks.running += count,
                TaskBucket::Succeeded => tasks.succeeded += count,
                TaskBucket::Failed => tasks.failed += count,
            },
        }
    }

    Ok(MetricsSnapshot { devices, tasks })
}

/// `COUNT(*)` 恒非负（无 `GROUP BY ... WITH ROLLUP`），负值只能是契约外数据 → fail-closed。
fn checked_count(count: i64) -> Result<u64, ControllerError> {
    u64::try_from(count)
        .map_err(|_| ControllerError::Config("metrics aggregate count overflow".into()))
}

// ---------------------------------------------------------------------------
// 测试替身（§7.5 的三种行为；仅测试构建存在，不进生产路径）
// ---------------------------------------------------------------------------

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

/// 仅测试构建可见：L2 测试直接 `Arc::new(FakeMetricsSnapshotSource::…())`。
/// 计数是**实例级**原子量（每个测试各建一个 fake，因此并行测试互不干扰），
/// 语义为"已完成的采集次数"——挂起变体永不产出结果，也就绝不计数。
#[cfg(test)]
#[derive(Clone)]
pub struct FakeMetricsSnapshotSource {
    behavior: Arc<FakeBehavior>,
    calls: Arc<AtomicUsize>,
}

#[cfg(test)]
enum FakeBehavior {
    Fixed(MetricsSnapshot),
    Fails(&'static str),
    Hanging,
}

#[cfg(test)]
impl FakeMetricsSnapshotSource {
    pub fn fixed(snapshot: MetricsSnapshot) -> Self {
        Self::with(FakeBehavior::Fixed(snapshot))
    }
    pub fn failing(message: &'static str) -> Self {
        Self::with(FakeBehavior::Fails(message))
    }
    /// 永久挂起（`std::future::pending`）：用于并发闸占用与 handler 取消语义。
    pub fn hanging() -> Self {
        Self::with(FakeBehavior::Hanging)
    }
    /// 已完成的采集次数。
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn with(behavior: FakeBehavior) -> Self {
        Self {
            behavior: Arc::new(behavior),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[cfg(test)]
impl MetricsSnapshotSource for FakeMetricsSnapshotSource {
    fn snapshot(&self) -> MetricsFuture<'_, MetricsSnapshot> {
        Box::pin(async move {
            let outcome = match self.behavior.as_ref() {
                FakeBehavior::Fixed(snapshot) => Ok(*snapshot),
                FakeBehavior::Fails(message) => {
                    Err(ControllerError::Config((*message).to_string()))
                }
                FakeBehavior::Hanging => std::future::pending().await,
            };
            self.calls.fetch_add(1, Ordering::SeqCst);
            outcome
        })
    }
}

#[cfg(test)]
impl std::fmt::Debug for FakeMetricsSnapshotSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeMetricsSnapshotSource")
            .field("calls", &self.calls())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::model::{DeviceCounts, PoolCounts, TaskCounts};
    use std::time::{Duration, Instant};

    fn message(error: ControllerError) -> String {
        match error {
            ControllerError::Config(text) => text,
            other => panic!("必须是 ControllerError::Config，实得 {other:?}"),
        }
    }

    /// §7.4 末行：uptime 由单调锚点求差，宿主墙钟跳变不得使其倒退或清零。
    #[test]
    fn uptime_never_regresses_across_wall_clock_jumps() {
        let started = Instant::now() - Duration::from_secs(60);
        let runtime = MetricsRuntime {
            started_at: started,
            version: env!("CARGO_PKG_VERSION"),
            edition: crate::metrics::EDITION,
        };
        assert!(
            runtime.uptime_seconds() >= 60,
            "锚点在过去 60 秒时 uptime 必须 >= 60"
        );
        let first = runtime.uptime_seconds();
        for _ in 0..8 {
            assert!(runtime.uptime_seconds() >= first, "uptime 不得倒退");
        }
        // 锚点即当下：uptime 为 0（不是负数回卷）。
        let fresh = MetricsRuntime {
            started_at: Instant::now(),
            ..MetricsRuntime::new()
        };
        assert_eq!(fresh.uptime_seconds(), 0);
    }

    /// `new()` 取构建常量，`scrape_values` 合成渲染输入（uptime 来自锚点，计数来自参数）。
    #[test]
    fn scrape_values_uses_build_constants_and_pool_reading() {
        let runtime = MetricsRuntime::new();
        let snapshot = MetricsSnapshot {
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
        };
        let values = runtime.scrape_values(&snapshot, PoolCounts { active: 2, idle: 3 });
        assert_eq!(values.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(values.edition, crate::metrics::EDITION);
        assert_eq!(values.devices, snapshot.devices);
        assert_eq!(values.tasks, snapshot.tasks);
        assert_eq!((values.pool.active, values.pool.idle), (2, 3));
        assert!(values.uptime_seconds < 5);
    }

    /// §5.3 第 5 条：池 gauge 只读内存字段，绝不发起连接或查询。
    /// lazy 池（永不实际拨号）上 `size()` 恒 0 → `active` 饱和为 0，且调用不产生连接。
    /// （`connect_lazy` 在 sqlx 0.8 要求 Tokio 运行时上下文，故用 `#[tokio::test]`。）
    #[tokio::test]
    async fn pool_counts_reads_memory_only_on_lazy_pool() {
        let pool = lazy_pool();
        let counts = pool_counts(&pool);
        assert_eq!(counts.active, 0);
        assert_eq!(counts.idle, 0);
        assert_eq!(pool.0.size(), 0, "读取池读数不得建立连接");
        assert_eq!(pool.0.num_idle(), 0);
    }

    /// §7.4 的 golden 取值装配路径复用同一 fixture（求和不变量在采集侧同样成立）。
    #[test]
    fn snapshot_buckets_sum_to_fixture_totals() {
        let snapshot = MetricsSnapshot {
            devices: DeviceCounts {
                approved: 7,
                pending: 2,
                rejected: 1,
            },
            tasks: TaskCounts {
                pending: 1,
                running: 2,
                succeeded: 40,
                failed: 3,
            },
        };
        assert_eq!(
            snapshot.devices.approved + snapshot.devices.pending + snapshot.devices.rejected,
            10
        );
        assert_eq!(
            snapshot.tasks.pending
                + snapshot.tasks.running
                + snapshot.tasks.succeeded
                + snapshot.tasks.failed,
            46
        );
    }

    /// §5.3 约束 2/4：两条聚合的 SQL 必须是字节级 `CAST(... AS BINARY)` 分组写法，
    /// 选择列表与 `GROUP BY` 的表达式逐字符相同（`ONLY_FULL_GROUP_BY`），且不含
    /// `COLLATE`/`JOIN`/`WHERE`/`ORDER BY`/`MAX_EXECUTION_TIME`。
    #[test]
    fn aggregate_sql_is_byte_level_grouping() {
        let normalized = |sql: &str| sql.split_whitespace().collect::<Vec<_>>().join(" ");
        assert_eq!(
            normalized(DEVICE_AGGREGATE_SQL),
            "SELECT CAST(admission_state AS BINARY) AS bucket, COUNT(*) AS n \
             FROM devices GROUP BY CAST(admission_state AS BINARY)"
        );
        assert_eq!(
            normalized(TASK_AGGREGATE_SQL),
            "SELECT CAST(state AS BINARY) AS state, CAST(outcome AS BINARY) AS outcome, \
             COUNT(*) AS n FROM main_tasks \
             GROUP BY CAST(state AS BINARY), CAST(outcome AS BINARY)"
        );
        for sql in [DEVICE_AGGREGATE_SQL, TASK_AGGREGATE_SQL] {
            let flat = normalized(sql);
            assert!(
                !flat.contains("COLLATE"),
                "MUST NOT 使用 COLLATE（PAD SPACE 陷阱）：{flat}"
            );
            assert!(!flat.contains("JOIN"), "两条聚合都是单表查询：{flat}");
            assert!(!flat.contains("WHERE"), "全表分布不得加过滤条件：{flat}");
            assert!(
                !flat.contains("ORDER BY"),
                "行数恒定，不需要稳定顺序：{flat}"
            );
            assert!(
                !flat.contains("MAX_EXECUTION_TIME"),
                "该提示须先经 §7.7 双引擎确认才可加入（§5.5.1 规则 5）：{flat}"
            );
        }
    }

    /// §5.5.1 规则 2 + 三轮复审 F1：连接借出后到 `close().await` 之间禁止 `?` 早退。
    /// 源码级门禁（与 §7.8 第 9 条 grep 互为冗余）：本文件的错误一律折入 `scrape_outcome`，
    /// 并在**无条件**的 `close()` 之后统一返回。
    /// 去掉行注释后的纯代码文本（判定"有没有 `?` 早退"必须看代码而不是注释；
    /// 本文件的注释里大量出现 `?`、`return`、`detach` 等词，直接扫原文会误判）。
    fn code_only(source: &str) -> String {
        source
            .lines()
            .map(|line| match line.find("//") {
                Some(index) => &line[..index],
                None => line,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn snapshot_kernel_has_no_early_exit_between_acquire_and_close() {
        let source = include_str!("collect.rs");
        let start = source
            .find("pub async fn snapshot_via")
            .expect("必须存在 snapshot_via 内核");
        let body = {
            // 计划原文写作 `source.find("\n}\n", start)`（memchr 风格双参数），
            // `str::find` 无此签名；等价改法：在 `start` 之后的切片上找第一个收尾。
            let offset = source[start..]
                .find("\n}\n")
                .expect("snapshot_via 函数体结束");
            &source[start..start + offset + 3]
        };
        let code = code_only(body);
        assert!(
            code.contains("conn.close_on_drop();"),
            "第 2 步兜底缺失：借到连接必须立即 close_on_drop()"
        );
        assert!(
            code.contains("let _ = conn.close().await;"),
            "第 4 步缺失：必须无条件 await conn.close()"
        );
        let borrowed_at = code
            .find("conn.close_on_drop();")
            .expect("close_on_drop 调用必须存在");
        let close_at = code.find("conn.close().await").expect("close 调用必须存在");
        let acquire_at = code
            .find("POOL_ACQUIRE_TIMEOUT")
            .expect("显式 acquire 超时必须存在");
        assert!(
            acquire_at < borrowed_at && borrowed_at < close_at,
            "顺序必须是 acquire → 兜底 → close"
        );
        // F1：自"连接已借出"到"close 完成"之间不得出现 `?` 或 `return`（错误一律折入变量）。
        let between = &code[borrowed_at..close_at];
        assert!(
            !between.contains('?'),
            "连接借出后到 close 之前出现 `?` 早退（F1 违规）：{between}"
        );
        assert!(
            !between.contains("return"),
            "连接借出后到 close 之前出现 return 早退（F1 违规）：{between}"
        );
        // 只有两层超时（acquire 与聚合），MUST NOT 为 close 再设会提前放行许可的第三层超时。
        assert_eq!(
            code.matches("tokio::time::timeout(").count(),
            2,
            "恰好两层超时：POOL_ACQUIRE_TIMEOUT 与 SCRAPE_TIMEOUT"
        );
        assert!(
            code.contains("scrape_outcome"),
            "结果必须折入变量后统一返回"
        );
        assert!(
            !code.contains("detach"),
            "detach 会让进程实际超出 max_connections"
        );
        assert!(
            !code.contains("disconnect_idle"),
            "对借出中的连接无效，MUST NOT 依赖"
        );
    }

    /// §5.5.1 规则 1/3 与 §5.5 的常量取值：`POOL_ACQUIRE_TIMEOUT < SCRAPE_TIMEOUT`、
    /// 并发闸上限 4（与 §1.3 "至多 4 槽、至少 6 槽属于业务" 的可指认数值）。
    #[test]
    fn guard_constants_match_spec() {
        assert_eq!(MAX_CONCURRENT_SCRAPE, 4);
        assert_eq!(SCRAPE_TIMEOUT, Duration::from_secs(2));
        assert_eq!(POOL_ACQUIRE_TIMEOUT, Duration::from_millis(500));
        assert!(POOL_ACQUIRE_TIMEOUT < Duration::from_secs(1));
        assert!(POOL_ACQUIRE_TIMEOUT < SCRAPE_TIMEOUT);
    }

    /// 采集侧的两条去敏错误在 `run_aggregates` 映射路径上可指认（L1 侧的等价断言：
    /// 未知取值经映射函数必然产生去敏错误，DB 行数据绝不进消息）。
    #[test]
    fn mapping_errors_are_redacted_and_distinguishable() {
        assert_eq!(
            message(crate::metrics::model::task_aggregate_error(b"DONE")),
            "metrics task aggregate: unknown task state"
        );
        assert_eq!(
            message(crate::metrics::model::task_aggregate_error(b"completed")),
            "metrics task aggregate: unknown task outcome"
        );
        assert_eq!(
            message(crate::metrics::model::unknown_admission_state_error()),
            "metrics device aggregate: unknown admission state"
        );
        // 固定文案里绝不出现 SQL 片段或行数据。
        for text in [
            message(crate::metrics::model::unknown_admission_state_error()),
            message(crate::metrics::model::task_aggregate_error(b"x")),
        ] {
            assert!(!text.contains("SELECT"), "{text}");
            assert!(!text.contains("devices"), "{text}");
        }
    }

    /// 挂起中的 fake 可被外部取消（证明 handler 侧取消只影响 handler；
    /// 采集任务的许可持有语义由 Task 5 的 in-proc 屏障用例与 Task 7 的 L4 用例覆盖）。
    #[tokio::test]
    async fn fake_source_can_be_cancelled_by_caller() {
        let source = FakeMetricsSnapshotSource::hanging();
        let outcome = tokio::time::timeout(Duration::from_millis(20), source.snapshot()).await;
        assert!(outcome.is_err(), "挂起源必须可被调用方取消");
        assert_eq!(source.calls(), 0, "未产出结果时不应计入完成次数");
    }

    #[tokio::test]
    async fn fake_source_fixed_and_fail_behaviours() {
        let snapshot = MetricsSnapshot::default();
        let source = FakeMetricsSnapshotSource::fixed(snapshot);
        assert!(matches!(source.snapshot().await, Ok(other) if other == snapshot));
        assert_eq!(source.calls(), 1);
        let source = FakeMetricsSnapshotSource::failing("metrics fixture failure");
        assert!(source.snapshot().await.is_err());
        assert_eq!(source.calls(), 1);
    }

    fn lazy_pool() -> DbPool {
        DbPool(
            sqlx::mysql::MySqlPoolOptions::new()
                .acquire_timeout(Duration::from_millis(50))
                .connect_lazy("mysql://invalid:invalid@127.0.0.1:1/test")
                .expect("lazy 池构造不连接"),
        )
    }
}

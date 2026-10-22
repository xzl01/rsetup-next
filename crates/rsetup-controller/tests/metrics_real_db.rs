//! L4 真实数据库验收（规范 §7.7）。全部 `#[ignore]`：只在操作员逐引擎显式授权的
//! 隔离开发库上运行，凭据仅进程内载入（`tests/common::required_test_db`），报告只出
//! 去敏数值（耗时、permit/size/idle 计数），绝不回显 DSN（`AGENTS.md` 凭据保护）。
//!
//! 运行方式（fixture 组用例会向共享开发库临时写入脏值行，同二进制内的其他用例
//! 必须串行，避免互相污染聚合结果）：
//! `cargo test -p rsetup-controller --test metrics_real_db -- --ignored --test-threads=1`
//!
//! 诚实界定（三条，交付报告必须复述）：
//! 1. L2/L3 的合成源通过 MUST NOT 被表述为"真库聚合已验收"；L4 未运行时，交付报告
//!    必须显式写明"两条聚合 SQL 的真实引擎验收仍待执行"。
//! 2. 慢查询与受控关闭屏障使用**测试变体内核**（本文件的 `guarded_snapshot`），它是
//!    §5.5.1 生产骨架的逐字复制，只有两处不同：两条聚合换成 `SELECT SLEEP(...)`，
//!    close await 之前可选插入屏障。生产实现与骨架的一致性由 Task 4 的 L1 源码级门禁
//!    `snapshot_kernel_has_no_early_exit_between_acquire_and_close` 与 §7.8 第 9 条 grep
//!    钉住；本文件证明的是这套骨架在真实驱动下的 `close()`/permit 语义成立。
//! 3. 普通 `SELECT SLEEP` 只拖慢**服务端查询**，而 `MySqlConnection::close()` 只发一个
//!    Quit 报文，"close 中"与"下一波借出"的真实重叠窗口极短且依赖时序，因此第 3 组
//!    第 1 步只能证明宏观不饥饿；"close 完成前 permit 绝不回落"的确定性证明由第 4 步
//!    的受控关闭屏障完成（对旧缺陷实现稳定必红，对正确实现稳定绿）。
#![allow(dead_code)]

mod common;

use rsetup_controller::DbPool;
use rsetup_controller::metrics::collect::{
    DEVICE_AGGREGATE_SQL, MAX_CONCURRENT_SCRAPE, MetricsSnapshotSource, POOL_ACQUIRE_TIMEOUT,
    SCRAPE_TIMEOUT, SqlxMetricsSnapshotSource, TASK_AGGREGATE_SQL,
};
use sqlx::Row as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore, oneshot};

/// 大于 `SCRAPE_TIMEOUT`，保证每波都以查询超时收场。
const SLEEP_SECONDS: u64 = 3;
const WAVES: usize = 3;

/// 一次抓取尝试的**确定性定性**（四轮复审 F1：布尔返回值把"查询超时"与"闸满被拒"
/// 混成同一个 `false`，导致超时波次的断言与语义相反）。每一路都必须落到唯一变体：
/// 生产侧 `TimedOut`/`RejectedByGate`/`Failed` 一律映射 503，但测试断言依赖区分它们。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempt {
    /// 并发闸满：未借出连接、未发起查询，**立即**被拒（生产等价 503）。
    RejectedByGate,
    /// 查询在 `SCRAPE_TIMEOUT` 内未返回，close 完成后才算结束（生产等价 503）。
    TimedOut,
    /// 借连接失败/超时，或查询以非超时方式报错（生产等价 503）。
    Failed,
    /// 查询在上限内成功返回。
    Succeeded,
}

fn count_variant(attempts: &[Attempt], want: Attempt) -> usize {
    attempts.iter().filter(|a| **a == want).count()
}

// ---------------------------------------------------------------------------
// §5.5.1 骨架的测试变体内核（受控关闭屏障的唯一注入点，MUST NOT 进生产代码）
// ---------------------------------------------------------------------------

/// 受控关闭屏障：`wait_at_close` 在 close await **之前**抵达并挂起，直到测试放行。
/// 只控制 close 的完成时刻，不改变 acquire 超时、`close_on_drop()` 兜底、许可持有
/// 任务等任何其余语义。
struct CloseGate {
    waiting: AtomicUsize,
    arrived: Notify,
    release: Semaphore,
}

/// `tokio::sync::Semaphore` 未实现 `Default`，手写等价 derive：放行侧 0 permits，
/// 抵达即挂起，与 `CloseGate::new()` 的语义完全一致。
impl Default for CloseGate {
    fn default() -> Self {
        Self {
            waiting: AtomicUsize::new(0),
            arrived: Notify::new(),
            release: Semaphore::new(0),
        }
    }
}

impl CloseGate {
    /// 0 permits：抵达即挂起，由测试 `release_all` 放行。
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    fn waiting(&self) -> usize {
        self.waiting.load(Ordering::SeqCst)
    }
    async fn wait_at_close(&self) {
        self.waiting.fetch_add(1, Ordering::SeqCst);
        self.arrived.notify_waiters();
        let permit = self
            .release
            .acquire()
            .await
            .expect("放行侧只 add_permits，永不 close");
        drop(permit);
    }
    fn release_all(&self, count: usize) {
        self.release.add_permits(count);
    }
}

/// 轮询到"恰好 N 路 close 已挂起"，让随后的断言完全不依赖时序侥幸。
async fn await_waiting(gate: &CloseGate, want: usize) {
    for _ in 0..200 {
        if gate.waiting() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "受控关闭屏障未在 5 秒内挂住 {want} 路 close（实测 {}）",
        gate.waiting()
    );
}

/// 轮询到并发闸的可用许可恰好回到 `want`，超时即 panic（证明许可的最终归宿）。
async fn await_permits(guard: &Semaphore, want: usize) {
    for _ in 0..200 {
        if guard.available_permits() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!(
        "并发闸许可未在 5 秒内回到 {want}（实测 {}）",
        guard.available_permits()
    );
}

/// §5.5.1 代码骨架的复制：显式 acquire 超时 → 立即 `close_on_drop()` → 超时包裹慢
/// 查询 → 结果折入变量 → **无条件** `await close()`。全程禁止 `?` 早退（同 Task 4）。
/// 对端生产实现：`src/metrics/collect.rs::snapshot_via`——任何对骨架的改动都必须
/// 同步到这里（两侧注释都写明对端位置）。
/// 返回本次采集的定性（`Attempt` 三态之一：`TimedOut` / `Failed` / `Succeeded`；
/// `RejectedByGate` 只可能由 `scrape_holding_permit` 产生）。
async fn guarded_snapshot(db: &DbPool, gate: Option<Arc<CloseGate>>) -> Attempt {
    let mut conn = match tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, db.0.acquire()).await {
        // 借不到连接：没有连接产生，许可可立即释放。
        Ok(Ok(conn)) => conn,
        _ => return Attempt::Failed,
    };
    conn.close_on_drop();
    let outcome = tokio::time::timeout(SCRAPE_TIMEOUT, async {
        sqlx::query(&format!("SELECT SLEEP({SLEEP_SECONDS})"))
            .fetch_optional(&mut *conn)
            .await
    })
    .await;
    let timed_out = outcome.is_err();
    if let Some(gate) = gate {
        gate.wait_at_close().await;
    }
    let _ = conn.close().await;
    if timed_out {
        Attempt::TimedOut
    } else if outcome.expect("上面的 is_err 分支已排除 Err").is_ok() {
        Attempt::Succeeded
    } else {
        Attempt::Failed
    }
}

/// 生产 handler 许可接线的测试侧等价物：许可由 `tokio::spawn` 的任务持有，任务返回
/// （= 连接已完全关闭）才随 Drop 释放；闸满时**立即**返回 `RejectedByGate`。
async fn scrape_holding_permit(
    db: &DbPool,
    guard: Arc<Semaphore>,
    gate: Option<Arc<CloseGate>>,
) -> Attempt {
    let Ok(permit) = guard.clone().try_acquire_owned() else {
        return Attempt::RejectedByGate;
    };
    let (tx, rx) = oneshot::channel();
    let db = db.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _ = tx.send(guarded_snapshot(&db, gate).await);
    });
    // 调用方被取消/采集任务 panic 时以 Failed 收场（生产等价 503）。
    rx.await.unwrap_or(Attempt::Failed)
}

/// 业务哨兵：每 100 ms 一次 `acquire()` + `SELECT 1` + 归还，记录耗时与成败；
/// 同时采样 `pool.size()` 峰值，用于"指标线占用至多 4 槽"的封顶断言。
#[derive(Default)]
struct Sentinel {
    samples: Mutex<Vec<(u128, bool)>>,
    peak_size: AtomicUsize,
}

impl Sentinel {
    fn spawn(self: &Arc<Self>, db: DbPool) -> tokio::task::JoinHandle<()> {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let started = Instant::now();
                let alive = match tokio::time::timeout(Duration::from_millis(2_000), db.0.acquire())
                    .await
                {
                    Ok(Ok(mut conn)) => {
                        let ok = sqlx::query_scalar::<_, i64>("SELECT 1")
                            .fetch_optional(&mut *conn)
                            .await
                            .is_ok();
                        let _ = conn.close().await;
                        ok
                    }
                    _ => false,
                };
                this.samples
                    .lock()
                    .expect("哨兵样本锁")
                    .push((started.elapsed().as_millis(), alive));
                let _ = this
                    .peak_size
                    .fetch_max(db.0.size() as usize, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    }
    /// `(样本数, 失败数, p50 毫秒, p99 毫秒)`。
    fn report(&self) -> (usize, usize, u128, u128) {
        let mut samples = self.samples.lock().expect("哨兵样本锁");
        let total = samples.len();
        let failed = samples.iter().filter(|(_, alive)| !*alive).count();
        samples.sort_unstable_by_key(|(ms, _)| *ms);
        let percentile = |q: f64| -> u128 {
            if samples.is_empty() {
                return 0;
            }
            let index = ((samples.len() - 1) as f64 * q).round() as usize;
            samples[index].0
        };
        (total, failed, percentile(0.50), percentile(0.99))
    }
}

async fn join_all(handles: Vec<tokio::task::JoinHandle<Attempt>>) -> Vec<Attempt> {
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        // JoinError（panic/取消）按 Failed 计，绝不伪装成任何一种正常定性。
        out.push(handle.await.unwrap_or(Attempt::Failed));
    }
    out
}

// ---------------------------------------------------------------------------
// §7.7 第 1 组：分布对账与求和不变量
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires an operator-authorized isolated disposable dev DB (MySQL or TiDB); read-only"]
async fn aggregates_match_table_distribution_and_sum_invariant() {
    let db = common::required_test_db().await;
    let engine: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert!(!engine.trim().is_empty(), "SELECT VERSION() 必须返回版本");

    let source = SqlxMetricsSnapshotSource::new(db.clone());
    let snapshot = source
        .snapshot()
        .await
        .expect("既有数据上的两条聚合必须成功");
    // 对账一律使用**持久层取值**：`admission_state` 的合法枚举是 APPROVED/PENDING/
    // REVOKED，而指标模型（`src/metrics/model.rs`）把持久值 `REVOKED` 导出为指标标签
    // `REJECTED`（§5.2.2 刻意留档的命名映射偏差）。用标签 `REJECTED` 去 COUNT(*) 恒为
    // 0，与快照读数必然失配（P1）。
    for (db_val, expected) in [
        ("APPROVED", snapshot.devices.approved),
        ("PENDING", snapshot.devices.pending),
        ("REVOKED", snapshot.devices.rejected), // 持久层 REVOKED → 指标侧 rejected 桶
    ] {
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devices WHERE CAST(admission_state AS BINARY) = CAST(? AS BINARY)",
        )
        .bind(db_val)
        .fetch_one(&db.0)
        .await
        .unwrap();
        assert_eq!(
            rows as u64, expected,
            "admission_state={db_val} 的聚合取值必须与独立 COUNT(*) 对账"
        );
    }
    let device_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM devices")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(
        device_rows as u64,
        snapshot.devices.approved + snapshot.devices.pending + snapshot.devices.rejected,
        "三桶求和必须恒等于 devices 行数（§5.1 第 4 条）"
    );
    let task_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM main_tasks")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(
        task_rows as u64,
        snapshot.tasks.pending
            + snapshot.tasks.running
            + snapshot.tasks.succeeded
            + snapshot.tasks.failed,
        "四桶求和必须恒等于 main_tasks 行数"
    );
    // 去敏报告：只出计数，不出 DSN、不出取值原文。
    eprintln!(
        "L4 对账：devices_rows={device_rows} tasks_rows={task_rows} 指标侧 devices={}/{}/{} tasks={}/{}/{}/{}",
        snapshot.devices.approved,
        snapshot.devices.pending,
        snapshot.devices.rejected,
        snapshot.tasks.pending,
        snapshot.tasks.running,
        snapshot.tasks.succeeded,
        snapshot.tasks.failed,
    );
}

// ---------------------------------------------------------------------------
// §7.7 第 2 组：字节级分组 + PAD SPACE 对照（F2：单类异常隔离 + 任务表覆盖；
// F5：fixture 全部使用随机唯一主键，普通 INSERT，重复即报错，绝不覆盖既有行）
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires an operator-authorized isolated disposable dev DB with writable fixture tables; inserts only rows it deletes afterwards"]
async fn grouping_is_byte_exact_on_both_engines() {
    let db = common::required_test_db().await;
    let source = SqlxMetricsSnapshotSource::new(db.clone());

    // ---- A1（设备侧·合法枚举值正常）：三个合法取值各自成组且映射成功 ----
    let valid = fixture_rows(&db, &["APPROVED", "PENDING", "REVOKED"]).await;
    let snapshot = source
        .snapshot()
        .await
        .expect("只存在合法 admission_state 取值时 snapshot 必须成功");
    // 与第 1 组同源（P1）：插入的是持久值 `REVOKED`，对账 SQL 的过滤条件也必须是
    // 持久值；指标标签 `REJECTED` 只存在于渲染层，用它 COUNT(*) 必为 0。
    for (db_val, got) in [
        ("APPROVED", snapshot.devices.approved),
        ("PENDING", snapshot.devices.pending),
        ("REVOKED", snapshot.devices.rejected), // 持久层是 REVOKED，指标模型映射为 rejected
    ] {
        let want: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devices WHERE CAST(admission_state AS BINARY) = CAST(? AS BINARY)",
        )
        .bind(db_val)
        .fetch_one(&db.0)
        .await
        .unwrap();
        assert_eq!(
            want as u64, got,
            "合法取值下 admission_state={db_val} 必须与独立 COUNT(*) 对账"
        );
    }
    cleanup_rows(&db, &valid).await;

    // ---- A2（设备侧·字节级分组）：两对分组键：只差大小写、只差尾部空格。
    // 列名以 `DESCRIBE devices` 为准，本用例只依赖 admission_state 可写入任意
    // 字节串（§5.2.2：库侧已无 CHECK 约束）。
    // `grouped_devices` 与生产映射同规则剥除尾部 `0x00` 哨兵（缺哨兵直接 panic），
    // 因此这里的"各自成组"断言断的是**剥哨兵后的真实字节取值**。
    let dirty = fixture_rows(&db, &["CASE", "case", "PADX", "PADX "]).await;
    let byte_rows = grouped_devices(&db).await;
    assert!(
        byte_rows.iter().any(|(key, _)| key == "CASE")
            && byte_rows.iter().any(|(key, _)| key == "case"),
        "大小写必须各自成组（字节级分组生效）：{byte_rows:?}"
    );
    assert!(
        byte_rows.iter().any(|(key, _)| key == "PADX")
            && byte_rows.iter().any(|(key, _)| key == "PADX "),
        "尾部空格必须各自成组（TiDB v8.5.8 预编译线路的折叠缺陷已由哨兵挡住）：{byte_rows:?}"
    );
    assert!(
        count_of(&byte_rows, "CASE") >= 1 && count_of(&byte_rows, "case") >= 1,
        "两行必须落在两个不同分组：{byte_rows:?}"
    );
    // PAD SPACE 陷阱对照：同一份数据用 utf8mb4_bin 分组会**折叠**尾部空格。
    let collation_rows = grouped_collation_control(
        &db,
        "SELECT CAST(admission_state AS CHAR) COLLATE utf8mb4_bin AS bucket, COUNT(*) AS n \
         FROM devices WHERE CAST(admission_state AS BINARY) IN (CAST('PADX' AS BINARY), CAST('PADX ' AS BINARY)) \
         GROUP BY CAST(admission_state AS CHAR) COLLATE utf8mb4_bin",
    )
    .await;
    assert!(
        collation_rows
            .iter()
            .filter(|(key, _)| key.trim_end() == "PADX")
            .count()
            <= 1,
        "utf8mb4_bin 必须把 'PADX' 与 'PADX ' 折叠为同一组，否则本规范对引擎事实的评审不成立：{collation_rows:?}"
    );

    // ---- A3（设备侧·单类异常隔离，F2）：**每次只存在一类异常取值**也必须让
    // 映射返回 None → snapshot 稳定返回错误（Fail-Closed 503），排除"多类脏值
    // 混合才触发"的假阳性；同时钉住去敏：错误不得回显取值原文。
    // 其中的尾空格变体（`"APPROVED "`）就是 TiDB v8.5.8 预编译线路折叠缺陷的回归
    // 用例：折叠一旦复现，脏值会被并进合法分组、snapshot 反而返回 Ok，本用例必红。
    cleanup_rows(&db, &dirty).await;
    for value in ["CASE", "case", "PADX", "PADX ", "approved", "APPROVED "] {
        let one = fixture_rows(&db, &[value]).await;
        let error = source
            .snapshot()
            .await
            .expect_err("单个未知 admission_state 取值必须让整次抓取失败（I4/G4）");
        let text = format!("{error:?}");
        assert!(
            text.contains("unknown admission state"),
            "设备侧未知取值必须走固定去敏文案：{text}"
        );
        for marker in ["CASE", "case", "PADX", "approved", "APPROVED"] {
            assert!(!text.contains(marker), "去敏：错误不得回显取值原文：{text}");
        }
        cleanup_rows(&db, &one).await;
    }

    // ---- B1（任务侧·合法取值正常）：queued/running/completed（含 outcome NULL）----
    let valid_tasks = task_fixture_rows(
        &db,
        &[
            ("queued", None),
            ("running", None),
            ("completed", Some("success")),
            ("completed", None),
        ],
    )
    .await;
    let snapshot = source
        .snapshot()
        .await
        .expect("只存在合法任务 state/outcome 时 snapshot 必须成功");
    let task_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM main_tasks")
        .fetch_one(&db.0)
        .await
        .unwrap();
    assert_eq!(
        task_rows as u64,
        snapshot.tasks.pending
            + snapshot.tasks.running
            + snapshot.tasks.succeeded
            + snapshot.tasks.failed,
        "合法任务 fixture 后四桶求和仍必须恒等于 main_tasks 行数"
    );
    cleanup_task_rows(&db, &valid_tasks).await;

    // ---- B2（任务侧·字节级分组，F2）：`state` 与 `outcome` 两列都按字节分组。
    // 以合法基线 `('completed', 'success')` 为中心，四行变体各自**只有一列**与基线
    // 不同（差大小写或差尾部空格），共 5 个分组键：任一列退化为 PAD SPACE 都只会
    // 让**它自己那一列**的变体并回基线组，`task_groups.len() >= 5` + 逐组计数即可
    // 独立证明"state 或 outcome 任一列折叠"都能被发现。
    let dirty_tasks = task_fixture_rows(
        &db,
        &[
            ("completed", Some("success")),  // 合法基线
            ("Completed", Some("success")),  // state 差大小写
            ("completed ", Some("success")), // state 差尾部空格
            ("completed", Some("SUCCESS")),  // outcome 差大小写
            ("completed", Some("success ")), // outcome 差尾部空格
        ],
    )
    .await;
    let task_groups = grouped_tasks(&db).await;
    assert!(
        task_groups.len() >= 5,
        "5 个分组键必须各自成组（state/outcome 任一列退化为 PAD SPACE 都会让分组数掉到 5 以下）：\
         实测 {} 组 {task_groups:?}",
        task_groups.len()
    );
    for (state, outcome) in [
        ("completed", Some("success")),
        ("Completed", Some("success")),
        ("completed ", Some("success")),
        ("completed", Some("SUCCESS")),
        ("completed", Some("success ")),
    ] {
        assert!(
            task_group_count(&task_groups, state, outcome) >= 1,
            "state/outcome 的字节级差异必须各自成组：({state:?}, {outcome:?}) 缺席于 {task_groups:?}"
        );
    }
    cleanup_task_rows(&db, &dirty_tasks).await;

    // ---- B3/B4（任务侧·单类异常隔离，F2）：
    // B3：`state` 不在四值域（大小写或尾空格任一形式的字节差异都算）→ unknown task state；
    // B4：`state` 合法而 `outcome` 不在枚举（含尾空格）→ unknown task outcome。
    // 每轮**只存在这一行异常**，排除"多类脏值混合才触发"的假阳性。
    for (row, marker) in [
        (("Completed", Some("success")), "unknown task state"),
        (("completed ", Some("success")), "unknown task state"), // state 尾空格
        (("completed", Some("SUCCESS")), "unknown task outcome"),
        (("completed", Some("success ")), "unknown task outcome"), // outcome 尾空格
    ] {
        let one = task_fixture_rows(&db, &[row]).await;
        let error = source
            .snapshot()
            .await
            .expect_err("单个未知任务 state/outcome（含尾空格）必须让整次抓取失败（I4/G4）");
        let text = format!("{error:?}");
        assert!(
            text.contains(marker),
            "任务侧异常必须走 {marker} 去敏文案：{text}"
        );
        for leak in ["Completed", "SUCCESS", "completed", "success"] {
            assert!(!text.contains(leak), "去敏：错误不得回显取值原文：{text}");
        }
        cleanup_task_rows(&db, &one).await;
    }
    source
        .snapshot()
        .await
        .expect("fixture 清理后 snapshot 必须恢复成功（异常定性不残留）");

    // ---- F3：`SHOW WARNINGS` 与聚合 SQL 绑定在**同一物理连接**上验证 ----
    // `db.0` 是连接池：两条语句各自借还可能不同的连接时，`SHOW WARNINGS` 属于
    // "另一条连接的会话状态"，结论无效。必须借出单个连接、在同一个连接上
    // 先验证 sql_mode、再执行聚合、紧接着 SHOW WARNINGS。
    let mut conn = db.0.acquire().await.expect("必须能借到一条专用连接");
    let sql_mode: String = sqlx::query_scalar("SELECT @@SESSION.sql_mode")
        .fetch_one(&mut *conn)
        .await
        .expect("必须能读到会话 sql_mode");
    assert!(
        sql_mode
            .split(',')
            .map(str::trim)
            .any(|mode| mode == "ONLY_FULL_GROUP_BY"),
        "验收前提是 ONLY_FULL_GROUP_BY：实测 sql_mode={sql_mode}"
    );
    for (name, sql) in [
        ("devices 聚合", DEVICE_AGGREGATE_SQL),
        ("main_tasks 聚合", TASK_AGGREGATE_SQL),
    ] {
        // ONLY_FULL_GROUP_BY 下必须可执行。
        sqlx::query(sql)
            .fetch_all(&mut *conn)
            .await
            .unwrap_or_else(|error| panic!("{name} 在 ONLY_FULL_GROUP_BY 下必须可执行：{error}"));
        // **同一连接**上紧接着执行：上一条语句的告警只存在于这条连接的会话里。
        // MySQL 8.0/8.4 的 `SHOW WARNINGS` 不支持 Prepared Statement 协议
        // （HY000/1295），必须走 `raw_sql` 文本协议。
        let warnings = sqlx::raw_sql("SHOW WARNINGS")
            .fetch_all(&mut *conn)
            .await
            .expect("SHOW WARNINGS 必须与聚合 SQL 同连接执行");
        assert!(
            warnings.is_empty(),
            "{name} 在同一连接上不得产生任何告警（含 CONCAT/0x00 字面量的截断或字符集告警）：{warnings:?}"
        );
    }
    drop(conn);
}

/// OS 随机源读 `len` 字节；不可用时退化为"纳秒时间戳 ^ 进程 id"的种子流。
/// 两种来源都保证同一轮测试内的主键互不相同（F5：绝不硬编码固定主键）。
fn random_bytes(len: usize) -> Vec<u8> {
    use std::io::Read;
    let mut buf = vec![0u8; len];
    if let Ok(mut file) = std::fs::File::open("/dev/urandom")
        && file.read_exact(&mut buf).is_ok()
    {
        return buf;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut seed = nanos ^ (u128::from(std::process::id()) << 64);
    for byte in &mut buf {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *byte = (seed >> 33) as u8;
    }
    buf
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 插入给定 `admission_state` 取值的 fixture 行，返回主键以便收尾**定点**删除。
/// 列名取自 `migrations/0003_identity_application_integrity.sql` 的 `devices` 实表
/// （主键 `public_key BINARY(32)`、`display_name`/`review_decision`/`revision` 均
/// NOT NULL 且无默认值）。F5：主键与 display_name 都是随机唯一值；普通
/// `INSERT INTO`——若 32 字节随机主键真的撞库必须**报错暴露**，MUST NOT 用
/// `ON DUPLICATE KEY UPDATE` 静默覆盖既有业务行。
async fn fixture_rows(db: &DbPool, values: &[&str]) -> Vec<Vec<u8>> {
    let mut ids = Vec::new();
    for value in values {
        let id = random_bytes(32);
        let display_name = format!("l4-metrics-{}", hex(&random_bytes(8)));
        sqlx::query(
            "INSERT INTO devices (public_key, display_name, admission_state, review_decision, revision) \
             VALUES (?, ?, ?, 'none', 1)",
        )
        .bind(&id)
        .bind(display_name)
        .bind(*value)
        .execute(&db.0)
        .await
        .expect("fixture 行插入必须在已授权开发库内成功（随机主键冲突属异常，必须暴露）");
        ids.push(id);
    }
    ids
}

/// 同上（F5）：`main_tasks` fixture，主键 `id BINARY(16)` 随机唯一，
/// `idempotency_key` 随机 hex 保证 `(actor_user_id, idempotency_key)` 唯一键不碰撞。
/// NOT NULL 列的取值口径与 `migrations/0004_tasks.sql` 一致。
async fn task_fixture_rows(db: &DbPool, rows: &[(&str, Option<&str>)]) -> Vec<Vec<u8>> {
    let mut ids = Vec::new();
    for (state, outcome) in rows {
        let id = random_bytes(16);
        sqlx::query(
            "INSERT INTO main_tasks (id, actor_user_id, idempotency_key, request_hash, target_hash, \
             preview_token_hash, state, outcome, revision, counts_json, created_time_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, '{}', '{}', NOW(6), NOW(6))",
        )
        .bind(&id)
        .bind(random_bytes(16))
        .bind(hex(&random_bytes(16)))
        .bind(random_bytes(32))
        .bind(random_bytes(32))
        .bind(random_bytes(32))
        .bind(*state)
        .bind(*outcome)
        .execute(&db.0)
        .await
        .expect("main_tasks fixture 行插入必须在已授权开发库内成功");
        ids.push(id);
    }
    ids
}

/// 只删除本次插入的唯一主键行，MUST NOT 按取值或前缀批量删除（F5：避免误删既有行）。
async fn cleanup_rows(db: &DbPool, ids: &[Vec<u8>]) {
    for id in ids {
        sqlx::query("DELETE FROM devices WHERE public_key = ?")
            .bind(id)
            .execute(&db.0)
            .await
            .expect("fixture 必须清理干净，不留残留行");
    }
}

async fn cleanup_task_rows(db: &DbPool, ids: &[Vec<u8>]) {
    for id in ids {
        sqlx::query("DELETE FROM main_tasks WHERE id = ?")
            .bind(id)
            .execute(&db.0)
            .await
            .expect("main_tasks fixture 必须清理干净，不留残留行");
    }
}

/// **不带哨兵的对照查询**专用：`COLLATE utf8mb4_bin` 对照语句自己选择列，返回值
/// 就是原样字符串（MUST NOT 剥哨兵，否则对照组被悄悄改写成生产写法）。
async fn grouped_collation_control(db: &DbPool, sql: &str) -> Vec<(String, i64)> {
    let rows = sqlx::query(sql).fetch_all(&db.0).await.unwrap();
    rows.iter()
        .map(|row| {
            let bucket: Vec<u8> = row.try_get("bucket").unwrap();
            let n: i64 = row.try_get("n").unwrap();
            (String::from_utf8_lossy(&bucket).into_owned(), n)
        })
        .collect()
}

/// 生产 SQL 的返回值契约：每列取值末尾恒有一个 `0x00` 哨兵
/// （`CONCAT(CAST(<col> AS BINARY), 0x00)`）。
/// 剥除**且只剥一个**；缺哨兵直接 panic——它同时钉住"生产 SQL 确实是哨兵写法"
/// 与"引擎按字节原样回传（预编译线路）"两件事。
fn strip_sentinel(bytes: &[u8]) -> String {
    let Some(body) = bytes.strip_suffix(b"\0") else {
        panic!(
            "聚合取值必须以 0x00 尾部哨兵结尾，实测末字节 {:?}（长度 {}）",
            bytes.last(),
            bytes.len()
        );
    };
    String::from_utf8_lossy(body).into_owned()
}

/// `DEVICE_AGGREGATE_SQL` 的 `(bucket, n)` 行（哨兵已按生产映射同规则剥除）。
async fn grouped_devices(db: &DbPool) -> Vec<(String, i64)> {
    let rows = sqlx::query(DEVICE_AGGREGATE_SQL)
        .fetch_all(&db.0)
        .await
        .unwrap();
    rows.iter()
        .map(|row| {
            let bucket: Vec<u8> = row.try_get("bucket").unwrap();
            let n: i64 = row.try_get("n").unwrap();
            (strip_sentinel(&bucket), n)
        })
        .collect()
}

/// `TASK_AGGREGATE_SQL` 的 `(state, outcome, n)` 行：`outcome` 可为 `NULL`
/// （`CONCAT(CAST(NULL AS BINARY), 0x00)` 仍是 `NULL`，因此 `NULL` 不经过哨兵剥除）。
async fn grouped_tasks(db: &DbPool) -> Vec<(String, Option<String>, i64)> {
    let rows = sqlx::query(TASK_AGGREGATE_SQL)
        .fetch_all(&db.0)
        .await
        .unwrap();
    rows.iter()
        .map(|row| {
            let state: Vec<u8> = row.try_get("state").unwrap();
            let outcome: Option<Vec<u8>> = row.try_get("outcome").unwrap();
            let n: i64 = row.try_get("n").unwrap();
            (
                strip_sentinel(&state),
                outcome.map(|o| strip_sentinel(&o)),
                n,
            )
        })
        .collect()
}

fn task_group_count(
    rows: &[(String, Option<String>, i64)],
    state: &str,
    outcome: Option<&str>,
) -> i64 {
    rows.iter()
        .find(|(s, o, _)| s == state && o.as_deref() == outcome)
        .map(|(_, _, n)| *n)
        .unwrap_or(0)
}

fn count_of(rows: &[(String, i64)], key: &str) -> i64 {
    rows.iter()
        .find(|(k, _)| k == key)
        .map(|(_, n)| *n)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// §7.7 第 3 组：多波超时 + 业务哨兵 + 受控关闭屏障 + 取消变体
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an operator-authorized isolated disposable dev DB; runs 3 slow waves plus a controlled close barrier"]
async fn metrics_slow_query_does_not_exhaust_shared_business_pool() {
    let db = common::required_test_db().await; // **同一个池**，绝不新建（§7.7 第 3 组）
    let baseline = db.0.size() as usize;
    // 全流程唯一的全局并发闸：波次、屏障与取消变体共用（F4：绝不按波新建 Semaphore）。
    let guard = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPE));
    let sentinel = Arc::new(Sentinel::default());
    let sentinel_task = Arc::clone(&sentinel).spawn(db.clone());

    // 第 1 步：3 波背靠背，每波 MAX+1 路。F1：按 `Attempt` 四态精确定性——
    // `MAX_CONCURRENT_SCRAPE` 路以真实查询超时（TimedOut）收场、恰好 1 路闸满
    // 即拒（RejectedByGate）、0 路成功、0 路借连失败。
    let mut wave_elapsed = Vec::new();
    for wave in 0..WAVES {
        let started = Instant::now();
        let handles: Vec<_> = (0..=MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let db = db.clone();
                let guard = Arc::clone(&guard);
                tokio::spawn(async move { scrape_holding_permit(&db, guard, None).await })
            })
            .collect();
        let attempts = join_all(handles).await;
        assert_eq!(
            (
                count_variant(&attempts, Attempt::TimedOut),
                count_variant(&attempts, Attempt::RejectedByGate),
                count_variant(&attempts, Attempt::Failed),
                count_variant(&attempts, Attempt::Succeeded),
            ),
            (MAX_CONCURRENT_SCRAPE, 1, 0, 0),
            "第 {wave} 波必须是 {MAX_CONCURRENT_SCRAPE} 路查询超时 + 1 路闸满即拒 + 0 路成功/失败：{attempts:?}"
        );
        wave_elapsed.push(started.elapsed());
        // 辅助判据：若许可随超时被提前放行，下一波会立刻进闸并再付一次 2 秒超时；
        // 正确实现下第 2、3 波应在毫秒级结束（"闸满即拒"的比例不低于第 1 波）。
        if wave > 0 {
            assert!(
                wave_elapsed[wave] < Duration::from_millis(4_000),
                "第 {wave} 波耗时 {:?} 说明许可未按连接生命周期持有",
                wave_elapsed[wave]
            );
        }
    }

    // 第 2/3 步：哨兵零失败、每次 acquire < 500 ms、p99 不随波次上升。
    tokio::time::sleep(Duration::from_secs(5)).await;
    sentinel_task.abort();
    let (total, failed, p50, p99) = sentinel.report();
    assert!(
        total >= 30,
        "哨兵样本过少（{total}），不足以证明无累积型饥饿"
    );
    assert_eq!(failed, 0, "业务哨兵必须零失败");
    assert!(p50 < 500, "业务 acquire p50 必须 < 500 ms，实测 {p50} ms");
    assert!(p99 < 500, "业务 acquire 每次都必须 < 500 ms，实测 {p99} ms");
    let peak = sentinel.peak_size.load(Ordering::SeqCst);
    assert!(
        peak <= baseline + MAX_CONCURRENT_SCRAPE,
        "指标线任何时刻（在途 + 关闭中）至多占 {MAX_CONCURRENT_SCRAPE} 槽，实测峰值 {peak}（基线 {baseline}）"
    );
    assert!(
        (db.0.size() as usize) <= baseline + 1,
        "末波结束 5 秒内池容量必须回落基线附近，实测 {}",
        db.0.size()
    );
    eprintln!(
        "L4 池回落：size={}/{} idle={} 哨兵 n={total} 失败={failed} p50={p50}ms p99={p99}ms 波耗时={wave_elapsed:?}",
        db.0.size(),
        baseline + MAX_CONCURRENT_SCRAPE,
        db.0.num_idle(),
    );

    // 第 4 步：**受控关闭屏障**——close 完成前 permit 绝不回落的确定性断言。
    let gate = CloseGate::new();
    let handles: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
        .map(|_| {
            let db = db.clone();
            let guard = Arc::clone(&guard);
            let gate = Arc::clone(&gate);
            tokio::spawn(async move { scrape_holding_permit(&db, guard, Some(gate)).await })
        })
        .collect();
    await_waiting(&gate, MAX_CONCURRENT_SCRAPE).await;
    assert_eq!(
        guard.available_permits(),
        0,
        "屏障挂起期间（close 未完成）permit 绝不回落"
    );
    let fifth_started = Instant::now();
    let fifth = scrape_holding_permit(&db, Arc::clone(&guard), None).await;
    assert_eq!(fifth, Attempt::RejectedByGate, "第 5 路必须闸满拒绝");
    assert!(
        fifth_started.elapsed() < Duration::from_millis(200),
        "闸满必须**立即**返回，实测 {:?}",
        fifth_started.elapsed()
    );
    // 放行屏障：4 路 close 完成 → 任务退出 → permit 恢复 → 新抓取可进闸。
    gate.release_all(MAX_CONCURRENT_SCRAPE);
    join_all(handles).await;
    await_permits(guard.as_ref(), MAX_CONCURRENT_SCRAPE).await;
    drop(db.0.acquire().await.expect("业务必须始终能借到连接"));

    // 第 6 步（SHOULD）：取消变体（F4）——4 路抓取**确认真实启动**（已借出连接、
    // 查询超时后抵达受控关闭屏障）后才掐断调用方；再证明：
    // 1) 调用方取消后 permit 仍由后台 close 任务持有（available_permits 恒 0）；
    // 2) 并发槽位受**全局同一个 guard** 严格封顶（<= MAX_CONCURRENT_SCRAPE）；
    // 3) 放行后 permit 随后台 close 完成全部恢复，业务 acquire 不受影响。
    for wave in 0..2 {
        let cancel_gate = CloseGate::new();
        let handles: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let db = db.clone();
                let guard = Arc::clone(&guard); // 全局同一个闸，绝不 per-wave 新建
                let cancel_gate = Arc::clone(&cancel_gate);
                tokio::spawn(
                    async move { scrape_holding_permit(&db, guard, Some(cancel_gate)).await },
                )
            })
            .collect();
        // 确认真实启动：4 路都已借出连接、查询超时并挂起在 close 屏障上。
        await_waiting(&cancel_gate, MAX_CONCURRENT_SCRAPE).await;
        assert_eq!(guard.available_permits(), 0, "掐断前 4 路必须全部持 permit");
        let peak_during = db.0.size() as usize;
        assert!(
            peak_during <= baseline + MAX_CONCURRENT_SCRAPE,
            "取消变体的池占用同样受全局封顶：实测 {peak_during}（基线 {baseline}）"
        );
        // 掐断调用方（模拟客户端断线）：持有 permit 的是后台任务，不受影响。
        for handle in handles {
            handle.abort();
        }
        tokio::task::yield_now().await;
        assert_eq!(
            guard.available_permits(),
            0,
            "第 {wave} 轮：调用方被取消后，permit 必须仍由后台 close 任务持有"
        );
        let fifth = scrape_holding_permit(&db, Arc::clone(&guard), None).await;
        assert_eq!(
            fifth,
            Attempt::RejectedByGate,
            "第 {wave} 轮：后台 close 未完成前并发闸必须仍然封顶"
        );
        let started = Instant::now();
        let conn = tokio::time::timeout(Duration::from_millis(500), db.0.acquire())
            .await
            .unwrap_or_else(|_| panic!("第 {wave} 轮掐断后业务 acquire 仍须 < 500 ms"))
            .expect("掐断调用方后业务必须仍能借到连接");
        drop(conn);
        eprintln!("L4 取消变体：业务 acquire 耗时 {:?}", started.elapsed());
        // 放行：后台 close 完成 → permit 逐一归还 → 全局闸恢复满额，
        // 下一轮复用**同一个** guard 从满额起步。
        cancel_gate.release_all(MAX_CONCURRENT_SCRAPE);
        await_permits(guard.as_ref(), MAX_CONCURRENT_SCRAPE).await;
    }
}

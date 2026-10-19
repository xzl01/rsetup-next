//! L4 真实数据库验收（规范 §7.7）。全部 `#[ignore]`：只在操作员逐引擎显式授权的
//! 隔离开发库上运行，凭据仅进程内载入（`tests/common::required_test_db`），报告只出
//! 去敏数值（耗时、permit/size/idle 计数），绝不回显 DSN（`AGENTS.md` 凭据保护）。
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
    SCRAPE_TIMEOUT, SqlxMetricsSnapshotSource,
};
use sqlx::Row as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore, oneshot};

/// 大于 `SCRAPE_TIMEOUT`，保证每波都以查询超时收场。
const SLEEP_SECONDS: u64 = 3;
const WAVES: usize = 3;

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

/// §5.5.1 代码骨架的复制：显式 acquire 超时 → 立即 `close_on_drop()` → 超时包裹慢
/// 查询 → 结果折入变量 → **无条件** `await close()`。全程禁止 `?` 早退（同 Task 4）。
/// 对端生产实现：`src/metrics/collect.rs::snapshot_via`——任何对骨架的改动都必须
/// 同步到这里（两侧注释都写明对端位置）。
/// 返回 `true` 表示查询超时（生产侧映射 503）。
async fn guarded_snapshot(db: &DbPool, gate: Option<Arc<CloseGate>>) -> bool {
    let mut conn = match tokio::time::timeout(POOL_ACQUIRE_TIMEOUT, db.0.acquire()).await {
        // 借不到连接：没有连接产生，许可可立即释放。
        Ok(Ok(conn)) => conn,
        _ => return false,
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
    timed_out
}

/// 生产 handler 许可接线的测试侧等价物：许可由 `tokio::spawn` 的任务持有，任务返回
/// （= 连接已完全关闭）才随 Drop 释放；返回 `false` 对应生产侧 503（闸满或采集失败）。
async fn scrape_holding_permit(
    db: &DbPool,
    guard: Arc<Semaphore>,
    gate: Option<Arc<CloseGate>>,
) -> bool {
    let Ok(permit) = guard.clone().try_acquire_owned() else {
        return false;
    };
    let (tx, rx) = oneshot::channel();
    let db = db.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _ = tx.send(guarded_snapshot(&db, gate).await);
    });
    rx.await.unwrap_or(false)
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

async fn join_all(handles: Vec<tokio::task::JoinHandle<bool>>) -> Vec<bool> {
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.unwrap_or(false));
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
    for (label, expected) in [
        ("APPROVED", snapshot.devices.approved),
        ("PENDING", snapshot.devices.pending),
        ("REJECTED", snapshot.devices.rejected),
    ] {
        let rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devices WHERE CAST(admission_state AS BINARY) = CAST(? AS BINARY)",
        )
        .bind(label)
        .fetch_one(&db.0)
        .await
        .unwrap();
        assert_eq!(
            rows as u64, expected,
            "admission={label} 的聚合取值必须与独立 COUNT(*) 对账"
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
// §7.7 第 2 组：字节级分组 + PAD SPACE 对照
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires an operator-authorized isolated disposable dev DB with writable fixture tables; inserts only rows it deletes afterwards"]
async fn grouping_is_byte_exact_on_both_engines() {
    let db = common::required_test_db().await;
    // 两对分组键：只差大小写、只差尾部空格。列名以 `DESCRIBE devices` 为准，
    // 本用例只依赖 admission_state 可写入任意字节串（§5.2.2：库侧已无 CHECK 约束）。
    let ids = fixture_rows(&db, &["CASE", "case", "PADX", "PADX "]).await;

    let byte_rows = grouped_by(&db, DEVICE_AGGREGATE_SQL).await;
    assert!(
        byte_rows.iter().any(|(key, _)| key == "CASE")
            && byte_rows.iter().any(|(key, _)| key == "case"),
        "大小写必须各自成组（字节级分组生效）：{byte_rows:?}"
    );
    assert!(
        byte_rows.iter().any(|(key, _)| key == "PADX")
            && byte_rows.iter().any(|(key, _)| key == "PADX "),
        "尾部空格必须各自成组：{byte_rows:?}"
    );
    assert!(
        count_of(&byte_rows, "CASE") >= 1 && count_of(&byte_rows, "case") >= 1,
        "两行必须落在两个不同分组：{byte_rows:?}"
    );

    // PAD SPACE 陷阱对照：同一份数据用 utf8mb4_bin 分组会**折叠**尾部空格。
    let collation_rows = grouped_by(
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

    // 两类异常值都必须让映射返回 None → 经采集路径得到 503 fail-closed。
    let source = SqlxMetricsSnapshotSource::new(db.clone());
    let error = source
        .snapshot()
        .await
        .expect_err("未知 admission_state 取值必须让整次抓取失败（I4/G4）");
    let text = format!("{error:?}");
    for marker in ["CASE", "case", "PADX"] {
        assert!(!text.contains(marker), "去敏：错误不得回显取值原文：{text}");
    }

    // ONLY_FULL_GROUP_BY 下可执行；SHOW WARNINGS 不得含 CAST(... AS BINARY) 弃用告警。
    sqlx::query(DEVICE_AGGREGATE_SQL)
        .fetch_all(&db.0)
        .await
        .expect("ONLY_FULL_GROUP_BY 下必须可执行");
    let warnings = sqlx::query("SHOW WARNINGS").fetch_all(&db.0).await.unwrap();
    for row in &warnings {
        let mut text = String::new();
        for column in ["Level", "Message"] {
            if let Ok(bytes) = row.try_get::<&[u8], _>(column) {
                text.push_str(&String::from_utf8_lossy(bytes));
                text.push(' ');
            }
        }
        assert!(
            !text.to_lowercase().contains("deprecated"),
            "SHOW WARNINGS 含 CAST(... AS BINARY) 弃用告警：{text}"
        );
    }
    cleanup_rows(&db, &ids).await;
}

/// 插入给定 `admission_state` 取值的 fixture 行，返回主键以便收尾删除。
/// 列名取自 `migrations/0003_identity_application_integrity.sql` 的 `devices` 实表
/// （主键 `public_key BINARY(32)`、`display_name`/`review_decision`/`revision` 均
/// NOT NULL 且无默认值），与计划一致：本用例只依赖 `admission_state` 可写入任意字节串。
async fn fixture_rows(db: &DbPool, values: &[&'static str]) -> Vec<Vec<u8>> {
    let mut ids = Vec::new();
    for (index, value) in values.iter().enumerate() {
        // 32 字节主键（devices.public_key 为 BINARY(32)，定长 32 字节两侧兼容）。
        let mut id = format!("l4m{index}").into_bytes();
        id.resize(32, b'0');
        sqlx::query(
            "INSERT INTO devices (public_key, display_name, admission_state, review_decision, revision) \
             VALUES (?, ?, ?, 'none', 1) \
             ON DUPLICATE KEY UPDATE admission_state = VALUES(admission_state)",
        )
        .bind(&id)
        .bind(format!("l4-metrics-{index}"))
        .bind(*value)
        .execute(&db.0)
        .await
        .expect("fixture 行插入必须在已授权开发库内成功");
        ids.push(id);
    }
    ids
}

async fn cleanup_rows(db: &DbPool, ids: &[Vec<u8>]) {
    for id in ids {
        sqlx::query("DELETE FROM devices WHERE public_key = ?")
            .bind(id)
            .execute(&db.0)
            .await
            .expect("fixture 必须清理干净，不留残留行");
    }
}

async fn grouped_by(db: &DbPool, sql: &str) -> Vec<(String, i64)> {
    let rows = sqlx::query(sql).fetch_all(&db.0).await.unwrap();
    rows.iter()
        .map(|row| {
            let bucket: &[u8] = row.try_get("bucket").unwrap();
            let n: i64 = row.try_get("n").unwrap();
            (String::from_utf8_lossy(bucket).into_owned(), n)
        })
        .collect()
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
    let guard = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPE));
    let sentinel = Arc::new(Sentinel::default());
    let sentinel_task = Arc::clone(&sentinel).spawn(db.clone());

    // 第 1 步：3 波背靠背，每波 MAX+1 路（其中 1 路必然闸满）。
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
        let results = join_all(handles).await;
        assert!(
            results.iter().all(|ok| !ok),
            "第 {wave} 波必须全部定性为采集失败或闸满：{results:?}"
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
    assert!(!fifth, "第 5 路必须闸满拒绝（等价断言：始终拿不到许可）");
    assert!(
        fifth_started.elapsed() < Duration::from_millis(200),
        "闸满必须**立即**返回，实测 {:?}",
        fifth_started.elapsed()
    );
    // 放行屏障：4 路 close 完成 → 任务退出 → permit 恢复 → 新抓取可进闸。
    gate.release_all(MAX_CONCURRENT_SCRAPE);
    join_all(handles).await;
    for _ in 0..40 {
        if guard.available_permits() == MAX_CONCURRENT_SCRAPE {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(
        guard.available_permits(),
        MAX_CONCURRENT_SCRAPE,
        "close 完成后 permit 必须随任务退出恢复"
    );
    drop(db.0.acquire().await.expect("业务必须始终能借到连接"));

    // 第 6 步（SHOULD）：取消变体——4 路抓取刚发起就掐断调用方，再连发 2 波。
    for wave in 0..2 {
        let cancel_guard = Arc::new(Semaphore::new(MAX_CONCURRENT_SCRAPE));
        let handles: Vec<_> = (0..MAX_CONCURRENT_SCRAPE)
            .map(|_| {
                let db = db.clone();
                let guard = Arc::clone(&cancel_guard);
                tokio::spawn(async move { scrape_holding_permit(&db, guard, None).await })
            })
            .collect();
        for handle in handles {
            handle.abort(); // 模拟客户端断线：调用方被取消，采集任务不受影响
        }
        let started = Instant::now();
        let conn = tokio::time::timeout(Duration::from_millis(500), db.0.acquire())
            .await
            .unwrap_or_else(|_| panic!("第 {wave} 波掐断后业务 acquire 仍须 < 500 ms"))
            .expect("掐断调用方后业务必须仍能借到连接");
        drop(conn);
        eprintln!("L4 取消变体：业务 acquire 耗时 {:?}", started.elapsed());
    }
}

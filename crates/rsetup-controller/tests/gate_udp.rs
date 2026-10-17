//! Runtime Task 1b/1c 受控测试：
//! - Task 1b：真请求关联（correlate）、门禁纯函数与启动总预算（顶层用例 + `startup_budget`）；
//! - Task 1c：后台 `TimeGateDriver` 契约 1–8（源列表重读、周期/stale 复验、单调退避、跳钟失效、
//!   脱敏审计、共享权威快照与线程安全通知、换代契约 7、`start_time_gate_with` 装配接线）
//!   与 `wait_ready` RT-N4 断言（`driver_contracts`）。
//!
//! 全部使用进程内受控 fake（构造 48B NTP 响应、受控 `SntpResolver`/`SntpBindFuture`、
//! 本地 127.0.0.1 fake UDP 服务器、可编程 `TimeSampler`/`NtpSourceList`/`WallClock`/`Sleeper`/
//! `TimeAuditSink`），不访问真实网络/NTP 服务。
//! 时间控制 = 真实时间 + 短预算（300ms）+ `#[tokio::test]` 当前线程运行时；
//! `startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger`
//! 是本文件唯一 3s 量级用例（选定的单次上限本身为 3s，必须真实时间运行约 3s）。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use rsetup_controller::time::gate::{
    DEFAULT_NTP_MAX_RTT_MS, NTP_STARTUP_BUDGET, gate_should_close, next_backoff,
    select_request_timeout,
};
use rsetup_controller::time::ntp::NtpError;
use rsetup_controller::time::socket::{LocalStamp, NtpCorrelationRequest, correlate_sntp_response};

const REQUEST_TX_NONCE: u64 = 0xd00d_cafe_1234_5678; // 64位唯一发送时戳/nonce

/// 构造标准的 48 字节 SNTP 服务器响应。
/// Originate（字节 24..32）全量回显请求发送时戳/nonce；Receive (32..40) 与 Transmit (40..48) 为服务端时钟。
fn make_server_response(
    echoed_originate: u64,
    receive_timestamp: u64,
    transmit_timestamp: u64,
) -> [u8; 48] {
    let mut packet = [0u8; 48];
    packet[0] = (4u8 << 3) | 4u8; // LI=0, VN=4, Mode=4 (Server)
    packet[1] = 2; // stratum
    packet[12..16].copy_from_slice(b"LOCL"); // reference id
    packet[24..32].copy_from_slice(&echoed_originate.to_be_bytes());
    packet[32..40].copy_from_slice(&receive_timestamp.to_be_bytes());
    packet[40..48].copy_from_slice(&transmit_timestamp.to_be_bytes());
    packet
}

fn request(
    server_addr: SocketAddr,
    tx_nonce: u64,
    t1_wall: DateTime<Utc>,
    t1_mono: Instant,
) -> NtpCorrelationRequest {
    NtpCorrelationRequest {
        server_addr,
        request_tx: tx_nonce,
        t1_wall_utc: t1_wall,
        t1_mono,
    }
}

#[test]
fn sntp_response_from_unrequested_source_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let wrong_addr: SocketAddr = "203.0.113.7:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    // 构造合法的服务器报文
    let packet = make_server_response(
        REQUEST_TX_NONCE,
        0x0001_0000_0000_0000,
        0x0001_0000_0000_0001,
    );
    let t4 = LocalStamp {
        wall_utc: t1_wall + chrono::Duration::milliseconds(10),
        mono: t1_mono + Duration::from_millis(10),
    };
    let err = correlate_sntp_response(&packet, &req, wrong_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::SourceMismatch)));
}

#[test]
fn sntp_response_with_mismatched_originate_nonce_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    // Originate 错位 1 比特或回显他方时戳
    let packet = make_server_response(
        REQUEST_TX_NONCE ^ 0x1,
        0x0001_0000_0000_0000,
        0x0001_0000_0000_0001,
    );
    let t4 = LocalStamp {
        wall_utc: t1_wall + chrono::Duration::milliseconds(10),
        mono: t1_mono + Duration::from_millis(10),
    };
    let err = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::OriginateMismatch)));
}

#[test]
fn sntp_response_with_zero_receive_timestamp_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    let packet = make_server_response(REQUEST_TX_NONCE, 0, 0x0001_0000_0000_0001);
    let t4 = LocalStamp {
        wall_utc: t1_wall + chrono::Duration::milliseconds(10),
        mono: t1_mono + Duration::from_millis(10),
    };
    let err = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::ZeroReceiveTimestamp)));
}

#[test]
fn correlated_sample_is_accepted_despite_large_local_wall_offset() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    // 本地墙钟严重漂移（如比真实时间落后 10 年），只要关联正确、RTT 合格，仍必须接受样本
    let skewed_wall = Utc.timestamp_opt(1_420_070_400, 0).unwrap(); // 2015-01-01
    let req = request(target_addr, REQUEST_TX_NONCE, skewed_wall, t1_mono);

    // 服务端时间：2026-10-16（NTP 秒约 3_999_999_999）
    let server_ntp_sec = 3_999_999_999u64;
    let t2_raw = (server_ntp_sec << 32) | 0x4000_0000; // +250ms
    let t3_raw = (server_ntp_sec << 32) | 0x8000_0000; // +500ms
    let packet = make_server_response(REQUEST_TX_NONCE, t2_raw, t3_raw);

    // 往返单调耗时 50ms（合格）
    let t4 = LocalStamp {
        wall_utc: skewed_wall + chrono::Duration::milliseconds(50),
        mono: t1_mono + Duration::from_millis(50),
    };

    let sample = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS)
        .expect("correlated sample must be accepted even with large wall skew");
    assert_eq!(sample.rtt_ms, 50, "rtt measured by monotonic span");
    assert!(
        sample.offset_ms > 300_000_000 * 1000,
        "large positive offset expected due to 10yr skew"
    );
}

#[test]
fn rtt_over_local_monotonic_bound_is_rejected() {
    let target_addr: SocketAddr = "192.0.2.1:123".parse().unwrap();
    let t1_mono = Instant::now();
    let t1_wall = Utc::now();
    let req = request(target_addr, REQUEST_TX_NONCE, t1_wall, t1_mono);
    let packet = make_server_response(
        REQUEST_TX_NONCE,
        0x0001_0000_0000_0000,
        0x0001_0000_0000_0001,
    );
    // 单调耗时 2500ms，超过 DEFAULT_NTP_MAX_RTT_MS (2000ms)
    let t4 = LocalStamp {
        wall_utc: t1_wall + chrono::Duration::milliseconds(2500),
        mono: t1_mono + Duration::from_millis(2500),
    };
    let err = correlate_sntp_response(&packet, &req, target_addr, t4, DEFAULT_NTP_MAX_RTT_MS);
    assert!(matches!(err, Err(NtpError::RttTooLarge)));
}

#[test]
fn gate_budget_is_enforced_at_30_seconds() {
    // 00 §3 C-TIME 已确认约束：预算 30s 是强制上限，未取得关联样本也必须关闭。
    assert!(!gate_should_close(
        Duration::from_millis(29_999),
        NTP_STARTUP_BUDGET,
        false
    ));
    assert!(gate_should_close(
        Duration::from_secs(30),
        NTP_STARTUP_BUDGET,
        false
    ));
    assert!(gate_should_close(
        Duration::from_secs(31),
        NTP_STARTUP_BUDGET,
        false
    ));
    // 关联成功可提前关闭
    assert!(gate_should_close(
        Duration::from_millis(5),
        NTP_STARTUP_BUDGET,
        true
    ));
}

#[test]
fn background_backoff_is_bounded_and_monotonic() {
    assert_eq!(next_backoff(0), Duration::from_secs(1));
    assert_eq!(next_backoff(1), Duration::from_secs(2));
    assert_eq!(next_backoff(2), Duration::from_secs(4));
    assert!(next_backoff(6) <= Duration::from_secs(60));
    assert_eq!(next_backoff(30), Duration::from_secs(60));
}

#[test]
fn single_request_timeout_selection_caps_at_3s_and_clamps_to_remaining() {
    // 表驱动证明选择规则：min(configured, 05 §2 建议 3s 上限, 剩余预算)。
    // 规格 05 §2 原文是建议（非强制契约），本计划明确采用该建议作为实现策略。
    // 本纯 helper 由真实 `run_startup_sample` 消费（Step 3）；真实请求超时证据在
    // `startup_budget::startup_single_request_never_exceeds_3s_cap_when_configured_larger`
    // （事件驱动本地 pending/多源路径），不单以纯 helper 充当真实超时证据。
    let cases = [
        // (configured, remaining, expected)
        (
            Duration::from_secs(10),
            Duration::from_secs(30),
            Duration::from_secs(3),
        ), // configured > 3s：裁到 3s，不突破
        (
            Duration::from_secs(3),
            Duration::from_secs(30),
            Duration::from_secs(3),
        ), // configured == 上限：保持
        (
            Duration::from_secs(1),
            Duration::from_secs(30),
            Duration::from_secs(1),
        ), // configured < 3s：不放大
        (
            Duration::from_secs(10),
            Duration::from_millis(500),
            Duration::from_millis(500),
        ), // 剩余 < 3s：用剩余
        (
            Duration::from_secs(1),
            Duration::from_millis(200),
            Duration::from_millis(200),
        ), // 剩余 < configured < 3s：用剩余
        (Duration::from_secs(10), Duration::ZERO, Duration::ZERO), // 剩余耗尽：0
        (Duration::ZERO, Duration::from_secs(10), Duration::ZERO), // configured 为 0：0
    ];
    for (configured, remaining, expected) in cases {
        assert_eq!(
            select_request_timeout(configured, remaining),
            expected,
            "configured={configured:?} remaining={remaining:?}",
        );
    }
}

/// 启动总预算 `run_startup_sample` 受控测试：注入 bind/DNS future + 进程内 fake UDP 服务器，
/// 无外部 NTP、无新增依赖。时间控制 = 真实时间 + 短预算（300ms）+ `#[tokio::test]` 当前线程运行时；
/// 本模块不用 `start_paused`。执行命令按规范以进程级外部 watchdog（`timeout --kill-after=…`）包装，
/// 防回归引入忙循环导致挂起。
mod startup_budget {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use chrono::Utc;
    use rsetup_controller::time::gate::{DEFAULT_NTP_MAX_RTT_MS, run_startup_sample};
    use rsetup_controller::time::ntp::NtpError;
    use rsetup_controller::time::socket::{
        NTP_EPOCH_OFFSET_MS, SntpBindFuture, SntpResolveFuture, SntpResolver,
    };

    const BUDGET: Duration = Duration::from_millis(300); // 可测试短预算；生产默认 30s 不变
    const PER_REQ: Duration = Duration::from_secs(3); // 配置的单次请求超时；实际选择为 min(configured, 3s 上限, remaining)
    const SLOW_CI_SLACK: Duration = Duration::from_millis(500);

    /// 被 drop 时标记 “DNS future 已被取消”。
    struct DropFlag(Arc<AtomicBool>);
    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// 可注入 DNS future 选择器：`Resolve` 立即解析；`Pending` 永不完成（带 drop 标记）。
    #[derive(Clone, Copy)]
    enum DnsBehavior {
        Resolve,
        Pending,
    }

    struct SpyResolver {
        behavior: DnsBehavior,
        server_addr: SocketAddr,
        calls: Arc<AtomicUsize>,
        dropped: Arc<AtomicBool>,
    }

    impl SntpResolver for SpyResolver {
        fn resolve<'a>(&'a self, _host: &'a str) -> SntpResolveFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.behavior {
                DnsBehavior::Resolve => {
                    let addr = self.server_addr;
                    Box::pin(async move { Ok(addr) })
                }
                DnsBehavior::Pending => {
                    let flag = self.dropped.clone();
                    Box::pin(async move {
                        let _guard = DropFlag(flag);
                        std::future::pending::<()>().await;
                        Err(NtpError::Io)
                    })
                }
            }
        }
    }

    /// 构造 spy resolver，返回查询 spy 与 drop 标记句柄。
    fn spy(
        behavior: DnsBehavior,
        addr: SocketAddr,
    ) -> (SpyResolver, Arc<AtomicUsize>, Arc<AtomicBool>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let dropped = Arc::new(AtomicBool::new(false));
        (
            SpyResolver {
                behavior,
                server_addr: addr,
                calls: calls.clone(),
                dropped: dropped.clone(),
            },
            calls,
            dropped,
        )
    }

    /// 受控 fake NTP 服务器：收 48B 请求（客户端地址取自 recv_from），全量回显 Originate，
    /// 回标准 48B 响应（t3 = t2，本地短驻留标准响应）。
    async fn fake_ntp_server(sock: tokio::net::UdpSocket) {
        let mut buf = [0u8; 512];
        let (len, client) = sock.recv_from(&mut buf).await.expect("fake server recv");
        assert_eq!(len, 48, "请求必须是 48B SNTP 报文");
        let nonce = u64::from_be_bytes(buf[40..48].try_into().unwrap());
        let now_ms = Utc::now().timestamp_millis();
        let t2 = ((now_ms / 1000) as u64).wrapping_add(NTP_EPOCH_OFFSET_MS as u64 / 1000) << 32;
        // t3 = t2：本地短驻留标准响应；SNTP 的 RTT 上限由本地 T1/T4 单调跨度严格度量。
        let t3 = t2;
        let resp = super::make_server_response(nonce, t2, t3);
        sock.send_to(&resp, client).await.expect("fake server send");
    }

    /// 受控 bind future：本地 127.0.0.1 UDP socket，bind 计数 spy。
    fn bind_spy() -> (SntpBindFuture, Arc<AtomicUsize>) {
        let binds = Arc::new(AtomicUsize::new(0));
        let bind_calls = binds.clone();
        (
            Box::pin(async move {
                bind_calls.fetch_add(1, Ordering::SeqCst);
                tokio::net::UdpSocket::bind("127.0.0.1:0")
                    .await
                    .map_err(|_| NtpError::Io)
            }),
            binds,
        )
    }

    /// 按源地址 DNS 选择器：`srv0` → 静默（绑定但永不响应），`srv1` → 应答 fake 服务器；
    /// 供“源 0 单次请求被 3s 上限裁剪、源 1 在预算内完成”的多源真实路径使用。
    struct SplitResolver {
        addrs: Vec<(String, SocketAddr)>,
        calls: Arc<AtomicUsize>,
    }

    impl SntpResolver for SplitResolver {
        fn resolve<'a>(&'a self, host: &'a str) -> SntpResolveFuture<'a> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let addr = self
                .addrs
                .iter()
                .find(|(h, _)| h == host)
                .map(|(_, a)| *a)
                .unwrap();
            Box::pin(async move { Ok(addr) })
        }
    }

    #[tokio::test]
    async fn startup_success_returns_sample_and_source_before_deadline() {
        // 正例：首源立即返回合格样本 → 门禁必须提前返回，query spy > 0。
        let server_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            fake_ntp_server(server_sock).await;
        });
        let (bind, binds) = bind_spy();
        let (resolver, calls, _dropped) = spy(DnsBehavior::Resolve, server_addr);
        let servers = vec!["srv0".to_string(), "srv1".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(
            bind,
            &resolver,
            &servers,
            PER_REQ,
            BUDGET,
            DEFAULT_NTP_MAX_RTT_MS,
            now,
        )
        .await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        let (sample, source) = res.expect("deadline 前应获得合格样本");
        assert_eq!(source, "srv0", "样本来自首个源");
        assert!(sample.uncertainty_ms >= 1, "不确定度至少 1ms");
        assert!(calls.load(Ordering::SeqCst) > 0, "query spy 必须被调用");
        assert_eq!(binds.load(Ordering::SeqCst), 1, "socket 只绑定一次");
        assert!(elapsed < BUDGET, "成功路径必须提前返回，不等待整个预算");
        server_task.abort();
    }

    #[tokio::test]
    async fn startup_pending_dns_is_cancelled_by_total_budget() {
        // DNS 永不完成：必须被总预算取消（drop），门禁以 None 关闭。
        let (bind, _binds) = bind_spy();
        let (resolver, _calls, dropped) =
            spy(DnsBehavior::Pending, "127.0.0.1:123".parse().unwrap());
        let servers = vec!["srv0".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(
            bind,
            &resolver,
            &servers,
            PER_REQ,
            BUDGET,
            DEFAULT_NTP_MAX_RTT_MS,
            now,
        )
        .await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        assert!(res.is_none(), "pending DNS 在总预算内不可能产出样本");
        assert!(elapsed >= BUDGET, "必须等到绝对 deadline，不得提前返回");
        assert!(
            dropped.load(Ordering::SeqCst),
            "预算到期必须取消（drop）pending DNS future"
        );
    }

    #[tokio::test]
    async fn startup_multi_source_total_budget_is_not_extended() {
        // 三源全 pending：若按源重建 deadline 则总时长 ≈ 3×3s；同一绝对 deadline 下总时长 ≈ BUDGET。
        let (bind, _binds) = bind_spy();
        let (resolver, calls, _dropped) =
            spy(DnsBehavior::Pending, "127.0.0.1:123".parse().unwrap());
        let servers = vec!["a".to_string(), "b".to_string(), "c".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(
            bind,
            &resolver,
            &servers,
            PER_REQ,
            BUDGET,
            DEFAULT_NTP_MAX_RTT_MS,
            now,
        )
        .await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        assert!(res.is_none());
        assert!(elapsed >= BUDGET, "首源占满剩余预算，门禁在 deadline 关闭");
        assert!(elapsed < BUDGET + SLOW_CI_SLACK, "总预算不得按源扩容");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "首源占满全部剩余预算，后续源不再查询"
        );
    }

    #[tokio::test]
    async fn startup_empty_source_waits_deadline_without_busy_loop() {
        // 空源列表：必须有界等待（唯一 await）至绝对 deadline 后返回 None，不忙循环。
        let (bind, binds) = bind_spy();
        let (resolver, calls, _dropped) =
            spy(DnsBehavior::Resolve, "127.0.0.1:123".parse().unwrap());

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(
            bind,
            &resolver,
            &[],
            PER_REQ,
            BUDGET,
            DEFAULT_NTP_MAX_RTT_MS,
            now,
        )
        .await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        assert!(res.is_none(), "空源列表视不可用，得不到样本");
        assert!(elapsed >= BUDGET, "空源必须等到绝对 deadline 再回退");
        assert!(elapsed < BUDGET + SLOW_CI_SLACK, "有界等待，不超发");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "空源不查询");
        assert_eq!(
            binds.load(Ordering::SeqCst),
            1,
            "socket 只绑定一次，无每轮重绑（忙循环标记）"
        );
    }

    #[tokio::test]
    async fn startup_single_request_never_exceeds_3s_cap_when_configured_larger() {
        // configured = 10s（> 05 §2 选定的 3s 上限），总预算 5s（> 上限），真实时间、无 paused clock：
        // 源 0 绑定但永不响应，源 1 为标准 fake 服务器（t3 = t2 短驻留响应）。
        // 若单次请求选择不裁上限，源 0 吞掉整个 5s 预算 → None；
        // 裁上限后源 0 在 ~3s 被截断，源 1 在剩余预算内完成 → Some("srv1")。
        // 本模块唯一 3s 量级用例（上限本身为 3s，需真实时间运行约 3s）。
        let silent_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let silent_addr = silent_sock.local_addr().unwrap();
        let server_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_sock.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            fake_ntp_server(server_sock).await;
        });
        let (bind, _binds) = bind_spy();
        let calls = Arc::new(AtomicUsize::new(0));
        let resolver = SplitResolver {
            addrs: vec![
                ("srv0".to_string(), silent_addr),
                ("srv1".to_string(), server_addr),
            ],
            calls: calls.clone(),
        };
        let servers = vec!["srv0".to_string(), "srv1".to_string()];

        let now = tokio::time::Instant::now();
        let res = run_startup_sample(
            bind,
            &resolver,
            &servers,
            Duration::from_secs(10),
            Duration::from_secs(5),
            DEFAULT_NTP_MAX_RTT_MS,
            now,
        )
        .await;
        let elapsed = tokio::time::Instant::now().duration_since(now);

        let (_sample, source) = res.expect("源 0 单次请求被 3s 上限截断，源 1 必须在预算内成功");
        assert_eq!(source, "srv1", "样本必须来自源 1");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "每源恰好解析一次 DNS");
        assert!(
            elapsed >= Duration::from_secs(3),
            "源 0 单次请求必须真实等到 3s 上限被截断（上限生效而非 0）"
        );
        assert!(
            elapsed < Duration::from_secs(3) + SLOW_CI_SLACK,
            "若不裁上限，源 0 会吞掉整个 5s 预算"
        );
        server_task.abort();
    }
}

/// Runtime Task 1c 受控测试：后台 `TimeGateDriver` 契约 1–8 与 `wait_ready` RT-N4 断言。
///
/// 时间确定性策略（本仓库 tokio 未启用 `test-util`，无 `pause`/`advance` 虚拟时钟，
/// Cargo 依赖冻结不可新增 feature，故 driver 的全部时间/IO 交互均为注入点）：
/// - `RecordingSleeper` 记录 driver **请求**的等待时长（`revalidate_interval`、`next_backoff`），
///   真实只等待 `min(请求, 5ms)`：节奏断言读记录值，绝不真实等待 1s..60s；
/// - `FakeWallClock` 按“第 N 次读取”注入回跳/前跳，跳钟判定不依赖真实系统时钟；
/// - 证据时效（`max_evidence_age_ms`）用毫秒级配置 + 真实短等待触发；
/// - `wait_ready` 的 30s 上限由 `TimeGateHandle::with_ready_timeout` 注入 300ms 短超时，
///   生产默认 30s 由 `READY_WAIT_TIMEOUT` 常量断言覆盖。
///
/// 全部 fake 进程内运行，不访问真实网络/NTP；执行按规范挂进程级 watchdog（`timeout --kill-after=5s 35s`）。
mod driver_contracts {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    use rsetup_controller::time::evidence::TimeQuality;
    use rsetup_controller::time::gate::{
        BACKOFF_MAX, BACKOFF_MIN, JUMP_FORWARD_MAX, NtpSourceList, READY_WAIT_TIMEOUT, SleepFuture,
        Sleeper, SourceListFuture, TimeAuditEvent, TimeAuditSink, TimeGateConfig, TimeGateDeps,
        TimeGateDriver, TimeGateHandle, TimeSampleFuture, TimeSampler, WallClock, next_backoff,
        start_time_gate_with,
    };
    use rsetup_controller::time::socket::NtpSample;

    /// 受控源标识：形如真实 NTP 源地址/主机，用于断言审计事件绝不泄露源地址/端口/nonce。
    const SOURCE_A: &str = "ntp-redacted-a.invalid";
    const SOURCE_B: &str = "198.51.100.7:123";
    const FAKE_ADDR: &str = "203.0.113.9:123";
    const FAKE_NONCE: &str = "deadbeefcafe1234";
    /// 受控就绪等待超时（RT-N4 生产默认 30s 由常量断言覆盖）。
    const READY_TIMEOUT: Duration = Duration::from_millis(300);
    /// `RecordingSleeper` 真实等待上限：请求时长只记录，真实最多等 5ms。
    const SLEEP_CAP: Duration = Duration::from_millis(5);

    // --------------------------------------------------------------------------
    // 可编程 fake
    // --------------------------------------------------------------------------

    #[derive(Clone, Copy, Debug)]
    struct Step {
        /// 采样前真实等待（模拟慢 DNS/慢响应）。
        delay_ms: u64,
        /// `Some(shift)` → 成功样本（reference_utc = 当前墙钟 + shift 毫秒）；`None` → 失败。
        ok_shift_ms: Option<i64>,
    }

    impl Step {
        const fn ok(shift_ms: i64) -> Self {
            Self {
                delay_ms: 0,
                ok_shift_ms: Some(shift_ms),
            }
        }
        const fn slow_ok(delay_ms: u64) -> Self {
            Self {
                delay_ms,
                ok_shift_ms: Some(0),
            }
        }
        const fn fail() -> Self {
            Self {
                delay_ms: 0,
                ok_shift_ms: None,
            }
        }
    }

    #[derive(Default)]
    struct SamplerState {
        queue: VecDeque<Step>,
        sticky: Option<Step>,
        calls: usize,
        seen: Vec<Vec<String>>,
    }

    /// 可编程采样器：队列逐步消费，耗尽后重复 `sticky`；记录每次调用时刻看到的源列表。
    struct FakeSampler {
        state: Mutex<SamplerState>,
    }

    impl FakeSampler {
        fn new(steps: Vec<Step>) -> Arc<Self> {
            let sticky = steps.last().copied();
            Arc::new(Self {
                state: Mutex::new(SamplerState {
                    queue: steps.into(),
                    sticky,
                    ..Default::default()
                }),
            })
        }
        fn set_repeated(&self, step: Step) {
            let mut state = self.state.lock().unwrap();
            state.queue.clear();
            state.sticky = Some(step);
        }
        /// 先按队列失败 `failures` 次，之后固定成功（用于确定性的退避尝试计数）。
        fn succeed_after(&self, failures: usize) {
            let mut state = self.state.lock().unwrap();
            state.queue = (0..failures).map(|_| Step::fail()).collect();
            state.sticky = Some(Step::ok(0));
        }
        fn calls(&self) -> usize {
            self.state.lock().unwrap().calls
        }
        fn seen(&self, index: usize) -> Vec<String> {
            let state = self.state.lock().unwrap();
            state.seen.get(index).cloned().unwrap_or_default()
        }
    }

    impl TimeSampler for FakeSampler {
        fn sample<'a>(&'a self, servers: &'a [String]) -> TimeSampleFuture<'a> {
            let (step, idx) = {
                let mut state = self.state.lock().unwrap();
                let idx = state.calls;
                state.calls += 1;
                state.seen.push(servers.to_vec());
                let step = match state.queue.pop_front() {
                    Some(step) => step,
                    None => state.sticky.unwrap_or(Step::fail()),
                };
                (step, idx)
            };
            let source = servers
                .first()
                .cloned()
                .unwrap_or_else(|| "srv-none".to_string());
            Box::pin(async move {
                if step.delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(step.delay_ms)).await;
                }
                step.ok_shift_ms.map(|shift| {
                    let millis = Utc::now().timestamp_millis() + shift;
                    let reference_utc = Utc
                        .timestamp_millis_opt(millis)
                        .single()
                        .expect("受控参考时戳必须合法");
                    (
                        NtpSample {
                            offset_ms: 1_000 + 7 * idx as i64,
                            rtt_ms: 20,
                            reference_utc,
                            uncertainty_ms: 10,
                        },
                        source,
                    )
                })
            })
        }
    }

    /// 可修改源列表（契约 1 重读断言）。
    struct FakeSources {
        list: Mutex<Vec<String>>,
    }

    impl FakeSources {
        fn new(list: Vec<String>) -> Arc<Self> {
            Arc::new(Self {
                list: Mutex::new(list),
            })
        }
        fn set(&self, list: Vec<String>) {
            *self.list.lock().unwrap() = list;
        }
    }

    impl NtpSourceList for FakeSources {
        fn read(&self) -> SourceListFuture<'_> {
            let list = self.list.lock().unwrap().clone();
            Box::pin(async move { list })
        }
    }

    /// 可编程墙钟：第 `read_index` 次读取时把偏移量加上 `delta_ms`（负值 = 回跳）。
    struct FakeWallClock {
        base: SystemTime,
        offset_ms: AtomicI64,
        script: Mutex<Vec<(usize, i64)>>,
        reads: AtomicUsize,
    }

    impl FakeWallClock {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                base: SystemTime::now(),
                offset_ms: AtomicI64::new(0),
                script: Mutex::new(Vec::new()),
                reads: AtomicUsize::new(0),
            })
        }
        fn script_at(&self, read_index: usize, delta_ms: i64) {
            self.script.lock().unwrap().push((read_index, delta_ms));
        }
    }

    impl WallClock for FakeWallClock {
        fn now(&self) -> SystemTime {
            let read = self.reads.fetch_add(1, Ordering::SeqCst);
            let mut offset = self.offset_ms.load(Ordering::SeqCst);
            for (index, delta) in self.script.lock().unwrap().iter() {
                if *index == read {
                    offset = offset.saturating_add(*delta);
                }
            }
            self.offset_ms.store(offset, Ordering::SeqCst);
            if offset >= 0 {
                self.base + Duration::from_millis(offset as u64)
            } else {
                self.base - Duration::from_millis(offset.unsigned_abs())
            }
        }
    }

    /// 记录 driver 请求的等待时长（真实只等 `min(请求, SLEEP_CAP)`）。
    #[derive(Default)]
    struct RecordingSleeper {
        sleeps: Mutex<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }
        fn sleeps(&self) -> Vec<Duration> {
            self.sleeps.lock().unwrap().clone()
        }
        fn count(&self) -> usize {
            self.sleeps.lock().unwrap().len()
        }
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
            self.sleeps.lock().unwrap().push(duration);
            Box::pin(tokio::time::sleep(duration.min(SLEEP_CAP)))
        }
    }

    /// 内存审计 sink（契约 5 事件序列断言）。
    #[derive(Default)]
    struct MemAuditSink {
        events: Mutex<Vec<TimeAuditEvent>>,
    }

    impl MemAuditSink {
        fn events(&self) -> Vec<TimeAuditEvent> {
            self.events.lock().unwrap().clone()
        }
        fn find(&self, kind: &str) -> Option<TimeAuditEvent> {
            self.events().into_iter().find(|event| event.kind == kind)
        }
        fn count_of(&self, kind: &str) -> usize {
            self.events()
                .iter()
                .filter(|event| event.kind == kind)
                .count()
        }
    }

    impl TimeAuditSink for MemAuditSink {
        fn record(&self, event: TimeAuditEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    // --------------------------------------------------------------------------
    // 装配与轮询 helper
    // --------------------------------------------------------------------------

    struct Harness {
        handle: TimeGateHandle,
        sampler: Arc<FakeSampler>,
        sources: Arc<FakeSources>,
        wall: Arc<FakeWallClock>,
        sleeper: Arc<RecordingSleeper>,
        audit: Arc<MemAuditSink>,
    }

    /// `startup = true`：driver 先做启动采样（契约 8）；`false`：仅后台循环，保持初始 Stale。
    fn build(steps: Vec<Step>, revalidate_ms: u64, max_age_ms: u64, startup: bool) -> Harness {
        let config = TimeGateConfig {
            servers: vec![SOURCE_A.to_string()],
            request_timeout: Duration::from_millis(50),
            startup_budget: Duration::from_millis(200),
            max_rtt_ms: 2_000,
            revalidate_interval: Duration::from_millis(revalidate_ms),
            max_evidence_age_ms: max_age_ms,
        };
        let sampler = FakeSampler::new(steps);
        let sources = FakeSources::new(vec![SOURCE_A.to_string()]);
        let wall = FakeWallClock::new();
        let sleeper = RecordingSleeper::new();
        let audit = Arc::new(MemAuditSink::default());
        let handle = TimeGateHandle::new(&config, Uuid::new_v4()).with_ready_timeout(READY_TIMEOUT);
        let sampler_dep: Arc<dyn TimeSampler> = sampler.clone();
        let sources_dep: Arc<dyn NtpSourceList> = sources.clone();
        let wall_dep: Arc<dyn WallClock> = wall.clone();
        let sleeper_dep: Arc<dyn Sleeper> = sleeper.clone();
        let audit_dep: Arc<dyn TimeAuditSink> = audit.clone();
        let driver = TimeGateDriver::new(
            &handle,
            config,
            TimeGateDeps {
                sampler: sampler_dep,
                sources: sources_dep,
                wall_clock: wall_dep,
                sleeper: sleeper_dep,
                audit: audit_dep,
            },
        );
        tokio::spawn(async move {
            if startup {
                driver.run().await;
            } else {
                driver.run_background().await;
            }
        });
        Harness {
            handle,
            sampler,
            sources,
            wall,
            sleeper,
            audit,
        }
    }

    fn harness(steps: Vec<Step>, revalidate_ms: u64, max_age_ms: u64) -> Harness {
        build(steps, revalidate_ms, max_age_ms, true)
    }

    fn harness_background(steps: Vec<Step>, revalidate_ms: u64, max_age_ms: u64) -> Harness {
        build(steps, revalidate_ms, max_age_ms, false)
    }

    /// 取某类审计事件的**最后**一条（driver 持续运行，断言只看最新状态）。
    fn last_event(h: &Harness, kind: &str) -> TimeAuditEvent {
        h.audit
            .events()
            .into_iter()
            .rev()
            .find(|event| event.kind == kind)
            .expect("审计事件")
    }

    /// 真实短轮询（2ms 步进）：只等被测行为发生，不做精确计时断言。
    async fn wait_until<F: FnMut() -> bool>(mut cond: F, timeout: Duration, what: &str) {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if cond() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "超时未满足条件：{what}"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    // --------------------------------------------------------------------------
    // A. driver 契约 1–6 与 Stale 分支（计划 6 个用例）
    // --------------------------------------------------------------------------

    /// 契约 1 + 2：每轮循环开头重读源列表；`revalidate_interval` 到期恰好触发一次复验，
    /// 成功复验刷新证据并重置 `sample_age_ms`。
    #[tokio::test]
    async fn driver_revalidates_at_revalidate_interval() {
        let h = harness(vec![Step::ok(0)], 20, 60_000);
        wait_until(
            || h.sampler.calls() >= 1,
            Duration::from_secs(2),
            "启动采样",
        )
        .await;
        let evidence = h.handle.current().await;
        assert_eq!(
            evidence.quality,
            TimeQuality::NtpValid,
            "启动采样成功应转 NtpValid"
        );
        assert!(h.handle.is_ready(), "启动采样成功应转 ready");
        assert_eq!(h.sampler.seen(0), vec![SOURCE_A.to_string()]);

        // 等 driver 进入复验等待（本轮源列表已在等待前重读完毕）后再改源列表
        wait_until(
            || h.sleeper.count() >= 1,
            Duration::from_secs(2),
            "进入复验等待",
        )
        .await;
        h.sources.set(vec![SOURCE_B.to_string()]);
        wait_until(
            || h.sampler.calls() >= 2,
            Duration::from_secs(2),
            "周期复验",
        )
        .await;
        assert_eq!(
            h.sampler.seen(1),
            vec![SOURCE_A.to_string()],
            "本轮复验使用进入等待前重读的源列表"
        );
        wait_until(
            || h.sampler.calls() >= 3,
            Duration::from_secs(2),
            "源列表重读",
        )
        .await;
        assert_eq!(
            h.sampler.seen(2),
            vec![SOURCE_B.to_string()],
            "契约 1：下一轮必须重读最新源列表"
        );

        // 契约 2：每次复验前恰好一次 revalidate_interval 等待（断言请求时长，非真实耗时）
        let sleeps = h.sleeper.sleeps();
        let calls = h.sampler.calls();
        assert!(
            (calls - 1..=calls).contains(&sleeps.len()),
            "每次复验前恰好一次等待（至多一次在途），无忙循环：calls={calls} sleeps={}",
            sleeps.len()
        );
        assert!(
            sleeps.iter().all(|d| *d == Duration::from_millis(20)),
            "等待时长必须 = revalidate_interval：{sleeps:?}"
        );

        // 复验成功刷新证据并重置 sample_age_ms
        let refreshed = h.handle.current().await;
        assert_eq!(refreshed.source.as_deref(), Some(SOURCE_B));
        assert!(
            refreshed.sample_age_ms.unwrap() < 200,
            "成功复验重置 sample_age_ms = 0（仅随锚点推进）：{:?}",
            refreshed.sample_age_ms
        );
        let events = h.audit.events();
        assert_eq!(events[0].kind, "ntp_startup");
        let revalidated = events
            .iter()
            .find(|event| event.kind == "ntp_revalidated")
            .expect("复验审计事件");
        assert_eq!(
            (revalidated.from, revalidated.to),
            (TimeQuality::NtpValid, TimeQuality::NtpValid)
        );
        assert_eq!(revalidated.reason, "ntp_revalidated");
        assert_eq!(
            revalidated.offset_ms,
            Some(1_007),
            "证据 offset 刷新为第 2 次样本"
        );
    }

    /// 契约 2：复验失败且 `sample_age_ms > max_evidence_age_ms` → 降级 SystemFallback、attempt = 0。
    #[tokio::test]
    async fn driver_evidence_over_max_age_invalidates_to_fallback() {
        // 启动成功 → NtpValid；此后复验持续失败：仅靠锚点经过时间（无新写入）令 age 越过上限
        let h = harness(vec![Step::ok(0), Step::fail()], 10, 60);
        wait_until(
            || h.audit.count_of("evidence_stale") >= 1,
            Duration::from_secs(3),
            "证据超龄失效",
        )
        .await;
        let events = h.audit.events();
        let stale_at = events
            .iter()
            .position(|event| event.kind == "evidence_stale")
            .expect("evidence_stale 事件");
        assert_eq!(
            stale_at, 1,
            "复验失败但未超龄期间必须保留证据、不写变迁审计（此前仅启动一条）"
        );
        let stale = &events[stale_at];
        assert_eq!(stale.from, TimeQuality::NtpValid);
        assert_eq!(stale.to, TimeQuality::SystemFallback);
        assert_eq!(stale.reason, "evidence_stale");
        assert_eq!(stale.attempt, 0, "契约 2：失效降级 attempt = 0");
        assert_eq!(
            (stale.offset_ms, stale.uncertainty_ms),
            (None, None),
            "失效清空 offset/uncertainty"
        );
        assert!(
            h.handle.background_retry_active(),
            "降级后必须继续后台退避重试（attempt 复位由失效审计事件的 attempt = 0 证明）"
        );
        let evidence = h.handle.current().await;
        assert_eq!(
            evidence.quality,
            TimeQuality::SystemFallback,
            "driver 持久降级（非仅读取侧动态降级）"
        );
        assert_eq!(evidence.source, None, "失效清空 source");

        // 契约 7（M1）：失效后重新恢复采样时，旧源与旧锚点已被清空、连续性不可证明，
        // 必须保守换代（RT-R13-2 fail-closed）：新 clock_epoch + 紧跟的换代审计，
        // 使失效前创建的 held 任务在恢复后被判定为参考系断裂，而非同纪元连续。
        let epoch_before_stale = stale.clock_epoch;
        assert_eq!(
            h.audit.count_of("clock_epoch_renewed"),
            0,
            "失效本身不换代（换代由恢复承接）"
        );
        h.sampler.succeed_after(1);
        wait_until(
            || h.audit.count_of("ntp_recovered") >= 1,
            Duration::from_secs(3),
            "退避后恢复采样",
        )
        .await;
        let events = h.audit.events();
        let recovered = events
            .iter()
            .rev()
            .find(|event| event.kind == "ntp_recovered")
            .expect("恢复审计事件");
        let renewed = events
            .iter()
            .rev()
            .find(|event| event.kind == "clock_epoch_renewed")
            .expect("M1：失效后恢复必须写换代审计");
        assert_ne!(
            recovered.clock_epoch, epoch_before_stale,
            "M1：evidence_stale 失效后恢复不得沿用旧 clock_epoch"
        );
        assert_eq!(
            renewed.clock_epoch, recovered.clock_epoch,
            "换代审计与恢复事件同纪元"
        );
        assert_eq!(
            (recovered.from, recovered.to, recovered.reason),
            (
                TimeQuality::SystemFallback,
                TimeQuality::NtpValid,
                "ntp_recovered"
            )
        );
        let after = h.handle.current().await;
        assert_eq!(after.quality, TimeQuality::NtpValid);
        assert_eq!(
            after.clock_epoch, recovered.clock_epoch,
            "换代后 handle 读到新纪元"
        );
        assert_ne!(
            after.clock_epoch, epoch_before_stale,
            "权威快照的纪元必须已更新"
        );
    }

    /// Stale 分支：不可信但不放行、不降级；按 `revalidate_interval` 复验，成功转 NtpValid。
    #[tokio::test]
    async fn driver_stale_is_untrusted_and_revalidated_without_release() {
        // run_background：保持 start_time_gate 的初始 Stale 态，driver 只跑后台循环
        let h = harness_background(vec![Step::fail()], 15, 60_000);
        wait_until(
            || h.sampler.calls() >= 3,
            Duration::from_secs(2),
            "Stale 周期复验",
        )
        .await;
        assert_eq!(
            h.handle.current().await.quality,
            TimeQuality::Stale,
            "契约 2：Stale 维持当前 gate 状态，不降级放行"
        );
        assert!(!h.handle.is_ready(), "Stale 不可信，不得转 ready 放行");
        assert!(
            h.audit.events().is_empty(),
            "维持 Stale 不写变迁审计（无证据可失效）"
        );
        let sleeps = h.sleeper.sleeps();
        assert!(
            sleeps.iter().all(|d| *d == Duration::from_millis(15)),
            "Stale 按 revalidate_interval 复验，不走退避：{sleeps:?}"
        );

        h.sampler.set_repeated(Step::ok(0));
        wait_until(
            || h.handle.is_ready(),
            Duration::from_secs(2),
            "Stale→NtpValid",
        )
        .await;
        assert_eq!(h.handle.current().await.quality, TimeQuality::NtpValid);
        let events = h.audit.events();
        assert_eq!(events.len(), 1, "Stale→NtpValid 恰好一条变迁审计");
        assert_eq!(
            (events[0].kind, events[0].from, events[0].to),
            (
                "stale_revalidated",
                TimeQuality::Stale,
                TimeQuality::NtpValid
            )
        );
    }

    /// 契约 6：每次变迁后 `TimeGateHandle::current()` 与 driver 写入的权威快照一致；
    /// 转 ready 后 `wait_ready` 返回同一快照。
    #[tokio::test]
    async fn driver_handle_current_matches_driver_snapshot() {
        let h = harness(vec![Step::ok(0)], 15, 60_000);
        wait_until(
            || h.handle.is_ready(),
            Duration::from_secs(2),
            "启动转 ready",
        )
        .await;
        let startup = h.audit.find("ntp_startup").expect("启动审计事件");
        let evidence = h.handle.current().await;
        assert_eq!(evidence.quality, startup.to);
        assert_eq!(
            evidence.clock_epoch, startup.clock_epoch,
            "契约 6：handle 读到的纪元 = driver 写入的权威快照"
        );
        assert_eq!(evidence.offset_ms, startup.offset_ms);
        assert_eq!(
            evidence
                .uncertainty_ms
                .map(i64::try_from)
                .transpose()
                .unwrap(),
            startup.uncertainty_ms
        );
        assert_eq!(evidence.reference_utc, startup.reference_utc);
        assert_eq!(evidence.source.as_deref(), Some(SOURCE_A));

        // 同源平滑复验：沿用旧 clock_epoch
        wait_until(
            || h.audit.count_of("ntp_revalidated") >= 1,
            Duration::from_secs(2),
            "同源复验",
        )
        .await;
        let smooth = h.audit.find("ntp_revalidated").expect("同源复验审计");
        assert_eq!(
            smooth.clock_epoch, startup.clock_epoch,
            "RT-R12-2：同源且外推偏差 ≤1s 沿用旧纪元"
        );

        // 来源切换 → 换代 + 换代审计
        wait_until(
            || h.sleeper.count() >= 1,
            Duration::from_secs(2),
            "进入复验等待",
        )
        .await;
        h.sources.set(vec![SOURCE_B.to_string()]);
        wait_until(
            || h.audit.count_of("clock_epoch_renewed") >= 1,
            Duration::from_secs(2),
            "来源切换换代",
        )
        .await;
        let events = h.audit.events();
        let switched = events
            .iter()
            .rev()
            .find(|event| event.kind == "ntp_revalidated")
            .expect("切换后的复验审计");
        let renewed = events
            .iter()
            .rev()
            .find(|event| event.kind == "clock_epoch_renewed")
            .expect("换代审计");
        assert_ne!(
            switched.clock_epoch, startup.clock_epoch,
            "RT-R12-2 / RT-R13-2：来源切换必须换代"
        );
        assert_eq!(renewed.clock_epoch, switched.clock_epoch);
        let after = h.handle.current().await;
        assert_eq!(
            after.clock_epoch, switched.clock_epoch,
            "换代后 handle 读到新纪元"
        );
        assert_eq!(after.source.as_deref(), Some(SOURCE_B));
        assert_eq!(after.offset_ms, switched.offset_ms);

        let ready = h.handle.wait_ready().await;
        assert_eq!(ready.quality, TimeQuality::NtpValid);
        assert_eq!(ready.clock_epoch, switched.clock_epoch);
    }

    /// 契约 7（RT-R12-2 / RT-R13-2）：**同源**阶跃校正双向均须换代。
    /// 源列表全程不变（换代只可能由阶跃触发）：
    /// - 同源平滑（参考系 +200ms，偏差 ≤ `CLOCK_STEP_MAX_MS`）→ 沿用旧纪元、零换代审计；
    /// - 同源快进（+2s）→ 换代 + 换代审计；
    /// - 同源回跳（−2s，相对外推期望 −4s）→ 再次换代 + 换代审计；
    /// - 换代后继续同源平滑 → 不反复换代（换代次数稳定）。
    #[tokio::test]
    async fn driver_same_source_clock_step_renews_epoch_both_directions() {
        let h = harness(vec![Step::ok(0)], 15, 60_000);
        wait_until(
            || h.handle.is_ready(),
            Duration::from_secs(2),
            "启动转 ready",
        )
        .await;
        let startup = h.audit.find("ntp_startup").expect("启动审计事件");
        let epoch_initial = startup.clock_epoch;

        // ① 同源平滑：+200ms ≤ 1s → 沿用旧纪元，且全程不得出现换代审计
        h.sampler.set_repeated(Step::ok(200));
        let smooth_base = h.audit.count_of("ntp_revalidated");
        wait_until(
            || h.audit.count_of("ntp_revalidated") >= smooth_base + 3,
            Duration::from_secs(2),
            "同源平滑复验（+200ms）",
        )
        .await;
        assert_eq!(
            h.audit.count_of("clock_epoch_renewed"),
            0,
            "RT-R12-2：同源且外推偏差 ≤1s 不得换代"
        );
        assert_eq!(
            last_event(&h, "ntp_revalidated").clock_epoch,
            epoch_initial,
            "同源平滑沿用旧纪元"
        );

        // ② 同源快进 +2s（> 1s）→ 换代
        h.sampler.set_repeated(Step::ok(2_000));
        wait_until(
            || h.audit.count_of("clock_epoch_renewed") >= 1,
            Duration::from_secs(2),
            "同源快进换代",
        )
        .await;
        let forward = last_event(&h, "clock_epoch_renewed");
        let epoch_forward = forward.clock_epoch;
        assert_ne!(
            epoch_forward, epoch_initial,
            "RT-R13-2：同源快进 >1s 必须换代"
        );
        assert_eq!(
            last_event(&h, "ntp_revalidated").clock_epoch,
            epoch_forward,
            "换代审计与触发它的复验变迁同纪元"
        );
        assert_eq!(forward.reason, "clock_epoch_renewed");
        assert_eq!(
            h.handle.current().await.clock_epoch,
            epoch_forward,
            "快进换代后 handle 读到新纪元"
        );

        // ③ 同源回跳 −2s（相对外推期望 −4s，> 1s）→ 再次换代
        h.sampler.set_repeated(Step::ok(-2_000));
        wait_until(
            || h.audit.count_of("clock_epoch_renewed") >= 2,
            Duration::from_secs(2),
            "同源回跳换代",
        )
        .await;
        let backward = last_event(&h, "clock_epoch_renewed");
        assert_ne!(
            backward.clock_epoch, epoch_forward,
            "RT-R13-2：同源回跳同样必须换代（双向触发）"
        );
        assert_ne!(backward.clock_epoch, epoch_initial);
        assert_eq!(
            last_event(&h, "ntp_revalidated").clock_epoch,
            backward.clock_epoch,
            "回跳换代审计与复验变迁同纪元"
        );
        let after = h.handle.current().await;
        assert_eq!(after.quality, TimeQuality::NtpValid);
        assert_eq!(after.clock_epoch, backward.clock_epoch);
        assert_eq!(
            after.source.as_deref(),
            Some(SOURCE_A),
            "全程同源：换代只可能由阶跃触发"
        );

        // ④ 换代后同源平滑：换代次数稳定在 2，不反复换代、纪元不再漂移
        let settled_base = h.audit.count_of("ntp_revalidated");
        wait_until(
            || h.audit.count_of("ntp_revalidated") >= settled_base + 3,
            Duration::from_secs(2),
            "换代后同源平滑复验",
        )
        .await;
        assert_eq!(
            h.audit.count_of("clock_epoch_renewed"),
            2,
            "同源平滑复验不得反复换代"
        );
        assert_eq!(h.handle.current().await.clock_epoch, backward.clock_epoch);
    }

    /// 契约 5：每次状态变迁写一条脱敏审计事件；载荷只含约定字段，不含源地址/端口/nonce。
    #[tokio::test]
    async fn driver_emits_redacted_audit_event_on_each_transition() {
        let h = harness(vec![Step::ok(0)], 10, 60);
        wait_until(
            || h.handle.is_ready(),
            Duration::from_secs(2),
            "启动转 ready",
        )
        .await;
        wait_until(
            || h.audit.count_of("ntp_revalidated") >= 1,
            Duration::from_secs(2),
            "同源复验",
        )
        .await;
        wait_until(
            || h.sleeper.count() >= 1,
            Duration::from_secs(2),
            "进入复验等待",
        )
        .await;
        h.sources.set(vec![SOURCE_B.to_string()]);
        wait_until(
            || h.audit.count_of("clock_epoch_renewed") >= 1,
            Duration::from_secs(2),
            "来源切换换代",
        )
        .await;
        h.sampler.set_repeated(Step::fail());
        wait_until(
            || h.audit.count_of("evidence_stale") >= 1,
            Duration::from_secs(3),
            "证据超龄失效",
        )
        .await;
        h.sampler.succeed_after(2);
        wait_until(
            || h.audit.count_of("ntp_recovered") >= 1,
            Duration::from_secs(3),
            "退避后恢复",
        )
        .await;

        let events = h.audit.events();
        let kinds: Vec<&'static str> = events.iter().map(|event| event.kind).collect();
        assert_eq!(
            kinds.first().copied(),
            Some("ntp_startup"),
            "事件序列：{kinds:?}"
        );
        let renewed_at = kinds
            .iter()
            .position(|kind| *kind == "clock_epoch_renewed")
            .expect("换代审计");
        assert_eq!(
            kinds[renewed_at - 1],
            "ntp_revalidated",
            "换代审计必须紧跟触发换代的状态变迁：{kinds:?}"
        );
        let stale_at = kinds
            .iter()
            .position(|kind| *kind == "evidence_stale")
            .expect("超龄失效审计");
        let recovered_at = kinds
            .iter()
            .position(|kind| *kind == "ntp_recovered")
            .expect("恢复审计");
        assert!(
            renewed_at < stale_at && stale_at < recovered_at,
            "变迁顺序：换代 → 超龄失效 → 恢复：{kinds:?}"
        );
        assert!(
            kinds.iter().all(|kind| matches!(
                *kind,
                "ntp_startup"
                    | "ntp_revalidated"
                    | "clock_epoch_renewed"
                    | "evidence_stale"
                    | "ntp_recovered"
            )),
            "事件种类集合封闭：{kinds:?}"
        );

        let startup = &events[0];
        assert_eq!(
            (startup.from, startup.to, startup.reason),
            (TimeQuality::Stale, TimeQuality::NtpValid, "ntp_startup")
        );
        assert_eq!(startup.offset_ms, Some(1_000));
        assert_eq!(startup.uncertainty_ms, Some(10));
        assert_eq!(startup.attempt, 0);
        let stale = &events[stale_at];
        assert_eq!(
            (stale.from, stale.to, stale.reason, stale.attempt),
            (
                TimeQuality::NtpValid,
                TimeQuality::SystemFallback,
                "evidence_stale",
                0
            )
        );
        assert_eq!((stale.offset_ms, stale.uncertainty_ms), (None, None));
        let recovered = &events[recovered_at];
        assert_eq!(
            (recovered.from, recovered.to, recovered.reason),
            (
                TimeQuality::SystemFallback,
                TimeQuality::NtpValid,
                "ntp_recovered"
            )
        );
        assert!(
            recovered.attempt >= 1,
            "恢复事件记录复位前消耗的退避尝试数：{}",
            recovered.attempt
        );
        // M1：失效后恢复必须换代——恢复事件之后紧跟换代审计，且纪元与失效前不同
        let (renewed_after_recovery, _) = kinds
            .iter()
            .enumerate()
            .find(|(index, kind)| *index > recovered_at && **kind == "clock_epoch_renewed")
            .expect("M1：失效后恢复必须写换代审计");
        assert_eq!(
            events[renewed_after_recovery].clock_epoch, recovered.clock_epoch,
            "换代审计与恢复事件同纪元"
        );
        assert_ne!(
            recovered.clock_epoch, stale.clock_epoch,
            "M1：evidence_stale 失效后恢复不得沿用旧 clock_epoch"
        );
        assert_eq!(h.handle.retry_attempt(), 0, "恢复后 attempt 归零");
        assert!(
            events.iter().all(|event| event.reason == event.kind),
            "reason 与 kind 一致"
        );

        // 契约 5：审计载荷脱敏，绝不含 NTP 源原始地址、nonce、socket 端口
        for event in &events {
            let rendered = format!("{event:?}");
            for secret in [SOURCE_A, SOURCE_B, FAKE_ADDR, FAKE_NONCE] {
                assert!(
                    !rendered.contains(secret),
                    "审计事件泄露未脱敏信息 {secret}：{rendered}"
                );
            }
        }
    }

    /// 契约 3 + 4：墙钟回跳/前跳即失效降级并换代；退避间隔单调不降且封顶 BACKOFF_MAX。
    #[tokio::test]
    async fn driver_wall_clock_jump_falls_back_with_monotonic_backoff() {
        let h = harness(vec![Step::fail()], 10, 60_000);
        // 第 2 次墙钟读取回跳 5s（> JUMP_BACK_EPS）；第 12 次前跳 1h+1ms（> JUMP_FORWARD_MAX）
        h.wall.script_at(2, -5_000);
        h.wall
            .script_at(12, i64::try_from(JUMP_FORWARD_MAX.as_millis()).unwrap() + 1);
        wait_until(
            || h.audit.count_of("wall_clock_jump") >= 2,
            Duration::from_secs(3),
            "两次跳钟失效",
        )
        .await;

        let events = h.audit.events();
        assert_eq!(events[0].kind, "ntp_startup");
        assert_eq!(
            events[0].to,
            TimeQuality::SystemFallback,
            "启动采样失败 → SystemFallback"
        );
        let jumps: Vec<&TimeAuditEvent> = events
            .iter()
            .filter(|event| event.kind == "wall_clock_jump")
            .collect();
        assert_eq!(jumps[0].from, TimeQuality::SystemFallback);
        assert_eq!(jumps[0].to, TimeQuality::SystemFallback);
        assert_eq!(jumps[0].reason, "wall_clock_jump");
        assert_eq!(jumps[0].attempt, 0, "契约 4：跳钟后 attempt = 0");
        assert_eq!((jumps[0].offset_ms, jumps[0].uncertainty_ms), (None, None));
        assert_ne!(
            jumps[0].clock_epoch, events[0].clock_epoch,
            "RT-R8-1：跳钟即换代，旧纪元证据不再被信任"
        );
        assert_ne!(
            jumps[0].clock_epoch, jumps[1].clock_epoch,
            "第二次跳钟再次换代"
        );
        assert_eq!(
            h.handle.current().await.quality,
            TimeQuality::SystemFallback,
            "跳钟后权威状态为 SystemFallback"
        );

        // 契约 3：跳钟复位 attempt 后从 BACKOFF_MIN 重新单调退避，间隔不降、封顶 BACKOFF_MAX
        wait_until(
            || h.sleeper.count() >= 10,
            Duration::from_secs(3),
            "退避推进",
        )
        .await;
        let sleeps = h.sleeper.sleeps();
        let expected: Vec<Duration> = (1..=8u32).map(next_backoff).collect();
        assert_eq!(
            &sleeps[2..10],
            expected.as_slice(),
            "跳钟（第 3 轮）后退避序列必须重新从 BACKOFF_MIN 单调增长并封顶：{sleeps:?}"
        );
        assert!(
            sleeps[2..10].windows(2).all(|pair| pair[0] <= pair[1]),
            "退避间隔单调不降：{sleeps:?}"
        );
        assert!(
            sleeps[2..10]
                .iter()
                .all(|d| *d >= BACKOFF_MIN && *d <= BACKOFF_MAX),
            "退避区间 [{BACKOFF_MIN:?}, {BACKOFF_MAX:?}]：{sleeps:?}"
        );
    }

    // --------------------------------------------------------------------------
    // B. wait_ready 就绪等待受控断言（RT-N4，配合契约 6）
    // --------------------------------------------------------------------------

    /// ① 提前唤醒：driver 未 ready 时不得提前返回 Stale；转 ready 后立即被唤醒返回 NtpValid。
    #[tokio::test]
    async fn wait_ready_returns_early_when_driver_becomes_ready() {
        let h = harness(vec![Step::slow_ok(150)], 1_000, 60_000);
        let started = tokio::time::Instant::now();
        let evidence = h.handle.wait_ready().await;
        let elapsed = started.elapsed();
        assert_eq!(
            evidence.quality,
            TimeQuality::NtpValid,
            "wait_ready 必须返回转 ready 后的 NtpValid 快照"
        );
        assert!(
            elapsed >= Duration::from_millis(100),
            "driver 未 ready 前不得提前返回 Stale 快照：{elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "转 ready 后应提前唤醒，远小于 30s：{elapsed:?}"
        );
    }

    /// ② 超时兜底：driver 始终不 ready 时，等满就绪超时返回当前快照，而非永久挂起。
    #[tokio::test]
    async fn wait_ready_times_out_to_current_snapshot() {
        assert_eq!(
            READY_WAIT_TIMEOUT,
            Duration::from_secs(30),
            "RT-N4：生产就绪等待上限 30s（本用例以 with_ready_timeout 注入 300ms 短超时）"
        );
        let h = harness(vec![Step::fail()], 10, 60_000);
        let started = tokio::time::Instant::now();
        let evidence = h.handle.wait_ready().await;
        let elapsed = started.elapsed();
        assert!(!h.handle.is_ready(), "driver 始终未 ready");
        assert!(
            elapsed >= READY_TIMEOUT,
            "未 ready 必须等满就绪超时，不得提前返回：{elapsed:?}"
        );
        assert!(
            elapsed < READY_TIMEOUT + Duration::from_millis(200),
            "超时兜底应及时返回：{elapsed:?}"
        );
        assert_eq!(
            evidence.quality,
            TimeQuality::SystemFallback,
            "超时兜底返回当前 SystemFallback 快照"
        );
    }

    /// ③ 已 ready 短路：先 ready 再调用，零等待直接返回（覆盖通知先于等待发出的丢通知场景）。
    #[tokio::test]
    async fn wait_ready_short_circuits_when_already_ready() {
        let h = harness(vec![Step::ok(0)], 10, 60_000);
        wait_until(|| h.handle.is_ready(), Duration::from_secs(2), "先转 ready").await;
        let started = tokio::time::Instant::now();
        let evidence = h.handle.wait_ready().await;
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "已 ready 应零等待返回：{:?}",
            started.elapsed()
        );
        assert_eq!(evidence.quality, TimeQuality::NtpValid);
    }

    /// ④ 检查-注册竞态（RT-R7-4）：并发挂起多个调用方后一次 notify 全部唤醒，无一死等超时。
    #[tokio::test]
    async fn wait_ready_wakes_all_concurrent_waiters() {
        const WAITERS: usize = 8;
        let h = harness(vec![Step::slow_ok(200)], 1_000, 60_000);
        let started = tokio::time::Instant::now();
        let waiters: Vec<_> = (0..WAITERS)
            .map(|_| {
                let handle = h.handle.clone();
                tokio::spawn(async move { handle.wait_ready().await })
            })
            .collect();
        // 全部调用方完成监听器注册后，driver 才发出 is_ready + notify_waiters
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!h.handle.is_ready(), "通知必须晚于监听器注册");
        let mut results = Vec::with_capacity(WAITERS);
        for waiter in waiters {
            results.push(waiter.await.expect("waiter 任务不应 panic"));
        }
        let elapsed = started.elapsed();
        assert_eq!(results.len(), WAITERS);
        assert!(
            results
                .iter()
                .all(|evidence| evidence.quality == TimeQuality::NtpValid),
            "全部调用方均被唤醒并返回 NtpValid 快照"
        );
        assert!(
            elapsed >= Duration::from_millis(150) && elapsed < Duration::from_secs(5),
            "一次 notify 唤醒全部调用方，无一因丢通知死等超时：{elapsed:?}"
        );
    }

    // --------------------------------------------------------------------------
    // C. 契约 8：公开装配路径 `start_time_gate_with` 的真实 spawn 接线
    // --------------------------------------------------------------------------

    /// 契约 8：公开装配函数 `start_time_gate_with` 必须真正完成「校验配置 → 构造 handle →
    /// 组装 `TimeGateDriver` → `tokio::spawn`」接线：由 spawn 出的 driver 执行启动采样，
    /// 把 handle 从初始 `Stale` 推到 `NtpValid` + ready，并写 `ntp_startup` 脱敏审计。
    /// 若装配或 spawn 回归被破坏，`is_ready` 永不置位，本用例在 2s 轮询处失败。
    #[tokio::test]
    async fn start_time_gate_with_spawns_driver_that_reaches_ready() {
        let config = TimeGateConfig {
            servers: vec![SOURCE_A.to_string()],
            request_timeout: Duration::from_millis(50),
            startup_budget: Duration::from_millis(200),
            max_rtt_ms: 2_000,
            revalidate_interval: Duration::from_millis(20),
            max_evidence_age_ms: 60_000,
        };
        let sampler = FakeSampler::new(vec![Step::ok(0)]);
        let sources = FakeSources::new(vec![SOURCE_A.to_string()]);
        let wall = FakeWallClock::new();
        let sleeper = RecordingSleeper::new();
        let audit = Arc::new(MemAuditSink::default());
        let sampler_dep: Arc<dyn TimeSampler> = sampler.clone();
        let sources_dep: Arc<dyn NtpSourceList> = sources.clone();
        let wall_dep: Arc<dyn WallClock> = wall.clone();
        let sleeper_dep: Arc<dyn Sleeper> = sleeper.clone();
        let audit_dep: Arc<dyn TimeAuditSink> = audit.clone();
        let clock_epoch = Uuid::new_v4();

        let handle = start_time_gate_with(
            config,
            clock_epoch,
            TimeGateDeps {
                sampler: sampler_dep,
                sources: sources_dep,
                wall_clock: wall_dep,
                sleeper: sleeper_dep,
                audit: audit_dep,
            },
        );

        wait_until(
            || handle.is_ready(),
            Duration::from_secs(2),
            "start_time_gate_with spawn 的 driver 完成启动采样并转 ready",
        )
        .await;
        let evidence = handle.current().await;
        assert_eq!(
            evidence.quality,
            TimeQuality::NtpValid,
            "契约 8：启动采样成功 → NtpValid"
        );
        assert_eq!(
            evidence.clock_epoch, clock_epoch,
            "契约 7 情形 1：首次采样沿用调用方传入的 clock_epoch"
        );
        assert_eq!(evidence.source.as_deref(), Some(SOURCE_A));
        assert!(
            !handle.background_retry_active(),
            "启动采样成功即停止后台退避重试"
        );
        assert!(sampler.calls() >= 1, "spawn 出的 driver 必须真实调用采样器");
        assert_eq!(
            sampler.seen(0),
            vec![SOURCE_A.to_string()],
            "driver 通过注入的源列表读取源"
        );

        // 已 ready 的 handle 上 `wait_ready` 立即返回同一权威快照（接线端到端可用）
        let ready = handle.wait_ready().await;
        assert_eq!(ready.quality, TimeQuality::NtpValid);
        assert_eq!(ready.clock_epoch, clock_epoch);

        let events = audit.events();
        let startup = events.first().expect("启动审计事件");
        assert_eq!(
            (startup.kind, startup.from, startup.to, startup.reason),
            (
                "ntp_startup",
                TimeQuality::Stale,
                TimeQuality::NtpValid,
                "ntp_startup"
            )
        );
        assert_eq!(startup.clock_epoch, clock_epoch);
        assert_eq!(
            audit.count_of("clock_epoch_renewed"),
            0,
            "首次采样沿用调用方纪元，不写换代审计"
        );
        assert!(
            events
                .iter()
                .all(|event| matches!(event.kind, "ntp_startup" | "ntp_revalidated")),
            "装配路径事件种类封闭：{:?}",
            events.iter().map(|event| event.kind).collect::<Vec<_>>()
        );
    }
}

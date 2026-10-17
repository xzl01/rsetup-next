//! Runtime Task 1b 受控测试：真请求关联（correlate）、门禁纯函数与启动总预算。
//!
//! 全部使用进程内受控 fake（构造 48B NTP 响应、受控 `SntpResolver`/`SntpBindFuture`、
//! 本地 127.0.0.1 fake UDP 服务器），不访问真实网络/NTP 服务。
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
    packet[0] = (0u8 << 6) | (4u8 << 3) | 4u8; // LI=0, VN=4, Mode=4 (Server)
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

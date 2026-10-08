# PollScheduler 显式 Tick Cadence 防洪峰实施计划（TDD）

> **致执行代理：** 使用 superpowers:subagent-driven-development 或 superpowers:executing-plans，逐步跟踪 `- [ ]`。

**Goal:** 消除亚周期停顿后 `due(now)` 一次倾泻数百台设备，同时保持正常 tick 防饿死与原始相位。

**Architecture:** `PollScheduler` 除设备轮询 `period` 外还接收驱动 `tick_cadence`，保存上次调用的 `Instant`。调用间隔超过 cadence 或系统过载时，本次不下发、按整数周期将已逾期设备移至未来；正常 tick 才按原相位下发。`SnapshotStore` 不在此任务内。

**Tech Stack:** Rust 2024 / MSRV 1.85；仅 `std::time::{Instant, Duration}`，不新增 crate。

**Spec:** [错峰 cadence 修订设计](../specs/2026-10-07-controller-poll-cadence-correction-design.md)、[现有运行期计划 Task 5](2026-10-07-controller-v1-remaining-runtime-tdd.md)、[05 规格 §6](../specs/2026-09-23-controller-v1-05-runtime-operations.md)。

## Global Constraints

- 构造器 `new(origin: Instant, period: Duration, tick_cadence: Duration, devices: Vec<[u8; 32]>) -> Self`；不保留隐式猜测 tick 的三参数生产入口。周期、cadence、设备数由调用方决定；1024/10s/100ms/11 只在测试中作为建议基准。`due`, `set_overloaded`, `backlog` 保持原名称。
- 一次停顿 `now - last_tick > tick_cadence` 或明确过载时零下发、无队列、保持原相位；正常 tick 不饿死。`backlog()==0` 不等于系统完成整体背压。
- RED 在旧三参数实现上编写可运行的行为断言；先证实输出数百台导致断言失败，再修改接口。编译错误、缺依赖和零用例均非有效 RED。
- 离线锁定 Cargo；无 DB/NTP/硬件/外网/root 锁，不能声称 Task5 整体完成或容量实测。

### Task 1：Tick Cadence 停顿识别与回归

**Files:** Modify `crates/rsetup-controller/src/polling/scheduler.rs` (生产与本模块单测)；已有计划 Task5 的签名和算法说明已按本设计修订。

**Interfaces:** Consumes 旧三参数 `new` 和 `due(now)`；Produces 四参数 `new(origin,period,tick_cadence,devices)`，其余名称不变。

- [ ] **Step 1 RED：** 先在旧签名上编写两个新增单测并运行（各匹配一个用例）：

```rust
#[test]
fn five_second_pause_does_not_burst() {
    let origin = Instant::now();
    let mut s = PollScheduler::new(origin, Duration::from_secs(10), make_test_devices(1024));
    let now = origin.checked_add(Duration::from_secs(5)).unwrap();
    assert!(s.due(now).is_empty(), "paused tick cannot replay past phases");
}
#[test]
fn exact_period_pause_does_not_burst() {
    let origin = Instant::now();
    let mut s = PollScheduler::new(origin, Duration::from_secs(10), make_test_devices(1024));
    let now = origin.checked_add(Duration::from_secs(10)).unwrap();
    assert!(s.due(now).is_empty(), "missed period cannot flood one tick");
}
```

运行 `cargo test --offline --locked -p rsetup-controller five_second_pause_does_not_burst --` 与对应 `exact_period_pause_does_not_burst`；预期各执行 1 项，旧实现分别返回约 513 和 1023 台而断言失败。

- [ ] **Step 2 GREEN：** 在 `PollScheduler` 中加入 `tick_cadence: Duration`、`last_tick: Instant`，四参数 `new` 保存 `last_tick=origin`；修改已有 scheduler 单测所有构造调用显式传入 `Duration::from_millis(100)`。保持 `period == Duration::ZERO` 的一次性行为与空设备集合的安全返回。核心分支必须是：

```rust
// 在 due(now) 入口处；时间倒退绝不回写 cursor。
if now < self.last_tick || self.tick_cadence.is_zero() {
    return Vec::new();
}
let paused = now.duration_since(self.last_tick) > self.tick_cadence;
self.last_tick = now;
```

之后遍历所有 `next_due <= now` 的设备；`period == Duration::ZERO` 保留旧的一次性分支并在除法前完成。对普通周期，若 `self.overloaded || paused || now.duration_since(next_due) >= self.period`，以原有 `mul_duration_u128` 和 `checked_add` 按 `(elapsed/period)+1` 个整数周期前推该设备的 `next_due` 至 `> now`，不把它加入 `ready`；否则将设备 ID 加入 `ready` 且 `next_due.checked_add(self.period)`。整轮 `paused` 的调用必须返回空，而不能对某些刚到期设备放行。`backlog()` 恒零仅代表内部不排队。

- [ ] **Step 3 扩展测试：** 5s/10s/30s+1ns 首次停顿应零发放，然后 100 次 100ms tick 每次 `<=11` 且收齐 1024 个唯一设备；首个正常 `origin+100ms` 下发非空；过载解除不回放；相同 `now` 无双发；60s 周期+1s cadence 的正常驱动不饿死；零 cadence、零周期、空设备和倒退 now 不 panic、不发旧轮次。参考测试（保留计划原 30s、normal tick 及已有 overload 测试，将其构造器改成显式 cadence）：

```rust
#[test]
fn five_second_pause_recovers_next_full_cycle_without_burst() {
    let origin = Instant::now();
    let mut s = PollScheduler::new(origin, Duration::from_secs(10), Duration::from_millis(100), make_test_devices(1024));
    let paused = origin.checked_add(Duration::from_secs(5)).unwrap();
    assert!(s.due(paused).is_empty());
    let mut seen = std::collections::HashSet::new();
    for tick in 1..=100 {
        let at = paused.checked_add(Duration::from_millis(tick * 100)).unwrap();
        let batch = s.due(at);
        assert!(batch.len() <= 11);
        for id in batch { assert!(seen.insert(id), "duplicate device in recovery period"); }
    }
    assert_eq!(seen.len(), 1024);
    assert_eq!(s.backlog(), 0);
}
#[test]
fn one_second_cadence_with_sixty_second_period_is_not_starved() {
    let origin = Instant::now();
    let mut s = PollScheduler::new(origin, Duration::from_secs(60), Duration::from_secs(1), make_test_devices(60));
    let mut seen = std::collections::HashSet::new();
    for tick in 0..60 {
        let at = origin.checked_add(Duration::from_secs(tick)).unwrap();
        for id in s.due(at) { assert!(seen.insert(id)); }
    }
    assert_eq!(seen.len(), 60);
}
```

零 cadence、零周期、空设备及时间倒退沿用原单测中的 `Instant` 夹具扩充断言：零 cadence 构造后即使到期也 `due(origin).is_empty()`，倒退 `now` 返回空且下一次正常 `origin+100ms` 仍可发放；零周期在正 cadence 下只单发一次。用 `cargo test --offline --locked -p rsetup-controller polling::scheduler::tests --` 核对实际运行数与结果。

- [ ] **Step 4 回归与提交：** `cargo fmt --all -- --check`、`cargo test --workspace --offline --locked -q`、`cargo clippy --workspace --all-targets --offline --locked -- -D warnings`。审核 staged diff 不含任何凭据，仅提交本任务代码、对应测试及已确认的两份修订文档；独立审查并明确保留 SnapshotStore、认证连接代际和真实容量验收的未完成状态。不自动合并或推送。
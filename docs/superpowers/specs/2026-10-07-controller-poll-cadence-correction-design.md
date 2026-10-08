# Controller 纯错峰调度器 Tick Cadence 修订设计（2026-10-07）

## 决策与范围

现有 `PollScheduler::new(origin, period, devices)` 只知道每台采集周期，不知道驱动 `due()` 的预期间隔。若创建后首次调用发生在 `origin + 5s`（1024 台、10s 周期），旧代码因每台设备 `elapsed < period` 一次返回约 513 台；恰好 `origin + 10s` 则约 1023 台。这违反停顿后跳轮、不积压、不产生洪峰的约束，现有 30s 停顿测试未覆盖亚周期边界。

本修订仅针对当前 [运行期计划 Task 5](../plans/2026-10-07-controller-v1-remaining-runtime-tdd.md) 的 **5.1 纯相位调度器**，不实现 SnapshotStore、认证连接管理器、多代际校验、NTP 网络、数据库、速率令牌桶或实际下发。计划中 `now - next_due >= period` 作为停顿判据不充分，应改为按驱动 tick 的调用间隔识别漏掉的窗口。

## 公开接口与数据流

将构造器明确为 `PollScheduler::new(origin: Instant, period: Duration, tick_cadence: Duration, devices: Vec<[u8; 32]>) -> Self`；`due(now)`, `set_overloaded(bool)`, `backlog()` 名称不变。当前无生产调用方，更新 Task 5 测试和计划中的签名，不保留隐式猜测 tick 间隔的三参数重载。`period`、`tick_cadence`、设备数均由调用方传入，1024/10s/100ms/≤11 仅是基准测试组合，不是生产硬编码常量。`tick_cadence` 表示一次普通调度调用可接受的最长时间窗口，而非 NTP/设备采样周期。

调度器记录 `last_tick`（初始为 `origin`）。当 `due(now)` 满足 `now < last_tick` 时不下发且不倒退状态；重复相同 `now` 不重复下发。`tick_cadence == 0` 失败关闭为不下发（也不猜测 cadence），以后的配置修复须重新建调度器；`period == 0` 保持既有一次性安全行为，只在最初 `origin` 到期且非过载时下发一次。

每次调用先确认调用间隔：若 `now - last_tick > tick_cadence`，这是停顿恢复，**本次不下发任何设备**，将所有已到期的 `next_due` 以整数周期跳到严格大于 `now` 的下一个同相位点，`backlog()` 仍为 0；未来正常 tick 继续按原相位分布。若明确 `overloaded`，同样跳过已到期项，零下发；恢复重调用同一时刻不能回补。只有未过载且调用间隔 `<= tick_cadence` 时才下发本次到期设备，推进该设备的 `next_due` 一个周期；若某设备本身已过期一整个 period，仍跳过至未来，不自动补发旧周期。所有 `Instant` / `Duration` 运算使用 checked/saturating 方法，溢出时该设备不再下发，绝不 panic。

此调度器只保证自身不积压和不因停顿突发，不声称拥有系统查询速率、在途信号量、板端健康或全量快照新鲜度。接线层必须以相同 `tick_cadence` 驱动，并独立限制查询预算；停顿后的系统级真实容量和尾延迟需另行验收。

## TDD 与验证

1. 在既有三参数接口上先新增 `pause_five_seconds_skips_burst` 与 `pause_exact_period_skips_burst` 行为测试，先看到约 513/1023 台导致 `<=11` 断言失败（RED，不能用构造器编译错误冒充）。
2. 修改接口、实现及现有测试夹具为显式 `tick_cadence`。保留并扩展 30s+1ns 跳轮/后续 100 个正常 100ms tick 恰好收齐 1024 个唯一 ID 的测试；验证 `origin+100ms` 首 tick 正常发放且 `<=11`、同一时刻重复调用无双发、过载解除不回放漏掉周期。
3. 补充 5s/10s 无标记停顿下首调用空列表、随后整轮相位分布；验证第二种配置（例如 60s 周期和 1s cadence）不因写死 100ms 产生饥饿；零 cadence、零周期、空设备、时刻倒退和安全整数边界均有可观察断言。
4. 使用 `--offline --locked` 跑 controller 定向测试、Cargo workspace 测试、`cargo fmt --all -- --check` 和 `cargo clippy --workspace --all-targets --offline --locked -- -D warnings`；禁用任何真实数据库、网络、硬件或 root 锁验证。独立规格与质量复查后，仅在隔离分支提交修订，不自动合并或推送。完整 Task 5 的快照与认证代际仍标未完成。

## 安全和验收边界

本修订不改变 05 规格中待审的系统限额，不宣称 1024 台容量已经真实验证，也不把单元测试中的恒为 0 的内部 backlog 当作系统无背压。若后续调用方不能保证传入与实际 timer 一致的 cadence，恢复后仍可能丢采样或发生流控问题；必须由真实轮询驱动集成测试与容量测量另行证明。协议安全阻断、精确 Wire codec 上限与真实双引擎验收保持原状态。

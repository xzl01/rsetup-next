# T1 实施报告：核心读取模型和健康判定纯函数

## 实现内容
- 新建 `crates/rsetup-core/src/health.rs`，实现并导出 `nvme_health_state`、`mmc_health_state`。
- 在 `model.rs` 新增 `TelemetryReadState`、`HealthState`、`TelemetryErrorKind`、`TelemetryError`、`TelemetryStatus`，严格使用 spec 要求的 snake_case/camelCase 序列化和默认值。
- NVMe 判定顺序：不可用或 `smart=None` 为 `Unknown`；非零 `critical_warning` 为 `Critical`；任意 warning flag 为 `Warning`；否则 `Healthy`。
- MMC 判定顺序：不可用为 `Unknown`；pre-EOL=3、A/B>100 或指定 urgent/exceeded flag 为 `Critical`；pre-EOL=2、A/B=100 或任意 flag 为 `Warning`；pre-EOL=1 或任一有效寿命值为 `Healthy`；全未知为 `Unknown`。
- 未修改 `Device` 字段、依赖或采集层。

## 变更文件
- `crates/rsetup-core/src/health.rs`（新增）
- `crates/rsetup-core/src/model.rs`
- `crates/rsetup-core/src/lib.rs`

## TDD 证据
基线（任务给定数字与当前 checkout 不一致）：
- `cargo test -p rsetup-core --locked`：122 passed, 2 ignored（随后新增 5 tests 后为 127 passed, 2 ignored）。

RED：
- 命令：`cargo test -p rsetup-core health --locked`
- 退出码：101
- 首次尝试有源码括号编译错误；修正后再次运行得到真实断言失败（非编译错误）：
  - `nvme_health_state_applies_warning_and_critical_priority`：`left: Unknown, right: Warning`
  - `mmc_health_state_keeps_real_pre_eol_warning`：`left: Unknown, right: Warning`
  - `mmc_health_state_handles_boundaries_and_unknown_flags`：`left: Unknown, right: Warning`
- 失败原因是健康判定函数仍为 stub，证明测试能捕获缺失行为。

GREEN：
- 同一命令：`cargo test -p rsetup-core health --locked`
- 退出码：0
- 5 passed, 0 failed；包含真实 pre-EOL warning/critical、缺失数据 fail-closed、NVMe warning/critical/healthy、MMC 100/101 边界与未知 flag、序列化契约断言。

## 回归验证
- `cargo test -p rsetup-core --locked`：127 passed, 2 ignored, 0 failed。
- 忽略项为既有环境依赖测试（device-tree-compiler、curl/loopback）。
- 未执行全仓无关格式化；仅对 T1 Rust 文件运行过 rustfmt，且已恢复其余文件的误格式化变更。

## Fix round1：审查缺口补齐

### 覆盖测试
- `health::tests::nvme_health_state_is_table_driven_and_unknown_bits_are_critical`：覆盖 `critical_warning` 的 `0x01` 与未知位 `0x80`、仅未知 warning flag、正常值，以及 `Unsupported`/`Unavailable` 携带残留数据仍为 `Unknown`。
- `health::tests::mmc_health_state_is_table_driven_and_symmetric`：覆盖 `Unsupported`/`Unavailable` 携带残留数据、全未知、仅 A/仅 B、pre-EOL=1、A/B 各 100 与 101、未知 flag、urgent/exceeded 指定 flags，并显式验证 A/B 对称。

### TDD / mutation evidence
- 先加入上述表驱动测试；临时将生产逻辑 `critical_warning != 0` 变异为 `== 0`。
- RED 命令：`cargo test -p rsetup-core health --locked`；退出码 `101`。
- 真实失败输出：`health::tests::nvme_health_state_is_table_driven_and_unknown_bits_are_critical ... FAILED`，`left: Healthy`，`right: Critical`，失败用例为 `critical warning bit 0x01`。
- 随后恢复生产逻辑；该 RED 是 mutation evidence，不是新发现的生产 bug。

### GREEN / 回归
- `cargo test -p rsetup-core health --locked`：退出码 `0`，5 passed，0 failed。
- `cargo test -p rsetup-core --locked`：退出码 `0`，127 passed，0 failed，2 ignored（既有环境依赖测试）。
- `rustfmt crates/rsetup-core/src/health.rs`：仅格式化当前文件。

### 自审与文件范围
- 生产判定逻辑未为制造 RED 而永久改变，优先级保持：读取状态 → critical → warning → healthy/unknown。
- 未修改模型、`Device` 字段、依赖或其他 scratch；仅更新 `health.rs` 测试与本报告。
- 提交文件列表：`crates/rsetup-core/src/health.rs`、`.superpowers/sdd/2026-09-13-storage-health-correctness-tdd-plan/task-1-report.md`。
- 更正原报告基线疑虑：workspace `162 = core 122 + app 39 + helper 1`；JS 基线命令 `node --test ui/*.test.mjs` 已由主代理执行并为 51 通过。本任务只要求并验证 core，未重复执行 JS。

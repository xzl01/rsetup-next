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

## 疑虑
- 任务 brief 声称基线 Rust 162 passed / JS 51 passed，但当前 checkout 实测基线为 122 passed / 2 ignored；本任务未发现或修复该差异。
- 当前 checkout 仅发现 `apps/desktop/package.json`，没有根级 JS 测试入口；因此没有运行 JS 基线测试。

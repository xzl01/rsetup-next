# Task 3.2 Report: CLI 命令与格式化输出

## Status: DONE

## Commit

- `5cb92c1` `feat(app): add hardware mmc and storage CLI commands` (branch `dev-aghost`)

## Changes (single file: `crates/rsetup-app/src/main.rs`)

1. **HardwareCommands 新增变体**（紧跟 `Nvme` 之后，doc 注释逐字取自 brief）：
   - `Mmc { #[arg(long)] json: bool }`
   - `Storage { #[arg(long)] json: bool }`
2. **命令分发**（紧跟 `HardwareCommands::Nvme` 匹配分支之后）：
   - `Mmc` → `controller.mmc_status()?`，json 分支 `serde_json::to_string_pretty`，文本分支 `format_mmc_status(&status, locale)`
   - `Storage` → `controller.storage_status()?`，同上调用 `format_storage_status`
   - `HardwareError` 与 `nvme_status()` 同类型，现有 `?` 路径直接复用，无需额外转换。
3. **格式化函数**（紧挨 `format_nvme_status` 之后，风格一致：`is_zh` 分支、`=== ... ===` 头、两空格缩进键值行）：
   - `format_mmc_status(&MmcStatus, Locale)`：未初始化（有/无 message 四种文案）、已初始化空设备提示、多设备空行分隔；每设备输出 类型/型号/厂商/序列号/固件版本/总容量（`format_bytes`）/预 EOL 状态（0=未定义、1=正常、2=预警 (80% 寿命)、3=紧急 (建议更换)、其他=未知 (n)）/寿命估计 (A/B)（`Some(p)` → `p%`，`None` → 不支持/N-A）/告警标志（空 → 无/None，否则 `join(", ")`）。
   - `format_storage_status(&StorageStatus, Locale)`：按 brief 建议 **先 NVMe 后 MMC**，`format_nvme_status` + 空行 + `format_mmc_status`。
4. **单元测试**（`#[cfg(test)] mod tests`，镜像 NVMe 测试写法）：
   - `hardware_cli_parses_mmc_subcommand_and_flags`
   - `hardware_cli_parses_storage_subcommand_and_flags`
   - `format_mmc_status_uninitialized_en_and_zh`
   - `format_mmc_status_initialized_with_devices`（2 设备：eMMC `life_time_est_a_percent: Some(10)`、SD `None`；断言 `mmc0:0001`、`/dev/mmcblk0`、`MMC`、`SD`、`10%`、`不支持` / `N/A`、`Normal`）
   - `format_storage_status_contains_both_sections`（1 NVMe + 1 MMC 设备，断言同时含 `nvme0` 与 `mmc0:0001`）

## TDD 过程

- **RED**：先写 5 个测试 + 测试模块 import（`MmcDevice, MmcHealth, MmcStatus, StorageStatus`），`cargo test -p rsetup-next` 编译失败（9 个 E0425/E0599：`Mmc`/`Storage` 变体与两个 format 函数不存在）。
- **GREEN**：实现变体、分发分支、两个格式化函数后，全部通过。

## 验证结果

- `PATH="/usr/bin:$PATH" cargo test -p rsetup-next`：**33 passed; 0 failed**（含 5 个新测试；helper 1 passed）。
- 全工作区 `cargo test`：rsetup-core 117 passed / 2 ignored，rsetup-next 33 passed，helper 1 passed —— 无回归。
- `PATH="/usr/bin:$PATH" cargo build`：成功（`rsetup-next` v0.5.0，无警告）。
- 冒烟测试（`--demo`）：`hardware mmc` 与 `hardware storage`（en/zh）输出均符合预期，storage 先 NVMe 段后 MMC 段。

## 偏差 / 备注

- brief 中验证命令写作 `cargo test -p rsetup-app`，但 `crates/rsetup-app` 的包名实际是 **`rsetup-next`**（见其 `Cargo.toml`），`-p rsetup-app` 报 "did not match any packages"。已按包名 `rsetup-next` 执行同等验证，并用全工作区 `cargo test` 覆盖。后续任务 brief 建议统一使用 `-p rsetup-next`。
- 无其他偏差；所有文案、字段标签（含 `  类型:             ` 等对齐空格）、pre-EOL 映射、`format_storage_status` 拼接顺序均逐字遵循 brief。

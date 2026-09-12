# Task 3.2 Brief: CLI 命令与格式化输出

## Requirements

### 1. `crates/rsetup-app/src/main.rs`

1. **HardwareCommands 新增两个子命令**（在 `Nvme` 变体之后，风格一致）：
```rust
/// Inspect MMC/eMMC/SD storage devices and endurance health / 查看 MMC/eMMC/SD 存储设备与寿命健康状态
Mmc {
    #[arg(long)]
    json: bool,
},
/// Inspect unified storage (NVMe + MMC) summary / 查看统一存储（NVMe + MMC）汇总
Storage {
    #[arg(long)]
    json: bool,
},
```

2. **命令分发**（在 `HardwareCommands::Nvme` 匹配分支之后，参照其写法）：
```rust
HardwareCommands::Mmc { json } => {
    let status = controller.mmc_status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("{}", format_mmc_status(&status, locale));
    }
}
HardwareCommands::Storage { json } => {
    let status = controller.storage_status()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!("{}", format_storage_status(&status, locale));
    }
}
```
   - 确认 `controller.mmc_status()?` / `controller.storage_status()?` 的 `HardwareError` 能被现有 `?` 路径转换（与 `nvme_status()` 相同的错误类型，应该可以直接复用）。

3. **格式化函数**（紧挨 `format_nvme_status` 之后新增，参照其结构：`is_zh` 分支、`=== 设备标题 ===` 头、缩进两空格的 `键: 值` 行）：

   `fn format_mmc_status(status: &rsetup_core::MmcStatus, locale: Locale) -> String`：
   - 未初始化（`!status.initialized`）：
     - 有 message：中文 `"未检测到 MMC/SD 存储设备，模块未激活。（{msg}）"` / 英文 `"No MMC/SD storage devices detected; module is uninitialized. ({msg})"`
     - 无 message：中文 `"未检测到 MMC/SD 存储设备，模块未激活。"` / 英文 `"No MMC/SD storage devices detected; module is uninitialized."`
   - 已初始化但设备为空：中文 `"已检测到 MMC 控制器，但未发现可用设备。"` / 英文 `"MMC host detected, but no storage devices found."`
   - 每个设备（多个设备之间空行分隔）：
     - 标题：中文 `"=== 存储设备: {name} ({block_path}) [{card_type}] ==="` / 英文 `"=== Storage Device: {name} ({block_path}) [{card_type}] ==="`
     - 字段（中文 / 英文）：
       - `"  类型:             MMC/SD 卡类型值"` — 实际输出 card_type 原值（如 "MMC"/"SD"）
       - `"  型号:             {model}"` / `"  Model:            {model}"`
       - `"  厂商:             {manufacturer}"` / `"  Manufacturer:   {manufacturer}"`
       - `"  序列号:           {serial}"` / `"  Serial Number:    {serial}"`
       - `"  固件版本:         {firmware}"` / `"  Firmware:         {firmware}"`
       - `"  总容量:           {size_str} ({total_bytes} 字节)"` / `"  Total Capacity:   {size_str} ({total_bytes} bytes)"` — 使用现有的 `format_bytes`
       - 健康状态行（中文 / 英文）：
         - `pre_eol_info` 映射：`0` -> `"未定义"` / `"Undefined"`、`1` -> `"正常"` / `"Normal"`、`2` -> `"预警 (80% 寿命)"` / `"Warning (80% endurance)"`、`3` -> `"紧急 (建议更换)"` / `"Urgent (replace soon)"`、其他 -> `"未知 ({pre_eol_info})"` / `"Unknown ({pre_eol_info})"`
         - 寿命行：`life_time_est_a_percent` / `life_time_est_b_percent` 为 `Some(p)` 时输出 `"{p}%"`，`None` 时输出 `"不支持"` / `"N/A"`：
           - 中文：`"  预 EOL 状态:       {pre_eol_str}"` 与 `"  寿命估计 (A/B):   {life_a_str} / {life_b_str}"`
           - 英文：`"  Pre-EOL:            {pre_eol_str}"` 与 `"  Life Time Est (A/B): {life_a_str} / {life_b_str}"`
       - 告警行：`warning_flags` 为空时中文 `"无"` / 英文 `"None"`，否则 `join(", ")`：
         - 中文 `"  告警标志:         {flags}"` / 英文 `"  Warning Flags:    {flags}"`

   `fn format_storage_status(status: &rsetup_core::StorageStatus, locale: Locale) -> String`：
   - 直接拼接：`format_mmc_status(&status.mmc, locale)` + 空行 + `format_nvme_status(&status.nvme, locale)`（先 NVMe 后 MMC，或先 MMC 后 NVMe 均可，选一种并在测试中保持一致；建议 **先 NVMe 后 MMC**，因为 NVMe 通常是主力存储）。

### 2. 单元测试（`main.rs` 的 `#[cfg(test)]`，参照 `hardware_cli_parses_nvme_subcommand_and_flags` 与 `format_nvme_status_*` 测试）

1. `hardware_cli_parses_mmc_subcommand_and_flags`：`["rsetup-next", "hardware", "mmc"]` 解析为 `HardwareCommands::Mmc { json: false }`；加 `--json` 为 `json: true`。
2. `hardware_cli_parses_storage_subcommand_and_flags`：同上验证 `Storage` 变体。
3. `format_mmc_status_uninitialized_en_and_zh`：`MmcStatus { initialized: false, devices: vec![], message: Some("No MMC/SD devices detected in system".into()) }`，中文输出包含 `"未检测到 MMC/SD"`，英文包含 `"No MMC/SD"`。
4. `format_mmc_status_initialized_with_devices`：用 `Controller` demo 数据不方便（main.rs 不直接依赖 demo 函数），直接构造 `MmcStatus`（2 个设备：一个 eMMC `life_time_est_a_percent: Some(10)`，一个 SD `None`）：
   - 中文输出包含 `"mmc0:0001"`、`"/dev/mmcblk0"`、`"MMC"`、`"SD"`、`"10%"`、`"不支持"`；
   - 英文输出包含 `"mmc0:0001"`、`"N/A"`、`"Normal"`。
5. `format_storage_status_contains_both_sections`：构造 `StorageStatus { nvme: <1 个 nvme0 设备的 NvmeStatus>, mmc: <1 个设备的 MmcStatus> }`，断言输出同时包含 `"nvme0"` 与 `"mmc0:0001"`（验证两段都被拼接）。

### 3. 验证与提交
- 运行 `PATH="/usr/bin:$PATH" cargo test -p rsetup-app` 全部通过（不得回归现有测试）。
- 运行 `PATH="/usr/bin:$PATH" cargo build` 确认二进制可构建。
- 提交信息：`feat(app): add hardware mmc and storage CLI commands`

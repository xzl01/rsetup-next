# Task 3.1 Brief: Controller 状态聚合与 Demo 模式

## Requirements

### 1. `crates/rsetup-core/src/actions.rs`

1. **Controller 增加 MMC 管理器**：
   - 在 `Controller` 结构体中新增字段 `mmc: Arc<MmcManager>`（在 `nvme` 字段之后）。
   - 在 `Controller::new` 中初始化 `mmc: Arc::new(MmcManager::new())`（与 `nvme` 相同风格，不区分 synthetic——`MmcManager::new()` 在空 sysfs 下安全返回 `initialized: false`）。
   - `MmcManager` 已在 `crates/rsetup-core/src/lib.rs` 中导出（`pub use mmc::{MmcError, MmcManager};`）。

2. **新增公开方法**（紧挨 `nvme_status` 之后，风格一致）：
```rust
pub fn mmc_status(&self) -> Result<MmcStatus, HardwareError> {
    if self.synthetic {
        return Ok(demo_mmc_status());
    }
    Ok(self.mmc.status())
}

pub fn storage_status(&self) -> Result<StorageStatus, HardwareError> {
    Ok(StorageStatus {
        nvme: self.nvme_status()?,
        mmc: self.mmc_status()?,
    })
}
```
   - 需要 `use crate::model::{MmcStatus, StorageStatus};` 或确认现有 import 已覆盖（检查文件顶部的 model import 列表，按需添加 `MmcStatus, StorageStatus`）。

3. **Demo 模式数据**（与 `demo_nvme_status` 并排新增 `fn demo_mmc_status() -> MmcStatus`）：
   - 返回 `initialized: true`，包含 **2 个设备**：
     - 设备 1（eMMC）：
       - `name: "mmc0:0001"`、`block_path: "/dev/mmcblk0"`、`card_type: "MMC"`
       - `model: "FE4MB4"`、`manufacturer: "Samsung (0x000015)"`、`serial: "0x12345678"`、`firmware: "0x01"`
       - `total_bytes: 62_537_072_640`（~58.2 GiB）
       - `health: MmcHealth { pre_eol_info: 1, life_time_est_a_percent: Some(10), life_time_est_b_percent: Some(10), warning_flags: vec![] }`
     - 设备 2（SD 卡）：
       - `name: "mmc1:59b4"`、`block_path: "/dev/mmcblk1"`、`card_type: "SD"`
       - `model: "SC64G"`、`manufacturer: "SanDisk (0x000045)"`、`serial: "0x87654321"`、`firmware: "0x01"`
       - `total_bytes: 64_026_691_584`（~59.6 GiB）
       - `health: MmcHealth { pre_eol_info: 0, life_time_est_a_percent: None, life_time_est_b_percent: None, warning_flags: vec![] }`
       （SD 卡无寿命指标，pre_eol 为 0 表示未定义）
   - `message: None`

### 2. 单元测试（`crates/rsetup-core/src/actions.rs` 的 `#[cfg(test)] mod tests`）

参照现有 `test_controller_nvme_status_demo` / `test_controller_nvme_status_live` 的写法（先读这两个测试再写）：

1. `test_controller_mmc_status_demo()`：
   - `Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)`
   - 断言 `status.initialized == true`、`status.devices.len() == 2`
   - 断言设备 0 为 eMMC（card_type "MMC"，block_path "/dev/mmcblk0"，total_bytes == 62_537_072_640，life_time_est_a_percent == Some(10)）
   - 断言设备 1 为 SD（card_type "SD"，life_time_est_a_percent == None）
2. `test_controller_mmc_status_live()`：
   - `Controller::new(ProbeMode::Live, ExecutionPolicy::DryRun)`
   - 仅断言调用不 panic 且返回 `Ok`（开发机无 /dev/mmcblk，`initialized` 可能为 false，不要断言设备数量）
3. `test_controller_storage_status_demo()`：
   - `Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)`
   - 断言 `storage.nvme.initialized == true`、`storage.nvme.devices.len() == 1`
   - 断言 `storage.mmc.initialized == true`、`storage.mmc.devices.len() == 2`

### 3. 验证与提交
- 运行 `PATH="/usr/bin:$PATH" cargo test -p rsetup-core` 全部通过（现有 114+ 个测试不得回归）。
- 提交信息：`feat(core): aggregate MMC and unified storage status in Controller`

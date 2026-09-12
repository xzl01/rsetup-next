# Task 3.1 Report: Controller 状态聚合与 Demo 模式

## Status: DONE

## Commit

- `6ec422f` — `feat(core): aggregate MMC and unified storage status in Controller` (1 file, +99/-3)

## Changes (`crates/rsetup-core/src/actions.rs`)

1. **Controller 增加 MMC 管理器**
   - 新增字段 `mmc: Arc<MmcManager>`，紧跟 `nvme` 字段之后。
   - `Controller::new` 中初始化 `mmc: Arc::new(MmcManager::new())`（与 `nvme` 相同风格，不区分 synthetic）。
   - import 列表新增 `MmcManager, MmcStatus`（来自 crate root re-export）。

2. **新增公开方法**（紧挨 `nvme_status` 之后）
   - `pub fn mmc_status(&self) -> Result<MmcStatus, HardwareError>`：synthetic 时返回 `demo_mmc_status()`，否则 `Ok(self.mmc.status())`。
   - `pub fn storage_status(&self) -> Result<StorageStatus, HardwareError>`：聚合 `nvme_status()?` 与 `mmc_status()?`。
   - import 列表新增 `MmcDevice, MmcHealth, StorageStatus`。

3. **Demo 数据 `fn demo_mmc_status() -> MmcStatus`**（与 `demo_nvme_status` 并排）
   - `initialized: true`、`message: None`、2 个设备：
     - 设备 0（eMMC）：`name "mmc0:0001"`、`/dev/mmcblk0`、`card_type "MMC"`、model `FE4MB4`、`Samsung (0x000015)`、`0x12345678`、firmware `0x01`、`total_bytes 62_537_072_640`、health `pre_eol_info 1`、`life_time_est_a_percent Some(10)`、`life_time_est_b_percent Some(10)`、无 warning flags。
     - 设备 1（SD）：`name "mmc1:59b4"`、`/dev/mmcblk1`、`card_type "SD"`、model `SC64G`、`SanDisk (0x000045)`、`0x87654321`、firmware `0x01`、`total_bytes 64_026_691_584`、health `pre_eol_info 0`、`life_time_est_a_percent None`、`life_time_est_b_percent None`、无 warning flags。
   - 全部值与 brief 逐字一致。

## TDD 过程

1. **Red**：先写 3 个新测试（未改实现），`cargo test -p rsetup-core` 编译失败——`no method named mmc_status / storage_status found for struct Controller`（E0599，3 处），确认失败。
2. **Green**：按 brief 实现后重新运行，全部通过。
3. **Clippy**：`cargo clippy -p rsetup-core` 无警告。

## 新增测试（`actions.rs` `#[cfg(test)] mod tests`）

- `test_controller_mmc_status_demo`：Demo + DryRun；断言 `initialized == true`、`devices.len() == 2`、设备 0 eMMC（card_type `"MMC"`、block_path `/dev/mmcblk0`、total_bytes `62_537_072_640`、`life_time_est_a_percent == Some(10)`）、设备 1 SD（card_type `"SD"`、`life_time_est_a_percent == None`）。
- `test_controller_mmc_status_live`：Live + DryRun；仅断言不 panic 且返回 `Ok`（未初始化时 message 非空，未断言设备数量）。
- `test_controller_storage_status_demo`：Demo + DryRun；断言 `storage.nvme.initialized == true`、`storage.nvme.devices.len() == 1`、`storage.mmc.initialized == true`、`storage.mmc.devices.len() == 2`。

## 测试总结

`PATH="/usr/bin:$PATH" cargo test -p rsetup-core`：**117 passed; 0 failed; 2 ignored**（基线 114 + 新增 3；`actions::tests` 模块 15/15 通过，含 3 个新测试）。

## Concerns

无。

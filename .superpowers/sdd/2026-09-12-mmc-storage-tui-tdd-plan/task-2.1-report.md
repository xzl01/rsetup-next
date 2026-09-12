# Task 2.1 Report: 基于 sysfs 目录树的多设备探测 (MmcManager)

## Summary
- **Status**: DONE
- **Implemented Components**:
  1. `crates/rsetup-core/src/mmc/sys.rs`:
     - `read_trimmed_attr(path: &Path) -> Option<String>`: Reads and trims attributes from file.
     - `read_block_device_size(sysfs_root: &Path, block_name: &str) -> u64`: Reads `/sys/class/block/{block_name}/size` and converts 512-byte blocks into total capacity bytes (`blocks.saturating_mul(512)`).
     - `read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice, MmcError>`: Discovers attributes (`type`, `name`, `manfid`, `serial`, `fwrev`/`prv`/`hwrev`, `life_time`, `pre_eol_info`), extracts associated block device (`mmcblk*`), computes capacity and builds `MmcDevice`.
  2. `crates/rsetup-core/src/mmc.rs`:
     - `MmcManager` struct holding `status: MmcStatus` and `sysfs_root: PathBuf`.
     - `MmcManager::probe_and_init(sysfs_root: Option<&Path>) -> Self`
     - `MmcManager::new() -> Self`
     - `MmcManager::sysfs_root(&self) -> &Path`
     - `MmcManager::status(&self) -> MmcStatus`
     - `MmcManager::is_initialized(&self) -> bool`
     - `MmcManager::probe_sysfs(root: &Path) -> Vec<String>`: scans `sys/bus/mmc/devices`, filters by `type == "MMC"` or `"SD"`, excludes `SDIO`, sorts alphabetically.
  3. `crates/rsetup-core/src/lib.rs`:
     - Exported `pub use mmc::{MmcError, MmcManager};`.
  4. Unit Tests:
     - `test_mmc_probing_no_devices`: verified behavior on empty sysfs tree.
     - `test_mmc_probing_multi_devices`: verified multi-device detection (eMMC + SD, ignoring SDIO), capacity calculation, manufacturer name formatting, health metric parsing and warning flag generation.

## Test Verification
- Ran `PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc`:
  ```
  running 7 tests
  test mmc::tests::test_format_manufacturer ... ok
  test mmc::tests::test_generate_warning_flags ... ok
  test mmc::tests::test_parse_life_time ... ok
  test mmc::tests::test_parse_pre_eol_info ... ok
  test mmc::tests::test_mmc_probing_no_devices ... ok
  test model::tests::mmc_and_storage_models_serialize_and_deserialize ... ok
  test mmc::tests::test_mmc_probing_multi_devices ... ok

  test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 103 filtered out; finished in 0.00s
  ```
- Ran full test suite `PATH="/usr/bin:$PATH" cargo test -p rsetup-core`: 108 passed, 0 failed, 2 ignored.

## Commits
- `feat(core): implement MmcManager and sysfs multi-device probing`

## Review Finding Resolution: Primary MMC Block Device Filtering
- **Issue**: In `crates/rsetup-core/src/mmc/sys.rs`, directory entry scanning previously checked `name.starts_with("mmcblk")`. Due to non-deterministic filesystem order, `mmcblk0boot0`, `mmcblk0boot1`, `mmcblk0rpmb`, or `mmcblk0p1` could be chosen instead of the primary device node `mmcblk0`.
- **Fix**:
  - Implemented `pub fn is_primary_mmcblk(name: &str) -> bool` to strictly match names like `mmcblk[0-9]+` where the suffix after `mmcblk` consists purely of ASCII digits (non-empty), rejecting boot/rpmb/partitions.
  - Used `is_primary_mmcblk` in both `block` directory search and fallback `dev_dir` search.
  - Added unit test `test_mmc_primary_block_device_selection` asserting that `boot0`, `boot1`, `rpmb`, and `p1` alongside `mmcblk0` never match when `mmcblk0` is absent, and that `mmcblk0` is correctly selected regardless of directory order.
- **Verification**:
  - Ran `PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc`: 8 passed, 0 failed.
  - Ran `PATH="/usr/bin:$PATH" cargo test -p rsetup-core`: 109 passed, 0 failed, 2 ignored.


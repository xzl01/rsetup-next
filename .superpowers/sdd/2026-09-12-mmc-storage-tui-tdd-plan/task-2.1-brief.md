# Task 2.1 Brief: 基于 sysfs 目录树的多设备探测 (MmcManager)

## Requirements
In `crates/rsetup-core/src/mmc.rs` and `crates/rsetup-core/src/mmc/sys.rs`:

1. Create `crates/rsetup-core/src/mmc/sys.rs`:
   - Implement `pub fn read_trimmed_attr(path: &Path) -> Option<String>`
   - Implement `pub fn read_block_device_size(sysfs_root: &Path, block_name: &str) -> u64`:
     - Reads `/sys/class/block/{block_name}/size` (or relative to sysfs_root).
     - Blocks are 512-byte sectors: `blocks.saturating_mul(512)`.
   - Implement `pub fn read_device_sysfs(sysfs_root: &Path, dev_name: &str) -> Result<MmcDevice, MmcError>`:
     - `dev_dir` = `sys/bus/mmc/devices/{dev_name}` relative to `sysfs_root`.
     - Read `type`: "MMC" or "SD" or "SDIO". (If SDIO or empty, return error or handle accordingly; caller should filter).
     - Read attributes:
       - `name`: product name (from `name` attr, fallback to empty)
       - `manfid`: manufacturer ID string (from `manfid` attr) -> format using `format_manufacturer`
       - `serial`: serial string (from `serial` attr)
       - `fwrev` or `prv` or `hwrev`: read firmware (check `fwrev`, fallback to `prv` or `hwrev`, or empty)
       - `life_time`: read `life_time` attr -> parse using `parse_life_time_str`
       - `pre_eol_info`: read `pre_eol_info` attr -> parse using `parse_pre_eol_info_str`
       - `warning_flags`: generate using `generate_warning_flags(pre_eol, life_a, life_b)`
     - Determine block device:
       - Look under `dev_dir/block` or entries matching `block/mmcblk*`.
       - If not directly found under `block`, also check if `dev_dir` has entries starting with `mmcblk` (or read `sys/class/block`).
       - If a matching block node is found (e.g. `mmcblk0`), set `block_path = format!("/dev/{}", block_name)`.
       - Compute `total_bytes` from block device size. If not found, default to 0.
     - Construct and return `MmcDevice`.

2. In `crates/rsetup-core/src/mmc.rs`:
   - Define `pub struct MmcManager`:
     ```rust
     #[derive(Debug, Clone)]
     pub struct MmcManager {
         status: MmcStatus,
         sysfs_root: PathBuf,
     }
     ```
   - Implement `MmcManager`:
     - `pub fn probe_and_init(sysfs_root: Option<&Path>) -> Self`
     - `pub fn new() -> Self` -> delegates to `probe_and_init(None)`
     - `pub fn sysfs_root(&self) -> &Path`
     - `pub fn status(&self) -> MmcStatus`
     - `pub fn is_initialized(&self) -> bool`
     - `pub fn probe_sysfs(root: &Path) -> Vec<String>`:
       - Scans `sys/bus/mmc/devices` under `root`.
       - For each entry (e.g. `mmc0:0001`), checks `type` file.
       - If `type` is `"MMC"` or `"SD"`: include in the probe list.
       - If `type` is `"SDIO"`: exclude.
       - Sort device names before returning.
   - If `probe_sysfs` finds no devices, `status.initialized = false` and `message = Some("No MMC/SD devices detected in system".into())`.
   - If devices found, `status.initialized = true`, `devices` contains the mapped `MmcDevice`s.

3. Add Unit Tests:
   - `test_mmc_probing_no_devices()`: empty directory returns `initialized: false`.
   - `test_mmc_probing_multi_devices()`:
     - Setup temp dir with:
       - `sys/bus/mmc/devices/mmc0:0001`: type="MMC", name="FE4MB4", manfid="0x000015", serial="0x12345678", life_time="0x01 0x01", pre_eol_info="0x01", symlink/directory `block/mmcblk0`.
       - `sys/class/block/mmcblk0/size`: "122142720" (62,537,072,640 bytes = ~62.5 GB).
       - `sys/bus/mmc/devices/mmc1:59b4`: type="SD", name="SC64G", manfid="0x000045", serial="0x87654321", block/mmcblk1.
       - `sys/class/block/mmcblk1/size`: "124735488".
       - `sys/bus/mmc/devices/mmc2:0001`: type="SDIO", name="WIFI" (must be ignored!).
     - Assert `MmcManager` detects exactly 2 devices (`mmc0:0001` and `mmc1:59b4`).
     - Assert `mmc0:0001` has `card_type == "MMC"`, `total_bytes == 62537072640`, health has SLC 10% / MLC 10%.
     - Assert `mmc1:59b4` has `card_type == "SD"`.
     - Assert `initialized == true`.

4. Follow TDD:
   - Run `PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc`.
   - Ensure all tests pass. Commit the changes.

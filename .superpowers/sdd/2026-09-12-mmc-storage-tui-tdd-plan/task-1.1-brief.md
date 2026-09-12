# Task 1.1 Brief: 定义 MMC 与统一 Storage 数据模型

## Requirements
In `crates/rsetup-core/src/model.rs`:
1. Define the following structures with `#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]` and `#[serde(rename_all = "camelCase")]`:
- `MmcHealth`:
  - `pub pre_eol_info: u8`
  - `pub life_time_est_a_percent: Option<u8>`
  - `pub life_time_est_b_percent: Option<u8>`
  - `pub warning_flags: Vec<String>`
  - Also derive `Default`.
- `MmcDevice`:
  - `pub name: String`
  - `pub block_path: String`
  - `pub card_type: String`
  - `pub model: String`
  - `pub manufacturer: String`
  - `pub serial: String`
  - `pub firmware: String`
  - `pub total_bytes: u64`
  - `pub health: MmcHealth`
- `MmcStatus`:
  - `pub initialized: bool`
  - `pub devices: Vec<MmcDevice>`
  - `pub message: Option<String>`
- `StorageStatus`:
  - `pub nvme: NvmeStatus`
  - `pub mmc: MmcStatus`

2. Re-export these in `crates/rsetup-core/src/lib.rs` if needed or through `model`.
3. Add unit tests verifying serialization and deserialization in `crates/rsetup-core/src/model.rs` (or its tests).
4. Run `PATH="/usr/bin:$PATH" cargo test -p rsetup-core model` to verify.
5. Commit the changes.

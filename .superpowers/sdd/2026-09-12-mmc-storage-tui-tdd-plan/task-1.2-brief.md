# Task 1.2 Brief: MMC 寿命与 EXT_CSD 转换解析器

## Requirements
In `crates/rsetup-core/src/mmc.rs` (create the module and add `pub mod mmc;` to `crates/rsetup-core/src/lib.rs`):
1. Implement error enum `MmcError` with `thiserror::Error`:
   - `#[error("Invalid buffer length: expected {expected}, got {actual}")] InvalidBufferLength { expected: usize, actual: usize }`
   - `#[error("Device not supported: {0}")] NotSupported(String)`
   - `#[error("I/O error: {0}")] Io(String)`
2. Implement parser functions:
   - `pub fn parse_life_time_str(s: &str) -> (Option<u8>, Option<u8>)`:
     - Parses a string like `"0x01 0x02"` or `"0x01\n0x02"` or `"1 2"`.
     - Value mapping per JEDEC specification:
       - `0x00`: Not defined / unknown -> `None`
       - `0x01`..=`0x0A`: `val * 10` (e.g. `0x01` -> `Some(10)`, `0x02` -> `Some(20)`, ..., `0x0A` -> `Some(100)`)
       - `0x0B`: Exceeded 100% -> `Some(101)` (or >100%)
       - `0x0C`..: Reserved / invalid -> `None`
     - Returns `(Option<u8>, Option<u8>)` for (typ_a, typ_b). If string is empty or invalid format, return `(None, None)`.
   - `pub fn parse_pre_eol_info_str(s: &str) -> u8`:
     - Parses a string like `"0x01"` or `"1"`.
     - `0x01` -> 1 (Normal)
     - `0x02` -> 2 (Warning)
     - `0x03` -> 3 (Urgent)
     - Any other value or error -> 0 (Undefined/Unknown).
   - `pub fn parse_manfid_to_name(manfid: u32) -> &'static str`:
     - Common JEDEC manufacturers:
       - `0x15` -> `"Samsung"`
       - `0x90` -> `"SK Hynix"`
       - `0x13` -> `"Micron"`
       - `0x45` -> `"SanDisk"`
       - `0x70` -> `"Kingston"`
       - `0x11` -> `"Toshiba"`
       - `0xfe` -> `"Micron"`
       - default -> `"Unknown"`
   - `pub fn format_manufacturer(manfid_str: &str) -> String`:
     - Parses hexadecimal/decimal manfid string (e.g. `"0x000015"`) and formats as `"Samsung (0x000015)"` or `"0x000099"` if unknown.
   - `pub fn generate_warning_flags(pre_eol: u8, life_a: Option<u8>, life_b: Option<u8>) -> Vec<String>`:
     - If `pre_eol == 2`: add `"pre_eol_warning"`
     - If `pre_eol == 3`: add `"pre_eol_urgent"`
     - If `life_a.map(|v| v >= 100).unwrap_or(false)`: add `"life_time_typ_a_exceeded"`
     - If `life_b.map(|v| v >= 100).unwrap_or(false)`: add `"life_time_typ_b_exceeded"`
3. Add unit tests in `crates/rsetup-core/src/mmc.rs`:
   - `test_parse_life_time()`: valid pairs, exceeded, invalid values, empty string.
   - `test_parse_pre_eol_info()`: 1, 2, 3, invalid values.
   - `test_format_manufacturer()`: Samsung, SK Hynix, unknown id.
   - `test_generate_warning_flags()`: normal vs warning/urgent flags.
4. Follow TDD: test-first, run `PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc`, verify all pass.
5. Commit the changes.

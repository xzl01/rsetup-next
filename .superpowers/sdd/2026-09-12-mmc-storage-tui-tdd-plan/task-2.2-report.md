# Task 2.2 Report: Direct ioctl `MMC_IOC_CMD` Fallback 结构与函数

## Status: DONE

Commit: `aa5fa98` — `feat(core): add MMC_IOC_CMD ioctl fallback for EXT_CSD health data`
Branch: `dev-aghost` (parent commit `9237ef6`)

## TDD 流程

1. **Red**：先写入全部 5 个测试（`mmc.rs` 2 个 + `sys.rs` 3 个），运行
   `PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc`，观察到预期的编译失败
   （`map_life_time_byte_to_percent` / `MmcIocCmd` / `parse_ext_csd` /
   `read_ext_csd_raw` 均不存在）。期间修正了测试代码自身的一个笔误
   （`uuid::Uuid::new4()` → `new_v4()`，对齐既有测试写法）。
2. **Green**：实现后同一命令 13/13 通过；全 crate 回归 `cargo test -p rsetup-core`
   **114 passed, 0 failed, 2 ignored**。
3. **Verify**：`cargo build -p rsetup-core` 与 `cargo clippy -p rsetup-core
   --all-targets` 均零警告。

## 实现内容

### `crates/rsetup-core/src/mmc/sys.rs`

- `MmcIocCmd`（`#[repr(C)]`，与 `struct mmc_ioc_cmd` 逐字段对齐，14 个字段，
  含 `_pad: u32` 与 `data_ptr: u64`）。测试断言 `size_of::<MmcIocCmd>() == 72`
  且 `offset_of!(MmcIocCmd, data_ptr) == 64`（8 字节对齐，32/64 位一致）。
- `MMC_BLOCK_MAJOR: u8 = 179`（`include/uapi/linux/major.h`）。
- `MMC_IOC_CMD`：手工推导，参照 `nvme/sys.rs` 中 `NVME_IOCTL_ADMIN_CMD` 的
  注释风格——`dir = _IOC_READ|_IOC_WRITE = 3`，`size = 72 (0x48)`，
  `type = 179 (0xB3)`，`nr = 0` → `3<<30 | 72<<16 | 0xB3<<8 | 0 = 0xc048b300`。
  常量为 `3u64 << 30 | 72u64 << 16 | (MMC_BLOCK_MAJOR as u64) << 8` 推导得出
  （数值上恒等于 `0xc048b300`，同时消除 dead_code 警告）。
- 命令常量：`MMC_OPCODE_SEND_EXT_CSD = 8`、`MMC_RSP_R1 = 0x15`、
  `MMC_CMD_ADTC = 1 << 5`。
- EXT_CSD 偏移常量：`EXT_CSD_REV = 192`、`EXT_CSD_SEC_CNT = 212`、
  `EXT_CSD_FIRMWARE_VERSION = 254`、`EXT_CSD_PRE_EOL_INFO = 267`、
  `EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A = 268`、
  `EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B = 269`。
- `MmcExtCsd` 结构体 + `pub fn parse_ext_csd(buf: &[u8; 512]) -> MmcExtCsd`：
  纯安全代码；`sec_count` 按 `u32::from_le_bytes(buf[212..216])` 小端解析；
  `firmware_version` 为原始 8 字节。
- `pub fn read_ext_csd_raw(dev_path: &str) -> Result<[u8; 512], MmcError>`：
  风格完全对齐 `nvme/sys.rs::read_smart_log_raw`：`CString::new` →
  `libc::open(O_RDONLY)`（失败 → `MmcError::Io`）→ 填充 `MmcIocCmd`
  （`write_flag = 0`、`is_acmd = 0`、`opcode = 8`、`arg = 0`、
  `flags = MMC_RSP_R1 | MMC_CMD_ADTC`、`blksz = 512`、`blocks = 1`、
  其余为 0、`data_ptr = buf.as_mut_ptr() as u64`）→ `unsafe { libc::ioctl(fd,
  MMC_IOC_CMD, &mut cmd) }`，`ret < 0` 时取 `last_os_error()` →
  `MmcError::Io` → 无论成败都 `close(fd)`。
- `read_device_sysfs` fallback 逻辑：将 block 设备探测提前，随后当
  `life_a.is_none() && life_b.is_none() && pre_eol == 0 && !block_path.is_empty()`
  时调用 `read_ext_csd_raw(&block_path)`；成功则 `pre_eol_info` 取原值、寿命
  字节经 `map_life_time_byte_to_percent` 换算填充 `MmcHealth`；且当
  `fwrev/prv/hwrev` 全为空时，用 `firmware_version` 去掉尾部 NUL 后的 ASCII
  作为 firmware。ioctl 失败静默保持 sysfs 结果（`if let Ok` 即
  `unwrap_or_default` 语义），不产生错误日志。

### `crates/rsetup-core/src/mmc.rs`

- 将原私有 `map_life_time_val` 提取为
  `pub fn map_life_time_byte_to_percent(byte_val: u8) -> Option<u8>`
  （`0x00 → None`、`0x01..=0x0A → Some(val*10)`、`0x0B → Some(101)`、其余
  `None`），`parse_life_time_str` 改用该函数。

## 测试（5 个新增，全部通过）

| 测试 | 位置 | 结果 |
|---|---|---|
| `test_mmc_ioc_cmd_struct_size`（size == 72，data_ptr 偏移 64） | `sys.rs` | ok |
| `test_parse_ext_csd`（rev=8、sec_count=122_142_720、firmware `"0x01"+NUL`、pre_eol=0x02、typ_a=0x05、typ_b=0x06） | `sys.rs` | ok |
| `test_read_ext_csd_raw_nonexistent_device`（`/dev/nonexistent_mmc_device_xyz` → `Err(MmcError::Io)`） | `sys.rs` | ok |
| `test_map_life_time_byte_to_percent`（0x00→None、0x01→10、0x0A→100、0x0B→101、0x0C→None，另含 0x05→50） | `mmc.rs` | ok |
| `test_sysfs_fallback_to_ioctl_when_attributes_missing`（临时 sysfs 树，无 life_time/pre_eol_info/fwrev 等文件 + `sys/class/block/mmcblk0/size`；本机无 `/dev/mmcblk0`，ioctl 失败，断言设备正常构建：health 0/None、firmware 空、`block_path == "/dev/mmcblk0"`、`total_bytes == 122_142_720*512`、无 warning_flags） | `mmc.rs` | ok |

验证命令：`PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc`
→ `test result: ok. 13 passed; 0 failed`（含既有 mmc/model 测试回归）。

## 约束符合性

- `unsafe` 仅出现在 `read_ext_csd_raw` 的 open/ioctl/close（与 nvme/sys.rs 一致）；
  `parse_ext_csd` 与 `map_life_time_byte_to_percent` 为纯安全代码。
- 未引入任何新 crate 依赖（仅用已有的 `libc`）。
- 结构体大小/对齐由测试保证（72 字节、data_ptr@64）。

## 备注 / 关注点

- 本机为无 MMC 设备的 x86 开发机（`/dev/mmcblk*` 不存在，uid 1000 非 root），
  故 ioctl 成功路径（真实 `MMC_IOC_CMD` 读取）只能在本机以失败路径覆盖；
  成功路径的 ABI 正确性依赖结构体/常量与内核头逐字段对齐（已对照
  `include/uapi/linux/mmc/ioctl.h` 人工核验）及 Task 3 真机验证。
- `test_sysfs_fallback_to_ioctl_when_attributes_missing` 按 brief 要求验证的是
  "fallback 失败时不影响设备探测"；若该测试将来在有 `/dev/mmcblk0` 权限的
  机器上运行且 sysfs 属性确实缺失，ioctl 可能成功并填充 health 字段（这是
  期望行为，但该测试的断言针对失败路径编写）。真机回归时如遇到此情形，
  应视为 fallback 生效而非回归。
- `firmware` 字段在 fallback 成功时取 EXT_CSD 原始 ASCII（未做 `0x` 前缀等
  额外处理），符合 brief 的"去掉尾部 NUL 后的 ASCII"字面要求。

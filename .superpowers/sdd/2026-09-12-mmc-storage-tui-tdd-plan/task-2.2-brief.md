# Task 2.2 Brief: Direct ioctl `MMC_IOC_CMD` Fallback 结构与函数

## Background
参考内核头文件（`/home/aghost/workspace/aghost-ble-mesh-parent/tmp/linux-rockchip64-6.18.44/include/uapi/linux/mmc/ioctl.h`）与 `include/linux/mmc/core.h`、`include/linux/mmc/mmc.h`。
NVMe 模块中已有 `crates/rsetup-core/src/nvme/sys.rs` 的 `ioctl(NVME_IOCTL_ADMIN_CMD)` 先例，本任务对 MMC 采用完全相同风格：只读、纯 libc、零外部库。

## Requirements

### 1. `crates/rsetup-core/src/mmc/sys.rs` 新增内容

1. **内核 ABI 结构体**（与 `struct mmc_ioc_cmd` 逐字段对齐）：
```rust
#[repr(C)]
struct MmcIocCmd {
    write_flag: libc::c_int,
    is_acmd: libc::c_int,
    opcode: u32,
    arg: u32,
    response: [u32; 4],
    flags: u32,
    blksz: u32,
    blocks: u32,
    postsleep_min_us: u32,
    postsleep_max_us: u32,
    data_timeout_ns: u32,
    cmd_timeout_ms: u32,
    _pad: u32,
    data_ptr: u64,
}
```
   - 结构体大小必须为 **72 字节**（内核注释明确要求 32/64 位一致）。

2. **ioctl 命令字常量**：
   - `MMC_BLOCK_MAJOR = 179`（`include/uapi/linux/major.h`）
   - `MMC_IOC_CMD = _IOWR(179, 0, struct mmc_ioc_cmd)`。
   - 手工计算：dir=3 (RW), size=72 (0x48), type=179 (0xB3), nr=0 → `0xc048b300`。
   - 写清推导注释（参照 nvme/sys.rs 中 NVME_IOCTL_ADMIN_CMD 的注释风格）。

3. **常量**（来自 `include/linux/mmc/core.h` 与 `include/linux/mmc/mmc.h`）：
   - `MMC_OPCODE_SEND_EXT_CSD: u32 = 8`
   - `MMC_RSP_R1: u32 = 0x15`（`(1<<0)|(1<<2)|(1<<4)`）
   - `MMC_CMD_ADTC: u32 = 1 << 5`
   - EXT_CSD 偏移（`include/linux/mmc/mmc.h`）：`EXT_CSD_REV = 192`、`EXT_CSD_SEC_CNT = 212`（4 字节小端）、`EXT_CSD_FIRMWARE_VERSION = 254`（8 字节）、`EXT_CSD_PRE_EOL_INFO = 267`、`EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_A = 268`、`EXT_CSD_DEVICE_LIFE_TIME_EST_TYP_B = 269`。

4. **EXT_CSD 解析**：
```rust
pub struct MmcExtCsd {
    pub rev: u8,
    pub sec_count: u32,           // 扇区数（512 字节/扇区）
    pub firmware_version: [u8; 8],
    pub pre_eol_info: u8,
    pub life_time_est_typ_a: u8,  // 原始字节值（0x00 未定义 … 0x0B 耗尽）
    pub life_time_est_typ_b: u8,
}

pub fn parse_ext_csd(buf: &[u8; 512]) -> MmcExtCsd
```
   - `rev = buf[192]`；`sec_count = u32::from_le_bytes(buf[212..216])`；
   - `firmware_version = buf[254..262]`；`pre_eol_info = buf[267]`；`life_time_est_typ_a = buf[268]`；`life_time_est_typ_b = buf[269]`。

5. **原始 ioctl 读取**：
```rust
pub fn read_ext_csd_raw(dev_path: &str) -> Result<[u8; 512], MmcError>
```
   - 风格完全参照 `nvme/sys.rs::read_smart_log_raw`：
     - `CString::new(dev_path)` → `libc::open(path, O_RDONLY)` → 失败返回 `MmcError::Io`；
     - 填充 `MmcIocCmd`：`write_flag = 0`（读）、`is_acmd = 0`、`opcode = MMC_OPCODE_SEND_EXT_CSD`、`arg = 0`（EXT_CSD 从偏移 0 开始）、`flags = MMC_RSP_R1 | MMC_CMD_ADTC`、`blksz = 512`、`blocks = 1`、其余为 0、`data_ptr = buf.as_mut_ptr() as u64`；
     - `unsafe { libc::ioctl(fd, MMC_IOC_CMD, &mut cmd) }`，ret < 0 时取 `last_os_error` 返回 `MmcError::Io`；
     - 无论成败都 `close(fd)`。

6. **接入 `read_device_sysfs` 的 fallback 逻辑**：
   - 当 sysfs 的 `life_time`、`pre_eol_info` 均缺失/为空（即解析结果为 `life_a = None && life_b = None && pre_eol == 0`）且 `block_path` 非空时，尝试 `read_ext_csd_raw(&block_path)`；
   - 成功则：用 `parse_ext_csd` 结果填充 `MmcHealth`（`pre_eol_info` 直接取原值；寿命字节经 Task 1.2 的映射换算：`0x01..=0x0A → val*10`、`0x0B → 101`、其余 `None`——请在 mmc.rs 中把 Task 1.2 的映射提取为 `pub fn map_life_time_byte_to_percent(byte_val: u8) -> Option<u8>` 并复用，`parse_life_time_str` 也改用该函数）；
   - 若 sysfs 的 `fwrev/prv/hwrev` 也全为空且 ioctl 成功，用 `firmware_version` 去掉尾部 NUL 后的 ASCII 作为 firmware；
   - ioctl 失败则静默保持 sysfs 结果（`unwrap_or_default` 语义），不产生错误日志刷屏。

### 2. 单元测试（`crates/rsetup-core/src/mmc.rs` 或 sys.rs 的 `#[cfg(test)]`）

1. `test_mmc_ioc_cmd_struct_size`：`std::mem::size_of::<MmcIocCmd>() == 72`，且 `data_ptr` 偏移为 64。
2. `test_parse_ext_csd`：构造 512 字节全零缓冲，按上述偏移写入已知值（rev=8、sec_count=122_142_720、firmware "0x01" + NUL、pre_eol=0x02、typ_a=0x05、typ_b=0x06），断言解析结果全部正确。
3. `test_read_ext_csd_raw_nonexistent_device`：`read_ext_csd_raw("/dev/nonexistent_mmc_device_xyz")` 返回 `Err`。
4. `test_map_life_time_byte_to_percent`：0x00→None、0x01→Some(10)、0x0A→Some(100)、0x0B→Some(101)、0x0C→None。
5. `test_sysfs_fallback_to_ioctl_when_attributes_missing`：
   - 临时目录构造 `sys/bus/mmc/devices/mmc0:0001`（type="MMC"，无 life_time/pre_eol_info/fwrev 文件）与 `sys/class/block/mmcblk0/size`；
   - `read_device_sysfs` 在真实测试机上无 `/dev/mmcblk0`，ioctl 会失败，断言设备仍能正常构建（health 字段为 0/None，firmware 为空）——验证 fallback 路径失败时不 panic、不影响设备探测。

### 3. 约束
- `MmcIocCmd`、`parse_ext_csd`、`read_ext_csd_raw` 中 ioctl 部分使用 `unsafe`（与 nvme/sys.rs 一致）；`parse_ext_csd` 本身保持纯安全代码。
- 不得引入任何新 crate 依赖（libc 已在依赖中）。
- 运行 `PATH="/usr/bin:$PATH" cargo test -p rsetup-core mmc`，全部通过后提交。

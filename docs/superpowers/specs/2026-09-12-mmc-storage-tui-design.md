# 基于内核接口的多盘 MMC 支持与统一存储 (Storage) TUI 设计规范

## 1. 概述与设计原则

在 Rockchip、Allwinner 等嵌入式 SBC（单板计算机）平台中，存储介质通常包括板载 eMMC、外置 MicroSD (TF) 卡以及通过 M.2/PCIe 接入的 NVMe SSD。
当前 `rsetup-next` 已经实现了直接基于 Linux 内核接口（sysfs + `ioctl(NVME_IOCTL_ADMIN_CMD)`）的 NVMe 硬件监控，具有零外部共享库（如 `libnvme.so`）依赖的特性，并在 TUI 中拥有专属显示区域。

本规范设计以下两个关键特性：
1. **多 MMC 设备原生内核采集**：
   - 遵循与 NVMe 相同的原则：**零外部用户态工具依赖**（不依赖 `mmc-utils`、`lsblk` 或其它外部二进制程序）；
   - 支持系统挂载的**多个 MMC/SD 设备**并发枚举（例如 `mmcblk0` 作为板载 eMMC，`mmcblk1` 作为 MicroSD 卡）；
   - 优先通过 Linux 内核 sysfs 节点提取设备拓扑、元数据（型号、厂商、生产日期、序列号等）和 JEDEC 5.0+ Extended CSD (EXT_CSD) 健康寿命指标；必要时支持 `ioctl(MMC_IOC_CMD)` 发起直接只读查询。
2. **多 NVMe / 多 MMC 统一存储 (Storage) TUI 呈现**：
   - 将终端界面（TUI）原有的独立 NVMe 视窗重构合并为统一的 **“存储 (Storage)”** 区域；
   - 动态汇聚并展示全部检测到的 NVMe 设备与 MMC/SD 设备；
   - 针对单盘与多盘（例如 2 块 NVMe + 1 块 eMMC + 1 块 SD）场景设计紧凑、自适应的高度分配算法与指标折叠策略，确保终端视窗无溢出与截断。

---

## 2. 内核接口与指标采集

### 2.1 拓扑与静态属性读取
- **sysfs 枚举路径**：
  - 扫描 `/sys/bus/mmc/devices/` 目录下的所有实体项（条目命名一般为 `mmcX:XXXX`，例如 `mmc0:0001`、`mmc1:59b4`）。
  - 对每个条目检查 `type` 属性：
    - `MMC`：标识为板载 eMMC 芯片；
    - `SD`：标识为 SD / MicroSD 卡；
    - `SDIO`：无线/蓝牙接口芯片（在块存储探测中静默过滤排除）。
  - 关联块设备节点：
    - 遍历子目录中匹配 `block/mmcblk*` 的软链接，确定主设备节点（如 `/dev/mmcblk0`），过滤忽略 `mmcblk0boot0`、`mmcblk0boot1`、`mmcblk0rpmb` 等物理子分区作为独立驱动器处理。
    - 从 `/sys/class/block/<name>/size` 读取扇区数，按 512 字节/扇区计算格式化总容量。
- **静态元数据**：
  - `name`: 硬件产品型号（如 `FE4MB4`）
  - `manfid`: 厂商 ID（16进制字符串，如 `0x000015` 代表 Samsung，`0x000090` 代表 SK Hynix，`0x000013` 代表 Micron，`0x000045` 代表 SanDisk）
  - `oemid`: OEM 识别码
  - `serial`: 序列号（如 `0x12ab34cd`）
  - `fwrev` / `prv` / `hwrev`: 固件与产品修订版本
  - `date`: 生产日期（MM/YYYY）

### 2.2 健康度与寿命遥测（SMART 等价物）
对于 eMMC (JEDEC 5.0+)，内核驱动会在设备初始化时读取 EXT_CSD 并通过 sysfs 暴露健康信息：
1. **内核 sysfs 节点（无特权安全读取）**：
   - `/sys/bus/mmc/devices/mmcX:XXXX/life_time`：
     - 输出两个以空格分隔的十六进制字节，如 `0x01 0x02`。
     - 第一个字节为 `DEVICE_LIFE_TIME_EST_TYP_A`（通常对应 SLC Cache / 预留区）：
       - `0x01`: 0% - 10% 寿命耗损
       - `0x02`: 10% - 20%
       - ...
       - `0x0B`: 超过 100%（预警耗尽）
     - 第二个字节为 `DEVICE_LIFE_TIME_EST_TYP_B`（通常对应 MLC/TLC 主存储区）。
   - `/sys/bus/mmc/devices/mmcX:XXXX/pre_eol_info`：
     - `0x01`: Normal (正常)
     - `0x02`: Warning (耗损已达 80% 阈值，预警)
     - `0x03`: Urgent (耗损严重，建议立即更换备份)
2. **Direct `ioctl(MMC_IOC_CMD)` Fallback**：
   - 当内核未开启或导出 sysfs `life_time` 属性时，通过内核头文件 `linux/mmc/ioctl.h` 规范：
     - 命令字：`MMC_IOC_CMD = _IOWR(179, 0, struct mmc_ioc_cmd)`
     - 结构体填充：`opcode = MMC_SEND_EXT_CSD (8)`，`flags = MMC_RSP_R1 | MMC_CMD_ADTC`，`blksz = 512`，`blocks = 1`
     - 解析 512 字节返回数据：byte 267 (`pre_eol_info`), byte 268 (`life_time_est_typ_a`), byte 269 (`life_time_est_typ_b`)。
   - 读取操作为只读，绝不触发写操作。

---

## 3. 数据模型设计 (`crates/rsetup-core/src/model.rs`)

```rust
/// MMC 设备的寿命及健康遥测信息
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MmcHealth {
    pub pre_eol_info: u8,                    // 0: 未知/未支持, 1: 正常, 2: 预警(80%), 3: 紧急
    pub life_time_est_a_percent: Option<u8>, // SLC 估算使用寿命百分比 (0-100+)
    pub life_time_est_b_percent: Option<u8>, // MLC/TLC 估算使用寿命百分比 (0-100+)
    pub warning_flags: Vec<String>,          // 格式化告警标识
}

/// 单个 MMC 存储设备
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MmcDevice {
    pub name: String,         // sysfs 设备名，如 "mmc0:0001"
    pub block_path: String,   // 块设备路径，如 "/dev/mmcblk0"
    pub card_type: String,    // "MMC" 或 "SD"
    pub model: String,        // 产品名称，如 "FE4MB4"
    pub manufacturer: String, // 厂商标识，如 "Samsung (0x000015)"
    pub serial: String,       // 序列号
    pub firmware: String,     // 固件版本
    pub total_bytes: u64,     // 格式化总容量
    pub health: MmcHealth,    // 寿命与健康指标
}

/// MMC 子系统状态
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MmcStatus {
    pub initialized: bool,
    pub devices: Vec<MmcDevice>, // 支持包含多个 MMC/SD 设备
    pub message: Option<String>,
}

/// 统一存储系统状态（汇聚 NVMe 与 MMC）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct StorageStatus {
    pub nvme: NvmeStatus,
    pub mmc: MmcStatus,
}
```

---

## 4. 架构与模块组织

1. **`crates/rsetup-core/src/mmc.rs`**：
   - 定义 `MmcManager`，支持通过 `sysfs_root: Option<&Path>` 依赖注入，便于全平台测试与 Mock。
   - `pub fn probe_and_init(sysfs_root: Option<&Path>) -> Self`：按顺序探测并枚举所有可用 MMC 设备。
   - 解析 `life_time` 字节与预警标志。
2. **`crates/rsetup-core/src/mmc/sys.rs`**：
   - 封装底层的 `read_device_sysfs` 与 `ioctl(MMC_IOC_CMD)` 读取逻辑。
3. **`crates/rsetup-core/src/actions.rs`**：
   - 控制器整合：`Controller` 新增 `mmc: Arc<MmcManager>` 成员。
   - 增加公开接口：
     - `pub fn mmc_status(&self) -> Result<MmcStatus, HardwareError>`
     - `pub fn storage_status(&self) -> Result<StorageStatus, HardwareError>`
   - Demo 模式下返回包含多块存储的合成状态（模拟 1 块 NVMe + 1 块 eMMC + 1 块 SD 卡）。
4. **`crates/rsetup-app/src/main.rs`**：
   - CLI 新增子命令：`rsetup-next hardware mmc [--json]`
   - CLI 新增汇总命令：`rsetup-next hardware storage [--json]`
   - 格式化输出多个 MMC 与 NVMe 的设备树及健康报告。

---

## 5. TUI “存储 (Storage)” 视窗整合

### 5.1 布局与排版规范
1. **替换目标**：
   - 将原 `render_nvme_summary` 函数完全重命名并升级为 `render_storage_summary`。
   - 标题统一为：`storage_telemetry`（中: `"存储状态"`, 英: `"Storage Devices"`）。
2. **多盘高度动态规划**：
   - 设总设备数 $N = N_{\text{nvme}} + N_{\text{mmc}}$。
   - 当 $N = 0$ 时：占用高度 3 行（边框 + 1 行提示信息 `storage_not_detected`）。
   - 当 $N = 1$ 时：占用高度 5~6 行，支持完整规格展开（设备节点、型号、容量、健康状态、温度、已用寿命、备用空间/阈值）。
   - 当 $N \ge 2$ 时：
     - 采用每盘 2 行紧凑排版：
       - **行 1 (基础信息)**：`[类型] 设备名称 (块设备) · 型号 · 格式化容量`
       - **行 2 (健康度与指标)**：
         - NVMe: `状态: 正常 · 44.0 °C · 寿命消耗: 0% · 备用: 100% · 读 12.3G / 写 4.5G`
         - eMMC: `状态: 正常 · 寿命耗损: SLC <10% / MLC <10% · 预警: 正常 · 固件: 0x01`
         - SD 卡: `状态: 正常 · 厂商: SanDisk · 序列号: 0x12ab34cd`
     - 动态计算高度：$H = \min(\text{剩余可用高度}, 2 + 2 \times N)$。
     - 若总空间不足以显示全部设备，保留能完整容纳的项并在末尾附上 `... (+M more devices)`。

### 5.2 示意图

```text
┌ 存储状态 (Storage) ────────────────────────────────────────────────────────┐
│ [NVMe] nvme0 (/dev/nvme0n1) · ZHITAI TiPlus7100 · 953.9 GiB                │
│   状态: 正常 · 44.0 °C · 寿命消耗: 0% · 备用: 100% · 读 12.3G / 写 4.5G    │
│ [NVMe] nvme1 (/dev/nvme1n1) · Samsung 980 PRO 1TB · 931.5 GiB              │
│   状态: 正常 · 48.0 °C · 寿命消耗: 5% · 备用: 100% · 读 250G / 写 180G     │
│ [eMMC] mmcblk0 (FE4MB4) · Samsung · 58.2 GiB                               │
│   状态: 正常 · 寿命耗损: SLC <10% / MLC <10% · 预警: 正常                  │
│ [SD]   mmcblk1 (SC64G) · SanDisk · 59.5 GiB                                │
│   状态: 正常 · 序列号: 0x24a87b1c · 固件: 0x01                            │
└────────────────────────────────────────────────────────────────────────────┘
```

---

## 6. 测试与质量保证

1. **零外部库编译检查**：确保所有代码使用标准库与 libc 构建，在无目标平台专用用户态工具下均可正常编译。
2. **内核 Mock 单元测试**：使用临时目录构造全套 sysfs 模拟结构，包括：
   - 多个 NVMe 设备（`nvme0`, `nvme1`）；
   - 多个 MMC 设备（`mmc0:0001` 作为 eMMC, `mmc1:0001` 作为 SD 卡）；
   - 边界测试：缺失 `life_time` 属性、属性非法格式、空设备目录、非数值字符等。
3. **TUI TestBackend 自动化渲染测试**：
   - 单盘 NVMe 渲染测试；
   - 单盘 MMC 渲染测试；
   - 多盘 NVMe + 多盘 MMC 紧凑排版渲染测试；
   - 无设备空状态渲染测试；
   - 中英多语言断言。

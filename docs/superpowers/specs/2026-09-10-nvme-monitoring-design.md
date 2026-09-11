# NVMe 磁盘状态监测模块设计规范 (Spec)

## 1. 概述与背景

`rsetup-next` 作为 Linux 单板计算机 (SBC) 的统一控制平面，需要提供对挂载 NVMe 固态硬盘（如 M.2 NVMe SSD）的健康与状态观测能力。
为确保极低的运行时开销与自包含性，本功能**不依赖任何外部 NVMe 库**：通过 Linux 内核的 NVMe Admin Passthru `ioctl` 直接读取设备的只读识别信息与 SMART/Health 日志页，指标解析在进程内完成（详见 2.2）。

### 核心约束
1. **启动时按需侦测与条件初始化**：在应用启动加载硬件模块时，首先执行底层侦测；仅当系统存在至少一个物理 NVMe 控制器/设备时，才正式初始化并激活 NVMe 管理子模块。若未侦测到 NVMe 硬件，该模块保持未初始化（Uninitialized/Disabled）状态。
2. **零外部依赖的采集路径**：不链接 `libnvme`（既非静态也非动态），不调用任何 `libnvme` 符号。SMART/Health 数据经 `ioctl(NVME_IOCTL_ADMIN_CMD)` 从 `/dev/nvmeX` 读取，因此构建产物在目标 SBC 上运行时无需安装 `libnvme.so.1`，交叉编译时也无需目标架构的 `libnvme` 开发包。设计决策与依据见 2.2。
3. **只读安全性**：本模块为纯观察型硬件监控，仅发送 NVMe Identify 与 Log Page（SMART / Health）只读查询指令，不包含格式化、固件写入或任何破坏性写操作。

---

## 2. 架构与生命周期

### 2.1 启动侦测与条件初始化流程

```text
应用启动 (Controller::new / from_environment)
      │
      ▼
侦测系统环境 (Linux sysfs /sys/class/nvme)
      │
      ├─► [无 NVMe 设备] ──► NVMe 模块标记为 Uninitialized (supported: false)
      │                     ├── API/CLI: 返回 nvme.available = false, devices = []
      │                     └── Web/TUI: 硬件页面隐藏或置灰 NVMe 监控卡片
      │
      ▼
  [发现 ≥1 个 NVMe 设备]
      │
      ▼
初始化 NvmeManager / NvmeProvider
      │
      ├── 加载拓扑 (Controllers, Namespaces, Paths)
      ├── 读取 Identify Controller/Namespace
      └── 轮询或按需读取 SMART Log (温度, 备用空间, 寿命消耗, 读写字节, 警告标志)
```

### 2.2 采集路径与依赖策略（无外部库）

采集实现位于 `crates/rsetup-core/src/nvme/sys.rs`：

- **设备拓扑与静态属性**：读取 `/sys/class/nvme/nvmeX/` 下的 `model`、`serial`、`firmware_rev`，并由关联命名空间（如 `nvme0n1/size`）换算容量，不触发任何设备命令。
- **SMART / Health 日志**：对 `/dev/nvmeX` 发起 Linux Admin Passthru `ioctl`（`NVME_IOCTL_ADMIN_CMD = 0xc0484e41`，`opcode 0x02` Get Log Page，`LID 0x02` SMART/Health），读取 512 字节日志缓冲区，再由 `parse_smart_log` 在进程内解析为 `NvmeSmartLog`。
- **失败降级**：设备节点不存在或权限不足时返回 `NvmeError::Io`，不 panic、不阻断其他硬件模块。

#### 为什么不用 libnvme

早期设计曾要求静态链接 `libnvme`。实现阶段改为直接使用内核 `ioctl` 接口，理由：

1. **自包含性更强**：`ioctl` 路径不引入任何库依赖，产物天然不依赖 `libnvme.so.1`。
2. **交叉编译更简单**：静态链接 `libnvme` 需要目标架构的 `libnvme.a` 及其传递依赖（如 `json-c`、`libuuid`）；实测本机 `/usr/lib/aarch64-linux-gnu` 下并无 `libnvme`，交叉编译无法满足该前提。
3. **接口面更小**：仅需一个 Log Page 读取，`libnvme` 的拓扑扫描与命令封装属于用不到的能力，符合 YAGNI。

因此 `crates/rsetup-core/build.rs` 及其 `libnvme` 探测/链接逻辑已被**整体移除**；仓库中不存在对 `libnvme` 的任何构建期或运行期引用。

> 变更记录：该决定与移除动作见 `docs/testing/nvme-2026-09-12/report.md`；对应的历史计划文档 `docs/superpowers/plans/2026-09-10-nvme-monitoring-tdd-plan.md` 保留原始阶段 3 描述并附有偏离说明。

---

## 3. 数据模型设计 (`rsetup-core::model`)

### 3.1 数据结构定义

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NvmeStatus {
    /// 模块是否侦测并成功初始化
    pub initialized: bool,
    /// 侦测到的 NVMe 设备列表
    pub devices: Vec<NvmeDevice>,
    /// 未初始化或不可用时的原因说明
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NvmeDevice {
    /// 控制器设备名称（例如 nvme0）
    pub name: String,
    /// 主命名空间设备路径（例如 /dev/nvme0n1）
    pub path: String,
    /// 硬件型号（Model Number，如 "Samsung SSD 980 500GB"）
    pub model: String,
    /// 设备序列号（Serial Number）
    pub serial: String,
    /// 固件版本（Firmware Revision）
    pub firmware: String,
    /// 容量总字节数
    pub total_bytes: u64,
    /// SMART 健康状态数据
    pub smart: NvmeSmartLog,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NvmeSmartLog {
    /// 关键警告标志掩码 (Critical Warning: 0 表示正常, 掩码包含备用空间低、温度超标、只读模式等)
    pub critical_warning: u8,
    /// 警告标志可读列表 (例如 ["spare_below_threshold", "temperature_exceeded"])
    pub warning_flags: Vec<String>,
    /// 摄氏度温度 (解析自 Composite Temperature)
    pub temperature_c: f32,
    /// 可用备用空间百分比 (Available Spare: 0-100%)
    pub available_spare_percent: u8,
    /// 备用空间阈值 (Available Spare Threshold)
    pub spare_threshold_percent: u8,
    /// 已使用寿命百分比 (Percentage Used: 0-100%，超过 100% 代表超出设计寿命)
    pub percentage_used: u8,
    /// 数据读取量 (以字节为单位计算，底层为 1000 个 512 字节单元的数据单元数)
    pub data_read_bytes: u64,
    /// 数据写入量 (以字节为单位计算)
    pub data_written_bytes: u64,
    /// 读操作总次数
    pub host_reads: u64,
    /// 写操作总次数
    pub host_writes: u64,
    /// 通电时间（小时）
    pub power_on_hours: u64,
    /// 不安全关机次数
    pub unsafe_shutdowns: u64,
    /// 介质与数据完整性错误计数
    pub media_errors: u64,
    /// 错误日志项计数
    pub num_err_log_entries: u64,
}
```

---

## 4. 接口与各层呈现策略

### 4.1 Core 控制层 (`Controller`)
- `pub fn nvme_status(&self) -> Result<NvmeStatus, HardwareError>`
- 在 `Controller::new` / `Controller::from_environment` 时执行初始化探测：
  - 检查 `/sys/class/nvme` 目录下是否存在 `nvme*` 条目。
  - 若存在，尝试初始化 `NvmeManager` 并保留实例；若不存在，保留 `None`，并标记 `initialized: false`。

### 4.2 CLI (`rsetup-next`)
- 新增命令：`rsetup-next hardware nvme`
  - 参数：`--json` 支持输出标准 JSON。
  - 格式化文本输出：
    - 若未检测到 NVMe 硬件，输出：`No NVMe controllers detected. NVMe hardware module is disabled.`（中英支持）。
    - 若检测到，输出各 NVMe 设备的型号、容量、健康状态、温度、剩余寿命、读写统计与警告指示。

### 4.3 HTTP API 与 Web GUI / Tauri
- HTTP 端点：`GET /api/v1/hardware/nvme`
  - 返回 `NvmeStatus` JSON。
- Web GUI (前端):
  - 在 Hardware 工具箱中，根据 `nvmeStatus.initialized` 动态渲染卡片。
  - 若为 `false`，界面优雅提示“未检测到 NVMe 存储设备，模块未激活”。
  - 若为 `true`，以规整的 Soft UI 卡片呈现磁盘概览、温度计量条、寿命百分比环形/进度条及 SMART 告警指示。
- Tauri:
  - 注册 `nvme_status` command，直接桥接 `Controller::nvme_status`。

---

## 5. 演示模式 (Demo / Synthetic)
- 在 `--demo` 模式或非 Linux 平台：
  - 构造一个模拟的 NVMe 设备（如 `"Radxa NVMe SSD 256GB"`, 序列号 `"RADXA2026NVME01"`，温度 42°C，剩余可用空间 100%，已消耗寿命 3%，无告警）。
  - 确保本地开箱即用的前端调试与 UI 预览保持完整。

---

## 6. 测试与质量保证
1. **单元测试**：
   - 数据解析测试：验证从原生 512 字节 SMART Log 字节缓冲区解析为 `NvmeSmartLog` 结构的准确性（字节序转换、单位换算、告警掩码拆解）。
   - 条件初始化测试：模拟无 NVMe 目录与有 NVMe 设备两种启动场景，断言 `initialized` 状态与行为。
   - 演示模式数据完整性测试。
2. **跨平台兼容测试**：
   - 保证在缺少 `libnvme` 静态库的主机或非 Linux 架构下，通过模拟接口顺利编译并通过 `cargo test`。

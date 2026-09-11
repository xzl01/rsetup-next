# 多盘 MMC 支持与统一存储 (Storage) TUI 实施计划 (TDD Plan)

**目标**：按照设计规范 `docs/superpowers/specs/2026-09-12-mmc-storage-tui-design.md`，使用测试驱动开发 (TDD) 方式，基于 Linux 内核接口实现多 MMC 设备的探测与健康指标采集，并将 TUI 的 NVMe 视窗重构为多 NVMe 与多 MMC 汇聚的统一“存储 (Storage)”监控区域。

**设计原则**：
- **测试先行 (Test-First)**：先编写针对新结构体、枚举与解析逻辑的失败测试，再实现逻辑使其通过。
- **零外部依赖**：不依赖 `mmc-utils` 二进制，使用标准 sysfs 与 libc ioctl。
- **渐进迭代**：拆分为数据模型 -> MMC 核心探测 -> 控制器与 CLI 整合 -> TUI 存储区域重构 四个独立阶段。

---

## 阶段 1：数据模型与健康指标解析逻辑 (TDD)

### 任务 1.1：定义 MMC 数据模型
- **文件**：`crates/rsetup-core/src/model.rs`
- **步骤**：
  1. 编写测试：验证 `MmcHealth`、`MmcDevice`、`MmcStatus` 与 `StorageStatus` 的序列化与反序列化。
  2. 在 `model.rs` 中增加上述结构体定义与 serde 属性。
  3. 运行测试：`cargo test -p rsetup-core model` 确保通过。

### 任务 1.2：MMC 寿命与 EXT_CSD 转换解析器
- **文件**：`crates/rsetup-core/src/mmc.rs`
- **步骤**：
  1. 编写测试 `test_parse_life_time_hex()`：
     - 输入 `"0x01 0x02"` -> 期望 A: 10%, B: 20%；
     - 输入 `"0x0B 0x01"` -> 期望 A: >100% (耗尽), B: 10%；
     - 异常输入（单值、空值、非法字符）返回 `None`。
  2. 编写测试 `test_parse_pre_eol_info()`：
     - 输入 `"0x01"` (Normal), `"0x02"` (Warning), `"0x03"` (Urgent)。
  3. 实现解析函数 `parse_life_time_str` 与 `parse_pre_eol_info_str`。
  4. 运行测试验证通过。

---

## 阶段 2：MMC 内核探测器实现 (TDD)

### 任务 2.1：基于 sysfs 目录树的多设备探测
- **文件**：`crates/rsetup-core/src/mmc.rs`、`crates/rsetup-core/src/mmc/sys.rs`
- **步骤**：
  1. 编写测试 `test_mmc_probing_no_devices()`：模拟空 sysfs 目录，验证返回 `initialized = false`。
  2. 编写测试 `test_mmc_probing_multi_devices()`：
     - 在临时测试目录构造：
       - `sys/bus/mmc/devices/mmc0:0001`（`type="MMC"`, `name="FE4MB4"`, `manfid="0x000015"`, `serial="0x12345678"`, `life_time="0x01 0x01"`, `pre_eol_info="0x01"`, 包含块设备 `block/mmcblk0` 其 `size` 对应 64GB）；
       - `sys/bus/mmc/devices/mmc1:59b4`（`type="SD"`, `name="SC64G"`, `manfid="0x000045"`, 包含块设备 `block/mmcblk1`）；
       - `sys/bus/mmc/devices/mmc2:0001`（`type="SDIO"`, Wi-Fi 芯片，期望被探测逻辑过滤忽略）。
     - 断言探测到 2 个设备，正确解析 `card_type`（eMMC 与 SD），容量、型号及寿命估计。
  3. 实现 `MmcManager::probe_and_init(sysfs_root: Option<&Path>)` 及 `read_device_sysfs`。
  4. 运行测试：`cargo test -p rsetup-core mmc` 验证通过。

### 任务 2.2：Direct ioctl `MMC_IOC_CMD` Fallback 结构与函数
- **文件**：`crates/rsetup-core/src/mmc/sys.rs`
- **步骤**：
  1. 定义 `struct MmcIocCmd` 映射 Linux 内核 `mmc_ioc_cmd`（`_IOWR(179, 0, struct mmc_ioc_cmd)`）。
  2. 编写模拟测试验证 ioctl 命令结构体大小和对齐符合 64 位平台 ABI。
  3. 编写 `read_ext_csd_raw` 函数（对 `/dev/mmcblkX` 执行只读查询）。

---

## 阶段 3：控制器 (Controller) 与 CLI 命令扩展 (TDD)

### 任务 3.1：Controller 状态聚合与 Demo 模式
- **文件**：`crates/rsetup-core/src/actions.rs`
- **步骤**：
  1. 编写测试：
     - `test_controller_mmc_status_demo()`：Demo 模式下返回模拟的 eMMC 与 SD 卡设备；
     - `test_controller_storage_status_demo()`：返回包含多 NVMe + 多 MMC 的 `StorageStatus`；
     - `test_controller_mmc_status_live()`。
  2. 在 `Controller` 中增加 `mmc: Arc<MmcManager>`。
  3. 实现 `controller.mmc_status()` 与 `controller.storage_status()`。
  4. 运行测试：`cargo test -p rsetup-core actions` 确保通过。

### 任务 3.2：CLI 命令与格式化输出
- **文件**：`crates/rsetup-app/src/main.rs`
- **步骤**：
  1. 编写测试：`test_cli_parses_mmc_and_storage_subcommands()`。
  2. 编写测试：`test_format_mmc_status_output()` 与 `test_format_storage_status_output()`。
  3. 在 `HardwareCommands` 中增加 `Mmc { json: bool }` 与 `Storage { json: bool }`。
  4. 实现命令行格式化展示。
  5. 运行测试：`cargo test -p rsetup-app main` 确保通过。

---

## 阶段 4：TUI 存储视窗重构 (统一呈现多 NVMe + 多 MMC)

### 任务 4.1：国际化与字典键扩展
- **文件**：`crates/rsetup-app/src/i18n.rs`
- **步骤**：
  1. 编写测试：`test_storage_tui_dictionary_keys()` 验证中英文词条：
     - `"storage_telemetry"`, `"storage_not_detected"`, `"storage_healthy"`, `"storage_warning"`, `"mmc_life_time"`, `"mmc_slc"`, `"mmc_mlc"`。
  2. 在 `i18n.rs` 中补全词条映射。
  3. 运行测试：`cargo test -p rsetup-app i18n` 确保通过。

### 任务 4.2：TUI App 结构与刷新循环接入
- **文件**：`crates/rsetup-app/src/tui.rs`
- **步骤**：
  1. 编写测试：`test_tui_app_loads_storage_status()` 验证 `App::new` 和 `App::refresh` 均能正确采集 `nvme_status` 与 `mmc_status`。
  2. 在 `App` 结构体中添加 `pub(crate) mmc_status: rsetup_core::MmcStatus`（或统一的 `storage_status`）。
  3. 更新构造函数与刷新逻辑。

### 任务 4.3：实现多盘自适应 `render_storage_summary`
- **文件**：`crates/rsetup-app/src/tui.rs`
- **步骤**：
  1. 编写测试：
     - `test_render_storage_summary_empty()`：无设备时展示单行占位符；
     - `test_render_storage_summary_single_nvme()`：仅 1 块 NVMe 时的展示；
     - `test_render_storage_summary_single_mmc()`：仅 1 块 eMMC 时的展示；
     - `test_render_storage_summary_multi_disk()`：同时有 2 块 NVMe 与 2 块 MMC 时的紧凑排版断言（验证每块设备节点名称、容量、状态均被正确渲染）。
  2. 将原 `render_nvme_summary` 重构升级为 `render_storage_summary`：
     - 支持统计 NVMe 与 MMC 设备总数；
     - 动态自适应行高计算，防止挤占其他窗口；
     - 采用 `[NVMe]`、`[eMMC]`、`[SD]` 前缀清晰区分磁盘类型。
  3. 运行测试：`cargo test -p rsetup-app tui` 确保所有渲染测试通过。

---

## 阶段 5：全量构建验证与回归检查

1. 运行工作区所有测试：
   `PATH="/usr/bin:$PATH" cargo test --all`
2. 运行 clippy 与代码格式检查：
   `cargo clippy --all-targets`

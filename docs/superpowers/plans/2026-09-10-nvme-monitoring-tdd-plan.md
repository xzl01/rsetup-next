# NVMe 磁盘状态监测功能 TDD 实施文档与测试计划

## 1. 概述

本计划遵循严格的 **Test-Driven Development (TDD)** 流程：**先写失败的测试（RED）-> 验证因缺少实现而正确失败 -> 编写最简实现代码（GREEN）-> 验证测试通过 -> 重构并保持绿色（REFACTOR）**。

实现目标：
1. ~~通过 `libnvme` 静态链接~~ 读取 NVMe 磁盘设备与 SMART / Health 健康遥测数据（实际经 Linux Admin Passthru `ioctl` 直读，无 `libnvme` 依赖——见阶段 3 偏离说明）。
2. 启动时自动探测，仅在检测到 NVMe 硬件存在时初始化该子模块，未检测到时保持未初始化/禁用状态。
3. 暴露于 `rsetup-core` 控制器、CLI、HTTP API 和 Web 界面。

---

## 2. 阶段化 TDD 实施任务表

| 阶段 | 关注点 | 目标源码文件 | 对应测试文件 / 测试函数 |
| --- | --- | --- | --- |
| **阶段 1** | 数据模型与 SMART 字节解析 | `crates/rsetup-core/src/model.rs`<br>`crates/rsetup-core/src/nvme.rs` | `tests/nvme_parsing_test.rs` 或 `nvme::tests::test_smart_log_parsing` |
| **阶段 2** | 启动侦测与条件初始化 | `crates/rsetup-core/src/nvme.rs` | `nvme::tests::test_nvme_probing_and_conditional_init` |
| **阶段 3** | ~~Libnvme 静态链接与~~ 原生采集后端（ioctl，实际实现无 `libnvme`，见阶段 3 偏离说明） | ~~`crates/rsetup-core/build.rs`~~<br>`crates/rsetup-core/src/nvme/sys.rs` | `nvme::tests::test_native_or_mock_nvme_read` |
| **阶段 4** | Controller 聚合与状态暴露 | `crates/rsetup-core/src/actions.rs`<br>`crates/rsetup-core/src/lib.rs` | `actions::tests::test_controller_nvme_status` |
| **阶段 5** | CLI 交互与呈现格式化 | `crates/rsetup-app/src/main.rs`<br>`crates/rsetup-app/src/i18n.rs` | `cli::tests` / 命令行集成验证 |
| **阶段 6** | HTTP API 路由与端点响应 | `crates/rsetup-app/src/server.rs` | `server::tests::test_api_hardware_nvme` |
| **阶段 7** | 前端界面渲染与演示模式 | `crates/rsetup-core/src/probe.rs`<br>`ui/app.js`<br>`ui/i18n.js` | `ui/*.test.mjs` |

---

## 3. 详细 TDD 步骤

### 阶段 1：数据模型与 NVMe SMART Log 字节解析

#### 1.1 RED - 编写测试用例
- **测试目标**：
  验证将标准的 512 字节 NVMe SMART / Health Log 内存数据正确解析为 `NvmeSmartLog` 结构体：
  - 字节 0: `critical_warning`（位掩码解析出具体告警列表）
  - 字节 1-2: `composite_temperature`（开尔文温度换算为摄氏度：`K - 273.15`）
  - 字节 3: `available_spare`（可用备用空间百分比）
  - 字节 4: `spare_threshold`（备用空间阈值）
  - 字节 5: `percent_used`（已用寿命百分比）
  - 字节 32-47: `data_units_read`（大数单位，1000 * 512 字节换算为字节总数）
  - 字节 48-63: `data_units_written`
  - 字节 64-79: `host_read_commands`
  - 字节 80-95: `host_write_commands`
  - 字节 128-143: `power_on_hours`
  - 字节 144-159: `unsafe_shutdowns`
  - 字节 160-175: `media_errors`
  - 字节 176-191: `num_err_log_entries`
- **预期失败**：编译错误（未定义 `NvmeSmartLog`、`NvmeDevice`、`NvmeStatus` 及 `parse_smart_log` 函数）。

#### 1.2 GREEN - 编写最简实现
- 在 `rsetup-core/src/model.rs` 中定义 `NvmeStatus`、`NvmeDevice`、`NvmeSmartLog`。
- 在 `rsetup-core/src/nvme.rs` 中实现 `pub fn parse_smart_log(buf: &[u8; 512]) -> Result<NvmeSmartLog, NvmeError>`。
- 运行测试并验证通过。

#### 1.3 REFACTOR - 重构
- 提取温度合法性检查、告警掩码常量定义。

---

### 阶段 2：启动侦测与条件初始化

#### 2.1 RED - 编写测试用例
- **测试目标 1（无 NVMe 设备）**：
  - 模拟系统路径（如 `/sys/class/nvme` 为空目录或不存在）。
  - 调用 `NvmeManager::probe_and_init(sysfs_root)`。
  - 断言返回 `NvmeStatus { initialized: false, devices: [], message: Some(...) }`。
- **测试目标 2（存在 NVMe 设备）**：
  - 构造包含虚拟 `nvme0` 的 sysfs 目录结构。
  - 调用 `NvmeManager::probe_and_init(sysfs_root)`。
  - 断言返回 `initialized: true`，且探测到对应设备列表。
- **预期失败**：`NvmeManager` 未实现探测函数，测试失败。

#### 2.2 GREEN - 编写最简实现
- 在 `nvme.rs` 中实现 `NvmeManager`：
  - 检查指定 root 下的 `sys/class/nvme` 是否有目录项。
  - 若无，返回未初始化状态；若有，构造已初始化状态。
- 运行测试并验证通过。

---

### 阶段 3：底层 Libnvme 静态构建与原生 FFI 绑定

> **实施偏离说明（2026-09-12 追记）**：本阶段原计划静态链接 `libnvme` 并封装其 FFI。实际实现**未使用 `libnvme`**，改为直接通过 Linux Admin Passthru `ioctl`（`NVME_IOCTL_ADMIN_CMD`，Get Log Page `0x02`/`LID 0x02`）读取 512 字节 SMART/Health 日志，静态属性取自 sysfs。
> 偏离原因：`libnvme` 静态链接需目标架构的 `libnvme.a` 及其传递依赖，而交叉编译环境（`/usr/lib/aarch64-linux-gnu`）无该库；且本功能只需一个日志页读取，`libnvme` 的拓扑扫描与命令封装属用不到的能力。
> 后续动作：`crates/rsetup-core/build.rs` 及其 `libnvme` 探测/链接逻辑已**整体移除**，仓库中不再存在对 `libnvme` 的任何引用。规格与验收标准已同步修订，见 `docs/superpowers/specs/2026-09-10-nvme-monitoring-design.md` 第 2.2 节与本文档第 4 节。
> 下方 3.1 / 3.2 原文保留，作为历史记录，**不再代表当前设计**。

#### 3.1 RED - 编写测试用例
- **测试目标**：
  - 测试底层设备拓扑发现与原生读取入口 `sys::read_nvme_device_status(ctrl_name)`。
  - 在非 NVMe 环境或非 Linux 平台优雅降级为错误返回，不触发 panic 或符号缺失崩溃。
- **预期失败**：缺少 FFI 符号或静态链接未配置。

#### 3.2 GREEN - 编写最简实现
- 在 `crates/rsetup-core/build.rs` 中配置 `libnvme` 静态链接探测：
  ```rust
  // 仅在 linux 目标且存在 libnvme 时配置静态链接
  println!("cargo:rustc-link-lib=static=nvme");
  ```
  同时引入容错降级分支，无静态库或缺少环境时支持 stub 模式。
- 在 `crates/rsetup-core/src/nvme/sys.rs` 封装调用 `nvme_scan_topology`、`nvme_ctrl_first_ns` 以及 ioctl / `nvme_cli_identify` / `nvme_get_log_smart` 等核心接口。
- 运行 `cargo test -p rsetup-core` 验证通过。

---

### 阶段 4：Controller 聚合与硬件 API 暴露

#### 4.1 RED - 编写测试用例
- **测试目标**：
  - 在 `Controller` 创建时 (`Controller::new(ProbeMode::Auto, ExecutionPolicy::DryRun)`)，自动完成 `NvmeManager` 探测。
  - 调用 `controller.nvme_status()`：
    - 在 Demo 模式下断言返回模拟的 NVMe 磁盘（`initialized: true`，包含示例设备及合理温度、寿命指标）。
    - 在真实环境中若无 NVMe 硬件，断言返回 `initialized: false`，不产生 panic。
- **预期失败**：`Controller` 上无 `nvme_status` 方法。

#### 4.2 GREEN - 编写最简实现
- 在 `crates/rsetup-core/src/actions.rs` 中将 `NvmeManager` 纳入 `Controller` 结构。
- 实现 `pub fn nvme_status(&self) -> Result<NvmeStatus, HardwareError>`。
- 完善 Demo 模式合成数据。

---

### 阶段 5：CLI 交互与呈现

#### 5.1 RED - 编写测试用例
- **测试目标**：
  - 测试 `rsetup-next hardware nvme --json` 输出符合 JSON Schema 的 `NvmeStatus`。
  - 测试未检测到 NVMe 时打印友好说明信息。
- **预期失败**：`HardwareCommands` 枚举无 `Nvme` 变体。

#### 5.2 GREEN - 编写最简实现
- 在 `crates/rsetup-app/src/main.rs` 的 `HardwareCommands` 中加入 `Nvme { #[arg(long)] json: bool }`。
- 在 `crates/rsetup-app/src/i18n.rs` 中增加中英文格式化渲染函数 `print_nvme_status`。
- 运行验证输出。

---

### 阶段 6：HTTP API 端点与路由测试

#### 6.1 RED - 编写测试用例
- **测试目标**：
  - 发送 `GET /api/v1/hardware/nvme` 请求，验证状态码为 200，响应体解析为 `NvmeStatus`。
- **预期失败**：404 Not Found。

#### 6.2 GREEN - 编写最简实现
- 在 `crates/rsetup-app/src/server.rs` 注册路由：`.route("/api/v1/hardware/nvme", get(nvme_status))`。
- 实现 handler 函数，调用 `controller.nvme_status()`。
- 运行并验证通过。

---

### 阶段 7：Web GUI 适配与验证

#### 7.1 测试目标
- 在 `ui/app.js` 增加硬件 NVMe 卡片渲染逻辑：
  - 若 `initialized === false`：显示“未检测到 NVMe 存储设备，模块未激活”。
  - 若 `initialized === true`：渲染设备型号、容量、健康状态徽章、温度数值、已使用寿命进度条与 SMART 错误统计。
- 在 `ui/i18n.js` 补充对应中英文字典。
- 执行 `npm test`（如有）及浏览器端断言脚本。

---

## 4. 验证与最终验收标准
1. `cargo test --workspace` 全量测试通过，无任何失败，新增测试用例覆盖全部核心逻辑。
2. 在无 NVMe 主机上运行：模块正确侦测并保持未初始化，CLI/Web 不报错，友好提示。
3. 在有 NVMe 或 Demo 模式下运行：正确读取展示基础信息及 SMART 健康指标。
4. **无 `libnvme` 依赖**（原标准为"静态链接验证：`ldd` 不强制动态依赖 `libnvme.so.1`"，2026-09-12 修订）：编译出的二进制文件不得动态或静态依赖 `libnvme`。实测以 `readelf -d` 检查，动态依赖仅 `libgcc_s.so.1`、`libm.so.6`、`libc.so.6`。

> 标准 4 的修订原因见阶段 3 偏离说明：实现采用 `ioctl` 直读，不存在 `libnvme` 引用，因此原"静态链接 libnvme"表述不再适用。修订后的标准反而更强——它要求**根本不存在**该依赖。

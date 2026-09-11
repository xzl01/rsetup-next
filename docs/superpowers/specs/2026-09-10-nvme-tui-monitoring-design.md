# TUI 终端界面 NVMe 监控与 SMART 遥测设计规范 (Spec)

## 1. 概述与设计目标

本规范定义在 `rsetup-next` 的 TUI（终端用户界面，基于 `ratatui` + `crossterm`）中集成 NVMe 磁盘状态监测与 SMART/Health 遥测功能。

### 核心设计原则
1. **条件性与非侵入式渲染**：
   - 遵循整机启动探测原则：当 `nvme_status.initialized == false` 时，终端界面不强制占用宝贵的屏幕行展示无意义的空白，而是在存储/遥测区域呈现紧凑占位状态或静默折叠；
   - 当 `nvme_status.initialized == true` 且存在 NVMe 设备时，在设备核心监控视窗（Device Core / Mission View）中渲染出 NVMe 磁盘信息（设备名称、型号、实时摄氏度温度、剩余寿命、可用备用空间、关键告警标志）。
2. **多语言适配 (i18n)**：
   - 严格跟随全局 `Locale`（中文 / 英文），所有指标标签（温度、健康、寿命、备用空间、读写量、告警等）均接入 `crates/rsetup-app/src/i18n.rs` 字典系统。
3. **安全与高性能**：
   - TUI 状态轮询 (`refresh`) 复用控制器 `controller.nvme_status()` 统一接口，不引入任何破坏性写入操作；
   - 在高刷新或按键交互循环中无额外阻塞，UI 渲染采用 `ratatui` 原生控件（Paragraph / Gauge / Line / Span / Block），保持流畅与风格一致。
4. **终端视口自适应**：
   - 在终端高度受限（高度 < 24 行）或宽度受限时，安全自适应排版，不产生 panic 或字符溢出断行畸变。

---

## 2. 界面布局架构与交互设计

### 2.1 布局调整 (`crates/rsetup-app/src/tui.rs`)
当前的 `render_mission` 垂直划分为 3 行：
- Row 0 (Length 6): CPU & Memory 仪表盘
- Row 1 (Length 6): 基础硬件设备信息 (Product / SoC / Uptime / Load / Temp)
- Row 2 (Min 5): 系统服务信号列表

#### 优化后布局规划：
将 `render_mission` 调整为自适应垂直 4 分段（或在 Row 1/2 动态重构）：
- **Row 0 (Length 6)**: CPU & 内存仪表盘 (`render_gauges`)
- **Row 1 (Length 6)**: 设备主板信息 (`render_device_core`)
- **Row 2 (Length 5-6，根据是否有 NVMe 动态分配)**: NVMe 存储监控卡片 (`render_nvme_summary`)
  - 若已初始化且存在设备：展示磁盘型号、容量、温度、可用备用空间、寿命已用百分比、健康状态。
  - 若未检测到 NVMe：展示一行简洁的“未检测到 NVMe 存储设备”（或在小终端尺寸下与主板状态紧凑合并）。
- **Row 3 (Min 4)**: 系统服务信号 (`render_services`)

### 2.2 详细视觉呈现格式

#### 初始化且检测到 NVMe 设备（以单盘为例）：
```text
┌ NVMe 存储遥测 ──────────────────────────────────────────────┐
│ nvme0 (/dev/nvme0n1) · ZHITAI TiPlus7100 1TB · 953.87 GiB   │
│ 状态: 正常  ·  温度: 44.9 °C  ·  健康度: 100% (寿命已用 0%)  │
│ 读写: 41.53 GiB / 135.20 GiB  ·  备用空间: 100% (阈值 1%)    │
└─────────────────────────────────────────────────────────────┘
```
当告警位不为 0 时：
- “状态: 告警”以珊瑚红（`CORAL`）醒目渲染，并列出主要警告标志。

#### 未检测到 NVMe 时：
```text
┌ NVMe 存储遥测 ──────────────────────────────────────────────┐
│ 未检测到 NVMe 存储设备，模块未激活                          │
└─────────────────────────────────────────────────────────────┘
```

---

## 3. 数据流与控制器接入

1. `App` 结构体中新增字段：
   ```rust
   nvme_status: rsetup_core::NvmeStatus,
   ```
2. 在 `App::new(controller, locale)` 中：
   ```rust
   let nvme_status = controller.nvme_status().unwrap_or_else(|_| rsetup_core::NvmeStatus {
       initialized: false,
       devices: vec![],
       message: Some("Failed to query NVMe status".into()),
   });
   ```
3. 在 `App::refresh(&mut self)` 刷新事件中重新采集 `self.nvme_status = self.controller.nvme_status()...`。
4. 格式化逻辑复用 `crates/rsetup-app/src/main.rs` 中的 `format_bytes` 格式化换算，确保容量与传输量换算标准全局一致。

---

## 4. 国际化字典新增规范 (`crates/rsetup-app/src/i18n.rs`)

在 `i18n.rs` 的 `Locale::text` 中增加对应键值：
- `"nvme_telemetry"` -> 中文: `"NVMe 存储遥测"`, 英文: `"NVMe Storage Telemetry"`
- `"nvme_healthy"` -> 中文: `"正常"`, 英文: `"Healthy"`
- `"nvme_warning"` -> 中文: `"告警"`, 英文: `"Warning"`
- `"nvme_endurance"` -> 中文: `"已用寿命"`, 英文: `"Used Endurance"`
- `"nvme_spare"` -> 中文: `"可用备用"`, 英文: `"Available Spare"`
- `"nvme_io"` -> 中文: `"累计读写"`, 英文: `"Data Read/Written"`
- `"nvme_not_detected"` -> 中文: `"未检测到 NVMe 存储设备"`, 英文: `"No NVMe storage devices detected"`

---

## 5. 测试与验证标准

1. **单元测试与端到端渲染测试**：
   - 编写针对 TUI 渲染缓冲区的测试用例：
     - 使用 `ratatui::backend::TestBackend` 初始化虚拟终端画布；
     - 测试在 Demo 模式下（包含模拟 NVMe 固态硬盘），调用渲染函数后缓冲内容中包含 `"Radxa M.2 NVMe SSD 512GB"`、温度 `"38.5 °C"` 及 `"NVMe"` 相关标签；
     - 测试在无 NVMe 设备（`initialized: false`）情况下，缓冲区包含 `"未检测到 NVMe 存储设备"` / `"No NVMe storage devices detected"`，不产生越界或 panic。
2. **国际化测试**：
   - 验证中文与英文模式下对应文本均完整映射，无模板未定义泄露。
3. **实机验证与视觉截图判定标准（Visual Screenshot Verification）**：
   - **自动化截屏工具链**：在实机通过无头/终端伪终端（PTY）捕获 TUI 首屏 ANSI 渲染序列，结合本地转换脚本生成真实的终端视觉截图 PNG 图片（存放于 `docs/testing/screenshots/tui-nvme-*.png`）。
   - **视觉断言与判定标准**：
     - **有 NVMe 机器 (`192.168.27.88`) 截图断言**：
       - 读取生成截图并通过视觉工具核验：NVMe 视窗完整展示设备名称 `nvme0`、真实型号 `ZHITAI TiPlus7100 1TB`、实时温度（约 44~46°C）、备用空间（100%）、已用寿命（0%）与读写统计；
       - 边框无字符断裂，各行列对齐，颜色层次分明（标签灰、数值亮、状态绿/正常）。
     - **无 NVMe 机器 (`192.168.27.34`) 截图断言**：
       - 截图核验：NVMe 视窗清晰显示“未检测到 NVMe 存储设备，模块未激活”（或英文版提示）；
       - 视窗自适应缩紧，不留无意义空白大黑块，且不破坏上方 CPU/内存仪表盘及下方服务列表的整体排版结构。


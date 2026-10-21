# MMC/eMMC 统一存储 (Storage) 呈现 实机核验报告

日期：2026-09-12。对象：`rsetup-next` 的 MMC/eMMC 内核采集与统一「存储 (Storage)」呈现（核心库 sysfs 采集、CLI、TUI、Web 控制中心）。范围：只读观测，未执行任何硬件或系统写入。核验基线：`dev-aghost` 上的统一存储 Web 集成（恢复提交 `6465543` / `54a755c`，随后一次仅涉及测试与文案的收紧提交，见第 11 节）。

判定依据：TUI 部分对照 `docs/superpowers/specs/2026-09-12-mmc-storage-tui-design.md`；Web 部分对照本分支实现的统一存储呈现行为（REST 聚合端点 + 硬件矩阵 `storage` 工具 + MMC/eMMC 卡片）与本次重实现计划 `docs/superpowers/plans/2026-09-12-mmc-storage-web-reimplementation-plan.md`。

## 1. 结论摘要

- **MMC/eMMC 数据链路符合预期**：TUI 与 Web 在同一台设备上呈现的型号、厂商、容量、序列号、固件、SLC/MLC 寿命、pre-EOL 状态，与 sysfs、CLI（`hardware mmc/storage --json`）三方一致；`lifeTimeEstBPercent: null`（Rock 5B 的 `life_time = 0x01 0x00`）在 TUI 与 Web 均显示「不支持」且进度条置灰。
- **Web 传输层与呈现符合预期**：`GET /api/v1/hardware/storage` 在两类实机（有/无 NVMe）均返回聚合结构与正确 capability detail；**`GET /api/v1/hardware/nvme` 按设计移除**，实机返回 404；硬件矩阵 `storage` 卡片、面板头计数徽章、NVMe 与 MMC 卡片布局、移动端单列均符合预期。
- **上一轮核验发现的两项 Web 缺陷已修复并锁定**：
  - **W1**（抽屉标题/副标题文案错误）：实机抽屉标题现为「存储」（EN `Storage`），副标题为「监测 NVMe 与 MMC/eMMC 存储健康状态。」（EN `Monitor NVMe and MMC/eMMC storage health.`）。
  - **W2**（矩阵卡片标签未本地化）：中文界面下卡片标题现为「存储」，与同屏其他卡片一致（EN 仍为 `Storage`）。
- **发现 1 项 TUI 外观问题**（无数据丢失，仍未处理）：**T1** 在 Rock 3A 上 eMMC 设备行超出卡片内宽（61 > 59 列）折行，容量被拆成「58.24」/「GiB」两行。
- 另有若干「规范示意 vs 实现」的等价差异（第 6 节）与代码质量待办（第 10 节第 2 条，来自本次任务审查的次要发现）。
- 未覆盖面（第 7 节）：SD 卡分支、多 MMC 设备、warning/critical 健康态、空态在实机上未触发，仅由单元/渲染测试覆盖。

## 2. 测试基线

### 2.1 测试设备（均为实验室设备，SSH + PTY / SSH 隧道驱动）

| 设备 | 主机 | 型号 / SoC | 内核 | 存储硬件 |
| --- | --- | --- | --- | --- |
| Rock 5B | `192.168.27.88` | Radxa ROCK 5B / RK3588 · aarch64 | `6.1.115-vendor-rk35xx` | NVMe `nvme0`（1TB）+ eMMC `mmc0:0001` |
| Rock 3A | `192.168.27.34` | Radxa ROCK 3A / RK3568 · aarch64 | `6.18.44-current-rockchip64` | 仅 eMMC `mmc0:0001`（`/sys/class/nvme` 为空） |

本机（x86_64 Ubuntu，内核 7.0.0-31-generic）无任何 MMC 设备，故 MMC 证据全部来自上述两台 SBC。

### 2.2 sysfs 原始值（判定基线）

| 字段 | Rock 5B `mmc0:0001` | Rock 3A `mmc0:0001` |
| --- | --- | --- |
| `type` | `MMC` | `MMC` |
| `name` | `A3A562` | `064G02` |
| `manfid` | `0x0000d6`（无名称映射） | `0x000011` → Toshiba |
| `serial` | `[已脱敏]` | `[已脱敏]` |
| `fwrev` | `0x1100000000000000` | `0x0200000000000000` |
| `date` | `05/2022` | `12/2019` |
| `/sys/class/block/mmcblk0/size` | `241664000` 扇区 → 115.23 GiB | `122142720` 扇区 → 58.24 GiB |
| `life_time` | `0x01 0x00` | `0x01 0x01` |
| `pre_eol_info` | `0x01`（正常） | `0x01`（正常） |
| 子分区 | `mmcblk0boot0/boot1/rpmb` 均未被当作独立驱动器 | 同左 |

`life_time` 第一字节 `0x01` 在两台机器上都被解析为 10%（JEDEC 0x01 = 0–10% 耗损区间），第二字节 `0x00`（未定义）在 Rock 5B 上被正确解析为 `null` → TUI/Web 均显示「不支持」，这是本轮最有价值的一致性与空值用例。

### 2.3 构建、部署与采集方式

- 构建：本机 x86_64 交叉编译 `aarch64-unknown-linux-gnu` release（`aarch64-linux-gnu-gcc` 链接）；二进制 SHA-256 `1c57f5991733b01c9abee6e327229f1f216f590126746acc38c4dc3f3859fd51`，经 `scp` 部署到两台机器的 `/tmp/rsetup-next-test`（部署后逐机校验哈希一致）。
- TUI 采集：`scripts/capture-tui-remote.py <host> <raw> zh_CN.UTF-8 100 30` 采集原始 ANSI 流，再由 `scripts/capture-tui-screenshot.py <raw> <png> 100 30` 离屏渲染为 PNG（CJK 字体、东亚宽字符占两格）。
- Web 采集：两台机器分别 `rsetup-next serve --listen 127.0.0.1:19088|19089`（仅监听回环，未开放局域网），经 `ssh -L` 隧道映射到本机；`scripts/capture-web-screenshot.mjs --tool storage`，headless Chrome 152、light 主题、desktop 1440×900 / mobile 390×844 @2x / 矩阵无点击视图、`--settle 1800`。英文视图由 `--eval` 调用页面自身的 `window.RsetupI18n.setLocale("en")` 切换后同流程采集。
- 采集工具链：Node v22.22.1、Python 3 + Pillow 12.1.1。

## 3. 三方一致性（核心判定表）

| 字段 | sysfs | CLI `hardware storage --json` | TUI 截图 | Web 截图 |
| --- | --- | --- | --- | --- |
| 88 型号 | `A3A562` | `A3A562` | `A3A562` | `A3A562` |
| 88 厂商 | `0x0000d6` | `0x0000d6` | `0x0000d6` | `0x0000d6` |
| 88 容量 | 115.23 GiB | `totalBytes` 123 731 968 000 → 115.23 GiB | `115.23 GiB` | `115.2 GiB` |
| 88 SLC/MLC | `0x01` / `0x00` | `10` / `null` | `SLC 寿命: 10% · MLC 寿命: 不支持` | `SLC 寿命 10%` / `MLC 寿命 不支持`（灰条 `is-na`） |
| 88 pre-EOL | `0x01` | `preEolInfo: 1` | `预警: 正常` | `预 EOL 状态 正常` |
| 34 型号 / 厂商 | `064G02` / `0x000011` | `064G02` / `Toshiba (0x000011)` | `064G02 · Toshiba (0x000011)` | `064G02` / `Toshiba (0x000011)` |
| 34 容量 | 58.24 GiB | `totalBytes` 62 537 072 640 → 58.24 GiB | `58.24 GiB`（折行，见 T1） | `58.2 GiB` |
| 34 SLC/MLC | `0x01` / `0x01` | `10` / `10` | `SLC 寿命: 10% · MLC 寿命: 10%` | 两条 10% 绿条 |
| 34 NVMe | 无 | `initialized: false` + `message` | 不渲染 NVMe 行 | 不渲染 NVMe 区、无空提示 |

实机 REST 复核（两台机器）：`GET /api/v1/hardware/storage` → **200**（88：1 NVMe + 1 MMC；34：1 MMC + `nvme.initialized:false`）、`GET /api/v1/hardware/nvme` → **404**；`/api/v1/snapshot` 的 capability 为 `("storage", available:true, "1 NVMe · 1 eMMC")` / `("storage", available:true, "1 eMMC")`。

## 4. 符合预期项

### 4.1 TUI（对照 TUI 规范 §5）

| 规范条目 | 实测 | 判定 |
| --- | --- | --- |
| §5.1 面板重命名为「存储状态」(`storage_telemetry`) | Rock 5B / 3A 均显示「存储状态」 | 符合 |
| §5.1 汇聚全部 NVMe 与 MMC/SD 设备，NVMe 在前 | 88：`nvme0` 三行 → 空行 → `[eMMC]` 两行；34：仅 `[eMMC]` 两行 | 符合 |
| §5.1 自适应高度、无溢出与截断 | 卡片高度按折行后行数计算；88 的 NVMe 备用值/阈值、34 的 eMMC 状态行均完整可见 | 符合 |
| §5.2 eMMC 行 1：类型 + 块设备 + 型号 + 厂商 + 容量 | `[eMMC] /dev/mmcblk0 · A3A562 · 0x0000d6 · 115.23 GiB` | 符合（字形与规范示意略有差异，见第 6 节） |
| §5.2 eMMC 行 2：状态 + SLC/MLC 寿命 + pre-EOL | `状态: 正常 · SLC 寿命: 10% · MLC 寿命: 不支持 · 预警: 正常` | 符合 |
| 未支持/未定义指标降级 | 88 的 MLC 显示 `不支持`（源自 `0x00`），未误报为 0% | 符合 |
| 中英多语言 | 本轮 TUI 为 zh_CN 实机；en 由 `cargo test` 渲染用例覆盖 | 部分覆盖（见第 7 节） |

### 4.2 Web 控制中心（统一存储呈现）

| 核验项 | 实测 | 判定 |
| --- | --- | --- |
| REST `GET /api/v1/hardware/storage` 返回聚合结构 | 88：200，`nvme.devices` 1 项 + `mmc.devices` 1 项；34：200，`mmc.devices` 1 项 + `nvme.initialized: false` | 符合 |
| `/api/v1/hardware/nvme` 端点按设计移除 | 两台均 404 | 符合 |
| camelCase 键名与空态语义 | `blockPath`/`cardType`/`totalBytes`/`health.preEolInfo` 等键名正确；34 的 NVMe 子对象为 `initialized: false` + 未检测文案（200 而非错误） | 符合 |
| capability id 为 `storage`，availability = 任一类非空 | 硬件矩阵存在 `storage` 卡片；detail 88 = `1 NVMe · 1 eMMC`，34 = `1 eMMC` | 符合 |
| 抽屉标题与描述 | 标题「存储」/`Storage`，描述「监测 NVMe 与 MMC/eMMC 存储健康状态。」/`Monitor NVMe and MMC/eMMC storage health.`（**W1 修复后**） | 符合 |
| 矩阵卡片 label 随界面语言本地化 | 中文「存储」（**W2 修复后**）；英文 `Storage` | 符合 |
| 矩阵卡片图标 | 显示磁盘堆叠图标（`icon-storage`，与 `icon-nvme` 同风格） | 符合 |
| 面板头 = 描述文案 + 计数徽章 | `监测 NVMe 与 MMC/eMMC 存储健康状态。` + `1 NVMe · 1 MMC/eMMC`（88）/ `1 MMC/eMMC`（34） | 符合 |
| 仅一侧有设备时只渲染该侧 | 34 无 NVMe 区、无 NVMe 空提示；88 两类都在 | 符合 |
| NVMe 区在上、MMC 区在下 | 88 桌面截图：NVMe 卡片 → MMC 卡片 | 符合 |
| MMC 卡片标题=model、副标题=blockPath | `A3A562` / `064G02` + `/dev/mmcblk0` | 符合 |
| 卡类型徽章 `MMC`→eMMC、`SD`→SD | 两枚徽章均显示 `eMMC` | 符合（SD 分支未在实机触发） |
| 健康徽章三态优先级 | 两台 `preEolInfo: 1`、`warningFlags: []` → 健康徽章「健康」/`Healthy` | 符合（warning/critical 未在实机触发） |
| 规格网格 4 项（容量/序列号/固件/厂商） | 桌面 2×2、移动单列，值与 CLI 一致 | 符合 |
| SLC/MLC 寿命进度条，`null` → 置灰 + N/A 文案 | DOM 实测：`is-normal` + `--metric-percent: 10%`（SLC）、`is-na` + `0%`（MLC，88）、两条 `is-normal` 10%（34） | 符合 |
| pre-EOL 行四态文案 | 两台均 `正常` | 符合 |
| 移动端布局 | 390×844 @2x 下卡片单列铺满，无横向溢出 | 符合 |
| 前端无运行时错误 | 全部 10 次采集 `consoleErrors: []` | 符合 |

## 5. 发现的问题

### W1（缺陷，Web）Storage 抽屉标题与副标题文案错误 — **已修复**

- **原现象**：抽屉标题为「存储空间」、副标题为「暂无详细信息」（EN `Storage map` / `Details unavailable`），而面板内部同时显示完整的 NVMe 与 MMC 遥测。
- **根因**：`ui/app.js` 的 `hardwareToolCopy()` prefix map 把 `storage` 指向既有的 `storage.*` 键族（系统页根存储面板），而 `storage.description` 键不存在，`i18n.t()` 回退到缺失键哨兵 `text.unavailable`。
- **修复**：`storage` 改走 `storageTool.*` 命名空间（标题「存储」/`Storage`，描述取 `storageTool.description`）——一处 token 改动。
- **证据**：① 测试 `ui/storage.test.mjs` 的 `storage drawer copy resolves through the storageTool namespace`（提取真实 `hardwareToolCopy` + 真实字典，断言两种语言的解析结果）；把该 token 还原为 `storage` 后该用例 **FAIL**（控制器在一次性 worktree 中独立复现：1/6 失败），修复后全绿。② 实机 DOM 复核（两台机器）：`drawerTitle` = 「存储」/`Storage`、`drawerDescription` = 「监测 NVMe 与 MMC/eMMC 存储健康状态。」/`Monitor NVMe and MMC/eMMC storage health.`。③ 截图：`web-rock5b-storage-desktop.png`、`web-rock5b-storage-desktop-en.png`、`web-rock3a-storage-desktop.png`。

### W2（缺陷，Web）中文界面下矩阵卡片标签未本地化 — **已修复**

- **原现象**：硬件矩阵中该卡片标题为英文 `Storage`，同屏其余卡片均为中文。
- **根因**：`ui/i18n.js` 的 `capabilityCopy` 仍以旧 capability id `nvme` 为键，改名后无 `storage` 条目 → 直接回退到后端英文 label。
- **修复**：`capabilityCopy` 键改为 `storage`（中文 label「存储」），并补 `hardwareReason` 的 `"No storage devices detected" → "未检测到存储设备"` 映射；同时删除改名后不可达的 `value.id === "nvme"` 明细分支与 `"NVMe controllers detected"` 残留字符串。
- **证据**：① 测试 `ui/i18n.test.mjs` 的 `storage capability card is localized under its renamed id`；把键还原为 `nvme` 后该用例 **FAIL**（控制器独立复现：1/16），修复后全绿。② 实机 DOM 复核：中文界面矩阵卡片为「存储」（88 `1 NVMe · 1 eMMC`、34 `1 eMMC`），英文界面为 `Storage`。③ 截图：`web-rock5b-storage-matrix.png`、`web-rock3a-storage-matrix.png`。

### T1（外观问题，TUI）窄屏下 eMMC 设备行折行，容量数值跨行 — **已修复（代码与用例；实机复检待做）**

- **现象**：Rock 3A（100×30 终端、卡片内宽 59 列）上该行渲染为两行：`[eMMC] /dev/mmcblk0 · 064G02 · Toshiba (0x000011) · 58.24` / `GiB`。
- **实测**：该行显示宽度 61 列 > 59 列内宽。Rock 5B（厂商为未映射的 `0x0000d6`，行宽更短）不折行，故问题只在厂商名带名称映射时出现。
- **影响**：**无数据丢失**——卡片高度按折行行数计算（`crates/rsetup-app/src/tui.rs` 的 `wrapped_rows`），未发生截断；但「一台设备两行」的排版被破坏，容量数值被拆到第二行，可读性下降。
- **一致性缺口**：NVMe 行有宽度自适应降级（`已用寿命:` 超宽时改成短标签），MMC 行没有等价的降级路径。

**修复（提交 `26fab4b`）**——MMC 行补上与 NVMe 行同构的宽度降级路径：

- **设备行**（`crates/rsetup-app/src/tui.rs` 的 `mmc_identity_line`）按 `/dev/` 前缀 → 厂商 → 型号 的顺位丢弃字段，仍放不下时以省略号截断标识部分；类型徽章与容量恒保留在同一行。Rock 3A 的行在 59 列内宽下降级为 `[eMMC] mmcblk0 · 064G02 · Toshiba (0x000011) · 58.24 GiB`（55 列），容量与单位不再拆分。
- **状态行**同批处理：中文全写（`状态: 正常 · SLC 寿命: 10% · MLC 寿命: 10% · 预警: 正常`，55 列）可容纳，英文全写需 65 列，故先改用短寿命标签（新增 i18n 键 `storage_life_a_short`/`storage_life_b_short`），再舍弃取值为「正常/未定义」的 pre-EOL 段；pre-EOL 处于告警或紧急时不舍弃。
- 附带修正：sysfs 读取失败的设备其 `model`/`manufacturer` 为空，原实现会打印悬空的 ` · ` 分隔符，现按非空字段拼接。

**证据**：`crates/rsetup-app/src/tui.rs` 新增 5 个渲染/宽度用例——`test_render_storage_summary_keeps_mmc_capacity_on_one_row`（61 列下容量与徽章同行、`/dev/` 已舍弃，中英双语）、`test_render_storage_summary_mmc_identity_degrades_by_width`（100 列保留全量、59 列保留厂商）、`test_render_storage_summary_mmc_telemetry_line_fits_narrow_card`（中文全写 / 英文短标签）、`test_mmc_identity_line_fits_every_inner_width`（16~61 列逐列不变量，更窄处容量不丢）、`test_mmc_identity_line_skips_empty_identity_fields`。`cargo test -p rsetup-next` 92 项全绿。**尚未在 Rock 3A 实机复检**（需重新交叉编译部署并复采 `tui-rock3a-storage-zh.png`）。

## 6. 规范 vs 实现的等价差异（建议回写规范，非缺陷）

1. **TUI 高度策略**：规范 §5.1 要求 N=1 时 5~6 行完整展开、N≥2 时每盘 2 行紧凑排版且状态行缩进；实现统一为 NVMe 每盘 3 行、MMC 每盘 2 行、状态行不缩进。信息量与可读性等价。
2. **eMMC 行字形**：规范 §5.2 示意 `[eMMC] mmcblk0 (FE4MB4) · Samsung · 58.2 GiB`；实现为 `[eMMC] /dev/mmcblk0 · A3A562 · 0x0000d6 · 115.23 GiB`（带 `/dev/` 前缀、`·` 分隔、厂商输出原始 hex）。信息等价。
3. **寿命文案**：规范 §5.1 示例为 `寿命耗损: SLC <10% / MLC <10%`；实现为 `SLC 寿命: 10%`，即以 JEDEC 区间上界呈现 `0x01`。CLI、TUI 与 Web 端采用同一口径（Web 的 `lifeTimeEstAPercent: 10`）。
4. **容量精度**：TUI/CLI 用二进制单位（115.23 / 58.24 GiB），Web 卡片用一位小数（115.2 / 58.2 GiB）。同一数值的不同精度呈现，非不一致。

## 7. 未覆盖范围与局限

- **无 SD 卡**：两台实机各只有一个 `mmc0`（`type = MMC`），`cardType === "SD"` 的徽章分支与 SD 特有线未在实机验证，仅由单元/渲染用例覆盖。
- **无多 MMC / 多盘密集场景**：无法验证 N≥2 时的紧凑排版与 `... (+M more devices)` 截断提示（代码路径存在但实机未触发）。
- **无告警健康态**：两台设备 `pre_eol_info = 0x01`、`warningFlags` 为空，只验证了健康徽章与绿色进度条；warning/critical 配色未在实机出现。
- **无空态**：本机 x86_64 无 MMC/NVMe 设备，两台 SBC 都有设备，「未检测到存储设备」空态未在实机触发（仅有单元/渲染用例覆盖）。
- **TUI 英文实机截图未采**：本轮 TUI 截图为 zh_CN 实机；TUI 英文渲染由 `cargo test` 覆盖。Web 侧中英两种界面均已实机截图（见第 8 节）。
- **采集通道**：TUI 经 SSH PTY 采集原始 ANSI 流后离屏渲染为 PNG（非物理控制台）；Web 为 headless Chrome 152 截图。温度、读写计数等实时量随运行时间自然漂移，不要求跨次采集严格相等。

## 8. 证据清单

目录：`docs/testing/mmc-2026-09-12/screenshots/`

| 文件 | 采集配置 | 打码处 | SHA-256（前 16 位） |
| --- | --- | --- | --- |
| `tui-rock5b-storage-zh.png` | TUI 100×30，zh_CN，Rock 5B（NVMe + eMMC） | —（TUI 不渲染序列号） | `bbe930f549e310dd` |
| `tui-rock3a-storage-zh.png` | TUI 100×30，zh_CN，Rock 3A（仅 eMMC） | —（同上） | `89d7cbc244a8762d` |
| `web-rock5b-storage-desktop.png` | 1440×900，zh，drawer 顶部（面板头 + NVMe 卡片 + MMC 卡片起始） | NVMe + eMMC 序列号 | `f1177360fa228a24` |
| `web-rock5b-storage-desktop-mmc.png` | 1440×900，zh，drawer 滚到底（NVMe 尾部 + eMMC 整卡） | NVMe + eMMC 序列号 | `ea028ee6ac5deeaf` |
| `web-rock5b-storage-desktop-en.png` | 1440×900，en，drawer 顶部（W1 英文文案证据） | NVMe + eMMC 序列号 | `a19a04ac07319644` |
| `web-rock5b-storage-mobile-top.png` | 390×844 @2x，zh，drawer 顶部（面板头徽章 `1 NVMe · 1 MMC/eMMC`） | NVMe 序列号（eMMC 值在折叠线以下） | `2999a9de29af58e1` |
| `web-rock5b-storage-mobile-mmc.png` | 390×844 @2x，zh，drawer 滚到底（eMMC 单列卡） | eMMC 序列号（NVMe 值已滚出视口） | `4d9fb74fd14dd824` |
| `web-rock5b-storage-matrix.png` | 1440×900，zh，硬件矩阵（`storage` 卡片 + `1 NVMe · 1 eMMC`） | —（矩阵不渲染序列号） | `aeb32f7779379cb6` |
| `web-rock3a-storage-desktop.png` | 1440×900，zh，仅 eMMC 面板 | eMMC 序列号 | `19f4080ee8f7541c` |
| `web-rock3a-storage-mobile.png` | 390×844 @2x，zh，仅 eMMC 面板 | eMMC 序列号 | `33f3f53b984eb6e2` |
| `web-rock3a-storage-matrix.png` | 1440×900，zh，硬件矩阵（`storage` 卡片 + `1 eMMC`） | —（同上） | `2d58a0c92864d6ed` |

### 打码说明

**策略：所有设备序列号（NVMe 磁盘与 MMC/eMMC 芯片）一律打码。**

- 方法：采集时用 CDP 实测序列号元素（`… .nvme-spec-item:nth-child(2) b`）的 `getBoundingClientRect`，再对 PNG 做**像素级实心打码**（取色与既有 `scripts/redact-screenshots.py` 一致，为标签文字同色 `rgb(41,50,70)`）。除掩码区域外，其余像素与"已做过视觉核验的那张图"逐字节一致。
- 覆盖面：11 张截图中含序列号的 7 张共 **10 处**已打码（Rock 5B：desktop 与 desktop-en 各 2 处、desktop-mmc 2 处、mobile-top 1 处、mobile-mmc 1 处；Rock 3A：desktop 1 处、mobile 1 处）。两处值因滚出视口而不可见（`web-rock5b-storage-mobile-mmc.png` 的 NVMe 序列号实测 y = −463；`web-rock5b-storage-mobile-top.png` 的 eMMC 序列号 y = 1284 > 视口高 844）。TUI 截图与矩阵截图不含任何序列号（TUI 的 eMMC 行只渲染块设备、型号、厂商、容量）。
- 校验：① 每个掩码区域应用后逐像素确认纯色实心（`solid=True`）；② 打码只覆盖"值"元素，`序列号` 标签仍可读（字段存在性不受影响）；③ 对全部 11 张图做全图 OCR（`chi_sim+eng` / `eng`）搜索两台机器的序列号及其近似形态，**无一命中**；④ 本报告不留存序列号明文（第 2.2 节以 `[已脱敏]` 占位）。

## 9. 复现步骤

```bash
# 1) 交叉编译并部署
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
  cargo build --release --locked --target aarch64-unknown-linux-gnu -p rsetup-next
scp target/aarch64-unknown-linux-gnu/release/rsetup-next root@<host>:/tmp/rsetup-next-test

# 2) TUI 截图（有 NVMe 与仅 MMC 的两台机器各一次）
python3 scripts/capture-tui-remote.py <host> raw.ansi zh_CN.UTF-8 100 30 /tmp/rsetup-next-test
python3 scripts/capture-tui-screenshot.py raw.ansi tui-storage-zh.png 100 30

# 3) Web 截图（服务仅监听回环，经 SSH 隧道）
ssh -f -N -L 19088:127.0.0.1:19088 root@<host>   # 远端先执行 serve --listen 127.0.0.1:19088
node scripts/capture-web-screenshot.mjs --url "http://127.0.0.1:19088/#hardware" \
  --out web-storage-desktop.png --width 1440 --height 900 --tool storage --settle 1800 \
  --eval '(() => { const box=(s)=>{const e=document.querySelector(s); if(!e) return null;
            const r=e.getBoundingClientRect(); return {x:Math.round(r.x),y:Math.round(r.y),
            w:Math.round(r.width),h:Math.round(r.height)};};
            return {nvme: box("[data-hardware-body] .nvme-card .nvme-spec-item:nth-child(2) b"),
                    mmc:  box("[data-hardware-body] .storage-mmc-card .nvme-spec-item:nth-child(2) b")}; })()'

# 4) 英文视图：在同一命令的 --eval 里先切换语言
#    window.RsetupI18n.setLocale("en");  （再返回上面的矩形，供打码使用）
```

## 10. 建议后续动作

1. **修 T1**（当前唯一实机可见待办）：为 MMC 设备行补一条宽度降级路径（例如超宽时省略 `/dev/` 前缀或厂商名，优先保住容量数值），并补一个 61 列边界的渲染用例；或按 TUI 规范改为两行专用排版。——**已按第一种方案实现（提交 `26fab4b`），见第 5 节 T1 的修复与证据；实机复检待做**。
2. **代码质量待办（本次任务审查记录的次要发现，不影响行为）**：
   - `ui/styles.css` 的 `.storage-mmc-card` 与既有 `.nvme-card` 规则逐条重复 → 可改为 `class="nvme-card storage-mmc-card"`（需同步收紧 `ui/storage.test.mjs` 中 `!includes('class="nvme-card"')` 断言）；**已处理（提交 `1bfa6ce`）**：删除重复盒模型，卡片同时带 `.nvme-card`（盒模型）与 `.storage-mmc-card`（定位钩子，第 9 节的取值选择器不变），断言改为校验两张 MMC 卡都带共享盒模型与自身钩子；
   - `storageTool.model`、`nvme.description`/`uninitialized`/`noDevices` 已无引用，`ui/index.html` 的 `icon-nvme` 成为孤立 symbol → 可清理；**部分处理（提交 `1bfa6ce`）**：`icon-nvme` 已删除；`storageTool.model` 与 `nvme.description`/`uninitialized`/`noDevices` 四个 i18n 键仍未被引用（`ui/i18n.test.mjs` 的字典用例仍断言其取值），留待与 i18n 字典一并清理；
   - `crates/rsetup-core/src/probe.rs` 在探测 MMC 计数时以硬编码绝对路径重读 `type`，而 `MmcManager::probe_sysfs` 已按注入的 root 读取同一文件；**已消除（随 `b76df32` 接入 deviceinfo）**：该分支连同 `/sys/bus/mmc/devices` 一并移除，三类计数现均来自 `deviceinfo::inspect_storage(&self.root)`，其 `device/type` 读取本就相对注入根；提交 `1178221` 补 `mmc_counts_come_from_the_injected_sysfs_root` 用例钉住。附注：`probe.rs` 的 `spi_nor_detected()`、`probe_interfaces()` 与 GPIO/thermal/LED 的 `Path::new("/sys/...").exists()` 仍走全局绝对路径（与 MMC 无关，本项未覆盖）；
   - `ui/storage.test.mjs` 中 NVMe 卡片渲染现仅经由 stub 覆盖，保留的 NVMe 卡片断言较此前减少。
   - **已处理（提交 `1bfa6ce`，选择「给一个可见宽度」）**：`.nvme-metric-bar-fill.is-na` 加 `width: 100%`，并按第 4.2 节「null → 置灰」的设计意图去掉 N/A 填充上误导性的 `data-metric-percent`（N/A 不是百分比）；`ui/storage.test.mjs` 新增用例锁定「斜纹规则带宽度、N/A 填充不带百分比、`.storage-mmc-card` 不再出现第二条盒模型规则」。原缺陷描述：`ui/styles.css` 的 `.nvme-metric-bar-fill.is-na` 目前是死 CSS：基础规则的 `width: var(--metric-percent, 0%)` 与 NA 分支的 `data-metric-percent="0"`（`ui/app.js`）叠加后，填充在 `overflow: hidden` 的 `.nvme-metric-bar` 轨道内宽度恒为 0%，因此「N/A」寿命条渲染为空轨道而非灰色斜纹填充；需决定给它一个可见宽度，或删除该规则、该 class 及 `ui/storage.test.mjs` 中对应的计数断言（`nvme-metric-bar-fill is-na` 计数为 2）。
   - 历史文档 `docs/testing/nvme-2026-09-12/report.md` 与 `docs/superpowers/specs/2026-09-10-nvme-monitoring-design.md` 仍将 `GET /api/v1/hardware/nvme` 描述为在线端点；它们现已早于该端点被 `/api/v1/hardware/storage` 取代这一变更。
3. 规范回写：第 6 节的三项等价差异建议直接在 TUI 规范 §5.1/§5.2 中改成实现现状，避免后续核验再次产生假阳性。
4. 保留（非序列号、且是核验所需的设备上下文）：第 2.2 节的型号 `A3A562` / `064G02`、厂商 `0x0000d6` / `0x000011`、固件与容量。如需连型号/固件一并脱敏，可再处理。

## 11. 核验记录

- **代码基线**：恢复提交 `6465543`（后端 storage capability + REST 端点）与 `54a755c`（前端 storage 工具 + MMC 卡片 + W1/W2 修复）。11 张截图是针对由 commit `54a755c` 构建的二进制（SHA-256 `1c57f5991733b01c9abee6e327229f1f216f590126746acc38c4dc3f3859fd51`）采集的；其后的修复提交 `38751fe` 仅收紧一处测试接缝（test seam）与一处不可达的中文回退文案、不改变任何渲染结果，该提交重新构建（SHA-256 `262775fad71c55cb…`）、重新部署到两台机器并实机复检，渲染输出完全一致，故这 11 张截图对最终代码仍然有效。
- **自动化**：`cargo test --workspace --locked` 全绿（core 122 passed（124 run，其中 2 ignored）/ app 39 / helper 1）、`node --test ui/*.test.mjs` 全绿（含 `ui/storage.test.mjs` 6 项与 `ui/i18n.test.mjs` 16 项）、`cargo clippy --workspace --all-targets -- -D warnings` 无告警。`cargo fmt --all -- --check` 在本仓库基线上即为红（未改动树实测 61 处 `Diff in`，全在未触碰文件；本次改动的两个 Rust 文件为 0 处），属既有 rustfmt 版本漂移，未纳入本次范围；后续以一次纯机械的 `style:` 提交（`cargo fmt --all`，经 token 级比对确认无语义变化——仅含空白/换行重排、`reorder_imports`/`reorder_modules` 造成的顺序调整，以及 Rust 可忽略的尾随逗号增删和单表达式 match arm 去块，无任何标识符或表达式改动）使该门禁恢复为绿（`cargo fmt --all -- --check` 退出码 0、无输出），并注明该漂移早于本次工作即已存在（`7618471` 处实测 34 处），其余大部分由 MMC 存储 TUI 实现链引入（该链结束时 62 处 / 10 文件）。
- **W1/W2 的失败态已独立复现**：在一次性 worktree 中分别把两处修复还原后，对应用例失败（`storage drawer copy …` 1/6 失败、`storage capability card …` 1/16 失败），确认两个用例真正锁定了缺陷，而非事后补写。
- **实机证据**：两机 REST（200/404 + capability detail）、W1/W2 文案（中英）、MMC 卡片字段、`consoleErrors: []`、11 张已打码截图，均见第 3、4、8 节。

## 12. 第 5 节 T1 修复（`26fab4b`）的实现级复审与修复（2026-10-21 补记）

本次补记只覆盖 **T1 修复自身引入的 P2 回归**与同一区域的 **P3 健壮性项**；证据全部来自代码与 `TestBackend` 的最终 `Buffer`。

- **P2（回归）**：`26fab4b` 新增的 `wrapped_rows` 以 `ceil(行宽 / 卡片内宽)` 估算行数，而 ratatui 的 `Paragraph` + `Wrap { trim: true }` 按**词**断行（词不可分、断点空白丢弃），估算与真实行数在两个方向都可能不符。低估时卡片比内容矮一行，第 5 节想保住的容量/告警仍被裁——评审给出的 Rock 3A eMMC fixture（内宽 31、`pre_eol_info=2`、健康 `Warning`、SLC/MLC 10%）在 33 列视口下实际渲染为

  ```
  │[eMMC] mmcblk0 · 58.24 GiB     │
  │Health: Warning · SLC: 10% ·   │
  │MLC: 10% · Pre-EOL:            │      ← Warning(80%) 整段丢失
  ```

  扫描 16..61 列 × 中英 × 预警 2/3 × 寿命缺失/10/100/101 共 19 种组合复现。**未固定增高、未删告警**：`wrapped_rows` 改为渲染同一个 `Paragraph` 到 scratch `Buffer` 后数非空行（`crates/rsetup-app/src/tui.rs`），`render_mission` 与 `render_storage_summary` 因此共用同一个度量；高度受限时健康块按「危险优先」显式降级（寿命值先让位，`pre_eol_info ∈ {2,3}` 时预警值永不丢弃），连身份+预警都放不下的宽度则整台设备计入「更多设备」而不画半截值。修复后 31 列 5 行卡片渲染为

  ```
  │[eMMC] mmcblk0 · 58.24 GiB     │
  │Health: Warning · Pre-EOL:     │
  │Warning(80%)                   │
  ```

  6 行时四个值全在。（**第二轮更正，见 §13**：本行原写「59 列仍保持单行短标签」，但该单行示例丢了两条寿命估算值；第二轮按有效字段统一评分后，卡片足够高时 59 列会保留寿命估算（紧凑拼写可落单行，否则让到第二行），由 `test_render_mission_card_keeps_the_life_estimates_while_they_fit` 钉住，不再出现「单行却少一个值」。）
- **P3（健壮性）**：同区域 `ellipsize` 逐字符累加宽度，对 VS16/ZWJ 等非可加情形会超出预算（`ellipsize("❤️❤️❤️", 2)` 实测返回 3 列宽）；改为按用户可感字符簇前进、对**完整候选串**测宽。（**第二轮更正，见 §13**：第一轮手写字符簇白名单 + 裁口回退基字符，复审证明 `A`+ZWJ+ZWJ 后仍留下孤立连接符、印地文连字被从随音符号处切开；第二轮改用 ratatui 公开的 `Span::styled_graphemes`（内部真 `UnicodeSegmentation`，无新依赖），不再手写启发式、也不再「回退基字符」。）正常 Linux `block_path` 为 ASCII，故该项是实现健壮性补强，不主张为实机缺陷。
- **新增/改动测试**（均在 `crates/rsetup-app/src/tui.rs`）：`test_wrapped_rows_matches_the_paragraph_word_wrap`（以独立 `TestBackend` 渲染作 oracle 的全矩阵差分）、`test_render_mission_gives_the_mmc_card_the_rows_it_needs`、`test_render_mission_prefers_the_danger_row_when_the_card_is_short`、`test_mmc_warning_survives_every_card_width`、`test_render_mission_card_keeps_the_life_estimates_while_they_fit`、`test_render_mission_narrow_card_drops_life_before_the_warning`、`test_mmc_warning_degrades_by_decision_not_by_clipping`、`test_mmc_card_without_room_counts_the_device_out_instead_of_halving_it`、`test_ellipsize_never_splits_a_grapheme`、`test_mmc_identity_line_cuts_a_wide_identity_on_a_cluster_boundary`。（**第二轮更正，见 §13；第三轮更正过滤名称，见 §14**：并非所列十例改前均为红——只有 6 例随扫描矩阵红。那次 RED 运行的命令是 `cargo test -p rsetup-next --bin rsetup-next tui::tests::test_`（**不是**按 `test_wrapped_rows_matches_the_paragraph_word_wrap` 单例过滤——该过滤器只命中 1 例，报不出 24 例的合计），结果 `18 passed; 6 failed`；`test_ellipsize_never_splits_a_grapheme` 则是在还原旧 `ellipsize` 后按 `tui::tests::test_ellipsize` 过滤运行、报 `0 passed; 1 failed`。`test_mmc_warning_degrades_by_decision_not_by_clipping`、`test_mmc_card_without_room_...`、`test_mmc_identity_line_cuts_...` 是排版/健壮性钉桩，旧实现亦通过。`18 passed; 6 failed` 只反映那次过滤运行，不代表十例全红。数据来源：`.superpowers/sdd/all-remaining-2026-10-20/mmc-wrap-fix-report.md` 第 3 节。）
- **门禁**：（**第三轮更正，见 §14**：本行的 `102 passed` 是**首轮**末态数字。）首轮：`cargo test -p rsetup-next --bin rsetup-next` → `102 passed; 0 failed`，退出码 0；`cargo clippy --package rsetup-next --all-targets -- -D warnings` 退出码 0；`cargo fmt --package rsetup-next -- --check` 退出码 0（未执行 `cargo fmt --all`，理由见 `AGENTS.md`）。第二轮末态同一命令为 `107 passed; 0 failed`（§13），第三轮末态为 `112 passed; 0 failed`（§14）。第 11 节记录的 workspace 级全量门禁在第二轮、第三轮均未重跑（共享工作树同时有其他改动在写）；第三轮改跑的是任务指定的三条包级门禁，见 §14。
- **未做**：**Rock3A 实机复检未做**——本次未连接真机、未构建 aarch64 产物、未部署、未重采截图；第 10 节第 1 项「实机复检待做」的状态不变。本次亦未提交、未暂存、未推送。逐项细节见 `.superpowers/sdd/all-remaining-2026-10-20/mmc-wrap-fix-report.md`。

## 13. 第二轮复审（P2×2 + P3×2）的整改与首轮数据更正（2026-10-21 补记，第三轮补写）

本节是补写的：第 12 节三处「见 §13」的引用在第二轮并没有落成章节（第三轮复审把它列为文档项之一），历史链条因此断了一节。以下内容按第二轮留在工作树中的实现（`git diff` 现状）与其测试名单还原，不追加第二轮未做过的实现主张。

- **范围**：第二轮处理第 12 节实现被复审出的四项——P2-1 空块被计一行（健康单设备卡片多出幽灵空行）、P2-2 第二台告警设备既不显示也不被计数、P3-3 scratch 上界不是上界且截断行数被当作精确行数、P3-4 手写字符簇白名单加「裁口回退基字符」。
- **实现改动**（均在 `crates/rsetup-app/src/tui.rs`）：
  - `wrapped_rows` 返回 `RowFit::{Fits,Overflow}`：只有 scratch 渲染未被截断时 `Fits` 才是精确行数，被截断就如实报 `Overflow`；新增常量 `MAX_WRAP_ROWS = 512` 只用于封住病态输入（超长单行）的 scratch 规模；`wrapped_rows_capped` 在溢出时返回 `cap + 1`，让上层「一定放不下」；
  - 在第一轮「按剩余行索取」（`max_rows`）的设备块之上，候选健康块改为按**有效字段数**评分（`fit_block`：`pre_eol_info == 1` 的 `Normal` 不计字段，同字段数内再取行数最少），且连身份 + 危险值都放不下的设备整块返回 `None`，由调用方计入「更多设备」而不是画半台；
  - `ellipsize` 改用 ratatui 公开的 `Span::styled_graphemes`（内部即 `UnicodeSegmentation`），删除手写白名单与「回退基字符」；
  - `render_mission` 把 `headroom` 的计算上提到行数估算之前，同时作为所有度量的上限。
- **第二轮新增 5 例**（均在同一文件）：
  1. `test_wrapped_rows_of_an_empty_block_is_zero` — 空块（含 0 内宽）为 0 行、单个显式空行是 1 行；`render_mission` 排出的 33×30 健康单设备卡片内不得出现空内容行（P2-1）；
  2. `test_second_warning_device_is_accounted_for` — 两台告警 eMMC 挤不进一张 33×5 卡片时（中英）：要么两台都在屏，要么提示中出现「更多设备」；在屏设备的 pre-EOL 预警值必须整条在屏，不许留残缺片段（P2-2）；
  3. `test_wrapped_rows_tracks_ratatui_on_adversarial_content` — 前后带双空格的行、31 个 CJK 字符、`"x "` × 40、六个双字母词，这 4 个对抗块在 3/5 内宽（预算 511）下的度量与独立渲染 oracle 逐块一致（P3-3）；
  4. `test_wrapped_rows_is_resource_bounded` — `"x "` × 65535 在 59 内宽、5 行预算下必须 `Overflow`（不得返回被截断的计数）、空块在 `usize::MAX` 预算下 0 行、20 字符长词在 3 列按 oracle 精确 `Fits`；
  5. `test_ellipsize_cuts_on_a_real_grapheme_boundary` — `A`+ZWJ+ZWJ、VS16 心形、区域指示符国旗、组合重音、残缺 ZWJ 家庭、印地文连字共 6 个 fixture × `max ∈ 0..=w+2`，裁口必须是 oracle 字符簇序列的整簇前缀（P3-4）。
- **门禁（第二轮末态）**：`cargo test -p rsetup-next --bin rsetup-next` → `107 passed; 0 failed`，退出码 0（第三轮开工前重跑基线复现该数字）。第二轮未重跑第 11 节的 workspace 级全量门禁，亦未做 Rock3A 实机复检。
- **第二轮之后仍存在的缺陷（由第三轮处理，见 §14）**：错误行可独占全部内容行使设备静默丢失；「更多设备」提示固定按 1 行预留并按 1 行渲染；`wrapped_rows` 以 scratch 最后一行是否空白判断段落结束；以及本节缺失。

## 14. 第三轮复审（P2×2 + P3×2）的整改（2026-10-21 补记）

起跑线是第二轮末态的 `107 passed; 0 failed`（本次开工前重跑复现）。改动只落在 `crates/rsetup-app/src/tui.rs` 与本文档；证据为 `TestBackend` 的最终 `Buffer`，以及「把被修掉的行为原样还原回去」的变异运行。

### 14.1 P2：错误行不再独占内容行，设备不会静默丢失

第二轮的顺序是「错误行先吃，吃满为止；设备用剩下的；提示用错误行没吃到的最后一行」。当错误行的行数恰好等于预算时，设备拿到 0 预算、提示也拿到 0 行，**两台告警 eMMC 既不显示也不计数**（评审 P2-1；实测复现于 `En 33×5`、`En 18×9`）：

```
修复前 En 33×5（3 个内容行）              修复后 En 33×5
┌ Storage Devices ──────────────┐        ┌ Storage Devices ──────────────┐
│NVMe device enumeration failed:│        │NVMe device enumeration failed:│
│unable to manage hardware:     │        │unable to manage hardware: nv… │
│nvme-enumeration-eio           │        │... 2 more device(s)           │
└───────────────────────────────┘        └───────────────────────────────┘
  ↑ 3 个内容行全归错误行，无提示行          ↑ 错误少说一截，设备数仍在
```

- 提示所需的行在**分配任何内容行之前**扣出：`storage_more_hint_floor` 用最紧凑拼写（`+N`）在该卡片内宽下的真实行数作为预留额度；错误行的预算是 `budget - 额度`，设备之间的保留额也按同一额度计算（第二轮固定为 1）。卡片没有设备时不扣——没有可数的东西。
- 错误行放不进剩余额度时不再整条丢弃：`fit_line_in_rows` 保留行首、按 `ellipsize` 的省略号交出尾部（跨 span 时各 span 保样式，是否放得下仍由 `wrapped_rows` 实测），短卡片上是「错误少说一点 + 设备数还在」，而不是二选一。
- 单内容行卡片（`budget == 1`）且存在设备时，这一行给设备计数——按评审「一台都显示不下也必须把总数呈现在提示里」的硬要求，此时由错误行让位（`En 33×3` 只有 `... 2 more device(s)`）。
- 不变量：`test_error_rows_leave_the_device_count_visible` 在 18/25/33/45/61 列 × 3..=16 行 × 中英共 140 个卡片形状上断言「在屏台数 + 提示数字 == 实际台数」，且显示出的设备不得被重复计数；在屏台数以身份行的 `[eMMC]` 徽标计数（内宽 16 时块设备名本身会被收缩，数字名不可靠）。
- `test_error_and_device_count_share_a_short_card` 钉住评审的 33×4：第一行是被省略号标注过短的错误（其去空白文本必须是原错误文本的前缀，不允许无故省略号、也不允许切成半截标签），第二行必须是完整的一条提示。
- `test_mission_counts_the_devices_a_narrow_card_cannot_show` 走 `render_mission` 自己排布的 18/21 列 × 19 行视口：存储卡片被夹到 3 行，其下服务卡片仍在，那 1 行内容必须是完整提示。

### 14.2 P2：「更多设备」提示按真实度量取形，绝不画残缺单词

第二轮把提示固定按 1 行预留、也只准备 1 行的文案。18 列卡片内宽只有 16，英文 `... 2 more device(s)` 需要 2 行，于是那唯一一行被画成 `... 2 more`，`device(s)` 掉出卡片（评审 P2-2，修复前实测）：

```
修复前 En 18×3        修复后 En 18×3        修复后 ZhCn 18×3
┌ Storage Devices┐    ┌ Storage Devices┐    ┌ 存 储 状 态  ──────┐
│... 2 more      │    │... +2          │    │... 更 多 设 备  2  │
└────────────────┘    └────────────────┘    └────────────────┘
```

- `storage_more_hints` 给出三级文案阶梯（富→简）：`... N more device(s)` / `... +更多设备 N`（中文语序）、`... +N`、`+N`；`storage_more_hint` 用 `wrapped_rows` 逐级实测，取**能在剩余行内整条画完**的最富形态；连最简的一级都放不下时宁可不画，也绝不画残缺单词。
- 预留额度（14.1）取阶梯最末一级 `+N` 的真实行数：英文在 16 内宽下不再按 1 行误算，因而设备循环不会把提示要用的行花掉。
- `test_more_devices_hint_is_painted_whole_at_every_width` 在 18..=61 列（内宽 16..=59）× 中英、单内容行卡片上断言：那一行去空白文本必须**恰好等于**三级文案之一，且必须是「放得下的最富一级」——把紧凑形态删掉的变异运行会直接报 `painted ""`。

### 14.3 P3：`wrapped_rows` 不再以空白行判断段落结束

旧实现读 scratch buffer 的最后一个非空行来同时回答两个问题（要几行、有没有超出预算），而 ratatui 的 `Paragraph` 对**空行与「空白被折出的行」不写任何单元格**：`wrapped_rows(["abcde", "", "WARNING"], 3, 2)` 里 `WARNING` 已在缓冲之外，缓冲的最后一行却是空行的空白，于是报出 `Fits(2)`（评审 P3-3 的逐字反例）。

- 现在探针块在预算之外多备两行：一行区分「填满缓冲 / 在缓冲内结束」，一行是 `ROW_PROBE`（`#`，必画）。渲染两次——原块与「原块 + 探针行」——**探针是否出现**才是「是否越界」的判据：出现则行数即探针所在行（精确，且把纯空白行也算进去），没出现则确定越界（`Overflow`）。空白行不再可能被误读成段落结束。
- 探针高度上限仍是 `MAX_WRAP_ROWS`（512），资源上界测试不变。
- `test_wrapped_rows_never_reads_a_blank_row_as_the_end_of_a_block`：评审反例在预算 0..=5 下一律 `Overflow`，预算 6 时精确 `Fits(6)`；补上「以空白行结尾」的同源盲区（`["abcdef", ""]` 在 3 列是 3 行，预算 2 时 `Overflow`、预算 3 时 `Fits(3)`）；并钉住空块 0 行、显式空行照旧各占 1 行。
- 既有的四组度量/边界差分——第一轮的 `test_wrapped_rows_matches_the_paragraph_word_wrap`，第二轮的 `..._tracks_ratatui_on_adversarial_content`、`..._is_resource_bounded`、`..._of_an_empty_block_is_zero`——在新的判据下全部保持绿。

### 14.4 新增测试与 RED 证据

本轮新增 5 例（均在 `crates/rsetup-app/src/tui.rs`）：`test_wrapped_rows_never_reads_a_blank_row_as_the_end_of_a_block`、`test_more_devices_hint_is_painted_whole_at_every_width`、`test_error_rows_leave_the_device_count_visible`、`test_error_and_device_count_share_a_short_card`、`test_mission_counts_the_devices_a_narrow_card_cannot_show`。

把被修的行为单独还原回去（只改生产代码，不动测试），对应测试变红、其余保持绿——证明锁定的是本轮缺陷，而非顺带改到的排版：

| 变异（只还原被修的那一处行为） | 结果 |
| --- | --- |
| `wrapped_rows` 还原为「探针 = cap+1 且读最后非空行」 | `cargo test -p rsetup-next --bin rsetup-next wrapped_rows` → `4 passed; 1 failed`，失败断言 `left: Fits(2)`、`right: Overflow`（评审反例本身） |
| 错误行重新可独占内容行（去掉提示预留与缩短） | 全量 → `110 passed; 2 failed`：`test_error_rows_leave_the_device_count_visible`（`En at a 18x9 card: 0 of 2 device(s) are on screen and the card counts []`）、`test_error_and_device_count_share_a_short_card` |
| 提示只保留完整拼写（删除 `... +N` / `+N`） | 全量 → `109 passed; 3 failed`：`test_more_devices_hint_is_painted_whole_at_every_width`（`En at a 18x3 card painted ""`）、`test_error_rows_leave_the_device_count_visible`（`En at a 18x3 card: ... counts []`）、`test_mission_counts_the_devices_a_narrow_card_cannot_show` |

三组变异均只用于取证，运行后从备份原样还原文件，最终树与 GREEN 运行同一份代码。

### 14.5 门禁（本轮末态）

- `cargo test -p rsetup-next --bin rsetup-next` → `test result: ok. 112 passed; 0 failed; 0 ignored`，退出码 0（第二轮 107 + 本轮 5）。
- `cargo clippy -p rsetup-next --all-targets -- -D warnings` → 退出码 0，无告警。
- `make fmt-check` → 退出码 0、无差异输出（按 `FMT_PACKAGES` 逐包 `cargo fmt -- --check`；未执行 `cargo fmt --all`，理由见 `AGENTS.md`）。

### 14.6 未做

- **Rock3A 实机复检仍未做**：未连接真机、未构建 aarch64 产物、未部署、未重采截图，第 10 节第 1 项状态不变。
- 第 11 节的 workspace 级全量门禁（`cargo test --workspace --locked`、`cargo clippy --workspace`、`node --test ui/*.test.mjs`）未重跑：共享工作树同时有其他改动在写，本轮只跑任务指定的三条包级门禁。
- 未提交、未暂存、未推送。

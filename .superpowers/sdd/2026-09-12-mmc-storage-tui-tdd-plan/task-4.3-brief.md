# Task 4.3 Brief: 实现多盘自适应 `render_storage_summary` (tui.rs)

## 目标
将 `crates/rsetup-app/src/tui.rs` 中独立的 NVMe 渲染（`render_nvme_summary` + `render_mission` 中的高度计算）重构为统一的 `render_storage_summary`：在同一个“存储状态”卡片中渲染所有 NVMe 设备与所有 MMC/SD 设备，高度按设备数自适应，超出可视空间时截断尾部设备并显示 “更多设备” 提示行。

## 布局裁决（binding，必须遵守）
1. **NVMe 设备保持现有 3 行布局不变**（行1 标识/型号/容量、行2 状态/温度/寿命/备用、行3 读写/阈值），包括现有的“窄屏时已用寿命标签降级为短标签”逻辑（`Used Endurance:` → `Used:`）。理由：现有 3 个专项测试验证过该布局在 59 列（100 列终端的左侧面板）内的适配性，紧凑化为 2 行会挤爆 59 列。
2. **MMC/SD 设备每盘 2 行**：
   - 行1：`[{类型}] {block_path} · {model} · {manufacturer} · {容量}`
     - 类型标签：`card_type == "MMC"` → `locale.text("storage_emmc")`（eMMC）；`"SD"` → `locale.text("storage_sd")`（SD 卡 / SD Card）。
     - 容量用 `crate::format_bytes(total_bytes)`。
   - 行2：`{状态label}: {health} · {SLC label}: {a} · {MLC label}: {b} · {预警label}: {eol}`
     - 状态 label = `locale.text("storage_health")`（状态 / Health）。
     - health：`warning_flags` 非空或 `pre_eol_info == 3` → `storage_critical`（故障/Critical，CORAL 色）；`pre_eol_info == 2` 或 flags 非空 → `storage_warning`（告警/Warning，AMBER 色）；否则 `storage_healthy`（正常/Healthy，SIGNAL 色）。
     - SLC/MLC label = `storage_life_a` / `storage_life_b`；值：`Some(p)` → `"{p}%"`，`None` → `storage_na`（不支持/N/A）。
     - 预警 label = `storage_pre_eol`（预警/Pre-EOL）；值映射：0 → `storage_eol_undefined`、1 → `storage_eol_normal`、2 → `storage_eol_warning`、3 → `storage_eol_urgent`、其他 → `storage_eol_undefined`。
     - 所有 label 与值的格式与现有 NVMe 行2 一致：`label` 为 MUTED 色，值为 BONE 色，字段间 ` · `（MUTED 色）分隔；health 值按上述状态着色。
3. **卡片标题**：`instrument(locale.text("storage_telemetry"))`（存储状态 / Storage Devices）。
4. **无设备状态**：NVMe 与 MMC 均未初始化或设备列表都为空时，单行显示 `locale.text("storage_not_detected")`（MUTED 色），即高度 3。
   - 注意：只有 NVMe 未初始化但 MMC 有设备（或反之）时，**不显示**任何未检测提示，只渲染存在的设备。
5. **高度计算**（替换 `render_mission` 中现有 `has_nvme_devices`/`desired_nvme_height`/`nvme_headroom`/`nvme_height` 逻辑）：
   - 设 `N = nvme_devices.len() + mmc_devices.len()`（未初始化的子系统按 0 计）。
   - 每设备内容行数：NVMe = 3，MMC = 2；设备之间 1 空行（即内容总行数 = `3*nvme + 2*mmc + (N-1)`，N=0 时为 0）。
   - `desired = 2 + 内容行数`（N=0 时为 3）。
   - `headroom = area.height.saturating_sub(6 + 6 + 4)`；`height = desired.min(headroom).max(3)`。
   - 设备渲染顺序：**NVMe 全部在前，MMC 全部在后**。若 `2 + 内容行数 > height`（budget = `height - 2` 不够），按顺序累加：放不下某个设备时停止加入后续设备，剩余设备数记为 `more`；若 `budget - 已用行数 >= 1` 且 `more > 0`，在末尾追加一行 `... {locale.text("storage_more_devices")} {more}`（MUTED 色，例如 `... 更多设备 1` / `... 1 more device(s)`）。
6. 设备间空行、颜色（BONE/MUTED/AMBER/SIGNAL/CORAL）、`Wrap { trim: true }` 等均沿用现有 `render_nvme_summary` 的写法。

## 代码改动
1. 删除 `fn render_nvme_summary`，新增 `fn render_storage_summary(frame: &mut Frame, app: &App, area: Rect)` 实现上述逻辑（NVMe 每设备的 3 行构建逻辑从旧函数原样搬入）。
2. `render_mission`：
   - 更新高度计算（见布局裁决第 5 条）；
   - `render_nvme_summary(frame, app, rows[2])` 调用改为 `render_storage_summary(frame, app, rows[2])`。
3. 不改动 `App` 结构体、`refresh`、i18n、其他渲染函数。

## 测试改动（TDD：先改测试观察失败，再实现）
**更新现有测试**（在 `tui.rs` 的 `#[cfg(test)]` 中）：
1. 4 个直接调用 `render_nvme_summary` 的测试（`test_render_nvme_summary_demo`、`test_render_nvme_summary_fits_available_spare`、`test_render_nvme_summary_uses_full_endurance_label_when_wide`、`test_render_nvme_summary_warning_state`）：
   - 将调用改为 `render_storage_summary`；测试函数更名为 `test_render_storage_summary_demo`、`test_render_storage_summary_fits_available_spare`、`test_render_storage_summary_uses_full_endurance_label_when_wide`、`test_render_storage_summary_warning_state`。
   - **注意**：demo 模式下 App 同时有 2 个 MMC 设备，卡片总高度变大。这些测试使用的小 backend（如 80x6、61x6）高度不足时，按布局规则会截断 MMC 设备——NVMe 是第一个设备，其 3 行始终完整渲染，因此原有断言（备用/阈值/寿命/型号等）应继续成立。若个别断言因布局变化失败，调整断言至与布局裁决一致（不得弱化 NVMe 3 行关键断言）。
2. `test_render_full_tui_with_nvme_zh`：
   - 标题断言 `norm_text.contains("NVMe存储遥测")` 改为 `norm_text.contains("存储状态")`；
   - 其余 NVMe 断言保持不变（100x30 下 NVMe 3 行必须仍完整可见——这是回归红线，若失败说明高度计算错误）；
   - 可追加断言 `raw_text.contains("FE4MB4")`（demo eMMC 应出现）。
3. `test_render_full_tui_uninitialized_nvme` 更名为 `test_render_full_tui_no_storage_devices`：
   - 在原有设置 `app.nvme_status` 为空的基础上，**同时**设置 `app.mmc_status = rsetup_core::MmcStatus { initialized: false, devices: vec![], message: Some("No MMC".into()) }`；
   - 中文断言改为 `norm_text.contains("未检测到NVMe或MMC存储设备")`；
   - 英文断言改为 `raw_text.contains("No NVMe or MMC storage devices detected.")`。

**新增测试**：
4. `test_render_storage_summary_mmc_only`：demo App，将 `app.nvme_status` 置为空（`initialized: false, devices: vec![], message: None`），保留 2 个 demo MMC 设备；在 100x20 backend 渲染整个 `render`（或直接 `render_storage_summary` 于足够大的 area），断言包含 `"FE4MB4"`、`"/dev/mmcblk0"`、`"SC64G"`、`"/dev/mmcblk1"`、`"10%"`、`"存储状态"`（ZhCn）；再对 Locale::En 断言 `"eMMC"`、`"SD Card"`、`"N/A"`（SD 卡寿命为 None）。
5. `test_render_storage_summary_multi_device_all_visible_when_tall`：demo App（1 NVMe + 2 MMC）不变，在 **100x40** backend 渲染 `render`，断言 `nvme0`、`/dev/mmcblk0`、`/dev/mmcblk1` 三者同时出现（高终端下全部可见，无截断）。
6. `test_render_storage_summary_empty`：demo App，`nvme_status` 与 `mmc_status` 均置空，在 100x20 渲染，断言 ZhCn 含 `"未检测到NVMe或MMC存储设备"`、En 含 `"No NVMe or MMC storage devices detected."`。

## 验证与提交
- **正确的测试包名是 `rsetup-next`**。
- `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui` 全部通过（含更新与新增的全部测试）。
- `PATH="/usr/bin:$PATH" cargo test -p rsetup-next` 整个 app crate 无回归。
- 提交信息：`feat(app): unified multi-device storage TUI panel`

## 回归红线（必须全部成立）
- NVMe 单设备 3 行布局在 59 列内完整（备用 100%、阈值 10% 可见）；
- 100x30 中文全 TUI 下 NVMe 三行内容完整；
- 空设备状态文案正确（zh/en）；
- 60x18 与 40x12 小视口渲染不 panic（现有 `test_render_tui_small_viewport`）。

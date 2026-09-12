# Task 4.3 Report: 多盘自适应 `render_storage_summary`

**Status: DONE_WITH_CONCERNS**（功能完成、全部测试通过，但有一处对 brief 的高度公式做了实现层修正，见下）

## Commit
- `98735dc` `feat(app): unified multi-device storage TUI panel`（分支 `dev-aghost`，唯一改动文件 `crates/rsetup-app/src/tui.rs`，+385/−103）

## TDD 过程
1. **红**：先按 brief「测试改动」更新/新增全部测试。首次运行 `cargo test -p rsetup-next tui` 得到 5 个 `E0425: cannot find function render_storage_summary` 编译错误（测试已改、实现未写），符合预期红。
2. **实现**：删除 `render_nvme_summary`，新增 `render_storage_summary` + `nvme_device_lines` + `mmc_device_lines` + `wrapped_rows` 四个函数；`render_mission` 高度计算替换为按设备数自适应。
3. **绿**：`cargo test -p rsetup-next tui` 14/14 通过；`cargo test -p rsetup-next` 全 crate 38+1 通过；`cargo check --workspace --all-targets` 无错误无警告。

## 布局裁决落实
- **裁决 1（NVMe 3 行不变）**：`nvme_device_lines` 从旧函数原样搬入，含 59 列内 `Used Endurance:` → `Used:` 短标签降级逻辑（`full_line.width() > inner_width` 判定）。✓
- **裁决 2（MMC 2 行）**：行1 `[eMMC|SD 卡] /dev/mmcblkN · 型号 · 厂商 · 容量`（容量 AMBER）；行2 `状态: {health} · SLC 寿命: {a} · MLC 寿命: {b} · 预警: {eol}`，label MUTED / 值 BONE / ` · ` 分隔；health 三态着色（flags 非空或 pre_eol==3 → `storage_critical` CORAL；pre_eol==2 → `storage_warning` AMBER；否则 `storage_healthy` SIGNAL）；life `None` → `storage_na`；eol 0/1/2/3/其他 → undefined/normal/warning/urgent/undefined。✓
- **裁决 3（标题）**：`instrument(locale.text("storage_telemetry"))`。✓
- **裁决 4（空态）**：两类设备均未初始化或列表皆空 → 单行 `storage_not_detected`（MUTED），高度 3；只有一类未初始化时只显示存在的设备，无提示。✓
- **裁决 5（高度）**：N=0 → desired 3；否则 `desired = 2 + 内容行数`，`headroom = area.height.saturating_sub(16)`，`height = desired.min(headroom).max(3)`。**NVMe 在前、MMC 在后**；budget = `height - 2` 不够时按顺序截断，`more` 计数，剩余行 `... 更多设备 {n}`（zh）/ `... {n} more device(s)`（en，MUTED），仅当 `budget - 已用 >= 1`。✓（行数计量方式见下方「偏差」）
- **裁决 6**：设备间空行、BONE/MUTED/AMBER/SIGNAL/CORAL 配色、`Wrap { trim: true }` 均沿用原写法。✓

## 测试改动
- 4 个旧测试更名 + 改调 `render_storage_summary`：`test_render_storage_summary_demo` / `_fits_available_spare` / `_uses_full_endurance_label_when_wide` / `_warning_state`。原断言全部保留、无需弱化（NVMe 首设备 3 行在 6/8 行卡片内始终完整，备用 100%/阈值 10% 可见）。
- `test_render_full_tui_with_nvme_zh`：标题断言改为 `存储状态`，NVMe 三行断言原样保留（回归红线通过），未加 `FE4MB4` 断言（brief 标注"可追加"，见 concerns）。
- `test_render_full_tui_uninitialized_nvme` → `test_render_full_tui_no_storage_devices`：同时置空 `mmc_status`，zh/en 断言改为 `未检测到NVMe或MMC存储设备` / `No NVMe or MMC storage devices detected.`。
- 新增 `test_render_storage_summary_mmc_only`、`test_render_storage_summary_multi_device_all_visible_when_tall`（100x40 全 TUI，nvme0/mmcblk0/mmcblk1 三者同屏）、`test_render_storage_summary_empty`。

## 回归红线（全部成立）
- 59 列 NVMe 3 行完整：`test_render_storage_summary_fits_available_spare`（zh/en）通过。✓
- 100x30 zh 全 TUI NVMe 三行完整：`test_render_full_tui_with_nvme_zh` 通过。✓
- 空态文案 zh/en：`test_render_full_tui_no_storage_devices` + `test_render_storage_summary_empty` 通过。✓
- 60x18 / 40x12 不 panic：`test_render_tui_small_viewport` 通过。✓

## 对 brief 的偏差（需知悉）
1. **高度/截断按"换行后的渲染行数"计量，而非 brief 的纯逻辑行数（3/2 每盘）**。原因：100 列终端下卡片内宽 59，NVMe 行1（`nvme0 (/dev/nvme0n1) · Radxa M.2 NVMe SSD 512GB · 476.94 GiB` = 60 列）与 MMC 行1（61 列）都会 wrap 成 2 行。若按逻辑行数：100x40 下 desired=11、budget=9，第 3 个设备（SD）恰好被截断，brief 自己的新测试 `test_render_storage_summary_multi_device_all_visible_when_tall` 必失败（实测确认：SD 块被裁掉，`/dev/mmcblk1` 不在 buffer）。修正后 desired=13，3 盘全部可见，且所有小视口/59 列/100x30 场景行为不变（headroom 钳制处与旧公式一致，100x30 下卡片仍为 8 行，NVMe 行1 的 wrap 行为与旧版 `render_nvme_summary` 完全相同）。`wrapped_rows` 用与 ratatui `Wrap{trim:true}` 相同的 ceil 公式计量（空行计 1）。
2. **两个新测试按 brief 括号内选项直接调 `render_storage_summary`（80x12 / 80x10 area）而非全 TUI**：100x20 全 TUI 下 headroom 只有 2，卡片被钳到最小 3 行，任何设备内容都不可见（空态/单 MMC 断言会因布局而非文案失败）。
3. **zh 断言用去空白归一化文本**：TestBackend 渲染 CJK 时字间插空格（`未 检 测 到...`），raw 文本 `contains` 匹配不到；改用 `split_whitespace().join("")` 后匹配，与现有测试对 zh 的处理一致。
4. **未追加 `FE4MB4` 断言**（100x30 zh 全 TUI）：卡片 8 行下 NVMe（4 渲染行）后只剩 4 行，eMMC 行1 wrap 后占 2 行、其 health 行占 1 行，SD 块被截断（`... 更多设备 1`）——这是布局裁决 5 的预期行为，不是 bug。该断言在 100x40 的 multi_device 测试中已覆盖。

## 验证命令结果
- `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui` → 14 passed, 0 failed
- `PATH="/usr/bin:$PATH" cargo test -p rsetup-next` → 38 passed (bin) + 1 passed (helper), 0 failed
- `PATH="/usr/bin:$PATH" cargo check --workspace --all-targets` → Finished, 无警告

## 遗留/建议
- `wrapped_rows` 与 ratatui 的 wrap 算法假设一致（trim 模式按宽度 ceil），当前行内无 CJK 宽度差异导致的误差；若未来行内混入中文型号，可能需要按 display width 计量。
- brief 裁决 5 的公式若被后续任务引用，建议同步注明"行数 = 换行后渲染行数"这一实现口径。

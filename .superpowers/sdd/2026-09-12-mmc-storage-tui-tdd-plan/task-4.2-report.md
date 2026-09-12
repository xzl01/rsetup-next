# Task 4.2 Report: TUI App 状态接入 (tui.rs)

## Status: DONE

## Commits
- `4e61103` `feat(app): wire MMC status into TUI app state` (branch `dev-aghost`, 1 file changed, 27 insertions)

## TDD 过程
1. **红**：先新增测试 `test_tui_app_loads_and_refreshes_mmc_status`（紧挨 `test_tui_app_loads_and_refreshes_nvme_status` 之后），运行 `cargo test -p rsetup-next tui` 观察到预期编译错误：`error[E0609]: no field 'mmc_status' on type 'tui::App'`（4 处）。
2. **绿**：实现后重跑，全部通过。

## 实现内容（仅 `crates/rsetup-app/src/tui.rs`，状态层，未触碰渲染逻辑）
1. **App 结构体**：在 `nvme_status` 之后新增字段
   `pub(crate) mmc_status: rsetup_core::MmcStatus,`
2. **`App::new`**：在 `nvme_status` 获取之后、`Self { ... }` 之前新增
   ```rust
   let mmc_status = controller.mmc_status().unwrap_or_else(|_| rsetup_core::MmcStatus {
       initialized: false,
       devices: vec![],
       message: Some("Failed to query MMC status".into()),
   });
   ```
   并在字面量 `nvme_status,` 之后加入 `mmc_status,`。
3. **`App::refresh`**：在 `self.nvme_status = ...` 赋值块之后、`if self.source_picker { ... }` 之前新增对称的 `self.mmc_status = ...` 赋值块（相同的 `unwrap_or_else` 兜底）。
4. **导入**：无需改动，`rsetup_core::MmcStatus` 以全限定引用（crate 根已 re-export）。

## 测试与验证
- 新增测试 `test_tui_app_loads_and_refreshes_mmc_status`：Demo 模式下 `App::new` 与 `refresh` 后均断言 `initialized == true` 且 `devices.len() == 2`（eMMC + SD），与 brief 中的代码逐字一致。
- `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui`：**11 passed, 0 failed**（含原有 `test_tui_app_loads_and_refreshes_nvme_status`，无回归）。
- `PATH="/usr/bin:$PATH" cargo test -p rsetup-next`：**35 + 1 passed, 0 failed**（整个 app crate 无回归）。

## 约束遵守
- 未修改/删除 `render_nvme_summary`、`render_mission` 或任何渲染逻辑（任务 4.3 范围）。
- 未派发子代理。

## Concerns
- 无。

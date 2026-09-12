# Task 4.2 Brief: TUI App 状态接入 (tui.rs)

## Background
`crates/rsetup-app/src/tui.rs` 的 `App` 结构体（约 71 行）当前有字段 `pub(crate) nvme_status: rsetup_core::NvmeStatus`。`App::new`（约 92 行）与 `App::refresh`（约 190 行）都通过 `controller.nvme_status().unwrap_or_else(...)` 填充该字段。

本任务为 TUI 增加 MMC 状态接入，与 NVMe 对称。**本任务只改 App 状态层（结构体字段 + new + refresh + 一个测试），不改任何渲染逻辑（`render_nvme_summary` / `render_mission` 等保持原样，由任务 4.3 处理）。**

## Requirements

### 1. `crates/rsetup-app/src/tui.rs`

1. **App 结构体新增字段**（紧挨 `nvme_status` 之后）：
```rust
    pub(crate) mmc_status: rsetup_core::MmcStatus,
```

2. **`App::new`**：在 `nvme_status` 的获取代码之后、构造 `Self { ... }` 之前，新增：
```rust
        let mmc_status = controller.mmc_status().unwrap_or_else(|_| rsetup_core::MmcStatus {
            initialized: false,
            devices: vec![],
            message: Some("Failed to query MMC status".into()),
        });
```
   并在 `Self { ... }` 字面量中、`nvme_status,` 之后加入 `mmc_status,`。

3. **`App::refresh`**：在 `self.nvme_status = ...` 赋值块之后，新增：
```rust
        self.mmc_status = self
            .controller
            .mmc_status()
            .unwrap_or_else(|_| rsetup_core::MmcStatus {
                initialized: false,
                devices: vec![],
                message: Some("Failed to query MMC status".into()),
            });
```
   （放在 `if self.source_picker { ... }` 之前，保持与 nvme 赋值相同的顺序习惯。）

4. **导入检查**：`rsetup_core::MmcStatus` 已通过 crate 根 re-export（任务 1.1 已完成），`tui.rs` 中以 `rsetup_core::MmcStatus` 全限定引用即可，无需改 import。

### 2. 单元测试（`tui.rs` 的 `#[cfg(test)]`，紧挨 `test_tui_app_loads_and_refreshes_nvme_status` 之后新增）

参照 `test_tui_app_loads_and_refreshes_nvme_status` 的写法：
```rust
    #[test]
    fn test_tui_app_loads_and_refreshes_mmc_status() {
        let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
        let mut app = App::new(controller, Locale::En).expect("init app");
        // Demo mode returns 2 MMC devices (eMMC + SD).
        assert!(app.mmc_status.initialized);
        assert_eq!(app.mmc_status.devices.len(), 2);
        app.refresh().expect("refresh app");
        assert!(app.mmc_status.initialized);
        assert_eq!(app.mmc_status.devices.len(), 2);
    }
```
   - 注意：Demo 模式的 `demo_mmc_status()`（任务 3.1）返回 2 个设备。

### 3. 验证与提交
- **正确的测试包名是 `rsetup-next`**（`crates/rsetup-app/Cargo.toml` 中 `[package] name = "rsetup-next"`）。
- 运行 `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui` 全部通过（不得回归现有 tui 测试，包括 `test_tui_app_loads_and_refreshes_nvme_status`）。
- 再运行 `PATH="/usr/bin:$PATH" cargo test -p rsetup-next` 确认整个 app crate 无回归。
- 提交信息：`feat(app): wire MMC status into TUI app state`

## 注意
- 本任务**不**删除或修改 `render_nvme_summary`、`render_mission`，也**不**改变 TUI 渲染行为。渲染重构是任务 4.3。
- 若 `App::new` 中因新增字段导致结构体字面量缺少 `mmc_status` 而编译报错，这正是 TDD 红阶段的预期。

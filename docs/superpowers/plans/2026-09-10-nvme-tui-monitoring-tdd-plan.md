# TUI NVMe 磁盘监控与 SMART 遥测功能 TDD 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 `rsetup-next` 的终端用户界面（TUI）中集成 NVMe 固态硬盘健康状态与 SMART 遥测信息，支持中英双语、自适应终端视口，并在无 NVMe 硬件时安全优雅降级。

**Architecture:** 在 `rsetup-app` 的 TUI 状态管理器 `App` 中接入 `controller.nvme_status()`，在左侧任务监控视窗 (`render_mission`) 中增加 `render_nvme_summary` 区域，结合 `ratatui` 渲染组件与 `i18n` 字典系统，并通过 `ratatui::backend::TestBackend` 进行无界面终端测试断言。

**Tech Stack:** Rust, `ratatui` (0.29), `crossterm` (0.28), `rsetup-core`

**Spec:** `docs/superpowers/specs/2026-09-10-nvme-tui-monitoring-design.md`

## Global Constraints

- **TDD 严格流程**：每个任务必须先写失败的测试（RED），验证失败，编写最小实现使测试通过（GREEN），重构优化（REFACTOR），再提交。
- **只读安全性**：TUI 仅调用 `controller.nvme_status()`，严禁包含任何硬件写操作。
- **无破坏性渲染**：在尺寸受限的终端中严防布局溢出和 panic，遵循 CSP 与代码安全规范。
- **国际化对齐**：所有新增文本必须在中英文两套字典中严格对应。

---

## 任务拆解表

| 任务 | 目标关注点 | 涉及文件 |
| --- | --- | --- |
| **Task 1** | 多语言字典扩充与测试 | `crates/rsetup-app/src/i18n.rs` |
| **Task 2** | TUI 状态状态集成与刷新循环 | `crates/rsetup-app/src/tui.rs` |
| **Task 3** | TUI NVMe 视窗渲染与自适应布局 | `crates/rsetup-app/src/tui.rs` |
| **Task 4** | 基于 `TestBackend` 的完整 TUI 自动化测试 | `crates/rsetup-app/src/tui.rs` |
| **Task 5** | 全工作区回归与交叉编译 | `crates/rsetup-app` |
| **Task 6** | 双实机实机运行、截图生成与视觉预期判定 | `scripts/tui-screenshot.py`, 目标机 `192.168.27.88` & `192.168.27.34` |

---

### Task 1: 扩充 TUI NVMe 国际化字典

**Files:**
- Modify: `crates/rsetup-app/src/i18n.rs`

**Interfaces:**
- Consumes: `Locale::text(self, key: &str) -> &'static str`
- Produces: 新增键值 `"nvme_telemetry"`, `"nvme_healthy"`, `"nvme_warning"`, `"nvme_endurance"`, `"nvme_spare"`, `"nvme_io"`, `"nvme_not_detected"`

- [ ] **Step 1: Write the failing test**
  在 `crates/rsetup-app/src/i18n.rs` 的测试模块中添加针对新增 NVMe 键值的断言测试：
  ```rust
  #[test]
  fn test_nvme_tui_dictionary_keys() {
      let keys = [
          "nvme_telemetry",
          "nvme_healthy",
          "nvme_warning",
          "nvme_endurance",
          "nvme_spare",
          "nvme_io",
          "nvme_not_detected",
      ];
      for key in keys {
          assert_ne!(Locale::ZhCn.text(key), key, "missing zh_CN translation for {key}");
          assert_ne!(Locale::En.text(key), key, "missing En translation for {key}");
      }
  }
  ```

- [ ] **Step 2: Run test to verify it fails**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next i18n::tests::test_nvme_tui_dictionary_keys`
  预期：失败（missing zh_CN/En translation）。

- [ ] **Step 3: Write minimal implementation**
  在 `Locale::text` 的中英文匹配列表中添加上述 7 个词条翻译。

- [ ] **Step 4: Run test to verify it passes**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next i18n::tests::test_nvme_tui_dictionary_keys`
  预期：PASS。

- [ ] **Step 5: Commit**
  ```bash
  git add crates/rsetup-app/src/i18n.rs
  git commit -m "feat(i18n): add NVMe telemetry dictionary keys for TUI"
  ```

---

### Task 2: 在 TUI `App` 中集成 NVMe 状态与刷新

**Files:**
- Modify: `crates/rsetup-app/src/tui.rs`

**Interfaces:**
- Consumes: `controller.nvme_status() -> Result<NvmeStatus, HardwareError>`
- Produces: `App.nvme_status: rsetup_core::NvmeStatus`

- [ ] **Step 1: Write the failing test**
  在 `crates/rsetup-app/src/tui.rs` 测试中编写测试：
  ```rust
  #[test]
  fn test_tui_app_loads_and_refreshes_nvme_status() {
      let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
      let mut app = App::new(controller, Locale::En).expect("init app");
      assert!(app.nvme_status.initialized);
      assert_eq!(app.nvme_status.devices.len(), 1);
      app.refresh().expect("refresh app");
      assert!(app.nvme_status.initialized);
  }
  ```

- [ ] **Step 2: Run test to verify it fails**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui::tests::test_tui_app_loads_and_refreshes_nvme_status`
  预期：编译错误（`no field nvme_status on type App`）。

- [ ] **Step 3: Write minimal implementation**
  - 在 `App` 结构体中添加 `pub(crate) nvme_status: rsetup_core::NvmeStatus`；
  - 在 `App::new` 中查询 `controller.nvme_status()`；
  - 在 `App::refresh` 中重新获取 `controller.nvme_status()`。

- [ ] **Step 4: Run test to verify it passes**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui::tests::test_tui_app_loads_and_refreshes_nvme_status`
  预期：PASS。

- [ ] **Step 5: Commit**
  ```bash
  git add crates/rsetup-app/src/tui.rs
  git commit -m "feat(tui): bind NVMe status lifecycle to TUI App"
  ```

---

### Task 3: 编写 TUI NVMe 监控卡片渲染与自适应布局

**Files:**
- Modify: `crates/rsetup-app/src/tui.rs`

**Interfaces:**
- Consumes: `App.nvme_status`, `format_bytes`
- Produces: `fn render_nvme_summary(frame: &mut Frame, app: &App, area: Rect)`

- [ ] **Step 1: Write the failing test**
  使用 `ratatui::backend::TestBackend` 编写渲染验证测试：
  ```rust
  #[test]
  fn test_render_nvme_summary_demo() {
      let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
      let app = App::new(controller, Locale::ZhCn).expect("init app");
      let backend = ratatui::backend::TestBackend::new(80, 25);
      let mut terminal = Terminal::new(backend).expect("init test terminal");
      terminal.draw(|frame| {
          let area = Rect::new(0, 0, 80, 6);
          render_nvme_summary(frame, &app, area);
      }).expect("draw");
      let buffer = terminal.backend().buffer();
      let text = buffer.content().iter().map(|c| c.symbol()).collect::<String>();
      assert!(text.contains("Radxa M.2 NVMe SSD 512GB") || text.contains("nvme0"));
      assert!(text.contains("38.5") || text.contains("温度"));
  }
  ```

- [ ] **Step 2: Run test to verify it fails**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui::tests::test_render_nvme_summary_demo`
  预期：编译错误（`cannot find function render_nvme_summary`）。

- [ ] **Step 3: Write minimal implementation**
  - 实现 `render_nvme_summary` 函数：
    - 若 `!app.nvme_status.initialized`：展示单行 `app.locale.text("nvme_not_detected")`；
    - 若 `app.nvme_status.initialized` 且有设备：格式化渲染设备节点、型号、容量、温度、健康徽章、寿命百分比、读写字节量等；
  - 调整 `render_mission` 的行切分，将 NVMe 区域无缝嵌入到左侧监控视窗中。

- [ ] **Step 4: Run test to verify it passes**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui::tests::test_render_nvme_summary_demo`
  预期：PASS。

- [ ] **Step 5: Commit**
  ```bash
  git add crates/rsetup-app/src/tui.rs
  git commit -m "feat(tui): implement NVMe telemetry summary renderer"
  ```

---

### Task 4: TUI 端到端交互与无硬件降级测试套件

**Files:**
- Modify: `crates/rsetup-app/src/tui.rs`

**Interfaces:**
- Consumes: 全量 TUI 渲染流程 `render(frame, &mut app)`

- [ ] **Step 1: Write the failing tests**
  补充多语言与空状态边界测试：
  - `test_render_full_tui_with_nvme_zh`：中文全屏幕（100x30）完整 `render`，断言 NVMe 标题与设备显示；
  - `test_render_full_tui_uninitialized_nvme`：在无 NVMe 硬件状态下（构造 `initialized: false` 的 mock 或 auto 模式测试环境），断言全屏幕正常绘制，无 panic 且包含未激活提示；
  - `test_render_tui_small_viewport`：超小视口（60x18）绘制测试，断言不 panic，自适应降级。

- [ ] **Step 2: Run test to verify it fails**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui::tests`
  预期：按测试用例编写逐步验证。

- [ ] **Step 3: Implement & refine**
  完善小视口下的截断与边框保护，保证在任何极端终端大小下均稳健渲染。

- [ ] **Step 4: Run test to verify it passes**
  `PATH="/usr/bin:$PATH" cargo test -p rsetup-next tui::tests`
  预期：全部通过。

- [ ] **Step 5: Commit**
  ```bash
  git add crates/rsetup-app/src/tui.rs
  git commit -m "test(tui): add comprehensive viewport and localization tests for NVMe"
  ```

---

### Task 5: 全工作区回归与交叉编译

**Files:**
- Workspace 全量验证

- [ ] **Step 1: Workspace 测试全量通过**
  - 运行 `PATH="/usr/bin:$PATH" cargo test --workspace`
  - 运行 `node --test ui/*.test.mjs`

- [ ] **Step 2: 交叉编译 aarch64 目标**
  - `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc cargo build --target aarch64-unknown-linux-gnu --release`

- [ ] **Step 3: Commit**
  ```bash
  git commit -m "build: verify workspace and compile aarch64 release target"
  ```

---

### Task 6: 双实机实机运行、截图生成与视觉预期判定

**Files:**
- Create: `scripts/capture-tui-screenshot.py`
- Output: `docs/testing/screenshots/tui-nvme-live-rock5b.png`, `docs/testing/screenshots/tui-nvme-live-rock3a.png`

**Interfaces:**
- Consumes: target binary `/tmp/rsetup-next-test` on `192.168.27.88` and `192.168.27.34`
- Produces: Visual PNG screenshots converted from actual ANSI terminal output

- [ ] **Step 1: 编写终端 ANSI 转 PNG 截图工具**
  创建 `scripts/capture-tui-screenshot.py`（基于 Python PIL / ANSI 解析生成高保真终端图片）。

- [ ] **Step 2: 在 192.168.27.88（有 NVMe）执行实机捕获与截图**
  - 推送最新 aarch64 release 二进制至 `root@192.168.27.88:/tmp/rsetup-next-test`；
  - 通过 PTY 捕获 TUI 运行首帧屏幕内容；
  - 渲染生成 `docs/testing/screenshots/tui-nvme-live-rock5b.png`。

- [ ] **Step 3: 对 192.168.27.88 截图执行视觉判定**
  - 读取图片，验证：
    1. NVMe 存储视窗明确显示 `ZHITAI TiPlus7100 1TB`；
    2. 温度、寿命（0%）、备用空间（100%）指标正常显示；
    3. 边框对齐整齐，无重叠乱码。

- [ ] **Step 4: 在 192.168.27.34（无 NVMe）执行实机捕获与截图**
  - 推送二进制至 `root@192.168.27.34:/tmp/rsetup-next-test`；
  - 捕获首帧渲染内容，生成 `docs/testing/screenshots/tui-nvme-live-rock3a.png`。

- [ ] **Step 5: 对 192.168.27.34 截图执行视觉判定**
  - 读取图片，验证：
    1. 显示未检测到 NVMe 设备的友好提示；
    2. 界面自适应紧凑布局，无崩溃且其他 CPU/内存/服务视窗正常。

- [ ] **Step 6: 清理实机临时文件并提交文档与截图**
  - 删除两台机器上的临时测试程序；
  - 提交更新的测试截图及代码。

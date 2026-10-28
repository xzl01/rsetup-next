# 子规格 02：`crates/rsetup-app/src/tui.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 02 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未连接数据库、未提交。

撰写基线：分支 `dev-aghost`，工作树头提交 `c7ff640`；实测 `git status` 当时未提交的改动不含 [crates/rsetup-app/src/tui.rs](../../../../crates/rsetup-app/src/tui.rs)。行号为撰写时实测，实施时须按当时工作树重新核对（总览 §3 并行改动注记）。

## 1. 实测基线与锚点

对 [crates/rsetup-app/src/tui.rs](../../../../crates/rsetup-app/src/tui.rs)（5,514 行）实测：

- 实现区 1–2,586 行，测试区 2,587–5,514 行（单一 `#[cfg(test)] mod tests`，2,588 行起，无嵌套测试子模块），与总览 §3 快照「2,586 / 2,928」一致。
- 测试函数 **53 个**（全部 `#[test]`，按属性实测）。任务简报所称「91」与实测不符，本文一律以 53 为准；内联测试占文件过半、C3 为主要难度的总览判断不变。
- 测试全部离线：控制器一律 `Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)` 构造（2,681 行起多处），屏幕断言一律 `ratatui::backend::TestBackend`（2,686 行起），安装写路径由 `DryRunWriter`（实现）或 `RecordingBackend`（2,631 行，fixture）在内存计数；不触真实块设备、不发 controller 命令、不涉数据库。
- 对外调用面（C1 核对面，实测全仓 grep）：仅 [main.rs](../../../../crates/rsetup-app/src/main.rs) 第 3 行 `mod tui;`（bin 私有模块）与第 751 行 `Commands::Tui => tui::run(controller, locale)?` 两处。`pub fn run`（29–41 行）是唯一对外符号，无 `pub use` 再导出；`App` 及其字段不被 tui.rs 之外任何文件读取。
- tui.rs 反向引用 main.rs 的 crate 级项（拆分后原样保留 `crate::` 路径，共 7 项）：`format_bytes`（main.rs 1,103）、`system_install_error_code`（1,510–1,512，`pub(crate)`，本文件 1,013 行在用——依 07 号规格 §1.3 全量盘点补登）、`effective_uid`（1,609）、`install_needs_privilege`（1,620）、`InstallApplyError`（1,635）、`run_privileged_system_install`（1,857），以及测试引用的 `PRIVILEGED_HELPER`（3,099 行处使用）。main.rs 本体属 07 号子规格（`2026-10-26-large-file-split/07-app-main.md`，已创建并通过静态审查），本规格不动它。

### 1.1 实现区符号锚点（实测行号区间）

终端生命周期：`run` 29–41（内含 `RestoreTerminal` Drop 守卫，31–37）；`run_loop` 43–82（250 ms 轮询 + 按键分派，安装面板按键所有权在 58–64）。

主状态机：`struct App` 84–106（19 个字段，其中 `nvme_status`/`mmc_status` 现为 `pub(crate)`、`nvme_error`/`mmc_error` 现为 `pub`——均为私有结构体上的字段，实测 bin crate 内无模块外读取）；`impl App` 108–380（`new` 109、`next`/`previous` 163/213、`start_benchmark`/`poll_benchmark` 175/191、`install_key` 227、`refresh_storage` 243、`refresh` 274、`request_run` 289、`execute_selected` 314、`update_source_plan` 358、`close_source_picker` 375）。

安装面板域：`InstallFocus` 387–392；`InstallStatus` 396–413；`InstallPanel` 418–436 与 `impl` 438–630（`from_manager` 439、`open_panel`/`close` 458/463、`refresh_targets` 473、目标与焦点导航 487–513、`push`/`backspace`/`confirm` 515–544、`load_plan` 546、`attempt` 576、`acknowledgement_ready` 624）；`DryRunWriter` 636–672（`impl BlockDeviceBackend`：`write_chunk` 恒 `Ok(())` 655–662、`reread_partitions` 恒 `Unsupported` 664–671）；`system_install_manager` 677–689（synthetic/live 两级装配）。

安装呈现：`install_device_kind_label` 691–693；`install_target_notes` 695–715；`install_plan_lines` 718–835；`verification_text` 845–865（`Unsupported` 两义按 `observed` 区分）；`render_install_panel` 867–1,087；`render_install_footer` 1,089–1,119。

屏幕装配：`render` 1,121–1,145（3 行头 + Min(18) 体 + 3 行脚；61/39 双栏）；`render_header` 1,147–1,189；`render_mission` 1,191–1,373；`render_storage_summary` 1,375–1,505（局部 `enum Device` 1,407）；`render_actions` 2,323–2,418；`render_source_picker` 2,420–2,503；`benchmark_detail` 2,505–2,531；`render_footer` 2,533–2,568；`instrument` 2,570–2,575；`duration` 2,577–2,585。

布局与宽度度量：颜色常量 22–27；`storage_more_hints`/`_floor`/`_hint` 1,507–1,558；`fit_line_in_rows` 1,560–1,603；`storage_error_lines` 1,605–1,626；`MAX_WRAP_ROWS` 1,633、`RowFit` 1,636–1,645、`ROW_PROBE` 1,649（三者注释携带 mmc review 结论）；`painted_rows` 1,659–1,668；`wrapped_rows` 1,683–1,726（探针法，禁改）；`wrapped_rows_capped` 1,730–1,735；`nvme_device_lines` 1,740–1,933；`text_width` 1,935–1,954；`ellipsize` 1,956–1,979（grapheme 边界）；`join_identity` 1,981–1,995；`mmc_identity_line` 1,997–2,070；`type Segment`/`join_segments` 2,072–2,092；`mmc_health_candidates` 2,094–2,269；`fit_block` 2,271–2,292；`mmc_device_lines` 2,297–2,321。

### 1.2 测试区结构（53 个测试 + fixture 清单）

mod tests 内模块级 fixture（实测行号）：`InstallScratch`/`install_scratch`/`install_image_bytes` 2,594–2,629、`RecordingBackend` 2,631–2,663、`recording_panel` 2,665、`demo_app` 2,680、`render_app` 2,685、`select_target`/`type_into`/`written` 2,698–2,715、`finished_install`/`finished_install_with` 2,876–2,945、`rock3a_mmc_app` 3,468、`render_storage_rows` 3,484、`line_text` 3,507、`measured_rows` 3,627、`rock3a_warning_app` 3,650、`eol_text` 3,665、`render_mission_rows` 3,674、`flatten_screen` 3,731、`EnumerationFixture` 4,484、`storage_card_text` 4,511、`norm` 4,530、`oracle_rows` 5,004、`oracle_clusters` 5,026、`card_inner_rows` 5,068、`hint_wordings` 5,277、`no_space` 5,290、`only_content_row` 5,295。测试函数体内嵌的 `letters` 2,949、`squash` 3,009、`render_norm` 3,422（闭包）、`FakeStorage` 4,268 随函数走。

按使用位置实测划归（§5 细化）：仅 install 测试使用——`demo_app`、`render_app` 及 2,594–2,945 全部安装 fixture；仅 textfit 测试使用——`measured_rows`、`oracle_rows`、`oracle_clusters`、`card_inner_rows`（5,058 行处被 textfit 测试调用）；仅存储卡测试使用——`render_storage_rows`、`EnumerationFixture`、`storage_card_text`、`norm`、`hint_wordings`、`no_space`、`only_content_row`；跨 2+ 子模块共用——`line_text`（3,507 定义；mmc 直测 3,619/4,994 与度量测试 3,721、5,150/5,158 均用）、`rock3a_mmc_app`（storage 卡与 mmc 直测）、`rock3a_warning_app`（textfit/mission/storage 卡）、`eol_text`（mission/storage 卡）、`render_mission_rows`（mission/textfit/storage 卡）、`flatten_screen`（mission/storage 卡）。

## 2. 目标文件树（拟建，未创建）

```text
crates/rsetup-app/src/tui.rs  →  crates/rsetup-app/src/tui/
├── mod.rs                    薄门面：pub fn run + run_loop + 模块装配 + 全屏组装测试（3 个）
├── app.rs                    App 与 impl App（主状态机）
│   └── tests.rs              App 状态测试（3 个）
├── theme.rs                  6 个颜色常量 + instrument()
├── textfit.rs                RowFit/MAX_WRAP_ROWS/ROW_PROBE/painted_rows/wrapped_rows(_capped)/
│                             fit_line_in_rows/text_width/ellipsize/join_identity/fit_block
│   └── tests.rs              度量测试（7 个）+ oracle/measured/card_inner fixture
├── storage/
│   ├── mod.rs                render_storage_summary + enum Device + storage_more_hint* + storage_error_lines
│   ├── card_tests.rs         存储卡渲染与枚举屏幕测试（20 个，test-only 子模块）
│   ├── nvme.rs               nvme_device_lines
│   └── mmc.rs                mmc_identity_line/Segment/join_segments/mmc_health_candidates/mmc_device_lines
│                             （3 个直测内联其测试模块）
├── mission.rs                render_mission + duration
│   └── tests.rs              任务卡测试（6 个）
├── shell.rs                  render/render_header/render_actions/render_source_picker/
│                             benchmark_detail/render_footer
├── install/
│   ├── mod.rs                InstallFocus/InstallStatus/InstallPanel
│   ├── backend.rs            DryRunWriter + system_install_manager
│   ├── render.rs             install_* 文本构件 + verification_text + render_install_panel/footer
│   └── tests.rs              面板行为测试（11 个）+ 安装 fixture 群
└── fixtures.rs               #[cfg(test)] 专用共享 fixture（仅 §1.2 所列跨模块 6 项）
```

**树实施修订（2026-10-26）**：树中 `textfit.rs` 与 `mission.rs` 两行的 `tests.rs` 分文件注记按「实施修订：测试内联于域文件 `#[cfg(test)] mod tests`，mission 沿用批次 1 先例（提交 3507537/c9a9f1b）」理解，不再单列分文件行；其余 `tests.rs`/`card_tests.rs` 行原文不变，登记详见 §11。

入口决策（明确选择）：**保留 `tui` 名与 `run` 入口**，`tui.rs` 变为 `tui/mod.rs`（目录模块），[main.rs](../../../../crates/rsetup-app/src/main.rs) 的 `mod tui;` 与 `tui::run(...)` 调用路径逐字不变；不重命名为 `ui/`、`app_ui/` 等（无理由重命名，C1）。`run_loop` 放 `mod.rs` 而非 `app.rs`：`run_loop` 调 `shell::render`、渲染各模块又读 `App`，若 `run_loop` 在 `app.rs` 则 `app ↔ shell` 互引；放门面后依赖单向。

体量估算（含各自测试，实现按 §1.1 区间求和，测试按 §5 归属求和，约值）：除 `storage/card_tests.rs`（约 1,100）与 `install/mod.rs`（实测 944，实施修订）外全部 ≤900。`card_tests` 落 900–1,500 档的例外说明：其 20 个测试同测「storage_telemetry 卡」一个渲染域（宽度预算、hint 整行、枚举错误屏幕、错误行与设备数共存），共享 `render_storage_rows`/`EnumerationFixture`/`hint_wordings` 等 helper，再拆会在文件间重复 helper 或扩大跨文件可见性；若实施时实测超 1,500，后续再拆方案为把枚举屏幕测试（4,484–4,800 域）单独成 `storage/enumeration_tests.rs`（纯 test-only 子模块，不动实现）。`install/mod.rs` 落 900–1,500 档的例外说明（实施修订，提交 1530847）：11 个面板行为测试与安装 fixture 群沿用测试内联先例（批次 1/3）以 `#[cfg(test)] mod tests` 内联于 `mod.rs`——原分文件方案由 `install/tests.rs` 承载 11 测试＋fixture 群（§5 表、§7 批次 4）——沿用 `card_tests` 同型论证：同域测试共享 fixture，再拆会文件间重复 helper 或扩大跨文件可见性；实测 944 落 900–1,500 档且 <1,500。

## 3. 符号归属表

| 符号（§1.1 锚点） | 目标子模块 | 移动后可见性 |
| --- | --- | --- |
| `run` 29–41 | `tui/mod.rs` | `pub`（唯一对外符号，路径不变） |
| `run_loop` 43–82 | `tui/mod.rs` | 私有 |
| 颜色常量 22–27、`instrument` 2,570 | `theme.rs` | `pub(in crate::tui)` |
| `App` 84–106、`impl App` 108–380 | `app.rs` | 类型与 `new` `pub(in crate::tui)`；字段与方法见 §4.2 |
| `InstallFocus`/`InstallStatus`/`InstallPanel` 387–630 | `install/mod.rs` | `InstallFocus`/`InstallPanel` `pub(in crate::tui)`；`InstallStatus` 私有；字段与方法见 §4.2；本文件将 `backend` 声明为 `pub(in crate::tui) mod`（实施修订：`App::new` 跨树调 `system_install_manager`，与 §3 表 `system_install_manager` 目标可见性一致，提交 1530847；模块路径可达性见 §4.2） |
| `DryRunWriter` 636–672 | `install/backend.rs` | 私有 |
| `system_install_manager` 677–689 | `install/backend.rs` | `pub(in crate::tui)`（`app.rs::new` 调用） |
| `install_*` 文本构件 691–835、`verification_text` 845–865、`render_install_panel` 867–1,087、`render_install_footer` 1,089–1,119 | `install/render.rs` | `render_install_panel` `pub(in crate::tui)`（shell 1,134 调用），`render_install_footer` `pub(in crate::tui)`（实施修订：入口 `render` 现值 399 行直调、`shell.rs` 终态同需，提交 1530847），其余 `install_*` 文本构件与 `verification_text` 私有；`install/mod.rs` 将 `render` 声明为 `pub(in crate::tui) mod`（模块路径可达性见 §4.2） |
| `render` 1,121–1,145、`render_header` 1,147–1,189、`render_actions` 2,323–2,418、`render_source_picker` 2,420–2,503、`benchmark_detail` 2,505–2,531、`render_footer` 2,533–2,568 | `shell.rs` | `render` `pub(in crate::tui)`，其余私有 |
| `render_mission` 1,191–1,373、`duration` 2,577–2,585 | `mission.rs` | `render_mission` `pub(in crate::tui)`（shell 调用），`duration` 私有 |
| `render_storage_summary`+`enum Device` 1,375–1,505、`storage_more_hints`/`_floor`/`_hint` 1,507–1,558、`storage_error_lines` 1,605–1,626 | `storage/mod.rs` | `render_storage_summary` `pub(in crate::tui)`（`render_mission` 1,350 调用）、`storage_error_lines` `pub(in crate::tui)`（实施修订：mission 跨树直调，提交 1e8e034），其余私有；本文件将 `nvme`/`mmc` 声明为 `pub(in crate::tui) mod`（模块路径可达性见 §4.2） |
| `nvme_device_lines` 1,740–1,933 | `storage/nvme.rs` | `pub(in crate::tui)`（被 `render_mission` 1,220 直调；经 `storage/mod.rs` 的 `pub(in crate::tui) mod nvme` 可达，见 §4.2） |
| `mmc_identity_line` 1,997–2,070、`Segment`/`join_segments` 2,072–2,092、`mmc_health_candidates` 2,094–2,269、`mmc_device_lines` 2,297–2,321 | `storage/mmc.rs` | `mmc_identity_line`/`mmc_device_lines` `pub(in crate::tui)`（后者被 `render_mission` 1,232 直调与测试用），`Segment`/`join_segments`/`mmc_health_candidates` 私有（仅 mmc.rs 与其内联测试使用） |
| `RowFit`/`MAX_WRAP_ROWS`/`ROW_PROBE`/`painted_rows`/`wrapped_rows`/`wrapped_rows_capped`/`fit_line_in_rows`/`text_width`/`ellipsize`/`join_identity`/`fit_block` | `textfit.rs` | `wrapped_rows`（返回 `RowFit`，二者同批）`wrapped_rows_capped`/`fit_line_in_rows`/`ellipsize`/`fit_block`/`join_identity` `pub(in crate::tui)`；`MAX_WRAP_ROWS` `pub(in crate::tui)`（`storage/mod.rs` 的 `storage_more_hint_floor` 1,524 引用）；`text_width` `pub(in crate::tui)`（`storage/card_tests.rs` 5,340 引用；`mmc_identity_line` 实现 2,022/2,042 亦用）；`painted_rows`/`ROW_PROBE` 私有（仅 textfit 与其测试使用） |

无删除、无改名、无签名变化；跨文件引用一律 `use crate::tui::...`。

## 4. 依赖方向与可见性

### 4.1 子模块引用方向（无环证明）

自底向上单向：`theme`、`textfit` 为叶子（不引用 tui 树内任何其他子模块）；`storage/nvme`、`storage/mmc` → `textfit`+`theme`+`app`（只读 `&App`）；`storage/mod` → `storage/nvme|mmc`+`textfit`+`theme`+`app`；`mission` → `storage/mod|nvme|mmc`（`render_mission` 1,220/1,232/1,243/1,350 直调 `nvme_device_lines`/`mmc_device_lines`/`storage_error_lines`/`render_storage_summary`；`storage_error_lines` 系实施修订补登、1,243 为按撰写时点登记的现值行号）+`textfit`+`theme`+`app`；`shell` → `mission`+`install/render`+`theme`+`app`；`install/mod` → `install/backend`；`install/render` → `theme`+`app`；`app` → `install/mod`+`install/backend`（`App::new` 装配面板）；`tui/mod` → `app`+`shell`。test-only 的 `fixtures` 终态依赖为 `app`+`mission`（`rock3a_mmc_app`/`rock3a_warning_app` 经 `App::new` 构造并改字段、`render_mission_rows` 调 `render_mission`），与被挂接的 `tui/mod` 同向、不成环。每条边都指向更底层，不存在回边；`run_loop` 置于 `mod.rs` 正是为消除 `app ↔ shell` 的唯一潜在回边。批次迁移期的 `fixtures` 临时依赖（批次 1–2 期父 `tui.rs` 未迁的 `render_mission`）必须随该符号迁入 `mission` 的批次同步回改为 `crate::tui::mission::render_mission`，不得残留、不得新增其他临时引用（批次 1 已登记的两条临时父引用除外，见 §7）。

### 4.2 最窄可见性清单（C1 口径，全部为「新增内部子模块间暴露」）

- 本文件在 bin crate 内是私有模块，**对外可见面为零**（仅 `run`，路径不变）：不新增任何 `pub` 或 `pub(crate)`；跨子模块暴露统一用 `pub(in crate::tui)`，理由均为「兄弟子模块（或其测试子模块）需引用」，逐项见 §3 表。
- 字段级：`App` 的 19 个字段中，被兄弟模块读的（`locale`、`snapshot`、`install`、`actions`、`selected`、`confirm_pending`、`last_run`、`source_*`、`notice`、`nvme_status`、`mmc_status`、`nvme_error`、`mmc_error`、`benchmarks`、`benchmark_rx`）设 `pub(in crate::tui)`；仅 `impl App` 自身读的（`controller`）保持私有。现 `pub(crate) nvme_status`/`mmc_status`、`pub nvme_error`/`mmc_error` 随拆分**收窄**为 `pub(in crate::tui)`：私有结构体字段、实测 bin 内无外部读取方，收窄是纯声明变化（非 C1 意义上的对外契约变化）。
- 字段级（`InstallPanel`）：仅跨出 install 子树的 2 个字段设 `pub(in crate::tui)`——`open`（`run_loop` 58 行读、shell 的 `render` 1,133 行读）与 `focus`（`App::install_key` 228 行读）；其余 10 个字段 `manager`、`targets`、`selected`、`image_path`、`plan`、`typed`、`status`、`result`、`live_write`、`privileged` 全部保持字段私有：`install/render.rs`（867–1,119 读 `targets`/`selected`/`image_path`/`focus`/`plan`/`typed`/`status`/`result`/`live_write`）、`install/backend.rs`、`install/tests.rs`（2,728–3,263 读 `privileged`（3,108 行写）及其余字段）都是 install 的子代模块，天然可见父模块私有项；`InstallStatus` 类型同样保持私有。
- 方法级（跨树调用的 `impl` 方法必须 `pub(in crate::tui)`，`impl` 方法默认私有、漏标即编译失败）：`App` 上被 `run_loop`（`tui/mod.rs`）调用的 `new`、`poll_benchmark`、`install_key`、`next`、`previous`、`refresh`、`start_benchmark`、`request_run`、`execute_selected`、`close_source_picker`（实测 43–82 行；`refresh` 另被存储卡测试 4,599–4,772 多处调用、`install_key` 另被 `install/tests.rs` 2,837–2,867 调用）；`install/mod.rs` 上被 `App` 调用的 `from_manager`（`app.rs::new` 125 行）与 `open_panel`/`close`（`run_loop` 67/60 行），及被 `App::install_key` 转调的 `focus_next`、`next_target`、`previous_target`、`refresh_targets`、`backspace`、`confirm`、`push`（实测 228–238 行）。仅树内调用的 `refresh_storage`、`update_source_plan`、`load_plan`、`attempt`、`acknowledgement_ready`（967 行由 `install/render.rs` 读）保持私有。
- 模块路径级（C1 最窄可行；仅「条目可见」不够，路径本身必须对兄弟模块可达）：`storage/mod.rs` 将 `nvme`/`mmc` 声明为 `pub(in crate::tui) mod`（`render_mission` 1,220/1,232 直调其实体，而 `storage` 内的私有 `mod` 仅对 storage 子树可见，mission 不可达）；`install/mod.rs` 将 `render` 声明为 `pub(in crate::tui) mod`（shell 的 `render` 1,134 行调 `render_install_panel`），`backend` 声明为 `pub(in crate::tui) mod`（实施修订：`App::new` 跨树调 `system_install_manager`，与 §3 表 `system_install_manager` 目标可见性一致，提交 1530847）。默认即采用上述 `pub(in crate::tui) mod` 声明、与 §3 表一致；在各自 `mod.rs` 做选择性 re-export（如 `pub(in crate::tui) use nvme::nvme_device_lines;`）只是说明性的等价替代方案，默认不采用。除此之外，`theme`/`textfit`/`app`/`mission`/`storage`/`install`/`shell` 由 `tui/mod.rs` 声明为私有 `mod`——私有项对定义模块的全部后代可见，兄弟模块路径本已可达，不升级。
- test-only：`tui/fixtures.rs` 经 `mod.rs` 以 `#[cfg(test)] pub(in crate::tui) mod fixtures;` 挂载；各 `tests.rs`/内联测试模块对同文件私有项直接访问（与现状一致），跨文件访问被测项依赖 §3/§4.2 的 `pub(in crate::tui)`，**不引入** `#[cfg(test)] pub` 逃逸，也不新建 `tests/` 集成测试。

## 5. 测试与 fixture 迁移（C3）

53 个测试按被测职责逐组迁移，断言与被测行为同位、断言文本零改写：

| 测试（行号锚点） | 数量 | 目标测试位 |
| --- | --- | --- |
| 面板行为：2,717、2,829、2,946、3,005、3,059、3,073、3,098、3,140、3,172、3,215、3,247 | 11 | `install/tests.rs`（连同 2,594–2,945 安装 fixture 群整体随行） |
| App 状态：3,305、3,315、4,259 | 3 | `app/tests.rs` |
| 全屏组装：3,908、3,950、4,020 | 3 | `tui/mod.rs` 的 `#[cfg(test)] mod tests`（测门面 `render` 组装出的全屏终态——以 `TestBackend` 绘制 `render(frame, &mut app)` 并断言缓冲区内容；`run_loop` 本身无直接测试） |
| 度量：3,695、4,905、5,034、5,133、5,164、5,191、5,234 | 7 | `textfit/tests.rs`（`measured_rows`/`oracle_rows`/`oracle_clusters`/`card_inner_rows` 内联于此；3,695/5,133 所用 `line_text` 取 `fixtures.rs`） |
| mmc 直测：3,583、3,609、4,976 | 3 | `storage/mmc.rs` 内联测试模块（3,609/4,976 所用 `line_text` 取 `fixtures.rs`） |
| 存储卡渲染与枚举屏幕：3,327、3,359、3,421、3,515、3,538、3,555、4,035、4,102、4,180、4,215、4,413、4,536、4,565、4,804、4,845、5,085、5,310、5,354、5,397、5,455 | 20 | `storage/card_tests.rs`（`render_storage_rows`/`EnumerationFixture`/`storage_card_text`/`norm`/`hint_wordings`/`no_space`/`only_content_row` 随行） |
| 任务卡：3,268、3,741、3,782、3,808、3,848、3,884 | 6 | `mission/tests.rs`（实施修订：测试内联于 `mission.rs` `#[cfg(test)] mod tests`，沿用批次 1 先例，提交 c9a9f1b） |

- 跨模块共用 fixture 恰 6 项：`rock3a_mmc_app`、`rock3a_warning_app`、`eol_text`、`render_mission_rows`、`flatten_screen`、`line_text`（3,507，`Line`→`String` 纯函数、无 tui 树内依赖） → `tui/fixtures.rs`（test-only，约 115 行）；单模块 fixture 一律内联在该模块测试处，不集中堆放。6 项的迁入批次由「首个用到它们的测试所在批次」决定，见 §7 批次 1/2/3；迁入前不得让测试引用不到、迁入后不得留副本。
- 「面板呈现走屏幕断言、按键走状态断言」的混合测试（如 2,717、3,140）整体留在 `install/tests.rs`，不拆断言——面板状态机与其呈现是同一职责。
- 禁止事项：不把 2,587–5,514 整块搬进单一 `tests.rs`（总览 C3 明文）；每个测试文件与实现职责一一对应，体量均 ≤900 或已给 §2 例外说明。

## 6. 语义保持清单（C2，逐条不得变）

- 渲染文字与布局：全部文案键、颜色常量值、`Layout` 约束（3/Min(18)/3、61/39 列）、卡片行预算算法逐字移动；`wrapped_rows` 的探针法（`ROW_PROBE`、`cap+2`、`MAX_WRAP_ROWS=512`）与 `ellipsize` 的 grapheme 边界是所有宽度测试的唯一裁决者，**只移动、不得顺手优化**。
- 事件循环与终端恢复：`run` 的 `RestoreTerminal` Drop 守卫先于 `try_init` 安装（29–41 行注释语义）；`run_loop` 按键顺序——安装面板打开时的按键所有权（Esc 关面板、其余全部转 `install_key`）、主屏 q/i/Esc/j/k/r/b/Enter/y/n 判定次序、`KeyEventKind::Press` 过滤、250 ms `event::poll`——逐字不变。
- 安装确认与门禁：`confirm → load_plan/attempt` 链、typed 名字原样交 core 裁决（`acknowledgement_ready` 仅 affordance）、privileged 时经 `crate::run_privileged_system_install` 走 helper、`InstallStatus::Planned/Canceled/Helper` 的区分语义、`close` 清空 plan/typed 语义不变；`DryRunWriter::write_chunk` 恒 `Ok(())` 且 `reread_partitions` 恒 `Unsupported`（未落盘的可观测契约）不变；`system_install_manager` 的 synthetic/live 两级包装顺序不变。
- 与 main.rs 的接缝：对 `crate::format_bytes`/`system_install_error_code`/`effective_uid`/`install_needs_privilege`/`InstallApplyError`/`run_privileged_system_install` 的 `crate::` 引用原样保留；`main.rs:751` 调用路径与签名不变；不改 07 号范围的任何 main.rs 内容。
- 测试：53 个测试断言零删改；测试仍全部 Demo/DryRun + TestBackend，实施后不得新增真实设备或网络访问。

## 7. 迁移批次与回滚（C8-7）

按依赖自底向上分 5 个批次，每批一个提交（提交信息用简体中文，见 [AGENTS.md](../../../../AGENTS.md)）、批批可独立编译：

1. **theme + textfit + fixtures.rs 首批**：建目录树，移叶子实现与 7 个度量测试；`tui.rs` 暂时 `mod`+`use` 转发。同步建 `fixtures.rs` 并入 `rock3a_mmc_app`、`rock3a_warning_app`、`render_mission_rows`、`line_text` 4 项——度量测试 3,695 与 5,034 已用到后两者（3,703/5,055/5,056 行），而 `rock3a_warning_app` 依赖 `rock3a_mmc_app`（3,651 行）；度量测试 3,695 与 5,133 同批已用 `line_text`（3,721/5,150/5,158 行），它是纯函数、无新增依赖，迁入后原处不留副本（mmc 直测 3,619/4,994 在批次 2 前经父 `tui.rs` 可见的 `fixtures` 引用它）；此时 `render_mission_rows` 暂用父 `tui.rs` 尚未迁移的 `render_mission`（fixtures 是 tui 子代模块，读父私有项合法），批次 3 回改；度量测试 3,695 亦临时引用父 `tui.rs` 的 `mmc_device_lines`（`textfit` 测试模块内 `use crate::tui::mmc_device_lines`，同型临时父引用，提交 3507537 实施登记），批次 2 迁入 `storage/mmc.rs` 时随域回改为 `crate::tui::storage::mmc::mmc_device_lines`。回滚点：revert 即恢复原单文件。
2. **storage 树**：`storage/mod|nvme|mmc` + `card_tests.rs` + mmc 内联测试；`fixtures.rs` 本批补入 `eol_text` 与 `flatten_screen`——存储卡测试 4,845 与 5,085 已在用 `eol_text`（4,858/5,122 行）与 `flatten_screen`（4,857 行），不能等到批次 3；本批随 `mmc_device_lines` 迁入 `storage/mmc.rs` 回改 `textfit` 对其的临时父引用（批次 1 登记）。
3. **mission**：实现 + `tests.rs`（6 个）；本批把 `fixtures.rs` 中 `render_mission_rows` 对父 `tui.rs` 的临时引用回改为 `crate::tui::mission::render_mission`（其 `pub(in crate::tui)` 就位），临时引用不得跨批残留。
4. **install 树**：`mod/backend/render` + `tests.rs`（11 个）。测试载体沿用内联先例（批次 1/3 实施修订），`tests.rs` 仅为原分文件方案的表述。`install/render` 依赖 `App`，故晚于 storage/mission 亦可提前，置于此处仅为批内体量均衡。
5. **app + shell + 门面收尾**：`App` 移 `app.rs`、渲染装配移 `shell.rs`、`run`/`run_loop`/全屏测试留在 `tui/mod.rs`；删除临时转发。门面转换必须原子：`tui.rs` 的删除与 `tui/mod.rs` 的创建在同一提交内完成（`git mv` 后一次性 `git add -A` 提交），同一提交里二者不得并存——否则模块解析出现两个候选（`tui.rs` 与 `tui/mod.rs`），直接 E0761 歧义编译失败。

每批完成即跑 §8 命令并如实报告；任一批失败或需越界改动时 `git revert` 该批提交，前序批次不受影响（纯移动、无行为开关）。禁止批次间夹带重构或格式化噪声（`make fmt-check` 口径）。

## 8. 验证命令与前提（总览 §6.2 基线；本轮未执行任何一条）

| 命令 | 前提 |
| --- | --- |
| `make fmt-check` | 无（F1：勿用 `cargo fmt --all` 判定，见 [AGENTS.md](../../../../AGENTS.md)） |
| `cargo clippy -p rsetup-next --all-targets -- -D warnings` | F2/F4：编译型检查；包名实测为 `rsetup-next`（[Cargo.toml](../../../../crates/rsetup-app/Cargo.toml) `name` 字段与 [Makefile](../../../../Makefile) `FMT_PACKAGES` 一致；总览 §6.2 表中包名已更正为 `rsetup-next`（原 `-p rsetup-app` 为路径名笔误），命令以实测包名为准）。该包仅依赖 rsetup-core，**无需** web dist（`apps/controller-web/dist` 是 rsetup-controller 的 build.rs 需求）；干净检出需网络拉取依赖（F3 同理） |
| `cargo test -p rsetup-next --locked` | 同上；`tui` 模块的 53 个测试全离线（Demo/DryRun + TestBackend），不写设备、不发 controller 命令、无数据库；该包另有其他测试 target，计数口径见下方拆分校验 |
| `make controller-test-offline` | 全量回归出口（F2/F3），可选在最终批后执行 |

拆分校验（只读）：**53 只限 `tui` 模块**——`cargo test -p rsetup-next` 还编译并统计 `main.rs` 文件内测试与 `tests/` 集成 target，包级测试总数不等于 53，不得以包总数冒充。守恒以测试叶子名清单比对：拆分前后各跑一次 `cargo test -p rsetup-next --locked -- --list`（`--locked` 是 cargo 参数、置于 `--` 之前；`--list` 是 libtest 参数、置于其后），取 `tui` 模块条目的叶子名排序存档比对：

```bash
cargo test -p rsetup-next --locked -- --list 2>/dev/null \
  | sed 's/: test$//' | grep -E '^tui::' | awk -F'::' '{print $NF}' | sort
```

拆分前后各存一份（如 `before.txt`/`after.txt`），`comm -3 before.txt after.txt` 应无输出——53 个叶子名（及总数 53）一个不多、一个不少；全限定路径前缀会随归属变化（`tui::tests::…` → `tui::install::tests::…` 等），只允许路径变、名字不变。逐测试再以「移动 diff」核对断言文本零差异；`rg -n "mod tests" crates/rsetup-app/src/tui.rs` 风格的整块测试残留应为零。真库/`--ignored` 轨道（F6）与本模块无关，不登记。

## 9. 模块专属风险与 DoD

风险：

1. `textfit` 全部宽度/行数度量是每张卡片各宽度布局断言的唯一基线，任何「顺手」改动会在未知终端宽度下变形而不被本模块测试捕获——迁移只动位置不动语义。
2. 跨文件暴露面集中在方法、模块路径与少量字段（§4.2）：字段跨子树仅 `App` 若干与 `InstallPanel` 的 `open`/`focus`，其余靠 `run_loop`/`install_key` 所调方法与 `nvme`/`mmc`/`render` 的 `mod` 路径声明，漏标只导致编译失败（良性失败模式），但须对照 §4.2 三张清单（字段级/方法级/模块路径级）逐项核对，避免图省事整体 `pub(crate)`。
3. `run_loop` 按键路由顺序是交互协议；`RestoreTerminal` 守卫不得移出 `run`（含初始化失败路径的终端恢复语义）。
4. `install/render.rs` 读取面板 12 字段中的多数且呈现有优先级（status/result 先后）；按整函数移动，不逐句重组。
5. 工作树并行改动（总览 §3）：实施前重新核对锚点；本规格行号基于 `c7ff640` 工作树。
6. 与 main.rs 的同型 DryRunWriter 包装（634–635 行注释登记的 follow-up）不在本规格合并——那属 07 号范围。

DoD（专属部分，通用部分按总览 §6.3）：C1 自检=对外仅 `tui::run` 且路径/签名逐字不变（§1 登记的 main.rs 两处调用点核对）；C2 自检=§6 清单逐项 diff 为零；C3 自检=53 测试按 §5 表就位、fixture 保持 `#[cfg(test)]`、无新增集成测试；C4 自检=除已论证的 `storage/card_tests.rs` 与 `install/mod.rs`（实测 944 行，11 测试沿用内联先例的派生例外，提交 1530847，例外说明见 §2）外无超 900 行目标文件，900–1,500 档例外说明在文；C5 不适用（非 ui/app.js）；C6 自检=未触碰 `externals/`、`tests/*.rs`；C7 自检=本文与实施 diff 无凭据；§8 命令按前提执行并如实报告（含未执行项）；§7 批次全部可 revert。

## 10. 与简报/总览的出入（实测为准）

1. 内联测试实测 **53** 个，非简报所称 91；总拆分难度结论不变。
2. 总览 §6.2 包名已更正为 `rsetup-next`（原专项命令 `cargo test -p rsetup-app --locked` 为路径名笔误，目录名 ≠ 包名），本文 §8 采用实测包名，命令语义与总览基线一致。
3. `run`/`run_loop` 同留门面、`run_loop` 不入 `app.rs`，是 §2 所述的无环依赖选择，非对总览的偏离（总览不规定内部树形）。

## 11. 审查修订记录（2026-10-25）

以下为本文独立审查后的定点修订（6 条 Important、4 条局部 Minor），逐条**已修订、待复核**；未编译、未测试、未改架构与 §5 测试归属，不声称任何验证通过。

1. I1 §3 表：`MAX_WRAP_ROWS` 由私有改 `pub(in crate::tui)`——`storage/mod.rs` 的 `storage_more_hint_floor`（1,524 行）引用该常量（1,633 行）。
2. I2 §3 表：`text_width` 由私有改 `pub(in crate::tui)`（`storage/card_tests.rs` 5,340 行引用）；`painted_rows`/`ROW_PROBE`/`Segment`/`join_segments`/`mmc_health_candidates` 并列写明保持私有（仅所属子树与其测试使用）。
3. I3 §7 批次 1：`fixtures.rs` 提前到批次 1 建立并入 `rock3a_mmc_app`、`rock3a_warning_app`、`render_mission_rows`（度量测试 3,695/5,034 需要后两者、其依赖链含前者）；`render_mission_rows` 暂用父 `tui.rs` 未迁的 `render_mission`，use 路径于批次 3 回改，§4.1/§5 同步登记。
4. I4 §7 批次 2：`eol_text` 与 `flatten_screen` 移入批次 2 迁入——存储卡测试 4,845/5,085 已在用，不能等批次 3。
5. I5 §4.2 新增方法级与模块路径级清单：`App` 被 `run_loop` 调用的 `new`/`poll_benchmark`/`install_key`/`next`/`previous`/`refresh`/`start_benchmark`/`request_run`/`execute_selected`/`close_source_picker`，`InstallPanel` 被 `run_loop` 调用的 `open_panel`/`close`、被 `App::new` 调用的 `from_manager`、被 `App::install_key` 转调的 `focus_next`/`next_target`/`previous_target`/`refresh_targets`/`backspace`/`confirm`/`push` 设 `pub(in crate::tui)`，其余保持私有；`storage/mod.rs` 对 `nvme`/`mmc`、`install/mod.rs` 对 `render` 声明 `pub(in crate::tui) mod`（备选：选择性 re-export，默认不采用，见 §4.2），§3 表行与 §4.1 依赖边同步。
6. I6 §4.2：`InstallPanel` 字段收窄为仅 `open`/`focus` `pub(in crate::tui)`；`targets`/`selected`/`image_path`/`plan`/`typed`/`status`/`result`/`live_write`/`privileged`（连同 `manager`）仅 install 子树使用、保持私有，`InstallStatus` 保持私有；§9 风险 2 同步。
7. M1 §7 批次 5：`tui.rs` 删除与 `tui/mod.rs` 创建必须同一提交原子完成，禁止并存（避免 E0761 模块候选歧义）。
8. M2 §4.1：登记 `fixtures`（test-only）终态依赖 = `app`+`mission`、与被挂接的 `tui/mod` 同向不成环；批次期临时依赖随对应符号迁移批次同步回改、不得残留。
9. M3 §8：53 计数限定 `tui` 模块（`cargo test -p rsetup-next` 还含 `main.rs` 文件内测试与 `tests/` 集成 target，不用包总数冒充），给出 `cargo test -p rsetup-next --locked -- --list`（`--locked` 在 `--` 前、libtest `--list` 在其后）取叶子名 `sort` 后 `comm -3` 前后比对的命令，并同步 `cargo test` 行前提。
10. M4 §5 表：全屏组装 3 测的说明由「`run_loop` 所绘终态」改为「以 `TestBackend` 绘制门面 `render` 得到的全屏终态」，并注明 `run_loop` 无直接测试。

另附带修正：DoD 中「§1.3 两处调用点核对」为笔误，改指 §1 登记的 main.rs 两处调用点；标题层级（≤3 层）、行尾空白与相对链接已复查。

第二轮定点修订（2026-10-25，上述 10 项审查关闭后）：2 项新问题 + 3 条非阻断小错，逐条**已修订、待复核**；未编译、未测试，§5 测试 53 项归属不变。

1. N1 §1.2/§2 树/§5/§7：`line_text`（3,507 定义）误归「仅 storage/mmc 使用」——实测 mmc 直测（3,619/4,994）与度量测试 3,695/5,133（3,721/5,150/5,158）均用，改列共享 fixture 第 6 项归 `tui/fixtures.rs`（共享清单 5→6 项），删除「`line_text` 随行 mmc」表述；§7 批次 1 清单补入 `line_text`（首批度量测试即需），纯函数无新增依赖、不复制副本。
2. N2 §4.2 模块路径级：明确默认采用 `pub(in crate::tui) mod` 声明（与 §3 表一致），选择性 re-export 仅为说明性等价替代、默认不采用；删「二选一在实施时定稿」与上条第 5 项记录的「实施定稿」未决措辞。
3. 小错修正：§1.2 函数体内嵌清单补 `render_norm`（3,422 闭包，随测试走）；§4.2 方法级 `open_panel`/`close` 的 `run_loop` 行号次序改为 67/60；§3 表 `text_width` 可见性理由补 `mmc_identity_line` 实现 2,022/2,042 亦用（`storage/card_tests.rs` 5,340 之外）。

终审后清扫（已修订，待复核，不称 PASS）：§8 前提列与 §10 第 2 条「总览 §6.2 `-p rsetup-app` 笔误／不改总览」时态句改为「总览已更正为 `rsetup-next`」；§1 跨模块消费清单 6→7 项——补登 `system_install_error_code`（本文件 1,013 行在用、main.rs 1,510–1,512 定义 `pub(crate)`，依 07 号规格 §1.3 全量盘点补登），§6「与 main.rs 的接缝」清单同步加入该符号；其余不动，未提交、未编译、未测试、未读 `secret/`。

终态更新（2026-10-26，已修订）：§1 中 07 号子规格的时态短语「未创建」改为「已创建并通过静态审查」（依总览 §4 登记表 07 行「已通过静态审查」口径），括注其余内容不变；其余不动，未提交、未编译、未测试、未读 `secret/`。

批次 1 实施登记（已修订，2026-10-26，提交 3507537；`mmc_device_lines` 临时父引用批次 2 回改）：批次 1 实施实测发现度量测试 3,695 除已登记的 `render_mission` 临时引用外，还直接调用本批未迁的 `mmc_device_lines`（storage/mmc 域，批次 2 迁入），已在其测试模块（`textfit.rs`）加 `use crate::tui::mmc_device_lines;` 临时父引用，属同型派生、规格原仅登记前者——§7 批次 1 补登该条并写明批次 2 迁入 `storage/mmc.rs` 时随域回改为 `crate::tui::storage::mmc::mmc_device_lines`，§7 批次 2 补记该回改，§4.1 补注批次 1 已登记的两条临时父引用除外；其余不动，未提交、未编译、未测试、未读 `secret/`。

批次 2 实施登记（已修订，2026-10-26，提交 1e8e034；`storage_error_lines` 提 `pub(in crate::tui)`，mission 终态仍需）：批次 2 实施发现 `storage_error_lines` 被入口 `render_mission`（`tui.rs` 1,243 现值）直调，私有不可编译，已按 `pub(in crate::tui)` 实施；审查判定该提升编译必需且终态合理——mission 迁入后仍需跨树调用，与 `render_storage_summary`/`nvme`/`mmc` 同型——§3 表该符号原列「其余私有」属登记疏漏，已同步修订 §3 表 `storage/mod.rs` 行与 §4.1 mission→storage 依赖边（补登 1,243 直调）。附注：`card_tests` 7 处 rustfmt 必然重排已审查确认。其余不动，未提交、未编译、未测试、未读 `secret/`。

批次 3 实施登记（已修订，2026-10-26，提交 c9a9f1b；mission 6 测试内联于 `mission.rs`，§2/§5 分文件表述同步修订；另入口 `render_storage_summary` use 下沉 `mod tests` 属 clippy 必需）：6 个任务卡测试以 `#[cfg(test)] mod tests` 内联于 `mission.rs`（沿用批次 1 textfit 先例，审查判定 C3 实质——断言同位/零改写/职责对应/禁整块搬——全部满足，体量 ≤900），未创建 `mission/tests.rs` 分文件；§2 树后总注与 §5 表 mission 行短注据此同步。其余不动，未提交、未编译、未测试、未读 `secret/`。

批次 4 实施登记（已修订，2026-10-26，提交 1530847；`mod backend` 与 `render_install_footer` 可见性、`install/mod.rs` 944 行 C4 例外）：批次 4（install 树）实施登记三处派生事项，审查判定均合规、不改代码——① `install/mod.rs` 将 `backend` 声明为 `pub(in crate::tui) mod`：`App::new` 跨树调 `system_install_manager`（私有 `mod` 仅 install 子树可见），与 §3 表 `system_install_manager` 行目标可见性一致，已同步 §3 表 `install/mod.rs` 行与 §4.2 模块路径级；② `render_install_footer` 设 `pub(in crate::tui)`：入口 `render` 现值 399 行直调、`shell.rs` 终态同需，§3 表 install/render 行原「其余私有」相应收窄为「其余 `install_*` 文本构件与 `verification_text` 私有」；③ `install/mod.rs` 实测 944 行入 C4 例外：11 个面板行为测试与安装 fixture 群沿用测试内联先例（批次 1/3）内联于域文件，未创建 `install/tests.rs` 分文件（§7 批次 4 已登记载体沿用），沿用 `card_tests` 同型论证（同域测试共享 fixture、再拆将文件间重复 helper 或扩大跨文件可见性），落 900–1,500 档且 <1,500，§2 体量估算行与 §9 DoD C4 据此同步。其余不动，未提交、未编译、未测试、未读 `secret/`。

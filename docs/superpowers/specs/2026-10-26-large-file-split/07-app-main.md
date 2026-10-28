# 子规格 07：`crates/rsetup-app/src/main.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 07 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码、未编译、未运行测试、未连接数据库、未提交、未执行任何 CLI/设备命令。

撰写基线：当前工作树（会话内 `wc -l`/`grep` 实测）。实测 [main.rs](../../../../crates/rsetup-app/src/main.rs) 共 3,241 行，与总览 §3 快照「2,124 / 1,117」一致：实现区 1–2,123、测试区 2,125–3,241（两个文件内测试模块 `tests`（2,125 起）与 `decoding_tests`（3,228 起），实现区无其他 `#[cfg(test)]`）。实施时须按当时工作树重新核对锚点（总览 §3 并行改动注记）。

crate 与包事实（实测）：目录 `crates/rsetup-app` 的真实包名为 `rsetup-next`（[Cargo.toml](../../../../crates/rsetup-app/Cargo.toml) `name` 字段，与 [Makefile](../../../../Makefile) `FMT_PACKAGES` 一致）；bin crate 有两个入口——`rsetup-next`（`src/main.rs`，crate 根）与 `rsetup-next-helper`（`src/helper.rs`，独立 crate 根）。main.rs 声明 `mod i18n; mod server; mod tui;`（1–3 行）；`helper.rs` 有自己的 `main()`（helper.rs 74 行）与自带 `geteuid` 包装（339 行），**全文件零 `crate::` 引用**，与 main.rs 无符号耦合（实测）。

## 1. 实测基线与锚点

### 1.1 实现区结构（行号实测）

CLI 解析（clap derive，25–411）：`Cli` 25–52（全局 flag：`--demo`、`--live-execution`、`--lang`（默认 `auto`）；name/version/about 30 行）；子命令类型 `Commands` 54–101（含 `Serve` 的 `--listen` 默认 `127.0.0.1:8788`）；`SystemCommands` 103–131；`SourceCommands` 133–165；`HardwareCommands` 167–217；`OverlayCommands` 219–247；`SpiFlashCommands` 249–280（`operation` 以 `value_parser = ["install","erase"]` 枚举）；`VideoCommands` 282–293；`ThermalCommands` 295–313；`FanCurveCommands` 315–376（`--point` 用 `value_parser = parse_fan_curve_point` 328/343；`poll_ms` 默认 2000、`hysteresis_c` 默认 2.0；`Daemon`/`FailSafe` 为 `hide = true` 371/374）；`LedCommands` 378–411（`--mode` 默认 `solid`、RGB 默认值 396–405）。

配置与分发（413–757）：`main()` 413–757——tracing 初始化 415–421（默认 filter `rsetup_next=info`）；`Cli::parse` 与 locale 解析 423–424；`live_linux_only` 前置 bail 425–427；`ProbeMode`/`ExecutionPolicy`（`from_environment`）/`Controller` 装配 428–438；`InstallOptions` 装配 440–443（`synthetic: cli.demo || !cfg!(target_os = "linux")`）；`match cli.command.unwrap_or_else(default_command)` 444–755——`Status`/`Actions` 445–446、`Run` 分支体 447–478、`Sources`（benchmark 循环 480–500、status/plan/apply 501–523）、`Hardware`（overlays/gpio/spiflash/leds/video/thermal(含 fan-curve Daemon 716、FailSafe 717–723)/nvme/mmc/storage，525–750）、`Tui => tui::run` 751、`Serve => server::serve(...).await` 752、`Doctor` 753、`System => run_system_command` 754。benchmark（源测速）在 main.rs 内只有 480–500 的分发与打印，实现全部在 `rsetup_core`，不为其单设模块。

helper 函数（759–1,495）：`parse_fan_curve_point` 759–773；`fan_curve_request` 775–792；`run_fan_curve_daemon` 794–840（含 `#[cfg(unix)]` 双 select 分支 797–836）；`force_fan_curve_failsafe` 842–851；`print_source_status` 853–881；`print_source_plan` 883–913；`print_source_apply` 915–935；`default_command` 937–943（stdin/stdout 双终端判定）；`print_status` 945–1,000；`print_actions` 1,002–1,034；`print_doctor` 1,036–1,101；`format_bytes`（`pub(crate)`）1,103–1,120；`format_nvme_status` 1,122–1,281；`format_mmc_status` 1,283–1,427；`format_storage_status` 1,429–1,433；`print_json_or_debug` 1,435–1,445；`decode_base64` 1,447–1,495。

系统安装域（1,497–2,123）：`PRIVILEGED_HELPER`（`pub(crate)` 常量，1,504）、`PKEXEC` 1,505、`system_install_error_code`（`pub(crate)`）1,510–1,512、`InstallOptions` 1,516–1,523、`system_install_backend` 1,525–1,531、`system_install_manager` 1,536–1,544、`DryRunBackend`（写计数恒成功、`reread_partitions` 恒 `Unsupported`、`is_dry_run` 恒 true）1,548–1,593、`system_install_request` 1,595–1,601、`InstallExecer` 1,604–1,607、`effective_uid`（`pub(crate)`）1,609–1,611、`install_needs_privilege`（`pub(crate)`）1,620–1,626、`InstallApplyError`（`pub(crate)`，三变体 + `code()`/`message()`）1,635–1,688、`run_system_command` 1,690–1,774、`run_system_install`（plan→预览→dry-run 通告→prompt/`--acknowledge`→进程内或 helper apply）1,779–1,807、`io_failure` 1,809–1,811、`prompt_install_acknowledgement` 1,815–1,833、`helper_error_envelope`（`pub(crate)`）1,845–1,855、`run_privileged_system_install`（`pub(crate)`；helper 缺失→`RootRequired`、argv 固定序、退出码 126→`AuthorizationCanceled`、错误信封解析）1,857–1,906、`install_failure` 1,910–1,920、`write_install_targets` 1,922–1,960、`write_install_plan` 1,963–2,055、`write_install_result`（verification 三态、压缩/平台两义按 `observed` 区分）2,057–2,123。

### 1.2 测试区实测（25 个测试）

`#[test]` 属性实测 **25 个**：`mod tests`（2,125–3,226）内 24 个 + `mod decoding_tests`（3,228–3,241）1 个。按被测职责分组（函数名与行号实测）：

| 组 | 测试（行号） | 数量 |
| --- | --- | --- |
| 安装 CLI 解析：`system_cli_parses_install_and_preview_commands` 2,216 | 安装域入口的 clap 解析 | 1 |
| 安装流程：`install_shows_consequences_before_prompting` 2,298、`install_requires_the_typed_device_name` 2,333、`install_refuses_the_device_that_hosts_the_running_system` 2,380、`install_rejects_an_untrusted_image_path` 2,399 | 走 `run_install` 注入终端 | 4 |
| 安装呈现：`the_cli_reports_the_core_verification_reason` 2,453、`the_cli_wording_separates_the_compressed_case_from_the_platform_one` 2,487、`a_failed_reread_is_not_printed_as_a_mismatch` 2,572、`install_plan_preview_is_localized` 2,777 | 经 `run_install`/`manager.plan` 取得结果后直接调 `write_install_*` | 4 |
| 特权与错误码：`install_error_codes_are_stable_across_surfaces` 2,655、`privileged_install_without_the_helper_requires_root` 2,695、`the_helper_error_envelope_is_read_only_when_it_is_one` 2,719、`install_privilege_routing_follows_policy_provider_and_uid` 2,739、`cancelled_authorization_keeps_its_own_code` 2,765 | helper 桥接/权限路由/稳定码 | 5 |
| CLI 解析（硬件）：`hardware_cli_parses_nvme_subcommand_and_flags` 2,806、`hardware_cli_parses_mmc_subcommand_and_flags` 2,912、`hardware_cli_parses_storage_subcommand_and_flags` 2,936 | clap 解析 | 3 |
| 存储格式化：`format_nvme_status_uninitialized_en_and_zh` 2,830、`format_nvme_status_initialized_with_device` 2,851、`format_mmc_status_uninitialized_en_and_zh` 2,961、`format_mmc_status_initialized_with_devices` 2,982、`format_storage_status_contains_both_sections` 3,055、`format_nvme_and_mmc_status_handles_unavailable_and_special_percentages` 3,125、`mmc_unavailable_preserves_reason_in_both_cli_commands` 3,194 | 文本呈现 | 7 |
| 解码：`base64_requires_canonical_padding` 3,233 | `decode_base64` | 1 |

测试 fixture（`mod tests` 内模块级）：`InstallScratch`/`install_scratch` 2,135–2,156（scratch 目录在 `target/` 下、Drop 自清理）、`mbr_image` 2,159–2,167、`demo_install_manager` 2,169–2,174、`install_request_for` 2,176–2,178、`run_install` 2,181–2,213（注入 `Cursor` 终端并折叠 `InstallApplyError`）。全部离线：仅 Demo 后端 + 内存写入（`DryRunBackend`/demo 写计数），不触真实块设备、不启动 helper；`privileged_install_without_the_helper_requires_root` 在装有 helper 的主机自行 return（2,696–2,700）。

### 1.3 跨模块消费面（C1 核对面，实测全 crate grep `crate::`）

`helper.rs` 零引用（见前言）。`tui.rs` 与 `server.rs` 经 crate 根路径消费的 main.rs 项**恰为以下 7 个**（`--list` 口径穷举，另有 `crate::i18n` 模块引用与文档注释文字）：

| 符号 | 消费点（实测行号） |
| --- | --- |
| `format_bytes` | tui.rs 741/776/795/797/906/1050/1064/1,746/1,853/1,854/2,003（含 2,734 测试）；server.rs 无直调 |
| `effective_uid` | tui.rs 128、3,100（测试）；server.rs 952、2,022（测试） |
| `install_needs_privilege` | tui.rs 128；server.rs 952 |
| `InstallApplyError`（+三变体+`code()`/`message()`） | tui.rs 592/608/609/612；server.rs 1,176–1,194、2,055/2,060/2,083/2,094（测试） |
| `run_privileged_system_install` | tui.rs 588；server.rs 953 |
| `system_install_error_code` | tui.rs 1,013；server.rs 1,168 |
| `PRIVILEGED_HELPER` | tui.rs 3,099（测试）；server.rs 2,021（测试） |

与 02 号规格 §1 登记的 6 项（`format_bytes` 等 5 项 + `PRIVILEGED_HELPER`）核对：本表新增 `system_install_error_code`（tui.rs 1,013 实测在用，02 未列）。本规格不改 02，拆分以本表 7 项为准回导。`helper_error_envelope`（`pub(crate)`）实测仅 main.rs 内部（1,889）与其测试使用，无跨模块消费，拆分后**不再需要** crate 根路径（见 §3 说明）。

另有集成测试（不属于 main.rs，但为其 CLI 协议护航，实测存在）：`crates/rsetup-app/tests/system_install_json.rs`（子进程跑 `--demo system install`，断言 JSON 模式 stdout 恰一个文档、预览走 stderr）与 `crates/rsetup-app/tests/helper_cli_args.rs`（只跑 `rsetup-next-helper`，非法/未知参数 fail-closed，不触特权动作）。两者按 C6 不拆分、不修改，作为 §7 验证手段。

## 2. 目标文件树（拟建，未创建）

```text
crates/rsetup-app/src/
├── main.rs                保留（crate 根）：3 个既有 mod + 新 mod 声明、main()（配置解析与
│                          全部命令分发 match 原样）、§3 的 crate 根回导清单
├── cli.rs                 拟建：Cli 与 10 个子命令 enum（clap 派生类型合计 11）、parse_fan_curve_point、default_command
├── output.rs              拟建：print_source_* / print_status / print_actions / print_doctor /
│                          print_json_or_debug / decode_base64
├── storage_format.rs      拟建：format_bytes / format_nvme_status / format_mmc_status / format_storage_status
├── fan_curve.rs           拟建：fan_curve_request / run_fan_curve_daemon / force_fan_curve_failsafe
└── system_install/        拟建：系统安装命令域（现有 1,497–2,123 整体）
    ├── mod.rs             InstallOptions / system_install_backend / system_install_manager /
    │                      DryRunBackend / system_install_request + 测试共享 fixture（test-only）
    ├── privileged.rs      PRIVILEGED_HELPER / PKEXEC / system_install_error_code / effective_uid /
    │                      install_needs_privilege / InstallApplyError / helper_error_envelope /
    │                      run_privileged_system_install
    ├── command.rs         run_system_command / run_system_install / io_failure /
    │                      prompt_install_acknowledgement / install_failure
    └── render.rs          write_install_targets / write_install_plan / write_install_result
```

入口决策：**main.rs 保留为 bin crate 根**（`[[bin]] path = "src/main.rs"`，C1 禁改 Cargo.toml，无法目录化），只留 mod 声明、`main()`（约 345 行分发体，即「配置 + 命令分发」职责本体）与回导，终态约 400 行。不新建 `benchmark` 等模块：benchmark 在 main.rs 只有分发体（480–500），实现属 `rsetup_core`；`Run` 分支体（447–478）同样只留分发处。`i18n.rs`、`server.rs`、`tui.rs`、`helper.rs` 一律不动：`server.rs` 的资源接线由 05 号前端任务的单独规格负责，本规格不拆 `server.rs`、不与其争范围；02（tui）、03（core system_install）已定稿边界不因本文调整。

体量估算（实现区间求和 + §1.2 测试归属，约值）：`cli.rs` ≈ 430 + 解析测试 3 + 安装解析 1 ≈ 630；`output.rs` ≈ 345 + 1 测试 ≈ 365；`storage_format.rs` ≈ 330 + 7 测试 ≈ 740；`fan_curve.rs` ≈ 115；`system_install/`：mod ≈ 130、privileged ≈ 230 + 5 测试、command ≈ 190 + 4 测试 + `run_install` fixture、render ≈ 210 + 4 测试——全部 ≤ 900，无需 C4 例外。

## 3. 符号归属与回导（完整表）

跨模块符号一律**保持原 crate 根路径**：定义迁入子模块（可见性见括号），main.rs 增加 `pub(crate) use` 回导，使 tui.rs/server.rs 的 `crate::X` 逐字不改。回导链权限必须真正合法（rustc 1.99.1 实测，非推断）：`pub(crate) use` 与定义处同可见性时合法、有效可见性不变；**重导出可见性不得超过定义处自身可见性**——定义若取 `pub(in crate::…)` 而上层做 `pub(crate) use`，编译直接 E0364「is private, and cannot be re-exported」。因此 §1.3 的 7 项跨模块符号（`format_bytes` + 安装域 6 项）**定义处直接保持现 `pub(crate)` 迁入**，中间层 `system_install/mod.rs` 与根 main.rs 均为 `pub(crate) use`，全链统一 `pub(crate)`：这是**既有可见性的保持，不是新增生产 `pub`**（bin crate 无外部消费者，可见性语义止于 crate 内）。

| 符号（§1.1 锚点） | 目标 | 定义处可见性 / 回导 |
| --- | --- | --- |
| `Cli` 25–52 | `cli.rs` | 类型 `pub(crate)`（现私有；main() 424–444 引用类型与分发）。字段逐项：`demo`、`live_execution`、`lang`、`command` 各 `pub(crate)`——main() 对四者全部**点读**（`cli.lang` 424、`cli.live_execution` 425/433/441、`cli.demo` 428/442、`cli.command` 444），跨模块私有字段点读 E0616（rustc 1.99.1 实测），「现私有」只是同文件共置的事实，`pub(crate)` 为最窄补救 |
| `Commands`/`SystemCommands`/`SourceCommands`/`HardwareCommands`/`OverlayCommands`/`SpiFlashCommands`/`VideoCommands`/`ThermalCommands`/`FanCurveCommands`/`LedCommands` 54–411（10 个子命令 enum，不含 `Cli`） | `cli.rs` | 类型各 `pub(crate)`（main() 的 match 与 `run_system_command` 参数需变体路径）；**变体字段全部保持私有**——消费点（main() 445–755、command.rs 体内）仅以 `match` 模式绑定字段，跨模块对 enum 私有变体字段做模式绑定合法（实测编译运行通过；E0451/E0616 只覆盖字面量构造与点读，不覆盖模式绑定），故不扩其他子命令内部字段 |
| `parse_fan_curve_point` 759、`default_command` 937 | `cli.rs` | `pub(crate)`（derive 属性路径、main.rs 使用） |
| `fan_curve_request` 775、`run_fan_curve_daemon` 794、`force_fan_curve_failsafe` 842 | `fan_curve.rs` | `pub(crate)`（仅根分发调用） |
| `print_source_status/plan/apply` 853–935、`print_status` 945、`print_actions` 1,002、`print_doctor` 1,036、`print_json_or_debug` 1,435、`decode_base64` 1,447 | `output.rs` | `pub(crate)`（仅根分发调用） |
| `format_bytes` 1,103 | `storage_format.rs` | 定义处保持现 `pub(crate)` 随定义迁入；main.rs `pub(crate) use storage_format::format_bytes;` 回导保 `crate::format_bytes`（§1.3 消费面）。定义若收窄为 `pub(in crate::storage_format)`，根层 `pub(crate) use` 即 E0364（实测），故不得收窄 |
| `format_nvme_status`/`format_mmc_status`/`format_storage_status` 1,122–1,433 | `storage_format.rs` | `pub(crate)`（仅根分发调用；tui 不复用 CLI 文本） |
| `PRIVILEGED_HELPER` 1,504、`system_install_error_code` 1,510、`effective_uid` 1,609、`install_needs_privilege` 1,620、`InstallApplyError` 1,635（含 `code()`/`message()` 方法）、`run_privileged_system_install` 1,857 | `system_install/privileged.rs` | 六项现均 `pub(crate)`，**定义处保持 `pub(crate)` 原样迁入**；`system_install/mod.rs` `pub(crate) use privileged::{…六项};`、main.rs `pub(crate) use system_install::{…六项};` 两级回导——重导出与定义同可见性，合法（实测）且有效可见性 = 现值 |
| `helper_error_envelope` 1,845 | `system_install/privileged.rs` | 现 `pub(crate)`，实测唯一调用点 1,889（同文件）与同模块测试，零跨文件消费者 → **收窄为 privileged.rs 文件私有**，不回导（本规格唯一一项可见性收窄，非契约变化） |
| `PKEXEC` 1,505 | `system_install/privileged.rs` | **现即私有项**，随 `run_privileged_system_install` 迁入后保持私有（仅同文件 1,868 一处使用：全文件实测 `PKEXEC` 只有 1,505 定义与 1,868 一处引用，测试不直接引用）；非收窄、不回导 |
| `InstallOptions` 1,516 | `system_install/mod.rs` | 类型 + 字段逐项：`live`、`synthetic` 各 `pub(crate)`——main() 440–443 **字面量构造**（跨模块私有字段字面量构造 E0451，实测）；fixtures 2,170 构造点为同域子模块、天然可达。main() 以根私有 `use system_install::InstallOptions;` 导入即可，不进 §1.3 七项回导清单 |
| `system_install_backend` 1,525、`DryRunBackend` 1,548 | `system_install/mod.rs` | mod.rs 文件私有——消费者仅同文件 `system_install_manager` 体内 |
| `system_install_manager` 1,536、`system_install_request` 1,595 | `system_install/mod.rs` | `pub(in crate::system_install)`——消费者为 command.rs 的 `run_system_command` 体（manager 1,697/1,713/1,731、request 1,714/1,732）与域内 fixtures（`demo_install_manager` 2,170、`install_request_for` 2,177），此级别对域内兄弟模块可见（实测）；main 与 crate 其余部分不可见 |
| `run_system_command` 1,690 | `system_install/command.rs` | `pub(crate)`——main() 消费的**唯一 command 入口**（754 处调用 `system_install::run_system_command(...)`，全路径或根私有 `use` 均可，不需回导） |
| `InstallExecer` 1,604、`run_system_install` 1,779、`io_failure` 1,809、`prompt_install_acknowledgement` 1,815、`install_failure` 1,910 | `system_install/command.rs` | command.rs 文件私有——域内自治：仅被 `run_system_command`/`run_system_install` 与同文件测试调用；`run_system_install` 的跨文件消费只发生在测试，由 test-only 的 `run_install` 包装（§5）覆盖，无需提升定义可见性；main 不可见 |
| `write_install_targets` 1,922、`write_install_plan` 1,963、`write_install_result` 2,057 | `system_install/render.rs` | `pub(in crate::system_install)`——仅 command.rs（1,704/1,721/1,769/1,791）与 render.rs 域内测试使用；main 不可见（只经 `run_system_command` 入口进入） |

可见性变更清单（C1 口径）：新增的跨文件暴露仅两类——(a) main.rs 根回导 7 项，定义处保持现 `pub(crate)`、重导出同可见性、有效可见性与路径均逐字不变；(b) `cli.rs`/`output.rs`/`fan_curve.rs`/`storage_format.rs` 内部符号的类型可见性与字段清单（本表逐项：`Cli` 的 `demo`/`live_execution`/`lang`/`command` 四字段、`InstallOptions` 类型及其 `live`/`synthetic` 两字段）——原「根文件私有」拆分后必须跨文件可达，属最窄可行；可见性上界仍为 crate 内（bin crate 无外部消费者），不构成新增生产 `pub`。收窄仅一项：`helper_error_envelope`（现 `pub(crate)`，实测 crate 内除定义文件外零消费，降为 privileged.rs 文件私有）。`PKEXEC` 现为私有项，维持原可见性，**不在收窄之列**。`InstallApplyError::code()`/`message()` 两个 `pub(crate)` 方法随类型回导路径不变。

依赖方向（无环）：`cli` → `i18n`（`LocaleArg`，`Cli.lang` 字段类型 48）与 `rsetup_core`（`FanCurvePoint`——`parse_fan_curve_point` 759–773 的返回类型与构造、`--point` 的 `value_parser` 328/343 引用）；`output` → `i18n`+`rsetup_core`；`storage_format` → `i18n`（`Locale`）+`rsetup_core`；`fan_curve` → `rsetup_core`；`system_install/*` → `i18n`+`rsetup_core`+`storage_format`（仅 render.rs 用 `format_bytes`）+`cli`（仅 command.rs：`run_system_command` 签名 1,690–1,694 的 `command: SystemCommands` 形参与函数体 1,695–1,773 的变体 `match`，须 `use crate::cli::SystemCommands`；`SystemCommands` 按本表已取 `pub(crate)`，该边可行）；main.rs（根）→ 全部。域间边共两条，均为单向：render→storage_format 与 command→cli（`SystemCommands` 在域内的出现点实测仅 1,693/1,696/1,708/1,725 这四处，故 command→cli 是除 render→storage_format 外唯一的域间边）；域内另有 command→mod/privileged/render 的单向边（§6 批次 4 内部）。`cli` 不依赖 `system_install`，`storage_format` 不依赖任何新增模块，`output`/`storage_format`/`fan_curve`/`cli` 四者互不引用——终态依赖图无环结论不变；`tui`/`server`/`i18n`/`helper` 对新增模块零依赖（其消费全部走 §3 根回导路径）。

## 4. 语义保持清单（C2，逐条不得变）

1. clap 对外协议：全部 flag 名/短长式、`global = true` 范围、默认值（`--lang auto`、`--listen 127.0.0.1:8788`、`poll_ms 2000`、`hysteresis_c 2.0`、`--mode solid`、RGB 默认 255/255/255/100/5000）、`value_parser` 枚举与 `parse_fan_curve_point` 的 `TEMP:SPEED` 报错文案、`hide = true` 的 `Daemon`/`FailSafe`、name/version/about 与各 doc 注释 help 文案——逐字节移动，不重排、不改写。
2. 执行路径顺序：tracing 先于 parse；`live_linux_only` bail 先于任何 controller 构造；`ProbeMode`/`ExecutionPolicy::from_environment`/`Controller::new`/`InstallOptions`（含 `demo || !linux` 合成判定）顺序不变；无子命令时 `default_command` 的双终端判定（937–943）语义不变。
3. 退出码与流：`Ok(())`→0；`Err` 经 `#[tokio::main]` 打印 `Error: …` 至 stderr、退出 1；clap 解析失败 2。JSON 模式（`system install`）：预览与 prompt 写 stderr、结果 JSON 是 stdout 唯一文档、错误信封 `println!` 到 stdout 且 anyhow 错误仍使退出 1（1,741–1,770、1,910–1,920）；非 JSON 全走 stdout；`install_not_a_terminal` 走 stderr（1,742–1,744）。
4. 权限与 helper 分支：execer 判定在 manager/request 构造之后、prompt 之前（1,731–1,740）；`install_needs_privilege(live, synthetic, effective_uid())` 的表义不变；特权路径 argv 固定序 `pkexec PRIVILEGED_HELPER system-install-apply <target> <image> <plan_token> <ack> --confirmed`、helper 文件缺失先行短路 `RootRequired`、退出码 126 → `AuthorizationCanceled`、错误信封仅在其形状成立时采信（1,857–1,906）。`DryRunBackend` 计数恒成功、`reread_partitions` 恒 `Unsupported`、`is_dry_run` 恒 true 的未落盘契约不变。
5. 设备副作用顺序（install 主链）：plan → 写预览 → dry-run 通告 → 键入/prompt → apply（进程内或 helper）→ 结果打印；prompt 原样交 core 裁决。`Run`/`Serve`/`Tui`/fan-curve `Daemon`（信号监听先于首个 tick，794–836）与 FailSafe 的分支体只移动位置、不改控制流。
6. 与兄弟文件的接缝：main() 751/752 对 `tui::run`、`server::serve` 的调用与签名不变；§1.3 七项回导路径逐字可用；helper.rs（第二 bin）职责零改动；02 号规格所依赖的接缝（其 §6「与 main.rs 的接缝」）由回导整条覆盖。

## 5. 测试与 fixture 迁移（C3）

25 个测试按 §1.2 分组随被测职责走，断言零删改；**不把 `mod tests` 整块搬进单一 `tests.rs`**（总览 C3）。

| 测试组 | 数量 | 目标测试位 |
| --- | --- | --- |
| 安装 CLI 解析 + 硬件 CLI 解析 | 4 | `cli.rs` 的 `#[cfg(test)] mod tests` |
| 存储格式化 | 7 | `storage_format.rs` 的 `#[cfg(test)] mod tests` |
| 解码 | 1 | `output.rs` 的 `#[cfg(test)] mod decoding_tests`（名随 `decode_base64` 走） |
| 特权与错误码 | 5 | `system_install/privileged.rs` 的 `#[cfg(test)] mod tests` |
| 安装流程 | 4 | `system_install/command.rs` 的 `#[cfg(test)] pub(in crate::system_install) mod tests`（`run_install` 须被 render 测试跨模块取用，模块声明可见性见下） |
| 安装呈现 | 4 | `system_install/render.rs` 的 `#[cfg(test)] mod tests` |

fixture：`InstallScratch`/`install_scratch`/`mbr_image`/`demo_install_manager`/`install_request_for` 为 render 与 command 两域测试共用 → `system_install/mod.rs` 的 `#[cfg(test)] pub(in crate::system_install) mod fixtures`，**与首批消费者（§6 批次 4 的 render 与 command 测试）同批建立**、不留副本。**模块可见 ≠ 模块内项可见**：fixtures 成员默认私有，command.rs/render.rs 测试（拆分后为 fixtures 的兄弟模块）消费须**逐项** `pub(in crate::system_install)`（实测：此级别兄弟模块可达；不默认全给 `pub(crate)`）。按实际消费逐项核准：`InstallScratch`（类型本身；其元组字段保持私有——消费者仅经其 `Deref` 取 `&Path`，无 `.0` 命名点）、`install_scratch`、`mbr_image`、`demo_install_manager`、`install_request_for` 五项。`run_install` 并非仅流程测试用：render 域三测 `the_cli_reports_the_core_verification_reason`（2,453）、`the_cli_wording_separates_the_compressed_case_from_the_platform_one`（2,487）、`a_failed_reread_is_not_printed_as_a_mismatch`（2,572）也经它取得真实 apply 结果（实测调用点 2,458/2,492/2,577）→ 置于 `command.rs` 的 `#[cfg(test)] pub(in crate::system_install) mod tests` 内、以 test-only 项 `#[cfg(test)] pub(in crate::system_install) fn run_install(…)` 暴露（最窄选择：`run_system_install` 与 `InstallExecer` 因此可保持 command.rs 文件私有，不必提升为域可见）。**该模块声明本身即须如此取可见性、不可保持默认私有**：项的有效可见性被其各级祖先模块封顶，私有模块内的项只对该模块自身及其后代可见——`mod tests` 若保持默认私有，render.rs 测试解析 `crate::system_install::command::tests::run_install` 时被约束的是中间的 `tests` 模块而非 `run_install` 自身，测试构建阶段即 E0603（故只提模块声明，`run_install` 的声明不动，也无须升到 `pub(crate)`）。`tests` 内其余测试项保持默认私有：`#[test]` 函数由测试框架直接注册、不经路径访问，暴露面仍限 test 构建 + 域内。不得改走「`run_install` 放 mod.rs」路线——那会迫使 `run_system_install` 与 `InstallExecer`（现取 command.rs 文件私有，见 §3）提升为域可见，与本节及 §10.1 的 M5 最窄结论冲突。`privileged.rs` 的 5 个测试现不依赖上述 fixture（实测 2,655–2,774），无提前共享需要。fixture 保持 test-only，不新增对外接口、不迁 `tests/` 集成测试。

## 6. 分批顺序与回滚（C8-7）

每批一个提交（简体中文提交信息，[AGENTS.md](../../../../AGENTS.md)），批批为纯移动 + 必要 `mod`/`use`/回导（批次 4 含 main() 754 处调用点改 `system_install::` 前缀这一处必要改写）；「每批可编译」是设计断言，验证仅按 §7 执行并如实报告。回滚单位 = `git revert` 单批提交（无行为开关、无相互依赖）。

1. **storage_format.rs**：4 个格式化函数 + 7 测试迁入（定义处可见性随定义原样迁移）；main.rs 加 `mod` + 根回导 `format_bytes`。
2. **cli.rs**：11 个 clap 类型（`Cli` + 10 个子命令 enum）+ `parse_fan_curve_point` + `default_command` + 4 解析测试；`Cli` 四字段按 §3 清单升 `pub(crate)`、子命令变体字段保持私有；main.rs 原位置删除、加 `mod`/`use`。
3. **output.rs + fan_curve.rs**：8 个输出/工具函数 + `decode_base64` 测试；3 个 fan 函数。
4. **system_install/{mod,privileged,command,render}.rs（整域原子批）**：域类型与后端、特权桥接、命令与渲染一并迁入。`command.rs` 必须入本批的原因：main 入口链的调用者 `run_system_command`（main() 754 直调）与 `run_system_install` 在函数体内直接调用 `system_install_manager`/`system_install_request`/`write_install_*`/`InstallExecer` 等 helper——helper 定义先迁走而这两名调用者滞留 main.rs 即失去可见性（E0603），只能靠临时权限扩大或临时回导掩盖；故本批把 command.rs 及其全部直接调用点合批原子迁移，不引入临时态（备选「把被调 helper 一起延后」会拖碎域内文件，本规格取合批，不留二选一）。内容包括：13 个测试（5 特权随 privileged、4 呈现随 render、4 流程随 command，`run_install` 随 command 测试模块，且该 `mod tests` 的声明本批即取 `#[cfg(test)] pub(in crate::system_install)`——render 测试跨模块引用 `command::tests::run_install`，私有模块的项对兄弟模块不可达，只提项不提模块声明本批仍 E0603，见 §5）；`fixtures` 本批随首批（即全部域内）消费者建立；main.rs 落定 §1.3 六项安装域回导 + `use system_install::InstallOptions;`（根私有导入即可）、754 调用点改 `system_install::run_system_command`、440 `InstallOptions` 字面量构造点留在 main()（类型与字段 `pub(crate)` 支撑）。域内文件间依赖（command→mod/privileged/render、command→cli（`run_system_command` 的形参与变体 `match` 需 `use crate::cli::SystemCommands`，见 §3 依赖方向；`SystemCommands` 已于批次 2 取 `pub(crate)`，属本批必要 `use`）、render/command 测试→fixtures、fixtures→域项）全部在本批内自洽——本批是单一提交，也是单一 revert 单位，拆成两批不存在都能编译的切法。
5. **终检批（无代码迁移）**：main.rs 只余 `mod` 声明、`main()`、§3 终态回导与字段可见性；清理各批遗留的过渡 `use`（如有）；§1.3 七项路径与 §3 逐字段清单逐项核对后跑 §7 命令。回滚单位仍为上一批提交——本批 revert 不还原任何被迁代码，仅作门面终检。

每批结束跑 §7 命令并如实报告；任批失败或需越界改动即 revert 该批。与 02 的协调：批次 1 的 `format_bytes` 回导落地前后，tui 拆分批（02 §7）引用的 `crate::format_bytes` 恒可解析——07 先行或同窗均可，唯一硬约束是两棵树的提交不并发改 main.rs（总览 §5.3）。

## 7. 验证命令与前提（总览 §6.2 基线；本轮未执行任何一条）

| 命令 | 前提 |
| --- | --- |
| `make fmt-check` | 无（F1：勿以 `cargo fmt --all -- --check` 判定，见 [AGENTS.md](../../../../AGENTS.md)） |
| `cargo clippy -p rsetup-next --all-targets -- -D warnings` | F2/F4 编译型检查；包名实测 `rsetup-next`（目录名 ≠ 包名，总览 §6.2 包名已更正为 `rsetup-next`，命令以实测包名为准）。本包仅依赖 rsetup-core，**无需** `apps/controller-web/dist`（那是 rsetup-controller 的 build.rs 先决，F2）；干净检出拉依赖可能需网络（F3） |
| `cargo test -p rsetup-next --locked` | 同上；含 `main.rs` 单测 25 个与本包 `tests/` 两个集成 target（`system_install_json` 以 `--demo` 子进程跑 CLI、`helper_cli_args` 只喂非法/未知参数——均不触真实设备/特权） |
| `make controller-test-offline` | 全量回归出口（F2/F3：先 web dist，「offline」不承诺无网络），最终批后可选 |

拆分校验（只读）：拆分前后各跑一次 `cargo test -p rsetup-next --locked --bin rsetup-next -- --list 2>/dev/null | sed 's/: test$//' | grep -vE '^tui::' | awk -F'::' '{print $NF}' | sort`（`--locked` 为 cargo 参数置于 `--` 前，libtest `--list` 在其后），`sort` 之前先用 awk 以 `::` 取**测试函数叶子名**（最后一段）再送 `comm -3`：main.rs 的 25 个测试（`tests::…`24 + `decoding_tests::…`1）一个不少。**比对键是叶子名而非全限定路径**——拆分后模块前缀必然变化（`tests::format_nvme_status_…` → `storage_format::tests::…` 等），直接全限定比对会出系统性假阳性；25 个测试的叶子名实测互异（§1.2 表逐名核对），若 diff 出现重名再按名计数比对。「测试 → 目标文件」的归属映射仍按 §5 分组表口径，本命令只核对一个不少。`tui::` 条目属 02 的基线，排除过滤在取叶子名之前、避免串档。真库轨道（F6）与本模块无关；**特权/设备人工轨道仅登记、绝不执行**：真实块设备上的 `system install --live-execution`、pkexec 授权链与 2,695 测试的「装有 helper 的主机」分支属板端人工验证面，拆分前后由人工各验一次即可，不纳入本规格自动化断言。本包实测无 `--ignored` 用例登记需求。

## 8. 模块专属风险与 DoD

风险：

1. **回导遗漏即编译失败**：§1.3 七项路径任一漏回导，tui.rs/server.rs（02/05 域文件，本规格不改）直接 E0425/E0433——失败良性、可即时发现，但禁止用改 tui/server import 的方式「顺手修复」。
2. **JSON/stdout 协议**：install JSON 的「stdout 恰一文档」由 `tests/system_install_json.rs` 护航；批次 4（整域原子批）中 `install_failure` 的 `println!`（stdout）与 out 定向（stderr）不得合并改写（§4.3）。
3. **clap help 即协议**：doc 注释兼作 help 文案，移动时必须整段含注释搬迁；`hide = true` 子命令的 flag 面也在协议内。
4. **工作树并行改动**：行号锚点以撰写时工作树为准（总览 §3）；与 02 交叠面仅 `crate::` 消费清单（§1.3），与 05 交叠面仅 server.rs 的 7 路径可用性，均不回改对方规格。
5. `#[cfg(unix)]` 信号分支（797–836）与 `DecodeBase64` 的 canonical padding 边界（3,233 测试）是纯移动敏感点：只移位置，条件编译属性成组随行。

DoD（专属部分，通用部分按总览 §6.3）：C1 = §1.3 七项路径与 §3 逐字段可见性清单（`Cli` 四字段、`InstallOptions` 类型与 `live`/`synthetic` 两字段）逐项比对清单核对通过、`tui.rs`/`server.rs`/`helper.rs`/`i18n.rs` 与 `Cargo.toml`/`Makefile`/CI 零 diff、收窄仅 `helper_error_envelope` 一项（见 §3 清单；`PKEXEC` 现私有、维持原状不计收窄）；C2 = §4 六项逐条 diff 为零（clap 属性文本、argv 顺序、stdout/stderr 定向、退出码语义）；C3 = 25 测试按 §5 表就位、fixtures 模块与其五项成员逐项 test-only + `pub(in crate::system_install)` 就位、`run_install` 以 test-only 项落 command 测试模块、该 `mod tests` 声明同步取 `#[cfg(test)] pub(in crate::system_install)`（私有模块的项对兄弟模块不可达，漏提即批 4 E0603）、且不晚于首批消费者、无新增集成测试；C4 = 全部目标文件 ≤900、无例外申请；C5 不适用；C6 = 未触碰 `tests/*.rs` 与 `externals/`；C7 = 本文与实施 diff 无凭据、未读 `secret/`；§7 命令按前提执行并如实报告（含未执行项与设备/特权人工轨道的登记状态）；§6 五批均可独立 revert（批次 4 为整域单提交单 revert 单位）。

## 9. 与简报/总览的出入（实测为准）

1. main.rs 的内联测试实测 **25** 个（24 + 1），§1.2 逐名穷举；总览 §3 的行数口径（2,124/1,117）与实测一致。
2. 总览 §6.2 包名已更正为 `rsetup-next`（原专项命令 `-p rsetup-app` 为路径名笔误，目录名 ≠ 包名），本文按实测包名给出，命令语义与总览基线一致，无「旧命令统一修」待办。
3. 02 号规格 §1 的 main.rs 消费清单为 6 项；实测补 `system_install_error_code`（tui.rs 1,013），共 7 项（§1.3）。仅登记事实，不回改 02；该跨文档一致项已由 02 §1 补登第 7 项关闭，与本表 7 项一致。

## 10. 评审修订记录（逐项已修订，待复核，不构成 PASS 结论）

### 10.1 上轮评审项（I1–I3、M1–M5）

| 项 | 修订落点 | 内容与核准方式 |
| --- | --- | --- |
| I1 | §3 开篇、`format_bytes`/privileged 各行、§3 变更清单、§8 DoD | 回导链统一 `pub(crate)`：7 项跨模块符号定义处保持现 `pub(crate)` 迁入，`system_install/mod.rs` 与 main.rs 均 `pub(crate) use`。rustc 1.99.1 实测：同可见性链编译通过；「定义 `pub(in …)` + 上层 `pub(crate) use`」报 E0364——上轮该扩大权限链建议不合法、未照抄；已删「定义私有」「或模块私有」矛盾表述，明确全链统一是既有权限保持、非新增生产 `pub`。`helper_error_envelope` 原 `pub(crate)`、无跨文件消费者 → 收窄文件私有（唯一收窄项） |
| I2 | §3 `Cli` 行、10 个子命令 enum 行、`InstallOptions` 行 | 逐字段清单：`Cli` 的 `demo`/`live_execution`/`lang`/`command` 与 `InstallOptions` 类型 + `live`/`synthetic` 各 `pub(crate)`，理由锚定 main() 实测点读/字面量构造行号（E0616/E0451 实测依据）；10 个子命令 enum 变体字段保持私有（跨模块模式绑定合法性实测），未扩其他子命令内部字段 |
| I3 | §6 批次 4/5、§5、§8 风险 2 | command.rs 及其全部直接调用者并入批 4 原子迁移（不把被调 helper 一起延后，避免临时权限扩大），批 5 仅终检清理门面与临时 `use`，无二选一；批 4 整批依赖/fixtures/回滚自洽（单提交单 revert 单位）已明示。终态：`write_install_*`/`system_install_manager`/`system_install_request` 仅域内可见，main 只经 `run_system_command`（`pub(crate)`）唯一 command 入口，§3 已列清单 |
| M1 | §7 拆分校验 | `sort` 前插入 `awk -F'::' '{print $NF}'` 取叶子名再 `comm -3`，消除全限定路径比对的假阳性；25 个测试的「测试→文件」映射口径仍按 §5 表保留 |
| M2 | §3 `PKEXEC` 行、§3 变更清单、§8 DoD | `PKEXEC` 现为私有项（main.rs 1,505 实测无 `pub(crate)`），删除「收窄」错误事实，改为保持原可见性 |
| M3 | §3 依赖方向 | 补 `cli` → `rsetup_core`（`FanCurvePoint`：`parse_fan_curve_point` 返回类型与 `--point` value_parser 引用）边；`cli` → `i18n`（`LocaleArg`）保留 |
| M4 | §2 文件树、§6 批次 2 | 子命令 enum 实为 10 个，clap 类型合计 `Cli` + 10 = 11；已改「11 个子命令 enum」「12 个 clap 类型」两处 |
| M5 | §5 fixture 段、§8 DoD | 模块可见 ≠ 项可见：fixtures 五项成员按实际消费者逐项 `pub(in crate::system_install)`（`InstallScratch` 元组字段保持私有，经 `Deref` 消费）；`run_install` 实测被 render 域 3 测共用，改为 command 测试模块内 test-only `pub(in crate::system_install)` 项（最窄，`run_system_install`/`InstallExecer` 免提域可见；其承载 `mod tests` 的声明可见性由 §10.2 的 R1 同步补足）；§1.2 呈现组描述同步修正 |

未在本轮处理（按指示保留跨文档处理）：02 号规格漏 `system_install_error_code` 的跨文档一致项（§9.3），02 与总览均不改；该项已由 02 §1 补登第 7 项关闭（终审后清扫）。

### 10.2 定点复核项（R1–R3，本轮已修订，待复核，不称 PASS）

| 项 | 修订落点 | 内容与核准方式 |
| --- | --- | --- |
| R1 | §5 分组表「安装流程」行与 fixture 段、§6 批次 4、§8 DoD（C3）、10.1 的 M5 行 | 批 4 必撞 E0603 修正：`run_install` 的承载模块 `command.rs` 的 `#[cfg(test)] mod tests` 由默认私有改为 `#[cfg(test)] pub(in crate::system_install) mod tests`——项的有效可见性被各级祖先模块封顶，私有模块内的项仅对该模块自身及其后代可见，故 render.rs 测试解析 `crate::system_install::command::tests::run_install` 在测试构建阶段即 E0603（约束落在中间的 `tests` 模块上，而非 `run_install` 自身）。`run_install` 声明保持不动；`tests` 内其余测试项保持默认私有（`#[test]` 由测试框架注册、不经路径访问），暴露面仍限 test 构建 + 域内。明确不改走「`run_install` 放 mod.rs」路线：那会迫使 `run_system_install`/`InstallExecer` 提为域可见，与 §3/§5 及 M5 的最窄结论冲突。核准方式：main.rs 2,125（`mod tests` 默认私有）、2,181（`run_install` 定义）、2,458/2,492/2,577（render 域调用点）实测核对；本轮不编译，E0603 为可见性规则的确定性推论 |
| R2 | §3 依赖方向、§6 批次 4 域内依赖清单 | 原文「子模块之间除 render→storage_format 单向边外互不引用」不实，已改写：`run_system_command` 签名 1,690–1,694 含 `command: SystemCommands`、函数体 1,695–1,773 `match` 其变体（域内 `SystemCommands` 出现点实测仅 1,693/1,696/1,708/1,725 四处），`command.rs` 须 `use crate::cli::SystemCommands`，即存在第二条域间边 command→cli；`SystemCommands` 已按 §3 取 `pub(crate)`，该边可行、属批 4 必要 `use`。`cli` 不依赖 `system_install`，`output`/`storage_format`/`fan_curve`/`cli` 互不引用，终态依赖图无环结论保持 |
| R3 | §3 `PKEXEC` 行 | 措辞收紧（非阻断）：原「消费者仅同文件与其测试」过宽——全文件实测 `PKEXEC` 仅 1,505 定义与 1,868 一处使用（`std::process::Command::new(PKEXEC)`），测试不直接引用，已改为「仅同文件 1,868 一处使用」。事实与结论（现私有、非收窄、不回导）不变 |

### 10.3 终审后清扫记录

| 项 | 修订落点 | 内容与核准方式 |
| --- | --- | --- |
| 终审后清扫（已修订，待复核，不称 PASS） | §7 表注、§9 第 2/3 条、§10.1 末段 | 「总览 §6.2 `-p rsetup-app` 为路径名笔误／不改总览」时态句改为「总览 §6.2 包名已更正为 `rsetup-next`」口径（总览 §6.2 注记已明示 02、07 命令以 `-p rsetup-next` 为准）；§9.3 与 §10.1 末段关于 02 漏项的开放句改为「已由 02 §1 补登第 7 项关闭」（02 §1 消费清单现为 7 项，与本表 §1.3 一致，符号名/定义位置/`pub(crate)` 可见性经全 crate grep 实测复核）。其余不动；未提交、未编译、未测试、未读 `secret/` |

### 10.4 实施裁决与终态记录（批次 3 / 批次 4）

| 裁决项 | 实施落点 | 事实核定与裁决结论 |
| --- | --- | --- |
| PRIVILEGED_HELPER 两级回导带 `#[cfg(test)]` | `src/system_install/mod.rs:16-17`、`src/main.rs:11-12` | 定义处（`src/system_install/privileged.rs:9`）仍为 `pub(crate)` 保持未收窄；全 crate 消费点实测仅两处（`tui/install/mod.rs:777`、`server.rs:2183`），且两处均位于 `#[cfg(test)]` 测试函数内。两级回导（mod.rs 与 main.rs）附带 `#[cfg(test)]` 属性使生产构建暴露面更窄更保守，合法且合规（由批次 3 提交 `2f7e7d9` 确立）。 |
| 3 行旧分节注释处理结果 | 原 `main.rs:385-387`（批次 3 前） | 原 3 行分节注释（`// ---------------------------------------------------------------------------` / `// System image installation: system targets\|plan\|install` / `// ---------------------------------------------------------------------------`）在批次 3（`2f7e7d9`）整域原子搬迁时已随安装域全部内容一并自 main.rs 移除，且未残留于 `system_install/` 子树（子树内各文件头已有自洽职责与文档注释）。main.rs 中零孤儿注释残留，现状干净，无须改动代码。 |
| `print_doctor` 的 `serde_json::json!` 折行保持现状 | `src/output.rs:217-222` | 批次 2 拆分时 `print_doctor` 中 `checks.iter().map(...)` 内的 `serde_json::json!` 由单行折为 6 行。经 `make fmt-check`（rustfmt 稳定工具链）实测，该多行排版处于 rustfmt 格式化稳定区间（fmt-check 零差异通过）；按实证保持现状并在规格登记，不作回改。 |
| 拆分终态验收 | 全模块树与 25 测试 | `main.rs` 终态 382 行（仅 mod 声明、根回导、CLI 启动与顶级 match 分发，无任何业务实现体残留）；拆分目标树包含 `cli.rs` (577)、`fan_curve.rs` (81)、`output.rs` (332)、`storage_format.rs` (691) 与 `system_install/` (command.rs 381, mod.rs 171, privileged.rs 298, render.rs 452)，均 ≤ 900 行；main.rs 既有 25 个单测全数就位并按 4/1/7/13 分布保持无损。全套门禁（fmt-check、clippy、test）全绿。 |

实施登记（补录，2026-10-28；批次实施期间未逐批追记，现据 git 历史补录）：
- `1083a9e` refactor(app): 拆分格式化域至 output 并先行根回导（批次 1，审查 PASS）
- `cc77d04` fix(app): 修正批次1格式化域文件归属至 storage_format（批次 1 修复，审查 PASS）
- `55f3ce7` refactor(app): 拆分 CLI 解析与输出辅助至 cli、output、fan_curve（批次 2，审查 PASS）
- `2f7e7d9` refactor(app): 拆分安装域至 system_install 子树（批次 3，审查 PASS）
- `e05c489` docs(app): 登记 main.rs 拆分批次3/4 实施裁决与终态（批次 4 登记，审查 PASS）



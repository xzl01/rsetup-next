# 子规格 09：`crates/rsetup-core/src/hardware.rs` 拆分设计

> **状态：待用户审阅，尚未实施。** 本文件是总览规范 [2026-10-26-large-file-split-design.md](../2026-10-26-large-file-split-design.md) §4 登记的 09 号子规格，遵守其 C1–C9 契约与 F1–F8 验证前提（下称「总览」）。本任务只写文档：未改任何源码/测试、未编译、未运行测试、未读 `secret/`、未执行任何设备/固件/网络操作、未提交。

撰写基线：分支 `dev-aghost`，头提交 `c7ff640`；实测 `git status` 当时 [hardware.rs](../../../../crates/rsetup-core/src/hardware.rs) 与 [lib.rs](../../../../crates/rsetup-core/src/lib.rs) 均无未提交改动。实测 `wc -l` = 2,913，与总览 §3 快照「2,443 / 470」一致：实现区 1–2,443，测试区 2,444–2,913（单一 `#[cfg(test)] mod tests`，2,444–2,913）。本文全部行号为撰写会话逐段实测，实施时须按当时工作树重新核对（总览 §3 并行改动注记）。

## 1. 实测现状与锚点

### 1.1 体量、公开面与消费者

- 全文件 2,913 行 = 实现 2,443 + 内联测试 470（单一测试模块，无嵌套测试子模块；`crates/rsetup-core/tests/` 实测不存在，总览 C6 的集成测试面在本 crate 为空）。测试函数 **15 个**（对 `#[test]` 属性逐条实测计数；无 `#[ignore]`、无 `#[tokio::test]`、无软跳过守卫、不触数据库）。
- 模块声明：[lib.rs](../../../../crates/rsetup-core/src/lib.rs) 第 5 行 `mod hardware;`（crate 私有）；第 26–31 行 `pub use hardware::{...}` 选择性再导出**恰 23 个名字**（CoolingDevice、GpioChip、GpioConnector、GpioPin、GpioStatus、HardwareError、LedDevice、LedSavedState、LedStatus、OverlayApplyResult、OverlayBootChange、OverlayBootConfig、OverlayChange、OverlayEntry、OverlayPlan、OverlayStatus、RgbLedConfig、RgbLedGroup、ThermalStatus、ThermalZone、VideoDevice、VideoFrame、VideoStatus）。拆分前后 lib.rs 两行逐字不变（C1）。
- crate 内消费者（实测全部经 `crate::hardware::X` 顶层路径，无一触子模块路径）：[probe.rs](../../../../crates/rsetup-core/src/probe.rs) 4/48–50（`HardwareManager::new` + `overlay_status`/`video_status`）；[actions.rs](../../../../crates/rsetup-core/src/actions.rs) 10–13 导入 11 个名字、128 构造 `Arc<HardwareManager>`、于 213/231/326/330/427/441/460/470/474/478/482/518/528 调用 manager 方法 13 处（实测）；[video.rs](../../../../crates/rsetup-core/src/video.rs) 2（`VideoDevice`）；[storage.rs](../../../../crates/rsetup-core/src/storage.rs) 1（`HardwareError`）；[efi_overlay.rs](../../../../crates/rsetup-core/src/efi_overlay.rs) 6–9 导入 10 个名字（含 4 个 `pub(crate)` 自由函数）、112 `crate::hardware::uefi_boot_mode`、测试区 839/905–906/910/944/1009/1093/1222/1243 经 `HardwareManager::at_root` 与 `#[cfg(test)] gpio_status` 消费。拆分设计（§4/§5）令上述**全部 import 行零修改**。
- crate 外消费者：`rsetup-app` 四个面（实测 [server.rs](../../../../crates/rsetup-app/src/server.rs) 33 处、[tui.rs](../../../../crates/rsetup-app/src/tui.rs) 11 处、[main.rs](../../../../crates/rsetup-app/src/main.rs)/[helper.rs](../../../../crates/rsetup-app/src/helper.rs) 各 2/3 处）与 workspace 外独立构建的 [apps/desktop/src-tauri/src/main.rs](../../../../apps/desktop/src-tauri/src/main.rs)（33 处，总览 C6 排除项）**全部经 `rsetup_core::` 顶层路径**消费（`HardwareError`、`OverlayStatus` 等 23 名单内类型），拆分对其透明。
- `pub(crate)` 自由函数跨出模块树的恰 5 个（实测消费者）：`uefi_boot_mode`（1,577）、`read_overlays`（1,590）、`fingerprint`（2,062）、`display_path`（2,078）、`hardware_run`（2,130）——前四个被 efi_overlay.rs 消费，`display_path`/`hardware_run`/`fingerprint` 同时是 EFI BLS 路径的 revision/记账原语。`HardwareManager`（333，`pub(crate)`）与其全部 `pub(crate)` 方法为 crate 内契约面，可见性逐项不变。
- 依赖方向输入：`crate::model`（`ActionRun`/`ActionStatus`）、`crate::pinout`（`FunctionEvidence`/`PinoutPin`/`PinoutProfile`/`profile_by_id`/`profile_for_root`/`resolve_function_evidence`，3–6 行）、`crate::transaction::{ProcessLock, atomic_replace}`（7 行）、`crate::efi_overlay::{status, boot_change, apply}`（361/482/502）、`crate::video::capture_devices`（906）、第三方 `deviceinfo`（1,336 `observe_thermal`，经 [Cargo.toml](../../../../crates/rsetup-core/Cargo.toml) 11 行 `deviceinfo.workspace = true` 由 `[workspace.dependencies]` 引入的钉住上游 commit 的 [externals/devicesinfo](../../../../externals/devicesinfo) submodule，**绝不触碰、不格式化、不在拆分范围**）。本文件无进程内缓存、无 OnceLock/lazy 静态。

### 1.2 实现区职责锚点（实测行号区间）

- **常量** 20–23：`THERMAL_POLICY_FILE`/`THERMAL_POLICY_UNIT`（仅 thermal 域消费：1,381/1,454/1,512/2,012）、`LED_STATE_FILE`/`LED_STATE_UNIT`（仅 led 域：1,855/1,878/1,893）。
- **设备树 overlay 域（U-Boot 轨）**：公开类型 25–113（`OverlayStatus` 25、`OverlayEntry` 48、`OverlayChange` 60、`OverlayBootConfig` 68、`OverlayBootChange` 79、`OverlayPlan` 90、`OverlayApplyResult` 105）；manager 方法 `overlay_status` 354–405（UEFI 先分流 360、`find_overlay_directory` 363、`u-boot-update` 可用性合取 382）、`plan_overlays` 407、`plan_overlays_from_status` 415–494（独占资源冲突判定 448–458、`package_installed` 复判 459–466、token 铸造 481）、`apply_overlays_live` 496–579（锁→plan→验 token→改名→`u-boot-update`→失败回滚）、`find_overlay_directory` 1,553–1,574（`U_BOOT_FDT_OVERLAYS_DIR` → `boot/dtbo` → `boot/overlays` → unknown 三步探测）；自由函数 `uefi_boot_mode` 1,577–1,588（`sys/firmware/efi` 与 DT 目录分离判定）、`read_overlays` 1,590–1,626（`.dtbo`/`.dtbo.disabled` 判别、先 validate 后构造、title→id 排序）、`overlay_metadata` 1,628–1,659（`dtc -I dtb -O dts` 文本切片：`metadata {` 起点与 `\n\t};` 终点）、`quoted_values` 1,661（`\0` 分隔多值）、`validate_overlay_id` 1,905、`verify_overlay_plan` 1,938（空→`PlanRequired`/不等→`StalePlan`）、`overlay_paths` 1,948、`rollback_renames` 1,958、`rollback_overlays` 1,972（回滚改名后再跑 `u-boot-update`）、`overlay_revision` 2,040（`overlays-v1` 前缀）、`overlay_plan_token` 2,049（`overlay-plan-v1` 前缀）、`rooted_path` 2,074、`humanize_overlay_id` 2,152、`package_installed` 2,111–2,118（`dpkg-query -W -f=${db:Status-Abbrev}` 且输出以 `ii ` 开头）、`bounded_output` 2,120–2,128（stdout+stderr 截 8,000 字符）；demo 数据 `demo_overlay_status` 2,190–2,262。
- **GPIO 引脚域**：公开类型 115–175（`GpioStatus` 115、`GpioConnector` 141、`GpioChip` 149、`GpioPin` 157）；`gpio_status` 581（`#[cfg(test)]`，被 efi_overlay.rs 1009 消费）、`gpio_status_for_profile` 586（`#[cfg(test)]`）、`gpio_status_with_overlays` 594–699（chip 探测→profile 解析→overlay evidence 融合→40 针兜底）、`discover_gpio_chips` 701（`sys/bus/gpio/devices`）、`serial_console_detected` 720–732（`console=tty<字母>` 规则及 Allwinner `ttyAS` 教训注释 723–727）、`resolve_gpio_profile` 734–774（`#[allow(clippy::type_complexity)]` 734 随行）、`probe_profile_gpio_pin` 776–845（power/ground/analog/conflict/overlay/default 六分支）、`probe_gpio_pin` 847–900、`gpio_physical_kind` 2,093–2,103。
- **视频采集域**：公开类型 177–205（`VideoStatus`/`VideoDevice`/`VideoFrame`）；`video_status` 902–925（`crate::video::capture_devices` + `ffmpeg`/`timeout` 双存在性 908）、`capture_video_frame` 927–995（`valid_video_id` 936、`timeout --signal=KILL 8 ffmpeg …` 参数数组 955–974、8 MiB 帧上限 983）、`valid_video_id` 1,932、`encode_base64` 2,167–2,188（自实现、无 base64 依赖）；demo `demo_video_status` 2,264、`demo_video_frame` 2,288–2,304（SVG 帧经 `encode_base64`）。
- **LED/RGB 域**：公开类型 242–297（`LedStatus` 242、`LedDevice` 255、`RgbLedGroup` 268、`RgbLedConfig` 278、`LedSavedState` 290）+ 私有 `RgbLedSnapshot` 299–306；`led_status` 997–1,085（节点发现→RGB 通道配对 1,012–1,066：三通道齐且全支持 `pattern` 才成组）、`apply_led_trigger_live` 1,087–1,137、`apply_rgb_led_live` 1,139–1,179、`validate_rgb_led_config` 1,181、`restore_led_state_live` 1,200–1,240（先 triggers 后 rgb、逐项可用性复核）、`apply_rgb_led_transactional` 1,242–1,313（快照 trigger/pattern/brightness→逐通道写入→失败回滚）、`led_nodes` 1,315–1,328（`sys/class/leds` 先入 + `leds-gpio`/`leds_pwm` 平台驱动目录补录、`or_insert_with` 首见保留）；自由函数 `collect_led_directory` 1,681、`read_led_device` 1,693（`[current]` 中括号解析）、`split_rgb_channel` 1,727（6 后缀表）、`validate_led_id` 1,743/`validate_led_trigger` 1,756/`validate_rgb_config` 1,768（字符集/长度/mode/brightness/cycle_ms 域）、`rgb_patterns` 1,786–1,811（solid/breath/rainbow 三模式 pattern 串）、`rollback_rgb_led_snapshot` 1,813–1,842（**先恢复 brightness 再回 trigger** 的内联注释 1,817–1,818）、`read_led_saved_state` 1,854（键值与组 id 一致复判 1,867–1,870）、`write_led_saved_state` 1,877（`atomic_replace`）、`enable_led_persistence` 1,888–1,903（`root != "/"` 短路 1,889）；demo `demo_led_status` 2,306–2,379。
- **热管理域**：公开类型 207–240（`ThermalStatus`/`ThermalZone`/`CoolingDevice`）；`thermal_status` 1,330–1,411（`deviceinfo::observe_thermal` 1,336→ThermalZone 过滤 1,341→冷却设备→`policy_intersection`→`step_wise`/`power_allocator` 推荐启发式 1,383–1,396）、`apply_thermal_policy_live` 1,413–1,502（锁→fan-curve 冲突门禁 1,419→`systemctl enable` 前置 1,453→逐 zone 写→持久化，失败回滚）、`restore_thermal_policy_live` 1,504、`apply_thermal_policy_without_persistence` 1,518；自由函数 `validate_policy` 1,920、`restore_zone_policies` 1,994、`write_thermal_policy` 2,011（`atomic_replace`）、`policy_intersection` 2,021–2,038；demo `demo_thermal_status` 2,381–2,442。
- **共享件**：`HardwareError` 308–330（10 变体：Unsupported/InvalidInput/Conflict/ConfirmationRequired/PlanRequired/StalePlan/RootRequired/AuthorizationCanceled/Authorization/Io；`ConfirmationRequired`/`AuthorizationCanceled`/`Authorization(_)` 由 [actions.rs](../../../../crates/rsetup-core/src/actions.rs) 侧构造（实测构造共 27 处：ConfirmationRequired 6 + AuthorizationCanceled 7 + Authorization 14；另 2,020/2,063 为测试侧 `matches!` 匹配，与 1,211 同口径）、`RootRequired` 在 efi_overlay.rs 侧（466 构造；1,211 为测试侧 `matches!` 匹配；32 为生产侧 `matches!` 匹配（status 函数内））——枚举定义留在薄入口、逐字不变即两侧消费者零改动）；`HardwareManager` 332–336（`root`/`synthetic` 两私有字段）+ `new` 339 + `#[cfg(test)] at_root` 346；`with_rollback_result` 1,844–1,852（`"{error}; rollback failed: {rollback}"`）、`fingerprint` 2,062–2,072（双链 64 位 FNV，常量/旋转位逐字）、`display_path` 2,078（root 前缀剥离显示）、`read_trimmed` 2,087、`command_exists` 2,105–2,109（PATH 扫描）、`hardware_run` 2,130–2,150（Uuid v4 + `finished_at`）。

### 1.3 测试区结构与 ignored 设备/环境轨道登记

15 个测试全部自包含：各测试在 `std::env::temp_dir()` 下以 Uuid 建一次性注入 root，断言后 `remove_dir_all`；不触真实设备、不执行挂载/固件/网络操作；`enable_led_persistence` 因注入 root ≠ `"/"` 短路（1,889）不 spawn `systemctl`；唯一可能 spawn 的外部命令是 `live_overlay_probe` 路径上的 `dtc`（`overlay_metadata` 1,629–1,637 对缺失/失败**静默降级**为空 metadata，测试无硬依赖）。本文件 `#[ignore]` 为 **0**。

CI 对 `rsetup-core` 的 **2 个 `#[ignore]` 用例**（实测名称与依赖，均属兄弟模块、不在本拆分范围，**仅登记、本任务禁止执行**，迁移时位置/名称/`#[ignore]` 授权语义零改动）：

| 位置 | 名称 | ignore 文案与依赖 |
| --- | --- | --- |
| [efi_overlay.rs](../../../../crates/rsetup-core/src/efi_overlay.rs) 1,216–1,217 | `native_fdtoverlay_validation_and_apply` | `requires device-tree-compiler tools; run explicitly in packaging/HIL validation`（需 device-tree-compiler：`dtc`/`fdtdump`/`fdtoverlay`，设备/HIL 轨道） |
| [sources/benchmark.rs](../../../../crates/rsetup-core/src/sources/benchmark.rs) 325–326 | `curl_benchmark_fixture` | `requires curl and loopback sockets; run explicitly in CI`（需 curl 与回环 socket，环境轨道） |

## 2. 行为与不变量清单（逐字保持，MUST NOT 借拆分改写）

1. **探测顺序**：`overlay_status` 先 `uefi_boot_mode` 分流 EFI 后端（360–362，绝不回退 U-Boot 文件的注释 358–359 随行）、再 `find_overlay_directory` 三步探测（配置项→`boot/dtbo`→`boot/overlays`）；`updater_available = command_exists("u-boot-update") && bootloader == "u-boot"`（382）；`supported`/`mutable`/`unavailable_reason` 三分支语义不变。`led_nodes` 的 `sys/class/leds` 先入、平台驱动目录 `or_insert_with` 首见保留（1,315–1,328）不变。
2. **外部命令与参数数组逐字**（本文件是 crate 内唯一 spawn 点，共 5 类）：`u-boot-update`（542/1,974，应用与回滚各一次）；`dtc -I dtb -O dts`（1,629–1,632，metadata 切片边界 `"metadata {"` 与 `"\n\t};"`、`\0` 分隔、5 个 key 白名单 1,647）；`timeout --signal=KILL 8 ffmpeg -hide_banner -loglevel error -f video4linux2 -i <dev> -frames:v 1 -f image2pipe -vcodec mjpeg pipe:1`（955–974，**8 秒超时**）；`dpkg-query -W -f=${db:Status-Abbrev} <pkg>`（2,112–2,116，仅认 `ii ` 前缀）；`systemctl enable` 两个 unit 常量（1,453/1,892）。帧大小上限 8 MiB（983）、`bounded_output` 8,000 字符（2,127）不变。
3. **锁与写入硬序**：三把 `ProcessLock` 名称为协议的一部分——`rsetup-next-uboot-overlays.lock`（506）、`rsetup-next-led.lock`（1,092/1,143/1,201）、`rsetup-next-fan-curve.lock`（1,417/1,505）。`apply_overlays_live` 硬序：UEFI 分流→取锁→重建 plan→验 token→空变更短路成功→目录消失门禁→逐 change `fs::rename`（记录 completed）→`u-boot-update`→失败 `rollback_overlays`（逆序改名 + 再跑更新器）。LED 写序：锁→validate→status 复判→`enable_led_persistence`→写 `trigger`→写持久化状态，失败回写 previous trigger。RGB 事务：先全通道快照（trigger/pattern/brightness 三项，任一缺失即 `Unsupported`「no safe rollback」1,263–1,265）→逐通道 `trigger=pattern`→写 pattern→失败 `rollback_rgb_led_snapshot`（**先 `none`+brightness 再回 trigger**，1,817–1,824 的内核行为注释随行）。thermal：锁→fan-curve 冲突门禁→`validate_policy`→status/available 复判→`power_allocator`×pwm-fan 冲突→`systemctl enable` **先于**任何 zone 写入→逐 zone 写（失败回滚 `restore_zone_policies`）→`write_thermal_policy`。
4. **持久化与原子写**：`/etc/rsetup-next/led-state.json` 与 `/etc/rsetup-next/thermal-policy` 一律 `parent create_dir_all` + `crate::transaction::atomic_replace`（1,885/2,017）；读取侧 `read_led_saved_state` 的逐项 validate 与「saved RGB key == group_id」复判（1,862–1,873）不变。本模块**无进程内缓存**：`cached`/`configuration_cached` 字段恒由 efi_overlay/探测结果透传（U-Boot 路径恒 `false`，398/627），拆分不新增也不删除任何缓存。
5. **指纹与 token 文本**：`fingerprint` 双链 FNV-64（左链常量 `0xcbf2_9ce4_8422_2325` 起、右链 `0x8422_2325_cbf2_9ce4` 起、乘子 `0x100_0000_01b3`、rotate 1/7、输出 `{prefix}-{left:016x}{right:016x}`）是跨进程可见文本（API/UI 回显 revision/plan_token），且被 efi_overlay 复用——逐比特不变；`overlays-v1`/`overlay-plan-v1` 前缀与字节串接顺序（2,040–2,060）不变；`verify_overlay_plan` 的「空→`PlanRequired`、不等→`StalePlan`」映射不变。
6. **错误映射**：`HardwareError` 10 变体的 `#[error(...)]` 文案逐字；`with_rollback_result` 的 `"{error}; rollback failed: {rollback}"` 拼接、`rollback_overlays` 的双错误 `"{first}; {second}"` 合并（1,987–1,991）不变；serde 线上 camelCase 与 `#[serde(default)]` 字段集不变（rsetup-app HTTP/TUI 协议面）。
7. **合成（demo）语义**：`synthetic` 短路恒在各方法第一行（356/904/934/999/1,332）；synthetic 时跳过 `package_installed`（460）与 profile 覆盖语义（`"none"`→无 profile、缺省 rock5b，611–616）；5 个 `demo_*` 函数为生产可用的演示数据（03 号先例同口径：**MUST NOT 降格为 test-only**）。
8. **串口控制台判定**：`console=tty` + 首字母为 ASCII 字母即串口（722–731）与虚拟终端 `tty<数字>` 反例集合，逐字随 `serial_console_detected` 迁移，其上方 4 行教训注释（723–727）随行。

## 3. 目标私有模块树与薄入口（拟建，未创建）

沿用仓内既有 `foo.rs` + `foo/` 目录模块风格（实测 `mmc.rs` + `mmc/`、`nvme.rs` + `nvme/` 同构），入口文件名不变、lib.rs 第 5 行 `mod hardware;` 零改动：

```text
crates/rsetup-core/src/hardware.rs        薄入口（约 120 行）：23 个 `pub use` 名单 +
  ├── 5 个 `pub(crate) use` 再导出（§4.2）+ HardwareError(308–330) +
  └── HardwareManager 结构体(332–336)+new(339)+#[cfg(test)] at_root(346)——字段保持私有，
      入口私有项对本子树全部后代直接可见，五个域文件得以零提权直用 self.root/self.synthetic
crates/rsetup-core/src/hardware/
├── overlay.rs   设备树 overlay（U-Boot 轨）：7 公开类型 + overlay_status/plan_overlays/
│                plan_overlays_from_status/apply_overlays_live/find_overlay_directory +
│                uefi_boot_mode/read_overlays 等 15 个自由函数 + demo_overlay_status + 5 测试
│                （约 780–860 行）
├── gpio.rs      GPIO 引脚解析：4 公开类型 + 8 个方法（含 2 个 #[cfg(test)]）+
│                gpio_physical_kind + 5 测试（约 560 行）
├── video.rs     视频采集：3 公开类型 + video_status/capture_video_frame + valid_video_id/
│                encode_base64 + 2 个 demo + 1 测试（约 210 行；与兄弟模块 crate::video
│                的采集设备枚举分工：本域只负责状态封装与 ffmpeg 帧采集）
├── led.rs       LED/RGB：5 公开类型 + RgbLedSnapshot + 2 常量 + 7 个方法 + 11 个自由函数 +
│                demo_led_status + 4 测试（约 850–900 行，最紧目标，§9 预论证）
├── thermal.rs   热管理：3 公开类型 + 2 常量 + 4 个方法 + 4 个自由函数 + demo_thermal_status
│                （无测试）（约 390 行）
└── shared.rs    共享原语：with_rollback_result(1,844)/read_trimmed(2,087)/command_exists(2,105)
                 三个文件私有函数（迁后 `pub(super)`）+ fingerprint(2,062)/display_path(2,078)/
                 hardware_run(2,130) 三个 `pub(crate)` 函数（可见性不变）（约 75 行）
```

行数为「实现 + 同位测试」合计估算，除 `led.rs` 处于 900 线附近（§9 例外预论证与再拆方案）外全部目标 < 900（总览 C4）。**不设立** 独立 demo 文件（5 个 `demo_*` 分别只被各自域消费，随域迁移零跨文件暴露）；**不设立** 巨型 `tests.rs`（C3，测试随被测职责走，§6）。fdtdump/fdtoverlay 封装不在本文件（属 [efi_overlay.rs](../../../../crates/rsetup-core/src/efi_overlay.rs)，本规格零改动）。不新增 crate、不改任何 `Cargo.toml`/`Cargo.lock`、不增删依赖（C1/C6）。

## 4. 符号归属表与薄根再导出

### 4.1 对外可见符号（lib.rs 23 名单 + crate 内契约面）

> 行号口径（实测）：本表行号一般为符号声明行（`pub struct`/`pub enum` 行，不含其上方 derive/属性行）；overlay 域行的行号例外，与 §1.2 一致取 derive/属性首行（两行 derive 的其余类型 §1.2 比本表小 2）；`HardwareError`/`HardwareManager` 为单行 derive，§1.2（308/332）与本表（309/333）差 1；自由函数无属性，两处行号一致。

| 符号（原定义行） | 目标子模块 | 新可见性 | 保持路径证据 |
| --- | --- | --- | --- |
| `OverlayStatus`/`OverlayEntry`/`OverlayChange`/`OverlayBootConfig`/`OverlayBootChange`/`OverlayPlan`/`OverlayApplyResult`（25/48/60/68/79/90/105） | `hardware/overlay.rs` | `pub`（不变） | lib.rs 26–31、`crate::hardware::X`（actions.rs 10–13、efi_overlay.rs 6–9） |
| `GpioStatus`/`GpioConnector`/`GpioChip`/`GpioPin`（117/143/151/159） | `hardware/gpio.rs` | `pub`（不变） | lib.rs 26–31、actions.rs 11 |
| `VideoStatus`/`VideoDevice`/`VideoFrame`（179/190/199） | `hardware/video.rs` | `pub`（不变） | lib.rs 26–31、video.rs 2、probe.rs |
| `ThermalStatus`/`ThermalZone`/`CoolingDevice`（209/225/235） | `hardware/thermal.rs` | `pub`（不变） | lib.rs 26–31、actions.rs 11 |
| `LedStatus`/`LedDevice`/`RgbLedGroup`/`RgbLedConfig`/`LedSavedState`（244/257/270/280/292） | `hardware/led.rs` | `pub`（不变） | lib.rs 26–31、actions.rs 11 |
| `HardwareError`（309） | 薄入口 | `pub`（不变） | lib.rs 26–31、storage.rs 1、efi_overlay.rs 6–9 |
| `HardwareManager`（333）+ `new`/`#[cfg(test)] at_root` + 全部 26 个方法（实测 `impl` 块内 28 个函数含 2 个构造子） | 结构体与构造子在薄入口；方法按域随 `impl HardwareManager` 块进 5 个域文件 | `pub(crate)`/`#[cfg(test)] pub(crate)`/私有，逐项不变 | probe.rs 4、actions.rs 11/79/128、efi_overlay.rs 839/905 |
| `uefi_boot_mode`/`read_overlays`（1,577/1,590） | `hardware/overlay.rs` | `pub(crate)`（不变）+ 薄根再导出 | efi_overlay.rs 112 / 6–9 |
| `fingerprint`/`display_path`/`hardware_run`（2,062/2,078/2,130） | `hardware/shared.rs` | `pub(crate)`（不变）+ 薄根再导出 | efi_overlay.rs 6–9/266–293/547 |

### 4.2 薄根再导出清单（逐条对应现路径，MUST NOT 增删）

- `pub use self::{overlay/gpio/video/led/thermal 域}::{…}`——合计恰 **23 个** `pub` 类型，与 lib.rs 26–31 名单逐项一致（lib.rs 本身零改动）。父对子 `pub` 项做 `pub use` 合法，且这是 23 个名字保持 `crate::hardware::X` 与 `rsetup_core::X` 双路径的唯一机制。
- `pub(crate) use self::overlay::{read_overlays, uefi_boot_mode};`、`pub(crate) use self::shared::{display_path, fingerprint, hardware_run};`——合计恰 **5 项**，对应 §1.1 实测的全部跨出子树消费者（efi_overlay.rs），其 import 行零修改。
- **`pub(super)` 项不得在薄根再导出**（再导出会把有效可见性从 `hardware` 子树扩至全 crate）。除上述 28 条再导出外，薄根不含其它 `use`。

### 4.3 可见性变更清单（C1 口径）

- 新增 `pub`：**0**。新增 `pub(crate)`：**0**（5 个既有 `pub(crate)` 自由函数仅移动位置）。
- 文件私有自由函数共 39 个（44 个自由函数减 5 个 `pub(crate)`），其中 **36 个保持私有**（全部消费点与定义同在 `overlay.rs`/`gpio.rs`/`video.rs`/`led.rs`/`thermal.rs` 同文件，含测试）；**恰 3 个升 `pub(super)`**：`read_trimmed`（消费者跨 overlay/gpio/led/thermal 四域，实测 711/712/721/1,351/1,352/1,381/1,513/1,555/1,694/1,719/1,720 行共 11 处）、`command_exists`（overlay 382 + video 908）、`with_rollback_result`（overlay 535/563/573 + led 1,126/1,156/1,306 + thermal 1,481/1,488）。
- **`pub(super)` 论证（同 03/08 先例）**：每个域文件都以私有 `mod` 声明在薄根内，`crate::hardware::shared::X` 路径对子树之外本就不可达，故子树内 `pub(super)` 与 `pub(crate)` 的**有效可见性集完全等值**，两者均不新增对外可见面，本设计一律取最小书写 `pub(super)`；又因薄根对 `pub(super)` 项不再导出（§4.2），提升不产生 crate 内新路径。
- **字段零提升的设计前提**：`HardwareManager` 结构体（含私有字段 `root`/`synthetic`）与 `new`/`at_root` 定义在薄根，入口私有项对本子树全部后代模块直接可见，五个域文件的 `impl HardwareManager` 块得以零提权访问字段（03 号先例同机制）；结构体若迁入任何域文件，其余四域即失去该性质。**字段提升数：0**；`HardwareError` 定义在薄入口，其 10 个变体的字段均为内联 `String`/单元，无跨文件字段访问问题。
- `RgbLedSnapshot`（299）及其 5 个私有字段随 led 域保持私有（测试同文件，实测 2,889–2,902 直读 `.pattern`/`.path`）；4 个路径常量随 thermal/led 域保持私有。

## 5. 依赖方向（实测引用推出，无环）

```text
薄根 hardware.rs ←（声明并再导出）← 全部域文件
shared  → crate::model(ActionRun/ActionStatus), chrono, uuid                 （零兄弟依赖，唯一叶子）
overlay → shared(6), crate::efi_overlay, crate::transaction::ProcessLock, 入口(HardwareManager/HardwareError)
gpio    → overlay(类型 OverlayStatus——唯一的跨兄弟类型引用，方向单一), shared(read_trimmed),
          crate::pinout, 入口(HardwareManager)
video   → shared(command_exists, encode_base64 自含), crate::video, 入口(HardwareManager)
led     → shared(read_trimmed/with_rollback_result/hardware_run), crate::transaction(ProcessLock+atomic_replace),
          入口(HardwareManager)
thermal → shared(read_trimmed/with_rollback_result/hardware_run), crate::transaction(ProcessLock+atomic_replace),
          deviceinfo(observe_thermal), 入口(HardwareManager)
```

无反向边：`overlay` 不引用 gpio/video/led/thermal；`shared` 不引用任何域；`gpio → overlay` 仅消费公开类型 `OverlayStatus`（`gpio_status_with_overlays` 594–597 的签名），`overlay` 对 gpio 零引用，不成环。硬件探测与 efi_overlay 的模块级双向依赖（361/482/502 ↔ efi_overlay 6–112）是既有事实，拆分不改变其方向。

## 6. 测试与 fixture 迁移映射（15 → 15，测试随被测职责走）

本文件**无共享 fixture/test-only helper**（实测：15 个测试各自内联临时目录构造，`use super::*;` 后无公共构件），因此不需要 `test_support` 类文件，也「不晚首消费者」——无 fixture 即无该约束的落点。私有方法的测试必须与定义同文件（零提权设计的关键约束，实测两处）：`serial_console_detected` 为私有（720）且被 `live_q8b…`(2,623) 与 `a_serial_console…`(2,647) 直测；`apply_rgb_led_transactional`/`rollback_rgb_led_snapshot`/`read_led_saved_state` 为私有且被 led 4 测试直测——**测试落位即正确性论证**。逐模块归属（测试函数行号）：

| 目标模块 | 测试 | 数量 |
| --- | --- | --- |
| `overlay.rs` | `uefi_dt_never_uses_leftover_u_boot_overlays_or_writes_them` 2,448；`uefi_detection_keeps_firmware_and_dt_separate` 2,497；`demo_overlay_plan_is_exact_and_rejects_stale_token` 2,521；`overlay_ids_cannot_escape_the_managed_directory` 2,531；`live_overlay_probe_reads_enabled_and_disabled_files` 2,691 | 5 |
| `gpio.rs` | `demo_gpio_combines_the_board_pinout_with_the_configured_overlays` 2,537；`demo_gpio_profile_override_keeps_the_debug_device_consistent` 2,565；`live_q8b_gpio_uses_documented_defaults_and_detects_the_msm_console` 2,588；`a_serial_console_is_recognised_whatever_the_soc_calls_it` 2,629（上方文档注释 2,627–2,628 随行）；`live_gpio_uses_the_configured_overlay_selection` 2,655 | 5 |
| `video.rs` | `base64_encoder_handles_padding` 2,907 | 1 |
| `led.rs` | `led_probe_and_native_writes_use_fixed_sysfs_nodes` 2,716；`led_inputs_cannot_escape_sysfs_or_exceed_safe_ranges` 2,789；`led_changes_roll_back_when_persistence_fails` 2,807；`rgb_rollback_restores_default_trigger_and_brightness_without_prior_pattern` 2,865 | 4 |
| `thermal.rs`、`shared.rs` | 无（实测本文件测试区无 thermal/共享原语用例，迁移不新增） | 0 |

计数守恒：5 + 5 + 1 + 4 + 0 = **15**。断言、临时目录构造与清理逐字随迁；测试对已迁移符号的引用改经各域文件自身 `use`（例：`overlay.rs::tests` 经同文件可达 `verify_overlay_plan`/`uefi_boot_mode`，`gpio.rs::tests` 经入口可达 `#[cfg(test)] at_root`——`pub(crate)` 构造子不属提权项）。**筛选前缀变化（§8 核对口径）**：现 `cargo test -p rsetup-core -- hardware::tests::` 命中 15；拆分后按域变为 `hardware::overlay::tests::`（5）、`hardware::gpio::tests::`（5）、`hardware::video::tests::`（1）、`hardware::led::tests::`（4），回归一律用父前缀 `-- hardware::`；末段函数名逐字不变。实测 Makefile 与 [.github/workflows/ci.yml](../../../../.github/workflows/ci.yml) 均不按 `hardware::tests::` 过滤，无 CI 接线影响。

## 7. 分批顺序、未迁代码暂存与回滚（每批设计为可编译，本轮一律未编译验证）

实施顺序 = 依赖方向自底向上 + 域间唯一类型依赖 `gpio → overlay` 前置；每批一个提交、一个可独立回滚单元（中文提交信息，[AGENTS.md](../../../../AGENTS.md)）。

| 批 | 内容 | 未迁代码暂存点与临时 use 回改点 |
| --- | --- | --- |
| B1 | 骨架：建 `hardware/`；抽 `shared.rs`（6 个函数：3 个升 `pub(super)`、3 个保持 `pub(crate)`）；薄根补 `mod shared;` + `pub(crate) use` 3 项 | 其余全部实现（含 23 个类型与整个 `impl HardwareManager`）与 15 测试留在入口原状；入口临时 `use self::shared::{read_trimmed, command_exists, with_rollback_result};` 供未迁代码续用（**B6 整行删除**；`command_exists` 消费 382/908 分别随 B2/B4 迁出——默认序下 **B4 起收缩移除**，若换序取 B4 早于 B2 则收缩顺延至 382 迁出（以 382/908 均不在入口为准），若不收缩则入口临时 use 转为 unused import，中间批次 `clippy --all-targets -- -D warnings` 必挂） |
| B2 | overlay 域：7 类型 + 4 个 manager 方法 + `find_overlay_directory` + 15 个自由函数（含 `uefi_boot_mode`）+ `demo_overlay_status` + 5 测试；薄根补 `pub use` 7 项、`pub(crate) use` 2 项（终态，非临时） | 入口剩余 `impl` 方法经入口作用域引用已迁符号（入口是域文件父模块，`pub`/`pub(crate)`/`pub(super)` 项均可经路径直接消费）；未迁域代码继续用 B1 的临时 use |
| B3 | gpio 域：4 类型 + 8 方法（含 2 个 `#[cfg(test)]`）+ `gpio_physical_kind` + 5 测试；`gpio.rs` 以 `use crate::hardware::OverlayStatus;` 消费 B2 公开类型 | 同上；本批不得早于 B2（类型依赖） |
| B4 | video 域：3 类型 + 2 方法 + `valid_video_id`/`encode_base64` + 2 demo + 1 测试 | 同上 |
| B5 | led 域：5 类型 + `RgbLedSnapshot` + 2 常量 + 7 方法 + 11 自由函数 + `demo_led_status` + 4 测试 | 同上 |
| B6 | thermal 域（3 类型 + 2 常量 + 4 方法 + 4 自由函数 + demo，无测试）+ **收尾**：薄根收敛为 §3 形态，删除 B1 的全部临时 use；此后入口只剩 §4.2 的 28 条再导出、`HardwareError`、`HardwareManager` 与 6 行 `mod` 声明 | 回改点清零：入口不再引用任何域内私有/`pub(super)` 项 |

规则：未迁代码**只留在入口 `hardware.rs` 原位置**，不另立临时文件、不注释掉、不双份共存；同批迁移该职责测试；B2–B5 可换序（仅 B3 不早于 B2），B6 恒为末批（收尾批：thermal 域是 `read_trimmed`/`with_rollback_result` 的最后消费者，入口临时 use 的删除只能在全部域迁完后进行，B6 提前即断中间批次供给）。单批回滚 = revert 该提交（改动面恒为「入口 + 该批新文件」）；各批完成不等于全量拆分完成，期间门禁以实施会话逐批 `cargo` 结果为准，本规格不声称任何编译/测试通过（总览 C7）。

## 8. 验证命令（登记项，本轮一律未执行、不声称通过）

前提实测：真实包名 `rsetup-core`（[Cargo.toml](../../../../crates/rsetup-core/Cargo.toml) 第 2 行 `name`）；core 编译不依赖 `apps/controller-web/dist`（总览 F2 仅约束 controller 的 build.rs），无需 web dist；`--locked` 属 cargo 参数（置于 `--` 之前）。工具链 stable，`make fmt-check` 的 `FMT_PACKAGES` 第 1 位即 `rsetup-core`（[Makefile](../../../../Makefile) 73 行，实测）。

1. `make fmt-check`（总览 F1 口径；禁用 `cargo fmt --all`，勿把 `externals/devicesinfo` 的钉住上游格式差异当成本仓问题）。
2. `cargo clippy -p rsetup-core --all-targets -- -D warnings`。
3. `cargo test -p rsetup-core --locked`；定向 `-- hardware::` 拆分前后恒命中 15（拆分前为 `hardware::tests::` 前缀，拆分后按 §6 域前缀，末段函数名逐字）；`-- efi_overlay::`、`-- actions::`、`-- probe::` 覆盖 §1.1 消费者的编译与行为面。
4. 全量回归出口：`make controller-test-offline`（总览 F2/F3：offline 仅指不初始化/不触数据库，干净检出下其先决仍可能执行 `npm ci` 与 vite 构建）。
5. `#[ignore]` 设备/环境轨道**仅登记、禁止执行**（总览 §6.2 末段与 F6 同口径）：本文件 0 个；本 crate 的 2 个为 §1.3 表所列 `efi_overlay::native_fdtoverlay_validation_and_apply`（device-tree-compiler）与 `sources::benchmark::curl_benchmark_fixture`（curl+回环 socket）——本拆分不触碰其所在文件，`#[ignore]` 属性与授权语义、名称、位置零改动。真机语义（sysfs/`/proc` 写、`systemctl`、`u-boot-update`、`ffmpeg` 采集）注入 root 不覆盖，需操作员在授权设备上验证，本任务不执行、不声称。

## 9. 模块专属风险、阈值例外与 DoD

- **风险 R1 指纹外部契约**：`fingerprint`/`overlay_revision`/`overlay_plan_token` 输出文本进 API/UI 且被 efi_overlay 复用（§2.5），B1 移动时逐比特不变，任何「顺手改哈希实现」即行为变更。DoD：§2.5 常量表逐项 diff 为零。
- **风险 R2 命令副作用与测试内藏耦合**：`enable_led_persistence` 的 `root != "/"` 短路（1,889）同时是生产语义（真机才 `systemctl enable`）与测试 hermetic 前提，MUST NOT 在迁移中「清理」；5 类外部命令参数数组与超时/上限逐字（§2.2）。DoD：命令字符串/参数数组/锁名逐字 diff。
- **风险 R3 结构体位置前提**：`HardwareManager` 迁入任何域文件都会使另外四域失去私有字段可见性（§4.3），实施会话若发现字段被迫提 `pub(super)` 即偏离本设计，须停批修正而非追认。
- **风险 R4 测试专用面**：`gpio_status`/`gpio_status_for_profile` 的 `#[cfg(test)] pub(crate)`（581–592）被 efi_overlay.rs 测试 1,009 消费，保持 `#[cfg(test)]` 原样；`at_root` 同理。降格或删除即破坏 efi_overlay 测试编译面。
- **风险 R5 deviceinfo submodule**：`thermal_status` 经 `deviceinfo::observe_thermal` 触第三方 [externals/devicesinfo](../../../../externals/devicesinfo)（钉住上游 commit，经 `[workspace.dependencies]` 引入）：只移动调用点，submodule 目录零触碰、不格式化（[AGENTS.md](../../../../AGENTS.md)「Rust 格式检查口径」）。
- **风险 R6 测试前缀漂移**：§6 前缀变化仅影响人工/脚本过滤习惯，CI 实测不过滤（无 Makefile/workflow 引用 `hardware::tests::`）；若实施引入过滤脚本须改用父前缀。
- **C4 阈值预论证（唯一边缘目标 `led.rs` ≈850–900 行）**：LED 触发器、RGB 图案、sysfs 节点发现（`led_nodes` 被 trigger 与 RGB 两径共用）、持久化状态与回滚纪律是同一职责域，4 个同位测试与被测语句互证（2,716 的巨型用例横跨探测→trigger→RGB→restore 全链）；实测估算贴线，若实施实测落入 900–1,500，按总览 C4 在本节补一段内聚论证即可，默认再拆方案为 `hardware/led.rs` + `hardware/led/rgb.rs`（`split_rgb_channel`/`rgb_patterns`/`rollback_rgb_led_snapshot`/`RgbLedSnapshot` 四符号连同其 2 个测试下移），本轮不预先拆。其余目标全部 < 900，无例外申请。
- **DoD**：① C1–C7 逐条自检记录；② lib.rs 第 5 行与 26–31 行 23 名单逐项比对 + 5 个 crate 内消费者（probe/actions/video/storage/efi_overlay）import 零修改 + rsetup-app 四面与 desktop shell 零修改；③ 测试 15 = 15（5/5/1/4/0，§6 表逐条点名）、本文件 `#[ignore]` 保持 0、§1.3 两项轨道位置/名称/`#[ignore]` 零改动；④ 可见性核对：新增 `pub(super)` 恰 3 项（`read_trimmed`/`command_exists`/`with_rollback_result`）、新增 `pub(crate)` 0、新增 `pub` 0、字段提升 0、`pub(super)` 再导出 0；薄根再导出恰 23 + 5 项（§4.2）；⑤ 行为不变清单 §2 八条以语句序/参数数组/锁名/错误文案 diff 核对为零（其中 §2.6 的 `HardwareError` 10 个变体 `#[error(...)]` 文案逐条 diff）；⑥ §8 命令按前提执行并如实报告（含未执行项，无据不声称通过）；⑦ 每批单提交、`git revert` 可用；⑧ 不新增 CI/Makefile 目标（总览 F8 链路不触发）、不改任何 `Cargo.toml`/`Cargo.lock`、`externals/devicesinfo` 零触碰。

## 10. 修订记录（独立审查后定点修订，逐项已修订，待复核，不构成 PASS 结论）

- **I1（Important）**：`HardwareError` 变体数三处 9 修正为 10（§1.2/§2.6/§4.3）；实测 308–330 恰含 10 变体（Unsupported/InvalidInput/Conflict/ConfirmationRequired/PlanRequired/StalePlan/RootRequired/AuthorizationCanceled/Authorization/Io）；DoD ⑤ 补充按 10 变体逐条 `#[error(...)]` 文案 diff 的核对口径。
- **M2**：§4.3 `read_trimmed` 消费清单由 7 处补全为 11 处（增补 712/1,352/1,719/1,720，逐行实测核对），`pub(super)` 结论不变。
- **M3**：§7 规则「B2–B6 相互独立可换序」更正为「B2–B5 可换序（仅 B3 不早于 B2），B6 恒为末批」——B6 为收尾批（thermal 是 `read_trimmed`/`with_rollback_result` 最后消费者、临时 use 删除在其内），提前即断中间批次供给。
- **M4**：§7 B1 行补注「B4 起将 `command_exists` 从入口临时 use 收缩移除」（其最后消费点 908 随 B4 迁走，不收缩则中间批次 `clippy --all-targets -- -D warnings` 必挂），保持每批 clippy 可跑的口径，与 M3 的 B6 末批定位一致。
- **M5**：§1.2 `RootRequired` 的 efi_overlay.rs 侧两处定性改为「466 构造；1,211 为测试侧模式匹配」（实测：466 在 `io_error` 生产函数内构造，1,211 在测试 `matches!` 内）；同段 actions.rs 侧三变体构造点改为举例口径并标注实测共 29 处（该口径含 2,020/2,063 两处测试侧 `matches!` 匹配，本轮 R-2 修正为实测构造共 27 处）。
- **§4.1 表头注记（可选项，按实测口径落笔）**：核对了本表全部类型行号，与审查建议相反——本表一般取声明行（不含 derive），overlay 域行沿用 §1.2 的 derive 首行口径，已在表前加注说明。
- **R-1**：§7 B1 行 `command_exists` 收缩注记改为条件式——实测 hardware.rs 仅 382/908 两处消费，分别随 B2（overlay 域）/B4（video 域）迁出；默认序下 B4 起收缩移除，若换序取 B4 早于 B2 则收缩顺延至 382 迁出，以「382/908 均不在入口」为准，消除与 M3「B2–B5 可换序」的组合矛盾。
- **R-2**：§1.2 actions.rs 侧三变体「实测共 29 处」修正为「实测构造共 27 处：ConfirmationRequired 6 + AuthorizationCanceled 7 + Authorization 14」；27 个行号逐行开文件核对为 270/348/400/439/491/567、239/1341/1372/1391/1421/1446/1477、237/242/1339/1344/1370/1375/1389/1394/1419/1424/1444/1449/1475/1480；2,020/2,063 两处为测试侧 `matches!` 匹配（ConfirmationRequired），与 1,211 同口径不计入构造（29 = 27 + 2 即旧口径之由）；M5 历史记录已同步标注本轮修正。
- **R-3**：§1.2 `RootRequired` 的 efi_overlay.rs 侧定性补全为三项并列——466 构造（`io_error` 函数内）、1,211 为测试侧 `matches!` 匹配、32 为生产侧 `matches!` 匹配（status 函数内）；实测该文件 `RootRequired` 仅且恰此三处。

本轮为文档级定点修订，未改任何源码/测试、未编译、未运行测试、未执行设备命令、未读 `secret/`、未提交；正文各处的旧变体计数表述已全部更新（本记录所述修正本身属修订历史）。

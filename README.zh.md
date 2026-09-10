# rsetup next

[![CI](https://github.com/xzl01/rsetup-next/actions/workflows/ci.yml/badge.svg)](https://github.com/xzl01/rsetup-next/actions/workflows/ci.yml)

面向 Linux SBC 的统一控制平面，可用作可脚本化的 CLI、交互式 TUI、环回 Web 控制台，
以及可选的 Tauri 桌面应用。

这是原版
[`radxa-pkg/rsetup`](https://github.com/radxa-pkg/rsetup) 的基于 Rust 的后继项目。新的 Rust 控制
平面直接承担探测、策略与操作执行；在运行时不调用、也不依赖旧版 `rsetup` 命令。

## 当前可用功能

| 界面 | 入口 | 用途 |
| --- | --- | --- |
| CLI | `rsetup-next status`, `actions`, `run`, `sources`, `hardware`, `doctor` | 面向自动化与恢复场景的输出，包括 JSON |
| TUI | `rsetup-next tui` | 可通过键盘操作的本地任务控制中心 |
| Web GUI | `rsetup-next serve` | 响应式浏览器控制中心与 JSON API |
| Desktop GUI | `apps/desktop` | 使用相同静态 UI 与 `rsetup-core` 的 Tauri 外壳 |

核心目前探测板卡标识、操作系统、内存、负载、温度、运行时长、存储、网络接口、选定服务
以及硬件能力信号。其引导式操作目录涵盖检查、系统更新、完整的 OpenSSH 与 Docker 服务
生命周期、网络恢复、根文件系统扩展、可逆的休眠策略以及重启。
不适用于当前系统的操作仍会显示并附带原因，而不是直到执行开始后才失败。

APT 软件源管理以引导式工作流的形式实现在 CLI、TUI、Web 和 Tauri 界面上。它同时检测传统 `.list` 文件和 Deb822
`.sources` 文件，将替换限制在已知的 Debian、Ubuntu 与 Radxa
端点，保持第三方软件源原样不动，并在确认前预览每一处
受影响的行。实际执行会创建带时间戳的
备份、进行原子写入、运行 `apt-get update`，并在刷新失败时自动恢复
之前的文件。

镜像测速是只读的，无需管理员授权。
可使用软件镜像面板中的 **Test mirrors**、TUI 中对选中的镜像按 `b`，
或运行 `rsetup-next sources benchmark --mirror official --json`（省略
`--mirror` 则测速整个目录）。每个提供商采样一个已配置的系统
`InRelease` 索引，并在适用时采样一个 Radxa 索引。结果将
首字节时间与样本传输速率分开显示；小型索引无法测量峰值
带宽，也不能证明每个包/pocket 都可用。每个索引最多消耗 512 KiB，
使用 2 秒连接超时和 6 秒总超时。
仅使用目录中的 HTTPS 端点，验证 TLS，不跟随重定向，
并忽略 curl 配置文件。不会发生 APT 刷新、文件写入
或自动镜像选择。演示模式使用带标注的合成结果。
选择某个已测速的镜像仍然需要在应用前进行预览和确认。

原生硬件管理器现已覆盖七个已迁移的工作流：

- 设备树 Overlay 从受管理的启动目录中列出，检查
  其声明的资源冲突与软件包依赖，使用精确的版本绑定 token 进行预览，并在
  U-Boot 系统上于 `u-boot-update` 之前以事务方式重命名。UEFI + DT 使用由 Q6A 和 Q8B 共享的独立原生 EDK2/BLS
  后端，遵循上游 rsetup 中
  [edk2-menu.sh](https://github.com/radxa-pkg/rsetup/blob/main/src/usr/lib/rsetup/cli/edk2-menu.sh)
  的布局，但不调用该脚本。正在运行的内核的精确 Type #1 条目及其有序的
  `devicetree-overlay` 引用决定保存的选择，而非文件后缀。
  预览会列出内核、条目以及 DTB/DTBO 路径。它 does **not** 更改
  默认条目或其他内核版本：更改仅在该内核启动时
  生效。`fdtoverlay` 在私有备份、原子 BLS 替换与失败回滚之前验证该选择。不明确/不安全的布局按
  失败关闭（fail closed）处理；EFI 永不回退到遗留的 U-Boot 配置。
  受保护的 ESP 文件需要显式的 **Authorize read** 操作（CLI：
  `hardware overlays status --authorize --json`，或带 `--authorize` 的
  `plan` / `gpio`）。这一固定的辅助操作是只读的。经授权的快照
  驻留在进程内存中，标记为缓存，并可显式重新读取；
  应用时总会针对计划 token 重新验证当前 root 拥有的输入。
- 40 针 GPIO 排针是一个只读引脚图，其背后是 `xzl01/pin-out` 中的 20 个规范化 SBC
  配置文件，外加一个官方 Radxa Dragon Q8B 配置文件。
  每个物理引脚恰好显示一个已配置的功能：已保存且启用的 Overlay 分配优先，否则已知的 SBC 显示其精确的默认功能（pin-out 中的 `Function1`，
  Q8B 文档中的 `Function0`）。未知的
  通用排针保持未分配。
  抽屉面板省略 GPIO 芯片、行号、方向、消费者及内核归属
  元数据。已保存的 Overlay 更改会立即显示，并标记为需要
  重启才能生效；状态路径从不调用 `gpioget`，也不请求
  GPIO 行。
  EFI/BLS 中保存的选择送入同一个 GPIO 解析器。当配置
  无法读取时，只显示官方默认值，而不声称是当前的 mux 状态。实际的 overlay 激活与电气行为仍需在单独确认的写入和重启之后进行硬件
  测试。
- 经验证的 Video4Linux 采集节点可以通过 `ffmpeg` 捕获受限的单帧摄像头测试。发现过程使用只读的 `VIDIOC_QUERYCAP`，排除
  编解码/M2M、输出、元数据以及未验证的节点。设备 ID 经过枚举
  和验证，而不是接受为任意路径。
- 热区与散热设备直接从 sysfs 检查。原始的 thermal-governor 选择被保留，包括
  `pwm-fan`/`power_allocator` 不兼容检查；选定的策略由原生 systemd 单元在启动时恢复。
- 温度驱动的 `pwm-fan` 曲线接受 2–8 个点，要求温度递增、转速不减，
  要求 90 °C 时达到 100% 冷却，
  提供有界的迟滞与轮询，并允许在确认精确计划之前预览解析出的整数
  冷却状态。禁用该曲线会恢复所保留的内核 governor。
- Linux LED 类设备提供经过验证的状态灯触发器和受支持的
  RGB 组。保存的触发器与 RGB 状态由新的控制平面在启动时恢复。
- SPI 启动闪存管理检测 NOR MTD 目标和可信的已安装
  Rockchip U-Boot 布局。写入和擦除操作要求精确的
  版本绑定计划，创建仅 root 可访问的备份，校验回读，并在操作
  失败时尝试恢复。

## 安全模型

rsetup 将观察与变更分离：

- 在非 Linux 开发主机上，它自动使用带标签的合成 SBC 遥测数据。
- 变更操作在所有主机上默认执行演练。
- 实时执行需要 Linux、按声明处以 root 运行，以及显式选择 `RSETUP_EXECUTION=live` 或 `--live-execution`。
- 受保护的、高风险的和关键的操作需要确认。
- HTTP API 只接受来自固定目录的操作标识符；它不暴露任意 shell 执行。
- Web 服务器默认绑定到 `127.0.0.1:8788`。
- fan-curve 的 apply 和 disable 在最终状态重新校验之前获取排他性的跨进程锁。精确的请求以及包含已持久化 `previous_policy` 的修订版本会绑定到预览令牌，并要求显式确认。
- 温度采样缺失、致命控制器错误、SIGTERM 或 systemd `ExecStop` 会强制配置的冷却设备进入其最大状态。

合成快照用于演示接口。它们不是物理板卡或外设已经过测试的证据。

当从调试菜单更改合成设备时，可见的 SBC 标识和 GPIO profile 会一起刷新。单调递增的加载版本可以防止较早的在途 GPIO 响应覆盖更新的设备选择。

## 构建与运行

```bash
cargo build --workspace
cargo test --workspace

# 查看当前主机。macOS 会自动返回演示遥测数据。
cargo run -p rsetup-next -- status

# 显式演示模式。
cargo run -p rsetup-next -- --demo tui
cargo run -p rsetup-next -- --demo serve
```

启动服务器后，打开 `http://127.0.0.1:8788`。

常用 CLI 操作：

```bash
rsetup-next --demo status --json
rsetup-next --demo actions --json
rsetup-next --demo run system.inspect
rsetup-next --demo run system.update --confirm
rsetup-next --demo sources status
rsetup-next --demo sources plan cqu
rsetup-next --demo sources apply cqu --plan-token PLAN_TOKEN_FROM_PREVIEW --confirm
rsetup-next --demo hardware overlays status --json
rsetup-next --demo hardware overlays plan --enable rk3588-uart2-m0.dtbo
rsetup-next --demo hardware gpio --json
rsetup-next --demo hardware leds status --json
rsetup-next --demo hardware spi-flash status --json
rsetup-next --demo hardware spi-flash plan install mtd0 --image rock-5b-rk3588:rockchip-rk35
rsetup-next --demo hardware video status
rsetup-next --demo hardware video capture video0 --output camera.svg
rsetup-next --demo hardware thermal status
rsetup-next --demo hardware thermal set step_wise --confirm
rsetup-next --demo hardware thermal fan-curve status --json
rsetup-next --demo hardware thermal fan-curve plan --zone thermal_zone0 \
  --device cooling_device0 --point 40:20 --point 55:45 \
  --point 70:75 --point 82:100 --json
rsetup-next doctor
```

在 Linux 板卡上，以普通用户身份进行查看和预览。直接 CLI 会话仍可显式提升权限：

```bash
rsetup-next sources plan cqu
sudo rsetup-next --live-execution sources apply cqu --plan-token PLAN_TOKEN_FROM_PREVIEW --confirm

rsetup-next hardware thermal fan-curve plan --zone thermal_zone0 \
  --device cooling_device0 --point 40:20 --point 55:45 \
  --point 70:75 --point 82:100 --json
sudo rsetup-next --live-execution hardware thermal fan-curve apply \
  --zone thermal_zone0 --device cooling_device0 \
  --point 40:20 --point 55:45 --point 70:75 --point 82:100 \
  --plan-token PLAN_TOKEN_FROM_PREVIEW --confirm
```

从紧接其前的 plan 输出中复制 `PLAN_TOKEN`。source 令牌绑定提供方和完整的源文件内容；fan-curve 令牌绑定精确的曲线请求和提供方修订版本，包括已持久化的 `previous_policy`。如果绑定状态在执行前发生变化，命令会拒绝过期的 plan 并要求重新预览。

浏览器和桌面进程保持无特权状态。Debian 软件包附带 `/usr/libexec/rsetup-next-helper` 以及用于实时 GUI 操作的 Polkit 策略。该 helper 只接受固定目录中的操作 ID、精确的已审查 source、overlay、SPI 或 fan-curve plan、经过校验的 thermal 和 LED 配置、它们固定的启动时恢复 verb，或只读的 `overlays-inspect` verb（不带路径或命令参数）。它没有任意命令模式。如果授权被取消，各接口会报告 `authorization_failed`，且不会更改系统。

原生 fan-curve 契约由 `hardware thermal fan-curve` 下的 CLI 命令、HTTP `GET /api/v1/hardware/thermal/fan-curve`、`POST /api/v1/hardware/thermal/fan-curve/plan` 和 `POST /api/v1/hardware/thermal/fan-curve/apply`、Tauri 调用的 `fan_curve_status`、`plan_fan_curve` 和 `apply_fan_curve`，以及 helper 固定的 `fan-curve-apply REQUEST_JSON PLAN_TOKEN --confirmed` verb 共享。Hardware > Thermal 下的 Web 抽屉以提供方返回的 `status.config` 和 `status.active` 作为已保存/运行中/已停止状态的依据；选择器和编辑过的点在返回的不可变 plan 被确认之前均仅为草稿。

fan curve 要求 thermal zone 独占绑定到所选的 PWM 风扇，且没有其他 zone 控制该风扇。混合 CPU/GPU 的冷却 zone 会被拒绝，以保留内核节流。绑定会在每次 daemon tick 时再次检查；无效的已保存绑定会触发恢复内核 governor。

SPI apply 操作在所有 MTD 目标之间共享跨进程锁，该锁贯穿备份、擦除、写入、校验以及任何回滚过程。

## 英文与中文

Web 和 Tauri 接口在首次启动时检测浏览器或操作系统语言。使用顶栏的语言控件在英文和简体中文之间切换；该选择会在本地记忆。

CLI 和 TUI 的语言选择依次遵循 `--lang`、然后 `RSETUP_LANG`、然后是标准的 `LC_ALL`、`LC_MESSAGES` 和 `LANG` 环境变量：

```bash
rsetup-next --lang zh-CN status
rsetup-next --lang en actions
RSETUP_LANG=zh-CN rsetup-next --demo tui
```

`auto` 是默认值。源管理（source management）的指引和结果遵循相同的 locale。JSON 输出和 HTTP API 负载无论显示语言如何，都保持稳定的标识符和提供方值的原样，因此用户切换 locale 时脚本的行为不会改变。

## 桌面应用

Tauri 应用直接调用 `rsetup-core`，而不是启动 HTTP sidecar。同样的 `ui/` 文件在存在 Tauri invoke 传输时选用它，在普通浏览器中则选用 HTTP 传输。

```bash
cd apps/desktop
npm install
npm run dev
```

Tauri 打包有意放在根 Cargo workspace 之外，以便普通的 CLI/TUI/server 开发不会下载原生桌面依赖。

## 项目结构

```text
crates/rsetup-core/       类型化的遥测数据、capability、action 和审计模型
crates/rsetup-app/        clap CLI、ratatui TUI、axum API 和嵌入式 Web 资源
ui/                       浏览器/Tauri 控制中心和展示层 locale 目录
apps/desktop/src-tauri/   可选的桌面 shell
data/pinouts.json         规范化的 20 个 profile 的 SBC pinout 目录
data/pinouts/dragon-q8b.json  官方 Q8B profile（单独维护）
scripts/import-pinouts.mjs 用于本地 pin-out checkout 的可复现导入器
```

参见[架构说明](docs/architecture.md)，了解提供方边界、API 路由、操作执行以及计划中的远程节点接缝。

## Debian 软件包

该软件包安装 `rsetup-next` CLI/TUI/Web 二进制文件、其最小特权的 helper、配套的 Polkit 策略、thermal、fan-curve 和 LED 单元，以及固定 SPI 操作使用的 `mtd-utils` 依赖。风扇服务将 root 可写、用户可读的配置持久化到 `/etc/rsetup-next/fan-curve.json`，并作为 `rsetup-next-fan-curve.service` 运行。它不安装已移除的 Bash 实现，也不以 root 身份运行浏览器进程。可选的 `device-tree-compiler`、`gpiod`、`v4l-utils` 和 `ffmpeg` 软件包可增强对应的硬件工具。

```bash
make deb-prepare
make deb
```

`deb-prepare` 将锁定版本的 Rust 依赖下载到被忽略的、软件包本地的 Cargo 缓存中。随后的 `dpkg-buildpackage` 步骤以离线模式运行 Cargo。生成的软件包仅通过本仓库维护和分发；不打算提交至 Debian 归档。

软件包在 Debian 13/Trixie 上以 Rust 1.85 或更高版本构建，然后在 Debian 12/Bookworm 上安装并冒烟测试。Bookworm 是运行时兼容基线；其 Rust 1.63 工具链不用于源码构建。CI 同时校验 amd64 和 arm64 软件包。

`ui/assets` 下的 SoC 厂商标识仍归其各自所有者所有。它们仅用于设备厂商识别，不会按本项目的 GPL-3+ 许可证重新授权。

规范化的 20 个 profile 的 GPIO 目录派生自 [`xzl01/pin-out`](https://github.com/xzl01/pin-out)。其版权所有者已授权将转换后的快照以 `GPL-3.0-or-later` 分发；确切的源提交与再生成说明见 [`data/PINOUT_PROVENANCE.md`](data/PINOUT_PROVENANCE.md)。额外的 Q8B profile 派生自 Radxa 的 GPIO 文档，其 CC-BY-4.0 署名与许可证单独保留。

## 硬件验证边界

Rust workspace、demo provider、CLI、HTTP API 和浏览器 GUI 可以在开发主机上测试。Q8B 实时只读探测已在 Ubuntu 26.04.1（UEFI + DT）上验证；结果与限制见[测试报告](docs/testing/q8b-2026-09-07/report.md)。其他受支持的 SBC 仍需要实时验证。overlay 事务、Overlay-to-Pinout 映射、真实摄像头采集、sysfs thermal/LED 写入、启动时恢复以及带备份的 SPI NOR 操作均已实现，但尚未在物理硬件上执行过。特别是，温度驱动的曲线尚未用真实的 Linux SBC `pwm-fan` 冷却设备进行物理验证。Overlay 文件名和 mux 模式仍需在受支持的 Pinout profile 范围内对照真实软件包进行验证；未知的通用 pin header 有意保持未分配。Bootloader、SPI/eMMC、GPIO、overlay、thermal、网络以及改变电源的操作必须在可恢复的硬件上逐一测试之后，才能被视为生产可用。

## 许可证

GPL-3.0-or-later，与上游项目保持一致。

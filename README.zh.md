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
| CLI | `rsetup-next status`, `actions`, `run`, `sources`, `hardware`, `system`, `doctor` | 面向自动化与恢复场景的输出，包括 JSON |
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

把系统镜像写入板卡的 UFS、eMMC、SD 卡或 NVMe 由
`rsetup-next system targets|plan|install` 提供。`targets` 与 `plan` 是只读的：它们列出
候选设备、写入将清除的全部分区、镜像格式、未压缩的写入大小，以及前 1 MiB 的 SHA-256
绑定。`install` 在 Linux 上除非给出 `--live-execution`，否则始终只是预演；并且只有在
精确键入目标设备名之后才会执行。承载正在运行的根或 `/boot` 文件系统的设备被永久拒绝，
已挂载目标会被拒绝并列出挂载点。写入无法回滚，失败时会报告设备上已存在的字节数。
`.img.xz` 由纯 Rust 解析器预览，仅在写入过程中通过 `xz -dc`（即 `xz-utils` 依赖）解压。
同一套流程可从 CLI、TUI 面板（按 `i`）、环回 HTTP API
（`GET/POST /api/v1/storage/install/…`，镜像只接受固定服务器目录中的 id，变更类调用需带
`x-rsetup-request: 1`）、Tauri 桌面外壳以及浏览器 **系统安装 / Install** 页面执行。任何界面
在未启用实时写入时都会拒绝写入，因此演示或预演会话不可能写盘。完整流程、稳定错误码以及
Radxa Cubie A7Z 的板上验证清单见 [docs/system-install.zh.md](docs/system-install.zh.md)。

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
- 写入系统镜像是破坏性且不可逆的：在 Linux 上需要 `--live-execution`、需要精确键入设备
  名，并且永远不会选中承载正在运行的根或 `/boot` 文件系统的设备。写入失败时会报告设备上
  已存在的字节数；没有任何回滚。
- HTTP API 只接受来自固定目录的操作标识符；它不暴露任意 shell 执行。
- Web 服务器默认绑定到 `127.0.0.1:8788`。
- fan-curve 的 apply 和 disable 在最终状态重新校验之前获取排他性的跨进程锁。精确的请求以及包含已持久化 `previous_policy` 的修订版本会绑定到预览令牌，并要求显式确认。
- 温度采样缺失、致命控制器错误、SIGTERM 或 systemd `ExecStop` 会强制配置的冷却设备进入其最大状态。

合成快照用于演示接口。它们不是物理板卡或外设已经过测试的证据。

当从调试菜单更改合成设备时，可见的 SBC 标识和 GPIO profile 会一起刷新。单调递增的加载版本可以防止较早的在途 GPIO 响应覆盖更新的设备选择。

## 构建与运行

```bash
# 前端按需构建：第一条要编译 rsetup-controller 的目标会先替你跑 `npm ci` 与
# `vite build`，因为 apps/controller-web/dist 是构建产物，不随仓库提交。
make build
make test

# 查看当前主机。macOS 会自动返回演示遥测数据。
cargo run -p rsetup-next -- status

# 显式演示模式。
cargo run -p rsetup-next -- --demo tui
cargo run -p rsetup-next -- --demo serve
```

启动服务器后，打开 `http://127.0.0.1:8788`。

`build` 与 `test`（以及同样要编译 workspace 的 `lint`、`metrics-gates`、
`controller-test-offline`）都依赖 `web-dist-check` 目标：它保证 cargo 开工前
`apps/controller-web/dist/index.html` 已在位——**缺失就自动执行 `make web-build`**
把前端构建出来，已存在则只确认一次，热工作区不会重复装依赖。
`apps/controller-web/dist` 是 **vite 生成的产物**，整个目录被 git 忽略：前端在本地、
CI 与 Debian 打包时各自编译，仓库不再保存固定哈希的提交产物，任何地方也都不写死
生成文件的文件名与 hash。

工具链统一用当前 stable Rust（本地以 rustc 1.99.1 验证），**不再锁定最低版本**：
manifest 里没有 `rust-version`，Makefile 与 CI 也都不再有单独的低版本验证作业。

改过 `apps/controller-web` 源码后没有额外要提交的东西：`make web-build` 重新生成
`dist`（下次跑编译类目标时 `web-dist-check` 也会自己补上），产物与 stamp 都在忽略
列表里，永远不出现在 diff 中。`make web-build` 是显式的重建入口，用**两个** stamp 分层：

| stamp | 何时重新生成 | 配方 |
| --- | --- | --- |
| `apps/controller-web/node_modules/.rsetup-npm-ci-stamp` | `package.json` 或 `package-lock.json` 变化 | `npm --prefix apps/controller-web ci $(NPM_CI_FLAGS)` |
| `apps/controller-web/dist/.rsetup-build-stamp` | `index.html`、`vite.config.ts`、`tsconfig*.json`、`apps/controller-web/src/` 下的非测试文件（src 目录本身也是先决条件，新增/删除文件才会失效）或 Vite 的 `public/` 目录变化 | `npm --prefix apps/controller-web run build` |

分层正是要点：改一个组件只会重跑 `vite build`，绝不再跑 `npm ci`；`*.test.ts` 与
`*.test.js` 不是 `vite build` 的输入，改测试文件同样不会重建 `dist`。两个 stamp 文件
都落在被 git 忽略的目录里（`node_modules/` 与 `dist/`），这也是 dist 永远不会出现在
diff 里的原因。

构建前端这一步才需要 Node 22 与 npm（冷缓存首次执行还需网络）。两条离线路子：

```bash
# 1. 本机 npm cache 已预热：把参数直接透传给 `npm ci`。
make web-build NPM_CI_FLAGS=--offline

# 2. 以其他方式准备好了 node_modules/：自己补上两个 stamp 让 make 不再重建。
#    make 只比对 stamp 的时间戳，不看目录本身。
touch apps/controller-web/node_modules/.rsetup-npm-ci-stamp
touch apps/controller-web/dist/.rsetup-build-stamp
```

只把 `node_modules` 拷过来而不补 stamp 是无效的：make 仍认为先决条件更新，照样去跑
`npm ci`，而这正是离线环境会失败的一步。这两条同样能满足 `web-dist-check`：机器没有
网络时，只要 `dist` 已在位，编译类目标就不会去碰 npm。

`make metrics-static-gates` 只跑指标线的只读 grep 静态门禁：不编译、不联网、不需要前端
产物，CI 可单独调用。其中「不得提交配置实体/凭据目录」这条查**两份**清单——暂存区
（`git diff --cached --name-only`）与已跟踪文件（`git ls-files`）：CI 是干净检出，暂存区
为空，只查暂存区的门禁在那里等于什么都没看。`make metrics-gates` 在其之外再跑
`fmt-check`、前端产物检查（`web-dist-check`，dist 缺失时会自动构建）、clippy 与
controller 测试。

Rust 格式检查请使用 `make fmt-check`，不要使用 `cargo fmt --all`，因为后者会把第三方 submodule `externals/devicesinfo` 一并纳入检查范围；详见 [AGENTS.md](AGENTS.md) 的「Rust 格式检查口径」。

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

## 控制器

`rsetup-controller` 是独立的 HTTP 控制面可执行文件，与 `rsetup-next` 的 CLI/TUI/server
分开，并以 `rsetup-controller` 之名独立成包（与 `rsetup-next` 同一源码包，安装方式见下文
「从 Debian 软件包安装」）。它作为 workspace 成员构建（编译期读取的前端 `dist` 缺失时由
`web-dist-check` 自动构建，无需记住额外的步骤，详见「构建与运行」），启动方式：

```bash
cargo run -p rsetup-controller
```

必需环境变量，缺失或为空即退出，不会降级启动：

| 变量 | 作用 |
| --- | --- |
| `CONTROLLER_DATABASE_URL` | 存放 controller schema 的 MySQL/TiDB 连接串。 |
| `CONTROLLER_ALLOWED_HOSTS` | 精确匹配的逗号分隔 `Host` 白名单。 |
| `CONTROLLER_ALLOWED_ORIGIN` | 唯一精确匹配的浏览器 `Origin`。 |
| `CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX` | 十六进制编码的管理 cursor 密钥。 |

可选变量：`CONTROLLER_LISTEN_ADDRESS`（默认 `127.0.0.1:8080`）、
`CONTROLLER_TRUST_TLS`（只有取值恰好是 `1` 或 `true` 才给会话 Cookie 追加 `Secure`，用于由
反向代理终止 TLS 的部署；不设置或为空即不加，其他取值属于启动错误）、
`CONTROLLER_BOOTSTRAP_SECRET_LOG`（接收一次性管理员凭据的私有文件路径，默认是相对于工作
目录的 `controller-bootstrap-secret.log`）、`NTP_SERVERS`（逗号或空白分隔的 NTP 源，供启动
时间门禁采样；不设置即回退系统时钟，任务线会持续报告时间证据不可信）。

指标线另读三个：`CONTROLLER_CONFIG`（配置文件路径，默认 `controller.toml`，其中保存抓取
凭据，必须留在仓库之外——静态门禁会拒绝被跟踪的 `controller.toml` 或 `secret/` 条目）、
`PROMETHEUS_AUTH_USER` 与 `PROMETHEUS_AUTH_PASSWORD`（覆盖 Basic 认证身份，必须成对给出，
只给一个即启动错误）。以上只列常用项，完整配置约定见
[指标端点设计文档](docs/superpowers/specs/2026-10-19-prometheus-metrics-endpoint-design.md)。

默认只绑定回环地址，且 HTTP 面按受控可信管理网络设计：不适用于暴露给不可信网络或直接
接入互联网，进程自身也不终止 TLS。系统不预置任何凭据：首次针对未初始化 schema 启动时，
进程生成一次性管理员凭据并写入上述私有文件（以 `0600` 与 `O_EXCL` 创建，父目录不受保护，
因此必须在私有目录下启动进程）。该文件读取一次、登录后立即改密并删除。

`rsetup-admin` 是内置管理员账号的本机应急通道：不接受任何参数，向控制终端签发一次性新
凭据并留下审计事件。它的门禁是 OS 的真实/有效 UID，加上 stdin、stdout、stderr 各自都是
真实 TTY 且 `/dev/tty` 可写。因此非交互的 SSH 单命令执行、cron 与管道都无法驱动它；而
交互式 `ssh -t` 会分配 pty，同样的 root+TTY 检查会像在本地键盘前一样通过，仍受同一道门禁
约束。门禁卡的是进程如何连接终端与权限，不是「必须坐在机器前」。

### 从 Debian 软件包安装

`rsetup-controller` 二进制包由本源码包构建。它的构建步骤会先用 npm 现生成前端
（`debian/rules` 里的 `npm --prefix apps/controller-web ci` 与
`npm --prefix apps/controller-web run build`），再进入 cargo：`build.rs` 编译期读取的
`apps/controller-web/dist` 不在源码包里，只有 Rust 侧保持离线。包内容：

| 路径 | 用途 |
| --- | --- |
| `/usr/bin/rsetup-controller` | 控制面进程（单元 `rsetup-controller.service`） |
| `/usr/bin/rsetup-controller-migrate` | 显式 schema 迁移，`--check` / `--apply` |
| `/usr/bin/rsetup-admin` | 本机 root 管理员应急恢复（cargo bin 名与交付名一致；源文件 `src/bin/reset-admin.rs`） |
| `/etc/default/rsetup-controller` | `EnvironmentFile` 模板，0644，不含任何秘密 |
| `/usr/lib/systemd/system/rsetup-controller.service` | 单元：`User=rsetup-controller`（静态系统账户）、`StateDirectory=rsetup-controller` |
| `/usr/share/doc/rsetup-controller/README.Debian` | 完整的安装后操作步骤 |

单元显式降权到静态系统账户 `rsetup-controller`（`User=`/`Group=`）：该账户与同名私有组由
包安装脚本以 `adduser --system --group --home /var/lib/rsetup-controller --no-create-home
--disabled-login` 幂等创建，uid/gid 一经分配不再变化，`/var/lib/rsetup-controller` 的属主
因此跨重启、跨升级保持稳定（0700，`rsetup-controller:rsetup-controller`），备份、rsync 与
ACL 策略都有稳定锚点。不再使用 `DynamicUser=yes`：动态用户每次启动换一个临时 uid/gid，
systemd 每次都要重新 chown 状态目录，持久状态的属主随重启漂移，事后取证与授权策略失去锚点。
安装脚本只建账户，不启动也不 enable 任何服务。

软件包既不 enable 也不启动服务：`dh_installsystemd --no-enable` 让单元不进启动路径，单元
自身的 `ConditionPathExists=/etc/default/rsetup-controller` 又保证未写配置时保持 inactive，
因此装完包只是一堆未启动的文件。后续步骤一律由操作员执行，且没有任何一步已代表你对真实
数据库做过：

1. 填写 `/etc/default/rsetup-controller`：`CONTROLLER_DATABASE_URL`、
   `CONTROLLER_ALLOWED_HOSTS`、`CONTROLLER_ALLOWED_ORIGIN`、
   `CONTROLLER_MANAGEMENT_CURSOR_SECRET_HEX`，随后 `chmod 0600`。随包的模板之所以是
   0644，正因为它不含秘密。
2. 先对目标库做一份**真实可恢复**的备份，再在 `CONTROLLER_DATABASE_URL` 旁导出四个迁移
   门禁：`CONTROLLER_MIGRATION_ALLOW_DESTRUCTIVE=1`、
   `CONTROLLER_MIGRATION_EXPECTED_DATABASE`（必须等于服务端真实返回的库名）、
   `CONTROLLER_MIGRATION_BACKUP_REF`（指向该备份的引用）、
   `CONTROLLER_MIGRATION_ACK=isolated-exclusive-backed-up-disposable`。这四项是操作员的
   显式确认，工具不会替你猜：声明库名与真实连接不一致即 fail-closed。
3. 先 `rsetup-controller-migrate --check`（只读，只打印 `schema_version=<数字>` 或
   `schema_version=missing`），再 `rsetup-controller-migrate --apply`。已是受支持版本只做
   只读一致性复核（幂等）；空库或 v1/v2/v3 先升到 v3 再升到 v4；未识别版本直接拒绝。
   它不需要 root（写权限来自连接串里的数据库账号），也从不读写 `/dev/tty`。
4. `systemctl start rsetup-controller`（需要在开机拉起时再 `enable`）。
5. 首次启动把一次性管理员凭据写到
   `/var/lib/rsetup-controller/bootstrap-secret.log`（`0600`、`O_EXCL`）；读取一次、登录
   改密后删除该文件。
6. 管理员账号遗失或死锁时，在中控主机本机以 root 交互式终端执行
   `rsetup-admin`。
7. 默认只监听 `127.0.0.1:8080`；要跨机访问必须经反向代理 + TLS + 可信管理网，并让
   `CONTROLLER_ALLOWED_HOSTS`/`CONTROLLER_ALLOWED_ORIGIN` 与反代实际转发的值一致。

尚未接线的部分：transport 线（设备实时缓存、恢复控制器、准入通知、板端任务客户端）以
fail-closed 装配位接入，只报告「not wired」或设备离线，不伪造取值；SSE 事件总线除心跳与
总线自身的 reset 外还没有业务发布方；设备快照源还没有写入方。这些未接线端点在当前版本
返回 503 且错误码为 `STORAGE_UNAVAILABLE`（装配位以配置错误形态上抛，错误文案一律不回显），
或返回结构合法的「无样本」响应体（`snapshot: null`、`freshness: "unknown"`、
`age_ms: null`），而不是编造读数。`NOT_READY` 表示的是 schema 与加密能力未就绪，未接线的
transport 装配位不会报这个码。接真实板卡运行前，完整清单见
`docs/testing/controller-v1-known-gaps.md`。

### 板端代理本地测试

`crates/rsetup-board-agent` 现在提供可在 amd64 本机运行的测试二进制：走真实 typed dispatch
（`/proc` 的 capabilities/status/clock、journal 查询、错误映射）并提供回环 JSON-lines 控制台；
它**不是**生产加密隧道，也绝不执行真实主机重启。

```bash
cargo build -p rsetup-board-agent --release --bin rsetup-board-agent
./target/release/rsetup-board-agent --once --journal-dir /tmp/rsetup-board-agent-journal
./target/release/rsetup-board-agent --watch --interval-secs 5 --journal-dir /tmp/rsetup-board-agent-journal
./target/release/rsetup-board-agent --loopback-test-server --listen 127.0.0.1:39876 \
  --journal-dir /tmp/rsetup-board-agent-journal
```

JSON-lines 协议与边界见 [crates/rsetup-board-agent/README.md](crates/rsetup-board-agent/README.md)。

板侧交付与运行用户：`rsetup-board-agent` 是同源码包构建的独立 Debian 包，装
`/usr/bin/rsetup-board-agent` 与单元
`/usr/lib/systemd/system/rsetup-board-agent.service`（`dh_installsystemd --no-enable`，默认
不启用）。单元固定以 `User=root`/`Group=root` 运行只读采样 watch 循环
（`--interval-secs 10 --journal-dir /var/lib/rsetup-board-agent`，状态目录由
`StateDirectory` 建为 0700）：整机硬件与系统状态来源并非一律对非特权用户可读，因此板端的
生产与测试部署口径统一为 root，配合 `ProtectSystem=strict`、`NoNewPrivileges=yes`、
`PrivateTmp=yes`、`ProtectHome=yes`、`LockPersonality=yes` 与唯一 `ReadWritePaths` 的沙箱
限制运行。手工在 shell 里运行不做任何提权，仍以调用者身份执行。**边界未变**：生产加密隧道
尚未接线，该单元启动的是只写本地 journald 的采样循环，不与中控建立任何加密会话；
`device.reboot.prepare` 与 `device.reboot.execute` 一律返回 capability unavailable，真实重启
边界仍未实现。

支持并按 CI 验证的架构为 `arm64`（aarch64）与 `amd64`（x86_64），不再维护 32 位目标。

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

同一源码包另会构建两个包。`rsetup-controller`：控制面进程、两个操作员 CLI
（`rsetup-controller-migrate`、`rsetup-admin`）、一个 systemd 单元与
`/etc/default/rsetup-controller` 模板；单元降权到安装时创建的静态系统账户
`rsetup-controller`。它装了什么、以及操作员仍需亲自执行的迁移与配置步骤，
见「控制器」章节下的「从 Debian 软件包安装」。`rsetup-board-agent`：板端代理二进制与
自己的 systemd 单元，以 root 运行只读采样 watch，生产加密隧道与真实重启尚未接线。
三个包的构建都需要 Node.js 与 npm（两者都在 `Build-Depends` 里）：`debian/rules` 在
cargo 之前用 `npm ci` + `npm run build` 现生成 controller 嵌入的前端产物，Rust 侧才保持
`CARGO_NET_OFFLINE` 与包内 Cargo 缓存离线。两个 systemd 单元都以关闭状态
安装（`dh_installsystemd --no-enable`），controller 单元另外在未写配置文件时自身拒绝启动，因此装包不拉起任何
服务、也不触碰任何数据库。

该软件包依赖 `xz-utils`：写入压缩的 `.img.xz` 系统镜像时通过 `xz -dc` 解压。选择硬依赖
而非 `Recommends` 的取舍，以及缺少该工具时的回退行为，记录在
[`debian/README.source`](debian/README.source)。系统镜像指南随包安装到
`/usr/share/doc/rsetup-next/system-install.md`。

```bash
make deb-prepare
make deb
```

`deb-prepare` 将锁定版本的 Rust 依赖下载到被忽略的、软件包本地的 Cargo 缓存中。随后的 `dpkg-buildpackage` 步骤以离线模式运行 Cargo。生成的软件包仅通过本仓库维护和分发；不打算提交至 Debian 归档。

软件包在 Debian 13/Trixie 上以其当前 stable Rust 工具链构建，然后在 Debian 12/Bookworm 上安装并冒烟测试。Bookworm 是运行时兼容基线；其 Rust 1.63 工具链不用于源码构建。CI 同时校验 amd64 和 arm64 软件包。

`ui/assets` 下的 SoC 厂商标识仍归其各自所有者所有。它们仅用于设备厂商识别，不会按本项目的 GPL-3+ 许可证重新授权。

规范化的 20 个 profile 的 GPIO 目录派生自 [`xzl01/pin-out`](https://github.com/xzl01/pin-out)。其版权所有者已授权将转换后的快照以 `GPL-3.0-or-later` 分发；确切的源提交与再生成说明见 [`data/PINOUT_PROVENANCE.md`](data/PINOUT_PROVENANCE.md)。额外的 Q8B profile 派生自 Radxa 的 GPIO 文档，其 CC-BY-4.0 署名与许可证单独保留。

## 硬件验证边界

Rust workspace、demo provider、CLI、HTTP API 和浏览器 GUI 可以在开发主机上测试。Q8B 实时只读探测已在 Ubuntu 26.04.1（UEFI + DT）上验证；结果与限制见[测试报告](docs/testing/q8b-2026-09-07/report.md)。其他受支持的 SBC 仍需要实时验证。把系统镜像写入真实 UFS/eMMC/SD/NVMe 存储的功能已实现，并由注入式夹具与预演测试覆盖，但尚未在任何物理板卡上执行过写入；完整命令序列、期望输出、失败判据以及"已本地验证／待真机验证"的划分见 [docs/system-install.zh.md](docs/system-install.zh.md)，主机侧证据见[本地验证报告](docs/testing/system-install-2026-10-13/report.md)。设备级检查无法在 CI 中执行，也不作为 CI 门禁。overlay 事务、Overlay-to-Pinout 映射、真实摄像头采集、sysfs thermal/LED 写入、启动时恢复以及带备份的 SPI NOR 操作均已实现，但尚未在物理硬件上执行过。特别是，温度驱动的曲线尚未用真实的 Linux SBC `pwm-fan` 冷却设备进行物理验证。Overlay 文件名和 mux 模式仍需在受支持的 Pinout profile 范围内对照真实软件包进行验证；未知的通用 pin header 有意保持未分配。Bootloader、SPI/eMMC、GPIO、overlay、thermal、网络以及改变电源的操作必须在可恢复的硬件上逐一测试之后，才能被视为生产可用。

## 许可证

GPL-3.0-or-later，与上游项目保持一致。

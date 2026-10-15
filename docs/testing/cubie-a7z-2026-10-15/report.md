# 真机只读验证报告 — Radxa Cubie A7Z

日期：2026-10-15。测试对象：`rsetup-next` 0.5.0 的 **CI 构建 arm64 产物**
（`rsetup-next-debian-arm64`，解包后直接运行，**未安装**）。
范围：在真实板卡上复核 `system targets/plan/install` 的归属判定与闸门，以及
`status`/`hardware *`/`sources`/`doctor` 等**只读面**的结果是否与内核自身视图一致。

> **本报告不含任何真实块设备写入。** 全程未使用 `--live-execution`、未使用 root、
> 未执行 `dpkg -i`，因此不能作为"写盘功能验收"依据；写盘路径仍属未验证边界（见文末）。

## 结论

结论分三类：**已实测通过**、**本次发现并已修复**、**观察项（未改，取决于设计意图）**。

**已实测通过。** 设备归属、运行盘拒绝、压缩镜像载荷校验、令牌绑定、干跑如实汇报，以及
八个只读面与内核视图逐条一致。其中两处是量化结论而非"看起来对"：

- `status` 的一次性 CPU 采样在真机上是**真实测量**：空闲 0.0%，8 核中压满 4 核时为
  50.3–50.9%（4/8 = 50%），温度同步 37.6 °C → 51.0 °C。
- `sda/sdb/sdc` 的 `mounted` / `holdsRunningSystem` / `mountPoints` 与 `lsblk`、`findmnt`、
  `/proc/self/mountinfo` 三方交叉比对**完全吻合**，且对运行盘的写入被
  `writes_running_system` 拒绝。

**本次发现并已修复（2 项，均带反向验证的回归测试）。**

1. 干跑（不带 `--live-execution`）把"只统计了字节数"汇报成 `status: succeeded` +
   `summary: "Wrote 512 of 512 bytes to /dev/sdc."`。已在 core/TUI/Web 三端改为如实汇报
   （`dryRun`、`planned`）。见 §6。
2. `serial_console_detected` 用硬编码 SoC 前缀白名单匹配 `console=`，本板
   `console=ttyAS0,115200n8`（Allwinner）不在名单内，于是**内核正在使用串口控制台却报告
   没有**。已改为按名字形态判断。见 §8。

**观察项（未改）。** `sources status` 只列出 5 个文件，而 `/etc/apt/sources.list.d/` 有 7 个：
漏掉 `50-bullseye-backports.list`（`archive.debian.org`）与 `local-apt-repository.list`
（`file://`）。查源码确认这是**按主机名识别受管仓库**的设计（只认
`deb.debian.org` 与 `radxa-repo.github.io`），不是缺陷；但实际影响是换镜像时 backports
不会一起换。是否纳入属实现决策。见 §7.8。

## 与既有记录的关系

- `docs/testing/system-install-2026-10-13/report.md` 覆盖的是**主机侧**（demo + 预演），
  其正文明确写着"尚未在 Radxa Cubie A7Z（或任何板卡）上做过真实写入"。
- 此后 2026-10-14 已在**本板**上做过真实写入：`docs/system-install.md` 的验证表记录，
  一枚 10 GiB 镜像写入这颗 119.1 GiB UFS（4096 字节扇区）后板卡**从该 UFS 启动**，
  `sda3` 由 9.5G 扩到 118.6G。
- **本次只读会话独立印证了那一结果**：`findmnt` 显示当前 `/` 就在 `/dev/sda3`
  （即那颗 UFS 的 LUN0），而非 SD 卡。也就是说本板当前的启动配置本身就是 2026-10-14
  那次写盘有效的旁证。
- 本记录是**只读补充**：不重复上述写入结论，只补充在被审 revision（含挂载归属修复与
  干跑汇报修复）上的归属判定、闸门与只读面对账。按既有格式要求，真实写入类记录须含
  "镜像名与 SHA-256"；本记录**不含**，因为它没有使用真实发行镜像，只用 512 B 合成夹具。

## 测试基线

- 板卡：Radxa **Cubie A7Z**（`sun60iw2`，Allwinner A733），aarch64，8 核。
- 系统：Debian GNU/Linux 11 (bullseye)，kernel `5.15.147-21-a733`，hostname
  `radxa-cubie-a7z`，测试时 uptime 约 23 小时、load 0.01–0.05。
- 存储：单颗 YMTC UFS（`YMUS8A1TE2D1C1`，走 `4520000.ufs` 控制器）暴露三个 LUN —
  `sda`（LUN0，119.1 GiB，含 `/` `/boot/efi` `/config`）、`sdb`（LUN1，16 MiB）、
  `sdc`（LUN2，16 MiB）；另有 `zram0` swap。**注意 `sdb`/`sdc` 是同一颗 UFS 的其它 LUN，
  不是独立磁盘**（见"方法论"一节）。
- 产物来源：GitHub Actions run `38702069043` 的 `rsetup-next-debian-arm64` 工件。
  先在本机 `ar x` + `tar xf data.tar.xz` 解包，仅取
  `usr/bin/rsetup-next` 与 `usr/libexec/rsetup-next-helper` 复制到板卡 `/tmp/rsetup-test/`
  运行；**未执行 `dpkg -i`**，`rsetup-next` 包至今未安装（板卡上只有系统自带的旧版
  `rsetup` 0.4.27）。测试后已删除全部 `/tmp` 产物。
- 产物自证：`file` 显示 `statically linked`；`strings` 能查到本次修复相关的字符串
  （`could not be resolved to a block device`、`nothing was written (dry run)` 等），
  确认测的就是被审的那份代码。
- 权限：以 `uid=1000(radxa)` 运行，不在 `disk` 组；`/dev/sd*` 为
  `brw-rw---- root disk`，该用户**连读都被拒**（`od: Permission denied`）。
- 报告对应的提交：`0e74ec2`（串口控制台）、`43a2ba8`（rustfmt）、`01a5e80`（干跑汇报）。
  该 revision 上本地：Rust 289 passed / 2 ignored、UI 101 passed、桌面 19 passed、
  `fmt-check` 与 `clippy -D warnings` 均通过；CI run `38704085313` 11/11 通过。

## 已完成验证（实测输出）

### 1. CPU 首采样是真实测量

`status` 是完全独立的一次性进程，加 200 ms 基线窗口后第一次调用就应给出真实利用率。
在 8 核板子上用 4 个忙循环制造 50% 负载：

```console
$ /tmp/rsetup-test/rsetup-next status
CPU   0.0%   MEM  18.5%   TEMP 37.4°C          # 空闲

$ for i in 1 2 3 4; do (timeout 5 sh -c "while :; do :; done") & done
$ /tmp/rsetup-test/rsetup-next status
CPU  50.9%   MEM  18.5%   TEMP 42.8°C
CPU  50.3%   MEM  18.5%   TEMP 48.7°C
CPU  50.3%   MEM  18.5%   TEMP 51.0°C
CPU   0.0%   MEM  18.5%   TEMP 44.4°C          # 停载后回落
```

4/8 = 50%，读数与真实负载吻合；修复前该字段恒为"不可用"。

### 2. 设备归属三方交叉比对

```console
$ /tmp/rsetup-test/rsetup-next system targets --json
sda  /dev/sda   127884328960  mounted=True   holdsRunningSystem=True   ['/', '/boot/efi', '/config']
sdb  /dev/sdb   16777216      mounted=False  holdsRunningSystem=False  []
sdc  /dev/sdc   16777216      mounted=False  holdsRunningSystem=False  []
```

内核侧事实：`/proc/mounts` 为 `/dev/sda3 /`、`/dev/sda2 /boot/efi`、`/dev/sda1 /config`；
`/proc/self/mountinfo` 给出权威映射 `8:3 / / /dev/sda3`、`8:2 / /boot/efi /dev/sda2`、
`8:1 / /config /dev/sda1`。逐条吻合，`zram0` 作为虚拟设备被排除。

### 3. 运行盘被拒绝，空闲盘放行

```console
$ /tmp/rsetup-test/rsetup-next system install --target sda --image /tmp/x.img --acknowledge sda --json
{"error":{"code":"writes_running_system","message":"refusing to write /dev/sda: it hosts the running root or boot filesystem"}}
exit=1

$ /tmp/rsetup-test/rsetup-next system install --target sdc --image /tmp/x.img --acknowledge sdc --json
exit=0   # 进入计划/预演
```

### 4. 压缩镜像在计划期就校验载荷

```console
$ xz -1 -c /tmp/notes.txt > /tmp/notes.txt.xz     # 92 B 的合法 xz 容器
$ /tmp/rsetup-test/rsetup-next system plan --target sdc --image /tmp/notes.txt.xz --json
{"error":{"code":"invalid_install_request","message":"invalid system install request: the image is smaller than one sector and cannot hold a partition table"}}
exit=1

$ xz -1 -c /tmp/x.img > /tmp/x.img.xz             # 88 B，真 MBR 镜像
$ /tmp/rsetup-test/rsetup-next system plan --target sdc --image /tmp/x.img.xz --json
  format            : xz
  sizeBytes         : 88 (compressed)
  uncompressedBytes : 512
  partitionTable    : mbr
  partitions        : [{'name': 'p1', 'number': 1, 'startBytes': 1048576, 'sizeBytes': 524288}]
```

即：容器合法不等于磁盘镜像合法；真镜像则解压后解析出载荷分区表。本板 `xz` 为
Debian 11 自带版本，故同时也验证了与真实解压器的互通。

### 5. 令牌绑定文件身份

2 MiB 镜像 → 取计划令牌 → 在 **1.5 MiB**（超出 1 MiB 摘要范围）改 1 字节、长度不变 →
重新取令牌：

```console
token before edit : install-plan-2305f2a073abc9e5-006a...
token after  edit : install-plan-eb45d6be62932c05-006a...
```

令牌改变，说明预览之后的越界修改会使旧计划失效。

### 6. 干跑不再谎报写入（本次修复项）

修复前（同一命令、同一板卡）：

```json
{ "dryRun": <字段不存在>, "run": { "status": "succeeded", "synthetic": false,
  "summary": "Wrote 512 of 512 bytes to /dev/sdc." } }
```

修复后：

```console
$ /tmp/rsetup-test/rsetup-next system install --target sdc --image /tmp/x.img --acknowledge sdc --json
  dryRun       : True
  run.status   : planned
  run.synthetic: False
  run.summary  : Planned 512 bytes for /dev/sdc; nothing was written (dry run).
  bytesWritten : 512
```

`bytesWritten` 保留为"计划写入量"，`synthetic: false` 仍然是正确的（目标是真的，只是没写）。
`--live-execution` 缺失时后端为进程内干跑包装，`write_chunk` 只在内存计数、从不打开设备。

### 7. 只读面与内核视图逐条对账

| 面 | 结论 | 关键证据 |
| --- | --- | --- |
| overlays | 一致 | 无 `/sys/firmware/efi`、存在 `/boot/uEnv.txt` 与 `/boot/extlinux` ⇒ `bootloader: u-boot`；`directory: /boot/dtbo` 真实存在；22 个 overlay **全部 `enabled: false`**，与磁盘上**每个文件都带 `.disabled`** 一致；`mutable: false` 与 `/boot/dtbo` 属 `root root` 一致 |
| thermal | 一致（8/8） | 8 个 zone 的 id、kind 与温度逐条对上（2 个差 0.1 °C 为采样时间差）；`currentPolicy: power_allocator` == `/sys/class/thermal/thermal_zone0/policy`；`availablePolicies` 与 `available_policies` 同集合 |
| fan-curve | 一致 | `pwmFanDetected: true`；4 个 cooling device 的 id/kind/currentState/maxState 与 sysfs 全部一致（含 `cooling_device3 = pwm-fan, cur=4, max=4`） |
| leds | 一致 | 仅 `radxa:blue:user`：`brightness=0`、`maxBrightness=1`、`currentTrigger=heartbeat` 与 sysfs 完全一致；`supportsPattern: true` 与 `pattern` 出现在 `availableTriggers` 中自洽 |
| spi-flash | 一致（诚实） | `/proc/mtd` 无设备行、`/sys/class/mtd` 不存在 ⇒ `supported: false` + `"No SPI NOR MTD device was detected."`（本板为 UFS，本无 SPI NOR） |
| mmc | 一致（诚实） | 有 `mmc0/1/2` **控制器**但无任何 `mmcblk*` 块设备 ⇒ `devices: []` + `"No MMC/SD devices detected in system"` |
| video | 一致 | 无 `/dev/video*`、无 v4l 设备 ⇒ `captureAvailable: false`、`supported: false` + `"No video capture device was detected."` |
| sources | 一致（附观察项） | `distributionName: "Debian GNU/Linux 11 (bullseye)"`、`architecture: aarch64`、`currentSystemProvider/currentRadxaProvider: official`，与实际启用的 `deb.debian.org` 与 `radxa-repo.github.io` 相符；`providers: 10` 为可选镜像列表 |
| gpio | 一致 | `boardName: "Cubie A7Z"`、`layout: 40-pin`、`pins: 40`、chips `gpiochip0/1` 与 `gpiodetect`（352 + 64 lines）一致；pin1 为 3.3V；`configuredOverlays: []` 与 overlays 面一致 |
| doctor | 一致 | 返回 4 条（`probe`/`execution`/`native-actions`/`device-tree`），exit 0，JSON 可解析 |

注：全部只读命令 exit=0 且 stdout 为单个可解析 JSON 文档；人类可读预览一律在 stderr。

### 8. 串口控制台检测（本次修复项）

```console
$ /tmp/rsetup-test/rsetup-next hardware gpio --json | ... serialConsoleDetected
  serialConsoleDetected: False
$ grep -o "console=[^ ]*" /proc/cmdline
console=tty1
console=ttyAS0,115200n8
```

内核同时在用虚拟控制台与 **串口控制台**（`ttyAS0`，Allwinner 命名）。修复前
`serial_console_detected()` 用 `starts_with` 白名单匹配 `ttyS` / `ttyAMA` / `ttyAML` /
`ttyFIQ` / `ttyMSM`，`ttyAS` 不在其中，故报告 `false`。

修复后改为按形态判断：`console=tty` 后跟**字母**为串口（`ttyS0`/`ttyAMA0`/`ttyAS0`/`ttyUSB0`），
后跟**数字**为虚拟控制台（`tty0`/`tty1`）——正是原测试用 `console=tty0` 已断言的区分。
回归测试覆盖 8 组取值；改回白名单后该测试在**本板真实 cmdline** 上失败
（`left: false, right: true`）。

注：该标志目前尚未被 UI 消费，仅出现在 JSON 契约中，故严重度不高；但它是给脚本与前端
的公开契约。

### 9. 自动化测试（该 revision）

`cargo test --workspace --locked`：201 + 83 + 3 + 2 passed / 2 ignored / 0 failed；
`node --test ui/*.test.mjs`：101 passed；桌面 `cargo test --locked`：19 passed。
本次新增/修改的用例均做过**反向验证**（先确认去掉修复即失败）：
干跑汇报、干跑 Web 渲染、跨磁盘挂载归属、替换窗口写盘、串口控制台形态。

## 未完成边界（仍未验证）

- **真实设备写入**。未执行；本轮所有 install 均在无 `--live-execution` 的干跑路径上。
  写盘路径（真实 `write_chunk`、`O_EXCL`、`fsync`、`BLKRRPART` 重读与写后校验文案）
  在本材料中依然**未经任何真机执行**。
- **P1-2 的跨进程写锁**。需要两个真实写入进程竞争同一目标才能验证；干跑后端不提供锁根
  （`lock_root()` 为 `None`），因此本轮的干跑路径不触发它，仅有单元测试覆盖。
- **32 位目标的长度截断修复**。本板为 aarch64，无法复现 32 位场景；仅有单元测试与
  i686/armv7 交叉编译覆盖。
- **EFI 体系下的 overlay 授权路径**。本板为 U-Boot（无 `/sys/firmware/efi`），
  `requiresAuthorization` 的 EFI 分支未在此板出现。
- **桌面端与浏览器端在真机上的操作**。本轮只用 CLI。
- **`sources` 的镜像切换写入**。只读；未实际切换过镜像源。

## 本次发现

1. **干跑谎报写入**（已修，`01a5e80`）：`status: succeeded` +
   `"Wrote 512 of 512 bytes to /dev/sdc."`，而设备从未被打开。这是**在真机上运行修好的
   二进制时才暴露的**——本地单元测试从未覆盖干跑结果，因此长期未被发现。
2. **串口控制台白名单**（已修，`0e74ec2`）：`console=ttyAS0` 不被识别。与 P1-1 挂载源
   匹配同属一类错误：用"想得到的名字清单"代替形态/结构判定。
3. **`sources` 的受管范围**（未改，观察项）：只列 5 个文件，漏掉 backports
   （`archive.debian.org`）与 local-apt（`file://`）。按主机名识别受管仓库属设计，
   但换镜像时 backports 不会被一并切换。

## 方法论：两次差点误报

本项目已多次出现"凭字段/命名猜测就下结论"的错误，本轮的两次都靠**先取地面真值**才避免：

1. **`sdb`/`sdc` 被标成 UFS**，一度怀疑误分类。查
   `readlink -f /sys/class/block/sdc` 发现它们在 `4520000.ufs/host0/.../0:0:0:2` 下，是
   **同一颗 UFS 的 LUN1/LUN2**，标 UFS 是**正确的**。
2. **thermal 探针全 `None`**，一度像 bug。实际是我用猜的字段名
   （`name`/`temperatureCelsius`/`state`）去读，真实字段是 `id`/`kind`/`temperatureC`；
   改成正确字段后 8/8 吻合。**是探针错，不是产品错。**

结论：真机对账的每一步都应先取内核侧事实（`/proc`、`/sys`、`lsblk`、`findmnt`、
`gpiodetect`），再与工具输出比对；仅凭 JSON 字段缺失或命名异常不足以判定缺陷。

## 安全声明（非破坏性证据）

- 未执行任何写入：无 `--live-execution`、无 root、无 `dpkg -i`。
- 进程以 `uid=1000(radxa)` 运行且不在 `disk` 组；`/dev/sd*` 为 `brw-rw---- root disk`，
  该用户连读都被拒绝（上文 `e3b0c442…` 是**空输入的 SHA-256**，即读取被拒，而非全零扇区）。
- 干跑后端的 `write_chunk` 只在内存计数，从不打开设备；`reread_partitions` 直接返回
  Unsupported。
- `/tmp` 产物已全部删除；`rsetup-next` 包未安装；挂载点与 uptime 未变；无残留进程。

## 备注（环境，非产品问题）

- 板卡会对**快速连续的 SSH 连接**限流，出现间歇 `Permission denied`；实测需间隔
  30–45 秒并把命令合并进单个会话。
- `xz -9 -T0` 会打满该板内存（`Cannot allocate memory`），用 `xz -1` 即可。
- 该主机 `known_hosts` 中 `192.168.8.165` 的旧 ECDSA 主机密钥已失效，板卡现为 ED25519
  （`SHA256:nCJ6DfrWxmtEC9U/nbyxfQfvZWJWaE8MCDzmyfr4AIs`）。本轮使用隔离的 known_hosts
  文件，**未修改**操作者的 `known_hosts`。

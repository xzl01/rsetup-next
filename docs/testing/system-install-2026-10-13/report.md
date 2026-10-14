# 系统镜像安装 本地验证报告

日期：2026-10-13。测试对象：`rsetup-next system targets|plan|install`（CLI 与 core）。
范围：在开发主机上验证命令面、闸门、输入校验、`.img.xz` 预览与缺少 `xz-utils` 时的
可解释错误；**不包含任何真实块设备写入**。

> 本文是**按日期的历史记录**，正文保留当时观测到的原文，不做事后改写。其中若干结论已被
> 后续任务取代（尤其是 token 模型与压缩镜像的写后校验文案）；文末
> "后续修订（superseded）"一节逐条列明被取代的结论与取代者。

## 结论

主机侧（demo + 预演）全部通过，闸门与校验路径的退出码与错误码与设计一致；`.img.xz`
的预览不需要 `xz-utils`，写入缺少它时给出稳定错误码 `install_unsupported` 与可执行的
修复提示。**尚未在 Radxa Cubie A7Z（或任何板卡）上做过真实写入**，因此本报告不能作为
硬件验收依据。板上步骤见 [docs/system-install.md](../../system-install.md)（中文镜像
`docs/system-install.zh.md`）。

## 测试基线

- 主机：macOS 27.2（arm64），Rust/cargo 1.98.1，工作树 HEAD `04b3459`。
- 工作树包含尚未提交的 t11/t12 改动（`crates/rsetup-core`、`crates/rsetup-app`），
  以及并行进行的 t15（TUI 面板）改动，不是该提交的纯净构建。
- 二进制：`cargo build -p rsetup-next --locked` → `target/debug/rsetup-next`，
  `--version` 输出 `rsetup-next 0.5.0`。
- 夹具：`/tmp/rsn-doc/radxa-fixture-mbr.img`（4 MiB，一个 MBR 入口，type `0x83`，
  起始 LBA 2048、1024 扇区），其 LZMA 压缩版 `.img.xz`（768 B），以及全零
  `zero.img`。全部位于 `/tmp`，**不使用** root、loop 设备或真实 `/dev` 节点。

## 已完成验证（实测输出）

### 1. 候选设备列表

```console
$ ./target/debug/rsetup-next --demo system targets
/dev/mmcblk0 — eMMC AJTD4R eMMC · 58.00 GiB · 2 Existing partitions destroyed
/dev/mmcblk1 — SD card SD 32GB · 29.00 GiB · 2 Existing partitions destroyed · Removable media · Mounted /, /boot · Hosts the running system
/dev/nvme0n1 — NVMe Radxa M.2 NVMe 256GB · 238.00 GiB · 1 Existing partitions destroyed
/dev/sda — UFS RADXA UFS 128GB · 128.00 GiB · 3 Existing partitions destroyed
```

退出码 0。`--json` 输出同一组设备的 camelCase 对象（`kind`、`removable`、
`mountPoints`、`holdsRunningSystem`、`partitions`）。

### 2. raw 镜像预览

```console
$ ./target/debug/rsetup-next --demo system plan --target sda --image /tmp/rsn-doc/radxa-fixture-mbr.img
System image write preview
Target device: /dev/sda — UFS (RADXA UFS 128GB)
Capacity: 128.00 GiB
Existing partitions destroyed (3):
  - sda1 · 128.00 MiB
  - sda2 · 2.00 GiB
  - sda3 · 6.00 GiB
Image: /tmp/rsn-doc/radxa-fixture-mbr.img
  Format: Raw · File size: 4.00 MiB · Write size: 4.00 MiB
  SHA-256 (first 1 MiB): d77c1d40d6f20b154ca2b6586a8b7d26a92dd3115297b4b3d893b58caba92fdc
  Partition table: MBR
Administrator authorization required
! Writing replaces every partition on the target.
! This write cannot be rolled back.
```

退出码 0；`--lang zh-CN` 输出等价中文清单。

### 3. `.img.xz` 预览（不需要 xz 工具）

```console
$ ./target/debug/rsetup-next --demo system plan --target sda --image /tmp/rsn-doc/radxa-fixture-mbr.img.xz
  Format: Xz · File size: 768 B · Write size: 4.00 MiB
  SHA-256 (first 1 MiB): 2e835412001233bd54b817d2f388a596e1857afe6b817502ff038c8356b6a215
  Partition table: not read (compressed image)
Administrator authorization required
! Writing replaces every partition on the target.
! This write cannot be rolled back.
! The image is compressed and is decompressed while writing (requires xz).
```

退出码 0。写入大小（4.00 MiB）来自 `.xz` 索引，文件本身只有 768 B；该路径完全由纯
Rust 解析完成。

### 4. 预演不写盘

```console
$ ./target/debug/rsetup-next --demo system install --target sda --image /tmp/rsn-doc/radxa-fixture-mbr.img --acknowledge sda
…（预览同上）…
Dry run: nothing is written; add --live-execution to write.
Write complete
  Target device: /dev/sda
  Bytes written: 4.00 MiB
  Partition table verification: this host cannot re-read partition tables
  Synthetic target: no real device was written
```

退出码 0。预演路径与真实路径共用同一套计划与确认判定，只有写入与分区表重读被替换为
计数/不支持。

### 5. 闸门与失败的判据（stderr）

| 命令 | 退出码 | stderr |
| --- | --- | --- |
| `install … --acknowledge mmcblk0`（错名） | 1 | `Error: acknowledgement_mismatch: type "sda" to write /dev/sda; received "mmcblk0"` |
| `install … < /dev/null`（空回答） | 1 | `Error: confirmation_required: confirmation is required before writing a system image` |
| `install --target mmcblk1 … --acknowledge mmcblk1`（承载运行系统） | 1 | `Error: writes_running_system: refusing to write /dev/mmcblk1: it hosts the running root or boot filesystem` |
| `plan --image relative.img` | 1 | `invalid_install_request: … the image path must be absolute: relative.img` |
| `plan --image /tmp/rsn-doc/nope.img` | 1 | `invalid_install_request: … the image … is unreadable: No such file or directory` |
| `plan --image /tmp/rsn-doc/zero.img` | 1 | `invalid_install_request: … the image has no MBR or GPT partition table; it is not a bootable system image` |
| `plan --image /tmp/rsn-doc/link.img`（符号链接） | 1 | `invalid_install_request: … the image … is not a regular file` |
| `plan --target sdz` | 1 | `invalid_install_request: … unknown block device sdz` |
| `--live-execution install …`（macOS） | 1 | `Error: live execution is only supported on Linux SBC hosts` |

另有一条针对空回答的交互提示：stdin 不是终端时程序提示
`Standard input is not a terminal: pass --acknowledge <device>, or pipe the device name in.`
（提示在 stdout，失败仍在 stderr）。

### 6. 缺少 `xz-utils`

在 `PATH` 中移除 `xz`（`env -i PATH=/usr/bin:/bin`，该 PATH 下无 `xz`）后：

```console
$ env PATH=/usr/bin:/bin rsetup-next --demo system install \
    --target sda --image /tmp/rsn-doc/radxa-fixture-mbr.img.xz --acknowledge sda --json
…（预览，含 "requires xz" 警告）…
{"error":{"code":"install_unsupported","message":"system image installation is unavailable: install xz-utils to write a compressed image"}}
$ echo $?
1
```

即：预览阶段照常工作，只有真正开始写入时才因缺少工具而失败，且给出稳定错误码与可执行
的修复提示。这是包装层 `Depends: xz-utils` 之外的第二道防线。

### 7. 自动化测试

```console
$ cargo test -p rsetup-core --lib system_install -- --list
… 22 tests, 0 benchmarks
$ cargo test --workspace --locked
174 passed / 60 passed / 1 passed，0 failed（2 ignored）
```

（同一工作树早些时候的计数为 152 / 44 / 1；t11 的 22 个 `system_install` 用例与 t15 的
TUI 用例在此期间并行落地，计数增长与本报告结论无关。同日文档同步时重跑为
183 / 71 / 1，0 failed。）

22 个 `system_install` 用例覆盖注入根枚举、完整后果清单、确认/错名/布尔不可行、token
单次使用与过期、运行系统与已挂载闸门、MBR/GPT/xz 识别与损坏拒绝、分块字节一致、部分写
不回滚、重读 unsupported/mismatched、镜像落在目标上/超容量拒绝、演示路径不落盘与挂载
转义解析。全部使用注入后端与夹具文件，不接触真实设备或 loop 设备。

### 8. HTTP 界面实测（同日文档同步时追加，t16 接线）

用真实 demo 服务器与 curl 逐条验证 HTTP 路由、payload 与状态码（未启用 live，因此没有
任何写入路径被触发）：

```console
$ RSETUP_IMAGE_DIR=/tmp/rsn-images rsetup-next --demo serve --listen 127.0.0.1:8799
$ curl -s $B/api/v1/storage/install/targets            # HTTP 200
  liveAllowed=false；targets=mmcblk0(eMMC)/mmcblk1(SD, holdsRunningSystem, mounted)/
  nvme0n1/sda(UFS)；images=demo.img(4194304)、not-an-image.img(1048576)
  （符号链接 link.img 未列出）
$ curl -s -H 'x-rsetup-request: 1' -d '{"targetId":"sda","imageId":"demo.img"}' .../plan
  HTTP 200；request.imagePath=/private/tmp/rsn-images/demo.img（目录内绝对路径）
$ .../apply {"acknowledgement":"mmcblk0"}              # HTTP 409 acknowledgement_mismatch
  type "sda" to write /dev/sda; received "mmcblk0"
$ .../apply {"acknowledgement":""}                      # HTTP 409 confirmation_required
$ .../apply {"acknowledgement":"sda"}                   # HTTP 422，服务器未以 live 启动
  install_unsupported: Live execution is not enabled on this surface; … Nothing was written.
$ .../apply 复用同一令牌                                # HTTP 409 stale_plan
$ .../plan  {"imageId":"../demo.img"}                   # HTTP 400 invalid_install_request
  the image id must be a plain file name, not a path
$ .../plan  {"imageId":"link.img"}                      # HTTP 400
  the image must be a regular file, not a symlink or a directory
$ .../plan  {"imageId":"not-an-image.img"}              # HTTP 400
  the image has no MBR or GPT partition table; it is not a bootable system image
$ .../plan  {"targetId":"mmcblk1"}                      # HTTP 409 writes_running_system
$ .../plan  {"targetId":"sdz"}                          # HTTP 400 unknown block device sdz
$ .../plan 不带 x-rsetup-request 头                     # HTTP 403 request_forbidden
```

界面测试：`node --test ui/*.test.mjs` → 86 passed、0 failed（含 10 个安装页面用例）。本
报告没有在浏览器或 TUI 中手工操作，也没有启动桌面 GUI；这三者由自动化测试覆盖。

## 未完成边界（待真机验证）

- 在 Cubie A7Z 上对真实 UFS/eMMC 的写入、分区表重读与重启进入新系统。
- 真实 `/proc/mounts` 下的挂载冲突判定、真实可移动介质警告。
- 写入中断后的真实恢复流程（代码中的"不回滚 + 报告已写字节数"由失败夹具覆盖）。
- Linux 上 `BLKRRPART` 重读的真实结果。
- 通过 TUI、HTTP、Tauri 或浏览器页面在板卡上真实写入（四者都需要在主机上启用 live
  执行）。它们的非 live 行为已由下节的端点实测与自动化测试覆盖，但"在真实设备上写盘"
  这一路径仍未在任何材料上执行过。

## 本次发现（不在本任务范围内）

对**压缩镜像**：写入成功后，CLI 的"分区表校验"行会打印
`the re-read partition table DOES NOT match the image`（中文：`重读的分区表与镜像不一致`）。
原因是 `verify()` 先按 `same_partition_layout(plan.image.partitions /* .xz 为空 */, observed)`
判定为不匹配，再进入为压缩镜像准备的分支；因此那条"压缩镜像未读布局"的说明只作为
`detail` 附在 mismatched 状态上，界面上仍显示为不匹配（见
`crates/rsetup-core/src/system_install.rs` 的 `verify()` 与
`crates/rsetup-app/src/i18n.rs` 的 `install_verification_mismatched`）。写入本身成功，
但该行文案会误导操作者；`docs/system-install.md` 已把它写成预期输出。是否修复属于
crates/** 的实现决策。

## 后续修订（superseded，2026-10-14 注记）

以上正文是按日期的原始记录，**未作改写**。以下结论已被后续任务取代，阅读时以取代者为准：

| 本报告中的结论（原文位置） | 现状 | 取代者与依据 |
| --- | --- | --- |
| "本次发现"一节：压缩镜像写后校验打印 `the re-read partition table DOES NOT match the image`，界面显示为不匹配 | **已被取代**：压缩镜像现在返回 `Unsupported` + detail（说明布局未在写入前解析、因此未比较重读表），不再出现 `DOES NOT match`；t19 之后 CLI 与 TUI 进一步区分两类 Unsupported（压缩镜像 vs 主机无法重读分区表），压缩镜像打印专属文案 + core 原因 | t19（`verify()` 的压缩分支）、t23（`verification_text`/`write_install_result` + i18n 键 `install_verification_compressed_not_compared`）、t28；本文档同步见 t30（`debian/rsetup-next.1`、`docs/system-install.md` 及其中文镜像） |
| "### 7. 自动化测试"中对当时 22 个 core 用例的描述里"单次使用与过期"一语所隐含的 token 模型（进程内集合 + 共享台账目录 / `RSETUP_INSTALL_TOKEN_DIR`） | **已被取代**：token 现为每进程 HMAC-SHA256 签名的 handle；单次使用与 600 秒窗口由**签发进程的记录**判定；台账目录与其环境变量、tmpfiles.d 规则、postinst 步骤、smoke 断言全部删除；跨进程预览检查（`verify_preview_token`）仅检查状态漂移与年龄，属 **advisory**、不构成对抗性保证 | t28（token 重做与台账删除）、t31（源码注释同步）、t30（debian/ 与文档清理）；模块文档见 `crates/rsetup-core/src/system_install.rs` 的 "Tokens: what is guaranteed, and by what" |
| 本文 §6 "缺少 `xz-utils`" 的结论、§1–§5 的 CLI/demo 观测、§4 "预演不写盘" | **仍然有效**（后续未改动这些行为；门禁在最终 revision 上仍全绿） | t18（四端语义一致性复验 PASS）、t30 |
| "未完成边界"中"通过 TUI、HTTP、Tauri 或浏览器页面在板卡上真实写入" | **仍然有效**：四端非 live 行为已实测/由测试覆盖，但真实设备写入仍未在任何材料上执行 | t18（四端非 live 行为矩阵）、本文 §8 |
| §7 与 §8 的测试计数与用例清单（core 22 个 `system_install` 用例、`node --test ui/*.test.mjs` 86 passed / 10 个安装页面用例） | **已被取代（仅计数，不是结论）**：core 的 `system_install` 用例已增至 31 个；`node --test ui/*.test.mjs` 现为 88 passed，其中 `ui/system-install.test.mjs` 12 个用例；`cargo test --workspace --locked` 现为 190+74+2 passed / 2 ignored / 0 failed | t23/t25/t28/t31（新增用例）、t18（最终 revision 上的双跑实测）；文档侧计数同步见 t30 的 `docs/system-install.md` 及中文镜像 |

注：本报告记录的是 2026-10-13 当时的工作树；此后 `crates/` 的改动（t19/t23/t25/t28/t31）与
`debian/`、`docs/` 的改动（t30）均不在当时范围内，故正文中的行号、哈希与用例计数不保证与
当前工作树一致。

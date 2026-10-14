# 系统镜像安装

`rsetup-next system` 会列出运行中 SBC 的块设备，并在打印完整后果清单、要求键入
精确设备名之后，把系统镜像（`.img` 或 `.img.xz`）写入其中一块设备。首个目标板卡是
Radxa Cubie A7Z（CIX A733，UFS + eMMC + SD）。

本文是面向使用者的操作流程、板上验证清单，以及"哪些已验证、哪些未验证"的记录。
拒绝写入背后的闸门不变量记录在[架构说明](architecture.md)中，本文不重复。

## 状态标签

| 标签 | 含义 |
| --- | --- |
| **已本地验证** | 在开发主机上以出厂二进制复现：CLI 预演、对 demo HTTP 服务器的真实请求，或自动化套件。它不是真实板卡的证据。 |
| **由测试覆盖** | 由自动化套件通过夹具、注入后端或 mock 运行时执行，未在本机手工操作。 |
| **已真机验证** | 在真实 Cubie A7Z 上执行过；声称之处会给出该次运行。 |
| **待真机验证** | 必须在真实 Cubie A7Z 上用真实镜像执行。目前没有任何结果；不得当作通过。 |

合成、预演或夹具结果永远不能算作硬件证据；标签只适用于真正执行过的内容。下文每条
关于界面的陈述都在"每条陈述的来源"中列出出处。

## 命令面

```sh
rsetup-next system targets [--json]
rsetup-next system plan --target <id> --image <绝对路径> [--json]
rsetup-next system install --target <id> --image <绝对路径> \
  [--acknowledge <设备名>] [--live-execution] [--json]
```

- `targets` 与 `plan` 是只读预览，无需 root，也不写任何数据。
- `install` 先预览、要求键入设备名，然后执行。
- 在 Linux 上不带 `--live-execution` 时，`install` 始终是**预演**：它会展示真实设备
  身份并报告"写入完成"，但一个字节都不落盘。
- 真实写入需要 root：用 `sudo`，或以普通用户运行并让 `pkexec` 启动固定动词
  helper `/usr/libexec/rsetup-next-helper`。
- 同一套流程也可从 TUI、HTTP API、Tauri 桌面外壳与浏览器页面执行；见下文
  "一套流程，四个界面"。

## 一套流程，四个界面

四个产品界面——CLI、TUI、Web 控制台、桌面外壳——调用同一个 `SystemInstallManager`，因此
闸门、镜像校验、计划令牌与键入名称比对不可能在各界面之间漂移。任何界面都不持有自己的
执行策略：只有控制器的策略为 `Live`（CLI 为 `--live-execution`）时才真正写入，提供者
（真实或演示）来自控制器快照。未处于 live 的界面会**拒绝**写入而不是模拟写入，调用方
因此不可能把模拟写入误认为真实写入。

| 界面 | 入口 | 真实写入条件 |
| --- | --- | --- |
| CLI | `rsetup-next system targets\|plan\|install` | Linux 上的 `--live-execution` |
| TUI | 主界面按 `i`，`Esc` 返回 | 控制器策略 |
| HTTP API | `GET/POST /api/v1/storage/install/…` | 服务器以 `--live-execution` 启动 |
| Tauri 桌面外壳 | `system_install_targets` / `system_install_plan` / `system_install_apply` | 同一控制器策略 |
| 浏览器页面 | 环回控制台的导航 **系统安装 / Install** | 同一服务器策略 |

表中列出五行，是因为 Web 界面既可经 HTTP API 访问、也是一张页面；两者共享同一个服务器端
管理器与同一策略。

### TUI 面板

在主界面按 `i` 打开系统镜像面板（`Esc` 关闭并返回）。`Tab` 在设备列表、镜像路径与键入
设备名三个字段间切换；`j`/`k`（或方向键）在列表中移动，`r` 刷新设备列表——这三个字母
只在列表获得焦点时作为快捷键，在路径与确认字段中按普通字符输入。`Backspace` 删除，
`Enter` 在列表/路径字段载入计划、在确认字段尝试写入。所有拒绝都来自 core，面板原样呈现
core 的错误码；面板"键入名称匹配前禁用"的提示仅用于显示，从不绕过 core。面板默认永不
写盘：demo 与非 Linux 主机使用合成提供者，真实目标还需要 Live 策略，否则干跑写入器会
丢弃每一个字节。

**由测试覆盖**（通过 `TestBackend` 断言渲染与按键路由）；本文没有手工驱动该面板。

### HTTP API

```text
GET  /api/v1/storage/install/targets
  → 200 {"liveAllowed": false,
         "targets": [<SystemInstallTarget>…],
         "images":  [{"id": "radxa-cubie-a7z.img.xz", "sizeBytes": 1200000000}]}

POST /api/v1/storage/install/plan
  body {"targetId": "sda", "imageId": "radxa-cubie-a7z.img.xz"}
  → 200 <SystemInstallPlan，camelCase>

POST /api/v1/storage/install/apply
  body {"targetId": "sda", "imageId": "radxa-cubie-a7z.img.xz",
        "planToken": "<来自 plan>", "acknowledgement": "sda"}
  → 200 {"run": …,"plan": …,"bytesWritten": 4194304,
         "verification": {"state": "matched|mismatched|unsupported", …}}
```

服务器在 core 看到请求之前先强制以下规则：

- 请求必须来自环回源，且每个变更类调用必须带 `x-rsetup-request: 1`；否则返回 403
  `request_forbidden`（"Use the local console or an SSH tunnel…"）。
- 客户端永远不提交路径，只提交 `imageId`：必须是服务器镜像目录
  （`/var/lib/rsetup-next/images`，可用 `RSETUP_IMAGE_DIR` 覆盖）内的纯文件名，且以
  `.img` 或 `.img.xz` 结尾。包含路径穿越或分隔符、`.`/`..`、控制字符、超过 255 字节、
  符号链接、目录或不存在的文件，一律 400 `invalid_install_request`。请求体中多余的
  `imagePath` 字段会被忽略：解析后的路径始终来自目录。
- `plan` 会把目录 id 作为 `image.id` 回显，与解析后的 `request.imagePath` 并列，因此客户端
  可以仅凭 plan 响应构造 apply 负载；浏览器页面就以该回显 id 为唯一来源。令牌绑定用的是
  解析后的路径、大小与哈希，而不是这个标签，所以回显 id 不可能扩大可写入范围。
- `targets` 只列出 `apply` 会接受的 id（同样的规则）。通过目录规则但没有 MBR/GPT 分区表
  的文件仍会被列出，随后由 `plan` 拒绝——所有与设备相关的判定都留在 core。
- 除非服务器以 `--live-execution` 启动，否则 `liveAllowed` 为 false；此时一次本可成功的
  `apply` 会以 422 `install_unsupported` 与"…Nothing was written."被拒绝，因为该界面拒绝
  自己的写入，而不是报告一次模拟写入。

错误使用与 API 其余部分相同的信封 `{"error":{"code":"…","message":"…"}}`；每个错误码的
HTTP 状态见下表（`root_required` 为 403，写入期 `internal_error` 为 500），服务器繁忙时
还可能返回 429 `server_busy`。

**已本地验证**：2026-10-13 对真实 demo 服务器实测（完整命令见文末"如何复现主机侧检查"
与[本地验证报告](testing/system-install-2026-10-13/report.md)）。

```console
$ RSETUP_IMAGE_DIR=/tmp/rsn-images rsetup-next --demo serve --listen 127.0.0.1:8799
$ curl -s $B/api/v1/storage/install/targets
  HTTP 200 · liveAllowed=false · images: demo.img、not-an-image.img（link.img 被排除）
$ curl -s -H 'x-rsetup-request: 1' -d '{"targetId":"sda","imageId":"demo.img"}' .../plan
  HTTP 200 · request.imagePath=/private/tmp/rsn-images/demo.img
$ .../apply {"acknowledgement":"mmcblk0"}
  HTTP 409 {"error":{"code":"acknowledgement_mismatch","message":"type \"sda\" to write /dev/sda; received \"mmcblk0\""}}
$ .../apply {"acknowledgement":""}
  HTTP 409 {"error":{"code":"confirmation_required","message":"confirmation is required before writing a system image"}}
$ .../apply {"acknowledgement":"sda"}                       # 服务器未以 live 启动
  HTTP 422 {"error":{"code":"install_unsupported","message":"Live execution is not enabled on this surface; restart with --live-execution to write an image. Nothing was written."}}
$ .../apply 复用同一令牌
  HTTP 409 {"error":{"code":"stale_plan","message":"the target or the image changed after preview; create a fresh plan"}}
$ .../plan {"imageId":"../demo.img"} | {"imageId":"link.img"} | {"imageId":"not-an-image.img"}
  HTTP 400 invalid_install_request（"…plain file name, not a path" / "…regular file, not a symlink or a directory" / "…no MBR or GPT partition table…"）
$ .../plan {"targetId":"mmcblk1"}
  HTTP 409 {"error":{"code":"writes_running_system","message":"refusing to write /dev/mmcblk1: it hosts the running root or boot filesystem"}}
$ .../plan 不带 x-rsetup-request 头
  HTTP 403 {"error":{"code":"request_forbidden","message":"Use the local console or an SSH tunnel; cross-origin requests are forbidden."}}
```

### Tauri 桌面外壳

三个命令，payload 与 HTTP 相同：

| 命令 | 参数（JS `invoke` 的键名） | 返回 |
| --- | --- | --- |
| `system_install_targets` | 无 | `{liveAllowed, targets, images}` |
| `system_install_plan` | `targetId`、`imageId` | `SystemInstallPlan` |
| `system_install_apply` | `targetId`、`imageId`、`planToken`、`acknowledgement` | `SystemInstallApplyResult` |

失败以 `{code, message}` 返回，错误码与 CLI／HTTP 共用同一组十个（该 crate 在转换实现上方
注明镜像自 CLI 的集中表）。只有 `system_install_targets` 被归类为只读：它没有参数、不改变
任何状态；plan 与 apply 被有意排除在该清单之外。未启用 live 时，外壳与 HTTP 一样拒绝写入
（同一个拒绝写入器），因此演示或预览会话不可能写盘。

**由测试覆盖**：命令经 mock 运行时真实派发、apply 必须带 acknowledgement、注册表守卫在命令
被删除时会失败。本文没有启动桌面应用手工操作。

### 浏览器页面

环回控制台新增 **系统安装 / Install** 路由（`data-route="install"` →
`<section class="view" data-view="install">`），并带一个刷新目录的按钮。它会：

- 读取 `GET /api/v1/storage/install/targets`（嵌入 Tauri 时为 `system_install_targets`），
  渲染 `data-live-allowed="true|false"` 横幅——未启用实时写入时明确写出：确认后的写入会被
  服务器以 `install_unsupported` 拒绝且不会写入任何数据；
- 列出全部检测到的设备，但禁用承载运行系统或已挂载的设备（在选项文案中给出原因），镜像
  只列目录 id（绝不提供自由文本路径）；
- 渲染完整计划：目标身份、被清除分区及其挂载点、镜像格式/大小/SHA-256 前缀/分区表、两条
  强制警告加上 core 自己的警告，以及管理员授权提示；
- 把键入的设备名作为 `acknowledgement` 字符串提交
  `{targetId, imageId, planToken, acknowledgement}`——绝不用布尔值、绝不带 `imagePath`；
  其中 `imageId` 取自被审阅 plan 回显的 id（唯一来源），因此“显示的身份”与“提交的身份”
  不可能分歧；按钮的禁用状态来自仅用于显示的谓词，它与 core 的 trim 后精确比对一致；
- 按稳定错误码逐个渲染独立文案（十个码 + 一个通用回退），失败绝不折叠成一句通用错误；
  `stale_plan`/`plan_required` 会丢弃计划并要求重新生成。

**由测试覆盖**：`node --test ui/*.test.mjs` → 88 passed、0 failed，其中包含
`ui/system-install.test.mjs` 的 12 个用例（后果清单、键入名称门控、payload 形状与唯一
image id 来源、逐码文案、过期计划重置、未启用 live 的拒绝）。本文没有在浏览器中手工点击
该页面。

## 预览会打印什么

```text
$ rsetup-next system plan --target sda --image /root/radxa-cubie-a7z.img.xz
系统镜像写入预览
目标设备: /dev/sda — UFS (RADXA UFS 128GB)
容量: 128.00 GiB
将被清除的现有分区 (3):
  - sda1 · 128.00 MiB
  - sda2 · 2.00 GiB
  - sda3 · 6.00 GiB
镜像: /root/radxa-cubie-a7z.img.xz
  格式: Xz · 文件大小: 768 B · 写入大小: 4.00 MiB
  SHA-256（前 1 MiB）: 2e835412001233bd54b817d2f388a596e1857afe6b817502ff038c8356b6a215
  分区表: 未读取（压缩镜像）
写入需要管理员授权
! 写入会替换目标设备上的全部分区。
! 此写入无法回滚。
! 镜像是压缩的，会在写入时解压（需要 xz）。
```

示例中的数值来自演示目标与 4 MiB 夹具，**结构是契约，数值不是**。要点：

- **目标身份**：路径、类型（`UFS`/`eMMC`/`SD card`/`NVMe`）、型号、容量，以及适用的
  `可移动介质`、`已挂载 <挂载点>`、`承载运行系统` 标记。
- **被清除分区**：目标上每个分区的名称、大小与挂载点——这就是写入的全部后果。
- **镜像**：格式、压缩文件大小、未压缩的**写入大小**（`.img.xz` 由流索引得出，不是
  文件大小）、以及前 1 MiB 的 SHA-256（计划令牌绑定该值）。
- **分区表**：`MBR`、`GPT` 或 `未读取（压缩镜像）`。
- **警告**：始终包含"替换全部分区"与"无法回滚"；可移动介质或需要 xz 时再追加。

`--json` 输出同一份计划（`planToken`、`target`、`image`、`destroys`、`warnings`、
`requiresRoot`、`chunkBytes`）。成功走 stdout；被拒绝时输出
`{"error":{"code":…,"message":…}}` 并退出码 1。

## 失败码

所有界面共用同一组稳定错误码，脚本可以按码分支，而不必解析文案。CLI 输出
`Error: <code>: <message>` 并以退出码 1 结束；HTTP API 在错误信封中返回同一码并带下表状态；
Tauri 命令返回 `{code, message}`；浏览器页面按码渲染各自的文案。错误码只定义一次
（`crates/rsetup-app/src/main.rs::system_install_error_code`），桌面 crate 镜像同一套。
消息文本目前来自 core，全部为英文。

| 错误码 | HTTP | 含义 | 处理方式 |
| --- | --- | --- | --- |
| `confirmation_required` | 409 | 键入的设备名为空（例如 stdin 已关闭）。 | 重新键入，或使用 `--acknowledge <设备名>`。 |
| `acknowledgement_mismatch` | 409 | 键入的名称不是目标设备名。 | 重新查看预览；消息会给出期望值与实际值。 |
| `writes_running_system` | 409 | 目标承载正在运行的根或 `/boot` 文件系统。 | 换一块启动介质后再写入；没有覆盖开关。 |
| `install_conflict` | 409 | 目标已挂载，或镜像就存放在目标上。 | 卸载目标，或把镜像移到别处。 |
| `invalid_install_request` | 400 | 相对路径/不可读、镜像 id 不是目录内的纯文件名、符号链接、目录、空文件、无 MBR/GPT、设备不存在、镜像大于目标等。 | 修正请求；消息会指出具体问题。 |
| `install_unsupported` | 422 | 该步骤在此处不可行：主机不能重读分区表、`.img.xz` 缺少 `xz`，或该界面未以 live 启动。 | 使用 Linux；安装 `xz-utils`；或启用 live 执行。不会写入任何数据。 |
| `plan_required` | 409 | 未带计划令牌就调用 apply。 | 先执行 `plan`。 |
| `stale_plan` | 409 | 令牌过期（600 秒）、已被**签发它的进程**消费，或目标/镜像已变化。 | 重新预览并使用新令牌。 |
| `root_required` | 403 | 未提权、且 helper 不存在时请求真实写入。 | 使用 `sudo`，或安装随包提供的 helper。 |
| `internal_error` | 500 | 写入过程中的 I/O 或设备错误。 | 阅读消息：其中会报告已写入的字节数。 |

HTTP 边界还可能在这些错误码之前返回 403 `request_forbidden`（跨源、非环回，或变更类调用
缺少 `x-rsetup-request: 1`），以及并发过多时的 429 `server_busy`。

HTTP 列来自该界面自身的映射（`ApiError::from_install` 加上 live 拒绝的覆盖分支）。上例中
引用的每个码都在 demo 服务器上实测过；demo 目标无法产生的码——已挂载但不承载启动的
目标（`install_conflict`）与 `root_required`——仅按该映射记录。

## 令牌如何工作

计划令牌是 **handle，不是 credential**：

- 它单次使用、签发后 600 秒过期，这两件事都由**签发进程交给外部的记录**判定，而不由调用方
  写进令牌的内容决定。别的进程签发的令牌一律被直接拒绝，无论其字段写成什么。
- 改动令牌任一字段（binding、签发时刻、nonce 或签名）都会使其校验失败，因此已消费的令牌
  无法伪装成新的、过期令牌也无法被“重新武装”。
- 在一个进程预览、在另一个进程写入的场景（特权 helper），会用 `verify_preview_token` 发现
  设备状态相对操作者所见预览已经漂移，或预览已经过旧。由于签发时刻由调用方提供，**这项
  跨进程检查是 advisory**：它防的是误操作与重复执行，不构成对抗性边界。
- 真正保护设备的是其余全部：计划会按当前状态重建且 binding 必须相等、硬闸门会重新评估、
  必须键入设备名、且真实写入需要管理员授权。

## 每条陈述的来源

| 本文陈述 | 来源（2026-10-14 核对） |
| --- | --- |
| CLI 命令、默认预演、helper 路径、错误码与退出状态 | `crates/rsetup-app/src/main.rs`（`SystemCommands`、`run_system_command`、`system_install_error_code`） |
| TUI 面板入口按键、焦点路由、仅显示用谓词、干跑写入器 | `crates/rsetup-app/src/tui.rs`（`InstallPanel`、`App::install_key`、`install_plan_lines`） |
| HTTP 路由、响应字段、目录规则、边界头、状态映射、live 拒绝 | `crates/rsetup-app/src/server.rs`（路由 202-204 行；`InstallOptions`/`InstallPlanRequest`/`InstallApplyRequest`；`catalog_image_path`；`InstallSurface::install_error`；`ApiError::from_install`） |
| Tauri 命令名、参数、只读归类、镜像错误码 | `apps/desktop/src-tauri/src/main.rs`（`system_install_targets|plan|apply`、`generate_handler!`、`IPC_COMMANDS`、`READ_ONLY_COMMANDS`、`CommandError`） |
| 浏览器页面入口、payload、禁用谓词、逐码文案 | `ui/index.html`（导航 `data-route="install"`、`data-view="install"`）、`ui/app.js`（2841 行起的 `install*` 辅助函数与视图处理）、`ui/i18n.js`（`install.*` 键） |
| 闸门、镜像校验、键入名称比对、分区表重读 | `crates/rsetup-core/src/system_install.rs`；不变量见[架构说明](architecture.md) |
| 令牌模型：handle 而非 credential、由签发进程记录判定、HMAC、单次使用、600 秒窗口、跨进程预览检查为 advisory | `crates/rsetup-core/src/system_install.rs`（模块文档 “Tokens: what is guaranteed, and by what”；`SystemInstallManager::{plan, apply_live, consume, verify_preview_token}`） |
| 两类 Unsupported 校验（压缩镜像 vs 主机无法重读） | `crates/rsetup-app/src/tui.rs`（`verification_text`）、`crates/rsetup-app/src/main.rs`（`write_install_result`）、`crates/rsetup-app/src/i18n.rs`（`install_verification_compressed_not_compared`） |
| `xz -dc` 解压与缺少工具时的原因 | `crates/rsetup-core/src/system_install.rs`（`SystemImageOpener::open`） |
| 本文引用的测试与端点结果 | 文末命令：`cargo test --workspace --locked`、`node --test ui/*.test.mjs`，以及复现出的 demo 服务器 `curl` 运行 |

## 打包：xz-utils 依赖

`.img.xz` 是 Radxa 官方发布的镜像格式，解压属于主安装流程，因此软件包声明硬依赖
`Depends: xz-utils`；取舍（以及为何不用 `Recommends`）记录在
[debian/README.source](../debian/README.source)。

缺少该工具时不会崩溃，也不会静默少写：

- **预览不需要 `xz-utils`**：`.xz` 的流头、索引与页脚由纯 Rust 解析，写入大小、
  SHA-256 绑定与"需要 xz"警告都能正常给出。**已本地验证**（夹具 `.img.xz`，预览
  退出码 0）。
- **写入本身需要它**。当 `PATH` 中没有 `xz` 时：

  ```console
  $ env PATH=/usr/bin:/bin rsetup-next --demo system install \
      --target sda --image image.img.xz --acknowledge sda --json
  {"error":{"code":"install_unsupported","message":"system image installation is unavailable: install xz-utils to write a compressed image"}}
  $ echo $?
  1
  ```

  **已本地验证**（在 `PATH` 中移除 `xz` 的主机上实测）。

`debian/tests/smoke` 断言打包后的二进制暴露新命令面（`system targets --json`），并在
请求被拒绝时仍给出稳定错误码。**已本地验证**（两条断言均实测通过）。

## 板上验证清单：Radxa Cubie A7Z

本节由人工在板卡上执行。除注明"已本地验证"的步骤外，本节内容全部**待真机验证**。
请记录每一步的输出；没有记录的步骤不算通过。

以下每一步都使用 CLI。通过 TUI、环回 Web 控制台或桌面外壳进行真实写入，需要在板卡上启用
live 执行，且不属于本流程；这些路径同样保持**待真机验证**。

### 步骤 0 — 前置条件

- Cubie A7Z 使用独立供电，具备串口控制台或 SSH，并准备**第二块**启动介质
  （SD 卡或 eMMC），确保写入目标绝不是正在运行的系统。不要指望靠断电来恢复。
- 已知的恢复路径：Radxa maskrom／板卡文档化的烧录模式，以及重新烧录介质所需的工具。
- 已安装软件包：`rsetup-next --version` 输出 `rsetup-next 0.5.0` 或更高，
  `/usr/libexec/rsetup-next-helper` 存在。
- 镜像及其官方 SHA-256：

  ```sh
  ls -l /root/radxa-cubie-a7z_*.img.xz
  sha256sum /root/radxa-cubie-a7z_*.img.xz
  xz -t /root/radxa-cubie-a7z_*.img.xz   # 无输出，退出码 0
  curl -fsSLO <官方 SHA256SUMS 或 .sha256 文件>
  sha256sum -c <官方校验文件>
  ```

  **失败判据**：校验和不符、`xz -t` 报损坏、或镜像大于目标容量——停止，不要写入。

### 步骤 1 — 识别候选设备

```sh
rsetup-next system targets
rsetup-next system targets --json
```

期望：每块候选设备一行，包含类型、型号、容量与被清除分区数量。A733 板卡上 UFS
控制器枚举为 `sd*` 节点，请以**类型列**（`UFS`）为准，而不是节点名。系统所启动的
SD 卡必须显示 `可移动介质`、挂载点与 `承载运行系统`：

```text
/dev/mmcblk1 — SD card SD 32GB · 29.00 GiB · 2 Existing partitions destroyed · Removable media · Mounted /, /boot · Hosts the running system
```

**已本地验证**（macOS，合成设备）：上述清单的文本与 JSON 输出，无需 root、不访问设备。
**待真机验证**：真实 UFS/eMMC 的类型、容量、分区列表是否正确，启动介质是否被标记为
承载运行系统。

**失败判据**：缺少 UFS 或 eMMC、类型判断错误、已挂载目标未标记 `Mounted …`、或启动
介质未被标记 `Hosts the running system`。出现任一项都必须在写入前上报。

### 步骤 2 — 预览写入（无需 root，不写入）

```sh
rsetup-next system plan --target sda --image /root/radxa-cubie-a7z_*.img.xz
rsetup-next system plan --target sda --image /root/radxa-cubie-a7z_*.img.xz --json
```

期望：与上文同构的预览（数值替换为你的设备与镜像），且必须包含：

- `写入大小:` 等于镜像未压缩大小（大于文件大小），绝不能是 `0 B`；
- `分区表: 未读取（压缩镜像）`；
- `! 镜像是压缩的，会在写入时解压（需要 xz）。`；
- 退出码 0，且没有向任何设备写入。

随后对 eMMC 重复（`--target mmcblk0`）。

**失败判据**：非零退出、缺少破坏性警告、写入大小超过目标容量、或预览中出现了你并不
打算覆盖的设备。若打算用 `--json` + 带外 apply，请保存 `planToken`。

### 步骤 3 — 写入目标（不可回滚）

```sh
sudo rsetup-next --live-execution system install --target sda \
  --image /root/radxa-cubie-a7z_*.img.xz --acknowledge sda
```

键入的名称必须精确等于目标设备名（`sda`，而不是 `/dev/sda`、`SDA` 或分区名）。比对由
core 完成而非 CLI，因此该规则对后续的 TUI 与 API 同样成立。

期望输出：先打印预览，再打印写入报告：

```text
写入完成
  目标设备: /dev/sda
  已写入字节: <镜像未压缩大小>
  分区表校验: <见下>
```

- **raw `.img`**：若内核重读成功，最后一行应为 `重读的分区表与镜像一致`。在本次验证的
  Cubie A7Z UFS（2026-10-14）上，内核**拒绝**重读并返回 `Invalid argument (os error 22)`，
  因此该行呈现为 `the kernel could not re-read the partition table of /dev/sda after the
  image was written, so the write could not be verified: … Invalid argument (os error 22)`
  —— 这是 `a310400` 之后的诚实文案（修复前同一情形会被误报为"不一致"）；同一设备上在
  shell 里执行 `blockdev --rereadpt /dev/sda` 反而成功，且写入已由 `dd` 回读确认逐字节
  相同（见"目前的验证范围"中的 `BLKRRPART` 一行）。开发主机根本无法执行该重读，那里显示
  `当前主机无法重读分区表`。
- **压缩 `.img.xz`**：压缩镜像在写入前从不读取布局，因此没有可比对的对象——运行会把校验
  状态报告为 `unsupported` 并给出原因，而**不是**声称“不一致”。CLI 与 TUI 会区分两种情形：
  压缩镜像打印压缩镜像专属文案并附上 core 给出的原因；而根本无法重读分区表的主机（非 Linux
  开发主机）打印通用的“无法重读”原因。JSON 中同样是
  `verification.state = "unsupported"`，`detail` 说明压缩镜像的布局未在写入前解析。这属于
  **预期结果，不是失败**；请改用步骤 4 确认成功。**待真机验证**——该文案只能在 Linux 上对
  真实设备观察到。

**失败判据**：任何非零退出；字节数与预览的写入大小不符；写入中途停止（此时错误会报
`wrote N of M bytes before failing (…); the target now holds a partial image and cannot
be rolled back`）；内核日志出现大量 I/O 错误；或写入后板卡丢失目标设备。

### 步骤 4 — 重启前校验介质

```sh
lsblk -o NAME,SIZE,TYPE,FSTYPE,LABEL,MOUNTPOINTS
sudo blkid
sudo partprobe /dev/sda     # 可选：请求内核重读
sudo cmp -n 4194304 /root/<image>.img /dev/sda   # 仅 raw 镜像
```

期望：分区表与镜像一致，各分区大小与预览中"将被清除"的条目（现已替换）相符；对 raw
镜像，`cmp` 对比较区段报告无差异。

若镜像小于介质，`sfdisk` 还可能打印下文"首次启动会扩展根分区并修正备份 GPT"一节描述的
保护性 MBR 与备份 GPT 提示。它们不是失败：主 GPT 始终有效，首次启动会自行修正。

**失败判据**：没有分区表、分区大小与镜像不符、或文件系统无法读取。不要重启进入，重新
烧录。

### 步骤 5 — 启动写入后的介质

1. 断电，移除另一块启动介质，让板卡从刚写入的 UFS/eMMC 启动。
2. 在板上执行：`findmnt /`、`cat /etc/os-release`、`uptime`。
3. 在新系统上重跑步骤 1，确认它现在启动的设备正是你写入的那块，且旧启动介质不再被标记
   `Hosts the running system`。

**失败判据**：无法启动、启动到非预期系统、或根文件系统与镜像不符。用文档化的烧录模式
恢复。

### 首次启动会扩展根分区并修正备份 GPT（背景说明，无需操作）

发行镜像按家族中最小容量构建（本次验证写入的是 10 GiB 镜像），通常小于实际写入的介质。
因此镜像自带的首启流程会把根分区扩展到整个设备（`growpart` 一类，由镜像自身完成，不由本
工具完成），并在同一步里把**备份 GPT 与保护性 MBR**移动到设备末尾。

这就是为什么把较小的 GPT 镜像写入较大设备后，`sfdisk` 会打印如下提示：

```text
GPT PMBR size mismatch (2621439 != 31221759)
The backup GPT table is not on the end of the device.
```

它们描述的是**备份**表的位置，而不是主表损坏：主 GPT 始终有效、各分区全程可用。首次启动
就会消除这两条提示，无需任何操作——不要运行 `sgdisk -e` 或其它修复工具，也不要把它们当作
步骤 4 的失败。

**已真机验证**（Radxa Cubie A7Z，2026-10-14）：把 10 GiB 镜像写入 119.1 GiB、4096 字节
扇区的 UFS 后，板卡从该 UFS 启动，`sda3` 自动从 9.5G 增长到 118.6G，分区全程可用。

**设计后果**：因此本工具**有意完全不修改分区表**：它只写入镜像字节，并在步骤 3 请求内核
重读一次分区表用于比对；它从不调用 `sgdisk`/`parted`、从不搬移备份 GPT、也从不改写保护性
MBR。上述现象会自愈，因此写入时也不需要任何提示。

### 步骤 6 — 反向检查（在破坏性写入之前执行）

以下检查用于证明闸门在真实板卡上生效。最后一项请使用**可丢弃的**启动介质，或在步骤 3
之前、目标尚未被改动时执行。

```sh
# 1. 承载运行系统的设备必须被永久拒绝，且没有覆盖开关
rsetup-next system plan --target mmcblk1 --image /root/<image>.img.xz ; echo $?
# → Error: writes_running_system: refusing to write /dev/mmcblk1: it hosts the running root or boot filesystem
#   退出码 1

# 2. 键入错误名称必须在预览之后被拒绝
sudo rsetup-next --live-execution system install --target sda \
  --image /root/<image>.img.xz --acknowledge mmcblk0 ; echo $?
# → Error: acknowledgement_mismatch: type "sda" to write /dev/sda; received "mmcblk0"
#   退出码 1

# 3. 空回答必须被拒绝
sudo rsetup-next --live-execution system install --target sda \
  --image /root/<image>.img.xz < /dev/null ; echo $?
# → Error: confirmation_required: confirmation is required before writing a system image
#   退出码 1

# 4. 已挂载目标必须被拒绝并列出挂载点
sudo mount /dev/sdb1 /mnt
rsetup-next system plan --target sdb --image /root/<image>.img.xz ; echo $?
# → Error: install_conflict: …（并列出挂载点）
sudo umount /mnt
```

**已本地验证**（macOS，演示后端）：检查 1–3 的确切消息与退出码，以及相对路径、缺失
文件、目录与符号链接、无分区表镜像、未知设备 id、非 Linux 主机上的 `--live-execution`
（`Error: live execution is only supported on Linux SBC hosts`）等拒绝路径。

**待真机验证**：检查 1–4 面对真实 sysfs 与 `/proc/mounts` 是否成立，以及被拒绝时确实
没有写入任何字节。用前后采样证明"零字节"：

```sh
sudo dd if=/dev/sda bs=1M count=4 status=none | sha256sum   # 之前
# … 执行一条被拒绝的命令 …
sudo dd if=/dev/sda bs=1M count=4 status=none | sha256sum   # 必须完全相同
```

**闸门失败判据**：被拒绝却退出码 0、错误码不符、消息未指明设备，或本应被拒绝的目标其
前 4 MiB 发生了变化。

### 结果记录

按 `docs/testing/` 中既有报告的格式，把本轮写成
`docs/testing/system-install-<日期>/report.md`：板卡与 SoC、镜像名与 SHA-256、逐条命令、
每一步的实际输出、覆盖到的闸门、失败项，以及明确列出**未覆盖**的范围。没有输出的
清单步骤不构成证据。

## 目前的验证范围

| 范围 | 状态 | 证据 |
| --- | --- | --- |
| 候选设备列表（文本与 JSON） | **已本地验证** | `--demo system targets` 打印四块带标注设备；无需 root、不访问设备节点。 |
| 预览：目标、被清除分区、SHA-256、警告 | **已本地验证** | 对 4 MiB MBR 夹具执行 `system plan`：`分区表: MBR`，入口与分区一一对应，两条强制警告齐全。 |
| `.img.xz` 预览（写入大小取自索引、"需要 xz"警告） | **已本地验证** | 对 LZMA 压缩夹具执行 `system plan`：文件 768 B 而 `写入大小: 4.00 MiB`。 |
| 预演不写盘 | **已本地验证** | `--demo system install … --acknowledge sda` 报告写入完成并打印演示提示；`--demo` 永不触达设备。 |
| 键入名称与空回答闸门 | **已本地验证** | `acknowledgement_mismatch` / `confirmation_required`，退出码 1，消息含期望值与实际值。 |
| 承载运行系统与已挂载目标闸门 | **已本地验证** | 对承载 `/`、`/boot` 的演示 SD 卡得到 `writes_running_system`；挂载判定由注入根用例覆盖。 |
| 请求校验（路径、符号链接、空文件、无 MBR/GPT、未知设备、超容量） | **已本地验证** | 每种情况均退出码 1、`invalid_install_request`，消息具体。 |
| 缺少 `xz-utils` | **已本地验证** | 在 `PATH` 移除 `xz` 后得到 `install_unsupported` 与 "install xz-utils to write a compressed image"。 |
| core 行为 | **已本地验证** | 22 个 `system_install` 用例加上 workspace 全量测试（`cargo test --workspace --locked`），全部使用注入后端，不触碰真实设备节点。 |
| HTTP 约定（路由、payload、状态码、目录与边界规则） | **已本地验证** | 2026-10-13 对 demo 服务器实测（`--demo serve --listen 127.0.0.1:8799`，`RSETUP_IMAGE_DIR`）：targets 200、plan 200、错名/空名 409、正确名称但未启用 live 422、令牌复用 409、穿越/符号链接/无分区表 id 400、承载运行系统目标 409、缺少 `x-rsetup-request` 403。 |
| TUI 面板（入口、焦点路由、后果清单渲染、由 core 判定的确认） | **由测试覆盖** | `cargo test --workspace --locked`（app 套件）含 `TestBackend` 的渲染与按键路由用例；本文未手工驱动。 |
| Tauri 命令（`system_install_targets/plan/apply`） | **由测试覆盖** | 桌面 crate 测试：经 mock 运行时真实派发、必须带 acknowledgement、注册表守卫完好。本文未启动 GUI。 |
| 浏览器页面（后果清单、键入名称门控、payload 形状、逐码文案） | **由测试覆盖** | `node --test ui/*.test.mjs` → 88 passed、0 failed（其中 `ui/system-install.test.mjs` 12 个用例）。本文未在浏览器中点击。 |
| 在 Cubie A7Z 上真实写入 UFS/eMMC | **已真机验证**（2026-10-14） | 把 10 GiB 镜像写入 119.1 GiB、4096 字节扇区的 UFS 后，板卡从该 UFS 启动，`sda3` 自动从 9.5G 增长到 118.6G——见步骤 5 之后的背景说明。仍标记 **待真机验证** 的各行仍无硬件运行记录。 |
| 通过 TUI、HTTP、Tauri 或 Web 界面在板卡上真实写入 | **待真机验证** | 四者都需要在主机上启用 live 执行；从未在硬件上执行过。 |
| 真实设备的 `BLKRRPART` 重读与写入后校验行 | **已真机验证**（2026-10-14） | 对 Cubie A7Z 的 UFS（119.1 GiB、4096 字节扇区）执行过两次写入：内核**拒绝**重读并返回 `Invalid argument (os error 22)`（EINVAL），因此写后校验行输出 `the kernel could not re-read the partition table of /dev/sda after the image was written, so the write could not be verified: … Invalid argument (os error 22)` —— 这是 `a310400` 之后的诚实文案（修复前同一情形被误报为"不一致"）。写入本身已由独立手段确认：`dd` 回读前 64 MiB 与末 16 MiB，与解压后的镜像逐字节相同。同一内核下在 shell 里执行 `blockdev --rereadpt /dev/sda` 反而成功，因此这是该设备的特性，不是镜像或本工具的问题。 |
| 可移动介质警告、对真实 `/proc/mounts` 的挂载冲突 | **已真机验证**（部分） | 已验：对板卡真实 sysfs 的设备枚举；基于真实 `/proc/mounts` 的运行系统拒绝（`mmcblk0` → `writes_running_system`）。**未验**：目标不是运行系统、但其某分区已被挂载的真正 mount 冲突——该路径目前仍仅由注入夹具覆盖。 |
| 写入中断后的恢复 | **待真机验证** | "不回滚"行为由失败夹具写入器测试；真实恢复流程未测试。 |

## CI 能否做设备级测试

本功能的自动化测试**只到夹具层**，上文依赖硬件的步骤绝不可变成 CI 门禁：

- CI 运行器是非特权容器或虚拟机。挂载 loop 设备需要 root 与支持块设备的宿主机，且项目
  在 CI 中没有真实 SBC、U 盘或 UFS/eMMC 目标。声称"写入镜像"的测试实际上只会写到文件，
  而不是设备。
- 开发主机 macOS 完全没有 `losetup`，而 `BLKRRPART` 是 Linux ioctl。
- 因此测试驱动的是注入的设备根（夹具目录下的 `/sys/class/block`、`/proc/mounts`、
  `/dev/<name>`）、注入的镜像打开器与合成后端。夹具根永远无法触及真实 `/dev`。
- `debian/tests/smoke` 有意保持表层化：只断言打包后的 CLI 暴露命令面，并在演示模式下
  稳定复现错误码。没有任何一步接触块设备。

结论：CI 全绿只能证明闸门、解析与流式写入逻辑，**不能证明任何真实板卡行为**。硬件结果
记录在 `docs/testing/`，只有届时才能把上表某行升级为"真机已验证"。

## 如何复现主机侧检查

所有标记为**已本地验证**的项目都可以在无板卡、无 root 的情况下复现：

```sh
# 构建一次
cargo build -p rsetup-next --locked

# 只读命令面
./target/debug/rsetup-next --demo system targets
./target/debug/rsetup-next --demo system targets --json

# 生成一次性 MBR 夹具，然后预览与预演
python3 - <<'PY'
import struct
buf = bytearray(4 * 1024 * 1024)
e = bytearray(16); e[4] = 0x83
struct.pack_into('<I', e, 8, 2048); struct.pack_into('<I', e, 12, 1024)
buf[446:462] = e
buf[510:512] = b'\x55\xaa'
open('/tmp/fixture.img', 'wb').write(bytes(buf))
PY
./target/debug/rsetup-next --demo system plan --target sda --image /tmp/fixture.img
./target/debug/rsetup-next --demo system install --target sda --image /tmp/fixture.img --acknowledge sda

# 缺少 xz 的路径（任意不含 xz 的 PATH）
env PATH=/usr/bin:/bin "$PWD/target/debug/rsetup-next" --demo system install \
  --target sda --image /tmp/fixture.img.xz --acknowledge sda --json

# 闸门与校验用例
./target/debug/rsetup-next --demo system install --target sda --image /tmp/fixture.img --acknowledge mmcblk0
./target/debug/rsetup-next --demo system install --target sda --image /tmp/fixture.img < /dev/null
./target/debug/rsetup-next --demo system install --target mmcblk1 --image /tmp/fixture.img --acknowledge mmcblk1
./target/debug/rsetup-next --demo system plan --target sda --image relative.img
./target/debug/rsetup-next --demo --live-execution system install --target sda --image /tmp/fixture.img --acknowledge sda

# 界面测试
node --test ui/*.test.mjs

# 对真实 demo 服务器验证 HTTP 界面（即上文引用的那次运行）
mkdir -p /tmp/rsn-images && cp /tmp/fixture.img /tmp/rsn-images/demo.img
RSETUP_IMAGE_DIR=/tmp/rsn-images ./target/debug/rsetup-next --demo serve --listen 127.0.0.1:8799 &
curl -s http://127.0.0.1:8799/api/v1/storage/install/targets
curl -s -H 'x-rsetup-request: 1' -H 'content-type: application/json' \
  -d '{"targetId":"sda","imageId":"demo.img"}' \
  http://127.0.0.1:8799/api/v1/storage/install/plan
```

`cargo test --workspace --locked` 运行自动化套件；`crates/rsetup-core` 的
`system_install` 用例、`crates/rsetup-app` 的 CLI/TUI/HTTP 用例、桌面 crate 的命令用例，
以及浏览器页面的 Node 测试，共同构成这一功能的回归网。

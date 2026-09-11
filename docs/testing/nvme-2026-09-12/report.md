# NVMe 磁盘状态监测功能 实机测试报告

日期：2026-09-12。测试对象：`rsetup-next` 的 NVMe 磁盘状态监测与 SMART 遥测功能（核心库、CLI、HTTP API、TUI、Web GUI）。范围：只读观测，全部在 `RSETUP_EXECUTION=dry-run` 下进行，未执行任何硬件或系统写入。

## 当前结论

功能在"有 NVMe"与"无 NVMe"两类实机上均按设计工作，**四项验收标准全部达成**。

测试期间通过实机视觉判定发现并修复了 **2 项真实产品缺陷**（TUI 可用备用空间数值被裁切、Web 序列号被省略号截断），以及 3 项测试工具链缺陷。测试还揭示了一项规格偏差——原设计要求的 `libnvme` 静态链接在实现中并未真正生效；经决策**移除 `libnvme` 全部相关逻辑并修订文档**（见"规格偏差与处置"一节）。所有修复均已回归。

代码尚未提交推送，本报告基于工作区状态。

## 测试基线

- 起始提交 `7728a51`；功能提交 `5e2f544`（核心 + CLI + API + Web）、`6026219`（TUI 设计规范与 TDD 计划）、`c0e1763`（TUI 实现 + 截图工具）。本次测试期间产生的工作区改动（序列号截断修复、打码与校验脚本）尚未提交。
- 构建方式：本机 x86_64 交叉编译 `aarch64-unknown-linux-gnu` release，链接器 `aarch64-linux-gnu-gcc`。
- 被测二进制 SHA-256（每次部署前重建，未逐次记录，属本报告局限）。

### 测试设备

| 设备 | 主机 | 型号 / SoC | 内核 | NVMe 硬件 |
| --- | --- | --- | --- | --- |
| Rock 5B | `192.168.27.88` | Radxa ROCK 5B / RK3588 · aarch64 | `6.1.115-vendor-rk35xx` (Armbian) | **有**：ZHITAI TiPlus7100 1TB，`/dev/nvme0`，1024209543168 B，固件 `ZTA22006` |
| Rock 3A | `192.168.27.34` | Radxa ROCK 3A / RK3568 · aarch64 | `6.18.44-current-rockchip64` (Armbian) | **无**：`/sys/class/nvme` 为空目录，无 `/dev/nvme*` |

两台设备均通过 SSH + PTY 驱动；Web 服务仅监听 `127.0.0.1`，经 SSH 隧道映射到测试机（88→19088，34→19089），未开放局域网监听。

## 已完成验证

| 层级 | 结果 | 边界 |
| --- | --- | --- |
| Rust 单元/集成测试 | `cargo test --workspace`：130 passed、0 failed、2 ignored（2 项为既有的环境门控用例，非本次新增） | 不代表硬件电气行为 |
| 其中 NVMe 相关用例 | 23 项（按 `--list` 逐条核对），覆盖 SMART 字节解析、告警掩码、饱和溢出、sysfs 探测与条件初始化、Demo/实机 Controller 聚合、CLI 参数解析与格式化、TUI 渲染 | — |
| UI 测试 | `node --test ui/*.test.mjs`：47 passed、0 failed，其中 `ui/nvme.test.mjs` 4 项 | 渲染断言基于 DOM 字符串，非像素 |
| CLI（实机，有 NVMe） | `hardware nvme` 中英文、`hardware nvme --json` 均正常；数值与 sysfs / `blockdev` 一致 | — |
| CLI（实机，无 NVMe） | 输出"未检测到 NVMe 存储设备，模块未激活（No NVMe controller detected in system）"，退出码正常，无 panic | — |
| HTTP API | `GET /api/v1/hardware/nvme` 在两类实机均 200，响应体为规范 `NvmeStatus`；`status --json` 的 `nvme` 能力项在 3A 为 `available:false` | 仅 GET，未覆盖写路径 |
| TUI（实机） | 两类实机均正常启动、`:q` 正常退出；有 NVMe 时展示型号/容量/温度/寿命/备用；无 NVMe 时降级为单行提示并自适应收窄卡片 | 经 SSH PTY 采集，非物理控制台 |
| Web（实机） | 桌面 1440×900 与移动 390×844（@2x）均正常；NVMe 抽屉展示完整 SMART 遥测；无 NVMe 时卡片置灰且不泄漏他机数据 | 浏览器控制台无 error |
| 二进制依赖 | release 二进制 `readelf -d` 仅依赖 `libgcc_s.so.1`/`libm.so.6`/`libc.so.6`，无 `libnvme.so.1`；仓库内无 `libnvme` 引用 | 采集经 ioctl 直读，无 `libnvme` 依赖（见"规格偏差与处置"） |

### 实机读取一致性抽样

以 Rock 5B 为例，三处独立读取结果一致：

| 字段 | sysfs / blockdev | CLI `--json` | Web API |
| --- | --- | --- | --- |
| 型号 | `ZHITAI TiPlus7100 1TB` | 同 | 同 |
| 固件 | `ZTA22006` | 同 | 同 |
| 容量 | `1024209543168` B | 同 | 同 |
| 综合温度 | — | 43.9–45.9 °C（随负载变化） | 43.9 °C |
| 可用备用 / 阈值 | — | 100% / 1% | 100% / 1% |

温度与读写计数随运行时间自然漂移，故不要求跨次采集严格相等。

## 测试中发现并修复的缺陷

### 产品缺陷

**D1（已修复）TUI 可用备用空间数值被整段裁切。**
实机截图显示 NVMe 卡片第三行止于 `备用空间:`，数值与阈值不可见。根因：左栏 59 显示列 + 卡片仅 3 行内高，`备 用 空 间: 100% (阈值 1%)` 超出宽度后换行溢出被裁。
修复：备用值上移至第 2 行、分隔符改为单空格，卡片内高提升至 4 行内容（`render_mission` 中 `desired_nvme_height` 由 5 改为 6）。
回归锁定由两个测试分担，两者职责不同：`test_render_nvme_summary_fits_available_spare` 直接以 61×6 矩形渲染卡片，锁定 59 显示列内 `100%` 与阈值 `1%` 可见；`test_render_full_tui_with_nvme_zh` 走完整 `render` 流程，锁定整屏布局下的同一性质。
**红绿验证（本报告实测）**：将 `desired_nvme_height` 临时改回 5 后，`test_render_full_tui_with_nvme_zh` **失败**（`7 passed; 1 failed`），`fits_available_spare` 仍通过（因其不经 `render_mission`，不受面板高度影响）；恢复为 6 后全部通过。已确认 `crates/rsetup-app/src/tui.rs` 还原至提交状态。

**D2（已修复）Web 序列号被省略号截断。**
实机截图显示序列号渲染为 `ZTA71T0AB252410…`。CDP 实测根因：`.nvme-spec-item b` 为 `nowrap + ellipsis`，4 列网格每格 `client=117px`，而 18 位序列号需 `scroll=130px`；移动端更严重（`100px`）。
第一次修复（`grid-column: span 2`）实测行 1 恰好填满但 `综合温度` 单独落行、留下 **3 个空槽**，故被否决；最终改为与相邻 `.nvme-metrics-grid` 对齐——桌面 2 列、窄屏 1 列。修复后实测：桌面 2×2、每格 302px（需 130px）、零空槽；移动端单列 268px（client 240px）、零空槽。

### 测试工具链缺陷（非产品问题）

**D3（已修复）TUI 截图中文全部渲染为方块。** 渲染脚本回退到 `DejaVuSansMono.ttf`，该字体无 CJK 字形。改用 `Noto Sans Mono CJK SC` 并实现东亚宽字符占 2 格的处理，单元格几何由字体制表推导。验证方式为像素级比对：从截图裁出单字与直出字形位图求 IoU（`状` 0.581）、与空心 tofu 框求 IoU（0.047）。
**D4（已修复）Rock 3A 截图未拍到 NVMe 卡片。** 该卡片在能力矩阵中排第 7，落在 900px 折叠线以下；采集脚本增加 `scrollIntoView`。
**D5（已修复）序列号暴露于测试产物。** 3 张 Web 截图与 3 个 sidecar JSON 含真实序列号。截图按浏览器实测的字形运行区做像素级实心打码（未采用部分打码，理由见脚本注释），sidecar 中的值一并替换，并从两个脚本中移除硬编码字面量，改为泛化匹配。

### 判定过程中的假阴性（已排除，非产品缺陷）

- `ZTA22006`（固件）在全页 OCR 中被读作 `ZTA22666`；对精确元素矩形裁剪并 4 倍放大二值化后读回 `ZTA22006`，与 DOM 及 sysfs 一致。
- 移动端 `综合温度` 标签在全页 OCR 中整行丢失；对元素矩形精确裁剪后 OCR 得到 `综合瘟度`（`度` 被误读），证明标签确实渲染。
结论：这两项是 11–12px 等宽字下 tesseract 的识别极限，非渲染缺陷。判定脚本据此改为分层证据，并明确标注不再从栅格断言最小字号字段的精确值。

## 验收标准对照

| 标准（原始计划第 4 节） | 结果 |
| --- | --- |
| 1. `cargo test --workspace` 全量通过，新增用例覆盖核心逻辑 | **达成**：130 passed / 0 failed；NVMe 用例 23 项 + UI 4 项 |
| 2. 无 NVMe 主机：模块正确侦测并保持未初始化，CLI/Web 不报错 | **达成**：Rock 3A 上 CLI、API、TUI、Web 四层均正常降级 |
| 3. 有 NVMe 或 Demo 模式：正确读取展示基础信息与 SMART 指标 | **达成**：Rock 5B 实机四层均展示，且与 sysfs/blockdev 交叉一致 |
| 4. 二进制不得依赖 `libnvme`（原标准为"静态链接 libnvme 且 `ldd` 不强制依赖 `libnvme.so.1`"，2026-09-12 修订） | **达成**：`readelf -d` 动态依赖仅 `libgcc_s.so.1`/`libm.so.6`/`libc.so.6`；仓库内已无任何 `libnvme` 构建期或运行期引用 |

## 规格偏差与处置

测试发现规格要求的"通过 `libnvme` 静态链接读取"与实际实现不符。已确认的事实：

- `crates/rsetup-core/src/nvme/sys.rs` 通过 Linux Admin Passthru `ioctl`（`NVME_IOCTL_ADMIN_CMD = 0xc0484e41`，opcode `0x02`，LID `0x02`）直接读取 512 字节 SMART/Health Log，指标在进程内解析；
- 全部源码中不存在 `extern "C"` 或 `#[link]` 声明，即**无任何 libnvme FFI 符号被引用**；
- 原 `crates/rsetup-core/build.rs` 会探测 `libnvme.a`/`libnvme.so` 并发出链接指令，并输出 `cargo:rustc-cfg=feature="libnvme"`，但由于没有任何符号被引用，**该链接实际不生效，且该 cfg 在全部源码中从未被消费**（属纯死代码）；
- 交叉编译时本机 `/usr/lib/aarch64-linux-gnu` 下无 `libnvme`，即静态链接路径在目标架构上本就无法成立。

**处置（2026-09-12）**：经决策采用 `ioctl` 方案并**移除全部 `libnvme` 相关逻辑**——`crates/rsetup-core/build.rs` 已整体删除（`git rm`）。文档同步修订：

| 文档 | 修订内容 |
| --- | --- |
| `docs/superpowers/specs/2026-09-10-nvme-monitoring-design.md` | 概述与核心约束改为"零外部依赖的采集路径"；原 2.2「静态编译策略」重写为「采集路径与依赖策略（无外部库）」，含不用 `libnvme` 的三条理由 |
| `docs/superpowers/plans/2026-09-10-nvme-monitoring-tdd-plan.md` | 阶段 3 标题与任务表加删除线并附「实施偏离说明」；验收标准 4 由"静态链接验证"修订为"二进制不得依赖 `libnvme`"（原文保留以免伪造历史） |
| 本报告 | 本节由"待决策偏差"改为"已处置"，验收标准 4 判定同步更新 |

该偏差不影响功能正确性：`ioctl` 路径在 Rock 5B 实机上读取的温度、备用空间、寿命与读写统计均与 sysfs / `blockdev` 交叉一致，且产物对目标 SBC 更自包含（无需安装 `libnvme.so.1`，交叉编译无需目标架构开发包）。

## 未验证边界

1. **图像判定非人工复核。** 控制端模型为纯文本模型，无法直接查看图片；视觉判定由一个支持图像输入的模型（`deepseek-v4-flash-vision-exp`）完成，关键结论（序列号截断）由我在浏览器中独立实测 DOM 几何复核，其余视觉结论未经人工确认。
2. **仅一种 NVMe 型号。** 只测了 ZHITAI TiPlus7100 1TB；未覆盖多盘、多命名空间、PCIe 热插拔、非 512 字节 LBA、异常/告警态磁盘（`critical_warning != 0` 仅在单元测试中覆盖）。
3. **未验证告警渲染的真实硬件路径。** 告警色与告警标志展示仅由构造数据在测试中覆盖。
4. **未做持久化与长稳。** 无重启保持、无长时间轮询、无并发或高负载下的性能数据。
5. **权限路径未覆盖非 root。** 实机均以 root 运行；普通用户下 `/dev/nvme0` 读取失败时的降级行为仅在单元测试中以"设备节点不存在"模拟。
6. **Web 写路径未测。** 仅验证 GET；服务以 dry-run 启动，未验证任何变更接口。
7. **截图证据的时效性。** 温度、读写计数等为采集瞬间值，后续重跑不会完全一致。
8. **未记录二进制 SHA-256。** 每次修复后重建部署，未逐次留存哈希，无法从报告反查确切的被测二进制。

## 证据

截图位于 `screenshots/`：

| 文件 | 内容 |
| --- | --- |
| `tui-nvme-live-rock5b.png` | Rock 5B TUI，NVMe 卡片展示型号/容量/温度/寿命/备用/阈值 |
| `tui-nvme-live-rock3a.png` | Rock 3A TUI，卡片降级为"未检测到 NVMe 存储设备，模块未激活" |
| `web-nvme-rock5b-desktop.png` | Rock 5B Web 桌面 1440×900，NVMe 抽屉完整遥测，序列号已打码 |
| `web-nvme-rock5b-mobile.png` | Rock 5B Web 移动 390×844@2x，抽屉顶部 |
| `web-nvme-rock5b-mobile-metrics.png` | 同上，滚动至底部以显示下方 6 项指标 |
| `web-nvme-rock3a-desktop.png` | Rock 3A Web 桌面，NVMe 卡片置灰（此设备未检测到 / 当前不可用） |
| `web-nvme-rock3a-mobile.png` | Rock 3A Web 移动，同上 |

### 证据生成与复核工具

| 脚本 | 作用 |
| --- | --- |
| `scripts/capture-tui-remote.py` | SSH + 本地 PTY 采集远端 TUI 的 ANSI 流 |
| `scripts/capture-tui-screenshot.py` | ANSI → PNG，CJK 感知（等宽 CJK 字体、宽字符占 2 格、字体制表推导单元格） |
| `scripts/capture-web-screenshot.mjs` | headless Chrome + 自实现 CDP WebSocket 客户端（无三方依赖）驱动页面并截图；支持 `--no-click`、`--scroll-drawer-bottom`、`--eval` 探针 |
| `scripts/redact-screenshots.py` | 按浏览器实测字形区域对序列号做像素级打码，并清洗文本产物；不硬编码标识符 |
| `scripts/verify-web-screenshots.py` | OCR + 像素层打码校验，退出码 0/1，当前 5/5 PASS |

复核命令：

```bash
python3 scripts/verify-web-screenshots.py     # OCR + 打码校验，5/5 PASS
python3 scripts/redact-screenshots.py         # 打码（幂等，可重复执行）
```

## 复测建议

按优先级：第二种 NVMe 型号与告警态实机 → 普通用户权限降级路径 → 重启后的一致性与长稳轮询 → 记录被测二进制哈希。（原第 1 项"补齐 libnvme 静态链接决策"已于 2026-09-12 处置完毕，见"规格偏差与处置"。）

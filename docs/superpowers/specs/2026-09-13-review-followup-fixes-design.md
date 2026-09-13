# 近期提交复审问题修复规范（Spec）

- 日期：2026-09-13
- 代码基线：`473c78c8501e05f42eb779ad7891e1035c698341`
- 状态：已确认设计方向的待实施规范；不表示修复代码或新增测试已经完成。
- 对应计划：[TDD 实施计划](../plans/2026-09-13-review-followup-fixes-tdd-plan.md)
- 用户确认：修复本轮复审问题 **1/4/5/6/7**；问题 1 必须按 `target_arch` 条件编译，明确验证 ARM32、ARM64、x86、x86_64。

## 1. 范围与编号

本规范的编号来自对 `6d029be..473c78c` 的复审，**不沿用**此前存储健康规范的问题编号。

| 本轮编号 | 基线缺陷 | 目标 | 主要入口 |
|---|---|---|---|
| R1 | MMC ioctl 常量的 `u64` 表达式不能赋给 32 位 `c_ulong` | 四个目标架构编译通过，保持正确的 Linux MMC ABI | `crates/rsetup-core/src/mmc/sys.rs` |
| R4 | UI 调用的 `storage_status` 未在 Tauri 注册 | 桌面 Storage 可读、可刷新，返回契约与 REST 一致 | `apps/desktop/src-tauri/src/main.rs` |
| R5 | 普通轮询持续使在途刷新失效 | 慢请求能落地，同时保留操作后刷新的一致性 | `ui/app.js` |
| R6 | MMC unavailable 与 unsupported 展示混淆，CLI 声称无告警 | CLI/TUI/Web 都如实显示可用性及错误原因 | `main.rs`、`tui.rs`、`ui/app.js` |
| R7 | TUI 吞掉子系统枚举失败，显示未发现设备 | 分别显示 NVMe/MMC 枚举错误与成功结果 | `crates/rsetup-app/src/tui.rs` |

### 1.1 与已有规范的关系

继续遵守 [存储健康正确性规范](2026-09-13-storage-health-correctness-design.md) 的 JSON 模型、共享健康分类、按请求重新采集和只读约束。本规范补齐展示和桌面接入，并明确覆盖旧规范曾排除的 32 位请求常量与全局刷新队列。

旧规范的历史范围不回写；发生范围冲突时，本规范对 R1/R4/R5/R6/R7 的要求优先。

### 1.2 非目标

- 不处理本轮 R2/R3（EFI 回滚）、R8/R9（TUI 设备溢出提示及通用换行估算）。
- 不扩展存储协议，不引入缓存、后台采集、自动提权或设备写操作。
- 不改变 REST/JSON 错误模型或让 CLI 因单盘遥测不可读取而新增退出码。
- 不进行全局 TUI 布局重构；为 R7 的错误行预留局部空间属于必要改动。
- 本次产物是规范和计划，实施、提交和发布需另行执行。

## 2. 方案选择

| 议题 | 采用 | 未采用及原因 |
|---|---|---|
| R1 | 基于 `target_arch` 的显式 ABI 分支 | 单一 `as c_ulong` 强转不能体现用户指定的架构分支；屏蔽 ARM32 MMC 功能不满足目标 |
| R4 | 复用 Controller，通过 `spawn_blocking` 采集 | 新建桌面专用采集器会重复业务逻辑 |
| R5 | 普通刷新合并，操作后刷新显式失效 | 增大轮询间隔仍会在更慢请求下复发；所有结果都渲染会恢复操作前旧数据 |
| R6 | 复用已有 telemetry/healthState，在各展示端统一语义 | 从空 flags 或缺失百分比反推支持性仍会掩盖错误 |
| R7 | TUI 内保存每个子系统的当前错误，失败时清除该子系统旧列表 | 保留旧列表需要额外 stale 语义；扩展公共 StorageStatus 会不必要地改变 API |

## 3. 全局约束

- Rust MSRV 保持 **1.85.0**，不新增生产依赖。
- 明确验证目标：`armv7-unknown-linux-gnueabihf`、`aarch64-unknown-linux-gnu`、`i686-unknown-linux-gnu`、`x86_64-unknown-linux-gnu`。
- JSON 字段与枚举保持现有 `telemetry.state`、`telemetry.error.kind/code`、`healthState`；不修改 schema。
- `available` 不等于 `healthy`；`unavailable`、`unsupported` 的健康等级为 `unknown`。
- 权限错误必须如实呈现；不得自动提权、读取 SD 的 EXT_CSD 或执行硬件写操作。
- 新用户文案同时提供简体中文和英文，动态错误必须转义，切换语言后重新渲染。
- 测试必须运行实际函数或实际命令接线，不能用复制的业务实现证明修复。
- 环境缺少交叉目标、系统库、Tauri 或运行器时记录阻塞，不将未执行检查写成通过。

## 4. R1：按架构编译 MMC ioctl

### 4.1 ABI 分支

Linux 下按 `target_arch` 分为两组，二者互斥：

| Rust 架构值 | 请求表达式 | GNU Linux `libc::c_ulong` | `MMC_IOC_CMD` |
|---|---|---|---|
| `arm`、`x86` | 使用 `u32` 的移位/或运算 | 32 位 | `0xc048_b300` |
| `aarch64`、`x86_64` | 使用 `u64` 的移位/或运算 | 64 位 | `0xc048_b300` |

在调用 `libc::ioctl` 的边界继续匹配 libc 的请求参数类型。上述 GNU 目标不能通过把错误结果强转为预期值掩盖编译问题。

`read_ext_csd_raw(&str) -> Result<[u8; 512], MmcError>` 保持不变。在这四种架构上均保留真实只读 ioctl 路径。

对四种架构之外的 Linux 目标，不猜测其 ioctl 编码：只将原始 ioctl 入口条件编译为返回 `MmcError::NotSupported("MMC ioctl ABI is not implemented for this target architecture".into())` 的分支，且在打开设备前返回。sysfs 枚举、纯解析和注入 reader 仍可编译和使用；不宣称这些目标已获整仓支持。该错误按当前错误映射呈现为 `unavailable + io/code=None`，表示应用缺少读取路径，不宣称设备本身不支持健康指标。

### 4.2 结构体与编译证据

- `size_of::<MmcIocCmd>() == 72`。
- `offset_of!(MmcIocCmd, data_ptr) == 64`，显式 pad 保持不变。
- `data_ptr` 字段偏移是 8 的倍数；不能误要求 i686 上 `align_of::<MmcIocCmd>() == 8`，也不能把 `repr(C)` 描述为所有目标均保证 8 字节类型对齐。
- 每个目标编译时都检查常量值、大小和偏移。只在 x86_64 上跑运行时测试不足以验收 ARM32。
- 不通过手工覆盖内置 `target_arch` cfg 假装交叉编译；使用真实 `--target`。

## 5. R4：桌面 Storage 接入

新增并注册命令：

```rust
#[tauri::command]
async fn storage_status(
    controller: tauri::State<'_, Controller>,
) -> Result<rsetup_core::StorageStatus, CommandError>
```

命令 clone 已管理的 Controller，将 `controller.storage_status()` 放入 `tauri::async_runtime::spawn_blocking`。worker join 错误使用 `CommandError::internal`，`HardwareError` 使用已有 `CommandError::from`。

要求：

1. 名称与 `ui/app.js` 的 `tauriInvoke("storage_status")` 完全一致，加入实际 `generate_handler!`。
2. 返回 `StorageStatus { nvme, mmc }`，保持序列化字段、Demo 模式与 Controller 行为；不在 handler 中重新构造 Controller。
3. 同一桌面进程连续调用会重新查询，不引入启动缓存。
4. 保留现有 live opt-in；Demo 调用不访问真实设备。
5. 通过实际 Tauri dispatch 的测试覆盖注册，不仅测试孤立 helper；增加低成本 UI/注册表一致性检查作为辅助。

## 6. R5：普通刷新与操作后失效

### 6.1 接口与调用方

扩展为 `refreshAll({ quiet = false, invalidate = false } = {})`。

- `quiet` 只控制提示与动效，不代表一致性要求。
- 启动、10 秒定时器和手动刷新使用 `invalidate: false`。
- 在途普通刷新被后续普通刷新复用，不排入无意义的新批次，不使已有结果失效。
- 完成或收到已有流程需重新读取状态的变更操作后，调用 `refreshAll({ quiet: true, invalidate: true })`。
- 审计全部调用点：任务执行、软件源应用、Overlay、温控、风扇曲线、LED、SPI。失败/取消路径沿用原行为，不额外启动操作。
- `refreshStorageTool()` 仍独立触发；存储请求挂起不能阻塞全局快照刷新。

### 6.2 状态机

保留一个 drain Promise 与 `refreshRequested` 待处理标记，新增单调递增的 `refreshEpoch`（初始 0）。

1. 无在途 Promise：普通或失效刷新均排入一批读取。
2. 有在途 Promise + 普通刷新：返回等待当前 drain 的 Promise，不改变 epoch、不排队。
3. 失效刷新：epoch 加一并设置待处理标记；若已有 drain，则复用它。
4. 每批读取开始捕获 epoch，发出原来的四个并行请求。
5. 响应成功或失败时先比较 epoch：旧批次不得修改快照、数据派生状态、toast、错误状态或成功时间。
6. 有新的失效刷新排队则继续读取；同一在途批次中的多次失效可合并为下一批。
7. 没有待处理请求则 drain 完成，所有等待者结束，清理刷新状态和 Promise。

保证在网络请求最终完成、变更操作次数有限的前提下，普通轮询不能导致饥饿。持续发生真实变更时丢弃变更前数据是正确行为，本规范不承诺永远显示过期数据来保证完成。

### 6.3 必须覆盖的时序

| 情况 | 预期 |
|---|---|
| 一批请求跨越四次轮询 tick | 仅一批请求；成功结果渲染一次；等待者结束 |
| 读取 A 在途，变更完成并发出失效请求，A 成功 | A 不渲染，启动 B，等待者直到 B 完成 |
| 同上但 A 失败 | 不展示 A 的过期错误；仍读取 B |
| A 在途出现多次失效，B 开始后再失效 | A 之后一批 B；B 过期后继续 C |
| 最新批次失败 | 显示真实错误、清理状态；后续重试能成功 |
| 存储抽屉请求不完成 | 快照 drain 仍可结束 |

## 7. R6：MMC 展示真值表

三端保留静态身份信息，先判断 telemetry，再解释健康指标。JSON 输出不修改。

| telemetry | 健康等级 | 指标与告警展示 |
|---|---|---|
| available，指标有效 | 使用已有 `healthState` | 保留数值与告警；只有成功读取且健康信息有定义时才可显示“无/None” |
| available，部分字段未定义 | 使用已有 `healthState` | 缺失字段显示“未定义/Undefined”，不能标注整个功能不支持 |
| available，全部健康字段未定义 | unknown | 显示未定义；告警显示“未知/Unknown”，不宣称健康或无告警 |
| unsupported | unknown | “不支持健康指标/Health telemetry unsupported”；不显示默认正常值、进度条或无告警结论 |
| unavailable，带错误 | unknown | “不可读取/Unavailable”及原因、存在的错误码；不显示虚构的健康指标 |
| 缺少 telemetry 或 unavailable/error=null | unknown | 通用不可读取；不捏造 errno、协议状态或不支持结论 |

### 7.1 文案及格式

复用已有词条；缺少的文案添加到 Rust `i18n.rs` 和 Web `i18n.js`：

| 语义 | 中文 | 英文 |
|---|---|---|
| permission_denied | 权限不足 | Permission denied |
| io | 读取失败 | Read failed |
| 未定义字段 | 未定义 | Undefined |
| 未知健康或告警 | 未知 | Unknown |
| NVMe 枚举失败 | NVMe 设备枚举失败 | NVMe device enumeration failed |
| MMC 枚举失败 | MMC 设备枚举失败 | MMC device enumeration failed |

错误码为 `Some(code)`/非 null 时附加 ` (code)`，例如 `权限不足 (13)`。没有错误码不输出 `(0)`。CLI/TUI 可以共用 Locale 格式化方法；Web 独立实现相同语义，无需创建跨语言协议。

Web 的抽屉请求失败/stale 是更外层状态：仍保留 stale 警示和 unknown 徽章；单盘错误说明可显示为上次数据的一部分，但不能覆盖请求级错误或冒充本次采集结果。动态文本继续使用 `escapeHtml`。

## 8. R7：TUI 子系统枚举状态

在 App 中增加两个独立的 `Option<HardwareError>` 字段，分别对应 NVMe 和 MMC。它们是本地展示状态，不进入公共 JSON。

- `App::new` 与 `App::refresh` 使用一致的结果处理路径。
- 查询成功：替换该子系统 status，清除其 error，包括成功但空列表的结果。
- 查询失败：保存本次 error，清空该子系统旧设备；另一子系统仍照常查询和展示。
- 不依赖 `Status.message` 的字符串内容判断错误，不仅写入可能被其他界面覆盖的通用 notice。
- 渲染先显示子系统错误，再显示成功设备；为错误摘要预留空间并计入存储卡片所需高度。较小卡片也应优先显示错误类别，详情可截断。
- 只有两边查询均成功且设备列表均为空时，才显示 `storage_not_detected`。
- 保持 TUI 可交互和按 `r` 重试；存储枚举失败本身不令 App 退出。

| NVMe 查询 | MMC 查询 | 卡片结果 |
|---|---|---|
| 空成功 | 空成功 | 未发现存储设备 |
| 失败 | 空成功 | NVMe 枚举错误，无“未发现”结论 |
| 有设备 | 失败 | NVMe 设备 + MMC 枚举错误 |
| 失败 | 有设备 | NVMe 枚举错误 + MMC 设备 |
| 失败 | 失败 | 两个独立错误 |
| 上次失败，本次成功 | 任意 | 相应旧错误清除 |
| 上次成功，本次失败 | 任意 | 相应旧设备清除，以当前错误替代 |

R7 的枚举错误与 R6 的单盘遥测错误必须分别测试：前者查询返回 Err，后者查询成功且设备携带 unavailable。

## 9. 验收与证据

| 验收 ID | 内容 | 计划任务 |
|---|---|---|
| A1 | 四架构真实交叉 check，MMC ABI 常量/大小/偏移断言 | Task 1 |
| A4 | 桌面 IPC dispatch、注册一致性、Demo 和错误传播 | Task 2 |
| A5 | 慢轮询不饥饿、操作后失效、旧失败丢弃、重试 | Task 3 |
| A6 | CLI/TUI/Web 中英文真值表，无虚假无告警，错误码保真 | Task 4 |
| A7 | 同一 App 启动、部分失败、重试恢复与旧设备清理 | Task 5 |
| AX | 原有刷新/健康/只读防线保持，全量基础回归 | Task 6 |

实施记录必须列出 RED 时具体断言或编译错误、GREEN 命令及退出结果。依赖缺失造成的编译失败不是功能回归测试的 RED。

既有复审结果中的 65 项 UI 测试通过仅为历史基线，不作为本规范实施后的通过证据。

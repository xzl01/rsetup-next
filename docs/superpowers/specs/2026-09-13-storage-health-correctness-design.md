# 存储健康正确性修复设计规范（Spec）

- 日期：2026-09-13
- 基线：`dev-aghost` / `bf7b485`
- 状态：待实施设计稿；不代表代码已修复或新增测试已通过。
- 对应计划：[TDD 实施计划](../plans/2026-09-13-storage-health-correctness-tdd-plan.md)
- 用户确认：允许 NVMe `smart=null`，显式表达读取失败，不保留全零 SMART 兼容占位。

## 1. 目标与范围

本规范只覆盖审查问题 **1/2/4/5/6/7**：

| 编号 | 当前问题 | 修复目标 |
|---|---|---|
| 1 | Manager 只返回启动时缓存，Web 抽屉只打开时读取 | 同一进程每次查询重新采集；Web 可原位刷新 |
| 2 | NVMe 错误转换为默认 SMART，MMC/SD 无健康信息仍显示健康 | 分离读取可用性与健康等级；未知不冒充正常 |
| 4 | NVMe ioctl 正数 completion error 被当作成功 | 仅 ret=0 成功；保留正数协议状态与负数系统错误 |
| 5 | pre_eol_warning 被 UI/TUI 升级为 critical | 统一等级判定，真实预警保持 Warning |
| 6 | 0x0A 的寿命区间被标记 exceeded | 0x0A=90–100% 已用；0x0B 才超过估计寿命 |
| 7 | SD 无健康字段也触发 eMMC EXT_CSD CMD8 | 只有 MMC 允许 EXT_CSD fallback |

### 非目标

- 不修问题 3：32 位 `MMC_IOC_CMD` 常量类型不匹配。
- 不新增依赖、后台采集服务、TTL 缓存、自动提权、设备写操作或压力/磨损测试。
- 不扩展 USB/SATA 支持，不重写 TUI 布局，不整理无关代码或修改其它工具的刷新队列。
- 不宣称 eMMC sysfs 一定来自即时芯片采样：部分内核会缓存 EXT_CSD；应用只能保证重新读取当前内核暴露值。

## 2. 现状与方案选择

当前 `actions.rs` 创建两个 Manager，`nvme.rs`/`mmc.rs` 保存 `status` 并在查询时 clone。`nvme/sys.rs` 吞掉 SMART 读取错误，`ui/app.js` 与 `tui.rs` 又分别根据 flags 推导健康。数据采集生命周期、可用性和显示等级需要同时修正。

| 方案 | 优点 | 代价 | 结论 |
|---|---|---|---|
| 按需重新采集 | 简单；每次请求都可观察新值；无需后台生命周期 | 每次查询产生一次读取成本 | **采用** |
| TTL 缓存 | 降低请求量 | 仍有陈旧窗口，需定义强制刷新及失败缓存 | 不采用 |
| 后台采集 | 读接口快速 | 引入任务、停止、调度、同步和过期策略 | 本轮过重，不采用 |

Web 的单抽屉请求去重只合并正在进行的工作，不是存储数据缓存。多个独立客户端可能产生多个只读采集请求；本轮不引入全局调度器。

## 3. 核心模型与 JSON 契约

### 3.1 读取状态与健康等级分别建模

在 `model.rs` 新增并从 `lib.rs` 导出：

```rust
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryReadState {
    Available,
    Unsupported,
    #[default]
    Unavailable,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthState {
    #[default]
    Unknown,
    Healthy,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryErrorKind { PermissionDenied, Io, NvmeStatus }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryError {
    pub kind: TelemetryErrorKind,
    pub code: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryStatus {
    pub state: TelemetryReadState,
    pub error: Option<TelemetryError>,
}
```

修改 Device 模型：

- `NvmeDevice.smart: Option<NvmeSmartLog>`，新增 `telemetry: TelemetryStatus`、`health_state: HealthState`。
- `MmcDevice.health: MmcHealth` 保持原结构；新增相同的 `telemetry`、`health_state`。
- 新增两个字段使用 `#[serde(default)]`。缺少新字段时读取状态为 Unavailable、等级为 Unknown，不从旧数据猜测读取成功。
- JSON 字段为 `healthState`；枚举值为 `unknown/healthy/warning/critical`。
- 不增加 source、芯片采样时间或服务端更新时间字段。Web 可显示客户端最后收到成功响应的时间，但必须标注为“刷新时间”。

### 3.2 不变量

1. NVMe 只有读取和解析成功才输出 `smart: {...}`、Available；失败必须 `smart:null`、Unavailable、Unknown。
2. Available 表示读取路径成功，不等于 Healthy。MMC 成功读取但字段全未定义时为 Available + Unknown。
3. Unsupported 表示当前设备/协议不提供该健康功能，如 SD 或 EXT_CSD rev<7。权限不足不能报 Unsupported。
4. 实际采集失败的 Unavailable 必须带错误分类；只有缺少新字段的兼容默认可为 `error:null`。
5. `initialized` 仍表示发现设备，不是健康读取成功。一块盘无法读健康时仍保留该盘标识及已知静态属性。
6. 不保留上次健康数据冒充本次成功：本次失败返回 null/未知；Web 若保留旧响应，只能明确标为过期。

错误映射：EACCES/EPERM → `permission_denied`，其它 errno → `io`，NVMe 正数 completion status → `nvme_status`。`code` 原样保留 errno 或协议状态；没有数值的上下文错误用 `null`。不向用户请求自动提权。

### 3.3 JSON 示例

NVMe 设备存在，但没有读取权限：

```json
{
  "name": "nvme0",
  "path": "/dev/nvme0",
  "model": "FIXTURE SSD",
  "serial": "test-only",
  "firmware": "1",
  "totalBytes": 4096,
  "smart": null,
  "telemetry": {
    "state": "unavailable",
    "error": { "kind": "permission_denied", "code": 13 }
  },
  "healthState": "unknown"
}
```

SD 设备的健康部分：

```json
{
  "health": {
    "preEolInfo": 0,
    "lifeTimeEstAPercent": null,
    "lifeTimeEstBPercent": null,
    "warningFlags": []
  },
  "telemetry": { "state": "unsupported", "error": null },
  "healthState": "unknown"
}
```

eMMC 预警必须保持真实 flag 组合：

```json
{
  "health": {
    "preEolInfo": 2,
    "lifeTimeEstAPercent": 10,
    "lifeTimeEstBPercent": 10,
    "warningFlags": ["pre_eol_warning"]
  },
  "telemetry": { "state": "available", "error": null },
  "healthState": "warning"
}
```

### 3.4 兼容性与发布

`smart` 从对象变为可空是有意的契约变化。CLI JSON 使用者须在读取指标前检查 telemetry 和 null。Rust 三端、Web、Demo 及所有 fixtures 必须同一批迁移；不允许旧消费者继续显示默认健康。旧 JSON 未提供新字段时，新消费者 fail-closed 为 Unknown。本轮不恢复已删除的 `/api/v1/hardware/nvme`，统一接口仍为 `/api/v1/hardware/storage`。

## 4. 统一健康判定

新建 `health.rs`，由核心生产者计算 `health_state`，前端不再维护另一套阈值逻辑：

```rust
pub fn nvme_health_state(
    telemetry: &TelemetryStatus,
    smart: Option<&NvmeSmartLog>,
) -> HealthState;

pub fn mmc_health_state(
    telemetry: &TelemetryStatus,
    health: &MmcHealth,
) -> HealthState;
```

消费者可防御性将“不兼容或不可用的数据却携带 healthy”降级 Unknown，但不得自行把普通 Warning 升级 Critical。

### 4.1 NVMe 优先级

| 条件（从上往下匹配） | 等级 |
|---|---|
| telemetry 不为 Available，或 smart=None | Unknown |
| critical_warning 非零（包括未知位） | Critical |
| critical_warning=0，但 warning_flags 非空 | Warning |
| 有有效 SMART 且没有告警 | Healthy |

本轮不重定义 NVMe 温度阈值策略。真实读取的零计数仍是合法值，不能因值为零而误判读取失败。

### 4.2 MMC 优先级

| 条件（从上往下匹配） | 等级 |
|---|---|
| telemetry 不为 Available | Unknown |
| pre_eol_info=3，或 A/B >100，或 pre_eol_urgent / life_time_typ_a_exceeded / life_time_typ_b_exceeded | Critical |
| pre_eol_info=2，或 pre_eol_warning，或 A/B=100，或其余非空 flags | Warning |
| pre_eol_info=1，或 A/B 任一有效 Some，且没有上述告警 | Healthy |
| 所有健康字段未知 | Unknown |

A/B 的逻辑必须对称。Healthy 仅表示已获取的健康指标无告警，不表示未支持的字段也正常。显示层保留缺失字段的“不支持/未定义”说明。

### 4.3 寿命区间

- 0x00、保留值 → None。
- 0x01..0x0A → Some(10)..Some(100)，仍沿用区间上界数值模型。
- 0x0B → Some(101)，用于区分“超过”。
- `generate_warning_flags` 仅对 A/B >100 生成 exceeded，不对100生成 exceeded。
- 100 的用户文案统一 `90–100%`，等级 Warning；101 文案统一 `>100%`，等级 Critical。
- Web 进度条宽度最多100%，但文本与API值不能被截成100；其它已用寿命区间保留既有数值显示，本轮不扩大精度表达改造。

## 5. 采集层与 I/O 安全

### 5.1 NVMe ioctl

新增 `NvmeError::CommandStatus(i32)`、`NvmeError::IoCode(i32)`；保留 `Io(String)` 用于上下文错误。

```rust
pub(crate) fn check_admin_result(ret: i32, errno: i32) -> Result<(), NvmeError>;
```

- ret=0：唯一成功路径，随后才能解析缓冲区。
- ret<0：立即在 close 前保存 errno，返回 IoCode。
- ret>0：返回 CommandStatus，不能用 errno 替代 completion status。
- 文件使用 O_RDONLY | O_CLOEXEC，OwnedFd 或等价 RAII 保证所有返回路径关闭。
- 不自动重试 ioctl；每次用户刷新属于新的请求。

返回约定参考 [NVMe Admin passthrough 文档](https://manpages.debian.org/testing/libnvme-dev/nvme_admin_passthru.2.en.html#RETURN)。

### 5.2 MMC fallback

| 输入情况 | EXT_CSD reader 调用 | 读取状态 |
|---|---:|---|
| SD | 0 | Unsupported |
| MMC，有任意有效 life A/B 或有效 pre-EOL | 0 | Available |
| MMC，全健康字段缺失/无效，有主 block | 1 | 由读取结果决定 |
| MMC，全未知，无主 block | 0 | Unavailable，Io/code=null |
| fallback 成功，rev<7 | 已调用1次 | Unsupported，不解析健康字节 |
| fallback 成功，rev>=7，至少一有效健康字段 | 已调用1次 | Available，统一函数判级 |
| fallback 成功，rev>=7，全未知 | 已调用1次 | Available + Unknown |
| fallback 打开/命令失败 | 已尝试1次 | Unavailable + 错误分类 |

SDIO 仍从存储枚举中过滤。保留主 mmcblk 过滤规则，不对分区、boot、rpmb 调用 fallback。只保留当前 sysfs 优先策略，不因为部分字段缺失再发 ioctl。

新增 `MmcError::IoCode(i32)`，保存权限和 I/O 原因。成功返回但未定义的 pre-EOL 值必须规范化为0，寿命保留值为None。

### 5.3 测试 seam 与宿主隔离

```rust
pub(crate) fn read_controller_sysfs_with(
    root: &Path, name: &str,
    reader: &dyn Fn(&str) -> Result<[u8; 512], NvmeError>,
) -> Result<NvmeDevice, NvmeError>;

pub(crate) fn read_device_sysfs_with(
    root: &Path, name: &str,
    reader: &dyn Fn(&str) -> Result<[u8; 512], MmcError>,
) -> Result<MmcDevice, MmcError>;
```

生产 wrapper 只有 root 为 `/` 才传真实 ioctl reader。其它 root 默认拒绝设备读取，返回 `NotSupported("fixture requires an injected reader")`；这表示测试读取路径不可用，映射为 Unavailable/Io，而不是声称真实设备 Unsupported。测试使用 `_with` 显式传fake：即使 fake sysfs 内写着 mmcblk0，也不能访问宿主 `/dev/mmcblk0`。禁止因当前机器没有该设备而把测试当作安全。

## 6. 新鲜采集与 Controller

### 6.1 Manager 生命周期

Manager 不保存权威 status，只保存 root。构造函数不做存储 I/O。保留 `new`、`probe_and_init`、`sysfs_root` 名称，更新 `probe_and_init` 文档明确只配置路径。

- `NvmeManager::status(&self) -> Result<NvmeStatus, NvmeError>`。
- `MmcManager::status(&self) -> Result<MmcStatus, MmcError>`。
- NVMe 增加 crate 内部 `status_with(&self, reader: &dyn Fn(&str) -> Result<[u8; 512], NvmeError>) -> Result<NvmeStatus, NvmeError>`；`status` 按 root 选择安全 reader 后委托此方法。它只作为状态采集的内部 seam，不保存 reader 或 status，允许测试在同一个 Manager 上控制两次 SMART 读数。MMC 的 sysfs 变化测试直接调用其 status。
- 每次 status 重新枚举，再读取每个设备；不存在的目录或空目录是没有设备。
- 新增 `try_probe_sysfs(root) -> Result<Vec<String>, 对应Error>`，EACCES、ENOTDIR 等枚举错误不能变成成功的空列表。
- 保留原 `probe_sysfs(root)->Vec<String>` 兼容 capability 调用，但状态采集不得走此吞错 wrapper。本轮不重设计 capability 的错误模型。
- 若保留 `is_initialized`，必须查询本次状态；便利布尔不能代替API的错误信息。
- 枚举后单盘健康/属性读取出错或设备消失时，保留枚举标识和可读取的metadata，并标 Unavailable；不阻断其它盘。整体目录枚举失败返回 Err。

### 6.2 可替换读取源

新建 `storage.rs`：

```rust
pub trait StorageReader: Send + Sync {
    fn nvme_status(&self) -> Result<NvmeStatus, HardwareError>;
    fn mmc_status(&self) -> Result<MmcStatus, HardwareError>;
}

pub struct SystemStorageReader {
    nvme: NvmeManager,
    mmc: MmcManager,
}
```

`SystemStorageReader::new()` 只构造Manager，读取时映射枚举错误为 `HardwareError::Io(String)`。`Controller` 保存 `Arc<dyn StorageReader>`，保留原 `new(mode, policy)`；增加 `pub #[doc(hidden)] with_storage_reader(mode, policy, Arc<dyn StorageReader>)` 供 core/app 跨crate的确定性测试。

Controller 的 synthetic 分支必须先返回Demo，不能调用 reader。Demo NVMe/eMMC 为 Available+Healthy，SD为Unsupported+Unknown。`storage_status` 顺序聚合两种读取，不做跨设备原子快照承诺；单设备错误体现在设备状态，子系统枚举失败沿既有 HardwareError 失败返回。

REST 保留 `blocking` 执行，不在 async worker 上直接 ioctl。整体错误沿 `ApiError::from_hardware` 返回；不更改 loopback 安全边界。

## 7. CLI / TUI 呈现

- `hardware nvme/mmc/storage --json` 输出新契约；普通输出仍保留型号、路径、容量等静态信息。
- 不可用：显示“不可读取 / Unavailable”，权限失败解释权限不足，协议错误显示状态码；不显示伪造的0°C、零磨损或“告警：无”。
- 不支持：显示“不支持健康指标 / Health telemetry unsupported”；未知字段显示未定义，不作为正常证据。
- TUI 使用 `health_state` 统一等级；Unknown为中性色，Warning为黄色，Critical为红色。
- TUI保留用户按 `r` 的刷新方式，不新增自动轮询。本轮承诺同一个App刷新后使用新数据，而非重新启动TUI才能更新。
- 所有新增文案中英成对；不把读取错误转换成“未检测到设备”。

## 8. Web 刷新与竞态

### 8.1 入口与状态

新增 `async function refreshStorageTool()`。新增状态字段：

| 字段 | 初值 | 意义 |
|---|---|---|
| storageRefreshPromise | null | 当前代次正在进行的工作 |
| storageRefreshing | false | 刷新按钮/加载状态 |
| storageRefreshError | null | 原始错误对象，渲染时翻译 |
| storageStale | false | 保留的上次响应是否已因失败过期 |
| storageRefreshedAt | null | 客户端收到成功响应的时间 |

- 首次打开storage、局部刷新按钮、全局手动刷新和原有10秒轮询都调用同一函数。
- 在 `refreshAll` 入口独立触发 `void refreshStorageTool()`，不 await、不加入快照 Promise.all，避免设备读取阻塞全局操作完成。
- 沿用已有10秒timer，不新增另一个timer；只有选中storage抽屉才发存储请求。
- 同一代次多个触发复用正在进行的工作，不无限排队。一次成功后下一次可发新请求。
- 不给所有硬件抽屉增加轮询。

### 8.2 迟到结果与生命周期

每个请求绑定 `hardwareLoadVersion` 和请求promise身份。success、catch、finally均同时检查：

1. `selectedHardware === "storage"`；
2. version仍等于当前 `hardwareLoadVersion`；
3. promise仍是本代次的 `storageRefreshPromise`。

关闭、切换、重新打开使旧代次失效，清当前句柄，不等待旧请求。旧请求即使先完成，也不能在finally清除新请求的loading或promise。transport同步throw同样进入错误处理，不留下永久loading。不要求HTTP取消能力。

### 8.3 显示与错误

- 无数据、空列表、加载失败和成功有数据四种情况都保留刷新外壳；不能被 `renderHardwareTool` 的 `!hardwareData` early return挡住。
- 刷新中禁用按钮，保留已有内容；失败有旧数据时显示醒目的“数据已过期 / Stale data”。旧值可保留供参考，但所有当前健康徽章降级Unknown、中性显示，不保留绿色健康语义。
- 失败没有旧数据时显示错误及重试；重试成功清 stale/error，更新刷新时间。
- smart=null不渲染虚构指标和进度条。缺少新契约字段默认Unknown。
- UI仅消费后端healthState，不再用任意非空flags判断Critical。过期降级是客户端新鲜度防护，不覆盖后端原始数据。
- 生命周期测试必须覆盖close/reopen、switch、旧success、旧error、旧finally及同代去重。

## 9. 验收与测试边界

| 编号 | 自动化验收 | 方法 |
|---|---|---|
| 1 | 同一Manager/Controller/App/router重复查询可变化；Web不重开也更新 | 临时sysfs、可替换reader、deferred Promise |
| 2 | EACCES/EIO/不支持/全未知不显示Healthy和伪造数字 | 注入错误、JSON roundtrip、CLI/TUI/Web真实渲染 |
| 4 | 0唯一成功；负数保存errno；正数保留completion status | 纯判定测试+raw入口接线审查 |
| 5 | preEol2+真实pre_eol_warning为Warning | 真实parser/flags输出与跨层fixture |
| 6 | 0x0A不exceeded、0x0B才exceeded，A/B对称 | 边界表驱动+文本与进度条断言 |
| 7 | SD reader调用严格0次，MMC按条件0或1次 | panic reader/调用计数 |

自动化不依赖真实NVMe/SD、root权限或设备移除。权限失败用fake，不用可能被root绕过的chmod。异步测试控制Promise完成顺序，不用sleep。

可选HIL只在用户另行授权时执行：同一进程重复只读查询、非特权读取、Web刷新；热插拔仅限已安全卸载的非系统卡。禁止通过磨损、故障注入硬件或对SD发送错误原始命令制造测试条件。

若没有硬件，只报告未执行HIL。旧截图和之前的正常数据测试不能作为本轮修复完成证据。最终需有六项问题分别对应的RED/GREEN记录。

# 存储健康正确性修复 Implementation Plan（TDD）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复审查问题 **1/2/4/5/6/7**，让存储查询获得新数据、未知健康不被误报，并纠正 NVMe completion status 与 MMC 边界/卡类型判定。

**Architecture:** 以按需重新枚举和采集替换启动缓存，通过显式 telemetry 和可空 SMART 区分读取失败；核心统一计算健康等级，三端共同消费。Web 存储刷新独立于快照队列，以抽屉代次和请求身份防止迟到结果污染。

**Tech Stack:** Rust edition 2024、现有 libc/serde/thiserror/uuid、Axum/Tokio、Ratatui、原生 JavaScript、Node.js `node:test`/`vm`。

**Spec:** [存储健康正确性修复设计规范](../specs/2026-09-13-storage-health-correctness-design.md)

**状态：** 待实施；以下代码与测试是实施指导，不表示已落地或已通过。编写基线 `bf7b485`，日期 2026-09-13。

## Global Constraints

- 仅修复问题 1/2/4/5/6/7；问题 3（32 位 MMC ioctl 常量类型）不在本次范围。
- 不新增第三方依赖、后台采集服务、TTL 缓存、自动提权或设备写操作。
- NVMe `smart` 允许为 `null`；CLI/TUI/Web 与 JSON 契约同步迁移，不维持伪造的全零兼容数据。
- 每次状态请求重新枚举并读取当前内核暴露值；不宣称 eMMC sysfs 值一定是芯片实时值。
- MMC 保持 sysfs 优先；仅 MMC 缺少全部有效健康字段时执行 EXT_CSD fallback；SD 永不执行该 ioctl。
- 测试使用临时 sysfs 和显式注入 reader，不访问宿主 `/dev/nvme*` 或 `/dev/mmcblk*`。
- Demo 不调用真实存储 reader；禁止以“测试机器恰好没有设备”为隔离措施。
- 中英文文案齐备；未知、不可读取、不支持和过期状态不得显示绿色健康。
- 保留现有 loopback 服务边界与 blocking 调度，不把同步 ioctl 放进 Tokio async worker。
- 不修改 `Cargo.toml`、`Cargo.lock` 的依赖或版本要求；不顺带修复无关平台问题。

---

## 1. 文件划分与执行规则

所有路径相对仓库根目录。Rust app 的 package 名是 **`rsetup-next`**，不是 `rsetup-app`。

| 文件 | 操作 | 责任 |
|---|---|---|
| `crates/rsetup-core/src/model.rs` | 修改 | 新读取状态、错误、健康等级与可空 SMART |
| `crates/rsetup-core/src/health.rs` | 新建 | 等级判定纯函数及真值表测试 |
| `crates/rsetup-core/src/lib.rs` | 修改 | 新模型、判定函数、注入接口导出 |
| `crates/rsetup-core/src/nvme.rs`、`nvme/sys.rs` | 修改 | 新鲜采集、错误保留、ioctl 状态判断 |
| `crates/rsetup-core/src/mmc.rs`、`mmc/sys.rs` | 修改 | 寿命边界、MMC guard、sysfs/reader 隔离 |
| `crates/rsetup-core/src/storage.rs` | 新建 | StorageReader 与 SystemStorageReader |
| `crates/rsetup-core/src/actions.rs` | 修改 | Controller 注入、Demo、查询生命周期 |
| `crates/rsetup-app/src/main.rs`、`i18n.rs`、`tui.rs` | 修改 | 三种 CLI 命令与 TUI 状态/刷新 |
| `crates/rsetup-app/src/server.rs` | 测试为主 | JSON 与同一 router 重复查询 |
| `ui/app.js`、`i18n.js`、`styles.css` | 修改 | 正确呈现、刷新与过期语义 |
| `ui/storage.test.mjs`、`ui/i18n.test.mjs` | 修改 | 真实卡片、边界与双语回归 |
| `ui/storage-refresh.test.mjs` | 新建 | deferred Promise 竞态测试 |

### 顺序与提交边界

**T1 → T2 → T3 → T4 → T5 → T6 → T7**。

T1–T3 提供类型/纯逻辑/I/O seam；T4 一次性迁移生产者、消费者和 fixtures，保证可空 SMART 不留下无法编译的中间提交；T5 修复采集生命周期；T6 修复 Web 原位刷新；T7 闭环验证。

每个任务：先写测试 → 运行RED → 最小实现 → 同命令GREEN → 相关回归 → 提交。新增符号的第一次RED可以是缺少接口；建立接口后仍须验证旧错误行为会导致断言失败，不以编译错误充当所有行为证据。

实施时记录命令、失败断言、退出码。filter匹配0项不是通过。建议提交使用明确文件列表，不使用 `git add .`；本文的commit是未来实施建议，本次写文档不执行。

## T1：建立读取模型与健康判定纯函数

**Files:** 新建 `crates/rsetup-core/src/health.rs`；修改 `model.rs`、`lib.rs`。

**Consumes:** 现有 `NvmeSmartLog`、`MmcHealth`。

**Produces:** spec §3 的 `TelemetryReadState`、`HealthState`、`TelemetryErrorKind`、`TelemetryError`、`TelemetryStatus`；以下函数从 crate 根导出：

```rust
pub fn nvme_health_state(telemetry: &TelemetryStatus, smart: Option<&NvmeSmartLog>) -> HealthState;
pub fn mmc_health_state(telemetry: &TelemetryStatus, health: &MmcHealth) -> HealthState;
```

- [ ] **RED：在新模块登记后编写真实预警组合测试。**

```rust
#[test]
fn mmc_health_state_keeps_real_pre_eol_warning() {
    let ready = TelemetryStatus { state: TelemetryReadState::Available, error: None };
    let h = MmcHealth {
        pre_eol_info: 2,
        life_time_est_a_percent: Some(10),
        life_time_est_b_percent: Some(10),
        warning_flags: crate::mmc::generate_warning_flags(2, Some(10), Some(10)),
    };
    assert_eq!(mmc_health_state(&ready, &h), HealthState::Warning);
    assert_eq!(mmc_health_state(&ready, &MmcHealth { pre_eol_info: 3, ..h }), HealthState::Critical);
}

#[test]
fn missing_data_is_not_healthy() {
    let absent = TelemetryStatus::default();
    assert_eq!(nvme_health_state(&absent, None), HealthState::Unknown);
    assert_eq!(mmc_health_state(&absent, &MmcHealth::default()), HealthState::Unknown);
    let ready = TelemetryStatus { state: TelemetryReadState::Available, error: None };
    assert_eq!(nvme_health_state(&ready, None), HealthState::Unknown);
    assert_eq!(mmc_health_state(&ready, &MmcHealth::default()), HealthState::Unknown);
}
```

- [ ] **运行：** `cargo test -p rsetup-core health --locked`。建立函数签名后，复制旧的“非空flag即critical / 空即healthy”判断应不能通过。
- [ ] **GREEN：按spec类型定义实现模型，暂不改变Device字段。** 枚举snake_case，结构体camelCase；默认读取Unavailable、健康Unknown；TelemetryStatus默认error=None。
- [ ] **实现判定函数，顺序不得颠倒。**

```rust
// MMC判定中的关键量；先处理读取不可用，再处理危急，再处理预警。
if telemetry.state != TelemetryReadState::Available { return HealthState::Unknown; }
let exceeded = [health.life_time_est_a_percent, health.life_time_est_b_percent]
    .into_iter().flatten().any(|p| p > 100);
let at_limit = [health.life_time_est_a_percent, health.life_time_est_b_percent]
    .into_iter().flatten().any(|p| p == 100);
let urgent_flag = health.warning_flags.iter().any(|f| matches!(f.as_str(),
    "pre_eol_urgent" | "life_time_typ_a_exceeded" | "life_time_typ_b_exceeded"));
if health.pre_eol_info == 3 || exceeded || urgent_flag { return HealthState::Critical; }
if health.pre_eol_info == 2 || at_limit || !health.warning_flags.is_empty() {
    return HealthState::Warning;
}
if health.pre_eol_info == 1 || health.life_time_est_a_percent.is_some()
    || health.life_time_est_b_percent.is_some() { return HealthState::Healthy; }
HealthState::Unknown
```

NVMe必须先检查Available和Some，再检查critical_warning非零、其余flags，最后Healthy。

- [ ] **表驱动补齐：** NVMe None、不可用但有残留Some、非零critical位、仅未知flag、正常；MMC Unsupported、Unavailable、全未知、仅A有效、仅pre-EOL正常、A/B各100与101、未知flag。A/B对称。
- [ ] **序列化断言：** 默认TelemetryStatus为 `{"state":"unavailable","error":null}`，NvmeStatus错误kind为 `nvme_status`，默认HealthState为 `unknown`。
- [ ] **验证并提交：** `cargo test -p rsetup-core --locked`；建议 `feat(core): define explicit storage telemetry and health states`。

## T2：NVMe completion 错误与可注入读取

**Files:** `crates/rsetup-core/src/nvme.rs`、`nvme/sys.rs`。

**Consumes:** SMART parser、T1错误分类。

**Produces:** `NvmeError::CommandStatus(i32)`、`NvmeError::IoCode(i32)`，保留现有Io(String)；接口：

```rust
pub(crate) fn check_admin_result(ret: i32, errno: i32) -> Result<(), NvmeError>;
pub(crate) fn read_controller_sysfs_with(
    root: &std::path::Path, name: &str,
    reader: &dyn Fn(&str) -> Result<[u8; 512], NvmeError>,
) -> Result<NvmeDevice, NvmeError>;
```

- [ ] **RED：写返回值测试。**

```rust
#[test]
fn nvme_admin_result_requires_zero() {
    assert_eq!(check_admin_result(0, libc::EIO), Ok(()));
    assert_eq!(check_admin_result(2, libc::EACCES), Err(NvmeError::CommandStatus(2)));
    assert_eq!(check_admin_result(0x4002, 0), Err(NvmeError::CommandStatus(0x4002)));
    assert_eq!(check_admin_result(-1, libc::EIO), Err(NvmeError::IoCode(libc::EIO)));
}
```

- [ ] **运行：** `cargo test -p rsetup-core nvme_admin_result --locked`，确认正数状态不能成功。
- [ ] **GREEN：实现并接入真实raw函数。**

```rust
pub(crate) fn check_admin_result(ret: i32, errno: i32) -> Result<(), NvmeError> {
    match ret {
        0 => Ok(()),
        n if n > 0 => Err(NvmeError::CommandStatus(n)),
        _ => Err(NvmeError::IoCode(errno)),
    }
}
```

ioctl后仅ret<0时立刻保存errno，在关闭fd前完成捕获；调用判定后才返回buf。使用OwnedFd或等价RAII，open改为O_RDONLY | O_CLOEXEC，所有return均释放fd；不自动重试。

- [ ] **加入reader seam与隔离。** 原 `read_controller_sysfs` 委托 `_with`；root为 `/` 才传真reader，其余root默认reader返回 `NotSupported("fixture requires an injected reader".into())`。本任务暂保留旧Device结构；T4删除默认SMART占位。
- [ ] **测试真实解析路径：** 临时 `sys/class/nvme/nvme0` 建metadata与namespace size，注入固定512字节和调用计数。fake把 `buf[1..3]` 写为 `310u16.to_le_bytes()`，断言温度约36.85°C且调用一次。所有测试读的是fake，不访问宿主/dev。
- [ ] **验证并提交：** `cargo test -p rsetup-core nvme --locked`；建议 `fix(core): reject nonzero NVMe completion status`。

## T3：MMC 寿命边界、卡类型 guard 与 fallback

**Files:** `crates/rsetup-core/src/mmc.rs`、`mmc/sys.rs`。

**Consumes:** EXT_CSD parser、T1分级函数。

**Produces:** `MmcError::IoCode(i32)`，保留Io(String)；接口：

```rust
pub(crate) fn read_device_sysfs_with(
    root: &std::path::Path, name: &str,
    reader: &dyn Fn(&str) -> Result<[u8; 512], MmcError>,
) -> Result<MmcDevice, MmcError>;
```

- [ ] **RED：连接真实原始字节 → flags → 等级。**

```rust
#[test]
fn mmc_life_0a_is_not_exceeded_but_0b_is() {
    let ready = TelemetryStatus { state: TelemetryReadState::Available, error: None };
    for (raw, expected) in [(0x0A, HealthState::Warning), (0x0B, HealthState::Critical)] {
        let a = map_life_time_byte_to_percent(raw);
        let flags = generate_warning_flags(1, a, None);
        assert_eq!(flags.iter().any(|f| f == "life_time_typ_a_exceeded"), raw == 0x0B);
        let h = MmcHealth { pre_eol_info: 1, life_time_est_a_percent: a,
            life_time_est_b_percent: None, warning_flags: flags };
        assert_eq!(crate::mmc_health_state(&ready, &h), expected);
    }
}
```

- [ ] **运行：** `cargo test -p rsetup-core mmc_life_0a --locked`，应在0x0A的exceeded断言失败。
- [ ] **GREEN：** A/B生成exceeded的条件都从 `>=100` 改为 `>100`。修改旧测试中Some(100)也产生exceeded的错误期望，保留覆盖。pre_eol_warning不改名。
- [ ] **写临时卡fixture与SD禁调用测试。**

```rust
fn card_fixture(card_type: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("rsetup-storage-{}", uuid::Uuid::new_v4()));
    let card = root.join("sys/bus/mmc/devices/mmc0:0001");
    std::fs::create_dir_all(card.join("block/mmcblk0")).unwrap();
    std::fs::write(card.join("type"), card_type).unwrap();
    std::fs::write(card.join("name"), "FIXTURE").unwrap();
    let block = root.join("sys/class/block/mmcblk0");
    std::fs::create_dir_all(&block).unwrap();
    std::fs::write(block.join("size"), "4096").unwrap();
    root
}

#[test]
fn sd_never_calls_ext_csd_reader() {
    let root = card_fixture("SD");
    let forbidden = |_: &str| -> Result<[u8; 512], MmcError> {
        panic!("SD must not issue eMMC CMD8");
    };
    let device = read_device_sysfs_with(&root, "mmc0:0001", &forbidden).unwrap();
    assert_eq!(device.card_type, "SD");
    std::fs::remove_dir_all(root).unwrap();
}
```

- [ ] **运行第二组RED：** `cargo test -p rsetup-core sd_never_calls --locked`。seam接好而guard未加时必须panic，不能让新函数成为不调用reader的空壳。
- [ ] **GREEN：** 在fallback前判断card_type为MMC、全部健康字段未知、主block存在。default wrapper沿用NVMe的root隔离。open为只读+O_CLOEXEC，错误保留errno，fd全路径关闭。
- [ ] **补充次数用例：** MMC全缺失/非法→1次；仅life A、仅pre-EOL有效→0次；SD→0次；SDIO过滤；无block→0次；分区/boot/rpmb不作为主block。fake设置 `buf[192]=7; buf[267]=2; buf[268]=0x0A; buf[269]=0x0B` 验证fallback实际解析。
- [ ] **版本与保留值：** rev<7不解释健康字节；rev>=7才解释；pre-EOL非1/2/3规范化0，寿命保留值为None。T4追加Unsupported/Unavailable状态断言。
- [ ] **验证并提交：** `cargo test -p rsetup-core mmc --locked`；建议 `fix(core): correct MMC endurance boundaries and restrict EXT_CSD reads`。不编辑问题3对应常量。

## T4：可空 SMART 与三端状态迁移

**Files:** core `model.rs`、`lib.rs`、`actions.rs`、`nvme.rs`、`nvme/sys.rs`、`mmc/sys.rs`；app `main.rs`、`tui.rs`、`i18n.rs`、`server.rs`；`ui/app.js`、`ui/i18n.js`、`ui/styles.css`、`ui/storage.test.mjs`、`ui/i18n.test.mjs`。

**Consumes:** T1模型/分级、T2/T3读取seam。

**Produces:**

```rust
// NvmeDevice替换/追加：
pub smart: Option<NvmeSmartLog>,
#[serde(default)]
pub telemetry: TelemetryStatus,
#[serde(default)]
pub health_state: HealthState,
// MmcDevice保留health对象，追加相同telemetry/health_state。
```

- [ ] **RED：新增 `storage_contract_` 前缀测试。** 临时NVMe controller fixture分别注入IoCode(EACCES)、IoCode(EIO)、CommandStatus(2)。断言metadata保留、smart=None、telemetry=Unavailable、health_state=Unknown，且原code保留。
- [ ] **JSON roundtrip精确断言。**

```rust
let device: NvmeDevice = serde_json::from_value(serde_json::json!({
    "name":"nvme0", "path":"/dev/nvme0", "model":"FIXTURE", "serial":"test-only",
    "firmware":"1", "totalBytes":4096, "smart":null,
    "telemetry":{"state":"unavailable","error":{"kind":"permission_denied","code":13}},
    "healthState":"unknown"
})).unwrap();
assert!(device.smart.is_none());
assert_eq!(device.health_state, HealthState::Unknown);
let value = serde_json::to_value(&device).unwrap();
assert!(value["smart"].is_null());
assert_eq!(value["telemetry"]["error"]["code"], 13);
assert_eq!(value["healthState"], "unknown");
```

- [ ] **运行：** `cargo test -p rsetup-core storage_contract --locked`。新字段缺失可以是第一次RED；接好模型后还须证明旧unwrap_or_default语义不能通过行为断言。
- [ ] **GREEN：显式构造数据与状态。**

```rust
let (smart, telemetry) = match reader(&dev_path).and_then(|buf| parse_smart_log(&buf)) {
    Ok(log) => (Some(log), TelemetryStatus { state: TelemetryReadState::Available, error: None }),
    Err(error) => (None, telemetry_from_nvme_error(&error)),
};
let health_state = crate::nvme_health_state(&telemetry, smart.as_ref());
```

在nvme/sys.rs定义私有 `telemetry_from_nvme_error(&NvmeError)->TelemetryStatus`：IoCode(EACCES/EPERM)→PermissionDenied；其它IoCode→Io；CommandStatus→NvmeStatus；带上下文Io/fixture reader拒绝→Io/code=None。状态均Unavailable。MMC私有 `telemetry_from_mmc_error(&MmcError)->TelemetryStatus`同理但无NvmeStatus。

- [ ] **MMC矩阵：** SD→Unsupported/Unknown；有任一有效sysfs字段→Available；全未知fallback失败→Unavailable+错误；无block→Unavailable/Io/code=None；成功rev<7→Unsupported；成功rev>=7全未知→Available/Unknown。有数据仍由统一函数判级。
- [ ] **机械迁移所有字面量与消费者，保持同一提交可编译。** 搜索 `NvmeDevice {`、`MmcDevice {`、`.smart`；Demo NVMe用Some+Available+Healthy，eMMC用Available+Healthy，SD用Unsupported+Unknown。metadata读取中途失败不得重新造Default SMART。
- [ ] **CLI/TUI测试先行：** 在Demo结构上修改smart=None、telemetry=Unavailable、health_state=Unknown，检查格式化和Ratatui TestBackend结果保留型号、含“不可读取”/“Unavailable”，不含伪造0°C或Healthy。补SD、不完整MMC、真实预警、100、101中英文用例。
- [ ] **CLI/TUI实现：** NVMe用Option分支渲染，失败显示原因且不显示数字；按后端health_state显示等级，读取不可用/null优先Unknown。MMC100文字 `90–100%`，101文字 `>100%`；缺失字段显示未定义/不支持。TUI仍用手动r入口，数据源刷新在T5完成。
- [ ] **Web RED：加载真实NVMe卡片函数，不再以stub替代被测实现。** 在现有vm上下文中删除renderNvmeDeviceCard stub、加载 `handler("renderNvmeDeviceCard")`，补齐依赖。使用下面完整状态，禁止为了黄色徽章删除真实flags：

```javascript
const missingNvme = { ...nvmeDevice, smart: null,
  telemetry: { state: "unavailable", error: { kind: "permission_denied", code: 13 } },
  healthState: "unknown" };
const sd = { ...sdDevice, telemetry: { state: "unsupported", error: null }, healthState: "unknown" };
const warning = { ...emmcDevice,
  telemetry: { state: "available", error: null }, healthState: "warning",
  health: { ...emmcDevice.health, preEolInfo: 2, warningFlags: ["pre_eol_warning"] } };
const { context } = createTestContext();
assert.doesNotMatch(context.renderNvmeDeviceCard(missingNvme), /nvme-badge-healthy/);
assert.doesNotMatch(context.renderNvmeDeviceCard(missingNvme), /0\.0 °C/);
assert.doesNotMatch(context.renderStorageMmcCard(sd), /nvme-badge-healthy/);
assert.match(context.renderStorageMmcCard(warning), /nvme-badge-warning/);
assert.doesNotMatch(context.renderStorageMmcCard(warning), /nvme-badge-critical/);
```

- [ ] **运行Web RED：** `node --test ui/storage.test.mjs ui/i18n.test.mjs`。
- [ ] **Web GREEN：** 消费healthState，缺新字段默认Unknown；null smart不渲染虚构数字或条形图；读取不可用优先Unknown；新增中性badge和中英文unknown/unavailable/unsupported与权限/协议错误文案。MMC文本100为90–100%，101为>100%，只有bar宽度截到100；去除percent>=100一律critical的判断。
- [ ] **兼容回归：** 缺少新字段的旧JSON为Unknown；真实成功SMART中的零计数仍可显示；locale切换后错误文案更新；字典键/占位符对称。
- [ ] **验证并提交：** `cargo test --workspace --locked`、`node --test ui/*.test.mjs`；建议 `fix(storage): represent unavailable telemetry and unify health rendering`。

## T5：按需采集、注入源与同一实例刷新

**Files:** 新建 `crates/rsetup-core/src/storage.rs`；修改 `lib.rs`、`actions.rs`、`nvme.rs`、`mmc.rs`；app `tui.rs`、`server.rs`测试。

**Consumes:** T4的新设备契约。

**Produces:**

```rust
pub trait StorageReader: Send + Sync {
    fn nvme_status(&self) -> Result<NvmeStatus, HardwareError>;
    fn mmc_status(&self) -> Result<MmcStatus, HardwareError>;
}
pub struct SystemStorageReader { nvme: NvmeManager, mmc: MmcManager }
// SystemStorageReader::new只配置root；从crate根导出StorageReader供app测试使用。

// Controller：public + #[doc(hidden)]，用于跨crate确定性测试。
pub fn with_storage_reader(mode: ProbeMode, policy: ExecutionPolicy,
    reader: std::sync::Arc<dyn StorageReader>) -> Self;

// NvmeManager与MmcManager分别升级：
pub fn status(&self) -> Result<NvmeStatus, NvmeError>;
pub fn status(&self) -> Result<MmcStatus, MmcError>;
// 各模块自己的try_probe_sysfs(root: &Path)返回Result<Vec<String>,对应Error>。
```

- [ ] **RED：同一Manager读取变化。** 在已有mmc.rs多设备fixture里创建初始化life_time为0x01的卡；新测试名 `storage_freshness_mmc_rereads_attributes`。

```rust
let manager = MmcManager::probe_and_init(Some(&root));
let before = manager.status().unwrap();
std::fs::write(root.join("sys/bus/mmc/devices/mmc0:0001/life_time"), "0x0B 0x01\n").unwrap();
let after = manager.status().unwrap();
assert_eq!(before.devices[0].health.life_time_est_a_percent, Some(10));
assert_eq!(after.devices[0].health.life_time_est_a_percent, Some(101));
assert_eq!(after.devices[0].health_state, HealthState::Critical);
```

先为Result签名机械适配但不修缓存；运行 `cargo test -p rsetup-core storage_freshness --locked`，after断言必须失败。

- [ ] **GREEN：移除权威status字段，仅保存root。** 构造不采集，status每次 `try_probe_sysfs`→逐设备读取。保留probe_and_init名字但更新文档；is_initialized如保留也查询本次状态。API不能用布尔值吞掉错误。
- [ ] **NVMe 同一Manager也必须有RED/GREEN，不只测Controller fake。** 增加 crate 内部 `NvmeManager::status_with(&self, reader: &dyn Fn(&str) -> Result<[u8; 512], NvmeError>) -> Result<NvmeStatus, NvmeError>`，status按root选安全reader后委托；用T2的临时NVMe目录执行以下测试，确认每次状态查询都到达真实采集流程：

```rust
let manager = NvmeManager::probe_and_init(Some(&root));
let calls = std::cell::Cell::new(0u32);
let reader = |_: &str| -> Result<[u8; 512], NvmeError> {
    calls.set(calls.get() + 1);
    let mut buf = [0u8; 512];
    let kelvin: u16 = if calls.get() == 1 { 310 } else { 320 };
    buf[1..3].copy_from_slice(&kelvin.to_le_bytes());
    Ok(buf)
};
let first = manager.status_with(&reader).unwrap();
let second = manager.status_with(&reader).unwrap();
assert_eq!(calls.get(), 2);
assert_ne!(first.devices[0].smart, second.devices[0].smart);
```

先保留缓存逻辑运行 `cargo test -p rsetup-core storage_freshness --locked` 得到失败，再实现每次枚举并调用 `_with` 的最小修复。用例命名为 `storage_freshness_nvme_rereads_smart`；构造函数不调用reader，第一次读数应来自第一次status_with。

- [ ] **枚举边界：** 不存在/空目录→成功无设备；用普通文件占据sysfs设备目录触发ENOTDIR→Err；权限错误用fake，不用chmod。保留probe_sysfs的Vec包装供capability兼容，真正状态读取只能走try版本。
- [ ] **列表/局部故障：** 同一Manager在fixture新增/删除卡后返回新列表；单盘健康失败保留该盘metadata+Unknown，其它盘继续返回；枚举后设备消失也保留已枚举标识为不可用，下一次枚举移除。
- [ ] **Controller RED：使用可变fake，不以构造第二个Controller代替刷新。**

```rust
#[derive(Clone)]
struct FakeStorage(std::sync::Arc<std::sync::Mutex<StorageStatus>>);
impl StorageReader for FakeStorage {
    fn nvme_status(&self) -> Result<NvmeStatus, HardwareError> {
        Ok(self.0.lock().unwrap().nvme.clone())
    }
    fn mmc_status(&self) -> Result<MmcStatus, HardwareError> {
        Ok(self.0.lock().unwrap().mmc.clone())
    }
}
let seed = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun).storage_status().unwrap();
let shared = std::sync::Arc::new(std::sync::Mutex::new(seed));
let c = Controller::with_storage_reader(ProbeMode::Live, ExecutionPolicy::DryRun,
    std::sync::Arc::new(FakeStorage(shared.clone())));
let before = c.storage_status().unwrap();
shared.lock().unwrap().nvme.devices[0].smart.as_mut().unwrap().temperature_c = 71.0;
{
    let mut current = shared.lock().unwrap();
    current.mmc.devices.clear();
    current.mmc.initialized = false;
}
let after = c.storage_status().unwrap();
assert_ne!(before.nvme.devices[0].smart, after.nvme.devices[0].smart);
assert!(after.mmc.devices.is_empty());
```

- [ ] **Demo隔离：** 注入所有方法panic的reader，在Demo下构造并调用三种状态方法都不得panic；禁止先建立会采集真实硬件的Manager再覆盖。
- [ ] **GREEN：** Controller::new委托with_storage_reader；SystemStorageReader::new只配置路径；Controller保存Arc<dyn StorageReader>，synthetic先分支，Live每次读source；Manager枚举Err映射HardwareError::Io；不得新增unwrap_or_default。storage_status顺序聚合，两类设备不是原子快照。
- [ ] **TUI：** 同一个App使用Live+fake，修改shared后调用app.refresh，断言温度、设备列表及渲染值更新。相关非存储快照仍可能使用当前Linux主机，测试明确Linux环境，禁止用它验证存储I/O。
- [ ] **REST：** 同一个router clone后两次oneshot合法loopback host请求，中途更新fake；解析response JSON证明更新、null及Unknown。整体reader Err沿现有ApiError返回，不能200空列表；单盘健康错误不应让整接口失败。
- [ ] **验证并提交：** `cargo test --workspace --locked`；建议 `fix(core): recollect storage telemetry on each status request`。

## T6：Web 原位刷新与异步隔离

**Files:** `ui/app.js`、`ui/i18n.js`、`ui/styles.css`、`ui/storage.test.mjs`；新建 `ui/storage-refresh.test.mjs`。

**Consumes:** T4/T5 JSON，现有hardwareLoadVersion、selectedHardware、transport、10秒timer。

**Produces:** `async function refreshStorageTool()`；state新增 `storageRefreshPromise=null`、`storageRefreshing=false`、`storageRefreshError=null`、`storageStale=false`、`storageRefreshedAt=null`。最后一个是客户端收到响应的时间。

- [ ] **RED：新测试文件沿用现有handler/vm方法加载真实函数，使用deferred而不是sleep。**

```javascript
function deferred() {
  let resolve, reject;
  const promise = new Promise((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

const pending = deferred();
let calls = 0;
context.transport = { storageStatus: () => { calls += 1; return pending.promise; } };
context.state.selectedHardware = "storage";
context.state.hardwareLoadVersion = 1;
const a = context.refreshStorageTool();
const b = context.refreshStorageTool();
await Promise.resolve(); // 让实际transport微任务开始，不等待真实时间。
assert.equal(calls, 1);
pending.resolve({ nvme: { initialized: false, devices: [] }, mmc: { initialized: false, devices: [] } });
await Promise.all([a, b]);
assert.equal(context.state.storageRefreshing, false);
assert.equal(context.state.storageRefreshPromise, null);
```

`context`由vm.createContext建立，注入state、transport、renderHardwareTool计数stub与Date；handler复用现有函数提取逻辑。不要比较async包装后的 `a===b`，验证底层调用次数与两个调用者都等待工作。

- [ ] **运行：** `node --test ui/storage-refresh.test.mjs`，确认入口和去重缺失。
- [ ] **GREEN：以如下生命周期实现，全部结果路径加generation与promise身份保护。**

```javascript
async function refreshStorageTool() {
  if (state.selectedHardware !== "storage") return;
  if (state.storageRefreshPromise) return state.storageRefreshPromise;
  const version = state.hardwareLoadVersion;
  state.storageRefreshing = true;
  let task;
  const current = () => state.selectedHardware === "storage"
    && state.hardwareLoadVersion === version && state.storageRefreshPromise === task;
  task = Promise.resolve().then(() => transport.storageStatus()).then((data) => {
    if (!current()) return;
    state.hardwareData = data;
    state.storageRefreshError = null;
    state.storageStale = false;
    state.storageRefreshedAt = Date.now();
  }).catch((error) => {
    if (!current()) return;
    state.storageRefreshError = error;
    state.storageStale = state.hardwareData !== null;
  }).finally(() => {
    if (!current()) return;
    state.storageRefreshing = false;
    state.storageRefreshPromise = null;
    renderHardwareTool();
  });
  state.storageRefreshPromise = task;
  renderHardwareTool();
  return task;
}
```

通过Promise微任务延后transport调用，保证同步throw也不会抢在共享句柄赋值前发生。`renderHardwareTool`要允许storage在hardwareData=null时显示加载/失败外壳，不被旧early return拦截。

- [ ] **首次打开接线：** openHardwareTool("storage")建新version后调用统一入口并提前return；删掉loaders里会双读的storage分支。关闭/切换/重开清当前storage句柄与状态，不等旧HTTP完成。
- [ ] **全局刷新接线：** refreshAll入口 `void refreshStorageTool()`；不加入全局Promise.all或await。现有10秒interval和手动刷新已经调用refreshAll，因此不加第二个timer。非storage返回不请求。
- [ ] **外壳与状态：** 空列表/首次失败/成功数据均显示 `[data-storage-refresh]`；加载中禁用；显示客户端刷新时间；失败有旧数据标stale并把当前健康徽章都降Unknown/中性，保留值也标“过期”；无旧数据显示错误/重试；下次成功清stale/error。错误对象保存原值，locale切换时翻译。
- [ ] **按控制的resolve/reject顺序添加以下用例。**

| 用例 | 精确断言 |
|---|---|
| 同代两触发 | transport1次，两个调用者都等完成；完成后可再读 |
| close后success/error | 不回写，不渲染已关闭抽屉 |
| storage切thermal | 旧storage结果不覆盖thermal |
| close/reopen，新结果先到 | 旧success/error不覆盖新数据 |
| 旧finally先到，新仍pending | 新promise与loading保留 |
| 首次失败重试 | 错误可见，下次成功清错误 |
| 有旧数据时失败 | stale可见，NVMe/MMC无绿色健康 |
| retry恢复 | 新数据、新时间、新等级，stale=false |
| 空列表 | 刷新按钮仍存在 |
| 捕获10秒callback手动调用 | transport重复发起，不重开抽屉即可更新 |
| storage永不resolve，快照resolve | refreshAll仍完成，不阻塞操作 |
| transport同步throw | 清本次句柄，允许重试 |

- [ ] **真实渲染回归：** stale时两类卡片均无healthy类；切换语言重译错误；focus保护与CSSOM条宽写入保持有效。
- [ ] **验证并提交：** `node --test ui/*.test.mjs`；建议 `fix(web): refresh storage telemetry with generation-safe requests`。

## T7：跨层回归与最终验收

**Files:** 强化T1–T6各模块测试；仅对遗漏行为修对应生产文件。

**Consumes/Produces:** 消费现有新接口，交付六项问题逐项行为证据，不新增功能接口。

- [ ] **补充未闭环的RED：** 后端真实generate_warning_flags生成预警/0x0A/0x0B，序列化后检查healthState与三端标签；UI fixture与后端真值表一致，不人为删除flag。必要时修正对应生产逻辑后再GREEN。
- [ ] **执行全部验证。**

```bash
cargo test --workspace --locked
node --test ui/*.test.mjs
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
git diff --check
```

每条命令检查退出码。新增测试数不预先硬编码；审查时162项Rust通过、2忽略、51项JS通过仅是历史基线，不是这轮修复证据。若遇到基线fmt/clippy问题，先核对diff关系并记录，不扩大范围。

- [ ] **填写验收映射。**

| 问题 | 必须保留的证据 | 任务 |
|---|---|---|
| 1 | 同一Manager变化、同一Controller/App/router重复查询、Web刷新与竞态 | T5/T6 |
| 2 | EACCES/EIO/SD/未知MMC/null SMART；三端无伪造Healthy或0指标 | T1/T4 |
| 4 | ret0/-1/正数；raw入口确实调用判定；code不混用errno | T2/T4 |
| 5 | 真实pre_eol_warning在三端保持Warning | T1/T4/T7 |
| 6 | 0x0A/0x0B、A/B对称、API100/101、区间/>100文字、bar限宽 | T1/T3/T4 |
| 7 | SD reader0次、MMC条件0/1次、fixture不触宿主设备 | T3/T4 |

- [ ] **可选HIL，只在另行授权且具备硬件时执行。** 同一服务进程重复只读NVMe查询，观察温度/计数但不强求短时间值变化；Web不重开抽屉刷新；无权限账户确认smart=null；SD仅确认正常应用不调用EXT_CSD，不发送原始错误命令。热插拔仅限授权且安全卸载的非系统卡。故障与耗尽通过fake模拟，禁止真实磨损测试。
- [ ] **记录边界。** eMMC sysfs可能由内核缓存，相同读数不等于应用没刷新；fixture变化才是确定性重读证据。无硬件则明确未执行HIL，旧截图不算本轮验证。
- [ ] **最终代码审查和提交。** 查无错误上的默认SMART、任意flags即critical、>=100即exceeded；检查异步旧finally不清新请求。只提交剩余回归测试和必要修复，建议 `test(storage): cover telemetry failure and refresh regressions`。

## 2. 交付标准

实施报告须逐项给出测试名、RED失败原因、GREEN命令与退出码，并列出跳过测试/未运行HIL。所有六项都有行为证据后才可声明完成，不以既有测试全绿替代新增回归。

本计划不要求现在执行修复，不自动提交文档，不把问题3混入本轮。执行前确认spec；字段契约、读取策略或刷新生命周期若变动，先同步修订spec和本计划。

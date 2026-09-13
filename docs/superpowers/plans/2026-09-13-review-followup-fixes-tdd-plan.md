# 近期提交复审问题修复 TDD 实施计划

> **致执行代理：** 使用 `executing-plans` 逐项落实；用户选择子代理驱动时使用 `subagent-driven-development`。使用下列复选框记录实际进展，每项先 RED、再最小实现、再 GREEN 和审查。

**Goal:** 修复本轮复审 R1/R4/R5/R6/R7，恢复四架构 MMC 编译、桌面存储接线、全局刷新活性及存储错误的真实展示。

**Architecture:** MMC 按 `target_arch` 选择请求常量；Tauri 复用 Controller 的阻塞工作线程。Web 将普通刷新合并与变更后的失效刷新分开；三端使用现有 telemetry 契约，TUI 另存子系统枚举错误。

**Tech Stack:** Rust 1.85.0+、libc、Tauri 2、Ratatui、Node.js 内置 test/vm、GitHub Actions。

**Spec:** [近期提交复审问题修复规范](../specs/2026-09-13-review-followup-fixes-design.md)。编号以该规范为准，不沿用旧存储健康文档中的编号。

**Baseline:** `473c78c8501e05f42eb779ad7891e1035c698341`。本文是待执行计划，代码块中的新增测试与函数均未落入生产源码。

## Global Constraints

- Rust MSRV 保持 **1.85.0**，不新增生产依赖。
- 明确验证目标：`armv7-unknown-linux-gnueabihf`、`aarch64-unknown-linux-gnu`、`i686-unknown-linux-gnu`、`x86_64-unknown-linux-gnu`。
- JSON 字段与枚举保持现有 `telemetry.state`、`telemetry.error.kind/code`、`healthState`；不修改 schema。
- `available` 不等于 `healthy`；`unavailable`、`unsupported` 的健康等级为 `unknown`。
- 权限错误必须如实呈现；不得自动提权、读取 SD 的 EXT_CSD 或执行硬件写操作。
- 新用户文案同时提供简体中文和英文，动态错误必须转义，切换语言后重新渲染。
- 测试必须运行实际函数或实际命令接线，不能用复制的业务实现证明修复。
- 环境缺少交叉目标、系统库、Tauri 或运行器时记录阻塞，不将未执行检查写成通过。
- 只实施 R1/R4/R5/R6/R7；不顺带修复 EFI 回滚和通用 TUI 换行/溢出问题。
- 本次用户只要求文档。未来实施中的 commit 步骤仅在用户另行授权提交后执行；每项可独立审查，不自动提交或推送。

## 文件与职责

| 文件 | 操作 | 职责 |
|---|---|---|
| `crates/rsetup-core/src/mmc/sys.rs` | 修改 | 架构条件编译、ABI 断言、原始读取入口 |
| `.github/workflows/ci.yml` | 修改 | 四架构 cargo check；不改变现有 Debian 发布矩阵 |
| `apps/desktop/src-tauri/src/main.rs` | 修改 | Storage handler、实际注册表复用、IPC 测试 |
| `apps/desktop/src-tauri/Cargo.toml` | 修改 | 仅测试所需 Tauri test feature 和 serde_json dev-dependency |
| `apps/desktop/src-tauri/Cargo.lock` | 必要时更新 | 记录测试依赖解析；不手工编辑 lock |
| `ui/desktop-ipc.test.mjs` | 新增 | UI 字面量 IPC 命令与真实注册表一致性 |
| `ui/app.js` | 修改 | 全局刷新状态机、调用点、MMC 可用性渲染 |
| `ui/hardening.test.mjs` | 修改 | 全局刷新合并、失效、慢轮询、错误和恢复 |
| `ui/storage-refresh.test.mjs` | 修改 | 实际 init 定时器接线、存储与全局刷新独立性 |
| `ui/storage.test.mjs` | 修改 | MMC 三态、错误与中英文文案 |
| `ui/i18n.js`、`ui/i18n.test.mjs` | 修改 | Web 新文案及字典一致性 |
| `crates/rsetup-app/src/main.rs` | 修改 | MMC CLI 文本及单元测试 |
| `crates/rsetup-app/src/tui.rs` | 修改 | MMC 文本、枚举错误状态、实际渲染测试 |
| `crates/rsetup-app/src/i18n.rs` | 修改 | Rust Locale 文案及遥测错误格式化 |

Task 1/2/3 可分别审查；Task 4 完成三端遥测语义后执行 Task 5；最后 Task 6 汇总回归。代码位置以函数名为准，避免行号因先前任务修改漂移。

## 执行前检查

- [ ] 运行 `git status --short` 和 `git rev-parse HEAD`，确认用户工作并记录实际基线。
- [ ] 阅读 Spec，确认 A1/A4/A5/A6/A7/AX 的含义。
- [ ] 运行 `rustc --version`、`rustup target list --installed`、`node --version`。桌面 crate 不在根 workspace 的测试覆盖内，必须单独检查。
- [ ] 确定可用的交叉目标和桌面 Linux 系统库。缺失时安装对应环境或记录阻塞；不能用依赖错误当 RED。

---

## Task 1：R1 架构条件编译与 MMC ABI

**Files:** 修改 `crates/rsetup-core/src/mmc/sys.rs`、`.github/workflows/ci.yml`。

**Consumes:** Linux MMC `_IOWR(179, 0, struct mmc_ioc_cmd)`；当前 `MmcIocCmd` 和 `MmcError`。

**Produces:** 保持 `pub fn read_ext_csd_raw(dev_path: &str) -> Result<[u8; 512], MmcError>`；新增按架构互斥的 `MMC_IOC_CMD` 定义及编译期 ABI 校验。

- [ ] **1. 写 ABI 检查。** 在受支持架构 cfg 下增加编译期断言，使 cargo check 也能验证 ABI；另加同名运行时测试，便于本机报告。

```rust
const _: () = {
    assert!(MMC_IOC_CMD as u64 == 0xc048_b300);
    assert!(std::mem::size_of::<MmcIocCmd>() == 72);
    assert!(std::mem::offset_of!(MmcIocCmd, data_ptr) == 64);
};

#[test]
fn mmc_ioctl_abi_matches_linux_uapi() {
    assert_eq!(MMC_IOC_CMD as u64, 0xc048_b300);
    assert_eq!(std::mem::size_of::<MmcIocCmd>(), 72);
    assert_eq!(std::mem::offset_of!(MmcIocCmd, data_ptr), 64);
}
```

- [ ] **2. RED：使用真实 32 位目标。** 安装目标后执行：

```bash
rustup target add armv7-unknown-linux-gnueabihf i686-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu
cargo check -p rsetup-core --lib --locked --target armv7-unknown-linux-gnueabihf
cargo check -p rsetup-core --lib --locked --target i686-unknown-linux-gnu
```

预期当前常量报 E0308：expected u32, found u64。本机 ABI 测试在旧代码上可能通过，这是应由交叉编译抓住的回归，不伪造本机失败。

- [ ] **3. 最小实现。** 为两组架构添加明确分支；注释说明请求编码与结构体字段偏移，不错误声称 i686 类型总是 8 字节对齐。

```rust
#[cfg(any(target_arch = "arm", target_arch = "x86"))]
const MMC_IOC_CMD: libc::c_ulong =
    3u32 << 30 | 72u32 << 16 | (MMC_BLOCK_MAJOR as u32) << 8;

#[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
const MMC_IOC_CMD: libc::c_ulong =
    3u64 << 30 | 72u64 << 16 | (MMC_BLOCK_MAJOR as u64) << 8;
```

将真实 `read_ext_csd_raw` 及只供该入口使用的 FFI 项放在同一受支持架构 cfg 下。对其余架构添加互斥入口：

```rust
#[cfg(not(any(
    target_arch = "arm", target_arch = "x86",
    target_arch = "aarch64", target_arch = "x86_64"
)))]
pub fn read_ext_csd_raw(_dev_path: &str) -> Result<[u8; 512], MmcError> {
    Err(MmcError::NotSupported(
        "MMC ioctl ABI is not implemented for this target architecture".into(),
    ))
}
```

不要 gate 掉 sysfs、解析器或注入 reader。保留现有 `telemetry_from_mmc_error` 映射，不将 host 缺少读取路径误说成设备 unsupported。

- [ ] **4. GREEN：逐目标编译，运行本机行为回归。**

```bash
cargo check --workspace --all-targets --locked --target armv7-unknown-linux-gnueabihf
cargo check --workspace --all-targets --locked --target aarch64-unknown-linux-gnu
cargo check --workspace --all-targets --locked --target i686-unknown-linux-gnu
cargo check --workspace --all-targets --locked --target x86_64-unknown-linux-gnu
cargo test -p rsetup-core --locked mmc::
```

四目标 cargo check 不运行跨架构二进制，不能宣称已验证 ioctl 真机行为；编译期断言提供请求值和布局证据。

- [ ] **5. CI 加入四目标 check 矩阵。** 每个 job 用 stable 工具链安装对应 `targets`，运行上面的 `cargo check --workspace --all-targets --locked --target ${{ matrix.target }}`。保留现有本机测试、MSRV 和 Debian amd64/arm64 jobs。
- [ ] **6. 审查差异，记录 RED/GREEN；获提交授权时单独提交。** 建议消息：`fix(core): select MMC ioctl ABI by target architecture`。

## Task 2：R4 桌面 Storage IPC

**Files:** 修改桌面 `src-tauri/src/main.rs`、`Cargo.toml`，必要时更新桌面 `Cargo.lock`；新增 `ui/desktop-ipc.test.mjs`。

**Consumes:** `Controller::storage_status(&self) -> Result<StorageStatus, HardwareError>`；现有 `CommandError` 转换。

**Produces:** 实际注册的异步 `storage_status` 命令；供生产和 mock app 共用的 `command_handler<R: tauri::Runtime>()` 注册表函数。

- [ ] **1. 添加低成本命令一致性失败测试。** 新文件内容：

```javascript
import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

test("desktop registers every literal shared UI IPC command", () => {
  const ui = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");
  const rust = fs.readFileSync(new URL(
    "../apps/desktop/src-tauri/src/main.rs", import.meta.url,
  ), "utf8");
  const blocks = [...rust.matchAll(/generate_handler!\s*\[([\s\S]*?)\]/g)];
  assert.equal(blocks.length, 1, "production and tests must share one registry");
  const registered = new Set(blocks[0][1].match(/\b[a-z_][a-z_0-9]*\b/g));
  const calls = [...ui.matchAll(/tauriInvoke\(\s*["']([^"']+)["']/g)];
  assert.ok(calls.some((match) => match[1] === "storage_status"));
  for (const [, name] of calls) assert.ok(registered.has(name), name);
});
```

- [ ] **2. RED：** `node --test ui/desktop-ipc.test.mjs`，预期缺少 `storage_status`。此检查只验证名字，不能替代下面的实际 dispatch。
- [ ] **3. 添加 Tauri dispatch 测试。** 开启仅 dev-dependency 的 `tauri` `test` feature，并加入 `serde_json = "1"` dev-dependency。在桌面 `main.rs` 测试模块使用 `tauri::test::{mock_builder, mock_context, noop_assets, get_ipc_response}`，构造 mock app 并注册生产 `command_handler()`。

下面是成功路径核心测试；`CommandError` 不需要新增生产 API：

```rust
#[test]
fn desktop_storage_ipc_returns_demo_contract() {
    let app = tauri::test::mock_builder()
        .manage(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun))
        .invoke_handler(command_handler())
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let webview = tauri::WebviewWindowBuilder::new(
        &app, "main", tauri::WebviewUrl::default(),
    ).build().unwrap();
    let response = tauri::test::get_ipc_response(
        &webview,
        tauri::webview::InvokeRequest {
            cmd: "storage_status".into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(serde_json::json!({})),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        },
    ).unwrap();
    let value = response.deserialize::<serde_json::Value>().unwrap();
    assert!(value["nvme"]["devices"].as_array().unwrap().len() > 0);
    assert!(value["mmc"]["devices"].as_array().unwrap().len() > 0);
    assert!(value["mmc"]["devices"][0]["telemetry"]["state"].is_string());
    assert!(value["mmc"]["devices"][0]["healthState"].is_string());
}
```

将当前唯一的 `generate_handler![...]` 提取到泛型注册表函数，生产 `.invoke_handler(command_handler())` 和测试使用同一函数。先不加入 Storage，运行 dispatch 测试，确认 unknown command 的 RED。Tauri lock 中 minor 版本的测试 API 以实际依赖为准，调整测试适配，不改生产 IPC 契约。

- [ ] **4. 最小实现 handler 并注册。**

```rust
#[tauri::command]
async fn storage_status(
    controller: tauri::State<'_, Controller>,
) -> Result<rsetup_core::StorageStatus, CommandError> {
    let controller = controller.inner().clone();
    tauri::async_runtime::spawn_blocking(move || controller.storage_status())
        .await
        .map_err(CommandError::internal)?
        .map_err(CommandError::from)
}
```

注册表函数返回 `impl Fn(tauri::ipc::Invoke<R>) -> bool + Send + Sync + 'static`，函数体为包含所有原命令及 `storage_status` 的 `tauri::generate_handler![...]`。保留 live opt-in、生产资产和原有命令。

- [ ] **5. 补充实际 dispatch 错误与新鲜度测试。** 使用 `Controller::with_storage_reader(ProbeMode::Live, ExecutionPolicy::DryRun, Arc<dyn StorageReader>)`。fake 的两个方法签名为 `fn nvme_status(&self) -> Result<NvmeStatus, HardwareError>` 和 `fn mmc_status(&self) -> Result<MmcStatus, HardwareError>`；用 `Arc<Mutex<StorageStatus>>` 保存可变结果，另用标记控制返回 `HardwareError::Io("injected storage failure".into())`。在同一 mock app 连续调用两次，修改设备型号后断言响应变化；开启失败标记后断言 rejection 的 code=`internal_error`、message 保留注入原因，不是 unknown command。Demo 用调用即 panic 的 reader 验证从未调用它。通过实际 dispatch 测试的异步完成确认 blocking worker 结果已被等待。
- [ ] **6. GREEN：**

```bash
node --test ui/desktop-ipc.test.mjs
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --locked desktop_storage_
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --locked
```

第一次增加 dev-dependency 时先正常解析更新桌面 lock，再执行 `--locked`。缺少 GTK/WebKit 系统库或 Tauri 下载条件时标记阻塞，不能仅凭 Node 检查宣称桌面通过。

- [ ] **7. 审查并记录；获授权时提交。** 建议消息：`fix(desktop): register asynchronous storage status command`。

## Task 3：R5 全局刷新活性与操作后失效

**Files:** 修改 `ui/app.js`、`ui/hardening.test.mjs`、`ui/storage-refresh.test.mjs`。

**Consumes:** 原 `refreshAll`/`refreshOnce`、四个 transport 方法、真实 `init()` 定时器及全部变更调用方。

**Produces:** `refreshAll({ quiet = false, invalidate = false } = {})`；内部 `refreshEpoch` 初始为 0，`refreshOnce({ quiet = false, epoch = 0 } = {})`。

- [ ] **1. 在现有 harness 内添加慢轮询 RED。** 复用文件里的 `harness`，不复制生产刷新逻辑：

```javascript
test("routine ticks cannot starve a slow successful refresh", async () => {
  const { c } = harness(["refreshAll", "refreshOnce"]);
  const requests = [], rendered = [];
  c.state = {};
  c.applyDebugDevice = value => value;
  c.renderAll = () => rendered.push(c.state.snapshot.id);
  c.transport = {
    snapshot: () => new Promise(resolve => requests.push(resolve)),
    actions: async () => [], activity: async () => [],
    sourceStatus: async () => ({ sourceRevision: "same" }),
  };
  const first = c.refreshAll();
  const ticks = Array.from({ length: 4 }, () => c.refreshAll({ quiet: true }));
  requests[0]({ id: "slow-success", synthetic: true });
  for (let i = 0; i < 20; i++) await Promise.resolve();
  assert.deepEqual(rendered, ["slow-success"]);
  assert.equal(requests.length, 1);
  await Promise.all([first, ...ticks]);
  assert.equal(c.state.refreshing, false);
  assert.equal(c.document.body.dataset.state, "demo");
});
```

- [ ] **2. 更新旧的 queued-refresh 测试调用语义。** 将表示“操作后读取”的第二次调用改为 `c.refreshAll({ quiet: true, invalidate: true })`，仍要求 A 不渲染、B 完成后所有等待者结束。补充以下独立测试：

| 测试名 | 操作序列与核心断言 |
|---|---|
| `obsolete refresh errors are discarded` | A pending → invalidate → reject A；无旧 toast/error；B 成功后渲染 B |
| `multiple invalidations coalesce but later changes reread` | A 期间两次 invalidate → 仅 B；B 期间 invalidate → C；仅 C 落地 |
| `latest failure clears drain and allows retry` | 最新批次 reject → error、Promise=null；重试成功 → demo/live |
| `manual refresh joins pending polling` | poll pending → manual；一批请求，手动调用最终结束 |

原变更 handler 的 harness 将 `refreshAll` stub 改为记录 options，逐个断言成功路径至少调用一次 `invalidate === true`；否则仅修状态机仍会漏掉业务接线。

- [ ] **3. RED：** `node --test ui/hardening.test.mjs`。应看到慢请求没有渲染、旧错误泄漏或调用点未传 invalidate 的断言失败，不能接受测试超时作为唯一证据。
- [ ] **4. 最小实现入口队列。**

```javascript
async function refreshAll({ quiet = false, invalidate = false } = {}) {
  if (typeof refreshStorageTool === "function") void refreshStorageTool();
  state.refreshEpoch ??= 0;
  if (invalidate) {
    state.refreshEpoch += 1;
    state.refreshRequested = true;
  }
  if (!quiet) state.refreshLoud = true;
  if (state.refreshPromise) return state.refreshPromise;
  state.refreshRequested = true;
  state.refreshPromise = (async () => {
    while (state.refreshRequested) {
      state.refreshRequested = false;
      const nextQuiet = !state.refreshLoud;
      state.refreshLoud = false;
      await refreshOnce({ quiet: nextQuiet, epoch: state.refreshEpoch });
    }
  })().finally(() => {
    state.refreshPromise = null;
    state.refreshLoud = false;
  });
  return state.refreshPromise;
}
```

`refreshOnce` 成功和 catch 分支的所有副作用前均加入 `if (epoch !== state.refreshEpoch) return;`，删除原 `if (state.refreshRequested) return`。保留最终状态清理，最新成功/失败路径照常渲染；不要改变 storage drawer 自己的 generation。

- [ ] **5. 更新全部变更调用点并验证实际定时器。** 对 `executeSelectedAction`、`applySourcePlan`、`applyOverlays`、`applyThermalPolicy`、`applyFanCurve`、`applyLedConfiguration`、`applySpiFlash` 中已有的操作后读取传入 `invalidate: true`。其他同类变更读取也逐个审计。启动/手动/timer 保持默认 false。在 `storage-refresh.test.mjs` 现有实际 `init()` harness 捕获 `setInterval` 回调，保持 transport pending，触发四次回调，再 resolve，断言只发一批、数据更新且全局 Promise 结束；不等待真实 40 秒。
- [ ] **6. GREEN：**

```bash
node --test ui/hardening.test.mjs ui/storage-refresh.test.mjs
```

必须保留“storage never resolves but snapshot does”的原回归，确认存储采集没有被加入全局 `Promise.all`。

- [ ] **7. 审查并记录；获授权时提交。** 建议消息：`fix(web): separate polling from invalidating refreshes`。

## Task 4：R6 MMC CLI/TUI/Web 三态展示

**Files:** 修改 `crates/rsetup-app/src/main.rs`、`tui.rs`、`i18n.rs`；`ui/app.js`、`storage.test.mjs`、`i18n.js`、`i18n.test.mjs`。

**Consumes:** `TelemetryStatus`、`TelemetryError`、`TelemetryReadState`、`HealthState`、`MmcHealth`；Spec §7 真值表。

**Produces:** 三端一致的可用性语义；Rust 新增 `Locale::storage_telemetry_error(&self, error: Option<&rsetup_core::TelemetryError>) -> String` 用于 CLI/TUI 原因格式化。它返回原因，不重复外层的“不可读取”标签。

- [ ] **1. CLI RED 测试。** 在 `main.rs` 的现有 tests 中用真实 Demo 数据构造失败设备，调用真实 formatter：

```rust
#[test]
fn mmc_unavailable_preserves_reason_in_both_cli_commands() {
    use rsetup_core::{HealthState, MmcHealth, TelemetryError,
        TelemetryErrorKind, TelemetryReadState, TelemetryStatus};
    let controller = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun);
    let mut storage = controller.storage_status().unwrap();
    storage.mmc.devices.truncate(1);
    let dev = &mut storage.mmc.devices[0];
    dev.health = MmcHealth::default();
    dev.health_state = HealthState::Unknown;
    dev.telemetry = TelemetryStatus {
        state: TelemetryReadState::Unavailable,
        error: Some(TelemetryError {
            kind: TelemetryErrorKind::PermissionDenied, code: Some(13),
        }),
    };
    for (locale, reason) in [(Locale::En, "Permission denied (13)"),
                             (Locale::ZhCn, "权限不足 (13)")] {
        let mmc = format_mmc_status(&storage.mmc, locale);
        let combined = format_storage_status(&storage, locale);
        assert!(mmc.contains(reason), "{mmc}");
        assert!(combined.contains(reason), "{combined}");
        assert!(!mmc.lines().any(|line| {
            (line.contains("Warning Flags:") && line.trim_end().ends_with("None"))
                || (line.contains("告警标志:") && line.trim_end().ends_with('无'))
        }));
    }
}
```

- [ ] **2. TUI RED 测试。** 在 `tui.rs` tests 创建 `App::new(Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun), locale)`；修改其中第一块 MMC，使用步骤 1 的 unavailable fixture。调用实际 `mmc_device_lines` 并收集 `Line.spans` 的 `content`，断言中英文原因和 `(13)`；再通过 `render_storage_summary` 的 TestBackend 测试确认原因真正出现在卡片中，而非只在辅助函数返回值里。
- [ ] **3. Web RED 测试。** 加到已有 `storage.test.mjs`，复用其真实 i18n 和 renderer harness：

```javascript
test("MMC unavailable explains permission failure in both locales", () => {
  for (const [language, reason] of [
    ["en", "Permission denied (13)"], ["zh-CN", "权限不足 (13)"],
  ]) {
    const i18n = loadI18n(language);
    const { context } = createTestContext({ t: (key, params) => i18n.t(key, params) });
    const device = structuredClone(emmcDevice);
    device.health = { preEolInfo: 0, lifeTimeEstAPercent: null,
      lifeTimeEstBPercent: null, warningFlags: [] };
    device.healthState = "unknown";
    device.telemetry = { state: "unavailable",
      error: { kind: "permission_denied", code: 13 } };
    const html = context.renderStorageMmcCard(device);
    assert.ok(html.includes(reason), html);
    assert.doesNotMatch(html, /data-metric-percent/);
  }
});
```

如果新增独立 JS 文案 helper，必须将真实函数加载进现有 VM harness；不以 stub 返回期望文案。

- [ ] **4. RED：**

```bash
cargo test -p rsetup-next --bin rsetup-next --locked mmc_unavailable
node --test ui/storage.test.mjs
```

预期 formatter/renderer 没有输出权限原因。TUI 测试名也以 `mmc_unavailable` 开头，以此命令覆盖。

- [ ] **5. 最小实现。** CLI 和 TUI 在身份信息后先按 `dev.telemetry.state` 分支；unavailable 显示不可读取及 `locale.storage_telemetry_error(dev.telemetry.error.as_ref())`，unsupported 显示不支持健康指标；二者均跳过健康数值和无告警结论。available 保留指标，单字段 None/0 使用 Undefined，所有健康字段未定义且 flags 为空时告警使用 Unknown。Web 同步处理，继续使用已有 healthState 和 stale 覆盖规则。

Rust 原因格式化核心：

```rust
pub fn storage_telemetry_error(
    &self, error: Option<&rsetup_core::TelemetryError>,
) -> String {
    use rsetup_core::TelemetryErrorKind;
    let Some(error) = error else {
        return self.text("storage_telemetry_unavailable").to_string();
    };
    let key = match error.kind {
        TelemetryErrorKind::PermissionDenied => "storage_permission_denied",
        TelemetryErrorKind::Io => "storage_read_failed",
        TelemetryErrorKind::NvmeStatus => "storage_protocol_error",
    };
    match error.code {
        Some(code) => format!("{} ({code})", self.text(key)),
        None => self.text(key).to_string(),
    }
}
```

新增并测试上述三个词条；协议错误中文为“设备协议错误”，英文为“Device protocol error”。无 error 时调用方只输出一次不可读取标签，避免重复。Web 采用同样存在性判断，不用 `code || 0` 或 `code ?? 0`。

- [ ] **6. 完整真值表测试。** 三端分别覆盖下表，每条都断言身份保留、unknown/现有等级正确以及不存在误导性文本：

| 输入 | 正向断言 | 负向断言 |
|---|---|---|
| PermissionDenied，13 和 1 | 对应原因与真实 code | 不支持、健康、无告警 |
| Io，5 | Read failed/读取失败 (5) | 不支持、无告警 |
| Io，None；Unavailable，None | 通用原因或不可读取 | `(0)`、虚构 errno |
| Unsupported | 不支持健康指标 | 读取失败、健康数值条 |
| Available，部分字段未定义 | 有效字段保留，Undefined | 缺失字段被说成不支持 |
| Available，全部未定义 | Unknown + Undefined | Healthy、无告警 |
| Available，Healthy/Warning/Critical | 原等级、数值与告警保持 | 错误降级或虚构警报 |
| 缺少 telemetry 的旧 JSON（Web） | Unknown + Unavailable | 从 flags 推断 Healthy |
| Web stale + unavailable | stale 提示与 unknown 保持 | 本次采集成功或绿色 healthy |

Web 另验证语言切换重新翻译错误、动态字符串转义。转义测试须加载真实 `escapeHtml`，不能使用现有 `createTestContext` 中删除特殊字符的 stub 作为安全性证据。Rust 检查 Locale 词条，Web 检查双语字典 key/placeholder 一致性。

- [ ] **7. GREEN：**

```bash
cargo test -p rsetup-next --bin rsetup-next --locked
node --test ui/storage.test.mjs ui/storage-refresh.test.mjs ui/i18n.test.mjs
```

- [ ] **8. 审查并记录；获授权时提交。** 建议消息：`fix(storage): expose MMC telemetry availability across views`。

## Task 5：R7 TUI 枚举错误及恢复

**Files:** 修改 `crates/rsetup-app/src/tui.rs`、`i18n.rs`。

**Consumes:** `StorageReader` 两个独立的 `Result`、App 原有 status 字段；Task 4 的本地化方式。

**Produces:** App 的 `nvme_error: Option<rsetup_core::HardwareError>`、`mmc_error: Option<rsetup_core::HardwareError>`；私有 `refresh_storage(&mut self)`，启动和 `refresh` 共用；不修改公共模型。

- [ ] **1. 新增可变失败 reader 测试 fixture。** 放在 `tui.rs` tests 中：

```rust
#[derive(Clone)]
struct EnumerationFixture(std::sync::Arc<std::sync::Mutex<(
    rsetup_core::StorageStatus, bool, bool,
)>>);

impl rsetup_core::StorageReader for EnumerationFixture {
    fn nvme_status(&self) -> Result<rsetup_core::NvmeStatus, rsetup_core::HardwareError> {
        let state = self.0.lock().unwrap();
        if state.1 { Err(rsetup_core::HardwareError::Io("nvme-enumeration-eio".into())) }
        else { Ok(state.0.nvme.clone()) }
    }
    fn mmc_status(&self) -> Result<rsetup_core::MmcStatus, rsetup_core::HardwareError> {
        let state = self.0.lock().unwrap();
        if state.2 { Err(rsetup_core::HardwareError::Io("mmc-enumeration-eio".into())) }
        else { Ok(state.0.mmc.clone()) }
    }
}

fn storage_card_text(app: &App) -> String {
    let backend = ratatui::backend::TestBackend::new(120, 40);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| {
        render_storage_summary(frame, app, Rect::new(0, 0, 120, 40));
    }).unwrap();
    terminal.backend().buffer().content().iter()
        .map(|cell| cell.symbol()).collect::<String>()
}
```

- [ ] **2. 核心 RED 测试启动双失败。**

```rust
#[test]
#[cfg(target_os = "linux")]
fn storage_enumeration_errors_are_not_no_devices() {
    use std::sync::{Arc, Mutex};
    let demo = Controller::new(ProbeMode::Demo, ExecutionPolicy::DryRun)
        .storage_status().unwrap();
    let shared = Arc::new(Mutex::new((demo, true, true)));
    let controller = Controller::with_storage_reader(
        ProbeMode::Live, ExecutionPolicy::DryRun,
        Arc::new(EnumerationFixture(shared)),
    );
    let app = App::new(controller, Locale::En).unwrap();
    let text = storage_card_text(&app);
    assert!(text.contains("NVMe device enumeration failed"), "{text}");
    assert!(text.contains("MMC device enumeration failed"), "{text}");
    assert!(!text.contains(app.locale.text("storage_not_detected")), "{text}");
}
```

- [ ] **3. RED：** `cargo test -p rsetup-next --bin rsetup-next --locked storage_enumeration`。基线应显示未检测到设备而触发断言。
- [ ] **4. 最小实现状态处理。** `App::new` 构造空 status、两个 error=None，再调用共享 `refresh_storage()`；`App::refresh` 移除原 `unwrap_or_else`，改调用该方法。分别 match 两个查询，不因第一个失败提前返回。NVMe 分支如下；MMC 分支同样显式实现并测试：

```rust
match self.controller.nvme_status() {
    Ok(status) => {
        self.nvme_status = status;
        self.nvme_error = None;
    }
    Err(error) => {
        self.nvme_status = rsetup_core::NvmeStatus {
            initialized: false, devices: vec![], message: None,
        };
        self.nvme_error = Some(error);
    }
}
match self.controller.mmc_status() {
    Ok(status) => {
        self.mmc_status = status;
        self.mmc_error = None;
    }
    Err(error) => {
        self.mmc_status = rsetup_core::MmcStatus {
            initialized: false, devices: vec![], message: None,
        };
        self.mmc_error = Some(error);
    }
}
```

这两段是 `refresh_storage` 的函数体；无 `?`，因此会查询双方。渲染用固定双语子系统摘要 + `error.to_string()` 详情，不用 message 文本推断失败。未知底层详情原样作为文本显示。

- [ ] **5. 最小渲染与高度调整。** 在设备行前加入错误摘要；先从 budget 扣除错误行，再排成功设备。`render_mission` 计算存储卡高度时计入同样的错误行。无设备短路改为“双 error=None 且双列表为空”。紧凑高度优先保留错误摘要，不能让单盘身份长文本挤掉所有枚举错误。此项不更改通用 `wrapped_rows` 算法。
- [ ] **6. 覆盖全部状态转换。** 复用同一 shared fixture 和同一 App，分别以中英文执行：双空成功；NVMe 失败/MMC 空；MMC 失败/NVMe 有设备；NVMe 失败/MMC 有设备；双失败；失败→成功（清除 error）；成功→失败（清除旧设备）。断言成功侧型号仍可见，错误侧原型号不再显示。另将单盘 telemetry 改 unavailable 但查询仍 Ok，断言只显示 Task 4 的遥测错误，不显示枚举错误。
- [ ] **7. 验证真实整屏布局。** 用 120×40 和 100×28 TestBackend 调用实际 `render`，确认错误摘要可见且应用不 panic；详细长错误可截断。不要仅断言 App 字段或孤立行生成器。
- [ ] **8. GREEN：**

```bash
cargo test -p rsetup-next --bin rsetup-next --locked storage_enumeration
cargo test -p rsetup-next --bin rsetup-next --locked tui::tests::
```

- [ ] **9. 审查并记录；获授权时提交。** 建议消息：`fix(tui): preserve storage enumeration errors across refreshes`。

## Task 6：验收矩阵与最终回归

**Files:** 只处理上述任务暴露的相关问题，不新增功能。执行证据附在本计划末尾或实现交付报告中。

- [ ] **1. 对照 Spec 验收矩阵。** A1/A4/A5/A6/A7 每项列出新增测试名、RED 失败原因和 GREEN 命令；缺少一个前端的 R6 测试不能视为完成。
- [ ] **2. 运行基础回归。**

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
node --test ui/*.test.mjs
cargo +1.85.0 test --workspace --locked
```

- [ ] **3. 检查独立桌面 crate。**

```bash
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml -- --check
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --locked
cargo check --manifest-path apps/desktop/src-tauri/Cargo.toml --locked
```

- [ ] **4. 在最终代码状态执行 Task 1 的四目标 check 矩阵。** 若前次检查后没有任何影响 Rust 的改动，可引用同一最终 SHA 的检查结果，不为增加次数重复测试。
- [ ] **5. Demo 冒烟。** 运行 CLI 的 `--demo hardware mmc` 和 `--demo hardware storage`，打开桌面 Demo 的 Hardware → Storage，刷新后仍能显示两类设备；记录桌面执行条件。注入 fixture 的失败测试提供错误路径证据，不对真实设备执行权限或 I/O 破坏实验。
- [ ] **6. 最终差异检查。** `git diff --check`、`git diff --stat`、`git status --short`；确认没有 R2/R3/R8/R9 的实现混入，没有修改真实设备状态。

## 实施交付记录要求

交付时报告：实际基线与最终 SHA（未提交时说明工作区状态）、五个问题对应文件、各 RED/GREEN 结果、四架构编译结果、桌面独立测试结果、未运行项及具体原因。

本计划不预填通过数量，不把此前复审的测试结果移作本次修复证据。所有复选框只在执行并核实后勾选。

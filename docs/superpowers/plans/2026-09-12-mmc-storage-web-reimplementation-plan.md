# 统一存储 (Storage) Web 呈现 重实现 计划

日期：2026-09-12。目标：恢复 Web 侧（REST + 前端）的 MMC/统一存储集成，并修掉上次实机核验发现的两项前端缺陷 W1/W2；随后在两台 SBC 上重新实机验证。

## 1. 背景

TUI/CLI/核心的 MMC 支持已在 `dev-aghost`（`ebc4f69`…`5b14b73`，已推送）。Web 侧集成曾实现并通过实机核验，但所在提交已按序列号脱敏要求回滚并物理删除，当前分支上不存在（逐文件核验：`probe.rs` capability 为 `nvme`、`server.rs` 只有 `/api/v1/hardware/nvme`、`ui/app.js` 工具集为 `nvme`、`index.html` 无 `icon-storage`、`styles.css` 无 `storage-mmc-card`、`i18n.js` 无 `storageTool.*`）。

恢复材料：上次 SDD 运行保留的 review package 中带完整 `git diff`，覆盖被回滚链的全部 6 个代码提交。**以补丁恢复为默认路径**，不做从零重写（偏差更小、测试与实现同时回来）；仅当某补丁无法应用时才改用手工重写，并在台账记录裁定。

## 2. 恢复材料（已解包到本计划工作区 `recovery/`）

| 补丁 | 原提交 | 内容 | 目标文件 |
| --- | --- | --- | --- |
| `1-backend-probe-detail.patch` | `5a40fca` | `storage_detail()` helper + demo 快照 `storage` 条目 | `crates/rsetup-core/src/probe.rs` |
| `2-backend-probe-live.patch` | `bae3e23` | live 分支由 `nvme` capability 改为 `storage` | `crates/rsetup-core/src/probe.rs` |
| `3-backend-probe-fix.patch` | `11f5262` | 零设备时保留统一文案（绕过 `capability()` 的 detail 覆盖） | `crates/rsetup-core/src/probe.rs` |
| `4-backend-api.patch` | `d8c3481` | 路由 `/api/v1/hardware/nvme` → `/api/v1/hardware/storage`，handler 改名 | `crates/rsetup-app/src/server.rs` |
| `5-frontend-i18n-icon-css.patch` | `ae94e09` | `storageTool.*` i18n 键（en/zh-CN）+ `#icon-storage` + MMC 卡片 CSS + i18n 测试 | `ui/i18n.js`、`ui/i18n.test.mjs`、`ui/index.html`、`ui/styles.css` |
| `6-frontend-storage-tool.patch` | `9713fb0` | `renderStorageTool`/`renderStorageMmcCard` + 工具注册改写；`ui/nvme.test.mjs` 删除、`ui/storage.test.mjs` 新增 | `ui/app.js`、`ui/storage.test.mjs`、`ui/nvme.test.mjs` |

基线（原链）文档提交 `10f0fe6`/`33be9df`/`83b0731`（Web 设计规范与 TDD 计划）**不在本次恢复范围**：它们随同批提交被移除，本次以本计划 + 已提交的实机报告 `docs/testing/mmc-2026-09-12/report.md` §4.2 行为清单作为验收依据。

## 3. Global Constraints

1. **破坏性变更沿用原决定**：移除 `GET /api/v1/hardware/nvme`（调用方 404）；snapshot capabilities 中 `nvme` → `storage`，availability = NVMe 与 MMC 任一非空，demo 为 `true`。
2. **数据模型与 controller 零改动**：`StorageStatus`/`MmcStatus`/`Controller::storage_status()` 已在分支上，直接复用；`MmcManager`/`NvmeManager` 不改。
3. **前端零新依赖**：纯前端渲染 + 既有 axum 路由；MMC 卡片复用既有 `nvme-*` 样式类与 `nvme.*` i18n 键。
4. **W1/W2 必须修**（本次新增工作，见 Task 3）：抽屉标题/描述走 `storageTool.*`；矩阵卡片 label 随中文界面本地化。
5. **实机值不入库**：截图序列号一律打码；文档不留序列号明文；本计划与报告不复述实机序列号。
6. 每个任务：先跑覆盖测试并留红/绿证据 → 提交 → 写报告文件。测试命令固定为 `cargo test --workspace --locked` 与 `node --test ui/*.test.mjs`；静态检查 `cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`。

## 4. 任务

### Task 1 — 后端恢复（core capability + REST 端点）

- 文件：`crates/rsetup-core/src/probe.rs`、`crates/rsetup-app/src/server.rs`。
- 步骤：按序应用 `recovery/1-backend-probe-detail.patch`、`2-backend-probe-live.patch`、`3-backend-probe-fix.patch`、`4-backend-api.patch`（`git apply`）；若某补丁冲突，先 `git apply --3way`，仍失败则按补丁内容手工等价实现并在报告说明。
- 测试：`cargo test --workspace --locked`；补丁自带的 `probe.rs` / `server.rs` 测试必须全绿（含：`storage` capability 在仅 MMC 时可用、两类皆无时不可用、demo detail 文案、`/api/v1/hardware/storage` 200 且 demo 有 1 NVMe + 2 MMC、`/api/v1/hardware/nvme` 404）。
- 提交：`feat(core): restore unified storage capability and REST endpoint`（按实际改动拆分或合并）。

### Task 2 — 前端恢复（i18n / 图标 / 样式 / storage 工具）

- 文件：`ui/i18n.js`、`ui/i18n.test.mjs`、`ui/index.html`、`ui/styles.css`、`ui/app.js`、`ui/storage.test.mjs`、`ui/nvme.test.mjs`。
- 步骤：按序应用 `recovery/5-frontend-i18n-icon-css.patch`、`6-frontend-storage-tool.patch`。
- 测试：`node --test ui/*.test.mjs` 全绿；`ui/storage.test.mjs` 覆盖空态、仅 NVMe、仅 MMC、双设备、`preEolInfo 2/3` 徽章、`lifeTimeEstBPercent: null` → N/A + `is-na`；`ui/i18n.test.mjs` 覆盖 `storageTool.*` 在 en/zh-CN 均可解析。
- 提交：`feat(web): restore unified storage hardware tool`。

### Task 3 — TDD 修复 W1 与 W2

- **W1（抽屉标题/描述文案）**：`ui/app.js` 的 `hardwareToolCopy()` prefix map 中 `storage` 目前指向 `storage.*`（系统页「存储空间」键族），导致抽屉标题为「存储空间」、描述命中缺失键回退 `text.unavailable`（「暂无详细信息」）。修复：让 `storage` 走 `storageTool.*`，即标题取 `storageTool.title`（en `Storage` / zh `存储`）、描述取 `storageTool.description`。
- **W2（矩阵卡片本地化）**：`ui/i18n.js` 的 `capabilityCopy` 增加 `storage` 条目（zh label `存储`，detail 回退文案 `NVMe 与 MMC/eMMC 存储设备`），并清理已失效的 `nvme` 条目与 `value.id === "nvme"` 死分支、`"NVMe controllers detected"` 残留字符串。
- 顺序要求：**先写失败测试并观察失败**（`node --test ui/storage.test.mjs` / `ui/i18n.test.mjs`），再改实现，最后全绿。
- 测试要求：新测试必须能在修复前失败、修复后通过；断言真实行为（`hardwareToolCopy("storage")` 的 title/description、`i18n.capability({id:"storage",...})` 的 zh label），不得只断言 mock。
- 提交：`fix(web): correct storage drawer copy and localize the matrix card`。

### Task 4 — 实机重新验证（由控制器执行，不派发子代理）

- 交叉编译 `aarch64-unknown-linux-gnu` release，部署到 `192.168.27.88`（Rock 5B：NVMe + eMMC）与 `192.168.27.34`（Rock 3A：仅 eMMC）。
- 采集：TUI（`scripts/capture-tui-remote.py` + `capture-tui-screenshot.py`，100×30，zh_CN）与 Web（`scripts/capture-web-screenshot.mjs --tool storage`，desktop 1440×900 / mobile 390×844 @2x / 矩阵卡片）。
- 判定项：报告 §4.2 的 16 项行为 + W1/W2 修复后的文案复核（抽屉标题应为「存储」、描述为存储工具描述；矩阵卡片 label 为「存储」）。
- 产物：打码后的截图归档 `docs/testing/mmc-2026-09-12/screenshots/`，更新 `report.md`（Web 章节由"上次核验"更新为本次重实现后的核验结果，W1/W2 标注为已修复），提交并推送。

## 5. 验收标准

1. `cargo test --workspace --locked`、`node --test ui/*.test.mjs`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` 全绿。
2. 破坏性变更落地：`/api/v1/hardware/storage` 200、`/api/v1/hardware/nvme` 404、capability id 为 `storage`。
3. W1/W2 有失败→通过的测试证据；实机截图中抽屉标题为「存储」、矩阵卡片中文标签为「存储」。
4. 两台实机截图归档且无序列号明文；报告更新并推送。

# Controller V1 跨专题集成与验收增量实施计划（2026-10-16）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 分项落实；用 `- [ ]` 跟踪步骤。这是**条件性后续计划**，不是现有草案获批、真库/硬件测试许可或生产发布授权。
>
> **本轮修订（2026-10-16，文档状态：尚未批准）：** 仅收敛四线 Task 4 的执行范围与安全门禁，不实现/扩展其他 V1 功能。完整 V1 的 AT-01..18、完整 SSE 与容量/发布压测移入「非当前 Task 4 后续索引」（不删除未来要求、不称已 PASS、不顺带实施）。

**Goal:** 在身份、设备传输、任务运行与 Web 分项交付之后，给中控 V1 建立可复现的双引擎、双进程、浏览器及容量/安全联合验收证据，不让离线测试冒充上线证据。**当前执行范围**为四线 Task 4 收敛验收（同源生产装配、9 条 Runtime 业务路由与 26 条管理路由、固定预览/幂等/授权全集、任务投影与恢复、板端 B04 与获批 wire、浏览器任务流程/a11y 与单二进制嵌入资源）；完整 V1 联合验收（AT-01..18、完整 SSE、容量/发布压测）为后续索引目标，两层分层、不互相冒充。

**Architecture:** 所有集成测试只消费已审专题产出的生产接线，不重新实现 auth、BoardClient、业务协议或前端权限判断。CI 提供普通离线回归；真实 MySQL/TiDB、两进程网络、浏览器与容量测量各自独立门控和留存去敏结果。入口为[总计划](2026-10-02-controller-v1-tdd-index.md)的 G0–G5；本计划当前执行范围负责 Task 4 四线收敛的证据（G5 子集），完整 AT-01..18 证据汇总列入非当前 Task 4 后续索引，不取代四份剩余计划与原计划中的 RED/GREEN。

**Tech Stack:** Rust 2024 / MSRV 1.85、Cargo、Axum、SQLx、MySQL/TiDB、Vue 3/Vitest；浏览器验收使用**零依赖手工受控真实浏览器清单（runbook）+ 既有 Vitest（jsdom）**。Playwright 为待审候选且**尚未获批**：本范围不创建其 config/spec、不安装、不自动选工具，后续若新增须另审明确批准。使用已获批准且锁定的版本，不因本计划隐式安装新依赖。

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` §3–6、同组 01–06（尤其 `06-web-acceptance.md` §6–8）、`docs/superpowers/specs/2026-09-22-controller-design.md`、`docs/protocol_spec.md`；执行者同时阅读对应规格及本计划。

## Global Constraints

- 本计划只规划未来工作；经本轮只读源码核实（设计树 worktree HEAD `405c0f51`）：`crates/rsetup-controller/src/main.rs` 仍只挂探活 router（`/healthz`、`/readyz`，生产装配未就位）；`apps/controller-web` 已有登录/强制改密/会话/设备列表/设备详情页面，任务页面尚未实现（旧稿“尚无设备/任务页面”不再整体成立，以“任务页面缺失”为准）；`Makefile:test` 只跑 Cargo 与既有单机 UI 的 Node 测试，CI 未将 controller-web Vitest/build 与真库验收接入。四线正式状态仍为 0/4 APPROVED，最近审查结论均为 REJECTED（各线正针对新一轮技术阻断进行内聚收敛：Identity ID-R6、Runtime RT-N、Transport TR-R6、Web WEB-R5，未获正式批准、未授权实施、未实现，当前仍处于执行计划收敛阶段）；接口清单虽有历史限定批准，仍非完整实施许可，其状态文字待同步，须与四完整计划准确区分。不得把新增计划、接口清单历史限定批准或上述既有页面误称为已验证结果。
- **文档静态检查与产品功能测试明确分层：** 本轮计划修订仅针对测试与验收追踪文档的审查缺陷进行规范校正，严禁将“文档修改完成”表述为“产品功能已修复”或“测试已通过”。当前所有功能实现、集成接线与测试用例仍处于未实现/待验证状态。
- 所有中控 V1 draft-1 新增接口/默认值/版本/阈值按 00 §5、01–06 显式审阅；已审批的窄范围切片不等于整个 V1 或部署批准。协议外部专家书面决议、现行协议修订、双端负例与互操作证据未齐时，G0/安全发布维持 BLOCKED；不伪造决议、向量、备份或压测报告。
- 本地 `secret/` 只有两个已明确授权的专用隔离可丢弃开发库可能用于未来测试；**凭据存在不等于授权任意操作**。真库步骤必须串行、逐引擎：① 先只读预检并确认真实目标（如 `SELECT DATABASE()`）、隔离可丢弃、实际权限与 schema/数据版本形状闭合；② 由执行方制作该目标的**真实备份**（私有 0700 目录 + 0600 文件），独立核验目标匹配、SHA256、可读性与可恢复性；③ 排他停写并满足显式迁移门禁后才运行需要这些条件的迁移。不能猜 URL、复制/回显凭据、宽泛运行 ignored 测试、扩大 grant、`DROP DATABASE` 或以 fixture 代替真实备份，不伪造 backup_ref/ACK；计划命令不得把 secret 派生的 URL 放进参数、报告或截图，凭据文件由程序进程内载入，不复制 secret。未满足则跳过真库/迁移并标“未验证”；有已核准 URL 但测试失败或实际运行数为零则记失败。
- **真实 DB 与双进程 smoke 隔离：** 门禁纯测试（离线无库装配检查）与完整真实 DB smoke 必须严格分轨。完整真实 DB smoke 必须标 `#[ignore]` 并通过显式单 case `--ignored --exact` 串行执行；严禁原命令普通 cargo 跑网络 DB，严禁注入“只读 DB fake”冒充初始化、登录写入与准入写事务，生产入口不增加 fake 仓储开关。仅当上述真实目标/备份/停写门禁齐备后才显式单 case 串行；**零行为断言骨架**（仅连接/注释/可空跑）不算真实 DB 证据。
- **Observer 锁观察证据红线（本轮收敛）：** 用户本轮只跳过**需要数据库 root 权限**的锁观察；因此“无独立 Observer 账号/最小权限（MySQL performance_schema 锁表 SELECT / TiDB PROCESS）、无真实接线”只维持对应锁观察项 BLOCKED，**不再整体阻断 Task 4**（旧 Observer 方案移入历史/后续索引，不恢复已暂停工具、不额外扩权）。普通应用账号下的真实事务、设备锁、CAS、撤权/提交并发仍必须验证。旧 writer 连接自观察绝对不是独立的 Observer 证据；MySQL 8.0 绝不能替代 8.4 LTS（TiDB 必须对应 8.5 LTS）。
- **真实静态资源发布与脱离目录硬约束：** 发布二进制缺少 `dist` 产物时 release 构建必须直接失败（严禁打包空目录）；运行时必须脱离源码与静态文件目录（在独立临时目录启动），无需 Node 和外部静态目录即可正常服务；测试必须解析构建产物中真实 JS/CSS 文件并验证内容、精确 MIME 与 immutable 缓存，严禁以空 HTML 或 assets 恒 404 伪装测试通过。
- 真实板端重启仅在专门审核的隔离环境才可测试；本计划两进程 smoke 必须使用可注入的 `RebootOs` 安全替身且不能触碰开发宿主机的 reboot。旧备份、授权撤销和任务 `unknown` 均不得在恢复时自动重发重启。
- RED 必须是可编译、可运行的行为断言失败；缺包、缺浏览器、未获批准的 DB、无法连服务、缺脚本均不算 RED。各项 GREEN 必须分别记录层级：纯函数 / fake / tower oneshot / 真 DB / 双进程 / 真实浏览器 / 物理硬件；未运行不记 PASS。非零测试数只是必要条件：仅读取 URL、注释说明、恒真断言或两个 mock 布尔值的测试不能作为 DB/恢复/互操作证据；fixture 与真实被测路径未实现时标 BLOCKED，不提供可空跑成功的 ignored 验收函数。G5 只在上游 G1–G4 与实际部署门禁均过后可验收。
- **依赖与供应链审查：** 未来依赖若新增成员或直接引用，必须包含 manifest 清单与离线 lock 文件严格审查（锁定 MSRV 1.85、许可证兼容、无新隐式网络依赖）；既有本地 `rsetup-app` 无认证 loopback HTTP 不能复用作远程管理。中控管理 HTTP 仅适用于受控可信网络，CSRF 不等于链路加密。
- **codec/crypto 门禁：** 须独立专家书面决议 + 用户复审 + 获批 codec 三者齐备，才验证精确 512KiB Protobuf 信封与真实认证/双流互通；齐备前 raw 字节入口持续 failclosed。不自行版本升级、不自造 protobuf 编码器或签名 ACK；typed/NoCodec 切片不证明精确 512KiB 或双端互通。
- **板端重启边界：** 测试中宿主真实 reboot 次数恒为 0（FakeOS 边界）；普通 reopen/重连不等真实断电证据；真实宿主 reboot 与真实电源故障属于后续索引（专门审核的隔离环境）。
- **浏览器工具与分层证据：** Playwright 尚未获批，本范围不创建其 config/spec、不安装、不自动选工具；执行层使用零依赖手工受控真实浏览器清单与既有 Vitest。fixture/mock 浏览器交互与真实同源生产端到端分层记录，**前者不批准后者**；后续若新增工具须另审明确批准，不暗示已批准。
- **TDD 与合入门禁：** 四份完整计划经指定模型复审通过后才进入 TDD；四线独立代码审查 + 实际真实集成验收全部通过后才本地 merge `dev-aghost`（用户已有整体授权，不重复询问）；禁 push、禁部分提前 merge。新 commit 信息示例用中文（type/scope 标记可保留英文），本计划不自动 commit。
- **条件性 runbook 与默认未验证：** 本计划收敛为明确条件性验收 runbook，逐项区分已存在接口/计划接口/未知依赖；所有未运行的项默认“未验证”，实质性专家/真实目标门禁保持明确 BLOCKED；文档文字预期不等于测试证据。

## 文件职责及上游依赖

| 文件（未来实施阶段） | 职责 / 前提 |
| --- | --- |
| `Makefile`、`.github/workflows/ci.yml`、`scripts/tests/controller-ci-contract.test.mjs` | 普通回归/前端 build、配置合同测试与预审通过后可配置的双引擎 ignored 测试门控；不得自动连接无审批数据库。 |
| `crates/rsetup-controller/tests/acceptance_runtime.rs` | 使用已接入生产的 auth、设备监听、BoardClient、NTP/DB/恢复门禁，作仅本机双进程分阶段网络测试；依赖身份、传输、运行计划。 |
| Playwright 候选（`apps/controller-web/tests/controller-flow.spec.ts`、`apps/controller-web/playwright.config.ts`，尚未获批） | 本范围不创建 config/spec、不安装、不自动选工具；后续若启用随工具审批另审（仅此一句）；当前执行范围以零依赖手工受控真实浏览器清单 + 既有 Vitest 分层替代，完整 SSE 撤权/reset 属后续索引；依赖 Web/后端 API 完成。 |
| `docs/testing/controller-v1-acceptance.md`、`scripts/check_controller_acceptance.py`、`scripts/tests/check_controller_acceptance_test.py` | 未来操作员填写真实执行命令、引擎版本、脱敏结果、缺失证据和每一 AT 项；检查脚本只验证证据字段存在/去敏，不预填结果或证明证据真伪。 |

## Task 0：唯一独立owner的生产路由装配

**Files:** Create `crates/rsetup-controller/src/production.rs`、`crates/rsetup-controller/tests/production_composition.rs`；Modify `src/lib.rs`（注册production）、`src/main.rs`（唯一生产装配调用者）。Identity/Runtime/Web只能各自交付子router，不再在各线导出顶层builder。此任务不改旧AppState结构，不复制共享gate，不新增生产fake开关。

**Consumes:** Identity `build_management_router(ManagementState)->Router<()>`、Runtime `build_tasks_router(HttpTasksState)->Router<()>`、独立status `build_device_status_router(DeviceStatusState)->Router<()>`、Web嵌入资源子router、既有`build_http_router(AppState)`（已含两个探活）。**Produces:** 本模块唯一`compose_component_routers`与`build_production_router(ProductionInputs)->Result<Router<()>,ControllerError>`；main经只读版本/数据guard、bootstrap、≤30s NTP关卡、持久锁恢复和恢复模式确认后才使用该builder/开启相应监听。

- [ ] **Step 0：可编译前置与真实源门。** 先仅交付纯组件merge函数与其测试；四线状态/DTO/子router尚缺时不导入其未定义类型。后续typed装配块只有在上游全部可编译后追加；完整production输入必须全部就位，缺SQLx网关、typed缓存/probe/sink或任务API即BLOCKED，不用Option/默认源挂部分Task4。codec/专家门未解除时raw继续fail-closed，不能以typedrouter合并宣称真实wire就绪。
- [ ] **Step 1：纯装配行为RED。** 在production.rs定义完整签名但仅返回auth的桩：

```rust
pub fn compose_component_routers(
    auth: axum::Router<()>, _management: axum::Router<()>,
    _tasks: axum::Router<()>, _status: axum::Router<()>, _web: axum::Router<()>,
) -> axum::Router<()> { auth }
```

下面测试文件全部定义齐全，synthetic组件只验证合并拓扑，不证明认证/SQL/资源内容或生产来源。它们不是生产CLI的fake入口：

```rust
// tests/production_composition.rs
use axum::{Router, body::{Body, to_bytes}, http::{Request, StatusCode}, routing::get};
use tower::ServiceExt;
use rsetup_controller::production::compose_component_routers;

fn component(path: &str, marker: &'static str) -> Router<()> {
    Router::new().route(path, get(move || async move { marker }))
}
fn composed() -> Router<()> {
    let auth = component("/healthz", "auth-health")
        .merge(component("/readyz", "auth-ready"));
    compose_component_routers(
        auth, component("/api/v1/users", "management"),
        component("/api/v1/tasks", "tasks"),
        component("/api/v1/devices/test/status", "status"),
        component("/", "web"),
    )
}
#[tokio::test]
async fn every_component_is_reachable_without_duplicate_probes() {
    let router = composed();
    for (path, marker) in [("/healthz", "auth-health"), ("/readyz", "auth-ready"),
        ("/api/v1/users", "management"), ("/api/v1/tasks", "tasks"),
        ("/api/v1/devices/test/status", "status"), ("/", "web")] {
        let response = router.clone().oneshot(Request::builder().uri(path).body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "component {path}");
        let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(bytes.as_ref(), marker.as_bytes(), "component {path}");
    }
}
```

未来设计根命令：`CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller --test production_composition every_component_is_reachable_without_duplicate_probes -- --exact`，恰好1方法，RED在首management组件404而非200失败，auth两个probe此前可过；不虚称每个分支都已跑到失败。

- [ ] **Step 2：最小GREEN合并。** 不再额外注册探活，路径集合归各线，重复业务路径应在审查和组件注册测试中拒绝，不靠覆盖隐藏重复：

```rust
pub fn compose_component_routers(
    auth: axum::Router<()>, management: axum::Router<()>,
    tasks: axum::Router<()>, status: axum::Router<()>, web: axum::Router<()>,
) -> axum::Router<()> {
    auth.merge(management).merge(tasks).merge(status).merge(web)
}
```

- [ ] **Step 3：上游就位后的typed生产装配。** 此块不是Step0可提前复制的类型桩；导出名由唯一集成owner维护，Identity Task13不再拥有lib/main。required字段不允许部分Task4缺省：

```rust
// production.rs；仅上游定义/DTO/真实adapter已交付后追加。
use std::sync::Arc;
use crate::{AppState, ControllerError};
use crate::api::{ManagementState, build_management_router};
use crate::http_tasks::{HttpTasksState, build_tasks_router};
use crate::http_device_status::{DeviceStatusState, build_device_status_router};

pub struct ProductionInputs {
    pub auth: AppState,
    pub management: ManagementState,
    pub tasks: HttpTasksState,
    pub status: DeviceStatusState,
    pub web: axum::Router<()>,
}

pub fn build_production_router(inputs: ProductionInputs) -> Result<axum::Router<()>, ControllerError> {
    if !Arc::ptr_eq(&inputs.auth.auth, &inputs.management.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.tasks.auth)
        || !Arc::ptr_eq(&inputs.auth.auth, &inputs.status.auth)
    {
        return Err(ControllerError::Config("production auth context mismatch".into()));
    }
    Ok(compose_component_routers(
        crate::http_auth::build_http_router(inputs.auth),
        build_management_router(inputs.management),
        build_tasks_router(inputs.tasks),
        build_device_status_router(inputs.status), inputs.web,
    ))
}
```

main唯一构造点从同一DbPool创建真实SqlxIdentityRepository/LiveAuth、Identity生产网关及AuthzService、Runtime生产TaskApi/恢复控制器/缓存、Transport真实typed设备缓存通知适配器、真实嵌入资源；所有状态clone同一Arc<HttpAuthState>共享session/CSRF/限流上下文。不得在main中构造测试网关、connect_lazy占位池、固定offline/capabilities或authz epoch0。上游真实adapter未完整交付时本Task保持未完成，不以compose synthetic测试作真装配证明。

- [ ] **Step 4：分层验证。** 重跑上方1方法验证纯拓扑GREEN；再跑Identity26管理与Runtime9业务实际gateway spy矩阵、真实资源web_assets、条件性双引擎/同源smoke。完整生产builder的auth指针不一致负例与真实数据来源验收必须随上游fixture交付后落实，未执行时记未验证，不能从合并成功推断HTTP安全或SQL来源。release缺dist构建拒绝独立测试，不当本组件行为RED。
- [ ] **Step 5：条件提交。** 仅暂存本Task的production/lib/main与production_composition测试，中文消息“feat(controller): 统一生产路由装配与共享认证上下文”；先凭据/范围审查，不自动commit，不push。全部Task4门过前不合dev-aghost。

## Task 1：普通 CI 与真实双引擎测试分轨

**Files:** Modify `Makefile`, `.github/workflows/ci.yml`; Test `crates/rsetup-controller/tests/{mysql_identity,tidb_identity}.rs` 以及新增的 auth/session/runtime 真库测试文件；Read 上游真实 DB fixture 与迁移关卡。

**Interfaces:** Consumes 各专题已审的可运行测试目标与 `CONTROLLER_TEST_DATABASE_URL` 门控；Produces `make controller-test-offline`（仅 Cargo/Vitest/TS/build、既有回归）和**操作员显式运行**的单引擎 ignored 测试入口；CI 缺单引擎 URL 时报告未验证，提供且失败/零用例时失败。不能在普通 `make test` 里隐式初始化库。

- [ ] **Step 1 RED：** 在已具备测试工具的离线 CI fixture 中添加以下配置合同（`scripts/tests/controller-ci-contract.test.mjs`）；它检查实际 `Makefile` 与 CI 文本，初次 RED 应为现有文件缺目标/缺前端 build，不以 npm 不存在冒充。先只钉住可验证的离线命令和先后顺序，DB 作业另增加对显式 URL、`--ignored`、实际用例数非零的条件合同；不 mock 跳过执行。

```js
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import assert from 'node:assert/strict';
const make = readFileSync('Makefile', 'utf8');
const ci = readFileSync('.github/workflows/ci.yml', 'utf8');
test('controller offline target and CI build frontend before Rust assets', () => {
  const target = make.split(/^controller-test-offline:/m)[1]?.split(/^\S[^\n]*:/m)[0];
  assert.ok(target, 'missing controller-test-offline target');
  assert.match(target, /apps\/controller-web/);
  for (const step of ['test -- --run', 'run typecheck', 'run build']) assert.ok(target.includes(step));
  assert.ok(target.indexOf('run build') < target.indexOf('--test web_assets'));
  assert.match(ci, /apps\/controller-web/);
  assert.match(ci, /--test web_assets/);
});
```

- [ ] **Step 2 确认 RED：** `node --test ui/*.test.mjs` 仍须通过；运行新增 `node --test scripts/tests/controller-ci-contract.test.mjs`（测试文件与本任务同建），记录配置断言失败。不得在此时运行任何 DB 用例。
- [ ] **Step 3 GREEN：** 在 `Makefile` 新增明确 `controller-test-offline` 目标，只有 Web 专题已交付 `web_assets` 测试后才接线：

```make
.PHONY: controller-test-offline
controller-test-offline:
	npm --prefix apps/controller-web test -- --run
	npm --prefix apps/controller-web run typecheck
	npm --prefix apps/controller-web run build
	cargo test -p rsetup-controller --test web_assets --locked
	$(MAKE) test
```

CI 构建顺序为 `npm ci`（已锁版本，按构建机策略）→ `npm test -- --run` → `npm run typecheck` → `npm run build` → Rust 路由测试，保留既有 `make test`；为 MySQL 与 TiDB 配置互不复用的、显式受控作业与非零用例断言。DB URL 不存在时只标“未验证”且普通 CI 可绿，URL 存在而实际用例零或失败时作业失败；迁移 fixture 分两类：首次创建类只允许已确认空库；版本升级类允许经只读验证的准确受支持 identity v3 结构/数据基线（已有表/合法数据），拒绝未知/污染/不符版本，不以非空拒绝合法 v3→v4；两类均须实际目标隔离可丢弃、实际权限、执行方真实私密备份独立核验、排他停写与显式迁移门，普通启动绝不迁移；不削弱 `DROP DATABASE` 禁令、不复制/回显凭据、不伪 ACK，未满足保持未验证/BLOCKED。任何可执行 URL/凭据不写入脚本、文档、日志或提交。
- [ ] **Step 4 GREEN 验证：** 重跑新增 CI 合同测试、`make controller-test-offline`、`make test`、`cargo fmt --all -- --check` 与 `cargo clippy --workspace --all-targets -- -D warnings`；真库命令**仅在操作员核验目标/权限/实际备份/迁移关卡后**按每引擎分别执行并记录实际用例数与版本，未授权时不运行且记未验证。普通离线绿不等于 DB 绿。
- [ ] **Step 5 条件提交：** 仅通过评审后显式暂存 `Makefile`、`.github/workflows/ci.yml`、`scripts/tests/controller-ci-contract.test.mjs` 及本任务经核准的测试文件；检查 staged diff 不含秘密，再独立提交。

## Task 2：进程边界与启动/恢复联合 smoke

**Files:** Create `crates/rsetup-controller/tests/acceptance_runtime.rs`; Modify 未来可注入进程启动 fixture（若已存在则复用，不重写生产路由/板端实现）。

**Interfaces:** Consumes 身份计划完成的生产 auth router 和 v3 一致性检查、传输计划完成的设备监听/板端入口、运行计划完成的 NTP gate / v4 检查 / BoardClient / recovery；Produces 阶段证据：独立进程启动→受控登录→待审准入/批准→双流 probe→只读 RPC→恢复锁检查。未满足任一依赖时不构造伪“通过”的 smoke。

- [ ] **Step 1 RED：** 区分门禁纯测试（离线装配）与完整真实 DB 双进程 smoke 行为断言：
  1. **门禁纯测试（无库）：** 预先将内存中的 DB/时间/恢复 gate 挂起时，管理 `/readyz` 不得为 200、设备业务不得可用；明确放行 gate 后，`/readyz` 正常返回。
  2. **完整真实 DB 双进程 smoke（`#[ignore]`）：** 包含真实 DB 初始化、登录写入、待审准入/批准写事务、连接独立板端进程，断言握手与审批、Control/Data 各自有效 pong、`capabilities/status/clock/task.get` 网络往返均成功。`RebootOs` 必须由测试板端进程注入安全替身，并断言宿主机 reboot 调用为零；掉线/中控重启后有 dispatching 意图但结果未知时不发第二次 execute。失败应是缺生产接线/真实门禁错误，而非仅 mock 函数调用次数。
- [ ] **Step 2 确认 RED：** 门禁纯测试与完整真实 DB smoke 必须严格分轨执行：
  1. 门禁纯测试通过普通非忽略命令运行：`cargo test -p rsetup-controller --test acceptance_runtime gate_blocks_business_before_readiness -- --exact`。
  2. 真实 DB 双进程 smoke 必须标记为 `#[ignore]`，**严禁使用原命令普通 cargo 跑网络 DB**，必须通过显式单 case `--ignored --exact` 串行授权执行：`cargo test -p rsetup-controller --test acceptance_runtime two_process_real_db_smoke -- --ignored --exact --test-threads=1`。
  若缺可注入 fixture、DB 审批、真实隔离库授权、板端二进制或独立专家决策，标 BLOCKED 并不把编译/连接错误写成 RED；严禁注入“只读 DB fake”冒充初始化、登录写入与准入写事务。
- [ ] **Step 3 GREEN：** 接线 production `main` 同源 router 与**独立设备监听**；管理监听使用 `into_make_service_with_connect_info::<SocketAddr>()` 提供可信连接来源，合法 Host/Origin/JSON 登录必须真正到达认证服务，不能仅以“非404”判断接线成功。普通启动对 schema 仅做只读验证受支持且结构/数据闭合的版本；v4 启动必须进行结构与数据闭合检查，且公共写 guard、身份数据扫描与 auth/admission 消费者须一起升级并回归，不仅比对版本数字，绝不把原来只认 v3 的 guard 留在 v4 写路径中。NTP≤30s fallback 可与 DB 前置并行；持久锁/未决任务恢复完成、恢复模式明确后才开放相应业务，不自动迁移。使用真实两进程 TCP（本机隔离网络）；门禁纯测试仅验证装配与内存门禁阻断；完整真实 DB smoke 必须串行连接已授权隔离真实 DB，绝对不能注入“只读 DB fake”冒充初始化、登录写入、准入提交或持久任务恢复的完整事务链路，生产入口与集成测试绝不增加 fake 仓储开关。旧进程/旧流代际事件无效、审批决定提交后再通知并重读持久真相、已提交意图后网络 execute、未知不重发。
- [ ] **Step 4 GREEN 验证：** 门禁纯测试在普通离线环境下验证通过；完整真实 DB smoke 仅在操作员核验目标、权限、实际备份及隔离专用库后，按单 case 显式串行命令 `cargo test -p rsetup-controller --test acceptance_runtime two_process_real_db_smoke -- --ignored --exact --test-threads=1` 执行；连同传输计划双端负例、运行计划 `controller_recovery` 的两引擎测试分别验证；用阶段日志列明物理事实与伪件，不以库内 loopback 或 tower oneshot 冒充独立进程网络。安全门未解除或 DB 环境不满足则本任务维持 BLOCKED 未完成。
- [ ] **Step 5 条件提交：** 路由接线和测试按专题归属分小提交；本任务只暂存自身真实 smoke 与 fixture 文件，不混入原有数据库/协议大改动。

## Task 3：浏览器任务流程与 a11y 分层验收（手工受控真实浏览器 + 既有 Vitest）

**Files:** Create 零依赖手工受控真实浏览器清单（runbook，随验收报告模板存放），预期路径按既有 Web 计划为 `apps/controller-web/tests/browser/README.md`（未来 create 引用，**本范围不实际创建**）； Test 既有 Vitest（jsdom 组件/fixture 交互层）与 `crates/rsetup-controller/tests/web_assets.rs`（该目标当前实际尚不存在，为 Web/独立集成 owner 未来创建并交付的测试目标，见下方前置）； Modify `Makefile`、`.github/workflows/ci.yml`（仅补本项独立门）。Playwright 的 `apps/controller-web/tests/controller-flow.spec.ts`、`apps/controller-web/playwright.config.ts` 标为待审候选，**本范围不创建**（获批前不安装、不自动选工具，不暗示已批准）。

**Interfaces:** Consumes 后端 `GET/POST /api/v1` 的生产 cookie/Host/Origin/CSRF 与强制改密门禁、Web 的 `App` 页面/危险确认与嵌入资源路由（完整 SSE 属后续索引，不作为本项前置）；Produces 不读取真实秘密的分层浏览器证据：fixture/mock 交互层（既有 Vitest）与真实同源生产端到端层（手工受控真实浏览器 + 单二进制），撤权后旧投影经重新 GET/epoch 重验不泄漏，未知任务仍显示未知。

**前置（证据分层，全部满足前 Step 2 的行为 RED 不计有效；本项不自行实现 asset embedding 或 root 装配，先由既有 Web/独立集成 owner 交付）：**
- **preflight-checklist（任一条不满足即停止本项，一律记 BLOCKED/未验证，不算 RED，不伪 fixture、不以实际 test 数计通过）：**
  1. `crates/rsetup-controller/tests/web_assets.rs` 测试目标已交付（未来待创建目标）且可离线 locked 编译；
  2. 真实 Vite `dist` 已存在（含 `index.html` 与实际哈希指纹的 JS/CSS 产物，非空目录）；
  3. 有效 release 单二进制（嵌入资源）已就绪并可脱离目录启动。
**证据分层：** 已批准工具和依赖下才对真实 router/page 行为写断言并实际跑行为 RED；正常行为 RED 使用真实 `dist` 与有效 binary，只失败于 HTTP/MIME/cache/SPA 或 DOM 等机器断言，目标非 0 退出且编译，build 成功不等业务 GREEN。缺 target/`dist`、无法编译或 binary 不可启动一律 BLOCKED/未验证，不能算 RED，不能用空 assets fake 满足；“release 缺 `dist` 必须拒绝构建”的负例另在工作区内可丢弃隔离目录/目标中单独验证，不移删主 `dist`/缓存，不把编译失败当功能 RED。
**继承门（不重述）：** 完整四先批准才 TDD、真实同源与 fake 分层、codec 前置、双库门、禁 host reboot；本项不安装 Playwright 等候选工具。

- [ ] **Step 1 RED：** 编写分层验收清单（不依赖未获批浏览器工具），并落实下列规范：
  - **A 层 · fixture/mock 交互（既有 Vitest/jsdom）：** 明确 fixture 前置、幂等性、危险确认与 a11y 组件契约的行为测试；
  - **B 层 · 真实同源生产端到端（零依赖手工受控真实浏览器 runbook）：** 单二进制脱离目录启动、同源 API、任务流程（预览→确认→详情→取消/time-review/unknown 释放）与键盘/a11y 检查；
  - **后续索引 · 完整 SSE 事件流**（五种命名事件/背压/reset/重连）待获批浏览器工具或受控流服务后执行，不作为本 Task 4 门。
  规范如下：
  1. **fixture 前置（防认证畸形）：** 若集成测试没有受控会话列表接口，必须明确完整合法的 fixture 前置，包括 `/api/v1/auth/me`（包含合法 `user.id`、`revision`、`active: true`、`must_change_password: false`、`csrf_token`、`authz_epoch`）、`/api/v1/auth/sessions`（有效会话列表信封 `{ data: { items: [...], next_cursor: null }, request_id }`）；auth/me、sessions 为合法 fixture 前置可保留但不强制，**不强制 SSE/`/api/v1/events` 或 `EventSource` 前置**（完整 SSE 规范移入文末后续索引独立段），**严禁因 session 列表未 mock 或认证畸形导致页面在初始化阶段直接重定向或崩溃**；初始 RED 必须是因为业务组件或预期功能未实现，而非前置认证畸形伪 RED。
  2. **危险操作幂等性（Idempotency）规范：**
     - 用户在确认对话框中触发危险操作（如设备重启）时，客户端生成单次确认 UUID；同一次确认流程中该 UUID 必须在客户端生命周期中保留，用户重试时**绝不更换 key**；
     - 幂等 key 必须通过 HTTP 请求头 `Idempotency-Key: <UUID>` 发送，**严禁放入 JSON body**；
     - 在普通远程 HTTP 非安全上下文（insecure context）中，现代浏览器禁用 `crypto.randomUUID`；前端必须实现并测试基于 `crypto.getRandomValues` 构造的 UUIDv4 方案；
     - **缺能力禁操作：** 若运行环境连 `crypto.getRandomValues` 亦不可用（缺少密码学安全随机源），**严禁弱随机（如 `Math.random`）fallback**，必须直接禁用危险操作按钮并提示缺少安全随机源；测试须分别断言有能力时复用 key 与无能力时禁用操作。
  3. **真实静态资源与脱离目录硬约束：**
     - release 编译硬门禁：构建脚本中若缺少前端 `dist` 产物，release 构建必须直接报错失败，严禁打包空静态目录；
     - 资源内容与缓存校验：`crates/rsetup-controller/tests/web_assets.rs` 必须实际读取并解析构建生成的 `dist/index.html`，提取出实际包含哈希指纹的真实 JS/CSS 文件名（如 `assets/index-[hash].js`），向真实路由请求断言其内容非空、MIME 类型严格为 `text/javascript` / `text/css`、以及长期 `Cache-Control: public, max-age=31536000, immutable`（入口 `index.html` 则为 `no-cache`）；
     - 脱离目录运行：测试二进制必须在完全脱离项目源码和前端 dist 目录的临时独立工作目录中启动运行，验证单二进制能够独立正常提供页面与 API，且 `/api/v1/missing` 与 `/assets/missing.js` 绝不回退为 HTML 200，严禁以空 HTML 或 assets 恒 404 伪装测试通过。

（Playwright 待审候选示例暂移除，不新增依赖或候选 dead code；后续若获批该工具，其 spec/fixture 示例随工具审批另审。）

- [ ] **Step 2 确认 RED：** 完整 gate 顺序与实际命令：先在 `apps/controller-web` 执行既有 `npm test -- --run`（A 层）、`npm run typecheck`、`npm run build`（既有脚本，不新增/安装候选工具）；前置满足后执行 `CARGO_HOME=/home/aghost/workspace/rsetup-next/.cargo-home CARGO_TARGET_DIR="$PWD/.cargo-target" cargo test --offline --locked -p rsetup-controller --test web_assets`（在设计树根目录运行（$PWD 为该设计树）：主工作区 cache 可复用、设计树私有 target 目录隔离、命令不含秘密路径，offline locked），确认其编译通过、非 0 退出且只失败于 HTTP/MIME/cache/SPA 或 DOM 等机器断言；确认失败因任务页面组件缺失或幂等/撤权行为未就绪，并非认证畸形或未 build。使用真实 `dist` 与有效 binary 按 B 层清单脱离目录启动手工核对；缺 target/`dist`、无法编译或 binary 不可启动一律记 BLOCKED/未验证，不算 RED。将失败的 HTTP/DOM/静态资源条件分层单独记录；build 成功不等业务 GREEN。
- [ ] **Step 3 GREEN：** 完成授权状态变更时丢弃过期投影（A 层以代际/重新 GET 验证，B 层以真实页面行为验证）；强制改密始终不得加载业务页面；危险确认通过 `Idempotency-Key` 头传递且重试保留同一次 UUID（远程 HTTP 下使用 `getRandomValues`，缺能力时禁用按钮）；任务流程（预览固定清单→确认→详情→取消/time-review/unknown 释放）在 B 层真实同源生产端到端跑通，`unknown`/`202` 仍不能误报成功；发布二进制在脱离源码临时目录中独立提供已嵌入资源，精确 MIME 与长期缓存，静态资源缓存与 SPA/API 边界严格隔离。测试使用合成内容，不存 cookie/token 明文或真实 DB 值到截图、日志、fixture。独立审核语言切换后键盘焦点与 aria、320px/对比度/减少动效。
- [ ] **Step 4 GREEN 验证：** 分层核对：A 层 `npm test -- --run`、`npm run typecheck`、`npm run build` 与受影响旧 UI 用例全绿；B 层手工受控真实浏览器清单逐项通过（含一轮真实受控同源 HTTP 前后端交互），Rust `web_assets`（解析实际构建 `index.html` 中的哈希 JS/CSS）通过，脱离目录独立运行二进制检查；A 层通过只称 fixture/mock 层验证，**不批准** B 层真实同源生产端到端，B 层未执行时只称“未验证”。
- [ ] **Step 5 条件提交：** 只暂存本项测试/工具和受审 CI 接线；先完成脱敏检查，依赖版本变化须一起提交 lock 并另审。

## Task 4：Task 4 收敛条件性验收 runbook（完整 V1 移入后续索引）

> **本轮 scope narrowing（2026-10-16，文档状态：尚未批准）：** 唯一目标是**范围收敛**——完整 V1 的 AT-01..18 全量、完整 SSE 与容量/发布压测从**本次执行范围**明确移入「非当前 Task 4 后续索引」（见文末），**不删除未来要求、不称它们已 PASS、不顺带实施**；本 Task 4 收敛为**明确条件性验收 runbook**，不是完整 V1 验收框架重写，也不编造缺失 fixtures。本 Task 4 真正必要的保留项：同源生产装配（`lib/main` 唯一独立集成 owner，四线只提供子 router）、cookie/Host/Origin/CSRF/强制改密九 Runtime 业务及 26 管理、Task 4 固定预览/幂等/授权全集、Main 四态/outcome/counts/TimeEvidence/`actions.can_cancel`、单调缓存 status 独立 Runtime 路由、intent 最多一次 execute/恢复只 query/迟到证据不删新 generation、板端 B04 故障与 typed→真实获批 wire、浏览器任务流程与 a11y/单二进制嵌入资源。

**Files:** Create `docs/testing/controller-v1-acceptance.md`, `scripts/check_controller_acceptance.py`, `scripts/tests/check_controller_acceptance_test.py`; Read 本计划、四线四份执行计划与接口清单、00 总目录及 01–06 对应规格；不写伪测试数据。

**Interfaces:** Consumes 每项实际命令/版本/测试报告及独立专家审查原件；Produces 当前 12 项固定 ID `T4-01..T4-12` 验收账本（映射 runbook 表 1–12，不改变验收要求/门禁）与后续索引 `AT-01..AT-18` 的 `PASS / FAIL / 未验证 / BLOCKED` 状态、执行时间、执行层级、命令或步骤、去敏证据路径与缺口/持续风险。行合同固定 `| ID | status | executed_at | layer | command_or_steps | evidence |`：T4 12 行与 AT 18 行各自唯一且全具备（当前 12 行独立于后续 AT-01..18，不只用 AT 占位代替），PASS 要求 executed_at/layer/command_or_steps/evidence 后 4 字段非空，未运行默认“未验证”，时间/层级/命令/证据皆去敏；提供标准库 CLI 入口（`main(argv)` 支持测试与 `__main__`），校验指定 UTF-8 报告；参数不符、文件不存在或 I/O 解码异常固定去敏非零退出且不回显路径与异常；错误信息只输出固定字段与枚举，不回显未知/恶意 ID 或原行内容；成功返回 0，校验失败返回非零；checker 只校验格式/完整性（ID 集合、重复、越界、合法状态、PASS 证据、`Spec: 06 §6` 引用），不能证明外部证据真实；缺 report/scripts 只是 BLOCKED 不是 RED。

- [ ] **Step 1 RED：** 先给 `scripts/check_controller_acceptance.py` 可导入/可编译 RED 前置：包含完整 `_cells(line)` parser（两份实现一致，仅剥单个外框、保留额外空列）、完整确定性生成函数 `default_report()`（当前 12 行固定 ID `T4-01..T4-12` 与后续 18 行 `AT-01..AT-18` 全部默认“未验证”空记录，含 `Spec: 06 §6` 引用与固定行合同）、完整标准库 CLI `main(argv)`、`if __name__ == '__main__': raise SystemExit(main())` 入口与空桩 `validate(text: str) -> list[str]`（返回 `[]`）；再写 `scripts/tests/check_controller_acceptance_test.py` 纯测试（包含纯函数断言与 subprocess CLI 正反例测试）。行合同固定 `| ID | status | executed_at | layer | command_or_steps | evidence |`，`PASS / FAIL / 未验证 / BLOCKED` 为 4 合法状态；PASS 要求后 4 字段（executed_at/layer/command_or_steps/evidence）非空，未运行默认“未验证”，时间/层级/命令/证据皆去敏；checker 只校验格式/完整性（缺任一行/重复/越界/非法状态/PASS 缺证据/缺 `Spec: 06 §6` 引用均报固定错误，不回显恶意行/ID），不能证明外部证据为真。前置具备完整 CLI 使 invalid 报告 CLI 目标断言因返回 0 失败（而非脚本缺失），正例退出 0；测试不造 PASS 写进真实模板，坏样例只出现在单测 synthetic 文本中。

```python
# scripts/check_controller_acceptance.py（RED 前置：default_report 与 CLI 完整 + validate 空桩）
import os
import re
import sys
from collections import Counter

HEADER = "| ID | status | executed_at | layer | command_or_steps | evidence |"
T4_IDS = [f"T4-{i:02d}" for i in range(1, 13)]
AT_IDS = [f"AT-{i:02d}" for i in range(1, 19)]
ALL_IDS = T4_IDS + AT_IDS
STATUSES = {"PASS", "FAIL", "未验证", "BLOCKED"}
PASS_FIELDS = ("executed_at", "layer", "command_or_steps", "evidence")
ROW_ID_RE = re.compile(r"^(T4|AT)-\d{2}$")
PREFIX_RE = re.compile(r"^(T4|AT)")


def default_report() -> str:
    lines = ["# Controller V1 集成验收报告", "Spec: 06 §6", "", HEADER,
             "| --- | --- | --- | --- | --- | --- |"]
    for rid in ALL_IDS:
        lines.append(f"| {rid} | 未验证 | | | | |")
    return "\n".join(lines) + "\n"


def _cells(line: str) -> list[str]:
    raw = line.strip()
    if raw.startswith("|"):
        raw = raw[1:]
    if raw.endswith("|"):
        raw = raw[:-1]
    return [c.strip() for c in raw.split("|")]


def validate(text: str) -> list[str]:
    return []


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        sys.stderr.write("invalid arguments\n")
        return 2
    try:
        with open(args[0], "r", encoding="utf-8") as f:
            text = f.read()
    except Exception:
        sys.stderr.write("cannot read report\n")
        return 1
    errors = validate(text)
    if errors:
        for err in errors:
            sys.stderr.write(f"{err}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

```python
# scripts/tests/check_controller_acceptance_test.py（纯测试；坏样例为单测内 synthetic）
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from contextlib import contextmanager
from pathlib import Path

from scripts.check_controller_acceptance import _cells, default_report, validate


def _row(rid: str, status: str = "未验证", rest: tuple = ("", "", "", "")) -> str:
    return f"| {rid} | {status} | {rest[0]} | {rest[1]} | {rest[2]} | {rest[3]} |"


def _drop(text: str, rid: str) -> str:
    return "\n".join(l for l in text.splitlines() if not l.startswith(f"| {rid} |")) + "\n"


SCRIPT_PATH = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "check_controller_acceptance.py"))


@contextmanager
def _cli_workspace_dir():
    repo_root = Path(__file__).resolve().parents[2]
    if repo_root != Path.cwd().resolve() or not (repo_root / "AGENTS.md").is_file() or not (repo_root / "scripts").is_dir():
        raise RuntimeError("acceptance test requires design tree root")
    prefix = ".controller-checker-"
    # mkdtemp 无析构/退出时自动删除：清理校验失败后也不能被 finalizer 绕过。
    raw_path = Path(tempfile.mkdtemp(dir=str(repo_root), prefix=prefix))
    owned_path = raw_path.resolve(strict=True)
    created = raw_path.lstat()
    owner_identity = (created.st_dev, created.st_ino)
    try:
        yield owned_path
    finally:
        try:
            current = raw_path.resolve(strict=True)
            info = raw_path.lstat()
            valid = (
                current == owned_path
                and current.parent == repo_root
                and raw_path.name.startswith(prefix)
                and not raw_path.is_symlink()
                and raw_path.is_dir()
                and (info.st_dev, info.st_ino) == owner_identity
            )
        except OSError:
            raise RuntimeError("refusing unverified fixture cleanup") from None
        if not valid:
            raise RuntimeError("refusing unverified fixture cleanup")
        # 只删除上方已重新核验绝对路径/parent/本例inode身份的独占目录。
        shutil.rmtree(raw_path)


class AcceptanceReportContractTest(unittest.TestCase):
    """行合同：| ID | status | executed_at | layer | command_or_steps | evidence |。"""

    def test_default_all_unverified_report_passes(self):
        self.assertEqual(validate(default_report()), [])

    def test_missing_t4_row_rejected(self):
        self.assertIn("T4-05: missing", validate(_drop(default_report(), "T4-05")))

    def test_duplicate_t4_row_rejected(self):
        self.assertIn("T4-03: duplicate row", validate(default_report() + _row("T4-03") + "\n"))

    def test_t4_pass_without_evidence_rejected(self):
        text = _drop(default_report(), "T4-01") + _row("T4-01", "PASS") + "\n"
        self.assertIn("T4-01: PASS missing executed_at", validate(text))

    def test_counter_examples_data_driven(self):
        base = default_report()
        cases = [
            ("malformed id", _drop(base, "T4-01") + _row("T4-1") + "\n"),
            ("malformed id", _drop(base, "T4-01") + _row("T4-001") + "\n"),
            ("malformed id", _drop(base, "T4-01") + _row("T4-xx") + "\n"),
            ("out of range id", _drop(base, "AT-18") + _row("AT-19") + "\n"),
            ("malformed id", _drop(base, "AT-01") + _row("AT-001") + "\n"),
            ("T4-01: illegal status", base.replace("| T4-01 | 未验证 |", "| T4-01 | DONE |")),
            ("T4-01: malformed row", base.replace("| T4-01 | 未验证 | | | | |", "| T4-01 | 未验证 | | |")),
            ("T4-02: duplicate row", base + _row("T4-02") + "\n"),
            ("Spec: 06 §6 reference missing",
             "\n".join(l for l in base.splitlines() if not l.startswith("Spec:")) + "\n"),
            ("T4-01: PASS missing executed_at",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("", "tower", "cmd", "ref")) + "\n"),
            ("T4-01: PASS missing layer",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "", "cmd", "ref")) + "\n"),
            ("T4-01: PASS missing command_or_steps",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "tower", "", "ref")) + "\n"),
            ("T4-01: PASS missing evidence",
             _drop(base, "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "tower", "cmd", "")) + "\n"),
        ]
        for expected, text in cases:
            with self.subTest(expected=expected):
                self.assertIn(expected, validate(text))

    def test_trailing_extra_empty_column_rejected(self):
        base = default_report()
        row = "| T4-01 | PASS | 2026-10-16 | tower | cmd | ref ||"
        text = _drop(base, "T4-01") + row + "\n"
        self.assertEqual(len(_cells(row)), 7)
        self.assertIn("T4-01: malformed row", validate(text))

    def test_complete_pass_row_pure_success(self):
        # 仅合成测试文本；不把 PASS 写入真实验收报告。
        row = _row("T4-01", "PASS", ("2026-10-16", "tower", "synthetic-command", "synthetic-ref"))
        self.assertEqual(validate(_drop(default_report(), "T4-01") + row + "\n"), [])

    def test_cli_complete_pass_row_success(self):
        text = _drop(default_report(), "T4-01") + _row("T4-01", "PASS", ("2026-10-16", "tower", "synthetic-command", "synthetic-ref")) + "\n"
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            path.write_text(text, encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (0, "", ""))

    def test_cli_pass_missing_each_required_field(self):
        fields = ("executed_at", "layer", "command_or_steps", "evidence")
        for index, field in enumerate(fields):
            with self.subTest(field=field), _cli_workspace_dir() as d:
                values = ["2026-10-16", "tower", "synthetic-command", "synthetic-ref"]
                values[index] = ""
                text = _drop(default_report(), "T4-01") + _row("T4-01", "PASS", tuple(values)) + "\n"
                expected = f"T4-01: PASS missing {field}"
                self.assertEqual(validate(text), [expected])
                path = d / "SYNTHETIC_PATH_SENTINEL.md"
                path.write_text(text, encoding="utf-8")
                res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
                self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", expected + "\n"))

    def test_extra_invalid_id_keeps_all_required_rows(self):
        cases = [("T4-001", "malformed id"), ("T4-13", "out of range id"),
                 ("AT-19", "out of range id"), ("T4-SYNTHETIC_SECRET_SENTINEL", "malformed id")]
        for rid, expected in cases:
            with self.subTest(rid=rid), _cli_workspace_dir() as d:
                # 原30行全保留，只追加恶意/畸形/越界行；不能以缺行副作用假通过。
                text = default_report() + _row(rid) + "\n"
                self.assertEqual(validate(text), [expected])
                path = d / "SYNTHETIC_PATH_SENTINEL.md"
                path.write_text(text, encoding="utf-8")
                res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
                self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", expected + "\n"))
                self.assertNotIn("SYNTHETIC_SECRET_SENTINEL", res.stdout + res.stderr)
                self.assertNotIn(str(path), res.stdout + res.stderr)
                self.assertNotIn("Traceback", res.stdout + res.stderr)

    def test_known_id_wrong_column_duplicate_rejected(self):
        text = default_report() + "| T4-03 | 未验证 | |\n"
        expected = ["T4-03: malformed row", "T4-03: duplicate row"]
        self.assertEqual(validate(text), expected)
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            path.write_text(text, encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path)], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (1, "", "\n".join(expected) + "\n"))

    def test_cli_extra_arguments_exact_exit_two(self):
        with _cli_workspace_dir() as d:
            path = d / "SYNTHETIC_PATH_SENTINEL.md"
            self.assertFalse(path.exists())
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(path), "SYNTHETIC_ARGUMENT_SENTINEL"], capture_output=True, text=True)
            self.assertEqual((res.returncode, res.stdout, res.stderr), (2, "", "invalid arguments\n"))

    def test_cli_default_report_success(self):
        with _cli_workspace_dir() as d:
            report_path = d / "report.md"
            report_path.write_text(default_report(), encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(report_path)], capture_output=True, text=True)
            self.assertEqual(res.returncode, 0)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "")

    def test_cli_invalid_report_rejected(self):
        with _cli_workspace_dir() as d:
            report_path = d / "report.md"
            report_path.write_text(_drop(default_report(), "T4-01"), encoding="utf-8")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(report_path)], capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "T4-01: missing\n")

    def test_cli_missing_file_rejected(self):
        with _cli_workspace_dir() as d:
            missing_path = d / "missing.md"
            self.assertFalse(missing_path.exists())
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(missing_path)],
                                 capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "cannot read report\n")

    def test_cli_invalid_arguments_rejected(self):
        res = subprocess.run([sys.executable, SCRIPT_PATH], capture_output=True, text=True)
        self.assertEqual(res.returncode, 2)
        self.assertEqual(res.stdout, "")
        self.assertEqual(res.stderr, "invalid arguments\n")

    def test_cli_utf8_decode_error_rejected(self):
        with _cli_workspace_dir() as d:
            bin_path = d / "invalid_utf8.bin"
            bin_path.write_bytes(b"\xff\xfe\x00\x00")
            res = subprocess.run([sys.executable, SCRIPT_PATH, str(bin_path)], capture_output=True, text=True)
            self.assertEqual(res.returncode, 1)
            self.assertEqual(res.stdout, "")
            self.assertEqual(res.stderr, "cannot read report\n")
```
- [ ] **Step 2 确认 RED：** 先执行 `python3 -m unittest scripts.tests.check_controller_acceptance_test`，空桩 `validate` 恒返回 `[]`，函数级断言（如 `test_missing_t4_row_rejected` 等）与 CLI 目标断言（`test_cli_invalid_report_rejected` 因 validate 空桩返回 0 违反期望 1 及固定缺行 stderr 而失败；读取/参数负例已有 main 正确可过，不虚称全部 CLI 失败）必失败，命令实际退出非 0 即为 RED（报告校验器行为 RED 是格式合同，无 migration/业务行为 RED，不证明数据库/browser/wire 真实证据）；缺 `scripts/` 或报告文件只是环境未就绪（BLOCKED），不算 RED。
- [ ] **Step 3 GREEN：** 将 `scripts/check_controller_acceptance.py` 的 `validate` 空桩替换为以下完整最小实现（仅标准库 `os`/`re`/`sys`/`collections`，校验核心与 CLI 完整闭合，无新依赖，不留 TODO/未定义 parse helpers/“后续实现”；签名与字段列同 Step 1 测试）：

```python
# scripts/check_controller_acceptance.py（完整实现；default_report、CLI 与 Step 1 逐字一致）
"""报告格式/完整性校验器：只验账本格式合同，不证明外部证据真实。"""
import os
import re
import sys
from collections import Counter

HEADER = "| ID | status | executed_at | layer | command_or_steps | evidence |"
T4_IDS = [f"T4-{i:02d}" for i in range(1, 13)]
AT_IDS = [f"AT-{i:02d}" for i in range(1, 19)]
ALL_IDS = T4_IDS + AT_IDS
STATUSES = {"PASS", "FAIL", "未验证", "BLOCKED"}
PASS_FIELDS = ("executed_at", "layer", "command_or_steps", "evidence")
ROW_ID_RE = re.compile(r"^(T4|AT)-\d{2}$")
PREFIX_RE = re.compile(r"^(T4|AT)")


def default_report() -> str:
    lines = ["# Controller V1 集成验收报告", "Spec: 06 §6", "", HEADER,
             "| --- | --- | --- | --- | --- | --- |"]
    for rid in ALL_IDS:
        lines.append(f"| {rid} | 未验证 | | | | |")
    return "\n".join(lines) + "\n"


def _cells(line: str) -> list[str]:
    raw = line.strip()
    if raw.startswith("|"):
        raw = raw[1:]
    if raw.endswith("|"):
        raw = raw[:-1]
    return [c.strip() for c in raw.split("|")]


def validate(text: str) -> list[str]:
    errors: list[str] = []
    counts: Counter = Counter()
    for line in text.splitlines():
        if not line.lstrip().startswith("|"):
            continue
        cells = _cells(line)
        if not cells or not PREFIX_RE.match(cells[0]):
            continue  # 表头 | ID |... / 分隔 | --- |... / 其他表格行忽略
        rid = cells[0]
        if rid not in ALL_IDS:
            if ROW_ID_RE.match(rid):
                errors.append("out of range id")
            else:
                errors.append("malformed id")
            continue
        counts[rid] += 1
        if len(cells) != 6:
            errors.append(f"{rid}: malformed row")
            continue
        status, executed_at, layer, command, evidence = cells[1:]
        if status not in STATUSES:
            errors.append(f"{rid}: illegal status")
        if status == "PASS":
            for name, value in zip(PASS_FIELDS, (executed_at, layer, command, evidence)):
                if not value:
                    errors.append(f"{rid}: PASS missing {name}")
    for rid in ALL_IDS:
        if counts[rid] == 0:
            errors.append(f"{rid}: missing")
        elif counts[rid] > 1:
            errors.append(f"{rid}: duplicate row")
    if not re.search(r"Spec:\s*06\s*§6", text):
        errors.append("Spec: 06 §6 reference missing")
    return errors


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 1:
        sys.stderr.write("invalid arguments\n")
        return 2
    try:
        with open(args[0], "r", encoding="utf-8") as f:
            text = f.read()
    except Exception:
        sys.stderr.write("cannot read report\n")
        return 1
    errors = validate(text)
    if errors:
        for err in errors:
            sys.stderr.write(f"{err}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
```

执行 `python3 -m unittest scripts.tests.check_controller_acceptance_test` 17 个测试方法（原11+新增6；数据驱动子测试共21个：原13+PASS缺字段4+额外ID4，不把subTest当独立方法；CLI临时helper非测试，未来运行必须在设计根，清理只对重新核验解析路径/父级/创建时inode身份的自有目录，无自动finalizer绕过）全绿。当前 Task 4 收敛项由对应线责任人提供实际对应证据；只有严格核对后才将该项标 PASS。1024 已批准 + 1024 初次待审、20 管理会话、RTT≤100ms、稳定1h、99%≤15s 等容量/发布基准仅为 06 §7 **待审阅基准**，属**非当前 Task 4 后续索引**：本 Task 4 不以容量压测为通过条件；后续执行压测时须记录 CPU/RAM、DB 产品/版本、板端模拟规模、报文大小、网络条件、队列峰值、延迟分位和拒绝计数；无仪表与受控环境则未验证，不得宣传支持1024。

- [ ] **Step 4 验证：** 回归执行 `python3 -m unittest scripts.tests.check_controller_acceptance_test`（退出 0），再对真实报告模板执行 `python3 scripts/check_controller_acceptance.py docs/testing/controller-v1-acceptance.md`（真实报告保持未验证直到取得对应证据；checker 退出 0 仅证明格式/完整性）。当前报告 checker 通过**不自动**使任何 T4 项 PASS，AT-01..18 后续范围保持默认未验证，T4 全过也不推定四线 execution/真实门通过。手工与程序双重核对所有 PASS 行均有独立可追溯且去敏的证据；G0/G1/G2/G3/G4/G5 分开判定。**当前 Task 4 门禁**（同源装配、9 条 Runtime 业务 + 26 管理、预览/幂等/授权、任务投影与恢复、B04、获批 wire、浏览器任务流程/a11y、单二进制嵌入、真库门禁项）未齐则 Task 4 维持未完成；**完整 V1 发布门禁**（独立安全审查、双端负例/向量/互操作、真实 DB 迁移和备份恢复、板端持久 journal 证据）任何一项缺失，发布栏维持 BLOCKED，即使离线回归全部通过；12 项全部安全/真实证据且最终全部门齐备才 merge，no push。
- [ ] **Step 5 条件提交：** 只暂存无真实秘密的报告模板/检查脚本；实际验收报告须由执行方审查脱敏后再提交，绝不预造 PASS 或签字。

### 当前 Task 4 收敛验收 runbook（条件性；全部默认未验证）

证据分层：① 纯逻辑/组件 RED→GREEN；② 进程内 HTTP 权限矩阵、文件持久化故障注入、模拟网络/时钟；③ 双引擎真实 SQLx 与真实同源双进程 smoke（真库门禁项逐引擎串行单 case）；④ 专家书面决议 + 用户复审 + 获批 codec 后的真实 wire/浏览器端到端。已存在/计划接口/未知依赖逐项标注；未运行默认“未验证”，实质性专家/真实目标门禁保持明确 BLOCKED。

| ID | 收敛验收项 | 接口状态 | 证据要求与门禁 |
| --- | --- | --- | --- |
| T4-01 | 同源生产装配：`lib/main` 唯一独立集成 owner，四线只提供子 router（auth ⊕ management ⊕ tasks，路径集互斥，不重复注册 `/healthz`、`/readyz`） | 计划（`build_management_router`/`build_tasks_router` 待审目标接口，未实现） | 真实同源 smoke：合法 Host/Origin/JSON 登录与业务请求真正到达认证服务，不以“非 404”判通过 |
| T4-02 | 共享 HTTP 边界与强制改密应用于 9 条 Runtime 业务路由（单值 Host、严格 Cookie、权威会话、Origin/CSRF、1 MiB、限速）；认证后 `must_change_password` 对业务路由精确 403 `PASSWORD_CHANGE_REQUIRED` | 计划（`http_gate` 抽取待实现；Me/Sessions 切片已存在） | 进程内权限矩阵 + 真实同源正反例；写端点缺有效会话不触达业务仓储 |
| T4-03 | 26 条管理路由按契约逐个接入真实读写与权限；未实现路由不注册、不返回恒定 200；DB 错误 503 | 计划（Identity 管理网关未实现） | 逐路由 spy 矩阵（拒绝零调用、有权阳性>0）；`LastAdmin` → 409 `LAST_ADMIN` |
| T4-04 | 固定预览/幂等/授权全集：预览固定清单 1..=1024、60s 单调有效；`Idempotency-Key` actor+key 唯一，同内容重放先命中持久幂等记录，不同内容 409；counts 为当前授权全集汇总（非分页） | 已存在（Web `task-presentation.ts`、独立 Idempotency-Key 头切片、Runtime SNTP 结构解析切片为已实现前置；后两者在独立 worktree 分支，须在目标集成分支再次验证）+ 计划（v4 五表提交事务未实现） | 真库重放/并发用例（门禁齐备后单 case 串行）；普通用户按可见子集重算并标 `authorized_subset` |
| T4-05 | MainTask 投影：state 四态（queued/running/held/completed）、outcome 五词（非 completed 为 null）、counts 十 key 全存在（零值 "0"、u64 十进制字符串）、`created_time` 为 `TimeEvidence`、`actions.can_cancel` | 计划（Runtime Task 6 DTO 待审目标） | DTO 契约测试 + 真实同源投影核对；`can_cancel` 只表示可请求取消而非一定成功 |
| T4-06 | 单调缓存 status：`GET /devices/{id}/status` 独立 Runtime 路由读缓存快照（GET 零采集/零执行），单调接收年龄、旧会话/旧序号不覆盖；`EndpointId::DeviceStatusRead` 独立读桶 | 计划（`DeviceLiveProbe` 真实源未接线；未接线不挂载，整体 Task 4 未完成） | 缓存快照故障注入 + 真实同源读取；无真实快照源不常量伪 PASS |
| T4-07 | intent 至多一次 execute / 恢复只 query / 迟到证据：dispatching 意图提交后当前进程至多一次 execute；恢复对 dispatching/accepted/verifying 仅 `task.get` 只读、不重发；迟到有效证据更新旧任务 outcome/counts 并递增 revision，不删新 generation 设备锁、不重执行 | 计划 | 双进程 crash/断线故障注入（FakeOS）；普通 reopen 不等真实断电 |
| T4-08 | 板端 B04 故障：accepted/attempted 两阶段持久化与 OS 调用边界故障分类（rename 前确定失败与提交未知分开，分阶段以 03 §5 B-04 完整规范为准：insert_accepted DefinitelyNotCommitted 确无接受且 OS0 可非零空 payload；insert accepted CommitUnknown 断流无 reply 且 OS0；accepted 可靠后 mark_attempted DefinitelyNotCommitted 以 status 0 回复最后可靠 accepted 且 OS0，mark CommitUnknown 断流无 reply 且 OS0；OS 已调用后 finish DefinitelyNotCommitted/CommitUnknown 均断流无业务 reply（fake OS 计数 1、宿主真 reboot 0）；仅可靠 failed/OS_ERROR 才 status 0，不得泛化为“所有 OS 错误均断流”） | 计划（板端 crate/journal 未实现） | 每阶段故障注入 RED；测试中宿主真实 reboot 恒 0（FakeOS 边界） |
| T4-09 | typed → 真实获批 wire：`rsetup-protocol::wire` 共享 DTO 先行（Capabilities/DeviceStatus/ClockSample/RebootTicket/TaskRecord）；raw 字节入口 failclosed；精确 512KiB 信封与真实认证/双流互通待**专家书面决议 + 用户复审 + 获批 codec** | 计划（wire DTO 未实现）+ 未知依赖（codec/crypto） | 不自行版本升级/自造 protobuf/签名 ACK；typed/NoCodec 不证明精确 512KiB 或互通；专家决议齐备前该项 BLOCKED |
| T4-10 | 浏览器任务流程与 a11y：预览→确认→详情→取消/time-review/unknown 释放，键盘/焦点/Esc/320px/对比度/减少动效、中英切换 | 部分已存在（既有 Web 基础与 Vitest；任务页面未实现） | A 层 fixture/mock 与 B 层手工受控真实浏览器分层记录；A 层不批准 B 层 |
| T4-11 | 单二进制嵌入资源：缺 `dist` release 构建直接失败；脱离源码/静态目录运行；解析构建产物真实 JS/CSS、精确 MIME 与 immutable 缓存；API/缺失资产不回退 HTML | 计划（旧计划已有测试方案；当前实际 `web_assets` target 待创建，前置满足后才执行，接线待 Web 专题完成） | `cargo test -p rsetup-controller --test web_assets` + 脱离目录独立运行 |
| T4-12 | 真库门禁项（双引擎）：普通应用账号真实事务、设备锁、CAS、撤权/提交并发；用户已决定跳过仅 root 的锁观察（该项单独留痕，不阻断 Task 4） | 未知依赖（真实目标须串行预检：授权/隔离可丢弃/实际权限/schema 数据版本；真实备份 0700/0600 + 独立核验 + 排他停写） | 门禁齐备后显式单 case `--ignored --exact` 串行；明示实际引擎版本与非零用例数；骨架不算证据 |

## 非当前 Task 4 后续索引（完整 V1 目标；不删除未来要求、不称已 PASS、不顺带实施）

以下完整 V1 验收要求本轮从 Task 4 执行范围移出，保留原强度：完整 AT-01..18（下表 06 §6 矩阵）、完整 SSE（五种命名事件/背压/reset/重连，AT-18 与 06 §4，完整规范见下文独立小节）、06 §7 容量/发布压测基准、真实宿主 reboot 与真实电源故障（专门审核的隔离环境；本任务真实宿主 reboot 绝不执行，列入后续索引不视为自动授权）、旧 Observer 方案（独立 Observer 账号/最小权限/真实接线）、完整 V1 协议发布额外事项（现行协议修订与 00 §6 发布 BLOCKED 项；**专家书面决议 + 用户复审 + 获批 codec 仍是当前 Task 4 真实 wire 验证与最终本地 merge 的前置，不属本索引、不因收敛延期**）、旧备份受控恢复演练（AT-15）。完整目标的真实 wire/crypto/DB/浏览器验收不因 Task 4 收敛而缩减。

### 完整 SSE 统一事件命名与时序/代际规范（完整 V1 目标；保留原要求，不作当前 Task 4 RED/GREEN 或浏览器前提）

- 统一五种命名事件：`event: device.updated`、`event: task.updated`、`event: permissions.changed`、`event: system.time.changed`、`event: reset`（JSON 数据放在 `data:` 字段，客户端分别注册独立监听，不只监听默认 `message`）；
- **实际 open 后发起全量 GET：** 页面加载或重连时，必须在 SSE 连接实际 open 建立成功之后，再发起列表数据的全量 GET 请求，防止连接握手期间发生状态更新遗漏；
- **连接与同步代际（generation）：** 维护连接与同步代际，来自旧代际的旧 GET 响应或乱序事件必须丢弃；
- **对象 kind+id 水位：** revision 水位只在**同一对象类型 (kind) + 对象 ID** 内部单调递增比对，列表页与 Page 无全局 revision；
- **断线非实时：** 断线重连期间或退化为 GET 轮询时，页面状态指示器绝对不能标记为“实时”；
- **旧代际/低水位丢弃：** 旧GET响应、旧同步代际事件，以及同一kind+id不高于已应用revision水位的更新直接丢弃；不因此清DOM或重认证，不自造Page全局revision。
- **reset/断线/重连重同步：** 旧同步代际失效、取消旧GET并在回写前再核代际；重新缓冲，不把事件写入旧快照；订阅实际open后重新GET快照及合并同对象水位。同步完成且连接已建立才标实时，独立GET成功不能标实时。
- **撤权/会话失效重鉴权：** 清除旧授权投影、缓存和缓冲，关闭旧订阅并重新鉴权/进入鉴权保护页；这与普通旧响应丢弃/reset重同步不同。后续真实浏览器验收分别断言三类行为，不能只靠EventSource mock。

### 06 §6 验收追踪（按项归属；本轮文档工作全部为“未验证”）

| AT | 上游功能计划 | G5 必须核对的独立证据 |
| --- | --- | --- |
| 01–02 | 身份增量、原 01 | 初始化/强制改密、重启失效/CSRF/限速/并发最后管理员；真实 DB 与同源 HTTP 分别留证 |
| 03 | 身份增量 + 运行增量 + Web 增量 | 授权并集、撤权和隐藏目标；任务前后 epoch 重验、SSE 已排队旧字段不泄漏 |
| 04 | 身份准入 + 传输增量 | 1024 待审批资源、无总 TTL、超限可重试/重连不增假记录 |
| 05–07 | 运行增量 + 传输增量 | 固定清单与持久锁、发送意图提交后 execute、崩溃/断线/boot变化后不自动重发、unknown 保锁 |
| 08–09 | 运行增量 | 30s NTP 回退与持续退避、1970/回跳/跨 boot/TTL 单调时钟证据 |
| 10 | 运行增量 + 传输增量 | 1024 错峰/过载跳轮恢复原相位、probe 预算独立保留 |
| 11 | 传输增量 | HTTP2 ACK 与 Control/Data 业务 probe 分离；首 probe 无效与旧 nonce 不误判健康 |
| 12–13 | 传输增量 + 运行增量 | 畸形业务/票据/journal 拒绝、丢失通知查询核实且 journal epoch 变化不重发 |
| 14 | 身份增量 + 运行增量 | MySQL 8.4 LTS 与 TiDB 8.5 LTS 分别迁移、唯一约束、CAS、授权撤销/提交竞争；受限 Observer 锁观察（旧 writer 自观察不是 O 证据、MySQL 8.0 不替代 8.4，无接线/最小权限维持 BLOCKED，不恢复已暂停工具/额外扩权）；明示实际引擎版本与非零用例数 |
| 15 | 运行增量 | 真实备份条件下受控恢复；不复活撤权/密码、不盲发排队任务 |
| 16 | Web 增量 | 中英、键盘/焦点/对比度/320px、确认/unknown 在真实浏览器中验收；危险操作同一次确认保留 UUID 且重试不换 key，通过 Idempotency-Key 头传递（非 body）；普通远程 HTTP 下 `getRandomValues` 安全随机方案（无弱随机 fallback，缺能力直接禁用操作） |
| 17 | Web 增量 | 发布二进制无静态目录运行（脱离目录）、解析构建 index 中真实 JS/CSS 并验证内容/精确 MIME/immutable 缓存；缺 dist release 直接失败，严禁以空 HTML 或 assets 恒 404 伪装通过；API 和缺失资产不回退 HTML |
| 18 | 运行 SSE + Web SSE + 身份授权 | 统一 `device.updated/task.updated/permissions.changed/system.time.changed/reset` 命名事件；实际 open 后全量 GET、连接/同步代际、对象 kind+id 水位（Page 无全局 revision）；断线 GET 不标实时；旧代际GET/事件及低水位直接丢弃，不因此清DOM；reset/断线/重连使旧同步代际失效、取消旧GET、重新缓冲并在open后GET重同步；撤权/会话失效清授权投影/缓存/缓冲、关闭旧订阅并重新鉴权；三类分别真实浏览器端到端验证，不能只检查EventSource mock |

**状态：** 文档**尚未批准**（四线完整计划经指定模型复审通过前不进入 TDD）；本轮修订只收敛 Task 4 范围与安全门禁，未运行任何行为测试，全部项目默认未验证，文字预期不等于测试证据。

**交接：** 文档可作为未来测试路线图；它本身不执行 TDD、连接任何真实数据库、采集硬件数据或关闭安全发布门。交付时仅报告已写计划、当前实现基线与仍为未验证/BLOCKED 的项目，执行阶段须再次取得相应批准。

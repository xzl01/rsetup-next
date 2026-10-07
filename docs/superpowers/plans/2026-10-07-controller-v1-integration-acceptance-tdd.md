# Controller V1 跨专题集成与验收增量实施计划（2026-10-07）

> **致执行代理：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 分项落实；用 `- [ ]` 跟踪步骤。这是**条件性后续计划**，不是现有草案获批、真库/硬件测试许可或生产发布授权。

**Goal:** 在身份、设备传输、任务运行与 Web 分项交付之后，给中控 V1 建立可复现的双引擎、双进程、浏览器及容量/安全联合验收证据，不让离线测试冒充上线证据。

**Architecture:** 所有集成测试只消费已审专题产出的生产接线，不重新实现 auth、BoardClient、业务协议或前端权限判断。CI 提供普通离线回归；真实 MySQL/TiDB、两进程网络、浏览器与容量测量各自独立门控和留存去敏结果。入口为[总计划](2026-10-02-controller-v1-tdd-index.md)的 G0–G5；本计划负责 G5 和 AT-01..18 的证据汇总，不取代四份剩余计划与原计划中的 RED/GREEN。

**Tech Stack:** Rust 2024 / MSRV 1.85、Cargo、Axum、SQLx、MySQL/TiDB、Vue 3/Vitest、Playwright（浏览器工具为待审候选）；使用已获批准且锁定的版本，不因本计划隐式安装新依赖。

**Spec:** `docs/superpowers/specs/2026-09-23-controller-v1-00-index.md` §3–6、同组 01–06（尤其 `06-web-acceptance.md` §6–8）、`docs/superpowers/specs/2026-09-22-controller-design.md`、`docs/protocol_spec.md`；执行者同时阅读对应规格及本计划。

## Global Constraints

- 本计划只规划未来工作；截至文档编写时，`crates/rsetup-controller/src/main.rs` 仍只运行探活 router，`apps/controller-web` 尚无设备/任务页面；当前 `Makefile:test` 只跑 Cargo 与既有单机 UI 的 Node 测试，CI 未将 controller-web Vitest/build 与真库验收接入。不得把新增计划误称为已验证结果。
- **文档静态检查与产品功能测试明确分层：** 本轮计划修订仅针对测试与验收追踪文档的审查缺陷进行规范校正，严禁将“文档修改完成”表述为“产品功能已修复”或“测试已通过”。当前所有功能实现、集成接线与测试用例仍处于未实现/待验证状态。
- 所有中控 V1 draft-1 新增接口/默认值/版本/阈值按 00 §5、01–06 显式审阅；已审批的窄范围切片不等于整个 V1 或部署批准。协议外部专家书面决议、现行协议修订、双端负例与互操作证据未齐时，G0/安全发布维持 BLOCKED；不伪造决议、向量、备份或压测报告。
- 本地 `secret/` 只有已明确授权的两个隔离可丢弃开发目标可能用于未来测试；执行前仍须验证真实目标、权限、实际备份及迁移门禁。不能猜 URL、复制/输出凭据、宽泛运行 ignored 测试、扩大 grant、删除数据库或以 fixture 代替真实备份。未满足则跳过真库/迁移并标“未验证”；有已核准 URL 但测试失败或实际运行数为零则记失败。
- **真实 DB 与双进程 smoke 隔离：** 门禁纯测试（离线无库装配检查）与完整真实 DB smoke 必须严格分轨。完整真实 DB smoke 必须标 `#[ignore]` 并通过显式单 case `--ignored --exact` 串行执行；严禁原命令普通 cargo 跑网络 DB，严禁注入“只读 DB fake”冒充初始化、登录写入与准入写事务，生产入口不增加 fake 仓储开关。
- **Observer 锁观察证据红线：** 旧 writer 连接自观察绝对不是独立的 Observer 证据；MySQL 8.0 绝不能替代 8.4 LTS（TiDB 必须对应 8.5 LTS）；无独立 Observer 账号与最小权限（MySQL performance_schema 锁表 SELECT / TiDB PROCESS）、无真实接线时维持 BLOCKED，不恢复已暂停工具或额外扩权。
- **真实静态资源发布与脱离目录硬约束：** 发布二进制缺少 `dist` 产物时 release 构建必须直接失败（严禁打包空目录）；运行时必须脱离源码与静态文件目录（在独立临时目录启动），无需 Node 和外部静态目录即可正常服务；测试必须解析构建产物中真实 JS/CSS 文件并验证内容、精确 MIME 与 immutable 缓存，严禁以空 HTML 或 assets 恒 404 伪装测试通过。
- 真实板端重启仅在专门审核的隔离环境才可测试；本计划两进程 smoke 必须使用可注入的 `RebootOs` 安全替身且不能触碰开发宿主机的 reboot。旧备份、授权撤销和任务 `unknown` 均不得在恢复时自动重发重启。
- RED 必须是可编译、可运行的行为断言失败；缺包、缺浏览器、未获批准的 DB、无法连服务、缺脚本均不算 RED。各项 GREEN 必须分别记录层级：纯函数 / fake / tower oneshot / 真 DB / 双进程 / 真实浏览器 / 物理硬件；未运行不记 PASS。非零测试数只是必要条件：仅读取 URL、注释说明、恒真断言或两个 mock 布尔值的测试不能作为 DB/恢复/互操作证据；fixture 与真实被测路径未实现时标 BLOCKED，不提供可空跑成功的 ignored 验收函数。G5 只在上游 G1–G4 与实际部署门禁均过后可验收。
- **依赖与供应链审查：** 未来依赖若新增成员或直接引用，必须包含 manifest 清单与离线 lock 文件严格审查（锁定 MSRV 1.85、许可证兼容、无新隐式网络依赖）；既有本地 `rsetup-app` 无认证 loopback HTTP 不能复用作远程管理。中控管理 HTTP 仅适用于受控可信网络，CSRF 不等于链路加密。

## 文件职责及上游依赖

| 文件（未来实施阶段） | 职责 / 前提 |
| --- | --- |
| `Makefile`、`.github/workflows/ci.yml`、`scripts/tests/controller-ci-contract.test.mjs` | 普通回归/前端 build、配置合同测试与预审通过后可配置的双引擎 ignored 测试门控；不得自动连接无审批数据库。 |
| `crates/rsetup-controller/tests/acceptance_runtime.rs` | 使用已接入生产的 auth、设备监听、BoardClient、NTP/DB/恢复门禁，作仅本机双进程分阶段网络测试；依赖身份、传输、运行计划。 |
| `apps/controller-web/tests/controller-flow.spec.ts`、`apps/controller-web/playwright.config.ts` | 构建后的浏览器同源、授权投影、危险确认、SSE 撤权/reset 行为；依赖 Web/后端 API 完成。 |
| `docs/testing/controller-v1-acceptance.md`、`scripts/check_controller_acceptance.py`、`scripts/tests/check_controller_acceptance_test.py` | 未来操作员填写真实执行命令、引擎版本、脱敏结果、缺失证据和每一 AT 项；检查脚本只验证证据字段存在/去敏，不预填结果或证明证据真伪。 |

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

CI 构建顺序为 `npm ci`（已锁版本，按构建机策略）→ `npm test -- --run` → `npm run typecheck` → `npm run build` → Rust 路由测试，保留既有 `make test`；为 MySQL 与 TiDB 配置互不复用的、显式受控作业与非零用例断言。DB URL 不存在时只标“未验证”且普通 CI 可绿，URL 存在而实际用例零或失败时作业失败；fixture 能拒绝非隔离/非空/无真实备份的迁移测试。任何可执行 URL/凭据不写入脚本、文档、日志或提交。
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

## Task 3：真实浏览器端到端合同

**Files:** Create `apps/controller-web/tests/controller-flow.spec.ts`, `apps/controller-web/playwright.config.ts`; Test `crates/rsetup-controller/tests/web_assets.rs`（Web 专题完成后）； Modify `Makefile`、`.github/workflows/ci.yml`（仅补本项独立 E2E 门）。

**Interfaces:** Consumes 后端 `GET/POST /api/v1` 的生产 cookie/Host/Origin/CSRF、Web 的 `App` 页面/危险确认/SSE 与嵌入资源路由；Produces 不读取真实秘密的浏览器网络证据：无 Node/外部 dist 二进制提供前端，同源 API，撤权后旧页面/事件不泄漏，未知任务仍显示未知。

- [ ] **Step 1 RED：** 先在浏览器依赖已获批准、锁定并离线安装后建立可运行 Playwright 项目；编写包含明确 fixture 前置、幂等性、SSE 事件流与真实静态资源边界的行为测试：
  1. **Playwright Mock Fixture 前置（防认证畸形）：** 若集成测试没有真实事件受控来源或会话列表接口，必须明确完整合法的 fixture 前置，包括 `/api/v1/auth/me`（包含合法 `user.id`、`revision`、`active: true`、`must_change_password: false`、`csrf_token`、`authz_epoch`）、`/api/v1/auth/sessions`（提供有效会话列表信封 `{ data: { items: [...], next_cursor: null }, request_id }`）以及受控 `/api/v1/events`（返回 `Content-Type: text/event-stream`、`Cache-Control: no-cache`），**严禁因 session 列表未 mock 或认证畸形导致页面在初始化阶段直接重定向或崩溃**；初始 RED 必须是因为业务组件或预期功能未实现，而非前置认证畸形伪 RED。
  2. **危险操作幂等性（Idempotency）规范：**
     - 用户在确认对话框中触发危险操作（如设备重启）时，客户端生成单次确认 UUID；同一次确认流程中该 UUID 必须在客户端生命周期中保留，用户重试时**绝不更换 key**；
     - 幂等 key 必须通过 HTTP 请求头 `Idempotency-Key: <UUID>` 发送，**严禁放入 JSON body**；
     - 在普通远程 HTTP 非安全上下文（insecure context）中，现代浏览器禁用 `crypto.randomUUID`；前端必须实现并测试基于 `crypto.getRandomValues` 构造的 UUIDv4 方案；
     - **缺能力禁操作：** 若运行环境连 `crypto.getRandomValues` 亦不可用（缺少密码学安全随机源），**严禁弱随机（如 `Math.random`）fallback**，必须直接禁用危险操作按钮并提示缺少安全随机源；测试须分别断言有能力时复用 key 与无能力时禁用操作。
  3. **SSE 统一事件命名与时序/代际规范：**
     - 统一五种命名事件：`event: device.updated`、`event: task.updated`、`event: permissions.changed`、`event: system.time.changed`、`event: reset`（JSON 数据放在 `data:` 字段，客户端分别注册独立监听，不只监听默认 `message`）；
     - **实际 open 后发起全量 GET：** 页面加载或重连时，必须在 SSE 连接实际 open 建立成功之后，再发起列表数据的全量 GET 请求，防止连接握手期间发生状态更新遗漏；
     - **连接与同步代际（generation）：** 维护连接与同步代际，来自旧代际的旧 GET 响应或乱序事件必须丢弃；
     - **对象 kind+id 水位：** revision 水位只在**同一对象类型 (kind) + 对象 ID** 内部单调递增比对，列表页与 Page 无全局 revision；
     - **断线非实时：** 断线重连期间或退化为 GET 轮询时，页面状态指示器绝对不能标记为“实时”；
     - **清理旧 DOM 与重认证：** 收到 `reset` 事件、旧 GET 响应、事件乱序或权限撤销（`permissions.changed` 移除了必要权限）时，必须立即清除旧 DOM 投影字段，并触发重认证或跳回鉴权保护页，在真实浏览器中进行端到端断言，不仅依靠内存 EventSource mock。
  4. **真实静态资源与脱离目录硬约束：**
     - release 编译硬门禁：构建脚本中若缺少前端 `dist` 产物，release 构建必须直接报错失败，严禁打包空静态目录；
     - 资源内容与缓存校验：`crates/rsetup-controller/tests/web_assets.rs` 必须实际读取并解析构建生成的 `dist/index.html`，提取出实际包含哈希指纹的真实 JS/CSS 文件名（如 `assets/index-[hash].js`），向真实路由请求断言其内容非空、MIME 类型严格为 `text/javascript` / `text/css`、以及长期 `Cache-Control: public, max-age=31536000, immutable`（入口 `index.html` 则为 `no-cache`）；
     - 脱离目录运行：测试二进制必须在完全脱离项目源码和前端 dist 目录的临时独立工作目录中启动运行，验证单二进制能够独立正常提供页面与 API，且 `/api/v1/missing` 与 `/assets/missing.js` 绝不回退为 HTML 200，严禁以空 HTML 或 assets 恒 404 伪装测试通过。

```ts
import { test, expect } from '@playwright/test';

test('reboot-only projection never requests detail or status', async ({ page }) => {
  let detailRequests = 0;
  let statusRequests = 0;
  const requestId = '123e4567-e89b-42d3-a456-426614174001';
  // 明确合法的认证/会话 fixture 前置，避免初始因认证畸形直接重定向
  await page.route('**/api/v1/auth/me', route => route.fulfill({ json: {
    data: { user: {
      id: '123e4567-e89b-42d3-a456-426614174000', username: 'fixture',
      active: true, is_admin: false, must_change_password: false, revision: '1'
    }, csrf_token: 'c'.repeat(64), authz_epoch: '1' }, request_id: requestId
  } }));
  await page.route('**/api/v1/auth/sessions**', route => route.fulfill({ json: {
    data: { items: [], next_cursor: null }, request_id: requestId
  } }));
  // 本例仅验 reboot-only 列表权限；以受控 open 信号触发先订阅后 GET。
  // 真 SSE 背压/reset/重连另由保持连接的可控流服务验证，不把短暂 fulfill 当长连接。
  await page.addInitScript(() => {
    class ReadOnlyTestEventSource extends EventTarget {
      readonly readyState = 1;
      readonly withCredentials = true;
      readonly url = '/api/v1/events';
      onopen = null; onmessage = null; onerror = null;
      constructor() { super(); queueMicrotask(() => this.dispatchEvent(new Event('open'))) }
      close() {}
    }
    Object.defineProperty(window, 'EventSource', { value: ReadOnlyTestEventSource, configurable: true });
  });
  await page.route('**/api/v1/devices**', route => {
    const path = new URL(route.request().url()).pathname;
    if (/^\/api\/v1\/devices\/[^/]+\/status$/.test(path)) statusRequests += 1;
    else if (/^\/api\/v1\/devices\/[^/]+$/.test(path)) detailRequests += 1;
    if (path !== '/api/v1/devices') {
      return route.fulfill({ status: 404, json: {
        error: { code: 'NOT_FOUND', message_key: 'device.not_found', params: {} },
        request_id: requestId
      } });
    }
    return route.fulfill({ json: { data: { items: [{
      device_id: 'ab'.repeat(32), display_name: 'Fixture Device',
      effective_permissions: ['device.reboot']
    }], next_cursor: null }, request_id: requestId } });
  });
  await page.goto('/');
  await expect(page.getByText('Fixture Device')).toBeVisible();
  expect(detailRequests).toBe(0);
  expect(statusRequests).toBe(0);
});
```

- [ ] **Step 2 确认 RED：** 在 `apps/controller-web` 执行 `npm test -- --run` 和 `npm run build` 后，运行 `npx playwright test tests/controller-flow.spec.ts --project=chromium`（仅已安装的锁定工具，禁止临时联网安装）；确认因页面组件缺失或幂等/SSE/撤权行为未就绪而失败，并非认证畸形或未 build；在临时目录启动未完成资源的二进制，确认 `cargo test -p rsetup-controller --test web_assets` 失败。将失败的 HTTP/DOM/静态资源条件单独记录。
- [ ] **Step 3 GREEN：** 完成授权状态变更时丢弃过期投影；SSE 先建立 open 连接再发起全量 GET，代际乱序丢弃，reset/断线重取；强制改密始终不得加载业务页面；危险确认通过 `Idempotency-Key` 头传递且重试保留同一次 UUID（远程 HTTP 下使用 `getRandomValues`，缺能力时禁用按钮）；`unknown`/`202` 仍不能误报成功；发布二进制在脱离源码临时目录中独立提供已嵌入资源，精确 MIME 与长期缓存，静态资源缓存与 SPA/API 边界严格隔离。测试使用合成内容，不存 cookie/token 明文或真实 DB 值到截图、日志、fixture。独立审核语言切换后键盘焦点与 aria、320px/对比度/减少动效。
- [ ] **Step 4 GREEN 验证：** 浏览器测试、`npm test -- --run`、`npm run typecheck`、`npm run build`、Rust `web_assets`（解析实际构建 `index.html` 中的哈希 JS/CSS）与受影响旧 UI 用例全绿；脱离目录独立运行二进制检查；另记录一轮真实受控同源 HTTP 前后端交互，否则只称浏览器受控/mock E2E，不称生产联验。
- [ ] **Step 5 条件提交：** 只暂存本项测试/工具和受审 CI 接线；先完成脱敏检查，依赖版本变化须一起提交 lock 并另审。

## Task 4：AT-01..18 与容量/安全发布门禁

**Files:** Create `docs/testing/controller-v1-acceptance.md`, `scripts/check_controller_acceptance.py`, `scripts/tests/check_controller_acceptance_test.py`; Read 本计划、四份增量计划、原 01–04 与 06 §6–8；不写伪测试数据。

**Interfaces:** Consumes 每项实际命令/版本/测试报告及独立专家审查原件；Produces 每个 AT 的 `PASS / FAIL / 未验证 / BLOCKED` 和证据路径、执行层级、缺口与持续风险。默认“未验证”，不得先填通过。

- [ ] **Step 1 RED：** 为纯报告校验器写 `scripts/tests/check_controller_acceptance_test.py`；先给 `scripts/check_controller_acceptance.py` 可导入的 `validate(text: str) -> list[str]` 空桩（返回 `[]`），再写如下行为断言。真实报告模板的 18 行默认均为“未验证”，不把示例 PASS 填进报告；校验器验证字段存在，不证明外部证据为真。

```python
import unittest
from scripts.check_controller_acceptance import validate

class AcceptanceDocumentContract(unittest.TestCase):
    def test_pass_without_evidence_is_rejected(self):
        row = '| AT-01 | PASS | | | | |\n'
        self.assertIn('AT-01: PASS missing evidence', validate(row))

    def test_eighteen_unverified_rows_remain_honest(self):
        rows = 'Spec: 06 §6\n' + ''.join(f'| AT-{i:02d} | 未验证 | | | | |\n' for i in range(1, 19))
        self.assertEqual(validate(rows), [])
```

`validate` 对每个 AT 只允许唯一行，PASS 要求非空执行时间、环境/层级、命令或步骤、去敏证据路径；缺失或重复编号、非法状态及没有 06 §6 引用均返回固定错误。
- [ ] **Step 2 确认 RED：** 先执行 `python3 -m unittest scripts.tests.check_controller_acceptance_test`，确认空桩因 `PASS missing evidence` 断言失败；无脚本/模板只是环境未就绪，不算 RED。再使不合格样例被拒、18 行未验证样例合格后执行 `python3 scripts/check_controller_acceptance.py docs/testing/controller-v1-acceptance.md`，将真实报告保持未验证直到取得对应证据。
- [ ] **Step 3 GREEN：** 每个 AT 由下表的专题责任人提供实际对应证据；只有严格核对后才将该项标 PASS。1024 已批准 + 1024 初次待审、20 管理会话、RTT≤100ms、稳定1h、99%≤15s 等仅为 06 §7 **待审阅基准**，压测需记录 CPU/RAM、DB 产品/版本、板端模拟规模、报文大小、网络条件、队列峰值、延迟分位和拒绝计数；无仪表与受控环境则未验证，不得宣传支持1024。
- [ ] **Step 4 验证：** 手工与程序双重核对所有 PASS 行均有独立可追溯且去敏的证据；G0/G1/G2/G3/G4/G5 分开判定。独立安全审查、双端负例/向量/互操作、真实 DB 迁移和备份恢复、板端持久 journal 证据任何一项缺失，发布栏维持 BLOCKED，即使离线回归全部通过。
- [ ] **Step 5 条件提交：** 只暂存无真实秘密的报告模板/检查脚本；实际验收报告须由执行方审查脱敏后再提交，绝不预造 PASS 或签字。

## 06 §6 验收追踪（按项归属；本轮文档工作全部为“未验证”）

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
| 18 | 运行 SSE + Web SSE + 身份授权 | 统一 `device.updated/task.updated/permissions.changed/system.time.changed/reset` 命名事件；实际 open 后全量 GET、连接/同步代际、对象 kind+id 水位（Page 无全局 revision）；断线 GET 不标实时；reset/旧 GET/乱序/撤权清旧 DOM 及重认证真实浏览器端到端验证，不能只检查 EventSource mock |

**交接：** 文档可作为未来测试路线图；它本身不执行 TDD、连接任何真实数据库、采集硬件数据或关闭安全发布门。交付时仅报告已写计划、当前实现基线与仍为未验证/BLOCKED 的项目，执行阶段须再次取得相应批准。

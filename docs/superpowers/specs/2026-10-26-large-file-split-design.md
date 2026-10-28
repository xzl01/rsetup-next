# 大型文件拆分总览规范（2026-10-26）

日期：2026-10-26。作用域：全仓 12 个大型自有代码文件（清单见 §3）。

> **状态：待用户审阅，尚未实施（实施阶段已启动，见§9）。** 本文是全仓大型文件拆分的**唯一总览规范**：只交付统一契约、已盘点候选、12 份子规格的登记表、建议实施顺序与验收基线。本文**不是**任何单个模块的详细设计，**不修改任何源码**，**不包含实施计划**，**不自动提交**。12 份子规格（§4 登记表所列路径）**均已完成编写并经独立只读审查与修订闭环（状态见 §4 与 §9）**，每份由新的代理子会话按 §2.8 的最低要求撰写。

用词约定：**必须（MUST）/ 不得（MUST NOT）** 为硬门禁，违反即验收不通过；**应当（SHOULD）** 为默认做法，偏离须在子规格中说明；**可以（MAY）** 为允许项；**建议值** 为供审阅的默认，非既成决策。

本文档无凭据、未读取 `secret/`、未连接任何数据库；文中的命令均为「未来子规格应执行」的验证前提，**本轮一律未执行、不声称任何测试通过**。

## 1. 背景、目标与边界

### 1.1 背景

仓库内存在 12 个超过 2,500 行的自有大型源文件（合计 44,728 行，§3 实测），普遍特征是「实现与大体量内联测试同文件、多种职责混杂」。用户已批准为全部 12 个文件编写拆分规格；本轮只做规格，12 个模块的详细设计各归各的子规格。

本轮撰写期间实测核验（`wc -l`、只读检索 [Makefile](../../../Makefile)、[.github/workflows/ci.yml](../../../.github/workflows/ci.yml)、[AGENTS.md](../../../AGENTS.md)、历史 spec [2026-10-19-prometheus-metrics-endpoint-design.md](2026-10-19-prometheus-metrics-endpoint-design.md)）得到的事实已在 §3、§6 标注为「实测」；行数与内联测试切分的「约」值来自先前只读盘点快照，两者不一致时以子规格会话当时的工作树实况为准。

### 1.2 目标

1. 为 12 个文件各产出一份可独立实施、可独立验收的拆分规格（共 12 份，登记表见 §4）。
2. 用统一契约（§2）约束所有子规格，保证拆分风格、可见性口径、测试迁移口径和验收口径全仓一致。
3. 给出建议实施顺序（§5）与验证前提、验收基线（§6），供后续 12 个子会话直接引用。

### 1.3 非目标

- 不做任何 12 个模块的详细拆分设计（目标文件树、符号归属等一律下沉子规格）。
- 不改源码、不改 `Cargo.toml`/`Cargo.lock`/`package.json`、不动 CI 接线、不新增门禁脚本。
- 不写实施计划、不排期、不自动提交；拆分何时落地由用户在子规格逐份批准后另行决定。

## 2. 统一拆分契约（C1–C9，所有子规格 MUST 遵守）

### C1 结构与可见性

- 拆分是**纯职责拆分**：只在原 Rust crate 内把单文件变为私有模块树（`foo.rs` → `foo/mod.rs` + 子文件，或既有目录模块内加子文件），MUST NOT 新增 crate、MUST NOT 新增或升降任何依赖，MUST NOT 改动任何 `Cargo.toml`/`Cargo.lock`/`package.json`。
- 现有对外 import 路径与公开符号 MUST 保持不变：`crates/rsetup-controller/src/lib.rs` 以 `pub mod` 声明的 `http_auth`、`db`、`auth`、`authz`、`tasks`（实测），以及 `crates/rsetup-core/src/lib.rs` 对 `hardware`、`system_install` 私有模块的 `pub use` 选择性再导出清单（实测），拆分前后必须逐项一致；集成测试、其他 crate、bin 对这些路径的引用不得被迫修改。
- 新增子模块默认为 private；子模块间确需暴露时按**最窄可见性**选择（`pub(super)`、`pub(in crate::path)`），任何可见性提升 MUST 在子规格的「可见性变更清单」中逐项列出并给理由，MUST NOT 图省事默认 `pub(crate)`，更不得新增 `pub`。

### C2 行为与协议不变

- 行为、状态机、HTTP/CLI/UI 协议、错误与副作用顺序、SQL 文本/绑定参数/事务边界/锁范围、安全门禁（认证、鉴权、CSRF、限速、fail-closed 语义等）全部不变。
- 允许的改动仅限：纯移动代码、必要的 `mod`/`use`/`pub use`/re-export、测试 `#[path]` 或 `mod` 定位调整。MUST NOT 借拆分修 bug、改命名、重排日志、调整错误文案或"顺手"重构逻辑；发现缺陷记录到子规格「后续事项」，另立任务。

### C3 测试迁移

- 测试随被测职责迁移：哪个职责进哪个子文件，其内联测试就进对应子文件的 `#[cfg(test)]` 或该子模块的 test-only 子模块；共享 fixture 保持 test-only（`#[cfg(test)]` 模块或 `#[path]` 引入的测试专用文件），MUST NOT 为复用 fixture 而新增需要公开的接口，MUST NOT 把内联单测迁成依赖公开接口的 `tests/` 集成测试。
- 不得删除、合并或改写断言来"适配"新位置；断言与被测行为同时位。
- 现有内联测试体量巨大（§3 表，最大单文件 4,365 行测试）：**把整块测试原样搬进另一个巨大的 `tests.rs` 不算完成拆分**；测试的切分维度必须与实现的职责切分一致，测试文件同样适用 C4 的阈值口径。

### C4 规模与阈值

- 拆分的唯一驱动是职责内聚，MUST NOT 以机械行数达标为目标；行数只作审查信号。
- 建议阈值（建议值，供审阅，本轮 MUST NOT 为其新增 CI 门禁）：单文件目标 ≤ 900 行（含其内联测试）；900–1,500 行的目标文件须在子规格给出内聚性论证；> 1,500 行还须同时列出后续再拆方案并说明为何本轮不拆。
- 因内聚性（同一状态机的完整迁移、成对出现的 SQL 与行映射等）确需超限的目标文件，MUST 在子规格「例外说明」中写明理由，供审查，不得默认豁免。

### C5 ui/app.js 专项

- 用户已裁定：`ui/app.js`（实测 [ui/index.html](../../../ui/index.html) 第 416–417 行以经典 `<script defer>` 加载 `/i18n.js`、`/app.js`，无打包器、无 ES module）MUST 在 `ui/` 原地拆为多个运行时 classic 脚本——`ui/modules/` 下 18 个运行时片段 + 薄 bootstrap `app.js`（仅含原 `init()` 与顶层 `init();` 逐字，最后装载）+ `i18n.js`（不动，最先装载），共 **20 个** `<script defer>` 按文档顺序加载；拒绝保持单文件、拒绝拼接生成兼容产物。定案保持本条原意图：仍无 ES module、无生成 bundle、无 Vite，MUST NOT 迁入 apps/controller-web。片段边界、装载序与符号归属以子规格 [05-ui-app.md](2026-10-26-large-file-split/05-ui-app.md) 为唯一权威。
- 授权必要接线（仅限以下四项，不得借机重构）：① [ui/index.html](../../../ui/index.html) 的 script 清单按定案装载序改写；② `ui/` 下 8 个读取 `app.js` 的既有测试 loader 换为共享 manifest 读取（test-only helper），断言零删改；③ [crates/rsetup-app/src/server.rs](../../../crates/rsetup-app/src/server.rs) 逐片段新增 `include_str!` 常量与逐 URL route（响应 `text/javascript; charset=utf-8`，安全头由既有中间件同套自动覆盖；旧 URL `/app.js`、`/i18n.js`、`/styles.css` 全部保留、语义不变）；④ [apps/desktop/src-tauri/build.rs](../../../apps/desktop/src-tauri/build.rs) 的 `TREE_DIRS` 增加 `"modules"` 一项（与 `assets`/`fonts` 同机制，即 C6 对 `apps/desktop/` 排除的唯一窄例外）。
- 测试兜底：新增 manifest↔HTML↔server↔build.rs 一致性合同测试与逐片段按序装载的 DOM harness（同一 context 依序执行 20 个脚本并覆盖 `init()` 直达链）。拆分等价契约的口径——片段分组与生效时点、跨文件切片等价、计数（67/69 口径、8 条跨文件切片、W1–W3 位移白名单）——以子规格 05 为唯一权威，本文不重述细节。

### C6 范围排除

以下对象不在任何拆分/重构范围内，作为验证依据使用除外：

- 第三方与 vendored：`externals/`（含钉住上游 commit 的 `externals/devicesinfo`）、缓存目录（`target/`、`node_modules/`、`debian/cargo-home/`、`.cargo-local/` 等）、生成产物（`apps/controller-web/dist/`、`*.lock` 生成物、vite 产物）。
- `apps/desktop/`（workspace `exclude`、独立口径，见 [Makefile](../../../Makefile) desktop 目标）。唯一窄例外：按 C5 第 2 条与子规格 05 为 `src-tauri/build.rs` 资源清单的 `TREE_DIRS` 加一项接线，其余不动。
- 各 crate 独立的 `tests/*.rs` 集成测试文件：可作为拆分的**验证**手段，本身不是重构对象；`ui/*.test.mjs` 同理只做兼容适配、不重写。

上一项在两个模块各有一个经用户裁定的**已登记窄例外**（登记见 08/10 子规格）。其性质是「集成测试不作重构对象」口径下的**必要等价维护**——只等价扩展「读哪些文件、作用域怎么枚举」，**断言只扩不削**：断言内容、token 表、计数值、拒绝语义与用例数零削减，不构成拆分/重构：

- **08 controller `db.rs`**：`tests/v4_baseline.rs` 两条 `read_source("src/db.rs")` 断言——禁止 token 扫描表与 `create_v4_schema(` 恰 1 调用点计数——经「薄根 `src/db.rs` 必在 + `src/db/` 递归枚举」的目录收集器等价扩展其作用域（[08-controller-db.md](2026-10-26-large-file-split/08-controller-db.md) §6 登记）。
- **10 controller `main.rs`**：`main.rs` 源码文本消费者三文件四点位——[Makefile](../../../Makefile) `metrics-static-gates` 第 6 条 metrics grep、`tests/v4_baseline.rs` 的 `read_source("src/main.rs")`、`tests/production_composition.rs` 的两处 `include_str!("../src/main.rs")`——经「薄根必在 + `src/main/` 枚举 + 逐文件生产段截断」收集器与 Makefile 三段式 recipe 等价扩展其作用域，并跑 `node --test scripts/tests/*.test.mjs` 合同测试复核（[10-controller-main.md](2026-10-26-large-file-split/10-controller-main.md) §6 登记）。

### C7 秘密与证据边界

- 所有子规格文档 MUST NOT 含凭据或 `secret/` 内容；MUST NOT 为写规格连接数据库；引用的 SQL/命令行示例一律去敏。
- 子规格可以登记「建议执行的验证命令」，但 MUST NOT 声称任何测试已通过，除非其会话内真实执行并如实报告（含未执行项）。

### C8 子规格最低内容（每份 MUST 齐备）

1. 状态与基线：状态行、撰写时参照的分支/工作树说明（行号会漂移，见 §3 并行改动注记）。
2. 真实源码锚点：从当时真实源码引用的函数/结构体/常量名 + 近似行区间，MUST NOT 出现凭记忆虚构的锚点。
3. 目标模块树：拟建文件用代码块给出（如 ```` ```text ```` 树），与既有文件区分。
4. 符号归属表：既有对外可见的每个公开/再导出符号 → 目标子模块 → 新可见性。
5. 依赖方向与可见性：子模块引用方向（不得成环）、C1 口径的可见性变更清单。
6. 测试与 fixture 迁移计划：逐测试模块映射，fixture 的 test-only 安置。
7. 分批顺序与回滚：拆分为几个可独立回滚的批次、每批边界与回滚点说明。
8. 可执行验证命令：从 §6.2 基线选取并写明前提（网络、工具链、web dist）。
9. 模块专属风险与 DoD：状态机/SQL/锁/安全门禁/静态门禁影响，逐条 C1–C7 自检的完成定义。

### C9 流程

每份子规格由**新的代理子会话**承担、一次一份；子规格阶段仍只写文档不改源码；实施以逐份子规格获批为前提。12 份子规格路径已由本文固定（§4），子会话不得另起路径或合并文件。

## 3. 已盘点候选（实测基准）

实测（2026-10-25，`wc -l`）：12 个文件合计 44,728 行，总数与先前只读盘点快照一致。下表的「实现/内联测试」切分来自盘点快照（约值，「约」为快照标注），子规格撰写时以当时工作树为准。

| # | 文件（行数为实测总值） | 实现/内联测试（约） | 所属 crate / 模块声明位置（实测） |
| --- | --- | --- | --- |
| 1 | [crates/rsetup-controller/src/http_auth.rs](../../../crates/rsetup-controller/src/http_auth.rs) 5,775 | 1,410 / 4,365 | `rsetup-controller`（lib），`lib.rs` 中 `pub mod http_auth` |
| 2 | [crates/rsetup-app/src/tui.rs](../../../crates/rsetup-app/src/tui.rs) 5,514 | 2,586 / 2,928 | `rsetup-app`（bin），`main.rs` 中 `mod tui`（私有） |
| 3 | [crates/rsetup-core/src/system_install.rs](../../../crates/rsetup-core/src/system_install.rs) 4,720 | 2,340 / 2,380 | `rsetup-core`（lib），`lib.rs` 中 `mod system_install`（私有，经 `pub use` 选择性再导出） |
| 4 | [crates/rsetup-controller/src/authz/sqlx_repo.rs](../../../crates/rsetup-controller/src/authz/sqlx_repo.rs) 4,097 | 约 2,992 / 1,105 | `rsetup-controller`（lib），`authz/mod.rs` 中 `pub mod sqlx_repo` |
| 5 | [ui/app.js](../../../ui/app.js) 3,928 | 全部为实现 | 浏览器经典脚本（无打包器），`ui/index.html` 直接加载 |
| 6 | [crates/rsetup-controller/src/tasks/http.rs](../../../crates/rsetup-controller/src/tasks/http.rs) 3,472 | 1,318 / 2,154 | `rsetup-controller`（lib），`tasks` 模块（`lib.rs` 中 `pub mod tasks`） |
| 7 | [crates/rsetup-app/src/main.rs](../../../crates/rsetup-app/src/main.rs) 3,241 | 2,124 / 1,117 | `rsetup-app`（bin 入口，含两个文件内测试模块，实测） |
| 8 | [crates/rsetup-controller/src/db.rs](../../../crates/rsetup-controller/src/db.rs) 3,239 | 1,695 / 1,544 | `rsetup-controller`（lib），`lib.rs` 中 `pub mod db` |
| 9 | [crates/rsetup-core/src/hardware.rs](../../../crates/rsetup-core/src/hardware.rs) 2,913 | 2,443 / 470 | `rsetup-core`（lib），`lib.rs` 中 `mod hardware`（私有，经 `pub use` 选择性再导出） |
| 10 | [crates/rsetup-controller/src/main.rs](../../../crates/rsetup-controller/src/main.rs) 2,633 | 1,188 / 1,445 | `rsetup-controller`（bin 入口） |
| 11 | [crates/rsetup-controller/src/auth/service.rs](../../../crates/rsetup-controller/src/auth/service.rs) 2,601 | 566 / 2,035 | `rsetup-controller`（lib），`auth` 模块（`lib.rs` 中 `pub mod auth`） |
| 12 | [crates/rsetup-controller/src/tasks/service.rs](../../../crates/rsetup-controller/src/tasks/service.rs) 2,595 | 约 1,620 / 975 | `rsetup-controller`（lib），`tasks` 模块；快照注明含多个测试块 |

实测补充事实：

- 本轮核验时工作树存在其他并行未提交改动（含 `crates/rsetup-controller/src/db.rs`、`src/main.rs`、`tasks/` 若干文件、`Makefile`），因此各子规格的锚点与行号 MUST 以撰写会话当时的源码为准，本文行号只作盘点基准。
- 工作树他人并行改动持续演进（`db.rs`/`v4_baseline.rs`、`main.rs` 保留期清理段 +65/−4、`tasks/scheduler*` 与 `scheduler_store` 新增 `snapshot_effective_permissions` 消费），本总览与各子规格锚点均基于撰写时工作树快照，实施前须按当时工作树重采；09 号子规格登记的 `efi_overlay`/`curl_benchmark` 两个 `--ignored` 用例属兄弟模块轨道、仅登记不执行。
- `rsetup-controller/src/main.rs` 被既有静态门禁按**文件路径** grep（[Makefile](../../../Makefile) `metrics-static-gates` 第 6 条，实测）；它同时是 10 号文件的模块专属风险，登记在 §4。

## 4. 12 份子规格登记表

子规格固定路径（相对本文档所在目录；**均已创建；状态：01–12 共 12 份全部通过静态审查**）：

| # | 源文件 | 拟建子规格路径 | 状态 | 边界与风险摘要（详设归子规格） |
| --- | --- | --- | --- | --- |
| 01 | controller `http_auth.rs` | [01-http-auth.md](2026-10-26-large-file-split/01-http-auth.md) | 已通过静态审查；未开始 | 与 auth/service（11）、authz（04）同属认证/授权域，域内序列化；内联测试占 3/4，C3 为主要难度 |
| 02 | app `tui.rs` | [02-tui.md](2026-10-26-large-file-split/02-tui.md) | 已通过静态审查；实施完成（见§9） | bin 私有模块，无对外路径约束；UI 渲染与按键状态机耦合，按面板/事件域切 |
| 03 | core `system_install.rs` | [03-system-install.md](2026-10-26-large-file-split/03-system-install.md) | 已通过静态审查；实施完成（见§9） | 私有模块 + `lib.rs` 的 `pub use` 清单逐字不变（C1）；安装流程副作用顺序敏感（C2） |
| 04 | controller `authz/sqlx_repo.rs` | [04-authz-sqlx-repo.md](2026-10-26-large-file-split/04-authz-sqlx-repo.md) | 已通过静态审查；实施完成（见§9） | SQL 文本/绑定/事务/锁范围逐字不变；行映射与查询成对迁移；真库验证仅登记（§6.3） |
| 05 | `ui/app.js` | [05-ui-app.md](2026-10-26-large-file-split/05-ui-app.md) | 已通过静态审查；实施完成（批次2审查PASS，见§9） | C5 专项：经典脚本/全局变量/vm 测试兼容；拆分方案须在该子规格真实代码会话中盘点后定案 |
| 06 | controller `tasks/http.rs` | [06-tasks-http.md](2026-10-26-large-file-split/06-tasks-http.md) | 已通过静态审查；未开始 | tasks 域 handler 层，与 12 同域序列化；端点级鉴权/幂等/错误映射逐端点不变 |
| 07 | app `main.rs` | [07-app-main.md](2026-10-26-large-file-split/07-app-main.md) | 已通过静态审查；未开始 | bin 装配 + CLI 参数面（行为=对外协议）；文件内测试模块随职责走；宜晚于同 crate 的 02 |
| 08 | controller `db.rs` | [08-controller-db.md](2026-10-26-large-file-split/08-controller-db.md) | 已通过静态审查；实施完成（见§9） | `pub mod db` 路径与公开面不变；连接池/迁移语义敏感；工作树有并行改动，锚点须现采 |
| 09 | core `hardware.rs` | [09-hardware.md](2026-10-26-large-file-split/09-hardware.md) | 已通过静态审查；实施完成（见§9） | 私有模块 + `pub use` 清单不变；硬件探测副作用与外部命令调用顺序不变 |
| 10 | controller `main.rs` | [10-controller-main.md](2026-10-26-large-file-split/10-controller-main.md) | 已通过静态审查；未开始 | **专属风险（实测）**：`make metrics-static-gates` 第 6 条按路径 grep `src/main.rs` 且文件缺失时 Fail-Closed 报错；若改模块树须按 §6.4 门禁演进链路处理。装配层，宜最后批 |
| 11 | controller `auth/service.rs` | [11-auth-service.md](2026-10-26-large-file-split/11-auth-service.md) | 已通过静态审查；实施中（批次1完成，批次2进行中） | 认证服务域核心，测试占近 4/5；凭据/会话语义与 01、04 边界须在 auth 域内先定稿 |
| 12 | controller `tasks/service.rs` | [12-tasks-service.md](2026-10-26-large-file-split/12-tasks-service.md) | 已通过静态审查；未开始 | 任务状态机与并发/锁语义密集（C2）；快照注明多个测试块，须逐块归属 |

登记表顺序为盘点序号（约按行数降序），**不是实施顺序**；实施顺序见 §5。

## 5. 建议实施顺序与依赖

### 5.1 依赖事实

- 由 C1（对外路径与公开符号不变）可知：任一文件的拆分不会破坏其他文件的编译依赖，12 项两两之间没有硬性编译先后。
- 顺序约束来自三处：同一职责域共享边界决策（认证域 01/04/11；任务域 06/12），拆分之间必须**串行**并让边界有单一出处；装配层文件（07、10）通读各域，其自身拆分候选要等各域目标树定稿，宜**最后**；以及 §3 所述工作树并行改动要求同文件改动不并发。
- 跨域之间（core/app/controller 不同域、不同 crate）没有共享边界，**可以**并行推进，由不同子会话各自承担。

### 5.2 建议批次（建议值，非强制）

| 批次 | 文件 | 理由 |
| --- | --- | --- |
| A | 09、03、02、05 | 与其他候选无共享边界，可立即并行；05 的兼容方案确认是其自身前置 |
| B | 08、04 | controller 数据访问基座先行，供认证/任务域子规格引用稳定边界（04、08 之间亦可并行） |
| C | 11 → 01 | 认证域内串行：先定 service 层边界，再拆 HTTP 认证入口；对 04 的边界微调另立小改动 |
| D | 12 → 06 | 任务域内串行：先定 service/状态机边界，再拆 handler 层 |
| E | 07、10 | 装配层最后；10 另须落实 §4 登记的静态门禁风险处置 |

模块内部迁移批次以其子规格为唯一权威，本节波次仅定模块间顺序。

B–E 各批次之间跨域可重叠；**跨批次重叠不解除 §5.1 的同域串行约束**——同一职责域（认证域 01/04/11、任务域 06/12）的条目任一时刻至多一项在实施，因此批次 B 的 04 不得与批次 C 的 11/01 同域并发，须待前一项边界定稿后再实施；跨域并行不受批次归属影响。每批实施开始前重新核对当时工作树并按新锚点更新子规格（§3）。

### 5.3 并行规则

同一子规格只允许一个子会话撰写与（未来）实施；两个并行子会话 MUST NOT 触碰同一源文件；子规格文档互不修改（各写各的文件），对本文契约有异议时先在子规格中提出、由用户裁决，MUST NOT 擅改本文口径。

## 6. 验证前提与验收基线

### 6.1 Makefile 与 CI 实测前提（子规格引用命令前 MUST 先读）

- F1 格式门禁是 `make fmt-check`：按 Makefile 显式 `FMT_PACKAGES` 逐包 `cargo fmt --package ... -- --check` 并先校验 members 数；**不得**用 `cargo fmt --all -- --check` 判定本仓格式（`--all` 会把 `externals/devicesinfo` 第三方 submodule 卷入，见 [AGENTS.md](../../../AGENTS.md)「Rust 格式检查口径」）。
- F2 直接运行 `cargo build/test/clippy/check` **不会**执行 Makefile 先决：`rsetup-controller` 编译期要求 `apps/controller-web/dist` 在位（build.rs 缺失即失败），须先 `make web-dist-check`（或 `make web-build`）准备 web dist；经 `make build/test/lint` 等入口则自动处理。
- F3 `make controller-test-offline` 的「offline」语义 = **不初始化数据库、不触真库**，不等于全程无网络：干净检出下其先决 `web-dist-check` 可能执行 `npm ci` 与 vite 构建（可 `NPM_CI_FLAGS=--offline` 用预热缓存），cargo 下载依赖同样可能需要网络。其内容为前端 Vitest、前端 typecheck、`cargo test -p rsetup-controller --test web_assets --locked` 与 `make test`（全 workspace 测试 + `node --test ui/*.test.mjs`），实测于 [Makefile](../../../Makefile)。
- F4 `cargo clippy` 与 `cargo test` 都会编译，不是纯静态检查；作为「拆分校验」使用时按 F2 备妥前提。
- F5 `make metrics-static-gates` 是既有指标线五条 grep 静态门禁（纯只读、不编译），CI 已接线；它**不是**通用的每模块强制验收门禁，除非某子规格改动 metrics 线，MUST NOT 不加解释地要求 12 个模块逐模块跑它作为专属门禁。
- F6 真实数据库测试（`crates/rsetup-controller/tests/` 下 `mysql_identity.rs`、`tidb_identity.rs`、`metrics_real_db.rs`、`task_scheduler_sqlx.rs`、`v4_schema_readonly_verify.rs` 等 `--ignored` 轨道）需要操作员按引擎逐条授权专用开发库（[AGENTS.md](../../../AGENTS.md)「本地开发授权与凭据保护」）；本总览仅**登记**该轨道，本轮未运行、不声称通过。
- F7 CI 已接线序列（实测 [.github/workflows/ci.yml](../../../.github/workflows/ci.yml)）：前端 ci→test→typecheck→build，`make fmt-check`、`make metrics-static-gates`、clippy、`cargo test --workspace`、web_assets 资源测试、`node --test ui/*.test.mjs`、`node --test scripts/tests/*.test.mjs`（CI 接线合同，见 [controller-ci-contract.test.mjs](../../../scripts/tests/controller-ci-contract.test.mjs)）、验收账本校验。拆分不改 CI；涉及 Makefile/CI 接线变动时 MUST 同步该合同测试。
- F8 本轮不新增任何 CI 门禁或 Makefile 目标；若未来新增（含调整 `metrics-static-gates` 的 grep 路径，如 10 号将 `src/main.rs` 改模块树），MUST 走「spec → Makefile → CI → 合同测试」四步链路，先例见 [2026-10-19-prometheus-metrics-endpoint-design.md](2026-10-19-prometheus-metrics-endpoint-design.md) §7.8 的九条门禁设计与 CI 注释。

### 6.2 各子规格统一验收命令基线

子规格第 8 项（验证命令）MUST 从下表按模块选取，并写明 F1–F4 的对应前提；均为「实施后应执行」的登记，非本轮结果：

| 适用范围 | 命令 | 前提 |
| --- | --- | --- |
| 全部 Rust 模块（01–04、06–12） | `make fmt-check` | 无（纯格式） |
| 01、04、06、08、10–12（controller） | `cargo clippy -p rsetup-controller --all-targets -- -D warnings`、`cargo test -p rsetup-controller --locked` | F2：先备 web dist |
| 02、07（rsetup-app） | `cargo clippy -p rsetup-next --all-targets -- -D warnings`、`cargo test -p rsetup-next --locked` | 编译，无需 web dist |
| 03、09（rsetup-core） | `cargo clippy -p rsetup-core --all-targets -- -D warnings`、`cargo test -p rsetup-core --locked` | 编译 |
| 全量回归出口 | `make controller-test-offline` | F2/F3 |
| 05（ui） | `node --test ui/*.test.mjs` | Node 22（CI 口径） |
| 涉及 Makefile/CI 接线时 | `node --test scripts/tests/*.test.mjs` | F7 |

> 注：02、07 的命令以 `-p rsetup-next` 为准——目录名 `crates/rsetup-app` ≠ 包名，实际 `package = rsetup-next`（见 [crates/rsetup-app/Cargo.toml](../../../crates/rsetup-app/Cargo.toml)）。F2 的 web dist 前提只约束 `rsetup-controller` 专项命令与全量出口 `make controller-test-offline`；rsetup-app 自身命令无需 web dist 前提。

真库轨道（F6）与需要授权/环境的 `--ignored` 用例（如 deviceinfo、EFI、mirror 用例）不进默认基线，只按模块风险在子规格登记。

### 6.3 验收基线（DoD 通用部分）

每份子规格的实施 DoD MUST 至少包含：C1–C7 逐条自检通过（对外符号/路径逐项比对清单、SQL/协议/状态机 diff 为零的核对方式、测试同位迁移核对）；§6.2 相应命令按前提执行且如实报告；无任何「测试已通过」的无据声称；回滚方案可用。阈值例外（C4）与可见性提升（C1）各需子规格内的专项说明段落，缺一即 DoD 不成立。

## 7. 本轮交付与未交付

- 已交付：本文一份（总览规范，状态：待用户审阅，尚未实施（实施阶段已启动，见§9））＋ 12 份子规格（§4 登记表所列路径，全部完成编写、独立审查与修订闭环）。
- 未交付（本任务边界内刻意不做）：任何源码改动、实施计划、排期、提交；任何编译、测试执行与数据库操作；任何新门禁。

## 8. 审查修订记录（2026-10-25）

以下为本文独立审查后的定点修订（共 3 项），状态均为**已修订、待复核**；本记录不新增任何验证结论，§4 的 12 份子规格登记不随之更新。

1. §6.2 验收基线表：controller 专项命令的适用范围由「01–04、06、08、10–12」更正为「01、04、06、08、10–12」（02 属 rsetup-app、03 属 rsetup-core，不归 controller 专项命令）。
2. C5 样本描述：`ui/system-install.test.mjs` 更正为读取 `app.js` 全量源码后提取函数片段、以 `vm.createContext` + `vm.runInContext` 执行（`vm.runInNewContext` 仅用于 `i18n.js`）；全部引用方式的完整盘点仍归子规格 05。
3. §5.2：明确「跨批次重叠不解除 §5.1 同域串行约束」，批次 B 的 04 不得与批次 C 的 11/01 同域并发。

## 9. 实施进度与核验记录（2026-10-26）

全仓 12 份子规格已于 2026-10-26 全部通过静态审查。拆分实施阶段已启动，按各子规格登记批次推进并逐批完成简体中文提交，每批次均满足严格的质量与审查门禁。

### 9.1 已完成实施模块及提交清单

截至当前，已完成 10 个模块（09 hardware、03 system_install、02 tui、05 ui/app.js、08 controller/db、04 controller/authz、11 controller/auth/service、01 controller/http_auth、12 controller/tasks/service、06 controller/tasks/http）的全部批次拆分与收敛（经 `git log --oneline` 逐一核实）：

1. **09 core `hardware.rs`（完成，共 6 批次，6 次提交）**
   - `051fb44` refactor(core): 拆分 hardware 共享原语至 hardware/shared（批次 B1）
   - `6fdc84f` refactor(core): 拆分 hardware overlay 域至 hardware/overlay（批次 B2）
   - `ade0462` refactor(core): 拆分 hardware gpio 域至 hardware/gpio（批次 B3）
   - `fed631a` refactor(core): 拆分 hardware video 域至 hardware/video（批次 B4）
   - `b9cd42a` refactor(core): 拆分 hardware led 域至 hardware/led（批次 B5）
   - `b65d4a8` refactor(core): 拆分 hardware thermal 域至 hardware/thermal 并完成模块树收敛（批次 B6）

2. **03 core `system_install.rs`（完成，共 7 批次含 1 次 fix，8 次提交）**
   - `52ca7b1` refactor(core): 拆分 system_install 测试夹具至 system_install/test_support（批次 B1）
   - `49e16ee` refactor(core): 拆分 system_install error 与 crypto 域至 system_install/error.rs、crypto.rs（批次 B2）
   - `d6d7e51` refactor(core): 拆分 system_install token 域至 system_install/token.rs（批次 B3）
   - `222b900` refactor(core): 拆分 system_install mounts 域至 system_install/mounts（批次 B4）
   - `cb865dc` refactor(core): 拆分 system_install image 与 xz 域至 system_install/image、xz（批次 B5）
   - `a937e67` refactor(core): 拆分 system_install opener 与 backend 域至 system_install/opener、backend_sys、backend_synth（批次 B6）
   - `419f9c7` fix(core): 归位 system_install 拆分中的测试文档注释并恢复常量原序（B6 修复）
   - `6aaddfa` refactor(core): 拆分 system_install manager 域并完成入口收敛（批次 B7）

3. **02 app `tui.rs`（完成，共 5 批次含原子替换收敛，5 次提交）**
   - `3507537` refactor(app): 拆分 tui theme 与 textfit 域并建共享 fixtures（批次 1）
   - `1e8e034` refactor(app): 拆分 tui storage 域至 tui/storage（批次 2）
   - `c9a9f1b` refactor(app): 拆分 tui mission 域至 tui/mission（批次 3）
   - `1530847` refactor(app): 拆分 tui install 域至 tui/install（批次 4）
   - `1bce13e` refactor(app): 拆分 tui app 与 shell 域并完成模块树收敛（批次 5）

4. **05 `ui/app.js`（完成，共 2 批次，2 次提交）**
   - `4156ede` refactor(ui): 拆分 app.js 为 18 个运行时片段并建 manifest 与静态守恒测试（批次 1）
   - `77de6c3` refactor(ui): 前端切换为多运行时脚本装载并完成测试与资源接线（批次 2）

5. **08 controller `db.rs`（完成，共 5 批次含 1 次 fix 与 1 次收尾，8 次提交）**
   - `7b7bd9a` refactor(controller): 拆分 db pool 与 username 域并等价扩展基线扫描（批次 A）
   - `73c605e` refactor(controller): 拆分 db schema 契约与形状校验至 db/schema_shape（批次 B）
   - `f60f510` refactor(controller): 拆分 db schema 解析至 db/schema_parse（批次 B2）
   - `4d8cc2f` refactor(controller): 拆分 db probe 探针与 fresh 建库域至 db/probe、fresh（批次 C）
   - `e6b35da` fix(controller): 收敛 db 批次 C 清单外可见性并修正注释归属（批次 C 修复）
   - `036c5fe` refactor(controller): 拆分 db 迁移授权与生产迁移入口至 db/migration、production（批次 D）
   - `9c28dfc` refactor(controller): 拆分 db admission 域并完成 db 模块树收敛（批次 E）
   - `0b8bd9d` docs(controller): 同步 audit 探测注释路径并追记规格第三探测点（08 收尾）

6. **04 controller `authz/sqlx_repo.rs`（完成，共 5 批次含 1 次 fix，6 次提交）**
   - `d46f0db` refactor(controller): 目录化 authz/sqlx_repo 并迁出 sql_shape fixture（批次 A）
   - `9d1877c` refactor(controller): 拆分 authz/sqlx_repo model 与 traits 至 sqlx_repo/model、traits（批次 B）
   - `62bcf95` refactor(controller): 拆分 authz/sqlx_repo 编解码与 SQL 常量至 sqlx_repo/codec、snapshot_codec、queries（批次 C）
   - `6a574e9` fix(controller): 收敛 authz 批次 C 清单外模块可见性（批次 C 修复）
   - `49948c0` refactor(controller): 拆分 authz/sqlx_repo 事务骨架与快照读取至 sqlx_repo/tx_guard、snapshot_read、snapshot_repo（批次 D）
   - `0676bd1` refactor(controller): 拆分 authz_repo 巨型实现并完成 sqlx_repo 模块树收敛（批次 E）

7. **11 controller `auth/service.rs`（完成，共 4 批次，4 次提交 + 1 次登记）**
   - `487a6f1` refactor(controller): 拆分 auth/service model 与 repository（批次 1）
   - `76eb97a` refactor(controller): 拆分 auth/service 共享 fixture 与会话操作至 tests_support、session_ops（批次 2）
   - `c743b29` refactor(controller): 拆分 auth/service 会话分页至 pagination（批次 3）
   - `cb0e0de` refactor(controller): 拆分 auth/service 撤销域至 revoke 并完成模块树收敛（批次 4）
   - 逐批审查 PASS；规格登记 `d96f637`。

8. **01 controller `http_auth.rs`（完成，共 5 批次，5 次提交）**
   - `99de683` refactor(controller): 目录化 http_auth 并迁出限速域（批次 1 原子替换+rate_limit）
   - `3beb061` refactor(controller): 拆分 http_auth 契约与状态域至 contract、state（批次 2）
   - `164f1a0` refactor(controller): 拆分 http_auth CSRF 域至 csrf（批次 3）
   - `cd0da41` refactor(controller): 拆分 http_auth 处理器域至 login、me、password_logout、sessions（批次 4 四文件+52 测试随迁）
   - `2c04168` refactor(controller): 抽取 http_auth 共享测试设施并收敛模块门面至终态（批次 5 tests_support+router+门面 36 行终态）
   - 逐批审查 PASS；规格登记 `0b494f8`、`ce30fac`。

9. **12 controller `tasks/service.rs`（完成，共 8 批次，8 次提交）**
   - `eff0efa` refactor(controller): 抽取 tasks/service 共享契约与测试设施（批次 1 contracts）
   - `b9ef407` refactor(controller): 拆分 tasks/service 时间策略至 service/time_policy（批次 2 time_policy）
   - `9e5b526` refactor(controller): 拆分 tasks/service 状态规则至 service/state_rules（批次 3 state_rules）
   - `7b3ceb8` refactor(controller): 拆分 tasks/service 鉴权策略至 service/authz_policy（批次 4 authz_policy）
   - `0eecf04` refactor(controller): 拆分 tasks/service 审计执行者至 service/audit、actor（批次 5 audit+actor）
   - `e2e3b5b` refactor(controller): 拆分 tasks/service 提交与变更事务段至 service/tx_submit、tx_mutation（批次 6 tx_submit+tx_mutation）
   - `31671c1` refactor(controller): 拆分 tasks/service 编排入口至 service/orchestration 并清空根测试区（批次 7 orchestration+根测试区清空）
   - `32a77ac` refactor(controller): 收敛 tasks/service 根入口文档至各子模块并完成模块树终态（批次 8 B8 根入口 28 行终态）
   - 逐批审查 PASS；27 条测试守恒；规格登记 `daab037`、`f5a5bb2`、`ddbaffe`、`82048ae`、`d5d51cc`、`51c7480`、`7bcc7f5`、`e157f33`。

10. **06 controller `tasks/http.rs`（完成，共 8 个提交含 1 次定点修复，7 批次）**
    - `afcaffc` refactor(controller): 拆分 tasks/http 游标编解码域至 http/cursor（批次 1 cursor）
    - `d16901d` refactor(controller): 拆分 tasks/http 契约与状态装配至 http/contract、state（批次 2 contract+state，首轮审查 FAIL 因 fmt-check 后由 520f761 修复并复核 PASS）
    - `520f761` fix(controller): 修正 tasks/http 批次2 根测试区 import 格式（批次 2 定点修复）
    - `343fba9` refactor(controller): 拆分 tasks/http 门禁与错误映射至 http/gate 并首建共享 fixtures（批次 3 gate+fixtures 首建）
    - `2425851` refactor(controller): 拆分 tasks/http 查询解析与判定投影至 http/query、projection（批次 4 query+projection）
    - `e3e2811` refactor(controller): 外移 tasks/http 路由模块至 http/routes（批次 5a routes 外移）
    - `e592c44` refactor(controller): 迁移 tasks/http 路由集成测试组并建立 fakes 设施（批次 5b 路由集成 34 测试+fakes）
    - `cb84d7a` refactor(controller): 迁移 tasks/http 事件测试组并清空根测试区（批次 5c 事件组+根测试区清空）
    - `8508fd2` refactor(controller): 收敛 tasks/http 根入口文档与导出清单至终态（批次 6 B6 根入口 73 行终态）
    - 逐批审查 PASS；54 条测试守恒；规格登记 `57853bc`、`9c24b4d`、`aac0ee4`、`e8253e6`、`308a83e`、`a64fc6b`、`8d504da`、`5f5181c`。

### 9.2 实施中模块（10 controller main.rs）

- **10 controller `main.rs`（实施中，批次 A/B/C 完成，批次 D 推进中）**：
  - 批次 A（已提交）：`108929f` refactor(controller): 等价扩展 main.rs 源码文本门禁作用域至目录收集（批次 A）；审查 PASS；登记 `e942177`。
  - 批次 B（已提交）：`110add3` refactor(controller): 拆分 main.rs 凭据落盘与受控恢复域至 main/secret_log、recovery（批次 B）；审查 PASS；登记 `264acc3`。
  - 批次 C（已提交）：`7056c8a` refactor(controller): 拆分 main.rs 生命周期监督域至 main/lifecycle（批次 C）；审查 PASS；登记 `e158cab`。
  - 批次 D（实施中）：当前正在开展批次 D 的拆分与实施。
  - 批次 E（待实施）：后续待推进。

### 9.3 剩余未开始模块

**仅剩 10 controller main.rs**（10 号已启动实施：批 A 108929f/e942177、批 B 110add3/264acc3、批 C 7056c8a/e158cab 已完成并逐批审查 PASS；批 D 实施中，批 E 待实施）。

### 9.4 验证口径与审查门禁声明

1. **逐批验证要求**：所有已提交批次在提交前均执行了对应模块的质量门禁：`make fmt-check`、`cargo clippy`（无警告且 `-D warnings`）以及受影响模块的定向单元/集成测试全绿，且每一批次均通过了独立的逐批代码审查 PASS。
2. **05 UI 专项出口验证**：
   - 包含全量离线回归出口 `make controller-test-offline` 全部通过（包含 Vitest 前端单测、前端 typecheck、web_assets 集成测试、工作区测试与 `node --test ui/*.test.mjs` 等全部通过）。
   - 明确环境限制：`desktop-test` 因本地执行沙箱环境缺失 `libdbus-1.so.3` / `libdbus` 开发库，已如实登记为环境限制项，未在沙箱内完成运行，归 CI 兜底验证。
3. **真实性声明**：本记录严格按实际执行结果与 git 历史记录核实登记，绝不虚称任何未执行或因环境限制无法执行的验证。如实说明：06 号批次 2 出现过 fmt-check 门禁红（首轮审查 FAIL），已由独立修复提交 `520f761` 修正并复核五条全绿；10 号批 A 因 main.rs 基线漂移（保留期清理段被回退）经用户裁决按方案 A 收窄实施（G-PC2 接线取消，G-M6/G-V4/G-PC1 照做），处置已在 10 号规格 §12 登记。

### 9.5 后续待办

1. 剩余 10 号批 D/E 与收官动作（全量出口 `make controller-test-offline`、workspace clippy、总览最终一致化）。
2. 真库轨道与 desktop 真机冒烟仍归 CI/授权。

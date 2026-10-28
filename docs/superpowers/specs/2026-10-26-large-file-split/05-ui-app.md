# 子规格 05：ui/app.js 运行时脚本拆分（2026-10-26）

状态：**待用户审阅，尚未实施**。本会话只新增本文档，未改动任何源码/配置/测试/总览，未提交、未编译、未运行测试、未读取 `secret/`。行号基线：本会话工作树实测 [ui/app.js](../../../../ui/app.js) 共 3,928 行。

本轮为第 3 轮修订：落实第 2 轮复核裁决（C1 第 4 处位移关闭、I1 提取计数全面更正为复核实测，另 2 项小项口径统一），保持 18 运行时片段多脚本骨架、不动其他部分；第 2 轮逐项落点见 §16.1，本轮逐项落点见 §16.2（均已修订，待复核，不称通过）。

> **对总览的限定例外（重要）**：用户已新增明确决定——`ui/app.js` **必须**拆为多个运行时 classic 脚本；**拒绝**保持单文件；**拒绝**拼接生成兼容产物；**拒绝** type=module / 动态 import / 网络异步 loader / Vite / 生成单 bundle。允许为此做必要的 `ui/index.html`、8 个测试 loader、HTTP 静态资源路由、desktop 资源清单接线——desktop 只是**窄范围资源复制例外**，不借机重构业务。总览 [C5](../2026-10-26-large-file-split-design.md)/C6 已同步（多运行时脚本定案与窄例外登记见总览 C5/C6；逐项登记见 §15）。C1–C4、C7–C9 其余契约继续适用，其中 C1 的「crate 内私有模块」口径按浏览器运行时脚本等价物理解：拆分只在 `ui/` 原地进行、不新增依赖、不引入构建工具。

## 1. 范围与非目标

范围：[ui/app.js](../../../../ui/app.js)（3,928 行，全部为实现）及其加载（[ui/index.html](../../../../ui/index.html) 416–417 行）、HTTP 静态接线（[crates/rsetup-app/src/server.rs](../../../../crates/rsetup-app/src/server.rs)）、desktop 资源复制（[apps/desktop/src-tauri/build.rs](../../../../apps/desktop/src-tauri/build.rs)）、8 个直接读取 app.js 的测试的 loader 接线。

非目标：不重构业务逻辑、不改任何行为/文案/协议；不动 [ui/i18n.js](../../../../ui/i18n.js)（IIFE，1,596 行，导出 `window.RsetupI18n`，`ui/i18n.test.mjs` 不引用 app.js）；不动 `apps/controller-web` 的 Vite `/assets/app.js` 链路（与本任务无关）；不新增任何 CI 门禁或 Makefile 目标（F8）。

## 2. 实测基线事实（撰写会话核查）

- 加载：`ui/index.html` 416–417 行仅两个 classic `<script defer>`：`/i18n.js` → `/app.js`；`defer` 保证按文档顺序解析后执行，app.js 无 `DOMContentLoaded` 监听，入口是第 3,928 行顶层 `init();`（`init()` 定义于 3914）。
- app.js 顶层结构：1–2 行 `const i18n = window.RsetupI18n; const { t } = i18n;`；4–130 常量组（routes/workflowGroups/quickActionIds/socVendors/capabilityVisuals/hardwareToolIds/debugDeviceProfiles/defaultDebugCustom/helpProfiles/genericHelpResources/helpFaqs/contactChannels）；132–199 `const state`；201–203 `$`/`$$`/`tauriInvoke`；205–401 `const transport` 对象；403–429 `async function request`——全部 HTTP 请求的唯一发源点，向每个请求注入 `x-rsetup-request: "1"` 硬化头（服务端 `valid_request_boundary` 在 server.rs:268 校验该头、server 测试 `mutation_requires_non_simple_request_header` 保护它；hardening.test.mjs:283 再以全文正则 `"x-rsetup-request": "1"` 锁定客户端字面量）；431–436 `displayError`；438 起其余 180 个顶层 `function`/`async function` 直至文件尾（末两个为 3906 `startClock`、3914 `async function init`，3928 是顶层 `init();`）。
- 顶层声明总数：**19 个顶层 `const`**（12 个常量 + `i18n` + `t` + `state` + `$` + `$$` + `tauriInvoke` + `transport`；实测 `grep -c '^const ' ui/app.js` = 19，无顶层 `let`）+ **182 个顶层 `function`/`async function`**（`grep -c '^function \|^async function '` = 182）= **201 个顶层声明**，即 §5 归属表必须逐名覆盖的全集。
- 测试（`node --test ui/*.test.mjs`，[Makefile](../../../../Makefile) `test` 目标第 80 行）：9 个 ui 测试中 8 个读取 app.js **整文件字符串后切片/正则**，无一整文件 vm 执行：
  1. `handler(name)` 切片（正则 `(?:async )?function ${name}\\(` 起点，截到下一个 `\n(?:async )?function `）：overlay、sources、hardening、storage、storage-refresh、system-install 六文件；**去重后被切到的不同函数共 64 个（复核实测）**——overlay 4（beginApply/showApplyError/previewOverlays/applyOverlays）、sources 5（resetSourceBenchmark/benchmarkSources/benchmarkError/clearSourcePlan/previewSources）、hardening **13**（绝不能只数字面量 `handler("`：44 行 `vm.runInContext(["beginApply", "showApplyError", ...names].map(handler)…)` 会展开 harness 传入的全部 names——`tools` 数组 7 项 for 循环（applyOverlays/applyThermalPolicy/applySpiFlash/applyFanCurve/applyLedConfiguration/applySourcePlan/executeSelectedAction）、`harness(["refreshAll", "refreshOnce"])` 两处、再加字面的 navigate 与 applyFanCurveStyles）、storage 5（applyStorageMetricStyles/renderNvmeDeviceCard/renderStorageMmcCard/renderStorageTool/hardwareToolCopy）、storage-refresh 7（refreshStorageTool/renderNvmeDeviceCard/renderStorageMmcCard/renderStorageTool/refreshAll/refreshOnce/init）、system-install **38**（经 `INSTALL_FUNCTIONS.map(handler)` 全量切 38 项——38 恰是每次 createContext 的 handler 调用数；另有 installApplyRequest/applySystemInstall/installPlanImageId/planSystemInstall/installSelectableImages/pickInstallImage 等字面 handler 调用，其名均包含于该 38 项内）；各文件计数（4/5/13/5/7/38）跨文件重名去重后共 64（六组之和为 72，其中跨文件重名共 8 处：hardening 组的 beginApply/showApplyError/applyOverlays 与 overlay 组重合、refreshAll/refreshOnce 与 storage-refresh 组重合，storage 组的 renderNvmeDeviceCard/renderStorageMmcCard/renderStorageTool 与 storage-refresh 组重合，72−8=64）。加上 telemetry 双锚点 `renderCpuMetric`/`byteUnit` 与 install 审计块尾锚 `renderActions`，8 测试锚定的**不同函数共 67 个**；再加 `transport` 对象起点与 install 审计块起点 2860 这 2 个非函数锚点，切片单元合计 **69 个**；
  2. telemetry：`indexOf("function renderCpuMetric(")` 到 `indexOf("\nfunction byteUnit(")` 双锚点（起锚落 471；止锚命中 484 行行尾的 `\n`（即 485 行首），切片内容为 **471–484**：自 `renderCpuMetric` 首行至 `byteUnit` 声明前的 `\n`，含 478 `memoryUsagePercent` 全体；与 §8 第 2 条同口径）；
  3. system-install：`transportSource()` 从 `const transport = {` 到其后首个 `\n};`（即 205–401）；install 审计块的锚点 `source.indexOf("// System image install page\n//")` 命中 **2860**，而物理注释块是 **2859–2873**（2859 是该块上方的 `// ---…` 分隔线），截断点 `indexOf("function renderActions()")`（3573）——故该切片自 2860 起、到 3573 的 `function renderActions()` 之前结束（含 2860–2873 注释块余部与全部 38 个 install 单元，不含 `renderActions` 本体），而整块 2859–2873 必须作为一个整体随迁；另读 hardening.test.mjs 源码自审两条断言；
  4. desktop-ipc：以 `/tauriInvoke\(\s*["']([^"']+)["']/g` 遍历 app.js 全文（实测 31 处字面量、31 个不同命令名）比对 desktop `generate_handler!` 注册表。
  5. hardening 另有 3 条作用于**整个 app.js 字符串**的断言（[hardening.test.mjs](../../../../ui/hardening.test.mjs) 281–283）：不含 `\sstyle="`、含 `escapeHtml(frame.base64)`、含 `"x-rsetup-request": "1"`。切换 loader 后这 3 条的扫描作用域必须仍是「原 app.js 的等价物」，不得顺带扩到 `ui/i18n.js`（作用域定义见 §8）。
- 静态服务：server.rs 29–32 行对根 4 文件 `include_str!`；147–150 逐 route 硬编码（`/`、`/styles.css`、`/i18n.js`、`/app.js`），`/app.js` → `script()`（306–311，返回 `text/javascript; charset=utf-8`；同模式另有 295–297 `index()`、300–305 `styles()`、314–319 `i18n_script()`，全部是「常量 + 固定 Content Type」的无参 handler）；`local_boundary` 中间件（212–225）对**所有**响应统一注入 CSP/`nosniff`/`no-referrer`/`no-store`；2379–2394 既有测试断言 GET `/app.js` 200 + 四个安全头。
- desktop：[build.rs](../../../../apps/desktop/src-tauri/build.rs) 68 行 `ROOT_FILES: [&str; 4] = ["index.html", "app.js", "i18n.js", "styles.css"]` 逐文件复制、71 行 `TREE_DIRS: [&str; 2] = ["assets", "fonts"]` 整树复制；`copy_tree`（84–96）递归子目录并以 `target.join(entry.file_name())` 保留相对路径；`stage`（99–107）把 `TREE_DIRS` 每项复制为 `staging/<name>/…`；`publish`（140–153）按 `relative_files`（110–137）给出的相对路径逐项 `output.join(relative)` 移动 → 子目录必然落到 `dist/<name>/…`；`main` 末行 314 `tauri_build::build()` 按 `tauri.conf.json` 的 `frontendDist: ../dist` 由 tauri-codegen 嵌入**整个 dist 树**（无逐文件登记；`assets/`、`fonts/` 即此机制，与子目录数量无关）。`tauri.conf.json`:23 的 CSP 字符串是 `default-src 'self' tauri: asset:; style-src 'self'; font-src 'self'; img-src 'self' data:; connect-src ipc: http://ipc.localhost`——**没有** `script-src` 指令，同源脚本靠 `default-src 'self' tauri: asset:` 回退放行；`withGlobalTauri: true`。
- 跨脚本可见性前提：多个 classic 脚本共享同一全局词法环境，后脚本可见前脚本的顶层 `const/let/function`；一个绑定只能声明一次（重复 `const` 直接 `SyntaxError`）。

## 3. 拆分总原则（用户决定 + C2/C4 口径）

1. **运行时多 classic 脚本**：`ui/index.html` 按固定顺序加载多个 `<script defer src="/modules/app-*.js">`，全部为顶层 `function` 声明与顶层 `const` 的 classic 脚本；不加 `type=module`、不加 `export`、不改箭头函数、不建 window 大命名空间、不改 `state` 协议。
2. **纯移动**：所有顶层单元（函数体、transport 对象、常量、`state`）连同其紧邻注释**逐字**搬移；切片等价的前提是切片内容与现状字节一致（C2）。
3. **逐单元字节恒等 + 唯一归属 + 位移白名单（取代「整串原序恒等」）**：契约按单元成立、不按整串成立——(a) 每个顶层单元（函数体、`transport` 对象、各常量、`state`）连同其紧邻注释逐字搬移，在新片段内的字节与其在原 app.js 内逐字节相同；(b) 201 个顶层声明各自恰好归属一个文件（§5），跨文件零重复；(c) 拼接串（= 18 片段 + 薄 app.js，即 §8 的 `loadRuntimeSource()`）的单元顺序等于原始行号顺序，唯一允许的差异是 §4.2 白名单 W1–W3 的三处单元位移，以及文件边界处的**空行数量**（实测 182 个单元：单元间隙除 install 区域的 12 处成块注释外一律是单个空行；这 12 处中 11 处（2916–2919、2982–2983、2988–2998、3008–3010、3015–3016、3073–3085、3090–3093、3141–3143、3355–3356、3378–3380、3430–3432）位于 app-install.js 内部、不参与任何文件边界，只有 1 处（renderNvmeDeviceCard 与 installTargetSelectable 之间 2858–2874 的间隙里含 2859–2873 审计块）正跨在 app-storage 与 app-install 的边界上，整块随 app-install 迁移）。既不新增也不删除任何文件头/文件尾文本。测试 helper 的拼接是 **test-only**，不是生产 bundle。
4. **规模**：每份目标文件 ≤900 行（C4 建议阈值），实际最大 ≈713（app-install.js，逐字区间 2859–3571），无需例外论证。
5. `ui/app.js` 保留为**薄 bootstrap URL**：只含 `async function init()` 与顶层 `init();`，必须是最后一个运行时脚本；`init()` 之前不得有任何顶层立即执行逻辑（模块文件的顶层只有声明与常量初始化，除 `app-const.js` 首单元第 1–2 行读 `window.RsetupI18n`（现状）外均无跨文件顶层求值）。

## 4. 拟建文件树

### 4.1 被否决的备选（用户决定为准，登记备查）

| 备选 | 否决理由 |
| --- | --- |
| 保持 app.js 单文件 | 用户明确拒绝 |
| 构建期拼接生成 `/app.js` 兼容产物 | 用户明确拒绝「拼接生成兼容产物」；且与「多运行时脚本」目标相悖 |
| `type=module` / 动态 import / 异步网络 loader | 用户拒绝；且 8 测试按 classic 顶层词法切片，模块作用域会令全部 `handler()`/正则失效 |
| Vite / 打包单 bundle | 用户拒绝；ui/ 无包管理器与构建链，引入即违背 C1「不新增依赖/工具」 |
| 单文件 + window 大命名空间 | 属「保持单文件」变体；且需重写 8 测试全部裸标识符切片，违背「保留原断言」 |
| 片段平铺 `ui/` 根（`/app-const.js`…） | 可行但使 desktop `ROOT_FILES` 从 4 项膨胀到 22 项（4 项既有根文件 + 18 片段）、新增片段必须逐次登记、易漏；子目录只需 `TREE_DIRS` 加一项，且 `copy_tree`（84–96）递归保留相对路径、`publish`（140–153）按相对路径落到 `dist/modules/`、`tauri_build::build()` 全树嵌入，与 `assets`/`fonts` 完全同机制（§14 风险③ 是**真机验证义务**，不是待选兜底方案） |

### 4.2 文件树

```text
ui/
├── index.html                  # 仅改 416–417 两行为 script 清单（顺序同 manifest）
├── i18n.js                     # 不动，最先加载
├── app.js                      # 薄 bootstrap：init() + init();（原 3914–3928 逐字）
├── app-manifest.mjs            # 新增（test-only 模块，不被 node --test glob 拾取）
├── app-consistency.test.mjs    # 新增：静态守恒组（批次 1）+ 接线组 (a)(c)(d)（批次 2），见 §8
├── modules/                    # 新增目录，18 个 classic 运行时片段，按 §6.2 顺序加载（列出顺序即装载顺序；数字 = 所承接原行号区间的行数，区间内空行与注释原样计入，跨段文件按各段相加；树注行数为单元区间行数之和，不含段间单空行；实施后以真实文件行数为最终口径（如app-render 115=113+2段间隙、app-hardware 138=137+1））
│   ├── app-const.js            # 130  i18n/t(1–2) + 常量组（§5 行 4–130）
│   ├── app-state.js            #  72  行 132–203（state/$/$$/tauriInvoke）
│   ├── app-transport.js        # 232  205–436：transport(205–401) + request(403–429) + displayError(431–436)
│   ├── app-format.js           # 106  行 442–547（setText…localizedDocsUrl；549 起属 app-debug）
│   ├── app-debug.js            # 110  debugProfileById…applyCustomDebugDevice(549–658)
│   ├── app-refresh.js          #  79  openDialog/dismissDialog/refreshAll/refreshOnce/resolveSignals(670–748)
│   ├── app-render.js           # 113  renderAll(750–758) + renderSnapshot(998–1092)【W2】+ applyStaticTranslations(660–668)【W1】，文件内即此顺序
│   ├── app-sources.js          # 237  providerName…applySourcePlan(760–996)
│   ├── app-hardware.js         # 138  hardwareReason(438–440)【W3】+ bindHardwareButtons…overlayDisplayCopy/openHardwareTool/closeHardwareTool/renderHardwareError(1094–1227)
│   ├── app-overlay.js          # 230  renderHardwareTool…applyOverlays(1229–1458，含 beginApply/showApplyError)
│   ├── app-spi.js              # 176  initializeSpiFlashState…applySpiFlash(1460–1635)
│   ├── app-gpio-video.js       # 115  gpioFunction…captureVideoFrame(1637–1751)
│   ├── app-thermal.js          # 416  defaultFanCurveConfig…applyThermalPolicy(1753–2168)
│   ├── app-led.js              # 239  standaloneLeds…applyLedConfiguration(2170–2408)
│   ├── app-storage.js          # 448  applyStorageMetricStyles…renderStorageTool(2410–2694) + renderNvmeDeviceCard(2696–2857)
│   ├── app-install.js          # 713  install 审计块整块(2859–2873) + 38 个 install 单元(2875–3571)，MUST 紧随 app-storage，见 §5 硬约束
│   ├── app-actions.js          # 121  renderActions…renderHelp(3573–3693)
│   └── app-control.js          # 218  navigate…bindEvents/startClock(3695–3912，不含 init)
└── app-dom-harness.test.mjs    # 新增：全片段按序执行的 DOM 冒烟测试（§9）
```

拼接顺序 = HTML 顺序 = manifest 顺序 = 本树的**列出顺序**（§6.2 同序），其中 `app-gpio-video.js` 位于 spi 与 thermal 之间即原行号序。除白名单三处外，单元顺序等于原始行号顺序。

**位移白名单（§3.3(c)，仅此三处，白名单外的位移即违例）**：

| 编号 | 单元 | 原位置 | 新位置 | 理由 |
| --- | --- | --- | --- | --- |
| W1 | `applyStaticTranslations`（660–668） | refresh 组（670–748）之前 | app-render，refresh 片段之后 | 静态文案属渲染职责 |
| W2 | `renderSnapshot`（998–1092） | app-sources（760–996）之后 | app-render，sources 片段之前 | 与 `renderAll` 同文件，`renderAll` 首行即调用它 |
| W3 | `hardwareReason`（438–440） | `displayError`(431–436) 与 `setText`(442) 之间 | app-hardware 首单元 | 与硬件域同文件 |

W1–W3 只改变单元在拼接串中的**位置**，不改变任何单元内字节；被 `handler()` 切到的单元，其切片内容只取决于「本单元字节 + 下一个 `function` 出现在何处」，位移不改变本单元字节，故 §8 第 1 条的等价式仍然成立。

**相邻性硬约束（不可插队）**：8 条跨文件切片（§8 第 1 条 ①–⑧）依赖的前后片段对必须在装载顺序中紧邻——`app-sources` → `app-hardware`（`applySourcePlan` 末单元切片越界止于其首单元 `hardwareReason`(438)，即 W3——正是 W3 位移把 `hardwareReason` 放到了 hardware 首位）、`app-overlay` → `app-spi`（`applyOverlays` → `initializeSpiFlashState`(1460)）、`app-spi` → `app-gpio-video`（`applySpiFlash` → `gpioFunction`(1637)）、`app-thermal` → `app-led`（`applyThermalPolicy` → `standaloneLeds`(2170)）、`app-led` → `app-storage`（`applyLedConfiguration` → `applyStorageMetricStyles`(2410)）、`app-storage` → `app-install`（`renderNvmeDeviceCard`(2696–2857) 是 app-storage 末单元，其原有切片越过 2858 的空行、吞下 2859–2873 审计块与 2874 空行、终止于 2875 `function installTargetSelectable`，而审计块按 §5 归属 app-install 头部）、`app-install` → `app-actions`（审计块与 `applySystemInstall` 的截断点同为 `function renderActions()`(3573，app-actions 首单元)）。其中 `app-storage` → `app-install` → `app-actions` 三段必须按此顺序紧邻。

合并/再拆自由度：实现可在**不改变任一函数相对顺序（白名单除外）、不移动/新增白名单、不拆散 §5 硬约束对、不打破上面 7 对切片相邻对（对成员可合并以保紧邻、不可在对之间插入其他片段）**的前提下合并相邻文件（如 const+state），但 install、control 的边界不可动。

## 5. 顶层符号完整归属表

单元 → 目标文件（行号为拆分前 app.js 实测区间）。本表给出 §2 的 **201 个顶层声明**（19 个 `const` + 182 个 `function`/`async function`）的逐名归属：除 install 行以「38 个 install 单元」整段承接（其首末单元与关键成员已在硬约束里点名）外，其余各行逐名列出；无遗漏、无跨文件重复归属。带【W#】标记的是 §4.2 位移白名单成员。

| 原行区间 | 顶层单元 | 目标文件 |
| --- | --- | --- |
| 1–130 | `i18n` `{t}`（第 1–2 行，`const i18n = window.RsetupI18n; const { t } = i18n;`，全文件首单元）+ `routes` `workflowGroups` `quickActionIds` `socVendors` `capabilityVisuals` `hardwareToolIds` `debugDeviceProfiles` `defaultDebugCustom` `helpProfiles` `genericHelpResources` `helpFaqs` `contactChannels`（4–130 行，12 个 const） | app-const.js（其首单元读取 i18n.js 的全局导出，属现状跨文件顶层求值，故装载序保持 i18n.js 最先；第 3 行空行随 1–130 区间一并承接） |
| 132–203 | `state`（132–199） `$` `$$` `tauriInvoke`（201–203） | app-state.js（顶层全为常量初始化、无跨文件求值依赖；第 1–2 行 `i18n`/`{t}` 已改归 app-const.js，恢复原行号序，无需新增白名单） |
| 205–401, 403–429, 431–436 | `transport`、`request`、`displayError` | app-transport.js（三段是同一条错误/传输链且原序相邻：`transport` 各方法体调用同文件后方的 `request`，`request` 注入 `x-rsetup-request` 硬化头并把 HTTP/网络失败归一成 error 对象，`displayError` 再做文案归一；同文件同序即保持现状，不跨文件）|
| 442–547 | `setText` `icon` `escapeHtml` `formatNumber` `formatPercent` `renderCpuMetric` `memoryUsagePercent` `byteUnit` `duration` `relativeTime` `riskLabel` `socVendor` `socDisplayName` `capabilityVisual` `helpProfile` `localizedDocsUrl` | app-format.js（`localizedDocsUrl` 止于 547；549 起的 `debugProfileById` 属 app-debug，两段不重叠）|
| 549–658 | `debugProfileById` `gpioProfileOverride` `loadDebugState` `saveDebugState` `applyDebugDevice` `renderDebugControls` `activateDebugProfile` `applyCustomDebugDevice` | app-debug.js |
| 660–668 | `applyStaticTranslations` | app-render.js【W1】 |
| 670–748 | `openDialog` `dismissDialog` `refreshAll` `refreshOnce` `resolveSignals` | app-refresh.js |
| 750–758, 998–1092 | `renderAll` `renderSnapshot` | app-render.js（`renderSnapshot` 为【W2】；`renderAll` 首行调用它，同文件同序）|
| 760–996 | `providerName` `providerLocation` `renderSources` `resetSourceBenchmark` `benchmarkSources` `renderSourceBenchmark` `benchmarkError` `renderProviderDetail` `clearSourcePlan` `renderSourcePlan` `previewSources` `applySourcePlan` | app-sources.js（末单元 `applySourcePlan` 962–996）|
| 438–440, 1094–1227 | `hardwareReason`【W3】 `bindHardwareButtons` `hardwareToolCopy` `overlayDisplayCopy` `openHardwareTool` `closeHardwareTool` `renderHardwareError` | app-hardware.js（文件内顺序：`hardwareReason` 首位，其后按原行号序）|
| 1229–1458 | `renderHardwareTool` `preserveToolFocus` `renderOverlayTool` `overlayReadControl` `bindOverlayRead` `authorizeOverlayRead` `previewOverlays` `renderOverlayPlan` `beginApply` `showApplyError` `applyOverlays` | app-overlay.js |
| 1460–1635 | `initializeSpiFlashState` `spiFlashRequest` `sameSpiFlashRequest` `resetSpiFlashPlan` `renderSpiFlashTool` `previewSpiFlash` `renderSpiFlashPlan` `applySpiFlash` | app-spi.js |
| 1637–1751 | `gpioFunction` `gpioSource` `gpioPinCell` `gpioPinDetail` `gpioConnectorView` `renderGpioTool` `renderVideoTool` `captureVideoFrame` | app-gpio-video.js |
| 1753–2168 | `defaultFanCurveConfig` `initializeFanCurveState` `resetFanCurvePlan` `fanCurveRequest` `normalizedFanCurveRequest` `sameFanCurveRequest` `fanCurveValidationKey` `fanCurveChart` `renderThermalTool` `renderThermalPolicyPanel` `bindThermalPolicyPanel` `fanCurvePointRow` `renderFanCurvePanel` `invalidateFanCurvePreview` `updateFanCurveChart` `applyFanCurveStyles` `bindFanCurvePanel` `previewFanCurve` `renderFanCurvePlan` `applyFanCurve` `applyThermalPolicy` | app-thermal.js |
| 2170–2408 | `standaloneLeds` `defaultRgbLedConfig` `preferredLedTrigger` `initializeLedState` `rgbLedHex` `renderLedTool` `renderStatusLedControls` `renderRgbLedControls` `bindLedControls` `resetLedConfirmation` `applyLedConfiguration` | app-led.js |
| 2410–2857 | `applyStorageMetricStyles` `renderStorageMmcCard` `refreshStorageTool` `renderStorageTool` `renderNvmeDeviceCard` | app-storage.js（`renderNvmeDeviceCard` 2696–**2857** 是该文件末单元；其后是 2858 空行与 2859–2873 审计块，**不在**本文件内）|
| 2859–3571 | install 审计注释块**整块** 2859–2873（含 2859 的 `// ---…` 分隔线与 2860 的 `// System image install page` 标题行）+ `installTargetSelectable` … `applySystemInstall` 全部 38 个 install 单元（2875–3571，含 `wireInstallEvents`/`onInstallAcknowledgementInput`） | app-install.js（**MUST 紧随 app-storage**：`renderNvmeDeviceCard` 的既有切片正是吞下本块后终止于 2875 `function installTargetSelectable`，块与本文件之间不得插入任何片段）|
| 3573–3693 | `renderActions` `workflowButton` `operationButton` `renderActivity` `helpResourceUrl` `renderHelpResource` `renderContactLink` `renderContactLinks` `renderHelp` `openContact` `closeContact` `eventRow` `emptyRow` `emptyListItem` | app-actions.js |
| 3695–3912 | `navigate` `renderActionDetails` `openAction` `closeAction` `executeSelectedAction` `bindActionButtons` `openCommands` `renderCommandResults` `toast` `setTheme` `bindEvents` `startClock` | app-control.js |
| 3914–3928 | `init` + 顶层 `init();` | app.js（薄，唯一 init 调用点，最后加载） |

硬约束对（不可分文件）：`renderCpuMetric`–`byteUnit`（telemetry 双锚点）；`transport` 整体（含注释，锚点唯一）→ `request` → `displayError`（同文件同序，§2 的 `x-rsetup-request` 硬化链不跨文件）；`// System image install page\n//` 审计块**整块 2859–2873** → 全部 38 个 install 单元 → 其后紧邻 `function renderActions()`（system-install 审计块按 `indexOf` 截断，install 文件不得混入 `renderActions` 或其他单元，app-install 之前只允许 app-storage；`state.install*` 字段与 transport.install* 方法留在各自现文件）；`refreshAll`/`refreshOnce`/`resolveSignals` 相邻；`applyFanCurve`→`applyThermalPolicy` 之间单元不得拆散；storage 五函数按原相对顺序同文件。函数间引用全部发生在事件/渲染期（加载后），跨文件仅依赖全局词法可见性，无需回调注册或状态协议改动。跨片段引用方向与现状同构：`transport` 各方法（208 起）在运行期调用定义在其后的 `request`；前载片段调用后载片段的函数（`refreshAll` 681 行调用 app-storage 的 `refreshStorageTool`、`refreshOnce` 调用 app-render 的 `renderAll`，现状 680 定义于 750 之前）；后载片段调用前载片段的函数（`navigate` 3709 行调用 app-install 的 `openSystemInstallPage`）。三者都只在**调用时**解析自由标识符，与「定义在调用前、运行在加载后」的整文件现状等价。

## 6. 加载与执行模型

### 6.1 片段间引用方向与可见性

- 片段间没有任何**加载期**互相求值（除 app-const 读 i18n.js 的既有全局导出，属现状而非新增）；全部跨文件引用发生在函数体内、由 `init()`/事件/渲染触发，因此引用方向不受「先声明后使用」约束，运行期硬顺序要求只有两条：i18n.js 在 app-state 前、`/app.js` 最后（§4.2 的 storage→install→actions 紧邻性是**测试等价**层面的硬约束，不是浏览器装载要求，二者不要混同）。
- 运行期引用方向（近似，供审查，不成环）：领域文件 → app-const/app-state/app-format/app-transport（基础层，基础层不反向引用领域文件）；app-refresh → app-render（`renderAll`）；app-render → 各领域 render 函数；app-control（navigate/bindEvents）→ 各页面渲染/装载函数。基础层在前、领域居中、装配与控制流最后，与加载顺序一致，无循环依赖问题。
- 可见性口径（C1 的浏览器等价物）：除 `state`/`transport`/`t`/`i18n`/`$`/`$$`/`tauriInvoke` 与全部顶层 `function` 维持现状的全局词法可见外，不新增任何对外可见绑定，不新增 window 属性，不给既有符号加导出/前缀——即「可见性变更清单」为空。

### 6.2 执行时序

- 唯一顺序（i18n 最先——app-const 的首单元第 1–2 行即读取 `window.RsetupI18n`，保持现状相对序、装载序不因拆分改变；init 最后）：`/i18n.js` → `/modules/app-const.js` → app-state → app-transport → app-format → app-debug → app-refresh → app-render → app-sources → app-hardware → app-overlay → app-spi → app-gpio-video → app-thermal → app-led → app-storage → app-install → app-actions → app-control → `/app.js`。共 **18 个 `/modules/*` + i18n + app.js = 20 个运行时脚本**；其中 i18n 之后的 19 项（18 片段 + 薄 app.js）就是 §8 `loadRuntimeSource()` 的拼接成员，与原 app.js 的扫描作用域一一对应。
- 全部 `defer`、classic、无模块语义；每个文件的顶层执行仅求值 const 对象字面量（app-const 依赖 i18n.js 的全局导出——首单元 1–2 行读 `window.RsetupI18n`，为全部片段中唯一的跨文件顶层求值；app-state 无依赖，其余跨文件顶层求值为零）；函数仅声明不执行；唯一入口 `init()` 在最后一个脚本末尾调用一次。
- 绑定唯一性：§5 的 201 个顶层 `const`/`function` 名在「18 片段 + 薄 app.js」中恰好各声明一次（`$`/`$$`/`tauriInvoke` 只在 app-state，`transport`/`request`/`displayError` 只在 app-transport，`init` 只在 app.js），杜绝重复 `const` 的运行时 SyntaxError；`ui/i18n.js` 是 IIFE，不向全局词法环境贡献顶层绑定（唯一导出 `window.RsetupI18n`），不参与该唯一性集合。
- 不引入 `window.Rsetup*` 命名空间对象、不改 `state` 字段、不把 transport 拆对象（system-install 测试逐字执行它）。

## 7. 资源接线（逐项对应表）

清单唯一权威来源：`ui/app-manifest.mjs` 导出的有序数组 `RUNTIME_SCRIPTS`（20 项，与 HTML 顺序一致，由 §8 一致性断言双向锁定）。**路径三段规范式（唯一定义，§8 (a)(c) 的比较基准，不做二次归一化）**：manifest 项 = 相对 `ui/` 的 POSIX 路径、无前导斜杠（例：`modules/app-const.js`，另两项特殊成员是首项 `i18n.js` 与末项 `app.js`）；URL ≡ `"/" + manifestPath`；HTML `src` 必须逐字等于该 URL（因此「HTML src 去前导 `/`」≡ manifest 路径）；server.rs 的 `include_str!("../../../ui/<manifestPath>")` 以「去掉 `../../../ui/` 前缀后的路径尾段」≡ manifest 路径。handler 与常量的命名式（仅用于新增 18 条）：`modules/app-const.js` → 常量 `APP_CONST_JS`、handler `app_const_script`；既有 4 条根资源沿用现状名（`script`/`i18n_script`/`styles`/`index`），不参与命名式。

| 新增 URL | HTML `<script defer src>` | server.rs 常量（include_str!） | server.rs route/handler | build.rs 复制 |
| --- | --- | --- | --- | --- |
| `/modules/app-const.js` … `/modules/app-control.js`（18 项，顺序同上） | 与 manifest 逐行同序 | 每文件一个 `const APP_XXX_JS: &str = include_str!("../../../ui/modules/app-xxx.js");` | 每 route 一行 `.route("/modules/app-xxx.js", get(app_xxx_script))`，handler 逐字仿既有 `script()`（306–311 模式：常量 + `text/javascript; charset=utf-8`；GET/HEAD，无 path 参数） | `TREE_DIRS` 增加 `"modules"` 一项（递归复制、保留相对路径；逐文件清单模式对 18 个新文件不可维护，`assets`/`fonts` 即同机制先例） |
| `/app.js`（不变） | 保留，指向薄 bootstrap | 既有 `APP_JS` | 既有 `script()` | ROOT_FILES 既有 `app.js` |
| `/i18n.js`、`/styles.css`（不变） | 不变 | 既有 | 既有 | 既有 |

安全与 XSS：新 route 与 `/app.js` 同经 `local_boundary`，自动获得同套 CSP/`x-content-type-options: nosniff`/`referrer-policy: no-referrer`/`cache-control: no-store`；每个新 URL 的 handler 只回 `include_str!` 的固定字节，路由为逐条字面量，**不扩任意文件服务**（无目录路由、无 path 变量、无 fallback）；desktop 打包资源集合同样仅由 ROOT_FILES/TREE_DIRS 白名单决定。

## 8. 测试接线（8 个既有测试全部保留原断言）

**test-only helper**（`ui/app-manifest.mjs`，非 `*.test.mjs`，不被 `node --test ui/*.test.mjs` glob 执行）：导出 `RUNTIME_SCRIPTS`（20 项有序相对路径，权威清单，§7 规范式）、`loadRuntimeSource()`（按序读取 **i18n 之后的 19 项**——18 片段 + 薄 app.js——并 `join("\n")` 返回拼接串；**故意不含 `i18n.js`**，使拼接串始终是「原 app.js 的等价物」，hardening 的 3 条全量断言作用域因此逐字不变）、`loadRuntimeParts()`（同一 19 项的 `[path, text]` 有序数组，供 DOM harness 与 IPC 扫描逐文件使用）、`handler(source, name)`（现六处 handler 的逐字等价实现，作用于拼接串）。8 个测试只把 `readFileSync("./app.js")` 换为 `loadRuntimeSource()`、把本地 `handler(name)` 换为共享实现，**其余断言一条不删、不改、不弱化**：

1. 六处 `handler()` 切片（64 个不同函数名，§2 已逐文件点名）：等价式按「单元字节 + 下一个顶层 `function` 的位置」成立，不按「函数不得位于文件末」成立——**没有**「边界函数必须留在文件中部」这类约束。规则：(i) 片段边界**零新增、零删除**任何头尾文本（不写文件头注释、不加 `"use strict"`、不动 import/export——本来就没有）；(ii) 既有 install 审计注释块 2859–2873 整块随 app-install 迁移，不复制、不改写；(iii) 文件末单元的切片跨越文件边界，一直取到**下一文件的首个顶层 `function`**，与原字节等价，唯一允许的差异是边界处的空行数量，而既有断言全部是内容正则/vm 执行/字符串包含，不数空行。跨文件切片共 **8 条**（复核实测）：① `applyOverlays`（app-overlay 末 1422–1458 → app-spi 首 `initializeSpiFlashState`(1460)）；② `applySourcePlan`（app-sources 末 962–996 → app-hardware 首 `hardwareReason`(438，即 W3)）；③ `applySpiFlash`（app-spi 末 1586–1635 → app-gpio-video 首 `gpioFunction`(1637)）；④ `applyThermalPolicy`（app-thermal 末 2137–2168 → app-led 首 `standaloneLeds`(2170)）；⑤ `applyLedConfiguration`（app-led 末 2371–2408 → app-storage 首 `applyStorageMetricStyles`(2410)）；⑥ `renderNvmeDeviceCard`（app-storage 末 2696–2857 → app-install 首 `installTargetSelectable`(2875)，间含整块审计注释 2859–2873）；⑦ `applySystemInstall`（app-install 末 3530–3571 → app-actions 首 `renderActions`(3573)）；⑧ install 审计块切片（起点 2860、截断 3573，横跨 app-install 全体）。①–⑦ 全部被切：`applySourcePlan`/`applySpiFlash`/`applyThermalPolicy`/`applyLedConfiguration` 四个文件末单元由 hardening 的 `tools` 数组 7 项 for 循环经 44 行动态 `map(handler)` 切到，「未被切到」的说法不成立。这 8 条由规则 (i)（片段边界零头尾文本）、(iii)（文件末取到下一文件首个 `function`）、§4.2 装载序相邻（7 对、不可插队）与一致性 (b4) 单元序核对共同覆盖，不引入新机制。其余 57 个被切函数（64 减去 7 个跨边界末单元函数）的锚点落在本文件内部，其前驱/后继按 §4.2/§5 归属保持不变（`benchmarkError` 后仍是 `renderProviderDetail`、`navigate` 后仍是 `renderActionDetails`、`applyFanCurveStyles` 后仍是 `bindFanCurvePanel`、`refreshAll` 后仍是 `refreshOnce`、`hardwareToolCopy` 前 `bindHardwareButtons` 后 `overlayDisplayCopy`）。拼接串最后单元 `init`（薄 app.js 本体）的切片到拼接串尾 = 与原文件 EOF 语义等价，不计入跨文件切片；其既有 `.replace(/init\(\);?\s*$/,"")` 去尾属现状语义、不是拆分引入的差异。批次校验以 §3.3 的静态守恒断言 + 全部 8 个测试原断言通过为准，不以某个「20 函数清单」为准。
2. telemetry 双锚点：`renderCpuMetric`/`byteUnit` 同在 app-format 且相对顺序不变，`indexOf` 双锚点在拼接串上落回原区间——起锚 471、止锚命中 484 行行尾的 `\n`（485 行首），切片内容即 471–484：自 `renderCpuMetric` 首行至 `byteUnit` 声明前的 `\n`，含 478 `memoryUsagePercent` 全体（与 §2 第 2 条同口径），锚点之间无文件边界，切片字节不变。
3. system-install：`transportSource()` 两锚点在拼接串上落回 205–401 原字节区间（`transport` 是 app-transport 首单元，其前方无其他内容，首个 `\n};` 仍是 401）；install 审计块 `// System image install page\n//` 在拼接串中仍唯一、锚点仍命中其 2860 行、截断点 `function renderActions()` 由 §5 硬约束与 §4.2 紧邻性保证；hardening 源码自审的两条断言字面量**必须继续写在 hardening.test.mjs 本体**（被 system-install 按文本匹配），不得搬入 helper；helper 与 server.rs/build.rs 等旁文件的读取禁止引入 ` style="` 等被全量 `doesNotMatch` 扫到的文本（helper 读的是路径，不并入源码字符串，天然安全）。system-install 自审的匹配文本含 `source` 标识符（`assert.match(hardening, /doesNotMatch\(source, /\\sstyle="\/\)/)`），故 hardening.test.mjs 迁移时须保留 `const source = loadRuntimeSource()` 的命名，不得改动该标识符名。
4. desktop-ipc：扫描作用域改为 `loadRuntimeSource()`（= 18 片段 + 薄 app.js 的拼接串，等价于原 app.js 全文），或等价地对 `loadRuntimeParts()` 各文件跑同一正则再并入同一集合；`ui/i18n.js` 实测 0 处 `tauriInvoke(`，扫与不扫结果等价，但 `RUNTIME_SCRIPTS` 的 20 项必须完整（漏项由一致性测试的序列断言兜住），断言集合与 `generate_handler!` 注册表的包含关系不变。

**八测试逐文件迁移映射**（每行仅改 loader 来源，断言集合恒等）：

| 测试文件 | 现读取/切片方式 | 迁移后 | 依赖的片段边界约束 |
| --- | --- | --- | --- |
| overlay.test.mjs | `handler()` ×4（beginApply/showApplyError/previewOverlays/applyOverlays） | `loadRuntimeSource()` + 共享 `handler()` | 四函数同落 app-overlay 且相对顺序不变；`applyOverlays` 是本文件末单元，其切片跨到 app-spi 首个 `function initializeSpiFlashState`（§8 第 1 条 (iii)） |
| sources.test.mjs | `handler()` ×5（resetSourceBenchmark/benchmarkSources/benchmarkError/clearSourcePlan/previewSources） | 同上 | 全落 app-sources；`benchmarkError` 之后仍是 `renderProviderDetail`；文件末单元 `applySourcePlan` 被 hardening 的 tools 循环切到，其切片跨至 app-hardware 首单元 `hardwareReason`（W3），依赖 §4.2 相邻对 `app-sources` → `app-hardware` |
| hardening.test.mjs | `handler()` ×**13**（字面 navigate/applyFanCurveStyles + 44 行动态 `map(handler)` 展开：beginApply/showApplyError、`tools` 数组 7 项 for 循环（applyOverlays/applyThermalPolicy/applySpiFlash/applyFanCurve/applyLedConfiguration/applySourcePlan/executeSelectedAction）、harness 传入的 refreshAll/refreshOnce）+ 3 条全量源码断言（281–283） | 同上 | `navigate` 落 app-control、后继仍是 `renderActionDetails`；`applyFanCurveStyles` 落 app-thermal、后继仍是 `bindFanCurvePanel`；4 条文件末跨文件切片：`applyThermalPolicy`→`standaloneLeds`、`applySpiFlash`→`gpioFunction`、`applyLedConfiguration`→`applyStorageMetricStyles`、`applySourcePlan`→`hardwareReason`（§8 第 1 条 (iii) + §4.2 相邻对）；3 条全量断言的作用域 = `loadRuntimeSource()`（不含 i18n.js）；两条被 system-install 自审的断言留在本体，`source` 标识符名迁移后不变 |
| storage.test.mjs | `handler()` ×5（applyStorageMetricStyles/renderStorageMmcCard/renderStorageTool/renderNvmeDeviceCard/hardwareToolCopy） | 同上 | 前四者同落 app-storage（2410–2857 原序）；`renderNvmeDeviceCard` 是本文件末单元——其切片穿过 2858 空行、含 2859–2873 审计块、终止于紧邻 app-install 的首个 `function installTargetSelectable`（**不是截到 EOF**）；`hardwareToolCopy` 落 app-hardware，前驱 `bindHardwareButtons`、后继 `overlayDisplayCopy` |
| storage-refresh.test.mjs | `handler()` ×7 名（refreshStorageTool/renderNvmeDeviceCard/renderStorageMmcCard/renderStorageTool/refreshAll/refreshOnce/init），其中 `handler("init")` 带 `.replace(...)` 去尾 | 同上 | 存储四者同 app-storage（跨边界规则同上）；`refreshAll`/`refreshOnce` 同落 app-refresh 且其后继仍是 `resolveSignals`；`init` 仍是拼接串最后一个单元，其前一单元为 app-control 末单元 `startClock` |
| system-install.test.mjs | `handler()` ×**38**（经 `INSTALL_FUNCTIONS.map(handler)` 全量切片——38 恰是每次 createContext 的 handler 调用数，另有六处字面 `handler()` 其名均在 38 内）+ `transportSource()` + 审计块 + hardening 源码自审 | 同上 | §5 硬约束对与 §4.2 紧邻性全部满足；`applySystemInstall` 的切片跨到 app-actions 首个 `renderActions`；`transportSource()` 两锚点仍在 app-transport 内闭合 |
| telemetry.test.mjs | renderCpuMetric→byteUnit 双锚点 | 锚点改在拼接串上，逐字不变 | 双锚点同落 app-format 且顺序不变，锚点间无文件边界 |
| desktop-ipc.test.mjs | 全文正则扫 `tauriInvoke("…")` | 对 `loadRuntimeSource()` 拼接串跑同一正则（或遍历 `loadRuntimeParts()` 合并集合） | 20 项清单完整（i18n 首位、app.js 末位）；扫描作用域覆盖 18 片段 + 薄 app.js（i18n.js 实测 0 处，不影响结果） |

**一致性测试**（新增 `ui/app-consistency.test.mjs`，第 9 个测试）——**两组断言、按批次启用，生效时点写死如下**：

- **静态守恒组（批次 1 提交即入库并生效）**：(b1) `RUNTIME_SCRIPTS` 各项路径存在、对应文件 `node --check` 可解析；(b2) 顶层 `const`/`function` 名跨文件零重复；(b3) §3.3 的逐单元字节恒等 + 唯一归属 + 位移白名单（W1–W3）核对——以**此批次仍然原样存在**的旧 `ui/app.js`（3,928 行）为基准，因此不需要为旧文件另存副本；(b4) 拼接串单元序列 = 「原行号序 + W1–W3」这一唯一允许的序列。**作用域随批次走**：批次 1 时薄 bootstrap 还不存在，本组只处理 manifest 的中间 18 项（18 片段），声明全集是 **200 名**（`init` 及其顶层调用此时仍在旧 app.js 内）；批次 2 把末项 `app.js` 纳入后全集恢复 **201 名**，首项 `i18n.js` 始终只做存在性/可解析性检查、不参与声明唯一性集合。
- **接线组（批次 2 cutover 提交内启用）**：(a) HTML 中 `src` 以 `/` 开头且以 `.js` 结尾的 script 序列 === `RUNTIME_SCRIPTS` 的 `"/" + p` 逐项相等（§7 规范式，i18n 首位、`/app.js` 末位）；(c) 逐条比对 server.rs（见下）；(d) `apps/desktop/src-tauri/build.rs` 的 `TREE_DIRS` 含 `"modules"`、`ROOT_FILES` 含 `app.js`/`i18n.js`/`index.html`/`styles.css`。

批次 2 之后旧 app.js 变成薄 bootstrap，(b3) 不能再以它为基准：同一提交内把它改写为**等价的静态守恒断言**——把 201 个单元名按 manifest 顺序内联为测试里的基线数组（连同 W1–W3 三处标注），断言「从各片段解析出的顶层声明序列 === 基线数组」，即顺序/归属/白名单三项继续被锁定；不引入任何旧文件副本或大文件基线快照。(a)(c)(d) 在批次 1 期间不写入测试文件（此时旧 app.js 仍是生产文件，(a)(d) 必判假失败），批次 2 提交内追加对应 `test()` 即生效。

(c) 的比较是**逐条三元组**、不是计数相等：对每个 manifest 路径 `p`，(i) `router()` 内恰有一条 `.route("/" + p, get(h))`；(ii) `h` 必须等于 §7 命名式给出的该 URL 的规范 handler 名（如 `app_const_script`），且该 handler 体返回的常量名 ≡ `p` 的大写蛇形（`APP_CONST_JS`）；(iii) 该常量的 `include_str!` 字面量去掉 `../../../ui/` 前缀后 ≡ `p`；(iv) 反扫 `router()` 内所有以 `.js` 结尾的 route，逐条必须落在 `"/" + RUNTIME_SCRIPTS` 集合内（既有 `/i18n.js`、`/app.js` 亦在清单内），无未登记项、无遗漏项。该测试与 8 个测试共同构成「清单 ↔ HTML ↔ server ↔ desktop」的静态锁定；HTTP 200/头断言在批次 2 加入 server.rs 既有 inline 测试模块（与 2379–2394 同模式逐 URL 加 GET 断言），不改既有断言。

**禁止**：把生产代码拼接成 bundle 再加载；用 helper 之外方式复制片段；只新增测试而放宽/删除上表任一既有断言。

## 9. 真实执行验证设计（仅设计，本任务不执行）

现有 8 测试只执行片段，存在单片段 vm 盲点（跨片段绑定、加载顺序、init 恰一次、重复 const）。新增 `ui/app-dom-harness.test.mjs`：

- **DOM stub**：最小 document（选择器 → 节点表）、window、location、history、localStorage、`setInterval`/`setTimeout` 捕获；stub 上未预期的方法/属性访问一律 `throw`（fail-closed，不返回万能对象），以免把「引用了不存在的 API」误判成通过。
- **逐脚本执行**：同一个 `vm.createContext` 内按 `RUNTIME_SCRIPTS` 顺序先跑 `i18n.js`（得 `window.RsetupI18n`），再用 `loadRuntimeParts()` 依次 `runInContext` 那 19 项（18 片段 + 薄 app.js），模拟浏览器逐脚本装载；覆盖范围＝这 **20 个脚本的顶层执行 + `init()` 的直达调用链**（`init` → `loadDebugState`/`applyStaticTranslations`/`setTheme`/`bindEvents`/`startClock`/`refreshAll` 第一层；`navigate` 只在 stub 的 `location.hash` 命中路由时被调），不声称覆盖事件回调内部的分支链。任一脚本读取未定义标识符都会让该次求值抛 `ReferenceError` 并使测试失败。
- **async `init()` 的等待（不二次调用、不改生产代码）**：薄 app.js 的最后一条语句就是顶层 `init();`，经典脚本的完成值即该调用的返回值，所以 `vm.runInContext(bootstrapSource, ctx)` 的**返回值就是那个原始 promise**；harness 直接 `await` 它，任何 rejection 即判失败（不 catch 吞掉、不再调用 `init()`、不加 wrapper）。生产侧 `init` 的定义与调用点逐字不变。
- **init 恰一次的证据**：`window.setInterval` stub 断言恰被调用两次——`startClock` 的 1,000ms tick（3911）与 `init` 末尾的 10,000ms `refreshAll`（3925）各恰一次；配合 `bindEvents` 内 `addEventListener` 注册次数的相等式，等价于「init 体恰执行一次」。

浏览器轨道：本机 `cargo run -p rsetup-next` 后人工/脚本冒烟（路由切换、storage 抽屉、install 页、telemetry 刷新）；desktop 轨道：`make desktop-test` + 人工启动 `apps/desktop`（`withGlobalTauri` 注入 `window.__TAURI__` 后 transport 走 IPC 分支；`tauri.conf.json`:23 未定义 `script-src`，同源脚本由 `default-src 'self' tauri: asset:` 回退放行，新 URL 无需改 CSP），两轨道都须覆盖新清单。

## 10. 分批原子接线与回滚

- 批次 1（纯新增，可独立回滚）：新增 `ui/modules/*.js`（18 个）、`ui/app-manifest.mjs`（`RUNTIME_SCRIPTS`/`loadRuntimeSource()`/`loadRuntimeParts()`/`handler()`）与 `ui/app-consistency.test.mjs` 的**静态守恒组**（§8 的 b1–b4：文件存在 + `node --check` + 绑定零重复 + 逐单元字节恒等/唯一归属/位移白名单）。b3 以本批次**仍在原位**的旧 `ui/app.js`（3,928 行，仍是生产文件）为基准直接读取比对，不复制大文件、不留旧副本、不新增入库工具。此批不改变任何现有行为：旧 app.js/HTML/server/desktop 未动，8 个既有测试仍读 app.js。
- 批次 2（cutover，MUST 单提交原子接线，缺一即白屏/404）：HTML script 清单 + server.rs 常量/route/handler + server inline 测试新 URL 断言 + build.rs `TREE_DIRS` + 8 测试 loader 切换 + 薄 app.js + consistency **接线组**（(a)(c)(d) 三个 `test()`）与把 b3 基准改写为入库顺序基线数组（§8）+ `ui/app-dom-harness.test.mjs`。旧 app.js 的 3,928 行本体在同提交被替换为薄 bootstrap（内容由批次 1 片段承接；不留临时副本）。dom-harness 只能在本批启用：批次 1 时 manifest 末项 `app.js` 仍指向全文，与 18 片段同 context 装载会重复声明。
- 批次 3（出口验证，见 §12）：`node --test ui/*.test.mjs`、`cargo test -p rsetup-next --locked`（含 server 测试）、`make desktop-test`；desktop `dist` 陈旧片段提示按 build.rs 既有 `cargo:warning` 口径处理（`rm -rf apps/desktop/dist`）。
- 回滚单位=批次提交；不存在双加载窗口（每提交内旧/新恰一套生效）；`init()` 全链唯一，无重复执行。**revert 批次 2 只回到批次 1 完成态**（18 片段与 helper/静态守恒测试仍在库里但不被加载，旧 app.js 恢复为 3,928 行并继续由 HTML/server/desktop 消费）；要回到拆分前的原始状态必须**再 revert 批次 1**。

## 11. 不变清单（行为/文字/HTTP/IPC/状态/安全）

页面行为与渲染输出、i18n 键与文案、`/api/*` 与 `tauriInvoke` 命令及参数（desktop-ipc 断言）、`state` 字段与 `transport` 方法字节、既有 URL 的响应体语义（`/app.js` 仍 200 且为 JS）、新 URL 的方法（GET/HEAD）、MIME `text/javascript; charset=utf-8`、四个安全头、CSP 字符串、desktop 资源白名单语义——全部不变。允许变化仅限：HTML script 标签清单、`/app.js` 响应体改为薄 bootstrap、8 个测试的 loader 来源（断言不动）、build.rs `TREE_DIRS` 目录清单一项、server.rs 纯新增常量/route/handler 与新 URL 的 GET 断言、`ui/` 内纯新增文件（18 片段 + `app-manifest.mjs` + `app-consistency.test.mjs` + `app-dom-harness.test.mjs`）。

## 12. 验证命令（登记，非本轮结果）

- `node --test ui/*.test.mjs`（Node 22 CI 口径；含新 9/10 号测试）——拆分主体。
- `cargo test -p rsetup-next --locked` 与 `cargo clippy -p rsetup-next --all-targets -- -D warnings`——server.rs 接线。**包名说明**：目录 `crates/rsetup-app` 的包名实测为 `rsetup-next`（[Cargo.toml](../../../../crates/rsetup-app/Cargo.toml) `name` 字段，与 [Makefile](../../../../Makefile) `FMT_PACKAGES` 一致）；总览 §6.2 包名已更正为 `rsetup-next`（§15 第 4 条）。本包只依赖 rsetup-core，**不需要** `apps/controller-web/dist`，因此本组命令**无 F2 前提**；F2（先 `make web-dist-check`）只约束 `rsetup-controller` 的编译命令与全量出口，不适用于这里。
- `make desktop-test`、`make desktop-check`（manifest-path 口径，desktop 在 workspace 之外；本会话未执行，不伪称通过）。
- 全量出口（F2/F3）：`make controller-test-offline`（其先决 `web-dist-check` 即 F2；干净检出可能触发 npm/vite 网络，见 F3）。
- 本会话仅写文档，以上均未执行。

## 13. C1–C9/C8 逐条自检

C1：拆分在 `ui/` 原地、零新依赖、零构建工具，可见性变更清单为空（§6.1）；`x-rsetup-request` 硬化链（transport → request → displayError）整条留在 app-transport.js，不跨文件；C2：§11 不变清单 + 单元逐字搬移（§4.2 白名单 W1–W3 只改变单元位置、不改变单元内字节）；C3：8 测试断言原样保留、helper 为 test-only（§8）；C4：最大片段 ≈713 行（app-install.js，逐字区间 2859–3571，含 2859–2873 审计块），全部 ≤900，无例外申请；C5：本方案即 C5 要求由子规格 05 定案的产物（含全局符号与加载顺序盘点 §2/§5/§6）；C6：desktop 仅 §7 一行清单接线例外（总览 C6 已同步，窄例外登记见总览 C6）；C7：无凭据、未连库、未读 secret；C8：状态行与本树基线（首段）、真实锚点（§2/§5 行区间）、目标模块树（§4）、符号归属表（§5）、依赖方向与可见性（§6.1）、测试迁移计划（§8 映射表）、分批与回滚（§10）、验证命令（§12）、模块专属风险与 DoD（§14/§3/§9 各节）逐项齐备；C9：本文即子会话一次性交付，不改总览与其他子规格。

## 14. 风险与 DoD

风险：① 片段边界截断测试切片——以 §3.3 的逐单元字节恒等（批次 1 入库断言）+ §8 第 1 条的跨边界等价规则（跨文件切片 ①–⑧ 共 8 条，含 7 个被切的文件末函数逐一点名）+ §4.2 相邻性硬约束拦截，最终以 8 个测试原断言全部通过为判据；② 重复顶层绑定致运行时装载 SyntaxError——consistency 静态守恒组 (b2) + dom-harness 逐片段装载（§9）；③ **desktop 轨道的验证义务**（机制已定案，不是待选方案）：`copy_tree`（84–96）递归并保留相对路径、`publish`（140–153）按相对路径把文件搬到 `dist/<相对路径>`，`modules/` 必然落到 `dist/modules/`，`main` 末行 `tauri_build::build()` 经 tauri-codegen 嵌入整个 dist 树——与 `fonts`/`assets` 完全同一机制，`TREE_DIRS` 加 `"modules"` 即为定案；批次 3 **必须**实测并如实报告：`apps/desktop/dist/modules/` 下 18 个片段全部在位、`report_stale` 不报新增陈旧项、desktop 启动后 20 个脚本均从嵌入协议加载且无 CSP 违规。`ROOT_FILES` 逐项列名只是**说明性替代**（22 项、每次新增都要同步），默认不采用；若真机现象与上述机制不符，按现象另立规格，不在本任务内改机制；④ HTML/server/desktop/manifest 四表漂移——consistency 接线组 (a)(c)(d) 静态锁定（批次 2 起）；⑤ `no-store` 下首屏共 20 个脚本请求（新增 18 个，全部本地回环）的开销（HTTP/1.1 可接受，不引入构建工具即无更优解）。DoD：§8 全部既有断言原样通过；consistency 静态守恒组自批次 1 起、接线组自批次 2 起持续通过；§12 命令按前提执行并如实报告（含未执行项）；§7 对应表逐项落地无缺项；C4 无超限文件；C7 无凭据；无 TODO/TBD 残留；不新增 CI 门禁（F8）；本规格不声称任何测试已 PASS。

## 15. 总览同步事项（已全部完成）

1. 总览 C5 已同步（多运行时脚本定案见总览 C5）：「方案本轮不定案」已更新为已定案口径——运行时多 classic 脚本 + 薄 bootstrap app.js + test-only 拼接 helper，拒绝单 bundle/拼接产物。2. 总览 C6 已同步（窄例外登记见总览 C6）：对 `apps/desktop/` 的排除已补注「本项仅 build.rs 资源清单窄范围接线例外」。3. §4 登记表 05 行状态与 §6.2 基线表的 desktop 轨道说明（`apps/desktop/src-tauri/build.rs` `TREE_DIRS` 窄接线）已随总览 C5 第 2 条④一并处理。4. 总览 §6.2 包名已更正为 `rsetup-next`（原 `-p rsetup-app` 为路径名笔误，目录名 ≠ 包名，见 §12），命令语义与总览基线一致，无「旧命令统一修」待办。

自检记录（撰写会话 + 第 2 轮修订会话 + 第 3 轮修订会话）：

- 链接：全部相对链接（app.js/index.html/i18n.js/hardening.test.mjs/server.rs/build.rs/Makefile/Cargo.toml/总览）逐一核验存在。
- 行数基线：app.js 3,928、i18n.js 1,596、index.html 419、server.rs 2,462、build.rs 315、tauri.conf.json 32（`wc -l` 实测）。
- 归属核对：182 个顶层 `function`/`async function` + **19 个**顶层 `const`（无顶层 `let`）= **201 个声明**逐名归入 §5 表，与 `grep -n '^function\|^async function\|^const'` 清单逐项比对，无遗漏、无重复归属。§4.2 各行数字 = 所承接原行号区间的行数（单段文件即区间长度：130/72/232/106/110/79/237/230/176/115/416/239/448/713/121/218/15；跨段文件按各段相加：render 9+95+9=113、hardware 3+135=138），最大者 app-install 713 < 900；18 片段 + 薄 app.js 的承接区间合计 **3,908 行**，与 3,928 的差额恰为区间之间那 **20 行单个空行**（实测逐处核对，全部为空行；第 3 行空行随 app-const 的 1–130 区间一并承接），即无遗漏、无重叠、无第三方归属。
- 加载图：§6.2 顺序 = §4.2 树列序 = §7 manifest 声明，三处一致（20 项：i18n 最先、app.js（init）最后、中间 18 片段）；与原始行号序之差恰为白名单 W1–W3。
- 接线表：§7 每行覆盖 URL/HTML/server 常量/server route/desktop 清单五列，无「待定」项；18 条新 URL 与 §4.2 的 18 个片段一一对应。
- 测试覆盖：§8 映射表覆盖全部 8 个读 app.js 的测试 + i18n.test.mjs（§2 注明其不引用 app.js、零改动）；§2 的 64 个 `handler()` 不同函数名（overlay 4/sources 5/hardening 13/storage 5/storage-refresh 7/system-install 38，跨文件去重 64）+ telemetry 双锚点 `renderCpuMetric`/`byteUnit` + 审计块尾锚 `renderActions` = 67 个被锚定函数，另加 `transport` 对象起点与审计块起点 2860 这 2 个非函数锚点（合计 69 个切片单元），跨文件切片 8 条（①–⑧，§8 第 1 条），与六处 `handler()`、双锚点、transport、审计块、IPC 扫描逐一对应。
- 文字：无 TODO/TBD、无凭据与 secret 内容、无尾随空白、标题 ≤3 级。
- 未验证范围（不作声称）：浏览器与 desktop 真机运行、Tauri 嵌入协议在运行时对 `modules/` 的实际加载（机制已由 build.rs/`tauri_build::build()` 源码定案，运行时行为仍未实测）、`cargo`/`npm`/`make`/`node --test` 各命令的实际执行结果（本任务未编译未测试）；§4.2 的行数与 §3.4 的最大值是逐字区间求和，实施后以真实文件行数为最终口径（均远离 900 阈值与例外线）。

## 16. 修订记录（逐项已修订，待复核）

### 16.1 第 2 轮修订记录

以下逐项对应审查意见；该轮只改本文件，未动源码/总览/其他子规格，未提交、未编译、未运行测试、未读 `secret/`；「已修订」= 文本层面的修正与实测锚点复核，**不表示任何测试或构建已通过**。

| 项 | 落点 |
| --- | --- |
| C1 | §2 新增 403–429 `request`（含 `x-rsetup-request` 硬化锚点）与声明总数盘点；§5 归属行改为 `transport`+`request`+`displayError` 同文件同序；§4.2 app-transport 目标 ≈230（按 205–436 逐字区间为 232）；§6.2 绑定唯一性、§13 C1 同步 |
| C2 | 4 处 `-p rsetup-app` 全部改为 `-p rsetup-next`（§9、§10 批 3、§12 两处）并加包名说明；删除 rsetup-next 命令上的错误 F2 前提，F2 仅保留在 controller/全量出口；§15 第 4 条登记总览笔误 |
| I1 | §3.3 改为「逐单元字节恒等 + 唯一归属 + 位移白名单」；§4.2 新增 W1–W3 白名单表与紧邻性硬约束；§8 第 1 条、§10 批 1 校验、§14 风险① 全部改用新契约（原「整串原序恒等」表述已删除）。另实测发现第 3 处位移 `hardwareReason`(438–440)（源自 §5 把它归入 app-hardware），已并入白名单为 W3，与审查意见的「2 处」之差在此登记待复核 |
| I2 | §8 第 1 条删除「20 个边界函数不得位于文件末」约束；§2 给出真实切片计数（26 个 handler 名 / 29 个被锚定函数 / 31 个切片单元）；补「片段边界零新增头尾文本 + 审计块整块随迁 + 文件末单元切片取到下一文件首个 function（仅空行数可差、断言不依赖）」，并点名 4 条真实跨文件切片与 4 个同位置文件末单元（第 3 轮更正注记：本条 26/29/31 计数与「4 条跨文件切片 + 4 个未被切到的文件末单元」已按复核实测更正为 64/67/69 与跨文件切片 8 条 ①–⑧（七个文件末函数均被切），见 §16.2 I1） |
| I3 | §4.2/§5/§8 删除 `renderNvmeDeviceCard` 「截到 EOF」说法，改为「2696–2857，切片终止于紧邻 app-install 的 `installTargetSelectable`(2875)」，并把 app-install 紧随 app-storage 写成硬约束 |
| I4 | §7 新增路径三段规范式与 handler 命名式；§8 (c) 改为逐条三元组断言（route↔规范 handler↔规范常量↔include_str 尾段）+ 反扫全部 `.js` route 无未登记/无遗漏 |
| I5 | §2 desktop 条目与 §9 desktop 轨道两处事实改正：tauri.conf.json:23 无 `script-src`，同源脚本由 `default-src 'self' tauri: asset:` 回退放行 |
| I6 | §8 一致性测试拆为「静态守恒组（批 1 入库生效）/接线组（批 2 生效）」并写死启用时点；b3 批 1 以原位旧 app.js 为基准、批 2 改写为入库顺序基线数组（顺序/归属/白名单继续锁定），不留大文件副本；§10 批次内容同步 |
| I7 | §9 改写 DOM harness：逐脚本装载 20 项、fail-closed stub、用 `vm.runInContext` 的完成值取回顶层 `init();` 的原始 promise 并 await（rejection 即失败、不二次调用、不改生产代码）、`setInterval` 两次（1,000ms 与 10,000ms）各恰一次证 init 单次、覆盖范围限于顶层 + init 直达链 |
| I8 | §2 desktop 条目与 §4.1/§14 风险③ 改为机制定案 + 真机验证义务（`copy_tree` 84–96、`publish` 140–153、`tauri_build::build()` 全树嵌入，与 fonts/assets 同机制）；`TREE_DIRS` 加 `"modules"` 为定案，`ROOT_FILES` 列名仅作说明性替代、默认不采用 |
| M1–M9 | M1 §2/§5/自检 19 个 const、201 声明；M2 §4.2/§5 format 改 442–547 不与 549 重叠；M3 §2/§4.2/§5 审计块整块 2859–2873；M4 §8 映射表与 §2 的真实切片计数（sources 5 / hardening 2 / storage-refresh 7 / system-install 6，38 为 install 单元数）、其余按实测保留（第 3 轮更正注记：hardening 实测 13——44 行动态 `map(handler)` + tools 数组 7 项 for 循环 + harness 传入 refreshAll/refreshOnce + 两字面量；system-install 实测 38——经 `INSTALL_FUNCTIONS.map(handler)`，38 恰是每次 createContext 的 handler 调用数，「38 不是调用数」为假断言已删除，见 §16.2 I1）；M5 §3.3/§8 删除「唯一差异是文件末截 EOF」；M6 §5 跨片段引用例子改为 `refreshAll`→`refreshStorageTool`/`renderAll`（`navigate`→`openSystemInstallPage` 归为反向引用）；M7 §2 route 147–150 与 handler 306–311、§7 引用同一锚点；M8 §10 revert 批 2 只回批 1、需再撤批 1 才回原始；M9 §8 第 4 条与映射表 IPC 行：扫描作用域 = 18 片段 + 薄 app.js 拼接串（与原全文等价），i18n 实测 0 处、清单仍须完整 20 项不得漏 |
| 顺带一致性 | 片段数从 19 修正为 18（脚本总数 20），§4.1 平铺备选改 22 项、§14⑤ 改首屏 20 请求；§4.2 行数列统一为逐字区间行数并令树列序 = 装载序；§5 修正 renderAll 750–758、sources 760–996、storage 2410–2857、install 2859–3571；§13 C4 最大值改 ≈713；§11 允许变化清单补入 4 个 `ui/` 新增文件；§15 增第 4 条 |

### 16.2 第 3 轮修订记录（本轮：2 项裁决 + 2 项小项，已修订，待复核）

本轮落实第 2 轮复核裁决，保持 18 运行时片段多脚本方案骨架不变；同前轮，只改本文件，未动源码/总览/其他子规格，未提交、未编译、未运行测试、未读 `secret/`；「已修订」不表示任何测试或构建已通过，不称 PASS。

| 项 | 落点 |
| --- | --- |
| C1（第 4 处位移关闭） | 第 1–2 行 `i18n`/`{t}` 从 app-state 改归 **app-const**：装载序不变、拼接序即恢复原行号序，**无需新增 W4**，白名单仍恰为 W1–W3，§3.3(c)、§8 (b4) 与自检「与原始行号序之差恰为 W1–W3」表述随之自洽；§4.2 树行数改 app-const 130（1–130，含第 3 行空行）/app-state 72（132–203）；§5 归属两行同步改写；「唯一跨脚本顶层求值」叙事改为 **app-const 首单元含 `window.RsetupI18n` 读取**（i18n.js 仍最先加载，§3 第 5 条、§6.1、§6.2 同步）；自检承接区间合计 3,908 行、差额 20 行空行、单段清单 130/72/… 同步 |
| I1（提取计数复核实测） | §2 第 1 条与自检测试覆盖行：handler **去重 64** 个不同函数（overlay 4/sources 5/**hardening 13**/storage 5/storage-refresh 7/**system-install 38**）；+`renderCpuMetric`/`byteUnit`/`renderActions` 锚定 = **67**；+2 个非函数锚点（`transport` 起点、审计块起点 2860）= 切片单元 **69**；hardening 13 的构成（44 行动态 `map(handler)`、tools 数组 7 项 for 循环、harness 传入 refreshAll/refreshOnce、字面 navigate/applyFanCurveStyles，不能只数字面量 `handler("`）写入 §2 与映射表；system-install 38 改按 `INSTALL_FUNCTIONS.map(handler)`（38 恰是每次 createContext 的 handler 调用数），删除「38 不是调用数」假断言（§16.1 M4 加更正注记）；§8 第 1 条改为**跨文件切片共 8 条 ①–⑧**（逐条含起止行号落点）并声明由 (i)(iii) + §4.2 装载序相邻 + (b4) 共同覆盖，「其余 22」改「其余 57」，删除「未被切到」假断言（四个文件末单元均被 tools 循环切到；`init` 至拼接串尾 = 原 EOF 语义等价、不计跨文件）；§8 映射表 sources/hardening（×2→×13 + 4 条跨边界依赖：`applyThermalPolicy`→`standaloneLeds`、`applySpiFlash`→`gpioFunction`、`applyLedConfiguration`→`applyStorageMetricStyles`、`applySourcePlan`→`hardwareReason`）/system-install（×6→×38）三行同步；§4.2 相邻性硬约束扩为 7 对、合并自由度必须保持上述相邻对；§14 风险①计数同步（§16.1 I2 加更正注记） |
| 小项① | §2 第 2 条与 §8 第 2 条 telemetry 切片口径统一：起锚 471、止锚命中 484 行行尾 `\n`（485 行首），切片内容为 471–484——含 `memoryUsagePercent` 全体、至 `byteUnit` 声明前的 `\n` 为止（原「471–483」「471→485 闭区间」两种口径统一） |
| 小项② | §8 第 3 条补明：system-install 自审的匹配文本含 `source` 标识符，hardening.test.mjs 迁移时须保留 `const source = loadRuntimeSource()` 命名 |
| I1 复核实测更正（本轮追加，仅数字口径） | 跨文件重名实测共 8 处——hardening 组的 beginApply/showApplyError/applyOverlays 与 overlay 组重合（此前漏计）、refreshAll/refreshOnce 与 storage-refresh 组重合、storage 组的 renderNvmeDeviceCard/renderStorageMmcCard/renderStorageTool 与 storage-refresh 组重合；72 名次去重后为 **64**（非 67）、+`renderCpuMetric`/`byteUnit`/`renderActions` 锚定 = **67**（非 70）、+2 个非函数锚点切片单元 = **69**（非 72）、§8 第 1 条「其余」= 64−7 = **57**（非 60）。文件级计数 4/5/13/5/7/38、hardening 13 的构成与 `INSTALL_FUNCTIONS` 38 均不变；同步落点为 §2 第 1 条（含重合口径补句）、§8 第 1 条、自检「测试覆盖」行、§16.1 I2 更正注记与本节 I1 行，其余文字、结构、8 条切片清单、W1–W3 与装载序未动。已修订，待复核，不称 PASS |

### 16.3 终审后清扫记录

| 项 | 落点 |
| --- | --- |
| 终审后清扫（已修订） | L7 限定例外块「总览 C5/C6 尚未同步该决定……由其他任务统一执行」时态句改为「总览 C5/C6 已同步（多运行时脚本定案与窄例外登记见总览 C5/C6）」；§15 四项改为已同步/已更正/已一并处理口径；§12 包名说明「总览 §6.2 写的 `-p rsetup-app` 是路径名笔误」改为「总览 §6.2 包名已更正为 `rsetup-next`」；§13 C6 自检「（§15 待同步）」改为「（总览 C6 已同步，窄例外登记见总览 C6）」。其余不动；未提交、未编译、未测试、未读 `secret/` |
| 终态更新（2026-10-26，已修订） | §15 标题「需总览统一同步事项（本任务不改总览）」改为「总览同步事项（已全部完成）」（§15 所列事项均已随总览修订落地）；其余不动；未提交、未编译、未测试、未读 `secret/` |
| 批次1实施勘误（已修订，2026-10-26，提交4156ede；app-hardware区间1094–1227与树注口径说明） | §4.2 树 app-hardware 行与 §5 表第二区间勘误为 1094–1227（实测 `renderHardwareError` 止于 1227、1228 为文件间空行、1229 为 `renderHardwareTool`；一致性测试 SEGMENTS 已用正确值，文件 138 行 = 3 + 1 空行 + 134）；§4.2 树注（modules/ 行括注）补口径说明：树注行数为单元区间行数之和，不含段间单空行，实施后以真实文件行数为最终口径（如 app-render 115=113+2 段间隙、app-hardware 138=137+1）。其余不动；未提交、未编译、未测试、未读 `secret/` |
| 批次2+3实施登记（已修订，2026-10-27，提交77de6c3；审查PASS，desktop真机冒烟归CI兜底） | 1. desktop-test环境限制：沙箱缺libdbus-1-dev/pkg-config（无root），已以真实build.rs（stub tauri_build::build）实跑替代验证——dist/modules 18片段与ui/modules逐字节一致、四根文件cmp一致、report_stale无陈旧项；tauri-codegen全树嵌入与桌面真机CSP行为未真机实测（§14风险③残余，归CI desktop-tauri-ipc作业/真机冒烟闭合）。2. handler迁移采用单行委托包装handler=(name)=>sharedHandler(source,name)保.map(handler)调用点零改动，审查判定合规（共享实现与原本地实现逐字等价）。3. desktop-ipc采用loadRuntimeSource()拼接串扫描（§8允许两式之一，i18n实测0处）。4. consistency (b3)基准改内联201单元顺序基线数组（=原行号序+W1–W3，与片段解析交叉验证），不留旧app.js副本；新dom-harness按§8设计落地（vm按序20脚本/取回init原promise await/setInterval恰两次/fail-closed Proxy）。其余不动；未提交、未编译、未测试、未读 `secret/` |

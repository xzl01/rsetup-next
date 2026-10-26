// ui/app.js 拆分一致性测试（规格 §8）。两组断言、按批次启用：
//   - 静态守恒组 (b1)–(b4)：批次 1 提交即入库并生效；批次 2 cutover 起旧 app.js
//     已是薄 bootstrap，(b3) 改以「内联 201 单元顺序基线」为基准（规格 §8：
//     顺序/归属/白名单三项继续被锁定），(b2) 的 201 名全集改由
//     「18 片段 + 薄 app.js」自身构成，(b4) 锁定拼接串顶层 init(); 恰一处。
//     首项 i18n.js 始终只做存在性/可解析性检查、不参与声明唯一性集合。
//   - 接线组 (a)(c)(d)：批次 2 cutover 提交内启用——(a) manifest ↔ HTML src、
//     (c) server.rs 逐条三元组（route ↔ 规范 handler ↔ 规范常量 ↔ include_str 尾段）
//     + 反扫 router() 全部 .js route、(d) desktop build.rs 清单。
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RUNTIME_SCRIPTS, loadRuntimeParts, loadRuntimeSource } from "./app-manifest.mjs";

const uiRoot = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.join(uiRoot, "..");

// 装载序（§6.2）中 i18n 与薄 app.js 之间的 18 个运行时片段，顺序即拼接/装载顺序。
const FRAGMENTS = RUNTIME_SCRIPTS.slice(1, -1);

// §7 路径三段规范式（唯一定义）：manifest 项 = 相对 ui/ 的 POSIX 路径、无前导斜杠；
// URL ≡ "/" + manifestPath；HTML src 必须逐字等于该 URL；
// include_str!("../../../ui/<p>") 去掉 "../../../ui/" 前缀后 ≡ p。
const INCLUDE_PREFIX = "../../../ui/";

// 既有根资源沿用现状名（script/i18n_script），不参与 §7 handler 命名式；
// 常量名仍与派生式一致（APP_JS/I18N_JS）。
const EXISTING_HANDLERS = new Map([
  ["app.js", "script"],
  ["i18n.js", "i18n_script"],
]);

// §7 handler 与常量的命名式：modules/app-const.js → APP_CONST_JS / app_const_script。
function expectedNames(p) {
  const stem = p.replace(/^modules\//, "").replace(/\.js$/, "");
  const snake = stem.replace(/-/g, "_");
  return { constant: `${snake.toUpperCase()}_JS`, handler: `${snake}_script` };
}

// 逐行解析顶层声明（行首 const/let/var/function/async function），返回 [{kind, name}]。
// 顶层 `let`/`var` 以 kind="other" 返回，供计数断言排除（§2：实测无顶层 let/var）。
function topLevelDecls(text) {
  const decls = [];
  for (const line of text.split("\n")) {
    let match = /^(?:async )?function ([\w$]+)/.exec(line);
    if (match) {
      decls.push({ kind: "function", name: match[1] });
      continue;
    }
    match = /^const ([\w$]+)\s*=/.exec(line);
    if (match) {
      decls.push({ kind: "const", name: match[1] });
      continue;
    }
    match = /^const \{ ([\w$]+) \}/.exec(line);
    if (match) {
      decls.push({ kind: "const", name: match[1] });
      continue;
    }
    if (/^let |^var /.test(line)) decls.push({ kind: "other", name: line });
  }
  return decls;
}

// (b3) 入库基线：manifest/装载序拼接出的 201 个顶层声明完整序列
// （§2/§5 全集 = 19 个 const + 182 个 function/async function）。
// 该序列 = 拆分前 app.js 的原行号序 + 白名单恰三处位移（§4.2 W1–W3，标注在对应
// 片段注释上）；批次 1 曾以原位旧 app.js 逐字节核对并锁定此序，批次 2 起以本数组
// 为基准继续锁定顺序/归属/白名单，不再读旧全量 app.js（§8）。
const UNIT_ORDER = [
  // modules/app-const.js（1–130）
  "i18n", "t", "routes", "workflowGroups", "quickActionIds", "socVendors", "capabilityVisuals", "hardwareToolIds", "debugDeviceProfiles", "defaultDebugCustom", "helpProfiles", "genericHelpResources", "helpFaqs", "contactChannels",
  // modules/app-state.js（132–203）
  "state", "$", "$$", "tauriInvoke",
  // modules/app-transport.js（205–436）
  "transport", "request", "displayError",
  // modules/app-format.js（442–547）
  "setText", "icon", "escapeHtml", "formatNumber", "formatPercent", "renderCpuMetric", "memoryUsagePercent", "byteUnit", "duration", "relativeTime", "riskLabel", "socVendor", "socDisplayName", "capabilityVisual", "helpProfile", "localizedDocsUrl",
  // modules/app-debug.js（549–658）
  "debugProfileById", "gpioProfileOverride", "loadDebugState", "saveDebugState", "applyDebugDevice", "renderDebugControls", "activateDebugProfile", "applyCustomDebugDevice",
  // modules/app-refresh.js（670–748）
  "openDialog", "dismissDialog", "refreshAll", "refreshOnce", "resolveSignals",
  // modules/app-render.js（750–758 + 998–1092 + 660–668）：W2 renderSnapshot、W1 applyStaticTranslations
  "renderAll", "renderSnapshot", "applyStaticTranslations",
  // modules/app-sources.js（760–996）
  "providerName", "providerLocation", "renderSources", "resetSourceBenchmark", "benchmarkSources", "renderSourceBenchmark", "benchmarkError", "renderProviderDetail", "clearSourcePlan", "renderSourcePlan", "previewSources", "applySourcePlan",
  // modules/app-hardware.js（438–440 + 1094–1227）：W3 hardwareReason 首单元
  "hardwareReason", "bindHardwareButtons", "hardwareToolCopy", "overlayDisplayCopy", "openHardwareTool", "closeHardwareTool", "renderHardwareError",
  // modules/app-overlay.js（1229–1458）
  "renderHardwareTool", "preserveToolFocus", "renderOverlayTool", "overlayReadControl", "bindOverlayRead", "authorizeOverlayRead", "previewOverlays", "renderOverlayPlan", "beginApply", "showApplyError", "applyOverlays",
  // modules/app-spi.js（1460–1635）
  "initializeSpiFlashState", "spiFlashRequest", "sameSpiFlashRequest", "resetSpiFlashPlan", "renderSpiFlashTool", "previewSpiFlash", "renderSpiFlashPlan", "applySpiFlash",
  // modules/app-gpio-video.js（1637–1751）
  "gpioFunction", "gpioSource", "gpioPinCell", "gpioPinDetail", "gpioConnectorView", "renderGpioTool", "renderVideoTool", "captureVideoFrame",
  // modules/app-thermal.js（1753–2168）
  "defaultFanCurveConfig", "initializeFanCurveState", "resetFanCurvePlan", "fanCurveRequest", "normalizedFanCurveRequest", "sameFanCurveRequest", "fanCurveValidationKey", "fanCurveChart", "renderThermalTool", "renderThermalPolicyPanel", "bindThermalPolicyPanel", "fanCurvePointRow", "renderFanCurvePanel", "invalidateFanCurvePreview", "updateFanCurveChart", "applyFanCurveStyles", "bindFanCurvePanel", "previewFanCurve", "renderFanCurvePlan", "applyFanCurve", "applyThermalPolicy",
  // modules/app-led.js（2170–2408）
  "standaloneLeds", "defaultRgbLedConfig", "preferredLedTrigger", "initializeLedState", "rgbLedHex", "renderLedTool", "renderStatusLedControls", "renderRgbLedControls", "bindLedControls", "resetLedConfirmation", "applyLedConfiguration",
  // modules/app-storage.js（2410–2694 + 2696–2857）
  "applyStorageMetricStyles", "renderStorageMmcCard", "refreshStorageTool", "renderStorageTool", "renderNvmeDeviceCard",
  // modules/app-install.js（2859–2873 + 2875–3571）
  "installTargetSelectable", "installSelectedTarget", "installKindLabel", "installFormatLabel", "installWarningCopy", "installErrorCodeList", "installErrorCopy", "installErrorFrom", "installPlanRequest", "installApplyRequest", "installPlanImageId", "installAcknowledgementReady", "installApplyDisabled", "installConfirmStateCopy", "installFact", "installTargetOptionLabel", "installTargetOptions", "installImageOptions", "installPickerAvailable", "installSelectableImages", "pickInstallImage", "installHandleProblem", "renderInstallTargetCard", "renderInstallConsequences", "renderInstallConfirmation", "renderInstallLiveBanner", "renderInstallErrorBox", "renderInstallResult", "renderSystemInstallBody", "renderInstallPickNotice", "renderSystemInstallPage", "invalidateInstallPlan", "wireInstallEvents", "onInstallAcknowledgementInput", "openSystemInstallPage", "loadInstallOptions", "planSystemInstall", "applySystemInstall",
  // modules/app-actions.js（3573–3693）
  "renderActions", "workflowButton", "operationButton", "renderActivity", "helpResourceUrl", "renderHelpResource", "renderContactLink", "renderContactLinks", "renderHelp", "openContact", "closeContact", "eventRow", "emptyRow", "emptyListItem",
  // modules/app-control.js（3695–3912）
  "navigate", "renderActionDetails", "openAction", "closeAction", "executeSelectedAction", "bindActionButtons", "openCommands", "renderCommandResults", "toast", "setTheme", "bindEvents", "startClock",
  // app.js（薄 bootstrap，3914–3928）
  "init",
];

test("(b1) 静态守恒：RUNTIME_SCRIPTS 20 项结构与 §6.2 装载序一致，路径存在且逐文件 node --check 可解析", () => {
  assert.equal(RUNTIME_SCRIPTS[0], "i18n.js");
  assert.equal(RUNTIME_SCRIPTS.at(-1), "app.js");
  assert.deepEqual(FRAGMENTS.map((p) => path.basename(p)), [
    "app-const.js", "app-state.js", "app-transport.js", "app-format.js",
    "app-debug.js", "app-refresh.js", "app-render.js", "app-sources.js",
    "app-hardware.js", "app-overlay.js", "app-spi.js", "app-gpio-video.js",
    "app-thermal.js", "app-led.js", "app-storage.js", "app-install.js",
    "app-actions.js", "app-control.js",
  ]);
  assert.equal(RUNTIME_SCRIPTS.length, 20);
  for (const relativePath of RUNTIME_SCRIPTS) {
    assert.match(relativePath, /^(?:i18n\.js|app\.js|modules\/app-[a-z-]+\.js)$/);
    assert.ok(fs.existsSync(path.join(uiRoot, relativePath)), relativePath);
    execFileSync(process.execPath, ["--check", path.join(uiRoot, relativePath)]);
  }
});

test("(b2) 静态守恒：顶层绑定跨文件零重复；18 片段 + 薄 app.js 恰构成 201 名全集（19 const + 182 function）", () => {
  const parts = loadRuntimeParts();
  assert.equal(parts.length, 19, "i18n 之后的运行时脚本应为 18 片段 + 薄 app.js");
  const seen = new Map();
  let constCount = 0;
  let functionCount = 0;
  for (const [file, text] of parts) {
    for (const { kind, name } of topLevelDecls(text)) {
      assert.notEqual(kind, "other", `发现计划外的顶层 let/var 声明：${file} ${name}`);
      if (kind === "const") constCount += 1;
      else functionCount += 1;
      const key = `${kind}:${name}`;
      assert.ok(!seen.has(key), `顶层绑定 ${name} 重复声明：${seen.get(key) ?? ""} 与 ${file}`);
      seen.set(key, file);
    }
  }
  assert.equal(constCount, 19, "§2/§5：const 全集 19 名（12 常量 + i18n + t + state + $ + $$ + tauriInvoke + transport）");
  assert.equal(functionCount, 182, "§2/§5：顶层 function 全集 182 名（含薄 app.js 的 init）");
  assert.equal(seen.size, 201, "§2：201 个顶层声明全集，跨文件零重复");

  // 薄 bootstrap 只贡献 `init` 一个声明（§3 第 5 条）。
  const [, bootstrap] = parts.at(-1);
  assert.equal(parts.at(-1)[0], "app.js");
  assert.deepEqual(topLevelDecls(bootstrap), [{ kind: "function", name: "init" }]);
});

test("(b3) 静态守恒：拼接串顶层声明序列 === 内联 201 单元顺序基线（原行号序 + 仅 W1–W3）", () => {
  const actual = topLevelDecls(loadRuntimeSource()).map(({ name }) => name);
  assert.deepEqual(actual, UNIT_ORDER, "顺序/归属/白名单被锁定：拼接串单元序列 ≠ 内联基线");
  assert.equal(UNIT_ORDER.length, 201);
});

test("(b4) 静态守恒：init 唯一调用点——拼接串以顶层 init(); 结尾且全文恰一处", () => {
  const source = loadRuntimeSource();
  assert.match(source, /\ninit\(\);\n$/, "拼接串必须以顶层 init(); 结尾（唯一入口，最后加载）");
  assert.equal((source.match(/^init\(\);$/gm) ?? []).length, 1, "顶层 init(); 调用点必须恰一处");
});

test("(a) 接线：index.html 的 script 序列 === RUNTIME_SCRIPTS 加前导斜杠逐字相等", () => {
  const html = fs.readFileSync(path.join(uiRoot, "index.html"), "utf8");
  const tags = [...html.matchAll(/<script\b[^>]*>/g)].map((match) => match[0]);
  const sources = tags
    .map((tag) => /\bsrc="([^"]+)"/.exec(tag)?.[1])
    .filter((src) => src !== undefined && src.startsWith("/") && src.endsWith(".js"));
  // HTML src ≡ "/" + manifest 路径（§7 规范式）：逐项、按序、逐字相等。
  assert.deepEqual(sources, RUNTIME_SCRIPTS.map((p) => `/${p}`));
  for (const tag of tags.filter((tag) => /\.js"/.test(tag))) {
    assert.match(tag, /\bdefer\b/, `运行时脚本必须 defer：${tag}`);
    assert.doesNotMatch(tag, /\btype\s*=/, `运行时脚本必须 classic：${tag}`);
  }
});

test("(c) 接线：server.rs 与 manifest 逐条三元组一致，router() 的 .js 路由反扫恰等于清单", () => {
  const rust = fs.readFileSync(path.join(repoRoot, "crates/rsetup-app/src/server.rs"), "utf8");
  const routerStart = rust.indexOf("pub fn router(");
  assert.ok(routerStart >= 0, "router() 必须存在");
  const routerBody = rust.slice(routerStart, rust.indexOf("\n}\n", routerStart));
  assert.ok(routerBody.length > 0);

  const routes = new Map();
  for (const match of routerBody.matchAll(/\.route\(\s*"([^"]+)"\s*,\s*get\(([a-z_0-9]+)\)\s*\)/g)) {
    assert.ok(!routes.has(match[1]), `route 重复注册：${match[1]}`);
    routes.set(match[1], match[2]);
  }
  // (iv) 反扫 router() 内全部 .js route：逐条落在 "/" + RUNTIME_SCRIPTS 集合内，无未登记、无遗漏。
  assert.deepEqual(
    [...routes.keys()].filter((url) => url.endsWith(".js")).sort(),
    RUNTIME_SCRIPTS.map((p) => `/${p}`).sort(),
  );

  // (i)(ii)(iii) 逐条三元组：URL ↔ 规范 handler ↔ 规范常量 ↔ include_str 尾段。
  for (const p of RUNTIME_SCRIPTS) {
    const url = `/${p}`;
    const handler = routes.get(url);
    assert.ok(handler, `router() 缺少 route：${url}`);
    const expected = expectedNames(p);
    if (EXISTING_HANDLERS.has(p)) {
      assert.equal(handler, EXISTING_HANDLERS.get(p), `${url} 既有 handler 沿用现状名`);
    } else {
      assert.equal(handler, expected.handler, `${url} handler 命名式（§7）`);
    }
    const bodyMatch = new RegExp(`async fn ${handler}\\(\\) -> impl IntoResponse \\{([\\s\\S]*?)\\n\\}`).exec(rust);
    assert.ok(bodyMatch, `handler 定义缺失：${handler}`);
    assert.match(
      bodyMatch[1],
      /\[\(header::CONTENT_TYPE, "text\/javascript; charset=utf-8"\)\]/,
      `${handler} 必须返回 text/javascript; charset=utf-8`,
    );
    const constants = bodyMatch[1].match(/\b[A-Z0-9_]+_JS\b/g) ?? [];
    assert.deepEqual(constants, [expected.constant], `${handler} 必须逐字返回规范常量（§7 命名式）`);
    const includeMatch = new RegExp(`const ${expected.constant}: &str = include_str!\\("([^"]+)"\\);`).exec(rust);
    assert.ok(includeMatch, `常量 ${expected.constant} 必须经 include_str! 内嵌`);
    assert.ok(includeMatch[1].startsWith(INCLUDE_PREFIX), `常量 ${expected.constant} 的 include_str 前缀`);
    assert.equal(includeMatch[1].slice(INCLUDE_PREFIX.length), p, `常量 ${expected.constant} 的 include_str 尾段`);
  }
});

test("(d) 接线：desktop build.rs 的 TREE_DIRS 含 modules、ROOT_FILES 恰为四个既有根文件", () => {
  const build = fs.readFileSync(path.join(repoRoot, "apps/desktop/src-tauri/build.rs"), "utf8");
  const treeMatch = /const TREE_DIRS: \[&str; (\d+)\] = \[([^\]]*)\];/.exec(build);
  assert.ok(treeMatch, "TREE_DIRS 定义缺失");
  const treeDirs = (treeMatch[2].match(/"([^"]+)"/g) ?? []).map((item) => item.slice(1, -1));
  assert.ok(treeDirs.includes("modules"), "TREE_DIRS 必须含 \"modules\"（递归复制、保留相对路径）");
  assert.equal(Number(treeMatch[1]), treeDirs.length, "TREE_DIRS 声明长度必须等于条目数");
  const rootMatch = /const ROOT_FILES: \[&str; (\d+)\] = \[([^\]]*)\];/.exec(build);
  assert.ok(rootMatch, "ROOT_FILES 定义缺失");
  const rootFiles = (rootMatch[2].match(/"([^"]+)"/g) ?? []).map((item) => item.slice(1, -1));
  assert.deepEqual([...rootFiles].sort(), ["app.js", "i18n.js", "index.html", "styles.css"]);
  assert.equal(Number(rootMatch[1]), rootFiles.length, "ROOT_FILES 声明长度必须等于条目数");
});

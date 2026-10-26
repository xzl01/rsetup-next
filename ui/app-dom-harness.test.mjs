// 全片段按序装载的 DOM 冒烟测试（规格 §9）。现有 8 个测试只做单片段 vm 切片执行，
// 存在跨片段绑定、加载顺序、init 恰一次、重复 const 的盲点。本测试在同一个 vm 上下文
// 里按 RUNTIME_SCRIPTS 顺序先跑 i18n.js、再用 loadRuntimeParts() 依次执行那 19 项
// （18 片段 + 薄 app.js），模拟浏览器逐 classic 脚本装载：任一脚本读取未定义标识符
// 都会 ReferenceError 失败；重复 const 会在装载期 SyntaxError 失败。
// 覆盖范围 = 20 个脚本的顶层执行 + init() 的直达调用链（init → loadDebugState/
// applyStaticTranslations/setTheme/bindEvents/startClock/refreshAll 第一层；
// location.hash 为空串，navigate 不被触达），不声称覆盖事件回调内部的分支链。
// DOM stub 一律 fail-closed：未预期的方法/属性访问与未登记的查询都直接 throw。
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { RUNTIME_SCRIPTS, loadRuntimeParts } from "./app-manifest.mjs";

const uiRoot = path.dirname(fileURLToPath(import.meta.url));

function makeDom() {
  const events = [];      // 形如 "label|type" 的 addEventListener 注册记录
  const intervals = [];   // setInterval 的毫秒数，按调用顺序
  const timeouts = [];    // setTimeout 的毫秒数（回调一律不触发）
  const fetchCalls = [];  // { path, headers }
  const storage = new Map();
  const children = [];    // 所有 append() 进去的子节点
  const created = [];     // document.createElement 产出的节点

  // fail-closed 守卫：白名单之外的属性读/写直接 throw（symbol 透传，保证内部机制可用）。
  const guarded = (label, target, allowed) => {
    const allow = new Set(allowed);
    return new Proxy(target, {
      get(t, key) {
        if (typeof key === "symbol") return Reflect.get(t, key);
        if (allow.has(key)) return Reflect.get(t, key);
        throw new Error(`[${label}] unexpected property read: ${String(key)}`);
      },
      set(t, key, value) {
        if (allow.has(key)) return Reflect.set(t, key, value);
        throw new Error(`[${label}] unexpected property write: ${String(key)}`);
      },
      has(t, key) { return Reflect.has(t, key); },
    });
  };

  const table = new Map(); // 选择器 → 节点数组（显式登记的桩 DOM；未登记即 throw）

  function makeNode(label, datasetValues = {}) {
    const attributes = new Map();
    const own = [];
    const backing = {
      textContent: "",
      className: "",
      innerHTML: "",
      lang: "",
      dataset: { ...datasetValues },
      setAttribute: (name, value) => { attributes.set(name, String(value)); },
      getAttribute: (name) => attributes.get(name) ?? null,
      addEventListener: (type) => { events.push(`${label}|${type}`); },
      append: (child) => { children.push(child); own.push(child); },
      remove: () => {},
      classList: { add() {}, remove() {}, toggle() {}, contains: () => false },
    };
    const node = guarded(label, backing, [
      "textContent", "className", "innerHTML", "lang", "dataset",
      "setAttribute", "getAttribute", "addEventListener", "append", "remove", "classList",
    ]);
    return { node, label, attributes, children: own };
  }

  function register(selector, count = 0, datasetValues = {}) {
    const entries = Array.from({ length: count }, () => makeNode(selector, datasetValues));
    table.set(selector, entries);
    return entries;
  }

  const querySelector = (selector) => {
    if (!table.has(selector)) throw new Error(`document 收到未登记的选择器（querySelector）: ${selector}`);
    return table.get(selector)[0]?.node ?? null;
  };
  const querySelectorAll = (selector) => {
    if (!table.has(selector)) throw new Error(`document 收到未登记的选择器（querySelectorAll）: ${selector}`);
    return table.get(selector).map((entry) => entry.node);
  };

  const body = makeNode("body");
  const documentElement = makeNode("documentElement");
  const document = guarded("document", {
    title: "",
    body: body.node,
    documentElement: documentElement.node,
    querySelector,
    querySelectorAll,
    createElement: (tagName) => {
      const entry = makeNode(`created:${tagName}`);
      created.push(entry);
      return entry.node;
    },
    addEventListener: (type) => { events.push(`document|${type}`); },
  }, ["title", "body", "documentElement", "querySelector", "querySelectorAll", "createElement", "addEventListener"]);

  const window = guarded("window", {
    RsetupI18n: undefined,
    __TAURI__: undefined,
    setInterval: (_callback, ms) => { intervals.push(ms); return intervals.length; },
    setTimeout: (_callback, ms) => { timeouts.push(ms); return timeouts.length; },
    addEventListener: (type) => { events.push(`window|${type}`); },
    dispatchEvent: () => true,
  }, ["RsetupI18n", "__TAURI__", "setInterval", "setTimeout", "addEventListener", "dispatchEvent"]);

  const localStorage = guarded("localStorage", {
    getItem: (key) => storage.get(key) ?? null,
    setItem: (key, value) => { storage.set(key, String(value)); },
    removeItem: (key) => { storage.delete(key); },
  }, ["getItem", "setItem", "removeItem"]);

  // fetch 一律以网络失败拒绝：init 直达链走 request → transport 失败归一路径
  // （x-rsetup-request 硬化头在拒绝前已被记录并断言）。
  const fetch = (url, options) => {
    fetchCalls.push({ path: url, headers: options?.headers ?? {} });
    return Promise.reject(new TypeError("offline stub"));
  };

  // —— 显式声明的桩 DOM：init 直达链查询到的每个选择器都必须在此登记 ——
  const metaDescription = register("meta[name='description']", 1)[0];
  const i18nText = register("[data-i18n]", 1, { i18n: "brand.channel" })[0];
  register("[data-i18n-aria]", 1, { i18nAria: "nav.label" });
  register("[data-i18n-alt]", 1, { i18nAlt: "brand.open" });
  register("[data-i18n-placeholder]", 1, { i18nPlaceholder: "command.placeholder" });
  const languageLabel = register("[data-language-label]", 1)[0];
  register("[data-clock]", 1);
  const footerStatus = register("[data-footer-status]", 1)[0];
  const footerDetail = register("[data-footer-detail]", 1)[0];
  const toasts = register("[data-toasts]", 1)[0];
  // bindEvents 逐一 $()/$$() 查询的控件；[data-install-refresh] 用可选链，登记为空（querySelector → null）。
  for (const selector of [
    "[data-route]", "[data-refresh]", "[data-contact-open]", "[data-command-trigger]",
    "[data-command-input]", "[data-command-dialog]", "[data-language-toggle]", "[data-theme-toggle]",
    "[data-contact-close]", "[data-contact-dialog]", "[data-task-close]", "[data-task-drawer]",
    "[data-task-confirm]", "[data-task-execute]", "[data-hardware-close]", "[data-hardware-drawer]",
    "[data-source-provider]", "[data-source-preview]", "[data-source-benchmark]", "[data-source-benchmark-stop]",
    "[data-source-benchmark-results]", "[data-source-confirm]", "[data-source-apply]",
    "[data-debug-profile]", "[data-debug-apply]", "[data-debug-reset]",
  ]) register(selector, 1);
  register("[data-install-refresh]", 0);

  const location = guarded("location", { hash: "" }, ["hash"]);
  const navigator = guarded("navigator", { language: "en-US", languages: ["en-US"] }, ["language", "languages"]);
  const history = guarded("history", {}, []);

  const context = vm.createContext({ window, document, location, history, localStorage, navigator, fetch });

  return {
    context, window, document, storage, events, intervals, timeouts, fetchCalls,
    nodes: { body, documentElement, metaDescription, i18nText, languageLabel, footerStatus, footerDetail, toasts },
    children, created,
  };
}

test("20 个运行时脚本按 §6.2 顺序在 fail-closed DOM stub 上装载，init 直达链恰执行一次", async () => {
  const dom = makeDom();

  // 逐脚本装载：i18n.js 最先（app-const 首单元读 window.RsetupI18n，现状跨文件顶层求值）。
  assert.equal(RUNTIME_SCRIPTS[0], "i18n.js");
  vm.runInContext(fs.readFileSync(path.join(uiRoot, "i18n.js"), "utf8"), dom.context, { filename: "i18n.js" });
  const i18n = dom.window.RsetupI18n;
  assert.ok(i18n && typeof i18n.t === "function", "i18n.js 必须导出 window.RsetupI18n");
  assert.equal(typeof dom.window.__TAURI__, "undefined", "浏览器分支：transport 必须走 fetch");

  // 18 片段 + 薄 app.js：同一个上下文按序 runInContext，重复 const 会在装载期 SyntaxError。
  const parts = loadRuntimeParts();
  assert.equal(parts.length, 19);
  let bootstrapCompletion;
  for (const [relativePath, text] of parts) {
    bootstrapCompletion = vm.runInContext(text, dom.context, { filename: relativePath });
  }

  // 薄 app.js 的最后一条语句就是顶层 init();，经典脚本的完成值即该调用的原始 promise：
  // 直接 await 它，rejection 即判失败（不二次调用、不 catch 吞掉、不改生产代码）。
  await bootstrapCompletion;

  // init 恰执行一次的证据：setInterval 恰两次——startClock 的 1,000ms tick 与
  // init 末尾的 10,000ms refreshAll 各恰一次。
  assert.deepEqual(dom.intervals, [1000, 10000], "setInterval 必须恰被调用两次（1000ms/10000ms 各一次）");
  // 配合 bindEvents 内 addEventListener 注册次数的相等式（顺序 + 次数逐一相等）。
  assert.deepEqual(dom.events, [
    "[data-route]|click", "[data-refresh]|click", "[data-command-trigger]|click", "[data-command-input]|input",
    "[data-command-dialog]|cancel", "[data-language-toggle]|click", "[data-theme-toggle]|click",
    "[data-contact-open]|click", "[data-contact-close]|click", "[data-contact-dialog]|cancel",
    "[data-task-close]|click", "[data-task-drawer]|cancel", "[data-task-confirm]|change", "[data-task-execute]|click",
    "[data-hardware-close]|click", "[data-hardware-drawer]|cancel", "[data-source-provider]|change",
    "[data-source-preview]|click", "[data-source-benchmark]|click", "[data-source-benchmark-stop]|click",
    "[data-source-benchmark-results]|click", "[data-source-confirm]|change", "[data-source-apply]|click",
    "[data-debug-profile]|change", "[data-debug-apply]|click", "[data-debug-reset]|click",
    "document|click", "window|rsetup:locale", "window|keydown",
  ], "addEventListener 注册序列必须与 bindEvents + document/window 的恰一组注册逐项相等");

  // init 直达链的行为断言：翻译、主题、失败归一与提示。
  assert.equal(dom.document.title, i18n.t("document.title"));
  assert.equal(dom.nodes.metaDescription.attributes.get("content"), i18n.t("document.description"));
  assert.equal(dom.nodes.i18nText.node.textContent, i18n.t("brand.channel"));
  assert.equal(dom.nodes.languageLabel.node.textContent, i18n.t("language.short"));
  assert.equal(dom.nodes.documentElement.node.dataset.theme, "light");
  assert.equal(dom.storage.get("rsetup-theme-v2"), "light");
  assert.equal(dom.nodes.body.node.dataset.state, "error", "fetch 失败归一后 body[data-state] 必须是 error");
  assert.equal(dom.nodes.footerStatus.node.textContent, i18n.t("status.unavailable"));
  assert.equal(dom.nodes.footerDetail.node.textContent, i18n.t("api.transport_failure"));
  assert.equal(dom.children.length, 1, "toast 必须恰追加一条");
  assert.match(dom.created[0].node.innerHTML, new RegExp(i18n.t("toast.refreshFailed")));

  // 第一层刷新经 request 发源，每个请求注入 x-rsetup-request 硬化头。
  assert.deepEqual(dom.fetchCalls.map((call) => call.path), [
    "/api/v1/snapshot", "/api/v1/actions", "/api/v1/activity", "/api/v1/sources",
  ]);
  for (const call of dom.fetchCalls) {
    assert.equal(call.headers["x-rsetup-request"], "1", `${call.path} 必须携带 x-rsetup-request 硬化头`);
  }
  // setTimeout 记录存在但回调不触发（toast 的 4,200ms 自动移除），保证确定性。
  assert.ok(dom.timeouts.every((ms) => typeof ms === "number"));
});

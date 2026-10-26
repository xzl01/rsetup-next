import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { handler as sharedHandler, loadRuntimeSource } from "./app-manifest.mjs";

// 自审断言（system-install.test.mjs 按文本匹配）依赖 `source` 标识符名，迁移时保持不变。
const source = loadRuntimeSource();
const handler = (name) => sharedHandler(source, name);
function harness(names) {
  const nodes = new Map();
  function node(selector) {
    if (!nodes.has(selector)) nodes.set(selector, {
      disabled: false, checked: true, hidden: true, textContent: "", dataset: {},
      classList: { error: false, add() {}, remove() {}, toggle(_, value) { this.error = value; } },
      span: { textContent: "" },
    });
    return nodes.get(selector);
  }
  const c = {
    state: {
      selectedHardware: "device-tree", hardwareLoadVersion: 1,
      overlayPlan: { selectedIds: ["uart.dtbo"], planToken: "token" },
      thermalPolicy: "step_wise",
      fanCurvePlan: { request: { enabled: false }, planToken: "token" },
      spiFlashPlan: { request: { operation: "erase" }, target: { path: "/dev/mtd0" }, planToken: "token" },
      rgbLedConfig: { mode: "solid" }, ledSelection: { trigger: "heartbeat", ledId: "status" },
      sourcePlan: { provider: { id: "cqu" }, planToken: "token", changes: [{}] },
      selectedAction: { id: "ssh.enable", available: true, risk: "guarded", steps: [] },
    },
    transport: {}, $: (selector, parent) => selector === "span" && parent ? parent.span : node(selector),
    t: (key) => key, displayError: (error) => error.message,
    i18n: { action: (a) => a, runSummary: () => "summary", getLocale: () => "en" },
    sameFanCurveRequest: () => true, sameSpiFlashRequest: () => true, spiFlashRequest: () => ({}),
    refreshCalls: [],
    refreshAll: async (options) => { c.refreshCalls.push(options); }, toast() {},
    document: { body: { dataset: {} } }, setText() {}, resolveSignals() {},
  };
  vm.createContext(c);
  vm.runInContext(["beginApply", "showApplyError", ...names].map(handler).join("\n"), c);
  return { c, node };
}
const tools = [
  ["applyOverlays", "applyOverlays", "overlay", "overlay.apply"],
  ["applyThermalPolicy", "applyThermalPolicy", "thermal", "thermal.apply"],
  ["applySpiFlash", "applySpiFlash", "spi", "spiFlash.apply.erase"],
  ["applyFanCurve", "applyFanCurve", "fan", "fanCurve.disable"],
  ["applyLedConfiguration", "applyLedTrigger", "led", "led.applyTrigger"],
  ["applySourcePlan", "applySources", "source", "sources.apply"],
  ["executeSelectedAction", "runAction", "task", "drawer.run"],
];
for (const [name, transport, prefix, label] of tools) {
  for (const cancel of [false, true]) {
    test(name + (cancel ? " clears authorization after cancellation" : " completes dry-run and can be re-confirmed"), async () => {
      const { c, node } = harness([name]);
      const button = node(prefix === "task" ? "[data-task-execute]" : "[data-" + prefix + "-apply]");
      button.dataset.ledApply = "trigger";
      const confirmation = node("[data-" + prefix + "-confirm]");
      const result = node("[data-" + prefix + "-result]");
      c.transport[transport] = async () => {
        if (cancel) throw Object.assign(new Error("canceled"), { code: "authorization_canceled" });
        const run = { synthetic: true, status: "planned" };
        return ["applyThermalPolicy", "applyLedTrigger", "runAction"].includes(transport)
          ? run : { run, backups: [], rolledBack: false };
      };
      await c[name]({ currentTarget: button });
      if (!cancel) {
        assert.ok(
          c.refreshCalls.some((call) => call && call.invalidate === true),
          `${name} must call refreshAll with invalidate: true`
        );
      }
      assert.equal(button.span.textContent, label);
      assert.equal(button.disabled, true);
      assert.equal(button.rsetupRunning, false);
      assert.equal(confirmation.checked, false);
      assert.equal(confirmation.disabled, false);
      if (cancel) {
        assert.equal(result.classList.error, false);
        assert.equal(result.textContent, "api.authorization_canceled");
      }
      // A fresh acknowledgement permits another attempt, without reopening the tool.
      confirmation.checked = true;
      button.disabled = false;
      await c[name]({ currentTarget: button });
      assert.equal(button.rsetupRunning, false);
    });
  }
}
test("duplicate apply clicks do not submit a second operation", async () => {
  const { c } = harness(["applyOverlays"]);
  let resolve, calls = 0;
  c.transport.applyOverlays = () => { calls++; return new Promise((done) => { resolve = done; }); };
  const pending = c.applyOverlays();
  await c.applyOverlays();
  assert.equal(calls, 1);
  resolve({ run: { synthetic: true } });
  await pending;
});

test("queued refresh supersedes a pre-change snapshot and callers await it", async () => {
  const { c } = harness(["refreshAll", "refreshOnce"]);
  const requests = [], rendered = [];
  c.state = {};
  c.applyDebugDevice = (snapshot) => snapshot;
  c.renderAll = () => rendered.push(c.state.snapshot.id);
  c.transport = {
    snapshot: () => new Promise((done) => requests.push(done)),
    actions: async () => [], activity: async () => [],
    sourceStatus: async () => ({ sourceRevision: "same" }),
  };
  const first = c.refreshAll();
  const second = c.refreshAll({ quiet: true, invalidate: true });
  requests[0]({ id: "before", synthetic: true });
  for (let i = 0; i < 10 && requests.length < 2; i++) await Promise.resolve();
  assert.equal(requests.length, 2);
  assert.deepEqual(rendered, []);
  requests[1]({ id: "after", synthetic: true });
  await Promise.all([first, second]);
  assert.deepEqual(rendered, ["after"]);
  assert.equal(c.state.refreshing, false);
});

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

test("obsolete refresh errors are discarded", async () => {
  const { c } = harness(["refreshAll", "refreshOnce"]);
  const requests = [], rendered = [], toasts = [];
  c.state = {};
  c.applyDebugDevice = value => value;
  c.renderAll = () => rendered.push(c.state.snapshot.id);
  c.toast = (title, detail, error) => toasts.push({ title, detail, error });
  c.transport = {
    snapshot: () => new Promise((resolve, reject) => requests.push({ resolve, reject })),
    actions: async () => [], activity: async () => [],
    sourceStatus: async () => ({ sourceRevision: "same" }),
  };
  const first = c.refreshAll();
  const invalidated = c.refreshAll({ quiet: true, invalidate: true });
  // Reject A (obsolete epoch)
  requests[0].reject(new Error("network failure in A"));
  for (let i = 0; i < 20 && requests.length < 2; i++) await Promise.resolve();
  assert.equal(requests.length, 2);
  assert.deepEqual(toasts, []);
  assert.notEqual(c.document.body.dataset.state, "error");

  // Resolve B
  requests[1].resolve({ id: "snapshot-b", synthetic: true });
  await Promise.all([first, invalidated]);
  assert.deepEqual(rendered, ["snapshot-b"]);
  assert.equal(c.document.body.dataset.state, "demo");
  assert.equal(c.state.refreshing, false);
});

test("multiple invalidations coalesce but later changes reread", async () => {
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
  // A starts
  const pA = c.refreshAll();
  assert.equal(requests.length, 1);

  // Two invalidations while A is running coalesce into B
  const pB1 = c.refreshAll({ quiet: true, invalidate: true });
  const pB2 = c.refreshAll({ quiet: true, invalidate: true });
  requests[0]({ id: "A", synthetic: true }); // Finish A

  for (let i = 0; i < 20 && requests.length < 2; i++) await Promise.resolve();
  assert.equal(requests.length, 2); // B started

  // Invalidation while B is running triggers C
  const pC = c.refreshAll({ quiet: true, invalidate: true });
  requests[1]({ id: "B", synthetic: true }); // Finish B

  for (let i = 0; i < 20 && requests.length < 3; i++) await Promise.resolve();
  assert.equal(requests.length, 3); // C started
  requests[2]({ id: "C", synthetic: true }); // Finish C

  await Promise.all([pA, pB1, pB2, pC]);
  assert.deepEqual(rendered, ["C"]);
  assert.equal(c.state.refreshing, false);
});

test("latest failure clears drain and allows retry", async () => {
  const { c } = harness(["refreshAll", "refreshOnce"]);
  const requests = [], rendered = [], toasts = [];
  c.state = {};
  c.applyDebugDevice = value => value;
  c.renderAll = () => rendered.push(c.state.snapshot.id);
  c.toast = (title, detail, error) => toasts.push({ title, detail, error });
  c.transport = {
    snapshot: () => new Promise((resolve, reject) => requests.push({ resolve, reject })),
    actions: async () => [], activity: async () => [],
    sourceStatus: async () => ({ sourceRevision: "same" }),
  };
  // Latest batch fails
  const pFail = c.refreshAll();
  requests[0].reject(new Error("injected refresh failure"));
  await pFail;

  assert.equal(c.state.refreshPromise, null);
  assert.equal(c.document.body.dataset.state, "error");
  assert.equal(toasts.length, 1);
  assert.equal(toasts[0].error, true);

  // Retry succeeds
  const pRetry = c.refreshAll();
  for (let i = 0; i < 20 && requests.length < 2; i++) await Promise.resolve();
  assert.equal(requests.length, 2);
  requests[1].resolve({ id: "retry-success", synthetic: true });
  await pRetry;

  assert.equal(c.document.body.dataset.state, "demo");
  assert.deepEqual(rendered, ["retry-success"]);
  assert.equal(c.state.refreshPromise, null);
});

test("manual refresh joins pending polling", async () => {
  const { c } = harness(["refreshAll", "refreshOnce"]);
  const requests = [], rendered = [], toasts = [];
  c.state = {};
  c.applyDebugDevice = value => value;
  c.renderAll = () => rendered.push(c.state.snapshot.id);
  c.toast = (title, detail, error) => toasts.push({ title, detail, error });
  c.transport = {
    snapshot: () => new Promise(resolve => requests.push(resolve)),
    actions: async () => [], activity: async () => [],
    sourceStatus: async () => ({ sourceRevision: "same" }),
  };
  // Polling starts (quiet)
  const pollPromise = c.refreshAll({ quiet: true });
  assert.equal(c.state.refreshLoud, false);

  // Manual refresh joins (not quiet)
  const manualPromise = c.refreshAll();
  assert.equal(c.state.refreshLoud, true);

  // Only one transport request made
  assert.equal(requests.length, 1);
  requests[0]({ id: "joined", synthetic: true });

  await Promise.all([pollPromise, manualPromise]);
  assert.deepEqual(rendered, ["joined"]);
  assert.equal(requests.length, 1);
  assert.equal(c.state.refreshPromise, null);
  assert.equal(c.state.refreshing, false);
});

test("runtime markup contains no CSP-blocked style attributes", () => {
  assert.doesNotMatch(source, /\sstyle="/);
  assert.match(source, /escapeHtml\(frame.base64\)/);
  assert.match(source, /"x-rsetup-request": "1"/);
});

test("a short route rail scrolls internally and a mobile rail has one column per route", () => {
  const styles = fs.readFileSync(new URL("./styles.css", import.meta.url), "utf8");
  const nav = fs.readFileSync(new URL("./index.html", import.meta.url), "utf8");
  const rail = styles.match(/\.route-rail\s*\{([\s\S]*?)\}/);
  assert.ok(rail, "the fixed route rail must exist");
  assert.match(rail[1], /overflow-y:\s*auto/, "a short viewport must scroll the rail instead of clipping entries");
  assert.match(rail[1], /overscroll-behavior:\s*contain/, "rail scrolling must not chain to the workspace");

  const routeCount = (nav.match(/class="route-button/g) || []).length;
  assert.equal(routeCount, 8);
  const mobile = styles.match(/@media \(max-width: 780px\) \{([\s\S]*?)\n\}/);
  assert.ok(mobile, "the mobile bottom rail must exist");
  assert.match(mobile[1], /grid-template-columns:\s*repeat\(8, minmax\(0, 1fr\)\)/,
    "every visible route needs one bottom-rail column");
});

test("navigation brings the active route into view in a short rail", () => {
  const calls = [];
  function button(route, isRoute) {
    return {
      dataset: { route },
      setAttribute() {},
      removeAttribute() {},
      classList: {
        classes: new Set(isRoute ? ["route-button"] : []),
        toggle(name, on) { if (on) this.classes.add(name); else this.classes.delete(name); },
        contains(name) { return this.classes.has(name); },
      },
      scrollIntoView() { calls.push(route); },
    };
  }
  const buttons = [button("overview", true), button("help", true), button("contact", false)];
  const context = {
    routes: [{ id: "overview" }, { id: "help" }],
    state: {},
    $$: (selector) => selector === "[data-route]" ? buttons : [],
    $: () => ({ scrollTo() {}, open: false }),
    history: { replaceState() {} },
    openSystemInstallPage() {},
    dismissDialog() {},
  };
  vm.createContext(context);
  vm.runInContext(handler("navigate"), context);
  context.navigate("help");
  assert.deepEqual(calls, ["help"], "only the active route button is scrolled into view");
});

test("fan dots use the node collection and apply CSSOM properties", () => {
  const dot = { dataset: { fanX: "28.6", fanY: "80" }, style: { values: {}, setProperty(key, value) { this.values[key] = value; } } };
  const c = { $$: () => [dot] };
  vm.createContext(c);
  vm.runInContext(handler("applyFanCurveStyles"), c);
  c.applyFanCurveStyles({});
  assert.equal(dot.style.values["--fan-x"], "28.6%");
  assert.equal(dot.style.values["--fan-y"], "80%");
});

import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");
function handler(name) {
  const start = source.search(new RegExp("(?:async )?function " + name + "\\("));
  assert.ok(start >= 0, name);
  const rest = source.slice(start);
  const next = rest.slice(1).search(/\n(?:async )?function /);
  return next < 0 ? rest : rest.slice(0, next + 1);
}
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
    refreshAll: async () => {}, toast() {},
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
  const second = c.refreshAll({ quiet: true });
  requests[0]({ id: "before", synthetic: true });
  for (let i = 0; i < 10 && requests.length < 2; i++) await Promise.resolve();
  assert.equal(requests.length, 2);
  assert.deepEqual(rendered, []);
  requests[1]({ id: "after", synthetic: true });
  await Promise.all([first, second]);
  assert.deepEqual(rendered, ["after"]);
  assert.equal(c.state.refreshing, false);
});

test("runtime markup contains no CSP-blocked style attributes", () => {
  assert.doesNotMatch(source, /\sstyle="/);
  assert.match(source, /escapeHtml\(frame.base64\)/);
  assert.match(source, /"x-rsetup-request": "1"/);
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

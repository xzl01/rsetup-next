import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

// Run the actual drawer handlers without a browser or a live privileged provider.
const source = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");
function handler(name) {
  const start = source.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert.ok(start >= 0);
  const rest = source.slice(start);
  const next = rest.slice(1).search(/\n(?:async )?function /);
  return next < 0 ? rest : rest.slice(0, next + 1);
}
function harness() {
  const element = { disabled: false, hidden: false, textContent: "", classList: { add() {}, remove() {} } };
  const context = {
    state: { selectedHardware: "device-tree", hardwareLoadVersion: 1, overlaySelection: ["uart.dtbo"], overlayPlan: null },
    transport: {}, $: () => element, t: (key) => key, displayError: (error) => error.message,
    renders: 0, errors: [], requestAnimationFrame() {},
    refreshAll: async () => {},
    renderOverlayTool() { context.renders++; },
    toast(title, detail, error) { if (error) context.errors.push(detail); },
  };
  vm.createContext(context);
  vm.runInContext(`${handler("previewOverlays")}\n${handler("applyOverlays")}`, context);
  return context;
}

test("a preview arriving after drawer close is discarded", async () => {
  const c = harness();
  let resolve;
  c.transport.planOverlays = () => new Promise((done) => { resolve = done; });
  const pending = c.previewOverlays();
  c.state.selectedHardware = null;
  c.state.hardwareLoadVersion++;
  resolve({ planToken: "late" });
  await pending;
  assert.equal(c.state.overlayPlan, null);
  assert.equal(c.renders, 0);
});

test("only the newest selection receives a preview", async () => {
  const c = harness();
  const requests = [];
  c.transport.planOverlays = (selected) => new Promise((resolve) => requests.push({ selected, resolve }));
  const first = c.previewOverlays();
  c.state.overlaySelection = ["spi.dtbo"];
  const second = c.previewOverlays();
  requests[1].resolve({ planToken: "new" });
  await second;
  requests[0].resolve({ planToken: "old" });
  await first;
  assert.equal(c.state.overlayPlan.planToken, "new");
  assert.deepEqual(Array.from(requests[0].selected), ["uart.dtbo"]);
  assert.equal(c.renders, 1);
});

test("a failed preview keeps the selection editable and discards old plans", async () => {
  const c = harness();
  c.state.overlayPlan = { planToken: "old" };
  c.transport.planOverlays = async () => { throw new Error("conflict"); };
  await c.previewOverlays();
  assert.equal(c.state.overlayPlan, null);
  assert.deepEqual(c.state.overlaySelection, ["uart.dtbo"]);
  assert.equal(c.renders, 1);
  assert.deepEqual(c.errors, ["conflict"]);
});

test("apply submits the immutable reviewed selection, not a later draft", async () => {
  const c = harness();
  c.state.overlayPlan = { planToken: "reviewed", selectedIds: ["reviewed.dtbo"] };
  c.state.overlaySelection = ["unreviewed.dtbo"];
  let request;
  c.transport.applyOverlays = async (...args) => {
    request = args;
    return { run: { synthetic: true } };
  };
  await c.applyOverlays();
  assert.deepEqual(request, [["reviewed.dtbo"], "reviewed", true]);
});

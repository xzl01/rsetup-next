import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");
function handler(name) {
  const start = source.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert.ok(start >= 0);
  const rest = source.slice(start);
  const next = rest.slice(1).search(/\n(?:async )?function /);
  return next < 0 ? rest : rest.slice(0, next + 1);
}
function harness() {
  const c = {
    state: { sources: { supported: true, sourceRevision: "v1", providers: [{ id: "official" }, { id: "ustc" }] }, sourceBenchmark: { version: 0, running: false } },
    transport: {}, renders: 0, errors: [],
    renderSourceBenchmark() { c.renders++; },
    t: (key) => key, displayError: (error) => error.message,
    toast(title, message) { c.errors.push(message); },
  };
  vm.createContext(c);
  vm.runInContext(`${handler("resetSourceBenchmark")}\n${handler("benchmarkSources")}`, c);
  return c;
}
const result = (id) => ({ providerId: id, sourceRevision: "v1", synthetic: true, probes: [] });

test("benchmarks providers sequentially without selecting or applying a mirror", async () => {
  const c = harness();
  const calls = [];
  c.transport.benchmarkSource = async (id) => { calls.push(id); return result(id); };
  await c.benchmarkSources();
  assert.deepEqual(calls, ["official", "ustc"]);
  assert.equal(c.state.sourceBenchmark.completed, 2);
  assert.equal(c.state.sourceBenchmark.running, false);
  assert.equal(c.state.sourcePlan, undefined);
});

test("duplicate clicks are ignored and stop ends after the in-flight mirror", async () => {
  const c = harness();
  let resolve;
  let requests = 0;
  c.transport.benchmarkSource = () => { requests++; return new Promise((done) => { resolve = done; }); };
  const first = c.benchmarkSources();
  await c.benchmarkSources();
  assert.equal(requests, 1);
  c.state.sourceBenchmark.stop = true;
  resolve(result("official"));
  await first;
  assert.equal(requests, 1);
  assert.equal(c.state.sourceBenchmark.running, false);
});

test("source refresh invalidates late success and late errors", async () => {
  for (const rejectResponse of [false, true]) {
    const c = harness();
    let resolve, reject;
    c.transport.benchmarkSource = () => new Promise((yes, no) => { resolve = yes; reject = no; });
    const pending = c.benchmarkSources();
    c.resetSourceBenchmark();
    if (rejectResponse) reject(new Error("old failure")); else resolve(result("official"));
    await pending;
    assert.equal(c.state.sourceBenchmark.results.length, 0);
    assert.equal(c.errors.length, 0);
  }
});

test("mismatched revision or provider invalidates results", async () => {
  for (const wrong of [{ sourceRevision: "v2" }, { providerId: "cqu" }]) {
    const c = harness();
    c.transport.benchmarkSource = async () => ({ ...result("official"), ...wrong });
    await c.benchmarkSources();
    assert.equal(c.state.sourceBenchmark.results.length, 0);
    assert.deepEqual(c.errors, ["sources.benchmarkStale"]);
  }
});

test("one failing mirror does not hide later results", async () => {
  const c = harness();
  c.transport.benchmarkSource = async (id) => {
    if (id === "official") throw new Error("offline");
    return result(id);
  };
  await c.benchmarkSources();
  assert.equal(c.state.sourceBenchmark.results[0].error.message, "offline");
  assert.equal(c.state.sourceBenchmark.results[1].providerId, "ustc");
});

test("saved request errors are translated at render time", () => {
  const c = harness();
  let locale = "en";
  c.t = (key) => `${locale}:${key}`;
  c.i18n = { apiError: (code) => `${locale}:${code}` };
  vm.runInContext(handler("benchmarkError"), c);
  const transportError = { translationKey: "api.transport_failure", message: "old language" };
  const apiError = { code: "sources_unsupported", message: "old language", providerMessage: "raw" };
  assert.equal(c.benchmarkError(transportError), "en:api.transport_failure");
  locale = "zh";
  assert.equal(c.benchmarkError(transportError), "zh:api.transport_failure");
  assert.equal(c.benchmarkError(apiError), "zh:sources_unsupported");
});

test("selecting another mirror invalidates an in-flight change preview", async () => {
  const c = harness();
  const elements = new Map();
  c.$ = (selector) => {
    if (!elements.has(selector)) elements.set(selector, { value: "official", classList: { remove() {} }, scrollIntoView() {} });
    return elements.get(selector);
  };
  c.state.sourcePreviewVersion = 0;
  c.renderSourcePlan = () => { c.renders++; };
  let resolve;
  c.transport.planSources = () => new Promise((done) => { resolve = done; });
  vm.runInContext(`${handler("clearSourcePlan")}\n${handler("previewSources")}`, c);
  const pending = c.previewSources();
  c.$("[data-source-provider]").value = "ustc";
  c.clearSourcePlan();
  resolve({ provider: { id: "official" }, sourceRevision: "v1", planToken: "old" });
  await pending;
  assert.equal(c.state.sourcePlan, null);
  assert.equal(c.renders, 0);
});

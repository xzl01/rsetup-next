import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");
const render = source.slice(source.indexOf("function renderCpuMetric("), source.indexOf("\nfunction byteUnit("));

for (const locale of ["en-US", "zh-CN"]) {
  test(`CPU unknown, zero and recovered samples (${locale})`, () => {
    const label = { textContent: "" };
    const meter = { style: {} };
    const context = vm.createContext({
      setText: (_, text) => { label.textContent = text; },
      $: () => meter,
      formatPercent: (value) => `${new Intl.NumberFormat(locale, { minimumFractionDigits: 1 }).format(value)}%`,
    });
    vm.runInContext(render, context);
    for (const value of [null, undefined, NaN, Infinity, "31.4"]) {
      context.renderCpuMetric(value);
      assert.equal(label.textContent, "—");
      assert.equal(meter.style.transform, "scaleX(0)");
    }
    context.renderCpuMetric(0);
    assert.equal(label.textContent, "0.0%");
    context.renderCpuMetric(31.4);
    assert.equal(label.textContent, "31.4%");
    assert.equal(meter.style.transform, "scaleX(0.314)");
    context.renderCpuMetric(null);
    assert.equal(label.textContent, "—", "reboot must clear the old percent");
    context.renderCpuMetric(105);
    assert.equal(label.textContent, "100.0%");
    assert.ok(source.includes("renderCpuMetric(metrics.cpuPercent)"));
    assert.equal(context.memoryUsagePercent({ memoryUsedBytes: null, memoryTotalBytes: 100 }), null);
    assert.equal(context.memoryUsagePercent({ memoryUsedBytes: 10, memoryTotalBytes: null }), null);
    assert.equal(context.memoryUsagePercent({ memoryUsedBytes: 10, memoryTotalBytes: 0 }), null);
    assert.equal(context.memoryUsagePercent({ memoryUsedBytes: 200, memoryTotalBytes: 100 }), null);
    assert.equal(context.memoryUsagePercent({ memoryUsedBytes: 0, memoryTotalBytes: 100 }), 0);
    assert.equal(context.memoryUsagePercent({ memoryUsedBytes: 60, memoryTotalBytes: 100 }), 60);
  });
}

import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");

function handler(name) {
  const start = source.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert.ok(start >= 0, `function ${name} not found`);
  const rest = source.slice(start);
  const next = rest.slice(1).search(/\n(?:async )?function /);
  return next < 0 ? rest : rest.slice(0, next + 1);
}

function createTestContext(overrides = {}) {
  const elements = new Map();
  function getElement(sel) {
    if (!elements.has(sel)) {
      elements.set(sel, {
        innerHTML: "",
        textContent: "",
        value: "",
        disabled: false,
        dataset: {},
        style: {
          properties: {},
          setProperty(k, v) { this.properties[k] = v; },
        },
      });
    }
    return elements.get(sel);
  }

  const context = {
    state: {
      hardwareData: null,
      selectedHardware: "nvme",
      hardwareLoadVersion: 1,
      ...overrides.state,
    },
    $: (sel) => getElement(sel),
    $$: (sel, root) => {
      if (sel === "[data-metric-percent]") {
        return overrides.metricBars || [];
      }
      return [];
    },
    escapeHtml: (str) => String(str || "").replace(/[&<>"']/g, ""),
    formatNumber: (n, d) => Number(n).toFixed(d),
    byteUnit: (b) => `${b} B`,
    t: (key) => key,
    preserveToolFocus: () => {},
    ...overrides,
  };

  vm.createContext(context);
  vm.runInContext(
    `${handler("applyNvmeMetricStyles")}\n${handler("renderNvmeDeviceCard")}\n${handler("renderNvmeTool")}`,
    context
  );
  return { context, getElement };
}

test("renderNvmeTool renders graceful message when uninitialized", () => {
  const { context, getElement } = createTestContext({
    state: {
      hardwareData: { initialized: false, message: "No NVMe controllers detected" },
      selectedHardware: "nvme",
    },
  });

  context.renderNvmeTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("hardware-tool-empty"));
  assert.ok(host.innerHTML.includes("No NVMe controllers detected"));
});

test("renderNvmeTool renders graceful empty message when initialized but no devices", () => {
  const { context, getElement } = createTestContext({
    state: {
      hardwareData: { initialized: true, devices: [] },
      selectedHardware: "nvme",
    },
  });

  context.renderNvmeTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("hardware-tool-empty"));
  assert.ok(host.innerHTML.includes("nvme.noDevices"));
});

test("renderNvmeTool renders device details and smart telemetry when initialized", () => {
  const demoDevice = {
    name: "nvme0",
    path: "/dev/nvme0n1",
    model: "Radxa M.2 NVMe SSD 512GB",
    serial: "RADXA2026NVME01",
    firmware: "1.0.0",
    totalBytes: 512110190592,
    smart: {
      criticalWarning: 0,
      warningFlags: [],
      temperatureC: 38.5,
      availableSparePercent: 100,
      spareThresholdPercent: 10,
      percentageUsed: 2,
      dataReadBytes: 1250000000000,
      dataWrittenBytes: 850000000000,
      hostReadCommands: 25000000,
      hostWriteCommands: 18000000,
      powerOnHours: 120,
      unsafeShutdowns: 1,
      mediaErrors: 0,
      numErrLogEntries: 0,
    },
  };

  const { context, getElement } = createTestContext({
    state: {
      hardwareData: { initialized: true, devices: [demoDevice] },
      selectedHardware: "nvme",
    },
  });

  context.renderNvmeTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("nvme-card"));
  assert.ok(host.innerHTML.includes("Radxa M.2 NVMe SSD 512GB"));
  assert.ok(host.innerHTML.includes("/dev/nvme0n1"));
  assert.ok(host.innerHTML.includes("nvme-badge-healthy"));
  assert.ok(host.innerHTML.includes("38.5 °C"));
  assert.ok(host.innerHTML.includes("RADXA2026NVME01"));
  assert.ok(host.innerHTML.includes("1.0.0"));
  assert.ok(host.innerHTML.includes("120 h"));
});

test("renderNvmeTool handles critical warnings and high temperatures appropriately", () => {
  const warningDevice = {
    name: "nvme1",
    path: "/dev/nvme1n1",
    model: "Test NVMe SSD 256GB",
    serial: "TESTSERIAL123",
    firmware: "2.1.0",
    totalBytes: 256000000000,
    smart: {
      criticalWarning: 1,
      warningFlags: ["spare_below_threshold"],
      temperatureC: 82.0,
      availableSparePercent: 5,
      spareThresholdPercent: 10,
      percentageUsed: 102,
      dataReadBytes: 5000000000000,
      dataWrittenBytes: 4000000000000,
      hostReadCommands: 100000000,
      hostWriteCommands: 90000000,
      powerOnHours: 5000,
      unsafeShutdowns: 15,
      mediaErrors: 3,
      numErrLogEntries: 5,
    },
  };

  const { context, getElement } = createTestContext({
    state: {
      hardwareData: { initialized: true, devices: [warningDevice] },
      selectedHardware: "nvme",
    },
  });

  context.renderNvmeTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("nvme-badge-critical"));
  assert.ok(host.innerHTML.includes("spare_below_threshold"));
  assert.ok(host.innerHTML.includes("82.0 °C"));
});


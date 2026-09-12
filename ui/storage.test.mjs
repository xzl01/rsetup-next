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

function loadI18n(language) {
  const attributes = {};
  const storage = new Map();
  const context = {
    CustomEvent: class CustomEvent {
      constructor(type, options) { this.type = type; this.detail = options?.detail; }
    },
    document: {
      documentElement: {
        dataset: {},
        set lang(value) { attributes.lang = value; },
        get lang() { return attributes.lang; },
      },
    },
    localStorage: {
      getItem: (key) => storage.get(key) ?? null,
      setItem: (key, value) => storage.set(key, value),
    },
    navigator: { language, languages: [language] },
  };
  context.window = { dispatchEvent() {}, RsetupI18n: null };
  vm.runInNewContext(fs.readFileSync(new URL("./i18n.js", import.meta.url), "utf8"), context);
  return context.window.RsetupI18n;
}

const TEMPLATES = {
  "storageTool.countNvme": "{count} NVMe",
  "storageTool.countMmc": "{count} MMC/eMMC",
};

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
      selectedHardware: "storage",
      ...overrides.state,
    },
    $: (sel) => getElement(sel),
    $$: (sel, root) => (sel === "[data-metric-percent]" ? overrides.metricBars || [] : []),
    escapeHtml: (str) => String(str || "").replace(/[&<>"']/g, ""),
    formatNumber: (n, d) => Number(n).toFixed(d),
    byteUnit: (b) => `${Math.round(Number(b) / 1024)} KiB`,
    t: (key, params) => {
      let value = TEMPLATES[key] || key;
      for (const [name, param] of Object.entries(params || {})) {
        value = value.replaceAll(`{${name}}`, String(param));
      }
      return value;
    },
    preserveToolFocus: () => {},
    renderNvmeDeviceCard: (device) => `<section class="nvme-card"><strong>${device.model}</strong></section>`,
    ...overrides,
  };

  vm.createContext(context);
  vm.runInContext(
    [
      handler("applyStorageMetricStyles"),
      handler("renderStorageMmcCard"),
      handler("renderStorageTool"),
    ].join("\n"),
    context
  );
  return { context, getElement };
}

const emmcDevice = {
  name: "mmc0:0001",
  blockPath: "/dev/mmcblk0",
  cardType: "MMC",
  model: "FE4MB4",
  manufacturer: "Samsung (0x000015)",
  serial: "0x12345678",
  firmware: "0x01",
  totalBytes: 62537072640,
  health: { preEolInfo: 1, lifeTimeEstAPercent: 10, lifeTimeEstBPercent: 10, warningFlags: [] },
};

const sdDevice = {
  name: "mmc1:59b4",
  blockPath: "/dev/mmcblk1",
  cardType: "SD",
  model: "SC64G",
  manufacturer: "SanDisk (0x000045)",
  serial: "0x87654321",
  firmware: "0x01",
  totalBytes: 64026691584,
  health: { preEolInfo: 0, lifeTimeEstAPercent: null, lifeTimeEstBPercent: null, warningFlags: [] },
};

const nvmeDevice = {
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

test("renderStorageTool renders uninitialized message when no module initialized", () => {
  const { context, getElement } = createTestContext({
    state: {
      hardwareData: {
        nvme: { initialized: false, devices: [], message: "No NVMe controller detected in system" },
        mmc: { initialized: false, devices: [], message: "No MMC devices detected in system" },
      },
      selectedHardware: "storage",
    },
  });

  context.renderStorageTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("hardware-tool-empty"));
  assert.ok(host.innerHTML.includes("storageTool.uninitialized"));
});

test("renderStorageTool renders NVMe only without MMC section", () => {
  const { context, getElement } = createTestContext({
    state: {
      hardwareData: {
        nvme: { initialized: true, devices: [nvmeDevice] },
        mmc: { initialized: true, devices: [] },
      },
      selectedHardware: "storage",
    },
  });

  context.renderStorageTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("nvme-card"));
  assert.ok(host.innerHTML.includes("Radxa M.2 NVMe SSD 512GB"));
  assert.ok(!host.innerHTML.includes("storage-mmc-card"));
  assert.ok(host.innerHTML.includes("1 NVMe"));
});

test("renderStorageTool renders MMC card with life bars and N/A", () => {
  const { context, getElement } = createTestContext({
    state: {
      hardwareData: {
        nvme: { initialized: false, devices: [] },
        mmc: { initialized: true, devices: [emmcDevice, sdDevice] },
      },
      selectedHardware: "storage",
    },
  });

  context.renderStorageTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("storage-mmc-card"));
  assert.ok(host.innerHTML.includes("FE4MB4"));
  assert.ok(host.innerHTML.includes("/dev/mmcblk0"));
  assert.ok(host.innerHTML.includes("storageTool.emmc"));
  assert.ok(host.innerHTML.includes("storageTool.sd"));
  assert.ok(host.innerHTML.includes("storageTool.na")); // SD life is null
  // The SD card reports no life estimates, so both of its bars take the N/A
  // state; the eMMC keeps a real percentage.
  assert.equal((host.innerHTML.match(/nvme-metric-bar-fill is-na/g) || []).length, 2);
  assert.ok(host.innerHTML.includes('data-metric-percent="10"'));
  assert.ok(!host.innerHTML.includes("nvme-metric-bar-fill is-critical"));
  assert.ok(!host.innerHTML.includes('class="nvme-card"'));
  assert.ok(host.innerHTML.includes("2 MMC/eMMC"));
});

test("renderStorageTool renders NVMe above MMC with combined counts", () => {
  const { context, getElement } = createTestContext({
    state: {
      hardwareData: {
        nvme: { initialized: true, devices: [nvmeDevice] },
        mmc: { initialized: true, devices: [emmcDevice] },
      },
      selectedHardware: "storage",
    },
  });

  context.renderStorageTool();
  const host = getElement("[data-hardware-body]");
  const nvmePos = host.innerHTML.indexOf('class="nvme-card"');
  const mmcPos = host.innerHTML.indexOf("storage-mmc-card");
  assert.ok(nvmePos >= 0 && mmcPos >= 0);
  assert.ok(nvmePos < mmcPos, "NVMe section must render above MMC section");
  assert.ok(host.innerHTML.includes("1 NVMe"));
  assert.ok(host.innerHTML.includes("1 MMC/eMMC"));
});

test("renderStorageMmcCard maps preEolInfo to badge classes", () => {
  const critical = { ...emmcDevice, health: { ...emmcDevice.health, preEolInfo: 3 } };
  const warning = { ...emmcDevice, health: { ...emmcDevice.health, preEolInfo: 2 } };
  const flagged = { ...emmcDevice, health: { ...emmcDevice.health, warningFlags: ["life_exceeded"] } };
  const undefined = { ...emmcDevice, health: { ...emmcDevice.health, preEolInfo: 0 } };

  assert.ok(createTestContext().context.renderStorageMmcCard(critical).includes("nvme-badge-critical"));
  assert.ok(createTestContext().context.renderStorageMmcCard(warning).includes("nvme-badge-warning"));
  assert.ok(createTestContext().context.renderStorageMmcCard(flagged).includes("nvme-badge-critical"));
  assert.ok(createTestContext().context.renderStorageMmcCard(emmcDevice).includes("nvme-badge-healthy"));
  assert.ok(createTestContext().context.renderStorageMmcCard(undefined).includes("storageTool.preEolUndefined"));
});


test("storage drawer copy resolves through the storageTool namespace", () => {
  const { context } = createTestContext({ t: (key) => key });
  vm.runInContext(handler("hardwareToolCopy"), context);
  const copy = context.hardwareToolCopy("storage");
  // Both keys must exist in both dictionaries, or t() silently falls back to
  // the "details unavailable" copy and the drawer shows the wrong title.
  assert.equal(copy.title, "storageTool.title");
  assert.equal(copy.description, "storageTool.description");
  assert.equal(loadI18n("en-US").t(copy.title), "Storage");
  assert.equal(loadI18n("zh-CN").t(copy.title), "存储");
  assert.equal(loadI18n("en-US").t(copy.description), "Monitor NVMe and MMC/eMMC storage health.");
  assert.equal(loadI18n("zh-CN").t(copy.description), "监测 NVMe 与 MMC/eMMC 存储健康状态。");
});

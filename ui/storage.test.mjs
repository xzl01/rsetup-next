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
    icon: (name) => `<svg class="icon-${name}"></svg>`,
    relativeTime: () => "just now",
    t: (key, params) => {
      let value = TEMPLATES[key] || key;
      for (const [name, param] of Object.entries(params || {})) {
        value = value.replaceAll(`{${name}}`, String(param));
      }
      return value;
    },
    preserveToolFocus: () => {},
    ...overrides,
  };

  vm.createContext(context);
  vm.runInContext(
    [
      handler("applyStorageMetricStyles"),
      handler("renderNvmeDeviceCard"),
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
  telemetry: { state: "available", error: null },
  healthState: "healthy",
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
  telemetry: { state: "unsupported", error: null },
  healthState: "unknown",
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
  telemetry: { state: "available", error: null },
  healthState: "healthy",
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

test("renderStorageMmcCard maps healthState to badge classes", () => {
  const critical = { ...emmcDevice, health: { ...emmcDevice.health, preEolInfo: 3 }, healthState: "critical" };
  const warning = { ...emmcDevice, health: { ...emmcDevice.health, preEolInfo: 2, warningFlags: ["pre_eol_warning"] }, healthState: "warning" };
  const flagged = { ...emmcDevice, health: { ...emmcDevice.health, warningFlags: ["life_time_typ_a_exceeded"] }, healthState: "critical" };
  const undefinedDev = { ...emmcDevice, health: { ...emmcDevice.health, preEolInfo: 0 }, healthState: "unknown" };

  assert.ok(createTestContext().context.renderStorageMmcCard(critical).includes("nvme-badge-critical"));
  assert.ok(createTestContext().context.renderStorageMmcCard(warning).includes("nvme-badge-warning"));
  assert.ok(!createTestContext().context.renderStorageMmcCard(warning).includes("nvme-badge-critical"));
  assert.ok(createTestContext().context.renderStorageMmcCard(flagged).includes("nvme-badge-critical"));
  assert.ok(createTestContext().context.renderStorageMmcCard(emmcDevice).includes("nvme-badge-healthy"));
  assert.ok(createTestContext().context.renderStorageMmcCard(undefinedDev).includes("storageTool.preEolUndefined"));
});

test("renderNvmeDeviceCard and renderStorageMmcCard handle missing telemetry and contract requirements", () => {
  const missingNvme = {
    name: "nvme0",
    path: "/dev/nvme0",
    model: "FIXTURE SSD",
    serial: "test-only",
    firmware: "1",
    totalBytes: 4096,
    smart: null,
    telemetry: { state: "unavailable", error: { kind: "permission_denied", code: 13 } },
    healthState: "unknown",
  };

  const cardHtml = createTestContext().context.renderNvmeDeviceCard(missingNvme);
  assert.ok(!cardHtml.includes("nvme-badge-healthy"));
  assert.ok(!cardHtml.includes("0.0 °C"));
  assert.ok(cardHtml.includes("nvme-badge-unknown"));
  assert.ok(cardHtml.includes("storageTool.permissionDenied"));
  assert.ok(cardHtml.includes("FIXTURE SSD"));

  const sdHtml = createTestContext().context.renderStorageMmcCard(sdDevice);
  assert.ok(!sdHtml.includes("nvme-badge-healthy"));
  assert.ok(sdHtml.includes("nvme-badge-unknown"));
  assert.ok(sdHtml.includes("storageTool.unsupported"));

  const warnEmmc = {
    ...emmcDevice,
    health: { preEolInfo: 2, lifeTimeEstAPercent: 10, lifeTimeEstBPercent: 10, warningFlags: ["pre_eol_warning"] },
    telemetry: { state: "available", error: null },
    healthState: "warning",
  };
  const warnHtml = createTestContext().context.renderStorageMmcCard(warnEmmc);
  assert.ok(warnHtml.includes("nvme-badge-warning"));
  assert.ok(!warnHtml.includes("nvme-badge-critical"));
  assert.ok(!warnHtml.includes("nvme-badge-healthy"));

  // 0x0A (100% endurance, not exceeded) vs 0x0B (101% endurance, exceeded) without artificial flag manipulation
  const dev0A = {
    ...emmcDevice,
    health: { preEolInfo: 1, lifeTimeEstAPercent: 100, lifeTimeEstBPercent: 50, warningFlags: [] },
    telemetry: { state: "available", error: null },
    healthState: "warning",
  };
  const html0A = createTestContext().context.renderStorageMmcCard(dev0A);
  assert.ok(html0A.includes("nvme-badge-warning"));
  assert.ok(!html0A.includes("nvme-badge-critical"));
  assert.ok(html0A.includes("90–100%"));

  const dev0B = {
    ...emmcDevice,
    health: { preEolInfo: 1, lifeTimeEstAPercent: 101, lifeTimeEstBPercent: 50, warningFlags: ["life_time_typ_a_exceeded"] },
    telemetry: { state: "available", error: null },
    healthState: "critical",
  };
  const html0B = createTestContext().context.renderStorageMmcCard(dev0B);
  assert.ok(html0B.includes("nvme-badge-critical"));
  assert.ok(!html0B.includes("nvme-badge-warning"));
  assert.ok(html0B.includes(">100%"));
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

test("stale cards downgrade health badges to unknown and error objects re-translate on locale render", () => {
  const i18nEn = loadI18n("en-US");
  const i18nZh = loadI18n("zh-CN");

  const staleContextEn = createTestContext({
    state: {
      hardwareData: {
        nvme: { initialized: true, devices: [nvmeDevice] },
        mmc: { initialized: true, devices: [emmcDevice] },
      },
      selectedHardware: "storage",
      storageStale: true,
      storageRefreshError: { code: "permission_denied", message: "Forbidden" },
    },
    t: (key, params) => i18nEn.t(key, params),
    relativeTime: () => "just now",
  });

  staleContextEn.context.renderStorageTool();
  const hostEn = staleContextEn.getElement("[data-hardware-body]");
  assert.ok(hostEn.innerHTML.includes("Stale data"));
  assert.ok(!hostEn.innerHTML.includes("nvme-badge-healthy"));
  assert.ok(hostEn.innerHTML.includes("nvme-badge-unknown"));

  const staleContextZh = createTestContext({
    state: {
      hardwareData: {
        nvme: { initialized: true, devices: [nvmeDevice] },
        mmc: { initialized: true, devices: [emmcDevice] },
      },
      selectedHardware: "storage",
      storageStale: true,
      storageRefreshError: { code: "permission_denied", message: "Forbidden" },
    },
    t: (key, params) => i18nZh.t(key, params),
    relativeTime: () => "刚刚",
  });

  staleContextZh.context.renderStorageTool();
  const hostZh = staleContextZh.getElement("[data-hardware-body]");
  assert.ok(hostZh.innerHTML.includes("数据已过期"));
  assert.ok(!hostZh.innerHTML.includes("nvme-badge-healthy"));
  assert.ok(hostZh.innerHTML.includes("nvme-badge-unknown"));

  // Initial failure without data re-translates error dynamically per locale
  const errContextEn = createTestContext({
    state: {
      hardwareData: null,
      selectedHardware: "storage",
      storageStale: false,
      storageRefreshError: { code: "request_forbidden", message: "Forbidden" },
    },
    t: (key, params) => i18nEn.t(key, params),
    displayError: (err) => i18nEn.apiError(err.code, err.message),
    relativeTime: () => "just now",
  });
  errContextEn.context.renderStorageTool();
  assert.ok(errContextEn.getElement("[data-hardware-body]").innerHTML.includes("Open the console"));

  const errContextZh = createTestContext({
    state: {
      hardwareData: null,
      selectedHardware: "storage",
      storageStale: false,
      storageRefreshError: { code: "request_forbidden", message: "Forbidden" },
    },
    t: (key, params) => i18nZh.t(key, params),
    displayError: (err) => i18nZh.apiError(err.code, err.message),
    relativeTime: () => "刚刚",
  });
  errContextZh.context.renderStorageTool();
  assert.ok(errContextZh.getElement("[data-hardware-body]").innerHTML.includes("请通过本机地址"));
});

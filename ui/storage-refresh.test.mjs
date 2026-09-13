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

function deferred() {
  let resolve, reject;
  const promise = new Promise((ok, fail) => {
    resolve = ok;
    reject = fail;
  });
  return { promise, resolve, reject };
}

// 1. Same generation two triggers => 1 transport, both await; after completion next call rereads
test("scenario 1: same generation two triggers => 1 transport, both await; after completion next call rereads", async () => {
  const pending = deferred();
  let calls = 0;

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => {
        calls += 1;
        return pending.promise;
      },
    },
    renderHardwareTool: () => {},
    Date,
  };

  vm.createContext(context);
  vm.runInContext(handler("refreshStorageTool"), context);

  const a = context.refreshStorageTool();
  const b = context.refreshStorageTool();
  await Promise.resolve();

  assert.equal(calls, 1);
  pending.resolve({ nvme: { initialized: false, devices: [] }, mmc: { initialized: false, devices: [] } });
  await Promise.all([a, b]);

  assert.equal(context.state.storageRefreshing, false);
  assert.equal(context.state.storageRefreshPromise, null);
  assert.equal(context.state.storageStale, false);
  assert.ok(context.state.storageRefreshedAt != null);

  // After completion, next call rereads
  const nextPending = deferred();
  context.transport.storageStatus = () => {
    calls += 1;
    return nextPending.promise;
  };
  const c = context.refreshStorageTool();
  await Promise.resolve();
  assert.equal(calls, 2);
  nextPending.resolve({ nvme: { initialized: false, devices: [] }, mmc: { initialized: false, devices: [] } });
  await c;
  assert.equal(context.state.storageRefreshing, false);
  assert.equal(context.state.storageRefreshPromise, null);
});

// 2. Close then old success/error => no writes/no render closed drawer
test("scenario 2: close then old success/error => no writes and no render closed drawer", async () => {
  const pendingSuccess = deferred();
  let renderCalls = 0;

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => pendingSuccess.promise,
    },
    renderHardwareTool: () => {
      renderCalls += 1;
    },
    Date,
  };

  vm.createContext(context);
  vm.runInContext(handler("refreshStorageTool"), context);

  const req = context.refreshStorageTool();
  await Promise.resolve();
  renderCalls = 0; // Reset counter after initial renderHardwareTool() call from refreshStorageTool()

  // Simulate drawer close: increments version, selectedHardware = null, clears state
  state.selectedHardware = null;
  state.hardwareLoadVersion += 1;
  state.hardwareData = null;
  state.storageRefreshPromise = null;
  state.storageRefreshing = false;

  // Resolve old request
  pendingSuccess.resolve({ nvme: { initialized: true, devices: [{ name: "nvme0" }] }, mmc: { initialized: false, devices: [] } });
  await req;

  assert.equal(state.hardwareData, null, "hardwareData must not be overwritten after drawer closed");
  assert.equal(state.storageRefreshedAt, null);
  assert.equal(renderCalls, 0, "must not re-render closed drawer");

  // Error case
  const pendingError = deferred();
  state.selectedHardware = "storage";
  state.hardwareLoadVersion += 1;
  context.transport.storageStatus = () => pendingError.promise;

  const reqErr = context.refreshStorageTool();
  await Promise.resolve();
  renderCalls = 0;

  // Close drawer
  state.selectedHardware = null;
  state.hardwareLoadVersion += 1;
  state.storageRefreshPromise = null;
  state.storageRefreshing = false;

  pendingError.reject(new Error("network failure"));
  await reqErr;

  assert.equal(state.storageRefreshError, null, "storageRefreshError must not be written after close");
  assert.equal(renderCalls, 0, "must not render after close");
});

// 3. Storage switched thermal => old storage never replaces thermal
test("scenario 3: storage switched thermal => old storage never replaces thermal", async () => {
  const pendingStorage = deferred();
  let renderCalls = 0;

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => pendingStorage.promise,
    },
    renderHardwareTool: () => {
      renderCalls += 1;
    },
    Date,
  };

  vm.createContext(context);
  vm.runInContext(handler("refreshStorageTool"), context);

  const storageReq = context.refreshStorageTool();
  await Promise.resolve();

  // User switches to thermal tool
  state.selectedHardware = "thermal";
  state.hardwareLoadVersion = 2;
  const thermalData = { currentPolicy: "step_wise", fanCurve: null };
  state.hardwareData = thermalData;
  state.storageRefreshPromise = null;
  state.storageRefreshing = false;
  renderCalls = 0;

  // Old storage arrives
  pendingStorage.resolve({ nvme: { initialized: true, devices: [{ name: "nvme0" }] }, mmc: { initialized: false, devices: [] } });
  await storageReq;

  assert.equal(state.hardwareData, thermalData, "thermal data must not be overwritten by old storage");
  assert.equal(state.storageRefreshedAt, null);
  assert.equal(renderCalls, 0, "must not render thermal with old storage");
});

// 4. Close/reopen new result first => old success/error cannot replace
test("scenario 4: close/reopen new result first => old success/error cannot replace", async () => {
  const oldReqPending = deferred();
  const newReqPending = deferred();
  let currentPending = oldReqPending;

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => currentPending.promise,
    },
    renderHardwareTool: () => {},
    Date,
  };

  vm.createContext(context);
  vm.runInContext(handler("refreshStorageTool"), context);

  const req1 = context.refreshStorageTool();
  await Promise.resolve();

  // Reopen storage: new generation (version 2)
  state.hardwareLoadVersion = 2;
  state.storageRefreshPromise = null;
  state.storageRefreshing = false;
  currentPending = newReqPending;

  const req2 = context.refreshStorageTool();
  await Promise.resolve();

  // New result arrives first
  const newData = { nvme: { initialized: true, devices: [{ name: "nvme-new" }] }, mmc: { initialized: false, devices: [] } };
  newReqPending.resolve(newData);
  await req2;

  assert.equal(state.hardwareData, newData);

  // Now old result arrives late
  const oldData = { nvme: { initialized: true, devices: [{ name: "nvme-old" }] }, mmc: { initialized: false, devices: [] } };
  oldReqPending.resolve(oldData);
  await req1;

  assert.equal(state.hardwareData, newData, "old response must not overwrite new data");
});

// 5. Old finally first while new pending => new promise and loading retained
test("scenario 5: old finally first while new pending => new promise and loading retained", async () => {
  const oldReqPending = deferred();
  const newReqPending = deferred();
  let currentPending = oldReqPending;

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => currentPending.promise,
    },
    renderHardwareTool: () => {},
    Date,
  };

  vm.createContext(context);
  vm.runInContext(handler("refreshStorageTool"), context);

  const req1 = context.refreshStorageTool();
  await Promise.resolve();

  // Reopen: new generation
  state.hardwareLoadVersion = 2;
  state.storageRefreshPromise = null;
  state.storageRefreshing = false;
  currentPending = newReqPending;

  const req2 = context.refreshStorageTool();
  await Promise.resolve();

  assert.equal(state.storageRefreshing, true);
  const req2Task = state.storageRefreshPromise;
  assert.ok(req2Task !== null);

  // Old completes (finally executes) while new is still pending
  oldReqPending.resolve({ nvme: { initialized: false, devices: [] }, mmc: { initialized: false, devices: [] } });
  await req1;

  // New promise and loading must still be retained
  assert.equal(state.storageRefreshing, true, "loading must stay true for new request");
  assert.equal(state.storageRefreshPromise, req2Task, "promise identity must not be wiped by old finally");

  // New completes
  newReqPending.resolve({ nvme: { initialized: true, devices: [{ name: "nvme0" }] }, mmc: { initialized: false, devices: [] } });
  await req2;

  assert.equal(state.storageRefreshing, false);
  assert.equal(state.storageRefreshPromise, null);
});

// 6. First failure visible, retry success clears
test("scenario 6: first failure visible, retry success clears", async () => {
  const failPending = deferred();

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => failPending.promise,
    },
    renderHardwareTool: () => {},
    Date,
  };

  vm.createContext(context);
  vm.runInContext(handler("refreshStorageTool"), context);

  const req1 = context.refreshStorageTool();
  await Promise.resolve();

  const sampleError = new Error("EACCES permission denied");
  failPending.reject(sampleError);
  await req1;

  assert.equal(state.storageRefreshError, sampleError);
  assert.equal(state.storageStale, false, "stale is false because hardwareData was null");
  assert.equal(state.hardwareData, null);

  // Retry succeeds
  const successPending = deferred();
  context.transport.storageStatus = () => successPending.promise;

  const req2 = context.refreshStorageTool();
  await Promise.resolve();

  const successData = { nvme: { initialized: true, devices: [{ name: "nvme0" }] }, mmc: { initialized: false, devices: [] } };
  successPending.resolve(successData);
  await req2;

  assert.equal(state.storageRefreshError, null, "retry success clears error");
  assert.equal(state.storageStale, false);
  assert.equal(state.hardwareData, successData);
  assert.ok(state.storageRefreshedAt != null);
});

// 7. Prior data failure => stale and neither NVMe nor MMC healthy class
test("scenario 7: prior data failure => stale and neither NVMe nor MMC healthy class", async () => {
  const initialPending = deferred();

  const initialData = {
    nvme: {
      initialized: true,
      devices: [
        {
          name: "nvme0",
          model: "Samsung SSD",
          totalBytes: 256000,
          healthState: "healthy",
          smart: {
            temperatureC: 45,
            availableSparePercent: 100,
            percentageUsed: 5,
            warningFlags: [],
          },
        },
      ],
    },
    mmc: {
      initialized: true,
      devices: [
        {
          name: "mmcblk0",
          model: "eMMC 64GB",
          cardType: "MMC",
          totalBytes: 64000,
          healthState: "healthy",
          health: {
            preEolInfo: 1,
            lifeTimeEstAPercent: 10,
            lifeTimeEstBPercent: 10,
          },
        },
      ],
    },
  };

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => initialPending.promise,
    },
    renderHardwareTool: () => {},
    Date,
    t: (key) => key,
    escapeHtml: (s) => String(s || ""),
    formatNumber: (n) => String(n),
    byteUnit: (b) => `${b} B`,
  };

  vm.createContext(context);
  vm.runInContext(
    [
      handler("renderNvmeDeviceCard"),
      handler("renderStorageMmcCard"),
      handler("refreshStorageTool"),
    ].join("\n"),
    context
  );

  const req1 = context.refreshStorageTool();
  await Promise.resolve();
  initialPending.resolve(initialData);
  await req1;

  assert.equal(state.hardwareData, initialData);
  assert.equal(state.storageStale, false);

  // When fresh, both cards have healthy badges
  const freshNvmeHtml = context.renderNvmeDeviceCard(initialData.nvme.devices[0]);
  const freshMmcHtml = context.renderStorageMmcCard(initialData.mmc.devices[0]);
  assert.ok(freshNvmeHtml.includes("nvme-badge-healthy"));
  assert.ok(freshMmcHtml.includes("nvme-badge-healthy"));

  // Subsequent refresh fails
  const secondPending = deferred();
  context.transport.storageStatus = () => secondPending.promise;

  const req2 = context.refreshStorageTool();
  await Promise.resolve();
  secondPending.reject(new Error("Kernel status failure"));
  await req2;

  assert.equal(state.storageStale, true, "must be marked stale because hardwareData != null");
  assert.equal(state.hardwareData, initialData, "prior data preserved in state");

  // When stale, badges downgraded to unknown/neutral, never healthy
  const staleNvmeHtml = context.renderNvmeDeviceCard(initialData.nvme.devices[0]);
  const staleMmcHtml = context.renderStorageMmcCard(initialData.mmc.devices[0]);
  assert.ok(!staleNvmeHtml.includes("nvme-badge-healthy"), "NVMe must not have healthy class when stale");
  assert.ok(staleNvmeHtml.includes("nvme-badge-unknown"), "NVMe must have unknown badge when stale");
  assert.ok(!staleMmcHtml.includes("nvme-badge-healthy"), "MMC must not have healthy class when stale");
  assert.ok(staleMmcHtml.includes("nvme-badge-unknown"), "MMC must have unknown badge when stale");
});

// 8. Retry restores new data/new time/new grade/stale=false
test("scenario 8: retry restores new data/new time/new grade/stale=false", async () => {
  const retryPending = deferred();

  const staleData = {
    nvme: {
      initialized: true,
      devices: [
        {
          name: "nvme0",
          healthState: "healthy",
          smart: { temperatureC: 40, availableSparePercent: 90, percentageUsed: 5 },
        },
      ],
    },
    mmc: { initialized: false, devices: [] },
  };

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: staleData,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: new Error("temporary error"),
    storageStale: true,
    storageRefreshedAt: 1000,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => retryPending.promise,
    },
    renderHardwareTool: () => {},
    Date: {
      now: () => 5000,
    },
    t: (key) => key,
    escapeHtml: (s) => String(s || ""),
    formatNumber: (n) => String(n),
    byteUnit: (b) => `${b} B`,
  };

  vm.createContext(context);
  vm.runInContext(
    [
      handler("renderNvmeDeviceCard"),
      handler("refreshStorageTool"),
    ].join("\n"),
    context
  );

  const req = context.refreshStorageTool();
  await Promise.resolve();

  const refreshedData = {
    nvme: {
      initialized: true,
      devices: [
        {
          name: "nvme0",
          healthState: "warning",
          smart: { temperatureC: 75, availableSparePercent: 80, percentageUsed: 92, warningFlags: ["temperature_warning"] },
        },
      ],
    },
    mmc: { initialized: false, devices: [] },
  };

  retryPending.resolve(refreshedData);
  await req;

  assert.equal(state.storageStale, false, "stale must be cleared");
  assert.equal(state.storageRefreshError, null, "error must be cleared");
  assert.equal(state.storageRefreshedAt, 5000, "refreshedAt must be updated");
  assert.equal(state.hardwareData, refreshedData);

  const cardHtml = context.renderNvmeDeviceCard(refreshedData.nvme.devices[0]);
  assert.ok(cardHtml.includes("nvme-badge-warning"), "card reflects new warning grade");
  assert.ok(!cardHtml.includes("nvme-badge-unknown"));
});

// 9. Empty lists still render refresh button
test("scenario 9: empty lists still render refresh button", () => {
  const elements = new Map();
  function getElement(sel) {
    if (!elements.has(sel)) {
      elements.set(sel, {
        innerHTML: "",
        textContent: "",
        disabled: false,
        dataset: {},
        style: { setProperty() {} },
      });
    }
    return elements.get(sel);
  }

  const state = {
    selectedHardware: "storage",
    hardwareData: {
      nvme: { initialized: true, devices: [] },
      mmc: { initialized: true, devices: [] },
    },
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    $: (sel) => getElement(sel),
    $$: () => [],
    t: (key) => key,
    escapeHtml: (s) => String(s || ""),
    preserveToolFocus: () => {},
    relativeTime: () => "just now",
    applyStorageMetricStyles: () => {},
  };

  vm.createContext(context);
  vm.runInContext(handler("renderStorageTool"), context);

  context.renderStorageTool();
  const host = getElement("[data-hardware-body]");
  assert.ok(host.innerHTML.includes("data-storage-refresh"), "refresh button must be rendered even when device list is empty");
});

// 10. Capture existing 10s callback and invoke manually => repeat transport and updated data without reopening
test("scenario 10: timer callback triggers transport and updates data without reopening drawer", async () => {
  let intervalCallback = null;
  const mockWindow = {
    setInterval: (fn, ms) => {
      if (ms === 10_000) intervalCallback = fn;
      return 123;
    },
    addEventListener: () => {},
  };

  let storageTransportCalls = 0;
  const pendingStorage = deferred();

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
    refreshRequested: false,
    refreshPromise: null,
    refreshLoud: false,
    refreshing: false,
    snapshot: { synthetic: false },
    sources: null,
  };

  const context = {
    state,
    window: mockWindow,
    document: { body: { dataset: {}, classList: { remove() {}, add() {} } } },
    transport: {
      storageStatus: () => {
        storageTransportCalls += 1;
        return pendingStorage.promise;
      },
      snapshot: async () => ({ synthetic: false, collectedAt: Date.now() }),
      actions: async () => [],
      activity: async () => [],
      sourceStatus: async () => ({ sourceRevision: "1" }),
    },
    renderHardwareTool: () => {},
    renderAll: () => {},
    setText: () => {},
    t: (key) => key,
    toast: () => {},
    displayError: (e) => String(e?.message || e),
    applyDebugDevice: (s) => s,
    Date,
  };

  vm.createContext(context);
  vm.runInContext(
    [
      handler("refreshStorageTool"),
      handler("refreshOnce"),
      handler("refreshAll"),
    ].join("\n"),
    context
  );

  // Invoke refreshAll as interval would
  const allPromise = context.refreshAll({ quiet: true });
  await Promise.resolve();

  assert.equal(storageTransportCalls, 1, "refreshAll must trigger storage refresh without reopening drawer");
  const storageTask = state.storageRefreshPromise;
  pendingStorage.resolve({ nvme: { initialized: true, devices: [{ name: "nvme0" }] }, mmc: { initialized: false, devices: [] } });
  await storageTask;
  await allPromise;

  assert.ok(state.hardwareData != null);
  assert.equal(state.hardwareLoadVersion, 1, "drawer version must not change");
});

// 11. Storage never resolves but snapshot does => refreshAll completes
test("scenario 11: storage never resolves but snapshot does => refreshAll completes", async () => {
  const unresolvingStorage = deferred(); // never resolves

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
    refreshRequested: false,
    refreshPromise: null,
    refreshLoud: false,
    refreshing: false,
    snapshot: null,
  };

  const context = {
    state,
    document: { body: { dataset: {}, classList: { remove() {}, add() {} } } },
    transport: {
      storageStatus: () => unresolvingStorage.promise,
      snapshot: async () => ({ synthetic: false, collectedAt: Date.now() }),
      actions: async () => [],
      activity: async () => [],
      sourceStatus: async () => ({ sourceRevision: "1" }),
    },
    renderHardwareTool: () => {},
    renderAll: () => {},
    setText: () => {},
    t: (key) => key,
    toast: () => {},
    displayError: (e) => String(e?.message || e),
    applyDebugDevice: (s) => s,
    Date,
  };

  vm.createContext(context);
  vm.runInContext(
    [
      handler("refreshStorageTool"),
      handler("refreshOnce"),
      handler("refreshAll"),
    ].join("\n"),
    context
  );

  // refreshAll must complete without waiting for unresolving storageStatus
  const refreshPromise = context.refreshAll({ quiet: true });
  await refreshPromise;

  assert.ok(state.snapshot != null, "snapshot was processed and refreshAll resolved");
  assert.equal(state.storageRefreshing, true, "storage is still ongoing in background without blocking refreshAll");
});

// 12. Synchronous transport throw => current handle clears and retry possible
test("scenario 12: synchronous transport throw => current handle clears and retry possible", async () => {
  let calls = 0;

  const state = {
    selectedHardware: "storage",
    hardwareLoadVersion: 1,
    hardwareData: null,
    storageRefreshPromise: null,
    storageRefreshing: false,
    storageRefreshError: null,
    storageStale: false,
    storageRefreshedAt: null,
  };

  const context = {
    state,
    transport: {
      storageStatus: () => {
        calls += 1;
        throw new Error("sync throw inside storageStatus");
      },
    },
    renderHardwareTool: () => {},
    Date,
  };

  vm.createContext(context);
  vm.runInContext(handler("refreshStorageTool"), context);

  const task = context.refreshStorageTool();
  await task;

  assert.equal(state.storageRefreshing, false, "storageRefreshing must reset to false");
  assert.equal(state.storageRefreshPromise, null, "storageRefreshPromise must reset to null");
  assert.ok(state.storageRefreshError != null, "error must be captured");
  assert.equal(state.storageRefreshError.message, "sync throw inside storageStatus");

  // Retry is possible
  const retryPending = deferred();
  context.transport.storageStatus = () => {
    calls += 1;
    return retryPending.promise;
  };

  const retryTask = context.refreshStorageTool();
  await Promise.resolve();

  assert.equal(calls, 2);
  assert.equal(state.storageRefreshing, true);
  retryPending.resolve({ nvme: { initialized: false, devices: [] }, mmc: { initialized: false, devices: [] } });
  await retryTask;

  assert.equal(state.storageRefreshing, false);
  assert.equal(state.storageRefreshPromise, null);
  assert.equal(state.storageRefreshError, null);
});

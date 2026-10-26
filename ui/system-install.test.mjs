import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";
import { handler as sharedHandler, loadRuntimeSource } from "./app-manifest.mjs";

const source = loadRuntimeSource();

// Static assertions below must judge executable code, not the comments that sit
// next to it (the comments deliberately talk about the fallback that was
// removed), so comments are stripped before scanning.
function codeOnly(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/[^\n]*/g, "");
}

const handler = (name) => sharedHandler(source, name);

function loadI18n(language) {  const attributes = {};
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

// Deterministic bytes formatter: the assertions below must not depend on the
// locale of the machine running the suite.
function bytes(value) {
  const number = Number(value || 0);
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let scaled = number;
  let index = 0;
  while (scaled >= 1024 && index < units.length - 1) { scaled /= 1024; index += 1; }
  return index === 0 ? `${scaled} B` : `${scaled.toFixed(1)} ${units[index]}`;
}

const INSTALL_FUNCTIONS = [
  "installTargetSelectable", "installSelectedTarget", "installKindLabel", "installFormatLabel",
  "installWarningCopy", "installErrorCodeList", "installErrorCopy", "installErrorFrom",
  "installPlanRequest", "installApplyRequest", "installPlanImageId", "installAcknowledgementReady", "installApplyDisabled",
  "installConfirmStateCopy", "installFact", "installTargetOptionLabel", "installTargetOptions",
  "installImageOptions", "renderInstallTargetCard", "renderInstallConsequences",
  "renderInstallConfirmation", "renderInstallLiveBanner", "renderInstallErrorBox",
  "renderInstallResult", "renderSystemInstallBody", "renderSystemInstallPage", "wireInstallEvents",
  "onInstallAcknowledgementInput", "openSystemInstallPage", "loadInstallOptions",
  "invalidateInstallPlan", "planSystemInstall", "applySystemInstall",
  "installPickerAvailable", "installSelectableImages", "pickInstallImage", "installHandleProblem",
  "renderInstallPickNotice",
];

const INITIAL_STATE = {
  installOptions: null,
  installLiveAllowed: false,
  installLoading: false,
  installLoadingPromise: null,
  installLoadError: null,
  installTargetId: "",
  installImageId: "",
  installPickedImage: null,
  installPicking: false,
  installPickNotice: null,
  installPlan: null,
  installPlanning: false,
  installAcknowledgement: "",
  installRunning: false,
  installError: null,
  installResult: null,
};

// `shell: true` injects the same capability the desktop shell provides
// (`window.__TAURI__.core.invoke`); without it the page must behave exactly like
// the browser, which has no chooser at all.
function createContext({ language = "en-US", state = {}, transport = {}, shell = false } = {}) {
  const elements = new Map();
  function getElement(selector) {
    if (!elements.has(selector)) {
      elements.set(selector, {
        innerHTML: "",
        textContent: "",
        value: "",
        disabled: false,
        hidden: false,
        dataset: {},
        classes: new Set(),
        classList: {
          add: (name) => elements.get(selector).classes.add(name),
          remove: (name) => elements.get(selector).classes.delete(name),
          toggle: (name, on) => (on ? elements.get(selector).classes.add(name) : elements.get(selector).classes.delete(name)),
        },
      });
    }
    return elements.get(selector);
  }
  const i18n = loadI18n(language);
  const toasts = [];
  const context = {
    state: { ...INITIAL_STATE, ...state },
    transport,
    toasts,
    $: (selector) => getElement(selector),
    $$: () => [],
    t: (key, params) => i18n.t(key, params),
    i18n,
    escapeHtml: (value) => String(value ?? "").replace(/[&<>"']/g, ""),
    byteUnit: bytes,
    icon: (name) => `<svg class="icon-${name}"></svg>`,
    displayError: (error) => String(error?.message || error),
    toast: (title, detail, error) => toasts.push({ title, detail, error }),
    setText: () => {},
  };
  if (shell) context.tauriInvoke = (...args) => context.invocations.push(args);
  context.invocations = [];
  vm.createContext(context);
  vm.runInContext(INSTALL_FUNCTIONS.map(handler).join("\n"), context);
  return { context, getElement, elements, toasts, i18n };
}

const DESTROYS = [
  { name: "mmcblk0p1", number: 1, startBytes: 0, sizeBytes: 268435456, mountPoint: null },
  { name: "mmcblk0p2", number: 2, startBytes: 268435456, sizeBytes: 1073741824, mountPoint: "/boot" },
];

function demoTarget(overrides = {}) {
  return {
    id: "sda",
    path: "/dev/sda",
    model: "UFS 128GB Demo",
    sizeBytes: 128035676160,
    kind: "ufs",
    removable: true,
    mounted: false,
    mountPoints: [],
    holdsRunningSystem: false,
    partitions: DESTROYS.map((partition) => ({ ...partition })),
    ...overrides,
  };
}

function demoImage(overrides = {}) {
  return {
    path: "/var/lib/rsetup-next/images/demo.img",
    id: "demo.img",
    format: "raw",
    sizeBytes: 2097152,
    uncompressedBytes: 2097152,
    sha256Prefix: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    prefixBytes: 2097152,
    partitionTable: "gpt",
    partitions: [],
    ...overrides,
  };
}

function demoPlan(overrides = {}) {
  const target = demoTarget(overrides.target);
  const plan = {
    synthetic: true,
    revision: "revision-1",
    planToken: "install-plan-0f1e2d3c4b5a6978",
    createdAt: "2026-01-01T00:00:00Z",
    request: { targetId: target.id, imageId: "demo.img" },
    target,
    image: demoImage(overrides.image),
    destroys: target.partitions,
    warnings: [
      "writing_an_image_replaces_every_partition_on_the_target",
      "the_write_cannot_be_rolled_back",
      "the_target_is_removable_media",
    ],
    requiresRoot: true,
    chunkBytes: 4194304,
    ...overrides,
  };
  if (!overrides.destroys) plan.destroys = target.partitions;
  return plan;
}

function demoOptions(overrides = {}) {
  return {
    liveAllowed: true,
    targets: [demoTarget(), demoTarget({ id: "mmcblk0", path: "/dev/mmcblk0", model: "eMMC 32GB", kind: "emmc", sizeBytes: 32130314240, removable: false, mounted: true, mountPoints: ["/"], holdsRunningSystem: true })],
    images: [{ id: "demo.img", sizeBytes: 2097152 }],
    ...overrides,
  };
}

function applySucceeded(plan = demoPlan()) {
  return {
    run: { id: "run-1", actionId: "system.install", status: "succeeded", synthetic: true, summary: "Wrote 2097152 bytes." },
    plan,
    bytesWritten: 2097152,
    verification: {
      state: "matched",
      expected: DESTROYS.map((partition) => ({ ...partition })),
      observed: DESTROYS.map((partition) => ({ ...partition })),
      detail: null,
    },
  };
}

test("consequence list shows target identity, destroyed partitions, image identity and both warnings", () => {
  const expectations = {
    "en-US": { kind: "UFS", removable: "Yes — removable", mounted: "Not mounted", format: /Formatting/, irreversible: /cannot be rolled back/, replaceAll: /replaces every partition/ },
    "zh-CN": { kind: "UFS", removable: "是 —— 可移动", mounted: "未挂载", format: /格式化/, irreversible: /无法回滚/, replaceAll: /替换目标设备上的全部分区/ },
  };
  for (const [language, expected] of Object.entries(expectations)) {
    const { context, getElement } = createContext({
      language,
      state: { installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img", installPlan: demoPlan() },
    });
    const html = context.renderInstallConsequences(demoPlan());

    // Target identity: path / model / capacity / kind / removability / mount state.
    assert.ok(html.includes("/dev/sda"), language);
    assert.ok(html.includes("UFS 128GB Demo"), language);
    assert.ok(html.includes("119.2 GiB"), language);
    assert.ok(html.includes(expected.kind), language);
    assert.ok(html.includes(expected.removable), language);
    assert.ok(html.includes(expected.mounted), language);
    assert.ok(html.includes("data-install-plan-target"), language);

    // Every existing partition that the write destroys, with size and mount point.
    assert.ok(html.includes("data-install-destroys"), language);
    assert.ok(html.includes("mmcblk0p1") && html.includes("mmcblk0p2"), language);
    assert.ok(html.includes("256.0 MiB") && html.includes("1.0 GiB"), language);
    assert.ok(html.includes("/boot"), language);

    // Image identity: catalogue id, resolved path, format, sizes, table, sha256.
    assert.ok(html.includes("demo.img"), language);
    assert.ok(html.includes("/var/lib/rsetup-next/images/demo.img"), language);
    assert.ok(html.includes("gpt"), language);
    assert.ok(html.includes("2.0 MiB"), language);
    assert.ok(html.includes("9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"), language);

    // Formatting and irreversible warnings are always present, plus the core's own warnings.
    assert.ok(html.includes("data-install-format-warning"), language);
    assert.ok(html.includes("data-install-irreversible-warning"), language);
    assert.match(html, expected.format, language);
    assert.match(html, expected.irreversible, language);
    assert.match(html, expected.replaceAll, language);
    assert.ok(html.includes("data-install-admin-warning"), language);

    // The page never emits inline style attributes (CSP hardening).
    assert.doesNotMatch(html, /\sstyle="/, language);

    // Mounted / running-system targets are still described honestly.
    const risky = context.renderInstallTargetCard(demoTarget({
      path: "/dev/mmcblk0", mounted: true, mountPoints: ["/", "/boot"], holdsRunningSystem: true,
    }));
    assert.ok(risky.includes("data-install-target-card"), language);
    assert.ok(risky.includes("/dev/mmcblk0"), language);
    assert.ok(language === "zh-CN" ? risky.includes("已挂载于 /, /boot") : risky.includes("Mounted at /, /boot"), language);

    // A target without a partition table says so instead of rendering an empty list.
    const empty = context.renderInstallConsequences(demoPlan({ destroys: [] }));
    assert.ok(empty.includes("data-install-destroys-empty"), language);
  }
});

test("the destructive button stays disabled until the typed name matches the target", () => {
  for (const language of ["en-US", "zh-CN"]) {
    const plan = demoPlan();
    const { context, getElement } = createContext({
      language,
      state: { installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img", installPlan: plan },
    });
    const host = getElement("[data-install-body]");
    context.renderSystemInstallPage();

    const button = getElement("[data-install-apply]");
    assert.equal(button.disabled, true, `${language}: a plan alone must not enable the write`);
    assert.equal(context.installApplyDisabled(), true, language);
    assert.match(host.innerHTML, /data-install-acknowledge/, language);
    assert.match(host.innerHTML, /data-install-apply[^>]*disabled/, language);
    assert.match(host.innerHTML, /data-install-expected="sda"/, language);

    // Boolean-looking acknowledgements can never satisfy the predicate.
    for (const wrong of ["true", "yes", "1", "sda1", "/dev/sda", "SDA", "mmcblk0", "sda p1"]) {
      context.onInstallAcknowledgementInput(wrong, host);
      assert.equal(context.installAcknowledgementReady(plan, wrong), false, `${language}: ${wrong}`);
      assert.equal(button.disabled, true, `${language}: ${wrong} must not enable the write`);
      assert.equal(context.installApplyDisabled(), true, `${language}: ${wrong} must not enable the write`);
    }

    // The exact device name enables it; surrounding whitespace is trimmed by the core too.
    context.onInstallAcknowledgementInput("sda", host);
    assert.equal(context.installAcknowledgementReady(plan, "sda"), true, language);
    assert.equal(button.disabled, false, language);
    assert.equal(context.installApplyDisabled(), false, language);
    assert.match(getElement("[data-install-confirm-state]").textContent, /sda/, language);

    context.onInstallAcknowledgementInput("  sda  ", host);
    assert.equal(button.disabled, false, language);

    context.onInstallAcknowledgementInput("", host);
    assert.equal(button.disabled, true, language);
    assert.equal(context.installApplyDisabled(), true, language);
  }
});

test("apply submits the typed acknowledgement string, never a boolean confirm or an image path", async () => {
  const calls = [];
  const { context, getElement } = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
      installPlan: demoPlan(), installAcknowledgement: "sda",
    },
    transport: { installApply: async (payload) => { calls.push(payload); return applySucceeded(); } },
  });

  const plan = demoPlan();
  const payload = context.installApplyRequest(plan, "sda");
  assert.deepEqual(Object.keys(payload).sort(), ["acknowledgement", "imageId", "planToken", "targetId"]);
  assert.equal(typeof payload.acknowledgement, "string");
  assert.equal(payload.acknowledgement, "sda");
  assert.equal("confirm" in payload, false);
  assert.equal("imagePath" in payload, false);
  assert.equal("imagePath" in context.installPlanRequest("sda", "demo.img"), false);
  const planRequest = context.installPlanRequest("sda", "demo.img");
  assert.deepEqual(Object.keys(planRequest).sort(), ["imageId", "targetId"]);
  assert.equal(planRequest.targetId, "sda");
  assert.equal(planRequest.imageId, "demo.img");

  await context.applySystemInstall();
  assert.equal(calls.length, 1);
  assert.equal(typeof calls[0].acknowledgement, "string");
  assert.equal(calls[0].acknowledgement, "sda");
  assert.equal(calls[0].planToken, plan.planToken);
  assert.equal(calls[0].imageId, "demo.img");
  assert.equal("confirm" in calls[0], false);
  assert.equal(context.state.installResult.bytesWritten, 2097152);
  const resultHtml = getElement("[data-install-body]").innerHTML;
  assert.match(resultHtml, /data-install-result/);
  assert.match(resultHtml, /2.0 MiB/);
  assert.match(resultHtml, /data-install-result-synthetic/);

  // A mismatched name is never submitted, and the refusal is explained.
  const guardedCalls = [];
  const guarded = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
      installPlan: demoPlan(), installAcknowledgement: "mmcblk0",
    },
    transport: { installApply: async (payload) => { guardedCalls.push(payload); return applySucceeded(); } },
  });
  assert.equal(await guarded.context.applySystemInstall(), null);
  assert.equal(guardedCalls.length, 0);
  assert.equal(guarded.context.state.installError.code, "acknowledgement_mismatch");
  assert.equal(guarded.context.state.installResult, null);

  // An empty field is the core's confirmation_required, not a boolean.
  const empty = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
      installPlan: demoPlan(), installAcknowledgement: "",
    },
    transport: { installApply: async () => { throw new Error("must not be called"); } },
  });
  assert.equal(await empty.context.applySystemInstall(), null);
  assert.equal(empty.context.state.installError.code, "confirmation_required");
});

test("each stable error code renders its own localized message", () => {
  const codes = [
    "confirmation_required", "acknowledgement_mismatch", "plan_required", "stale_plan",
    "root_required", "install_conflict", "writes_running_system", "install_unsupported",
    "invalid_install_request", "internal_error",
  ];
  const expectations = {
    "en-US": {
      confirmation_required: /No device name was typed/,
      acknowledgement_mismatch: /“mmcblk0” was typed, but writing \/dev\/sda requires exactly “sda”/,
      stale_plan: /Build a fresh plan/,
      install_unsupported: /Live execution is not enabled/,
      nothingWritten: /Nothing was written/,
    },
    "zh-CN": {
      confirmation_required: /尚未键入设备名/,
      acknowledgement_mismatch: /“mmcblk0”/,
      stale_plan: /重新生成计划/,
      install_unsupported: /未启用实时写入/,
      nothingWritten: /没有写入任何数据/,
    },
  };
  for (const [language, expected] of Object.entries(expectations)) {
    const { context } = createContext({
      language,
      state: { installPlan: demoPlan(), installAcknowledgement: "mmcblk0", installError: { code: "confirmation_required", message: "core message" } },
    });
    const titles = new Set();
    for (const code of codes) {
      const copy = context.installErrorCopy({ code, message: "core message" });
      assert.equal(copy.code, code, `${language} ${code}`);
      assert.notEqual(copy.title, "", `${language} ${code}`);
      assert.ok(copy.hint.length > 0, `${language} ${code} hint`);
      titles.add(copy.title);
    }
    // No two codes collapse into the same sentence.
    assert.equal(titles.size, codes.length, `${language}: every code needs its own copy`);

    // Every code renders its own message, addressed by the code itself.
    for (const code of codes) {
      context.state.installError = { code, message: "core message" };
      const box = context.renderInstallErrorBox();
      assert.match(box, new RegExp(`data-install-error-code="${code}"`), `${language} ${code}`);
      assert.ok(box.includes(context.installErrorCopy({ code, message: "core message" }).title), `${language} ${code} title`);
      assert.ok(box.includes(context.installErrorCopy({ code, message: "core message" }).hint), `${language} ${code} hint`);
    }

    for (const code of ["confirmation_required", "acknowledgement_mismatch", "stale_plan", "install_unsupported"]) {
      context.state.installError = { code, message: "core message" };
      assert.match(context.renderInstallErrorBox(), expected[code], `${language} ${code}`);
    }
    assert.match(context.installErrorCopy({ code: "install_unsupported", message: "" }).detail, expected.nothingWritten, language);

    // The mismatch copy names the expected device and what was typed.
    context.state.installError = { code: "acknowledgement_mismatch", message: "core message" };
    const mismatch = context.renderInstallErrorBox();
    assert.match(mismatch, /mmcblk0/, `${language}: provided value must be shown`);
    assert.match(mismatch, /sda/, `${language}: expected device name must be shown`);
    assert.equal(context.installErrorCopy({ code: "acknowledgement_mismatch" }).replan, false, language);
    assert.equal(context.installErrorCopy({ code: "stale_plan" }).replan, true, language);
    assert.equal(context.installErrorCopy({ code: "install_unsupported" }).liveDisabled, true, language);

    // Unknown codes still produce readable, distinct copy instead of the raw key.
    const unknown = context.installErrorCopy({ code: "surface_teapot", message: "teapot" });
    assert.equal(unknown.code, "internal_error", language);
    assert.ok(unknown.title.length > 0, language);
  }
});

test("the live-disabled surface is refused explicitly, not reported as success", async () => {
  const expectations = {
    "en-US": { notice: /Live execution is not enabled/, refused: /Live execution is not enabled\./, detail: /Nothing was written/ },
    "zh-CN": { notice: /未启用实时写入/, refused: /未启用实时写入。/, detail: /没有写入任何数据/ },
  };
  for (const [language, expected] of Object.entries(expectations)) {
    const refusal = new Error("Live execution is not enabled for the fixed action catalog. Nothing was written.");
    refusal.code = "install_unsupported";
    const { context, getElement, toasts } = createContext({
      language,
      state: {
        installOptions: demoOptions({ liveAllowed: false }), installTargetId: "sda",
        installImageId: "demo.img", installPlan: demoPlan(), installAcknowledgement: "sda",
      },
      transport: { installApply: async () => { throw refusal; } },
    });

    // `liveAllowed` is read before anything else and shown to the user.
    context.renderSystemInstallPage();
    const before = getElement("[data-install-body]").innerHTML;
    assert.match(before, /data-install-live-banner[^>]*data-live-allowed="false"/, language);
    assert.match(before, expected.notice, language);
    assert.equal(context.state.installOptions.liveAllowed, false, language);

    assert.equal(await context.applySystemInstall(), null);
    assert.equal(context.state.installResult, null, language);
    assert.equal(context.state.installError.code, "install_unsupported", language);

    const after = getElement("[data-install-body]").innerHTML;
    assert.match(after, /data-install-error-code="install_unsupported"/, language);
    assert.match(after, expected.refused, language);
    assert.match(after, expected.detail, language);
    assert.doesNotMatch(after, /install-result/, `${language}: a refusal must never render the success panel`);
    assert.equal(toasts.length, 1, language);
    assert.equal(toasts[0].error, true, language);
  }

  // With live writes enabled the banner says so and names the destruction.
  const live = createContext({ state: { installOptions: demoOptions({ liveAllowed: true }) } });
  live.context.renderSystemInstallPage();
  const liveHtml = live.getElement("[data-install-body]").innerHTML;
  assert.match(liveHtml, /data-install-live-banner[^>]*data-live-allowed="true"/);
  assert.match(liveHtml, /Live writes are enabled/);
  assert.match(liveHtml, /destroys its partitions/);

  // Loading the catalogue reports the policy flag from the server payload.
  const loaded = createContext({ transport: { installTargets: async () => demoOptions({ liveAllowed: false }) } });
  await loaded.context.loadInstallOptions();
  assert.equal(loaded.context.state.installLiveAllowed, false);
  assert.equal(loaded.context.state.installLoadingPromise, null);
  assert.equal(loaded.context.state.installTargetId, "sda");
  assert.equal(loaded.context.state.installImageId, "demo.img");
  assert.match(loaded.getElement("[data-install-body]").innerHTML, /data-live-allowed="false"/);
});

test("a stale plan is dropped and the page asks for a fresh one", async () => {
  const stale = new Error("the target changed after the plan was built");
  stale.code = "stale_plan";
  const { context, getElement, toasts } = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
      installPlan: demoPlan(), installAcknowledgement: "sda",
    },
    transport: { installApply: async () => { throw stale; } },
  });
  assert.equal(await context.applySystemInstall(), null);
  assert.equal(context.state.installPlan, null, "a stale plan must not be replayed");
  assert.equal(context.state.installAcknowledgement, "");
  assert.equal(context.installApplyDisabled(), true);

  const html = getElement("[data-install-body]").innerHTML;
  assert.match(html, /data-install-error-code="stale_plan"/);
  assert.match(html, /data-install-replan/);
  assert.doesNotMatch(html, /data-install-apply/);
  assert.equal(toasts.length, 1);
  assert.equal(toasts[0].error, true);
});

test("the plan's echoed image id is the only source for an apply", async () => {
  // t22 makes both surfaces echo the resolved catalogue id back in
  // `plan.image.id`. That response is the single source: the page must not fall
  // back to the selection it remembers, which could disagree with the plan.
  const plan = demoPlan();
  const calls = [];
  const { context, getElement } = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda",
      // Deliberately a *different* id than the plan echoed: the remembered
      // selection must never reach the apply payload.
      installImageId: "other.img",
      installPlan: plan, installAcknowledgement: "sda",
    },
    transport: { installApply: async (payload) => { calls.push(payload); return applySucceeded(plan); } },
  });

  assert.equal(context.installPlanImageId(plan), plan.image.id);
  assert.equal(context.installApplyRequest(plan, "sda").imageId, plan.image.id);
  assert.equal(context.installApplyRequest.length, 2, "the builder takes no second image id");

  await context.applySystemInstall();
  assert.equal(calls.length, 1);
  assert.equal(calls[0].imageId, plan.image.id, "imageId == plan.image.id");
  assert.notEqual(calls[0].imageId, context.state.installImageId, "the remembered selection must not be used");
  assert.equal(calls[0].targetId, plan.target.id);
  assert.equal(typeof calls[0].acknowledgement, "string");

  const html = context.renderInstallConsequences(plan);
  assert.ok(html.includes("/var/lib/rsetup-next/images/demo.img"), "the resolved path is shown");
  assert.ok(html.includes(plan.image.id), "the catalogue id shown is the plan's echoed id");
  assert.doesNotMatch(html, /other\.img/, "the remembered selection is never rendered as the plan identity");
  assert.ok(html.includes("Not mounted"), "a missing mount point is stated, not rendered as undefined");
  assert.doesNotMatch(html, /undefined/);
  assert.ok(getElement("[data-install-body]").innerHTML.includes("data-install-result"));
});

test("no second image id source survives in the apply path", async () => {
  const { context } = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
      installPlan: demoPlan(), installAcknowledgement: "sda",
    },
  });
  const applyBuilder = codeOnly(handler("installApplyRequest"));
  const applyFunction = codeOnly(handler("applySystemInstall"));
  const planIdHelper = codeOnly(handler("installPlanImageId"));
  // The apply path may only read the id out of the plan response.
  assert.doesNotMatch(applyBuilder, /installImageId/, "the builder must not read the remembered selection");
  assert.doesNotMatch(applyBuilder, /request\.imageId|request\?\.imageId/, "no request.imageId fallback");
  assert.doesNotMatch(applyBuilder, /fallback/, "no fallback argument");
  assert.doesNotMatch(applyBuilder, /imageId: String\(/, "the id is not re-derived from a parameter");
  assert.match(applyBuilder, /imageId: installPlanImageId\(plan\)/, "the id comes from the plan helper");
  assert.doesNotMatch(applyFunction, /installImageId/, "applySystemInstall must not pass the remembered selection");
  assert.doesNotMatch(planIdHelper, /plan\?\.request|state\.|fallback/, "the plan helper has exactly one source");
  assert.match(planIdHelper, /plan\?\.image\?\.id/);
  assert.equal(context.installPlanImageId.length, 1, "no fallback parameter");
  // The removed fallback is documented where it used to live.
  assert.match(handler("installApplyRequest"), /single source|exactly one place/i);
  // The remembered selection still exists, but only to build the *plan* request.
  assert.match(codeOnly(handler("planSystemInstall")), /state\.installImageId/, "the selection still builds the plan request");
  assert.equal("imageId" in context.installPlanRequest("sda", "demo.img"), true);

  // An older response that does not echo the id yields an empty value instead of
  // silently falling back: the surface then refuses with invalid_install_request.
  const legacy = demoPlan();
  delete legacy.image.id;
  legacy.request = { targetId: "sda", imagePath: "/var/lib/rsetup-next/images/demo.img" };
  assert.equal(context.installPlanImageId(legacy), "");
  const payload = context.installApplyRequest(legacy, "sda");
  assert.equal(payload.imageId, "");
  assert.deepEqual(Object.keys(payload).sort(), ["acknowledgement", "imageId", "planToken", "targetId"]);
});

test("a legacy plan without an echoed id is refused, never silently patched", async () => {
  const legacy = demoPlan();
  delete legacy.image.id;
  legacy.request = { targetId: "sda", imagePath: "/var/lib/rsetup-next/images/demo.img" };
  const refusal = new Error("the image id must be a plain file name, not a path");
  refusal.code = "invalid_install_request";
  const calls = [];
  const { context, getElement } = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
      installPlan: legacy, installAcknowledgement: "sda",
    },
    transport: {
      installApply: async (payload) => {
        calls.push(payload);
        // The surface rejects an empty id before it ever looks at the device.
        assert.equal(payload.imageId, "");
        throw refusal;
      },
    },
  });
  assert.equal(await context.applySystemInstall(), null);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].imageId, "", "no fallback to the remembered selection");
  assert.equal(context.state.installError.code, "invalid_install_request");
  assert.equal(context.state.installResult, null);
  const html = getElement("[data-install-body]").innerHTML;
  assert.match(html, /data-install-error-code="invalid_install_request"/);
  assert.doesNotMatch(html, /install-result/);
});

const PICK_HANDLE = "pick-0123456789abcdef0123456789abcdef-demo.img";

function pickedResponse(overrides = {}) {
  return { cancelled: false, handle: PICK_HANDLE, name: "demo.img", sizeBytes: 2097152, ...overrides };
}

test("the desktop shell offers the native chooser and adopts its opaque handle", async () => {
  const calls = [];
  const { context, getElement } = createContext({
    shell: true,
    state: { installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img" },
    transport: { pickInstallImage: async () => { calls.push("pick"); return pickedResponse(); } },
  });
  assert.equal(context.installPickerAvailable(), true);

  context.renderSystemInstallPage();
  const before = getElement("[data-install-body]").innerHTML;
  assert.match(before, /data-install-pick-image/, "the desktop shell offers the chooser");
  assert.match(before, /Choose image file/);
  assert.match(before, /native file chooser/);
  assert.doesNotMatch(before, /data-install-picked-image/, "nothing is chosen yet");

  const selection = await context.pickInstallImage();
  assert.equal(calls.length, 1, "the chooser command is called exactly once");
  assert.equal(selection, PICK_HANDLE, "the handle becomes the plan selection");
  assert.equal(context.state.installImageId, PICK_HANDLE);
  assert.equal(context.state.installPickedImage.handle, PICK_HANDLE);
  assert.equal(context.state.installPickedImage.name, "demo.img");
  assert.equal(context.state.installPickedImage.sizeBytes, 2097152);
  assert.equal(context.state.installPickedImage.path, undefined, "the pick response carries no path");
  assert.equal(context.state.installPlan, null, "a new choice invalidates the previous plan");
  assert.equal(PICK_HANDLE.includes("/"), false, "a handle is never a path");

  const after = getElement("[data-install-body]").innerHTML;
  assert.match(after, /data-install-picked-image/, "the chosen file is shown");
  assert.match(after, /data-install-picked-name="demo\.img"/, "the file name is shown");
  assert.match(after, /Chosen file: demo\.img · 2\.0 MiB/, "the file name and size are shown");
  assert.match(after, new RegExp(`<option value="${PICK_HANDLE}" selected data-install-image-picked>`), "the choice is one entry in the same list");
  assert.doesNotMatch(after, /\/tmp\/|\.\.\//, "no local path is rendered from the pick response");

  // The chooser command itself is the desktop shell's, and takes no arguments.
  assert.match(source, /tauriInvoke\("system_install_pick_image"\)/);
  assert.doesNotMatch(source, /tauriInvoke\("system_install_pick_image",/);
});

test("a chosen file is planned and applied like any other source, with the plan's identity", async () => {
  const plan = demoPlan();
  plan.image.id = PICK_HANDLE;
  const applied = [];
  const { context, getElement } = createContext({
    shell: true,
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: PICK_HANDLE,
      installPickedImage: { handle: PICK_HANDLE, name: "demo.img", sizeBytes: 2097152 },
      installPlan: plan,
    },
    transport: { installApply: async (payload) => { applied.push(payload); return applySucceeded(plan); } },
  });
  context.renderSystemInstallPage();

  // The existing disabled logic is untouched by the new source.
  const host = getElement("[data-install-body]");
  assert.equal(getElement("[data-install-apply]").disabled, true, "disabled before the name matches");
  context.onInstallAcknowledgementInput(`${PICK_HANDLE.slice(0, 12)}`, host);
  assert.equal(getElement("[data-install-apply]").disabled, true);
  context.onInstallAcknowledgementInput("sda", host);
  assert.equal(getElement("[data-install-apply]").disabled, false, "enabled once the target name matches");

  await context.applySystemInstall();
  assert.equal(applied.length, 1);
  assert.equal(applied[0].imageId, plan.image.id, "imageId == plan.image.id for the handle source");
  assert.equal(applied[0].imageId, PICK_HANDLE);
  assert.deepEqual(Object.keys(applied[0]).sort(), ["acknowledgement", "imageId", "planToken", "targetId"]);
  assert.equal(typeof applied[0].acknowledgement, "string");

  const html = getElement("[data-install-body]").innerHTML;
  assert.match(html, new RegExp(PICK_HANDLE), "the plan echoes the handle back");
  assert.match(html, /data-install-result/);
});

test("the browser keeps the catalogue only and never shows a local file chooser", async () => {
  const picks = [];
  const { context, getElement } = createContext({
    state: { installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img" },
    transport: { pickInstallImage: async () => { picks.push("pick"); return pickedResponse(); } },
  });
  assert.equal(context.installPickerAvailable(), false, "no desktop shell in the browser");

  context.renderSystemInstallPage();
  const html = getElement("[data-install-body]").innerHTML;
  assert.doesNotMatch(html, /data-install-pick-image/, "no chooser entry in the browser");
  assert.doesNotMatch(html, /data-install-pick-hint/);
  assert.doesNotMatch(html, /type="file"/, "no file input in the browser");
  assert.doesNotMatch(html, /Choose image file/);
  assert.doesNotMatch(html, /install-pick-button/);
  assert.match(html, /data-install-images/, "the fixed catalogue list stays");
  assert.match(html, /demo\.img/, "the catalogue id is still the source");

  // Even a direct call performs no selection and no I/O in the browser.
  assert.equal(await context.pickInstallImage(), null);
  assert.equal(picks.length, 0, "the browser never invokes the picker");

  // And a picked entry cannot appear in the browser list, even if state were
  // polluted: the entry is gated on the same capability.
  context.state.installPickedImage = { handle: PICK_HANDLE, name: "demo.img", sizeBytes: 2097152 };
  context.renderSystemInstallPage();
  const polluted = getElement("[data-install-body]").innerHTML;
  assert.doesNotMatch(polluted, /data-install-image-picked/);
  assert.doesNotMatch(polluted, /data-install-picked-image/);
  assert.equal(context.installSelectableImages(demoOptions().images).length, 1, "only the catalogue is selectable");

  // The browser plan request still submits a catalogue id only.
  const payload = context.installApplyRequest(demoPlan(), "sda");
  assert.equal(payload.imageId, "demo.img");
  assert.deepEqual(Object.keys(payload).sort(), ["acknowledgement", "imageId", "planToken", "targetId"]);
});

test("both image sources share one payload shape with no path and no confirm", () => {
  const { context } = createContext({ shell: true });
  const idPlan = demoPlan();
  const handlePlan = demoPlan();
  handlePlan.image.id = PICK_HANDLE;
  for (const plan of [idPlan, handlePlan]) {
    const payload = context.installApplyRequest(plan, "sda");
    assert.deepEqual(Object.keys(payload).sort(), ["acknowledgement", "imageId", "planToken", "targetId"]);
    assert.equal(payload.imageId, plan.image.id);
    for (const forbidden of ["confirm", "path", "imagePath", "file", "handle", "acknowledgementConfirmed"]) {
      assert.equal(forbidden in payload, false, `${forbidden} must never be sent`);
    }
  }
  // A chosen file in state never becomes a second identity for the payload.
  context.state.installPickedImage = { handle: "pick-ffffffffffffffffffffffffffffffff-other.img", name: "other.img", sizeBytes: 1 };
  assert.equal(context.installApplyRequest(handlePlan, "sda").imageId, handlePlan.image.id);
  assert.equal(context.installApplyRequest(idPlan, "sda").imageId, idPlan.image.id);
});

test("cancelling the chooser is reported as an answer, never as an error or a success", async () => {
  const expectations = {
    "en-US": /No image file was chosen/,
    "zh-CN": /未选择任何镜像文件/,
  };
  for (const [language, expected] of Object.entries(expectations)) {
    const { context, getElement, toasts } = createContext({
      language,
      shell: true,
      state: {
        installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
        installResult: applySucceeded(),
      },
      transport: { pickInstallImage: async () => ({ cancelled: true, handle: null, name: null, sizeBytes: null }) },
    });
    assert.equal(await context.pickInstallImage(), null);
    assert.equal(context.state.installError, null, "a cancellation is not an error");
    assert.equal(context.state.installResult, null, "an older success panel must not survive a cancellation");
    assert.equal(context.state.installImageId, "demo.img", "the previous selection stands");

    const html = getElement("[data-install-body]").innerHTML;
    assert.match(html, /data-install-pick-notice="cancelled"/, language);
    assert.match(html, expected, language);
    assert.doesNotMatch(html, /data-install-error/, `${language}: cancellation is not an error`);
    assert.doesNotMatch(html, /install-result/, `${language}: cancellation is not a success`);
    assert.equal(toasts.length, 0, language);
  }
});

test("a broken chooser or a rejected file is explained without a success panel", async () => {
  const cases = [
    {
      code: "install_unsupported",
      message: "zenity is not available: No such file or directory",
      title: /The file chooser could not run|无法启动文件选择器/,
    },
    {
      code: "invalid_install_request",
      message: "the chosen file is not a regular file",
      title: /That file cannot be used as a system image|该文件不能作为系统镜像使用/,
    },
  ];
  for (const language of ["en-US", "zh-CN"]) {
    for (const item of cases) {
      const failure = new Error(item.message);
      failure.code = item.code;
      const { context, getElement } = createContext({
        language,
        shell: true,
        state: {
          installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
          installPlan: demoPlan(), installAcknowledgement: "sda",
        },
        transport: { pickInstallImage: async () => { throw failure; } },
      });
      assert.equal(await context.pickInstallImage(), null);
      assert.equal(context.state.installResult, null, language);
      assert.equal(context.state.installError.code, item.code, language);

      const html = getElement("[data-install-body]").innerHTML;
      assert.match(html, new RegExp(`data-install-error-code="${item.code}"`), `${language} ${item.code}`);
      assert.match(html, item.title, `${language} ${item.code}`);
      assert.match(html, /zenity is not available|not a regular file/, `${language}: the surface reason is shown`);
      assert.match(html, /data-install-repick/, `${language}: the desktop offers "choose again"`);
      assert.doesNotMatch(html, /install-result/, `${language}: a failure is never a success`);
    }
  }
});

test("a refused or expired handle is explained next to its unchanged stable code", async () => {
  const cases = [
    {
      message: "invalid system install request: the image handle is not one this process issued; choose the image through the desktop shell",
      title: /The image handle was not issued by this session|该镜像句柄不是本次会话签发的/,
      hint: /Choose the image file again|请重新选择镜像文件/,
    },
    {
      message: "invalid system install request: the image handle has expired; choose the image again",
      title: /The image handle has expired|该镜像句柄已过期/,
      hint: /Choose the image file again|请重新选择镜像文件/,
    },
  ];
  for (const language of ["en-US", "zh-CN"]) {
    for (const item of cases) {
      const failure = new Error(item.message);
      failure.code = "invalid_install_request";
      const { context, getElement } = createContext({
        language,
        shell: true,
        state: {
          installOptions: demoOptions(), installTargetId: "sda", installImageId: PICK_HANDLE,
          installPickedImage: { handle: PICK_HANDLE, name: "demo.img", sizeBytes: 2097152 },
        },
        transport: { installPlan: async () => { throw failure; } },
      });
      assert.equal(await context.planSystemInstall(), null);
      assert.equal(context.state.installError.code, "invalid_install_request", language);
      assert.equal(context.state.installResult, null, language);
      assert.equal(context.state.installPlan, null, language);

      const html = getElement("[data-install-body]").innerHTML;
      assert.match(html, /data-install-error-code="invalid_install_request"/, `${language}: the stable code is still shown`);
      assert.match(html, item.title, language);
      assert.match(html, item.hint, language);
      assert.match(html, /data-install-repick/, language);
      assert.doesNotMatch(html, /install-result/, language);
    }
  }

  // The chooser introduces no new error code: the vocabulary is unchanged.
  const { context } = createContext({ shell: true });
  assert.equal(context.installErrorCodeList().join(","), [
    "confirmation_required", "acknowledgement_mismatch", "plan_required", "stale_plan",
    "root_required", "install_conflict", "writes_running_system", "install_unsupported",
    "invalid_install_request", "authorization_canceled", "internal_error",
  ].join(","));
  assert.equal(context.installHandleProblem("the image handle has expired"), "expired");
  assert.equal(context.installHandleProblem("the image handle is not one this process issued"), "unknown");
  assert.equal(context.installHandleProblem("a plan conflict"), null, "unrelated failures keep their own copy");
});

// The real `transport` object, lifted out of app.js verbatim so the command
// names and wire shapes are exercised as shipped rather than as stubbed.
function transportSource() {
  const start = source.indexOf("const transport = {");
  assert.ok(start >= 0, "the transport object must exist");
  const end = source.indexOf("\n};", start);
  assert.ok(end > start, "the transport object must be terminated");
  return source.slice(start, end + 3);
}

test("the picker goes through the shared transport, and only in the desktop shell", async () => {
  const shell = { request: async () => ({}), installPlanRequest: (targetId, imageId) => ({ targetId, imageId }), invocations: [] };
  shell.tauriInvoke = (...args) => { shell.invocations.push(args); return { cancelled: true, handle: null, name: null, sizeBytes: null }; };
  vm.createContext(shell);
  vm.runInContext(`${transportSource()}\nglobalThis.__transport = transport;`, shell);

  const picked = await shell.__transport.pickInstallImage();
  assert.equal(shell.invocations.length, 1);
  assert.equal(shell.invocations[0].length, 1, "the chooser command takes no arguments");
  assert.equal(shell.invocations[0][0], "system_install_pick_image");
  assert.equal(picked.cancelled, true);

  // The handle travels as the same `imageId` field a catalogue id uses.
  await shell.__transport.installPlan("sda", PICK_HANDLE);
  const planCall = shell.invocations.at(-1);
  assert.equal(planCall[0], "system_install_plan");
  assert.equal(planCall[1].imageId, PICK_HANDLE);
  assert.equal("imagePath" in planCall[1], false, "no path field is ever sent");
  assert.deepEqual(Object.keys(planCall[1]).sort(), ["imageId", "targetId"]);

  // The browser: no shell, no chooser, no invocation, and the plan request is
  // still a plain JSON body with the catalogue id.
  const browserCalls = [];
  const browser = { request: async (path, options) => { browserCalls.push({ path, options }); return {}; }, installPlanRequest: (targetId, imageId) => ({ targetId, imageId }) };
  vm.createContext(browser);
  // In the browser `tauriInvoke` exists and is undefined (it is read from
  // `window.__TAURI__`), so the transport branch is `!undefined`, not a missing
  // binding.
  vm.runInContext("const tauriInvoke = undefined;", browser);
  vm.runInContext(`${transportSource()}\nglobalThis.__transport = transport;`, browser);
  assert.equal(await browser.__transport.pickInstallImage(), null, "the browser has no chooser");
  assert.equal(browserCalls.length, 0, "the browser performs no pick request");
  await browser.__transport.installPlan("sda", "demo.img");
  assert.equal(browserCalls[0].path, "/api/v1/storage/install/plan");
  const body = JSON.parse(browserCalls[0].options.body);
  assert.deepEqual(Object.keys(body).sort(), ["imageId", "targetId"]);
  assert.equal(body.imageId, "demo.img");
  assert.equal("imagePath" in body, false);
});

test("a fresh catalogue read discards the plan and the acknowledgement", async () => {
  const { context, getElement } = createContext({
    state: {
      installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img",
      installPlan: demoPlan(), installAcknowledgement: "sda", installResult: applySucceeded(),
    },
    transport: { installTargets: async () => demoOptions() },
  });
  await context.loadInstallOptions();
  assert.equal(context.state.installPlan, null);
  assert.equal(context.state.installAcknowledgement, "");
  assert.equal(context.state.installResult, null);
  assert.equal(context.installApplyDisabled(), true);
  assert.doesNotMatch(getElement("[data-install-body]").innerHTML, /data-install-apply/);
});

test("a dry run is rendered as a preview, not a completed write", () => {
  // The CLI without --live-execution returns a real target and a byte count while
  // writing nothing. Rendering that as a finished write is what a script or an
  // operator would act on.
  // A real dry run: the target is live, only the write is withheld.
  const dry = {
    ...applySucceeded(demoPlan({ synthetic: false })),
    dryRun: true,
  };
  const { context } = createContext({
    state: { installResult: dry, installOptions: demoOptions() },
  });
  const html = context.renderInstallResult(dry);
  assert.match(html, /data-install-result-dry-run/, "the dry run is stated");
  assert.match(html, /Bytes planned/, "the count is not labelled as written");
  assert.doesNotMatch(html, /data-install-result-synthetic/, "the target is real, not demo");

  // The panel header must not announce a finished write either.
  const page = context.renderSystemInstallBody();
  assert.match(page, /Preview only/);

  // A payload from a surface that predates the field keeps its old rendering.
  const written = applySucceeded();
  const writtenHtml = context.renderInstallResult(written);
  assert.doesNotMatch(writtenHtml, /data-install-result-dry-run/);
  assert.match(writtenHtml, /Bytes written/);
});

test("a catalogue refresh discards a preview that is still in flight", async () => {
  // The finding: the refresh cleared the plan but did not invalidate the version,
  // so a preview that was already in flight came back and restored a confirmation
  // for the pre-refresh selection.
  let release = null;
  const refreshed = demoOptions({
    targets: [demoTarget({ id: "mmcblk0", path: "/dev/mmcblk0", model: "eMMC 32GB", kind: "emmc", mounted: false, holdsRunningSystem: false })],
    images: [{ id: "b.img", sizeBytes: 4194304 }],
  });
  const { context } = createContext({
    state: { installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img" },
    transport: {
      installPlan: () => new Promise((resolve) => {
        release = () => resolve({ target: { id: "sda" }, image: { id: "demo.img" } });
      }),
      installTargets: async () => refreshed,
    },
  });
  const inFlight = context.planSystemInstall();
  await context.loadInstallOptions();
  assert.equal(context.state.installTargetId, "mmcblk0", "the refresh moved the selection");
  release();
  assert.equal(await inFlight, null, "the stale preview resolves to nothing");
  assert.equal(
    context.state.installPlan,
    null,
    "no plan is adopted for the selection the refresh replaced",
  );
  assert.equal(context.state.installPlanning, false, "the page is not left mid-preview");
});

test("a preview that lands after the selection moved is discarded", async () => {
  // The finding: changing the device or image while a preview is in flight used
  // to leave the page showing one selection and a plan for another, so the
  // confirmation described a device the operator had already moved away from.
  let release = null;
  const { context } = createContext({
    state: { installOptions: demoOptions(), installTargetId: "sda", installImageId: "demo.img" },
    transport: {
      installPlan: () => new Promise((resolve) => {
        release = () => resolve({ target: { id: "sda" }, image: { id: "demo.img" } });
      }),
    },
  });
  const inFlight = context.planSystemInstall();
  // The operator changes the target while the preview is outstanding.
  context.state.installTargetId = "mmcblk0";
  context.invalidateInstallPlan();
  release();
  assert.equal(await inFlight, null, "the stale preview resolves to nothing");
  assert.equal(context.state.installPlan, null, "no plan is adopted for the previous selection");
  assert.equal(context.state.installPlanning, false, "the page is not left mid-preview");
});

test("a failed plan build is shown with its own code and asks for a retry", async () => {
  const failure = new Error("/dev/mmcblk0 has mounted partitions: /");
  failure.code = "install_conflict";
  const { context, getElement } = createContext({
    state: { installOptions: demoOptions(), installTargetId: "mmcblk0", installImageId: "demo.img" },
    transport: { installPlan: async () => { throw failure; } },
  });
  assert.equal(await context.planSystemInstall(), null);
  assert.equal(context.state.installPlan, null);
  assert.equal(context.state.installError.code, "install_conflict");
  const html = getElement("[data-install-body]").innerHTML;
  assert.match(html, /data-install-error-code="install_conflict"/);
  assert.match(html, /Unmount the target or pick an image that fits/);
  assert.doesNotMatch(html, /data-install-apply/);
  assert.match(html, /data-install-plan-button/);
  assert.match(html, /Show every consequence/);
});

test("the page keeps the hardened i18n and transport style and never fetches devices directly", () => {
  const blockStart = source.indexOf("// System image install page\n//");
  assert.ok(blockStart > 0, "install page block anchor must be unique");
  const block = source.slice(blockStart, source.indexOf("function renderActions()"));
  assert.ok(block.length > 0, "install page block must exist");
  // The anchor must not accidentally catch the state-field comment.
  assert.match(block, /async function applySystemInstall\(/);
  assert.doesNotMatch(block, /installLoadingPromise: null/);
  // Only the shared transport talks to the control plane; the page never calls
  // fetch, never reads localStorage and never touches a device path itself.
  assert.doesNotMatch(block, /\bfetch\(/);
  assert.doesNotMatch(block, /localStorage/);
  assert.doesNotMatch(block, /confirm:/, "no boolean confirm may ever be sent");
  assert.match(block, /transport\.installPlan\(/);
  assert.match(block, /transport\.installApply\(payload\)/);
  assert.match(source, /"\/api\/v1\/storage\/install\/targets"/);
  assert.match(source, /"\/api\/v1\/storage\/install\/plan"/);
  assert.match(source, /"\/api\/v1\/storage\/install\/apply"/);
  assert.match(source, /tauriInvoke\("system_install_targets"/);
  assert.match(source, /tauriInvoke\("system_install_plan"/);
  assert.match(source, /tauriInvoke\("system_install_apply"/);
  assert.match(source, /tauriInvoke\("system_install_pick_image"\)/);
  assert.doesNotMatch(source, /tauriInvoke\("system_install_pick_image",/, "the chooser command takes no arguments");
  assert.doesNotMatch(block, /\sstyle="/, "the page must not emit inline styles");

  // The chooser is desktop-only and never becomes a second identity: both its
  // entry points sit behind the shell capability, and the apply payload builder
  // knows nothing about paths or handles.
  assert.match(block, /installPickerAvailable\(\)/);
  assert.ok(block.includes("${canPick ? `"), "the chooser entry is rendered only in the desktop shell");
  assert.ok(block.includes("${canPick && picked ? `"), "the chosen file is displayed only in the desktop shell");
  assert.match(codeOnly(handler("installSelectableImages")), /installPickerAvailable\(\)/);
  assert.doesNotMatch(codeOnly(handler("installApplyRequest")), /path|handle/i, "no path or handle field may exist in the payload");
  assert.doesNotMatch(codeOnly(handler("pickInstallImage")), /imagePath|href|fetch\(/);

  // Every key the page uses exists in both dictionaries, with placeholders that
  // match, so nothing silently falls back to "Details unavailable".
  const i18nSource = fs.readFileSync(new URL("./i18n.js", import.meta.url), "utf8");
  const english = loadI18n("en-US");
  const chinese = loadI18n("zh-CN");
  const literals = new Set([...block.matchAll(/t\("(install\.[A-Za-z0-9_.]+)"/g)].map((match) => match[1]));
  for (const code of ["confirmation_required", "acknowledgement_mismatch", "plan_required", "stale_plan", "root_required", "install_conflict", "writes_running_system", "install_unsupported", "invalid_install_request", "internal_error", "generic"]) {
    literals.add(`install.error.${code}`);
    literals.add(`install.error.${code}.hint`);
  }
  for (const code of ["confirmation_required", "acknowledgement_mismatch", "stale_plan", "install_unsupported"]) {
    literals.add(`install.error.${code}.detail`);
  }
  for (const key of ["install.result.verification.matched", "install.result.verification.mismatched", "install.result.verification.unsupported"]) {
    literals.add(key);
  }
  // Keys the page builds dynamically for the chooser and for a refused handle.
  for (const key of ["install.pickFailed", "install.pickRejected", "install.handleUnknown", "install.handleExpired"]) {
    literals.add(key);
    literals.add(`${key}.hint`);
  }
  const params = { expected: "sda", provided: "x", target: "/dev/sda", mounts: "/boot", number: 1, size: "2.0 MiB", name: "demo.img", warning: "core warning", expectedCount: 2, observed: 2 };
  for (const key of literals) {
    assert.ok(i18nSource.includes(`"${key}"`), `${key} must be declared in i18n.js`);
    for (const i18n of [english, chinese]) {
      const value = i18n.t(key, params);
      assert.notEqual(value, "Details unavailable", `${key} must be localized`);
      assert.ok(value.trim().length > 0, key);
    }
  }
  // The critical confirmations are genuinely bilingual.
  assert.match(chinese.t("install.error.acknowledgement_mismatch.hint", params), /重新键入目标设备名/);
  assert.match(english.t("install.error.acknowledgement_mismatch.hint", params), /Retype the target device name/);
  assert.match(chinese.t("install.error.stale_plan.hint", params), /重新生成计划/);
  assert.match(english.t("install.error.stale_plan.hint", params), /Build a fresh plan/);
  assert.match(chinese.t("install.apply"), /替换全部分区/);
  assert.match(english.t("install.apply"), /replace all partitions/);
  // The chooser copy is bilingual as well.
  assert.match(chinese.t("install.pickImage"), /选择镜像文件/);
  assert.match(english.t("install.pickImage"), /Choose image file/);
  assert.match(chinese.t("install.pickedFile", params), /已选择文件：demo\.img · 2\.0 MiB/);
  assert.match(english.t("install.pickedFile", params), /Chosen file: demo\.img · 2\.0 MiB/);
  assert.match(chinese.t("install.pickCancelledHint", params), /保持不变/);
  assert.match(english.t("install.pickCancelledHint", params), /unchanged/);

  // The hardening suite is the contract this page must not weaken.
  const hardening = fs.readFileSync(new URL("./hardening.test.mjs", import.meta.url), "utf8");
  assert.match(hardening, /doesNotMatch\(source, \/\\sstyle="\/\)/);
  assert.match(hardening, /"x-rsetup-request": "1"/);
});

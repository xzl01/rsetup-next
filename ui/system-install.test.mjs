import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = fs.readFileSync(new URL("./app.js", import.meta.url), "utf8");

// Static assertions below must judge executable code, not the comments that sit
// next to it (the comments deliberately talk about the fallback that was
// removed), so comments are stripped before scanning.
function codeOnly(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, "").replace(/\/\/[^\n]*/g, "");
}

function handler(name) {
  const start = source.search(new RegExp(`(?:async )?function ${name}\\(`));
  assert.ok(start >= 0, `function ${name} not found`);
  const rest = source.slice(start);
  const next = rest.slice(1).search(/\n(?:async )?function /);
  return next < 0 ? rest : rest.slice(0, next + 1);
}

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
  "planSystemInstall", "applySystemInstall",
];

const INITIAL_STATE = {
  installOptions: null,
  installLiveAllowed: false,
  installLoading: false,
  installLoadingPromise: null,
  installLoadError: null,
  installTargetId: "",
  installImageId: "",
  installPlan: null,
  installPlanning: false,
  installAcknowledgement: "",
  installRunning: false,
  installError: null,
  installResult: null,
};

function createContext({ language = "en-US", state = {}, transport = {} } = {}) {
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
  assert.doesNotMatch(block, /\sstyle="/, "the page must not emit inline styles");

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
  const params = { expected: "sda", provided: "x", target: "/dev/sda", mounts: "/boot", number: 1, size: "2.0 MiB", warning: "core warning", expectedCount: 2, observed: 2 };
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

  // The hardening suite is the contract this page must not weaken.
  const hardening = fs.readFileSync(new URL("./hardening.test.mjs", import.meta.url), "utf8");
  assert.match(hardening, /doesNotMatch\(source, \/\\sstyle="\/\)/);
  assert.match(hardening, /"x-rsetup-request": "1"/);
});
